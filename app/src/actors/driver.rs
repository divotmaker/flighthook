//! [`DriverActor`]: a launch monitor card that runs on a flightrelay device
//! driver ([`DeviceDriver`]).
//!
//! The driver owns the device: connect, session, shots, telemetry, raw
//! frames. The actor is the host around it, one thread per card:
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
//! - **The bus in.** `set_detection_mode` and `club_info` become the
//!   latched [`DeviceCommand`] (also during a connect, through
//!   [`ConnectHost::tick`]) and the session's commands; the card's
//!   [`Hooks::action`] matches become session actions (one asked while no
//!   session runs is dropped when the next connect starts; one asked during
//!   a connect reaches the session it opens).
//! - **The bus out** ([`Sink`]): every [`DeviceEvent`] goes on the bus as
//!   flighthook's event with the session's device id. A shot's `launch` is
//!   held until its `flight_result`, its `shot_finished` or the end of the
//!   `poll` pass it arrived in, whichever comes first, so the simulation
//!   runs once, anchored on the device's own flight values (a driver sends
//!   a shot's flight result in the pass of its launch), and the ball goes
//!   out without waiting for the device to finish the shot.
//!   Telemetry is merged per actor and published in full; raw frames go to
//!   the shot's capture for the export, never onto the bus.
//! - **Live config** ([`DriverActor::reconfigure`]): host fields and the
//!   driver fields the driver applies live take effect without a restart,
//!   at the loop's next pass whatever it is doing (in a session, before a
//!   connect, during a retry delay); any other change restarts the card.
//! - **A driver panic** ends the card Disconnected with an error (and
//!   `ready=false`), never stuck Connected; Reconnect starts it again.
//!
//! What a card adds to its driver is its [`Hooks`]: its config section, its
//! shot source and any host-side processing of the device's events.

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flightrelay::driver::{
    Backoff, ConnectCtx, ConnectHost, DeviceCommand, DeviceDriver, DeviceEvent, DeviceIdentity,
    DeviceSession, DriverInfo, Events, HostFlags, Level, SessionEnd, Stop,
};
use flightrelay::{FrpEvent, RawFrame};
use tracing::{debug, error, info, warn};

