//! The Trackball card (alpha): thumbswing's device driver ([`Thumbswing`])
//! on [`DriverActor`].
//!
//! The driver owns the device: it opens the trackball or mouse the card's
//! `device` setting names (or the only one there is) on Linux, turns each
//! swing (pull the ball back, push it forward) into a full shot (launch with
//! both spin pairs, then club data), follows the club, the hand and the
//! putting mode the game names, and reports readiness. Every value is
//! synthesized from the swing, so the card is a virtual launch monitor: a
//! tournament round holds its shots, and a swing made while shots are held
//! is dropped and takes no shot number. When the device is unplugged the
//! session stays up, reports `ready=false` and reopens the device by
//! itself. Off Linux the driver reports that the trackball is not supported
//! there and gives up, so a section copied to another computer shows that
//! error on its card. The tuning, the thresholds, the club and the hand
//! apply to the next swing; a change of `device`, `cpi` or `report_limit`
//! restarts the card. The card never grabs the device, so the desktop
//! pointer keeps following the ball, and a new instance does not wait for
//! the old one to let the device go.
//!
//! The card adds its section ([`TrackballSection`]) and its shot source.
//! Its shots cut no camera clip (no impact lead).

use flighthook::TrackballSection;
use thumbswing::driver::Thumbswing;

use crate::actors::driver::{DriverActor, Hooks};
use crate::actors::provenance::ShotSource;

/// The Trackball card: nothing to add to its driver.
#[derive(Debug, Default)]
pub struct TrackballHooks;

impl Hooks<Thumbswing> for TrackballHooks {
    type Section = TrackballSection;
    const SOURCE: ShotSource = ShotSource::Trackball;
}

