use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use flowkey_config::Config;
use flowkey_core::daemon::{DaemonRuntime, DaemonState};
use flowkey_core::recovery::ReconnectBackoff;
use flowkey_core::DaemonCommand;
use flowkey_core::DaemonStatus;
use flowkey_input::capture::{CaptureSignal, InputCapture};
use flowkey_input::hotkey::HotkeyBinding;
use flowkey_input::loopback::{LoopbackSuppressor, SharedLoopbackSuppressor};
use flowkey_input::InputEventSink;
use flowkey_net::connection::{
    connect_and_authenticate, run_authenticated_session, session_channel, SessionSender,
    SessionStateCallback,
};
use flowkey_net::discovery::DiscoveryAdvertisement;
use flowkey_net::heartbeat::HeartbeatConfig;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::time::sleep;
use tracing::{error, info, warn};

pub async fn run_daemon(config: Config) -> Result<()> {
    let listen_addr: SocketAddr = config
        .node
        .listen_addr
        .parse()
        .with_context(|| format!("invalid listen address {}", config.node.listen_addr))?;
    let status_path = Config::status_path()?;
    let control_path = Config::control_path()?;
    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.node.listen_addr))?;
    let runtime: Arc<Mutex<DaemonRuntime>> = Arc::new(Mutex::new(DaemonRuntime::new()));
    let session_senders: Arc<Mutex<HashMap<String, SessionSender>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let loopback = LoopbackSuppressor::shared(Duration::from_millis(40));
    seed_platform_diagnostics(&runtime);
    let discovery = advertise_discovery_service(&config, &runtime, &status_path);

    info!(
        node = %config.node.name,
        listen = %config.node.listen_addr,
        "starting daemon runtime"
    );

    println!("flky daemon running");
    println!("node: {}", config.node.name);
    println!("listen: {}", config.node.listen_addr);
    println!("hotkey: {}", config.switch.hotkey);
    println!("trusted peers: {}", config.peers.len());
    persist_status_snapshot(&runtime, &status_path);
    print_runtime_notes(&runtime);

    let hotkey_binding = match HotkeyBinding::parse(&config.switch.hotkey) {
        Ok(binding) => binding,
        Err(error) => {
            warn!(%error, "local hotkey listener is disabled");
            HotkeyBinding {
                code: flowkey_input::keycode::KeyCode::Character('k'),
                modifiers: flowkey_input::event::Modifiers {
                    shift: true,
                    control: true,
                    alt: true,
                    meta: false,
                },
            }
        }
    };

    spawn_hotkey_watcher(
        Arc::clone(&runtime),
        Arc::clone(&session_senders),
        Arc::clone(&loopback),
        status_path.clone(),
        hotkey_binding,
    );
    spawn_control_watcher(
        Arc::clone(&runtime),
        Arc::clone(&session_senders),
        control_path.clone(),
        status_path.clone(),
    );

    let incoming_config = config.clone();
    let incoming_runtime = Arc::clone(&runtime);
    let incoming_senders = Arc::clone(&session_senders);
    let incoming_loopback = Arc::clone(&loopback);
    let incoming_status_path = status_path.clone();
    let incoming_task = tokio::spawn(async move {
        loop {
            let (stream, addr) = match listener.accept().await {
                Ok(pair) => pair,
                Err(error) => {
                    error!(%error, "failed to accept incoming connection");
                    break;
                }
            };

            let config = incoming_config.clone();
            let runtime = Arc::clone(&incoming_runtime);
            let session_senders = Arc::clone(&incoming_senders);
            let loopback = Arc::clone(&incoming_loopback);
            let status_path = incoming_status_path.clone();
            tokio::spawn(async move {
                match flowkey_net::connection::authenticate_incoming_stream(&config, stream).await {
                    Ok(connection) => {
                        let peer_id = connection.info.peer_id.clone();
                        runtime
                            .lock()
                            .expect("daemon runtime mutex should not be poisoned")
                            .mark_authenticated(peer_id.clone());
                        let (sender, receiver) = session_channel();
                        let sender_count = {
                            let mut senders = session_senders
                                .lock()
                                .expect("session sender registry should not be poisoned");
                            senders.insert(peer_id.clone(), sender);
                            senders.len()
                        };
                        persist_status_snapshot(&runtime, &status_path);
                        info!(
                            peer = %peer_id,
                            remote = %addr,
                            sender_count,
                            "incoming session authenticated and sender registered"
                        );
                        let (mut sink, backend, note) = create_platform_input_sink(loopback);
                        {
                            let mut runtime = runtime
                                .lock()
                                .expect("daemon runtime mutex should not be poisoned");
                            runtime.diagnostics.input_injection_backend = backend.to_string();
                            if let Some(note) = note {
                                push_runtime_note(&mut runtime, note);
                            }
                        }
                        persist_status_snapshot(&runtime, &status_path);
                        let callback = DaemonSessionCallback {
                            runtime: Arc::clone(&runtime),
                            status_path: status_path.clone(),
                        };
                        if let Err(error) = run_authenticated_session(
                            connection,
                            &config.node.id,
                            HeartbeatConfig::default(),
                            sink.as_mut(),
                            receiver,
                            &callback,
                        )
                        .await
                        {
                            warn!(peer = %addr, %error, "incoming session ended");
                        }
                        cleanup_session(
                            &peer_id,
                            &session_senders,
                            &runtime,
                            &status_path,
                            sink.as_mut(),
                        );
                    }
                    Err(error) => {
                        warn!(%error, remote = %addr, "incoming session rejected");
                    }
                }
            });
        }
    });

    let outbound_config = config.clone();
    let outbound_runtime = Arc::clone(&runtime);
    let outbound_senders = Arc::clone(&session_senders);
    let outbound_loopback = Arc::clone(&loopback);
    let outbound_status_path = status_path.clone();
    let outbound_tasks: Vec<_> = outbound_config
        .peers
        .iter()
        .filter(|peer| peer.trusted)
        .filter(|peer| peer.id > outbound_config.node.id)
        .cloned()
        .map(|peer| {
            let config = outbound_config.clone();
            let runtime = Arc::clone(&outbound_runtime);
            let session_senders = Arc::clone(&outbound_senders);
            let loopback = Arc::clone(&outbound_loopback);
            let status_path = outbound_status_path.clone();
            tokio::spawn(async move {
                let mut backoff = ReconnectBackoff::default();
                loop {
                    info!(peer = %peer.id, addr = %peer.addr, "attempting outbound connection");
                    match connect_and_authenticate(&config, &peer).await {
                        Ok(connection) => {
                            backoff.reset();
                            let peer_id = connection.info.peer_id.clone();
                            runtime
                                .lock()
                                .expect("daemon runtime mutex should not be poisoned")
                                .mark_authenticated(peer_id.clone());
                            let (sender, receiver) = session_channel();
                            let sender_count = {
                                let mut senders = session_senders
                                    .lock()
                                    .expect("session sender registry should not be poisoned");
                                senders.insert(peer_id.clone(), sender);
                                senders.len()
                            };
                            persist_status_snapshot(&runtime, &status_path);
                            info!(
                                peer = %peer_id,
                                sender_count,
                                "outbound session authenticated and sender registered"
                            );
                            let (mut sink, backend, note) =
                                create_platform_input_sink(Arc::clone(&loopback));
                            {
                                let mut runtime = runtime
                                    .lock()
                                    .expect("daemon runtime mutex should not be poisoned");
                                runtime.diagnostics.input_injection_backend = backend.to_string();
                                if let Some(note) = note {
                                    push_runtime_note(&mut runtime, note);
                                }
                            }
                            persist_status_snapshot(&runtime, &status_path);
                            let callback = DaemonSessionCallback {
                                runtime: Arc::clone(&runtime),
                                status_path: status_path.clone(),
                            };
                            if let Err(error) = run_authenticated_session(
                                connection,
                                &config.node.id,
                                HeartbeatConfig::default(),
                                sink.as_mut(),
                                receiver,
                                &callback,
                            )
                            .await
                            {
                                warn!(peer = %peer.id, %error, "outbound session ended");
                            }
                            cleanup_session(
                                &peer_id,
                                &session_senders,
                                &runtime,
                                &status_path,
                                sink.as_mut(),
                            );
                        }
                        Err(error) => {
                            warn!(peer = %peer.id, %error, "outbound session failed");
                        }
                    }

                    sleep(backoff.next_delay()).await;
                }
            })
        })
        .collect();

    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("shutdown requested");
        }
    }

    incoming_task.abort();
    for task in outbound_tasks {
        task.abort();
    }
    if let Some(discovery) = &discovery {
        if let Err(error) = discovery.shutdown() {
            warn!(%error, "failed to stop discovery advertisement");
        }
    }
    clear_status_snapshot(&status_path);
    let runtime = runtime
        .lock()
        .expect("daemon runtime mutex should not be poisoned");
    info!(sessions = runtime.sessions.len(), state = ?runtime.state, "daemon stopped");

    Ok(())
}

