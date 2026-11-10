mod trackball;

use std::collections::HashMap;

use crate::app::{CameraList, FlighthookApp};
use crate::net;
use crate::panels::cards::AddPreset;
use crate::panels::devices;
use crate::theme;
use crate::types::{
    CameraListEntry, CameraMode, CameraPosition, CameraSection, Club, Distance, DistanceExt,
    FlightScopeEnvironment, FlighthookConfig, MevoSection, MockMonitorSection,
    OpenConnectClientSection, PureplaySection, R10Section, RandomClubSection, ReplaySection,
    ShotGrouping, SimSurface, SimulationSection, SpinSource, UnitSystem, WebserverSection,
};
use flighthook::{
    BodyTrackingMode, BodyTrackingSection, DriverConfig, MevoConfig, MockConfig, PlayerSection,
    PoseModel, Sex, SimMode, SquareConfig,
};

const DISTANCE_UNITS: &[(&str, &str)] = &[
    ("inches", "in"),
    ("feet", "ft"),
    ("yards", "yd"),
    ("meters", "m"),
    ("centimeters", "cm"),
];

/// Format a distance value: strip trailing zeros but keep at least one decimal.
fn format_distance_value(v: f64) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}

/// The FlightScope driver's values for settings left unset, as its card
/// shows them.
const MEVO_ADDRESS: &str = flighthook::MEVO_DEFAULT_ADDRESS;
const MEVO_BALL_TYPE: u8 = flighthook::MEVO_DEFAULT_BALL_TYPE;
const MEVO_TEE_HEIGHT: Distance = flighthook::MEVO_DEFAULT_TEE_HEIGHT;
const MEVO_RANGE: Distance = flighthook::MEVO_DEFAULT_RANGE;
const MEVO_SURFACE_HEIGHT: Distance = flighthook::MEVO_DEFAULT_SURFACE_HEIGHT;

/// The FlightScope environments, in menu order.
const ENVIRONMENTS: [FlightScopeEnvironment; 2] = [
    FlightScopeEnvironment::LimitedFlight,
    FlightScopeEnvironment::Outdoor,
];

/// The FlightScope Xi spin sources, in menu order.
const SPIN_SOURCES: [SpinSource; 3] = [SpinSource::D4, SpinSource::E8, SpinSource::Auto];

/// The FlightScope camera modes, in menu order.
const CAMERA_MODES: [CameraMode; 3] = [
    CameraMode::Standard,
    CameraMode::Fusion,
    CameraMode::RawFusion,
];

fn environment_label(env: FlightScopeEnvironment) -> &'static str {
    match env {
        FlightScopeEnvironment::LimitedFlight => "Limited flight (sim/indoor)",
        FlightScopeEnvironment::Outdoor => "Outdoor (full flight)",
    }
}

fn spin_source_label(src: SpinSource) -> &'static str {
    match src {
        SpinSource::D4 => "D4 (matches FS Golf)",
        SpinSource::E8 => "E8",
        SpinSource::Auto => "Auto (E8 on full swings)",
    }
}

fn camera_mode_label(mode: CameraMode) -> &'static str {
    match mode {
        CameraMode::Standard => "Standard (radar only)",
        CameraMode::Fusion => "Fusion — older firmware",
        CameraMode::RawFusion => "Raw Fusion — BM17.04+",
    }
}

/// The FlightScope track % as the card shows it: whole percent, the
/// driver's default when unset.
fn track_pct_text(track_pct: Option<f64>) -> String {
    format!(
        "{:.0}",
        track_pct.unwrap_or(flighthook::MEVO_DEFAULT_TRACK_PCT)
    )
}

/// A distance setting from the form: `loaded` while the field still shows
/// it (or `default` for an unset one), else the value typed in `unit`;
/// blank or not a number is unset.
fn form_distance(
    loaded: Option<Distance>,
    default: Distance,
    value: &str,
    unit: &str,
) -> Option<Distance> {
    let shown = loaded.unwrap_or(default);
    let value = value.trim();
    if value == format_distance_value(shown.value()) && unit == shown.unit_key() {
        return loaded;
    }
    value
        .parse::<f64>()
        .ok()
        .map(|v| Distance::from_value_and_unit(v, unit))
}

fn unit_suffix(key: &str) -> &str {
    DISTANCE_UNITS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, suffix)| *suffix)
        .unwrap_or("in")
}

/// Find the lowest unused integer key (starting at "0") in a set of existing keys.
fn next_index(existing: &[&str]) -> String {
    let mut i = 0u32;
    loop {
        let key = i.to_string();
        if !existing.iter().any(|k| *k == key) {
            return key;
        }
        i += 1;
    }
}

/// Per-device form entry. The `id` is the map index (e.g. "0"),
/// and `monitor_type` encodes which section map it belongs to.
/// Distance fields are `(value_string, unit_key)` pairs where unit_key
/// is one of "inches", "feet", "meters", "centimeters".
#[derive(Clone)]
pub(crate) struct DeviceFormEntry {
    pub(crate) id: String,
    pub(crate) monitor_type: String, // "mevo" | "mock_monitor"
    pub(crate) name: String,
    /// Whether the card is on (`enabled`): the saved value, which the
    /// header switch saves at once.
    pub(crate) enabled: bool,
    pub(crate) address: String,
    pub(crate) ball_type: u8,
    pub(crate) tee_height_val: String,
    pub(crate) tee_height_unit: String,
    pub(crate) range_val: String,
    pub(crate) range_unit: String,
    pub(crate) surface_height_val: String,
    pub(crate) surface_height_unit: String,
    pub(crate) track_pct: String,
    pub(crate) use_estimated: bool,
    /// Mevo: camera mode. Fusion modes are what produce club data.
    pub(crate) camera_mode: CameraMode,
    /// FlightScope: environment (limited flight or outdoor), which picks the
    /// full-swing detection mode.
    pub(crate) environment: FlightScopeEnvironment,
    /// FlightScope Xi: which of its two spin values a shot reports.
    pub(crate) spin_source: SpinSource,
    /// FlightScope: backspin per mph of ball speed for a result with launch
    /// data but no spin, as typed. Blank means the default.
    pub(crate) launch_only_spin: String,
    /// Launch monitors: seconds from impact to the monitor's report, for the
    /// cameras' clips, as typed. Blank means the device's default.
    pub(crate) impact_lead: String,
    /// Square Golf: discard zero-spin reads, except when putting.
    pub(crate) discard_zero_spin: bool,
    /// Square Golf: the driver's settings as loaded. The form edits
    /// `address`, `discard_non_putting_zero_spin` and the calibration of
    /// the clubs it lists; the rest (`club`, `advanced_spin`, calibration
    /// keys the form does not list) is carried verbatim so that saving
    /// settings does not wipe it from the config file.
    pub(crate) square: SquareConfig,
    /// Square Golf: per-club face impact calibration override, in mm from
    /// the bottom of the sticker dot down to face center. Keyed by club; blank means "use
    /// allsquare's default" for that club. The putter is excluded — it has
    /// no vertical impact estimate.
    pub(crate) square_impact_mm: std::collections::HashMap<Club, String>,
    /// FlightScope: the driver's settings as loaded (for a new card: the
    /// values the card shows, stated). The form edits every one of them; a
    /// field still showing its loaded value keeps it, set or unset, so a
    /// save writes an unchanged section as it was.
    pub(crate) mevo: MevoConfig,
    /// Mock launch monitor: the driver's settings, which the form does not
    /// surface. Carried verbatim so that saving settings does not wipe them
    /// from the config file, and checked with the driver's own validation.
    pub(crate) mock: MockConfig,
    /// Trackball: the driver's settings, every one a field; `None` on a card
    /// of another type.
    pub(crate) trackball: Option<Box<trackball::TrackballForm>>,
    /// The card's device driver settings as saved, as JSON; `None` for a
    /// card not saved yet or one that runs no device driver. A refused
    /// value blocks a save only when the card changes it
    /// ([`driver_blocks_save`](Self::driver_blocks_save)).
    pub(crate) saved_driver: Option<serde_json::Value>,
    /// Camera: which camera, where it stands and its video mode.
    pub(crate) cam: Box<CameraFields>,
    pub(crate) dirty: bool,
}

/// The settings of a camera card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CameraFields {
    /// The camera's stable id (`CameraSection::camera`); blank until one is
    /// picked.
    pub(crate) id: String,
    /// The camera's own name, as the camera list gave it when it was picked.
    pub(crate) name: Option<String>,
    pub(crate) position: CameraPosition,
    /// `auto` or `WIDTHxHEIGHT@FPS`.
    pub(crate) mode: String,
    /// Seconds a frame is old when it arrives, as typed. Blank means 0.
    pub(crate) latency: String,
    /// Clockwise quarter turns, degrees: 0, 90, 180 or 270.
    pub(crate) rotation: u16,
    /// Degrees the camera points off square, as typed. Blank means 0.
    pub(crate) yaw: String,
    /// Degrees the camera tilts down, as typed. Blank means 0.
    pub(crate) pitch: String,
}

impl Default for CameraFields {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: None,
            position: CameraPosition::default(),
            mode: CameraSection::AUTO_MODE.to_owned(),
            latency: String::new(),
            rotation: 0,
            yaw: String::new(),
            pitch: String::new(),
        }
    }
}

/// Whether a camera video mode is one the server accepts: `auto` or
/// `WIDTHxHEIGHT@FPS`.
pub(crate) fn video_mode_valid(mode: &str) -> bool {
    mode.parse::<stance::FormatRequest>().is_ok()
}

/// Whether a string looks like a BLE device identifier: a MAC address
/// (`AA:BB:CC:DD:EE:FF`), on macOS, which hides MAC addresses, a peripheral
/// UUID (`8-4-4-4-12` hex groups), or an advertised Square Golf name such as
/// `SquareGolf(CD01)`, which is the same on every OS. The all-zero MAC is
/// the placeholder macOS reports in place of a hidden address, so it is
/// rejected.
///
/// Blank is handled by callers — for devices that auto-discover, an empty
/// address is valid.
pub(crate) fn is_ble_address(s: &str) -> bool {
    fn hex_groups(s: &str, sep: char, lens: &[usize]) -> bool {
        let groups: Vec<&str> = s.split(sep).collect();
        groups.len() == lens.len()
            && groups
                .iter()
                .zip(lens)
                .all(|(g, &n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()))
    }
    const NAME_PREFIX: &str = "SquareGolf";
    let s = s.trim();
    let advertised_name = s.len() > NAME_PREFIX.len()
        && s.get(..NAME_PREFIX.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(NAME_PREFIX));
    let mac = hex_groups(s, ':', &[2; 6]) && s != "00:00:00:00:00:00";
    mac || hex_groups(s, '-', &[8, 4, 4, 4, 12]) || advertised_name
}

/// Clubs with their own Square Golf face impact calibration: every club the
/// device has a code for except the putter, which has no vertical estimate.
/// The device is armed with the nearest of these for any other club.
const SQUARE_CALIBRATED_CLUBS: &[Club] = &[
    Club::Driver,
    Club::Wood3,
    Club::Wood5,
    Club::Wood7,
    Club::Hybrid3,
    Club::Hybrid4,
    Club::Hybrid5,
    Club::Iron3,
    Club::Iron4,
    Club::Iron5,
    Club::Iron6,
    Club::Iron7,
    Club::Iron8,
    Club::Iron9,
    Club::PitchingWedge,
    Club::GapWedge,
    Club::SandWedge,
    Club::LobWedge,
];

/// Build the face-impact-calibration form map from config overrides. A club
/// with no entry renders blank, meaning "use allsquare's default".
fn impact_mm_form(
    overrides: &std::collections::BTreeMap<String, f64>,
) -> std::collections::HashMap<Club, String> {
    let mut map = std::collections::HashMap::new();
    for (key, &mm) in overrides {
        if let Some(club) = Club::from_code(key)
            && SQUARE_CALIBRATED_CLUBS.contains(&club)
        {
            map.insert(club, format_distance_value(mm));
        }
    }
    map
}

/// Whether a face-impact-calibration field is acceptable: blank (use the
/// default) or a finite, non-negative mm value.
fn is_valid_impact_mm(s: &str) -> bool {
    let s = s.trim();
    s.is_empty() || s.parse::<f64>().is_ok_and(|v| v.is_finite() && v >= 0.0)
}

/// The `launch_only_spin` a form field holds: blank is the default, else a
/// number from 0 to 1000. `None` when it is neither.
fn launch_only_spin_value(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return Some(MevoSection::DEFAULT_LAUNCH_ONLY_SPIN);
    }
    s.parse::<f64>()
        .ok()
        .filter(|v| MevoSection::launch_only_spin_valid(*v))
}

/// The `impact_lead_s` a form field holds: `Some(None)` when blank (the
/// device default), `Some(Some(s))` for a number from 0 to 5, `None` when it
/// is neither.
fn impact_lead_value(s: &str) -> Option<Option<f64>> {
    let s = s.trim();
    if s.is_empty() {
        return Some(None);
    }
    s.parse::<f64>()
        .ok()
        .filter(|v| flighthook::impact_lead_valid(Some(*v)))
        .map(Some)
}

/// The `latency_s` a camera form field holds: `Some(None)` when blank (0),
/// `Some(Some(s))` for a number from 0 to 10, `None` when it is neither.
fn latency_value(s: &str) -> Option<Option<f64>> {
    let s = s.trim();
    if s.is_empty() {
        return Some(None);
    }
    s.parse::<f64>()
        .ok()
        .filter(|v| flighthook::latency_valid(Some(*v)))
        .map(Some)
}

/// The `yaw_deg` or `pitch_deg` a camera form field holds: `Some(None)` when
/// blank (0), `Some(Some(deg))` for a number from -30 to 30, `None` when it
/// is neither.
fn angle_value(s: &str) -> Option<Option<f64>> {
    let s = s.trim();
    if s.is_empty() {
        return Some(None);
    }
    s.parse::<f64>()
        .ok()
        .filter(|v| flighthook::camera_angle_valid(Some(*v)))
        .map(Some)
}

/// A section's `impact_lead_s` as the form field shows it.
fn impact_lead_text(v: Option<f64>) -> String {
    v.map(format_distance_value).unwrap_or_default()
}

/// Build the `dot_bottom_to_face_center_mm` config map from the form: non-blank
/// entries only, keyed by club code. Returns `None` when no overrides are set.
fn impact_mm_to_config(
    form: &std::collections::HashMap<Club, String>,
) -> std::collections::BTreeMap<String, f64> {
    let mut map = std::collections::BTreeMap::new();
    for (club, s) in form {
        let s = s.trim();
        if s.is_empty() {
            continue;
        }
        if let Ok(v) = s.parse::<f64>() {
            map.insert(club.to_string(), v);
        }
    }
    map
}

impl DeviceFormEntry {
    pub(crate) fn from_mevo(id: &str, s: &MevoSection) -> Self {
        let d = &s.driver;
        let tee = d.tee_height.unwrap_or(MEVO_TEE_HEIGHT);
        let rng = d.range.unwrap_or(MEVO_RANGE);
        let surf = d.surface_height.unwrap_or(MEVO_SURFACE_HEIGHT);
        Self {
            id: id.into(),
            monitor_type: "mevo".into(),
            name: s.name.clone(),
            enabled: s.enabled,
            address: d.address.clone().unwrap_or_default(),
            ball_type: d.ball_type.unwrap_or(MEVO_BALL_TYPE),
            tee_height_val: format_distance_value(tee.value()),
            tee_height_unit: tee.unit_key().into(),
            range_val: format_distance_value(rng.value()),
            range_unit: rng.unit_key().into(),
            surface_height_val: format_distance_value(surf.value()),
            surface_height_unit: surf.unit_key().into(),
            track_pct: track_pct_text(d.track_pct),
            use_estimated: d.use_estimated(),
            camera_mode: d.camera_mode(),
            environment: d.environment(),
            spin_source: d.spin_source,
            launch_only_spin: format_distance_value(s.launch_only_spin),
            impact_lead: impact_lead_text(s.impact_lead_s),
            discard_zero_spin: true,
            square: SquareConfig::default(),
            square_impact_mm: std::collections::HashMap::new(),
            mevo: d.clone(),
            mock: MockConfig::default(),
            trackball: None,
            saved_driver: serde_json::to_value(d).ok(),
            cam: Box::default(),
            dirty: false,
        }
    }

    pub(crate) fn from_r10(id: &str, s: &R10Section) -> Self {
        Self {
            id: id.into(),
            monitor_type: "r10".into(),
            name: s.name.clone(),
            enabled: s.enabled,
            address: String::new(),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            // Blank means "unset" — the device keeps its own tee distance.
            range_val: s
                .driver
                .range
                .map(|d| format_distance_value(d.value()))
                .unwrap_or_default(),
            range_unit: s
                .driver
                .range
                .map_or("feet".into(), |d| d.unit_key().to_string()),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: false,
            camera_mode: CameraMode::default(),
            environment: FlightScopeEnvironment::default(),
            spin_source: SpinSource::default(),
            launch_only_spin: format_distance_value(MevoSection::DEFAULT_LAUNCH_ONLY_SPIN),
            impact_lead: impact_lead_text(s.impact_lead_s),
            discard_zero_spin: true,
            square: SquareConfig::default(),
            square_impact_mm: std::collections::HashMap::new(),
            mevo: MevoConfig::default(),
            mock: MockConfig::default(),
            trackball: None,
            saved_driver: serde_json::to_value(&s.driver).ok(),
            cam: Box::default(),
            dirty: false,
        }
    }

    pub(crate) fn from_square(id: &str, s: &flighthook::SquareSection) -> Self {
        Self {
            id: id.into(),
            monitor_type: "square".into(),
            name: s.name.clone(),
            enabled: s.enabled,
            address: s.driver.address.clone().unwrap_or_default(),
            discard_zero_spin: s.driver.discard_non_putting_zero_spin,
            square: s.driver.clone(),
            square_impact_mm: impact_mm_form(&s.driver.dot_bottom_to_face_center_mm),
            mevo: MevoConfig::default(),
            mock: MockConfig::default(),
            trackball: None,
            saved_driver: serde_json::to_value(&s.driver).ok(),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            range_val: "8".into(),
            range_unit: "feet".into(),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: false,
            camera_mode: CameraMode::default(),
            environment: FlightScopeEnvironment::default(),
            spin_source: SpinSource::default(),
            launch_only_spin: format_distance_value(MevoSection::DEFAULT_LAUNCH_ONLY_SPIN),
            impact_lead: impact_lead_text(s.impact_lead_s),
            cam: Box::default(),
            dirty: false,
        }
    }

    pub(crate) fn from_exputt(id: &str, s: &flighthook::ExPuttSection) -> Self {
        Self {
            id: id.into(),
            monitor_type: "exputt".into(),
            name: s.name.clone(),
            enabled: s.enabled,
            address: s.driver.address.clone().unwrap_or_default(),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            range_val: "8".into(),
            range_unit: "feet".into(),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: false,
            camera_mode: CameraMode::default(),
            environment: FlightScopeEnvironment::default(),
            spin_source: SpinSource::default(),
            launch_only_spin: format_distance_value(MevoSection::DEFAULT_LAUNCH_ONLY_SPIN),
            impact_lead: impact_lead_text(s.impact_lead_s),
            discard_zero_spin: true,
            square: SquareConfig::default(),
            square_impact_mm: std::collections::HashMap::new(),
            mevo: MevoConfig::default(),
            mock: MockConfig::default(),
            trackball: None,
            saved_driver: serde_json::to_value(&s.driver).ok(),
            cam: Box::default(),
            dirty: false,
        }
    }

    pub(crate) fn from_openconnect(id: &str, s: &flighthook::OpenConnectServerSection) -> Self {
        Self {
            id: id.into(),
            monitor_type: "openconnect_server".into(),
            name: s.name.clone(),
            enabled: s.enabled,
            address: s.bind.clone().unwrap_or_default(),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            range_val: "8".into(),
            range_unit: "feet".into(),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: false,
            camera_mode: CameraMode::default(),
            environment: FlightScopeEnvironment::default(),
            spin_source: SpinSource::default(),
            launch_only_spin: format_distance_value(MevoSection::DEFAULT_LAUNCH_ONLY_SPIN),
            impact_lead: impact_lead_text(s.impact_lead_s),
            discard_zero_spin: true,
            square: SquareConfig::default(),
            square_impact_mm: std::collections::HashMap::new(),
            mevo: MevoConfig::default(),
            mock: MockConfig::default(),
            trackball: None,
            saved_driver: None,
            cam: Box::default(),
            dirty: false,
        }
    }

    pub(crate) fn from_mock(id: &str, s: &MockMonitorSection) -> Self {
        Self {
            name: s.name.clone(),
            enabled: s.enabled,
            impact_lead: impact_lead_text(s.impact_lead_s),
            mock: s.driver.clone(),
            saved_driver: serde_json::to_value(&s.driver).ok(),
            ..Self::blank(id, "mock_monitor")
        }
    }

    /// A card of type `monitor_type` at index `id` with no name and every
    /// field at its default, for the constructors to fill in.
    fn blank(id: &str, monitor_type: &str) -> Self {
        Self {
            id: id.into(),
            monitor_type: monitor_type.into(),
            name: String::new(),
            enabled: true,
            address: String::new(),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            range_val: "8".into(),
            range_unit: "feet".into(),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: false,
            camera_mode: CameraMode::default(),
            environment: FlightScopeEnvironment::default(),
            spin_source: SpinSource::default(),
            launch_only_spin: format_distance_value(MevoSection::DEFAULT_LAUNCH_ONLY_SPIN),
            impact_lead: String::new(),
            discard_zero_spin: true,
            square: SquareConfig::default(),
            square_impact_mm: std::collections::HashMap::new(),
            mevo: MevoConfig::default(),
            mock: MockConfig::default(),
            trackball: None,
            saved_driver: None,
            cam: Box::default(),
            dirty: false,
        }
    }

    pub(crate) fn from_trackball(id: &str, s: &flighthook::TrackballSection) -> Self {
        Self {
            name: s.name.clone(),
            enabled: s.enabled,
            trackball: Some(Box::new(trackball::TrackballForm::new(&s.driver))),
            saved_driver: serde_json::to_value(&s.driver).ok(),
            ..Self::blank(id, "trackball")
        }
    }

    pub(crate) fn from_camera(id: &str, s: &CameraSection) -> Self {
        Self {
            name: s.name.clone(),
            enabled: s.enabled,
            cam: Box::new(CameraFields {
                id: s.camera.clone(),
                name: s.camera_name.clone(),
                position: s.position,
                mode: s.mode.clone(),
                latency: impact_lead_text(s.latency_s),
                rotation: s.rotation_deg,
                yaw: impact_lead_text(s.yaw_deg),
                pitch: impact_lead_text(s.pitch_deg),
            }),
            ..Self::blank(id, "camera")
        }
    }

    /// Why the device driver refuses the settings the form carries for it
    /// (its own validation, the same the server applies on save), as
    /// `"<field>: <reason>"`; `None` when it accepts them or the card runs
    /// no device driver.
    pub(crate) fn driver_error(&self) -> Option<String> {
        if let Some(error) = self.unparsable() {
            return Some(error);
        }
        match self.monitor_type.as_str() {
            "mock_monitor" => self.mock.validate().err().map(|e| e.to_string()),
            "mevo" => self.mevo_config().validate().err().map(|e| e.to_string()),
            "r10" => self.r10_config().validate().err().map(|e| e.to_string()),
            "square" => self.square_config().validate().err().map(|e| e.to_string()),
            "exputt" => self.exputt_config().validate().err().map(|e| e.to_string()),
            "trackball" => self
                .trackball_config()
                .validate()
                .err()
                .map(|e| e.to_string()),
            _ => None,
        }
    }

    /// The device driver settings the form carries, as JSON; `None` for a
    /// card that runs no device driver.
    fn driver_json(&self) -> Option<serde_json::Value> {
        match self.monitor_type.as_str() {
            "mock_monitor" => serde_json::to_value(&self.mock).ok(),
            "mevo" => serde_json::to_value(self.mevo_config()).ok(),
            "r10" => serde_json::to_value(self.r10_config()).ok(),
            "square" => serde_json::to_value(self.square_config()).ok(),
            "exputt" => serde_json::to_value(self.exputt_config()).ok(),
            "trackball" => serde_json::to_value(self.trackball_config()).ok(),
            _ => None,
        }
    }

