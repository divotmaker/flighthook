//! The Pureplay Accessory card (beta): Pureplay's Accessory API, actor
//! `pureplay_accessory.N`, config `[pureplay_accessory.N]`.
//!
//! Pureplay listens on a WebSocket (`ws://<host>:9770/`, subprotocol
//! `binary`); the card connects, waits for `hello`, checks its protocol and
//! version, and pairs: `pair` with the saved token, or without one, which
//! asks the player to approve flighthook in Pureplay. The token Pureplay
//! gives is kept in the token file ([`token`]), never the config, and is
//! replaced by `<redacted>` in every frame before the frame is logged or put
//! on the bus; a `paired` frame's token is learned before the frame is
//! parsed, and a frame that is not JSON is logged by its length only.
//! Paired, the card subscribes with a snapshot and follows the round
//! ([`model`]). It is the bus's `game` and issues the `pureplay` golfer
//! namespace: every player as `golfer.profile` `pureplay.<name>` (or
//! `pureplay.player-<playerId>`), the round as `game.round`, the ball about
//! to be played as `game.ball` and the scores as `game.scores`
//! ([`game`]). For the local player up it sends one complete
//! `golfer.select` (golfer, club, hand, mode) on each turn, player and club
//! event, and again whenever flighthook's turn differs from it, unless a
//! game or simulator card caused that turn. The mode is the club +
//! distance rule: the putter putts, a ball under 30 yd from the hole
//! chips, anything else is a full swing. A remote player up sends
//! nothing. `game.aim` is answered `unsupported`.
//!
//! Club out ([`club_out`]): a club picked in flighthook (the header, the
//! API, a device; never a simulator's or the card's own) goes to Pureplay
//! as `club.set` with the bag entry's id and `playerId` (or `club.next` /
//! `club.prev` steps), when the pairing grants `control` (the player chose
//! Aim and Club), Pureplay offers the command, its "Let Apps Change Aim and
//! Club" switch is on and the player who is up has a bag; a pick Pureplay
//! cannot take yet waits for it. A pick that cannot be applied gets the
//! card's complete select with Pureplay's club. The card sends no other
//! command.
//!
//! Shots ([`shots`]): a local player's `shot.launched` is a candidate, and
//! the card's Shots setting decides it: taken as a flighthook shot
//! (`shot_trigger` with no timestamp, `launch`, `shot_finished`, with the
//! card's own key), matched as the echo of a shot a simulator output sent,
//! or ignored. A matched echo puts nothing on the bus. Pureplay's
//! `shot.completed` goes on the bus as `game.stroke`, with the launch
//! monitor's `shot_id` of a taken shot or a matched echo (none for a
//! Gimme, AutoPutt or PenaltyOnly); a `shot.cancelled` `discarded` sends no
//! stroke. Shots come only from live events, never from a snapshot; the
//! shot ids survive a reconnect to the same Pureplay
//! (`serverInstanceId`). The card is a virtual launch monitor
//! ([`ShotSource::PureplayAccessory`](super::provenance::ShotSource)) and
//! a game: no simulator output takes its shots or its readiness.
//! Pureplay's readiness blockers are the card's `bus.actor` `blockers`.
//!
//! The server sends JSON in binary frames; the card reads binary and text
//! and sends text ([`SEND_BINARY`] switches that).
//!
//! Connection, pairing and refusals ([`End`]). A refused response decides
//! with its close code, or by itself when the socket ends without one:
//!
//! - Refused, or dropped without a close frame (`1006`: Pureplay closed,
//!   accessories turned off, settings changed): Reconnecting, backoff 1, 2,
//!   4 ... 30 s, reset once a connection has held for 30 s; the token kept.
//! - `4429` before `hello` (too many apps connected): Reconnecting, the
//!   `pairing` row says so, backoff.
//! - A `hello` that is not Accessory version 1, or that lists none of the
//!   events the card follows: Reconnecting with an `error`, retried every
//!   30 s. The `error` goes once Pureplay is unreachable.
//! - Waiting for approval (queued, then `pairing.pending` counting down):
//!   Starting, the `pairing` row says so. Only a pair without a token waits
//!   without limit; with a token the card gives up after 5 s.
//! - `bad_request` to `pair` (another app on this address is waiting):
//!   Reconnecting, retried in 10 s; after the sixth, Disconnected with
//!   Pureplay's message.
//! - A refusal (`forbidden_scope`/`unauthorised` then `4403`, `unauthorised`
//!   then `4408`), or a removal in Pureplay (`unauthorised` with `id: null`
//!   mid-session, then `4401`, which also deletes the token): Disconnected,
//!   the `pairing` row says why, no retry until the user's Reconnect (a new
//!   actor start).
//! - `rate_limited` then `4429` (Pureplay's cooldown): Disconnected, the row
//!   counts the wait down, then the attempt is made once more.
//! - `unauthorised` echoing the `pair` then `4401` (the saved token is
//!   dead): the token is deleted and the card pairs once without one.
//!
//! A pair without a token is sent at most once per actor start by itself;
//! after a refusal only a Reconnect starts another.
//!
//! While connected, a `seq` gap, `context.invalidated`, `match.started` or
//! an event naming a player the card does not hold asks for a fresh
//! snapshot (`state.get`): one unanswered at a time, at least a second
//! apart. A player a snapshot asked for did not bring is asked for again
//! only after `match.started`, `context.invalidated`, `player.joined` or a
//! reconnect. A request that gets no snapshot (no answer in 5 s, or
//! refused) marks nobody: it is sent again after 5, 10, then every 30 s (or
//! the refusal's `retryAfterMs` when longer) until a snapshot comes, with
//! the players it asked for and any named meanwhile. A link that has sent nothing for 20 s gets a WebSocket ping; after
//! two unanswered it counts as lost.
//!
//! The card's telemetry while connected (keys only while they have a
//! value): `name`, `club`, `handed` (the active local player), `hole`,
//! `blockers` (what keeps Pureplay's shot from being played), `shots` (the
//! Shots setting, and in Auto where the last shot came from),
//! `club_out` (whether a club picked in flighthook goes to Pureplay: `on`,
//! `read only`, `off in Pureplay`, `not supported`), `missed_events` (`seq`
//! gaps), `pureplay` (the game's version);
//! `last_shot` (the shot the card took last and when) stays after the
//! connection. `error` holds a version mismatch or a token file that could
//! not be written.

pub mod club_out;
pub mod game;
pub mod model;
pub mod protocol;
pub mod shots;
pub mod token;

use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::{HandshakeError, Message, WebSocket};

use super::provenance::ShotSource;
use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, LocalEvent, PollError};
use crate::state::SystemState;
use crate::state::sent_shots::SimAddr;
use club_out::{Action, ClubOut};
use flighthook::{ActorStatus, Club, PureplayShots, ShotKey};
use flightrelay::types::{blockers, roles};
use flightrelay::{
    BusActor, BusReply, DEFAULT_HEARTBEAT, DeviceInfo, FrpBody, ReplyError, ShotFinished,
    ShotLaunch, ShotTrigger,
};
use model::{Model, Want};
use protocol::{Hello, Incoming, Paired, ResponseError};
use shots::{Decision, Flown, ShotMap};
use token::{Token, TokenStore};

/// Pureplay's default Accessory address.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:9770";

/// Whether the card sends its JSON in binary frames rather than text ones.
/// Pureplay's example clients send text.
pub const SEND_BINARY: bool = false;

/// Whether the card pings a link that has gone quiet ([`PING_EVERY`]).
pub const PING: bool = true;

/// `ms` in a build, `test_ms` in tests.
const fn timing(ms: u64, test_ms: u64) -> Duration {
    if cfg!(test) {
        Duration::from_millis(test_ms)
    } else {
        Duration::from_millis(ms)
    }
}

/// Reconnect backoff: 1, 2, 4 ... seconds, capped.
const MIN_BACKOFF: Duration = timing(1_000, 40);
const MAX_BACKOFF: Duration = timing(30_000, 320);
/// A connection that held this long resets the backoff.
const STABLE: Duration = timing(30_000, 320);
/// A `hello` from another protocol or version is retried this often.
const VERSION_RETRY: Duration = timing(30_000, 150);
/// After `bad_request` to `pair` the card tries again this much later.
const BAD_REQUEST_RETRY: Duration = timing(10_000, 150);
/// `bad_request` answers in a row after which the card stops asking.
const BAD_REQUEST_TRIES: u32 = 6;
/// Longest a TCP connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest the WebSocket handshake may take.
const WS_HANDSHAKE_TIMEOUT: Duration = timing(2_500, 500);
/// Longest `hello` may take after the handshake.
const HELLO_TIMEOUT: Duration = timing(5_000, 1_000);
/// Longest a `pair` with a token may wait for its answer.
const PAIR_TIMEOUT: Duration = timing(5_000, 1_000);
/// How long a refused `pair` waits for its close frame before the response
/// decides by itself.
const CLOSE_GRACE: Duration = timing(1_000, 200);
/// Longest the snapshot may take after `subscribe`, and a `state.get`'s
/// answer.
const SNAPSHOT_TIMEOUT: Duration = timing(5_000, 1_000);
/// Least time between two `state.get`.
const STATE_GET_GAP: Duration = timing(1_000, 100);
/// The waits before a `state.get` that got no snapshot (no answer in
/// [`SNAPSHOT_TIMEOUT`], or refused) is sent again: one per failure in a
/// row, the last one repeating until a snapshot comes.
const STATE_GET_RETRY: [Duration; 3] =
    [timing(5_000, 200), timing(10_000, 400), timing(30_000, 800)];
