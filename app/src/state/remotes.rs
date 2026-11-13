//! Whether the actors on the far side of the hub are still there.
//!
//! Only remote actors (messages that came in on a hub link) are tracked:
//! an actor this process hosts never expires, whatever its heartbeat timer
//! did. A remote actor is gone when its `bus.actor` has not been heard for
//! three of its heartbeats. A `bus.actor` whose `heartbeat` is zero,
//! negative or not a number counts as the default heartbeat (2 s); one
//! under 0.5 s counts as 0.5 s and one over 60 s as 60 s. An actor
//! that never sent a `bus.actor` is gone the same three default heartbeats
//! after its last message.
//!
//! The system actor runs [`Remotes`] and announces each gone actor on the
//! internal bus as [`LocalEvent::ActorGone`](crate::bus::LocalEvent).
//!
//! A look that comes more than [`LATE`] after it was due means the process
//! stalled (a sleeping PC, a paused process). The remote actors' heartbeats
//! from that time may still wait unread in their sockets, so that look
//! expires no one and every clock starts again at it: an actor is then
//! judged from the first messages after the stall.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use flightrelay::{DEFAULT_HEARTBEAT, FrpBody, FrpMessage, MISSED_HEARTBEATS, StateKind};

/// How often the system actor looks for gone actors.
pub const EXPIRY_TICK: Duration = Duration::from_secs(1);

/// How late a look may come before it counts as after a stall: one default
/// heartbeat.
pub const LATE: Duration = DEFAULT_HEARTBEAT;

#[derive(Debug, Clone, Copy)]
struct Heard {
    at: Instant,
    /// How long the actor may stay silent.
    allowed: Duration,
    /// A `bus.actor` arrived: only the next one restarts the clock.
    announced: bool,
}

/// The remote actors' heartbeat clocks. See the [module docs](self).
#[derive(Debug, Default)]
pub struct Remotes {
    heard: HashMap<String, Heard>,
}

/// The shortest heartbeat taken as given; a shorter one counts as this.
pub const MIN_HEARTBEAT: Duration = Duration::from_millis(500);

/// The longest heartbeat taken as given; a longer one counts as this.
pub const MAX_HEARTBEAT: Duration = Duration::from_secs(60);

/// Three heartbeats of `heartbeat` seconds, held to [`MIN_HEARTBEAT`] ..
/// [`MAX_HEARTBEAT`]; the default heartbeat stands in for one that is not a
/// positive, finite number.
fn allowed(heartbeat: f64) -> Duration {
    Duration::try_from_secs_f64(heartbeat)
        .ok()
        .filter(|d| !d.is_zero())
        .map_or(DEFAULT_HEARTBEAT, |d| d.clamp(MIN_HEARTBEAT, MAX_HEARTBEAT))
        .saturating_mul(MISSED_HEARTBEATS)
}

impl Remotes {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take one message that came in on a hub link, heard at `now`.
    pub fn hear(&mut self, msg: &FrpMessage, now: Instant) {
        match &msg.body {
            FrpBody::BusActor(actor) => {
                self.heard.insert(
                    msg.from.clone(),
                    Heard {
                        at: now,
                        allowed: allowed(actor.heartbeat),
                        announced: true,
                    },
                );
            }
            FrpBody::Removed(r) if r.kind == StateKind::BusActor => {
                self.heard.remove(&msg.from);
            }
            _ => {
                let fresh = Heard {
                    at: now,
                    allowed: allowed(0.0),
                    announced: false,
                };
                let heard = self.heard.entry(msg.from.clone()).or_insert(fresh);
                if !heard.announced {
                    *heard = fresh;
                }
            }
        }
    }

    /// The look due at `due`, made at `now`: [`Self::expire`], unless it
    /// comes more than [`LATE`] late. Then it expires no one and restarts
    /// every clock at `now`.
    pub fn check(&mut self, now: Instant, due: Instant) -> Vec<String> {
        if now.saturating_duration_since(due) > LATE {
            for heard in self.heard.values_mut() {
                heard.at = now;
            }
            return Vec::new();
        }
        self.expire(now)
    }

