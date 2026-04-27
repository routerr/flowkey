use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use flowkey_config::CaptureMode;
use flowkey_core::daemon::{DaemonRuntime, DaemonState};
use flowkey_core::RuntimeSnapshot;
use flowkey_input::capture::{CaptureSignal, InputCapture};
use flowkey_input::hotkey::HotkeyBinding;
use flowkey_input::loopback::SharedLoopbackSuppressor;
use flowkey_input::InputEventSink;
use flowkey_net::connection::SessionSender;
use tracing::{error, info, warn};

use crate::control_ipc::{notify_peer_release, notify_peer_switch};
use crate::status_writer::refresh_and_persist_status_snapshot;

pub(crate) fn spawn_hotkey_watcher(
    runtime: Arc<Mutex<DaemonRuntime>>,
    status_snapshot: Arc<arc_swap::ArcSwap<RuntimeSnapshot>>,
    session_senders: Arc<Mutex<HashMap<String, SessionSender>>>,
    loopback: SharedLoopbackSuppressor,
    status_path: std::path::PathBuf,
    binding: HotkeyBinding,
    capture_mode: CaptureMode,
    suppression_state: Arc<AtomicBool>,
) {
    let (mut capture, capture_note, capture_restart_counter): (
        Box<dyn InputCapture>,
        Option<String>,
        Option<Arc<AtomicU64>>,
    ) = create_platform_input_capture(
        binding,
        loopback.clone(),
        capture_mode,
        Arc::clone(&suppression_state),
    );

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (
        &runtime,
        &session_senders,
        &status_path,
        capture_mode,
        &suppression_state,
    );

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        if let Some(note) = capture_note {
            {
                match runtime.lock() {
                    Ok(mut runtime) => {
                        push_runtime_note(&mut runtime, note);
                    }
                    Err(e) => {
                        error!("daemon runtime mutex poisoned: {}", e);
                        warn!("failed to add capture startup note due to mutex poisoning");
                    }
                }
            }
            refresh_and_persist_status_snapshot(&runtime, &status_snapshot, &status_path);
        }

        if let Err(error) = capture.start() {
            {
                match runtime.lock() {
                    Ok(mut runtime) => {
                        runtime.diagnostics.local_capture_enabled = false;
                        push_runtime_note(
                            &mut runtime,
                            format!("local hotkey listener disabled: {error}"),
                        );
                    }
                    Err(e) => {
                        error!("daemon runtime mutex poisoned: {}", e);
                        warn!("failed to disable capture due to mutex poisoning");
                    }
                }
            }
            refresh_and_persist_status_snapshot(&runtime, &status_snapshot, &status_path);
            warn!(%error, "failed to start local hotkey listener");
            return;
        }

        {
            match runtime.lock() {
                Ok(mut runtime) => {
                    runtime.diagnostics.local_capture_enabled = true;
                    runtime.diagnostics.capture_restarts = 0;
                }
                Err(e) => {
                    error!("daemon runtime mutex poisoned: {}", e);
                    warn!("failed to enable capture due to mutex poisoning");
                }
            }
        }
        refresh_and_persist_status_snapshot(&runtime, &status_snapshot, &status_path);

        if let Some(capture_restart_counter) = capture_restart_counter {
            let runtime = Arc::clone(&runtime);
            let status_snapshot = Arc::clone(&status_snapshot);
            let status_path = status_path.clone();
            thread::spawn(move || {
                let mut last_seen = 0u64;
                loop {
                    let current = capture_restart_counter.load(Ordering::SeqCst);
                    if current != last_seen {
                        {
                            match runtime.lock() {
                                Ok(mut runtime) => {
                                    runtime.diagnostics.capture_restarts = current;
                                }
                                Err(e) => {
                                    error!("daemon runtime mutex poisoned: {}", e);
                                    warn!("failed to update capture restart counter due to mutex poisoning");
                                    break;
                                }
                            }
                        }
                        refresh_and_persist_status_snapshot(
                            &runtime,
                            &status_snapshot,
                            &status_path,
                        );
                        last_seen = current;
                    }
                    thread::sleep(Duration::from_millis(250));
                }
            });
        }

        thread::spawn(move || loop {
            match capture.wait() {
                Some(CaptureSignal::HotkeyPressed) => {
                    let result = {
                        match runtime.lock() {
                            Ok(mut runtime) => {
                                match runtime.toggle_controller() {
                                    Ok(()) => {
                                        let state = runtime.state.clone();
                                        let peer = runtime.active_peer_id.clone();

                                        match &state {
                                            DaemonState::Controlling { .. } => {
                                                capture.set_suppression_enabled(true);

                                                // Release local keys to prevent stuck modifiers from the hotkey
                                                // trigger or other keys pressed during the transition.
                                                let (mut sink, _, _) =
                                                    create_platform_input_sink(loopback.clone());
                                                let _ = sink.release_all();
                                            }
                                            _ => {
                                                capture.set_suppression_enabled(false);
                                            }
                                        }

                                        Ok((state, peer))
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            Err(e) => {
                                error!("daemon runtime mutex poisoned: {}", e);
                                warn!("hotkey switch ignored due to mutex poisoning");
                                Err("daemon state unavailable".to_string())
                            }
                        }
                    };

                    match result {
                        Ok((state, peer)) => {
                            refresh_and_persist_status_snapshot(
                                &runtime,
                                &status_snapshot,
                                &status_path,
                            );
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
                        match runtime.lock() {
                            Ok(runtime) => {
                                if matches!(
                                    runtime.state,
                                    flowkey_core::daemon::DaemonState::Controlling { .. }
                                ) {
                                    runtime.active_peer_id.clone()
                                } else {
                                    warn!(event = ?event, state = ?runtime.state, "dropping captured input: not in Controlling state");
                                    None
                                }
                            }
                            Err(e) => {
                                error!("daemon runtime mutex poisoned: {}", e);
                                warn!("dropping captured input due to mutex poisoning");
                                None
                            }
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
                                    crate::session_flow::mark_lost_session(
                                        &peer_id,
                                        &session_senders,
                                        &runtime,
                                        &status_snapshot,
                                        &status_path,
                                        &suppression_state,
                                    );
                                } else {
                                    info!(peer = %peer_id, event = ?event, "forwarded local input to active peer");
                                }
                            }
                            None => {
                                warn!(peer = %peer_id, "no session sender registered for active peer");
                                crate::session_flow::mark_lost_session(
                                    &peer_id,
                                    &session_senders,
                                    &runtime,
                                    &status_snapshot,
                                    &status_path,
                                    &suppression_state,
                                );
                            }
                        }
                    }
                }
                Some(CaptureSignal::HotkeySuppressed) => {}
                None => {}
            }
        });
    }
}

pub(crate) fn create_platform_input_capture(
    binding: HotkeyBinding,
    loopback: SharedLoopbackSuppressor,
    capture_mode: CaptureMode,
    _suppression_state: Arc<AtomicBool>,
) -> (
    Box<dyn InputCapture>,
    Option<String>,
    Option<Arc<AtomicU64>>,
) {
    #[cfg(target_os = "macos")]
    {
        let note = match capture_mode {
            CaptureMode::Passive => None,
            CaptureMode::Exclusive => Some(
                "exclusive capture mode enabled; local hotkey listener suppresses mirrored input"
                    .to_string(),
            ),
        };
        let capture = flowkey_platform_macos::capture::MacosCapture::with_loopback(
            binding,
            Some(loopback),
            matches!(capture_mode, CaptureMode::Exclusive),
            _suppression_state,
        );
        let restart_counter = capture.capture_restart_counter();
        return (Box::new(capture), note, restart_counter);
    }

    #[cfg(target_os = "windows")]
    {
        let note = match capture_mode {
            CaptureMode::Passive => None,
            CaptureMode::Exclusive => Some(
                "exclusive capture mode enabled; local input is suppressed while controlling"
                    .to_string(),
            ),
        };
        let capture: Box<dyn InputCapture> = match capture_mode {
            CaptureMode::Exclusive => Box::new(
                flowkey_platform_windows::capture::WindowsExclusiveCapture::with_loopback(
                    binding,
                    Some(loopback),
                    _suppression_state,
                ),
            ),
            CaptureMode::Passive => Box::new(
                flowkey_platform_windows::capture::WindowsCapture::with_loopback(
                    binding,
                    Some(loopback),
                    _suppression_state,
                ),
            ),
        };
        let restart_counter = capture.capture_restart_counter();
        return (capture, note, restart_counter);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (binding, loopback, capture_mode, _suppression_state);
        let capture = flowkey_input::capture::LocalInputCapture::new(HotkeyBinding {
            code: flowkey_input::keycode::KeyCode::Character('k'),
            modifiers: flowkey_input::event::Modifiers::default(),
        });
        let restart_counter = capture.capture_restart_counter();
        (Box::new(capture), None, restart_counter)
    }
}

pub(crate) fn create_platform_input_sink(
    loopback: SharedLoopbackSuppressor,
) -> (Box<dyn InputEventSink>, &'static str, Option<String>) {
    #[cfg(target_os = "macos")]
    {
        let sink = match flowkey_input::native_injector::NativeInputSink::with_loopback(
            "macos",
            Some(loopback),
        ) {
            Ok(sink) => Box::new(sink) as Box<dyn InputEventSink>,
            Err(error) => {
                warn!(%error, "macOS native input sink unavailable; using logging sink");
                Box::new(LoggingInputSink)
            }
        };
        return (
            sink,
            "native",
            Some("macOS native input sink active".to_string()),
        );
    }

    #[cfg(target_os = "windows")]
    {
        let sink = match flowkey_input::native_injector::NativeInputSink::with_loopback(
            "windows",
            Some(loopback),
        ) {
            Ok(sink) => Box::new(sink) as Box<dyn InputEventSink>,
            Err(error) => {
                warn!(%error, "Windows native input sink unavailable; using logging sink");
                Box::new(LoggingInputSink)
            }
        };
        return (
            sink,
            "native",
            Some("Windows native input sink active".to_string()),
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = loopback;
        (
            Box::new(LoggingInputSink),
            "logging",
            Some("platform input injection is not available on this OS".to_string()),
        )
    }
}

pub(crate) fn seed_platform_diagnostics(runtime: &Arc<Mutex<DaemonRuntime>>) {
    for note in platform_notes() {
        let mut runtime = runtime
            .lock()
            .expect("daemon runtime mutex should not be poisoned");
        push_runtime_note(&mut runtime, note);
    }
}

pub(crate) fn push_runtime_note(runtime: &mut DaemonRuntime, note: String) {
    if !runtime
        .diagnostics
        .notes
        .iter()
        .any(|existing| existing == &note)
    {
        runtime.diagnostics.notes.push(note);
    }
}

pub(crate) fn print_runtime_notes(status_snapshot: &Arc<arc_swap::ArcSwap<RuntimeSnapshot>>) {
    let notes = status_snapshot.load().notes.clone();

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
