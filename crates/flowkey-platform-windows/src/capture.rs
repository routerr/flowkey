use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use flowkey_input::capture::{CaptureSignal, CaptureState, InputCapture, LocalInputCapture};
use flowkey_input::event::InputEvent;
use flowkey_input::hotkey::{HotkeyBinding, HotkeyTracker};
use flowkey_input::loopback::SharedLoopbackSuppressor;
use tracing::{debug, info, warn};
use windows_sys::Win32::Foundation::{GetLastError, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT,
    VK_RWIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SetCursorPos, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, WHEEL_DELTA,
};

pub struct WindowsCapture {
    inner: LocalInputCapture,
    suppression_enabled: Arc<AtomicBool>,
}

pub struct WindowsExclusiveCapture {
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    receiver: Option<Receiver<CaptureSignal>>,
    suppression_enabled: Arc<AtomicBool>,
    started: bool,
    restart_count: Arc<AtomicU64>,
}

impl WindowsCapture {
    pub fn new(binding: HotkeyBinding) -> Self {
        Self::with_loopback(binding, None, Arc::new(AtomicBool::new(false)))
    }

    pub fn with_loopback(
        binding: HotkeyBinding,
        loopback: Option<SharedLoopbackSuppressor>,
        suppression_enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner: LocalInputCapture::with_loopback(binding, loopback),
            suppression_enabled,
        }
    }
}

impl InputCapture for WindowsCapture {
    fn start(&mut self) -> Result<(), String> {
        self.inner.start()
    }

    fn poll(&mut self) -> Option<CaptureSignal> {
        self.inner.poll()
    }

    fn wait(&mut self) -> Option<CaptureSignal> {
        self.inner.wait()
    }

    fn set_suppression_enabled(&mut self, enabled: bool) {
        self.suppression_enabled.store(enabled, Ordering::SeqCst);
        self.inner.set_suppression_enabled(enabled);
    }
}

