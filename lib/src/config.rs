use std::fmt;

use flightrelay::units::{Distance, Velocity};
use serde::{Deserialize, Serialize};

use crate::game_state::Club;
use crate::group::ShotGrouping;

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
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
    /// Default shot view (by set, by club, none). Sets are assigned the same
    /// way in every view. Omitted from the file and the API while it is at
    /// its default (`by_set`).
    #[serde(default, skip_serializing_if = "ShotGrouping::is_default")]
    pub shot_grouping: ShotGrouping,
    /// Simulated flight and roll (beta). Omitted from the file and the API
    /// while it is at its defaults.
    #[serde(default, skip_serializing_if = "SimulationSection::is_default")]
    pub simulation: SimulationSection,
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
    /// Open Connect clients: outputs that dial a simulator (GSPro, Muni
    /// Golf, ...).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub openconnect_client: std::collections::HashMap<String, OpenConnectClientSection>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub random_club: std::collections::HashMap<String, RandomClubSection>,
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
        } = FlighthookConfigInput::deserialize(d)?;
        for (index, section) in gspro {
            config.openconnect_client.entry(index).or_insert(section);
        }
        Ok(config)
    }
}

impl Serialize for FlighthookConfig {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        FlighthookConfig::serialize(self, s)
    }
}

/// Settings for simulated flight and roll (beta).
///
/// [`mode`](Self::mode) picks what is simulated: nothing, the result values
/// the launch monitor does not report, or every result value. The rest sets
/// the conditions the shot is simulated under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulationSection {
    /// What is simulated. Defaults to [`SimMode::On`]; omitted from the file
    /// and the API while at the default.
    #[serde(default, skip_serializing_if = "SimMode::is_default")]
    pub mode: SimMode,
    /// Altitude above sea level, any distance unit. Defaults to `"0ft"`.
    /// Valid range: 0 to 5000 m.
    #[serde(default = "default_sim_altitude")]
    pub altitude: Distance,
    /// Air temperature, degrees Celsius. Defaults to 25. Valid range: -30
    /// to 50.
    #[serde(default = "default_sim_temperature_c")]
    pub temperature_c: f64,
    /// Ground the ball lands and rolls on. Defaults to `fairway`.
    #[serde(default)]
    pub surface: SimSurface,
}

/// What flighthook simulates for each shot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimMode {
    /// No simulation: only what the launch monitor reports.
    Off,
    /// Simulate the result values (carry, apex, flight time, descent,
    /// offline, roll, total) the launch monitor does not report. The default.
    #[default]
    On,
    /// Simulate every result value from the measured launch, for every
    /// device. The device's own results are kept but not shown. Launch values
    /// (ball speed, launch angles, spin, club data) are never replaced.
    Force,
}

impl SimMode {
    /// True for the default mode ([`SimMode::On`]).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Stable key used by the settings UI and config file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Force => "force",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::On => "On",
            Self::Force => "Force",
        }
    }

    /// Every mode, in display order.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Off, Self::On, Self::Force]
    }
}

fn default_sim_altitude() -> Distance {
    Distance::Feet(0.0)
}

fn default_sim_temperature_c() -> f64 {
    SimulationSection::DEFAULT_TEMPERATURE_C
}

impl Default for SimulationSection {
    fn default() -> Self {
        Self {
            mode: SimMode::default(),
            altitude: default_sim_altitude(),
            temperature_c: default_sim_temperature_c(),
            surface: SimSurface::default(),
        }
    }
}

impl SimulationSection {
    /// Default air temperature (°C): the ball model's calibration point.
    pub const DEFAULT_TEMPERATURE_C: f64 = 25.0;
    /// Highest accepted altitude (m).
    pub const MAX_ALTITUDE_M: f64 = 5000.0;
    /// Lowest accepted temperature (°C).
    pub const MIN_TEMPERATURE_C: f64 = -30.0;
    /// Highest accepted temperature (°C).
    pub const MAX_TEMPERATURE_C: f64 = 50.0;

