use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use allsquare::{Client, Event, SpinMode, ble};
use tracing::{debug, info, warn};

use super::backoff::Backoff;
use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::raw_capture::{self, RawCapture, RawCaptureConfig};
use crate::sim;
use crate::state::SystemState;

use flighthook::{
    ActorStatus, BallFlight, Club, ClubData, Distance, FaceImpact, FlighthookEvent,
    FlighthookMessage, Handedness, RawDir, RawRecord, Severity, ShotKey, Velocity,
};

/// Reconnect backoff bounds (linear: +1s per failed attempt, capped at 15s,
/// back to the minimum after a session that lasted 10s).
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(15);

/// Square Golf Omni BLE actor. Connects over BLE — the device is never paired
/// or bonded — selects a club, arms ball detection and processes shots.
///
/// The original Square / Square Home is **not** supported: it uses a different
/// club-code scheme, so club selection would be wrong on all but the putter.
pub struct SquareActor {
    pub address: Option<String>,
    pub club: Club,
    pub advanced_spin: bool,
    /// Discard shots that read zero spin, unless the putter is selected.
    pub discard_non_putting_zero_spin: bool,
    /// Per-club face impact calibration, built from allsquare's defaults plus
    /// any configured overrides.
    pub impact_calibration: allsquare::ImpactCalibration,
}

impl Actor for SquareActor {
    fn start(&self, state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let address = self.address.clone();
        let club = self.club;
        let advanced_spin = self.advanced_spin;
        let discard_zero_spin = self.discard_non_putting_zero_spin;
        let impact_calibration = self.impact_calibration.clone();
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run(
                    &state,
                    address,
                    club,
                    advanced_spin,
                    discard_zero_spin,
                    impact_calibration,
                    sender,
                    receiver,
                );
            })
            .expect("failed to spawn square thread");
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let actor_id = sender.actor_id();
        let Some((_, index)) = actor_id.split_once('.') else {
            return ReconfigureOutcome::Applied;
        };

        let snap = state.system.snapshot();
        match snap.square.get(index) {
            // Address, club and spin mode are applied at connect time.
            Some(section) => {
                let impact_calibration =
                    build_impact_calibration(section.dot_bottom_to_face_centre_mm.as_ref());
                if section.address == self.address && impact_calibration == self.impact_calibration
                {
                    ReconfigureOutcome::Applied
                } else {
                    ReconfigureOutcome::RestartRequired
                }
            }
            None => ReconfigureOutcome::RestartRequired, // section removed
        }
    }
}

/// Build the per-club face impact calibration from config overrides.
///
/// Starts from allsquare's built-in defaults and applies each override.
/// Unknown club keys, clubs Square Golf has no code for, and non-finite or
/// negative values are warned about and skipped, leaving that club at its
/// built-in default.
pub(crate) fn build_impact_calibration(
    overrides: Option<&BTreeMap<String, f64>>,
) -> allsquare::ImpactCalibration {
    let mut cal = allsquare::ImpactCalibration::default();
    let Some(overrides) = overrides else {
        return cal;
    };
    for (key, &mm) in overrides {
        let Some(club) = Club::from_code(key) else {
            warn!("square: unknown club '{key}' in dot_bottom_to_face_centre_mm, ignoring");
            continue;
        };
        let Some(club) = allsquare_club(club) else {
            warn!(
                "square: Square Golf has no code for '{key}'; it is armed as {}, whose \
                 calibration applies; ignoring its dot_bottom_to_face_centre_mm",
                to_allsquare_club(club).to_string().to_uppercase()
            );
            continue;
        };
        if !mm.is_finite() || mm < 0.0 {
            warn!("square: invalid dot_bottom_to_face_centre_mm for '{key}': {mm}, ignoring");
            continue;
        }
        cal.set_dot_bottom_to_face_centre_mm(club, Some(mm));
    }
    cal
}

// ---------------------------------------------------------------------------
// Raw capture
// ---------------------------------------------------------------------------

/// Frames never captured: heartbeat acks, query replies and our heartbeats.
/// Neither a heartbeat ack nor a query reply is shot data.
fn export_excluded(rec: &RawRecord) -> bool {
    match rec.dir {
        RawDir::Rx => rec
            .hex
            .as_deref()
            .and_then(decode_hex)
            .and_then(|bytes| allsquare::protocol::parse(&bytes).ok())
            .is_some_and(|n| {
                matches!(
                    n,
                    allsquare::protocol::Notification::HeartbeatAck(_)
                        | allsquare::protocol::Notification::QueryResponse(_)
                )
            }),
        RawDir::Tx => {
            rec.hex
                .as_deref()
                .and_then(decode_hex)
                .and_then(|bytes| allsquare::Command::type_byte_of(&bytes))
                == Some(allsquare::Command::Heartbeat.type_byte())
        }
    }
}

/// The bytes of a lowercase hex string, or `None` when it is not valid hex.
fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.is_ascii() || !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Raw capture for one Square Golf connection. The pre-roll holds the ball
/// notification, our club metrics request and the club notification, which
/// all precede allsquare's shot event.
fn raw_capture_config() -> RawCaptureConfig {
    RawCaptureConfig {
        transport: "square-ble",
        pre_roll_frames: 64,
        max_frames: 64,
        max_bulk_bytes: 0,
        exclude: export_excluded,
        is_bulk: raw_capture::no_bulk,
    }
}

/// Transport wrapper that records notifications and commands for the shot
/// export. Characteristic reads, where identity lives, are never recorded,
/// and the capture starts disabled until the identity exchange is done.
struct Tap<T> {
    inner: T,
    capture: RawCapture,
}

impl<T> Tap<T> {
    fn new(inner: T) -> Self {
        let mut capture = RawCapture::new(raw_capture_config());
        capture.set_enabled(false);
        Self { inner, capture }
    }
}

