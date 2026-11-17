//! The Open Connect client driver and its session. See the
//! [module docs](super).

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use flightrelay::config::{ConfigError, DriverConfig};
use flightrelay::driver::{
    Activity, Backoff, ConnectCtx, DeviceIdentity, DriverInfo, Events, FrpDriver, FrpSession,
    Level, Live, NoAction, RawProfile, SessionEnd, Stopped,
};
use flightrelay::{
    BusActor, Club, CompletedShot, FrpBody, FrpMessage, GolferSelect, Handedness, RawDir, RawFrame,
    Removal, ShotAccumulator, ShotEvent, Source, StateKind, roles,
};

use super::api::GsProMessage;
use super::shot::output_shot;
use super::{Config, mapper, proto};
use crate::local::{self, Registration};

/// Reconnect backoff: 1 s, +1 s per failed attempt, capped at 3 s, back to
/// 1 s after a connection that lasted. The simulator is on loopback or the
/// LAN, so retrying a refused connect costs nothing, and a short cap means
/// the client is back within a few seconds of the simulator starting.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(3);

/// Longest a connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a connection must stay up before the connect succeeds: an Open
/// Connect server refuses its own client as soon as it accepts it, well
/// within this, so a refused connection never shows as connected.
pub(crate) const REFUSAL_VERDICT: Duration = Duration::from_secs(1);

/// How often the session sends a heartbeat. The first goes out 1 s after
/// the session starts.
pub(crate) const HEARTBEAT_EVERY: Duration = Duration::from_secs(10);

/// When the first heartbeat goes out, after the session starts.
const FIRST_HEARTBEAT: Duration = Duration::from_secs(1);

/// How long a shot may take from trigger to finish: flighthook's shot
/// timeout.
const SHOT_TIMEOUT: Duration = Duration::from_secs(45);

/// How long nothing goes out after a shot: some simulators read only one
/// message per read, so the next message waits until they have taken the
/// shot and answered it.
pub(crate) const SHOT_GAP: Duration = Duration::from_millis(250);

/// How long a shot may go unanswered before the card says the simulator is
/// not reading shots.
pub(crate) const REPLY_WITHIN: Duration = Duration::from_secs(1);

/// The card's `error` while a shot has gone unanswered.
pub(crate) const NOT_READING: &str =
    "the simulator isn't reading shots; restart its Open Connect window";

/// The longest a closing connection waits for the simulator to end its side.
const CLOSE_WAIT: Duration = Duration::from_millis(250);

/// The readiness last sent on this connection.
const MONITOR_READY: &str = "monitor_ready";
/// The simulator's last response on this connection: code and message.
const RESPONSE: &str = "response";
/// A telemetry value with nothing to report.
const NONE: &str = "none";

/// What a connect or a session failed with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// This process's own Open Connect server refused the connection; the
    /// reason it gave.
    #[error("{0}")]
    Refused(String),
    #[error("stopped")]
    Stopped,
}

impl From<Stopped> for Error {
    fn from(_: Stopped) -> Self {
        Self::Stopped
    }
}

/// A connection that can end gracefully: what the session needs of its
/// stream besides reading and writing.
pub trait Close: Read + Write {
    /// End the connection with a close, not a reset: stop writing, then read
    /// whatever the simulator still sends until it ends its side, for at
    /// most a short while.
    fn close(&mut self);
}

impl Close for TcpStream {
    fn close(&mut self) {
        let _ = self.shutdown(Shutdown::Write);
        let end = Instant::now() + CLOSE_WAIT;
        let mut buf = [0u8; 4096];
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if self.set_read_timeout(Some(left)).is_err()
                || self.set_nonblocking(false).is_err()
                || !matches!(self.read(&mut buf), Ok(n) if n > 0)
            {
                break;
            }
        }
    }
}

/// The Open Connect client driver: one connection to the simulator at a
/// time, over TCP.
#[derive(Debug)]
pub struct Client {
    config: Config,
    backoff: Backoff,
    /// The `ShotNumber` sent last, 0 before the first; kept across
    /// connections.
    last_number: Live<u32>,
}

impl FrpDriver for Client {
    type Config = Config;
    type Error = Error;
    type Action = NoAction;
    type Session = Session<TcpStream>;

