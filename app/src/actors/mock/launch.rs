//! Mock launch monitor actor — produces a random shot every 30 seconds.
//!
//! No ironsight dependency. Generates shot lifecycle events directly from
//! schema types, using the current mode (derived from club selection in
//! global state) to pick realistic values. Useful for testing the full
//! pipeline without hardware.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::info;

use crate::actors::Actor;
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;
use flighthook::{
    ActorStatus, BallFlight, ClubData, Distance, FlighthookEvent, FlighthookMessage,
    ShotDetectionMode, ShotKey, Velocity,
};

const SHOT_INTERVAL: Duration = Duration::from_secs(30);
const ARM_DELAY: Duration = Duration::from_secs(2);
const BALL_READY_DELAY: Duration = Duration::from_secs(1);
const POST_SHOT_DELAY: Duration = Duration::from_secs(1);

/// Mock device lifecycle phase.
enum Phase {
    /// Just connected or post-shot, waiting to arm.
    Idle { since: Instant },
    /// Armed, waiting for ball detection.
    Armed { since: Instant },
    /// Armed + ball detected, waiting for shot timer.
    Ready { since: Instant },
    /// Shot in progress (brief pause before re-arming).
    PostShot { since: Instant },
}

/// Mock launch monitor actor. Generates random shots at a fixed interval.
pub struct MockLaunchActor {
    pub initial_mode: ShotDetectionMode,
}

impl Actor for MockLaunchActor {
    fn start(&self, _state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let initial_mode = self.initial_mode;
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(initial_mode, sender, receiver))
            .expect("failed to spawn mock launch thread");
    }
}

fn mode_str(mode: ShotDetectionMode) -> &'static str {
    match mode {
        ShotDetectionMode::Full => "full",
        ShotDetectionMode::Chipping => "chipping",
        ShotDetectionMode::Putting => "putting",
    }
}

fn telemetry(shot_count: u32, mode: ShotDetectionMode) -> HashMap<String, String> {
    HashMap::from([
        (
            "device_info".into(),
            "Mock Launch Monitor (simulated)".into(),
        ),
        ("shot_count".into(), shot_count.to_string()),
        ("tracking_mode".into(), mode_str(mode).into()),
    ])
}

