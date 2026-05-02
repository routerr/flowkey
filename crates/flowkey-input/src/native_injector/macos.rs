use super::{
    CoordinateBounds, DockCursorZone, InputEvent, NativeInputSink, SharedLoopbackSuppressor,
};
use crate::event::Modifiers;
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use enigo::{Direction, Keyboard, Mouse};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

extern "C" {
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
}


pub(super) fn move_mouse(sink: &mut NativeInputSink, dx: i32, dy: i32) -> Result<(), String> {
    let current = match sink.cursor_position {
        Some(pos) => pos,
        None => {
            let (x, y) = sink.enigo.location().map_err(|error| error.to_string())?;
            (f64::from(x), f64::from(y))
        }
    };
    let raw_target = (current.0 + f64::from(dx), current.1 + f64::from(dy));
    let bounds = cached_desktop_bounds(sink);
    let target = bounds
        .map(|bounds| clamp_point(raw_target, bounds))
        .unwrap_or(raw_target);
    let posted_dx = macos_posted_delta(dx, round_delta(target.0 - current.0));
    let posted_dy = macos_posted_delta(dy, round_delta(target.1 - current.1));
    let dest = CGPoint::new(target.0, target.1);
    let clamped = target != raw_target;

    // Use CGWarpMouseCursorPosition to move the cursor, then immediately
    // call CGAssociateMouseAndMouseCursorPosition(true) to reset the warp
    // suppression timer. This is the proven approach used by Barrier/Synergy
    // — it reliably positions the cursor without the ~250ms delta freeze.
    CGDisplay::warp_mouse_cursor_position(dest).map_err(|error| format!("{error:?}"))?;
    unsafe {
        CGAssociateMouseAndMouseCursorPosition(true);
    }

    // Post a CGEvent so applications see the mouse-move event stream.
    if sink.pressed_buttons.is_empty() {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "failed to create macOS event source for move".to_string())?;
        let move_event =
            CGEvent::new_mouse_event(source, CGEventType::MouseMoved, dest, CGMouseButton::Left)
                .map_err(|_| "failed to create macOS mouse-move event".to_string())?;
        move_event.set_integer_value_field(
            core_graphics::event::EventField::MOUSE_EVENT_DELTA_X,
            i64::from(posted_dx),
        );
        move_event.set_integer_value_field(
            core_graphics::event::EventField::MOUSE_EVENT_DELTA_Y,
            i64::from(posted_dy),
        );
        move_event.post(CGEventTapLocation::HID);
    } else {
        let (event_type, cg_button) = if sink.pressed_buttons.contains(&enigo::Button::Left) {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if sink.pressed_buttons.contains(&enigo::Button::Right) {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        };
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "failed to create macOS event source for drag".to_string())?;
        let event = CGEvent::new_mouse_event(source, event_type, dest, cg_button)
            .map_err(|_| "failed to create macOS drag event".to_string())?;
        event.set_integer_value_field(
            core_graphics::event::EventField::MOUSE_EVENT_DELTA_X,
            i64::from(posted_dx),
        );
        event.set_integer_value_field(
            core_graphics::event::EventField::MOUSE_EVENT_DELTA_Y,
            i64::from(posted_dy),
        );
        event.post(CGEventTapLocation::HID);
    }

    if clamped || posted_dx != dx || posted_dy != dy {
        debug!(
            platform = sink.platform,
            current_x = current.0,
            current_y = current.1,
            raw_target_x = raw_target.0,
            raw_target_y = raw_target.1,
            target_x = target.0,
            target_y = target.1,
            requested_dx = dx,
            requested_dy = dy,
            posted_dx,
            posted_dy,
            buttons_pressed = sink.pressed_buttons.len(),
            "macOS mouse move reached edge-sensitive injection path"
        );
    }

    if let Some(bounds) = bounds {
        sink.update_macos_dock_proxy(target, bounds)?;
    }

    sink.cursor_position = Some(target);
    Ok(())
}

