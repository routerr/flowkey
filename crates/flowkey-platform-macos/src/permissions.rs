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
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn CGPreflightListenEventAccess() -> u8;
}

#[cfg(target_os = "macos")]
unsafe fn ax_is_process_trusted() -> bool {
    AXIsProcessTrusted() != 0
}

#[cfg(target_os = "macos")]
unsafe fn cg_preflight_listen_event_access() -> bool {
    CGPreflightListenEventAccess() != 0
}
