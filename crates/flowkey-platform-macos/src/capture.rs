use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use flowkey_input::capture::{CaptureSignal, CaptureState, InputCapture};
use flowkey_input::hotkey::{HotkeyBinding, HotkeyTracker};
use flowkey_input::loopback::SharedLoopbackSuppressor;
use tracing::warn;

pub struct MacosCapture {
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    receiver: Option<Receiver<CaptureSignal>>,
    suppression_enabled: Arc<AtomicBool>,
    started: bool,
    exclusive: bool,
}

impl MacosCapture {
    pub fn new(binding: HotkeyBinding) -> Self {
        Self::with_loopback(binding, None, false, Arc::new(AtomicBool::new(false)))
    }

    pub fn with_loopback(
        binding: HotkeyBinding,
        loopback: Option<SharedLoopbackSuppressor>,
        exclusive: bool,
        suppression_enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            binding,
            loopback,
            receiver: None,
            suppression_enabled,
            started: false,
            exclusive,
        }
    }
}

impl InputCapture for MacosCapture {
    fn start(&mut self) -> Result<(), String> {
        if self.started {
            return Ok(());
        }

        let (sender, receiver) = mpsc::channel();
        let binding = self.binding.clone();
        let loopback = self.loopback.clone();
        let suppression_enabled = Arc::clone(&self.suppression_enabled);
        let exclusive = self.exclusive;
        self.receiver = Some(receiver);
        self.started = true;

        thread::spawn(move || {
            let tracker = Arc::new(Mutex::new(HotkeyTracker::new(binding)));
            let state = Arc::new(Mutex::new(CaptureState::default()));

            if exclusive {
                let grab_tracker = Arc::clone(&tracker);
                let grab_state = Arc::clone(&state);
                let result = rdev::grab(move |event: rdev::Event| {
                    let mut tracker = grab_tracker.lock().unwrap();
                    let mut state = grab_state.lock().unwrap();

                    let saved_mouse_position = state.last_mouse_position;
                    let signal = state.translate(event.clone(), &mut tracker, loopback.as_ref());
                    if let Some(signal) = signal {
                        match signal {
                            CaptureSignal::HotkeyPressed => {
                                let _ = sender.send(signal);
                                Some(event)
                            }
                            CaptureSignal::HotkeySuppressed => {
                                Some(event)
                            }
                            CaptureSignal::Input(_) => {
                                let _ = sender.send(signal);
                                if suppression_enabled.load(Ordering::SeqCst) {
                                    // Restore the mouse position to its pre-translate value.
                                    // When an event is suppressed the OS cursor stays in place,
                                    // so the next delta must be relative to the actual cursor
                                    // position, not the position that was never applied.
                                    state.last_mouse_position = saved_mouse_position;
                                    None
                                } else {
                                    Some(event)
                                }
                            }
                        }
                    } else {
                        // Event was suppressed by loopback (it was injected by us).
                        // Don't forward it over the network, but DO pass it through
                        // to the OS so the injection actually takes effect.
                        Some(event)
                    }
                });

                if let Err(error) = result {
                    warn!(error = ?error, "macOS exclusive capture (grab) stopped");
                }
            } else {
                let listen_tracker = Arc::clone(&tracker);
                let listen_state = Arc::clone(&state);
                let result = rdev::listen(move |event: rdev::Event| {
                    let mut tracker = listen_tracker.lock().unwrap();
                    let mut state = listen_state.lock().unwrap();

                    if let Some(signal) = state.translate(event, &mut tracker, loopback.as_ref()) {
                        if !matches!(signal, CaptureSignal::HotkeySuppressed) {
                            let _ = sender.send(signal);
                        }
                    }
                });

                if let Err(error) = result {
                    warn!(error = ?error, "macOS passive capture (listen) stopped");
                }
            }
        });

        Ok(())
    }

    fn poll(&mut self) -> Option<CaptureSignal> {
        self.receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
    }

    fn wait(&mut self) -> Option<CaptureSignal> {
        self.receiver
            .as_ref()
            .and_then(|receiver| receiver.recv().ok())
    }

    fn set_suppression_enabled(&mut self, enabled: bool) {
        if self.exclusive {
            self.suppression_enabled.store(enabled, Ordering::SeqCst);
        }
    }
}
