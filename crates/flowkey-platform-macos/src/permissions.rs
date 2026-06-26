#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionStatus {
    pub accessibility: bool,
    pub input_monitoring: bool,
}

impl PermissionStatus {
    pub fn probe() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                accessibility: unsafe { ax_is_process_trusted() },
                input_monitoring: unsafe { cg_preflight_listen_event_access() },
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            Self {
                accessibility: false,
                input_monitoring: false,
            }
        }
    }

    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();

        if self.accessibility {
            notes.push("macOS Accessibility permission is granted".to_string());
        } else {
            notes.push("macOS requires Accessibility permission for input control; enable it in System Settings > Privacy & Security > Accessibility".to_string());
        }

        if self.input_monitoring {
            notes.push("macOS Input Monitoring permission is granted".to_string());
        } else {
            notes.push("macOS requires Input Monitoring permission for global capture; enable it in System Settings > Privacy & Security > Input Monitoring".to_string());
        }

        notes
    }

    /// Trigger the native macOS permission prompts for any access not yet
    /// granted, then re-probe and return the resulting status. The prompts show
    /// a system dialog with a direct "Open System Settings" button, which is
    /// far easier than asking the user to navigate Settings manually.
    ///
    /// macOS only re-shows a prompt while the permission is undecided; if the
    /// user previously denied it, the prompt is suppressed and they must use the
    /// Settings panes instead (see [`Self::open_accessibility_pane`]).
    pub fn request() -> Self {
        #[cfg(target_os = "macos")]
        {
            let accessibility = unsafe { ax_request_with_prompt() };
            let input_monitoring = unsafe { cg_request_listen_event_access() };
            Self {
                accessibility,
                input_monitoring,
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            Self {
                accessibility: true,
                input_monitoring: true,
            }
        }
    }

    #[cfg(target_os = "macos")]
    pub fn open_accessibility_pane() -> Result<(), String> {
        open_system_settings_pane("Privacy_Accessibility")
    }

    #[cfg(target_os = "macos")]
    pub fn open_input_monitoring_pane() -> Result<(), String> {
        open_system_settings_pane("Privacy_ListenEvent")
    }

    #[cfg(not(target_os = "macos"))]
    pub fn open_accessibility_pane() -> Result<(), String> {
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn open_input_monitoring_pane() -> Result<(), String> {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    // Like AXIsProcessTrusted, but shows the system Accessibility prompt (with a
    // direct "Open System Settings" button) when the options dict requests it.
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> u8;
    fn CGPreflightListenEventAccess() -> u8;
    // Triggers the system Input Monitoring prompt if access is still undecided.
    fn CGRequestListenEventAccess() -> u8;
}

#[cfg(target_os = "macos")]
unsafe fn ax_is_process_trusted() -> bool {
    AXIsProcessTrusted() != 0
}

#[cfg(target_os = "macos")]
unsafe fn cg_preflight_listen_event_access() -> bool {
    CGPreflightListenEventAccess() != 0
}

/// Prompt for Accessibility access, showing the native system dialog when the
/// permission is still undecided. Returns the trust state at call time.
#[cfg(target_os = "macos")]
unsafe fn ax_request_with_prompt() -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    // Literal value of Apple's `kAXTrustedCheckOptionPrompt` constant.
    let key = CFString::from_static_string("AXTrustedCheckOptionPrompt");
    let value = CFBoolean::true_value();
    let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
    AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef() as *const std::ffi::c_void) != 0
}

#[cfg(target_os = "macos")]
unsafe fn cg_request_listen_event_access() -> bool {
    CGRequestListenEventAccess() != 0
}

#[cfg(target_os = "macos")]
fn open_system_settings_pane(anchor: &str) -> Result<(), String> {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
    let status = std::process::Command::new("open")
        .arg(&url)
        .status()
        .map_err(|error| format!("failed to open System Settings: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("open exited with status {status}"))
    }
}
