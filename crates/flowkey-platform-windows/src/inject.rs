use flowkey_input::event::InputEvent;
use flowkey_input::loopback::SharedLoopbackSuppressor;
use flowkey_input::InputEventSink;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use flowkey_input::native_injector::NativeInputSink;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct WindowsInjector {
    inner: NativeInputSink,
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct WindowsInjector;

impl WindowsInjector {
    pub fn new() -> Result<Self, String> {
        Self::with_loopback(None)
    }

    pub fn with_loopback(loopback: Option<SharedLoopbackSuppressor>) -> Result<Self, String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            Ok(Self {
                inner: NativeInputSink::with_loopback("windows", loopback)?,
            })
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Err("native Windows input injection is unavailable on this target".to_string())
        }
    }
}

impl InputEventSink for WindowsInjector {
    fn handle(&mut self, event: &InputEvent) -> Result<(), String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.inner.handle(event)
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = event;
            Err("native Windows input injection is unavailable on this target".to_string())
        }
    }

    fn release_all(&mut self) -> Result<(), String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.inner.release_all()
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Err("native Windows input injection is unavailable on this target".to_string())
        }
    }
}
