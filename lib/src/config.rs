use std::fmt;

use flightrelay::units::{Distance, Velocity};
use serde::{Deserialize, Serialize};

use crate::game_state::Club;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum ShotDetectionMode {
    Full,
    Putting,
    Chipping,
}

impl fmt::Display for ShotDetectionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => write!(f, "full"),
            Self::Putting => write!(f, "putting"),
            Self::Chipping => write!(f, "chipping"),
        }
    }
}

impl From<ShotDetectionMode> for flightrelay::DetectionMode {
    fn from(m: ShotDetectionMode) -> Self {
        match m {
            ShotDetectionMode::Full => Self::Full,
            ShotDetectionMode::Putting => Self::Putting,
            ShotDetectionMode::Chipping => Self::Chipping,
        }
    }
}

impl From<flightrelay::DetectionMode> for ShotDetectionMode {
    fn from(m: flightrelay::DetectionMode) -> Self {
        match m {
            flightrelay::DetectionMode::Full => Self::Full,
            flightrelay::DetectionMode::Putting => Self::Putting,
            flightrelay::DetectionMode::Chipping => Self::Chipping,
        }
    }
}

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

// ---------------------------------------------------------------------------
// Persisted config types (shared between app and UI)
// ---------------------------------------------------------------------------

/// Top-level persisted config. All fields are in user-friendly units
/// (inches, feet, 0-100 percent) so the TOML file is hand-editable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlighthookConfig {
    /// Default unit system for shot display (freedom units by default)
    #[serde(default)]
    pub default_units: UnitSystem,
    /// Clubs that trigger Chipping mode on selection.
    #[serde(default = "default_chipping_clubs")]
    pub chipping_clubs: Vec<Club>,
    /// Clubs that trigger Putting mode on selection.
    #[serde(default = "default_putting_clubs")]
    pub putting_clubs: Vec<Club>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub webserver: std::collections::HashMap<String, WebserverSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub mevo: std::collections::HashMap<String, MevoSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub mock_monitor: std::collections::HashMap<String, MockMonitorSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub gspro: std::collections::HashMap<String, GsProSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub random_club: std::collections::HashMap<String, RandomClubSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebserverSection {
    #[serde(default)]
    pub name: String,
    pub bind: String,
}

/// A Mevo/Mevo+ device instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MevoSection {
    #[serde(default)]
    pub name: String,
    pub address: Option<String>,
    pub ball_type: Option<u8>,
    pub tee_height: Option<Distance>,
    pub range: Option<Distance>,
    pub surface_height: Option<Distance>,
    pub track_pct: Option<f64>,
    /// Whether to use estimated (E8 fallback) shots. Defaults to true when
    /// absent for backwards compatibility. Estimated shots may lack sidespin
    /// and carry less data, but are often the only result for short chips.
    #[serde(default)]
    pub use_estimated: Option<bool>,
}

/// A mock launch monitor instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockMonitorSection {
    #[serde(default)]
    pub name: String,
}

/// A GSPro integration instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GsProSection {
    #[serde(default)]
    pub name: String,
    pub address: Option<String>,
    /// Actor ID for full-swing shots (e.g. "mevo.0"). None = accept from any monitor.
    #[serde(default)]
    pub full_monitor: Option<String>,
    /// Actor ID for chipping shots. None = accept from any monitor.
    #[serde(default)]
    pub chipping_monitor: Option<String>,
    /// Actor ID for putting shots. None = accept from any monitor.
    #[serde(default)]
    pub putting_monitor: Option<String>,
}

/// A random club cycling integration instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RandomClubSection {
    #[serde(default)]
    pub name: String,
}

impl FlighthookConfig {
    /// Look up the detection mode for a club based on the configured mapping.
    ///
    /// Clubs in `putting_clubs` → Putting, in `chipping_clubs` → Chipping,
    /// everything else → Full.
    pub fn club_mode(&self, club: Club) -> ShotDetectionMode {
        if self.putting_clubs.contains(&club) {
            ShotDetectionMode::Putting
        } else if self.chipping_clubs.contains(&club) {
            ShotDetectionMode::Chipping
        } else {
            ShotDetectionMode::Full
        }
    }
}

impl Default for FlighthookConfig {
    /// Known good defaults: a single Mevo device and a GSPro target.
    fn default() -> Self {
        let mut mevo = std::collections::HashMap::new();
        mevo.insert(
            "0".into(),
            MevoSection {
                name: "Mevo WiFi".into(),
                address: Some("192.168.2.1:5100".into()),
                ball_type: Some(0),
                tee_height: Some(Distance::Inches(1.5)),
                range: Some(Distance::Feet(8.0)),
                surface_height: Some(Distance::Inches(0.0)),
                track_pct: Some(80.0),
                use_estimated: None,
            },
        );
        let mut gspro = std::collections::HashMap::new();
        gspro.insert(
            "0".into(),
            GsProSection {
                name: "Local GSPro".into(),
                address: Some("127.0.0.1:921".into()),
                full_monitor: None,
                chipping_monitor: None,
                putting_monitor: None,
            },
        );
        let mut webserver = std::collections::HashMap::new();
        webserver.insert(
            "0".into(),
            WebserverSection {
                name: "Web Server".into(),
                bind: "0.0.0.0:5880".into(),
            },
        );
        Self {
            default_units: UnitSystem::default(),
            chipping_clubs: default_chipping_clubs(),
            putting_clubs: default_putting_clubs(),
            webserver,
            mevo,
            mock_monitor: std::collections::HashMap::new(),
            gspro,
            random_club: std::collections::HashMap::new(),
        }
    }
}
