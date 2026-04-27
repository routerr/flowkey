use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
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
    fn CFRunLoopSourceInvalidate(source: CFRunLoopSourceRef);
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CFMachPortInvalidate(tap: CFMachPortRef);
    fn CFRunLoopRun();
    fn CFRelease(cf: *const c_void);
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
    restart_count: Arc<AtomicU64>,
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
            restart_count: Arc::new(AtomicU64::new(0)),
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
        let restart_count = Arc::clone(&self.restart_count);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        self.receiver = Some(receiver);

        thread::spawn(move || {
            let backoff = [1_u64, 2, 5, 10];
            let mut backoff_index = 0usize;
            let mut startup_tx = Some(startup_tx);

            loop {
                let exit = run_event_tap(
                    binding.clone(),
                    loopback.clone(),
                    Arc::clone(&suppression_enabled),
                    exclusive,
                    sender.clone(),
                    startup_tx.take(),
                );

                if exit.startup_failed {
                    return;
                }

                let restart = restart_count.fetch_add(1, Ordering::SeqCst) + 1;
                warn!(restart, error = %exit.error, "macOS capture listener exited; restarting");

                if sender.send(CaptureSignal::HotkeySuppressed).is_err() {
                    break;
                }

                let delay = backoff[backoff_index];
                if backoff_index + 1 < backoff.len() {
                    backoff_index += 1;
                }
                thread::sleep(std::time::Duration::from_secs(delay));
            }
        });

        match startup_rx.recv() {
            Ok(Ok(())) => {
                self.started = true;
                Ok(())
            }
            Ok(Err(error)) => {
                self.receiver = None;
                Err(error)
            }
            Err(_) => {
                self.receiver = None;
                Err("macOS event tap startup acknowledgment channel closed".to_string())
            }
        }
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

    fn capture_restart_counter(&self) -> Option<Arc<AtomicU64>> {
        Some(Arc::clone(&self.restart_count))
    }
}

struct TapExit {
    error: String,
    startup_failed: bool,
}