/// A link that has sent nothing this long is pinged.
const PING_EVERY: Duration = Duration::from_secs(20);
/// Pings without an answer after which the link counts as lost.
const PINGS_UNANSWERED: u32 = 2;
/// How long a read waits, so the actor keeps draining the bus.
const READ_POLL: Duration = timing(100, 20);
/// Longest a write may block.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// The wait Pureplay's cooldown answer is taken to ask for when it names
/// none: its default, two minutes.
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(120);

/// Card telemetry keys.
const PAIRING: &str = "pairing";
const ERROR: &str = "error";
const NAME: &str = "name";
const CLUB: &str = "club";
const HANDED: &str = "handed";
const HOLE: &str = "hole";
const BLOCKERS: &str = "blockers";
const CLUB_OUT: &str = "club_out";
const SHOTS: &str = "shots";
const LAST_SHOT: &str = "last_shot";
const MISSED: &str = "missed_events";
const PUREPLAY: &str = "pureplay";
/// The keys that hold for one connection.
const PER_LINK: [&str; 9] = [
    NAME, CLUB, HANDED, HOLE, BLOCKERS, CLUB_OUT, SHOTS, MISSED, PUREPLAY,
];

/// The card's identity in its `DeviceTelemetry`.
const MANUFACTURER: &str = "Pureplay";
const MODEL: &str = "Accessory API";

/// The `pairing` row's texts.
const TOO_MANY: &str = "Pureplay has too many apps connected";
const WAITING: &str = "Waiting for Pureplay";
const ANOTHER_APP: &str = "Another app on this computer is waiting for approval in Pureplay";
const ASK_OFF: &str = "Turn on Ask about new apps in Pureplay's Settings > Accessories";
const DENIED: &str = "Pureplay turned the request down";
const TIMED_OUT: &str = "The request timed out in Pureplay";
const REMOVED: &str = "Removed in Pureplay. Press Reconnect to pair again.";
const PRESS_RECONNECT: &str = "Press Reconnect to pair again.";

/// The `error` row's texts.
const NOT_ACCESSORY: &str = "Not a Pureplay Accessory server";
const NO_EVENTS: &str = "Pureplay offers none of the events flighthook follows";

/// The `pairing` row while Pureplay's prompt waits, `secs` left.
fn approve_text(secs: u64) -> String {
    format!("Approve flighthook in Pureplay ({secs} s)")
}

/// The `pairing` row during Pureplay's cooldown, `secs` left.
fn cooldown_text(secs: u64) -> String {
    format!("Pureplay asks to wait ({}:{:02})", secs / 60, secs % 60)
}

/// A shot the card took, for the `last_shot` row: its number and the
/// local time.
fn last_shot_text(number: u32) -> String {
    #[cfg(debug_assertions)]
    if let Some(clock) = crate::screens::clock() {
        return format!("#{number} at {clock}");
    }
    format!("#{number} at {}", chrono::Local::now().format("%H:%M:%S"))
}

/// Whole seconds from `now` to `until`, rounded up.
fn secs_left(until: Instant, now: Instant) -> u64 {
    let left = until.saturating_duration_since(now);
    left.as_secs() + u64::from(left.subsec_nanos() > 0)
}

/// Pureplay Accessory card actor (`pureplay_accessory.N`).
pub struct PureplayAccessoryActor {
    pub addr: SocketAddr,
}

impl Actor for PureplayAccessoryActor {
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let addr = self.addr;
        std::thread::Builder::new()
            .name(format!("pureplay_accessory:{}", sender.actor_id()))
            .spawn(move || {
                let tokens = Arc::clone(&state.pureplay_tokens);
                run(addr, &state, &tokens, &sender, receiver);
            })
            .expect("failed to spawn pureplay_accessory thread")
    }

    /// Pureplay is reached as a client: a connect still running in the old
    /// instance keeps nothing from the new one.
    fn replaceable_while_stopping(&self) -> bool {
        true
    }

    fn source(&self) -> Option<ShotSource> {
        Some(ShotSource::PureplayAccessory)
    }

    /// The Shots setting and the impact lead are read where they are used;
    /// only an address change restarts the card.
    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let Some((_, index)) = sender.actor_id().split_once('.') else {
            return ReconfigureOutcome::Applied;
        };
        let snap = state.system.snapshot();
        let Some(section) = snap.pureplay_accessory.get(index) else {
            return ReconfigureOutcome::RestartRequired;
        };
        let addr = section.address.as_deref().unwrap_or(DEFAULT_ADDRESS);
        if addr.parse::<SocketAddr>().ok() == Some(self.addr) {
            ReconfigureOutcome::Applied
        } else {
            ReconfigureOutcome::RestartRequired
        }
    }
}

/// The token file key of the card at `addr`.
#[must_use]
pub fn token_key(addr: SocketAddr) -> String {
    flighthook::address_key(&addr.to_string())
}

/// How a connection ended, and what the card does next.
#[derive(Debug, Clone, PartialEq)]
enum End {
    /// The actor was stopped.
    Shutdown,
    /// Refused or lost: Reconnecting, try again after the backoff.
    Lost,
    /// Try again after the backoff, keeping the rows set.
    Backoff,
    /// Try again after `delay` with `status`, keeping the rows set.
    RetryAfter {
        delay: Duration,
        status: ActorStatus,
    },
    /// Connect again at once.
    Again,
    /// Disconnected until the user's Reconnect.
    Stopped,
    /// Disconnected through Pureplay's cooldown, counted down in the
    /// `pairing` row; then one more attempt when `retry`.
    Cooldown { until: Instant, retry: bool },
}

/// Exponential reconnect backoff.
#[derive(Debug)]
struct Backoff {
    next: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self { next: MIN_BACKOFF }
    }

    fn delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (self.next * 2).min(MAX_BACKOFF);
        delay
    }

    fn reset(&mut self) {
        self.next = MIN_BACKOFF;
    }
}

/// The card's telemetry: the rows it should show and the ones it showed.
#[derive(Debug, Default)]
struct Rows {
    want: BTreeMap<&'static str, String>,
    shown: BTreeMap<String, String>,
}

impl Rows {
    fn set(&mut self, key: &'static str, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            self.want.remove(key);
        } else {
            self.want.insert(key, value);
        }
    }

    fn clear(&mut self, key: &'static str) {
        self.want.remove(key);
    }

    /// The update to send: every row (and an empty value for each one shown
    /// that is gone) when `all`, else only what changed.
    fn take(&mut self, all: bool) -> HashMap<String, String> {
        let mut update = HashMap::new();
        for (k, v) in &self.want {
            if all || self.shown.get(*k) != Some(v) {
                update.insert((*k).to_owned(), v.clone());
            }
        }
        for k in self.shown.keys() {
            if !self.want.contains_key(k.as_str()) {
                update.insert(k.clone(), String::new());
            }
        }
        self.shown = self
            .want
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        update
    }
}

/// The `state.get` requests of one connection: one unanswered at a time, at
/// least [`STATE_GET_GAP`] apart, and the players they ask for.
///
/// Only a real snapshot answering a request marks the players that request
/// asked for and the snapshot did not bring; they are not asked for again
/// until [`StateGets::clear_not_brought`]. A request that times out or is
/// refused marks nobody: it is sent again after a growing wait
/// ([`STATE_GET_RETRY`]), and its players with it. A player named while a
/// request is out is asked for by the next request.
#[derive(Debug, Default)]
struct StateGets {
    /// Requests sent, for their ids.
    sent: u32,
    /// The unanswered request: its id, when it went out and the players it
    /// asks for.
    pending: Option<(String, Instant, BTreeSet<i64>)>,
    /// When the last request went out.
    last: Option<Instant>,
    /// A fresh snapshot is wanted.
    wanted: bool,
    /// Players named while the card does not hold them, for the next
    /// request.
    waiting: BTreeSet<i64>,
    /// Players a real snapshot asked for did not bring.
    not_brought: BTreeSet<i64>,
    /// Requests in a row that got no snapshot.
    failures: u32,
    /// No request before this, after a failed one.
    not_before: Option<Instant>,
}

impl StateGets {
    /// Ask for a fresh snapshot. While one is asked for already, its answer
    /// is fresh enough: it is made after everything that came before it.
    fn want(&mut self) {
        if self.pending.is_none() {
            self.wanted = true;
        }
    }

