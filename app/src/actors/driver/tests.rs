use std::collections::BTreeSet;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use flightrelay::config::{ConfigError, is_default};
use flightrelay::driver::{RawProfile, Stopped};
use flightrelay::{Basis, ClubData, Distance, FaceImpact, Severity, Source, Velocity};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::*;
use crate::actors::web::tests::TempDir;
use crate::state::session::SessionWriter as GameStateWriter;

// -- A scripted fake driver ---------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct FakeConfig {
    /// The script the driver follows ([`SCRIPTS`]).
    #[serde(skip_serializing_if = "is_default")]
    script: u64,
    /// Applied live.
    #[serde(skip_serializing_if = "is_default")]
    live: u32,
    /// Needs a new session.
    #[serde(skip_serializing_if = "is_default")]
    restart: u32,
    /// Refused by `validate`.
    #[serde(skip_serializing_if = "is_default")]
    bad: bool,
}

impl DriverConfig for FakeConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.bad {
            Err(ConfigError::field("bad", "must be false"))
        } else {
            Ok(())
        }
    }

    fn restart_needed(old: &Self, new: &Self) -> bool {
        Self {
            live: 0,
            ..old.clone()
        } != Self {
            live: 0,
            ..new.clone()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct FakeSection {
    #[serde(default)]
    name: String,
    #[serde(flatten)]
    driver: FakeConfig,
}

impl DeviceSection for FakeSection {
    type Driver = FakeConfig;
    const KIND: &'static str = "fake";

    fn driver(&self) -> &FakeConfig {
        &self.driver
    }

    fn sections(_: &flighthook::FlighthookConfig) -> &HashMap<String, Self> {
        static NONE: LazyLock<HashMap<String, FakeSection>> = LazyLock::new(HashMap::new);
        &NONE
    }
}

/// What a connect does.
enum Connect {
    Fail(&'static str),
    Session(Vec<Step>),
    /// Ticks until stopped.
    Block,
    /// Shows a phase, then does the inner connect.
    Phase(&'static str, Box<Connect>),
}

/// What one session step does; an empty list idles.
enum Step {
    Emit(Vec<DeviceEvent>),
    Fail(&'static str),
    Panic,
}

#[derive(Default)]
struct Script {
    connects: VecDeque<Connect>,
    /// `live` of every config applied.
    applied: Vec<u32>,
    /// Turn hints the sessions took.
    commands: Vec<TurnHints>,
    /// The turn hints at each connect.
    latched: Vec<TurnHints>,
    closes: u32,
    /// The retry delay the driver asks for; 10 ms when `None`.
    retry: Option<Duration>,
    /// Told when a connect begins.
    entered: Option<std::sync::mpsc::Sender<()>>,
}

static SCRIPTS: LazyLock<Mutex<HashMap<u64, Arc<Mutex<Script>>>>> = LazyLock::new(Mutex::default);
static NEXT_SCRIPT: AtomicU64 = AtomicU64::new(1);

fn script(connects: Vec<Connect>) -> (u64, Arc<Mutex<Script>>) {
    let id = NEXT_SCRIPT.fetch_add(1, Ordering::SeqCst);
    let script = Arc::new(Mutex::new(Script {
        connects: connects.into(),
        ..Script::default()
    }));
    lock(&SCRIPTS).insert(id, Arc::clone(&script));
    (id, script)
}

#[derive(Debug)]
struct FakeError(String);

impl std::fmt::Display for FakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FakeError {}

impl From<Stopped> for FakeError {
    fn from(_: Stopped) -> Self {
        Self("stopped".into())
    }
}

struct FakeDriver {
    config: FakeConfig,
    script: Arc<Mutex<Script>>,
}

struct FakeSession {
    identity: DeviceIdentity,
    steps: VecDeque<Step>,
    script: Arc<Mutex<Script>>,
}

fn is_bulk(frame: &RawFrame) -> bool {
    frame.label.as_deref() == Some("bulk")
}

impl FrpDriver for FakeDriver {
    type Config = FakeConfig;
    type Error = FakeError;
    type Action = flightrelay::driver::NoAction;
    type Session = FakeSession;
    const STOP_WAIT: Duration = Duration::from_secs(1);

    fn info(_: &FakeConfig) -> DriverInfo {
        DriverInfo::new("fake", "Fake", Source::Measured)
            .with_raw(RawProfile::new("fake-tcp", 4, 8, 16, is_bulk, false))
    }

    fn new(config: FakeConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        let script = lock(&SCRIPTS)
            .get(&config.script)
            .cloned()
            .unwrap_or_default();
        Ok(Self { config, script })
    }

    fn config(&self) -> &FakeConfig {
        &self.config
    }

    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<FakeSession, FakeError> {
        ctx.tick()?;
        let mut next = {
            let mut script = lock(&self.script);
            script.latched.push(*ctx.hints());
            if let Some(entered) = &script.entered {
                let _ = entered.send(());
            }
            script.connects.pop_front()
        };
        while let Some(Connect::Phase(phase, inner)) = next {
            ctx.out.phase(phase);
            next = Some(*inner);
        }
        match next {
            Some(Connect::Fail(why)) => Err(FakeError(why.into())),
            Some(Connect::Session(steps)) => Ok(FakeSession {
                identity: DeviceIdentity::new("FAKE-1")
                    .with_manufacturer("Birdie Labs")
                    .with_model("Eagle"),
                steps: steps.into(),
                script: Arc::clone(&self.script),
            }),
            Some(Connect::Block | Connect::Phase(..)) | None => loop {
                ctx.tick()?;
                std::thread::sleep(Duration::from_millis(20));
            },
        }
    }

    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, FakeError>,
        _: &mut Backoff,
        out: &mut Events<'_>,
    ) -> Option<Duration> {
        out.alert(Severity::Warn, ended.error.to_string());
        Some(
            lock(&self.script)
                .retry
                .unwrap_or(Duration::from_millis(10)),
        )
    }

    fn apply_config(&mut self, new: &FakeConfig) {
        lock(&self.script).applied.push(new.live);
        self.config = new.clone();
    }
}

impl FrpSession for FakeSession {
    type Error = FakeError;
    type Action = flightrelay::driver::NoAction;

    fn step(&mut self, _: Instant, out: &mut Events<'_>) -> Result<Activity, FakeError> {
        match self.steps.pop_front() {
            Some(Step::Emit(events)) => {
                for event in events {
                    out.emit(event);
                }
                Ok(Activity::Active)
            }
            Some(Step::Fail(why)) => Err(FakeError(why.into())),
            Some(Step::Panic) => panic!("a decoder bug"),
            None => Ok(Activity::Idle),
        }
    }

    fn hints(&mut self, hints: &TurnHints, _: &mut Events<'_>) -> Result<(), FakeError> {
        lock(&self.script).commands.push(*hints);
        Ok(())
    }

    fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    fn close(self, out: &mut Events<'_>) {
        lock(&self.script).closes += 1;
        out.ready(false);
    }
}

/// A measured card. Its hook fills an estimated backspin into a launch
/// without spin on a card named "estimate".
#[derive(Default)]
struct FakeHooks;

impl Hooks<FakeDriver> for FakeHooks {
    type Section = FakeSection;
    const SOURCE: Option<ShotSource> = Some(ShotSource::Mevo);

    fn on_frp(&mut self, body: &mut FrpBody, ctx: &mut HookCtx<'_, FakeSection>) -> Pass {
        if let FrpBody::ShotLaunch(m) = body
            && ctx.section.name == "estimate"
            && m.launch.backspin_rpm.is_none()
        {
            m.launch.backspin_rpm = Some(1234);
        }
        Pass::Send
    }
}

// -- Helpers -------------------------------------------------------------

fn temp_state() -> (TempDir, Arc<SystemState>, GameStateWriter) {
    let dir = std::env::temp_dir().join(format!(
        "flighthook-driver-test-{}",
        crate::state::config::generate_id()
    ));
    let (state, writer) = SystemState::new(dir.join("config.toml"));
    (TempDir(dir), Arc::new(state), writer)
}

fn key(n: u32) -> ShotKey {
    ShotKey::new(format!("shot-{n}"), n)
}

fn launch() -> Launch {
    Launch {
        launch_speed: Some(Velocity::MetersPerSecond(60.0)),
        launch_elevation: Some(14.0),
        launch_azimuth: Some(-1.0),
        backspin_rpm: Some(3000),
        sidespin_rpm: Some(-300),
        ..Launch::default()
    }
}

fn flight() -> FlightResult {
    FlightResult {
        carry_distance: Some(Distance::Meters(150.0)),
        ..FlightResult::new(Basis::Tracked)
    }
}

fn frp(body: impl Into<FrpBody>) -> DeviceEvent {
    DeviceEvent::Frp(body.into())
}

fn trigger(key: ShotKey) -> FrpBody {
    ShotTrigger::new(key).into()
}

fn trigger_at(key: ShotKey, ms: u64) -> FrpBody {
    ShotTrigger::at(key, ms).into()
}

fn launch_of(key: ShotKey, launch: Launch) -> FrpBody {
    flightrelay::ShotLaunch { key, launch }.into()
}

fn flight_of(key: ShotKey, flight: FlightResult) -> FrpBody {
    flightrelay::ShotFlight { key, flight }.into()
}

fn finished(key: ShotKey) -> FrpBody {
    ShotFinished { key }.into()
}

fn frame(dir: flightrelay::RawDir, label: &str, data: &[u8]) -> DeviceEvent {
    let mut frame = RawFrame::new(dir, 0, data.to_vec());
    frame.label = Some(label.into());
    frp(FrpBody::ShotRaw(frame))
}

/// A sink with the card's hooks, which it hands to [`Sink::handle`].
struct TestSink<D: FrpDriver, H: Hooks<D>> {
    sink: Sink<D, H>,
    hooked: Hooked<D, H>,
}

impl<D: FrpDriver, H: Hooks<D>> TestSink<D, H> {
    fn handle(&mut self, event: DeviceEvent) {
        self.sink.handle(event, &mut self.hooked);
    }
}

impl<D: FrpDriver, H: Hooks<D>> std::ops::Deref for TestSink<D, H> {
    type Target = Sink<D, H>;
    fn deref(&self) -> &Sink<D, H> {
        &self.sink
    }
}

impl<D: FrpDriver, H: Hooks<D>> std::ops::DerefMut for TestSink<D, H> {
    fn deref_mut(&mut self) -> &mut Sink<D, H> {
        &mut self.sink
    }
}

/// A connected sink of card `H` named `name`, and the bus behind it.
fn sink<H: Hooks<FakeDriver, Section = FakeSection>>(
    name: &str,
) -> (
    TempDir,
    TestSink<FakeDriver, H>,
    broadcast::Receiver<BusMsg>,
    GameStateWriter,
) {
    let (dir, mut sink, rx, writer) = connecting_sink(name);
    sink.connected(&DeviceIdentity::new("FAKE-1").with_model("Eagle"));
    (dir, sink, rx, writer)
}

/// [`sink`] before its first session has connected.
fn connecting_sink<H: Hooks<FakeDriver, Section = FakeSection>>(
    name: &str,
) -> (
    TempDir,
    TestSink<FakeDriver, H>,
    broadcast::Receiver<BusMsg>,
    GameStateWriter,
) {
    let (dir, state, writer) = temp_state();
    let (tx, rx) = broadcast::channel(256);
    let sender =
        BusSender::new("fake.0".into(), tx, Arc::default()).with_frp_id(Some("fake.0".into()));
    let section = FakeSection {
        name: name.into(),
        driver: FakeConfig::default(),
    };
    let mut sink = TestSink {
        sink: Sink::new(state, sender, FakeDriver::info(&section.driver)),
        hooked: Hooked::new(section),
    };
    sink.begin_session();
    (dir, sink, rx, writer)
}

fn drain(rx: &mut broadcast::Receiver<BusMsg>) -> Vec<BusMsg> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn kind(msg: &BusMsg) -> &'static str {
    match msg {
        BusMsg::Frp { msg, .. } => match &msg.body {
            FrpBody::ShotTrigger(_) => "trigger",
            FrpBody::ShotLaunch(_) => "launch",
            FrpBody::ShotFlight(_) => "flight",
            FrpBody::ShotClub(_) => "club",
            FrpBody::ShotImpact(_) => "impact",
            FrpBody::ShotFinished(_) => "finished",
            FrpBody::BusActor(_) => "actor",
            FrpBody::BusAlert(_) => "alert",
            _ => "other",
        },
        BusMsg::Local { event, .. } => match event {
            LocalEvent::CardStatus { .. } => "status",
            LocalEvent::ShotCapture { .. } => "capture",
            _ => "other",
        },
    }
}

fn kinds(messages: &[BusMsg]) -> Vec<&'static str> {
    messages.iter().map(kind).collect()
}

/// The body of an FRP message.
fn body(msg: &BusMsg) -> &FrpBody {
    &msg.as_frp().expect("an FRP message").body
}

/// The status and telemetry of a card status.
fn card_status(msg: &BusMsg) -> Option<(ActorStatus, &HashMap<String, String>)> {
    match msg.as_local() {
        Some((_, LocalEvent::CardStatus { status, telemetry })) => Some((*status, telemetry)),
        _ => None,
    }
}

fn raw_of(messages: &[BusMsg]) -> ShotRaw {
    messages
        .iter()
        .find_map(|m| match m.as_local() {
            Some((_, LocalEvent::ShotCapture { raw, .. })) => Some((**raw).clone()),
            _ => None,
        })
        .expect("a finished shot")
}

/// What card `card` (hooks `H` on driver `D`, with `section`) puts on the
/// bus for one session of device `identity` that emits `events`, the
/// session in `mode`: through the host's own sink, with no thread, device
/// or clock. Connected and the card's `bus.actor` (with `D`'s roles and the
/// card's source) come first, then each event through the card's hooks.
pub(crate) fn card_bus<D: FrpDriver, H: Hooks<D>>(
    card: &str,
    section: H::Section,
    mode: Option<Mode>,
    identity: &DeviceIdentity,
    events: impl IntoIterator<Item = DeviceEvent>,
) -> Vec<BusMsg> {
    let (_dir, state, _writer) = temp_state();
    let (tx, mut rx) = broadcast::channel(4096);
    let sender = BusSender::new(card.into(), tx, Arc::default()).with_frp_id(Some(card.into()));
    let mut sink = Sink::<D, H>::new(state, sender, D::info(&section.driver_settings()));
    let mut hooked = Hooked::new(section);
    sink.begin_session();
    start_session(&mut sink, &mut hooked, identity);
    sink.session_mode = mode;
    for event in events {
        sink.handle(event, &mut hooked);
    }
    drain(&mut rx)
}

// -- The sink ------------------------------------------------------------

/// A session's start: Connected with the card text and the shot count,
/// then the card's `bus.actor` (with the device and its `source`)
/// before anything else.
#[test]
fn a_session_starts_with_connected_and_the_identity() {
    let (_dir, _sink, mut rx, _w) = sink::<FakeHooks>("");
    let seen = drain(&mut rx);
    assert_eq!(kinds(&seen), ["status", "actor"]);
    let (status, telemetry) = card_status(&seen[0]).expect("a card status");
    assert_eq!(status, ActorStatus::Connected);
    assert_eq!(telemetry["device_info"], "Eagle");
    assert_eq!(telemetry["shot_count"], "0");
    let FrpBody::BusActor(actor) = body(&seen[1]) else {
        unreachable!()
    };
    let device = actor.device.as_ref().expect("the device");
    assert_eq!(device.model.as_deref(), Some("Eagle"));
    assert_eq!(device.source, Some(Source::Measured));
    assert_eq!(actor.telemetry, None);
    assert!(
        seen.iter()
            .filter_map(BusMsg::as_frp)
            .all(|m| m.from == "fake.0")
    );
}

/// Every kind of driver event reaches the bus: phase as the card's
/// `phase`, the device's status in the card's `bus.actor`, alerts, the
/// shot lifecycle (trigger with its timestamp, launch, flight result,
/// club, impact, the raw frames, finished), the shot count, and nothing
/// for a log line.
#[test]
fn every_event_kind_goes_on_the_bus_with_the_device() {
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
    drain(&mut rx);
    let k = key(1);
    for event in [
        DeviceEvent::Phase("arming"),
        DeviceEvent::Log(Level::Info, "armed".into()),
        DeviceEvent::Status(DeviceStatus::new().with_telemetry("ready", "true")),
        DeviceEvent::Status(
            DeviceStatus::new()
                .with_telemetry("ready", "true")
                .with_battery(80),
        ),
        frp(flightrelay::BusAlert::new(Severity::Warn, "low battery")),
        frp(trigger_at(k.clone(), 1_700_000_000_000)),
        frp(launch_of(k.clone(), launch())),
        frp(flight_of(k.clone(), flight())),
        frp(flightrelay::ShotClub {
            key: k.clone(),
            club: ClubData {
                club_speed: Some(Velocity::MetersPerSecond(41.0)),
                ..ClubData::default()
            },
        }),
        frp(flightrelay::ShotImpact {
            key: k.clone(),
            impact: FaceImpact {
                lateral: Some(Distance::Millimeters(3.0)),
                ..FaceImpact::default()
            },
        }),
        frp(finished(k.clone())),
    ] {
        sink.handle(event);
    }
    let seen = drain(&mut rx);
    assert_eq!(
        kinds(&seen),
        [
            "status", "actor", "actor", "alert", "trigger", "launch", "flight", "club", "impact",
            "capture", "finished", "status"
        ]
    );
    let (status, telemetry) = card_status(&seen[0]).expect("a card status");
    assert_eq!(status, ActorStatus::Connected);
    assert!(telemetry["phase"] == "arming" && telemetry.len() == 1);
    let FrpBody::BusActor(actor) = body(&seen[2]) else {
        unreachable!()
    };
    let merged = actor.telemetry.as_ref().expect("telemetry");
    assert_eq!(merged["ready"], "true");
    assert_eq!(actor.device.as_ref().and_then(|d| d.battery_pct), Some(80));
    assert_eq!(
        actor.device.as_ref().and_then(|d| d.source),
        Some(Source::Measured)
    );
    assert!(matches!(body(&seen[3]), FrpBody::BusAlert(a) if a.message == "low battery"));
    assert!(matches!(
        body(&seen[4]),
        FrpBody::ShotTrigger(t) if t.timestamp == Some(1_700_000_000_000)
    ));
    let FrpBody::ShotLaunch(l) = body(&seen[5]) else {
        unreachable!()
    };
    assert_eq!(l.launch.backspin_rpm, Some(3000));
    assert!(matches!(body(&seen[6]), FrpBody::ShotFlight(f) if f.flight == self::flight()));
    assert_eq!(raw_of(&seen).transport, "fake-tcp");
    let (status, telemetry) = card_status(&seen[11]).expect("a card status");
    assert_eq!(status, ActorStatus::Connected);
    assert_eq!(telemetry["shot_count"], "1");
}

/// A hook may change a message before it goes on the bus: the
/// "estimate" card's hook fills in a launch's missing spin.
#[test]
fn a_hook_marks_an_estimated_spin() {
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("estimate");
    drain(&mut rx);
    let no_spin = Launch {
        backspin_rpm: None,
        sidespin_rpm: None,
        ..launch()
    };
    sink.handle(frp(trigger(key(1))));
    sink.handle(frp(launch_of(key(1), no_spin)));
    sink.handle(frp(finished(key(1))));
    let seen = drain(&mut rx);
    let FrpBody::ShotLaunch(l) = body(&seen[1]) else {
        panic!("{:?}", kinds(&seen))
    };
    assert_eq!(l.launch.backspin_rpm, Some(1234));
}

/// A card that keeps what it saw of each shot at its end.
#[derive(Default)]
struct ShotHooks(Vec<ShotSoFar>);

impl Hooks<FakeDriver> for ShotHooks {
    type Section = FakeSection;
    const SOURCE: Option<ShotSource> = Some(ShotSource::Mevo);

    fn before_finished(
        &mut self,
        shot: &ShotSoFar,
        _: &mut HookCtx<'_, FakeSection>,
    ) -> Vec<FrpBody> {
        self.0.push(shot.clone());
        Vec::new()
    }
}

/// The hooks see a shot's mode as the session ran in at its trigger,
/// whatever it runs in by the shot's end, with the measured launch.
#[test]
fn a_shots_mode_is_the_sessions_at_its_trigger() {
    let (_dir, mut sink, _rx, _w) = sink::<ShotHooks>("");
    sink.session_mode = Some(Mode::Putting);
    sink.handle(frp(trigger(key(1))));
    sink.session_mode = Some(Mode::Full);
    sink.handle(frp(launch_of(key(1), launch())));
    sink.handle(frp(finished(key(1))));
    let shot = &sink.hooked.hooks.0[0];
    assert_eq!(shot.mode, Some(Mode::Putting));
    assert_eq!(shot.launch, Some(launch()));
}

/// Raw frames go to the shot's capture (pre-roll included) with their
/// label as the driver gave it, bulk frames charged to the bulk budget,
/// never onto the bus; frame numbers count on per device id across
/// sessions.
#[test]
fn raw_frames_are_captured_with_their_label_and_numbered_across_sessions() {
    use flightrelay::RawDir::{Rx, Tx};
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
    drain(&mut rx);
    sink.handle(frame(Tx, "pre roll", &[0x01]));
    sink.handle(frp(trigger(key(1))));
    sink.handle(frame(Rx, "D4 DSP>APP", &[0xde, 0xad]));
    sink.handle(frame(Rx, "bulk", &[0xaa; 10]));
    sink.handle(frame(Rx, "bulk", &[0xbb; 10]));
    sink.handle(frp(finished(key(1))));
    let seen = drain(&mut rx);
    assert_eq!(
        kinds(&seen),
        ["trigger", "capture", "finished", "status"],
        "no frame on the bus"
    );
    let raw = raw_of(&seen);
    let labels: Vec<_> = raw.frames.iter().map(|f| f.label.as_deref()).collect();
    assert_eq!(labels, [Some("pre roll"), Some("D4 DSP>APP"), Some("bulk")]);
    assert_eq!(raw.dropped, 1, "over the bulk budget");
    assert_eq!(raw.frames[0].dir, RawDir::Tx);
    assert!(raw.frames[0].t_ms <= 0);
    assert_eq!(raw.frames[1].hex.as_deref(), Some("dead"));
    assert!(
        raw.frames
            .iter()
            .all(|f| f.r#type.is_none() && f.channel.is_none())
    );
    assert_eq!(sink.seqs["FAKE-1"], 4);

    sink.begin_session();
    sink.connected(&DeviceIdentity::new("FAKE-1"));
    sink.handle(frame(Rx, "again", &[0x02]));
    assert_eq!(sink.seqs["FAKE-1"], 5, "counting on in the next session");
}

/// A shot's capture carries the device's own id locally, for the
/// export scrub; no FRP message names it beyond its `device` field.
#[test]
fn the_capture_carries_the_device_id_for_the_scrub() {
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
    drain(&mut rx);
    sink.handle(frp(trigger(key(1))));
    sink.handle(frp(finished(key(1))));
    let seen = drain(&mut rx);
    let scrub = seen
        .iter()
        .find_map(|m| match m.as_local() {
            Some((_, LocalEvent::ShotCapture { scrub, .. })) => Some(scrub.clone()),
            _ => None,
        })
        .expect("a capture");
    assert_eq!(scrub, ["FAKE-1"]);
}

/// Timestamps of one millisecond map to one instant however long after
/// each other they are converted; later milliseconds map later.
#[test]
fn one_millisecond_is_one_instant() {
    let mut clock = UtcClock::default();
    let ms = flightrelay::utc_ms().expect("clock");
    let first = clock.instant_of(ms);
    for _ in 0..5 {
        std::thread::sleep(Duration::from_micros(700));
        assert_eq!(clock.instant_of(ms), first);
    }
    assert_eq!(clock.instant_of(ms + 1) - first, Duration::from_millis(1));
    assert_eq!(first - clock.instant_of(ms - 5), Duration::from_millis(5));
}

/// Raw frames the driver stamps in one millisecond keep the order it
/// handed them over, in the pre-roll and in the shot window.
#[test]
fn frames_of_one_millisecond_keep_their_order() {
    use flightrelay::RawDir::Rx;
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
    drain(&mut rx);
    let ms = flightrelay::utc_ms().expect("clock");
    let stamped = |n: u8| {
        let mut frame = RawFrame::new(Rx, 0, vec![n]);
        frame.label = Some(n.to_string());
        frame.timestamp = Some(ms);
        frp(FrpBody::ShotRaw(frame))
    };
    for n in 0..3 {
        sink.handle(stamped(n));
    }
    sink.handle(frp(trigger_at(key(1), ms)));
    // The fake's profile keeps 4 frames of pre-roll and 8 a shot.
    for n in 3..8 {
        sink.handle(stamped(n));
        std::thread::sleep(Duration::from_micros(300));
    }
    sink.handle(frp(finished(key(1))));
    let raw = raw_of(&drain(&mut rx));
    let order: Vec<u8> = raw
        .frames
        .iter()
        .map(|f| {
            f.label
                .as_deref()
                .and_then(|l| l.parse().ok())
                .expect("label")
        })
        .collect();
    assert_eq!(order, (0..8).collect::<Vec<u8>>());
}

/// Frames sent during the first connect, before the device id is known,
/// are numbered on into the session.
#[test]
fn frames_before_the_first_session_are_numbered_on() {
    use flightrelay::RawDir::{Rx, Tx};
    let (_dir, mut sink, _rx, _w) = connecting_sink::<FakeHooks>("");
    sink.handle(frame(Tx, "hello", &[0x01]));
    sink.handle(frame(Rx, "hello back", &[0x02]));
    sink.connected(&DeviceIdentity::new("FAKE-1"));
    sink.handle(frame(Rx, "status", &[0x03]));
    assert_eq!(sink.seqs.len(), 1);
    assert_eq!(sink.seqs["FAKE-1"], 3);
}

/// The status a driver sends while it connects goes out in the card's
/// `bus.actor` with the identity once the session connects (its
/// telemetry shows on the card as rows meanwhile), and the connect's
/// `phase` leaves the card then; a connect without a phase sends no
/// `phase` key.
#[test]
fn a_connects_telemetry_goes_out_with_the_identity() {
    let ready = |pct: u8| {
        DeviceEvent::Status(
            DeviceStatus::new()
                .with_telemetry("ready", "false")
                .with_battery(pct),
        )
    };
    let (_dir, mut sink, mut rx, _w) = connecting_sink::<FakeHooks>("");
    sink.handle(DeviceEvent::Phase("handshaking"));
    sink.handle(ready(50));
    sink.handle(ready(48));
    let seen = drain(&mut rx);
    assert_eq!(kinds(&seen), ["status", "status"], "no bus.actor yet");
    let (_, rows) = card_status(&seen[1]).expect("a card status");
    assert_eq!(rows, &HashMap::from([("ready".into(), "false".into())]));
    sink.connected(&DeviceIdentity::new("FAKE-1").with_model("Eagle"));
    let seen = drain(&mut rx);
    assert_eq!(kinds(&seen), ["status", "actor"]);
    let (status, telemetry) = card_status(&seen[0]).expect("a card status");
    assert_eq!(status, ActorStatus::Connected);
    assert!(telemetry["phase"].is_empty());
    let FrpBody::BusActor(actor) = body(&seen[1]) else {
        unreachable!()
    };
    assert_eq!(
        actor.device.as_ref().and_then(|d| d.model.as_deref()),
        Some("Eagle")
    );
    assert_eq!(
        actor.telemetry.as_ref().expect("telemetry")["ready"],
        "false"
    );
    sink.handle(ready(49));
    assert_eq!(kinds(&drain(&mut rx)), ["actor"], "connected: at once");

    sink.begin_session();
    sink.connected(&DeviceIdentity::new("FAKE-1"));
    let seen = drain(&mut rx);
    let (_, telemetry) = card_status(&seen[0]).expect("a card status");
    assert!(!telemetry.contains_key("phase"));
}

/// A telemetry key the driver drops during a session leaves the card:
/// the `bus.actor` lists only the keys the device has, and the key that
/// went follows as an empty row.
#[test]
fn a_telemetry_key_dropped_in_a_session_leaves_the_card() {
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
    let mut card = flighthook::ActorStatusResponse::new("Fake", ActorStatus::Starting);
    let mut rows = |seen: &[BusMsg]| {
        for m in seen {
            if let Some((status, telemetry)) = card_status(m) {
                card.apply_status(status, telemetry, 0);
            } else if let FrpBody::BusActor(actor) = body(m) {
                for (k, v) in flighthook::bus_actor_rows(actor) {
                    if v.is_empty() {
                        card.telemetry.remove(&k);
                    } else {
                        card.telemetry.insert(k, v);
                    }
                }
            }
        }
        card.telemetry.clone()
    };
    rows(&drain(&mut rx));
    sink.handle(DeviceEvent::Status(
        DeviceStatus::new()
            .with_telemetry("ready", "false")
            .with_telemetry("blockers", "shot in progress"),
    ));
    assert_eq!(rows(&drain(&mut rx))["blockers"], "shot in progress");
    sink.handle(DeviceEvent::Status(
        DeviceStatus::new().with_telemetry("ready", "true"),
    ));
    let card = rows(&drain(&mut rx));
    assert_eq!(card["ready"], "true");
    assert!(!card.contains_key("blockers"), "{card:?}");
}

/// A discarded shot's frames leave the pre-roll.
#[test]
fn a_discarded_shot_clears_the_pre_roll() {
    use flightrelay::RawDir::Rx;
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
    drain(&mut rx);
    sink.handle(frame(Rx, "misread", &[0x11]));
    sink.handle(DeviceEvent::ShotDiscarded);
    sink.handle(frp(trigger(key(1))));
    sink.handle(frp(finished(key(1))));
    let raw = raw_of(&drain(&mut rx));
    assert!(raw.frames.is_empty(), "{raw:?}");
    assert!(raw.unavailable.is_none());
}

/// What a [`log_capture`] subscriber wrote.
#[derive(Clone, Default)]
pub(crate) struct Buf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        lock(&self.0).extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    /// The log lines written so far.
    pub(crate) fn lines(&self) -> Vec<String> {
        String::from_utf8(lock(&self.0).clone())
            .expect("text")
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// A subscriber for `tracing::subscriber::with_default` that writes
/// events up to `level` as text lines into the returned buffer.
fn log_capture(level: tracing::Level) -> (Buf, impl tracing::Subscriber + Send + Sync) {
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    (buf, subscriber)
}

/// The lines of the one INFO-level log the whole test process writes
/// into, from every thread (the actor's too), installed on first use.
/// Tests that run at the same time write into it as well, so a test
/// looks only at the lines of its own actor ID. A scoped subscriber
/// would lose lines of callsites that other tests were the first to
/// hit.
pub(crate) fn process_log() -> &'static Buf {
    static LOG: LazyLock<Buf> = LazyLock::new(|| {
        let (buf, subscriber) = log_capture(tracing::Level::INFO);
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other test installs a global subscriber");
        buf
    });
    &LOG
}

/// The lines of [`process_log`] that mention `actor`, in order.
fn log_of(actor: &str) -> Vec<String> {
    let tag = format!(" {actor}: ");
    process_log()
        .lines()
        .into_iter()
        .filter(|l| l.contains(&tag))
        .collect()
}

/// Trace lines go to the `audit` target, and a raw frame's audit line
/// carries its direction, number, label and hex.
#[test]
fn trace_lines_and_frames_go_to_the_audit_log() {
    let mut beef = RawFrame::new(flightrelay::RawDir::Rx, 7, vec![0xbe, 0xef]);
    beef.label = Some("D4 DSP>APP".into());
    assert_eq!(
        audit_line("fake.0", &beef, false),
        "fake.0 rx #7 D4 DSP>APP beef"
    );
    let json = RawFrame::new(flightrelay::RawDir::Tx, 8, br#"{"a":1}"#.to_vec());
    assert_eq!(
        audit_line("fake.0", &json, true),
        r#"fake.0 tx #8  {"a":1}"#
    );

    let (buf, subscriber) = log_capture(tracing::Level::TRACE);
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
    drain(&mut rx);
    tracing::subscriber::with_default(subscriber, || {
        sink.handle(DeviceEvent::Log(Level::Trace, "rx handshake 01ab".into()));
    });
    let log = buf.lines().join("\n");
    assert!(
        log.contains("TRACE audit: fake.0 rx handshake 01ab"),
        "{log}"
    );
}

// -- The actor -----------------------------------------------------------

struct Rig {
    _dir: TempDir,
    state: Arc<SystemState>,
    writer: GameStateWriter,
    tx: broadcast::Sender<BusMsg>,
    rx: broadcast::Receiver<BusMsg>,
}

fn rig() -> Rig {
    let (dir, state, writer) = temp_state();
    let (tx, rx) = broadcast::channel(1024);
    Rig {
        _dir: dir,
        state,
        writer,
        tx,
        rx,
    }
}

impl Rig {
    /// Start `fake.0` with FRP actor id `fake.0` (it has no config
    /// section to take one from).
    fn start(&self, section: FakeSection) {
        crate::actors::start_actor_as(
            "fake.0".into(),
            Some("fake.0".into()),
            Box::new(DriverActor::<FakeDriver, FakeHooks>::new(section)),
            &self.state,
            &self.tx,
        );
    }

    /// Messages of `fake.0` until one matches `want`; blocks on the bus.
    fn until(&mut self, mut want: impl FnMut(&BusMsg) -> bool) -> Vec<BusMsg> {
        let mut seen = Vec::new();
        loop {
            match crate::actors::scripted::recv_within(&mut self.rx) {
                Some(m) if of_fake(&m) => {
                    let done = want(&m);
                    seen.push(m);
                    if done {
                        return seen;
                    }
                }
                Some(_) => {}
                None => panic!("timed out: {:?}", kinds(&seen)),
            }
        }
    }
}

/// Whether `m` is of the card `fake.0`.
fn of_fake(m: &BusMsg) -> bool {
    match m {
        BusMsg::Frp { msg, .. } => msg.from == "fake.0",
        BusMsg::Local { card, .. } => card == "fake.0",
    }
}

fn statuses(seen: &[BusMsg]) -> Vec<ActorStatus> {
    seen.iter()
        .filter_map(|m| card_status(m).map(|(s, _)| s))
        .collect()
}

fn is_status(m: &BusMsg, want: ActorStatus) -> bool {
    card_status(m).is_some_and(|(s, _)| s == want)
}

fn is_alert(m: &BusMsg) -> bool {
    matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::BusAlert(_)))
}

fn alert_texts(seen: &[BusMsg]) -> Vec<&str> {
    seen.iter()
        .filter_map(|m| match m.as_frp().map(|f| &f.body) {
            Some(FrpBody::BusAlert(a)) => Some(a.message.as_str()),
            _ => None,
        })
        .collect()
}

/// Whether `m` is the card's `bus.actor` blocked by `not_armed`.
fn is_not_armed(m: &BusMsg) -> bool {
    matches!(
        m.as_frp().map(|f| &f.body),
        Some(FrpBody::BusActor(a)) if a.blockers.iter().any(|b| b == "not_armed")
    )
}

fn section(script: u64) -> FakeSection {
    FakeSection {
        name: "Fake".into(),
        driver: FakeConfig {
            script,
            ..FakeConfig::default()
        },
    }
}

/// The card reads Starting before the first attempt (a failed attempt
/// repeats the status only to carry its error), Connected for each
/// session and Reconnecting once a session has been established, with
/// `ready=false` just before it; each failure is the driver's alert. Every
/// session is closed, the lost one too; a stop ends Disconnected.
#[test]
fn the_card_reads_starting_connected_then_reconnecting() {
    let ready = |_: &str| DeviceEvent::Status(DeviceStatus::new());
    let (id, script) = script(vec![
        Connect::Fail("no device"),
        Connect::Fail("no device yet"),
        Connect::Session(vec![Step::Emit(vec![ready("true")]), Step::Fail("lost")]),
        Connect::Fail("still gone"),
        Connect::Session(vec![]),
    ]);
    let mut rig = rig();
    rig.start(section(id));
    let mut connects = 0;
    let seen = rig.until(|m| {
        connects += usize::from(is_status(m, ActorStatus::Connected));
        connects == 2
    });
    use ActorStatus::{Connected, Disconnected, Reconnecting, Starting};
    assert_eq!(
        statuses(&seen),
        [
            Starting,
            Starting,
            Starting,
            Connected,
            Reconnecting,
            Reconnecting,
            Connected
        ]
    );
    let at = seen
        .iter()
        .position(|m| is_status(m, Reconnecting))
        .expect("reconnecting");
    assert!(is_not_armed(&seen[at - 1]), "not_armed first");
    let alerts = alert_texts(&seen);
    assert_eq!(alerts, ["no device", "no device yet", "lost", "still gone"]);
    assert_eq!(
        rig.state.stop_and_wait("fake.0"),
        crate::state::Stopped::Exited
    );
    let end = rig.until(|m| is_status(m, Disconnected));
    assert!(
        is_not_armed(&end[end.len() - 2]),
        "close says not ready: {:?}",
        kinds(&end)
    );
    assert_eq!(lock(&script).closes, 2);
}

/// A stop ends a connect that is still waiting within the driver's
/// `STOP_WAIT`.
#[test]
fn a_stop_ends_a_blocked_connect_within_the_stop_wait() {
    let (id, script) = script(vec![Connect::Block]);
    let (entered, connect) = std::sync::mpsc::channel();
    lock(&script).entered = Some(entered);
    let mut rig = rig();
    rig.start(section(id));
    connect
        .recv_timeout(crate::actors::GIVE_UP)
        .expect("the connect begins");
    assert_eq!(
        rig.state.stop_and_wait("fake.0"),
        crate::state::Stopped::Exited
    );
    rig.until(|m| is_status(m, ActorStatus::Disconnected));
}

/// The `phase` of a connect that fails leaves the card before the retry
/// delay (and before the driver's alert); the next attempt shows its
/// own again.
#[test]
fn a_failed_connects_phase_leaves_the_card() {
    let (id, _script) = script(vec![
        Connect::Phase("handshaking", Box::new(Connect::Fail("no answer"))),
        Connect::Phase("discovering", Box::new(Connect::Block)),
    ]);
    let mut rig = rig();
    rig.start(section(id));
    let seen = rig.until(|m| {
        card_status(m).is_some_and(|(_, telemetry)| {
            telemetry.get("phase").is_some_and(|p| p == "discovering")
        })
    });
    let phases: Vec<(ActorStatus, &str)> = seen
        .iter()
        .filter_map(|m| {
            card_status(m).and_then(|(status, telemetry)| {
                telemetry.get("phase").map(|p| (status, p.as_str()))
            })
        })
        .collect();
    use ActorStatus::Starting;
    assert_eq!(
        phases,
        [
            (Starting, "handshaking"),
            (Starting, ""),
            (Starting, "discovering")
        ]
    );
    let cleared = seen
        .iter()
        .position(|m| {
            card_status(m).is_some_and(|(_, t)| t.get("phase").is_some_and(String::is_empty))
        })
        .expect("cleared");
    let alert = seen.iter().position(is_alert).expect("the driver's alert");
    assert!(cleared < alert, "{:?}", kinds(&seen));
    rig.state.stop_and_wait("fake.0");
}

/// The error row of the card after each of `seen`'s statuses, as the web
/// layer and the UI keep it.
fn error_rows(seen: &[BusMsg]) -> Vec<Option<String>> {
    let mut card = flighthook::ActorStatusResponse::new("Fake", ActorStatus::Starting);
    seen.iter()
        .filter_map(|m| {
            let (status, telemetry) = card_status(m)?;
            card.apply_status(status, telemetry, 0);
            Some(card.telemetry.get("error").cloned())
        })
        .collect()
}

/// A card whose connects keep failing carries the latest connect error as
/// its `error` row: set by the first failure, kept through the retry
/// delays and the next attempts' phases, updated when the text changes
/// and removed when a session connects. The status carries it only when
/// it is new, so a late UI reads it from the actor's held state.
#[test]
fn a_card_that_cannot_connect_shows_why() {
    let (id, _script) = script(vec![
        Connect::Fail("no device"),
        Connect::Phase("probing", Box::new(Connect::Fail("no device"))),
        Connect::Phase("probing", Box::new(Connect::Fail("no device"))),
        Connect::Fail("busy"),
        Connect::Fail("busy"),
        Connect::Session(vec![]),
    ]);
    let mut rig = rig();
    rig.start(section(id));
    let seen = rig.until(|m| is_status(m, ActorStatus::Connected));
    let rows = error_rows(&seen);
    let found: Vec<Option<&str>> = rows.iter().map(Option::as_deref).collect();
    // The first status is the plain Starting; from the first failure on
    // the row is there, whatever else the statuses carry (a probing
    // phase included), until the Connected status removes it.
    assert_eq!(found.first(), Some(&None), "{found:?}");
    assert_eq!(found.last(), Some(&None), "{found:?}");
    assert!(
        found[1..found.len() - 1].iter().all(Option::is_some),
        "{found:?}"
    );
    let mut texts: Vec<&str> = found.iter().flatten().copied().collect();
    texts.dedup();
    assert_eq!(texts, ["no device", "busy"], "{found:?}");
    // Sent as a value only when new: one status for each text, and the
    // empty value that removes it.
    let sent: Vec<&str> = seen
        .iter()
        .filter_map(|m| card_status(m).and_then(|(_, t)| t.get("error").map(String::as_str)))
        .collect();
    assert!(
        sent.contains(&"busy") && sent.last() == Some(&""),
        "{sent:?}"
    );
    rig.state.stop_and_wait("fake.0");
}

/// A stop in the middle of an outage leaves the card Disconnected without
/// the connect error.
#[test]
fn a_stop_ends_the_connect_error() {
    let (id, script) = script(vec![Connect::Fail("no device")]);
    lock(&script).retry = Some(Duration::from_secs(30));
    let mut rig = rig();
    rig.start(section(id));
    let mut seen = rig.until(is_alert);
    assert_eq!(
        rig.state.stop_and_wait("fake.0"),
        crate::state::Stopped::Exited
    );
    seen.extend(rig.until(|m| is_status(m, ActorStatus::Disconnected)));
    let rows = error_rows(&seen);
    assert!(
        rows.contains(&Some("no device".to_owned())),
        "set during the outage: {rows:?}"
    );
    assert_eq!(rows.last(), Some(&None), "{rows:?}");
}

/// The log says why a card cannot connect once when an outage begins and
/// again when the reason changes, not on every retry, and once when it
/// connects again.
#[test]
fn a_failed_connect_is_logged_once_per_outage_and_per_new_reason() {
    let (id, _script) = script(vec![
        Connect::Fail("no device"),
        Connect::Fail("no device"),
        Connect::Fail("no device"),
        Connect::Fail("busy"),
        Connect::Fail("busy"),
        Connect::Session(vec![Step::Fail("lost")]),
        Connect::Fail("no device"),
        Connect::Session(vec![]),
    ]);
    process_log();
    let rig = rig();
    let actor = DriverActor::<FakeDriver, FakeHooks>::new(section(id));
    let sender = BusSender::new("logged.0".into(), rig.tx.clone(), Arc::default())
        .with_frp_id(Some("logged.0".into()));
    let mut rx = rig.tx.subscribe();
    let thread = actor.start(Arc::clone(&rig.state), sender.clone(), sender.subscribe());
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut connects = 0;
    while connects < 2 {
        assert!(Instant::now() < deadline, "timed out");
        match rx.try_recv() {
            Ok(m)
                if m.as_local().is_some_and(|(card, _)| card == "logged.0")
                    && is_status(&m, ActorStatus::Connected) =>
            {
                connects += 1;
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    actor.stop();
    thread.join().expect("the thread ends");
    let lines = log_of("logged.0");
    let cannot: Vec<&str> = lines
        .iter()
        .filter(|l| l.contains("WARN") && l.contains("cannot connect: "))
        .map(|l| l.rsplit("cannot connect: ").next().unwrap_or(""))
        .collect();
    assert_eq!(cannot, ["no device", "busy", "no device"], "{lines:?}");
    let again: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("after failed connects"))
        .collect();
    assert_eq!(again.len(), 2, "{lines:?}");
    assert!(again.iter().all(|l| l.contains("INFO")), "{again:?}");
}

/// A driver's alert goes to the log too: a warning at warn, an error or a
/// critical one at error; the same alert again is logged once (it is on
/// the bus every time).
#[test]
fn a_drivers_alerts_are_logged() {
    process_log();
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
    sink.actor = "alerting.0".into();
    drain(&mut rx);
    for (severity, message) in [
        (Severity::Warn, "careful"),
        (Severity::Warn, "careful"),
        (Severity::Error, "broken"),
        (Severity::Critical, "gone"),
    ] {
        sink.handle(frp(flightrelay::BusAlert::new(severity, message)));
    }
    let lines = log_of("alerting.0");
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].contains("WARN") && lines[0].ends_with("alerting.0: careful"));
    assert!(lines[1].contains("ERROR") && lines[1].ends_with("alerting.0: broken"));
    assert!(lines[2].contains("ERROR") && lines[2].ends_with("alerting.0: gone"));
    assert_eq!(
        kinds(&drain(&mut rx)),
        ["alert"; 4],
        "every alert is on the bus"
    );
}

/// A stop that ends a connect leaves the card Disconnected without the
/// connect's `phase`.
#[test]
fn a_stop_during_a_connect_leaves_no_phase() {
    let (id, _script) = script(vec![Connect::Phase(
        "handshaking",
        Box::new(Connect::Block),
    )]);
    let mut rig = rig();
    rig.start(section(id));
    let seen = rig.until(|m| card_status(m).is_some_and(|(_, t)| t.contains_key("phase")));
    assert_eq!(
        rig.state.stop_and_wait("fake.0"),
        crate::state::Stopped::Exited
    );
    let end = rig.until(|m| is_status(m, ActorStatus::Disconnected));
    // The card's view of every status: no phase once Disconnected.
    let mut card = flighthook::ActorStatusResponse::new("Fake", ActorStatus::Starting);
    for m in seen.iter().chain(&end) {
        if let Some((status, telemetry)) = card_status(m) {
            card.apply_status(status, telemetry, 0);
        }
    }
    assert_eq!(card.status, ActorStatus::Disconnected);
    assert!(
        !card.telemetry.contains_key("phase"),
        "{:?}",
        card.telemetry
    );
}

/// Another actor's `bus.sync` gets the card's current `bus.actor` again,
/// so a card added later learns the device's state at once.
#[test]
fn a_bus_sync_republishes_the_cards_actor() {
    let (id, _script) = script(vec![Connect::Session(vec![Step::Emit(vec![
        DeviceEvent::Status(DeviceStatus::new()),
    ])])]);
    let mut rig = rig();
    rig.start(section(id));
    let clear = |m: &BusMsg| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::BusActor(a)) if a.is_clear());
    rig.until(clear);
    let _ = rig.tx.send(BusMsg::frp(flightrelay::FrpMessage::new(
        "late-card",
        flightrelay::BusSync::new(),
    )));
    let again = rig.until(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::BusActor(_))));
    assert!(clear(&again[again.len() - 1]), "{:?}", kinds(&again));
    assert_eq!(
        rig.state.stop_and_wait("fake.0"),
        crate::state::Stopped::Exited
    );
}