    /// An actor with no roles: it sends nothing on the bus but its replies
    /// (`golfer.select`, `bus.alert`). Its frames are text raw frames, kept
    /// by no shot.
    fn info(_: &Config) -> DriverInfo {
        DriverInfo::new("openconnect_client", "Open Connect", Source::Virtual)
            .with_roles(&[])
            .with_telemetry(&[
                (MONITOR_READY, "unknown"),
                (RESPONSE, NONE),
                ("club", NONE),
                ("handed", NONE),
            ])
            .with_raw(RawProfile::new("openconnect-tcp", 0, 0, 0, |_| false, true))
    }

    fn new(config: Config) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            backoff: Backoff::new(MIN_BACKOFF, MAX_BACKOFF),
            last_number: Live::new(0),
        })
    }

    fn config(&self) -> &Config {
        &self.config
    }

    /// The TCP connect, the connection registered as this process's own,
    /// then 1 s for this process's own server to refuse it.
    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<Session<TcpStream>, Error> {
        ctx.out.phase("connecting");
        let addr = self
            .config
            .addr()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let stream = ctx.connect_tcp(addr, CONNECT_TIMEOUT)??;
        // Registered before anything is sent, so a server reading our data
        // already knows the connection is ours.
        let registration = local::clients().register(stream.local_addr()?, &self.config.actor);
        stream.set_nodelay(true)?;
        stream.set_nonblocking(true)?;
        ctx.wait(REFUSAL_VERDICT)?;
        if let Some(reason) = registration.refusal() {
            return Err(Error::Refused(reason));
        }
        let identity = DeviceIdentity::new(addr.to_string())
            .with_model("Open Connect")
            .with_label(addr.to_string());
        ctx.out.phase("connected");
        Ok(Session {
            _registration: Some(registration),
            ..Session::new(stream, identity, self.last_number.clone(), ctx.now())
        })
    }

    /// The backoff (1-3 s), with no alert: a simulator not running is
    /// normal, and a refusal shows as the card's error.
    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, Error>,
        _backoff: &mut Backoff,
        _out: &mut Events<'_>,
    ) -> Option<Duration> {
        Some(self.backoff.delay(ended.connected_for))
    }

    fn apply_config(&mut self, new: &Config) {
        self.config = new.clone();
    }
}

/// One connection to the simulator over transport `S`, from the connect to
/// its end: the driver's [`FrpSession`], over TCP, or over any other `S`
/// in a test.
#[derive(Debug)]
pub struct Session<S = TcpStream> {
    stream: S,
    identity: DeviceIdentity,
    /// The connection's entry as this process's own; removed with the
    /// session.
    _registration: Option<Registration<'static>>,
    /// When the last heartbeat went out.
    last_heartbeat: Instant,
    /// The readiness last sent.
    ready: bool,
    /// The `ShotNumber` sent last, shared with the driver.
    last_number: Live<u32>,
    /// The selects this connection sent.
    selects: u64,
    /// What the simulator's player information gave.
    club: Option<Club>,
    hand: Option<Handedness>,
    /// The launch monitors in view: their `bus.actor`, by actor id.
    monitors: BTreeMap<String, BusActor>,
    shots: ShotAccumulator,
    /// Finished shots not sent yet.
    outbox: VecDeque<CompletedShot>,
    /// The time of the last step, which stamps what the session is handed.
    now: Instant,
    /// Nothing is written before this time.
    hold_until: Instant,
    /// When the oldest shot the simulator has not answered was sent.
    unanswered: Option<Instant>,
    /// The card shows that the simulator is not reading shots.
    not_reading: bool,
}

impl<S: Close> FrpSession for Session<S> {
    type Error = Error;
    type Action = NoAction;

    /// One pass at `now`: what the simulator sent, then at most one
    /// message, and none until [`SHOT_GAP`] after a shot: the oldest shot
    /// waiting, else the readiness when it changed, else the heartbeat when
    /// one is due.
    ///
    /// # Errors
    ///
    /// Returns what ended the connection: a read or write that failed, or
    /// the simulator closing it.
    fn step(&mut self, now: Instant, out: &mut Events<'_>) -> Result<Activity, Error> {
        self.now = now;
        let read = self.read(out)?;
        self.check_replies(out);
        let wrote = now >= self.hold_until && self.write_next(out)?;
        Ok(if read || wrote {
            Activity::Active
        } else {
            Activity::Idle
        })
    }

