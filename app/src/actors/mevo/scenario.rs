//! The FlightScope card's scripted-device bench: a scripted device served
//! over a local TCP port, the system actor, and a `mevo.0` card on
//! ironsight's driver reading it. The card's tests hand the device shots
//! and read what the card put on the bus.
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
use std::time::{Duration, Instant};

use flighthook::{MevoSection, Mode};
use flightrelay::{FrpBody, GolferSelect};
use tokio::sync::broadcast;

use super::iron::testing::{FakeDevice, Shot};
use crate::actors::Actor;
use crate::bus::BusMsg;
use crate::state::SystemState;

/// Which device the script plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    MevoPlus,
    Xi,
}

/// What the scripted device does next.
pub enum Do {
    /// Send this shot (and answer its result request).
    Shot(Shot),
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
                    Model::Xi => FakeDevice::xi(),
                };
                let mut buf = [0u8; 4096];
                while !stopped.load(Ordering::SeqCst) {
                    while let Ok(Do::Shot(shot)) = todo_rx.try_recv() {
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

/// A running `mevo.0` card on a scripted device, the system actor, and the
/// card's messages seen so far.
pub struct Bench {
    pub state: Arc<SystemState>,
    pub device: Device,
    rx: broadcast::Receiver<BusMsg>,
    /// The card's FRP actor id.
    frp: String,
    pub seen: Vec<BusMsg>,
    dir: PathBuf,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Bench {
    /// `section` (its address pointed at the scripted `model`) as card
    /// `mevo.0`, built by `actor`, with the session in Full mode.
    pub fn start(
        model: Model,
        mut section: MevoSection,
        actor: impl FnOnce(&MevoSection) -> Box<dyn Actor>,
    ) -> Self {
        let device = Device::serve(model);
        set_address(&mut section, device.addr);
        let dir = std::env::temp_dir().join(format!(
            "flighthook-mevo-bench-{}",
            crate::state::config::generate_id()
        ));
        let (state, writer) = SystemState::new(dir.join("config.toml"));
        let _ = writer.apply(&GolferSelect {
            mode: Some(Mode::Full),
            ..GolferSelect::new("test")
        });
        state.system.update(|c| {
            c.mevo.insert("0".into(), section.clone());
        });
        let frp = state.actor_of_card("mevo.0").expect("frp id");
        let state = Arc::new(state);
        let (tx, rx) = broadcast::channel(4096);
        crate::actors::system::start_for_test(writer, &state, &tx, Arc::default());
        crate::actors::start_actor("mevo.0".into(), actor(&section), &state, &tx);
        Self {
            state,
            device,
            rx,
            frp,
            seen: Vec::new(),
            dir,
        }
    }

    /// Whether `m` is the card's: its local events and its FRP messages.
    fn mine(&self, m: &BusMsg) -> bool {
        match m {
            BusMsg::Local { card, .. } => card == "mevo.0",
            BusMsg::Frp { msg, .. } => msg.from == self.frp,
        }
    }

    /// Messages of `mevo.0` until one matches `want`, within 20 s.
    pub fn until(&mut self, mut want: impl FnMut(&BusMsg) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out after {} messages: {:?}",
                self.seen.len(),
                self.seen.last()
            );
            match self.rx.try_recv() {
                Ok(m) if self.mine(&m) => {
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

    /// Until the card's `bus.actor` says ready (no blockers) as `ready`.
    pub fn ready(&mut self, ready: bool) {
        self.until(|m| {
            matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::BusActor(a))
                if a.blockers.is_empty() == ready)
        });
    }
}

/// `section` with its device address set to `addr`.
fn set_address(section: &mut MevoSection, addr: SocketAddr) {
    let mut json = serde_json::to_value(&*section).expect("section");
    json["address"] = serde_json::Value::String(addr.to_string());
    *section = serde_json::from_value(json).expect("section");
}

/// The card sections the bench runs with.
pub fn section(model: Model) -> MevoSection {
    let json = match model {
        // As the card writes a new FlightScope card.
        Model::MevoPlus => serde_json::json!({
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
