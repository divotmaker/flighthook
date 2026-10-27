pub mod settings;

pub use settings::SessionConfig;

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::FRAC_PI_2;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ironsight::protocol::handshake::{DeviceGen, DeviceModel};
use ironsight::protocol::shot::{FlightResult, FlightResultV1};
use ironsight::protocol::status::DspStatus;
use ironsight::protocol::{
    TYPE_AVR_CONFIG_QUERY, TYPE_AVR_CONFIG_RESP, TYPE_CAL_DATA_REQ, TYPE_CAL_DATA_RESP,
    TYPE_CAL_PARAM_REQ, TYPE_CAL_PARAM_RESP, TYPE_CLUB_PRC, TYPE_DEV_INFO_REQ, TYPE_DEV_INFO_RESP,
    TYPE_DSP_QUERY, TYPE_DSP_QUERY_RESP, TYPE_FLIGHT_RESULT, TYPE_NET_CONFIG, TYPE_PRC_DATA,
    TYPE_PROD_INFO, TYPE_SENSOR_ACT, TYPE_SENSOR_ACT_RESP, TYPE_SPEED_PROFILE, TYPE_STATUS,
    TYPE_TIME_SYNC, TYPE_WIFI_SCAN,
};
use ironsight::seq::ShotDatum;
use ironsight::{BinaryClient, BinaryConnection, BinaryEvent, ConnError, Message};
use tracing::{debug, info, warn};

use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::raw_capture::{self, RawCapture, RawCaptureConfig};
use crate::sim;
use crate::state::SystemState;
use settings::{cam_config, fusion_cam_config};

use flighthook::{
    ActorStatus, BallFlight, CameraMode, ClubData, Distance, FlighthookEvent, FlighthookMessage,
    RawDir, RawRecord, Severity, ShotDetectionMode, ShotKey, SpinSource, Velocity, hex_lower,
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

/// How long the Pi camera subsystem needs in standard mode before it will
/// accept a Fusion config. Applying one earlier leaves the SHM buffer
/// unallocated and no club data arrives.
const CAMERA_WARMUP: Duration = Duration::from_secs(15);

/// Message types never captured for the shot export, in either direction.
/// They carry device identity (status: hardware and application IDs; device
/// and product info: serial numbers; DSP query: hardware info), network
/// details (network config: SSID and password; WiFi scan: nearby networks),
/// unit-specific factory calibration, the sensor activation exchange (a
/// per-device certificate), or handshake housekeeping (time sync, AVR config
/// query). Status is also the keepalive, which has no shot content.
const EXPORT_DENYLIST: [u8; 17] = [
    TYPE_STATUS,
    TYPE_DEV_INFO_REQ,
    TYPE_DEV_INFO_RESP,
    TYPE_PROD_INFO,
    TYPE_DSP_QUERY,
    TYPE_DSP_QUERY_RESP,
    TYPE_NET_CONFIG,
    TYPE_WIFI_SCAN,
    TYPE_CAL_PARAM_REQ,
    TYPE_CAL_PARAM_RESP,
    TYPE_CAL_DATA_REQ,
    TYPE_CAL_DATA_RESP,
    TYPE_TIME_SYNC,
    TYPE_AVR_CONFIG_QUERY,
    TYPE_AVR_CONFIG_RESP,
    TYPE_SENSOR_ACT,
    TYPE_SENSOR_ACT_RESP,
];

/// Bulk per-shot message types (PRC paging, club PRC, speed profile),
/// charged to the per-shot byte budget instead of the frame cap.
const EXPORT_BULK: [u8; 3] = [TYPE_PRC_DATA, TYPE_CLUB_PRC, TYPE_SPEED_PROFILE];

/// FlightScope session actor. Connects to a Mevo/Mevo+ or Xi, runs the
/// handshake, arms, and processes shots in a reconnecting event loop.
///
/// The model is detected by ironsight during the handshake. The Xi has no
/// camera, so camera modes do not apply to it, and it runs with fixed
/// settings: the environment's detection mode and the radar parameters
/// stored on the device.
///
/// `spin_source` picks the Xi's spin. The run loop reads it from the live
/// config on every shot; the field is the value at start, used if the
/// section cannot be read.
pub struct MevoActor {
    pub addr: SocketAddr,
    pub initial_mode: ShotDetectionMode,
    pub session_config: SessionConfig,
    pub use_estimated: bool,
    pub camera_mode: CameraMode,
    pub spin_source: SpinSource,
}

impl Actor for MevoActor {
    fn start(&self, state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let addr = self.addr;
        let initial_mode = self.initial_mode;
        let session_config = self.session_config.clone();
        let use_estimated = self.use_estimated;
        let camera_mode = self.camera_mode;
        let spin_source = self.spin_source;
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run(
                    &state,
                    addr,
                    initial_mode,
                    session_config,
                    use_estimated,
                    camera_mode,
                    spin_source,
                    sender,
                    receiver,
                );
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
        let new_use_estimated = section.use_estimated.unwrap_or(false);
        if new_use_estimated != self.use_estimated {
            return ReconfigureOutcome::RestartRequired;
        }

        // Camera mode is sent once per session -> restart to apply
        if section.camera_mode.unwrap_or_default() != self.camera_mode {
            return ReconfigureOutcome::RestartRequired;
        }

        // Spin source is read from the live config on every shot, so a
        // change applies on the next shot without a restart.
        ReconfigureOutcome::Applied
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn same_mode(a: ShotDetectionMode, b: ShotDetectionMode) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

/// Detection mode label as sent to the device.
fn wire_label(
    model: DeviceModel,
    session_config: &SessionConfig,
    mode: &ShotDetectionMode,
) -> &'static str {
    if model == DeviceModel::Xi {
        session_config.environment_label()
    } else {
        session_config.mode_label(mode)
    }
}

/// Whether a D4 is the device's "distance model failed" result: launch speed
/// 0 (or not a number). This happens in full-flight outdoor mode when the
/// ball hits a net or screen; the E8 that came before it is still valid.
fn d4_model_failed(d4: &FlightResult) -> bool {
    d4.launch_speed.is_nan() || d4.launch_speed <= 0.0
}

fn hex_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02X}");
            s
        })
}

/// Product name and firmware for display and the shot export.
///
/// The name comes from the detected model and, for a Mevo, the DSP hardware
/// generation: `"Mevo+"`, `"Mevo Gen2"`, `"Mevo"` when the generation is
/// unknown, or `"Xi"`. `avr_info` is the AVR device info text: a version, an
/// internal hardware code, then the build date and time, e.g.
/// `"1.23   AB CD 01 Feb  3 2024 10:00:00"`. Firmware is the version and the
/// build date, `"1.23 (Feb 3 2024)"`; either part is left out when it is not
/// found. The hardware code and build time are not used.
fn product_and_firmware(
    model: DeviceModel,
    device_gen: Option<DeviceGen>,
    avr_info: &str,
) -> (String, Option<String>) {
    let product = match (model, device_gen) {
        (DeviceModel::Xi, _) => "Xi",
        (DeviceModel::Mevo, Some(g @ (DeviceGen::MevoPlus | DeviceGen::Gen2))) => g.label(),
        _ => model.label(),
    };

    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let tokens: Vec<&str> = avr_info.split_whitespace().collect();
    let version = tokens.first().copied().filter(|t| {
        t.starts_with(|c: char| c.is_ascii_digit())
            && t.chars().all(|c| c.is_ascii_digit() || c == '.')
    });
    let date = tokens.windows(3).find_map(|w| {
        let day_ok = w[1].len() <= 2 && w[1].parse::<u8>().is_ok_and(|d| (1..=31).contains(&d));
        let year_ok = w[2].len() == 4 && w[2].chars().all(|c| c.is_ascii_digit());
        (MONTHS.contains(&w[0]) && day_ok && year_ok).then(|| format!("{} {} {}", w[0], w[1], w[2]))
    });
    let firmware = match (version, date) {
        (Some(v), Some(d)) => Some(format!("{v} ({d})")),
        (Some(v), None) => Some(v.to_string()),
        (None, Some(d)) => Some(format!("built {d}")),
        (None, None) => None,
    };
    (product.to_string(), firmware)
}