    /// Take one message of the session's view of the bus (see the
    /// [module docs](super)). Reads no clock: it is stamped with the last
    /// step's time.
    fn bus(&mut self, msg: &FrpMessage, _: &mut Events<'_>) -> Result<(), Error> {
        match &msg.body {
            FrpBody::BusActor(actor) if actor.has_role(roles::LAUNCH_MONITOR) => {
                self.monitors.insert(msg.from.clone(), actor.clone());
            }
            FrpBody::Removed(Removal {
                kind: StateKind::BusActor,
                ..
            }) => {
                self.monitors.remove(&msg.from);
            }
            _ => {}
        }
        if let Some(ShotEvent::Finished(shot)) = self.shots.feed_at(msg, self.now) {
            self.outbox.push_back(shot);
        }
        Ok(())
    }

    fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// Ends the connection with a close, not a reset ([`Close::close`]).
    fn close(mut self, _: &mut Events<'_>) {
        self.stream.close();
    }
}

impl<S: Close> Session<S> {
    /// A connection over `stream` to the simulator `identity`, at `now`,
    /// numbering shots after `last_number`.
    pub(crate) fn new(
        stream: S,
        identity: DeviceIdentity,
        last_number: Live<u32>,
        now: Instant,
    ) -> Self {
        Self {
            stream,
            identity,
            _registration: None,
            last_heartbeat: now
                .checked_sub(HEARTBEAT_EVERY - FIRST_HEARTBEAT)
                .unwrap_or(now),
            ready: false,
            last_number,
            selects: 0,
            club: None,
            hand: None,
            monitors: BTreeMap::new(),
            shots: ShotAccumulator::new().with_timeout(SHOT_TIMEOUT),
            outbox: VecDeque::new(),
            now,
            hold_until: now,
            unanswered: None,
            not_reading: false,
        }
    }

    /// Whether the simulator is told a launch monitor is ready: one in view
    /// has no blockers.
    fn ready(&self) -> bool {
        self.monitors.values().any(BusActor::is_clear)
    }

    /// Write `msg`, then hand it to the host as a raw frame labeled `label`,
    /// with shot key `key` when it carries a shot.
    fn send(
        &mut self,
        msg: &GsProMessage,
        label: &str,
        key: Option<flightrelay::ShotKey>,
        out: &mut Events<'_>,
    ) -> Result<(), Error> {
        let json =
            serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.stream.write_all(&json)?;
        self.stream.flush()?;
        match key {
            Some(key) => out.frp(FrpBody::ShotRaw(RawFrame {
                key: Some(key),
                label: Some(label.to_owned()),
                ..RawFrame::new(RawDir::Tx, 0, json)
            })),
            None => out.wire(RawDir::Tx, label, json, flightrelay::utc_ms(), false),
        }
        Ok(())
    }

