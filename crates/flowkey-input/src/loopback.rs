use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::event::InputEvent;
use tracing::warn;

#[derive(Debug)]
pub struct LoopbackSuppressor {
    window: Duration,
    recent: VecDeque<RecordedEvent>,
}

#[derive(Debug, Clone)]
struct RecordedEvent {
    recorded_at: Instant,
    event: InputEvent,
}

pub type SharedLoopbackSuppressor = Arc<Mutex<LoopbackSuppressor>>;

pub fn lock_recovering<'a>(
    shared: &'a SharedLoopbackSuppressor,
) -> MutexGuard<'a, LoopbackSuppressor> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            warn!(target: "loopback", "poisoned mutex, recovering");
            shared.clear_poison();
            poisoned.into_inner()
        }
    }
}

impl LoopbackSuppressor {
    pub fn shared(window: Duration) -> SharedLoopbackSuppressor {
        Arc::new(Mutex::new(Self::new(window)))
    }

    pub fn new(window: Duration) -> Self {
        Self {
            window,
            recent: VecDeque::new(),
        }
    }

    pub fn record(&mut self, event: InputEvent) {
        self.purge_expired();
        self.recent.push_back(RecordedEvent {
            recorded_at: Instant::now(),
            event,
        });
    }

    pub fn should_suppress(&mut self, event: &InputEvent) -> bool {
        self.purge_expired();

        if let Some(index) = self
            .recent
            .iter()
            .position(|recorded| recorded.event.matches_ignoring_timestamp(event))
        {
            self.recent.remove(index);
            return true;
        }

        false
    }

    fn purge_expired(&mut self) {
        let cutoff = Instant::now()
            .checked_sub(self.window)
            .unwrap_or_else(Instant::now);

        while self
            .recent
            .front()
            .is_some_and(|recorded| recorded.recorded_at < cutoff)
        {
            self.recent.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::event::{InputEvent, Modifiers, MouseButton};

    use super::{lock_recovering, LoopbackSuppressor};

    #[test]
    fn suppresses_recently_recorded_events_exactly_once() {
        let mut suppressor = LoopbackSuppressor::new(Duration::from_secs(1));
        let event = InputEvent::MouseButtonDown {
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            timestamp_us: 0,
        };

        suppressor.record(event.clone());

        assert!(suppressor.should_suppress(&event));
        assert!(!suppressor.should_suppress(&event));
    }

    #[test]
    fn does_not_suppress_unrelated_events() {
        let mut suppressor = LoopbackSuppressor::new(Duration::from_secs(1));
        let injected = InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: true,
                alt: false,
                meta: false,
            },
            timestamp_us: 0,
        };
        let captured = InputEvent::KeyDown {
            code: "KeyL".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: true,
                alt: false,
                meta: false,
            },
            timestamp_us: 0,
        };

        suppressor.record(injected);

        assert!(!suppressor.should_suppress(&captured));
    }

    #[test]
    fn suppresses_matching_events_even_when_timestamps_differ() {
        let mut suppressor = LoopbackSuppressor::new(Duration::from_secs(1));
        let injected = InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: true,
                alt: false,
                meta: false,
            },
            timestamp_us: 100,
        };
        let captured = InputEvent::KeyDown {
            code: "KeyK".to_string(),
            modifiers: Modifiers {
                shift: false,
                control: true,
                alt: false,
                meta: false,
            },
            timestamp_us: 900,
        };

        suppressor.record(injected);

        assert!(suppressor.should_suppress(&captured));
    }

    #[test]
    fn recovers_from_poisoned_mutex() {
        let shared = LoopbackSuppressor::shared(Duration::from_secs(1));
        let event = InputEvent::MouseButtonDown {
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            timestamp_us: 0,
        };

        let poison = std::panic::catch_unwind({
            let shared = shared.clone();
            move || {
                let _guard = shared.lock().unwrap();
                panic!("poison the loopback mutex");
            }
        });
        assert!(poison.is_err());
        assert!(shared.lock().is_err());

        {
            let mut suppressor = lock_recovering(&shared);
            suppressor.record(event.clone());
        }

        assert!(shared.lock().is_ok());
        let mut suppressor = shared.lock().unwrap();
        assert!(suppressor.should_suppress(&event));
    }
}
