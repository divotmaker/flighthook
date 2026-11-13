//! What the simulator outputs (the Open Connect client and the Pureplay
//! Connect output) share: the shots and readiness they take from the bus,
//! and the turn they hold for the simulator.
//!
//! - [`Feed`] composes each shot with the SDK's [`ShotAggregator`] (fed
//!   only flighthook's own `golfer.turn`) and lets a finished shot through
//!   when its launch monitor is the one routed for the mode of flighthook's
//!   turn as the shot finishes. Routing names launch monitors by actor id;
//!   Auto-chip always passes; a game's launch monitor (the Pureplay
//!   Accessory card, any actor holding `game`) never does. Readiness is the
//!   routed launch monitor's `bus.actor` with no blockers (under "Any": any
//!   launch monitor that may feed a simulator).
//! - [`Held`] is what a game or simulator card holds for the turn (club,
//!   hand and, when the card sets one, the mode). The card sends it whole in
//!   one `golfer.select` ([`Held::select`]) whenever its simulator reports
//!   it, and again whenever flighthook's turn differs from it, unless the
//!   turn's cause is a game or simulator card: between those the last
//!   sender wins ([`Held::reasserts`]).

use std::collections::HashMap;
use std::sync::Arc;

use flightrelay::types::roles;
use flightrelay::{
    BusActor, CompletedShot, FrpBody, GolferSelect, GolferTurn, Handedness, Removal,
    ShotAggregator, ShotEvent, StateKind,
};

use super::gspro::GsProRouting;
use super::provenance::ShotSource;
use crate::bus::BusMsg;
use crate::state::SystemState;
use flighthook::{Club, Mode, SHOT_TIMEOUT};
use flighthook::{ClubData, FaceImpact, FlightResult, Launch, Source};

/// A finished shot an output takes.
#[derive(Debug, Clone)]
pub(crate) struct FinishedShot {
    pub shot: CompletedShot,
}

/// What a message on the bus meant for an output.
#[derive(Debug, Clone)]
pub(crate) enum FeedEvent {
    /// A shot finished whose launch monitor the routing passes in the
    /// current mode.
    Shot(FinishedShot),
    /// A shot finished that the routing does not pass (for the log).
    Skipped(CompletedShot),
    /// A routed shot finished with no data an output sends: no launch,
    /// club or impact, and no flight of its own launch monitor (a missed
    /// read). Outputs never write it.
    NoData(CompletedShot),
    /// flighthook's turn changed, with its cause.
    Turn,
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
}

/// Shots and readiness from the bus for one simulator output.
pub(crate) struct Feed {
    pub routing: GsProRouting,
    state: Option<Arc<SystemState>>,
    /// flighthook's actor id: only its turn counts.
    session: String,
    /// Auto-chip's actor id.
    auto_chip: String,
    shots: ShotAggregator,
    owners: ShotOwners,
    /// flighthook's turn and the actor that caused it.
    pub turn: Option<(GolferTurn, Option<String>)>,
    /// Every launch monitor's `bus.actor`, by actor id.
    monitors: HashMap<String, BusActor>,
    /// Readiness may have changed since it was last taken.
    pub readiness_changed: bool,
}