impl WindowsExclusiveCapture {
    pub fn with_loopback(
        binding: HotkeyBinding,
        loopback: Option<SharedLoopbackSuppressor>,
        suppression_enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            binding,
            loopback,
            receiver: None,
            suppression_enabled,
            started: false,
            restart_count: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl InputCapture for WindowsExclusiveCapture {
    fn start(&mut self) -> Result<(), String> {
        if self.started {
            return Ok(());
        }

        let (sender, receiver) = mpsc::channel();
        let binding = self.binding.clone();
        let loopback = self.loopback.clone();
        let suppression_enabled = Arc::clone(&self.suppression_enabled);
        let restart_count = Arc::clone(&self.restart_count);
        let native_keyboard_events = Arc::new(AtomicU64::new(0));
        self.receiver = Some(receiver);
        self.started = true;

        thread::spawn(move || {
            spawn_rdev_keyboard_fallback(
                binding.clone(),
                loopback.clone(),
                sender.clone(),
                Arc::clone(&native_keyboard_events),
            );
            spawn_polling_keyboard_fallback(
                binding.clone(),
                loopback.clone(),
                sender.clone(),
                Arc::clone(&native_keyboard_events),
            );

            let backoff = [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(5),
                Duration::from_secs(10),
            ];
            let mut backoff_index = 0usize;

            loop {
                let result = spawn_grab_thread(
                    binding.clone(),
                    loopback.clone(),
                    Arc::clone(&suppression_enabled),
                    sender.clone(),
                    Arc::clone(&native_keyboard_events),
                )
                .join();

                match result {
                    Ok(()) => {}
                    Err(panic_info) => {
                        warn!(error = ?panic_info, "Windows exclusive capture (grab) panicked");
                    }
                }

                if sender.send(CaptureSignal::HotkeySuppressed).is_err() {
                    // Receiver dropped — the capture was stopped intentionally.
                    break;
                }

                restart_count.fetch_add(1, Ordering::SeqCst);
                let delay = backoff[backoff_index];
                if backoff_index + 1 < backoff.len() {
                    backoff_index += 1;
                }
                warn!(
                    restart = restart_count.load(Ordering::SeqCst),
                    "Windows exclusive capture restarting"
                );
                thread::sleep(delay);
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
        self.suppression_enabled.store(enabled, Ordering::SeqCst);
    }

    fn capture_restart_counter(&self) -> Option<Arc<AtomicU64>> {
        Some(Arc::clone(&self.restart_count))
    }
}

// Thread-local hook state — lives on the grab thread, so no cross-thread contention.
use std::cell::RefCell;
use std::sync::atomic::AtomicIsize;

// HHOOK = *mut c_void; store as isize for atomic access.
static NATIVE_KEYBOARD_HOOK: AtomicIsize = AtomicIsize::new(0);
static NATIVE_MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);

fn hook_to_isize(h: HHOOK) -> isize {
    h as isize
}
fn isize_to_hook(v: isize) -> HHOOK {
    v as *mut c_void
}

struct NativeGrabState {
    tracker: HotkeyTracker,
    capture_state: CaptureState,
    loopback: Option<SharedLoopbackSuppressor>,
    suppression_enabled: Arc<AtomicBool>,
    sender: mpsc::Sender<CaptureSignal>,
    pending_recenter: Option<(f64, f64)>,
    native_keyboard_events: Arc<AtomicU64>,
}

thread_local! {
    static GRAB_STATE: RefCell<Option<NativeGrabState>> = const { RefCell::new(None) };
}

unsafe extern "system" fn native_keyboard_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code == HC_ACTION as i32 {
        let suppress = GRAB_STATE
            .try_with(|cell| {
                cell.try_borrow_mut().ok().and_then(|mut guard| {
                    guard.as_mut().map(|s| handle_keyboard(s, wparam, lparam))
                })
            })
            .ok()
            .flatten()
            .unwrap_or(false);
        if suppress {
            return 1;
        }
    }
    let hook = isize_to_hook(NATIVE_KEYBOARD_HOOK.load(Ordering::SeqCst));
    CallNextHookEx(hook, code, wparam, lparam)
}

unsafe extern "system" fn native_mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let suppress = GRAB_STATE
            .try_with(|cell| {
                cell.try_borrow_mut()
                    .ok()
                    .and_then(|mut guard| guard.as_mut().map(|s| handle_mouse(s, wparam, lparam)))
            })
            .ok()
            .flatten()
            .unwrap_or(false);
        if suppress {
            return 1;
        }
    }
    let hook = isize_to_hook(NATIVE_MOUSE_HOOK.load(Ordering::SeqCst));
    CallNextHookEx(hook, code, wparam, lparam)
}

fn handle_keyboard(state: &mut NativeGrabState, wparam: WPARAM, lparam: LPARAM) -> bool {
    let msg_id = wparam as u32;
    if !matches!(msg_id, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP) {
        return false;
    }

    let pressed = matches!(msg_id, WM_KEYDOWN | WM_SYSKEYDOWN);
    let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    let vk = kb.vkCode as u16;
    state.native_keyboard_events.fetch_add(1, Ordering::SeqCst);

    let rdev_key = rdev_key_from_vk(vk);

    debug!(
        target: "keyboard_trace",
        platform = "windows",
        wparam = msg_id,
        vk_code = vk,
        rdev_key = ?rdev_key,
        scan_code = kb.scanCode,
        flags = kb.flags,
        pressed,
        "raw keyboard callback received"
    );
    crate::debug::emit(
        "raw-keyboard",
        format!(
            "vk={vk} scan={} flags={} pressed={pressed} mapped={rdev_key:?}",
            kb.scanCode, kb.flags
        ),
    );
    let timestamp_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    sync_current_modifier_state(&mut state.capture_state);

    let input_event = state
        .capture_state
        .translate_key_event(rdev_key, pressed, timestamp_us);

    let Some(input) = input_event else {
        // normalize_key_code returned None — unknown key, let it pass through
        debug!(
            target: "keyboard_trace",
            platform = "windows",
            vk_code = vk,
            pressed,
            "keyboard event has no protocol mapping, passing through"
        );
        crate::debug::emit(
            "keyboard-unmapped",
            format!("vk={vk} pressed={pressed}; passing through locally"),
        );
        return false;
    };

    // Check loopback suppressor (event was injected by us, skip forwarding)
    if let Some(loopback) = &state.loopback {
        if let Ok(mut lb) = loopback.lock() {
            if lb.should_suppress(&input) {
                crate::debug::emit("keyboard-loopback", format!("{input:?}"));
                return false; // pass through; injection will take effect
            }
        }
    }

    let suppress_local = state.suppression_enabled.load(Ordering::SeqCst);

    match state.tracker.process(&input) {
        flowkey_input::hotkey::HotkeyOutcome::Pressed => {
            let _ = state.sender.send(CaptureSignal::HotkeyPressed);
            crate::debug::emit("hotkey-pressed", format!("{input:?}"));
            return true; // suppress hotkey activation to prevent leakage to foreground app
        }
        flowkey_input::hotkey::HotkeyOutcome::Suppressed => {
            crate::debug::emit("hotkey-suppressed", format!("{input:?}"));
            return true; // suppress chord release sequence
        }
        flowkey_input::hotkey::HotkeyOutcome::Forward => {}
    }

    if let InputEvent::KeyDown {
        ref code,
        ref modifiers,
        timestamp_us,
    }
    | InputEvent::KeyUp {
        ref code,
        ref modifiers,
        timestamp_us,
    } = input
    {
        debug!(
            target: "keyboard_trace",
            platform = "windows",
            code = %code,
            pressed,
            shift = modifiers.shift,
            control = modifiers.control,
            alt = modifiers.alt,
            meta = modifiers.meta,
            timestamp_us,
            suppress_local,
            "forwarding keyboard event from Windows capture"
        );
        crate::debug::emit(
            "keyboard-forward",
            format!("{input:?}; suppress_local={suppress_local}"),
        );
    }

    let _ = state.sender.send(CaptureSignal::Input(input));
    suppress_local
}

fn sync_current_modifier_state(capture_state: &mut CaptureState) {
    let (shift_l, shift_r, ctrl_l, ctrl_r, alt_l, alt_r, meta_l, meta_r) = unsafe {
        (
            key_is_down(VK_LSHIFT),
            key_is_down(VK_RSHIFT),
            key_is_down(VK_LCONTROL),
            key_is_down(VK_RCONTROL),
            key_is_down(VK_LMENU),
            key_is_down(VK_RMENU),
            key_is_down(VK_LWIN),
            key_is_down(VK_RWIN),
        )
    };

    capture_state.sync_physical_modifiers(
        shift_l, shift_r, ctrl_l, ctrl_r, alt_l, alt_r, meta_l, meta_r,
    );
}

unsafe fn key_is_down(vk: u16) -> bool {
    GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0
}

fn handle_mouse(state: &mut NativeGrabState, wparam: WPARAM, lparam: LPARAM) -> bool {
    let mouse = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
    let timestamp_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    use flowkey_input::event::MouseButton;
    use flowkey_input::normalize::normalize_wheel_delta;

    let modifiers = state.capture_state.modifiers;

    let input_event = match wparam as u32 {
        WM_MOUSEMOVE => {
            let x = mouse.pt.x as f64;
            let y = mouse.pt.y as f64;
            let last = state.capture_state.last_mouse_position;
            state.capture_state.last_mouse_position = Some((x, y));

            // Consume synthetic recentering move
            if let Some(target) = state.pending_recenter {
                if (x - target.0).abs() <= 1.0 && (y - target.1).abs() <= 1.0 {
                    state.pending_recenter = None;
                    return false; // discard, don't forward
                }
            }

            if let Some((lx, ly)) = last {
                let dx = (x - lx).round() as i32;
                let dy = (y - ly).round() as i32;
                if dx == 0 && dy == 0 {
                    return false;
                }
                Some(InputEvent::MouseMove {
                    dx,
                    dy,
                    modifiers,
                    timestamp_us,
                })
            } else {
                None
            }
        }
        WM_LBUTTONDOWN => Some(InputEvent::MouseButtonDown {
            button: MouseButton::Left,
            modifiers,
            timestamp_us,
        }),
        WM_LBUTTONUP => Some(InputEvent::MouseButtonUp {
            button: MouseButton::Left,
            modifiers,
            timestamp_us,
        }),
        WM_RBUTTONDOWN => Some(InputEvent::MouseButtonDown {
            button: MouseButton::Right,
            modifiers,
            timestamp_us,
        }),
        WM_RBUTTONUP => Some(InputEvent::MouseButtonUp {
            button: MouseButton::Right,
            modifiers,
            timestamp_us,
        }),
        WM_MBUTTONDOWN => Some(InputEvent::MouseButtonDown {
            button: MouseButton::Middle,
            modifiers,
            timestamp_us,
        }),
        WM_MBUTTONUP => Some(InputEvent::MouseButtonUp {
            button: MouseButton::Middle,
            modifiers,
            timestamp_us,
        }),
        WM_MOUSEWHEEL => {
            let raw_delta = (((mouse.mouseData >> 16) & 0xFFFF) as i16) as i32;
            let ticks = raw_delta / WHEEL_DELTA as i32;
            if ticks == 0 {
                None
            } else {
                normalize_wheel_delta(0.0, ticks as f64).map(|(dx, dy)| InputEvent::MouseWheel {
                    delta_x: dx,
                    delta_y: dy,
                    modifiers,
                    timestamp_us,
                })
            }
        }
        _ => None,
    };

    let Some(input) = input_event else {
        return false;
    };

    // Loopback check
    if let Some(loopback) = &state.loopback {
        if let Ok(mut lb) = loopback.lock() {
            if lb.should_suppress(&input) {
                return false;
            }
        }
    }

    let suppress_local = state.suppression_enabled.load(Ordering::SeqCst);

    if suppress_local {
        if let InputEvent::MouseMove { dx, dy, .. } = &input {
            let saved = state.capture_state.last_mouse_position;
            if let Some(center) = recenter_cursor_to_virtual_center() {
                debug!(
                    dx,
                    dy,
                    center_x = center.0,
                    center_y = center.1,
                    "recentering suppressed Windows cursor"
                );
                state.capture_state.last_mouse_position = Some(center);
                state.pending_recenter = Some(center);
            } else {
                state.capture_state.last_mouse_position = saved;
            }
        }
    }

    let _ = state.sender.send(CaptureSignal::Input(input));
    suppress_local
}

/// Map Windows virtual key code to rdev::Key (mirrors rdev's key_from_code).
fn rdev_key_from_vk(vk: u16) -> rdev::Key {
    use rdev::Key;
    match vk {
        65 => Key::KeyA,
        66 => Key::KeyB,
        67 => Key::KeyC,
        68 => Key::KeyD,
        69 => Key::KeyE,
        70 => Key::KeyF,
        71 => Key::KeyG,
        72 => Key::KeyH,
        73 => Key::KeyI,
        74 => Key::KeyJ,
        75 => Key::KeyK,
        76 => Key::KeyL,
        77 => Key::KeyM,
        78 => Key::KeyN,
        79 => Key::KeyO,
        80 => Key::KeyP,
        81 => Key::KeyQ,
        82 => Key::KeyR,
        83 => Key::KeyS,
        84 => Key::KeyT,
        85 => Key::KeyU,
        86 => Key::KeyV,
        87 => Key::KeyW,
        88 => Key::KeyX,
        89 => Key::KeyY,
        90 => Key::KeyZ,
        48 => Key::Num0,
        49 => Key::Num1,
        50 => Key::Num2,
        51 => Key::Num3,
        52 => Key::Num4,
        53 => Key::Num5,
        54 => Key::Num6,
        55 => Key::Num7,
        56 => Key::Num8,
        57 => Key::Num9,
        // Function keys
        112 => Key::F1,
        113 => Key::F2,
        114 => Key::F3,
        115 => Key::F4,
        116 => Key::F5,
        117 => Key::F6,
        118 => Key::F7,
        119 => Key::F8,
        120 => Key::F9,
        121 => Key::F10,
        122 => Key::F11,
        123 => Key::F12,
        // Navigation
        37 => Key::LeftArrow,
        38 => Key::UpArrow,
        39 => Key::RightArrow,
        40 => Key::DownArrow,
        36 => Key::Home,
        35 => Key::End,
        33 => Key::PageUp,
        34 => Key::PageDown,
        45 => Key::Insert,
        46 => Key::Delete,
        // Special
        8 => Key::Backspace,
        9 => Key::Tab,
        13 => Key::Return,
        27 => Key::Escape,
        32 => Key::Space,
        // Modifiers
        160 => Key::ShiftLeft,
        161 => Key::ShiftRight,
        162 => Key::ControlLeft,
        163 => Key::ControlRight,
        164 => Key::Alt,
        165 => Key::AltGr,
        16 => Key::ShiftLeft,
        17 => Key::ControlLeft,
        18 => Key::Alt,
        91 => Key::MetaLeft,
        92 => Key::MetaRight,
        // Locks
        20 => Key::CapsLock,
        144 => Key::NumLock,
        145 => Key::ScrollLock,
        // Numpad
        96 => Key::Kp0,
        97 => Key::Kp1,
        98 => Key::Kp2,
        99 => Key::Kp3,
        100 => Key::Kp4,
        101 => Key::Kp5,
        102 => Key::Kp6,
        103 => Key::Kp7,
        104 => Key::Kp8,
        105 => Key::Kp9,
        110 => Key::KpDelete,
        107 => Key::KpPlus,
        109 => Key::KpMinus,
        106 => Key::KpMultiply,
        111 => Key::KpDivide,
        // Punctuation
        192 => Key::BackQuote,
        189 => Key::Minus,
        187 => Key::Equal,
        219 => Key::LeftBracket,
        221 => Key::RightBracket,
        220 => Key::BackSlash,
        186 => Key::SemiColon,
        222 => Key::Quote,
        188 => Key::Comma,
        190 => Key::Dot,
        191 => Key::Slash,
        44 => Key::PrintScreen,
        19 => Key::Pause,
        _ => Key::Unknown(vk as u32),
    }
}

fn spawn_grab_thread(
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    suppression_enabled: Arc<AtomicBool>,
    sender: mpsc::Sender<CaptureSignal>,
    native_keyboard_events: Arc<AtomicU64>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let hmod = unsafe { GetModuleHandleW(null_mut()) };

        let kb_hook: HHOOK =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(native_keyboard_proc), hmod, 0) };
        if kb_hook.is_null() {
            let err = unsafe { GetLastError() };
            warn!(error_code = err, "failed to install WH_KEYBOARD_LL hook");
            return;
        }
        NATIVE_KEYBOARD_HOOK.store(hook_to_isize(kb_hook), Ordering::SeqCst);
        info!("WH_KEYBOARD_LL hook installed");

        let ms_hook: HHOOK =
            unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(native_mouse_proc), hmod, 0) };
        if ms_hook.is_null() {
            let err = unsafe { GetLastError() };
            warn!(error_code = err, "failed to install WH_MOUSE_LL hook");
            unsafe { UnhookWindowsHookEx(kb_hook) };
            NATIVE_KEYBOARD_HOOK.store(0, Ordering::SeqCst);
            return;
        }
        NATIVE_MOUSE_HOOK.store(hook_to_isize(ms_hook), Ordering::SeqCst);
        info!("WH_MOUSE_LL hook installed");

        GRAB_STATE.with(|cell| {
            let mut capture_state = CaptureState::default();

            // Sync initial modifier state from OS
            let shift_l;
            let shift_r;
            let ctrl_l;
            let ctrl_r;
            let alt_l;
            let alt_r;
            let meta_l;
            let meta_r;

            unsafe {
                shift_l = key_is_down(VK_LSHIFT);
                shift_r = key_is_down(VK_RSHIFT);
                ctrl_l = key_is_down(VK_LCONTROL);
                ctrl_r = key_is_down(VK_RCONTROL);
                alt_l = key_is_down(VK_LMENU);
                alt_r = key_is_down(VK_RMENU);
                meta_l = key_is_down(VK_LWIN);
                meta_r = key_is_down(VK_RWIN);
            }
            capture_state.sync_physical_modifiers(
                shift_l, shift_r, ctrl_l, ctrl_r, alt_l, alt_r, meta_l, meta_r,
            );

            *cell.borrow_mut() = Some(NativeGrabState {
                tracker: HotkeyTracker::new(binding),
                capture_state,
                loopback,
                suppression_enabled,
                sender,
                pending_recenter: None,
                native_keyboard_events,
            });
        });

        let mut msg = MSG {
            hwnd: null_mut(),
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
        };
        loop {
            let ret = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
            match ret {
                0 | -1 => break,
                _ => unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                    windows_sys::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
                },
            }
        }

        GRAB_STATE.with(|cell| drop(cell.borrow_mut().take()));
        unsafe {
            UnhookWindowsHookEx(kb_hook);
            UnhookWindowsHookEx(ms_hook);
        };
        NATIVE_KEYBOARD_HOOK.store(0, Ordering::SeqCst);
        NATIVE_MOUSE_HOOK.store(0, Ordering::SeqCst);
    })
}

