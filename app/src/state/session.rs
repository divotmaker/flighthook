//! The session: flighthook's `golfer.turn` (golfer, club, hand, mode).
//!
//! [`Session`] is the read side every actor and the web layer share;
//! [`SessionWriter`] is held by the system actor alone, which applies each
//! `golfer.select` on the bus and publishes the turn when it changed.
//!
//! The session's own rules:
//!
//! - It starts with the config's start golfer (the `selected_golfer`, else
//!   the first golfer, else no golfer), that golfer's hand (else
//!   right-handed), no club and mode full.
//! - A select's club, hand and mode each replace the turn's; a field the
//!   select leaves out stays. The mode is never derived from the club: only
//!   a select carrying `mode` changes it.
//! - A select's golfer is taken in any namespace, known or not. When it
//!   changes the golfer (by the lookup rule) and gives no club, the golfer
//!   gets their own last club this run back (the current club when they
//!   have none); when it gives no hand, the hand of the golfer the lookup
//!   rule finds, when that golfer has one. The last club is kept per
//!   resolved golfer.
//! - When the golfer up is a game's (any namespace but flighthook's) and
//!   that golfer's `golfer.profile` is gone (removed by every actor that
//!   published it: the round ended, the card's connection ended, its actor
//!   is gone), the turn leaves the game golfer, in mode full. A game id a
//!   saved golfer lists becomes that saved golfer, club and hand kept. Any
//!   other goes back to the start golfer with the start golfer's last club,
//!   and the start golfer's saved hand when there is one (else the hand is
//!   kept). Whoever holds a mode re-asserts it by the usual rule.
//! - A change of the golfers (a link of a game id to a golfer) keys each
//!   last club again by the new lookup rule; where two ids become one
//!   golfer, the club kept last wins.
//! - Nothing is refused; the last writer wins.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use flighthook::FlighthookConfig;
use flightrelay::{
    Club, FrpBody, FrpMessage, GolferId, GolferSelect, GolferTurn, Handedness, Mode, StateKind,
};

use super::config::ConfigView;

/// The turn the session starts with under no config: no golfer, no club,
/// right-handed, full.
#[must_use]
pub fn start_turn() -> GolferTurn {
    GolferTurn::new(None, None, Handedness::Right, Mode::Full)
}

/// The turn the session starts with under `config`: the start golfer
/// ([`FlighthookConfig::start_golfer`]) and its hand, else
/// [`start_turn`].
#[must_use]
pub fn start_turn_for(config: &FlighthookConfig) -> GolferTurn {
    let mut turn = start_turn();
    if let Some(golfer) = config.start_golfer() {
        turn.golfer = golfer.own_id().and_then(|id| id.parse().ok());
        turn.handedness = Some(golfer.handed.unwrap_or(Handedness::Right));
    }
    turn
}

/// A golfer's last club and when it was kept (the session's count).
type LastClub = (Option<Club>, u64);

/// The session's turn, read side.
#[derive(Clone)]
pub struct Session {
    inner: Arc<RwLock<GolferTurn>>,
}

/// The session's turn, write side (the system actor). It also keeps the
/// turn's cause (the `from` of the select that last changed it) and each
/// resolved golfer's last club this run.
pub struct SessionWriter {
    inner: Arc<RwLock<GolferTurn>>,
    cause: Arc<RwLock<Option<String>>>,
    config: ConfigView,
    /// The club each resolved golfer last had, by
    /// [`FlighthookConfig::golfer_key`].
    last_club: Mutex<HashMap<Option<String>, LastClub>>,
    /// Counts each last club as it is kept: the larger is the later.
    clock: std::sync::atomic::AtomicU64,
    /// The senders of each game golfer's `golfer.profile`, by golfer id.
    profiles: Mutex<HashMap<String, BTreeSet<String>>>,
}

impl Session {
    /// A session with no golfers, at [`start_turn`], and its writer.
    #[cfg(test)]
    #[must_use]
    pub fn new() -> (Self, SessionWriter) {
        Self::with_config(ConfigView::detached(FlighthookConfig::default()))
    }

