//! [`DriverActor`]: a launch monitor card that runs on a flightrelay device
//! driver ([`DeviceDriver`]) as an FRP launch monitor actor.
//!
//! The driver owns the device: connect, session, shots, status, raw frames.
//! The actor is the host around it, one thread per card:
//!
//! - **The run loop.** It builds the driver from the card's section
//!   ([`DeviceDriver::new`]; a refused config shows on the card and the
//!   actor waits for its stop), connects, polls the session until it fails
//!   or the actor is stopped, and reconnects after the delay the driver
//!   asks for ([`DeviceDriver::retry_delay`], slept so that a stop cuts it
//!   short). The card reads Starting before the first attempt, Connected
//!   with each session, Reconnecting from the end of the first session on,
//!   and Disconnected once stopped or given up. A connect that fails puts
//!   its error on the card as the `error` row until a session connects, and
//!   in the log once per outage and again when the text changes.
//! - **The bus in.** flighthook's own `golfer.turn` becomes the driver's
//!   [`TurnHints`]: handed to the session whenever they change, and to a
//!   connect through [`ConnectHost::hints`]. The card's [`Hooks::action`]
//!   matches become session actions (one asked while no session runs is
//!   dropped when the next connect starts; one asked during a connect
//!   reaches the session it opens).
//! - **The bus out** ([`Sink`]): every shot message and alert goes on the
//!   bus under the card's actor id as soon as the driver gives it. The
//!   device's status and identity make the card's `bus.actor` (role
//!   `launch_monitor`, `device.source` from the card's shot source),
//!   published when it changes; while no session is up it is blocked by
//!   `not_armed` and carries no battery, attitude or telemetry. Raw frames
//!   go to the shot's capture, which goes out as the local `ShotCapture`
//!   just before the shot's `shot.finished`, never as FRP.
//! - **Live config** ([`DriverActor::reconfigure`]): host fields and the
//!   driver fields the driver applies live take effect without a restart,
//!   at the loop's next pass whatever it is doing (in a session, before a
//!   connect, during a retry delay); any other change restarts the card.
//! - **A driver panic** ends the card Disconnected with an error (and
//!   `not_armed`), never stuck Connected; Reconnect starts it again.
//!
//! What a card adds to its driver is its [`Hooks`]: its config section, its
//! shot source and any host-side processing of the device's messages.

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flightrelay::driver::{
    Backoff, ConnectCtx, ConnectHost, DeviceDriver, DeviceEvent, DeviceIdentity, DeviceSession,
    DeviceStatus, DriverInfo, Events, HostFlags, Level, SessionEnd, Stop, TurnHints,
};
use flightrelay::types::roles;
use flightrelay::{
    BusActor, DEFAULT_HEARTBEAT, DeviceInfo, FrpBody, RawFrame, Removal, ShotFinished, ShotTrigger,
    StateKind,
};
use tracing::{debug, error, info, warn};

use crate::actors::provenance::{ShotSource, Source};
use crate::actors::{Actor, ReconfigureOutcome};
use crate::bus::{BusMsg, BusReceiver, BusSender, LocalEvent, PollError};
use crate::raw_capture::{RawCapture, RawCaptureConfig};
use crate::state::SystemState;
use flighthook::{
    ActorStatus, DeviceSection, DriverConfig, FlightResult, Launch, Mode, RawDir, RawRecord,
    Severity, ShotKey, ShotRaw,
};

/// How long one session poll may take.
const POLL_TIMEOUT: Duration = Duration::from_millis(50);

/// How often the bus is drained while the actor waits (a retry delay, or
/// for its stop).
const IDLE_SLICE: Duration = Duration::from_millis(100);

/// The card's error after the device driver panicked.
const DRIVER_FAILED: &str = "the device driver failed; press Reconnect";

/// The phase of a running session in the device driver contract's list
/// (`DeviceEvent::Phase`). A driver that sets it as the last step of its
/// connect keeps it on the card while Connected.
const SESSION_PHASE: &str = "measuring";

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/// What a card adds to its device driver `D`.
pub trait Hooks<D: DeviceDriver>: Default + Send + 'static {
    /// The card's config section: host fields with `D`'s config flattened
    /// next to them.
    type Section: DeviceSection<Driver = D::Config>;

    /// The shot source the card is. Its class must be the source `D`
    /// declares ([`DriverInfo::source`]).
    const SOURCE: ShotSource;

    /// Host-side processing of one of the device's messages (a `shot.*`
    /// message or a `bus.alert`) before it goes on the bus. A hook keeps the
    /// message's kind. Raw frames do not reach it. Default: none.
    fn on_frp(&mut self, _body: &mut FrpBody, _ctx: &mut HookCtx<'_, Self::Section>) {}

    /// Messages the card adds to `shot`, its own, just before the shot's
    /// `shot.finished` goes on the bus (e.g. the Mevo card's launch-only
    /// spin estimate, as its own flight result). Default: none.
    fn before_finished(
        &mut self,
        _shot: &ShotSoFar,
        _ctx: &mut HookCtx<'_, Self::Section>,
    ) -> Vec<FrpBody> {
        Vec::new()
    }

    /// The device action a bus message asks of the card with card key
    /// `card`, if any. Default: none.
    fn action(_msg: &BusMsg, _card: &str) -> Option<D::Action> {
        None
    }
}

/// What [`Hooks::on_frp`] and [`Hooks::before_finished`] see besides the
/// message.
#[derive(Debug)]
pub struct HookCtx<'a, S> {
    /// The card's section as it is in effect.
    pub section: &'a S,
}

/// What the card sent of one of its shots so far.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotSoFar {
    pub key: ShotKey,
    /// The measured launch, if one went out.
    pub launch: Option<Launch>,
    /// The device's own flight result, if one went out.
    pub flight: Option<FlightResult>,
    /// The mode the session ran in at the shot's trigger.
    pub mode: Option<Mode>,
}

// ---------------------------------------------------------------------------
// The actor
// ---------------------------------------------------------------------------