fn spawn_polling_keyboard_fallback(
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    sender: mpsc::Sender<CaptureSignal>,
    native_keyboard_events: Arc<AtomicU64>,
) {
    thread::spawn(move || {
        const KEYS: &[(u16, &str)] = &[
            (VK_LSHIFT, "ShiftLeft"),
            (VK_RSHIFT, "ShiftRight"),
            (VK_LCONTROL, "ControlLeft"),
            (VK_RCONTROL, "ControlRight"),
            (VK_LMENU, "AltLeft"),
            (VK_RMENU, "AltRight"),
            (VK_LWIN, "MetaLeft"),
            (VK_RWIN, "MetaRight"),
            (0x08, "Backspace"),
            (0x09, "Tab"),
            (0x0D, "Enter"),
            (0x1B, "Escape"),
            (0x20, "Space"),
            (0x25, "ArrowLeft"),
            (0x26, "ArrowUp"),
            (0x27, "ArrowRight"),
            (0x28, "ArrowDown"),
            (0x30, "Digit0"),
            (0x31, "Digit1"),
            (0x32, "Digit2"),
            (0x33, "Digit3"),
            (0x34, "Digit4"),
            (0x35, "Digit5"),
            (0x36, "Digit6"),
            (0x37, "Digit7"),
            (0x38, "Digit8"),
            (0x39, "Digit9"),
            (0x41, "KeyA"),
            (0x42, "KeyB"),
            (0x43, "KeyC"),
            (0x44, "KeyD"),
            (0x45, "KeyE"),
            (0x46, "KeyF"),
            (0x47, "KeyG"),
            (0x48, "KeyH"),
            (0x49, "KeyI"),
            (0x4A, "KeyJ"),
            (0x4B, "KeyK"),
            (0x4C, "KeyL"),
            (0x4D, "KeyM"),
            (0x4E, "KeyN"),
            (0x4F, "KeyO"),
            (0x50, "KeyP"),
            (0x51, "KeyQ"),
            (0x52, "KeyR"),
            (0x53, "KeyS"),
            (0x54, "KeyT"),
            (0x55, "KeyU"),
            (0x56, "KeyV"),
            (0x57, "KeyW"),
            (0x58, "KeyX"),
            (0x59, "KeyY"),
            (0x5A, "KeyZ"),
            (0xBA, "Semicolon"),
            (0xBB, "Equal"),
            (0xBC, "Comma"),
            (0xBD, "Minus"),
            (0xBE, "Dot"),
            (0xBF, "Slash"),
            (0xC0, "BackQuote"),
            (0xDB, "LeftBracket"),
            (0xDC, "BackSlash"),
            (0xDD, "RightBracket"),
            (0xDE, "Quote"),
        ];
        let mut previous = [false; KEYS.len()];
        let mut tracker = HotkeyTracker::new(binding);
        let mut capture_state = CaptureState::default();

        info!("Windows GetAsyncKeyState keyboard fallback starting");
        loop {
            if native_keyboard_events.load(Ordering::SeqCst) > 0 {
                thread::sleep(Duration::from_millis(25));
                continue;
            }

            for (index, (vk, label)) in KEYS.iter().enumerate() {
                let pressed = unsafe { key_is_down(*vk) };
                if pressed != previous[index] {
                    previous[index] = pressed;
                    crate::debug::emit("key-state-probe", format!("{label} pressed={pressed}"));
                    debug!(
                        target: "keyboard_trace",
                        platform = "windows",
                        vk_code = *vk,
                        key = *label,
                        pressed,
                        "GetAsyncKeyState keyboard probe changed"
                    );

                    sync_current_modifier_state(&mut capture_state);
                    let timestamp_us = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64;
                    let rdev_key = rdev_key_from_vk(*vk);

                    let Some(input) =
                        capture_state.translate_key_event(rdev_key, pressed, timestamp_us)
                    else {
                        crate::debug::emit(
                            "poll-keyboard-unmapped",
                            format!("{label} vk={vk} pressed={pressed}"),
                        );
                        continue;
                    };

                    if let Some(loopback) = &loopback {
                        if let Ok(mut lb) = loopback.lock() {
                            if lb.should_suppress(&input) {
                                crate::debug::emit("poll-keyboard-loopback", format!("{input:?}"));
                                continue;
                            }
                        }
                    }

                    match tracker.process(&input) {
                        flowkey_input::hotkey::HotkeyOutcome::Pressed => {
                            crate::debug::emit("poll-hotkey-pressed", format!("{input:?}"));
                            let _ = sender.send(CaptureSignal::HotkeyPressed);
                            continue;
                        }
                        flowkey_input::hotkey::HotkeyOutcome::Suppressed => {
                            crate::debug::emit("poll-hotkey-suppressed", format!("{input:?}"));
                            continue;
                        }
                        flowkey_input::hotkey::HotkeyOutcome::Forward => {}
                    }

                    crate::debug::emit("poll-keyboard-forward", format!("{input:?}"));
                    debug!(
                        target: "keyboard_trace",
                        platform = "windows",
                        vk_code = *vk,
                        key = *label,
                        pressed,
                        input = ?input,
                        "forwarding keyboard event from Windows polling fallback"
                    );
                    let _ = sender.send(CaptureSignal::Input(input));
                }
            }

            thread::sleep(Duration::from_millis(10));
        }
    });
}

