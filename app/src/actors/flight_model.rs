//! The flight model: the Simulation card's FRP actor (beta).
//!
//! It flies every shot a launch monitor finishes and publishes what it
//! computed as a `shot.flight` addendum (`basis: modeled`, model
//! "flighthook <version> (dimple <version>)"), with only the result fields
//! it computed:
//!
//! - under `[simulation] mode = on`, the ones the launch monitor's own
//!   flight left empty, anchored on its measured values (an Auto-chip's
//!   own flight has every one, so it gets nothing), and the ones it wins
//!   over a measured value: a flown shot's carry or flight time measured
//!   as exactly 0, a ground ball's carry, apex and flight time (0), roll
//!   and offlines;
//! - under `force`, every result field;
//! - under `off` it is not on the bus: its `bus.actor` is removed and it
//!   publishes nothing.
//!
//! Its input is the shot's launch as a flight model takes it
//! ([`CompletedShot::launch_for_flight`]: the measured values, filled from
//! the launch monitor's own flight's `assumed`), so a launch-only chip with
//! the Mevo card's spin estimate flies. A ground ball rolls on the green
//! when the mode at the shot's trigger was putting (the launch monitor's
//! fixed mode while its card's thread runs, by
//! actor id). It publishes as soon as the launch monitor's `shot.finished`
//! arrives; nothing waits for it. The path goes out with the flight as the
//! local `ShotPath`, since FRP has no path field. A shot it can't fly, or
//! has no field to add to, gets nothing.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use flightrelay::types::roles;
use flightrelay::{
    BusActor, CompletedShot, DEFAULT_HEARTBEAT, FrpBody, FrpMessage, Removal, ShotAggregator,
    ShotEvent, ShotFlight, StateKind,
};
use tracing::info;

use crate::bus::{BusMsg, BusReceiver, BusSender, LocalEvent, PollError};
use crate::sim;
use crate::state::SystemState;
use flighthook::{SHOT_TIMEOUT, SimMode};

/// The flight model's card key (the Simulation card).
pub const CARD: &str = "simulation";

/// The flight model's actor name.
const NAME: &str = "Simulation";

/// How long the loop sleeps with nothing to do.
const IDLE: Duration = Duration::from_millis(20);

/// The flight model's `bus.actor`.
fn actor() -> BusActor {
    BusActor::new(NAME, DEFAULT_HEARTBEAT.as_secs_f64()).with_role(roles::FLIGHT_MODEL)
}

/// Start the flight model on its own thread, sending as `sender` (card
/// [`CARD`], the flight model's actor id). It ends at the bus's shutdown.
pub fn start(state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("flight-model".into())
        .spawn(move || run(&state, &sender, receiver))
        .expect("failed to spawn the flight model thread")
}

fn run(state: &SystemState, sender: &BusSender, mut receiver: BusReceiver) {
    let mut model = FlightModel::new(state, sender);
    loop {
        model.follow_mode();
        match receiver.poll() {
            Err(PollError::Shutdown) => return,
            Ok(None) => std::thread::sleep(IDLE),
            Ok(Some(BusMsg::Frp { msg, .. })) => model.handle(&msg),
            Ok(Some(BusMsg::Local {
                event: LocalEvent::ActorGone { actor },
                ..
            })) => model.shots.forget_actor(&actor),
            Ok(Some(_)) => {}
        }
    }
}

/// The flight model's state between bus messages.
struct FlightModel<'a> {
    state: &'a SystemState,
    sender: &'a BusSender,
    /// flighthook's own actor id: only its turn counts.
    session: String,
    /// The flight model's own actor id.
    own: String,
    shots: ShotAggregator,
    /// Each shot's launch monitor: only it sends the launch it flies.
    owners: super::simout::ShotOwners,
    /// Its `bus.actor` is published (the mode is not `off`).
    on_bus: bool,
}

impl<'a> FlightModel<'a> {
    fn new(state: &'a SystemState, sender: &'a BusSender) -> Self {
        Self {
            state,
            sender,
            session: state.flighthook_id(),
            own: sender.frp_id().unwrap_or_default().to_owned(),
            shots: ShotAggregator::new().with_timeout(SHOT_TIMEOUT),
            owners: super::simout::ShotOwners::default(),
            on_bus: false,
        }
    }

    /// Publish the `bus.actor` when the mode leaves `off`, and remove it
    /// when the mode becomes `off`.
    fn follow_mode(&mut self) {
        let wanted = self.state.system.simulation().mode != SimMode::Off;
        if wanted != self.on_bus {
            self.on_bus = wanted;
            if wanted {
                self.sender.frp(actor());
            } else {
                self.sender.frp(Removal::of(StateKind::BusActor));
            }
        }
    }

