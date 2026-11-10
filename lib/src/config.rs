use std::fmt;

use flightrelay::config::DriverConfig;
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
///
/// A `golfer_height` in `[body_tracking]` is read as `[player] height` when
/// the player has no height of their own.
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
    /// Swing replay clips from the cameras (beta). Omitted from the file and
    /// the API while it is at its defaults.
    #[serde(default, skip_serializing_if = "ReplaySection::is_default")]
    pub replay: ReplaySection,
    /// Body tracking on the cameras' pictures (alpha). Omitted from the file
    /// and the API while it is at its defaults.
    #[serde(default, skip_serializing_if = "BodyTrackingSection::is_default")]
    pub body_tracking: BodyTrackingSection,
    /// Facts about the player: sex and height. Omitted from the file and
    /// the API while it is at its defaults.
    #[serde(default, skip_serializing_if = "PlayerSection::is_default")]
    pub player: PlayerSection,
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
        let legacy = config.body_tracking.legacy_golfer_height.take();
        config.player.height = config.player.height.or(legacy);
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
    /// Whether the Auto-chip control is offered (the header chip and
    /// `POST /api/auto-chip`). Defaults to `false`; omitted from the file and
    /// the API while at the default. Auto-chip is simulated under the
    /// conditions above whatever `mode` is.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_chip: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
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
            auto_chip: false,
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
            && !self.auto_chip
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

/// Settings for the swing replay clips the cameras record (beta).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplaySection {
    /// Clips kept on disk, newest first; older ones are deleted. Counts
    /// clips, not shots: a swing two launch monitors saw is one clip, and a
    /// trigger that read nothing records one too. Defaults to 20.
    #[serde(default = "default_keep_shots")]
    pub keep_shots: u32,
    /// Seconds recorded before the estimated impact. Defaults to 4.
    #[serde(default = "default_pre_roll_s")]
    pub pre_roll_s: f64,
    /// Seconds recorded after the launch monitor reports the shot. Defaults
    /// to 1.5.
    #[serde(default = "default_post_roll_s")]
    pub post_roll_s: f64,
    /// Seconds before the estimated impact where a replay opens, and where it
    /// restarts once it played to the end. Defaults to 2.
    #[serde(default = "default_start_before_impact_s")]
    pub start_before_impact_s: f64,
    /// Folder the clips are written to. Defaults to `flighthook/clips` in
    /// the user's local data folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_dir: Option<String>,
    /// The Shots tab's Cards view shows camera cards that loop the shown
    /// shot's clip. Off, the card row has no camera cards; clips are still
    /// recorded and replayed. Defaults to true.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub shot_cards: bool,
}

fn default_keep_shots() -> u32 {
    ReplaySection::DEFAULT_KEEP_SHOTS
}

fn default_pre_roll_s() -> f64 {
    ReplaySection::DEFAULT_PRE_ROLL_S
}

fn default_post_roll_s() -> f64 {
    ReplaySection::DEFAULT_POST_ROLL_S
}

fn default_start_before_impact_s() -> f64 {
    ReplaySection::DEFAULT_START_BEFORE_IMPACT_S
}

impl Default for ReplaySection {
    fn default() -> Self {
        Self {
            keep_shots: Self::DEFAULT_KEEP_SHOTS,
            pre_roll_s: Self::DEFAULT_PRE_ROLL_S,
            post_roll_s: Self::DEFAULT_POST_ROLL_S,
            start_before_impact_s: Self::DEFAULT_START_BEFORE_IMPACT_S,
            clip_dir: None,
            shot_cards: true,
        }
    }
}

impl ReplaySection {
    /// Default clips kept.
    pub const DEFAULT_KEEP_SHOTS: u32 = 20;
    /// Most clips kept.
    pub const MAX_KEEP_SHOTS: u32 = 1000;
    /// Default seconds before the estimated impact.
    pub const DEFAULT_PRE_ROLL_S: f64 = 4.0;
    /// Default seconds after the shot is reported.
    pub const DEFAULT_POST_ROLL_S: f64 = 1.5;
    /// Longest pre-roll or post-roll, seconds.
    pub const MAX_ROLL_S: f64 = 10.0;
    /// Default seconds before the estimated impact where a replay starts: a
    /// backswing and downswing take about 1.2 to 1.5 s, plus a buffer.
    pub const DEFAULT_START_BEFORE_IMPACT_S: f64 = 2.0;
    /// Longest start before the impact, seconds.
    pub const MAX_START_BEFORE_IMPACT_S: f64 = 10.0;

    /// True when every setting is at its default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Why a setting is out of range, or `None` when all are valid:
    /// `keep_shots` 1 to [`MAX_KEEP_SHOTS`](Self::MAX_KEEP_SHOTS), each roll
    /// 0 to [`MAX_ROLL_S`](Self::MAX_ROLL_S) seconds, `start_before_impact_s`
    /// 0 to [`MAX_START_BEFORE_IMPACT_S`](Self::MAX_START_BEFORE_IMPACT_S)
    /// seconds.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        if !(1..=Self::MAX_KEEP_SHOTS).contains(&self.keep_shots) {
            return Some(format!(
                "keep_shots {} is outside 1 to {}",
                self.keep_shots,
                Self::MAX_KEEP_SHOTS
            ));
        }
        for (name, v, max) in [
            ("pre_roll_s", self.pre_roll_s, Self::MAX_ROLL_S),
            ("post_roll_s", self.post_roll_s, Self::MAX_ROLL_S),
            (
                "start_before_impact_s",
                self.start_before_impact_s,
                Self::MAX_START_BEFORE_IMPACT_S,
            ),
        ] {
            if !v.is_finite() || !(0.0..=max).contains(&v) {
                return Some(format!("{name} {v} is outside 0 to {max} s"));
            }
        }
        None
    }
}

/// The body tracking pose model (alpha).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoseModel {
    /// RTMPose-m: the more accurate, about twice the work of `-s`.
    #[default]
    #[serde(rename = "rtmpose-m")]
    RtmposeM,
    /// RTMPose-s: lighter and faster.
    #[serde(rename = "rtmpose-s")]
    RtmposeS,
}

impl PoseModel {
    /// Stable key, as the config file holds it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::RtmposeM => "rtmpose-m",
            Self::RtmposeS => "rtmpose-s",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::RtmposeM => "RTMPose-m",
            Self::RtmposeS => "RTMPose-s",
        }
    }

    /// Every model, in display order.
    #[must_use]
    pub fn all() -> [Self; 2] {
        [Self::RtmposeM, Self::RtmposeS]
    }
}

/// When body tracking runs (alpha).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyTrackingMode {
    /// No body tracking: the cameras record and replay clips only. The
    /// default.
    #[default]
    Off,
    /// A clip is tracked the first time it is viewed, and the result kept
    /// beside it. No live tracking.
    Historical,
    /// The live views are tracked while watched, and every clip right
    /// after it is recorded.
    Live,
}

impl BodyTrackingMode {
    /// True for the default mode ([`BodyTrackingMode::Off`]).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Stable key, as the config file holds it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Historical => "historical",
            Self::Live => "live",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Historical => "Historical",
            Self::Live => "Live",
        }
    }

    /// Every mode, in display order.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Off, Self::Historical, Self::Live]
    }

    /// Whether body tracking runs at all (not [`BodyTrackingMode::Off`]).
    #[must_use]
    pub fn is_on(self) -> bool {
        self != Self::Off
    }
}

/// Body tracking on the cameras' pictures (alpha): a skeleton drawn over
/// the live views and over the shots' clips, and the swing analysis of
/// each clip.
///
/// Every field is read live; a change never restarts a camera. A field at
/// its default is left out of the file and the API. A section with
/// `enabled` and no `mode` reads `enabled = false` as
/// [`BodyTrackingMode::Off`] and `enabled = true` as
/// [`BodyTrackingMode::Historical`]; only `mode` is written. The golfer's
/// height is [`PlayerSection::height`]; a `golfer_height` in the section is
/// read and moved there when the config loads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "BodyTrackingRaw")]
pub struct BodyTrackingSection {
    /// When body tracking runs. Defaults to [`BodyTrackingMode::Off`].
    #[serde(skip_serializing_if = "BodyTrackingMode::is_default")]
    pub mode: BodyTrackingMode,
    /// The pose model. Defaults to RTMPose-m.
    #[serde(default, skip_serializing_if = "is_default_pose_model")]
    pub model: PoseModel,
    /// Keypoints scoring below this are not drawn, 0 to 1. Defaults to 0.3.
    #[serde(
        default = "default_min_score",
        skip_serializing_if = "is_default_min_score"
    )]
    pub min_score: f64,
    /// Live smoothing: the One Euro filter's cutoff at rest in Hz, 0 (off)
    /// or [`MIN_SMOOTHING_HZ`](Self::MIN_SMOOTHING_HZ) to
    /// [`MAX_SMOOTHING_HZ`](Self::MAX_SMOOTHING_HZ). Lower is smoother and
    /// lags more. Defaults to 1 Hz. Live views only; recorded swings keep the
    /// model's raw keypoints.
    #[serde(
        default = "default_smoothing",
        skip_serializing_if = "is_default_smoothing"
    )]
    pub smoothing: f64,
    /// The live view looks for the golfer afresh at least every this many
    /// frames, 1 to [`MAX_DETECT_EVERY`](Self::MAX_DETECT_EVERY) (and
    /// whenever it loses them); in between it follows them from the last
    /// frame. Defaults to 10.
    #[serde(
        default = "default_detect_every",
        skip_serializing_if = "is_default_detect_every"
    )]
    pub detect_every: u32,
    /// The skeleton is drawn over the live views, the shots' clips and the
    /// replay. Defaults to true.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub show_skeleton: bool,
    /// A `golfer_height` read from the section, in centimeters, until the
    /// config moves it to [`PlayerSection::height`]. Never written, never
    /// compared.
    #[serde(skip)]
    #[doc(hidden)]
    pub legacy_golfer_height: Option<f64>,
}

impl PartialEq for BodyTrackingSection {
    fn eq(&self, other: &Self) -> bool {
        self.mode == other.mode
            && self.model == other.model
            && self.min_score == other.min_score
            && self.smoothing == other.smoothing
            && self.detect_every == other.detect_every
            && self.show_skeleton == other.show_skeleton
    }
}

