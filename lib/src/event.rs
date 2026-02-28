//! Shared domain types used by both the unified bus and the session thread.
//!
//! These are pure data structures with no channel affinity. The bus message
//! types in `message.rs` reference them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{Distance, PartialMode, UnitSystem, Velocity};

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BallFlight {
    // Critical launch conditions — always present
    pub launch_speed: Velocity,
    pub launch_elevation: f64, // deg (VLA)
    pub launch_azimuth: f64,   // deg (HLA)
    // Flight results
    #[serde(default)]
    pub carry_distance: Option<Distance>,
    #[serde(default)]
    pub total_distance: Option<Distance>,
    #[serde(default)]
    pub max_height: Option<Distance>,
    #[serde(default)]
    pub flight_time: Option<f64>, // s
    #[serde(default)]
    pub roll_distance: Option<Distance>,
    // Spin from ball flight message (D4/E8) — separate from SpinData (EF)
    #[serde(default)]
    pub backspin_rpm: Option<i32>,
    #[serde(default)]
    pub sidespin_rpm: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClubData {
    pub club_speed: Velocity,
    #[serde(default)]
    pub path: Option<f64>, // deg (strike direction)
    #[serde(default)]
    pub attack_angle: Option<f64>, // deg
    #[serde(default)]
    pub face_angle: Option<f64>, // deg
    #[serde(default)]
    pub dynamic_loft: Option<f64>, // deg
    #[serde(default)]
    pub smash_factor: Option<f64>,
    #[serde(default)]
    pub club_speed_post: Option<Velocity>,
    #[serde(default)]
    pub swing_plane_horizontal: Option<f64>, // deg
    #[serde(default)]
    pub swing_plane_vertical: Option<f64>, // deg
    #[serde(default)]
    pub club_offset: Option<Distance>,
    #[serde(default)]
    pub club_height: Option<Distance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpinData {
    pub total_spin: i16, // RPM
    pub spin_axis: f64,  // deg
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShotData {
    #[serde(default)]
    pub source: String,
    pub shot_number: i32,
    pub ball: BallFlight,
    #[serde(default)]
    pub club: Option<ClubData>,
    #[serde(default)]
    pub spin: Option<SpinData>,
    #[serde(default)]
    pub estimated: bool,
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
        let ball = BallFlight {
            launch_speed: Velocity::MilesPerHour(self.ball.launch_speed.as_mph()),
            launch_elevation: self.ball.launch_elevation,
            launch_azimuth: self.ball.launch_azimuth,
            carry_distance: self
                .ball
                .carry_distance
                .map(|d| Distance::Yards(d.as_yards())),
            total_distance: self
                .ball
                .total_distance
                .map(|d| Distance::Yards(d.as_yards())),
            max_height: self.ball.max_height.map(|d| Distance::Feet(d.as_feet())),
            flight_time: self.ball.flight_time,
            roll_distance: self
                .ball
                .roll_distance
                .map(|d| Distance::Yards(d.as_yards())),
            backspin_rpm: self.ball.backspin_rpm,
            sidespin_rpm: self.ball.sidespin_rpm,
        };
        let club = self.club.as_ref().map(|c| ClubData {
            club_speed: Velocity::MilesPerHour(c.club_speed.as_mph()),
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
            source: self.source.clone(),
            shot_number: self.shot_number,
            ball,
            club,
            spin: self.spin.clone(),
            estimated: self.estimated,
        }
    }

    fn to_metric(&self) -> ShotData {
        let ball = BallFlight {
            launch_speed: Velocity::MetersPerSecond(self.ball.launch_speed.as_mps()),
            launch_elevation: self.ball.launch_elevation,
            launch_azimuth: self.ball.launch_azimuth,
            carry_distance: self
                .ball
                .carry_distance
                .map(|d| Distance::Meters(d.as_meters())),
            total_distance: self
                .ball
                .total_distance
                .map(|d| Distance::Meters(d.as_meters())),
            max_height: self
                .ball
                .max_height
                .map(|d| Distance::Meters(d.as_meters())),
            flight_time: self.ball.flight_time,
            roll_distance: self
                .ball
                .roll_distance
                .map(|d| Distance::Meters(d.as_meters())),
            backspin_rpm: self.ball.backspin_rpm,
            sidespin_rpm: self.ball.sidespin_rpm,
        };
        let club = self.club.as_ref().map(|c| ClubData {
            club_speed: Velocity::MetersPerSecond(c.club_speed.as_mps()),
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
            source: self.source.clone(),
            shot_number: self.shot_number,
            ball,
            club,
            spin: self.spin.clone(),
            estimated: self.estimated,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevoConfigEvent {
    pub ball_type: u8,
    pub tee_height: Distance,
    pub range: Distance,
    pub surface_height: Distance,
    pub track_pct: f64, // 0-100 (user units)
    pub use_partial: PartialMode,
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

/// Actor state emitted on the bus. Carries lifecycle status and
/// actor-specific key/value telemetry (battery, tilt, club, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorState {
    pub status: ActorStatus,
    #[serde(default)]
    pub telemetry: HashMap<String, String>,
}

impl ActorState {
    pub fn new(status: ActorStatus, telemetry: HashMap<String, String>) -> Self {
        Self { status, telemetry }
    }
}