/// A launch monitor card running device driver `D` with the card's hooks
/// `H`. See the [module docs](self).
pub struct DriverActor<D: DeviceDriver, H: Hooks<D>> {
    /// The section the actor runs with, as last applied.
    section: Mutex<H::Section>,
    stop: Stop,
    /// A section applied live, for the running thread to take.
    updates: Arc<Mutex<Option<H::Section>>>,
    _types: PhantomData<fn() -> (D, H)>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<D: DeviceDriver, H: Hooks<D>> DriverActor<D, H> {
    /// An actor for the card with config section `section`.
    pub fn new(section: H::Section) -> Self {
        Self {
            section: Mutex::new(section),
            stop: Stop::new(),
            updates: Arc::default(),
            _types: PhantomData,
        }
    }

    /// Take `new`, the card's section in the current config (`None`: the
    /// section was removed). Applied when only host fields changed, or
    /// driver fields the driver takes live
    /// ([`DriverConfig::restart_needed`]); the running thread then hands the
    /// new driver config to [`DeviceDriver::apply_config`]. A removed
    /// section, a restart-needing change, a config the driver refuses, or a
    /// change while the current config is refused is RestartRequired.
    fn apply_section(&self, new: Option<H::Section>) -> ReconfigureOutcome {
        let Some(new) = new else {
            return ReconfigureOutcome::RestartRequired;
        };
        let mut current = lock(&self.section);
        if *current == new {
            return ReconfigureOutcome::Applied;
        }
        let (old, next) = (current.driver_settings(), new.driver_settings());
        let live = old == next
            || (old.validate().is_ok()
                && next.validate().is_ok()
                && !<D::Config as DriverConfig>::restart_needed(&old, &next));
        if !live {
            return ReconfigureOutcome::RestartRequired;
        }
        *current = new.clone();
        *lock(&self.updates) = Some(new);
        ReconfigureOutcome::Applied
    }
}

impl<D: DeviceDriver, H: Hooks<D>> Actor for DriverActor<D, H> {
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> JoinHandle<()> {
        let run = Run::<D, H> {
            section: lock(&self.section).clone(),
            stop: self.stop.clone(),
            updates: Arc::clone(&self.updates),
            _types: PhantomData,
        };
        let name = format!("device:{}", sender.actor_id());
        std::thread::Builder::new()
            .name(name)
            .spawn(move || run.run(state, sender, receiver))
            .expect("failed to spawn a device driver thread")
    }

    /// Ends a blocking connect too: the driver checks the stop token.
    fn stop(&self) {
        self.stop.stop();
    }

    fn stop_wait(&self) -> Duration {
        D::STOP_WAIT
    }

    /// A device that takes one connection at a time must be let go first.
    fn replaceable_while_stopping(&self) -> bool {
        !D::info(&lock(&self.section).driver_settings()).exclusive
    }

    fn source(&self) -> Option<ShotSource> {
        Some(H::SOURCE)
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let Some((_, index)) = sender.actor_id().split_once('.') else {
            return ReconfigureOutcome::Applied;
        };
        let config = state.system.snapshot();
        self.apply_section(H::Section::sections(&config).get(index).cloned())
    }
}

// ---------------------------------------------------------------------------
// The run loop
// ---------------------------------------------------------------------------

/// What the actor's thread starts with.
struct Run<D: DeviceDriver, H: Hooks<D>> {
    section: H::Section,
    stop: Stop,
    updates: Arc<Mutex<Option<H::Section>>>,
    _types: PhantomData<fn() -> (D, H)>,
}

/// The lowest driver log level anyone sees: trace when the `audit` target
/// shows trace lines, else debug when this module shows debug, else info.
fn min_level() -> Level {
    if tracing::enabled!(target: "audit", tracing::Level::TRACE) {
        Level::Trace
    } else if tracing::enabled!(tracing::Level::DEBUG) {
        Level::Debug
    } else {
        Level::Info
    }
}

/// The text of panic payload `panic`.
fn panic_text(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

impl<D: DeviceDriver, H: Hooks<D>> Run<D, H> {
    /// The actor's thread: [`run_with`](Self::run_with), and if the driver
    /// panics, `not_armed` and Disconnected with [`DRIVER_FAILED`], then
    /// the thread ends (Reconnect starts a new one).
    fn run(self, state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let actor = sender.actor_id().to_owned();
        let info = D::info(&self.section.driver_settings());
        // The fixed mode is forgotten when this thread ends.
        let _fixed_mode = match (info.fixed_mode, sender.frp_id()) {
            (Some(mode), Some(id)) => Some(state.set_fixed_mode(id, mode)),
            _ => None,
        };
        let flags = HostFlags::new();
        flags.set_wants_raw(true);
        flags.set_min_level(min_level());
        let mut sink = Sink::<D, H>::new(state, sender, info, self.section.clone());
        let mut host = Host::<D, H>::new(
            receiver,
            self.stop.clone(),
            actor.clone(),
            &sink.state,
            Synced::new(&sink),
        );
        sink.publish_actor();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.run_with(&mut host, &mut sink, &flags);
        }));
        if let Err(panic) = ran {
            error!("{actor}: the device driver failed: {}", panic_text(&*panic));
            sink.session_ended();
            let mut telemetry = HashMap::from([("error".into(), DRIVER_FAILED.into())]);
            sink.end_phase(&mut telemetry);
            sink.status(ActorStatus::Disconnected, telemetry);
        } else if host.stopped() {
            sink.remove_actor();
        }
    }

    /// Build the driver, then connect, drive each session and wait out the
    /// retry delays until the actor is stopped or the driver gives up.
    fn run_with(&self, host: &mut Host<D, H>, sink: &mut Sink<D, H>, flags: &HostFlags) {
        let actor = sink.actor.clone();
        let mut driver = match D::new(sink.section.driver_settings()) {
            Ok(driver) => driver,
            Err(e) => {
                warn!("{actor}: settings not usable, not started: {e}");
                sink.status(
                    ActorStatus::Disconnected,
                    HashMap::from([("error".into(), e.to_string())]),
                );
                self.wait_for_stop(host, sink, None);
                return;
            }
        };

        let mut backoff = Backoff::default();
        let mut attempts = 0u32;
        sink.status(ActorStatus::Starting, HashMap::new());
        loop {
            if host.stopped() {
                break;
            }
            take_update(&self.updates, sink, Some(&mut driver));
            // Device actions asked while no session ran were for no device.
            host.actions.clear();
            sink.begin_session();
            host.begin_connect();
            flags.set_shots_allowed(sink.shots_allowed());
            let connected = {
                let mut emit = |e| sink.handle(e);
                let out = Events::new(&mut emit, flags);
                let mut ctx = ConnectCtx::new(&self.stop, out, host);
                driver.connect(&mut ctx)
            };
            let (error, connected_for) = match connected {
                Err(e) => {
                    if host.stopped() {
                        break;
                    }
                    // The failed connect's phase is not shown through the
                    // retry delay; its error is.
                    sink.connect_failed(&e.to_string());
                    attempts += 1;
                    (e, None)
                }
                Ok(mut session) => {
                    attempts = 0;
                    let since = Instant::now();
                    sink.connected(session.identity());
                    let ended = self.drive(&mut session, &mut driver, host, sink, flags);
                    match ended {
                        Err(e) if !host.stopped() => {
                            drop(session);
                            sink.session_lost();
                            (e, Some(since.elapsed()))
                        }
                        _ => {
                            let mut emit = |e| sink.handle(e);
                            if let Err(e) = session.close(&mut Events::new(&mut emit, flags)) {
                                warn!("{actor}: the device was not released cleanly: {e}");
                            }
                            break;
                        }
                    }
                }
            };
            let ended = SessionEnd {
                error: Some(&error),
                connected_for,
                attempts,
            };
            let delay = {
                let mut emit = |e| sink.handle(e);
                driver.retry_delay(&ended, &mut backoff, &mut Events::new(&mut emit, flags))
            };
            match delay {
                Some(delay) => {
                    if !self.wait(host, sink, &mut driver, delay) {
                        break;
                    }
                }
                None => {
                    warn!("{actor}: giving up: {error}");
                    sink.status(
                        ActorStatus::Disconnected,
                        HashMap::from([("error".into(), error.to_string())]),
                    );
                    self.wait_for_stop(host, sink, Some(&mut driver));
                    return;
                }
            }
        }
        // A stop that ended a connect leaves no phase and no connect error
        // on the card.
        sink.session_ended();
        let mut telemetry = HashMap::new();
        sink.end_phase(&mut telemetry);
        sink.end_connect_error(&mut telemetry);
        sink.status(ActorStatus::Disconnected, telemetry);
    }

    /// Wait `duration` (a retry delay), draining the bus and taking live
    /// config; `false` when a stop cut it short.
    fn wait(
        &self,
        host: &mut Host<D, H>,
        sink: &mut Sink<D, H>,
        driver: &mut D,
        duration: Duration,
    ) -> bool {
        let end = Instant::now() + duration;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if !host.idle(left.min(IDLE_SLICE)) {
                return false;
            }
            take_update(&self.updates, sink, Some(driver));
            if left <= IDLE_SLICE {
                return true;
            }
        }
    }

    /// Wait for the stop (refused settings, or the driver gave up), taking
    /// live config meanwhile.
    fn wait_for_stop(
        &self,
        host: &mut Host<D, H>,
        sink: &mut Sink<D, H>,
        mut driver: Option<&mut D>,
    ) {
        while host.idle(IDLE_SLICE) {
            take_update(&self.updates, sink, driver.as_deref_mut());
        }
    }

    /// Poll `session` until it fails (`Err`) or the actor is stopped
    /// (`Ok`), handing it new turn hints and the bus's actions, live config
    /// and the round policy between polls.
    fn drive(
        &self,
        session: &mut D::Session,
        driver: &mut D,
        host: &mut Host<D, H>,
        sink: &mut Sink<D, H>,
        flags: &HostFlags,
    ) -> Result<(), D::Error> {
        // The connect read the hints at some point; a turn that changed
        // since reaches the session now.
        let mut given = host.connect_hints.take().unwrap_or_default();
        loop {
            host.drain();
            if host.stopped() {
                return Ok(());
            }
            if host.hints != given {
                given = host.hints;
                session.hints(&given)?;
            }
            while let Some(action) = host.actions.pop_front() {
                session.action(&action)?;
            }
            take_update(&self.updates, sink, Some(driver));
            flags.set_shots_allowed(sink.shots_allowed());
            sink.session_mode = given.mode;
            {
                let mut emit = |e| sink.handle(e);
                session.poll(POLL_TIMEOUT, &mut Events::new(&mut emit, flags))?;
            }
            sink.prune_shots();
        }
    }
}

