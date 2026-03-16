pub mod settings;

pub use settings::SessionConfig;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ironsight::seq::ShotDatum;
use ironsight::{BinaryClient, BinaryConnection, BinaryEvent, ConnError, Message};
use tracing::{debug, info, warn};

use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;
use settings::cam_config;

use flighthook::{
    ActorStatus, BallFlight, ClubData,
    Distance, FlighthookEvent, FlighthookMessage, Severity, ShotDetectionMode, ShotKey, Velocity,
};

/// No events for this long → treat as disconnected.
const STALE_TIMEOUT: Duration = Duration::from_secs(10);

/// Keepalive cadence for BinaryClient.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(3);

/// Reconnect backoff bounds (linear: +1s per attempt, capped at 15s).
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(15);

/// TCP connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Mevo session actor. Connects to a Mevo/Mevo+ device, runs the handshake,
/// arms, and processes shots in a reconnecting event loop.
pub struct MevoActor {
    pub addr: SocketAddr,
    pub initial_mode: ShotDetectionMode,
    pub session_config: SessionConfig,
    pub use_estimated: bool,
}

impl Actor for MevoActor {
    fn start(&self, _state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let addr = self.addr;
        let initial_mode = self.initial_mode;
        let session_config = self.session_config.clone();
        let use_estimated = self.use_estimated;
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run(addr, initial_mode, session_config, use_estimated, sender, receiver);
            })
            .expect("failed to spawn mevo thread");
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        // Parse actor_id "mevo.0" -> ("mevo", "0")
        let actor_id = sender.actor_id();
        let (_, index) = match actor_id.split_once('.') {
            Some(pair) => pair,
            None => return ReconfigureOutcome::Applied,
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

        // Check if session config changed -> restart to apply
        let new_session = SessionConfig::from_mevo_section(section);
        if new_session != self.session_config {
            return ReconfigureOutcome::RestartRequired;
        }

        // Check if use_estimated changed -> restart to apply
        let new_use_estimated = section.use_estimated.unwrap_or(true);
        if new_use_estimated != self.use_estimated {
            return ReconfigureOutcome::RestartRequired;
        }

        ReconfigureOutcome::Applied
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn same_mode(a: ShotDetectionMode, b: ShotDetectionMode) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

fn emit_device_status(sender: &BusSender, status: ActorStatus, telemetry: HashMap<String, String>) {
    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status,
        telemetry,
    }));
}

fn emit_alert(sender: &BusSender, severity: Severity, message: impl Into<String>) {
    sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
        severity,
        message: message.into(),
    }));
}

fn stamp_device(msg: FlighthookMessage, device_id: &Option<String>) -> FlighthookMessage {
    match device_id {
        Some(dev) => msg.device(dev),
        None => msg,
    }
}

fn try_emit_device_telemetry(
    sender: &BusSender,
    device_id: &Option<String>,
    telemetry: &HashMap<String, String>,
) {
    let Some(dev) = device_id else { return };
    sender.send(
        FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
            manufacturer: None,
            model: None,
            firmware: None,
            telemetry: Some(telemetry.clone()),
        })
        .device(dev),
    );
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