impl Feed {
    /// A feed with `routing`, reading the app's state (`None` in tests that
    /// run without one: flighthook's actor is `flighthook`, Auto-chip's
    /// `autochip`).
    pub(crate) fn new(routing: GsProRouting, state: Option<Arc<SystemState>>) -> Self {
        let (session, auto_chip) = match &state {
            Some(s) => (s.flighthook_id(), s.auto_chip_id()),
            None => ("flighthook".into(), "autochip".into()),
        };
        Self {
            routing,
            state,
            session,
            auto_chip,
            shots: ShotAggregator::new().with_timeout(SHOT_TIMEOUT),
            owners: ShotOwners::default(),
            turn: None,
            monitors: HashMap::new(),
            readiness_changed: false,
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
                .round
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
    pub(crate) fn monitor(&self, actor: &str) -> Option<&BusActor> {
        self.monitors.get(actor)
    }

    /// Whether the simulator is told a launch monitor is ready in the
    /// current mode: the routed one has no blockers; under "Any", any
    /// launch monitor that may feed a simulator has none. `counts` limits
    /// which monitors count (a tournament round counts only measured ones).
    pub(crate) fn ready(&self, counts: impl Fn(&BusActor) -> bool) -> bool {
        let usable = |id: &str, a: &BusActor| {
            id != self.auto_chip && self.feeds_simulators(id) && counts(a) && a.is_clear()
        };
        match self.routed(self.mode()) {
            Some(id) => self.monitors.get(id).is_some_and(|a| usable(id, a)),
            None => self.monitors.iter().any(|(id, a)| usable(id, a)),
        }
    }

    /// The remote actor `actor` is gone: its `bus.actor` and its open
    /// shots are forgotten, as on its `bus.actor` removal.
    fn forget_actor(&mut self, actor: &str) {
        if self.monitors.remove(actor).is_some() {
            self.readiness_changed = true;
        }
        self.shots.forget_actor(actor);
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
                self.shots.feed(msg);
                self.turn = Some((turn.clone(), cause.clone()));
                self.readiness_changed = true;
                return Some(FeedEvent::Turn);
            }
            FrpBody::PracticeBlock(_) if msg.from != self.session => return None,
            FrpBody::BusActor(actor) => {
                if actor.has_role(roles::LAUNCH_MONITOR) {
                    self.monitors.insert(msg.from.clone(), actor.clone());
                    self.readiness_changed = true;
                }
            }
            FrpBody::Removed(Removal {
                kind: StateKind::BusActor,
                ..
            }) => {
                if self.monitors.remove(&msg.from).is_some() {
                    self.readiness_changed = true;
                }
            }
            _ => {}
        }
        match self.shots.feed(msg)? {
            ShotEvent::Finished(shot) => {
                if !self.passes(&shot.from, self.mode()) {
                    Some(FeedEvent::Skipped(shot))
                } else if output_shot(&shot, "", &self.auto_chip).has_data() {
                    Some(FeedEvent::Shot(FinishedShot { shot }))
                } else {
                    Some(FeedEvent::NoData(shot))
                }
            }
            _ => None,
        }
    }
}

/// A shot as a simulator output sends it: the launch, the launch
/// monitor's own flight, club and impact, numbered by the launch monitor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputShot {
    /// The launch monitor's card name, for the log.
    pub actor: String,
    pub shot_number: u32,
    pub shot_id: Option<String>,
    pub launch: Option<Launch>,
    /// The launch monitor's own flight result.
    pub flight: Option<FlightResult>,
    pub club: Option<ClubData>,
    pub impact: Option<FaceImpact>,
    /// What stands behind the shot, from the launch monitor's `bus.actor`.
    pub source: Option<Source>,
}

impl OutputShot {
    /// True when the shot carries launch, flight, club or impact data.
    #[must_use]
    pub fn has_data(&self) -> bool {
        self.launch.is_some()
            || self.flight.is_some()
            || self.club.is_some()
            || self.impact.is_some()
    }
}

/// The shot a simulator output sends outside a tournament round: the
/// launch for a receiver that flies the ball itself (measured values,
/// filled from the launch monitor's own flight's `assumed`), the launch
/// monitor's own flight (not Auto-chip's, `auto_chip`; never a flight
/// model's), club and impact. `card` names the launch monitor in the log.
pub(crate) fn output_shot(shot: &CompletedShot, card: &str, auto_chip: &str) -> OutputShot {
    OutputShot {
        actor: card.to_owned(),
        shot_number: shot.key.shot_number,
        shot_id: Some(shot.key.shot_id.clone()),
        launch: shot.launch_for_flight(),
        flight: (shot.from != auto_chip)
            .then(|| {
                shot.flights
                    .iter()
                    .find(|f| f.from == shot.from)
                    .map(|f| f.flight.clone())
            })
            .flatten(),
        club: shot.club.clone(),
        impact: shot.impact.clone(),
        source: shot.source,
    }
}