pub(super) fn post_mouse_button(
    sink: &mut NativeInputSink,
    button: enigo::Button,
    direction: Direction,
) -> Result<(), String> {
    // Resolve the current cursor position from our tracked state, falling
    // back to querying the OS only when we have never seen a move event.
    // This avoids calling NSEvent::mouseLocation() (which enigo does
    // internally) — that API returns a stale/frozen position when the
    // cursor has been decoupled via CGAssociateMouseAndMouseCursorPosition.
    let (x, y) = match sink.cursor_position {
        Some(pos) => pos,
        None => {
            let (sx, sy) = sink.enigo.location().map_err(|e| e.to_string())?;
            let pos = (f64::from(sx), f64::from(sy));
            sink.cursor_position = Some(pos);
            pos
        }
    };
    let dest = CGPoint::new(x, y);

    let (event_type, cg_button) = match (button, &direction) {
        (enigo::Button::Left, Direction::Press) => {
            (CGEventType::LeftMouseDown, CGMouseButton::Left)
        }
        (enigo::Button::Left, Direction::Release) => {
            (CGEventType::LeftMouseUp, CGMouseButton::Left)
        }
        (enigo::Button::Right, Direction::Press) => {
            (CGEventType::RightMouseDown, CGMouseButton::Right)
        }
        (enigo::Button::Right, Direction::Release) => {
            (CGEventType::RightMouseUp, CGMouseButton::Right)
        }
        (_, Direction::Press) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
        (_, Direction::Release) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        // Click is Press+Release; handle each half separately.
        (enigo::Button::Left, Direction::Click) => {
            post_mouse_button(sink, button, Direction::Press)?;
            return post_mouse_button(sink, button, Direction::Release);
        }
        (enigo::Button::Right, Direction::Click) => {
            post_mouse_button(sink, button, Direction::Press)?;
            return post_mouse_button(sink, button, Direction::Release);
        }
        (_, Direction::Click) => {
            post_mouse_button(sink, button, Direction::Press)?;
            return post_mouse_button(sink, button, Direction::Release);
        }
    };

    // Use HIDSystemState source + HID level, consistent with mouse-move
    // and keyboard injection. This ensures the event enters the full input
    // pipeline and is handled by all macOS subsystems. The loopback
    // suppressor prevents the HID-level tap from re-capturing it.
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "failed to create macOS event source for button event".to_string())?;
    let event = CGEvent::new_mouse_event(source, event_type, dest, cg_button)
        .map_err(|_| "failed to create macOS mouse-button event".to_string())?;
    event.post(CGEventTapLocation::HID);
    Ok(())
}

/// Post a keyboard event directly via CGEvent, bypassing enigo.
/// This mirrors the approach used for mouse buttons — enigo's keyboard
/// implementation can fail silently on macOS when an active CGEventTap
/// is running on the same process (the tap intercepts and re-routes
/// events before they reach apps). Direct CGEvent posting at HID level
/// is more reliable.
///
/// Modifier keys (Shift, Control, Alt, Meta) are posted as FlagsChanged
/// events so that the CGEventTap's modifier state tracking stays accurate,
/// ensuring loopback suppressor comparisons match correctly.
pub(super) fn post_key_event(
    sink: &mut NativeInputSink,
    code: &str,
    key_down: bool,
) -> Result<(), String> {
    let Some(keycode) = key_code_to_macos_virtual(code) else {
        warn!(
            target: "keyboard_trace",
            platform = sink.platform,
            code = %code,
            pressed = key_down,
            "macOS keyboard injection fell back to enigo/unicode path"
        );
        // Fall back to enigo for unmapped keys (Unicode characters, etc.)
        let key = match code.len() {
            1 => enigo::Key::Unicode(code.chars().next().unwrap()),
            _ => return Err(format!("unmapped macOS keycode for: {code}")),
        };
        let direction = if key_down {
            enigo::Direction::Press
        } else {
            enigo::Direction::Release
        };
        return sink
            .enigo
            .key(key, direction)
            .map_err(|error| error.to_string());
    };

    // For modifier keys, build the flags that reflect the new state after
    // this key event, then post a FlagsChanged CGEvent. This matches how
    // macOS generates real modifier key events and keeps the CGEventTap's
    // modifier tracking in sync with the injected state.
    if let Some(modifier_flag) = modifier_flag_for_keycode(keycode) {
        let mut flags = build_modifier_flags(&sink.current_modifiers);
        if key_down {
            flags |= modifier_flag;
        } else {
            flags &= !modifier_flag;
        }
        // FlagsChanged events use HIDSystemState, same as regular keys.
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "failed to create macOS event source for modifier event".to_string())?;
        let event = CGEvent::new_keyboard_event(source, keycode, key_down)
            .map_err(|_| format!("failed to create macOS modifier event for {code}"))?;
        event.set_flags(flags);
        debug!(
            target: "keyboard_trace",
            platform = sink.platform,
            code = %code,
            macos_keycode = keycode,
            pressed = key_down,
            "posting macOS modifier FlagsChanged CGEvent"
        );
        event.post(CGEventTapLocation::HID);
        return Ok(());
    }

    let flags = build_modifier_flags(&sink.current_modifiers);
    // Use HIDSystemState as the event source. This is the same source used
    // by real hardware events and is accepted reliably by all macOS subsystems.
    // The HID-level event tap will see the injected event, but the loopback
    // suppressor filters it out (via matches_ignoring_timestamp). Even if
    // loopback matching fails, the tap passes the event through because
    // suppress_active is false when this machine is being controlled.
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "failed to create macOS event source for key event".to_string())?;
    let event = CGEvent::new_keyboard_event(source, keycode, key_down)
        .map_err(|_| format!("failed to create macOS keyboard event for {code}"))?;
    event.set_flags(flags);
    debug!(
        target: "keyboard_trace",
        platform = sink.platform,
        code = %code,
        macos_keycode = keycode,
        pressed = key_down,
        shift = sink.current_modifiers.shift,
        control = sink.current_modifiers.control,
        alt = sink.current_modifiers.alt,
        meta = sink.current_modifiers.meta,
        "posting macOS keyboard CGEvent"
    );
    event.post(CGEventTapLocation::HID);
    Ok(())
}

