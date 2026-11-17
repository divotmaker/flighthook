//! The Pureplay Connect driver and its session. See the
//! [module docs](super).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use flightrelay::config::{ConfigError, DriverConfig};
use flightrelay::driver::{
    Activity, Backoff, ConnectCtx, DeviceIdentity, DriverInfo, Events, FrpDriver, FrpSession,
    Level, Live, NoAction, RawProfile, SessionEnd, Stopped,
};
use flightrelay::{
    BusActor, Club, CompletedShot, FrpBody, FrpMessage, GolferSelect, Handedness, Mode, RawDir,
    RawFrame, Removal, Severity, ShotAccumulator, ShotEvent, ShotKey, Source, StateKind, blockers,
    roles,
};

use super::codec::{self, BallData, ConnectionState, FrameReader, Message, ReadyState};
use super::shot::{OutputShot, measured_shot, output_shot, sends};
use super::{Config, mapper};

/// The name the device gives itself in Hello, before the launch monitor's.
const DEVICE_NAME: &str = "flighthook";

/// Reconnect backoff: 1 s, +1 s per failed attempt, capped at 3 s
/// (Pureplay is on loopback or the LAN).
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(3);

/// Longest a connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long Pureplay has to answer Hello.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest a launch monitor counts as processing a shot after its trigger
/// when the shot never finishes.
pub const PROCESSING_LIMIT: Duration = Duration::from_secs(10);

/// Errors from Pureplay closer together than this are one burst, alerted
/// once: a host that answers every connect with an Error and closes does so
/// on each 1-3 s retry.
pub const HOST_ERROR_BURST_GAP: Duration = Duration::from_secs(30);

/// How long a shot may take from trigger to finish: flighthook's shot
/// timeout.
const SHOT_TIMEOUT: Duration = Duration::from_secs(45);

/// The distance to the hole under which a shot is a chip: 30 yd.
const CHIP_DISTANCE_M: f64 = 27.432;

/// Why the handshake failed.
const ERROR: &str = "error";
/// The ready state last sent on this link.
const READY_STATE: &str = "ready_state";
/// Whether strict mode is on for this link.
const STRICT_MODE: &str = "strict_mode";

/// What a connect or a session failed with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// The handshake failed: the reason, for the card.
    #[error("{0}")]
    Handshake(String),
    #[error("stopped")]
    Stopped,
}

impl From<Stopped> for Error {
    fn from(_: Stopped) -> Self {
        Self::Stopped
    }
}

/// The mode for a putt, a chip or a full swing from the club in play and the
/// distance to the hole before the shot: the putter putts; a distance under
/// 30 yd chips; anything else, an unknown distance included, is a full
/// swing. A distance of 0 or less is unknown.
#[must_use]
pub fn mode_for_lie(club: Option<Club>, distance_m: Option<f64>) -> Mode {
    if club == Some(Club::Putter) {
        Mode::Putting
    } else if distance_m.is_some_and(|d| d.is_finite() && d > 0.0 && d < CHIP_DISTANCE_M) {
        Mode::Chipping
    } else {
        Mode::Full
    }
}

/// The Pureplay Connect driver: one link to Pureplay at a time, over TCP.
#[derive(Debug)]
pub struct Connect {
    config: Config,
    backoff: Backoff,
    /// When Pureplay last sent an Error, on any link.
    host_errors: Live<Option<Instant>>,
}

impl FrpDriver for Connect {
    type Config = Config;
    type Error = Error;
    type Action = NoAction;
    type Session = Session<TcpStream>;

    /// An actor with no roles: it sends nothing on the bus but its replies
    /// (`golfer.select`, `bus.alert`). Its frames are raw frames, kept by no
    /// shot.
    fn info(_: &Config) -> DriverInfo {
        DriverInfo::new("pureplay", "Pureplay Connect", Source::Virtual)
            .with_roles(&[])
            .with_raw(RawProfile::new("pureplay-tcp", 0, 0, 0, |_| false, false))
    }