fn export_excluded(rec: &RawRecord) -> bool {
    raw_capture::type_byte(rec).is_none_or(|t| EXPORT_DENYLIST.contains(&t))
}

fn export_bulk(rec: &RawRecord) -> bool {
    raw_capture::type_byte(rec).is_some_and(|t| EXPORT_BULK.contains(&t))
}

/// Raw capture for one FlightScope connection: the shot window runs from
/// BALL TRIGGER to the device re-armed, both directions.
fn raw_capture_config() -> RawCaptureConfig {
    RawCaptureConfig {
        transport: "flightscope-tcp",
        pre_roll_frames: 8,
        max_frames: 256,
        max_bulk_bytes: 128 * 1024,
        exclude: export_excluded,
        is_bulk: export_bulk,
    }
}

/// Install the send/receive hooks: hex audit logging, raw capture for the
/// shot export, and the latest D4 payload (for logging a D4 that is dropped).
fn attach_raw_capture<S: Read + Write>(
    conn: &mut BinaryConnection<S>,
    actor_id: &str,
    capture: &Rc<RefCell<RawCapture>>,
) -> Rc<RefCell<Vec<u8>>> {
    let send_id = actor_id.to_string();
    let send_capture = Rc::clone(capture);
    conn.set_on_send(move |cmd, dest| {
        tracing::debug!(
            "{send_id} send >> {dest:?} {:?} [{}]",
            cmd,
            cmd.debug_hex(dest)
        );
        let frame = cmd.encode(dest);
        if EXPORT_DENYLIST.contains(&frame.type_id) {
            return;
        }
        send_capture.borrow_mut().push(RawRecord {
            r#type: Some(raw_capture::type_label(frame.type_id)),
            src: Some(frame.src.to_string()),
            dst: Some(dest.to_string()),
            ..RawRecord::binary(RawDir::Tx, &frame.payload)
        });
    });

    let audit_id = actor_id.to_string();
    let last_d4_raw: Rc<RefCell<Vec<u8>>> = Rc::default();
    let d4_raw_sink = Rc::clone(&last_d4_raw);
    let recv_capture = Rc::clone(capture);
    conn.set_on_recv(move |env| {
        if env.type_id == TYPE_FLIGHT_RESULT {
            d4_raw_sink.replace(env.raw.clone());
        }
        tracing::debug!(
            "{audit_id} recv << 0x{:02X} {} | {:?}",
            env.type_id,
            hex_string(&env.raw),
            env.message,
        );
        if EXPORT_DENYLIST.contains(&env.type_id) {
            return;
        }
        recv_capture.borrow_mut().push(RawRecord {
            t_ms: 0,
            dir: RawDir::Rx,
            r#type: Some(raw_capture::type_label(env.type_id)),
            src: Some(env.src.to_string()),
            dst: Some("APP".into()),
            channel: None,
            hex: Some(hex_lower(&env.raw)),
            text: None,
        });
    });
    last_d4_raw
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

/// Battery level from a DSP status, or `None` when the status layout has no
/// decoded battery field (the raw variant used by the Mevo Gen2 and the Xi).
fn dsp_battery_pct(dsp: &DspStatus) -> Option<u8> {
    match dsp {
        DspStatus::V80(_) => Some(dsp.battery_percent()),
        DspStatus::V46(_) => None,
    }
}

/// Set `battery_pct` from a DSP status, removing the key when the battery
/// level is unknown so it is never reported as 0 %.
fn set_battery_telemetry(telemetry: &mut HashMap<String, String>, dsp: &DspStatus) {
    match dsp_battery_pct(dsp) {
        Some(pct) => {
            telemetry.insert("battery_pct".into(), pct.to_string());
        }
        None => {
            telemetry.remove("battery_pct");
        }
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

#[allow(clippy::too_many_arguments)]
fn run(
    state: &SystemState,
    addr: SocketAddr,
    initial_mode: ShotDetectionMode,
    initial_session_config: SessionConfig,
    use_estimated: bool,
    camera_mode: CameraMode,
    spin_source: SpinSource,
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
            state,
            &addr,
            initial_mode,
            use_estimated,
            camera_mode,
            spin_source,
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
    state: &SystemState,
    addr: &SocketAddr,
    initial_mode: ShotDetectionMode,
    use_estimated: bool,
    camera_mode: CameraMode,
    initial_spin_source: SpinSource,
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

    // Audit logging, raw capture for the shot export, and the latest D4.
    let capture = Rc::new(RefCell::new(RawCapture::new(raw_capture_config())));
    let last_d4_raw = attach_raw_capture(&mut conn, sender.actor_id(), &capture);

    // 2. Create non-blocking client
    let mut client = BinaryClient::from_tcp(conn)?;
    client.set_keepalive_interval(KEEPALIVE_INTERVAL);

    // 3. Enqueue the handshake. Configuration depends on the model it
    //    detects, so it is enqueued when the handshake completes.
    client.handshake();

    // 4. Event loop state
    let mut model = DeviceModel::Mevo;
    let mut current_mode = initial_mode;
    let mut pending_reconfig: Option<ShotDetectionMode> = None;

    // Shot lifecycle state
    let mut shot_counter: u32 = 0;
    let mut current_shot_key: Option<ShotKey> = None;
    let mut shot_had_d4 = false;
    let mut stashed_e8: Option<FlightResultV1> = None;
    // Detection mode in effect when the current shot's trigger fired, for
    // the `Auto` spin source and the ground-ball surface. `current_mode` can
    // change mid-shot; this doesn't.
    let mut shot_detection_mode: Option<ShotDetectionMode> = None;

    // Fusion camera config, applied once warmup has elapsed. None for
    // Standard, which keeps the warmup config for the whole session.
    let mut pending_fusion_cam = fusion_cam_config(camera_mode);
    let session_start = Instant::now();

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
                Ok(Some(msg)) => {
                    if let FlighthookEvent::SetDetectionMode {
                        mode: Some(new_mode),
                        ..
                    } = msg.event
                        && !same_mode(current_mode, new_mode)
                    {
                        if model == DeviceModel::Xi {
                            info!(
                                "mode change: {:?} -> {:?} not applied -- FlightScope Xi stays in {}",
                                current_mode,
                                new_mode,
                                session_config.environment_label()
                            );
                            current_mode = new_mode;
                        } else if client.is_armed() {
                            let new_wire = session_config.mode_label(&new_mode);
                            info!(
                                "mode change: {:?} -> {:?} wire={new_wire}",
                                current_mode, new_mode
                            );
                            client
                                .configure_avr(session_config.to_avr_settings_mode_only(&new_mode));
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
                    }
                }
            }
        }

        // ==============================================================
        // Phase 1b: Apply the Fusion camera config after warmup
        // ==============================================================
        // The device boots the camera in standard mode; only then will it
        // accept a Fusion config. Held back until the shot in progress (if
        // any) has finished so the reconfigure cannot interrupt it.
        if pending_fusion_cam.is_some()
            && current_shot_key.is_none()
            && session_start.elapsed() >= CAMERA_WARMUP
            && let Some(cam) = pending_fusion_cam.take()
        {
            info!(
                "camera warmup complete -- applying {} config",
                camera_mode.key()
            );
            client.configure_cam(cam);
            client.arm();
            emit_device_status(sender, ActorStatus::Connected, HashMap::new());
            device_telemetry.insert("ready".into(), "false".into());
            try_emit_device_telemetry(sender, device_id, &device_telemetry);
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
                        model = h.model;
                        let gen_label = match h.model {
                            DeviceModel::Xi => h.model.to_string(),
                            _ => h
                                .dsp
                                .hw_info
                                .as_ref()
                                .map_or("Unknown", |hw| hw.device_gen().label())
                                .to_string(),
                        };
                        let info_text = format!("{} ({})", h.avr.dev_info.text, gen_label);
                        let battery = dsp_battery_pct(&h.dsp.status)
                            .map_or_else(|| "unknown".to_string(), |pct| format!("{pct}%"));
                        let external_power = h.dsp.status.external_power();
                        let tilt = h.avr.status.tilt;
                        let roll = -h.avr.status.roll;

                        // FRP device identity: the WiFi SSID reported by the
                        // camera Pi. The Xi has no Pi, so it is identified by
                        // model and address.
                        let identity = match h.pi {
                            Some(ref pi) => pi.ssid.clone(),
                            None => format!("{} ({addr})", h.model),
                        };
                        if !identity.is_empty() {
                            *device_id = Some(identity.clone());
                            info!("device identity: {identity}");
                        }

                        info!(
                            "handshake complete: {info_text} | battery {battery}{} | tilt {tilt:.1} roll {roll:.1}",
                            if external_power { " (charging)" } else { "" },
                        );

                        // Emit identity DeviceTelemetry + initial device state
                        set_battery_telemetry(&mut device_telemetry, &h.dsp.status);
                        device_telemetry.insert("tilt".into(), format!("{tilt:.1}"));
                        device_telemetry.insert("roll".into(), format!("{roll:.1}"));
                        device_telemetry.insert("temp_c".into(), "0.0".into());
                        device_telemetry
                            .insert("external_power".into(), external_power.to_string());
                        device_telemetry.insert("ready".into(), "false".into());

                        // Product name and AVR firmware. Never the DSP
                        // device info: it carries the serial, and these
                        // values are published in shot exports.
                        let (device_model, firmware) = product_and_firmware(
                            h.model,
                            h.dsp.hw_info.as_ref().map(|hw| hw.device_gen()),
                            &h.avr.dev_info.text,
                        );
                        if let Some(dev) = device_id.as_deref() {
                            sender.send(
                                FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                    manufacturer: Some("FlightScope".into()),
                                    model: Some(device_model),
                                    firmware,
                                    telemetry: Some(device_telemetry.clone()),
                                })
                                .device(dev),
                            );
                        }

                        let status_telemetry = match h.model {
                            DeviceModel::Xi => {
                                HashMap::from([("device_info".into(), h.model.to_string())])
                            }
                            _ => HashMap::new(),
                        };
                        emit_device_status(sender, ActorStatus::Connected, status_telemetry);

                        // Configure and arm for the detected model.
                        if h.model == DeviceModel::Xi {
                            info!(
                                "FlightScope Xi (alpha): detection mode fixed to {} (environment); \
                                 radar settings and camera mode are not applied",
                                session_config.environment_label()
                            );
                            pending_fusion_cam = None;
                            client.configure_avr(session_config.to_xi_avr_settings());
                        } else {
                            client.configure_avr(session_config.to_avr_settings(&current_mode));
                            client.configure_cam(cam_config());
                        }
                        client.arm();
                    }

                    BinaryEvent::Disarmed => {
                        info!("device disarmed (preparing for reconfigure)");
                    }

                    BinaryEvent::Configured => {
                        let wire = wire_label(model, session_config, &current_mode);
                        info!("device configured (mode={wire})");
                    }

                    BinaryEvent::Armed => {
                        let wire = wire_label(model, session_config, &current_mode);
                        info!("armed -- mode={wire} -- waiting for shots");

                        emit_device_status(sender, ActorStatus::Connected, HashMap::new());
                        device_telemetry.insert("ready".into(), "true".into());
                        try_emit_device_telemetry(sender, device_id, &device_telemetry);

                        // Apply pending reconfig now that we're idle
                        if model == DeviceModel::Xi
                            && let Some(target) = pending_reconfig.take()
                        {
                            info!(
                                "deferred mode change to {target:?} not applied -- FlightScope Xi stays in {}",
                                session_config.environment_label()
                            );
                            current_mode = target;
                        } else if let Some(target) = pending_reconfig.take() {
                            let target_wire = session_config.mode_label(&target);
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
                                session_config.to_avr_settings_mode_only(&target)
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
                        capture.borrow_mut().mark_trigger();
                        let msg = FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                            key: key.clone(),
                        });
                        sender.send(stamp_device(msg, device_id));
                        current_shot_key = Some(key);
                        shot_had_d4 = false;
                        stashed_e8 = None;
                        shot_detection_mode = Some(current_mode);
                        emit_device_status(sender, ActorStatus::Connected, HashMap::new());
                        device_telemetry.insert("ready".into(), "false".into());
                        try_emit_device_telemetry(sender, device_id, &device_telemetry);
                    }

                    BinaryEvent::ShotDatum(datum) => {
                        if let Some(ref key) = current_shot_key {
                            match datum {
                                ShotDatum::Flight(d4) if d4_model_failed(&d4) => {
                                    // The device's distance model failed (e.g.
                                    // full-flight outdoor mode into a screen).
                                    // Treat the shot as having no D4 so the E8
                                    // fallback applies.
                                    warn!(
                                        "shot #{} D4 dropped: launch speed {} (distance model failed) -- 0x{:02X} {}",
                                        key.shot_number,
                                        d4.launch_speed,
                                        TYPE_FLIGHT_RESULT,
                                        hex_string(&last_d4_raw.borrow()),
                                    );
                                }
                                ShotDatum::Flight(d4) => {
                                    shot_had_d4 = true;
                                    let mut ball = ball_from_d4(&d4);
                                    let mut spin_source = "D4";
                                    let mode = shot_mode(shot_detection_mode, current_mode);
                                    if model == DeviceModel::Xi {
                                        let source = live_spin_source(state, sender.actor_id())
                                            .unwrap_or(initial_spin_source);
                                        let (spin, label) =
                                            xi_spin(source, mode, &d4, stashed_e8.as_ref());
                                        if let Some((back, side)) = spin {
                                            ball.backspin_rpm = Some(back);
                                            ball.sidespin_rpm = Some(side);
                                        }
                                        spin_source = label;
                                    }
                                    info!(
                                        "shot #{} (D4): ball={:.1}mph VLA={:.1} HLA={:.1} carry={:.1}yd back={:.0}rpm side={:.0}rpm spin={spin_source}",
                                        key.shot_number,
                                        ball.launch_speed.map(|v| v.as_mph()).unwrap_or(0.0),
                                        ball.launch_elevation.unwrap_or(0.0),
                                        ball.launch_azimuth.unwrap_or(0.0),
                                        ball.carry_distance.map(|d| d.as_yards()).unwrap_or(0.0),
                                        ball.backspin_rpm.unwrap_or(0),
                                        ball.sidespin_rpm.unwrap_or(0),
                                    );
                                    let msg =
                                        FlighthookMessage::new(sim::ball_flight_event_in_mode(
                                            state,
                                            key.clone(),
                                            ball,
                                            Some(mode),
                                        ));
                                    sender.send(stamp_device(msg, device_id));
                                }
                                ShotDatum::FlightV1(e8) => {
                                    debug!("stashed E8 for fallback");
                                    stashed_e8 = Some(e8);
                                }
                                ShotDatum::Club(ed) => {
                                    let club = club_from_ed(&ed);
                                    let msg = FlighthookMessage::new(FlighthookEvent::ClubPath {
                                        key: key.clone(),
                                        club: Box::new(club),
                                    });
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
                        let raw = capture.borrow_mut().take();
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
                                            ball.carry_distance
                                                .map(|d| d.as_yards())
                                                .unwrap_or(0.0),
                                        );
                                        let mode = shot_mode(shot_detection_mode, current_mode);
                                        let msg =
                                            FlighthookMessage::new(sim::ball_flight_event_in_mode(
                                                state,
                                                key.clone(),
                                                ball,
                                                Some(mode),
                                            ));
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
                                FlighthookMessage::new(FlighthookEvent::ShotFinished {
                                    key,
                                    raw: Some(Box::new(raw)),
                                }),
                                device_id,
                            ));
                        }
                        stashed_e8 = None;
                        shot_had_d4 = false;

                        // Device is auto-re-armed by BinaryClient's ShotSequencer,
                        // which reports the completed shot rather than an Armed
                        // event, so readiness is restored here. Without this the
                        // device reads not-ready for the rest of the session, from
                        // the first trigger onwards.
                        device_telemetry.insert("ready".into(), "true".into());
                        try_emit_device_telemetry(sender, device_id, &device_telemetry);

                        // Apply pending reconfig if any.
                        if model == DeviceModel::Xi
                            && let Some(target) = pending_reconfig.take()
                        {
                            info!(
                                "deferred mode change to {target:?} not applied -- FlightScope Xi stays in {}",
                                session_config.environment_label()
                            );
                            current_mode = target;
                        } else if let Some(target) = pending_reconfig.take() {
                            let target_wire = session_config.mode_label(&target);
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
                                session_config.to_avr_settings_mode_only(&target)
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
                            set_battery_telemetry(&mut device_telemetry, dsp);
                            device_telemetry
                                .insert("temp_c".into(), format!("{:.1}", dsp.temperature_c()));
                            device_telemetry
                                .insert("external_power".into(), dsp.external_power().to_string());
                            changed = true;
                        }
                        if changed {
                            try_emit_device_telemetry(sender, device_id, &device_telemetry);
                        }
                    }

                    BinaryEvent::Message(env) => match &env.message {
                        Message::DspStatus(dsp) => {
                            set_battery_telemetry(&mut device_telemetry, dsp);
                            device_telemetry
                                .insert("temp_c".into(), format!("{:.1}", dsp.temperature_c()));
                            device_telemetry
                                .insert("external_power".into(), dsp.external_power().to_string());
                            try_emit_device_telemetry(sender, device_id, &device_telemetry);
                        }
                        Message::AvrStatus(avr) => {
                            device_telemetry.insert("tilt".into(), format!("{:.1}", avr.tilt));
                            device_telemetry.insert("roll".into(), format!("{:.1}", -avr.roll));
                            try_emit_device_telemetry(sender, device_id, &device_telemetry);
                        }
                        _ => {}
                    },
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
                        STALE_TIMEOUT
                            .saturating_sub(last_event_time.elapsed())
                            .as_secs_f64(),
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

