//! GSPro Open Connect V1 bridge.
//!
//! Subscribes to the unified bus for shot data and forwards to GSPro's TCP API
//! (port 921). Emits PlayerInfo/ClubInfo for club/player changes and
//! ActorStatus for connection status. Reconnects with linear backoff on failure.
//!
//! The bus is drained the whole time, connected or not. Every shot that
//! finishes while GSPro is connected is sent, in order; a shot that finishes
//! while it is not, or is older than [`STALE_SHOT`] when the bridge could
//! send it, is dropped (debug log only: GSPro not running is normal).

pub mod api;
pub mod mapper;
pub(crate) mod proto;

use proto::*;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::backoff::Backoff;
use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;
use flighthook::{
    ActorStatus, FlighthookEvent, FlighthookMessage, Handedness, ShotAccumulator, ShotData,
    ShotDetectionMode, ShotKey,
};

/// Bridge-internal error type.
#[derive(Debug)]
pub(crate) enum BridgeError {
    Io(std::io::Error),
    Shutdown,
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
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

/// GSPro bridge actor. Connects to GSPro on port 921 (TCP, JSON),
/// forwards shot data, and handles heartbeats.
pub struct GsProActor {
    pub addr: SocketAddr,
    pub routing: GsProRouting,
}

impl Actor for GsProActor {
    fn start(&self, _state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let addr = self.addr;
        let routing = self.routing.clone();
        let thread_name = format!("gspro:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(addr, routing, sender, receiver))
            .expect("failed to spawn gspro thread");
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let actor_id = sender.actor_id();
        let (_, index) = match actor_id.split_once('.') {
            Some(pair) => pair,
            None => return ReconfigureOutcome::Applied,
        };

        let snap = state.system.snapshot();
        let section = match snap.gspro.get(index) {
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

/// Reconnect backoff: 1 s, +1 s per failed attempt, capped at 3 s, back to
/// 1 s after a connection. GSPro is on loopback or the LAN, so retrying a
/// refused connect costs nothing, and a short cap means the bridge is back
/// within a few seconds of GSPro starting.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(3);

/// Oldest a finished shot may be when the bridge sends it; older shots are
/// dropped. A connected bridge sends a shot within one loop pass (~150 ms),
/// and a dropped connection is back within ~1.5 s (read timeout + 1 s
/// backoff + connect), so a shot caught by a brief blip still goes through.
/// 5 s is well short of the time a golfer needs to set up the next ball, so
/// GSPro only ever plays the shot the golfer has just hit.
const STALE_SHOT: Duration = Duration::from_secs(5);

/// How often the loops poll the bus and the shutdown flag while idle.
const IDLE_POLL: Duration = Duration::from_millis(100);

/// Main bridge loop. Reconnects forever until the bus closes.
fn run(addr: SocketAddr, routing: GsProRouting, sender: BusSender, receiver: BusReceiver) {
    run_with(
        addr,
        routing,
        Backoff::new(MIN_BACKOFF, MAX_BACKOFF),
        &sender,
        receiver,
    );
}

fn run_with(
    addr: SocketAddr,
    routing: GsProRouting,
    mut backoff: Backoff,
    sender: &BusSender,
    mut receiver: BusReceiver,
) {
    let mut relay = Relay::new(routing);
    let mut ever_connected = false;

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
        sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
            status,
            telemetry: HashMap::new(),
        }));

        let result = connect_and_run(addr, &mut relay, sender, &mut receiver);
        let connected = std::mem::replace(&mut relay.connected, false);
        ever_connected |= connected;
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
                let delay = backoff.delay(connected);
                let text = e.to_string();
                // GSPro not running is normal (one simulator at a time), so a
                // lost or refused connection is a status, not an alert.
                tracing::info!("gspro bridge: {text}, retrying in {delay:?}");
                sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                    status: backoff_status,
                    telemetry: HashMap::new(),
                }));

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

/// A finished shot waiting to be sent.
struct PendingShot {
    shot: ShotData,
    finished_at: Instant,
}

/// Bus-side bridge state, kept across reconnects so the bus is drained the
/// whole time the actor runs, connected or not.
struct Relay {
    routing: GsProRouting,
    /// Whether GSPro is connected. Shots that finish while it is not are
    /// dropped.
    connected: bool,
    mode: ShotDetectionMode,
    handed: Handedness,
    monitor_state: HashMap<String, bool>,
    readiness_changed: bool,
    /// Per-actor shot accumulators, keyed by (actor, shot_key).
    accumulators: HashMap<(String, ShotKey), ShotAccumulator>,
    /// Shots finished while connected, oldest first.
    outbox: VecDeque<PendingShot>,
}

