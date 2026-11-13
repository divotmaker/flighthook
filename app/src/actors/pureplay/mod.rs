//! Pureplay Connect v1 output: the `pureplay` actor (alpha).
//!
//! Pureplay is the host and listens on TCP 9760; flighthook is the device
//! and connects to it. After the handshake (Hello, HelloAck) the actor
//! pushes the launch monitor's ready state, sends each routed shot as
//! BallData plus ClubData when the device measured club values, answers
//! every Ping with a Heartbeat, and takes the host's commands:
//!
//! - SetClubInfo and SetShotContext give what the card holds for the turn:
//!   the club and hand, and the mode from the club and the distance to the
//!   target ([`mode_for_lie`]: the putter putts, under 30 yd chips, else a
//!   full swing; an unknown distance is full). The card sends all three in
//!   one `golfer.select` from its own actor on every SetClubInfo and
//!   whenever a new distance changes the mode, and again whenever
//!   flighthook's turn differs from them unless the change came from
//!   another game or simulator card ([`Held`]).
//! - SetDetect and SetBallType are logged and otherwise left alone.
//! - SetStrictMode(1) (a recorded tournament round) asks the round policy
//!   for a round ([`RoundPolicy::begin_round`]), naming the launch monitors
//!   routed to this output, and acks its answer: accepted, or refused with
//!   the policy's reason. The link holds the [`RoundGuard`] while strict
//!   mode is on; SetStrictMode(0) or the end of the link drops it. During
//!   the round the card is a measured-only receiver: each shot goes out as
//!   its launch monitor sent it (launch, club and impact; no flight result,
//!   no value filled from an `assumed`), and a shot whose source is not
//!   measured ([`RoundGuard::counts`]) is not sent (logged once per
//!   source); the ready state and heartbeat count only the launch monitors
//!   the policy counts. Every new link starts with strict mode off.
//! - An Error from the host raises a Warn alert, one per burst of errors
//!   less than [`HOST_ERROR_BURST_GAP`] apart.
//! - Any other message type is answered with Error (code 0).
//!
//! The "routed launch monitor" is the one the mode of flighthook's turn (taken
//! from the session at start, then from the bus) is routed to, by actor id,
//! as for the Open Connect client ([`GsProRouting`]); with "Any"
//! routing, the readiness and connection state are the best of every
//! launch monitor's, and the monitor named in Hello and asked for its
//! battery is the only configured one, when there is exactly one.
//!
//! A simulator source (the Pureplay Accessory card) feeds no simulator output: its
//! shots, readiness, connection state and battery never reach Pureplay
//! through this output, and a round never names it, under "Any" routing or
//! routed by ID ([`Feed::feeds_simulators`]). Each shot is recorded in the send
//! registry ([`SentShots`]) just before it is written, so the Pureplay Accessory card
//! can tell the shot Pureplay then reports from one of its own; a shot
//! whose ball data fails to write takes its record back.
//!
//! The bus is drained the whole time, connected or not. A shot that
//! finishes while the link is not up (handshake done), or is older than
//! [`STALE_SHOT`] when it could be sent, is dropped.
//!
//! The card's telemetry: `address` (Pureplay's), `last_shot` (the ShotId
//! sent last and when), and for the current link `shots_sent`,
//! `ready_state` (the ready state last sent) and `strict_mode`. `error` is
//! set when the handshake fails (no HelloAck, an unsupported version) and
//! cleared by a successful handshake or a connection that fails another
//! way: Pureplay not running is normal, not an error. The web layer shows
//! the club and hand Pureplay set as `club` and `handed`; the actor removes
//! those when the link ends.

pub mod codec;
pub mod mapper;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::backoff::Backoff;
use super::gspro::GsProRouting;
use super::provenance::Source;
use super::simout::{Feed, FeedEvent, Held, measured_shot, mode_for_lie, output_shot};
use super::{Actor, ReconfigureOutcome};
use crate::actors::simout::OutputShot;
use crate::bus::{BusMsg, BusReceiver, BusSender, LocalEvent, PollError};
use crate::state::SystemState;
use crate::state::round::{RoundGuard, RoundPolicy, RoundRequest, not_measured_reason};
use crate::state::sent_shots::{SentShots, SimAddr};
use codec::{ConnectionState, FrameReader, Message, ReadyState};
use flighthook::{ActorStatus, Severity, ShotKey};
use flightrelay::{CompletedShot, FrpBody, GolferTurn};

/// Pureplay's default address.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:9760";

/// The name the device gives itself in Hello, before the launch monitor's.
const DEVICE_NAME: &str = "flighthook";

/// Launch monitor display names by actor ID, read when a link starts.
pub(crate) type MonitorNames = dyn Fn() -> HashMap<String, String> + Send + Sync;

/// The output's display name, read when it asks for a round.
pub(crate) type HolderName = dyn Fn() -> String + Send + Sync;

/// The configured launch monitors' display names by actor id, those turned
/// off and simulator sources (the Pureplay Accessory cards) left out.
pub(crate) fn monitor_names(state: &SystemState) -> HashMap<String, String> {
    let config = state.system.snapshot();
    super::actor_names(&config)
        .into_iter()
        .filter(|(card, _)| config.actor_enabled(card) && !card.starts_with("pureplay_accessory."))
        .filter_map(|(card, name)| Some((config.actor_of_card(&card)?, name)))
        .filter(|(id, _)| {
            state
                .round
                .source(id)
                .is_some_and(|s| s.is_launch_monitor() && !s.is_simulator())
        })
        .collect()
}

/// The display name of output `actor`: its card name, or "Pureplay
/// Connect" when it has none.
fn holder_name(state: &SystemState, actor: &str) -> String {
    super::actor_names(&state.system.snapshot())
        .get(actor)
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .unwrap_or("Pureplay Connect")
        .to_owned()
}

/// Actor error type.
#[derive(Debug)]
pub(crate) enum LinkError {
    Io(std::io::Error),
    /// The handshake failed: the reason, for the card.
    Handshake(String),
    Shutdown,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Handshake(reason) => write!(f, "{reason}"),
            Self::Shutdown => write!(f, "shutdown"),
        }
    }
}

