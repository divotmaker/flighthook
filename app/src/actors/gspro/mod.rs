//! GSPro Open Connect V1 client: the `openconnect_client` actor.
//!
//! Subscribes to the unified bus for shot data and forwards it over TCP to a
//! simulator that accepts Open Connect: GSPro (port 921), Muni Golf (9210),
//! or any other. Emits PlayerInfo/ClubInfo/HandInfo for player, club and
//! hand changes and ActorStatus for connection status. Reconnects with linear backoff on failure.
//!
//! The bus is drained the whole time, connected or not. Every shot that
//! finishes while the simulator is connected is sent, in order; a shot that
//! finishes while it is not, or is older than [`STALE_SHOT`] when the bridge
//! could send it, is dropped (debug log only: a simulator not running is
//! normal).
//!
//! A connection refused by this process's own Open Connect server (the client
//! points at it, which would loop shots back) is a setup mistake,
//! not a simulator that is not running: the card's telemetry carries the
//! reason as `error` until a connection is accepted or fails another way.
//!
//! The card's telemetry ([`Telemetry`]): `address` (the simulator's),
//! `last_shot` (the shot number the simulator got last and when it was
//! sent), and for the current connection only `shots_sent`,
//! `monitor_ready` (the readiness last sent to the simulator) and
//! `response` (the simulator's last response code and message). The web
//! layer shows the simulator's player information as `club` and `handed`;
//! the client removes those too when the connection ends.

pub mod api;
pub mod mapper;
pub(crate) mod proto;

use proto::*;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::backoff::Backoff;
use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;
use crate::state::local_clients::LocalClients;
use flighthook::{
    ActorStatus, FlighthookEvent, FlighthookMessage, ShotAccumulator, ShotData, ShotDetectionMode,
    ShotKey,
};

/// Bridge-internal error type.
#[derive(Debug)]
pub(crate) enum BridgeError {
    Io(std::io::Error),
    /// This process's own Open Connect server refused the connection; the
    /// reason it gave.
    Refused(String),
    Shutdown,
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Refused(reason) => write!(f, "{reason}"),
            Self::Shutdown => write!(f, "shutdown"),
        }
    }
}

/// Per-mode launch monitor routing configuration.
#[derive(Debug, Clone, Default)]
pub struct GsProRouting {
    pub full_monitor: Option<String>,
    pub chipping_monitor: Option<String>,
    pub putting_monitor: Option<String>,
}

/// Open Connect client actor (`openconnect_client.N`). Connects to a
/// simulator that accepts GSPro Open Connect V1 (GSPro on port 921, Muni Golf
/// on 9210; TCP, JSON), forwards shot data, and handles heartbeats.
pub struct GsProActor {
    pub addr: SocketAddr,
    pub routing: GsProRouting,
}

impl Actor for GsProActor {
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let addr = self.addr;
        let routing = self.routing.clone();
        let clients = Arc::clone(&state.local_clients);
        let thread_name = format!("gspro:{}", sender.actor_id());
        if let Some(warning) = putting_routing_warning(&state.system.snapshot(), &routing) {
            tracing::warn!("{}: {warning}", sender.actor_id());
        }

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(addr, routing, &clients, sender, receiver))
            .expect("failed to spawn gspro thread")
    }

    /// The simulator is reached over TCP as a client, and the old
    /// instance's registration as a local client ends with its connection:
    /// a connect still running in the old instance keeps nothing from the
    /// new one, and closes itself once it completes.
    fn replaceable_while_stopping(&self) -> bool {
        true
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let actor_id = sender.actor_id();
        let (_, index) = match actor_id.split_once('.') {
            Some(pair) => pair,
            None => return ReconfigureOutcome::Applied,
        };

        let snap = state.system.snapshot();
        let section = match snap.openconnect_client.get(index) {
            Some(s) => s,
            None => return ReconfigureOutcome::RestartRequired, // section removed
        };

        let new_addr_str = section.address.as_deref().unwrap_or("127.0.0.1:921");
        if let Ok(new_addr) = new_addr_str.parse::<SocketAddr>() {
            if new_addr != self.addr {
                return ReconfigureOutcome::RestartRequired;
            }
        } else {
            return ReconfigureOutcome::RestartRequired;
        }

        // Check routing changes
        if section.full_monitor != self.routing.full_monitor
            || section.chipping_monitor != self.routing.chipping_monitor
            || section.putting_monitor != self.routing.putting_monitor
        {
            return ReconfigureOutcome::RestartRequired;
        }

        ReconfigureOutcome::Applied
    }
}

/// Why the routing may send unwanted putts: an ExPutt is configured but the
/// Putting Monitor accepts any device, so a putt another device also sees
/// reaches the simulator twice.
fn putting_routing_warning(
    config: &flighthook::FlighthookConfig,
    routing: &GsProRouting,
) -> Option<&'static str> {
    (!config.exputt.is_empty() && routing.putting_monitor.is_none()).then_some(
        "an ExPutt is configured but the Putting Monitor is Any, so putts from every \
         device are sent; set the Putting Monitor to the ExPutt",
    )
}

/// Reconnect backoff: 1 s, +1 s per failed attempt, capped at 3 s, back to
/// 1 s after a session that lasted 10 s
/// ([`STABLE_SESSION`](super::backoff::STABLE_SESSION)). GSPro is on loopback
/// or the LAN, so retrying a refused connect costs nothing, and a short cap
/// means the bridge is back within a few seconds of GSPro starting.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(3);

/// Oldest a finished shot may be when the bridge sends it, counted from when
/// the bridge took it off the bus; older shots are dropped. A connected bridge sends a shot within one loop pass (~150 ms),
/// and a connection that drops after being up 10 s or more is back within
/// ~1.5 s (read timeout + 1 s backoff + connect), so a shot caught by a
/// brief blip still goes through.
/// 5 s is well short of the time a golfer needs to set up the next ball, so
/// GSPro only ever plays the shot the golfer has just hit.
const STALE_SHOT: Duration = Duration::from_secs(5);

/// How often the loops poll the bus and the shutdown flag while idle.
const IDLE_POLL: Duration = Duration::from_millis(100);

/// Longest a write to GSPro may block. A write that times out ends the
/// connection like any other I/O error, so a GSPro that stops reading cannot
/// stall the bridge and leave the bus undrained.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a connection must stay up before the client reports itself
/// connected: an Open Connect server refuses its own client as soon as it
/// accepts it, well within this, so a refused connection never shows as
/// Connected.
const REFUSAL_VERDICT: Duration = Duration::from_secs(1);

/// Longest a connect to the simulator may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Telemetry clearing the `error` key: an empty value removes a key.
fn clear_error() -> HashMap<String, String> {
    HashMap::from([("error".into(), String::new())])
}