/// `[body_tracking]` as written: `mode`, or the `enabled` switch of a
/// section from before the modes.
#[derive(Deserialize)]
struct BodyTrackingRaw {
    #[serde(default)]
    mode: Option<BodyTrackingMode>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    model: PoseModel,
    #[serde(default = "default_min_score")]
    min_score: f64,
    #[serde(default = "default_smoothing")]
    smoothing: f64,
    #[serde(default = "default_detect_every")]
    detect_every: u32,
    #[serde(default)]
    golfer_height: Option<f64>,
    #[serde(default = "default_true")]
    show_skeleton: bool,
}

impl From<BodyTrackingRaw> for BodyTrackingSection {
    fn from(raw: BodyTrackingRaw) -> Self {
        let mode = raw.mode.unwrap_or(match raw.enabled {
            Some(true) => BodyTrackingMode::Historical,
            Some(false) | None => BodyTrackingMode::Off,
        });
        Self {
            mode,
            model: raw.model,
            min_score: raw.min_score,
            smoothing: raw.smoothing,
            detect_every: raw.detect_every,
            show_skeleton: raw.show_skeleton,
            legacy_golfer_height: raw.golfer_height,
        }
    }
}

fn is_default_pose_model(v: &PoseModel) -> bool {
    *v == PoseModel::default()
}

fn default_min_score() -> f64 {
    BodyTrackingSection::DEFAULT_MIN_SCORE
}

fn is_default_min_score(v: &f64) -> bool {
    v.to_bits() == BodyTrackingSection::DEFAULT_MIN_SCORE.to_bits()
}

fn default_smoothing() -> f64 {
    BodyTrackingSection::DEFAULT_SMOOTHING_HZ
}

fn is_default_smoothing(v: &f64) -> bool {
    v.to_bits() == BodyTrackingSection::DEFAULT_SMOOTHING_HZ.to_bits()
}

fn default_detect_every() -> u32 {
    BodyTrackingSection::DEFAULT_DETECT_EVERY
}

fn is_default_detect_every(v: &u32) -> bool {
    *v == BodyTrackingSection::DEFAULT_DETECT_EVERY
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

impl Default for BodyTrackingSection {
    fn default() -> Self {
        Self {
            mode: BodyTrackingMode::default(),
            model: PoseModel::default(),
            min_score: Self::DEFAULT_MIN_SCORE,
            smoothing: Self::DEFAULT_SMOOTHING_HZ,
            detect_every: Self::DEFAULT_DETECT_EVERY,
            show_skeleton: true,
            legacy_golfer_height: None,
        }
    }
}

impl BodyTrackingSection {
    /// Default keypoint drawing threshold.
    pub const DEFAULT_MIN_SCORE: f64 = 0.3;
    /// Default live smoothing, Hz.
    pub const DEFAULT_SMOOTHING_HZ: f64 = 1.0;
    /// Lowest live smoothing cutoff other than off, Hz: the smoothest.
    pub const MIN_SMOOTHING_HZ: f64 = 0.1;
    /// Highest live smoothing cutoff, Hz: the least smoothing.
    pub const MAX_SMOOTHING_HZ: f64 = 10.0;
    /// Default frames between two looks for the golfer.
    pub const DEFAULT_DETECT_EVERY: u32 = 10;
    /// Most frames between two looks for the golfer.
    pub const MAX_DETECT_EVERY: u32 = 60;

    /// True when every setting is at its default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Why a setting is out of range, or `None` when all are valid:
    /// `min_score` 0 to 1, `smoothing` 0 or
    /// [`MIN_SMOOTHING_HZ`](Self::MIN_SMOOTHING_HZ) to
    /// [`MAX_SMOOTHING_HZ`](Self::MAX_SMOOTHING_HZ), `detect_every` 1 to
    /// [`MAX_DETECT_EVERY`](Self::MAX_DETECT_EVERY).
    #[must_use]
    pub fn error(&self) -> Option<String> {
        if !self.min_score.is_finite() || !(0.0..=1.0).contains(&self.min_score) {
            return Some(format!("min_score {} is outside 0 to 1", self.min_score));
        }
        if !smoothing_valid(self.smoothing) {
            return Some(format!(
                "smoothing {} is neither 0 (off) nor {} to {} Hz",
                self.smoothing,
                Self::MIN_SMOOTHING_HZ,
                Self::MAX_SMOOTHING_HZ
            ));
        }
        if !(1..=Self::MAX_DETECT_EVERY).contains(&self.detect_every) {
            return Some(format!(
                "detect_every {} is outside 1 to {}",
                self.detect_every,
                Self::MAX_DETECT_EVERY
            ));
        }
        None
    }

    /// The settings in effect: every value out of range replaced by its
    /// default.
    #[must_use]
    pub fn in_effect(&self) -> Self {
        let defaults = Self::default();
        Self {
            mode: self.mode,
            model: self.model,
            min_score: if self.min_score.is_finite() && (0.0..=1.0).contains(&self.min_score) {
                self.min_score
            } else {
                defaults.min_score
            },
            smoothing: if smoothing_valid(self.smoothing) {
                self.smoothing
            } else {
                defaults.smoothing
            },
            detect_every: if (1..=Self::MAX_DETECT_EVERY).contains(&self.detect_every) {
                self.detect_every
            } else {
                defaults.detect_every
            },
            show_skeleton: self.show_skeleton,
            legacy_golfer_height: None,
        }
    }
}

/// Whether `hz` is an acceptable live smoothing: 0 (off), or finite and
/// from [`BodyTrackingSection::MIN_SMOOTHING_HZ`] to
/// [`BodyTrackingSection::MAX_SMOOTHING_HZ`].
fn smoothing_valid(hz: f64) -> bool {
    hz == 0.0
        || (hz.is_finite()
            && (BodyTrackingSection::MIN_SMOOTHING_HZ..=BodyTrackingSection::MAX_SMOOTHING_HZ)
                .contains(&hz))
}

/// Where a camera stands, as seen from the golfer's stance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraPosition {
    /// Looking down the target line from behind the golfer's hands.
    TargetLine,
    /// Facing the golfer's chest. The default.
    #[default]
    GolferFront,
    /// Looking at the golfer's back.
    GolferBack,
    /// Above the golfer, looking down.
    Overhead,
}

impl CameraPosition {
    /// Stable key used by the settings UI and config file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::TargetLine => "target_line",
            Self::GolferFront => "golfer_front",
            Self::GolferBack => "golfer_back",
            Self::Overhead => "overhead",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::TargetLine => "Target Line",
            Self::GolferFront => "Golfer Front",
            Self::GolferBack => "Golfer Back",
            Self::Overhead => "Overhead",
        }
    }

    /// Every position, in display order.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [
            Self::TargetLine,
            Self::GolferFront,
            Self::GolferBack,
            Self::Overhead,
        ]
    }
}

/// A swing camera instance (beta): records a clip around every shot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The camera's stable id, from the camera list (`GET /api/cameras`):
    /// it survives replugging into the same USB port and reboots.
    /// [`CameraSection::TEST_PATTERN`] is a generated test picture.
    #[serde(default)]
    pub camera: String,
    /// The camera's own name when it was picked, for display, and to find
    /// the camera when its id is not connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_name: Option<String>,
    /// Where the camera stands.
    #[serde(default)]
    pub position: CameraPosition,
    /// Video mode: `auto` (the highest frame rate, then the largest size)
    /// or `WIDTHxHEIGHT@FPS`, e.g. `1280x720@120`.
    #[serde(default = "default_video_mode")]
    pub mode: String,
    /// Seconds a frame is old when it arrives, 0 to [`MAX_LATENCY_S`]: a
    /// camera that delivers late, such as a buffered network stream. Every
    /// frame is dated that much earlier than its arrival. Absent is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_s: Option<f64>,
    /// Quarter turns applied to the camera's frames before body tracking
    /// and display: 0, 90, 180 or 270 degrees clockwise. Clips are stored as
    /// the camera sent them. Defaults to 0.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub rotation_deg: u16,
    /// How far the camera points off square, degrees, -[`MAX_CAMERA_ANGLE_DEG`]
    /// to [`MAX_CAMERA_ANGLE_DEG`]: positive when it points to the right of
    /// the target line for a right-handed golfer. Absent is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw_deg: Option<f64>,
    /// The camera's tilt down from level, degrees, -[`MAX_CAMERA_ANGLE_DEG`]
    /// to [`MAX_CAMERA_ANGLE_DEG`]. Absent is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch_deg: Option<f64>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

fn default_video_mode() -> String {
    CameraSection::AUTO_MODE.to_owned()
}

impl CameraSection {
    /// The [`camera`](Self::camera) id of the generated test picture.
    pub const TEST_PATTERN: &'static str = "test-pattern";
    /// The automatic [`mode`](Self::mode).
    pub const AUTO_MODE: &'static str = "auto";
}

impl Default for CameraSection {
    fn default() -> Self {
        Self {
            name: "Camera".into(),
            enabled: true,
            camera: String::new(),
            camera_name: None,
            position: CameraPosition::default(),
            mode: default_video_mode(),
            latency_s: None,
            rotation_deg: 0,
            yaw_deg: None,
            pitch_deg: None,
        }
    }
}

/// Longest camera `latency_s`, seconds.
pub const MAX_LATENCY_S: f64 = 10.0;

/// Whether `v`, a camera's `latency_s`, is acceptable: absent, or finite and
/// from 0 to [`MAX_LATENCY_S`].
#[must_use]
pub fn latency_valid(v: Option<f64>) -> bool {
    v.is_none_or(|v| v.is_finite() && (0.0..=MAX_LATENCY_S).contains(&v))
}

/// The latency in effect for a camera, seconds: its `configured` value when
/// it is valid, else 0.
#[must_use]
pub fn latency_in_effect_s(configured: Option<f64>) -> f64 {
    configured
        .filter(|v| latency_valid(Some(*v)))
        .unwrap_or(0.0)
}

/// Why `latency_s` of camera `id` (e.g. `camera.0`) is not acceptable, or
/// `None` when it is ([`latency_valid`]).
#[must_use]
pub fn latency_error(id: &str, v: Option<f64>) -> Option<String> {
    if latency_valid(v) {
        return None;
    }
    v.map(|v| format!("{id}: latency_s {v} is outside 0 to {MAX_LATENCY_S} s"))
}

/// The camera rotations accepted, clockwise degrees.
pub const CAMERA_ROTATIONS: [u16; 4] = [0, 90, 180, 270];

/// Largest camera `yaw_deg` or `pitch_deg` either way, degrees.
pub const MAX_CAMERA_ANGLE_DEG: f64 = 30.0;

