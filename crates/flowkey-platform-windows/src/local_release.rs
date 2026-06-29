//! Release locally-stuck modifier keys when the controller loses the session.
//!
//! Taking control via the activation hotkey (e.g. Ctrl+Alt+Shift+K) lets the
//! modifier key-DOWN events reach the OS before suppression engages; the
//! matching key-UPs are then suppressed, so Windows is left believing those
//! modifiers are still held. A normal switch-back clears this (the second
//! chord's key-ups pass through once suppression is off), but an abrupt
//! disconnect — e.g. the remote peer crashing — does not, leaving every later
//! keystroke interpreted as Ctrl+Alt+Shift+<key> so the keyboard appears dead
//! while the mouse still works.
//!
//! Synthesizing the missing key-ups returns full local control no matter how the
//! session ended.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT,
    VK_RWIN,
};

/// Every modifier virtual-key, with whether it is an "extended" key (right-hand
/// Ctrl/Alt and the Win keys). Releasing a key that is not held is a harmless
/// no-op, but we still gate on `GetAsyncKeyState` to avoid emitting spurious
/// events.
const MODIFIERS: &[(u16, bool)] = &[
    (VK_LSHIFT, false),
    (VK_RSHIFT, false),
    (VK_LCONTROL, false),
    (VK_RCONTROL, true),
    (VK_LMENU, false),
    (VK_RMENU, true),
    (VK_LWIN, true),
    (VK_RWIN, true),
];

/// Synthesize key-up events for any modifier key the OS currently believes is
/// held, clearing modifiers left stuck after an abrupt loss of control. Safe to
/// call at any time; it is a no-op when nothing is stuck.
pub fn release_held_modifiers() {
    let mut released = Vec::new();
    for &(vk, extended) in MODIFIERS {
        // GetAsyncKeyState reflects the real OS key state; a suppressed key-up
        // leaves the corresponding key reading as "down" here.
        let down = unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 };
        if !down {
            continue;
        }

        let mut flags = KEYEVENTF_KEYUP;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        // SAFETY: `input` is a fully-initialized INPUT of INPUT_KEYBOARD kind.
        unsafe {
            SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
        }
        released.push(vk);
    }

    if !released.is_empty() {
        tracing::info!(
            ?released,
            "released stuck local modifier keys after losing control"
        );
    }
}
