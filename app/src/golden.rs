//! Wire goldens: what flighthook writes to its outputs for one fixed script.
//!
//! Records what flighthook writes to its outputs for one fixed script:
//! every JSON object written to a fake GSPro, every frame written to a mock
//! Pureplay Connect host (normal and strict), and everything the Open
//! Connect server writes to a connected monitor (its `200` replies and its
//! `201` player information).
//!
//! The real actors run on one bus with the real system actor: the
//! FlightScope cards (Mevo+ and Xi) on ironsight's `FakeDevice` over TCP,
//! the R10 and Omni cards on tenover's / allsquare's real drivers over
//! scripted transports, the Open Connect server on a local port with a
//! fake monitor, and the output card under test against its fake peer.
//!
//! Run (each test writes its files into `$FH_GOLDEN_OUT`, default
//! `target/goldens`):
//!
//! ```text
//! FH_GOLDEN_OUT=/some/dir cargo test -p flighthook-app --no-default-features \
//!     golden -- --ignored --test-threads=1 --nocapture
//! diff -r app/goldens /some/dir
//! ```
//!
//! `app/goldens` holds the expected files.
//!
//! Normalizations (the only ones):
//! - GSPro heartbeats are not recorded one by one (their count and position
//!   depend on 10 s timers and on how a device's ready flicker falls into
//!   the bridge's 50 ms passes). At the end of each step the last heartbeat
//!   written so far is recorded, when it differs from the one recorded last.
//! - Pureplay `ReadyState` pushes: the same rule (the last one sent by the
//!   end of each step, when it differs from the one recorded last).
//! - Pureplay `Hello`: the firmware string (the crate version) is replaced
//!   by `VERSION` and the frame re-encoded (CRC recomputed).
//! - At the end of each step the mock Pureplay host sends one `Ping`; the
//!   `Heartbeat` answer is recorded at the end of the step.
//!
//! Everything else is recorded byte for byte, in order.

#![allow(clippy::too_many_lines, clippy::missing_panics_doc)]

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use flighthook::{
    ActorStatus, Club, ClubExt, Handedness, Mode, R10Config, R10Section, SimMode, SquareConfig,
    SquareSection,
};
use flightrelay::config::ConfigError;
use flightrelay::driver::{
    Backoff, ConnectCtx, DeviceDriver, DeviceIdentity, DeviceSession, DriverInfo, Events, NoAction,
    SessionEnd, TurnHints,
};
use flightrelay::{FrpBody, FrpMessage, GolferSelect};
use ironsight::testing::{FakeDevice, Shot, fixtures};
use tokio::sync::broadcast;

use crate::actors::driver::{DriverActor, Hooks};
use crate::actors::provenance::ShotSource;
use crate::actors::pureplay::codec::{self, FrameReader, Message};
use crate::actors::r10::R10Hooks;
use crate::actors::scripted::Handoff;
use crate::actors::square::SquareHooks;
use crate::bus::{BusMsg, LocalEvent};
use crate::state::SystemState;

/// How long each step waits after its own events, for every output to
/// pass over them (bridges poll every 50 to 100 ms).
const SETTLE: Duration = Duration::from_millis(700);
/// Longest wait for any one thing.
const WAIT: Duration = Duration::from_secs(30);

// ===========================================================================
// FlightScope devices: ironsight's FakeDevice served over TCP
// ===========================================================================

/// A scripted FlightScope device on a local TCP port, one connection at a
/// time; the device lives on the serving thread.
struct FsDevice {
    addr: SocketAddr,
    todo: mpsc::Sender<Shot>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FsDevice {
    fn serve(xi: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (todo, todo_rx) = mpsc::channel::<Shot>();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            listener.set_nonblocking(true).expect("nonblocking");
            while !stopped.load(Ordering::SeqCst) {
                let mut socket = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                socket.set_nonblocking(true).expect("nonblocking");
                let mut device = if xi {
                    FakeDevice::xi()
                } else {
                    FakeDevice::mevo_plus()
                };
                let mut buf = [0u8; 4096];
                while !stopped.load(Ordering::SeqCst) {
                    while let Ok(shot) = todo_rx.try_recv() {
                        device.push_shot(&shot);
                    }
                    match socket.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => device.write_all(&buf[..n]).expect("device write"),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                        Err(_) => break,
                    }
                    match device.read(&mut buf) {
                        Ok(n) => {
                            if socket.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            }
        });
        Self {
            addr,
            todo,
            stop,
            thread: Some(thread),
        }
    }

    fn shot(&self, shot: Shot) {
        self.todo.send(shot).expect("the device thread runs");
    }
}

impl Drop for FsDevice {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Write `value` as a big-endian signed 24-bit integer at `at`.
fn put_int24(payload: &mut [u8], at: usize, value: i32) {
    #[allow(clippy::cast_sign_loss)]
    let raw = (value as u32) & 0x00FF_FFFF;
    payload[at] = (raw >> 16) as u8;
    payload[at + 1] = (raw >> 8) as u8;
    payload[at + 2] = raw as u8;
}

/// A launch-only full result: the Xi fixture's full result with speed
/// 7.019 m/s (15.7 mph), launch 22.8°, azimuth 0.856° (wire: left), and no
/// spin, carry, flight time, apex, landing, roll or total. Its club values
/// stay the fixture's.
fn launch_only_d4() -> Vec<u8> {
    let mut d4 = fixtures::XI_SHOT_D4.to_vec();
    put_int24(&mut d4, 16, 7019);
    put_int24(&mut d4, 19, 856);
    put_int24(&mut d4, 22, 22800);
    for at in [
        25, 28, 31, 34, 37, 40, 43, 46, 49, 52, 55, 58, 61, 64, 67, 70, 73, 76, 79, 82,
    ] {
        put_int24(&mut d4, at, 0);
    }
    d4
}

/// A launch-only chip as the device sends it: the full result and the club
/// result, no early result.
fn launch_only_chip() -> Shot {
    use ironsight::protocol::{TYPE_CLUB_RESULT, TYPE_FLIGHT_RESULT};
    Shot::new(
        91,
        ironsight::BusAddr::Avr,
        [
            (TYPE_FLIGHT_RESULT, launch_only_d4()),
            (TYPE_CLUB_RESULT, fixtures::XI_SHOT_ED.to_vec()),
        ],
        fixtures::XI_SHOT_ED.to_vec(),
    )
}

// ===========================================================================
// R10: tenover's driver over a scripted transport
// ===========================================================================

/// MultiLink handle the scripted R10 grants.
const R10_HANDLE: u8 = 0x05;
/// The scripted R10's BLE address (synthetic).
const R10_ADDRESS: &str = "F5:D1:00:00:00:01";
/// Empty reads between two batches.
const R10_GAP_READS: u32 = 4;

#[derive(Default)]
struct R10Wire {
    batches: VecDeque<VecDeque<Vec<u8>>>,
    gap: u32,
}

/// A scripted R10: batches of chunks the harness queues, a few empty reads
/// between batches; quiet (not gone) when nothing is queued.
#[derive(Clone, Default)]
struct R10Link(Arc<Mutex<R10Wire>>);

impl R10Link {
    fn queue(&self, batches: Vec<Vec<Vec<u8>>>) {
        let mut w = self.0.lock().unwrap();
        w.batches.extend(batches.into_iter().map(Into::into));
    }
}

impl tenover::Transport for R10Link {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let mut w = self.0.lock().unwrap();
        loop {
            let Some(batch) = w.batches.front_mut() else {
                return Err(ErrorKind::WouldBlock.into());
            };
            if let Some(chunk) = batch.pop_front() {
                buf[..chunk.len()].copy_from_slice(&chunk);
                return Ok(chunk.len());
            }
            if w.gap < R10_GAP_READS {
                w.gap += 1;
                return Err(ErrorKind::WouldBlock.into());
            }
            w.gap = 0;
            w.batches.pop_front();
        }
    }