/// Whether `v`, a camera's `rotation_deg`, is acceptable: 0, 90, 180 or 270.
#[must_use]
pub fn rotation_valid(v: u16) -> bool {
    CAMERA_ROTATIONS.contains(&v)
}

/// The rotation in effect for a camera, clockwise degrees: its `configured`
/// value when it is valid, else 0.
#[must_use]
pub fn rotation_in_effect_deg(configured: u16) -> u16 {
    if rotation_valid(configured) {
        configured
    } else {
        0
    }
}

/// Why `rotation_deg` of camera `id` (e.g. `camera.0`) is not acceptable, or
/// `None` when it is ([`rotation_valid`]).
#[must_use]
pub fn rotation_error(id: &str, v: u16) -> Option<String> {
    (!rotation_valid(v)).then(|| format!("{id}: rotation_deg {v} is not 0, 90, 180 or 270"))
}

/// Whether `v`, a camera's `yaw_deg` or `pitch_deg`, is acceptable: absent,
/// or finite and from -[`MAX_CAMERA_ANGLE_DEG`] to [`MAX_CAMERA_ANGLE_DEG`].
#[must_use]
pub fn camera_angle_valid(v: Option<f64>) -> bool {
    v.is_none_or(|v| v.is_finite() && (-MAX_CAMERA_ANGLE_DEG..=MAX_CAMERA_ANGLE_DEG).contains(&v))
}

/// The angle in effect for a camera's `yaw_deg` or `pitch_deg`, degrees: its
/// `configured` value when it is valid, else 0.
#[must_use]
pub fn camera_angle_in_effect_deg(configured: Option<f64>) -> f64 {
    configured
        .filter(|v| camera_angle_valid(Some(*v)))
        .unwrap_or(0.0)
}

/// Why `yaw_deg` of camera `id` is not acceptable, or `None` when it is
/// ([`camera_angle_valid`]).
#[must_use]
pub fn yaw_error(id: &str, v: Option<f64>) -> Option<String> {
    angle_error(id, "yaw_deg", v)
}

/// Why `pitch_deg` of camera `id` is not acceptable, or `None` when it is
/// ([`camera_angle_valid`]).
#[must_use]
pub fn pitch_error(id: &str, v: Option<f64>) -> Option<String> {
    angle_error(id, "pitch_deg", v)
}

fn angle_error(id: &str, field: &str, v: Option<f64>) -> Option<String> {
    if camera_angle_valid(v) {
        return None;
    }
    v.map(|v| {
        format!("{id}: {field} {v} is outside -{MAX_CAMERA_ANGLE_DEG} to {MAX_CAMERA_ANGLE_DEG}")
    })
}

impl CameraSection {
    /// Why a setting of this section, camera `id` (e.g. `camera.0`), is out
    /// of range, or `None` when all are valid: the first of `latency_s`
    /// ([`latency_error`]), `rotation_deg` ([`rotation_error`]), `yaw_deg`
    /// ([`yaw_error`]) and `pitch_deg` ([`pitch_error`]).
    #[must_use]
    pub fn error(&self, id: &str) -> Option<String> {
        latency_error(id, self.latency_s)
            .or_else(|| rotation_error(id, self.rotation_deg))
            .or_else(|| yaw_error(id, self.yaw_deg))
            .or_else(|| pitch_error(id, self.pitch_deg))
    }
}

/// The player's sex: picks the men's or the women's tour averages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sex {
    /// Men's tour averages. The default.
    #[default]
    Male,
    /// Women's tour averages.
    Female,
}

impl Sex {
    /// True for the default ([`Sex::Male`]).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Stable key, as the config file holds it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Male => "male",
            Self::Female => "female",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Male => "Men's",
            Self::Female => "Women's",
        }
    }

    /// Both, in display order.
    #[must_use]
    pub fn all() -> [Self; 2] {
        [Self::Male, Self::Female]
    }
}

/// Facts about the player that the swing analysis uses. Every field is
/// read live. A field at its default is left out of the file and the API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerSection {
    /// Whose tour averages the player is compared with. Defaults to
    /// [`Sex::Male`].
    #[serde(default, skip_serializing_if = "Sex::is_default")]
    pub sex: Sex,
    /// The player's height in centimeters,
    /// [`MIN_HEIGHT_CM`](Self::MIN_HEIGHT_CM) to
    /// [`MAX_HEIGHT_CM`](Self::MAX_HEIGHT_CM). With it, swing metrics that
    /// are distances are also given in centimeters (inches in imperial
    /// units); without it they stay fractions of the golfer's torso length.
    /// Not set by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
}

impl PlayerSection {
    /// Shortest height accepted, cm.
    pub const MIN_HEIGHT_CM: f64 = 100.0;
    /// Tallest height accepted, cm.
    pub const MAX_HEIGHT_CM: f64 = 250.0;

    /// True when every setting is at its default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Why a setting is out of range, or `None` when all are valid:
    /// `height` (when set) [`MIN_HEIGHT_CM`](Self::MIN_HEIGHT_CM) to
    /// [`MAX_HEIGHT_CM`](Self::MAX_HEIGHT_CM).
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.height.filter(|h| !height_valid(*h)).map(|h| {
            format!(
                "height {h} is outside {} to {} cm",
                Self::MIN_HEIGHT_CM,
                Self::MAX_HEIGHT_CM
            )
        })
    }

    /// The settings in effect: a height out of range is no height.
    #[must_use]
    pub fn in_effect(&self) -> Self {
        Self {
            sex: self.sex,
            height: self.height.filter(|h| height_valid(*h)),
        }
    }
}

/// Whether `cm` is an acceptable player height: finite and from
/// [`PlayerSection::MIN_HEIGHT_CM`] to [`PlayerSection::MAX_HEIGHT_CM`].
fn height_valid(cm: f64) -> bool {
    cm.is_finite() && (PlayerSection::MIN_HEIGHT_CM..=PlayerSection::MAX_HEIGHT_CM).contains(&cm)
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
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
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
    /// Backspin to use, in rpm per mph of ball speed, for a shot whose result
    /// has launch data but no spin and no flight (very short chips on the
    /// Xi). Sidespin is 0. `0` turns it off: such a shot keeps its 0 carry,
    /// apex and flight time and is rolled as a ground ball. Not used in
    /// Putting mode. Defaults to [`MevoSection::DEFAULT_LAUNCH_ONLY_SPIN`],
    /// and is left out of the config file at the default. A value outside
    /// 0 to [`MevoSection::MAX_LAUNCH_ONLY_SPIN`] (or non-finite) is treated
    /// as `0`.
    #[serde(
        default = "default_launch_only_spin",
        skip_serializing_if = "is_default_launch_only_spin"
    )]
    pub launch_only_spin: f64,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
}

fn default_launch_only_spin() -> f64 {
    MevoSection::DEFAULT_LAUNCH_ONLY_SPIN
}

fn is_default_launch_only_spin(v: &f64) -> bool {
    *v == MevoSection::DEFAULT_LAUNCH_ONLY_SPIN
}

impl MevoSection {
    /// Default `launch_only_spin`, rpm per mph: about 1500 rpm for a 15 mph
    /// chip.
    pub const DEFAULT_LAUNCH_ONLY_SPIN: f64 = 100.0;

    /// Highest acceptable `launch_only_spin`, rpm per mph.
    pub const MAX_LAUNCH_ONLY_SPIN: f64 = 1000.0;

    /// Whether `v` is an acceptable `launch_only_spin`: finite, from 0 to
    /// [`MevoSection::MAX_LAUNCH_ONLY_SPIN`].
    #[must_use]
    pub fn launch_only_spin_valid(v: f64) -> bool {
        v.is_finite() && (0.0..=Self::MAX_LAUNCH_ONLY_SPIN).contains(&v)
    }

    /// The `launch_only_spin` in effect, rpm per mph: the configured value
    /// when it is valid ([`MevoSection::launch_only_spin_valid`]), else `0`
    /// (off).
    #[must_use]
    pub fn launch_only_spin_rpm_per_mph(&self) -> f64 {
        if Self::launch_only_spin_valid(self.launch_only_spin) {
            self.launch_only_spin
        } else {
            0.0
        }
    }
}

/// A Garmin R10 BLE device instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct R10Section {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Distance from the device to the tee, sent to the R10 as its tee
    /// distance once it wakes up. Garmin recommends placing the R10 6-8 ft
    /// behind the ball.
    ///
    /// When absent, no tee distance is sent and the device keeps whatever tee
    /// distance was last set on it (e.g. by the Garmin Golf app).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Distance>,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
}

/// A Square Golf Omni BLE device instance.
///
/// The original Square / Square Home is not supported: it uses a different
/// club-code scheme.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SquareSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
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
    /// Use the device's advanced spin measurement. Defaults to true.
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
    /// edge of the club sticker's dot down to face center, in millimeters.
    /// Measure with the sticker in its normal spot.
    ///
    /// Keyed the same way `club` is (e.g. `"DR"`, `"7i"`, `"pw"`). Clubs not
    /// listed use allsquare's built-in defaults; the putter has no built-in
    /// default and reports no vertical impact unless given one here.
    #[serde(
        default,
        skip_serializing_if = "is_none_or_empty_map",
        alias = "dot_bottom_to_face_centre_mm"
    )]
    pub dot_bottom_to_face_center_mm: Option<std::collections::BTreeMap<String, f64>>,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
}

/// An ExPutt RG putting device instance, reached over WiFi. The device must
/// be in its mobile mode on the same network.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExPuttSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// IP address of the device. When absent, the device is found on the
    /// local network automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
}

fn is_none_or_empty_map(m: &Option<std::collections::BTreeMap<String, f64>>) -> bool {
    m.as_ref().is_none_or(std::collections::BTreeMap::is_empty)
}

/// The config section of a card whose device runs on a flightrelay device
/// driver: flighthook's own fields (`name`, `enabled`, `impact_lead_s`, ...)
/// with the driver's settings flattened next to them, so the file and the
/// REST JSON hold one flat table per card. No driver key has the name of a
/// host field.
pub trait DeviceSection:
    Serialize + serde::de::DeserializeOwned + Clone + PartialEq + fmt::Debug + Send + Sync + 'static
{
    /// The driver's settings.
    type Driver: DriverConfig;

    /// The section type's key in the config, e.g. `mock_monitor`.
    const KIND: &'static str;

    /// The driver's settings in this section.
    fn driver(&self) -> &Self::Driver;

    /// The sections of this type in `config`, by index.
    fn sections(config: &FlighthookConfig) -> &std::collections::HashMap<String, Self>;

    /// Why this section's driver settings are not usable, as
    /// `"<id>: <field>: <reason>"` for the section with ID `id` (e.g.
    /// `mock_monitor.0`), or `None` when the driver accepts them
    /// ([`DriverConfig::validate`]).
    #[must_use]
    fn driver_error(&self, id: &str) -> Option<String> {
        self.driver().validate().err().map(|e| format!("{id}: {e}"))
    }
}

