use std::collections::HashSet;

use enigo::{Axis, Button, Direction, Enigo, Key, Keyboard, Mouse, Settings};
#[cfg(not(target_os = "macos"))]
use enigo::Coordinate;
use tracing::warn;

#[cfg(target_os = "macos")]
use core_graphics::display::CGDisplay;
#[cfg(target_os = "macos")]
use core_graphics::event::CGEvent;
#[cfg(target_os = "macos")]
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
#[cfg(target_os = "macos")]
use core_graphics::geometry::CGPoint;

use crate::event::{InputEvent, Modifiers, MouseButton};
use crate::inject::InputInjector;
use crate::keycode::{modifier_from_mask, parse_key_code, KeyCode, ModifierKind, NamedKey};
use crate::loopback::SharedLoopbackSuppressor;

pub struct NativeInputSink {
    platform: &'static str,
    enigo: Enigo,
    pressed_keys: HashSet<Key>,
    pressed_buttons: HashSet<Button>,
    current_modifiers: Modifiers,
    loopback: Option<SharedLoopbackSuppressor>,
    #[cfg(target_os = "macos")]
    cursor_position: Option<(f64, f64)>,
}

impl NativeInputSink {
    pub fn new(platform: &'static str) -> Result<Self, String> {
        Self::with_loopback(platform, None)
    }

    pub fn with_loopback(
        platform: &'static str,
        loopback: Option<SharedLoopbackSuppressor>,
    ) -> Result<Self, String> {
        let enigo = Enigo::new(&Settings::default()).map_err(|error| error.to_string())?;

        Ok(Self {
            platform,
            enigo,
            pressed_keys: HashSet::new(),
            pressed_buttons: HashSet::new(),
            current_modifiers: Modifiers::none(),
            loopback,
            #[cfg(target_os = "macos")]
            cursor_position: None,
        })
    }

    fn handle_input_event(&mut self, event: &InputEvent) -> Result<(), String> {
        self.record_loopback(event);
        match event {
            InputEvent::KeyDown { code, modifiers } => {
                let key_code = parse_key_code(code);
                self.sync_modifiers(modifiers, modifier_code_for(&key_code))?;
                self.key_action(key_code, Direction::Press)
            }
            InputEvent::KeyUp { code, modifiers } => {
                let key_code = parse_key_code(code);
                self.sync_modifiers(modifiers, modifier_code_for(&key_code))?;
                self.key_action(key_code, Direction::Release)
            }
            InputEvent::MouseMove { dx, dy, modifiers } => {
                self.sync_modifiers(modifiers, None)?;
                self.move_mouse(*dx, *dy)
            }
            InputEvent::MouseButtonDown { button, modifiers } => {
                self.sync_modifiers(modifiers, None)?;
                self.button_action(*button, Direction::Press)
            }
            InputEvent::MouseButtonUp { button, modifiers } => {
                self.sync_modifiers(modifiers, None)?;
                self.button_action(*button, Direction::Release)
            }
            InputEvent::MouseWheel {
                delta_x,
                delta_y,
                modifiers,
            } => {
                self.sync_modifiers(modifiers, None)?;
                if *delta_y != 0 {
                    self.enigo
                        .scroll(*delta_y, Axis::Vertical)
                        .map_err(|error| error.to_string())?;
                }
                if *delta_x != 0 {
                    self.enigo
                        .scroll(*delta_x, Axis::Horizontal)
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            }
        }
    }

    fn record_loopback(&mut self, event: &InputEvent) {
        if let Some(loopback) = &self.loopback {
            let mut loopback = loopback
                .lock()
                .expect("loopback suppressor mutex should not be poisoned");
            loopback.record(event.clone());
        }
    }

    fn sync_modifiers(
        &mut self,
        desired: &Modifiers,
        exclude: Option<ModifierKind>,
    ) -> Result<(), String> {
        for kind in [
            ModifierKind::Shift,
            ModifierKind::Control,
            ModifierKind::Alt,
            ModifierKind::Meta,
        ] {
            if Some(kind) == exclude {
                continue;
            }

            let desired_state = modifier_from_mask(desired, kind);
            let current_state = modifier_from_mask(&self.current_modifiers, kind);

            if desired_state != current_state {
                self.set_modifier(kind, desired_state)?;
            }
        }

        Ok(())
    }

    fn set_modifier(&mut self, kind: ModifierKind, pressed: bool) -> Result<(), String> {
        let key = modifier_key(kind);
        let direction = if pressed {
            Direction::Press
        } else {
            Direction::Release
        };

        self.enigo
            .key(key, direction)
            .map_err(|error| error.to_string())?;

        match kind {
            ModifierKind::Shift => self.current_modifiers.shift = pressed,
            ModifierKind::Control => self.current_modifiers.control = pressed,
            ModifierKind::Alt => self.current_modifiers.alt = pressed,
            ModifierKind::Meta => self.current_modifiers.meta = pressed,
        }

        Ok(())
    }

    fn move_mouse(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            return self.move_mouse_macos(dx, dy);
        }

        #[cfg(not(target_os = "macos"))]
        {
            self.enigo
                .move_mouse(dx, dy, Coordinate::Rel)
                .map_err(|error| error.to_string())
        }
    }

