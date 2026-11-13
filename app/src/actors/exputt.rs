//! The ExPutt RG card: threeputt's device driver ([`ExPutt`]) on
//! [`DriverActor`].
//!
//! The driver owns the device: it finds the ExPutt on the local network (or
//! reaches the IP the card's `address` names), starts measuring with the
//! player's hand, turns every putt into a putter shot (ball speed and launch
//! direction, putter path and face when tracked), follows hand changes with
//! a new round, and reports readiness and the device state. It also keeps a
//! device that stalls from being torn down too early or hammered with new
//! connections: the silence limits, the spacing of mat searches, the wait
//! after a stall, quick retries after a measuring session, and one "not
//! answering" alert per outage. Putts are simulated in putting mode
//! whatever the game's mode is (the driver's fixed mode), and each shot's
//! trigger carries the time the ball was struck, which the cameras use.
//!
//! The card adds its section ([`ExPuttSection`]), its shot source, the
//! Find mat button (a `find_mat` for this card asks the driver to locate the
//! mat again) and the card's words for the discovery hint. The device takes
//! one connection at a time, so a new instance waits for the old one to
//! close (the driver is exclusive).

use flighthook::ExPuttSection;
use flightrelay::FrpBody;
use threeputt::driver::{Action, ExPutt};

use crate::actors::driver::{DriverActor, HookCtx, Hooks};
use crate::actors::provenance::ShotSource;
use crate::bus::{BusMsg, LocalEvent};

/// The alert after 30 s of discovery without the device, in the card's
/// words: the address is the card's IP Address field.
const DISCOVERY_HINT: &str = "ExPutt not found on this network. If it is on another network, \
     or a firewall blocks discovery, enter its IP address on the device card.";

/// The ExPutt card: the Find mat button and the discovery hint.
#[derive(Debug, Default)]
pub struct ExPuttHooks;

impl Hooks<ExPutt> for ExPuttHooks {
    type Section = ExPuttSection;
    const SOURCE: ShotSource = ShotSource::ExPutt;

    fn on_frp(&mut self, body: &mut FrpBody, _ctx: &mut HookCtx<'_, ExPuttSection>) {
        card_words(body);
    }

    fn action(msg: &BusMsg, card: &str) -> Option<Action> {
        find_mat(msg, card)
    }
}

/// The driver's discovery hint in the card's words ([`DISCOVERY_HINT`]).
fn card_words(body: &mut FrpBody) {
    if let FrpBody::BusAlert(alert) = body
        && alert.message == threeputt::driver::DISCOVERY_HINT
    {
        DISCOVERY_HINT.clone_into(&mut alert.message);
    }
}

/// [`Action::FindMat`] when `msg` is a Find mat for the card `card`.
fn find_mat(msg: &BusMsg, card: &str) -> Option<Action> {
    match msg.as_local() {
        Some((_, LocalEvent::FindMat { target })) if target == card => Some(Action::FindMat),
        _ => None,
    }
}