/// Settings the driver refuses keep the card from starting and show on
/// it; the actor still stops at once.
#[test]
fn refused_settings_show_on_the_card() {
    let mut rig = rig();
    let mut bad = section(0);
    bad.driver.bad = true;
    rig.start(bad);
    let seen = rig.until(|m| is_status(m, ActorStatus::Disconnected));
    // The card's `bus.actor` goes out as the actor starts.
    assert_eq!(kinds(&seen), ["actor", "status"], "no Starting, no connect");
    assert!(card_status(&seen[1]).is_some_and(|(_, t)| t["error"] == "bad: must be false"));
    assert_eq!(
        rig.state.stop_and_wait("fake.0"),
        crate::state::Stopped::Exited
    );
}

/// A driver that panics ends the card Disconnected with the error and
/// `ready=false`, never stuck Connected; the thread has ended.
#[test]
fn a_driver_panic_ends_disconnected_with_the_error() {
    let ready = DeviceEvent::Status(DeviceStatus::new());
    let (id, _script) = script(vec![Connect::Session(vec![
        Step::Emit(vec![ready]),
        Step::Panic,
    ])]);
    let mut rig = rig();
    rig.start(section(id));
    let seen = rig.until(|m| is_status(m, ActorStatus::Disconnected));
    use ActorStatus::{Connected, Disconnected, Starting};
    assert_eq!(statuses(&seen), [Starting, Connected, Disconnected]);
    let last = seen.len() - 1;
    assert!(card_status(&seen[last]).is_some_and(|(_, t)| t["error"] == DRIVER_FAILED));
    assert!(is_not_armed(&seen[last - 1]), "{:?}", kinds(&seen));
    assert_eq!(
        rig.state.stop_and_wait("fake.0"),
        crate::state::Stopped::Exited
    );
}