fn run(initial_mode: ShotDetectionMode, sender: BusSender, mut receiver: BusReceiver) {
    let mut current_mode = initial_mode;
    let mut shot_count: u32 = 0;
    let mut phase = Phase::Idle {
        since: Instant::now(),
    };

    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status: ActorStatus::Connected,
        telemetry: telemetry(shot_count, current_mode),
    }));
    sender.send(FlighthookMessage::new(FlighthookEvent::DeviceInfo {
        manufacturer: None,
        model: None,
        firmware: None,
        telemetry: Some(HashMap::from([
            ("ready".into(), "false".into()),
            ("ball_detected".into(), "false".into()),
        ])),
    }));
    info!("mock: connected -- arming in {ARM_DELAY:?}");

    loop {
        // Drain bus commands
        let mut mode_changed = false;
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => {
                    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                        status: ActorStatus::Disconnected,
                        telemetry: HashMap::new(),
                    }));
                    return;
                }
                Ok(None) => break,
                Ok(Some(msg)) => {
                    if let FlighthookEvent::SetDetectionMode { mode } = msg.event
                        && std::mem::discriminant(&current_mode)
                            != std::mem::discriminant(&mode)
                    {
                        info!("mock: mode change: {current_mode:?} -> {mode:?}");
                        current_mode = mode;
                        mode_changed = true;
                    }
                }
            }
        }

        if mode_changed {
            sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                status: ActorStatus::Connected,
                telemetry: telemetry(shot_count, current_mode),
            }));
        }

        // State machine
        match phase {
            Phase::Idle { since } if since.elapsed() >= ARM_DELAY => {
                info!("mock: armed -- waiting for ball");
                sender.send(FlighthookMessage::new(FlighthookEvent::DeviceInfo {
                    manufacturer: None,
                    model: None,
                    firmware: None,
                    telemetry: Some(HashMap::from([
                        ("ready".into(), "true".into()),
                        ("ball_detected".into(), "false".into()),
                    ])),
                }));
                let mut t = telemetry(shot_count, current_mode);
                t.insert("ready".into(), "true".into());
                sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                    status: ActorStatus::Connected,
                    telemetry: t,
                }));
                phase = Phase::Armed {
                    since: Instant::now(),
                };
            }
            Phase::Armed { since } if since.elapsed() >= BALL_READY_DELAY => {
                info!("mock: ball detected -- ready to fire");
                sender.send(FlighthookMessage::new(FlighthookEvent::DeviceInfo {
                    manufacturer: None,
                    model: None,
                    firmware: None,
                    telemetry: Some(HashMap::from([
                        ("ready".into(), "true".into()),
                        ("ball_detected".into(), "true".into()),
                    ])),
                }));
                phase = Phase::Ready {
                    since: Instant::now(),
                };
            }
            Phase::Ready { since } if since.elapsed() >= SHOT_INTERVAL => {
                shot_count += 1;

                let key = ShotKey {
                    shot_id: uuid::Uuid::new_v4().to_string(),
                    shot_number: shot_count,
                };
                let (ball, club) = generate_shot(shot_count, current_mode);

                let ball_mph = ball.launch_speed.map(|v| v.as_mph()).unwrap_or(0.0);
                let carry_yd = ball
                    .carry_distance
                    .map(|d| d.as_yards())
                    .unwrap_or(0.0);
                info!(
                    "mock shot #{}: ball={:.1}mph VLA={:.1} carry={:.1}yd",
                    shot_count, ball_mph, ball.launch_elevation.unwrap_or(0.0), carry_yd,
                );

                // Disarm
                sender.send(FlighthookMessage::new(FlighthookEvent::DeviceInfo {
                    manufacturer: None,
                    model: None,
                    firmware: None,
                    telemetry: Some(HashMap::from([
                        ("ready".into(), "false".into()),
                        ("ball_detected".into(), "false".into()),
                    ])),
                }));
                let mut shooting = telemetry(shot_count, current_mode);
                shooting.insert("shooting".into(), "true".into());
                sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                    status: ActorStatus::Connected,
                    telemetry: shooting,
                }));

                // Shot lifecycle
                sender.send(FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                    key: key.clone(),
                }));
                sender.send(FlighthookMessage::new(FlighthookEvent::BallFlight {
                    key: key.clone(),
                    ball: Box::new(ball),
                }));
                sender.send(FlighthookMessage::new(FlighthookEvent::ClubPath {
                    key: key.clone(),
                    club: Box::new(club),
                }));
                sender.send(FlighthookMessage::new(FlighthookEvent::ShotFinished {
                    key,
                }));

                sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                    status: ActorStatus::Connected,
                    telemetry: telemetry(shot_count, current_mode),
                }));
                phase = Phase::PostShot {
                    since: Instant::now(),
                };
            }
            Phase::PostShot { since } if since.elapsed() >= POST_SHOT_DELAY => {
                info!("mock: re-arming");
                phase = Phase::Idle {
                    since: Instant::now(),
                };
            }
            _ => {}
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn generate_shot(
    n: u32,
    mode: ShotDetectionMode,
) -> (BallFlight, ClubData) {
    let v = (n as f64 * 0.7).sin(); // -1..1 variation seed

    let (ball_speed, vla, hla, carry, height, backspin, sidespin, club_speed, aoa, loft) =
        match mode {
            ShotDetectionMode::Full => (
                53.0 + 3.0 * v,
                17.0 + 1.5 * v,
                0.8 * v,
                155.0 + 10.0 * v,
                26.0 + 2.0 * v,
                5500.0 + 300.0 * v,
                80.0 * v,
                36.0 + 2.0 * v,
                -4.0 + 0.8 * v,
                22.0 + 1.0 * v,
            ),
            ShotDetectionMode::Chipping => (
                18.0 + 2.0 * v,
                35.0 + 3.0 * v,
                0.5 * v,
                27.0 + 4.0 * v,
                8.0 + 1.5 * v,
                5000.0 + 400.0 * v,
                50.0 * v,
                25.0 + 2.0 * v,
                -5.0 + 1.0 * v,
                38.0 + 2.0 * v,
            ),
            ShotDetectionMode::Putting => (
                3.5 + 0.5 * v,
                2.0 + 0.3 * v,
                0.3 * v,
                4.5 + 1.0 * v,
                0.1 + 0.02 * v,
                200.0 + 40.0 * v,
                20.0 * v,
                3.0 + 0.4 * v,
                -2.0 + 0.5 * v,
                4.0 + 0.5 * v,
            ),
        };

    let face_angle = hla * 0.7;
    let path = hla * 0.4;

    let ball = BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(ball_speed)),
        launch_elevation: Some(vla),
        launch_azimuth: Some(hla),
        carry_distance: Some(Distance::Meters(carry)),
        total_distance: Some(Distance::Meters(carry * 1.1)),
        max_height: Some(Distance::Meters(height)),
        flight_time: Some(5.0),
        roll_distance: Some(Distance::Meters(carry * 0.1)),
        backspin_rpm: Some(backspin as i32),
        sidespin_rpm: Some(sidespin as i32),
    };
    let club = ClubData {
        club_speed: Some(Velocity::MetersPerSecond(club_speed)),
        path: Some(path),
        attack_angle: Some(aoa),
        face_angle: Some(face_angle),
        dynamic_loft: Some(loft),
        smash_factor: Some(ball_speed / club_speed),
        club_speed_post: Some(Velocity::MetersPerSecond(club_speed * 0.7)),
        swing_plane_horizontal: Some(0.0),
        swing_plane_vertical: Some(45.0),
        club_offset: None,
        club_height: None,
    };

    (ball, club)
}
