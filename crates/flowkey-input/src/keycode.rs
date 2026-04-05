use crate::event::Modifiers;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModifierKind {
    Shift,
    Control,
    Alt,
    Meta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Backspace,
    Tab,
    Enter,
    Escape,
    Space,
    LeftArrow,
    RightArrow,
    UpArrow,
    DownArrow,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    CapsLock,
    NumLock,
    ScrollLock,
    PrintScreen,
    Pause,
    F(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCode {
    Character(char),
    Modifier(ModifierKind),
    Named(NamedKey),
    Unmapped(String),
}

pub fn parse_key_code(code: &str) -> KeyCode {
    match code {
        "ShiftLeft" | "ShiftRight" => KeyCode::Modifier(ModifierKind::Shift),
        "ControlLeft" | "ControlRight" | "CtrlLeft" | "CtrlRight" => {
            KeyCode::Modifier(ModifierKind::Control)
        }
        "AltLeft" | "AltRight" | "OptionLeft" | "OptionRight" => {
            KeyCode::Modifier(ModifierKind::Alt)
        }
        "MetaLeft" | "MetaRight" | "OSLeft" | "OSRight" | "CommandLeft" | "CommandRight" => {
            KeyCode::Modifier(ModifierKind::Meta)
        }
        "Backspace" => KeyCode::Named(NamedKey::Backspace),
        "Tab" => KeyCode::Named(NamedKey::Tab),
        "Enter" => KeyCode::Named(NamedKey::Enter),
        "Escape" | "Esc" => KeyCode::Named(NamedKey::Escape),
        "Space" | "Spacebar" => KeyCode::Named(NamedKey::Space),
        "ArrowLeft" => KeyCode::Named(NamedKey::LeftArrow),
        "ArrowRight" => KeyCode::Named(NamedKey::RightArrow),
        "ArrowUp" => KeyCode::Named(NamedKey::UpArrow),
        "ArrowDown" => KeyCode::Named(NamedKey::DownArrow),
        "Home" => KeyCode::Named(NamedKey::Home),
        "End" => KeyCode::Named(NamedKey::End),
        "PageUp" => KeyCode::Named(NamedKey::PageUp),
        "PageDown" => KeyCode::Named(NamedKey::PageDown),
        "Insert" => KeyCode::Named(NamedKey::Insert),
        "Delete" | "Del" => KeyCode::Named(NamedKey::Delete),
        "CapsLock" => KeyCode::Named(NamedKey::CapsLock),
        "NumLock" => KeyCode::Named(NamedKey::NumLock),
        "ScrollLock" => KeyCode::Named(NamedKey::ScrollLock),
        "PrintScreen" => KeyCode::Named(NamedKey::PrintScreen),
        "Pause" | "Break" => KeyCode::Named(NamedKey::Pause),
        code if code.starts_with('F') => {
            if let Ok(index) = code[1..].parse::<u8>() {
                if (1..=24).contains(&index) {
                    return KeyCode::Named(NamedKey::F(index));
                }
            }

            code_to_character(code)
                .map(KeyCode::Character)
                .unwrap_or_else(|| KeyCode::Unmapped(code.to_string()))
        }
        "Minus" => KeyCode::Character('-'),
        "Equal" => KeyCode::Character('='),
        "BracketLeft" => KeyCode::Character('['),
        "BracketRight" => KeyCode::Character(']'),
        "Backslash" => KeyCode::Character('\\'),
        "Semicolon" => KeyCode::Character(';'),
        "Quote" => KeyCode::Character('\''),
        "Backquote" => KeyCode::Character('`'),
        "Comma" => KeyCode::Character(','),
        "Period" => KeyCode::Character('.'),
        "Slash" => KeyCode::Character('/'),
        "IntlBackslash" => KeyCode::Character('\\'),
        "Numpad0" => KeyCode::Character('0'),
        "Numpad1" => KeyCode::Character('1'),
        "Numpad2" => KeyCode::Character('2'),
        "Numpad3" => KeyCode::Character('3'),
        "Numpad4" => KeyCode::Character('4'),
        "Numpad5" => KeyCode::Character('5'),
        "Numpad6" => KeyCode::Character('6'),
        "Numpad7" => KeyCode::Character('7'),
        "Numpad8" => KeyCode::Character('8'),
        "Numpad9" => KeyCode::Character('9'),
        "NumpadEnter" => KeyCode::Named(NamedKey::Enter),
        "NumpadAdd" => KeyCode::Character('+'),
        "NumpadSubtract" => KeyCode::Character('-'),
        "NumpadMultiply" => KeyCode::Character('*'),
        "NumpadDivide" => KeyCode::Character('/'),
        "NumpadDecimal" => KeyCode::Character('.'),
        "NumpadEqual" => KeyCode::Character('='),
        "NumpadComma" => KeyCode::Character(','),
        other => code_to_character(other)
            .map(KeyCode::Character)
            .unwrap_or_else(|| KeyCode::Unmapped(other.to_string())),
    }
}

pub fn modifier_from_mask(modifiers: &Modifiers, kind: ModifierKind) -> bool {
    match kind {
        ModifierKind::Shift => modifiers.shift,
        ModifierKind::Control => modifiers.control,
        ModifierKind::Alt => modifiers.alt,
        ModifierKind::Meta => modifiers.meta,
    }
}

fn code_to_character(code: &str) -> Option<char> {
    match code {
        "KeyA" | "A" => Some('a'),
        "KeyB" | "B" => Some('b'),
        "KeyC" | "C" => Some('c'),
        "KeyD" | "D" => Some('d'),
        "KeyE" | "E" => Some('e'),
        "KeyF" | "F" => Some('f'),
        "KeyG" | "G" => Some('g'),
        "KeyH" | "H" => Some('h'),
        "KeyI" | "I" => Some('i'),
        "KeyJ" | "J" => Some('j'),
        "KeyK" | "K" => Some('k'),
        "KeyL" | "L" => Some('l'),
        "KeyM" | "M" => Some('m'),
        "KeyN" | "N" => Some('n'),
        "KeyO" | "O" => Some('o'),
        "KeyP" | "P" => Some('p'),
        "KeyQ" | "Q" => Some('q'),
        "KeyR" | "R" => Some('r'),
        "KeyS" | "S" => Some('s'),
        "KeyT" | "T" => Some('t'),
        "KeyU" | "U" => Some('u'),
        "KeyV" | "V" => Some('v'),
        "KeyW" | "W" => Some('w'),
        "KeyX" | "X" => Some('x'),
        "KeyY" | "Y" => Some('y'),
        "KeyZ" | "Z" => Some('z'),
        "Digit0" | "0" => Some('0'),
        "Digit1" | "1" => Some('1'),
        "Digit2" | "2" => Some('2'),
        "Digit3" | "3" => Some('3'),
        "Digit4" | "4" => Some('4'),
        "Digit5" | "5" => Some('5'),
        "Digit6" | "6" => Some('6'),
        "Digit7" | "7" => Some('7'),
        "Digit8" | "8" => Some('8'),
        "Digit9" | "9" => Some('9'),
        code if code.len() == 1 => code.chars().next(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{modifier_from_mask, parse_key_code, KeyCode, ModifierKind};
    use crate::event::Modifiers;

    #[test]
    fn parses_letter_key_codes() {
        assert_eq!(parse_key_code("KeyK"), KeyCode::Character('k'));
        assert_eq!(parse_key_code("Digit3"), KeyCode::Character('3'));
    }

    #[test]
    fn parses_modifier_key_codes() {
        assert_eq!(
            parse_key_code("ShiftLeft"),
            KeyCode::Modifier(ModifierKind::Shift)
        );
        assert_eq!(
            parse_key_code("ControlRight"),
            KeyCode::Modifier(ModifierKind::Control)
        );
    }

    #[test]
    fn modifier_mask_looks_up_expected_bits() {
        let modifiers = Modifiers {
            shift: true,
            control: false,
            alt: true,
            meta: false,
        };

        assert!(modifier_from_mask(&modifiers, ModifierKind::Shift));
        assert!(!modifier_from_mask(&modifiers, ModifierKind::Control));
        assert!(modifier_from_mask(&modifiers, ModifierKind::Alt));
        assert!(!modifier_from_mask(&modifiers, ModifierKind::Meta));
    }
}
