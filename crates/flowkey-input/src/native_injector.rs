use std::collections::HashSet;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(not(target_os = "macos"))]
use enigo::Coordinate;
use enigo::{Axis, Button, Direction, Enigo, Key, Keyboard, Mouse, Settings};
#[cfg(target_os = "macos")]
use tracing::debug;
use tracing::warn;

#[cfg(target_os = "macos")]
use core_graphics::display::CGDisplay;
#[cfg(target_os = "macos")]
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
};
#[cfg(target_os = "macos")]
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
#[cfg(target_os = "macos")]
use core_graphics::geometry::{CGPoint, CGRect};

use crate::event::{InputEvent, Modifiers, MouseButton};
use crate::inject::InputInjector;
use crate::keycode::{modifier_from_mask, parse_key_code, KeyCode, ModifierKind, NamedKey};
use crate::loopback::SharedLoopbackSuppressor;

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq)]
struct CoordinateBounds {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

pub struct NativeInputSink {
    platform: &'static str,
    enigo: Enigo,
    pressed_keys: HashSet<Key>,
    pressed_buttons: HashSet<Button>,
    current_modifiers: Modifiers,
    loopback: Option<SharedLoopbackSuppressor>,
    #[cfg(target_os = "macos")]
    cursor_position: Option<(f64, f64)>,
    #[cfg(target_os = "macos")]
    last_dock_zone: DockCursorZone,
    #[cfg(target_os = "macos")]
    last_dock_action_at: Option<Instant>,
    #[cfg(target_os = "macos")]
    dock_hide_allowed_at: Option<Instant>,
    #[cfg(target_os = "macos")]
    dock_visible: bool,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockCursorZone {
    Edge,
    BottomBand,
    Interior,
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
            #[cfg(target_os = "macos")]
            last_dock_zone: DockCursorZone::Interior,
            #[cfg(target_os = "macos")]
            last_dock_action_at: None,
            #[cfg(target_os = "macos")]
            dock_hide_allowed_at: None,
            #[cfg(target_os = "macos")]
            dock_visible: false,
        })
    }

    fn handle_input_event(&mut self, event: &InputEvent) -> Result<(), String> {
        self.record_loopback(event);
        match event {
            InputEvent::KeyDown { code, modifiers, .. } => {
                let key_code = parse_key_code(code);
                self.sync_modifiers(modifiers, modifier_code_for(&key_code))?;
                self.key_action(key_code, Direction::Press)
            }
            InputEvent::KeyUp { code, modifiers, .. } => {
                let key_code = parse_key_code(code);
                self.sync_modifiers(modifiers, modifier_code_for(&key_code))?;
                self.key_action(key_code, Direction::Release)
            }
            InputEvent::MouseMove { dx, dy, modifiers, .. } => {
                self.sync_modifiers(modifiers, None)?;
                self.move_mouse(*dx, *dy)
            }
            InputEvent::MouseButtonDown { button, modifiers, .. } => {
                self.sync_modifiers(modifiers, None)?;
                self.button_action(*button, Direction::Press)
            }
            InputEvent::MouseButtonUp { button, modifiers, .. } => {
                self.sync_modifiers(modifiers, None)?;
                self.button_action(*button, Direction::Release)
            }
            InputEvent::MouseWheel {
                delta_x,
                delta_y,
                modifiers,
                ..
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
                // First move: read the actual cursor position via enigo which
                // uses NSEvent::mouseLocation (reliable, always reflects the
                // true screen position including after programmatic warps).
                // CGEvent::new(HIDSystemState) can return (0, 0) when no
                // recent hardware input exists.
                let (x, y) = self.enigo.location().map_err(|error| error.to_string())?;
                (f64::from(x), f64::from(y))
            }
        };
        let raw_target = (current.0 + f64::from(dx), current.1 + f64::from(dy));
        let bounds = macos_visible_desktop_bounds();
        let target = bounds
            .map(|bounds| clamp_point(raw_target, bounds))
            .unwrap_or(raw_target);
        let posted_dx = macos_posted_delta(dx, round_delta(target.0 - current.0));
        let posted_dy = macos_posted_delta(dy, round_delta(target.1 - current.1));
        let dest = CGPoint::new(target.0, target.1);
        let clamped = target != raw_target;

        // Always warp the cursor first — this is reliable, invisible to the
        // event system (no CGEvent generated), and keeps the OS cursor in sync.
        CGDisplay::warp_mouse_cursor_position(dest).map_err(|error| format!("{error:?}"))?;

        if self.pressed_buttons.is_empty() {
            // Follow the warp with a real mouse-moved event so macOS features
            // that key off pointer motion, such as Dock edge reveal, can
            // observe the movement. A warp alone updates position but does
            // not always behave like a hardware mouse-move from AppKit's
            // perspective.
            let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                .map_err(|_| "failed to create macOS event source for move".to_string())?;
            let move_event = CGEvent::new_mouse_event(
                source,
                CGEventType::MouseMoved,
                dest,
                CGMouseButton::Left,
            )
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
            // A button is held: additionally post a drag event so macOS
            // recognises the gesture as a drag-and-drop operation. The warp
            // above already moved the cursor; this CGEvent tells AppKit and
            // other frameworks that a drag is in progress.
            let (event_type, cg_button) = if self.pressed_buttons.contains(&Button::Left) {
                (CGEventType::LeftMouseDragged, CGMouseButton::Left)
            } else if self.pressed_buttons.contains(&Button::Right) {
                (CGEventType::RightMouseDragged, CGMouseButton::Right)
            } else {
                (CGEventType::OtherMouseDragged, CGMouseButton::Center)
            };
            let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                .map_err(|_| "failed to create macOS event source for drag".to_string())?;
            let event = CGEvent::new_mouse_event(source, event_type, dest, cg_button)
                .map_err(|_| "failed to create macOS drag event".to_string())?;
            // Set the relative delta fields so the system sees real movement.
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
                platform = self.platform,
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
                buttons_pressed = self.pressed_buttons.len(),
                "macOS mouse move reached edge-sensitive injection path"
            );
        }

        if let Some(bounds) = bounds {
            self.update_macos_dock_proxy(target, bounds)?;
        }

        self.cursor_position = Some(target);
        Ok(())
    }

    #[cfg(target_os = "macos")]
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
                if self.dock_visible && self.dock_hide_is_allowed() && self.trigger_macos_dock_hide()? {
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

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
    fn dock_hide_is_allowed(&self) -> bool {
        match self.dock_hide_allowed_at {
            Some(deadline) => Instant::now() >= deadline,
            None => true,
        }
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
                    timestamp_us: 0,
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
            self.last_dock_zone = DockCursorZone::Interior;
            self.dock_hide_allowed_at = None;
            self.dock_visible = false;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn clamp_point(point: (f64, f64), bounds: CoordinateBounds) -> (f64, f64) {
    (
        point.0.clamp(bounds.min_x, bounds.max_x),
        point.1.clamp(bounds.min_y, bounds.max_y),
    )
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn macos_posted_delta(requested: i32, applied: i32) -> i32 {
    if applied == 0 && requested != 0 {
        requested
    } else {
        applied
    }
}

#[cfg(target_os = "macos")]
const D_KEYCODE: CGKeyCode = 0x02;

#[cfg(target_os = "macos")]
const LEFT_COMMAND_KEYCODE: CGKeyCode = 0x37;

#[cfg(target_os = "macos")]
const LEFT_OPTION_KEYCODE: CGKeyCode = 0x3A;

#[cfg(target_os = "macos")]
fn keycode_to_event_flag(keycode: CGKeyCode) -> CGEventFlags {
    match keycode {
        LEFT_COMMAND_KEYCODE => CGEventFlags::CGEventFlagCommand,
        LEFT_OPTION_KEYCODE => CGEventFlags::CGEventFlagAlternate,
        0x38 => CGEventFlags::CGEventFlagShift,    // Left Shift
        0x3B => CGEventFlags::CGEventFlagControl,   // Left Control
        _ => CGEventFlags::CGEventFlagNull,
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockProxyAction {
    Show,
    Hide,
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn post_macos_key_event(keycode: CGKeyCode, key_down: bool, flags: CGEventFlags) -> Result<(), String> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "failed to create macOS event source for keyboard event".to_string())?;
    let event = CGEvent::new_keyboard_event(source, keycode, key_down)
        .map_err(|_| "failed to create macOS keyboard event".to_string())?;
    event.set_flags(flags);
    event.post(CGEventTapLocation::HID);
    Ok(())
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

    let mut loopback = loopback
        .lock()
        .expect("loopback suppressor mutex should not be poisoned");
    loopback.record(event);
}

#[cfg(target_os = "macos")]
impl CoordinateBounds {
    fn from_rect(rect: CGRect) -> Self {
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

#[cfg(all(test, target_os = "macos"))]
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

    Some(InputEvent::KeyUp {
        code,
        modifiers,
        timestamp_us: 0,
    })
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