    /// Ask for `player`, named by an event while the card does not hold
    /// them, unless a real snapshot already left them out.
    fn want_player(&mut self, player: i64) {
        if self.not_brought.contains(&player) {
            return;
        }
        self.waiting.insert(player);
        self.wanted = true;
    }

    /// A new match, a context change or a join: every player may be asked
    /// for again.
    fn clear_not_brought(&mut self) {
        self.not_brought.clear();
    }

    /// Whether `request_id` names the request that is out.
    fn is_pending(&self, request_id: Option<&str>) -> bool {
        matches!((&self.pending, request_id), (Some((id, ..)), Some(r)) if r == id)
    }

    /// A snapshot came, answering `request_id`; `holds` says whether it
    /// brought a player. When it answers the request that is out, that
    /// request's players it did not bring are marked. When nobody is left
    /// waiting, nothing more is asked for: the snapshot is as fresh as the
    /// next one would be.
    fn answered(&mut self, request_id: Option<&str>, holds: impl Fn(i64) -> bool) {
        if let Some((id, _, _)) = &self.pending
            && request_id.is_none_or(|r| r == id)
            && let Some((_, _, asked)) = self.pending.take()
        {
            self.not_brought
                .extend(asked.into_iter().filter(|p| !holds(*p)));
            self.failures = 0;
            self.not_before = None;
        }
        let not_brought = &self.not_brought;
        self.waiting
            .retain(|p| !holds(*p) && !not_brought.contains(p));
        if self.waiting.is_empty() {
            self.wanted = false;
            self.failures = 0;
            self.not_before = None;
        } else {
            self.wanted = true;
        }
    }

    /// The request that is out was refused (an error response to its id):
    /// it is sent again after the next wait, or `retry_after` when that is
    /// longer.
    fn refused(&mut self, now: Instant, retry_after: Option<Duration>) {
        self.failed(now, retry_after);
    }

    /// The request that is out ends with no snapshot: its players wait for
    /// the next request, which goes out after the next [`STATE_GET_RETRY`]
    /// wait.
    fn failed(&mut self, now: Instant, retry_after: Option<Duration>) {
        let Some((_, _, asked)) = self.pending.take() else {
            return;
        };
        self.waiting.extend(asked);
        self.failures += 1;
        let step = usize::try_from(self.failures - 1).unwrap_or(usize::MAX);
        let wait = STATE_GET_RETRY[step.min(STATE_GET_RETRY.len() - 1)];
        self.not_before = Some(now + retry_after.map_or(wait, |r| r.max(wait)));
        self.wanted = true;
    }

    /// The id of the `state.get` to send now, if one is due. A request
    /// unanswered for [`SNAPSHOT_TIMEOUT`] is given up (`Err` with its id)
    /// and asked again later.
    fn due(&mut self, now: Instant) -> Result<Option<String>, String> {
        if let Some((id, at, _)) = &self.pending {
            if now.duration_since(*at) < SNAPSHOT_TIMEOUT {
                return Ok(None);
            }
            let id = id.clone();
            self.failed(now, None);
            return Err(id);
        }
        if !self.wanted
            || self
                .last
                .is_some_and(|l| now.duration_since(l) < STATE_GET_GAP)
            || self.not_before.is_some_and(|t| now < t)
        {
            return Ok(None);
        }
        self.wanted = false;
        self.sent += 1;
        let id = format!("s-{}", self.sent);
        self.pending = Some((id.clone(), now, std::mem::take(&mut self.waiting)));
        self.last = Some(now);
        Ok(Some(id))
    }
}

/// The card's state across connections.
struct Card<'a> {
    actor: String,
    /// The card's FRP actor id.
    frp_id: Option<String>,
    /// Republishes the card's `bus.actor` in answer to `bus.sync`.
    bus: BusSender,
    /// The selects the card sent.
    selects: std::sync::atomic::AtomicU64,
    addr: SocketAddr,
    /// Where Pureplay is, as the current connection sees it: what an echo
    /// is matched with.
    here: SimAddr,
    key: String,
    state: &'a SystemState,
    tokens: &'a TokenStore,
    /// A pair without a token may still be sent by itself.
    may_pair: bool,
    /// The one attempt after a cooldown is still to come.
    cooldown_retry: bool,
    /// `bad_request` answers to `pair` in a row.
    bad_requests: u32,
    /// Every token seen, replaced in every frame logged or put on the bus.
    secrets: Vec<Token>,
    /// The token Pureplay gave this actor start, used while the token file
    /// could not keep it.
    held: Option<Token>,
    /// Why the token file could not keep the token, shown as `error`.
    save_error: Option<String>,
    model: Model,
    /// Clubs picked in flighthook, on their way to Pureplay.
    club_out: ClubOut,
    /// The latest pick the bus carried since the last pass, while
    /// following the round.
    picked: Option<Club>,
    /// The card's `game.*` and `golfer.profile` as last published.
    published: game::Published,
    /// The round is being followed: picks are taken.
    following: bool,
    server_instance: Option<String>,
    game_version: Option<String>,
    /// The commands `hello.capabilities` lists.
    commands: Vec<String>,
    /// The scopes the pairing grants.
    scopes: Vec<String>,
    missed: u32,
    /// Pureplay's shot ids against flighthook's keys of the shots they
    /// stand for; kept across a reconnect to the same Pureplay.
    shot_map: ShotMap,
    /// What Pureplay's recent shots flew.
    flown: Flown,
    /// The shots Pureplay discarded: no stroke goes out for them.
    discarded: shots::Discarded,
    /// The shots the card took this actor start.
    shots_taken: u32,
    /// Where the last candidate shot came from in Auto: `true` for
    /// Pureplay, `false` for flighthook (an echo).
    last_from_pureplay: Option<bool>,
    /// The `blockers` the card's `bus.actor` said last on this
    /// connection.
    blockers_sent: Option<Vec<String>>,
    rows: Rows,
    status: Option<ActorStatus>,
    backoff: Backoff,
    /// When the connection became Connected.
    connected_since: Option<Instant>,
    /// When a frame last arrived.
    heard: Instant,
    /// A quiet link is pinged this often.
    ping_every: Duration,
}

/// The main loop: connects until the actor is stopped.
pub(crate) fn run(
    addr: SocketAddr,
    state: &SystemState,
    tokens: &TokenStore,
    sender: &BusSender,
    receiver: BusReceiver,
) {
    run_with(addr, state, tokens, sender, receiver, PING_EVERY);
}

/// [`run`], pinging a quiet link every `ping_every`.
fn run_with(
    addr: SocketAddr,
    state: &SystemState,
    tokens: &TokenStore,
    sender: &BusSender,
    mut receiver: BusReceiver,
    ping_every: Duration,
) {
    let mut card = Card {
        actor: sender.actor_id().to_owned(),
        frp_id: sender.frp_id().map(str::to_owned),
        bus: sender.clone(),
        selects: std::sync::atomic::AtomicU64::new(0),
        addr,
        here: SimAddr::of(None, addr),
        key: token_key(addr),
        state,
        tokens,
        may_pair: true,
        cooldown_retry: true,
        bad_requests: 0,
        secrets: Vec::new(),
        held: None,
        save_error: None,
        model: Model::default(),
        club_out: ClubOut::new(sender.actor_id()),
        picked: None,
        published: game::Published::default(),
        following: false,
        server_instance: None,
        game_version: None,
        commands: Vec::new(),
        scopes: Vec::new(),
        missed: 0,
        shot_map: ShotMap::default(),
        flown: Flown::default(),
        discarded: shots::Discarded::default(),
        shots_taken: 0,
        last_from_pureplay: None,
        blockers_sent: None,
        rows: Rows::default(),
        status: None,
        backoff: Backoff::new(),
        connected_since: None,
        heard: Instant::now(),
        ping_every,
    };
    // The turn the session holds now: a turn is published only when it
    // changes, so the card would never see it otherwise.
    card.model
        .flighthook_holds(&state.session.turn(), state.session.cause().as_deref());
    card.publish(sender, ActorStatus::Starting);
    loop {
        if card.token().is_none() && !card.may_pair {
            // Nothing to pair with until the user's Reconnect.
            card.publish(sender, ActorStatus::Disconnected);
            card.idle(&mut receiver, sender, None);
            return;
        }
        let end = card.connect(sender, &mut receiver);
        card.end_ready(sender);
        card.clear_link(sender);
        if card
            .connected_since
            .take()
            .is_some_and(|t| t.elapsed() >= STABLE)
        {
            card.backoff.reset();
        }
        let wait = match end {
            End::Shutdown => return,
            End::Lost => {
                card.rows.clear(PAIRING);
                card.unreachable();
                card.publish(sender, ActorStatus::Reconnecting);
                card.backoff.delay()
            }
            End::Backoff => {
                card.unreachable();
                card.publish(sender, ActorStatus::Reconnecting);
                card.backoff.delay()
            }
            End::RetryAfter { delay, status } => {
                card.publish(sender, status);
                delay
            }
            End::Again => {
                card.publish(sender, ActorStatus::Starting);
                Duration::ZERO
            }
            End::Stopped => {
                card.publish(sender, ActorStatus::Disconnected);
                card.idle(&mut receiver, sender, None);
                return;
            }
            End::Cooldown { until, retry } => {
                card.rows
                    .set(PAIRING, cooldown_text(secs_left(until, Instant::now())));
                card.publish(sender, ActorStatus::Disconnected);
                if !card.idle(&mut receiver, sender, Some(until)) {
                    return;
                }
                if !retry {
                    card.rows.set(PAIRING, PRESS_RECONNECT);
                    card.publish(sender, ActorStatus::Disconnected);
                    card.idle(&mut receiver, sender, None);
                    return;
                }
                Duration::ZERO
            }
        };
        tracing::info!("{}: next attempt in {wait:?}", card.actor);
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if !card.drain(&mut receiver) {
                return;
            }
            std::thread::sleep(
                until
                    .saturating_duration_since(Instant::now())
                    .min(READ_POLL),
            );
        }
    }
}

