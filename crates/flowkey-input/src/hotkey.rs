use crate::event::{InputEvent, Modifiers};
use crate::keycode::{parse_key_code, KeyCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyOutcome {
    Pressed,
    Suppressed,
    Forward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyBinding {
    pub code: KeyCode,
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyTracker {
    binding: HotkeyBinding,
    latched: bool,
    suppress_remaining: usize,
}

impl HotkeyBinding {
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut modifiers = Modifiers::none();
        let mut key: Option<KeyCode> = None;

        for raw_part in value.split('+') {
            let part = raw_part.trim();
            if part.is_empty() {
                continue;
            }

            match part.to_ascii_lowercase().as_str() {
                "shift" => modifiers.shift = true,
                "ctrl" | "control" => modifiers.control = true,
                "alt" | "option" => modifiers.alt = true,
                "meta" | "cmd" | "command" | "win" | "windows" | "super" => modifiers.meta = true,
                _ => {
                    if key.is_some() {
                        return Err("hotkey must contain exactly one non-modifier key".to_string());
                    }

                    let parsed = parse_key_code(part);
                    if matches!(parsed, KeyCode::Unmapped(_)) {
                        return Err(format!("unsupported hotkey key: {part}"));
                    }
                    key = Some(parsed);
                }
            }
        }

        let code = key.ok_or_else(|| "hotkey is missing a non-modifier key".to_string())?;
        Ok(Self { code, modifiers })
    }

    pub fn component_count(&self) -> usize {
        1 + [
            self.modifiers.shift,
            self.modifiers.control,
            self.modifiers.alt,
            self.modifiers.meta,
        ]
        .into_iter()
        .filter(|pressed| *pressed)
        .count()
    }

    pub fn matches(&self, event: &InputEvent) -> bool {
        matches!(
            event,
            InputEvent::KeyDown { code, modifiers, .. }
                if self.code_matches(code) && *modifiers == self.modifiers
        )
    }

    pub fn code_matches(&self, code: &str) -> bool {
        parse_key_code(code) == self.code
    }

    fn matches_released_component(&self, event: &InputEvent) -> bool {
        match event {
            InputEvent::KeyUp { code, .. } => match parse_key_code(code) {
                KeyCode::Character(_) => self.code_matches(code),
                KeyCode::Named(_) => self.code_matches(code),
                KeyCode::Modifier(kind) => match kind {
                    crate::keycode::ModifierKind::Shift => self.modifiers.shift,
                    crate::keycode::ModifierKind::Control => self.modifiers.control,
                    crate::keycode::ModifierKind::Alt => self.modifiers.alt,
                    crate::keycode::ModifierKind::Meta => self.modifiers.meta,
                },
                KeyCode::Unmapped(_) => false,
            },
            _ => false,
        }
    }
}

impl HotkeyTracker {
    pub fn new(binding: HotkeyBinding) -> Self {
        Self {
            binding,
            latched: false,
            suppress_remaining: 0,
        }
    }

    pub fn process(&mut self, event: &InputEvent) -> HotkeyOutcome {
        if self.suppress_remaining > 0 {
            if self.binding.matches_released_component(event) {
                self.suppress_remaining = self.suppress_remaining.saturating_sub(1);
                if let InputEvent::KeyUp { code, .. } = event {
                    if self.binding.code_matches(code) {
                        self.latched = false;
                    }
                }
                return HotkeyOutcome::Suppressed;
            }

            if self.binding.matches(event) {
                if !self.latched {
                    tracing::info!(target: "hotkey", "hotkey triggered: {:?}", self.binding.code);
                    self.latched = true;
                    self.suppress_remaining = self.binding.component_count();
                    return HotkeyOutcome::Pressed;
                }

                return HotkeyOutcome::Suppressed;
            }

            return HotkeyOutcome::Forward;
        }

        match event {
            InputEvent::KeyDown {
                code, modifiers, ..
            } => {
                let code_matches = self.binding.code_matches(code);
                let modifiers_match = *modifiers == self.binding.modifiers;

                if !self.latched && code_matches && modifiers_match {
                    tracing::info!(target: "hotkey", "hotkey triggered: {:?}", self.binding.code);
                    self.latched = true;
                    self.suppress_remaining = self.binding.component_count();
                    return HotkeyOutcome::Pressed;
                }

                if code_matches
                    || modifiers.shift
                    || modifiers.control
                    || modifiers.alt
                    || modifiers.meta
                {
                    tracing::debug!(
                        target: "hotkey",
                        code = %code,
                        code_matches,
                        modifiers_match,
                        modifiers = ?modifiers,
                        expected = ?self.binding.modifiers,
                        latched = self.latched,
                        "hotkey matching failed"
                    );
                }
            }
            InputEvent::KeyUp { code, .. } => {
                if self.binding.code_matches(code) {
                    self.latched = false;
                }
            }
            _ => {}
        }

        HotkeyOutcome::Forward
    }
}

#[cfg(test)]
mod tests {
    use super::{HotkeyBinding, HotkeyOutcome, HotkeyTracker};
    use crate::event::{InputEvent, Modifiers};

    #[test]
    fn parses_common_hotkey_syntax() {
        let binding = HotkeyBinding::parse("Ctrl+Alt+Shift+K").expect("binding should parse");

        assert_eq!(
            binding,
            HotkeyBinding {
                code: crate::keycode::KeyCode::Character('k'),
                modifiers: Modifiers {
                    shift: true,
                    control: true,
                    alt: true,
                    meta: false,
                },
            }
        );
    }

    #[test]
    fn matches_the_pressed_hotkey_key() {
        let binding = HotkeyBinding::parse("Ctrl+Alt+Shift+K").expect("binding should parse");

        assert!(binding.matches(&InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: true,
                control: true,
                alt: true,
                meta: false,
            },
            timestamp_us: 0,
        }));
    }

    #[test]
    fn tracker_latches_until_key_release() {
        let binding = HotkeyBinding::parse("Ctrl+Alt+Shift+K").expect("binding should parse");
        let mut tracker = HotkeyTracker::new(binding);

        let press = InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: true,
                control: true,
                alt: true,
                meta: false,
            },
            timestamp_us: 0,
        };
        let release = InputEvent::KeyUp {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: false,
                alt: false,
                meta: false,
            },
            timestamp_us: 0,
        };
        let release_shift = InputEvent::KeyUp {
            code: "ShiftLeft".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: true,
                alt: true,
                meta: false,
            },
            timestamp_us: 0,
        };
        let release_control = InputEvent::KeyUp {
            code: "ControlLeft".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: false,
                alt: true,
                meta: false,
            },
            timestamp_us: 0,
        };
        let release_alt = InputEvent::KeyUp {
            code: "AltLeft".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: false,
                alt: false,
                meta: false,
            },
            timestamp_us: 0,
        };

        assert!(matches!(tracker.process(&press), HotkeyOutcome::Pressed));
        assert!(matches!(tracker.process(&press), HotkeyOutcome::Suppressed));
        assert!(matches!(
            tracker.process(&release),
            HotkeyOutcome::Suppressed
        ));
        assert!(matches!(
            tracker.process(&release_shift),
            HotkeyOutcome::Suppressed
        ));
        assert!(matches!(
            tracker.process(&release_control),
            HotkeyOutcome::Suppressed
        ));
        assert!(matches!(
            tracker.process(&release_alt),
            HotkeyOutcome::Suppressed
        ));
        assert!(matches!(tracker.process(&press), HotkeyOutcome::Pressed));
    }

    #[test]
    fn tracker_suppresses_the_activation_chord_release_sequence() {
        let binding = HotkeyBinding::parse("Ctrl+Alt+Shift+K").expect("binding should parse");
        let mut tracker = HotkeyTracker::new(binding);

        let press = InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: true,
                control: true,
                alt: true,
                meta: false,
            },
            timestamp_us: 0,
        };
        let release_key = InputEvent::KeyUp {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: true,
                control: true,
                alt: true,
                meta: false,
            },
            timestamp_us: 0,
        };
        let release_shift = InputEvent::KeyUp {
            code: "ShiftLeft".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: true,
                alt: true,
                meta: false,
            },
            timestamp_us: 0,
        };

        assert!(matches!(
            tracker.process(&press),
            super::HotkeyOutcome::Pressed
        ));
        assert!(matches!(
            tracker.process(&release_key),
            super::HotkeyOutcome::Suppressed
        ));
        assert!(matches!(
            tracker.process(&release_shift),
            super::HotkeyOutcome::Suppressed
        ));
    }

    #[test]
    fn tracker_forwards_unrelated_keys_while_waiting_for_chord_releases() {
        let binding = HotkeyBinding::parse("Ctrl+Alt+Shift+K").expect("binding should parse");
        let mut tracker = HotkeyTracker::new(binding);

        let press_hotkey = InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: true,
                control: true,
                alt: true,
                meta: false,
            },
            timestamp_us: 0,
        };
        let unrelated = InputEvent::KeyDown {
            code: "Backspace".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: false,
                alt: false,
                meta: false,
            },
            timestamp_us: 1,
        };

        assert!(matches!(
            tracker.process(&press_hotkey),
            super::HotkeyOutcome::Pressed
        ));
        assert!(matches!(
            tracker.process(&unrelated),
            super::HotkeyOutcome::Forward
        ));
    }
}