    fn write(&mut self, _data: &[u8]) -> Result<(), std::io::Error> {
        Ok(())
    }

    fn write_register(&mut self, _data: &[u8]) -> Result<(), std::io::Error> {
        Ok(())
    }
}

fn r10_msg(frame: &[u8]) -> Vec<Vec<u8>> {
    tenover::testing::chunks(R10_HANDLE, frame, 20)
}

fn r10_envelope(envelope: &[u8]) -> Vec<Vec<u8>> {
    r10_msg(&tenover::testing::protobuf_frame(envelope))
}

/// Handshake, subscribe, wake (the tee distance goes out), armed.
fn r10_session_start() -> Vec<Vec<Vec<u8>>> {
    use tenover::testing as t;
    let handshake = [
        vec![t::register_response(R10_HANDLE)],
        r10_msg(&t::device_info_frame()),
        r10_msg(&t::configuration_frame()),
        r10_msg(&t::ack_frame(tenover::gfdi::MSG_CONFIGURATION, 0, &[])),
    ]
    .concat();
    vec![
        handshake,
        r10_envelope(&t::subscribe_result_envelope(true)),
        r10_envelope(&t::wake_result_envelope(0)),
        r10_envelope(&t::environment_result_envelope(true)),
        r10_envelope(&t::state_envelope(2)),
    ]
}

/// Recording, processing, a shot, armed again. The device drops a shot id
/// it reported recently, so each shot has its own: the first is tenover's
/// golden full shot (device shot 42), later ones tenover's regular test shot
/// (4200 rpm, axis -3°) with device shot 100 + `n`.
fn r10_full_shot(n: u32) -> Vec<Vec<Vec<u8>>> {
    use tenover::testing as t;
    let shot = if n == 1 {
        R10_SHOT_FULL.to_vec()
    } else {
        t::shot_envelope(100 + n, 4200.0, -3.0)
    };
    vec![
        r10_envelope(&t::state_envelope(3)),
        r10_envelope(&t::state_envelope(4)),
        r10_envelope(&shot),
        r10_envelope(&t::state_envelope(2)),
    ]
}

/// tenover's golden regular shot message (its `wire_golden` fixture): every
/// ball, club and swing value.
const R10_SHOT_FULL: &[u8] = &[
    0xf2, 0x01, 0x53, 0x1a, 0x51, 0x08, 0x08, 0xca, 0x3e, 0x4c, 0x12, 0x4a, 0x08, 0x2a, 0x10, 0x01,
    0x1a, 0x1d, 0x0d, 0x00, 0x00, 0x48, 0x41, 0x15, 0x9a, 0x99, 0x99, 0xbf, 0x1d, 0x00, 0x00, 0x86,
    0x42, 0x25, 0x00, 0x00, 0xa0, 0x40, 0x2d, 0x00, 0x00, 0x2f, 0x45, 0x30, 0x00, 0x38, 0x01, 0x22,
    0x14, 0x0d, 0x00, 0x00, 0x30, 0x42, 0x15, 0x00, 0x00, 0xc0, 0x3f, 0x1d, 0x00, 0x00, 0x00, 0xc0,
    0x25, 0x00, 0x00, 0x60, 0xc0, 0x2a, 0x0f, 0x08, 0xe8, 0x07, 0x10, 0xdc, 0x0b, 0x18, 0x88, 0x0e,
    0x20, 0x98, 0x11, 0x28, 0xc4, 0x13,
];

/// Scripted R10 links waiting for a driver, by the card's range.
static R10_LINKS: Handoff<String, R10Link> = Handoff::new();

fn r10_key(config: &R10Config) -> String {
    config.range.map(|d| format!("{d:?}")).unwrap_or_default()
}

/// tenover's R10 with its BLE scan replaced by the scripted link.
struct GoldenR10 {
    inner: tenover::driver::R10,
    link: Option<R10Link>,
}

struct GoldenR10Session(tenover::driver::R10Session<R10Link>);

impl DeviceDriver for GoldenR10 {
    type Config = R10Config;
    type Error = tenover::driver::Error;
    type Action = NoAction;
    type Session = GoldenR10Session;
    const STOP_WAIT: Duration = tenover::driver::R10::STOP_WAIT;

    fn info(config: &R10Config) -> DriverInfo {
        tenover::driver::R10::info(config)
    }

    fn new(config: R10Config) -> Result<Self, ConfigError> {
        let link = R10_LINKS.take(&r10_key(&config));
        Ok(Self {
            inner: tenover::driver::R10::new(config)?,
            link,
        })
    }

    fn config(&self) -> &R10Config {
        self.inner.config()
    }

    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<GoldenR10Session, Self::Error> {
        ctx.tick()?;
        ctx.out.phase("discovering");
        match self.link.take() {
            Some(link) => Ok(GoldenR10Session(self.inner.connect_with(
                ctx,
                link,
                R10_ADDRESS,
            )?)),
            None => loop {
                ctx.tick()?;
                std::thread::sleep(Duration::from_millis(20));
            },
        }
    }

    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, Self::Error>,
        backoff: &mut Backoff,
        out: &mut Events<'_>,
    ) -> Option<Duration> {
        self.inner.retry_delay(ended, backoff, out)
    }

    fn apply_config(&mut self, new: &R10Config) {
        self.inner.apply_config(new);
    }
}

impl DeviceSession for GoldenR10Session {
    type Driver = GoldenR10;

    fn poll(
        &mut self,
        timeout: Duration,
        out: &mut Events<'_>,
    ) -> Result<(), tenover::driver::Error> {
        self.0.poll(timeout, out)
    }

    fn identity(&self) -> &DeviceIdentity {
        self.0.identity()
    }

    fn close(self, out: &mut Events<'_>) -> Result<(), tenover::driver::Error> {
        self.0.close(out)
    }
}

impl Hooks<GoldenR10> for R10Hooks {
    type Section = R10Section;
    const SOURCE: ShotSource = ShotSource::R10;
}

// ===========================================================================
// Omni: allsquare's driver over a scripted link
// ===========================================================================

const OMNI_NAME: &str = "SquareGolf(GLD1)";
const OMNI_ADDRESS: &str = "02:00:00:00:00:02";