/// The shot as a measured-only receiver takes it (a tournament round): the
/// launch, club and impact as the launch monitor sent them, no flight, and
/// the launch monitor's declared source.
pub(crate) fn measured_shot(shot: &CompletedShot, card: &str) -> OutputShot {
    OutputShot {
        actor: card.to_owned(),
        shot_number: shot.key.shot_number,
        shot_id: Some(shot.key.shot_id.clone()),
        launch: shot.launch.clone(),
        club: shot.club.clone(),
        impact: shot.impact.clone(),
        source: shot.source,
        ..OutputShot::default()
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

/// The mode for a putt, a chip or a full swing from the club in play and the
/// distance to the hole before the shot, for a game that gives both: the
/// putter putts; a distance under [`CHIP_DISTANCE_M`] chips; anything else,
/// an unknown distance included, is a full swing. A distance of 0 or less
/// is unknown.
#[must_use]
pub(crate) fn mode_for_lie(club: Option<Club>, distance_m: Option<f64>) -> Mode {
    if club == Some(Club::Putter) {
        Mode::Putting
    } else if distance_m.is_some_and(|d| d.is_finite() && d > 0.0 && d < CHIP_DISTANCE_M) {
        Mode::Chipping
    } else {
        Mode::Full
    }
}

/// The distance to the hole under which a shot is a chip: 30 yd.
pub(crate) const CHIP_DISTANCE_M: f64 = 27.432;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lie_gives_the_mode() {
        assert_eq!(mode_for_lie(Some(Club::Putter), Some(150.0)), Mode::Putting);
        assert_eq!(
            mode_for_lie(Some(Club::SandWedge), Some(20.0)),
            Mode::Chipping
        );
        assert_eq!(
            mode_for_lie(Some(Club::SandWedge), Some(27.432)),
            Mode::Full
        );
        assert_eq!(mode_for_lie(Some(Club::Driver), None), Mode::Full);
        assert_eq!(mode_for_lie(Some(Club::SandWedge), Some(0.0)), Mode::Full);
        assert_eq!(mode_for_lie(Some(Club::SandWedge), Some(-5.0)), Mode::Full);
        assert_eq!(mode_for_lie(None, Some(5.0)), Mode::Chipping);
    }

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
        let mut feed = Feed::new(GsProRouting::default(), None);
        let mut remotes = crate::state::remotes::Remotes::new();
        let t0 = std::time::Instant::now();
        let msg = remote(ready_monitor(), "aaaa000000000001");
        if let BusMsg::Frp { msg: frp, .. } = &msg {
            remotes.hear(frp, t0);
        }
        feed.handle(&msg);
        assert!(feed.ready(|_| true));
        // Its program died: no removal, only silence.
        let gone = remotes.expire(t0 + std::time::Duration::from_secs(7));
        assert_eq!(gone, ["aaaa000000000001"]);
        feed.readiness_changed = false;
        for actor in gone {
            feed.handle(&BusMsg::local(
                "system",
                crate::bus::LocalEvent::ActorGone { actor },
            ));
        }
        assert!(feed.readiness_changed);
        assert!(!feed.ready(|_| true));
        assert!(feed.monitor("aaaa000000000001").is_none());
    }

    /// A shot's launch, club, impact and finish come from its launch
    /// monitor alone; another actor's are dropped, its flight is kept.
    #[test]
    fn only_the_shots_launch_monitor_sends_its_parts() {
        use flightrelay::{ShotFinished, ShotFlight, ShotKey, ShotLaunch, ShotTrigger};
        let mut feed = Feed::new(GsProRouting::default(), None);
        let key = ShotKey::new("s1", 1);
        let launch = |mps: f64| ShotLaunch {
            key: key.clone(),
            launch: Launch {
                launch_speed: Some(flighthook::Velocity::MetersPerSecond(mps)),
                ..Launch::default()
            },
        };
        feed.handle(&remote(ready_monitor(), "lm"));
        feed.handle(&remote(ShotTrigger::new(key.clone()), "lm"));
        feed.handle(&remote(launch(99.0), "intruder"));
        feed.handle(&remote(ShotTrigger::new(key.clone()), "intruder"));
        assert!(
            feed.handle(&remote(ShotFinished { key: key.clone() }, "intruder"))
                .is_none(),
            "only the launch monitor finishes its shot"
        );
        feed.handle(&remote(launch(50.0), "lm"));
        feed.handle(&remote(
            ShotFlight {
                key: key.clone(),
                flight: FlightResult::new(flightrelay::Basis::Modeled),
            },
            "model",
        ));
        let Some(FeedEvent::Shot(done)) = feed.handle(&remote(ShotFinished { key }, "lm")) else {
            panic!("the launch monitor's finish lets the shot through");
        };
        assert_eq!(
            done.shot.launch.as_ref().and_then(|l| l.launch_speed),
            Some(flighthook::Velocity::MetersPerSecond(50.0))
        );
        assert!(done.shot.flights.iter().any(|f| f.from == "model"));
    }
}
