#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryState {
    pub forced_key_releases: usize,
    pub forced_button_releases: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectBackoff {
    current_secs: u64,
    initial_secs: u64,
    max_secs: u64,
}

impl ReconnectBackoff {
    pub fn new(initial_secs: u64, max_secs: u64) -> Self {
        let initial_secs = initial_secs.max(1);
        let max_secs = max_secs.max(initial_secs);

        Self {
            current_secs: initial_secs,
            initial_secs,
            max_secs,
        }
    }

    pub fn next_delay(&mut self) -> std::time::Duration {
        let delay = std::time::Duration::from_secs(self.current_secs);
        self.current_secs = (self.current_secs.saturating_mul(2)).min(self.max_secs);
        delay
    }

    pub fn reset(&mut self) {
        self.current_secs = self.initial_secs;
    }
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        Self::new(1, 8)
    }
}

#[cfg(test)]
mod tests {
    use super::ReconnectBackoff;

    #[test]
    fn reconnect_backoff_grows_then_caps() {
        let mut backoff = ReconnectBackoff::new(1, 8);

        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(1));
        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(2));
        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(4));
        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(8));
        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(8));
    }

    #[test]
    fn reconnect_backoff_resets_after_success() {
        let mut backoff = ReconnectBackoff::new(1, 8);

        backoff.next_delay();
        backoff.next_delay();
        backoff.reset();

        assert_eq!(backoff.next_delay(), std::time::Duration::from_secs(1));
    }
}