    fn new(config: Config) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            backoff: Backoff::new(MIN_BACKOFF, MAX_BACKOFF),
            host_errors: Live::new(None),
        })
    }

    fn config(&self) -> &Config {
        &self.config
    }

    /// The TCP connect. The handshake runs in the session, after the host
    /// has handed it what it sees.
    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<Session<TcpStream>, Error> {
        let addr = self
            .config
            .addr()
            .map_err(|e| Error::Handshake(e.to_string()))?;
        let stream = ctx.connect_tcp(addr, CONNECT_TIMEOUT)??;
        stream.set_nodelay(true)?;
        stream.set_nonblocking(true)?;
        // The host takes what is on the bus now, so the first step's Hello
        // names what is in view.
        ctx.tick()?;
        let identity = DeviceIdentity::new(addr.to_string())
            .with_model("Pureplay")
            .with_label(addr.to_string());
        Ok(Session::new(
            stream,
            identity,
            self.host_errors.clone(),
            ctx.now(),
        ))
    }

    /// The backoff (1-3 s), with no alert: Pureplay not running is normal.
    /// A failed handshake puts its reason in `error`.
    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, Error>,
        _backoff: &mut Backoff,
        out: &mut Events<'_>,
    ) -> Option<Duration> {
        if let Error::Handshake(reason) = ended.error {
            out.telemetry([(ERROR, reason.clone())]);
        }
        Some(self.backoff.delay(ended.connected_for))
    }

    fn apply_config(&mut self, new: &Config) {
        self.config = new.clone();
    }
}

/// One link to Pureplay over transport `S`, from the TCP connect to its
/// end: the driver's [`FrpSession`], over TCP, or over any other `S` in a
/// test.
#[derive(Debug)]
pub struct Session<S = TcpStream> {
    stream: S,
    identity: DeviceIdentity,
    reader: FrameReader,
    /// When Hello went out; `None` before the first step.
    hello_at: Option<Instant>,
    /// HelloAck received.
    handshaken: bool,
    /// Strict mode is on.
    strict: bool,
    /// The ShotId given out last on this link.
    last_shot_id: u32,
    /// The ready state last sent.
    ready: Option<ReadyState>,
    /// A frame with its reserved byte set was logged.
    reserved_seen: bool,
    /// Sources whose shots strict mode kept back (logged once each).
    withheld: HashSet<String>,
    /// The selects this link sent.
    selects: u64,
    /// The launch monitors in view: their `bus.actor`, by actor id.
    monitors: BTreeMap<String, BusActor>,
    /// When each launch monitor's shot in progress was triggered.
    processing: HashMap<String, Instant>,
    shots: ShotAccumulator,
    /// Finished shots not sent yet.
    outbox: VecDeque<CompletedShot>,
    /// The time of the last step, which stamps what the session is handed.
    now: Instant,
    /// What Pureplay's SetClubInfo and SetShotContext gave.
    club: Option<Club>,
    hand: Option<Handedness>,
    distance_m: Option<f64>,
    /// When Pureplay last sent an Error, shared across links.
    host_errors: Live<Option<Instant>>,
}

impl<S: Read + Write> FrpSession for Session<S> {
    type Error = Error;
    type Action = NoAction;

