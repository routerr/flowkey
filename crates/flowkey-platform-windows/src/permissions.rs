#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionStatus {
    pub user_session: bool,
}

impl PermissionStatus {
    pub fn probe() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self {
                user_session: is_interactive_console_session(),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            Self {
                user_session: false,
            }
        }
    }

    pub fn notes(&self) -> Vec<String> {
        if self.user_session {
            vec!["Windows is running in the active console session".to_string()]
        } else {
            vec![
                "Windows requires an interactive user session for input capture and injection"
                    .to_string(),
            ]
        }
    }
}

#[cfg(target_os = "windows")]
const SM_REMOTESESSION: i32 = 0x1000;

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn ProcessIdToSessionId(dwProcessId: u32, pSessionId: *mut u32) -> i32;
    fn WTSGetActiveConsoleSessionId() -> u32;
}

#[cfg(target_os = "windows")]
#[link(name = "user32")]
extern "system" {
    fn GetSystemMetrics(nIndex: i32) -> i32;
}

#[cfg(target_os = "windows")]
fn is_interactive_console_session() -> bool {
    unsafe {
        if GetSystemMetrics(SM_REMOTESESSION) != 0 {
            return false;
        }

        let mut current_session_id = 0u32;
        if ProcessIdToSessionId(GetCurrentProcessId(), &mut current_session_id as *mut u32) == 0 {
            return false;
        }

        current_session_id == WTSGetActiveConsoleSessionId()
    }
}