    /// True when every setting is at its default. Altitude compares by
    /// length, so `"0m"` and `"0ft"` are both default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.mode.is_default()
            && self.altitude.as_meters() == 0.0
            && self.temperature_c == Self::DEFAULT_TEMPERATURE_C
            && self.surface == SimSurface::default()
    }

    /// Why the altitude or temperature is out of range, or `None` when both
    /// are valid. Shots are not simulated while this is `Some`.
    #[must_use]
    pub fn environment_error(&self) -> Option<String> {
        let alt = self.altitude.as_meters();
        if !alt.is_finite() || !(0.0..=Self::MAX_ALTITUDE_M).contains(&alt) {
            return Some(format!(
                "altitude {} is outside 0 to {} m",
                self.altitude,
                Self::MAX_ALTITUDE_M
            ));
        }
        let t = self.temperature_c;
        if !t.is_finite() || !(Self::MIN_TEMPERATURE_C..=Self::MAX_TEMPERATURE_C).contains(&t) {
            return Some(format!(
                "temperature {t} C is outside {} to {} C",
                Self::MIN_TEMPERATURE_C,
                Self::MAX_TEMPERATURE_C
            ));
        }
        None
    }
}

/// Ground for simulated landing and roll. A ground ball taken in Putting
/// mode rolls on `Green` when this is `Fairway`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimSurface {
    /// Flat fairway. The default.
    #[default]
    Fairway,
    /// Flat green at normal speed.
    Green,
    /// Flat firm, fast green.
    FirmGreen,
}

impl SimSurface {
    /// Stable key used by the settings UI and config file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Fairway => "fairway",
            Self::Green => "green",
            Self::FirmGreen => "firm_green",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Fairway => "Fairway",
            Self::Green => "Green",
            Self::FirmGreen => "Firm green",
        }
    }

    /// Every surface, in display order.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Fairway, Self::Green, Self::FirmGreen]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebserverSection {
    #[serde(default)]
    pub name: String,
    pub bind: String,
}

/// Camera mode requested from the device at session start.
///
/// Every mode reports ball flight and the device's radar club data (club
/// speed, attack angle, path, and face and loft where the device provides
/// them). Fusion processing adds the full club data set (path, face angle,
/// attack angle, dynamic loft, smash factor, swing planes). It needs the Pro
/// Package enabled on the device; without it the device reports ball flight
/// and radar club data only, whatever this is set to. The two Fusion variants
/// are firmware-dependent — picking the wrong one yields no Fusion club data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    /// Ball flight and radar club data, no Fusion processing. The default.
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
            Self::Standard => "Standard (radar only)",
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

/// Where a FlightScope device is used, which picks its full-swing detection
/// mode.
///
/// FlightScope's own app calls these "Limited flight" and "Outdoor".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlightScopeEnvironment {
    /// Nets and simulator screens, where the ball's flight is cut short. The
    /// default.
    #[default]
    LimitedFlight,
    /// Full ball flight outdoors. Needs about 40 yd of unobstructed flight;
    /// shots into a net or screen do not read.
    Outdoor,
}

impl FlightScopeEnvironment {
    /// Stable key used by the settings UI and config file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::LimitedFlight => "limited_flight",
            Self::Outdoor => "outdoor",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LimitedFlight => "Limited flight (sim/indoor)",
            Self::Outdoor => "Outdoor (full flight)",
        }
    }

    /// Every environment, in display order.
    #[must_use]
    pub fn all() -> [Self; 2] {
        [Self::LimitedFlight, Self::Outdoor]
    }
}

/// Which backspin and sidespin a FlightScope Xi shot reports.
///
/// The Xi sends two backspin values per shot: `D4`, its final spin (the one
/// FlightScope's own app displays), and `E8`, a second value. On a Mevo+ or
/// Gen2 there is no `E8` value, so all three sources give the same result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpinSource {
    /// Backspin and sidespin from the D4 result, exactly as reported. The
    /// default.
    #[default]
    D4,
    /// Total spin from the E8 result, with the spin axis from D4 when the
    /// shot has club data and 0 otherwise.
    E8,
    /// The `E8` rule when flighthook's detection mode is Full at shot time,
    /// otherwise `D4` exactly as reported.
    Auto,
}

