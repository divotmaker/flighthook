//! Linear reconnect backoff shared by the reconnecting actors.

use std::time::Duration;

/// Growth per failed attempt.
const STEP: Duration = Duration::from_secs(1);

/// Shortest session that restarts the backoff at `min`. Longer than any
/// connect-time setup (GSPro's first heartbeat goes out ~1 s after connect,
/// a Square Golf identity exchange and arm take a few seconds), so a link
/// that is accepted and then drops during setup counts as a failed attempt
/// and keeps growing towards the cap.
pub(crate) const STABLE_SESSION: Duration = Duration::from_secs(10);

/// Linear reconnect backoff: starts at `min`, grows by 1 s per failed
/// attempt, capped at `max`. A session that lasted at least
/// [`STABLE_SESSION`] restarts the sequence at `min`, so one long outage does
/// not slow every later reconnect, while a link that keeps connecting and
/// dropping at once still backs off to `max`.
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

    /// Delay before the next attempt. `session` is how long the attempt that
    /// just ended stayed connected, `None` if it never connected.
    #[must_use]
    pub(crate) fn delay(&mut self, session: Option<Duration>) -> Duration {
        if session.is_some_and(|s| s >= STABLE_SESSION) {
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
        let delays: Vec<_> = (0..5).map(|_| b.delay(None)).collect();
        assert_eq!(delays, [secs(1), secs(2), secs(3), secs(3), secs(3)]);
    }

    #[test]
    fn resets_after_a_session_that_lasted() {
        let mut b = Backoff::new(secs(1), secs(15));
        for _ in 0..20 {
            let _ = b.delay(None);
        }
        assert_eq!(b.delay(None), secs(15));
        assert_eq!(b.delay(Some(STABLE_SESSION)), secs(1));
        assert_eq!(b.delay(None), secs(2));
        assert_eq!(b.delay(Some(secs(3600))), secs(1));
    }

    #[test]
    fn a_session_that_dropped_at_once_does_not_reset() {
        let mut b = Backoff::new(secs(1), secs(15));
        let delays: Vec<_> = (0..20).map(|_| b.delay(Some(Duration::ZERO))).collect();
        assert_eq!(delays[..3], [secs(1), secs(2), secs(3)]);
        assert_eq!(delays[19], secs(15));
        let just_short = STABLE_SESSION - Duration::from_millis(1);
        assert_eq!(b.delay(Some(just_short)), secs(15));
    }
}
