//! Process memory housekeeping helpers.

use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};

/// Ask Windows to trim this process's working set, returning freed pages to the
/// OS so reported memory drops promptly.
///
/// Call this right after a large allocation is released — e.g. once the manager
/// window (and its WebView) has been destroyed while the app stays in the tray.
/// The WebView2 host runs in separate processes that are reclaimed on their own
/// when the window is destroyed; this only trims our main process's own heap,
/// but it makes the working set shrink immediately instead of lingering.
///
/// Passing the magic `(usize::MAX, usize::MAX)` min/max is the documented way to
/// request a temporary trim (the OS grows the set back on demand).
pub fn trim_working_set() {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing,
    // and SetProcessWorkingSetSize only adjusts our own working-set limits.
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}
