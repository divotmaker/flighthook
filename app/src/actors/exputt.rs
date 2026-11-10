//! ExPutt RG putting device actor.
//!
//! Runs a `threeputt` session on its own thread: finds the device on the
//! local network (or uses the configured IP), starts measuring with the
//! player's hand, and turns every putt into a putter shot on the bus.
//! Reconnects with backoff when the session ends or cannot start.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use threeputt::{Beacon, Event, Hand, PuttResult, Session, SessionOptions};
use tracing::{debug, info, warn};

use super::backoff::Backoff;
use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::sim;
use crate::state::SystemState;

use flighthook::{
    ActorStatus, ClubData, FlighthookEvent, FlighthookMessage, Handedness, Launch, RawDir,
    RawRecord, Severity, ShotDetectionMode, ShotKey, ShotRaw, Velocity,
};

/// Reconnect backoff bounds (linear: +1s per failed attempt, capped at 15s,
/// back to the minimum after a session that lasted 10s).
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(15);

/// Reconnect delay cap for the first [`QUICK_RETRIES`] attempts after a
/// session that was measuring, so a device that went quiet for a moment is
/// back quickly. Later attempts use the shared backoff.
const QUICK_RETRY: Duration = Duration::from_secs(1);
const QUICK_RETRIES: u32 = 3;

/// Session start timeouts. Starting a session cannot be interrupted, so these
/// bound how long a stop or a reconnect can wait on a device that is away.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
const MAT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest a stop waits for the actor. A session start in progress
/// cannot be cut short: the TCP connect and the device's greeting (up to
/// [`CONNECT_TIMEOUT`] each), two handshake replies ([`REPLY_TIMEOUT`] each)
/// and the mat search ([`MAT_TIMEOUT`]), then a second to close the session.
pub(crate) const STOP_WAIT: Duration = Duration::from_secs(
    2 * CONNECT_TIMEOUT.as_secs() + 2 * REPLY_TIMEOUT.as_secs() + MAT_TIMEOUT.as_secs() + 1,
);

/// Longest single wait for the device to show up on the network, between
/// checks for shutdown.
const DISCOVERY_SLICE: Duration = Duration::from_secs(1);

/// How long discovery runs without finding the device before the user is
/// told how to reach it another way.
const DISCOVERY_HINT_AFTER: Duration = Duration::from_secs(30);

const DISCOVERY_HINT: &str = "ExPutt not found on this network. If it is on another network, \
     or a firewall blocks discovery, enter its IP address on the device card.";

/// Longest wait for a device event between checks of the bus.
const EVENT_POLL: Duration = Duration::from_millis(100);

/// How long the device may stay silent. It sends a heartbeat about once a
/// second but can pause for a while, longer while it works out a putt, and its
/// app can stall for half a minute or more and then carry on, so a stalled
/// device is given time. A connection the device closes or resets ends the
/// session at once; a device that just disappears is found by this limit
/// alone. The bridge is told the device is not ready from the warning on.
const SILENCE: SilenceLimits = SilenceLimits {
    baseline: Duration::from_secs(90),
    after_strike: Duration::from_secs(120),
    warn_after: Duration::from_secs(10),
};

/// Shortest wait before reconnecting to a device that went silent, and the
/// shortest gap between attempts until a session succeeds again. A stalled
/// device is left alone rather than hit with new connections.
const STALL_RECONNECT_WAIT: Duration = Duration::from_secs(15);

/// Shortest gap between two mat searches (the session's own, counted from the
/// end of session start; a hand change; Find mat). A request inside the gap
/// waits for its end; only the latest one is run.
const RESYNC_GAP: Duration = Duration::from_secs(10);

// A search that is still running when the gap ends would swallow the next
// request: the session ignores a Find mat while the previous search waits for
// its mat.
const _: () = assert!(RESYNC_GAP.as_millis() > MAT_TIMEOUT.as_millis());

/// How long connection attempts may fail to get an answer from a device that
/// accepts the connection before the user is told.
const NOT_ANSWERING_AFTER: Duration = Duration::from_secs(120);

const NOT_ANSWERING: &str = "The ExPutt is not answering. If this lasts, turn it off and on again.";

/// A mat search in a running session that takes longer than this is logged as
/// a warning.
const SLOW_MAT_SEARCH: Duration = Duration::from_secs(3);

/// Transport label of the raw record attached to each putt.
const RAW_TRANSPORT: &str = "exputt-wifi";

/// Product name, used as the FRP model and in the device label.
const MODEL: &str = "ExPutt RG";

/// ExPutt RG putting device, reached over WiFi. The device must be in its
/// mobile mode on the same network.
pub struct ExPuttActor {
    /// Device IP. `None` finds the device on the network.
    pub address: Option<IpAddr>,
}

/// Parse a configured ExPutt address: blank or absent means "find the device
/// on the network", anything else must be an IP address.
pub(crate) fn parse_address(address: Option<&str>) -> Result<Option<IpAddr>, String> {
    match address.map(str::trim).filter(|a| !a.is_empty()) {
        None => Ok(None),
        Some(a) => a
            .parse()
            .map(Some)
            .map_err(|e| format!("invalid address '{a}': {e}")),
    }
}

impl Actor for ExPuttActor {
    /// A launch monitor flighthook reads directly.
    fn source(&self) -> Option<crate::actors::provenance::ShotSource> {
        Some(crate::actors::provenance::ShotSource::ExPutt)
    }

    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let link = Link {
            address: self.address,
            options: Box::new(session_options),
            discover: Box::new(threeputt::discover),
            discovery_hint_after: DISCOVERY_HINT_AFTER,
            silence: SILENCE,
            backoff: Backoff::new(MIN_BACKOFF, MAX_BACKOFF),
            stall_wait: STALL_RECONNECT_WAIT,
            resync_gap: RESYNC_GAP,
            not_answering_after: NOT_ANSWERING_AFTER,
        };
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(&state, link, sender, receiver))
            .expect("failed to spawn exputt thread")
    }

    /// Starting a session cannot be interrupted ([`STOP_WAIT`]).
    fn stop_wait(&self) -> Duration {
        STOP_WAIT
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let actor_id = sender.actor_id();
        let Some((_, index)) = actor_id.split_once('.') else {
            return ReconfigureOutcome::Applied;
        };

        let snap = state.system.snapshot();
        match snap.exputt.get(index) {
            // The address is used at connect time; nothing else is configured.
            Some(section) if parse_address(section.address.as_deref()) == Ok(self.address) => {
                ReconfigureOutcome::Applied
            }
            _ => ReconfigureOutcome::RestartRequired,
        }
    }
}

/// Session options for the device, with the actor's start timeouts.
fn session_options() -> SessionOptions {
    SessionOptions {
        connect_timeout: CONNECT_TIMEOUT,
        reply_timeout: REPLY_TIMEOUT,
        mat_timeout: MAT_TIMEOUT,
        ..SessionOptions::default()
    }
}

/// Reconnect delays: the shared backoff, shortened for the first
/// [`QUICK_RETRIES`] attempts after a session that was measuring, and
/// lengthened after a session lost to silence.
#[derive(Debug, Default)]
struct Retry {
    /// Attempts left whose delay is capped at [`QUICK_RETRY`].
    quick_left: u32,
    /// The last session that ended was lost to silence and none has
    /// succeeded since.
    calm: bool,
}

impl Retry {
    /// Delay before the next attempt. `session` is how long the attempt that
    /// just ended was measuring, `None` if it never got there; `silent` is
    /// whether it ended because the device went silent. After a silent
    /// session every delay is at least `stall_wait`, until a session
    /// succeeds again.
    fn delay(
        &mut self,
        backoff: &mut Backoff,
        session: Option<Duration>,
        silent: bool,
        stall_wait: Duration,
    ) -> Duration {
        if session.is_some() {
            self.quick_left = QUICK_RETRIES;
            self.calm = false;
        }
        self.calm |= silent;
        let mut delay = backoff.delay(session);
        if self.quick_left > 0 {
            self.quick_left -= 1;
            delay = delay.min(QUICK_RETRY);
        }
        if self.calm {
            delay = delay.max(stall_wait);
        }
        delay
    }
}

/// The session ended because the device sent nothing for too long.
#[derive(Debug)]
struct SilenceLost(Duration);

impl std::fmt::Display for SilenceLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no data from the device for {:.0}s",
            self.0.as_secs_f32()
        )
    }
}

impl std::error::Error for SilenceLost {}

/// Tracks an outage: connection attempts that reach a device which accepts
/// the connection but never answers. The clock starts at the first such
/// attempt and runs until a session succeeds; attempts that fail some other
/// way neither stop nor restart it, so a flapping outage is still reported.
#[derive(Debug)]
struct Unanswered {
    /// How long the outage may go on before the user is told.
    after: Duration,
    /// When the first unanswered attempt of the outage happened.
    since: Option<Instant>,
    /// The user has been told about this outage.
    told: bool,
}

impl Unanswered {
    fn new(after: Duration) -> Self {
        Self {
            after,
            since: None,
            told: false,
        }
    }

    /// An attempt got no answer at `now`. Whether the user is to be told
    /// now: once per outage.
    fn no_answer(&mut self, now: Instant) -> bool {
        let since = *self.since.get_or_insert(now);
        if !self.told && now.saturating_duration_since(since) >= self.after {
            self.told = true;
            return true;
        }
        false
    }

    /// A session succeeded: the outage is over.
    fn recovered(&mut self) {
        self.since = None;
        self.told = false;
    }
}

/// What a mat search is asked for.
#[derive(Debug, PartialEq, Eq)]
enum Resync {
    /// A new round with this hand.
    Restart(Hand),
    /// Locate the mat again, same hand.
    Find,
}

/// Spaces the mat searches of a session. A search is never started within
/// `gap` of the previous one (the session's own at its start included);
/// requests made inside the gap are folded into the latest hand and one Find
/// mat, and run once when the gap ends.
#[derive(Debug)]
struct ResyncGate {
    gap: Duration,
    /// When the last search started.
    last: Instant,
    /// The hand the device was last told.
    hand: Hand,
    /// The latest hand asked for.
    want: Hand,
    /// A Find mat is waiting.
    find: bool,
}

