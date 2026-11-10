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
use crate::actors::provenance::ShotSource;
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::sim;
use crate::state::SystemState;
use crate::state::round::RoundPolicy;
use flighthook::{
    ActorStatus, Basis, ClubData, Distance, FlightResult, FlighthookEvent, FlighthookMessage,
    Launch, ShotDetectionMode, ShotKey, ShotRaw, Velocity,
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

/// What became of a shot the mock was due to make.
#[derive(Debug, PartialEq, Eq)]
enum Fired<T> {
    /// It was made: what the send returned.
    Sent(T),
    /// A tournament round is active, so it was not made. `logged` is true
    /// the first time in the round, when the mock says so in the log.
    Held { logged: bool },
}

/// The mock's shots during tournament rounds: none, with one log line per
/// round saying so.
#[derive(Debug, Default)]
struct RoundPause {
    /// A shot has been held since the last one was made.
    holding: bool,
}

impl RoundPause {
    /// Make a shot with `send`, which puts its trigger on the bus, unless a
    /// round is active ([`RoundPolicy::send_if_allowed`]).
    fn fire<T>(&mut self, round: &RoundPolicy, send: impl FnOnce() -> T) -> Fired<T> {
        match round.send_if_allowed(ShotSource::MockMonitor.provenance(), send) {
            Some(sent) => {
                self.holding = false;
                Fired::Sent(sent)
            }
            None => {
                let logged = !std::mem::replace(&mut self.holding, true);
                if logged {
                    info!("mock: no shots during the tournament round");
                }
                Fired::Held { logged }
            }
        }
    }
}

/// Mock launch monitor actor. Generates random shots at a fixed interval.
pub struct MockLaunchActor {
    pub initial_mode: ShotDetectionMode,
}

impl Actor for MockLaunchActor {
    /// A virtual launch monitor: its random shots are no device's reading.
    fn source(&self) -> Option<ShotSource> {
        Some(ShotSource::MockMonitor)
    }

    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let initial_mode = self.initial_mode;
        let device_id = format!("mock-{}", sender.actor_id());
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(&state, initial_mode, device_id, sender, receiver))
            .expect("failed to spawn mock launch thread")
    }

    /// It holds no device or port.
    fn replaceable_while_stopping(&self) -> bool {
        true
    }
}

fn mode_str(mode: ShotDetectionMode) -> &'static str {
    match mode {
        ShotDetectionMode::Full => "full",
        ShotDetectionMode::Chipping => "chipping",
        ShotDetectionMode::Putting => "putting",
    }
}

fn telemetry(
    shot_count: u32,
    mode: ShotDetectionMode,
    radar_mode: &str,
) -> HashMap<String, String> {
    HashMap::from([
        (
            "device_info".into(),
            "Mock Launch Monitor (simulated)".into(),
        ),
        ("shot_count".into(), shot_count.to_string()),
        ("tracking_mode".into(), mode_str(mode).into()),
        ("radar_mode".into(), radar_mode.into()),
    ])
}

