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
pub struct Session {
    inner: Arc<RwLock<GolferTurn>>,
    cause: Arc<RwLock<Option<String>>>,
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
                cause: Arc::clone(&cause),
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

    /// The `from` of the select that last changed the turn; `None` when
    /// the session itself is the cause.
    #[must_use]
    pub fn cause(&self) -> Option<String> {
        self.cause.read().unwrap_or_else(|e| e.into_inner()).clone()
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
    /// session itself is the cause (the start turn, [`apply`](Self::apply)).
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
mod tests {
    use super::*;
    use flightrelay::Club;

    fn select(
        club: Option<Option<Club>>,
        hand: Option<Handedness>,
        mode: Option<Mode>,
    ) -> GolferSelect {
        GolferSelect {
            club,
            handedness: hand,
            mode,
            ..GolferSelect::new("r")
        }
    }

    #[test]
    fn the_session_starts_right_handed_in_full_with_no_golfer_or_club() {
        let (session, _writer) = Session::new();
        assert_eq!(session.turn(), start_turn());
        assert_eq!(start_turn().golfer, None);
        assert_eq!(start_turn().club, None);
        assert_eq!(start_turn().handedness, Some(Handedness::Right));
        assert_eq!(start_turn().mode, Some(Mode::Full));
    }

    #[test]
    fn a_club_never_sets_the_mode() {
        let (session, writer) = Session::new();
        let turn = writer
            .apply(&select(Some(Some(Club::Putter)), None, None))
            .expect("changed");
        assert_eq!(turn.club, Some(Club::Putter));
        assert_eq!(turn.mode, Some(Mode::Full));
        assert_eq!(session.turn(), turn);
    }

    #[test]
    fn only_a_select_with_a_mode_changes_the_mode() {
        let (_session, writer) = Session::new();
        let turn = writer
            .apply(&select(
                Some(Some(Club::SandWedge)),
                None,
                Some(Mode::Chipping),
            ))
            .expect("changed");
        assert_eq!(turn.mode, Some(Mode::Chipping));
        // A later club alone (a header pick) leaves the mode the card set.
        let turn = writer
            .apply(&select(Some(Some(Club::Driver)), None, None))
            .expect("changed");
        assert_eq!(turn.club, Some(Club::Driver));
        assert_eq!(turn.mode, Some(Mode::Chipping));
        let turn = writer
            .apply(&select(None, None, Some(Mode::Full)))
            .expect("changed");
        assert_eq!(turn.mode, Some(Mode::Full));
    }

    #[test]
    fn a_select_that_changes_nothing_publishes_nothing() {
        let (_session, writer) = Session::new();
        let all = select(
            Some(Some(Club::SandWedge)),
            Some(Handedness::Left),
            Some(Mode::Chipping),
        );
        assert!(writer.apply(&all).is_some());
        assert!(writer.apply(&all).is_none());
        assert!(writer.apply(&select(None, None, None)).is_none());
    }

    #[test]
    fn each_field_replaces_its_own_and_the_rest_stays() {
        let (_session, writer) = Session::new();
        writer.apply(&select(
            Some(Some(Club::Iron7)),
            Some(Handedness::Left),
            Some(Mode::Chipping),
        ));
        let turn = writer
            .apply(&select(None, None, Some(Mode::Putting)))
            .expect("changed");
        assert_eq!(turn.club, Some(Club::Iron7));
        assert_eq!(turn.handedness, Some(Handedness::Left));
        assert_eq!(turn.mode, Some(Mode::Putting));
        let turn = writer
            .apply(&select(Some(None), None, None))
            .expect("changed");
        assert_eq!(turn.club, None);
        assert_eq!(turn.mode, Some(Mode::Putting));
    }

    #[test]
    fn the_cause_is_the_sender_of_the_last_change() {
        let (_session, writer) = Session::new();
        assert_eq!(writer.cause(), None);
        let iron = select(Some(Some(Club::Iron7)), None, None);
        assert!(writer.apply_from("gspro-1", &iron).is_some());
        assert_eq!(writer.cause().as_deref(), Some("gspro-1"));
        // A select that changes nothing leaves the cause alone.
        assert!(writer.apply_from("web", &iron).is_none());
        assert_eq!(writer.cause().as_deref(), Some("gspro-1"));
        assert!(writer.apply(&select(Some(None), None, None)).is_some());
        assert_eq!(writer.cause(), None);
    }

    fn golfers() -> FlighthookConfig {
        let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
        eric.identifiers.push("pureplay.local-3".into());
        eric.handed = Some(Handedness::Left);
        FlighthookConfig {
            golfers: vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")],
            ..FlighthookConfig::default()
        }
    }

    fn pick(golfer: &str) -> GolferSelect {
        GolferSelect {
            golfer: Some(Some(golfer.parse().expect("id"))),
            ..GolferSelect::new("r")
        }
    }

    #[test]
    fn the_session_starts_with_the_selected_golfer_else_the_first_and_its_hand() {
        let config = golfers();
        let turn = start_turn_for(&config);
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("flighthook.e")
        );
        assert_eq!(turn.handedness, Some(Handedness::Left));
        assert_eq!(turn.club, None);
        assert_eq!(turn.mode, Some(Mode::Full));
        let config = FlighthookConfig {
            selected_golfer: Some("flighthook.s".into()),
            ..golfers()
        };
        let turn = start_turn_for(&config);
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("flighthook.s")
        );
        assert_eq!(turn.handedness, Some(Handedness::Right), "Sam has no hand");
        assert_eq!(start_turn_for(&FlighthookConfig::default()), start_turn());
    }

