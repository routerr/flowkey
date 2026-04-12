use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Modifiers {
    pub const fn none() -> Self {
        Self {
            shift: false,
            control: false,
            alt: false,
            meta: false,
        }
    }
}

impl Default for Modifiers {
    fn default() -> Self {
        Self::none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    KeyDown {
        code: String,
        modifiers: Modifiers,
        timestamp_us: u64,
    },
    KeyUp {
        code: String,
        modifiers: Modifiers,
        timestamp_us: u64,
    },
    MouseMove {
        dx: i32,
        dy: i32,
        modifiers: Modifiers,
        timestamp_us: u64,
    },
    MouseButtonDown {
        button: MouseButton,
        modifiers: Modifiers,
        timestamp_us: u64,
    },
    MouseButtonUp {
        button: MouseButton,
        modifiers: Modifiers,
        timestamp_us: u64,
    },
    MouseWheel {
        delta_x: i32,
        delta_y: i32,
        modifiers: Modifiers,
        timestamp_us: u64,
    },
}

impl InputEvent {
    /// Compare two events ignoring `timestamp_us`.
    ///
    /// The loopback suppressor records injected events with the remote
    /// timestamp, but the local capture generates a fresh local timestamp.
    /// This method allows matching on everything except the timestamp.
    pub fn matches_ignoring_timestamp(&self, other: &Self) -> bool {
        match (self, other) {
            (
                InputEvent::KeyDown {
                    code: c1,
                    modifiers: m1,
                    ..
                },
                InputEvent::KeyDown {
                    code: c2,
                    modifiers: m2,
                    ..
                },
            ) => c1 == c2 && m1 == m2,
            (
                InputEvent::KeyUp {
                    code: c1,
                    modifiers: m1,
                    ..
                },
                InputEvent::KeyUp {
                    code: c2,
                    modifiers: m2,
                    ..
                },
            ) => c1 == c2 && m1 == m2,
            (
                InputEvent::MouseMove {
                    dx: dx1,
                    dy: dy1,
                    modifiers: m1,
                    ..
                },
                InputEvent::MouseMove {
                    dx: dx2,
                    dy: dy2,
                    modifiers: m2,
                    ..
                },
            ) => dx1 == dx2 && dy1 == dy2 && m1 == m2,
            (
                InputEvent::MouseButtonDown {
                    button: b1,
                    modifiers: m1,
                    ..
                },
                InputEvent::MouseButtonDown {
                    button: b2,
                    modifiers: m2,
                    ..
                },
            ) => b1 == b2 && m1 == m2,
            (
                InputEvent::MouseButtonUp {
                    button: b1,
                    modifiers: m1,
                    ..
                },
                InputEvent::MouseButtonUp {
                    button: b2,
                    modifiers: m2,
                    ..
                },
            ) => b1 == b2 && m1 == m2,
            (
                InputEvent::MouseWheel {
                    delta_x: dx1,
                    delta_y: dy1,
                    modifiers: m1,
                    ..
                },
                InputEvent::MouseWheel {
                    delta_x: dx2,
                    delta_y: dy2,
                    modifiers: m2,
                    ..
                },
            ) => dx1 == dx2 && dy1 == dy2 && m1 == m2,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InputEvent, Modifiers, MouseButton};

    #[test]
    fn input_event_round_trips_through_toml() {
        let event = InputEvent::MouseButtonDown {
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            timestamp_us: 123456789,
        };

        let encoded = toml::to_string(&event).expect("event should serialize");
        let decoded: InputEvent = toml::from_str(&encoded).expect("event should deserialize");

        assert_eq!(decoded, event);
    }

    #[test]
    fn modifiers_none_is_clear() {
        assert_eq!(
            Modifiers::none(),
            Modifiers {
                shift: false,
                control: false,
                alt: false,
                meta: false,
            }
        );
    }

    #[test]
    fn event_match_can_ignore_timestamp_for_keyboard_events() {
        let first = InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: true,
                alt: false,
                meta: false,
            },
            timestamp_us: 1,
        };
        let second = InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: true,
                alt: false,
                meta: false,
            },
            timestamp_us: 2,
        };

        assert!(first.matches_ignoring_timestamp(&second));
    }

    #[test]
    fn event_match_still_rejects_different_payloads() {
        let first = InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers::none(),
            timestamp_us: 1,
        };
        let second = InputEvent::KeyDown {
            code: "KeyL".to_string(),
            modifiers: Modifiers::none(),
            timestamp_us: 1,
        };

        assert!(!first.matches_ignoring_timestamp(&second));
    }
}
