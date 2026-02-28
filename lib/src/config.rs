use std::fmt;

use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

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

/// A distance value with unit. Serializes as a suffix string: `"1.5in"`,
/// `"9ft"`, `"3m"`, `"30cm"`, `"100yd"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Distance {
    Feet(f64),
    Inches(f64),
    Meters(f64),
    Centimeters(f64),
    Yards(f64),
}

impl Serialize for Distance {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Distance {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

impl std::str::FromStr for Distance {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        // Try each suffix longest-first to avoid "m" matching before "cm"
        for (suffix, ctor) in &[
            ("cm", Self::Centimeters as fn(f64) -> Self),
            ("ft", Self::Feet as fn(f64) -> Self),
            ("in", Self::Inches as fn(f64) -> Self),
            ("yd", Self::Yards as fn(f64) -> Self),
            ("m", Self::Meters as fn(f64) -> Self),
        ] {
            if let Some(num) = s.strip_suffix(suffix) {
                let v: f64 = num
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid number in distance: {s:?}"))?;
                return Ok(ctor(v));
            }
        }
        Err(format!(
            "invalid distance {s:?}: expected number with suffix (ft, in, m, cm, yd)"
        ))
    }
}

impl Distance {
    pub fn value(self) -> f64 {
        match self {
            Self::Feet(v)
            | Self::Inches(v)
            | Self::Meters(v)
            | Self::Centimeters(v)
            | Self::Yards(v) => v,
        }
    }

    pub fn unit_suffix(self) -> &'static str {
        match self {
            Self::Feet(_) => "ft",
            Self::Inches(_) => "in",
            Self::Meters(_) => "m",
            Self::Centimeters(_) => "cm",
            Self::Yards(_) => "yd",
        }
    }

    pub fn unit_key(self) -> &'static str {
        match self {
            Self::Feet(_) => "feet",
            Self::Inches(_) => "inches",
            Self::Meters(_) => "meters",
            Self::Centimeters(_) => "centimeters",
            Self::Yards(_) => "yards",
        }
    }

    pub fn from_value_and_unit(value: f64, unit: &str) -> Self {
        match unit {
            "feet" => Self::Feet(value),
            "meters" => Self::Meters(value),
            "centimeters" => Self::Centimeters(value),
            "yards" => Self::Yards(value),
            _ => Self::Inches(value),
        }
    }

    pub fn as_feet(self) -> f64 {
        match self {
            Self::Feet(v) => v,
            Self::Inches(v) => v / 12.0,
            Self::Meters(v) => v / 0.3048,
            Self::Centimeters(v) => v / 30.48,
            Self::Yards(v) => v * 3.0,
        }
    }

    pub fn as_inches(self) -> f64 {
        match self {
            Self::Feet(v) => v * 12.0,
            Self::Inches(v) => v,
            Self::Meters(v) => v / 0.0254,
            Self::Centimeters(v) => v / 2.54,
            Self::Yards(v) => v * 36.0,
        }
    }

    pub fn as_meters(self) -> f64 {
        match self {
            Self::Feet(v) => v * 0.3048,
            Self::Inches(v) => v * 0.0254,
            Self::Meters(v) => v,
            Self::Centimeters(v) => v / 100.0,
            Self::Yards(v) => v * 0.9144,
        }
    }

    pub fn as_centimeters(self) -> f64 {
        match self {
            Self::Feet(v) => v * 30.48,
            Self::Inches(v) => v * 2.54,
            Self::Meters(v) => v * 100.0,
            Self::Centimeters(v) => v,
            Self::Yards(v) => v * 91.44,
        }
    }

    pub fn as_yards(self) -> f64 {
        match self {
            Self::Feet(v) => v / 3.0,
            Self::Inches(v) => v / 36.0,
            Self::Meters(v) => v / 0.9144,
            Self::Centimeters(v) => v / 91.44,
            Self::Yards(v) => v,
        }
    }

    pub fn as_mm(self) -> u16 {
        (self.as_meters() * 1000.0) as u16
    }
}

impl Default for Distance {
    fn default() -> Self {
        Self::Feet(0.0)
    }
}

impl fmt::Display for Distance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.value(), self.unit_suffix())
    }
}

/// A velocity value with unit. Serializes as a suffix string: `"67.2mph"`,
/// `"30.0mps"`, `"108.0kph"`, `"100.0fps"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Velocity {
    MilesPerHour(f64),
    FeetPerSecond(f64),
    MetersPerSecond(f64),
    KilometersPerHour(f64),
}

impl Serialize for Velocity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Velocity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

