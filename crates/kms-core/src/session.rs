#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub peer_id: String,
    pub connected: bool,
    pub healthy: bool,
    pub authenticated: bool,
}

impl Session {
    pub fn authenticated(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            connected: true,
            healthy: true,
            authenticated: true,
        }
    }
}
