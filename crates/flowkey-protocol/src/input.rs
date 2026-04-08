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
}