/// A live change made during a retry delay reaches the driver (and the
/// hooks' section) before the next connect, not only in a session.
#[test]
fn live_config_is_applied_during_a_retry_delay() {
    let (id, script) = script(vec![Connect::Fail("no device"), Connect::Session(vec![])]);
    lock(&script).retry = Some(Duration::from_secs(3));
    let mut rig = rig();
    let start = section(id);
    let actor = DriverActor::<FakeDriver, FakeHooks>::new(start.clone());
    let sender = BusSender::new("fake.0".into(), rig.tx.clone(), Arc::default())
        .with_frp_id(Some("fake.0".into()));
    let thread = actor.start(Arc::clone(&rig.state), sender.clone(), sender.subscribe());
    rig.until(is_alert);
    let mut live = start.clone();
    live.driver.live = 7;
    assert!(matches!(
        actor.apply_section(Some(live)),
        ReconfigureOutcome::Applied
    ));
    let deadline = Instant::now() + Duration::from_secs(2);
    while lock(&script).applied.is_empty() {
        assert!(Instant::now() < deadline, "not applied during the delay");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(lock(&script).applied, [7]);
    assert_eq!(lock(&script).latched.len(), 1, "before the next connect");
    actor.stop();
    thread.join().expect("the thread ends");
}

/// Host fields and live driver fields are applied without a restart, the
/// live ones handed to the running driver; any other change, a refused
/// config or a removed section restarts the card.
#[test]
fn live_config_is_applied_and_other_changes_restart() {
    let (id, script) = script(vec![Connect::Session(vec![])]);
    let rig = rig();
    let start = section(id);
    let actor = DriverActor::<FakeDriver, FakeHooks>::new(start.clone());
    let sender = BusSender::new("fake.0".into(), rig.tx.clone(), Arc::default())
        .with_frp_id(Some("fake.0".into()));
    let thread = actor.start(Arc::clone(&rig.state), sender.clone(), sender.subscribe());

    let renamed = FakeSection {
        name: "Renamed".into(),
        ..start.clone()
    };
    assert!(matches!(
        actor.apply_section(Some(renamed.clone())),
        ReconfigureOutcome::Applied
    ));
    let mut live = renamed.clone();
    live.driver.live = 5;
    assert!(matches!(
        actor.apply_section(Some(live.clone())),
        ReconfigureOutcome::Applied
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    while lock(&script).applied.is_empty() {
        assert!(Instant::now() < deadline, "not applied");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(lock(&script).applied, [5], "the driver part only once");

    let mut restart = live.clone();
    restart.driver.restart = 1;
    assert!(matches!(
        actor.apply_section(Some(restart)),
        ReconfigureOutcome::RestartRequired
    ));
    let mut refused = live.clone();
    refused.driver.bad = true;
    assert!(matches!(
        actor.apply_section(Some(refused)),
        ReconfigureOutcome::RestartRequired
    ));
    assert!(matches!(
        actor.apply_section(None),
        ReconfigureOutcome::RestartRequired
    ));
    assert!(
        matches!(actor.apply_section(Some(live)), ReconfigureOutcome::Applied),
        "unchanged"
    );

    actor.stop();
    thread.join().expect("the thread ends");
}

/// The connect's turn hints start from the session's turn and follow
/// flighthook's `golfer.turn` on the bus; the session gets the new
/// hints.
#[test]
fn commands_reach_the_latched_command_and_the_session() {
    let (id, script) = script(vec![Connect::Session(vec![])]);
    let mut rig = rig();
    let _ = rig.writer.apply(&flightrelay::GolferSelect {
        mode: Some(Mode::Putting),
        ..flightrelay::GolferSelect::new("r")
    });
    rig.start(section(id));
    rig.until(|m| is_status(m, ActorStatus::Connected));
    assert_eq!(lock(&script).latched[0].mode, Some(Mode::Putting));
    rig.tx
        .send(BusMsg::frp(flightrelay::FrpMessage::new(
            rig.state.flighthook_id(),
            flightrelay::GolferTurn::new(
                None,
                Some(flightrelay::Club::SandWedge),
                flightrelay::Handedness::Left,
                Mode::Chipping,
            ),
        )))
        .expect("sent");
    let deadline = Instant::now() + Duration::from_secs(5);
    while lock(&script).commands.is_empty() {
        assert!(Instant::now() < deadline, "no hints");
        std::thread::sleep(Duration::from_millis(5));
    }
    let hints = lock(&script).commands[0];
    assert_eq!(hints.mode, Some(Mode::Chipping));
    assert_eq!(hints.handedness, Some(flightrelay::Handedness::Left));
    assert_eq!(hints.club, Some(flightrelay::Club::SandWedge));
    rig.state.stop_and_wait("fake.0");
}

// -- Software drivers ----------------------------------------------------

use crate::actors::scripted::{
    SOFTWARE, Script as SoftScript, Seen, Software, SoftwareConfig, SoftwareSession,
};

fn game_round() -> flightrelay::GameRound {
    flightrelay::GameRound {
        id: "round-1".into(),
        status: flightrelay::RoundStatus::Playing,
        end_reason: None,
        course: None,
        scoring: None,
        players: Vec::new(),
        up: None,
        hole: None,
    }
}

fn game_aim() -> flightrelay::FrpMessage {
    flightrelay::FrpMessage::new(
        "sim",
        flightrelay::GameAim {
            r#ref: "a".into(),
            action: flightrelay::AimAction::Reset,
            degrees: None,
            target: None,
        },
    )
}

/// A scripted software driver's config: its key and its roles.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct SoftConfig {
    key: String,
    /// `"game"` (with the `soft` namespace), `"launch_monitor"`, or no
    /// role (an output).
    role: String,
}

impl DriverConfig for SoftConfig {}

impl SoftwareConfig for SoftConfig {
    fn info(&self) -> DriverInfo {
        let info = DriverInfo::new("soft", "Soft", Source::Virtual);
        match self.role.as_str() {
            "game" => info.with_roles(&[roles::GAME]).with_namespaces(&["soft"]),
            "launch_monitor" => info,
            _ => info.with_roles(&[]),
        }
    }

    fn key(&self) -> String {
        self.key.clone()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct SoftSection {
    #[serde(default)]
    name: String,
    #[serde(flatten)]
    driver: SoftConfig,
}

impl DeviceSection for SoftSection {
    type Driver = SoftConfig;
    const KIND: &'static str = "soft";

    fn driver(&self) -> &SoftConfig {
        &self.driver
    }

    fn sections(_: &flighthook::FlighthookConfig) -> &HashMap<String, Self> {
        static NONE: LazyLock<HashMap<String, SoftSection>> = LazyLock::new(HashMap::new);
        &NONE
    }
}

type Soft = Software<SoftConfig>;

/// A software card that is no shot source. It hands its session every
/// FRP message of another actor, starts each session with the current
/// turn, asks the session for `aim` on a `game.aim`, and drops shot #13.
#[derive(Default)]
struct SoftHooks;

impl Hooks<Soft> for SoftHooks {
    type Section = SoftSection;
    const SOURCE: Option<ShotSource> = None;

    fn on_frp(&mut self, body: &mut FrpBody, _: &mut HookCtx<'_, SoftSection>) -> Pass {
        match body {
            FrpBody::ShotTrigger(t) if t.key.shot_number == 13 => Pass::Drop,
            _ => Pass::Send,
        }
    }

    fn action(msg: &BusMsg, _: &str) -> Option<String> {
        matches!(msg.as_frp().map(|m| &m.body), Some(FrpBody::GameAim(_))).then(|| "aim".into())
    }

    fn route(&mut self, msg: &BusMsg, ctx: &mut HookCtx<'_, SoftSection>) {
        if let Some(m) = msg.as_frp()
            && ctx.sender.frp_id() != Some(m.from.as_str())
        {
            ctx.to_session.push(m.clone());
        }
    }

    fn session_started(&mut self, ctx: &mut HookCtx<'_, SoftSection>) {
        ctx.to_session.push(flightrelay::FrpMessage::new(
            ctx.state.flighthook_id(),
            ctx.state.session.turn(),
        ));
    }
}

/// The same card as a virtual shot source.
#[derive(Default)]
struct SoftSourceHooks;

impl Hooks<Soft> for SoftSourceHooks {
    type Section = SoftSection;
    const SOURCE: Option<ShotSource> = Some(ShotSource::MockMonitor);
}

/// A software card `H` with `role`, its sink not connected yet, and the
/// bus behind it.
fn soft_sink<H: Hooks<Soft, Section = SoftSection>>(
    role: &str,
) -> (TempDir, TestSink<Soft, H>, broadcast::Receiver<BusMsg>) {
    let (dir, state, _writer) = temp_state();
    let (tx, rx) = broadcast::channel(256);
    let sender =
        BusSender::new("soft.0".into(), tx, Arc::default()).with_frp_id(Some("soft.0".into()));
    let section = SoftSection {
        name: "Soft".into(),
        driver: SoftConfig {
            key: "soft".into(),
            role: role.into(),
        },
    };
    let sink = TestSink {
        sink: Sink::new(state, sender, Soft::info(&section.driver)),
        hooked: Hooked::new(section),
    };
    (dir, sink, rx)
}

/// The kinds a driver's messages may have, by its roles and namespaces:
/// any driver sends alerts, selects and replies; a game, or a driver
/// that issues golfer ids, `game.*` and `golfer.profile` with their
/// removals; a launch monitor `shot.*`. The host's own kinds never
/// pass.
#[test]
fn a_drivers_roles_decide_the_kinds_it_sends() {
    use flightrelay::{GolferId, GolferProfile, GolferSelect, GolferTurn, PracticeBlock};
    let output = DriverInfo::new("o", "O", Source::Virtual).with_roles(&[]);
    let game = DriverInfo::new("g", "G", Source::Virtual).with_roles(&[roles::GAME]);
    let issuer = DriverInfo::new("i", "I", Source::Virtual)
        .with_roles(&[])
        .with_namespaces(&["soft"]);
    let monitor = DriverInfo::new("m", "M", Source::Measured);
    let bodies: Vec<(FrpBody, [bool; 4])> = vec![
        (
            flightrelay::BusAlert::new(Severity::Warn, "w").into(),
            [true; 4],
        ),
        (GolferSelect::new("r").into(), [true; 4]),
        (
            flightrelay::BusReply::ok("fake.0", "golfer.select", "r").into(),
            [true; 4],
        ),
        (game_round().into(), [false, true, true, false]),
        (
            Removal::of(StateKind::GameRound).into(),
            [false, true, true, false],
        ),
        (
            GolferProfile::new(GolferId::new("soft", "1").expect("an id"), "Ann").into(),
            [false, true, true, false],
        ),
        (trigger(key(1)), [false, false, false, true]),
        (finished(key(1)), [false, false, false, true]),
        (BusActor::new("x", 2.0).into(), [false; 4]),
        (Removal::of(StateKind::BusActor).into(), [false; 4]),
        (
            GolferTurn::new(None, None, flightrelay::Handedness::Right, Mode::Full).into(),
            [false; 4],
        ),
        (
            PracticeBlock {
                id: "b".into(),
                golfer: None,
                club: None,
                number: 1,
                session: 1,
                started_at: 0,
            }
            .into(),
            [false; 4],
        ),
        (flightrelay::BusSync::new().into(), [false; 4]),
    ];
    for (body, want) in bodies {
        let got = [&output, &game, &issuer, &monitor].map(|info| role_sends(info, &body));
        assert_eq!(got, want, "{}", body.kind());
    }
}

/// Through the sink: a game card's `game.round` and `golfer.select` go
/// on the bus under its id; its `shot.*` and `bus.actor` do not.
#[test]
fn a_game_cards_messages_go_out_by_its_roles() {
    let (_dir, mut sink, mut rx) = soft_sink::<SoftHooks>("game");
    sink.connected(&DeviceIdentity::new("soft"));
    drain(&mut rx);
    for body in [
        game_round().into(),
        flightrelay::GolferSelect::new("r").into(),
        trigger(key(1)),
        BusActor::new("x", 2.0).into(),
    ] {
        sink.handle(DeviceEvent::Frp(body));
    }
    let sent: Vec<_> = drain(&mut rx)
        .iter()
        .filter_map(|m| {
            m.as_frp()
                .map(|f| (f.from.clone(), f.body.kind().to_owned()))
        })
        .collect();
    assert_eq!(
        sent,
        [
            ("soft.0".to_owned(), "game.round".to_owned()),
            ("soft.0".to_owned(), "golfer.select".to_owned()),
        ]
    );
}

/// A hook's `Pass::Drop` on a trigger drops the whole shot (until its
/// `shot.finished`, or the shot timeout); other shots go on.
#[test]
fn a_hook_drops_a_whole_shot_at_its_trigger() {
    let (_dir, mut sink, mut rx) = soft_sink::<SoftHooks>("launch_monitor");
    sink.connected(&DeviceIdentity::new("soft"));
    drain(&mut rx);
    for body in [
        trigger(key(13)),
        launch_of(key(13), launch()),
        finished(key(13)),
        trigger(key(1)),
        trigger(key(13)),
    ] {
        sink.handle(DeviceEvent::Frp(body));
    }
    assert_eq!(kinds(&drain(&mut rx)), ["trigger"]);
    assert_eq!(sink.dropped.len(), 1, "a dropped shot that never finishes");
    let old = Instant::now()
        .checked_sub(flighthook::SHOT_TIMEOUT + Duration::from_secs(1))
        .expect("an old instant");
    *sink.dropped.get_mut("shot-13").expect("dropped") = old;
    sink.prune_shots();
    assert!(sink.dropped.is_empty(), "forgotten after the shot timeout");
}

/// A session that ends with a shot open finishes it at once, as a
/// finished shot goes out: its raw capture, `shot.finished`, the shot
/// count; then the card is `not_armed` and Reconnecting.
#[test]
fn a_lost_sessions_open_shot_is_finished() {
    let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
    for body in [trigger(key(7)), launch_of(key(7), launch())] {
        sink.handle(frp(body));
    }
    drain(&mut rx);
    let TestSink { sink, hooked } = &mut sink;
    sink.session_lost(hooked);
    let seen = drain(&mut rx);
    assert_eq!(
        kinds(&seen),
        ["capture", "finished", "status", "actor", "status"]
    );
    let FrpBody::ShotFinished(finished) = body(&seen[1]) else {
        unreachable!()
    };
    assert_eq!(finished.key, key(7));
    assert_eq!(card_status(&seen[2]).expect("count").1["shot_count"], "1");
    assert!(is_not_armed(&seen[3]));
    assert!(is_status(&seen[4], ActorStatus::Reconnecting));
}

/// A card's `bus.actor` has its driver's roles and namespaces, the
/// `device.source` only for a shot source, and `not_armed` (no session)
/// only for a launch monitor.
#[test]
fn a_cards_actor_comes_from_its_drivers_roles_and_namespaces() {
    let (_d1, game, _r1) = soft_sink::<SoftHooks>("game");
    let actor = game.bus_actor();
    assert_eq!(actor.roles, [roles::GAME]);
    assert_eq!(actor.namespaces, ["soft"]);
    assert_eq!(actor.device.and_then(|d| d.source), None);
    assert!(actor.blockers.is_empty(), "{:?}", actor.blockers);

    let (_d2, output, _r2) = soft_sink::<SoftHooks>("");
    let actor = output.bus_actor();
    assert!(actor.roles.is_empty() && actor.namespaces.is_empty());

    let (_d3, monitor, _r3) = soft_sink::<SoftSourceHooks>("launch_monitor");
    let actor = monitor.bus_actor();
    assert_eq!(actor.roles, [roles::LAUNCH_MONITOR]);
    assert!(actor.namespaces.is_empty());
    assert_eq!(actor.device.and_then(|d| d.source), Some(Source::Virtual));
    assert_eq!(actor.blockers, [flightrelay::blockers::NOT_ARMED]);
}

/// A connected session sees what `session_started` pushed first (what
/// the hooks pushed before the connect is dropped), then each pass hands
/// it what the hooks routed off the bus, in order, then the new turn
/// hints and the bus's actions, then one step at the given `now`.
#[test]
fn a_session_sees_its_start_then_the_routed_bus_then_hints_actions_and_the_step() {
    let (_dir, sink, _rx) = soft_sink::<SoftHooks>("game");
    let TestSink { mut sink, hooked } = sink;
    let hooked = RefCell::new(hooked);
    let state = Arc::clone(&sink.state);
    let mut host = Host::<Soft, SoftHooks>::new(
        sink.sender.subscribe(),
        Stop::new(),
        "soft.0".into(),
        Arc::clone(&state),
        Synced::new(&sink),
    );
    let script = SoftScript::default();
    let seen = Arc::clone(&script.seen);
    let mut session = SoftwareSession::<SoftConfig>::new("soft", script);

    let early = flightrelay::FrpMessage::new("early", flightrelay::BusSync::new());
    hooked.borrow_mut().to_session.push(early);
    start_session(&mut sink, &mut hooked.borrow_mut(), session.identity());

    let turn = flightrelay::FrpMessage::new(
        state.flighthook_id(),
        flightrelay::GolferTurn::new(
            None,
            Some(flightrelay::Club::SandWedge),
            flightrelay::Handedness::Left,
            Mode::Chipping,
        ),
    );
    let aim = game_aim();
    sink.sender.send(BusMsg::frp(turn.clone()));
    sink.sender.send(BusMsg::frp(aim.clone()));
    host.drain(&mut hooked.borrow_mut());
    let now = Instant::now();
    let mut given = TurnHints::default();
    let flags = HostFlags::new();
    let activity = step_session(
        &mut session,
        &mut host,
        &mut sink,
        &hooked,
        &mut given,
        &flags,
        now,
    )
    .expect("a pass");
    assert_eq!(activity, Activity::Idle);
    let started = flightrelay::FrpMessage::new(state.flighthook_id(), state.session.turn());
    assert_eq!(
        *seen.lock(),
        [
            Seen::Bus(Box::new(started)),
            Seen::Bus(Box::new(turn)),
            Seen::Bus(Box::new(aim)),
            Seen::Hints(given),
            Seen::Action("aim".into()),
            Seen::Step(now),
        ]
    );
    assert_eq!(given.mode, Some(Mode::Chipping));
    assert!(hooked.borrow().to_session.is_empty(), "handed once");
}

/// A software card runs on the host: its session gets the hooks'
/// start and the routed bus, its game messages go on the bus, its shot
/// messages (not a launch monitor) do not, and its `bus.actor` has its
/// roles and no shot source.
#[test]
fn a_software_card_runs_on_the_host() {
    let mut steps = VecDeque::new();
    steps.push_back(vec![frp(game_round()), frp(trigger(key(1)))]);
    let script = SoftScript {
        steps,
        ..SoftScript::default()
    };
    let seen = Arc::clone(&script.seen);
    SOFTWARE.put("soft-smoke".into(), script);
    let mut rig = rig();
    crate::actors::start_actor_as(
        "fake.0".into(),
        Some("fake.0".into()),
        Box::new(DriverActor::<Soft, SoftHooks>::new(SoftSection {
            name: "Soft".into(),
            driver: SoftConfig {
                key: "soft-smoke".into(),
                role: "game".into(),
            },
        })),
        &rig.state,
        &rig.tx,
    );
    let seen_on_bus =
        rig.until(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::GameRound(_))));
    let actor = seen_on_bus
        .iter()
        .rev()
        .find_map(|m| match m.as_frp().map(|f| &f.body) {
            Some(FrpBody::BusActor(a)) => Some(a.clone()),
            _ => None,
        })
        .expect("the card's bus.actor");
    assert_eq!(actor.roles, [roles::GAME]);
    assert_eq!(actor.device.and_then(|d| d.source), None);
    let aim = game_aim();
    rig.tx.send(BusMsg::frp(aim.clone())).expect("sent");
    let handed = seen.wait_until(|seen| seen.contains(&Seen::Action("aim".into())));
    let buses: Vec<_> = handed
        .iter()
        .filter_map(|s| match s {
            Seen::Bus(m) => Some((**m).clone()),
            _ => None,
        })
        .collect();
    assert!(
        matches!(buses[0].body, FrpBody::GolferTurn(_)),
        "the turn first: {buses:?}"
    );
    assert!(buses.contains(&aim), "{buses:?}");
    assert_eq!(
        rig.state.stop_and_wait("fake.0"),
        crate::state::Stopped::Exited
    );
    let rest = drain(&mut rig.rx);
    assert!(
        !rest
            .iter()
            .chain(&seen_on_bus)
            .filter(|m| of_fake(m))
            .any(|m| kind(m) == "trigger"),
        "no shot from a game"
    );
}

// -- Every hooked driver -------------------------------------------------

fn provenance_of(source: Source) -> Option<Source> {
    match source {
        Source::Measured => Some(Source::Measured),
        Source::Virtual => Some(Source::Virtual),
        _ => None,
    }
}

/// A card's shot source has the class its driver declares, so a driver
/// that changes its source fails here instead of being classed
/// differently.
fn source_matches<D: FrpDriver, H: Hooks<D>>() {
    assert_eq!(
        H::SOURCE.map(ShotSource::provenance),
        provenance_of(D::info(&D::Config::default()).source),
        "{:?}",
        H::SOURCE
    );
}

#[test]
fn every_cards_source_is_its_drivers() {
    source_matches::<flightrelay::mock::MockDriver, crate::actors::mock::MockHooks>();
    source_matches::<ironsight::driver::Mevo, crate::actors::mevo::MevoHooks>();
    source_matches::<tenover::driver::R10, crate::actors::r10::R10Hooks>();
    source_matches::<allsquare::driver::Omni, crate::actors::square::SquareHooks>();
    source_matches::<threeputt::driver::ExPutt, crate::actors::exputt::ExPuttHooks>();
    source_matches::<thumbswing::driver::Thumbswing, crate::actors::trackball::TrackballHooks>();
    source_matches::<FakeDriver, FakeHooks>();
}

/// The field names serde reads for struct `T`.
fn struct_fields<'de, T: Deserialize<'de>>() -> &'static [&'static str] {
    struct Grab(Option<&'static [&'static str]>);
    impl<'de> serde::Deserializer<'de> for &mut Grab {
        type Error = serde::de::value::Error;
        fn deserialize_any<V: serde::de::Visitor<'de>>(
            self,
            _: V,
        ) -> Result<V::Value, Self::Error> {
            Err(serde::de::Error::custom("not a struct"))
        }
        fn deserialize_struct<V: serde::de::Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Self::Error> {
            self.0 = Some(fields);
            Err(serde::de::Error::custom("fields taken"))
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple
            tuple_struct map enum identifier ignored_any
        }
    }
    let mut grab = Grab(None);
    let _ = T::deserialize(&mut grab);
    grab.0.unwrap_or(&[])
}