#[derive(Default)]
struct OmniWire {
    notes: VecDeque<Vec<u8>>,
    /// The second copy of the last notification (the device sends each
    /// twice).
    repeat: Option<Vec<u8>>,
    written: Vec<Vec<u8>>,
}

/// A scripted Omni: the notifications the harness queues, each sent twice;
/// quiet when none is queued. Commands written are kept.
#[derive(Clone, Default)]
struct OmniLink(Arc<Mutex<OmniWire>>);

impl OmniLink {
    fn queue(&self, notes: Vec<Vec<u8>>) {
        self.0.lock().unwrap().notes.extend(notes);
    }
}

impl allsquare::Transport for OmniLink {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut w = self.0.lock().unwrap();
        if let Some(note) = w.repeat.take() {
            buf[..note.len()].copy_from_slice(&note);
            return Ok(note.len());
        }
        match w.notes.pop_front() {
            Some(note) => {
                buf[..note.len()].copy_from_slice(&note);
                w.repeat = Some(note.clone());
                Ok(note.len())
            }
            None => Err(ErrorKind::WouldBlock.into()),
        }
    }

    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        let mut w = self.0.lock().unwrap();
        w.written.push(data.to_vec());
        // The device answers every heartbeat (it ends the link after three
        // unanswered intervals); the answer goes behind anything queued.
        if data.get(1) == Some(&0x83) {
            w.notes.push_back(omni_ack(allsquare::DeviceState::Ready));
        }
        Ok(())
    }

    fn read_characteristic(&mut self, id: u128) -> std::io::Result<Vec<u8>> {
        use allsquare::protocol::uuid;
        let s = match id {
            uuid::FW_VERSION => r#"{"launcher": "1.0.1", "mmi": "1.4.0", "lm": "1.5.8"}"#,
            uuid::HW_VERSION => "3.1",
            uuid::GAP_NAME => "SGO300A",
            uuid::DEVICE_ID => "SQ-GOLDEN-0001",
            _ => return Err(ErrorKind::NotFound.into()),
        };
        Ok(s.as_bytes().to_vec())
    }
}

impl allsquare::driver::BleLink for OmniLink {
    fn name(&self) -> &str {
        OMNI_NAME
    }
    fn address(&self) -> &str {
        OMNI_ADDRESS
    }
    fn release(&mut self) -> allsquare::Result<()> {
        Ok(())
    }
}

static OMNI_LINKS: Handoff<String, OmniLink> = Handoff::new();

struct GoldenOmni {
    inner: allsquare::driver::Omni,
    link: Option<OmniLink>,
}

struct GoldenOmniSession(allsquare::driver::OmniSession);

impl DeviceDriver for GoldenOmni {
    type Config = SquareConfig;
    type Error = allsquare::driver::Error;
    type Action = NoAction;
    type Session = GoldenOmniSession;
    const STOP_WAIT: Duration = allsquare::driver::Omni::STOP_WAIT;

    fn info(config: &SquareConfig) -> DriverInfo {
        allsquare::driver::Omni::info(config)
    }

    fn new(config: SquareConfig) -> Result<Self, ConfigError> {
        let link = OMNI_LINKS.take(&config.address.clone().unwrap_or_default());
        Ok(Self {
            inner: allsquare::driver::Omni::new(config)?,
            link,
        })
    }

    fn config(&self) -> &SquareConfig {
        self.inner.config()
    }

    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<GoldenOmniSession, Self::Error> {
        ctx.tick()?;
        ctx.out.phase("discovering");
        match self.link.take() {
            Some(link) => Ok(GoldenOmniSession(self.inner.connect_with(ctx, link)?)),
            None => loop {
                ctx.tick()?;
                std::thread::sleep(Duration::from_millis(20));
            },
        }
    }

    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, Self::Error>,
        backoff: &mut Backoff,
        out: &mut Events<'_>,
    ) -> Option<Duration> {
        self.inner.retry_delay(ended, backoff, out)
    }

    fn apply_config(&mut self, new: &SquareConfig) {
        self.inner.apply_config(new);
    }
}

impl DeviceSession for GoldenOmniSession {
    type Driver = GoldenOmni;

    fn poll(
        &mut self,
        timeout: Duration,
        out: &mut Events<'_>,
    ) -> Result<(), allsquare::driver::Error> {
        self.0.poll(timeout, out)
    }

    fn hints(&mut self, hints: &TurnHints) -> Result<(), allsquare::driver::Error> {
        self.0.hints(hints)
    }

    fn identity(&self) -> &DeviceIdentity {
        self.0.identity()
    }

    fn close(self, out: &mut Events<'_>) -> Result<(), allsquare::driver::Error> {
        self.0.close(out)
    }
}

impl Hooks<GoldenOmni> for SquareHooks {
    type Section = SquareSection;
    const SOURCE: ShotSource = ShotSource::Square;
}

fn omni_ack(state: allsquare::DeviceState) -> Vec<u8> {
    allsquare::testing::heartbeat_ack(state).to_vec()
}

/// A full 7-iron swing with every club value (allsquare's full-swing pin).
fn omni_full_swing() -> Vec<Vec<u8>> {
    use allsquare::{BallMetrics, ClubMetrics, DeviceState, testing};
    let ball = BallMetrics {
        speed: 60.0,
        launch_angle: 14.0,
        direction: -1.2,
        total_spin: 3059,
        spin_axis: -11.31,
        back_spin: 3000,
        side_spin: -600,
        ..testing::ball_metrics()
    };
    let club = ClubMetrics {
        mask: 0xff,
        path: Some(-2.4),
        face_angle: Some(-0.8),
        attack_angle: Some(-1.6),
        dynamic_loft: Some(17.9),
        impact_horizontal: Some(3.5),
        impact_vertical: Some(-4.0),
        club_speed: Some(41.4),
        smash_factor: Some(1.45),
    };
    vec![
        testing::ball_notification(&ball).to_vec(),
        testing::club_notification(&club).to_vec(),
        omni_ack(DeviceState::Shot),
        omni_ack(DeviceState::Done),
        omni_ack(DeviceState::Ready),
    ]
}

/// A putt without club data.
fn omni_putt() -> Vec<Vec<u8>> {
    use allsquare::{BallMetrics, ClubMetrics, DeviceState, testing};
    let ball = BallMetrics {
        speed: 2.5,
        launch_angle: 1.2,
        direction: -0.8,
        total_spin: 0,
        spin_axis: 0.0,
        back_spin: 0,
        side_spin: 0,
        ..testing::ball_metrics()
    };
    vec![
        testing::ball_notification(&ball).to_vec(),
        testing::club_notification(&ClubMetrics::default()).to_vec(),
        omni_ack(DeviceState::Shot),
        omni_ack(DeviceState::Done),
        omni_ack(DeviceState::Ready),
    ]
}

// ===========================================================================
// The Open Connect monitor: a fake launch monitor on the server
// ===========================================================================

/// Splits a byte stream into the JSON objects in it, raw text each; bytes
/// that are not a JSON object come out as `!garbage <hex>`.
#[derive(Default)]
struct JsonSplitter {
    buf: Vec<u8>,
}