impl Relay {
    fn new(routing: GsProRouting) -> Self {
        Self {
            routing,
            connected: false,
            mode: ShotDetectionMode::Full,
            handed: Handedness::Right,
            monitor_state: HashMap::new(),
            readiness_changed: false,
            accumulators: HashMap::new(),
            outbox: VecDeque::new(),
        }
    }

    /// Process every message waiting on the bus.
    fn drain(&mut self, receiver: &mut BusReceiver) -> Result<(), BridgeError> {
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => return Err(BridgeError::Shutdown),
                Ok(None) => return Ok(()),
                Ok(Some(msg)) => self.handle(msg, Instant::now()),
            }
        }
    }

    fn handle(&mut self, msg: FlighthookMessage, now: Instant) {
        match msg.event {
            FlighthookEvent::ShotTrigger { ref key } => {
                let acc = ShotAccumulator::new(msg.actor.clone(), key.clone());
                self.accumulators
                    .insert((msg.actor.clone(), key.clone()), acc);
            }
            // `derived` is ignored: GSPro simulates the flight itself.
            FlighthookEvent::BallFlight {
                ref key, ref ball, ..
            } => {
                if let Some(acc) = self.accumulators.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_ball(*ball.clone());
                }
            }
            FlighthookEvent::FaceImpact {
                ref key,
                ref impact,
            } => {
                if let Some(acc) = self.accumulators.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_impact(*impact.clone());
                }
            }
            FlighthookEvent::ClubPath { ref key, ref club } => {
                if let Some(acc) = self.accumulators.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_club(*club.clone());
                }
            }
            FlighthookEvent::ShotFinished { ref key, .. } => {
                if let Some(acc) = self.accumulators.remove(&(msg.actor.clone(), key.clone())) {
                    if !shot_matches_routing(&self.routing, self.mode, &msg.actor) {
                        tracing::debug!(
                            "gspro bridge: skipping shot #{} from '{}' (routed to {:?} for mode {:?})",
                            key.shot_number,
                            msg.actor,
                            match self.mode {
                                ShotDetectionMode::Full => &self.routing.full_monitor,
                                ShotDetectionMode::Chipping => &self.routing.chipping_monitor,
                                ShotDetectionMode::Putting => &self.routing.putting_monitor,
                            },
                            self.mode,
                        );
                    } else if let Some(shot) = acc.finish() {
                        if self.connected {
                            self.outbox.push_back(PendingShot {
                                shot,
                                finished_at: now,
                            });
                        } else {
                            tracing::debug!(
                                "gspro bridge: GSPro not connected, dropping shot #{} from '{}'",
                                key.shot_number,
                                msg.actor,
                            );
                        }
                    }
                }
            }
            FlighthookEvent::DeviceTelemetry {
                telemetry: Some(ref tel),
                ..
            } if tel.contains_key("ready") => {
                let ready = tel.get("ready").is_some_and(|v| v == "true");
                self.monitor_state.insert(msg.actor.clone(), ready);
                self.readiness_changed = true;
            }
            FlighthookEvent::SetDetectionMode { mode, handed } => {
                if let Some(m) = mode {
                    self.mode = m;
                    self.readiness_changed = true;
                }
                if let Some(h) = handed {
                    self.handed = h;
                }
            }
            FlighthookEvent::ActorStatus { status, .. } => {
                if matches!(
                    status,
                    ActorStatus::Disconnected | ActorStatus::Reconnecting
                ) && self.monitor_state.contains_key(&msg.actor)
                {
                    self.monitor_state.insert(msg.actor.clone(), false);
                    self.readiness_changed = true;
                }
            }
            _ => {}
        }
    }

    /// Next shot to send, oldest first. Shots older than [`STALE_SHOT`] at
    /// `now` are dropped.
    fn next_shot(&mut self, now: Instant) -> Option<ShotData> {
        while let Some(pending) = self.outbox.pop_front() {
            let age = now.saturating_duration_since(pending.finished_at);
            if age <= STALE_SHOT {
                return Some(pending.shot);
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
        std::mem::take(&mut self.readiness_changed)
    }
}

/// Compute readiness from routing config, current mode, and per-monitor state.
///
/// Returns a single `ready` bool. `DeviceTelemetry.ready` is the unified
/// readiness signal (all device conditions met). GSPro's
/// `launch_monitor_is_ready` and `launch_monitor_ball_detected` are both
/// set from this single value.
fn readiness(
    routing: &GsProRouting,
    mode: ShotDetectionMode,
    states: &HashMap<String, bool>,
) -> bool {
    let target = match mode {
        ShotDetectionMode::Full => &routing.full_monitor,
        ShotDetectionMode::Chipping => &routing.chipping_monitor,
        ShotDetectionMode::Putting => &routing.putting_monitor,
    };
    match target {
        Some(id) => *states.get(id.as_str()).unwrap_or(&false),
        None => {
            // "Any" — ready if any monitor is ready
            states.values().any(|&r| r)
        }
    }
}

/// Check if a shot actor matches the routing target for the current mode.
fn shot_matches_routing(routing: &GsProRouting, mode: ShotDetectionMode, actor: &str) -> bool {
    let target = match mode {
        ShotDetectionMode::Full => &routing.full_monitor,
        ShotDetectionMode::Chipping => &routing.chipping_monitor,
        ShotDetectionMode::Putting => &routing.putting_monitor,
    };
    match target {
        Some(id) => actor == id,
        None => true, // "Any" — accept from all
    }
}

fn connect_and_run(
    addr: SocketAddr,
    relay: &mut Relay,
    sender: &BusSender,
    receiver: &mut BusReceiver,
) -> Result<(), BridgeError> {
    let name = sender.actor_id();

    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(BridgeError::Io)?;
    stream.set_nodelay(true).map_err(BridgeError::Io)?;
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .map_err(BridgeError::Io)?;

    // Shots that finished during the connect attempt finished while GSPro
    // was not connected.
    relay.drain(receiver)?;
    relay.connected = true;
    tracing::info!("gspro bridge: connected to {addr}");
    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status: ActorStatus::Connected,
        telemetry: HashMap::new(),
    }));

    // Backdate so the first heartbeat fires after ~1s instead of waiting the full 10s.
    let mut last_heartbeat = Instant::now() - Duration::from_secs(9);
    let mut read_buf = vec![0u8; 4096];
    let mut prev_readiness = false;

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
                handle_response(&read_buf[..n], sender);
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

        // 3. Send every finished shot, oldest first
        while let Some(shot) = relay.next_shot(Instant::now()) {
            let msg = mapper::map_shot(&shot, relay.handed);
            log_outbound(&msg);
            if let Ok(json_str) = serde_json::to_string(&msg) {
                tracing::info!(
                    target: "audit",
                    "{name} sent shot {json_str} | {msg:?}",
                );
            }
            if let Err(e) = send_message(&mut stream, &msg) {
                tracing::debug!(
                    "gspro bridge: sending shot #{} failed, not retried",
                    shot.shot_number,
                );
                return Err(e);
            }
            activity = true;
        }

        // 4. Immediate heartbeat on readiness change
        if relay.take_readiness_changed() {
            let new_readiness = readiness(&relay.routing, relay.mode, &relay.monitor_state);
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
                send_message(&mut stream, &msg)?;
                last_heartbeat = Instant::now();
                activity = true;
            }
        }

        // 5. Periodic heartbeat if due
        if last_heartbeat.elapsed() >= Duration::from_secs(10) {
            last_heartbeat = Instant::now();
            let ready = readiness(&relay.routing, relay.mode, &relay.monitor_state);
            prev_readiness = ready;
            let msg = api::GsProMessage::heartbeat_with_readiness(ready, ready);
            tracing::debug!("gspro -> heartbeat (ready={ready})");
            if let Ok(json_str) = serde_json::to_string(&msg) {
                tracing::info!(
                    target: "audit",
                    "{name} sent heartbeat {json_str} | {msg:?}",
                );
            }
            send_message(&mut stream, &msg)?;
            activity = true;
        }

        if !activity {
            std::thread::sleep(IDLE_POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use tokio::sync::broadcast;

    use super::*;

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
        let ball = mapper::tests::square_like_shot(None)
            .ball
            .expect("golden shot has ball data");
        monitor.send(FlighthookMessage::new(FlighthookEvent::ShotTrigger {
            key: key(n),
        }));
        monitor.send(FlighthookMessage::new(FlighthookEvent::BallFlight {
            key: key(n),
            ball: Box::new(ball),
            derived: None,
        }));
        monitor.send(FlighthookMessage::new(FlighthookEvent::ShotFinished {
            key: key(n),
            raw: None,
        }));
    }

    /// The golden JSON for the shot `emit_shot(_, n)` puts on the bus.
    fn golden(n: u32) -> String {
        mapper::tests::GOLDEN_RH.replace("\"ShotNumber\":12,", &format!("\"ShotNumber\":{n},"))
    }

    // -- Relay (pure) -------------------------------------------------------

    fn relay_with_bus() -> (Relay, BusSender, BusReceiver) {
        let (tx, _) = broadcast::channel(64);
        let monitor = BusSender::new("mevo.0".into(), tx.clone(), Arc::default());
        let receiver = BusSender::new("gspro.0".into(), tx, Arc::default()).subscribe();
        (Relay::new(GsProRouting::default()), monitor, receiver)
    }

    fn sent_numbers(relay: &mut Relay, now: Instant) -> Vec<u32> {
        std::iter::from_fn(|| relay.next_shot(now))
            .map(|s| s.shot_number)
            .collect()
    }

    #[test]
    fn shots_finished_in_one_drain_are_all_queued_in_order() {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connected = true;
        for n in 1..=3 {
            emit_shot(&monitor, n);
        }
        relay.drain(&mut receiver).expect("bus open");
        assert_eq!(sent_numbers(&mut relay, Instant::now()), [1, 2, 3]);
    }

    #[test]
    fn shots_finished_while_disconnected_are_dropped() {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        emit_shot(&monitor, 1);
        relay.drain(&mut receiver).expect("bus open");
        relay.connected = true;
        emit_shot(&monitor, 2);
        relay.drain(&mut receiver).expect("bus open");
        assert_eq!(sent_numbers(&mut relay, Instant::now()), [2]);
    }

    #[test]
    fn a_shot_older_than_the_stale_limit_is_dropped() {
        let (mut relay, monitor, mut receiver) = relay_with_bus();
        relay.connected = true;
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
        relay.connected = true;
        emit_shot(&monitor, 1);
        relay.drain(&mut receiver).expect("bus open");
        let now = relay.outbox[0].finished_at + STALE_SHOT;
        assert_eq!(sent_numbers(&mut relay, now), [1]);
    }

    // -- Bridge against a fake GSPro ---------------------------------------

    /// A running bridge (`gspro.0`) wired to a fake GSPro listener it dials.
    struct Bridge {
        gspro: TcpListener,
        monitor: BusSender,
        events: broadcast::Receiver<FlighthookMessage>,
        shutdown: Arc<AtomicBool>,
        done: mpsc::Receiver<()>,
    }

    impl Bridge {
        fn start(backoff: Backoff) -> Self {
            let gspro = TcpListener::bind("127.0.0.1:0").expect("bind fake GSPro");
            gspro.set_nonblocking(true).expect("nonblocking listener");
            let addr = gspro.local_addr().expect("listener addr");
            let (tx, events) = broadcast::channel(1024);
            let shutdown = Arc::new(AtomicBool::new(false));
            let sender = BusSender::new("gspro.0".into(), tx.clone(), Arc::clone(&shutdown));
            let receiver = sender.subscribe();
            let monitor = BusSender::new("mevo.0".into(), tx, Arc::default());
            let (done_tx, done) = mpsc::channel();
            std::thread::spawn(move || {
                run_with(addr, GsProRouting::default(), backoff, &sender, receiver);
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

        /// Wait for a bus event from `gspro.0` matching `want`; returns every
        /// `gspro.0` alert seen on the way.
        fn wait_for(&mut self, what: &str, want: impl Fn(&FlighthookEvent) -> bool) -> Vec<String> {
            let deadline = Instant::now() + WAIT;
            let mut alerts = Vec::new();
            loop {
                match self.events.try_recv() {
                    Ok(msg) if msg.actor == "gspro.0" => {
                        if let FlighthookEvent::Alert { ref message, .. } = msg.event {
                            alerts.push(message.clone());
                        }
                        if want(&msg.event) {
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

        /// Every `gspro.0` alert already on the bus, without waiting.
        fn alerts_so_far(&mut self) -> Vec<String> {
            let mut alerts = Vec::new();
            while let Ok(msg) = self.events.try_recv() {
                if let (true, FlighthookEvent::Alert { message, .. }) =
                    (msg.actor == "gspro.0", msg.event)
                {
                    alerts.push(message);
                }
            }
            alerts
        }

        fn wait_for_status(&mut self, status: ActorStatus) -> Vec<String> {
            self.wait_for(
                &format!("{status:?}"),
                |e| matches!(e, FlighthookEvent::ActorStatus { status: s, .. } if *s == status),
            )
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
}
