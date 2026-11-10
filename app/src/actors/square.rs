//! The Square Golf Omni card: allsquare's device driver ([`Omni`]) on
//! [`DriverActor`].
//!
//! The driver owns the device: it finds the Omni over BLE (or the one the
//! card's `address` pins), connects without pairing, arms it with the club
//! and hand in play (the card's `club` until the game names one), follows
//! club and hand changes with one club select each, drops zero-spin
//! misreads unless the putter is in play, and reports readiness, device
//! state, battery and shots with the per-club face impact calibration
//! applied. The Omni takes one central at a time, so stopping disarms and
//! disconnects it, and a new instance waits for that (the driver is
//! exclusive). The card adds its section ([`SquareSection`]) and its shot
//! source; it has no processing of its own. Any change of the driver's
//! settings takes a new session (a restart).
//!
//! The original Square / Square Home is not supported: it uses a different
//! club-code scheme.

use allsquare::driver::Omni;
use flighthook::SquareSection;

use crate::actors::driver::{DriverActor, Hooks};
use crate::actors::provenance::ShotSource;

/// The Square Golf card: no processing of its own.
#[derive(Debug, Default)]
pub struct SquareHooks;

impl Hooks<Omni> for SquareHooks {
    type Section = SquareSection;
    const SOURCE: ShotSource = ShotSource::Square;
}