impl JsonSplitter {
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let mut it =
                serde_json::Deserializer::from_slice(&self.buf).into_iter::<serde_json::Value>();
            match it.next() {
                Some(Ok(_)) => {
                    let end = it.byte_offset();
                    out.push(String::from_utf8_lossy(&self.buf[..end]).into_owned());
                    self.buf.drain(..end);
                }
                Some(Err(e)) if e.is_eof() => break,
                None => break,
                Some(Err(_)) => {
                    out.push(format!("!garbage {}", codec::hex(&self.buf)));
                    self.buf.clear();
                    break;
                }
            }
        }
        out
    }
}

/// A TCP peer whose reader thread collects what the other side writes.
struct Peer {
    stream: TcpStream,
    got: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
}

impl Peer {
    fn new(stream: TcpStream) -> Self {
        stream
            .set_read_timeout(Some(Duration::from_millis(10)))
            .expect("timeout");
        let got = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut reader = stream.try_clone().expect("clone");
        let (g, s) = (Arc::clone(&got), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while !s.load(Ordering::SeqCst) {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => g.lock().unwrap().extend_from_slice(&buf[..n]),
                    Err(_) => {}
                }
            }
        });
        Self { stream, got, stop }
    }

    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.got.lock().unwrap())
    }

    fn write(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).expect("peer write");
        self.stream.flush().expect("flush");
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// The fake launch monitor connected to the Open Connect server.
struct OcMonitor {
    peer: Peer,
    json: JsonSplitter,
    count: u32,
}

const OC_HEARTBEAT: &str = r#"{"DeviceID":"Golden LM","Units":"Yards","ShotNumber":0,"APIversion":"1","ShotDataOptions":{"ContainsBallData":false,"ContainsClubData":false,"LaunchMonitorIsReady":true,"LaunchMonitorBallDetected":true,"IsHeartBeat":true}}"#;

impl OcMonitor {
    fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).expect("monitor connects");
        Self {
            peer: Peer::new(stream),
            json: JsonSplitter::default(),
            count: 0,
        }
    }

    fn send(&mut self, text: &str) {
        self.peer.write(text.as_bytes());
    }

    /// A full shot with ball and club data.
    fn full_shot(&mut self) {
        self.count += 1;
        let text = format!(
            r#"{{"DeviceID":"Golden LM","Units":"Yards","ShotNumber":{},"APIversion":"1","BallData":{{"Speed":131.4,"SpinAxis":-3.5,"TotalSpin":5120.0,"HLA":-1.8,"VLA":16.2,"CarryDistance":171.3}},"ClubData":{{"Speed":91.2,"AngleOfAttack":-3.1,"FaceToTarget":0.6,"Lie":0.0,"Loft":20.4,"Path":1.9,"SpeedAtImpact":0.0,"VerticalFaceImpact":2.0,"HorizontalFaceImpact":-4.5,"ClosureRate":0.0}},"ShotDataOptions":{{"ContainsBallData":true,"ContainsClubData":true,"LaunchMonitorIsReady":true,"LaunchMonitorBallDetected":true,"IsHeartBeat":false}}}}"#,
            self.count
        );
        self.send(&text);
    }

    /// A putt: ball data only.
    fn putt(&mut self) {
        self.count += 1;
        let text = format!(
            r#"{{"DeviceID":"Golden LM","Units":"Yards","ShotNumber":{},"APIversion":"1","BallData":{{"Speed":5.6,"SpinAxis":0.0,"TotalSpin":0.0,"HLA":0.7,"VLA":1.1}},"ShotDataOptions":{{"ContainsBallData":true,"ContainsClubData":false,"LaunchMonitorIsReady":true,"LaunchMonitorBallDetected":true,"IsHeartBeat":false}}}}"#,
            self.count
        );
        self.send(&text);
    }

    /// The objects the server wrote since the last call.
    fn take(&mut self) -> Vec<String> {
        let bytes = self.peer.take();
        self.json.push(&bytes)
    }
}

// ===========================================================================
// The outputs' fake peers
// ===========================================================================

/// The simulator side of one output card.
trait Sim {
    /// The simulator tells flighthook the player's hand and club.
    fn player(&mut self, hand: Handedness, club: Club);
    /// Before each shot: what the simulator tells flighthook of the lie.
    /// Returns whether it sent anything.
    fn before_shot(&mut self) -> bool {
        false
    }
    /// The step's recorded lines (golden) and their decoded form (for
    /// reading).
    fn step_lines(&mut self) -> (Vec<String>, Vec<String>);
}

/// A fake GSPro: accepts the Open Connect client and records every object.
struct FakeGsPro {
    peer: Peer,
    json: JsonSplitter,
    last_heartbeat: Option<String>,
    recorded_heartbeat: Option<String>,
}

impl FakeGsPro {
    fn accept(listener: &TcpListener) -> Self {
        listener.set_nonblocking(false).expect("blocking");
        let (stream, _) = listener.accept().expect("the client connects");
        Self {
            peer: Peer::new(stream),
            json: JsonSplitter::default(),
            last_heartbeat: None,
            recorded_heartbeat: None,
        }
    }
}

fn hand_code(hand: Handedness) -> &'static str {
    match hand {
        Handedness::Right => "RH",
        Handedness::Left => "LH",
    }
}

impl Sim for FakeGsPro {
    fn player(&mut self, hand: Handedness, club: Club) {
        let text = format!(
            r#"{{"Code":201,"Message":"GSPro Player Information","Player":{{"Handed":"{}","Club":"{}"}}}}"#,
            hand_code(hand),
            club.gspro_code()
        );
        self.peer.write(text.as_bytes());
    }

    fn step_lines(&mut self) -> (Vec<String>, Vec<String>) {
        let bytes = self.peer.take();
        let mut lines = Vec::new();
        for obj in self.json.push(&bytes) {
            let heartbeat = serde_json::from_str::<serde_json::Value>(&obj)
                .ok()
                .and_then(|v| v.pointer("/ShotDataOptions/IsHeartBeat").cloned())
                .is_some_and(|v| v == serde_json::Value::Bool(true));
            if heartbeat {
                self.last_heartbeat = Some(obj);
            } else {
                lines.push(format!("sent {obj}"));
            }
        }
        if self.last_heartbeat != self.recorded_heartbeat {
            self.recorded_heartbeat.clone_from(&self.last_heartbeat);
            if let Some(hb) = &self.last_heartbeat {
                lines.push(format!("heartbeat {hb}"));
            }
        }
        (lines.clone(), lines)
    }
}

/// A mock Pureplay Connect host: answers Hello with HelloAck, pings at the
/// end of every step, records every frame.
struct MockPureplay {
    peer: Peer,
    /// The club the host set last.
    club: Option<Club>,
    reader: FrameReader,
    /// Frames read but not yet recorded.
    pending: VecDeque<codec::Frame>,
    last_ready: Option<codec::Frame>,
    recorded_ready: Option<Vec<u8>>,
}

