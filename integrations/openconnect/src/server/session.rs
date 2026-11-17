//! The Open Connect server driver and its session. See the
//! [module docs](super).

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use flightrelay::config::{ConfigError, DriverConfig};
use flightrelay::driver::{
    Activity, Backoff, ConnectCtx, DeviceIdentity, DriverInfo, Events, FrpDriver, FrpSession, IDLE,
    Level, Live, NoAction, RawProfile, SessionEnd, ShotIds, Stopped, TurnHints,
};
use flightrelay::{Club, Handedness, RawDir, Severity, ShotKey, Source};

use super::{Config, mapper};
use crate::client::api::{GsProMessage, GsProResponse, ShotDataOptions};
use crate::local;

/// Cap on buffered bytes from a single peer before the connection is
/// dropped. A well-behaved client never approaches this; it exists so a peer
/// that opens a socket and streams garbage cannot grow the buffer without
/// bound.
const MAX_BUFFER: usize = 1 << 20;

/// Refusals of one client closer together than this are one burst of its
/// reconnect attempts, alerted once. The client retries every 1-3 s.
const REFUSAL_BURST_GAP: Duration = Duration::from_secs(30);

/// How long after a reply (200) the player information (201) waits, so the
/// two never arrive in the same read on the monitor's side.
pub(crate) const PLAYER_INFO_GAP: Duration = Duration::from_millis(50);

/// What a connect or a session failed with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Open Connect server could not bind {addr}: {source}")]
    Bind { addr: SocketAddr, source: io::Error },
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// The monitor closed the connection.
    #[error("the monitor disconnected")]
    PeerLeft,
    /// The peer is one of this process's own Open Connect clients.
    #[error("the peer is this process's own Open Connect client")]
    OwnClient,
    /// The peer sent more than the buffer holds with no message in it.
    #[error("Open Connect peer sent unparsable data, dropping connection")]
    Overflow,
    #[error("stopped")]
    Stopped,
}

impl From<Stopped> for Error {
    fn from(_: Stopped) -> Self {
        Self::Stopped
    }
}

/// Refusals so far, by client actor id: when each was last refused.
#[derive(Debug, Default)]
pub(crate) struct Refusals(HashMap<String, Instant>);

impl Refusals {
    /// Record a refusal of `client` at `now`. Returns whether it starts a
    /// new burst (and so is alerted).
    fn record(&mut self, client: &str, now: Instant) -> bool {
        let last = self.0.insert(client.to_string(), now);
        last.is_none_or(|at| now.saturating_duration_since(at) >= REFUSAL_BURST_GAP)
    }
}

/// What the server does about its own client `client` connecting from
/// `peer`: tells it why (`reason`), and alerts once per burst.
fn refuse(
    refusals: &Live<Refusals>,
    client: &str,
    peer: SocketAddr,
    reason: &str,
    now: Instant,
    out: &mut Events<'_>,
) {
    local::refuse(peer, reason);
    out.log(
        Level::Warn,
        format!("refused {peer}, from {client}: it is this process's own client"),
    );
    if refusals.update(|r| r.record(client, now)) {
        out.alert(
            Severity::Error,
            format!(
                "Refused Open Connect Client \"{client}\" from {peer}: it is this \
                 flighthook's own client, which would loop shots back. Point it at the \
                 simulator's port."
            ),
        );
    }
}

/// The reason the server gives a client of this process that points at it,
/// listening on `bound`.
fn refusal_reason(bound: SocketAddr) -> String {
    format!(
        "Refused by this flighthook's own Open Connect Server on {bound}: this \
         address points at it, which would loop shots back. Use the simulator's \
         port instead."
    )
}

/// The first complete message in `buf` and the bytes it takes; `None` while
/// there is none yet.
fn next_message(buf: &[u8]) -> Result<Option<(GsProMessage, usize)>, serde_json::Error> {
    let mut iter = serde_json::Deserializer::from_slice(buf).into_iter::<GsProMessage>();
    match iter.next() {
        None => Ok(None),
        Some(Ok(msg)) => Ok(Some((msg, iter.byte_offset()))),
        Some(Err(e)) if e.is_eof() => Ok(None),
        Some(Err(e)) => Err(e),
    }
}

/// What `stream` has for us, appended to `buf`: `None` while there is
/// nothing; an `Ok(0)` read is [`Error::PeerLeft`].
fn read_into(stream: &mut impl Read, buf: &mut Vec<u8>) -> Result<Option<usize>, Error> {
    let mut chunk = [0u8; 4096];
    match stream.read(&mut chunk) {
        Ok(0) => Err(Error::PeerLeft),
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > MAX_BUFFER {
                return Err(Error::Overflow);
            }
            Ok(Some(n))
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(Error::Io(e)),
    }
}

