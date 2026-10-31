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
    TYPE_DSP_QUERY, TYPE_DSP_QUERY_RESP, TYPE_FLIGHT_RESULT, TYPE_FLIGHT_RESULT_V1,
    TYPE_NET_CONFIG, TYPE_PRC_DATA, TYPE_PROD_INFO, TYPE_SENSOR_ACT, TYPE_SENSOR_ACT_RESP,
    TYPE_SPEED_PROFILE, TYPE_STATUS, TYPE_TIME_SYNC, TYPE_WIFI_SCAN,
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
    RawDir, RawRecord, Severity, ShotDetectionMode, ShotKey, ShotRaw, SpinSource, Velocity,
    hex_lower,
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
/// camera, so camera modes do not apply to it. It is configured the way FS
/// Golf configures it (`SessionConfig::to_xi_settings`), and ironsight acks
/// each of its flight results; both go through the same send hook as every
/// other frame, so the audit log and the shot export record them.
///
/// `spin_source` picks the Xi's spin, and `launch_only_spin` (rpm per mph,
/// `0` = off) the backspin of a result with launch data but no spin and no
/// flight. The run loop reads both from the live config on every shot; the
/// fields are the values at start, used if the section cannot be read.
pub struct MevoActor {
    pub addr: SocketAddr,
    pub initial_mode: ShotDetectionMode,
    pub session_config: SessionConfig,
    pub use_estimated: bool,
    pub camera_mode: CameraMode,
    pub spin_source: SpinSource,
    pub launch_only_spin: f64,
}

impl Actor for MevoActor {
    fn start(&self, state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let addr = self.addr;
        let initial_mode = self.initial_mode;
        let session_config = self.session_config.clone();
        let use_estimated = self.use_estimated;
        let camera_mode = self.camera_mode;
        let spin_source = self.spin_source;
        let launch_only_spin = self.launch_only_spin;
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
                    launch_only_spin,
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

        // Spin source and launch-only spin are read from the live config on
        // every shot, so a change applies on the next shot without a restart.
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
        session_config.xi_label(mode)
    } else {
        session_config.mode_label(mode)
    }
}

/// Whether switching between two detection modes changes what is sent to
/// the device. On the Xi, Chipping and Putting select the same session.
fn changes_wire(
    model: DeviceModel,
    session_config: &SessionConfig,
    from: &ShotDetectionMode,
    to: &ShotDetectionMode,
) -> bool {
    if model == DeviceModel::Xi {
        session_config.to_xi_settings(from) != session_config.to_xi_settings(to)
    } else {
        !same_mode(*from, *to)
    }
}

/// Apply a mode change that waited for the device to be idle. Returns
/// whether a configuration was queued (the device is then re-armed).
fn apply_deferred<S: Read + Write>(
    client: &mut BinaryClient<S>,
    model: DeviceModel,
    session_config: &SessionConfig,
    current_mode: &mut ShotDetectionMode,
    target: ShotDetectionMode,
) -> bool {
    let target_wire = wire_label(model, session_config, &target);
    let is_mode_change = !same_mode(*current_mode, target);
    if model == DeviceModel::Xi && !changes_wire(model, session_config, current_mode, &target) {
        info!("deferred mode change to {target:?} -- device stays in {target_wire}");
        *current_mode = target;
        return false;
    }
    if is_mode_change {
        info!("applying deferred mode change to {target:?} wire={target_wire}");
    } else {
        info!("applying deferred settings reconfig");
    }
    *current_mode = target;
    queue_configure(client, model, session_config, &target, is_mode_change);
    true
}

/// Queue the configuration for `mode`, then an arm. A Mevo+ gets the mode
/// alone on a mode change and its full settings otherwise. The Xi gets FS
/// Golf's configuration: its connect-time block on the connection's first
/// configuration, the mode block after that (`BinaryClient` disarms first
/// when armed).
fn queue_configure<S: Read + Write>(
    client: &mut BinaryClient<S>,
    model: DeviceModel,
    session_config: &SessionConfig,
    mode: &ShotDetectionMode,
    mode_change: bool,
) {
    if model == DeviceModel::Xi {
        client.configure_xi(session_config.to_xi_settings(mode));
    } else if mode_change {
        client.configure_avr(session_config.to_avr_settings_mode_only(mode));
    } else {
        client.configure_avr(session_config.to_avr_settings(mode));
    }
    client.arm();
}

/// Whether a D4 is the device's "distance model failed" result: launch speed
/// 0 (or not a number). This happens in full-flight outdoor mode when the
/// ball hits a net or screen; the E8 that came before it is still valid.
fn d4_model_failed(d4: &FlightResult) -> bool {
    d4.launch_speed.is_nan() || d4.launch_speed <= 0.0
}

/// Whether an E8 is the device's "no read" result: ball speed 0 (or not a
/// number). The device sends it when it triggered but tracked no ball; its
/// other fields are zero too.
fn e8_no_read(e8: &FlightResultV1) -> bool {
    e8.ball_velocity.is_nan() || e8.ball_velocity <= 0.0
}

/// Flight and club results of the shot in progress.
///
/// Club data waits here until the shot has a ball result: it is sent right
/// after an accepted D4, or when the shot completes, so a shot the device
/// did not read sends no club data.
#[derive(Default)]
struct ShotResults {
    /// A D4 with a launch speed arrived and its ball flight was sent.
    had_d4: bool,
    /// Latest E8, for the spin source and the E8 fallback.
    e8: Option<FlightResultV1>,
    /// Club data not sent yet.
    club: Option<ClubData>,
}

/// What a completed shot still sends, from [`ShotResults::finish`].
#[derive(Debug)]
enum ShotOutcome {
    /// The D4 ball flight was sent.
    Measured,
    /// No D4: the ball flight comes from the E8.
    Estimated(FlightResultV1),
    /// No D4, and estimated shots are off.
    Skipped,
    /// No D4 and no E8.
    NoFlight,
    /// The device triggered but did not read the ball: its E8 is empty and
    /// there is no D4. Nothing is sent for the shot but the alert.
    NoRead(FlightResultV1),
}

impl ShotResults {
    /// Record an accepted D4. Returns the club data to send after its ball
    /// flight.
    fn accept_d4(&mut self) -> Option<ClubData> {
        self.had_d4 = true;
        self.club.take()
    }

    /// Record club data. Returns it when it can be sent now (the ball
    /// flight is already out); otherwise it waits for the ball result.
    fn club(&mut self, club: ClubData) -> Option<ClubData> {
        if self.had_d4 {
            Some(club)
        } else {
            self.club = Some(club);
            None
        }
    }

    /// The shot's outcome and the club data it still sends.
    fn finish(self, use_estimated: bool) -> (ShotOutcome, Option<ClubData>) {
        if self.had_d4 {
            return (ShotOutcome::Measured, self.club);
        }
        match self.e8 {
            Some(e8) if e8_no_read(&e8) => (ShotOutcome::NoRead(e8), None),
            _ if !use_estimated => (ShotOutcome::Skipped, self.club),
            Some(e8) => (ShotOutcome::Estimated(e8), self.club),
            None => (ShotOutcome::NoFlight, self.club),
        }
    }
}