impl std::str::FromStr for Velocity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        // Try each suffix longest-first to avoid "mps" matching before "mph"
        for (suffix, ctor) in &[
            ("mph", Self::MilesPerHour as fn(f64) -> Self),
            ("mps", Self::MetersPerSecond as fn(f64) -> Self),
            ("kph", Self::KilometersPerHour as fn(f64) -> Self),
            ("fps", Self::FeetPerSecond as fn(f64) -> Self),
        ] {
            if let Some(num) = s.strip_suffix(suffix) {
                let v: f64 = num
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid number in velocity: {s:?}"))?;
                return Ok(ctor(v));
            }
        }
        Err(format!(
            "invalid velocity {s:?}: expected number with suffix (mph, mps, kph, fps)"
        ))
    }
}

impl Velocity {
    pub fn value(self) -> f64 {
        match self {
            Self::MilesPerHour(v)
            | Self::FeetPerSecond(v)
            | Self::MetersPerSecond(v)
            | Self::KilometersPerHour(v) => v,
        }
    }

    pub fn unit_suffix(self) -> &'static str {
        match self {
            Self::MilesPerHour(_) => "mph",
            Self::FeetPerSecond(_) => "fps",
            Self::MetersPerSecond(_) => "mps",
            Self::KilometersPerHour(_) => "kph",
        }
    }

    pub fn unit_key(self) -> &'static str {
        match self {
            Self::MilesPerHour(_) => "mph",
            Self::FeetPerSecond(_) => "fps",
            Self::MetersPerSecond(_) => "mps",
            Self::KilometersPerHour(_) => "kph",
        }
    }

    pub fn from_value_and_unit(value: f64, unit: &str) -> Self {
        match unit {
            "mph" => Self::MilesPerHour(value),
            "fps" => Self::FeetPerSecond(value),
            "kph" => Self::KilometersPerHour(value),
            _ => Self::MetersPerSecond(value),
        }
    }

    pub fn as_mph(self) -> f64 {
        match self {
            Self::MilesPerHour(v) => v,
            Self::FeetPerSecond(v) => v * 0.681818,
            Self::MetersPerSecond(v) => v * 2.23694,
            Self::KilometersPerHour(v) => v * 0.621371,
        }
    }

    pub fn as_fps(self) -> f64 {
        match self {
            Self::MilesPerHour(v) => v * 1.46667,
            Self::FeetPerSecond(v) => v,
            Self::MetersPerSecond(v) => v * 3.28084,
            Self::KilometersPerHour(v) => v * 0.911344,
        }
    }

    pub fn as_mps(self) -> f64 {
        match self {
            Self::MilesPerHour(v) => v * 0.44704,
            Self::FeetPerSecond(v) => v * 0.3048,
            Self::MetersPerSecond(v) => v,
            Self::KilometersPerHour(v) => v / 3.6,
        }
    }

    pub fn as_kph(self) -> f64 {
        match self {
            Self::MilesPerHour(v) => v * 1.60934,
            Self::FeetPerSecond(v) => v * 1.09728,
            Self::MetersPerSecond(v) => v * 3.6,
            Self::KilometersPerHour(v) => v,
        }
    }
}

impl Default for Velocity {
    fn default() -> Self {
        Self::MetersPerSecond(0.0)
    }
}

impl fmt::Display for Velocity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.value(), self.unit_suffix())
    }
}

/// Controls whether the E8 (FlightResultV1) fallback is used when the device
/// does not send a full D4 flight result (common for short chips).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialMode {
    Never,
    ChippingOnly,
    Always,
}

impl Default for PartialMode {
    fn default() -> Self {
        Self::ChippingOnly
    }
}

/// Unit system for display. Imperial = yards/feet/inches/mph, Metric = meters/m/s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitSystem {
    Imperial,
    Metric,
}

impl Default for UnitSystem {
    fn default() -> Self {
        Self::Imperial
    }
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
// Persisted config types (shared between app and UI)
// ---------------------------------------------------------------------------

/// Top-level persisted config. All fields are in user-friendly units
/// (inches, feet, 0-100 percent) so the TOML file is hand-editable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlighthookConfig {
    /// Default unit system for shot display (freedom units by default)
    #[serde(default)]
    pub default_units: UnitSystem,
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
    pub use_partial: Option<PartialMode>,
}

/// A mock launch monitor instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockMonitorSection {
    #[serde(default)]
    pub name: String,
}

/// A GSPro integration instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
                use_partial: Some(PartialMode::ChippingOnly),
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
                bind: "0.0.0.0:3030".into(),
            },
        );
        Self {
            default_units: UnitSystem::default(),
            webserver,
            mevo,
            mock_monitor: std::collections::HashMap::new(),
            gspro,
            random_club: std::collections::HashMap::new(),
        }
    }
}