impl SpinSource {
    /// Whether this is the default (`D4`). Used to keep the field out of
    /// the config file when it is unset.
    #[must_use]
    pub fn is_d4(&self) -> bool {
        matches!(self, Self::D4)
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::D4 => "D4 (matches FS Golf)",
            Self::E8 => "E8",
            Self::Auto => "Auto (E8 on full swings)",
        }
    }

    /// Every source, in display order.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::D4, Self::E8, Self::Auto]
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
    /// Whether to use estimated (E8 fallback) shots when no full result
    /// arrives. Defaults to false when absent: a shot with only an estimate
    /// (e.g. a flight too short to read in Outdoor mode, or a very short
    /// chip) is skipped. Estimated shots may lack sidespin and carry less
    /// data.
    #[serde(default)]
    pub use_estimated: Option<bool>,
    /// Camera mode. Defaults to `Standard` (no Fusion processing) when absent.
    /// Fusion modes additionally require the Pro Package on the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_mode: Option<CameraMode>,
    /// Environment: limited flight (nets, sim screens) or full-flight
    /// outdoor. Sets the full-swing detection mode. Defaults to
    /// `LimitedFlight` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<FlightScopeEnvironment>,
    /// FlightScope Xi only: which spin the shot reports (`d4`, `e8`, or
    /// `auto`). Defaults to `D4`, and is left out of the config file at the
    /// default.
    #[serde(default, skip_serializing_if = "SpinSource::is_d4")]
    pub spin_source: SpinSource,
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
    /// Club selected on connect, e.g. `"7i"`, `"DR"`, `"PT"`.
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
    /// Per-club override for face impact calibration: distance from the bottom
    /// edge of the club sticker's dot down to face centre, in millimetres.
    /// Measure with the sticker in its normal spot.
    ///
    /// Keyed the same way `club` is (e.g. `"DR"`, `"7i"`, `"pw"`). Clubs not
    /// listed use allsquare's built-in defaults; the putter has no built-in
    /// default and reports no vertical impact unless given one here.
    #[serde(default, skip_serializing_if = "is_none_or_empty_map")]
    pub dot_bottom_to_face_centre_mm: Option<std::collections::BTreeMap<String, f64>>,
}

fn is_none_or_empty_map(m: &Option<std::collections::BTreeMap<String, f64>>) -> bool {
    m.as_ref().is_none_or(std::collections::BTreeMap::is_empty)
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
/// [`OpenConnectClientSection`], which dials a simulator as a client.
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

/// An OpenConnect client instance — an *output*.
///
/// Dials a simulator that accepts GSPro Open Connect V1 (GSPro on port 921,
/// Muni Golf on 9210) and sends it every shot. This is the inverse of
/// [`OpenConnectServerSection`], which accepts shots from a launch monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenConnectClientSection {
    #[serde(default)]
    pub name: String,
    /// Simulator address. Defaults to `127.0.0.1:921`.
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

/// Deprecated alias of [`OpenConnectClientSection`].
#[deprecated(note = "use OpenConnectClientSection")]
pub type GsProSection = OpenConnectClientSection;

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
            || !self.openconnect_client.is_empty()
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
            shot_grouping: ShotGrouping::default(),
            simulation: SimulationSection::default(),
            webserver,
            mevo: std::collections::HashMap::new(),
            r10: std::collections::HashMap::new(),
            square: std::collections::HashMap::new(),
            mock_monitor: std::collections::HashMap::new(),
            openconnect_server: std::collections::HashMap::new(),
            openconnect_client: std::collections::HashMap::new(),
            random_club: std::collections::HashMap::new(),
        }
    }
}

