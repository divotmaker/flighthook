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
    pub r10: std::collections::HashMap<String, R10Section>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub square: std::collections::HashMap<String, SquareSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub mock_monitor: std::collections::HashMap<String, MockMonitorSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub openconnect_server: std::collections::HashMap<String, OpenConnectServerSection>,
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

/// Camera mode requested from the device at session start.
///
/// Fusion processing is what produces club data (path, face angle, attack
/// angle, dynamic loft, smash factor, swing planes). It needs the Pro Package
/// enabled on the device; without it the device reports ball flight only,
/// whatever this is set to. The two Fusion variants are firmware-dependent —
/// picking the wrong one yields no club data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    /// Ball flight only, no Fusion processing. The default.
    #[default]
    Standard,
    /// High-resolution JPEG Fusion (1640x1232), older firmware.
    Fusion,
    /// Raw Fusion (640x480 @ 180fps), firmware BM17.04 and newer.
    RawFusion,
}

impl CameraMode {
    /// Whether this mode asks the device for Fusion processing.
    #[must_use]
    pub fn is_fusion(self) -> bool {
        !matches!(self, Self::Standard)
    }

    /// Stable key used by the settings UI and config file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Fusion => "fusion",
            Self::RawFusion => "raw_fusion",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard (ball flight only)",
            Self::Fusion => "Fusion — older firmware",
            Self::RawFusion => "Raw Fusion — BM17.04+",
        }
    }

    /// Every mode, in display order.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Standard, Self::Fusion, Self::RawFusion]
    }
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
    /// Camera mode. Defaults to `Standard` (ball flight only) when absent.
    /// Fusion modes additionally require the Pro Package on the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_mode: Option<CameraMode>,
}

/// A Garmin R10 BLE device instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct R10Section {
    #[serde(default)]
    pub name: String,
    /// Distance from the device to the tee, sent to the R10 as `tee_range`
    /// once it wakes up. Garmin recommends placing the R10 6-8 ft behind the
    /// ball.
    ///
    /// When absent, no shot config is sent and the device keeps whatever tee
    /// distance was last set on it (e.g. by the Garmin Golf app).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Distance>,
}

/// A Square Golf Omni BLE device instance.
///
/// The original Square / Square Home is not supported: it uses a different
/// club-code scheme.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SquareSection {
    #[serde(default)]
    pub name: String,
    /// BLE address to connect to. When absent, the first device advertising the
    /// `SquareGolf` name prefix is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Club selected on connect, e.g. `"7i"`, `"driver"`, `"putter"`.
    ///
    /// The device is told which club is in play — it affects how the shot is
    /// classified. When GSPro reports a club change, the actor follows it and
    /// this is only the starting value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club: Option<String>,
    /// Use the device's advanced spin measurement. Defaults to true, matching
    /// the vendor app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_spin: Option<bool>,
    /// Discard shots that report zero spin, unless the putter is selected.
    ///
    /// A struck ball always spins, so a zero-spin read is a failed read —
    /// typically a ball near the edge of the detection zone. Passing it through
    /// sends a spinless shot to the sim, which flies far too long.
    ///
    /// Putts are always exempt: there is no airborne flight for the device to
    /// measure spin over, so a putt reads zero every time and discarding those
    /// would make putting impossible.
    ///
    /// Defaults to true when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discard_non_putting_zero_spin: Option<bool>,
}

/// A mock launch monitor instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockMonitorSection {
    #[serde(default)]
    pub name: String,
}

