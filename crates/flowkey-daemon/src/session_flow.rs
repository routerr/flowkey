use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use flowkey_core::daemon::DaemonRuntime;
use flowkey_core::recovery::HeldKeyTracker;
use flowkey_core::RuntimeSnapshot;
use flowkey_input::InputEventSink;
use flowkey_net::connection::SessionSender;
use tracing::{info, warn};

use crate::status_writer::refresh_and_persist_status_snapshot;

pub(crate) struct DaemonSessionCallback {
    pub(crate) runtime: Arc<Mutex<DaemonRuntime>>,
    pub(crate) status_snapshot: Arc<ArcSwap<RuntimeSnapshot>>,
    pub(crate) status_path: PathBuf,
    pub(crate) suppression_state: Arc<AtomicBool>,
    pub(crate) accept_remote_control: bool,
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
                self.suppression_state.store(false, Ordering::SeqCst);
                refresh_and_persist_status_snapshot(
                    &self.runtime,
                    &self.status_snapshot,
                    &self.status_path,
                );
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
                self.suppression_state.store(false, Ordering::SeqCst);
                refresh_and_persist_status_snapshot(
                    &self.runtime,
                    &self.status_snapshot,
                    &self.status_path,
                );
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
        let mut senders = session_senders
            .lock()
            .expect("session sender registry should not be poisoned");
        senders.remove(peer_id);
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

    runtime
        .lock()
        .expect("daemon runtime mutex should not be poisoned")
        .mark_disconnected(peer_id);
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
        fn handle(&mut self, event: &InputEvent) -> Result<(), String> {
            self.handled_events.push(event.clone());
            Ok(())
        }

        fn release_all(&mut self) -> Result<(), String> {
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
            runtime.mark_authenticated(peer_id);
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
            runtime.mark_authenticated(peer_id);
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
            runtime.mark_authenticated("office-pc");
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
