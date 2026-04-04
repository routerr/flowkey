#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatConfig {
    pub interval_secs: u64,
    pub timeout_secs: u64,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval_secs: 2,
            timeout_secs: 6,
        }
    }
}