fn ball_from_d4(d4: &FlightResult) -> BallFlight {
    // DSP→PC negation: wire neg=right/slice, FRP pos=right/slice.
    let flew = flew(d4.carry_distance, d4.flight_time, d4.max_height);
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(d4.launch_speed)),
        launch_elevation: Some(d4.launch_elevation),
        launch_azimuth: Some(-d4.launch_azimuth),
        carry_distance: Some(Distance::Meters(d4.carry_distance)),
        carry_offline: flew
            .then(|| finite(-d4.landing_position[2]))
            .flatten()
            .map(Distance::Meters),
        max_height: Some(Distance::Meters(d4.max_height)),
        flight_time: Some(d4.flight_time),
        descent_angle: flew.then(|| descent_angle(d4.landing_velocity)).flatten(),
        backspin_rpm: Some(d4.backspin_rpm),
        sidespin_rpm: Some(-d4.sidespin_rpm),
        ..BallFlight::default()
    }
}

/// Whether a flight result describes a ball in flight: positive, finite
/// carry, flight time and apex. A ball that stayed on the ground has no
/// landing, so no landing offline or descent angle.
fn flew(carry: f64, flight_time: f64, apex: f64) -> bool {
    [carry, flight_time, apex]
        .iter()
        .all(|v| v.is_finite() && *v > 0.0)
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// Angle below horizontal (deg) of the D4 landing velocity
/// `[forward, vertical, lateral]`, or `None` when the ball is not descending.
fn descent_angle(v: [f64; 3]) -> Option<f64> {
    let [forward, vertical, lateral] = v;
    let horizontal = forward.hypot(lateral);
    let angle = (-vertical).atan2(horizontal).to_degrees();
    (vertical < 0.0 && angle.is_finite()).then_some(angle)
}

/// The `[mevo.N]` spin source for the actor `actor_id` (`"mevo.N"`), from
/// the live config.
fn live_spin_source(state: &SystemState, actor_id: &str) -> Option<SpinSource> {
    let (_, index) = actor_id.split_once('.')?;
    state.system.mevo_spin_source(index)
}

/// Xi spin for `source`: the wire-signed (backspin, FRP sidespin) to use
/// instead of D4's, or `None` to keep D4's, plus a label naming the source
/// for the shot log.
///
/// `D4` keeps D4. `E8` takes the total spin from the E8 backspin. The spin
/// axis comes from D4 only when the shot carries club data (club speed plus
/// attack angle or path) and D4's backspin is positive; D4's axis then
/// follows face-to-path. Otherwise the axis is 0. The axis is clamped to ±90°
/// so backspin never turns negative. `E8` keeps D4 when there is no E8
/// backspin, since that is a missing value, not a measurement. Sidespin keeps
/// the DSP→PC negation of [`ball_from_d4`].
///
/// `Auto` applies the `E8` rule when `mode` (flighthook's detection mode at
/// shot time) is `Full`, and otherwise keeps D4 exactly like `D4`. Either
/// way, the label is the one the underlying rule would have used, with
/// " (auto)" appended.
fn xi_spin(
    source: SpinSource,
    mode: ShotDetectionMode,
    d4: &FlightResult,
    e8: Option<&FlightResultV1>,
) -> (Option<(i32, i32)>, &'static str) {
    match source {
        SpinSource::D4 => (None, "D4"),
        SpinSource::E8 => xi_spin_e8(d4, e8),
        SpinSource::Auto if mode == ShotDetectionMode::Full => {
            let (spin, label) = xi_spin_e8(d4, e8);
            (spin, auto_label(label))
        }
        SpinSource::Auto => (None, "D4 (auto)"),
    }
}

/// Detection mode in effect for a shot: the mode captured at its trigger,
/// else the current one. It picks the Xi `Auto` spin source and the
/// surface a ground ball rolls on.
fn shot_mode(
    at_trigger: Option<ShotDetectionMode>,
    current: ShotDetectionMode,
) -> ShotDetectionMode {
    at_trigger.unwrap_or(current)
}

/// The `E8` rule: total spin from the E8 backspin, with the spin axis from
/// D4 when the shot has club data, falling back to D4 when there is no E8
/// backspin.
fn xi_spin_e8(
    d4: &FlightResult,
    e8: Option<&FlightResultV1>,
) -> (Option<(i32, i32)>, &'static str) {
    let Some(e8) = e8.filter(|e8| e8.backspin_rpm > 0) else {
        return (None, "D4 (no E8 spin)");
    };
    let total = f64::from(e8.backspin_rpm);
    let has_club_data =
        d4.clubhead_speed > 0.0 && (d4.club_attack_angle != 0.0 || d4.club_strike_direction != 0.0);
    let axis = if has_club_data && d4.backspin_rpm > 0 {
        f64::from(d4.sidespin_rpm)
            .atan2(f64::from(d4.backspin_rpm))
            .clamp(-FRAC_PI_2, FRAC_PI_2)
    } else {
        0.0
    };
    let back = (total * axis.cos()).round() as i32;
    let side = -(total * axis.sin()).round() as i32;
    let label = if side == 0 {
        "E8 total, axis 0"
    } else {
        "E8 total, D4 axis"
    };
    (Some((back, side)), label)
}

/// Appends " (auto)" to one of [`xi_spin_e8`]'s labels, for the `Auto` source
/// in `Full` mode.
fn auto_label(label: &'static str) -> &'static str {
    match label {
        "D4 (no E8 spin)" => "D4 (no E8 spin) (auto)",
        "E8 total, axis 0" => "E8 total, axis 0 (auto)",
        "E8 total, D4 axis" => "E8 total, D4 axis (auto)",
        _ => "D4 (auto)",
    }
}

fn ball_from_e8(e8: &FlightResultV1) -> BallFlight {
    // DSP→PC negation: wire neg=right, FRP pos=right.
    let flew = flew(e8.distance, e8.flight_time, e8.height);
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(e8.ball_velocity)),
        launch_elevation: Some(e8.elevation),
        launch_azimuth: Some(-e8.azimuth),
        carry_distance: Some(Distance::Meters(e8.distance)),
        carry_offline: flew
            .then(|| finite(-e8.lateral))
            .flatten()
            .map(Distance::Meters),
        max_height: Some(Distance::Meters(e8.height)),
        flight_time: Some(e8.flight_time),
        backspin_rpm: Some(e8.backspin_rpm),
        sidespin_rpm: None, // always zero in E8 — None, not Some(0)
        ..BallFlight::default()
    }
}

