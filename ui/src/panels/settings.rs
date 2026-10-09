use crate::app::FlighthookApp;
use crate::net;
use crate::types::{
    CameraMode, Club, Distance, DistanceExt, FlighthookConfig, GsProSection, MevoSection,
    MockMonitorSection, R10Section, RandomClubSection, UnitSystem, WebserverSection,
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
    /// Square Golf: discard zero-spin reads, except when putting.
    pub(crate) discard_zero_spin: bool,
    /// Square Golf fields the form does not surface. Carried verbatim so that
    /// saving settings does not wipe them from the config file.
    pub(crate) square_club: Option<String>,
    pub(crate) square_advanced_spin: Option<bool>,
    /// Square Golf: per-club face impact calibration override, in mm from
    /// the bottom of the sticker dot down to face centre. Keyed by club; blank means "use
    /// allsquare's default" for that club. The putter is excluded — it has
    /// no vertical impact estimate.
    pub(crate) square_impact_mm: std::collections::HashMap<Club, String>,
    pub(crate) dirty: bool,
}

/// Whether a string looks like a BLE device identifier: a MAC address
/// (`AA:BB:CC:DD:EE:FF`), on macOS, which hides MAC addresses, a peripheral
/// UUID (`8-4-4-4-12` hex groups), or an advertised Square Golf name such as
/// `SquareGolf(54E4)`, which is the same on every OS. The all-zero MAC is
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

