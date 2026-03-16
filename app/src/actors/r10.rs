use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tenover::ble::BleTransport;
use tenover::{Client, Event};
use tracing::{debug, info, warn};

use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;

use flighthook::{
    ActorStatus, BallFlight, ClubData, FlighthookEvent, FlighthookMessage, Severity,
    ShotDetectionMode, ShotKey, Velocity,
};

/// Reconnect backoff bounds (linear: +1s per attempt, capped at 15s).
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(15);

/// Garmin R10 BLE actor. Connects via BLE auto-discovery, runs the
/// MultiLink/GFDI/protobuf handshake, and processes shots.
pub struct R10Actor {
    pub initial_mode: ShotDetectionMode,
}

impl Actor for R10Actor {
    fn start(&self, _state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let initial_mode = self.initial_mode;
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run(initial_mode, sender, receiver);
            })
            .expect("failed to spawn r10 thread");
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let actor_id = sender.actor_id();
        let Some((_, index)) = actor_id.split_once('.') else {
            return ReconfigureOutcome::Applied;
        };

        let snap = state.system.snapshot();
        match snap.r10.get(index) {
            Some(_) => ReconfigureOutcome::Applied, // no configurable params
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

// ---------------------------------------------------------------------------
// Protocol type -> bus type conversion helpers
// ---------------------------------------------------------------------------

fn ball_from_r10(b: &tenover::proto::BallData) -> BallFlight {
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(f64::from(b.ball_speed))),
        launch_elevation: Some(f64::from(b.launch_angle)),
        launch_azimuth: Some(f64::from(b.launch_direction)),
        carry_distance: None, // R10 doesn't compute carry
        total_distance: None,
        max_height: None,
        flight_time: None,
        roll_distance: None,
        #[allow(clippy::cast_possible_truncation)]
        backspin_rpm: Some(b.backspin.round() as i32),
        #[allow(clippy::cast_possible_truncation)]
        sidespin_rpm: Some(b.sidespin.round() as i32),
    }
}

