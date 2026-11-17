//! What the simulator outputs (the Open Connect client and the Pureplay
//! Connect output) share: the shots and readiness they take from the bus,
//! and the turn they hold for the simulator.
//!
//! - [`Feed`] follows flighthook's turn (its own `golfer.turn` only) and
//!   the launch monitors' `bus.actor`s, and lets a shot's `shot.finished`
//!   through when its launch monitor is the one routed ([`Routing`]) for
//!   the mode of flighthook's turn as the shot finishes. Routing names
//!   launch monitors by actor id; Auto-chip always passes; a game's launch
//!   monitor (the Pureplay Accessory card, any actor holding `game`) never
//!   does.
//! - [`RoutedView`] is what a software output's session sees of the launch
//!   monitors: the `bus.actor` of each one the [`Feed`] stands for in the
//!   current mode, and its removal when it leaves that view.
//! - [`Held`] is what a game or simulator card holds for the turn (club,
//!   hand and, when the card sets one, the mode). The card sends it whole in
//!   one `golfer.select` ([`Held::select`]) whenever its simulator reports
//!   it, and again whenever flighthook's turn differs from it, unless the
//!   turn's cause is a game or simulator card: between those the last
//!   sender wins ([`Held::reasserts`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use flightrelay::types::roles;
use flightrelay::{
    BusActor, FrpBody, FrpMessage, GolferSelect, GolferTurn, Handedness, Removal, StateKind,
};

use super::provenance::ShotSource;
use crate::bus::BusMsg;
use crate::state::SystemState;
use flighthook::{Club, Mode, SHOT_TIMEOUT};

/// A simulator output's launch monitor for each mode, by actor id; `None`
/// takes any.
#[derive(Debug, Clone, Default)]
pub struct Routing {
    pub full_monitor: Option<String>,
    pub chipping_monitor: Option<String>,
    pub putting_monitor: Option<String>,
}

/// What a message on the bus meant for an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeedEvent {
    /// A shot's `shot.finished` from its launch monitor, which the routing
    /// passes in the current mode.
    Finished,
    /// flighthook's turn changed, with its cause.
    Turn,
    /// A part of a shot before its finish (trigger, launch, club, impact, or
    /// its launch monitor's own flight) from a launch monitor that may feed
    /// a simulator; never Auto-chip's flight.
    Part,
}

/// The launch monitor of each shot (the sender of its `shot.trigger`).
/// A message carries only what its sender owns: a shot's launch, club,
/// impact and finish come from its launch monitor alone. From any other
/// actor they are dropped, logged once per actor. A flight may come from
/// anyone (a flight model's addendum).
#[derive(Debug, Default)]
pub(crate) struct ShotOwners {
    /// Shot id to its launch monitor and when its trigger arrived.
    owners: HashMap<String, (String, std::time::Instant)>,
    /// Actors whose shot parts were dropped (logged once each).
    warned: std::collections::HashSet<String>,
}

impl ShotOwners {
    /// Whether `msg` may be taken: false for a `shot.trigger`, `shot.launch`,
    /// `shot.club`, `shot.impact` or `shot.finished` of a shot whose trigger
    /// came from another actor. A shot is known for the shot timeout after
    /// its trigger.
    pub(crate) fn admits(
        &mut self,
        msg: &flightrelay::FrpMessage,
        now: std::time::Instant,
    ) -> bool {
        let Some(key) = msg.body.shot_key() else {
            return true;
        };
        if matches!(msg.body, FrpBody::ShotFlight(_)) {
            return true;
        }
        self.owners
            .retain(|_, (_, at)| now.saturating_duration_since(*at) <= SHOT_TIMEOUT);
        let owner = match self.owners.get(&key.shot_id) {
            Some((owner, _)) => owner,
            None if matches!(msg.body, FrpBody::ShotTrigger(_)) => {
                self.owners
                    .insert(key.shot_id.clone(), (msg.from.clone(), now));
                return true;
            }
            None => return true,
        };
        if *owner == msg.from {
            return true;
        }
        if self.warned.insert(msg.from.clone()) {
            tracing::warn!(
                "{} from {} for shot {} of {owner} dropped: only a shot's launch monitor \
                 sends its trigger, launch, club, impact and finish",
                msg.body.kind(),
                msg.from,
                key.shot_id
            );
        }
        false
    }

    /// The actor `actor` is gone: its warning may be logged again.
    pub(crate) fn forget_actor(&mut self, actor: &str) {
        self.warned.remove(actor);
    }