/// What a read gave. One per frame, taken apart at once: not worth a box.
#[allow(clippy::large_enum_variant)]
enum Read {
    /// Nothing within the poll.
    Nothing,
    /// A message, with its frame's text, the tokens replaced.
    Msg(Incoming, String),
    /// The connection ended: the close code, `None` without a close frame.
    Closed(Option<u16>),
}

/// The card's TCP stream. A read that times out reads as `WouldBlock` on
/// every platform (Windows reports `TimedOut`), so the WebSocket handshake
/// can be resumed after each poll.
struct Polled(TcpStream);

impl std::io::Read for Polled {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::TimedOut {
                std::io::ErrorKind::WouldBlock.into()
            } else {
                e
            }
        })
    }
}

impl std::io::Write for Polled {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// One WebSocket connection.
struct Link {
    ws: WebSocket<Polled>,
    actor: String,
}

impl Link {
    fn send(&mut self, text: &str, secrets: &[Token]) -> Result<(), ()> {
        tracing::info!(
            target: "audit",
            "{} sent {} {}",
            self.actor,
            if SEND_BINARY { "binary" } else { "text" },
            token::redact(text, secrets),
        );
        let msg = if SEND_BINARY {
            Message::binary(text.as_bytes().to_vec())
        } else {
            Message::text(text.to_owned())
        };
        self.ws.send(msg).map_err(|e| {
            tracing::info!("{}: send failed: {e}", self.actor);
        })
    }

    /// Send a WebSocket ping.
    fn ping(&mut self) -> Result<(), ()> {
        tracing::debug!(target: "audit", "{} sent ping", self.actor);
        self.ws
            .send(Message::Ping(tungstenite::Bytes::new()))
            .map_err(|e| {
                tracing::info!("{}: ping failed: {e}", self.actor);
            })
    }

    /// Close the connection from this side.
    fn close(&mut self) {
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
    }
}

impl Card<'_> {
    /// Send the card's status: every row on a change of status, else what
    /// changed (nothing when nothing did).
    fn publish(&mut self, sender: &BusSender, status: ActorStatus) {
        let changed = self.status != Some(status);
        let telemetry = self.rows.take(changed);
        if changed || !telemetry.is_empty() {
            self.status = Some(status);
            sender.local(LocalEvent::CardStatus { status, telemetry });
        }
    }

    /// Remove what held for the connection that ended, a club pick
    /// included, and the card's game state from the bus.
    fn clear_link(&mut self, sender: &BusSender) {
        for body in self.published.clear() {
            sender.frp(body);
        }
        for key in PER_LINK {
            self.rows.clear(key);
        }
        self.model.forget_held();
        self.club_out.clear();
        self.model.hold_club(false);
        self.following = false;
        self.picked = None;
    }

    /// Pureplay is unreachable: an `error` it caused goes; a token file
    /// that could not be written stays.
    fn unreachable(&mut self) {
        self.rows
            .set(ERROR, self.save_error.clone().unwrap_or_default());
    }

