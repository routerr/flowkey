use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use arc_swap::ArcSwap;
use flowkey_config::Config;
use flowkey_core::daemon::{DaemonRuntime, DaemonState, Role};
use flowkey_core::recovery::HeldKeyTracker;
use flowkey_core::RuntimeSnapshot;
use flowkey_input::loopback::SharedLoopbackSuppressor;
use flowkey_input::InputEventSink;
use flowkey_net::connection::{
    run_authenticated_session, session_channel_with_coalesce_window, AuthenticatedConnection,
    SessionSender,
};
use flowkey_net::heartbeat::HeartbeatConfig;
use tracing::{error, info, warn};

use crate::platform::{create_platform_input_sink, push_runtime_note};
use crate::status_writer::refresh_and_persist_status_snapshot;

pub(crate) struct DaemonSessionCallback {
    pub(crate) runtime: Arc<Mutex<DaemonRuntime>>,
    pub(crate) status_snapshot: Arc<ArcSwap<RuntimeSnapshot>>,
    pub(crate) status_path: PathBuf,
    pub(crate) suppression_state: Arc<AtomicBool>,
    pub(crate) accept_remote_control: bool,
}

impl DaemonSessionCallback {
    /// Applies a state transition and captures both state before and after for logging.
    /// Returns (transition result, state before, state after).
    fn apply_transition_with_state_snapshot<F>(
        &self,
        f: F,
    ) -> (Result<(), String>, DaemonState, DaemonState)
    where
        F: FnOnce(&mut DaemonRuntime) -> Result<(), String>,
    {
        let (result, state_before, state_after) = {
            match self.runtime.lock() {
                Ok(mut runtime) => {
                    let state_before = runtime.state.clone();
                    let result = f(&mut runtime);
                    let state_after = runtime.state.clone();
                    (result, state_before, state_after)
                }
                Err(e) => {
                    error!("daemon runtime mutex poisoned: {}", e);
                    (
                        Err("daemon state unavailable".to_string()),
                        DaemonState::Disconnected,
                        DaemonState::Disconnected,
                    )
                }
            }
        };
        (result, state_before, state_after)
    }

    /// Generic handler for state transitions with logging.
    /// Applies the transition, updates suppression state, persists status snapshot, and logs the result.
    fn apply_state_transition_and_log<F>(
        &self,
        peer_id: &str,
        request_id: &str,
        operation_name: &str,
        f: F,
    ) -> Result<(), String>
    where
        F: FnOnce(&mut DaemonRuntime) -> Result<(), String>,
    {
        let (result, state_before, state_after) = self.apply_transition_with_state_snapshot(f);

        match result {
            Ok(()) => {
                self.suppression_state.store(false, Ordering::SeqCst);
                refresh_and_persist_status_snapshot(
                    &self.runtime,
                    &self.status_snapshot,
                    &self.status_path,
                );
                info!(
                    peer = %peer_id,
                    request = %request_id,
                    state_before = ?state_before,
                    state_after = ?state_after,
                    operation = %operation_name,
                    "state transition succeeded"
                );
                Ok(())
            }
            Err(error) => {
                warn!(
                    peer = %peer_id,
                    request = %request_id,
                    state_before = ?state_before,
                    %error,
                    operation = %operation_name,
                    "state transition failed"
                );
                Err(error)
            }
        }
    }
}

impl flowkey_net::connection::SessionStateCallback for DaemonSessionCallback {
    fn on_remote_switch(&self, peer_id: &str, request_id: &str) {
        if !self.accept_remote_control {
            warn!(
                peer = %peer_id,
                request = %request_id,
                "remote switch request rejected by local configuration"
            );
            return;
        }

        let _ = self.apply_state_transition_and_log(
            peer_id,
            request_id,
            "remote-switch",
            |runtime| runtime.mark_controlled_by(peer_id),
        );
    }

