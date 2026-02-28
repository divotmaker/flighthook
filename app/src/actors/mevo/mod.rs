pub(crate) mod accumulator;
mod client;
pub mod settings;

pub use client::MevoClient;
pub use settings::SessionConfig;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ironsight::{ConnError, Message};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Message labeling (audit log)
// ---------------------------------------------------------------------------

fn msg_label(msg: &Message) -> &'static str {
    match msg {
        Message::FlightResult(_) => "FlightResult(0xD4)",
        Message::FlightResultV1(_) => "FlightResultV1(0xE8)",
        Message::ClubResult(_) => "ClubResult(0xED)",
        Message::SpinResult(_) => "SpinResult(0xEF)",
        Message::SpeedProfile(_) => "SpeedProfile(0xD9)",
        Message::TrackingStatus(_) => "TrackingStatus(0xE9)",
        Message::PrcData(_) => "PrcData(0xEC)",
        Message::ClubPrc(_) => "ClubPrc(0xEE)",
        Message::ShotText(_) => "ShotText(0xE5)",
        Message::AvrStatus(_) => "AvrStatus(0xAA)",
        Message::DspStatus(_) => "DspStatus(0xAA)",
        Message::PiStatus(_) => "PiStatus(0xAA)",
        Message::ConfigAck(_) => "ConfigAck(0x95)",
        Message::ModeAck(_) => "ModeAck(0xB1)",
        Message::Text(_) => "Text(0xE3)",
        Message::ModeSet(_) => "ModeSet(0xA5)",
        Message::ParamValue(_) => "ParamValue(0xBF)",
        Message::RadarCal(_) => "RadarCal(0xA4)",
        Message::ConfigResp(_) => "ConfigResp(0xA0)",
        Message::AvrConfigResp(_) => "AvrConfigResp(0xA2)",
        Message::DspQueryResp(_) => "DspQueryResp(0xC8)",
        Message::DevInfoResp(_) => "DevInfoResp(0xE7)",
        Message::ProdInfoResp(_) => "ProdInfoResp(0xFD)",
        Message::NetConfigResp(_) => "NetConfigResp(0xDE)",
        Message::CalParamResp(_) => "CalParamResp(0xD1)",
        Message::CalDataResp(_) => "CalDataResp(0xD3)",
        Message::TimeSync(_) => "TimeSync(0x9B)",
        Message::CamState(_) => "CamState(0x81)",
        Message::CamConfig(_) => "CamConfig(0x82)",
        Message::CamImageAvail(_) => "CamImageAvail(0x84)",
        Message::SensorActResp(_) => "SensorActResp(0x89)",
        Message::WifiScan { .. } => "WifiScan(0x87)",
        Message::DspDebug(_) => "DspDebug(0xF0)",
        Message::Unknown { type_id, .. } => {
            let _ = type_id;
            "Unknown"
        }
    }
}

use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;
use accumulator::ShotAccumulator;
use flighthook::{
    ActorState, ActorStatus, AlertLevel, AlertMessage, ConfigChanged, FlighthookEvent,
    FlighthookMessage, GameStateCommandEvent, LaunchMonitorEvent, ShotDetectionMode,
};

const RECV_TIMEOUT: Duration = Duration::from_millis(900);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Mevo session actor. Connects to a Mevo/Mevo+ device, runs the handshake,
/// arms, and processes shots in a reconnecting event loop.
pub struct MevoActor {
    pub addr: SocketAddr,
    pub initial_mode: ShotDetectionMode,
    pub session_config: SessionConfig,
}