fn spawn_rdev_keyboard_fallback(
    binding: HotkeyBinding,
    loopback: Option<SharedLoopbackSuppressor>,
    sender: mpsc::Sender<CaptureSignal>,
    native_keyboard_events: Arc<AtomicU64>,
) {
    thread::spawn(move || {
        let mut tracker = HotkeyTracker::new(binding);
        let mut capture_state = CaptureState::default();

        info!("Windows rdev keyboard fallback listener starting");
        let result = rdev::listen(move |event| {
            let is_keyboard = matches!(
                event.event_type,
                rdev::EventType::KeyPress(_) | rdev::EventType::KeyRelease(_)
            );
            if !is_keyboard {
                return;
            }

            if native_keyboard_events.load(Ordering::SeqCst) > 0 {
                return;
            }

            crate::debug::emit("rdev-keyboard-raw", format!("{:?}", event.event_type));

            if let Some(signal) = capture_state.translate(event, &mut tracker, loopback.as_ref()) {
                match &signal {
                    CaptureSignal::Input(input) => {
                        crate::debug::emit("rdev-keyboard-forward", format!("{input:?}"));
                    }
                    CaptureSignal::HotkeyPressed => {
                        crate::debug::emit("rdev-hotkey-pressed", "fallback hotkey pressed");
                    }
                    CaptureSignal::HotkeySuppressed => {
                        crate::debug::emit("rdev-hotkey-suppressed", "fallback hotkey suppressed");
                    }
                }
                let _ = sender.send(signal);
            }
        });

        if let Err(error) = result {
            warn!(error = ?error, "Windows rdev keyboard fallback listener stopped");
        }
    });
}