impl<T: allsquare::Transport> allsquare::Transport for Tap<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.capture.push(RawRecord {
                r#type: Some(raw_capture::type_label(buf[0])),
                channel: Some("notify".into()),
                ..RawRecord::binary(RawDir::Rx, &buf[..n])
            });
        }
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.inner.write(data)?;
        self.capture.push(RawRecord {
            r#type: allsquare::Command::type_byte_of(data).map(raw_capture::type_label),
            channel: Some("cmd".into()),
            ..RawRecord::binary(RawDir::Tx, data)
        });
        Ok(())
    }

    fn read_characteristic(&mut self, uuid: u128) -> io::Result<Vec<u8>> {
        self.inner.read_characteristic(uuid)
    }

    fn model_hint(&self) -> Option<String> {
        self.inner.model_hint()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Whether `address` is the all-zero placeholder macOS reports in place of a
/// hidden BLE address. It identifies no device.
fn is_zero_address(address: &str) -> bool {
    address.trim().eq_ignore_ascii_case("00:00:00:00:00:00")
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

/// The device hand for the player's handedness; right when unknown.
fn handed_of(handed: Option<Handedness>) -> allsquare::Handed {
    if handed == Some(Handedness::Left) {
        allsquare::Handed::Left
    } else {
        allsquare::Handed::Right
    }
}

/// Map a flighthook club to the allsquare club the device is set to.
///
/// A club change coming from GSPro is pushed straight to the device. This
/// matters more here than on other monitors: Square Golf uses the selected
/// club to classify the shot, and putting mode is nothing more than "the putter
/// is selected". A club the device has no code for (see [`allsquare_club`]) is
/// sent as the nearest club of the same kind it does have, taking the stronger
/// loft on a tie.
fn to_allsquare_club(club: Club) -> allsquare::Club {
    match club {
        Club::Driver | Club::MiniDriver => allsquare::Club::Driver,
        Club::Wood2 | Club::Wood3 | Club::Wood4 => allsquare::Club::Wood3,
        Club::Wood5 | Club::Wood6 => allsquare::Club::Wood5,
        Club::Wood7 | Club::Wood8 | Club::Wood9 => allsquare::Club::Wood7,
        Club::Hybrid1 | Club::Hybrid2 | Club::Hybrid3 => allsquare::Club::Hybrid3,
        Club::Hybrid4 => allsquare::Club::Hybrid4,
        Club::Hybrid5 | Club::Hybrid6 | Club::Hybrid7 | Club::Hybrid8 | Club::Hybrid9 => {
            allsquare::Club::Hybrid5
        }
        Club::Iron1 | Club::Iron2 | Club::Iron3 => allsquare::Club::Iron3,
        Club::Iron4 => allsquare::Club::Iron4,
        Club::Iron5 => allsquare::Club::Iron5,
        Club::Iron6 => allsquare::Club::Iron6,
        Club::Iron7 => allsquare::Club::Iron7,
        Club::Iron8 => allsquare::Club::Iron8,
        Club::Iron9 => allsquare::Club::Iron9,
        Club::PitchingWedge => allsquare::Club::PitchingWedge,
        Club::GapWedge => allsquare::Club::GapWedge,
        Club::SandWedge => allsquare::Club::SandWedge,
        Club::LobWedge => allsquare::Club::LobWedge,
        Club::Putter => allsquare::Club::Putter,
    }
}

/// The allsquare club with the same code, or `None` for a club Square Golf has
/// no code for (MD, 2W, 4W, 6W, 8W, 9W, 1H, 2H, 6H, 7H, 8H, 9H, 1I, 2I).
fn allsquare_club(club: Club) -> Option<allsquare::Club> {
    match club {
        Club::MiniDriver
        | Club::Wood2
        | Club::Wood4
        | Club::Wood6
        | Club::Wood8
        | Club::Wood9
        | Club::Hybrid1
        | Club::Hybrid2
        | Club::Hybrid6
        | Club::Hybrid7
        | Club::Hybrid8
        | Club::Hybrid9
        | Club::Iron1
        | Club::Iron2 => None,
        _ => Some(to_allsquare_club(club)),
    }
}

// ---------------------------------------------------------------------------
// Protocol type -> bus type conversion helpers
// ---------------------------------------------------------------------------

/// Whether a shot is a spin misread and should be dropped rather than forwarded.
///
/// The device occasionally returns a shot with no spin at all, typically a ball
/// struck at the very front of the detection zone. A real strike always imparts
/// spin, and a spinless shot handed to a sim carries much further than it should
/// — a zero-spin 8 iron flies roughly 30 yards long — so it is better to lose the
/// shot than to record a wrong one.
///
/// The putter is exempt, and it is the only exemption that makes sense. A putt
/// has no airborne flight for the device to measure spin over, so it reads zero
/// every time; discarding those would make putting impossible. Every other club
/// is a struck shot that should show spin, whatever the distance.
fn is_zero_spin_misread(b: &allsquare::BallMetrics, club: Club, enabled: bool) -> bool {
    if !enabled || club == Club::Putter {
        return false;
    }
    b.total_spin == 0 && b.back_spin == 0 && b.side_spin == 0
}

fn ball_from_square(b: &allsquare::BallMetrics) -> BallFlight {
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(b.speed)),
        launch_elevation: Some(b.launch_angle),
        launch_azimuth: Some(b.direction),
        // The device measures launch only; flighthook simulates the flight.
        backspin_rpm: Some(i32::from(b.back_spin)),
        // Negated: the device reports negative sidespin for rightward curve,
        // FRP defines positive as rightward. Without this, fades draw and
        // draws fade in GSPro (which derives spin axis from these components).
        sidespin_rpm: Some(-i32::from(b.side_spin)),
        // The device reports total spin and axis alongside the components.
        // The axis has the device's sidespin polarity (negative = right), so
        // it is negated too. A total of 0 has no axis.
        total_spin_rpm: Some(i32::from(b.total_spin)),
        spin_axis: (b.total_spin != 0).then_some(-b.spin_axis),
        ..BallFlight::default()
    }
}

fn club_from_square(c: &allsquare::ClubMetrics) -> ClubData {
    ClubData {
        club_speed: c.club_speed.map(Velocity::MetersPerSecond),
        club_speed_post: None,
        path: c.path,
        attack_angle: c.attack_angle,
        face_angle: c.face_angle,
        // Unlike the R10, the Omni reports these two.
        dynamic_loft: c.dynamic_loft,
        smash_factor: c.smash_factor,
        swing_plane_horizontal: None,
        swing_plane_vertical: None,
        // Impact location belongs in FaceImpact, not here.
        club_offset: None,
        club_height: None,
        ..ClubData::default()
    }
}

