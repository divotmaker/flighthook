//! The FlightScope card (Mevo+, Mevo Gen2, Xi): ironsight's device driver
//! ([`Mevo`]) on [`DriverActor`].
//!
//! The driver owns the device: it connects over TCP, runs the handshake,
//! configures and arms the device for the detected model in the detection
//! mode in effect, follows mode changes (a reconfigure and re-arm when the
//! change reaches the device), applies the Fusion camera config after the
//! camera's warmup, and reports readiness, tilt, roll, battery, shots and
//! the frames of each shot. The ball goes out as soon as the device's full
//! result arrives. The Xi's spin source is applied live, without a new
//! session; any other setting change takes a new session (a restart).
//!
//! The card adds its section ([`MevoSection`]), its shot source and the
//! **launch-only spin estimate** (beta): when the device reports a launch
//! with speed and launch angle but no spin and sends no flight result (very
//! short chips on the Xi), the card publishes its own flight result just
//! before the shot's `shot.finished`: `basis: modeled`, model
//! [`LAUNCH_ONLY_MODEL`], with only `assumed` holding a backspin of
//! `launch_only_spin` rpm per mph of ball speed and no sidespin. The launch
//! itself carries only what the device measured. A flight model and the
//! simulator outputs take the assumed spin as the launch's
//! (`CompletedShot::launch_for_flight`). Not for a shot whose mode at the
//! trigger was Putting, a launch under [`LAUNCH_ONLY_MIN_LAUNCH_DEG`] (it
//! rolls as a ground ball), or a `launch_only_spin` of 0.

use flighthook::{AssumedLaunch, Basis, FlightResult, Launch, MevoSection, Mode};
use flightrelay::{FrpBody, ShotFlight};
use ironsight::driver::Mevo;
use ironsight::driver::convert::LAUNCH_ONLY_MIN_LAUNCH_DEG;
use tracing::info;

use crate::actors::driver::{DriverActor, HookCtx, Hooks, ShotSoFar};
use crate::actors::provenance::ShotSource;

/// The model named on the card's launch-only spin estimate.
pub const LAUNCH_ONLY_MODEL: &str = "flighthook launch-only spin";

/// The FlightScope card: the launch-only spin estimate.
#[derive(Debug, Default)]
pub struct MevoHooks;

impl Hooks<Mevo> for MevoHooks {
    type Section = MevoSection;
    const SOURCE: ShotSource = ShotSource::Mevo;

    fn before_finished(
        &mut self,
        shot: &ShotSoFar,
        ctx: &mut HookCtx<'_, MevoSection>,
    ) -> Vec<FrpBody> {
        estimate(shot, ctx.section).into_iter().collect()
    }
}

/// The card's launch-only spin estimate for `shot`, as its own flight
/// result, when it applies.
fn estimate(shot: &ShotSoFar, section: &MevoSection) -> Option<FrpBody> {
    if shot.flight.is_some() {
        return None;
    }
    let rpm_per_mph = section.launch_only_spin_rpm_per_mph();
    let back = launch_only_spin(shot.launch.as_ref()?, rpm_per_mph, shot.mode)?;
    info!(
        "shot #{}: launch only, estimated backspin {back} rpm ({rpm_per_mph} rpm per mph)",
        shot.key.shot_number,
    );
    Some(
        ShotFlight {
            key: shot.key.clone(),
            flight: FlightResult {
                model: Some(LAUNCH_ONLY_MODEL.into()),
                assumed: Some(AssumedLaunch {
                    backspin_rpm: Some(back),
                    sidespin_rpm: Some(0),
                    ..AssumedLaunch::default()
                }),
                ..FlightResult::new(Basis::Modeled)
            },
        }
        .into(),
    )
}

/// The estimated backspin (rpm) for `launch`, a launch the device sent no
/// flight result for: `rpm_per_mph` times its ball speed in mph, rounded.
/// `None` (no estimate) when `rpm_per_mph` is not above 0, `mode` (the mode
/// at the trigger) is Putting, the launch has any spin, its launch angle is
/// under [`LAUNCH_ONLY_MIN_LAUNCH_DEG`] or unknown, or its speed is not
/// positive or the product is out of range for an rpm.
fn launch_only_spin(launch: &Launch, rpm_per_mph: f64, mode: Option<Mode>) -> Option<i32> {
    if rpm_per_mph <= 0.0 || mode == Some(Mode::Putting) {
        return None;
    }
    if launch.backspin_rpm.unwrap_or(0) != 0
        || launch.sidespin_rpm.unwrap_or(0) != 0
        || launch.total_spin_rpm.unwrap_or(0) != 0
    {
        return None;
    }
    if !launch
        .launch_elevation
        .is_some_and(|vla| vla >= LAUNCH_ONLY_MIN_LAUNCH_DEG)
    {
        return None;
    }
    let mph = launch.launch_speed?.as_mph();
    if !(mph.is_finite() && mph > 0.0) {
        return None;
    }
    let rpm = (rpm_per_mph * mph).round();
    if rpm > f64::from(i32::MAX) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)] // 0 to i32::MAX, checked above
    Some(rpm as i32)
}

