//! The config file (`FlighthookConfig`): its schema and (de)serialization here, the sections
//! and the rules over a whole config in the submodules.

use std::fmt;

use allsquare::config::Config as SquareConfig;
use flightrelay::config::DriverConfig;
use flightrelay::mock::MockConfig;
use flightrelay::units::{Distance, Velocity};
use ironsight::config::Config as MevoConfig;
pub use ironsight::config::{CameraMode, FlightScopeEnvironment, SpinSource};
use serde::{Deserialize, Serialize};
use tenover::config::Config as R10Config;
use threeputt::config::Config as ExPuttConfig;
use thumbswing::Settings as TrackballConfig;

use crate::group::ShotGrouping;
use crate::{Club, Handedness};
#[cfg(test)]
mod body_tracking_tests;
#[cfg(test)]
mod camera_tests;
mod cameras;
mod defaults;
mod devices;
#[cfg(test)]
mod exputt_section_tests;
mod general;
mod golfers;
mod impact_lead;
#[cfg(test)]
mod impact_lead_tests;
#[cfg(test)]
mod latency_tests;
#[cfg(test)]
mod launch_only_spin_tests;
#[cfg(test)]
mod mevo_section_tests;
#[cfg(test)]
mod mock_section_tests;
#[cfg(test)]
mod openconnect_client_tests;
mod outputs;
#[cfg(test)]
mod pureplay_accessory_section_tests;
#[cfg(test)]
mod r10_section_tests;
mod rules;
#[cfg(test)]
mod shot_type_tests;
#[cfg(test)]
mod simulation_tests;
#[cfg(test)]
mod simulator_route_tests;
#[cfg(test)]
mod square_section_tests;
#[cfg(test)]
mod trackball_section_tests;

pub use cameras::*;
pub use devices::*;
pub use general::*;
pub use golfers::*;
pub use impact_lead::*;
pub use outputs::*;

/// The kind of stroke the golfer is about to play (FRP's `mode`).
pub use flightrelay::Mode;

// ---------------------------------------------------------------------------
// Extension traits for flightrelay unit types (UI helpers, not protocol)
// ---------------------------------------------------------------------------

/// Flighthook-specific helpers on [`Distance`] for UI dropdowns and wire protocol shortcuts.
pub trait DistanceExt {
    fn unit_key(self) -> &'static str;
    fn from_value_and_unit(value: f64, unit: &str) -> Self;
    fn to_mm(self) -> u16;
}

impl DistanceExt for Distance {
    fn unit_key(self) -> &'static str {
        match self {
            Self::Feet(_) => "feet",
            Self::Inches(_) => "inches",
            Self::Meters(_) => "meters",
            Self::Centimeters(_) => "centimeters",
            Self::Yards(_) => "yards",
            Self::Millimeters(_) => "millimeters",
        }
    }

    fn from_value_and_unit(value: f64, unit: &str) -> Self {
        match unit {
            "feet" => Self::Feet(value),
            "meters" => Self::Meters(value),
            "centimeters" => Self::Centimeters(value),
            "yards" => Self::Yards(value),
            "millimeters" => Self::Millimeters(value),
            _ => Self::Inches(value),
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn to_mm(self) -> u16 {
        self.as_millimeters() as u16
    }
}

/// Flighthook-specific helpers on [`Velocity`] for UI dropdowns.
pub trait VelocityExt {
    fn unit_key(self) -> &'static str;
    fn from_value_and_unit(value: f64, unit: &str) -> Self;
}

impl VelocityExt for Velocity {
    fn unit_key(self) -> &'static str {
        self.unit_suffix()
    }

    fn from_value_and_unit(value: f64, unit: &str) -> Self {
        match unit {
            "mph" => Self::MilesPerHour(value),
            "fps" => Self::FeetPerSecond(value),
            "kph" => Self::KilometersPerHour(value),
            _ => Self::MetersPerSecond(value),
        }
    }
}

/// Unit system for display. Imperial = yards/feet/inches/mph, Metric = meters/m/s.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitSystem {
    #[default]
    Imperial,
    Metric,
}

impl fmt::Display for UnitSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Imperial => write!(f, "imperial"),
            Self::Metric => write!(f, "metric"),
        }
    }
}

// ---------------------------------------------------------------------------
// Club-to-mode mapping defaults
// ---------------------------------------------------------------------------

pub fn default_chipping_clubs() -> Vec<Club> {
    vec![Club::GapWedge, Club::SandWedge, Club::LobWedge]
}

pub fn default_putting_clubs() -> Vec<Club> {
    vec![Club::Putter]
}

/// Clubs offered by the Settings screen's Chipping Clubs picker.
pub fn chipping_club_options() -> &'static [Club] {
    &[
        Club::PitchingWedge,
        Club::GapWedge,
        Club::SandWedge,
        Club::LobWedge,
    ]
}

/// Clubs offered by the Settings screen's Putting Clubs picker.
pub fn putting_club_options() -> &'static [Club] {
    &[Club::Putter]
}

// ---------------------------------------------------------------------------
// Persisted config types (shared between app and UI)
// ---------------------------------------------------------------------------