fn recenter_cursor_to_virtual_center() -> Option<(f64, f64)> {
    let origin_x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let origin_y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

    if width <= 0 || height <= 0 {
        return None;
    }

    let center_x = origin_x + (width / 2);
    let center_y = origin_y + (height / 2);
    let success = unsafe { SetCursorPos(center_x, center_y) };
    if success == 0 {
        None
    } else {
        Some((f64::from(center_x), f64::from(center_y)))
    }
}

#[cfg(test)]
mod tests {
    use super::rdev_key_from_vk;
    use rdev::Key;

    #[test]
    fn rdev_key_from_vk_maps_generic_modifiers() {
        assert_eq!(rdev_key_from_vk(16), Key::ShiftLeft);
        assert_eq!(rdev_key_from_vk(17), Key::ControlLeft);
        assert_eq!(rdev_key_from_vk(18), Key::Alt);
    }

    #[test]
    fn rdev_key_from_vk_maps_sided_modifiers() {
        assert_eq!(rdev_key_from_vk(160), Key::ShiftLeft);
        assert_eq!(rdev_key_from_vk(161), Key::ShiftRight);
        assert_eq!(rdev_key_from_vk(162), Key::ControlLeft);
        assert_eq!(rdev_key_from_vk(163), Key::ControlRight);
    }
}