/// The Square Golf actor (`square.N`).
pub type SquareActor = DriverActor<Omni, SquareHooks>;

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use allsquare::driver::{BleLink, OmniSession};
    use allsquare::testing;
    use allsquare::{BallMetrics, ClubMetrics, DeviceState};
    use flighthook::{
        ActorStatus, Club, FlighthookEvent, FlighthookMessage, Handedness, ShotDetectionMode,
        SquareConfig,
    };
    use flightrelay::config::ConfigError;
    use flightrelay::driver::{
        Backoff, ConnectCtx, DeviceCommand, DeviceDriver, DeviceIdentity, DeviceSession,
        DriverInfo, Events, NoAction, SessionEnd,
    };
    use serde_json::{Value, json};
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::scripted::Handoff;
    use crate::actors::web::tests::TempDir;
    use crate::actors::{Actor, ReconfigureOutcome};
    use crate::bus::BusSender;
    use crate::state::SystemState;

    // -- The scripted Omni ----------------------------------------------------

    /// The scripted device's advertised name and BLE address (synthetic).
    const EQ_NAME: &str = "SquareGolf(EQ01)";
    const EQ_ADDRESS: &str = "02:00:00:00:00:01";

    /// One step of what the scripted device does.
    #[derive(Clone)]
    enum Step {
        /// A notification; the device sends every notification twice.
        Note(Vec<u8>),
        /// Something happens elsewhere that the game reports on the bus (a
        /// club or hand change); the device is quiet until the card's next
        /// pass, which takes the event off the bus first.
        Bus(FlighthookEvent),
        /// The link is gone.
        Lost,
    }

    /// What the scripted device has left to do and what it was told.
    #[derive(Default)]
    struct EqWire {
        steps: VecDeque<Step>,
        /// The second copy of the last notification.
        repeat: Option<Vec<u8>>,
        /// The card's session passes so far (polls).
        passes: u64,
        /// Quiet until this pass.
        quiet_until: Option<u64>,
        lost: bool,
        written: Vec<Vec<u8>>,
        released: u32,
    }

    /// Holds a session inside its device read, so a test can put several
    /// bus events out for one pass of the card.
    #[derive(Default)]
    struct Gate {
        held: Mutex<()>,
        waiting: AtomicBool,
    }

    /// A scripted Omni link: identity characteristics, the script's
    /// notifications, and a log of every command written. Bus steps go out
    /// through `bus`.
    #[derive(Clone)]
    struct EqLink {
        wire: Arc<Mutex<EqWire>>,
        bus: Option<BusSender>,
        gate: Arc<Gate>,
        /// While set, a connect to this device waits (and keeps ticking).
        hold: Arc<AtomicBool>,
    }

    impl EqLink {
        fn new(steps: Vec<Step>, bus: Option<BusSender>) -> Self {
            Self {
                wire: Arc::new(Mutex::new(EqWire {
                    steps: steps.into(),
                    ..EqWire::default()
                })),
                bus,
                gate: Arc::default(),
                hold: Arc::default(),
            }
        }

        fn wire(&self) -> std::sync::MutexGuard<'_, EqWire> {
            self.wire
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        /// The club selects written so far.
        fn selects(&self) -> Vec<Vec<u8>> {
            let select = allsquare::Command::SelectClub {
                club: allsquare::Club::Driver,
                handed: allsquare::Handed::Right,
            }
            .type_byte();
            self.wire()
                .written
                .iter()
                .filter(|w| allsquare::Command::type_byte_of(w) == Some(select))
                .cloned()
                .collect()
        }

        /// Wait until `count` club selects have been written.
        fn wait_for_selects(&self, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.selects().len() < count && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Put `events` on the bus while the session waits in a read, so the
        /// card takes them in one pass.
        fn send_together(&self, bus: &BusSender, events: Vec<FlighthookEvent>) {
            let held = self.gate.held.lock().unwrap_or_else(|e| e.into_inner());
            self.gate.waiting.store(false, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.gate.waiting.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            for event in events {
                bus.send(FlighthookMessage::new(event));
            }
            drop(held);
        }
    }

    impl allsquare::Transport for EqLink {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.gate.waiting.store(true, Ordering::SeqCst);
            drop(self.gate.held.lock().unwrap_or_else(|e| e.into_inner()));
            let mut w = self.wire();
            if w.lost {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "link lost"));
            }
            if let Some(note) = w.repeat.take() {
                buf[..note.len()].copy_from_slice(&note);
                return Ok(note.len());
            }
            if let Some(until) = w.quiet_until {
                if w.passes < until {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                w.quiet_until = None;
            }
            match w.steps.pop_front() {
                Some(Step::Note(note)) => {
                    buf[..note.len()].copy_from_slice(&note);
                    w.repeat = Some(note.clone());
                    Ok(note.len())
                }
                Some(Step::Bus(event)) => {
                    w.quiet_until = Some(w.passes + 1);
                    drop(w);
                    if let Some(bus) = &self.bus {
                        bus.send(FlighthookMessage::new(event));
                    }
                    Err(io::ErrorKind::WouldBlock.into())
                }
                Some(Step::Lost) => {
                    w.lost = true;
                    Err(io::Error::new(io::ErrorKind::BrokenPipe, "link lost"))
                }
                None => Err(io::ErrorKind::WouldBlock.into()),
            }
        }

        fn write(&mut self, data: &[u8]) -> io::Result<()> {
            self.wire().written.push(data.to_vec());
            Ok(())
        }

        fn read_characteristic(&mut self, id: u128) -> io::Result<Vec<u8>> {
            use allsquare::protocol::uuid;
            let s = match id {
                uuid::FW_VERSION => r#"{"launcher": "1.0.1", "mmi": "1.4.0", "lm": "1.5.8"}"#,
                uuid::HW_VERSION => "3.1",
                uuid::GAP_NAME => "SGO300A",
                uuid::DEVICE_ID => "SQ-TEST-0001",
                _ => return Err(io::ErrorKind::NotFound.into()),
            };
            Ok(s.as_bytes().to_vec())
        }
    }

    impl BleLink for EqLink {
        fn name(&self) -> &str {
            EQ_NAME
        }
        fn address(&self) -> &str {
            EQ_ADDRESS
        }
        fn release(&mut self) -> allsquare::Result<()> {
            self.wire().released += 1;
            Ok(())
        }
    }

    // -- The script the harness plays ----------------------------------------

    fn note(bytes: &[u8]) -> Step {
        Step::Note(bytes.to_vec())
    }

    fn ack(state: DeviceState) -> Step {
        note(&testing::heartbeat_ack(state))
    }

    /// A full 7-iron swing with every club value (the full-swing pin's).
    fn full_swing() -> (BallMetrics, ClubMetrics) {
        (
            BallMetrics {
                speed: 60.0,
                launch_angle: 14.0,
                direction: -1.2,
                total_spin: 3059,
                spin_axis: -11.31,
                back_spin: 3000,
                side_spin: -600,
                ..testing::ball_metrics()
            },
            ClubMetrics {
                mask: 0xff,
                path: Some(-2.4),
                face_angle: Some(-0.8),
                attack_angle: Some(-1.6),
                dynamic_loft: Some(17.9),
                impact_horizontal: Some(3.5),
                impact_vertical: Some(-4.0),
                club_speed: Some(41.4),
                smash_factor: Some(1.45),
            },
        )
    }

    /// A tracked fade chip (the chip pin's).
    fn chip() -> (BallMetrics, ClubMetrics) {
        (
            BallMetrics {
                speed: 3.30,
                launch_angle: 32.51,
                direction: 7.39,
                total_spin: 627,
                spin_axis: -8.00,
                back_spin: 621,
                side_spin: -87,
                ..testing::ball_metrics()
            },
            ClubMetrics {
                mask: 0xff,
                path: Some(2.15),
                face_angle: Some(4.62),
                attack_angle: Some(-3.18),
                dynamic_loft: Some(38.4),
                impact_horizontal: Some(-6.5),
                impact_vertical: Some(2.25),
                club_speed: Some(3.02),
                smash_factor: Some(1.09),
            },
        )
    }

    fn shot(ball: &BallMetrics, club: &ClubMetrics) -> [Step; 2] {
        [
            note(&testing::ball_notification(ball)),
            note(&testing::club_notification(club)),
        ]
    }

    /// The session the harness plays: waiting, battery, ready, a full
    /// swing, the device's state through the shot, a zero-spin 7-iron (a
    /// misread), a notification no one recognizes and a sensor report, the
    /// game's putter, a putt without club data, a left-handed player, the
    /// sand wedge, a tracked chip; then the link is gone.
    fn eq_script(bus: bool) -> Vec<Step> {
        let mut sensor = vec![0x11, 0x01, 0x00, 0x01, 0x01];
        sensor.resize(17, 0);
        let zero_spin = BallMetrics {
            speed: 30.0,
            launch_angle: 18.0,
            total_spin: 0,
            spin_axis: 0.0,
            back_spin: 0,
            side_spin: 0,
            ..testing::ball_metrics()
        };
        let putt = BallMetrics {
            speed: 2.5,
            launch_angle: 1.2,
            direction: -0.8,
            total_spin: 0,
            spin_axis: 0.0,
            back_spin: 0,
            side_spin: 0,
            ..testing::ball_metrics()
        };
        let untracked = ClubMetrics::default();
        let (swing_ball, swing_club) = full_swing();
        let (chip_ball, chip_club) = chip();
        let club = |club| {
            Step::Bus(FlighthookEvent::ClubInfo {
                club_info: flighthook::ClubInfo { club },
            })
        };
        let mut steps = vec![
            ack(DeviceState::Detect),
            note(&[0x91, 0x4b, 0x01]),
            ack(DeviceState::Ready),
        ];
        steps.extend(shot(&swing_ball, &swing_club));
        steps.extend([
            ack(DeviceState::Shot),
            ack(DeviceState::Done),
            ack(DeviceState::Ready),
        ]);
        steps.extend(shot(&zero_spin, &untracked));
        steps.extend([note(&[0x11, 0x55, 0x01, 0x02, 0x03]), note(&sensor)]);
        if bus {
            steps.push(club(Club::Putter));
        }
        steps.extend(shot(&putt, &untracked));
        if bus {
            steps.push(Step::Bus(FlighthookEvent::SetDetectionMode {
                mode: None,
                handed: Some(Handedness::Left),
                club: None,
            }));
            steps.push(club(Club::SandWedge));
        }
        steps.extend(shot(&chip_ball, &chip_club));
        steps.push(Step::Lost);
        steps
    }

    /// The Square section the harness runs with: a pinned device, the sand
    /// wedge's calibration, and the zero-spin switch written out as the
    /// 0.1.24 card wrote it.
    const EQ_SECTION: &str = r#"{"name":"Square Golf Omni","address":"eq-harness","discard_non_putting_zero_spin":true,"dot_bottom_to_face_center_mm":{"SW":12.5}}"#;

    // -- allsquare's driver over the scripted device -------------------------

    /// Scripted links waiting for a driver, by the card's `address`: each
    /// test runs with its own (see [`Handoff`]).
    static DEVICES: Handoff<String, VecDeque<EqLink>> = Handoff::new();

    /// allsquare's [`Omni`] with its BLE scan replaced by the scripted links
    /// queued for its address: a connect runs the real identity read and
    /// session over the next one, and waits for its stop when none is left.
    struct ScriptedOmni {
        inner: Omni,
        devices: VecDeque<EqLink>,
    }

    impl DeviceDriver for ScriptedOmni {
        type Config = SquareConfig;
        type Error = allsquare::driver::Error;
        type Action = NoAction;
        type Session = ScriptedSession;
        const STOP_WAIT: Duration = Omni::STOP_WAIT;

        fn info(config: &SquareConfig) -> DriverInfo {
            Omni::info(config)
        }

        fn new(config: SquareConfig) -> Result<Self, ConfigError> {
            let devices = DEVICES
                .take(&config.address.clone().unwrap_or_default())
                .unwrap_or_default();
            Ok(Self {
                inner: Omni::new(config)?,
                devices,
            })
        }

        fn config(&self) -> &SquareConfig {
            self.inner.config()
        }

        fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<ScriptedSession, Self::Error> {
            ctx.tick()?;
            ctx.out.phase("discovering");
            match self.devices.pop_front() {
                Some(link) => {
                    while link.hold.load(Ordering::SeqCst) {
                        ctx.tick()?;
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let session = self.inner.connect_with(ctx, link.clone())?;
                    Ok(ScriptedSession(session, link))
                }
                None => loop {
                    ctx.tick()?;
                    std::thread::sleep(Duration::from_millis(20));
                },
            }
        }

        fn retry_delay(
            &mut self,
            ended: &SessionEnd<'_, Self::Error>,
            backoff: &mut Backoff,
            out: &mut Events<'_>,
        ) -> Option<Duration> {
            self.inner.retry_delay(ended, backoff, out)
        }

        fn apply_config(&mut self, new: &SquareConfig) {
            self.inner.apply_config(new);
        }
    }

    /// allsquare's session, counting the card's passes for the scripted
    /// link.
    struct ScriptedSession(OmniSession, EqLink);

    impl DeviceSession for ScriptedSession {
        type Driver = ScriptedOmni;

        fn poll(
            &mut self,
            timeout: Duration,
            out: &mut Events<'_>,
        ) -> Result<(), allsquare::driver::Error> {
            self.1.wire().passes += 1;
            self.0.poll(timeout, out)
        }

        fn command(&mut self, cmd: &DeviceCommand) -> Result<(), allsquare::driver::Error> {
            self.0.command(cmd)
        }

        fn identity(&self) -> &DeviceIdentity {
            self.0.identity()
        }

        fn close(self, out: &mut Events<'_>) -> Result<(), allsquare::driver::Error> {
            self.0.close(out)
        }
    }

    impl Hooks<ScriptedOmni> for SquareHooks {
        type Section = SquareSection;
        const SOURCE: ShotSource = ShotSource::Square;
    }

    /// A running Square card (`square.0`) on a scripted device, and the bus.
    struct Card {
        _dir: TempDir,
        state: Arc<SystemState>,
        tx: broadcast::Sender<FlighthookMessage>,
        rx: broadcast::Receiver<FlighthookMessage>,
        link: EqLink,
    }

    impl Card {
        /// Start `section`'s card on a device playing `steps`. With
        /// `hand`, the game state holds that hand first; with
        /// `system_actor`, the system actor runs on the bus.
        fn start(
            section: &SquareSection,
            steps: Vec<Step>,
            hand: Option<Handedness>,
            queued: Vec<FlighthookEvent>,
            system_actor: bool,
        ) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flighthook-square-test-{}",
                crate::state::config::generate_id()
            ));
            let (state, writer) = SystemState::new(dir.join("config.toml"));
            if let Some(hand) = hand {
                writer.set_handed(hand);
            }
            state.system.update(|c| {
                c.square.insert("0".into(), section.clone());
            });
            let state = Arc::new(state);
            let (tx, rx) = broadcast::channel(1024);
            if system_actor {
                crate::actors::system::start_for_test(writer, &state, &tx, Arc::default());
            }
            let game = BusSender::new("web".into(), tx.clone(), Arc::default());
            let link = EqLink::new(steps, Some(game.clone()));
            link.hold.store(!queued.is_empty(), Ordering::SeqCst);
            DEVICES.put(
                section.driver.address.clone().unwrap_or_default(),
                VecDeque::from([link.clone()]),
            );
            crate::actors::start_actor(
                "square.0".into(),
                Box::new(DriverActor::<ScriptedOmni, SquareHooks>::new(
                    section.clone(),
                )),
                &state,
                &tx,
            );
            // On the bus before the device is connected.
            for event in queued {
                game.send(FlighthookMessage::new(event));
            }
            link.hold.store(false, Ordering::SeqCst);
            Self {
                _dir: TempDir(dir),
                state,
                tx,
                rx,
                link,
            }
        }

        /// The game's side of the bus (`web`).
        fn game(&self) -> BusSender {
            BusSender::new("web".into(), self.tx.clone(), Arc::default())
        }

        /// Messages of `square.0` until one matches `want`, within 10 s.
        fn until(
            &mut self,
            mut want: impl FnMut(&FlighthookMessage) -> bool,
        ) -> Vec<FlighthookMessage> {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut seen = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "timed out: {} seen", seen.len());
                match self.rx.try_recv() {
                    Ok(m) if m.actor == "square.0" => {
                        let done = want(&m);
                        seen.push(m);
                        if done {
                            return seen;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }

        /// The whole scripted session: every message up to the
        /// Disconnected after the stop that follows the lost connection.
        fn session(&mut self) -> Vec<FlighthookMessage> {
            let mut seen = self.until(|m| is_status(m, ActorStatus::Reconnecting));
            assert_eq!(
                self.state.stop_and_wait("square.0"),
                crate::state::Stopped::Exited
            );
            seen.extend(self.until(|m| is_status(m, ActorStatus::Disconnected)));
            seen
        }

        /// Stop the card and wait for it.
        fn stop(&self) {
            assert_eq!(
                self.state.stop_and_wait("square.0"),
                crate::state::Stopped::Exited
            );
        }
    }

    fn is_status(m: &FlighthookMessage, want: ActorStatus) -> bool {
        matches!(&m.event, FlighthookEvent::ActorStatus { status, .. } if *status == want)
    }

    /// A section pinned to its own scripted device by `address`.
    fn section(address: &str) -> SquareSection {
        SquareSection {
            driver: SquareConfig {
                address: Some(address.into()),
                ..SquareConfig::default()
            },
            ..SquareSection::default()
        }
    }

    // -- The equivalence harness ---------------------------------------------

    /// The bus messages and export lines of flighthook 897419c's Square
    /// actor (its own session code on allsquare 0.3.0's client) for
    /// [`eq_script`] and [`EQ_SECTION`], recorded with this file's scripted
    /// device.
    const OLD_BUS: &str = include_str!("../testdata/square_old_session.jsonl");
    const OLD_EXPORT: &str = include_str!("../testdata/square_old_export.jsonl");

    fn lines(text: &str) -> Vec<Value> {
        text.lines()
            .map(|l| serde_json::from_str(l).expect("a recorded line"))
            .collect()
    }

    /// `messages` with their shot and group ids numbered in order of
    /// appearance (the ids are random), and each shot's raw frames taken
    /// out (compared on their own): the shot's frames in order.
    fn normalized(mut messages: Vec<Value>) -> (Vec<Value>, Vec<Vec<Value>>) {
        let mut ids: HashMap<String, String> = HashMap::new();
        let mut raws = Vec::new();
        for m in &mut messages {
            renumber(m, &mut ids);
            let at = if m.pointer("/event/raw").is_some() {
                "/event/raw"
            } else {
                "/raw"
            };
            if let Some(raw) = m.pointer_mut(at)
                && raw.is_object()
            {
                raws.push(raw["frames"].as_array().cloned().unwrap_or_default());
                *raw = json!({ "transport": raw["transport"] });
            }
        }
        (messages, raws)
    }

    /// Replace every UUID string under `v` by its number of appearance.
    fn renumber(v: &mut Value, ids: &mut HashMap<String, String>) {
        match v {
            Value::String(s) if s.len() == 36 && s.matches('-').count() == 4 => {
                let n = ids.len();
                *s = ids
                    .entry(s.clone())
                    .or_insert_with(|| format!("id-{n}"))
                    .clone();
            }
            Value::Array(a) => a.iter_mut().for_each(|v| renumber(v, ids)),
            Value::Object(o) => o.values_mut().for_each(|v| renumber(v, ids)),
            _ => {}
        }
    }

    fn kind(m: &Value) -> &str {
        m["event"]["kind"].as_str().unwrap_or("")
    }

    fn status_message(status: &str, telemetry: Value) -> Value {
        json!({ "actor": "square.0", "event": { "kind": "actor_status", "status": status, "telemetry": telemetry } })
    }

    /// What the recorded actor's `messages` become on `DriverActor` with
    /// allsquare's driver: each change is one of design §7.3's rows for the
    /// Square, a rule of `DriverActor` accepted in steps 1 to 3R, or design
    /// §6.3's identity rule.
    fn expected_from_old(old: &[Value]) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        let mut i = 0;
        let mut device: Option<Value> = None;
        while i < old.len() {
            let mut m = old[i].clone();
            let status = m["event"]["status"].as_str().unwrap_or("").to_owned();
            match kind(&m) {
                // §7.3 #8: the driver's connect phase shows on the card while
                // it connects.
                "actor_status" if status == "starting" => {
                    out.push(m);
                    out.push(status_message(
                        "starting",
                        json!({ "phase": "discovering" }),
                    ));
                    i += 1;
                    continue;
                }
                // §7.3 #8 (step 3R): Connected removes the connect's phase.
                // Design §6.3: the BLE address is on the card label only, so
                // the `ble_address` key is gone. From here on every message
                // carries the session's device id (the sink's rule, step 2).
                "actor_status" if status == "connected" && device.is_none() => {
                    let telemetry = m["event"]["telemetry"].as_object_mut().expect("telemetry");
                    telemetry.remove("ble_address");
                    telemetry.insert("phase".into(), json!(""));
                    device = Some(old[i + 1]["device"].clone());
                    // The driver arms on its first poll and says `ready =
                    // false` then; the identity goes out on its own first
                    // (driver order).
                    let mut identity = old[i + 1].clone();
                    let ready = identity["event"]
                        .as_object_mut()
                        .expect("event")
                        .remove("telemetry")
                        .expect("the recorded identity carries ready");
                    out.push(m);
                    out.push(identity);
                    out.push(json!({
                        "actor": "square.0",
                        "device": old[i + 1]["device"],
                        "event": {
                            "kind": "device_telemetry",
                            "source": old[i + 1]["event"]["source"],
                            "telemetry": ready,
                        },
                    }));
                    i += 2;
                    continue;
                }
                // Steps 1/2: a shot's `shot_count` goes out after its
                // `shot_finished`, and its launch is held until the shot
                // ends (no flight result from the Omni), so it follows the
                // club data.
                "actor_status" if m["event"]["telemetry"].get("shot_count").is_some() => {
                    let end = (i..old.len())
                        .find(|&j| kind(&old[j]) == "shot_finished")
                        .expect("the shot's end");
                    let launch = old[i + 1..end]
                        .iter()
                        .find(|x| kind(x) == "launch")
                        .cloned();
                    for x in &old[i + 1..end] {
                        if kind(x) != "launch" {
                            out.push(x.clone());
                        }
                    }
                    out.extend(launch);
                    out.push(old[end].clone());
                    out.push(m);
                    i = end + 1;
                    continue;
                }
                // Step 2: a lost session sends `ready=false` with the rest
                // of the session's telemetry, then Reconnecting, then the
                // driver's per-attempt alert.
                "alert"
                    if m["event"]["message"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("Square Golf error")) =>
                {
                    let mut lost = old[i + 1].clone();
                    let mut merged = out
                        .iter()
                        .rev()
                        .find(|x| {
                            kind(x) == "device_telemetry" && x["event"].get("telemetry").is_some()
                        })
                        .map_or_else(|| json!({}), |x| x["event"]["telemetry"].clone());
                    merged["ready"] = json!("false");
                    lost["event"]["telemetry"] = merged;
                    out.push(lost);
                    out.push(old[i + 2].clone());
                    out.push(m);
                    i += 3;
                    continue;
                }
                _ => {}
            }
            out.push(m);
            i += 1;
        }
        if let Some(device) = device {
            let from = out
                .iter()
                .position(|m| kind(m) == "actor_status" && m["event"]["status"] == "connected")
                .expect("connected");
            for m in &mut out[from..] {
                m["device"] = device.clone();
            }
        }
        out
    }

    /// FRP 0.2.0 (design §3.1): a shot trigger carries the time its ball
    /// notification arrived. Taken out of `messages` (it is the host's
    /// clock), after checking every trigger has one.
    fn without_trigger_times(messages: &mut [Value]) {
        for m in messages.iter_mut().filter(|m| kind(m) == "shot_trigger") {
            let at = m["event"]
                .as_object_mut()
                .expect("event")
                .remove("timestamp");
            assert!(
                at.as_ref().is_some_and(Value::is_u64),
                "a trigger time: {at:?}"
            );
        }
    }

    /// The bytes of a lowercase hex string.
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// §7.3 #1: each shot's raw records are the recorded actor's, minus the
    /// notifications allsquare does not recognize (its driver keeps them
    /// out of the export, `drivers-050-decisions.md`), each labeled by the
    /// driver (`notify 11 02`, `notify 91`, `cmd 87`) instead of a
    /// `type` and `channel`.
    fn assert_raw_matches(old: &[Vec<Value>], new: &[Vec<Value>]) {
        assert_eq!(old.len(), new.len(), "shots with raw frames");
        for (n, (old, new)) in old.iter().zip(new).enumerate() {
            let want: Vec<(String, String)> = old
                .iter()
                .filter(|f| {
                    f["dir"] == "tx"
                        || allsquare::protocol::parse(&hex(f["hex"].as_str().expect("hex"))).is_ok()
                })
                .map(|f| {
                    assert!(f.get("label").is_none());
                    (
                        f["dir"].as_str().expect("dir").to_owned(),
                        f["hex"].as_str().expect("hex").to_owned(),
                    )
                })
                .collect();
            let got: Vec<(String, String)> = new
                .iter()
                .map(|f| {
                    (
                        f["dir"].as_str().expect("dir").to_owned(),
                        f["hex"].as_str().expect("hex").to_owned(),
                    )
                })
                .collect();
            assert_eq!(got, want, "shot {n}");
            for f in new {
                let bytes = hex(f["hex"].as_str().expect("hex"));
                let label = match (f["dir"].as_str(), bytes.as_slice()) {
                    (Some("rx"), [0x11, ty, ..]) => format!("notify 11 {ty:02x}"),
                    (Some("rx"), [family, ..]) => format!("notify {family:02x}"),
                    (Some("tx"), [_, ty, ..]) => format!("cmd {ty:02x}"),
                    other => panic!("shot {n}: {other:?}"),
                };
                assert_eq!(f["label"], label.as_str(), "shot {n}");
                assert!(f.get("channel").is_none() && f.get("type").is_none());
            }
        }
    }

    /// Design §7.2's equivalence harness: the recorded Square actor and the
    /// Square card on allsquare's driver, fed the same scripted device and
    /// the same game events, put the same messages on the bus and export
    /// the same shots, except for the differences `expected_from_old` and
    /// `without_trigger_times` list (design §7.3 rows 1 and 8, design §6.3
    /// and `DriverActor`'s accepted rules) and the raw records' labels and
    /// unrecognized notifications (§7.3 #1).
    #[tokio::test(flavor = "multi_thread")]
    async fn the_square_card_matches_the_recorded_actor() {
        let section: SquareSection = serde_json::from_str(EQ_SECTION).expect("section");
        let mut card = Card::start(&section, eq_script(true), None, vec![], false);
        let seen = card.session();

        let (old, old_raws) = normalized(lines(OLD_BUS));
        let (mut new, new_raws) = normalized(
            seen.iter()
                .map(|m| serde_json::to_value(m).expect("json"))
                .collect(),
        );
        without_trigger_times(&mut new);
        let expected = expected_from_old(&old);
        for (n, (want, got)) in expected.iter().zip(&new).enumerate() {
            assert_eq!(got, want, "message {n}");
        }
        assert_eq!(
            new.len(),
            expected.len(),
            "{:?}",
            new.iter().map(kind).collect::<Vec<_>>()
        );
        assert_raw_matches(&old_raws, &new_raws);

        // The game's club and hand changes: one select each, as recorded.
        let selects = card.link.selects();
        let select = |club, handed| allsquare::Command::SelectClub { club, handed };
        let is = |frame: &[u8], club, handed| {
            (0..=u8::MAX).any(|seq| select(club, handed).encode(seq).as_slice() == frame)
        };
        use allsquare::Club as S;
        use allsquare::Handed::{Left, Right};
        assert_eq!(selects.len(), 4, "the arm and three changes");
        assert!(is(&selects[0], S::Iron7, Right));
        assert!(is(&selects[1], S::Putter, Right));
        assert!(is(&selects[2], S::Putter, Left));
        assert!(is(&selects[3], S::SandWedge, Left));

        // The export: the same lines but for the schema (§7.3 #1), the
        // device settings (§7.3 #16: values at their default are left out)
        // and the raw records (compared above).
        let config = card.state.system.snapshot();
        let export = crate::actors::web::tests::export_lines(&config, &seen).await;
        let (old_export, old_export_raws) = normalized(lines(OLD_EXPORT));
        let (new_export, new_export_raws) = normalized(
            export
                .iter()
                .map(|l| serde_json::from_str(l).expect("json"))
                .collect(),
        );
        assert_eq!(new_export.len(), old_export.len());
        for (mut want, got) in old_export.into_iter().zip(new_export) {
            assert_eq!(want["schema"], 3);
            want["time"] = got["time"].clone();
            want["flighthook_version"] = got["flighthook_version"].clone();
            let settings = want["settings"]["device"]
                .as_object_mut()
                .expect("device settings");
            assert_eq!(
                settings.remove("discard_non_putting_zero_spin"),
                Some(json!(true))
            );
            assert_eq!(got, want);
        }
        assert_raw_matches(&old_export_raws, &new_export_raws);
    }

    // -- Club, hand and stop ---------------------------------------------------

    fn select_is(frame: &[u8], club: allsquare::Club, handed: allsquare::Handed) -> bool {
        let cmd = allsquare::Command::SelectClub { club, handed };
        (0..=u8::MAX).any(|seq| cmd.encode(seq).as_slice() == frame)
    }

    fn hand_event(mode: Option<ShotDetectionMode>, handed: Handedness) -> FlighthookEvent {
        FlighthookEvent::SetDetectionMode {
            mode,
            handed: Some(handed),
            club: None,
        }
    }

    fn club_event(club: Club) -> FlighthookEvent {
        FlighthookEvent::ClubInfo {
            club_info: flighthook::ClubInfo { club },
        }
    }

    /// A left-handed player's first club select, the arm, carries the left
    /// hand; without a hand in the game state it is right-handed.
    #[test]
    fn the_arm_carries_the_game_states_hand() {
        use allsquare::Handed::{Left, Right};
        for (address, hand, want) in [
            ("arm-left", Some(Handedness::Left), Left),
            ("arm-unknown", None, Right),
        ] {
            let card = Card::start(&section(address), vec![], hand, vec![], false);
            card.link.wait_for_selects(1);
            card.stop();
            let selects = card.link.selects();
            assert_eq!(selects.len(), 1, "{address}");
            assert!(
                select_is(&selects[0], allsquare::Club::Iron7, want),
                "{address}"
            );
        }
    }

    /// The card's club is armed until the game names one; a hand that is on
    /// the bus before the device is connected rides the arm.
    #[test]
    fn a_hand_before_the_arm_rides_the_arm() {
        let mut section = section("hand-before-arm");
        section.driver.club = "pw".into();
        let card = Card::start(
            &section,
            vec![],
            None,
            vec![hand_event(None, Handedness::Left)],
            false,
        );
        card.link.wait_for_selects(1);
        std::thread::sleep(Duration::from_millis(200));
        card.stop();
        let selects = card.link.selects();
        assert_eq!(selects.len(), 1);
        assert!(select_is(
            &selects[0],
            allsquare::Club::PitchingWedge,
            allsquare::Handed::Left
        ));
    }

    /// A hand change while connected re-sends the active club once with the
    /// new hand; the same hand again writes nothing; the next club carries
    /// the hand. A mode in the same event does not matter.
    #[test]
    fn a_hand_change_resends_the_active_club_once() {
        use allsquare::Club::{Iron7, Iron8};
        use allsquare::Handed::{Left, Right};
        let card = Card::start(&section("hand-change"), vec![], None, vec![], false);
        let game = card.game();
        card.link.wait_for_selects(1);
        game.send(FlighthookMessage::new(hand_event(
            Some(ShotDetectionMode::Putting),
            Handedness::Left,
        )));
        card.link.wait_for_selects(2);
        game.send(FlighthookMessage::new(hand_event(None, Handedness::Left)));
        game.send(FlighthookMessage::new(club_event(Club::Iron8)));
        card.link.wait_for_selects(3);
        std::thread::sleep(Duration::from_millis(200));
        card.stop();
        let selects = card.link.selects();
        assert_eq!(selects.len(), 3);
        assert!(select_is(&selects[0], Iron7, Right));
        assert!(select_is(&selects[1], Iron7, Left));
        assert!(select_is(&selects[2], Iron8, Left));
    }

    /// A hand and a club that change in the same pass write one select: the
    /// new club with the new hand.
    #[test]
    fn a_hand_and_a_club_in_one_pass_write_one_select() {
        use allsquare::Club::{Iron7, Iron8};
        use allsquare::Handed::{Left, Right};
        let card = Card::start(&section("one-pass"), vec![], None, vec![], false);
        let game = card.game();
        card.link.wait_for_selects(1);
        card.link.send_together(
            &game,
            vec![
                hand_event(Some(ShotDetectionMode::Full), Handedness::Left),
                club_event(Club::Iron8),
            ],
        );
        card.link.wait_for_selects(2);
        // The next select is the next frame: nothing else was written.
        game.send(FlighthookMessage::new(club_event(Club::Iron7)));
        card.link.wait_for_selects(3);
        std::thread::sleep(Duration::from_millis(200));
        card.stop();
        let selects = card.link.selects();
        assert_eq!(selects.len(), 3);
        assert!(select_is(&selects[0], Iron7, Right));
        assert!(select_is(&selects[1], Iron8, Left));
        assert!(select_is(&selects[2], Iron7, Left));
    }

    /// A hand set in the UI (`hand_info` from `web`) reaches the Omni through
    /// the system actor's command: the active club is re-sent once with it,
    /// and the next club carries it.
    #[test]
    fn a_hand_set_in_the_ui_reaches_the_device() {
        use allsquare::Club::{Iron7, Iron8};
        use allsquare::Handed::{Left, Right};
        let card = Card::start(&section("ui-hand"), vec![], None, vec![], true);
        let game = card.game();
        card.link.wait_for_selects(1);
        let left = || {
            FlighthookMessage::new(FlighthookEvent::HandInfo {
                hand_info: flighthook::HandInfo {
                    handed: Handedness::Left,
                },
            })
        };
        game.send(left());
        card.link.wait_for_selects(2);
        game.send(left());
        game.send(FlighthookMessage::new(club_event(Club::Iron8)));
        card.link.wait_for_selects(3);
        std::thread::sleep(Duration::from_millis(200));
        card.stop();
        let selects = card.link.selects();
        assert_eq!(selects.len(), 3);
        assert!(select_is(&selects[0], Iron7, Right));
        assert!(select_is(&selects[1], Iron7, Left));
        assert!(select_is(&selects[2], Iron8, Left));
    }

    /// Stopping disarms the device and ends the link at once.
    #[test]
    fn a_stop_disarms_the_device_and_ends_the_link_at_once() {
        let card = Card::start(&section("stop"), vec![], None, vec![], false);
        card.link.wait_for_selects(1);
        assert_eq!(card.link.wire().released, 0);
        let stopped = Instant::now();
        card.stop();
        let took = stopped.elapsed();
        assert!(took < Duration::from_secs(1), "{took:?}");
        let wire = card.link.wire();
        assert_eq!(wire.released, 1, "the link is ended");
        let disarm = allsquare::Command::DetectBall {
            on: false,
            spin: allsquare::SpinMode::Advanced,
        };
        assert!(
            wire.written
                .iter()
                .any(|w| (0..=u8::MAX).any(|seq| disarm.encode(seq).as_slice() == w.as_slice())),
            "disarmed"
        );
    }

    /// The Omni takes one central: the card is not replaced while it stops.
    #[test]
    fn the_card_is_exclusive() {
        let actor = SquareActor::new(SquareSection::default());
        assert!(!actor.replaceable_while_stopping());
        assert_eq!(actor.stop_wait(), Duration::from_secs(6));
        assert_eq!(actor.source(), Some(ShotSource::Square));
    }

    /// Design §7.3 #5: a change of the driver's settings (`club`,
    /// `advanced_spin`, `discard_non_putting_zero_spin`, the calibration,
    /// the address) takes a new session; the card's own fields apply live.
    #[test]
    fn a_driver_setting_change_restarts_the_card() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-square-reconfigure-{}",
            crate::state::config::generate_id()
        ));
        let _dir = TempDir(dir.clone());
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let state = Arc::new(state);
        let (tx, _rx) = broadcast::channel(16);
        let sender = BusSender::new("square.0".into(), tx, Arc::default());
        let saved = SquareSection::default();
        let actor = SquareActor::new(saved.clone());
        let outcome = |section: &SquareSection| {
            state.system.update(|c| {
                c.square.insert("0".into(), section.clone());
            });
            actor.reconfigure(&state, &sender)
        };
        let changed: [fn(&mut SquareSection); 5] = [
            |s| s.driver.club = "PW".into(),
            |s| s.driver.advanced_spin = false,
            |s| s.driver.discard_non_putting_zero_spin = false,
            |s| {
                s.driver
                    .dot_bottom_to_face_center_mm
                    .insert("7I".into(), 20.0);
            },
            |s| s.driver.address = Some("SquareGolf(CD01)".into()),
        ];
        for change in changed {
            let mut section = saved.clone();
            change(&mut section);
            assert!(matches!(
                outcome(&section),
                ReconfigureOutcome::RestartRequired
            ));
        }
        let live = SquareSection {
            name: "Bay 2".into(),
            impact_lead_s: Some(0.5),
            ..saved.clone()
        };
        assert!(matches!(outcome(&live), ReconfigureOutcome::Applied));
    }

    // -- Shot values and outputs ---------------------------------------------

    /// The bus messages of a card on a device that makes the one shot
    /// `(ball, club)` with the 7-iron, up to the shot's end. The 7-iron's
    /// calibration is 0, so the vertical impact is the device's value.
    fn first_shot(
        address: &str,
        (ball, club): (BallMetrics, ClubMetrics),
    ) -> Vec<FlighthookMessage> {
        let mut section = section(address);
        section
            .driver
            .dot_bottom_to_face_center_mm
            .insert("7I".into(), 0.0);
        let mut steps = vec![ack(DeviceState::Ready)];
        steps.extend(shot(&ball, &club));
        let mut card = Card::start(&section, steps, None, vec![], false);
        let messages = card.until(|m| matches!(m.event, FlighthookEvent::ShotFinished { .. }));
        card.stop();
        messages
    }

    /// Every output's numbers for an Omni shot with club data and face
    /// impact, through the card on allsquare's driver: the fade chip and a
    /// full swing.
    #[test]
    fn outputs_keep_their_numbers() {
        for (case, address, shot) in [
            ("square", "pins-chip", chip()),
            ("square_full_swing", "pins-full", full_swing()),
        ] {
            let messages = first_shot(address, shot);
            crate::test_ball::assert_outputs_pinned(case, &messages);
        }
    }

    /// The impact height with the per-club offset to the face center is a
    /// calibrated reading, not a model's value: a tournament round keeps
    /// it, and the launch's four spin values.
    #[test]
    fn a_round_keeps_the_calibrated_impact_height() {
        let messages = first_shot("round", chip());
        let impact = messages
            .iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::FaceImpact { impact, .. } => Some((**impact).clone()),
                _ => None,
            })
            .expect("an impact");
        let launch = messages
            .iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::Launch { launch, .. } => Some((**launch).clone()),
                _ => None,
            })
            .expect("a launch");
        assert_eq!(impact.lateral, Some(flighthook::Distance::Millimeters(6.5)));
        assert_eq!(
            impact.vertical,
            Some(flighthook::Distance::Millimeters(2.25))
        );
        let round = crate::state::round::RoundPolicy::default();
        let guard = round
            .begin_round(&crate::state::round::RoundRequest {
                holder: "Pureplay",
                feeds: &[],
            })
            .expect("accepted");
        let shot = flighthook::ShotData {
            launch: Some(launch.clone()),
            impact: Some(impact.clone()),
            provenance: Some(ShotSource::Square.provenance()),
            ..flighthook::ShotData::default()
        };
        assert_eq!(shot.unmeasured_fields().count(), 0);
        let certified = guard.certify(&shot).expect("measured");
        assert_eq!(certified.shot().impact.as_ref(), Some(&impact));
        assert_eq!(certified.shot().launch.as_ref(), Some(&launch));
    }

    /// The device's negative (rightward) sidespin lands the ball right in
    /// the simulation.
    #[test]
    fn a_device_fade_lands_right_in_the_simulation() {
        let (fade, _) = chip();
        let cfg = flighthook::SimulationSection::default();
        let d =
            crate::sim::derive(&allsquare::driver::launch(&fade), None, &cfg).expect("simulated");
        let off = d.carry_offline.expect("offline").as_meters();
        assert!(off > 0.0, "{off}");
        // The same fade at full speed and launched straight, so only the
        // sidespin can move it right.
        let straight = BallMetrics {
            speed: 60.0,
            launch_angle: 14.0,
            direction: 0.0,
            back_spin: 3000,
            side_spin: -600,
            ..fade
        };
        let d = crate::sim::derive(&allsquare::driver::launch(&straight), None, &cfg)
            .expect("simulated");
        let off = d.carry_offline.expect("offline").as_meters();
        assert!(off > 0.0, "{off}");
    }

    /// The scripted device answers nothing until it is told to stop: the
    /// session tests above rely on it, and a lost link ends a session.
    #[test]
    fn a_lost_link_ends_the_session_and_reconnects() {
        let mut card = Card::start(
            &section("lost"),
            vec![ack(DeviceState::Ready), Step::Lost],
            None,
            vec![],
            false,
        );
        let seen = card.until(|m| is_status(m, ActorStatus::Reconnecting));
        card.stop();
        assert!(seen.iter().any(|m| matches!(&m.event,
            FlighthookEvent::DeviceTelemetry { telemetry: Some(t), .. }
                if t.get("ready").map(String::as_str) == Some("false")
                    && t.get("device_state").map(String::as_str) == Some("ready"))));
        assert_eq!(card.link.wire().released, 1, "the lost link is let go");
    }
}