/// Every key a driver config `C` reads or writes.
fn driver_keys<C: DriverConfig>() -> BTreeSet<String> {
    let mut keys: BTreeSet<String> = struct_fields::<C>()
        .iter()
        .map(|k| (*k).to_owned())
        .collect();
    keys.extend(C::example().to_map().into_iter().map(|(k, _)| k));
    keys
}

/// `populated` (every host field set, the driver part at its defaults)
/// writes only host keys, none of them a driver key, and a section with
/// every driver field set reads back as written.
fn keys_apart<S: DeviceSection>(populated: &S, full: &S) {
    assert!(
        !struct_fields::<S::Driver>().is_empty(),
        "{}: the driver config reads as a struct",
        S::KIND
    );
    let driver = driver_keys::<S::Driver>();
    let serde_json::Value::Object(host) = serde_json::to_value(populated).expect("json") else {
        panic!("a section is a table");
    };
    for key in host.keys() {
        assert!(
            !driver.contains(key),
            "{}: host key {key} is a driver key",
            S::KIND
        );
    }
    let json = serde_json::to_string(full).expect("json");
    assert_eq!(
        &serde_json::from_str::<S>(&json).expect("reads"),
        full,
        "{json}"
    );
    let toml = toml::to_string(full).expect("toml");
    assert_eq!(&toml::from_str::<S>(&toml).expect("reads"), full, "{toml}");
}