/// The simulator's address.
const ADDRESS: &str = "address";
/// The shot number the simulator got last and when it was sent.
const LAST_SHOT: &str = "last_shot";
/// The shots sent on this connection.
const SHOTS_SENT: &str = "shots_sent";
/// The readiness last sent to the simulator on this connection.
const MONITOR_READY: &str = "monitor_ready";
/// The simulator's last response on this connection: code and message.
const RESPONSE: &str = "response";
/// The simulator's player information, shown by the web layer.
const PLAYER: [&str; 2] = ["club", "handed"];
/// The keys that hold for one connection.
const PER_CONNECTION: [&str; 3] = [SHOTS_SENT, MONITOR_READY, RESPONSE];

/// The card's actor telemetry. A key goes out only when its value changes;
/// an empty value removes it.
struct Telemetry {
    /// The values the card holds now.
    reported: HashMap<String, String>,
    /// Changes not yet sent.
    pending: HashMap<String, String>,
}

impl Telemetry {
    /// A new instance's telemetry for a simulator at `addr`: its first
    /// status sets the address and removes what an earlier instance left.
    fn new(addr: SocketAddr) -> Self {
        let mut pending: HashMap<String, String> = std::iter::once(LAST_SHOT)
            .chain(PER_CONNECTION)
            .chain(PLAYER)
            .map(|k| (k.to_owned(), String::new()))
            .collect();
        pending.insert(ADDRESS.into(), addr.to_string());
        Self {
            reported: HashMap::new(),
            pending,
        }
    }

    /// Set `key` to `value`.
    fn set(&mut self, key: &str, value: String) {
        if self.reported.get(key).map_or("", String::as_str) == value {
            self.pending.remove(key);
        } else {
            self.pending.insert(key.to_owned(), value);
        }
    }

    /// Record that the card shows `key` without sending it: the web layer
    /// sets the player information.
    fn shown(&mut self, key: &str) {
        self.reported.insert(key.to_owned(), "shown".into());
    }

    /// Remove what held for the connection that ended.
    fn clear_connection(&mut self) {
        for key in PER_CONNECTION.into_iter().chain(PLAYER) {
            self.set(key, String::new());
        }
    }

    /// The changes not yet sent, now taken as sent.
    fn take(&mut self) -> HashMap<String, String> {
        for (k, v) in &self.pending {
            if v.is_empty() {
                self.reported.remove(k);
            } else {
                self.reported.insert(k.clone(), v.clone());
            }
        }
        std::mem::take(&mut self.pending)
    }
}

/// A response for the card: its code and message.
fn response_text(code: i32, message: &str) -> String {
    format!("{code} {}", message.trim()).trim_end().to_owned()
}

/// A shot sent to the simulator for the card: its number and the local time.
fn last_shot_text(number: u32) -> String {
    format!("#{number} at {}", chrono::Local::now().format("%H:%M:%S"))
}

/// Main bridge loop. Reconnects forever until the bus closes.
fn run(
    addr: SocketAddr,
    routing: GsProRouting,
    clients: &LocalClients,
    sender: BusSender,
    receiver: BusReceiver,
) {
    run_with(
        addr,
        routing,
        Backoff::new(MIN_BACKOFF, MAX_BACKOFF),
        clients,
        &sender,
        receiver,
    );
}

pub(crate) fn run_with(
    addr: SocketAddr,
    routing: GsProRouting,
    mut backoff: Backoff,
    clients: &LocalClients,
    sender: &BusSender,
    mut receiver: BusReceiver,
) {
    let mut relay = Relay::new(routing);
    let mut telemetry = Telemetry::new(addr);
    let mut ever_connected = false;
    // Whether our own Open Connect server refused the last connection.
    let mut refused = false;

    loop {
        if relay.drain(&mut receiver).is_err() {
            tracing::info!("gspro bridge: shutting down");
            return;
        }

        let status = if ever_connected {
            ActorStatus::Reconnecting
        } else {
            ActorStatus::Starting
        };
        // The first status also clears an `error` left by an earlier run of
        // this actor.
        let mut update = telemetry.take();
        if !ever_connected {
            update.extend(clear_error());
        }
        sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
            status,
            telemetry: update,
        }));

        let result = connect_and_run(
            addr,
            &mut relay,
            &mut telemetry,
            clients,
            sender,
            &mut receiver,
        );
        let session = relay.disconnect();
        telemetry.clear_connection();
        ever_connected |= session.is_some();
        match result {
            Ok(()) | Err(BridgeError::Shutdown) => {
                tracing::info!("gspro bridge: shutting down");
                return;
            }
            Err(e) => {
                let backoff_status = if ever_connected {
                    ActorStatus::Reconnecting
                } else {
                    ActorStatus::Starting
                };
                let delay = backoff.delay(session);
                let text = e.to_string();
                // GSPro not running is normal (one simulator at a time), so a
                // lost or refused connection is a status, not an alert. Our
                // own server refusing us is a setup mistake: the card says
                // why (the server raises the alert).
                let mut update = telemetry.take();
                match e {
                    BridgeError::Refused(reason) => {
                        refused = true;
                        update.insert("error".into(), reason);
                    }
                    _ if std::mem::take(&mut refused) => update.extend(clear_error()),
                    _ => {}
                }
                tracing::info!("gspro bridge: {text}, retrying in {delay:?}");
                sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                    status: backoff_status,
                    telemetry: update,
                }));

                let deadline = Instant::now() + delay;
                loop {
                    if relay.drain(&mut receiver).is_err() {
                        tracing::info!("gspro bridge: shutting down");
                        return;
                    }
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    std::thread::sleep(left.min(IDLE_POLL));
                }
            }
        }
    }
}

/// A finished shot waiting to be sent.
struct PendingShot {
    shot: ShotData,
    finished_at: Instant,
}

/// Bus-side bridge state, kept across reconnects so the bus is drained the
/// whole time the actor runs, connected or not.
struct Relay {
    routing: GsProRouting,
    /// When GSPro connected, `None` while it is not. Shots that finish while
    /// it is not are dropped.
    connected_at: Option<Instant>,
    mode: ShotDetectionMode,
    monitor_state: HashMap<String, bool>,
    readiness_changed: bool,
    /// Per-actor shot accumulators, keyed by (actor, shot_key).
    accumulators: HashMap<(String, ShotKey), ShotAccumulator>,
    /// Shots finished while connected, oldest first.
    outbox: VecDeque<PendingShot>,
    /// The `ShotNumber` handed out last ([`Relay::next_shot`]), 0 before the
    /// first. Kept for the actor's lifetime, across reconnects.
    last_number: u32,
}

impl Relay {
    fn new(routing: GsProRouting) -> Self {
        Self {
            routing,
            connected_at: None,
            mode: ShotDetectionMode::Full,
            monitor_state: HashMap::new(),
            readiness_changed: false,
            accumulators: HashMap::new(),
            outbox: VecDeque::new(),
            last_number: 0,
        }
    }

    /// Mark GSPro connected at `now`.
    fn connect(&mut self, now: Instant) {
        self.connected_at = Some(now);
    }

