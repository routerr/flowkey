use std::collections::HashMap;
#[cfg(not(target_os = "windows"))]
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use flowkey_core::daemon::{DaemonRuntime, DaemonState};
use flowkey_core::DaemonCommand;
use flowkey_core::RuntimeSnapshot;
use flowkey_net::connection::SessionSender;
#[cfg(target_os = "windows")]
use tokio::net::windows::named_pipe::ServerOptions;
#[cfg(target_os = "macos")]
use tokio::net::UnixListener;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

use crate::status_writer::refresh_and_persist_status_snapshot;

pub(crate) fn spawn_control_watcher(
    runtime: Arc<Mutex<DaemonRuntime>>,
    status_snapshot: Arc<ArcSwap<RuntimeSnapshot>>,
    session_senders: Arc<Mutex<HashMap<String, SessionSender>>>,
    control_path: PathBuf,
    _control_pipe_name: String,
    status_path: PathBuf,
    suppression_state: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        #[cfg(target_os = "windows")]
        let _ = &control_path;

        #[cfg(target_os = "macos")]
        {
            let socket_path = control_path.with_extension("sock");
            if socket_path.exists() {
                let _ = fs::remove_file(&socket_path);
            }

            loop {
                match UnixListener::bind(&socket_path) {
                    Ok(listener) => {
                        info!(path = %socket_path.display(), "daemon control socket listening");
                        loop {
                            match listener.accept().await {
                                Ok((mut stream, _)) => {
                                    let runtime = Arc::clone(&runtime);
                                    let status_snapshot = Arc::clone(&status_snapshot);
                                    let session_senders = Arc::clone(&session_senders);
                                    let status_path = status_path.clone();
                                    let suppression_state = Arc::clone(&suppression_state);

                                    tokio::spawn(async move {
                                        if let Err(error) = handle_control_stream(
                                            &mut stream,
                                            &runtime,
                                            &status_snapshot,
                                            &session_senders,
                                            &status_path,
                                            &suppression_state,
                                        )
                                        .await
                                        {
                                            warn!(%error, "failed to handle daemon control command");
                                        }
                                    });
                                }
                                Err(error) => {
                                    warn!(%error, "failed to accept control socket connection");
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        error!(%error, path = %socket_path.display(), "failed to bind daemon control socket; retrying in 1s");
                        sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            let mut first_instance = true;
            loop {
                let mut builder = ServerOptions::new();
                if first_instance {
                    builder.first_pipe_instance(true);
                    first_instance = false;
                }

                let mut pipe: tokio::net::windows::named_pipe::NamedPipeServer = match builder
                    .create(&_control_pipe_name)
                {
                    Ok(pipe) => {
                        info!(pipe = %_control_pipe_name, "daemon control pipe listening");
                        pipe
                    }
                    Err(error) => {
                        error!(%error, pipe = %_control_pipe_name, "failed to create daemon control pipe; retrying in 1s");
                        sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                if let Err(error) = pipe.connect().await {
                    warn!(%error, pipe = %_control_pipe_name, "failed to accept daemon control pipe client");
                    sleep(Duration::from_millis(150)).await;
                    continue;
                }

                let runtime = Arc::clone(&runtime);
                let status_snapshot = Arc::clone(&status_snapshot);
                let session_senders = Arc::clone(&session_senders);
                let status_path = status_path.clone();
                let suppression_state = Arc::clone(&suppression_state);

                tokio::spawn(async move {
                    if let Err(error) = handle_control_stream(
                        &mut pipe,
                        &runtime,
                        &status_snapshot,
                        &session_senders,
                        &status_path,
                        &suppression_state,
                    )
                    .await
                    {
                        warn!(%error, "failed to handle daemon control pipe command");
                    }
                });
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            loop {
                if !control_path.exists() {
                    sleep(Duration::from_millis(150)).await;
                    continue;
                }

                let command = match DaemonCommand::load_from_path(&control_path) {
                    Ok(command) => command,
                    Err(error) => {
                        warn!(%error, path = %control_path.display(), "failed to load daemon control command");
                        let _ = fs::remove_file(&control_path);
                        sleep(Duration::from_millis(150)).await;
                        continue;
                    }
                };

                if let Err(error) = handle_control_command(
                    command,
                    &runtime,
                    &status_snapshot,
                    &session_senders,
                    &status_path,
                    &suppression_state,
                ) {
                    warn!(%error, path = %control_path.display(), "daemon control command failed");
                }

                let _ = fs::remove_file(&control_path);
                sleep(Duration::from_millis(150)).await;
            }
        }
    });
}

async fn handle_control_stream<S>(
    stream: &mut S,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_snapshot: &Arc<ArcSwap<RuntimeSnapshot>>,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
    status_path: &std::path::Path,
    suppression_state: &Arc<AtomicBool>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let command = DaemonCommand::read_from(stream)
        .await
        .map_err(|error| error.to_string())?;
    handle_control_command(
        command,
        runtime,
        status_snapshot,
        session_senders,
        status_path,
        suppression_state,
    )
}

fn handle_control_command(
    command: DaemonCommand,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_snapshot: &Arc<ArcSwap<RuntimeSnapshot>>,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
    status_path: &std::path::Path,
    suppression_state: &Arc<AtomicBool>,
) -> Result<(), String> {
    match command {
        DaemonCommand::Switch { peer_id } => {
            let (state, peer, previous_peer) = {
                match runtime.lock() {
                    Ok(mut runtime) => {
                        let releasing_existing_session = matches!(
                            runtime.state,
                            DaemonState::Controlling { .. } | DaemonState::ControlledBy { .. }
                        );
                        let previous_peer = if releasing_existing_session {
                            runtime.active_peer_id.clone()
                        } else {
                            None
                        };
                        if releasing_existing_session {
                            runtime.release_control()?;
                        }

                        runtime.select_active_peer(peer_id.clone())?;
                        if !matches!(runtime.state, DaemonState::Controlling { .. }) {
                            runtime.toggle_controller()?;
                        }

                        if matches!(runtime.state, DaemonState::Controlling { .. }) {
                            suppression_state.store(true, Ordering::SeqCst);
                        }

                        (
                            runtime.state.clone(),
                            runtime.active_peer_id.clone(),
                            previous_peer,
                        )
                    }
                    Err(e) => {
                        error!("daemon runtime mutex poisoned: {}", e);
                        return Err("daemon state unavailable".to_string());
                    }
                }
            };
            refresh_and_persist_status_snapshot(runtime, status_snapshot, status_path);
            if let Some(previous_peer) = previous_peer.as_deref() {
                if previous_peer != peer_id {
                    notify_peer_release(previous_peer, session_senders);
                }
            }
            notify_peer_switch(&peer_id, session_senders);
            info!(
                request = "switch",
                peer = %peer_id,
                state = ?state,
                active_peer = ?peer,
                "daemon control request applied"
            );
            Ok(())
        }
        DaemonCommand::Release => {
            let active_peer = {
                match runtime.lock() {
                    Ok(runtime) => runtime.active_peer_id.clone(),
                    Err(e) => {
                        error!("daemon runtime mutex poisoned: {}", e);
                        return Err("daemon state unavailable".to_string());
                    }
                }
            };
            if let Some(peer_id) = &active_peer {
                notify_peer_release(peer_id, session_senders);
            }
            {
                match runtime.lock() {
                    Ok(mut runtime) => {
                        runtime.release_control()?;
                    }
                    Err(e) => {
                        error!("daemon runtime mutex poisoned: {}", e);
                        return Err("daemon state unavailable".to_string());
                    }
                }
            }
            suppression_state.store(false, Ordering::SeqCst);
            refresh_and_persist_status_snapshot(runtime, status_snapshot, status_path);
            let state = match runtime.lock() {
                Ok(runtime) => runtime.state.clone(),
                Err(e) => {
                    error!("daemon runtime mutex poisoned: {}", e);
                    return Err("daemon state unavailable".to_string());
                }
            };
            info!(
                request = "release",
                state = ?state,
                active_peer = ?active_peer,
                "daemon control request applied"
            );
            Ok(())
        }
    }
}

pub(crate) fn notify_peer_switch(
    peer_id: &str,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
) {
    let request_id = generate_request_id();
    let request_label = request_id.clone();
    let (sender, sender_count, connected_peers) = {
        match session_senders.lock() {
            Ok(senders) => {
                let peers = senders.keys().cloned().collect::<Vec<_>>();
                (senders.get(peer_id).cloned(), senders.len(), peers)
            }
            Err(e) => {
                error!("session sender registry mutex poisoned: {}", e);
                warn!(peer = %peer_id, request = %request_id, "failed to queue switch due to mutex poisoning");
                return;
            }
        }
    };
    if let Some(sender) = sender {
        info!(
            peer = %peer_id,
            request = %request_id,
            sender_count,
            connected_peers = ?connected_peers,
            "queueing switch request for peer session"
        );
        if let Err(error) = sender.send_switch(request_id) {
            warn!(peer = %peer_id, %error, "failed to send switch request to peer");
        } else {
            info!(peer = %peer_id, request = %request_label, "switch request queued onto session channel");
        }
    } else {
        warn!(
            peer = %peer_id,
            request = %request_label,
            sender_count,
            connected_peers = ?connected_peers,
            "no session sender registered for switch request"
        );
    }
}

pub(crate) fn notify_peer_release(
    peer_id: &str,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
) {
    let request_id = generate_request_id();
    let request_label = request_id.clone();
    let (sender, sender_count, connected_peers) = {
        match session_senders.lock() {
            Ok(senders) => {
                let peers = senders.keys().cloned().collect::<Vec<_>>();
                (senders.get(peer_id).cloned(), senders.len(), peers)
            }
            Err(e) => {
                error!("session sender registry mutex poisoned: {}", e);
                warn!(peer = %peer_id, request = %request_id, "failed to queue release due to mutex poisoning");
                return;
            }
        }
    };
    if let Some(sender) = sender {
        info!(
            peer = %peer_id,
            request = %request_id,
            sender_count,
            connected_peers = ?connected_peers,
            "queueing release request for peer session"
        );
        if let Err(error) = sender.send_release(request_id) {
            warn!(peer = %peer_id, %error, "failed to send release request to peer");
        } else {
            info!(peer = %peer_id, request = %request_label, "release request queued onto session channel");
        }
        let _ = sender.send_release_all();
    } else {
        warn!(
            peer = %peer_id,
            request = %request_label,
            sender_count,
            connected_peers = ?connected_peers,
            "no session sender registered for release request"
        );
    }
}

fn generate_request_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("req-{ts}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use arc_swap::ArcSwap;
    use flowkey_core::daemon::{DaemonRuntime, DaemonState};
    use flowkey_core::DaemonCommand;
    use flowkey_core::RuntimeSnapshot;
    use flowkey_net::connection::{session_channel, SessionCommand, SessionSender};

    use super::handle_control_command;

    fn temp_status_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "flowkey-daemon-control-{label}-{}-{}.toml",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn switch_command_releases_previous_peer_before_targeting_new_one() {
        let runtime = Arc::new(Mutex::new(DaemonRuntime::new()));
        let session_senders = Arc::new(Mutex::new(HashMap::<String, SessionSender>::new()));
        let suppression_state = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let status_path = temp_status_path("switch-release");
        let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
            &runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned"),
        )));

        let (old_sender, old_receiver) = session_channel();
        let (new_sender, new_receiver) = session_channel();
        let old_peer = "office-pc";
        let new_peer = "spare-pc";

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.mark_authenticated(old_peer).expect("should authenticate");
            runtime.mark_authenticated(new_peer).expect("should authenticate");
            runtime.toggle_controller().expect("should enter control");
        }
        {
            let mut senders = session_senders
                .lock()
                .expect("session sender registry should not be poisoned");
            senders.insert(old_peer.to_string(), old_sender);
            senders.insert(new_peer.to_string(), new_sender);
        }

        handle_control_command(
            DaemonCommand::switch(new_peer),
            &runtime,
            &status_snapshot,
            &session_senders,
            &status_path,
            &suppression_state,
        )
        .expect("switch command should succeed");

        assert!(matches!(
            old_receiver
                .recv()
                .expect("old peer should receive release"),
            SessionCommand::ReleaseControl { .. }
        ));
        assert!(matches!(
            old_receiver.recv().expect("old peer should receive flush"),
            SessionCommand::ReleaseAll
        ));

        let new_command = new_receiver.recv().expect("new peer should receive switch");
        assert!(matches!(new_command, SessionCommand::SwitchControl { .. }));

        fs::remove_file(&status_path).ok();
    }

    #[test]
    fn release_command_notifies_active_peer_before_transition() {
        let runtime = Arc::new(Mutex::new(DaemonRuntime::new()));
        let session_senders = Arc::new(Mutex::new(HashMap::<String, SessionSender>::new()));
        let suppression_state = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let status_path = temp_status_path("release");
        let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
            &runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned"),
        )));

        let (sender, receiver) = session_channel();
        let peer_id = "office-pc";

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.mark_authenticated(peer_id).expect("should authenticate");
            runtime.toggle_controller().expect("should enter control");
        }
        session_senders
            .lock()
            .expect("session sender registry should not be poisoned")
            .insert(peer_id.to_string(), sender);

        handle_control_command(
            DaemonCommand::release(),
            &runtime,
            &status_snapshot,
            &session_senders,
            &status_path,
            &suppression_state,
        )
        .expect("release command should succeed");

        assert!(matches!(
            receiver
                .recv()
                .expect("peer should receive release request"),
            SessionCommand::ReleaseControl { .. }
        ));
        assert!(matches!(
            receiver.recv().expect("peer should receive flush request"),
            SessionCommand::ReleaseAll
        ));

        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        assert_eq!(runtime.state, DaemonState::ConnectedIdle);

        fs::remove_file(&status_path).ok();
    }

    #[test]
    fn release_from_controlled_by_notifies_controller_and_clears_suppression() {
        // Verifies the cross-node release path: when the controlled side issues a
        // Release command its daemon must send SwitchRelease to the controller so
        // the controller can exit Controlling, and local suppression must be cleared.
        let runtime = Arc::new(Mutex::new(DaemonRuntime::new()));
        let session_senders = Arc::new(Mutex::new(HashMap::<String, SessionSender>::new()));
        let suppression_state = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let status_path = temp_status_path("release-controlled-by");
        let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
            &runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned"),
        )));

        let (controller_sender, controller_receiver) = session_channel();
        let controller_peer = "office-pc";

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.mark_authenticated(controller_peer).expect("should authenticate");
            runtime
                .mark_controlled_by(controller_peer)
                .expect("should enter controlled-by");
        }
        session_senders
            .lock()
            .expect("session sender registry should not be poisoned")
            .insert(controller_peer.to_string(), controller_sender);

        handle_control_command(
            DaemonCommand::release(),
            &runtime,
            &status_snapshot,
            &session_senders,
            &status_path,
            &suppression_state,
        )
        .expect("release command should succeed from controlled-by state");

        // Controller peer should receive SwitchRelease so it exits Controlling.
        assert!(
            matches!(
                controller_receiver
                    .recv()
                    .expect("controller should receive release notification"),
                SessionCommand::ReleaseControl { .. }
            ),
            "controller should receive ReleaseControl"
        );
        assert!(
            matches!(
                controller_receiver
                    .recv()
                    .expect("controller should receive flush"),
                SessionCommand::ReleaseAll
            ),
            "controller should receive ReleaseAll flush"
        );

        // Local state must return to ConnectedIdle.
        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        assert_eq!(
            runtime.state,
            DaemonState::ConnectedIdle,
            "local state should be ConnectedIdle after release"
        );

        // Suppression must be cleared so the local machine regains input.
        assert!(
            !suppression_state.load(std::sync::atomic::Ordering::SeqCst),
            "suppression_state should be false after release"
        );

        fs::remove_file(&status_path).ok();
    }

    #[test]
    fn switch_command_enables_suppression_state() {
        // Verifies that taking control via the IPC Switch command flips
        // suppression_state to true so the platform capture layer can suppress
        // local input while forwarding to the remote.
        let runtime = Arc::new(Mutex::new(DaemonRuntime::new()));
        let session_senders = Arc::new(Mutex::new(HashMap::<String, SessionSender>::new()));
        let suppression_state = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let status_path = temp_status_path("switch-suppression");
        let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
            &runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned"),
        )));

        let (sender, _receiver) = session_channel();
        let peer_id = "office-pc";

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.mark_authenticated(peer_id).expect("should authenticate");
        }
        session_senders
            .lock()
            .expect("session sender registry should not be poisoned")
            .insert(peer_id.to_string(), sender);

        handle_control_command(
            DaemonCommand::switch(peer_id),
            &runtime,
            &status_snapshot,
            &session_senders,
            &status_path,
            &suppression_state,
        )
        .expect("switch command should succeed");

        assert!(
            suppression_state.load(std::sync::atomic::Ordering::SeqCst),
            "suppression_state should be true after switching into Controlling"
        );

        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        assert!(
            matches!(runtime.state, DaemonState::Controlling { .. }),
            "daemon should be in Controlling state"
        );

        fs::remove_file(&status_path).ok();
    }
}