    /// A session following the golfers of `config`, at
    /// [`start_turn_for`] it, and its writer.
    #[must_use]
    pub fn with_config(config: ConfigView) -> (Self, SessionWriter) {
        let first = start_turn_for(&config.read());
        let inner = Arc::new(RwLock::new(first));
        let cause = Arc::new(RwLock::new(None));
        (
            Self {
                inner: Arc::clone(&inner),
            },
            SessionWriter {
                inner,
                cause,
                config,
                last_club: Mutex::new(HashMap::new()),
                clock: std::sync::atomic::AtomicU64::new(0),
                profiles: Mutex::new(HashMap::new()),
            },
        )
    }

    /// The current turn.
    #[must_use]
    pub fn turn(&self) -> GolferTurn {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The current mode.
    #[cfg(test)]
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.turn().mode.unwrap_or(Mode::Full)
    }
}

impl SessionWriter {
    /// The current turn.
    #[must_use]
    pub fn turn(&self) -> GolferTurn {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The `from` of the select that last changed the turn; `None` when the
    /// session itself is the cause (the start turn, or `apply` in tests).
    #[must_use]
    pub fn cause(&self) -> Option<String> {
        self.cause.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Apply `select` with the session itself as the cause. Returns the new
    /// turn when it changed anything.
    #[cfg(test)]
    pub fn apply(&self, select: &GolferSelect) -> Option<GolferTurn> {
        self.change(None, select)
    }

    /// Apply `select`, sent by `from`. Returns the new turn when it changed
    /// anything; `from` then becomes the turn's [`cause`](Self::cause).
    pub fn apply_from(&self, from: &str, select: &GolferSelect) -> Option<GolferTurn> {
        self.change(Some(from), select)
    }

    /// Follow a change of the golfers in the config, as `from`. `before`
    /// holds each golfer's own id and hand as they were before the change.
    ///
    /// - The golfers went from none to some while no golfer is up, or the
    ///   change removed the `flighthook.*` golfer up: the start golfer
    ///   ([`start_turn_for`]) comes up with its hand.
    /// - The change gave the golfer up (by the lookup rule) another hand:
    ///   the turn takes it.
    ///
    /// Returns the new turn when it changed.
    pub fn follow_golfers(
        &self,
        from: &str,
        before: &BTreeMap<String, Option<Handedness>>,
    ) -> Option<GolferTurn> {
        self.rekey_clubs();
        let select = {
            let config = self.config.read();
            let turn = self.turn();
            let start = start_turn_for(&config).golfer;
            let come_up = match turn.golfer.as_ref() {
                None => before.is_empty() && !config.golfers.is_empty(),
                Some(id) => {
                    before.contains_key(id.as_str()) && config.golfer_of(id.as_str()).is_none()
                }
            };
            if come_up {
                (start != turn.golfer).then(|| GolferSelect {
                    golfer: Some(start),
                    ..GolferSelect::new("")
                })
            } else {
                turn.golfer
                    .as_ref()
                    .and_then(|id| config.golfer_of(id.as_str()))
                    .and_then(|g| {
                        let hand = g.handed?;
                        let was = before.get(g.own_id()?)?;
                        (*was != Some(hand)).then(|| GolferSelect {
                            handedness: Some(hand),
                            ..GolferSelect::new("")
                        })
                    })
            }
        }?;
        self.change(Some(from), &select)
    }

    /// Take note of `frp`, a message on the bus, as flighthook (`own`).
    /// A game's `golfer.profile` is kept with the set of actors that
    /// published it. A sender's removal of it (or the removal of the
    /// sender's `bus.actor`) drops only that sender; a removal by an actor
    /// that never published it does nothing. The golfer is gone when no
    /// sender is left: when that golfer is up, a golfer a saved golfer
    /// lists becomes that saved golfer (club and hand kept), any other
    /// goes back to the start golfer ([`start_turn_for`]) with the start
    /// golfer's saved hand, if any; mode full, caused by `own`. Returns
    /// the new turn when it changed.
    pub fn follow_profiles(&self, own: &str, frp: &FrpMessage) -> Option<GolferTurn> {
        if frp.from == own {
            return None;
        }
        match &frp.body {
            FrpBody::GolferProfile(profile) => {
                self.profiles
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry(profile.id.as_str().to_owned())
                    .or_default()
                    .insert(frp.from.clone());
                None
            }
            FrpBody::Removed(r) if r.kind == StateKind::GolferProfile => {
                let id = r.id.as_deref()?;
                {
                    let mut held = self.profiles.lock().unwrap_or_else(|e| e.into_inner());
                    // Only an actor that published a profile takes its own
                    // copy away; the golfer stays while another holds it.
                    let senders = held.get_mut(id)?;
                    if !senders.remove(&frp.from) || !senders.is_empty() {
                        return None;
                    }
                    held.remove(id);
                }
                self.golfers_gone(own, &[id.to_owned()])
            }
            FrpBody::Removed(r) if r.kind == StateKind::BusActor => self.actor_gone(own, &frp.from),
            _ => None,
        }
    }

    /// Everything `from` published is gone (its actor is): see
    /// [`follow_profiles`](Self::follow_profiles).
    pub fn actor_gone(&self, own: &str, from: &str) -> Option<GolferTurn> {
        let gone: Vec<String> = {
            let mut held = self.profiles.lock().unwrap_or_else(|e| e.into_inner());
            let mut gone = Vec::new();
            held.retain(|id, senders| {
                if senders.remove(from) && senders.is_empty() {
                    gone.push(id.clone());
                    false
                } else {
                    true
                }
            });
            gone.sort();
            gone
        };
        self.golfers_gone(own, &gone)
    }

    fn golfers_gone(&self, own: &str, gone: &[String]) -> Option<GolferTurn> {
        let up = self.turn().golfer?;
        if !gone.iter().any(|id| id == up.as_str())
            || up.namespace() == flighthook::GOLFER_NAMESPACE
        {
            return None;
        }
        let config = self.config.read();
        // A game id a saved golfer lists is that golfer: the golfer, club
        // and hand stay. Any other goes to the start golfer, with the hand
        // that golfer has saved, if any; without one the turn keeps its
        // hand.
        let select = if let Some(own_id) = config
            .golfer_of(up.as_str())
            .and_then(flighthook::GolferRecord::own_id)
            .and_then(|id| id.parse().ok())
        {
            GolferSelect {
                golfer: Some(Some(own_id)),
                mode: Some(Mode::Full),
                ..GolferSelect::new("")
            }
        } else {
            let start = config.start_golfer();
            GolferSelect {
                golfer: Some(
                    start
                        .and_then(flighthook::GolferRecord::own_id)
                        .and_then(|id| id.parse().ok()),
                ),
                handedness: start.and_then(|g| g.handed),
                mode: Some(Mode::Full),
                ..GolferSelect::new("")
            }
        };
        drop(config);
        self.change(Some(own), &select)
    }

    /// Key each golfer's last club by the lookup rule as it is now: the
    /// golfers may have changed, and two ids may be one golfer, who has the
    /// club they had last.
    fn rekey_clubs(&self) {
        let config = self.config.read();
        let mut last = self.last_club.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::mem::take(&mut *last);
        for (key, (club, tick)) in old {
            let now = key
                .as_deref()
                .and_then(|k| config.golfer_key(Some(k)))
                .or(key);
            if last.get(&now).is_none_or(|(_, held)| *held < tick) {
                last.insert(now, (club, tick));
            }
        }
    }

    fn change(&self, from: Option<&str>, select: &GolferSelect) -> Option<GolferTurn> {
        let config = self.config.read();
        let mut last = self.last_club.lock().unwrap_or_else(|e| e.into_inner());
        let mut turn = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let mut next = turn.clone();
        let key_of =
            |golfer: &Option<GolferId>| config.golfer_key(golfer.as_ref().map(GolferId::as_str));
        if let Some(golfer) = &select.golfer {
            next.golfer.clone_from(golfer);
            let to = key_of(&next.golfer);
            if key_of(&turn.golfer) != to {
                if select.club.is_none()
                    && let Some((club, _)) = last.get(&to)
                {
                    next.club = *club;
                }
                if select.handedness.is_none()
                    && let Some(hand) = next
                        .golfer
                        .as_ref()
                        .and_then(|id| config.golfer_of(id.as_str()))
                        .and_then(|g| g.handed)
                {
                    next.handedness = Some(hand);
                }
            }
        }
        if let Some(club) = select.club {
            next.club = club;
        }
        if let Some(hand) = select.handedness {
            next.handedness = Some(hand);
        }
        if let Some(mode) = select.mode {
            next.mode = Some(mode);
        }
        if next == *turn {
            return None;
        }
        let tick = self
            .clock
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        last.insert(key_of(&next.golfer), (next.club, tick));
        *turn = next.clone();
        *self.cause.write().unwrap_or_else(|e| e.into_inner()) = from.map(str::to_owned);
        Some(next)
    }
}

#[cfg(test)]
mod tests;