    #[test]
    fn hand_and_last_club_follow_the_golfer() {
        let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
        // Eric (lh) is up: 7I.
        writer.apply(&select(Some(Some(Club::Iron7)), None, None));
        // Sam: no last club yet, keeps the club in play; no hand of his own.
        let turn = writer.apply(&pick("flighthook.s")).expect("changed");
        assert_eq!(turn.club, Some(Club::Iron7));
        assert_eq!(turn.handedness, Some(Handedness::Left));
        writer.apply(&select(
            Some(Some(Club::Driver)),
            Some(Handedness::Right),
            None,
        ));
        // Back to Eric: his own club and hand.
        let turn = writer.apply(&pick("flighthook.e")).expect("changed");
        assert_eq!(turn.club, Some(Club::Iron7));
        assert_eq!(turn.handedness, Some(Handedness::Left));
        // Sam again: the driver.
        let turn = writer.apply(&pick("flighthook.s")).expect("changed");
        assert_eq!(turn.club, Some(Club::Driver));
        // Eric's linked game id is Eric: his last club, shared.
        let turn = writer.apply(&pick("pureplay.local-3")).expect("changed");
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("pureplay.local-3")
        );
        assert_eq!(turn.club, Some(Club::Iron7));
        // Eric's own id after his linked one is the same golfer: no club or
        // hand change, only the raw id.
        writer.apply(&select(Some(Some(Club::Putter)), None, None));
        let turn = writer.apply(&pick("flighthook.e")).expect("changed");
        assert_eq!(turn.club, Some(Club::Putter));
        // A select carrying club and hand with the golfer: those win.
        let mut s = pick("flighthook.s");
        s.club = Some(Some(Club::Wood3));
        s.handedness = Some(Handedness::Left);
        let turn = writer.apply(&s).expect("changed");
        assert_eq!(turn.club, Some(Club::Wood3));
        assert_eq!(turn.handedness, Some(Handedness::Left));
        // An unknown id stands on its own, and is taken.
        let turn = writer.apply(&pick("backnine.p7")).expect("changed");
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("backnine.p7")
        );
        assert_eq!(turn.club, Some(Club::Wood3), "no last club of its own");
    }

    /// Each golfer's own id and hand in `config`: what
    /// [`SessionWriter::follow_golfers`] compares against.
    fn hands(config: &FlighthookConfig) -> BTreeMap<String, Option<Handedness>> {
        config
            .golfers
            .iter()
            .filter_map(|g| Some((g.own_id()?.to_owned(), g.handed)))
            .collect()
    }

    #[test]
    fn a_golfer_change_in_the_config_is_followed() {
        let view = ConfigView::detached(FlighthookConfig::default());
        let (_session, writer) = Session::with_config(view.clone());
        // Follow the change from what the config held before it.
        let follow = |change: &dyn Fn(&mut FlighthookConfig)| {
            let before = hands(&view.read());
            change(&mut view.write());
            writer.follow_golfers("fh", &before)
        };
        assert_eq!(writer.turn().golfer, None);
        assert!(follow(&|_| {}).is_none(), "still no golfer");
        // The first saved golfer becomes the golfer up, with its hand.
        let turn = follow(&|c| *c = golfers()).expect("changed");
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("flighthook.e")
        );
        assert_eq!(turn.handedness, Some(Handedness::Left));
        assert_eq!(writer.cause().as_deref(), Some("fh"));
        // Another golfer added: the golfer up stays.
        assert!(
            follow(&|c| c
                .golfers
                .push(flighthook::GolferRecord::new("flighthook.x", "X")))
            .is_none()
        );
        // The golfer up removed: the start golfer comes up.
        let turn = follow(&|c| {
            c.golfers.remove(0);
        })
        .expect("changed");
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("flighthook.s")
        );
        // A game id up is never replaced.
        writer.apply(&pick("pureplay.local-3"));
        assert!(follow(&|c| c.golfers.truncate(2)).is_none());
        // Every golfer removed: no golfer.
        writer.apply(&pick("flighthook.s"));
        let turn = follow(&|c| c.golfers.clear()).expect("changed");
        assert_eq!(turn.golfer, None);
    }

    fn profile(from: &str, id: &str) -> FrpMessage {
        FrpMessage::new(
            from,
            flightrelay::GolferProfile::new(id.parse().expect("id"), "G"),
        )
    }

    fn removed(from: &str, id: &str) -> FrpMessage {
        FrpMessage::new(
            from,
            flightrelay::Removal::of_id(StateKind::GolferProfile, id),
        )
    }

    /// The game golfer up whose profile goes: the turn goes back to the
    /// start golfer with that golfer's hand and mode full. A user with no
    /// saved golfers gets no golfer.
    #[test]
    fn a_game_golfer_up_whose_profile_goes_returns_the_turn_to_the_start_golfer() {
        let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
        // Eric (lh) is the start golfer. A game golfer comes up in the
        // putter and chipping.
        assert!(
            writer
                .follow_profiles("fh", &profile("pp", "pureplay.player-1"))
                .is_none()
        );
        writer.apply_from(
            "pp",
            &GolferSelect {
                golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
                club: Some(Some(Club::Putter)),
                handedness: Some(Handedness::Right),
                mode: Some(Mode::Putting),
                ..GolferSelect::new("r")
            },
        );
        // Another golfer's profile going changes nothing.
        assert!(
            writer
                .follow_profiles("fh", &profile("pp", "pureplay.player-2"))
                .is_none()
        );
        assert!(
            writer
                .follow_profiles("fh", &removed("pp", "pureplay.player-2"))
                .is_none()
        );
        let turn = writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .expect("back");
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("flighthook.e")
        );
        assert_eq!(turn.handedness, Some(Handedness::Left));
        assert_eq!(turn.mode, Some(Mode::Full));
        assert_eq!(writer.cause().as_deref(), Some("fh"));
        // Nothing more when it is already back.
        assert!(
            writer
                .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
                .is_none()
        );
    }

    #[test]
    fn a_gone_card_takes_its_golfers_with_it() {
        let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
        writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
        writer.apply_from(
            "pp",
            &GolferSelect {
                golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
                mode: Some(Mode::Chipping),
                ..GolferSelect::new("r")
            },
        );
        // Another actor going leaves the turn.
        assert!(writer.actor_gone("fh", "other").is_none());
        let turn = writer.actor_gone("fh", "pp").expect("back");
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("flighthook.e")
        );
        assert_eq!(turn.mode, Some(Mode::Full));
        // The removal of its `bus.actor` does the same.
        writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
        writer.apply_from("pp", &pick("pureplay.player-1"));
        let bye = FrpMessage::new("pp", flightrelay::Removal::of(StateKind::BusActor));
        assert!(writer.follow_profiles("fh", &bye).is_some());
    }

    #[test]
    fn with_no_saved_golfers_the_turn_goes_back_to_no_golfer() {
        let (_session, writer) = Session::new();
        writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
        writer.apply_from("pp", &pick("pureplay.player-1"));
        let turn = writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .expect("back");
        assert_eq!(turn.golfer, None);
        assert_eq!(turn.handedness, Some(Handedness::Right));
        assert_eq!(turn.mode, Some(Mode::Full));
    }

    /// A flighthook golfer up is never sent back by someone's removal.
    #[test]
    fn a_flighthook_golfer_is_not_a_game_golfer() {
        let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
        writer.apply_from("web", &pick("flighthook.s"));
        assert!(
            writer
                .follow_profiles("fh", &removed("pp", "flighthook.s"))
                .is_none()
        );
    }

    /// Linking a game id to a golfer moves the id's last club to that
    /// golfer: a header pick of the golfer then gives the club just hit
    /// under the game's id.
    #[test]
    fn linking_a_game_id_rekeys_its_last_club() {
        let view = ConfigView::detached(FlighthookConfig {
            golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
            ..FlighthookConfig::default()
        });
        let (_session, writer) = Session::with_config(view.clone());
        // Eric had the 7 iron; then the game's golfer took the 9 iron.
        writer.apply(&select(Some(Some(Club::Iron7)), None, None));
        writer.apply(&pick("pureplay.local-3"));
        writer.apply(&select(Some(Some(Club::Iron9)), None, None));
        writer.apply(&pick("flighthook.s"));
        // The link: the game's golfer is Eric now.
        view.write().golfers[0]
            .identifiers
            .push("pureplay.local-3".into());
        writer.follow_golfers("fh", &hands(&view.read()));
        let turn = writer.apply(&pick("flighthook.e")).expect("changed");
        assert_eq!(turn.club, Some(Club::Iron9), "the club just hit");
    }

    /// A game golfer linked to a saved golfer who is not the start golfer:
    /// the turn goes to that golfer, not to the start golfer.
    #[test]
    fn a_linked_golfer_whose_profile_goes_stays_that_golfer() {
        let mut sam = flighthook::GolferRecord::new("flighthook.s", "Sam");
        sam.identifiers.push("pureplay.player-1".into());
        sam.handed = Some(Handedness::Right);
        let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
        eric.handed = Some(Handedness::Left);
        let config = FlighthookConfig {
            golfers: vec![eric, sam],
            ..FlighthookConfig::default()
        };
        let (_session, writer) = Session::with_config(ConfigView::detached(config));
        writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
        writer.apply_from(
            "pp",
            &GolferSelect {
                golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
                club: Some(Some(Club::Iron7)),
                handedness: Some(Handedness::Right),
                mode: Some(Mode::Chipping),
                ..GolferSelect::new("r")
            },
        );
        let turn = writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .expect("changed");
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("flighthook.s")
        );
        assert_eq!(turn.handedness, Some(Handedness::Right));
        assert_eq!(turn.club, Some(Club::Iron7));
        assert_eq!(turn.mode, Some(Mode::Full));
    }

    /// With no saved hand to go back to, the turn keeps the hand it has.
    #[test]
    fn the_fallback_keeps_the_hand_when_there_is_no_saved_hand() {
        let (_session, writer) = Session::new();
        writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
        writer.apply_from(
            "pp",
            &GolferSelect {
                golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
                handedness: Some(Handedness::Left),
                ..GolferSelect::new("r")
            },
        );
        let turn = writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .expect("changed");
        assert_eq!(turn.golfer, None);
        assert_eq!(turn.handedness, Some(Handedness::Left));
        // A saved golfer with no hand of their own is the same.
        let config = FlighthookConfig {
            golfers: vec![flighthook::GolferRecord::new("flighthook.s", "Sam")],
            ..FlighthookConfig::default()
        };
        let (_session, writer) = Session::with_config(ConfigView::detached(config));
        writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
        writer.apply_from(
            "pp",
            &GolferSelect {
                golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
                handedness: Some(Handedness::Left),
                ..GolferSelect::new("r")
            },
        );
        let turn = writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .expect("changed");
        assert_eq!(
            turn.golfer.as_ref().map(GolferId::as_str),
            Some("flighthook.s")
        );
        assert_eq!(turn.handedness, Some(Handedness::Left));
    }

    /// Only the actor that published a golfer's profile takes it away.
    #[test]
    fn another_actors_removal_of_a_profile_does_nothing() {
        let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
        writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
        writer.apply_from("pp", &pick("pureplay.player-1"));
        assert!(
            writer
                .follow_profiles("fh", &removed("someone-else", "pureplay.player-1"))
                .is_none()
        );
        assert_eq!(
            writer.turn().golfer.as_ref().map(GolferId::as_str),
            Some("pureplay.player-1")
        );
        // The sender's own removal still counts, and the record is gone.
        assert!(
            writer
                .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
                .is_some()
        );
        writer.apply_from("pp", &pick("pureplay.player-1"));
        assert!(
            writer
                .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
                .is_none(),
            "a removal of an id nobody holds is nothing"
        );
    }

    /// A second publisher of an id cannot take it away by removing its own
    /// copy: the golfer stays while the first publisher still holds it.
    #[test]
    fn a_second_publishers_removal_leaves_the_first_ones_golfer() {
        let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
        writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
        writer.apply_from("pp", &pick("pureplay.player-1"));
        writer.follow_profiles("fh", &profile("other", "pureplay.player-1"));
        assert!(
            writer
                .follow_profiles("fh", &removed("other", "pureplay.player-1"))
                .is_none()
        );
        assert_eq!(
            writer.turn().golfer.as_ref().map(GolferId::as_str),
            Some("pureplay.player-1")
        );
        assert!(
            writer
                .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
                .is_some(),
            "the last sender's removal counts"
        );
    }

    /// Two publishers of one id: the golfer is gone only once both removed it.
    #[test]
    fn a_golfer_held_by_two_senders_goes_when_both_removed_it() {
        let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
        writer.follow_profiles("fh", &profile("other", "pureplay.player-1"));
        writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
        writer.apply_from("pp", &pick("pureplay.player-1"));
        assert!(
            writer
                .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
                .is_none(),
            "still held by the other sender"
        );
        assert_eq!(
            writer.turn().golfer.as_ref().map(GolferId::as_str),
            Some("pureplay.player-1")
        );
        let back = writer
            .follow_profiles("fh", &removed("other", "pureplay.player-1"))
            .expect("gone with its last sender");
        assert_ne!(
            back.golfer.as_ref().map(GolferId::as_str),
            Some("pureplay.player-1")
        );
    }
}
