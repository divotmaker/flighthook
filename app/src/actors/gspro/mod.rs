//! GSPro Open Connect V1 bridge.
//!
//! Subscribes to the unified bus for shot data and forwards to GSPro's TCP API
//! (port 921). Emits GameStateCommand for club/player changes and Internal
//! events for connection status. Reconnects with exponential backoff on failure.

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
    ActorState, ActorStatus, AlertLevel, AlertMessage, FlighthookEvent, FlighthookMessage,
    GameStateCommandEvent, LaunchMonitorEvent, PartialMode, ShotDetectionMode,
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

/// Decide whether a partial (E8-only) shot should be forwarded to GSPro.
fn should_forward_partial(mode: ShotDetectionMode, use_partial: PartialMode) -> bool {
    match use_partial {
        PartialMode::Always => true,
        PartialMode::ChippingOnly => matches!(mode, ShotDetectionMode::Chipping),
        PartialMode::Never => false,
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
            None => return ReconfigureOutcome::NoChange,
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

        ReconfigureOutcome::NoChange
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
                sender.send(FlighthookMessage::new(AlertMessage {
                    level: AlertLevel::Error,
                    message: format!("GSPro connection failed: {e}"),
                }));
                sender.send(FlighthookMessage::new(ActorState::new(
                    ActorStatus::Reconnecting,
                    HashMap::new(),
                )));
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

/// Compute readiness from routing config, current mode, and per-monitor state.
fn readiness(
    routing: &GsProRouting,
    mode: ShotDetectionMode,
    states: &HashMap<String, (bool, bool)>,
) -> (bool, bool) {
    let target = match mode {
        ShotDetectionMode::Full => &routing.full_monitor,
        ShotDetectionMode::Chipping => &routing.chipping_monitor,
        ShotDetectionMode::Putting => &routing.putting_monitor,
    };
    match target {
        Some(id) => *states.get(id.as_str()).unwrap_or(&(false, false)),
        None => {
            // "Any" — ready if any monitor is ready
            let armed = states.values().any(|&(a, _)| a);
            let ball = states.values().any(|&(_, b)| b);
            (armed, ball)
        }
    }
}

/// Check if a shot source matches the routing target for the current mode.
fn shot_matches_routing(routing: &GsProRouting, mode: ShotDetectionMode, source: &str) -> bool {
    let target = match mode {
        ShotDetectionMode::Full => &routing.full_monitor,
        ShotDetectionMode::Chipping => &routing.chipping_monitor,
        ShotDetectionMode::Putting => &routing.putting_monitor,
    };
    match target {
        Some(id) => source == id,
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
    sender.send(FlighthookMessage::new(ActorState::new(
        ActorStatus::Connected,
        HashMap::new(),
    )));

    let mut current_mode = ShotDetectionMode::Full;
    let mut use_partial = PartialMode::default();
    // Backdate so the first heartbeat fires after ~1s instead of waiting the full 10s.
    let mut last_heartbeat = Instant::now() - Duration::from_secs(9);
    let mut read_buf = vec![0u8; 4096];
    let mut monitor_state: HashMap<String, (bool, bool)> = HashMap::new();
    let mut prev_readiness = (false, false);

    loop {
        if receiver.is_shutdown() {
            return Err(BridgeError::Shutdown);
        }
        let mut activity = false;

        // 1. Read any data from GSPro
        match stream.read(&mut read_buf) {
            Ok(0) => {
                tracing::warn!("gspro <- connection closed");
                sender.send(FlighthookMessage::new(AlertMessage {
                    level: AlertLevel::Warn,
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
                sender.send(FlighthookMessage::new(AlertMessage {
                    level: AlertLevel::Warn,
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
                    FlighthookEvent::LaunchMonitor(recv) => match recv.event {
                        LaunchMonitorEvent::ShotResult { shot } => {
                            if !shot_matches_routing(routing, current_mode, &msg.source) {
                                tracing::debug!(
                                    "gspro bridge: skipping shot #{} from '{}' (routed to {:?} for mode {current_mode:?})",
                                    shot.shot_number,
                                    msg.source,
                                    match current_mode {
                                        ShotDetectionMode::Full => &routing.full_monitor,
                                        ShotDetectionMode::Chipping => &routing.chipping_monitor,
                                        ShotDetectionMode::Putting => &routing.putting_monitor,
                                    },
                                );
                            } else if shot.estimated
                                && !should_forward_partial(current_mode, use_partial)
                            {
                                tracing::debug!(
                                    "gspro bridge: skipping estimated shot #{} (mode={current_mode:?}, use_partial={use_partial:?})",
                                    shot.shot_number,
                                );
                            } else {
                                shot_to_send = Some(shot);
                            }
                        }
                        LaunchMonitorEvent::ReadyState {
                            armed,
                            ball_detected,
                        } => {
                            monitor_state.insert(msg.source.clone(), (armed, ball_detected));
                            readiness_changed = true;
                        }
                    },
                    FlighthookEvent::ConfigChanged(changed) => {
                        use_partial = changed.config.use_partial;
                    }
                    FlighthookEvent::GameStateCommand(cmd) => {
                        if let GameStateCommandEvent::SetMode { mode } = cmd.event {
                            current_mode = mode;
                            readiness_changed = true;
                        }
                    }
                    FlighthookEvent::ActorStatus(ref state) => {
                        if matches!(
                            state.status,
                            ActorStatus::Disconnected | ActorStatus::Reconnecting
                        ) {
                            if monitor_state.contains_key(&msg.source) {
                                monitor_state.insert(msg.source.clone(), (false, false));
                                readiness_changed = true;
                            }
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
                    api::GsProMessage::heartbeat_with_readiness(new_readiness.0, new_readiness.1);
                tracing::debug!(
                    "gspro -> heartbeat (readiness: armed={}, ball={})",
                    new_readiness.0,
                    new_readiness.1
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
            let (armed, ball) = readiness(routing, current_mode, &monitor_state);
            prev_readiness = (armed, ball);
            let msg = api::GsProMessage::heartbeat_with_readiness(armed, ball);
            tracing::debug!("gspro -> heartbeat (armed={armed}, ball={ball})");
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