impl ResyncGate {
    fn new(gap: Duration, hand: Hand, now: Instant) -> Self {
        Self {
            gap,
            last: now,
            hand,
            want: hand,
            find: false,
        }
    }

    fn want_hand(&mut self, hand: Hand) {
        self.want = hand;
    }

    fn want_find(&mut self) {
        self.find = true;
    }

    /// Something is asked for that has not run.
    fn pending(&self) -> bool {
        self.find || self.want != self.hand
    }

    /// Time left until a pending search may run, `None` if none is pending or
    /// it may run now.
    fn hold(&self, now: Instant) -> Option<Duration> {
        if !self.pending() {
            return None;
        }
        self.gap
            .checked_sub(now.saturating_duration_since(self.last))
            .filter(|left| !left.is_zero())
    }

    /// The search to start at `now`, if one is pending and the gap is over.
    fn due(&mut self, now: Instant) -> Option<Resync> {
        if !self.pending() || self.hold(now).is_some() {
            return None;
        }
        self.last = now;
        self.find = false;
        if self.want == self.hand {
            Some(Resync::Find)
        } else {
            self.hand = self.want;
            Some(Resync::Restart(self.hand))
        }
    }
}

/// Counts since the actor started, kept across reconnects.
#[derive(Debug, Default)]
struct Totals {
    shots: u32,
    connects: u32,
    resyncs: u32,
}

/// `ready` as published: not ready while a requested mat search is waiting or
/// the device has been silent past the warning threshold.
fn published_ready(device_ready: &'static str, gate: &ResyncGate, silent: bool) -> &'static str {
    if silent || gate.pending() {
        "false"
    } else {
        device_ready
    }
}

/// How the actor reaches the device.
struct Link {
    /// Device IP, or `None` to find the device on the network.
    address: Option<IpAddr>,
    /// Session options for the next attempt. The hand is set from the game
    /// state for each session.
    options: Box<dyn FnMut() -> SessionOptions + Send>,
    /// Waits up to the given time for the device to announce itself.
    discover: Box<dyn FnMut(Duration) -> threeputt::Result<Beacon> + Send>,
    /// Discovery time without a device before the hint alert.
    discovery_hint_after: Duration,
    silence: SilenceLimits,
    backoff: Backoff,
    /// Shortest reconnect delay after a session lost to silence.
    stall_wait: Duration,
    /// Shortest gap between mat searches.
    resync_gap: Duration,
    /// Failed handshakes this long before the user is told.
    not_answering_after: Duration,
}

/// How long a measuring session may go without a device event.
#[derive(Debug, Clone, Copy)]
struct SilenceLimits {
    /// Longest silence before the session is treated as lost.
    baseline: Duration,
    /// Longest silence after a strike, until the putt's result or the device
    /// is ready again.
    after_strike: Duration,
    /// Silence after which one warning is logged.
    warn_after: Duration,
}

/// What a quiet spell amounts to so far.
#[derive(Debug, PartialEq, Eq)]
enum Quiet {
    /// Within the limits.
    Fine,
    /// Past the warning threshold for the first time in this spell.
    Warn(Duration),
    /// Past the limit: the session is lost.
    Lost(Duration),
}

/// Tracks silence from the device against [`SilenceLimits`].
#[derive(Debug)]
struct Watchdog {
    limits: SilenceLimits,
    last: Instant,
    /// A strike was seen and its result has not arrived yet.
    awaiting_result: bool,
    /// The current quiet spell has been warned about.
    warned: bool,
}

impl Watchdog {
    fn new(limits: SilenceLimits, now: Instant) -> Self {
        Self {
            limits,
            last: now,
            awaiting_result: false,
            warned: false,
        }
    }

    /// A device event arrived at `now`. Returns the length of the quiet spell
    /// it ends when that spell was warned about.
    fn event(&mut self, event: &Event, now: Instant) -> Option<Duration> {
        match event {
            Event::BallStruck => self.awaiting_result = true,
            Event::PuttResult(_) | Event::PuttReady => self.awaiting_result = false,
            _ => {}
        }
        let spell = now.saturating_duration_since(self.last);
        self.last = now;
        std::mem::take(&mut self.warned).then_some(spell)
    }

    /// The silence allowed right now.
    fn limit(&self) -> Duration {
        if self.awaiting_result {
            self.limits.after_strike
        } else {
            self.limits.baseline
        }
    }

    fn check(&mut self, now: Instant) -> Quiet {
        let quiet = now.saturating_duration_since(self.last);
        if quiet >= self.limit() {
            Quiet::Lost(quiet)
        } else if quiet >= self.limits.warn_after && !self.warned {
            self.warned = true;
            Quiet::Warn(quiet)
        } else {
            Quiet::Fine
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn emit_status(sender: &BusSender, status: ActorStatus, telemetry: HashMap<String, String>) {
    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status,
        telemetry,
    }));
}

/// Actor status with the session phase (`discovering`, `connecting`,
/// `measuring`).
fn emit_phase(sender: &BusSender, status: ActorStatus, phase: &str) {
    emit_status(
        sender,
        status,
        HashMap::from([("phase".into(), phase.into())]),
    );
}

fn emit_alert(sender: &BusSender, severity: Severity, message: impl Into<String>) {
    sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
        severity,
        message: message.into(),
    }));
}

/// Set `key` to `value`; whether that changed anything.
fn set_value(telemetry: &mut HashMap<String, String>, key: &str, value: &str) -> bool {
    if telemetry.get(key).map(String::as_str) == Some(value) {
        return false;
    }
    telemetry.insert(key.into(), value.into());
    true
}

fn send_telemetry(sender: &BusSender, device: &str, telemetry: &HashMap<String, String>) {
    sender.send(
        FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
            manufacturer: None,
            model: None,
            firmware: None,
            source: None,
            telemetry: Some(telemetry.clone()),
        })
        .device(device),
    );
}

/// The putting hand for the player's handedness (right when unknown).
fn hand_of(handed: Option<Handedness>) -> Hand {
    if handed == Some(Handedness::Left) {
        Hand::Left
    } else {
        Hand::Right
    }
}

// ---------------------------------------------------------------------------
// Protocol type -> bus type conversion helpers
// ---------------------------------------------------------------------------

/// Ball data of a putt: ball speed and launch direction, rolled along the
/// ground by the simulation. The device measures no vertical launch and no
/// spin, so neither is reported.
fn ball_from_putt(putt: &PuttResult) -> Launch {
    Launch {
        launch_speed: Some(Velocity::MetersPerSecond(putt.ball_speed_mps)),
        launch_elevation: None,
        // Both are positive to the right of the target line.
        launch_azimuth: Some(putt.launch_direction_deg),
        ..Launch::default()
    }
}

/// The putt's 16 values as a text raw record, in the device's order.
fn raw_from_putt(putt: &PuttResult) -> ShotRaw {
    let values: Vec<String> = putt.raw.iter().map(f64::to_string).collect();
    ShotRaw {
        transport: RAW_TRANSPORT.into(),
        frames: vec![RawRecord {
            channel: Some("tcp".into()),
            ..RawRecord::text(RawDir::Rx, values.join(","))
        }],
        ..ShotRaw::default()
    }
}

/// Putter data of a putt: path and face angle, each when the device tracked
/// it. `None` when it tracked neither (the putter was not seen); the ball
/// data still makes a shot.
fn club_from_putt(putt: &PuttResult) -> Option<ClubData> {
    if putt.path_deg.is_none() && putt.face_deg.is_none() {
        return None;
    }
    Some(ClubData {
        // Both are positive to the right of the target line.
        path: putt.path_deg,
        face_angle: putt.face_deg,
        ..ClubData::default()
    })
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

fn run(state: &SystemState, mut link: Link, sender: BusSender, mut receiver: BusReceiver) {
    let mut ever_connected = false;
    let mut device: Option<String> = None;
    // Counted across reconnects, so shot numbers keep increasing.
    let mut totals = Totals::default();
    let mut retry = Retry::default();
    let mut unanswered = Unanswered::new(link.not_answering_after);

    loop {
        if receiver.poll().is_err() {
            break;
        }

        let mut connected_at = None;
        match connect_and_run(
            state,
            &mut link,
            &sender,
            &mut receiver,
            &mut connected_at,
            &mut device,
            &mut totals,
        ) {
            Ok(()) => break,
            Err(e) => {
                let session = connected_at.map(|at: Instant| at.elapsed());
                ever_connected |= session.is_some();
                warn!("session error: {e}");
                // Not ready before the status changes, so a bridge never
                // sees a lost device as ready.
                if let Some(dev) = device.as_deref() {
                    send_telemetry(
                        &sender,
                        dev,
                        &HashMap::from([("ready".into(), "false".into())]),
                    );
                }

                // A device that accepts the connection and never answers is
                // not hammered (see `Retry`); the user is told once, and the
                // per-attempt alerts stop until a session succeeds.
                let mut tell_not_answering = false;
                if session.is_some() {
                    unanswered.recovered();
                } else if matches!(
                    e.downcast_ref::<threeputt::Error>(),
                    Some(threeputt::Error::Timeout(_))
                ) {
                    tell_not_answering = unanswered.no_answer(Instant::now());
                }
                if !unanswered.told {
                    emit_alert(&sender, Severity::Warn, format!("ExPutt error: {e}"));
                }
                if tell_not_answering {
                    warn!(
                        "no answer from the device to {} s of connection attempts",
                        link.not_answering_after.as_secs()
                    );
                    emit_alert(&sender, Severity::Warn, NOT_ANSWERING);
                }

                let backoff_status = if ever_connected {
                    ActorStatus::Reconnecting
                } else {
                    ActorStatus::Starting
                };
                emit_phase(&sender, backoff_status, "");

                let verb = if ever_connected {
                    "reconnecting"
                } else {
                    "retrying"
                };
                let silent = e.downcast_ref::<SilenceLost>().is_some();
                let delay = retry.delay(&mut link.backoff, session, silent, link.stall_wait);
                info!("{verb} in {}s", delay.as_secs_f32());

                let deadline = Instant::now() + delay;
                while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                    if receiver.poll().is_err() {
                        emit_status(&sender, ActorStatus::Disconnected, HashMap::new());
                        return;
                    }
                    std::thread::sleep(left.min(Duration::from_millis(250)));
                }
            }
        }
    }

    emit_status(&sender, ActorStatus::Disconnected, HashMap::new());
}