/// Impact location as millimetres from face centre, if the device measured it.
///
/// allsquare reports both axes from face centre (beta calibration, with the
/// vertical offset taken from the armed club). Lateral is negated: the device
/// reports negative toward the toe, FRP defines positive toward the toe.
fn face_impact_from_square(c: &allsquare::ClubMetrics) -> Option<FaceImpact> {
    if c.impact_horizontal.is_none() && c.impact_vertical.is_none() {
        return None;
    }
    Some(FaceImpact {
        lateral: c.impact_horizontal.map(|v| Distance::Millimeters(-v)),
        vertical: c.impact_vertical.map(Distance::Millimeters),
    })
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run(
    state: &SystemState,
    address: Option<String>,
    club: Club,
    advanced_spin: bool,
    discard_zero_spin: bool,
    impact_calibration: allsquare::ImpactCalibration,
    sender: BusSender,
    mut receiver: BusReceiver,
) {
    let address = address.filter(|a| {
        let zero = is_zero_address(a);
        if zero {
            warn!(
                "ignoring configured address {a}: it is the placeholder macOS reports \
                 for a hidden BLE address, not a device address; auto-discovering instead"
            );
        }
        !zero
    });
    let mut backoff = Backoff::new(MIN_BACKOFF, MAX_BACKOFF);
    let mut ever_connected = false;
    let mut device_id: Option<String> = None;
    // Latched across reconnects so the device comes back with the right club.
    let mut current_club = club;

    loop {
        if receiver.poll().is_err() {
            break;
        }

        emit_device_status(&sender, ActorStatus::Starting, HashMap::new());

        let mut connected_at = None;
        match connect_and_run(
            state,
            address.as_deref(),
            &mut current_club,
            advanced_spin,
            discard_zero_spin,
            &impact_calibration,
            &sender,
            &mut receiver,
            &mut connected_at,
            &mut device_id,
        ) {
            Ok(()) => break,
            Err(e) => {
                let session = connected_at.map(|at| at.elapsed());
                ever_connected |= session.is_some();
                warn!("session error: {e}");
                emit_alert(&sender, Severity::Warn, format!("Square Golf error: {e}"));

                let backoff_status = if ever_connected {
                    ActorStatus::Reconnecting
                } else {
                    ActorStatus::Starting
                };
                emit_device_status(&sender, backoff_status, HashMap::new());
                if let Some(dev) = device_id.as_deref() {
                    sender.send(
                        FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                            manufacturer: None,
                            model: None,
                            firmware: None,
                            telemetry: Some(HashMap::from([("ready".into(), "false".into())])),
                        })
                        .device(dev),
                    );
                }

                let verb = if ever_connected {
                    "reconnecting"
                } else {
                    "retrying"
                };
                let delay = backoff.delay(session);
                info!("{verb} in {}s", delay.as_secs());

                let deadline = Instant::now() + delay;
                while Instant::now() < deadline {
                    if receiver.poll().is_err() {
                        emit_device_status(&sender, ActorStatus::Disconnected, HashMap::new());
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
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
    address: Option<&str>,
    current_club: &mut Club,
    advanced_spin: bool,
    discard_zero_spin: bool,
    impact_calibration: &allsquare::ImpactCalibration,
    sender: &BusSender,
    receiver: &mut BusReceiver,
    connected_at: &mut Option<Instant>,
    device_id: &mut Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("searching for Square Golf device...");
    let transport = ble::connect(address)?;
    *connected_at = Some(Instant::now());

    let name = transport.name().to_string();
    let ble_address = transport.address().to_string();
    // Log the address explicitly: after a first auto-discovered connect the
    // user needs it to pin this device in config, and auto-discovery would
    // otherwise never show it anywhere.
    info!("connected to {name} at {ble_address}");
    *device_id = Some(name.clone());

    run_session(
        transport,
        name,
        ble_address,
        state,
        current_club,
        advanced_spin,
        discard_zero_spin,
        impact_calibration,
        sender,
        receiver,
    )
}

/// Drive a connected device until shutdown or a link error: follow the bus
/// (club, handedness), arm the device, and publish its shots.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn run_session<T: allsquare::Transport>(
    transport: T,
    name: String,
    ble_address: String,
    state: &SystemState,
    current_club: &mut Club,
    advanced_spin: bool,
    discard_zero_spin: bool,
    impact_calibration: &allsquare::ImpactCalibration,
    sender: &BusSender,
    receiver: &mut BusReceiver,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut client = Client::new(Tap::new(transport));
    client.set_impact_calibration(impact_calibration.clone());
    // Every club select carries the player's hand.
    let mut hand = handed_of(state.game.snapshot().handed);
    client.set_handed(hand);
    let spin = if advanced_spin {
        SpinMode::Advanced
    } else {
        SpinMode::Standard
    };
    // Whether the device has been armed; a hand change before that only
    // records the hand, which the arm then carries.
    let mut armed = false;

    let mut shot_counter: u32 = 0;
    let mut discarded_counter: u32 = 0;
    let mut idle_count: u32 = 0;
    let mut device_telemetry: HashMap<String, String> = HashMap::new();

    loop {
        // ==============================================================
        // Phase 1: Drain bus commands
        // ==============================================================
        // A club or hand change seen in this drain; one select after it
        // carries both.
        let mut reselect = false;
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => {
                    let _ = client.disarm();
                    return Ok(());
                }
                Ok(None) => break,
                Ok(Some(msg)) => match msg.event {
                    // Follow the simulator's club selection. Square Golf uses
                    // it to classify the shot, so keeping it in sync matters.
                    FlighthookEvent::ClubInfo { club_info } => {
                        if club_info.club != *current_club {
                            *current_club = club_info.club;
                            reselect = true;
                        }
                    }
                    // Follow the player's hand: the system actor's command, whatever set it.
                    // The same event can carry a mode, which does not apply
                    // (see below).
                    FlighthookEvent::SetDetectionMode { mode, handed } => {
                        if let Some(handed) = handed {
                            let new_hand = handed_of(Some(handed));
                            if new_hand != hand {
                                hand = new_hand;
                                client.set_handed(hand);
                                // Before the arm the hand is only recorded.
                                reselect |= armed;
                            }
                        }
                        // There is no device-side shot mode: putting and
                        // chipping differ only by which club is selected.
                        if let Some(new_mode) = mode {
                            debug!(
                                "mode change to {new_mode:?} ignored \
                                 — Square Golf has no mode control, select a club"
                            );
                        }
                    }
                    _ => {}
                },
            }
        }
        if reselect {
            let mapped = to_allsquare_club(*current_club);
            info!("club -> {mapped}, hand {hand:?}");
            if let Err(e) = client.select_club(mapped) {
                warn!("club select failed: {e}");
            }
        }

        // ==============================================================
        // Phase 2: Drive the Square Golf client
        // ==============================================================
        match client.poll() {
            Ok(Some(event)) => {
                idle_count = 0;

                match event {
                    Event::Connected {
                        firmware,
                        hardware,
                        device_id: serial,
                        model,
                    } => {
                        info!(
                            "model {model}, firmware lm {} (mmi {}), hw {hardware}, id {serial}",
                            firmware.lm, firmware.mmi
                        );
                        // Identity has been read; capture from here on.
                        client.transport_mut().capture.set_enabled(true);
                        emit_device_status(
                            sender,
                            ActorStatus::Connected,
                            HashMap::from([
                                (
                                    "device_info".into(),
                                    if model.is_empty() {
                                        format!("Square Golf {name} ({ble_address})")
                                    } else {
                                        format!("Square Golf {model} {name} ({ble_address})")
                                    },
                                ),
                                // Surfaced on the bus (so it reaches the UI log
                                // and telemetry) specifically so a user who
                                // auto-discovered can copy it into config.
                                ("ble_address".into(), ble_address.clone()),
                                ("shot_count".into(), shot_counter.to_string()),
                            ]),
                        );

                        device_telemetry.insert("ready".into(), "false".into());
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: Some("Invant".into()),
                                // What the device calls itself, e.g. SGO300A.
                                model: if model.is_empty() {
                                    None
                                } else {
                                    Some(model.clone())
                                },
                                firmware: Some(firmware.lm.clone()),
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&name),
                        );

                        // Arm once — the device stays armed across shots.
                        client.arm(to_allsquare_club(*current_club), spin)?;
                        armed = true;
                        info!("armed with {}", to_allsquare_club(*current_club));
                    }

                    Event::StateChanged(state) => {
                        use allsquare::DeviceState;
                        debug!("device state: {state:?}");
                        let ready = state == DeviceState::Ready;
                        let label = match state {
                            DeviceState::Detect => Some("waiting"),
                            DeviceState::Ready => Some("ready"),
                            DeviceState::Shot => Some("recording"),
                            DeviceState::Done => Some("processing"),
                            _ => None,
                        };
                        device_telemetry.insert("ready".into(), ready.to_string());
                        if let Some(label) = label {
                            device_telemetry.insert("device_state".into(), label.into());
                        }
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: None,
                                model: None,
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&name),
                        );
                    }

                    Event::Battery { percent, state } => {
                        debug!("battery {percent}% {state:?}");
                        device_telemetry.insert("battery_pct".into(), percent.to_string());
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: None,
                                model: None,
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&name),
                        );
                    }

                    Event::Shot { ball, club } => {
                        // Drop before the counter advances, so a discarded shot
                        // leaves no gap in the numbering and no half-shot on the
                        // bus — nothing downstream ever learns it happened.
                        if is_zero_spin_misread(&ball, *current_club, discard_zero_spin) {
                            let mph = Velocity::MetersPerSecond(ball.speed).as_mph();
                            discarded_counter += 1;
                            warn!("discarded shot: {mph:.1}mph with zero spin (misread)");
                            emit_alert(
                                sender,
                                Severity::Warn,
                                format!(
                                    "Square Golf: discarded a {mph:.0} mph shot that read zero \
                                     spin — the ball was likely too far forward in the hitting \
                                     zone. Re-hit it."
                                ),
                            );
                            // Also surface a running count as telemetry. The log
                            // line says one shot was lost; this says whether it
                            // is a one-off or a ball-position problem worth
                            // fixing on the mat.
                            device_telemetry
                                .insert("zero_spin_discards".into(), discarded_counter.to_string());
                            sender.send(
                                FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                    manufacturer: None,
                                    model: None,
                                    firmware: None,
                                    telemetry: Some(device_telemetry.clone()),
                                })
                                .device(&name),
                            );
                            client.transport_mut().capture.discard();
                            continue;
                        }

                        shot_counter += 1;
                        let key = ShotKey {
                            shot_id: uuid::Uuid::new_v4().to_string(),
                            shot_number: shot_counter,
                        };
                        info!("shot #{}", key.shot_number);
                        client.transport_mut().capture.mark_trigger();

                        emit_device_status(
                            sender,
                            ActorStatus::Connected,
                            HashMap::from([("shot_count".into(), shot_counter.to_string())]),
                        );

                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                                key: key.clone(),
                            })
                            .device(&name),
                        );

                        let bf = ball_from_square(&ball);
                        info!(
                            "  ball: {:.1}mph VLA={:.1} HLA={:.1} back={}rpm side={}rpm",
                            bf.launch_speed.map_or(0.0, Velocity::as_mph),
                            bf.launch_elevation.unwrap_or(0.0),
                            bf.launch_azimuth.unwrap_or(0.0),
                            bf.backspin_rpm.unwrap_or(0),
                            bf.sidespin_rpm.unwrap_or(0),
                        );
                        sender.send(
                            FlighthookMessage::new(sim::ball_flight_event(state, key.clone(), bf))
                                .device(&name),
                        );

                        // Club data is absent when the device could not track
                        // the sticker — a putt, or a mishit it declined.
                        if let Some(ref c) = club {
                            let cd = club_from_square(c);
                            info!(
                                "  club: {:.1}mph face={:.1} path={:.1} AoA={:.1} smash={:.2}",
                                cd.club_speed.map_or(0.0, Velocity::as_mph),
                                cd.face_angle.unwrap_or(0.0),
                                cd.path.unwrap_or(0.0),
                                cd.attack_angle.unwrap_or(0.0),
                                cd.smash_factor.unwrap_or(0.0),
                            );
                            sender.send(
                                FlighthookMessage::new(FlighthookEvent::ClubPath {
                                    key: key.clone(),
                                    club: Box::new(cd),
                                })
                                .device(&name),
                            );

                            if let Some(impact) = face_impact_from_square(c) {
                                info!(
                                    "  impact (beta): toe={:.1}mm up={:.1}mm",
                                    impact.lateral.map_or(f64::NAN, |d| d.as_millimeters()),
                                    impact.vertical.map_or(f64::NAN, |d| d.as_millimeters()),
                                );
                                sender.send(
                                    FlighthookMessage::new(FlighthookEvent::FaceImpact {
                                        key: key.clone(),
                                        impact: Box::new(impact),
                                    })
                                    .device(&name),
                                );
                            }
                        } else {
                            debug!("  no club data (sticker not tracked)");
                        }

                        let raw = client.transport_mut().capture.take();
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::ShotFinished {
                                key,
                                raw: Some(Box::new(raw)),
                            })
                            .device(&name),
                        );
                    }

                    Event::Sensor(_) | Event::Clock(_) | Event::Alignment(_) => {}

                    _ => {}
                }
            }

            Ok(None) => {
                idle_count += 1;
                if idle_count >= 3 {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }

            Err(allsquare::Error::Disconnected) => {
                info!("device closed the link");
                return Err(Box::new(allsquare::Error::Disconnected));
            }
            Err(allsquare::Error::Transport(e)) => {
                info!("BLE transport error: {e}");
                return Err(Box::new(allsquare::Error::Transport(e)));
            }
            Err(e) => {
                warn!("protocol error: {e}");
                emit_alert(sender, Severity::Warn, format!("Square Golf: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use allsquare::testing::{self, BALL_NOTIFICATION};
    use allsquare::{ClubMetrics, DeviceState};
    use flighthook::ShotDetectionMode;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The device reports negative toward the toe; FRP's lateral is positive
    /// toward the toe. Vertical passes through.
    #[test]
    fn face_impact_flips_lateral_to_frp() {
        let c = allsquare::ClubMetrics {
            impact_horizontal: Some(-12.5),
            impact_vertical: Some(3.0),
            ..Default::default()
        };
        let impact = face_impact_from_square(&c).expect("impact");
        assert_eq!(impact.lateral, Some(Distance::Millimeters(12.5)));
        assert_eq!(impact.vertical, Some(Distance::Millimeters(3.0)));
    }

    /// No vertical estimate (e.g. the putter) still reports lateral.
    #[test]
    fn face_impact_without_vertical_keeps_lateral() {
        let c = allsquare::ClubMetrics {
            impact_horizontal: Some(4.0),
            ..Default::default()
        };
        let impact = face_impact_from_square(&c).expect("impact");
        assert_eq!(impact.lateral, Some(Distance::Millimeters(-4.0)));
        assert_eq!(impact.vertical, None);
        assert!(face_impact_from_square(&allsquare::ClubMetrics::default()).is_none());
    }

    /// The device reports negative sidespin for a rightward curve; FRP defines
    /// positive as rightward. Getting this backwards makes fades draw and draws
    /// fade in GSPro, which derives spin axis from the spin components.
    #[test]
    fn sidespin_polarity_is_inverted() {
        let fade = allsquare::BallMetrics {
            speed: 3.30,
            launch_angle: 32.51,
            direction: 7.39,
            total_spin: 627,
            spin_axis: -8.00,
            back_spin: 621,
            side_spin: -87,
            ..testing::ball_metrics()
        };
        let bf = ball_from_square(&fade);
        assert_eq!(bf.sidespin_rpm, Some(87), "device -87 (right) -> FRP +87");
        assert_eq!(bf.backspin_rpm, Some(621), "backspin is not flipped");
        assert_eq!(bf.launch_azimuth, Some(7.39), "azimuth is not flipped");
    }

    /// Device sidespin polarity carries through FRP into the simulation: the
    /// device's negative (rightward) sidespin must land the ball right.
    #[test]
    fn device_fade_lands_right_in_the_simulation() {
        let fade = allsquare::BallMetrics {
            speed: 3.30,
            launch_angle: 32.51,
            direction: 7.39,
            total_spin: 627,
            spin_axis: -8.00,
            back_spin: 621,
            side_spin: -87,
            ..testing::ball_metrics()
        };
        let cfg = flighthook::SimulationSection::default();
        let d = sim::derive(&ball_from_square(&fade), &cfg).expect("simulated");
        let off = d.carry_offline.expect("offline").as_meters();
        assert!(off > 0.0, "{off}");

        // The same fade at full speed and launched straight, so only the
        // sidespin can move it right.
        let straight = allsquare::BallMetrics {
            speed: 60.0,
            launch_angle: 14.0,
            direction: 0.0,
            back_spin: 3000,
            side_spin: -600,
            ..fade
        };
        let d = sim::derive(&ball_from_square(&straight), &cfg).expect("simulated");
        let off = d.carry_offline.expect("offline").as_meters();
        assert!(off > 0.0, "{off}");
    }

    fn ball(speed_ms: f64, total: i16, back: i16, side: i16) -> allsquare::BallMetrics {
        allsquare::BallMetrics {
            speed: speed_ms,
            launch_angle: 18.0,
            total_spin: total,
            back_spin: back,
            side_spin: side,
            ..testing::ball_metrics()
        }
    }

    /// A full-swing club reading zero spin is a misread, whatever the ball
    /// speed: a struck ball always spins, and a spinless shot flies far too
    /// long in the sim.
    #[test]
    fn rejects_zero_spin_shot() {
        assert!(is_zero_spin_misread(
            &ball(49.0, 0, 0, 0),
            Club::Iron8,
            true
        ));
    }

    /// Distance is irrelevant: a chip is a struck shot and should show spin, so
    /// a zero-spin chip is a misread like any other.
    #[test]
    fn rejects_zero_spin_chip() {
        // 8 m/s ~= 18 mph, a 10-yard chip.
        assert!(is_zero_spin_misread(
            &ball(8.0, 0, 0, 0),
            Club::LobWedge,
            true
        ));
    }

    /// The putter is the one real exemption: there is no airborne flight to
    /// measure spin over, so a putt reads zero every time and discarding those
    /// would make putting impossible.
    #[test]
    fn never_discards_a_putt() {
        // 3.3 m/s ~= 7 mph, a normal putt.
        assert!(!is_zero_spin_misread(
            &ball(3.3, 0, 0, 0),
            Club::Putter,
            true
        ));
        // Even a rammed one.
        assert!(!is_zero_spin_misread(
            &ball(9.0, 0, 0, 0),
            Club::Putter,
            true
        ));
    }

    /// Any measured spin means the read succeeded, however fast the ball.
    #[test]
    fn allows_shot_with_spin() {
        assert!(!is_zero_spin_misread(
            &ball(70.0, 2400, 2400, 0),
            Club::Driver,
            true
        ));
        // Sidespin alone still counts as a successful read.
        assert!(!is_zero_spin_misread(
            &ball(70.0, 0, 0, -87),
            Club::Driver,
            true
        ));
    }

    /// Disabled means disabled — nothing is examined, whatever the club.
    #[test]
    fn disabled_forwards_everything() {
        assert!(!is_zero_spin_misread(
            &ball(80.0, 0, 0, 0),
            Club::Driver,
            false
        ));
        assert!(!is_zero_spin_misread(
            &ball(3.3, 0, 0, 0),
            Club::Putter,
            false
        ));
    }

    #[test]
    fn zero_address_is_detected() {
        assert!(is_zero_address("00:00:00:00:00:00"));
        assert!(is_zero_address(" 00:00:00:00:00:00\n"));
        assert!(!is_zero_address("02:00:00:AB:CD:01"));
        assert!(!is_zero_address("SquareGolf(CD01)"));
    }

    #[test]
    fn no_overrides_matches_allsquare_default() {
        assert_eq!(
            build_impact_calibration(None),
            allsquare::ImpactCalibration::default()
        );
    }

    #[test]
    fn valid_override_is_applied() {
        let mut overrides = BTreeMap::new();
        overrides.insert("DR".to_string(), 30.0);
        let cal = build_impact_calibration(Some(&overrides));
        assert_eq!(
            cal.dot_bottom_to_face_centre_mm(allsquare::Club::Driver),
            Some(30.0)
        );
    }

    /// An unknown club key is warned about and skipped — every other club
    /// still gets allsquare's built-in default.
    #[test]
    fn unknown_club_key_is_skipped() {
        let mut overrides = BTreeMap::new();
        overrides.insert("bogus".to_string(), 30.0);
        let cal = build_impact_calibration(Some(&overrides));
        assert_eq!(cal, allsquare::ImpactCalibration::default());
    }

    /// A club the device has no code for has no calibration of its own; its
    /// key is skipped rather than overriding the club it is armed as.
    #[test]
    fn club_without_device_code_is_skipped() {
        let mut overrides = BTreeMap::new();
        overrides.insert("6H".to_string(), 30.0);
        let cal = build_impact_calibration(Some(&overrides));
        assert_eq!(cal, allsquare::ImpactCalibration::default());
    }

    /// Every club the device has no code for is armed as its nearest club.
    #[test]
    fn clubs_without_device_code_map_to_nearest() {
        let cases = [
            (Club::MiniDriver, allsquare::Club::Driver),
            (Club::Wood2, allsquare::Club::Wood3),
            (Club::Wood4, allsquare::Club::Wood3),
            (Club::Wood6, allsquare::Club::Wood5),
            (Club::Wood8, allsquare::Club::Wood7),
            (Club::Wood9, allsquare::Club::Wood7),
            (Club::Hybrid1, allsquare::Club::Hybrid3),
            (Club::Hybrid2, allsquare::Club::Hybrid3),
            (Club::Hybrid6, allsquare::Club::Hybrid5),
            (Club::Hybrid7, allsquare::Club::Hybrid5),
            (Club::Hybrid8, allsquare::Club::Hybrid5),
            (Club::Hybrid9, allsquare::Club::Hybrid5),
            (Club::Iron1, allsquare::Club::Iron3),
            (Club::Iron2, allsquare::Club::Iron3),
        ];
        for (club, expected) in cases {
            assert_eq!(allsquare_club(club), None, "{club}");
            assert_eq!(to_allsquare_club(club), expected, "{club}");
        }
        let exact = Club::ALL
            .iter()
            .filter(|&&c| allsquare_club(c).is_some())
            .count();
        assert_eq!(exact, 19);
    }

    /// A negative value is nonsensical (it is a distance) and is skipped —
    /// the club keeps its built-in default rather than taking a bad value.
    #[test]
    fn negative_value_is_skipped() {
        let mut overrides = BTreeMap::new();
        overrides.insert("DR".to_string(), -5.0);
        let cal = build_impact_calibration(Some(&overrides));
        assert_eq!(
            cal.dot_bottom_to_face_centre_mm(allsquare::Club::Driver),
            allsquare::ImpactCalibration::default()
                .dot_bottom_to_face_centre_mm(allsquare::Club::Driver)
        );
    }

    /// `reconfigure()` restarts the actor exactly when the built calibration
    /// changes — this is the equality check it relies on.
    #[test]
    fn calibration_changes_are_detected() {
        let mut overrides_a = BTreeMap::new();
        overrides_a.insert("DR".to_string(), 19.0);
        let mut overrides_b = overrides_a.clone();
        overrides_b.insert("DR".to_string(), 25.0);

        let cal_a = build_impact_calibration(Some(&overrides_a));
        let cal_a_again = build_impact_calibration(Some(&overrides_a));
        let cal_b = build_impact_calibration(Some(&overrides_b));

        assert_eq!(cal_a, cal_a_again, "same overrides must compare equal");
        assert_ne!(cal_a, cal_b, "changed overrides must compare unequal");
    }

    /// Holds a session inside its device read, between two bus drains.
    #[derive(Default)]
    struct Gate {
        closed: Mutex<()>,
        waiting: AtomicBool,
    }

    /// Scripted Omni: queued notifications, identity characteristics, and a
    /// log of every command written.
    #[derive(Default)]
    struct FakeTransport {
        notifications: std::collections::VecDeque<Vec<u8>>,
        written: Arc<Mutex<Vec<Vec<u8>>>>,
        gate: Option<Arc<Gate>>,
    }

    impl allsquare::Transport for FakeTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if let Some(gate) = &self.gate {
                gate.waiting.store(true, Ordering::SeqCst);
                drop(gate.closed.lock().unwrap_or_else(|e| e.into_inner()));
            }
            let Some(n) = self.notifications.pop_front() else {
                return Err(io::ErrorKind::WouldBlock.into());
            };
            buf[..n.len()].copy_from_slice(&n);
            Ok(n.len())
        }

        fn write(&mut self, data: &[u8]) -> io::Result<()> {
            self.written
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(data.to_vec());
            Ok(())
        }

        fn read_characteristic(&mut self, uuid: u128) -> io::Result<Vec<u8>> {
            use allsquare::protocol::uuid;
            match uuid {
                uuid::DEVICE_ID => Ok(b"SQ-123456".to_vec()),
                uuid::FW_VERSION => Ok(br#"{"lm":"1.0","mmi":"1.0","launcher":"1.0"}"#.to_vec()),
                uuid::HW_VERSION => Ok(b"1".to_vec()),
                _ => Err(io::ErrorKind::NotFound.into()),
            }
        }
    }

    #[test]
    fn tap_captures_the_shot_without_identity_or_housekeeping() {
        let club = testing::club_notification(&ClubMetrics::default());
        let notifications = [
            testing::query_response(&[0xaa, 0xbb]),
            testing::heartbeat_ack(DeviceState::Idle).to_vec(),
            testing::heartbeat_ack(DeviceState::Init).to_vec(),
            BALL_NOTIFICATION.to_vec(),
            BALL_NOTIFICATION.to_vec(),
            club.to_vec(),
        ];
        let fake = FakeTransport {
            notifications: notifications.into_iter().collect(),
            ..FakeTransport::default()
        };
        let mut client = Client::new(Tap::new(fake));

        let mut shot = false;
        for _ in 0..32 {
            match client.poll().expect("poll") {
                Some(Event::Connected { device_id, .. }) => {
                    assert_eq!(device_id, "SQ-123456");
                    client.transport_mut().capture.set_enabled(true);
                }
                Some(Event::Shot { .. }) => {
                    shot = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(shot, "no shot event");
        client.transport_mut().capture.mark_trigger();
        let raw = client.transport_mut().capture.take();

        let seq: Vec<(RawDir, Option<&str>, Option<String>)> = raw
            .frames
            .iter()
            .map(|r| (r.dir, r.channel.as_deref(), r.r#type.clone()))
            .collect();
        let notify = Some(raw_capture::type_label(BALL_NOTIFICATION[0]));
        let request = Some(raw_capture::type_label(
            allsquare::Command::RequestClubMetrics.type_byte(),
        ));
        assert_eq!(
            seq,
            [
                (RawDir::Rx, Some("notify"), notify.clone()),
                (RawDir::Tx, Some("cmd"), request),
                (RawDir::Rx, Some("notify"), notify.clone()),
                (RawDir::Rx, Some("notify"), notify),
            ]
        );
        assert_eq!(raw.transport, "square-ble");
        assert_eq!(
            raw.frames[0].hex.as_deref(),
            Some(flighthook::hex_lower(&BALL_NOTIFICATION).as_str())
        );
        assert_eq!(
            raw.frames[3].hex.as_deref(),
            Some(flighthook::hex_lower(&club).as_str())
        );
        // The replies to the identity exchange's query and heartbeat arrive
        // after the capture is enabled and are dropped by `export_excluded`;
        // the identity itself is never read through the tap.
        let json = serde_json::to_string(&raw).expect("ser");
        assert!(!json.contains(&flighthook::hex_lower(b"SQ-123456")));
        assert!(
            !json.contains(&flighthook::hex_lower(&testing::query_response(&[
                0xaa, 0xbb
            ])))
        );
        assert!(
            !json.contains(&flighthook::hex_lower(&testing::heartbeat_ack(
                DeviceState::Idle
            )))
        );
    }

    #[test]
    fn tap_records_nothing_until_enabled() {
        let fake = FakeTransport {
            notifications: [BALL_NOTIFICATION.to_vec()].into_iter().collect(),
            ..FakeTransport::default()
        };
        let mut tap = Tap::new(fake);
        let mut buf = [0u8; 64];
        assert_eq!(
            allsquare::Transport::read(&mut tap, &mut buf).expect("read"),
            17
        );
        allsquare::Transport::write(&mut tap, &allsquare::Command::RequestClubMetrics.encode(1))
            .expect("write");
        tap.capture.mark_trigger();
        assert!(tap.capture.take().frames.is_empty());
    }

    #[test]
    fn heartbeat_command_is_excluded() {
        let hb = RawRecord::binary(RawDir::Tx, &allsquare::Command::Heartbeat.encode(7));
        assert!(export_excluded(&hb));
        let req = RawRecord::binary(
            RawDir::Tx,
            &allsquare::Command::RequestClubMetrics.encode(7),
        );
        assert!(!export_excluded(&req));
        assert!(!export_excluded(&RawRecord::binary(
            RawDir::Rx,
            &BALL_NOTIFICATION
        )));
    }

    #[test]
    fn heartbeat_ack_and_query_reply_are_excluded() {
        let ack = RawRecord::binary(RawDir::Rx, &testing::heartbeat_ack(DeviceState::Ready));
        let query = RawRecord::binary(RawDir::Rx, &testing::QUERY_RESPONSE);
        assert!(export_excluded(&ack));
        assert!(export_excluded(&query));
        assert!(!export_excluded(&RawRecord::binary(
            RawDir::Rx,
            &BALL_NOTIFICATION
        )));
        // A frame that does not parse is kept, never dropped silently.
        let truncated = &testing::heartbeat_ack(DeviceState::Ready)[..3];
        assert!(!export_excluded(&RawRecord::binary(RawDir::Rx, truncated)));
        assert_eq!(decode_hex("0a1b"), Some(vec![0x0a, 0x1b]));
        assert_eq!(decode_hex("0a1"), None);
        assert_eq!(decode_hex("zz"), None);
        assert_eq!(decode_hex("é1"), None);
    }

    /// The device's total spin passes through; its axis is negated like its
    /// sidespin (device negative = right, FRP positive = right), so the two
    /// FRP spin pairs agree.
    #[test]
    fn total_spin_and_axis_follow_frp_signs() {
        let fade = allsquare::BallMetrics {
            speed: 3.30,
            launch_angle: 32.51,
            direction: 7.39,
            total_spin: 627,
            spin_axis: -8.00,
            back_spin: 621,
            side_spin: -87,
            ..testing::ball_metrics()
        };
        let bf = ball_from_square(&fade);
        assert_eq!(bf.total_spin_rpm, Some(627));
        assert_eq!(bf.spin_axis, Some(8.0), "device -8 (right) -> FRP +8");
        let mut components = BallFlight {
            backspin_rpm: bf.backspin_rpm,
            sidespin_rpm: bf.sidespin_rpm,
            ..BallFlight::default()
        };
        components.complete_spin();
        assert_eq!(components.total_spin_rpm, Some(627));
        assert!((components.spin_axis.unwrap() - 8.0).abs() < 0.1);
    }

    #[test]
    fn zero_total_spin_has_no_axis() {
        let bf = ball_from_square(&ball(1.5, 0, 0, 0));
        assert_eq!(bf.total_spin_rpm, Some(0));
        assert_eq!(bf.spin_axis, None);
    }

    #[test]
    fn handedness_maps_to_the_device_hand() {
        assert_eq!(handed_of(Some(Handedness::Left)), allsquare::Handed::Left);
        assert_eq!(handed_of(Some(Handedness::Right)), allsquare::Handed::Right);
        assert_eq!(handed_of(None), allsquare::Handed::Right);
    }

    /// A session against a scripted device on its own thread.
    struct Session {
        written: Arc<Mutex<Vec<Vec<u8>>>>,
        sender: BusSender,
        shutdown: Arc<AtomicBool>,
        gate: Arc<Gate>,
        thread: Option<std::thread::JoinHandle<Result<(), String>>>,
        dir: std::path::PathBuf,
    }

    impl Session {
        /// The player's hand is `hand` when the session starts; `queued` is
        /// on the bus before the session first looks at it.
        fn start(hand: Option<Handedness>, queued: Vec<FlighthookEvent>) -> Self {
            Self::launch(hand, queued, false)
        }

        /// A session with the system actor on its bus, so the hand is set
        /// with `hand_info` as every source sets it.
        fn with_system_actor() -> Self {
            Self::launch(None, vec![], true)
        }

        fn launch(
            hand: Option<Handedness>,
            queued: Vec<FlighthookEvent>,
            system_actor: bool,
        ) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flighthook-square-{}",
                crate::state::config::generate_id()
            ));
            let (state, writer) = SystemState::new(dir.join("config.toml"));
            let state = Arc::new(state);
            if let Some(hand) = hand {
                writer.set_handed(hand);
            }
            let (tx, _rx) = tokio::sync::broadcast::channel(1024);
            let shutdown = Arc::new(AtomicBool::new(false));
            if system_actor {
                crate::actors::system::start_for_test(writer, &state, &tx, shutdown.clone());
            }
            let sender = BusSender::new("square.0".into(), tx, shutdown.clone());
            let mut receiver = sender.subscribe();
            for event in queued {
                sender.send(FlighthookMessage::new(event));
            }
            let gate = Arc::new(Gate::default());
            let fake = FakeTransport {
                gate: Some(gate.clone()),
                ..FakeTransport::default()
            };
            let written = fake.written.clone();
            let session_sender = sender.clone();
            let thread = std::thread::spawn(move || {
                let mut club = Club::Iron7;
                run_session(
                    fake,
                    "SquareGolf(TEST)".into(),
                    "00:11:22:33:44:55".into(),
                    &state,
                    &mut club,
                    true,
                    true,
                    &allsquare::ImpactCalibration::default(),
                    &session_sender,
                    &mut receiver,
                )
                .map_err(|e| e.to_string())
            });
            Self {
                written,
                sender,
                shutdown,
                gate,
                thread: Some(thread),
                dir,
            }
        }

        fn send(&self, event: FlighthookEvent) {
            self.sender.send(FlighthookMessage::new(event));
        }

        /// Put `events` on the bus so the session drains them in one pass.
        fn send_together(&self, events: Vec<FlighthookEvent>) {
            let held = self.gate.closed.lock().unwrap_or_else(|e| e.into_inner());
            self.gate.waiting.store(false, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.gate.waiting.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            for event in events {
                self.send(event);
            }
            drop(held);
        }

        /// The club selects written so far, as the `(club, hand)` each one
        /// carries.
        fn selects(&self) -> Vec<(allsquare::Club, allsquare::Handed)> {
            use allsquare::Handed::{Left, Right};
            let probe = allsquare::Command::SelectClub {
                club: allsquare::Club::Driver,
                handed: Right,
            };
            let log = self.written.lock().unwrap_or_else(|e| e.into_inner());
            log.iter()
                .filter(|frame| allsquare::Command::type_byte_of(frame) == Some(probe.type_byte()))
                .map(|frame| {
                    let matches = |club, handed| {
                        let cmd = allsquare::Command::SelectClub { club, handed };
                        (0..=u8::MAX).any(|seq| cmd.encode(seq).as_slice() == frame.as_slice())
                    };
                    Club::ALL
                        .iter()
                        .map(|&club| to_allsquare_club(club))
                        .flat_map(|club| [(club, Right), (club, Left)])
                        .find(|&(club, handed)| matches(club, handed))
                        .unwrap_or_else(|| {
                            panic!("a club select for no club and hand: {frame:02x?}")
                        })
                })
                .collect()
        }

        /// Wait until `count` club selects have been written.
        fn wait_for(&self, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.selects().len() < count && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Wait for `count` club selects, then stop the session and return
        /// every select it wrote.
        fn finish(mut self, count: usize) -> Vec<(allsquare::Club, allsquare::Handed)> {
            self.wait_for(count);
            self.shutdown.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("session thread").expect("session ran");
            }
            self.selects()
        }
    }

    impl Drop for Session {
        /// Stops a session that `finish` did not (a failed assertion) and
        /// removes its config directory.
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn hand_event(mode: Option<ShotDetectionMode>, handed: Handedness) -> FlighthookEvent {
        FlighthookEvent::SetDetectionMode {
            mode,
            handed: Some(handed),
        }
    }

    fn club_event(club: Club) -> FlighthookEvent {
        FlighthookEvent::ClubInfo {
            club_info: flighthook::ClubInfo { club },
        }
    }

    /// A left-handed player's first club select, the arm, carries the left
    /// hand.
    #[test]
    fn arm_carries_the_game_states_left_hand() {
        let session = Session::start(Some(Handedness::Left), vec![]);
        let seven = to_allsquare_club(Club::Iron7);
        assert_eq!(session.finish(1), [(seven, allsquare::Handed::Left)]);
    }

    /// Without a hand in the game state the arm is right-handed.
    #[test]
    fn arm_is_right_handed_when_the_hand_is_unknown() {
        let session = Session::start(None, vec![]);
        let seven = to_allsquare_club(Club::Iron7);
        assert_eq!(session.finish(1), [(seven, allsquare::Handed::Right)]);
    }

    /// A hand that arrives before the device is armed is recorded, not sent
    /// as a club select of its own.
    #[test]
    fn hand_before_the_arm_rides_the_arm() {
        let session = Session::start(None, vec![hand_event(None, Handedness::Left)]);
        let seven = to_allsquare_club(Club::Iron7);
        assert_eq!(session.finish(1), [(seven, allsquare::Handed::Left)]);
    }

    /// A hand change while connected re-sends the active club once with the
    /// new hand; the same hand again writes nothing. The club change after it
    /// pins the order: its select is the next frame, and carries the hand. A
    /// mode in the same event does not matter.
    #[test]
    fn hand_change_resends_the_active_club_once() {
        use allsquare::Handed::{Left, Right};
        let session = Session::start(None, vec![]);
        let seven = to_allsquare_club(Club::Iron7);
        let eight = to_allsquare_club(Club::Iron8);
        session.wait_for(1);
        session.send(hand_event(
            Some(ShotDetectionMode::Putting),
            Handedness::Left,
        ));
        session.wait_for(2);
        session.send(hand_event(None, Handedness::Left));
        session.send(club_event(Club::Iron8));
        assert_eq!(
            session.finish(3),
            [(seven, Right), (seven, Left), (eight, Left)]
        );
    }

    /// A hand set in the UI (`hand_info` from `web`) reaches the Omni through
    /// the system actor's command: the active club is re-sent once with it,
    /// and the next club carries it.
    #[test]
    fn a_hand_set_in_the_ui_reaches_the_device() {
        use allsquare::Handed::{Left, Right};
        let session = Session::with_system_actor();
        let seven = to_allsquare_club(Club::Iron7);
        let eight = to_allsquare_club(Club::Iron8);
        session.wait_for(1);
        let web = BusSender::new(
            "web".into(),
            session.sender.raw_sender().clone(),
            Arc::default(),
        );
        let left = || {
            FlighthookMessage::new(FlighthookEvent::HandInfo {
                hand_info: flighthook::HandInfo {
                    handed: Handedness::Left,
                },
            })
        };
        web.send(left());
        session.wait_for(2);
        web.send(left());
        web.send(FlighthookMessage::new(club_event(Club::Iron8)));
        assert_eq!(
            session.finish(3),
            [(seven, Right), (seven, Left), (eight, Left)]
        );
    }

    /// A hand and a club that change in the same pass write one select: the
    /// new club with the new hand.
    #[test]
    fn hand_and_club_in_one_pass_write_one_select() {
        use allsquare::Handed::{Left, Right};
        let session = Session::start(None, vec![]);
        let seven = to_allsquare_club(Club::Iron7);
        let eight = to_allsquare_club(Club::Iron8);
        session.wait_for(1);
        session.send_together(vec![
            hand_event(Some(ShotDetectionMode::Full), Handedness::Left),
            club_event(Club::Iron8),
        ]);
        session.wait_for(2);
        // The next select is the next frame: nothing else was written.
        session.send(club_event(Club::Iron7));
        assert_eq!(
            session.finish(3),
            [(seven, Right), (eight, Left), (seven, Left)]
        );
    }
}
