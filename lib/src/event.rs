//! Shared domain types used by both the unified bus and the session thread.
//!
//! Shot data types (`BallFlight`, `ClubData`, `FaceImpact`) are re-exported
//! from the `flightrelay` crate. This module defines the composed `ShotData`
//! and accumulator types built on top of them.

use serde::{Deserialize, Serialize};

use flightrelay::types::{BallFlight, ClubData, FaceImpact};
use flightrelay::units::{Distance, Velocity};

use crate::{ShotKey, UnitSystem};

// ---------------------------------------------------------------------------
// Composed shot data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShotData {
    #[serde(default)]
    pub actor: String,
    pub shot_number: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ball: Option<BallFlight>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club: Option<ClubData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<FaceImpact>,
}

impl ShotData {
    /// Convert all distance and velocity fields to the given unit system.
    pub fn to_unit_system(&self, system: UnitSystem) -> ShotData {
        match system {
            UnitSystem::Imperial => self.to_imperial(),
            UnitSystem::Metric => self.to_metric(),
        }
    }

    fn to_imperial(&self) -> ShotData {
        let ball = self.ball.as_ref().map(|b| BallFlight {
            launch_speed: b.launch_speed.map(|v| Velocity::MilesPerHour(v.as_mph())),
            launch_elevation: b.launch_elevation,
            launch_azimuth: b.launch_azimuth,
            carry_distance: b.carry_distance.map(|d| Distance::Yards(d.as_yards())),
            total_distance: b.total_distance.map(|d| Distance::Yards(d.as_yards())),
            max_height: b.max_height.map(|d| Distance::Feet(d.as_feet())),
            flight_time: b.flight_time,
            roll_distance: b.roll_distance.map(|d| Distance::Yards(d.as_yards())),
            backspin_rpm: b.backspin_rpm,
            sidespin_rpm: b.sidespin_rpm,
        });
        let club = self.club.as_ref().map(|c| ClubData {
            club_speed: c.club_speed.map(|v| Velocity::MilesPerHour(v.as_mph())),
            path: c.path,
            attack_angle: c.attack_angle,
            face_angle: c.face_angle,
            dynamic_loft: c.dynamic_loft,
            smash_factor: c.smash_factor,
            club_speed_post: c
                .club_speed_post
                .map(|v| Velocity::MilesPerHour(v.as_mph())),
            swing_plane_horizontal: c.swing_plane_horizontal,
            swing_plane_vertical: c.swing_plane_vertical,
            club_offset: c.club_offset.map(|d| Distance::Inches(d.as_inches())),
            club_height: c.club_height.map(|d| Distance::Inches(d.as_inches())),
        });
        ShotData {
            actor: self.actor.clone(),
            shot_number: self.shot_number,
            ball,
            club,
            impact: self.impact.clone(),
        }
    }

    fn to_metric(&self) -> ShotData {
        let ball = self.ball.as_ref().map(|b| BallFlight {
            launch_speed: b
                .launch_speed
                .map(|v| Velocity::MetersPerSecond(v.as_mps())),
            launch_elevation: b.launch_elevation,
            launch_azimuth: b.launch_azimuth,
            carry_distance: b
                .carry_distance
                .map(|d| Distance::Meters(d.as_meters())),
            total_distance: b
                .total_distance
                .map(|d| Distance::Meters(d.as_meters())),
            max_height: b.max_height.map(|d| Distance::Meters(d.as_meters())),
            flight_time: b.flight_time,
            roll_distance: b
                .roll_distance
                .map(|d| Distance::Meters(d.as_meters())),
            backspin_rpm: b.backspin_rpm,
            sidespin_rpm: b.sidespin_rpm,
        });
        let club = self.club.as_ref().map(|c| ClubData {
            club_speed: c
                .club_speed
                .map(|v| Velocity::MetersPerSecond(v.as_mps())),
            path: c.path,
            attack_angle: c.attack_angle,
            face_angle: c.face_angle,
            dynamic_loft: c.dynamic_loft,
            smash_factor: c.smash_factor,
            club_speed_post: c
                .club_speed_post
                .map(|v| Velocity::MetersPerSecond(v.as_mps())),
            swing_plane_horizontal: c.swing_plane_horizontal,
            swing_plane_vertical: c.swing_plane_vertical,
            club_offset: c.club_offset.map(|d| Distance::Meters(d.as_meters())),
            club_height: c.club_height.map(|d| Distance::Meters(d.as_meters())),
        });
        ShotData {
            actor: self.actor.clone(),
            shot_number: self.shot_number,
            ball,
            club,
            impact: self.impact.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// ActorStatus — generic actor lifecycle
// ---------------------------------------------------------------------------

/// Generic actor lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorStatus {
    Starting,
    Disconnected,
    Connected,
    Reconnecting,
}

impl std::fmt::Display for ActorStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Disconnected => write!(f, "disconnected"),
            Self::Connected => write!(f, "connected"),
            Self::Reconnecting => write!(f, "reconnecting"),
        }
    }
}

