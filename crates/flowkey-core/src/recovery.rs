use flowkey_input::event::{InputEvent, Modifiers, MouseButton};
use flowkey_input::keycode::{parse_key_code, KeyCode, ModifierKind};
use flowkey_input::InputEventSink;
use tracing::warn;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryState {
    pub forced_key_releases: usize,
    pub forced_button_releases: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HeldKeyTracker {
    held_inputs: Vec<HeldInput>,
    modifiers: Modifiers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HeldInput {
    Key(String),
    Button(MouseButton),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectBackoff {
    current_secs: u64,
    initial_secs: u64,
    max_secs: u64,
}

impl ReconnectBackoff {
    pub fn new(initial_secs: u64, max_secs: u64) -> Self {
        let initial_secs = initial_secs.max(1);
        let max_secs = max_secs.max(initial_secs);

        Self {
            current_secs: initial_secs,
            initial_secs,
            max_secs,
        }
    }

    pub fn next_delay(&mut self) -> std::time::Duration {
        let delay = std::time::Duration::from_secs(self.current_secs);
        self.current_secs = (self.current_secs.saturating_mul(2)).min(self.max_secs);
        delay
    }

    pub fn reset(&mut self) {
        self.current_secs = self.initial_secs;
    }
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        Self::new(1, 8)
    }
}

impl HeldKeyTracker {
    pub fn observe(&mut self, event: &InputEvent) {
        match event {
            InputEvent::KeyDown { code, .. } => {
                self.hold_key(code);
                if let KeyCode::Modifier(kind) = parse_key_code(code) {
                    self.set_modifier(kind, true);
                }
            }
            InputEvent::KeyUp { code, .. } => {
                self.release_key(code);
                if let KeyCode::Modifier(kind) = parse_key_code(code) {
                    self.set_modifier(kind, false);
                }
            }
            InputEvent::MouseButtonDown { button, .. } => self.hold_button(*button),
            InputEvent::MouseButtonUp { button, .. } => self.release_button(*button),
            InputEvent::MouseMove { .. } | InputEvent::MouseWheel { .. } => {}
        }
    }

    pub fn release_all(&mut self, sink: &mut dyn InputEventSink) -> RecoveryState {
        let mut recovery = RecoveryState::default();
        let mut held_keys = Vec::new();
        let mut held_buttons = Vec::new();

        for held in self.held_inputs.iter().rev().cloned() {
            match held {
                HeldInput::Key(code) => held_keys.push(code),
                HeldInput::Button(button) => held_buttons.push(button),
            }
        }

        let mut cleared_modifiers = self.modifiers;
        for code in held_keys {
            let modifiers = self.modifiers_for_key_release(&code);
            let event = InputEvent::KeyUp {
                code: code.clone(),
                modifiers,
                timestamp_us: 0,
            };

            if let Err(error) = sink.handle(&event) {
                warn!(%error, code = %code, "failed to force key release");
            }

            recovery.forced_key_releases += 1;
            cleared_modifiers = modifiers;
        }

        self.modifiers = cleared_modifiers;

        for button in held_buttons {
            let event = InputEvent::MouseButtonUp {
                button,
                modifiers: self.modifiers,
                timestamp_us: 0,
            };

            if let Err(error) = sink.handle(&event) {
                warn!(%error, ?button, "failed to force mouse button release");
            }

            recovery.forced_button_releases += 1;
        }

        self.held_inputs.clear();
        self.modifiers = Modifiers::none();
        recovery
    }

    fn hold_key(&mut self, code: &str) {
        let held = HeldInput::Key(code.to_string());
        if !self.held_inputs.contains(&held) {
            self.held_inputs.push(held);
        }
    }

    fn release_key(&mut self, code: &str) {
        if let Some(index) = self
            .held_inputs
            .iter()
            .position(|held| matches!(held, HeldInput::Key(existing) if existing == code))
        {
            self.held_inputs.remove(index);
        }
    }

    fn hold_button(&mut self, button: MouseButton) {
        let held = HeldInput::Button(button);
        if !self.held_inputs.contains(&held) {
            self.held_inputs.push(held);
        }
    }

    fn release_button(&mut self, button: MouseButton) {
        if let Some(index) = self
            .held_inputs
            .iter()
            .position(|held| matches!(held, HeldInput::Button(existing) if *existing == button))
        {
            self.held_inputs.remove(index);
        }
    }

    fn set_modifier(&mut self, kind: ModifierKind, pressed: bool) {
        match kind {
            ModifierKind::Shift => self.modifiers.shift = pressed,
            ModifierKind::Control => self.modifiers.control = pressed,
            ModifierKind::Alt => self.modifiers.alt = pressed,
            ModifierKind::Meta => self.modifiers.meta = pressed,
        }
    }

    fn modifiers_for_key_release(&self, code: &str) -> Modifiers {
        let mut modifiers = self.modifiers;
        if let KeyCode::Modifier(kind) = parse_key_code(code) {
            match kind {
                ModifierKind::Shift => modifiers.shift = false,
                ModifierKind::Control => modifiers.control = false,
                ModifierKind::Alt => modifiers.alt = false,
                ModifierKind::Meta => modifiers.meta = false,
            }
        }
        modifiers
    }
}

#[cfg(test)]
mod tests {
    use super::{HeldKeyTracker, ReconnectBackoff, RecoveryState};
    use flowkey_input::event::{InputEvent, Modifiers, MouseButton};
    use flowkey_input::InputEventSink;

    #[derive(Default)]
    struct RecordingSink {
        handled_events: Vec<InputEvent>,
    }

    impl InputEventSink for RecordingSink {
        fn handle(&mut self, event: &InputEvent) -> Result<(), String> {
            self.handled_events.push(event.clone());
            Ok(())
        }

        fn release_all(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn reconnect_backoff_grows_then_caps() {
        let mut backoff = ReconnectBackoff::new(1, 8);

        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(1));
        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(2));
        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(4));
        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(8));
        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(8));
    }

    #[test]
    fn reconnect_backoff_resets_after_success() {
        let mut backoff = ReconnectBackoff::new(1, 8);

        backoff.next_delay();
        backoff.next_delay();
        backoff.reset();

        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(1));
    }

    #[test]
    fn held_key_tracker_flushes_in_reverse_order() {
        let mut tracker = HeldKeyTracker::default();
        let mut sink = RecordingSink::default();

        tracker.observe(&InputEvent::KeyDown {
            code: "ShiftLeft".to_string(),
            modifiers: Modifiers {
                shift: true,
                control: false,
                alt: false,
                meta: false,
            },
            timestamp_us: 10,
        });
        tracker.observe(&InputEvent::KeyDown {
            code: "KeyA".to_string(),
            modifiers: Modifiers {
                shift: true,
                control: false,
                alt: false,
                meta: false,
            },
            timestamp_us: 11,
        });
        tracker.observe(&InputEvent::MouseButtonDown {
            button: MouseButton::Left,
            modifiers: Modifiers {
                shift: true,
                control: false,
                alt: false,
                meta: false,
            },
            timestamp_us: 12,
        });

        let recovery = tracker.release_all(&mut sink);

        assert_eq!(
            recovery,
            RecoveryState {
                forced_key_releases: 2,
                forced_button_releases: 1,
            }
        );
        assert_eq!(
            sink.handled_events,
            vec![
                InputEvent::KeyUp {
                    code: "KeyA".to_string(),
                    modifiers: Modifiers {
                        shift: true,
                        control: false,
                        alt: false,
                        meta: false,
                    },
                    timestamp_us: 0,
                },
                InputEvent::KeyUp {
                    code: "ShiftLeft".to_string(),
                    modifiers: Modifiers::none(),
                    timestamp_us: 0,
                },
                InputEvent::MouseButtonUp {
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                    timestamp_us: 0,
                },
            ]
        );
    }

    #[test]
    fn held_key_tracker_drops_explicit_releases_before_flush() {
        let mut tracker = HeldKeyTracker::default();
        let mut sink = RecordingSink::default();

        tracker.observe(&InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers::none(),
            timestamp_us: 1,
        });
        tracker.observe(&InputEvent::KeyUp {
            code: "KeyK".to_string(),
            modifiers: Modifiers::none(),
            timestamp_us: 2,
        });

        let recovery = tracker.release_all(&mut sink);

        assert_eq!(recovery, RecoveryState::default());
        assert!(sink.handled_events.is_empty());
    }
}
