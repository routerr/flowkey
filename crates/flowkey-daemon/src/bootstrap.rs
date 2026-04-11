use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use flowkey_config::Config;
use flowkey_core::daemon::{DaemonRuntime, DaemonState, Role};
use flowkey_core::recovery::{HeldKeyTracker, ReconnectBackoff};
use flowkey_core::RuntimeSnapshot;
use flowkey_input::hotkey::HotkeyBinding;
use flowkey_input::loopback::LoopbackSuppressor;
use flowkey_net::connection::{
    connect_and_authenticate, run_authenticated_session, session_channel, SessionSender,
};
use flowkey_net::heartbeat::HeartbeatConfig;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::control_ipc::spawn_control_watcher;
use crate::platform::{
    create_platform_input_sink, print_runtime_notes, push_runtime_note, seed_platform_diagnostics,
    spawn_hotkey_watcher,
};
use crate::session_flow::{cleanup_session, DaemonSessionCallback};
use crate::status_writer::{
    advertise_discovery_service, clear_status_snapshot, refresh_and_persist_status_snapshot,
};

pub async fn run_daemon(config: Config) -> Result<()> {
    run_daemon_with_shutdown(config, CancellationToken::new()).await
}

pub(crate) async fn run_daemon_with_shutdown(
    config: Config,
    shutdown: CancellationToken,
) -> Result<()> {
    let listen_addr: SocketAddr = config
        .node
        .listen_addr
        .parse()
        .with_context(|| format!("invalid listen address {}", config.node.listen_addr))?;

    #[cfg(target_os = "windows")]
    {
        let permissions = flowkey_platform_windows::permissions::PermissionStatus::probe();
        if !permissions.user_session {
            error!("flowkey daemon must run inside an interactive desktop session; aborting");
            return Err(anyhow::anyhow!(
                "flowkey daemon must run inside an interactive desktop session"
            ));
        }
    }

    let probe_addr = config.node.listen_addr.clone();
    let local_id = config.node.id.clone();
    tokio::spawn(async move {
        flowkey_net::probe::listen_for_probes(probe_addr, local_id).await;
    });

    let status_path = Config::status_path()?;
    let control_path = Config::control_path()?;
    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.node.listen_addr))?;
    let runtime: Arc<Mutex<DaemonRuntime>> = Arc::new(Mutex::new(DaemonRuntime::new()));
    let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
        &runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned"),
    )));
    let suppression_state = Arc::new(AtomicBool::new(false));
    let session_senders: Arc<Mutex<HashMap<String, SessionSender>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let loopback = LoopbackSuppressor::shared(Duration::from_millis(40));
    seed_platform_diagnostics(&runtime);
    let discovery = advertise_discovery_service(&config, &runtime, &status_snapshot, &status_path);

    info!(
        node = %config.node.name,
        listen = %config.node.listen_addr,
        "starting daemon runtime"
    );

    println!("flky daemon running");
    println!("node: {}", config.node.name);
    println!("listen: {}", config.node.listen_addr);
    println!("hotkey: {}", config.switch.hotkey);
    println!("capture mode: {}", config.switch.capture_mode.as_str());
    println!("trusted peers: {}", config.peers.len());
    refresh_and_persist_status_snapshot(&runtime, &status_snapshot, &status_path);
    print_runtime_notes(&status_snapshot);

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
        Arc::clone(&status_snapshot),
        Arc::clone(&session_senders),
        Arc::clone(&loopback),
        status_path.clone(),
        hotkey_binding,
        config.switch.capture_mode,
        Arc::clone(&suppression_state),
    );
    spawn_control_watcher(
        Arc::clone(&runtime),
        Arc::clone(&status_snapshot),
        Arc::clone(&session_senders),
        control_path.clone(),
        config.control_pipe_name(),
        status_path.clone(),
        Arc::clone(&suppression_state),
    );

    let incoming_config = config.clone();
    let incoming_runtime = Arc::clone(&runtime);
    let incoming_status_snapshot = Arc::clone(&status_snapshot);
    let incoming_senders = Arc::clone(&session_senders);
    let incoming_loopback = Arc::clone(&loopback);
    let incoming_status_path = status_path.clone();
    let incoming_suppression_state = Arc::clone(&suppression_state);
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
            let status_snapshot = Arc::clone(&incoming_status_snapshot);
            let session_senders = Arc::clone(&incoming_senders);
            let loopback = Arc::clone(&incoming_loopback);
            let status_path = incoming_status_path.clone();
            let suppression_state = Arc::clone(&incoming_suppression_state);
            tokio::spawn(async move {
                match flowkey_net::connection::authenticate_incoming_stream(&config, stream).await {
                    Ok(connection) => {
                        let peer_id = connection.info.peer_id.clone();
                        let resumed_role = runtime
                            .lock()
                            .expect("daemon runtime mutex should not be poisoned")
                            .mark_authenticated(peer_id.clone());

                        let (sender, receiver) = session_channel();

                        if resumed_role == Some(Role::Controlling) {
                            let request_id = uuid::Uuid::new_v4().to_string();
                            info!(peer = %peer_id, "automatically resuming control session");
                            if let Err(error) = sender.send_switch(request_id) {
                                tracing::warn!(peer = %peer_id, %error, "failed to send resume switch request");
                            } else {
                                let mut runtime_guard = runtime
                                    .lock()
                                    .expect("daemon runtime mutex should not be poisoned");
                                if !matches!(runtime_guard.state, DaemonState::Controlling { .. }) {
                                    let _ = runtime_guard.toggle_controller();
                                }
                                suppression_state.store(true, Ordering::SeqCst);
                            }
                        }

                        let sender_count = {
                            let mut senders = session_senders
                                .lock()
                                .expect("session sender registry should not be poisoned");
                            senders.insert(peer_id.clone(), sender);
                            senders.len()
                        };
                        refresh_and_persist_status_snapshot(
                            &runtime,
                            &status_snapshot,
                            &status_path,
                        );
                        info!(
                            peer = %peer_id,
                            remote = %addr,
                            sender_count,
                            "incoming session authenticated and sender registered"
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
                        refresh_and_persist_status_snapshot(
                            &runtime,
                            &status_snapshot,
                            &status_path,
                        );
                        let callback = DaemonSessionCallback {
                            runtime: Arc::clone(&runtime),
                            status_snapshot: Arc::clone(&status_snapshot),
                            status_path: status_path.clone(),
                            suppression_state: Arc::clone(&suppression_state),
                            accept_remote_control: config.node.accept_remote_control,
                        };
                        let mut held_keys = HeldKeyTracker::default();
                        if let Err(error) = run_authenticated_session(
                            connection,
                            &config.node.id,
                            HeartbeatConfig::default(),
                            sink.as_mut(),
                            &mut held_keys,
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
                            &status_snapshot,
                            &status_path,
                            &suppression_state,
                            &mut held_keys,
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
    let outbound_status_snapshot = Arc::clone(&status_snapshot);
    let outbound_senders = Arc::clone(&session_senders);
    let outbound_loopback = Arc::clone(&loopback);
    let outbound_status_path = status_path.clone();
    let outbound_suppression_state = Arc::clone(&suppression_state);
    let outbound_tasks: Vec<_> = outbound_config
        .peers
        .iter()
        .filter(|peer| peer.trusted)
        .filter(|peer| peer.id > outbound_config.node.id)
        .cloned()
        .map(|peer| {
            let config = outbound_config.clone();
            let runtime = Arc::clone(&outbound_runtime);
            let status_snapshot = Arc::clone(&outbound_status_snapshot);
            let session_senders = Arc::clone(&outbound_senders);
            let loopback = Arc::clone(&outbound_loopback);
            let status_path = outbound_status_path.clone();
            let suppression_state = Arc::clone(&outbound_suppression_state);
            tokio::spawn(async move {
                let mut backoff = ReconnectBackoff::default();
                loop {
                    let mut current_addr = peer.addr.clone();

                    let local_id = config.node.id.clone();
                    if let Ok(Ok(discovered)) = tokio::task::spawn_blocking(move || {
                        flowkey_net::discovery::discover(Duration::from_secs(1), Some(&local_id))
                    })
                    .await
                    {
                        if let Some(discovered_peer) =
                            discovered.into_iter().find(|p| p.id == peer.id)
                        {
                            let mut candidates = discovered_peer.addrs.clone();
                            if !candidates.contains(&current_addr) {
                                candidates.push(current_addr.clone());
                            }

                            if let Ok(winner) = flowkey_net::probe::run_reachability_race(
                                &candidates,
                                &peer.id,
                                Duration::from_millis(500),
                            )
                            .await
                            {
                                current_addr = winner;
                            }
                        }
                    }

                    info!(peer = %peer.id, addr = %current_addr, "attempting outbound connection");
                    let mut dynamic_peer = peer.clone();
                    dynamic_peer.addr = current_addr;
                    match connect_and_authenticate(&config, &dynamic_peer).await {
                        Ok(connection) => {
                            backoff.reset();
                            let peer_id = connection.info.peer_id.clone();
                            let resumed_role = runtime
                                .lock()
                                .expect("daemon runtime mutex should not be poisoned")
                                .mark_authenticated(peer_id.clone());
                            let (sender, receiver) = session_channel();

                            if resumed_role == Some(Role::Controlling) {
                                let request_id = uuid::Uuid::new_v4().to_string();
                                info!(peer = %peer_id, "automatically resuming control session");
                                if let Err(error) = sender.send_switch(request_id) {
                                    tracing::warn!(peer = %peer_id, %error, "failed to send resume switch request");
                                } else {
                                    let mut runtime_guard = runtime
                                        .lock()
                                        .expect("daemon runtime mutex should not be poisoned");
                                    if !matches!(runtime_guard.state, DaemonState::Controlling { .. }) {
                                        let _ = runtime_guard.toggle_controller();
                                    }
                                    suppression_state.store(true, Ordering::SeqCst);
                                }
                            }

                            let sender_count = {
                                let mut senders = session_senders
                                    .lock()
                                    .expect("session sender registry should not be poisoned");
                                senders.insert(peer_id.clone(), sender);
                                senders.len()
                            };
                            refresh_and_persist_status_snapshot(
                                &runtime,
                                &status_snapshot,
                                &status_path,
                            );
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
                            refresh_and_persist_status_snapshot(
                                &runtime,
                                &status_snapshot,
                                &status_path,
                            );
                            let callback = DaemonSessionCallback {
                                runtime: Arc::clone(&runtime),
                                status_snapshot: Arc::clone(&status_snapshot),
                                status_path: status_path.clone(),
                                suppression_state: Arc::clone(&suppression_state),
                                accept_remote_control: config.node.accept_remote_control,
                            };
                            let mut held_keys = HeldKeyTracker::default();
                            if let Err(error) = run_authenticated_session(
                                connection,
                                &config.node.id,
                                HeartbeatConfig::default(),
                                sink.as_mut(),
                                &mut held_keys,
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
                                &status_snapshot,
                                &status_path,
                                &suppression_state,
                                &mut held_keys,
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
            info!("shutdown requested via Ctrl+C");
        }
        _ = shutdown.cancelled() => {
            info!("shutdown requested");
        }
    }

    info!("cleaning up system state before exit");
    suppression_state.store(false, Ordering::SeqCst);

    let loopback = LoopbackSuppressor::shared(Duration::from_millis(0));
    let (mut sink, _, _) = create_platform_input_sink(loopback);
    let _ = sink.release_all();

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