/// Take a section applied live, if one waits in `updates`: the sink's (and
/// so the hooks') section, and the driver's config when its part changed
/// and there is a driver.
fn take_update<D: DeviceDriver, H: Hooks<D>>(
    updates: &Mutex<Option<H::Section>>,
    sink: &mut Sink<D, H>,
    driver: Option<&mut D>,
) {
    let Some(new) = lock(updates).take() else {
        return;
    };
    if let Some(driver) = driver {
        let settings = new.driver_settings();
        if settings != sink.section.driver_settings() {
            driver.apply_config(&settings);
        }
    }
    sink.section = new;
    // A renamed card is a renamed actor.
    sink.publish_actor();
}

// ---------------------------------------------------------------------------
// The bus in
// ---------------------------------------------------------------------------

/// The actor's inputs: the bus, the turn hints, queued actions and the stop
/// token. It is the driver's [`ConnectHost`].
struct Host<D: DeviceDriver, H> {
    receiver: BusReceiver,
    /// The hints of flighthook's latest turn.
    hints: TurnHints,
    /// The hints a connect read last ([`ConnectHost::hints`]).
    connect_hints: Option<TurnHints>,
    actions: VecDeque<D::Action>,
    stop: Stop,
    actor: String,
    /// flighthook's actor id: only its turn counts.
    session: String,
    /// Answers `bus.sync`.
    synced: Synced,
    _hooks: PhantomData<fn() -> H>,
}

/// What the card republishes when another actor sends `bus.sync`: the
/// `bus.actor` it last published. The host answers while it drains the bus,
/// so a card added later learns the device's state at once, also while a
/// connect runs.
struct Synced {
    sender: BusSender,
    published: Arc<Mutex<Option<BusActor>>>,
}

impl Synced {
    fn new<D: DeviceDriver, H: Hooks<D>>(sink: &Sink<D, H>) -> Self {
        Self {
            sender: sink.sender.clone(),
            published: Arc::clone(&sink.published),
        }
    }

    /// Republish the card's `bus.actor`, if it has published one.
    fn answer(&self) {
        let published = lock(&self.published).clone();
        if let Some(actor) = published {
            self.sender.frp(actor);
        }
    }
}