    fn on_remote_release(&self, peer_id: &str, request_id: &str) {
        let _ = self.apply_state_transition_and_log(
            peer_id,
            request_id,
            "remote-release",
            |runtime| runtime.release_control(),
        );
    }
}

/// Runs the full session lifecycle after authentication: registers the sender,
/// resumes any prior control role, creates the platform input sink, runs the
/// session to completion, and cleans up. Returns the elapsed session duration
/// so callers can decide whether to reset their reconnect backoff.
pub(crate) async fn setup_and_run_session(
    connection: AuthenticatedConnection,
    remote_addr: Option<SocketAddr>,
    config: &Config,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_snapshot: &Arc<ArcSwap<RuntimeSnapshot>>,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
    loopback: &SharedLoopbackSuppressor,
    status_path: &PathBuf,
    suppression_state: &Arc<AtomicBool>,
) -> Duration {
    let peer_id = connection.info.peer_id.clone();
    let resumed_role = match runtime.lock() {
        Ok(mut runtime) => match runtime.mark_authenticated(peer_id.clone()) {
            Ok(role) => role,
            Err(e) => {
                warn!(peer = %peer_id, error = %e, "failed to mark authenticated");
                None
            }
        },
        Err(e) => {
            error!("daemon runtime mutex poisoned: {}", e);
            warn!(peer = %peer_id, "failed to mark authenticated due to mutex poisoning");
            None
        }
    };

    let (sender, receiver) =
        session_channel_with_coalesce_window(config.switch.input_coalesce_window_ms);

    if resumed_role == Some(Role::Controlling) {
        let request_id = uuid::Uuid::new_v4().to_string();
        info!(peer = %peer_id, "automatically resuming control session");
        if let Err(error) = sender.send_switch(request_id) {
            warn!(peer = %peer_id, %error, "failed to send resume switch request");
        } else {
            let mut runtime_guard = match runtime.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    error!("daemon runtime mutex poisoned: {}", e);
                    warn!(peer = %peer_id, "failed to toggle controller due to mutex poisoning");
                    return Duration::ZERO;
                }
            };
            if !matches!(runtime_guard.state, DaemonState::Controlling { .. }) {
                let _ = runtime_guard.toggle_controller();
            }
            suppression_state.store(true, Ordering::SeqCst);
        }
    }

    let sender_count = {
        let mut senders = match session_senders.lock() {
            Ok(senders) => senders,
            Err(e) => {
                error!("session sender registry mutex poisoned: {}", e);
                warn!(peer = %peer_id, "failed to register sender due to mutex poisoning");
                return Duration::from_secs(0);
            }
        };
        senders.insert(peer_id.clone(), sender);
        senders.len()
    };
    refresh_and_persist_status_snapshot(runtime, status_snapshot, status_path);
    match remote_addr {
        Some(addr) => info!(
            peer = %peer_id,
            remote = %addr,
            sender_count,
            "incoming session authenticated and sender registered"
        ),
        None => info!(
            peer = %peer_id,
            sender_count,
            "outbound session authenticated and sender registered"
        ),
    }

    let (mut sink, backend, note) = create_platform_input_sink(Arc::clone(loopback));
    {
        let mut runtime_guard = match runtime.lock() {
            Ok(guard) => guard,
            Err(e) => {
                error!("daemon runtime mutex poisoned: {}", e);
                warn!(peer = %peer_id, "failed to update diagnostics due to mutex poisoning");
                return Duration::from_secs(0);
            }
        };
        runtime_guard.diagnostics.input_injection_backend = backend.to_string();
        if let Some(note) = note {
            push_runtime_note(&mut runtime_guard, note);
        }
    }
    refresh_and_persist_status_snapshot(runtime, status_snapshot, status_path);

    let callback = DaemonSessionCallback {
        runtime: Arc::clone(runtime),
        status_snapshot: Arc::clone(status_snapshot),
        status_path: status_path.clone(),
        suppression_state: Arc::clone(suppression_state),
        accept_remote_control: config.node.accept_remote_control,
    };
    let mut held_keys = HeldKeyTracker::default();
    let session_start = Instant::now();
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
        match remote_addr {
            Some(addr) => {
                warn!(peer = %peer_id, remote = %addr, %error, "incoming session ended")
            }
            None => warn!(peer = %peer_id, %error, "outbound session ended"),
        }
    }
    cleanup_session(
        &peer_id,
        session_senders,
        runtime,
        status_snapshot,
        status_path,
        suppression_state,
        &mut held_keys,
        sink.as_mut(),
    );
    session_start.elapsed()
}