/// Whether a message is a shot: not a heartbeat, and it says it carries
/// ball data. Open Connect sends every field of its fixed schema, so a block
/// the message does not say it carries holds zeros, not readings.
pub(super) fn is_shot(options: &ShotDataOptions) -> bool {
    !options.is_heart_beat && options.contains_ball_data
}

/// The Open Connect server driver: one monitor at a time, over TCP.
#[derive(Debug)]
pub struct Server {
    config: Config,
    /// Bound by the first connect and kept across sessions.
    listener: Option<TcpListener>,
    refusals: Live<Refusals>,
    ids: ShotIds,
}

impl FrpDriver for Server {
    type Config = Config;
    type Error = Error;
    type Action = NoAction;
    type Session = Session<TcpStream>;

    /// A measured launch monitor: the monitor software's own app sends it
    /// the shots that monitor measured. It holds the listen port, so a
    /// replacement waits for it to stop. Its frames are text.
    fn info(_: &Config) -> DriverInfo {
        DriverInfo::new("openconnect_server", "Open Connect", Source::Measured)
            .exclusive()
            .with_telemetry(&[("ball_detected", "unknown")])
            .with_raw(RawProfile::new(
                "openconnect-json",
                0,
                16,
                0,
                |_| false,
                true,
            ))
    }

    fn new(config: Config) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            listener: None,
            refusals: Live::default(),
            ids: ShotIds::new(),
        })
    }

    fn config(&self) -> &Config {
        &self.config
    }

    /// Bind (once), accept, and wait for the peer's first message.
    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<Session<TcpStream>, Error> {
        let listener = match self.listener.take() {
            Some(listener) => listener,
            None => self.bind(ctx)?,
        };
        let accepted = self.accept(&listener, ctx);
        self.listener = Some(listener);
        accepted
    }

    /// A failed bind is an Error alert and the host's backoff (1 s growing
    /// to 15 s); a monitor that left or a client of this process is followed
    /// by the next connect at once, with no alert; anything else is a Warn
    /// alert and the backoff.
    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, Error>,
        backoff: &mut Backoff,
        out: &mut Events<'_>,
    ) -> Option<Duration> {
        match ended.error {
            Error::PeerLeft | Error::OwnClient => return Some(Duration::ZERO),
            error @ Error::Bind { .. } => out.alert(Severity::Error, error.to_string()),
            error => out.alert(Severity::Warn, error.to_string()),
        }
        Some(backoff.delay(ended.connected_for))
    }

    fn apply_config(&mut self, new: &Config) {
        self.config = new.clone();
    }
}

impl Server {
    fn bind(&self, ctx: &mut ConnectCtx<'_>) -> Result<TcpListener, Error> {
        let addr = self
            .config
            .addr()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let listener = TcpListener::bind(addr).map_err(|source| Error::Bind { addr, source })?;
        listener.set_nonblocking(true)?;
        ctx.out.log(
            Level::Info,
            format!("listening on {}", listener.local_addr()?),
        );
        Ok(listener)
    }