/// Returns the CGEventFlags bit for modifier keys, or None for regular keys.
fn modifier_flag_for_keycode(keycode: CGKeyCode) -> Option<CGEventFlags> {
    match keycode {
        0x38 | 0x3C => Some(CGEventFlags::CGEventFlagShift),    // ShiftLeft / ShiftRight
        0x3B | 0x3E => Some(CGEventFlags::CGEventFlagControl),  // ControlLeft / ControlRight
        0x3A | 0x3D => Some(CGEventFlags::CGEventFlagAlternate),// AltLeft / AltRight
        0x37 | 0x36 => Some(CGEventFlags::CGEventFlagCommand),  // MetaLeft / MetaRight
        _ => None,
    }
}

fn build_modifier_flags(modifiers: &super::super::event::Modifiers) -> CGEventFlags {
    let mut flags = CGEventFlags::CGEventFlagNonCoalesced;
    if modifiers.shift {
        flags |= CGEventFlags::CGEventFlagShift;
    }
    if modifiers.control {
        flags |= CGEventFlags::CGEventFlagControl;
    }
    if modifiers.alt {
        flags |= CGEventFlags::CGEventFlagAlternate;
    }
    if modifiers.meta {
        flags |= CGEventFlags::CGEventFlagCommand;
    }
    flags
}

/// Maps our protocol key code strings to macOS virtual keycodes.
fn key_code_to_macos_virtual(code: &str) -> Option<CGKeyCode> {
    Some(match code {
        "KeyA" => 0x00,
        "KeyS" => 0x01,
        "KeyD" => 0x02,
        "KeyF" => 0x03,
        "KeyH" => 0x04,
        "KeyG" => 0x05,
        "KeyZ" => 0x06,
        "KeyX" => 0x07,
        "KeyC" => 0x08,
        "KeyV" => 0x09,
        "KeyB" => 0x0B,
        "KeyQ" => 0x0C,
        "KeyW" => 0x0D,
        "KeyE" => 0x0E,
        "KeyR" => 0x0F,
        "KeyY" => 0x10,
        "KeyT" => 0x11,
        "Digit1" => 0x12,
        "Digit2" => 0x13,
        "Digit3" => 0x14,
        "Digit4" => 0x15,
        "Digit6" => 0x16,
        "Digit5" => 0x17,
        "Equal" => 0x18,
        "Digit9" => 0x19,
        "Digit7" => 0x1A,
        "Minus" => 0x1B,
        "Digit8" => 0x1C,
        "Digit0" => 0x1D,
        "BracketRight" => 0x1E,
        "KeyO" => 0x1F,
        "KeyU" => 0x20,
        "BracketLeft" => 0x21,
        "KeyI" => 0x22,
        "KeyP" => 0x23,
        "Enter" => 0x24,
        "KeyL" => 0x25,
        "KeyJ" => 0x26,
        "Quote" => 0x27,
        "KeyK" => 0x28,
        "Semicolon" => 0x29,
        "Backslash" => 0x2A,
        "Comma" => 0x2B,
        "Slash" => 0x2C,
        "KeyN" => 0x2D,
        "KeyM" => 0x2E,
        "Period" => 0x2F,
        "Tab" => 0x30,
        "Space" => 0x31,
        "Backquote" => 0x32,
        "Backspace" => 0x33,
        "Escape" => 0x35,
        "MetaRight" => 0x36,
        "MetaLeft" => 0x37,
        "ShiftLeft" => 0x38,
        "CapsLock" => 0x39,
        "AltLeft" => 0x3A,
        "ControlLeft" => 0x3B,
        "ShiftRight" => 0x3C,
        "AltRight" => 0x3D,
        "ControlRight" => 0x3E,
        "F17" => 0x40,
        "NumpadDecimal" => 0x41,
        "NumpadMultiply" => 0x43,
        "NumpadAdd" => 0x45,
        "NumLock" => 0x47,
        "NumpadDivide" => 0x4B,
        "NumpadEnter" => 0x4C,
        "NumpadSubtract" => 0x4E,
        "F18" => 0x4F,
        "F19" => 0x50,
        "NumpadEqual" => 0x51,
        "Numpad0" => 0x52,
        "Numpad1" => 0x53,
        "Numpad2" => 0x54,
        "Numpad3" => 0x55,
        "Numpad4" => 0x56,
        "Numpad5" => 0x57,
        "Numpad6" => 0x58,
        "Numpad7" => 0x59,
        "Numpad8" => 0x5B,
        "Numpad9" => 0x5C,
        "F5" => 0x60,
        "F6" => 0x61,
        "F7" => 0x62,
        "F3" => 0x63,
        "F8" => 0x64,
        "F9" => 0x65,
        "F11" => 0x67,
        "F13" => 0x69,
        "F16" => 0x6A,
        "F14" => 0x6B,
        "F10" => 0x6D,
        "F12" => 0x6F,
        "F15" => 0x71,
        "Home" => 0x73,
        "PageUp" => 0x74,
        "Delete" => 0x75,
        "F4" => 0x76,
        "End" => 0x77,
        "F2" => 0x78,
        "PageDown" => 0x79,
        "F1" => 0x7A,
        "ArrowLeft" => 0x7B,
        "ArrowRight" => 0x7C,
        "ArrowDown" => 0x7D,
        "ArrowUp" => 0x7E,
        _ => return None,
    })
}