impl Actor for MevoActor {
    fn start(&self, _state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let addr = self.addr;
        let initial_mode = self.initial_mode;
        let session_config = self.session_config.clone();
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run(addr, initial_mode, session_config, sender, receiver);
            })
            .expect("failed to spawn mevo thread");
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        // Parse actor_id "mevo.0" -> ("mevo", "0")
        let actor_id = sender.actor_id();
        let (_, index) = match actor_id.split_once('.') {
            Some(pair) => pair,
            None => return ReconfigureOutcome::NoChange,
        };

        let snap = state.system.snapshot();
        let section = match snap.mevo.get(index) {
            Some(s) => s,
            None => return ReconfigureOutcome::RestartRequired, // section removed
        };

        // Check if address changed -> restart required
        let new_addr_str = section.address.as_deref().unwrap_or("192.168.2.1:5100");
        if let Ok(new_addr) = new_addr_str.parse::<SocketAddr>() {
            if new_addr != self.addr {
                return ReconfigureOutcome::RestartRequired;
            }
        } else {
            return ReconfigureOutcome::RestartRequired;
        }

        // Check if session config changed -> emit ConfigChanged on bus
        let new_session = SessionConfig::from_mevo_section(section);
        if new_session != self.session_config {
            sender.send(FlighthookMessage::new(ConfigChanged {
                config: new_session.to_config_event(),
            }));
            return ReconfigureOutcome::Applied;
        }

        ReconfigureOutcome::NoChange
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_transient(e: &ConnError) -> bool {
    matches!(e, ConnError::Timeout { .. } | ConnError::Protocol(_))
}

fn same_mode(a: ShotDetectionMode, b: ShotDetectionMode) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

fn change_mode(
    client: &mut MevoClient,
    mode: ShotDetectionMode,
    session_config: &SessionConfig,
) -> Result<(), ConnError> {
    client.configure(session_config, &mode)?;
    client.arm()
}

/// Build a telemetry state map for status emission.
fn telemetry_state(
    battery_pct: u8,
    tilt: f64,
    roll: f64,
    temp_c: f64,
    external_power: bool,
    armed: bool,
) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("battery_pct".into(), battery_pct.to_string());
    m.insert("tilt".into(), format!("{tilt:.1}"));
    m.insert("roll".into(), format!("{roll:.1}"));
    m.insert("temp_c".into(), format!("{temp_c:.1}"));
    m.insert("external_power".into(), external_power.to_string());
    m.insert("armed".into(), armed.to_string());
    m
}

fn emit_device_status(sender: &BusSender, status: ActorStatus, state: HashMap<String, String>) {
    sender.send(FlighthookMessage::new(ActorState::new(status, state)));
}

fn emit_alert(sender: &BusSender, level: AlertLevel, message: impl Into<String>) {
    sender.send(FlighthookMessage::new(AlertMessage {
        level,
        message: message.into(),
    }));
}

