use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::event::{InputEvent, Modifiers};
use crate::hotkey::{HotkeyBinding, HotkeyOutcome, HotkeyTracker};
use crate::loopback::{lock_recovering, SharedLoopbackSuppressor};
use crate::normalize::{
    normalize_button, normalize_key_code, normalize_mouse_move_delta, normalize_wheel_delta,
};
#[cfg(target_os = "windows")]
use tracing::debug;

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
    fn capture_restart_counter(&self) -> Option<Arc<AtomicU64>> {
        None
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn supervise_local_input_capture<F>(
    restart_count: Arc<AtomicU64>,
    mut spawn_listener: F,
    shutdown: Option<Arc<AtomicBool>>,
) where
    F: FnMut() -> thread::JoinHandle<()> + Send + 'static,
{
    let backoff = [
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(5),
        Duration::from_secs(10),
    ];
    let mut backoff_index = 0usize;

    loop {
        if shutdown
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            break;
        }

        let listener = spawn_listener();
        let exit_reason = match listener.join() {
            Ok(()) => "exited unexpectedly",
            Err(error) => {
                tracing::warn!(error = ?error, "local input capture listener panicked; restarting");
                "panicked"
            }
        };

        if shutdown
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            break;
        }

        restart_count.fetch_add(1, Ordering::SeqCst);
        tracing::warn!(
            reason = exit_reason,
            restart = restart_count.load(Ordering::SeqCst),
            "local input capture listener restarting"
        );

        let delay = backoff[backoff_index];
        if backoff_index + 1 < backoff.len() {
            backoff_index += 1;
        }
        thread::sleep(delay);
    }
}

#[derive(Debug)]
pub struct LocalInputCapture {
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    receiver: Option<Receiver<CaptureSignal>>,
    started: bool,
    restart_count: Arc<AtomicU64>,
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
            restart_count: Arc::new(AtomicU64::new(0)),
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
            let restart_count = Arc::clone(&self.restart_count);
            self.receiver = Some(receiver);
            self.started = true;

            thread::spawn(move || {
                supervise_local_input_capture(
                    restart_count,
                    move || {
                        spawn_local_input_listener(
                            binding.clone(),
                            loopback.clone(),
                            sender.clone(),
                        )
                    },
                    None,
                );
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

    fn capture_restart_counter(&self) -> Option<Arc<AtomicU64>> {
        Some(Arc::clone(&self.restart_count))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn spawn_local_input_listener(
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    sender: std::sync::mpsc::Sender<CaptureSignal>,
) -> thread::JoinHandle<()> {
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
    })
}

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
mod supervisor_tests {
    use super::supervise_local_input_capture;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn restarts_once_after_listener_panic() {
        let restart_count = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let listener_attempts = Arc::new(AtomicUsize::new(0));
        let allow_second_listener_to_exit = Arc::new(AtomicBool::new(false));

        let supervisor_restart_count = Arc::clone(&restart_count);
        let supervisor_shutdown = Arc::clone(&shutdown);
        let attempts = Arc::clone(&listener_attempts);
        let release_second = Arc::clone(&allow_second_listener_to_exit);

        let supervisor = thread::spawn(move || {
            supervise_local_input_capture(
                supervisor_restart_count,
                move || {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    let release_second = Arc::clone(&release_second);
                    thread::spawn(move || {
                        if attempt == 0 {
                            panic!("intentional capture panic");
                        }

                        while !release_second.load(Ordering::SeqCst) {
                            thread::sleep(Duration::from_millis(10));
                        }
                    })
                },
                Some(supervisor_shutdown),
            );
        });

        while listener_attempts.load(Ordering::SeqCst) < 2 {
            thread::sleep(Duration::from_millis(10));
        }

        shutdown.store(true, Ordering::SeqCst);
        allow_second_listener_to_exit.store(true, Ordering::SeqCst);
        supervisor
            .join()
            .expect("supervisor thread should exit cleanly");

        assert_eq!(restart_count.load(Ordering::SeqCst), 1);
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Debug, Default)]
pub struct CaptureState {
    pub last_mouse_position: Option<(f64, f64)>,
    pub modifiers: Modifiers,
    tracker: ModifierTracker,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ModifierTracker {
    shift_l: bool,
    shift_r: bool,
    ctrl_l: bool,
    ctrl_r: bool,
    alt_l: bool,
    alt_r: bool,
    meta_l: bool,
    meta_r: bool,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl ModifierTracker {
    fn to_modifiers(&self) -> Modifiers {
        Modifiers {
            shift: self.shift_l || self.shift_r,
            control: self.ctrl_l || self.ctrl_r,
            alt: self.alt_l || self.alt_r,
            meta: self.meta_l || self.meta_r,
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl CaptureState {
    pub fn sync_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
        // Best effort: set the Left variant to match the aggregate state.
        // This ensures the next release event correctly clears the bit.
        self.tracker.shift_l = modifiers.shift;
        self.tracker.ctrl_l = modifiers.control;
        self.tracker.alt_l = modifiers.alt;
        self.tracker.meta_l = modifiers.meta;
    }

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
        let code = match normalize_key_code(key) {
            Some(code) => code.to_string(),
            None => {
                tracing::warn!(target: "keyboard_trace", physical_key = ?key, "rdev key not mapped, dropping");
                return None;
            }
        };

        match key {
            rdev::Key::ShiftLeft => self.tracker.shift_l = pressed,
            rdev::Key::ShiftRight => self.tracker.shift_r = pressed,
            rdev::Key::ControlLeft => self.tracker.ctrl_l = pressed,
            rdev::Key::ControlRight => self.tracker.ctrl_r = pressed,
            rdev::Key::Alt => self.tracker.alt_l = pressed,
            rdev::Key::AltGr => self.tracker.alt_r = pressed,
            rdev::Key::MetaLeft => self.tracker.meta_l = pressed,
            rdev::Key::MetaRight => self.tracker.meta_r = pressed,
            _ => {}
        }

        self.modifiers = self.tracker.to_modifiers();
        let modifiers = self.modifiers;
        #[cfg(target_os = "windows")]
        debug!(
            target: "keyboard_trace",
            platform = "windows",
            physical_key = ?key,
            code = %code,
            pressed,
            shift = modifiers.shift,
            control = modifiers.control,
            alt = modifiers.alt,
            meta = modifiers.meta,
            timestamp_us,
            "captured keyboard event"
        );
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
        assert_eq!(normalize_key_code(rdev::Key::KeyK), Some("KeyK"));
        assert_eq!(normalize_key_code(rdev::Key::Num3), Some("Digit3"));
        assert_eq!(
            normalize_key_code(rdev::Key::ControlLeft),
            Some("ControlLeft")
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