impl MockPureplay {
    fn accept(listener: &TcpListener) -> Self {
        listener.set_nonblocking(false).expect("blocking");
        let (stream, _) = listener.accept().expect("the output connects");
        let mut host = Self {
            peer: Peer::new(stream),
            club: None,
            reader: FrameReader::new(),
            pending: VecDeque::new(),
            last_ready: None,
            recorded_ready: None,
        };
        let hello = host.wait_for(codec::kind::HELLO);
        host.pending.push_back(hello);
        host.send(&Message::HelloAck {
            protocol_version: codec::VERSION,
            host_caps: 0,
        });
        host
    }

    fn send(&mut self, msg: &Message) {
        self.peer.write(&msg.to_frame());
    }

    fn pull(&mut self) {
        let bytes = self.peer.take();
        self.reader.push(&bytes);
        while let Some(f) = self.reader.next_frame() {
            let dropped = self.reader.take_dropped();
            assert!(dropped.is_empty(), "clean framing");
            self.pending.push_back(f);
        }
    }

    /// Read until a frame of `kind` arrives; earlier frames stay pending.
    fn wait_for(&mut self, kind: u8) -> codec::Frame {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pull();
            if let Some(at) = self.pending.iter().position(|f| f.kind == kind) {
                return self.pending.remove(at).expect("frame");
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for 0x{kind:02X}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Strict mode on or off; the ack is recorded with the step.
    fn strict(&mut self, on: bool) {
        self.send(&Message::SetStrictMode {
            strict: u8::from(on),
        });
        let ack = self.wait_for(codec::kind::STRICT_MODE_ACK);
        self.pending.push_back(ack);
    }
}

fn decoded(frame: &codec::Frame) -> String {
    match Message::decode(frame.kind, &frame.payload) {
        Ok(m) => format!("{m:?}"),
        Err(e) => format!("undecodable: {e}"),
    }
}

/// The frame as golden text: kind name and hex. A Hello's firmware is
/// replaced by `VERSION` (re-encoded).
fn frame_line(frame: &codec::Frame) -> (String, String) {
    let name = codec::kind_name(frame.kind);
    if let Ok(Message::Hello {
        protocol_version,
        device_caps,
        device_name,
        ..
    }) = Message::decode(frame.kind, &frame.payload)
    {
        let msg = Message::Hello {
            protocol_version,
            device_caps,
            device_name,
            firmware: "VERSION".into(),
        };
        let bytes = msg.to_frame();
        return (
            format!("{name} {}", codec::hex(&bytes)),
            format!("{name} {msg:?}"),
        );
    }
    (
        format!("{name} {}", codec::hex(&frame.bytes)),
        format!("{name} {}", decoded(frame)),
    )
}

impl Sim for MockPureplay {
    fn player(&mut self, hand: Handedness, club: Club) {
        let handedness = match hand {
            Handedness::Right => 0,
            Handedness::Left => 1,
        };
        self.club = Some(club);
        let club = (0u8..=30)
            .find(|n| crate::actors::pureplay::mapper::club_from_pureplay(*n) == Some(club))
            .expect("a Pureplay club");
        self.send(&Message::SetClubInfo { handedness, club });
    }

    /// The distance to the target before each shot, which gives the mode
    /// the club in play had in the script: 150 m with a driver or iron, 20 m
    /// with the sand wedge, any with the putter.
    fn before_shot(&mut self) -> bool {
        let distance_to_target_m = match self.club {
            Some(Club::SandWedge) => 20.0,
            _ => 150.0,
        };
        self.send(&Message::SetShotContext {
            distance_to_target_m,
        });
        true
    }

    fn step_lines(&mut self) -> (Vec<String>, Vec<String>) {
        self.send(&Message::Ping);
        let heartbeat = self.wait_for(codec::kind::HEARTBEAT);
        let mut lines = Vec::new();
        let mut readable = Vec::new();
        while let Some(f) = self.pending.pop_front() {
            if f.kind == codec::kind::READY_STATE {
                self.last_ready = Some(f);
                continue;
            }
            let (l, r) = frame_line(&f);
            lines.push(l);
            readable.push(r);
        }
        if let Some(ready) = &self.last_ready
            && self.recorded_ready.as_deref() != Some(&ready.bytes[..])
        {
            self.recorded_ready = Some(ready.bytes.clone());
            let (l, r) = frame_line(ready);
            lines.push(format!("last {l}"));
            readable.push(format!("last {r}"));
        }
        let (l, r) = frame_line(&heartbeat);
        lines.push(format!("ping-reply {l}"));
        readable.push(format!("ping-reply {r}"));
        (lines, readable)
    }
}

// ===========================================================================
// The rig
// ===========================================================================

/// Which output the run records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
    GsPro,
    Pureplay,
    PureplayStrict,
}

/// Everything seen on the bus (by card), and each actor's latest `ready`.
#[derive(Default)]
struct Seen {
    messages: Vec<Heard>,
    ready: HashMap<String, bool>,
}

/// One bus message as the recorder keeps it: the card that sent it (the
/// actor id for one without a card) and what it was.
#[derive(Debug)]
struct Heard {
    actor: String,
    event: Event,
    /// The `ready` it reports: a launch monitor's `bus.actor` rows, or
    /// false for a card status other than Connected.
    ready: Option<bool>,
}

#[derive(Debug)]
enum Event {
    Status {
        status: ActorStatus,
        telemetry: HashMap<String, String>,
    },
    ShotFinished,
    /// The session's `golfer.turn`.
    Turn,
    Other,
}

impl Heard {
    fn of(root: &SystemState, bus: &BusMsg) -> Self {
        let (actor, event, ready) = match bus {
            BusMsg::Local {
                card,
                event: LocalEvent::CardStatus { status, telemetry },
            } => (
                card.clone(),
                Event::Status {
                    status: *status,
                    telemetry: telemetry.clone(),
                },
                (*status != ActorStatus::Connected).then_some(false),
            ),
            BusMsg::Local { card, .. } => (card.clone(), Event::Other, None),
            BusMsg::Frp { msg, .. } => {
                let card = crate::actors::web::stored::card_of(root, &msg.from);
                match &msg.body {
                    FrpBody::BusActor(a)
                        if a.has_role(flightrelay::types::roles::LAUNCH_MONITOR) =>
                    {
                        let ready = flighthook::bus_actor_rows(a)
                            .get("ready")
                            .map(|r| r == "true");
                        (card, Event::Other, ready)
                    }
                    FrpBody::ShotFinished(_) => (card, Event::ShotFinished, None),
                    FrpBody::GolferTurn(_) if msg.from == root.flighthook_id() => {
                        (card, Event::Turn, None)
                    }
                    _ => (card, Event::Other, None),
                }
            }
        };
        Self {
            actor,
            event,
            ready,
        }
    }
}

struct Rig {
    state: Arc<SystemState>,
    tx: broadcast::Sender<BusMsg>,
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    mevo: FsDevice,
    xi: FsDevice,
    r10: R10Link,
    omni: OmniLink,
    monitor: OcMonitor,
    sim: Box<dyn Sim>,
    chips: AtomicU32,
    r10_shots: u32,
    /// Recorded: the output's golden lines, their readable form, and the
    /// Open Connect server's.
    out: Vec<String>,
    readable: Vec<String>,
    oc: Vec<String>,
    step: u32,
    dir: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for id in self.state.actor_ids() {
            self.state.stop_actor(&id);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn section<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> T {
    serde_json::from_value(json).expect("section")
}

impl Rig {
    fn start(output: Output) -> Self {
        let run = format!("{output:?}").to_lowercase();
        let dir = std::env::temp_dir().join(format!(
            "flighthook-golden-{run}-{}",
            crate::state::config::generate_id()
        ));
        let (state, writer) = SystemState::new(dir.join("config.toml"));
        let mevo = FsDevice::serve(false);
        let xi = FsDevice::serve(true);
        let gspro = TcpListener::bind("127.0.0.1:0").expect("bind");
        let gspro_addr = gspro.local_addr().expect("addr");
        let range = match output {
            Output::GsPro => "7ft",
            Output::Pureplay => "8ft",
            Output::PureplayStrict => "9ft",
        };
        let omni_key = format!("golden-{run}");
        let routing = serde_json::json!({
            "chipping_monitor": "mevo.1",
            "putting_monitor": "openconnect_server.0",
        });
        state.system.update(|c| {
            c.simulation.auto_chip = true;
            c.mevo.insert(
                "0".into(),
                section(serde_json::json!({
                    "name": "Mevo+", "address": mevo.addr.to_string(), "ball_type": 1,
                    "tee_height": "1.5in", "range": "8ft", "surface_height": "0in",
                    "track_pct": 80.0, "use_estimated": false, "camera_mode": "standard",
                    "environment": "limited_flight"
                })),
            );
            c.mevo.insert(
                "1".into(),
                section(serde_json::json!({
                    "name": "Xi", "address": xi.addr.to_string(), "use_estimated": true,
                    "spin_source": "auto", "range": "8ft"
                })),
            );
            c.r10.insert(
                "0".into(),
                section(serde_json::json!({"name": "Garmin R10", "range": range})),
            );
            c.square.insert(
                "0".into(),
                section(serde_json::json!({"name": "Square Golf Omni", "address": omni_key})),
            );
            c.openconnect_server.insert(
                "0".into(),
                section(serde_json::json!({"name": "Bay LM", "bind": "127.0.0.1:0"})),
            );
            let mut out = routing.clone();
            out["address"] = serde_json::Value::String(gspro_addr.to_string());
            match output {
                Output::GsPro => {
                    out["name"] = "GSPro".into();
                    c.openconnect_client.insert("0".into(), section(out));
                }
                Output::Pureplay | Output::PureplayStrict => {
                    out["name"] = "Pureplay Connect".into();
                    c.pureplay.insert("0".into(), section(out));
                }
            }
        });
        // The script's routing names card keys, as a config file written by
        // 5021d9e does; loading such a file converts them to actor ids once.
        state.system.update(|c| {
            c.route_by_actor_id();
        });
        let state = Arc::new(state);
        let (tx, mut rx) = broadcast::channel::<BusMsg>(8192);
        let seen = Arc::new(Mutex::new(Seen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (seen, stop, root) = (Arc::clone(&seen), Arc::clone(&stop), Arc::clone(&state));
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match rx.try_recv() {
                        Ok(bus) => {
                            let mut s = seen.lock().unwrap();
                            let m = Heard::of(&root, &bus);
                            if let Some(r) = m.ready {
                                s.ready.insert(m.actor.clone(), r);
                            }
                            s.messages.push(m);
                        }
                        Err(broadcast::error::TryRecvError::Empty) => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(broadcast::error::TryRecvError::Lagged(n)) => {
                            panic!("the recorder lagged {n} messages")
                        }
                        Err(broadcast::error::TryRecvError::Closed) => return,
                    }
                }
            });
        }
        crate::actors::system::start_for_test(writer, &state, &tx, Arc::default());

        // Launch monitors: the FlightScope cards and the Open Connect server
        // as the config builds them, the R10 and Omni on scripted links.
        let r10 = R10Link::default();
        r10.queue(r10_session_start());
        let omni = OmniLink::default();
        omni.queue(vec![
            omni_ack(allsquare::DeviceState::Detect),
            vec![0x91, 0x4b, 0x01],
            omni_ack(allsquare::DeviceState::Ready),
        ]);
        let snap = state.system.snapshot();
        let r10_section = snap.r10.get("0").cloned().expect("r10");
        let square_section = snap.square.get("0").cloned().expect("square");
        // The handoff keys are what the drivers compute from their config.
        R10_LINKS.put(r10_key(&r10_section.driver), r10.clone());
        OMNI_LINKS.put(
            square_section.driver.address.clone().unwrap_or_default(),
            omni.clone(),
        );
        for resolved in crate::actors::resolve_actors(&snap) {
            if resolved.id.starts_with("mevo.") || resolved.id.starts_with("openconnect_server.") {
                crate::actors::start_actor(resolved.id, resolved.actor, &state, &tx);
            }
        }
        crate::actors::start_actor(
            "r10.0".into(),
            Box::new(DriverActor::<GoldenR10, R10Hooks>::new(r10_section)),
            &state,
            &tx,
        );
        crate::actors::start_actor(
            "square.0".into(),
            Box::new(DriverActor::<GoldenOmni, SquareHooks>::new(square_section)),
            &state,
            &tx,
        );

        let mut rig_seen = RigSeen(Arc::clone(&seen));
        for id in ["mevo.0", "mevo.1", "r10.0", "square.0"] {
            rig_seen.ready(id);
        }
        let oc_addr = rig_seen.until(|m| {
            if m.actor != "openconnect_server.0" {
                return None;
            }
            match &m.event {
                Event::Status { telemetry, .. } => telemetry
                    .get("bind")
                    .and_then(|b| b.parse::<SocketAddr>().ok())
                    .filter(|a| a.port() != 0),
                _ => None,
            }
        });
        let mut monitor = OcMonitor::connect(oc_addr);
        monitor.send(OC_HEARTBEAT);

        // The output, last.
        for resolved in crate::actors::resolve_actors(&state.system.snapshot()) {
            if resolved.id.starts_with("openconnect_client.")
                || resolved.id.starts_with("pureplay.")
            {
                crate::actors::start_actor(resolved.id, resolved.actor, &state, &tx);
            }
        }
        let sim: Box<dyn Sim> = match output {
            Output::GsPro => Box::new(FakeGsPro::accept(&gspro)),
            Output::Pureplay | Output::PureplayStrict => {
                let mut host = MockPureplay::accept(&gspro);
                if output == Output::PureplayStrict {
                    host.strict(true);
                }
                Box::new(host)
            }
        };
        let out_id = match output {
            Output::GsPro => "openconnect_client.0",
            Output::Pureplay | Output::PureplayStrict => "pureplay.0",
        };
        rig_seen.until(|m| {
            (m.actor == out_id
                && matches!(
                    m.event,
                    Event::Status {
                        status: ActorStatus::Connected,
                        ..
                    }
                ))
            .then_some(())
        });
        let mut rig = Self {
            state,
            tx,
            seen,
            stop,
            mevo,
            xi,
            r10,
            omni,
            monitor,
            sim,
            chips: AtomicU32::new(0),
            r10_shots: 0,
            out: Vec::new(),
            readable: Vec::new(),
            oc: Vec::new(),
            step: 0,
            dir,
        };
        // GSPro's first heartbeat goes out about 1 s after the connect.
        std::thread::sleep(Duration::from_millis(1500));
        rig.record("connected: every device ready, the output connected");
        rig
    }