pub(crate) fn cleanup_session(
    peer_id: &str,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_snapshot: &Arc<ArcSwap<RuntimeSnapshot>>,
    status_path: &std::path::Path,
    suppression_state: &Arc<AtomicBool>,
    held_keys: &mut HeldKeyTracker,
    sink: &mut dyn InputEventSink,
) {
    let sender_count = {
        let mut senders = match session_senders.lock() {
            Ok(senders) => senders,
            Err(e) => {
                error!("session sender registry mutex poisoned: {}", e);
                warn!(peer = %peer_id, "failed to remove sender due to mutex poisoning");
                return;
            }
        };
        let dropped_inputs = senders
            .get(peer_id)
            .map(|sender| sender.dropped_inputs())
            .unwrap_or(0);
        senders.remove(peer_id);
        if dropped_inputs > 0 {
            warn!(
                peer = %peer_id,
                dropped_inputs,
                "session closed with dropped input events"
            );
        }
        senders.len()
    };

    let recovery = held_keys.release_all(sink);
    if recovery.forced_key_releases > 0 || recovery.forced_button_releases > 0 {
        info!(
            peer = %peer_id,
            forced_key_releases = recovery.forced_key_releases,
            forced_button_releases = recovery.forced_button_releases,
            "released tracked input state during cleanup"
        );
    }

    if let Err(error) = sink.release_all() {
        warn!(peer = %peer_id, %error, "failed to release input state");
    }

    match runtime.lock() {
        Ok(mut runtime) => {
            if let Err(e) = runtime.mark_disconnected(peer_id) {
                warn!(peer = %peer_id, error = %e, "failed to mark disconnected");
            }
        }
        Err(e) => {
            error!("daemon runtime mutex poisoned: {}", e);
            warn!(peer = %peer_id, "failed to mark disconnected due to mutex poisoning");
        }
    }
    suppression_state.store(false, Ordering::SeqCst);
    refresh_and_persist_status_snapshot(runtime, status_snapshot, status_path);
    info!(peer = %peer_id, sender_count, "cleaned up session sender after disconnect");
}