    /// Mark GSPro not connected. Returns how long the connection lasted,
    /// `None` if it was not connected.
    fn disconnect(&mut self) -> Option<Duration> {
        self.connected_at.take().map(|at| at.elapsed())
    }

    /// Process every message waiting on the bus.
    fn drain(&mut self, receiver: &mut BusReceiver) -> Result<(), BridgeError> {
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => return Err(BridgeError::Shutdown),
                Ok(None) => return Ok(()),
                Ok(Some(msg)) => self.handle(msg, Instant::now()),
            }
        }
    }

    fn handle(&mut self, msg: FlighthookMessage, now: Instant) {
        match msg.event {
            FlighthookEvent::ShotTrigger { ref key } => {
                let acc = ShotAccumulator::new(msg.actor.clone(), key.clone());
                self.accumulators
                    .insert((msg.actor.clone(), key.clone()), acc);
            }
            // `derived` is ignored: GSPro simulates the flight itself.
            FlighthookEvent::BallFlight {
                ref key, ref ball, ..
            } => {
                if let Some(acc) = self.accumulators.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_ball(*ball.clone());
                }
            }
            FlighthookEvent::FaceImpact {
                ref key,
                ref impact,
            } => {
                if let Some(acc) = self.accumulators.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_impact(*impact.clone());
                }
            }
            FlighthookEvent::ClubPath { ref key, ref club } => {
                if let Some(acc) = self.accumulators.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_club(*club.clone());
                }
            }
            FlighthookEvent::ShotFinished { ref key, .. } => {
                if let Some(acc) = self.accumulators.remove(&(msg.actor.clone(), key.clone())) {
                    if !shot_matches_routing(&self.routing, self.mode, &msg.actor) {
                        tracing::debug!(
                            "gspro bridge: skipping shot #{} from '{}' (routed to {:?} for mode {:?})",
                            key.shot_number,
                            msg.actor,
                            match self.mode {
                                ShotDetectionMode::Full => &self.routing.full_monitor,
                                ShotDetectionMode::Chipping => &self.routing.chipping_monitor,
                                ShotDetectionMode::Putting => &self.routing.putting_monitor,
                            },
                            self.mode,
                        );
                    } else if let Some(shot) = acc.finish() {
                        if self.connected_at.is_some() {
                            self.outbox.push_back(PendingShot {
                                shot,
                                finished_at: now,
                            });
                        } else {
                            tracing::debug!(
                                "gspro bridge: GSPro not connected, dropping shot #{} from '{}'",
                                key.shot_number,
                                msg.actor,
                            );
                        }
                    }
                }
            }
            FlighthookEvent::DeviceTelemetry {
                telemetry: Some(ref tel),
                ..
            } if tel.contains_key("ready") => {
                let ready = tel.get("ready").is_some_and(|v| v == "true");
                self.monitor_state.insert(msg.actor.clone(), ready);
                self.readiness_changed = true;
            }
            FlighthookEvent::SetDetectionMode { mode: Some(m), .. } => {
                self.mode = m;
                self.readiness_changed = true;
            }
            FlighthookEvent::ActorStatus { status, .. } => {
                if matches!(
                    status,
                    ActorStatus::Disconnected | ActorStatus::Reconnecting
                ) && self.monitor_state.contains_key(&msg.actor)
                {
                    self.monitor_state.insert(msg.actor.clone(), false);
                    self.readiness_changed = true;
                }
            }
            _ => {}
        }
    }

    /// Next shot to send, oldest first. Shots older than [`STALE_SHOT`] at
    /// `now` are dropped.
    ///
    /// The shot's `shot_number` becomes its `ShotNumber` for the simulator:
    /// the shot's own number, or one more than the last one handed out when
    /// that is higher. Numbers handed out therefore always increase, even
    /// across sources that count separately (a launch monitor and Auto-chip)
    /// or a device whose count starts again after it reconnects; one
    /// device's increasing numbers go out unchanged.
    fn next_shot(&mut self, now: Instant) -> Option<ShotData> {
        while let Some(pending) = self.outbox.pop_front() {
            let age = now.saturating_duration_since(pending.finished_at);
            if age <= STALE_SHOT {
                let mut shot = pending.shot;
                let number = shot.shot_number.max(self.last_number.saturating_add(1));
                if number != shot.shot_number {
                    tracing::debug!(
                        "gspro bridge: shot #{} from '{}' sent as ShotNumber {number} \
                         (last sent {})",
                        shot.shot_number,
                        shot.actor,
                        self.last_number,
                    );
                }
                shot.shot_number = number;
                self.last_number = number;
                return Some(shot);
            }
            tracing::debug!(
                "gspro bridge: dropping shot #{} from '{}', finished {age:?} ago",
                pending.shot.shot_number,
                pending.shot.actor,
            );
        }
        None
    }

    /// Take the readiness-changed flag.
    fn take_readiness_changed(&mut self) -> bool {
        std::mem::take(&mut self.readiness_changed)
    }
}

/// Compute readiness from routing config, current mode, and per-monitor state.
///
/// Returns a single `ready` bool. `DeviceTelemetry.ready` is the unified
/// readiness signal (all device conditions met). GSPro's
/// `launch_monitor_is_ready` and `launch_monitor_ball_detected` are both
/// set from this single value.
fn readiness(
    routing: &GsProRouting,
    mode: ShotDetectionMode,
    states: &HashMap<String, bool>,
) -> bool {
    let target = match mode {
        ShotDetectionMode::Full => &routing.full_monitor,
        ShotDetectionMode::Chipping => &routing.chipping_monitor,
        ShotDetectionMode::Putting => &routing.putting_monitor,
    };
    match target {
        Some(id) => *states.get(id.as_str()).unwrap_or(&false),
        None => {
            // "Any" — ready if any monitor is ready
            states.values().any(|&r| r)
        }
    }
}

/// Check if a shot actor matches the routing target for the current mode.
/// Auto-chip shots ([`flighthook::AUTO_CHIP_ACTOR`]) match every routing:
/// they stand in for a chip the routed monitor could not read.
/// Readiness a client with `routing` reports in `mode` after handling
/// `messages`.
#[cfg(test)]
pub(crate) fn readiness_after(
    routing: GsProRouting,
    mode: ShotDetectionMode,
    messages: impl IntoIterator<Item = FlighthookMessage>,
) -> bool {
    let mut relay = Relay::new(routing);
    relay.mode = mode;
    for msg in messages {
        relay.handle(msg, Instant::now());
    }
    readiness(&relay.routing, relay.mode, &relay.monitor_state)
}

fn shot_matches_routing(routing: &GsProRouting, mode: ShotDetectionMode, actor: &str) -> bool {
    if actor == flighthook::AUTO_CHIP_ACTOR {
        return true;
    }
    let target = match mode {
        ShotDetectionMode::Full => &routing.full_monitor,
        ShotDetectionMode::Chipping => &routing.chipping_monitor,
        ShotDetectionMode::Putting => &routing.putting_monitor,
    };
    match target {
        Some(id) => actor == id,
        None => true, // "Any" — accept from all
    }
}