fn run(
    state: &SystemState,
    initial_mode: ShotDetectionMode,
    device_id: String,
    sender: BusSender,
    mut receiver: BusReceiver,
) {
    let mut current_mode = initial_mode;
    let mut shot_count: u32 = 0;
    let mut pause = RoundPause::default();
    let mut phase = Phase::Idle {
        since: Instant::now(),
    };

    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status: ActorStatus::Connected,
        telemetry: telemetry(shot_count, current_mode, "idle"),
    }));
    sender.send(
        FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
            manufacturer: None,
            model: None,
            firmware: None,
            source: None,
            telemetry: Some(HashMap::from([("ready".into(), "false".into())])),
        })
        .device(device_id.as_str()),
    );
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
                    if let FlighthookEvent::SetDetectionMode {
                        mode: Some(mode), ..
                    } = msg.event
                        && std::mem::discriminant(&current_mode) != std::mem::discriminant(&mode)
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
                telemetry: telemetry(shot_count, current_mode, "armed"),
            }));
        }

        // State machine
        match phase {
            Phase::Idle { since } if since.elapsed() >= ARM_DELAY => {
                info!("mock: armed -- waiting for ball");
                sender.send(
                    FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                        manufacturer: None,
                        model: None,
                        firmware: None,
                        source: None,
                        telemetry: Some(HashMap::from([("ready".into(), "false".into())])),
                    })
                    .device(device_id.as_str()),
                );
                sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                    status: ActorStatus::Connected,
                    telemetry: telemetry(shot_count, current_mode, "armed"),
                }));
                phase = Phase::Armed {
                    since: Instant::now(),
                };
            }
            Phase::Armed { since } if since.elapsed() >= BALL_READY_DELAY => {
                info!("mock: ball ready -- ready to fire");
                sender.send(
                    FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                        manufacturer: None,
                        model: None,
                        firmware: None,
                        source: None,
                        telemetry: Some(HashMap::from([("ready".into(), "true".into())])),
                    })
                    .device(device_id.as_str()),
                );
                phase = Phase::Ready {
                    since: Instant::now(),
                };
            }
            // A virtual producer: no shot while a tournament round is active.
            Phase::Ready { since } if since.elapsed() >= SHOT_INTERVAL => {
                let fired = pause.fire(&state.round, || {
                    let n = shot_count + 1;
                    let key = ShotKey {
                        shot_id: uuid::Uuid::new_v4().to_string(),
                        shot_number: n,
                    };
                    let (ball, flight, club) = generate_shot(n, current_mode);

                    let ball_mph = ball.launch_speed.map(|v| v.as_mph()).unwrap_or(0.0);
                    let carry_yd = flight.carry_distance.map(|d| d.as_yards()).unwrap_or(0.0);
                    info!(
                        "mock shot #{}: ball={:.1}mph VLA={:.1} carry={:.1}yd",
                        n,
                        ball_mph,
                        ball.launch_elevation.unwrap_or(0.0),
                        carry_yd,
                    );

                    // Disarm
                    sender.send(
                        FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                            manufacturer: None,
                            model: None,
                            firmware: None,
                            source: None,
                            telemetry: Some(HashMap::from([("ready".into(), "false".into())])),
                        })
                        .device(device_id.as_str()),
                    );
                    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                        status: ActorStatus::Connected,
                        telemetry: telemetry(n, current_mode, "idle"),
                    }));

                    // Shot lifecycle
                    sender.send(
                        FlighthookMessage::new(FlighthookEvent::shot_trigger(key.clone()))
                            .device(device_id.as_str()),
                    );
                    (n, key, ball, flight, club)
                });
                if let Fired::Sent((n, key, ball, flight, club)) = fired {
                    shot_count = n;
                    for event in sim::launch_events(state, key.clone(), ball, Some(flight)) {
                        sender.send(FlighthookMessage::new(event).device(device_id.as_str()));
                    }
                    sender.send(
                        FlighthookMessage::new(FlighthookEvent::ClubPath {
                            key: key.clone(),
                            club: Box::new(club),
                        })
                        .device(device_id.as_str()),
                    );
                    sender.send(
                        FlighthookMessage::new(FlighthookEvent::ShotFinished {
                            key,
                            raw: Some(Box::new(ShotRaw {
                                transport: "none".into(),
                                unavailable: Some("mock device has no wire protocol".into()),
                                ..ShotRaw::default()
                            })),
                        })
                        .device(device_id.as_str()),
                    );

                    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                        status: ActorStatus::Connected,
                        telemetry: telemetry(shot_count, current_mode, "idle"),
                    }));
                    phase = Phase::PostShot {
                        since: Instant::now(),
                    };
                }
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

/// A made-up shot: its launch, its flight values (from no real model, so
/// [`Basis::Modeled`]) and its club data.
fn generate_shot(n: u32, mode: ShotDetectionMode) -> (Launch, FlightResult, ClubData) {
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

    let ball = Launch {
        launch_speed: Some(Velocity::MetersPerSecond(ball_speed)),
        launch_elevation: Some(vla),
        launch_azimuth: Some(hla),
        backspin_rpm: Some(backspin as i32),
        sidespin_rpm: Some(sidespin as i32),
        ..Launch::default()
    };
    let flight = FlightResult {
        carry_distance: Some(Distance::Meters(carry)),
        total_distance: Some(Distance::Meters(carry * 1.1)),
        max_height: Some(Distance::Meters(height)),
        flight_time: Some(5.0),
        roll_distance: Some(Distance::Meters(carry * 0.1)),
        ..FlightResult::new(Basis::Modeled)
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
        ..ClubData::default()
    };

    (ball, flight, club)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::round::RoundRequest;

    /// During a round the mock makes no shot and says so once in the log;
    /// once the round ends its next shot goes out, and the next round is
    /// logged again.
    #[test]
    fn the_mock_holds_its_shots_during_a_round_and_logs_it_once() {
        let round = RoundPolicy::default();
        let mut pause = RoundPause::default();
        let begin = || {
            round
                .begin_round(&RoundRequest {
                    holder: "Pureplay",
                    feeds: &[],
                })
                .expect("accepted")
        };
        assert_eq!(pause.fire(&round, || 1), Fired::Sent(1));

        let guard = begin();
        let mut sent = false;
        assert_eq!(
            pause.fire(&round, || sent = true),
            Fired::Held { logged: true }
        );
        assert_eq!(
            pause.fire(&round, || sent = true),
            Fired::Held { logged: false },
            "one line per round"
        );
        assert!(!sent, "no trigger during the round");
        drop(guard);
        assert_eq!(pause.fire(&round, || 2), Fired::Sent(2));

        let _guard = begin();
        assert_eq!(pause.fire(&round, || 3), Fired::Held { logged: true });
    }

    /// Every output's numbers for a mock shot in each detection mode, from
    /// the mock's own shot generator.
    #[test]
    fn outputs_keep_their_numbers() {
        for (case, mode) in [
            ("mock_full", ShotDetectionMode::Full),
            ("mock_chipping", ShotDetectionMode::Chipping),
            ("mock_putting", ShotDetectionMode::Putting),
        ] {
            let (ball, flight, club) = generate_shot(3, mode);
            let messages = crate::test_ball::device_shot(
                "mock_monitor.0",
                ball,
                Some(flight),
                Some(club),
                None,
                Some(mode),
            );
            crate::test_ball::assert_outputs_pinned(case, &messages);
        }
    }
}