/// The Trackball actor (`trackball.N`).
pub type TrackballActor = DriverActor<Thumbswing, TrackballHooks>;

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use flighthook::{
        ActorStatus, FlighthookEvent, FlighthookMessage, Handedness, Provenance, Severity,
        ShotAccumulator, ShotData, ShotDetectionMode, TrackballConfig,
    };
    use flightrelay::config::ConfigError;
    use flightrelay::driver::{
        Backoff, ConnectCtx, DeviceCommand, DeviceDriver, DeviceIdentity, DeviceSession,
        DriverInfo, Events, NoAction, SessionEnd,
    };
    use thumbswing::driver::Session;
    use thumbswing::recording::{Pace, Recording, Replay};
    use thumbswing::testing::{StreamBuilder, synthetic_recording};
    use thumbswing::{Client, DeviceInfo, Error, Event, Input, Sample, Shot};
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::gspro::mapper::map_shot;
    use crate::actors::scripted::Handoff;
    use crate::actors::{Actor, ReconfigureOutcome};
    use crate::bus::BusSender;
    use crate::state::round::RoundRequest;
    use crate::state::{GameStateWriter, SystemState};

    /// Longest any test waits for one thing.
    const WAIT: Duration = Duration::from_secs(8);

    /// Opens the input a `device` setting names, with or without the grab.
    type Opener =
        Box<dyn Fn(Option<&str>, bool) -> Result<Box<dyn Input + Send>, Error> + Send + Sync>;

    // -- Inputs ----------------------------------------------------------------

    /// What a test hands a [`FedInput`]: swings, and the plug.
    #[derive(Default)]
    struct Feed {
        swings: Mutex<VecDeque<Vec<Sample>>>,
        unplugged: AtomicBool,
    }

    impl Feed {
        fn swing(&self, samples: Vec<Sample>) {
            self.swings
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push_back(samples);
        }
    }

    /// Clock time after the last report while the input's clock keeps
    /// running, µs: enough for every timer of the segmenter to run out.
    const TAIL_US: u64 = 3_000_000;

    /// A trackball the test drives: each swing handed to its [`Feed`] is
    /// played from the input's clock on, which runs 1 ms per read while a
    /// swing plays (and until its timers have run out) or the device is
    /// away, and stands still otherwise. Like a fast replay, it is not paced
    /// while its clock runs.
    struct FedInput {
        feed: Arc<Feed>,
        queue: VecDeque<Sample>,
        now: u64,
        running_until: u64,
        gone: bool,
        info: DeviceInfo,
    }

    impl FedInput {
        fn new(feed: &Arc<Feed>, info: DeviceInfo) -> Self {
            Self {
                feed: Arc::clone(feed),
                queue: VecDeque::new(),
                now: 1_000_000,
                running_until: 0,
                gone: false,
                info,
            }
        }

        fn running(&self) -> bool {
            !self.queue.is_empty() || self.now < self.running_until || self.gone
        }
    }

    impl Input for FedInput {
        fn try_recv(&mut self) -> Result<Option<Sample>, Error> {
            if self.feed.unplugged.load(Ordering::SeqCst) && !self.gone {
                self.gone = true;
                self.queue.clear();
                return Err(Error::Disconnected);
            }
            if self.queue.is_empty()
                && let Some(swing) = self
                    .feed
                    .swings
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .pop_front()
            {
                let t0 = swing.first().map_or(0, |s| s.t_us);
                let start = self.now + 1000;
                self.queue.extend(
                    swing
                        .iter()
                        .map(|s| Sample::new(start + s.t_us - t0, s.dx, s.dy)),
                );
            }
            match self.queue.front() {
                Some(s) if s.t_us <= self.now => {
                    let s = *s;
                    self.queue.pop_front();
                    self.running_until = s.t_us + TAIL_US;
                    Ok(Some(s))
                }
                _ => Ok(None),
            }
        }

        fn now_us(&mut self) -> u64 {
            if self.running() {
                self.now += 1000;
            }
            self.now
        }

        fn info(&self) -> &DeviceInfo {
            &self.info
        }

        fn reconnect(&mut self) -> Result<(), Error> {
            if self.feed.unplugged.load(Ordering::SeqCst) {
                return Err(Error::Disconnected);
            }
            self.gone = false;
            Ok(())
        }

        fn paced(&self) -> bool {
            !self.running()
        }
    }

    /// A trackball as the evdev backend describes one, with a `by-id` link
    /// that carries a serial number.
    fn trackball_info() -> DeviceInfo {
        DeviceInfo {
            path: PathBuf::from("/dev/input/event7"),
            by_id: vec![PathBuf::from(
                "/dev/input/by-id/usb-Example_Trackball_SN0042-event-mouse",
            )],
            name: "Example Trackball".into(),
            relative_xy: true,
            readable: true,
            kernel_timestamps: true,
            ..DeviceInfo::default()
        }
    }

    /// The device id of [`trackball_info`]: its link's file name.
    const TRACKBALL_ID: &str = "usb-Example_Trackball_SN0042-event-mouse";

    /// A straight pull, a pause, and a push at `counts` per 100 ms (1200 is
    /// full power at 400 CPI), `pull_deg` / `push_deg` from straight.
    fn swing(pull_deg: f64, push_deg: f64, counts: f64) -> Vec<Sample> {
        StreamBuilder::new(125.0)
            .pull(pull_deg, 600.0, 150.0)
            .pause_ms(300.0)
            .push(push_deg, counts, 100.0)
            .build()
    }

    /// Two swings: straight at full power, then a pull 20° left and a push
    /// 5° right at 75 %.
    fn two_swings() -> Recording {
        let samples = StreamBuilder::new(125.0)
            .pull(0.0, 600.0, 150.0)
            .pause_ms(300.0)
            .push(0.0, 1200.0, 100.0)
            .pause_ms(2500.0)
            .pull(20.0, 600.0, 150.0)
            .pause_ms(300.0)
            .push(5.0, 900.0, 100.0)
            .build();
        synthetic_recording(&samples, &[], 400.0, 125.0)
    }

    /// The shots thumbswing's own client makes of `recording` with
    /// `settings`, played to the end.
    fn shots_of(recording: &Recording, settings: &TrackballConfig) -> Vec<Shot> {
        let mut client = Client::with_tuning(
            Replay::new(recording, Pace::Fast),
            settings.config.clone(),
            settings.tuning,
        );
        client.set_club(settings.club);
        client.set_hand(settings.hand);
        let mut shots = Vec::new();
        let polls = recording.samples().last().map_or(0, |s| s.t_us / 1000) + 4000;
        for _ in 0..polls {
            if let Some(Event::Shot(shot)) = client.poll().expect("poll") {
                shots.push(*shot);
            }
        }
        shots
    }

    // -- The driver, with its inputs from the test -----------------------------

    /// Openers by the card's `device` setting (see [`Handoff`]).
    static OPENERS: Handoff<String, Opener> = Handoff::new();

    /// thumbswing's [`Thumbswing`] opening the inputs the test parked under
    /// its `device` setting instead of evdev devices.
    struct ScriptedTrackball(Thumbswing);

    impl DeviceDriver for ScriptedTrackball {
        type Config = TrackballConfig;
        type Error = Error;
        type Action = NoAction;
        type Session = ScriptedSession;
        const STOP_WAIT: Duration = Thumbswing::STOP_WAIT;

        fn info(config: &TrackballConfig) -> DriverInfo {
            Thumbswing::info(config)
        }

        fn new(config: TrackballConfig) -> Result<Self, ConfigError> {
            let key = config.device().unwrap_or_default().to_owned();
            match OPENERS.take(&key) {
                Some(open) => Thumbswing::with_opener(config, open).map(Self),
                None => Thumbswing::new(config).map(Self),
            }
        }

        fn config(&self) -> &TrackballConfig {
            self.0.config()
        }

        fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<ScriptedSession, Error> {
            self.0.connect(ctx).map(ScriptedSession)
        }

        fn retry_delay(
            &mut self,
            ended: &SessionEnd<'_, Error>,
            backoff: &mut Backoff,
            out: &mut Events<'_>,
        ) -> Option<Duration> {
            self.0.retry_delay(ended, backoff, out)
        }

        fn apply_config(&mut self, new: &TrackballConfig) {
            self.0.apply_config(new);
        }
    }

    struct ScriptedSession(Session);

    impl DeviceSession for ScriptedSession {
        type Driver = ScriptedTrackball;

        fn poll(&mut self, timeout: Duration, out: &mut Events<'_>) -> Result<(), Error> {
            self.0.poll(timeout, out)
        }

        fn command(&mut self, cmd: &DeviceCommand) -> Result<(), Error> {
            self.0.command(cmd)
        }

        fn identity(&self) -> &DeviceIdentity {
            self.0.identity()
        }

        fn close(self, out: &mut Events<'_>) -> Result<(), Error> {
            self.0.close(out)
        }
    }

    impl Hooks<ScriptedTrackball> for TrackballHooks {
        type Section = TrackballSection;
        const SOURCE: ShotSource = ShotSource::Trackball;
    }

    /// A new card's section reaching the test's inputs by `device`.
    fn section(device: &str) -> TrackballSection {
        let mut section = TrackballSection::default();
        section.driver.device = Some(device.into());
        section
    }

    /// An opener handing out `inputs` in turn, then failing as if the
    /// device were gone.
    fn opener(inputs: Vec<Box<dyn Input + Send>>) -> Opener {
        let inputs = Mutex::new(VecDeque::from(inputs));
        Box::new(move |_device, _grab| {
            inputs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .ok_or_else(|| Error::NotFound("no more test inputs".into()))
        })
    }

    // -- A running card --------------------------------------------------------

    /// A running Trackball card (`trackball.0`) and the bus.
    struct Card {
        dir: PathBuf,
        state: Arc<SystemState>,
        /// Game state's writer, until a system actor takes it.
        writer: Option<GameStateWriter>,
        tx: broadcast::Sender<FlighthookMessage>,
        rx: broadcast::Receiver<FlighthookMessage>,
        shutdown: Arc<AtomicBool>,
        section: TrackballSection,
    }

    impl Card {
        /// State and bus for `section`, its driver opening with `open`; the
        /// card not started yet.
        fn new(section: TrackballSection, open: Opener) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flighthook-trackball-{}",
                crate::state::config::generate_id()
            ));
            let (state, writer) = SystemState::new(dir.join("config.toml"));
            state.system.update(|c| {
                c.trackball.insert("0".into(), section.clone());
            });
            OPENERS.put(section.driver.device().unwrap_or_default().to_owned(), open);
            let (tx, rx) = broadcast::channel(4096);
            Self {
                dir,
                state: Arc::new(state),
                writer: Some(writer),
                tx,
                rx,
                shutdown: Arc::default(),
                section,
            }
        }

        /// A card on a trackball the test drives through `feed`, started.
        fn fed(device: &str, feed: &Arc<Feed>) -> Self {
            let mut card = Self::new(
                section(device),
                opener(vec![Box::new(FedInput::new(feed, trackball_info()))]),
            );
            card.start();
            card
        }

        /// Start a system actor on the bus, holding game state's writer:
        /// the club and the hand then come from `club_info` / `hand_info`.
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
                "trackball.0".into(),
                Box::new(DriverActor::<ScriptedTrackball, TrackballHooks>::new(
                    self.section.clone(),
                )),
                &self.state,
                &self.tx,
            );
        }

        /// Put `event` on the bus as the game (`web`).
        fn game(&self, event: FlighthookEvent) {
            let _ = self.tx.send(FlighthookMessage::new(event).actor("web"));
        }

        /// Messages of `trackball.0` until `want` matches one, returning
        /// them all (the match last).
        fn until(
            &mut self,
            what: &str,
            want: impl Fn(&FlighthookMessage) -> bool,
        ) -> Vec<FlighthookMessage> {
            let deadline = Instant::now() + WAIT;
            let mut seen = Vec::new();
            while Instant::now() < deadline {
                match self.rx.try_recv() {
                    Ok(m) if m.actor == "trackball.0" => {
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
            panic!(
                "{what} not seen; got {:?}",
                seen.iter().map(|m| &m.event).collect::<Vec<_>>()
            );
        }

        fn until_status(&mut self, status: ActorStatus) -> Vec<FlighthookMessage> {
            self.until(&format!("{status:?}"), |m| is_status(m, status))
        }

        /// Messages until a telemetry key `key` reads `value`.
        fn until_telemetry(&mut self, key: &str, value: &str) -> Vec<FlighthookMessage> {
            self.until(&format!("{key}={value}"), |m| {
                telemetry_of(m).is_some_and(|t| t.get(key).map(String::as_str) == Some(value))
            })
        }

        /// The messages of the next shot, trigger to finish.
        fn next_shot(&mut self) -> Vec<FlighthookMessage> {
            let mut seen = self.until("shot_finished", |m| {
                matches!(m.event, FlighthookEvent::ShotFinished { .. })
            });
            let start = seen
                .iter()
                .rposition(|m| matches!(m.event, FlighthookEvent::ShotTrigger { .. }))
                .expect("shot_trigger");
            seen.drain(..start);
            seen
        }

        /// Every message from the card waiting on the bus, without waiting.
        fn drain(&mut self) -> Vec<FlighthookMessage> {
            let mut seen = Vec::new();
            loop {
                match self.rx.try_recv() {
                    Ok(m) if m.actor == "trackball.0" => seen.push(m),
                    Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => return seen,
                }
            }
        }

        /// Take `section` live, as a save does.
        fn reconfigure(&self, section: &TrackballSection) -> ReconfigureOutcome {
            self.state.system.update(|c| {
                c.trackball.insert("0".into(), section.clone());
            });
            let sender = BusSender::new("trackball.0".into(), self.tx.clone(), Arc::default());
            self.state
                .reconfigure_actor("trackball.0", &self.state, &sender)
                .expect("registered")
        }

        /// Stop the card and wait for its thread.
        fn stop(&self) {
            assert_eq!(
                self.state.stop_and_wait("trackball.0"),
                crate::state::Stopped::Exited
            );
        }
    }

    impl Drop for Card {
        fn drop(&mut self) {
            let _ = self.state.stop_actor("trackball.0");
            let _ = self
                .state
                .wait_for_exit("trackball.0", Instant::now() + Thumbswing::STOP_WAIT);
            self.shutdown.store(true, Ordering::SeqCst);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn is_status(m: &FlighthookMessage, status: ActorStatus) -> bool {
        matches!(&m.event, FlighthookEvent::ActorStatus { status: s, .. } if *s == status)
    }

    fn telemetry_of(m: &FlighthookMessage) -> Option<&HashMap<String, String>> {
        match &m.event {
            FlighthookEvent::DeviceTelemetry {
                telemetry: Some(t), ..
            } => Some(t),
            _ => None,
        }
    }

    fn alerts(seen: &[FlighthookMessage]) -> Vec<(Severity, String)> {
        seen.iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::Alert {
                    severity, message, ..
                } => Some((*severity, message.clone())),
                _ => None,
            })
            .collect()
    }

    /// The shot the messages add up to, as the bridges accumulate it.
    fn accumulate(messages: &[FlighthookMessage]) -> ShotData {
        let FlighthookEvent::ShotTrigger { key, .. } = &messages[0].event else {
            panic!("starts with a trigger: {messages:?}");
        };
        let mut acc = ShotAccumulator::new("trackball.0".into(), key.clone());
        for m in messages {
            match &m.event {
                FlighthookEvent::Launch {
                    key: k,
                    launch,
                    derived,
                } => {
                    assert_eq!(k, key);
                    acc.set_launch(*launch.clone(), derived.as_deref().cloned());
                }
                FlighthookEvent::FlightResult { .. } => panic!("no flight result"),
                FlighthookEvent::ClubPath { key: k, club } => {
                    assert_eq!(k, key);
                    acc.set_club(*club.clone());
                }
                FlighthookEvent::ShotFinished { key: k, .. } => assert_eq!(k, key),
                _ => {}
            }
        }
        acc.finish().expect("shot data")
    }

    fn shot_number(messages: &[FlighthookMessage]) -> u32 {
        match &messages[0].event {
            FlighthookEvent::ShotTrigger { key, .. } => key.shot_number,
            other => panic!("not a trigger: {other:?}"),
        }
    }

    // -- Tests -----------------------------------------------------------------

    /// A recording replayed through the card: the card connects as the
    /// trackball, and each swing is the shot thumbswing makes of it,
    /// simulated by flighthook, with no flight result and no raw frames.
    #[test]
    fn a_replayed_recording_is_the_cards_shots() {
        let recording = two_swings();
        let mut card = Card::new(
            section("replay-two-swings"),
            opener(vec![Box::new(Replay::new(&recording, Pace::Fast))]),
        );
        card.start();

        let seen = card.until_status(ActorStatus::Connected);
        let FlighthookEvent::ActorStatus { telemetry, .. } = &seen.last().unwrap().event else {
            unreachable!()
        };
        assert_eq!(telemetry["device_info"], "thumbswing (synthetic trackball)");
        assert!(
            seen.iter()
                .filter_map(|m| match &m.event {
                    FlighthookEvent::ActorStatus { telemetry, .. } => telemetry.get("phase"),
                    _ => None,
                })
                .any(|p| p == "connecting"),
            "{seen:?}"
        );

        let seen = card.until_telemetry("ready", "true");
        let identity = seen
            .iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::DeviceTelemetry {
                    manufacturer,
                    model,
                    source,
                    ..
                } => Some((manufacturer, model, source, &m.device)),
                _ => None,
            })
            .expect("identity");
        assert_eq!(identity.0.as_deref(), Some("divotmaker"));
        assert_eq!(identity.1.as_deref(), Some("thumbswing trackball"));
        assert_eq!(*identity.2, Some(Provenance::Virtual));
        assert_eq!(identity.3.as_deref(), Some("synthetic trackball"));
        let mut keys = HashMap::new();
        for t in seen.iter().filter_map(telemetry_of) {
            keys.extend(t.clone());
        }
        assert_eq!(keys["synthetic"], "true");
        assert_eq!(keys["input"], "synthetic trackball");
        assert_eq!(keys["cpi"], "400");
        assert_eq!(keys["profile"], "7I");
        assert_eq!(keys["handed"], "rh");

        let expected = shots_of(&recording, &card.section.driver);
        assert_eq!(expected.len(), 2);
        for (n, want) in expected.iter().enumerate() {
            let messages = card.next_shot();
            assert_eq!(shot_number(&messages), u32::try_from(n + 1).unwrap());
            let kinds: Vec<&str> = messages
                .iter()
                .map(|m| match &m.event {
                    FlighthookEvent::ShotTrigger { .. } => "trigger",
                    FlighthookEvent::Launch { .. } => "launch",
                    FlighthookEvent::ClubPath { .. } => "club",
                    FlighthookEvent::ShotFinished { .. } => "finished",
                    _ => "other",
                })
                .filter(|k| *k != "other")
                .collect();
            assert_eq!(kinds, ["trigger", "club", "launch", "finished"]);
            let FlighthookEvent::ShotTrigger { timestamp, .. } = &messages[0].event else {
                unreachable!()
            };
            assert!(timestamp.is_some(), "dated at the swing");
            assert!(
                messages
                    .iter()
                    .all(|m| m.provenance.is_none() || m.provenance == Some(Provenance::Virtual))
            );
            assert_eq!(messages[0].provenance, Some(Provenance::Virtual));

            let shot = accumulate(&messages);
            assert_eq!(
                shot.launch.as_ref(),
                Some(&thumbswing::driver::launch(&want.launch)),
                "shot {n}"
            );
            assert_eq!(
                shot.club.as_ref(),
                Some(&thumbswing::driver::club_data(want)),
                "shot {n}"
            );
            let derived = shot.derived.as_ref().expect("simulated");
            assert!(derived.carry_distance.is_some_and(|d| d.as_meters() > 50.0));
            assert!(!derived.spin_estimated);

            let FlighthookEvent::ShotFinished { raw, .. } = &messages.last().unwrap().event else {
                unreachable!()
            };
            let raw = raw.as_ref().expect("raw");
            assert!(raw.frames.is_empty());
            assert_eq!(
                raw.unavailable.as_deref(),
                Some("thumbswing device has no wire protocol")
            );
        }
        card.stop();
    }

    /// The card is virtual, waits for nothing while stopping (it holds no
    /// grab) and stops within a second, also from a section that reads
    /// `grab = true`.
    #[test]
    fn the_card_is_virtual_and_never_waits_for_a_grab() {
        let card = TrackballActor::new(TrackballSection::default());
        assert_eq!(card.source(), Some(ShotSource::Trackball));
        assert_eq!(ShotSource::Trackball.provenance(), Provenance::Virtual);
        assert!(card.replaceable_while_stopping());
        assert_eq!(card.stop_wait(), Duration::from_secs(1));
        let read: TrackballSection =
            toml::from_str("name = \"Trackball\"\ngrab = true\n").expect("reads");
        assert!(TrackballActor::new(read).replaceable_while_stopping());
        let mut built = TrackballSection::default();
        built.driver.config.grab = true;
        assert!(TrackballActor::new(built).replaceable_while_stopping());
    }

    /// A section with `grab = true` opens the device without the grab.
    #[test]
    fn a_grab_key_opens_the_device_without_the_grab() {
        let feed = Arc::new(Feed::default());
        let grabs = Arc::new(Mutex::new(Vec::new()));
        let mut section: TrackballSection =
            toml::from_str("name = \"Trackball\"\ndevice = \"fed-no-grab\"\ngrab = true\n")
                .expect("reads");
        section.driver.config.grab = true;
        let input: Box<dyn Input + Send> = Box::new(FedInput::new(&feed, trackball_info()));
        let inputs = Mutex::new(Some(input));
        let seen = Arc::clone(&grabs);
        let open: Opener = Box::new(move |_device, grab| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(grab);
            inputs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .ok_or_else(|| Error::NotFound("no more test inputs".into()))
        });
        let mut card = Card::new(section, open);
        card.start();
        card.until_telemetry("ready", "true");
        assert_eq!(
            *grabs.lock().unwrap_or_else(PoisonError::into_inner),
            [false]
        );
        card.stop();
    }

    /// The club and the hand the game names reach the next swing, and the
    /// putting mode is the putter, rolled on the green.
    #[test]
    fn the_club_hand_and_putting_mode_follow_the_game() {
        let feed = Arc::new(Feed::default());
        let mut card = Card::fed("fed-club-hand", &feed);
        card.start_system_actor();
        card.until_telemetry("ready", "true");

        card.game(FlighthookEvent::ClubInfo {
            club_info: flighthook::ClubInfo {
                club: flighthook::Club::Driver,
            },
        });
        card.until_telemetry("profile", "DR");
        card.game(FlighthookEvent::HandInfo {
            hand_info: flighthook::HandInfo {
                handed: Handedness::Left,
            },
        });
        card.until_telemetry("handed", "lh");
        feed.swing(swing(0.0, 0.0, 1200.0));
        let drive = accumulate(&card.next_shot());
        let club = drive.club.as_ref().expect("club");
        // A full-power drive: the default 100 mph driver speed.
        assert_eq!(
            club.club_speed,
            Some(flighthook::Velocity::MilesPerHour(100.0))
        );
        assert!(drive.launch.as_ref().unwrap().launch_elevation.unwrap() > 8.0);

        card.game(FlighthookEvent::ClubInfo {
            club_info: flighthook::ClubInfo {
                club: flighthook::Club::Putter,
            },
        });
        card.until_telemetry("profile", "PT");
        feed.swing(swing(0.0, 0.0, 1200.0));
        let putt = accumulate(&card.next_shot());
        let launch = putt.launch.as_ref().expect("launch");
        assert_eq!(launch.launch_elevation, Some(0.3));
        assert_eq!(launch.backspin_rpm, Some(0));
        let total = putt
            .derived
            .as_ref()
            .and_then(|d| d.total_distance)
            .expect("rolled")
            .as_meters();
        // 16.5 mph rolls about 56 ft on the green.
        assert!((15.0..19.0).contains(&total), "{total} m");
        card.stop();
    }

    /// During a round the card's swings are dropped and take no shot
    /// number; after it the next swing is shot 1.
    #[test]
    fn a_round_holds_the_trackball() {
        let feed = Arc::new(Feed::default());
        let mut card = Card::fed("fed-round", &feed);
        card.until_telemetry("ready", "true");
        let guard = card
            .state
            .round
            .begin_round(&RoundRequest {
                holder: "Pureplay",
                feeds: &[],
            })
            .expect("a round");
        // A swing at the input's own pace takes a few milliseconds here:
        // give the card a few passes to see the round first, and after it.
        let passes = Duration::from_millis(300);
        std::thread::sleep(passes);
        feed.swing(swing(0.0, 0.0, 1200.0));
        std::thread::sleep(passes);
        let held = card.drain();
        assert!(
            held.iter()
                .all(|m| !matches!(m.event, FlighthookEvent::ShotTrigger { .. })),
            "{held:?}"
        );
        drop(guard);
        std::thread::sleep(passes);
        feed.swing(swing(0.0, 0.0, 1200.0));
        assert_eq!(shot_number(&card.next_shot()), 1, "no number taken");
        card.stop();
    }

    /// An unplugged trackball is not ready, and is ready again when it is
    /// back, without the card leaving Connected.
    #[test]
    fn an_unplugged_trackball_is_not_ready_until_it_is_back() {
        let feed = Arc::new(Feed::default());
        let mut card = Card::fed("fed-unplug", &feed);
        card.until_telemetry("ready", "true");
        feed.unplugged.store(true, Ordering::SeqCst);
        let seen = card.until_telemetry("ready", "false");
        feed.unplugged.store(false, Ordering::SeqCst);
        let mut seen_all = seen;
        seen_all.extend(card.until_telemetry("ready", "true"));
        assert!(
            !seen_all.iter().any(|m| matches!(
                &m.event,
                FlighthookEvent::ActorStatus { status, .. } if *status != ActorStatus::Connected
            )),
            "{seen_all:?}"
        );
        feed.swing(swing(0.0, 0.0, 1200.0));
        assert_eq!(shot_number(&card.next_shot()), 1);
        card.stop();
    }

    /// The tuning, the thresholds, the club and the hand apply to the next
    /// swing; `device`, `cpi` and `report_limit` restart the card, and a
    /// changed `grab` changes nothing.
    #[test]
    fn tuning_applies_live_and_device_settings_restart() {
        let feed = Arc::new(Feed::default());
        let mut card = Card::fed("fed-live", &feed);
        card.until_telemetry("ready", "true");
        feed.swing(swing(0.0, 0.0, 1200.0));
        let first = accumulate(&card.next_shot());
        let speed = |shot: &ShotData| shot.club.as_ref().unwrap().club_speed.unwrap().as_mph();
        assert!((speed(&first) - 80.0).abs() < 0.05, "a 7-iron at 100 mph");

        let mut live = card.section.clone();
        live.name = "Arcade".into();
        live.driver.tuning.max_club_speed = 120.0;
        live.driver.config.thresholds.coil_timeout_s = 20.0;
        live.driver.club = thumbswing::Club::Driver;
        live.driver.hand = thumbswing::Hand::Left;
        assert!(matches!(
            card.reconfigure(&live),
            ReconfigureOutcome::Applied
        ));
        card.until_telemetry("profile", "DR");
        feed.swing(swing(0.0, 0.0, 1200.0));
        let second = accumulate(&card.next_shot());
        assert!((speed(&second) - 120.0).abs() < 0.05, "{}", speed(&second));

        let restarts: [fn(&mut TrackballConfig); 3] = [
            |d| d.device = Some("another".into()),
            |d| d.config.cpi = 800.0,
            |d| d.config.report_limit = 2047,
        ];
        for change in restarts {
            let mut section = live.clone();
            change(&mut section.driver);
            assert!(matches!(
                card.reconfigure(&section),
                ReconfigureOutcome::RestartRequired
            ));
        }
        let mut grab = live.clone();
        grab.driver.config.grab = true;
        assert!(matches!(
            card.reconfigure(&grab),
            ReconfigureOutcome::Applied
        ));
        card.stop();
    }

    /// A setting the driver refuses keeps the card from starting and shows
    /// on it.
    #[test]
    fn a_refused_setting_shows_on_the_card() {
        let mut section = section("refused-cpi");
        section.driver.config.cpi = 0.0;
        let mut card = Card::new(section, opener(Vec::new()));
        card.start();
        let seen = card.until_status(ActorStatus::Disconnected);
        assert_eq!(seen.len(), 1, "no Starting, no connect");
        assert!(
            matches!(&seen[0].event, FlighthookEvent::ActorStatus { telemetry, .. }
                if telemetry["error"] == "cpi: must be greater than 0"),
            "{seen:?}"
        );
    }

    /// A trackball that cannot be opened is alerted with what to do and
    /// retried; the card reads Starting, never Connected, and shows the
    /// driver's error as its `error` row.
    #[test]
    fn a_trackball_that_cannot_be_opened_is_retried() {
        let mut card = Card::new(section("missing"), opener(Vec::new()));
        card.start();
        let seen = card.until("an alert", |m| {
            matches!(m.event, FlighthookEvent::Alert { .. })
        });
        assert_eq!(
            alerts(&seen),
            [(
                Severity::Warn,
                "no trackball or mouse matches the `device` setting".to_owned()
            )]
        );
        let seen = card.until("a second alert", |m| {
            matches!(m.event, FlighthookEvent::Alert { .. })
        });
        assert!(
            !seen.iter().any(|m| is_status(m, ActorStatus::Connected)),
            "{seen:?}"
        );
        // What the web layer holds for the card, and so what any UI shows.
        let mut held = flighthook::ActorStatusResponse::new("Trackball", ActorStatus::Starting);
        for m in &seen {
            if let FlighthookEvent::ActorStatus { status, telemetry } = &m.event {
                held.apply_status(*status, telemetry, 0);
            }
        }
        assert_eq!(held.status, ActorStatus::Starting);
        assert_eq!(
            held.telemetry.get("error").map(String::as_str),
            Some("no input device found: no more test inputs")
        );
        card.stop();
    }

    /// The card on the real driver, naming a device nobody has: on Linux it
    /// is alerted and retried; elsewhere the trackball is not supported, so
    /// the driver gives up and the card shows why.
    #[test]
    fn the_real_driver_names_what_is_wrong() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-trackball-{}",
            crate::state::config::generate_id()
        ));
        let _dir = crate::actors::web::tests::TempDir(dir.clone());
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let state = Arc::new(state);
        let (tx, mut rx) = broadcast::channel(256);
        let section = section("flighthook-test-no-such-trackball");
        crate::actors::start_actor(
            "trackball.0".into(),
            Box::new(TrackballActor::new(section)),
            &state,
            &tx,
        );
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        let done = |seen: &[FlighthookMessage]| {
            if cfg!(target_os = "linux") {
                !alerts(seen).is_empty()
            } else {
                seen.iter().any(|m| is_status(m, ActorStatus::Disconnected))
            }
        };
        while !done(&seen) && Instant::now() < deadline {
            match rx.try_recv() {
                Ok(m) => seen.push(m),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        if cfg!(target_os = "linux") {
            assert_eq!(
                alerts(&seen),
                [(
                    Severity::Warn,
                    "no trackball or mouse matches the `device` setting".to_owned()
                )]
            );
        } else {
            assert_eq!(
                alerts(&seen)[0].1,
                "trackball input is not supported on this platform (Linux only)"
            );
            assert!(seen.iter().any(|m| matches!(&m.event,
                FlighthookEvent::ActorStatus { status: ActorStatus::Disconnected, telemetry }
                    if telemetry["error"].contains("not supported"))));
        }
        let _ = state.stop_and_wait("trackball.0");
    }

    /// The shot export names the device by kind, maker, model and version
    /// only: neither the device id (a `by-id` name, which can carry a
    /// serial number) nor the `device` setting reaches it.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_export_names_no_device() {
        let feed = Arc::new(Feed::default());
        let mut card = Card::new(
            section(TRACKBALL_ID),
            opener(vec![Box::new(FedInput::new(&feed, trackball_info()))]),
        );
        card.start();
        let mut seen = card.until_telemetry("ready", "true");
        assert!(
            seen.iter()
                .any(|m| m.device.as_deref() == Some(TRACKBALL_ID))
        );
        feed.swing(swing(0.0, 0.0, 1200.0));
        seen.extend(card.next_shot());
        let config = card.state.system.snapshot();
        let export = crate::actors::web::tests::export_lines(&config, &seen).await;
        assert_eq!(export.len(), 1);
        assert!(!export[0].contains("SN0042"), "{}", export[0]);
        assert!(!export[0].contains("event-mouse"), "{}", export[0]);
        let line: serde_json::Value = serde_json::from_str(&export[0]).expect("json");
        assert_eq!(
            line["device"],
            serde_json::json!({
                "kind": "trackball",
                "manufacturer": "divotmaker",
                "model": "thumbswing trackball",
                "firmware": "0.0.1"
            })
        );
        assert_eq!(line["settings"]["device"], serde_json::json!({}));
        card.stop();
    }

    /// Every output's numbers for a trackball drive and putt
    /// (`output_pins.txt`).
    #[test]
    fn outputs_keep_their_numbers() {
        for (case, device, mode, club, pull, push, counts) in [
            (
                "trackball_full_swing",
                "pins-drive",
                ShotDetectionMode::Full,
                thumbswing::Club::Driver,
                20.0,
                5.0,
                900.0,
            ),
            (
                "trackball_putt",
                "pins-putt",
                ShotDetectionMode::Putting,
                thumbswing::Club::Iron7,
                0.0,
                -3.0,
                600.0,
            ),
        ] {
            let feed = Arc::new(Feed::default());
            let mut section = section(device);
            section.driver.club = club;
            let mut card = Card::new(
                section,
                opener(vec![Box::new(FedInput::new(&feed, trackball_info()))]),
            );
            card.writer.as_ref().expect("writer").set_mode(mode);
            card.start();
            card.until_telemetry("ready", "true");
            feed.swing(swing(pull, push, counts));
            let messages = card.next_shot();
            crate::test_ball::assert_outputs_pinned(case, &messages);
            let shot = accumulate(&messages);
            let gspro = map_shot(&shot);
            assert!(gspro.shot_data_options.contains_ball_data);
            card.stop();
        }
    }
}
