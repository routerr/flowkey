use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::SystemTime;

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, EventField};
use core_graphics::sys::CGEventRef;
use flowkey_input::capture::{CaptureSignal, CaptureState, InputCapture};
use flowkey_input::event::InputEvent;
use flowkey_input::hotkey::{HotkeyBinding, HotkeyTracker};
use flowkey_input::loopback::SharedLoopbackSuppressor;
use foreign_types_shared::ForeignType;
use rdev::{Button, Event, EventType, Key};
use tracing::warn;

type CFMachPortRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopMode = *mut c_void;
type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    etype: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[allow(non_upper_case_globals)]
const kCGHeadInsertEventTap: u32 = 0;

#[link(name = "Cocoa", kind = "framework")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: CGEventTapLocation,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        tap: CFMachPortRef,
        order: i64,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CFRunLoopRun();
    static kCFRunLoopCommonModes: CFRunLoopMode;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
}

pub struct MacosCapture {
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    receiver: Option<Receiver<CaptureSignal>>,
    suppression_enabled: Arc<AtomicBool>,
    started: bool,
    exclusive: bool,
}

impl MacosCapture {
    pub fn new(binding: HotkeyBinding) -> Self {
        Self::with_loopback(binding, None, false, Arc::new(AtomicBool::new(false)))
    }

    pub fn with_loopback(
        binding: HotkeyBinding,
        loopback: Option<SharedLoopbackSuppressor>,
        exclusive: bool,
        suppression_enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            binding,
            loopback,
            receiver: None,
            suppression_enabled,
            started: false,
            exclusive,
        }
    }
}

impl InputCapture for MacosCapture {
    fn start(&mut self) -> Result<(), String> {
        if self.started {
            return Ok(());
        }

        let (sender, receiver) = mpsc::channel();
        let binding = self.binding.clone();
        let loopback = self.loopback.clone();
        let suppression_enabled = Arc::clone(&self.suppression_enabled);
        let exclusive = self.exclusive;
        self.receiver = Some(receiver);
        self.started = true;

        thread::spawn(move || {
            let mut context = Box::new(TapContext {
                sender,
                tracker: Arc::new(Mutex::new(HotkeyTracker::new(binding))),
                state: Arc::new(Mutex::new(CaptureState::default())),
                loopback,
                suppression_enabled,
                exclusive,
                tap: std::ptr::null_mut(),
                last_flags: CGEventFlags::CGEventFlagNull,
                cursor_decoupled: false,
            });

            let context_ptr: *mut TapContext = &mut *context;
            let mask = event_mask(&[
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                CGEventType::MouseMoved,
                CGEventType::LeftMouseDragged,
                CGEventType::RightMouseDragged,
                CGEventType::OtherMouseDragged,
                CGEventType::KeyDown,
                CGEventType::KeyUp,
                CGEventType::FlagsChanged,
                CGEventType::ScrollWheel,
            ]);

            let tap = unsafe {
                CGEventTapCreate(
                    CGEventTapLocation::HID,
                    kCGHeadInsertEventTap,
                    if exclusive { 0 } else { 1 },
                    mask,
                    raw_callback,
                    context_ptr.cast(),
                )
            };

            if tap.is_null() {
                warn!("macOS event tap creation failed");
                return;
            }

            context.tap = tap;

            unsafe {
                let loop_source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
                if loop_source.is_null() {
                    warn!("macOS event tap runloop source creation failed");
                    return;
                }

                CFRunLoopAddSource(CFRunLoopGetCurrent(), loop_source, kCFRunLoopCommonModes);
                CGEventTapEnable(tap, true);
                CFRunLoopRun();
            }
        });

        Ok(())
    }

    fn poll(&mut self) -> Option<CaptureSignal> {
        self.receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
    }

    fn wait(&mut self) -> Option<CaptureSignal> {
        self.receiver
            .as_ref()
            .and_then(|receiver| receiver.recv().ok())
    }

    fn set_suppression_enabled(&mut self, enabled: bool) {
        if self.exclusive {
            self.suppression_enabled.store(enabled, Ordering::SeqCst);
            // Decouple the on-screen cursor from hardware mouse input while
            // we are the controller. CGEventTap alone does not freeze the
            // cursor — the OS moves it independently of the event stream —
            // so we must explicitly disassociate it here and reassociate on
            // release. Our tap still receives raw MOUSE_EVENT_DELTA_X/Y from
            // the HID layer, which we forward to the remote peer.
            unsafe {
                CGAssociateMouseAndMouseCursorPosition(!enabled);
            }
        }
    }
}