fn run_event_tap(
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    suppression_enabled: Arc<AtomicBool>,
    exclusive: bool,
    sender: mpsc::Sender<CaptureSignal>,
    mut startup_tx: Option<mpsc::SyncSender<Result<(), String>>>,
) -> TapExit {
    let mut context = Box::new(TapContext {
        sender,
        tracker: HotkeyTracker::new(binding),
        state: CaptureState::default(),
        loopback,
        suppression_enabled: Arc::clone(&suppression_enabled),
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
        let error = "macOS event tap creation failed".to_string();
        let startup_failed = startup_tx.is_some();
        if let Some(tx) = startup_tx.take() {
            let _ = tx.send(Err(error.clone()));
        }
        return TapExit {
            error,
            startup_failed,
        };
    }

    context.tap = tap;

    unsafe {
        let loop_source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        if loop_source.is_null() {
            let error = "macOS event tap runloop source creation failed".to_string();
            let startup_failed = startup_tx.is_some();
            if let Some(tx) = startup_tx.take() {
                let _ = tx.send(Err(error.clone()));
            }
            CFMachPortInvalidate(tap);
            CFRelease(tap.cast());
            return TapExit {
                error,
                startup_failed,
            };
        }

        CFRunLoopAddSource(CFRunLoopGetCurrent(), loop_source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        if exclusive && suppression_enabled.load(Ordering::SeqCst) {
            CGAssociateMouseAndMouseCursorPosition(false);
            context.cursor_decoupled = true;
        }
        if let Some(tx) = startup_tx.take() {
            let _ = tx.send(Ok(()));
        }
        CFRunLoopRun();

        if context.cursor_decoupled {
            CGAssociateMouseAndMouseCursorPosition(true);
        }
        CFRunLoopSourceInvalidate(loop_source);
        CFMachPortInvalidate(tap);
        CFRelease(loop_source.cast());
        CFRelease(tap.cast());
    }

    TapExit {
        error: "macOS event tap run loop exited unexpectedly".to_string(),
        startup_failed: false,
    }
}

struct TapContext {
    sender: mpsc::Sender<CaptureSignal>,
    tracker: HotkeyTracker,
    state: CaptureState,
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
            let modifiers = context.state.modifiers;
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

    let translated_events = convert_cg_event(event_type, &cg_event, &mut context.last_flags);
    if translated_events.is_empty() {
        return cg_event.as_ptr();
    };

    let mut final_ptr = cg_event.as_ptr();
    for translated_event in translated_events {
        match context.state.translate(
            translated_event.clone(),
            &mut context.tracker,
            context.loopback.as_ref(),
        ) {
            Some(CaptureSignal::HotkeyPressed) => {
                let _ = context.sender.send(CaptureSignal::HotkeyPressed);
                // Drop the hotkey key itself so it does not leak to local apps.
                final_ptr = std::ptr::null_mut();
            }
            Some(CaptureSignal::HotkeySuppressed) => {
                // Always deliver key release for hotkey modifiers locally,
                // otherwise stuck-key behavior.
            }
            Some(CaptureSignal::Input(input)) => {
                let _ = context.sender.send(CaptureSignal::Input(input));
                if suppress_active {
                    // Fully drop local input while controlling remote (explicit mode).
                    final_ptr = std::ptr::null_mut();
                }
            }
            None => {}
        }
    }
    final_ptr
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
) -> Vec<Event> {
    let now = SystemTime::now();
    let mut generated = Vec::new();

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
            generated.push(Event {
                time: now,
                name: None,
                event_type: EventType::MouseMove {
                    x: point.x,
                    y: point.y,
                },
            });
            return generated;
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
            let is_set = key_flag_is_set(code, current_flags);
            *last_flags = current_flags;
            
            // CapsLock (57) and Fn/Globe (63) only emit an event when their state toggles.
            // This means one physical press = one FlagsChanged event.
            // We simulate a full physical click (Press + Release) so the remote OS sees a complete key stroke.
            if code == 57 || code == 63 {
                generated.push(Event {
                    time: now,
                    name: None,
                    event_type: EventType::KeyPress(key),
                });
                generated.push(Event {
                    time: now,
                    name: None,
                    event_type: EventType::KeyRelease(key),
                });
                return generated;
            }

            if is_set {
                EventType::KeyPress(key)
            } else {
                EventType::KeyRelease(key)
            }
        }
        CGEventType::ScrollWheel => {
            let delta_y =
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
            let delta_x =
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
            generated.push(Event {
                time: now,
                name: None,
                event_type: EventType::Wheel { delta_x, delta_y },
            });
            return generated;
        }
        _ => return generated,
    };

    generated.push(Event {
        time: now,
        name: None,
        event_type,
    });
    generated
}

/// Returns `true` if the modifier bit corresponding to `code` is active in `flags`.
///
/// macOS emits one `FlagsChanged` event per modifier key transition and reports
/// the key code of the key that changed. By testing the specific bit that
/// belongs to that key we get correct press/release semantics regardless of
/// what other flag bits (e.g. `CGEventFlagNonCoalesced`) happen to be set.
fn key_flag_is_set(code: u16, flags: CGEventFlags) -> bool {
    let mask = match code {
        56 | 60 => CGEventFlags::CGEventFlagShift,       // ShiftLeft / ShiftRight
        58 | 61 => CGEventFlags::CGEventFlagAlternate,   // Option left / Option right
        59 | 62 => CGEventFlags::CGEventFlagControl,     // ControlLeft / ControlRight
        55 | 54 => CGEventFlags::CGEventFlagCommand,     // Command left / Command right
        57 => CGEventFlags::CGEventFlagAlphaShift,       // CapsLock
        63 => CGEventFlags::CGEventFlagSecondaryFn,      // Fn
        _ => return false, // Unknown modifier key; treat as release to avoid stuck keys
    };
    flags.contains(mask)
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