impl<D: DeviceDriver, H: Hooks<D>> Host<D, H> {
    /// Inputs over `receiver`, with the hints of the session's turn now.
    fn new(
        receiver: BusReceiver,
        stop: Stop,
        actor: String,
        state: &SystemState,
        synced: Synced,
    ) -> Self {
        Self {
            receiver,
            hints: TurnHints::from_turn(&state.session.turn()),
            connect_hints: None,
            actions: VecDeque::new(),
            stop,
            actor,
            session: state.flighthook_id(),
            synced,
            _hooks: PhantomData,
        }
    }

    /// Take what is on the bus: flighthook's turn into the hints, the card's
    /// actions into their queue. A closed bus or a shutdown flag stops the
    /// actor.
    fn drain(&mut self) {
        loop {
            let msg = match self.receiver.poll() {
                Ok(Some(msg)) => msg,
                Ok(None) => return,
                Err(PollError::Shutdown) => {
                    self.stop.stop();
                    return;
                }
            };
            if let Some((turn, _)) = msg.turn_of(&self.session) {
                self.hints = TurnHints::from_turn(turn);
            }
            if msg
                .as_frp()
                .is_some_and(|m| matches!(m.body, FrpBody::BusSync(_)))
            {
                self.synced.answer();
            }
            if let Some(action) = H::action(&msg, &self.actor) {
                self.actions.push_back(action);
            }
        }
    }

    /// Whether the actor was told to stop.
    fn stopped(&self) -> bool {
        if self.receiver.is_shutdown() {
            self.stop.stop();
        }
        self.stop.is_stopped()
    }

    /// Wait `duration`, draining the bus; `false` when a stop cut it short.
    fn idle(&mut self, duration: Duration) -> bool {
        let end = Instant::now() + duration;
        loop {
            self.drain();
            if self.stopped() {
                return false;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(IDLE_SLICE));
        }
    }
}

impl<D: DeviceDriver, H: Hooks<D>> ConnectHost for Host<D, H> {
    fn tick(&mut self) {
        self.drain();
    }

    fn hints(&self) -> &TurnHints {
        &self.hints
    }
}

impl<D: DeviceDriver, H: Hooks<D>> Host<D, H> {
    /// Note the hints a connect starts with, so the session gets only a
    /// later change.
    fn begin_connect(&mut self) {
        self.connect_hints = Some(self.hints);
    }
}

// ---------------------------------------------------------------------------
// The bus out
// ---------------------------------------------------------------------------

/// Where the driver's events go: the bus, the shot's raw capture and the
/// log. See the [module docs](self).
struct Sink<D: DeviceDriver, H: Hooks<D>> {
    state: Arc<SystemState>,
    sender: BusSender,
    actor: String,
    info: DriverInfo,
    source: Source,
    section: H::Section,
    hooks: H,
    /// The session's device id (for raw frame numbers), once connected.
    device: Option<String>,
    /// The device's identity, while a session is up.
    identity: Option<DeviceIdentity>,
    /// The card's status last sent.
    status: ActorStatus,
    /// The device's status, as the card's `bus.actor` carries it.
    device_status: DeviceStatus,
    /// The `bus.actor` last published, shared with [`Host`], which
    /// republishes it in answer to `bus.sync`.
    published: Arc<Mutex<Option<BusActor>>>,
    /// A connect is running: the device's status goes out with the
    /// session's identity once it connects.
    connecting: bool,
    /// The `phase` last set during this connect, if any (cleared when the
    /// session connects).
    phase_shown: Option<&'static str>,
    /// The error of the last failed connect of the current outage: the card's
    /// `error` row, on every status sent until a session connects.
    connect_error: Option<String>,
    /// The driver alert last logged, until a session connects: the same
    /// alert again (a driver may raise it on every retry) is not logged
    /// again.
    last_alert: Option<(Severity, String)>,
    /// The session's raw capture, for a driver with a raw profile.
    capture: Option<RawCapture>,
    /// The next raw frame number per device id, across sessions.
    seqs: HashMap<String, u64>,
    /// What went out of each shot not finished yet, by shot id, with when
    /// it started.
    shots: HashMap<String, (ShotSoFar, Instant)>,
    /// Shots the round policy refused at their trigger, by shot id, with
    /// when.
    refused: HashMap<String, Instant>,
    /// A shot has been held back for the tournament round since the last
    /// one went out (one log line per round).
    holding: bool,
    /// Shots finished over the actor's life.
    shot_count: u32,
    /// Puts the driver's timestamps on the host clock.
    clock: UtcClock,
    /// The mode the session runs in ([`HookCtx::session_mode`]).
    session_mode: Option<Mode>,
    _driver: PhantomData<fn() -> D>,
}

/// How far the host's wall clock may drift from the monotonic clock before
/// [`UtcClock`] takes a new anchor.
const MAX_CLOCK_DRIFT: Duration = Duration::from_millis(20);

/// Puts a driver's UTC timestamps (milliseconds since the Unix epoch) on
/// this host's monotonic clock, from one anchor pair, so every timestamp of
/// the same millisecond maps to the same instant: raw frames stamped in one
/// millisecond keep the order the driver handed them over (the capture sorts
/// by time, stably). A new anchor is taken when the wall clock has drifted
/// more than [`MAX_CLOCK_DRIFT`] from it.
#[derive(Debug, Default)]
struct UtcClock {
    /// A monotonic instant and the UTC milliseconds read with it.
    anchor: Option<(Instant, u64)>,
}

/// The current UTC time, milliseconds since the Unix epoch.
fn utc_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
}

/// `at` moved by `ms` milliseconds (back for a negative `ms`), or `at` when
/// that is out of range.
fn shifted(at: Instant, ms: i128) -> Instant {
    let by = Duration::from_millis(u64::try_from(ms.unsigned_abs()).unwrap_or(u64::MAX));
    if ms >= 0 {
        at.checked_add(by)
    } else {
        at.checked_sub(by)
    }
    .unwrap_or(at)
}

impl UtcClock {
    /// The instant of `ms` on this host's monotonic clock.
    fn instant_of(&mut self, ms: u64) -> Instant {
        let now = Instant::now();
        let Some(now_ms) = utc_ms() else {
            return now;
        };
        let fresh = self.anchor.is_some_and(|(mono, utc)| {
            let predicted = shifted(mono, i128::from(now_ms) - i128::from(utc));
            predicted.max(now) - predicted.min(now) <= MAX_CLOCK_DRIFT
        });
        if !fresh {
            self.anchor = Some((now, now_ms));
        }
        let (mono, utc) = self.anchor.unwrap_or((now, now_ms));
        shifted(mono, i128::from(ms) - i128::from(utc))
    }
}