/// A mock launch monitor instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockMonitorSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
}

/// An Open Connect server instance — a *launch monitor*, not an integration.
///
/// Accepts inbound shot data from monitors that speak GSPro Open Connect V1 as
/// a client (Uneekor, Foresight, SkyTrak, MLM2PRO, …). This is the inverse of
/// [`OpenConnectClientSection`], which dials a simulator as a client.
///
/// GSPro listens on 921 as well, but its port is movable: set
/// `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
/// `C:\GSPro\GSPC\GSPconnect.exe.config` to move GSPConnect to 922 and free
/// 921 for this actor, so both can share one host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenConnectServerSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Bind address. Defaults to `0.0.0.0:921`.
    pub bind: Option<String>,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
}

/// An Open Connect client instance — an *output*.
///
/// Dials a simulator that accepts GSPro Open Connect V1 (GSPro and Pureplay
/// on port 921, Muni Golf on 9210) and sends it every shot. This is the inverse of
/// [`OpenConnectServerSection`], which accepts shots from a launch monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenConnectClientSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
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

/// A Pureplay output instance (alpha).
///
/// Dials Pureplay (port 9760) and sends it every routed shot over the
/// Pureplay Connect protocol, taking its club, hand and strict-mode
/// commands. Routing works as for [`OpenConnectClientSection`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PureplaySection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Pureplay's address. Defaults to `127.0.0.1:9760`.
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
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

/// Seconds before the report of a shot that a clip assumes the swing's impact
/// happened, for a launch monitor without a better-known value. Beta.
pub const DEFAULT_IMPACT_LEAD_S: f64 = 1.5;

/// Longest `impact_lead_s`, seconds.
pub const MAX_IMPACT_LEAD_S: f64 = 5.0;

/// The default impact lead of the launch monitor type `prefix` (the part of
/// an actor ID before the dot, e.g. `mevo`), seconds: how long before the
/// monitor's `shot_trigger` the ball was struck. `None` for Auto-chip, which
/// is not a swing. Any other source is a launch monitor and gets
/// [`DEFAULT_IMPACT_LEAD_S`] unless a value is better known.
#[must_use]
pub fn impact_lead_default_s(prefix: &str) -> Option<f64> {
    match prefix {
        crate::api::AUTO_CHIP_ACTOR => None,
        // Reports at impact.
        "mevo" | "mock_monitor" => Some(0.0),
        "square" => Some(1.0),
        "r10" => Some(2.0),
        "openconnect_server" => Some(2.5),
        _ => Some(DEFAULT_IMPACT_LEAD_S),
    }
}

/// Whether `v`, a section's `impact_lead_s`, is acceptable: absent, or finite
/// and from 0 to [`MAX_IMPACT_LEAD_S`].
#[must_use]
pub fn impact_lead_valid(v: Option<f64>) -> bool {
    v.is_none_or(|v| v.is_finite() && (0.0..=MAX_IMPACT_LEAD_S).contains(&v))
}

/// The impact lead in effect for launch monitor type `prefix`, seconds: the
/// section's `configured` value when it is valid, else the type's default
/// ([`impact_lead_default_s`]). `None` for a source that is not a swing.
#[must_use]
pub fn impact_lead_in_effect_s(prefix: &str, configured: Option<f64>) -> Option<f64> {
    let default = impact_lead_default_s(prefix)?;
    Some(
        configured
            .filter(|v| impact_lead_valid(Some(*v)))
            .unwrap_or(default),
    )
}

/// Why `impact_lead_s` of section `id` (e.g. `mevo.0`) is not acceptable, or
/// `None` when it is ([`impact_lead_valid`]).
#[must_use]
pub fn impact_lead_error(id: &str, v: Option<f64>) -> Option<String> {
    if impact_lead_valid(v) {
        return None;
    }
    v.map(|v| format!("{id}: impact_lead_s {v} is outside 0 to {MAX_IMPACT_LEAD_S} s"))
}

impl FlighthookConfig {
    /// Every launch monitor section as (type, index, `impact_lead_s`), types
    /// in the order of the sections' fields and indices sorted.
    #[must_use]
    pub fn launch_monitor_leads(&self) -> Vec<(&'static str, &str, Option<f64>)> {
        fn rows<'a, T>(
            out: &mut Vec<(&'static str, &'a str, Option<f64>)>,
            kind: &'static str,
            map: &'a std::collections::HashMap<String, T>,
            lead: impl Fn(&T) -> Option<f64>,
        ) {
            let mut rows: Vec<_> = map
                .iter()
                .map(|(i, s)| (kind, i.as_str(), lead(s)))
                .collect();
            rows.sort_by(|a, b| a.1.cmp(b.1));
            out.extend(rows);
        }
        let mut out = Vec::new();
        rows(&mut out, "mevo", &self.mevo, |s| s.impact_lead_s);
        rows(&mut out, "r10", &self.r10, |s| s.impact_lead_s);
        rows(&mut out, "square", &self.square, |s| s.impact_lead_s);
        rows(&mut out, "exputt", &self.exputt, |s| s.impact_lead_s);
        rows(&mut out, "mock_monitor", &self.mock_monitor, |s| {
            s.impact_lead_s
        });
        rows(
            &mut out,
            "openconnect_server",
            &self.openconnect_server,
            |s| s.impact_lead_s,
        );
        out
    }

    /// The actor IDs (e.g. `mevo.0`) of every section that is off
    /// (`enabled = false`), sorted. Such a section keeps its card and its
    /// settings and runs no actor.
    #[must_use]
    pub fn disabled_actors(&self) -> Vec<String> {
        fn off<T>(
            out: &mut Vec<String>,
            kind: &str,
            map: &std::collections::HashMap<String, T>,
            enabled: impl Fn(&T) -> bool,
        ) {
            out.extend(
                map.iter()
                    .filter(|(_, s)| !enabled(s))
                    .map(|(i, _)| format!("{kind}.{i}")),
            );
        }
        let mut out = Vec::new();
        off(&mut out, "mevo", &self.mevo, |s| s.enabled);
        off(&mut out, "r10", &self.r10, |s| s.enabled);
        off(&mut out, "square", &self.square, |s| s.enabled);
        off(&mut out, "exputt", &self.exputt, |s| s.enabled);
        off(&mut out, "mock_monitor", &self.mock_monitor, |s| s.enabled);
        off(
            &mut out,
            "openconnect_server",
            &self.openconnect_server,
            |s| s.enabled,
        );
        off(
            &mut out,
            "openconnect_client",
            &self.openconnect_client,
            |s| s.enabled,
        );
        off(&mut out, "pureplay", &self.pureplay, |s| s.enabled);
        off(&mut out, "random_club", &self.random_club, |s| s.enabled);
        off(&mut out, "camera", &self.camera, |s| s.enabled);
        out.sort();
        out
    }

    /// Whether actor `actor_id` (e.g. `mevo.0`) may run: false only for a
    /// configured section that is off.
    #[must_use]
    pub fn actor_enabled(&self, actor_id: &str) -> bool {
        !self.disabled_actors().iter().any(|id| id == actor_id)
    }

    /// The `impact_lead_s` configured on launch monitor `actor_id` (e.g.
    /// `mevo.0`), or `None` when it is absent or there is no such section.
    #[must_use]
    pub fn configured_impact_lead_s(&self, actor_id: &str) -> Option<f64> {
        let (kind, index) = actor_id.split_once('.')?;
        self.launch_monitor_leads()
            .into_iter()
            .find(|(k, i, _)| *k == kind && *i == index)
            .and_then(|(_, _, lead)| lead)
    }

    /// The first camera section, by index, with a setting out of range
    /// ([`CameraSection::error`]), as a message.
    #[must_use]
    pub fn camera_error(&self) -> Option<String> {
        let mut cameras: Vec<_> = self.camera.iter().collect();
        cameras.sort_by(|a, b| a.0.cmp(b.0));
        cameras
            .into_iter()
            .find_map(|(index, s)| s.error(&format!("camera.{index}")))
    }

    /// The first camera section whose `latency_s` is out of range, as a
    /// message.
    #[must_use]
    pub fn latency_error(&self) -> Option<String> {
        let mut cameras: Vec<_> = self.camera.iter().collect();
        cameras.sort_by(|a, b| a.0.cmp(b.0));
        cameras
            .into_iter()
            .find_map(|(index, s)| latency_error(&format!("camera.{index}"), s.latency_s))
    }

    /// The first launch monitor section whose `impact_lead_s` is out of
    /// range, as a message.
    #[must_use]
    pub fn impact_lead_error(&self) -> Option<String> {
        self.launch_monitor_leads()
            .into_iter()
            .find_map(|(kind, index, lead)| impact_lead_error(&format!("{kind}.{index}"), lead))
    }

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
            || !self.exputt.is_empty()
            || !self.mock_monitor.is_empty()
            || !self.openconnect_server.is_empty()
            || !self.openconnect_client.is_empty()
            || !self.pureplay.is_empty()
            || !self.random_club.is_empty()
            || !self.camera.is_empty()
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
            replay: ReplaySection::default(),
            body_tracking: BodyTrackingSection::default(),
            player: PlayerSection::default(),
            webserver,
            mevo: std::collections::HashMap::new(),
            r10: std::collections::HashMap::new(),
            square: std::collections::HashMap::new(),
            exputt: std::collections::HashMap::new(),
            mock_monitor: std::collections::HashMap::new(),
            openconnect_server: std::collections::HashMap::new(),
            openconnect_client: std::collections::HashMap::new(),
            pureplay: std::collections::HashMap::new(),
            random_club: std::collections::HashMap::new(),
            camera: std::collections::HashMap::new(),
        }
    }
}

impl Default for MevoSection {
    fn default() -> Self {
        Self {
            name: "Mevo WiFi".into(),
            enabled: true,
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
            launch_only_spin: Self::DEFAULT_LAUNCH_ONLY_SPIN,
            impact_lead_s: None,
        }
    }
}

impl Default for R10Section {
    fn default() -> Self {
        Self {
            name: "Garmin R10".into(),
            enabled: true,
            // Absent: leave the device's own tee distance untouched.
            range: None,
            impact_lead_s: None,
        }
    }
}