    /// The launch monitor of shot `shot_id`, while it is known.
    fn owner(&self, shot_id: &str) -> Option<&str> {
        self.owners.get(shot_id).map(|(owner, _)| owner.as_str())
    }
}

/// Shots and launch monitors from the bus for one simulator output.
pub(crate) struct Feed {
    pub routing: Routing,
    state: Option<Arc<SystemState>>,
    /// flighthook's actor id: only its turn counts.
    session: String,
    /// Auto-chip's actor id.
    auto_chip: String,
    owners: ShotOwners,
    /// flighthook's turn and the actor that caused it.
    pub turn: Option<(GolferTurn, Option<String>)>,
    /// Every launch monitor's `bus.actor`, by actor id.
    monitors: HashMap<String, BusActor>,
}

impl Feed {
    /// A feed with `routing`, reading the app's state (`None` in tests that
    /// run without one: flighthook's actor is `flighthook`, Auto-chip's
    /// `autochip`).
    pub(crate) fn new(routing: Routing, state: Option<Arc<SystemState>>) -> Self {
        let (session, auto_chip) = match &state {
            Some(s) => (s.flighthook_id(), s.auto_chip_id()),
            None => ("flighthook".into(), "autochip".into()),
        };
        Self {
            routing,
            state,
            session,
            auto_chip,
            owners: ShotOwners::default(),
            turn: None,
            monitors: HashMap::new(),
        }
    }

    /// The mode of flighthook's turn (full before any turn).
    pub(crate) fn mode(&self) -> Mode {
        self.turn
            .as_ref()
            .and_then(|(t, _)| t.mode)
            .unwrap_or(Mode::Full)
    }

    /// The routed launch monitor for `mode`, `None` for "Any".
    pub(crate) fn routed(&self, mode: Mode) -> Option<&str> {
        match mode {
            Mode::Full => self.routing.full_monitor.as_deref(),
            Mode::Chipping => self.routing.chipping_monitor.as_deref(),
            Mode::Putting => self.routing.putting_monitor.as_deref(),
        }
    }

    /// Whether actor `actor` may feed a simulator output: not a game's
    /// launch monitor (the Pureplay Accessory card, or any actor holding
    /// `game`), whose shots are the simulator's own.
    pub(crate) fn feeds_simulators(&self, actor: &str) -> bool {
        if self
            .monitors
            .get(actor)
            .is_some_and(|a| a.has_role(roles::GAME))
        {
            return false;
        }
        let Some(state) = &self.state else {
            return true;
        };
        let card = state.card_of_actor(actor);
        !(card
            .as_deref()
            .is_some_and(|c| c.starts_with("pureplay_accessory."))
            || state
                .sources
                .source(actor)
                .is_some_and(ShotSource::is_simulator))
    }

    /// Whether the routing passes a shot of `actor` in `mode`.
    pub(crate) fn passes(&self, actor: &str, mode: Mode) -> bool {
        if !self.feeds_simulators(actor) {
            return false;
        }
        if actor == self.auto_chip {
            return true;
        }
        self.routed(mode).is_none_or(|id| id == actor)
    }

    /// The launch monitor `actor`'s `bus.actor`, if it has published one.
    #[cfg(test)]
    pub(crate) fn monitor(&self, actor: &str) -> Option<&BusActor> {
        self.monitors.get(actor)
    }

    /// The launch monitors the output stands for in the current mode, by
    /// actor id: the routed one, or under "Any" every one that may feed a
    /// simulator; never Auto-chip, which is no readiness.
    pub(crate) fn in_view(&self) -> BTreeMap<String, BusActor> {
        let routed = self.routed(self.mode());
        self.monitors
            .iter()
            .filter(|(id, _)| {
                **id != self.auto_chip
                    && self.feeds_simulators(id)
                    && routed.is_none_or(|r| r == id.as_str())
            })
            .map(|(id, a)| (id.clone(), a.clone()))
            .collect()
    }

    /// Whether `msg` is a [`FeedEvent::Part`].
    fn is_part(&self, msg: &FrpMessage) -> bool {
        match &msg.body {
            FrpBody::ShotTrigger(_)
            | FrpBody::ShotLaunch(_)
            | FrpBody::ShotClub(_)
            | FrpBody::ShotImpact(_) => self.feeds_simulators(&msg.from),
            FrpBody::ShotFlight(f) => {
                msg.from != self.auto_chip
                    && self.feeds_simulators(&msg.from)
                    && self.owners.owner(&f.key.shot_id) == Some(msg.from.as_str())
            }
            _ => false,
        }
    }