fn spawn_hotkey_watcher(
    runtime: Arc<Mutex<DaemonRuntime>>,
    session_senders: Arc<Mutex<HashMap<String, SessionSender>>>,
    loopback: SharedLoopbackSuppressor,
    status_path: PathBuf,
    binding: HotkeyBinding,
) {
    #[cfg(target_os = "macos")]
    let mut capture: Box<dyn InputCapture> = Box::new(
        flowkey_platform_macos::capture::MacosCapture::with_loopback(binding, Some(loopback)),
    );

    #[cfg(target_os = "windows")]
    let mut capture: Box<dyn InputCapture> = Box::new(
        flowkey_platform_windows::capture::WindowsCapture::with_loopback(binding, Some(loopback)),
    );

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (runtime, session_senders, loopback, status_path, binding);

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        if let Err(error) = capture.start() {
            {
                let mut runtime = runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned");
                runtime.diagnostics.local_capture_enabled = false;
                push_runtime_note(
                    &mut runtime,
                    format!("local hotkey listener disabled: {error}"),
                );
            }
            persist_status_snapshot(&runtime, &status_path);
            warn!(%error, "failed to start local hotkey listener");
            return;
        }

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.diagnostics.local_capture_enabled = true;
        }
        persist_status_snapshot(&runtime, &status_path);

        thread::spawn(move || loop {
            match capture.poll() {
                Some(CaptureSignal::HotkeyPressed) => {
                    let result = {
                        let mut runtime = runtime
                            .lock()
                            .expect("daemon runtime mutex should not be poisoned");
                        match runtime.toggle_controller() {
                            Ok(()) => {
                                let state = runtime.state.clone();
                                let peer = runtime.active_peer_id.clone();
                                Ok((state, peer))
                            }
                            Err(error) => Err(error),
                        }
                    };

                    match result {
                        Ok((state, peer)) => {
                            persist_status_snapshot(&runtime, &status_path);
                            if let Some(ref peer_id) = peer {
                                match &state {
                                    DaemonState::Controlling { .. } => {
                                        notify_peer_switch(peer_id, &session_senders);
                                    }
                                    DaemonState::ConnectedIdle => {
                                        notify_peer_release(peer_id, &session_senders);
                                    }
                                    _ => {}
                                }
                            }
                            info!(state = ?state, peer = ?peer, "hotkey switched daemon role");
                        }
                        Err(error) => {
                            warn!(%error, "hotkey switch ignored");
                        }
                    }
                }
                Some(CaptureSignal::Input(event)) => {
                    let active_peer_id = {
                        let runtime = runtime
                            .lock()
                            .expect("daemon runtime mutex should not be poisoned");
                        if matches!(
                            runtime.state,
                            flowkey_core::daemon::DaemonState::Controlling { .. }
                        ) {
                            runtime.active_peer_id.clone()
                        } else {
                            None
                        }
                    };

                    if let Some(peer_id) = active_peer_id {
                        let sender = session_senders
                            .lock()
                            .expect("session sender registry should not be poisoned")
                            .get(&peer_id)
                            .cloned();

                        match sender {
                            Some(sender) => {
                                if let Err(error) = sender.send_input(event.clone()) {
                                    warn!(peer = %peer_id, %error, "failed to forward local input");
                                    mark_lost_session(
                                        &peer_id,
                                        &session_senders,
                                        &runtime,
                                        &status_path,
                                    );
                                } else {
                                    info!(peer = %peer_id, event = ?event, "forwarded local input to active peer");
                                }
                            }
                            None => {
                                warn!(peer = %peer_id, "no session sender registered for active peer");
                                mark_lost_session(
                                    &peer_id,
                                    &session_senders,
                                    &runtime,
                                    &status_path,
                                );
                            }
                        }
                    }
                }
                None => thread::sleep(Duration::from_millis(10)),
            }
        });
    }
}

