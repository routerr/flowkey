use kms_input::capture::{CaptureSignal, InputCapture, LocalInputCapture};
use kms_input::hotkey::HotkeyBinding;
use kms_input::loopback::SharedLoopbackSuppressor;

pub struct WindowsCapture {
    inner: LocalInputCapture,
}

impl WindowsCapture {
    pub fn new(binding: HotkeyBinding) -> Self {
        Self::with_loopback(binding, None)
    }

    pub fn with_loopback(
        binding: HotkeyBinding,
        loopback: Option<SharedLoopbackSuppressor>,
    ) -> Self {
        Self {
            inner: LocalInputCapture::with_loopback(binding, loopback),
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
}
