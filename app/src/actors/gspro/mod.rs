//! GSPro Open Connect V1 client: the `openconnect_client` actor.
//!
//! Subscribes to the unified bus for shot data and forwards it over TCP to a
//! simulator that accepts Open Connect: GSPro and Pureplay (port 921), Muni
//! Golf (9210), or any other. The simulator's player information (club and
//! hand) becomes one complete `golfer.select` from this card's actor, with
//! the mode the card's club lists give (both lists empty: no mode); the card
//! sends it again whenever flighthook's turn differs from it, unless the
//! change came from another game or simulator card ([`Held`]). The card's
//! status goes out as a local card status. Reconnects with linear backoff
//! on failure.
//!
//! The bus is drained the whole time, connected or not. Every shot that
//! finishes while the simulator is connected is sent, in order; a shot that
//! finishes while it is not, or is older than [`STALE_SHOT`] when the bridge
//! could send it, is dropped (debug log only: a simulator not running is
//! normal).
//!
//! A simulator source (the Pureplay card) feeds no simulator output: its
//! shots and readiness never reach the simulator, under "Any" routing or
//! routed by ID ([`feeds_simulators`]). Each shot is recorded in the send
//! registry ([`SentShots`]) just before it is written, so the Pureplay card
//! can tell the shot Pureplay then reports from one of its own; a write
//! that fails takes its record back.
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

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::backoff::Backoff;
use super::simout::OutputShot;
use super::simout::{Feed, FeedEvent, Held, output_shot};
use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusMsg, BusReceiver, BusSender, LocalEvent, PollError};
use crate::state::SystemState;
use crate::state::local_clients::LocalClients;
use crate::state::sent_shots::{SentShots, SimAddr};
use flighthook::{ActorStatus, Club, Handedness, Mode, ShotKey};

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

impl GsProRouting {
    /// The launch monitors whose shots this routing passes in any detection
    /// mode, by actor ID: each mode's routed monitor, and for a mode routed
    /// to "Any" every monitor in `any`.
    pub fn feeds<'a>(&'a self, any: BTreeSet<&'a str>) -> BTreeSet<&'a str> {
        let mut feeds = BTreeSet::new();
        for routed in [
            &self.full_monitor,
            &self.chipping_monitor,
            &self.putting_monitor,
        ] {
            match routed {
                Some(id) => {
                    feeds.insert(id.as_str());
                }
                None => feeds.extend(any.iter().copied()),
            }
        }
        feeds
    }
}

/// Open Connect client actor (`openconnect_client.N`). Connects to a
/// simulator that accepts GSPro Open Connect V1 (GSPro and Pureplay on port
/// 921, Muni Golf on 9210; TCP, JSON), forwards shot data, and handles heartbeats.
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
        let registry = Registry {
            state: Some(Arc::clone(&state)),
            sent: Some(Arc::clone(&state.sent_shots)),
        };
        let thread_name = format!("gspro:{}", sender.actor_id());
        if let Some(warning) = putting_routing_warning(&state.system.snapshot(), &routing) {
            tracing::warn!("{}: {warning}", sender.actor_id());
        }

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(addr, routing, registry, &clients, sender, receiver))
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

/// Why the simulator refused the connection.
const ERROR: &str = "error";
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
/// The simulator's player information: the club and hand the card holds.
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

    /// Show the club and hand the card holds (`held`) on its card.
    fn player(&mut self, held: &Held) {
        self.set("club", held.club.map(|c| c.to_string()).unwrap_or_default());
        self.set(
            "handed",
            held.hand.map(|h| h.to_string()).unwrap_or_default(),
        );
    }

    /// Remove what held for the connection that ended.
    fn clear_connection(&mut self) {
        for key in PER_CONNECTION.into_iter().chain(PLAYER) {
            self.set(key, String::new());
        }
    }

    /// The changes not yet sent plus every other value the card holds, now
    /// taken as sent. A status that starts a new instance or ends a
    /// connection drops what the card held, so it carries the values that
    /// outlast the connection (the address, the last shot, a refusal).
    fn take_all(&mut self) -> HashMap<String, String> {
        let mut update = self.take();
        for (k, v) in &self.reported {
            update.entry(k.clone()).or_insert_with(|| v.clone());
        }
        update
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
    #[cfg(debug_assertions)]
    if let Some(clock) = crate::screens::clock() {
        return format!("#{number} at {clock}");
    }
    format!("#{number} at {}", chrono::Local::now().format("%H:%M:%S"))
}

/// What the client reads from, and writes to, the rest of the app.
#[derive(Clone, Default)]
pub(crate) struct Registry {
    /// The app's state: actor ids, shot sources, the card's club lists.
    /// `None` in tests that run without one.
    pub state: Option<Arc<SystemState>>,
    /// Where each shot is recorded before it is written; `None` records
    /// nothing.
    pub sent: Option<Arc<SentShots>>,
}

/// Main bridge loop. Reconnects forever until the bus closes.
fn run(
    addr: SocketAddr,
    routing: GsProRouting,
    registry: Registry,
    clients: &LocalClients,
    sender: BusSender,
    receiver: BusReceiver,
) {
    run_shared(
        addr,
        routing,
        registry,
        Backoff::new(MIN_BACKOFF, MAX_BACKOFF),
        clients,
        &sender,
        receiver,
    );
}

/// [`run_shared`] with no shot source registered and no send registry.
#[cfg(test)]
pub(crate) fn run_with(
    addr: SocketAddr,
    routing: GsProRouting,
    backoff: Backoff,
    clients: &LocalClients,
    sender: &BusSender,
    receiver: BusReceiver,
) {
    run_shared(
        addr,
        routing,
        Registry::default(),
        backoff,
        clients,
        sender,
        receiver,
    );
}