    /// Drain the bus: flighthook's turn (and its cause) is what flighthook
    /// holds from then on; while the round is followed, a `golfer.select`
    /// carrying a club from outside the game and simulator cards is a pick,
    /// the latest kept, and awaits its `bus.reply` like the card's own
    /// selects. Falling behind on the bus forgets the unanswered selects and
    /// holds the current turn. `bus.sync` gets the card's `bus.actor` and
    /// game state again; `game.aim` is answered `unsupported`. Returns false
    /// once the actor is stopped.
    fn drain(&mut self, receiver: &mut BusReceiver) -> bool {
        let session = self.state.flighthook_id();
        loop {
            let next = receiver.poll();
            // Fallen behind: a reply may be among the dropped messages, so
            // nothing waits for one and the current turn is compared now.
            if receiver.take_lagged() {
                self.model.lagged(
                    &self.state.session.turn(),
                    self.state.session.cause().as_deref(),
                );
            }
            match next {
                Ok(Some(msg)) => {
                    if let Some((turn, cause)) = msg.turn_of(&session) {
                        self.model.flighthook_holds(turn, cause);
                    }
                    if let Some(flightrelay::FrpMessage {
                        from,
                        body: FrpBody::BusReply(reply),
                    }) = msg.as_frp()
                        && *from == session
                        && reply.command == "golfer.select"
                    {
                        self.model.select_answered(&reply.to, &reply.r#ref);
                    }
                    if msg
                        .as_frp()
                        .is_some_and(|m| matches!(m.body, FrpBody::BusSync(_)))
                    {
                        let blockers = self
                            .blockers_sent
                            .clone()
                            .unwrap_or_else(|| vec![blockers::NOT_ARMED.into()]);
                        self.bus.frp(self.bus_actor(blockers));
                        for body in self.published.all() {
                            self.bus.frp(body);
                        }
                    }
                    if let Some(flightrelay::FrpMessage {
                        from,
                        body: FrpBody::GameAim(aim),
                    }) = msg.as_frp()
                        && Some(from.as_str()) != self.frp_id.as_deref()
                    {
                        tracing::info!("{}: game.aim from {from}, unsupported", self.actor);
                        self.bus.frp(BusReply::refused(
                            from.clone(),
                            "game.aim",
                            aim.r#ref.clone(),
                            ReplyError::Unsupported,
                        ));
                    }
                    if let Some(flightrelay::FrpMessage {
                        from,
                        body: FrpBody::GolferSelect(select),
                    }) = msg.as_frp()
                        && let Some(Some(club)) = select.club
                        && self.following
                        && Some(from.as_str()) != self.frp_id.as_deref()
                        && !self.state.is_game_card(from)
                    {
                        self.model.select_sent(from, &select.r#ref);
                        self.picked = Some(club);
                    }
                }
                Ok(None) => return !receiver.is_shutdown(),
                Err(PollError::Shutdown) => return false,
            }
        }
    }

    /// Wait, draining the bus, until `until` (for ever when `None`),
    /// counting a cooldown down in the `pairing` row. Returns false once
    /// the actor is stopped.
    fn idle(
        &mut self,
        receiver: &mut BusReceiver,
        sender: &BusSender,
        until: Option<Instant>,
    ) -> bool {
        loop {
            if !self.drain(receiver) {
                return false;
            }
            let now = Instant::now();
            if let Some(until) = until {
                if now >= until {
                    return true;
                }
                self.rows.set(PAIRING, cooldown_text(secs_left(until, now)));
                self.publish(sender, ActorStatus::Disconnected);
            }
            std::thread::sleep(READ_POLL);
        }
    }

    /// Keep `token` out of every frame logged or put on the bus.
    fn learn(&mut self, token: &str) {
        if !token.is_empty() && !self.secrets.iter().any(|t| t.expose() == token) {
            self.secrets.push(Token::new(token));
        }
    }

    /// `text` with every token the card knows replaced.
    fn redact(&self, text: &str) -> String {
        token::redact(text, &self.secrets)
    }

    /// Read what has arrived, waiting up to [`READ_POLL`].
    fn read(&mut self, link: &mut Link) -> Read {
        let msg = match link.ws.read() {
            Ok(msg) => {
                self.heard = Instant::now();
                msg
            }
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Read::Nothing;
            }
            Err(e) => {
                tracing::info!("{}: connection lost: {e}", self.actor);
                return Read::Closed(None);
            }
        };
        let (text, kind) = match msg {
            Message::Text(t) => (t.as_str().to_owned(), "text"),
            Message::Binary(b) => match String::from_utf8(b.to_vec()) {
                Ok(t) => (t, "binary"),
                Err(_) => {
                    tracing::warn!(
                        target: "audit",
                        "{} received binary, not UTF-8: {}",
                        self.actor,
                        flighthook::hex_lower(&b),
                    );
                    return Read::Nothing;
                }
            },
            Message::Close(frame) => {
                let code = frame.as_ref().map(|f| u16::from(f.code));
                tracing::info!(
                    target: "audit",
                    "{} received close {} {}",
                    self.actor,
                    code.map_or_else(|| "(no code)".to_owned(), |c| c.to_string()),
                    self.redact(frame.as_ref().map_or("", |f| f.reason.as_str())),
                );
                let _ = link.ws.flush();
                return Read::Closed(code);
            }
            _ => return Read::Nothing,
        };
        // A `paired` frame carries a token the card has not seen: learn it
        // before anything logs the frame, however the rest of it reads.
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            tracing::warn!(
                target: "audit",
                "{} received {kind}, {} bytes, not JSON (not shown)",
                self.actor,
                text.len(),
            );
            return Read::Nothing;
        };
        if value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("paired"))
            && let Some(t) = value.pointer("/data/token").and_then(Value::as_str)
        {
            self.learn(t);
        }
        let shown = self.redact(&text);
        tracing::info!(target: "audit", "{} received {kind} {shown}", self.actor);
        match protocol::parse(&text) {
            Ok(msg) => Read::Msg(msg, shown),
            Err(e) => {
                tracing::warn!(
                    "{}: unreadable message ({}), ignored",
                    self.actor,
                    self.redact(&e.to_string())
                );
                Read::Nothing
            }
        }
    }

    /// Connect and run one connection until it ends.
    fn connect(&mut self, sender: &BusSender, receiver: &mut BusReceiver) -> End {
        let stream = match super::connect_tcp(&self.actor, self.addr, CONNECT_TIMEOUT, receiver) {
            Ok(Some(s)) => s,
            Ok(None) => return End::Shutdown,
            Err(e) => {
                tracing::info!("{}: no connection to {}: {e}", self.actor, self.addr);
                return End::Lost;
            }
        };
        let _ = stream.set_nodelay(true);
        self.here = SimAddr::of(
            stream.local_addr().ok(),
            stream.peer_addr().unwrap_or(self.addr),
        );
        if stream.set_read_timeout(Some(READ_POLL)).is_err()
            || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
        {
            return End::Lost;
        }
        let mut request = match format!("ws://{}/", self.addr).into_client_request() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("{}: {e}", self.actor);
                return End::Lost;
            }
        };
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            tungstenite::http::HeaderValue::from_static("binary"),
        );
        // Each read waits one poll, so a stop is seen while Pureplay is slow
        // to answer.
        let deadline = Instant::now() + WS_HANDSHAKE_TIMEOUT;
        let mut attempt = tungstenite::client(request, Polled(stream));
        let ws = loop {
            match attempt {
                Ok((ws, _)) => break ws,
                Err(HandshakeError::Failure(e)) => {
                    tracing::info!("{}: WebSocket handshake failed: {e}", self.actor);
                    return End::Lost;
                }
                Err(HandshakeError::Interrupted(mid)) => {
                    if !self.drain(receiver) {
                        return End::Shutdown;
                    }
                    if Instant::now() >= deadline {
                        tracing::info!("{}: no WebSocket handshake reply", self.actor);
                        return End::Lost;
                    }
                    attempt = mid.handshake();
                }
            }
        };
        let mut link = Link {
            ws,
            actor: self.actor.clone(),
        };
        tracing::info!("{}: connected to {}", self.actor, self.addr);
        self.heard = Instant::now();
        let end = self.session(&mut link, sender, receiver);
        if end == End::Shutdown {
            link.close();
        }
        end
    }

    /// One connection: `hello`, pairing, subscription, then the round.
    fn session(&mut self, link: &mut Link, sender: &BusSender, receiver: &mut BusReceiver) -> End {
        let hello = match self.hello(link, receiver) {
            Ok(h) => h,
            Err(end) => return end,
        };
        let saved = self.token();
        let paired = match self.pair(link, sender, receiver, saved.as_ref()) {
            Ok(p) => p,
            Err(end) => return end,
        };
        self.keep_pairing(&paired, saved.as_ref());
        let last_seq = match self.subscribe(link, sender, receiver, &hello) {
            Ok(seq) => seq,
            Err(end) => return end,
        };
        self.follow(link, sender, receiver, last_seq)
    }

    /// Wait for `hello` and check it.
    fn hello(&mut self, link: &mut Link, receiver: &mut BusReceiver) -> Result<Hello, End> {
        let started = Instant::now();
        let hello = loop {
            if !self.drain(receiver) {
                return Err(End::Shutdown);
            }
            match self.read(link) {
                Read::Msg(Incoming::Hello(h), _) => break h,
                Read::Msg(..) => {}
                Read::Closed(Some(4429)) => {
                    self.rows.set(PAIRING, TOO_MANY);
                    return Err(End::Backoff);
                }
                Read::Closed(_) => return Err(End::Lost),
                Read::Nothing => {
                    if started.elapsed() >= HELLO_TIMEOUT {
                        tracing::info!("{}: no hello from {}", self.actor, self.addr);
                        return Err(End::Lost);
                    }
                }
            }
        };
        let retry = End::RetryAfter {
            delay: VERSION_RETRY,
            status: ActorStatus::Reconnecting,
        };
        if hello.protocol.as_deref() != Some(protocol::PROTOCOL) || hello.version.is_none() {
            self.rows.set(ERROR, NOT_ACCESSORY);
            link.close();
            return Err(retry);
        }
        if let Some(version) = hello.version.filter(|v| *v != protocol::VERSION) {
            self.rows.set(
                ERROR,
                format!(
                    "Pureplay speaks Accessory version {version}; this flighthook speaks {}",
                    protocol::VERSION
                ),
            );
            link.close();
            return Err(retry);
        }
        self.rows
            .set(ERROR, self.save_error.clone().unwrap_or_default());
        if let Some(instance) = &hello.server_instance_id {
            if self.server_instance.as_ref().is_some_and(|s| s != instance) {
                tracing::info!("{}: Pureplay restarted, cached state dropped", self.actor);
                self.model.reset();
                self.club_out.clear();
                self.shot_map.clear();
                self.flown.clear();
                self.discarded.clear();
            }
            self.server_instance = Some(instance.clone());
        }
        self.game_version.clone_from(&hello.game_version);
        self.commands = hello
            .capabilities
            .as_ref()
            .map(|c| c.commands.clone())
            .unwrap_or_default();
        tracing::info!(
            "{}: Pureplay {} offers commands {}",
            self.actor,
            self.game_version.as_deref().unwrap_or("(no version)"),
            self.commands.join(", ")
        );
        Ok(hello)
    }

    /// Pair, with the saved token or by asking the player.
    fn pair(
        &mut self,
        link: &mut Link,
        sender: &BusSender,
        receiver: &mut BusReceiver,
        saved: Option<&Token>,
    ) -> Result<Paired, End> {
        if let Some(t) = saved {
            self.learn(t.expose());
        } else {
            self.rows.set(PAIRING, WAITING);
            self.publish(sender, ActorStatus::Starting);
        }
        if link
            .send(&protocol::pair(saved.map(Token::expose)), &self.secrets)
            .is_err()
        {
            return Err(End::Lost);
        }
        let had_token = saved.is_some();
        // Only a pair without a token waits in Pureplay's queue for as long
        // as it takes; a refusal waits a moment for its close frame.
        let mut deadline = had_token.then(|| Instant::now() + PAIR_TIMEOUT);
        let mut refusal: Option<ResponseError> = None;
        let mut prompt_until: Option<Instant> = None;
        loop {
            if !self.drain(receiver) {
                return Err(End::Shutdown);
            }
            match self.read(link) {
                Read::Msg(Incoming::Paired(p), _) => return Ok(p),
                Read::Msg(Incoming::PairingPending { expires_in_sec }, _) => {
                    let secs = expires_in_sec.unwrap_or(60);
                    prompt_until = Some(Instant::now() + Duration::from_secs(secs));
                    self.rows.set(PAIRING, approve_text(secs));
                    self.publish(sender, ActorStatus::Starting);
                }
                Read::Msg(Incoming::Response(r), _) if !r.ok => {
                    let error = r.error.unwrap_or_default();
                    let message = self.redact(error.message.as_deref().unwrap_or(""));
                    tracing::info!(
                        "{}: pair refused: {} {message}",
                        self.actor,
                        self.redact(&error.code),
                    );
                    if error.code == "bad_request" && !had_token {
                        link.close();
                        self.bad_requests += 1;
                        if self.bad_requests >= BAD_REQUEST_TRIES {
                            let text = if message.is_empty() {
                                ANOTHER_APP.to_owned()
                            } else {
                                message
                            };
                            self.rows.set(PAIRING, text);
                            return Err(End::Stopped);
                        }
                        self.rows.set(PAIRING, ANOTHER_APP);
                        return Err(End::RetryAfter {
                            delay: BAD_REQUEST_RETRY,
                            status: ActorStatus::Reconnecting,
                        });
                    }
                    refusal = Some(error);
                    deadline = Some(Instant::now() + CLOSE_GRACE);
                }
                Read::Msg(..) => {}
                Read::Closed(code) => return Err(self.refused(code, refusal.as_ref(), had_token)),
                Read::Nothing => {
                    if let Some(until) = prompt_until {
                        let secs = secs_left(until, Instant::now());
                        self.rows.set(PAIRING, approve_text(secs));
                        self.publish(sender, ActorStatus::Starting);
                    }
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        link.close();
                        if refusal.is_none() {
                            tracing::info!("{}: no answer to pair", self.actor);
                        }
                        return Err(self.refused(None, refusal.as_ref(), had_token));
                    }
                }
            }
        }
    }

    /// Keep what pairing gave: the token (saved when new) and the scopes.
    fn keep_pairing(&mut self, paired: &Paired, saved: Option<&Token>) {
        if saved.is_none() {
            // The one pair without a token this start was used.
            self.may_pair = false;
        }
        self.bad_requests = 0;
        self.rows.clear(PAIRING);
        if let Some(t) = paired.token.as_deref().filter(|t| !t.is_empty()) {
            self.learn(t);
            let t = Token::new(t);
            self.held = Some(t.clone());
            if saved != Some(&t) {
                let client_id = paired.client_id.as_deref().unwrap_or_default();
                match self.tokens.save(&self.key, &t, client_id) {
                    Ok(()) => {
                        self.save_error = None;
                        self.rows.clear(ERROR);
                        tracing::info!(
                            "{}: paired, saved in {}",
                            self.actor,
                            self.tokens.path().display()
                        );
                    }
                    Err(e) => {
                        tracing::warn!("{}: couldn't save the pairing: {e}", self.actor);
                        let text = format!("Couldn't save the Pureplay pairing: {e}");
                        self.rows.set(ERROR, text.clone());
                        self.save_error = Some(text);
                    }
                }
            }
        }
        self.scopes.clone_from(&paired.scopes);
        tracing::info!(
            "{}: paired with scopes {}",
            self.actor,
            self.redact(&self.scopes.join(", "))
        );
    }

    /// Subscribe and take the snapshot. Returns the snapshot's `seq`.
    fn subscribe(
        &mut self,
        link: &mut Link,
        sender: &BusSender,
        receiver: &mut BusReceiver,
        hello: &Hello,
    ) -> Result<Option<u64>, End> {
        let events = protocol::events_to_subscribe(hello.capabilities.as_ref());
        if events.is_empty() {
            // An empty list would subscribe to every event.
            tracing::warn!("{}: Pureplay lists none of the card's events", self.actor);
            self.rows.set(ERROR, NO_EVENTS);
            link.close();
            return Err(End::RetryAfter {
                delay: VERSION_RETRY,
                status: ActorStatus::Reconnecting,
            });
        }
        if link
            .send(&protocol::subscribe(&events), &self.secrets)
            .is_err()
        {
            return Err(End::Lost);
        }
        let asked = Instant::now();
        let mut removal = false;
        loop {
            if !self.drain(receiver) {
                return Err(End::Shutdown);
            }
            match self.read(link) {
                Read::Msg(Incoming::Response(r), _)
                    if r.id.as_deref() == Some(protocol::SUBSCRIBE_ID) && !r.ok =>
                {
                    tracing::warn!(
                        "{}: subscribe refused: {}",
                        self.actor,
                        self.redact(&format!("{:?}", r.error))
                    );
                    link.close();
                    return Err(End::Lost);
                }
                Read::Msg(Incoming::Response(r), _) if r.id.is_none() && !r.ok => {
                    removal |= self.unprompted(&r);
                }
                Read::Msg(Incoming::Event(e), raw) if e.name == "state.snapshot" => {
                    self.snapshot(&e, &raw, sender);
                    return Ok(e.seq);
                }
                Read::Msg(..) => {}
                Read::Closed(code) => return Err(self.lost(code, removal)),
                Read::Nothing => {
                    if asked.elapsed() >= SNAPSHOT_TIMEOUT {
                        tracing::info!("{}: no snapshot after subscribe", self.actor);
                        link.close();
                        return Err(End::Lost);
                    }
                }
            }
        }
    }

    /// An unprompted refused response (`id: null`): logged. Returns whether
    /// it tells of a removal in Pureplay (`unauthorised`).
    fn unprompted(&self, r: &protocol::Response) -> bool {
        let code = r.error.as_ref().map_or("", |e| e.code.as_str());
        tracing::info!("{}: Pureplay says {}", self.actor, self.redact(code));
        code == "unauthorised"
    }

    /// Follow the round until the connection ends.
    fn follow(
        &mut self,
        link: &mut Link,
        sender: &BusSender,
        receiver: &mut BusReceiver,
        mut last_seq: Option<u64>,
    ) -> End {
        self.connected_since = Some(Instant::now());
        self.missed = 0;
        self.club_out.link(&self.scopes, &self.commands);
        self.following = true;
        self.link_rows();
        self.publish(sender, ActorStatus::Connected);
        self.device_ready(sender);
        tracing::info!("{}: following the round", self.actor);

        // Per connection, so a reconnect asks for every player again.
        let mut gets = StateGets::default();
        let mut removal = false;
        let mut pings = 0u32;
        loop {
            if !self.drain(receiver) {
                return End::Shutdown;
            }
            if let Some(club) = self.picked.take() {
                let actions = self.club_out.pick(club, &self.model, Instant::now());
                self.club_actions(actions, link, sender, None);
            }
            // Compare the turn the card holds with flighthook's, after a
            // pick in the same drain is held.
            self.sync(sender);
            match self.read(link) {
                Read::Msg(Incoming::Event(e), raw) => {
                    if let (Some(last), Some(seq)) = (last_seq, e.seq) {
                        if seq > last + 1 {
                            self.missed += 1;
                            tracing::warn!(
                                "{}: events {}..{} missed, asking for the state",
                                self.actor,
                                last + 1,
                                seq - 1
                            );
                            gets.want();
                        } else if seq < last {
                            tracing::info!(
                                "{}: event seq {seq} after {last}, counting from it",
                                self.actor
                            );
                        }
                    }
                    last_seq = e.seq.or(last_seq);
                    if e.name == "state.snapshot" {
                        self.snapshot(&e, &raw, sender);
                        gets.answered(e.request_id.as_deref(), |id| {
                            self.model.player(id).is_some()
                        });
                        self.club_out.readiness_changed(self.model.readiness());
                        let actions = self.club_out.drive(&self.model, Instant::now());
                        self.club_actions(actions, link, sender, Some(&raw));
                    } else {
                        // What drops a pending pick is known before the
                        // model gives the club of a new turn.
                        let mut actions = self.club_out.before_event(&e);
                        self.model.hold_club(self.club_out.holds_club());
                        if let Err(err) = self.model.apply_event(&e) {
                            tracing::warn!(
                                "{}: {} not read: {}",
                                self.actor,
                                e.name,
                                self.redact(&err.to_string())
                            );
                        }
                        self.shot_event(&e, &raw, sender);
                        self.game_changes(sender);
                        if matches!(
                            e.name.as_str(),
                            "context.invalidated" | "match.started" | "player.joined"
                        ) {
                            gets.clear_not_brought();
                        }
                        if e.name == "context.invalidated" || e.name == "match.started" {
                            gets.want();
                        } else if let Some(id) = self.model.unknown_player(&e) {
                            gets.want_player(id);
                        }
                        actions.extend(self.club_out.drive(&self.model, Instant::now()));
                        self.club_actions(actions, link, sender, Some(&raw));
                    }
                    self.sync(sender);
                    self.link_rows();
                    self.publish(sender, ActorStatus::Connected);
                    self.device_ready(sender);
                }
                Read::Msg(Incoming::Response(r), _) if r.id.is_none() && !r.ok => {
                    removal |= self.unprompted(&r);
                }
                Read::Msg(Incoming::Response(r), _) if gets.is_pending(r.id.as_deref()) => {
                    if !r.ok {
                        let error = r.error.as_ref();
                        tracing::info!(
                            "{}: state.get refused: {}",
                            self.actor,
                            self.redact(error.map_or("", |e| e.code.as_str()))
                        );
                        let retry_after = error
                            .and_then(|e| e.retry_after_ms)
                            .map(Duration::from_millis);
                        gets.refused(Instant::now(), retry_after);
                    }
                }
                Read::Msg(Incoming::Response(r), raw) => {
                    if let Some(actions) = self.club_out.reply(&r, &mut self.model, Instant::now())
                    {
                        self.club_actions(actions, link, sender, Some(&raw));
                    }
                }
                Read::Msg(..) | Read::Nothing => {}
                Read::Closed(code) => return self.lost(code, removal),
            }
            let actions = self.club_out.drive(&self.model, Instant::now());
            self.club_actions(actions, link, sender, None);
            // Also on a pass with no event, so the `shots` row follows the
            // setting while Pureplay is quiet; nothing goes out when no row
            // changed.
            self.link_rows();
            self.publish(sender, ActorStatus::Connected);
            let now = Instant::now();
            match gets.due(now) {
                Ok(Some(id)) => {
                    let _ = link.send(&protocol::state_get(&id), &self.secrets);
                }
                Ok(None) => {}
                Err(id) => {
                    tracing::warn!(
                        "{}: no answer to state.get {id}, asking again later",
                        self.actor
                    );
                }
            }
            if PING {
                let quiet = now.saturating_duration_since(self.heard);
                if quiet < self.ping_every {
                    pings = 0;
                } else if quiet >= self.ping_every * (pings + 1) {
                    if pings >= PINGS_UNANSWERED {
                        tracing::info!(
                            "{}: nothing from Pureplay for {quiet:?}, {pings} pings unanswered",
                            self.actor
                        );
                        link.close();
                        return End::Lost;
                    }
                    if link.ping().is_err() {
                        return End::Lost;
                    }
                    pings += 1;
                }
            }
        }
    }

    /// Take a snapshot event: the model, the bus, the rows.
    fn snapshot(&mut self, e: &protocol::Event, _raw: &str, sender: &BusSender) {
        match protocol::payload::<protocol::Snapshot>(e) {
            Ok(snap) => {
                self.model.hold_club(self.club_out.holds_club());
                self.model.apply_snapshot(&snap);
                // The card's `bus.actor` goes out before the first profile
                // names a namespace it issues.
                if self.blockers_sent.is_none() {
                    let not_armed = vec![blockers::NOT_ARMED.to_owned()];
                    self.blockers_sent = Some(not_armed.clone());
                    sender.frp(self.bus_actor(not_armed));
                }
                self.game_changes(sender);
            }
            Err(err) => tracing::warn!(
                "{}: snapshot not read: {}",
                self.actor,
                self.redact(&err.to_string())
            ),
        }
    }

    /// Carry out what club out asks: send its commands. Once no pick holds
    /// the club, the card's select goes out when flighthook's club differs
    /// from Pureplay's (`raw`, the frame that ended the pick, is logged).
    fn club_actions(
        &mut self,
        actions: Vec<Action>,
        link: &mut Link,
        sender: &BusSender,
        raw: Option<&str>,
    ) {
        for action in actions {
            match action {
                // A failed send ends the connection, which drops the pick.
                Action::Send(text) => {
                    let _ = link.send(&text, &self.secrets);
                }
                Action::SnapBack => {
                    if let Some(raw) = raw {
                        tracing::debug!(target: "audit", "{} pick ended by {raw}", self.actor);
                    }
                    tracing::info!("{}: Pureplay's club stays", self.actor);
                }
            }
        }
        self.model.hold_club(self.club_out.holds_club());
        self.sync(sender);
    }

    /// Send the card's complete `golfer.select` when the model says so.
    fn sync(&mut self, sender: &BusSender) {
        let config = self.state.system.view();
        let key = |id: Option<&str>| config.read().golfer_key(id);
        let state = self.state;
        if let Some(want) = self.model.sync(key, |a| state.is_game_card(a)) {
            self.emit(&want, sender);
        }
    }

    /// Put `want` on the bus in one `golfer.select` from the card's actor,
    /// which then awaits its `bus.reply`.
    fn emit(&mut self, want: &Want, sender: &BusSender) {
        let select = want.select(format!(
            "{}-{}",
            self.actor,
            self.selects
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1
        ));
        tracing::info!("{}: {want:?} from Pureplay", self.actor);
        if let Some(from) = sender.frp_id() {
            self.model.select_sent(from, &select.r#ref);
        }
        sender.frp(select);
    }

    /// Publish what changed of the card's game state.
    fn game_changes(&mut self, sender: &BusSender) {
        for body in self.published.changes(&mut self.model) {
            sender.frp(body);
        }
    }

    /// The rows of a connection, from the model.
    fn link_rows(&mut self) {
        let player = self.model.active_local().cloned();
        self.rows.set(
            NAME,
            player
                .as_ref()
                .and_then(|p| p.name.clone())
                .unwrap_or_default(),
        );
        self.rows.set(
            CLUB,
            player
                .as_ref()
                .and_then(|p| p.club.as_deref())
                .and_then(protocol::club)
                .map(|c| c.to_string())
                .unwrap_or_default(),
        );
        self.rows.set(
            HANDED,
            player
                .as_ref()
                .and_then(|p| p.handedness.as_deref())
                .and_then(protocol::hand)
                .map(|h| h.to_string())
                .unwrap_or_default(),
        );
        self.rows
            .set(HOLE, self.model.hole_text().unwrap_or_default());
        self.rows.set(
            BLOCKERS,
            self.model
                .readiness()
                .map(protocol::blockers_text)
                .unwrap_or_default(),
        );
        self.rows
            .set(CLUB_OUT, self.club_out.state(&self.model).text());
        self.rows.set(SHOTS, self.shots_text());
        self.rows.set(
            MISSED,
            if self.missed > 0 {
                self.missed.to_string()
            } else {
                String::new()
            },
        );
        self.rows
            .set(PUREPLAY, self.game_version.clone().unwrap_or_default());
    }

    /// The card's Shots setting, read from the config each time.
    fn shots_setting(&self) -> PureplayShots {
        self.state.system.pureplay_shots(&self.actor)
    }

    /// The `shots` row: the setting, and in Auto where the last shot came
    /// from.
    fn shots_text(&self) -> String {
        match self.shots_setting() {
            PureplayShots::Auto => match self.last_from_pureplay {
                Some(true) => "auto: last from Pureplay".into(),
                Some(false) => "auto: last from flighthook".into(),
                None => "auto".into(),
            },
            PureplayShots::FromPureplay => "from Pureplay".into(),
            PureplayShots::FromFlighthook => "from flighthook".into(),
        }
    }

    /// Take a live shot event: a `shot.launched` taken or matched, a
    /// `shot.completed` sent as `game.stroke`, a discarded shot forgotten.
    fn shot_event(&mut self, e: &protocol::Event, raw: &str, sender: &BusSender) {
        let read = |r: Result<(), protocol::ParseError>| {
            if let Err(err) = r {
                tracing::warn!(
                    "{}: {} not read: {}",
                    self.actor,
                    e.name,
                    self.redact(&err.to_string())
                );
            }
        };
        match e.name.as_str() {
            "shot.launched" => match protocol::payload::<protocol::ShotLaunched>(e) {
                Ok(shot) => self.shot_launched(&shot, raw, sender),
                Err(err) => read(Err(err)),
            },
            "shot.completed" => match protocol::payload::<protocol::ShotCompleted>(e) {
                Ok(done) => self.shot_completed(&done, raw, sender),
                Err(err) => read(Err(err)),
            },
            "shot.cancelled" => match protocol::payload::<protocol::ShotCancelled>(e) {
                Ok(c) => {
                    if c.reason.as_deref() == Some("discarded")
                        && let Some(id) = c.shot_id.as_deref()
                    {
                        self.shot_discarded(id, raw, sender);
                    }
                }
                Err(err) => read(Err(err)),
            },
            _ => {}
        }
    }

    /// A `shot.cancelled` `discarded` (a scramble shot the team did not
    /// pick): its id is forgotten, so a result after the discard sends no
    /// `game.stroke`. A stroke sent before it is not retracted.
    fn shot_discarded(&mut self, id: &str, raw: &str, _sender: &BusSender) {
        // Whatever the card held of it: a shot it did not map (a remote
        // player's, one ignored under "From flighthook", one launched
        // before the card connected) is discarded too.
        self.discarded.insert(id);
        self.shot_map.remove(id);
        tracing::info!("{}: Pureplay discarded shot {id}", self.actor);
        tracing::debug!(target: "audit", "{} discard {raw}", self.actor);
    }

    /// A `shot.launched`: a local player's shot is taken, matched as an
    /// echo, or ignored, as the Shots setting says. A `shotId` the card
    /// already holds is not taken again.
    fn shot_launched(&mut self, shot: &protocol::ShotLaunched, raw: &str, sender: &BusSender) {
        let id = shot.shot_id.clone().unwrap_or_default();
        if !id.is_empty()
            && let Some(launch) = shots::launch(shot)
        {
            self.flown.insert(&id, launch);
        }
        let Some(player) = shot.player_id.filter(|p| self.model.is_local(*p)) else {
            tracing::info!(
                "{}: shot {id} of a player at another machine, not taken",
                self.actor
            );
            return;
        };
        if !id.is_empty()
            && let Some(key) = self.shot_map.get(&id)
        {
            tracing::info!(
                "{}: shot {id} again, already shot #{} ({}); not taken",
                self.actor,
                key.shot_number,
                key.shot_id
            );
            return;
        }
        let mode = self.shots_setting();
        let here = self.here;
        let sent = &self.state.sent_shots;
        let decision = shots::decide(mode, || sent.take_echo(here, Instant::now()));
        match decision {
            Decision::Echo(key) => {
                tracing::info!(
                    "{}: shot {id} echoes shot #{} ({}) flighthook sent; not taken",
                    self.actor,
                    key.shot_number,
                    key.shot_id
                );
                self.last_from_pureplay = Some(false);
                if !id.is_empty() {
                    self.shot_map.insert(&id, key);
                }
            }
            Decision::Ignore => {
                tracing::info!(
                    "{}: shot {id} of player {player} not taken (shots from flighthook), \
                     and no shot flighthook sent matches it",
                    self.actor
                );
            }
            Decision::Take => {
                let Some(launch) = shots::launch(shot) else {
                    tracing::warn!("{}: shot {id} has no ball speed, not taken", self.actor);
                    return;
                };
                self.shots_taken += 1;
                let key = ShotKey::new(uuid::Uuid::new_v4().to_string(), self.shots_taken);
                tracing::info!(
                    "{}: shot {id} of player {player} taken as shot #{}",
                    self.actor,
                    key.shot_number
                );
                tracing::debug!(target: "audit", "{} shot from {raw}", self.actor);
                sender.frp(ShotTrigger::new(key.clone()));
                sender.frp(ShotLaunch {
                    key: key.clone(),
                    launch,
                });
                sender.frp(ShotFinished { key: key.clone() });
                self.rows.set(LAST_SHOT, last_shot_text(key.shot_number));
                if mode == PureplayShots::Auto {
                    self.last_from_pureplay = Some(true);
                }
                if !id.is_empty() {
                    self.shot_map.insert(&id, key);
                }
            }
        }
    }

    /// A `shot.completed`: Pureplay's stroke as `game.stroke`, with the
    /// launch monitor's `shot_id` of the flighthook shot its `shotId`
    /// stands for (a taken shot or a matched echo). A discarded shot sends
    /// none.
    fn shot_completed(&self, done: &protocol::ShotCompleted, raw: &str, sender: &BusSender) {
        let id = done.shot_id.as_deref().unwrap_or_default();
        if self.discarded.contains(id) {
            tracing::info!("{}: shot {id} was discarded; no stroke", self.actor);
            return;
        }
        let Some(golfer) = done.player_id.and_then(|p| self.model.golfer(p)) else {
            tracing::info!(
                "{}: the card does not hold the player of shot {id}; no stroke",
                self.actor
            );
            return;
        };
        let key = self.shot_map.get(id);
        let replaces = self.model.replaces(id);
        let Some(stroke) = shots::stroke(done, key, golfer, self.flown.get(id), replaces) else {
            tracing::warn!(
                "{}: shot {id} ended {} at hole {:?}, stroke {:?}; no stroke",
                self.actor,
                done.outcome.as_deref().unwrap_or("(no outcome)"),
                done.hole_number,
                done.stroke_number
            );
            return;
        };
        tracing::info!(
            "{}: shot {id} ended {}{}",
            self.actor,
            done.outcome.as_deref().unwrap_or("(no outcome)"),
            stroke
                .shot_id
                .as_deref()
                .map(|s| format!(", shot {s}"))
                .unwrap_or_default()
        );
        tracing::debug!(target: "audit", "{} stroke from {raw}", self.actor);
        sender.frp(stroke);
    }

    /// The card's `bus.actor`: a game issuing the `pureplay` namespace
    /// (and a virtual launch monitor for the shots it takes), blocked by
    /// `blockers`.
    fn bus_actor(&self, blockers: Vec<String>) -> BusActor {
        let name = crate::actors::actor_names(&self.state.system.snapshot())
            .remove(&self.actor)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Pureplay".into());
        let mut actor = BusActor::new(name, DEFAULT_HEARTBEAT.as_secs_f64())
            .with_role(roles::GAME)
            .with_role(roles::LAUNCH_MONITOR)
            .with_namespace(model::NAMESPACE)
            .with_device(DeviceInfo {
                manufacturer: Some(MANUFACTURER.to_owned()),
                model: Some(MODEL.to_owned()),
                firmware: self.game_version.clone(),
                source: Some(crate::actors::provenance::ShotSource::PureplayAccessory.provenance()),
                ..DeviceInfo::default()
            });
        actor.blockers = blockers;
        actor
    }

    /// Pureplay's readiness blockers, as it names them: none when ready.
    /// Not ready with none named, or no readiness known yet, is
    /// `not_armed`.
    fn blockers(&self) -> Vec<String> {
        match self.model.readiness() {
            Some(r) if r.ready => Vec::new(),
            Some(r) if !r.blockers.is_empty() => r.blockers.clone(),
            _ => vec![blockers::NOT_ARMED.into()],
        }
    }

    /// Publish the card's `bus.actor` when Pureplay's blockers changed.
    fn device_ready(&mut self, sender: &BusSender) {
        let blockers = self.blockers();
        if self.blockers_sent.as_ref() == Some(&blockers) {
            return;
        }
        self.blockers_sent = Some(blockers.clone());
        sender.frp(self.bus_actor(blockers));
    }

    /// The connection ended: the card's `bus.actor` is `not_armed`, when it
    /// said anything else.
    fn end_ready(&mut self, sender: &BusSender) {
        let not_armed = vec![blockers::NOT_ARMED.to_owned()];
        if self.blockers_sent.take().is_some_and(|b| b != not_armed) {
            sender.frp(self.bus_actor(not_armed));
        }
    }

    /// Why a `pair` ended with close `code` (`None`: the socket ended
    /// without one, or the card gave up waiting), after the refused
    /// response's `error`, and what the card does next. Without a code the
    /// response decides.
    fn refused(
        &mut self,
        code: Option<u16>,
        error: Option<&ResponseError>,
        had_token: bool,
    ) -> End {
        let said = |c: &str| error.is_some_and(|e| e.code == c);
        match code {
            Some(4401) => self.dead_token(had_token),
            Some(4403) => {
                self.may_pair = false;
                let text = if said("forbidden_scope") {
                    ASK_OFF
                } else {
                    DENIED
                };
                self.rows.set(PAIRING, text);
                End::Stopped
            }
            Some(4408) => {
                self.may_pair = false;
                self.rows.set(PAIRING, TIMED_OUT);
                End::Stopped
            }
            Some(4429) if said("rate_limited") => self.cooldown(error),
            Some(4429) => {
                self.rows.set(PAIRING, TOO_MANY);
                End::Backoff
            }
            None if said("unauthorised") && had_token => self.dead_token(true),
            None if said("unauthorised") => {
                // Denied, or the prompt timed out: the close code would
                // have told which.
                self.may_pair = false;
                self.rows.set(PAIRING, DENIED);
                End::Stopped
            }
            None if said("forbidden_scope") => {
                self.may_pair = false;
                self.rows.set(PAIRING, ASK_OFF);
                End::Stopped
            }
            None if said("rate_limited") => self.cooldown(error),
            _ => End::Lost,
        }
    }

    /// The saved token was refused: pair once without one when that is
    /// still allowed, else wait for the user's Reconnect.
    fn dead_token(&mut self, had_token: bool) -> End {
        self.forget_token();
        if had_token && self.may_pair {
            tracing::info!("{}: the saved pairing is no longer valid", self.actor);
            End::Again
        } else {
            self.may_pair = false;
            self.rows.set(PAIRING, REMOVED);
            End::Stopped
        }
    }

    /// Pureplay's cooldown, for the wait `error` names.
    fn cooldown(&mut self, error: Option<&ResponseError>) -> End {
        let wait = error
            .and_then(|e| e.retry_after_ms)
            .map_or(DEFAULT_COOLDOWN, Duration::from_millis);
        let retry = std::mem::take(&mut self.cooldown_retry);
        End::Cooldown {
            until: Instant::now() + wait,
            retry,
        }
    }

    /// How a paired connection ended with close `code`; `removal` when an
    /// unprompted `unauthorised` came first, which decides by itself when
    /// the socket ended without a code.
    fn lost(&mut self, code: Option<u16>, removal: bool) -> End {
        match code {
            Some(4401) => self.removed(),
            None if removal => {
                tracing::info!("{}: removed in Pureplay (no close frame)", self.actor);
                self.removed()
            }
            Some(code @ (4403 | 4408 | 4429)) => self.refused(Some(code), None, true),
            _ => End::Lost,
        }
    }

    /// Removed in Pureplay: the token goes; no retry until Reconnect.
    fn removed(&mut self) -> End {
        tracing::info!("{}: removed in Pureplay", self.actor);
        self.forget_token();
        self.may_pair = false;
        self.rows.set(PAIRING, REMOVED);
        End::Stopped
    }

    /// The token to pair with: the token file's, else, while the file
    /// could not keep it, the one Pureplay gave this actor start.
    fn token(&self) -> Option<Token> {
        self.tokens
            .get(&self.key)
            .or_else(|| self.save_error.as_ref().and(self.held.clone()))
    }

    fn forget_token(&mut self) {
        self.held = None;
        match self.tokens.forget(&self.key) {
            Ok(true) => tracing::info!("{}: saved pairing deleted", self.actor),
            Ok(false) => {}
            Err(e) => tracing::warn!("{}: couldn't delete the saved pairing: {e}", self.actor),
        }
    }
}

#[cfg(test)]
mod tests;