    /// Why a number field of the card's driver settings holds no number, as
    /// `"<field>: <why>"`: the R10's range, the FlightScope's tee height,
    /// range, surface height and track %, every Trackball number.
    fn unparsable(&self) -> Option<String> {
        if self.is_trackball() {
            return self
                .trackball
                .as_deref()
                .and_then(trackball::TrackballForm::unparsable);
        }
        self.unparsable_field()
            .map(|field| format!("{field}: not a number"))
    }

    /// The first number field of the card's driver settings that holds
    /// text that is not a number (blank is unset): the R10's range, the
    /// FlightScope's tee height, range, surface height and track %.
    fn unparsable_field(&self) -> Option<&'static str> {
        let fields: &[(&'static str, &str)] = match self.monitor_type.as_str() {
            "r10" => &[("range", &self.range_val)],
            "mevo" => &[
                ("tee_height", &self.tee_height_val),
                ("range", &self.range_val),
                ("surface_height", &self.surface_height_val),
                ("track_pct", &self.track_pct),
            ],
            _ => &[],
        };
        fields
            .iter()
            .find(|(_, text)| {
                let text = text.trim();
                !text.is_empty() && text.parse::<f64>().is_err()
            })
            .map(|(field, _)| *field)
    }

    /// The FlightScope driver's settings from the form, over the settings
    /// as loaded ([`mevo`](Self::mevo)): a field that still shows its
    /// loaded value keeps it (set or unset), any other is taken as typed
    /// (a blank address or number is unset; a number that doesn't parse is
    /// unset too, and [`driver_error`](Self::driver_error) refuses it).
    pub(crate) fn mevo_config(&self) -> MevoConfig {
        let m = &self.mevo;
        let address = self.address.trim();
        let pick = |loaded: Option<bool>, default: bool, shown: bool| {
            if shown == loaded.unwrap_or(default) {
                loaded
            } else {
                Some(shown)
            }
        };
        MevoConfig {
            address: if address == m.address.as_deref().unwrap_or_default().trim() {
                m.address.clone()
            } else {
                (!address.is_empty()).then(|| address.to_owned())
            },
            ball_type: if self.ball_type == m.ball_type.unwrap_or(MEVO_BALL_TYPE) {
                m.ball_type
            } else {
                Some(self.ball_type)
            },
            tee_height: form_distance(
                m.tee_height,
                MEVO_TEE_HEIGHT,
                &self.tee_height_val,
                &self.tee_height_unit,
            ),
            range: form_distance(m.range, MEVO_RANGE, &self.range_val, &self.range_unit),
            surface_height: form_distance(
                m.surface_height,
                MEVO_SURFACE_HEIGHT,
                &self.surface_height_val,
                &self.surface_height_unit,
            ),
            track_pct: if self.track_pct.trim() == track_pct_text(m.track_pct) {
                m.track_pct
            } else {
                self.track_pct.trim().parse().ok()
            },
            use_estimated: pick(m.use_estimated, false, self.use_estimated),
            camera_mode: if self.camera_mode == m.camera_mode() {
                m.camera_mode
            } else {
                Some(self.camera_mode)
            },
            environment: if self.environment == m.environment() {
                m.environment
            } else {
                Some(self.environment)
            },
            spin_source: self.spin_source,
        }
    }

    /// The R10 driver's settings from the form: the range as typed, blank
    /// (or not a number, which [`driver_error`](Self::driver_error)
    /// refuses) for none.
    pub(crate) fn r10_config(&self) -> flighthook::R10Config {
        flighthook::R10Config {
            range: self
                .range_val
                .trim()
                .parse::<f64>()
                .ok()
                .map(|v| Distance::from_value_and_unit(v, &self.range_unit)),
        }
    }

    /// The ExPutt driver's settings from the form: the IP address as typed
    /// (trimmed), blank for none (the device is found on the network).
    pub(crate) fn exputt_config(&self) -> flighthook::ExPuttConfig {
        let address = self.address.trim();
        flighthook::ExPuttConfig {
            address: (!address.is_empty()).then(|| address.to_owned()),
        }
    }

    /// The Square Golf driver's settings from the form: the address
    /// (blank for none), the zero-spin switch and the listed clubs'
    /// calibration as typed, over the settings carried from the file.
    pub(crate) fn square_config(&self) -> SquareConfig {
        let address = self.address.trim();
        let mut calibration: std::collections::BTreeMap<String, f64> = self
            .square
            .dot_bottom_to_face_center_mm
            .iter()
            .filter(|(key, _)| {
                !Club::from_code(key).is_some_and(|c| SQUARE_CALIBRATED_CLUBS.contains(&c))
            })
            .map(|(key, &mm)| (key.clone(), mm))
            .collect();
        calibration.extend(impact_mm_to_config(&self.square_impact_mm));
        SquareConfig {
            address: (!address.is_empty()).then(|| address.to_owned()),
            discard_non_putting_zero_spin: self.discard_zero_spin,
            dot_bottom_to_face_center_mm: calibration,
            ..self.square.clone()
        }
    }

    /// Whether the driver settings keep the card from saving: refused
    /// ([`driver_error`](Self::driver_error)) and changed from the saved
    /// ones. A refused saved value still shows on the card, and the card
    /// still saves its other fields and its on/off switch, as the server
    /// does.
    pub(crate) fn driver_blocks_save(&self) -> bool {
        self.unparsable().is_some()
            || (self.driver_error().is_some() && self.saved_driver != self.driver_json())
    }

    pub(crate) fn is_mevo(&self) -> bool {
        self.monitor_type == "mevo"
    }

    pub(crate) fn is_square(&self) -> bool {
        self.monitor_type == "square"
    }

    pub(crate) fn is_r10(&self) -> bool {
        self.monitor_type == "r10"
    }

    pub(crate) fn is_trackball(&self) -> bool {
        self.monitor_type == "trackball"
    }

    /// The Trackball fields; a new card's when the entry has none yet.
    pub(crate) fn trackball_form(&mut self) -> &mut trackball::TrackballForm {
        self.trackball.get_or_insert_with(Box::default)
    }

    /// The Trackball driver settings the fields give (a new card's when the
    /// entry has none).
    fn trackball_config(&self) -> flighthook::TrackballConfig {
        self.trackball.as_deref().map_or_else(
            || trackball::TrackballForm::default().config(),
            trackball::TrackballForm::config,
        )
    }

    /// Whether the card has a Camera timing field: a launch monitor whose
    /// shots the cameras cut a clip for (not the trackball's).
    pub(crate) fn has_camera_timing(&self) -> bool {
        self.is_launch_monitor() && flighthook::impact_lead_default_s(&self.monitor_type).is_some()
    }

    /// Whether this is a launch monitor: anything but a swing camera.
    pub(crate) fn is_launch_monitor(&self) -> bool {
        !self.is_camera()
    }

    /// Whether this is a swing camera rather than a launch monitor.
    pub(crate) fn is_camera(&self) -> bool {
        self.monitor_type == "camera"
    }

    /// Whether this device takes an optional IP address. Blank means find
    /// the device on the network, so blank is valid.
    pub(crate) fn has_ip_address(&self) -> bool {
        self.monitor_type == "exputt"
    }

    /// Whether this device takes a TCP `ip:port` address (validated as such).
    pub(crate) fn has_network_address(&self) -> bool {
        self.is_mevo()
    }

    /// Whether this device is a server taking the address it listens on.
    /// Blank means the default, [`OPENCONNECT_BIND`], so blank is valid.
    pub(crate) fn has_bind_address(&self) -> bool {
        self.monitor_type == "openconnect_server"
    }

    /// Whether this device takes an optional BLE address. Blank means
    /// auto-discover by name prefix, so blank is valid.
    pub(crate) fn has_ble_address(&self) -> bool {
        self.is_square()
    }

    /// Whether the Mevo tuning block applies — ball type, tee height, range,
    /// surface height, track percentage, estimated shots. These are all
    /// FlightScope radar settings with no meaning on any other device.
    pub(crate) fn has_mevo_tuning(&self) -> bool {
        self.is_mevo()
    }
}

/// The bind address of an Open Connect server whose bind is blank.
pub(crate) const OPENCONNECT_BIND: &str = "0.0.0.0:921";

/// Placeholder in an empty ExPutt IP Address field.
pub(crate) const EXPUTT_IP_PLACEHOLDER: &str = "empty = auto-detect";

/// Per-integration form entry. The `id` is the map index,
/// `integration_type` encodes which section map it belongs to.
#[derive(Clone)]
pub(crate) struct IntegrationFormEntry {
    pub(crate) id: String,
    pub(crate) integration_type: String, // "openconnect_client" | "pureplay" | "random_club" | "webserver"
    pub(crate) name: String,
    /// Whether the card is on (`enabled`); always on for a web server.
    pub(crate) enabled: bool,
    pub(crate) address: String,
    /// Routing: actor ID for full-swing monitor, or empty = "Any".
    pub(crate) full_monitor: String,
    /// Routing: actor ID for chipping monitor, or empty = "Any".
    pub(crate) chipping_monitor: String,
    /// Routing: actor ID for putting monitor, or empty = "Any".
    pub(crate) putting_monitor: String,
    pub(crate) dirty: bool,
}

/// Unified actor form entry wrapping both device and integration entries.
// One entry per card, a few dozen at most, built and matched in many places:
// boxing the device entry would only add indirection at each of them.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub(crate) enum ActorFormEntry {
    Device(DeviceFormEntry),
    Integration(IntegrationFormEntry),
}

impl ActorFormEntry {
    pub(crate) fn name(&self) -> &str {
        match self {
            ActorFormEntry::Device(d) => &d.name,
            ActorFormEntry::Integration(i) => &i.name,
        }
    }

    pub(crate) fn dirty(&self) -> bool {
        match self {
            ActorFormEntry::Device(d) => d.dirty,
            ActorFormEntry::Integration(i) => i.dirty,
        }
    }

    pub(crate) fn set_dirty(&mut self, v: bool) {
        match self {
            ActorFormEntry::Device(d) => d.dirty = v,
            ActorFormEntry::Integration(i) => i.dirty = v,
        }
    }

    pub(crate) fn type_label(&self) -> &str {
        match self {
            ActorFormEntry::Device(d) => match d.monitor_type.as_str() {
                "mevo" => "Mevo",
                "r10" => "R10",
                "square" => "Square Golf Omni",
                "exputt" => "ExPutt",
                "trackball" => "Trackball",
                "openconnect_server" => "Open Connect Server",
                "mock_monitor" => "Mock",
                "camera" => "Camera",
                _ => &d.monitor_type,
            },
            ActorFormEntry::Integration(i) => match i.integration_type.as_str() {
                "openconnect_client" => "Open Connect Client",
                "pureplay" => "Pureplay Connect",
                "random_club" => "Random Club",
                "webserver" => "Web Server",
                _ => &i.integration_type,
            },
        }
    }

    pub(crate) fn type_tooltip(&self) -> &str {
        match self {
            ActorFormEntry::Device(d) => match d.monitor_type.as_str() {
                "mevo" => "FlightScope Mevo / Mevo+ / Xi — connects via WiFi (TCP)",
                "r10" => "Garmin R10 — auto-detects from system connected Bluetooth devices",
                "square" => {
                    "Square Golf Omni — BLE, no pairing required; leave address blank to auto-discover"
                }
                "exputt" => {
                    "ExPutt RG putting device — WiFi; put the device in its mobile mode on the same network. Leave the IP address blank to find it automatically"
                }
                "trackball" => {
                    "Trackball (alpha) — a trackball or mouse on this computer (Linux): pull the ball back, then push it forward, for a full shot. Every value is made from the swing"
                }
                "openconnect_server" => {
                    "Accepts inbound shots from any launch monitor that speaks GSPro Open Connect (Uneekor, Foresight, SkyTrak). Bind address, default 0.0.0.0:921 — to share a host with GSPro, move GSPConnect to 922 via OpenAPIUseAltPort"
                }
                "mock_monitor" => "Mock launch monitor — generates random shots for testing",
                "camera" => {
                    "Swing camera (beta) — a USB camera on this computer. Records a clip around every shot for replay on the Shots tab"
                }
                _ => "",
            },
            ActorFormEntry::Integration(i) => match i.integration_type.as_str() {
                "openconnect_client" => {
                    "Sends shots to a simulator that accepts GSPro Open Connect (GSPro and Pureplay on 921, Muni Golf on 9210) — connects via TCP"
                }
                "pureplay" => {
                    "Sends shots to Pureplay over Pureplay Connect (port 9760) and takes its club and hand — connects via TCP"
                }
                "random_club" => "Cycles through clubs automatically on each shot",
                "webserver" => "HTTP/WebSocket server for the dashboard UI and API",
                _ => "",
            },
        }
    }

    /// The config section and actor ID prefix: `"mevo"`,
    /// `"openconnect_client"`, ...
    pub(crate) fn type_key(&self) -> &str {
        match self {
            ActorFormEntry::Device(d) => &d.monitor_type,
            ActorFormEntry::Integration(i) => &i.integration_type,
        }
    }

    /// Turn the card on or off in the form.
    fn set_enabled(&mut self, on: bool) {
        match self {
            ActorFormEntry::Device(d) => d.enabled = on,
            ActorFormEntry::Integration(i) => i.enabled = on,
        }
    }

    /// The instance index within its section.
    pub(crate) fn index(&self) -> &str {
        match self {
            ActorFormEntry::Device(d) => &d.id,
            ActorFormEntry::Integration(i) => &i.id,
        }
    }

    /// The actor ID: `"mevo.0"`.
    pub(crate) fn global_id(&self) -> String {
        format!("{}.{}", self.type_key(), self.index())
    }

    /// Sort key: (name, type_label, id) for stable ordering.
    fn sort_key(&self) -> (&str, &str, &str) {
        match self {
            ActorFormEntry::Device(d) => (&d.name, self.type_label(), &d.id),
            ActorFormEntry::Integration(i) => (&i.name, self.type_label(), &i.id),
        }
    }
}

/// A new, unsaved entry from (+) menu `preset` at index `id`, with its
/// type's defaults and the preset's name and address. `None` for a type no
/// (+) menu offers.
fn new_actor_entry(preset: &AddPreset, id: String) -> Option<ActorFormEntry> {
    let AddPreset {
        type_key,
        name,
        address,
        ..
    } = *preset;
    let device = |ball_type: u8| {
        ActorFormEntry::Device(DeviceFormEntry {
            id: id.clone(),
            monitor_type: type_key.into(),
            name: name.into(),
            enabled: true,
            address: address.into(),
            ball_type,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            range_val: "8".into(),
            range_unit: "feet".into(),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: false,
            camera_mode: CameraMode::default(),
            environment: FlightScopeEnvironment::default(),
            spin_source: SpinSource::default(),
            launch_only_spin: format_distance_value(MevoSection::DEFAULT_LAUNCH_ONLY_SPIN),
            impact_lead: String::new(),
            discard_zero_spin: true,
            square: SquareConfig::default(),
            square_impact_mm: std::collections::HashMap::new(),
            // A new FlightScope card states the values it shows.
            mevo: MevoConfig {
                ball_type: Some(ball_type),
                tee_height: Some(MEVO_TEE_HEIGHT),
                range: Some(MEVO_RANGE),
                surface_height: Some(MEVO_SURFACE_HEIGHT),
                track_pct: Some(flighthook::MEVO_DEFAULT_TRACK_PCT),
                use_estimated: Some(false),
                camera_mode: Some(CameraMode::Standard),
                environment: Some(FlightScopeEnvironment::LimitedFlight),
                ..MevoConfig::default()
            },
            mock: MockConfig::default(),
            trackball: (type_key == "trackball").then(Box::default),
            saved_driver: None,
            cam: Box::default(),
            dirty: true,
        })
    };
    let integration = || {
        ActorFormEntry::Integration(IntegrationFormEntry {
            id: id.clone(),
            integration_type: type_key.into(),
            name: name.into(),
            enabled: true,
            address: address.into(),
            full_monitor: String::new(),
            chipping_monitor: String::new(),
            putting_monitor: String::new(),
            dirty: true,
        })
    };
    match type_key {
        "mevo" => Some(device(1)),
        "square" | "r10" | "exputt" | "trackball" | "openconnect_server" | "camera" => {
            Some(device(0))
        }
        "openconnect_client" | "pureplay" => Some(integration()),
        _ => None,
    }
}

/// Pending removal confirmation: the actor's global ID and display name.
pub(crate) struct PendingRemoval {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// What a save writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveTarget {
    /// The global settings: the Global and Simulation cards.
    Global,
    /// One actor's section, by global ID.
    Actor(String),
    /// One actor's removal, by global ID.
    Remove(String),
    /// The whole config: the setup wizard.
    Full,
}

impl SaveTarget {
    /// The `scope` its `POST /api/settings` carries.
    pub(crate) fn scope(&self) -> Option<&str> {
        match self {
            SaveTarget::Global => Some("global"),
            SaveTarget::Actor(id) | SaveTarget::Remove(id) => Some(id),
            SaveTarget::Full => None,
        }
    }
}

/// A posted save waiting for its reply: what it writes and the body sent.
#[derive(Clone)]
struct InFlightSave {
    target: SaveTarget,
    posted: FlighthookConfig,
}

/// A save that failed: what it wrote and why, shown on its card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveError {
    pub(crate) target: SaveTarget,
    pub(crate) message: String,
}

/// Altitude units offered for the simulation.
const ALTITUDE_UNITS: &[(&str, &str)] = &[("feet", "ft"), ("meters", "m")];

fn celsius_to_fahrenheit(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

fn fahrenheit_to_celsius(f: f64) -> f64 {
    (f - 32.0) * 5.0 / 9.0
}

/// Form state for the `[simulation]` section (beta).
///
/// Temperature is edited in °F when `fahrenheit` is set (imperial default
/// units), °C otherwise, and stored as °C rounded to 0.1. Fields left as
/// loaded save their loaded value exactly.
#[derive(Clone)]
pub(crate) struct SimulationForm {
    pub(crate) mode: SimMode,
    pub(crate) altitude_val: String,
    pub(crate) altitude_unit: String,
    pub(crate) temperature_val: String,
    pub(crate) fahrenheit: bool,
    pub(crate) surface: SimSurface,
    pub(crate) auto_chip: bool,
    loaded: SimulationSection,
}

impl Default for SimulationForm {
    fn default() -> Self {
        Self::from_section(&SimulationSection::default(), UnitSystem::default())
    }
}

impl SimulationForm {
    pub(crate) fn from_section(s: &SimulationSection, units: UnitSystem) -> Self {
        let fahrenheit = units == UnitSystem::Imperial;
        // A zero altitude carries no unit of its own, so show it in the
        // user's default unit system rather than whatever unit it happened
        // to be stored in.
        let altitude_unit = if units == UnitSystem::Metric && s.altitude.as_meters() == 0.0 {
            "meters"
        } else {
            s.altitude.unit_key()
        };
        Self {
            mode: s.mode,
            altitude_val: format_distance_value(s.altitude.value()),
            altitude_unit: altitude_unit.to_string(),
            temperature_val: Self::temperature_text(s.temperature_c, fahrenheit),
            fahrenheit,
            surface: s.surface,
            auto_chip: s.auto_chip,
            loaded: s.clone(),
        }
    }

    fn temperature_text(c: f64, fahrenheit: bool) -> String {
        format_distance_value(if fahrenheit {
            celsius_to_fahrenheit(c)
        } else {
            c
        })
    }

    /// Switch the temperature display unit, converting the typed value.
    pub(crate) fn set_fahrenheit(&mut self, fahrenheit: bool) {
        if fahrenheit == self.fahrenheit {
            return;
        }
        if let Some(c) = self.temperature_c() {
            self.temperature_val = Self::temperature_text(c, fahrenheit);
        }
        self.fahrenheit = fahrenheit;
    }

    pub(crate) fn temperature_unit(&self) -> &'static str {
        if self.fahrenheit { "°F" } else { "°C" }
    }

    fn altitude(&self) -> Option<Distance> {
        let loaded = &self.loaded.altitude;
        if self.altitude_val == format_distance_value(loaded.value())
            && self.altitude_unit == loaded.unit_key()
        {
            return Some(*loaded);
        }
        let v: f64 = self.altitude_val.trim().parse().ok()?;
        // Zero is zero regardless of unit: an untouched zero altitude shown
        // in a different unit than it was loaded in (see `from_section`) is
        // still unchanged, and must save as the exact value loaded.
        if v == 0.0 && loaded.as_meters() == 0.0 {
            return Some(*loaded);
        }
        Some(Distance::from_value_and_unit(v, &self.altitude_unit))
    }

    fn temperature_c(&self) -> Option<f64> {
        if self.temperature_val
            == Self::temperature_text(self.loaded.temperature_c, self.fahrenheit)
        {
            return Some(self.loaded.temperature_c);
        }
        let v: f64 = self.temperature_val.trim().parse().ok()?;
        let c = if self.fahrenheit {
            fahrenheit_to_celsius(v)
        } else {
            v
        };
        Some((c * 10.0).round() / 10.0)
    }

    /// The section to save, or `None` while a field is unparsable or out of
    /// range.
    pub(crate) fn to_section(&self) -> Option<SimulationSection> {
        let s = SimulationSection {
            mode: self.mode,
            altitude: self.altitude()?,
            temperature_c: self.temperature_c()?,
            surface: self.surface,
            auto_chip: self.auto_chip,
        };
        s.environment_error().is_none().then_some(s)
    }

    /// The section to save, falling back to the loaded one while invalid.
    fn to_section_or_loaded(&self) -> SimulationSection {
        self.to_section().unwrap_or_else(|| self.loaded.clone())
    }
}

/// Form state for the `[replay]` section (beta). Fields left as loaded save
/// their loaded value exactly; the clip folder is not edited here.
#[derive(Clone)]
pub(crate) struct ReplayForm {
    pub(crate) keep_shots: String,
    pub(crate) pre_roll_s: String,
    pub(crate) post_roll_s: String,
    pub(crate) start_before_impact_s: String,
    pub(crate) shot_cards: bool,
    loaded: ReplaySection,
}

impl Default for ReplayForm {
    fn default() -> Self {
        Self::from_section(&ReplaySection::default())
    }
}

impl ReplayForm {
    pub(crate) fn from_section(s: &ReplaySection) -> Self {
        Self {
            keep_shots: s.keep_shots.to_string(),
            pre_roll_s: format_distance_value(s.pre_roll_s),
            post_roll_s: format_distance_value(s.post_roll_s),
            start_before_impact_s: format_distance_value(s.start_before_impact_s),
            shot_cards: s.shot_cards,
            loaded: s.clone(),
        }
    }

    /// The configured clip folder, if the section names one.
    pub(crate) fn clip_dir(&self) -> Option<&str> {
        self.loaded.clip_dir.as_deref()
    }

    /// Seconds before the estimated impact where a replay opens, as saved
    /// (held to its valid range).
    pub(crate) fn start_before_impact_s(&self) -> f64 {
        let s = self.loaded.start_before_impact_s;
        if s.is_finite() {
            s.clamp(0.0, ReplaySection::MAX_START_BEFORE_IMPACT_S)
        } else {
            ReplaySection::DEFAULT_START_BEFORE_IMPACT_S
        }
    }

    /// A seconds field's value: the loaded value while the text is unchanged.
    fn seconds(text: &str, loaded: f64) -> Option<f64> {
        if text == format_distance_value(loaded) {
            return Some(loaded);
        }
        text.trim().parse().ok()
    }

    /// The section to save, or `None` while a field is unparsable or out of
    /// range ([`ReplaySection::error`]).
    pub(crate) fn to_section(&self) -> Option<ReplaySection> {
        let s = ReplaySection {
            keep_shots: self.keep_shots.trim().parse().ok()?,
            pre_roll_s: Self::seconds(&self.pre_roll_s, self.loaded.pre_roll_s)?,
            post_roll_s: Self::seconds(&self.post_roll_s, self.loaded.post_roll_s)?,
            start_before_impact_s: Self::seconds(
                &self.start_before_impact_s,
                self.loaded.start_before_impact_s,
            )?,
            clip_dir: self.loaded.clip_dir.clone(),
            shot_cards: self.shot_cards,
        };
        s.error().is_none().then_some(s)
    }

    /// The section to save, falling back to the loaded one while invalid.
    fn to_section_or_loaded(&self) -> ReplaySection {
        self.to_section().unwrap_or_else(|| self.loaded.clone())
    }
}