// ---------------------------------------------------------------------------
// ShotAccumulator — consumer-side shot assembly
// ---------------------------------------------------------------------------

/// Collects shot lifecycle events (`BallFlight`, `ClubPath`, `FaceImpact`)
/// and produces a complete [`ShotData`] on `ShotFinished`.
///
/// Used by consumers that need all shot fields together (GSPro bridge,
/// web shot cache, UI shot grid). Keyed by `(actor, ShotKey)`.
#[derive(Debug)]
pub struct ShotAccumulator {
    pub actor: String,
    pub key: ShotKey,
    ball: Option<BallFlight>,
    club: Option<ClubData>,
    impact: Option<FaceImpact>,
}

impl ShotAccumulator {
    /// Create a new accumulator for a shot trigger.
    pub fn new(actor: String, key: ShotKey) -> Self {
        Self {
            actor,
            key,
            ball: None,
            club: None,
            impact: None,
        }
    }

    /// Record ball flight data.
    pub fn set_ball(&mut self, ball: BallFlight) {
        self.ball = Some(ball);
    }

    /// Record club path data.
    pub fn set_club(&mut self, club: ClubData) {
        self.club = Some(club);
    }

    /// Record face impact data.
    pub fn set_impact(&mut self, impact: FaceImpact) {
        self.impact = Some(impact);
    }

    /// Finalize into a `ShotData`. Returns `None` if no data arrived at all.
    pub fn finish(self) -> Option<ShotData> {
        if self.ball.is_none() && self.club.is_none() && self.impact.is_none() {
            return None;
        }
        Some(ShotData {
            actor: self.actor,
            shot_number: self.key.shot_number,
            ball: self.ball,
            club: self.club,
            impact: self.impact,
        })
    }
}

// ---------------------------------------------------------------------------
// ShotAggregator — feed FlighthookMessages, get complete ShotData out
// ---------------------------------------------------------------------------

/// High-level shot collector that manages multiple in-flight shots.
///
/// Feed [`FlighthookMessage`](crate::FlighthookMessage) events via
/// [`feed`](Self::feed) and receive complete [`ShotData`] when a shot
/// lifecycle finishes.
///
/// ```ignore
/// # use flighthook::{ShotAggregator, FlighthookClient};
/// let mut client = FlighthookClient::connect("ws://localhost:5880/frp", "my-app").unwrap();
/// let mut shots = ShotAggregator::new();
///
/// loop {
///     let msg = client.recv().unwrap();
///     if let Some(shot) = shots.feed(&msg) {
///         println!("shot #{}: {:?}", shot.shot_number, shot.ball);
///     }
/// }
/// ```
#[derive(Debug, Default)]
pub struct ShotAggregator {
    pending: std::collections::HashMap<(String, ShotKey), ShotAccumulator>,
}

impl ShotAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a bus message. Returns a completed [`ShotData`] when a
    /// `ShotFinished` event finalizes an accumulated shot.
    pub fn feed(&mut self, msg: &crate::FlighthookMessage) -> Option<ShotData> {
        match &msg.event {
            crate::FlighthookEvent::ShotTrigger { key } => {
                let acc = ShotAccumulator::new(msg.actor.clone(), key.clone());
                self.pending.insert((msg.actor.clone(), key.clone()), acc);
                None
            }
            crate::FlighthookEvent::BallFlight { key, ball } => {
                if let Some(acc) = self.pending.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_ball(*ball.clone());
                }
                None
            }
            crate::FlighthookEvent::ClubPath { key, club } => {
                if let Some(acc) = self.pending.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_club(*club.clone());
                }
                None
            }
            crate::FlighthookEvent::FaceImpact { key, impact } => {
                if let Some(acc) = self.pending.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_impact(*impact.clone());
                }
                None
            }
            crate::FlighthookEvent::ShotFinished { key } => self
                .pending
                .remove(&(msg.actor.clone(), key.clone()))
                .and_then(ShotAccumulator::finish),
            _ => None,
        }
    }

    /// Number of shots currently being accumulated.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}