/// Build the face-impact-calibration form map from config overrides. A club
/// with no entry renders blank, meaning "use allsquare's default".
fn impact_mm_form(
    overrides: Option<&std::collections::BTreeMap<String, f64>>,
) -> std::collections::HashMap<Club, String> {
    let mut map = std::collections::HashMap::new();
    if let Some(overrides) = overrides {
        for (key, &mm) in overrides {
            if let Some(club) = Club::from_code(key)
                && club != Club::Putter
            {
                map.insert(club, format_distance_value(mm));
            }
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

/// Build the `dot_bottom_to_face_centre_mm` config map from the form: non-blank
/// entries only, keyed by club code. Returns `None` when no overrides are set.
fn impact_mm_to_config(
    form: &std::collections::HashMap<Club, String>,
) -> Option<std::collections::BTreeMap<String, f64>> {
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
    if map.is_empty() { None } else { Some(map) }
}

impl DeviceFormEntry {
    pub(crate) fn from_mevo(id: &str, s: &MevoSection) -> Self {
        let tee = s.tee_height.unwrap_or(Distance::Inches(1.5));
        let rng = s.range.unwrap_or(Distance::Feet(8.0));
        let surf = s.surface_height.unwrap_or(Distance::Inches(0.0));
        Self {
            id: id.into(),
            monitor_type: "mevo".into(),
            name: s.name.clone(),
            address: s.address.clone().unwrap_or_default(),
            ball_type: s.ball_type.unwrap_or(1),
            tee_height_val: format_distance_value(tee.value()),
            tee_height_unit: tee.unit_key().into(),
            range_val: format_distance_value(rng.value()),
            range_unit: rng.unit_key().into(),
            surface_height_val: format_distance_value(surf.value()),
            surface_height_unit: surf.unit_key().into(),
            track_pct: format!("{:.0}", s.track_pct.unwrap_or(80.0)),
            use_estimated: s.use_estimated.unwrap_or(true),
            camera_mode: s.camera_mode.unwrap_or_default(),
            discard_zero_spin: true,
            square_club: None,
            square_advanced_spin: None,
            square_impact_mm: std::collections::HashMap::new(),
            dirty: false,
        }
    }

    pub(crate) fn from_r10(id: &str, s: &R10Section) -> Self {
        Self {
            id: id.into(),
            monitor_type: "r10".into(),
            name: s.name.clone(),
            address: String::new(),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            // Blank means "unset" — the device keeps its own tee distance.
            range_val: s
                .range
                .map(|d| format_distance_value(d.value()))
                .unwrap_or_default(),
            range_unit: s.range.map_or("feet".into(), |d| d.unit_key().to_string()),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: true,
            camera_mode: CameraMode::default(),
            discard_zero_spin: true,
            square_club: None,
            square_advanced_spin: None,
            square_impact_mm: std::collections::HashMap::new(),
            dirty: false,
        }
    }

    pub(crate) fn from_square(id: &str, s: &flighthook::SquareSection) -> Self {
        Self {
            id: id.into(),
            monitor_type: "square".into(),
            name: s.name.clone(),
            address: s.address.clone().unwrap_or_default(),
            discard_zero_spin: s.discard_non_putting_zero_spin.unwrap_or(true),
            square_club: s.club.clone(),
            square_advanced_spin: s.advanced_spin,
            square_impact_mm: impact_mm_form(s.dot_bottom_to_face_centre_mm.as_ref()),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            range_val: "8".into(),
            range_unit: "feet".into(),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: true,
            camera_mode: CameraMode::default(),
            dirty: false,
        }
    }

    pub(crate) fn from_openconnect(id: &str, s: &flighthook::OpenConnectServerSection) -> Self {
        Self {
            id: id.into(),
            monitor_type: "openconnect_server".into(),
            name: s.name.clone(),
            address: s.bind.clone().unwrap_or_default(),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            range_val: "8".into(),
            range_unit: "feet".into(),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: true,
            camera_mode: CameraMode::default(),
            discard_zero_spin: true,
            square_club: None,
            square_advanced_spin: None,
            square_impact_mm: std::collections::HashMap::new(),
            dirty: false,
        }
    }

    pub(crate) fn from_mock(id: &str, s: &MockMonitorSection) -> Self {
        Self {
            id: id.into(),
            monitor_type: "mock_monitor".into(),
            name: s.name.clone(),
            address: String::new(),
            ball_type: 0,
            tee_height_val: "1.5".into(),
            tee_height_unit: "inches".into(),
            range_val: "8".into(),
            range_unit: "feet".into(),
            surface_height_val: "0".into(),
            surface_height_unit: "inches".into(),
            track_pct: "80".into(),
            use_estimated: true,
            camera_mode: CameraMode::default(),
            discard_zero_spin: true,
            square_club: None,
            square_advanced_spin: None,
            square_impact_mm: std::collections::HashMap::new(),
            dirty: false,
        }
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

    /// Whether this device takes a TCP `ip:port` address (validated as such).
    pub(crate) fn has_network_address(&self) -> bool {
        self.is_mevo()
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

/// Per-integration form entry. The `id` is the map index,
/// `integration_type` encodes which section map it belongs to.
#[derive(Clone)]
pub(crate) struct IntegrationFormEntry {
    pub(crate) id: String,
    pub(crate) integration_type: String, // "gspro" | "random_club"
    pub(crate) name: String,
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
                "openconnect_server" => "OpenConnect Server",
                "mock_monitor" => "Mock",
                _ => &d.monitor_type,
            },
            ActorFormEntry::Integration(i) => match i.integration_type.as_str() {
                "gspro" => "GSPro",
                "random_club" => "Random Club",
                "webserver" => "Web",
                _ => &i.integration_type,
            },
        }
    }

    pub(crate) fn type_tooltip(&self) -> &str {
        match self {
            ActorFormEntry::Device(d) => match d.monitor_type.as_str() {
                "mevo" => "FlightScope Mevo / Mevo+ — connects via WiFi (TCP)",
                "r10" => "Garmin R10 — auto-detects from system connected Bluetooth devices",
                "square" => {
                    "Square Golf Omni — BLE, no pairing required; leave address blank to auto-discover"
                }
                "openconnect_server" => {
                    "Accepts inbound shots from any launch monitor that speaks GSPro Open Connect (Uneekor, Foresight, SkyTrak). Bind address, default 0.0.0.0:921 — to share a host with GSPro, move GSPConnect to 922 via OpenAPIUseAltPort"
                }
                "mock_monitor" => "Mock launch monitor — generates random shots for testing",
                _ => "",
            },
            ActorFormEntry::Integration(i) => match i.integration_type.as_str() {
                "gspro" => "GSPro simulator — connects via TCP (Open Connect API)",
                "random_club" => "Cycles through clubs automatically on each shot",
                "webserver" => "HTTP/WebSocket server for the dashboard UI and API",
                _ => "",
            },
        }
    }

    /// Sort key: (name, type_label, id) for stable ordering.
    fn sort_key(&self) -> (&str, &str, &str) {
        match self {
            ActorFormEntry::Device(d) => (&d.name, self.type_label(), &d.id),
            ActorFormEntry::Integration(i) => (&i.name, self.type_label(), &i.id),
        }
    }
}

/// Pending removal confirmation: (index in actors Vec, display name).
pub(crate) struct PendingRemoval(pub(crate) usize, pub(crate) String);

/// Which section is being saved.
#[derive(Clone, Debug)]
pub(crate) enum SaveTarget {
    /// Global settings (default units).
    Global,
    /// Actor at the given index in the actors Vec.
    Actor(usize),
    /// Full config (e.g. after removal).
    Full,
}

/// Settings form state.
#[derive(Clone)]
pub(crate) struct SettingsForm {
    pub(crate) default_units: UnitSystem,
    pub(crate) chipping_clubs: Vec<Club>,
    pub(crate) putting_clubs: Vec<Club>,
    pub(crate) global_dirty: bool,
    pub(crate) actors: Vec<ActorFormEntry>,
    pub(crate) loaded: bool,
    pub(crate) dirty: bool,
    pub(crate) saving: bool,
    pub(crate) save_target: Option<SaveTarget>,
    /// Snapshot of the config at last load/save — used to build scoped requests.
    original_config: Option<FlighthookConfig>,
}

impl Default for SettingsForm {
    fn default() -> Self {
        Self {
            default_units: UnitSystem::default(),
            chipping_clubs: flighthook::default_chipping_clubs(),
            putting_clubs: flighthook::default_putting_clubs(),
            global_dirty: false,
            actors: Vec::new(),
            loaded: false,
            dirty: false,
            saving: false,
            save_target: None,
            original_config: None,
        }
    }
}

impl SettingsForm {
    pub(crate) fn load_from(&mut self, s: &FlighthookConfig) {
        self.original_config = Some(s.clone());
        self.default_units = s.default_units;
        self.chipping_clubs = s.chipping_clubs.clone();
        self.putting_clubs = s.putting_clubs.clone();
        self.global_dirty = false;

        self.actors.clear();
        for (id, section) in &s.mevo {
            self.actors
                .push(ActorFormEntry::Device(DeviceFormEntry::from_mevo(
                    id, section,
                )));
        }
        for (id, section) in &s.square {
            self.actors
                .push(ActorFormEntry::Device(DeviceFormEntry::from_square(
                    id, section,
                )));
        }
        for (id, section) in &s.r10 {
            self.actors
                .push(ActorFormEntry::Device(DeviceFormEntry::from_r10(
                    id, section,
                )));
        }
        for (id, section) in &s.openconnect_server {
            self.actors
                .push(ActorFormEntry::Device(DeviceFormEntry::from_openconnect(
                    id, section,
                )));
        }
        for (id, section) in &s.mock_monitor {
            self.actors
                .push(ActorFormEntry::Device(DeviceFormEntry::from_mock(
                    id, section,
                )));
        }
        for (id, section) in &s.gspro {
            self.actors
                .push(ActorFormEntry::Integration(IntegrationFormEntry {
                    id: id.clone(),
                    integration_type: "gspro".into(),
                    name: section.name.clone(),
                    address: section.address.clone().unwrap_or_default(),
                    full_monitor: section.full_monitor.clone().unwrap_or_default(),
                    chipping_monitor: section.chipping_monitor.clone().unwrap_or_default(),
                    putting_monitor: section.putting_monitor.clone().unwrap_or_default(),
                    dirty: false,
                }));
        }
        for (id, section) in &s.random_club {
            self.actors
                .push(ActorFormEntry::Integration(IntegrationFormEntry {
                    id: id.clone(),
                    integration_type: "random_club".into(),
                    name: section.name.clone(),
                    address: String::new(),
                    full_monitor: String::new(),
                    chipping_monitor: String::new(),
                    putting_monitor: String::new(),
                    dirty: false,
                }));
            let _ = section;
        }
        for (id, section) in &s.webserver {
            self.actors
                .push(ActorFormEntry::Integration(IntegrationFormEntry {
                    id: id.clone(),
                    integration_type: "webserver".into(),
                    name: section.name.clone(),
                    address: section.bind.clone(),
                    full_monitor: String::new(),
                    chipping_monitor: String::new(),
                    putting_monitor: String::new(),
                    dirty: false,
                }));
        }
        self.actors.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));

        self.loaded = true;
        self.dirty = false;
    }

    pub(crate) fn is_valid(&self) -> bool {
        for actor in &self.actors {
            match actor {
                ActorFormEntry::Device(dev) => {
                    if dev.name.is_empty() {
                        return false;
                    }
                    if dev.has_network_address()
                        && dev.address.parse::<std::net::SocketAddr>().is_err()
                    {
                        return false;
                    }
                    // A BLE address is optional — blank means auto-discover.
                    if dev.has_ble_address() {
                        let a = dev.address.trim();
                        if !a.is_empty() && !is_ble_address(a) {
                            return false;
                        }
                    }
                    // Face impact calibration: blank means "use the default",
                    // anything else must be a finite, non-negative mm value.
                    if dev.is_square()
                        && dev
                            .square_impact_mm
                            .values()
                            .any(|v| !is_valid_impact_mm(v))
                    {
                        return false;
                    }
                }
                ActorFormEntry::Integration(entry) => {
                    if entry.name.is_empty() {
                        return false;
                    }
                    if entry.integration_type != "random_club"
                        && entry.address.parse::<std::net::SocketAddr>().is_err()
                    {
                        return false;
                    }
                }
            }
        }
        true
    }

    pub(crate) fn to_request(&self) -> FlighthookConfig {
        let mut webserver = std::collections::HashMap::new();
        let mut mevo = std::collections::HashMap::new();
        let mut r10 = std::collections::HashMap::new();
        let mut square = std::collections::HashMap::new();
        let mut mock_monitor = std::collections::HashMap::new();
        let mut openconnect_server = std::collections::HashMap::new();
        let mut gspro = std::collections::HashMap::new();
        let mut random_club = std::collections::HashMap::new();

        for actor in &self.actors {
            match actor {
                ActorFormEntry::Device(dev) => match dev.monitor_type.as_str() {
                    "mevo" => {
                        mevo.insert(
                            dev.id.clone(),
                            MevoSection {
                                name: dev.name.clone(),
                                address: if dev.address.is_empty() {
                                    None
                                } else {
                                    Some(dev.address.clone())
                                },
                                ball_type: Some(dev.ball_type),
                                tee_height: dev.tee_height_val.parse::<f64>().ok().map(|v| {
                                    Distance::from_value_and_unit(v, &dev.tee_height_unit)
                                }),
                                range: dev
                                    .range_val
                                    .parse::<f64>()
                                    .ok()
                                    .map(|v| Distance::from_value_and_unit(v, &dev.range_unit)),
                                surface_height: dev.surface_height_val.parse::<f64>().ok().map(
                                    |v| Distance::from_value_and_unit(v, &dev.surface_height_unit),
                                ),
                                track_pct: dev.track_pct.parse().ok(),
                                use_estimated: Some(dev.use_estimated),
                                camera_mode: Some(dev.camera_mode),
                            },
                        );
                    }
                    "square" => {
                        square.insert(
                            dev.id.clone(),
                            flighthook::SquareSection {
                                name: dev.name.clone(),
                                address: if dev.address.trim().is_empty() {
                                    None
                                } else {
                                    Some(dev.address.trim().to_string())
                                },
                                // Carried through the form so a save does
                                // not wipe values the UI does not surface.
                                club: dev.square_club.clone(),
                                advanced_spin: dev.square_advanced_spin,
                                discard_non_putting_zero_spin: Some(dev.discard_zero_spin),
                                dot_bottom_to_face_centre_mm: impact_mm_to_config(
                                    &dev.square_impact_mm,
                                ),
                            },
                        );
                    }
                    "r10" => {
                        r10.insert(
                            dev.id.clone(),
                            R10Section {
                                name: dev.name.clone(),
                                range: dev
                                    .range_val
                                    .parse::<f64>()
                                    .ok()
                                    .map(|v| Distance::from_value_and_unit(v, &dev.range_unit)),
                            },
                        );
                    }
                    "openconnect_server" => {
                        openconnect_server.insert(
                            dev.id.clone(),
                            flighthook::OpenConnectServerSection {
                                name: dev.name.clone(),
                                bind: if dev.address.is_empty() {
                                    None
                                } else {
                                    Some(dev.address.clone())
                                },
                            },
                        );
                    }
                    "mock_monitor" => {
                        mock_monitor.insert(
                            dev.id.clone(),
                            MockMonitorSection {
                                name: dev.name.clone(),
                            },
                        );
                    }
                    _ => {}
                },
                ActorFormEntry::Integration(entry) => match entry.integration_type.as_str() {
                    "gspro" => {
                        gspro.insert(
                            entry.id.clone(),
                            GsProSection {
                                name: entry.name.clone(),
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
                    "random_club" => {
                        random_club.insert(
                            entry.id.clone(),
                            RandomClubSection {
                                name: entry.name.clone(),
                            },
                        );
                    }
                    "webserver" => {
                        webserver.insert(
                            entry.id.clone(),
                            WebserverSection {
                                name: entry.name.clone(),
                                bind: entry.address.clone(),
                            },
                        );
                    }
                    _ => {}
                },
            }
        }

        FlighthookConfig {
            default_units: self.default_units,
            chipping_clubs: self.chipping_clubs.clone(),
            putting_clubs: self.putting_clubs.clone(),
            webserver,
            mevo,
            r10,
            square,
            mock_monitor,
            openconnect_server,
            gspro,
            random_club,
        }
    }

    /// Build a config that applies only the global settings change on top of the
    /// original config.
    pub(crate) fn build_global_request(&self) -> FlighthookConfig {
        let mut config = self
            .original_config
            .clone()
            .unwrap_or_else(|| self.to_request());
        config.default_units = self.default_units;
        config.chipping_clubs = self.chipping_clubs.clone();
        config.putting_clubs = self.putting_clubs.clone();
        config
    }

    /// Build a config that applies only the actor at `idx` on top of the
    /// original config. Returns `(config, global_id)`.
    pub(crate) fn build_actor_request(&self, idx: usize) -> (FlighthookConfig, String) {
        let mut config = self
            .original_config
            .clone()
            .unwrap_or_else(|| self.to_request());
        let actor = &self.actors[idx];
        let global_id = apply_actor_to_config(&mut config, actor);
        (config, global_id)
    }

    /// After a successful scoped save, update the original config to reflect
    /// the saved values for the given target.
    pub(crate) fn update_original_after_save(&mut self, target: &SaveTarget) {
        match target {
            SaveTarget::Global => {
                if let Some(ref mut orig) = self.original_config {
                    orig.default_units = self.default_units;
                    orig.chipping_clubs = self.chipping_clubs.clone();
                    orig.putting_clubs = self.putting_clubs.clone();
                }
            }
            SaveTarget::Actor(idx) => {
                // Clone the actor to avoid borrow conflict
                let actor = match self.actors.get(*idx) {
                    Some(a) => a.clone(),
                    None => return,
                };
                if let Some(ref mut orig) = self.original_config {
                    apply_actor_to_config(orig, &actor);
                }
            }
            SaveTarget::Full => {
                // Full save — replace original with current form state
                let new_config = self.to_request();
                self.original_config = Some(new_config);
            }
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
                            address: if dev.address.is_empty() {
                                None
                            } else {
                                Some(dev.address.clone())
                            },
                            ball_type: Some(dev.ball_type),
                            tee_height: dev
                                .tee_height_val
                                .parse::<f64>()
                                .ok()
                                .map(|v| Distance::from_value_and_unit(v, &dev.tee_height_unit)),
                            range: dev
                                .range_val
                                .parse::<f64>()
                                .ok()
                                .map(|v| Distance::from_value_and_unit(v, &dev.range_unit)),
                            surface_height: dev.surface_height_val.parse::<f64>().ok().map(|v| {
                                Distance::from_value_and_unit(v, &dev.surface_height_unit)
                            }),
                            track_pct: dev.track_pct.parse().ok(),
                            use_estimated: Some(dev.use_estimated),
                            camera_mode: Some(dev.camera_mode),
                        },
                    );
                }
                "square" => {
                    config.square.insert(
                        dev.id.clone(),
                        flighthook::SquareSection {
                            name: dev.name.clone(),
                            address: if dev.address.trim().is_empty() {
                                None
                            } else {
                                Some(dev.address.trim().to_string())
                            },
                            club: dev.square_club.clone(),
                            advanced_spin: dev.square_advanced_spin,
                            discard_non_putting_zero_spin: Some(dev.discard_zero_spin),
                            dot_bottom_to_face_centre_mm: impact_mm_to_config(
                                &dev.square_impact_mm,
                            ),
                        },
                    );
                }
                "r10" => {
                    config.r10.insert(
                        dev.id.clone(),
                        R10Section {
                            name: dev.name.clone(),
                            range: dev
                                .range_val
                                .parse::<f64>()
                                .ok()
                                .map(|v| Distance::from_value_and_unit(v, &dev.range_unit)),
                        },
                    );
                }
                "openconnect_server" => {
                    config.openconnect_server.insert(
                        dev.id.clone(),
                        flighthook::OpenConnectServerSection {
                            name: dev.name.clone(),
                            bind: if dev.address.is_empty() {
                                None
                            } else {
                                Some(dev.address.clone())
                            },
                        },
                    );
                }
                "mock_monitor" => {
                    config.mock_monitor.insert(
                        dev.id.clone(),
                        MockMonitorSection {
                            name: dev.name.clone(),
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
                "gspro" => {
                    config.gspro.insert(
                        entry.id.clone(),
                        GsProSection {
                            name: entry.name.clone(),
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
                "random_club" => {
                    config.random_club.insert(
                        entry.id.clone(),
                        RandomClubSection {
                            name: entry.name.clone(),
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

impl FlighthookApp {
    pub(crate) fn render_settings_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // Lazy-load settings on first render of this tab
        if !self.settings.loaded {
            net::fetch_settings(ctx, &self.pending);
        }

        let field_width = 200.0;

        egui::ScrollArea::both()
            .auto_shrink(false)
            .show(ui, |ui| {
                // --- GLOBAL ---
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Global")
                            .strong()
                            .color(egui::Color32::from_rgb(180, 200, 255)),
                    );
                    let save_btn = if self.settings.global_dirty {
                        egui::Button::new(
                            egui::RichText::new("Save").size(11.0).color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(200, 50, 50))
                    } else {
                        egui::Button::new(egui::RichText::new("Save").size(11.0))
                    };
                    if ui.add_enabled(self.settings.global_dirty && self.settings.is_valid() && !self.settings.saving, save_btn).clicked() {
                        self.settings.saving = true;
                        self.settings.save_target = Some(crate::panels::settings::SaveTarget::Global);
                        let req = self.settings.build_global_request();
                        net::post_settings(ctx, &self.pending, &req, None);
                    }
                    if ui.button(egui::RichText::new("API Docs").size(11.0)).clicked() {
                        self.show_api_docs = true;
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Default Units:").on_hover_text("Default unit system for shot display. Can be toggled per-session in the Shots tab.");
                    let units_label = match self.settings.default_units {
                        UnitSystem::Imperial => "Imperial",
                        UnitSystem::Metric => "Metric",
                    };
                    egui::ComboBox::from_id_salt("default_units")
                        .selected_text(units_label)
                        .width(field_width)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(self.settings.default_units == UnitSystem::Imperial, "Imperial").clicked() {
                                self.settings.default_units = UnitSystem::Imperial;
                                self.settings.global_dirty = true;
                                self.settings.dirty = true;
                                self.units_toggle = UnitSystem::Imperial;
                            }
                            if ui.selectable_label(self.settings.default_units == UnitSystem::Metric, "Metric").clicked() {
                                self.settings.default_units = UnitSystem::Metric;
                                self.settings.global_dirty = true;
                                self.settings.dirty = true;
                                self.units_toggle = UnitSystem::Metric;
                            }
                        });
                });

                // --- Club-to-mode mapping ---
                ui.add_space(6.0);
                for (label, tooltip, mode_clubs, other_clubs) in [
                    ("Chipping Clubs:", "When a game selects one of these clubs, the launch monitor will be set to chipping mode.", "chipping", "putting"),
                    ("Putting Clubs:", "When a game selects one of these clubs, the launch monitor will be set to putting mode.", "putting", "chipping"),
                ] {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(label).on_hover_text(tooltip);
                        for &club in Club::ALL {
                            let in_this = match mode_clubs {
                                "chipping" => self.settings.chipping_clubs.contains(&club),
                                _ => self.settings.putting_clubs.contains(&club),
                            };
                            let btn = egui::Button::new(
                                egui::RichText::new(format!("{club}")).size(11.0),
                            );
                            let btn = if in_this {
                                btn.fill(egui::Color32::from_rgb(60, 100, 160))
                            } else {
                                btn
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
                                self.settings.global_dirty = true;
                                self.settings.dirty = true;
                            }
                        }
                    });
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // --- ACTORS (devices + integrations) ---
                let mut save_idx = None;
                let settings_valid = self.settings.is_valid();
                let settings_saving = self.settings.saving;

                // Collect device actor IDs for routing dropdowns (before mutable iteration)
                let device_monitor_options: Vec<(String, String)> = self.settings.actors.iter()
                    .filter_map(|a| match a {
                        ActorFormEntry::Device(d) => {
                            let global_id = format!("{}.{}", d.monitor_type, d.id);
                            Some((global_id, d.name.clone()))
                        }
                        _ => None,
                    })
                    .collect();

                for (idx, actor) in self.settings.actors.iter_mut().enumerate() {
                    let type_label = actor.type_label().to_string();
                    let type_tooltip = actor.type_tooltip().to_string();
                    let dirty = actor.dirty();

                    // Header: name + type badge + Remove + Save
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(actor.name())
                                .strong()
                                .color(egui::Color32::from_rgb(180, 200, 255)),
                        ).on_hover_text(&type_tooltip);
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(60, 80, 120))
                            .corner_radius(4.0)
                            .inner_margin(egui::Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&type_label)
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(200, 220, 255)),
                                ).on_hover_text(&type_tooltip);
                            });
                        if ui
                            .button(egui::RichText::new("Remove").size(11.0))
                            .clicked()
                        {
                            self.confirm_remove = Some(PendingRemoval(idx, actor.name().to_string()));
                        }
                        let save_btn = if dirty {
                            egui::Button::new(
                                egui::RichText::new("Save").size(11.0).color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(200, 50, 50))
                        } else {
                            egui::Button::new(egui::RichText::new("Save").size(11.0))
                        };
                        if ui.add_enabled(dirty && settings_valid && !settings_saving, save_btn).clicked() {
                            save_idx = Some(idx);
                        }
                    });

                    // Type-specific fields
                    match actor {
                        ActorFormEntry::Device(dev) => {
                            // Name
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label("Name:").on_hover_text("Display name for this device in the UI and logs.");
                                if ui
                                    .add(egui::TextEdit::singleline(&mut dev.name).desired_width(field_width))
                                    .changed()
                                {
                                    dev.dirty = true;
                                }
                            });

                            if dev.has_network_address() {
                                // Address
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Address:").on_hover_text("TCP address of the launch monitor. Connect to the device WiFi AP first.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.address).desired_width(field_width))
                                        .on_hover_text("ip:port (e.g. 192.168.2.1:5100)")
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                    if dev.address.parse::<std::net::SocketAddr>().is_err() {
                                        ui.label(
                                            egui::RichText::new("Invalid address")
                                                .color(egui::Color32::from_rgb(255, 80, 80))
                                                .size(11.0),
                                        );
                                    }
                                });
                            }

                            if dev.has_ble_address() {
                                // BLE address — optional; blank auto-discovers.
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("BLE Address:").on_hover_text("Advertised name of the device, e.g. SquareGolf(54E4) — the same on every OS. A Bluetooth address (a UUID on macOS) also works. Leave blank to auto-discover.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.address).desired_width(field_width))
                                        .on_hover_text("optional, e.g. SquareGolf(54E4)")
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                    let a = dev.address.trim();
                                    if !a.is_empty() && !is_ble_address(a) {
                                        ui.label(
                                            egui::RichText::new("Invalid BLE address")
                                                .color(egui::Color32::from_rgb(255, 80, 80))
                                                .size(11.0),
                                        );
                                    }
                                });
                            }

                            if dev.is_square() {
                                // Zero-spin rejection (putts always exempt).
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    if ui
                                        .checkbox(
                                            &mut dev.discard_zero_spin,
                                            "Discard zero-spin shots",
                                        )
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
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    egui::CollapsingHeader::new("Face impact calibration (beta)")
                                        .id_salt(format!("impact_cal_{}", dev.id))
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(
                                                    "Distance from the bottom edge of the club \
                                                     sticker's dot down to face centre, in mm. \
                                                     Blank uses the built-in default for that club.",
                                                )
                                                .size(11.0)
                                                .weak(),
                                            );
                                            for &club in Club::ALL {
                                                if club == Club::Putter {
                                                    continue;
                                                }
                                                let mut val = dev
                                                    .square_impact_mm
                                                    .get(&club)
                                                    .cloned()
                                                    .unwrap_or_default();
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
                                                    ui.label(
                                                        egui::RichText::new("mm").size(11.0).weak(),
                                                    );
                                                    if invalid {
                                                        ui.label(
                                                            egui::RichText::new("Invalid")
                                                                .color(egui::Color32::from_rgb(
                                                                    255, 80, 80,
                                                                ))
                                                                .size(11.0),
                                                        );
                                                    }
                                                });
                                            }
                                        });
                                });
                            }

                            if dev.is_r10() {
                                // Tee distance, pushed to the device on wake-up.
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Monitor-to-Ball:").on_hover_text("Distance from the R10 to the ball, sent to the device as its tee distance.\nGarmin recommends 6-8 ft behind the ball.\n\nLeave blank to keep whatever distance is already set on the device.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.range_val).desired_width(field_width))
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
                                    ui.add_space(16.0);
                                    ui.label("Ball Type:").on_hover_text("RCT = Radar Capture Technology.\nStandard = any regular golf ball.");
                                    let ball_text = if dev.ball_type == 1 { "RCT" } else { "Standard" };
                                    egui::ComboBox::from_id_salt(format!("ball_type_{}", dev.id))
                                        .selected_text(ball_text)
                                        .width(field_width)
                                        .show_ui(ui, |ui| {
                                            if ui.selectable_label(dev.ball_type == 1, "RCT").clicked() {
                                                dev.ball_type = 1;
                                                dev.dirty = true;
                                            }
                                            if ui.selectable_label(dev.ball_type == 0, "Standard").clicked() {
                                                dev.ball_type = 0;
                                                dev.dirty = true;
                                            }
                                        });
                                });

                                // Tee Height
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Tee Height:").on_hover_text("Height of the tee above the hitting surface.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.tee_height_val).desired_width(field_width))
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                    egui::ComboBox::from_id_salt(format!("tee_unit_{}", dev.id))
                                        .selected_text(unit_suffix(&dev.tee_height_unit))
                                        .width(50.0)
                                        .show_ui(ui, |ui| {
                                            for &(key, label) in DISTANCE_UNITS {
                                                if ui.selectable_label(dev.tee_height_unit == key, label).clicked() {
                                                    dev.tee_height_unit = key.to_string();
                                                    dev.dirty = true;
                                                }
                                            }
                                        });
                                });

                                // Monitor-to-Ball
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Monitor-to-Ball:").on_hover_text("Distance from the front of the launch monitor to the ball.\nMevo+ recommended range: 7-9 ft.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.range_val).desired_width(field_width))
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
                                    ui.add_space(16.0);
                                    ui.label("Surface Height:").on_hover_text("Height of the hitting surface above the monitor.\nSet to 0 if the ball and monitor are on the same level.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.surface_height_val).desired_width(field_width))
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
                                    ui.add_space(16.0);
                                    ui.label("Track %:").on_hover_text("Minimum outdoor tracking percentage (0-100).\nLower values accept shorter-tracked shots. GSPro default: 100, FS Golf default: 60.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut dev.track_pct).desired_width(field_width))
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                    ui.label("%");
                                });

                                // Use Estimated
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    if ui.checkbox(&mut dev.use_estimated, "Use Estimated Shots")
                                        .on_hover_text("Include estimated (E8 fallback) shots.\nEstimated shots may lack sidespin and carry less data,\nbut are often the only result for short chips.")
                                        .changed()
                                    {
                                        dev.dirty = true;
                                    }
                                });

                                // Camera Mode
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Camera Mode:").on_hover_text(
                                        "Standard reports ball flight only.\nFusion modes add club data (path, face angle, attack angle,\ndynamic loft, smash factor, swing planes) and need the\nPro Package enabled on the device.\n\nWhich Fusion mode depends on firmware: Raw Fusion for\nBM17.04 and newer, Fusion for older. The wrong one yields\nno club data. Fusion is applied after a 15s camera warmup.",
                                    );
                                    egui::ComboBox::from_id_salt(format!("cam_mode_{}", dev.id))
                                        .selected_text(dev.camera_mode.label())
                                        .width(field_width * 2.0)
                                        .show_ui(ui, |ui| {
                                            for mode in CameraMode::all() {
                                                if ui
                                                    .selectable_label(
                                                        dev.camera_mode == mode,
                                                        mode.label(),
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
                        }
                        ActorFormEntry::Integration(entry) => {
                            // Name
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label("Name:").on_hover_text("Display name for this integration in the UI and logs.");
                                if ui
                                    .add(egui::TextEdit::singleline(&mut entry.name).desired_width(field_width))
                                    .changed()
                                {
                                    entry.dirty = true;
                                }
                            });

                            // Address field (skip for mock)
                            if entry.integration_type != "random_club" {
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label("Address:").on_hover_text("TCP address of the simulator. Shot data is forwarded here as JSON.");
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut entry.address).desired_width(field_width))
                                        .on_hover_text("ip:port (e.g. 127.0.0.1:921)")
                                        .changed()
                                    {
                                        entry.dirty = true;
                                    }
                                    if entry.address.parse::<std::net::SocketAddr>().is_err() {
                                        ui.label(
                                            egui::RichText::new("Invalid address")
                                                .color(egui::Color32::from_rgb(255, 80, 80))
                                                .size(11.0),
                                        );
                                    }
                                });
                            }

                            // Routing dropdowns (GSPro only)
                            if entry.integration_type == "gspro" {
                                for (field_label, field_val, salt) in [
                                    ("Full Monitor:", &mut entry.full_monitor, "full"),
                                    ("Chipping Monitor:", &mut entry.chipping_monitor, "chipping"),
                                    ("Putting Monitor:", &mut entry.putting_monitor, "putting"),
                                ] {
                                    ui.horizontal(|ui| {
                                        ui.add_space(16.0);
                                        ui.label(field_label).on_hover_text(
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
                                            .show_ui(ui, |ui| {
                                                if ui.selectable_label(field_val.is_empty(), "Any").clicked() {
                                                    field_val.clear();
                                                    entry.dirty = true;
                                                }
                                                for (monitor_id, monitor_name) in &device_monitor_options {
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
                    ui.add_space(4.0);
                }

                // Recalculate global dirty from per-entry flags
                self.settings.dirty = self.settings.global_dirty
                    || self.settings.actors.iter().any(|a| a.dirty());

                if let Some(idx) = save_idx {
                    self.settings.saving = true;
                    let (req, scope) = self.settings.build_actor_request(idx);
                    self.settings.save_target = Some(crate::panels::settings::SaveTarget::Actor(idx));
                    net::post_settings(ctx, &self.pending, &req, Some(&scope));
                }

                // Add dropdown (all actor types)
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("add_actor")
                        .selected_text("+ Add")
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(false, "Mevo").clicked() {
                                let existing: Vec<&str> = self.settings.actors.iter()
                                    .filter_map(|a| match a {
                                        ActorFormEntry::Device(d) if d.monitor_type == "mevo" => Some(d.id.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                let id = next_index(&existing);
                                self.settings.actors.push(ActorFormEntry::Device(DeviceFormEntry {
                                    id,
                                    monitor_type: "mevo".into(),
                                    name: "Mevo WiFi".into(),
                                    address: "192.168.2.1:5100".into(),
                                    ball_type: 1,
                                    tee_height_val: "1.5".into(),
                                    tee_height_unit: "inches".into(),
                                    range_val: "8".into(),
                                    range_unit: "feet".into(),
                                    surface_height_val: "0".into(),
                                    surface_height_unit: "inches".into(),
                                    track_pct: "80".into(),
                                    use_estimated: true,
                                    camera_mode: CameraMode::default(),
                                    discard_zero_spin: true,
                                    square_club: None,
                                    square_advanced_spin: None,
                                    square_impact_mm: std::collections::HashMap::new(),
                                    dirty: true,
                                }));
                                self.settings.dirty = true;
                            }
                            if ui.selectable_label(false, "Square Golf Omni").clicked() {
                                let existing: Vec<&str> = self.settings.actors.iter()
                                    .filter_map(|a| match a {
                                        ActorFormEntry::Device(d) if d.monitor_type == "square" => Some(d.id.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                let id = next_index(&existing);
                                self.settings.actors.push(ActorFormEntry::Device(DeviceFormEntry {
                                    id,
                                    monitor_type: "square".into(),
                                    name: "Square Golf Omni".into(),
                                    // Blank address = auto-discover by name prefix.
                                    address: String::new(),
                                    ball_type: 0,
                                    tee_height_val: "1.5".into(),
                                    tee_height_unit: "inches".into(),
                                    range_val: "8".into(),
                                    range_unit: "feet".into(),
                                    surface_height_val: "0".into(),
                                    surface_height_unit: "inches".into(),
                                    track_pct: "80".into(),
                                    use_estimated: true,
                                    camera_mode: CameraMode::default(),
                                    discard_zero_spin: true,
                                    square_club: None,
                                    square_advanced_spin: None,
                                    square_impact_mm: std::collections::HashMap::new(),
                                    dirty: true,
                                }));
                                self.settings.dirty = true;
                            }
                            if ui.selectable_label(false, "R10").clicked() {
                                let existing: Vec<&str> = self.settings.actors.iter()
                                    .filter_map(|a| match a {
                                        ActorFormEntry::Device(d) if d.monitor_type == "r10" => Some(d.id.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                let id = next_index(&existing);
                                self.settings.actors.push(ActorFormEntry::Device(DeviceFormEntry {
                                    id,
                                    monitor_type: "r10".into(),
                                    name: "Garmin R10".into(),
                                    address: String::new(),
                                    ball_type: 0,
                                    tee_height_val: "1.5".into(),
                                    tee_height_unit: "inches".into(),
                                    range_val: "8".into(),
                                    range_unit: "feet".into(),
                                    surface_height_val: "0".into(),
                                    surface_height_unit: "inches".into(),
                                    track_pct: "80".into(),
                                    use_estimated: true,
                                    camera_mode: CameraMode::default(),
                                    discard_zero_spin: true,
                                    square_club: None,
                                    square_advanced_spin: None,
                                    square_impact_mm: std::collections::HashMap::new(),
                                    dirty: true,
                                }));
                                self.settings.dirty = true;
                            }
                            if ui.selectable_label(false, "Uneekor (OpenConnect)").clicked() {
                                let existing: Vec<&str> = self.settings.actors.iter()
                                    .filter_map(|a| match a {
                                        ActorFormEntry::Device(d) if d.monitor_type == "openconnect_server" => Some(d.id.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                let id = next_index(&existing);
                                self.settings.actors.push(ActorFormEntry::Device(DeviceFormEntry {
                                    id,
                                    monitor_type: "openconnect_server".into(),
                                    name: "OpenConnect Server".into(),
                                    address: "0.0.0.0:921".into(),
                                    ball_type: 0,
                                    tee_height_val: "1.5".into(),
                                    tee_height_unit: "inches".into(),
                                    range_val: "8".into(),
                                    range_unit: "feet".into(),
                                    surface_height_val: "0".into(),
                                    surface_height_unit: "inches".into(),
                                    track_pct: "80".into(),
                                    use_estimated: true,
                                    camera_mode: CameraMode::default(),
                                    discard_zero_spin: true,
                                    square_club: None,
                                    square_advanced_spin: None,
                                    square_impact_mm: std::collections::HashMap::new(),
                                    dirty: true,
                                }));
                                self.settings.dirty = true;
                            }
                            if ui.selectable_label(false, "GSPro").clicked() {
                                let existing: Vec<&str> = self.settings.actors.iter()
                                    .filter_map(|a| match a {
                                        ActorFormEntry::Integration(i) if i.integration_type == "gspro" => Some(i.id.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                let id = next_index(&existing);
                                self.settings.actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
                                    id,
                                    integration_type: "gspro".into(),
                                    name: "Local GSPro".into(),
                                    address: "127.0.0.1:921".into(),
                                    full_monitor: String::new(),
                                    chipping_monitor: String::new(),
                                    putting_monitor: String::new(),
                                    dirty: true,
                                }));
                                self.settings.dirty = true;
                            }
                            if ui.selectable_label(false, "Web Server").clicked() {
                                let existing: Vec<&str> = self.settings.actors.iter()
                                    .filter_map(|a| match a {
                                        ActorFormEntry::Integration(i) if i.integration_type == "webserver" => Some(i.id.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                let id = next_index(&existing);
                                self.settings.actors.push(ActorFormEntry::Integration(IntegrationFormEntry {
                                    id,
                                    integration_type: "webserver".into(),
                                    name: "Web Server".into(),
                                    address: "0.0.0.0:5880".into(),
                                    full_monitor: String::new(),
                                    chipping_monitor: String::new(),
                                    putting_monitor: String::new(),
                                    dirty: true,
                                }));
                                self.settings.dirty = true;
                            }
                        });
                });

                ui.add_space(12.0);

                // --- Status indicators ---
                if self.settings.saving {
                    ui.horizontal(|ui| {
                        ui.spinner();
                    });
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::is_ble_address;

    #[test]
    fn ble_address_accepts_mac_and_uuid() {
        assert!(is_ble_address("DC:0D:30:62:54:E4"));
        assert!(is_ble_address(" dc:0d:30:62:54:e4 "));
        assert!(is_ble_address("5F2A9C1E-3B7D-4E8A-9C0F-1A2B3C4D5E6F"));
    }

    #[test]
    fn ble_address_accepts_advertised_name() {
        assert!(is_ble_address("SquareGolf(54E4)"));
        assert!(is_ble_address(" squaregolf(54e4) "));
    }

    #[test]
    fn ble_address_rejects_malformed() {
        assert!(!is_ble_address(""));
        assert!(!is_ble_address("DC:0D:30:62:54"));
        assert!(!is_ble_address("DC:0D:30:62:54:G4"));
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
}
