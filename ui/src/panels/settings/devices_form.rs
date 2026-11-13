//! Devices section form state: `DeviceFormEntry` (a launch monitor or camera card's fields as
//! edited), its defaults, parsing and validation helpers, and `CameraFields`.

use super::*;

pub(super) const DISTANCE_UNITS: &[(&str, &str)] = &[
    ("inches", "in"),
    ("feet", "ft"),
    ("yards", "yd"),
    ("meters", "m"),
    ("centimeters", "cm"),
];

/// Format a distance value: strip trailing zeros but keep at least one decimal.
pub(super) fn format_distance_value(v: f64) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}

/// The FlightScope driver's values for settings left unset, as its card
/// shows them.
pub(super) const MEVO_ADDRESS: &str = flighthook::MEVO_DEFAULT_ADDRESS;
pub(super) const MEVO_BALL_TYPE: u8 = flighthook::MEVO_DEFAULT_BALL_TYPE;
pub(super) const MEVO_TEE_HEIGHT: Distance = flighthook::MEVO_DEFAULT_TEE_HEIGHT;
pub(super) const MEVO_RANGE: Distance = flighthook::MEVO_DEFAULT_RANGE;
pub(super) const MEVO_SURFACE_HEIGHT: Distance = flighthook::MEVO_DEFAULT_SURFACE_HEIGHT;

/// The FlightScope environments, in menu order.
pub(super) const ENVIRONMENTS: [FlightScopeEnvironment; 2] = [
    FlightScopeEnvironment::LimitedFlight,
    FlightScopeEnvironment::Outdoor,
];

/// The FlightScope Xi spin sources, in menu order.
pub(super) const SPIN_SOURCES: [SpinSource; 3] = [SpinSource::D4, SpinSource::E8, SpinSource::Auto];

/// The FlightScope camera modes, in menu order.
pub(super) const CAMERA_MODES: [CameraMode; 3] = [
    CameraMode::Standard,
    CameraMode::Fusion,
    CameraMode::RawFusion,
];

pub(super) fn environment_label(env: FlightScopeEnvironment) -> &'static str {
    match env {
        FlightScopeEnvironment::LimitedFlight => "Limited flight (sim/indoor)",
        FlightScopeEnvironment::Outdoor => "Outdoor (full flight)",
    }
}

pub(super) fn spin_source_label(src: SpinSource) -> &'static str {
    match src {
        SpinSource::D4 => "D4 (matches FS Golf)",
        SpinSource::E8 => "E8",
        SpinSource::Auto => "Auto (E8 on full swings)",
    }
}

/// The Pureplay Accessory card's Shots choices, in the order the picker lists them.
pub(super) const PUREPLAY_SHOTS: [flighthook::PureplayShots; 3] = [
    flighthook::PureplayShots::Auto,
    flighthook::PureplayShots::FromPureplay,
    flighthook::PureplayShots::FromFlighthook,
];

pub(crate) fn pureplay_shots_label(shots: flighthook::PureplayShots) -> &'static str {
    match shots {
        flighthook::PureplayShots::Auto => "Auto",
        flighthook::PureplayShots::FromPureplay => "From Pureplay",
        flighthook::PureplayShots::FromFlighthook => "From flighthook",
    }
}

pub(super) fn camera_mode_label(mode: CameraMode) -> &'static str {
    match mode {
        CameraMode::Standard => "Standard (radar only)",
        CameraMode::Fusion => "Fusion — older firmware",
        CameraMode::RawFusion => "Raw Fusion — BM17.04+",
    }
}

/// The FlightScope track % as the card shows it: whole percent, the
/// driver's default when unset.
pub(super) fn track_pct_text(track_pct: Option<f64>) -> String {
    format!(
        "{:.0}",
        track_pct.unwrap_or(flighthook::MEVO_DEFAULT_TRACK_PCT)
    )
}

/// A distance setting from the form: `loaded` while the field still shows
/// it (or `default` for an unset one), else the value typed in `unit`;
/// blank or not a number is unset.
pub(super) fn form_distance(
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

pub(super) fn unit_suffix(key: &str) -> &str {
    DISTANCE_UNITS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, suffix)| *suffix)
        .unwrap_or("in")
}

