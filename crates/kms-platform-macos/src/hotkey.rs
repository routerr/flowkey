use kms_input::hotkey::HotkeyBinding;

pub struct MacosHotkey {
    binding: HotkeyBinding,
}

impl MacosHotkey {
    pub fn parse(spec: &str) -> Result<Self, String> {
        Ok(Self {
            binding: HotkeyBinding::parse(spec)?,
        })
    }

    pub fn binding(&self) -> &HotkeyBinding {
        &self.binding
    }
}