fn club_from_r10(c: &tenover::proto::ClubData) -> ClubData {
    ClubData {
        club_speed: Some(Velocity::MetersPerSecond(f64::from(c.club_head_speed))),
        club_speed_post: None,
        path: Some(f64::from(c.path_angle)),
        attack_angle: Some(f64::from(c.attack_angle)),
        face_angle: Some(f64::from(c.face_angle)),
        dynamic_loft: None,
        smash_factor: None,
        swing_plane_horizontal: None,
        swing_plane_vertical: None,
        club_offset: None,
        club_height: None,
    }
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

#[allow(clippy::needless_pass_by_value)]
fn run(
    _initial_mode: ShotDetectionMode,
    sender: BusSender,
    mut receiver: BusReceiver,
) {
    let mut backoff = MIN_BACKOFF;
    let mut ever_connected = false;
    let mut device_id: Option<String> = None;

    loop {
        if receiver.poll().is_err() {
            break;
        }

        emit_device_status(&sender, ActorStatus::Starting, HashMap::new());

        match connect_and_run(
            &sender,
            &mut receiver,
            &mut ever_connected,
            &mut device_id,
        ) {
            Ok(()) => break,
            Err(e) => {
                warn!("session error: {e}");
                emit_alert(
                    &sender,
                    Severity::Warn,
                    format!("R10 connection error: {e}"),
                );
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
                            telemetry: Some(HashMap::from([
                                ("ready".into(), "false".into()),
                            ])),
                        })
                        .device(dev),
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

#[allow(clippy::too_many_lines)]
fn connect_and_run(
    sender: &BusSender,
    receiver: &mut BusReceiver,
    ever_connected: &mut bool,
    device_id: &mut Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. BLE connect
    info!("searching for Garmin R10...");
    let transport = BleTransport::auto_connect()?;
    *ever_connected = true;

    let addr = transport.device_address().to_string();
    let mtu = transport.mtu();
    info!("connected to R10 at {addr} (mtu={mtu})");

    // Capture BLE address as FRP device identity
    *device_id = Some(addr.clone());

    // 2. Create client and start registration
    let mut client = Client::new(transport, mtu.into());
    client.start()?;

    // 3. Event loop state
    let mut shot_counter: u32 = 0;
    let mut idle_count: u32 = 0;
    let mut device_telemetry: HashMap<String, String> = HashMap::new();

    loop {
        // ==============================================================
        // Phase 1: Drain bus commands
        // ==============================================================
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => {
                    drop(client);
                    return Ok(());
                }
                Ok(None) => break,
                Ok(Some(msg)) => {
                    // R10 doesn't support mode switching — just log
                    if let FlighthookEvent::SetDetectionMode { mode: Some(new_mode), .. } = msg.event {
                        debug!("mode change to {new_mode:?} (ignored — R10 has no mode control)");
                    }
                }
            }
        }

        // ==============================================================
        // Phase 2: Drive R10 client
        // ==============================================================
        match client.poll() {
            Ok(Some(event)) => {
                idle_count = 0;

                match event {
                    Event::Registered { handle } => {
                        info!("MultiLink registered (handle={handle})");
                    }

                    Event::HandshakeComplete => {
                        info!("handshake complete");
                        emit_device_status(
                            sender,
                            ActorStatus::Connected,
                            HashMap::from([
                                ("device_info".into(), format!("Garmin R10 ({addr})")),
                                ("shot_count".into(), shot_counter.to_string()),
                            ]),
                        );

                        // Emit identity DeviceTelemetry
                        device_telemetry.insert("ready".into(), "false".into());
                        device_telemetry.insert("tilt".into(), "ok".into());
                        device_telemetry.insert("roll".into(), "ok".into());
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: Some("Garmin".into()),
                                model: Some("R10".into()),
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&addr),
                        );
                    }

                    Event::Ready => {
                        info!("armed — waiting for shots");
                        device_telemetry.insert("ready".into(), "true".into());
                        device_telemetry.insert("device_state".into(), "waiting".into());
                        device_telemetry.insert("tilt".into(), "ok".into());
                        device_telemetry.insert("roll".into(), "ok".into());
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: None,
                                model: None,
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&addr),
                        );
                    }

                    Event::Shot(shot) => {
                        shot_counter += 1;
                        let key = ShotKey {
                            shot_id: uuid::Uuid::new_v4().to_string(),
                            shot_number: shot_counter,
                        };
                        info!("shot #{} (device_id={})", key.shot_number, shot.shot_id);

                        emit_device_status(
                            sender,
                            ActorStatus::Connected,
                            HashMap::from([
                                ("shot_count".into(), shot_counter.to_string()),
                            ]),
                        );

                        // Emit ShotTrigger
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                                key: key.clone(),
                            })
                            .device(&addr),
                        );

                        // Emit BallFlight if ball data present
                        if let Some(ref ball) = shot.ball {
                            let bf = ball_from_r10(ball);
                            info!(
                                "  ball: {:.1}mph VLA={:.1} HLA={:.1} back={:.0}rpm side={:.0}rpm",
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
                                .device(&addr),
                            );
                        }

                        // Emit ClubPath if club data present
                        if let Some(ref club) = shot.club {
                            let cd = club_from_r10(club);
                            info!(
                                "  club: {:.1}mph face={:.1} path={:.1} AoA={:.1}",
                                cd.club_speed.map_or(0.0, Velocity::as_mph),
                                cd.face_angle.unwrap_or(0.0),
                                cd.path.unwrap_or(0.0),
                                cd.attack_angle.unwrap_or(0.0),
                            );
                            sender.send(
                                FlighthookMessage::new(FlighthookEvent::ClubPath {
                                    key: key.clone(),
                                    club: Box::new(cd),
                                })
                                .device(&addr),
                            );
                        }

                        // Emit ShotFinished
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::ShotFinished {
                                key,
                            })
                            .device(&addr),
                        );

                        // R10 auto-returns to WAITING after processing
                        device_telemetry.insert("ready".into(), "false".into());
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: None,
                                model: None,
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&addr),
                        );
                    }

                    Event::StateChange(state) => {
                        // Only surface user-relevant states as telemetry.
                        // Standby and InterferenceTest are transient internal
                        // states that flicker during shot retransmits.
                        let label = match state {
                            tenover::proto::DeviceState::Waiting => Some("waiting"),
                            tenover::proto::DeviceState::Recording => Some("recording"),
                            tenover::proto::DeviceState::Processing => Some("processing"),
                            _ => None,
                        };
                        debug!("device state: {state:?}");
                        if let Some(label) = label {
                            device_telemetry.insert("device_state".into(), label.into());
                            sender.send(
                                FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                    manufacturer: None,
                                    model: None,
                                    firmware: None,
                                    telemetry: Some(device_telemetry.clone()),
                                })
                                .device(&addr),
                            );
                        }
                    }

                    Event::DeviceError(err) => {
                        let severity = match err.severity {
                            tenover::proto::ErrorSeverity::Warning => Severity::Warn,
                            tenover::proto::ErrorSeverity::Serious
                            | tenover::proto::ErrorSeverity::Fatal => Severity::Error,
                        };
                        let mut message = format!("R10: {:?}", err.code);
                        if let Some((roll, pitch)) = err.tilt {
                            use std::fmt::Write;
                            let _ = write!(message, " (roll={roll:.1}, pitch={pitch:.1})");
                            device_telemetry.insert("tilt".into(), format!("{pitch:.1}"));
                            device_telemetry.insert("roll".into(), format!("{roll:.1}"));
                        }
                        warn!("{message}");
                        emit_alert(sender, severity, &message);
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: None,
                                model: None,
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&addr),
                        );
                    }

                    Event::Subscribed { .. } | Event::WakeUpResponse { .. } => {}
                }
            }

            Ok(None) => {
                idle_count += 1;
                if idle_count >= 3 {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }

            Err(tenover::Error::Transport(e)) => {
                info!("BLE transport error: {e}");
                return Err(Box::new(tenover::Error::Transport(e)));
            }
            Err(e) => {
                warn!("protocol error: {e}");
                emit_alert(
                    sender,
                    Severity::Warn,
                    format!("R10 protocol error: {e}"),
                );
            }
        }
    }
}
