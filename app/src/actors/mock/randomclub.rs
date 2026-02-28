//! Random club integration — logs shots, always connected.
//!
//! Subscribes to bus for shots, emits GameStateCommand for connection status.
//! Randomly cycles club and handedness after each shot to exercise the UI.
//! Useful for testing without a real simulator (GSPro, etc.).

use std::sync::Arc;
use std::time::Duration;

use std::collections::HashMap;

use super::super::Actor;
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::SystemState;
use flighthook::{
    ActorState, ActorStatus, Club, ClubInfo, FlighthookEvent, FlighthookMessage,
    GameStateCommandEvent, LaunchMonitorEvent, PlayerInfo,
};

const HANDEDNESS: &[&str] = &["RH", "LH"];

/// Random club cycling integration actor.
pub struct RandomClubActor;

impl Actor for RandomClubActor {
    fn start(&self, _state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let thread_name = format!("mock:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(sender, receiver))
            .expect("failed to spawn random_club thread");
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn next_rand(seed: &mut u64) -> usize {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*seed >> 33) as usize
}

fn emit_status(sender: &BusSender, club: Club, handed: &str) {
    let mut telemetry = HashMap::new();
    telemetry.insert("club".into(), club.to_string());
    telemetry.insert("handed".into(), handed.to_string());
    sender.send(FlighthookMessage::new(ActorState::new(
        ActorStatus::Connected,
        telemetry,
    )));
}

fn run(sender: BusSender, mut receiver: BusReceiver) {
    let name = sender.actor_id().to_string();
    tracing::info!("random_club '{name}': started");

    let mut seed = now_ms();

    // Pick initial club and handedness
    let mut handed = HANDEDNESS[next_rand(&mut seed) % HANDEDNESS.len()].to_string();
    let mut club = Club::ALL[next_rand(&mut seed) % Club::ALL.len()];

    // Report connected with telemetry + emit game state
    emit_status(&sender, club, &handed);
    sender.send(FlighthookMessage::new(
        GameStateCommandEvent::SetPlayerInfo {
            player_info: PlayerInfo {
                handed: handed.clone(),
            },
        },
    ));
    sender.send(FlighthookMessage::new(GameStateCommandEvent::SetClubInfo {
        club_info: ClubInfo { club },
    }));

    loop {
        match receiver.poll() {
            Err(PollError::Shutdown) => {
                tracing::info!("random_club '{name}': shutting down");
                return;
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(Some(msg)) => {
                if let FlighthookEvent::LaunchMonitor(recv) = &msg.event {
                    let LaunchMonitorEvent::ShotResult { ref shot } = recv.event else {
                        continue;
                    };
                    let b = &shot.ball;
                    let summary = format!(
                        "shot #{}: speed={:.1}m/s carry={:.1}m spin={}rpm",
                        shot.shot_number,
                        b.launch_speed.as_mps(),
                        b.carry_distance.map(|d| d.as_meters()).unwrap_or(0.0),
                        b.backspin_rpm.unwrap_or(0),
                    );
                    tracing::info!("random_club '{name}': {summary}");

                    // Cycle to a new random club and handedness after each shot
                    club = Club::ALL[next_rand(&mut seed) % Club::ALL.len()];
                    handed = HANDEDNESS[next_rand(&mut seed) % HANDEDNESS.len()].to_string();
                    emit_status(&sender, club, &handed);
                    sender.send(FlighthookMessage::new(
                        GameStateCommandEvent::SetPlayerInfo {
                            player_info: PlayerInfo {
                                handed: handed.clone(),
                            },
                        },
                    ));
                    sender.send(FlighthookMessage::new(GameStateCommandEvent::SetClubInfo {
                        club_info: ClubInfo { club },
                    }));
                }
            }
        }
    }
}