/// The FlightScope actor (`mevo.N`).
pub type MevoActor = DriverActor<Mevo, MevoHooks>;

#[cfg(test)]
use ironsight as iron;
#[cfg(test)]
mod scenario;

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use flighthook::{ActorStatus, Distance, Launch, MevoConfig, ShotKey, SpinSource};
    use flightrelay::config::{ConfigError, DriverConfig};
    use flightrelay::driver::{
        ConnectCtx, DeviceDriver, DeviceIdentity, DeviceSession, DriverInfo, Events, NoAction,
    };
    use flightrelay::{FrpMessage, GolferSelect, ShotFinished, ShotLaunch, ShotTrigger};
    use ironsight::protocol::shot::{FlightResult as D4, FlightResultV1};
    use ironsight::testing::{Shot, fixtures};
    use tokio::sync::broadcast;

    use super::scenario::{Bench, Do, Model, section};
    use super::*;
    use crate::actors::scripted::Handoff;
    use crate::actors::{Actor, ReconfigureOutcome};
    use crate::bus::{BusMsg, BusSender, LocalEvent};
    use crate::state::SystemState;
    use crate::state::session::SessionWriter;

    // -- A replayed device ----------------------------------------------------

    /// What a replayed device does next.
    enum Step {
        /// These messages, in one poll.
        Emit(Vec<FrpBody>),
        /// Nothing, until the flag is set.
        Hold(Arc<AtomicBool>),
    }

    /// Replays waiting for a driver, by the card's `address`: each test
    /// runs with its own (see [`Handoff`]).
    static REPLAYS: Handoff<String, Vec<Step>> = Handoff::new();

    /// A FlightScope driver that replays a list of messages as ironsight's
    /// session emits them: for the card's own processing of shots the
    /// scripted device cannot produce (a launch-only chip).
    struct ReplayMevo {
        config: MevoConfig,
        steps: VecDeque<Step>,
    }

    struct ReplaySession {
        identity: DeviceIdentity,
        steps: VecDeque<Step>,
    }

    impl DeviceDriver for ReplayMevo {
        type Config = MevoConfig;
        type Error = ironsight::driver::Error;
        type Action = NoAction;
        type Session = ReplaySession;
        const STOP_WAIT: Duration = Mevo::STOP_WAIT;

        fn info(config: &MevoConfig) -> DriverInfo {
            Mevo::info(config)
        }

        fn new(config: MevoConfig) -> Result<Self, ConfigError> {
            config.validate()?;
            let steps = REPLAYS
                .take(&config.address.clone().unwrap_or_default())
                .unwrap_or_default();
            Ok(Self {
                config,
                steps: steps.into(),
            })
        }

        fn config(&self) -> &MevoConfig {
            &self.config
        }

        fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<ReplaySession, Self::Error> {
            ctx.tick()?;
            Ok(ReplaySession {
                identity: DeviceIdentity::new("FS M2-REPLAY")
                    .with_manufacturer("FlightScope")
                    .with_model("Mevo+"),
                steps: std::mem::take(&mut self.steps),
            })
        }

        fn apply_config(&mut self, new: &MevoConfig) {
            self.config = new.clone();
        }
    }

    impl DeviceSession for ReplaySession {
        type Driver = ReplayMevo;

        fn poll(
            &mut self,
            timeout: Duration,
            out: &mut Events<'_>,
        ) -> Result<(), ironsight::driver::Error> {
            match self.steps.front() {
                Some(Step::Emit(_)) => {
                    if let Some(Step::Emit(bodies)) = self.steps.pop_front() {
                        for body in bodies {
                            out.frp(body);
                        }
                    }
                }
                Some(Step::Hold(flag)) if flag.load(Ordering::SeqCst) => {
                    self.steps.pop_front();
                }
                _ => std::thread::sleep(timeout.min(Duration::from_millis(5))),
            }
            Ok(())
        }

        fn identity(&self) -> &DeviceIdentity {
            &self.identity
        }

        fn close(self, out: &mut Events<'_>) -> Result<(), ironsight::driver::Error> {
            out.ready(false);
            Ok(())
        }
    }

    impl Hooks<ReplayMevo> for MevoHooks {
        type Section = MevoSection;
        const SOURCE: ShotSource = ShotSource::Mevo;

        fn before_finished(
            &mut self,
            shot: &ShotSoFar,
            ctx: &mut HookCtx<'_, MevoSection>,
        ) -> Vec<FrpBody> {
            <Self as Hooks<Mevo>>::before_finished(self, shot, ctx)
        }
    }

    /// A running replay card (`mevo.0`), the session it reads and the bus.
    struct Replay {
        state: Arc<SystemState>,
        writer: SessionWriter,
        actor: Arc<DriverActor<ReplayMevo, MevoHooks>>,
        sender: BusSender,
        rx: broadcast::Receiver<BusMsg>,
        _dir: crate::actors::web::tests::TempDir,
    }

    /// A card with `section` (its address the replay's key) replaying
    /// `steps`, the session in `mode`.
    fn replay(section: MevoSection, mode: Mode, steps: Vec<Step>) -> Replay {
        REPLAYS.put(section.driver.address.clone().unwrap_or_default(), steps);
        let dir = std::env::temp_dir().join(format!(
            "flighthook-mevo-replay-{}",
            crate::state::config::generate_id()
        ));
        let (state, writer) = SystemState::new(dir.join("config.toml"));
        writer.apply(&select(mode));
        state.system.update(|c| {
            c.mevo.insert("0".into(), section.clone());
        });
        let state = Arc::new(state);
        let (tx, rx) = broadcast::channel(1024);
        let sender =
            BusSender::new("mevo.0".into(), tx, Arc::default()).with_frp_id(Some("mevo.0".into()));
        let actor = Arc::new(DriverActor::<ReplayMevo, MevoHooks>::new(section));
        actor.start(Arc::clone(&state), sender.clone(), sender.subscribe());
        Replay {
            state,
            writer,
            actor,
            sender,
            rx,
            _dir: crate::actors::web::tests::TempDir(dir),
        }
    }

    fn select(mode: Mode) -> GolferSelect {
        GolferSelect {
            mode: Some(mode),
            ..GolferSelect::new("test")
        }
    }

    fn body(m: &BusMsg) -> Option<&FrpBody> {
        m.as_frp().map(|f: &FrpMessage| &f.body)
    }

    impl Replay {
        /// Messages until one matches `want`, within 5 s.
        fn until(&mut self, mut want: impl FnMut(&BusMsg) -> bool) -> Vec<BusMsg> {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut seen = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "timed out: {} seen", seen.len());
                match self.rx.try_recv() {
                    Ok(m) => {
                        let done = want(&m);
                        seen.push(m);
                        if done {
                            return seen;
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }

        /// The messages of the next finished shot, up to its end.
        fn shot(&mut self) -> Vec<BusMsg> {
            self.until(|m| matches!(body(m), Some(FrpBody::ShotFinished(_))))
        }

        /// The session's mode changes: the session publishes its turn.
        fn turn_to(&self, mode: Mode) {
            let turn = self
                .writer
                .apply(&select(mode))
                .unwrap_or_else(|| self.writer.turn());
            self.sender
                .for_card("system", Some(self.state.flighthook_id()))
                .frp(turn);
        }

        /// The messages of the next [`launch_only_shot`]: up to its launch,
        /// which is on the bus before the device finishes the shot, then,
        /// once `finish` is set, up to its end.
        fn launch_only(&mut self, finish: &AtomicBool) -> Vec<BusMsg> {
            let mut seen = self.until(|m| matches!(body(m), Some(FrpBody::ShotLaunch(_))));
            assert!(
                !seen
                    .iter()
                    .any(|m| matches!(body(m), Some(FrpBody::ShotFinished(_)))),
                "the ball goes out before the device finishes the shot"
            );
            finish.store(true, Ordering::SeqCst);
            seen.extend(self.shot());
            seen
        }
    }

    impl Drop for Replay {
        fn drop(&mut self) {
            self.actor.stop();
        }
    }

    /// A card section replaying under `key` (an address no other test uses).
    fn replay_section(key: &str) -> MevoSection {
        MevoSection {
            driver: MevoConfig {
                address: Some(key.into()),
                ..MevoConfig::default()
            },
            ..MevoSection::default()
        }
    }

    fn shot_key(n: u32) -> ShotKey {
        ShotKey::new(format!("replay-{n}"), n)
    }

    fn launch_body(n: u32, launch: Launch) -> FrpBody {
        ShotLaunch {
            key: shot_key(n),
            launch,
        }
        .into()
    }

    fn finished_body(n: u32) -> FrpBody {
        ShotFinished { key: shot_key(n) }.into()
    }

    /// Shot `n` as ironsight's session emits it: the trigger, a hold until
    /// `go` when given, then `rest`.
    fn replayed_shot(n: u32, go: Option<Arc<AtomicBool>>, rest: Vec<FrpBody>) -> Vec<Step> {
        let mut trigger = ShotTrigger::new(shot_key(n));
        trigger.timestamp = Some(now_ms());
        let mut steps = vec![Step::Emit(vec![trigger.into()])];
        if let Some(go) = go {
            steps.push(Step::Hold(go));
        }
        steps.push(Step::Emit(rest));
        steps
    }

    fn now_ms() -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_millis(),
        )
        .expect("ms")
    }

    /// Shot `n` as ironsight's session emits a launch-only full result: the
    /// trigger, then `launch` with no flight result, then `shot.finished`
    /// only once `finish` is set (the device finishes the shot after its
    /// re-arm, up to seconds later).
    fn launch_only_shot(n: u32, launch: Launch, finish: &Arc<AtomicBool>) -> Vec<Step> {
        let mut steps = replayed_shot(n, None, vec![launch_body(n, launch)]);
        steps.push(Step::Hold(Arc::clone(finish)));
        steps.push(Step::Emit(vec![finished_body(n)]));
        steps
    }

    /// The launch-only Xi chip: 7.019 m/s (15.7 mph) at 22.8°, 0.856° left,
    /// no spin, no carry, apex or flight time.
    fn launch_only_d4() -> D4 {
        D4 {
            total: 1,
            track_time: 0.0,
            start_position: [0.0; 3],
            launch_speed: 7.019,
            // Wire sign: positive = left.
            launch_azimuth: 0.856,
            launch_elevation: 22.8,
            carry_distance: 0.0,
            flight_time: 0.0,
            max_height: 0.0,
            landing_position: [0.0; 3],
            backspin_rpm: 0,
            sidespin_rpm: 0,
            riflespin_rpm: 0,
            landing_spin_rpm: [0; 3],
            landing_velocity: [0.0; 3],
            total_distance: 0.0,
            roll_distance: 0.0,
            final_position: [0.0; 3],
            clubhead_speed: 0.0,
            club_strike_direction: 0.0,
            club_attack_angle: 0.0,
            clubhead_speed_post: 0.0,
            club_swing_plane_tilt: 0.0,
            club_swing_plane_rotation: 0.0,
            club_effective_loft: 0.0,
            club_face_angle: 0.0,
            poly_scale: 0,
            poly_x: [0.0; 5],
            poly_y: [0.0; 5],
            poly_z: [0.0; 5],
        }
    }

    /// The launch ironsight's session sends for a launch-only full result
    /// `d4`: its launch with no spin fields, and no flight result.
    fn launch_only_launch(d4: &D4) -> Launch {
        assert!(ironsight::driver::convert::launch_only(d4));
        Launch {
            backspin_rpm: None,
            sidespin_rpm: None,
            total_spin_rpm: None,
            spin_axis: None,
            ..ironsight::driver::convert::launch(d4)
        }
    }

    /// The launch on the bus in `seen`.
    fn launch_of(seen: &[BusMsg]) -> Launch {
        seen.iter()
            .find_map(|m| match body(m) {
                Some(FrpBody::ShotLaunch(l)) => Some(l.launch.clone()),
                _ => None,
            })
            .expect("a launch")
    }

    /// The card's launch-only estimate in `seen`: its assumed backspin and
    /// sidespin. Checks it is on the bus before the shot's `shot.finished`.
    fn estimate_of(seen: &[BusMsg]) -> Option<(Option<i32>, Option<i32>)> {
        let at = seen.iter().position(|m| {
            matches!(body(m), Some(FrpBody::ShotFlight(f))
                if f.flight.model.as_deref() == Some(LAUNCH_ONLY_MODEL))
        })?;
        let finished = seen
            .iter()
            .position(|m| matches!(body(m), Some(FrpBody::ShotFinished(_))))
            .expect("finished");
        assert!(at < finished, "the estimate comes before shot.finished");
        let Some(FrpBody::ShotFlight(f)) = body(&seen[at]) else {
            unreachable!()
        };
        assert_eq!(f.flight.basis, Basis::Modeled);
        let assumed = f.flight.assumed.as_ref().expect("assumed");
        Some((assumed.backspin_rpm, assumed.sidespin_rpm))
    }

    // -- The card's own processing ------------------------------------------

    /// The mode at the trigger is the mode the session (the driver) was
    /// handed by the session's `golfer.turn`, not the session state's: the
    /// session's state can change before the card drains the bus's turn,
    /// so in the trigger's `poll` pass the two can differ. The estimate
    /// goes with the driver's mode (none in Putting).
    #[test]
    fn a_mode_change_in_the_triggers_pass_is_the_drivers() {
        // The session's mode and the bus's turn at the trigger.
        let estimated = |key: &str, session: Mode, bus: Option<Mode>| {
            let go = Arc::new(AtomicBool::new(false));
            let mut steps = vec![Step::Hold(Arc::clone(&go))];
            steps.extend(replayed_shot(
                1,
                None,
                vec![
                    launch_body(1, launch_only_launch(&launch_only_d4())),
                    finished_body(1),
                ],
            ));
            let mut card = replay(replay_section(key), Mode::Full, steps);
            card.until(|m| {
                matches!(
                    m.as_local(),
                    Some((
                        _,
                        LocalEvent::CardStatus {
                            status: ActorStatus::Connected,
                            ..
                        }
                    ))
                )
            });
            card.writer.apply(&select(session));
            if let Some(mode) = bus {
                // A turn as the session publishes it (the state already
                // holds `session`, so publish the bus's mode directly).
                let mut turn = card.writer.turn();
                turn.mode = Some(mode);
                card.sender
                    .for_card("system", Some(card.state.flighthook_id()))
                    .frp(turn);
            }
            // The next pass drains what was sent before the hold ends, then
            // the trigger comes.
            go.store(true, Ordering::SeqCst);
            estimate_of(&card.shot()).is_some()
        };
        use Mode::{Full, Putting};
        assert!(estimated("10.1.7.1:5100", Full, None));
        assert!(!estimated("10.1.7.2:5100", Putting, Some(Putting)));
        // The session shows Putting, the card has not taken it yet.
        assert!(estimated("10.1.7.3:5100", Putting, None));
        // The card took Putting, the session does not show it yet.
        assert!(!estimated("10.1.7.4:5100", Full, Some(Putting)));
    }

    /// A launch-only chip (speed and launch angle, no spin, no
    /// flight result) gets the card's own flight result, modeled, assuming
    /// a backspin of `launch_only_spin` rpm per mph and no sidespin; the
    /// launch carries only what the device measured. The
    /// launch goes out at its full result, not when the device finishes
    /// the shot.
    #[test]
    fn a_launch_only_chip_gets_the_estimated_spin() {
        let finish = Arc::new(AtomicBool::new(false));
        let steps = launch_only_shot(1, launch_only_launch(&launch_only_d4()), &finish);
        let mut card = replay(replay_section("10.1.1.1:5100"), Mode::Chipping, steps);
        let seen = card.launch_only(&finish);
        // 100 rpm per mph × 15.7 mph.
        assert_eq!(estimate_of(&seen), Some((Some(1570), Some(0))));
        let launch = launch_of(&seen);
        assert_eq!(launch.backspin_rpm, None);
        assert_eq!(launch.sidespin_rpm, None);
    }

    /// No estimate when the mode at the trigger was Putting (whatever it is
    /// when the launch arrives), when the launch is under 5° (it rolls as a
    /// ground ball), when `launch_only_spin` is 0, or when the launch has a
    /// spin of the device's (an Xi E8 total).
    #[test]
    fn a_launch_only_estimate_needs_a_chip_without_spin() {
        let base = launch_only_launch(&launch_only_d4());
        // The card, the launch, `launch_only_spin`, the mode at the trigger
        // and at the launch, and the backspin the estimate assumes.
        type Case = (&'static str, Launch, f64, Mode, Mode, Option<i32>);
        let cases: [Case; 6] = [
            (
                "10.1.2.1:5100",
                base.clone(),
                100.0,
                Mode::Putting,
                Mode::Full,
                None,
            ),
            (
                "10.1.2.2:5100",
                base.clone(),
                100.0,
                Mode::Full,
                Mode::Putting,
                Some(1570),
            ),
            (
                "10.1.2.3:5100",
                Launch {
                    launch_elevation: Some(4.9),
                    ..base.clone()
                },
                100.0,
                Mode::Full,
                Mode::Full,
                None,
            ),
            (
                "10.1.2.4:5100",
                base.clone(),
                0.0,
                Mode::Full,
                Mode::Full,
                None,
            ),
            (
                "10.1.2.5:5100",
                Launch {
                    total_spin_rpm: Some(1800),
                    ..base.clone()
                },
                100.0,
                Mode::Full,
                Mode::Full,
                None,
            ),
            (
                "10.1.2.6:5100",
                base.clone(),
                80.0,
                Mode::Chipping,
                Mode::Chipping,
                // 80 rpm per mph × 15.7 mph.
                Some(1256),
            ),
        ];
        for (key, launch, k, at_trigger, at_launch, want) in cases {
            let go = Arc::new(AtomicBool::new(false));
            let steps = replayed_shot(
                1,
                Some(Arc::clone(&go)),
                vec![launch_body(1, launch), finished_body(1)],
            );
            let mut section = replay_section(key);
            section.launch_only_spin = k;
            let mut card = replay(section, at_trigger, steps);
            card.until(|m| matches!(body(m), Some(FrpBody::ShotTrigger(_))));
            card.turn_to(at_launch);
            go.store(true, Ordering::SeqCst);
            let got = estimate_of(&card.shot()).and_then(|(back, _)| back);
            assert_eq!(got, want, "{key}");
        }
    }

    /// `launch_only_spin` is a card setting applied live: a change reaches
    /// the next shot without a restart.
    #[test]
    fn a_launch_only_spin_change_applies_to_the_next_shot() {
        let go = Arc::new(AtomicBool::new(false));
        let chip = |n| {
            vec![
                launch_body(n, launch_only_launch(&launch_only_d4())),
                finished_body(n),
            ]
        };
        let mut steps = replayed_shot(1, None, chip(1));
        steps.extend(replayed_shot(2, Some(Arc::clone(&go)), chip(2)));
        let section = replay_section("10.1.3.1:5100");
        let mut card = replay(section.clone(), Mode::Chipping, steps);
        assert_eq!(
            estimate_of(&card.shot()).and_then(|(back, _)| back),
            Some(1570)
        );
        let changed = MevoSection {
            launch_only_spin: 50.0,
            ..section
        };
        card.state.system.update(|c| {
            c.mevo.insert("0".into(), changed.clone());
        });
        assert!(matches!(
            card.actor.reconfigure(&card.state, &card.sender),
            ReconfigureOutcome::Applied
        ));
        std::thread::sleep(Duration::from_millis(200));
        go.store(true, Ordering::SeqCst);
        assert_eq!(
            estimate_of(&card.shot()).and_then(|(back, _)| back),
            Some(785)
        );
    }

    // -- Output pins -----------------------------------------------------------

    /// The messages of the first shot `shot` makes on a scripted `model`
    /// card with `section`, up to its end.
    fn first_wire_shot(model: Model, section: MevoSection, shot: Shot) -> Vec<BusMsg> {
        let mut bench = Bench::start(model, section, |s| Box::new(MevoActor::new(s.clone())));
        bench.ready(true);
        let from = bench.seen.len();
        bench.device.send(Do::Shot(shot));
        bench.until(|m| matches!(body(m), Some(FrpBody::ShotFinished(_))));
        let seen = bench.seen[from..].to_vec();
        bench.state.stop_and_wait("mevo.0");
        seen
    }

    /// An Xi missed read (an empty result) ends as a shot with no data,
    /// which a simulator output never writes.
    #[test]
    fn a_missed_read_reaches_no_output() {
        use crate::actors::gspro::GsProRouting;
        use crate::actors::simout::{Feed, FeedEvent};
        let seen = first_wire_shot(Model::Xi, section(Model::Xi), Shot::xi_no_read());
        let mut feed = Feed::new(GsProRouting::default(), None);
        let events: Vec<FeedEvent> = seen.iter().filter_map(|m| feed.handle(m)).collect();
        assert!(
            matches!(events.last(), Some(FeedEvent::NoData(_))),
            "{events:?}"
        );
        assert!(!events.iter().any(|e| matches!(e, FeedEvent::Shot(_))));
    }

    /// Every output's numbers for a Mevo+ and an Xi full swing (through the
    /// card on ironsight's driver, over the scripted device), a launch-only
    /// Xi chip with the card's spin estimate and a Mevo shot sent from its
    /// early result (both replayed as ironsight's session emits them).
    #[test]
    fn outputs_keep_their_numbers() {
        use ironsight::protocol::{TYPE_CLUB_RESULT, TYPE_FLIGHT_RESULT, TYPE_FLIGHT_RESULT_V1};
        let d4 = || fixtures::XI_SHOT_D4.to_vec();
        let e8 = || fixtures::XI_SHOT_E8.to_vec();
        let ed = || fixtures::XI_SHOT_ED.to_vec();
        let avr = ironsight::BusAddr::Avr;
        let full = Some(Mode::Full);

        let mevo = first_wire_shot(
            Model::MevoPlus,
            section(Model::MevoPlus),
            Shot::new(
                63,
                avr,
                [(TYPE_FLIGHT_RESULT, d4()), (TYPE_CLUB_RESULT, ed())],
                ed(),
            ),
        );
        crate::test_ball::assert_outputs_pinned("mevo_full_swing", "mevo.0", full, &mevo);

        let mut xi_section = section(Model::Xi);
        xi_section.driver.spin_source = SpinSource::E8;
        let xi = first_wire_shot(
            Model::Xi,
            xi_section,
            Shot::new(
                63,
                avr,
                [
                    (TYPE_FLIGHT_RESULT_V1, e8()),
                    (TYPE_FLIGHT_RESULT, d4()),
                    (TYPE_CLUB_RESULT, ed()),
                ],
                ed(),
            ),
        );
        crate::test_ball::assert_outputs_pinned("xi_full_swing", "mevo.0", full, &xi);

        let finish = Arc::new(AtomicBool::new(false));
        let mut chip = replay(
            replay_section("10.1.5.1:5100"),
            Mode::Full,
            launch_only_shot(1, launch_only_launch(&launch_only_d4()), &finish),
        );
        crate::test_ball::assert_outputs_pinned(
            "xi_launch_only_chip",
            "mevo.0",
            full,
            &chip.launch_only(&finish),
        );

        let early = FlightResultV1::decode(fixtures::XI_SHOT_E8).expect("e8");
        let mut fallback = replay(
            replay_section("10.1.5.2:5100"),
            Mode::Full,
            replayed_shot(
                1,
                None,
                vec![
                    launch_body(1, ironsight::driver::convert::early_launch(&early)),
                    ShotFlight {
                        key: shot_key(1),
                        flight: ironsight::driver::convert::early_flight_result(&early),
                    }
                    .into(),
                    finished_body(1),
                ],
            ),
        );
        crate::test_ball::assert_outputs_pinned(
            "mevo_e8_fallback",
            "mevo.0",
            full,
            &fallback.shot(),
        );
    }

    /// Strict mode, through the real path: an Xi with the E8 spin source
    /// sends its spin pairs; a launch-only chip's spin estimate (the card's
    /// `assumed` spin, no measurement) is withheld.
    #[test]
    fn strict_mode_sends_the_xi_e8_spin_and_withholds_an_assumed_one() {
        use ironsight::protocol::{TYPE_CLUB_RESULT, TYPE_FLIGHT_RESULT, TYPE_FLIGHT_RESULT_V1};
        let d4 = || fixtures::XI_SHOT_D4.to_vec();
        let e8 = || fixtures::XI_SHOT_E8.to_vec();
        let ed = || fixtures::XI_SHOT_ED.to_vec();
        let mut xi_section = section(Model::Xi);
        xi_section.driver.spin_source = SpinSource::E8;
        let xi = first_wire_shot(
            Model::Xi,
            xi_section,
            Shot::new(
                63,
                ironsight::BusAddr::Avr,
                [
                    (TYPE_FLIGHT_RESULT_V1, e8()),
                    (TYPE_FLIGHT_RESULT, d4()),
                    (TYPE_CLUB_RESULT, ed()),
                ],
                ed(),
            ),
        );
        let frames = crate::test_ball::strict_frames("mevo.0", &xi).expect("sent");
        let pair = crate::actors::pureplay::codec::BallData::BACK_SPIN
            | crate::actors::pureplay::codec::BallData::SIDE_SPIN;
        assert_eq!(frames.ball.valid & pair, pair);
        assert!(frames.ball.back_spin_rpm > 0.0);

        let finish = Arc::new(AtomicBool::new(false));
        let mut chip = replay(
            replay_section("10.1.5.3:5100"),
            Mode::Full,
            launch_only_shot(1, launch_only_launch(&launch_only_d4()), &finish),
        );
        let frames =
            crate::test_ball::strict_frames("mevo.0", &chip.launch_only(&finish)).expect("sent");
        let spin = crate::actors::pureplay::codec::BallData::TOTAL_SPIN
            | crate::actors::pureplay::codec::BallData::BACK_SPIN
            | crate::actors::pureplay::codec::BallData::SIDE_SPIN
            | crate::actors::pureplay::codec::BallData::SPIN_AXIS;
        assert_eq!(frames.ball.valid & spin, 0, "no measured spin");
        assert_ne!(
            frames.ball.valid & crate::actors::pureplay::codec::BallData::BALL_SPEED,
            0
        );
    }

    // -- The card ----------------------------------------------------------------

    /// The device takes a new connection while the old one closes (the
    /// driver is not exclusive), the card waits the driver's 3 s for a
    /// stop, and its shots are measured.
    #[test]
    fn the_card_is_replaceable_while_stopping() {
        let actor = MevoActor::new(MevoSection::default());
        assert!(actor.replaceable_while_stopping());
        assert_eq!(actor.stop_wait(), Duration::from_secs(3));
        assert_eq!(actor.source(), Some(ShotSource::Mevo));
    }

    /// The Xi spin source, the launch-only spin, the camera timing and the
    /// name apply live; every other FlightScope setting starts a new session
    /// (a restart).
    #[test]
    fn only_the_spin_source_and_card_settings_apply_live() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-mevo-reconfigure-{}",
            crate::state::config::generate_id()
        ));
        let _dir = crate::actors::web::tests::TempDir(dir.clone());
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let state = Arc::new(state);
        let (tx, _rx) = broadcast::channel(16);
        let sender = BusSender::new("mevo.0".into(), tx, Arc::default());
        let saved = MevoSection::default();
        let actor = MevoActor::new(saved.clone());
        let outcome = |section: &MevoSection| {
            state.system.update(|c| {
                c.mevo.insert("0".into(), section.clone());
            });
            actor.reconfigure(&state, &sender)
        };
        let restarts: [fn(&mut MevoConfig); 9] = [
            |d| d.address = Some("10.0.0.9:5100".into()),
            |d| d.ball_type = Some(0),
            |d| d.tee_height = Some(Distance::Inches(2.0)),
            |d| d.range = Some(Distance::Feet(9.0)),
            |d| d.surface_height = Some(Distance::Inches(1.0)),
            |d| d.track_pct = Some(70.0),
            |d| d.use_estimated = Some(true),
            |d| d.camera_mode = Some(flighthook::CameraMode::RawFusion),
            |d| d.environment = Some(flighthook::FlightScopeEnvironment::Outdoor),
        ];
        for change in restarts {
            let mut section = saved.clone();
            change(&mut section.driver);
            assert!(matches!(
                outcome(&section),
                ReconfigureOutcome::RestartRequired
            ));
        }
        let mut live = MevoSection {
            name: "Bay 2".into(),
            launch_only_spin: 60.0,
            impact_lead_s: Some(0.2),
            ..saved.clone()
        };
        live.driver.spin_source = SpinSource::Auto;
        assert!(matches!(outcome(&live), ReconfigureOutcome::Applied));
    }

    /// A setting the driver refuses (from a hand-edited file) keeps the card
    /// from starting and shows on it.
    #[test]
    fn a_refused_setting_shows_on_the_card() {
        let mut section = replay_section("10.1.6.1:5100");
        section.driver.track_pct = Some(150.0);
        let mut card = replay(section, Mode::Full, Vec::new());
        let seen = card.until(|m| {
            matches!(
                m.as_local(),
                Some((
                    _,
                    LocalEvent::CardStatus {
                        status: ActorStatus::Disconnected,
                        ..
                    }
                ))
            )
        });
        let statuses: Vec<_> = seen
            .iter()
            .filter_map(|m| match m.as_local() {
                Some((_, LocalEvent::CardStatus { status, telemetry })) => {
                    Some((*status, telemetry.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(statuses.len(), 1, "no Starting, no connect");
        assert_eq!(statuses[0].1["error"], "track_pct: must be 0 to 100");
    }
}