pub(super) fn reset_state(sink: &mut NativeInputSink) {
    sink.cursor_position = None;
    sink.last_dock_zone = DockCursorZone::Interior;
    sink.dock_hide_allowed_at = None;
    sink.dock_visible = false;
    sink.cached_bounds = None;
}

/// Return cached desktop bounds, re-querying the system at most once per second.
fn cached_desktop_bounds(sink: &mut NativeInputSink) -> Option<CoordinateBounds> {
    const BOUNDS_TTL: Duration = Duration::from_secs(1);
    let now = Instant::now();
    if let Some((cached_at, bounds)) = sink.cached_bounds {
        if now.duration_since(cached_at) < BOUNDS_TTL {
            return Some(bounds);
        }
    }
    let bounds = macos_visible_desktop_bounds()?;
    sink.cached_bounds = Some((now, bounds));
    Some(bounds)
}

impl NativeInputSink {
    fn update_macos_dock_proxy(
        &mut self,
        target: (f64, f64),
        bounds: CoordinateBounds,
    ) -> Result<(), String> {
        if !self.pressed_buttons.is_empty() {
            return Ok(());
        }

        let screen_height = bounds.max_y - bounds.min_y;
        if screen_height <= 0.0 {
            return Ok(());
        }

        let distance_from_bottom = bounds.max_y - target.1;
        let reveal_threshold = 1.0;
        let hide_threshold = (screen_height * 0.10).max(24.0);
        let zone = dock_cursor_zone(distance_from_bottom, reveal_threshold, hide_threshold);
        let action = dock_proxy_transition(self.last_dock_zone, zone);
        self.last_dock_zone = zone;

        match action {
            Some(DockProxyAction::Show) => {
                if !self.dock_visible && self.trigger_macos_dock_show()? {
                    self.dock_visible = true;
                    self.dock_hide_allowed_at = Some(Instant::now() + Duration::from_millis(450));
                    debug!(
                        platform = self.platform,
                        cursor_y = target.1,
                        distance_from_bottom,
                        zone = ?zone,
                        hide_threshold,
                        "revealed macOS Dock via edge-entry proxy state machine"
                    );
                }
            }
            Some(DockProxyAction::Hide) => {
                if self.dock_visible
                    && self.dock_hide_is_allowed()
                    && self.trigger_macos_dock_hide()?
                {
                    self.dock_visible = false;
                    debug!(
                        platform = self.platform,
                        cursor_y = target.1,
                        distance_from_bottom,
                        zone = ?zone,
                        hide_threshold,
                        "hid macOS Dock via upward-exit proxy state machine"
                    );
                }
            }
            None => {}
        }

        Ok(())
    }

    fn trigger_macos_dock_show(&mut self) -> Result<bool, String> {
        let now = Instant::now();
        if let Some(last_action_at) = self.last_dock_action_at {
            if now.duration_since(last_action_at) < Duration::from_millis(350) {
                return Ok(false);
            }
        }

        post_modified_macos_key_chord(
            LEFT_COMMAND_KEYCODE,
            D_KEYCODE,
            LEFT_OPTION_KEYCODE,
            "MetaLeft",
            "KeyD",
            "AltLeft",
            &self.current_modifiers,
            &self.loopback,
        )?;
        self.last_dock_action_at = Some(now);
        Ok(true)
    }

    fn trigger_macos_dock_hide(&mut self) -> Result<bool, String> {
        let now = Instant::now();
        if let Some(last_action_at) = self.last_dock_action_at {
            if now.duration_since(last_action_at) < Duration::from_millis(350) {
                return Ok(false);
            }
        }

        post_modified_macos_key_chord(
            LEFT_COMMAND_KEYCODE,
            D_KEYCODE,
            LEFT_OPTION_KEYCODE,
            "MetaLeft",
            "KeyD",
            "AltLeft",
            &self.current_modifiers,
            &self.loopback,
        )?;
        self.last_dock_action_at = Some(now);
        Ok(true)
    }