struct TapContext {
    sender: mpsc::Sender<CaptureSignal>,
    tracker: Arc<Mutex<HotkeyTracker>>,
    state: Arc<Mutex<CaptureState>>,
    loopback: Option<SharedLoopbackSuppressor>,
    suppression_enabled: Arc<AtomicBool>,
    exclusive: bool,
    tap: CFMachPortRef,
    last_flags: CGEventFlags,
    cursor_decoupled: bool,
}

unsafe extern "C" fn raw_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let context = &mut *(user_info as *mut TapContext);
    let cg_event = ManuallyDrop::new(CGEvent::from_ptr(event));

    if matches!(
        event_type,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        warn!(
            target: "capture",
            ?event_type,
            "macOS event tap was disabled; re-enabling"
        );
        if !context.tap.is_null() {
            CGEventTapEnable(context.tap, true);
        }
        return cg_event.as_ptr();
    }

    let suppress_active = context.exclusive && context.suppression_enabled.load(Ordering::SeqCst);

    // Sync cursor decoupling with current suppression state. CGEventTap
    // alone cannot freeze the on-screen cursor, so whenever suppression
    // flips we must explicitly (dis)associate hardware mouse input from
    // cursor position. Doing this inside the tap callback means both the
    // hotkey path and the IPC path (which only flips the atomic) converge
    // on the same decouple without extra wiring.
    if suppress_active != context.cursor_decoupled {
        CGAssociateMouseAndMouseCursorPosition(!suppress_active);
        context.cursor_decoupled = suppress_active;
    }

    // Mouse moves in exclusive+suppression mode: use raw hardware deltas.
    // This avoids the last_mouse_position tracking, which becomes unreliable
    // once the OS cursor is frozen (deltas accumulate incorrectly otherwise).
    if suppress_active
        && matches!(
            event_type,
            CGEventType::MouseMoved
                | CGEventType::LeftMouseDragged
                | CGEventType::RightMouseDragged
                | CGEventType::OtherMouseDragged
        )
    {
        let raw_dx = cg_event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X);
        let raw_dy = cg_event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y);
        if raw_dx != 0 || raw_dy != 0 {
            let modifiers = {
                let state = lock_recovering(&context.state, "capture state");
                state.modifiers
            };
            let timestamp_us = SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as u64;
            let input = InputEvent::MouseMove {
                dx: raw_dx as i32,
                dy: raw_dy as i32,
                modifiers,
                timestamp_us,
            };
            let _ = context.sender.send(CaptureSignal::Input(input));
        }
        // Fully drop the event so the local Mac cursor does not move.
        return std::ptr::null_mut();
    }

    let Some(translated_event) = convert_cg_event(event_type, &cg_event, &mut context.last_flags)
    else {
        return cg_event.as_ptr();
    };

    let mut tracker = lock_recovering(&context.tracker, "hotkey tracker");
    let mut state = lock_recovering(&context.state, "capture state");

    match state.translate(
        translated_event.clone(),
        &mut tracker,
        context.loopback.as_ref(),
    ) {
        Some(CaptureSignal::HotkeyPressed) => {
            let _ = context.sender.send(CaptureSignal::HotkeyPressed);
            // Drop the hotkey key itself so it does not leak to local apps.
            std::ptr::null_mut()
        }
        Some(CaptureSignal::HotkeySuppressed) => {
            // Always deliver key release for hotkey modifiers locally,
            // otherwise stuck-key behavior.
            cg_event.as_ptr()
        }
        Some(CaptureSignal::Input(input)) => {
            let _ = context.sender.send(CaptureSignal::Input(input));
            if suppress_active {
                // Fully drop local input while controlling remote (explicit mode).
                std::ptr::null_mut()
            } else {
                cg_event.as_ptr()
            }
        }
        None => cg_event.as_ptr(),
    }
}

fn lock_recovering<'a, T>(mutex: &'a Arc<Mutex<T>>, label: &'static str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            warn!(target: "capture", mutex = label, "poisoned mutex, recovering");
            mutex.clear_poison();
            poisoned.into_inner()
        }
    }
}

fn event_mask(events: &[CGEventType]) -> u64 {
    events.iter().fold(0u64, |mask, event_type| {
        mask | (1u64 << (*event_type as u64))
    })
}

