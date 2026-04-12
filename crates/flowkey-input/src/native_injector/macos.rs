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
use enigo::{Direction, Mouse};
use std::time::{Duration, Instant};
use tracing::debug;

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

    CGDisplay::warp_mouse_cursor_position(dest).map_err(|error| format!("{error:?}"))?;

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
        move_event.post(CGEventTapLocation::Session);
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

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "failed to create macOS event source for button event".to_string())?;
    let event = CGEvent::new_mouse_event(source, event_type, dest, cg_button)
        .map_err(|_| "failed to create macOS mouse-button event".to_string())?;
    event.post(CGEventTapLocation::HID);
    Ok(())
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
        dock_cursor_zone, dock_proxy_transition, macos_posted_delta, DockCursorZone,
        DockProxyAction,
    };

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