/// Latest raw payload of each shot result type, for the log lines that
/// report a dropped result.
#[derive(Default)]
struct ResultRaw {
    d4: Vec<u8>,
    e8: Vec<u8>,
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
/// shot export, and the latest D4, E8 and ED payloads (for logging a result
/// that is dropped).
fn attach_raw_capture<S: Read + Write>(
    conn: &mut BinaryConnection<S>,
    actor_id: &str,
    capture: &Rc<RefCell<RawCapture>>,
) -> Rc<RefCell<ResultRaw>> {
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
    let result_raw: Rc<RefCell<ResultRaw>> = Rc::default();
    let raw_sink = Rc::clone(&result_raw);
    let recv_capture = Rc::clone(capture);
    conn.set_on_recv(move |env| {
        let mut sink = raw_sink.borrow_mut();
        match env.type_id {
            TYPE_FLIGHT_RESULT => sink.d4.clone_from(&env.raw),
            TYPE_FLIGHT_RESULT_V1 => sink.e8.clone_from(&env.raw),
            _ => {}
        }
        drop(sink);
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
    result_raw
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

/// What the shot handlers need from the connection.
#[derive(Clone, Copy)]
struct ShotContext<'a> {
    state: &'a SystemState,
    sender: &'a BusSender,
    device_id: &'a Option<String>,
    model: DeviceModel,
    /// Xi spin source when the live config has none for this actor.
    spin_source: SpinSource,
    /// Launch-only spin (rpm per mph) when the live config has none for
    /// this actor.
    launch_only_spin: f64,
    result_raw: &'a RefCell<ResultRaw>,
}

/// Handle one result of the shot in progress: send a D4 ball flight, keep
/// the E8 for the fallback, and send or hold club data.
fn on_shot_datum(
    ctx: &ShotContext<'_>,
    results: &mut ShotResults,
    key: &ShotKey,
    mode: ShotDetectionMode,
    datum: ShotDatum,
) {
    let ShotContext {
        state,
        sender,
        device_id,
        model,
        result_raw,
        ..
    } = *ctx;
    match datum {
        ShotDatum::Flight(d4) if d4_model_failed(&d4) => {
            // The device's distance model failed (e.g. full-flight outdoor
            // mode into a screen). Treat the shot as having no D4 so the E8
            // fallback applies.
            warn!(
                "shot #{} D4 dropped: launch speed {} (distance model failed) -- 0x{:02X} {}",
                key.shot_number,
                d4.launch_speed,
                TYPE_FLIGHT_RESULT,
                hex_string(&result_raw.borrow().d4),
            );
        }
        ShotDatum::Flight(d4) => {
            let held_club = results.accept_d4();
            let mut ball = ball_from_d4(&d4);
            let mut spin_source = "D4";
            if model == DeviceModel::Xi {
                let source = live_spin_source(state, sender.actor_id()).unwrap_or(ctx.spin_source);
                let (spin, label) = xi_spin(source, mode, &d4, results.e8.as_ref());
                if let Some((back, side)) = spin {
                    ball.backspin_rpm = Some(back);
                    ball.sidespin_rpm = Some(side);
                }
                spin_source = label;
            }
            let rpm_per_mph =
                live_launch_only_spin(state, sender.actor_id()).unwrap_or(ctx.launch_only_spin);
            let estimated = estimate_launch_only_spin(&mut ball, &d4, rpm_per_mph, mode);
            if estimated {
                spin_source = "estimate";
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
            let mut event = sim::ball_flight_event_in_mode(state, key.clone(), ball, Some(mode));
            if estimated {
                mark_spin_estimated(&mut event);
            }
            let msg = FlighthookMessage::new(event);
            sender.send(stamp_device(msg, device_id));
            if let Some(club) = held_club {
                send_club(sender, device_id, key, club);
            }
        }
        ShotDatum::FlightV1(e8) => {
            debug!("stashed E8 for fallback");
            results.e8 = Some(e8);
        }
        ShotDatum::Club(ed) => {
            if let Some(club) = results.club(club_from_ed(&ed)) {
                send_club(sender, device_id, key, club);
            }
        }
        ShotDatum::Spin(_) => {
            // Spin axis / total spin are derived from backspin + sidespin in
            // BallFlight; EF is redundant.
        }
    }
}

fn send_club(sender: &BusSender, device_id: &Option<String>, key: &ShotKey, club: ClubData) {
    let msg = FlighthookMessage::new(FlighthookEvent::ClubPath {
        key: key.clone(),
        club: Box::new(club),
    });
    sender.send(stamp_device(msg, device_id));
}

/// A shot the device has finished: its key, the detection mode captured at
/// its trigger, and its raw frames.
struct CompletedShot {
    key: ShotKey,
    mode: ShotDetectionMode,
    raw: ShotRaw,
}

/// Send what a completed shot still sends (the E8 ball flight, the club
/// data held back, or an alert), then its `ShotFinished` with the raw
/// frames.
fn finish_shot(
    ctx: &ShotContext<'_>,
    shot: CompletedShot,
    (outcome, club): (ShotOutcome, Option<ClubData>),
) {
    let ShotContext {
        state,
        sender,
        device_id,
        model,
        result_raw,
        ..
    } = *ctx;
    let CompletedShot { key, mode, raw } = shot;
    match outcome {
        ShotOutcome::Measured => {}
        ShotOutcome::Estimated(e8) => {
            let ball = ball_from_e8(&e8);
            info!(
                "shot #{} (E8): ball={:.1}mph VLA={:.1} HLA={:.1} carry={:.1}yd",
                key.shot_number,
                ball.launch_speed.map(|v| v.as_mph()).unwrap_or(0.0),
                ball.launch_elevation.unwrap_or(0.0),
                ball.launch_azimuth.unwrap_or(0.0),
                ball.carry_distance.map(|d| d.as_yards()).unwrap_or(0.0),
            );
            let msg = FlighthookMessage::new(sim::ball_flight_event_in_mode(
                state,
                key.clone(),
                ball,
                Some(mode),
            ));
            sender.send(stamp_device(msg, device_id));
        }
        ShotOutcome::Skipped => {
            info!(
                "shot #{} had only E8 (estimated) — skipped (use_estimated=false)",
                key.shot_number,
            );
        }
        ShotOutcome::NoFlight => {
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
        ShotOutcome::NoRead(e8) => {
            warn!(
                "shot #{} not read: E8 ball speed {} and no D4 -- 0x{:02X} {}",
                key.shot_number,
                e8.ball_velocity,
                TYPE_FLIGHT_RESULT_V1,
                hex_string(&result_raw.borrow().e8),
            );
            emit_alert(
                sender,
                Severity::Warn,
                format!(
                    "Shot #{}: {} didn't track the ball (no read)",
                    key.shot_number,
                    model.label(),
                ),
            );
        }
    }
    if let Some(club) = club {
        send_club(sender, device_id, &key, club);
    }
    sender.send(stamp_device(
        FlighthookMessage::new(FlighthookEvent::ShotFinished {
            key,
            raw: Some(Box::new(raw)),
        }),
        device_id,
    ));
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
    launch_only_spin: f64,
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
            launch_only_spin,
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
    initial_launch_only_spin: f64,
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

    // Audit logging, raw capture for the shot export, and the latest
    // D4/E8/ED payloads.
    let capture = Rc::new(RefCell::new(RawCapture::new(raw_capture_config())));
    let result_raw = attach_raw_capture(&mut conn, sender.actor_id(), &capture);

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
    let mut shot_results = ShotResults::default();
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
                        if !changes_wire(model, session_config, &current_mode, &new_mode) {
                            info!(
                                "mode change: {:?} -> {:?} -- device stays in {}",
                                current_mode,
                                new_mode,
                                wire_label(model, session_config, &new_mode)
                            );
                            current_mode = new_mode;
                        } else if client.is_armed() {
                            let new_wire = wire_label(model, session_config, &new_mode);
                            info!(
                                "mode change: {:?} -> {:?} wire={new_wire}",
                                current_mode, new_mode
                            );
                            queue_configure(&mut client, model, session_config, &new_mode, true);
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
                                "FlightScope Xi: FS Golf configuration, {}; \
                                 ball type, tee height, track % and camera mode are not applied",
                                session_config.xi_label(&current_mode)
                            );
                            pending_fusion_cam = None;
                            queue_configure(
                                &mut client,
                                h.model,
                                session_config,
                                &current_mode,
                                false,
                            );
                        } else {
                            client.configure_avr(session_config.to_avr_settings(&current_mode));
                            client.configure_cam(cam_config());
                            client.arm();
                        }
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
                        if let Some(target) = pending_reconfig.take()
                            && apply_deferred(
                                &mut client,
                                model,
                                session_config,
                                &mut current_mode,
                                target,
                            )
                        {
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
                        shot_results = ShotResults::default();
                        shot_detection_mode = Some(current_mode);
                        emit_device_status(sender, ActorStatus::Connected, HashMap::new());
                        device_telemetry.insert("ready".into(), "false".into());
                        try_emit_device_telemetry(sender, device_id, &device_telemetry);
                    }

                    BinaryEvent::ShotDatum(datum) => {
                        if let Some(ref key) = current_shot_key {
                            let ctx = ShotContext {
                                state,
                                sender,
                                device_id,
                                model,
                                spin_source: initial_spin_source,
                                launch_only_spin: initial_launch_only_spin,
                                result_raw: &result_raw,
                            };
                            let mode = shot_mode(shot_detection_mode, current_mode);
                            on_shot_datum(&ctx, &mut shot_results, key, mode, datum);
                        }
                    }

                    BinaryEvent::ShotComplete(_shot_data) => {
                        let raw = capture.borrow_mut().take();
                        let results = std::mem::take(&mut shot_results);
                        if let Some(key) = current_shot_key.take() {
                            let ctx = ShotContext {
                                state,
                                sender,
                                device_id,
                                model,
                                spin_source: initial_spin_source,
                                launch_only_spin: initial_launch_only_spin,
                                result_raw: &result_raw,
                            };
                            let shot = CompletedShot {
                                key,
                                mode: shot_mode(shot_detection_mode, current_mode),
                                raw,
                            };
                            finish_shot(&ctx, shot, results.finish(use_estimated));
                        }

                        // Device is auto-re-armed by BinaryClient's ShotSequencer,
                        // which reports the completed shot rather than an Armed
                        // event, so readiness is restored here. Without this the
                        // device reads not-ready for the rest of the session, from
                        // the first trigger onwards.
                        device_telemetry.insert("ready".into(), "true".into());
                        try_emit_device_telemetry(sender, device_id, &device_telemetry);

                        // Apply pending reconfig if any.
                        if let Some(target) = pending_reconfig.take()
                            && apply_deferred(
                                &mut client,
                                model,
                                session_config,
                                &mut current_mode,
                                target,
                            )
                        {
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

                    BinaryEvent::StepRefused(env) => {
                        warn!(
                            "device refused a step, continuing: {} 0x{:02X} {}",
                            env.src,
                            env.type_id,
                            hex_string(&env.raw),
                        );
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

/// Lowest launch angle (deg) of a launch-only D4. Below it (putts, bladed
/// shots) a D4 with no spin and no flight keeps its apex of 0 and is a
/// ground ball.
const LAUNCH_ONLY_MIN_LAUNCH_DEG: f64 = 5.0;

/// Whether a D4 is launch-only: it has launch data but no spin (backspin and
/// sidespin 0) and no flight at all (carry, flight time and apex each 0,
/// negative or non-finite), and was launched at
/// [`LAUNCH_ONLY_MIN_LAUNCH_DEG`] or more. The Xi sends these on very short
/// chips. A D4 with any one of carry, flight time or apex is not.
fn launch_only(d4: &FlightResult) -> bool {
    let absent = |v: f64| !(v.is_finite() && v > 0.0);
    absent(d4.carry_distance)
        && absent(d4.flight_time)
        && absent(d4.max_height)
        && d4.backspin_rpm == 0
        && d4.sidespin_rpm == 0
        && d4.launch_elevation >= LAUNCH_ONLY_MIN_LAUNCH_DEG
}

/// Backspin (rpm) for a ball with launch data but no spin: `rpm_per_mph`
/// times the ball speed in mph, rounded. `None` (keep the ball's spin) when
/// `rpm_per_mph` is not above 0 or not a valid `launch_only_spin`
/// ([`MevoSection::launch_only_spin_valid`](flighthook::MevoSection::launch_only_spin_valid)),
/// in Putting mode, when the ball has backspin or sidespin, or when its speed
/// is not positive or the product is out of range for an rpm.
fn launch_only_spin(ball: &BallFlight, rpm_per_mph: f64, mode: ShotDetectionMode) -> Option<i32> {
    if !(flighthook::MevoSection::launch_only_spin_valid(rpm_per_mph) && rpm_per_mph > 0.0)
        || mode == ShotDetectionMode::Putting
    {
        return None;
    }
    if ball.backspin_rpm.unwrap_or(0) != 0 || ball.sidespin_rpm.unwrap_or(0) != 0 {
        return None;
    }
    let mph = ball.launch_speed?.as_mph();
    if !(mph.is_finite() && mph > 0.0) {
        return None;
    }
    let rpm = (rpm_per_mph * mph).round();
    if rpm > f64::from(i32::MAX) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)] // 0 to i32::MAX, checked above
    Some(rpm as i32)
}

/// Give `ball`, from the launch-only `d4` ([`launch_only`]), the estimated
/// backspin [`launch_only_spin`] and no sidespin, and unset its carry, apex
/// and flight time: the device computed none, so the simulation flies the
/// shot from its launch and that spin. Returns whether the estimate was
/// applied. When it is not (`d4` is not launch-only, the estimate is off or
/// declines, or the spin, after the Xi's spin source, is not 0/0), `ball` is
/// left as it is: a D4 with no flight keeps its 0 carry, apex and flight time
/// and is rolled as a ground ball.
fn estimate_launch_only_spin(
    ball: &mut BallFlight,
    d4: &FlightResult,
    rpm_per_mph: f64,
    mode: ShotDetectionMode,
) -> bool {
    if !launch_only(d4) {
        return false;
    }
    let Some(back) = launch_only_spin(ball, rpm_per_mph, mode) else {
        return false;
    };
    ball.backspin_rpm = Some(back);
    ball.sidespin_rpm = Some(0);
    ball.carry_distance = None;
    ball.max_height = None;
    ball.flight_time = None;
    true
}

/// Mark a `BallFlight` event's spin as flighthook's estimate
/// ([`DerivedFlight::spin_estimated`](flighthook::DerivedFlight::spin_estimated)),
/// adding a derived block that holds only the flag when the shot was not
/// simulated.
fn mark_spin_estimated(event: &mut FlighthookEvent) {
    if let FlighthookEvent::BallFlight { derived, .. } = event {
        derived.get_or_insert_with(Box::default).spin_estimated = true;
    }
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

/// The `[mevo.N]` launch-only spin (rpm per mph, `0` = off) for the actor
/// `actor_id` (`"mevo.N"`), from the live config.
fn live_launch_only_spin(state: &SystemState, actor_id: &str) -> Option<f64> {
    let (_, index) = actor_id.split_once('.')?;
    state.system.mevo_launch_only_spin(index)
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
    use ironsight::protocol::TYPE_CLUB_RESULT;
    use ironsight::protocol::shot::ClubResult;

    #[test]
    fn xi_chipping_and_putting_share_a_session() {
        use ShotDetectionMode::{Chipping, Full, Putting};
        let s = SessionConfig::default();
        let xi = DeviceModel::Xi;
        assert!(!changes_wire(xi, &s, &Chipping, &Putting));
        assert!(!changes_wire(xi, &s, &Putting, &Chipping));
        assert!(changes_wire(xi, &s, &Full, &Chipping));
        assert!(changes_wire(xi, &s, &Putting, &Full));
        assert!(changes_wire(DeviceModel::Mevo, &s, &Chipping, &Putting));
        assert_eq!(wire_label(xi, &s, &Putting), "pitching limited-flight(9)");
        assert_eq!(wire_label(DeviceModel::Mevo, &s, &Putting), "putting(3)");
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// D4 of an Xi shot with club data: back/side 7127/-652 rpm.
    const CLUB_SHOT_D4: &str = "9c00000400005600000000002700020a008a28001fcd005209014ca2000ff30034e30149f8000000002a04\
        001bd7fffd740000000003e9fffefbffe76d003c1affce900006f70000000000000000000000000000000\
        06c96002c8fffc66e004e9500d43600571500975a00197701df710000003d08ebf5cc2b019bbcffe3fd000\
        00017c8affb5496ff6c7f00126500000008b809fe35a1004bd6fffab0";

    /// Xi shot with club data: D4 back/side 7127/-652 rpm, E8 backspin 7157 rpm.
    fn shot_with_club_data() -> (FlightResult, FlightResultV1) {
        let d4 = hex(CLUB_SHOT_D4);
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
        let result_raw = attach_raw_capture(&mut conn, "mevo.0", &capture);
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
        assert!(!result_raw.borrow().d4.is_empty());
        assert!(!result_raw.borrow().e8.is_empty());

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

    // Xi limited-flight chips. A chip the device did not read: BALL
    // TRIGGER, then an empty E8 and an empty ED (the ED is also its reply to
    // the 0x6D request).
    const XI_NO_READ_TRIGGER: &str = "1542414c4c20545249474745523a20313837206d7300";
    const XI_NO_READ_E8: &str = "5d000009000000000000000000000000000000000000000000000000000000000000000000000000000000\
        0000003d090000000000000000000000000000000000000000000000000000000000000000000000000000\
        0000000000000000";
    const XI_NO_READ_ED: &str = "a6000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000003d0900000000000000000000000000000000000000000000000000000000000000000000000000\
        00000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        0000000000000000000000000000000000000000000000000000000000000000000000000000";

    // A chip the device read: E8 only, no D4.
    const XI_CHIP_TRIGGER: &str = "1542414c4c20545249474745523a20313233206d7300";
    const XI_CHIP_E8: &str = "5d00000c003652004619000879006e040012f20008610078e4000f76000000019640000fc50002040000df\
        00000003fac40000003d08faf913c6017639ffdd7300000024a1b0eec81bffddb40021d9000000043815ff\
        d4c300034200009b";
    const XI_CHIP_ED: &str = "a60700000000056e00000000000000000000025000121700050a0000000000000000000000000000000000\
        000000000463970000000000ce0005ab0000000000003d08f4000000000000000000000000000000000000\
        00000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        00000000000000000000000000000000000000000000000000000000000000000000000003f1";

    /// The frames that end a shot: E5 PROCESSED, E5 IDLE, then the B1.
    /// The Xi's result acks are tx frames like any other: sent through
    /// the connection, they reach the export with their hex, directed APP
    /// to AVR, after the result they ack.
    #[test]
    fn raw_capture_records_xi_result_acks() {
        use ironsight::{BusAddr, seq};

        let state = Rc::new(RefCell::new(fake_device::State::default()));
        {
            let mut st = state.borrow_mut();
            st.push(BusAddr::Avr, TYPE_FLIGHT_RESULT_V1, hex(XI_CHIP_E8));
            st.push(BusAddr::Avr, TYPE_FLIGHT_RESULT, hex(CLUB_SHOT_D4));
        }
        let mut conn = BinaryConnection::new(fake_device::Stream(Rc::clone(&state)));
        let capture = Rc::new(RefCell::new(RawCapture::new(raw_capture_config())));
        let _ = attach_raw_capture(&mut conn, "mevo.0", &capture);
        capture.borrow_mut().mark_trigger();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut acked = 0;
        while acked < 2 {
            assert!(Instant::now() < deadline, "results never arrived");
            if let Some(env) = conn.recv().expect("recv") {
                let ack = seq::result_ack(DeviceModel::Xi, &env).expect("result ack");
                seq::send_action(&mut conn, ack).expect("send");
                acked += 1;
            }
        }

        let raw = capture.borrow_mut().take();
        let got: Vec<(RawDir, &str, &str, &str, &str)> = raw
            .frames
            .iter()
            .map(|r| {
                (
                    r.dir,
                    r.r#type.as_deref().unwrap_or(""),
                    r.src.as_deref().unwrap_or(""),
                    r.dst.as_deref().unwrap_or(""),
                    r.hex.as_deref().unwrap_or(""),
                )
            })
            .collect();
        use RawDir::{Rx, Tx};
        assert_eq!(
            got,
            [
                (Rx, "0xE8", "AVR", "APP", XI_CHIP_E8),
                (Tx, "0x95", "APP", "AVR", "021068"),
                (Rx, "0xD4", "AVR", "APP", CLUB_SHOT_D4),
                (Tx, "0x95", "APP", "AVR", "021054"),
            ]
        );
    }

    /// The Xi DSP power-mode write (DSP param 0x01 = 1) and its ack carry
    /// no identity, so the export keeps them like any other param write:
    /// tx 0xBF APP→DSP with its hex, then the DSP's 0x95 and the E3 power
    /// mode texts.
    #[test]
    fn raw_capture_records_xi_dsp_power_mode_write() {
        use ironsight::BusAddr;
        use ironsight::protocol::Command;
        use ironsight::protocol::config::{ParamData, ParamValue};

        assert!(!EXPORT_DENYLIST.contains(&ironsight::protocol::TYPE_PARAM_VALUE));
        assert!(!EXPORT_DENYLIST.contains(&0x95));
        let state = Rc::new(RefCell::new(fake_device::State::default()));
        let mut conn = BinaryConnection::new(fake_device::Stream(Rc::clone(&state)));
        let capture = Rc::new(RefCell::new(RawCapture::new(raw_capture_config())));
        let _ = attach_raw_capture(&mut conn, "mevo.0", &capture);
        capture.borrow_mut().mark_trigger();

        let power_mode = Command::ParamValue(ParamValue {
            param_id: 0x01,
            value: ParamData::Int24(1),
        });
        conn.send(&power_mode, BusAddr::Dsp).expect("send");
        {
            let mut st = state.borrow_mut();
            st.push(BusAddr::Dsp, 0x95, hex("02403f"));
            st.push(BusAddr::Dsp, 0xE3, b"  REQ Power Mode : 1 (4) \0".to_vec());
            st.push(BusAddr::Dsp, 0xE3, b"  NEW Power Mode : 1 \0".to_vec());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut received = 0;
        while received < 3 {
            assert!(Instant::now() < deadline, "replies never arrived");
            if conn.recv().expect("recv").is_some() {
                received += 1;
            }
        }

        let raw = capture.borrow_mut().take();
        let got: Vec<(RawDir, &str, &str, &str)> = raw
            .frames
            .iter()
            .map(|r| {
                (
                    r.dir,
                    r.r#type.as_deref().unwrap_or(""),
                    r.src.as_deref().unwrap_or(""),
                    r.dst.as_deref().unwrap_or(""),
                )
            })
            .collect();
        use RawDir::{Rx, Tx};
        assert_eq!(
            got,
            [
                (Tx, "0xBF", "APP", "DSP"),
                (Rx, "0x95", "DSP", "APP"),
                (Rx, "0xE3", "DSP", "APP"),
                (Rx, "0xE3", "DSP", "APP"),
            ]
        );
        assert_eq!(raw.frames[0].hex.as_deref(), Some("06000001000001"));
        assert_eq!(raw.frames[1].hex.as_deref(), Some("02403f"));
    }

    const SHOT_PROCESSED: &str = "0a50524f43455353454400";
    const SHOT_IDLE: &str = "0549444c4500";
    const SHOT_B1: &str = "020000";

    /// One shot as the device sends it: its frames from the BALL TRIGGER to
    /// the B1, and the ED it sends in reply to the 0x6D request.
    struct DeviceShot {
        number: u32,
        frames: Vec<(u8, Vec<u8>)>,
        reply_ed: Vec<u8>,
    }

    impl DeviceShot {
        /// Shot `number`: the BALL TRIGGER `trigger`, `results` in order, then
        /// PROCESSED, IDLE and the B1.
        fn new(number: u32, trigger: &str, results: Vec<(u8, Vec<u8>)>, reply_ed: Vec<u8>) -> Self {
            let mut frames = vec![(0xE5, hex(trigger))];
            frames.extend(results);
            frames.extend([
                (0xE5, hex(SHOT_PROCESSED)),
                (0xE5, hex(SHOT_IDLE)),
                (0xB1, hex(SHOT_B1)),
            ]);
            Self {
                number,
                frames,
                reply_ed,
            }
        }

        /// An Xi chip the device did not read.
        fn xi_no_read(number: u32) -> Self {
            Self::new(
                number,
                XI_NO_READ_TRIGGER,
                vec![
                    (TYPE_FLIGHT_RESULT_V1, hex(XI_NO_READ_E8)),
                    (TYPE_CLUB_RESULT, hex(XI_NO_READ_ED)),
                ],
                hex(XI_NO_READ_ED),
            )
        }

        /// An Xi chip the device read (E8 only).
        fn xi_chip(number: u32) -> Self {
            Self::new(
                number,
                XI_CHIP_TRIGGER,
                vec![
                    (TYPE_FLIGHT_RESULT_V1, hex(XI_CHIP_E8)),
                    (TYPE_CLUB_RESULT, hex(XI_CHIP_ED)),
                ],
                hex(XI_CHIP_ED),
            )
        }
    }

    /// Three Xi chips: a non-read, a read, then a non-read.
    fn xi_chips() -> Vec<DeviceShot> {
        vec![
            DeviceShot::xi_no_read(1),
            DeviceShot::xi_chip(2),
            DeviceShot::xi_no_read(3),
        ]
    }

    /// Replay `shots` from a fake `model` through the client and the actor's
    /// shot handlers, as the run loop drives them, and return every bus
    /// message.
    fn replay(
        state: &SystemState,
        model: DeviceModel,
        shots: &[DeviceShot],
        use_estimated: bool,
    ) -> Vec<FlighthookMessage> {
        let (bus_tx, mut bus_rx) = tokio::sync::broadcast::channel(1024);
        let sender = BusSender::new("mevo.0".into(), bus_tx, Arc::default());
        let device = Rc::new(RefCell::new(fake_device::State::default()));
        let mut conn = BinaryConnection::new(fake_device::Stream(Rc::clone(&device)));
        let capture = Rc::new(RefCell::new(RawCapture::new(raw_capture_config())));
        let result_raw = attach_raw_capture(&mut conn, "mevo.0", &capture);
        let mut client = BinaryClient::new(conn);
        let device_id = Some(format!("{} (test)", model.label()));
        let ctx = ShotContext {
            state,
            sender: &sender,
            device_id: &device_id,
            model,
            spin_source: SpinSource::E8,
            launch_only_spin: flighthook::MevoSection::DEFAULT_LAUNCH_ONLY_SPIN,
            result_raw: &result_raw,
        };
        let mode = ShotDetectionMode::Chipping;

        for (i, shot) in shots.iter().enumerate() {
            {
                let mut st = device.borrow_mut();
                st.shot_ed.clone_from(&shot.reply_ed);
                for (ty, payload) in &shot.frames {
                    st.push(ironsight::BusAddr::Avr, *ty, payload.clone());
                }
            }
            let mut current: Option<(ShotKey, ShotResults)> = None;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(Instant::now() < deadline, "shot {i} never completed");
                match client.poll().expect("poll") {
                    Some(BinaryEvent::Trigger) => {
                        capture.borrow_mut().mark_trigger();
                        let key = ShotKey {
                            shot_id: format!("chip-{i}"),
                            shot_number: shot.number,
                        };
                        sender.send(stamp_device(
                            FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                                key: key.clone(),
                            }),
                            &device_id,
                        ));
                        current = Some((key, ShotResults::default()));
                    }
                    Some(BinaryEvent::ShotDatum(datum)) => {
                        let (key, results) = current.as_mut().expect("datum before trigger");
                        on_shot_datum(&ctx, results, key, mode, datum);
                    }
                    Some(BinaryEvent::ShotComplete(_)) => {
                        let raw = capture.borrow_mut().take();
                        let (key, results) = current.take().expect("complete before trigger");
                        let shot = CompletedShot { key, mode, raw };
                        finish_shot(&ctx, shot, results.finish(use_estimated));
                        break;
                    }
                    Some(_) => {}
                    None => std::thread::sleep(Duration::from_millis(1)),
                }
            }
        }

        let mut out = Vec::new();
        while let Ok(msg) = bus_rx.try_recv() {
            out.push(msg);
        }
        out
    }

    /// Event kinds of `messages`, one string per shot (`|`-separated).
    fn kinds_by_shot(messages: &[FlighthookMessage]) -> Vec<String> {
        let mut shots: Vec<Vec<&str>> = Vec::new();
        for msg in messages {
            let kind = match &msg.event {
                FlighthookEvent::ShotTrigger { .. } => {
                    shots.push(Vec::new());
                    "trigger"
                }
                FlighthookEvent::BallFlight { .. } => "ball",
                FlighthookEvent::ClubPath { .. } => "club",
                FlighthookEvent::Alert { .. } => "alert",
                FlighthookEvent::ShotFinished { .. } => "finished",
                other => panic!("unexpected {other:?}"),
            };
            shots.last_mut().expect("trigger first").push(kind);
        }
        shots.into_iter().map(|k| k.join("|")).collect()
    }

    /// The alert texts in `messages`, all warnings.
    fn warnings(messages: &[FlighthookMessage]) -> Vec<&str> {
        messages
            .iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::Alert { severity, message } => {
                    assert_eq!(*severity, Severity::Warn);
                    Some(message.as_str())
                }
                _ => None,
            })
            .collect()
    }

    /// The club data sent in `messages`.
    fn club_paths(messages: &[FlighthookMessage]) -> Vec<ClubData> {
        messages
            .iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::ClubPath { club, .. } => Some(club.as_ref().clone()),
                _ => None,
            })
            .collect()
    }

    /// [`replay`] on a fresh system state.
    fn replay_fresh(
        model: DeviceModel,
        shots: &[DeviceShot],
        use_estimated: bool,
    ) -> Vec<FlighthookMessage> {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-mevo-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let messages = replay(&state, model, shots, use_estimated);
        let _ = std::fs::remove_dir_all(&dir);
        messages
    }

    /// The shots `messages` complete, as GSPro's accumulator builds them.
    fn completed_shots(messages: &[FlighthookMessage]) -> Vec<flighthook::ShotData> {
        use flighthook::ShotAccumulator;

        let mut sent = Vec::new();
        let mut acc: Option<ShotAccumulator> = None;
        for msg in messages {
            match &msg.event {
                FlighthookEvent::ShotTrigger { key } => {
                    acc = Some(ShotAccumulator::new(msg.actor.clone(), key.clone()));
                }
                FlighthookEvent::BallFlight { ball, .. } => {
                    acc.as_mut().expect("acc").set_ball(*ball.clone());
                }
                FlighthookEvent::ClubPath { club, .. } => {
                    acc.as_mut().expect("acc").set_club(*club.clone());
                }
                FlighthookEvent::ShotFinished { .. } => {
                    if let Some(shot) = acc.take().expect("acc").finish() {
                        sent.push(shot);
                    }
                }
                _ => {}
            }
        }
        sent
    }

    #[test]
    fn xi_empty_e8_is_a_no_read() {
        let e8 = |h| FlightResultV1::decode(&hex(h)).unwrap();
        assert!(e8_no_read(&e8(XI_NO_READ_E8)));
        assert!(!e8_no_read(&e8(XI_CHIP_E8)));
        let mut read = e8(XI_CHIP_E8);
        read.ball_velocity = f64::NAN;
        assert!(e8_no_read(&read));
    }

    #[test]
    fn shot_results_hold_club_data_until_the_ball_result() {
        let (d4, e8) = shot_with_club_data();
        let club =
            club_from_ed(&ironsight::protocol::shot::ClubResult::decode(&hex(SHOT_ED)).unwrap());
        let empty_e8 = FlightResultV1::decode(&hex(XI_NO_READ_E8)).unwrap();

        // ED before D4: held, then sent after the D4's ball flight.
        let mut r = ShotResults {
            e8: Some(e8.clone()),
            ..ShotResults::default()
        };
        assert!(r.club(club.clone()).is_none());
        assert!(!d4_model_failed(&d4));
        assert!(r.accept_d4().is_some());
        let (outcome, held) = r.finish(false);
        assert!(matches!(outcome, ShotOutcome::Measured));
        assert!(held.is_none());

        // ED after D4: sent at once.
        let mut r = ShotResults::default();
        assert!(r.accept_d4().is_none());
        assert!(r.club(club.clone()).is_some());

        // No D4: club data goes out with the E8 fallback, or alone when
        // estimated shots are off.
        let mut r = ShotResults {
            e8: Some(e8.clone()),
            ..ShotResults::default()
        };
        assert!(r.club(club.clone()).is_none());
        let (outcome, held) = r.finish(true);
        assert!(matches!(outcome, ShotOutcome::Estimated(_)));
        assert!(held.is_some());
        let mut r = ShotResults {
            e8: Some(e8),
            ..ShotResults::default()
        };
        r.club(club.clone());
        let (outcome, held) = r.finish(false);
        assert!(matches!(outcome, ShotOutcome::Skipped));
        assert!(held.is_some());
        let mut r = ShotResults::default();
        r.club(club.clone());
        let (outcome, held) = r.finish(true);
        assert!(matches!(outcome, ShotOutcome::NoFlight));
        assert!(held.is_some());

        // Empty E8 and no D4: no read, and the club data is dropped,
        // whether estimated shots are on or off.
        for use_estimated in [true, false] {
            let mut r = ShotResults {
                e8: Some(empty_e8.clone()),
                ..ShotResults::default()
            };
            r.club(club.clone());
            let (outcome, held) = r.finish(use_estimated);
            assert!(matches!(outcome, ShotOutcome::NoRead(_)));
            assert!(held.is_none());
        }

        // Empty E8 but a D4 with a launch speed: the D4 stands.
        let mut r = ShotResults {
            e8: Some(empty_e8),
            ..ShotResults::default()
        };
        r.accept_d4();
        assert!(matches!(r.finish(true).0, ShotOutcome::Measured));
    }

    /// Xi chips replayed end to end: the non-reads send no ball flight or
    /// club data, only an alert, and stay out of `/api/shots` and GSPro but
    /// in the export; the read (shot 2) decodes to the device's values.
    #[tokio::test]
    async fn xi_non_reads_replayed_end_to_end() {
        use crate::actors::gspro::mapper::map_shot;
        use crate::actors::web::export::build_export_line;
        use crate::actors::web::{routes, tests as web};
        use flighthook::Handedness;

        let (web_state, _writer, _dir) = web::test_state();
        let shots = xi_chips();
        let messages = replay(&web_state.root, DeviceModel::Xi, &shots, true);

        assert_eq!(
            kinds_by_shot(&messages),
            [
                "trigger|alert|finished",
                "trigger|ball|club|finished",
                "trigger|alert|finished",
            ]
        );
        assert_eq!(
            warnings(&messages),
            [
                "Shot #1: Xi didn't track the ball (no read)",
                "Shot #3: Xi didn't track the ball (no read)",
            ]
        );
        // Every shot keeps its raw frames, E8 and ED included.
        for msg in &messages {
            if let FlighthookEvent::ShotFinished { raw, .. } = &msg.event {
                let raw = raw.as_deref().expect("raw");
                for ty in ["0xE8", "0xED"] {
                    assert!(raw.frames.iter().any(|r| r.r#type.as_deref() == Some(ty)));
                }
            }
        }

        // GSPro: only shot 2 completes a shot.
        let sent = completed_shots(&messages);
        assert_eq!(sent.len(), 1);
        let shot = &sent[0];
        assert_eq!(shot.shot_number, 2);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
        let ball = shot.ball.as_ref().expect("ball");
        assert!(close(ball.launch_speed.unwrap().as_mph(), 40.141_888_3));
        assert!(close(ball.launch_elevation.unwrap(), 30.948));
        assert!(close(ball.launch_azimuth.unwrap(), -3.958));
        assert!(close(
            ball.carry_distance.unwrap().as_yards(),
            30.800_524_934
        ));
        assert!(close(
            ball.carry_offline.unwrap().as_yards(),
            -2.345_800_525
        ));
        assert!(close(ball.max_height.unwrap().as_feet(), 15.912_073_491));
        assert!(close(ball.flight_time.unwrap(), 2.169));
        assert_eq!(ball.backspin_rpm, Some(4037));
        assert_eq!(ball.sidespin_rpm, None);
        let club = shot.club.as_ref().expect("club");
        assert!(close(club.club_speed.unwrap().as_mph(), 31.093_466));
        assert!(close(club.face_angle.unwrap(), -5.92));
        assert!(close(club.dynamic_loft.unwrap(), 46.31));
        assert!(close(club.smash_factor.unwrap(), 1.29));
        let gspro = map_shot(shot, Handedness::Right);
        assert!(gspro.shot_data_options.contains_ball_data);
        assert!(close(gspro.ball_data.speed, 40.141_888_3));
        assert!(close(gspro.club_data.speed, 31.093_466));

        // Web: all three stored for the export, only shot 2 in /api/shots.
        let events = messages.into_iter().map(|m| m.event).collect();
        web::feed(&web_state, "mevo.0", events).await;
        {
            let stored = web_state.shots.read().await;
            assert_eq!(stored.len(), 3);
            for s in stored.iter() {
                assert_eq!(s.data.has_data(), s.key.shot_number == 2);
                let line = build_export_line(s, "", None, true);
                assert!(line.raw.is_some_and(|r| !r.frames.is_empty()));
                let data = serde_json::to_value(&line.data).expect("json");
                if s.key.shot_number != 2 {
                    assert!(data.get("ball").is_none() && data.get("club").is_none());
                }
            }
        }
        let api = routes::get_shots(
            axum::extract::State(Arc::new(web_state)),
            axum::extract::Query(routes::ShotsQuery {
                limit: 50,
                units: None,
            }),
        )
        .await;
        let numbers: Vec<u32> = api.0.iter().map(|s| s.shot_number).collect();
        assert_eq!(numbers, [2]);
    }

    /// With estimated shots off the non-reads still alert, and the read
    /// (E8 only) sends its club data alone.
    #[test]
    fn xi_non_reads_alert_with_estimated_shots_off() {
        let messages = replay_fresh(DeviceModel::Xi, &xi_chips(), false);
        assert_eq!(
            kinds_by_shot(&messages),
            [
                "trigger|alert|finished",
                "trigger|club|finished",
                "trigger|alert|finished",
            ]
        );
    }

    /// A Mevo shot with a full result and club data with zero club speed
    /// but a face angle, attack angle, loft and path: the club data is sent
    /// as decoded, after the ball flight, and reaches GSPro.
    #[test]
    fn zero_club_speed_beside_a_d4_sends_the_club_data() {
        use crate::actors::gspro::mapper::map_shot;
        use flighthook::Handedness;

        let mut ed = hex(SHOT_ED);
        ed[5..8].fill(0);
        let decoded = ClubResult::decode(&ed).unwrap();
        assert_eq!(decoded.pre_club_speed, 0.0);
        for v in [
            decoded.face_angle,
            decoded.attack_angle,
            decoded.dynamic_loft,
            decoded.strike_direction,
        ] {
            assert_ne!(v, 0.0);
        }
        // The BALL TRIGGER text is the same on every model.
        let shot = || {
            DeviceShot::new(
                1,
                XI_CHIP_TRIGGER,
                vec![
                    (TYPE_FLIGHT_RESULT, hex(CLUB_SHOT_D4)),
                    (TYPE_CLUB_RESULT, ed.clone()),
                ],
                ed.clone(),
            )
        };
        for use_estimated in [false, true] {
            let messages = replay_fresh(DeviceModel::Mevo, &[shot()], use_estimated);
            assert_eq!(kinds_by_shot(&messages), ["trigger|ball|club|finished"]);
            assert_eq!(club_paths(&messages), [club_from_ed(&decoded)]);

            let sent = completed_shots(&messages);
            assert_eq!(sent.len(), 1);
            let gspro = map_shot(&sent[0], Handedness::Right);
            assert!(gspro.shot_data_options.contains_club_data);
            assert_eq!(gspro.club_data.speed, 0.0);
            assert_eq!(gspro.club_data.face_to_target, -decoded.face_angle);
            assert_eq!(gspro.club_data.angle_of_attack, decoded.attack_angle);
            assert_eq!(gspro.club_data.loft, decoded.dynamic_loft);
            assert_eq!(gspro.club_data.path, -decoded.strike_direction);
        }
    }

    /// Club data the device sends before the full result goes out right
    /// after the ball flight, as when it follows it, and GSPro gets both.
    #[test]
    fn club_data_before_the_d4_follows_the_ball_flight() {
        let ed = hex(SHOT_ED);
        let club = club_from_ed(&ClubResult::decode(&ed).unwrap());
        let d4 = (TYPE_FLIGHT_RESULT, hex(CLUB_SHOT_D4));
        let ed_frame = (TYPE_CLUB_RESULT, ed.clone());
        for results in [
            vec![ed_frame.clone(), d4.clone()],
            vec![d4.clone(), ed_frame.clone()],
        ] {
            let shot = DeviceShot::new(1, XI_CHIP_TRIGGER, results, ed.clone());
            let messages = replay_fresh(DeviceModel::Xi, &[shot], false);
            assert_eq!(kinds_by_shot(&messages), ["trigger|ball|club|finished"]);
            assert_eq!(club_paths(&messages), std::slice::from_ref(&club));
            let sent = completed_shots(&messages);
            assert_eq!(sent.len(), 1);
            assert!(sent[0].ball.is_some());
            assert_eq!(sent[0].club.as_ref(), Some(&club));
        }
    }

    /// On a Mevo+ or Mevo Gen2 the no-read alert names the Mevo.
    #[test]
    fn no_read_alert_names_the_mevo() {
        for use_estimated in [false, true] {
            let messages = replay_fresh(
                DeviceModel::Mevo,
                &[DeviceShot::xi_no_read(1)],
                use_estimated,
            );
            assert_eq!(kinds_by_shot(&messages), ["trigger|alert|finished"]);
            assert_eq!(
                warnings(&messages),
                ["Shot #1: Mevo didn't track the ball (no read)"]
            );
        }
    }

    /// A synthetic launch-only D4, as the Xi sends on a very short chip:
    /// 7.019 m/s (15.7 mph) at 22.8 deg, 0.856 deg left, no spin, no carry,
    /// apex or flight time.
    fn launch_only_d4() -> FlightResult {
        FlightResult {
            total: 1,
            track_time: 0.0,
            start_position: [0.0; 3],
            launch_speed: 7.019,
            // Wire sign: positive = left.
            launch_azimuth: 0.856,
            launch_elevation: 22.8,
            carry_distance: 0.0,
            flight_time: 0.0,
            max_height: 0.0,
            landing_position: [0.0; 3],
            backspin_rpm: 0,
            sidespin_rpm: 0,
            riflespin_rpm: 0,
            landing_spin_rpm: [0; 3],
            landing_velocity: [0.0; 3],
            total_distance: 0.0,
            roll_distance: 0.0,
            final_position: [0.0; 3],
            clubhead_speed: 0.0,
            club_strike_direction: 0.0,
            club_attack_angle: 0.0,
            clubhead_speed_post: 0.0,
            club_swing_plane_tilt: 0.0,
            club_swing_plane_rotation: 0.0,
            club_effective_loft: 0.0,
            club_face_angle: 0.0,
            poly_scale: 0,
            poly_x: [0.0; 5],
            poly_y: [0.0; 5],
            poly_z: [0.0; 5],
        }
    }

    /// A ball with launch data only: `mph` at 22.8 deg, spin `back`/`side`.
    fn launch_only_ball(mph: f64, back: i32, side: i32) -> BallFlight {
        BallFlight {
            launch_speed: Some(Velocity::MilesPerHour(mph)),
            launch_elevation: Some(22.8),
            launch_azimuth: Some(-0.856),
            backspin_rpm: Some(back),
            sidespin_rpm: Some(side),
            ..BallFlight::default()
        }
    }

    #[test]
    fn launch_only_spin_scales_with_ball_speed() {
        let b = launch_only_ball(15.7, 0, 0);
        assert_eq!(
            launch_only_spin(&b, 100.0, ShotDetectionMode::Chipping),
            Some(1570)
        );
        assert_eq!(
            launch_only_spin(&b, 100.0, ShotDetectionMode::Full),
            Some(1570)
        );
        assert_eq!(
            launch_only_spin(&b, 80.0, ShotDetectionMode::Chipping),
            Some(1256)
        );
        // Rounded to the nearest rpm.
        let b = launch_only_ball(12.345, 0, 0);
        assert_eq!(
            launch_only_spin(&b, 100.0, ShotDetectionMode::Full),
            Some(1235)
        );
        let b = launch_only_ball(12.344, 0, 0);
        assert_eq!(
            launch_only_spin(&b, 100.0, ShotDetectionMode::Full),
            Some(1234)
        );
        // The D4 speed in m/s: 7.019 m/s is 15.70 mph.
        let ball = ball_from_d4(&launch_only_d4());
        assert_eq!(
            launch_only_spin(&ball, 100.0, ShotDetectionMode::Chipping),
            Some(1570)
        );
        // Absent spin is no spin reading too.
        let b = BallFlight {
            backspin_rpm: None,
            sidespin_rpm: None,
            ..launch_only_ball(15.7, 0, 0)
        };
        assert_eq!(
            launch_only_spin(&b, 100.0, ShotDetectionMode::Full),
            Some(1570)
        );
    }

    #[test]
    fn launch_only_spin_is_skipped_when_off_putting_or_spinning() {
        let b = launch_only_ball(15.7, 0, 0);
        for k in [0.0, -100.0, 1000.5, 1e12, f64::NAN, f64::INFINITY] {
            assert_eq!(
                launch_only_spin(&b, k, ShotDetectionMode::Chipping),
                None,
                "{k}"
            );
        }
        assert_eq!(
            launch_only_spin(&b, 100.0, ShotDetectionMode::Putting),
            None
        );
        for (back, side) in [(900, 0), (0, -150), (900, -150)] {
            let b = launch_only_ball(15.7, back, side);
            assert_eq!(
                launch_only_spin(&b, 100.0, ShotDetectionMode::Chipping),
                None,
                "{back} {side}"
            );
        }
        for speed in [0.0, -1.0, f64::NAN] {
            let b = launch_only_ball(speed, 0, 0);
            assert_eq!(
                launch_only_spin(&b, 100.0, ShotDetectionMode::Chipping),
                None,
                "{speed}"
            );
        }
        let b = BallFlight {
            launch_speed: None,
            ..launch_only_ball(15.7, 0, 0)
        };
        assert_eq!(
            launch_only_spin(&b, 100.0, ShotDetectionMode::Chipping),
            None
        );
    }

    /// The highest setting gives a large but in-range spin; a product past
    /// `i32::MAX` is declined, never saturated.
    #[test]
    fn launch_only_spin_never_saturates() {
        let b = launch_only_ball(15.7, 0, 0);
        assert_eq!(
            launch_only_spin(&b, 1000.0, ShotDetectionMode::Chipping),
            Some(15_700)
        );
        let b = launch_only_ball(1e7, 0, 0);
        assert_eq!(
            launch_only_spin(&b, 1000.0, ShotDetectionMode::Chipping),
            None
        );
    }

    /// A D4 with any one of carry, flight time or apex is not launch-only,
    /// even when the others are 0.
    #[test]
    fn launch_only_needs_carry_apex_and_flight_time_all_absent() {
        assert!(launch_only(&launch_only_d4()));
        let mut absent = launch_only_d4();
        absent.carry_distance = -1.0;
        absent.flight_time = f64::NAN;
        absent.max_height = f64::NEG_INFINITY;
        assert!(launch_only(&absent));
        for (carry, time, apex) in [(12.0, 0.0, 0.0), (0.0, 0.9, 0.0), (0.0, 0.0, 0.4)] {
            let mut d4 = launch_only_d4();
            d4.carry_distance = carry;
            d4.flight_time = time;
            d4.max_height = apex;
            assert!(!launch_only(&d4), "{carry} {time} {apex}");
        }
    }

    /// The estimate unsets the launch-only D4's 0 carry, apex and flight
    /// time; when it is not applied, the ball keeps them and its spin.
    #[test]
    fn estimate_unsets_the_zeros_only_when_applied() {
        let d4 = launch_only_d4();
        let mut ball = ball_from_d4(&d4);
        assert_eq!(ball.carry_distance, Some(Distance::Meters(0.0)));
        assert!(estimate_launch_only_spin(
            &mut ball,
            &d4,
            100.0,
            ShotDetectionMode::Chipping
        ));
        assert_eq!(ball.carry_distance, None);
        assert_eq!(ball.max_height, None);
        assert_eq!(ball.flight_time, None);
        assert_eq!(ball.carry_offline, None);
        assert_eq!(ball.descent_angle, None);
        assert_eq!(ball.backspin_rpm, Some(1570));
        assert_eq!(ball.sidespin_rpm, Some(0));
        assert_eq!(ball.launch_elevation, Some(22.8));
        assert_eq!(ball.launch_azimuth, Some(-0.856));

        // Off, Putting, or a spin the Xi's spin source supplied.
        let mut spun = ball_from_d4(&d4);
        spun.backspin_rpm = Some(900);
        for (mut ball, k, mode) in [
            (ball_from_d4(&d4), 0.0, ShotDetectionMode::Chipping),
            (ball_from_d4(&d4), 100.0, ShotDetectionMode::Putting),
            (spun, 100.0, ShotDetectionMode::Chipping),
        ] {
            let before = ball.clone();
            assert!(
                !estimate_launch_only_spin(&mut ball, &d4, k, mode),
                "{k} {mode:?}"
            );
            assert_eq!(ball, before, "{k} {mode:?}");
            assert_eq!(ball.max_height, Some(Distance::Meters(0.0)));
        }
    }

    /// A low launch with no spin and no flight (a putt, a bladed shot), or
    /// any D4 with spin, keeps the reported 0 carry, apex and flight time,
    /// so the simulation rolls it as a ground ball.
    #[test]
    fn d4_without_flight_keeps_its_zeros_below_the_launch_only_angle_or_with_spin() {
        let mut low = launch_only_d4();
        low.launch_elevation = 2.0;
        let mut spinning = launch_only_d4();
        spinning.backspin_rpm = 900;
        let mut sidespin = launch_only_d4();
        sidespin.sidespin_rpm = 120;
        for d4 in [low, spinning, sidespin] {
            assert!(!launch_only(&d4));
            let ball = ball_from_d4(&d4);
            assert_eq!(ball.carry_distance, Some(Distance::Meters(0.0)));
            assert_eq!(ball.max_height, Some(Distance::Meters(0.0)));
            assert_eq!(ball.flight_time, Some(0.0));
        }
        let mut edge = launch_only_d4();
        edge.launch_elevation = LAUNCH_ONLY_MIN_LAUNCH_DEG;
        assert!(launch_only(&edge));
        edge.launch_elevation = f64::NAN;
        assert!(!launch_only(&edge));
    }

    #[test]
    fn flown_d4_keeps_its_flight() {
        let (d4, _) = shot_with_club_data();
        assert!(!launch_only(&d4));
        let ball = ball_from_d4(&d4);
        assert_eq!(
            ball.carry_distance,
            Some(Distance::Meters(d4.carry_distance))
        );
        assert_eq!(ball.max_height, Some(Distance::Meters(d4.max_height)));
        assert_eq!(ball.flight_time, Some(d4.flight_time));
        assert!(ball.carry_offline.is_some());
        assert!(ball.descent_angle.is_some());
        assert_eq!(ball.backspin_rpm, Some(d4.backspin_rpm));
        assert_eq!(ball.sidespin_rpm, Some(-d4.sidespin_rpm));
    }

    /// Hand `d4` to the D4 handler of a `model` actor whose shot was taken
    /// in `mode`, and return the ball flight it sends with its simulated
    /// values. `section` is the live `[mevo.0]`, if any.
    fn handle_d4(
        model: DeviceModel,
        mode: ShotDetectionMode,
        section: Option<flighthook::MevoSection>,
        d4: FlightResult,
    ) -> (BallFlight, Option<flighthook::DerivedFlight>) {
        handle_d4_with(model, mode, section, flighthook::SimMode::On, d4)
    }

    /// [`handle_d4`] with `[simulation] mode = sim`.
    fn handle_d4_with(
        model: DeviceModel,
        mode: ShotDetectionMode,
        section: Option<flighthook::MevoSection>,
        sim: flighthook::SimMode,
        d4: FlightResult,
    ) -> (BallFlight, Option<flighthook::DerivedFlight>) {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-mevo-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        state.system.update(|c| {
            c.simulation.mode = sim;
            if let Some(section) = section {
                c.mevo.insert("0".into(), section);
            }
        });
        let (bus_tx, mut bus_rx) = tokio::sync::broadcast::channel(16);
        let sender = BusSender::new("mevo.0".into(), bus_tx, Arc::default());
        let device_id = None;
        let result_raw = RefCell::new(ResultRaw::default());
        let ctx = ShotContext {
            state: &state,
            sender: &sender,
            device_id: &device_id,
            model,
            spin_source: SpinSource::D4,
            launch_only_spin: flighthook::MevoSection::DEFAULT_LAUNCH_ONLY_SPIN,
            result_raw: &result_raw,
        };
        let key = ShotKey {
            shot_id: "chip".into(),
            shot_number: 1,
        };
        let mut results = ShotResults::default();
        on_shot_datum(&ctx, &mut results, &key, mode, ShotDatum::Flight(d4));
        let _ = std::fs::remove_dir_all(&dir);
        while let Ok(msg) = bus_rx.try_recv() {
            if let FlighthookEvent::BallFlight { ball, derived, .. } = msg.event {
                return (*ball, derived.map(|d| *d));
            }
        }
        panic!("no ball flight sent");
    }

    /// The Xi's launch-only chip gets the default 100 rpm per mph of
    /// backspin, no carry, and with the simulation on (the default) flies:
    /// not a ground ball, with carry, apex, flight time and descent derived.
    #[test]
    fn xi_launch_only_chip_flies_with_the_estimated_spin() {
        for mode in [ShotDetectionMode::Chipping, ShotDetectionMode::Full] {
            let (ball, derived) = handle_d4(DeviceModel::Xi, mode, None, launch_only_d4());
            assert_eq!(ball.backspin_rpm, Some(1570));
            assert_eq!(ball.sidespin_rpm, Some(0));
            assert_eq!(ball.carry_distance, None);
            assert_eq!(ball.max_height, None);
            assert_eq!(ball.flight_time, None);
            let d = derived.expect("simulated");
            assert!(d.spin_estimated, "{mode:?}");
            // 15.7 mph at 22.8 deg with 1570 rpm: about 3.9 yd carry
            // (3.57 m), 0.38 m apex, 0.56 s, 23 deg descent, 10 yd total.
            let carry_yd = d.carry_distance.expect("carry").as_yards();
            assert!((3.5..4.5).contains(&carry_yd), "{carry_yd}");
            let apex_m = d.max_height.expect("apex").as_meters();
            assert!((0.3..0.5).contains(&apex_m), "{apex_m}");
            let time = d.flight_time.expect("flight time");
            assert!((0.45..0.7).contains(&time), "{time}");
            let descent = d.descent_angle.expect("descent");
            assert!((18.0..30.0).contains(&descent), "{descent}");
            assert!(d.total_distance.expect("total").as_meters() > 3.57);
            // A flight, not a ground ball's lone tee point.
            let path = d.path.expect("path");
            assert!(path.flight.len() >= 2, "{:?}", path.flight);
            assert!(path.flight.iter().any(|p| p[2] > 0.3), "{:?}", path.flight);
        }
    }

    /// Any FlightScope model gets the estimate, and the live section's
    /// value is used.
    #[test]
    fn launch_only_spin_follows_the_live_section() {
        let section = |k| flighthook::MevoSection {
            launch_only_spin: k,
            ..flighthook::MevoSection::default()
        };
        let (ball, _) = handle_d4(
            DeviceModel::Mevo,
            ShotDetectionMode::Chipping,
            Some(section(80.0)),
            launch_only_d4(),
        );
        assert_eq!(ball.backspin_rpm, Some(1256));
        assert_eq!(ball.sidespin_rpm, Some(0));
    }

    /// With the estimate off, or in Putting mode, a launch-only chip goes out
    /// as the device sent it: spin 0, carry, apex and flight time 0, rolled
    /// by the simulation as a ground ball, and not flagged.
    #[test]
    fn launch_only_chip_without_the_estimate_is_a_ground_ball() {
        let off = flighthook::MevoSection {
            launch_only_spin: 0.0,
            ..flighthook::MevoSection::default()
        };
        for (mode, section) in [
            (ShotDetectionMode::Chipping, Some(off)),
            (ShotDetectionMode::Putting, None),
        ] {
            let (ball, derived) = handle_d4(DeviceModel::Xi, mode, section, launch_only_d4());
            assert_eq!(ball.backspin_rpm, Some(0), "{mode:?}");
            assert_eq!(ball.sidespin_rpm, Some(0), "{mode:?}");
            assert_eq!(ball.carry_distance, Some(Distance::Meters(0.0)), "{mode:?}");
            assert_eq!(ball.max_height, Some(Distance::Meters(0.0)), "{mode:?}");
            assert_eq!(ball.flight_time, Some(0.0), "{mode:?}");
            let d = derived.expect("rolled");
            assert!(!d.spin_estimated, "{mode:?}");
            let path = d.path.expect("path");
            assert_eq!(path.flight, vec![[0.0; 4]], "{mode:?}");
            assert!(d.total_distance.expect("total").as_meters() > 0.0);
        }
    }

    /// A D4 with a measured carry but apex 0 keeps its carry and gets no
    /// estimate.
    #[test]
    fn d4_with_carry_but_no_apex_keeps_its_carry() {
        let mut d4 = launch_only_d4();
        d4.carry_distance = 12.0;
        d4.flight_time = 0.9;
        let (ball, derived) = handle_d4(DeviceModel::Xi, ShotDetectionMode::Chipping, None, d4);
        assert_eq!(ball.carry_distance, Some(Distance::Meters(12.0)));
        assert_eq!(ball.flight_time, Some(0.9));
        assert_eq!(ball.max_height, Some(Distance::Meters(0.0)));
        assert_eq!(ball.backspin_rpm, Some(0));
        assert!(derived.is_none_or(|d| !d.spin_estimated));
    }

    /// A low launch with no spin and no flight (a putt) is still rolled as a
    /// ground ball, in any mode, with no spin added.
    #[test]
    fn low_launch_without_flight_is_still_a_ground_ball() {
        let mut putt = launch_only_d4();
        putt.launch_speed = 3.0;
        putt.launch_elevation = 2.0;
        for mode in [ShotDetectionMode::Putting, ShotDetectionMode::Chipping] {
            let (ball, derived) = handle_d4(DeviceModel::Xi, mode, None, putt.clone());
            assert_eq!(ball.backspin_rpm, Some(0), "{mode:?}");
            assert_eq!(ball.max_height, Some(Distance::Meters(0.0)), "{mode:?}");
            let d = derived.expect("rolled");
            assert!(!d.spin_estimated, "{mode:?}");
            let path = d.path.expect("path");
            assert_eq!(path.flight, vec![[0.0; 4]], "{mode:?}");
            assert!(d.total_distance.expect("total").as_meters() > 0.0);
        }
    }

    /// With the simulation off, the estimated spin still goes out flagged:
    /// a derived block that holds only `spin_estimated`.
    #[test]
    fn launch_only_chip_flags_its_spin_with_the_simulation_off() {
        let (ball, derived) = handle_d4_with(
            DeviceModel::Xi,
            ShotDetectionMode::Chipping,
            None,
            flighthook::SimMode::Off,
            launch_only_d4(),
        );
        assert_eq!(ball.backspin_rpm, Some(1570));
        let mut only_the_flag = flighthook::DerivedFlight::default();
        only_the_flag.spin_estimated = true;
        assert_eq!(derived, Some(only_the_flag));
    }

    /// A shot with the device's own spin is not flagged, simulated or not.
    #[test]
    fn measured_spin_is_not_flagged_as_estimated() {
        let (d4, _) = shot_with_club_data();
        for model in [DeviceModel::Xi, DeviceModel::Mevo] {
            let (ball, derived) = handle_d4(model, ShotDetectionMode::Full, None, d4.clone());
            assert_eq!(ball.backspin_rpm, Some(d4.backspin_rpm), "{model:?}");
            let d = derived.expect("simulated");
            assert!(!d.spin_estimated, "{model:?}");
            let (_, derived) = handle_d4_with(
                model,
                ShotDetectionMode::Full,
                None,
                flighthook::SimMode::Off,
                d4.clone(),
            );
            assert_eq!(derived, None, "{model:?}");
        }
    }
}
