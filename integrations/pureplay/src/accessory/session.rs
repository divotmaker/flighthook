//! The Accessory driver: [`Accessory`], one link to Pureplay at a time, and
//! [`Session`], one link from `hello` to its end.
//!
//! [`FrpDriver::connect`] opens the WebSocket and runs the link's opening
//! (`hello`, pairing, `subscribe` and the snapshot) to its end; the session
//! then follows the round. The opening and the round are one step function
//! ([`FrpSession::step`]), so a test drives both over a fake [`Link`] in
//! virtual time.
//!
//! What the card keeps across links (the token, the round model, the shot
//! map, club out) lives in the driver and is shared with its session.

use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use flightrelay::config::ConfigError;
use flightrelay::driver::{
    Activity, Backoff as HostBackoff, ConnectCtx, DeviceIdentity, DeviceStatus, DriverInfo, Events,
    FrpDriver, FrpSession, IDLE, Level, NoAction, RawProfile, SessionEnd, Stopped,
};
use flightrelay::{FrpMessage, RawDir, Source, roles};
use serde_json::Value;

use super::Config;
use super::card::Card;
use super::link::{Frame, Link, WsLink};
use super::model;
use super::protocol::{self, Capabilities, Hello, Incoming, ResponseError};
use super::token::Token;

/// Reconnect backoff: 1, 2, 4 ... seconds, capped.
pub(crate) const MIN_BACKOFF: Duration = Duration::from_millis(1_000);
pub(crate) const MAX_BACKOFF: Duration = Duration::from_millis(30_000);
/// A link that held this long resets the backoff.
pub(crate) const STABLE: Duration = Duration::from_millis(30_000);
/// A `hello` from another protocol or version is retried this often.
pub(crate) const VERSION_RETRY: Duration = Duration::from_millis(30_000);
/// After `bad_request` to `pair` the card tries again this much later.
pub(crate) const BAD_REQUEST_RETRY: Duration = Duration::from_millis(10_000);
/// `bad_request` answers in a row after which the card stops asking.
const BAD_REQUEST_TRIES: u32 = 6;
/// Longest a TCP connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest the WebSocket handshake may take.
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(2_500);
/// Longest `hello` may take after the handshake.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_millis(5_000);
/// Longest a `pair` with a token may wait for its answer.
pub(crate) const PAIR_TIMEOUT: Duration = Duration::from_millis(5_000);
/// How long a refused `pair` waits for its close frame before the response
/// decides by itself.
pub(crate) const CLOSE_GRACE: Duration = Duration::from_millis(1_000);
/// Longest the snapshot may take after `subscribe`, and a `state.get`'s
/// answer.
pub(crate) const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(5_000);
/// Least time between two `state.get`.
pub(crate) const STATE_GET_GAP: Duration = Duration::from_millis(1_000);
/// The waits before a `state.get` that got no snapshot (no answer in
/// [`SNAPSHOT_TIMEOUT`], or refused) is sent again: one per failure in a
/// row, the last one repeating until a snapshot comes.
pub(crate) const STATE_GET_RETRY: [Duration; 3] = [
    Duration::from_millis(5_000),
    Duration::from_millis(10_000),
    Duration::from_millis(30_000),
];
/// A link that has sent nothing this long is pinged.
pub(crate) const PING_EVERY: Duration = Duration::from_secs(20);
/// Pings without an answer after which the link counts as lost.
const PINGS_UNANSWERED: u32 = 2;
/// How long a socket read waits.
const READ_POLL: Duration = Duration::from_millis(5);
/// Longest a write may block.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// The wait Pureplay's cooldown answer is taken to ask for when it names
/// none: its default, two minutes.
pub(crate) const DEFAULT_COOLDOWN: Duration = Duration::from_secs(120);
/// How often the cooldown's countdown is updated.
const COOLDOWN_TICK: Duration = Duration::from_millis(100);

/// Card telemetry keys.
pub(crate) const PAIRING: &str = "pairing";
pub(crate) const ERROR: &str = "error";
pub(super) const NAME: &str = "name";
pub(super) const CLUB: &str = "club";
pub(super) const HANDED: &str = "handed";
pub(super) const HOLE: &str = "hole";
pub(super) const BLOCKERS: &str = "blockers";
pub(super) const CLUB_OUT: &str = "club_out";
pub(super) const MISSED: &str = "missed_events";
pub(super) const PUREPLAY: &str = "pureplay";
/// The keys that hold for one link.
pub(super) const PER_LINK: [&str; 8] = [
    NAME, CLUB, HANDED, HOLE, BLOCKERS, CLUB_OUT, MISSED, PUREPLAY,
];

/// The card's identity on its `bus.actor`.
const MANUFACTURER: &str = "Pureplay";
const MODEL: &str = "Accessory API";