    /// Forget and return every actor silent for longer than it may be at
    /// `now`, in id order.
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let mut gone: Vec<String> = self
            .heard
            .iter()
            .filter(|(_, h)| now.saturating_duration_since(h.at) > h.allowed)
            .map(|(id, _)| id.clone())
            .collect();
        gone.sort();
        for id in &gone {
            self.heard.remove(id);
        }
        gone
    }

    /// Whether `actor` is tracked as a remote actor.
    #[cfg(test)]
    #[must_use]
    pub fn knows(&self, actor: &str) -> bool {
        self.heard.contains_key(actor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightrelay::{BusActor, GolferTurn, Handedness, Mode, Removal};

    fn actor(from: &str, heartbeat: f64) -> FrpMessage {
        FrpMessage::new(from, BusActor::new(from, heartbeat))
    }

    fn turn(from: &str) -> FrpMessage {
        FrpMessage::new(
            from,
            GolferTurn::new(None, None, Handedness::Right, Mode::Full),
        )
    }

    #[test]
    fn a_remote_actor_goes_after_three_of_its_heartbeats() {
        let t0 = Instant::now();
        let mut remotes = Remotes::new();
        remotes.hear(&actor("lm", 1.0), t0);
        assert!(remotes.expire(t0 + Duration::from_millis(2900)).is_empty());
        remotes.hear(&turn("lm"), t0 + Duration::from_secs(2));
        assert_eq!(
            remotes.expire(t0 + Duration::from_millis(3100)),
            ["lm"],
            "only a bus.actor keeps an announced actor"
        );
        assert!(!remotes.knows("lm"));
    }

    /// A heartbeat of 0, a negative one or none counts as 2 s; an actor
    /// with no `bus.actor` goes 6 s after its last message.
    #[test]
    fn an_actor_with_no_heartbeat_clock_still_goes() {
        let t0 = Instant::now();
        let mut remotes = Remotes::new();
        remotes.hear(&actor("zero", 0.0), t0);
        remotes.hear(&actor("negative", -5.0), t0);
        remotes.hear(&actor("nan", f64::NAN), t0);
        remotes.hear(&turn("stray"), t0);
        remotes.hear(&turn("stray"), t0 + Duration::from_secs(3));
        assert!(remotes.expire(t0 + Duration::from_millis(5900)).is_empty());
        assert_eq!(
            remotes.expire(t0 + Duration::from_millis(6100)),
            ["nan", "negative", "zero"]
        );
        assert_eq!(remotes.expire(t0 + Duration::from_millis(9100)), ["stray"]);
    }

    /// The process stalls 8 s with two remote actors whose
    /// heartbeats wait in their sockets. The late look expires neither; the
    /// one that speaks again stays, the one that never does goes three
    /// heartbeats after the stall.
    #[test]
    fn a_late_look_after_a_stall_expires_no_one() {
        let t0 = Instant::now();
        let s = Duration::from_secs_f64;
        let mut remotes = Remotes::new();
        remotes.hear(&actor("game", 1.0), t0);
        remotes.hear(&actor("lm", 1.0), t0);
        // The look due at t0 + 1 s runs at t0 + 9 s.
        assert!(remotes.check(t0 + s(9.0), t0 + s(1.0)).is_empty());
        assert!(remotes.knows("game") && remotes.knows("lm"));
        // The held-up heartbeats arrive after it; the game is gone for real.
        remotes.hear(&actor("lm", 1.0), t0 + s(9.05));
        assert!(remotes.check(t0 + s(10.0), t0 + s(10.0)).is_empty());
        remotes.hear(&actor("lm", 1.0), t0 + s(11.0));
        assert!(remotes.check(t0 + s(11.9), t0 + s(11.9)).is_empty());
        assert_eq!(remotes.check(t0 + s(12.1), t0 + s(12.1)), ["game"]);
        assert!(remotes.knows("lm"));
        // A look a little late (under a heartbeat) still judges.
        assert_eq!(remotes.check(t0 + s(15.0), t0 + s(13.5)), ["lm"]);
    }

    /// A tiny heartbeat counts as 0.5 s and a huge one as
    /// 60 s, so neither expires at every look nor never.
    #[test]
    fn a_heartbeat_is_held_to_half_a_second_to_a_minute() {
        let t0 = Instant::now();
        let mut remotes = Remotes::new();
        remotes.hear(&actor("tiny", 0.001), t0);
        remotes.hear(&actor("huge", 86_400.0), t0);
        assert!(remotes.expire(t0 + Duration::from_millis(1400)).is_empty());
        assert_eq!(remotes.expire(t0 + Duration::from_millis(1600)), ["tiny"]);
        assert!(remotes.expire(t0 + Duration::from_secs(179)).is_empty());
        assert_eq!(remotes.expire(t0 + Duration::from_secs(181)), ["huge"]);
    }

    #[test]
    fn a_removed_actor_is_not_expired_again() {
        let t0 = Instant::now();
        let mut remotes = Remotes::new();
        remotes.hear(&actor("lm", 2.0), t0);
        remotes.hear(&FrpMessage::new("lm", Removal::of(StateKind::BusActor)), t0);
        assert!(remotes.expire(t0 + Duration::from_secs(60)).is_empty());
    }
}