/// Form state for the `[body_tracking]` section (alpha): every field of the
/// Body tracking card.
///
/// A number field left as loaded saves its loaded value exactly.
#[derive(Clone, Default)]
pub(crate) struct BodyTrackingForm {
    pub(crate) mode: BodyTrackingMode,
    pub(crate) model: PoseModel,
    pub(crate) show_skeleton: bool,
    pub(crate) min_score: String,
    pub(crate) smoothing: String,
    pub(crate) detect_every: String,
    loaded: BodyTrackingSection,
}

impl BodyTrackingForm {
    pub(crate) fn from_section(s: &BodyTrackingSection) -> Self {
        Self {
            mode: s.mode,
            model: s.model,
            show_skeleton: s.show_skeleton,
            min_score: format_distance_value(s.min_score),
            smoothing: format_distance_value(s.smoothing),
            detect_every: s.detect_every.to_string(),
            loaded: s.clone(),
        }
    }

    /// A decimal field's value: the loaded value while the text is
    /// unchanged.
    fn decimal(text: &str, loaded: f64) -> Option<f64> {
        if text == format_distance_value(loaded) {
            return Some(loaded);
        }
        text.trim().parse().ok()
    }

    /// Whether the defaults with `set` applied are valid: one field checked
    /// by [`BodyTrackingSection::error`], as the server checks it.
    fn valid(set: impl FnOnce(&mut BodyTrackingSection)) -> bool {
        let mut s = BodyTrackingSection::default();
        set(&mut s);
        s.error().is_none()
    }

    /// The minimum score to save, or `None` while it is not a number from 0
    /// to 1.
    pub(crate) fn min_score_value(&self) -> Option<f64> {
        let v = Self::decimal(&self.min_score, self.loaded.min_score)?;
        Self::valid(|s| s.min_score = v).then_some(v)
    }

    /// The live smoothing to save (Hz), or `None` while it is neither 0 nor
    /// a number in range.
    pub(crate) fn smoothing_value(&self) -> Option<f64> {
        let v = Self::decimal(&self.smoothing, self.loaded.smoothing)?;
        Self::valid(|s| s.smoothing = v).then_some(v)
    }

    /// The detection interval to save (frames), or `None` while it is not a
    /// whole number in range.
    pub(crate) fn detect_every_value(&self) -> Option<u32> {
        let v = self.detect_every.trim().parse().ok()?;
        Self::valid(|s| s.detect_every = v).then_some(v)
    }

    /// The section to save, or `None` while a field is unparsable or out
    /// of range.
    pub(crate) fn to_section(&self) -> Option<BodyTrackingSection> {
        let s = BodyTrackingSection {
            mode: self.mode,
            model: self.model,
            min_score: self.min_score_value()?,
            smoothing: self.smoothing_value()?,
            detect_every: self.detect_every_value()?,
            show_skeleton: self.show_skeleton,
            legacy_golfer_height: None,
        };
        s.error().is_none().then_some(s)
    }

    /// The section to save, falling back to the loaded one while invalid.
    fn to_section_or_loaded(&self) -> BodyTrackingSection {
        self.to_section().unwrap_or_else(|| self.loaded.clone())
    }
}

/// Form state for the `[player]` section: the Player card.
///
/// The height is edited in inches when `inches` is set (imperial default
/// units), centimeters otherwise, and stored in centimeters rounded to 0.1.
/// Empty is no height; a height left as loaded saves its loaded value
/// exactly.
#[derive(Clone, Default)]
pub(crate) struct PlayerForm {
    pub(crate) sex: Sex,
    pub(crate) height: String,
    pub(crate) inches: bool,
    loaded: PlayerSection,
}

impl PlayerForm {
    pub(crate) fn from_section(s: &PlayerSection, units: UnitSystem) -> Self {
        let inches = units == UnitSystem::Imperial;
        Self {
            sex: s.sex,
            height: Self::height_text(s.height, inches),
            inches,
            loaded: s.clone(),
        }
    }

    fn height_text(cm: Option<f64>, inches: bool) -> String {
        cm.map_or_else(String::new, |cm| {
            let v = if inches { cm / 2.54 } else { cm };
            format_distance_value((v * 10.0).round() / 10.0)
        })
    }

    /// The height's unit as the card shows it.
    pub(crate) fn height_unit(&self) -> &'static str {
        if self.inches { "in" } else { "cm" }
    }

    /// Switch the height's unit, converting the typed value.
    pub(crate) fn set_inches(&mut self, inches: bool) {
        if inches == self.inches {
            return;
        }
        if let Some(cm) = self.height_cm() {
            self.height = Self::height_text(cm, inches);
        }
        self.inches = inches;
    }

    /// The height to save in centimeters (`Some(None)`: no height), or
    /// `None` while the text is not a number.
    fn height_cm(&self) -> Option<Option<f64>> {
        if self.height == Self::height_text(self.loaded.height, self.inches) {
            return Some(self.loaded.height);
        }
        let text = self.height.trim();
        if text.is_empty() {
            return Some(None);
        }
        let v: f64 = text.parse().ok()?;
        let cm = if self.inches { v * 2.54 } else { v };
        Some(Some((cm * 10.0).round() / 10.0))
    }

    /// Whether the height is empty or a number in range.
    pub(crate) fn height_valid(&self) -> bool {
        self.height_cm().is_some_and(|height| {
            PlayerSection {
                height,
                ..PlayerSection::default()
            }
            .error()
            .is_none()
        })
    }

    /// The section to save, or `None` while the height is unparsable or out
    /// of range.
    pub(crate) fn to_section(&self) -> Option<PlayerSection> {
        let s = PlayerSection {
            sex: self.sex,
            height: self.height_cm()?,
        };
        s.error().is_none().then_some(s)
    }

    /// The section to save, falling back to the loaded one while invalid.
    fn to_section_or_loaded(&self) -> PlayerSection {
        self.to_section().unwrap_or_else(|| self.loaded.clone())
    }
}

/// Hover text for a simulation mode in the settings dropdown.
fn sim_mode_hover(mode: SimMode) -> &'static str {
    match mode {
        SimMode::Off => "Show only what the launch monitor reports.",
        SimMode::On => {
            "Simulate the carry, apex, offline, roll and total the launch monitor doesn't report."
        }
        SimMode::Force => {
            "Replace the carry, apex, offline, roll and total the launch monitor reports with flighthook's simulation of its launch numbers. Launch numbers are never changed."
        }
    }
}

/// One form entry per instance in `s`, in no particular order.
fn actor_entries(s: &FlighthookConfig) -> Vec<ActorFormEntry> {
    let mut actors = Vec::new();
    for (id, section) in &s.mevo {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_mevo(
            id, section,
        )));
    }
    for (id, section) in &s.square {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_square(
            id, section,
        )));
    }
    for (id, section) in &s.r10 {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_r10(
            id, section,
        )));
    }
    for (id, section) in &s.exputt {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_exputt(
            id, section,
        )));
    }
    for (id, section) in &s.trackball {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_trackball(
            id, section,
        )));
    }
    for (id, section) in &s.openconnect_server {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_openconnect(
            id, section,
        )));
    }
    for (id, section) in &s.mock_monitor {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_mock(
            id, section,
        )));
    }
    for (id, section) in &s.camera {
        actors.push(ActorFormEntry::Device(DeviceFormEntry::from_camera(
            id, section,
        )));
    }
    for (id, section) in &s.openconnect_client {
        actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
            id: id.clone(),
            integration_type: "openconnect_client".into(),
            name: section.name.clone(),
            enabled: section.enabled,
            address: section.address.clone().unwrap_or_default(),
            full_monitor: section.full_monitor.clone().unwrap_or_default(),
            chipping_monitor: section.chipping_monitor.clone().unwrap_or_default(),
            putting_monitor: section.putting_monitor.clone().unwrap_or_default(),
            dirty: false,
        }));
    }
    for (id, section) in &s.pureplay {
        actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
            id: id.clone(),
            integration_type: "pureplay".into(),
            name: section.name.clone(),
            enabled: section.enabled,
            address: section.address.clone().unwrap_or_default(),
            full_monitor: section.full_monitor.clone().unwrap_or_default(),
            chipping_monitor: section.chipping_monitor.clone().unwrap_or_default(),
            putting_monitor: section.putting_monitor.clone().unwrap_or_default(),
            dirty: false,
        }));
    }
    for (id, section) in &s.random_club {
        actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
            id: id.clone(),
            integration_type: "random_club".into(),
            name: section.name.clone(),
            enabled: section.enabled,
            address: String::new(),
            full_monitor: String::new(),
            chipping_monitor: String::new(),
            putting_monitor: String::new(),
            dirty: false,
        }));
    }
    for (id, section) in &s.webserver {
        actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
            id: id.clone(),
            integration_type: "webserver".into(),
            name: section.name.clone(),
            enabled: true,
            address: section.bind.clone(),
            full_monitor: String::new(),
            chipping_monitor: String::new(),
            putting_monitor: String::new(),
            dirty: false,
        }));
    }
    actors
}

/// The settings-only cards of the General group. They save, and cancel,
/// together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeneralCard {
    Global,
    Simulation,
    Replay,
    BodyTracking,
    Player,
}

/// Settings form state.
#[derive(Clone)]
pub(crate) struct SettingsForm {
    pub(crate) default_units: UnitSystem,
    pub(crate) shot_grouping: ShotGrouping,
    pub(crate) chipping_clubs: Vec<Club>,
    pub(crate) putting_clubs: Vec<Club>,
    pub(crate) simulation: SimulationForm,
    pub(crate) replay: ReplayForm,
    pub(crate) body_tracking: BodyTrackingForm,
    pub(crate) player: PlayerForm,
    /// The Global, Simulation, Replay, Body tracking and Player cards (one
    /// save) have unsaved edits.
    pub(crate) global_dirty: bool,
    /// Which of the cards the unsaved Global edits were made on, by
    /// [`GeneralCard`]; meaningful only while `global_dirty`.
    general_edited: [bool; 5],
    pub(crate) actors: Vec<ActorFormEntry>,
    pub(crate) loaded: bool,
    /// The save posted and not answered yet.
    save: Option<InFlightSave>,
    /// The last save's failure, until the next save, or a reload that finds
    /// its edits saved.
    pub(crate) save_error: Option<SaveError>,
    /// A failed save is waiting for the settings to be read again, to find
    /// out what the server holds.
    resync: bool,
    /// When the settings were last requested (egui time, s); `None` until
    /// the first request, and again after a failed save.
    pub(crate) fetched_at: Option<f64>,
    /// The server's config as of the last load or save: the base of every
    /// request and what Cancel goes back to.
    original_config: Option<FlighthookConfig>,
}

/// Least time between two settings requests while they have not loaded (s).
const SETTINGS_RETRY_S: f64 = 2.0;

impl Default for SettingsForm {
    fn default() -> Self {
        Self {
            default_units: UnitSystem::default(),
            shot_grouping: ShotGrouping::default(),
            chipping_clubs: flighthook::default_chipping_clubs(),
            putting_clubs: flighthook::default_putting_clubs(),
            simulation: SimulationForm::default(),
            replay: ReplayForm::default(),
            body_tracking: BodyTrackingForm::from_section(&BodyTrackingSection::default()),
            player: PlayerForm::from_section(&PlayerSection::default(), UnitSystem::default()),
            global_dirty: false,
            general_edited: [false; 5],
            actors: Vec::new(),
            loaded: false,
            save: None,
            save_error: None,
            resync: false,
            fetched_at: None,
            original_config: None,
        }
    }
}

impl SettingsForm {
    /// Whether to request the settings at `now`: while they have not
    /// loaded, or a failed save waits for them, at most once every
    /// [`SETTINGS_RETRY_S`]. Records the request.
    pub(crate) fn fetch_due(&mut self, now: f64) -> bool {
        if (self.loaded && !self.resync)
            || self.fetched_at.is_some_and(|t| now - t < SETTINGS_RETRY_S)
        {
            return false;
        }
        self.fetched_at = Some(now);
        true
    }

    /// Read the settings again: the server's config may have changed.
    pub(crate) fn request_reload(&mut self) {
        if self.loaded {
            self.resync = true;
            self.fetched_at = None;
        }
    }

    /// Take the server's config `s`. Every entry without unsaved edits, and
    /// the global settings unless they have unsaved edits, become the
    /// server's. An entry (or the global settings) with unsaved edits keeps
    /// them, and turns clean when the server already holds exactly those
    /// values: a save whose reply was lost. `s` becomes the saved config.
    pub(crate) fn load_from(&mut self, s: &FlighthookConfig) {
        if self.global_dirty && global_matches(&self.build_global_request(), s) {
            self.global_dirty = false;
        }
        if !self.global_dirty {
            self.default_units = s.default_units;
            self.shot_grouping = s.shot_grouping;
            self.chipping_clubs = s.chipping_clubs.clone();
            self.putting_clubs = s.putting_clubs.clone();
            self.simulation = SimulationForm::from_section(&s.simulation, s.default_units);
            self.replay = ReplayForm::from_section(&s.replay);
            self.body_tracking = BodyTrackingForm::from_section(&s.body_tracking);
            self.player = PlayerForm::from_section(&s.player, s.default_units);
            self.general_edited = [false; 5];
        }

        let edited: Vec<ActorFormEntry> = std::mem::take(&mut self.actors)
            .into_iter()
            .filter(ActorFormEntry::dirty)
            .collect();
        let mut actors: Vec<ActorFormEntry> = actor_entries(s)
            .into_iter()
            .filter(|a| !edited.iter().any(|e| e.global_id() == a.global_id()))
            .collect();
        for mut entry in edited {
            if entry_matches(s, &entry) {
                entry.set_dirty(false);
            }
            actors.push(entry);
        }
        actors.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        self.actors = actors;

        self.original_config = Some(s.clone());
        self.loaded = true;
        self.resync = false;
        if self
            .save_error
            .as_ref()
            .is_some_and(|e| self.settled(&e.target))
        {
            self.save_error = None;
        }
    }

    /// Whether nothing `target` would write is still unsaved.
    fn settled(&self, target: &SaveTarget) -> bool {
        match target {
            SaveTarget::Global => !self.global_dirty,
            SaveTarget::Actor(id) => self
                .actors
                .iter()
                .all(|a| a.global_id() != *id || !a.dirty()),
            SaveTarget::Remove(id) => self.actors.iter().all(|a| a.global_id() != *id),
            SaveTarget::Full => !self.dirty(),
        }
    }

    /// Whether the General cards can be saved: the simulation, replay,
    /// body tracking and player fields are in range.
    pub(crate) fn general_valid(&self) -> bool {
        self.simulation.to_section().is_some()
            && self.replay.to_section().is_some()
            && self.body_tracking.to_section().is_some()
            && self.player.to_section().is_some()
    }

    /// Clubs on the Global card are limited to the Chipping/Putting
    /// picker's options; a club carried over in the config that isn't offered
    /// (e.g. a hand-edited `9I`) is dropped on save.
    fn filtered_chipping_clubs(&self) -> Vec<Club> {
        let options = flighthook::chipping_club_options();
        self.chipping_clubs
            .iter()
            .copied()
            .filter(|c| options.contains(c))
            .collect()
    }

    fn filtered_putting_clubs(&self) -> Vec<Club> {
        let options = flighthook::putting_club_options();
        self.putting_clubs
            .iter()
            .copied()
            .filter(|c| options.contains(c))
            .collect()
    }

    /// The whole form as a config.
    pub(crate) fn to_request(&self) -> FlighthookConfig {
        let mut config = FlighthookConfig {
            default_units: self.default_units,
            chipping_clubs: self.filtered_chipping_clubs(),
            putting_clubs: self.filtered_putting_clubs(),
            shot_grouping: self.shot_grouping,
            simulation: self.simulation.to_section_or_loaded(),
            replay: self.replay.to_section_or_loaded(),
            body_tracking: self.body_tracking.to_section_or_loaded(),
            player: self.player.to_section_or_loaded(),
            webserver: HashMap::new(),
            mevo: HashMap::new(),
            r10: HashMap::new(),
            square: HashMap::new(),
            exputt: HashMap::new(),
            trackball: HashMap::new(),
            mock_monitor: HashMap::new(),
            openconnect_server: HashMap::new(),
            openconnect_client: HashMap::new(),
            pureplay: HashMap::new(),
            random_club: HashMap::new(),
            camera: HashMap::new(),
        };
        for actor in &self.actors {
            apply_actor_to_config(&mut config, actor);
        }
        config
    }

    /// Append a new, unsaved entry from (+) menu `preset` at the lowest
    /// index its type does not use yet. Returns its actor ID.
    pub(crate) fn add_preset(&mut self, preset: &AddPreset) -> Option<String> {
        let existing: Vec<&str> = self
            .actors
            .iter()
            .filter(|a| a.type_key() == preset.type_key)
            .map(ActorFormEntry::index)
            .collect();
        let id = next_index(&existing);
        let entry = new_actor_entry(preset, id)?;
        let global_id = entry.global_id();
        // A duplicate may be created, but not enabled: it starts off when an
        // enabled card of its type has its address.
        let mut probe = self.saved_config();
        apply_actor_to_config(&mut probe, &entry);
        let mut entry = entry;
        if probe.address_clash(&global_id).is_some() {
            entry.set_enabled(false);
        }
        self.actors.push(entry);
        Some(global_id)
    }

    /// [`add_preset`](Self::add_preset) with the first (+) menu preset of
    /// `type_key`.
    #[cfg(test)]
    pub(crate) fn add_actor(&mut self, type_key: &str) -> Option<String> {
        let preset = crate::panels::cards::CardGroup::ALL
            .iter()
            .flat_map(|g| crate::panels::cards::add_options(*g))
            .find(|p| p.type_key == type_key)?;
        self.add_preset(preset)
    }

    /// Whether anything in the form has unsaved edits.
    pub(crate) fn dirty(&self) -> bool {
        self.global_dirty || self.actors.iter().any(ActorFormEntry::dirty)
    }

    /// Whether a save has been posted and not answered yet.
    pub(crate) fn saving(&self) -> bool {
        self.save.is_some()
    }

    /// The save that is out: what it writes and its request body.
    #[cfg(test)]
    pub(crate) fn posted(&self) -> Option<(&SaveTarget, &FlighthookConfig)> {
        self.save.as_ref().map(|s| (&s.target, &s.posted))
    }

    /// The saved cameras, as of the last load or save: actor ID and section,
    /// by index.
    pub(crate) fn saved_cameras(&self) -> Vec<(String, CameraSection)> {
        let Some(config) = &self.original_config else {
            return Vec::new();
        };
        let mut cameras: Vec<(&String, &CameraSection)> = config.camera.iter().collect();
        cameras.sort_by(|(a, _), (b, _)| {
            (a.parse::<u64>().ok(), a.as_str()).cmp(&(b.parse::<u64>().ok(), b.as_str()))
        });
        cameras
            .into_iter()
            .map(|(index, section)| (format!("camera.{index}"), section.clone()))
            .collect()
    }

    /// The saved cameras that are on ([`Self::saved_cameras`] without the
    /// ones turned off): the ones that record and have a live picture.
    pub(crate) fn cameras_on(&self) -> Vec<(String, CameraSection)> {
        let mut cameras = self.saved_cameras();
        cameras.retain(|(_, section)| section.enabled);
        cameras
    }

    /// The rotation camera `actor` (e.g. `camera.0`) is shown with, in
    /// clockwise degrees: its saved `rotation_deg` in effect. `None` until
    /// the settings load, and for a camera that is not configured.
    /// What a shot's grades depend on besides its clip: the saved Player
    /// card (sex and height). Empty before the settings load.
    pub(crate) fn grades_key(&self) -> String {
        self.original_config
            .as_ref()
            .map(|c| format!("{:?}", c.player.in_effect()))
            .unwrap_or_default()
    }

    pub(crate) fn saved_rotation(&self, actor: &str) -> Option<u16> {
        let index = actor.strip_prefix("camera.")?;
        let section = self.original_config.as_ref()?.camera.get(index)?;
        Some(flighthook::rotation_in_effect_deg(section.rotation_deg))
    }

    /// Whether the Devices tab is locked: something has unsaved edits, or a
    /// save is out. Only the dirty cards stay editable, and only while no
    /// save is out.
    pub(crate) fn locked(&self) -> bool {
        self.dirty() || self.saving()
    }

    /// The form index of actor `global_id`.
    pub(crate) fn position(&self, global_id: &str) -> Option<usize> {
        self.actors.iter().position(|a| a.global_id() == global_id)
    }

    /// Record a save of `target` as posted and return its request body.
    pub(crate) fn begin_save(&mut self, target: SaveTarget) -> FlighthookConfig {
        let posted = match &target {
            SaveTarget::Global => self.build_global_request(),
            SaveTarget::Actor(id) => self.build_actor_request(id),
            SaveTarget::Remove(id) => {
                let mut config = self.saved_config();
                set_section(&mut config, None, id);
                config
            }
            SaveTarget::Full => self.to_request(),
        };
        self.begin_save_with(target, posted.clone());
        posted
    }

    /// Record a save of `target` with body `posted` as posted.
    pub(crate) fn begin_save_with(&mut self, target: SaveTarget, posted: FlighthookConfig) {
        self.save_error = None;
        self.save = Some(InFlightSave { target, posted });
    }

    /// Settle the posted save with its reply: `Ok` once the server applied
    /// it. A failure keeps every edit, records the error for the card and
    /// asks for the settings again, since the server may have applied the
    /// save even though its reply was lost. Returns the global ID of the
    /// actor a successful removal took out of the form.
    pub(crate) fn finish_save(&mut self, result: Result<(), String>) -> Option<String> {
        let save = self.save.take()?;
        match result {
            Ok(()) => {
                self.save_error = None;
                self.apply_saved(save)
            }
            Err(message) => {
                self.save_error = Some(SaveError {
                    target: save.target,
                    message,
                });
                self.resync = true;
                self.fetched_at = None;
                None
            }
        }
    }

    /// The server applied `save`: it becomes part of the saved config, and
    /// the edits it carried turn clean ([`Self::load_from`]). An edit that
    /// differs from what was posted, or an entry the server does not hold,
    /// stays dirty.
    fn apply_saved(&mut self, save: InFlightSave) -> Option<String> {
        let InFlightSave { target, posted } = save;
        let mut saved = self.saved_config();
        match &target {
            SaveTarget::Global => copy_global(&mut saved, &posted),
            SaveTarget::Actor(id) => set_section(&mut saved, Some(&posted), id),
            SaveTarget::Remove(id) => {
                set_section(&mut saved, Some(&posted), id);
                if let Some(idx) = self.position(id) {
                    self.actors.remove(idx);
                }
            }
            SaveTarget::Full => saved = posted,
        }
        self.load_from(&saved);
        match target {
            SaveTarget::Remove(id) if self.position(&id).is_none() => Some(id),
            _ => None,
        }
    }

    /// The saved config, or the form's own while nothing has loaded.
    fn saved_config(&self) -> FlighthookConfig {
        self.original_config
            .clone()
            .unwrap_or_else(|| self.to_request())
    }

    /// Record an edit on a General card.
    pub(crate) fn mark_general(&mut self, card: GeneralCard) {
        if !self.global_dirty {
            self.general_edited = [false; 5];
        }
        self.general_edited[card as usize] = true;
        self.global_dirty = true;
    }

    /// Whether `card` shows Save and Cancel: it has unsaved edits. Global
    /// edits made elsewhere than on any General card count as the Global
    /// card's.
    pub(crate) fn general_card_dirty(&self, card: GeneralCard) -> bool {
        if !self.global_dirty {
            return false;
        }
        match card {
            GeneralCard::Global => {
                self.general_edited[GeneralCard::Global as usize]
                    || !(self.general_edited[GeneralCard::Simulation as usize]
                        || self.general_edited[GeneralCard::Replay as usize]
                        || self.general_edited[GeneralCard::BodyTracking as usize]
                        || self.general_edited[GeneralCard::Player as usize])
            }
            GeneralCard::Simulation
            | GeneralCard::Replay
            | GeneralCard::BodyTracking
            | GeneralCard::Player => self.general_edited[card as usize],
        }
    }

    /// Put both General cards back to the saved config.
    pub(crate) fn cancel_general(&mut self) {
        let saved = self.original_config.clone().unwrap_or_default();
        self.default_units = saved.default_units;
        self.shot_grouping = saved.shot_grouping;
        self.chipping_clubs = saved.chipping_clubs.clone();
        self.putting_clubs = saved.putting_clubs.clone();
        self.simulation = SimulationForm::from_section(&saved.simulation, saved.default_units);
        self.replay = ReplayForm::from_section(&saved.replay);
        self.body_tracking = BodyTrackingForm::from_section(&saved.body_tracking);
        self.player = PlayerForm::from_section(&saved.player, saved.default_units);
        self.global_dirty = false;
        self.general_edited = [false; 5];
        if self.save_error.as_ref().map(|e| &e.target) == Some(&SaveTarget::Global) {
            self.save_error = None;
        }
    }