    fn seen(&self) -> RigSeen {
        RigSeen(Arc::clone(&self.seen))
    }

    fn mark(&self) -> usize {
        self.seen.lock().unwrap().messages.len()
    }

    /// Close the step: settle, then record what every peer got.
    fn record(&mut self, label: &str) {
        std::thread::sleep(SETTLE);
        self.step += 1;
        let head = format!("== {} {label}", self.step);
        eprintln!("{head}");
        let (lines, readable) = self.sim.step_lines();
        self.out.push(head.clone());
        self.out.extend(lines);
        self.readable.push(head.clone());
        self.readable.extend(readable);
        self.oc.push(head);
        self.oc.extend(
            self.monitor
                .take()
                .into_iter()
                .map(|o| format!("wrote {o}")),
        );
    }

    fn note(&mut self, text: &str) {
        self.out.push(format!("# {text}"));
        self.readable.push(format!("# {text}"));
        self.oc.push(format!("# {text}"));
    }

    /// Wait for `actor`'s ShotFinished after `from`, then until it is ready.
    fn finished(&self, actor: &str, from: usize) {
        self.seen().until_from(from, |m| {
            (m.actor == actor && matches!(m.event, Event::ShotFinished)).then_some(())
        });
        self.seen().ready(actor);
    }

    /// The simulator's lie before a shot, and the mode change it gives.
    fn before_shot(&mut self) {
        let from = self.mark();
        if self.sim.before_shot() {
            std::thread::sleep(Duration::from_millis(300));
            let changed = self.seen().0.lock().unwrap().messages[from..]
                .iter()
                .any(|m| m.actor == "system" && matches!(m.event, Event::Turn));
            if changed {
                self.mode_applied(from);
            }
        }
    }