/// Connect and run one session. The connection is registered in `clients`
/// while it lasts, so this process's own Open Connect server can refuse it;
/// a refusal ends the session as [`BridgeError::Refused`]. The client
/// reports itself connected only once the connection has outlived
/// [`REFUSAL_VERDICT`], clearing the error.
fn connect_and_run(
    addr: SocketAddr,
    relay: &mut Relay,
    telemetry: &mut Telemetry,
    clients: &LocalClients,
    sender: &BusSender,
    receiver: &mut BusReceiver,
) -> Result<(), BridgeError> {
    // A stop during the connect ends the bridge at once.
    let mut stream = super::connect_tcp(sender.actor_id(), addr, CONNECT_TIMEOUT, receiver)
        .map_err(BridgeError::Io)?
        .ok_or(BridgeError::Shutdown)?;
    // Registered before anything is sent, so a server reading our data
    // already knows the connection is ours.
    let registration = match stream.local_addr() {
        Ok(local) => Some(clients.register(local, sender.actor_id())),
        Err(e) => {
            tracing::debug!(
                "gspro bridge: no local address ({e}); our own Open Connect server \
                 cannot recognise this connection"
            );
            None
        }
    };
    let result = session(&mut stream, addr, relay, telemetry, sender, receiver);
    match registration.and_then(|r| r.refusal()) {
        Some(reason) if !matches!(result, Ok(()) | Err(BridgeError::Shutdown)) => {
            Err(BridgeError::Refused(reason))
        }
        _ => result,
    }
}

/// One connected session with the simulator. Its telemetry goes out with
/// the Connected status, then as it changes.
fn session(
    stream: &mut TcpStream,
    addr: SocketAddr,
    relay: &mut Relay,
    telemetry: &mut Telemetry,
    sender: &BusSender,
    receiver: &mut BusReceiver,
) -> Result<(), BridgeError> {
    let name = sender.actor_id();
    stream.set_nodelay(true).map_err(BridgeError::Io)?;
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .map_err(BridgeError::Io)?;
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(BridgeError::Io)?;

    // Shots that finished during the connect attempt finished while GSPro
    // was not connected.
    relay.drain(receiver)?;
    let connected_at = Instant::now();
    relay.connect(connected_at);
    tracing::info!("gspro bridge: connected to {addr}");
    // Shots flow from now; the Connected status waits for the verdict.
    let mut announced = false;
    let mut shots_sent: u32 = 0;
    telemetry.set(SHOTS_SENT, shots_sent.to_string());

    // Backdate so the first heartbeat fires after ~1s instead of waiting the full 10s.
    let mut last_heartbeat = Instant::now() - Duration::from_secs(9);
    let mut read_buf = vec![0u8; 4096];
    let mut prev_readiness = false;

    loop {
        if receiver.is_shutdown() {
            return Err(BridgeError::Shutdown);
        }
        let mut activity = false;

        // 1. Read any data from GSPro
        match stream.read(&mut read_buf) {
            Ok(0) => {
                tracing::info!("gspro <- connection closed");
                return Err(BridgeError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "connection closed by GSPro",
                )));
            }
            Ok(n) => {
                let raw_text = String::from_utf8_lossy(&read_buf[..n]);
                let decoded = parse_response_debug(&read_buf[..n]);
                tracing::info!(
                    target: "audit",
                    "{name} received response {raw_text}{}",
                    decoded.as_deref().map(|d| format!(" | {d}")).unwrap_or_default(),
                );
                let heard = handle_response(&read_buf[..n], sender);
                if let Some((code, message)) = heard.last {
                    telemetry.set(RESPONSE, response_text(code, &message));
                }
                if heard.player {
                    for key in PLAYER {
                        telemetry.shown(key);
                    }
                }
                activity = true;
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                tracing::info!("gspro <- read error: {e}");
                return Err(BridgeError::Io(e));
            }
        }

        // 2. Drain bus events
        relay.drain(receiver)?;

        // 3. Send every finished shot, oldest first
        let mut on_sent = |number| {
            shots_sent += 1;
            telemetry.set(SHOTS_SENT, shots_sent.to_string());
            telemetry.set(LAST_SHOT, last_shot_text(number));
        };
        if send_shots(relay, stream, Instant::now(), name, &mut on_sent)? {
            activity = true;
        }

        // 4. Immediate heartbeat on readiness change
        if relay.take_readiness_changed() {
            let new_readiness = readiness(&relay.routing, relay.mode, &relay.monitor_state);
            if new_readiness != prev_readiness {
                prev_readiness = new_readiness;
                let msg = api::GsProMessage::heartbeat_with_readiness(new_readiness, new_readiness);
                tracing::debug!("gspro -> heartbeat (ready={})", new_readiness,);
                if let Ok(json_str) = serde_json::to_string(&msg) {
                    tracing::info!(
                        target: "audit",
                        "{name} sent heartbeat {json_str} | {msg:?}",
                    );
                }
                send_message(stream, &msg)?;
                telemetry.set(MONITOR_READY, new_readiness.to_string());
                last_heartbeat = Instant::now();
                activity = true;
            }
        }

        // 5. Periodic heartbeat if due
        if last_heartbeat.elapsed() >= Duration::from_secs(10) {
            last_heartbeat = Instant::now();
            let ready = readiness(&relay.routing, relay.mode, &relay.monitor_state);
            prev_readiness = ready;
            let msg = api::GsProMessage::heartbeat_with_readiness(ready, ready);
            tracing::debug!("gspro -> heartbeat (ready={ready})");
            if let Ok(json_str) = serde_json::to_string(&msg) {
                tracing::info!(
                    target: "audit",
                    "{name} sent heartbeat {json_str} | {msg:?}",
                );
            }
            send_message(stream, &msg)?;
            telemetry.set(MONITOR_READY, ready.to_string());
            activity = true;
        }

        if !announced && connected_at.elapsed() >= REFUSAL_VERDICT {
            announced = true;
            let mut update = telemetry.take();
            update.extend(clear_error());
            sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                status: ActorStatus::Connected,
                telemetry: update,
            }));
        } else if announced && !telemetry.pending.is_empty() {
            sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                status: ActorStatus::Connected,
                telemetry: telemetry.take(),
            }));
        }

        if !activity {
            std::thread::sleep(IDLE_POLL);
        }
    }
}

