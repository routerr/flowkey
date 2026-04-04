use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::daemon::{DaemonRuntime, DaemonState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub state: String,
    pub active_peer_id: Option<String>,
    pub session_healthy: bool,
    #[serde(default)]
    pub local_capture_enabled: bool,
    #[serde(default = "default_input_injection_backend")]
    pub input_injection_backend: String,
    #[serde(default)]
    pub notes: Vec<String>,
}

fn default_input_injection_backend() -> String {
    "unconfigured".to_string()
}

impl DaemonStatus {
    pub fn from_runtime(runtime: &DaemonRuntime) -> Self {
        let (state, active_peer_id) = match &runtime.state {
            DaemonState::Disconnected => ("disconnected".to_string(), None),
            DaemonState::ConnectedIdle => {
                ("connected-idle".to_string(), runtime.active_peer_id.clone())
            }
            DaemonState::Controlling { peer_id } => {
                ("controlling".to_string(), Some(peer_id.clone()))
            }
            DaemonState::ControlledBy { peer_id } => {
                ("controlled-by".to_string(), Some(peer_id.clone()))
            }
            DaemonState::Recovering => ("recovering".to_string(), runtime.active_peer_id.clone()),
        };

        let session_healthy = matches!(
            runtime.state,
            DaemonState::ConnectedIdle
                | DaemonState::Controlling { .. }
                | DaemonState::ControlledBy { .. }
        );

        Self {
            state,
            active_peer_id,
            session_healthy,
            local_capture_enabled: runtime.diagnostics.local_capture_enabled,
            input_injection_backend: runtime.diagnostics.input_injection_backend.clone(),
            notes: runtime.diagnostics.notes.clone(),
        }
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read daemon status from {}", path.display()))?;
        let status = toml::from_str::<Self>(&raw)
            .with_context(|| format!("failed to parse daemon status from {}", path.display()))?;

        Ok(status)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create daemon status directory {}",
                    parent.display()
                )
            })?;
        }

        let raw = toml::to_string_pretty(self).context("failed to serialize daemon status")?;
        fs::write(path, raw)
            .with_context(|| format!("failed to write daemon status to {}", path.display()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::daemon::DaemonRuntime;

    use super::DaemonStatus;

    #[test]
    fn from_runtime_marks_connected_state_and_peer() {
        let mut runtime = DaemonRuntime::new();
        runtime.mark_authenticated("office-pc");
        runtime.diagnostics.local_capture_enabled = true;
        runtime.diagnostics.input_injection_backend = "native".to_string();

        let status = DaemonStatus::from_runtime(&runtime);

        assert_eq!(status.state, "connected-idle");
        assert_eq!(status.active_peer_id.as_deref(), Some("office-pc"));
        assert!(status.session_healthy);
        assert!(status.local_capture_enabled);
        assert_eq!(status.input_injection_backend, "native");
    }

    #[test]
    fn from_runtime_marks_recovery_as_unhealthy() {
        let mut runtime = DaemonRuntime::new();
        runtime.mark_authenticated("office-pc");
        runtime.mark_authenticated("spare-pc");
        runtime.toggle_controller().expect("should enter control");
        runtime.mark_disconnected("office-pc");

        let status = DaemonStatus::from_runtime(&runtime);

        assert_eq!(status.state, "recovering");
        assert_eq!(status.active_peer_id.as_deref(), Some("office-pc"));
        assert!(!status.session_healthy);
    }

    #[test]
    fn status_round_trips_through_toml() {
        let status = DaemonStatus {
            state: "controlling".to_string(),
            active_peer_id: Some("office-pc".to_string()),
            session_healthy: true,
            local_capture_enabled: true,
            input_injection_backend: "native".to_string(),
            notes: vec!["accessibility permission granted".to_string()],
        };
        let path = std::env::temp_dir().join(format!(
            "kms-status-test-{}-{}.toml",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));

        status
            .save_to_path(&path)
            .expect("status should save to temp path");
        let reloaded = DaemonStatus::load_from_path(&path).expect("status should reload");
        fs::remove_file(&path).expect("temp status should be removable");

        assert_eq!(reloaded, status);
    }
}