/// An OpenConnect server instance — a *launch monitor*, not an integration.
///
/// Accepts inbound shot data from monitors that speak GSPro Open Connect V1 as
/// a client (Uneekor, Foresight, SkyTrak, MLM2PRO, …). This is the inverse of
/// [`GsProSection`], which dials GSPro as a client.
///
/// GSPro listens on 921 as well, but its port is movable: set
/// `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
/// `C:\GSPro\GSPC\GSPconnect.exe.config` to move GSPConnect to 922 and free
/// 921 for this actor, so both can share one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenConnectServerSection {
    #[serde(default)]
    pub name: String,
    /// Bind address. Defaults to `0.0.0.0:921`.
    pub bind: Option<String>,
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

    /// Returns true if any user-configured actors (devices or integrations)
    /// exist. Webservers are infrastructure and don't count.
    pub fn has_user_actors(&self) -> bool {
        !self.mevo.is_empty()
            || !self.r10.is_empty()
            || !self.square.is_empty()
            || !self.mock_monitor.is_empty()
            || !self.openconnect_server.is_empty()
            || !self.gspro.is_empty()
            || !self.random_club.is_empty()
    }
}

impl Default for FlighthookConfig {
    /// Minimal empty config — webserver only, no devices or integrations.
    /// The setup wizard will add devices on first startup.
    fn default() -> Self {
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
            mevo: std::collections::HashMap::new(),
            r10: std::collections::HashMap::new(),
            square: std::collections::HashMap::new(),
            mock_monitor: std::collections::HashMap::new(),
            openconnect_server: std::collections::HashMap::new(),
            gspro: std::collections::HashMap::new(),
            random_club: std::collections::HashMap::new(),
        }
    }
}

impl Default for MevoSection {
    fn default() -> Self {
        Self {
            name: "Mevo WiFi".into(),
            address: Some("192.168.2.1:5100".into()),
            ball_type: Some(0),
            tee_height: Some(Distance::Inches(1.5)),
            range: Some(Distance::Feet(8.0)),
            surface_height: Some(Distance::Inches(0.0)),
            track_pct: Some(80.0),
            use_estimated: None,
            camera_mode: None,
        }
    }
}

impl Default for R10Section {
    fn default() -> Self {
        Self {
            name: "Garmin R10".into(),
            // Absent: leave the device's own tee distance untouched.
            range: None,
        }
    }
}

impl Default for SquareSection {
    fn default() -> Self {
        Self {
            name: "Square Golf Omni".into(),
            // Auto-discover by name prefix.
            address: None,
            club: None,
            advanced_spin: None,
            discard_non_putting_zero_spin: Some(true),
        }
    }
}

impl Default for OpenConnectServerSection {
    fn default() -> Self {
        Self {
            name: "OpenConnect Server".into(),
            bind: Some("0.0.0.0:921".into()),
        }
    }
}

impl Default for GsProSection {
    fn default() -> Self {
        Self {
            name: "Local GSPro".into(),
            address: Some("127.0.0.1:921".into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
        }
    }
}

#[cfg(test)]
mod camera_mode_tests {
    use super::*;

    #[test]
    fn absent_camera_mode_defaults_to_standard() {
        let s: MevoSection = serde_json::from_str(r#"{"name":"Mevo"}"#).expect("parse");
        assert_eq!(s.camera_mode, None);
        assert_eq!(s.camera_mode.unwrap_or_default(), CameraMode::Standard);
    }

    #[test]
    fn camera_mode_round_trips_by_key() {
        for mode in CameraMode::all() {
            let json = format!(r#"{{"name":"Mevo","camera_mode":"{}"}}"#, mode.key());
            let s: MevoSection = serde_json::from_str(&json).expect("parse");
            assert_eq!(s.camera_mode, Some(mode), "{}", mode.key());
        }
    }

    #[test]
    fn standard_camera_mode_is_not_written_back() {
        // `skip_serializing_if` keeps an untouched config file unchanged.
        let s = MevoSection::default();
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("camera_mode"), "{json}");
    }

    #[test]
    fn only_standard_skips_fusion() {
        assert!(!CameraMode::Standard.is_fusion());
        assert!(CameraMode::Fusion.is_fusion());
        assert!(CameraMode::RawFusion.is_fusion());
    }
}