    /// The remote actor `actor` is gone: its `bus.actor` is forgotten, as
    /// on its `bus.actor` removal.
    fn forget_actor(&mut self, actor: &str) {
        self.monitors.remove(actor);
        self.owners.forget_actor(actor);
    }

    /// Take one bus message.
    pub(crate) fn handle(&mut self, msg: &BusMsg) -> Option<FeedEvent> {
        let BusMsg::Frp { msg, cause, .. } = msg else {
            if let Some((_, crate::bus::LocalEvent::ActorGone { actor })) = msg.as_local() {
                self.forget_actor(actor);
            }
            return None;
        };
        if !self.owners.admits(msg, std::time::Instant::now()) {
            return None;
        }
        match &msg.body {
            FrpBody::GolferTurn(turn) => {
                if msg.from != self.session {
                    return None;
                }
                self.turn = Some((turn.clone(), cause.clone()));
                return Some(FeedEvent::Turn);
            }
            FrpBody::BusActor(actor) => {
                if actor.has_role(roles::LAUNCH_MONITOR) {
                    self.monitors.insert(msg.from.clone(), actor.clone());
                }
            }
            FrpBody::Removed(Removal {
                kind: StateKind::BusActor,
                ..
            }) => {
                self.monitors.remove(&msg.from);
            }
            FrpBody::ShotFinished(_) => {
                return self
                    .passes(&msg.from, self.mode())
                    .then_some(FeedEvent::Finished);
            }
            _ => {}
        }
        self.is_part(msg).then_some(FeedEvent::Part)
    }
}

/// What a software output's session sees of the launch monitors: the
/// `bus.actor` of each one in the feed's view ([`Feed::in_view`]), handed
/// again when it changes, and its removal once it leaves the view (to the
/// session, a monitor out of view is not on the bus).
#[derive(Debug, Default)]
pub(crate) struct RoutedView {
    /// What the session was handed last, by actor id.
    shown: BTreeMap<String, BusActor>,
}

impl RoutedView {
    /// Push into `to` what changed in `feed`'s view since the last update.
    pub(crate) fn update(&mut self, feed: &Feed, to: &mut Vec<FrpMessage>) {
        let view = feed.in_view();
        for id in self.shown.keys().filter(|id| !view.contains_key(*id)) {
            to.push(FrpMessage::new(
                id.clone(),
                Removal::of(StateKind::BusActor),
            ));
        }
        for (id, actor) in &view {
            if self.shown.get(id) != Some(actor) {
                to.push(FrpMessage::new(id.clone(), actor.clone()));
            }
        }
        self.shown = view;
    }

    /// A new session: it has been handed nothing yet.
    pub(crate) fn clear(&mut self) {
        self.shown.clear();
    }
}

/// What a game or simulator card holds for the turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Held {
    pub club: Option<Club>,
    pub hand: Option<Handedness>,
    /// The mode, when the card sets one.
    pub mode: Option<Mode>,
}

