use crate::event::InputEvent;

pub trait InputInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), String>;
    fn release_all(&mut self) -> Result<(), String>;
}
