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
//!   and Disconnected once stopped or given up.
//! - **The bus in.** `set_detection_mode` and `club_info` become the
//!   latched [`DeviceCommand`] (also during a connect, through
//!   [`ConnectHost::tick`]) and the session's commands; the card's
//!   [`Hooks::action`] matches become session actions.
//! - **The bus out** ([`Sink`]): every [`DeviceEvent`] goes on the bus as
//!   flighthook's event with the session's device id. A shot's `launch` is
//!   held until its `flight_result` or its `shot_finished`, so the
//!   simulation runs once, anchored on the device's own flight values.
//!   Telemetry is merged per actor and published in full; raw frames go to
//!   the shot's capture for the export, never onto the bus.
//! - **Live config** ([`DriverActor::reconfigure`]): host fields and the
//!   driver fields the driver applies live take effect without a restart;
//!   any other change restarts the card.
//!
//! What a card adds to its driver is its [`Hooks`]: its config section, its
//! shot source and any host-side processing of the device's events.

use std::collections::{HashMap, HashSet, VecDeque};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flightrelay::driver::{
    Backoff, ConnectCtx, ConnectHost, DeviceCommand, DeviceDriver, DeviceEvent, DeviceIdentity,
    DeviceSession, DriverInfo, Events, HostFlags, Level, SessionEnd, Stop,
};
use flightrelay::{FrpEvent, RawFrame};
use tracing::{debug, info, warn};

use crate::actors::provenance::{Provenance, ShotSource};
use crate::actors::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::raw_capture::{self, RawCapture, RawCaptureConfig};
use crate::sim;
use crate::state::SystemState;
use flighthook::{
    ActorStatus, Club, DeviceSection, DriverConfig, FlightResult, FlighthookEvent,
    FlighthookMessage, Launch, RawDir, RawRecord, ShotDetectionMode, ShotKey, ShotRaw,
};

/// How long one session poll may take.
const POLL_TIMEOUT: Duration = Duration::from_millis(50);

/// How often the bus is drained while the actor waits (a retry delay, or
/// for its stop).
const IDLE_SLICE: Duration = Duration::from_millis(100);

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
// No card's hooks read it yet (the mock has none of its own); the tests'
// hooks do.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub struct HookCtx<'a, S> {
    /// The card's section as it is in effect.
    pub section: &'a S,
    /// The detection mode the shot is simulated in: the driver's fixed mode,
    /// else the live game mode.
    pub mode: Option<ShotDetectionMode>,
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
        let (old, next) = (current.driver(), new.driver());
        let live = old == next
            || (old.validate().is_ok()
                && next.validate().is_ok()
                && !<D::Config as DriverConfig>::restart_needed(old, next));
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
        !D::info(lock(&self.section).driver()).exclusive
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