/// Find the lowest unused integer key (starting at "0") in a set of existing keys.
pub(super) fn next_index(existing: &[&str]) -> String {
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
    /// Pureplay Accessory card: which of Pureplay's shots it takes.
    pub(crate) pureplay_shots: flighthook::PureplayShots,
    /// R10: the aim offset in degrees (`aim_offset_deg`), as typed. Blank
    /// means unset.
    pub(crate) aim_offset: String,
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
pub(super) const SQUARE_CALIBRATED_CLUBS: &[Club] = &[
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
pub(super) fn impact_mm_form(
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
pub(super) fn is_valid_impact_mm(s: &str) -> bool {
    let s = s.trim();
    s.is_empty() || s.parse::<f64>().is_ok_and(|v| v.is_finite() && v >= 0.0)
}

/// The `launch_only_spin` a form field holds: blank is the default, else a
/// number from 0 to 1000. `None` when it is neither.
pub(super) fn launch_only_spin_value(s: &str) -> Option<f64> {
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
pub(super) fn impact_lead_value(s: &str) -> Option<Option<f64>> {
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
pub(super) fn latency_value(s: &str) -> Option<Option<f64>> {
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
pub(super) fn angle_value(s: &str) -> Option<Option<f64>> {
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
pub(super) fn impact_lead_text(v: Option<f64>) -> String {
    v.map(format_distance_value).unwrap_or_default()
}

/// Build the `dot_bottom_to_face_center_mm` config map from the form: non-blank
/// entries only, keyed by club code. Returns `None` when no overrides are set.
pub(super) fn impact_mm_to_config(
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
            pureplay_shots: flighthook::PureplayShots::Auto,
            aim_offset: String::new(),
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
            pureplay_shots: flighthook::PureplayShots::Auto,
            // Blank means "unset": no correction.
            aim_offset: s
                .driver
                .aim_offset_deg
                .map(format_distance_value)
                .unwrap_or_default(),
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
            pureplay_shots: flighthook::PureplayShots::Auto,
            aim_offset: String::new(),
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
            pureplay_shots: flighthook::PureplayShots::Auto,
            aim_offset: String::new(),
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
            pureplay_shots: flighthook::PureplayShots::Auto,
            aim_offset: String::new(),
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

    pub(crate) fn from_pureplay_accessory(
        id: &str,
        s: &flighthook::PureplayAccessorySection,
    ) -> Self {
        Self {
            name: s.name.clone(),
            enabled: s.enabled,
            address: s.address.clone().unwrap_or_default(),
            pureplay_shots: s.shots,
            impact_lead: impact_lead_text(s.impact_lead_s),
            ..Self::blank(id, "pureplay_accessory")
        }
    }

    pub(crate) fn from_mock(id: &str, s: &MockMonitorSection) -> Self {
        Self {
            name: s.name.clone(),
            enabled: s.enabled,
            impact_lead: impact_lead_text(s.impact_lead_s),
            aim_offset: String::new(),
            mock: s.driver.clone(),
            saved_driver: serde_json::to_value(&s.driver).ok(),
            ..Self::blank(id, "mock_monitor")
        }
    }

    /// A card of type `monitor_type` at index `id` with no name and every
    /// field at its default, for the constructors to fill in.
    pub(super) fn blank(id: &str, monitor_type: &str) -> Self {
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
            pureplay_shots: flighthook::PureplayShots::Auto,
            aim_offset: String::new(),
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
    pub(super) fn driver_json(&self) -> Option<serde_json::Value> {
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
    /// `"<field>: <why>"`: the R10's range and aim offset, the FlightScope's tee
    /// height, range, surface height and track %, every Trackball number.
    pub(super) fn unparsable(&self) -> Option<String> {
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
    /// text that is not a number (blank is unset): the R10's range and aim
    /// offset, the FlightScope's tee height, range, surface height and
    /// track %.
    pub(super) fn unparsable_field(&self) -> Option<&'static str> {
        let fields: &[(&'static str, &str)] = match self.monitor_type.as_str() {
            "r10" => &[
                ("range", &self.range_val),
                ("aim_offset_deg", &self.aim_offset),
            ],
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

    /// The R10 driver's settings from the form: the range and the aim offset
    /// as typed, blank (or not a number, which
    /// [`driver_error`](Self::driver_error) refuses) for none.
    pub(crate) fn r10_config(&self) -> flighthook::R10Config {
        flighthook::R10Config {
            range: self
                .range_val
                .trim()
                .parse::<f64>()
                .ok()
                .map(|v| Distance::from_value_and_unit(v, &self.range_unit)),
            aim_offset_deg: self.aim_offset.trim().parse::<f64>().ok(),
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
    pub(super) fn trackball_config(&self) -> flighthook::TrackballConfig {
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

    /// Whether this is a Pureplay Accessory card, which takes Pureplay's `ip:port`.
    /// Blank means the default, [`PUREPLAY_ACCESSORY_ADDRESS`].
    pub(crate) fn has_pureplay_address(&self) -> bool {
        self.monitor_type == "pureplay_accessory"
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