/// Send every shot in the outbox that is not stale at `now`, oldest first,
/// calling `on_sent` with each one's shot number once written. Returns
/// whether any was sent. A failed write is returned as the connection's
/// error; the shot it was writing is not retried.
fn send_shots(
    relay: &mut Relay,
    stream: &mut impl Write,
    now: Instant,
    name: &str,
    on_sent: &mut impl FnMut(u32),
) -> Result<bool, BridgeError> {
    let mut sent = false;
    while let Some(shot) = relay.next_shot(now) {
        let msg = mapper::map_shot(&shot);
        log_outbound(&msg);
        if let Ok(json_str) = serde_json::to_string(&msg) {
            tracing::info!(
                target: "audit",
                "{name} sent shot {json_str} | {msg:?}",
            );
        }
        if let Err(e) = send_message(stream, &msg) {
            tracing::debug!(
                "gspro bridge: sending shot #{} failed, not retried",
                shot.shot_number,
            );
            return Err(e);
        }
        on_sent(msg.shot_number);
        sent = true;
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use tokio::sync::broadcast;

    use super::*;

    /// Longest any test waits for one thing before failing.
    const WAIT: Duration = Duration::from_secs(5);

    fn key(n: u32) -> ShotKey {
        ShotKey {
            shot_id: format!("shot-{n}"),
            shot_number: n,
        }
    }

    /// Put a complete shot from `mevo.0` on the bus: trigger, the
    /// mapper's golden launch-only ball, finished.
    fn emit_shot(monitor: &BusSender, n: u32) {
        let ball = mapper::tests::square_like_shot(None)
            .ball
            .expect("golden shot has ball data");
        monitor.send(FlighthookMessage::new(FlighthookEvent::ShotTrigger {
            key: key(n),
        }));
        monitor.send(FlighthookMessage::new(FlighthookEvent::BallFlight {
            key: key(n),
            ball: Box::new(ball),
            derived: None,
        }));
        monitor.send(FlighthookMessage::new(FlighthookEvent::ShotFinished {
            key: key(n),
            raw: None,
        }));
    }

    /// The golden JSON for the shot `emit_shot(_, n)` puts on the bus.
    fn golden(n: u32) -> String {
        mapper::tests::GOLDEN_RH.replace("\"ShotNumber\":12,", &format!("\"ShotNumber\":{n},"))
    }

    // -- Relay (pure) -------------------------------------------------------

    fn relay_with_bus() -> (Relay, BusSender, BusReceiver) {
        let (tx, _) = broadcast::channel(64);
        let monitor = BusSender::new("mevo.0".into(), tx.clone(), Arc::default());
        let receiver =
            BusSender::new("openconnect_client.0".into(), tx, Arc::default()).subscribe();
        (Relay::new(GsProRouting::default()), monitor, receiver)
    }

    fn sent_numbers(relay: &mut Relay, now: Instant) -> Vec<u32> {
        std::iter::from_fn(|| relay.next_shot(now))
            .map(|s| s.shot_number)
            .collect()
    }

    #[test]
    fn shots_finished_in_one_drain_are_all_queued_in_order() {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connect(Instant::now());
        for n in 1..=3 {
            emit_shot(&monitor, n);
        }
        relay.drain(&mut receiver).expect("bus open");
        assert_eq!(sent_numbers(&mut relay, Instant::now()), [1, 2, 3]);
    }

    /// One device's increasing numbers, gaps included, go out unchanged.
    #[test]
    fn an_increasing_sequence_keeps_its_shot_numbers() {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connect(Instant::now());
        for n in [1, 2, 5, 6, 40] {
            emit_shot(&monitor, n);
        }
        relay.drain(&mut receiver).expect("bus open");
        assert_eq!(sent_numbers(&mut relay, Instant::now()), [1, 2, 5, 6, 40]);
    }

    /// Device #1, then Auto-chip #1, then device #2 go out as 1, 2, 3: a
    /// repeated or lower number is bumped past the last one sent.
    #[test]
    fn an_auto_chip_between_device_shots_never_repeats_a_shot_number() {
        let (tx, _) = broadcast::channel(64);
        let monitor = BusSender::new("mevo.0".into(), tx.clone(), Arc::default());
        let chip = BusSender::new(
            flighthook::AUTO_CHIP_ACTOR.into(),
            tx.clone(),
            Arc::default(),
        );
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx, Arc::default()).subscribe();
        let mut relay = Relay::new(GsProRouting::default());
        relay.connect(Instant::now());
        emit_shot(&monitor, 1);
        emit_shot(&chip, 1);
        emit_shot(&monitor, 2);
        relay.drain(&mut receiver).expect("bus open");
        let now = Instant::now();
        let sent: Vec<(String, u32)> = std::iter::from_fn(|| relay.next_shot(now))
            .map(|s| (s.actor, s.shot_number))
            .collect();
        assert_eq!(
            sent,
            [
                ("mevo.0".to_owned(), 1),
                (flighthook::AUTO_CHIP_ACTOR.to_owned(), 2),
                ("mevo.0".to_owned(), 3),
            ]
        );
    }

    /// A device whose count starts again (it reconnected), with GSPro
    /// reconnecting too, still gets increasing numbers.
    #[test]
    fn a_device_count_reset_after_a_reconnect_still_increases() {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connect(Instant::now());
        for n in 1..=3 {
            emit_shot(&monitor, n);
        }
        relay.drain(&mut receiver).expect("bus open");
        assert_eq!(sent_numbers(&mut relay, Instant::now()), [1, 2, 3]);
        relay.disconnect();
        relay.connect(Instant::now());
        for n in 1..=4 {
            emit_shot(&monitor, n);
        }
        relay.drain(&mut receiver).expect("bus open");
        assert_eq!(sent_numbers(&mut relay, Instant::now()), [4, 5, 6, 7]);
    }

    /// An Auto-chip goes out whatever monitor each mode is routed to, as a
    /// straight 28 deg chip with its solved speed, speed-scaled backspin, no
    /// sidespin, no carry and zeroed club data; under the same routing a
    /// shot from another monitor is still skipped.
    #[test]
    fn an_auto_chip_is_sent_whatever_the_routing() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-gspro-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let (tx, _) = broadcast::channel(64);
        let other = BusSender::new("square.0".into(), tx.clone(), Arc::default());
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx.clone(), Arc::default()).subscribe();
        let mevo = Some("mevo.0".to_owned());
        let mut relay = Relay::new(GsProRouting {
            full_monitor: mevo.clone(),
            chipping_monitor: mevo.clone(),
            putting_monitor: mevo,
        });
        relay.connect(Instant::now());
        let modes = [
            ShotDetectionMode::Full,
            ShotDetectionMode::Chipping,
            ShotDetectionMode::Putting,
        ];
        let chips = std::sync::atomic::AtomicU32::new(0);
        for (n, mode) in (1..).zip(modes) {
            relay.mode = mode;
            let (key, ball) = crate::autochip::send(&state, &tx, 3, &chips).expect("solved");
            assert_eq!(key.shot_number, n);
            emit_shot(&other, n);
            relay.drain(&mut receiver).expect("bus open");
            let shot = relay.next_shot(Instant::now()).expect("auto-chip sent");
            assert!(relay.next_shot(Instant::now()).is_none(), "{mode:?}");
            assert_eq!(shot.actor, flighthook::AUTO_CHIP_ACTOR);
            assert_eq!(shot.shot_number, key.shot_number);
            assert!(shot.club.is_none());

            let msg = mapper::map_shot(&shot);
            let speed = ball.launch_speed.expect("speed").as_mph();
            assert!((msg.ball_data.speed - speed).abs() < 1e-9);
            assert!(speed > 5.0 && speed < 12.0, "{speed}");
            assert_eq!(msg.ball_data.vla, 28.0);
            assert_eq!(msg.ball_data.hla, 0.0);
            assert_eq!(msg.ball_data.back_spin, (100.0 * speed).round());
            assert_eq!(msg.ball_data.side_spin, 0.0);
            assert_eq!(msg.ball_data.spin_axis, 0.0);
            assert_eq!(msg.ball_data.carry_distance, None);
            assert_eq!(msg.club_data.speed, 0.0);
            assert_eq!(msg.club_data.loft, 0.0);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Omni for full swings and chips, ExPutt for putts: each mode sends
    /// only its own monitor's shot, and readiness follows that monitor.
    #[test]
    fn full_shots_and_putts_come_from_their_own_monitors() {
        let (tx, _) = broadcast::channel(64);
        let omni = BusSender::new("square.0".into(), tx.clone(), Arc::default());
        let exputt = BusSender::new("exputt.0".into(), tx.clone(), Arc::default());
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx, Arc::default()).subscribe();
        let square = Some("square.0".to_owned());
        let routing = GsProRouting {
            full_monitor: square.clone(),
            chipping_monitor: square,
            putting_monitor: Some("exputt.0".to_owned()),
        };
        let mut relay = Relay::new(routing.clone());
        relay.connect(Instant::now());
        let modes = [
            (ShotDetectionMode::Full, "square.0"),
            (ShotDetectionMode::Chipping, "square.0"),
            (ShotDetectionMode::Putting, "exputt.0"),
        ];
        for (n, (mode, from)) in (1..).zip(modes) {
            relay.mode = mode;
            emit_shot(&omni, 2 * n);
            emit_shot(&exputt, 2 * n + 1);
            relay.drain(&mut receiver).expect("bus open");
            let sent: Vec<String> = std::iter::from_fn(|| relay.next_shot(Instant::now()))
                .map(|s| s.actor)
                .collect();
            assert_eq!(sent, [from], "{mode:?}");
        }

        let states = HashMap::from([
            ("square.0".to_owned(), false),
            ("exputt.0".to_owned(), true),
        ]);
        assert!(readiness(&routing, ShotDetectionMode::Putting, &states));
        assert!(!readiness(&routing, ShotDetectionMode::Full, &states));
    }

    #[test]
    fn an_exputt_with_putting_routed_to_any_is_warned_about() {
        let mut config = flighthook::FlighthookConfig::default();
        let any = GsProRouting::default();
        assert_eq!(putting_routing_warning(&config, &any), None, "no ExPutt");
        config.exputt.insert(
            "0".into(),
            flighthook::ExPuttSection {
                name: "ExPutt".into(),
                address: None,
            },
        );
        assert!(putting_routing_warning(&config, &any).is_some());
        let routed = GsProRouting {
            putting_monitor: Some("exputt.0".into()),
            ..GsProRouting::default()
        };
        assert_eq!(putting_routing_warning(&config, &routed), None);
    }

    /// An ExPutt shot in Full mode goes out while the Full Monitor accepts
    /// any device, and is dropped when it names another device.
    #[test]
    fn an_exputt_shot_in_full_mode_follows_the_full_monitor() {
        let (tx, _) = broadcast::channel(64);
        let exputt = BusSender::new("exputt.0".into(), tx.clone(), Arc::default());
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx, Arc::default()).subscribe();
        for (n, (full_monitor, sent)) in
            (1..).zip([(None, true), (Some("square.0".to_owned()), false)])
        {
            let mut relay = Relay::new(GsProRouting {
                full_monitor,
                chipping_monitor: None,
                putting_monitor: Some("exputt.0".to_owned()),
            });
            relay.connect(Instant::now());
            relay.mode = ShotDetectionMode::Full;
            emit_shot(&exputt, n);
            relay.drain(&mut receiver).expect("bus open");
            let actors: Vec<String> = std::iter::from_fn(|| relay.next_shot(Instant::now()))
                .map(|s| s.actor)
                .collect();
            assert_eq!(!actors.is_empty(), sent, "{:?}", relay.routing);
        }
    }

    #[test]
    fn shots_finished_while_disconnected_are_dropped() {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        emit_shot(&monitor, 1);
        relay.drain(&mut receiver).expect("bus open");
        relay.connect(Instant::now());
        emit_shot(&monitor, 2);
        relay.drain(&mut receiver).expect("bus open");
        assert_eq!(sent_numbers(&mut relay, Instant::now()), [2]);
    }

    #[test]
    fn a_shot_older_than_the_stale_limit_is_dropped() {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connect(Instant::now());
        emit_shot(&monitor, 1);
        emit_shot(&monitor, 2);
        relay.drain(&mut receiver).expect("bus open");
        let finished = relay.outbox[0].finished_at;
        relay.outbox[1].finished_at = finished + Duration::from_secs(1);
        // Shot 1 is just past the limit, shot 2 one second younger.
        let now = finished + STALE_SHOT + Duration::from_millis(1);
        assert_eq!(sent_numbers(&mut relay, now), [2]);
    }

    #[test]
    fn a_shot_at_the_stale_limit_is_sent() {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connect(Instant::now());
        emit_shot(&monitor, 1);
        relay.drain(&mut receiver).expect("bus open");
        let now = relay.outbox[0].finished_at + STALE_SHOT;
        assert_eq!(sent_numbers(&mut relay, now), [1]);
    }

    /// A socket whose every write times out.
    struct TimedOutSocket;

    impl Write for TimedOutSocket {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(ErrorKind::TimedOut.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Split the bytes the bridge wrote into its JSON messages.
    fn messages(wire: &[u8]) -> Vec<String> {
        let mut it = serde_json::Deserializer::from_slice(wire).into_iter::<serde_json::Value>();
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(value) = it.next() {
            value.expect("valid JSON");
            out.push(String::from_utf8(wire[start..it.byte_offset()].to_vec()).expect("utf-8"));
            start = it.byte_offset();
        }
        out
    }

    /// Shots 1-3 finish while connected and the write of shot 1 times out;
    /// shot 4 finishes during the outage; GSPro is back `reconnect_after`
    /// the shots finished. Returns what the bridge writes on the new
    /// connection.
    fn sent_after_failed_write(reconnect_after: Duration) -> Vec<String> {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connect(Instant::now());
        for n in 1..=3 {
            emit_shot(&monitor, n);
        }
        relay.drain(&mut receiver).expect("bus open");
        let finished = relay.outbox[0].finished_at;
        for pending in &mut relay.outbox {
            pending.finished_at = finished;
        }

        let failed = send_shots(
            &mut relay,
            &mut TimedOutSocket,
            finished,
            "openconnect_client.0",
            &mut |_| {},
        );
        assert!(
            matches!(failed, Err(BridgeError::Io(ref e)) if e.kind() == ErrorKind::TimedOut),
            "a timed-out write ends the connection: {failed:?}"
        );
        assert!(relay.disconnect().is_some());

        emit_shot(&monitor, 4);
        relay.drain(&mut receiver).expect("bus open");

        let back = finished + reconnect_after;
        relay.connect(back);
        let mut wire = Vec::new();
        send_shots(
            &mut relay,
            &mut wire,
            back,
            "openconnect_client.0",
            &mut |_| {},
        )
        .expect("writes to a Vec succeed");
        assert!(relay.outbox.is_empty());
        messages(&wire)
    }

    /// The bytes the bridge writes for one shot, with the hand set to
    /// `handed` first and the devices told so.
    fn sent_for(handed: flighthook::Handedness) -> Vec<String> {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connect(Instant::now());
        monitor.send(FlighthookMessage::new(FlighthookEvent::HandInfo {
            hand_info: flighthook::HandInfo { handed },
        }));
        emit_shot(&monitor, 1);
        relay.drain(&mut receiver).expect("bus open");
        let mut wire = Vec::new();
        send_shots(
            &mut relay,
            &mut wire,
            Instant::now(),
            "openconnect_client.0",
            &mut |_| {},
        )
        .expect("writes to a Vec succeed");
        messages(&wire)
    }

    /// The bus carries absolute signs for both hands and GSPro takes them as
    /// they are, so a left-handed player's shot goes out exactly like a
    /// right-handed one's (HLA -1.2, SideSpin -300 in the golden shot).
    #[test]
    fn a_left_handed_players_shot_is_sent_like_a_right_handed_ones() {
        let rh = sent_for(flighthook::Handedness::Right);
        let lh = sent_for(flighthook::Handedness::Left);
        assert_eq!(rh.len(), 1);
        assert_eq!(rh[0].trim_end(), golden(1));
        assert_eq!(lh, rh);
    }

    #[test]
    fn after_a_failed_write_a_quick_reconnect_sends_the_queued_shots() {
        let sent = sent_after_failed_write(STALE_SHOT - Duration::from_millis(100));
        assert_eq!(sent, [golden(2), golden(3)]);
    }

    #[test]
    fn after_a_failed_write_a_slow_reconnect_drops_the_queued_shots() {
        let sent = sent_after_failed_write(STALE_SHOT + Duration::from_millis(100));
        assert!(sent.is_empty(), "{sent:?}");
    }

    // -- Bridge against a fake GSPro ---------------------------------------

    /// A running bridge (`openconnect_client.0`) wired to a fake GSPro listener it dials.
    struct Bridge {
        gspro: TcpListener,
        monitor: BusSender,
        events: broadcast::Receiver<FlighthookMessage>,
        shutdown: Arc<AtomicBool>,
        done: mpsc::Receiver<()>,
    }

    impl Bridge {
        fn start(backoff: Backoff) -> Self {
            let gspro = TcpListener::bind("127.0.0.1:0").expect("bind fake GSPro");
            gspro.set_nonblocking(true).expect("nonblocking listener");
            let addr = gspro.local_addr().expect("listener addr");
            let (tx, events) = broadcast::channel(1024);
            let shutdown = Arc::new(AtomicBool::new(false));
            let sender = BusSender::new(
                "openconnect_client.0".into(),
                tx.clone(),
                Arc::clone(&shutdown),
            );
            let receiver = sender.subscribe();
            let monitor = BusSender::new("mevo.0".into(), tx, Arc::default());
            let (done_tx, done) = mpsc::channel();
            std::thread::spawn(move || {
                run_with(
                    addr,
                    GsProRouting::default(),
                    backoff,
                    &LocalClients::default(),
                    &sender,
                    receiver,
                );
                let _ = done_tx.send(());
            });
            Self {
                gspro,
                monitor,
                events,
                shutdown,
                done,
            }
        }

        /// Accept the bridge's next connection.
        fn accept(&self) -> GsPro {
            let deadline = Instant::now() + WAIT;
            loop {
                match self.gspro.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).expect("blocking stream");
                        stream
                            .set_read_timeout(Some(Duration::from_millis(50)))
                            .expect("read timeout");
                        return GsPro {
                            stream,
                            buf: Vec::new(),
                        };
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "bridge never connected");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
        }

        /// Wait for a bus event from `openconnect_client.0` matching `want`; returns every
        /// `openconnect_client.0` alert seen on the way.
        fn wait_for(&mut self, what: &str, want: impl Fn(&FlighthookEvent) -> bool) -> Vec<String> {
            let deadline = Instant::now() + WAIT;
            let mut alerts = Vec::new();
            loop {
                match self.events.try_recv() {
                    Ok(msg) if msg.actor == "openconnect_client.0" => {
                        if let FlighthookEvent::Alert { ref message, .. } = msg.event {
                            alerts.push(message.clone());
                        }
                        if want(&msg.event) {
                            return alerts;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::TryRecvError::Empty) => {
                        assert!(Instant::now() < deadline, "timed out waiting for {what}");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("bus: {e}"),
                }
            }
        }

        /// Every `openconnect_client.0` alert already on the bus, without waiting.
        fn alerts_so_far(&mut self) -> Vec<String> {
            let mut alerts = Vec::new();
            while let Ok(msg) = self.events.try_recv() {
                if let (true, FlighthookEvent::Alert { message, .. }) =
                    (msg.actor == "openconnect_client.0", msg.event)
                {
                    alerts.push(message);
                }
            }
            alerts
        }

        fn wait_for_status(&mut self, status: ActorStatus) -> Vec<String> {
            self.wait_for(
                &format!("{status:?}"),
                |e| matches!(e, FlighthookEvent::ActorStatus { status: s, .. } if *s == status),
            )
        }

        /// Set the shutdown flag and wait (bounded) for the bridge to return.
        fn stop(&self) -> Result<(), mpsc::RecvTimeoutError> {
            self.shutdown.store(true, Ordering::Relaxed);
            self.done.recv_timeout(Duration::from_secs(2))
        }
    }

    impl Drop for Bridge {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
        }
    }

    /// The fake GSPro's end of one bridge connection.
    struct GsPro {
        stream: TcpStream,
        buf: Vec<u8>,
    }

    impl GsPro {
        /// The next `count` shot messages (heartbeats skipped), as the raw
        /// JSON text the bridge wrote.
        fn shots(&mut self, count: usize) -> Vec<String> {
            let deadline = Instant::now() + WAIT;
            let mut shots = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let mut it = serde_json::Deserializer::from_slice(&self.buf)
                    .into_iter::<serde_json::Value>();
                let mut consumed = 0;
                while let Some(Ok(value)) = it.next() {
                    let raw = &self.buf[consumed..it.byte_offset()];
                    consumed = it.byte_offset();
                    if value["ShotDataOptions"]["IsHeartBeat"] == false {
                        shots.push(String::from_utf8(raw.to_vec()).expect("utf-8"));
                    }
                }
                self.buf.drain(..consumed);
                if shots.len() >= count {
                    return shots;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {count} shots, got {shots:?}"
                );
                match self.stream.read(&mut chunk) {
                    Ok(0) => panic!("bridge closed the connection"),
                    Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(e) => panic!("read: {e}"),
                }
            }
        }
    }

    fn is_shot_alert(message: &str) -> bool {
        message.to_lowercase().contains("shot")
    }

    #[test]
    fn shots_right_after_connect_are_all_sent_in_order_with_unchanged_json() {
        let mut bridge = Bridge::start(Backoff::new(MIN_BACKOFF, MAX_BACKOFF));
        let mut gspro = bridge.accept();
        bridge.wait_for_status(ActorStatus::Connected);
        for n in [12, 13, 14] {
            emit_shot(&bridge.monitor, n);
        }
        let shots = gspro.shots(3);
        assert_eq!(shots, [golden(12), golden(13), golden(14)]);
        bridge.stop().expect("bridge stops");
    }

    #[test]
    fn shots_while_disconnected_are_not_sent_and_raise_no_alert() {
        // A 3 s backoff keeps the bridge disconnected while the shots finish.
        let mut bridge = Bridge::start(Backoff::new(MAX_BACKOFF, MAX_BACKOFF));
        let first = bridge.accept();
        bridge.wait_for_status(ActorStatus::Connected);
        drop(first);
        let mut alerts = bridge.wait_for_status(ActorStatus::Reconnecting);
        emit_shot(&bridge.monitor, 1);
        emit_shot(&bridge.monitor, 2);

        let mut second = bridge.accept();
        alerts.extend(bridge.wait_for_status(ActorStatus::Connected));
        emit_shot(&bridge.monitor, 3);
        assert_eq!(second.shots(1), [golden(3)]);
        bridge.stop().expect("bridge stops");
        alerts.extend(bridge.alerts_so_far());

        assert!(
            !alerts.iter().any(|a| is_shot_alert(a)),
            "dropped shots must not raise an alert: {alerts:?}"
        );
    }

    /// The client's card telemetry, merged from its statuses as the web
    /// layer merges them, until `done` holds for it.
    fn telemetry_until(
        bridge: &mut Bridge,
        card: &mut HashMap<String, String>,
        what: &str,
        done: impl Fn(&HashMap<String, String>) -> bool,
    ) {
        let deadline = Instant::now() + WAIT;
        while !done(card) {
            match bridge.events.try_recv() {
                Ok(msg) if msg.actor == "openconnect_client.0" => {
                    if let FlighthookEvent::ActorStatus { telemetry, .. } = msg.event {
                        for (k, v) in telemetry {
                            if v.is_empty() {
                                card.remove(&k);
                            } else {
                                card.insert(k, v);
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for {what}: {card:?}"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("bus: {e}"),
            }
        }
    }

    /// The card shows the simulator's address, the readiness sent, the
    /// simulator's last response, the shots sent on this connection and the
    /// last one; when the connection ends, what held for it goes, the
    /// player information included, and the address and last shot stay.
    #[test]
    fn the_card_shows_the_connection_and_its_shots() {
        let mut bridge = Bridge::start(Backoff::new(MAX_BACKOFF, MAX_BACKOFF));
        let addr = bridge.gspro.local_addr().expect("addr").to_string();
        let mut gspro = bridge.accept();
        let mut card = HashMap::new();
        telemetry_until(&mut bridge, &mut card, "connected", |c| {
            c.get(SHOTS_SENT).map(String::as_str) == Some("0")
        });
        assert_eq!(card[ADDRESS], addr);
        assert!(!card.contains_key(LAST_SHOT));

        gspro
            .stream
            .write_all(
                br#"{"Code":201,"Message":"GSPro Player Information","Player":{"Handed":"RH","Club":"DR"}}"#,
            )
            .expect("player info");
        telemetry_until(&mut bridge, &mut card, "the response", |c| {
            c.contains_key(RESPONSE)
        });
        assert_eq!(card[RESPONSE], "201 GSPro Player Information");

        emit_shot(&bridge.monitor, 12);
        assert_eq!(gspro.shots(1), [golden(12)]);
        telemetry_until(&mut bridge, &mut card, "the shot", |c| {
            c.get(SHOTS_SENT).map(String::as_str) == Some("1")
        });
        let last = &card[LAST_SHOT];
        assert!(last.starts_with("#12 at "), "{last}");
        let time = last.trim_start_matches("#12 at ");
        assert_eq!(time.len(), 8, "{last}");
        assert!(
            chrono::NaiveTime::parse_from_str(time, "%H:%M:%S").is_ok(),
            "{last}"
        );
        telemetry_until(&mut bridge, &mut card, "the readiness", |c| {
            c.contains_key(MONITOR_READY)
        });
        assert_eq!(card[MONITOR_READY], "false");

        // The player information shows (web layer) until the connection ends.
        card.insert("club".into(), "DR".into());
        card.insert("handed".into(), "rh".into());
        drop(gspro);
        telemetry_until(&mut bridge, &mut card, "the connection's keys gone", |c| {
            !c.contains_key(SHOTS_SENT)
        });
        for key in PER_CONNECTION.into_iter().chain(PLAYER) {
            assert!(!card.contains_key(key), "{key}: {card:?}");
        }
        assert_eq!(card[ADDRESS], addr);
        assert!(card[LAST_SHOT].starts_with("#12 at "));
        bridge.stop().expect("bridge stops");
    }

    #[test]
    fn telemetry_goes_out_only_when_it_changes() {
        let mut t = Telemetry::new("127.0.0.1:921".parse().expect("addr"));
        let first = t.take();
        assert_eq!(first[ADDRESS], "127.0.0.1:921");
        assert!(first[SHOTS_SENT].is_empty(), "an earlier instance's go");
        t.set(SHOTS_SENT, "1".into());
        t.set(SHOTS_SENT, "1".into());
        assert_eq!(t.take(), HashMap::from([(SHOTS_SENT.into(), "1".into())]));
        t.set(SHOTS_SENT, "1".into());
        assert!(t.take().is_empty());
        t.set(SHOTS_SENT, "2".into());
        t.set(SHOTS_SENT, "1".into());
        assert!(t.take().is_empty(), "back where it was");
        t.clear_connection();
        assert_eq!(
            t.take(),
            HashMap::from([(SHOTS_SENT.into(), String::new())])
        );
        t.clear_connection();
        assert!(t.take().is_empty());
    }

    #[test]
    fn shutdown_during_backoff_returns_promptly() {
        let mut bridge = Bridge::start(Backoff::new(
            Duration::from_secs(60),
            Duration::from_secs(60),
        ));
        let gspro = bridge.accept();
        bridge.wait_for_status(ActorStatus::Connected);
        drop(gspro);
        bridge.wait_for_status(ActorStatus::Reconnecting);
        let asked = Instant::now();
        bridge.stop().expect("bridge returns during a 60 s backoff");
        assert!(asked.elapsed() < Duration::from_secs(1));
    }
}