impl<D: DeviceDriver, H: Hooks<D>> Run<D, H> {
    fn run(self, state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let actor = sender.actor_id().to_owned();
        let info = D::info(self.section.driver());
        let flags = HostFlags::new();
        flags.set_wants_raw(true);
        flags.set_min_level(min_level());
        let mut host = Host::<D, H>::new(receiver, self.stop.clone(), actor.clone(), &state);
        let mut sink = Sink::<D, H>::new(state, sender, info, self.section.clone());

        let mut driver = match D::new(sink.section.driver().clone()) {
            Ok(driver) => driver,
            Err(e) => {
                warn!("{actor}: settings not usable, not started: {e}");
                sink.status(
                    ActorStatus::Disconnected,
                    HashMap::from([("error".into(), e.to_string())]),
                );
                host.wait_for_stop();
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
            sink.begin_session();
            flags.set_shots_allowed(sink.shots_allowed());
            let connected = {
                let mut emit = |e| sink.handle(e);
                let out = Events::new(&mut emit, &flags);
                let mut ctx = ConnectCtx::new(&self.stop, out, &mut host);
                driver.connect(&mut ctx)
            };
            let (error, connected_for) = match connected {
                Err(e) => {
                    if host.stopped() {
                        break;
                    }
                    attempts += 1;
                    (e, None)
                }
                Ok(mut session) => {
                    attempts = 0;
                    let since = Instant::now();
                    sink.connected(session.identity());
                    let ended = self.drive(&mut session, &mut driver, &mut host, &mut sink, &flags);
                    sink.release_all();
                    match ended {
                        Err(e) if !host.stopped() => {
                            drop(session);
                            sink.session_lost();
                            (e, Some(since.elapsed()))
                        }
                        _ => {
                            let mut emit = |e| sink.handle(e);
                            if let Err(e) = session.close(&mut Events::new(&mut emit, &flags)) {
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
                driver.retry_delay(&ended, &mut backoff, &mut Events::new(&mut emit, &flags))
            };
            match delay {
                Some(delay) => {
                    if !host.idle(delay) {
                        break;
                    }
                }
                None => {
                    warn!("{actor}: giving up: {error}");
                    sink.status(
                        ActorStatus::Disconnected,
                        HashMap::from([("error".into(), error.to_string())]),
                    );
                    host.wait_for_stop();
                    return;
                }
            }
        }
        sink.status(ActorStatus::Disconnected, HashMap::new());
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
            for command in host.commands.drain(..) {
                session.command(&command)?;
            }
            while let Some(action) = host.actions.pop_front() {
                session.action(&action)?;
            }
            if let Some(new) = lock(&self.updates).take() {
                if new.driver() != sink.section.driver() {
                    driver.apply_config(new.driver());
                }
                sink.section = new;
            }
            flags.set_shots_allowed(sink.shots_allowed());
            {
                let mut emit = |e| sink.handle(e);
                session.poll(POLL_TIMEOUT, &mut Events::new(&mut emit, flags))?;
            }
            sink.prune_held();
        }
    }
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

    /// Drain the bus until the actor is stopped.
    fn wait_for_stop(&mut self) {
        while self.idle(Duration::from_secs(1)) {}
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
    /// The session's raw capture, for a driver with a raw profile.
    capture: Option<RawCapture>,
    /// The next raw frame number per FRP device id, across sessions.
    seqs: HashMap<String, u64>,
    /// Launches and flight results held, by shot id.
    held: HashMap<String, Held>,
    /// Shots the round policy refused at their trigger, by shot id.
    refused: HashSet<String>,
    /// A shot has been held back for the tournament round since the last
    /// one went out (one log line per round).
    holding: bool,
    /// Shots finished over the actor's life.
    shot_count: u32,
    _driver: PhantomData<fn() -> D>,
}

/// The instant of `ms`, milliseconds since the Unix epoch (UTC), on this
/// host's monotonic clock.
fn instant_of(ms: u64) -> Instant {
    let now = Instant::now();
    let Some(now_ms) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
    else {
        return now;
    };
    if ms <= now_ms {
        now.checked_sub(Duration::from_millis(now_ms - ms))
            .unwrap_or(now)
    } else {
        now + Duration::from_millis(ms - now_ms)
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
            capture: None,
            seqs: HashMap::new(),
            held: HashMap::new(),
            refused: HashSet::new(),
            holding: false,
            shot_count: 0,
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

    /// Send status `status` with actor telemetry `telemetry`.
    fn status(&mut self, status: ActorStatus, telemetry: HashMap<String, String>) {
        self.status = status;
        self.send(FlighthookEvent::ActorStatus { status, telemetry });
    }

    /// A connect is about to start: a new session's telemetry and capture.
    fn begin_session(&mut self) {
        self.telemetry.clear();
        self.capture = self.info.raw.as_ref().map(|p| {
            RawCapture::new(RawCaptureConfig {
                transport: p.transport,
                pre_roll_frames: p.pre_roll_frames,
                max_frames: p.max_frames,
                max_bulk_bytes: p.max_bulk_bytes,
                exclude: raw_capture::exclude_none,
                is_bulk: raw_capture::no_bulk,
            })
        });
    }

    /// A session connected as `identity`: Connected, with the card text and
    /// the shot count, then the device's identity (and `source`, which the
    /// bus sender stamps) before any shot.
    fn connected(&mut self, identity: &DeviceIdentity) {
        self.device = Some(identity.device.clone());
        info!("{}: connected to {}", self.actor, identity.label());
        self.status(
            ActorStatus::Connected,
            HashMap::from([
                ("device_info".into(), identity.label()),
                ("shot_count".into(), self.shot_count.to_string()),
            ]),
        );
        self.send(FlighthookEvent::DeviceTelemetry {
            manufacturer: identity.manufacturer.clone(),
            model: identity.model.clone(),
            firmware: identity.firmware.clone(),
            source: None,
            telemetry: (!self.telemetry.is_empty()).then(|| self.telemetry.clone()),
        });
    }

    /// The session failed: `ready=false`, then Reconnecting, so a card
    /// never shows a lost connection's values.
    fn session_lost(&mut self) {
        self.telemetry.insert("ready".into(), "false".into());
        self.send(FlighthookEvent::DeviceTelemetry {
            manufacturer: None,
            model: None,
            firmware: None,
            source: None,
            telemetry: Some(self.telemetry.clone()),
        });
        self.status(ActorStatus::Reconnecting, HashMap::new());
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
                self.send(FlighthookEvent::ActorStatus {
                    status: self.status,
                    telemetry: HashMap::from([("phase".into(), phase.to_owned())]),
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
            && self.refused.contains(id)
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
                self.send(FlighthookEvent::DeviceTelemetry {
                    manufacturer,
                    model,
                    firmware,
                    source: None,
                    telemetry: Some(self.telemetry.clone()),
                });
            }
            FrpEvent::Alert { severity, message } => {
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
                capture.mark_trigger_at(timestamp.map_or_else(Instant::now, instant_of));
            }
        } else {
            self.refused.insert(key.shot_id);
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
        {
            let flight = match &flight_event {
                Some(FrpEvent::FlightResult { flight, .. }) => Some(flight),
                _ => None,
            };
            let mut ctx = HookCtx {
                section: &self.section,
                mode,
                flight,
                spin_estimated: false,
            };
            self.hooks.on_frp(&mut launch_event, &mut ctx);
            spin_estimated |= ctx.spin_estimated;
        }
        if let Some(event) = &mut flight_event {
            let mut ctx = HookCtx {
                section: &self.section,
                mode,
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

    /// Release every held shot, oldest first: the session ended.
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
    /// consumer has given up on the shot.
    fn prune_held(&mut self) {
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
        let at = frame.timestamp.map_or_else(Instant::now, instant_of);
        let mut record = if profile.text {
            match String::from_utf8(frame.data) {
                Ok(text) => RawRecord::text(dir, text),
                Err(e) => RawRecord::binary(dir, e.as_bytes()),
            }
        } else {
            RawRecord::binary(dir, &frame.data)
        };
        record.label = frame.label;
        capture.push_classified(record, at, bulk);
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
    }

    /// What one session poll does; an empty list idles.
    enum Step {
        Emit(Vec<DeviceEvent>),
        Fail(&'static str),
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
            let next = {
                let mut script = lock(&self.script);
                script.latched.push(*ctx.latched());
                script.connects.pop_front()
            };
            match next {
                Some(Connect::Fail(why)) => Err(FakeError(why.into())),
                Some(Connect::Session(steps)) => Ok(FakeSession {
                    identity: DeviceIdentity::new("FAKE-1")
                        .with_manufacturer("Birdie Labs")
                        .with_model("Eagle"),
                    steps: steps.into(),
                    script: Arc::clone(&self.script),
                }),
                Some(Connect::Block) | None => loop {
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
            Some(Duration::from_millis(10))
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
    /// without spin on a card named "estimate", and marks a launch whose
    /// flight result it saw with spin axis 0.5.
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
        sink.connected(&DeviceIdentity::new("FAKE-1").with_model("Eagle"));
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

    /// Trace lines go to the `audit` target, and a raw frame's audit line
    /// carries its direction, number, label and hex.
    #[test]
    fn trace_lines_and_frames_go_to_the_audit_log() {
        let mut beef = RawFrame::new(flightrelay::RawDir::Rx, 7, vec![0xbe, 0xef]);
        beef.label = Some("D4 DSP>APP".into());
        assert_eq!(audit_line("fake.0", &beef), "fake.0 rx #7 D4 DSP>APP beef");

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
        let buf = Buf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let (_dir, mut sink, mut rx, _w) = sink::<FakeHooks>("");
        drain(&mut rx);
        tracing::subscriber::with_default(subscriber, || {
            sink.handle(DeviceEvent::Log(Level::Trace, "rx handshake 01ab".into()));
        });
        let log = String::from_utf8(lock(&buf.0).clone()).expect("text");
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

    /// The card reads Starting before the first attempt only (failed
    /// attempts don't repeat it), Connected for each session and
    /// Reconnecting once a session has been established, with `ready=false`
    /// just before it; each failure is the driver's alert. A stop closes
    /// the session and ends Disconnected.
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
            [Starting, Connected, Reconnecting, Connected]
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
        let driver = driver_keys::<S::Driver>();
        assert!(!driver.is_empty());
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
    }
}