pub(crate) fn run_shared(
    addr: SocketAddr,
    routing: GsProRouting,
    registry: Registry,
    mut backoff: Backoff,
    clients: &LocalClients,
    sender: &BusSender,
    mut receiver: BusReceiver,
) {
    let mut relay = Relay::new(routing, registry, sender.actor_id());
    // The actors already up republish their state (launch monitors' blockers,
    // flighthook's turn) for this card.
    sender.frp(flightrelay::BusSync::new());
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
        if !ever_connected {
            telemetry.set(ERROR, String::new());
        }
        let mut update = telemetry.take_all();
        if !ever_connected {
            update.extend(clear_error());
        }
        sender.local(LocalEvent::CardStatus {
            status,
            telemetry: update,
        });

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
                match e {
                    BridgeError::Refused(reason) => {
                        refused = true;
                        telemetry.set(ERROR, reason);
                    }
                    _ if std::mem::take(&mut refused) => telemetry.set(ERROR, String::new()),
                    _ => {}
                }
                let update = telemetry.take_all();
                tracing::info!("gspro bridge: {text}, retrying in {delay:?}");
                sender.local(LocalEvent::CardStatus {
                    status: backoff_status,
                    telemetry: update,
                });

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

/// A finished shot waiting to be sent, with its key as the bus carried it.
struct PendingShot {
    key: ShotKey,
    shot: OutputShot,
    finished_at: Instant,
}

/// Bus-side bridge state, kept across reconnects so the bus is drained the
/// whole time the actor runs, connected or not.
struct Relay {
    /// Shots, readiness and flighthook's turn from the bus.
    feed: Feed,
    /// The app's state and the send registry.
    registry: Registry,
    /// The card key (`openconnect_client.N`).
    card: String,
    /// Where the simulator is while connected: where a shot is recorded
    /// as sent to.
    peer: Option<SimAddr>,
    /// When GSPro connected, `None` while it is not. Shots that finish while
    /// it is not are dropped.
    connected_at: Option<Instant>,
    /// What the simulator's player information gave since it connected.
    held: Held,
    /// The card is to send its select again (a turn differs from it).
    reassert: bool,
    /// Shots finished while connected, oldest first.
    outbox: VecDeque<PendingShot>,
    /// The `ShotNumber` handed out last ([`Relay::next_shot`]), 0 before the
    /// first. Kept for the actor's lifetime, across reconnects.
    last_number: u32,
}

impl Relay {
    fn new(routing: GsProRouting, registry: Registry, card: &str) -> Self {
        Self {
            feed: Feed::new(routing, registry.state.clone()),
            registry,
            card: card.to_owned(),
            peer: None,
            connected_at: None,
            held: Held::default(),
            reassert: false,
            outbox: VecDeque::new(),
            last_number: 0,
        }
    }

    /// Mark GSPro connected at `now`.
    fn connect(&mut self, now: Instant) {
        self.connected_at = Some(now);
    }

    /// Mark GSPro not connected. Returns how long the connection lasted,
    /// `None` if it was not connected. What the card held goes with it.
    fn disconnect(&mut self) -> Option<Duration> {
        self.peer = None;
        self.held = Held::default();
        self.reassert = false;
        self.connected_at.take().map(|at| at.elapsed())
    }

    /// Record shot `key` in the send registry as written to the simulator
    /// at `now`; nothing without a registry or a connection.
    fn record_send(&self, key: ShotKey, now: Instant) {
        if let (Some(sent), Some(peer)) = (&self.registry.sent, self.peer) {
            sent.record(key, peer, now);
        }
    }

    /// Take shot `key`'s record back: its write failed.
    fn forget_send(&self, key: &ShotKey) {
        if let (Some(sent), Some(peer)) = (&self.registry.sent, self.peer) {
            sent.forget(key, peer);
        }
    }

    /// The card's mode for the simulator's `club`, from its club lists in
    /// the live config (`None`: it sends no mode).
    fn club_mode(&self, club: Option<Club>) -> Option<Mode> {
        let index = self.card.split_once('.').map_or("", |(_, i)| i);
        match &self.registry.state {
            Some(state) => state
                .system
                .snapshot()
                .openconnect_client
                .get(index)
                .and_then(|s| s.club_mode(club)),
            None => flighthook::OpenConnectClientSection::default().club_mode(club),
        }
    }

    /// The simulator's player information: the card now holds its club and
    /// hand (a hand it leaves out stays), with the mode its club lists give.
    fn hold(&mut self, hand: Option<Handedness>, club: Option<Club>) {
        if let Some(hand) = hand {
            self.held.hand = Some(hand);
        }
        if club.is_some() {
            self.held.club = club;
        }
        self.refresh_mode();
    }

    /// The held mode, from the held club and the club lists in the live
    /// config: none while the card holds no club.
    fn refresh_mode(&mut self) {
        self.held.mode = match self.held.club {
            Some(club) => self.club_mode(Some(club)),
            None => None,
        };
    }

    /// The name of launch monitor `actor` for the log: its card key, else
    /// the id.
    fn monitor_name(&self, actor: &str) -> String {
        self.registry
            .state
            .as_ref()
            .and_then(|s| s.card_of_actor(actor))
            .unwrap_or_else(|| actor.to_owned())
    }

    /// Whether `actor` is a game or simulator card.
    fn is_game_card(&self, actor: &str) -> bool {
        self.registry
            .state
            .as_ref()
            .is_some_and(|s| s.is_game_card(actor))
    }

    /// Process every message waiting on the bus.
    fn drain(&mut self, receiver: &mut BusReceiver) -> Result<(), BridgeError> {
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => return Err(BridgeError::Shutdown),
                Ok(None) => return Ok(()),
                Ok(Some(msg)) => self.handle(&msg, Instant::now()),
            }
        }
    }

    fn handle(&mut self, msg: &BusMsg, now: Instant) {
        match self.feed.handle(msg) {
            Some(FeedEvent::Shot(done)) => {
                let name = self.monitor_name(&done.shot.from);
                let key = done.shot.key.clone();
                if self.connected_at.is_some() {
                    let auto_chip = self
                        .registry
                        .state
                        .as_ref()
                        .map_or_else(|| "autochip".to_owned(), |s| s.auto_chip_id());
                    self.outbox.push_back(PendingShot {
                        key,
                        shot: output_shot(&done.shot, &name, &auto_chip),
                        finished_at: now,
                    });
                } else {
                    tracing::debug!(
                        "gspro bridge: GSPro not connected, dropping shot #{} from '{name}'",
                        key.shot_number,
                    );
                }
            }
            Some(FeedEvent::Skipped(shot)) => {
                let mode = self.feed.mode();
                tracing::debug!(
                    "gspro bridge: skipping shot #{} from '{}' (routed to {:?} for mode {mode:?})",
                    shot.key.shot_number,
                    self.monitor_name(&shot.from),
                    self.feed.routed(mode),
                );
            }
            Some(FeedEvent::NoData(shot)) => {
                tracing::info!(
                    "gspro bridge: shot #{} from '{}' has no data (no read), not sent",
                    shot.key.shot_number,
                    self.monitor_name(&shot.from),
                );
            }
            Some(FeedEvent::Turn) => {
                // The latest turn decides: one that matches what the card
                // holds cancels a re-assert an earlier one asked for.
                self.refresh_mode();
                self.reassert = self.feed.turn.as_ref().is_some_and(|(turn, cause)| {
                    self.connected_at.is_some()
                        && self
                            .held
                            .reasserts(turn, cause.as_deref(), |a| self.is_game_card(a))
                });
            }
            None => {}
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
    #[cfg(test)]
    fn next_shot(&mut self, now: Instant) -> Option<OutputShot> {
        self.next_send(now).map(|(_, shot)| shot)
    }

    /// [`next_shot`](Self::next_shot), with the shot's key as the bus
    /// carried it (its own shot number).
    fn next_send(&mut self, now: Instant) -> Option<(ShotKey, OutputShot)> {
        while let Some(pending) = self.outbox.pop_front() {
            let age = now.saturating_duration_since(pending.finished_at);
            if age <= STALE_SHOT {
                let key = pending.key;
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
                return Some((key, shot));
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
        std::mem::take(&mut self.feed.readiness_changed)
    }

    /// Whether the simulator is told a launch monitor is ready.
    fn ready(&self) -> bool {
        self.feed.ready(|_| true)
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
                 cannot recognize this connection"
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
    relay.peer = Some(SimAddr::of(
        stream.local_addr().ok(),
        stream.peer_addr().unwrap_or(addr),
    ));
    tracing::info!("gspro bridge: connected to {addr}");
    // Shots flow from now; the Connected status waits for the verdict.
    let mut announced = false;
    let mut shots_sent: u32 = 0;
    telemetry.set(SHOTS_SENT, shots_sent.to_string());

    // Backdate so the first heartbeat fires after ~1s instead of waiting the full 10s.
    let mut last_heartbeat = Instant::now() - Duration::from_secs(9);
    let mut read_buf = vec![0u8; 4096];
    let mut prev_readiness = false;
    let mut selects = 0u64;

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
                if let Some((hand, club)) = heard.info {
                    relay.hold(hand, club);
                    select(relay, sender, &mut selects);
                }
                if let Some((code, message)) = heard.last {
                    telemetry.set(RESPONSE, response_text(code, &message));
                }
                if heard.player || heard.info.is_some() {
                    telemetry.player(&relay.held);
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
        if std::mem::take(&mut relay.reassert) {
            select(relay, sender, &mut selects);
        }

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
            let new_readiness = relay.ready();
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
            let ready = relay.ready();
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
            telemetry.set(ERROR, String::new());
            let mut update = telemetry.take();
            update.extend(clear_error());
            sender.local(LocalEvent::CardStatus {
                status: ActorStatus::Connected,
                telemetry: update,
            });
        } else if announced && !telemetry.pending.is_empty() {
            sender.local(LocalEvent::CardStatus {
                status: ActorStatus::Connected,
                telemetry: telemetry.take(),
            });
        }

        if !activity {
            std::thread::sleep(IDLE_POLL);
        }
    }
}

/// Send the card's complete `golfer.select`: everything it holds.
fn select(relay: &Relay, sender: &BusSender, n: &mut u64) {
    if relay.held.is_empty() {
        return;
    }
    *n += 1;
    sender.frp(relay.held.select(format!("{}-{n}", relay.card)));
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
    while let Some((key, shot)) = relay.next_send(now) {
        let msg = mapper::map_shot(&shot);
        log_outbound(&msg);
        if let Ok(json_str) = serde_json::to_string(&msg) {
            tracing::info!(
                target: "audit",
                "{name} sent shot {json_str} | {msg:?}",
            );
        }
        relay.record_send(key.clone(), now);
        if let Err(e) = send_message(stream, &msg) {
            tracing::debug!(
                "gspro bridge: sending shot #{} failed, not retried",
                shot.shot_number,
            );
            relay.forget_send(&key);
            return Err(e);
        }
        on_sent(msg.shot_number);
        sent = true;
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use flighthook::ShotKey;
    use std::io::ErrorKind;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use flighthook::ActorStatusResponse;
    use tokio::sync::broadcast;

    use super::*;
    use crate::bus::LocalEvent;

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
        emit_shot_as(monitor, key(n));
    }

    /// [`emit_shot`] with the shot key `key`.
    fn emit_shot_as(monitor: &BusSender, key: ShotKey) {
        let ball = mapper::tests::square_like_shot()
            .launch
            .expect("golden shot has ball data");
        monitor.frp(flightrelay::ShotTrigger::new(key.clone()));
        monitor.frp(flightrelay::ShotLaunch {
            key: key.clone(),
            launch: ball,
        });
        monitor.frp(flightrelay::ShotFinished { key });
    }

    /// A sender for card `card` whose FRP actor id is the card key.
    fn actor(card: &str, tx: &broadcast::Sender<BusMsg>) -> BusSender {
        BusSender::new(card.into(), tx.clone(), Arc::default()).with_frp_id(Some(card.into()))
    }

    /// flighthook's turn in `mode` (right-handed, no club), caused by
    /// `cause`.
    fn turn(tx: &broadcast::Sender<BusMsg>, mode: Mode, club: Option<Club>, cause: Option<&str>) {
        let _ = tx.send(BusMsg::Frp {
            msg: flightrelay::FrpMessage::new(
                "flighthook",
                flightrelay::GolferTurn::new(None, club, Handedness::Right, mode),
            ),
            peer: None,
            cause: cause.map(str::to_owned),
        });
    }

    /// Launch monitor `id`'s `bus.actor`, ready or not.
    fn monitor_actor(tx: &broadcast::Sender<BusMsg>, id: &str, ready: bool) {
        let mut a = flightrelay::BusActor::new(id, 1.0)
            .with_role(flightrelay::types::roles::LAUNCH_MONITOR);
        if !ready {
            a.blockers.push("not_ready".into());
        }
        let _ = tx.send(BusMsg::frp(flightrelay::FrpMessage::new(id, a)));
    }

    /// The golden JSON for the shot `emit_shot(_, n)` puts on the bus.
    fn golden(n: u32) -> String {
        mapper::tests::GOLDEN_RH.replace("\"ShotNumber\":12,", &format!("\"ShotNumber\":{n},"))
    }

    // -- Relay (pure) -------------------------------------------------------

    fn relay_with_bus() -> (Relay, BusSender, BusReceiver) {
        let (tx, _) = broadcast::channel(64);
        let monitor = actor("mevo.0", &tx);
        let receiver =
            BusSender::new("openconnect_client.0".into(), tx, Arc::default()).subscribe();
        (
            Relay::new(
                GsProRouting::default(),
                Registry::default(),
                "openconnect_client.0",
            ),
            monitor,
            receiver,
        )
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
        let monitor = actor("mevo.0", &tx);
        let chip = actor(flighthook::AUTO_CHIP_ACTOR, &tx);
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx, Arc::default()).subscribe();
        let mut relay = Relay::new(
            GsProRouting::default(),
            Registry::default(),
            "openconnect_client.0",
        );
        relay.connect(Instant::now());
        emit_shot(&monitor, 1);
        emit_shot_as(&chip, ShotKey::new("chip-1", 1));
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
        // The device counts from 1 again; its new shots have new ids.
        for n in 1..=4 {
            emit_shot_as(
                &monitor,
                ShotKey {
                    shot_id: format!("again-{n}"),
                    shot_number: n,
                },
            );
        }
        relay.drain(&mut receiver).expect("bus open");
        assert_eq!(sent_numbers(&mut relay, Instant::now()), [4, 5, 6, 7]);
    }

    /// An Auto-chip goes out whatever monitor each mode is routed to, as a
    /// straight 45 deg chip with its solved speed, speed-scaled backspin, no
    /// sidespin, no carry and zeroed club data; under the same routing a
    /// shot from another monitor is still skipped.
    #[test]
    fn an_auto_chip_is_sent_whatever_the_routing() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-gspro-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let state = Arc::new(state);
        let (tx, _) = broadcast::channel(64);
        let other = actor("square.0", &tx);
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx.clone(), Arc::default()).subscribe();
        let mevo = Some("mevo.0".to_owned());
        let mut relay = Relay::new(
            GsProRouting {
                full_monitor: mevo.clone(),
                chipping_monitor: mevo.clone(),
                putting_monitor: mevo,
            },
            Registry {
                state: Some(Arc::clone(&state)),
                sent: None,
            },
            "openconnect_client.0",
        );
        relay.connect(Instant::now());
        let modes = [Mode::Full, Mode::Chipping, Mode::Putting];
        let chips = std::sync::atomic::AtomicU32::new(0);
        for (n, mode) in (1..).zip(modes) {
            let _ = tx.send(BusMsg::frp(flightrelay::FrpMessage::new(
                state.flighthook_id(),
                flightrelay::GolferTurn::new(None, None, Handedness::Right, mode),
            )));
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
            assert_eq!(msg.ball_data.vla, 45.0);
            assert_eq!(msg.ball_data.hla, 0.0);
            assert_eq!(msg.ball_data.back_spin, (300.0 * speed).round());
            assert_eq!(msg.ball_data.side_spin, 0.0);
            assert_eq!(msg.ball_data.spin_axis, 0.0);
            assert_eq!(msg.ball_data.carry_distance, None);
            assert_eq!(msg.club_data.speed, 0.0);
            assert_eq!(msg.club_data.loft, 0.0);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Tournament mode changes nothing GSPro receives: a launch-only chip
    /// (speed and angles measured, its spin the card's estimate) still goes
    /// out with the estimated spin, byte for byte as with the mode off.
    #[test]
    fn tournament_mode_leaves_a_launch_only_chips_estimated_spin_to_gspro() {
        use crate::actors::provenance::ShotSource;
        use flightrelay::types::{AssumedLaunch, Basis, FlightResult};
        let dir = std::env::temp_dir().join(format!(
            "flighthook-gspro-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let state = Arc::new(state);
        state.round.register_source("mevo.0", ShotSource::Mevo);
        let sent = |n: u32| {
            let (tx, _) = broadcast::channel(64);
            let mevo = actor("mevo.0", &tx);
            let mut receiver =
                BusSender::new("openconnect_client.0".into(), tx.clone(), Arc::default())
                    .subscribe();
            let mut relay = Relay::new(
                GsProRouting::default(),
                Registry {
                    state: Some(Arc::clone(&state)),
                    sent: None,
                },
                "openconnect_client.0",
            );
            relay.connect(Instant::now());
            let mut launch = mapper::tests::square_like_shot().launch.expect("ball data");
            launch.backspin_rpm = None;
            launch.sidespin_rpm = None;
            launch.total_spin_rpm = None;
            launch.spin_axis = None;
            let key = key(n);
            mevo.frp(flightrelay::ShotTrigger::new(key.clone()));
            mevo.frp(flightrelay::ShotLaunch {
                key: key.clone(),
                launch,
            });
            mevo.frp(flightrelay::ShotFlight {
                key: key.clone(),
                flight: FlightResult {
                    assumed: Some(AssumedLaunch {
                        backspin_rpm: Some(2000),
                        sidespin_rpm: Some(0),
                        ..AssumedLaunch::default()
                    }),
                    ..FlightResult::new(Basis::Modeled)
                },
            });
            mevo.frp(flightrelay::ShotFinished { key });
            relay.drain(&mut receiver).expect("bus open");
            let shot = relay.next_shot(Instant::now()).expect("sent to GSPro");
            serde_json::to_value(mapper::map_shot(&shot)).expect("json")
        };
        let off = sent(1);
        state.round.set_manual(true);
        assert!(state.round.active());
        let on = sent(1);
        assert_eq!(on["BallData"]["BackSpin"], 2000.0, "{on}");
        assert_eq!(on, off, "the payload does not change with tournament mode");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The card's default club lists: GW/SW/LW chip, PT putts, any other
    /// club is a full swing; the mode goes out in the card's select with
    /// its club and hand.
    #[test]
    fn the_default_club_lists_give_the_mode_in_the_select() {
        let mut relay = Relay::new(
            GsProRouting::default(),
            Registry::default(),
            "openconnect_client.0",
        );
        for (club, mode) in [
            (Club::GapWedge, Mode::Chipping),
            (Club::SandWedge, Mode::Chipping),
            (Club::LobWedge, Mode::Chipping),
            (Club::Putter, Mode::Putting),
            (Club::Iron7, Mode::Full),
            (Club::Driver, Mode::Full),
        ] {
            relay.hold(Some(Handedness::Left), Some(club));
            let select = relay.held.select("r".into());
            assert_eq!(select.club, Some(Some(club)));
            assert_eq!(select.handedness, Some(Handedness::Left));
            assert_eq!(select.mode, Some(mode), "{club:?}");
        }
    }

    /// Empty chipping and putting lists: the card sends no mode.
    #[test]
    fn empty_club_lists_send_no_mode() {
        let section = flighthook::OpenConnectClientSection {
            chipping_clubs: Vec::new(),
            putting_clubs: Vec::new(),
            ..flighthook::OpenConnectClientSection::default()
        };
        assert_eq!(section.club_mode(Some(Club::Putter)), None);
        assert_eq!(section.club_mode(Some(Club::SandWedge)), None);
        let default = flighthook::OpenConnectClientSection::default();
        assert_eq!(default.club_mode(Some(Club::Putter)), Some(Mode::Putting));
        assert_eq!(default.club_mode(None), Some(Mode::Full));
        let held = Held {
            club: Some(Club::Putter),
            hand: Some(Handedness::Right),
            mode: section.club_mode(Some(Club::Putter)),
        };
        assert_eq!(held.select("r".into()).mode, None);
    }

    /// A player message with a hand and no club holds the hand and no
    /// mode: the card asserts no Full it was never told.
    #[test]
    fn a_hand_without_a_club_holds_no_mode() {
        let mut relay = Relay::new(
            GsProRouting::default(),
            Registry::default(),
            "openconnect_client.0",
        );
        relay.hold(Some(Handedness::Left), None);
        let select = relay.held.select("r".into());
        assert_eq!(select.handedness, Some(Handedness::Left));
        assert_eq!(select.mode, None);
        assert_eq!(select.club, None);
    }

    /// The mode the card re-asserts comes from the club lists in the live
    /// config at the time, not from the lists when the club was held.
    #[test]
    fn a_reassert_recomputes_the_mode_from_the_live_club_lists() {
        let dir = crate::actors::web::tests::TempDir(std::env::temp_dir().join(format!(
            "flighthook-gspro-lists-{}",
            crate::state::config::generate_id()
        )));
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        state.system.update(|c| {
            c.frp.actor_id = Some("flighthook".into());
            c.openconnect_client
                .insert("0".into(), flighthook::OpenConnectClientSection::default());
        });
        let state = Arc::new(state);
        let (tx, _) = broadcast::channel(64);
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx.clone(), Arc::default()).subscribe();
        let mut relay = Relay::new(
            GsProRouting::default(),
            Registry {
                state: Some(Arc::clone(&state)),
                sent: None,
            },
            "openconnect_client.0",
        );
        relay.connect(Instant::now());
        relay.hold(Some(Handedness::Right), Some(Club::SandWedge));
        assert_eq!(relay.held.mode, Some(Mode::Chipping));
        state.system.update(|c| {
            if let Some(s) = c.openconnect_client.get_mut("0") {
                s.chipping_clubs.clear();
            }
        });
        turn(
            &tx,
            Mode::Chipping,
            Some(Club::SandWedge),
            Some("flighthook"),
        );
        relay.drain(&mut receiver).expect("bus open");
        assert_eq!(relay.held.mode, Some(Mode::Full));
        assert!(relay.reassert, "the turn's Chipping differs from Full");
    }

    /// The re-assert follows the latest turn of a drain: a turn that
    /// differs, then one that matches, re-asserts nothing.
    #[test]
    fn the_latest_turn_of_a_drain_decides_the_reassert() {
        let (tx, _) = broadcast::channel(64);
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx.clone(), Arc::default()).subscribe();
        let mut relay = Relay::new(
            GsProRouting::default(),
            Registry::default(),
            "openconnect_client.0",
        );
        relay.connect(Instant::now());
        relay.hold(Some(Handedness::Right), Some(Club::SandWedge));
        turn(&tx, Mode::Full, Some(Club::Driver), Some("flighthook"));
        turn(
            &tx,
            Mode::Chipping,
            Some(Club::SandWedge),
            Some("flighthook"),
        );
        relay.drain(&mut receiver).expect("bus open");
        assert!(!relay.reassert);
    }

    /// Two Open Connect cards whose simulators disagree, through the real
    /// session with the registry populated: the last sender wins, neither
    /// re-asserts over the other, a `bus.sync` (the session republishing
    /// its turn with its cause) starts no re-assert, and a header pick has
    /// both re-assert.
    #[test]
    fn two_disagreeing_simulator_cards_and_a_sync_through_the_session() {
        let dir = crate::actors::web::tests::TempDir(std::env::temp_dir().join(format!(
            "flighthook-two-sims-{}",
            crate::state::config::generate_id()
        )));
        let (state, writer) = SystemState::new(dir.0.join("config.toml"));
        let state = Arc::new(state);
        let card = |id: &str| flighthook::OpenConnectClientSection {
            name: "Open Connect".into(),
            enabled: true,
            actor_id: Some(id.into()),
            address: Some("not an address".into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
            ..Default::default()
        };
        state.system.update(|c| {
            c.openconnect_client.insert("0".into(), card("oc-a"));
            c.openconnect_client.insert("1".into(), card("oc-b"));
        });
        let (tx, _) = broadcast::channel(256);
        let shutdown = Arc::new(AtomicBool::new(false));
        crate::actors::system::start_for_test(writer, &state, &tx, Arc::clone(&shutdown));
        let me = state.flighthook_id();
        let mut watch = tx.subscribe();
        let side = |key: &str, id: &str| {
            let sender =
                BusSender::new(key.into(), tx.clone(), Arc::default()).with_frp_id(Some(id.into()));
            let receiver = sender.subscribe();
            let registry = Registry {
                state: Some(Arc::clone(&state)),
                sent: None,
            };
            let mut relay = Relay::new(GsProRouting::default(), registry, key);
            relay.connect(Instant::now());
            (relay, sender, receiver)
        };
        let (mut a, a_tx, mut a_rx) = side("openconnect_client.0", "oc-a");
        let (mut b, b_tx, mut b_rx) = side("openconnect_client.1", "oc-b");
        // The session's next turn with `club`, and its cause.
        let mut turned = |club: Club| -> Option<String> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match watch.try_recv() {
                    Ok(msg) => {
                        if let Some((turn, cause)) = msg.turn_of(&me)
                            && turn.club == Some(club)
                        {
                            return cause.map(str::to_owned);
                        }
                    }
                    Err(_) => {
                        assert!(Instant::now() < deadline, "no turn with {club:?}");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        };
        let mut n = 0;
        a.hold(Some(Handedness::Right), Some(Club::SandWedge));
        select(&a, &a_tx, &mut n);
        assert_eq!(turned(Club::SandWedge).as_deref(), Some("oc-a"));
        b.hold(Some(Handedness::Right), Some(Club::Driver));
        select(&b, &b_tx, &mut n);
        assert_eq!(turned(Club::Driver).as_deref(), Some("oc-b"));
        a.drain(&mut a_rx).expect("bus open");
        b.drain(&mut b_rx).expect("bus open");
        assert!(
            !a.reassert,
            "the other simulator caused it: last sender wins"
        );
        assert!(!b.reassert, "it holds the turn");

        let _ = tx.send(BusMsg::frp(flightrelay::FrpMessage::new(
            "web",
            flightrelay::BusSync::new(),
        )));
        assert_eq!(
            turned(Club::Driver).as_deref(),
            Some("oc-b"),
            "republished with its cause"
        );
        a.drain(&mut a_rx).expect("bus open");
        b.drain(&mut b_rx).expect("bus open");
        assert!(!a.reassert && !b.reassert, "a sync starts no re-assert");

        // A header pick (flighthook's own select): both simulators win it
        // back.
        let pick = flightrelay::GolferSelect {
            club: Some(Some(Club::Iron7)),
            ..flightrelay::GolferSelect::new("header")
        };
        let _ = tx.send(BusMsg::frp(flightrelay::FrpMessage::new(me.clone(), pick)));
        turned(Club::Iron7);
        a.drain(&mut a_rx).expect("bus open");
        b.drain(&mut b_rx).expect("bus open");
        assert!(a.reassert && b.reassert);
        shutdown.store(true, Ordering::Relaxed);
    }

    /// The card re-asserts its held select, mode included, when
    /// flighthook's turn differs from it, unless a game card caused the
    /// turn; the select it sends again carries club, hand and mode.
    #[test]
    fn the_card_reasserts_its_held_select_with_the_mode() {
        let (tx, _) = broadcast::channel(64);
        let sender = BusSender::new("openconnect_client.0".into(), tx.clone(), Arc::default())
            .with_frp_id(Some("openconnect_client.0".into()));
        let mut receiver = sender.subscribe();
        let mut relay = Relay::new(
            GsProRouting::default(),
            Registry::default(),
            "openconnect_client.0",
        );
        relay.connect(Instant::now());
        relay.hold(Some(Handedness::Right), Some(Club::SandWedge));

        // The turn matches: nothing to re-assert.
        turn(
            &tx,
            Mode::Chipping,
            Some(Club::SandWedge),
            Some("flighthook"),
        );
        relay.drain(&mut receiver).expect("bus open");
        assert!(!relay.reassert);

        // Only the mode differs: re-assert.
        turn(&tx, Mode::Full, Some(Club::SandWedge), Some("flighthook"));
        relay.drain(&mut receiver).expect("bus open");
        assert!(relay.reassert);

        let mut n = 0;
        select(&relay, &sender, &mut n);
        let sent = std::iter::from_fn(|| receiver.poll().ok().flatten())
            .find_map(|m| match m.as_frp().map(|f| &f.body) {
                Some(flightrelay::FrpBody::GolferSelect(s)) => Some(s.clone()),
                _ => None,
            })
            .expect("select sent");
        assert_eq!(sent.club, Some(Some(Club::SandWedge)));
        assert_eq!(sent.handedness, Some(Handedness::Right));
        assert_eq!(sent.mode, Some(Mode::Chipping));
        assert_eq!(n, 1);
    }

    /// Omni for full swings and chips, ExPutt for putts: each mode sends
    /// only its own monitor's shot, and readiness follows that monitor.
    #[test]
    fn full_shots_and_putts_come_from_their_own_monitors() {
        let (tx, _) = broadcast::channel(64);
        let omni = actor("square.0", &tx);
        let exputt = actor("exputt.0", &tx);
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx.clone(), Arc::default()).subscribe();
        let square = Some("square.0".to_owned());
        let routing = GsProRouting {
            full_monitor: square.clone(),
            chipping_monitor: square,
            putting_monitor: Some("exputt.0".to_owned()),
        };
        let mut relay = Relay::new(routing, Registry::default(), "openconnect_client.0");
        relay.connect(Instant::now());
        let modes = [
            (Mode::Full, "square.0"),
            (Mode::Chipping, "square.0"),
            (Mode::Putting, "exputt.0"),
        ];
        for (n, (mode, from)) in (1..).zip(modes) {
            turn(&tx, mode, None, None);
            emit_shot(&omni, 2 * n);
            emit_shot(&exputt, 2 * n + 1);
            relay.drain(&mut receiver).expect("bus open");
            let sent: Vec<String> = std::iter::from_fn(|| relay.next_shot(Instant::now()))
                .map(|s| s.actor)
                .collect();
            assert_eq!(sent, [from], "{mode:?}");
        }

        monitor_actor(&tx, "square.0", false);
        monitor_actor(&tx, "exputt.0", true);
        relay.drain(&mut receiver).expect("bus open");
        assert!(relay.ready(), "putting follows the ExPutt");
        turn(&tx, Mode::Full, None, None);
        relay.drain(&mut receiver).expect("bus open");
        assert!(!relay.ready(), "full follows the Omni");
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
                ..flighthook::ExPuttSection::default()
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
        let exputt = actor("exputt.0", &tx);
        let mut receiver =
            BusSender::new("openconnect_client.0".into(), tx.clone(), Arc::default()).subscribe();
        for (n, (full_monitor, sent)) in
            (1..).zip([(None, true), (Some("square.0".to_owned()), false)])
        {
            let mut relay = Relay::new(
                GsProRouting {
                    full_monitor,
                    chipping_monitor: None,
                    putting_monitor: Some("exputt.0".to_owned()),
                },
                Registry::default(),
                "openconnect_client.0",
            );
            relay.connect(Instant::now());
            turn(&tx, Mode::Full, None, None);
            emit_shot(&exputt, n);
            relay.drain(&mut receiver).expect("bus open");
            let actors: Vec<String> = std::iter::from_fn(|| relay.next_shot(Instant::now()))
                .map(|s| s.actor)
                .collect();
            assert_eq!(!actors.is_empty(), sent, "{:?}", relay.feed.routing);
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

    /// The bytes the bridge writes for one shot, with flighthook's turn
    /// set to `handed` first.
    fn sent_for(handed: flighthook::Handedness) -> Vec<String> {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connect(Instant::now());
        let _ = monitor
            .raw_sender()
            .send(BusMsg::frp(flightrelay::FrpMessage::new(
                "flighthook",
                flightrelay::GolferTurn::new(None, None, handed, Mode::Full),
            )));
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
        events: broadcast::Receiver<BusMsg>,
        shutdown: Arc<AtomicBool>,
        done: mpsc::Receiver<()>,
    }

    /// Whether `msg` is the card's own (`openconnect_client.0`).
    fn is_card(msg: &BusMsg) -> bool {
        match msg {
            BusMsg::Local { card, .. } => card == "openconnect_client.0",
            BusMsg::Frp { msg, .. } => msg.from == "openconnect_client.0",
        }
    }

    /// The card's alert text, if `msg` is one (a `bus.alert` or a local
    /// alert).
    fn card_alert(msg: &BusMsg) -> Option<String> {
        if !is_card(msg) {
            return None;
        }
        match msg {
            BusMsg::Local {
                event: LocalEvent::Alert { message, .. },
                ..
            } => Some(message.clone()),
            BusMsg::Frp { msg, .. } => match &msg.body {
                flightrelay::FrpBody::BusAlert(a) => Some(a.message.clone()),
                _ => None,
            },
            BusMsg::Local { .. } => None,
        }
    }

    /// The card's telemetry, if `msg` is its status.
    fn card_status(msg: &BusMsg) -> Option<(ActorStatus, HashMap<String, String>)> {
        match msg.as_local() {
            Some(("openconnect_client.0", LocalEvent::CardStatus { status, telemetry })) => {
                Some((*status, telemetry.clone()))
            }
            _ => None,
        }
    }

    impl Bridge {
        fn start(backoff: Backoff) -> Self {
            Self::start_after(backoff, |_, _| {})
        }

        /// [`start`](Self::start), with `up` run on the bus (and the
        /// shutdown flag) before the card subscribes: actors already up.
        fn start_after(
            backoff: Backoff,
            up: impl FnOnce(&broadcast::Sender<BusMsg>, &Arc<AtomicBool>),
        ) -> Self {
            Self::start_routed(backoff, GsProRouting::default(), up)
        }

        /// [`start_after`](Self::start_after) with `routing`.
        fn start_routed(
            backoff: Backoff,
            routing: GsProRouting,
            up: impl FnOnce(&broadcast::Sender<BusMsg>, &Arc<AtomicBool>),
        ) -> Self {
            let gspro = TcpListener::bind("127.0.0.1:0").expect("bind fake GSPro");
            gspro.set_nonblocking(true).expect("nonblocking listener");
            let addr = gspro.local_addr().expect("listener addr");
            let (tx, events) = broadcast::channel(1024);
            let shutdown = Arc::new(AtomicBool::new(false));
            up(&tx, &shutdown);
            let sender = BusSender::new(
                "openconnect_client.0".into(),
                tx.clone(),
                Arc::clone(&shutdown),
            )
            .with_frp_id(Some("openconnect_client.0".into()));
            let receiver = sender.subscribe();
            let monitor = actor("mevo.0", &tx);
            let (done_tx, done) = mpsc::channel();
            std::thread::spawn(move || {
                run_with(
                    addr,
                    routing,
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
        fn wait_for(&mut self, what: &str, want: impl Fn(&BusMsg) -> bool) -> Vec<String> {
            let deadline = Instant::now() + WAIT;
            let mut alerts = Vec::new();
            loop {
                match self.events.try_recv() {
                    Ok(msg) if is_card(&msg) => {
                        if let Some(message) = card_alert(&msg) {
                            alerts.push(message);
                        }
                        if want(&msg) {
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
                if let Some(message) = card_alert(&msg) {
                    alerts.push(message);
                }
            }
            alerts
        }

        fn wait_for_status(&mut self, status: ActorStatus) -> Vec<String> {
            self.wait_for(&format!("{status:?}"), |m| {
                matches!(
                    m.as_local(),
                    Some((_, LocalEvent::CardStatus { status: s, .. })) if *s == status
                )
            })
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

    /// `GsProActor::start`, as `start_actor` runs it, records each shot it
    /// writes in the app's send registry (`SystemState::sent_shots`), where
    /// the Pureplay card looks for it.
    #[test]
    fn the_started_actor_records_its_shots_in_the_apps_registry() {
        let dir = crate::actors::web::tests::TempDir(std::env::temp_dir().join(format!(
            "flighthook-gspro-registry-{}",
            crate::state::config::generate_id()
        )));
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        let state = Arc::new(state);
        let gspro = TcpListener::bind("127.0.0.1:0").expect("bind fake GSPro");
        let addr = gspro.local_addr().expect("listener addr");
        let (tx, mut events) = broadcast::channel(1024);
        crate::actors::start_actor(
            "openconnect_client.0".into(),
            Box::new(GsProActor {
                addr,
                routing: GsProRouting::default(),
            }),
            &state,
            &tx,
        );
        let (stream, _) = gspro.accept().expect("the actor connects");
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("read timeout");
        let mut gspro = GsPro {
            stream,
            buf: Vec::new(),
        };
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "never Connected");
            match events.try_recv() {
                Ok(msg) if card_status(&msg).is_some_and(|(s, _)| s == ActorStatus::Connected) => {
                    break;
                }
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("bus: {e}"),
            }
        }
        let monitor = actor("mevo.0", &tx);
        emit_shot(&monitor, 12);
        assert_eq!(gspro.shots(1), [golden(12)]);
        let loopback = SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 8765)));
        assert_eq!(
            state.sent_shots.take_echo(loopback, Instant::now()),
            Some(key(12))
        );
        assert!(!matches!(
            state.stop_and_wait("openconnect_client.0"),
            crate::state::Stopped::Lingering
        ));
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

    /// A missed read (a shot that finished with no data) writes nothing and
    /// takes no `ShotNumber`: the next shot goes out with its own number.
    #[test]
    fn a_no_read_writes_nothing_and_uses_no_shot_number() {
        let mut bridge = Bridge::start(Backoff::new(MIN_BACKOFF, MAX_BACKOFF));
        let mut gspro = bridge.accept();
        bridge.wait_for_status(ActorStatus::Connected);
        emit_shot(&bridge.monitor, 12);
        let missed = key(13);
        bridge
            .monitor
            .frp(flightrelay::ShotTrigger::new(missed.clone()));
        bridge
            .monitor
            .frp(flightrelay::ShotFinished { key: missed });
        emit_shot_as(
            &bridge.monitor,
            ShotKey {
                shot_id: "shot-13b".into(),
                shot_number: 13,
            },
        );
        assert_eq!(gspro.shots(2), [golden(12), golden(13)]);
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

    /// The client's card, built from its statuses as the web layer builds it
    /// (`ActorStatusResponse::apply_status`), until `done` holds for its
    /// telemetry.
    fn telemetry_until(
        bridge: &mut Bridge,
        card: &mut ActorStatusResponse,
        what: &str,
        done: impl Fn(&HashMap<String, String>) -> bool,
    ) {
        let deadline = Instant::now() + WAIT;
        while !done(&card.telemetry) {
            match bridge.events.try_recv() {
                Ok(msg) => {
                    if let Some((status, telemetry)) = card_status(&msg) {
                        card.apply_status(status, &telemetry, 0);
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for {what}: {:?}",
                        card.telemetry
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
        let mut card = ActorStatusResponse::new("GSPro", ActorStatus::Starting);
        telemetry_until(&mut bridge, &mut card, "connected", |c| {
            c.get(SHOTS_SENT).map(String::as_str) == Some("0")
        });
        assert_eq!(card.telemetry[ADDRESS], addr);
        assert!(!card.telemetry.contains_key(LAST_SHOT));

        gspro
            .stream
            .write_all(
                br#"{"Code":201,"Message":"GSPro Player Information","Player":{"Handed":"RH","Club":"DR"}}"#,
            )
            .expect("player info");
        telemetry_until(&mut bridge, &mut card, "the response", |c| {
            c.contains_key(RESPONSE)
        });
        assert_eq!(card.telemetry[RESPONSE], "201 GSPro Player Information");

        emit_shot(&bridge.monitor, 12);
        assert_eq!(gspro.shots(1), [golden(12)]);
        telemetry_until(&mut bridge, &mut card, "the shot", |c| {
            c.get(SHOTS_SENT).map(String::as_str) == Some("1")
        });
        let last = &card.telemetry[LAST_SHOT];
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
        assert_eq!(card.telemetry[MONITOR_READY], "false");

        // The player information shows (web layer) until the connection ends.
        card.telemetry.insert("club".into(), "DR".into());
        card.telemetry.insert("handed".into(), "rh".into());
        drop(gspro);
        telemetry_until(&mut bridge, &mut card, "the connection's keys gone", |c| {
            !c.contains_key(SHOTS_SENT)
        });
        for key in PER_CONNECTION.into_iter().chain(PLAYER) {
            assert!(
                !card.telemetry.contains_key(key),
                "{key}: {:?}",
                card.telemetry
            );
        }
        assert_eq!(card.telemetry[ADDRESS], addr);
        assert!(card.telemetry[LAST_SHOT].starts_with("#12 at "));
        bridge.stop().expect("bridge stops");
    }

    /// A card added after a launch monitor came up ready learns its
    /// `bus.actor` from the answer to the `bus.sync` it sends at start, and
    /// tells the simulator the monitor is ready.
    #[test]
    fn a_card_added_after_the_monitor_is_up_reports_ready() {
        let mut bridge = Bridge::start_after(Backoff::new(MAX_BACKOFF, MAX_BACKOFF), |tx, stop| {
            let mevo = actor("mevo.1", tx);
            let ready = flightrelay::BusActor::new("Mevo+", 1.0)
                .with_role(flightrelay::types::roles::LAUNCH_MONITOR);
            // Published before the card is on the bus: it never sees this.
            mevo.frp(ready.clone());
            let mut rx = tx.subscribe();
            let stop = Arc::clone(stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match rx.try_recv() {
                        Ok(BusMsg::Frp { msg, .. })
                            if matches!(msg.body, flightrelay::FrpBody::BusSync(_)) =>
                        {
                            mevo.frp(ready.clone());
                        }
                        Err(broadcast::error::TryRecvError::Empty) => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(broadcast::error::TryRecvError::Closed) => return,
                        _ => {}
                    }
                }
            });
        });
        let _gspro = bridge.accept();
        let mut card = ActorStatusResponse::new("GSPro", ActorStatus::Starting);
        telemetry_until(&mut bridge, &mut card, "the monitor ready", |c| {
            c.get(MONITOR_READY).map(String::as_str) == Some("true")
        });
        bridge.stop().expect("bridge stops");
    }

    /// A card (re)started while flighthook's turn is Putting learns the
    /// turn from the answer to its `bus.sync` and routes the next shot as a
    /// putt at once.
    #[test]
    fn a_card_restarted_while_putting_routes_as_putting_at_once() {
        let routing = GsProRouting {
            full_monitor: Some("mevo.0".into()),
            putting_monitor: Some("putter.0".into()),
            ..GsProRouting::default()
        };
        let mut bridge = Bridge::start_routed(
            Backoff::new(MIN_BACKOFF, MAX_BACKOFF),
            routing,
            |tx, stop| {
                // The turn before the card is on the bus: it never sees this.
                turn(tx, Mode::Putting, Some(Club::Putter), None);
                let mut rx = tx.subscribe();
                let (tx, stop) = (tx.clone(), Arc::clone(stop));
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match rx.try_recv() {
                            Ok(BusMsg::Frp { msg, .. })
                                if matches!(msg.body, flightrelay::FrpBody::BusSync(_)) =>
                            {
                                turn(&tx, Mode::Putting, Some(Club::Putter), None);
                            }
                            Err(broadcast::error::TryRecvError::Empty) => {
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            Err(broadcast::error::TryRecvError::Closed) => return,
                            _ => {}
                        }
                    }
                });
            },
        );
        let mut gspro = bridge.accept();
        bridge.wait_for_status(ActorStatus::Connected);
        let putter = actor("putter.0", &bridge.monitor.raw_sender().clone());
        emit_shot(&bridge.monitor, 12);
        emit_shot(&putter, 13);
        assert_eq!(gspro.shots(1), [golden(13)]);
        bridge.stop().expect("bridge stops");
    }

    /// Every status of a lost or restarted connection carries what outlasts
    /// the connection, so the card (which drops its telemetry then) still
    /// shows where it dials, the last shot and a refusal.
    #[test]
    fn a_status_that_drops_the_card_carries_the_lasting_values() {
        let mut t = Telemetry::new("127.0.0.1:921".parse().expect("addr"));
        let mut card = ActorStatusResponse::new("GSPro", ActorStatus::Starting);
        let apply = |card: &mut ActorStatusResponse, status, update: HashMap<String, String>| {
            card.apply_status(status, &update, 0);
        };
        apply(&mut card, ActorStatus::Starting, t.take_all());
        assert_eq!(card.telemetry[ADDRESS], "127.0.0.1:921");

        // A refusal, a retry before any connection: the address stays.
        t.set(ERROR, "refused".into());
        apply(&mut card, ActorStatus::Starting, t.take_all());
        apply(&mut card, ActorStatus::Starting, t.take_all());
        assert_eq!(card.telemetry[ADDRESS], "127.0.0.1:921");
        assert_eq!(card.telemetry[ERROR], "refused");

        // Connected: the verdict clears the error; one shot goes out; the
        // connection is lost.
        t.set(ERROR, String::new());
        t.set(SHOTS_SENT, "0".into());
        let mut update = t.take();
        update.extend(clear_error());
        apply(&mut card, ActorStatus::Connected, update);
        t.set(SHOTS_SENT, "1".into());
        t.set(LAST_SHOT, "#1 at 10:00:00".into());
        apply(&mut card, ActorStatus::Connected, t.take());
        t.clear_connection();
        apply(&mut card, ActorStatus::Reconnecting, t.take_all());
        assert_eq!(
            card.telemetry,
            HashMap::from([
                (ADDRESS.into(), "127.0.0.1:921".into()),
                (LAST_SHOT.into(), "#1 at 10:00:00".into()),
            ])
        );
        // The club and hand the card holds go out on its own card.
        t.player(&Held {
            club: Some(Club::Driver),
            hand: Some(Handedness::Right),
            mode: None,
        });
        let update = t.take_all();
        assert_eq!(update["club"], Club::Driver.to_string());
        assert_eq!(update["handed"], Handedness::Right.to_string());
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

    // -- Simulator sources and the send registry ---------------------------------

    mod sources {
        use super::*;
        use crate::actors::provenance::ShotSource;
        use crate::actors::web::tests::TempDir;
        use flightrelay::types::roles;

        /// One bus and senders for each actor id the traffic uses (their FRP
        /// ids are their card keys); flighthook's actor is `flighthook` and
        /// Auto-chip's `autochip`, as in a [`state`]'s config.
        struct Bus {
            tx: broadcast::Sender<BusMsg>,
        }

        impl Bus {
            fn new() -> Self {
                Self {
                    tx: broadcast::channel(1024).0,
                }
            }

            fn sender(&self, id: &str) -> BusSender {
                actor(id, &self.tx)
            }

            fn receiver(&self) -> BusReceiver {
                BusSender::new(
                    "openconnect_client.0".into(),
                    self.tx.clone(),
                    Arc::default(),
                )
                .subscribe()
            }

            /// Launch monitor `id`'s `bus.actor`: ready (no blockers) or
            /// not, with `battery` when given.
            fn ready(&self, id: &str, ready: bool, battery: Option<u8>) {
                let mut a = flightrelay::BusActor::new(id, 1.0).with_role(roles::LAUNCH_MONITOR);
                if !ready {
                    a.blockers.push("not_ready".into());
                }
                if battery.is_some() {
                    a = a.with_device(flightrelay::DeviceInfo {
                        battery_pct: battery,
                        ..flightrelay::DeviceInfo::default()
                    });
                }
                let _ = self
                    .tx
                    .send(BusMsg::frp(flightrelay::FrpMessage::new(id, a)));
            }

            /// A card's status: the card's local event and, for a lost
            /// connection, its `bus.actor` with a blocker.
            fn status(&self, id: &str, status: ActorStatus) {
                let _ = self.tx.send(BusMsg::local(
                    id,
                    LocalEvent::CardStatus {
                        status,
                        telemetry: HashMap::new(),
                    },
                ));
                if matches!(
                    status,
                    ActorStatus::Disconnected | ActorStatus::Reconnecting
                ) {
                    self.ready(id, false, None);
                }
            }

            /// The Pureplay Accessory card's `bus.actor` (a game's launch
            /// monitor), ready or not.
            fn card_ready(&self, ready: bool) {
                let mut a = flightrelay::BusActor::new("Pureplay", 1.0)
                    .with_role(roles::GAME)
                    .with_role(roles::LAUNCH_MONITOR);
                if !ready {
                    a.blockers.push("not_ready".into());
                }
                let _ = self.tx.send(BusMsg::frp(flightrelay::FrpMessage::new(
                    "pureplay_accessory.0",
                    a,
                )));
            }

            /// flighthook's turn in `mode`.
            fn mode(&self, mode: Mode) {
                turn(&self.tx, mode, None, None);
            }
        }

        /// The app's state with flighthook's actor `flighthook`, Auto-chip's
        /// `autochip`, and `sources` registered as shot sources.
        fn state(sources: &[(&str, ShotSource)]) -> (Arc<SystemState>, TempDir) {
            let dir = TempDir(std::env::temp_dir().join(format!(
                "flighthook-gspro-sources-{}",
                crate::state::config::generate_id()
            )));
            let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
            state.system.update(|c| {
                c.frp.actor_id = Some("flighthook".into());
                c.frp.auto_chip_actor_id = Some(flighthook::AUTO_CHIP_ACTOR.into());
            });
            for (actor, source) in sources {
                state.round.register_source(actor, *source);
            }
            (Arc::new(state), dir)
        }

        /// The same bus traffic: shots from the Mevo, the R10 and Auto-chip,
        /// readiness, a mode change, and a Pureplay card's shot and readiness
        /// interleaved; returns the bytes `relay` writes and its readiness.
        fn traffic(mut relay: Relay) -> (Vec<u8>, bool, Relay) {
            let bus = Bus::new();
            let mevo = bus.sender("mevo.0");
            let r10 = bus.sender("r10.0");
            let chip = bus.sender(flighthook::AUTO_CHIP_ACTOR);
            let card = bus.sender("pureplay_accessory.0");
            let mut receiver = bus.receiver();
            relay.connect(Instant::now());
            bus.card_ready(true);
            emit_shot(&mevo, 3);
            emit_shot_as(&card, ShotKey::new("card-1", 1));
            emit_shot(&r10, 2);
            bus.ready("mevo.0", true, None);
            emit_shot(&chip, 1);
            bus.mode(Mode::Chipping);
            emit_shot_as(&card, ShotKey::new("card-2", 2));
            emit_shot(&mevo, 4);
            relay.drain(&mut receiver).expect("bus");
            let mut wire = Vec::new();
            send_shots(&mut relay, &mut wire, Instant::now(), "test", &mut |_| {})
                .expect("written");
            let ready = relay.ready();
            (wire, ready, relay)
        }

        /// Registering the shot source types and keeping a send registry
        /// changes nothing a client writes: the same traffic goes out byte for
        /// byte from a client with both and from one with neither, the Pureplay
        /// card's shots and readiness from neither. Each written shot is
        /// recorded with its own key. The comparison with the code before the
        /// Pureplay card is `the_relay_writes_byte_for_byte_what_5021d9e_wrote`.
        #[test]
        fn the_registry_and_source_types_change_nothing() {
            let (state, _dir) = state(&[
                ("mevo.0", ShotSource::Mevo),
                ("r10.0", ShotSource::R10),
                (flighthook::AUTO_CHIP_ACTOR, ShotSource::AutoChip),
                ("pureplay_accessory.0", ShotSource::PureplayAccessory),
            ]);
            let sent = Arc::new(SentShots::default());
            let mut shared = Relay::new(
                GsProRouting::default(),
                Registry {
                    state: Some(state),
                    sent: Some(Arc::clone(&sent)),
                },
                "openconnect_client.0",
            );
            shared.peer = Some(SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 921))));

            let plain = Relay::new(
                GsProRouting::default(),
                Registry::default(),
                "openconnect_client.0",
            );
            let (plain_wire, plain_ready, _) = traffic(plain);
            let (shared_wire, shared_ready, _) = traffic(shared);
            assert_eq!(shared_wire, plain_wire);
            assert_eq!(shared_ready, plain_ready);
            assert!(plain_ready, "the Mevo's readiness counts");
            let wrote = messages(&plain_wire);
            assert_eq!(
                wrote.len(),
                4,
                "the Mevo's two, the R10's, the chip: {wrote:?}"
            );
            assert_eq!(wrote[0], golden(3));

            // Each written shot was recorded with its own key, before
            // renumbering.
            let loopback = SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 8765)));
            let now = Instant::now();
            let keys: Vec<ShotKey> = std::iter::from_fn(|| sent.take_echo(loopback, now)).collect();
            assert_eq!(
                keys,
                [key(3), key(2), ShotKey::new(key(1).shot_id, 1), key(4)]
            );
        }

        /// A shot whose write fails is not left in the send registry: it was
        /// not sent.
        #[test]
        fn a_failed_write_takes_its_record_back() {
            let (relay, monitor, mut receiver) = relay_with_bus();
            let sent = Arc::new(SentShots::default());
            let mut relay = Relay::new(
                relay.feed.routing,
                Registry {
                    state: None,
                    sent: Some(Arc::clone(&sent)),
                },
                "openconnect_client.0",
            );
            relay.peer = Some(SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 921))));
            relay.connect(Instant::now());
            emit_shot(&monitor, 1);
            relay.drain(&mut receiver).expect("bus open");
            let failed = send_shots(
                &mut relay,
                &mut TimedOutSocket,
                Instant::now(),
                "openconnect_client.0",
                &mut |_| {},
            );
            assert!(failed.is_err());
            assert_eq!(sent.len(), 0);
        }

        /// The Pureplay card feeds no client: its shots and readiness are
        /// skipped under "Any" and when routed to it by id, known by the
        /// `game` role in its `bus.actor` alone or registered as a simulator
        /// source too.
        #[test]
        fn a_simulator_source_feeds_no_open_connect_client() {
            let by_id = GsProRouting {
                full_monitor: Some("pureplay_accessory.0".into()),
                chipping_monitor: Some("pureplay_accessory.0".into()),
                putting_monitor: Some("pureplay_accessory.0".into()),
            };
            let (registered, _dir) =
                state(&[("pureplay_accessory.0", ShotSource::PureplayAccessory)]);
            // Registered, it is refused before its `bus.actor` arrives.
            let fresh = Feed::new(by_id.clone(), Some(Arc::clone(&registered)));
            assert!(!fresh.feeds_simulators("pureplay_accessory.0"));
            for routing in [GsProRouting::default(), by_id] {
                for state in [Some(Arc::clone(&registered)), None] {
                    let relay = Relay::new(
                        routing.clone(),
                        Registry { state, sent: None },
                        "openconnect_client.0",
                    );
                    let (wire, ready, relay) = traffic(relay);
                    assert!(!relay.feed.feeds_simulators("pureplay_accessory.0"));
                    for mode in [Mode::Full, Mode::Chipping, Mode::Putting] {
                        assert!(!relay.feed.passes("pureplay_accessory.0", mode));
                    }
                    let wrote = messages(&wire).len();
                    if routing.full_monitor.is_some() {
                        // Only Auto-chip, which every routing takes; the card's
                        // readiness does not count.
                        assert_eq!(wrote, 1, "{routing:?}");
                        assert!(!ready);
                    } else {
                        assert_eq!(wrote, 4, "the card's two left out");
                        assert!(ready, "the Mevo's");
                    }
                }
            }
            // A launch monitor, Auto-chip and a simulator output still feed the
            // clients.
            let (state, _dir) = state(&[("mevo.0", ShotSource::Mevo)]);
            let feed = Feed::new(GsProRouting::default(), Some(state));
            assert!(feed.feeds_simulators("mevo.0"));
            assert!(feed.feeds_simulators(flighthook::AUTO_CHIP_ACTOR));
            assert!(feed.feeds_simulators("pureplay.0"));
        }

        /// Put shot `n` of `monitor` on the bus, as [`emit_shot`] does, with
        /// a shot id of its own: FRP shot ids are unique across actors.
        fn shot(monitor: &BusSender, n: u32) {
            let id = monitor.frp_id().expect("an FRP id");
            emit_shot_as(monitor, ShotKey::new(format!("{id}-{n}"), n));
        }

        // -- Old-vs-new GSPro relay transcript ----------------------------------

        /// Run one fixed stream of bus traffic through relays built by `make`
        /// and return a transcript of what reached the simulator: per step,
        /// every byte written, the `ShotNumber`s handed to `on_sent`, and the
        /// readiness sent in the heartbeats.
        ///
        /// The traffic: eight shot sources (Mevo, R10, Square, ExPutt, Open
        /// Connect server, mock, trackball, Auto-chip), three routings ("Any";
        /// by id per mode; mixed), seven steps each: a shot while not
        /// connected, a burst, mode changes, readiness on and off,
        /// `Disconnected` / `Reconnecting` / `Starting` statuses, a stale shot,
        /// a reconnect with a device count that starts again, and device
        /// telemetry other than readiness. With `card`, a Pureplay card's
        /// readiness and shots are interleaved at every step.
        #[allow(clippy::too_many_lines)]
        fn relay_transcript(make: impl Fn(GsProRouting) -> Relay, card: bool) -> String {
            use std::fmt::Write as _;
            let mut out = String::new();
            let routings = [
                GsProRouting::default(),
                GsProRouting {
                    full_monitor: Some("mevo.0".into()),
                    chipping_monitor: Some("r10.0".into()),
                    putting_monitor: Some("exputt.0".into()),
                },
                GsProRouting {
                    full_monitor: Some("square.0".into()),
                    chipping_monitor: None,
                    putting_monitor: Some("openconnect_server.0".into()),
                },
            ];
            for routing in routings {
                writeln!(out, "== {routing:?}").unwrap();
                let bus = Bus::new();
                let mevo = bus.sender("mevo.0");
                let r10 = bus.sender("r10.0");
                let square = bus.sender("square.0");
                let exputt = bus.sender("exputt.0");
                let ocs = bus.sender("openconnect_server.0");
                let mock = bus.sender("mock_monitor.0");
                let track = bus.sender("trackball.0");
                let chip = bus.sender(flighthook::AUTO_CHIP_ACTOR);
                let pureplay = bus.sender("pureplay_accessory.0");
                let mut receiver = bus.receiver();
                let mut relay = make(routing);
                let mut card_shots = 0;
                let mut card_step = |ready: bool| {
                    if card {
                        card_shots += 1;
                        bus.card_ready(ready);
                        emit_shot_as(
                            &pureplay,
                            ShotKey {
                                shot_id: format!("card-{card_shots}"),
                                shot_number: card_shots,
                            },
                        );
                        bus.status("pureplay_accessory.0", ActorStatus::Connected);
                    }
                };
                let step = |relay: &mut Relay,
                            receiver: &mut BusReceiver,
                            label: &str,
                            late: bool,
                            out: &mut String| {
                    relay.drain(receiver).expect("bus");
                    let now = if late {
                        Instant::now() + Duration::from_secs(6)
                    } else {
                        Instant::now()
                    };
                    let mut wire = Vec::new();
                    let mut numbers = Vec::new();
                    send_shots(relay, &mut wire, now, "transcript", &mut |n| {
                        numbers.push(n);
                    })
                    .expect("write");
                    let _ = relay.take_readiness_changed();
                    writeln!(
                        out,
                        "{label}: ready={} numbers={numbers:?} wire={}",
                        relay.ready(),
                        String::from_utf8_lossy(&wire)
                    )
                    .unwrap();
                };
                card_step(true);
                shot(&mevo, 1);
                step(
                    &mut relay,
                    &mut receiver,
                    "1 not connected",
                    false,
                    &mut out,
                );
                relay.connect(Instant::now());
                bus.ready("mevo.0", true, None);
                card_step(true);
                bus.ready("r10.0", false, None);
                shot(&mevo, 5);
                shot(&r10, 2);
                shot(&square, 9);
                card_step(false);
                shot(&chip, 1);
                shot(&exputt, 3);
                shot(&ocs, 4);
                shot(&mock, 1);
                shot(&track, 7);
                step(&mut relay, &mut receiver, "2 burst", false, &mut out);
                bus.mode(Mode::Chipping);
                bus.ready("r10.0", true, None);
                bus.status("mevo.0", ActorStatus::Disconnected);
                card_step(true);
                shot(&r10, 3);
                shot(&mevo, 6);
                shot(&chip, 2);
                step(&mut relay, &mut receiver, "3 chipping", false, &mut out);
                bus.mode(Mode::Putting);
                bus.ready("exputt.0", true, None);
                shot(&exputt, 4);
                card_step(true);
                shot(&ocs, 5);
                bus.status("r10.0", ActorStatus::Reconnecting);
                step(&mut relay, &mut receiver, "4 putting", false, &mut out);
                shot(&mevo, 7);
                card_step(true);
                shot(&exputt, 5);
                step(&mut relay, &mut receiver, "5 stale", true, &mut out);
                relay.disconnect();
                shot(&mevo, 8);
                card_step(true);
                relay.drain(&mut receiver).expect("bus");
                relay.connect(Instant::now());
                bus.mode(Mode::Full);
                bus.ready("mevo.0", true, None);
                emit_shot_as(
                    &mevo,
                    ShotKey {
                        shot_id: "again-1".into(),
                        shot_number: 1,
                    },
                );
                card_step(true);
                emit_shot_as(
                    &square,
                    ShotKey {
                        shot_id: "again-2".into(),
                        shot_number: 2,
                    },
                );
                step(&mut relay, &mut receiver, "6 reconnected", false, &mut out);
                bus.ready("square.0", false, Some(80));
                bus.ready("square.0", true, Some(80));
                bus.status("square.0", ActorStatus::Starting);
                bus.status("mevo.0", ActorStatus::Reconnecting);
                card_step(true);
                bus.ready("openconnect_server.0", true, None);
                shot(&square, 30);
                shot(&ocs, 31);
                card_step(true);
                step(&mut relay, &mut receiver, "7 statuses", false, &mut out);
            }
            out
        }

        /// The relay as `GsProActor::start` builds it: the app's state with
        /// every shot source type registered (a Pureplay card's too), the send
        /// registry, and the simulator's address set.
        fn production_relay(state: &Arc<SystemState>) -> impl Fn(GsProRouting) -> Relay + '_ {
            move |routing| {
                let mut relay = Relay::new(
                    routing,
                    Registry {
                        state: Some(Arc::clone(state)),
                        sent: Some(Arc::new(SentShots::default())),
                    },
                    "openconnect_client.0",
                );
                relay.peer = Some(SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 921))));
                relay
            }
        }

        /// A line of `fixtures/relay-5021d9e.txt` as the transcript has it:
        /// the step's readiness, numbers and bytes. The fixture also carries
        /// the old relay's own bookkeeping (its readiness-changed hint and
        /// per-monitor readiness map), which reached the simulator only
        /// through the readiness.
        fn reached_the_simulator(line: &str) -> String {
            let Some((head, wire)) = line.split_once(" numbers=") else {
                return line.to_owned();
            };
            let label = head.split_once(": changed=").map_or(head, |(l, _)| l);
            let ready = head
                .split_once(" ready=")
                .and_then(|(_, r)| r.split_once(' '))
                .map_or("", |(r, _)| r);
            format!("{label}: ready={ready} numbers={wire}")
        }

        /// The production relay sends exactly what the relay at 5021d9e (before
        /// the Pureplay card and the FRP bus) sent: the same bytes, shot
        /// numbers and readiness, step for step, as
        /// `fixtures/relay-5021d9e.txt` records them, also with a Pureplay
        /// card's shots and readiness interleaved (they leave no trace).
        #[test]
        fn the_relay_writes_byte_for_byte_what_5021d9e_wrote() {
            let old = include_str!("fixtures/relay-5021d9e.txt");
            assert_eq!(old.matches("\"ShotNumber\":").count(), 31, "the fixture");
            let old: String = old
                .lines()
                .map(|l| reached_the_simulator(l) + "\n")
                .collect();
            let (state, _dir) = state(&[
                ("mevo.0", ShotSource::Mevo),
                ("r10.0", ShotSource::R10),
                ("square.0", ShotSource::Square),
                ("exputt.0", ShotSource::ExPutt),
                ("openconnect_server.0", ShotSource::OpenConnectServer),
                ("mock_monitor.0", ShotSource::MockMonitor),
                ("trackball.0", ShotSource::Trackball),
                (flighthook::AUTO_CHIP_ACTOR, ShotSource::AutoChip),
                ("pureplay_accessory.0", ShotSource::PureplayAccessory),
            ]);
            for card in [false, true] {
                let new = relay_transcript(production_relay(&state), card);
                for (n, (new, old)) in new.lines().zip(old.lines()).enumerate() {
                    assert_eq!(new, old, "line {} (card: {card})", n + 1);
                }
                assert!(new == old, "the whole transcript (card: {card})");
            }
        }
    }
}