fn club_from_ed(ed: &ironsight::protocol::shot::ClubResult) -> ClubData {
    // DSP→PC negation: path, face_angle, swing_plane_horizontal.
    ClubData {
        club_speed: Some(Velocity::MetersPerSecond(ed.pre_club_speed)),
        path: Some(-ed.strike_direction),
        attack_angle: Some(ed.attack_angle),
        face_angle: Some(-ed.face_angle),
        dynamic_loft: Some(ed.dynamic_loft),
        smash_factor: Some(ed.smash_factor),
        club_speed_post: Some(Velocity::MetersPerSecond(ed.post_club_speed)),
        swing_plane_horizontal: Some(-ed.swing_plane_horizontal),
        swing_plane_vertical: Some(ed.swing_plane_vertical),
        club_offset: Some(Distance::Meters(ed.club_offset)),
        club_height: Some(Distance::Meters(ed.club_height)),
        ..ClubData::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Xi shot with club data: D4 back/side 7127/-652 rpm, E8 backspin 7157 rpm.
    fn shot_with_club_data() -> (FlightResult, FlightResultV1) {
        let d4 = hex(
            "9c00000400005600000000002700020a008a28001fcd005209014ca2000ff30034e30149f8000000002a04\
             001bd7fffd740000000003e9fffefbffe76d003c1affce900006f70000000000000000000000000000000\
             06c96002c8fffc66e004e9500d43600571500975a00197701df710000003d08ebf5cc2b019bbcffe3fd000\
             00017c8affb5496ff6c7f00126500000008b809fe35a1004bd6fffab0",
        );
        let e8 = hex(
            "5d000004000000008a28000ff5014c9f0034ef002f1b005209001fcd000000000000001bf500000000005600\
             000001e00f0000003d08e0f5d3a3019570ffe4ea00000017ccdefb5b2eff69290012cb00000008bc60fe8a\
             0e003a8afffc10",
        );
        (
            FlightResult::decode(&d4).unwrap(),
            FlightResultV1::decode(&e8).unwrap(),
        )
    }

    /// Xi shot without club data: D4 back/side 2756/1464 rpm, E8 backspin 6897 rpm.
    fn shot_without_club_data() -> (FlightResult, FlightResultV1) {
        let d4 = hex(
            "9c00000600005800000000000d00021a009424000c48005d7e018a1600113700434201877e000000002d1c\
             000ac40005b8000000ffffa6000262fff32a00433bffc671000a9000000000000000000000000000000000\
             79fc000000000000005c5000000000000000000000000001c55f0000003d08edf75422014102ffeb5b0000\
             001b82defa4676ffadd3000bd50000000373af00b838ffd83c000317",
        );
        let e8 = hex(
            "5d00000600000000942400128d0178410049d60014a4005d7e000c48000000000000001af100000000005800\
             000001c6220000003d08f2f57fd4018863ffe88b0000001b5f02fb475bff7f2a000f4f000000035a4eff6c\
             7b00157efffeb8",
        );
        (
            FlightResult::decode(&d4).unwrap(),
            FlightResultV1::decode(&e8).unwrap(),
        )
    }

    #[test]
    fn xi_spin_uses_d4_axis_with_club_data() {
        let (d4, e8) = shot_with_club_data();
        assert_eq!((d4.backspin_rpm, d4.sidespin_rpm), (7127, -652));
        assert_eq!(e8.backspin_rpm, 7157);
        let (spin, label) = xi_spin(SpinSource::E8, ShotDetectionMode::Full, &d4, Some(&e8));
        let (back, side) = spin.unwrap();
        assert_eq!(label, "E8 total, D4 axis");
        // Same axis as D4 (sidespin negated for FRP), total from E8.
        assert_eq!((back, side), (7127, 652));
        let total = f64::from(back).hypot(f64::from(side));
        assert!((total - 7157.0).abs() < 1.0);
        let axis = f64::from(side).atan2(f64::from(back)).to_degrees();
        assert!((axis - 5.2).abs() < 0.05, "axis {axis}");
    }

    #[test]
    fn xi_spin_axis_zero_without_club_data() {
        let (d4, e8) = shot_without_club_data();
        assert_eq!((d4.backspin_rpm, d4.sidespin_rpm), (2756, 1464));
        assert_eq!(
            xi_spin(SpinSource::E8, ShotDetectionMode::Full, &d4, Some(&e8)),
            (Some((6897, 0)), "E8 total, axis 0")
        );
    }

    #[test]
    fn xi_spin_d4_keeps_d4() {
        for (d4, e8) in [shot_with_club_data(), shot_without_club_data()] {
            assert_eq!(
                xi_spin(SpinSource::D4, ShotDetectionMode::Full, &d4, Some(&e8)),
                (None, "D4")
            );
            assert_eq!(
                xi_spin(SpinSource::D4, ShotDetectionMode::Full, &d4, None),
                (None, "D4")
            );
            // The shot then carries D4's spin exactly (sidespin negated for FRP).
            let ball = ball_from_d4(&d4);
            assert_eq!(ball.backspin_rpm, Some(d4.backspin_rpm));
            assert_eq!(ball.sidespin_rpm, Some(-d4.sidespin_rpm));
        }
    }

    #[test]
    fn xi_spin_e8_has_no_placeholder_guard() {
        // A short pitch: the E8 value is used as reported, even below D4's;
        // `e8` has no placeholder guard.
        let (mut d4, mut e8) = shot_without_club_data();
        d4.backspin_rpm = 4000;
        e8.backspin_rpm = 2500;
        assert_eq!(
            xi_spin(SpinSource::E8, ShotDetectionMode::Full, &d4, Some(&e8)).0,
            Some((2500, 0))
        );
    }

    #[test]
    fn xi_spin_auto_uses_the_mode_at_trigger() {
        let (d4, e8) = shot_with_club_data();
        // Triggered in Full, switched to Chipping before the D4 arrived.
        let mode = shot_mode(Some(ShotDetectionMode::Full), ShotDetectionMode::Chipping);
        assert_eq!(mode, ShotDetectionMode::Full);
        assert_eq!(
            xi_spin(SpinSource::Auto, mode, &d4, Some(&e8)),
            (Some((7127, 652)), "E8 total, D4 axis (auto)")
        );
        // Triggered in Chipping, switched to Full mid-shot: D4 stays.
        let mode = shot_mode(Some(ShotDetectionMode::Chipping), ShotDetectionMode::Full);
        assert_eq!(
            xi_spin(SpinSource::Auto, mode, &d4, Some(&e8)),
            (None, "D4 (auto)")
        );
        // No trigger seen: the current mode applies.
        assert_eq!(
            shot_mode(None, ShotDetectionMode::Putting),
            ShotDetectionMode::Putting
        );
    }

    #[test]
    fn xi_spin_auto_full_applies_e8_rule() {
        let (d4, e8) = shot_with_club_data();
        assert_eq!(
            xi_spin(SpinSource::Auto, ShotDetectionMode::Full, &d4, Some(&e8)),
            (Some((7127, 652)), "E8 total, D4 axis (auto)")
        );
    }

    #[test]
    fn xi_spin_auto_chipping_keeps_d4() {
        let (d4, e8) = shot_with_club_data();
        assert_eq!(
            xi_spin(
                SpinSource::Auto,
                ShotDetectionMode::Chipping,
                &d4,
                Some(&e8)
            ),
            (None, "D4 (auto)")
        );
        // Also with no E8 at all -- still a plain D4 passthrough.
        assert_eq!(
            xi_spin(SpinSource::Auto, ShotDetectionMode::Chipping, &d4, None),
            (None, "D4 (auto)")
        );
    }

    #[test]
    fn product_and_firmware_from_device_info() {
        assert_eq!(
            product_and_firmware(
                DeviceModel::Mevo,
                Some(DeviceGen::MevoPlus),
                "0.37   M2 TR 03 Jan 21 2026 11:29:54"
            ),
            ("Mevo+".into(), Some("0.37 (Jan 21 2026)".into()))
        );
        assert_eq!(
            product_and_firmware(DeviceModel::Mevo, Some(DeviceGen::Gen2), "0.37   M2 TR 03"),
            ("Mevo Gen2".into(), Some("0.37".into()))
        );
        assert_eq!(
            product_and_firmware(
                DeviceModel::Mevo,
                Some(DeviceGen::Unknown(0x01)),
                "1.02 XY 7 Dec  9 2025 08:00:00"
            ),
            ("Mevo".into(), Some("1.02 (Dec 9 2025)".into()))
        );
        assert_eq!(
            product_and_firmware(
                DeviceModel::Xi,
                None,
                "2.07    FSCOPE  Apr  2 2018 12:00:00"
            ),
            ("Xi".into(), Some("2.07 (Apr 2 2018)".into()))
        );
        assert_eq!(
            product_and_firmware(DeviceModel::Mevo, None, ""),
            ("Mevo".into(), None)
        );
        // The hardware code never reaches the model or firmware.
        let (model, firmware) = product_and_firmware(
            DeviceModel::Mevo,
            None,
            "0.37   M2 TR 03 Jan 21 2026 11:29:54",
        );
        assert!(!format!("{model} {firmware:?}").contains("M2"));
    }

    #[test]
    fn zero_speed_d4_is_a_failed_model() {
        let (mut d4, _) = shot_with_club_data();
        assert!(!d4_model_failed(&d4));
        d4.launch_speed = 0.0;
        d4.launch_elevation = 0.0;
        assert!(d4_model_failed(&d4));
        d4.launch_speed = -0.1;
        assert!(d4_model_failed(&d4));
        d4.launch_speed = f64::NAN;
        assert!(d4_model_failed(&d4));
    }

    #[test]
    fn zero_speed_d4_decoded_from_wire_is_a_failed_model() {
        // The shot-with-club-data D4 with launch speed (INT24 at offset 16)
        // and elevation (INT24 at offset 22) zeroed, as the device sends when
        // its distance model fails.
        let mut raw = hex(
            "9c00000400005600000000002700020a008a28001fcd005209014ca2000ff30034e30149f8000000002a04\
             001bd7fffd740000000003e9fffefbffe76d003c1affce900006f70000000000000000000000000000000\
             06c96002c8fffc66e004e9500d43600571500975a00197701df710000003d08ebf5cc2b019bbcffe3fd000\
             00017c8affb5496ff6c7f00126500000008b809fe35a1004bd6fffab0",
        );
        assert!(!d4_model_failed(&FlightResult::decode(&raw).unwrap()));
        raw[16..19].fill(0);
        raw[22..25].fill(0);
        let failed = FlightResult::decode(&raw).unwrap();
        assert_eq!(failed.launch_speed, 0.0);
        assert_eq!(failed.launch_elevation, 0.0);
        assert!(d4_model_failed(&failed));
    }

    /// Scripted FlightScope on an in-memory stream: serves the queued frames
    /// and answers the shot-completion requests (0x6D, ARM) the way the device
    /// does.
    mod fake_device {
        use std::cell::RefCell;
        use std::collections::VecDeque;
        use std::io;
        use std::rc::Rc;

        use ironsight::{BusAddr, FrameSplitter, RawFrame};

        #[derive(Default)]
        pub struct State {
            splitter: FrameSplitter,
            outbox: VecDeque<u8>,
            /// ED returned for the ShotResultReq.
            pub shot_ed: Vec<u8>,
        }

        impl State {
            pub fn push(&mut self, src: BusAddr, type_id: u8, payload: Vec<u8>) {
                let frame = RawFrame {
                    dest: BusAddr::App,
                    src,
                    type_id,
                    payload,
                };
                self.outbox.extend(frame.encode());
            }

            fn respond(&mut self, f: &RawFrame) {
                match f.type_id {
                    0x6D => {
                        let ed = self.shot_ed.clone();
                        self.push(BusAddr::Avr, 0xED, ed);
                    }
                    0xB0 => {
                        self.push(BusAddr::Avr, 0x95, vec![0x02, 0x30, 0x30]);
                        self.push(BusAddr::Avr, 0xE3, b"\n\rARMED\0".to_vec());
                    }
                    _ => {}
                }
            }
        }

        pub struct Stream(pub Rc<RefCell<State>>);

        impl io::Read for Stream {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let mut st = self.0.borrow_mut();
                if st.outbox.is_empty() {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                let n = buf.len().min(st.outbox.len());
                for (b, v) in buf.iter_mut().zip(st.outbox.drain(..n)) {
                    *b = v;
                }
                Ok(n)
            }
        }

        impl io::Write for Stream {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let mut st = self.0.borrow_mut();
                for wire in st.splitter.feed(buf) {
                    let frame = RawFrame::parse(&wire).expect("valid frame");
                    st.respond(&frame);
                }
                Ok(buf.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
    }

    const SHOT_ED: &str = "a62f002040000adb0007db000474fffa3f00028b000f220004f80002240008b50015380003f2fffed00002\
        1f0000040028460000000000030000020009ea000000045a3bfffcf203ec210e3ea8fffcf203ebe9e12e7a\
        000030ff23313d086c000030ff233a2a9ca70015de00f05d3531680015df00f041146dc2ffff02fffa6f00\
        1f47ffff02fffa6f001f470016bb000dd80019820016bb000dd8001982fff3a0001080000102";

    const SHOT_EF: &str = "8930040003520008266a000251000643e00001a500052eb000016c0005216300016600054b0800\
        0164000521e900028800072c0f000264000732e00001b600062ed500027900093329000223000\
        821cb000220000800000000000000000000000000000000000000000011111bf500741bf50000\
        000000000000030000051bf503691111000000000000";

    /// Made-up AVR status: hardware ID bytes 0x12, 0x34 at offsets 2 and 5.
    fn avr_status() -> Vec<u8> {
        let mut p = vec![0u8; 25];
        p[0] = 0x18;
        p[2] = 0x12;
        p[5] = 0x34;
        p
    }

    /// Made-up AVR device info carrying a serial in its text slots.
    fn avr_dev_info() -> Vec<u8> {
        let mut p = vec![0u8; 75];
        p[0] = 0x4A;
        p[27..52].copy_from_slice(b"FS Mevo 2 BM17.04 SERIAL9");
        p
    }

    #[test]
    fn raw_capture_holds_the_shot_window_without_identity_frames() {
        use ironsight::BusAddr;

        let (d4, e8) = (
            hex(
                "9c00000400005600000000002700020a008a28001fcd005209014ca2000ff30034e30149f8000000002a04\
                 001bd7fffd740000000003e9fffefbffe76d003c1affce900006f70000000000000000000000000000000\
                 06c96002c8fffc66e004e9500d43600571500975a00197701df710000003d08ebf5cc2b019bbcffe3fd000\
                 00017c8affb5496ff6c7f00126500000008b809fe35a1004bd6fffab0",
            ),
            hex(
                "5d000004000000008a28000ff5014c9f0034ef002f1b005209001fcd000000000000001bf500000000005600\
                 000001e00f0000003d08e0f5d3a3019570ffe4ea00000017ccdefb5b2eff69290012cb00000008bc60fe8a\
                 0e003a8afffc10",
            ),
        );
        let state = Rc::new(RefCell::new(fake_device::State::default()));
        {
            let mut st = state.borrow_mut();
            st.shot_ed = hex(SHOT_ED);
            st.push(BusAddr::Avr, 0xAA, avr_status());
            st.push(BusAddr::Avr, 0xE7, avr_dev_info());
            st.push(BusAddr::Avr, 0xE5, b"\x0cBALL TRIGGER\0".to_vec());
            st.push(BusAddr::Dsp, 0xE8, e8);
            st.push(BusAddr::Avr, 0xAA, avr_status());
            st.push(BusAddr::Dsp, 0xD4, d4);
            st.push(BusAddr::Dsp, 0xED, hex(SHOT_ED));
            st.push(BusAddr::Dsp, 0xEF, hex(SHOT_EF));
            st.push(BusAddr::Avr, 0xE5, b"\x0aPROCESSED\0".to_vec());
            st.push(BusAddr::Avr, 0xE5, b"\x05IDLE\0".to_vec());
            st.push(BusAddr::Avr, 0xB1, vec![0x02, 0x00, 0x00]);
        }

        let mut conn = BinaryConnection::new(fake_device::Stream(Rc::clone(&state)));
        let capture = Rc::new(RefCell::new(RawCapture::new(raw_capture_config())));
        let last_d4 = attach_raw_capture(&mut conn, "mevo.0", &capture);
        let mut client = BinaryClient::new(conn);

        let deadline = Instant::now() + Duration::from_secs(10);
        let raw = loop {
            assert!(Instant::now() < deadline, "shot never completed");
            match client.poll().expect("poll") {
                Some(BinaryEvent::Trigger) => capture.borrow_mut().mark_trigger(),
                Some(BinaryEvent::ShotComplete(_)) => break capture.borrow_mut().take(),
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        };

        let seq: Vec<(RawDir, &str)> = raw
            .frames
            .iter()
            .map(|r| (r.dir, r.r#type.as_deref().unwrap_or("")))
            .collect();
        use RawDir::{Rx, Tx};
        assert_eq!(
            seq,
            [
                (Rx, "0xE5"),
                (Rx, "0xE8"),
                (Rx, "0xD4"),
                (Rx, "0xED"),
                (Rx, "0xEF"),
                (Rx, "0xE5"),
                (Tx, "0x69"),
                (Tx, "0x69"),
                (Rx, "0xE5"),
                (Tx, "0x6D"),
                (Rx, "0xB1"),
                (Rx, "0xED"),
                (Tx, "0xB0"),
                (Rx, "0x95"),
                (Rx, "0xE3"),
            ]
        );
        assert_eq!(raw.transport, "flightscope-tcp");
        assert_eq!(raw.dropped, 0);
        // The E5 is pre-roll, every later frame arrived after the trigger.
        assert!(raw.frames[0].t_ms <= 0);
        assert!(raw.frames[1..].iter().all(|r| r.t_ms >= 0));
        assert_eq!(raw.frames[0].src.as_deref(), Some("AVR"));
        assert_eq!(raw.frames[0].dst.as_deref(), Some("APP"));
        assert_eq!(raw.frames[12].src.as_deref(), Some("APP"));
        assert_eq!(raw.frames[12].dst.as_deref(), Some("AVR"));
        assert_eq!(raw.frames[12].hex.as_deref(), Some("0101"));
        assert!(!last_d4.borrow().is_empty());

        // No identity: the status and device info frames, the serial text,
        // and the status frame's hardware ID bytes are all absent.
        let json = serde_json::to_string(&raw).expect("ser");
        assert!(!json.contains("0xAA") && !json.contains("0xE7"));
        assert!(!json.contains(&hex_lower(b"SERIAL9")));
        assert!(!json.contains(&hex_lower(&avr_status())));
        assert!(
            raw.frames
                .iter()
                .filter_map(|r| r.hex.as_deref())
                .all(|h| !h.starts_with("1800120000340000"))
        );
    }

    #[test]
    fn export_denylist_covers_identity_types() {
        let rec = |t: u8| RawRecord {
            r#type: Some(raw_capture::type_label(t)),
            ..RawRecord::binary(RawDir::Rx, &[])
        };
        for t in [
            0xAA, 0x67, 0xE7, 0xFD, 0x48, 0xC8, 0xDE, 0x87, 0xD0, 0xD1, 0xD2, 0xD3, 0x9B, 0x23,
            0xA2, 0x90, 0x89,
        ] {
            assert!(export_excluded(&rec(t)), "0x{t:02X} must be excluded");
        }
        for t in [
            0xD4, 0xE8, 0xED, 0xEF, 0xE5, 0xE3, 0x36, 0xBA, 0xDF, 0xE0, 0xE1,
        ] {
            assert!(!export_excluded(&rec(t)), "0x{t:02X} must be kept");
        }
        for t in [0xEC, 0xEE, 0xD9] {
            assert!(export_bulk(&rec(t)));
        }
        assert!(!export_bulk(&rec(0xD4)));
    }

    /// The sensor activation exchange (0x90 out, 0x89 back) carries a
    /// per-device certificate and never reaches a shot export.
    #[test]
    fn export_skips_sensor_activation() {
        let rec = |dir: RawDir, t: u8| RawRecord {
            r#type: Some(raw_capture::type_label(t)),
            ..RawRecord::binary(dir, &[t; 8])
        };
        let mut capture = RawCapture::new(raw_capture_config());
        capture.push(rec(RawDir::Tx, 0x90));
        capture.push(rec(RawDir::Rx, 0x89));
        capture.mark_trigger();
        capture.push(rec(RawDir::Tx, 0x90));
        capture.push(rec(RawDir::Rx, 0x89));
        capture.push(rec(RawDir::Rx, 0xD4));
        let raw = capture.take();
        let types: Vec<u8> = raw
            .frames
            .iter()
            .filter_map(raw_capture::type_byte)
            .collect();
        assert_eq!(types, [0xD4]);
        assert_eq!(raw.dropped, 0);
    }

    #[test]
    fn xi_spin_axis_zero_without_d4_backspin() {
        let (mut d4, e8) = shot_with_club_data();
        assert!(d4.clubhead_speed > 0.0);
        for back in [0, -1200] {
            d4.backspin_rpm = back;
            assert_eq!(
                xi_spin(SpinSource::E8, ShotDetectionMode::Full, &d4, Some(&e8)),
                (Some((7157, 0)), "E8 total, axis 0"),
                "D4 backspin {back}"
            );
        }
    }

    #[test]
    fn xi_spin_keeps_d4_without_e8_spin() {
        let (d4, mut e8) = shot_with_club_data();
        assert_eq!(
            xi_spin(SpinSource::E8, ShotDetectionMode::Full, &d4, None),
            (None, "D4 (no E8 spin)")
        );
        e8.backspin_rpm = 0;
        assert_eq!(
            xi_spin(SpinSource::E8, ShotDetectionMode::Full, &d4, Some(&e8)),
            (None, "D4 (no E8 spin)")
        );
    }

    /// D4 landing lateral and E8 lateral are wire-signed like the azimuth
    /// (negative = right), so FRP's carry offline is their negation. This Xi
    /// shot launched left (FRP azimuth -8.14) and landed left.
    #[test]
    fn carry_offline_is_the_negated_wire_lateral() {
        let (d4, e8) = shot_with_club_data();
        let ball = ball_from_d4(&d4);
        assert_eq!(ball.launch_azimuth, Some(-d4.launch_azimuth));
        assert!(ball.launch_azimuth.unwrap() < 0.0);
        let offline = ball.carry_offline.expect("offline").as_meters();
        assert!((offline - -d4.landing_position[2]).abs() < 1e-12);
        assert!(offline < 0.0, "{offline}");

        let ball = ball_from_e8(&e8);
        let offline = ball.carry_offline.expect("offline").as_meters();
        assert!((offline - -e8.lateral).abs() < 1e-12);
        assert!(offline < 0.0, "{offline}");
    }

    #[test]
    fn d4_carry_offline_follows_the_wire_sign() {
        let (mut d4, mut e8) = shot_with_club_data();
        d4.landing_position[2] = -4.25;
        e8.lateral = -3.5;
        let d4_offline = ball_from_d4(&d4).carry_offline;
        let e8_offline = ball_from_e8(&e8).carry_offline;
        assert_eq!(d4_offline, Some(Distance::Meters(4.25)));
        assert_eq!(e8_offline, Some(Distance::Meters(3.5)));
    }

    /// Descent angle from the D4 landing velocity: angle below horizontal of
    /// `[forward, vertical, lateral]`.
    #[test]
    fn d4_descent_angle_from_landing_velocity() {
        let (d4, _) = shot_with_club_data();
        let [fwd, up, side] = d4.landing_velocity;
        assert!(up < 0.0);
        let want = (-up).atan2(fwd.hypot(side)).to_degrees();
        let got = ball_from_d4(&d4).descent_angle.expect("descent");
        assert!((got - want).abs() < 1e-12);
        assert!((got - 39.25).abs() < 0.01, "{got}");

        let steep = descent_angle([20.0, -20.0, 0.0]).expect("descending");
        assert!((steep - 45.0).abs() < 1e-12, "{steep}");
        let sideways = descent_angle([0.0, -10.0, 10.0]).expect("descending");
        assert!((sideways - 45.0).abs() < 1e-12, "{sideways}");
        // Still rising, level, or not a number: no descent.
        assert_eq!(descent_angle([20.0, 1.0, 0.0]), None);
        assert_eq!(descent_angle([20.0, 0.0, 0.0]), None);
        assert_eq!(descent_angle([f64::NAN, -1.0, 0.0]), None);
    }

    /// A ball that did not fly has no landing: no offline or descent.
    #[test]
    fn no_landing_fields_without_a_flight() {
        let (d4, e8) = shot_with_club_data();
        for (carry, time, apex) in [(0.0, 4.0, 13.0), (85.0, 0.0, 13.0), (85.0, 4.0, 0.0)] {
            let mut d = d4.clone();
            (d.carry_distance, d.flight_time, d.max_height) = (carry, time, apex);
            let ball = ball_from_d4(&d);
            assert_eq!(ball.carry_offline, None, "{carry} {time} {apex}");
            assert_eq!(ball.descent_angle, None, "{carry} {time} {apex}");

            let mut e = e8.clone();
            (e.distance, e.flight_time, e.height) = (carry, time, apex);
            assert_eq!(
                ball_from_e8(&e).carry_offline,
                None,
                "{carry} {time} {apex}"
            );
        }
        // E8 has no landing velocity, so never a descent angle.
        assert_eq!(ball_from_e8(&e8).descent_angle, None);
    }

    /// The Mevo reports spin components only; total and axis are left to the
    /// shot metrics.
    #[test]
    fn mevo_sends_spin_components_only() {
        let (d4, e8) = shot_with_club_data();
        for ball in [ball_from_d4(&d4), ball_from_e8(&e8)] {
            assert_eq!(ball.total_spin_rpm, None);
            assert_eq!(ball.spin_axis, None);
        }
    }
}