/// The `pairing` row's texts.
pub(crate) const TOO_MANY: &str = "Pureplay has too many apps connected";
pub(crate) const WAITING: &str = "Waiting for Pureplay";
pub(crate) const ANOTHER_APP: &str =
    "Another app on this computer is waiting for approval in Pureplay";
pub(crate) const ASK_OFF: &str = "Turn on Ask about new apps in Pureplay's Settings > Accessories";
pub(crate) const DENIED: &str = "Pureplay turned the request down";
pub(crate) const TIMED_OUT: &str = "The request timed out in Pureplay";
pub(crate) const REMOVED: &str = "Removed in Pureplay. Press Reconnect to pair again.";
pub(crate) const PRESS_RECONNECT: &str = "Press Reconnect to pair again.";

/// The texts of a link's end.
pub(crate) const NOT_ACCESSORY: &str = "Not a Pureplay Accessory server";
pub(crate) const NO_EVENTS: &str = "Pureplay offers none of the events flighthook follows";

/// The `pairing` row while Pureplay's prompt waits, `secs` left.
pub(crate) fn approve_text(secs: u64) -> String {
    format!("Approve flighthook in Pureplay ({secs} s)")
}

/// The `pairing` row during Pureplay's cooldown, `secs` left.
pub(crate) fn cooldown_text(secs: u64) -> String {
    format!("Pureplay asks to wait ({}:{:02})", secs / 60, secs % 60)
}