impl Default for SquareSection {
    fn default() -> Self {
        Self {
            name: "Square Golf Omni".into(),
            enabled: true,
            // Auto-discover by name prefix.
            address: None,
            club: None,
            advanced_spin: None,
            discard_non_putting_zero_spin: Some(true),
            dot_bottom_to_face_center_mm: None,
            impact_lead_s: None,
        }
    }
}

impl Default for ExPuttSection {
    fn default() -> Self {
        Self {
            name: "ExPutt".into(),
            enabled: true,
            // Found on the network automatically.
            address: None,
            impact_lead_s: None,
        }
    }
}

impl Default for OpenConnectServerSection {
    fn default() -> Self {
        Self {
            name: "Open Connect Server".into(),
            enabled: true,
            bind: Some("0.0.0.0:921".into()),
            impact_lead_s: None,
        }
    }
}

impl Default for OpenConnectClientSection {
    fn default() -> Self {
        Self {
            name: "GSPro".into(),
            enabled: true,
            address: Some("127.0.0.1:921".into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
        }
    }
}

impl Default for PureplaySection {
    fn default() -> Self {
        Self {
            name: "Pureplay".into(),
            enabled: true,
            address: Some("127.0.0.1:9760".into()),
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
mod launch_only_spin_tests {
    use super::*;

    #[test]
    fn absent_launch_only_spin_defaults_to_100() {
        let s: MevoSection = serde_json::from_str(r#"{"name":"Mevo"}"#).expect("parse");
        assert_eq!(s.launch_only_spin, 100.0);
        assert_eq!(s.launch_only_spin_rpm_per_mph(), 100.0);
        assert_eq!(MevoSection::default().launch_only_spin, 100.0);
    }

    #[test]
    fn default_launch_only_spin_is_not_written_back() {
        let json = serde_json::to_string(&MevoSection::default()).expect("serialize");
        assert!(!json.contains("launch_only_spin"), "{json}");
        let s: MevoSection =
            serde_json::from_str(r#"{"name":"Mevo","launch_only_spin":100}"#).expect("parse");
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("launch_only_spin"), "{json}");
    }

    #[test]
    fn zero_and_other_values_round_trip() {
        for v in [0.0, 80.0, 125.5] {
            let json = format!(r#"{{"name":"Mevo","launch_only_spin":{v}}}"#);
            let s: MevoSection = serde_json::from_str(&json).expect("parse");
            assert_eq!(s.launch_only_spin, v);
            let out = serde_json::to_string(&s).expect("serialize");
            assert!(out.contains("launch_only_spin"), "{out}");
            let back: MevoSection = serde_json::from_str(&out).expect("parse");
            assert_eq!(back, s);
            assert_eq!(back.launch_only_spin_rpm_per_mph(), v);
        }
    }

    #[test]
    fn negative_non_finite_or_above_1000_is_off() {
        for v in [
            -1.0,
            1000.001,
            1e12,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert!(!MevoSection::launch_only_spin_valid(v), "{v}");
            let s = MevoSection {
                launch_only_spin: v,
                ..MevoSection::default()
            };
            assert_eq!(s.launch_only_spin_rpm_per_mph(), 0.0, "{v}");
        }
        assert!(MevoSection::launch_only_spin_valid(0.0));
        assert!(MevoSection::launch_only_spin_valid(1000.0));
        assert_eq!(MevoSection::MAX_LAUNCH_ONLY_SPIN, 1000.0);
    }
}

#[cfg(test)]
mod square_impact_calibration_tests {
    use super::*;

    #[test]
    fn absent_map_is_not_written_back() {
        let s = SquareSection::default();
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("dot_bottom_to_face_center_mm"), "{json}");
    }

    #[test]
    fn empty_map_is_not_written_back() {
        let s = SquareSection {
            dot_bottom_to_face_center_mm: Some(std::collections::BTreeMap::new()),
            ..SquareSection::default()
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(!json.contains("dot_bottom_to_face_center_mm"), "{json}");
    }

    #[test]
    fn map_round_trips() {
        let mut s = SquareSection::default();
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert("driver".to_string(), 19.0);
        overrides.insert("7i".to_string(), 23.0);
        s.dot_bottom_to_face_center_mm = Some(overrides.clone());

        let json = serde_json::to_string(&s).expect("serialize");
        let back: SquareSection = serde_json::from_str(&json).expect("parse");
        assert_eq!(back.dot_bottom_to_face_center_mm, Some(overrides));
    }

    #[test]
    fn the_british_key_is_read_and_the_american_one_written() {
        let s: SquareSection =
            serde_json::from_str(r#"{"name":"Square","dot_bottom_to_face_centre_mm":{"DR":15.0}}"#)
                .expect("parse");
        let overrides = s.dot_bottom_to_face_center_mm.clone().expect("overrides");
        assert_eq!(overrides.get("DR"), Some(&15.0));
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(
            json.contains(r#""dot_bottom_to_face_center_mm":{"DR":15.0}"#),
            "{json}"
        );
        assert!(!json.contains("centre"), "{json}");
    }

    #[test]
    fn absent_map_parses_as_none() {
        let s: SquareSection = serde_json::from_str(r#"{"name":"Square"}"#).expect("parse");
        assert_eq!(s.dot_bottom_to_face_center_mm, None);
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

    /// Auto-chip is disabled by default and written only when enabled; an
    /// enabled one survives a JSON round trip and makes the section
    /// non-default.
    #[test]
    fn auto_chip_defaults_off_and_round_trips_when_on() {
        assert!(!SimulationSection::default().auto_chip);
        assert!(SimulationSection::default().is_default());
        let s: SimulationSection = serde_json::from_str("{}").expect("parses");
        assert!(!s.auto_chip, "an absent key is disabled");
        let json = serde_json::to_string(&SimulationSection::default()).expect("ser");
        assert!(!json.contains("auto_chip"), "{json}");

        let on = SimulationSection {
            auto_chip: true,
            ..SimulationSection::default()
        };
        assert!(!on.is_default());
        let json = serde_json::to_string(&on).expect("ser");
        assert!(json.contains(r#""auto_chip":true"#), "{json}");
        let back: SimulationSection = serde_json::from_str(&json).expect("parses");
        assert_eq!(back, on);
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
                auto_chip: true,
            },
            replay: ReplaySection {
                keep_shots: 7,
                pre_roll_s: 2.5,
                post_roll_s: 0.5,
                start_before_impact_s: 1.5,
                clip_dir: Some("/srv/clips".into()),
                shot_cards: false,
            },
            body_tracking: BodyTrackingSection {
                mode: BodyTrackingMode::Live,
                model: PoseModel::RtmposeS,
                min_score: 0.45,
                smoothing: 2.0,
                detect_every: 5,
                show_skeleton: false,
                legacy_golfer_height: None,
            },
            player: PlayerSection {
                sex: Sex::Female,
                height: Some(180.0),
            },
            webserver: one(WebserverSection {
                name: "Web".into(),
                bind: "0.0.0.0:5881".into(),
            }),
            mevo: one(MevoSection::default()),
            r10: one(R10Section::default()),
            square: one(SquareSection::default()),
            exputt: one(ExPuttSection {
                name: "Putting".into(),
                enabled: true,
                address: Some("192.168.1.50".into()),
                impact_lead_s: None,
            }),
            mock_monitor: one(MockMonitorSection {
                name: "Mock".into(),
                enabled: true,
                impact_lead_s: None,
            }),
            openconnect_server: one(OpenConnectServerSection {
                name: "Uneekor".into(),
                enabled: true,
                bind: Some("0.0.0.0:921".into()),
                impact_lead_s: None,
            }),
            openconnect_client: one(OpenConnectClientSection {
                name: "Muni Golf".into(),
                address: Some("127.0.0.1:9210".into()),
                full_monitor: Some("openconnect_server.3".into()),
                ..OpenConnectClientSection::default()
            }),
            pureplay: one(PureplaySection {
                name: "Pureplay".into(),
                address: Some("192.168.1.20:9760".into()),
                putting_monitor: Some("exputt.3".into()),
                ..PureplaySection::default()
            }),
            random_club: one(RandomClubSection {
                name: "Clubs".into(),
                enabled: true,
            }),
            camera: one(CameraSection {
                name: "Face".into(),
                enabled: true,
                camera: "usb-0000:00:14.0-1/HD Pro Webcam C920".into(),
                camera_name: Some("HD Pro Webcam C920".into()),
                position: CameraPosition::Overhead,
                mode: "1920x1080@30".into(),
                latency_s: Some(0.4),
                rotation_deg: 270,
                yaw_deg: Some(-7.5),
                pitch_deg: Some(12.0),
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
                enabled: true,
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

#[cfg(test)]
mod camera_tests {
    use super::*;

    #[test]
    fn replay_defaults_are_omitted_and_read_back() {
        let config = FlighthookConfig::default();
        let json = serde_json::to_string(&config).expect("ser");
        assert!(!json.contains("replay"), "{json}");
        assert!(!json.contains("camera"), "{json}");
        let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
        assert_eq!(back.replay, ReplaySection::default());
        let r = ReplaySection::default();
        assert_eq!((r.keep_shots, r.pre_roll_s, r.post_roll_s), (20, 4.0, 1.5));
        assert_eq!(r.start_before_impact_s, 2.0);
        assert_eq!(r.clip_dir, None);
        assert!(r.error().is_none());
        // A section with only some keys takes the defaults for the rest.
        let r: ReplaySection = serde_json::from_str(r#"{"keep_shots":5}"#).expect("de");
        assert_eq!((r.keep_shots, r.pre_roll_s, r.post_roll_s), (5, 4.0, 1.5));
        assert_eq!(r.start_before_impact_s, 2.0);
    }

    #[test]
    fn a_changed_replay_section_round_trips() {
        let config = FlighthookConfig {
            replay: ReplaySection {
                keep_shots: 50,
                pre_roll_s: 3.0,
                post_roll_s: 2.0,
                start_before_impact_s: 3.5,
                clip_dir: Some("D:/swings".into()),
                shot_cards: false,
            },
            ..FlighthookConfig::default()
        };
        let v = serde_json::to_value(&config).expect("ser");
        assert_eq!(v["replay"]["keep_shots"], 50);
        assert_eq!(v["replay"]["start_before_impact_s"], 3.5);
        assert_eq!(v["replay"]["clip_dir"], "D:/swings");
        assert_eq!(v["replay"]["shot_cards"], false);
        let back: FlighthookConfig = serde_json::from_value(v).expect("de");
        assert_eq!(back.replay, config.replay);
    }

    #[test]
    fn shot_cards_default_on_and_are_written_only_when_off() {
        // A section without the key: on, and written back without it.
        for text in ["{}", r#"{"keep_shots":5}"#, r#"{"clip_dir":"D:/swings"}"#] {
            let section: ReplaySection = serde_json::from_str(text).expect("de");
            assert!(section.shot_cards, "{text}");
            let json = serde_json::to_string(&section).expect("ser");
            assert!(!json.contains("shot_cards"), "{json}");
        }
        assert!(ReplaySection::default().shot_cards);
        let off = ReplaySection {
            shot_cards: false,
            ..ReplaySection::default()
        };
        assert!(!off.is_default());
        assert_eq!(off.error(), None);
        let config = FlighthookConfig {
            replay: off.clone(),
            ..FlighthookConfig::default()
        };
        let json = serde_json::to_string(&config).expect("ser");
        assert!(json.contains(r#""shot_cards":false"#), "{json}");
        let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
        assert_eq!(back.replay, off);
        let on: ReplaySection = serde_json::from_str(r#"{"shot_cards":true}"#).expect("de");
        assert!(on.is_default());
        let config = FlighthookConfig {
            replay: on,
            ..FlighthookConfig::default()
        };
        let json = serde_json::to_string(&config).expect("ser");
        assert!(!json.contains("replay"), "{json}");
    }

    #[test]
    fn replay_values_out_of_range_are_errors() {
        let bad = [
            ReplaySection {
                keep_shots: 0,
                ..ReplaySection::default()
            },
            ReplaySection {
                keep_shots: ReplaySection::MAX_KEEP_SHOTS + 1,
                ..ReplaySection::default()
            },
            ReplaySection {
                pre_roll_s: -0.1,
                ..ReplaySection::default()
            },
            ReplaySection {
                post_roll_s: 10.5,
                ..ReplaySection::default()
            },
            ReplaySection {
                pre_roll_s: f64::NAN,
                ..ReplaySection::default()
            },
            ReplaySection {
                start_before_impact_s: -0.1,
                ..ReplaySection::default()
            },
            ReplaySection {
                start_before_impact_s: 10.5,
                ..ReplaySection::default()
            },
            ReplaySection {
                start_before_impact_s: f64::INFINITY,
                ..ReplaySection::default()
            },
        ];
        for r in bad {
            assert!(r.error().is_some(), "{r:?}");
        }
        let edge = ReplaySection {
            keep_shots: 1,
            pre_roll_s: 0.0,
            post_roll_s: ReplaySection::MAX_ROLL_S,
            start_before_impact_s: ReplaySection::MAX_START_BEFORE_IMPACT_S,
            clip_dir: None,
            shot_cards: true,
        };
        assert!(edge.error().is_none());
        let zero = ReplaySection {
            start_before_impact_s: 0.0,
            ..edge
        };
        assert!(zero.error().is_none());
    }

    #[test]
    fn camera_sections_round_trip_with_snake_case_positions() {
        let keys = [
            (CameraPosition::TargetLine, "target_line"),
            (CameraPosition::GolferFront, "golfer_front"),
            (CameraPosition::GolferBack, "golfer_back"),
            (CameraPosition::Overhead, "overhead"),
        ];
        // The settings picker lists `all()` in this order.
        assert_eq!(CameraPosition::all(), keys.map(|(position, _)| position));
        assert_eq!(
            CameraPosition::all().map(CameraPosition::label),
            ["Target Line", "Golfer Front", "Golfer Back", "Overhead"]
        );
        for (position, key) in keys {
            assert_eq!(position.key(), key);
            let section = CameraSection {
                name: "DTL".into(),
                enabled: true,
                camera: "/dev/v4l/by-id/usb-cam-video-index0".into(),
                camera_name: Some("HD Pro Webcam C920".into()),
                position,
                mode: "1280x720@120".into(),
                latency_s: Some(2.5),
                rotation_deg: 90,
                yaw_deg: Some(4.0),
                pitch_deg: None,
            };
            let v = serde_json::to_value(&section).expect("ser");
            assert_eq!(v["position"], key);
            let back: CameraSection = serde_json::from_value(v).expect("de");
            assert_eq!(back, section);
        }
    }

    #[test]
    fn a_camera_section_needs_only_its_camera() {
        let s: CameraSection =
            serde_json::from_str(r#"{"name":"Cam","camera":"test-pattern"}"#).expect("de");
        assert_eq!(s.position, CameraPosition::GolferFront);
        assert_eq!(s.mode, CameraSection::AUTO_MODE);
        assert_eq!(s.camera_name, None);
        assert_eq!(s.camera, CameraSection::TEST_PATTERN);
    }

    #[test]
    fn cameras_count_as_user_actors_and_round_trip_in_toml_shape() {
        let mut config = FlighthookConfig::default();
        assert!(!config.has_user_actors());
        config.camera.insert(
            "0".into(),
            CameraSection {
                camera: CameraSection::TEST_PATTERN.into(),
                ..CameraSection::default()
            },
        );
        assert!(config.has_user_actors());
        let v = serde_json::to_value(&config).expect("ser");
        assert_eq!(v["camera"]["0"]["camera"], "test-pattern");
        assert_eq!(v["camera"]["0"]["mode"], "auto");
        let back: FlighthookConfig = serde_json::from_value(v).expect("de");
        assert_eq!(back, config);
    }
}

#[cfg(test)]
mod impact_lead_tests {
    use super::*;
    use crate::AUTO_CHIP_ACTOR;

    #[test]
    fn defaults_by_launch_monitor_kind() {
        for (prefix, s) in [
            ("mevo", 0.0),
            ("mock_monitor", 0.0),
            ("square", 1.0),
            ("exputt", 1.5),
            ("r10", 2.0),
            ("openconnect_server", 2.5),
            // A kind added later, or one without a better-known value.
            ("someday", 1.5),
        ] {
            assert_eq!(impact_lead_default_s(prefix), Some(s), "{prefix}");
        }
        assert_eq!(DEFAULT_IMPACT_LEAD_S, 1.5);
        assert_eq!(impact_lead_default_s(AUTO_CHIP_ACTOR), None);
    }

    #[test]
    fn the_value_in_effect_is_the_valid_setting_else_the_default() {
        assert_eq!(impact_lead_in_effect_s("square", Some(0.25)), Some(0.25));
        assert_eq!(impact_lead_in_effect_s("square", Some(0.0)), Some(0.0));
        assert_eq!(impact_lead_in_effect_s("square", Some(5.0)), Some(5.0));
        assert_eq!(impact_lead_in_effect_s("square", None), Some(1.0));
        for bad in [-0.1, 5.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                impact_lead_in_effect_s("r10", Some(bad)),
                Some(2.0),
                "{bad}"
            );
        }
        assert_eq!(impact_lead_in_effect_s(AUTO_CHIP_ACTOR, Some(1.0)), None);
    }

    #[test]
    fn the_setting_is_validated() {
        for ok in [None, Some(0.0), Some(1.5), Some(5.0)] {
            assert!(impact_lead_valid(ok), "{ok:?}");
            assert_eq!(impact_lead_error("mevo.0", ok), None);
        }
        for bad in [-0.1, 5.1, f64::NAN, f64::INFINITY] {
            assert!(!impact_lead_valid(Some(bad)), "{bad}");
            let error = impact_lead_error("mevo.0", Some(bad)).expect("error");
            assert!(error.starts_with("mevo.0: impact_lead_s "), "{error}");
            assert!(error.ends_with("is outside 0 to 5 s"), "{error}");
        }
        let mut config = FlighthookConfig::default();
        config.square.insert("1".into(), SquareSection::default());
        assert_eq!(config.impact_lead_error(), None);
        config.square.get_mut("1").unwrap().impact_lead_s = Some(6.0);
        assert!(
            config
                .impact_lead_error()
                .is_some_and(|e| e.starts_with("square.1:"))
        );
    }

    #[test]
    fn a_section_without_the_setting_serializes_as_before() {
        let json = serde_json::to_string(&MevoSection::default()).expect("ser");
        assert!(!json.contains("impact_lead_s"), "{json}");
        for json in [
            r#"{"mevo":{"0":{"name":"Mevo"}}}"#,
            r#"{"r10":{"0":{"name":"R10"}}}"#,
            r#"{"square":{"0":{"name":"Omni"}}}"#,
            r#"{"exputt":{"0":{"name":"Putt"}}}"#,
            r#"{"mock_monitor":{"0":{"name":"Mock"}}}"#,
            r#"{"openconnect_server":{"0":{"name":"Server","bind":null}}}"#,
        ] {
            let config: FlighthookConfig = serde_json::from_str(json).expect("parse");
            let out = serde_json::to_string(&config).expect("ser");
            assert!(!out.contains("impact_lead_s"), "{out}");
            let back: FlighthookConfig = serde_json::from_str(&out).expect("parse");
            assert_eq!(back, config);
        }
    }

    #[test]
    fn the_setting_round_trips_on_every_launch_monitor_section() {
        for kind in [
            "mevo",
            "r10",
            "square",
            "exputt",
            "mock_monitor",
            "openconnect_server",
        ] {
            let json = format!(r#"{{"{kind}":{{"0":{{"name":"X","impact_lead_s":0.75}}}}}}"#);
            let config: FlighthookConfig = serde_json::from_str(&json).expect("parse");
            assert_eq!(
                config.configured_impact_lead_s(&format!("{kind}.0")),
                Some(0.75)
            );
            assert_eq!(config.configured_impact_lead_s(&format!("{kind}.1")), None);
            let out = serde_json::to_string(&config).expect("ser");
            assert!(out.contains(r#""impact_lead_s":0.75"#), "{out}");
            let back: FlighthookConfig = serde_json::from_str(&out).expect("parse");
            assert_eq!(back, config);
        }
        // An integer is a number too.
        let config: FlighthookConfig =
            serde_json::from_str(r#"{"square":{"0":{"name":"X","impact_lead_s":2}}}"#)
                .expect("parse");
        assert_eq!(config.configured_impact_lead_s("square.0"), Some(2.0));
    }
}

#[cfg(test)]
mod latency_tests {
    use super::*;

    #[test]
    fn latency_is_valid_from_zero_to_the_maximum() {
        for ok in [None, Some(0.0), Some(0.25), Some(10.0)] {
            assert!(latency_valid(ok), "{ok:?}");
            assert_eq!(latency_error("camera.0", ok), None);
        }
        for bad in [-0.1, 10.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!latency_valid(Some(bad)), "{bad}");
            let error = latency_error("camera.0", Some(bad)).expect("error");
            assert!(error.starts_with("camera.0: latency_s "), "{error}");
            assert!(error.ends_with("is outside 0 to 10 s"), "{error}");
        }
    }

    #[test]
    fn an_invalid_latency_is_not_in_effect() {
        assert!((latency_in_effect_s(Some(2.5)) - 2.5).abs() < f64::EPSILON);
        assert!(latency_in_effect_s(Some(0.0)).abs() < f64::EPSILON);
        assert!(latency_in_effect_s(None).abs() < f64::EPSILON);
        for bad in [-0.1, 10.1, f64::NAN] {
            assert!(latency_in_effect_s(Some(bad)).abs() < f64::EPSILON, "{bad}");
        }
    }

    #[test]
    fn the_config_reports_its_first_invalid_camera_latency() {
        let mut config = FlighthookConfig::default();
        assert_eq!(config.latency_error(), None);
        for (index, latency_s) in [("0", Some(1.0)), ("1", Some(11.0)), ("2", Some(-1.0))] {
            config.camera.insert(
                index.into(),
                CameraSection {
                    camera: CameraSection::TEST_PATTERN.into(),
                    latency_s,
                    ..CameraSection::default()
                },
            );
        }
        let error = config.latency_error().expect("error");
        assert!(error.starts_with("camera.1: latency_s 11 "), "{error}");
    }

    #[test]
    fn enabled_defaults_on_and_is_written_only_when_off() {
        let text = r#"{
            "mevo": {"0": {"name": "Mevo", "address": "192.168.2.1:5100"}},
            "r10": {"0": {"name": "R10"}},
            "square": {"0": {"name": "Omni"}},
            "exputt": {"0": {"name": "ExPutt"}},
            "mock_monitor": {"0": {"name": "Mock"}},
            "openconnect_server": {"0": {"name": "Uneekor"}},
            "openconnect_client": {"0": {"name": "Open Connect"}},
            "pureplay": {"0": {"name": "Pureplay Connect"}},
            "random_club": {"0": {"name": "Random"}},
            "camera": {"0": {"name": "Camera"}}
        }"#;
        let mut config: FlighthookConfig = serde_json::from_str(text).expect("de");
        assert!(config.disabled_actors().is_empty(), "absent is on");
        let json = serde_json::to_string(&config).expect("ser");
        assert!(!json.contains("enabled"), "{json}");

        config.mock_monitor.get_mut("0").unwrap().enabled = false;
        config.openconnect_client.get_mut("0").unwrap().enabled = false;
        config.camera.get_mut("0").unwrap().enabled = false;
        assert_eq!(
            config.disabled_actors(),
            ["camera.0", "mock_monitor.0", "openconnect_client.0"]
        );
        assert!(!config.actor_enabled("mock_monitor.0"));
        assert!(config.actor_enabled("mevo.0"));
        assert!(config.actor_enabled("webserver.0"), "not a toggled type");
        assert!(config.actor_enabled("mevo.7"), "not configured");
        let json = serde_json::to_string(&config).expect("ser");
        assert_eq!(json.matches(r#""enabled":false"#).count(), 3, "{json}");
        assert!(!json.contains(r#""enabled":true"#), "{json}");
        let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
        assert_eq!(back.disabled_actors(), config.disabled_actors());
        // Written on: read as on.
        let on: MockMonitorSection =
            serde_json::from_str(r#"{"name":"Mock","enabled":true}"#).expect("de");
        assert!(on.enabled);
    }

    #[test]
    fn a_camera_without_a_latency_serializes_byte_identically() {
        let section = CameraSection {
            camera: CameraSection::TEST_PATTERN.into(),
            ..CameraSection::default()
        };
        assert_eq!(section.latency_s, None);
        let json = serde_json::to_string(&section).expect("ser");
        assert!(!json.contains("latency_s"), "{json}");
    }

    #[test]
    fn a_latency_round_trips_and_zero_is_kept() {
        for (text, expected) in [
            (r#"{"camera":"test-pattern","latency_s":2.5}"#, Some(2.5)),
            (r#"{"camera":"test-pattern","latency_s":0}"#, Some(0.0)),
            (r#"{"camera":"test-pattern"}"#, None),
        ] {
            let s: CameraSection = serde_json::from_str(text).expect("de");
            assert_eq!(s.latency_s, expected, "{text}");
            let back = serde_json::to_string(&s).expect("ser");
            assert_eq!(back.contains("latency_s"), expected.is_some(), "{back}");
        }
    }
}

#[cfg(test)]
mod body_tracking_tests {
    use super::*;

    #[test]
    fn a_config_without_the_section_reads_and_writes_without_it() {
        let c: FlighthookConfig =
            serde_json::from_str(r#"{"default_units":"metric"}"#).expect("parse");
        assert!(c.body_tracking.is_default());
        let json = serde_json::to_string(&c).expect("serialize");
        assert!(!json.contains("body_tracking"), "{json}");
        assert_eq!(
            c.body_tracking.mode,
            BodyTrackingMode::Off,
            "off by default"
        );
        // Written out at the defaults, the section is left out again.
        let c: FlighthookConfig = serde_json::from_str(
            r#"{"body_tracking":{"enabled":false,"model":"rtmpose-m","min_score":0.3,"smoothing":1.0,"detect_every":10}}"#,
        )
        .expect("parse");
        assert!(!serde_json::to_string(&c).unwrap().contains("body_tracking"));
    }

    #[test]
    fn only_the_fields_off_their_defaults_are_written() {
        let section = BodyTrackingSection {
            model: PoseModel::RtmposeS,
            detect_every: 30,
            ..BodyTrackingSection::default()
        };
        assert_eq!(
            serde_json::to_string(&section).unwrap(),
            r#"{"model":"rtmpose-s","detect_every":30}"#
        );
        let back: BodyTrackingSection =
            serde_json::from_str(r#"{"model":"rtmpose-s","detect_every":30}"#).unwrap();
        assert_eq!(back, section);
        let live: BodyTrackingSection = serde_json::from_str(r#"{"mode":"live"}"#).unwrap();
        assert!(live.mode == BodyTrackingMode::Live && live.model == PoseModel::RtmposeM);
        assert_eq!(serde_json::to_string(&live).unwrap(), r#"{"mode":"live"}"#);
        let off: BodyTrackingSection = serde_json::from_str(r#"{"mode":"off"}"#).unwrap();
        assert_eq!(off, BodyTrackingSection::default());
        assert!(serde_json::from_str::<BodyTrackingSection>(r#"{"model":"rtmpose-x"}"#).is_err());
        assert!(serde_json::from_str::<BodyTrackingSection>(r#"{"mode":"always"}"#).is_err());
    }

    #[test]
    fn show_skeleton_defaults_on_and_is_written_only_when_off() {
        // A section from before the field: on, and written back unchanged.
        for text in [
            "{}",
            r#"{"mode":"live","min_score":0.45}"#,
            r#"{"mode":"historical","model":"rtmpose-s","detect_every":30}"#,
        ] {
            let section: BodyTrackingSection = serde_json::from_str(text).unwrap();
            assert!(section.show_skeleton, "{text}");
            assert_eq!(serde_json::to_string(&section).unwrap(), text);
        }
        assert!(BodyTrackingSection::default().show_skeleton);
        let hidden = BodyTrackingSection {
            show_skeleton: false,
            ..BodyTrackingSection::default()
        };
        assert!(!hidden.is_default());
        assert_eq!(hidden.error(), None);
        assert!(!hidden.in_effect().show_skeleton);
        let json = serde_json::to_string(&hidden).unwrap();
        assert_eq!(json, r#"{"show_skeleton":false}"#);
        assert_eq!(
            serde_json::from_str::<BodyTrackingSection>(&json).unwrap(),
            hidden
        );
        let shown: BodyTrackingSection = serde_json::from_str(r#"{"show_skeleton":true}"#).unwrap();
        assert!(shown.is_default());
        assert_eq!(serde_json::to_string(&shown).unwrap(), "{}");
    }

    #[test]
    fn each_mode_round_trips_and_only_off_the_default_is_written() {
        for mode in BodyTrackingMode::all() {
            let section = BodyTrackingSection {
                mode,
                ..BodyTrackingSection::default()
            };
            let json = serde_json::to_string(&section).unwrap();
            if mode == BodyTrackingMode::Off {
                assert_eq!(json, "{}");
                assert!(section.is_default());
            } else {
                assert_eq!(json, format!(r#"{{"mode":"{}"}}"#, mode.key()));
            }
            assert_eq!(
                serde_json::from_str::<BodyTrackingSection>(&json).unwrap(),
                section
            );
        }
        assert_eq!(
            BodyTrackingMode::all().map(BodyTrackingMode::label),
            ["Off", "Historical", "Live"]
        );
        assert!(!BodyTrackingMode::Off.is_on() && BodyTrackingMode::Live.is_on());
    }

    #[test]
    fn the_switch_of_an_older_section_reads_as_a_mode_and_only_the_mode_is_written() {
        let read = |text: &str| -> BodyTrackingSection { serde_json::from_str(text).unwrap() };
        let on = read(r#"{"enabled":true,"model":"rtmpose-s"}"#);
        assert_eq!(on.mode, BodyTrackingMode::Historical);
        assert_eq!(on.model, PoseModel::RtmposeS);
        assert_eq!(
            serde_json::to_string(&on).unwrap(),
            r#"{"mode":"historical","model":"rtmpose-s"}"#
        );
        let off = read(r#"{"enabled":false}"#);
        assert_eq!(off.mode, BodyTrackingMode::Off);
        assert!(off.is_default());
        assert_eq!(serde_json::to_string(&off).unwrap(), "{}");
        // Neither: off.
        assert_eq!(read(r#"{"model":"rtmpose-s"}"#).mode, BodyTrackingMode::Off);
        // A mode wins over the switch.
        assert_eq!(
            read(r#"{"enabled":false,"mode":"live"}"#).mode,
            BodyTrackingMode::Live
        );
        // In a whole config.
        let c: FlighthookConfig =
            serde_json::from_str(r#"{"body_tracking":{"enabled":false}}"#).unwrap();
        assert!(c.body_tracking.is_default());
        assert!(!serde_json::to_string(&c).unwrap().contains("body_tracking"));
        let c: FlighthookConfig =
            serde_json::from_str(r#"{"body_tracking":{"enabled":true}}"#).unwrap();
        assert_eq!(c.body_tracking.mode, BodyTrackingMode::Historical);
        assert!(
            serde_json::to_string(&c)
                .unwrap()
                .contains(r#""body_tracking":{"mode":"historical"}"#)
        );
    }

    #[test]
    fn out_of_range_values_are_errors_and_default_in_effect() {
        let ok = BodyTrackingSection::default();
        assert_eq!(ok.error(), None);
        for (bad, field) in [
            (
                BodyTrackingSection {
                    min_score: 1.5,
                    ..ok.clone()
                },
                "min_score",
            ),
            (
                BodyTrackingSection {
                    min_score: f64::NAN,
                    ..ok.clone()
                },
                "min_score",
            ),
            (
                BodyTrackingSection {
                    smoothing: 0.05,
                    ..ok.clone()
                },
                "smoothing",
            ),
            (
                BodyTrackingSection {
                    smoothing: 11.0,
                    ..ok.clone()
                },
                "smoothing",
            ),
            (
                BodyTrackingSection {
                    detect_every: 0,
                    ..ok.clone()
                },
                "detect_every",
            ),
            (
                BodyTrackingSection {
                    detect_every: 61,
                    ..ok.clone()
                },
                "detect_every",
            ),
        ] {
            let error = bad.error().expect(field);
            assert!(error.starts_with(field), "{error}");
            assert_eq!(bad.in_effect(), ok, "{field}");
        }
        // Off, and the ends of each range, are fine.
        for good in [
            BodyTrackingSection {
                smoothing: 0.0,
                min_score: 0.0,
                detect_every: 1,
                ..ok.clone()
            },
            BodyTrackingSection {
                smoothing: BodyTrackingSection::MAX_SMOOTHING_HZ,
                min_score: 1.0,
                detect_every: BodyTrackingSection::MAX_DETECT_EVERY,
                mode: BodyTrackingMode::Off,
                model: PoseModel::RtmposeS,
                show_skeleton: false,
                legacy_golfer_height: None,
            },
        ] {
            assert_eq!(good.error(), None);
            assert_eq!(good.in_effect(), good);
        }
    }

    #[test]
    fn the_player_section_round_trips_and_is_omitted_at_its_defaults() {
        let c = FlighthookConfig::default();
        assert!(c.player.is_default());
        assert!(!serde_json::to_string(&c).unwrap().contains("player"));
        assert_eq!(c.player.sex, Sex::Male);
        let player = PlayerSection {
            sex: Sex::Female,
            height: Some(182.5),
        };
        assert!(!player.is_default());
        let json = serde_json::to_string(&player).unwrap();
        assert_eq!(json, r#"{"sex":"female","height":182.5}"#);
        assert_eq!(
            serde_json::from_str::<PlayerSection>(&json).unwrap(),
            player
        );
        let height_only = PlayerSection {
            height: Some(175.0),
            ..PlayerSection::default()
        };
        assert_eq!(
            serde_json::to_string(&height_only).unwrap(),
            r#"{"height":175.0}"#
        );
        let v = serde_json::to_value(FlighthookConfig {
            player: player.clone(),
            ..FlighthookConfig::default()
        })
        .unwrap();
        assert_eq!(
            v["player"],
            serde_json::json!({"sex": "female", "height": 182.5})
        );
        assert_eq!(Sex::all().map(Sex::key), ["male", "female"]);
        assert_eq!(Sex::all().map(Sex::label), ["Men's", "Women's"]);
        for sex in Sex::all() {
            assert_eq!(
                serde_json::to_value(sex).unwrap(),
                serde_json::Value::from(sex.key())
            );
        }
    }

    #[test]
    fn a_player_height_is_validated() {
        for good in [
            None,
            Some(PlayerSection::MIN_HEIGHT_CM),
            Some(180.0),
            Some(PlayerSection::MAX_HEIGHT_CM),
        ] {
            let s = PlayerSection {
                height: good,
                sex: Sex::Female,
            };
            assert_eq!(s.error(), None, "{good:?}");
            assert_eq!(s.in_effect(), s);
        }
        for bad in [99.0, 99.9, 250.5, f64::NAN, f64::INFINITY, -180.0] {
            let s = PlayerSection {
                height: Some(bad),
                sex: Sex::Female,
            };
            let error = s.error().expect("out of range");
            assert!(error.starts_with("height"), "{error}");
            assert_eq!(
                s.in_effect(),
                PlayerSection {
                    height: None,
                    sex: Sex::Female
                },
                "{bad}"
            );
        }
        assert_eq!(
            PlayerSection {
                height: Some(99.0),
                ..PlayerSection::default()
            }
            .error()
            .as_deref(),
            Some("height 99 is outside 100 to 250 cm")
        );
    }

    #[test]
    fn a_golfer_height_in_body_tracking_moves_to_the_player() {
        let c: FlighthookConfig =
            serde_json::from_str(r#"{"body_tracking":{"golfer_height":182.5}}"#).unwrap();
        assert_eq!(c.player.height, Some(182.5));
        assert_eq!(c.body_tracking.legacy_golfer_height, None);
        assert!(c.body_tracking.is_default());
        // Never written back where it came from.
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("golfer_height"), "{json}");
        assert!(!json.contains("body_tracking"), "{json}");
        assert!(json.contains(r#""player":{"height":182.5}"#), "{json}");
        // The player's own height wins.
        let c: FlighthookConfig = serde_json::from_str(
            r#"{"body_tracking":{"golfer_height":182.5},"player":{"height":170.0}}"#,
        )
        .unwrap();
        assert_eq!(c.player.height, Some(170.0));
        // Beside the other body tracking settings.
        let c: FlighthookConfig =
            serde_json::from_str(r#"{"body_tracking":{"mode":"live","golfer_height":182.5}}"#)
                .unwrap();
        assert_eq!(c.player.height, Some(182.5));
        assert_eq!(c.body_tracking.mode, BodyTrackingMode::Live);
        // A section read alone carries it, and it never counts as a setting.
        let section: BodyTrackingSection =
            serde_json::from_str(r#"{"golfer_height":182.5}"#).unwrap();
        assert_eq!(section.legacy_golfer_height, Some(182.5));
        assert!(section.is_default());
        assert_eq!(section, BodyTrackingSection::default());
        assert_eq!(serde_json::to_string(&section).unwrap(), "{}");
        assert_eq!(section.in_effect().legacy_golfer_height, None);
    }

    #[test]
    fn camera_angles_and_rotation_are_omitted_at_their_defaults() {
        let s = CameraSection {
            camera: CameraSection::TEST_PATTERN.into(),
            ..CameraSection::default()
        };
        let v = serde_json::to_value(&s).unwrap();
        for key in ["rotation_deg", "yaw_deg", "pitch_deg"] {
            assert!(v.get(key).is_none(), "{key}: {v}");
        }
        let turned = CameraSection {
            rotation_deg: 90,
            yaw_deg: Some(0.0),
            pitch_deg: Some(-3.5),
            ..s.clone()
        };
        let v = serde_json::to_value(&turned).unwrap();
        assert_eq!(v["rotation_deg"], 90);
        assert_eq!(v["yaw_deg"], 0.0);
        assert_eq!(v["pitch_deg"], -3.5);
        let back: CameraSection = serde_json::from_value(v).unwrap();
        assert_eq!(back, turned);
        // An old section loads with no rotation and no angles.
        let old: CameraSection = serde_json::from_str(
            r#"{"name":"Face","camera":"test-pattern","position":"golfer_front","mode":"auto"}"#,
        )
        .unwrap();
        assert_eq!(old.rotation_deg, 0);
        assert_eq!(old.yaw_deg, None);
        assert_eq!(old.pitch_deg, None);
    }

    #[test]
    fn camera_angles_and_rotation_are_validated() {
        let ok = CameraSection {
            camera: CameraSection::TEST_PATTERN.into(),
            ..CameraSection::default()
        };
        for rotation in CAMERA_ROTATIONS {
            let s = CameraSection {
                rotation_deg: rotation,
                ..ok.clone()
            };
            assert_eq!(s.error("camera.0"), None);
            assert_eq!(rotation_in_effect_deg(rotation), rotation);
        }
        for angle in [None, Some(-30.0), Some(0.0), Some(12.5), Some(30.0)] {
            let s = CameraSection {
                yaw_deg: angle,
                pitch_deg: angle,
                ..ok.clone()
            };
            assert_eq!(s.error("camera.0"), None, "{angle:?}");
            assert_eq!(camera_angle_in_effect_deg(angle), angle.unwrap_or(0.0));
        }
        let cases = [
            (
                CameraSection {
                    rotation_deg: 45,
                    ..ok.clone()
                },
                "camera.2: rotation_deg 45 is not 0, 90, 180 or 270",
            ),
            (
                CameraSection {
                    yaw_deg: Some(40.0),
                    ..ok.clone()
                },
                "camera.2: yaw_deg 40 is outside -30 to 30",
            ),
            (
                CameraSection {
                    yaw_deg: Some(-30.5),
                    ..ok.clone()
                },
                "camera.2: yaw_deg -30.5 is outside -30 to 30",
            ),
            (
                CameraSection {
                    pitch_deg: Some(f64::NAN),
                    ..ok.clone()
                },
                "camera.2: pitch_deg NaN is outside -30 to 30",
            ),
            (
                CameraSection {
                    latency_s: Some(11.0),
                    rotation_deg: 45,
                    ..ok.clone()
                },
                "camera.2: latency_s 11 is outside 0 to 10 s",
            ),
        ];
        for (section, message) in cases {
            assert_eq!(section.error("camera.2").as_deref(), Some(message));
        }
        assert_eq!(rotation_in_effect_deg(45), 0);
        assert_eq!(camera_angle_in_effect_deg(Some(40.0)), 0.0);
        assert_eq!(camera_angle_in_effect_deg(Some(f64::INFINITY)), 0.0);
        // The config reports the first camera by index.
        let mut c = FlighthookConfig::default();
        c.camera.insert(
            "1".into(),
            CameraSection {
                pitch_deg: Some(31.0),
                ..ok.clone()
            },
        );
        c.camera.insert(
            "0".into(),
            CameraSection {
                rotation_deg: 1,
                ..ok.clone()
            },
        );
        assert_eq!(
            c.camera_error().as_deref(),
            Some("camera.0: rotation_deg 1 is not 0, 90, 180 or 270")
        );
        c.camera.remove("0");
        assert_eq!(
            c.camera_error().as_deref(),
            Some("camera.1: pitch_deg 31 is outside -30 to 30")
        );
    }
}