/// Wait for the device to announce itself on the network, checking for
/// shutdown between waits. `Ok(None)` on shutdown. Raises one hint alert
/// when the device has not shown up after [`Link::discovery_hint_after`].
fn discover(
    link: &mut Link,
    sender: &BusSender,
    receiver: &mut BusReceiver,
) -> threeputt::Result<Option<Beacon>> {
    let started = Instant::now();
    let mut hinted = false;
    loop {
        // Drain the bus so it does not lag while the device is away.
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => return Ok(None),
                Ok(None) => break,
                Ok(Some(_)) => {}
            }
        }
        match (link.discover)(DISCOVERY_SLICE) {
            Ok(beacon) => return Ok(Some(beacon)),
            Err(threeputt::Error::Timeout(_)) => {}
            Err(e) => return Err(e),
        }
        if !hinted && started.elapsed() >= link.discovery_hint_after {
            hinted = true;
            info!(
                "no ExPutt found on the network after {}s",
                started.elapsed().as_secs()
            );
            // Warn is the lowest alert severity.
            emit_alert(sender, Severity::Warn, DISCOVERY_HINT);
        }
    }
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn connect_and_run(
    state: &SystemState,
    link: &mut Link,
    sender: &BusSender,
    receiver: &mut BusReceiver,
    connected_at: &mut Option<Instant>,
    device: &mut Option<String>,
    totals: &mut Totals,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (ip, firmware) = if let Some(ip) = link.address {
        (ip, None)
    } else {
        info!("looking for the ExPutt on the network...");
        emit_phase(sender, ActorStatus::Starting, "discovering");
        let Some(beacon) = discover(link, sender, receiver)? else {
            return Ok(());
        };
        info!(
            "found the ExPutt at {} (software {})",
            beacon.ip,
            beacon.app_version.as_deref().unwrap_or("unknown"),
        );
        (beacon.ip, beacon.app_version)
    };

    emit_phase(sender, ActorStatus::Starting, "connecting");
    let hand = hand_of(state.game.snapshot().handed);
    info!("connecting to the ExPutt at {ip} ({hand:?} hand)...");
    let options = SessionOptions {
        hand,
        ..(link.options)()
    };
    // The session's own mat search runs inside `start`.
    let resync_started = Instant::now();
    let session = Session::start(ip, &options)?;
    *connected_at = Some(Instant::now());
    totals.connects += 1;
    totals.resyncs += 1;

    let name = format!("{MODEL} ({ip})");
    info!("connected to the {name}, measuring");
    *device = Some(name.clone());

    emit_status(
        sender,
        ActorStatus::Connected,
        HashMap::from([
            ("device_info".into(), name.clone()),
            ("phase".into(), "measuring".into()),
            ("shot_count".into(), totals.shots.to_string()),
        ]),
    );
    let mut telemetry: HashMap<String, String> = HashMap::from([("ready".into(), "false".into())]);
    sender.send(
        FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
            manufacturer: None,
            model: Some(MODEL.into()),
            firmware,
            source: None,
            telemetry: Some(telemetry.clone()),
        })
        .device(&name),
    );

    let mut watchdog = Watchdog::new(link.silence, Instant::now());
    let mut gate = ResyncGate::new(link.resync_gap, hand, Instant::now());
    // The last `ready` the device itself reported; what is published also
    // depends on a waiting mat search.
    let mut device_ready: &'static str = "false";
    // The device has been silent past the warning threshold; not ready as
    // published until it sends something again.
    let mut silent = false;
    // The mat search in progress, and when it started. The session's own
    // runs inside `Session::start`, so its time includes connect and
    // handshake.
    let mut mat_search: Option<(Instant, bool)> = Some((resync_started, true));
    loop {
        // ==============================================================
        // Phase 1: Drain bus commands
        // ==============================================================
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => {
                    if gate.pending() {
                        debug!("mat search request dropped at shutdown");
                    }
                    info!("closing the session");
                    if let Err(e) = session.close() {
                        debug!("session close: {e}");
                    }
                    return Ok(());
                }
                Ok(None) => break,
                Ok(Some(msg)) => match msg.event {
                    // Follow the player's hand: the system actor's command, whatever set it.
                    // The detection mode does not apply: the device only
                    // measures putts. The new round finds the mat first, so
                    // the device is not ready until it reports ready again,
                    // as after a Find mat.
                    FlighthookEvent::SetDetectionMode {
                        handed: Some(handed),
                        ..
                    } => {
                        gate.want_hand(hand_of(Some(handed)));
                        log_hold(&gate);
                    }
                    // Not ready until the device is ready again; the device
                    // reports finding the mat and the mat found on the way.
                    FlighthookEvent::FindMat { target } if target == sender.actor_id() => {
                        gate.want_find();
                        log_hold(&gate);
                    }
                    _ => {}
                },
            }
        }

        // A mat search runs at most once per gap, with the latest hand.
        let now = Instant::now();
        if let Some(search) = gate.due(now) {
            match search {
                Resync::Restart(new_hand) => {
                    info!("hand -> {new_hand:?}, finding the mat");
                    session.restart_match(new_hand)?;
                }
                Resync::Find => {
                    info!("finding the mat");
                    session.find_mat()?;
                }
            }
            totals.resyncs += 1;
            mat_search = Some((now, false));
            device_ready = "false";
        }
        if set_value(
            &mut telemetry,
            "ready",
            published_ready(device_ready, &gate, silent),
        ) {
            send_telemetry(sender, &name, &telemetry);
        }

        // ==============================================================
        // Phase 2: Device events
        // ==============================================================
        let event = match session.events().recv_timeout(EVENT_POLL) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => {
                match watchdog.check(Instant::now()) {
                    Quiet::Fine => {}
                    Quiet::Warn(quiet) => {
                        warn!(
                            "no data from the device for {:.1}s, waiting up to {:.1}s \
                             ({} connects, {} mat searches since start)",
                            quiet.as_secs_f32(),
                            watchdog.limit().as_secs_f32(),
                            totals.connects,
                            totals.resyncs
                        );
                        silent = true;
                        if set_value(
                            &mut telemetry,
                            "ready",
                            published_ready(device_ready, &gate, silent),
                        ) {
                            send_telemetry(sender, &name, &telemetry);
                        }
                    }
                    Quiet::Lost(quiet) => {
                        warn!(
                            "giving up on the device after {:.0}s without data \
                             ({} connects, {} mat searches since start)",
                            quiet.as_secs_f32(),
                            totals.connects,
                            totals.resyncs
                        );
                        return Err(SilenceLost(quiet).into());
                    }
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                info!("device closed the connection");
                return Err("connection to the device lost".into());
            }
        };
        if let Some(spell) = watchdog.event(&event, Instant::now()) {
            silent = false;
            info!(
                "device sent data again after {:.1}s of silence",
                spell.as_secs_f32()
            );
        }

        // (ready, device_state) the event sets; `None` leaves a value alone.
        let (ready, device_state) = match event {
            Event::FindingMat => (None, Some("finding mat")),
            Event::MatFound => {
                if let Some((started, at_start)) = mat_search.take() {
                    let took = started.elapsed();
                    if at_start {
                        info!(
                            "mat found {:.1}s into session start (connect and handshake included)",
                            took.as_secs_f32()
                        );
                    } else {
                        info!("mat found after {:.1}s", took.as_secs_f32());
                        if took > SLOW_MAT_SEARCH {
                            warn!(
                                "mat search took {:.1}s, over {}s",
                                took.as_secs_f32(),
                                SLOW_MAT_SEARCH.as_secs()
                            );
                        }
                    }
                }
                (None, Some("mat found"))
            }
            Event::PuttReady => {
                info!("ready for a putt");
                (Some("true"), Some("ready"))
            }
            Event::BallStruck => {
                info!("ball struck");
                (Some("false"), Some("ball struck"))
            }
            Event::Heartbeat => (None, None),
            Event::PuttResult(putt) => {
                emit_putt(state, sender, &name, &putt, &mut totals.shots);
                (None, None)
            }
            Event::Unknown { packet_id, comment } => {
                debug!("unrecognized event {packet_id}: {comment:?}");
                (None, None)
            }
            other => {
                debug!("event {other:?}");
                (None, None)
            }
        };
        let mut changed = false;
        if let Some(ready) = ready {
            device_ready = ready;
        }
        changed |= set_value(
            &mut telemetry,
            "ready",
            published_ready(device_ready, &gate, silent),
        );
        if let Some(label) = device_state {
            changed |= set_value(&mut telemetry, "device_state", label);
        }
        if changed {
            debug!("device telemetry: {telemetry:?}");
            send_telemetry(sender, &name, &telemetry);
        }
    }
}

/// Logs how long a requested mat search is held back, if it is.
fn log_hold(gate: &ResyncGate) {
    if let Some(left) = gate.hold(Instant::now()) {
        info!(
            "mat search held for {:.1}s, after the last one",
            left.as_secs_f32()
        );
    }
}

