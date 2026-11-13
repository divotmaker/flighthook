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
    use std::collections::VecDeque;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use allsquare::driver::{BleLink, OmniSession};
    use allsquare::testing;
    use allsquare::{BallMetrics, ClubMetrics, DeviceState};
    use flighthook::{ActorStatus, SquareConfig};
    use flightrelay::config::ConfigError;
    use flightrelay::driver::{
        Backoff, ConnectCtx, DeviceDriver, DeviceIdentity, DeviceSession, DriverInfo, Events,
        NoAction, SessionEnd, TurnHints,
    };
    use flightrelay::{
        BusActor, Club, FrpBody, FrpMessage, GolferSelect, GolferTurn, Handedness, Mode,
    };
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::scripted::Handoff;
    use crate::actors::web::tests::TempDir;
    use crate::actors::{Actor, ReconfigureOutcome};
    use crate::bus::{BusMsg, BusSender, LocalEvent};
    use crate::state::{SessionWriter, SystemState};

    // -- The scripted Omni ----------------------------------------------------

    /// The scripted device's advertised name and BLE address (synthetic).
    const EQ_NAME: &str = "SquareGolf(EQ01)";
    const EQ_ADDRESS: &str = "02:00:00:00:00:01";

    /// One step of what the scripted device does.
    #[derive(Clone)]
    enum Step {
        /// A notification; the device sends every notification twice.
        Note(Vec<u8>),
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
    /// notifications, and a log of every command written.
    #[derive(Clone)]
    struct EqLink {
        wire: Arc<Mutex<EqWire>>,
        gate: Arc<Gate>,
        /// While set, a connect to this device waits (and keeps ticking).
        hold: Arc<AtomicBool>,
    }

    impl EqLink {
        fn new(steps: Vec<Step>) -> Self {
            Self {
                wire: Arc::new(Mutex::new(EqWire {
                    steps: steps.into(),
                    ..EqWire::default()
                })),
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

        /// Put `messages` on the bus while the session waits in a read, so
        /// the card takes them in one pass.
        fn send_together(&self, tx: &broadcast::Sender<BusMsg>, messages: Vec<BusMsg>) {
            let held = self.gate.held.lock().unwrap_or_else(|e| e.into_inner());
            self.gate.waiting.store(false, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.gate.waiting.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            for msg in messages {
                let _ = tx.send(msg);
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

        fn hints(&mut self, hints: &TurnHints) -> Result<(), allsquare::driver::Error> {
            self.0.hints(hints)
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
        tx: broadcast::Sender<BusMsg>,
        rx: broadcast::Receiver<BusMsg>,
        link: EqLink,
        /// The card's FRP actor id.
        frp: String,
    }

    impl Card {
        /// Start `section`'s card on a device playing `steps`. With `hand`,
        /// the session holds that hand first; `queued` turns go on the bus
        /// before the device is connected; with `system_actor`, the system
        /// actor runs on the bus.
        fn start(
            section: &SquareSection,
            steps: Vec<Step>,
            hand: Option<Handedness>,
            queued: Vec<GolferTurn>,
            system_actor: bool,
        ) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flighthook-square-test-{}",
                crate::state::config::generate_id()
            ));
            let (state, writer): (SystemState, SessionWriter) =
                SystemState::new(dir.join("config.toml"));
            if let Some(hand) = hand {
                let mut select = GolferSelect::new("test");
                select.handedness = Some(hand);
                writer.apply(&select);
            }
            state.system.update(|c| {
                c.square.insert("0".into(), section.clone());
            });
            let frp = state.actor_of_card("square.0").expect("frp id");
            let state = Arc::new(state);
            let (tx, rx) = broadcast::channel(1024);
            if system_actor {
                crate::actors::system::start_for_test(writer, &state, &tx, Arc::default());
            }
            let link = EqLink::new(steps);
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
            let card = Self {
                _dir: TempDir(dir),
                state,
                tx,
                rx,
                link,
                frp,
            };
            // On the bus before the device is connected.
            if !queued.is_empty() {
                for turn in queued {
                    let _ = card.tx.send(card.turn_msg(turn));
                }
                std::thread::sleep(Duration::from_millis(100));
                card.link.hold.store(false, Ordering::SeqCst);
            }
            card
        }

        /// The session's `golfer.turn` as the system actor publishes it.
        fn turn_msg(&self, turn: GolferTurn) -> BusMsg {
            BusMsg::frp(FrpMessage::new(self.state.flighthook_id(), turn))
        }

        /// Put the session's turn on the bus.
        fn turn(&self, turn: GolferTurn) {
            let _ = self.tx.send(self.turn_msg(turn));
        }

        /// A `golfer.select` from the web layer.
        fn select(&self, f: impl FnOnce(&mut GolferSelect)) {
            let mut select = GolferSelect::new("web");
            f(&mut select);
            let _ = self.tx.send(BusMsg::frp(FrpMessage::new("web", select)));
        }

        fn mine(&self, m: &BusMsg) -> bool {
            match m {
                BusMsg::Frp { msg, .. } => msg.from == self.frp,
                BusMsg::Local { card, .. } => card == "square.0",
            }
        }

        /// Messages of `square.0` until one matches `want`, within 10 s.
        fn until(&mut self, mut want: impl FnMut(&BusMsg) -> bool) -> Vec<BusMsg> {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut seen = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "timed out: {} seen", seen.len());
                match self.rx.try_recv() {
                    Ok(m) if self.mine(&m) => {
                        let done = want(&m);
                        seen.push(m);
                        if done {
                            return seen;
                        }
                    }
                    Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }

        /// Stop the card and wait for it.
        fn stop(&self) {
            assert_eq!(
                self.state.stop_and_wait("square.0"),
                crate::state::Stopped::Exited
            );
        }
    }

    fn body(m: &BusMsg) -> Option<&FrpBody> {
        m.as_frp().map(|f| &f.body)
    }

    fn is_status(m: &BusMsg, want: ActorStatus) -> bool {
        matches!(m.as_local(), Some((_, LocalEvent::CardStatus { status, .. })) if *status == want)
    }

    fn bus_actor(m: &BusMsg) -> Option<&BusActor> {
        match body(m) {
            Some(FrpBody::BusActor(a)) => Some(a),
            _ => None,
        }
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

    // -- Club, hand and stop ---------------------------------------------------

    fn select_is(frame: &[u8], club: allsquare::Club, handed: allsquare::Handed) -> bool {
        let cmd = allsquare::Command::SelectClub { club, handed };
        (0..=u8::MAX).any(|seq| cmd.encode(seq).as_slice() == frame)
    }

    /// The session's turn with `club`, `hand` and `mode`.
    fn turn(club: Option<Club>, hand: Handedness, mode: Mode) -> GolferTurn {
        GolferTurn::new(None, club, hand, mode)
    }

    /// A left-handed player's first club select, the arm, carries the left
    /// hand; without a hand in the session it is right-handed.
    #[test]
    fn the_arm_carries_the_sessions_hand() {
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

    /// The card's club is armed until the session names one; a hand that is
    /// on the bus before the device is connected rides the arm.
    #[test]
    fn a_hand_before_the_arm_rides_the_arm() {
        let mut section = section("hand-before-arm");
        section.driver.club = "pw".into();
        let card = Card::start(
            &section,
            vec![],
            None,
            vec![turn(None, Handedness::Left, Mode::Full)],
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
    /// the hand. A mode in the same turn does not matter.
    #[test]
    fn a_hand_change_resends_the_active_club_once() {
        use allsquare::Club::{Iron7, Iron8};
        use allsquare::Handed::{Left, Right};
        let card = Card::start(&section("hand-change"), vec![], None, vec![], false);
        card.link.wait_for_selects(1);
        card.turn(turn(None, Handedness::Left, Mode::Putting));
        card.link.wait_for_selects(2);
        card.turn(turn(None, Handedness::Left, Mode::Full));
        card.turn(turn(Some(Club::Iron8), Handedness::Left, Mode::Full));
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
        card.link.wait_for_selects(1);
        card.link.send_together(
            &card.tx,
            vec![
                card.turn_msg(turn(None, Handedness::Left, Mode::Full)),
                card.turn_msg(turn(Some(Club::Iron8), Handedness::Left, Mode::Full)),
            ],
        );
        card.link.wait_for_selects(2);
        // The next select is the next frame: nothing else was written.
        card.turn(turn(Some(Club::Iron7), Handedness::Left, Mode::Full));
        card.link.wait_for_selects(3);
        std::thread::sleep(Duration::from_millis(200));
        card.stop();
        let selects = card.link.selects();
        assert_eq!(selects.len(), 3);
        assert!(select_is(&selects[0], Iron7, Right));
        assert!(select_is(&selects[1], Iron8, Left));
        assert!(select_is(&selects[2], Iron7, Left));
    }

    /// A hand set in the UI (`golfer.select` from `web`) reaches the Omni
    /// through the session's turn: the active club is re-sent once with it,
    /// and the next club carries it.
    #[test]
    fn a_hand_set_in_the_ui_reaches_the_device() {
        use allsquare::Club::{Iron7, Iron8};
        use allsquare::Handed::{Left, Right};
        let card = Card::start(&section("ui-hand"), vec![], None, vec![], true);
        card.link.wait_for_selects(1);
        let left = |s: &mut GolferSelect| s.handedness = Some(Handedness::Left);
        card.select(left);
        card.link.wait_for_selects(2);
        card.select(left);
        card.select(|s| s.club = Some(Some(Club::Iron8)));
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

    /// A change of the driver's settings (`club`,
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
    fn first_shot(address: &str, (ball, club): (BallMetrics, ClubMetrics)) -> Vec<BusMsg> {
        let mut section = section(address);
        section
            .driver
            .dot_bottom_to_face_center_mm
            .insert("7I".into(), 0.0);
        let mut steps = vec![ack(DeviceState::Ready)];
        steps.extend(shot(&ball, &club));
        let mut card = Card::start(&section, steps, None, vec![], false);
        let messages = card.until(|m| matches!(body(m), Some(FrpBody::ShotFinished(_))));
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
            crate::test_ball::assert_outputs_pinned(case, "square.0", None, &messages);
        }
    }

    /// Strict mode, through the real path: the Omni's vertical impact (with
    /// its per-club offset) goes out with the horizontal one.
    #[test]
    fn strict_mode_sends_the_omni_vertical_impact() {
        let messages = first_shot("strict-full", full_swing());
        let frames = crate::test_ball::strict_frames("square.0", &messages).expect("sent");
        let club = frames.club.expect("club data");
        let impact = crate::actors::pureplay::codec::ClubData::IMPACT_POINT_X
            | crate::actors::pureplay::codec::ClubData::IMPACT_POINT_Y;
        assert_eq!(club.valid & impact, impact);
    }

    /// The impact height with the per-club offset to the face center is a
    /// calibrated reading, not a model's value: a tournament round keeps
    /// it, and the launch's four spin values.
    #[test]
    fn a_round_keeps_the_calibrated_impact_height() {
        let messages = first_shot("round", chip());
        let impact = messages
            .iter()
            .find_map(|m| match body(m) {
                Some(FrpBody::ShotImpact(i)) => Some(i.impact.clone()),
                _ => None,
            })
            .expect("an impact");
        let launch = messages
            .iter()
            .find_map(|m| match body(m) {
                Some(FrpBody::ShotLaunch(l)) => Some(l.launch.clone()),
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
        let shot = crate::actors::simout::OutputShot {
            launch: Some(launch.clone()),
            impact: Some(impact.clone()),
            source: Some(ShotSource::Square.provenance()),
            ..crate::actors::simout::OutputShot::default()
        };
        assert!(
            guard.counts(shot.source),
            "a measured source counts in a round"
        );
        assert_eq!(shot.impact.as_ref(), Some(&impact));
        assert_eq!(shot.launch.as_ref(), Some(&launch));
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
        // The session was ready; the lost link takes the card's `bus.actor`
        // to `not_armed` with none of the session's values, before the
        // Reconnecting status.
        let actors: Vec<&BusActor> = seen.iter().filter_map(bus_actor).collect();
        let ready = actors
            .iter()
            .position(|a| {
                a.blockers.is_empty()
                    && a.telemetry
                        .as_ref()
                        .and_then(|t| t.get("device_state"))
                        .map(String::as_str)
                        == Some("ready")
            })
            .expect("the session ready");
        let lost = actors.last().expect("the lost link's bus.actor");
        assert!(ready < actors.len() - 1, "{actors:?}");
        assert_eq!(lost.blockers, ["not_armed"]);
        assert_eq!(lost.telemetry, None);
        assert_eq!(
            lost.device.as_ref().and_then(|d| d.firmware.as_deref()),
            None
        );
        assert_eq!(card.link.wire().released, 1, "the lost link is let go");
    }
}