/// Pureplay output actor (`pureplay.N`).
pub struct PureplayActor {
    pub addr: SocketAddr,
    pub routing: GsProRouting,
}

impl Actor for PureplayActor {
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let addr = self.addr;
        let routing = self.routing.clone();
        let turn = state.session.turn();
        let round = state.round.clone();
        let sent = Some(Arc::clone(&state.sent_shots));
        let actor = sender.actor_id().to_owned();
        let holder_state = Arc::clone(&state);
        let holder_state_for_feed = Arc::clone(&state);
        let holder: Box<HolderName> = Box::new(move || holder_name(&holder_state, &actor));
        let names: Box<MonitorNames> = Box::new(move || monitor_names(&state));
        std::thread::Builder::new()
            .name(format!("pureplay:{}", sender.actor_id()))
            .spawn(move || {
                run_with(
                    addr,
                    routing,
                    turn,
                    Backoff::new(MIN_BACKOFF, MAX_BACKOFF),
                    Shared {
                        names: &*names,
                        holder: &*holder,
                        round,
                        sent,
                        state: Some(holder_state_for_feed),
                    },
                    &sender,
                    receiver,
                );
            })
            .expect("failed to spawn pureplay thread")
    }

    /// Pureplay is reached over TCP as a client: a connect still running in
    /// the old instance keeps nothing from the new one.
    fn replaceable_while_stopping(&self) -> bool {
        true
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let Some((_, index)) = sender.actor_id().split_once('.') else {
            return ReconfigureOutcome::Applied;
        };
        let snap = state.system.snapshot();
        let Some(section) = snap.pureplay.get(index) else {
            return ReconfigureOutcome::RestartRequired;
        };
        let addr = section.address.as_deref().unwrap_or(DEFAULT_ADDRESS);
        if addr.parse::<SocketAddr>().ok() != Some(self.addr)
            || section.full_monitor != self.routing.full_monitor
            || section.chipping_monitor != self.routing.chipping_monitor
            || section.putting_monitor != self.routing.putting_monitor
        {
            return ReconfigureOutcome::RestartRequired;
        }
        ReconfigureOutcome::Applied
    }
}

/// Reconnect backoff: 1 s, +1 s per failed attempt, capped at 3 s, as for
/// the Open Connect client (Pureplay is on loopback or the LAN).
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(3);

/// Oldest a finished shot may be when it is sent, counted from when the
/// actor took it off the bus.
const STALE_SHOT: Duration = Duration::from_secs(5);

/// How long Pureplay has to answer Hello (shorter in tests).
const HELLO_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(500)
} else {
    Duration::from_secs(5)
};

/// Longest a launch monitor counts as processing a shot after its trigger
/// when the shot never finishes.
const PROCESSING_LIMIT: Duration = Duration::from_secs(10);

/// Errors from Pureplay closer together than this are one burst, alerted
/// once: a host that answers every connect with an Error and closes does so
/// on each 1-3 s retry.
const HOST_ERROR_BURST_GAP: Duration = Duration::from_secs(30);

/// How often the loops poll the bus and the shutdown flag while idle.
const IDLE_POLL: Duration = Duration::from_millis(50);

/// Longest a write to Pureplay may block before the link counts as lost.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Longest a connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the handshake failed.
const ERROR: &str = "error";
/// Pureplay's address.
const ADDRESS: &str = "address";
/// The ShotId sent last and when.
const LAST_SHOT: &str = "last_shot";
/// Shots sent on this link.
const SHOTS_SENT: &str = "shots_sent";
/// The ready state last sent on this link.
const READY_STATE: &str = "ready_state";
/// Whether strict mode is on for this link.
const STRICT_MODE: &str = "strict_mode";
/// The club and hand Pureplay set, as the card holds them.
const PLAYER: [&str; 2] = ["club", "handed"];
/// The keys that hold for one link.
const PER_LINK: [&str; 3] = [SHOTS_SENT, READY_STATE, STRICT_MODE];

/// The card's actor telemetry. A key goes out only when its value changes;
/// an empty value removes it.
struct Telemetry {
    reported: HashMap<String, String>,
    pending: HashMap<String, String>,
}

impl Telemetry {
    /// A new instance's telemetry for Pureplay at `addr`: its first status
    /// sets the address and removes what an earlier instance left.
    fn new(addr: SocketAddr) -> Self {
        let mut pending: HashMap<String, String> = [ERROR, LAST_SHOT]
            .into_iter()
            .chain(PER_LINK)
            .chain(PLAYER)
            .map(|k| (k.to_owned(), String::new()))
            .collect();
        pending.insert(ADDRESS.into(), addr.to_string());
        Self {
            reported: HashMap::new(),
            pending,
        }
    }

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

    /// Remove what held for the link that ended.
    fn clear_link(&mut self) {
        for key in PER_LINK.into_iter().chain(PLAYER) {
            self.set(key, String::new());
        }
    }

    /// The changes not yet sent plus every other value the card holds: a
    /// status that starts an instance or ends a link drops what the card
    /// held, so it carries the values that outlast the link.
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

/// A shot sent for the card: its ShotId and the local time.
fn last_shot_text(shot_id: u32) -> String {
    #[cfg(debug_assertions)]
    if let Some(clock) = crate::screens::clock() {
        return format!("#{shot_id} at {clock}");
    }
    format!("#{shot_id} at {}", chrono::Local::now().format("%H:%M:%S"))
}

/// What the actor reads from, and asks of, the rest of the app.
pub(crate) struct Shared<'a> {
    /// The configured launch monitors' names, read fresh when needed.
    pub names: &'a MonitorNames,
    /// The output's display name, read fresh when it asks for a round.
    pub holder: &'a HolderName,
    /// The round policy: tournament rounds, and every shot source's type.
    pub round: RoundPolicy,
    /// Where each shot is recorded before it is written; `None` records
    /// nothing.
    pub sent: Option<Arc<SentShots>>,
    /// The app's state: actor ids, shot sources. `None` in tests that run
    /// without one.
    pub state: Option<Arc<SystemState>>,
}