    /// The saved config's entry for actor `global_id`.
    fn saved_entry(&self, global_id: &str) -> Option<ActorFormEntry> {
        actor_entries(self.original_config.as_ref()?)
            .into_iter()
            .find(|a| a.global_id() == global_id)
    }

    /// Put the entry at `idx` back to the saved config, or drop it if it is
    /// a new card. Returns whether it was dropped.
    pub(crate) fn cancel_actor(&mut self, idx: usize) -> bool {
        let Some(actor) = self.actors.get(idx) else {
            return false;
        };
        let global_id = actor.global_id();
        let removed = match self.saved_entry(&global_id) {
            Some(saved) => {
                self.actors[idx] = saved;
                false
            }
            None => {
                self.actors.remove(idx);
                true
            }
        };
        if self
            .save_error
            .as_ref()
            .is_some_and(|e| e.target.scope() == Some(global_id.as_str()))
        {
            self.save_error = None;
        }
        removed
    }

    /// Take the grouping mode the server reports (it also changes outside
    /// this form): into the loaded config, and into the field unless the
    /// Global section has unsaved edits.
    pub(crate) fn sync_shot_grouping(&mut self, mode: ShotGrouping) {
        if let Some(orig) = &mut self.original_config {
            orig.shot_grouping = mode;
        }
        if !self.global_dirty {
            self.shot_grouping = mode;
        }
    }

    /// The saved config with the form's global settings.
    pub(crate) fn build_global_request(&self) -> FlighthookConfig {
        let mut config = self.saved_config();
        config.default_units = self.default_units;
        config.shot_grouping = self.shot_grouping;
        config.chipping_clubs = self.filtered_chipping_clubs();
        config.putting_clubs = self.filtered_putting_clubs();
        config.simulation = self.simulation.to_section_or_loaded();
        config.replay = self.replay.to_section_or_loaded();
        config.body_tracking = self.body_tracking.to_section_or_loaded();
        config.player = self.player.to_section_or_loaded();
        config
    }

    /// Whether the saved settings offer Auto-chip. False until the settings
    /// have loaded, so a disabled chip never flashes at startup.
    pub(crate) fn auto_chip_enabled(&self) -> bool {
        self.original_config
            .as_ref()
            .is_some_and(|c| c.simulation.auto_chip)
    }

    /// Whether the saved settings show the camera cards in the Shots tab's
    /// card row. True until the settings have loaded, as the default is.
    pub(crate) fn shot_cards_enabled(&self) -> bool {
        self.original_config
            .as_ref()
            .is_none_or(|c| c.replay.shot_cards)
    }

    /// The saved `[body_tracking]` settings in effect (defaults until the
    /// settings have loaded).
    pub(crate) fn saved_body_tracking(&self) -> BodyTrackingSection {
        self.original_config
            .as_ref()
            .map(|c| c.body_tracking.in_effect())
            .unwrap_or_default()
    }

    /// The saved config with `body_tracking` in place of its own: what a
    /// save outside the card's Save sends (`scope=body_tracking`, which
    /// writes only that section).
    pub(crate) fn tuning_request(&self, body_tracking: &BodyTrackingSection) -> FlighthookConfig {
        let mut config = self.saved_config();
        config.body_tracking = body_tracking.clone();
        config
    }

    /// The saved config with the form's entry for actor `global_id`.
    pub(crate) fn build_actor_request(&self, global_id: &str) -> FlighthookConfig {
        let mut config = self.saved_config();
        if let Some(idx) = self.position(global_id) {
            let live = config.clone();
            apply_actor_to_config(&mut config, &self.actors[idx]);
            // A card saved on an address another enabled card has is saved
            // off.
            config.demote_clashing(Some(&live));
        }
        config
    }

    /// Why card `global_id`, which is off in the saved settings, cannot be
    /// turned on: an enabled card of its type has its address. `None` for a
    /// card that is on, or that has no clash.
    pub(crate) fn enable_clash(&self, global_id: &str) -> Option<flighthook::AddressClash> {
        let saved = self.original_config.as_ref()?;
        (!saved.actor_enabled(global_id))
            .then(|| saved.address_clash(global_id))
            .flatten()
    }
    /// Whether card `global_id` is off: in the save that is out, else in
    /// the saved settings. False for a card the settings don't hold.
    pub(crate) fn turned_off(&self, global_id: &str) -> bool {
        self.save
            .as_ref()
            .map(|s| &s.posted)
            .or(self.original_config.as_ref())
            .is_some_and(|c| c.disabled_actors().iter().any(|id| id == global_id))
    }

    /// Record a save that turns card `global_id` on or off and nothing
    /// else, scoped to it, and return its request body: the saved settings
    /// with only that card's `enabled` changed. `None` for a card that has
    /// no such switch or that the saved settings don't hold.
    pub(crate) fn begin_enable(&mut self, global_id: &str, on: bool) -> Option<FlighthookConfig> {
        let mut posted = self.saved_config();
        if !posted.set_actor_enabled(global_id, on) {
            return None;
        }
        self.begin_save_with(SaveTarget::Actor(global_id.to_owned()), posted.clone());
        Some(posted)
    }
}

/// Whether a card of actor type `type_key` has the on/off switch: every
/// launch monitor, camera and output, not the web server.
pub(crate) fn switchable(type_key: &str) -> bool {
    type_key != "webserver"
}

/// Whether `a` and `b` hold the same global settings.
fn global_matches(a: &FlighthookConfig, b: &FlighthookConfig) -> bool {
    a.default_units == b.default_units
        && a.chipping_clubs == b.chipping_clubs
        && a.putting_clubs == b.putting_clubs
        && a.shot_grouping == b.shot_grouping
        && a.simulation == b.simulation
        && a.replay == b.replay
        && a.body_tracking == b.body_tracking
        && a.player == b.player
}

/// Copy `from`'s global settings into `to`.
fn copy_global(to: &mut FlighthookConfig, from: &FlighthookConfig) {
    to.default_units = from.default_units;
    to.chipping_clubs.clone_from(&from.chipping_clubs);
    to.putting_clubs.clone_from(&from.putting_clubs);
    to.shot_grouping = from.shot_grouping;
    to.simulation = from.simulation.clone();
    to.replay = from.replay.clone();
    to.body_tracking = from.body_tracking.clone();
    to.player = from.player.clone();
}

/// Set `to`'s section for actor `global_id` to `from`'s: copied, or removed
/// when `from` is `None` or has none.
fn set_section(to: &mut FlighthookConfig, from: Option<&FlighthookConfig>, global_id: &str) {
    fn set<T: Clone>(to: &mut HashMap<String, T>, from: Option<&T>, index: &str) {
        match from {
            Some(section) => {
                to.insert(index.to_string(), section.clone());
            }
            None => {
                to.remove(index);
            }
        }
    }
    let Some((type_key, i)) = global_id.split_once('.') else {
        return;
    };
    match type_key {
        "webserver" => set(&mut to.webserver, from.and_then(|f| f.webserver.get(i)), i),
        "mevo" => set(&mut to.mevo, from.and_then(|f| f.mevo.get(i)), i),
        "r10" => set(&mut to.r10, from.and_then(|f| f.r10.get(i)), i),
        "square" => set(&mut to.square, from.and_then(|f| f.square.get(i)), i),
        "exputt" => set(&mut to.exputt, from.and_then(|f| f.exputt.get(i)), i),
        "trackball" => set(&mut to.trackball, from.and_then(|f| f.trackball.get(i)), i),
        "mock_monitor" => set(
            &mut to.mock_monitor,
            from.and_then(|f| f.mock_monitor.get(i)),
            i,
        ),
        "openconnect_server" => set(
            &mut to.openconnect_server,
            from.and_then(|f| f.openconnect_server.get(i)),
            i,
        ),
        "openconnect_client" => set(
            &mut to.openconnect_client,
            from.and_then(|f| f.openconnect_client.get(i)),
            i,
        ),
        "pureplay" => set(&mut to.pureplay, from.and_then(|f| f.pureplay.get(i)), i),
        "random_club" => set(
            &mut to.random_club,
            from.and_then(|f| f.random_club.get(i)),
            i,
        ),
        "camera" => set(&mut to.camera, from.and_then(|f| f.camera.get(i)), i),
        _ => {}
    }
}

/// `config`'s section for actor `global_id`, as JSON.
fn section_json(config: &FlighthookConfig, global_id: &str) -> Option<serde_json::Value> {
    fn get<T: serde::Serialize>(
        map: &HashMap<String, T>,
        index: &str,
    ) -> Option<serde_json::Value> {
        map.get(index).and_then(|s| serde_json::to_value(s).ok())
    }
    let (type_key, i) = global_id.split_once('.')?;
    match type_key {
        "webserver" => get(&config.webserver, i),
        "mevo" => get(&config.mevo, i),
        "r10" => get(&config.r10, i),
        "square" => get(&config.square, i),
        "exputt" => get(&config.exputt, i),
        "trackball" => get(&config.trackball, i),
        "mock_monitor" => get(&config.mock_monitor, i),
        "openconnect_server" => get(&config.openconnect_server, i),
        "openconnect_client" => get(&config.openconnect_client, i),
        "pureplay" => get(&config.pureplay, i),
        "random_club" => get(&config.random_club, i),
        "camera" => get(&config.camera, i),
        _ => None,
    }
}

/// Whether `config` holds exactly what form entry `entry` would save.
fn entry_matches(config: &FlighthookConfig, entry: &ActorFormEntry) -> bool {
    let mut own = config.clone();
    let id = apply_actor_to_config(&mut own, entry);
    let saved = section_json(config, &id);
    saved.is_some() && saved == section_json(&own, &id)
}

/// Whether form entry `actor` can be saved. Each reason it cannot shows
/// under its field on the card's Settings side.
pub(crate) fn actor_valid(actor: &ActorFormEntry) -> bool {
    match actor {
        ActorFormEntry::Device(dev) => {
            if dev.name.is_empty() {
                return false;
            }
            if dev.has_bind_address()
                && !dev.address.is_empty()
                && dev.address.parse::<std::net::SocketAddr>().is_err()
            {
                return false;
            }
            // A BLE address is optional — blank means auto-discover.
            let ble = dev.address.trim();
            if dev.has_ble_address() && !ble.is_empty() && !is_ble_address(ble) {
                return false;
            }
            if dev.is_mevo() && launch_only_spin_value(&dev.launch_only_spin).is_none() {
                return false;
            }
            if dev.has_camera_timing() && impact_lead_value(&dev.impact_lead).is_none() {
                return false;
            }
            if dev.driver_blocks_save() {
                return false;
            }
            if dev.is_camera()
                && (dev.cam.id.trim().is_empty()
                    || !video_mode_valid(&dev.cam.mode)
                    || latency_value(&dev.cam.latency).is_none()
                    || !flighthook::rotation_valid(dev.cam.rotation)
                    || angle_value(&dev.cam.yaw).is_none()
                    || angle_value(&dev.cam.pitch).is_none())
            {
                return false;
            }
            // Face impact calibration: blank means "use the default",
            // anything else must be a finite, non-negative mm value.
            !(dev.is_square()
                && dev
                    .square_impact_mm
                    .values()
                    .any(|v| !is_valid_impact_mm(v)))
        }
        ActorFormEntry::Integration(entry) => {
            !entry.name.is_empty()
                && (entry.integration_type == "random_club"
                    || entry.address.parse::<std::net::SocketAddr>().is_ok())
        }
    }
}

/// Apply a single actor form entry's data into a FlighthookConfig.
/// Returns the actor's global ID (e.g. "mevo.0").
fn apply_actor_to_config(config: &mut FlighthookConfig, actor: &ActorFormEntry) -> String {
    match actor {
        ActorFormEntry::Device(dev) => {
            let global_id = format!("{}.{}", dev.monitor_type, dev.id);
            match dev.monitor_type.as_str() {
                "mevo" => {
                    config.mevo.insert(
                        dev.id.clone(),
                        MevoSection {
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            launch_only_spin: launch_only_spin_value(&dev.launch_only_spin)
                                .unwrap_or(MevoSection::DEFAULT_LAUNCH_ONLY_SPIN),
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.mevo_config(),
                        },
                    );
                }
                "square" => {
                    config.square.insert(
                        dev.id.clone(),
                        flighthook::SquareSection {
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.square_config(),
                        },
                    );
                }
                "exputt" => {
                    config.exputt.insert(
                        dev.id.clone(),
                        flighthook::ExPuttSection {
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.exputt_config(),
                        },
                    );
                }
                "trackball" => {
                    config.trackball.insert(
                        dev.id.clone(),
                        flighthook::TrackballSection {
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            driver: dev.trackball_config(),
                        },
                    );
                }
                "r10" => {
                    config.r10.insert(
                        dev.id.clone(),
                        R10Section {
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.r10_config(),
                        },
                    );
                }
                "openconnect_server" => {
                    config.openconnect_server.insert(
                        dev.id.clone(),
                        flighthook::OpenConnectServerSection {
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            bind: if dev.address.is_empty() {
                                None
                            } else {
                                Some(dev.address.clone())
                            },
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                        },
                    );
                }
                "mock_monitor" => {
                    config.mock_monitor.insert(
                        dev.id.clone(),
                        MockMonitorSection {
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            impact_lead_s: impact_lead_value(&dev.impact_lead).flatten(),
                            driver: dev.mock.clone(),
                        },
                    );
                }
                "camera" => {
                    config.camera.insert(
                        dev.id.clone(),
                        CameraSection {
                            name: dev.name.clone(),
                            enabled: dev.enabled,
                            camera: dev.cam.id.trim().to_string(),
                            camera_name: dev.cam.name.clone(),
                            position: dev.cam.position,
                            mode: dev.cam.mode.trim().to_string(),
                            latency_s: latency_value(&dev.cam.latency).flatten(),
                            rotation_deg: dev.cam.rotation,
                            yaw_deg: angle_value(&dev.cam.yaw).flatten(),
                            pitch_deg: angle_value(&dev.cam.pitch).flatten(),
                        },
                    );
                }
                _ => {}
            }
            global_id
        }
        ActorFormEntry::Integration(entry) => {
            let global_id = format!("{}.{}", entry.integration_type, entry.id);
            match entry.integration_type.as_str() {
                "openconnect_client" => {
                    config.openconnect_client.insert(
                        entry.id.clone(),
                        OpenConnectClientSection {
                            name: entry.name.clone(),
                            enabled: entry.enabled,
                            address: if entry.address.is_empty() {
                                None
                            } else {
                                Some(entry.address.clone())
                            },
                            full_monitor: if entry.full_monitor.is_empty() {
                                None
                            } else {
                                Some(entry.full_monitor.clone())
                            },
                            chipping_monitor: if entry.chipping_monitor.is_empty() {
                                None
                            } else {
                                Some(entry.chipping_monitor.clone())
                            },
                            putting_monitor: if entry.putting_monitor.is_empty() {
                                None
                            } else {
                                Some(entry.putting_monitor.clone())
                            },
                        },
                    );
                }
                "pureplay" => {
                    let set = |v: &String| (!v.is_empty()).then(|| v.clone());
                    config.pureplay.insert(
                        entry.id.clone(),
                        PureplaySection {
                            name: entry.name.clone(),
                            enabled: entry.enabled,
                            address: set(&entry.address),
                            full_monitor: set(&entry.full_monitor),
                            chipping_monitor: set(&entry.chipping_monitor),
                            putting_monitor: set(&entry.putting_monitor),
                        },
                    );
                }
                "random_club" => {
                    config.random_club.insert(
                        entry.id.clone(),
                        RandomClubSection {
                            name: entry.name.clone(),
                            enabled: entry.enabled,
                        },
                    );
                }
                "webserver" => {
                    config.webserver.insert(
                        entry.id.clone(),
                        WebserverSection {
                            name: entry.name.clone(),
                            bind: entry.address.clone(),
                        },
                    );
                }
                _ => {}
            }
            global_id
        }
    }
}

/// Width of the label column in a settings card.
const LABEL_WIDTH: f32 = 140.0;
/// Width of a unit dropdown beside a field.
const UNIT_WIDTH: f32 = 50.0;
/// A text field's inner margin, both sides: its width beyond the
/// `desired_width` of its text.
const TEXT_EDIT_MARGIN: f32 = 8.0;

/// A field label in the card's label column. Hover text goes on the result.
fn field_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(LABEL_WIDTH, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(LABEL_WIDTH);
            ui.label(text)
        },
    )
    .inner
}

/// Skip the label column, so the row's control lines up with the fields.
fn field_indent(ui: &mut egui::Ui) {
    ui.add_space(LABEL_WIDTH + ui.spacing().item_spacing.x);
}

/// A validation message on its own row under a field, lined up with it.
fn field_error(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        field_indent(ui);
        ui.label(egui::RichText::new(text).color(theme::DANGER).size(11.0));
    });
}

/// A muted hint on its own row under a field, lined up with it, wrapped to
/// the card.
fn field_hint(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        field_indent(ui);
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            )
            .wrap(),
        );
    });
}

/// Widths of a card's fields: a text field or dropdown with room for a unit
/// dropdown after it, and a dropdown spanning the unit column too.
fn field_widths(ui: &egui::Ui) -> (f32, f32) {
    let gap = ui.spacing().item_spacing.x;
    let field = (ui.available_width() - LABEL_WIDTH - UNIT_WIDTH - 2.0 * gap).max(80.0);
    (field, field + gap + UNIT_WIDTH)
}

/// Horizontal padding of a type tag, each side.
const TAG_PAD: f32 = 6.0;
/// Narrowest a card's title gets before its type tag shrinks.
const TITLE_MIN_WIDTH: f32 = 40.0;
/// Narrowest text a type tag keeps; with less room it is left out.
const TAG_MIN_TEXT: f32 = 24.0;

/// Width of a type tag showing all of `text`.
fn tag_width(ui: &egui::Ui, text: &str) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(11.0),
        theme::HEADING,
    );
    galley.size().x + 2.0 * TAG_PAD
}

/// A card header's tag, at most `max_width` wide: its text is
/// cut short with "…" when it doesn't fit.
fn type_tag(ui: &mut egui::Ui, text: &str, hover: Option<&str>, max_width: f32) -> egui::Response {
    let tag = egui::Frame::new()
        .fill(theme::SURFACE_HOVER)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(TAG_PAD as i8, 2))
        .show(ui, |ui| {
            ui.set_max_width(max_width - 2.0 * TAG_PAD);
            ui.add(
                egui::Label::new(egui::RichText::new(text).size(11.0).color(theme::HEADING))
                    .truncate(),
            )
        });
    let response = tag.response;
    match hover {
        Some(hover) => response.on_hover_text(hover),
        None => response,
    }
}

/// Where a card header's tag sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TagPlace {
    /// Right after the title.
    Title,
    /// Right-aligned, just left of the buttons.
    Buttons,
}

/// A card's header row: the title on the left, `buttons` on the right, and
/// an optional tag at `place`. `buttons` runs in a right-to-left layout: the
/// first button added sits furthest right. The buttons keep their size;
/// the title is cut short first, down to [`TITLE_MIN_WIDTH`], then the
/// tag, which is left out once it would be cut under [`TAG_MIN_TEXT`].
/// `key` names the card for the tests.
pub(crate) fn card_header<R>(
    ui: &mut egui::Ui,
    key: &str,
    title: &str,
    tag: Option<(&str, TagPlace)>,
    hover: Option<&str>,
    buttons: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    card_header_with(ui, key, title, tag, hover, None, buttons).0
}

/// The on/off switch of a card header: whether the card is on, and whether
/// the switch takes input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeaderSwitch {
    pub(crate) on: bool,
    pub(crate) enabled: bool,
    /// Why the card cannot be turned on: an enabled card of its type has its
    /// address. Shown on hover while the switch is locked.
    pub(crate) clash: Option<String>,
}

/// [`card_header`] with an optional on/off `switch` right after the title,
/// which keeps its size like the buttons (hover "Turn off" / "Turn on",
/// mark `enabled:<key>`). Returns what `buttons` returned and whether the
/// switch was flipped.
pub(crate) fn card_header_with<R>(
    ui: &mut egui::Ui,
    key: &str,
    title: &str,
    tag: Option<(&str, TagPlace)>,
    hover: Option<&str>,
    switch: Option<HeaderSwitch>,
    buttons: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, bool) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let result = buttons(ui);
            let gap = ui.spacing().item_spacing.x;
            let switch_room = switch
                .as_ref()
                .map_or(0.0, |_| theme::ON_OFF_SWITCH.x + gap);
            let room = ui.available_width() - switch_room;
            let tag = tag.and_then(|(t, place)| {
                let full = tag_width(ui, t);
                let width = full.min(room - TITLE_MIN_WIDTH - gap);
                (width >= full.min(TAG_MIN_TEXT + 2.0 * TAG_PAD)).then_some((t, place, width))
            });
            let mut tag_after_title = None;
            match tag {
                Some((text, TagPlace::Buttons, width)) => {
                    let tag = type_tag(ui, text, hover, width);
                    devices::mark(ui, || format!("tag:{key}"), &tag);
                }
                Some((text, TagPlace::Title, width)) => tag_after_title = Some((text, width)),
                None => {}
            }
            let flipped = ui
                .with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let title_width = ui.available_width()
                        - switch_room
                        - tag_after_title.map_or(0.0, |(_, width)| width + gap);
                    ui.scope(|ui| {
                        ui.set_max_width(title_width.max(TITLE_MIN_WIDTH));
                        let title = ui.add(
                            egui::Label::new(
                                egui::RichText::new(title)
                                    .strong()
                                    .size(16.0)
                                    .color(theme::HEADING),
                            )
                            .truncate(),
                        );
                        devices::mark(ui, || format!("title:{key}"), &title);
                        if let Some(hover) = hover {
                            title.on_hover_text(hover);
                        }
                    });
                    let flipped = switch.as_ref().is_some_and(|switch| {
                        let response = ui
                            .add_enabled_ui(switch.enabled, |ui| {
                                theme::on_off_switch(ui, switch.on)
                            })
                            .inner
                            .on_hover_text(if switch.on { "Turn off" } else { "Turn on" });
                        let response = match &switch.clash {
                            Some(clash) => response.on_disabled_hover_text(clash),
                            None => response,
                        };
                        devices::mark(ui, || format!("enabled:{key}"), &response);
                        response.clicked()
                    });
                    if let Some((text, width)) = tag_after_title {
                        let tag = type_tag(ui, text, hover, width);
                        devices::mark(ui, || format!("tag:{key}"), &tag);
                    }
                    flipped
                })
                .inner;
            (result, flipped)
        })
        .inner
    })
    .inner
}

/// A small header button: accent-filled while `primary`.
pub(crate) fn header_button(text: &str, primary: bool) -> egui::Button<'static> {
    if primary {
        theme::primary_button(text, 11.0)
    } else {
        egui::Button::new(egui::RichText::new(text.to_string()).size(11.0))
    }
}

/// What a card's Save or Cancel asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    Save,
    Cancel,
}

/// A dirty card's Save and Cancel, in a right-to-left header: Save (the
/// primary) furthest right. A spinner sits beside them while a save is out.
/// `key` names the card for the tests.
pub(crate) fn save_cancel(
    ui: &mut egui::Ui,
    key: &str,
    can_save: bool,
    saving: bool,
) -> Option<Pending> {
    let mut pending = None;
    let save = ui.add_enabled(can_save && !saving, header_button("Save", true));
    devices::mark(ui, || format!("save:{key}"), &save);
    if save.clicked() {
        pending = Some(Pending::Save);
    }
    let cancel = ui.add_enabled(!saving, header_button("Cancel", false));
    devices::mark(ui, || format!("cancel:{key}"), &cancel);
    if cancel.clicked() {
        pending = Some(Pending::Cancel);
    }
    if saving {
        ui.spinner();
    }
    pending
}

/// The hint under a dirty card's header while the rest of the tab waits for
/// it.
pub(crate) fn lock_hint(ui: &mut egui::Ui) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.label(
            egui::RichText::new("Save or cancel your changes")
                .size(11.0)
                .color(theme::TEXT_MUTED),
        );
    });
}