fn convert_cg_event(
    event_type: CGEventType,
    event: &CGEvent,
    last_flags: &mut CGEventFlags,
) -> Option<Event> {
    let now = SystemTime::now();
    let event_type = match event_type {
        CGEventType::LeftMouseDown => EventType::ButtonPress(Button::Left),
        CGEventType::LeftMouseUp => EventType::ButtonRelease(Button::Left),
        CGEventType::RightMouseDown => EventType::ButtonPress(Button::Right),
        CGEventType::RightMouseUp => EventType::ButtonRelease(Button::Right),
        CGEventType::OtherMouseDown => EventType::ButtonPress(Button::Middle),
        CGEventType::OtherMouseUp => EventType::ButtonRelease(Button::Middle),
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            let point = event.location();
            return Some(Event {
                time: now,
                name: None,
                event_type: EventType::MouseMove {
                    x: point.x,
                    y: point.y,
                },
            });
        }
        CGEventType::KeyDown => EventType::KeyPress(key_from_code(
            event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16,
        )),
        CGEventType::KeyUp => EventType::KeyRelease(key_from_code(
            event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16,
        )),
        CGEventType::FlagsChanged => {
            let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
            let current_flags = event.get_flags();
            let key = key_from_code(code);
            let event_type = if current_flags < *last_flags {
                EventType::KeyRelease(key)
            } else {
                EventType::KeyPress(key)
            };
            *last_flags = current_flags;
            event_type
        }
        CGEventType::ScrollWheel => {
            let delta_y =
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
            let delta_x =
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
            return Some(Event {
                time: now,
                name: None,
                event_type: EventType::Wheel { delta_x, delta_y },
            });
        }
        _ => return None,
    };

    Some(Event {
        time: now,
        name: None,
        event_type,
    })
}

fn key_from_code(code: u16) -> Key {
    match code {
        58 => Key::Alt,
        61 => Key::AltGr,
        51 => Key::Backspace,
        57 => Key::CapsLock,
        59 => Key::ControlLeft,
        62 => Key::ControlRight,
        125 => Key::DownArrow,
        53 => Key::Escape,
        122 => Key::F1,
        109 => Key::F10,
        103 => Key::F11,
        111 => Key::F12,
        120 => Key::F2,
        99 => Key::F3,
        118 => Key::F4,
        96 => Key::F5,
        97 => Key::F6,
        98 => Key::F7,
        100 => Key::F8,
        101 => Key::F9,
        123 => Key::LeftArrow,
        55 => Key::MetaLeft,
        54 => Key::MetaRight,
        36 => Key::Return,
        124 => Key::RightArrow,
        56 => Key::ShiftLeft,
        60 => Key::ShiftRight,
        49 => Key::Space,
        48 => Key::Tab,
        126 => Key::UpArrow,
        50 => Key::BackQuote,
        18 => Key::Num1,
        19 => Key::Num2,
        20 => Key::Num3,
        21 => Key::Num4,
        23 => Key::Num5,
        22 => Key::Num6,
        26 => Key::Num7,
        28 => Key::Num8,
        25 => Key::Num9,
        29 => Key::Num0,
        27 => Key::Minus,
        24 => Key::Equal,
        12 => Key::KeyQ,
        13 => Key::KeyW,
        14 => Key::KeyE,
        15 => Key::KeyR,
        17 => Key::KeyT,
        16 => Key::KeyY,
        32 => Key::KeyU,
        34 => Key::KeyI,
        31 => Key::KeyO,
        35 => Key::KeyP,
        33 => Key::LeftBracket,
        30 => Key::RightBracket,
        0 => Key::KeyA,
        1 => Key::KeyS,
        2 => Key::KeyD,
        3 => Key::KeyF,
        5 => Key::KeyG,
        4 => Key::KeyH,
        38 => Key::KeyJ,
        40 => Key::KeyK,
        37 => Key::KeyL,
        41 => Key::SemiColon,
        39 => Key::Quote,
        42 => Key::BackSlash,
        6 => Key::KeyZ,
        7 => Key::KeyX,
        8 => Key::KeyC,
        9 => Key::KeyV,
        11 => Key::KeyB,
        45 => Key::KeyN,
        46 => Key::KeyM,
        43 => Key::Comma,
        47 => Key::Dot,
        44 => Key::Slash,
        63 => Key::Function,
        other => Key::Unknown(other as u32),
    }
}
