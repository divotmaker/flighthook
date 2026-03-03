pub(crate) mod accumulator;
pub mod settings;

pub use settings::SessionConfig;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ironsight::protocol::shot::FlightResultV1;
use ironsight::{BinaryClient, BinaryConnection, BinaryEvent, ConnError, Message};
use tracing::{debug, info, warn};

use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;
use accumulator::convert_shot;
use settings::cam_config;

use flighthook::{
    ActorState, ActorStatus, AlertLevel, AlertMessage, ConfigChanged, FlighthookEvent,
    FlighthookMessage, GameStateCommandEvent, LaunchMonitorEvent, ShotDetectionMode,
};

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
        Message::ConfigNack(_) => "ConfigNack(0x94)",
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

/// No events for this long → treat as disconnected.
const STALE_TIMEOUT: Duration = Duration::from_secs(10);

/// Keepalive cadence for BinaryClient.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(3);

/// Reconnect backoff bounds.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// TCP connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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

fn same_mode(a: ShotDetectionMode, b: ShotDetectionMode) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
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

                let deadline = Instant::now() + backoff;
                while Instant::now() < deadline {
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
    info!("connecting to {addr}...");
    let mut conn = BinaryConnection::connect_timeout(addr, CONNECT_TIMEOUT)?;
    *ever_connected = true;

    // Set up send/recv audit logging callbacks
    let send_id = device_id.to_string();
    conn.set_on_send(move |cmd, dest| {
        tracing::debug!("{send_id} send >> {dest:?} {:?} [{}]", cmd, cmd.debug_hex(dest));
    });
    let audit_id = device_id.to_string();
    conn.set_on_recv(move |env| {
        let hex: String =
            env.raw
                .iter()
                .fold(String::with_capacity(env.raw.len() * 2), |mut s, b| {
                    use std::fmt::Write;
                    let _ = write!(s, "{b:02X}");
                    s
                });
        tracing::debug!(
            "{audit_id} recv << 0x{:02X} {hex} | {:?}",
            env.type_id,
            env.message,
        );
    });

    // 2. Create non-blocking client
    let mut client = BinaryClient::from_tcp(conn)?;
    client.set_keepalive_interval(KEEPALIVE_INTERVAL);

    // 3. Enqueue startup operations
    let avr = session_config.to_avr_settings(&initial_mode);
    let cam = cam_config();
    client.handshake();
    client.configure_avr(avr);
    client.configure_cam(cam);
    client.arm();

    // 4. Event loop state
    let mut current_mode = initial_mode;
    let mut pending_reconfig: Option<ShotDetectionMode> = None;
    let mut stashed_e8: Option<FlightResultV1> = None;

    // Staleness + poll backoff
    let mut last_event_time = Instant::now();
    let mut idle_count: u32 = 0;

    // Persistent telemetry cache. Updated incrementally, always emitted
    // in full so downstream consumers never lose fields.
    let mut telemetry: HashMap<String, String> = HashMap::new();

    loop {
        // ==============================================================
        // Phase 1: Drain bus commands
        // ==============================================================
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => {
                    drop(client); // close TCP connection
                    return Ok(());
                }
                Ok(None) => break,
                Ok(Some(msg)) => match msg.event {
                    FlighthookEvent::ConfigChanged(changed) => {
                        session_config.apply_config_event(&changed.config);
                        info!("config updated: {:?}", changed.config);

                        if client.is_armed() {
                            let wire = SessionConfig::mode_label(&current_mode);
                            info!("reconfiguring device with updated settings (mode={wire})");
                            client.configure_avr(
                                session_config.to_avr_settings(&current_mode),
                            );
                            client.arm();
                            telemetry.insert("armed".into(), "false".into());
                            telemetry.insert("mode".into(), wire.into());
                            emit_device_status(sender, ActorStatus::Connected, telemetry.clone());
                            emit_ready_state(sender, false);
                        } else {
                            pending_reconfig = Some(current_mode);
                            info!("deferred settings reconfig (operation in progress)");
                        }
                    }
                    FlighthookEvent::GameStateCommand(cmd) => match cmd.event {
                        GameStateCommandEvent::SetMode { mode: new_mode } => {
                            if !same_mode(current_mode, new_mode) {
                                if client.is_armed() {
                                    let new_wire = SessionConfig::mode_label(&new_mode);
                                    info!(
                                        "mode change: {:?} -> {:?} wire={new_wire}",
                                        current_mode, new_mode
                                    );
                                    client.configure_avr(
                                        SessionConfig::to_avr_settings_mode_only(&new_mode),
                                    );
                                    client.arm();
                                    current_mode = new_mode;
                                    telemetry.insert("armed".into(), "false".into());
                                    telemetry.insert("mode".into(), new_wire.into());
                                    emit_device_status(sender, ActorStatus::Connected, telemetry.clone());
                                    emit_ready_state(sender, false);
                                } else {
                                    pending_reconfig = Some(new_mode);
                                    info!(
                                        "deferred mode change to {:?} (operation in progress)",
                                        new_mode
                                    );
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

        // ==============================================================
        // Phase 2: Drive BinaryClient
        // ==============================================================
        match client.poll() {
            Ok(Some(event)) => {
                last_event_time = Instant::now();
                idle_count = 0;
                debug!("poll -> {event:?}");

                match event {
                    BinaryEvent::Handshake(h) => {
                        let gen_label = h.dsp.hw_info.device_gen().label();
                        let device_info = format!("{} ({})", h.avr.dev_info.text, gen_label);
                        let battery_pct = h.dsp.status.battery_percent();
                        let external_power = h.dsp.status.external_power();
                        let tilt = h.avr.status.tilt;
                        let roll = -h.avr.status.roll;

                        info!(
                            "handshake complete: {device_info} | battery {battery_pct}%{} | tilt {tilt:.1} roll {roll:.1}",
                            if external_power { " (charging)" } else { "" },
                        );

                        telemetry.insert("device_info".into(), device_info);
                        telemetry.insert("battery_pct".into(), battery_pct.to_string());
                        telemetry.insert("tilt".into(), format!("{tilt:.1}"));
                        telemetry.insert("roll".into(), format!("{roll:.1}"));
                        telemetry.insert("temp_c".into(), "0.0".into());
                        telemetry.insert("external_power".into(), external_power.to_string());
                        telemetry.insert("armed".into(), "false".into());
                        telemetry.insert("shooting".into(), "false".into());
                        emit_device_status(sender, ActorStatus::Connected, telemetry.clone());
                        emit_ready_state(sender, false);
                    }

                    BinaryEvent::Disarmed => {
                        info!("device disarmed (preparing for reconfigure)");
                    }

                    BinaryEvent::Configured => {
                        let wire = SessionConfig::mode_label(&current_mode);
                        info!("device configured (mode={wire})");
                    }

                    BinaryEvent::Armed => {
                        let wire = SessionConfig::mode_label(&current_mode);
                        info!("armed -- mode={wire} -- waiting for shots");

                        telemetry.insert("armed".into(), "true".into());
                        telemetry.insert("shooting".into(), "false".into());
                        telemetry.insert("mode".into(), wire.into());
                        emit_device_status(sender, ActorStatus::Connected, telemetry.clone());
                        emit_ready_state(sender, true);

                        // Apply pending reconfig now that we're idle
                        if let Some(target) = pending_reconfig.take() {
                            let target_wire = SessionConfig::mode_label(&target);
                            let is_mode_change = !same_mode(current_mode, target);
                            if is_mode_change {
                                info!(
                                    "applying deferred mode change to {target:?} wire={target_wire}"
                                );
                            } else {
                                info!("applying deferred settings reconfig");
                            }
                            current_mode = target;
                            let avr = if is_mode_change {
                                SessionConfig::to_avr_settings_mode_only(&target)
                            } else {
                                session_config.to_avr_settings(&target)
                            };
                            client.configure_avr(avr);
                            client.arm();
                            telemetry.insert("armed".into(), "false".into());
                            telemetry.insert("mode".into(), target_wire.into());
                            emit_device_status(sender, ActorStatus::Connected, telemetry.clone());
                            emit_ready_state(sender, false);
                        }
                    }

                    BinaryEvent::Trigger => {
                        debug!("ball trigger detected");
                        telemetry.insert("shooting".into(), "true".into());
                        emit_device_status(sender, ActorStatus::Connected, telemetry.clone());
                        emit_ready_state(sender, false);
                    }

                    BinaryEvent::Shot(shot_data) => {
                        let had_d4 = shot_data.flight.is_some();
                        let had_e8 = stashed_e8.is_some();
                        let had_club = shot_data.club.is_some();
                        let had_spin = shot_data.spin.is_some();

                        match convert_shot(&shot_data, stashed_e8.as_ref(), device_id) {
                            Some(shot) => {
                                let ball_mph = shot.ball.launch_speed.as_mph();
                                let carry_yd = shot
                                    .ball
                                    .carry_distance
                                    .map(|d| d.as_yards())
                                    .unwrap_or(0.0);
                                let source = if had_d4 { "D4" } else { "E8" };
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
                                    }),
                                );
                            }
                            None => {
                                warn!(
                                    "shot processed but no result produced \
                                     (D4={had_d4}, E8={had_e8}, club={had_club}, spin={had_spin})"
                                );
                                emit_alert(
                                    sender,
                                    AlertLevel::Warn,
                                    format!(
                                        "Shot triggered but no result produced (D4={had_d4}, E8={had_e8}, club={had_club}, spin={had_spin})"
                                    ),
                                );
                            }
                        }

                        stashed_e8 = None;

                        // Device is auto-re-armed by BinaryClient's ShotSequencer.
                        // Apply pending reconfig if any.
                        if let Some(target) = pending_reconfig.take() {
                            let target_wire = SessionConfig::mode_label(&target);
                            let is_mode_change = !same_mode(current_mode, target);
                            if is_mode_change {
                                info!(
                                    "applying deferred mode change to {target:?} wire={target_wire}"
                                );
                            } else {
                                info!("applying deferred settings reconfig");
                            }
                            current_mode = target;
                            let avr = if is_mode_change {
                                SessionConfig::to_avr_settings_mode_only(&target)
                            } else {
                                session_config.to_avr_settings(&target)
                            };
                            client.configure_avr(avr);
                            client.arm();
                            telemetry.insert("armed".into(), "false".into());
                            telemetry.insert("mode".into(), target_wire.into());
                            emit_device_status(sender, ActorStatus::Connected, telemetry.clone());
                            emit_ready_state(sender, false);
                        }
                    }

                    BinaryEvent::Keepalive(status) => {
                        // Update avr fields first so emission has fresh tilt/roll
                        if let Some(avr) = &status.avr {
                            telemetry.insert("tilt".into(), format!("{:.1}", avr.tilt));
                            telemetry.insert("roll".into(), format!("{:.1}", -avr.roll));
                        }
                        if let Some(dsp) = &status.dsp {
                            telemetry.insert("battery_pct".into(), dsp.battery_percent().to_string());
                            telemetry.insert("temp_c".into(), format!("{:.1}", dsp.temperature_c()));
                            telemetry.insert("external_power".into(), dsp.external_power().to_string());
                            telemetry.insert("armed".into(), client.is_armed().to_string());
                            emit_device_status(sender, ActorStatus::Connected, telemetry.clone());
                        }
                    }

                    BinaryEvent::Message(env) => {
                        let label = msg_label(&env.message);

                        // Stash E8 for fallback when D4 is absent
                        if let Message::FlightResultV1(ref e8) = env.message {
                            stashed_e8 = Some(e8.clone());
                            debug!("stashed {label} for E8 fallback");
                        }

                        // Update telemetry cache from status messages
                        match &env.message {
                            Message::DspStatus(dsp) => {
                                telemetry.insert("battery_pct".into(), dsp.battery_percent().to_string());
                                telemetry.insert("temp_c".into(), format!("{:.1}", dsp.temperature_c()));
                                telemetry.insert("external_power".into(), dsp.external_power().to_string());
                                telemetry.insert("armed".into(), client.is_armed().to_string());
                                emit_device_status(sender, ActorStatus::Connected, telemetry.clone());
                            }
                            Message::AvrStatus(avr) => {
                                telemetry.insert("tilt".into(), format!("{:.1}", avr.tilt));
                                telemetry.insert("roll".into(), format!("{:.1}", -avr.roll));
                            }
                            _ => {}
                        }
                    }
                }
            }

            Ok(None) => {
                idle_count += 1;
                if idle_count >= 3 {
                    std::thread::sleep(Duration::from_millis(50));
                }
                if idle_count == 1 || idle_count % 200 == 0 {
                    debug!(
                        "poll -> None (idle={idle_count}, stale_in={:.1}s, armed={})",
                        STALE_TIMEOUT.saturating_sub(last_event_time.elapsed()).as_secs_f64(),
                        client.is_armed(),
                    );
                }
            }

            Err(ref e @ ConnError::Timeout) => {
                warn!("operation timeout: {e}");
                return Err(ConnError::Timeout);
            }
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

        // ==============================================================
        // Staleness check
        // ==============================================================
        if last_event_time.elapsed() >= STALE_TIMEOUT {
            warn!(
                "no events for {}s, assuming disconnected",
                STALE_TIMEOUT.as_secs()
            );
            return Err(ConnError::Disconnected);
        }
    }
}
