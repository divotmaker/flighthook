//! Linear reconnect backoff shared by the reconnecting actors.

use std::time::Duration;

/// Growth per failed attempt.
const STEP: Duration = Duration::from_secs(1);

/// Linear reconnect backoff: starts at `min`, grows by 1 s per failed
/// attempt, capped at `max`. An attempt that connected restarts the sequence
/// at `min`, so one long outage does not slow every later reconnect.
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    min: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    pub(crate) const fn new(min: Duration, max: Duration) -> Self {
        Self {
            min,
            max,
            next: min,
        }
    }

    /// Delay before the next attempt. `connected` says whether the attempt
    /// that just ended had connected.
    #[must_use]
    pub(crate) fn delay(&mut self, connected: bool) -> Duration {
        if connected {
            self.next = self.min;
        }
        let delay = self.next;
        self.next = (self.next + STEP).min(self.max);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn grows_by_a_second_up_to_the_cap() {
        let mut b = Backoff::new(secs(1), secs(3));
        let delays: Vec<_> = (0..5).map(|_| b.delay(false)).collect();
        assert_eq!(delays, [secs(1), secs(2), secs(3), secs(3), secs(3)]);
    }

    #[test]
    fn resets_after_a_connected_attempt() {
        let mut b = Backoff::new(secs(1), secs(15));
        for _ in 0..20 {
            let _ = b.delay(false);
        }
        assert_eq!(b.delay(false), secs(15));
        assert_eq!(b.delay(true), secs(1));
        assert_eq!(b.delay(false), secs(2));
        assert_eq!(b.delay(true), secs(1));
    }
}
