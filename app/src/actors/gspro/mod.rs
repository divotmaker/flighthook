//! GSPro Open Connect V1 bridge.
//!
//! Subscribes to the unified bus for shot data and forwards to GSPro's TCP API
//! (port 921). Emits PlayerInfo/ClubInfo for club/player changes and
//! ActorStatus for connection status. Reconnects with exponential backoff on failure.

pub mod api;
pub mod mapper;
pub(crate) mod proto;

use proto::*;

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;
use flighthook::{
    ActorStatus, FlighthookEvent, FlighthookMessage, Severity,
    ShotAccumulator, ShotDetectionMode, ShotKey,
};

/// Bridge-internal error type.
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

/// Main bridge loop. Reconnects forever until the bus closes.
fn run(addr: SocketAddr, routing: GsProRouting, sender: BusSender, mut receiver: BusReceiver) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(30);

    loop {
        if receiver.is_shutdown() {
            tracing::info!("gspro bridge: shutting down");
            return;
        }
        match connect_and_run(addr, &routing, &sender, &mut receiver) {
            Ok(()) => {
                tracing::info!("gspro bridge: shutting down");
                return;
            }
            Err(BridgeError::Shutdown) => {
                tracing::info!("gspro bridge: shutting down");
                return;
            }
            Err(e) => {
                tracing::info!("gspro bridge: {e}, reconnecting in {backoff:?}");
                sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                    severity: Severity::Error,
                    message: format!("GSPro connection failed: {e}"),
                }));
                sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                    status: ActorStatus::Reconnecting,
                    telemetry: HashMap::new(),
                }));
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(max_backoff);
            }
        }
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
    routing: &GsProRouting,
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

    tracing::info!("gspro bridge: connected to {addr}");
    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status: ActorStatus::Connected,
        telemetry: HashMap::new(),
    }));

    let mut current_mode = ShotDetectionMode::Full;
    // Backdate so the first heartbeat fires after ~1s instead of waiting the full 10s.
    let mut last_heartbeat = Instant::now() - Duration::from_secs(9);
    let mut read_buf = vec![0u8; 4096];
    let mut monitor_state: HashMap<String, bool> = HashMap::new();
    let mut prev_readiness = false;
    // Per-actor shot accumulators, keyed by (actor, shot_key)
    let mut accumulators: HashMap<(String, ShotKey), ShotAccumulator> = HashMap::new();

    loop {
        if receiver.is_shutdown() {
            return Err(BridgeError::Shutdown);
        }
        let mut activity = false;

        // 1. Read any data from GSPro
        match stream.read(&mut read_buf) {
            Ok(0) => {
                tracing::warn!("gspro <- connection closed");
                sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                    severity: Severity::Warn,
                    message: "GSPro closed the connection".into(),
                }));
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
                tracing::warn!("gspro <- read error: {e}");
                sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                    severity: Severity::Warn,
                    message: format!("GSPro read error: {e}"),
                }));
                return Err(BridgeError::Io(e));
            }
        }

        // 2. Drain bus events — collect any queued shot to send
        let mut shot_to_send = None;
        let mut readiness_changed = false;
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => return Err(BridgeError::Shutdown),
                Ok(None) => break,
                Ok(Some(msg)) => match msg.event {
                    FlighthookEvent::ShotTrigger { ref key } => {
                        let acc = ShotAccumulator::new(msg.actor.clone(), key.clone());
                        accumulators.insert((msg.actor.clone(), key.clone()), acc);
                    }
                    FlighthookEvent::BallFlight {
                        ref key,
                        ref ball,
                    } => {
                        if let Some(acc) =
                            accumulators.get_mut(&(msg.actor.clone(), key.clone()))
                        {
                            acc.set_ball(*ball.clone());
                        }
                    }
                    FlighthookEvent::FaceImpact {
                        ref key,
                        ref impact,
                    } => {
                        if let Some(acc) =
                            accumulators.get_mut(&(msg.actor.clone(), key.clone()))
                        {
                            acc.set_impact(*impact.clone());
                        }
                    }
                    FlighthookEvent::ClubPath { ref key, ref club } => {
                        if let Some(acc) =
                            accumulators.get_mut(&(msg.actor.clone(), key.clone()))
                        {
                            acc.set_club(*club.clone());
                        }
                    }
                    FlighthookEvent::ShotFinished { ref key } => {
                        if let Some(acc) =
                            accumulators.remove(&(msg.actor.clone(), key.clone()))
                        {
                            if !shot_matches_routing(routing, current_mode, &msg.actor) {
                                tracing::debug!(
                                    "gspro bridge: skipping shot #{} from '{}' (routed to {:?} for mode {current_mode:?})",
                                    key.shot_number,
                                    msg.actor,
                                    match current_mode {
                                        ShotDetectionMode::Full => &routing.full_monitor,
                                        ShotDetectionMode::Chipping => &routing.chipping_monitor,
                                        ShotDetectionMode::Putting => &routing.putting_monitor,
                                    },
                                );
                            } else if let Some(shot) = acc.finish() {
                                shot_to_send = Some(Box::new(shot));
                            }
                        }
                    }
                    FlighthookEvent::DeviceTelemetry {
                        telemetry: Some(ref tel), ..
                    } if tel.contains_key("ready") => {
                        let ready = tel.get("ready").is_some_and(|v| v == "true");
                        monitor_state.insert(msg.actor.clone(), ready);
                        readiness_changed = true;
                    }
                    FlighthookEvent::SetDetectionMode { mode: Some(m), .. } => {
                        current_mode = m;
                        readiness_changed = true;
                    }
                    FlighthookEvent::ActorStatus { status, .. } => {
                        if matches!(
                            status,
                            ActorStatus::Disconnected | ActorStatus::Reconnecting
                        ) && monitor_state.contains_key(&msg.actor)
                        {
                            monitor_state.insert(msg.actor.clone(), false);
                            readiness_changed = true;
                        }
                    }
                    _ => {}
                },
            }
        }

        // 3. Send shot if queued
        if let Some(shot) = shot_to_send {
            let msg = mapper::map_shot(&shot);
            log_outbound(&msg);
            if let Ok(json_str) = serde_json::to_string(&msg) {
                tracing::info!(
                    target: "audit",
                    "{name} sent shot {json_str} | {msg:?}",
                );
            }
            send_message(&mut stream, &msg)?;
            activity = true;
        }

        // 4. Immediate heartbeat on readiness change
        if readiness_changed {
            let new_readiness = readiness(routing, current_mode, &monitor_state);
            if new_readiness != prev_readiness {
                prev_readiness = new_readiness;
                let msg =
                    api::GsProMessage::heartbeat_with_readiness(new_readiness, new_readiness);
                tracing::debug!(
                    "gspro -> heartbeat (ready={})",
                    new_readiness,
                );
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
            let ready = readiness(routing, current_mode, &monitor_state);
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
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
