//! Random club integration — always connected.
//!
//! Picks a random club and hand at start and after each finished shot, and
//! sends them as a `golfer.select` under its own actor id. It holds nothing,
//! so it never re-asserts. Useful for testing without a real simulator
//! (GSPro, etc.).

use std::sync::Arc;
use std::time::Duration;

use std::collections::HashMap;

use super::super::Actor;
use crate::bus::{BusReceiver, BusSender, LocalEvent, PollError};
use crate::state::SystemState;
use flighthook::{ActorStatus, Club, Handedness};
use flightrelay::{FrpBody, GolferSelect};

const HANDEDNESS: &[Handedness] = &[Handedness::Right, Handedness::Left];

/// Random club cycling integration actor.
pub struct RandomClubActor;

impl Actor for RandomClubActor {
    fn start(
        &self,
        _state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let thread_name = format!("mock:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(sender, receiver))
            .expect("failed to spawn random_club thread")
    }

    /// It holds no device or port.
    fn replaceable_while_stopping(&self) -> bool {
        true
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

fn emit_status(sender: &BusSender, club: Club, handed: Handedness) {
    let mut telemetry = HashMap::new();
    telemetry.insert("club".into(), club.to_string());
    telemetry.insert("handed".into(), handed.to_string());
    sender.local(LocalEvent::CardStatus {
        status: ActorStatus::Connected,
        telemetry,
    });
}

/// Select `club` and `handed` as this card's actor.
fn select(sender: &BusSender, n: &mut u64, club: Club, handed: Handedness) {
    *n += 1;
    sender.frp(GolferSelect {
        club: Some(Some(club)),
        handedness: Some(handed),
        ..GolferSelect::new(format!("random-club-{n}"))
    });
}

fn run(sender: BusSender, mut receiver: BusReceiver) {
    let name = sender.actor_id().to_string();
    tracing::info!("random_club '{name}': started");

    let mut seed = now_ms();

    // Pick initial club and handedness
    let mut handed = HANDEDNESS[next_rand(&mut seed) % HANDEDNESS.len()];
    let mut club = Club::ALL[next_rand(&mut seed) % Club::ALL.len()];

    // Report connected with telemetry + emit game state
    let mut n = 0;
    emit_status(&sender, club, handed);
    select(&sender, &mut n, club, handed);

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
                // Cycle club/handedness on each completed shot
                if matches!(
                    msg.as_frp().map(|m| &m.body),
                    Some(FrpBody::ShotFinished(_))
                ) {
                    club = Club::ALL[next_rand(&mut seed) % Club::ALL.len()];
                    handed = HANDEDNESS[next_rand(&mut seed) % HANDEDNESS.len()];
                    emit_status(&sender, club, handed);
                    select(&sender, &mut n, club, handed);
                }
            }
        }
    }
}
