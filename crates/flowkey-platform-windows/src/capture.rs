use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use flowkey_input::capture::{CaptureSignal, InputCapture, LocalInputCapture};
use flowkey_input::hotkey::HotkeyBinding;
use flowkey_input::loopback::SharedLoopbackSuppressor;

pub struct WindowsCapture {
    inner: LocalInputCapture,
    suppression_enabled: Arc<AtomicBool>,
}

pub struct WindowsExclusiveCapture {
    inner: LocalInputCapture,
    suppression_enabled: Arc<AtomicBool>,
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
            inner: LocalInputCapture::with_loopback(binding, loopback),
            suppression_enabled,
        }
    }
}

impl InputCapture for WindowsExclusiveCapture {
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