pub(crate) fn mark_lost_session(
    peer_id: &str,
    session_senders: &Arc<Mutex<HashMap<String, SessionSender>>>,
    runtime: &Arc<Mutex<DaemonRuntime>>,
    status_snapshot: &Arc<ArcSwap<RuntimeSnapshot>>,
    status_path: &std::path::Path,
    suppression_state: &Arc<AtomicBool>,
) {
    let sender_count = {
        let mut senders = match session_senders.lock() {
            Ok(senders) => senders,
            Err(e) => {
                error!("session sender registry mutex poisoned: {}", e);
                warn!(peer = %peer_id, "failed to remove sender due to mutex poisoning");
                return;
            }
        };
        senders.remove(peer_id);
        senders.len()
    };

    match runtime.lock() {
        Ok(mut runtime) => {
            if let Err(e) = runtime.mark_disconnected(peer_id) {
                warn!(peer = %peer_id, error = %e, "failed to mark disconnected");
            }
        }
        Err(e) => {
            error!("daemon runtime mutex poisoned: {}", e);
            warn!(peer = %peer_id, "failed to mark disconnected due to mutex poisoning");
        }
    }
    suppression_state.store(false, Ordering::SeqCst);
    refresh_and_persist_status_snapshot(runtime, status_snapshot, status_path);
    warn!(peer = %peer_id, sender_count, "marked session lost and removed sender registration");
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use arc_swap::ArcSwap;
    use flowkey_core::daemon::{DaemonRuntime, DaemonState, Role};
    use flowkey_core::recovery::HeldKeyTracker;
    use flowkey_core::status::DaemonStatus;
    use flowkey_core::RuntimeSnapshot;
    use flowkey_input::event::InputEvent;
    use flowkey_input::InputEventSink;
    use flowkey_net::connection::SessionStateCallback;
    use flowkey_net::connection::{session_channel, SessionSender};

    use super::{cleanup_session, mark_lost_session};

    #[derive(Default)]
    struct RecordingSink {
        release_calls: usize,
        handled_events: Vec<InputEvent>,
    }

    impl InputEventSink for RecordingSink {
        fn handle(&mut self, event: &InputEvent) -> anyhow::Result<()> {
            self.handled_events.push(event.clone());
            Ok(())
        }

        fn release_all(&mut self) -> anyhow::Result<()> {
            self.release_calls += 1;
            Ok(())
        }
    }

    fn temp_status_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "flowkey-daemon-session-{label}-{}-{}.toml",
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
        let suppression_state = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
            &runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned"),
        )));
        let (sender, _receiver) = session_channel();
        let peer_id = "office-pc";
        let status_path = temp_status_path("cleanup");
        let mut sink = RecordingSink::default();
        let mut held_keys = HeldKeyTracker::default();

        held_keys.observe(&InputEvent::KeyDown {
            code: "ShiftLeft".to_string(),
            modifiers: flowkey_input::event::Modifiers {
                shift: true,
                control: false,
                alt: false,
                meta: false,
            },
            timestamp_us: 1,
        });

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

        cleanup_session(
            peer_id,
            &session_senders,
            &runtime,
            &status_snapshot,
            &status_path,
            &suppression_state,
            &mut held_keys,
            &mut sink,
        );

        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        assert_eq!(sink.release_calls, 1);
        assert_eq!(
            sink.handled_events,
            vec![InputEvent::KeyUp {
                code: "ShiftLeft".to_string(),
                modifiers: flowkey_input::event::Modifiers::none(),
                timestamp_us: 0,
            }]
        );
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
    fn cleanup_session_releases_sticky_shift_drag_state() {
        let runtime = Arc::new(Mutex::new(DaemonRuntime::new()));
        let session_senders = Arc::new(Mutex::new(HashMap::<String, SessionSender>::new()));
        let suppression_state = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
            &runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned"),
        )));
        let (sender, _receiver) = session_channel();
        let peer_id = "office-pc";
        let status_path = temp_status_path("sticky-cleanup");
        let mut sink = RecordingSink::default();
        let mut held_keys = HeldKeyTracker::default();

        held_keys.observe(&InputEvent::KeyDown {
            code: "ShiftLeft".to_string(),
            modifiers: flowkey_input::event::Modifiers {
                shift: true,
                control: false,
                alt: false,
                meta: false,
            },
            timestamp_us: 1,
        });
        held_keys.observe(&InputEvent::MouseButtonDown {
            button: flowkey_input::event::MouseButton::Left,
            modifiers: flowkey_input::event::Modifiers {
                shift: true,
                control: false,
                alt: false,
                meta: false,
            },
            timestamp_us: 2,
        });

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

        cleanup_session(
            peer_id,
            &session_senders,
            &runtime,
            &status_snapshot,
            &status_path,
            &suppression_state,
            &mut held_keys,
            &mut sink,
        );

        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        assert_eq!(
            sink.handled_events,
            vec![
                InputEvent::KeyUp {
                    code: "ShiftLeft".to_string(),
                    modifiers: flowkey_input::event::Modifiers {
                        shift: false,
                        control: false,
                        alt: false,
                        meta: false,
                    },
                    timestamp_us: 0,
                },
                InputEvent::MouseButtonUp {
                    button: flowkey_input::event::MouseButton::Left,
                    modifiers: flowkey_input::event::Modifiers::none(),
                    timestamp_us: 0,
                },
            ]
        );
        assert_eq!(sink.release_calls, 1);
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
        let suppression_state = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
            &runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned"),
        )));
        let (active_sender, _active_receiver) = session_channel();
        let (spare_sender, _spare_receiver) = session_channel();
        let peer_id = "office-pc";
        let spare_peer_id = "spare-pc";
        let status_path = temp_status_path("lost-session");

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.mark_authenticated(peer_id).expect("should authenticate");
            runtime.mark_authenticated(spare_peer_id).expect("should authenticate");
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

        mark_lost_session(
            peer_id,
            &session_senders,
            &runtime,
            &status_snapshot,
            &status_path,
            &suppression_state,
        );

        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        let senders = session_senders
            .lock()
            .expect("session sender registry should not be poisoned");
        let status = DaemonStatus::load_from_path(&status_path)
            .expect("status snapshot should persist after lost session");
        fs::remove_file(&status_path).ok();

        assert_eq!(
            runtime.state,
            DaemonState::Recovering {
                intended_role: Some(Role::Controlling)
            }
        );
        assert_eq!(runtime.active_peer_id.as_deref(), Some(peer_id));
        assert!(senders.get(spare_peer_id).is_some());
        assert!(senders.get(peer_id).is_none());
        assert_eq!(status.state, "recovering");
        assert_eq!(status.active_peer_id.as_deref(), Some(peer_id));
        assert!(!status.session_healthy);
    }

    #[test]
    fn on_remote_release_transitions_controlled_by_to_connected_idle() {
        // Verifies the controller-side handling when the controlled machine sends
        // SwitchRelease back: on_remote_release should exit Controlling state and
        // clear suppression, mirroring the controlled side's release_control path.
        let runtime = Arc::new(Mutex::new(DaemonRuntime::new()));
        let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
            &runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned"),
        )));
        let suppression_state = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let status_path = temp_status_path("on-remote-release-controlling");
        let callback = super::DaemonSessionCallback {
            runtime: Arc::clone(&runtime),
            status_snapshot,
            status_path: status_path.clone(),
            suppression_state: Arc::clone(&suppression_state),
            accept_remote_control: true,
        };

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.mark_authenticated("controlled-pc").expect("should authenticate");
            runtime.toggle_controller().expect("should enter Controlling");
        }

        callback.on_remote_release("controlled-pc", "req-release-1");

        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        assert_eq!(
            runtime.state,
            DaemonState::ConnectedIdle,
            "controller should return to ConnectedIdle after remote releases"
        );
        assert!(
            !suppression_state.load(std::sync::atomic::Ordering::SeqCst),
            "suppression_state should be cleared after remote release"
        );

        fs::remove_file(&status_path).ok();
    }

    #[test]
    fn remote_switch_rejected_when_remote_control_disabled() {
        let runtime = Arc::new(Mutex::new(DaemonRuntime::new()));
        let status_snapshot = Arc::new(ArcSwap::from_pointee(RuntimeSnapshot::from_runtime(
            &runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned"),
        )));
        let status_path = temp_status_path("remote-switch-disabled");
        let callback = super::DaemonSessionCallback {
            runtime: Arc::clone(&runtime),
            status_snapshot,
            status_path: status_path.clone(),
            suppression_state: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            accept_remote_control: false,
        };

        {
            let mut runtime = runtime
                .lock()
                .expect("daemon runtime mutex should not be poisoned");
            runtime.mark_authenticated("office-pc").expect("should authenticate");
            runtime.toggle_controller().expect("should enter control");
        }

        callback.on_remote_switch("office-pc", "request-1");

        let runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        assert_eq!(
            runtime.state,
            DaemonState::Controlling {
                peer_id: "office-pc".to_string()
            }
        );
        fs::remove_file(&status_path).ok();
    }
}
