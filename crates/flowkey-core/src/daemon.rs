use std::collections::HashMap;

use crate::session::Session;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Controlling,
    ControlledBy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    Disconnected,
    ConnectedIdle,
    Controlling { peer_id: String },
    ControlledBy { peer_id: String },
    Recovering { intended_role: Option<Role> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDiagnostics {
    pub local_capture_enabled: bool,
    pub input_injection_backend: String,
    pub notes: Vec<String>,
}

impl Default for RuntimeDiagnostics {
    fn default() -> Self {
        Self {
            local_capture_enabled: false,
            input_injection_backend: "unconfigured".to_string(),
            notes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DaemonRuntime {
    pub state: DaemonState,
    pub sessions: HashMap<String, Session>,
    pub active_peer_id: Option<String>,
    pub diagnostics: RuntimeDiagnostics,
}

impl DaemonRuntime {
    pub fn new() -> Self {
        Self {
            state: DaemonState::Disconnected,
            sessions: HashMap::new(),
            active_peer_id: None,
            diagnostics: RuntimeDiagnostics::default(),
        }
    }

    pub fn mark_authenticated(&mut self, peer_id: impl Into<String>) -> Option<Role> {
        let peer_id = peer_id.into();
        let resume_target = peer_id.clone();
        self.sessions
            .insert(peer_id.clone(), Session::authenticated(peer_id.clone()));
        if self.active_peer_id.is_none() {
            self.active_peer_id = Some(peer_id);
        }

        let mut resumed_role = None;
        if let DaemonState::Recovering { intended_role } = &self.state {
            if self
                .active_peer_id
                .as_deref()
                .is_some_and(|active| active == resume_target)
            {
                resumed_role = intended_role.clone();
                self.state = DaemonState::ConnectedIdle;
            }
        } else {
            self.state = DaemonState::ConnectedIdle;
        }

        resumed_role
    }

    pub fn mark_disconnected(&mut self, peer_id: &str) {
        let current_state = self.state.clone();
        let active_peer_removed = self.active_peer_id.as_deref() == Some(peer_id);

        self.sessions.remove(peer_id);
        if self.sessions.is_empty() {
            self.state = DaemonState::Disconnected;
            self.active_peer_id = None;
            return;
        }

        if active_peer_removed {
            let intended_role = match current_state {
                DaemonState::Controlling { .. } => Some(Role::Controlling),
                DaemonState::ControlledBy { .. } => Some(Role::ControlledBy),
                DaemonState::Recovering { intended_role } => intended_role,
                _ => None,
            };
            self.state = DaemonState::Recovering { intended_role };
            return;
        }

        if self.active_peer_id.is_none() {
            self.active_peer_id = self.sessions.keys().next().cloned();
        }
    }

    pub fn select_active_peer(&mut self, peer_id: impl Into<String>) -> Result<(), String> {
        let peer_id = peer_id.into();
        match self.sessions.get(&peer_id) {
            Some(session) if session.authenticated => {
                self.active_peer_id = Some(peer_id);
                self.state = match self.state {
                    DaemonState::Recovering { .. } => DaemonState::ConnectedIdle,
                    DaemonState::Controlling { .. } => DaemonState::Controlling {
                        peer_id: self
                            .active_peer_id
                            .clone()
                            .expect("active peer should exist"),
                    },
                    DaemonState::ControlledBy { .. } => DaemonState::ControlledBy {
                        peer_id: self
                            .active_peer_id
                            .clone()
                            .expect("active peer should exist"),
                    },
                    _ => self.state.clone(),
                };
                Ok(())
            }
            _ => Err("active peer must already be authenticated".to_string()),
        }
    }

    pub fn toggle_controller(&mut self) -> Result<(), String> {
        match &self.state {
            DaemonState::ConnectedIdle => {
                let peer_id = self
                    .active_peer_id
                    .clone()
                    .ok_or_else(|| "no active peer is available to control".to_string())?;
                if !self
                    .sessions
                    .get(&peer_id)
                    .map(|session| session.authenticated)
                    .unwrap_or(false)
                {
                    return Err("active peer is not authenticated".to_string());
                }

                self.state = DaemonState::Controlling { peer_id };
                Ok(())
            }
            DaemonState::Controlling { .. } | DaemonState::ControlledBy { .. } => {
                self.state = DaemonState::ConnectedIdle;
                Ok(())
            }
            DaemonState::Disconnected => Err("daemon is disconnected".to_string()),
            DaemonState::Recovering { .. } => Err("daemon is recovering".to_string()),
        }
    }

    pub fn mark_controlled_by(&mut self, peer_id: impl Into<String>) -> Result<(), String> {
        let peer_id = peer_id.into();
        match self.sessions.get(&peer_id) {
            Some(session) if session.authenticated => {
                self.active_peer_id = Some(peer_id.clone());
                self.state = DaemonState::ControlledBy { peer_id };
                Ok(())
            }
            _ => Err("controlled peer must already be authenticated".to_string()),
        }
    }

    pub fn release_control(&mut self) -> Result<(), String> {
        match self.state {
            DaemonState::Controlling { .. } | DaemonState::ControlledBy { .. } => {
                self.state = DaemonState::ConnectedIdle;
                Ok(())
            }
            DaemonState::ConnectedIdle => Ok(()),
            DaemonState::Disconnected => Err("daemon is disconnected".to_string()),
            DaemonState::Recovering { .. } => Err("daemon is recovering".to_string()),
        }
    }

    pub fn enter_recovering(&mut self) {
        self.state = DaemonState::Recovering {
            intended_role: None,
        };
    }
}

impl Default for DaemonRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{DaemonRuntime, DaemonState, Role};

    #[test]
    fn new_runtime_starts_disconnected() {
        let runtime = DaemonRuntime::new();

        assert_eq!(runtime.state, DaemonState::Disconnected);
        assert!(runtime.sessions.is_empty());
        assert!(runtime.active_peer_id.is_none());
    }

    #[test]
    fn authenticated_peer_becomes_active_peer() {
        let mut runtime = DaemonRuntime::new();

        runtime.mark_authenticated("office-pc");

        assert_eq!(runtime.state, DaemonState::ConnectedIdle);
        assert_eq!(runtime.active_peer_id.as_deref(), Some("office-pc"));
    }

    #[test]
    fn toggle_controller_switches_to_controlling_and_back() {
        let mut runtime = DaemonRuntime::new();
        runtime.mark_authenticated("office-pc");

        runtime
            .toggle_controller()
            .expect("should switch into control");
        assert_eq!(
            runtime.state,
            DaemonState::Controlling {
                peer_id: "office-pc".to_string()
            }
        );

        runtime.toggle_controller().expect("should release control");
        assert_eq!(runtime.state, DaemonState::ConnectedIdle);
    }

    #[test]
    fn select_active_peer_rejects_unknown_peer() {
        let mut runtime = DaemonRuntime::new();

        let result = runtime.select_active_peer("missing");

        assert!(result.is_err());
    }

    #[test]
    fn mark_disconnected_clears_active_peer_when_last_session_ends() {
        let mut runtime = DaemonRuntime::new();
        runtime.mark_authenticated("office-pc");

        runtime.mark_disconnected("office-pc");

        assert_eq!(runtime.state, DaemonState::Disconnected);
        assert!(runtime.active_peer_id.is_none());
    }

    #[test]
    fn disconnecting_other_peer_keeps_current_control_state() {
        let mut runtime = DaemonRuntime::new();
        runtime.mark_authenticated("office-pc");
        runtime.mark_authenticated("spare-pc");
        runtime.toggle_controller().expect("should enter control");

        runtime.mark_disconnected("spare-pc");

        assert_eq!(
            runtime.state,
            DaemonState::Controlling {
                peer_id: "office-pc".to_string()
            }
        );
        assert_eq!(runtime.active_peer_id.as_deref(), Some("office-pc"));
    }

    #[test]
    fn active_peer_disconnect_enters_recovering_for_resume() {
        let mut runtime = DaemonRuntime::new();
        runtime.mark_authenticated("office-pc");
        runtime.mark_authenticated("spare-pc");
        runtime.toggle_controller().expect("should enter control");

        runtime.mark_disconnected("office-pc");

        assert_eq!(
            runtime.state,
            DaemonState::Recovering {
                intended_role: Some(Role::Controlling)
            }
        );
        assert_eq!(runtime.active_peer_id.as_deref(), Some("office-pc"));
        assert!(!runtime.sessions.contains_key("office-pc"));
        assert!(runtime.sessions.contains_key("spare-pc"));
    }

    #[test]
    fn reconnecting_resume_peer_clears_recovery() {
        let mut runtime = DaemonRuntime::new();
        runtime.mark_authenticated("office-pc");
        runtime.toggle_controller().expect("should enter control");
        runtime.mark_disconnected("office-pc");

        runtime.mark_authenticated("office-pc");

        assert_eq!(runtime.state, DaemonState::ConnectedIdle);
        assert_eq!(runtime.active_peer_id.as_deref(), Some("office-pc"));
        assert!(runtime.sessions.contains_key("office-pc"));
    }

    #[test]
    fn authenticating_a_different_peer_while_recovering_keeps_recovery_state() {
        let mut runtime = DaemonRuntime::new();
        runtime.mark_authenticated("office-pc");
        runtime.mark_authenticated("spare-pc");
        runtime.toggle_controller().expect("should enter control");
        runtime.mark_disconnected("office-pc");

        runtime.mark_authenticated("backup-pc");

        assert_eq!(
            runtime.state,
            DaemonState::Recovering {
                intended_role: Some(Role::Controlling)
            }
        );
        assert_eq!(runtime.active_peer_id.as_deref(), Some("office-pc"));
        assert!(!runtime.sessions.contains_key("office-pc"));
        assert!(runtime.sessions.contains_key("spare-pc"));
        assert!(runtime.sessions.contains_key("backup-pc"));
    }

    #[test]
    fn selecting_a_different_peer_during_recovery_exits_recovery() {
        let mut runtime = DaemonRuntime::new();
        runtime.mark_authenticated("office-pc");
        runtime.mark_authenticated("spare-pc");
        runtime.toggle_controller().expect("should enter control");
        runtime.mark_disconnected("office-pc");

        runtime
            .select_active_peer("spare-pc")
            .expect("should switch to the surviving peer");

        assert_eq!(runtime.state, DaemonState::ConnectedIdle);
        assert_eq!(runtime.active_peer_id.as_deref(), Some("spare-pc"));
        assert!(runtime.sessions.contains_key("spare-pc"));
        assert!(!runtime.sessions.contains_key("office-pc"));
    }
}