    fn mevo_shot(&mut self, xi: bool, shot: Shot, label: &str) {
        self.before_shot();
        let from = self.mark();
        let actor = if xi { "mevo.1" } else { "mevo.0" };
        if xi {
            self.xi.shot(shot);
        } else {
            self.mevo.shot(shot);
        }
        self.finished(actor, from);
        self.record(label);
    }

    fn r10_shot(&mut self, label: &str) {
        self.before_shot();
        let from = self.mark();
        self.r10_shots += 1;
        self.r10.queue(r10_full_shot(self.r10_shots));
        self.finished("r10.0", from);
        self.record(label);
    }

    fn omni_shot(&mut self, notes: Vec<Vec<u8>>, label: &str) {
        self.before_shot();
        let from = self.mark();
        self.omni.queue(notes);
        self.finished("square.0", from);
        self.record(label);
    }

    fn oc_shot(&mut self, putt: bool, label: &str) {
        self.before_shot();
        let from = self.mark();
        if putt {
            self.monitor.putt();
        } else {
            self.monitor.full_shot();
        }
        self.seen().until_from(from, |m| {
            (m.actor == "openconnect_server.0" && matches!(m.event, Event::ShotFinished))
                .then_some(())
        });
        self.record(label);
    }

    fn auto_chip(&mut self, yards: u32, label: &str) {
        self.before_shot();
        let from = self.mark();
        match crate::autochip::send(&self.state, &self.tx, yards, &self.chips) {
            Ok(_) => {
                self.seen().until_from(from, |m| {
                    (m.actor == flighthook::AUTO_CHIP_ACTOR
                        && matches!(m.event, Event::ShotFinished))
                    .then_some(())
                });
            }
            Err(e) => self.note(&format!("auto-chip not sent: {e}")),
        }
        self.record(label);
    }

    /// Wait for the session's turn after `from`, then for every device to
    /// be ready again (a mode change re-arms).
    fn mode_applied(&self, from: usize) {
        self.seen().until_from(from, |m| {
            (m.actor == "system" && matches!(m.event, Event::Turn)).then_some(())
        });
        std::thread::sleep(Duration::from_millis(300));
        for id in ["mevo.0", "mevo.1", "r10.0", "square.0"] {
            self.seen().ready(id);
        }
    }