    #[cfg(target_os = "macos")]
    fn move_mouse_macos(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        let current = match self.cursor_position {
            Some(pos) => pos,
            None => {
                // First move: read the actual cursor position from the system.
                // CGEventSourceStateID::HIDSystemState is fine here because no
                // programmatic warps have occurred yet (or cursor_position was
                // reset after the last control session).
                let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                    .map_err(|_| "failed to create macOS event source".to_string())?;
                let event = CGEvent::new(source)
                    .map_err(|_| "failed to read macOS mouse location".to_string())?;
                let loc = event.location();
                (loc.x, loc.y)
            }
        };
        let target = (current.0 + f64::from(dx), current.1 + f64::from(dy));
        CGDisplay::warp_mouse_cursor_position(CGPoint::new(target.0, target.1))
            .map_err(|error| format!("{error:?}"))?;
        self.cursor_position = Some(target);
        Ok(())
    }

    fn key_action(&mut self, key_code: KeyCode, direction: Direction) -> Result<(), String> {
        match key_code {
            KeyCode::Modifier(kind) => {
                self.set_modifier(kind, matches!(direction, Direction::Press))
            }
            KeyCode::Character(ch) => self.apply_key(Key::Unicode(ch), direction),
            KeyCode::Named(named) => {
                if let Some(key) = named_key(named) {
                    self.apply_key(key, direction)
                } else {
                    warn!(platform = self.platform, key = ?named, "unsupported named key");
                    Ok(())
                }
            }
            KeyCode::Unmapped(code) => {
                warn!(platform = self.platform, code = %code, "unmapped key code");
                Ok(())
            }
        }
    }

    fn apply_key(&mut self, key: Key, direction: Direction) -> Result<(), String> {
        self.enigo
            .key(key, direction)
            .map_err(|error| error.to_string())?;

        match direction {
            Direction::Press => {
                self.pressed_keys.insert(key);
            }
            Direction::Release => {
                self.pressed_keys.remove(&key);
            }
            Direction::Click => {
                self.pressed_keys.remove(&key);
            }
        }

        Ok(())
    }

    fn button_action(&mut self, button: MouseButton, direction: Direction) -> Result<(), String> {
        let button = match button {
            MouseButton::Left => Button::Left,
            MouseButton::Right => Button::Right,
            MouseButton::Middle => Button::Middle,
        };

        self.enigo
            .button(button, direction)
            .map_err(|error| error.to_string())?;

        match direction {
            Direction::Press => {
                self.pressed_buttons.insert(button);
            }
            Direction::Release => {
                self.pressed_buttons.remove(&button);
            }
            Direction::Click => {
                self.pressed_buttons.remove(&button);
            }
        }

        Ok(())
    }

    fn release_all_pressed(&mut self) -> Result<(), String> {
        let keys: Vec<Key> = self.pressed_keys.iter().copied().collect();
        let buttons: Vec<Button> = self.pressed_buttons.iter().copied().collect();
        let modifiers = [
            (ModifierKind::Shift, self.current_modifiers.shift),
            (ModifierKind::Control, self.current_modifiers.control),
            (ModifierKind::Alt, self.current_modifiers.alt),
            (ModifierKind::Meta, self.current_modifiers.meta),
        ];

        for key in keys {
            if let Some(event) = key_release_event(key, self.current_modifiers) {
                self.record_loopback(&event);
            }
            self.enigo
                .key(key, Direction::Release)
                .map_err(|error| error.to_string())?;
        }

        for button in buttons {
            if let Some(button) = button_name(button) {
                self.record_loopback(&InputEvent::MouseButtonUp {
                    button,
                    modifiers: self.current_modifiers,
                });
            }
            self.enigo
                .button(button, Direction::Release)
                .map_err(|error| error.to_string())?;
        }

        for (kind, pressed) in modifiers.into_iter().rev() {
            if pressed {
                if let Some(event) = key_release_event(modifier_key(kind), self.current_modifiers) {
                    self.record_loopback(&event);
                }
                self.set_modifier(kind, false)?;
            }
        }

        self.pressed_keys.clear();
        self.pressed_buttons.clear();
        self.current_modifiers = Modifiers::none();
        #[cfg(target_os = "macos")]
        {
            self.cursor_position = None;
        }
        Ok(())
    }
}

impl InputInjector for NativeInputSink {
    fn inject(&mut self, event: &InputEvent) -> Result<(), String> {
        self.handle_input_event(event)
    }

    fn release_all(&mut self) -> Result<(), String> {
        self.release_all_pressed()
    }
}

impl crate::InputEventSink for NativeInputSink {
    fn handle(&mut self, event: &InputEvent) -> Result<(), String> {
        self.handle_input_event(event)
    }

    fn release_all(&mut self) -> Result<(), String> {
        self.release_all_pressed()
    }
}

fn modifier_key(kind: ModifierKind) -> Key {
    match kind {
        ModifierKind::Shift => Key::Shift,
        ModifierKind::Control => Key::Control,
        ModifierKind::Alt => Key::Alt,
        ModifierKind::Meta => Key::Meta,
    }
}