/// The audit log line of raw frame `frame` of actor `actor`: direction,
/// number, label, then the bytes as hex.
fn audit_line(actor: &str, frame: &RawFrame) -> String {
    format!(
        "{actor} {} #{} {} {}",
        frame.dir,
        frame.seq,
        frame.label.as_deref().unwrap_or(""),
        flighthook::hex_lower(&frame.data)
    )
}

impl<D: DeviceDriver, H: Hooks<D>> Sink<D, H> {
    fn new(
        state: Arc<SystemState>,
        sender: BusSender,
        info: DriverInfo,
        section: H::Section,
    ) -> Self {
        Self {
            actor: sender.actor_id().to_owned(),
            state,
            sender,
            info,
            source: H::SOURCE.provenance(),
            section,
            hooks: H::default(),
            device: None,
            identity: None,
            status: ActorStatus::Starting,
            device_status: DeviceStatus::not_armed(),
            published: Arc::new(Mutex::new(None)),
            connecting: false,
            phase_shown: None,
            connect_error: None,
            last_alert: None,
            capture: None,
            seqs: HashMap::new(),
            shots: HashMap::new(),
            refused: HashMap::new(),
            holding: false,
            shot_count: 0,
            clock: UtcClock::default(),
            session_mode: None,
            _driver: PhantomData,
        }
    }

