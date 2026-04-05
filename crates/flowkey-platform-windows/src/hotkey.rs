use flowkey_input::hotkey::HotkeyBinding;

pub struct WindowsHotkey {
    binding: HotkeyBinding,
}

impl WindowsHotkey {
    pub fn parse(spec: &str) -> Result<Self, String> {
        Ok(Self {
            binding: HotkeyBinding::parse(spec)?,
        })
    }

    pub fn binding(&self) -> &HotkeyBinding {
        &self.binding
    }
}