fn emit_ready_state(sender: &BusSender, armed: bool) {
    sender.send(FlighthookMessage::new(LaunchMonitorEvent::ReadyState {
        armed,
        ball_detected: armed, // Mevo has no ball sensor
    }));
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

fn run(
    addr: SocketAddr,
    initial_mode: ShotDetectionMode,
    initial_session_config: SessionConfig,
    sender: BusSender,
    mut receiver: BusReceiver,
) {
    let mut backoff = MIN_BACKOFF;
    let mut session_config = initial_session_config;
    let mut ever_connected = false;

    // Broadcast initial config
    sender.send(FlighthookMessage::new(ConfigChanged {
        config: session_config.to_config_event(),
    }));

    loop {
        if receiver.poll().is_err() {
            break;
        }

        emit_device_status(&sender, ActorStatus::Starting, HashMap::new());

        match connect_and_run(
            &addr,
            initial_mode,
            &sender,
            &mut receiver,
            &mut session_config,
            &mut ever_connected,
        ) {
            Ok(()) => break,
            Err(e) => {
                warn!("session error: {e}");
                emit_alert(
                    &sender,
                    AlertLevel::Warn,
                    format!("Device connection error: {e}"),
                );
                let backoff_status = if ever_connected {
                    ActorStatus::Reconnecting
                } else {
                    ActorStatus::Disconnected
                };
                emit_device_status(&sender, backoff_status, HashMap::new());
                emit_ready_state(&sender, false);

                let verb = if ever_connected {
                    "Reconnecting"
                } else {
                    "Retrying"
                };
                info!("{} in {}s", verb.to_lowercase(), backoff.as_secs());

                let deadline = std::time::Instant::now() + backoff;
                while std::time::Instant::now() < deadline {
                    if receiver.poll().is_err() {
                        emit_device_status(&sender, ActorStatus::Disconnected, HashMap::new());
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }

    emit_device_status(&sender, ActorStatus::Disconnected, HashMap::new());
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

fn connect_and_run(
    addr: &SocketAddr,
    initial_mode: ShotDetectionMode,
    sender: &BusSender,
    receiver: &mut BusReceiver,
    session_config: &mut SessionConfig,
    ever_connected: &mut bool,
) -> Result<(), ConnError> {
    let device_id = sender.actor_id();

    // 1. Connect
    info!("connecting...");
    let mut client = MevoClient::connect(addr)?;
    *ever_connected = true;

    // 2. Handshake
    info!("handshaking...");
    let handshake_info = client.handshake()?;

    info!(
        "handshake complete: {} | battery {}%{} | tilt {:.1} roll {:.1}",
        handshake_info.device_info,
        handshake_info.battery_pct,
        if handshake_info.external_power {
            " (charging)"
        } else {
            ""
        },
        handshake_info.tilt,
        handshake_info.roll,
    );

    // Emit connected status with device info and initial telemetry
    let mut init_state = telemetry_state(
        handshake_info.battery_pct,
        handshake_info.tilt,
        handshake_info.roll,
        0.0,
        handshake_info.external_power,
        false,
    );
    init_state.insert("device_info".into(), handshake_info.device_info);
    emit_device_status(sender, ActorStatus::Connected, init_state);
    emit_ready_state(sender, false);

    // 3. Configure
    let mode_wire = SessionConfig::mode_label(&initial_mode);
    info!("configuring: mode={:?} wire={mode_wire}", initial_mode);
    client.configure(session_config, &initial_mode)?;

    // 4. Arm
    client.arm()?;
    let mut armed_state = telemetry_state(
        handshake_info.battery_pct,
        handshake_info.tilt,
        handshake_info.roll,
        0.0,
        handshake_info.external_power,
        true,
    );
    armed_state.insert("mode".into(), mode_wire.into());
    emit_device_status(&sender, ActorStatus::Connected, armed_state);
    emit_ready_state(sender, true);
    info!("armed -- mode={mode_wire} -- waiting for shots");

    // 5. Event loop
    let mut accumulator = ShotAccumulator::new();
    let mut current_mode = initial_mode;
    let mut consecutive_keepalive_failures: u32 = 0;
    const MAX_KEEPALIVE_FAILURES: u32 = 6; // ~10s at 900ms recv + 900ms keepalive per cycle

    loop {
        // Drain bus commands
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => {
                    client.shutdown();
                    return Ok(());
                }
                Ok(None) => break,
                Ok(Some(msg)) => match msg.event {
                    FlighthookEvent::ConfigChanged(changed) => {
                        session_config.apply_config_event(&changed.config);
                        info!("config updated: {:?}", changed.config);

                        emit_device_status(sender, ActorStatus::Starting, HashMap::new());
                        emit_ready_state(sender, false);
                        let mode_wire = SessionConfig::mode_label(&current_mode);
                        match client
                            .configure(session_config, &current_mode)
                            .and_then(|()| client.arm())
                        {
                            Ok(()) => {
                                emit_device_status(sender, ActorStatus::Connected, {
                                    let mut m = HashMap::new();
                                    m.insert("armed".into(), "true".into());
                                    m.insert("mode".into(), mode_wire.into());
                                    m
                                });
                                emit_ready_state(sender, true);
                            }
                            Err(ref e) if is_transient(e) => {
                                warn!("reconfigure failed (transient): {e}");
                                emit_alert(
                                    sender,
                                    AlertLevel::Warn,
                                    format!("Settings update failed: {e}"),
                                );
                                emit_device_status(sender, ActorStatus::Connected, {
                                    let mut m = HashMap::new();
                                    m.insert("armed".into(), "true".into());
                                    m.insert("mode".into(), mode_wire.into());
                                    m
                                });
                                emit_ready_state(sender, true);
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    FlighthookEvent::GameStateCommand(cmd) => match cmd.event {
                        GameStateCommandEvent::SetMode { mode: new_mode } => {
                            if !same_mode(current_mode, new_mode) {
                                let old = current_mode;
                                let new_wire = SessionConfig::mode_label(&new_mode);
                                info!("mode change: {old:?} -> {new_mode:?} wire={new_wire}");
                                emit_device_status(sender, ActorStatus::Starting, HashMap::new());
                                emit_ready_state(sender, false);
                                match change_mode(&mut client, new_mode, session_config) {
                                    Ok(()) => {
                                        current_mode = new_mode;
                                        info!("mode changed to {new_mode:?} wire={new_wire}");
                                        emit_device_status(sender, ActorStatus::Connected, {
                                            let mut m = HashMap::new();
                                            m.insert("armed".into(), "true".into());
                                            m.insert("mode".into(), new_wire.into());
                                            m
                                        });
                                        emit_ready_state(sender, true);
                                    }
                                    Err(ref e) if is_transient(e) => {
                                        warn!("mode change failed (transient): {e}");
                                        emit_alert(
                                            sender,
                                            AlertLevel::Warn,
                                            format!("Mode change failed: {e}"),
                                        );
                                        emit_device_status(sender, ActorStatus::Connected, {
                                            let mut m = HashMap::new();
                                            m.insert("armed".into(), "true".into());
                                            m.insert(
                                                "mode".into(),
                                                SessionConfig::mode_label(&current_mode).into(),
                                            );
                                            m
                                        });
                                        emit_ready_state(sender, true);
                                    }
                                    Err(e) => return Err(e),
                                }
                            }
                        }
                        GameStateCommandEvent::SetPlayerInfo { player_info } => {
                            info!("player handedness: {}", player_info.handed);
                        }
                        _ => {}
                    },
                    _ => {}
                },
            }
        }

        match client.recv_timeout(RECV_TIMEOUT) {
            Ok(env) => {
                consecutive_keepalive_failures = 0;
                let msg = &env.message;
                let label = msg_label(msg);

                // Build raw hex for bus payload (hex-first policy — always populate)
                let raw_hex = env.raw.clone();
                let debug_str = format!("{:?}", env.message);

                // Log raw payload to audit tracing target (filtered by RUST_LOG)
                {
                    let hex: String = raw_hex.iter().fold(
                        String::with_capacity(raw_hex.len() * 2),
                        |mut s, b| {
                            use std::fmt::Write;
                            let _ = write!(s, "{b:02X}");
                            s
                        },
                    );
                    info!(
                        target: "audit",
                        "{device_id} recv 0x{:02X} {hex} | {debug_str}",
                        env.type_id,
                    );
                }

                let was_active = accumulator.active;
                let auto_activated = accumulator.handle(msg);

                if was_active || accumulator.active {
                    if let Message::ShotText(st) = msg {
                        debug!("shot-cycle msg: ShotText(0xE5) \"{}\"", st.text);
                    } else {
                        debug!("shot-cycle msg: {label}");
                    }
                }

                if auto_activated {
                    info!("shot data before trigger text -- auto-started accumulator on {label}");
                }

                match msg {
                    Message::ShotText(st) if st.is_trigger() => {
                        emit_device_status(sender, ActorStatus::Connected, {
                            let mut m = HashMap::new();
                            m.insert("shooting".into(), "true".into());
                            m
                        });
                        emit_ready_state(sender, false);
                    }
                    Message::ShotText(st) if st.is_processed() => {
                        let had_flight = accumulator.has_flight();
                        let had_flight_v1 = accumulator.has_flight_v1();
                        let had_club = accumulator.has_club();
                        let had_spin = accumulator.has_spin();
                        match accumulator.finalize(device_id) {
                            Some(shot) => {
                                let ball_mph = shot.ball.launch_speed.as_mph();
                                let carry_yd = shot
                                    .ball
                                    .carry_distance
                                    .map(|d| d.as_yards())
                                    .unwrap_or(0.0);
                                let source = if had_flight { "D4" } else { "E8" };
                                info!(
                                    "shot #{} ({}): ball={:.1}mph VLA={:.1} HLA={:.1} carry={:.1}yd back={:.0}rpm side={:.0}rpm",
                                    shot.shot_number,
                                    source,
                                    ball_mph,
                                    shot.ball.launch_elevation,
                                    shot.ball.launch_azimuth,
                                    carry_yd,
                                    shot.ball.backspin_rpm.unwrap_or(0),
                                    shot.ball.sidespin_rpm.unwrap_or(0),
                                );
                                sender.send(
                                    FlighthookMessage::new(LaunchMonitorEvent::ShotResult {
                                        shot: Box::new(shot),
                                    })
                                    .raw_binary(raw_hex.clone()),
                                );
                            }
                            None => {
                                warn!(
                                    "trigger processed but no shot produced \
                                     (D4={had_flight}, E8={had_flight_v1}, club={had_club}, spin={had_spin})"
                                );
                                emit_alert(
                                    sender,
                                    AlertLevel::Warn,
                                    format!(
                                        "Shot triggered but no result produced (D4={had_flight}, E8={had_flight_v1}, club={had_club}, spin={had_spin})"
                                    ),
                                );
                            }
                        }

                        emit_device_status(sender, ActorStatus::Starting, HashMap::new());
                        loop {
                            match client.complete_shot() {
                                Ok(()) => break,
                                Err(ref e) if is_transient(e) => {
                                    info!("post-shot retry: {e}");
                                    continue;
                                }
                                Err(e) => return Err(e),
                            }
                        }
                        emit_device_status(sender, ActorStatus::Connected, {
                            let mut m = HashMap::new();
                            m.insert("armed".into(), "true".into());
                            m.insert(
                                "mode".into(),
                                SessionConfig::mode_label(&current_mode).into(),
                            );
                            m
                        });
                        emit_ready_state(sender, true);
                    }
                    _ => {}
                }
            }

            Err(ConnError::Timeout { .. }) => match client.keepalive() {
                Ok(status) => {
                    consecutive_keepalive_failures = 0;
                    let mut ts = telemetry_state(
                        status.battery_pct,
                        status.tilt,
                        status.roll,
                        status.temp_c,
                        status.external_power,
                        true,
                    );
                    ts.insert(
                        "mode".into(),
                        SessionConfig::mode_label(&current_mode).into(),
                    );
                    emit_device_status(sender, ActorStatus::Connected, ts);
                }
                Err(ref e) if is_transient(e) => {
                    consecutive_keepalive_failures += 1;
                    if consecutive_keepalive_failures >= MAX_KEEPALIVE_FAILURES {
                        warn!(
                            "keepalive failed {consecutive_keepalive_failures} times, assuming disconnected"
                        );
                        return Err(ConnError::Disconnected);
                    }
                    info!(
                        "keepalive: {e} ({consecutive_keepalive_failures}/{MAX_KEEPALIVE_FAILURES})"
                    );
                }
                Err(e) => return Err(e),
            },

            Err(ConnError::Disconnected) => {
                info!("device disconnected");
                return Err(ConnError::Disconnected);
            }
            Err(ref e @ ConnError::Wire(_)) | Err(ref e @ ConnError::Protocol(_)) => {
                warn!(message = "Could not process message", error = ?e);
                sender.send(FlighthookMessage::new(AlertMessage {
                    level: AlertLevel::Warn,
                    message: format!("Could not process message: {e:?}"),
                }));
            }
            Err(e) => return Err(e),
        }
    }
}