    /// One pass at `now`: Hello on the first; then what Pureplay sent, the
    /// shots waiting and the ready state.
    ///
    /// # Errors
    ///
    /// Returns what ended the link: a read or write that failed, Pureplay
    /// closing it, or a handshake that failed.
    fn step(&mut self, now: Instant, out: &mut Events<'_>) -> Result<Activity, Error> {
        self.now = now;
        let Some(hello_at) = self.hello_at else {
            let hello = self.hello();
            self.send(&hello, out)?;
            self.hello_at = Some(now);
            return Ok(Activity::Active);
        };
        if !self.handshaken {
            for shot in self.outbox.drain(..) {
                out.log(
                    Level::Debug,
                    format!(
                        "not linked yet, dropping shot #{} from '{}'",
                        shot.key.shot_number, shot.from
                    ),
                );
            }
        }
        let read = self.read()?;
        self.handle_frames(out)?;
        if !self.handshaken {
            if now.saturating_duration_since(hello_at) >= HELLO_TIMEOUT {
                return Err(Error::Handshake(format!(
                    "no reply to Hello within {HELLO_TIMEOUT:?}; is Pureplay listening there?"
                )));
            }
            return Ok(if read {
                Activity::Active
            } else {
                Activity::Idle
            });
        }
        let sent = self.send_shots(out)?;
        let pushed = self.push_ready(now, out)?;
        Ok(if read || sent || pushed {
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
                self.processing.remove(&msg.from);
            }
            // A monitor out of view is not on the bus: its shot in progress
            // does not count when it comes into view.
            FrpBody::ShotTrigger(_) if self.monitors.contains_key(&msg.from) => {
                self.processing.insert(msg.from.clone(), self.now);
            }
            FrpBody::ShotFinished(_) => {
                self.processing.remove(&msg.from);
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

    /// Dropping the stream closes the link.
    fn close(self, _: &mut Events<'_>) {}
}

/// Whether a launch monitor's `bus.actor` says it is connected: it is not
/// blocked by `not_armed`.
fn connected(actor: &BusActor) -> bool {
    !actor.blockers.iter().any(|b| b == blockers::NOT_ARMED)
}

/// A launch monitor's battery, 0 to 100, if it reports one.
fn battery(actor: &BusActor) -> Option<u8> {
    actor
        .device
        .as_ref()
        .and_then(|d| d.battery_pct)
        .filter(|p| *p <= 100)
}

/// The source a launch monitor declares.
fn source(actor: &BusActor) -> Option<Source> {
    actor.device.as_ref().and_then(|d| d.source)
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

impl<S: Read + Write> Session<S> {
    /// A link over `stream` to the device `identity`, at `now`.
    pub(crate) fn new(
        stream: S,
        identity: DeviceIdentity,
        host_errors: Live<Option<Instant>>,
        now: Instant,
    ) -> Self {
        Self {
            stream,
            identity,
            reader: FrameReader::new(),
            hello_at: None,
            handshaken: false,
            strict: false,
            last_shot_id: 0,
            ready: None,
            reserved_seen: false,
            withheld: HashSet::new(),
            selects: 0,
            monitors: BTreeMap::new(),
            processing: HashMap::new(),
            shots: ShotAccumulator::new().with_timeout(SHOT_TIMEOUT),
            outbox: VecDeque::new(),
            now,
            club: None,
            hand: None,
            distance_m: None,
            host_errors,
        }
    }

    /// Write `msg`, then hand its frame to the host.
    fn send(&mut self, msg: &Message, out: &mut Events<'_>) -> Result<(), Error> {
        let frame = msg.to_frame();
        self.write(&frame)?;
        out.wire(
            RawDir::Tx,
            codec::kind_name(msg.kind()),
            frame.to_vec(),
            flightrelay::utc_ms(),
            false,
        );
        Ok(())
    }

    /// Write shot `key`'s BallData `ball`, then hand its frame to the host
    /// with the key: the record that the shot was sent.
    fn send_ball(
        &mut self,
        ball: BallData,
        key: ShotKey,
        out: &mut Events<'_>,
    ) -> Result<(), Error> {
        let msg = Message::BallData(ball);
        let frame = msg.to_frame();
        self.write(&frame)?;
        out.frp(FrpBody::ShotRaw(RawFrame {
            key: Some(key),
            label: Some(codec::kind_name(msg.kind()).to_owned()),
            ..RawFrame::new(RawDir::Tx, 0, frame)
        }));
        Ok(())
    }

    fn write(&mut self, frame: &[u8]) -> Result<(), Error> {
        self.stream.write_all(frame)?;
        self.stream.flush()?;
        Ok(())
    }

    /// Read what has arrived, without waiting. Returns whether anything
    /// did.
    fn read(&mut self) -> Result<bool, Error> {
        let mut buf = [0u8; 4096];
        match self.stream.read(&mut buf) {
            Ok(0) => Err(Error::Io(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "connection closed by Pureplay",
            ))),
            Ok(n) => {
                self.reader.push(&buf[..n]);
                Ok(true)
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Log the bytes the reader skipped since the last call, as hex.
    fn log_skipped(&mut self, out: &mut Events<'_>) {
        let dropped = self.reader.take_dropped();
        if !dropped.is_empty() {
            out.log(
                Level::Warn,
                format!(
                    "skipped {} bytes not in a frame: {}",
                    dropped.len(),
                    codec::hex(&dropped)
                ),
            );
        }
    }

    /// Handle every complete frame received.
    fn handle_frames(&mut self, out: &mut Events<'_>) -> Result<(), Error> {
        while let Some(frame) = self.reader.next_frame() {
            self.log_skipped(out);
            out.wire(
                RawDir::Rx,
                codec::kind_name(frame.kind),
                frame.bytes.to_vec(),
                flightrelay::utc_ms(),
                false,
            );
            if frame.reserved != 0 && !self.reserved_seen {
                self.reserved_seen = true;
                out.log(
                    Level::Warn,
                    format!(
                        "a frame with reserved byte 0x{:02X}; Pureplay Connect v1 sends 0, read \
                         anyway",
                        frame.reserved
                    ),
                );
            }
            match Message::decode(frame.kind, &frame.payload) {
                Ok(msg) => self.handle(msg, out)?,
                Err(e) => {
                    out.log(
                        Level::Warn,
                        format!(
                            "received {} {}: {e}",
                            codec::kind_name(frame.kind),
                            codec::hex(&frame.bytes)
                        ),
                    );
                    self.send(
                        &Message::Error {
                            code: codec::ERROR_GENERIC,
                            message: e.to_string(),
                        },
                        out,
                    )?;
                }
            }
        }
        // Bytes with no frame behind them yet.
        self.log_skipped(out);
        Ok(())
    }

    fn handle(&mut self, msg: Message, out: &mut Events<'_>) -> Result<(), Error> {
        match msg {
            Message::HelloAck {
                protocol_version, ..
            } => {
                if self.handshaken {
                    out.log(Level::Debug, "HelloAck again, ignored");
                } else if protocol_version == codec::VERSION {
                    self.handshaken = true;
                    // An empty value takes the row off the card, then the
                    // key leaves the actor.
                    out.telemetry([(ERROR, ""), (STRICT_MODE, on_off(false))]);
                    out.clear_telemetry([ERROR]);
                } else {
                    return Err(Error::Handshake(format!(
                        "Pureplay chose protocol version {protocol_version}; flighthook speaks \
                         version {}",
                        codec::VERSION
                    )));
                }
            }
            Message::Ping => {
                let heartbeat = Message::Heartbeat {
                    connection_state: self.connection_state().byte(),
                    ready_state: self.ready_state(self.now).byte(),
                    battery: self.battery().unwrap_or(codec::BATTERY_UNKNOWN),
                };
                self.send(&heartbeat, out)?;
            }
            Message::SetClubInfo { handedness, club } => {
                self.set_club_info(handedness, club, out);
                self.select(out);
                out.telemetry([
                    ("club", self.club.map(|c| c.to_string()).unwrap_or_default()),
                    (
                        "handed",
                        self.hand.map(|h| h.to_string()).unwrap_or_default(),
                    ),
                ]);
            }
            Message::SetDetect { mode } => {
                let what = match mode {
                    0 => "disarm",
                    1 => "arm",
                    _ => "unknown",
                };
                out.log(
                    Level::Info,
                    format!("SetDetect {mode} ({what}), not acted on"),
                );
            }
            Message::SetBallType { ball_type } => {
                let what = match ball_type {
                    0 => "marker",
                    1 => "markerless",
                    _ => "unknown",
                };
                out.log(
                    Level::Info,
                    format!("SetBallType {ball_type} ({what}), not acted on"),
                );
            }
            Message::SetShotContext {
                distance_to_target_m,
            } => {
                out.log(
                    Level::Info,
                    format!("SetShotContext {distance_to_target_m} m to the target"),
                );
                let before = self.mode();
                self.distance_m = Some(f64::from(distance_to_target_m));
                if self.mode() != before {
                    self.select(out);
                }
            }
            Message::SetStrictMode { strict } => {
                self.strict = strict != 0;
                self.send(
                    &Message::StrictModeAck {
                        accepted: true,
                        reason: String::new(),
                    },
                    out,
                )?;
                out.log(
                    Level::Info,
                    format!(
                        "SetStrictMode {strict}: strict mode {}",
                        on_off(self.strict)
                    ),
                );
                out.telemetry([(STRICT_MODE, on_off(self.strict))]);
            }
            Message::Error { code, message } => {
                let what = match code {
                    codec::ERROR_STRICT_MODE_REQUIRED => " (strict mode required)",
                    _ => "",
                };
                out.log(
                    Level::Warn,
                    format!("Pureplay error {code}{what}: {message}"),
                );
                let now = self.now;
                let burst = self.host_errors.update(|last| {
                    last.replace(now)
                        .is_none_or(|at| now.saturating_duration_since(at) >= HOST_ERROR_BURST_GAP)
                });
                if burst {
                    out.alert(
                        Severity::Warn,
                        format!("Pureplay error {code}{what}: {message}"),
                    );
                }
            }
            // Device-to-host messages are not commands for the device.
            other => {
                let kind = other.kind();
                out.log(
                    Level::Warn,
                    format!("unexpected {} (0x{kind:02X})", codec::kind_name(kind)),
                );
                self.send(
                    &Message::Error {
                        code: codec::ERROR_GENERIC,
                        message: format!("unexpected message type 0x{kind:02X}"),
                    },
                    out,
                )?;
            }
        }
        Ok(())
    }

    /// Take the hand and club of a SetClubInfo. A value Pureplay Connect v1
    /// does not define is logged and left out.
    fn set_club_info(&mut self, handedness: u8, club: u8, out: &mut Events<'_>) {
        match codec::Handedness::from_byte(handedness) {
            Some(codec::Handedness::Right) => self.hand = Some(Handedness::Right),
            Some(codec::Handedness::Left) => self.hand = Some(Handedness::Left),
            None => out.log(
                Level::Warn,
                format!("SetClubInfo: unknown handedness {handedness}, ignored"),
            ),
        }
        match mapper::club_from_pureplay(club) {
            Some(known) => self.club = Some(known),
            None => out.log(
                Level::Warn,
                format!("SetClubInfo: unknown club {club}, ignored"),
            ),
        }
    }

    /// The mode the session holds: from the club and the last distance,
    /// none while it holds no club.
    fn mode(&self) -> Option<Mode> {
        self.club.map(|c| mode_for_lie(Some(c), self.distance_m))
    }

    /// Send the session's complete `golfer.select`: everything it holds.
    fn select(&mut self, out: &mut Events<'_>) {
        if self.club.is_none() && self.hand.is_none() {
            return;
        }
        self.selects += 1;
        out.frp(GolferSelect {
            club: self.club.map(Some),
            handedness: self.hand,
            mode: self.mode(),
            ..GolferSelect::new(format!("select-{}", self.selects))
        });
    }

    /// The monitors the device's state is read from: every one in view; in
    /// strict mode only measured ones.
    fn subjects(&self) -> impl Iterator<Item = (&String, &BusActor)> {
        let strict = self.strict;
        self.monitors
            .iter()
            .filter(move |(_, a)| sends(strict, source(a)))
    }

    /// The ready state at `now`: Processing when any monitor is, else Ready
    /// when any is.
    fn ready_state(&self, now: Instant) -> ReadyState {
        let states: Vec<ReadyState> = self
            .subjects()
            .map(|(id, a)| {
                if !connected(a) {
                    ReadyState::NotReady
                } else if self
                    .processing
                    .get(id)
                    .is_some_and(|at| now.saturating_duration_since(*at) < PROCESSING_LIMIT)
                {
                    ReadyState::Processing
                } else if a.is_clear() {
                    ReadyState::Ready
                } else {
                    ReadyState::NotReady
                }
            })
            .collect();
        [ReadyState::Processing, ReadyState::Ready]
            .into_iter()
            .find(|s| states.contains(s))
            .unwrap_or(ReadyState::NotReady)
    }

    /// The connection state: the best of every monitor's.
    fn connection_state(&self) -> ConnectionState {
        let mut best = ConnectionState::Disconnected;
        for (_, actor) in self.subjects() {
            if connected(actor) {
                return ConnectionState::Connected;
            }
            best = ConnectionState::Connecting;
        }
        best
    }

    /// The monitor that stands for the device in Hello and the battery: the
    /// only one in view.
    fn named(&self) -> Option<&BusActor> {
        let mut monitors = self.monitors.values();
        match (monitors.next(), monitors.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }

    /// That monitor's battery while it is connected.
    fn battery(&self) -> Option<u8> {
        self.named().filter(|a| connected(a)).and_then(battery)
    }

    /// The Hello for this link.
    fn hello(&self) -> Message {
        let named = self.named();
        Message::Hello {
            protocol_version: codec::VERSION,
            device_caps: if named.and_then(battery).is_some() {
                codec::CAP_BATTERY_POWERED
            } else {
                0
            },
            device_name: match named.map(|a| a.name.trim()) {
                Some(name) if !name.is_empty() => format!("{DEVICE_NAME} ({name})"),
                _ => DEVICE_NAME.to_owned(),
            },
            firmware: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Push the ready state when it differs from the one last sent. Returns
    /// whether it did.
    fn push_ready(&mut self, now: Instant, out: &mut Events<'_>) -> Result<bool, Error> {
        let state = self.ready_state(now);
        if self.ready == Some(state) {
            return Ok(false);
        }
        self.send(&Message::ReadyState(state.byte()), out)?;
        self.ready = Some(state);
        out.telemetry([(READY_STATE, state.label())]);
        Ok(true)
    }

    /// Log a shot strict mode does not send: once per source at info, then
    /// at debug.
    fn withhold(&mut self, shot: &OutputShot, out: &mut Events<'_>) {
        if self.withheld.insert(shot.actor.clone()) {
            out.log(
                Level::Info,
                format!(
                    "strict mode, shots from '{}' are not sent: its source is not measured",
                    shot.actor
                ),
            );
        } else {
            out.log(
                Level::Debug,
                format!(
                    "strict mode, shot #{} from '{}' not sent",
                    shot.shot_number, shot.actor
                ),
            );
        }
    }

    /// Send every shot waiting, oldest first. Returns whether one went out.
    fn send_shots(&mut self, out: &mut Events<'_>) -> Result<bool, Error> {
        let mut sent = false;
        while let Some(done) = self.outbox.pop_front() {
            let shot = if self.strict {
                measured_shot(&done, &done.from)
            } else {
                output_shot(&done, &done.from)
            };
            if !sends(self.strict, shot.source) {
                self.withhold(&shot, out);
                continue;
            }
            let shot_id = self.last_shot_id + 1;
            let Some(frames) = mapper::map_shot(&shot, shot_id) else {
                out.log(
                    Level::Debug,
                    format!(
                        "shot #{} from '{}' has no ball data, not sent",
                        shot.shot_number, shot.actor
                    ),
                );
                continue;
            };
            self.last_shot_id = shot_id;
            out.log(
                Level::Info,
                format!(
                    "-> ShotId {shot_id} (shot #{} from '{}'){}{}",
                    shot.shot_number,
                    shot.actor,
                    if frames.club.is_some() {
                        " with club data"
                    } else {
                        ""
                    },
                    if self.strict { ", strict" } else { "" },
                ),
            );
            self.send_ball(frames.ball, done.key.clone(), out)?;
            if let Some(club) = frames.club {
                self.send(&Message::ClubData(club), out)?;
            }
            sent = true;
        }
        Ok(sent)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