    fn handle(&mut self, msg: &FrpMessage) {
        match &msg.body {
            FrpBody::BusSync(_) if self.on_bus => self.sender.frp(actor()),
            // Only flighthook's own turn counts.
            FrpBody::GolferTurn(_) | FrpBody::PracticeBlock(_) if msg.from != self.session => {
                return;
            }
            _ => {}
        }
        if msg.from == self.own || !self.owners.admits(msg, std::time::Instant::now()) {
            return;
        }
        if let Some(ShotEvent::Finished(shot)) = self.shots.feed(msg)
            && self.on_bus
        {
            fly(self.state, self.sender, &shot);
        }
    }
}

/// Fly `shot` and publish its flight result with its path. Nothing goes
/// out when there is no result field to add.
fn fly(state: &SystemState, sender: &BusSender, shot: &CompletedShot) {
    let Some(launch) = shot.launch_for_flight() else {
        return;
    };
    let cfg = state.system.simulation();
    let device = shot
        .flights
        .iter()
        .find(|f| f.from == shot.from)
        .map(|f| &f.flight);
    let mode = state
        .fixed_mode(&shot.from)
        .or_else(|| shot.turn.as_ref().and_then(|t| t.mode));
    let Some(modeled) = sim::model(&cfg, &launch, device, mode, &sim::model_name()) else {
        return;
    };
    let Some(flight) = modeled.flight else {
        return;
    };
    info!("  {}", sim::log_line(&modeled.derived));
    if let Some(path) = modeled.path {
        sender.local(LocalEvent::ShotPath {
            key: shot.key.clone(),
            path,
        });
    }
    sender.frp(ShotFlight {
        key: shot.key.clone(),
        flight,
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use flighthook::{Mode, SimulationSection};
    use flightrelay::{
        FlightResult, Launch, ShotFinished, ShotKey, ShotLaunch, ShotTrigger, Velocity,
    };
    use tokio::sync::broadcast;

    use super::*;

    const MONITOR: &str = "monitor-actor-id";

    struct Bench {
        state: SystemState,
        sender: BusSender,
        rx: broadcast::Receiver<BusMsg>,
        _dir: std::path::PathBuf,
    }

    fn bench(mode: SimMode) -> Bench {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-flight-model-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        set_mode(&state, mode);
        let (tx, rx) = broadcast::channel(64);
        let sender = BusSender::new(CARD.into(), tx, Arc::new(AtomicBool::new(false)))
            .with_frp_id(Some(state.flight_model_id()));
        Bench {
            state,
            sender,
            rx,
            _dir: dir,
        }
    }

    fn set_mode(state: &SystemState, mode: SimMode) {
        state.system.update(|c| c.simulation.mode = mode);
    }

    fn drain(rx: &mut broadcast::Receiver<BusMsg>) -> Vec<BusMsg> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn key() -> ShotKey {
        ShotKey {
            shot_id: "shot-1".into(),
            shot_number: 1,
        }
    }

    fn launch() -> Launch {
        Launch {
            launch_speed: Some(Velocity::MilesPerHour(140.0)),
            launch_elevation: Some(14.0),
            launch_azimuth: Some(0.0),
            backspin_rpm: Some(3500),
            sidespin_rpm: Some(0),
            ..Launch::default()
        }
    }

    /// A launch monitor's shot from `from`: trigger, launch, its own flight
    /// when it has one, finished.
    fn shot(from: &str, launch: Launch, own: Option<FlightResult>) -> Vec<FrpMessage> {
        let mut out = vec![
            FrpMessage::new(from, ShotTrigger::new(key())),
            FrpMessage::new(from, ShotLaunch { key: key(), launch }),
        ];
        if let Some(flight) = own {
            out.push(FrpMessage::new(from, ShotFlight { key: key(), flight }));
        }
        out.push(FrpMessage::new(from, ShotFinished { key: key() }));
        out
    }

    /// The flight model's flight and whether a path went out.
    fn outcome(seen: &[BusMsg]) -> (Option<FlightResult>, bool) {
        let flight = seen.iter().find_map(|m| match m.as_frp().map(|f| &f.body) {
            Some(FrpBody::ShotFlight(f)) => Some(f.flight.clone()),
            _ => None,
        });
        let path = seen
            .iter()
            .any(|m| matches!(m.as_local(), Some((_, LocalEvent::ShotPath { .. }))));
        (flight, path)
    }

    fn run_shot(
        b: &mut Bench,
        from: &str,
        own: Option<FlightResult>,
    ) -> (Option<FlightResult>, bool) {
        let mut model = FlightModel::new(&b.state, &b.sender);
        model.follow_mode();
        for msg in shot(from, launch(), own) {
            model.handle(&msg);
        }
        outcome(&drain(&mut b.rx))
    }

    /// `off` removes the flight model's `bus.actor` and it flies nothing;
    /// back on, it publishes its `bus.actor` again and flies.
    #[test]
    fn off_removes_the_actor_and_back_on_publishes_it() {
        let mut b = bench(SimMode::On);
        let mut model = FlightModel::new(&b.state, &b.sender);
        model.follow_mode();
        let seen = drain(&mut b.rx);
        assert!(matches!(
            seen[0].as_frp().map(|m| &m.body),
            Some(FrpBody::BusActor(_))
        ));
        set_mode(&b.state, SimMode::Off);
        model.follow_mode();
        let seen = drain(&mut b.rx);
        assert!(matches!(
            seen[0].as_frp().map(|m| &m.body),
            Some(FrpBody::Removed(Removal {
                kind: StateKind::BusActor,
                ..
            }))
        ));
        for msg in shot(MONITOR, launch(), None) {
            model.handle(&msg);
        }
        assert!(drain(&mut b.rx).is_empty(), "nothing while off");

        set_mode(&b.state, SimMode::On);
        model.follow_mode();
        let seen = drain(&mut b.rx);
        assert!(matches!(
            seen[0].as_frp().map(|m| &m.body),
            Some(FrpBody::BusActor(_))
        ));
        let other = ShotKey {
            shot_id: "shot-2".into(),
            shot_number: 2,
        };
        for body in [
            FrpBody::from(ShotTrigger::new(other.clone())),
            ShotLaunch {
                key: other.clone(),
                launch: launch(),
            }
            .into(),
            ShotFinished { key: other }.into(),
        ] {
            model.handle(&FrpMessage::new(MONITOR, body));
        }
        let (flight, path) = outcome(&drain(&mut b.rx));
        assert!(flight.is_some() && path, "flies again");
    }

    /// The flight model's flight and path for a shot with `launch` and the
    /// launch monitor's own flight `own`.
    fn modeled(
        b: &mut Bench,
        launch: Launch,
        own: FlightResult,
    ) -> (FlightResult, flighthook::FlightPath) {
        let mut model = FlightModel::new(&b.state, &b.sender);
        model.follow_mode();
        for msg in shot(MONITOR, launch, Some(own)) {
            model.handle(&msg);
        }
        let seen = drain(&mut b.rx);
        let path = seen.iter().find_map(|m| match m.as_local() {
            Some((_, LocalEvent::ShotPath { path, .. })) => Some(path.clone()),
            _ => None,
        });
        (outcome(&seen).0.expect("a flight"), path.expect("a path"))
    }

    fn m(d: Option<flightrelay::Distance>) -> f64 {
        d.expect("a distance").as_meters()
    }

    /// Under `on` a flown shot whose carry and flight time were measured as
    /// 0 shows the simulated ones (decisions, "Build-time calls"); the
    /// total is that carry plus the roll and the path lands at that carry.
    #[test]
    fn on_wins_over_a_measured_zero_carry() {
        let mut b = bench(SimMode::On);
        let own = FlightResult {
            carry_distance: Some(flightrelay::Distance::Meters(0.0)),
            flight_time: Some(0.0),
            max_height: Some(flightrelay::Distance::Meters(25.0)),
            ..FlightResult::new(flightrelay::Basis::Tracked)
        };
        let (flight, path) = modeled(&mut b, launch(), own);
        let carry = m(flight.carry_distance);
        assert!(carry > 100.0, "{carry}");
        assert!(flight.flight_time.is_some_and(|t| t > 1.0));
        assert_eq!(flight.max_height, None, "the measured apex stays");
        let total = m(flight.total_distance);
        let roll = m(flight.roll_distance);
        assert!(
            (carry + roll - total).abs() < 0.01,
            "{carry} + {roll} != {total}"
        );
        let landing = path.flight.last().expect("landing");
        assert!(
            (landing[1].hypot(landing[3]) - carry).abs() < 0.5,
            "{landing:?} vs {carry}"
        );
        let rest = path.ground.last().expect("rest");
        assert!(
            (rest[1].hypot(rest[3]) - total).abs() < 0.5,
            "{rest:?} vs {total}"
        );
    }

    /// A ground ball shows carry, apex and flight time 0 and its roll as
    /// its total, over a device's carry and time, as today.
    #[test]
    fn a_ground_ball_shows_zero_carry_over_a_measured_one() {
        let mut b = bench(SimMode::On);
        let ground = Launch {
            launch_speed: Some(Velocity::MilesPerHour(8.0)),
            launch_elevation: Some(0.0),
            launch_azimuth: Some(1.0),
            ..Launch::default()
        };
        let own = FlightResult {
            carry_distance: Some(flightrelay::Distance::Meters(4.0)),
            flight_time: Some(0.3),
            ..FlightResult::new(flightrelay::Basis::Tracked)
        };
        let (flight, path) = modeled(&mut b, ground, own);
        assert_eq!(m(flight.carry_distance), 0.0);
        assert_eq!(flight.flight_time, Some(0.0));
        assert_eq!(m(flight.max_height), 0.0);
        let total = m(flight.total_distance);
        assert!(total > 0.0);
        assert_eq!(m(flight.roll_distance), total);
        assert_eq!(path.flight, vec![[0.0; 4]], "rolled from the tee");
    }

    /// Under `on` a launch-only shot gets every result field; a partial own
    /// flight gets only the fields it lacks; under `force` every field.
    #[test]
    fn on_adds_only_missing_fields_and_force_adds_every_field() {
        let mut b = bench(SimMode::On);
        let (all, path) = run_shot(&mut b, MONITOR, None);
        let all = all.expect("a flight");
        assert!(path);
        assert!(all.carry_distance.is_some() && all.total_offline.is_some());

        let own = FlightResult {
            carry_distance: Some(flightrelay::Distance::Meters(150.0)),
            total_offline: Some(flightrelay::Distance::Meters(1.0)),
            ..FlightResult::new(flightrelay::Basis::Tracked)
        };
        let (on, _) = run_shot(&mut b, MONITOR, Some(own.clone()));
        let on = on.expect("a flight");
        assert_eq!(on.carry_distance, None);
        assert_eq!(on.total_offline, None);
        assert!(on.total_distance.is_some());

        set_mode(&b.state, SimMode::Force);
        let (forced, _) = run_shot(&mut b, MONITOR, Some(own));
        let forced = forced.expect("a flight");
        assert!(forced.carry_distance.is_some() && forced.total_offline.is_some());
    }

    /// A launch monitor whose own flight has every field the model computes
    /// gets no addendum and no path under `on`.
    #[test]
    fn nothing_to_add_sends_no_flight_and_no_path() {
        let mut b = bench(SimMode::On);
        let cfg = b.state.system.simulation();
        let mut own = sim::model(&cfg, &launch(), None, None, "own")
            .and_then(|m| m.flight)
            .expect("flown");
        // Values that differ from the model's still count as present.
        own.total_offline = Some(flightrelay::Distance::Meters(-3.0));
        own.basis = flightrelay::Basis::Tracked;
        assert_eq!(run_shot(&mut b, MONITOR, Some(own)), (None, false));
    }

    /// An Auto-chip's own flight has every field: under `on` the flight
    /// model sends nothing for it, `total_offline` included.
    #[test]
    fn an_auto_chip_gets_nothing_under_on() {
        let mut b = bench(SimMode::On);
        let cfg: SimulationSection = b.state.system.simulation();
        let ball = crate::autochip::ball(30.0);
        let own = sim::model(
            &cfg,
            &ball,
            None,
            Some(Mode::Chipping),
            crate::autochip::MODEL,
        )
        .and_then(|m| m.flight)
        .expect("an Auto-chip flight");
        let chip = b.state.auto_chip_id();
        let mut model = FlightModel::new(&b.state, &b.sender);
        model.follow_mode();
        for msg in shot(&chip, ball, Some(own)) {
            model.handle(&msg);
        }
        let seen = drain(&mut b.rx);
        assert_eq!(outcome(&seen), (None, false));
    }

    /// A launch monitor's fixed mode goes with its card's thread (the
    /// guard), never with a `bus.actor` removal: an old instance's removal
    /// read after a restarted card registered its mode leaves that mode.
    #[test]
    fn a_fixed_mode_survives_an_old_instances_removal() {
        let b = bench(SimMode::On);
        let old = b.state.set_fixed_mode(MONITOR, Mode::Putting);
        let new = b.state.set_fixed_mode(MONITOR, Mode::Putting);
        drop(old);
        let mut model = FlightModel::new(&b.state, &b.sender);
        model.handle(&FrpMessage::new(MONITOR, Removal::of(StateKind::BusActor)));
        assert_eq!(
            b.state.fixed_mode(MONITOR),
            Some(Mode::Putting),
            "the restarted card's mode stays"
        );
        drop(new);
        assert_eq!(b.state.fixed_mode(MONITOR), None);
    }
}