/// Main loop. Reconnects until the actor is stopped.
pub(crate) fn run_with(
    addr: SocketAddr,
    routing: GsProRouting,
    turn: GolferTurn,
    mut backoff: Backoff,
    shared: Shared<'_>,
    sender: &BusSender,
    mut receiver: BusReceiver,
) {
    let mut relay = Relay::new(routing, shared.round.clone(), shared.state.clone());
    relay.sent.clone_from(&shared.sent);
    relay.feed.turn = Some((turn, None));
    relay.card = sender.actor_id().to_owned();
    // The actors already up republish their state (launch monitors' sources
    // and blockers, flighthook's turn) for this card.
    sender.frp(flightrelay::BusSync::new());
    let mut telemetry = Telemetry::new(addr);
    let mut ever_connected = false;
    loop {
        if relay.drain(&mut receiver).is_err() {
            tracing::info!("pureplay: shutting down");
            return;
        }
        let status = if ever_connected {
            ActorStatus::Reconnecting
        } else {
            ActorStatus::Starting
        };
        sender.local(LocalEvent::CardStatus {
            status,
            telemetry: telemetry.take_all(),
        });

        let result = connect_and_run(
            addr,
            &mut relay,
            &mut telemetry,
            &shared,
            sender,
            &mut receiver,
        );
        let session = relay.disconnect();
        telemetry.clear_link();
        ever_connected |= session.is_some();
        match result {
            Ok(()) | Err(LinkError::Shutdown) => {
                tracing::info!("pureplay: shutting down");
                return;
            }
            Err(e) => {
                let delay = backoff.delay(session);
                match &e {
                    LinkError::Handshake(reason) => telemetry.set(ERROR, reason.clone()),
                    // Pureplay closed or not running: a status, not an error.
                    _ => telemetry.set(ERROR, String::new()),
                }
                tracing::info!("pureplay: {e}, retrying in {delay:?}");
                sender.local(LocalEvent::CardStatus {
                    status: if ever_connected {
                        ActorStatus::Reconnecting
                    } else {
                        ActorStatus::Starting
                    },
                    telemetry: telemetry.take_all(),
                });
                let deadline = Instant::now() + delay;
                loop {
                    if relay.drain(&mut receiver).is_err() {
                        tracing::info!("pureplay: shutting down");
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
    shot: CompletedShot,
    /// The launch monitor's name for the log (its card key).
    name: String,
    finished_at: Instant,
}

/// What flighthook knows about one launch monitor.
#[derive(Debug, Default)]
struct Monitor {
    /// Its class, from the round policy.
    provenance: Option<Source>,
    status: Option<ActorStatus>,
    /// The actor reports an `error`.
    error: bool,
    ready: bool,
    battery: Option<u8>,
    /// When its last shot was triggered, while that shot has not finished.
    processing_since: Option<Instant>,
}

impl Monitor {
    fn connection_state(&self) -> ConnectionState {
        match self.status {
            Some(ActorStatus::Connected) => ConnectionState::Connected,
            _ if self.error => ConnectionState::Error,
            Some(ActorStatus::Starting) => ConnectionState::Connecting,
            Some(ActorStatus::Reconnecting) => ConnectionState::Reconnecting,
            Some(ActorStatus::Disconnected) | None => ConnectionState::Disconnected,
        }
    }

    fn ready_state(&self, now: Instant) -> ReadyState {
        if self.status != Some(ActorStatus::Connected) {
            ReadyState::NotReady
        } else if self
            .processing_since
            .is_some_and(|at| now.saturating_duration_since(at) < PROCESSING_LIMIT)
        {
            ReadyState::Processing
        } else if self.ready {
            ReadyState::Ready
        } else {
            ReadyState::NotReady
        }
    }
}

/// Bus-side state, kept across reconnects so the bus is drained the whole
/// time the actor runs.
struct Relay {
    /// Shots, routing and flighthook's turn from the bus.
    feed: Feed,
    /// Which actors are launch monitors, and of what class.
    round: RoundPolicy,
    /// The app's state, `None` in tests that run without one.
    state: Option<Arc<SystemState>>,
    /// The card key (`pureplay.N`).
    card: String,
    /// Where each shot is recorded before it is written.
    sent: Option<Arc<SentShots>>,
    /// Where Pureplay is while the link is up: where a shot is recorded
    /// as sent to.
    peer: Option<SimAddr>,
    /// What flighthook knows of each launch monitor, by actor id.
    monitors: HashMap<String, Monitor>,
    outbox: VecDeque<PendingShot>,
    /// When the link's handshake finished, `None` while there is no link.
    connected_at: Option<Instant>,
    /// When Pureplay last sent an Error, on any link.
    last_host_error: Option<Instant>,
    /// When strict mode was last refused, on any link.
    last_refusal: Option<Instant>,
    /// What Pureplay's SetClubInfo and SetShotContext gave on this link.
    held: Held,
    /// The distance to the target of Pureplay's last SetShotContext, m.
    distance_m: Option<f64>,
    /// The card is to send its select again.
    reassert: bool,
}

/// Whether an event at `now` starts a new burst after the one `last`
/// ended (events less than [`HOST_ERROR_BURST_GAP`] apart are one burst);
/// records `now` in `last`.
fn new_burst(last: &mut Option<Instant>, now: Instant) -> bool {
    last.replace(now)
        .is_none_or(|at| now.saturating_duration_since(at) >= HOST_ERROR_BURST_GAP)
}

impl Relay {
    fn new(routing: GsProRouting, round: RoundPolicy, state: Option<Arc<SystemState>>) -> Self {
        Self {
            feed: Feed::new(routing, state.clone()),
            round,
            state,
            card: String::new(),
            sent: None,
            peer: None,
            monitors: HashMap::new(),
            outbox: VecDeque::new(),
            connected_at: None,
            last_host_error: None,
            last_refusal: None,
            held: Held::default(),
            distance_m: None,
            reassert: false,
        }
    }

    /// Pureplay's SetClubInfo: the card holds the club and hand, and the
    /// mode they give with the last distance.
    fn hold_club(&mut self, hand: Option<flighthook::Handedness>, club: Option<flighthook::Club>) {
        if hand.is_some() {
            self.held.hand = hand;
        }
        if club.is_some() {
            self.held.club = club;
        }
        self.refresh_mode();
    }

    /// Pureplay's SetShotContext: the distance to the target. Returns
    /// whether it changed the mode the card holds.
    fn hold_distance(&mut self, distance_m: f64) -> bool {
        self.distance_m = Some(distance_m);
        let before = self.held.mode;
        self.refresh_mode();
        self.held.mode != before
    }

    /// The held mode, from the held club and the last distance: none while
    /// the card holds no club.
    fn refresh_mode(&mut self) {
        self.held.mode = self
            .held
            .club
            .map(|club| mode_for_lie(Some(club), self.distance_m));
    }

    /// Record an Error from Pureplay at `now`. Returns whether it starts a
    /// new burst (and so is alerted).
    fn host_error(&mut self, now: Instant) -> bool {
        new_burst(&mut self.last_host_error, now)
    }

    /// Record a strict-mode refusal at `now`. Returns whether it starts a
    /// new burst (and so is alerted): Pureplay closes the link after a
    /// refusal, and every reconnect asks again.
    fn refusal(&mut self, now: Instant) -> bool {
        new_burst(&mut self.last_refusal, now)
    }

    /// The launch monitors whose shots reach this output, in any mode, with
    /// their display names (a remote one's `bus.actor` name; blank when
    /// they have none), for a round request:
    /// "Any" routing stands for every configured launch monitor that is on
    /// (`names`) and every one seen on the bus, and a monitor routed by ID
    /// counts when it is one of those. One turned off or removed feeds
    /// nothing, nor does a simulator source ([`Feed::feeds_simulators`]).
    fn feeds<'a>(&'a self, names: &'a HashMap<String, String>) -> Vec<(&'a str, &'a str)> {
        let known: BTreeSet<&str> = names
            .keys()
            .chain(self.monitors.keys())
            .map(String::as_str)
            .filter(|id| self.feed.feeds_simulators(id))
            .collect();
        self.feed
            .routing
            .feeds(known.clone())
            .into_iter()
            .filter(|id| known.contains(id))
            .map(|id| {
                let name = names
                    .get(id)
                    .map(String::as_str)
                    .or_else(|| self.feed.monitor(id).map(|a| a.name.as_str()));
                (id, name.unwrap_or(""))
            })
            .collect()
    }

    fn connect(&mut self, now: Instant) {
        self.connected_at = Some(now);
    }

    /// How long the link lasted, `None` if there was none.
    fn disconnect(&mut self) -> Option<Duration> {
        self.peer = None;
        self.held = Held::default();
        self.distance_m = None;
        self.reassert = false;
        self.connected_at.take().map(|at| at.elapsed())
    }

    /// Record shot `key` in the send registry as written to Pureplay at
    /// `now`; nothing without a registry or a link.
    fn record_send(&self, key: ShotKey, now: Instant) {
        if let (Some(sent), Some(peer)) = (&self.sent, self.peer) {
            sent.record(key, peer, now);
        }
    }

    /// Take shot `key`'s record back: its ball data was not written.
    fn forget_send(&self, key: &ShotKey) {
        if let (Some(sent), Some(peer)) = (&self.sent, self.peer) {
            sent.forget(key, peer);
        }
    }

    fn drain(&mut self, receiver: &mut BusReceiver) -> Result<(), LinkError> {
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => return Err(LinkError::Shutdown),
                Ok(None) => break,
                Ok(Some(msg)) => self.handle(&msg, Instant::now()),
            }
        }
        // A monitor removed from the config is not a known source any more,
        // so it does not feed this output.
        let (round, feed) = (&self.round, &self.feed);
        self.monitors
            .retain(|id, _| round.source(id).is_some() || feed.monitor(id).is_some());
        Ok(())
    }

    /// The launch monitor `actor`, `None` for an actor that is none or is a
    /// simulator source ([`Feed::feeds_simulators`]).
    fn monitor(&mut self, actor: &str) -> Option<&mut Monitor> {
        if !self.feed.feeds_simulators(actor) {
            return None;
        }
        let provenance = match self.round.source(actor) {
            Some(source) if source.is_launch_monitor() => Some(source.provenance()),
            Some(_) => return None,
            None => self
                .feed
                .monitor(actor)?
                .device
                .as_ref()
                .and_then(|d| d.source),
        };
        Some(
            self.monitors
                .entry(actor.to_owned())
                .or_insert_with(|| Monitor {
                    provenance,
                    ..Monitor::default()
                }),
        )
    }

    /// The name of launch monitor `actor` for the log: its card key, else
    /// the id.
    fn monitor_name(&self, actor: &str) -> String {
        self.state
            .as_ref()
            .and_then(|s| s.card_of_actor(actor))
            .unwrap_or_else(|| actor.to_owned())
    }

    fn handle(&mut self, msg: &BusMsg, now: Instant) {
        match msg {
            BusMsg::Local {
                card,
                event: LocalEvent::CardStatus { status, telemetry },
            } => {
                let Some(actor) = self.state.as_ref().and_then(|s| s.actor_of_card(card)) else {
                    return;
                };
                if let Some(m) = self.monitor(&actor) {
                    // As on the monitor's card: Starting, and a drop from
                    // Connected, end what its earlier statuses said; any
                    // other status changes only the keys it carries.
                    let ends_session = *status == ActorStatus::Starting
                        || (m.status == Some(ActorStatus::Connected)
                            && matches!(
                                status,
                                ActorStatus::Disconnected | ActorStatus::Reconnecting
                            ));
                    if ends_session {
                        m.error = false;
                    }
                    if let Some(error) = telemetry.get("error") {
                        m.error = !error.is_empty();
                    }
                    m.status = Some(*status);
                    if *status != ActorStatus::Connected {
                        m.ready = false;
                        m.processing_since = None;
                    }
                }
                return;
            }
            BusMsg::Local {
                event: LocalEvent::ActorGone { .. },
                ..
            } => {}
            BusMsg::Local { .. } => return,
            BusMsg::Frp { msg: frp, .. } => match &frp.body {
                FrpBody::ShotTrigger(_) => {
                    if let Some(m) = self.monitor(&frp.from) {
                        m.processing_since = Some(now);
                    }
                }
                FrpBody::ShotFinished(_) => {
                    if let Some(m) = self.monitor(&frp.from) {
                        m.processing_since = None;
                    }
                }
                _ => {}
            },
        }
        let event = self.feed.handle(msg);
        // A launch monitor's `bus.actor`: its readiness and battery.
        if let BusMsg::Frp { msg: frp, .. } = msg
            && let FrpBody::BusActor(actor) = &frp.body
            && actor.has_role(flightrelay::types::roles::LAUNCH_MONITOR)
        {
            let (ready, battery) = (
                actor.is_clear(),
                actor.device.as_ref().and_then(|d| d.battery_pct),
            );
            if let Some(m) = self.monitor(&frp.from) {
                m.ready = ready;
                // A monitor that reports itself ready is connected, also
                // when its Connected status went out before this actor
                // started.
                if m.ready && m.status.is_none() {
                    m.status = Some(ActorStatus::Connected);
                }
                m.battery = battery.filter(|p| *p <= 100);
            }
        }
        match event {
            Some(FeedEvent::Shot(done)) => {
                let name = self.monitor_name(&done.shot.from);
                if self.connected_at.is_some() {
                    self.outbox.push_back(PendingShot {
                        key: done.shot.key.clone(),
                        shot: done.shot,
                        name,
                        finished_at: now,
                    });
                } else {
                    tracing::debug!(
                        "pureplay: not connected, dropping shot #{} from '{name}'",
                        done.shot.key.shot_number,
                    );
                }
            }
            Some(FeedEvent::Skipped(shot)) => {
                tracing::debug!(
                    "pureplay: skipping shot #{} from '{}' (not routed for {:?})",
                    shot.key.shot_number,
                    self.monitor_name(&shot.from),
                    self.feed.mode(),
                );
            }
            Some(FeedEvent::NoData(shot)) => {
                tracing::info!(
                    "pureplay: shot #{} from '{}' has no data (no read), not sent",
                    shot.key.shot_number,
                    self.monitor_name(&shot.from),
                );
            }
            Some(FeedEvent::Turn) => {
                // The latest turn decides: one that matches what the card
                // holds cancels a re-assert an earlier one asked for.
                let state = self.state.clone();
                self.refresh_mode();
                self.reassert = self.feed.turn.as_ref().is_some_and(|(turn, cause)| {
                    self.connected_at.is_some()
                        && self.held.reasserts(turn, cause.as_deref(), |a| {
                            state.as_ref().is_some_and(|s| s.is_game_card(a))
                        })
                });
            }
            None => {}
        }
    }

    /// The launch monitor the current mode is routed to, `None` for Any.
    fn routed(&self) -> Option<&str> {
        self.feed.routed(self.feed.mode())
    }

    /// The monitors the device's state is read from: the routed one, or
    /// every launch monitor for Any; during a round (`round`) only those it
    /// counts.
    fn subjects(&self, round: Option<&RoundGuard>) -> Vec<&Monitor> {
        let counted = |m: &&Monitor| round.is_none_or(|r| r.counts(m.provenance));
        match self.routed() {
            Some(id) => self.monitors.get(id).into_iter().filter(counted).collect(),
            None => self.monitors.values().filter(counted).collect(),
        }
    }

    /// The ready state: the routed monitor's, or for Any, Processing when
    /// any monitor is, else Ready when any is.
    fn ready_state(&self, now: Instant, round: Option<&RoundGuard>) -> ReadyState {
        let states: Vec<ReadyState> = self
            .subjects(round)
            .iter()
            .map(|m| m.ready_state(now))
            .collect();
        [ReadyState::Processing, ReadyState::Ready]
            .into_iter()
            .find(|s| states.contains(s))
            .unwrap_or(ReadyState::NotReady)
    }

    /// The connection state: the routed monitor's, or for Any the best of
    /// every monitor's.
    fn connection_state(&self, round: Option<&RoundGuard>) -> ConnectionState {
        let states: Vec<ConnectionState> = self
            .subjects(round)
            .iter()
            .map(|m| m.connection_state())
            .collect();
        [
            ConnectionState::Connected,
            ConnectionState::Connecting,
            ConnectionState::Reconnecting,
            ConnectionState::Error,
        ]
        .into_iter()
        .find(|s| states.contains(s))
        .unwrap_or(ConnectionState::Disconnected)
    }

    /// The monitor that stands for the device in Hello and the battery: the
    /// routed one, or for Any the only configured one.
    fn named<'a>(&self, names: &'a HashMap<String, String>) -> Option<(&'a str, &'a str)> {
        match self.routed() {
            Some(id) => names.get_key_value(id),
            None if names.len() == 1 => names.iter().next(),
            None => None,
        }
        .map(|(id, name)| (id.as_str(), name.as_str()))
    }

    /// That monitor's battery, 0 to 100, while it is connected.
    fn battery(&self, names: &HashMap<String, String>) -> Option<u8> {
        let (id, _) = self.named(names)?;
        let m = self.monitors.get(id)?;
        (m.status == Some(ActorStatus::Connected))
            .then_some(m.battery)
            .flatten()
    }

    /// The Hello for a new link.
    fn hello(&self, names: &HashMap<String, String>) -> Message {
        let named = self.named(names);
        let battery = named
            .and_then(|(id, _)| self.monitors.get(id))
            .is_some_and(|m| m.battery.is_some());
        Message::Hello {
            protocol_version: codec::VERSION,
            device_caps: if battery {
                codec::CAP_BATTERY_POWERED
            } else {
                0
            },
            device_name: match named.map(|(_, n)| n.trim()) {
                Some(name) if !name.is_empty() => format!("{DEVICE_NAME} ({name})"),
                _ => DEVICE_NAME.to_owned(),
            },
            firmware: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// The next shot to send, oldest first; stale shots are dropped.
    #[cfg(test)]
    fn next_shot(&mut self, now: Instant) -> Option<OutputShot> {
        self.next_send(now).map(|(_, shot)| shot)
    }

    /// [`next_shot`](Self::next_shot), with the shot's key, as it goes out
    /// outside a round.
    #[cfg(test)]
    fn next_send(&mut self, now: Instant) -> Option<(ShotKey, OutputShot)> {
        self.next_pending(now).map(|(key, shot, name)| {
            let auto_chip = self
                .state
                .as_ref()
                .map_or_else(|| "autochip".to_owned(), |s| s.auto_chip_id());
            (key, output_shot(&shot, &name, &auto_chip))
        })
    }

    /// The next shot to send as it finished, oldest first, with its key and
    /// its launch monitor's name; stale shots are dropped.
    fn next_pending(&mut self, now: Instant) -> Option<(ShotKey, CompletedShot, String)> {
        while let Some(pending) = self.outbox.pop_front() {
            let age = now.saturating_duration_since(pending.finished_at);
            if age <= STALE_SHOT {
                return Some((pending.key, pending.shot, pending.name));
            }
            tracing::debug!(
                "pureplay: dropping shot #{} from '{}', finished {age:?} ago",
                pending.key.shot_number,
                pending.name,
            );
        }
        None
    }
}

/// One link's state, from the TCP connect to its end.
struct Link<'a, S> {
    stream: &'a mut S,
    name: &'a str,
    reader: FrameReader,
    /// HelloAck received.
    handshaken: bool,
    /// The tournament round, held while strict mode is on.
    round: Option<RoundGuard>,
    /// The ShotId given out last on this link.
    last_shot_id: u32,
    shots_sent: u32,
    /// The ready state last sent.
    ready: Option<ReadyState>,
    /// A frame with its reserved byte set was logged.
    reserved_seen: bool,
    /// This link refused strict mode: Pureplay's StrictModeRequired error
    /// that follows is expected, and not alerted again.
    refused: bool,
    /// Sources whose shots strict mode kept back on this link (logged once
    /// each).
    withheld: HashSet<String>,
    /// The selects this link sent.
    selects: u64,
}

impl<'a, S: Read + Write> Link<'a, S> {
    fn new(stream: &'a mut S, name: &'a str) -> Self {
        Self {
            stream,
            name,
            reader: FrameReader::new(),
            handshaken: false,
            round: None,
            last_shot_id: 0,
            shots_sent: 0,
            ready: None,
            reserved_seen: false,
            refused: false,
            withheld: HashSet::new(),
            selects: 0,
        }
    }

    fn send(&mut self, msg: &Message) -> Result<(), LinkError> {
        let frame = msg.to_frame();
        tracing::info!(
            target: "audit",
            "{} sent {} {} | {msg:?}",
            self.name,
            codec::kind_name(msg.kind()),
            codec::hex(&frame),
        );
        self.stream.write_all(&frame).map_err(LinkError::Io)?;
        self.stream.flush().map_err(LinkError::Io)
    }

    /// Read what has arrived, waiting up to the stream's read timeout.
    fn read(&mut self) -> Result<(), LinkError> {
        let mut buf = [0u8; 4096];
        match self.stream.read(&mut buf) {
            Ok(0) => Err(LinkError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection closed by Pureplay",
            ))),
            Ok(n) => {
                self.reader.push(&buf[..n]);
                Ok(())
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(LinkError::Io(e)),
        }
    }

    /// Log the bytes the reader skipped since the last call, as hex.
    fn log_skipped(&mut self) {
        let dropped = self.reader.take_dropped();
        if !dropped.is_empty() {
            tracing::warn!(
                target: "audit",
                "{} skipped {} bytes not in a frame: {}",
                self.name,
                dropped.len(),
                codec::hex(&dropped),
            );
        }
    }

    /// Handle every complete frame received.
    fn handle_frames(
        &mut self,
        relay: &mut Relay,
        names: &HashMap<String, String>,
        shared: &Shared<'_>,
        telemetry: &mut Telemetry,
        sender: &BusSender,
    ) -> Result<(), LinkError> {
        while let Some(frame) = self.reader.next_frame() {
            self.log_skipped();
            if frame.reserved != 0 && !self.reserved_seen {
                self.reserved_seen = true;
                tracing::warn!(
                    "pureplay <- a frame with reserved byte 0x{:02X}; Pureplay Connect v1 \
                     sends 0, read anyway",
                    frame.reserved
                );
            }
            let decoded = Message::decode(frame.kind, &frame.payload);
            tracing::info!(
                target: "audit",
                "{} received {} {} | {}",
                self.name,
                codec::kind_name(frame.kind),
                codec::hex(&frame.bytes),
                match &decoded {
                    Ok(msg) => format!("{msg:?}"),
                    Err(e) => e.to_string(),
                },
            );
            match decoded {
                Ok(msg) => self.handle(msg, relay, names, shared, telemetry, sender)?,
                Err(e) => {
                    tracing::warn!("pureplay <- {e}");
                    self.send(&Message::Error {
                        code: codec::ERROR_GENERIC,
                        message: e.to_string(),
                    })?;
                }
            }
        }
        // Bytes with no frame behind them yet.
        self.log_skipped();
        Ok(())
    }

    fn handle(
        &mut self,
        msg: Message,
        relay: &mut Relay,
        names: &HashMap<String, String>,
        shared: &Shared<'_>,
        telemetry: &mut Telemetry,
        sender: &BusSender,
    ) -> Result<(), LinkError> {
        match msg {
            Message::HelloAck {
                protocol_version, ..
            } => {
                if self.handshaken {
                    tracing::debug!("pureplay <- HelloAck again, ignored");
                } else if protocol_version == codec::VERSION {
                    self.handshaken = true;
                } else {
                    return Err(LinkError::Handshake(format!(
                        "Pureplay chose protocol version {protocol_version}; flighthook speaks \
                         version {}",
                        codec::VERSION
                    )));
                }
            }
            Message::Ping => {
                let round = self.round.as_ref();
                let heartbeat = Message::Heartbeat {
                    connection_state: relay.connection_state(round).byte(),
                    ready_state: relay.ready_state(Instant::now(), round).byte(),
                    battery: relay.battery(names).unwrap_or(codec::BATTERY_UNKNOWN),
                };
                self.send(&heartbeat)?;
            }
            Message::SetClubInfo { handedness, club } => {
                let (hand, club) = set_club_info(handedness, club);
                relay.hold_club(hand, club);
                self.select(relay, sender);
                telemetry.player(&relay.held);
            }
            Message::SetDetect { mode } => {
                tracing::info!(
                    "pureplay <- SetDetect {mode} ({}), not acted on",
                    match mode {
                        0 => "disarm",
                        1 => "arm",
                        _ => "unknown",
                    }
                );
            }
            Message::SetBallType { ball_type } => {
                tracing::info!(
                    "pureplay <- SetBallType {ball_type} ({}), not acted on",
                    match ball_type {
                        0 => "marker",
                        1 => "markerless",
                        _ => "unknown",
                    }
                );
            }
            Message::SetShotContext {
                distance_to_target_m,
            } => {
                tracing::info!("pureplay <- SetShotContext {distance_to_target_m} m to the target");
                if relay.hold_distance(f64::from(distance_to_target_m)) {
                    self.select(relay, sender);
                }
            }
            Message::SetStrictMode { strict } => {
                // A new guard replaces the one held before it lets go, so a
                // repeated request does not end the round in between.
                let refusal = if strict == 0 {
                    self.round = None;
                    None
                } else {
                    let names = (shared.names)();
                    let holder = (shared.holder)();
                    let request = RoundRequest {
                        holder: &holder,
                        feeds: &relay.feeds(&names),
                    };
                    match shared.round.begin_round(&request) {
                        Ok(guard) => {
                            self.round = Some(guard);
                            None
                        }
                        Err(refusal) => {
                            self.round = None;
                            Some(refusal.reason().to_owned())
                        }
                    }
                };
                self.send(&Message::StrictModeAck {
                    accepted: refusal.is_none(),
                    reason: refusal.clone().unwrap_or_default(),
                })?;
                match refusal {
                    Some(reason) => {
                        self.refused = true;
                        tracing::warn!("pureplay <- SetStrictMode {strict}: refused, {reason}");
                        if relay.refusal(Instant::now()) {
                            sender.alert(
                                Severity::Warn,
                                format!(
                                    "Pureplay strict mode refused: {reason}. Tournament rounds \
                                     take only shots a launch monitor measured."
                                ),
                            );
                        }
                    }
                    None => tracing::info!(
                        "pureplay <- SetStrictMode {strict}: strict mode {}",
                        if self.round.is_some() { "on" } else { "off" }
                    ),
                }
                telemetry.set(STRICT_MODE, on_off(self.round.is_some()));
            }
            Message::Error { code, message } => {
                let what = match code {
                    codec::ERROR_STRICT_MODE_REQUIRED => " (strict mode required)",
                    _ => "",
                };
                tracing::warn!("pureplay <- error {code}{what}: {message}");
                // After a refusal the StrictModeRequired error is expected,
                // and the refusal has its own alert.
                let expected = self.refused && code == codec::ERROR_STRICT_MODE_REQUIRED;
                if relay.host_error(Instant::now()) && !expected {
                    sender.alert(
                        Severity::Warn,
                        format!("Pureplay error {code}{what}: {message}"),
                    );
                }
            }
            // Device-to-host messages are not commands for the device.
            other => {
                let kind = other.kind();
                tracing::warn!(
                    "pureplay <- unexpected {} (0x{kind:02X})",
                    codec::kind_name(kind)
                );
                self.send(&Message::Error {
                    code: codec::ERROR_GENERIC,
                    message: format!("unexpected message type 0x{kind:02X}"),
                })?;
            }
        }
        Ok(())
    }

    /// Send the card's complete `golfer.select`: everything it holds.
    fn select(&mut self, relay: &Relay, sender: &BusSender) {
        if relay.held.is_empty() {
            return;
        }
        self.selects += 1;
        sender.frp(
            relay
                .held
                .select(format!("{}-{}", relay.card, self.selects)),
        );
    }

    /// Push the ready state when it differs from the one last sent.
    fn push_ready(
        &mut self,
        relay: &Relay,
        now: Instant,
        telemetry: &mut Telemetry,
    ) -> Result<(), LinkError> {
        let state = relay.ready_state(now, self.round.as_ref());
        if self.ready != Some(state) {
            self.send(&Message::ReadyState(state.byte()))?;
            self.ready = Some(state);
            telemetry.set(READY_STATE, state.label().to_owned());
        }
        Ok(())
    }

    /// Log a shot the round does not count: once per source at info, then
    /// at debug.
    fn withhold(&mut self, shot: &OutputShot) {
        if self.withheld.insert(shot.actor.clone()) {
            tracing::info!(
                "pureplay: strict mode, shots from '{}' are not sent: {}",
                shot.actor,
                not_measured_reason(shot.source, &shot.actor).unwrap_or_default(),
            );
        } else {
            tracing::debug!(
                "pureplay: strict mode, shot #{} from '{}' not sent",
                shot.shot_number,
                shot.actor,
            );
        }
    }

    /// Send every shot waiting, oldest first.
    fn send_shots(
        &mut self,
        relay: &mut Relay,
        now: Instant,
        telemetry: &mut Telemetry,
    ) -> Result<(), LinkError> {
        while let Some((key, done, name)) = relay.next_pending(now) {
            let shot_id = self.last_shot_id + 1;
            let (shot, frames) = match &self.round {
                None => {
                    let auto_chip = relay
                        .state
                        .as_ref()
                        .map_or_else(|| "autochip".to_owned(), |s| s.auto_chip_id());
                    let shot = output_shot(&done, &name, &auto_chip);
                    let frames = mapper::map_shot(&shot, shot_id);
                    (shot, frames)
                }
                Some(round) => {
                    // The measured-only receiver: the launch monitor's own
                    // launch, club and impact, from a measured source only.
                    let shot = measured_shot(&done, &name);
                    if !round.counts(shot.source) {
                        self.withhold(&shot);
                        continue;
                    }
                    let frames = mapper::map_shot(&shot, shot_id);
                    (shot, frames)
                }
            };
            let Some(frames) = frames else {
                tracing::debug!(
                    "pureplay: shot #{} from '{}' has no ball data, not sent",
                    shot.shot_number,
                    shot.actor,
                );
                continue;
            };
            self.last_shot_id = shot_id;
            relay.record_send(key.clone(), now);
            tracing::info!(
                "pureplay -> ShotId {shot_id} (shot #{} from '{}'){}{}",
                shot.shot_number,
                shot.actor,
                if frames.club.is_some() {
                    " with club data"
                } else {
                    ""
                },
                if self.round.is_some() { ", strict" } else { "" },
            );
            if let Err(e) = self.send(&Message::BallData(frames.ball)) {
                relay.forget_send(&key);
                return Err(e);
            }
            if let Some(club) = frames.club {
                self.send(&Message::ClubData(club))?;
            }
            self.shots_sent += 1;
            telemetry.set(SHOTS_SENT, self.shots_sent.to_string());
            telemetry.set(LAST_SHOT, last_shot_text(shot_id));
        }
        Ok(())
    }
}

fn on_off(on: bool) -> String {
    if on { "on" } else { "off" }.to_owned()
}

/// The hand and club of a SetClubInfo. A value Pureplay Connect v1 does not
/// define is logged and left out.
fn set_club_info(
    handedness: u8,
    club: u8,
) -> (Option<flighthook::Handedness>, Option<flighthook::Club>) {
    let hand = match codec::Handedness::from_byte(handedness) {
        Some(codec::Handedness::Right) => Some(flighthook::Handedness::Right),
        Some(codec::Handedness::Left) => Some(flighthook::Handedness::Left),
        None => {
            tracing::warn!("pureplay <- SetClubInfo: unknown handedness {handedness}, ignored");
            None
        }
    };
    let known = mapper::club_from_pureplay(club);
    if known.is_none() {
        tracing::warn!("pureplay <- SetClubInfo: unknown club {club}, ignored");
    }
    (hand, known)
}

/// Connect and run one link.
fn connect_and_run(
    addr: SocketAddr,
    relay: &mut Relay,
    telemetry: &mut Telemetry,
    shared: &Shared<'_>,
    sender: &BusSender,
    receiver: &mut BusReceiver,
) -> Result<(), LinkError> {
    let mut stream = super::connect_tcp(sender.actor_id(), addr, CONNECT_TIMEOUT, receiver)
        .map_err(LinkError::Io)?
        .ok_or(LinkError::Shutdown)?;
    stream.set_nodelay(true).map_err(LinkError::Io)?;
    stream
        .set_read_timeout(Some(IDLE_POLL))
        .map_err(LinkError::Io)?;
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(LinkError::Io)?;
    session(
        &mut stream,
        addr,
        relay,
        telemetry,
        shared,
        sender,
        receiver,
    )
}

/// One link: handshake, then shots, state and commands until it ends.
fn session(
    stream: &mut TcpStream,
    addr: SocketAddr,
    relay: &mut Relay,
    telemetry: &mut Telemetry,
    shared: &Shared<'_>,
    sender: &BusSender,
    receiver: &mut BusReceiver,
) -> Result<(), LinkError> {
    let name = sender.actor_id().to_owned();
    let stream_peer = stream.peer_addr().ok();
    let stream_local = stream.local_addr().ok();
    let mut link = Link::new(stream, &name);
    relay.drain(receiver)?;
    let names = (shared.names)();
    link.send(&relay.hello(&names))?;
    let hello_at = Instant::now();
    while !link.handshaken {
        if receiver.is_shutdown() {
            return Err(LinkError::Shutdown);
        }
        if hello_at.elapsed() >= HELLO_TIMEOUT {
            return Err(LinkError::Handshake(format!(
                "no reply to Hello from {addr} within {:?}; is Pureplay listening there?",
                HELLO_TIMEOUT
            )));
        }
        link.read()?;
        link.handle_frames(relay, &names, shared, telemetry, sender)?;
        relay.drain(receiver)?;
    }

    // Shots that finished before the handshake finished while there was no
    // link.
    relay.drain(receiver)?;
    relay.connect(Instant::now());
    relay.peer = Some(SimAddr::of(stream_local, stream_peer.unwrap_or(addr)));
    tracing::info!("pureplay: connected to {addr}");
    telemetry.set(ERROR, String::new());
    telemetry.set(SHOTS_SENT, "0".into());
    telemetry.set(STRICT_MODE, on_off(false));
    link.push_ready(relay, Instant::now(), telemetry)?;
    sender.local(LocalEvent::CardStatus {
        status: ActorStatus::Connected,
        telemetry: telemetry.take(),
    });

    loop {
        if receiver.is_shutdown() {
            return Err(LinkError::Shutdown);
        }
        // The read waits up to IDLE_POLL when nothing comes in, which paces
        // the loop.
        link.read()?;
        link.handle_frames(relay, &names, shared, telemetry, sender)?;
        relay.drain(receiver)?;
        if std::mem::take(&mut relay.reassert) {
            link.select(relay, sender);
        }
        let now = Instant::now();
        link.send_shots(relay, now, telemetry)?;
        link.push_ready(relay, now, telemetry)?;
        if !telemetry.pending.is_empty() {
            sender.local(LocalEvent::CardStatus {
                status: ActorStatus::Connected,
                telemetry: telemetry.take(),
            });
        }
    }
}

#[cfg(test)]
mod tests;