    /// Accept peers until one sends a first message.
    fn accept(
        &self,
        listener: &TcpListener,
        ctx: &mut ConnectCtx<'_>,
    ) -> Result<Session<TcpStream>, Error> {
        let reason = refusal_reason(listener.local_addr()?);
        ctx.out.phase("listening");
        loop {
            match listener.accept() {
                Ok((stream, peer)) => {
                    ctx.out.phase("handshaking");
                    let greeted = self.greet(stream, peer, &reason, ctx)?;
                    if let Some((stream, buf, first)) = greeted {
                        let identity = DeviceIdentity::new(peer.to_string())
                            .with_model(first.device_id.clone())
                            .with_label(format!("{} ({peer})", first.device_id));
                        let mut session = Session::new(
                            stream,
                            peer,
                            identity,
                            buf,
                            self.refusals.clone(),
                            reason,
                            self.ids.clone(),
                        );
                        session.hints = *ctx.hints();
                        ctx.out.phase("measuring");
                        return Ok(session);
                    }
                    ctx.out.phase("listening");
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => ctx.wait(IDLE)?,
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }

    /// Wait for `peer`'s first message. `None` when the peer is one of this
    /// process's own clients (refused), left, or sent only garbage.
    fn greet(
        &self,
        stream: TcpStream,
        peer: SocketAddr,
        reason: &str,
        ctx: &mut ConnectCtx<'_>,
    ) -> Result<Option<(TcpStream, Vec<u8>, GsProMessage)>, Error> {
        let mut stream = stream;
        // Some platforms hand out accepted sockets in the listener's mode.
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let mut buf = Vec::new();
        loop {
            match read_into(&mut stream, &mut buf) {
                Ok(_) => {}
                Err(Error::PeerLeft) => return Ok(None),
                Err(Error::Overflow) => {
                    ctx.out.alert(Severity::Warn, Error::Overflow.to_string());
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
            // The peer is looked up after each read, before the read is
            // handled: our client registers a moment after it connects.
            if let Some(client) = local::client_at(peer) {
                let now = ctx.now();
                refuse(&self.refusals, &client, peer, reason, now, &mut ctx.out);
                let _ = stream.shutdown(Shutdown::Both);
                return Ok(None);
            }
            match next_message(&buf) {
                Ok(Some((first, _))) => return Ok(Some((stream, buf, first))),
                Ok(None) => {}
                Err(e) => {
                    ctx.out
                        .alert(Severity::Warn, format!("Open Connect parse error: {e}"));
                    buf.clear();
                }
            }
            ctx.wait(IDLE)?;
        }
    }
}

/// One monitor's connection over transport `S`, from its first message to
/// its end: the driver's [`FrpSession`], over TCP, or over any other `S` in
/// a test.
#[derive(Debug)]
pub struct Session<S = TcpStream> {
    stream: S,
    peer: SocketAddr,
    identity: DeviceIdentity,
    /// What the peer sent that is not handled yet.
    buf: Vec<u8>,
    refusals: Live<Refusals>,
    /// Why this process's own client is refused.
    reason: String,
    ids: ShotIds,
    /// The hand and club of the turn.
    hints: TurnHints,
    /// The readiness and ball detection the monitor last said.
    ready: Option<bool>,
    ball_detected: Option<bool>,
    /// Whether a message has been handled: the monitor is told the player
    /// after the first one has been answered.
    announced: bool,
    /// The hand and club of the last player information sent; `None` for a
    /// field not sent yet.
    player_sent: (Option<Handedness>, Option<Club>),
    /// When the last reply (200) was written.
    replied_at: Option<Instant>,
}

impl<S: Read + Write> FrpSession for Session<S> {
    type Error = Error;
    type Action = NoAction;

    /// One pass at `now`: a look at who the peer is, a read of what the
    /// peer sent, the messages waiting, then the player information when it
    /// is due.
    ///
    /// # Errors
    ///
    /// Returns what ended the connection: the monitor leaving, a client of
    /// this process, a read or write that failed, or garbage.
    fn step(&mut self, now: Instant, out: &mut Events<'_>) -> Result<Activity, Error> {
        if let Some(client) = local::client_at(self.peer) {
            refuse(&self.refusals, &client, self.peer, &self.reason, now, out);
            return Err(Error::OwnClient);
        }
        let read = read_into(&mut self.stream, &mut self.buf)?.is_some();
        let handled = self.drain(now, out)?;
        let told = self.send_player_info(now, out)?;
        Ok(if handled || read || told {
            Activity::Active
        } else {
            Activity::Idle
        })
    }

    /// Take the turn's hints: the hand and club the monitor is told.
    fn hints(&mut self, hints: &TurnHints, _: &mut Events<'_>) -> Result<(), Error> {
        self.hints = *hints;
        Ok(())
    }

    fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// Dropping the stream closes the connection.
    fn close(self, _: &mut Events<'_>) {}
}

impl<S: Read + Write> Session<S> {
    /// A session with `peer` over `stream`, whose first bytes `buf` the
    /// driver has read already; `refusals` is the driver's record of the
    /// clients it refused, and `reason` what it tells them.
    pub(crate) fn new(
        stream: S,
        peer: SocketAddr,
        identity: DeviceIdentity,
        buf: Vec<u8>,
        refusals: Live<Refusals>,
        reason: String,
        ids: ShotIds,
    ) -> Self {
        Self {
            stream,
            peer,
            identity,
            buf,
            refusals,
            reason,
            ids,
            hints: TurnHints::default(),
            ready: None,
            ball_detected: None,
            announced: false,
            player_sent: (None, None),
            replied_at: None,
        }
    }

    /// Handle every complete message buffered. Returns whether there was
    /// one.
    fn drain(&mut self, now: Instant, out: &mut Events<'_>) -> Result<bool, Error> {
        let mut any = false;
        loop {
            match next_message(&self.buf) {
                Ok(Some((msg, used))) => {
                    let raw: Vec<u8> = self.buf.drain(..used).collect();
                    self.handle(&msg, &raw, now, out)?;
                    any = true;
                }
                Ok(None) => return Ok(any),
                Err(e) => {
                    out.alert(Severity::Warn, format!("Open Connect parse error: {e}"));
                    self.buf.clear();
                    return Ok(any);
                }
            }
        }
    }

    /// One message, which came as `raw`: what it says of the monitor, the
    /// shot it is, and the reply.
    fn handle(
        &mut self,
        msg: &GsProMessage,
        raw: &[u8],
        now: Instant,
        out: &mut Events<'_>,
    ) -> Result<(), Error> {
        self.announced = true;
        let options = &msg.shot_data_options;
        if self.ready != Some(options.launch_monitor_is_ready) {
            self.ready = Some(options.launch_monitor_is_ready);
            out.ready(options.launch_monitor_is_ready);
        }
        if self.ball_detected != Some(options.launch_monitor_ball_detected) {
            self.ball_detected = Some(options.launch_monitor_ball_detected);
            out.telemetry([(
                "ball_detected",
                options.launch_monitor_ball_detected.to_string(),
            )]);
        }
        let shot = is_shot(options);
        let reply = serde_json::to_vec(&GsProResponse::ok(if shot {
            "Shot received"
        } else {
            "Heartbeat received"
        }))
        .map_err(io::Error::from)?;
        let key = shot.then(|| self.emit_shot(msg, out));
        out.wire(
            RawDir::Rx,
            if shot { "request" } else { "heartbeat" },
            raw.to_vec(),
            flightrelay::utc_ms(),
            false,
        );
        out.wire(
            RawDir::Tx,
            "reply",
            reply.to_vec(),
            flightrelay::utc_ms(),
            false,
        );
        if let Some(key) = key {
            out.shot_finished(key);
        }
        self.stream.write_all(&reply)?;
        self.stream.flush()?;
        self.replied_at = Some(now);
        Ok(())
    }

    /// The shot messages of one shot message, up to its impact: the ball
    /// data as the launch (its spin pair completed from the other one) and
    /// the monitor's carry as its own flight result (`modeled`), and the club
    /// data and face impact when the message says it carries club data, each
    /// value as the monitor sent it. The key is for the shot's `finished`.
    fn emit_shot(&self, msg: &GsProMessage, out: &mut Events<'_>) -> ShotKey {
        let key = ShotKey {
            shot_number: msg.shot_number,
            ..self.ids.next_key()
        };
        let ball = mapper::map_launch(msg);
        out.log(
            Level::Info,
            format!(
                "shot #{}: {:.1}mph VLA={:.1} HLA={:.1} spin={:?}/{:?}",
                msg.shot_number,
                msg.ball_data.speed,
                msg.ball_data.vla,
                msg.ball_data.hla,
                ball.backspin_rpm.unwrap_or(0),
                ball.sidespin_rpm.unwrap_or(0),
            ),
        );
        out.shot_trigger(key.clone(), None);
        out.launch(key.clone(), ball);
        if let Some(flight) = mapper::map_flight(msg) {
            out.flight(key.clone(), flight);
        }
        if msg.shot_data_options.contains_club_data {
            out.club(key.clone(), mapper::map_club(msg));
            if let Some(impact) = mapper::map_face_impact(msg) {
                out.impact(key.clone(), impact);
            }
        }
        key
    }

    /// Tell the monitor the player's hand and club (a `201`, as GSPro does)
    /// once its first message has been answered, and again whenever either
    /// differs from what it was last told. Nothing is sent while the club is
    /// not known; `Handed` is included when it is. Neither object carries a
    /// terminator, so it is held back until the last 200 is at least
    /// [`PLAYER_INFO_GAP`] old. Returns whether it was sent.
    fn send_player_info(&mut self, now: Instant, out: &mut Events<'_>) -> Result<bool, Error> {
        if !self.announced
            || self
                .replied_at
                .is_some_and(|at| now.saturating_duration_since(at) < PLAYER_INFO_GAP)
        {
            return Ok(false);
        }
        let held = (self.hints.handedness, self.hints.club);
        if held == self.player_sent || held.1.is_none() {
            return Ok(false);
        }
        let player =
            serde_json::to_vec(&GsProResponse::player(held.0, held.1)).map_err(io::Error::from)?;
        out.wire(
            RawDir::Tx,
            "player",
            player.to_vec(),
            flightrelay::utc_ms(),
            false,
        );
        self.stream.write_all(&player)?;
        self.stream.flush()?;
        self.player_sent = held;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