#[test]
fn host_keys_never_collide_with_driver_keys() {
    let fake = FakeSection {
        name: "Fake".into(),
        driver: FakeConfig::default(),
    };
    let full = FakeSection {
        driver: FakeConfig {
            script: 1,
            live: 2,
            restart: 3,
            bad: true,
        },
        ..fake.clone()
    };
    keys_apart(&fake, &full);
    assert_eq!(
        driver_keys::<FakeConfig>(),
        BTreeSet::from(["bad", "live", "restart", "script"].map(String::from))
    );

    let mock = flighthook::MockMonitorSection {
        name: "Mock".into(),
        actor_id: Some("lm-1".into()),
        enabled: false,
        impact_lead_s: Some(1.5),
        driver: flighthook::MockConfig::default(),
    };
    let full = flighthook::MockMonitorSection {
        driver: {
            let mut config = flighthook::MockConfig::default();
            config.shot_interval_s = 5.0;
            config.arm_delay_s = 3.0;
            config.ball_ready_delay_s = 2.0;
            config.post_shot_delay_s = 4.0;
            config.serial = Some("7".into());
            config
        },
        ..mock.clone()
    };
    keys_apart(&mock, &full);
    assert_eq!(
        driver_keys::<flighthook::MockConfig>(),
        BTreeSet::from(
            [
                "arm_delay_s",
                "ball_ready_delay_s",
                "post_shot_delay_s",
                "serial",
                "shot_interval_s"
            ]
            .map(String::from)
        )
    );

    let mevo = flighthook::MevoSection {
        name: "Mevo".into(),
        actor_id: Some("lm-1".into()),
        enabled: false,
        launch_only_spin: 50.0,
        impact_lead_s: Some(0.5),
        driver: flighthook::MevoConfig::default(),
    };
    let full = flighthook::MevoSection {
        driver: {
            let mut config = flighthook::MevoConfig::example();
            config.address = Some("10.0.0.5:5100".into());
            config.spin_source = flighthook::SpinSource::Auto;
            config
        },
        ..mevo.clone()
    };
    keys_apart(&mevo, &full);
    assert_eq!(
        driver_keys::<flighthook::MevoConfig>(),
        BTreeSet::from(
            [
                "address",
                "ball_type",
                "camera_mode",
                "environment",
                "range",
                "spin_source",
                "surface_height",
                "tee_height",
                "track_pct",
                "use_estimated",
            ]
            .map(String::from)
        )
    );

    let r10 = flighthook::R10Section {
        name: "R10".into(),
        actor_id: Some("lm-1".into()),
        enabled: false,
        impact_lead_s: Some(2.5),
        driver: flighthook::R10Config::default(),
    };
    let full = flighthook::R10Section {
        driver: {
            let mut config = flighthook::R10Config::default();
            config.range = Some(flighthook::Distance::Feet(7.0));
            config.aim_offset_deg = Some(1.5);
            config
        },
        ..r10.clone()
    };
    keys_apart(&r10, &full);
    assert_eq!(
        driver_keys::<flighthook::R10Config>(),
        BTreeSet::from(["aim_offset_deg".to_owned(), "range".to_owned()])
    );

    let square = flighthook::SquareSection {
        name: "Omni".into(),
        actor_id: Some("lm-1".into()),
        enabled: false,
        impact_lead_s: Some(1.0),
        driver: flighthook::SquareConfig::default(),
    };
    let full = flighthook::SquareSection {
        driver: {
            let mut config = flighthook::SquareConfig::default();
            config.address = Some("SquareGolf(CD01)".into());
            config.club = "PW".into();
            config.advanced_spin = false;
            config.discard_non_putting_zero_spin = false;
            config.dot_bottom_to_face_center_mm =
                std::collections::BTreeMap::from([("7I".into(), 21.5)]);
            config
        },
        ..square.clone()
    };
    keys_apart(&square, &full);
    assert_eq!(
        driver_keys::<flighthook::SquareConfig>(),
        BTreeSet::from(
            [
                "address",
                "advanced_spin",
                "club",
                "discard_non_putting_zero_spin",
                "dot_bottom_to_face_center_mm",
                // Read too (the British spelling), never written.
                "dot_bottom_to_face_centre_mm",
            ]
            .map(String::from)
        )
    );

    let pureplay = flighthook::PureplaySection {
        name: "Pureplay".into(),
        actor_id: Some("out-1".into()),
        enabled: false,
        driver: flighthook::PureplayConfig::default(),
        full_monitor: Some("lm-1".into()),
        chipping_monitor: Some("lm-2".into()),
        putting_monitor: Some("lm-3".into()),
    };
    let full = flighthook::PureplaySection {
        driver: flighthook::PureplayConfig {
            address: Some("192.168.1.20:9760".into()),
        },
        ..pureplay.clone()
    };
    keys_apart(&pureplay, &full);
    assert_eq!(
        driver_keys::<flighthook::PureplayConfig>(),
        BTreeSet::from(["address".to_owned()])
    );

    let client = flighthook::OpenConnectClientSection {
        name: "GSPro".into(),
        actor_id: Some("out-2".into()),
        enabled: false,
        driver: flighthook::OpenConnectClientConfig::default(),
        full_monitor: Some("lm-1".into()),
        chipping_monitor: Some("lm-2".into()),
        putting_monitor: Some("lm-3".into()),
        chipping_clubs: vec![flighthook::Club::SandWedge],
        putting_clubs: vec![flighthook::Club::Putter],
    };
    let full = flighthook::OpenConnectClientSection {
        driver: flighthook::OpenConnectClientConfig {
            address: Some("192.168.1.20:921".into()),
            ..flighthook::OpenConnectClientConfig::default()
        },
        ..client.clone()
    };
    keys_apart(&client, &full);
    assert_eq!(
        driver_keys::<flighthook::OpenConnectClientConfig>(),
        BTreeSet::from(["address".to_owned()])
    );

    let exputt = flighthook::ExPuttSection {
        name: "ExPutt".into(),
        actor_id: Some("lm-1".into()),
        enabled: false,
        impact_lead_s: Some(0.5),
        driver: flighthook::ExPuttConfig::default(),
    };
    let full = flighthook::ExPuttSection {
        driver: {
            let mut config = flighthook::ExPuttConfig::default();
            config.address = Some("192.168.1.50".into());
            config
        },
        ..exputt.clone()
    };
    keys_apart(&exputt, &full);
    assert_eq!(
        driver_keys::<flighthook::ExPuttConfig>(),
        BTreeSet::from(["address".to_owned()])
    );

    // The trackball's settings flatten their device config, so serde
    // reads them as a map: their keys are those of settings with every
    // field set (`grab` among them, which the section reads and
    // ignores).
    let trackball = flighthook::TrackballSection {
        name: "Arcade".into(),
        actor_id: Some("lm-1".into()),
        enabled: false,
        driver: flighthook::TrackballConfig::default(),
    };
    let mut full = trackball.clone();
    let d = &mut full.driver;
    d.device = Some("usb-Example_Trackball-event-mouse".into());
    d.club = flighthook::TrackballClub::Driver;
    d.hand = flighthook::TrackballHand::Left;
    d.detector.cpi = 800.0;
    d.detector.report_limit = 2047;
    d.detector.grab = false;
    d.detector.thresholds.v_start = 0.08;
    d.tuning.curve_sense = flighthook::TrackballCurveSense::Physical;
    let serde_json::Value::Object(driver) = serde_json::to_value(&full.driver).expect("json")
    else {
        panic!("a table");
    };
    let driver: BTreeSet<String> = driver.keys().cloned().collect();
    assert_eq!(
        driver,
        BTreeSet::from(
            [
                "club",
                "cpi",
                "device",
                "grab",
                "hand",
                "report_limit",
                "thresholds",
                "tuning",
            ]
            .map(String::from)
        )
    );
    let serde_json::Value::Object(host) = serde_json::to_value(&trackball).expect("json") else {
        panic!("a table");
    };
    assert_eq!(
        host.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "actor_id".to_owned(),
            "enabled".to_owned(),
            "name".to_owned()
        ])
    );
    // The card drops the driver's hand: it comes from the turn.
    full.driver.hand = flighthook::TrackballHand::default();
    let json = serde_json::to_string(&full).expect("json");
    assert_eq!(
        serde_json::from_str::<flighthook::TrackballSection>(&json).expect("reads"),
        full
    );
    let toml = toml::to_string(&full).expect("toml");
    assert_eq!(
        toml::from_str::<flighthook::TrackballSection>(&toml).expect("reads"),
        full,
        "{toml}"
    );
}