    /// The simulator sends the player's hand and club. The turn changes
    /// unless it already holds them (a repeat), so a missing turn after a
    /// short wait is no failure.
    fn sim_player(&mut self, hand: Handedness, club: Club, label: &str) {
        let from = self.mark();
        self.sim.player(hand, club);
        let deadline = Instant::now() + Duration::from_secs(2);
        let changed = loop {
            let changed = self.seen().0.lock().unwrap().messages[from..]
                .iter()
                .any(|m| m.actor == "system" && matches!(m.event, Event::Turn));
            if changed || Instant::now() >= deadline {
                break changed;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if changed {
            self.mode_applied(from);
        }
        self.record(label);
    }

    /// A header pick: a `golfer.select` from the flighthook actor.
    fn header_select(&self, change: impl FnOnce(&mut GolferSelect)) {
        let mut select = GolferSelect::new(format!("golden-{}", self.step));
        change(&mut select);
        let _ = self.tx.send(BusMsg::frp(FrpMessage::new(
            self.state.flighthook_id(),
            select,
        )));
    }

    /// The header picks a club (no hand).
    fn header_club(&mut self, club: Club, label: &str) {
        let from = self.mark();
        self.header_select(|s| s.club = Some(Some(club)));
        self.mode_applied(from);
        self.record(label);
    }

    /// The header picks a mode.
    fn header_mode(&mut self, mode: Mode, label: &str) {
        self.header_select(|s| s.mode = Some(mode));
        std::thread::sleep(Duration::from_millis(300));
        for id in ["mevo.0", "mevo.1", "r10.0", "square.0"] {
            self.seen().ready(id);
        }
        self.record(label);
    }

    /// `[simulation] mode`, read live per shot.
    fn sim_mode(&mut self, mode: SimMode) {
        self.state.system.update(|c| c.simulation.mode = mode);
        self.note(&format!("[simulation] mode = {mode:?}"));
    }

    fn write(&self, name: &str) {
        let dir = std::env::var_os("FH_GOLDEN_OUT")
            .map_or_else(|| PathBuf::from("target/goldens"), PathBuf::from);
        std::fs::create_dir_all(&dir).expect("output dir");
        let text = |file: String, lines: &[String]| {
            let mut body = lines.join("\n");
            body.push('\n');
            std::fs::write(dir.join(&file), body).expect("write");
            println!("wrote {}", dir.join(file).display());
        };
        text(format!("{name}.golden.txt"), &self.out);
        if self.readable != self.out {
            text(format!("{name}.decoded.txt"), &self.readable);
        }
        text(format!("{name}-openconnect-server.golden.txt"), &self.oc);
    }
}

/// Waits on what the recorder saw.
struct RigSeen(Arc<Mutex<Seen>>);

impl RigSeen {
    fn until<T>(&mut self, want: impl FnMut(&Heard) -> Option<T>) -> T {
        self.until_from(0, want)
    }

    fn until_from<T>(&self, from: usize, mut want: impl FnMut(&Heard) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        let mut at = from;
        loop {
            {
                let s = self.0.lock().unwrap();
                while at < s.messages.len() {
                    if let Some(t) = want(&s.messages[at]) {
                        return t;
                    }
                    at += 1;
                }
            }
            if Instant::now() >= deadline {
                let s = self.0.lock().unwrap();
                for m in s.messages.iter().skip(from.saturating_sub(5)) {
                    eprintln!("  {} {:?}", m.actor, m.event);
                }
                panic!("timed out after message {at}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Until `actor`'s latest `ready` is true, and stays so for 100 ms.
    fn ready(&self, actor: &str) {
        let deadline = Instant::now() + WAIT;
        let mut since: Option<Instant> = None;
        loop {
            let ready = self.0.lock().unwrap().ready.get(actor) == Some(&true);
            match (ready, since) {
                (true, Some(t)) if t.elapsed() >= Duration::from_millis(100) => return,
                (true, None) => since = Some(Instant::now()),
                (false, _) => since = None,
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {actor} ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

// ===========================================================================
// The script
// ===========================================================================

/// The fixed script every output runs. Routing on the output card: Full =
/// Any, Chipping = `mevo.1` (the Xi), Putting = `openconnect_server.0`.
/// Default clubs: GW/SW/LW chip, PT putts.
fn play(rig: &mut Rig) {
    use Handedness::{Left, Right};
    // Header club before any hand: the Open Connect server's first 201 has
    // a club and no hand.
    rig.header_club(Club::Iron7, "header picks 7I (no hand set yet)");
    rig.sim_player(Right, Club::Driver, "simulator: RH, DR (Full)");
    rig.mevo_shot(false, Shot::xi_full_swing(), "Mevo+ full swing (Full, Any)");
    rig.r10_shot("R10 full shot (Full, Any)");
    rig.omni_shot(omni_full_swing(), "Omni full swing (Full, Any)");
    rig.mevo_shot(
        true,
        Shot::xi_full_swing(),
        "Xi full swing (Full, Any; spin source auto = E8 rule)",
    );
    rig.oc_shot(false, "Open Connect monitor full shot (Full, Any)");
    rig.mevo_shot(
        false,
        launch_only_chip(),
        "Mevo+ launch-only chip, sim on (Full, Any)",
    );
    rig.sim_mode(SimMode::Off);
    rig.mevo_shot(
        false,
        launch_only_chip(),
        "Mevo+ launch-only chip, sim off (Full, Any)",
    );
    rig.sim_mode(SimMode::Force);
    rig.mevo_shot(
        false,
        Shot::xi_full_swing(),
        "Mevo+ full swing, sim force (Full, Any)",
    );
    rig.r10_shot("R10 full shot, sim force (Full, Any)");
    rig.sim_mode(SimMode::On);
    rig.auto_chip(3, "Auto-chip 3 yd (Full)");

    rig.sim_player(Right, Club::SandWedge, "simulator: RH, SW (Chipping)");
    rig.mevo_shot(
        true,
        launch_only_chip(),
        "Xi launch-only chip, sim on (Chipping, routed Xi)",
    );
    rig.sim_mode(SimMode::Off);
    rig.mevo_shot(
        true,
        launch_only_chip(),
        "Xi launch-only chip, sim off (Chipping, routed Xi)",
    );
    rig.sim_mode(SimMode::Force);
    rig.mevo_shot(
        true,
        launch_only_chip(),
        "Xi launch-only chip, sim force (Chipping, routed Xi)",
    );
    rig.sim_mode(SimMode::On);
    rig.mevo_shot(
        true,
        Shot::xi_chip(),
        "Xi early-only chip, estimate on (Chipping, routed Xi)",
    );
    rig.mevo_shot(
        true,
        Shot::xi_full_swing(),
        "Xi full swing (Chipping, routed Xi; spin source auto = D4)",
    );
    rig.mevo_shot(
        false,
        Shot::xi_full_swing(),
        "Mevo+ full swing (Chipping: not routed)",
    );
    rig.r10_shot("R10 full shot (Chipping: not routed)");
    rig.auto_chip(1, "Auto-chip 1 yd (Chipping)");

    rig.header_mode(Mode::Full, "header picks Full (club stays SW)");
    rig.mevo_shot(
        false,
        Shot::xi_full_swing(),
        "Mevo+ full swing (hand-picked Full, Any)",
    );
    rig.sim_player(
        Right,
        Club::SandWedge,
        "simulator repeats RH, SW (back to Chipping)",
    );
    rig.mevo_shot(
        false,
        Shot::xi_full_swing(),
        "Mevo+ full swing (Chipping again: not routed)",
    );
    rig.mevo_shot(
        true,
        Shot::xi_full_swing(),
        "Xi full swing (Chipping again, routed Xi)",
    );

    rig.sim_player(Left, Club::Putter, "simulator: LH, PT (Putting)");
    rig.oc_shot(true, "Open Connect monitor putt (Putting, routed server)");
    rig.omni_shot(omni_putt(), "Omni putt (Putting: not routed)");
    rig.auto_chip(5, "Auto-chip 5 yd (Putting)");

    rig.sim_player(Left, Club::Iron7, "simulator: LH, 7I (Full)");
    rig.omni_shot(
        omni_full_swing(),
        "Omni full swing, left-handed (Full, Any)",
    );
    rig.r10_shot("R10 full shot, left-handed (Full, Any)");
    rig.mevo_shot(
        true,
        Shot::xi_full_swing(),
        "Xi full swing, left-handed (Full, Any)",
    );
    rig.oc_shot(
        false,
        "Open Connect monitor full shot, left-handed (Full, Any)",
    );
    rig.mevo_shot(
        false,
        launch_only_chip(),
        "Mevo+ launch-only chip, left-handed (Full, Any)",
    );
    rig.sim_player(Right, Club::Driver, "simulator: RH, DR (Full)");
}

fn run(output: Output, name: &str) {
    let mut rig = Rig::start(output);
    play(&mut rig);
    rig.write(name);
}

#[test]
#[ignore = "records the output goldens; run explicitly"]
fn golden_gspro() {
    run(Output::GsPro, "gspro");
}

#[test]
#[ignore = "records the output goldens; run explicitly"]
fn golden_pureplay_normal() {
    run(Output::Pureplay, "pureplay-normal");
}

#[test]
#[ignore = "records the output goldens; run explicitly"]
fn golden_pureplay_strict() {
    run(Output::PureplayStrict, "pureplay-strict");
}