fn spawn_control_watcher(
    runtime: Arc<Mutex<DaemonRuntime>>,
    session_senders: Arc<Mutex<HashMap<String, SessionSender>>>,
    control_path: PathBuf,
    status_path: PathBuf,
) {
    thread::spawn(move || loop {
        if !control_path.exists() {
            thread::sleep(Duration::from_millis(150));
            continue;
        }

        let command = match DaemonCommand::load_from_path(&control_path) {
            Ok(command) => command,
            Err(error) => {
                warn!(%error, path = %control_path.display(), "failed to load daemon control command");
                let _ = fs::remove_file(&control_path);
                thread::sleep(Duration::from_millis(150));
                continue;
            }
        };

        match handle_control_command(command, &runtime, &session_senders, &status_path) {
            Ok(()) => {
                let _ = fs::remove_file(&control_path);
            }
            Err(error) => {
                warn!(%error, path = %control_path.display(), "daemon control command failed");
                let _ = fs::remove_file(&control_path);
            }
        }

        thread::sleep(Duration::from_millis(150));
    });
}

fn handle_control_command(
    command: DaemonCommand,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
    status_path: &std::path::Path,
) -> Result<(), String> {
    match command {
        DaemonCommand::Switch { peer_id } => {
            let (state, peer) = {
                let mut runtime = runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned");

                let was_controlled_by_other =
                    matches!(runtime.state, DaemonState::ControlledBy { .. });
                if was_controlled_by_other {
                    runtime.release_control()?;
                }

                runtime.select_active_peer(peer_id.clone())?;
                if !matches!(runtime.state, DaemonState::Controlling { .. }) {
                    runtime.toggle_controller()?;
                }

                (runtime.state.clone(), runtime.active_peer_id.clone())
            };
            persist_status_snapshot(runtime, status_path);
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
                let runtime = runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned");
                runtime.active_peer_id.clone()
            };
            {
                let mut runtime = runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned");
                runtime.release_control()?;
            }
            persist_status_snapshot(runtime, status_path);
            if let Some(peer_id) = &active_peer {
                notify_peer_release(peer_id, session_senders);
            }
            let state = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned")
                .state
                .clone();
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

fn notify_peer_switch(peer_id: &str, session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>) {
    let request_id = generate_request_id();
    let request_label = request_id.clone();
    let (sender, sender_count, connected_peers) = {
        let senders = session_senders
            .lock()
            .expect("session sender registry should not be poisoned");
        let peers = senders.keys().cloned().collect::<Vec<_>>();
        (senders.get(peer_id).cloned(), senders.len(), peers)
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

fn notify_peer_release(
    peer_id: &str,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
) {
    let request_id = generate_request_id();
    let request_label = request_id.clone();
    let (sender, sender_count, connected_peers) = {
        let senders = session_senders
            .lock()
            .expect("session sender registry should not be poisoned");
        let peers = senders.keys().cloned().collect::<Vec<_>>();
        (senders.get(peer_id).cloned(), senders.len(), peers)
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

struct DaemonSessionCallback {
    runtime: Arc<Mutex<DaemonRuntime>>,
    status_path: PathBuf,
}

impl SessionStateCallback for DaemonSessionCallback {
    fn on_remote_switch(&self, peer_id: &str, request_id: &str) {
        let (result, state_before) = {
            let mut runtime = self
                .runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            let state_before = runtime.state.clone();
            (runtime.mark_controlled_by(peer_id), state_before)
        };
        match result {
            Ok(()) => {
                persist_status_snapshot(&self.runtime, &self.status_path);
                let state_after = self
                    .runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned")
                    .state
                    .clone();
                info!(
                    peer = %peer_id,
                    request = %request_id,
                    state_before = ?state_before,
                    state_after = ?state_after,
                    "transitioned to controlled-by via remote switch"
                );
            }
            Err(error) => {
                warn!(
                    peer = %peer_id,
                    request = %request_id,
                    state_before = ?state_before,
                    %error,
                    "failed to apply remote switch"
                );
            }
        }
    }

    fn on_remote_release(&self, peer_id: &str, request_id: &str) {
        let (result, state_before) = {
            let mut runtime = self
                .runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            let state_before = runtime.state.clone();
            (runtime.release_control(), state_before)
        };
        match result {
            Ok(()) => {
                persist_status_snapshot(&self.runtime, &self.status_path);
                let state_after = self
                    .runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned")
                    .state
                    .clone();
                info!(
                    peer = %peer_id,
                    request = %request_id,
                    state_before = ?state_before,
                    state_after = ?state_after,
                    "transitioned to connected-idle via remote release"
                );
            }
            Err(error) => {
                warn!(
                    peer = %peer_id,
                    request = %request_id,
                    state_before = ?state_before,
                    %error,
                    "failed to apply remote release"
                );
            }
        }
    }
}

fn cleanup_session(
    peer_id: &str,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_path: &std::path::Path,
    sink: &mut dyn InputEventSink,
) {
    let sender_count = {
        let mut senders = session_senders
            .lock()
            .expect("session sender registry should not be poisoned");
        senders.remove(peer_id);
        senders.len()
    };

    if let Err(error) = sink.release_all() {
        warn!(peer = %peer_id, %error, "failed to release input state");
    }

    runtime
        .lock()
        .expect("daemon runtime mutex should not be poisoned")
        .mark_disconnected(peer_id);
    persist_status_snapshot(runtime, status_path);
    info!(peer = %peer_id, sender_count, "cleaned up session sender after disconnect");
}

fn mark_lost_session(
    peer_id: &str,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_path: &std::path::Path,
) {
    let sender_count = {
        let mut senders = session_senders
            .lock()
            .expect("session sender registry should not be poisoned");
        senders.remove(peer_id);
        senders.len()
    };

    runtime
        .lock()
        .expect("daemon runtime mutex should not be poisoned")
        .mark_disconnected(peer_id);
    persist_status_snapshot(runtime, status_path);
    warn!(peer = %peer_id, sender_count, "marked session lost and removed sender registration");
}

fn create_platform_input_sink(
    loopback: SharedLoopbackSuppressor,
) -> (Box<dyn InputEventSink>, &'static str, Option<String>) {
    #[cfg(target_os = "macos")]
    {
        match flowkey_platform_macos::inject::MacosInjector::with_loopback(Some(loopback)) {
            Ok(injector) => (Box::new(injector), "native", None),
            Err(error) => {
                warn!(%error, "falling back to logging input sink on macOS");
                (
                    Box::new(LoggingInputSink),
                    "logging",
                    Some(
                        "native input injection unavailable on macOS; using logging sink"
                            .to_string(),
                    ),
                )
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        match flowkey_platform_windows::inject::WindowsInjector::with_loopback(Some(loopback)) {
            Ok(injector) => (Box::new(injector), "native", None),
            Err(error) => {
                warn!(%error, "falling back to logging input sink on Windows");
                (
                    Box::new(LoggingInputSink),
                    "logging",
                    Some(
                        "native input injection unavailable on Windows; using logging sink"
                            .to_string(),
                    ),
                )
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = loopback;
        (
            Box::new(LoggingInputSink),
            "logging",
            Some("native input injection is unavailable on this platform".to_string()),
        )
    }
}

fn seed_platform_diagnostics(runtime: &Arc<Mutex<DaemonRuntime>>) {
    let mut runtime = runtime
        .lock()
        .expect("daemon runtime mutex should not be poisoned");
    for note in platform_notes() {
        push_runtime_note(&mut runtime, note);
    }
}

fn push_runtime_note(runtime: &mut DaemonRuntime, note: String) {
    if !runtime
        .diagnostics
        .notes
        .iter()
        .any(|existing| existing == &note)
    {
        runtime.diagnostics.notes.push(note);
    }
}

fn print_runtime_notes(runtime: &Arc<Mutex<DaemonRuntime>>) {
    let notes = runtime
        .lock()
        .expect("daemon runtime mutex should not be poisoned")
        .diagnostics
        .notes
        .clone();

    for note in notes {
        println!("note: {note}");
    }
}

fn platform_notes() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        return flowkey_platform_macos::permissions::PermissionStatus::probe().notes();
    }

    #[cfg(target_os = "windows")]
    {
        return flowkey_platform_windows::permissions::PermissionStatus::probe().notes();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        vec!["platform diagnostics are limited on this operating system".to_string()]
    }
}

fn advertise_discovery_service(
    config: &Config,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_path: &std::path::Path,
) -> Option<DiscoveryAdvertisement> {
    match flowkey_net::discovery::advertise(config) {
        Ok(discovery) => {
            {
                let mut runtime = runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned");
                push_runtime_note(
                    &mut runtime,
                    "LAN discovery advertisement enabled".to_string(),
                );
            }
            persist_status_snapshot(runtime, status_path);
            Some(discovery)
        }
        Err(error) => {
            {
                let mut runtime = runtime
                    .lock()
                    .expect("daemon runtime mutex should not be poisoned");
                push_runtime_note(&mut runtime, format!("LAN discovery unavailable: {error}"));
            }
            persist_status_snapshot(runtime, status_path);
            warn!(%error, "failed to advertise discovery service");
            None
        }
    }
}

fn persist_status_snapshot(runtime: &Arc<Mutex<DaemonRuntime>>, status_path: &std::path::Path) {
    let status = {
        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        DaemonStatus::from_runtime(&runtime)
    };

    if let Err(error) = status.save_to_path(status_path) {
        warn!(%error, path = %status_path.display(), "failed to persist daemon status");
    }
}

fn clear_status_snapshot(status_path: &std::path::Path) {
    match fs::remove_file(status_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!(%error, path = %status_path.display(), "failed to clear daemon status"),
    }
}

struct LoggingInputSink;

impl InputEventSink for LoggingInputSink {
    fn handle(&mut self, event: &flowkey_input::event::InputEvent) -> Result<(), String> {
        info!(event = ?event, "routing input event to platform sink");
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use flowkey_core::daemon::{DaemonRuntime, DaemonState};
    use flowkey_core::status::DaemonStatus;
    use flowkey_input::event::InputEvent;
    use flowkey_input::InputEventSink;
    use flowkey_net::connection::session_channel;

    use super::{cleanup_session, mark_lost_session, SessionSender};

    #[derive(Default)]
    struct RecordingSink {
        release_calls: usize,
        handled_events: Vec<InputEvent>,
    }

    impl InputEventSink for RecordingSink {
        fn handle(&mut self, event: &InputEvent) -> Result<(), String> {
            self.handled_events.push(event.clone());
            Ok(())
        }

        fn release_all(&mut self) -> Result<(), String> {
            self.release_calls += 1;
            Ok(())
        }
    }

    fn temp_status_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "flowkey-daemon-bootstrap-{label}-{}-{}.toml",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn cleanup_session_releases_input_and_persists_disconnected_status() {
        let runtime = Arc::new(Mutex::new(DaemonRuntime::new()));
        let session_senders = Arc::new(Mutex::new(HashMap::<String, SessionSender>::new()));
        let (sender, _receiver) = session_channel();
        let peer_id = "office-pc";
        let status_path = temp_status_path("cleanup");
        let mut sink = RecordingSink::default();

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.mark_authenticated(peer_id);
            runtime.toggle_controller().expect("should enter control");
        }
        session_senders
            .lock()
            .expect("session sender registry should not be poisoned")
            .insert(peer_id.to_string(), sender);

        cleanup_session(peer_id, &session_senders, &runtime, &status_path, &mut sink);

        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        assert_eq!(sink.release_calls, 1);
        assert!(sink.handled_events.is_empty());
        assert!(session_senders
            .lock()
            .expect("session sender registry should not be poisoned")
            .is_empty());
        assert_eq!(runtime.state, DaemonState::Disconnected);
        assert!(runtime.active_peer_id.is_none());

        let status = DaemonStatus::load_from_path(&status_path)
            .expect("status snapshot should persist after cleanup");
        fs::remove_file(&status_path).ok();

        assert_eq!(status.state, "disconnected");
        assert!(status.active_peer_id.is_none());
        assert!(!status.session_healthy);
    }

    #[test]
    fn lost_session_enters_recovery_without_removing_other_sessions() {
        let runtime = Arc::new(Mutex::new(DaemonRuntime::new()));
        let session_senders = Arc::new(Mutex::new(HashMap::<String, SessionSender>::new()));
        let (active_sender, _active_receiver) = session_channel();
        let (spare_sender, _spare_receiver) = session_channel();
        let peer_id = "office-pc";
        let spare_peer_id = "spare-pc";
        let status_path = temp_status_path("lost-session");

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.mark_authenticated(peer_id);
            runtime.mark_authenticated(spare_peer_id);
            runtime
                .toggle_controller()
                .expect("should enter control for the active peer");
        }
        {
            let mut senders = session_senders
                .lock()
                .expect("session sender registry should not be poisoned");
            senders.insert(peer_id.to_string(), active_sender);
            senders.insert(spare_peer_id.to_string(), spare_sender);
        }

        mark_lost_session(peer_id, &session_senders, &runtime, &status_path);

        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        let senders = session_senders
            .lock()
            .expect("session sender registry should not be poisoned");
        let status = DaemonStatus::load_from_path(&status_path)
            .expect("status snapshot should persist after lost session");
        fs::remove_file(&status_path).ok();

        assert_eq!(runtime.state, DaemonState::Recovering);
        assert_eq!(runtime.active_peer_id.as_deref(), Some(peer_id));
        assert!(senders.get(spare_peer_id).is_some());
        assert!(senders.get(peer_id).is_none());
        assert_eq!(status.state, "recovering");
        assert_eq!(status.active_peer_id.as_deref(), Some(peer_id));
        assert!(!status.session_healthy);
    }
}