/// A failed save's reason, under the card's header. `key` names the card
/// for the tests.
pub(crate) fn save_error_line(ui: &mut egui::Ui, key: &str, message: &str) {
    let line = ui.label(
        egui::RichText::new(format!("Save failed: {message}"))
            .size(11.0)
            .color(theme::DANGER),
    );
    devices::mark(ui, || format!("error:{key}"), &line);
}

/// The Remove button of an actor's settings side, for its action row.
pub(crate) fn remove_button(ui: &mut egui::Ui, key: &str, enabled: bool) -> bool {
    action_button(ui, "Remove", &format!("remove:{key}"), enabled)
}

/// A header-style button in a card's action row. `mark` names it for the
/// tests.
pub(crate) fn action_button(ui: &mut egui::Ui, text: &str, mark: &str, enabled: bool) -> bool {
    let button = ui.add_enabled(enabled, header_button(text, false));
    devices::mark(ui, || mark.to_string(), &button);
    button.clicked()
}

/// Hover text of the Simulation card.
const SIMULATION_HOVER: &str = "When a launch monitor doesn't report carry, apex, flight time, descent, offline, roll or total, flighthook simulates them from the measured launch. Simulated values are tagged SIM on the Cards view and shown in italics in the Table. Never sent to GSPro, which simulates the flight itself.";

/// Hover text of the Auto-chip checkbox on the Simulation card.
const AUTO_CHIP_SETTING_HOVER: &str = "Show the Auto-chip control in the header: it sends a short straight chip to the sim, for chips your launch monitor can't read (radar monitors often miss short chips). The chip is simulated under the altitude, temperature and surface below, whatever the Simulation mode.";

/// Hover text of the Replay card.
const REPLAY_HOVER: &str = "The cameras record a clip around every shot: from the pre-roll before the estimated impact to the post-roll after the launch monitor reports the shot. The newest clips are kept on disk; older ones are deleted, and Clear history deletes them all.";

/// The fields of one actor card, by type. `cameras` is the camera list a
/// camera card picks from. Returns whether the camera list was asked to
/// refresh.
pub(crate) fn actor_fields(
    ui: &mut egui::Ui,
    actor: &mut ActorFormEntry,
    device_monitor_options: &[(String, String)],
    cameras: &CameraList,
) -> bool {
    let (field_width, wide_width) = field_widths(ui);
    let mut refresh = false;
    match actor {
        ActorFormEntry::Device(dev) => {
            // Name
            ui.horizontal(|ui| {
                field_label(ui, "Name:")
                    .on_hover_text("Display name for this device in the UI and logs.");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut dev.name)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed()
                {
                    dev.dirty = true;
                }
            });
            if dev.name.is_empty() {
                field_error(ui, "Name is required");
            }

            if dev.is_camera() {
                refresh = camera_fields(ui, dev, cameras, field_width, wide_width);
            }

            if dev.has_network_address() {
                // Address
                ui.horizontal(|ui| {
                    field_label(ui, "Address:").on_hover_text(
                        "TCP address of the launch monitor. Connect to the device WiFi AP first.",
                    );
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut dev.address)
                                .desired_width(field_width - TEXT_EDIT_MARGIN)
                                .hint_text(MEVO_ADDRESS),
                        )
                        .on_hover_text("ip:port (e.g. 192.168.2.1:5100)")
                        .changed()
                    {
                        dev.dirty = true;
                    }
                });
            }

            if dev.has_bind_address() {
                // Bind address — optional; blank listens on the default.
                ui.horizontal(|ui| {
                    field_label(ui, "Bind Address:").on_hover_text(format!(
                        "Address and port this server listens on. 0.0.0.0 accepts \
                         connections from other computers too; 127.0.0.1 only from \
                         this one. Leave blank for {OPENCONNECT_BIND}. Point the \
                         launch monitor's Open Connect connector at this port."
                    ));
                    let field = ui
                        .add(
                            egui::TextEdit::singleline(&mut dev.address)
                                .desired_width(field_width - TEXT_EDIT_MARGIN)
                                .hint_text(OPENCONNECT_BIND),
                        )
                        .on_hover_text("ip:port (e.g. 0.0.0.0:921)");
                    devices::mark(
                        ui,
                        || format!("bind:{}.{}", dev.monitor_type, dev.id),
                        &field,
                    );
                    if field.changed() {
                        dev.dirty = true;
                    }
                });
                if !dev.address.is_empty() && dev.address.parse::<std::net::SocketAddr>().is_err() {
                    field_error(ui, "Invalid bind address");
                }
            }

            if dev.has_ble_address() {
                // BLE address — optional; blank auto-discovers.
                ui.horizontal(|ui| {
                    field_label(ui, "BLE Address:").on_hover_text("Advertised name of the device, e.g. SquareGolf(CD01) — the same on every OS. A Bluetooth address (a UUID on macOS) also works. Leave blank to auto-discover.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.address).desired_width(field_width - TEXT_EDIT_MARGIN))
                        .on_hover_text("optional, e.g. SquareGolf(CD01)")
                        .changed()
                    {
                        dev.dirty = true;
                    }
                });
                let a = dev.address.trim();
                if !a.is_empty() && !is_ble_address(a) {
                    field_error(ui, "Invalid BLE address");
                }
            }

            if dev.has_ip_address() {
                // IP address — optional; blank finds the device.
                ui.horizontal(|ui| {
                    field_label(ui, "IP Address:").on_hover_text(
                        "IP address of the device on your network. Leave blank to \
                         find it automatically.",
                    );
                    let field = ui
                        .add(
                            egui::TextEdit::singleline(&mut dev.address)
                                .desired_width(field_width - TEXT_EDIT_MARGIN)
                                .hint_text(EXPUTT_IP_PLACEHOLDER),
                        )
                        .on_hover_text("optional, e.g. 192.168.1.50");
                    devices::mark(ui, || format!("ip:{}.{}", dev.monitor_type, dev.id), &field);
                    if field.changed() {
                        dev.dirty = true;
                    }
                });
            }

            if dev.is_square() {
                // Zero-spin rejection (putts always exempt).
                ui.horizontal(|ui| {
                    field_indent(ui);
                    if ui
                        .checkbox(&mut dev.discard_zero_spin, "Discard zero-spin shots")
                        .on_hover_text(
                            "A struck ball always spins, so a zero-spin reading \
                             is a failed read — usually a ball too far forward in \
                             the hitting zone. Such shots fly far too long in the \
                             sim, so they are discarded and you re-hit.\n\n\
                             Putts are never discarded: there is no airborne \
                             flight to measure spin over, so a putt reads zero \
                             every time.",
                        )
                        .changed()
                    {
                        dev.dirty = true;
                    }
                });

                // Per-club face impact calibration override.
                {
                    egui::CollapsingHeader::new("Face impact calibration (beta)")
                        .id_salt(format!("impact_cal_{}", dev.id))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "Distance from the bottom edge of the club \
                                     sticker's dot down to face center, in mm. \
                                     Blank uses the built-in default for that club.",
                                )
                                .size(11.0)
                                .weak(),
                            );
                            for &club in SQUARE_CALIBRATED_CLUBS {
                                let mut val =
                                    dev.square_impact_mm.get(&club).cloned().unwrap_or_default();
                                ui.horizontal(|ui| {
                                    ui.label(format!("{club}:"));
                                    let response = ui.add(
                                        egui::TextEdit::singleline(&mut val)
                                            .desired_width(80.0)
                                            .hint_text("default"),
                                    );
                                    let invalid = !is_valid_impact_mm(&val);
                                    if response.changed() {
                                        dev.square_impact_mm.insert(club, val);
                                        dev.dirty = true;
                                    }
                                    ui.label(egui::RichText::new("mm").size(11.0).weak());
                                    if invalid {
                                        ui.label(
                                            egui::RichText::new("Invalid")
                                                .color(theme::DANGER)
                                                .size(11.0),
                                        );
                                    }
                                });
                            }
                        });
                }
            }

            if dev.is_r10() {
                // Tee distance, pushed to the device on wake-up.
                ui.horizontal(|ui| {
                    field_label(ui, "Monitor-to-Ball:").on_hover_text("Distance from the R10 to the ball, sent to the device as its tee distance.\nGarmin recommends 6-8 ft behind the ball.\n\nLeave blank to keep whatever distance is already set on the device.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.range_val).desired_width(field_width - TEXT_EDIT_MARGIN))
                        .changed()
                    {
                        dev.dirty = true;
                    }
                    egui::ComboBox::from_id_salt(format!("range_unit_{}", dev.id))
                        .selected_text(unit_suffix(&dev.range_unit))
                        .width(50.0)
                        .show_ui(ui, |ui| {
                            for &(key, label) in DISTANCE_UNITS {
                                if ui.selectable_label(dev.range_unit == key, label).clicked() {
                                    dev.range_unit = key.to_string();
                                    dev.dirty = true;
                                }
                            }
                        });
                });
            }

            if dev.has_mevo_tuning() {
                // Ball Type
                ui.horizontal(|ui| {
                    field_label(ui, "Ball Type:").on_hover_text(
                        "RCT = Radar Capture Technology.\nStandard = any regular golf ball.",
                    );
                    let ball_text = if dev.ball_type == 1 {
                        "RCT"
                    } else {
                        "Standard"
                    };
                    egui::ComboBox::from_id_salt(format!("ball_type_{}", dev.id))
                        .selected_text(ball_text)
                        .width(field_width)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(dev.ball_type == 1, "RCT").clicked() {
                                dev.ball_type = 1;
                                dev.dirty = true;
                            }
                            if ui
                                .selectable_label(dev.ball_type == 0, "Standard")
                                .clicked()
                            {
                                dev.ball_type = 0;
                                dev.dirty = true;
                            }
                        });
                });

                // Tee Height
                ui.horizontal(|ui| {
                    field_label(ui, "Tee Height:")
                        .on_hover_text("Height of the tee above the hitting surface.");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut dev.tee_height_val)
                                .desired_width(field_width - TEXT_EDIT_MARGIN),
                        )
                        .changed()
                    {
                        dev.dirty = true;
                    }
                    egui::ComboBox::from_id_salt(format!("tee_unit_{}", dev.id))
                        .selected_text(unit_suffix(&dev.tee_height_unit))
                        .width(50.0)
                        .show_ui(ui, |ui| {
                            for &(key, label) in DISTANCE_UNITS {
                                if ui
                                    .selectable_label(dev.tee_height_unit == key, label)
                                    .clicked()
                                {
                                    dev.tee_height_unit = key.to_string();
                                    dev.dirty = true;
                                }
                            }
                        });
                });

                // Monitor-to-Ball
                ui.horizontal(|ui| {
                    field_label(ui, "Monitor-to-Ball:").on_hover_text("Distance from the front of the launch monitor to the ball.\nMevo+ recommended range: 7-9 ft.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.range_val).desired_width(field_width - TEXT_EDIT_MARGIN))
                        .changed()
                    {
                        dev.dirty = true;
                    }
                    egui::ComboBox::from_id_salt(format!("range_unit_{}", dev.id))
                        .selected_text(unit_suffix(&dev.range_unit))
                        .width(50.0)
                        .show_ui(ui, |ui| {
                            for &(key, label) in DISTANCE_UNITS {
                                if ui.selectable_label(dev.range_unit == key, label).clicked() {
                                    dev.range_unit = key.to_string();
                                    dev.dirty = true;
                                }
                            }
                        });
                });

                // Surface Height
                ui.horizontal(|ui| {
                    field_label(ui, "Surface Height:").on_hover_text("Height of the hitting surface above the monitor.\nSet to 0 if the ball and monitor are on the same level.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.surface_height_val).desired_width(field_width - TEXT_EDIT_MARGIN))
                        .changed()
                    {
                        dev.dirty = true;
                    }
                    egui::ComboBox::from_id_salt(format!("surface_unit_{}", dev.id))
                        .selected_text(unit_suffix(&dev.surface_height_unit))
                        .width(50.0)
                        .show_ui(ui, |ui| {
                            for &(key, label) in DISTANCE_UNITS {
                                if ui.selectable_label(dev.surface_height_unit == key, label).clicked() {
                                    dev.surface_height_unit = key.to_string();
                                    dev.dirty = true;
                                }
                            }
                        });
                });

                // Track %
                ui.horizontal(|ui| {
                    field_label(ui, "Track %:").on_hover_text("Minimum outdoor tracking percentage (0-100).\nLower values accept shorter-tracked shots. Default: 80.");
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.track_pct).desired_width(field_width - TEXT_EDIT_MARGIN))
                        .changed()
                    {
                        dev.dirty = true;
                    }
                    ui.label("%");
                });

                // Use Estimated
                ui.horizontal(|ui| {
                    field_indent(ui);
                    if ui.checkbox(&mut dev.use_estimated, "Use Estimated Shots")
                        .on_hover_text("Include estimated (E8 fallback) shots. Off by default.\nWith it off, a shot the device only estimated (a flight too short\nto read in Outdoor mode, a very short chip) is skipped.\nEstimated shots may lack sidespin and carry less data.")
                        .changed()
                    {
                        dev.dirty = true;
                    }
                });

                // Environment
                ui.horizontal(|ui| {
                    field_label(ui, "Environment:").on_hover_text(
                        "Limited flight is for nets and sim screens.\nOutdoor needs about 40 yd of unobstructed flight;\nshots into a screen won't read.",
                    );
                    egui::ComboBox::from_id_salt(format!("environment_{}", dev.id))
                        .selected_text(environment_label(dev.environment))
                        .width(wide_width)
                        .show_ui(ui, |ui| {
                            for env in ENVIRONMENTS {
                                if ui
                                    .selectable_label(
                                        dev.environment == env,
                                        environment_label(env),
                                    )
                                    .clicked()
                                {
                                    dev.environment = env;
                                    dev.dirty = true;
                                }
                            }
                        });
                });

                // Spin source (Xi)
                ui.horizontal(|ui| {
                    field_label(ui, "Spin source (Xi):").on_hover_text(
                        "FlightScope Xi only. The Xi sends two backspin values per shot.\nD4 is its final spin, the value FS Golf shows.\nE8 is the second value, often higher on full swings; on chips and\npitches E8 isn't measured, so choose knowingly. Auto uses E8 only\nwhile flighthook is in Full mode, and D4 otherwise -- a chip hit\nwhile still in Full mode gets E8's speed-scaled value, so set\nchipping clubs or switch modes first. A Mevo+ or Gen2 reports the\nsame spin either way.",
                    );
                    egui::ComboBox::from_id_salt(format!("spin_source_{}", dev.id))
                        .selected_text(spin_source_label(dev.spin_source))
                        .width(wide_width)
                        .show_ui(ui, |ui| {
                            for src in SPIN_SOURCES {
                                if ui
                                    .selectable_label(
                                        dev.spin_source == src,
                                        spin_source_label(src),
                                    )
                                    .clicked()
                                {
                                    dev.spin_source = src;
                                    dev.dirty = true;
                                }
                            }
                        });
                });

                // Launch-only spin
                ui.horizontal(|ui| {
                    let hint = "When the device measures launch but no spin (very short chips on the Xi), use this many rpm of backspin per mph of ball speed, 0 to 1000. 0 sends the shot as the device read it.";
                    field_label(ui, "Launch-only spin:").on_hover_text(hint);
                    let unit = "rpm per mph";
                    let unit_width = ui
                        .painter()
                        .layout_no_wrap(
                            unit.into(),
                            egui::TextStyle::Body.resolve(ui.style()),
                            egui::Color32::PLACEHOLDER,
                        )
                        .size()
                        .x;
                    let gap = ui.spacing().item_spacing.x;
                    let edit_width = (wide_width - gap - unit_width - TEXT_EDIT_MARGIN).max(40.0);
                    if ui
                        .add(egui::TextEdit::singleline(&mut dev.launch_only_spin).desired_width(edit_width))
                        .on_hover_text(hint)
                        .changed()
                    {
                        dev.dirty = true;
                    }
                    ui.label(unit);
                });
                if launch_only_spin_value(&dev.launch_only_spin).is_none() {
                    field_error(ui, "Must be a number, 0 to 1000");
                }

                // Camera Mode
                ui.horizontal(|ui| {
                    field_label(ui, "Camera Mode:").on_hover_text(
                        "Standard reports ball flight and radar club data.\nFusion modes add the full club data set (path, face angle,\nattack angle, dynamic loft, smash factor, swing planes) and\nneed the Pro Package enabled on the device.\n\nWhich Fusion mode depends on firmware: Raw Fusion for\nBM17.04 and newer, Fusion for older. The wrong one yields\nno Fusion club data. Fusion is applied after a 15s camera warmup.",
                    );
                    egui::ComboBox::from_id_salt(format!("cam_mode_{}", dev.id))
                        .selected_text(camera_mode_label(dev.camera_mode))
                        .width(wide_width)
                        .show_ui(ui, |ui| {
                            for mode in CAMERA_MODES {
                                if ui
                                    .selectable_label(
                                        dev.camera_mode == mode,
                                        camera_mode_label(mode),
                                    )
                                    .clicked()
                                {
                                    dev.camera_mode = mode;
                                    dev.dirty = true;
                                }
                            }
                        });
                });
            }

            if dev.is_trackball() {
                trackball::fields(ui, dev, field_width, wide_width);
            }

            if dev.has_camera_timing() {
                camera_timing_field(ui, dev, field_width);
            }
            if let Some(error) = dev.driver_error() {
                let line = ui.label(egui::RichText::new(error).color(theme::DANGER).size(11.0));
                devices::mark(
                    ui,
                    || format!("driver_error:{}.{}", dev.monitor_type, dev.id),
                    &line,
                );
            }
        }
        ActorFormEntry::Integration(entry) => {
            // Name
            ui.horizontal(|ui| {
                field_label(ui, "Name:")
                    .on_hover_text("Display name for this integration in the UI and logs.");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut entry.name)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed()
                {
                    entry.dirty = true;
                }
            });
            if entry.name.is_empty() {
                field_error(ui, "Name is required");
            }

            // Address field (skip for mock): a web server's bind address, a
            // client's simulator address.
            if entry.integration_type != "random_club" {
                let (label, hover, example) = if entry.integration_type == "webserver" {
                    (
                        "Bind Address:",
                        "Address and port the web server listens on, for the dashboard \
                         and the API. 0.0.0.0 accepts connections from other computers \
                         too; 127.0.0.1 only from this one.",
                        "ip:port (e.g. 0.0.0.0:5880)",
                    )
                } else if entry.integration_type == "pureplay" {
                    (
                        "Address:",
                        "TCP address of Pureplay. Shot data is sent here over Pureplay Connect.",
                        "ip:port (e.g. 127.0.0.1:9760)",
                    )
                } else {
                    (
                        "Address:",
                        "TCP address of the simulator. Shot data is forwarded here as JSON.",
                        "ip:port (e.g. 127.0.0.1:921)",
                    )
                };
                ui.horizontal(|ui| {
                    field_label(ui, label).on_hover_text(hover);
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut entry.address)
                                .desired_width(field_width - TEXT_EDIT_MARGIN),
                        )
                        .on_hover_text(example)
                        .changed()
                    {
                        entry.dirty = true;
                    }
                });
                if entry.address.parse::<std::net::SocketAddr>().is_err() {
                    field_error(ui, "Invalid address");
                }
            }

            // Routing dropdowns (Open Connect clients and Pureplay)
            if matches!(
                entry.integration_type.as_str(),
                "openconnect_client" | "pureplay"
            ) {
                for (label_text, field_val, salt) in [
                    ("Full Monitor:", &mut entry.full_monitor, "full"),
                    ("Chipping Monitor:", &mut entry.chipping_monitor, "chipping"),
                    ("Putting Monitor:", &mut entry.putting_monitor, "putting"),
                ] {
                    ui.horizontal(|ui| {
                        field_label(ui, label_text).on_hover_text(
                            "Which launch monitor to accept shots from for this mode.\n\"Any\" accepts shots from all monitors."
                        );
                        let display = if field_val.is_empty() {
                            "Any"
                        } else {
                            device_monitor_options.iter()
                                .find(|(id, _)| id == field_val.as_str())
                                .map(|(_, name)| name.as_str())
                                .unwrap_or(field_val.as_str())
                        };
                        egui::ComboBox::from_id_salt(format!("{}_{}", salt, entry.id))
                            .selected_text(display)
                            .width(field_width)
                            .truncate()
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(field_val.is_empty(), "Any").clicked() {
                                    field_val.clear();
                                    entry.dirty = true;
                                }
                                for (monitor_id, monitor_name) in device_monitor_options {
                                    if ui.selectable_label(field_val.as_str() == monitor_id, monitor_name).clicked() {
                                        *field_val = monitor_id.clone();
                                        entry.dirty = true;
                                    }
                                }
                            });
                    });
                }
            }
        }
    }
    refresh
}

/// How a camera entry reads in the picker: its name, and the card using it
/// when that is another one.
fn camera_choice_label(entry: &CameraListEntry, own: &str) -> String {
    match entry.in_use_by.as_deref() {
        Some(user) if user != own => format!("{} (in use by {user})", entry.name),
        _ => entry.name.clone(),
    }
}

/// A launch monitor card's Camera timing field: seconds from impact to the
/// monitor's report, blank for the device's default (shown in the field).
fn camera_timing_field(ui: &mut egui::Ui, dev: &mut DeviceFormEntry, field_width: f32) {
    ui.horizontal(|ui| {
        field_label(ui, "Camera timing:");
        let hint = flighthook::impact_lead_default_s(&dev.monitor_type)
            .map(format_distance_value)
            .unwrap_or_default();
        let field = ui.add(
            egui::TextEdit::singleline(&mut dev.impact_lead)
                .desired_width(field_width - TEXT_EDIT_MARGIN)
                .hint_text(hint),
        );
        devices::mark(
            ui,
            || format!("camera_timing:{}.{}", dev.monitor_type, dev.id),
            &field,
        );
        if field.changed() {
            dev.dirty = true;
        }
        ui.label("s");
    });
    if impact_lead_value(&dev.impact_lead).is_none() {
        field_error(
            ui,
            &format!("Must be a number, 0 to {}", flighthook::MAX_IMPACT_LEAD_S),
        );
    }
}