/// Whole seconds from `now` to `until`, rounded up.
fn secs_left(until: Instant, now: Instant) -> u64 {
    let left = until.saturating_duration_since(now);
    left.as_secs() + u64::from(left.subsec_nanos() > 0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a connect or a link ended, and so what the card does next
/// ([`FrpDriver::retry_delay`]).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Refused, unreachable or lost: try again after the backoff.
    #[error("{0}")]
    Lost(String),
    /// `4429` before `hello`: try again after the backoff, the `pairing`
    /// row kept.
    #[error("{TOO_MANY}")]
    TooMany,
    /// Try again after `delay`.
    #[error("{reason}")]
    Retry { delay: Duration, reason: String },
    /// The saved token is dead: pair again at once, without it.
    #[error("the saved pairing is no longer valid")]
    Again,
    /// Refused or removed in Pureplay, or nothing to pair with: no retry
    /// until the user's Reconnect. The `pairing` row says why.
    #[error("{PRESS_RECONNECT}")]
    Refused,
    /// Pureplay's cooldown: the next connect waits `wait` first, then tries
    /// once more when `retry`.
    #[error("Pureplay asks to wait")]
    Cooldown { wait: Duration, retry: bool },
    #[error("stopped")]
    Stopped,
}

impl From<Stopped> for Error {
    fn from(_: Stopped) -> Self {
        Self::Stopped
    }
}

/// Exponential reconnect backoff, reset by a link that held for [`STABLE`].
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

/// The `state.get` requests of one link: one unanswered at a time, at
/// least [`STATE_GET_GAP`] apart, and the players they ask for.
///
/// Only a real snapshot answering a request marks the players that request
/// asked for and the snapshot did not bring; they are not asked for again
/// until [`StateGets::clear_not_brought`]. A request that times out or is
/// refused marks nobody: it is sent again after a growing wait
/// ([`STATE_GET_RETRY`]), and its players with it. A player named while a
/// request is out is asked for by the next request.
#[derive(Debug, Default)]
pub(crate) struct StateGets {
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
    pub(crate) fn want(&mut self) {
        if self.pending.is_none() {
            self.wanted = true;
        }
    }

    /// Ask for `player`, named by an event while the card does not hold
    /// them, unless a real snapshot already left them out.
    pub(crate) fn want_player(&mut self, player: i64) {
        if self.not_brought.contains(&player) {
            return;
        }
        self.waiting.insert(player);
        self.wanted = true;
    }

    /// A new match, a context change or a join: every player may be asked
    /// for again.
    pub(crate) fn clear_not_brought(&mut self) {
        self.not_brought.clear();
    }

    /// Whether `request_id` names the request that is out.
    pub(crate) fn is_pending(&self, request_id: Option<&str>) -> bool {
        matches!((&self.pending, request_id), (Some((id, ..)), Some(r)) if r == id)
    }

    /// A snapshot came, answering `request_id`; `holds` says whether it
    /// brought a player. When it answers the request that is out, that
    /// request's players it did not bring are marked. When nobody is left
    /// waiting, nothing more is asked for: the snapshot is as fresh as the
    /// next one would be.
    pub(crate) fn answered(&mut self, request_id: Option<&str>, holds: impl Fn(i64) -> bool) {
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
    pub(crate) fn refused(&mut self, now: Instant, retry_after: Option<Duration>) {
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
    pub(crate) fn due(&mut self, now: Instant) -> Result<Option<String>, String> {
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

/// The Accessory driver: one link to Pureplay at a time, over a WebSocket.
/// See the [module docs](crate::accessory).
#[derive(Debug)]
pub struct Accessory {
    config: Config,
    card: Arc<Mutex<Card>>,
    backoff: Backoff,
    /// Pureplay's cooldown, waited out by the next connect, and whether one
    /// more attempt follows it.
    cooldown: Option<(Duration, bool)>,
}

impl Accessory {
    /// Wait out Pureplay's cooldown, counting it down in the `pairing` row;
    /// then `Err(Refused)` unless `retry`.
    fn wait_cooldown(
        &self,
        ctx: &mut ConnectCtx<'_>,
        wait: Duration,
        retry: bool,
    ) -> Result<(), Error> {
        let until = ctx.now() + wait;
        loop {
            let now = ctx.now();
            if now >= until {
                break;
            }
            {
                let mut card = lock(&self.card);
                card.set(PAIRING, cooldown_text(secs_left(until, now)));
                card.show(&mut ctx.out);
            }
            ctx.wait(until.saturating_duration_since(now).min(COOLDOWN_TICK))?;
        }
        if retry {
            return Ok(());
        }
        let mut card = lock(&self.card);
        card.set(PAIRING, PRESS_RECONNECT);
        card.show(&mut ctx.out);
        Err(Error::Refused)
    }

    /// Run a link's opening over `link`: `hello`, pairing, `subscribe` and
    /// the snapshot, stepping at `ctx`'s time. Returns the session that
    /// follows the round.
    pub(crate) fn open<L: Link>(
        &mut self,
        ctx: &mut ConnectCtx<'_>,
        link: L,
    ) -> Result<Session<L>, Error> {
        let addr = self.addr();
        let mut session = Session::new(link, Arc::clone(&self.card), addr);
        loop {
            let activity = session.step(ctx.now(), &mut ctx.out)?;
            if session.following() {
                return Ok(session);
            }
            match activity {
                Activity::Idle => ctx.wait(IDLE)?,
                _ => ctx.tick()?,
            }
        }
    }

    fn addr(&self) -> SocketAddr {
        self.config
            .addr()
            .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 9770)))
    }

    /// What starts every connect: the rows the card shows, the cooldown,
    /// something to pair with.
    fn before_open(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<(), Error> {
        lock(&self.card).restore(&mut ctx.out);
        if let Some((wait, retry)) = self.cooldown.take() {
            self.wait_cooldown(ctx, wait, retry)?;
        }
        let card = lock(&self.card);
        if card.token().is_none() && !card.may_pair {
            return Err(Error::Refused);
        }
        Ok(())
    }
}

impl FrpDriver for Accessory {
    type Config = Config;
    type Error = Error;
    type Action = NoAction;
    type Session = Session<WsLink>;

    /// The bus's `game`, issuing the `pureplay` golfer namespace, and a
    /// virtual launch monitor for Pureplay's shots. Its frames are raw text
    /// frames (the tokens replaced), kept by no shot.
    fn info(_: &Config) -> DriverInfo {
        DriverInfo::new("pureplay_accessory", "Pureplay", Source::Virtual)
            .with_manufacturer(MANUFACTURER)
            .with_roles(&[roles::GAME, roles::LAUNCH_MONITOR])
            .with_namespaces(&[model::NAMESPACE])
            .with_raw(RawProfile::new("pureplay-ws", 0, 0, 0, |_| false, true))
    }

    fn new(config: Config) -> Result<Self, ConfigError> {
        let addr = config.addr()?;
        Ok(Self {
            card: Arc::new(Mutex::new(Card::new(&config, addr))),
            config,
            backoff: Backoff::new(),
            cooldown: None,
        })
    }

    fn config(&self) -> &Config {
        &self.config
    }

    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<Session<WsLink>, Error> {
        self.before_open(ctx)?;
        let addr = self.addr();
        let stream = match ctx.connect_tcp(addr, CONNECT_TIMEOUT)? {
            Ok(stream) => stream,
            Err(e) => return Err(Error::Lost(format!("no connection to {addr}: {e}"))),
        };
        let link = open_ws(stream, addr, ctx)?;
        ctx.out.log(Level::Info, format!("connected to {addr}"));
        self.open(ctx, link)
    }

    /// The card's backoff (1, 2, 4 ... 30 s, reset by a link that held for
    /// 30 s), a set wait, at once, or no retry after a refusal or a removal
    /// (Press Reconnect). No alert.
    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, Error>,
        _backoff: &mut HostBackoff,
        out: &mut Events<'_>,
    ) -> Option<Duration> {
        let mut card = lock(&self.card);
        if let Some(held) = ended.connected_for {
            // The host and the card hold no status after a link.
            card.shown = DeviceStatus::not_armed();
            if held >= STABLE {
                self.backoff.reset();
            }
        }
        let delay = match ended.error {
            Some(Error::Lost(_)) => {
                card.rows.remove(PAIRING);
                Some(self.backoff.delay())
            }
            Some(Error::TooMany) => Some(self.backoff.delay()),
            Some(Error::Retry { delay, .. }) => Some(*delay),
            Some(Error::Cooldown { wait, retry }) => {
                self.cooldown = Some((*wait, *retry));
                Some(Duration::ZERO)
            }
            Some(Error::Refused) => None,
            Some(Error::Again | Error::Stopped) | None => Some(Duration::ZERO),
        };
        card.show(out);
        if let Some(wait) = delay {
            out.log(Level::Info, format!("next attempt in {wait:?}"));
        }
        delay
    }

    fn apply_config(&mut self, new: &Config) {
        self.config = new.clone();
    }
}

/// Open the WebSocket on `stream`, asking `ctx` between polls.
fn open_ws(stream: TcpStream, addr: SocketAddr, ctx: &mut ConnectCtx<'_>) -> Result<WsLink, Error> {
    let lost = |e: std::io::Error| Error::Lost(format!("connection lost: {e}"));
    stream.set_nodelay(true).map_err(lost)?;
    stream.set_read_timeout(Some(READ_POLL)).map_err(lost)?;
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(lost)?;
    WsLink::open(stream, addr, WS_HANDSHAKE_TIMEOUT, || ctx.tick().is_ok()).map_err(|e| match e {
        super::link::OpenError::Stopped => Error::Stopped,
        e => Error::Lost(e.to_string()),
    })
}

/// Where a link is.
#[derive(Debug)]
enum Phase {
    /// Waiting for `hello`.
    Hello,
    /// `pair` sent.
    Pair {
        had_token: bool,
        /// Only a pair without a token waits in Pureplay's queue for as
        /// long as it takes; a refusal waits a moment for its close frame.
        deadline: Option<Instant>,
        refusal: Option<ResponseError>,
        prompt_until: Option<Instant>,
    },
    /// `subscribe` sent at `asked`.
    Subscribe { asked: Instant },
    /// Following the round.
    Following,
}

/// What a read gave. One per frame, taken apart at once: not worth a box.
#[allow(clippy::large_enum_variant)]
enum Read {
    /// Nothing arrived.
    Nothing,
    /// A message, with its frame's text, the tokens replaced.
    Msg(Incoming, String),
    /// The link ended: the close code, `None` without a close frame.
    Closed(Option<u16>),
}

/// One link to Pureplay over `L`, from `hello` to its end: the driver's
/// [`FrpSession`], over a WebSocket, or over any other `L` in a test.
#[derive(Debug)]
pub struct Session<L = WsLink> {
    link: L,
    card: Arc<Mutex<Card>>,
    addr: SocketAddr,
    identity: DeviceIdentity,
    phase: Phase,
    /// When the first step ran.
    started: Option<Instant>,
    /// When a frame last arrived.
    heard: Option<Instant>,
    /// Bus messages handed since the last step.
    inbox: Vec<FrpMessage>,
    gets: StateGets,
    /// An unprompted `unauthorised` came: a removal in Pureplay.
    removal: bool,
    pings: u32,
    last_seq: Option<u64>,
    capabilities: Option<Capabilities>,
    /// The token the link pairs with.
    saved: Option<Token>,
}

impl<L: Link> FrpSession for Session<L> {
    type Error = Error;
    type Action = NoAction;

    /// One pass at `now`: what the bus handed, then at most one frame and
    /// what is due. An `Err` ends the link; the card's game state leaves
    /// the bus first.
    ///
    /// # Errors
    ///
    /// Returns how the link ended.
    fn step(&mut self, now: Instant, out: &mut Events<'_>) -> Result<Activity, Error> {
        let card = Arc::clone(&self.card);
        let mut card = lock(&card);
        let stepped = self.step_with(&mut card, now, out);
        if stepped.is_err() {
            card.end_link(out);
        }
        card.show(out);
        stepped
    }

    /// A bus message for the link, taken at the next step.
    fn bus(&mut self, msg: &FrpMessage, _: &mut Events<'_>) -> Result<(), Error> {
        self.inbox.push(msg.clone());
        Ok(())
    }

    fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// The card's game state leaves the bus and the link closes.
    fn close(mut self, out: &mut Events<'_>) -> Result<(), Error> {
        lock(&self.card).end_link(out);
        self.link.close();
        Ok(())
    }
}

impl<L: Link> Session<L> {
    fn new(link: L, card: Arc<Mutex<Card>>, addr: SocketAddr) -> Self {
        Self {
            link,
            card,
            addr,
            identity: identity(addr, None),
            phase: Phase::Hello,
            started: None,
            heard: None,
            inbox: Vec::new(),
            gets: StateGets::default(),
            removal: false,
            pings: 0,
            last_seq: None,
            capabilities: None,
            saved: None,
        }
    }

    /// Whether the link follows the round (its opening is done).
    pub(crate) fn following(&self) -> bool {
        matches!(self.phase, Phase::Following)
    }

    fn step_with(
        &mut self,
        card: &mut Card,
        now: Instant,
        out: &mut Events<'_>,
    ) -> Result<Activity, Error> {
        let started = *self.started.get_or_insert(now);
        self.heard.get_or_insert(now);
        for msg in std::mem::take(&mut self.inbox) {
            card.bus(&msg, now, out);
        }
        match &mut self.phase {
            Phase::Hello => self.hello(card, started, now, out),
            Phase::Pair { .. } => self.pair(card, now, out),
            Phase::Subscribe { asked } => {
                let asked = *asked;
                self.subscribe(card, asked, now, out)
            }
            Phase::Following => self.follow(card, now, out),
        }
    }

    /// Read what has arrived.
    fn read(&mut self, card: &mut Card, now: Instant, out: &mut Events<'_>) -> Read {
        let frame = match self.link.recv() {
            Ok(Some(frame)) => {
                self.heard = Some(now);
                frame
            }
            Ok(None) => return Read::Nothing,
            Err(e) => {
                out.log(Level::Info, format!("connection lost: {e}"));
                return Read::Closed(None);
            }
        };
        let (text, kind) = match frame {
            Frame::Text(t) => (t, "text"),
            Frame::Binary(b) => match String::from_utf8(b) {
                Ok(t) => (t, "binary"),
                Err(e) => {
                    out.log(
                        Level::Warn,
                        format!("received binary, not UTF-8: {}", hex(e.as_bytes())),
                    );
                    return Read::Nothing;
                }
            },
            Frame::Close { code, reason } => {
                let line = format!(
                    "{} {}",
                    code.map_or_else(|| "(no code)".to_owned(), |c| c.to_string()),
                    card.redact(&reason)
                );
                out.wire(
                    RawDir::Rx,
                    "close",
                    line.as_bytes().to_vec(),
                    flightrelay::utc_ms(),
                    false,
                );
                return Read::Closed(code);
            }
            Frame::Control => return Read::Nothing,
        };
        // A `paired` frame carries a token the card has not seen: learn it
        // before anything logs the frame, however the rest of it reads.
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            out.log(
                Level::Warn,
                format!(
                    "received {kind}, {} bytes, not JSON (not shown)",
                    text.len()
                ),
            );
            return Read::Nothing;
        };
        if value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("paired"))
            && let Some(t) = value.pointer("/data/token").and_then(Value::as_str)
        {
            card.learn(t);
        }
        let shown = card.redact(&text);
        out.wire(
            RawDir::Rx,
            kind,
            shown.as_bytes().to_vec(),
            flightrelay::utc_ms(),
            false,
        );
        match protocol::parse(&text) {
            Ok(msg) => Read::Msg(msg, shown),
            Err(e) => {
                out.log(
                    Level::Warn,
                    format!(
                        "unreadable message ({}), ignored",
                        card.redact(&e.to_string())
                    ),
                );
                Read::Nothing
            }
        }
    }

    /// Waiting for `hello`: check it, then pair.
    fn hello(
        &mut self,
        card: &mut Card,
        started: Instant,
        now: Instant,
        out: &mut Events<'_>,
    ) -> Result<Activity, Error> {
        let hello = match self.read(card, now, out) {
            Read::Msg(Incoming::Hello(h), _) => h,
            Read::Msg(..) => return Ok(Activity::Active),
            Read::Closed(Some(4429)) => {
                card.set(PAIRING, TOO_MANY);
                return Err(Error::TooMany);
            }
            Read::Closed(_) => return Err(Error::Lost("connection lost".into())),
            Read::Nothing => {
                if now.saturating_duration_since(started) >= HELLO_TIMEOUT {
                    return Err(Error::Lost("no hello from Pureplay".into()));
                }
                return Ok(Activity::Idle);
            }
        };
        let retry = |reason: String| Error::Retry {
            delay: VERSION_RETRY,
            reason,
        };
        if hello.protocol.as_deref() != Some(protocol::PROTOCOL) || hello.version.is_none() {
            self.link.close();
            return Err(retry(NOT_ACCESSORY.into()));
        }
        if let Some(version) = hello.version.filter(|v| *v != protocol::VERSION) {
            self.link.close();
            return Err(retry(format!(
                "Pureplay speaks Accessory version {version}; this flighthook speaks {}",
                protocol::VERSION
            )));
        }
        self.take_hello(card, &hello, out);
        self.send_pair(card, now, out)?;
        Ok(Activity::Active)
    }

    /// Keep what `hello` says: the instance (a new one drops the cached
    /// round), the version and the commands.
    fn take_hello(&mut self, card: &mut Card, hello: &Hello, out: &mut Events<'_>) {
        if let Some(instance) = &hello.server_instance_id {
            if card.server_instance.as_ref().is_some_and(|s| s != instance) {
                out.log(Level::Info, "Pureplay restarted, cached state dropped");
                card.model.reset();
                card.club_out.clear();
                card.shot_map.clear();
                card.flown.clear();
                card.discarded.clear();
            }
            card.server_instance = Some(instance.clone());
        }
        card.game_version.clone_from(&hello.game_version);
        card.commands = hello
            .capabilities
            .as_ref()
            .map(|c| c.commands.clone())
            .unwrap_or_default();
        self.capabilities.clone_from(&hello.capabilities);
        self.identity = identity(self.addr, card.game_version.as_deref());
        out.log(
            Level::Info,
            format!(
                "Pureplay {} offers commands {}",
                card.game_version.as_deref().unwrap_or("(no version)"),
                card.commands.join(", ")
            ),
        );
    }

    /// Pair, with the saved token or by asking the player.
    fn send_pair(
        &mut self,
        card: &mut Card,
        now: Instant,
        out: &mut Events<'_>,
    ) -> Result<(), Error> {
        let saved = card.token();
        if let Some(t) = &saved {
            card.learn(t.expose());
        } else {
            card.set(PAIRING, WAITING);
        }
        let pair = protocol::pair(saved.as_ref().map(Token::expose));
        if card.send(&mut self.link, &pair, out).is_err() {
            return Err(Error::Lost("send failed".into()));
        }
        self.phase = Phase::Pair {
            had_token: saved.is_some(),
            deadline: saved.is_some().then(|| now + PAIR_TIMEOUT),
            refusal: None,
            prompt_until: None,
        };
        self.saved = saved;
        Ok(())
    }

    /// `pair` sent: the queue, the prompt, `paired`, or a refusal.
    fn pair(
        &mut self,
        card: &mut Card,
        now: Instant,
        out: &mut Events<'_>,
    ) -> Result<Activity, Error> {
        let read = self.read(card, now, out);
        let Phase::Pair {
            had_token,
            deadline,
            refusal,
            prompt_until,
        } = &mut self.phase
        else {
            return Ok(Activity::Idle);
        };
        let had_token = *had_token;
        match read {
            Read::Msg(Incoming::Paired(p), _) => {
                let saved = self.saved.clone();
                card.keep_pairing(&p, saved.as_ref(), out);
                self.send_subscribe(card, now, out)?;
            }
            Read::Msg(Incoming::PairingPending { expires_in_sec }, _) => {
                let secs = expires_in_sec.unwrap_or(60);
                *prompt_until = Some(now + Duration::from_secs(secs));
                card.set(PAIRING, approve_text(secs));
            }
            Read::Msg(Incoming::Response(r), _) if !r.ok => {
                let error = r.error.unwrap_or_default();
                let message = card.redact(error.message.as_deref().unwrap_or(""));
                out.log(
                    Level::Info,
                    format!("pair refused: {} {message}", card.redact(&error.code)),
                );
                if error.code == "bad_request" && !had_token {
                    self.link.close();
                    card.bad_requests += 1;
                    if card.bad_requests >= BAD_REQUEST_TRIES {
                        let text = if message.is_empty() {
                            ANOTHER_APP.to_owned()
                        } else {
                            message
                        };
                        return Err(card.stop(&text));
                    }
                    card.set(PAIRING, ANOTHER_APP);
                    return Err(Error::Retry {
                        delay: BAD_REQUEST_RETRY,
                        reason: ANOTHER_APP.into(),
                    });
                }
                *refusal = Some(error);
                *deadline = Some(now + CLOSE_GRACE);
            }
            Read::Msg(..) => {}
            Read::Closed(code) => {
                let refusal = refusal.take();
                return Err(card.refused(code, refusal.as_ref(), had_token, out));
            }
            Read::Nothing => {
                if let Some(until) = *prompt_until {
                    card.set(PAIRING, approve_text(secs_left(until, now)));
                }
                if deadline.is_some_and(|d| now >= d) {
                    let refusal = refusal.take();
                    self.link.close();
                    if refusal.is_none() {
                        out.log(Level::Info, "no answer to pair");
                    }
                    return Err(card.refused(None, refusal.as_ref(), had_token, out));
                }
                return Ok(Activity::Idle);
            }
        }
        Ok(Activity::Active)
    }

    /// Subscribe to the events Pureplay offers, with a snapshot.
    fn send_subscribe(
        &mut self,
        card: &mut Card,
        now: Instant,
        out: &mut Events<'_>,
    ) -> Result<(), Error> {
        let events = protocol::events_to_subscribe(self.capabilities.as_ref());
        if events.is_empty() {
            // An empty list would subscribe to every event.
            out.log(Level::Warn, "Pureplay lists none of the card's events");
            self.link.close();
            return Err(Error::Retry {
                delay: VERSION_RETRY,
                reason: NO_EVENTS.into(),
            });
        }
        if card
            .send(&mut self.link, &protocol::subscribe(&events), out)
            .is_err()
        {
            return Err(Error::Lost("send failed".into()));
        }
        self.phase = Phase::Subscribe { asked: now };
        Ok(())
    }

    /// `subscribe` sent: the snapshot, then the round is followed.
    fn subscribe(
        &mut self,
        card: &mut Card,
        asked: Instant,
        now: Instant,
        out: &mut Events<'_>,
    ) -> Result<Activity, Error> {
        match self.read(card, now, out) {
            Read::Msg(Incoming::Response(r), _)
                if r.id.as_deref() == Some(protocol::SUBSCRIBE_ID) && !r.ok =>
            {
                out.log(
                    Level::Warn,
                    format!(
                        "subscribe refused: {}",
                        card.redact(&format!("{:?}", r.error))
                    ),
                );
                self.link.close();
                Err(Error::Lost("subscribe refused".into()))
            }
            Read::Msg(Incoming::Response(r), _) if r.id.is_none() && !r.ok => {
                self.removal |= unprompted(card, &r, out);
                Ok(Activity::Active)
            }
            Read::Msg(Incoming::Event(e), _) if e.name == "state.snapshot" => {
                card.snapshot(&e, out);
                self.last_seq = e.seq;
                self.begin_following(card, now, out);
                Ok(Activity::Active)
            }
            Read::Msg(..) => Ok(Activity::Active),
            Read::Closed(code) => Err(card.lost(code, self.removal, out)),
            Read::Nothing => {
                if now.saturating_duration_since(asked) >= SNAPSHOT_TIMEOUT {
                    out.log(Level::Info, "no snapshot after subscribe");
                    self.link.close();
                    return Err(Error::Lost("no snapshot after subscribe".into()));
                }
                Ok(Activity::Idle)
            }
        }
    }

    /// The snapshot came: the round is followed from now on.
    fn begin_following(&mut self, card: &mut Card, now: Instant, out: &mut Events<'_>) {
        card.missed = 0;
        let (scopes, commands) = (card.scopes.clone(), card.commands.clone());
        card.club_out.link(&scopes, &commands);
        card.following = true;
        card.link_rows();
        self.phase = Phase::Following;
        self.heard = Some(now);
        out.log(Level::Info, "following the round");
    }

    /// One pass while following the round.
    fn follow(
        &mut self,
        card: &mut Card,
        now: Instant,
        out: &mut Events<'_>,
    ) -> Result<Activity, Error> {
        if let Some(club) = card.picked.take() {
            let actions = card.club_out.pick(club, &card.model, now);
            card.club_actions(actions, &mut self.link, None, now, out);
        }
        // Compare the turn the card holds with flighthook's, after a pick
        // handed in the same pass is held.
        card.sync(now, out);
        let mut activity = Activity::Idle;
        match self.read(card, now, out) {
            Read::Msg(Incoming::Event(e), raw) => {
                activity = Activity::Active;
                self.event(card, &e, &raw, now, out);
            }
            Read::Msg(Incoming::Response(r), _) if r.id.is_none() && !r.ok => {
                activity = Activity::Active;
                self.removal |= unprompted(card, &r, out);
            }
            Read::Msg(Incoming::Response(r), _) if self.gets.is_pending(r.id.as_deref()) => {
                activity = Activity::Active;
                if !r.ok {
                    let error = r.error.as_ref();
                    out.log(
                        Level::Info,
                        format!(
                            "state.get refused: {}",
                            card.redact(error.map_or("", |e| e.code.as_str()))
                        ),
                    );
                    let retry_after = error
                        .and_then(|e| e.retry_after_ms)
                        .map(Duration::from_millis);
                    self.gets.refused(now, retry_after);
                }
            }
            Read::Msg(Incoming::Response(r), raw) => {
                activity = Activity::Active;
                if let Some(actions) = card.club_out.reply(&r, &mut card.model, now) {
                    card.club_actions(actions, &mut self.link, Some(&raw), now, out);
                }
            }
            Read::Msg(..) => activity = Activity::Active,
            Read::Nothing => {}
            Read::Closed(code) => return Err(card.lost(code, self.removal, out)),
        }
        let actions = card.club_out.drive(&card.model, now);
        card.club_actions(actions, &mut self.link, None, now, out);
        card.link_rows();
        match self.gets.due(now) {
            Ok(Some(id)) => {
                let _ = card.send(&mut self.link, &protocol::state_get(&id), out);
            }
            Ok(None) => {}
            Err(id) => out.log(
                Level::Warn,
                format!("no answer to state.get {id}, asking again later"),
            ),
        }
        self.ping(now, out)?;
        Ok(activity)
    }

    /// An event while following.
    fn event(
        &mut self,
        card: &mut Card,
        e: &protocol::Event,
        raw: &str,
        now: Instant,
        out: &mut Events<'_>,
    ) {
        if let (Some(last), Some(seq)) = (self.last_seq, e.seq) {
            if seq > last + 1 {
                card.missed += 1;
                out.log(
                    Level::Warn,
                    format!(
                        "events {}..{} missed, asking for the state",
                        last + 1,
                        seq - 1
                    ),
                );
                self.gets.want();
            } else if seq < last {
                out.log(
                    Level::Info,
                    format!("event seq {seq} after {last}, counting from it"),
                );
            }
        }
        self.last_seq = e.seq.or(self.last_seq);
        if e.name == "state.snapshot" {
            card.snapshot(e, out);
            let model = &card.model;
            self.gets
                .answered(e.request_id.as_deref(), |id| model.player(id).is_some());
            card.club_out.readiness_changed(card.model.readiness());
            let actions = card.club_out.drive(&card.model, now);
            card.club_actions(actions, &mut self.link, Some(raw), now, out);
        } else {
            // What drops a pending pick is known before the model gives the
            // club of a new turn.
            let mut actions = card.club_out.before_event(e);
            card.model.hold_club(card.club_out.holds_club());
            if let Err(err) = card.model.apply_event(e) {
                out.log(
                    Level::Warn,
                    format!("{} not read: {}", e.name, card.redact(&err.to_string())),
                );
            }
            card.shot_event(e, raw, out);
            card.game_changes(out);
            if matches!(
                e.name.as_str(),
                "context.invalidated" | "match.started" | "player.joined"
            ) {
                self.gets.clear_not_brought();
            }
            if e.name == "context.invalidated" || e.name == "match.started" {
                self.gets.want();
            } else if let Some(id) = card.model.unknown_player(e) {
                self.gets.want_player(id);
            }
            actions.extend(card.club_out.drive(&card.model, now));
            card.club_actions(actions, &mut self.link, Some(raw), now, out);
        }
        card.sync(now, out);
    }

    /// Ping a link that has gone quiet; lost after [`PINGS_UNANSWERED`].
    fn ping(&mut self, now: Instant, out: &mut Events<'_>) -> Result<(), Error> {
        let quiet = now.saturating_duration_since(self.heard.unwrap_or(now));
        if quiet < PING_EVERY {
            self.pings = 0;
        } else if quiet >= PING_EVERY * (self.pings + 1) {
            if self.pings >= PINGS_UNANSWERED {
                out.log(
                    Level::Info,
                    format!(
                        "nothing from Pureplay for {quiet:?}, {} pings unanswered",
                        self.pings
                    ),
                );
                self.link.close();
                return Err(Error::Lost("no answer to pings".into()));
            }
            out.wire(RawDir::Tx, "ping", Vec::new(), flightrelay::utc_ms(), false);
            if let Err(e) = self.link.ping() {
                out.log(Level::Info, format!("ping failed: {e}"));
                return Err(Error::Lost(format!("ping failed: {e}")));
            }
            self.pings += 1;
        }
        Ok(())
    }
}

/// An unprompted refused response (`id: null`): logged. Returns whether
/// it tells of a removal in Pureplay (`unauthorised`).
fn unprompted(card: &Card, r: &protocol::Response, out: &mut Events<'_>) -> bool {
    let code = r.error.as_ref().map_or("", |e| e.code.as_str());
    out.log(Level::Info, format!("Pureplay says {}", card.redact(code)));
    code == "unauthorised"
}

/// The card's identity for the link to `addr`: Pureplay's Accessory API,
/// the game's `version` as its firmware, labeled with the address.
fn identity(addr: SocketAddr, version: Option<&str>) -> DeviceIdentity {
    let identity = DeviceIdentity::new(addr.to_string())
        .with_manufacturer(MANUFACTURER)
        .with_model(MODEL)
        .with_label(addr.to_string());
    match version {
        Some(v) => identity.with_firmware(v),
        None => identity,
    }
}

/// `bytes` as lower-case hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
