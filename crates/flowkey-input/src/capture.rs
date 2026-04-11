use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::event::{InputEvent, Modifiers};
use crate::hotkey::{HotkeyBinding, HotkeyOutcome, HotkeyTracker};
use crate::loopback::{lock_recovering, SharedLoopbackSuppressor};
use crate::normalize::{
    normalize_button, normalize_key_code, normalize_mouse_move_delta, normalize_wheel_delta,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSignal {
    Input(InputEvent),
    HotkeyPressed,
    HotkeySuppressed,
}

pub trait InputCapture: Send {
    fn start(&mut self) -> Result<(), String>;
    fn poll(&mut self) -> Option<CaptureSignal>;
    fn wait(&mut self) -> Option<CaptureSignal>;
    fn set_suppression_enabled(&mut self, enabled: bool);
}

#[derive(Debug)]
pub struct LocalInputCapture {
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    receiver: Option<Receiver<CaptureSignal>>,
    started: bool,
}

impl LocalInputCapture {
    pub fn new(binding: HotkeyBinding) -> Self {
        Self::with_loopback(binding, None)
    }

    pub fn with_loopback(
        binding: HotkeyBinding,
        loopback: Option<SharedLoopbackSuppressor>,
    ) -> Self {
        Self {
            binding,
            loopback,
            receiver: None,
            started: false,
        }
    }
}

impl InputCapture for LocalInputCapture {
    fn start(&mut self) -> Result<(), String> {
        if self.started {
            return Ok(());
        }

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let (sender, receiver) = mpsc::channel();
            let binding = self.binding.clone();
            let loopback = self.loopback.clone();
            self.receiver = Some(receiver);
            self.started = true;

            thread::spawn(move || {
                let mut tracker = HotkeyTracker::new(binding);
                let mut state = CaptureState::default();

                let result = rdev::listen(move |event| {
                    if let Some(signal) = state.translate(event, &mut tracker, loopback.as_ref()) {
                        if !matches!(signal, CaptureSignal::HotkeySuppressed) {
                            let _ = sender.send(signal);
                        }
                    }
                });

                if let Err(error) = result {
                    tracing::warn!(error = ?error, "local input capture listener stopped");
                }
            });

            return Ok(());
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = &self.binding;
            Err("local input capture is only implemented on macOS and Windows".to_string())
        }
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

    fn set_suppression_enabled(&mut self, _enabled: bool) {
        // LocalInputCapture is passive and does not support suppression
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Debug, Default)]
pub struct CaptureState {
    pub last_mouse_position: Option<(f64, f64)>,
    pub modifiers: Modifiers,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl CaptureState {
    pub fn translate(
        &mut self,
        event: rdev::Event,
        tracker: &mut HotkeyTracker,
        loopback: Option<&SharedLoopbackSuppressor>,
    ) -> Option<CaptureSignal> {
        let input = self.translate_event(event)?;

        if let Some(loopback) = loopback {
            let mut loopback = lock_recovering(loopback);
            if loopback.should_suppress(&input) {
                return None;
            }
        }

        match tracker.process(&input) {
            HotkeyOutcome::Pressed => return Some(CaptureSignal::HotkeyPressed),
            HotkeyOutcome::Suppressed => return Some(CaptureSignal::HotkeySuppressed),
            HotkeyOutcome::Forward => {}
        }

        Some(CaptureSignal::Input(input))
    }

    pub fn translate_event(&mut self, event: rdev::Event) -> Option<InputEvent> {
        let modifiers = self.modifiers;
        let timestamp_us = event
            .time
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        match event.event_type {
            rdev::EventType::KeyPress(key) => self.translate_key_event(key, true, timestamp_us),
            rdev::EventType::KeyRelease(key) => self.translate_key_event(key, false, timestamp_us),
            rdev::EventType::ButtonPress(button) => Some(InputEvent::MouseButtonDown {
                button: normalize_button(button),
                modifiers,
                timestamp_us,
            }),
            rdev::EventType::ButtonRelease(button) => Some(InputEvent::MouseButtonUp {
                button: normalize_button(button),
                modifiers,
                timestamp_us,
            }),
            rdev::EventType::MouseMove { x, y } => {
                let last_position = self.last_mouse_position;
                self.last_mouse_position = Some((x, y));
                let delta = normalize_mouse_move_delta(last_position, x, y)?;
                Some(InputEvent::MouseMove {
                    dx: delta.0,
                    dy: delta.1,
                    modifiers,
                    timestamp_us,
                })
            }
            rdev::EventType::Wheel { delta_x, delta_y } => {
                let (delta_x, delta_y) = normalize_wheel_delta(delta_x as f64, delta_y as f64)?;
                Some(InputEvent::MouseWheel {
                    delta_x,
                    delta_y,
                    modifiers,
                    timestamp_us,
                })
            }
        }
    }

    pub fn translate_key_event(
        &mut self,
        key: rdev::Key,
        pressed: bool,
        timestamp_us: u64,
    ) -> Option<InputEvent> {
        let code = normalize_key_code(key)?;
        let mut modifiers = self.modifiers;

        match key {
            rdev::Key::ShiftLeft | rdev::Key::ShiftRight => modifiers.shift = pressed,
            rdev::Key::ControlLeft | rdev::Key::ControlRight => modifiers.control = pressed,
            rdev::Key::Alt | rdev::Key::AltGr => modifiers.alt = pressed,
            rdev::Key::MetaLeft | rdev::Key::MetaRight => modifiers.meta = pressed,
            _ => {}
        }

        self.modifiers = modifiers;
        if pressed {
            Some(InputEvent::KeyDown {
                code,
                modifiers,
                timestamp_us,
            })
        } else {
            Some(InputEvent::KeyUp {
                code,
                modifiers,
                timestamp_us,
            })
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
mod tests {
    use super::CaptureState;
    use crate::event::InputEvent;
    use crate::event::MouseButton;
    use crate::normalize::{normalize_button, normalize_key_code};

    #[test]
    fn translates_basic_keys_to_protocol_codes() {
        assert_eq!(
            normalize_key_code(rdev::Key::KeyK),
            Some("KeyK".to_string())
        );
        assert_eq!(
            normalize_key_code(rdev::Key::Num3),
            Some("Digit3".to_string())
        );
        assert_eq!(
            normalize_key_code(rdev::Key::ControlLeft),
            Some("ControlLeft".to_string())
        );
    }

    #[test]
    fn translates_mouse_buttons() {
        assert_eq!(normalize_button(rdev::Button::Left), MouseButton::Left);
        assert_eq!(normalize_button(rdev::Button::Right), MouseButton::Right);
        assert_eq!(normalize_button(rdev::Button::Middle), MouseButton::Middle);
    }

    #[test]
    fn first_mouse_move_initializes_position_and_second_emits_delta() {
        let mut state = CaptureState::default();

        let first = state.translate_event(rdev::Event {
            event_type: rdev::EventType::MouseMove { x: 100.0, y: 200.0 },
            time: std::time::SystemTime::now(),
            name: None,
        });
        assert_eq!(first, None);

        let second = state.translate_event(rdev::Event {
            event_type: rdev::EventType::MouseMove { x: 104.0, y: 197.0 },
            time: std::time::SystemTime::now(),
            name: None,
        });

        match second {
            Some(InputEvent::MouseMove {
                dx,
                dy,
                modifiers,
                timestamp_us,
            }) => {
                assert_eq!(dx, 4);
                assert_eq!(dy, -3);
                assert_eq!(modifiers, crate::event::Modifiers::none());
                assert!(timestamp_us > 0);
            }
            _ => panic!("Expected MouseMove event, got {:?}", second),
        }
    }
}