/// A camera card's fields: Camera, then Rotation (in sight without
/// scrolling, for a quick setup), Position, Video mode, Latency, Yaw and
/// Pitch. Returns whether the camera list was asked to refresh.
fn camera_fields(
    ui: &mut egui::Ui,
    dev: &mut DeviceFormEntry,
    cameras: &CameraList,
    field_width: f32,
    wide_width: f32,
) -> bool {
    let own = format!("{}.{}", dev.monitor_type, dev.id);
    let entries: &[CameraListEntry] = cameras.list.as_ref().map_or(&[], |l| &l.cameras);
    let picked = entries.iter().find(|e| e.id == dev.cam.id);
    let mut refresh = false;
    ui.horizontal(|ui| {
        field_label(ui, "Camera:").on_hover_text(
            "The USB camera to record from. It is found again after a replug into the \
             same port. The test pattern is a generated picture, for trying replay \
             without a camera.",
        );
        let shown = match (picked, &dev.cam.name) {
            (Some(e), _) => e.name.clone(),
            (None, Some(name)) => format!("{name} (not connected)"),
            (None, None) if dev.cam.id.is_empty() => "Pick a camera".to_owned(),
            (None, None) => format!("{} (not connected)", dev.cam.id),
        };
        let combo = egui::ComboBox::from_id_salt(format!("camera_{}", dev.id))
            .selected_text(shown)
            .width(field_width)
            .truncate()
            .show_ui(ui, |ui| {
                for entry in entries {
                    let label = camera_choice_label(entry, &own);
                    let item = ui.selectable_label(entry.id == dev.cam.id, label);
                    if item.clicked() && entry.id != dev.cam.id {
                        dev.cam.id.clone_from(&entry.id);
                        dev.cam.name = Some(entry.name.clone());
                        if !entry.modes.contains(&dev.cam.mode) {
                            dev.cam.mode = CameraSection::AUTO_MODE.to_owned();
                        }
                        dev.dirty = true;
                    }
                }
                if entries.is_empty() {
                    ui.label(
                        egui::RichText::new(if cameras.loading {
                            "Looking for cameras…"
                        } else {
                            "No cameras found"
                        })
                        .color(theme::TEXT_MUTED),
                    );
                }
            });
        devices::mark(ui, || format!("camera_pick:{own}"), &combo.response);
        if cameras.loading {
            ui.spinner();
        } else {
            let again = theme::icon(ui, theme::Icon::Refresh, 20.0, theme::ACCENT, true)
                .on_hover_text("Look for cameras again");
            devices::mark(ui, || format!("camera_refresh:{own}"), &again);
            refresh = again.clicked();
        }
    });
    if let Some(error) = &cameras.error {
        field_error(ui, &format!("Camera list: {error}"));
    }
    if dev.cam.id.trim().is_empty() {
        field_error(ui, "Pick a camera");
    }
    if let Some(error) = picked.and_then(|e| e.error.as_deref()) {
        field_error(ui, error);
    }

    ui.horizontal(|ui| {
        field_label(ui, "Rotation:").on_hover_text(
            "Turns the camera's picture clockwise before it is shown and tracked, for a \
             camera mounted on its side. Clips are stored as the camera sends them.",
        );
        let rotations = flighthook::CAMERA_ROTATIONS;
        let labels = rotations.map(|r| format!("{r}°"));
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected = rotations
            .iter()
            .position(|r| *r == dev.cam.rotation)
            .unwrap_or(usize::MAX);
        let toggle = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
        devices::mark(ui, || format!("camera_rotation:{own}"), &toggle.response);
        let (clicked, pills) = toggle.inner;
        for (rotation, pill) in rotations.iter().zip(&pills) {
            devices::mark(ui, || format!("camera_rotation:{own}:{rotation}"), pill);
        }
        if let Some(rotation) = clicked.map(|i| rotations[i])
            && rotation != dev.cam.rotation
        {
            dev.cam.rotation = rotation;
            dev.dirty = true;
        }
    });
    if !flighthook::rotation_valid(dev.cam.rotation) {
        field_error(ui, "0°, 90°, 180° or 270°");
    }

    ui.horizontal(|ui| {
        field_label(ui, "Position:").on_hover_text("Where the camera stands.");
        egui::ComboBox::from_id_salt(format!("camera_position_{}", dev.id))
            .selected_text(dev.cam.position.label())
            .width(wide_width)
            .show_ui(ui, |ui| {
                for position in CameraPosition::all() {
                    if ui
                        .selectable_label(dev.cam.position == position, position.label())
                        .clicked()
                    {
                        dev.cam.position = position;
                        dev.dirty = true;
                    }
                }
            });
    });

    ui.horizontal(|ui| {
        field_label(ui, "Video mode:").on_hover_text(
            "Auto picks the camera's highest frame rate, then its largest size at that \
             rate. A faster mode shows more of the swing in slow motion and takes more \
             disk space.",
        );
        let auto = CameraSection::AUTO_MODE;
        let label = |mode: &str| {
            if mode == auto {
                "Auto".to_owned()
            } else {
                mode.replace('x', "×").replace('@', " @ ")
            }
        };
        let mut modes: Vec<&str> = vec![auto];
        if dev.cam.mode != auto && !picked.is_some_and(|e| e.modes.contains(&dev.cam.mode)) {
            modes.push(dev.cam.mode.as_str());
        }
        let listed: Vec<String> = picked.map(|e| e.modes.clone()).unwrap_or_default();
        modes.extend(listed.iter().map(String::as_str));
        let mut chosen = None;
        let combo = egui::ComboBox::from_id_salt(format!("camera_mode_{}", dev.id))
            .selected_text(label(&dev.cam.mode))
            .width(wide_width)
            .show_ui(ui, |ui| {
                for mode in &modes {
                    if ui
                        .selectable_label(dev.cam.mode == *mode, label(mode))
                        .clicked()
                    {
                        chosen = Some((*mode).to_owned());
                    }
                }
            });
        devices::mark(ui, || format!("camera_mode:{own}"), &combo.response);
        if let Some(mode) = chosen.filter(|m| *m != dev.cam.mode) {
            dev.cam.mode = mode;
            dev.dirty = true;
        }
    });
    if !video_mode_valid(&dev.cam.mode) {
        field_error(ui, "Auto or WIDTHxHEIGHT@FPS");
    }

    ui.horizontal(|ui| {
        field_label(ui, "Latency:");
        let field = ui.add(
            egui::TextEdit::singleline(&mut dev.cam.latency)
                .desired_width(field_width - TEXT_EDIT_MARGIN)
                .hint_text("0"),
        );
        devices::mark(ui, || format!("camera_latency:{own}"), &field);
        if field.changed() {
            dev.dirty = true;
        }
        ui.label("s");
    });
    if latency_value(&dev.cam.latency).is_none() {
        field_error(
            ui,
            &format!("Must be a number, 0 to {}", flighthook::MAX_LATENCY_S),
        );
    }

    let max = flighthook::MAX_CAMERA_ANGLE_DEG;
    for (label, key, text, hint) in [
        (
            "Yaw:",
            "camera_yaw",
            &mut dev.cam.yaw,
            "How far the camera points off square, in degrees.",
        ),
        (
            "Pitch:",
            "camera_pitch",
            &mut dev.cam.pitch,
            "How far the camera tilts down from level, in degrees.",
        ),
    ] {
        ui.horizontal(|ui| {
            field_label(ui, label).on_hover_text(hint);
            let field = ui.add(
                egui::TextEdit::singleline(text)
                    .desired_width(field_width - TEXT_EDIT_MARGIN)
                    .hint_text("0"),
            );
            devices::mark(ui, || format!("{key}:{own}"), &field);
            if field.changed() {
                dev.dirty = true;
            }
            ui.label("°");
        });
        if angle_value(text).is_none() {
            field_error(ui, &format!("Must be a number, -{max} to {max}"));
        }
        if key == "camera_yaw" {
            field_hint(
                ui,
                "Positive: the camera points to the right of the target line for a right-hander",
            );
        }
    }
    refresh
}

impl FlighthookApp {
    /// Post a save of `target`.
    pub(crate) fn post_save(&mut self, ctx: &egui::Context, target: SaveTarget) {
        let req = self.settings.begin_save(target.clone());
        net::post_settings(ctx, &self.pending, &req, target.scope());
    }

    /// Save the Global and Simulation cards: one request for both.
    pub(crate) fn save_general(&mut self, ctx: &egui::Context) {
        self.post_save(ctx, SaveTarget::Global);
    }

    /// Cancel the Global and Simulation cards together. The Shots tab's
    /// units go back with the setting.
    pub(crate) fn cancel_general(&mut self) {
        self.settings.cancel_general();
    }

    /// The unit system every Shots view and the exports use: the Units
    /// setting, which the Shots tab follows as soon as it is changed on the
    /// Global card (Cancel puts the saved one back).
    pub(crate) fn units(&self) -> UnitSystem {
        self.settings.default_units
    }

    /// Change the Units setting on the Global card.
    pub(crate) fn set_default_units(&mut self, units: UnitSystem) {
        self.settings.default_units = units;
        self.settings
            .simulation
            .set_fahrenheit(units == UnitSystem::Imperial);
        self.settings
            .player
            .set_inches(units == UnitSystem::Imperial);
        self.settings.mark_general(GeneralCard::Global);
    }

    /// A General card's header buttons, right to left: Save and Cancel
    /// while `card` has unsaved edits.
    fn general_buttons(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        card: GeneralCard,
    ) -> bool {
        if !self.settings.general_card_dirty(card) {
            return false;
        }
        let key = match card {
            GeneralCard::Global => "global",
            GeneralCard::Simulation => "simulation",
            GeneralCard::Replay => "replay",
            GeneralCard::BodyTracking => "body_tracking",
            GeneralCard::Player => "player",
        };
        let can_save = self.settings.general_valid();
        match save_cancel(ui, key, can_save, self.settings.saving()) {
            Some(Pending::Save) => self.save_general(ctx),
            Some(Pending::Cancel) => self.cancel_general(),
            None => {}
        }
        true
    }

    /// A failed Global save's reason, on a dirty General card.
    fn general_error(&self, ui: &mut egui::Ui, key: &str) {
        if let Some(error) = &self.settings.save_error
            && error.target == SaveTarget::Global
        {
            save_error_line(ui, key, &error.message);
        }
    }