    /// The card's `bus.actor` as it is now: the card's name, role
    /// `launch_monitor`, the device's identity and status, and the card's
    /// shot source.
    fn bus_actor(&self) -> BusActor {
        let name = crate::actors::actor_names(&self.state.system.snapshot())
            .remove(&self.actor)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.info.name.to_owned());
        let identity = self.identity.as_ref();
        let status = &self.device_status;
        let mut actor = BusActor::new(name, DEFAULT_HEARTBEAT.as_secs_f64())
            .with_role(roles::LAUNCH_MONITOR)
            .with_device(DeviceInfo {
                manufacturer: identity
                    .and_then(|i| i.manufacturer.clone())
                    .or_else(|| self.info.manufacturer.map(str::to_owned)),
                model: identity.and_then(|i| i.model.clone()),
                firmware: identity.and_then(|i| i.firmware.clone()),
                source: Some(self.source),
                battery_pct: status.battery_pct,
                tilt: status.tilt,
                roll: status.roll,
            });
        actor.blockers.clone_from(&status.blockers);
        actor.telemetry = (!status.telemetry.is_empty()).then(|| status.telemetry.clone());
        actor
    }

    /// Send the card's `bus.actor` removal: the actor stopped cleanly.
    /// Every consumer forgets it; a `bus.sync` gets no answer from it.
    fn remove_actor(&mut self) {
        *lock(&self.published) = None;
        self.sender.frp(Removal::of(StateKind::BusActor));
    }

    /// Publish the card's `bus.actor` when it differs from the one last
    /// published.
    fn publish_actor(&mut self) {
        let actor = self.bus_actor();
        let mut published = lock(&self.published);
        if published.as_ref() != Some(&actor) {
            *published = Some(actor.clone());
            self.sender.frp(actor);
        }
    }

    /// Send status `status` with card telemetry `telemetry`, and the
    /// connect error still held unless the status is Connected or carries
    /// an `error` of its own (a Starting status drops what the card held).
    fn status(&mut self, status: ActorStatus, mut telemetry: HashMap<String, String>) {
        self.status = status;
        self.hold_connect_error(&mut telemetry);
        self.sender
            .local(LocalEvent::CardStatus { status, telemetry });
    }

    /// Put the connect error held into `telemetry`, unless the status shown
    /// is Connected or `telemetry` has an `error`.
    fn hold_connect_error(&self, telemetry: &mut HashMap<String, String>) {
        if let Some(error) = &self.connect_error
            && self.status != ActorStatus::Connected
        {
            telemetry
                .entry("error".into())
                .or_insert_with(|| error.clone());
        }
    }

    /// A connect failed with `error`: the card shows it as its `error` row
    /// through the retry delay, and the log gets one warning when it is the
    /// first of the outage or the text changed. The connect's `phase` leaves
    /// the card.
    fn connect_failed(&mut self, error: &str) {
        let changed = self.connect_error.as_deref() != Some(error);
        if changed {
            warn!("{}: cannot connect: {error}", self.actor);
            self.connect_error = Some(error.to_owned());
        }
        let mut telemetry = HashMap::new();
        self.end_phase(&mut telemetry);
        if changed || !telemetry.is_empty() {
            self.status(self.status, telemetry);
        }
    }

    /// The outage is over (a session connected, or the actor stopped): takes
    /// the held connect error back, removing the card's `error` row through
    /// `telemetry` (an empty value removes the key).
    fn end_connect_error(&mut self, telemetry: &mut HashMap<String, String>) {
        if self.connect_error.take().is_some() {
            telemetry.insert("error".into(), String::new());
        }
    }

    /// A connect is about to start: a new session's status and capture.
    fn begin_session(&mut self) {
        self.device_status = DeviceStatus::not_armed();
        self.connecting = true;
        self.capture = self.info.raw.as_ref().map(|p| {
            RawCapture::new(RawCaptureConfig {
                transport: p.transport,
                pre_roll_frames: p.pre_roll_frames,
                max_frames: p.max_frames,
                max_bulk_bytes: p.max_bulk_bytes,
            })
        });
    }

    /// A session connected as `identity`: Connected, with the card text and
    /// the shot count (and the connect's `phase` removed, unless the driver
    /// ended its connect on [`SESSION_PHASE`], which describes the session),
    /// then the card's `bus.actor` with the device's identity and the status
    /// the driver sent while connecting, before any shot.
    fn connected(&mut self, identity: &DeviceIdentity) {
        self.connecting = false;
        self.last_alert = None;
        // Frames sent while the first connect ran were numbered under the
        // actor's id; the device's numbers go on from there.
        if !self.seqs.contains_key(&identity.device)
            && let Some(next) = self.seqs.remove(&self.actor)
        {
            self.seqs.insert(identity.device.clone(), next);
        }
        self.device = Some(identity.device.clone());
        self.identity = Some(identity.clone());
        if self.connect_error.is_some() {
            info!(
                "{}: connected to {} after failed connects",
                self.actor,
                identity.label()
            );
        } else {
            info!("{}: connected to {}", self.actor, identity.label());
        }
        let mut telemetry = HashMap::from([
            ("device_info".into(), identity.label()),
            ("shot_count".into(), self.shot_count.to_string()),
        ]);
        self.end_connect_error(&mut telemetry);
        // An empty value removes the key: the phases were the connect's.
        match self.phase_shown.take() {
            Some(SESSION_PHASE) => {
                telemetry.insert("phase".into(), SESSION_PHASE.into());
            }
            Some(_) => {
                telemetry.insert("phase".into(), String::new());
            }
            None => {}
        }
        self.status(ActorStatus::Connected, telemetry);
        self.publish_actor();
    }

    /// A connect ended without a session: removes the `phase` it set, if
    /// any, through `telemetry` (an empty value removes the key).
    fn end_phase(&mut self, telemetry: &mut HashMap<String, String>) {
        if self.phase_shown.take().is_some() {
            telemetry.insert("phase".into(), String::new());
        }
    }

    /// The session failed: `not_armed`, then Reconnecting, so a card never
    /// shows a lost connection's values.
    fn session_lost(&mut self) {
        self.session_ended();
        self.status(ActorStatus::Reconnecting, HashMap::new());
    }

    /// No session: the card's `bus.actor` is `not_armed`, with no battery,
    /// attitude or telemetry, and its shots in progress are forgotten.
    fn session_ended(&mut self) {
        self.device_status = DeviceStatus::not_armed();
        self.identity = None;
        self.shots.clear();
        self.publish_actor();
    }

    /// Whether the driver may start a shot now, by the round policy; logs
    /// once per round when a virtual source's shots are held back.
    fn shots_allowed(&mut self) -> bool {
        let allowed = self.state.round.allows(self.source);
        if allowed {
            self.holding = false;
        } else if !self.holding {
            self.holding = true;
            info!("{}: no shots during the tournament round", self.actor);
        }
        allowed
    }

    /// The mode the session runs in: the driver's fixed mode, else the turn
    /// hints' mode.
    fn mode(&self) -> Option<Mode> {
        self.info.fixed_mode.or(self.session_mode)
    }

    /// Put one driver event where it goes.
    fn handle(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::Frp(body) => self.frp(body),
            DeviceEvent::Status(status) => {
                self.device_status = status;
                if !self.connecting {
                    self.publish_actor();
                }
            }
            DeviceEvent::StatusChange(change) => {
                self.device_status.apply(change);
                if !self.connecting {
                    self.publish_actor();
                }
            }
            DeviceEvent::Phase(phase) => {
                if self.connecting {
                    self.phase_shown = Some(phase);
                }
                let mut telemetry = HashMap::from([("phase".into(), phase.to_owned())]);
                self.hold_connect_error(&mut telemetry);
                self.sender.local(LocalEvent::CardStatus {
                    status: self.status,
                    telemetry,
                });
            }
            DeviceEvent::Log(level, line) => {
                let actor = &self.actor;
                match level {
                    Level::Trace => tracing::trace!(target: "audit", "{actor} {line}"),
                    Level::Debug => debug!("{actor}: {line}"),
                    Level::Info => info!("{actor}: {line}"),
                    Level::Warn => warn!("{actor}: {line}"),
                }
            }
            DeviceEvent::ShotDiscarded => {
                if let Some(capture) = &mut self.capture {
                    capture.discard();
                }
            }
            _ => {}
        }
    }

    fn frp(&mut self, mut body: FrpBody) {
        if let FrpBody::ShotRaw(frame) = body {
            self.raw(frame);
            return;
        }
        if let Some(key) = body.shot_key() {
            let id = key.shot_id.clone();
            if self.refused.contains_key(&id) {
                if matches!(body, FrpBody::ShotFinished(_)) {
                    self.refused.remove(&id);
                }
                return;
            }
        }
        let mut ctx = HookCtx {
            section: &self.section,
        };
        self.hooks.on_frp(&mut body, &mut ctx);
        match body {
            FrpBody::ShotTrigger(trigger) => self.trigger(trigger),
            FrpBody::ShotFinished(finished) => self.finished(finished.key),
            FrpBody::ShotLaunch(ref m) => {
                if let Some((shot, _)) = self.shots.get_mut(&m.key.shot_id) {
                    shot.launch.get_or_insert_with(|| m.launch.clone());
                }
                self.sender.frp(body);
            }
            FrpBody::ShotFlight(ref m) => {
                if let Some((shot, _)) = self.shots.get_mut(&m.key.shot_id) {
                    shot.flight.get_or_insert_with(|| m.flight.clone());
                }
                self.sender.frp(body);
            }
            FrpBody::ShotClub(_) | FrpBody::ShotImpact(_) => self.sender.frp(body),
            FrpBody::BusAlert(alert) => {
                let pair = (alert.severity, alert.message.clone());
                if self.last_alert.as_ref() != Some(&pair) {
                    match alert.severity {
                        Severity::Warn => warn!("{}: {}", self.actor, alert.message),
                        _ => error!("{}: {}", self.actor, alert.message),
                    }
                    self.last_alert = Some(pair);
                }
                self.sender.frp(alert);
            }
            // Not the driver's to send.
            _ => {}
        }
    }

    /// A shot's trigger: on the bus when the round policy lets it start
    /// (the atomic check, a backstop to the driver's own `shots_allowed`),
    /// which opens its raw capture window; else the rest of the shot is
    /// dropped.
    fn trigger(&mut self, trigger: ShotTrigger) {
        let key = trigger.key.clone();
        let timestamp = trigger.timestamp;
        let sender = &self.sender;
        let sent = self
            .state
            .round
            .send_if_allowed(self.source, || sender.frp(trigger));
        if sent.is_some() {
            self.holding = false;
            let mode = self.mode();
            self.shots.insert(
                key.shot_id.clone(),
                (
                    ShotSoFar {
                        key,
                        launch: None,
                        flight: None,
                        mode,
                    },
                    Instant::now(),
                ),
            );
            if let Some(capture) = &mut self.capture {
                let at = match timestamp {
                    Some(ms) => self.clock.instant_of(ms),
                    None => Instant::now(),
                };
                capture.mark_trigger_at(at);
            }
        } else {
            self.refused.insert(key.shot_id, Instant::now());
            if !std::mem::replace(&mut self.holding, true) {
                info!("{}: no shots during the tournament round", self.actor);
            }
        }
    }

    /// A shot's end: what the hooks add to it, the shot's raw frames
    /// (local), then `shot.finished`, and the shot count.
    fn finished(&mut self, key: ShotKey) {
        let shot = self.shots.remove(&key.shot_id).map_or_else(
            || ShotSoFar {
                key: key.clone(),
                launch: None,
                flight: None,
                mode: self.mode(),
            },
            |(shot, _)| shot,
        );
        let mut ctx = HookCtx {
            section: &self.section,
        };
        let extra = self.hooks.before_finished(&shot, &mut ctx);
        for body in extra {
            self.sender.frp(body);
        }
        let raw = match &mut self.capture {
            Some(capture) => capture.take(),
            None => ShotRaw {
                transport: "none".into(),
                unavailable: Some(format!("{} device has no wire protocol", self.info.kind)),
                ..ShotRaw::default()
            },
        };
        self.sender.local(LocalEvent::ShotCapture {
            key: key.clone(),
            raw: Box::new(raw),
            scrub: self.device.iter().cloned().collect(),
        });
        self.sender.frp(ShotFinished { key });
        self.shot_count += 1;
        self.sender.local(LocalEvent::CardStatus {
            status: self.status,
            telemetry: HashMap::from([("shot_count".into(), self.shot_count.to_string())]),
        });
    }

    /// Forget shots that never finished within flighthook's shot timeout,
    /// and refused shots after the same time.
    fn prune_shots(&mut self) {
        self.refused
            .retain(|_, at| at.elapsed() < flighthook::SHOT_TIMEOUT);
        let actor = &self.actor;
        self.shots.retain(|_, (shot, since)| {
            let keep = since.elapsed() < flighthook::SHOT_TIMEOUT;
            if !keep {
                debug!("{actor}: shot #{} never finished", shot.key.shot_number);
            }
            keep
        });
    }

    /// A raw frame: numbered per device id, hex-logged at the `audit`
    /// target, and recorded for the export with its label as the driver
    /// gave it, charged to the bulk budget by the driver's profile.
    fn raw(&mut self, mut frame: RawFrame) {
        let Some(profile) = &self.info.raw else {
            return;
        };
        let device = self.device.clone().unwrap_or_else(|| self.actor.clone());
        let next = self.seqs.entry(device).or_insert(0);
        frame.seq = *next;
        *next += 1;
        tracing::debug!(target: "audit", "{}", audit_line(&self.actor, &frame));
        let Some(capture) = &mut self.capture else {
            return;
        };
        let bulk = (profile.is_bulk)(&frame);
        let dir = match frame.dir {
            flightrelay::RawDir::Rx => RawDir::Rx,
            flightrelay::RawDir::Tx => RawDir::Tx,
        };
        let at = match frame.timestamp {
            Some(ms) => self.clock.instant_of(ms),
            None => Instant::now(),
        };
        let mut record = if profile.text {
            match String::from_utf8(frame.data) {
                Ok(text) => RawRecord::text(dir, text),
                Err(e) => RawRecord::binary(dir, e.as_bytes()),
            }
        } else {
            RawRecord::binary(dir, &frame.data)
        };
        record.label = frame.label;
        capture.push(record, at, bulk);
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
    use crate::state::round::RoundRequest;
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

    /// What one session poll does; an empty list idles.
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
    }

    static SCRIPTS: LazyLock<Mutex<HashMap<u64, Arc<Mutex<Script>>>>> =
        LazyLock::new(Mutex::default);
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

    impl DeviceDriver for FakeDriver {
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
            if let Some(error) = ended.error {
                out.alert(Severity::Warn, error.to_string());
            }
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

    impl DeviceSession for FakeSession {
        type Driver = FakeDriver;

        fn poll(&mut self, timeout: Duration, out: &mut Events<'_>) -> Result<(), FakeError> {
            match self.steps.pop_front() {
                Some(Step::Emit(events)) => {
                    for event in events {
                        out.emit(event);
                    }
                    Ok(())
                }
                Some(Step::Fail(why)) => Err(FakeError(why.into())),
                Some(Step::Panic) => panic!("a decoder bug"),
                None => {
                    std::thread::sleep(timeout.min(Duration::from_millis(10)));
                    Ok(())
                }
            }
        }

        fn hints(&mut self, hints: &TurnHints) -> Result<(), FakeError> {
            lock(&self.script).commands.push(*hints);
            Ok(())
        }

        fn identity(&self) -> &DeviceIdentity {
            &self.identity
        }

        fn close(self, out: &mut Events<'_>) -> Result<(), FakeError> {
            lock(&self.script).closes += 1;
            out.ready(false);
            Ok(())
        }
    }

    /// A measured card. Its hook fills an estimated backspin into a launch
    /// without spin on a card named "estimate".
    #[derive(Default)]
    struct FakeHooks;

    impl Hooks<FakeDriver> for FakeHooks {
        type Section = FakeSection;
        const SOURCE: ShotSource = ShotSource::Mevo;

        fn on_frp(&mut self, body: &mut FrpBody, ctx: &mut HookCtx<'_, FakeSection>) {
            if let FrpBody::ShotLaunch(m) = body
                && ctx.section.name == "estimate"
                && m.launch.backspin_rpm.is_none()
            {
                m.launch.backspin_rpm = Some(1234);
            }
        }
    }

    /// A virtual card on the same driver.
    #[derive(Default)]
    struct VirtualHooks;

    impl Hooks<FakeDriver> for VirtualHooks {
        type Section = FakeSection;
        const SOURCE: ShotSource = ShotSource::MockMonitor;
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

    fn club_of(key: ShotKey, club: ClubData) -> FrpBody {
        flightrelay::ShotClub { key, club }.into()
    }

    fn finished(key: ShotKey) -> FrpBody {
        ShotFinished { key }.into()
    }

    fn frame(dir: flightrelay::RawDir, label: &str, data: &[u8]) -> DeviceEvent {
        let mut frame = RawFrame::new(dir, 0, data.to_vec());
        frame.label = Some(label.into());
        frp(FrpBody::ShotRaw(frame))
    }

    /// A connected sink of card `H` named `name`, and the bus behind it.
    fn sink<H: Hooks<FakeDriver, Section = FakeSection>>(
        name: &str,
    ) -> (
        TempDir,
        Sink<FakeDriver, H>,
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
        Sink<FakeDriver, H>,
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
        let mut sink = Sink::new(state, sender, FakeDriver::info(&section.driver), section);
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
                "status", "actor", "actor", "alert", "trigger", "launch", "flight", "club",
                "impact", "capture", "finished", "status"
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
        let ms = utc_ms().expect("clock");
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
        let ms = utc_ms().expect("clock");
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
    /// `bus.actor` with the identity once the session connects, and the
    /// connect's `phase` leaves the card then; a connect without a phase
    /// sends no `phase` key.
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
        let seen = drain(&mut rx);
        assert_eq!(kinds(&seen), ["status"], "the phase only");
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
        assert_eq!(audit_line("fake.0", &beef), "fake.0 rx #7 D4 DSP>APP beef");

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

    /// Tournament mode holds a virtual card's shots (the trackball and the
    /// mock run on this driver), and they are taken again once it is off.
    #[test]
    fn tournament_mode_holds_a_virtual_cards_shots() {
        let (_dir, mut sink, mut rx, _w) = sink::<VirtualHooks>("");
        drain(&mut rx);
        sink.state.round.set_manual(true);
        assert!(!sink.shots_allowed());
        for event in [
            trigger(key(1)),
            launch_of(key(1), launch()),
            finished(key(1)),
        ] {
            sink.handle(frp(event));
        }
        assert!(drain(&mut rx).is_empty(), "nothing of the shot");
        assert!(sink.shots.is_empty());
        sink.state.round.set_manual(false);
        assert!(sink.shots_allowed());
    }

    /// During a round a virtual card's driver is told to hold its shots;
    /// a shot that starts anyway is dropped whole by the round policy.
    #[test]
    fn a_round_holds_a_virtual_cards_shots() {
        let (_dir, mut sink, mut rx, _w) = sink::<VirtualHooks>("");
        drain(&mut rx);
        assert!(sink.shots_allowed());
        let guard = sink
            .state
            .round
            .begin_round(&RoundRequest {
                holder: "Pureplay",
                feeds: &[],
            })
            .expect("a round");
        assert!(!sink.shots_allowed());
        for event in [
            trigger(key(1)),
            launch_of(key(1), launch()),
            club_of(key(1), ClubData::default()),
            finished(key(1)),
        ] {
            sink.handle(frp(event));
        }
        assert!(drain(&mut rx).is_empty(), "nothing of the shot");
        assert!(sink.refused.is_empty() && sink.shots.is_empty());
        sink.handle(frp(trigger(key(9))));
        assert_eq!(sink.refused.len(), 1, "a shot that never finishes");
        let old = Instant::now()
            .checked_sub(flighthook::SHOT_TIMEOUT + Duration::from_secs(1))
            .expect("an old instant");
        *sink.refused.get_mut("shot-9").expect("refused") = old;
        sink.prune_shots();
        assert!(sink.refused.is_empty(), "forgotten after the shot timeout");
        drop(guard);
        assert!(sink.shots_allowed());
        sink.handle(frp(trigger(key(2))));
        let seen = drain(&mut rx);
        assert_eq!(kinds(&seen), ["trigger"]);
        assert_eq!(
            sink.bus_actor().device.and_then(|d| d.source),
            Some(Source::Virtual)
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

        /// Messages of `fake.0` until one matches `want`, within 5 s.
        fn until(&mut self, mut want: impl FnMut(&BusMsg) -> bool) -> Vec<BusMsg> {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut seen = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "timed out: {:?}", kinds(&seen));
                match self.rx.try_recv() {
                    Ok(m) if of_fake(&m) => {
                        let done = want(&m);
                        seen.push(m);
                        if done {
                            return seen;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
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
    /// `ready=false` just before it; each failure is the driver's alert. A
    /// stop closes the session and ends Disconnected.
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
        assert_eq!(lock(&script).closes, 1);
    }

    /// A stop ends a connect that is still waiting within the driver's
    /// `STOP_WAIT`.
    #[test]
    fn a_stop_ends_a_blocked_connect_within_the_stop_wait() {
        let (id, _script) = script(vec![Connect::Block]);
        let mut rig = rig();
        rig.start(section(id));
        rig.until(|m| is_status(m, ActorStatus::Starting));
        std::thread::sleep(Duration::from_millis(100));
        let at = Instant::now();
        assert_eq!(
            rig.state.stop_and_wait("fake.0"),
            crate::state::Stopped::Exited
        );
        assert!(at.elapsed() < FakeDriver::STOP_WAIT, "{:?}", at.elapsed());
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
        let again =
            rig.until(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::BusActor(_))));
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
        let at = Instant::now();
        assert_eq!(
            rig.state.stop_and_wait("fake.0"),
            crate::state::Stopped::Exited
        );
        assert!(at.elapsed() < Duration::from_secs(2));
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
    fn source_matches<D: DeviceDriver, H: Hooks<D>>() {
        assert_eq!(
            Some(H::SOURCE.provenance()),
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
        source_matches::<thumbswing::driver::Thumbswing, crate::actors::trackball::TrackballHooks>(
        );
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
            driver: flighthook::MockConfig {
                shot_interval_s: 5.0,
                arm_delay_s: 3.0,
                ball_ready_delay_s: 2.0,
                post_shot_delay_s: 4.0,
                serial: Some("7".into()),
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
            driver: flighthook::MevoConfig {
                address: Some("10.0.0.5:5100".into()),
                spin_source: flighthook::SpinSource::Auto,
                ..flighthook::MevoConfig::example()
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
            driver: flighthook::R10Config {
                range: Some(flighthook::Distance::Feet(7.0)),
                aim_offset_deg: Some(1.5),
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
            driver: flighthook::SquareConfig {
                address: Some("SquareGolf(CD01)".into()),
                club: "PW".into(),
                advanced_spin: false,
                discard_non_putting_zero_spin: false,
                dot_bottom_to_face_center_mm: std::collections::BTreeMap::from([(
                    "7I".into(),
                    21.5,
                )]),
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

        let exputt = flighthook::ExPuttSection {
            name: "ExPutt".into(),
            actor_id: Some("lm-1".into()),
            enabled: false,
            impact_lead_s: Some(0.5),
            driver: flighthook::ExPuttConfig::default(),
        };
        let full = flighthook::ExPuttSection {
            driver: flighthook::ExPuttConfig {
                address: Some("192.168.1.50".into()),
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
        d.config.cpi = 800.0;
        d.config.report_limit = 2047;
        d.config.grab = false;
        d.config.thresholds.v_start = 0.08;
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
        let serde_json::Value::Object(host) = serde_json::to_value(&trackball).expect("json")
        else {
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
}