/// The ExPutt actor (`exputt.N`).
pub type ExPuttActor = DriverActor<ExPutt, ExPuttHooks>;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use crate::actors::simout::OutputShot;
    use flighthook::{
        ActorStatus, ClubData, ExPuttConfig, Handedness, Launch, Mode, Severity, ShotRaw, Velocity,
    };
    use flightrelay::config::{ConfigError, DriverConfig};
    use flightrelay::driver::{
        Backoff, ConnectCtx, DeviceDriver, DeviceIdentity, DeviceSession, DriverInfo, Events,
        SessionEnd, TurnHints,
    };
    use flightrelay::{BusActor, FrpMessage, GolferSelect, GolferTurn};
    use threeputt::driver::{ExPuttSession, NOT_ANSWERING, Tuning};
    use threeputt::testing::{
        BALL_STRUCK, MockConfig, MockDevice, PUTT_READY, PUTT_RESULT_BALL_ONLY, PUTT_RESULT_FULL,
        PUTT_RESULT_PULLED, device_event,
    };
    use threeputt::{Event, Hand, PuttResult, Request};
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::gspro::mapper::map_shot;
    use crate::actors::scripted::Handoff;
    use crate::actors::{Actor, ReconfigureOutcome};
    use crate::bus::BusSender;
    use crate::state::{SessionWriter, SystemState};

    /// Longest any test waits for one thing.
    const WAIT: Duration = Duration::from_secs(8);

    // -- The ExPutt on a mock device -----------------------------------------

    /// Test timings by the card's `address`: each test runs with its own
    /// (see [`Handoff`]).
    static TUNINGS: Handoff<String, Tuning> = Handoff::new();

    /// A UDP port nothing sends beacons to.
    fn quiet_udp_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|s| s.local_addr())
            .expect("udp port")
            .port()
    }

    /// The driver's timings scaled down (one second of silence standing
    /// for 90), its device on `port` (read at every connect), no mat search
    /// spacing, stall wait or "not answering" alert unless a test sets them.
    fn tuning(port: &Arc<AtomicU16>) -> Tuning {
        let mut t = Tuning::default();
        t.port = Arc::clone(port);
        t.discovery_port = quiet_udp_port();
        t.connect_timeout = Duration::from_secs(1);
        t.reply_timeout = Duration::from_secs(2);
        t.mat_timeout = Duration::from_secs(2);
        t.keepalive_interval = Duration::from_millis(100);
        t.discovery_hint_after = Duration::from_secs(3600);
        t.silence_baseline = Duration::from_millis(1000);
        t.silence_after_strike = Duration::from_millis(2000);
        t.silence_warn_after = Duration::from_millis(333);
        t.stall_wait = Duration::ZERO;
        t.resync_gap = Duration::ZERO;
        t.not_answering_after = Duration::from_secs(3600);
        t
    }

    /// threeputt's [`ExPutt`] with the test timings queued for its
    /// `address`, reaching the mock device on `127.0.0.1` whatever IP the
    /// address names (no address: discovery, on a port nobody announces
    /// on unless the test's timings say otherwise).
    struct ScriptedExPutt {
        inner: ExPutt,
        config: ExPuttConfig,
    }

    impl DeviceDriver for ScriptedExPutt {
        type Config = ExPuttConfig;
        type Error = threeputt::driver::Error;
        type Action = Action;
        type Session = ScriptedSession;
        const STOP_WAIT: Duration = ExPutt::STOP_WAIT;

        fn info(config: &ExPuttConfig) -> DriverInfo {
            ExPutt::info(config)
        }

        fn new(config: ExPuttConfig) -> Result<Self, ConfigError> {
            config.validate()?;
            let tuning = TUNINGS
                .take(&config.address.clone().unwrap_or_default())
                .unwrap_or_default();
            let local = ExPuttConfig {
                address: config.ip()?.map(|_| "127.0.0.1".into()),
            };
            Ok(Self {
                inner: ExPutt::with_tuning(local, tuning)?,
                config,
            })
        }

        fn config(&self) -> &ExPuttConfig {
            &self.config
        }

        fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<ScriptedSession, Self::Error> {
            self.inner.connect(ctx).map(ScriptedSession)
        }

        fn retry_delay(
            &mut self,
            ended: &SessionEnd<'_, Self::Error>,
            backoff: &mut Backoff,
            out: &mut Events<'_>,
        ) -> Option<Duration> {
            self.inner.retry_delay(ended, backoff, out)
        }

        fn apply_config(&mut self, new: &ExPuttConfig) {
            self.config = new.clone();
        }
    }

    struct ScriptedSession(ExPuttSession);

    impl DeviceSession for ScriptedSession {
        type Driver = ScriptedExPutt;

        fn poll(
            &mut self,
            timeout: Duration,
            out: &mut Events<'_>,
        ) -> Result<(), threeputt::driver::Error> {
            self.0.poll(timeout, out)
        }

        fn hints(&mut self, hints: &TurnHints) -> Result<(), threeputt::driver::Error> {
            self.0.hints(hints)
        }

        fn action(&mut self, action: &Action) -> Result<(), threeputt::driver::Error> {
            self.0.action(action)
        }

        fn identity(&self) -> &DeviceIdentity {
            self.0.identity()
        }

        fn close(self, out: &mut Events<'_>) -> Result<(), threeputt::driver::Error> {
            self.0.close(out)
        }
    }

    impl Hooks<ScriptedExPutt> for ExPuttHooks {
        type Section = ExPuttSection;
        const SOURCE: ShotSource = ShotSource::ExPutt;

        fn on_frp(&mut self, body: &mut FrpBody, _ctx: &mut HookCtx<'_, ExPuttSection>) {
            card_words(body);
        }

        fn action(msg: &BusMsg, card: &str) -> Option<Action> {
            find_mat(msg, card)
        }
    }

    /// A section reaching its own test timings by `address`.
    fn section(address: &str) -> ExPuttSection {
        ExPuttSection {
            name: "ExPutt RG".into(),
            driver: ExPuttConfig {
                address: Some(address.into()),
            },
            ..ExPuttSection::default()
        }
    }

    /// A running ExPutt card (`exputt.0`) and the bus.
    struct Card {
        dir: std::path::PathBuf,
        state: Arc<SystemState>,
        /// The session's writer, until a system actor takes it.
        writer: Option<SessionWriter>,
        tx: broadcast::Sender<BusMsg>,
        rx: broadcast::Receiver<BusMsg>,
        shutdown: Arc<AtomicBool>,
        section: ExPuttSection,
        /// The card's FRP actor id.
        frp: String,
    }

    impl Card {
        /// State and bus for `section`, the card not started yet; `tuning`
        /// is what its driver runs with.
        fn new(section: ExPuttSection, tuning: Tuning) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flighthook-exputt-{}",
                crate::state::config::generate_id()
            ));
            let (state, writer) = SystemState::new(dir.join("config.toml"));
            state.system.update(|c| {
                c.exputt.insert("0".into(), section.clone());
            });
            let frp = state.actor_of_card("exputt.0").expect("frp id");
            TUNINGS.put(section.driver.address.clone().unwrap_or_default(), tuning);
            let (tx, rx) = broadcast::channel(4096);
            Self {
                dir,
                state: Arc::new(state),
                writer: Some(writer),
                tx,
                rx,
                shutdown: Arc::default(),
                section,
                frp,
            }
        }

        /// A card on the device on `port`.
        fn on(address: &str, port: &Arc<AtomicU16>) -> Self {
            Self::new(section(address), tuning(port))
        }

        /// A card on `mock`, started.
        fn started(address: &str, mock: &MockDevice) -> Self {
            let mut card = Self::on(address, &Arc::new(AtomicU16::new(mock.port())));
            card.start();
            card
        }

        /// Set the session's turn before the card starts.
        fn select(&self, f: impl FnOnce(&mut GolferSelect)) {
            let mut select = GolferSelect::new("test");
            f(&mut select);
            self.writer.as_ref().expect("writer").apply(&select);
        }

        /// Start a system actor on the bus, holding the session's writer:
        /// the hand is then set with a `golfer.select`, as every source
        /// sets it.
        fn start_system_actor(&mut self) {
            let writer = self.writer.take().expect("writer");
            crate::actors::system::start_for_test(
                writer,
                &self.state,
                &self.tx,
                Arc::clone(&self.shutdown),
            );
        }

        fn start(&mut self) {
            crate::actors::start_actor(
                "exputt.0".into(),
                Box::new(DriverActor::<ScriptedExPutt, ExPuttHooks>::new(
                    self.section.clone(),
                )),
                &self.state,
                &self.tx,
            );
        }

        /// Put a local `event` on the bus as the web layer.
        fn web(&self, event: LocalEvent) {
            let _ = self.tx.send(BusMsg::local("web", event));
        }

        /// Put the session's turn with `hand` on the bus, as the system
        /// actor publishes it.
        fn turn(&self, hand: Handedness) {
            let turn = GolferTurn::new(None, None, hand, Mode::Full);
            let _ = self.tx.send(BusMsg::frp(FrpMessage::new(
                self.state.flighthook_id(),
                turn,
            )));
        }

        /// A `golfer.select` of `hand` from the web layer.
        fn select_hand(&self, hand: Handedness) {
            let mut select = GolferSelect::new("web");
            select.handedness = Some(hand);
            let _ = self.tx.send(BusMsg::frp(FrpMessage::new("web", select)));
        }

        fn mine(&self, m: &BusMsg) -> bool {
            match m {
                BusMsg::Frp { msg, .. } => msg.from == self.frp,
                BusMsg::Local { card, .. } => card == "exputt.0",
            }
        }

        /// Messages of `exputt.0` until `want` matches one, returning them
        /// all (the match last).
        fn until(&mut self, what: &str, want: impl Fn(&BusMsg) -> bool) -> Vec<BusMsg> {
            let deadline = Instant::now() + WAIT;
            let mut seen = Vec::new();
            while Instant::now() < deadline {
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
            panic!("{what} not seen; got {seen:?}");
        }

        fn until_status(&mut self, status: ActorStatus) -> Vec<BusMsg> {
            self.until(&format!("{status:?}"), |m| is_status(m, status))
        }

        /// Messages until a `bus.actor` ready (no blockers) as `ready` and,
        /// when given, with `device_state` = `state`.
        fn until_ready(&mut self, ready: bool, state: Option<&str>) -> Vec<BusMsg> {
            self.until(&format!("ready={ready} {state:?}"), |m| {
                is_ready(m) == Some(ready) && state.is_none_or(|s| device_state(m) == Some(s))
            })
        }

        /// The messages of the next shot, trigger to finish.
        fn next_shot(&mut self) -> Vec<BusMsg> {
            let mut seen = self.until("shot.finished", |m| {
                matches!(body(m), Some(FrpBody::ShotFinished(_)))
            });
            let start = seen
                .iter()
                .rposition(|m| matches!(body(m), Some(FrpBody::ShotTrigger(_))))
                .expect("shot.trigger");
            seen.drain(..start);
            seen
        }

        /// Every message from the card waiting on the bus, without waiting.
        fn drain(&mut self) -> Vec<BusMsg> {
            let mut seen = Vec::new();
            loop {
                match self.rx.try_recv() {
                    Ok(m) if self.mine(&m) => seen.push(m),
                    Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => return seen,
                }
            }
        }

        /// Stop the card and wait for its thread.
        fn stop(&self) {
            assert_eq!(
                self.state.stop_and_wait("exputt.0"),
                crate::state::Stopped::Exited
            );
        }
    }

    impl Drop for Card {
        fn drop(&mut self) {
            let _ = self.state.stop_actor("exputt.0");
            let _ = self
                .state
                .wait_for_exit("exputt.0", Instant::now() + ExPutt::STOP_WAIT);
            self.shutdown.store(true, Ordering::SeqCst);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn body(m: &BusMsg) -> Option<&FrpBody> {
        m.as_frp().map(|f| &f.body)
    }

    fn card_status(m: &BusMsg) -> Option<(ActorStatus, &HashMap<String, String>)> {
        match m.as_local() {
            Some((_, LocalEvent::CardStatus { status, telemetry })) => Some((*status, telemetry)),
            _ => None,
        }
    }

    fn is_status(m: &BusMsg, status: ActorStatus) -> bool {
        card_status(m).is_some_and(|(s, _)| s == status)
    }

    fn bus_actor(m: &BusMsg) -> Option<&BusActor> {
        match body(m) {
            Some(FrpBody::BusActor(a)) => Some(a),
            _ => None,
        }
    }

    /// Whether the card's `bus.actor` says ready (no blockers).
    fn is_ready(m: &BusMsg) -> Option<bool> {
        bus_actor(m).map(|a| a.blockers.is_empty())
    }

    fn device_state(m: &BusMsg) -> Option<&str> {
        bus_actor(m)
            .and_then(|a| a.telemetry.as_ref())
            .and_then(|t| t.get("device_state"))
            .map(String::as_str)
    }

    fn alert(m: &BusMsg) -> Option<(Severity, &str)> {
        match body(m) {
            Some(FrpBody::BusAlert(a)) => Some((a.severity, a.message.as_str())),
            _ => match m.as_local() {
                Some((_, LocalEvent::Alert { severity, message })) => {
                    Some((*severity, message.as_str()))
                }
                _ => None,
            },
        }
    }

    fn alerts(seen: &[BusMsg]) -> Vec<String> {
        seen.iter()
            .filter_map(|m| alert(m).map(|(_, a)| a.to_owned()))
            .collect()
    }

    /// The `phase` values the card's statuses carry in `seen`, in order.
    fn phases(seen: &[BusMsg]) -> Vec<&str> {
        seen.iter()
            .filter_map(|m| card_status(m).and_then(|(_, t)| t.get("phase")))
            .map(String::as_str)
            .collect()
    }

    /// The `device_state` values in `seen`, in order.
    fn device_states(seen: &[BusMsg]) -> Vec<&str> {
        seen.iter().filter_map(device_state).collect()
    }

    /// Readiness as the card last published it in `seen`.
    fn last_ready(seen: &[BusMsg]) -> bool {
        seen.iter().rev().find_map(is_ready).unwrap_or(false)
    }

    /// The putt the device reports for `comment`, read by the driver.
    fn putt(comment: &str) -> PuttResult {
        match Event::parse(&device_event(comment)) {
            Event::PuttResult(p) => p,
            other => panic!("not a putt: {other:?}"),
        }
    }

    /// The launch and club of a shot's messages (trigger first).
    fn accumulate(messages: &[BusMsg]) -> (Launch, Option<ClubData>) {
        let Some(FrpBody::ShotTrigger(trigger)) = body(&messages[0]) else {
            panic!("starts with a trigger: {messages:?}");
        };
        let mut launch = None;
        let mut club = None;
        for m in messages {
            match body(m) {
                Some(FrpBody::ShotLaunch(l)) => {
                    assert_eq!(l.key, trigger.key);
                    launch = Some(l.launch.clone());
                }
                Some(FrpBody::ShotFlight(f)) if f.key == trigger.key => {
                    panic!("a putt has no flight result of its own")
                }
                Some(FrpBody::ShotClub(c)) => {
                    assert_eq!(c.key, trigger.key);
                    club = Some(c.club.clone());
                }
                Some(FrpBody::ShotFinished(f)) => assert_eq!(f.key, trigger.key),
                _ => {}
            }
        }
        (launch.expect("shot.launch"), club)
    }

    /// The stored shape the outputs map.
    fn shot_data(launch: &Launch, club: Option<&ClubData>) -> OutputShot {
        OutputShot {
            launch: Some(launch.clone()),
            club: club.cloned(),
            ..OutputShot::default()
        }
    }

    fn sent(mock: &MockDevice, request: Request) -> bool {
        let json = request.to_json();
        mock.wait_until(WAIT, |packets| packets.iter().any(|p| p.json == json))
    }

    /// Whether `packets`, keepalives aside, are exactly a new round with
    /// `hand`: match end, mat resync, hand, match begin.
    fn round_sent(packets: &[threeputt::testing::ReceivedPacket], hand: Hand) -> bool {
        let want: Vec<String> = [
            Request::MatchEnded,
            Request::StartCamera,
            Request::ResyncToMat,
            Request::StopCamera,
            Request::SetHand(hand),
            Request::MatchBegin,
        ]
        .iter()
        .map(|r| r.to_json())
        .collect();
        let alive = Request::Alive.to_json();
        let sent: Vec<&String> = packets
            .iter()
            .map(|p| &p.json)
            .filter(|j| **j != alive)
            .collect();
        sent == want.iter().collect::<Vec<_>>()
    }

    fn quiet_mock() -> MockDevice {
        MockDevice::spawn(MockConfig {
            heartbeat: None,
            ..MockConfig::default()
        })
        .expect("mock")
    }

    /// A port nothing listens on: connection attempts are refused.
    fn closed_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("addr")
            .port()
    }

    fn now_ms() -> u64 {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis(),
        )
        .expect("ms")
    }

    // -- Session ---------------------------------------------------------------

    #[test]
    fn connects_measures_and_closes_the_session_on_stop() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.1", &mock);

        let seen = card.until_status(ActorStatus::Connected);
        // A configured address skips discovery; the card shows `measuring`
        // while Connected.
        assert_eq!(phases(&seen), ["connecting", "measuring", "measuring"]);
        let (_, telemetry) = card_status(seen.last().unwrap()).unwrap();
        assert_eq!(telemetry["device_info"], "ExPutt RG (127.0.0.1)");

        // Ready once the device says so; the identity goes with it.
        let seen = card.until_ready(true, None);
        let device = bus_actor(seen.last().unwrap())
            .and_then(|a| a.device.as_ref())
            .expect("device info");
        assert_eq!(device.model.as_deref(), Some("ExPutt RG"));
        assert!(
            sent(&mock, Request::SetHand(Hand::Right)),
            "right by default"
        );
        assert!(sent(&mock, Request::MatchBegin));

        card.stop();
        assert!(sent(&mock, Request::MatchEnded), "measuring ended");
        assert!(mock.wait_closed(WAIT), "connection closed");
        let seen = card.until_status(ActorStatus::Disconnected);
        assert!(seen.iter().any(|m| is_ready(m) == Some(false)));
    }

    #[test]
    fn the_card_is_exclusive_and_waits_out_a_session_start() {
        let card = ExPuttActor::new(ExPuttSection::default());
        assert!(!card.replaceable_while_stopping());
        assert_eq!(card.stop_wait(), Duration::from_secs(18));
        assert_eq!(card.stop_wait(), crate::actors::MAX_STOP_WAIT);
        assert_eq!(card.source(), Some(ShotSource::ExPutt));
    }

    #[test]
    fn a_putt_becomes_a_putter_shot_with_speed_direction_path_and_face() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.2", &mock);
        card.until_status(ActorStatus::Connected);

        mock.push_event(PUTT_RESULT_FULL);
        let messages = card.next_shot();
        let expected = putt(PUTT_RESULT_FULL);
        let (ball, club) = accumulate(&messages);

        assert_eq!(
            ball.launch_speed,
            Some(Velocity::MetersPerSecond(expected.ball_speed_mps))
        );
        assert!((ball.launch_speed.unwrap().as_mps() - 2.50).abs() < 0.01);
        assert_eq!(ball.launch_azimuth, Some(expected.launch_direction_deg));
        assert!(ball.launch_azimuth.unwrap() > 1.4, "R1.4 is positive");
        // Neither a launch angle nor spin is measured, so neither is sent.
        assert_eq!(ball.launch_elevation, None);
        assert_eq!(ball.backspin_rpm, None);
        assert_eq!(ball.sidespin_rpm, None);
        assert_eq!(ball.total_spin_rpm, None);

        let club = club.expect("club data");
        assert_eq!(club.path, expected.path_deg);
        assert_eq!(club.face_angle, expected.face_deg);
        assert!(club.path.unwrap() > 0.0 && club.face_angle.unwrap() > 0.0);
        assert_eq!(club.club_speed, None, "the putter speed is not used");

        // What GSPro receives: a putt with ball speed and HLA, no launch
        // angle or spin.
        let gspro = map_shot(&shot_data(&ball, Some(&club)));
        let b = &gspro.ball_data;
        assert!(
            (b.speed - Velocity::MetersPerSecond(expected.ball_speed_mps).as_mph()).abs() < 1e-9
        );
        assert!((b.speed - 5.6).abs() < 0.05, "{} mph", b.speed);
        assert_eq!(b.hla, expected.launch_direction_deg);
        assert_eq!(b.vla, 0.0);
        assert_eq!((b.back_spin, b.side_spin, b.total_spin), (0.0, 0.0, 0.0));
        assert_eq!(b.carry_distance, None);
        assert_eq!(gspro.club_data.path, expected.path_deg.unwrap());
        assert_eq!(gspro.club_data.face_to_target, expected.face_deg.unwrap());
        assert_eq!(gspro.club_data.speed, 0.0);
        assert!(gspro.shot_data_options.contains_ball_data);
    }

    /// The card runs in putting mode whatever the session's mode: its fixed
    /// mode, which the flight model simulates its shots in.
    #[test]
    fn a_putt_is_simulated_in_putting_mode_whatever_the_game_mode() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::on("10.77.0.3", &Arc::new(AtomicU16::new(mock.port())));
        card.select(|s| s.mode = Some(Mode::Full));
        card.start();
        card.until_status(ActorStatus::Connected);
        assert_eq!(card.state.fixed_mode(&card.frp), Some(Mode::Putting));
        assert_eq!(card.state.session.mode(), Mode::Full);
    }

    #[test]
    fn a_pulled_putt_is_negative_left() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.4", &mock);
        card.until_status(ActorStatus::Connected);

        mock.push_event(PUTT_RESULT_PULLED);
        let (ball, club) = accumulate(&card.next_shot());
        assert!(ball.launch_azimuth.unwrap() < -10.0, "L10.3 is negative");
        let club = club.expect("club");
        assert!(club.path.unwrap() < -5.0 && club.face_angle.unwrap() < -10.0);
    }

    #[test]
    fn a_putt_without_putter_data_is_a_ball_only_shot() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.5", &mock);
        card.until_status(ActorStatus::Connected);

        mock.push_event(PUTT_RESULT_BALL_ONLY);
        let messages = card.next_shot();
        assert!(
            !messages
                .iter()
                .any(|m| matches!(body(m), Some(FrpBody::ShotClub(_)))),
            "{messages:?}"
        );
        let expected = putt(PUTT_RESULT_BALL_ONLY);
        let (ball, club) = accumulate(&messages);
        assert!(club.is_none());
        assert_eq!(
            ball.launch_speed,
            Some(Velocity::MetersPerSecond(expected.ball_speed_mps))
        );
        assert_eq!(ball.launch_azimuth, Some(expected.launch_direction_deg));
        let gspro = map_shot(&shot_data(&ball, None));
        assert!(gspro.ball_data.speed > 0.0);
        assert_eq!(gspro.ball_data.hla, expected.launch_direction_deg);
    }

    /// A putt's trigger carries the time the strike arrived, so
    /// the cameras cut the clip at the stroke, not at the result.
    #[test]
    fn a_putt_is_dated_at_its_strike() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.6", &mock);
        card.until_ready(true, None);
        let from = now_ms();
        mock.push_event(BALL_STRUCK);
        card.until_ready(false, Some("ball struck"));
        let to = now_ms();
        std::thread::sleep(Duration::from_millis(300));
        mock.push_event(PUTT_RESULT_FULL);
        let messages = card.next_shot();
        let Some(FrpBody::ShotTrigger(trigger)) = body(&messages[0]) else {
            unreachable!()
        };
        let at = trigger.timestamp.expect("a dated trigger");
        assert!((from..=to).contains(&at), "{from} <= {at} <= {to}");
    }

    /// A putt's export holds the wire messages around it, labeled,
    /// the putt's own result among them.
    #[test]
    fn a_putt_carries_its_wire_messages_as_raw_records() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.7", &mock);
        card.until_status(ActorStatus::Connected);
        mock.push_event(PUTT_RESULT_FULL);
        let messages = card.next_shot();
        let raw: &ShotRaw = messages
            .iter()
            .find_map(|m| match m.as_local() {
                Some((_, LocalEvent::ShotCapture { raw, .. })) => Some(&**raw),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a capture with raw records: {messages:?}"));
        assert_eq!(raw.transport, "exputt-wifi");
        let result = raw
            .frames
            .iter()
            .find(|f| f.label.as_deref() == Some("PuttResult"))
            .expect("the putt's result");
        assert_eq!(result.dir, flighthook::RawDir::Rx);
        assert_eq!(result.hex, None);
        assert_eq!(
            result.text.as_deref().map(str::as_bytes),
            Some(device_event(PUTT_RESULT_FULL).as_slice())
        );
        assert!(
            raw.frames
                .iter()
                .all(|f| f.label.is_some() && f.text.is_some() && f.channel.is_none())
        );
    }

    // -- Hand and Find mat -------------------------------------------------------

    /// A hand change starts a new round, which finds the mat first: not
    /// ready through "finding mat" and "mat found" until the device is ready
    /// again, as after a Find mat, and no reconnect.
    #[test]
    fn the_hand_follows_the_player() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::on("10.77.0.8", &Arc::new(AtomicU16::new(mock.port())));
        card.select(|s| s.handedness = Some(Handedness::Left));
        card.start();
        card.until_status(ActorStatus::Connected);
        mock.push_event(PUTT_READY);
        card.until_ready(true, Some("ready"));
        assert!(sent(&mock, Request::SetHand(Hand::Left)), "starts left");
        let before = mock.received().len();

        // The session's turn: the player is right-handed.
        card.turn(Handedness::Right);
        let mut seen = card.until_ready(false, Some("ready"));
        seen.extend(card.until("mat found", |m| device_state(m) == Some("mat found")));
        mock.push_event(PUTT_READY);
        seen.extend(card.until_ready(true, Some("ready")));
        let mut states = device_states(&seen);
        states.dedup();
        assert_eq!(states, ["ready", "finding mat", "mat found", "ready"]);
        assert!(
            !seen.iter().any(|m| is_status(m, ActorStatus::Reconnecting)),
            "no reconnect"
        );
        assert!(
            mock.wait_until(WAIT, |p| round_sent(&p[before..], Hand::Right)),
            "{:?}",
            mock.received()
        );
    }

    /// A hand set in the UI (a `golfer.select` from `web`) reaches the
    /// ExPutt through the session's turn: a new round with the new hand,
    /// and a repeat of the same hand starts no other.
    #[test]
    fn a_hand_set_in_the_ui_reaches_the_device() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::on("10.77.0.9", &Arc::new(AtomicU16::new(mock.port())));
        card.start_system_actor();
        card.start();
        card.until_status(ActorStatus::Connected);
        assert!(sent(&mock, Request::MatchBegin));
        assert!(sent(&mock, Request::SetHand(Hand::Right)), "starts right");
        let before = mock.received().len();

        card.select_hand(Handedness::Left);
        assert!(
            mock.wait_until(WAIT, |p| round_sent(&p[before..], Hand::Left)),
            "{:?}",
            mock.received()
        );
        let after = mock.received().len();
        // A repeat of the same hand, then the other hand as a sentinel: the
        // bus is in order, so the round for the sentinel is the only one
        // since `after` unless the repeat started one of its own.
        card.select_hand(Handedness::Left);
        card.select_hand(Handedness::Right);
        assert!(
            mock.wait_until(WAIT, |p| round_sent(&p[after..], Hand::Right)),
            "{:?}",
            mock.received()
        );
    }

    #[test]
    fn find_mat_resyncs_the_device_and_is_ready_again() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.10", &mock);
        card.until_status(ActorStatus::Connected);
        mock.push_event(PUTT_READY);
        card.until_ready(true, Some("ready"));
        let before = mock.received().len();

        // A Find mat for another ExPutt is not for this one.
        card.web(LocalEvent::FindMat {
            target: "exputt.1".into(),
        });
        card.web(LocalEvent::FindMat {
            target: "exputt.0".into(),
        });
        let mut seen = card.until_ready(false, Some("ready"));
        seen.extend(card.until("mat found", |m| device_state(m) == Some("mat found")));
        mock.push_event(PUTT_READY);
        seen.extend(card.until_ready(true, Some("ready")));
        let mut states = device_states(&seen);
        states.dedup();
        assert_eq!(states, ["ready", "finding mat", "mat found", "ready"]);
        assert!(
            !seen.iter().any(|m| is_status(m, ActorStatus::Reconnecting)),
            "no reconnect"
        );
        assert!(
            mock.wait_until(WAIT, |p| round_sent(&p[before..], Hand::Right)),
            "{:?}",
            mock.received()
        );
    }

    /// The spacing of mat searches is the driver's: hand changes inside the
    /// gap after the session's own search are one restart to the final
    /// hand, held (not ready) until the gap ends.
    #[test]
    fn hand_changes_inside_the_gap_are_one_restart_to_the_final_hand() {
        let gap = Duration::from_millis(900);
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let port = Arc::new(AtomicU16::new(mock.port()));
        let mut t = tuning(&port);
        t.resync_gap = gap;
        let mut card = Card::new(section("10.77.0.11"), t);
        card.start();
        card.until_status(ActorStatus::Connected);
        let before = mock.received().len();
        for handed in [
            Handedness::Right,
            Handedness::Left,
            Handedness::Right,
            Handedness::Left,
            Handedness::Left,
        ] {
            card.turn(handed);
        }
        // Held: nothing sent yet.
        std::thread::sleep(gap / 3);
        assert!(
            mock.received()[before..]
                .iter()
                .all(|p| p.json == Request::Alive.to_json()),
            "{:?}",
            mock.received()
        );
        // One restart, to the final hand, once the gap is over.
        assert!(
            mock.wait_until(WAIT, |p| round_sent(&p[before..], Hand::Left)),
            "{:?}",
            mock.received()
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(round_sent(&mock.received()[before..], Hand::Left));
        assert!(
            !card
                .drain()
                .iter()
                .any(|m| is_status(m, ActorStatus::Reconnecting))
        );
    }

    // -- Readiness ---------------------------------------------------------------

    #[test]
    fn readiness_follows_the_putt_cycle() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.12", &mock);
        mock.push_event(PUTT_READY);
        let mut seen = card.until_ready(true, Some("ready"));
        assert!(last_ready(&seen), "ready for a putt");

        // Struck: not ready until the device says so again.
        mock.push_event(BALL_STRUCK);
        seen.extend(card.until_ready(false, Some("ball struck")));
        assert!(!last_ready(&seen), "ball struck");

        // The result changes neither value.
        mock.push_event(PUTT_RESULT_FULL);
        let shot = card.until("shot.finished", |m| {
            matches!(body(m), Some(FrpBody::ShotFinished(_)))
        });
        assert!(
            !shot.iter().any(|m| bus_actor(m).is_some()),
            "no bus.actor for the result"
        );
        seen.extend(shot);
        assert!(!last_ready(&seen), "still not ready after the result");

        mock.push_event(PUTT_READY);
        seen.extend(card.until_ready(true, Some("ready")));
        assert!(last_ready(&seen), "ready again");
    }

    #[test]
    fn a_dropped_device_is_not_ready_before_it_reconnects() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.13", &mock);
        mock.push_event(PUTT_READY);
        let mut seen = card.until_ready(true, Some("ready"));
        mock.disconnect();
        let dropped = card.until_status(ActorStatus::Reconnecting);
        let not_ready = dropped.iter().position(|m| is_ready(m) == Some(false));
        assert!(
            not_ready.is_some_and(|i| i < dropped.len() - 1),
            "ready=false before reconnecting: {dropped:?}"
        );
        // Not ready from the bus.actor alone.
        seen.extend(dropped[..dropped.len() - 1].iter().cloned());
        assert!(!last_ready(&seen));
    }

    #[test]
    fn a_silent_device_is_not_ready_until_it_sends_again() {
        // No heartbeat: ready from the device's own report, then silence past
        // the warning threshold (a third of the baseline limit).
        let mock = quiet_mock();
        let mut card = Card::started("10.77.0.14", &mock);
        mock.push_event(PUTT_READY);
        let mut seen = card.until_ready(true, Some("ready"));
        seen.extend(card.until_ready(false, None));
        assert!(!last_ready(&seen), "not ready during silence");

        // It sends again before the limit: ready as the device last said,
        // and the session stands.
        mock.push_event("Test,yy");
        seen.extend(card.until_ready(true, None));
        assert!(last_ready(&seen), "ready again");
        std::thread::sleep(Duration::from_millis(100));
        seen.extend(card.drain());
        assert!(
            !seen.iter().any(|m| is_status(m, ActorStatus::Reconnecting)),
            "no reconnect"
        );
    }

    // -- Reconnects and the stall policy ------------------------------------------

    #[test]
    fn a_dropped_device_is_reconnected_quickly_and_shots_keep_counting() {
        let first = MockDevice::spawn(MockConfig::default()).expect("mock");
        let port = Arc::new(AtomicU16::new(first.port()));
        let mut card = Card::on("10.77.0.15", &port);
        card.start();
        card.until_status(ActorStatus::Connected);
        first.push_event(PUTT_RESULT_FULL);
        assert_eq!(shot_number(&card.next_shot()), 1);

        // The device goes away; the next attempt finds it again (on a new
        // mock, so on a new port), within the quick retry after a measuring
        // session.
        let second = MockDevice::spawn(MockConfig::default()).expect("mock");
        port.store(second.port(), Ordering::SeqCst);
        first.disconnect();
        card.until_status(ActorStatus::Reconnecting);
        let lost = Instant::now();
        card.until_status(ActorStatus::Connected);
        assert!(
            lost.elapsed() < Duration::from_millis(1500),
            "{:?}",
            lost.elapsed()
        );
        assert!(sent(&second, Request::MatchBegin));
        second.push_event(PUTT_RESULT_FULL);
        assert_eq!(shot_number(&card.next_shot()), 2);
    }

    fn shot_number(messages: &[BusMsg]) -> u32 {
        let Some(FrpBody::ShotTrigger(trigger)) = body(&messages[0]) else {
            unreachable!()
        };
        trigger.key.shot_number
    }

    /// The driver's quick retries: refused attempts grow the host's backoff
    /// (1 s, 2 s), a measuring session that drops then reconnects within
    /// 1 s instead of the 3 s the backoff has reached.
    #[test]
    fn a_measuring_session_is_retried_quickly_whatever_the_backoff() {
        let port = Arc::new(AtomicU16::new(closed_port()));
        let mut card = Card::on("10.77.0.16", &port);
        card.start();
        for _ in 0..2 {
            card.until("alert", |m| alert(m).is_some());
        }
        let first = MockDevice::spawn(MockConfig::default()).expect("mock");
        port.store(first.port(), Ordering::SeqCst);
        card.until_status(ActorStatus::Connected);
        let second = MockDevice::spawn(MockConfig::default()).expect("mock");
        port.store(second.port(), Ordering::SeqCst);
        first.disconnect();
        card.until_status(ActorStatus::Reconnecting);
        let lost = Instant::now();
        card.until_status(ActorStatus::Connected);
        let took = lost.elapsed();
        assert!(took < Duration::from_millis(1800), "{took:?}");
    }

    /// The driver's stall wait: after a session lost to silence the next
    /// attempt waits at least the stall wait, longer than the host's
    /// backoff would.
    #[test]
    fn a_silent_device_gets_time_before_the_reconnect() {
        let stall_wait = Duration::from_millis(2500);
        let mock = quiet_mock();
        let second = MockDevice::spawn(MockConfig::default()).expect("mock");
        let port = Arc::new(AtomicU16::new(mock.port()));
        let mut t = tuning(&port);
        t.stall_wait = stall_wait;
        let mut card = Card::new(section("10.77.0.17"), t);
        card.start();
        card.until_status(ActorStatus::Connected);
        port.store(second.port(), Ordering::SeqCst);

        let seen = card.until("silence", |m| {
            alert(m).is_some_and(|(_, message)| message.contains("no data from the device"))
        });
        let lost = Instant::now();
        assert!(seen.iter().any(|m| is_status(m, ActorStatus::Reconnecting)));
        card.until_status(ActorStatus::Connected);
        let waited = lost.elapsed();
        assert!(
            waited >= stall_wait - Duration::from_millis(100),
            "reconnected after {waited:?}"
        );
        assert!(sent(&second, Request::MatchBegin));
    }

    /// A device whose app has stalled: it accepts every connection and
    /// greets, and never answers anything.
    struct Wedged {
        port: u16,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Wedged {
        fn spawn() -> Self {
            use std::io::Write as _;
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let port = listener.local_addr().expect("addr").port();
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                let mut held = Vec::new();
                while !flag.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.write_all(&threeputt::zmtp::greeting(
                                threeputt::zmtp::SOCKET_ROUTER,
                            ));
                            held.push(stream);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
            Self {
                port,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for Wedged {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    /// The driver's alert policy through the host: an "ExPutt error" per
    /// failed attempt, then once the device has not answered for a while one
    /// "not answering" alert, and then quiet; the host adds no alert of its
    /// own.
    #[test]
    fn a_device_that_never_answers_is_reported_once() {
        let wedged = Wedged::spawn();
        let port = Arc::new(AtomicU16::new(wedged.port));
        let mut t = tuning(&port);
        t.not_answering_after = Duration::from_millis(600);
        t.reply_timeout = Duration::from_millis(200);
        let mut card = Card::new(section("10.77.0.18"), t);
        card.start();

        let seen = card.until("not answering", |m| {
            alert(m).is_some_and(|(_, message)| message == NOT_ANSWERING)
        });
        let got = alerts(&seen);
        let (told, errors) = got.split_last().expect("alerts");
        assert_eq!(told, NOT_ANSWERING);
        assert!(!errors.is_empty(), "failed attempts first: {got:?}");
        assert!(
            errors.iter().all(|a| a.starts_with("ExPutt error")),
            "{got:?}"
        );
        let (severity, _) = alert(seen.last().unwrap()).unwrap();
        assert_eq!(severity, Severity::Warn);

        // Attempts go on (the next after the host's 2 s), silently.
        std::thread::sleep(Duration::from_millis(2600));
        assert_eq!(alerts(&card.drain()), Vec::<String>::new());
    }

    #[test]
    fn a_device_that_is_not_there_is_retried() {
        let port = Arc::new(AtomicU16::new(closed_port()));
        let mut card = Card::on("10.77.0.19", &port);
        card.start();
        let seen = card.until("alert", |m| alert(m).is_some());
        assert!(alerts(&seen)[0].starts_with("ExPutt error"));
        assert!(!seen.iter().any(|m| is_status(m, ActorStatus::Connected)));
        // Comes up once the device does.
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        port.store(mock.port(), Ordering::SeqCst);
        card.until_status(ActorStatus::Connected);
    }

    #[test]
    fn a_stop_while_measuring_closes_the_session_at_once() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut card = Card::started("10.77.0.20", &mock);
        card.until_status(ActorStatus::Connected);
        let stopped = Instant::now();
        card.stop();
        let took = stopped.elapsed();
        assert!(took < Duration::from_secs(1), "{took:?}");
        assert!(sent(&mock, Request::MatchEnded), "the session was closed");
    }

    /// Starting a session cannot be interrupted: a stop during a handshake
    /// the device never answers ends when the handshake times out, inside
    /// the card's stop wait.
    #[test]
    fn a_stop_during_a_wedged_session_start_ends_within_the_stop_wait() {
        let wedged = Wedged::spawn();
        let port = Arc::new(AtomicU16::new(wedged.port));
        let mut t = tuning(&port);
        t.reply_timeout = Duration::from_millis(1500);
        let mut card = Card::new(section("10.77.0.21"), t);
        card.start();
        card.until("connecting", |m| {
            phases(std::slice::from_ref(m)) == ["connecting"]
        });
        std::thread::sleep(Duration::from_millis(200));
        let stopped = Instant::now();
        card.stop();
        let took = stopped.elapsed();
        assert!(took < Duration::from_millis(2500), "{took:?}");
        assert!(took < ExPutt::STOP_WAIT, "{took:?}");
    }

    // -- Discovery -------------------------------------------------------------

    #[test]
    fn discovery_without_a_device_hints_once_and_a_stop_ends_it() {
        let port = Arc::new(AtomicU16::new(closed_port()));
        let mut t = tuning(&port);
        t.discovery_hint_after = Duration::from_millis(300);
        let mut card = Card::new(
            ExPuttSection {
                name: "ExPutt".into(),
                ..ExPuttSection::default()
            },
            t,
        );
        card.start();
        let mut seen = card.until("alert", |m| alert(m).is_some());
        assert_eq!(phases(&seen), ["discovering"]);
        // Many more slices, no second hint.
        std::thread::sleep(Duration::from_millis(800));
        seen.extend(card.drain());
        // In the card's words.
        assert_eq!(alerts(&seen), [DISCOVERY_HINT]);
        assert!(!seen.iter().any(|m| is_status(m, ActorStatus::Connected)));

        let stopped = Instant::now();
        card.stop();
        assert!(stopped.elapsed() < Duration::from_secs(1));
        card.until_status(ActorStatus::Disconnected);
    }

    /// A Find mat pressed while the card has no session is for no device:
    /// the next session starts with its own mat search only.
    #[test]
    fn a_find_mat_without_a_session_is_dropped() {
        let port = Arc::new(AtomicU16::new(closed_port()));
        let mut card = Card::on("10.77.0.22", &port);
        card.start();
        card.until("alert", |m| alert(m).is_some());
        card.web(LocalEvent::FindMat {
            target: "exputt.0".into(),
        });
        std::thread::sleep(Duration::from_millis(200));
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        port.store(mock.port(), Ordering::SeqCst);
        card.until_status(ActorStatus::Connected);
        std::thread::sleep(Duration::from_millis(500));
        let searches = mock
            .received()
            .iter()
            .filter(|p| p.json == Request::ResyncToMat.to_json())
            .count();
        assert_eq!(searches, 1, "{:?}", mock.received());
    }

    // -- Config ----------------------------------------------------------------

    /// An address that is not an IP keeps the card from starting
    /// and shows on it.
    #[test]
    fn a_refused_address_shows_on_the_card() {
        let port = Arc::new(AtomicU16::new(closed_port()));
        let mut card = Card::new(section("exputt.local"), tuning(&port));
        card.start();
        let seen = card.until_status(ActorStatus::Disconnected);
        let (_, telemetry) = card_status(seen.last().unwrap()).unwrap();
        assert!(
            telemetry["error"].contains("address") && telemetry["error"].contains("exputt.local"),
            "{telemetry:?}"
        );
        assert!(!seen.iter().any(|m| is_status(m, ActorStatus::Starting)));
    }

    #[test]
    fn an_address_change_restarts_the_card() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-exputt-reconfigure-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let state = Arc::new(state);
        let (tx, _rx) = broadcast::channel(16);
        let sender = BusSender::new("exputt.0".into(), tx, Arc::default());
        let actor = ExPuttActor::new(ExPuttSection::default());
        let set = |address: Option<&str>, lead: Option<f64>| {
            state.system.update(|c| {
                c.exputt.insert(
                    "0".into(),
                    ExPuttSection {
                        impact_lead_s: lead,
                        driver: ExPuttConfig {
                            address: address.map(str::to_string),
                        },
                        ..ExPuttSection::default()
                    },
                );
            });
        };
        let outcome = || actor.reconfigure(&state, &sender);

        set(None, None);
        assert!(matches!(outcome(), ReconfigureOutcome::Applied));
        // Blank is discovery too, and the camera timing is a host field.
        set(Some(""), Some(0.5));
        assert!(matches!(outcome(), ReconfigureOutcome::Applied));
        set(Some("10.0.0.7"), Some(0.5));
        assert!(matches!(outcome(), ReconfigureOutcome::RestartRequired));
        state.system.update(|c| {
            c.exputt.clear();
        });
        assert!(matches!(outcome(), ReconfigureOutcome::RestartRequired));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_mat_is_the_cards_own() {
        let mine = BusMsg::local(
            "web",
            LocalEvent::FindMat {
                target: "exputt.0".into(),
            },
        );
        assert_eq!(find_mat(&mine, "exputt.0"), Some(Action::FindMat));
        assert_eq!(find_mat(&mine, "exputt.1"), None);
        let other = BusMsg::local("web", LocalEvent::ClearShotHistory);
        assert_eq!(find_mat(&other, "exputt.0"), None);
    }

    #[test]
    fn the_discovery_hint_is_in_the_cards_words() {
        let mut hint: FrpBody =
            flightrelay::BusAlert::new(Severity::Warn, threeputt::driver::DISCOVERY_HINT).into();
        card_words(&mut hint);
        let FrpBody::BusAlert(alert) = &hint else {
            unreachable!()
        };
        assert_eq!(alert.message, DISCOVERY_HINT);
    }

    // -- Output pins -------------------------------------------------------------

    /// Every output's numbers for threeputt's recorded putts, a straight
    /// one and a pulled one, through the card on threeputt's driver.
    #[test]
    fn outputs_keep_their_numbers() {
        for (case, address, line) in [
            ("exputt", "10.77.0.23", PUTT_RESULT_FULL),
            ("exputt_pulled", "10.77.0.24", PUTT_RESULT_PULLED),
        ] {
            let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
            let mut card = Card::started(address, &mock);
            card.until_status(ActorStatus::Connected);
            mock.push_event(line);
            let messages = card.until("shot.finished", |m| {
                matches!(body(m), Some(FrpBody::ShotFinished(_)))
            });
            crate::test_ball::assert_outputs_pinned(
                case,
                "exputt.0",
                Some(Mode::Putting),
                &messages,
            );
        }
    }
}
