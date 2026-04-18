use crate::event::MouseButton;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn normalize_mouse_move_delta(
    last_position: Option<(f64, f64)>,
    x: f64,
    y: f64,
) -> Option<(i32, i32)> {
    let (last_x, last_y) = last_position?;
    let dx = round_delta(x - last_x);
    let dy = round_delta(y - last_y);

    if dx == 0 && dy == 0 {
        None
    } else {
        Some((dx, dy))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn normalize_wheel_delta(delta_x: f64, delta_y: f64) -> Option<(i32, i32)> {
    let dx = round_delta(delta_x);
    let dy = round_delta(delta_y);

    if dx == 0 && dy == 0 {
        None
    } else {
        Some((dx, dy))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn normalize_key_code(key: rdev::Key) -> Option<&'static str> {
    match key {
        rdev::Key::KeyA => Some("KeyA"),
        rdev::Key::KeyB => Some("KeyB"),
        rdev::Key::KeyC => Some("KeyC"),
        rdev::Key::KeyD => Some("KeyD"),
        rdev::Key::KeyE => Some("KeyE"),
        rdev::Key::KeyF => Some("KeyF"),
        rdev::Key::KeyG => Some("KeyG"),
        rdev::Key::KeyH => Some("KeyH"),
        rdev::Key::KeyI => Some("KeyI"),
        rdev::Key::KeyJ => Some("KeyJ"),
        rdev::Key::KeyK => Some("KeyK"),
        rdev::Key::KeyL => Some("KeyL"),
        rdev::Key::KeyM => Some("KeyM"),
        rdev::Key::KeyN => Some("KeyN"),
        rdev::Key::KeyO => Some("KeyO"),
        rdev::Key::KeyP => Some("KeyP"),
        rdev::Key::KeyQ => Some("KeyQ"),
        rdev::Key::KeyR => Some("KeyR"),
        rdev::Key::KeyS => Some("KeyS"),
        rdev::Key::KeyT => Some("KeyT"),
        rdev::Key::KeyU => Some("KeyU"),
        rdev::Key::KeyV => Some("KeyV"),
        rdev::Key::KeyW => Some("KeyW"),
        rdev::Key::KeyX => Some("KeyX"),
        rdev::Key::KeyY => Some("KeyY"),
        rdev::Key::KeyZ => Some("KeyZ"),
        rdev::Key::Num0 => Some("Digit0"),
        rdev::Key::Num1 => Some("Digit1"),
        rdev::Key::Num2 => Some("Digit2"),
        rdev::Key::Num3 => Some("Digit3"),
        rdev::Key::Num4 => Some("Digit4"),
        rdev::Key::Num5 => Some("Digit5"),
        rdev::Key::Num6 => Some("Digit6"),
        rdev::Key::Num7 => Some("Digit7"),
        rdev::Key::Num8 => Some("Digit8"),
        rdev::Key::Num9 => Some("Digit9"),
        rdev::Key::BackQuote => Some("Backquote"),
        rdev::Key::Minus => Some("Minus"),
        rdev::Key::Equal => Some("Equal"),
        rdev::Key::LeftBracket => Some("BracketLeft"),
        rdev::Key::RightBracket => Some("BracketRight"),
        rdev::Key::BackSlash => Some("Backslash"),
        rdev::Key::SemiColon => Some("Semicolon"),
        rdev::Key::Quote => Some("Quote"),
        rdev::Key::Comma => Some("Comma"),
        rdev::Key::Dot => Some("Period"),
        rdev::Key::Slash => Some("Slash"),
        rdev::Key::Return => Some("Enter"),
        rdev::Key::KpReturn => Some("NumpadEnter"),
        rdev::Key::KpMinus => Some("NumpadSubtract"),
        rdev::Key::KpPlus => Some("NumpadAdd"),
        rdev::Key::KpMultiply => Some("NumpadMultiply"),
        rdev::Key::KpDivide => Some("NumpadDivide"),
        rdev::Key::Kp0 => Some("Numpad0"),
        rdev::Key::Kp1 => Some("Numpad1"),
        rdev::Key::Kp2 => Some("Numpad2"),
        rdev::Key::Kp3 => Some("Numpad3"),
        rdev::Key::Kp4 => Some("Numpad4"),
        rdev::Key::Kp5 => Some("Numpad5"),
        rdev::Key::Kp6 => Some("Numpad6"),
        rdev::Key::Kp7 => Some("Numpad7"),
        rdev::Key::Kp8 => Some("Numpad8"),
        rdev::Key::Kp9 => Some("Numpad9"),
        rdev::Key::KpDelete => Some("NumpadDecimal"),
        rdev::Key::Tab => Some("Tab"),
        rdev::Key::Space => Some("Space"),
        rdev::Key::Escape => Some("Escape"),
        rdev::Key::Backspace => Some("Backspace"),
        rdev::Key::Home => Some("Home"),
        rdev::Key::End => Some("End"),
        rdev::Key::PageUp => Some("PageUp"),
        rdev::Key::PageDown => Some("PageDown"),
        rdev::Key::LeftArrow => Some("ArrowLeft"),
        rdev::Key::RightArrow => Some("ArrowRight"),
        rdev::Key::UpArrow => Some("ArrowUp"),
        rdev::Key::DownArrow => Some("ArrowDown"),
        rdev::Key::Insert => Some("Insert"),
        rdev::Key::Delete => Some("Delete"),
        rdev::Key::CapsLock => Some("CapsLock"),
        rdev::Key::NumLock => Some("NumLock"),
        rdev::Key::ScrollLock => Some("ScrollLock"),
        rdev::Key::PrintScreen => Some("PrintScreen"),
        rdev::Key::Pause => Some("Pause"),
        rdev::Key::F1 => Some("F1"),
        rdev::Key::F2 => Some("F2"),
        rdev::Key::F3 => Some("F3"),
        rdev::Key::F4 => Some("F4"),
        rdev::Key::F5 => Some("F5"),
        rdev::Key::F6 => Some("F6"),
        rdev::Key::F7 => Some("F7"),
        rdev::Key::F8 => Some("F8"),
        rdev::Key::F9 => Some("F9"),
        rdev::Key::F10 => Some("F10"),
        rdev::Key::F11 => Some("F11"),
        rdev::Key::F12 => Some("F12"),
        rdev::Key::ShiftLeft => Some("ShiftLeft"),
        rdev::Key::ShiftRight => Some("ShiftRight"),
        rdev::Key::ControlLeft => Some("ControlLeft"),
        rdev::Key::ControlRight => Some("ControlRight"),
        rdev::Key::Alt => Some("AltLeft"),
        rdev::Key::AltGr => Some("AltRight"),
        rdev::Key::MetaLeft => Some("MetaLeft"),
        rdev::Key::MetaRight => Some("MetaRight"),
        _ => None,
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn normalize_button(button: rdev::Button) -> MouseButton {
    match button {
        rdev::Button::Left => MouseButton::Left,
        rdev::Button::Right => MouseButton::Right,
        rdev::Button::Middle => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn round_delta(value: f64) -> i32 {
    let rounded = value.round();
    if rounded > i32::MAX as f64 {
        i32::MAX
    } else if rounded < i32::MIN as f64 {
        i32::MIN
    } else {
        rounded as i32
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    use super::{
        normalize_button, normalize_key_code, normalize_mouse_move_delta, normalize_wheel_delta,
    };
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    use crate::event::MouseButton;

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn normalizes_mouse_motion_to_relative_deltas() {
        assert_eq!(
            normalize_mouse_move_delta(Some((10.0, 20.0)), 13.6, 15.4),
            Some((4, -5))
        );
        assert_eq!(normalize_mouse_move_delta(Some((4.0, 8.0)), 4.2, 8.2), None);
        assert_eq!(normalize_mouse_move_delta(None, 4.2, 8.2), None);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn normalizes_wheel_motion_by_rounding_and_dropping_zeroes() {
        assert_eq!(normalize_wheel_delta(0.49, 1.51), Some((0, 2)));
        assert_eq!(normalize_wheel_delta(0.2, -0.3), None);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn normalizes_key_and_button_aliases() {
        assert_eq!(normalize_key_code(rdev::Key::Alt), Some("AltLeft"));
        assert_eq!(
            normalize_key_code(rdev::Key::ControlRight),
            Some("ControlRight")
        );
        assert_eq!(normalize_button(rdev::Button::Middle), MouseButton::Middle);
    }
}
