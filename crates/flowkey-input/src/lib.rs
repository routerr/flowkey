pub mod capture;
pub mod event;
pub mod hotkey;
pub mod inject;
pub mod keycode;
pub mod loopback;
pub mod normalize;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod native_injector;

use event::InputEvent;

pub trait InputEventSink: Send {
    fn handle(&mut self, event: &InputEvent) -> Result<(), String>;
    fn release_all(&mut self) -> Result<(), String>;
}