use crate::actors::provenance::{Provenance, ShotSource};
use crate::actors::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::raw_capture::{RawCapture, RawCaptureConfig};
use crate::sim;
use crate::state::SystemState;
use flighthook::{
    ActorStatus, Club, DeviceSection, DriverConfig, FlightResult, FlighthookEvent,
    FlighthookMessage, Launch, RawDir, RawRecord, Severity, ShotDetectionMode, ShotKey, ShotRaw,
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

    /// Host-side processing of one of the device's FRP events before it goes
    /// on the bus. A shot's `launch` reaches it when the shot is released
    /// (with its flight result in [`HookCtx::flight`]), then the flight
    /// result. A hook keeps the event's kind. Raw frames do not reach it.
    /// Default: none.
    fn on_frp(&mut self, _event: &mut FrpEvent, _ctx: &mut HookCtx<'_, Self::Section>) {}

    /// The device action a bus event asks of the card with actor ID
    /// `actor`, if any. Default: none.
    fn action(_event: &FlighthookEvent, _actor: &str) -> Option<D::Action> {
        None
    }
}

/// What [`Hooks::on_frp`] sees besides the event.
#[derive(Debug)]
pub struct HookCtx<'a, S> {
    /// The card's section as it is in effect.
    pub section: &'a S,
    /// The detection mode the shot is simulated in: the driver's fixed mode,
    /// else the live game mode. On a `launch` a hook may change it: the
    /// shot is simulated in the mode it leaves here (e.g. the mode the
    /// session ran in at the shot's trigger).
    pub mode: Option<ShotDetectionMode>,
    /// The detection mode the session runs in: the mode of the latched
    /// command the host handed it before this `poll` pass (the bus's
    /// `set_detection_mode`s drained at the pass's start), so the same mode
    /// the driver itself sees for an event of this pass. `None` outside a
    /// session or with no mode latched.
    pub session_mode: Option<ShotDetectionMode>,
    /// For a `launch`: the device's flight result of the same shot, if it
    /// sent one.
    pub flight: Option<&'a FlightResult>,
    /// Set by a hook that put an estimated spin into the launch: the shot's
    /// `derived.spin_estimated` is set.
    pub spin_estimated: bool,
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
    /// panics, `ready=false` and Disconnected with [`DRIVER_FAILED`], then
    /// the thread ends (Reconnect starts a new one).
    fn run(self, state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let actor = sender.actor_id().to_owned();
        let info = D::info(&self.section.driver_settings());
        let flags = HostFlags::new();
        flags.set_wants_raw(true);
        flags.set_min_level(min_level());
        let mut host = Host::<D, H>::new(receiver, self.stop.clone(), actor.clone(), &state);
        let mut sink = Sink::<D, H>::new(state, sender, info, self.section.clone());
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.run_with(&mut host, &mut sink, &flags);
        }));
        if let Err(panic) = ran {
            error!("{actor}: the device driver failed: {}", panic_text(&*panic));
            sink.session_ended();
            let mut telemetry = HashMap::from([("error".into(), DRIVER_FAILED.into())]);
            sink.end_phase(&mut telemetry);
            sink.status(ActorStatus::Disconnected, telemetry);
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
                    sink.release_all();
                    sink.session_mode = None;
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
    /// (`Ok`), handing it the bus's commands and actions, live config and
    /// the round policy between polls.
    fn drive(
        &self,
        session: &mut D::Session,
        driver: &mut D,
        host: &mut Host<D, H>,
        sink: &mut Sink<D, H>,
        flags: &HostFlags,
    ) -> Result<(), D::Error> {
        // Commands that arrived while connecting: the driver read the
        // latched command at some point; this hands it the latest.
        if !host.commands.is_empty() {
            host.commands.clear();
            session.command(&host.latched)?;
        }
        loop {
            host.drain();
            if host.stopped() {
                return Ok(());
            }
            // One command per pass: what the pass's messages set, field by
            // field, so a hand and a club changed together reach the device
            // together.
            let mut pass = DeviceCommand::default();
            for command in host.commands.drain(..) {
                pass.merge(&command);
            }
            if !pass.is_empty() {
                session.command(&pass)?;
            }
            while let Some(action) = host.actions.pop_front() {
                session.action(&action)?;
            }
            take_update(&self.updates, sink, Some(driver));
            flags.set_shots_allowed(sink.shots_allowed());
            sink.session_mode = host.latched.mode.map(Into::into);
            {
                let mut emit = |e| sink.handle(e);
                session.poll(POLL_TIMEOUT, &mut Events::new(&mut emit, flags))?;
            }
            // A shot's flight result comes in the pass of its launch: what
            // is still held goes out now, not when the device finishes.
            sink.release_all();
            sink.prune_held();
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
}

// ---------------------------------------------------------------------------
// The bus in
// ---------------------------------------------------------------------------

/// The actor's inputs: the bus, the latched command, queued commands and
/// actions, and the stop token. It is the driver's [`ConnectHost`].
struct Host<D: DeviceDriver, H> {
    receiver: BusReceiver,
    latched: DeviceCommand,
    /// Commands received since the session last took them.
    commands: Vec<DeviceCommand>,
    actions: VecDeque<D::Action>,
    stop: Stop,
    actor: String,
    _hooks: PhantomData<fn() -> H>,
}

/// flightrelay's club of the same code.
fn frp_club(club: Club) -> Option<flightrelay::Club> {
    club.to_string().parse().ok()
}

impl<D: DeviceDriver, H: Hooks<D>> Host<D, H> {
    /// Inputs over `receiver`, latching the game's mode, hand and club.
    fn new(receiver: BusReceiver, stop: Stop, actor: String, state: &SystemState) -> Self {
        let game = state.game.snapshot();
        let mut latched = DeviceCommand::default();
        if let Some(mode) = game.mode {
            latched = latched.with_mode(mode.into());
        }
        if let Some(handed) = game.handed {
            latched = latched.with_handed(handed);
        }
        if let Some(club) = game.club_info.and_then(|c| frp_club(c.club)) {
            latched = latched.with_club(club);
        }
        Self {
            receiver,
            latched,
            commands: Vec::new(),
            actions: VecDeque::new(),
            stop,
            actor,
            _hooks: PhantomData,
        }
    }

    /// Take what is on the bus: detection mode, hand and club into the
    /// latched command and the queue, the card's actions into theirs. A
    /// closed bus or a shutdown flag stops the actor.
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
            let command = match &msg.event {
                FlighthookEvent::SetDetectionMode { mode, handed, club } => {
                    let mut command = DeviceCommand::default();
                    if let Some(mode) = mode {
                        command = command.with_mode((*mode).into());
                    }
                    if let Some(handed) = handed {
                        command = command.with_handed(*handed);
                    }
                    if let Some(club) = club.and_then(frp_club) {
                        command = command.with_club(club);
                    }
                    Some(command)
                }
                FlighthookEvent::ClubInfo { club_info } => {
                    frp_club(club_info.club).map(|c| DeviceCommand::default().with_club(c))
                }
                _ => None,
            };
            if let Some(command) = command.filter(|c| !c.is_empty()) {
                self.latched.merge(&command);
                self.commands.push(command);
            }
            if let Some(action) = H::action(&msg.event, &self.actor) {
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

    fn latched(&self) -> &DeviceCommand {
        &self.latched
    }
}

// ---------------------------------------------------------------------------
// The bus out
// ---------------------------------------------------------------------------

/// A shot's launch and flight result, held until the shot is released.
struct Held {
    key: ShotKey,
    launch: Option<Launch>,
    flight: Option<FlightResult>,
    since: Instant,
}

/// Where the driver's events go: the bus, the shot's raw capture and the
/// log. See the [module docs](self).
struct Sink<D: DeviceDriver, H: Hooks<D>> {
    state: Arc<SystemState>,
    sender: BusSender,
    actor: String,
    info: DriverInfo,
    provenance: Provenance,
    section: H::Section,
    hooks: H,
    /// The session's FRP device id, once connected.
    device: Option<String>,
    /// The status last sent.
    status: ActorStatus,
    /// The session's device telemetry, merged.
    telemetry: HashMap<String, String>,
    /// A connect is running: device telemetry is merged and goes out with
    /// the session's identity once it connects.
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
    /// The next raw frame number per FRP device id, across sessions.
    seqs: HashMap<String, u64>,
    /// Launches and flight results held, by shot id.
    held: HashMap<String, Held>,
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
    /// The detection mode the session runs in ([`HookCtx::session_mode`]).
    session_mode: Option<ShotDetectionMode>,
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

/// Mark the `Launch` among `events` as carrying an estimated spin.
fn mark_spin_estimated(events: &mut [FlighthookEvent]) {
    for event in events {
        if let FlighthookEvent::Launch { derived, .. } = event {
            derived.get_or_insert_with(Box::default).spin_estimated = true;
        }
    }
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
            provenance: H::SOURCE.provenance(),
            section,
            hooks: H::default(),
            device: None,
            status: ActorStatus::Starting,
            telemetry: HashMap::new(),
            connecting: false,
            phase_shown: None,
            connect_error: None,
            last_alert: None,
            capture: None,
            seqs: HashMap::new(),
            held: HashMap::new(),
            refused: HashMap::new(),
            holding: false,
            shot_count: 0,
            clock: UtcClock::default(),
            session_mode: None,
            _driver: PhantomData,
        }
    }

    fn message(&self, event: FlighthookEvent) -> FlighthookMessage {
        let msg = FlighthookMessage::new(event);
        match &self.device {
            Some(device) => msg.device(device.as_str()),
            None => msg,
        }
    }

    fn send(&self, event: FlighthookEvent) {
        self.sender.send(self.message(event));
    }

    /// Send status `status` with actor telemetry `telemetry`, and the
    /// connect error still held unless the status is Connected or carries
    /// an `error` of its own (a Starting status drops what the card held).
    fn status(&mut self, status: ActorStatus, mut telemetry: HashMap<String, String>) {
        self.status = status;
        self.hold_connect_error(&mut telemetry);
        self.send(FlighthookEvent::ActorStatus { status, telemetry });
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

    /// A connect is about to start: a new session's telemetry and capture.
    fn begin_session(&mut self) {
        self.telemetry.clear();
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
    /// then the device's identity (and `source`, which the bus sender
    /// stamps) with the telemetry the driver sent while connecting, before
    /// any shot.
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
        self.send(FlighthookEvent::DeviceTelemetry {
            manufacturer: identity.manufacturer.clone(),
            model: identity.model.clone(),
            firmware: identity.firmware.clone(),
            source: None,
            telemetry: (!self.telemetry.is_empty()).then(|| self.telemetry.clone()),
        });
    }

    /// A connect ended without a session: removes the `phase` it set, if
    /// any, through `telemetry` (an empty value removes the key).
    fn end_phase(&mut self, telemetry: &mut HashMap<String, String>) {
        if self.phase_shown.take().is_some() {
            telemetry.insert("phase".into(), String::new());
        }
    }

    /// The session failed: `ready=false`, then Reconnecting, so a card
    /// never shows a lost connection's values.
    fn session_lost(&mut self) {
        self.session_ended();
        self.status(ActorStatus::Reconnecting, HashMap::new());
    }

    /// `ready=false` with the rest of the session's telemetry.
    fn session_ended(&mut self) {
        self.telemetry.insert("ready".into(), "false".into());
        self.send(FlighthookEvent::DeviceTelemetry {
            manufacturer: None,
            model: None,
            firmware: None,
            source: None,
            telemetry: Some(self.telemetry.clone()),
        });
    }

    /// Whether the driver may start a shot now, by the round policy; logs
    /// once per round when a virtual source's shots are held back.
    fn shots_allowed(&mut self) -> bool {
        let allowed = self.state.round.allows(self.provenance);
        if allowed {
            self.holding = false;
        } else if !self.holding {
            self.holding = true;
            info!("{}: no shots during the tournament round", self.actor);
        }
        allowed
    }

    /// The detection mode a shot is simulated in: the driver's fixed mode,
    /// else the live game mode.
    fn mode(&self) -> Option<ShotDetectionMode> {
        self.info
            .fixed_mode
            .map(Into::into)
            .or_else(|| self.state.game.snapshot().mode)
    }

    /// Put one driver event where it goes.
    fn handle(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::Frp(event) => self.frp(event),
            DeviceEvent::Phase(phase) => {
                if self.connecting {
                    self.phase_shown = Some(phase);
                }
                let mut telemetry = HashMap::from([("phase".into(), phase.to_owned())]);
                self.hold_connect_error(&mut telemetry);
                self.send(FlighthookEvent::ActorStatus {
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

    /// Run the card's hook on `event` (not a launch or flight result).
    fn hook(&mut self, event: &mut FrpEvent) {
        let mut ctx = HookCtx {
            section: &self.section,
            mode: self.mode(),
            session_mode: self.session_mode,
            flight: None,
            spin_estimated: false,
        };
        self.hooks.on_frp(event, &mut ctx);
    }

    fn frp(&mut self, mut event: FrpEvent) {
        let shot = match &event {
            FrpEvent::ShotTrigger { key, .. }
            | FrpEvent::Launch { key, .. }
            | FrpEvent::FlightResult { key, .. }
            | FrpEvent::ClubPath { key, .. }
            | FrpEvent::FaceImpact { key, .. }
            | FrpEvent::ShotFinished { key } => Some(key.shot_id.clone()),
            _ => None,
        };
        if let Some(id) = &shot
            && self.refused.contains_key(id)
        {
            if matches!(event, FrpEvent::ShotFinished { .. }) {
                self.refused.remove(id);
            }
            return;
        }
        match event {
            FrpEvent::Launch { key, launch } => {
                let held = self.hold(key);
                held.launch = Some(launch);
                if held.flight.is_some() {
                    self.release(&shot.unwrap_or_default());
                }
            }
            FrpEvent::FlightResult { key, flight } => {
                let held = self.hold(key);
                held.flight = Some(flight);
                if held.launch.is_some() {
                    self.release(&shot.unwrap_or_default());
                }
            }
            FrpEvent::RawFrame(frame) => self.raw(frame),
            FrpEvent::Unknown => {}
            _ => {
                self.hook(&mut event);
                self.publish(event);
            }
        }
    }

    /// The held entry of shot `key`.
    fn hold(&mut self, key: ShotKey) -> &mut Held {
        self.held
            .entry(key.shot_id.clone())
            .or_insert_with(|| Held {
                key,
                launch: None,
                flight: None,
                since: Instant::now(),
            })
    }

    /// Put a hooked event (not a launch, flight result or raw frame) on the
    /// bus.
    fn publish(&mut self, event: FrpEvent) {
        match event {
            FrpEvent::ShotTrigger { key, timestamp } => self.trigger(key, timestamp),
            FrpEvent::ClubPath { key, club } => self.send(FlighthookEvent::ClubPath {
                key,
                club: Box::new(club),
            }),
            FrpEvent::FaceImpact { key, impact } => self.send(FlighthookEvent::FaceImpact {
                key,
                impact: Box::new(impact),
            }),
            FrpEvent::ShotFinished { key } => self.finished(key),
            FrpEvent::DeviceTelemetry {
                manufacturer,
                model,
                firmware,
                telemetry,
                ..
            } => {
                self.telemetry.extend(telemetry.unwrap_or_default());
                // While connecting, it goes out with the identity (the
                // session's `DeviceIdentity`; these identity fields are not
                // used then).
                if self.connecting {
                    return;
                }
                self.send(FlighthookEvent::DeviceTelemetry {
                    manufacturer,
                    model,
                    firmware,
                    source: None,
                    telemetry: Some(self.telemetry.clone()),
                });
            }
            FrpEvent::Alert { severity, message } => {
                let alert = (severity, message);
                if self.last_alert.as_ref() != Some(&alert) {
                    match alert.0 {
                        Severity::Warn => warn!("{}: {}", self.actor, alert.1),
                        _ => error!("{}: {}", self.actor, alert.1),
                    }
                    self.last_alert = Some(alert.clone());
                }
                let (severity, message) = alert;
                self.send(FlighthookEvent::Alert { severity, message });
            }
            FrpEvent::Launch { .. }
            | FrpEvent::FlightResult { .. }
            | FrpEvent::RawFrame(_)
            | FrpEvent::Unknown => {}
        }
    }

    /// A shot's trigger: on the bus when the round policy lets it start
    /// (the atomic check, a backstop to the driver's own `shots_allowed`),
    /// which opens its raw capture window; else the rest of the shot is
    /// dropped.
    fn trigger(&mut self, key: ShotKey, timestamp: Option<u64>) {
        let msg = self.message(FlighthookEvent::ShotTrigger {
            key: key.clone(),
            timestamp,
        });
        let sender = &self.sender;
        let sent = self
            .state
            .round
            .send_if_allowed(self.provenance, || sender.send(msg));
        if sent.is_some() {
            self.holding = false;
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

    /// A shot's end: its held launch first, then `shot_finished` with the
    /// shot's raw frames, and the shot count.
    fn finished(&mut self, key: ShotKey) {
        self.release(&key.shot_id);
        let raw = match &mut self.capture {
            Some(capture) => capture.take(),
            None => ShotRaw {
                transport: "none".into(),
                unavailable: Some(format!("{} device has no wire protocol", self.info.kind)),
                ..ShotRaw::default()
            },
        };
        self.send(FlighthookEvent::ShotFinished {
            key,
            raw: Some(Box::new(raw)),
        });
        self.shot_count += 1;
        self.send(FlighthookEvent::ActorStatus {
            status: self.status,
            telemetry: HashMap::from([("shot_count".into(), self.shot_count.to_string())]),
        });
    }

    /// Release shot `id`'s held launch and flight result: the hooks, then
    /// the simulation once (with the device's flight result), then both on
    /// the bus. A flight result without a launch goes alone.
    fn release(&mut self, id: &str) {
        let Some(Held {
            key,
            launch,
            flight,
            ..
        }) = self.held.remove(id)
        else {
            return;
        };
        let mode = self.mode();
        let mut flight_event = flight.map(|flight| FrpEvent::FlightResult {
            key: key.clone(),
            flight,
        });
        let Some(launch) = launch else {
            if let Some(mut event) = flight_event {
                self.hook(&mut event);
                if let FrpEvent::FlightResult { key, flight } = event {
                    self.send(FlighthookEvent::FlightResult {
                        key,
                        flight: Box::new(flight),
                    });
                }
            }
            return;
        };
        let mut launch_event = FrpEvent::Launch {
            key: key.clone(),
            launch,
        };
        let mut spin_estimated = false;
        let mut mode = mode;
        {
            let flight = match &flight_event {
                Some(FrpEvent::FlightResult { flight, .. }) => Some(flight),
                _ => None,
            };
            let mut ctx = HookCtx {
                section: &self.section,
                mode,
                session_mode: self.session_mode,
                flight,
                spin_estimated: false,
            };
            self.hooks.on_frp(&mut launch_event, &mut ctx);
            spin_estimated |= ctx.spin_estimated;
            // The hook may have set the mode to simulate in.
            mode = ctx.mode;
        }
        if let Some(event) = &mut flight_event {
            let mut ctx = HookCtx {
                section: &self.section,
                mode,
                session_mode: self.session_mode,
                flight: None,
                spin_estimated: false,
            };
            self.hooks.on_frp(event, &mut ctx);
            spin_estimated |= ctx.spin_estimated;
        }
        let FrpEvent::Launch { launch, .. } = launch_event else {
            warn!(
                "{}: a hook changed a launch's kind; shot #{} dropped",
                self.actor, key.shot_number
            );
            return;
        };
        let flight = match flight_event {
            Some(FrpEvent::FlightResult { flight, .. }) => Some(flight),
            _ => None,
        };
        let mut events = sim::launch_events_in_mode(&self.state, key, launch, flight, mode);
        if spin_estimated {
            mark_spin_estimated(&mut events);
        }
        for event in events {
            self.send(event);
        }
    }

    /// Release every held shot, oldest first: at the end of each `poll`
    /// pass, and when the session ends.
    fn release_all(&mut self) {
        let mut ids: Vec<(Instant, String)> = self
            .held
            .iter()
            .map(|(id, h)| (h.since, id.clone()))
            .collect();
        ids.sort();
        for (_, id) in ids {
            self.release(&id);
        }
    }

    /// Drop shots held longer than flighthook's shot timeout: by then every
    /// consumer has given up on the shot (a backstop: the end of each pass
    /// releases them). Refused shots are forgotten after the same time.
    fn prune_held(&mut self) {
        self.refused
            .retain(|_, at| at.elapsed() < flighthook::SHOT_TIMEOUT);
        let actor = &self.actor;
        self.held.retain(|_, h| {
            let keep = h.since.elapsed() < flighthook::SHOT_TIMEOUT;
            if !keep {
                debug!(
                    "{actor}: shot #{} never finished; its launch is dropped",
                    h.key.shot_number
                );
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
mod tests {
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
    use crate::state::GameStateWriter;
    use crate::state::round::RoundRequest;

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
        /// Blocks until the test sends (within 5 s).
        Wait(std::sync::mpsc::Receiver<()>),
        Fail(&'static str),
        Panic,
    }

    #[derive(Default)]
    struct Script {
        connects: VecDeque<Connect>,
        /// `live` of every config applied.
        applied: Vec<u32>,
        /// Commands the sessions took.
        commands: Vec<DeviceCommand>,
        /// The latched command at each connect.
        latched: Vec<DeviceCommand>,
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
                script.latched.push(*ctx.latched());
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
                Some(Step::Wait(go)) => {
                    // A test that fails before it sends drops the sender.
                    let _ = go.recv_timeout(Duration::from_secs(5));
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

        fn command(&mut self, cmd: &DeviceCommand) -> Result<(), FakeError> {
            lock(&self.script).commands.push(*cmd);
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
    /// without spin on a card named "estimate", has a card named "putt"
    /// simulate its launches in Putting, and marks a launch whose flight
    /// result it saw with spin axis 0.5.
    #[derive(Default)]
    struct FakeHooks;

    impl Hooks<FakeDriver> for FakeHooks {
        type Section = FakeSection;
        const SOURCE: ShotSource = ShotSource::Mevo;

        fn on_frp(&mut self, event: &mut FrpEvent, ctx: &mut HookCtx<'_, FakeSection>) {
            if let FrpEvent::Launch { launch, .. } = event {
                if ctx.flight.is_some() {
                    launch.spin_axis = Some(0.5);
                }
                if ctx.section.name == "estimate"
                    && launch.backspin_rpm.is_none()
                    && ctx.mode != Some(ShotDetectionMode::Putting)
                {
                    launch.backspin_rpm = Some(1234);
                    ctx.spin_estimated = true;
                }
                if ctx.section.name == "putt" {
                    ctx.mode = Some(ShotDetectionMode::Putting);
                }
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

    fn frp(event: FrpEvent) -> DeviceEvent {
        DeviceEvent::Frp(event)
    }

    fn frame(dir: flightrelay::RawDir, label: &str, data: &[u8]) -> DeviceEvent {
        let mut frame = RawFrame::new(dir, 0, data.to_vec());
        frame.label = Some(label.into());
        frp(FrpEvent::RawFrame(frame))
    }

    /// A connected sink of card `H` named `name`, and the bus behind it.
    fn sink<H: Hooks<FakeDriver, Section = FakeSection>>(
        name: &str,
    ) -> (
        TempDir,
        Sink<FakeDriver, H>,
        broadcast::Receiver<FlighthookMessage>,
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
        broadcast::Receiver<FlighthookMessage>,
        GameStateWriter,
    ) {
        let (dir, state, writer) = temp_state();
        let (tx, rx) = broadcast::channel(256);
        let sender = BusSender::new("fake.0".into(), tx, Arc::default())
            .with_provenance(H::SOURCE.provenance());
        let section = FakeSection {
            name: name.into(),
            driver: FakeConfig::default(),
        };
        let mut sink = Sink::new(state, sender, FakeDriver::info(&section.driver), section);
        sink.begin_session();
        (dir, sink, rx, writer)
    }

    fn drain(rx: &mut broadcast::Receiver<FlighthookMessage>) -> Vec<FlighthookMessage> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn kind(event: &FlighthookEvent) -> &'static str {
        match event {
            FlighthookEvent::ShotTrigger { .. } => "trigger",
            FlighthookEvent::Launch { .. } => "launch",
            FlighthookEvent::FlightResult { .. } => "flight",
            FlighthookEvent::ClubPath { .. } => "club",
            FlighthookEvent::FaceImpact { .. } => "impact",
            FlighthookEvent::ShotFinished { .. } => "finished",
            FlighthookEvent::DeviceTelemetry { .. } => "telemetry",
            FlighthookEvent::Alert { .. } => "alert",
            FlighthookEvent::ActorStatus { .. } => "status",
            _ => "other",
        }
    }

    fn kinds(messages: &[FlighthookMessage]) -> Vec<&'static str> {
        messages.iter().map(|m| kind(&m.event)).collect()
    }

    fn raw_of(messages: &[FlighthookMessage]) -> ShotRaw {
        messages
            .iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::ShotFinished { raw: Some(raw), .. } => Some((**raw).clone()),
                _ => None,
            })
            .expect("a finished shot")
    }

    // -- The sink ------------------------------------------------------------

    /// A session's start: Connected with the card text and the shot count,
    /// then the identity (with `source`) before anything else.
    #[test]
    fn a_session_starts_with_connected_and_the_identity() {
        let (_dir, _sink, mut rx, _w) = sink::<FakeHooks>("");
        let seen = drain(&mut rx);
        assert_eq!(kinds(&seen), ["status", "telemetry"]);
        let FlighthookEvent::ActorStatus { status, telemetry } = &seen[0].event else {
            unreachable!()
        };
        assert_eq!(*status, ActorStatus::Connected);
        assert_eq!(telemetry["device_info"], "Eagle");
        assert_eq!(telemetry["shot_count"], "0");
        assert!(matches!(
            &seen[1].event,
            FlighthookEvent::DeviceTelemetry { model: Some(m), source: Some(Provenance::Measured), telemetry: None, .. }
                if m == "Eagle"
        ));
        assert!(seen.iter().all(|m| m.device.as_deref() == Some("FAKE-1")));
    }

    /// Every kind of driver event reaches the bus as flighthook's event,
    /// with the session's device id: phase as the card's `phase`, telemetry
    /// merged and published in full, alerts, the shot lifecycle (trigger
    /// with its timestamp and provenance, launch with its simulation and
    /// the flight result, club, impact, finished with the raw frames), the
    /// shot count, and nothing for a log line.
    #[test]
    fn every_event_kind_goes_on_the_bus_with_the_device() {
        let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
        drain(&mut rx);
        let k = key(1);
        for event in [
            DeviceEvent::Phase("arming"),
            DeviceEvent::Log(Level::Info, "armed".into()),
            frp(FrpEvent::DeviceTelemetry {
                manufacturer: None,
                model: None,
                firmware: None,
                source: None,
                telemetry: Some(HashMap::from([("ready".into(), "true".into())])),
            }),
            frp(FrpEvent::DeviceTelemetry {
                manufacturer: None,
                model: None,
                firmware: None,
                source: None,
                telemetry: Some(HashMap::from([("battery_pct".into(), "80".into())])),
            }),
            frp(FrpEvent::Alert {
                severity: Severity::Warn,
                message: "low battery".into(),
            }),
            frp(FrpEvent::shot_trigger_at(k.clone(), 1_700_000_000_000)),
            frp(FrpEvent::launch(k.clone(), launch())),
            frp(FrpEvent::flight_result(k.clone(), flight())),
            frp(FrpEvent::club_path(
                k.clone(),
                ClubData {
                    club_speed: Some(Velocity::MetersPerSecond(41.0)),
                    ..ClubData::default()
                },
            )),
            frp(FrpEvent::face_impact(
                k.clone(),
                FaceImpact {
                    lateral: Some(Distance::Millimeters(3.0)),
                    ..FaceImpact::default()
                },
            )),
            frp(FrpEvent::shot_finished(k.clone())),
        ] {
            sink.handle(event);
        }
        let seen = drain(&mut rx);
        assert_eq!(
            kinds(&seen),
            [
                "status",
                "telemetry",
                "telemetry",
                "alert",
                "trigger",
                "launch",
                "flight",
                "club",
                "impact",
                "finished",
                "status"
            ]
        );
        assert!(seen.iter().all(|m| m.device.as_deref() == Some("FAKE-1")));
        assert!(
            matches!(&seen[0].event, FlighthookEvent::ActorStatus { status: ActorStatus::Connected, telemetry }
            if telemetry["phase"] == "arming" && telemetry.len() == 1)
        );
        let FlighthookEvent::DeviceTelemetry {
            telemetry: Some(merged),
            source,
            ..
        } = &seen[2].event
        else {
            unreachable!()
        };
        assert_eq!(merged["ready"], "true");
        assert_eq!(merged["battery_pct"], "80");
        assert_eq!(*source, Some(Provenance::Measured));
        assert!(
            matches!(&seen[3].event, FlighthookEvent::Alert { message, .. } if message == "low battery")
        );
        assert!(matches!(
            &seen[4].event,
            FlighthookEvent::ShotTrigger {
                timestamp: Some(1_700_000_000_000),
                ..
            }
        ));
        assert_eq!(seen[4].provenance, Some(Provenance::Measured));
        let FlighthookEvent::Launch {
            launch: l, derived, ..
        } = &seen[5].event
        else {
            unreachable!()
        };
        assert_eq!(l.backspin_rpm, Some(3000));
        assert!(derived.is_some(), "simulated, with the device's carry");
        assert!(
            matches!(&seen[6].event, FlighthookEvent::FlightResult { flight, .. } if **flight == self::flight())
        );
        assert_eq!(raw_of(&seen).transport, "fake-tcp");
        assert!(
            matches!(&seen[10].event, FlighthookEvent::ActorStatus { status: ActorStatus::Connected, telemetry }
            if telemetry["shot_count"] == "1")
        );
    }

    /// A launch waits for its flight result; the hook sees the flight result
    /// when the launch is released, and both go out together, launch first.
    /// A flight result that comes first waits for its launch.
    #[test]
    fn a_launch_waits_for_its_flight_result() {
        let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
        drain(&mut rx);
        sink.handle(frp(FrpEvent::shot_trigger(key(1))));
        sink.handle(frp(FrpEvent::launch(key(1), launch())));
        assert_eq!(kinds(&drain(&mut rx)), ["trigger"], "held");
        sink.handle(frp(FrpEvent::flight_result(key(1), flight())));
        let seen = drain(&mut rx);
        assert_eq!(kinds(&seen), ["launch", "flight"]);
        let FlighthookEvent::Launch { launch, .. } = &seen[0].event else {
            unreachable!()
        };
        assert_eq!(
            launch.spin_axis,
            Some(0.5),
            "the hook saw the flight result"
        );

        sink.handle(frp(FrpEvent::flight_result(key(2), flight())));
        assert!(drain(&mut rx).is_empty(), "held");
        sink.handle(frp(FrpEvent::launch(key(2), self::launch())));
        assert_eq!(kinds(&drain(&mut rx)), ["launch", "flight"]);
        assert!(sink.held.is_empty());
    }

    /// A launch with no flight result goes out at its shot's end, just
    /// before `shot_finished`, simulated from the launch alone.
    #[test]
    fn a_launch_without_a_flight_result_goes_at_shot_finished() {
        let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
        drain(&mut rx);
        sink.handle(frp(FrpEvent::shot_trigger(key(1))));
        sink.handle(frp(FrpEvent::launch(key(1), launch())));
        sink.handle(frp(FrpEvent::club_path(key(1), ClubData::default())));
        assert_eq!(kinds(&drain(&mut rx)), ["trigger", "club"]);
        sink.handle(frp(FrpEvent::shot_finished(key(1))));
        let seen = drain(&mut rx);
        assert_eq!(kinds(&seen), ["launch", "finished", "status"]);
        let FlighthookEvent::Launch {
            launch, derived, ..
        } = &seen[0].event
        else {
            unreachable!()
        };
        assert_eq!(launch.spin_axis, None, "no flight result to see");
        assert!(derived.as_ref().is_some_and(|d| d.carry_distance.is_some()));
    }

    /// A hook that fills in a spin marks the shot's `derived.spin_estimated`.
    #[test]
    fn a_hook_marks_an_estimated_spin() {
        let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("estimate");
        drain(&mut rx);
        let no_spin = Launch {
            backspin_rpm: None,
            sidespin_rpm: None,
            ..launch()
        };
        sink.handle(frp(FrpEvent::shot_trigger(key(1))));
        sink.handle(frp(FrpEvent::launch(key(1), no_spin)));
        sink.handle(frp(FrpEvent::shot_finished(key(1))));
        let seen = drain(&mut rx);
        let FlighthookEvent::Launch {
            launch, derived, ..
        } = &seen[1].event
        else {
            panic!("{:?}", kinds(&seen))
        };
        assert_eq!(launch.backspin_rpm, Some(1234));
        assert!(derived.as_ref().is_some_and(|d| d.spin_estimated));
    }

    /// A hook may set the mode a launch is simulated in: a ground ball the
    /// "putt" card's hook sends to Putting rolls as on a green, like one
    /// struck with the game in Putting.
    #[test]
    fn a_hook_sets_the_mode_a_launch_is_simulated_in() {
        let ground_ball = Launch {
            launch_speed: Some(Velocity::MetersPerSecond(3.0)),
            launch_elevation: Some(0.0),
            launch_azimuth: Some(0.0),
            backspin_rpm: None,
            sidespin_rpm: None,
            ..Launch::default()
        };
        let total = |name: &str, game: Option<ShotDetectionMode>| {
            let (_dir, mut sink, mut rx, writer) = sink::<FakeHooks>(name);
            if let Some(mode) = game {
                writer.set_mode(mode);
            }
            drain(&mut rx);
            sink.handle(frp(FrpEvent::launch(key(1), ground_ball.clone())));
            sink.handle(frp(FrpEvent::shot_finished(key(1))));
            drain(&mut rx)
                .iter()
                .find_map(|m| match &m.event {
                    FlighthookEvent::Launch {
                        derived: Some(d), ..
                    } => d.total_distance.map(|t| t.as_meters()),
                    _ => None,
                })
                .expect("a rolled total")
        };
        let fairway = total("", Some(ShotDetectionMode::Full));
        let green = total("", Some(ShotDetectionMode::Putting));
        assert!(green > fairway, "{green} vs {fairway}");
        assert_eq!(total("putt", Some(ShotDetectionMode::Full)), green);
    }

    /// Held launches go out when the session ends; one held past
    /// flighthook's shot timeout is dropped, since every consumer has given
    /// up on its shot by then.
    #[test]
    fn held_launches_go_out_at_the_session_end_and_expire() {
        let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
        drain(&mut rx);
        sink.handle(frp(FrpEvent::launch(key(1), launch())));
        sink.handle(frp(FrpEvent::launch(key(2), launch())));
        let old = Instant::now()
            .checked_sub(flighthook::SHOT_TIMEOUT + Duration::from_secs(1))
            .expect("an old instant");
        sink.held.get_mut("shot-2").expect("held").since = old;
        sink.prune_held();
        assert_eq!(sink.held.len(), 1);
        sink.release_all();
        let seen = drain(&mut rx);
        assert_eq!(kinds(&seen), ["launch"]);
        assert!(
            matches!(&seen[0].event, FlighthookEvent::Launch { key, .. } if key.shot_number == 1)
        );
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
        sink.handle(frp(FrpEvent::shot_trigger(key(1))));
        sink.handle(frame(Rx, "D4 DSP>APP", &[0xde, 0xad]));
        sink.handle(frame(Rx, "bulk", &[0xaa; 10]));
        sink.handle(frame(Rx, "bulk", &[0xbb; 10]));
        sink.handle(frp(FrpEvent::shot_finished(key(1))));
        let seen = drain(&mut rx);
        assert_eq!(
            kinds(&seen),
            ["trigger", "finished", "status"],
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
            frp(FrpEvent::RawFrame(frame))
        };
        for n in 0..3 {
            sink.handle(stamped(n));
        }
        sink.handle(frp(FrpEvent::ShotTrigger {
            key: key(1),
            timestamp: Some(ms),
        }));
        // The fake's profile keeps 4 frames of pre-roll and 8 a shot.
        for n in 3..8 {
            sink.handle(stamped(n));
            std::thread::sleep(Duration::from_micros(300));
        }
        sink.handle(frp(FrpEvent::shot_finished(key(1))));
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

    /// Telemetry a driver sends while it connects goes out with the
    /// identity once the session connects (so with its device id), and the
    /// connect's `phase` leaves the card then; a connect without a phase
    /// sends no `phase` key.
    #[test]
    fn a_connects_telemetry_goes_out_with_the_identity() {
        let ready = || {
            frp(FrpEvent::DeviceTelemetry {
                manufacturer: None,
                model: None,
                firmware: None,
                source: None,
                telemetry: Some(HashMap::from([("ready".into(), "false".into())])),
            })
        };
        let (_dir, mut sink, mut rx, _w) = connecting_sink::<FakeHooks>("");
        sink.handle(DeviceEvent::Phase("handshaking"));
        sink.handle(ready());
        let seen = drain(&mut rx);
        assert_eq!(kinds(&seen), ["status"], "the phase only");
        sink.connected(&DeviceIdentity::new("FAKE-1").with_model("Eagle"));
        let seen = drain(&mut rx);
        assert_eq!(kinds(&seen), ["status", "telemetry"]);
        assert!(
            matches!(&seen[0].event, FlighthookEvent::ActorStatus { status: ActorStatus::Connected, telemetry }
            if telemetry["phase"].is_empty())
        );
        assert!(
            matches!(&seen[1].event, FlighthookEvent::DeviceTelemetry { model: Some(m), telemetry: Some(t), .. }
            if m == "Eagle" && t["ready"] == "false")
        );
        assert!(seen.iter().all(|m| m.device.as_deref() == Some("FAKE-1")));
        sink.handle(ready());
        assert_eq!(kinds(&drain(&mut rx)), ["telemetry"], "connected: at once");

        sink.begin_session();
        sink.connected(&DeviceIdentity::new("FAKE-1"));
        let seen = drain(&mut rx);
        assert!(
            matches!(&seen[0].event, FlighthookEvent::ActorStatus { telemetry, .. }
            if !telemetry.contains_key("phase"))
        );
    }

    /// A discarded shot's frames leave the pre-roll.
    #[test]
    fn a_discarded_shot_clears_the_pre_roll() {
        use flightrelay::RawDir::Rx;
        let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
        drain(&mut rx);
        sink.handle(frame(Rx, "misread", &[0x11]));
        sink.handle(DeviceEvent::ShotDiscarded);
        sink.handle(frp(FrpEvent::shot_trigger(key(1))));
        sink.handle(frp(FrpEvent::shot_finished(key(1))));
        let raw = raw_of(&drain(&mut rx));
        assert!(raw.frames.is_empty(), "{raw:?}");
        assert!(raw.unavailable.is_none());
    }

    /// What a [`log_capture`] subscriber wrote.
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

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
        fn lines(&self) -> Vec<String> {
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
    fn process_log() -> &'static Buf {
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
            FrpEvent::shot_trigger(key(1)),
            FrpEvent::launch(key(1), launch()),
            FrpEvent::club_path(key(1), ClubData::default()),
            FrpEvent::shot_finished(key(1)),
        ] {
            sink.handle(frp(event));
        }
        assert!(drain(&mut rx).is_empty(), "nothing of the shot");
        assert!(sink.refused.is_empty() && sink.held.is_empty());
        sink.handle(frp(FrpEvent::shot_trigger(key(9))));
        assert_eq!(sink.refused.len(), 1, "a shot that never finishes");
        let old = Instant::now()
            .checked_sub(flighthook::SHOT_TIMEOUT + Duration::from_secs(1))
            .expect("an old instant");
        *sink.refused.get_mut("shot-9").expect("refused") = old;
        sink.prune_held();
        assert!(sink.refused.is_empty(), "forgotten after the shot timeout");
        drop(guard);
        assert!(sink.shots_allowed());
        sink.handle(frp(FrpEvent::shot_trigger(key(2))));
        let seen = drain(&mut rx);
        assert_eq!(kinds(&seen), ["trigger"]);
        assert_eq!(seen[0].provenance, Some(Provenance::Virtual));
    }

    // -- The actor -----------------------------------------------------------

    struct Rig {
        _dir: TempDir,
        state: Arc<SystemState>,
        writer: GameStateWriter,
        tx: broadcast::Sender<FlighthookMessage>,
        rx: broadcast::Receiver<FlighthookMessage>,
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
        fn start(&self, section: FakeSection) {
            crate::actors::start_actor(
                "fake.0".into(),
                Box::new(DriverActor::<FakeDriver, FakeHooks>::new(section)),
                &self.state,
                &self.tx,
            );
        }

        /// Messages of `fake.0` until one matches `want`, within 5 s.
        fn until(
            &mut self,
            mut want: impl FnMut(&FlighthookMessage) -> bool,
        ) -> Vec<FlighthookMessage> {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut seen = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "timed out: {:?}", kinds(&seen));
                match self.rx.try_recv() {
                    Ok(m) if m.actor == "fake.0" => {
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

    fn statuses(seen: &[FlighthookMessage]) -> Vec<ActorStatus> {
        seen.iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::ActorStatus { status, .. } => Some(*status),
                _ => None,
            })
            .collect()
    }

    fn is_status(m: &FlighthookMessage, want: ActorStatus) -> bool {
        matches!(&m.event, FlighthookEvent::ActorStatus { status, .. } if *status == want)
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
        let ready = |v: &str| {
            frp(FrpEvent::DeviceTelemetry {
                manufacturer: None,
                model: None,
                firmware: None,
                source: None,
                telemetry: Some(HashMap::from([("ready".into(), v.into())])),
            })
        };
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
        assert!(
            matches!(&seen[at - 1].event, FlighthookEvent::DeviceTelemetry { telemetry: Some(t), .. }
            if t["ready"] == "false"),
            "ready=false first"
        );
        let alerts: Vec<&str> = seen
            .iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::Alert { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(alerts, ["no device", "no device yet", "lost", "still gone"]);
        assert_eq!(
            rig.state.stop_and_wait("fake.0"),
            crate::state::Stopped::Exited
        );
        let end = rig.until(|m| is_status(m, Disconnected));
        assert!(
            matches!(&end[end.len() - 2].event, FlighthookEvent::DeviceTelemetry { telemetry: Some(t), .. }
            if t["ready"] == "false"),
            "close says not ready: {:?}",
            kinds(&end)
        );
        assert_eq!(lock(&script).closes, 1);
    }

    /// A launch without a flight result goes out at the end of the `poll`
    /// pass it came in, not when the device finishes the shot passes later
    /// (a launch-only Xi chip, which the device finishes after its re-arm):
    /// the ball is on the bus, simulated from the launch alone, while the
    /// session is still in its next poll. It is simulated once, and an
    /// output that sends at `shot_finished` (GSPro) gets the shot when the
    /// device finishes it.
    #[test]
    fn a_launch_goes_out_at_the_end_of_its_pass() {
        let (go, wait) = std::sync::mpsc::channel();
        let (id, _script) = script(vec![Connect::Session(vec![
            Step::Emit(vec![
                frp(FrpEvent::shot_trigger(key(1))),
                frp(FrpEvent::launch(key(1), launch())),
            ]),
            Step::Wait(wait),
            Step::Emit(vec![frp(FrpEvent::shot_finished(key(1)))]),
        ])]);
        let mut rig = rig();
        rig.start(section(id));
        let early = rig.until(|m| matches!(m.event, FlighthookEvent::Launch { .. }));
        // The session is still blocked in its second poll.
        let mut gspro = flighthook::ShotAggregator::new();
        assert!(
            early.iter().all(|m| gspro.feed(m).is_none()),
            "not sent yet"
        );
        let FlighthookEvent::Launch { derived, .. } = &early[early.len() - 1].event else {
            unreachable!()
        };
        assert!(derived.as_ref().is_some_and(|d| d.carry_distance.is_some()));
        go.send(()).expect("the session waits");
        let rest = rig.until(|m| matches!(m.event, FlighthookEvent::ShotFinished { .. }));
        assert!(
            !kinds(&rest).contains(&"launch"),
            "simulated once: {:?}",
            kinds(&rest)
        );
        let shot = rest
            .iter()
            .find_map(|m| gspro.feed(m))
            .expect("the shot, at shot_finished");
        assert!(shot.launch.is_some() && shot.derived.is_some());
        assert_eq!(
            rig.state.stop_and_wait("fake.0"),
            crate::state::Stopped::Exited
        );
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
            matches!(&m.event, FlighthookEvent::ActorStatus { telemetry, .. }
            if telemetry.get("phase").is_some_and(|p| p == "discovering"))
        });
        let phases: Vec<(ActorStatus, &str)> = seen
            .iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::ActorStatus { status, telemetry } => {
                    telemetry.get("phase").map(|p| (*status, p.as_str()))
                }
                _ => None,
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
                matches!(&m.event, FlighthookEvent::ActorStatus { telemetry, .. }
                if telemetry.get("phase").is_some_and(String::is_empty))
            })
            .expect("cleared");
        let alert = seen
            .iter()
            .position(|m| matches!(&m.event, FlighthookEvent::Alert { .. }))
            .expect("the driver's alert");
        assert!(cleared < alert, "{:?}", kinds(&seen));
        rig.state.stop_and_wait("fake.0");
    }

    /// The error row of the card after each of `seen`'s statuses, as the web
    /// layer and the UI keep it.
    fn error_rows(seen: &[FlighthookMessage]) -> Vec<Option<String>> {
        let mut card = flighthook::ActorStatusResponse::new("Fake", ActorStatus::Starting);
        seen.iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::ActorStatus { status, telemetry } => {
                    card.apply_status(*status, telemetry, 0);
                    Some(card.telemetry.get("error").cloned())
                }
                _ => None,
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
            .filter_map(|m| match &m.event {
                FlighthookEvent::ActorStatus { telemetry, .. } => {
                    telemetry.get("error").map(String::as_str)
                }
                _ => None,
            })
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
        let mut seen = rig.until(|m| matches!(&m.event, FlighthookEvent::Alert { .. }));
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
            .with_provenance(Provenance::Measured);
        let mut rx = rig.tx.subscribe();
        let thread = actor.start(Arc::clone(&rig.state), sender.clone(), sender.subscribe());
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut connects = 0;
        while connects < 2 {
            assert!(Instant::now() < deadline, "timed out");
            match rx.try_recv() {
                Ok(m) if m.actor == "logged.0" && is_status(&m, ActorStatus::Connected) => {
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
            sink.handle(frp(FrpEvent::Alert {
                severity,
                message: message.into(),
            }));
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
        let seen = rig.until(|m| {
            matches!(&m.event, FlighthookEvent::ActorStatus { telemetry, .. }
            if telemetry.contains_key("phase"))
        });
        assert_eq!(
            rig.state.stop_and_wait("fake.0"),
            crate::state::Stopped::Exited
        );
        let end = rig.until(|m| is_status(m, ActorStatus::Disconnected));
        // The card's view of every status: no phase once Disconnected.
        let mut card = flighthook::ActorStatusResponse::new("Fake", ActorStatus::Starting);
        for m in seen.iter().chain(&end) {
            if let FlighthookEvent::ActorStatus { status, telemetry } = &m.event {
                card.apply_status(*status, telemetry, 0);
            }
        }
        assert_eq!(card.status, ActorStatus::Disconnected);
        assert!(
            !card.telemetry.contains_key("phase"),
            "{:?}",
            card.telemetry
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
        assert_eq!(kinds(&seen), ["status"], "no Starting, no connect");
        assert!(
            matches!(&seen[0].event, FlighthookEvent::ActorStatus { telemetry, .. }
            if telemetry["error"] == "bad: must be false")
        );
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
        let ready = frp(FrpEvent::DeviceTelemetry {
            manufacturer: None,
            model: None,
            firmware: None,
            source: None,
            telemetry: Some(HashMap::from([("ready".into(), "true".into())])),
        });
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
        assert!(
            matches!(&seen[last].event, FlighthookEvent::ActorStatus { telemetry, .. }
            if telemetry["error"] == DRIVER_FAILED)
        );
        assert!(
            matches!(&seen[last - 1].event, FlighthookEvent::DeviceTelemetry { telemetry: Some(t), .. }
            if t["ready"] == "false"),
            "{:?}",
            kinds(&seen)
        );
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
            .with_provenance(Provenance::Measured);
        let thread = actor.start(Arc::clone(&rig.state), sender.clone(), sender.subscribe());
        rig.until(|m| matches!(&m.event, FlighthookEvent::Alert { .. }));
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
            .with_provenance(Provenance::Measured);
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

    /// The latched command starts from the game state and follows the bus,
    /// during a connect too; the session gets every command.
    #[test]
    fn commands_reach_the_latched_command_and_the_session() {
        let (id, script) = script(vec![Connect::Session(vec![])]);
        let mut rig = rig();
        rig.writer.set_mode(ShotDetectionMode::Putting);
        rig.start(section(id));
        rig.until(|m| is_status(m, ActorStatus::Connected));
        assert_eq!(
            lock(&script).latched[0].mode,
            Some(flightrelay::DetectionMode::Putting)
        );
        rig.tx
            .send(
                FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
                    mode: Some(ShotDetectionMode::Chipping),
                    handed: Some(flighthook::Handedness::Left),
                    club: Some(Club::SandWedge),
                })
                .actor("system"),
            )
            .expect("sent");
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&script).commands.is_empty() {
            assert!(Instant::now() < deadline, "no command");
            std::thread::sleep(Duration::from_millis(5));
        }
        let command = lock(&script).commands[0];
        assert_eq!(command.mode, Some(flightrelay::DetectionMode::Chipping));
        assert_eq!(command.handed, Some(flighthook::Handedness::Left));
        assert_eq!(command.club, Some(flightrelay::Club::SandWedge));
        rig.state.stop_and_wait("fake.0");
    }

    // -- Every hooked driver -------------------------------------------------

    fn provenance_of(source: Source) -> Option<Provenance> {
        match source {
            Source::Measured => Some(Provenance::Measured),
            Source::Virtual => Some(Provenance::Virtual),
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
            BTreeSet::from(["enabled".to_owned(), "name".to_owned()])
        );
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