    fn dock_hide_is_allowed(&self) -> bool {
        match self.dock_hide_allowed_at {
            Some(deadline) => Instant::now() >= deadline,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_modifier_flags, dock_cursor_zone, dock_proxy_transition, key_code_to_macos_virtual,
        macos_posted_delta, modifier_flag_for_keycode, DockCursorZone, DockProxyAction,
    };
    use crate::event::Modifiers;
    use core_graphics::event::CGEventFlags;

    // ── build_modifier_flags tests ────────────────────────────────────

    #[test]
    fn modifier_flags_always_include_non_coalesced() {
        let flags = build_modifier_flags(&Modifiers::none());
        assert!(
            flags.contains(CGEventFlags::CGEventFlagNonCoalesced),
            "CGEventFlagNonCoalesced (0x100) must always be set"
        );
    }

    #[test]
    fn modifier_flags_with_no_modifiers_only_has_non_coalesced() {
        let flags = build_modifier_flags(&Modifiers::none());
        assert_eq!(flags, CGEventFlags::CGEventFlagNonCoalesced);
    }

    #[test]
    fn modifier_flag_for_keycode_identifies_modifier_keys() {
        // Shift
        assert!(modifier_flag_for_keycode(0x38).is_some());
        assert!(modifier_flag_for_keycode(0x3C).is_some());
        // Control
        assert!(modifier_flag_for_keycode(0x3B).is_some());
        assert!(modifier_flag_for_keycode(0x3E).is_some());
        // Alt/Option
        assert!(modifier_flag_for_keycode(0x3A).is_some());
        assert!(modifier_flag_for_keycode(0x3D).is_some());
        // Command/Meta
        assert!(modifier_flag_for_keycode(0x37).is_some());
        assert!(modifier_flag_for_keycode(0x36).is_some());
        // Regular key (KeyA = 0x00)
        assert!(modifier_flag_for_keycode(0x00).is_none());
    }

    #[test]
    fn modifier_flags_shift_sets_shift_bit() {
        let flags = build_modifier_flags(&Modifiers {
            shift: true,
            control: false,
            alt: false,
            meta: false,
        });
        assert!(flags.contains(CGEventFlags::CGEventFlagShift));
        assert!(flags.contains(CGEventFlags::CGEventFlagNonCoalesced));
        assert!(!flags.contains(CGEventFlags::CGEventFlagControl));
        assert!(!flags.contains(CGEventFlags::CGEventFlagAlternate));
        assert!(!flags.contains(CGEventFlags::CGEventFlagCommand));
    }

    #[test]
    fn modifier_flags_control_sets_control_bit() {
        let flags = build_modifier_flags(&Modifiers {
            shift: false,
            control: true,
            alt: false,
            meta: false,
        });
        assert!(flags.contains(CGEventFlags::CGEventFlagControl));
        assert!(flags.contains(CGEventFlags::CGEventFlagNonCoalesced));
    }

    #[test]
    fn modifier_flags_alt_sets_alternate_bit() {
        let flags = build_modifier_flags(&Modifiers {
            shift: false,
            control: false,
            alt: true,
            meta: false,
        });
        assert!(flags.contains(CGEventFlags::CGEventFlagAlternate));
        assert!(flags.contains(CGEventFlags::CGEventFlagNonCoalesced));
    }

    #[test]
    fn modifier_flags_meta_sets_command_bit() {
        let flags = build_modifier_flags(&Modifiers {
            shift: false,
            control: false,
            alt: false,
            meta: true,
        });
        assert!(flags.contains(CGEventFlags::CGEventFlagCommand));
        assert!(flags.contains(CGEventFlags::CGEventFlagNonCoalesced));
    }

    #[test]
    fn modifier_flags_all_modifiers_combined() {
        let flags = build_modifier_flags(&Modifiers {
            shift: true,
            control: true,
            alt: true,
            meta: true,
        });
        assert!(flags.contains(CGEventFlags::CGEventFlagShift));
        assert!(flags.contains(CGEventFlags::CGEventFlagControl));
        assert!(flags.contains(CGEventFlags::CGEventFlagAlternate));
        assert!(flags.contains(CGEventFlags::CGEventFlagCommand));
        assert!(flags.contains(CGEventFlags::CGEventFlagNonCoalesced));
    }

    // ── key_code_to_macos_virtual tests ───────────────────────────────

    #[test]
    fn all_letter_keys_map_to_distinct_virtual_keycodes() {
        let letters = [
            "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ",
            "KeyK", "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT",
            "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ",
        ];
        let mut seen = std::collections::HashSet::new();
        for letter in &letters {
            let keycode = key_code_to_macos_virtual(letter);
            assert!(
                keycode.is_some(),
                "{letter} should map to a macOS virtual keycode"
            );
            assert!(
                seen.insert(keycode.unwrap()),
                "{letter} maps to a duplicate keycode"
            );
        }
    }

    #[test]
    fn all_digit_keys_map_to_distinct_virtual_keycodes() {
        let digits = [
            "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7",
            "Digit8", "Digit9",
        ];
        let mut seen = std::collections::HashSet::new();
        for digit in &digits {
            let keycode = key_code_to_macos_virtual(digit);
            assert!(
                keycode.is_some(),
                "{digit} should map to a macOS virtual keycode"
            );
            assert!(
                seen.insert(keycode.unwrap()),
                "{digit} maps to a duplicate keycode"
            );
        }
    }

    #[test]
    fn modifier_keys_map_to_expected_virtual_keycodes() {
        assert_eq!(key_code_to_macos_virtual("ShiftLeft"), Some(0x38));
        assert_eq!(key_code_to_macos_virtual("ShiftRight"), Some(0x3C));
        assert_eq!(key_code_to_macos_virtual("ControlLeft"), Some(0x3B));
        assert_eq!(key_code_to_macos_virtual("ControlRight"), Some(0x3E));
        assert_eq!(key_code_to_macos_virtual("AltLeft"), Some(0x3A));
        assert_eq!(key_code_to_macos_virtual("AltRight"), Some(0x3D));
        assert_eq!(key_code_to_macos_virtual("MetaLeft"), Some(0x37));
        assert_eq!(key_code_to_macos_virtual("MetaRight"), Some(0x36));
    }

    #[test]
    fn navigation_keys_map_to_expected_virtual_keycodes() {
        assert_eq!(key_code_to_macos_virtual("ArrowUp"), Some(0x7E));
        assert_eq!(key_code_to_macos_virtual("ArrowDown"), Some(0x7D));
        assert_eq!(key_code_to_macos_virtual("ArrowLeft"), Some(0x7B));
        assert_eq!(key_code_to_macos_virtual("ArrowRight"), Some(0x7C));
        assert_eq!(key_code_to_macos_virtual("Home"), Some(0x73));
        assert_eq!(key_code_to_macos_virtual("End"), Some(0x77));
        assert_eq!(key_code_to_macos_virtual("PageUp"), Some(0x74));
        assert_eq!(key_code_to_macos_virtual("PageDown"), Some(0x79));
    }

    #[test]
    fn common_editing_keys_map_to_expected_virtual_keycodes() {
        assert_eq!(key_code_to_macos_virtual("Enter"), Some(0x24));
        assert_eq!(key_code_to_macos_virtual("Tab"), Some(0x30));
        assert_eq!(key_code_to_macos_virtual("Space"), Some(0x31));
        assert_eq!(key_code_to_macos_virtual("Backspace"), Some(0x33));
        assert_eq!(key_code_to_macos_virtual("Delete"), Some(0x75));
        assert_eq!(key_code_to_macos_virtual("Escape"), Some(0x35));
        assert_eq!(key_code_to_macos_virtual("CapsLock"), Some(0x39));
    }

    #[test]
    fn function_keys_f1_through_f12_are_mapped() {
        let f_keys = [
            "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
        ];
        for f_key in &f_keys {
            assert!(
                key_code_to_macos_virtual(f_key).is_some(),
                "{f_key} should map to a macOS virtual keycode"
            );
        }
    }

    #[test]
    fn punctuation_keys_are_mapped() {
        let punctuation = [
            "Minus",
            "Equal",
            "BracketLeft",
            "BracketRight",
            "Backslash",
            "Semicolon",
            "Quote",
            "Backquote",
            "Comma",
            "Period",
            "Slash",
        ];
        for key in &punctuation {
            assert!(
                key_code_to_macos_virtual(key).is_some(),
                "{key} should map to a macOS virtual keycode"
            );
        }
    }

    #[test]
    fn numpad_keys_are_mapped() {
        let numpad = [
            "Numpad0",
            "Numpad1",
            "Numpad2",
            "Numpad3",
            "Numpad4",
            "Numpad5",
            "Numpad6",
            "Numpad7",
            "Numpad8",
            "Numpad9",
            "NumpadAdd",
            "NumpadSubtract",
            "NumpadMultiply",
            "NumpadDivide",
            "NumpadDecimal",
            "NumpadEnter",
            "NumpadEqual",
        ];
        for key in &numpad {
            assert!(
                key_code_to_macos_virtual(key).is_some(),
                "{key} should map to a macOS virtual keycode"
            );
        }
    }

    #[test]
    fn unmapped_codes_return_none() {
        assert_eq!(key_code_to_macos_virtual("Insert"), None);
        assert_eq!(key_code_to_macos_virtual("ScrollLock"), None);
        assert_eq!(key_code_to_macos_virtual("PrintScreen"), None);
        assert_eq!(key_code_to_macos_virtual("Pause"), None);
        assert_eq!(key_code_to_macos_virtual("NonExistentKey"), None);
        assert_eq!(key_code_to_macos_virtual(""), None);
    }

    // ── Windows→Mac key code round-trip coverage ──────────────────────
    //
    // Every protocol code that `normalize_key_code` (Windows capture) can
    // emit for standard keys must have a mapping in
    // `key_code_to_macos_virtual`, otherwise those keys are silently
    // dropped on the Mac side.

    #[test]
    fn all_windows_normalized_keycodes_have_macos_virtual_mapping() {
        // These are the protocol codes emitted by normalize_key_code in
        // normalize.rs for all standard keyboard keys.
        let windows_codes = [
            // Letters
            "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ",
            "KeyK", "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT",
            "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ",
            // Digits
            "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7",
            "Digit8", "Digit9",
            // Modifiers
            "ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight",
            "MetaLeft", "MetaRight",
            // Navigation
            "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End", "PageUp",
            "PageDown",
            // Editing
            "Enter", "Tab", "Space", "Backspace", "Delete", "Escape", "CapsLock",
            // Punctuation
            "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "Semicolon", "Quote",
            "Backquote", "Comma", "Period", "Slash",
            // Function keys
            "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
            // Numpad
            "Numpad0", "Numpad1", "Numpad2", "Numpad3", "Numpad4", "Numpad5", "Numpad6",
            "Numpad7", "Numpad8", "Numpad9", "NumpadAdd", "NumpadSubtract", "NumpadMultiply",
            "NumpadDivide", "NumpadDecimal", "NumpadEnter", "NumpadEqual", "NumLock",
        ];

        let mut unmapped = Vec::new();
        for code in &windows_codes {
            if key_code_to_macos_virtual(code).is_none() {
                unmapped.push(*code);
            }
        }
        assert!(
            unmapped.is_empty(),
            "the following Windows key codes have no macOS virtual keycode mapping: {unmapped:?}"
        );
    }

    // ── existing tests ────────────────────────────────────────────────

    #[test]
    fn preserves_edge_pressure_when_cursor_is_clamped() {
        assert_eq!(macos_posted_delta(12, 0), 12);
        assert_eq!(macos_posted_delta(-7, 0), -7);
    }

    #[test]
    fn uses_applied_delta_when_cursor_actually_moves() {
        assert_eq!(macos_posted_delta(12, 9), 9);
        assert_eq!(macos_posted_delta(-7, -5), -5);
    }

    #[test]
    fn dock_proxy_only_shows_at_the_bottom_edge() {
        assert_eq!(dock_cursor_zone(0.0, 1.0, 80.0), DockCursorZone::Edge);
        assert_eq!(
            dock_proxy_transition(DockCursorZone::BottomBand, DockCursorZone::Edge),
            Some(DockProxyAction::Show)
        );
        assert_eq!(
            dock_proxy_transition(DockCursorZone::Interior, DockCursorZone::BottomBand),
            None
        );
    }

    #[test]
    fn dock_proxy_does_not_re_trigger_while_staying_at_edge() {
        assert_eq!(
            dock_proxy_transition(DockCursorZone::Edge, DockCursorZone::Edge),
            None
        );
    }

    #[test]
    fn dock_proxy_only_hides_after_leaving_the_bottom_band() {
        assert_eq!(
            dock_proxy_transition(DockCursorZone::Edge, DockCursorZone::BottomBand),
            None
        );
        assert_eq!(
            dock_proxy_transition(DockCursorZone::BottomBand, DockCursorZone::Interior),
            Some(DockProxyAction::Hide)
        );
    }
}

fn dock_cursor_zone(
    distance_from_bottom: f64,
    reveal_threshold: f64,
    hide_threshold: f64,
) -> DockCursorZone {
    if distance_from_bottom <= reveal_threshold {
        DockCursorZone::Edge
    } else if distance_from_bottom <= hide_threshold {
        DockCursorZone::BottomBand
    } else {
        DockCursorZone::Interior
    }
}

fn dock_proxy_transition(
    previous: DockCursorZone,
    current: DockCursorZone,
) -> Option<DockProxyAction> {
    match (previous, current) {
        (DockCursorZone::Edge, DockCursorZone::Edge) => None,
        (_, DockCursorZone::Edge) => Some(DockProxyAction::Show),
        (DockCursorZone::Interior, DockCursorZone::BottomBand) => None,
        (DockCursorZone::BottomBand, DockCursorZone::Interior) => Some(DockProxyAction::Hide),
        (DockCursorZone::Edge, DockCursorZone::Interior) => Some(DockProxyAction::Hide),
        _ => None,
    }
}

fn macos_posted_delta(requested: i32, applied: i32) -> i32 {
    if applied == 0 && requested != 0 {
        requested
    } else {
        applied
    }
}

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

const D_KEYCODE: CGKeyCode = 0x02;
const LEFT_COMMAND_KEYCODE: CGKeyCode = 0x37;
const LEFT_OPTION_KEYCODE: CGKeyCode = 0x3A;

fn keycode_to_event_flag(keycode: CGKeyCode) -> CGEventFlags {
    match keycode {
        LEFT_COMMAND_KEYCODE => CGEventFlags::CGEventFlagCommand,
        LEFT_OPTION_KEYCODE => CGEventFlags::CGEventFlagAlternate,
        0x38 => CGEventFlags::CGEventFlagShift,
        0x3B => CGEventFlags::CGEventFlagControl,
        _ => CGEventFlags::CGEventFlagNull,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockProxyAction {
    Show,
    Hide,
}

fn post_macos_key_event(
    keycode: CGKeyCode,
    key_down: bool,
    flags: CGEventFlags,
) -> Result<(), String> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "failed to create macOS event source for keyboard event".to_string())?;
    let event = CGEvent::new_keyboard_event(source, keycode, key_down)
        .map_err(|_| "failed to create macOS keyboard event".to_string())?;
    event.set_flags(flags);
    event.post(CGEventTapLocation::HID);
    Ok(())
}

fn post_modified_macos_key_chord(
    first_keycode: CGKeyCode,
    keycode: CGKeyCode,
    second_keycode: CGKeyCode,
    first_code: &str,
    key_code: &str,
    second_code: &str,
    current_modifiers: &Modifiers,
    loopback: &Option<SharedLoopbackSuppressor>,
) -> Result<(), String> {
    record_loopback_key_event(
        loopback,
        first_code,
        true,
        with_modifier_applied(*current_modifiers, first_code),
    );
    record_loopback_key_event(
        loopback,
        second_code,
        true,
        with_modifier_applied(
            with_modifier_applied(*current_modifiers, first_code),
            second_code,
        ),
    );
    record_loopback_key_event(
        loopback,
        key_code,
        true,
        with_modifier_applied(
            with_modifier_applied(*current_modifiers, first_code),
            second_code,
        ),
    );
    record_loopback_key_event(
        loopback,
        key_code,
        false,
        with_modifier_applied(
            with_modifier_applied(*current_modifiers, first_code),
            second_code,
        ),
    );
    record_loopback_key_event(
        loopback,
        second_code,
        false,
        with_modifier_applied(*current_modifiers, first_code),
    );
    record_loopback_key_event(loopback, first_code, false, *current_modifiers);

    let first_flag = keycode_to_event_flag(first_keycode);
    let second_flag = keycode_to_event_flag(second_keycode);
    let both_flags = first_flag | second_flag;

    post_macos_key_event(first_keycode, true, first_flag)?;
    post_macos_key_event(second_keycode, true, both_flags)?;
    post_macos_key_event(keycode, true, both_flags)?;
    post_macos_key_event(keycode, false, both_flags)?;
    post_macos_key_event(second_keycode, false, first_flag)?;
    post_macos_key_event(first_keycode, false, CGEventFlags::CGEventFlagNull)?;
    Ok(())
}

fn with_modifier_applied(mut modifiers: Modifiers, key_code: &str) -> Modifiers {
    match key_code {
        "MetaLeft" | "MetaRight" => modifiers.meta = true,
        "AltLeft" | "AltRight" => modifiers.alt = true,
        "ControlLeft" | "ControlRight" => modifiers.control = true,
        "ShiftLeft" | "ShiftRight" => modifiers.shift = true,
        _ => {}
    }
    modifiers
}

fn record_loopback_key_event(
    loopback: &Option<SharedLoopbackSuppressor>,
    code: &str,
    pressed: bool,
    modifiers: Modifiers,
) {
    let Some(loopback) = loopback else {
        return;
    };

    let event = if pressed {
        InputEvent::KeyDown {
            code: code.to_string(),
            modifiers,
            timestamp_us: 0,
        }
    } else {
        InputEvent::KeyUp {
            code: code.to_string(),
            modifiers,
            timestamp_us: 0,
        }
    };

    if let Ok(mut loopback) = loopback.lock() {
        loopback.record(event);
    }
}

fn macos_visible_desktop_bounds() -> Option<CoordinateBounds> {
    let displays = CGDisplay::active_displays().ok()?;
    let mut iter = displays.into_iter();
    let first = iter.next()?;
    let mut bounds = CoordinateBounds::from_rect(CGDisplay::new(first).bounds());

    for display_id in iter {
        bounds = bounds.union(CoordinateBounds::from_rect(
            CGDisplay::new(display_id).bounds(),
        ));
    }

    Some(bounds)
}

fn clamp_point(point: (f64, f64), bounds: CoordinateBounds) -> (f64, f64) {
    (
        point.0.clamp(bounds.min_x, bounds.max_x),
        point.1.clamp(bounds.min_y, bounds.max_y),
    )
}

impl CoordinateBounds {
    fn from_rect(rect: core_graphics::geometry::CGRect) -> Self {
        Self {
            min_x: rect.origin.x,
            max_x: rect.origin.x + rect.size.width,
            min_y: rect.origin.y,
            max_y: rect.origin.y + rect.size.height,
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            max_x: self.max_x.max(other.max_x),
            min_y: self.min_y.min(other.min_y),
            max_y: self.max_y.max(other.max_y),
        }
    }
}
