//! [`DriverActor`]: a card that runs on a flightrelay driver
//! ([`FrpDriver`]) as an FRP actor with the driver's roles: a launch
//! monitor device, or a software driver (a game, a simulator output).
//!
//! The driver owns the device or program: connect, session, shots, status,
//! raw frames. The actor is the host around it, one thread per card:
//!
//! - **The run loop.** It builds the driver from the card's section
//!   ([`FrpDriver::new`]; a refused config shows on the card and the
//!   actor waits for its stop), connects, steps the session until it fails
//!   or the actor is stopped, and reconnects after the delay the driver
//!   asks for ([`FrpDriver::retry_delay`], slept so that a stop cuts it
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
//!   reaches the session it opens). Every message goes through the card's
//!   [`Hooks::route`], which picks what the session sees; before each step
//!   the session gets that ([`FrpSession::bus`], in order), then the hints
//!   and actions. A new session sees what [`Hooks::session_started`] gives
//!   it first.
//! - **The bus out** ([`Sink`]): every message of a kind the driver's roles
//!   send goes on the bus under the card's actor id as soon as the driver
//!   gives it, through [`Hooks::on_frp`]. The device's status and identity
//!   make the card's `bus.actor` (the driver's roles and namespaces,
//!   `device.source` from the card's shot source, if it is one), published
//!   when it changes; while no session is up it carries no battery, each
//!   telemetry key the driver declares at its not-reported value, and a
//!   launch monitor's is blocked by `not_armed`. Telemetry and flags the
//!   driver sends during a connect go out in it at once. The card's `phase`
//!   row is the last phase the driver set while a connect or session runs,
//!   `connecting` while it starts or reconnects and `idle` once
//!   disconnected; its `error` row reads `none` once the outage it
//!   described is over, and `device_info` once the session ends. Raw frames
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
//! shot source, host-side processing of the driver's messages and
//! flighthook's policy for what its session sees.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use flightrelay::driver::{
    Activity, Backoff, ConnectCtx, ConnectHost, DeviceEvent, DeviceIdentity, DeviceStatus,
    DriverInfo, Events, FrpDriver, FrpSession, HostFlags, IDLE, Level, SessionEnd, Stop, TurnHints,
};
use flightrelay::{
    BusActor, DEFAULT_HEARTBEAT, FrpBody, FrpMessage, RawFrame, Removal, ShotFinished, ShotTrigger,
    StateKind, roles,
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

/// How often the bus is drained while the actor waits (a retry delay, or
/// for its stop).
const IDLE_SLICE: Duration = Duration::from_millis(100);

/// The card's error after the device driver panicked.
const DRIVER_FAILED: &str = "the device driver failed; press Reconnect";

/// A card row with nothing to report.
const NONE: &str = "none";

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/// What a card adds to its driver `D`.
pub trait Hooks<D: FrpDriver>: Default + Send + 'static {
    /// The card's config section: host fields with `D`'s config flattened
    /// next to them.
    type Section: DeviceSection<Driver = D::Config>;

    /// The shot source the card is, `None` for a card that is no shot
    /// source (an output). A source's class must be the source `D`
    /// declares ([`DriverInfo::source`]).
    const SOURCE: Option<ShotSource>;

    /// Host-side processing of one of the driver's messages before it goes
    /// on the bus: a shot message, a raw frame (before it reaches the shot's
    /// capture), or another kind the driver's roles send. A hook keeps the
    /// message's kind. [`Pass::Drop`] on a `shot.trigger` drops the whole
    /// shot; on anything else, that message. Default: send it as it is.
    fn on_frp(&mut self, _body: &mut FrpBody, _ctx: &mut HookCtx<'_, Self::Section>) -> Pass {
        Pass::Send
    }

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

    /// flighthook's policy for one bus message: what of it the session
    /// sees goes into [`HookCtx::to_session`]. Runs on every message the
    /// card takes off the bus, also while no session is up (what it pushes
    /// then is dropped). Default: the session sees nothing.
    fn route(&mut self, _msg: &BusMsg, _ctx: &mut HookCtx<'_, Self::Section>) {}

    /// A session connected: push what it must know first into
    /// [`HookCtx::to_session`] (the current view, the current turn). It
    /// sees this before anything else. Default: nothing.
    fn session_started(&mut self, _ctx: &mut HookCtx<'_, Self::Section>) {}

    /// The session ended, any way: drop what was held for it. Default:
    /// nothing.
    fn session_ended(&mut self, _ctx: &mut HookCtx<'_, Self::Section>) {}
}

/// What a hook does with one of the driver's messages ([`Hooks::on_frp`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// On the bus.
    Send,
    /// Not on the bus.
    Drop,
}

/// What a hook sees besides the message.
pub struct HookCtx<'a, S> {
    /// The card's section as it is in effect.
    pub section: &'a S,
    /// flighthook's state.
    pub state: &'a Arc<SystemState>,
    /// The card's own sender.
    pub sender: &'a BusSender,
    /// The card's status as the host last sent it: a hook that sends rows
    /// ([`LocalEvent::CardStatus`]) repeats it, the host owns it.
    pub status: ActorStatus,
    /// Bus messages for the session: handed to [`FrpSession::bus`] in this
    /// order before its next step.
    pub to_session: &'a mut Vec<FrpMessage>,
}

/// The card's hooks with what they keep: lent to the bus in
/// ([`Hooks::route`]) and the bus out ([`Hooks::on_frp`]), which a connect
/// runs side by side.
struct Hooked<D: FrpDriver, H: Hooks<D>> {
    hooks: H,
    /// The card's section as it is in effect.
    section: H::Section,
    /// What the hooks pushed for the session, not handed yet.
    to_session: Vec<FrpMessage>,
    _driver: PhantomData<fn() -> D>,
}

impl<D: FrpDriver, H: Hooks<D>> Hooked<D, H> {
    fn new(section: H::Section) -> Self {
        Self {
            hooks: H::default(),
            section,
            to_session: Vec::new(),
            _driver: PhantomData,
        }
    }

    /// Run `f` on the hooks with their context.
    fn with<R>(
        &mut self,
        state: &Arc<SystemState>,
        sender: &BusSender,
        status: ActorStatus,
        f: impl FnOnce(&mut H, &mut HookCtx<'_, H::Section>) -> R,
    ) -> R {
        let mut ctx = HookCtx {
            section: &self.section,
            state,
            sender,
            status,
            to_session: &mut self.to_session,
        };
        f(&mut self.hooks, &mut ctx)
    }
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

/// A card running driver `D` with the card's hooks `H`. See the
/// [module docs](self).
pub struct DriverActor<D: FrpDriver, H: Hooks<D>> {
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

impl<D: FrpDriver, H: Hooks<D>> DriverActor<D, H> {
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
    /// new driver config to [`FrpDriver::apply_config`]. A removed
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

impl<D: FrpDriver, H: Hooks<D>> Actor for DriverActor<D, H> {
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> JoinHandle<()> {
        let run = Run::<D, H> {
            hooked: RefCell::new(Hooked::new(lock(&self.section).clone())),
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
        const { assert!(D::STOP_WAIT.as_millis() <= crate::actors::MAX_STOP_WAIT.as_millis()) };
        D::STOP_WAIT
    }

    /// A device that takes one connection at a time must be let go first.
    fn replaceable_while_stopping(&self) -> bool {
        !D::info(&lock(&self.section).driver_settings()).exclusive
    }

    fn source(&self) -> Option<ShotSource> {
        H::SOURCE
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

/// What the actor's thread starts with, and the card's hooks, which it
/// lends to the bus in and out.
struct Run<D: FrpDriver, H: Hooks<D>> {
    hooked: RefCell<Hooked<D, H>>,
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

impl<D: FrpDriver, H: Hooks<D>> Run<D, H> {
    /// The actor's thread: [`run_with`](Self::run_with), and if the driver
    /// panics, `not_armed` and Disconnected with [`DRIVER_FAILED`], then
    /// the thread ends (Reconnect starts a new one).
    fn run(self, state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let actor = sender.actor_id().to_owned();
        let section = self.hooked.borrow().section.clone();
        let info = D::info(&section.driver_settings());
        // The fixed mode is forgotten when this thread ends.
        let _fixed_mode = match (info.fixed_mode, sender.frp_id()) {
            (Some(mode), Some(id)) => Some(state.set_fixed_mode(id, mode)),
            _ => None,
        };
        let flags = HostFlags::new();
        flags.set_wants_raw(true);
        flags.set_min_level(min_level());
        let mut sink = Sink::<D, H>::new(state, sender, info);
        let mut host = Host::<D, H>::new(
            receiver,
            self.stop.clone(),
            actor.clone(),
            Arc::clone(&sink.state),
            Synced::new(&sink),
        );
        sink.publish_actor();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.run_with(&mut host, &mut sink, &flags);
        }));
        if let Err(panic) = ran {
            error!("{actor}: the device driver failed: {}", panic_text(&*panic));
            let mut telemetry = sink.ended_rows();
            telemetry.insert("error".into(), DRIVER_FAILED.into());
            sink.session_ended(&mut self.hooked.borrow_mut());
            sink.status(ActorStatus::Disconnected, telemetry);
        } else if host.stopped() {
            sink.remove_actor();
        }
    }

    /// Build the driver, then connect, drive each session and wait out the
    /// retry delays until the actor is stopped or the driver gives up.
    fn run_with(&self, host: &mut Host<D, H>, sink: &mut Sink<D, H>, flags: &HostFlags) {
        let actor = sink.actor.clone();
        let settings = self.hooked.borrow().section.driver_settings();
        let mut driver = match D::new(settings) {
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
            self.take_update(sink, Some(&mut driver));
            // Device actions asked while no session ran were for no device.
            host.actions.clear();
            sink.begin_connect();
            host.begin_connect();
            let connected = {
                let mut emit = |e| sink.handle(e, &mut self.hooked.borrow_mut());
                let out = Events::new(&mut emit, flags);
                let mut connecting = Connecting {
                    host: &mut *host,
                    hooked: &self.hooked,
                };
                let mut ctx = ConnectCtx::new(&self.stop, out, &mut connecting);
                driver.connect(&mut ctx)
            };
            let (error, connected_for) = match connected {
                Err(e) => {
                    if host.stopped() {
                        break;
                    }
                    // The failed connect's error shows through the retry
                    // delay.
                    sink.connect_failed(&e.to_string());
                    attempts += 1;
                    (e, None)
                }
                Ok(mut session) => {
                    attempts = 0;
                    let since = Instant::now();
                    start_session(sink, &mut self.hooked.borrow_mut(), session.identity());
                    let ended = self.drive(&mut session, &mut driver, host, sink, flags);
                    self.hooked.borrow_mut().with(
                        &sink.state,
                        &sink.sender,
                        sink.status,
                        |h, ctx| {
                            h.session_ended(ctx);
                        },
                    );
                    {
                        let mut emit = |e| sink.handle(e, &mut self.hooked.borrow_mut());
                        session.close(&mut Events::new(&mut emit, flags));
                    }
                    match ended {
                        Err(e) if !host.stopped() => {
                            sink.session_lost(&mut self.hooked.borrow_mut());
                            (e, Some(since.elapsed()))
                        }
                        _ => break,
                    }
                }
            };
            let ended = SessionEnd::new(&error, Instant::now(), connected_for, attempts);
            let delay = {
                let mut emit = |e| sink.handle(e, &mut self.hooked.borrow_mut());
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
        // A stop that ended a connect ends its outage too.
        let mut telemetry = sink.ended_rows();
        sink.session_ended(&mut self.hooked.borrow_mut());
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
            if !host.idle(left.min(IDLE_SLICE), &self.hooked) {
                return false;
            }
            self.take_update(sink, Some(driver));
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
        while host.idle(IDLE_SLICE, &self.hooked) {
            self.take_update(sink, driver.as_deref_mut());
        }
    }

    /// Step `session` until it fails (`Err`) or the actor is stopped
    /// (`Ok`), taking live config and then, between steps, handing it what
    /// [`step_session`] hands it. A step with nothing to do
    /// ([`Activity::Idle`]) is followed by the driver contract's [`IDLE`] wait.
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
            host.drain(&mut self.hooked.borrow_mut());
            if host.stopped() {
                return Ok(());
            }
            self.take_update(sink, Some(driver));
            let activity = step_session(
                session,
                host,
                sink,
                &self.hooked,
                &mut given,
                flags,
                Instant::now(),
            )?;
            if activity == Activity::Idle {
                std::thread::sleep(IDLE);
            }
        }
    }

    /// Take a section applied live, if one waits in `updates`: the hooks'
    /// section, and the driver's config when its part changed and there is
    /// a driver.
    fn take_update(&self, sink: &mut Sink<D, H>, driver: Option<&mut D>) {
        let Some(new) = lock(&self.updates).take() else {
            return;
        };
        let mut hooked = self.hooked.borrow_mut();
        if let Some(driver) = driver {
            let settings = new.driver_settings();
            if settings != hooked.section.driver_settings() {
                driver.apply_config(&settings);
            }
        }
        hooked.section = new;
        // A renamed card is a renamed actor.
        sink.publish_actor();
    }
}

/// A session connected as `identity`: the card shows it, and the hooks'
/// [`Hooks::session_started`] fills what the session sees first (what they
/// pushed before is dropped).
fn start_session<D: FrpDriver, H: Hooks<D>>(
    sink: &mut Sink<D, H>,
    hooked: &mut Hooked<D, H>,
    identity: &DeviceIdentity,
) {
    sink.connected(identity);
    hooked.to_session.clear();
    hooked.with(&sink.state, &sink.sender, sink.status, |h, ctx| {
        h.session_started(ctx);
    });
}

/// One pass of a session after the bus was drained: what the hooks pushed
/// for it ([`FrpSession::bus`], in order), new turn hints, the bus's
/// actions, then one step at `now`.
fn step_session<D: FrpDriver, H: Hooks<D>>(
    session: &mut D::Session,
    host: &mut Host<D, H>,
    sink: &mut Sink<D, H>,
    hooked: &RefCell<Hooked<D, H>>,
    given: &mut TurnHints,
    flags: &HostFlags,
    now: Instant,
) -> Result<Activity, D::Error> {
    let to_session = std::mem::take(&mut hooked.borrow_mut().to_session);
    let turned = host.hints != *given;
    *given = host.hints;
    sink.session_mode = given.mode;
    let activity = {
        let mut emit = |e| sink.handle(e, &mut hooked.borrow_mut());
        let mut out = Events::new(&mut emit, flags);
        for msg in &to_session {
            session.bus(msg, &mut out)?;
        }
        if turned {
            session.hints(given, &mut out)?;
        }
        while let Some(action) = host.actions.pop_front() {
            session.action(&action, &mut out)?;
        }
        session.step(now, &mut out)?
    };
    sink.prune_shots();
    Ok(activity)
}

/// The driver's [`ConnectHost`] during a connect: the bus is drained (and
/// routed, with nothing kept for the session) on every tick.
struct Connecting<'a, D: FrpDriver, H: Hooks<D>> {
    host: &'a mut Host<D, H>,
    hooked: &'a RefCell<Hooked<D, H>>,
}

impl<D: FrpDriver, H: Hooks<D>> ConnectHost for Connecting<'_, D, H> {
    fn tick(&mut self) {
        let mut hooked = self.hooked.borrow_mut();
        self.host.drain(&mut hooked);
        hooked.to_session.clear();
    }

    fn hints(&self) -> &TurnHints {
        &self.host.hints
    }
}

// ---------------------------------------------------------------------------
// The bus in
// ---------------------------------------------------------------------------

/// The actor's inputs: the bus, the turn hints, queued actions and the stop
/// token.
struct Host<D: FrpDriver, H> {
    receiver: BusReceiver,
    state: Arc<SystemState>,
    /// The hints of flighthook's latest turn.
    hints: TurnHints,
    /// The hints at the start of the current connect
    /// ([`begin_connect`](Self::begin_connect)): the session it opens starts
    /// with them, and gets a later change before its first step.
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
    /// The card's status last sent, shared with the [`Sink`], for the hooks
    /// the host runs while it drains the bus.
    status: Arc<Mutex<ActorStatus>>,
}

impl Synced {
    fn new<D: FrpDriver, H: Hooks<D>>(sink: &Sink<D, H>) -> Self {
        Self {
            sender: sink.sender.clone(),
            published: Arc::clone(&sink.published),
            status: Arc::clone(&sink.shown),
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

impl<D: FrpDriver, H: Hooks<D>> Host<D, H> {
    /// Inputs over `receiver`, with the hints of the session's turn now.
    fn new(
        receiver: BusReceiver,
        stop: Stop,
        actor: String,
        state: Arc<SystemState>,
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
            state,
            synced,
            _hooks: PhantomData,
        }
    }

    /// Take what is on the bus: flighthook's turn into the hints, the card's
    /// actions into their queue, and every message through the hooks'
    /// [`Hooks::route`]. A closed bus or a shutdown flag stops the actor.
    fn drain(&mut self, hooked: &mut Hooked<D, H>) {
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
            let status = *lock(&self.synced.status);
            hooked.with(&self.state, &self.synced.sender, status, |h, ctx| {
                h.route(&msg, ctx);
            });
        }
    }

    /// Whether the actor was told to stop.
    fn stopped(&self) -> bool {
        if self.receiver.is_shutdown() {
            self.stop.stop();
        }
        self.stop.is_stopped()
    }

    /// Wait `duration`, draining the bus (no session sees what the hooks
    /// push meanwhile); `false` when a stop cut it short.
    fn idle(&mut self, duration: Duration, hooked: &RefCell<Hooked<D, H>>) -> bool {
        let end = Instant::now() + duration;
        loop {
            {
                let mut hooked = hooked.borrow_mut();
                self.drain(&mut hooked);
                hooked.to_session.clear();
            }
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

impl<D: FrpDriver, H: Hooks<D>> Host<D, H> {
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
struct Sink<D: FrpDriver, H: Hooks<D>> {
    state: Arc<SystemState>,
    sender: BusSender,
    actor: String,
    info: DriverInfo,
    /// The card's shot source, for its `bus.actor`.
    source: Option<Source>,
    /// The session's device id (for raw frame numbers), once connected.
    device: Option<String>,
    /// The device's identity, while a session is up.
    identity: Option<DeviceIdentity>,
    /// The card's status last sent.
    status: ActorStatus,
    /// `status`, shared with [`Synced`].
    shown: Arc<Mutex<ActorStatus>>,
    /// The device's status, as the card's `bus.actor` carries it.
    device_status: DeviceStatus,
    /// The `bus.actor` last published, shared with [`Host`], which
    /// republishes it in answer to `bus.sync`.
    published: Arc<Mutex<Option<BusActor>>>,
    /// The error of the last failed connect of the current outage, until a
    /// session connects or the actor stops.
    connect_error: Option<String>,
    /// The error the card's `error` row shows from the host (a failed
    /// connect, giving up, a driver panic), until it reads `none`. A driver
    /// that reports an `error` key of its own shares that row, so its
    /// `bus.actor` carries this one while it is set.
    host_error: Option<String>,
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
    /// Shots the hooks dropped at their trigger, by shot id, with when.
    dropped: HashMap<String, Instant>,
    /// Shots finished over the actor's life.
    shot_count: u32,
    /// Puts the driver's timestamps on the host clock.
    clock: UtcClock,
    /// The mode the session runs in, from the hints last handed to it.
    session_mode: Option<Mode>,
    _types: PhantomData<fn() -> (D, H)>,
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
        let Some(now_ms) = flightrelay::utc_ms() else {
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
/// number, label, then the bytes as hex, or as text for a text protocol
/// (`text`).
fn audit_line(actor: &str, frame: &RawFrame, text: bool) -> String {
    let data = if text {
        String::from_utf8_lossy(&frame.data).into_owned()
    } else {
        flighthook::hex_lower(&frame.data)
    };
    format!(
        "{actor} {} #{} {} {data}",
        frame.dir,
        frame.seq,
        frame.label.as_deref().unwrap_or(""),
    )
}

impl<D: FrpDriver, H: Hooks<D>> Sink<D, H> {
    fn new(state: Arc<SystemState>, sender: BusSender, info: DriverInfo) -> Self {
        Self {
            actor: sender.actor_id().to_owned(),
            state,
            sender,
            info,
            source: H::SOURCE.map(ShotSource::provenance),
            device: None,
            identity: None,
            status: ActorStatus::Starting,
            shown: Arc::new(Mutex::new(ActorStatus::Starting)),
            device_status: DeviceStatus::not_armed(),
            published: Arc::new(Mutex::new(None)),
            host_error: None,
            connect_error: None,
            last_alert: None,
            capture: None,
            seqs: HashMap::new(),
            shots: HashMap::new(),
            dropped: HashMap::new(),
            shot_count: 0,
            clock: UtcClock::default(),
            session_mode: None,
            _types: PhantomData,
        }
    }

    /// The card's `bus.actor` as it is now: the card's name, the driver's
    /// roles and namespaces, the device's identity and status, and the
    /// card's shot source, if it is one. `not_armed` is a launch monitor's
    /// only. While no session is up it carries
    /// [`DeviceStatus::off_session`]: the telemetry and flags the driver
    /// reported on a `not_armed` status; blockers and battery wait for the
    /// session.
    fn bus_actor(&self) -> BusActor {
        let name = crate::actors::actor_names(&self.state.system.snapshot())
            .remove(&self.actor)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.info.name.to_owned());
        let held;
        let status = if self.identity.is_some() {
            &self.device_status
        } else {
            held = self.device_status.off_session();
            &held
        };
        let mut actor =
            self.info
                .bus_actor(name, DEFAULT_HEARTBEAT, self.identity.as_ref(), status);
        if let Some(device) = actor.device.as_mut() {
            device.source = self.source;
        }
        if let (Some(error), Some(rows)) = (&self.host_error, actor.telemetry.as_mut())
            && let Some(row) = rows.get_mut("error")
        {
            row.clone_from(error);
        }
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

    /// Send status `status` with card telemetry `telemetry`. A status that
    /// is not Connected (no connect and no session runs) sets the card's
    /// `phase`: `connecting` while starting or reconnecting, `idle` once
    /// disconnected. An `error` it sends becomes the host's error (`none`
    /// ends it), republished in the `bus.actor` of a driver with an `error`
    /// key.
    fn status(&mut self, status: ActorStatus, mut telemetry: HashMap<String, String>) {
        self.status = status;
        *lock(&self.shown) = status;
        let phase = match status {
            ActorStatus::Connected => None,
            ActorStatus::Disconnected => Some("idle"),
            ActorStatus::Starting | ActorStatus::Reconnecting => Some("connecting"),
        };
        if let Some(phase) = phase {
            telemetry.insert("phase".into(), phase.into());
        }
        if let Some(error) = telemetry.get("error") {
            let error = (error != NONE).then(|| error.clone());
            if error != self.host_error {
                self.host_error = error;
                if self.info.telemetry.iter().any(|(k, _)| *k == "error") {
                    self.publish_actor();
                }
            }
        }
        self.sender
            .local(LocalEvent::CardStatus { status, telemetry });
    }

    /// A connect failed with `error`: the card's status again, with `error`
    /// as its `error` row and its `phase` back at `connecting` through the
    /// retry delay; the log gets one warning when it is the first of the
    /// outage or the text changed.
    fn connect_failed(&mut self, error: &str) {
        if self.connect_error.as_deref() != Some(error) {
            warn!("{}: cannot connect: {error}", self.actor);
            self.connect_error = Some(error.to_owned());
        }
        self.status(
            self.status,
            HashMap::from([("error".into(), error.to_owned())]),
        );
    }

    /// The outage is over (a session connected, or the actor stopped): takes
    /// the held connect error back, the card's `error` row reading `none`
    /// through `telemetry`.
    fn end_connect_error(&mut self, telemetry: &mut HashMap<String, String>) {
        if self.connect_error.take().is_some() {
            telemetry.insert("error".into(), NONE.into());
        }
    }

    /// A connect is about to start: it starts from `not_armed`, with a new
    /// capture.
    fn begin_connect(&mut self) {
        self.device_status = DeviceStatus::not_armed();
        self.capture = self.info.raw.as_ref().map(|p| {
            RawCapture::new(RawCaptureConfig {
                transport: p.transport,
                pre_roll_frames: p.pre_roll_frames,
                max_frames: p.max_frames,
                max_bulk_bytes: p.max_bulk_bytes,
            })
        });
    }

    /// A session connected as `identity`: Connected, with the card text and,
    /// for a launch monitor, the shot count, then the card's `bus.actor`
    /// with the device's identity and the status the driver sent while
    /// connecting, before any shot.
    fn connected(&mut self, identity: &DeviceIdentity) {
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
        let mut telemetry = HashMap::from([("device_info".into(), identity.label())]);
        if self.info.roles.contains(&roles::LAUNCH_MONITOR) {
            telemetry.insert("shot_count".into(), self.shot_count.to_string());
        }
        self.end_connect_error(&mut telemetry);
        self.status(ActorStatus::Connected, telemetry);
        self.publish_actor();
    }

    /// The session failed: Reconnecting, then its open shots finished and
    /// `not_armed`, so the card shows the not-reported values, not the lost
    /// connection's.
    fn session_lost(&mut self, hooked: &mut Hooked<D, H>) {
        let rows = self.ended_rows();
        self.status(ActorStatus::Reconnecting, rows);
        self.session_ended(hooked);
    }

    /// The card rows of a session that ends: `device_info` reads `none`
    /// (nothing while no session is up).
    fn ended_rows(&self) -> HashMap<String, String> {
        self.identity
            .as_ref()
            .map(|_| ("device_info".into(), NONE.into()))
            .into_iter()
            .collect()
    }

    /// No session: every shot it left open is finished, so outputs get it
    /// at once; the card's `bus.actor` is `not_armed`, with no battery and
    /// each declared telemetry key at its not-reported value.
    fn session_ended(&mut self, hooked: &mut Hooked<D, H>) {
        let mut open: Vec<(Instant, ShotKey)> = self
            .shots
            .values()
            .map(|(shot, since)| (*since, shot.key.clone()))
            .collect();
        open.sort_by_key(|(since, key)| (*since, key.shot_number));
        for (_, key) in open {
            warn!(
                "{}: finishing shot #{}: the session ended",
                self.actor, key.shot_number
            );
            self.finished(key, hooked);
        }
        self.device_status = DeviceStatus::not_armed();
        self.identity = None;
        self.shots.clear();
        self.dropped.clear();
        self.publish_actor();
    }

    /// The mode the session runs in: the driver's fixed mode, else the turn
    /// hints' mode.
    fn mode(&self) -> Option<Mode> {
        self.info.fixed_mode.or(self.session_mode)
    }

    /// Put one driver event where it goes, through the card's hooks.
    fn handle(&mut self, event: DeviceEvent, hooked: &mut Hooked<D, H>) {
        match event {
            DeviceEvent::Frp(body) => self.frp(body, hooked),
            DeviceEvent::Status(status) => {
                self.device_status = status;
                self.publish_actor();
            }
            DeviceEvent::StatusChange(change) => {
                self.device_status.apply(change);
                self.publish_actor();
            }
            DeviceEvent::Phase(phase) => {
                let telemetry = HashMap::from([("phase".into(), phase.to_owned())]);
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
                    _ => info!("{actor}: {line}"),
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

    /// One of the driver's messages: through the hooks, then a raw frame to
    /// the capture, a kind its roles send on the bus. The rest of a shot
    /// the hooks dropped at its trigger is dropped too.
    fn frp(&mut self, mut body: FrpBody, hooked: &mut Hooked<D, H>) {
        if let Some(key) = body.shot_key()
            && self.dropped.contains_key(&key.shot_id)
        {
            if matches!(body, FrpBody::ShotFinished(_)) {
                self.dropped.remove(&key.shot_id);
            }
            return;
        }
        let pass = hooked.with(&self.state, &self.sender, self.status, |h, ctx| {
            h.on_frp(&mut body, ctx)
        });
        if pass == Pass::Drop {
            if let FrpBody::ShotTrigger(trigger) = body {
                self.dropped.insert(trigger.key.shot_id, Instant::now());
            }
            return;
        }
        if let FrpBody::ShotRaw(frame) = body {
            self.raw(frame);
            return;
        }
        if !self.info.sends(&body) {
            return;
        }
        match body {
            FrpBody::ShotTrigger(trigger) => self.trigger(trigger),
            FrpBody::ShotFinished(finished) => self.finished(finished.key, hooked),
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
            _ => self.sender.frp(body),
        }
    }

    /// A shot's trigger: on the bus, which opens its raw capture window.
    fn trigger(&mut self, trigger: ShotTrigger) {
        let key = trigger.key.clone();
        let timestamp = trigger.timestamp;
        self.sender.frp(trigger);
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
    }

    /// A shot's end: what the hooks add to it, the shot's raw frames
    /// (local), then `shot.finished`, and the shot count.
    fn finished(&mut self, key: ShotKey, hooked: &mut Hooked<D, H>) {
        let shot = self.shots.remove(&key.shot_id).map_or_else(
            || ShotSoFar {
                key: key.clone(),
                launch: None,
                flight: None,
                mode: self.mode(),
            },
            |(shot, _)| shot,
        );
        let extra = hooked.with(&self.state, &self.sender, self.status, |h, ctx| {
            h.before_finished(&shot, ctx)
        });
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
    /// and dropped shots after the same time.
    fn prune_shots(&mut self) {
        self.dropped
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
        tracing::debug!(target: "audit", "{}", audit_line(&self.actor, &frame, profile.text));
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
pub(crate) mod tests;
