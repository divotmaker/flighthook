//! Mock launch monitor actor — produces a random shot every 30 seconds.
//!
//! No ironsight dependency. Generates `ShotData` directly from schema types,
//! using the current mode (derived from club selection in global state) to
//! pick realistic values. Useful for testing the full pipeline without hardware.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::info;

use crate::actors::Actor;
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;
use flighthook::{
    ActorState, ActorStatus, BallFlight, ClubData, Distance, FlighthookEvent, FlighthookMessage,
    GameStateCommandEvent, LaunchMonitorEvent, ShotData, ShotDetectionMode, SpinData, Velocity,
};

const SHOT_INTERVAL: Duration = Duration::from_secs(30);

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

fn telemetry(shot_count: i32, mode: ShotDetectionMode) -> HashMap<String, String> {
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
    let device_id = sender.actor_id().to_string();
    let mut current_mode = initial_mode;
    let mut shot_count: i32 = 0;
    // Backdate so the first shot fires after ~1s instead of waiting the full interval.
    let mut last_shot = Instant::now() - SHOT_INTERVAL + Duration::from_secs(1);

    // Go straight to ready
    sender.send(FlighthookMessage::new(ActorState::new(
        ActorStatus::Connected,
        telemetry(shot_count, current_mode),
    )));
    sender.send(FlighthookMessage::new(LaunchMonitorEvent::ReadyState {
        armed: true,
        ball_detected: true,
    }));
    info!("mock: ready -- generating shots every {SHOT_INTERVAL:?}");

    loop {
        // Drain bus commands
        let mut mode_changed = false;
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => {
                    sender.send(FlighthookMessage::new(ActorState::new(
                        ActorStatus::Disconnected,
                        HashMap::new(),
                    )));
                    return;
                }
                Ok(None) => break,
                Ok(Some(msg)) => match msg.event {
                    FlighthookEvent::GameStateCommand(cmd) => {
                        if let GameStateCommandEvent::SetMode { mode } = cmd.event {
                            if std::mem::discriminant(&current_mode)
                                != std::mem::discriminant(&mode)
                            {
                                info!("mock: mode change: {current_mode:?} -> {mode:?}");
                                current_mode = mode;
                                mode_changed = true;
                            }
                        }
                    }
                    _ => {}
                },
            }
        }

        // Emit updated telemetry on mode change
        if mode_changed {
            sender.send(FlighthookMessage::new(ActorState::new(
                ActorStatus::Connected,
                telemetry(shot_count, current_mode),
            )));
        }

        // Generate shot if due
        if last_shot.elapsed() >= SHOT_INTERVAL {
            shot_count += 1;
            last_shot = Instant::now();

            let shot = generate_shot(shot_count, current_mode, &device_id);
            let ball_mph = shot.ball.launch_speed.as_mph();
            let carry_yd = shot
                .ball
                .carry_distance
                .map(|d| d.as_yards())
                .unwrap_or(0.0);
            info!(
                "mock shot #{}: ball={:.1}mph VLA={:.1} carry={:.1}yd",
                shot.shot_number, ball_mph, shot.ball.launch_elevation, carry_yd,
            );

            let mut shooting = telemetry(shot_count, current_mode);
            shooting.insert("shooting".into(), "true".into());
            sender.send(FlighthookMessage::new(ActorState::new(
                ActorStatus::Connected,
                shooting,
            )));
            sender.send(FlighthookMessage::new(LaunchMonitorEvent::ReadyState {
                armed: false,
                ball_detected: false,
            }));
            sender.send(FlighthookMessage::new(LaunchMonitorEvent::ShotResult {
                shot: Box::new(shot),
            }));
            sender.send(FlighthookMessage::new(ActorState::new(
                ActorStatus::Connected,
                telemetry(shot_count, current_mode),
            )));
            sender.send(FlighthookMessage::new(LaunchMonitorEvent::ReadyState {
                armed: true,
                ball_detected: true,
            }));
        }

        std::thread::sleep(Duration::from_millis(250));
    }
}

fn generate_shot(n: i32, mode: ShotDetectionMode, source: &str) -> ShotData {
    let v = (n as f64 * 0.7).sin(); // -1..1 variation seed

    let (ball_speed, vla, hla, carry, height, backspin, sidespin, club_speed, aoa, loft) =
        match mode {
            ShotDetectionMode::Full => (
                53.0 + 3.0 * v, // ~120 mph (7-iron)
                17.0 + 1.5 * v,
                0.8 * v,
                155.0 + 10.0 * v,
                26.0 + 2.0 * v,
                5500.0 + 300.0 * v,
                80.0 * v,
                36.0 + 2.0 * v, // ~80 mph
                -4.0 + 0.8 * v,
                22.0 + 1.0 * v,
            ),
            ShotDetectionMode::Chipping => (
                18.0 + 2.0 * v, // ~40 mph
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
                3.5 + 0.5 * v, // ~8 mph
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
    let total_spin = (backspin * backspin + sidespin * sidespin).sqrt();
    let spin_axis = sidespin.atan2(backspin).to_degrees();

    ShotData {
        source: source.to_string(),
        shot_number: n,
        ball: BallFlight {
            launch_speed: Velocity::MetersPerSecond(ball_speed),
            launch_elevation: vla,
            launch_azimuth: hla,
            carry_distance: Some(Distance::Meters(carry)),
            total_distance: Some(Distance::Meters(carry * 1.1)),
            max_height: Some(Distance::Meters(height)),
            flight_time: Some(5.0),
            roll_distance: Some(Distance::Meters(carry * 0.1)),
            backspin_rpm: Some(backspin as i32),
            sidespin_rpm: Some(sidespin as i32),
        },
        club: Some(ClubData {
            club_speed: Velocity::MetersPerSecond(club_speed),
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
        }),
        spin: Some(SpinData {
            total_spin: total_spin as i16,
            spin_axis,
        }),
        estimated: false,
    }
}