impl Default for MevoSection {
    fn default() -> Self {
        Self {
            name: "Mevo WiFi".into(),
            address: Some("192.168.2.1:5100".into()),
            ball_type: Some(1),
            tee_height: Some(Distance::Inches(1.5)),
            range: Some(Distance::Feet(8.0)),
            surface_height: Some(Distance::Inches(0.0)),
            track_pct: Some(80.0),
            use_estimated: None,
            camera_mode: None,
            environment: None,
            spin_source: SpinSource::D4,
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
            dot_bottom_to_face_centre_mm: None,
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

impl Default for OpenConnectClientSection {
    fn default() -> Self {
        Self {
            name: "GSPro".into(),
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

#[cfg(test)]
mod environment_tests {
    use super::*;

    #[test]
    fn absent_environment_defaults_to_limited_flight() {
        let s: MevoSection = serde_json::from_str(r#"{"name":"Mevo"}"#).expect("parse");
        assert_eq!(s.environment, None);
        assert_eq!(
            s.environment.unwrap_or_default(),
            FlightScopeEnvironment::LimitedFlight
        );
    }

    #[test]
    fn environment_round_trips_by_key() {
        for env in FlightScopeEnvironment::all() {
            let json = format!(r#"{{"name":"Mevo","environment":"{}"}}"#, env.key());
            let s: MevoSection = serde_json::from_str(&json).expect("parse");
            assert_eq!(s.environment, Some(env), "{}", env.key());
            let back = serde_json::to_string(&s).expect("serialize");
            assert!(
                back.contains(&format!(r#""environment":"{}""#, env.key())),
                "{back}"
            );
        }
    }

    #[test]
    fn absent_environment_is_not_written_back() {
        let s = MevoSection::default();
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("environment"), "{json}");
    }
}

#[cfg(test)]
mod spin_source_tests {
    use super::*;

    #[test]
    fn absent_spin_source_defaults_to_d4() {
        let s: MevoSection = serde_json::from_str(r#"{"name":"Mevo"}"#).expect("parse");
        assert_eq!(s.spin_source, SpinSource::D4);
    }

    #[test]
    fn d4_spin_source_is_not_written_back() {
        let mut s = MevoSection::default();
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("spin_source"), "{json}");
        // An explicit `d4` in the file is dropped on the next save.
        s = serde_json::from_str(r#"{"name":"Mevo","spin_source":"d4"}"#).expect("parse");
        assert_eq!(s.spin_source, SpinSource::D4);
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("spin_source"), "{json}");
    }

    #[test]
    fn spin_source_round_trips_by_key() {
        let keys = [
            (SpinSource::D4, "d4"),
            (SpinSource::E8, "e8"),
            (SpinSource::Auto, "auto"),
        ];
        assert_eq!(SpinSource::all().len(), keys.len());
        for (src, key) in keys {
            let json = format!(r#"{{"name":"Mevo","spin_source":"{key}"}}"#);
            let s: MevoSection = serde_json::from_str(&json).expect("parse");
            assert_eq!(s.spin_source, src, "{key}");
            let back: MevoSection =
                serde_json::from_str(&serde_json::to_string(&s).expect("serialize"))
                    .expect("parse");
            assert_eq!(back, s);
        }
    }
}

#[cfg(test)]
mod square_impact_calibration_tests {
    use super::*;

    #[test]
    fn absent_map_is_not_written_back() {
        let s = SquareSection::default();
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("dot_bottom_to_face_centre_mm"), "{json}");
    }

    #[test]
    fn empty_map_is_not_written_back() {
        let s = SquareSection {
            dot_bottom_to_face_centre_mm: Some(std::collections::BTreeMap::new()),
            ..SquareSection::default()
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("dot_bottom_to_face_centre_mm"), "{json}");
    }

    #[test]
    fn map_round_trips() {
        let mut s = SquareSection::default();
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert("driver".to_string(), 19.0);
        overrides.insert("7i".to_string(), 23.0);
        s.dot_bottom_to_face_centre_mm = Some(overrides.clone());

        let json = serde_json::to_string(&s).expect("serialize");
        let back: SquareSection = serde_json::from_str(&json).expect("parse");
        assert_eq!(back.dot_bottom_to_face_centre_mm, Some(overrides));
    }

    #[test]
    fn absent_map_parses_as_none() {
        let s: SquareSection = serde_json::from_str(r#"{"name":"Square"}"#).expect("parse");
        assert_eq!(s.dot_bottom_to_face_centre_mm, None);
    }
}

#[cfg(test)]
mod simulation_tests {
    use super::*;

    /// `FlighthookConfig::default()` as JSON (`GET /api/settings`), with no
    /// `[simulation]` key. A default config must not emit one.
    const GOLDEN_DEFAULT_JSON: &str = r#"{"default_units":"imperial","chipping_clubs":["GW","SW","LW"],"putting_clubs":["PT"],"webserver":{"0":{"name":"Web Server","bind":"0.0.0.0:5880"}}}"#;

    #[test]
    fn default_config_json_is_unchanged() {
        let json = serde_json::to_string(&FlighthookConfig::default()).expect("ser");
        assert_eq!(json, GOLDEN_DEFAULT_JSON);
        assert!(!json.contains("simulation"));
    }

    #[test]
    fn shot_grouping_is_omitted_at_default() {
        let c: FlighthookConfig = serde_json::from_str(GOLDEN_DEFAULT_JSON).expect("parses");
        assert_eq!(c.shot_grouping, ShotGrouping::BySet);
        let json = serde_json::to_string(&c).expect("ser");
        assert!(!json.contains("shot_grouping"), "{json}");
    }

    #[test]
    fn shot_grouping_round_trips() {
        for (mode, name) in [
            (ShotGrouping::None, "none"),
            (ShotGrouping::ByClub, "by_club"),
        ] {
            let c = FlighthookConfig {
                shot_grouping: mode,
                ..FlighthookConfig::default()
            };
            let v = serde_json::to_value(&c).expect("ser");
            assert_eq!(v["shot_grouping"], name);
            let back: FlighthookConfig = serde_json::from_value(v).expect("de");
            assert_eq!(back.shot_grouping, mode);
        }
        // The default mode is omitted even when set explicitly.
        let c = FlighthookConfig {
            shot_grouping: ShotGrouping::BySet,
            ..FlighthookConfig::default()
        };
        let json = serde_json::to_string(&c).expect("ser");
        assert!(!json.contains("shot_grouping"), "{json}");
        let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
        assert_eq!(back.shot_grouping, ShotGrouping::BySet);
        // An explicit `none` round-trips.
        let c: FlighthookConfig =
            serde_json::from_str(r#"{"shot_grouping":"none"}"#).expect("parses");
        assert_eq!(c.shot_grouping, ShotGrouping::None);
    }

    #[test]
    fn absent_section_parses_to_defaults() {
        let c: FlighthookConfig = serde_json::from_str(GOLDEN_DEFAULT_JSON).expect("parses");
        assert_eq!(c.simulation, SimulationSection::default());
        assert_eq!(c.simulation.mode, SimMode::On);
        assert_eq!(c.simulation.altitude.as_meters(), 0.0);
        assert_eq!(c.simulation.temperature_c, 25.0);
        assert_eq!(c.simulation.surface, SimSurface::Fairway);
        assert_eq!(c, FlighthookConfig::default());
    }

    #[test]
    fn partial_section_fills_defaults() {
        let s: SimulationSection = serde_json::from_str(r#"{"mode":"off"}"#).expect("parses");
        assert_eq!(s.mode, SimMode::Off);
        assert_eq!(s.temperature_c, 25.0);
        let s: SimulationSection =
            serde_json::from_str(r#"{"altitude":"5280ft","surface":"firm_green"}"#)
                .expect("parses");
        assert_eq!(s.mode, SimMode::On);
        assert!((s.altitude.as_meters() - 1609.344).abs() < 1e-9);
        assert_eq!(s.surface, SimSurface::FirmGreen);
    }

    #[test]
    fn zero_altitude_in_any_unit_is_default() {
        for altitude in [
            Distance::Feet(0.0),
            Distance::Meters(0.0),
            Distance::Yards(0.0),
        ] {
            let s = SimulationSection {
                altitude,
                ..SimulationSection::default()
            };
            assert!(s.is_default(), "{altitude:?}");
        }
        let changed = SimulationSection {
            temperature_c: 30.0,
            ..SimulationSection::default()
        };
        assert!(!changed.is_default());
        let json = serde_json::to_string(&FlighthookConfig {
            simulation: changed,
            ..FlighthookConfig::default()
        })
        .expect("ser");
        assert!(
            json.contains(
                r#""simulation":{"altitude":"0ft","temperature_c":30.0,"surface":"fairway"}"#
            ),
            "{json}"
        );
    }

    #[test]
    fn environment_bounds() {
        let ok = |altitude, temperature_c| {
            SimulationSection {
                altitude,
                temperature_c,
                ..SimulationSection::default()
            }
            .environment_error()
            .is_none()
        };
        assert!(ok(Distance::Meters(0.0), 25.0));
        assert!(ok(Distance::Meters(5000.0), -30.0));
        assert!(ok(Distance::Feet(16_000.0), 50.0));
        assert!(!ok(Distance::Feet(16_500.0), 25.0));
        assert!(!ok(Distance::Meters(-0.1), 25.0));
        assert!(!ok(Distance::Meters(f64::NAN), 25.0));
        assert!(!ok(Distance::Meters(f64::INFINITY), 25.0));
        assert!(!ok(Distance::Meters(0.0), -30.1));
        assert!(!ok(Distance::Meters(0.0), 50.1));
        assert!(!ok(Distance::Meters(0.0), f64::NAN));
    }

    /// A `simulation` object carrying unknown keys (e.g. `enabled`) loads,
    /// ignoring them and keeping the default mode.
    #[test]
    fn unknown_simulation_keys_are_ignored_in_json() {
        let s: SimulationSection =
            serde_json::from_str(r#"{"enabled":false,"unknown":true}"#).expect("parses");
        assert_eq!(s.mode, SimMode::On);
        assert_eq!(s.temperature_c, SimulationSection::DEFAULT_TEMPERATURE_C);
        let wrapped = r#"{"simulation":{"enabled":false,"unknown":true}}"#;
        let c: FlighthookConfig = serde_json::from_str(wrapped).expect("parses");
        assert_eq!(c.simulation.mode, SimMode::On);
    }

    #[test]
    fn mode_round_trips_and_is_omitted_at_default() {
        for mode in SimMode::all() {
            let c = FlighthookConfig {
                simulation: SimulationSection {
                    mode,
                    ..SimulationSection::default()
                },
                ..FlighthookConfig::default()
            };
            assert_eq!(c.simulation.is_default(), mode == SimMode::On);

            let json = serde_json::to_string(&c).expect("ser");
            assert!(!json.contains("enabled"), "{json}");
            if mode == SimMode::On {
                assert!(!json.contains("simulation"), "{json}");
            } else {
                let want = format!(r#""simulation":{{"mode":"{}","#, mode.key());
                assert!(json.contains(&want), "{json}");
            }
            let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
            assert_eq!(back, c);
        }
        // Any other non-default setting writes the section without `mode`.
        let warm = SimulationSection {
            temperature_c: 30.0,
            ..SimulationSection::default()
        };
        let json = serde_json::to_string(&warm).expect("ser");
        assert!(!json.contains("mode"), "{json}");
    }

    #[test]
    fn mode_keys_match_serde() {
        for m in SimMode::all() {
            let json = serde_json::to_string(&m).expect("ser");
            assert_eq!(json, format!("\"{}\"", m.key()));
        }
        assert_eq!(SimMode::default(), SimMode::On);
    }

    #[test]
    fn surface_keys_match_serde() {
        for s in SimSurface::all() {
            let json = serde_json::to_string(&s).expect("ser");
            assert_eq!(json, format!("\"{}\"", s.key()));
        }
    }
}

#[cfg(test)]
mod openconnect_client_tests {
    use super::*;

    #[test]
    fn gspro_sections_read_as_openconnect_client() {
        let c: FlighthookConfig = serde_json::from_str(
            r#"{"gspro":{"1":{"name":"Bay","address":"10.0.0.5:921","full_monitor":"mevo.0"}}}"#,
        )
        .expect("parse");
        let s = &c.openconnect_client["1"];
        assert_eq!(s.name, "Bay");
        assert_eq!(s.address.as_deref(), Some("10.0.0.5:921"));
        assert_eq!(s.full_monitor.as_deref(), Some("mevo.0"));
        assert!(c.has_user_actors());
    }

    #[test]
    fn mixed_sections_merge_and_openconnect_client_wins_a_collision() {
        let c: FlighthookConfig = serde_json::from_str(
            r#"{
                "gspro": {"0": {"name": "old 0"}, "2": {"name": "old 2"}},
                "openconnect_client": {"0": {"name": "new 0"}, "1": {"name": "new 1"}}
            }"#,
        )
        .expect("parse");
        let mut names: Vec<_> = c
            .openconnect_client
            .iter()
            .map(|(i, s)| (i.as_str(), s.name.as_str()))
            .collect();
        names.sort_unstable();
        assert_eq!(names, [("0", "new 0"), ("1", "new 1"), ("2", "old 2")]);
    }

    #[test]
    fn output_only_carries_openconnect_client() {
        let c: FlighthookConfig =
            serde_json::from_str(r#"{"gspro":{"0":{"name":"GSPro"}}}"#).expect("parse");
        let json = serde_json::to_value(&c).expect("serialize");
        assert!(json.get("gspro").is_none(), "{json}");
        assert_eq!(json["openconnect_client"]["0"]["name"], "GSPro");
    }

    /// A config with every field away from its default and every section
    /// map holding an entry. Built field by field without `..`, so a field
    /// added to [`FlighthookConfig`] fails to compile here until it is set.
    fn every_field() -> FlighthookConfig {
        fn one<T>(section: T) -> std::collections::HashMap<String, T> {
            std::collections::HashMap::from([("3".to_string(), section)])
        }
        FlighthookConfig {
            default_units: UnitSystem::Metric,
            chipping_clubs: vec![Club::SandWedge],
            putting_clubs: vec![Club::LobWedge, Club::Putter],
            shot_grouping: ShotGrouping::ByClub,
            simulation: SimulationSection {
                mode: SimMode::Force,
                altitude: Distance::Meters(250.0),
                temperature_c: 12.5,
                surface: SimSurface::FirmGreen,
            },
            webserver: one(WebserverSection {
                name: "Web".into(),
                bind: "0.0.0.0:5881".into(),
            }),
            mevo: one(MevoSection::default()),
            r10: one(R10Section::default()),
            square: one(SquareSection::default()),
            mock_monitor: one(MockMonitorSection {
                name: "Mock".into(),
            }),
            openconnect_server: one(OpenConnectServerSection {
                name: "Uneekor".into(),
                bind: Some("0.0.0.0:921".into()),
            }),
            openconnect_client: one(OpenConnectClientSection {
                name: "Muni Golf".into(),
                address: Some("127.0.0.1:9210".into()),
                full_monitor: Some("openconnect_server.3".into()),
                ..OpenConnectClientSection::default()
            }),
            random_club: one(RandomClubSection {
                name: "Clubs".into(),
            }),
        }
    }

    #[test]
    fn every_field_survives_a_round_trip() {
        let c = every_field();
        assert_ne!(c, FlighthookConfig::default());
        let json = serde_json::to_string(&c).expect("serialize");
        let back: FlighthookConfig = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, c);
    }

    #[test]
    fn gspro_sections_join_every_other_field() {
        let mut json = serde_json::to_value(every_field()).expect("serialize");
        json["gspro"] = serde_json::json!({"4": {"name": "Bay", "address": "10.0.0.5:921"}});
        let back: FlighthookConfig = serde_json::from_value(json).expect("parse");
        let mut want = every_field();
        want.openconnect_client.insert(
            "4".into(),
            OpenConnectClientSection {
                name: "Bay".into(),
                address: Some("10.0.0.5:921".into()),
                full_monitor: None,
                chipping_monitor: None,
                putting_monitor: None,
            },
        );
        assert_eq!(back, want);
    }

    #[test]
    fn upsert_action_accepts_the_gspro_spelling() {
        let a: crate::ConfigAction = serde_json::from_str(
            r#"{"type":"upsert_gs_pro","index":"0","section":{"name":"GSPro"}}"#,
        )
        .expect("parse");
        assert!(matches!(
            a,
            crate::ConfigAction::UpsertOpenConnectClient { ref index, .. } if index == "0"
        ));
        let json = serde_json::to_value(&a).expect("serialize");
        assert_eq!(json["type"], "upsert_open_connect_client");
    }
}