impl Held {
    /// Whether the card holds anything yet.
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The card's complete `golfer.select`: every value it holds.
    pub(crate) fn select(&self, r#ref: String) -> GolferSelect {
        GolferSelect {
            club: self.club.map(Some),
            handedness: self.hand,
            mode: self.mode,
            ..GolferSelect::new(r#ref)
        }
    }

    /// Whether `turn` differs from what the card holds, in a value it
    /// holds.
    pub(crate) fn differs(&self, turn: &GolferTurn) -> bool {
        self.club.is_some_and(|c| turn.club != Some(c))
            || self.hand.is_some_and(|h| turn.handedness != Some(h))
            || self.mode.is_some_and(|m| turn.mode != Some(m))
    }

    /// Whether the card sends its select again for `turn`, caused by
    /// `cause`: it holds something, the turn differs from it, and the change
    /// was not made by a game or simulator card (`is_game_card`).
    pub(crate) fn reasserts(
        &self,
        turn: &GolferTurn,
        cause: Option<&str>,
        is_game_card: impl Fn(&str) -> bool,
    ) -> bool {
        !self.is_empty() && self.differs(turn) && !cause.is_some_and(is_game_card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(club: Option<Club>, hand: Handedness, mode: Mode) -> GolferTurn {
        GolferTurn::new(None, club, hand, mode)
    }

    #[test]
    fn a_card_reasserts_only_against_changes_no_game_card_made() {
        let held = Held {
            club: Some(Club::Iron7),
            hand: Some(Handedness::Right),
            mode: Some(Mode::Full),
        };
        let header = turn(Some(Club::Wood5), Handedness::Right, Mode::Full);
        let game = |id: &str| id == "gspro";
        assert!(held.reasserts(&header, Some("flighthook"), game));
        assert!(!held.reasserts(&header, Some("gspro"), game));
        let same = turn(Some(Club::Iron7), Handedness::Right, Mode::Full);
        assert!(!held.reasserts(&same, Some("flighthook"), game));
        let mode = turn(Some(Club::Iron7), Handedness::Right, Mode::Chipping);
        assert!(held.reasserts(&mode, Some("flighthook"), game));
        assert!(!Held::default().reasserts(&header, None, game));
        let no_mode = Held {
            mode: None,
            ..held.clone()
        };
        assert!(!no_mode.reasserts(&mode, Some("flighthook"), game));
        let select = held.select("r".into());
        assert_eq!(select.club, Some(Some(Club::Iron7)));
        assert_eq!(select.handedness, Some(Handedness::Right));
        assert_eq!(select.mode, Some(Mode::Full));
        assert_eq!(select.golfer, None);
    }

    fn remote(msg: impl Into<FrpBody>, from: &str) -> BusMsg {
        BusMsg::from_peer(
            flightrelay::FrpMessage::new(from, msg),
            crate::bus::PeerId(5),
        )
    }

    fn ready_monitor() -> BusActor {
        BusActor::new("Trackball", 2.0).with_role(roles::LAUNCH_MONITOR)
    }

    /// GSPro's readiness drops once a remote launch monitor that went
    /// silent is gone (the system actor's tracker announces it).
    #[test]
    fn readiness_drops_when_a_silent_remote_monitor_is_gone() {
        let mut feed = Feed::new(Routing::default(), None);
        let mut remotes = crate::state::remotes::Remotes::new();
        let t0 = std::time::Instant::now();
        let msg = remote(ready_monitor(), "aaaa000000000001");
        if let BusMsg::Frp { msg: frp, .. } = &msg {
            remotes.hear(frp, t0);
        }
        feed.handle(&msg);
        assert!(feed.in_view().values().any(BusActor::is_clear));
        // Its program died: no removal, only silence.
        let gone = remotes.expire(t0 + std::time::Duration::from_secs(7));
        assert_eq!(gone, ["aaaa000000000001"]);
        for actor in gone {
            feed.handle(&BusMsg::local(
                "system",
                crate::bus::LocalEvent::ActorGone { actor },
            ));
        }
        assert!(feed.in_view().is_empty());
        assert!(feed.monitor("aaaa000000000001").is_none());
    }

    /// A shot's launch, club, impact and finish come from its launch
    /// monitor alone: another actor's are dropped, and so is a flight
    /// model's flight; the launch monitor's own flight is a part.
    #[test]
    fn only_the_shots_launch_monitor_sends_its_parts() {
        use flighthook::{FlightResult, Launch};
        use flightrelay::{ShotFinished, ShotFlight, ShotKey, ShotLaunch, ShotTrigger};
        let mut feed = Feed::new(Routing::default(), None);
        let key = ShotKey::new("s1", 1);
        let launch = |mps: f64| ShotLaunch {
            key: key.clone(),
            launch: Launch {
                launch_speed: Some(flighthook::Velocity::MetersPerSecond(mps)),
                ..Launch::default()
            },
        };
        feed.handle(&remote(ready_monitor(), "lm"));
        assert_eq!(
            feed.handle(&remote(ShotTrigger::new(key.clone()), "lm")),
            Some(FeedEvent::Part)
        );
        assert_eq!(feed.handle(&remote(launch(99.0), "intruder")), None);
        assert_eq!(
            feed.handle(&remote(ShotTrigger::new(key.clone()), "intruder")),
            None
        );
        assert!(
            feed.handle(&remote(ShotFinished { key: key.clone() }, "intruder"))
                .is_none(),
            "only the launch monitor finishes its shot"
        );
        assert_eq!(
            feed.handle(&remote(launch(50.0), "lm")),
            Some(FeedEvent::Part)
        );
        let flight = |from: &str| {
            remote(
                ShotFlight {
                    key: key.clone(),
                    flight: FlightResult::new(flightrelay::Basis::Modeled),
                },
                from,
            )
        };
        assert_eq!(feed.handle(&flight("model")), None);
        assert_eq!(feed.handle(&flight("lm")), Some(FeedEvent::Part));
        assert_eq!(
            feed.handle(&remote(ShotFinished { key }, "lm")),
            Some(FeedEvent::Finished),
            "the launch monitor's finish lets the shot through"
        );
    }
}