fn run(
    addr: SocketAddr,
    initial_mode: ShotDetectionMode,
    initial_session_config: SessionConfig,
    use_estimated: bool,
    sender: BusSender,
    mut receiver: BusReceiver,
) {
    let mut backoff = MIN_BACKOFF;
    let mut session_config = initial_session_config;
    let mut ever_connected = false;
    // FRP device identity — set on first successful handshake (from SSID),
    // persists across reconnects. None until handshake completes; no
    // DeviceTelemetry is emitted before we have a device identity.
    let mut device_id: Option<String> = None;

    loop {
        if receiver.poll().is_err() {
            break;
        }

        emit_device_status(&sender, ActorStatus::Starting, HashMap::new());

        match connect_and_run(
            &addr,
            initial_mode,
            use_estimated,
            &sender,
            &mut receiver,
            &mut session_config,
            &mut ever_connected,
            &mut device_id,
        ) {
            Ok(()) => break,
            Err(e) => {
                warn!("session error: {e}");
                emit_alert(
                    &sender,
                    Severity::Warn,
                    format!("Device connection error: {e}"),
                );
                let backoff_status = if ever_connected {
                    ActorStatus::Reconnecting
                } else {
                    ActorStatus::Starting
                };
                emit_device_status(&sender, backoff_status, HashMap::new());
                if device_id.is_some() {
                    try_emit_device_telemetry(
                        &sender,
                        &device_id,
                        &HashMap::from([("ready".into(), "false".into())]),
                    );
                }

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
                backoff = (backoff + Duration::from_secs(1)).min(MAX_BACKOFF);
            }
        }
    }

    emit_device_status(&sender, ActorStatus::Disconnected, HashMap::new());
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn connect_and_run(
    addr: &SocketAddr,
    initial_mode: ShotDetectionMode,
    use_estimated: bool,
    sender: &BusSender,
    receiver: &mut BusReceiver,
    session_config: &mut SessionConfig,
    ever_connected: &mut bool,
    device_id: &mut Option<String>,
) -> Result<(), ConnError> {
    // 1. Connect
    info!("connecting to {addr}...");
    let mut conn = BinaryConnection::connect_timeout(addr, CONNECT_TIMEOUT)?;
    *ever_connected = true;

    // Set up send/recv audit logging callbacks
    let send_id = sender.actor_id().to_string();
    conn.set_on_send(move |cmd, dest| {
        tracing::debug!("{send_id} send >> {dest:?} {:?} [{}]", cmd, cmd.debug_hex(dest));
    });
    let audit_id = sender.actor_id().to_string();
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

    // Shot lifecycle state
    let mut shot_counter: u32 = 0;
    let mut current_shot_key: Option<ShotKey> = None;
    let mut shot_had_d4 = false;
    let mut stashed_e8: Option<ironsight::protocol::shot::FlightResultV1> = None;

    // Staleness + poll backoff
    let mut last_event_time = Instant::now();
    let mut idle_count: u32 = 0;

    // Device-reported telemetry cache (battery, tilt, roll, temp, ready, etc.).
    // Updated incrementally, always emitted in full so downstream consumers
    // never lose fields. Sent via DeviceTelemetry events.
    let mut device_telemetry: HashMap<String, String> = HashMap::new();

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
                Ok(Some(msg)) => if let FlighthookEvent::SetDetectionMode { mode: Some(new_mode), .. } = msg.event
                    && !same_mode(current_mode, new_mode) {
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
                            emit_device_status(sender, ActorStatus::Connected, HashMap::new());
                            device_telemetry.insert("ready".into(), "false".into());
                            try_emit_device_telemetry(sender, device_id, &device_telemetry);
                        } else {
                            pending_reconfig = Some(new_mode);
                            info!(
                                "deferred mode change to {:?} (operation in progress)",
                                new_mode
                            );
                        }
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
                        let info_text = format!("{} ({})", h.avr.dev_info.text, gen_label);
                        let battery_pct = h.dsp.status.battery_percent();
                        let external_power = h.dsp.status.external_power();
                        let tilt = h.avr.status.tilt;
                        let roll = -h.avr.status.roll;

                        // Capture SSID as FRP device identity
                        let ssid = h.pi.ssid.clone();
                        if !ssid.is_empty() {
                            *device_id = Some(ssid.clone());
                            info!("device identity: {ssid}");
                        }

                        info!(
                            "handshake complete: {info_text} | battery {battery_pct}%{} | tilt {tilt:.1} roll {roll:.1}",
                            if external_power { " (charging)" } else { "" },
                        );

                        // Emit identity DeviceTelemetry + initial device state
                        device_telemetry.insert("battery_pct".into(), battery_pct.to_string());
                        device_telemetry.insert("tilt".into(), format!("{tilt:.1}"));
                        device_telemetry.insert("roll".into(), format!("{roll:.1}"));
                        device_telemetry.insert("temp_c".into(), "0.0".into());
                        device_telemetry.insert("external_power".into(), external_power.to_string());
                        device_telemetry.insert("ready".into(), "false".into());

                        if let Some(dev) = device_id.as_deref() {
                            sender.send(
                                FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                    manufacturer: Some("FlightScope".into()),
                                    model: Some(h.avr.dev_info.text.clone()),
                                    firmware: None,
                                    telemetry: Some(device_telemetry.clone()),
                                })
                                .device(dev),
                            );
                        }

                        emit_device_status(sender, ActorStatus::Connected, HashMap::new());
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

                        emit_device_status(sender, ActorStatus::Connected, HashMap::new());
                        device_telemetry.insert("ready".into(), "true".into());
                        try_emit_device_telemetry(sender, device_id, &device_telemetry);

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
                            emit_device_status(sender, ActorStatus::Connected, HashMap::new());
                            device_telemetry.insert("ready".into(), "false".into());
                            try_emit_device_telemetry(sender, device_id, &device_telemetry);
                        }
                    }

                    BinaryEvent::Trigger => {
                        shot_counter += 1;
                        let key = ShotKey {
                            shot_id: uuid::Uuid::new_v4().to_string(),
                            shot_number: shot_counter,
                        };
                        debug!("ball trigger detected — shot_id={}", key.shot_id);
                        let msg = FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                            key: key.clone(),
                        });
                        sender.send(stamp_device(msg, device_id));
                        current_shot_key = Some(key);
                        shot_had_d4 = false;
                        stashed_e8 = None;
                        emit_device_status(sender, ActorStatus::Connected, HashMap::new());
                        device_telemetry.insert("ready".into(), "false".into());
                        try_emit_device_telemetry(sender, device_id, &device_telemetry);
                    }

                    BinaryEvent::ShotDatum(datum) => {
                        if let Some(ref key) = current_shot_key {
                            match datum {
                                ShotDatum::Flight(d4) => {
                                    shot_had_d4 = true;
                                    let ball = ball_from_d4(&d4);
                                    info!(
                                        "shot #{} (D4): ball={:.1}mph VLA={:.1} HLA={:.1} carry={:.1}yd back={:.0}rpm side={:.0}rpm",
                                        key.shot_number,
                                        ball.launch_speed.map(|v| v.as_mph()).unwrap_or(0.0),
                                        ball.launch_elevation.unwrap_or(0.0),
                                        ball.launch_azimuth.unwrap_or(0.0),
                                        ball.carry_distance.map(|d| d.as_yards()).unwrap_or(0.0),
                                        ball.backspin_rpm.unwrap_or(0),
                                        ball.sidespin_rpm.unwrap_or(0),
                                    );
                                    let msg = FlighthookMessage::new(
                                        FlighthookEvent::BallFlight {
                                            key: key.clone(),
                                            ball: Box::new(ball),
                                        },
                                    );
                                    sender.send(stamp_device(msg, device_id));
                                }
                                ShotDatum::FlightV1(e8) => {
                                    debug!("stashed E8 for fallback");
                                    stashed_e8 = Some(e8);
                                }
                                ShotDatum::Club(ed) => {
                                    let club = club_from_ed(&ed);
                                    let msg = FlighthookMessage::new(
                                        FlighthookEvent::ClubPath {
                                            key: key.clone(),
                                            club: Box::new(club),
                                        },
                                    );
                                    sender.send(stamp_device(msg, device_id));
                                }
                                ShotDatum::Spin(_) => {
                                    // Spin axis / total spin are derived from
                                    // backspin + sidespin in BallFlight; EF is redundant.
                                }
                            }
                        }
                    }

                    BinaryEvent::ShotComplete(_shot_data) => {
                        if let Some(key) = current_shot_key.take() {
                            // E8 fallback: emit BallFlight from stashed E8 if no D4
                            if !shot_had_d4 {
                                if use_estimated {
                                    if let Some(e8) = stashed_e8.take() {
                                        let ball = ball_from_e8(&e8);
                                        info!(
                                            "shot #{} (E8): ball={:.1}mph VLA={:.1} HLA={:.1} carry={:.1}yd",
                                            key.shot_number,
                                            ball.launch_speed.map(|v| v.as_mph()).unwrap_or(0.0),
                                            ball.launch_elevation.unwrap_or(0.0),
                                            ball.launch_azimuth.unwrap_or(0.0),
                                            ball.carry_distance.map(|d| d.as_yards()).unwrap_or(0.0),
                                        );
                                        let msg = FlighthookMessage::new(
                                            FlighthookEvent::BallFlight {
                                                key: key.clone(),
                                                ball: Box::new(ball),
                                            },
                                        );
                                        sender.send(stamp_device(msg, device_id));
                                    } else {
                                        warn!(
                                            "shot #{} processed but no flight result (D4 or E8)",
                                            key.shot_number,
                                        );
                                        emit_alert(
                                            sender,
                                            Severity::Warn,
                                            format!(
                                                "Shot #{} triggered but no flight result produced",
                                                key.shot_number,
                                            ),
                                        );
                                    }
                                } else {
                                    info!(
                                        "shot #{} had only E8 (estimated) — skipped (use_estimated=false)",
                                        key.shot_number,
                                    );
                                }
                            }
                            sender.send(stamp_device(
                                FlighthookMessage::new(FlighthookEvent::ShotFinished { key }),
                                device_id,
                            ));
                        }
                        stashed_e8 = None;
                        shot_had_d4 = false;

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
                            emit_device_status(sender, ActorStatus::Connected, HashMap::new());
                            device_telemetry.insert("ready".into(), "false".into());
                            try_emit_device_telemetry(sender, device_id, &device_telemetry);
                        }
                    }

                    BinaryEvent::Keepalive(status) => {
                        let mut changed = false;
                        if let Some(avr) = &status.avr {
                            device_telemetry.insert("tilt".into(), format!("{:.1}", avr.tilt));
                            device_telemetry.insert("roll".into(), format!("{:.1}", -avr.roll));
                            changed = true;
                        }
                        if let Some(dsp) = &status.dsp {
                            device_telemetry.insert("battery_pct".into(), dsp.battery_percent().to_string());
                            device_telemetry.insert("temp_c".into(), format!("{:.1}", dsp.temperature_c()));
                            device_telemetry.insert("external_power".into(), dsp.external_power().to_string());
                            changed = true;
                        }
                        if changed {
                            try_emit_device_telemetry(sender, device_id, &device_telemetry);
                        }
                    }

                    BinaryEvent::Message(env) => {
                        match &env.message {
                            Message::DspStatus(dsp) => {
                                device_telemetry.insert("battery_pct".into(), dsp.battery_percent().to_string());
                                device_telemetry.insert("temp_c".into(), format!("{:.1}", dsp.temperature_c()));
                                device_telemetry.insert("external_power".into(), dsp.external_power().to_string());
                                try_emit_device_telemetry(sender, device_id, &device_telemetry);
                            }
                            Message::AvrStatus(avr) => {
                                device_telemetry.insert("tilt".into(), format!("{:.1}", avr.tilt));
                                device_telemetry.insert("roll".into(), format!("{:.1}", -avr.roll));
                                try_emit_device_telemetry(sender, device_id, &device_telemetry);
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
                if idle_count == 1 || idle_count.is_multiple_of(200) {
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
                sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                    severity: Severity::Warn,
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

// ---------------------------------------------------------------------------
// Protocol type → bus type conversion helpers
// ---------------------------------------------------------------------------

fn ball_from_d4(d4: &ironsight::protocol::shot::FlightResult) -> BallFlight {
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(d4.launch_speed)),
        launch_elevation: Some(d4.launch_elevation),
        launch_azimuth: Some(d4.launch_azimuth),
        carry_distance: Some(Distance::Meters(d4.carry_distance)),
        total_distance: None,
        max_height: Some(Distance::Meters(d4.max_height)),
        flight_time: Some(d4.flight_time),
        roll_distance: None,
        backspin_rpm: Some(d4.backspin_rpm),
        sidespin_rpm: Some(d4.sidespin_rpm),
    }
}

fn ball_from_e8(e8: &ironsight::protocol::shot::FlightResultV1) -> BallFlight {
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(e8.ball_velocity)),
        launch_elevation: Some(e8.elevation),
        launch_azimuth: Some(e8.azimuth),
        carry_distance: Some(Distance::Meters(e8.distance)),
        total_distance: None,
        max_height: Some(Distance::Meters(e8.height)),
        flight_time: Some(e8.flight_time),
        roll_distance: None,
        backspin_rpm: Some(e8.backspin_rpm),
        sidespin_rpm: None, // always zero in E8 — None, not Some(0)
    }
}

fn club_from_ed(ed: &ironsight::protocol::shot::ClubResult) -> ClubData {
    ClubData {
        club_speed: Some(Velocity::MetersPerSecond(ed.pre_club_speed)),
        path: Some(ed.strike_direction),
        attack_angle: Some(ed.attack_angle),
        face_angle: Some(ed.face_angle),
        dynamic_loft: Some(ed.dynamic_loft),
        smash_factor: Some(ed.smash_factor),
        club_speed_post: Some(Velocity::MetersPerSecond(ed.post_club_speed)),
        swing_plane_horizontal: Some(ed.swing_plane_horizontal),
        swing_plane_vertical: Some(ed.swing_plane_vertical),
        club_offset: Some(Distance::Meters(ed.club_offset)),
        club_height: Some(Distance::Meters(ed.club_height)),
    }
}