/// Emit one putt as a putter shot: trigger, ball (rolled on the green by the
/// simulation), putter data when tracked, finish with the putt's values as
/// its raw record.
fn emit_putt(
    state: &SystemState,
    sender: &BusSender,
    device: &str,
    putt: &PuttResult,
    shot_counter: &mut u32,
) {
    *shot_counter += 1;
    let key = ShotKey {
        shot_id: uuid::Uuid::new_v4().to_string(),
        shot_number: *shot_counter,
    };
    let ball = ball_from_putt(putt);
    let club = club_from_putt(putt);
    info!(
        "putt #{}: {:.2}mph HLA={:+.2} path={} face={} values={:?}",
        key.shot_number,
        Velocity::MetersPerSecond(putt.ball_speed_mps).as_mph(),
        putt.launch_direction_deg,
        putt.path_deg.map_or("-".into(), |v| format!("{v:+.2}")),
        putt.face_deg.map_or("-".into(), |v| format!("{v:+.2}")),
        putt.raw,
    );

    emit_status(
        sender,
        ActorStatus::Connected,
        HashMap::from([("shot_count".into(), shot_counter.to_string())]),
    );
    sender.send(FlighthookMessage::new(FlighthookEvent::shot_trigger(key.clone())).device(device));
    for event in sim::launch_events_in_mode(
        state,
        key.clone(),
        ball,
        None,
        Some(ShotDetectionMode::Putting),
    ) {
        sender.send(FlighthookMessage::new(event).device(device));
    }
    if let Some(club) = club {
        sender.send(
            FlighthookMessage::new(FlighthookEvent::ClubPath {
                key: key.clone(),
                club: Box::new(club),
            })
            .device(device),
        );
    } else {
        debug!("  no putter data (putter not tracked)");
    }
    sender.send(
        FlighthookMessage::new(FlighthookEvent::ShotFinished {
            key,
            raw: Some(Box::new(raw_from_putt(putt))),
        })
        .device(device),
    );
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
    use std::thread::JoinHandle;

    use flighthook::{ShotAccumulator, ShotData};
    use threeputt::Request;
    use threeputt::testing::{
        BALL_STRUCK, MockConfig, MockDevice, PUTT_READY, PUTT_RESULT_BALL_ONLY, PUTT_RESULT_FULL,
        PUTT_RESULT_PULLED, device_event,
    };
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::gspro::mapper::map_shot;
    use crate::actors::gspro::{GsProRouting, readiness_after};
    use crate::state::GameStateWriter;

    const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    /// Longest any test waits for one thing.
    const WAIT: Duration = Duration::from_secs(8);

    /// [`SILENCE`] scaled down, one second standing for 30.
    const TEST_SILENCE: SilenceLimits = SilenceLimits {
        baseline: Duration::from_millis(1000),
        after_strike: Duration::from_millis(2000),
        warn_after: Duration::from_millis(333),
    };

    type Discover = Box<dyn FnMut(Duration) -> threeputt::Result<Beacon> + Send>;

    /// A discovery that never finds the device, each slice taking `slice`.
    fn never_found(slice: Duration) -> Discover {
        Box::new(move |_| {
            std::thread::sleep(slice);
            Err(threeputt::Error::Timeout("test"))
        })
    }

    /// The putt the device reports for `comment`, read by the driver.
    fn putt(comment: &str) -> PuttResult {
        match Event::parse(&device_event(comment)) {
            Event::PuttResult(p) => p,
            other => panic!("not a putt: {other:?}"),
        }
    }

    /// The actor running against a mock device on `port`.
    struct Harness {
        state: Arc<SystemState>,
        /// Game state's writer, until a system actor takes it.
        writer: Option<GameStateWriter>,
        tx: broadcast::Sender<FlighthookMessage>,
        rx: broadcast::Receiver<FlighthookMessage>,
        shutdown: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
        dir: std::path::PathBuf,
        /// Set before `start`: discovery replaces the configured address.
        discover: Option<Discover>,
        discovery_hint_after: Duration,
        stall_wait: Duration,
        resync_gap: Duration,
        not_answering_after: Duration,
        reply_timeout: Duration,
    }

    impl Harness {
        /// State and bus, actor not started yet.
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flighthook-exputt-{}",
                crate::state::config::generate_id()
            ));
            let (state, writer) = SystemState::new(dir.join("config.toml"));
            let (tx, rx) = broadcast::channel(1024);
            Self {
                state: Arc::new(state),
                writer: Some(writer),
                tx,
                rx,
                shutdown: Arc::new(AtomicBool::new(false)),
                thread: None,
                dir,
                discover: None,
                discovery_hint_after: Duration::from_secs(3600),
                stall_wait: Duration::ZERO,
                resync_gap: Duration::ZERO,
                not_answering_after: Duration::from_secs(3600),
                reply_timeout: Duration::from_secs(2),
            }
        }

        /// Start a system actor on the bus, holding game state's writer: the
        /// hand is then set with `hand_info`, as every source sets it.
        fn start_system_actor(&mut self) {
            let writer = self.writer.take().expect("writer");
            crate::actors::system::start_for_test(
                writer,
                &self.state,
                &self.tx,
                self.shutdown.clone(),
            );
        }

        /// Start the actor; each connection attempt goes to the port held
        /// in `port` at that moment.
        fn start(&mut self, port: Arc<AtomicU16>) {
            let sender = BusSender::new("exputt.0".into(), self.tx.clone(), self.shutdown.clone());
            let receiver = sender.subscribe();
            let discover = self.discover.take();
            let reply_timeout = self.reply_timeout;
            let link = Link {
                address: if discover.is_some() {
                    None
                } else {
                    Some(LOCALHOST)
                },
                options: Box::new(move || SessionOptions {
                    port: port.load(Ordering::SeqCst),
                    connect_timeout: reply_timeout.min(Duration::from_secs(1)),
                    reply_timeout,
                    mat_timeout: Duration::from_secs(2),
                    keepalive_interval: Duration::from_millis(100),
                    ..SessionOptions::default()
                }),
                discover: discover.unwrap_or_else(|| never_found(Duration::from_millis(10))),
                discovery_hint_after: self.discovery_hint_after,
                silence: TEST_SILENCE,
                backoff: Backoff::new(Duration::from_millis(50), Duration::from_millis(100)),
                stall_wait: self.stall_wait,
                resync_gap: self.resync_gap,
                not_answering_after: self.not_answering_after,
            };
            let state = Arc::clone(&self.state);
            self.thread = Some(std::thread::spawn(move || {
                run(&state, link, sender, receiver);
            }));
        }

        fn start_on(&mut self, mock: &MockDevice) {
            self.start(Arc::new(AtomicU16::new(mock.port())));
        }

        /// Messages from the actor until `want` matches one, returning them
        /// all (the match last).
        fn until(
            &mut self,
            what: &str,
            want: impl Fn(&FlighthookMessage) -> bool,
        ) -> Vec<FlighthookMessage> {
            let deadline = Instant::now() + WAIT;
            let mut seen = Vec::new();
            while Instant::now() < deadline {
                match self.rx.try_recv() {
                    Ok(msg) if msg.actor == "exputt.0" => {
                        let done = want(&msg);
                        seen.push(msg);
                        if done {
                            return seen;
                        }
                    }
                    Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            panic!(
                "{what} not seen; got {:?}",
                seen.iter().map(|m| &m.event).collect::<Vec<_>>()
            );
        }

        fn until_status(&mut self, status: ActorStatus) -> Vec<FlighthookMessage> {
            self.until(&format!("{status:?}"), |m| {
                matches!(&m.event, FlighthookEvent::ActorStatus { status: s, .. } if *s == status)
            })
        }

        /// The events of the next shot, trigger to finish.
        fn next_shot(&mut self) -> Vec<FlighthookEvent> {
            let mut events: Vec<FlighthookEvent> = self
                .until("shot_finished", |m| {
                    matches!(m.event, FlighthookEvent::ShotFinished { .. })
                })
                .into_iter()
                .map(|m| m.event)
                .collect();
            let start = events
                .iter()
                .rposition(|e| matches!(e, FlighthookEvent::ShotTrigger { .. }))
                .expect("shot_trigger");
            events.drain(..start);
            events
        }

        /// Every message from the actor waiting on the bus, without waiting.
        fn drain(&mut self) -> Vec<FlighthookMessage> {
            let mut seen = Vec::new();
            loop {
                match self.rx.try_recv() {
                    Ok(msg) if msg.actor == "exputt.0" => seen.push(msg),
                    Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => return seen,
                }
            }
        }

        /// Messages until a device telemetry with `ready` = `ready` and, when
        /// given, `device_state` = `state`.
        fn until_ready(&mut self, ready: bool, state: Option<&str>) -> Vec<FlighthookMessage> {
            let ready = ready.to_string();
            self.until(&format!("ready={ready} {state:?}"), |m| {
                matches!(&m.event, FlighthookEvent::DeviceTelemetry { telemetry: Some(t), .. }
                    if t.get("ready") == Some(&ready)
                        && state.is_none_or(|s| t.get("device_state").map(String::as_str) == Some(s)))
            })
        }

        fn stop(&mut self) {
            self.shutdown.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                t.join().expect("actor thread");
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.stop();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The shot the events add up to, as the bridges accumulate it.
    fn accumulate(events: &[FlighthookEvent]) -> ShotData {
        let FlighthookEvent::ShotTrigger { key, .. } = &events[0] else {
            panic!("starts with a trigger: {events:?}");
        };
        let mut acc = ShotAccumulator::new("exputt.0".into(), key.clone());
        for e in events {
            match e {
                FlighthookEvent::Launch {
                    key: k,
                    launch,
                    derived,
                } => {
                    assert_eq!(k, key);
                    acc.set_launch(*launch.clone(), derived.as_deref().cloned());
                }
                FlighthookEvent::FlightResult { .. } => panic!("a putt has no flight result"),
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

    fn sent(mock: &MockDevice, request: Request) -> bool {
        let json = request.to_json();
        mock.wait_until(WAIT, |packets| packets.iter().any(|p| p.json == json))
    }

    #[test]
    fn connects_measures_and_closes_the_session_on_shutdown() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_on(&mock);

        let seen = h.until_status(ActorStatus::Connected);
        let phases: Vec<&str> = seen
            .iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::ActorStatus { telemetry, .. } => {
                    telemetry.get("phase").map(String::as_str)
                }
                _ => None,
            })
            .collect();
        // A configured address skips discovery.
        assert_eq!(phases, ["connecting", "measuring"]);
        let FlighthookEvent::ActorStatus { telemetry, .. } = &seen.last().unwrap().event else {
            unreachable!()
        };
        assert_eq!(telemetry["device_info"], "ExPutt RG (127.0.0.1)");

        // Ready once the device says so; the identity goes with the first
        // telemetry.
        let seen = h.until("ready", |m| {
            matches!(&m.event, FlighthookEvent::DeviceTelemetry { telemetry: Some(t), .. }
                if t.get("ready").map(String::as_str) == Some("true"))
        });
        let first = seen
            .iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::DeviceTelemetry { model, .. } => Some((model, &m.device)),
                _ => None,
            })
            .expect("device telemetry");
        assert_eq!(first.0.as_deref(), Some("ExPutt RG"));
        assert_eq!(first.1.as_deref(), Some("ExPutt RG (127.0.0.1)"));
        assert!(sent(&mock, Request::SetHand(Hand::Right)));
        assert!(sent(&mock, Request::MatchBegin));

        h.stop();
        assert!(sent(&mock, Request::MatchEnded), "measuring ended");
        assert!(mock.wait_closed(WAIT), "connection closed");
        h.until_status(ActorStatus::Disconnected);
    }

    #[test]
    fn a_putt_becomes_a_putter_shot_with_speed_direction_path_and_face() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_on(&mock);
        h.until_status(ActorStatus::Connected);

        mock.push_event(PUTT_RESULT_FULL);
        let events = h.next_shot();
        let expected = putt(PUTT_RESULT_FULL);
        let shot = accumulate(&events);

        let ball = shot.launch.as_ref().expect("launch");
        assert_eq!(
            ball.launch_speed,
            Some(Velocity::MetersPerSecond(expected.ball_speed_mps))
        );
        assert!((ball.launch_speed.unwrap().as_mps() - 2.50).abs() < 0.01);
        assert_eq!(ball.launch_azimuth, Some(expected.launch_direction_deg));
        assert!(ball.launch_azimuth.unwrap() > 1.4, "R1.4 is positive");
        // Neither a launch angle nor spin is measured, so neither is sent,
        // and nothing is marked computed.
        assert_eq!(ball.launch_elevation, None);
        assert_eq!(ball.backspin_rpm, None);
        assert_eq!(ball.sidespin_rpm, None);
        assert_eq!(ball.total_spin_rpm, None);
        let derived = shot.derived.as_ref().expect("derived");
        assert!(derived.computed.is_empty(), "{:?}", derived.computed);

        let club = shot.club.as_ref().expect("club data");
        assert_eq!(club.path, expected.path_deg);
        assert_eq!(club.face_angle, expected.face_deg);
        assert!(club.path.unwrap() > 0.0 && club.face_angle.unwrap() > 0.0);
        assert_eq!(club.club_speed, None, "the putter speed is not used");

        // Rolled along the ground by the simulation.
        let derived = shot.derived.as_ref().expect("simulated roll");
        assert!(derived.total_distance.is_some_and(|d| d.as_meters() > 0.0));

        // What GSPro receives: a putt with ball speed and HLA, no launch
        // angle or spin.
        let gspro = map_shot(&shot);
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

    #[test]
    fn a_pulled_putt_is_negative_left() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_on(&mock);
        h.until_status(ActorStatus::Connected);

        mock.push_event(PUTT_RESULT_PULLED);
        let shot = accumulate(&h.next_shot());
        let ball = shot.launch.as_ref().expect("launch");
        assert!(ball.launch_azimuth.unwrap() < -10.0, "L10.3 is negative");
        let club = shot.club.as_ref().expect("club");
        assert!(club.path.unwrap() < -5.0 && club.face_angle.unwrap() < -10.0);
        // Left of the target lands left.
        let rest = shot
            .derived
            .as_ref()
            .and_then(|d| d.total_offline)
            .expect("offline");
        assert!(rest.as_meters() < 0.0, "{rest:?}");
    }

    #[test]
    fn a_putt_without_putter_data_is_a_ball_only_shot() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_on(&mock);
        h.until_status(ActorStatus::Connected);

        mock.push_event(PUTT_RESULT_BALL_ONLY);
        let events = h.next_shot();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FlighthookEvent::ClubPath { .. })),
            "{events:?}"
        );
        let expected = putt(PUTT_RESULT_BALL_ONLY);
        let shot = accumulate(&events);
        assert!(shot.club.is_none());
        let ball = shot.launch.as_ref().expect("launch");
        assert_eq!(
            ball.launch_speed,
            Some(Velocity::MetersPerSecond(expected.ball_speed_mps))
        );
        assert_eq!(ball.launch_azimuth, Some(expected.launch_direction_deg));

        let gspro = map_shot(&shot);
        assert!(gspro.ball_data.speed > 0.0);
        assert_eq!(gspro.ball_data.hla, expected.launch_direction_deg);
    }

    #[test]
    fn a_dropped_device_is_reconnected() {
        let first = MockDevice::spawn(MockConfig::default()).expect("mock");
        let port = Arc::new(AtomicU16::new(first.port()));
        let mut h = Harness::new();
        h.start(Arc::clone(&port));
        h.until_status(ActorStatus::Connected);
        mock_putt(&first, &mut h, 1);

        // The device goes away; the next attempt finds it again (on a new
        // mock, so on a new port).
        let second = MockDevice::spawn(MockConfig::default()).expect("mock");
        port.store(second.port(), Ordering::SeqCst);
        first.disconnect();
        h.until_status(ActorStatus::Reconnecting);
        h.until_status(ActorStatus::Connected);
        assert!(sent(&second, Request::MatchBegin));
        // Shot numbers keep counting.
        mock_putt(&second, &mut h, 2);
    }

    fn mock_putt(mock: &MockDevice, h: &mut Harness, number: u32) {
        mock.push_event(PUTT_RESULT_FULL);
        let events = h.next_shot();
        let FlighthookEvent::ShotTrigger { key, .. } = &events[0] else {
            unreachable!()
        };
        assert_eq!(key.shot_number, number);
    }

    #[test]
    fn a_silent_device_is_reconnected() {
        // No heartbeat: the session goes quiet.
        let mock = MockDevice::spawn(MockConfig {
            heartbeat: None,
            ..MockConfig::default()
        })
        .expect("mock");
        let second = MockDevice::spawn(MockConfig::default()).expect("mock");
        let port = Arc::new(AtomicU16::new(mock.port()));
        let mut h = Harness::new();
        h.start(Arc::clone(&port));
        h.until_status(ActorStatus::Connected);
        port.store(second.port(), Ordering::SeqCst);
        // Waits out the silence limit.
        let deadline = Instant::now() + TEST_SILENCE.baseline + WAIT;
        while Instant::now() < deadline {
            if let Ok(FlighthookMessage {
                event:
                    FlighthookEvent::ActorStatus {
                        status: ActorStatus::Reconnecting,
                        ..
                    },
                ..
            }) = h.rx.try_recv()
            {
                h.until_status(ActorStatus::Connected);
                assert!(sent(&second, Request::MatchBegin));
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("not reconnected");
    }

    /// A hand change starts a new round, which finds the mat first: not
    /// ready through "finding mat" and "mat found" until the device is ready
    /// again, as after a Find mat, and no reconnect.
    #[test]
    fn the_hand_follows_the_player() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.writer
            .as_ref()
            .expect("writer")
            .set_handed(Handedness::Left);
        h.start_on(&mock);
        h.until_ready(true, Some("ready"));
        assert!(sent(&mock, Request::SetHand(Hand::Left)), "starts left");
        let before = mock.received().len();

        // The devices are told the player is right-handed.
        let _ = h.tx.send(
            FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
                mode: None,
                handed: Some(Handedness::Right),
                club: None,
            })
            .actor("system"),
        );
        let mut seen = h.until_ready(false, Some("ready"));
        seen.extend(h.until_ready(true, Some("ready")));
        assert_eq!(
            device_states(&seen),
            ["ready", "finding mat", "mat found", "ready"]
        );
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

    /// A hand set in the UI (`hand_info` from `web`) reaches the ExPutt
    /// through the system actor's command: a new round with the new hand,
    /// and a repeat of the same hand starts no other.
    #[test]
    fn a_hand_set_in_the_ui_reaches_the_device() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_system_actor();
        h.start_on(&mock);
        h.until_ready(true, Some("ready"));
        assert!(sent(&mock, Request::SetHand(Hand::Right)), "starts right");
        let before = mock.received().len();

        let left = || {
            FlighthookMessage::new(FlighthookEvent::HandInfo {
                hand_info: flighthook::HandInfo {
                    handed: Handedness::Left,
                },
            })
            .actor("web")
        };
        let _ = h.tx.send(left());
        h.until_ready(false, Some("ready"));
        h.until_ready(true, Some("ready"));
        assert!(
            mock.wait_until(WAIT, |p| round_sent(&p[before..], Hand::Left)),
            "{:?}",
            mock.received()
        );
        let after = mock.received().len();
        // A repeat of the same hand, then the other hand as a sentinel: the
        // bus is in order, so the round for the sentinel is the only one
        // since `after` unless the repeat started one of its own.
        let _ = h.tx.send(left());
        let _ = h.tx.send(
            FlighthookMessage::new(FlighthookEvent::HandInfo {
                hand_info: flighthook::HandInfo {
                    handed: Handedness::Right,
                },
            })
            .actor("web"),
        );
        assert!(
            mock.wait_until(WAIT, |p| round_sent(&p[after..], Hand::Right)),
            "{:?}",
            mock.received()
        );
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

    #[test]
    fn a_device_that_is_not_there_is_retried() {
        // Nothing listens on this port.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr").port()
        };
        let port = Arc::new(AtomicU16::new(port));
        let mut h = Harness::new();
        h.start(Arc::clone(&port));
        let seen = h.until("alert", |m| {
            matches!(m.event, FlighthookEvent::Alert { .. })
        });
        assert!(seen.iter().all(|m| !matches!(
            m.event,
            FlighthookEvent::ActorStatus {
                status: ActorStatus::Connected,
                ..
            }
        )));
        // Comes up once the device does.
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        port.store(mock.port(), Ordering::SeqCst);
        h.until_status(ActorStatus::Connected);
    }

    #[test]
    fn addresses_are_an_ip_or_blank() {
        assert_eq!(parse_address(None), Ok(None));
        assert_eq!(parse_address(Some("  ")), Ok(None));
        assert_eq!(
            parse_address(Some(" 192.168.1.50 ")),
            Ok(Some("192.168.1.50".parse().unwrap()))
        );
        assert!(parse_address(Some("192.168.1.50:8000")).is_err());
        assert!(parse_address(Some("exputt.local")).is_err());
    }

    #[test]
    fn an_address_change_restarts_the_actor() {
        let h = Harness::new();
        let (tx, _rx) = broadcast::channel(16);
        let sender = BusSender::new("exputt.0".into(), tx, Arc::new(AtomicBool::new(false)));
        let actor = ExPuttActor { address: None };
        let set = |address: Option<&str>| {
            h.state.system.update(|c| {
                c.exputt.insert(
                    "0".into(),
                    flighthook::ExPuttSection {
                        name: "ExPutt".into(),
                        enabled: true,
                        address: address.map(str::to_string),
                        impact_lead_s: None,
                    },
                );
            });
        };
        let outcome = || actor.reconfigure(&h.state, &sender);

        set(None);
        assert!(matches!(outcome(), ReconfigureOutcome::Applied));
        set(Some(""));
        assert!(matches!(outcome(), ReconfigureOutcome::Applied));
        set(Some("10.0.0.7"));
        assert!(matches!(outcome(), ReconfigureOutcome::RestartRequired));
        h.state.system.update(|c| {
            c.exputt.clear();
        });
        assert!(matches!(outcome(), ReconfigureOutcome::RestartRequired));
    }

    #[test]
    fn hand_defaults_to_right() {
        assert_eq!(hand_of(None), Hand::Right);
        assert_eq!(hand_of(Some(Handedness::Right)), Hand::Right);
        assert_eq!(hand_of(Some(Handedness::Left)), Hand::Left);
    }

    fn is_status(m: &FlighthookMessage, status: ActorStatus) -> bool {
        matches!(&m.event, FlighthookEvent::ActorStatus { status: s, .. } if *s == status)
    }

    fn alerts(seen: &[FlighthookMessage]) -> Vec<String> {
        seen.iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::Alert { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    /// Readiness an Open Connect client routing putts to the ExPutt reports in
    /// Putting mode after `seen`.
    fn gspro_ready(seen: &[FlighthookMessage]) -> bool {
        let routing = GsProRouting {
            putting_monitor: Some("exputt.0".into()),
            ..GsProRouting::default()
        };
        readiness_after(routing, ShotDetectionMode::Putting, seen.iter().cloned())
    }

    fn quiet_mock() -> MockDevice {
        MockDevice::spawn(MockConfig {
            heartbeat: None,
            ..MockConfig::default()
        })
        .expect("mock")
    }

    #[test]
    fn a_putt_result_after_a_quiet_spell_keeps_the_session() {
        // No heartbeat; the result comes half way to the silence limit
        // (15 s of 30).
        let mock = quiet_mock();
        let mut h = Harness::new();
        h.start_on(&mock);
        let mut seen = h.until_status(ActorStatus::Connected);
        std::thread::sleep(TEST_SILENCE.baseline / 2);
        mock.push_event(PUTT_RESULT_FULL);
        seen.extend(h.until("shot_finished", |m| {
            matches!(m.event, FlighthookEvent::ShotFinished { .. })
        }));
        seen.extend(h.drain());
        assert!(
            !seen.iter().any(|m| is_status(m, ActorStatus::Reconnecting)),
            "no reconnect"
        );
        assert_eq!(alerts(&seen), Vec::<String>::new());
    }

    #[test]
    fn a_slow_result_after_a_strike_keeps_the_session() {
        // No heartbeat; the result comes past the baseline limit but within
        // the limit after a strike (40 s of 60).
        let mock = quiet_mock();
        let mut h = Harness::new();
        h.start_on(&mock);
        let mut seen = h.until_status(ActorStatus::Connected);
        mock.push_event(BALL_STRUCK);
        seen.extend(h.until_ready(false, Some("ball struck")));
        std::thread::sleep(TEST_SILENCE.after_strike * 2 / 3);
        mock.push_event(PUTT_RESULT_FULL);
        seen.extend(h.until("shot_finished", |m| {
            matches!(m.event, FlighthookEvent::ShotFinished { .. })
        }));
        seen.extend(h.drain());
        assert!(
            !seen.iter().any(|m| is_status(m, ActorStatus::Reconnecting)),
            "no reconnect"
        );
        assert_eq!(alerts(&seen), Vec::<String>::new());
    }

    #[test]
    fn silence_is_allowed_90s_and_120s_after_a_strike() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut w = Watchdog::new(SILENCE, t0);
        assert_eq!(w.check(at(9)), Quiet::Fine);
        // One warning per quiet spell.
        assert_eq!(w.check(at(10)), Quiet::Warn(Duration::from_secs(10)));
        assert_eq!(w.check(at(15)), Quiet::Fine);
        assert_eq!(w.check(at(89)), Quiet::Fine);
        assert_eq!(w.check(at(90)), Quiet::Lost(Duration::from_secs(90)));

        // A heartbeat ends the spell; the next one warns again.
        assert_eq!(
            w.event(&Event::Heartbeat, at(85)),
            Some(Duration::from_secs(85))
        );
        assert_eq!(w.event(&Event::Heartbeat, at(86)), None);
        assert_eq!(w.check(at(96)), Quiet::Warn(Duration::from_secs(10)));

        // After a strike, 120 s until the result or ready.
        let _ = w.event(&Event::BallStruck, at(100));
        assert_eq!(w.check(at(140)), Quiet::Warn(Duration::from_secs(40)));
        assert_eq!(w.check(at(219)), Quiet::Fine);
        assert_eq!(w.check(at(220)), Quiet::Lost(Duration::from_secs(120)));

        // Heartbeats in between keep the longer limit until the result.
        let mut w = Watchdog::new(SILENCE, t0);
        let _ = w.event(&Event::BallStruck, at(0));
        let _ = w.event(&Event::Heartbeat, at(20));
        assert_eq!(w.check(at(75)), Quiet::Warn(Duration::from_secs(55)));
        assert_eq!(w.check(at(139)), Quiet::Fine);
        assert_eq!(w.check(at(140)), Quiet::Lost(Duration::from_secs(120)));
        let _ = w.event(&Event::PuttResult(putt(PUTT_RESULT_FULL)), at(141));
        assert_eq!(w.check(at(231)), Quiet::Lost(Duration::from_secs(90)));

        // Ready again also ends the longer limit.
        let _ = w.event(&Event::BallStruck, at(240));
        let _ = w.event(&Event::PuttReady, at(241));
        assert_eq!(w.check(at(331)), Quiet::Lost(Duration::from_secs(90)));
    }

    #[test]
    fn session_start_times_out_quickly() {
        let o = session_options();
        assert_eq!(o.connect_timeout, Duration::from_secs(3));
        assert_eq!(o.reply_timeout, Duration::from_secs(3));
        assert_eq!(o.mat_timeout, Duration::from_secs(5));
    }

    #[test]
    fn reconnects_quickly_after_a_measuring_session() {
        let secs = Duration::from_secs;
        let mut backoff = Backoff::new(MIN_BACKOFF, MAX_BACKOFF);
        let mut retry = Retry::default();
        let mut delay = |session, silent| retry.delay(&mut backoff, session, silent, secs(15));
        // Never measured: the shared backoff.
        let delays: Vec<_> = (0..3).map(|_| delay(None, false)).collect();
        assert_eq!(delays, [secs(1), secs(2), secs(3)]);

        // A session that measured briefly and dropped: three attempts at
        // 1 s, then the shared backoff where it had got to.
        let mut delays = vec![delay(Some(secs(2)), false)];
        delays.extend((0..3).map(|_| delay(None, false)));
        assert_eq!(delays, [secs(1), secs(1), secs(1), secs(7)]);
    }

    #[test]
    fn reconnects_calmly_after_silence_until_a_session_succeeds() {
        let secs = Duration::from_secs;
        let mut backoff = Backoff::new(MIN_BACKOFF, MAX_BACKOFF);
        let mut retry = Retry::default();
        let mut delay = |session, silent| retry.delay(&mut backoff, session, silent, secs(15));
        // A measuring session lost to silence: 15 s before the first
        // attempt, and at least 15 s between the failed ones after it.
        let mut delays = vec![delay(Some(secs(300)), true)];
        delays.extend((0..4).map(|_| delay(None, false)));
        assert_eq!(delays, [secs(15); 5]);

        // A session that succeeded and was lost another way: fast again.
        assert_eq!(delay(Some(secs(60)), false), secs(1));
        assert_eq!(delay(None, false), secs(1));

        // A silent session that never measured also waits.
        assert_eq!(delay(None, true), secs(15));
    }

    #[test]
    fn mat_searches_are_spaced_and_coalesced() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut gate = ResyncGate::new(Duration::from_secs(10), Hand::Right, t0);
        assert_eq!(gate.due(at(1)), None, "nothing asked for");

        // rh, rh, lh, lh inside the gap: one restart to lh when it ends.
        gate.want_hand(Hand::Right);
        gate.want_hand(Hand::Right);
        assert!(!gate.pending(), "the hand in effect is no request");
        gate.want_hand(Hand::Left);
        gate.want_hand(Hand::Left);
        assert!(gate.pending());
        assert_eq!(gate.hold(at(2)), Some(Duration::from_secs(8)));
        assert_eq!(gate.due(at(9)), None);
        assert_eq!(gate.due(at(10)), Some(Resync::Restart(Hand::Left)));
        assert_eq!(gate.due(at(11)), None, "run once");

        // A flip back to the hand in effect cancels the request.
        gate.want_hand(Hand::Right);
        gate.want_hand(Hand::Left);
        assert!(!gate.pending());
        assert_eq!(gate.due(at(25)), None);

        // Find mat requests inside the gap are one search; a hand change
        // alongside it makes the search that hand's round.
        gate.want_find();
        gate.want_find();
        assert_eq!(gate.due(at(19)), None, "9 s after the last search");
        assert_eq!(gate.due(at(20)), Some(Resync::Find));
        assert_eq!(gate.due(at(21)), None);
        gate.want_find();
        gate.want_hand(Hand::Right);
        assert_eq!(gate.due(at(29)), None);
        assert_eq!(gate.due(at(30)), Some(Resync::Restart(Hand::Right)));
        assert_eq!(gate.due(at(31)), None);

        // Outside the gap a request runs at once.
        gate.want_find();
        assert_eq!(gate.due(at(60)), Some(Resync::Find));
    }

    #[test]
    fn the_user_is_told_once_per_outage_that_the_device_is_not_answering() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut u = Unanswered::new(Duration::from_secs(120));
        assert!(!u.no_answer(at(0)));
        assert!(!u.no_answer(at(60)));
        assert!(!u.no_answer(at(119)));
        assert!(u.no_answer(at(120)));
        assert!(!u.no_answer(at(135)), "not again");
        assert!(!u.no_answer(at(600)));

        // A session that succeeds ends the outage; the next one counts from
        // its own first failure.
        u.recovered();
        assert!(!u.no_answer(at(700)));
        assert!(!u.no_answer(at(819)));
        assert!(u.no_answer(at(820)));

        // Attempts that fail some other way in between (refused, not found)
        // are not reported here and do not move the clock: it runs from the
        // first unanswered attempt until a session succeeds.
        let mut u = Unanswered::new(Duration::from_secs(120));
        assert!(!u.no_answer(at(0)));
        assert!(!u.no_answer(at(100)));
        assert!(u.no_answer(at(120)));
    }

    #[test]
    fn a_discovered_device_is_connected() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        let mut slices = 0;
        h.discover = Some(Box::new(move |_| {
            slices += 1;
            if slices < 3 {
                return Err(threeputt::Error::Timeout("test"));
            }
            Ok(Beacon {
                ip: LOCALHOST,
                server_uuid: None,
                message: None,
                udp_port: None,
                app_version: Some("1.2.3".into()),
                auto_reconnect: None,
                app_language: None,
            })
        }));
        h.start_on(&mock);
        let seen = h.until_status(ActorStatus::Connected);
        let phases: Vec<&str> = seen
            .iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::ActorStatus { telemetry, .. } => {
                    telemetry.get("phase").map(String::as_str)
                }
                _ => None,
            })
            .collect();
        assert_eq!(phases, ["discovering", "connecting", "measuring"]);
        let seen = h.until_ready(true, None);
        let firmware = seen.iter().find_map(|m| match &m.event {
            FlighthookEvent::DeviceTelemetry { firmware, .. } => Some(firmware.clone()),
            _ => None,
        });
        assert_eq!(firmware, Some(Some("1.2.3".into())));
    }

    #[test]
    fn discovery_without_a_device_hints_once() {
        let mut h = Harness::new();
        h.discover = Some(never_found(Duration::from_millis(10)));
        h.discovery_hint_after = Duration::from_millis(100);
        h.start(Arc::new(AtomicU16::new(0)));
        let mut seen = h.until("alert", |m| {
            matches!(m.event, FlighthookEvent::Alert { .. })
        });
        // Many more slices, no second hint.
        std::thread::sleep(Duration::from_millis(500));
        seen.extend(h.drain());
        assert_eq!(alerts(&seen), [DISCOVERY_HINT]);
        let FlighthookEvent::Alert { severity, .. } = seen
            .iter()
            .find(|m| matches!(m.event, FlighthookEvent::Alert { .. }))
            .map(|m| &m.event)
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(*severity, Severity::Warn);
        assert!(!seen.iter().any(|m| is_status(m, ActorStatus::Connected)));
    }

    #[test]
    fn stopping_during_discovery_ends_the_actor() {
        let mut h = Harness::new();
        h.discover = Some(never_found(Duration::from_millis(200)));
        h.start(Arc::new(AtomicU16::new(0)));
        h.until("discovering", |m| {
            matches!(&m.event, FlighthookEvent::ActorStatus { telemetry, .. }
                if telemetry.get("phase").map(String::as_str) == Some("discovering"))
        });
        let stopped = Instant::now();
        h.stop();
        assert!(stopped.elapsed() < WAIT, "stopped within a slice");
        h.until_status(ActorStatus::Disconnected);
    }

    #[test]
    fn readiness_follows_the_putt_cycle_into_gspro() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_on(&mock);
        let mut seen = h.until_ready(true, Some("ready"));
        assert!(gspro_ready(&seen), "ready for a putt");

        // Struck: not ready until the device says so again.
        mock.push_event(BALL_STRUCK);
        seen.extend(h.until_ready(false, Some("ball struck")));
        assert!(!gspro_ready(&seen), "ball struck");

        // The result changes neither value.
        mock.push_event(PUTT_RESULT_FULL);
        let shot = h.until("shot_finished", |m| {
            matches!(m.event, FlighthookEvent::ShotFinished { .. })
        });
        assert!(
            !shot
                .iter()
                .any(|m| matches!(m.event, FlighthookEvent::DeviceTelemetry { .. })),
            "no telemetry for the result"
        );
        seen.extend(shot);
        assert!(!gspro_ready(&seen), "still not ready after the result");

        mock.push_event(PUTT_READY);
        seen.extend(h.until_ready(true, Some("ready")));
        assert!(gspro_ready(&seen), "ready again");
    }

    #[test]
    fn a_dropped_device_is_not_ready_before_it_reconnects() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_on(&mock);
        let mut seen = h.until_ready(true, Some("ready"));
        mock.disconnect();
        let dropped = h.until_status(ActorStatus::Reconnecting);
        let not_ready = dropped.iter().position(|m| {
            matches!(&m.event, FlighthookEvent::DeviceTelemetry { telemetry: Some(t), .. }
                if t.get("ready").map(String::as_str) == Some("false"))
        });
        assert!(
            not_ready.is_some_and(|i| i < dropped.len() - 1),
            "ready=false before reconnecting: {:?}",
            dropped.iter().map(|m| &m.event).collect::<Vec<_>>()
        );
        // GSPro sees the device not ready from the telemetry alone.
        let before_status = &dropped[..dropped.len() - 1];
        seen.extend(before_status.iter().cloned());
        assert!(!gspro_ready(&seen));
    }

    #[test]
    fn a_putt_carries_its_values_as_a_raw_record() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_on(&mock);
        h.until_status(ActorStatus::Connected);
        mock.push_event(PUTT_RESULT_FULL);
        let events = h.next_shot();
        let Some(FlighthookEvent::ShotFinished { raw: Some(raw), .. }) = events.last() else {
            panic!("finished with a raw record: {events:?}");
        };
        assert_eq!(raw.transport, "exputt-wifi");
        assert_eq!(raw.frames.len(), 1);
        let frame = &raw.frames[0];
        assert_eq!(frame.dir, RawDir::Rx);
        assert_eq!(frame.hex, None);
        let values: Vec<f64> = frame
            .text
            .as_deref()
            .expect("text")
            .split(',')
            .map(|v| v.parse().expect("number"))
            .collect();
        assert_eq!(values, putt(PUTT_RESULT_FULL).raw);
    }

    /// The `device_state` values in `seen`, in order.
    fn device_states(seen: &[FlighthookMessage]) -> Vec<&str> {
        seen.iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::DeviceTelemetry {
                    telemetry: Some(t), ..
                } => t.get("device_state").map(String::as_str),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn find_mat_resyncs_the_device_and_is_ready_again() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_on(&mock);
        h.until_ready(true, Some("ready"));
        let before = mock.received().len();

        // A command for another ExPutt is not for this one.
        let _ = h.tx.send(
            FlighthookMessage::new(FlighthookEvent::FindMat {
                target: "exputt.1".into(),
            })
            .actor("web"),
        );
        let _ = h.tx.send(
            FlighthookMessage::new(FlighthookEvent::FindMat {
                target: "exputt.0".into(),
            })
            .actor("web"),
        );
        let mut seen = h.until_ready(false, Some("ready"));
        seen.extend(h.until_ready(true, Some("ready")));
        assert_eq!(
            device_states(&seen),
            ["ready", "finding mat", "mat found", "ready"]
        );
        assert!(
            !seen.iter().any(|m| is_status(m, ActorStatus::Reconnecting)),
            "no reconnect"
        );

        let find_mat_sent =
            |p: &[threeputt::testing::ReceivedPacket]| round_sent(&p[before..], Hand::Right);
        assert!(
            mock.wait_until(WAIT, find_mat_sent),
            "{:?}",
            mock.received()
        );
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
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
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

    /// Alerts and their arrival times, until `want` matches one.
    fn alert_times(
        h: &mut Harness,
        what: &str,
        want: impl Fn(&str) -> bool,
    ) -> Vec<(String, Instant)> {
        let seen = std::cell::RefCell::new(Vec::new());
        h.until(what, |m| {
            let FlighthookEvent::Alert { message, .. } = &m.event else {
                return false;
            };
            seen.borrow_mut().push((message.clone(), Instant::now()));
            want(message)
        });
        seen.into_inner()
    }

    #[test]
    fn a_silent_device_gets_time_before_the_reconnect() {
        let stall_wait = Duration::from_millis(600);
        let mock = quiet_mock();
        let second = MockDevice::spawn(MockConfig::default()).expect("mock");
        let port = Arc::new(AtomicU16::new(mock.port()));
        let mut h = Harness::new();
        h.stall_wait = stall_wait;
        h.start(Arc::clone(&port));
        h.until_status(ActorStatus::Connected);
        port.store(second.port(), Ordering::SeqCst);

        let alerts = alert_times(&mut h, "silence", |m| m.contains("no data from the device"));
        let lost = alerts.last().expect("alert").1;
        h.until_status(ActorStatus::Connected);
        let waited = lost.elapsed();
        assert!(
            waited >= stall_wait - Duration::from_millis(30),
            "reconnected after {waited:?}"
        );
        assert!(sent(&second, Request::MatchBegin));
    }

    #[test]
    fn hand_changes_inside_the_gap_are_one_restart_to_the_final_hand() {
        let gap = Duration::from_millis(900);
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.resync_gap = gap;
        h.start_on(&mock);
        h.until_ready(true, Some("ready"));
        let before = mock.received().len();
        let hand = |handed| {
            FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
                mode: None,
                handed: Some(handed),
                club: None,
            })
            .actor("system")
        };
        // Within the gap after the session's own search: right (in effect),
        // left, right, left, left.
        for handed in [
            Handedness::Right,
            Handedness::Left,
            Handedness::Right,
            Handedness::Left,
            Handedness::Left,
        ] {
            let _ = h.tx.send(hand(handed));
        }
        // Held: not ready, nothing sent yet.
        h.until_ready(false, None);
        std::thread::sleep(gap / 3);
        assert!(
            mock.received()[before..]
                .iter()
                .all(|p| p.json == Request::Alive.to_json()),
            "{:?}",
            mock.received()
        );

        // One restart, to the final hand, once the gap is over.
        let mut seen = h.until_ready(true, Some("ready"));
        assert!(
            mock.wait_until(WAIT, |p| round_sent(&p[before..], Hand::Left)),
            "{:?}",
            mock.received()
        );
        seen.extend(h.drain());
        assert!(!seen.iter().any(|m| is_status(m, ActorStatus::Reconnecting)));
    }

    #[test]
    fn a_hand_flipped_back_inside_the_gap_sends_nothing() {
        let gap = Duration::from_millis(1000);
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.resync_gap = gap;
        h.start_on(&mock);
        h.until_ready(true, Some("ready"));
        let before = mock.received().len();
        let hand = |handed| {
            FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
                mode: None,
                handed: Some(handed),
                club: None,
            })
            .actor("system")
        };
        // Held for the gap: not ready meanwhile.
        let _ = h.tx.send(hand(Handedness::Left));
        h.until_ready(false, None);
        // Back to the hand in effect: nothing to do, ready as the device last
        // said.
        let _ = h.tx.send(hand(Handedness::Right));
        h.until_ready(true, None);
        std::thread::sleep(gap * 3 / 2);
        assert!(
            mock.received()[before..]
                .iter()
                .all(|p| p.json == Request::Alive.to_json()),
            "{:?}",
            mock.received()
        );
    }

    #[test]
    fn find_mat_inside_the_gap_waits_for_it() {
        let gap = Duration::from_millis(1000);
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.resync_gap = gap;
        h.start_on(&mock);
        h.until_ready(true, Some("ready"));
        let before = mock.received().len();
        for _ in 0..3 {
            let _ = h.tx.send(
                FlighthookMessage::new(FlighthookEvent::FindMat {
                    target: "exputt.0".into(),
                })
                .actor("web"),
            );
        }
        // Not ready, nothing sent until the gap is over; then one search.
        h.until_ready(false, None);
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            mock.received()[before..]
                .iter()
                .all(|p| p.json == Request::Alive.to_json()),
            "{:?}",
            mock.received()
        );
        h.until_ready(true, Some("ready"));
        std::thread::sleep(gap / 2);
        assert!(
            mock.wait_until(WAIT, |p| round_sent(&p[before..], Hand::Right)),
            "{:?}",
            mock.received()
        );
    }

    /// The alerts of one outage, up to and including the "not answering" one:
    /// the failed attempts are alerted first, the user is told once the
    /// outage has lasted `after`, and attempts after that are not alerted.
    fn assert_outage_alerts(seen: &[(String, Instant)], after: Duration) {
        let errors: Vec<&Instant> = seen
            .iter()
            .filter(|(m, _)| m.starts_with("ExPutt error") && !m.contains("lost"))
            .map(|(_, at)| at)
            .collect();
        assert!(errors.len() >= 2, "several failed attempts first: {seen:?}");
        let told = seen.last().expect("alert").1;
        assert!(
            told.duration_since(*errors[0]) >= after - Duration::from_millis(30),
            "told after {:?}: {seen:?}",
            told.duration_since(*errors[0])
        );
    }

    #[test]
    fn a_device_that_never_answers_is_reported_once_per_outage() {
        let after = Duration::from_millis(600);
        let wedged = Wedged::spawn();
        let good = MockDevice::spawn(MockConfig::default()).expect("mock");
        let port = Arc::new(AtomicU16::new(wedged.port));
        let mut h = Harness::new();
        h.not_answering_after = after;
        h.reply_timeout = Duration::from_millis(200);
        h.start(Arc::clone(&port));

        let seen = alert_times(&mut h, "not answering", |m| m == NOT_ANSWERING);
        assert_outage_alerts(&seen, after);

        // Attempts go on, silently: neither the per-attempt alert nor the
        // "not answering" one again while the outage lasts.
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(alerts(&h.drain()), Vec::<String>::new());

        // A session that succeeds ends the outage; the next one is alerted
        // per attempt again and told once more.
        port.store(good.port(), Ordering::SeqCst);
        h.until_status(ActorStatus::Connected);
        port.store(wedged.port, Ordering::SeqCst);
        good.disconnect();
        let again = alert_times(&mut h, "not answering again", |m| m == NOT_ANSWERING);
        assert_outage_alerts(&again, after);
    }

    #[test]
    fn an_outage_that_flaps_between_refused_and_unanswered_is_still_reported() {
        let after = Duration::from_millis(600);
        let wedged = Wedged::spawn();
        // A port nothing listens on: attempts are refused.
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port();
        let port = Arc::new(AtomicU16::new(wedged.port));
        let flapping = Arc::new(AtomicBool::new(true));
        let flapper = {
            let (port, flapping, wedged) = (Arc::clone(&port), Arc::clone(&flapping), wedged.port);
            std::thread::spawn(move || {
                let mut ports = [wedged, closed].into_iter().cycle();
                while flapping.load(Ordering::SeqCst) {
                    port.store(ports.next().expect("cycle"), Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(120));
                }
            })
        };
        let mut h = Harness::new();
        h.not_answering_after = after;
        h.reply_timeout = Duration::from_millis(200);
        h.start(Arc::clone(&port));

        let seen = alert_times(&mut h, "not answering", |m| m == NOT_ANSWERING);
        flapping.store(false, Ordering::SeqCst);
        flapper.join().expect("flapper");
        assert!(
            seen.iter()
                .any(|(m, _)| m.contains("Connection refused") || m.contains("refused")),
            "some attempts were refused: {seen:?}"
        );
    }

    #[test]
    fn a_silent_device_is_not_ready_until_it_sends_again() {
        // No heartbeat: ready from the device's own report, then silence past
        // the warning threshold (a third of the baseline limit).
        let mock = quiet_mock();
        let mut h = Harness::new();
        h.start_on(&mock);
        let mut seen = h.until_ready(true, Some("ready"));
        seen.extend(h.until_ready(false, None));
        assert!(
            !gspro_ready(&seen),
            "GSPro is told not ready during silence"
        );

        // It sends again before the limit: ready as the device last said,
        // and the session stands.
        mock.push_event("Test,yy");
        seen.extend(h.until_ready(true, None));
        assert!(gspro_ready(&seen), "ready again");
        std::thread::sleep(Duration::from_millis(100));
        seen.extend(h.drain());
        assert!(
            !seen.iter().any(|m| is_status(m, ActorStatus::Reconnecting)),
            "no reconnect"
        );
    }

    #[test]
    fn a_stop_while_measuring_closes_the_session_at_once() {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        let mut h = Harness::new();
        h.start_on(&mock);
        h.until_ready(true, Some("ready"));
        let stopped = Instant::now();
        h.stop();
        let took = stopped.elapsed();
        assert!(took < Duration::from_secs(1), "{took:?}");
        assert!(sent(&mock, Request::MatchEnded), "the session was closed");
    }

    /// Starting a session cannot be interrupted: a stop during a handshake
    /// the device never answers ends when the handshake times out, inside
    /// the actor's stop wait.
    #[test]
    fn a_stop_during_a_wedged_session_start_ends_within_the_stop_wait() {
        let wedged = Wedged::spawn();
        let mut h = Harness::new();
        h.reply_timeout = Duration::from_millis(1500);
        h.start(Arc::new(AtomicU16::new(wedged.port)));
        h.until("connecting", |m| {
            matches!(&m.event, FlighthookEvent::ActorStatus { telemetry, .. }
                if telemetry.get("phase").map(String::as_str) == Some("connecting"))
        });
        std::thread::sleep(Duration::from_millis(200));
        let stopped = Instant::now();
        h.stop();
        let took = stopped.elapsed();
        assert!(took < h.reply_timeout + Duration::from_secs(1), "{took:?}");
        assert!(took < ExPuttActor { address: None }.stop_wait(), "{took:?}");
    }

    /// Every output's numbers for an ExPutt putt.
    #[test]
    fn outputs_keep_their_numbers() {
        let p = putt(PUTT_RESULT_FULL);
        let messages = crate::test_ball::device_shot(
            "exputt.0",
            ball_from_putt(&p),
            None,
            club_from_putt(&p),
            Some(ShotDetectionMode::Putting),
        );
        crate::test_ball::assert_outputs_pinned("exputt", &messages);
    }
}
