//! The FlightScope card's equivalence script: a scripted device served over
//! a local TCP port, the system actor, and one script per model that waits
//! for the card's messages before each step. The recorded old actor and the
//! card on ironsight's driver run the same script; the card's tests compare
//! what each put on the bus and in the export.
//!
//! The including module brings `iron` into scope: the ironsight crate with
//! its `testing` feature.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flighthook::{ActorStatus, FlighthookEvent, FlighthookMessage, MevoSection, ShotDetectionMode};
use tokio::sync::broadcast;

use super::iron;
use super::iron::testing::{FakeDevice, Shot};
use crate::actors::Actor;
use crate::bus::BusSender;
use crate::state::SystemState;

/// Which device the script plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    MevoPlus,
    Gen2,
    Xi,
}

/// What the scripted device does next.
pub enum Do {
    /// Send this shot (and answer its result request).
    Shot(Shot),
    /// Send these frames.
    Frames(Vec<iron::RawFrame>),
    /// Drop the connection; the next one gets a fresh device.
    Hang,
}

/// A scripted `model` served on a local TCP port, one connection at a
/// time; each device lives on the serving thread.
pub struct Device {
    pub addr: SocketAddr,
    todo: mpsc::Sender<Do>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Device {
    pub fn serve(model: Model) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (todo, todo_rx) = mpsc::channel::<Do>();
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
                let mut device = match model {
                    Model::MevoPlus => FakeDevice::mevo_plus(),
                    Model::Gen2 => FakeDevice::mevo_gen2(),
                    Model::Xi => FakeDevice::xi(),
                };
                let mut buf = [0u8; 4096];
                'link: while !stopped.load(Ordering::SeqCst) {
                    while let Ok(next) = todo_rx.try_recv() {
                        match next {
                            Do::Shot(shot) => device.push_shot(&shot),
                            Do::Frames(frames) => {
                                for f in frames {
                                    device.push(f.src, f.type_id, f.payload);
                                }
                            }
                            Do::Hang => break 'link,
                        }
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

    pub fn send(&self, todo: Do) {
        self.todo.send(todo).expect("the device thread runs");
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The current UTC time, milliseconds.
pub fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("ms")
}

/// A running `mevo.0` card on a scripted device, the system actor, and the
/// messages of `mevo.0` seen so far.
pub struct Bench {
    pub state: Arc<SystemState>,
    pub device: Device,
    tx: broadcast::Sender<FlighthookMessage>,
    rx: broadcast::Receiver<FlighthookMessage>,
    pub seen: Vec<FlighthookMessage>,
    /// When each shot's first frames were handed to the device (UTC ms),
    /// and when its trigger reached the bus.
    pub shot_times: Vec<(u64, u64)>,
    pushed_at: Option<u64>,
    dir: PathBuf,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Bench {
    /// `section` (its address pointed at the scripted `model`) as card
    /// `mevo.0`, built by `actor`, with the game in Full mode.
    pub fn start(
        model: Model,
        mut section: MevoSection,
        actor: impl FnOnce(&MevoSection) -> Box<dyn Actor>,
    ) -> Self {
        let device = Device::serve(model);
        set_address(&mut section, device.addr);
        let dir = std::env::temp_dir().join(format!(
            "flighthook-mevo-eq-{}",
            crate::state::config::generate_id()
        ));
        let (state, writer) = SystemState::new(dir.join("config.toml"));
        writer.set_mode(ShotDetectionMode::Full);
        state.system.update(|c| {
            c.mevo.insert("0".into(), section.clone());
        });
        let state = Arc::new(state);
        let (tx, rx) = broadcast::channel(4096);
        crate::actors::system::start_for_test(writer, &state, &tx, Arc::default());
        crate::actors::start_actor("mevo.0".into(), actor(&section), &state, &tx);
        Self {
            state,
            device,
            tx,
            rx,
            seen: Vec::new(),
            shot_times: Vec::new(),
            pushed_at: None,
            dir,
        }
    }

    /// Messages of `mevo.0` until one matches `want`, within 20 s.
    pub fn until(&mut self, mut want: impl FnMut(&FlighthookMessage) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out after {} messages: {:?}",
                self.seen.len(),
                self.seen.last()
            );
            match self.rx.try_recv() {
                Ok(m) if m.actor == "mevo.0" => {
                    if matches!(m.event, FlighthookEvent::ShotTrigger { .. })
                        && let Some(pushed) = self.pushed_at.take()
                    {
                        self.shot_times.push((pushed, now_ms()));
                    }
                    let done = want(&m);
                    self.seen.push(m);
                    if done {
                        return;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Lagged(n)) => panic!("lagged {n}"),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }

    /// Until the device reports `ready` = `value`.
    pub fn ready(&mut self, value: &str) {
        self.until(|m| ready_is(m, value));
    }

    /// Hand `shot` to the device and wait until it finished and the device
    /// is ready again.
    pub fn shot(&mut self, shot: Shot) {
        self.pushed_at = Some(now_ms());
        self.device.send(Do::Shot(shot));
        self.until(|m| matches!(m.event, FlighthookEvent::ShotFinished { .. }));
        self.ready("true");
    }

    /// The game changes the detection mode.
    pub fn mode(&self, mode: ShotDetectionMode) {
        BusSender::new("web".into(), self.tx.clone(), Arc::default()).send(FlighthookMessage::new(
            FlighthookEvent::SetDetectionMode {
                mode: Some(mode),
                handed: None,
                club: None,
            },
        ));
    }

    /// The device drops the connection: until the card reads Reconnecting,
    /// then the card is stopped, until Disconnected.
    pub fn hang_and_stop(&mut self) {
        self.device.send(Do::Hang);
        self.until(|m| is_status(m, ActorStatus::Reconnecting));
        self.state.stop_and_wait("mevo.0");
        self.until(|m| is_status(m, ActorStatus::Disconnected));
    }
}

/// `section` with its device address set to `addr`.
fn set_address(section: &mut MevoSection, addr: SocketAddr) {
    let mut json = serde_json::to_value(&*section).expect("section");
    json["address"] = serde_json::Value::String(addr.to_string());
    *section = serde_json::from_value(json).expect("section");
}

pub fn is_status(m: &FlighthookMessage, want: ActorStatus) -> bool {
    matches!(&m.event, FlighthookEvent::ActorStatus { status, .. } if *status == want)
}

pub fn ready_is(m: &FlighthookMessage, value: &str) -> bool {
    matches!(&m.event, FlighthookEvent::DeviceTelemetry { telemetry: Some(t), .. }
        if t.get("ready").is_some_and(|v| v == value))
}

/// The card sections the scripts run with.
pub fn section(model: Model) -> MevoSection {
    let json = match model {
        // As the card writes a new FlightScope card.
        Model::MevoPlus | Model::Gen2 => serde_json::json!({
            "name": "Mevo WiFi", "address": "", "ball_type": 1, "tee_height": "1.5in",
            "range": "8ft", "surface_height": "0in", "track_pct": 80.0,
            "use_estimated": false, "camera_mode": "standard",
            "environment": "limited_flight"
        }),
        Model::Xi => serde_json::json!({
            "name": "Xi", "address": "", "use_estimated": true, "spin_source": "auto",
            "range": "8ft"
        }),
    };
    serde_json::from_value(json).expect("section")
}

/// The Xi full swing, with every result from the AVR.
fn full_swing() -> Shot {
    Shot::xi_full_swing()
}

/// A shot with only the early result and club data: an estimate.
fn early_only() -> Shot {
    Shot::xi_chip()
}

/// A truncated AVR status: a message the session cannot decode.
fn undecodable() -> iron::RawFrame {
    let mut payload = iron::testing::avr_status([0, 0]);
    payload.truncate(20);
    iron::RawFrame {
        dest: iron::BusAddr::App,
        src: iron::BusAddr::Avr,
        type_id: iron::protocol::TYPE_STATUS,
        payload,
    }
}

/// Run `model`'s script on `bench`.
pub fn play(bench: &mut Bench, model: Model) {
    bench.ready("true");
    match model {
        Model::MevoPlus => {
            bench.shot(full_swing());
            // Estimates are off: skipped.
            bench.shot(early_only());
            bench.shot(Shot::xi_no_read());
            // A mode change re-arms the device.
            bench.mode(ShotDetectionMode::Chipping);
            bench.ready("false");
            bench.ready("true");
            // A mode change while a shot runs: applied after it.
            let mut rest = full_swing();
            let trigger = rest.frames.remove(0);
            bench.pushed_at = Some(now_ms());
            bench.device.send(Do::Frames(vec![trigger]));
            bench.until(|m| matches!(m.event, FlighthookEvent::ShotTrigger { .. }));
            bench.mode(ShotDetectionMode::Full);
            std::thread::sleep(Duration::from_millis(300));
            bench.device.send(Do::Shot(rest));
            bench.until(|m| matches!(m.event, FlighthookEvent::ShotFinished { .. }));
            bench.ready("false");
            bench.ready("true");
            bench.device.send(Do::Frames(vec![undecodable()]));
            bench.until(|m| matches!(m.event, FlighthookEvent::Alert { .. }));
            bench.shot(full_swing());
        }
        Model::Gen2 => {
            bench.shot(full_swing());
        }
        Model::Xi => {
            // Auto in Full: the early result's spin.
            bench.shot(full_swing());
            bench.mode(ShotDetectionMode::Chipping);
            bench.ready("false");
            bench.ready("true");
            // Chipping to Putting doesn't change what the Xi is sent.
            bench.mode(ShotDetectionMode::Putting);
            std::thread::sleep(Duration::from_millis(300));
            // Auto in Putting: the full result's spin.
            bench.shot(full_swing());
            // Estimates are on: the early result makes the shot.
            bench.shot(early_only());
            bench.shot(Shot::xi_no_read());
        }
    }
    bench.hang_and_stop();
}