    /// The Global card: units, default shot view, chipping and putting
    /// clubs. Its edit controls are disabled unless `editable`.
    pub(crate) fn render_global_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
    ) {
        let card = devices::fixed_card(ui, "global", theme::HEADING, |card| {
            let ui = card.top();
            let dirty = card_header(ui, "global", "Global", None, None, |ui| {
                let dirty = self.general_buttons(ctx, ui, GeneralCard::Global);
                if ui
                    .button(egui::RichText::new("API Docs").size(11.0))
                    .clicked()
                {
                    self.show_api_docs = true;
                }
                dirty
            });
            if dirty {
                lock_hint(ui);
                self.general_error(ui, "global");
            }
            card.body(false, |ui| {
                ui.add_space(6.0);
                ui.add_enabled_ui(editable, |ui| self.global_fields(ui));
            });
        });
        devices::mark(ui, || "card:global".to_owned(), &card.response);
    }

    /// The Global card's fields.
    fn global_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);

        ui.horizontal(|ui| {
            field_label(ui, "Units:").on_hover_text(
                "Unit system for shot display and exports. The Shots tab follows it.",
            );
            let units_label = match self.settings.default_units {
                UnitSystem::Imperial => "Imperial",
                UnitSystem::Metric => "Metric",
            };
            let combo = egui::ComboBox::from_id_salt("default_units")
                .selected_text(units_label)
                .width(field_width)
                .show_ui(ui, |ui| {
                    for (units, label) in [
                        (UnitSystem::Imperial, "Imperial"),
                        (UnitSystem::Metric, "Metric"),
                    ] {
                        let item = ui.selectable_label(self.settings.default_units == units, label);
                        devices::mark(ui, || format!("units:{}", label.to_lowercase()), &item);
                        if item.clicked() {
                            self.set_default_units(units);
                        }
                    }
                });
            devices::mark(ui, || "units".to_owned(), &combo.response);
        });
        // A fixed row height, the segmented control's, so the label
        // centers on it.
        let row = egui::vec2(ui.available_width(), 36.0);
        ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                field_label(ui, "Default shot view:").on_hover_text("Which view the Shots tab opens in. Sets are always recorded; this only changes how they're shown.");
                let modes = crate::groups::GROUPING_MODES;
                let selected = modes
                    .iter()
                    .position(|(m, _)| *m == self.settings.shot_grouping)
                    .unwrap_or(0);
                let labels = modes.map(|(_, label)| label);
                if let Some(i) = theme::segmented(ui, &labels, selected, None).inner {
                    self.settings.shot_grouping = modes[i].0;
                    self.settings.mark_general(GeneralCard::Global);
                }
            });

        // --- Club-to-mode mapping ---
        ui.add_space(6.0);
        for (label, tooltip, mode_clubs, other_clubs, options) in [
            (
                "Chipping Clubs:",
                "When a game selects one of these wedges, the launch monitor will be set to chipping mode.",
                "chipping",
                "putting",
                flighthook::chipping_club_options(),
            ),
            (
                "Putting Clubs:",
                "When a game selects the putter, the launch monitor will be set to putting mode.",
                "putting",
                "chipping",
                flighthook::putting_club_options(),
            ),
        ] {
            ui.horizontal_wrapped(|ui| {
                field_label(ui, label).on_hover_text(tooltip);
                for &club in options {
                    let in_this = match mode_clubs {
                        "chipping" => self.settings.chipping_clubs.contains(&club),
                        _ => self.settings.putting_clubs.contains(&club),
                    };
                    let label = egui::RichText::new(format!("{club}")).size(12.0);
                    let btn = if in_this {
                        egui::Button::new(label.strong().color(theme::ON_ACCENT))
                            .fill(theme::ACCENT)
                    } else {
                        egui::Button::new(label)
                    };
                    if ui.add(btn).clicked() {
                        // Toggle: remove if present, add if absent
                        if in_this {
                            match mode_clubs {
                                "chipping" => self.settings.chipping_clubs.retain(|c| *c != club),
                                _ => self.settings.putting_clubs.retain(|c| *c != club),
                            }
                        } else {
                            // Remove from the other list first
                            match other_clubs {
                                "chipping" => self.settings.chipping_clubs.retain(|c| *c != club),
                                _ => self.settings.putting_clubs.retain(|c| *c != club),
                            }
                            match mode_clubs {
                                "chipping" => self.settings.chipping_clubs.push(club),
                                _ => self.settings.putting_clubs.push(club),
                            }
                        }
                        self.settings.mark_general(GeneralCard::Global);
                    }
                }
            });
        }
    }

    /// The Simulation card: simulated values (beta), saved with the Global
    /// card. Its edit controls are disabled unless `editable`.
    pub(crate) fn render_simulation_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
    ) {
        let card = devices::fixed_card(ui, "simulation", theme::HEADING, |card| {
            let ui = card.top();
            let dirty = card_header(
                ui,
                "simulation",
                "Simulation",
                Some(("beta", TagPlace::Title)),
                Some(SIMULATION_HOVER),
                |ui| self.general_buttons(ctx, ui, GeneralCard::Simulation),
            );
            if dirty {
                lock_hint(ui);
                self.general_error(ui, "simulation");
            }
            card.body(false, |ui| {
                ui.add_space(6.0);
                ui.add_enabled_ui(editable, |ui| self.simulation_fields(ui));
            });
        });
        devices::mark(ui, || "card:simulation".to_owned(), &card.response);
    }

    /// The Replay card: the cameras' clips (beta), saved with the Global
    /// card. Its edit controls are disabled unless `editable`.
    pub(crate) fn render_replay_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
    ) {
        let card = devices::fixed_card(ui, "replay", theme::HEADING, |card| {
            let ui = card.top();
            let dirty = card_header(
                ui,
                "replay",
                "Replay",
                Some(("beta", TagPlace::Title)),
                Some(REPLAY_HOVER),
                |ui| self.general_buttons(ctx, ui, GeneralCard::Replay),
            );
            if dirty {
                lock_hint(ui);
                self.general_error(ui, "replay");
            }
            card.body(false, |ui| {
                ui.add_space(6.0);
                ui.add_enabled_ui(editable, |ui| self.replay_fields(ui));
            });
        });
        devices::mark(ui, || "card:replay".to_owned(), &card.response);
    }

    /// The Body tracking card (alpha): every body tracking setting, saved
    /// with the Global card (turning it on from Off saves at once, after the
    /// model download prompt). Its edit controls are disabled unless
    /// `editable`.
    pub(crate) fn render_body_tracking_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
    ) {
        let card = devices::fixed_card(ui, "body_tracking", theme::HEADING, |card| {
            let ui = card.top();
            let dirty = card_header(
                ui,
                "body_tracking",
                "Body tracking",
                Some(("alpha", TagPlace::Title)),
                None,
                |ui| self.general_buttons(ctx, ui, GeneralCard::BodyTracking),
            );
            if dirty {
                lock_hint(ui);
                self.general_error(ui, "body_tracking");
            }
            // Six rows: no space above the first and a point less between
            // them than on the other cards, so they fit the card's height.
            card.body(false, |ui| {
                ui.spacing_mut().item_spacing.y -= 1.0;
                ui.add_enabled_ui(editable, |ui| self.body_tracking_fields(ui));
            });
        });
        devices::mark(ui, || "card:body_tracking".to_owned(), &card.response);
    }

    /// The Body tracking card's fields: Mode, Model, Show skeleton, Minimum
    /// score, Smoothing and Detect every, each with its range
    /// under it while out of range. Turning body tracking on from Off goes
    /// through the model download
    /// prompt ([`FlighthookApp::turn_body_tracking_on`]), which saves the
    /// mode itself; every other change is an edit saved with Save.
    fn body_tracking_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);
        let off = self.body_tracking().mode == BodyTrackingMode::Off;
        let mut turn_on = None;
        let form = &mut self.settings.body_tracking;
        let mut changed = false;
        ui.horizontal(|ui| {
            field_label(ui, "Mode:");
            let modes = BodyTrackingMode::all();
            let labels = modes.map(BodyTrackingMode::label);
            let selected = modes.iter().position(|m| *m == form.mode).unwrap_or(1);
            let toggle = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
            devices::mark(ui, || "body_tracking_mode".to_owned(), &toggle.response);
            let (clicked, pills) = toggle.inner;
            for (mode, pill) in modes.iter().zip(&pills) {
                devices::mark(ui, || format!("body_tracking_mode:{}", mode.key()), pill);
            }
            if let Some(i) = clicked {
                if off && modes[i].is_on() {
                    turn_on = Some(modes[i]);
                } else {
                    changed |= modes[i] != form.mode;
                    form.mode = modes[i];
                }
            }
        });
        ui.horizontal(|ui| {
            field_label(ui, "Model:");
            let models = PoseModel::all();
            let labels = models.map(PoseModel::label);
            let selected = models.iter().position(|m| *m == form.model).unwrap_or(0);
            let picker = theme::segmented(ui, &labels, selected, None);
            devices::mark(ui, || "body_tracking_model".to_owned(), &picker.response);
            if let Some(i) = picker.inner {
                changed |= models[i] != form.model;
                form.model = models[i];
            }
        });
        ui.horizontal(|ui| {
            field_indent(ui);
            let check = ui.checkbox(&mut form.show_skeleton, "Show skeleton");
            devices::mark(ui, || "body_tracking_skeleton".to_owned(), &check);
            changed |= check.changed();
        });
        let mut number_field =
            |ui: &mut egui::Ui, label: &str, mark: &str, text: &mut String, unit: Option<&str>| {
                ui.horizontal(|ui| {
                    field_label(ui, label);
                    let field = ui.add(
                        egui::TextEdit::singleline(text)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    );
                    devices::mark(ui, || mark.to_owned(), &field);
                    changed |= field.changed();
                    if let Some(unit) = unit {
                        ui.label(unit);
                    }
                });
            };
        number_field(
            ui,
            "Minimum score:",
            "body_tracking_min_score",
            &mut form.min_score,
            None,
        );
        if form.min_score_value().is_none() {
            field_error(ui, "0 to 1");
        }
        number_field(
            ui,
            "Smoothing:",
            "body_tracking_smoothing",
            &mut form.smoothing,
            Some("Hz"),
        );
        if form.smoothing_value().is_none() {
            field_error(
                ui,
                &format!(
                    "0, or {} to {} Hz",
                    BodyTrackingSection::MIN_SMOOTHING_HZ,
                    BodyTrackingSection::MAX_SMOOTHING_HZ
                ),
            );
        }
        number_field(
            ui,
            "Detect every:",
            "body_tracking_detect_every",
            &mut form.detect_every,
            Some("frames"),
        );
        if form.detect_every_value().is_none() {
            field_error(
                ui,
                &format!("1 to {}", BodyTrackingSection::MAX_DETECT_EVERY),
            );
        }
        if changed {
            self.settings.mark_general(GeneralCard::BodyTracking);
        }
        if let Some(mode) = turn_on {
            let ctx = ui.ctx().clone();
            self.turn_body_tracking_on(&ctx, mode);
        }
    }

    /// The Player card: facts about the player the swing analysis uses,
    /// saved with the Global card. Its edit controls are disabled unless
    /// `editable`.
    pub(crate) fn render_player_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        editable: bool,
    ) {
        let card = devices::fixed_card(ui, "player", theme::HEADING, |card| {
            let ui = card.top();
            let dirty = card_header(ui, "player", "Player", None, None, |ui| {
                self.general_buttons(ctx, ui, GeneralCard::Player)
            });
            if dirty {
                lock_hint(ui);
                self.general_error(ui, "player");
            }
            card.body(false, |ui| {
                ui.add_space(6.0);
                ui.add_enabled_ui(editable, |ui| self.player_fields(ui));
            });
        });
        devices::mark(ui, || "card:player".to_owned(), &card.response);
    }

    /// The Player card's fields: Sex and Height, the height's range under
    /// it while out of range.
    fn player_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);
        let form = &mut self.settings.player;
        let mut changed = false;
        ui.horizontal(|ui| {
            field_label(ui, "Sex:").on_hover_text("Men's or women's tour averages");
            let all = Sex::all();
            let labels = all.map(Sex::label);
            let selected = all.iter().position(|s| *s == form.sex).unwrap_or(0);
            let toggle = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
            devices::mark(ui, || "player_sex".to_owned(), &toggle.response);
            let (clicked, pills) = toggle.inner;
            for (sex, pill) in all.iter().zip(&pills) {
                devices::mark(ui, || format!("player_sex:{}", sex.key()), pill);
            }
            if let Some(i) = clicked {
                changed |= all[i] != form.sex;
                form.sex = all[i];
            }
        });
        ui.horizontal(|ui| {
            field_label(ui, "Height:").on_hover_text(
                "With the height, swing measurements that are distances are shown in \
                 inches or centimeters.",
            );
            let field = ui.add(
                egui::TextEdit::singleline(&mut form.height)
                    .desired_width(field_width - TEXT_EDIT_MARGIN),
            );
            devices::mark(ui, || "player_height".to_owned(), &field);
            changed |= field.changed();
            ui.label(form.height_unit());
        });
        if !form.height_valid() {
            let (min, max) = (PlayerSection::MIN_HEIGHT_CM, PlayerSection::MAX_HEIGHT_CM);
            field_error(
                ui,
                &if form.inches {
                    format!(
                        "{:.0} to {:.0} in",
                        (min / 2.54).ceil(),
                        (max / 2.54).floor()
                    )
                } else {
                    format!("{min:.0} to {max:.0} cm")
                },
            );
        }
        if changed {
            self.settings.mark_general(GeneralCard::Player);
        }
    }

    /// The Replay card's fields.
    fn replay_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);
        let replay = &mut self.settings.replay;
        let mut changed = false;
        ui.horizontal(|ui| {
            field_label(ui, "Keep clips:").on_hover_text(format!(
                "How many clips stay on disk, 1 to {}. Each camera's clip of a 5.5 s \
                 window at 1280×720, 120 fps is about 55 MB.",
                ReplaySection::MAX_KEEP_SHOTS
            ));
            let field = ui.add(
                egui::TextEdit::singleline(&mut replay.keep_shots)
                    .desired_width(field_width - TEXT_EDIT_MARGIN),
            );
            devices::mark(ui, || "replay_keep".to_owned(), &field);
            changed |= field.changed();
        });
        if !replay
            .keep_shots
            .trim()
            .parse::<u32>()
            .is_ok_and(|n| (1..=ReplaySection::MAX_KEEP_SHOTS).contains(&n))
        {
            field_error(ui, &format!("1 to {}", ReplaySection::MAX_KEEP_SHOTS));
        }
        for (label, hover, text, loaded) in [
            (
                "Pre-roll:",
                Some("Seconds recorded before the estimated impact, 0 to 10."),
                &mut replay.pre_roll_s,
                replay.loaded.pre_roll_s,
            ),
            (
                "Post-roll:",
                Some("Seconds recorded after the launch monitor reports the shot, 0 to 10."),
                &mut replay.post_roll_s,
                replay.loaded.post_roll_s,
            ),
            (
                "Start before impact:",
                None,
                &mut replay.start_before_impact_s,
                replay.loaded.start_before_impact_s,
            ),
        ] {
            ui.horizontal(|ui| {
                let label = field_label(ui, label);
                if let Some(hover) = hover {
                    label.on_hover_text(hover);
                }
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(text)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed();
                ui.label("s");
            });
            if ReplayForm::seconds(text, loaded)
                .is_none_or(|v| !(v.is_finite() && (0.0..=ReplaySection::MAX_ROLL_S).contains(&v)))
            {
                field_error(ui, "0 to 10 s");
            }
        }
        ui.horizontal(|ui| {
            field_indent(ui);
            let check = ui.checkbox(&mut replay.shot_cards, "Show video on shot cards");
            devices::mark(ui, || "replay_shot_cards".to_owned(), &check);
            changed |= check.changed();
        });
        let folder = replay
            .clip_dir()
            .map(str::to_owned)
            .or_else(|| self.clip_dir.clone());
        ui.horizontal(|ui| {
            field_label(ui, "Clip folder:")
                .on_hover_text("Where the clips are written: an AVI file and its index per camera, in a folder per clip.");
            let text = folder.as_deref().unwrap_or("-");
            let shown = ui.add(
                egui::Label::new(egui::RichText::new(text).color(theme::TEXT_MUTED)).truncate(),
            );
            devices::mark(ui, || "replay_folder".to_owned(), &shown);
        });
        if changed {
            self.settings.mark_general(GeneralCard::Replay);
        }
    }

    /// The Simulation card's fields.
    fn simulation_fields(&mut self, ui: &mut egui::Ui) {
        let (field_width, _) = field_widths(ui);
        let sim = &mut self.settings.simulation;
        let mut sim_changed = false;
        ui.horizontal(|ui| {
            field_label(ui, "Simulation:").on_hover_text(sim_mode_hover(sim.mode));
            egui::ComboBox::from_id_salt("sim_mode")
                .selected_text(sim.mode.label())
                .width(field_width)
                .show_ui(ui, |ui| {
                    for mode in SimMode::all() {
                        if ui
                            .selectable_label(sim.mode == mode, mode.label())
                            .on_hover_text(sim_mode_hover(mode))
                            .clicked()
                        {
                            sim.mode = mode;
                            sim_changed = true;
                        }
                    }
                });
        });
        ui.add_enabled_ui(sim.mode != SimMode::Off, |ui| {
            ui.horizontal(|ui| {
                field_label(ui, "Altitude:")
                    .on_hover_text("Altitude above sea level, 0 to 5000 m (16,404 ft).");
                sim_changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut sim.altitude_val)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed();
                egui::ComboBox::from_id_salt("sim_altitude_unit")
                    .selected_text(unit_suffix(&sim.altitude_unit))
                    .width(UNIT_WIDTH)
                    .show_ui(ui, |ui| {
                        for &(key, label) in ALTITUDE_UNITS {
                            if ui
                                .selectable_label(sim.altitude_unit == key, label)
                                .clicked()
                            {
                                sim.altitude_unit = key.to_string();
                                sim_changed = true;
                            }
                        }
                    });
            });
            if sim.altitude().is_none_or(|a| {
                let m = a.as_meters();
                !(m.is_finite() && (0.0..=SimulationSection::MAX_ALTITUDE_M).contains(&m))
            }) {
                field_error(ui, "0 to 5000 m");
            }
            ui.horizontal(|ui| {
                field_label(ui, "Temperature:").on_hover_text(
                    "Air temperature, -30 to 50 °C (-22 to 122 °F). Defaults to 25 °C (77 °F).",
                );
                sim_changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut sim.temperature_val)
                            .desired_width(field_width - TEXT_EDIT_MARGIN),
                    )
                    .changed();
                ui.label(sim.temperature_unit());
            });
            if sim.temperature_c().is_none_or(|c| {
                !(SimulationSection::MIN_TEMPERATURE_C..=SimulationSection::MAX_TEMPERATURE_C)
                    .contains(&c)
            }) {
                let range = if sim.fahrenheit {
                    "-22 to 122 °F"
                } else {
                    "-30 to 50 °C"
                };
                field_error(ui, range);
            }
            ui.horizontal(|ui| {
                field_label(ui, "Surface:")
                    .on_hover_text("Flat ground the ball lands and rolls on.");
                egui::ComboBox::from_id_salt("sim_surface")
                    .selected_text(sim.surface.label())
                    .width(field_width)
                    .show_ui(ui, |ui| {
                        for surface in SimSurface::all() {
                            if ui
                                .selectable_label(sim.surface == surface, surface.label())
                                .clicked()
                            {
                                sim.surface = surface;
                                sim_changed = true;
                            }
                        }
                    });
            });
        });
        ui.horizontal(|ui| {
            field_indent(ui);
            if ui
                .checkbox(&mut sim.auto_chip, "Auto-chip")
                .on_hover_text(AUTO_CHIP_SETTING_HOVER)
                .changed()
            {
                sim_changed = true;
            }
        });
        if sim_changed {
            self.settings.mark_general(GeneralCard::Simulation);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::is_ble_address;
    use super::{ActorFormEntry, GeneralCard, SettingsForm, SimulationForm};
    use crate::panels::cards::{self, CardGroup};
    use crate::panels::devices::device_card_order;
    use crate::types::{
        ActorStatus, ActorStatusResponse, Club, Distance, FlighthookConfig, MevoSection,
        MockMonitorSection, RandomClubSection, SimSurface, SimulationSection, UnitSystem,
    };
    use flighthook::{MockConfig, R10Section, SimMode};

    #[test]
    fn settings_are_requested_at_most_every_two_seconds_until_loaded() {
        let mut form = SettingsForm::default();
        assert!(form.fetch_due(10.0));
        // Not every frame while the request is out.
        assert!(!form.fetch_due(10.1));
        assert!(!form.fetch_due(11.9));
        // Unanswered: asked again.
        assert!(form.fetch_due(12.0));
        form.loaded = true;
        assert!(!form.fetch_due(100.0));
        // A save reloads at once.
        form.loaded = false;
        form.fetched_at = None;
        assert!(form.fetch_due(100.1));
    }

    /// IDs of the device cards of `form` by group, none of them running.
    fn card_ids(form: &SettingsForm) -> Vec<(CardGroup, Vec<String>)> {
        device_card_order(&HashMap::new(), &form.actors)
            .into_iter()
            .map(|(g, cards)| (g, cards.into_iter().map(|c| c.id).collect()))
            .collect()
    }

    /// A form with two of every type the (+) menus offer, plus a mock
    /// monitor and a random club, loaded from the config they save as.
    fn form_with_every_type() -> SettingsForm {
        let mut form = SettingsForm::default();
        form.load_from(&FlighthookConfig::default());
        for group in CardGroup::ALL {
            for preset in cards::add_options(group) {
                form.add_preset(preset);
                form.add_preset(preset);
            }
        }
        let mut config = form.to_request();
        config.mock_monitor.insert(
            "0".into(),
            MockMonitorSection {
                name: "Mock Monitor".into(),
                enabled: true,
                impact_lead_s: None,
                driver: Default::default(),
            },
        );
        config.random_club.insert(
            "0".into(),
            RandomClubSection {
                name: "Random Club".into(),
                enabled: true,
            },
        );
        // No menu entry adds a Pureplay Connect output; the file can.
        for i in ["0", "1"] {
            config.pureplay.insert(
                i.into(),
                crate::types::PureplaySection {
                    name: "Pureplay Connect".into(),
                    ..Default::default()
                },
            );
        }
        let mut loaded = SettingsForm::default();
        loaded.load_from(&config);
        loaded
    }

    #[test]
    fn settings_cards_group_by_type_then_index() {
        let form = form_with_every_type();
        let order = card_ids(&form);
        let ids = |group: CardGroup| -> Vec<String> {
            order.iter().find(|(g, _)| *g == group).unwrap().1.clone()
        };
        assert_eq!(ids(CardGroup::General), ["webserver.0"]);
        assert_eq!(
            ids(CardGroup::LaunchMonitors),
            [
                "mevo.0",
                "mevo.1",
                "r10.0",
                "r10.1",
                "square.0",
                "square.1",
                "exputt.0",
                "exputt.1",
                "trackball.0",
                "trackball.1",
                "mock_monitor.0",
                "openconnect_server.0",
                "openconnect_server.1",
                "openconnect_server.2",
                "openconnect_server.3",
            ]
        );
        assert_eq!(
            ids(CardGroup::Outputs),
            [
                "openconnect_client.0",
                "openconnect_client.1",
                "openconnect_client.2",
                "openconnect_client.3",
                "openconnect_client.4",
                "openconnect_client.5",
                "pureplay.0",
                "pureplay.1",
                "random_club.0",
            ]
        );
    }

    #[test]
    fn a_card_keeps_its_place_whether_running_configured_or_both() {
        let form = form_with_every_type();
        let actors: HashMap<String, ActorStatusResponse> = form
            .actors
            .iter()
            .map(|a| {
                let status = ActorStatusResponse::new(a.name(), ActorStatus::Connected);
                (a.global_id(), status)
            })
            .collect();
        let ids = |cards: Vec<(CardGroup, Vec<crate::panels::devices::DeviceCard>)>| {
            cards
                .into_iter()
                .map(|(g, cards)| (g, cards.into_iter().map(|c| c.id).collect()))
                .collect::<Vec<(CardGroup, Vec<String>)>>()
        };
        let configured = card_ids(&form);
        // Running and configured
        assert_eq!(ids(device_card_order(&actors, &form.actors)), configured);
        // Running, before the settings load
        assert_eq!(ids(device_card_order(&actors, &[])), configured);
    }

    #[test]
    fn add_puts_a_dirty_entry_in_the_group_that_offered_it() {
        for group in CardGroup::ALL {
            for preset in cards::add_options(group) {
                let type_key = preset.type_key;
                let mut form = SettingsForm::default();
                form.load_from(&FlighthookConfig::default());
                form.add_preset(preset);
                form.add_preset(preset);
                let added: Vec<&ActorFormEntry> = form
                    .actors
                    .iter()
                    .filter(|a| a.type_key() == type_key && a.dirty())
                    .collect();
                assert_eq!(added.len(), 2, "{type_key}");
                for a in &added {
                    assert_eq!(a.name(), preset.name, "{}", preset.label);
                }
                assert!(form.dirty());
                let order = card_ids(&form);
                let members = &order.iter().find(|(g, _)| *g == group).unwrap().1;
                for a in &added {
                    assert!(
                        members.contains(&a.global_id()),
                        "{} not under {}",
                        a.global_id(),
                        group.title()
                    );
                }
                // The lowest unused index for the type, from 0
                let mut indices: Vec<&str> = added.iter().map(|a| a.index()).collect();
                indices.sort_unstable();
                let first = form
                    .actors
                    .iter()
                    .filter(|a| a.type_key() == type_key)
                    .count()
                    - 2;
                assert_eq!(indices, [first.to_string(), (first + 1).to_string()]);
            }
        }
    }

    #[test]
    fn add_offers_only_known_types() {
        let mut form = SettingsForm::default();
        form.load_from(&FlighthookConfig::default());
        let before = form.actors.len();
        assert_eq!(form.add_actor("mock_monitor"), None);
        assert_eq!(form.add_actor("random_club"), None);
        assert_eq!(form.actors.len(), before);
        assert_eq!(
            form.add_actor("openconnect_client").as_deref(),
            Some("openconnect_client.0")
        );
    }

    #[test]
    fn cancel_restores_a_saved_entry_and_drops_a_new_one() {
        let mut form = form_with_every_type();
        let idx = |form: &SettingsForm, id: &str| {
            form.actors
                .iter()
                .position(|a| a.global_id() == id)
                .unwrap()
        };
        let saved_count = form.actors.len();

        // A saved entry: back to the saved values, clean.
        let i = idx(&form, "mevo.1");
        if let ActorFormEntry::Device(d) = &mut form.actors[i] {
            d.address = "10.0.0.9:5100".into();
            d.track_pct = "55".into();
            d.dirty = true;
        }
        assert!(form.locked());
        assert!(!form.cancel_actor(i));
        let ActorFormEntry::Device(d) = &form.actors[idx(&form, "mevo.1")] else {
            panic!("mevo.1 is a device");
        };
        assert_eq!(d.address, "192.168.2.1:5100");
        assert_eq!(d.track_pct, "80");
        assert!(!d.dirty);
        assert!(!form.dirty());
        assert!(!form.locked());
        assert_eq!(form.actors.len(), saved_count);
        assert_eq!(form.to_request(), form.original_config.clone().unwrap());

        // A new entry: gone.
        let id = form.add_actor("square").unwrap();
        assert!(form.locked());
        assert!(form.cancel_actor(idx(&form, &id)));
        assert_eq!(form.actors.len(), saved_count);
        assert!(!form.locked());

        // After a save, the saved values are what a cancel goes back to.
        let id = form.add_actor("openconnect_client").unwrap();
        let _ = form.begin_save(super::SaveTarget::Actor(id.clone()));
        assert_eq!(form.finish_save(Ok(())), None);
        let i = idx(&form, &id);
        assert!(!form.actors[i].dirty());
        if let ActorFormEntry::Integration(e) = &mut form.actors[i] {
            e.name = "Bay 3".into();
            e.dirty = true;
        }
        assert!(!form.cancel_actor(i));
        assert_eq!(form.actors[i].name(), "Open Connect");
        assert_eq!(form.actors.len(), saved_count + 1);
    }

    #[test]
    fn general_edits_mark_the_card_they_were_made_on() {
        let mut form = SettingsForm::default();
        form.load_from(&FlighthookConfig::default());
        for card in [GeneralCard::Global, GeneralCard::Simulation] {
            assert!(!form.general_card_dirty(card));
        }
        assert!(!form.locked());

        form.mark_general(GeneralCard::Simulation);
        assert!(form.locked());
        assert!(!form.general_card_dirty(GeneralCard::Global));
        assert!(form.general_card_dirty(GeneralCard::Simulation));
        form.mark_general(GeneralCard::Global);
        assert!(form.general_card_dirty(GeneralCard::Global));
        assert!(form.general_card_dirty(GeneralCard::Simulation));

        // Saved: both clean, and the next edit marks only its own card.
        let _ = form.begin_save(super::SaveTarget::Global);
        assert_eq!(form.finish_save(Ok(())), None);
        assert!(!form.global_dirty);
        assert!(!form.locked());
        form.mark_general(GeneralCard::Global);
        assert!(form.general_card_dirty(GeneralCard::Global));
        assert!(!form.general_card_dirty(GeneralCard::Simulation));

        // Unsaved Global edits from neither card show on the Global card.
        let mut form = SettingsForm::default();
        form.load_from(&FlighthookConfig::default());
        form.global_dirty = true;
        assert!(form.general_card_dirty(GeneralCard::Global));
        assert!(!form.general_card_dirty(GeneralCard::Simulation));
    }

    #[test]
    fn cancel_general_restores_both_cards() {
        let config = FlighthookConfig {
            chipping_clubs: vec![Club::SandWedge, Club::LobWedge],
            ..FlighthookConfig::default()
        };
        let mut form = SettingsForm::default();
        form.load_from(&config);
        form.default_units = UnitSystem::Metric;
        form.simulation.set_fahrenheit(false);
        form.shot_grouping = crate::types::ShotGrouping::None;
        form.chipping_clubs.push(Club::GapWedge);
        form.mark_general(GeneralCard::Global);
        form.simulation.mode = SimMode::Off;
        form.simulation.altitude_val = "300".into();
        form.mark_general(GeneralCard::Simulation);

        form.cancel_general();
        assert!(!form.global_dirty);
        assert!(!form.locked());
        assert_eq!(form.build_global_request(), config);
        assert!(form.simulation.fahrenheit);
        // Nothing to give back when the units were not changed.
        form.simulation.altitude_val = "200".into();
        form.mark_general(GeneralCard::Simulation);
        form.cancel_general();
        assert_eq!(form.build_global_request(), config);
    }

    #[test]
    fn simulation_form_round_trips_defaults_exactly() {
        for units in [UnitSystem::Imperial, UnitSystem::Metric] {
            let form = SimulationForm::from_section(&SimulationSection::default(), units);
            assert_eq!(form.to_section(), Some(SimulationSection::default()));
        }
        let imperial =
            SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Imperial);
        assert_eq!(imperial.temperature_val, "77");
        assert_eq!(imperial.temperature_unit(), "°F");
    }

    #[test]
    fn sim_mode_round_trips_through_the_form() {
        for mode in SimMode::all() {
            let section = SimulationSection {
                mode,
                ..SimulationSection::default()
            };
            for units in [UnitSystem::Imperial, UnitSystem::Metric] {
                let form = SimulationForm::from_section(&section, units);
                assert_eq!(form.mode, mode);
                assert_eq!(form.to_section(), Some(section.clone()));
            }

            // Picking the mode in a default form saves exactly that mode.
            let mut form =
                SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Metric);
            assert_eq!(form.mode, SimMode::On);
            form.mode = mode;
            assert_eq!(form.to_section(), Some(section.clone()));

            let cfg = FlighthookConfig {
                simulation: section,
                ..FlighthookConfig::default()
            };
            let mut settings = SettingsForm::default();
            settings.load_from(&cfg);
            assert_eq!(settings.simulation.mode, mode);
            let json = serde_json::to_string(&settings.build_global_request()).expect("ser");
            assert!(!json.contains("enabled"), "{json}");
            if mode == SimMode::On {
                assert!(!json.contains(r#""mode""#), "{json}");
            } else {
                let want = format!(r#""mode":"{}""#, mode.key());
                assert!(json.contains(&want), "{json}");
            }
        }
    }

    /// The Auto-chip setting loads into the form, is edited there, and goes
    /// out in the save request only while enabled (the default stays off
    /// the wire).
    #[test]
    fn the_auto_chip_setting_round_trips_through_the_form() {
        let mut settings = SettingsForm::default();
        assert!(
            !settings.auto_chip_enabled(),
            "off before the settings load"
        );
        settings.load_from(&FlighthookConfig::default());
        assert!(!settings.simulation.auto_chip, "disabled by default");
        assert!(!settings.auto_chip_enabled());
        let json = serde_json::to_string(&settings.build_global_request()).expect("ser");
        assert!(!json.contains("auto_chip"), "{json}");

        settings.simulation.auto_chip = true;
        settings.mark_general(GeneralCard::Simulation);
        assert!(
            settings
                .simulation
                .to_section()
                .is_some_and(|s| s.auto_chip)
        );
        let json = serde_json::to_string(&settings.build_global_request()).expect("ser");
        assert!(json.contains(r#""auto_chip":true"#), "{json}");
        assert!(
            !settings.auto_chip_enabled(),
            "the header follows the saved setting, not the edit"
        );

        let on = FlighthookConfig {
            simulation: SimulationSection {
                auto_chip: true,
                ..SimulationSection::default()
            },
            ..FlighthookConfig::default()
        };
        let mut loaded = SettingsForm::default();
        loaded.load_from(&on);
        assert!(loaded.simulation.auto_chip);
        assert!(loaded.auto_chip_enabled());
    }

    #[test]
    fn untouched_odd_values_save_unchanged() {
        let s = SimulationSection {
            altitude: Distance::Yards(12.345),
            temperature_c: 21.37,
            ..SimulationSection::default()
        };
        let form = SimulationForm::from_section(&s, UnitSystem::Imperial);
        assert_eq!(form.to_section(), Some(s));
    }

    #[test]
    fn metric_units_show_a_zero_altitude_in_meters() {
        let form = SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Metric);
        assert_eq!(form.altitude_unit, "meters");
        // Untouched, it still saves as the exact loaded (imperial) default.
        assert_eq!(form.to_section(), Some(SimulationSection::default()));

        let imperial =
            SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Imperial);
        assert_eq!(imperial.altitude_unit, "feet");
    }

    #[test]
    fn fahrenheit_is_stored_as_celsius() {
        let mut form =
            SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Imperial);
        form.temperature_val = "86".into();
        assert_eq!(form.to_section().map(|s| s.temperature_c), Some(30.0));
        form.temperature_val = "50.5".into();
        assert_eq!(form.to_section().map(|s| s.temperature_c), Some(10.3));
        form.set_fahrenheit(false);
        assert_eq!(form.temperature_val, "10.3");
        assert_eq!(form.to_section().map(|s| s.temperature_c), Some(10.3));
    }

    #[test]
    fn out_of_range_or_unparsable_values_are_invalid() {
        let base = SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Metric);
        for (alt, unit, temp) in [
            ("5001", "meters", "25"),
            ("16500", "feet", "25"),
            ("-1", "meters", "25"),
            ("abc", "meters", "25"),
            ("0", "meters", "50.2"),
            ("0", "meters", "-31"),
            ("0", "meters", "warm"),
            ("NaN", "meters", "25"),
        ] {
            let mut f = base.clone();
            f.altitude_val = alt.into();
            f.altitude_unit = unit.into();
            f.temperature_val = temp.into();
            assert_eq!(f.to_section(), None, "{alt} {unit} {temp}");
            let form = SettingsForm {
                simulation: f,
                ..SettingsForm::default()
            };
            assert!(!form.general_valid(), "{alt} {unit} {temp}");
        }
        let mut f = base;
        f.altitude_val = "5280".into();
        f.altitude_unit = "feet".into();
        f.surface = SimSurface::Green;
        let s = f.to_section().expect("valid");
        assert!((s.altitude.as_meters() - 1609.344).abs() < 1e-9);
        assert_eq!(s.surface, SimSurface::Green);
    }

    #[test]
    fn loading_a_default_config_keeps_simulation_off_the_wire() {
        let mut form = SettingsForm::default();
        form.load_from(&FlighthookConfig::default());
        let json = serde_json::to_string(&form.build_global_request()).expect("ser");
        assert!(!json.contains("simulation"), "{json}");
        assert!(form.general_valid());
    }

    #[test]
    fn launch_only_spin_survives_a_settings_save() {
        for k in [100.0, 0.0, 87.5] {
            let mut cfg = FlighthookConfig::default();
            cfg.mevo.insert(
                "0".into(),
                MevoSection {
                    launch_only_spin: k,
                    ..MevoSection::default()
                },
            );
            let mut form = SettingsForm::default();
            form.load_from(&cfg);
            assert_eq!(form.to_request().mevo["0"].launch_only_spin, k);
            assert!(super::actor_valid(&form.actors[0]), "{k}");

            let actor = form.actors[0].clone();
            let mut orig = FlighthookConfig::default();
            super::apply_actor_to_config(&mut orig, &actor);
            assert_eq!(orig.mevo["0"].launch_only_spin, k);
        }
    }

    /// The mock card's driver settings, which the form does not show,
    /// survive a save of the card; settings the driver refuses keep the
    /// card from saving, with the field named.
    #[test]
    fn mock_driver_settings_survive_a_save_and_are_validated() {
        let mut cfg = FlighthookConfig::default();
        let driver = MockConfig {
            shot_interval_s: 5.0,
            serial: Some("7".into()),
            ..MockConfig::default()
        };
        cfg.mock_monitor.insert(
            "0".into(),
            MockMonitorSection {
                name: "Mock".into(),
                driver: driver.clone(),
                ..MockMonitorSection::default()
            },
        );
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        assert!(super::actor_valid(&form.actors[0]));
        assert_eq!(
            form.build_actor_request("mock_monitor.0").mock_monitor["0"].driver,
            driver
        );
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        assert_eq!(dev.driver_error(), None);
        dev.mock.shot_interval_s = -1.0;
        assert_eq!(
            dev.driver_error().as_deref(),
            Some("shot_interval_s: must be 0 to 86400")
        );
        assert!(!super::actor_valid(&form.actors[0]));
    }

    /// The Trackball card saves its section as loaded unless a field
    /// changed: an unchanged card writes it byte for byte, an edited field
    /// alone is written, a blank number is the driver's default, a loaded
    /// `grab` is left out, and a new card writes its name.
    #[test]
    fn trackball_settings_are_saved_as_loaded_unless_changed() {
        let json = |section: &flighthook::TrackballSection| serde_json::to_string(section).unwrap();
        for loaded in [
            r#"{"name":"Arcade","device":"usb-Example_Trackball-event-mouse","club":"DR","hand":"lh","cpi":800.0,"report_limit":2047,"thresholds":{"v_start":0.0712,"coil_timeout_s":20.0},"tuning":{"full_power_speed":41.3,"curve_sense":"physical","mirror_for_left_hand":true}}"#,
            r#"{"name":"Trackball"}"#,
        ] {
            let section: flighthook::TrackballSection = serde_json::from_str(loaded).unwrap();
            let mut cfg = FlighthookConfig::default();
            cfg.trackball.insert("0".into(), section);
            let mut form = SettingsForm::default();
            form.load_from(&cfg);
            assert!(super::actor_valid(&form.actors[0]), "{loaded}");
            let saved = form.build_actor_request("trackball.0");
            assert_eq!(json(&saved.trackball["0"]), loaded);
        }
        for loaded in [
            r#"{"name":"Trackball","grab":true}"#,
            r#"{"name":"Trackball","grab":false}"#,
        ] {
            let section: flighthook::TrackballSection = serde_json::from_str(loaded).unwrap();
            let mut cfg = FlighthookConfig::default();
            cfg.trackball.insert("0".into(), section);
            let mut form = SettingsForm::default();
            form.load_from(&cfg);
            let saved = form.build_actor_request("trackball.0");
            assert_eq!(json(&saved.trackball["0"]), r#"{"name":"Trackball"}"#);
            assert!(!saved.trackball["0"].driver.config.grab, "{loaded}");
        }

        let mut cfg = FlighthookConfig::default();
        cfg.trackball.insert(
            "0".into(),
            serde_json::from_str(
                r#"{"name":"Trackball","cpi":800.0,"tuning":{"power_gamma":1.5}}"#,
            )
            .unwrap(),
        );
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.trackball_form()
            .numbers
            .insert("tuning.max_club_speed", "110".into());
        dev.trackball_form().numbers.insert("cpi", " ".into());
        dev.trackball_form().device = " event7 ".into();
        dev.trackball_form().club = flighthook::TrackballClub::Putter;
        dev.dirty = true;
        assert!(super::actor_valid(&form.actors[0]));
        let saved = form.build_actor_request("trackball.0");
        assert_eq!(
            json(&saved.trackball["0"]),
            r#"{"name":"Trackball","device":"event7","club":"PT","tuning":{"max_club_speed":110.0,"power_gamma":1.5}}"#,
            "blank CPI is the default, left out"
        );

        let mut form = SettingsForm::default();
        form.load_from(&FlighthookConfig::default());
        let preset = cards::add_options(CardGroup::LaunchMonitors)
            .iter()
            .find(|p| p.type_key == "trackball")
            .expect("a Trackball preset");
        let id = form.add_preset(preset).expect("added");
        assert_eq!(id, "trackball.0");
        let saved = form.build_actor_request(&id);
        assert_eq!(json(&saved.trackball["0"]), r#"{"name":"Trackball"}"#);

        // Only a Trackball card carries the Trackball fields.
        let mut cfg = FlighthookConfig::default();
        cfg.trackball
            .insert("0".into(), flighthook::TrackballSection::default());
        cfg.mock_monitor
            .insert("0".into(), MockMonitorSection::default());
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        form.add_preset(preset).expect("added");
        let devices: Vec<&super::DeviceFormEntry> = form
            .actors
            .iter()
            .filter_map(|a| match a {
                ActorFormEntry::Device(dev) => Some(dev),
                _ => None,
            })
            .collect();
        assert_eq!(devices.iter().filter(|d| d.is_trackball()).count(), 2);
        assert!(devices.iter().any(|d| !d.is_trackball()));
        for dev in devices {
            assert_eq!(dev.trackball.is_some(), dev.is_trackball(), "{}", dev.id);
        }
    }

    /// The Trackball card checks its settings with the driver's own
    /// validation and refuses text that is not a number; a stored refused
    /// value does not lock the card.
    #[test]
    fn the_trackball_settings_are_validated_by_its_driver() {
        let mut cfg = FlighthookConfig::default();
        cfg.trackball
            .insert("0".into(), flighthook::TrackballSection::default());
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        for (key, typed, error) in [
            ("cpi", "0", "cpi: must be greater than 0"),
            ("cpi", "lots", "cpi: not a number"),
            ("report_limit", "0", "report_limit: must be at least 1"),
            (
                "thresholds.gap_ms",
                "2.5",
                "thresholds.gap_ms: not a whole number",
            ),
            (
                "thresholds.speed_window_ms",
                "-1",
                "thresholds.speed_window_ms: not a whole number",
            ),
            (
                "thresholds.v_stop",
                "0.5",
                "thresholds.v_stop: must be below thresholds.v_start (0.067)",
            ),
            (
                "tuning.backswing_power",
                "2",
                "tuning.backswing_power: must be 0 to 1",
            ),
            (
                "tuning.power_gamma",
                "0",
                "tuning.power_gamma: must be greater than 0",
            ),
        ] {
            let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
                panic!("not a device");
            };
            let before = dev.trackball_form().numbers[key].clone();
            dev.trackball_form().numbers.insert(key, typed.into());
            dev.dirty = true;
            assert_eq!(dev.driver_error().as_deref(), Some(error), "{key} {typed}");
            assert!(!super::actor_valid(&form.actors[0]), "{key} {typed}");
            let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
                panic!("not a device");
            };
            dev.trackball_form().numbers.insert(key, before);
            assert_eq!(dev.driver_error(), None);
        }

        let mut cfg = FlighthookConfig::default();
        let mut refused = flighthook::TrackballSection::default();
        refused.driver.config.cpi = 0.0;
        cfg.trackball.insert("0".into(), refused);
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        assert_eq!(
            dev.driver_error().as_deref(),
            Some("cpi: must be greater than 0"),
            "the card shows it"
        );
        dev.name = "Arcade".into();
        dev.dirty = true;
        assert!(super::actor_valid(&form.actors[0]), "a rename saves");
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.trackball_form()
            .numbers
            .insert("tuning.power_gamma", "0".into());
        assert!(!super::actor_valid(&form.actors[0]), "another refusal");
    }

    /// A card whose saved driver settings are refused shows the refusal and
    /// still saves its name (its other fields stay as saved); changing the
    /// driver settings into other refused ones blocks the save, into usable
    /// ones does not.
    #[test]
    fn a_refused_saved_driver_setting_does_not_lock_the_card() {
        let mut cfg = FlighthookConfig::default();
        let mut section = MockMonitorSection {
            name: "Mock".into(),
            ..MockMonitorSection::default()
        };
        section.driver.shot_interval_s = -1.0;
        cfg.mock_monitor.insert("0".into(), section);
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        assert_eq!(
            dev.driver_error().as_deref(),
            Some("shot_interval_s: must be 0 to 86400"),
            "the card shows it"
        );
        dev.name = "Renamed".into();
        dev.dirty = true;
        assert!(super::actor_valid(&form.actors[0]), "a rename saves");
        let saved = form.build_actor_request("mock_monitor.0");
        assert_eq!(saved.mock_monitor["0"].name, "Renamed");
        assert_eq!(saved.mock_monitor["0"].driver.shot_interval_s, -1.0);
        assert!(
            form.begin_enable("mock_monitor.0", false).is_some(),
            "the switch saves"
        );

        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.mock.arm_delay_s = -1.0;
        assert!(
            !super::actor_valid(&form.actors[0]),
            "another refused value"
        );
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.mock.arm_delay_s = MockConfig::default().arm_delay_s;
        dev.mock.shot_interval_s = 10.0;
        assert!(super::actor_valid(&form.actors[0]), "fixed");
    }

    /// The ExPutt card's IP Address is the ExPutt driver's `address`: one
    /// the driver refuses (not an IP) shows under the card's fields and
    /// keeps a changed card from saving, blank finds the device, and a
    /// saved refused address still lets the card save its name.
    #[test]
    fn the_exputt_address_is_validated_by_its_driver() {
        let mut cfg = FlighthookConfig::default();
        cfg.exputt.insert(
            "0".into(),
            flighthook::ExPuttSection {
                driver: flighthook::ExPuttConfig {
                    address: Some("exputt.local".into()),
                },
                ..flighthook::ExPuttSection::default()
            },
        );
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        assert_eq!(dev.address, "exputt.local");
        let error = dev.driver_error().expect("shown");
        assert!(
            error.starts_with("address: ") && error.contains("exputt.local"),
            "{error}"
        );
        dev.name = "Renamed".into();
        dev.dirty = true;
        assert!(super::actor_valid(&form.actors[0]), "a rename saves");
        let saved = form.build_actor_request("exputt.0");
        assert_eq!(saved.exputt["0"].name, "Renamed");
        assert_eq!(
            saved.exputt["0"].driver.address.as_deref(),
            Some("exputt.local")
        );
        for bad in ["putter", "192.168.1.50:8889"] {
            let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
                panic!("not a device");
            };
            dev.address = bad.into();
            assert!(dev.driver_error().is_some(), "{bad}");
            assert!(!super::actor_valid(&form.actors[0]), "{bad}");
        }
        for (typed, address) in [("", None), (" 10.0.0.7 ", Some("10.0.0.7"))] {
            let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
                panic!("not a device");
            };
            dev.address = typed.into();
            assert_eq!(dev.driver_error(), None, "{typed:?}");
            assert!(super::actor_valid(&form.actors[0]), "{typed:?}");
            assert_eq!(
                form.build_actor_request("exputt.0").exputt["0"]
                    .driver
                    .address
                    .as_deref(),
                address
            );
        }
    }

    /// The R10 card's Monitor-to-Ball is the R10 driver's `range`: a value
    /// the driver refuses (0 or less) shows under the card's fields and
    /// keeps a changed card from saving, blank means none, and a saved
    /// refused range still lets the card save its name.
    /// The Square card edits the address, the zero-spin switch and the
    /// listed clubs' calibration; the driver's other settings (`club`,
    /// `advanced_spin`, calibration keys the card does not list) survive a
    /// save as they were. The driver refuses nothing, so the card never
    /// shows a driver error.
    #[test]
    fn square_driver_settings_survive_a_save() {
        let mut cfg = FlighthookConfig::default();
        cfg.square.insert(
            "0".into(),
            flighthook::SquareSection {
                driver: flighthook::SquareConfig {
                    address: Some("SquareGolf(CD01)".into()),
                    club: "PW".into(),
                    advanced_spin: false,
                    discard_non_putting_zero_spin: true,
                    dot_bottom_to_face_center_mm: std::collections::BTreeMap::from([
                        ("7i".into(), 21.5),
                        ("PT".into(), 8.0),
                    ]),
                },
                ..flighthook::SquareSection::default()
            },
        );
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        assert_eq!(dev.address, "SquareGolf(CD01)");
        assert!(dev.discard_zero_spin);
        assert_eq!(
            dev.square_impact_mm.get(&Club::Iron7).map(String::as_str),
            Some("21.5")
        );
        assert_eq!(dev.driver_error(), None);
        assert!(!dev.driver_blocks_save());

        // Saved as loaded: the same settings, the 7-iron's key in its code.
        let saved = form.build_actor_request("square.0").square["0"].clone();
        assert_eq!(saved.driver.club, "PW");
        assert!(!saved.driver.advanced_spin);
        assert_eq!(
            saved.driver.dot_bottom_to_face_center_mm,
            std::collections::BTreeMap::from([("7I".into(), 21.5), ("PT".into(), 8.0)])
        );

        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.address = "  ".into();
        dev.discard_zero_spin = false;
        dev.square_impact_mm.insert(Club::Iron7, String::new());
        dev.square_impact_mm.insert(Club::Driver, "19".into());
        assert_eq!(dev.driver_error(), None);
        assert!(super::actor_valid(&form.actors[0]));
        let saved = form.build_actor_request("square.0").square["0"].clone();
        assert_eq!(
            saved.driver,
            flighthook::SquareConfig {
                address: None,
                club: "PW".into(),
                advanced_spin: false,
                discard_non_putting_zero_spin: false,
                dot_bottom_to_face_center_mm: std::collections::BTreeMap::from([
                    ("DR".into(), 19.0),
                    ("PT".into(), 8.0),
                ]),
            }
        );
    }

    #[test]
    fn the_r10_range_is_validated_by_its_driver() {
        let mut cfg = FlighthookConfig::default();
        cfg.r10.insert(
            "0".into(),
            R10Section {
                driver: flighthook::R10Config {
                    range: Some(Distance::Feet(7.0)),
                },
                ..R10Section::default()
            },
        );
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        assert_eq!(
            (dev.range_val.as_str(), dev.range_unit.as_str()),
            ("7", "feet")
        );
        assert_eq!(dev.driver_error(), None);
        for bad in ["0", "-2"] {
            let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
                panic!("not a device");
            };
            dev.range_val = bad.into();
            assert_eq!(
                dev.driver_error().as_deref(),
                Some("range: must be a distance greater than 0"),
                "{bad}"
            );
            assert!(!super::actor_valid(&form.actors[0]), "{bad}");
        }
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.range_val = "7f".into();
        assert_eq!(dev.driver_error().as_deref(), Some("range: not a number"));
        assert!(
            !super::actor_valid(&form.actors[0]),
            "never saved as no range"
        );
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.range_val = String::new();
        assert_eq!(dev.driver_error(), None);
        assert!(super::actor_valid(&form.actors[0]));
        assert_eq!(
            form.build_actor_request("r10.0").r10["0"].driver.range,
            None
        );
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.range_val = "2.5".into();
        dev.range_unit = "meters".into();
        assert_eq!(
            form.build_actor_request("r10.0").r10["0"].driver.range,
            Some(Distance::Meters(2.5))
        );

        cfg.r10.get_mut("0").unwrap().driver.range = Some(Distance::Feet(0.0));
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        assert!(dev.driver_error().is_some(), "shown");
        dev.name = "Bay 1".into();
        assert!(super::actor_valid(&form.actors[0]), "a rename saves");

        // Text that is not a number never saves, also on a card with no
        // range saved.
        cfg.r10.get_mut("0").unwrap().driver.range = None;
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.range_val = "seven".into();
        assert!(!super::actor_valid(&form.actors[0]));
    }

    #[test]
    fn launch_only_spin_field_is_validated() {
        assert_eq!(super::launch_only_spin_value(""), Some(100.0));
        assert_eq!(super::launch_only_spin_value(" 0 "), Some(0.0));
        assert_eq!(super::launch_only_spin_value("120"), Some(120.0));
        assert_eq!(super::launch_only_spin_value("1000"), Some(1000.0));
        for bad in ["-1", "1000.5", "5000", "abc", "NaN", "inf"] {
            assert_eq!(super::launch_only_spin_value(bad), None, "{bad}");
        }
        let mut cfg = FlighthookConfig::default();
        cfg.mevo.insert("0".into(), MevoSection::default());
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        assert_eq!(dev.launch_only_spin, "100");
        dev.launch_only_spin = "-5".into();
        assert!(!super::actor_valid(&form.actors[0]));
    }

    #[test]
    fn spin_source_survives_a_settings_save() {
        for src in super::SPIN_SOURCES {
            let mut cfg = FlighthookConfig::default();
            let mut section = MevoSection::default();
            section.driver.spin_source = src;
            cfg.mevo.insert("0".into(), section);
            let mut form = SettingsForm::default();
            form.load_from(&cfg);
            assert_eq!(form.to_request().mevo["0"].driver.spin_source, src);

            let actor = form.actors[0].clone();
            let mut orig = FlighthookConfig::default();
            super::apply_actor_to_config(&mut orig, &actor);
            assert_eq!(orig.mevo["0"].driver.spin_source, src);
        }
    }

    /// Where the form holds its FlightScope card.
    fn mevo_at(form: &SettingsForm) -> usize {
        form.actors
            .iter()
            .position(|a| matches!(a, ActorFormEntry::Device(d) if d.monitor_type == "mevo"))
            .expect("a FlightScope card")
    }

    /// The FlightScope card saves a section it didn't change exactly as it
    /// was loaded (settings left unset stay unset), and a changed field as
    /// typed; a new card states the values it shows, as before.
    #[test]
    fn mevo_settings_are_saved_as_loaded_unless_changed() {
        for json in [
            r#"{"name":"Mevo WiFi","address":"192.168.2.1:5100","ball_type":1,"tee_height":"1.5in","range":"8ft","surface_height":"0in","track_pct":80.0,"use_estimated":false,"camera_mode":"standard","environment":"limited_flight"}"#,
            r#"{"name":"Xi","address":"10.0.0.7:5100","spin_source":"e8","range":"9ft"}"#,
            r#"{"name":"Garage","enabled":false}"#,
        ] {
            let section: MevoSection = serde_json::from_str(json).unwrap();
            let mut cfg = FlighthookConfig::default();
            cfg.mevo.insert("0".into(), section.clone());
            let mut form = SettingsForm::default();
            form.load_from(&cfg);
            let ActorFormEntry::Device(dev) = &form.actors[mevo_at(&form)] else {
                panic!("not a device");
            };
            assert_eq!(dev.driver_error(), None, "{json}");
            assert!(!dev.driver_blocks_save(), "{json}");
            assert_eq!(
                form.build_actor_request("mevo.0").mevo["0"],
                section,
                "{json}"
            );
        }

        let mut cfg = FlighthookConfig::default();
        let section: MevoSection =
            serde_json::from_str(r#"{"name":"Xi","address":"10.0.0.7:5100"}"#).unwrap();
        cfg.mevo.insert("0".into(), section);
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let at = mevo_at(&form);
        let ActorFormEntry::Device(dev) = &mut form.actors[at] else {
            panic!("not a device");
        };
        dev.range_val = "2.5".into();
        dev.range_unit = "meters".into();
        dev.track_pct = "70".into();
        dev.environment = crate::types::FlightScopeEnvironment::Outdoor;
        dev.address = String::new();
        let saved = form.build_actor_request("mevo.0").mevo["0"].driver.clone();
        assert_eq!(saved.range, Some(Distance::Meters(2.5)));
        assert_eq!(saved.track_pct, Some(70.0));
        assert_eq!(
            saved.environment,
            Some(crate::types::FlightScopeEnvironment::Outdoor)
        );
        assert_eq!(saved.address, None, "blank: the default address");
        assert_eq!(saved.tee_height, None, "still unset");
        assert_eq!(saved.camera_mode, None, "still unset");

        let mut form = SettingsForm::default();
        let id = form
            .add_preset(
                &super::super::cards::add_options(super::super::cards::CardGroup::LaunchMonitors)
                    [0],
            )
            .expect("added");
        assert!(id.starts_with("mevo."), "{id}");
        let saved = form.build_actor_request(&id).mevo[id.trim_start_matches("mevo.")].clone();
        assert_eq!(
            serde_json::to_value(&saved.driver).unwrap(),
            serde_json::json!({
                "address": "192.168.2.1:5100", "ball_type": 1, "tee_height": "1.5in",
                "range": "8ft", "surface_height": "0in", "track_pct": 80.0,
                "use_estimated": false, "camera_mode": "standard",
                "environment": "limited_flight"
            })
        );
    }

    /// Every FlightScope setting the driver can refuse is on the card and
    /// checked by the driver's own validation (§7.3 #15): track % outside
    /// 0 to 100, a negative distance, an address that is not an IP address
    /// and port; a number field that is not a number never saves. A stored
    /// refused value still lets the card save its other fields.
    #[test]
    fn the_mevo_settings_are_validated_by_its_driver() {
        let mut cfg = FlighthookConfig::default();
        cfg.mevo.insert("0".into(), MevoSection::default());
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        type Case = (fn(&mut super::DeviceFormEntry), &'static str);
        let cases: [Case; 6] = [
            (
                |d| d.track_pct = "150".into(),
                "track_pct: must be 0 to 100",
            ),
            (|d| d.track_pct = "lots".into(), "track_pct: not a number"),
            (
                |d| d.tee_height_val = "-1".into(),
                "tee_height: must be a distance of 0 or more",
            ),
            (|d| d.range_val = "8x".into(), "range: not a number"),
            (
                |d| d.surface_height_val = "-0.5".into(),
                "surface_height: must be a distance of 0 or more",
            ),
            (
                |d| d.address = "mevo.local".into(),
                "address: must be an IP address",
            ),
        ];
        for (set, want) in cases {
            let mut form = form.clone();
            let at = mevo_at(&form);
            let ActorFormEntry::Device(dev) = &mut form.actors[at] else {
                panic!("not a device");
            };
            set(dev);
            let error = dev.driver_error().expect("refused");
            assert!(error.starts_with(want), "{error}");
            assert!(!super::actor_valid(&form.actors[mevo_at(&form)]), "{want}");
        }

        let mut stored = cfg.clone();
        stored.mevo.get_mut("0").unwrap().driver.track_pct = Some(150.0);
        let mut form = SettingsForm::default();
        form.load_from(&stored);
        let at = mevo_at(&form);
        let ActorFormEntry::Device(dev) = &mut form.actors[at] else {
            panic!("not a device");
        };
        assert!(dev.driver_error().is_some(), "shown");
        dev.name = "Bay 1".into();
        assert!(
            super::actor_valid(&form.actors[mevo_at(&form)]),
            "a rename saves"
        );
    }

    #[test]
    fn ble_address_accepts_mac_and_uuid() {
        assert!(is_ble_address("02:00:00:AB:CD:01"));
        assert!(is_ble_address(" 02:00:00:ab:cd:01 "));
        assert!(is_ble_address("5F2A9C1E-3B7D-4E8A-9C0F-1A2B3C4D5E6F"));
    }

    #[test]
    fn ble_address_accepts_advertised_name() {
        assert!(is_ble_address("SquareGolf(CD01)"));
        assert!(is_ble_address(" squaregolf(cd01) "));
    }

    #[test]
    fn ble_address_rejects_malformed() {
        assert!(!is_ble_address(""));
        assert!(!is_ble_address("02:00:00:AB:CD"));
        assert!(!is_ble_address("02:00:00:AB:CD:G1"));
        assert!(!is_ble_address("5F2A9C1E-3B7D-4E8A-9C0F"));
        assert!(!is_ble_address("192.168.2.1:5100"));
        assert!(!is_ble_address("SquareGolf"));
        assert!(!is_ble_address("SGO300A"));
    }

    #[test]
    fn ble_address_rejects_zero_mac() {
        assert!(!is_ble_address("00:00:00:00:00:00"));
        assert!(!is_ble_address(" 00:00:00:00:00:00 "));
    }

    #[test]
    fn shot_grouping_survives_a_settings_save() {
        use super::SaveTarget;
        use crate::types::ShotGrouping;

        for (mode, _) in crate::groups::GROUPING_MODES {
            let cfg = FlighthookConfig {
                shot_grouping: mode,
                ..FlighthookConfig::default()
            };
            let mut form = SettingsForm::default();
            form.load_from(&cfg);
            assert_eq!(form.shot_grouping, mode);
            assert_eq!(form.to_request().shot_grouping, mode);
            assert_eq!(form.build_global_request().shot_grouping, mode);
        }

        // A change in the form is saved, and the loaded config follows it.
        let mut form = SettingsForm::default();
        form.load_from(&FlighthookConfig::default());
        form.shot_grouping = ShotGrouping::ByClub;
        form.global_dirty = true;
        assert_eq!(
            form.build_global_request().shot_grouping,
            ShotGrouping::ByClub
        );
        let _ = form.begin_save(SaveTarget::Global);
        assert_eq!(form.finish_save(Ok(())), None);
        assert!(!form.global_dirty);
        assert_eq!(
            form.original_config.as_ref().unwrap().shot_grouping,
            ShotGrouping::ByClub
        );

        // A mode set elsewhere reaches a clean form, and never an edited one.
        form.sync_shot_grouping(ShotGrouping::BySet);
        assert_eq!(form.shot_grouping, ShotGrouping::BySet);
        assert_eq!(form.to_request().shot_grouping, ShotGrouping::BySet);
        form.shot_grouping = ShotGrouping::None;
        form.global_dirty = true;
        form.sync_shot_grouping(ShotGrouping::ByClub);
        assert_eq!(form.shot_grouping, ShotGrouping::None);
        assert_eq!(
            form.original_config.as_ref().unwrap().shot_grouping,
            ShotGrouping::ByClub
        );
    }

    #[test]
    fn chipping_and_putting_pickers_offer_only_wedges_and_putter() {
        assert_eq!(
            flighthook::chipping_club_options(),
            &[
                Club::PitchingWedge,
                Club::GapWedge,
                Club::SandWedge,
                Club::LobWedge
            ]
        );
        assert_eq!(flighthook::putting_club_options(), &[Club::Putter]);
    }

    #[test]
    fn chipping_and_putting_clubs_round_trip_through_the_form() {
        let cfg = FlighthookConfig {
            chipping_clubs: vec![Club::PitchingWedge, Club::GapWedge],
            putting_clubs: vec![Club::Putter],
            ..FlighthookConfig::default()
        };
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        assert_eq!(
            form.chipping_clubs,
            vec![Club::PitchingWedge, Club::GapWedge]
        );
        assert_eq!(form.putting_clubs, vec![Club::Putter]);

        let req = form.to_request();
        assert_eq!(
            req.chipping_clubs,
            vec![Club::PitchingWedge, Club::GapWedge]
        );
        assert_eq!(req.putting_clubs, vec![Club::Putter]);

        let req = form.build_global_request();
        assert_eq!(
            req.chipping_clubs,
            vec![Club::PitchingWedge, Club::GapWedge]
        );
        assert_eq!(req.putting_clubs, vec![Club::Putter]);
    }

    #[test]
    fn a_non_offered_club_is_dropped_on_save_from_the_form() {
        use super::SaveTarget;

        // A hand-edited config can carry a club the picker never offers.
        let cfg = FlighthookConfig {
            chipping_clubs: vec![Club::GapWedge, Club::Iron9],
            putting_clubs: vec![Club::Putter],
            ..FlighthookConfig::default()
        };
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        // Still present in the loaded form (not yet saved)...
        assert_eq!(form.chipping_clubs, vec![Club::GapWedge, Club::Iron9]);

        // ...but dropped the moment the form builds a save request.
        assert_eq!(form.to_request().chipping_clubs, vec![Club::GapWedge]);
        assert_eq!(
            form.build_global_request().chipping_clubs,
            vec![Club::GapWedge]
        );

        form.global_dirty = true;
        let _ = form.begin_save(SaveTarget::Global);
        assert_eq!(form.finish_save(Ok(())), None);
        assert_eq!(form.chipping_clubs, vec![Club::GapWedge]);
        assert_eq!(
            form.original_config.as_ref().unwrap().chipping_clubs,
            vec![Club::GapWedge]
        );
    }
}
