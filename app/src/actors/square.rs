use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use allsquare::{Client, Event, SpinMode, ble};
use tracing::{debug, info, warn};

use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;

use flighthook::{
    ActorStatus, BallFlight, Club, ClubData, FaceImpact, FlighthookEvent, FlighthookMessage,
    Severity, ShotKey, Velocity,
};

/// Reconnect backoff bounds (linear: +1s per attempt, capped at 15s).
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
    /// Ball speed (mph) at or above which a zero-spin reading is treated as a
    /// failed read and the shot discarded. Zero disables the check.
    pub reject_zero_spin_above_mph: f64,
}

impl Actor for SquareActor {
    fn start(&self, _state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let address = self.address.clone();
        let club = self.club;
        let advanced_spin = self.advanced_spin;
        let zero_spin_cutoff = self.reject_zero_spin_above_mph;
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run(address, club, advanced_spin, zero_spin_cutoff, sender, receiver);
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
                if section.address == self.address {
                    ReconfigureOutcome::Applied
                } else {
                    ReconfigureOutcome::RestartRequired
                }
            }
            None => ReconfigureOutcome::RestartRequired, // section removed
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// Map a flighthook club to the allsquare equivalent.
///
/// The two enums line up one-for-one, so a club change coming from GSPro can be
/// pushed straight to the device. This matters more here than on other monitors:
/// Square Golf uses the selected club to classify the shot, and putting mode is
/// nothing more than "the putter is selected".
fn to_allsquare_club(club: Club) -> allsquare::Club {
    match club {
        Club::Driver => allsquare::Club::Driver,
        Club::Wood3 => allsquare::Club::Wood3,
        Club::Wood5 => allsquare::Club::Wood5,
        Club::Wood7 => allsquare::Club::Wood7,
        Club::Hybrid3 => allsquare::Club::Hybrid3,
        Club::Hybrid4 => allsquare::Club::Hybrid4,
        Club::Hybrid5 => allsquare::Club::Hybrid5,
        Club::Iron3 => allsquare::Club::Iron3,
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

// ---------------------------------------------------------------------------
// Protocol type -> bus type conversion helpers
// ---------------------------------------------------------------------------

/// Whether a shot is a spin misread and should be dropped rather than forwarded.
///
/// The device occasionally returns a shot with no spin at all — seen with a ball
/// struck at the very front of the detection zone. A real strike always imparts
/// spin, and a spinless shot handed to a sim carries much further than it should
/// (a zero-spin 8 iron ran ~30 yards long), so it is better to lose the shot than
/// to record a wrong one.
///
/// Slow shots are exempt: putts and soft chips can legitimately read zero, and
/// the consequence of a spinless putt is negligible. `cutoff_mph` of zero
/// disables the check entirely.
fn is_zero_spin_misread(b: &allsquare::BallMetrics, cutoff_mph: f64) -> bool {
    if cutoff_mph <= 0.0 {
        return false;
    }
    let no_spin = b.total_spin == 0 && b.back_spin == 0 && b.side_spin == 0;
    no_spin && Velocity::MetersPerSecond(b.speed).as_mph() >= cutoff_mph
}

fn ball_from_square(b: &allsquare::BallMetrics) -> BallFlight {
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(b.speed)),
        launch_elevation: Some(b.launch_angle),
        launch_azimuth: Some(b.direction),
        // The device measures launch only; the vendor app computes flight.
        carry_distance: None,
        total_distance: None,
        max_height: None,
        flight_time: None,
        roll_distance: None,
        backspin_rpm: Some(i32::from(b.back_spin)),
        // Negated: the device reports negative sidespin for rightward curve,
        // FRP defines positive as rightward. Without this, fades draw and
        // draws fade in GSPro (which derives spin axis from these components).
        // Same polarity the Mevo+ wire uses, and the same fix ironsight applies.
        sidespin_rpm: Some(-i32::from(b.side_spin)),
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
        // Impact location is reported via FaceImpact, which is its proper home.
        club_offset: None,
        club_height: None,
    }
}

/// Impact location, if the device measured it.
///
/// **The lateral sign is inverted.** FRP defines `lateral` as positive toward
/// the toe; the device reports negative toward the toe. `vertical` agrees on
/// both sides (positive is above centre). Units are millimetres — the best
/// current reading of the device's scale, not yet checked against a reference
/// launch monitor.
fn impact_from_square(c: &allsquare::ClubMetrics) -> Option<FaceImpact> {
    use flighthook::Distance;
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

fn run(
    address: Option<String>,
    club: Club,
    advanced_spin: bool,
    reject_zero_spin_above_mph: f64,
    sender: BusSender,
    mut receiver: BusReceiver,
) {
    let mut backoff = MIN_BACKOFF;
    let mut ever_connected = false;
    let mut device_id: Option<String> = None;
    // Latched across reconnects so the device comes back with the right club.
    let mut current_club = club;

    loop {
        if receiver.poll().is_err() {
            break;
        }

        emit_device_status(&sender, ActorStatus::Starting, HashMap::new());

        match connect_and_run(
            address.as_deref(),
            &mut current_club,
            advanced_spin,
            reject_zero_spin_above_mph,
            &sender,
            &mut receiver,
            &mut ever_connected,
            &mut device_id,
        ) {
            Ok(()) => break,
            Err(e) => {
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
                info!("{verb} in {}s", backoff.as_secs());

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

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn connect_and_run(
    address: Option<&str>,
    current_club: &mut Club,
    advanced_spin: bool,
    reject_zero_spin_above_mph: f64,
    sender: &BusSender,
    receiver: &mut BusReceiver,
    ever_connected: &mut bool,
    device_id: &mut Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("searching for Square Golf device...");
    let transport = ble::connect(address)?;
    *ever_connected = true;

    let name = transport.name().to_string();
    let ble_address = transport.address().to_string();
    // Log the address explicitly: after a first auto-discovered connect the
    // user needs it to pin this device in config, and auto-discovery would
    // otherwise never show it anywhere.
    info!("connected to {name} at {ble_address}");
    *device_id = Some(name.clone());

    let mut client = Client::new(transport);
    let spin = if advanced_spin {
        SpinMode::Advanced
    } else {
        SpinMode::Standard
    };

    let mut shot_counter: u32 = 0;
    let mut discarded_counter: u32 = 0;
    let mut idle_count: u32 = 0;
    let mut device_telemetry: HashMap<String, String> = HashMap::new();

    loop {
        // ==============================================================
        // Phase 1: Drain bus commands
        // ==============================================================
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
                            let mapped = to_allsquare_club(club_info.club);
                            info!("club -> {mapped}");
                            if let Err(e) = client.select_club(mapped) {
                                warn!("club select failed: {e}");
                            }
                        }
                    }
                    // There is no device-side shot mode: putting and chipping
                    // differ only by which club is selected.
                    FlighthookEvent::SetDetectionMode {
                        mode: Some(new_mode),
                        ..
                    } => {
                        debug!(
                            "mode change to {new_mode:?} ignored \
                             — Square Golf has no mode control, select a club"
                        );
                    }
                    _ => {}
                },
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
                        if is_zero_spin_misread(&ball, reject_zero_spin_above_mph) {
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
                            continue;
                        }

                        shot_counter += 1;
                        let key = ShotKey {
                            shot_id: uuid::Uuid::new_v4().to_string(),
                            shot_number: shot_counter,
                        };
                        info!("shot #{}", key.shot_number);

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
                            FlighthookMessage::new(FlighthookEvent::BallFlight {
                                key: key.clone(),
                                ball: Box::new(bf),
                            })
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

                            if let Some(impact) = impact_from_square(c) {
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

                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::ShotFinished { key })
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

    /// The device reports negative sidespin for a rightward curve; FRP defines
    /// positive as rightward. Getting this backwards makes fades draw and draws
    /// fade in GSPro, which derives spin axis from the spin components.
    #[test]
    fn sidespin_polarity_is_inverted() {
        let fade = allsquare::BallMetrics {
            shot_type: 0x37,
            speed: 3.30,
            launch_angle: 32.51,
            direction: 7.39,
            total_spin: 627,
            spin_axis: -8.00,
            back_spin: 621,
            side_spin: -87,
        };
        let bf = ball_from_square(&fade);
        assert_eq!(bf.sidespin_rpm, Some(87), "device -87 (right) -> FRP +87");
        assert_eq!(bf.backspin_rpm, Some(621), "backspin is not flipped");
        assert_eq!(bf.launch_azimuth, Some(7.39), "azimuth is not flipped");
    }

    fn ball(speed_ms: f64, total: i16, back: i16, side: i16) -> allsquare::BallMetrics {
        allsquare::BallMetrics {
            shot_type: 0x37,
            speed: speed_ms,
            launch_angle: 18.0,
            direction: 0.0,
            total_spin: total,
            spin_axis: 0.0,
            back_spin: back,
            side_spin: side,
        }
    }

    /// The case that prompted this: an 8 iron struck at the front of the
    /// detection zone read zero spin and flew ~30 yards long in the sim.
    #[test]
    fn rejects_fast_zero_spin_shot() {
        // 49 m/s ~= 110 mph, a normal 8 iron.
        assert!(is_zero_spin_misread(&ball(49.0, 0, 0, 0), 60.0));
    }

    /// Slow shots are exempt — a putt or soft chip can genuinely read zero, and
    /// a spinless putt does no harm.
    #[test]
    fn allows_slow_zero_spin_shot() {
        // 3.3 m/s ~= 7 mph, a putt.
        assert!(!is_zero_spin_misread(&ball(3.3, 0, 0, 0), 60.0));
        // 22 m/s ~= 49 mph, a chip — still under the cutoff.
        assert!(!is_zero_spin_misread(&ball(22.0, 0, 0, 0), 60.0));
    }

    /// Any measured spin means the read succeeded, however fast the ball.
    #[test]
    fn allows_fast_shot_with_spin() {
        assert!(!is_zero_spin_misread(&ball(70.0, 2400, 2400, 0), 60.0));
        // Sidespin alone still counts as a successful read.
        assert!(!is_zero_spin_misread(&ball(70.0, 0, 0, -87), 60.0));
    }

    /// A zero cutoff disables the check, so nothing is ever discarded.
    #[test]
    fn zero_cutoff_disables_rejection() {
        assert!(!is_zero_spin_misread(&ball(80.0, 0, 0, 0), 0.0));
    }
}