    /// Write the next message that is due, if any: the oldest shot waiting
    /// (holding every write for [`SHOT_GAP`]), else the readiness when it
    /// changed, else the heartbeat when one is due. Returns whether one went
    /// out.
    fn write_next(&mut self, out: &mut Events<'_>) -> Result<bool, Error> {
        if self.send_shot(out)? {
            self.hold_until = self.now + SHOT_GAP;
            return Ok(true);
        }
        let ready = self.ready();
        if ready != self.ready {
            let msg = GsProMessage::readiness(ready, ready);
            self.send(&msg, "readiness", None, out)?;
            self.set_ready(ready, out);
        } else if self.now.saturating_duration_since(self.last_heartbeat) >= HEARTBEAT_EVERY {
            let msg = GsProMessage::heartbeat_with_readiness(ready, ready);
            self.send(&msg, "heartbeat", None, out)?;
            self.last_heartbeat = self.now;
            self.set_ready(ready, out);
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    /// The readiness sent is `ready`.
    fn set_ready(&mut self, ready: bool, out: &mut Events<'_>) {
        self.ready = ready;
        out.telemetry([(MONITOR_READY, ready.to_string())]);
    }

    /// The card says so, once, when a shot has gone [`REPLY_WITHIN`]
    /// without a reply.
    fn check_replies(&mut self, out: &mut Events<'_>) {
        if !self.not_reading
            && self
                .unanswered
                .is_some_and(|sent| self.now.saturating_duration_since(sent) >= REPLY_WITHIN)
        {
            self.not_reading = true;
            out.log(Level::Warn, NOT_READING.to_owned());
            out.telemetry([("error", NOT_READING.to_owned())]);
        }
    }

    /// Read what has arrived, without waiting, and handle it. Returns
    /// whether anything did.
    fn read(&mut self, out: &mut Events<'_>) -> Result<bool, Error> {
        let mut buf = [0u8; 4096];
        let n = match self.stream.read(&mut buf) {
            Ok(0) => {
                return Err(Error::Io(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "connection closed by the simulator",
                )));
            }
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(false);
            }
            Err(e) => return Err(Error::Io(e)),
        };
        out.wire(
            RawDir::Rx,
            "response",
            buf[..n].to_vec(),
            flightrelay::utc_ms(),
            false,
        );
        let heard = proto::handle_response(&buf[..n], out);
        if heard.ok {
            self.unanswered = None;
            if std::mem::take(&mut self.not_reading) {
                out.telemetry([("error", NONE.to_owned())]);
            }
        }
        if let Some((hand, club)) = heard.info {
            if hand.is_some() {
                self.hand = hand;
            }
            if club.is_some() {
                self.club = club;
            }
            self.select(out);
        }
        if let Some((code, message)) = heard.last {
            let text = format!("{code} {}", message.trim()).trim_end().to_owned();
            out.telemetry([(RESPONSE, text)]);
        }
        if heard.player || heard.info.is_some() {
            out.telemetry([
                (
                    "club",
                    self.club.map_or_else(|| NONE.into(), |c| c.to_string()),
                ),
                (
                    "handed",
                    self.hand.map_or_else(|| NONE.into(), |h| h.to_string()),
                ),
            ]);
        }
        Ok(true)
    }

    /// Send the session's complete `golfer.select`: the club and hand it
    /// holds.
    fn select(&mut self, out: &mut Events<'_>) {
        if self.club.is_none() && self.hand.is_none() {
            return;
        }
        self.selects += 1;
        out.frp(GolferSelect {
            club: self.club.map(Some),
            handedness: self.hand,
            ..GolferSelect::new(format!("select-{}", self.selects))
        });
    }

    /// Send the oldest shot waiting (a shot with no data is dropped, and the
    /// next one is looked at). Returns whether one went out.
    ///
    /// The shot's `shot_number` becomes its `ShotNumber`: its own number,
    /// or one more than the last one sent when that is higher.
    fn send_shot(&mut self, out: &mut Events<'_>) -> Result<bool, Error> {
        while let Some(done) = self.outbox.pop_front() {
            let mut shot = output_shot(&done, &done.from);
            if !shot.has_data() {
                out.log(
                    Level::Info,
                    format!(
                        "shot #{} from '{}' has no data (no read), not sent",
                        shot.shot_number, shot.actor
                    ),
                );
                continue;
            }
            let last = self.last_number.update(|n| *n);
            let number = shot.shot_number.max(last.saturating_add(1));
            if number != shot.shot_number {
                out.log(
                    Level::Debug,
                    format!(
                        "shot #{} from '{}' sent as ShotNumber {number} (last sent {last})",
                        shot.shot_number, shot.actor
                    ),
                );
            }
            shot.shot_number = number;
            let msg = mapper::map_shot(&shot);
            log_outbound(&msg, out);
            self.send(&msg, "shot", Some(done.key.clone()), out)?;
            self.last_number.set(number);
            self.unanswered.get_or_insert(self.now);
            return Ok(true);
        }
        Ok(false)
    }
}

/// The log lines of a shot sent.
fn log_outbound(msg: &GsProMessage, out: &mut Events<'_>) {
    let b = &msg.ball_data;
    out.log(
        Level::Info,
        format!(
            "-> shot #{}: {:.1}mph {:.1}vla {:.1}hla {:.0}yd carry {:.0}spin",
            msg.shot_number,
            b.speed,
            b.vla,
            b.hla,
            b.carry_distance.unwrap_or(0.0),
            b.total_spin,
        ),
    );
    if msg.shot_data_options.contains_club_data {
        let c = &msg.club_data;
        out.log(
            Level::Info,
            format!(
                "->   club: {:.1}mph aoa={:.1} face={:.1} path={:.1} loft={:.1}",
                c.speed, c.angle_of_attack, c.face_to_target, c.path, c.loft,
            ),
        );
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