/// Top-level persisted config. All fields are in user-friendly units
/// (inches, feet, 0-100 percent) so the TOML file is hand-editable.
///
/// `gspro` sections (`[gspro.N]`, `"gspro": {...}`) are also accepted on
/// input and read as `openconnect_client` sections with the same index. When
/// both name the same index, the `openconnect_client` section is used.
/// Output only ever carries `openconnect_client`.
///
/// A `[player]` section (sex and height) is read into the golfers: the
/// first golfer takes its sex and height where it has none; with no golfer,
/// one named "Golfer" is made with them. A `golfer_height` in
/// `[body_tracking]` is read as `[player] height` when the player has no
/// height of their own. Output never carries `[player]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct FlighthookConfig {
    /// Default unit system for shot display (freedom units by default)
    #[serde(default)]
    pub default_units: UnitSystem,
    /// flighthook's own FRP actors' ids. Omitted while empty.
    #[serde(default, skip_serializing_if = "FrpSection::is_default")]
    pub frp: FrpSection,
    /// The `flighthook.*` id of the golfer last picked in flighthook: the
    /// golfer up at start. Omitted while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_golfer: Option<String>,
    /// Default shot view (by set, by club, none). Sets are assigned the same
    /// way in every view. Omitted from the file and the API while it is at
    /// its default (`by_set`).
    #[serde(default, skip_serializing_if = "ShotGrouping::is_default")]
    pub shot_grouping: ShotGrouping,
    /// Simulated flight and roll (beta). Omitted from the file and the API
    /// while it is at its defaults.
    #[serde(default, skip_serializing_if = "SimulationSection::is_default")]
    pub simulation: SimulationSection,
    /// Swing replay clips from the cameras (beta). Omitted from the file and
    /// the API while it is at its defaults.
    #[serde(default, skip_serializing_if = "ReplaySection::is_default")]
    pub replay: ReplaySection,
    /// Body tracking on the cameras' pictures (alpha). Omitted from the file
    /// and the API while it is at its defaults.
    #[serde(default, skip_serializing_if = "BodyTrackingSection::is_default")]
    pub body_tracking: BodyTrackingSection,
    /// The golfers, in display order. Omitted while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub golfers: Vec<GolferRecord>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub webserver: std::collections::HashMap<String, WebserverSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub mevo: std::collections::HashMap<String, MevoSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub r10: std::collections::HashMap<String, R10Section>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub square: std::collections::HashMap<String, SquareSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub exputt: std::collections::HashMap<String, ExPuttSection>,
    /// Trackball launch monitors (alpha): a trackball or mouse turned into
    /// a full shot (Linux).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub trackball: std::collections::HashMap<String, TrackballSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub mock_monitor: std::collections::HashMap<String, MockMonitorSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub openconnect_server: std::collections::HashMap<String, OpenConnectServerSection>,
    /// Open Connect clients: outputs that dial a simulator (GSPro, Pureplay,
    /// Muni Golf, ...).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub openconnect_client: std::collections::HashMap<String, OpenConnectClientSection>,
    /// Pureplay outputs (alpha): dial Pureplay over Pureplay Connect.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub pureplay: std::collections::HashMap<String, PureplaySection>,
    /// Pureplay Accessory cards (beta): Pureplay's Accessory API: the club both ways,
    /// Pureplay's hand and shots in flighthook, and Pureplay's result for
    /// each shot.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub pureplay_accessory: std::collections::HashMap<String, PureplayAccessorySection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub random_club: std::collections::HashMap<String, RandomClubSection>,
    /// Swing cameras (beta).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub camera: std::collections::HashMap<String, CameraSection>,
}

/// [`FlighthookConfig`] as read: its own fields, plus `gspro` sections.
/// The fields are [`FlighthookConfig`]'s derived deserializer (`remote =
/// "Self"`), flattened, so they are listed once.
#[derive(Deserialize)]
struct FlighthookConfigInput {
    #[serde(flatten)]
    config: FlighthookConfigFields,
    #[serde(default)]
    gspro: std::collections::HashMap<String, OpenConnectClientSection>,
    /// The club lists that were global settings: copied into every Open
    /// Connect client.
    #[serde(default)]
    chipping_clubs: Option<Vec<Club>>,
    #[serde(default)]
    putting_clubs: Option<Vec<Club>>,
    /// The player's sex and height: moved into the golfers.
    #[serde(default)]
    player: Option<PlayerSection>,
}

/// [`FlighthookConfig`] read by its derived deserializer.
struct FlighthookConfigFields(FlighthookConfig);

impl<'de> Deserialize<'de> for FlighthookConfigFields {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        FlighthookConfig::deserialize(d).map(Self)
    }
}

impl<'de> Deserialize<'de> for FlighthookConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let FlighthookConfigInput {
            config: FlighthookConfigFields(mut config),
            gspro,
            chipping_clubs,
            putting_clubs,
            player,
        } = FlighthookConfigInput::deserialize(d)?;
        for (index, section) in gspro {
            config.openconnect_client.entry(index).or_insert(section);
        }
        for section in config.openconnect_client.values_mut() {
            if let Some(clubs) = &chipping_clubs {
                section.chipping_clubs.clone_from(clubs);
            }
            if let Some(clubs) = &putting_clubs {
                section.putting_clubs.clone_from(clubs);
            }
        }
        let legacy = config.body_tracking.legacy_golfer_height.take();
        let mut player = player.unwrap_or_default();
        player.height = player.height.or(legacy);
        config.take_player(&player);
        Ok(config)
    }
}

impl Serialize for FlighthookConfig {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        FlighthookConfig::serialize(self, s)
    }
}