fn key_code_name(key: Key) -> String {
    match key {
        Key::Backspace => "Backspace".to_string(),
        Key::Tab => "Tab".to_string(),
        Key::Return => "Enter".to_string(),
        Key::Escape => "Escape".to_string(),
        Key::Space => "Space".to_string(),
        Key::UpArrow => "ArrowUp".to_string(),
        Key::DownArrow => "ArrowDown".to_string(),
        Key::LeftArrow => "ArrowLeft".to_string(),
        Key::RightArrow => "ArrowRight".to_string(),
        Key::Home => "Home".to_string(),
        Key::End => "End".to_string(),
        Key::PageUp => "PageUp".to_string(),
        Key::PageDown => "PageDown".to_string(),
        Key::Shift => "ShiftLeft".to_string(),
        Key::Control => "ControlLeft".to_string(),
        Key::Alt => "AltLeft".to_string(),
        Key::Meta => "MetaLeft".to_string(),
        Key::Unicode(ch) => format!("Key{}", ch.to_ascii_uppercase()),
        other => format!("{other:?}"),
    }
}

fn key_release_event(key: Key, modifiers: Modifiers) -> Option<InputEvent> {
    let code = key_code_name(key);
    let modifiers = match key {
        Key::Shift => Modifiers {
            shift: false,
            ..modifiers
        },
        Key::Control => Modifiers {
            control: false,
            ..modifiers
        },
        Key::Alt => Modifiers {
            alt: false,
            ..modifiers
        },
        Key::Meta => Modifiers {
            meta: false,
            ..modifiers
        },
        _ => modifiers,
    };

    Some(InputEvent::KeyUp { code, modifiers })
}

fn button_name(button: Button) -> Option<MouseButton> {
    match button {
        Button::Left => Some(MouseButton::Left),
        Button::Right => Some(MouseButton::Right),
        Button::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

fn named_key(named: NamedKey) -> Option<Key> {
    #[cfg(target_os = "macos")]
    {
        Some(match named {
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Tab => Key::Tab,
            NamedKey::Enter => Key::Return,
            NamedKey::Escape => Key::Escape,
            NamedKey::Space => Key::Space,
            NamedKey::LeftArrow => Key::LeftArrow,
            NamedKey::RightArrow => Key::RightArrow,
            NamedKey::UpArrow => Key::UpArrow,
            NamedKey::DownArrow => Key::DownArrow,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            NamedKey::Insert => return None,
            NamedKey::Delete => Key::Delete,
            NamedKey::CapsLock => Key::CapsLock,
            NamedKey::F(index) => match index {
                1 => Key::F1,
                2 => Key::F2,
                3 => Key::F3,
                4 => Key::F4,
                5 => Key::F5,
                6 => Key::F6,
                7 => Key::F7,
                8 => Key::F8,
                9 => Key::F9,
                10 => Key::F10,
                11 => Key::F11,
                12 => Key::F12,
                13 => Key::F13,
                14 => Key::F14,
                15 => Key::F15,
                16 => Key::F16,
                17 => Key::F17,
                18 => Key::F18,
                19 => Key::F19,
                20 => Key::F20,
                _ => return None,
            },
            NamedKey::NumLock | NamedKey::ScrollLock | NamedKey::PrintScreen | NamedKey::Pause => {
                return None;
            }
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Some(match named {
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Tab => Key::Tab,
            NamedKey::Enter => Key::Return,
            NamedKey::Escape => Key::Escape,
            NamedKey::Space => Key::Space,
            NamedKey::LeftArrow => Key::LeftArrow,
            NamedKey::RightArrow => Key::RightArrow,
            NamedKey::UpArrow => Key::UpArrow,
            NamedKey::DownArrow => Key::DownArrow,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            NamedKey::Insert => return None,
            NamedKey::Delete => Key::Delete,
            NamedKey::CapsLock => Key::CapsLock,
            NamedKey::NumLock => Key::Numlock,
            #[cfg(target_os = "windows")]
            NamedKey::ScrollLock => Key::Scroll,
            #[cfg(not(target_os = "windows"))]
            NamedKey::ScrollLock => Key::ScrollLock,
            NamedKey::PrintScreen => Key::PrintScr,
            NamedKey::Pause => Key::Pause,
            NamedKey::F(index) => match index {
                1 => Key::F1,
                2 => Key::F2,
                3 => Key::F3,
                4 => Key::F4,
                5 => Key::F5,
                6 => Key::F6,
                7 => Key::F7,
                8 => Key::F8,
                9 => Key::F9,
                10 => Key::F10,
                11 => Key::F11,
                12 => Key::F12,
                13 => Key::F13,
                14 => Key::F14,
                15 => Key::F15,
                16 => Key::F16,
                17 => Key::F17,
                18 => Key::F18,
                19 => Key::F19,
                20 => Key::F20,
                _ => return None,
            },
        })
    }
}

fn modifier_code_for(key_code: &KeyCode) -> Option<ModifierKind> {
    match key_code {
        KeyCode::Modifier(kind) => Some(*kind),
        _ => None,
    }
}
