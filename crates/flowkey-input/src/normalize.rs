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
pub fn normalize_key_code(key: rdev::Key) -> Option<String> {
    match key {
        rdev::Key::KeyA => Some("KeyA".to_string()),
        rdev::Key::KeyB => Some("KeyB".to_string()),
        rdev::Key::KeyC => Some("KeyC".to_string()),
        rdev::Key::KeyD => Some("KeyD".to_string()),
        rdev::Key::KeyE => Some("KeyE".to_string()),
        rdev::Key::KeyF => Some("KeyF".to_string()),
        rdev::Key::KeyG => Some("KeyG".to_string()),
        rdev::Key::KeyH => Some("KeyH".to_string()),
        rdev::Key::KeyI => Some("KeyI".to_string()),
        rdev::Key::KeyJ => Some("KeyJ".to_string()),
        rdev::Key::KeyK => Some("KeyK".to_string()),
        rdev::Key::KeyL => Some("KeyL".to_string()),
        rdev::Key::KeyM => Some("KeyM".to_string()),
        rdev::Key::KeyN => Some("KeyN".to_string()),
        rdev::Key::KeyO => Some("KeyO".to_string()),
        rdev::Key::KeyP => Some("KeyP".to_string()),
        rdev::Key::KeyQ => Some("KeyQ".to_string()),
        rdev::Key::KeyR => Some("KeyR".to_string()),
        rdev::Key::KeyS => Some("KeyS".to_string()),
        rdev::Key::KeyT => Some("KeyT".to_string()),
        rdev::Key::KeyU => Some("KeyU".to_string()),
        rdev::Key::KeyV => Some("KeyV".to_string()),
        rdev::Key::KeyW => Some("KeyW".to_string()),
        rdev::Key::KeyX => Some("KeyX".to_string()),
        rdev::Key::KeyY => Some("KeyY".to_string()),
        rdev::Key::KeyZ => Some("KeyZ".to_string()),
        rdev::Key::Num0 => Some("Digit0".to_string()),
        rdev::Key::Num1 => Some("Digit1".to_string()),
        rdev::Key::Num2 => Some("Digit2".to_string()),
        rdev::Key::Num3 => Some("Digit3".to_string()),
        rdev::Key::Num4 => Some("Digit4".to_string()),
        rdev::Key::Num5 => Some("Digit5".to_string()),
        rdev::Key::Num6 => Some("Digit6".to_string()),
        rdev::Key::Num7 => Some("Digit7".to_string()),
        rdev::Key::Num8 => Some("Digit8".to_string()),
        rdev::Key::Num9 => Some("Digit9".to_string()),
        rdev::Key::BackQuote => Some("Backquote".to_string()),
        rdev::Key::Minus => Some("Minus".to_string()),
        rdev::Key::Equal => Some("Equal".to_string()),
        rdev::Key::LeftBracket => Some("BracketLeft".to_string()),
        rdev::Key::RightBracket => Some("BracketRight".to_string()),
        rdev::Key::BackSlash => Some("Backslash".to_string()),
        rdev::Key::SemiColon => Some("Semicolon".to_string()),
        rdev::Key::Quote => Some("Quote".to_string()),
        rdev::Key::Comma => Some("Comma".to_string()),
        rdev::Key::Dot => Some("Period".to_string()),
        rdev::Key::Slash => Some("Slash".to_string()),
        rdev::Key::Return => Some("Enter".to_string()),
        rdev::Key::KpReturn => Some("NumpadEnter".to_string()),
        rdev::Key::KpMinus => Some("NumpadSubtract".to_string()),
        rdev::Key::KpPlus => Some("NumpadAdd".to_string()),
        rdev::Key::KpMultiply => Some("NumpadMultiply".to_string()),
        rdev::Key::KpDivide => Some("NumpadDivide".to_string()),
        rdev::Key::Kp0 => Some("Numpad0".to_string()),
        rdev::Key::Kp1 => Some("Numpad1".to_string()),
        rdev::Key::Kp2 => Some("Numpad2".to_string()),
        rdev::Key::Kp3 => Some("Numpad3".to_string()),
        rdev::Key::Kp4 => Some("Numpad4".to_string()),
        rdev::Key::Kp5 => Some("Numpad5".to_string()),
        rdev::Key::Kp6 => Some("Numpad6".to_string()),
        rdev::Key::Kp7 => Some("Numpad7".to_string()),
        rdev::Key::Kp8 => Some("Numpad8".to_string()),
        rdev::Key::Kp9 => Some("Numpad9".to_string()),
        rdev::Key::KpDelete => Some("NumpadDecimal".to_string()),
        rdev::Key::Tab => Some("Tab".to_string()),
        rdev::Key::Space => Some("Space".to_string()),
        rdev::Key::Escape => Some("Escape".to_string()),
        rdev::Key::Backspace => Some("Backspace".to_string()),
        rdev::Key::Home => Some("Home".to_string()),
        rdev::Key::End => Some("End".to_string()),
        rdev::Key::PageUp => Some("PageUp".to_string()),
        rdev::Key::PageDown => Some("PageDown".to_string()),
        rdev::Key::LeftArrow => Some("ArrowLeft".to_string()),
        rdev::Key::RightArrow => Some("ArrowRight".to_string()),
        rdev::Key::UpArrow => Some("ArrowUp".to_string()),
        rdev::Key::DownArrow => Some("ArrowDown".to_string()),
        rdev::Key::Insert => Some("Insert".to_string()),
        rdev::Key::Delete => Some("Delete".to_string()),
        rdev::Key::CapsLock => Some("CapsLock".to_string()),
        rdev::Key::NumLock => Some("NumLock".to_string()),
        rdev::Key::ScrollLock => Some("ScrollLock".to_string()),
        rdev::Key::PrintScreen => Some("PrintScreen".to_string()),
        rdev::Key::Pause => Some("Pause".to_string()),
        rdev::Key::F1 => Some("F1".to_string()),
        rdev::Key::F2 => Some("F2".to_string()),
        rdev::Key::F3 => Some("F3".to_string()),
        rdev::Key::F4 => Some("F4".to_string()),
        rdev::Key::F5 => Some("F5".to_string()),
        rdev::Key::F6 => Some("F6".to_string()),
        rdev::Key::F7 => Some("F7".to_string()),
        rdev::Key::F8 => Some("F8".to_string()),
        rdev::Key::F9 => Some("F9".to_string()),
        rdev::Key::F10 => Some("F10".to_string()),
        rdev::Key::F11 => Some("F11".to_string()),
        rdev::Key::F12 => Some("F12".to_string()),
        rdev::Key::ShiftLeft => Some("ShiftLeft".to_string()),
        rdev::Key::ShiftRight => Some("ShiftRight".to_string()),
        rdev::Key::ControlLeft => Some("ControlLeft".to_string()),
        rdev::Key::ControlRight => Some("ControlRight".to_string()),
        rdev::Key::Alt => Some("AltLeft".to_string()),
        rdev::Key::AltGr => Some("AltRight".to_string()),
        rdev::Key::MetaLeft => Some("MetaLeft".to_string()),
        rdev::Key::MetaRight => Some("MetaRight".to_string()),
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
        assert_eq!(
            normalize_key_code(rdev::Key::Alt),
            Some("AltLeft".to_string())
        );
        assert_eq!(
            normalize_key_code(rdev::Key::ControlRight),
            Some("ControlRight".to_string())
        );
        assert_eq!(normalize_button(rdev::Button::Middle), MouseButton::Middle);
    }
}
