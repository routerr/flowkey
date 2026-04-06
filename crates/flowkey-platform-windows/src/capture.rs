use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use flowkey_input::capture::{CaptureSignal, CaptureState, InputCapture, LocalInputCapture};
use flowkey_input::event::InputEvent;
use flowkey_input::hotkey::{HotkeyBinding, HotkeyTracker};
use flowkey_input::loopback::SharedLoopbackSuppressor;
use tracing::warn;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SetCursorPos, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

pub struct WindowsCapture {
    inner: LocalInputCapture,
    suppression_enabled: Arc<AtomicBool>,
}

pub struct WindowsExclusiveCapture {
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    receiver: Option<Receiver<CaptureSignal>>,
    suppression_enabled: Arc<AtomicBool>,
    started: bool,
}

impl WindowsCapture {
    pub fn new(binding: HotkeyBinding) -> Self {
        Self::with_loopback(binding, None, Arc::new(AtomicBool::new(false)))
    }

    pub fn with_loopback(
        binding: HotkeyBinding,
        loopback: Option<SharedLoopbackSuppressor>,
        suppression_enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner: LocalInputCapture::with_loopback(binding, loopback),
            suppression_enabled,
        }
    }
}

impl InputCapture for WindowsCapture {
    fn start(&mut self) -> Result<(), String> {
        self.inner.start()
    }

    fn poll(&mut self) -> Option<CaptureSignal> {
        self.inner.poll()
    }

    fn wait(&mut self) -> Option<CaptureSignal> {
        self.inner.wait()
    }

    fn set_suppression_enabled(&mut self, enabled: bool) {
        self.suppression_enabled.store(enabled, Ordering::SeqCst);
        self.inner.set_suppression_enabled(enabled);
    }
}

impl WindowsExclusiveCapture {
    pub fn with_loopback(
        binding: HotkeyBinding,
        loopback: Option<SharedLoopbackSuppressor>,
        suppression_enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            binding,
            loopback,
            receiver: None,
            suppression_enabled,
            started: false,
        }
    }
}

impl InputCapture for WindowsExclusiveCapture {
    fn start(&mut self) -> Result<(), String> {
        if self.started {
            return Ok(());
        }

        let (sender, receiver) = mpsc::channel();
        let binding = self.binding.clone();
        let loopback = self.loopback.clone();
        let suppression_enabled = Arc::clone(&self.suppression_enabled);
        self.receiver = Some(receiver);
        self.started = true;

        thread::spawn(move || {
            let tracker = Arc::new(Mutex::new(HotkeyTracker::new(binding)));
            let state = Arc::new(Mutex::new(CaptureState::default()));
            let pending_recenter = Arc::new(Mutex::new(None::<(f64, f64)>));

            let grab_tracker = Arc::clone(&tracker);
            let grab_state = Arc::clone(&state);
            let grab_pending_recenter = Arc::clone(&pending_recenter);
            let result = rdev::grab(move |event: rdev::Event| {
                if consume_pending_recenter_event(
                    &event,
                    &mut grab_pending_recenter.lock().unwrap(),
                ) {
                    return None;
                }

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
                            // Don't forward hotkey releases over the network, but ALWAYS pass them
                            // to the local OS to prevent modifier keys from getting stuck when releasing control.
                            Some(event)
                        }
                        CaptureSignal::Input(input) => {
                            let _ = sender.send(CaptureSignal::Input(input.clone()));
                            if suppression_enabled.load(Ordering::SeqCst) {
                                if matches!(input, InputEvent::MouseMove { .. }) {
                                    if let Some(center) = recenter_cursor_to_virtual_center() {
                                        state.last_mouse_position = Some(center);
                                        *grab_pending_recenter.lock().unwrap() = Some(center);
                                    } else {
                                        // Fall back to the old behavior if we fail to recenter.
                                        state.last_mouse_position = saved_mouse_position;
                                    }
                                } else {
                                    // The OS cursor stays in place for suppressed non-mouse-move
                                    // events, so preserve the pre-event coordinate baseline.
                                    state.last_mouse_position = saved_mouse_position;
                                }
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
                warn!(error = ?error, "Windows exclusive capture (grab) stopped");
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
        self.suppression_enabled.store(enabled, Ordering::SeqCst);
    }
}

fn consume_pending_recenter_event(
    event: &rdev::Event,
    pending_recenter: &mut Option<(f64, f64)>,
) -> bool {
    let Some(target) = pending_recenter.as_ref().copied() else {
        return false;
    };

    let rdev::EventType::MouseMove { x, y } = event.event_type else {
        return false;
    };

    if (x - target.0).abs() <= 1.0 && (y - target.1).abs() <= 1.0 {
        *pending_recenter = None;
        true
    } else {
        false
    }
}

fn recenter_cursor_to_virtual_center() -> Option<(f64, f64)> {
    let origin_x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let origin_y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

    if width <= 0 || height <= 0 {
        return None;
    }

    let center_x = origin_x + (width / 2);
    let center_y = origin_y + (height / 2);
    let success = unsafe { SetCursorPos(center_x, center_y) };
    if success == 0 {
        None
    } else {
        Some((f64::from(center_x), f64::from(center_y)))
    }
}

#[cfg(test)]
mod tests {
    use super::consume_pending_recenter_event;

    #[test]
    fn consumes_matching_recentering_move_once() {
        let mut pending = Some((500.0, 400.0));
        let event = rdev::Event {
            event_type: rdev::EventType::MouseMove { x: 500.0, y: 400.0 },
            time: std::time::SystemTime::now(),
            name: None,
        };

        assert!(consume_pending_recenter_event(&event, &mut pending));
        assert_eq!(pending, None);
    }

    #[test]
    fn ignores_unrelated_mouse_move() {
        let mut pending = Some((500.0, 400.0));
        let event = rdev::Event {
            event_type: rdev::EventType::MouseMove { x: 540.0, y: 400.0 },
            time: std::time::SystemTime::now(),
            name: None,
        };

        assert!(!consume_pending_recenter_event(&event, &mut pending));
        assert_eq!(pending, Some((500.0, 400.0)));
    }
}
