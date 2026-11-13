//! Software section form state: `IntegrationFormEntry` (an output card), `ActorFormEntry` over
//! every card type, and the new entry a (+) preset adds.

use super::*;

/// The bind address of an Open Connect server whose bind is blank.
pub(crate) const OPENCONNECT_BIND: &str = "0.0.0.0:921";

/// Pureplay's Accessory address when a Pureplay Accessory card's is blank.
pub(crate) const PUREPLAY_ACCESSORY_ADDRESS: &str = "127.0.0.1:9770";

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
    /// An Open Connect client's clubs that set chipping and putting.
    pub(crate) chipping_clubs: Vec<Club>,
    pub(crate) putting_clubs: Vec<Club>,
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
                "pureplay_accessory" => "Pureplay Accessory",
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
                    "Accepts inbound shots from launch monitor software that sends GSPro Open Connect. Bind address, default 0.0.0.0:921 — to share a host with GSPro, move GSPConnect to 922 via OpenAPIUseAltPort"
                }
                "pureplay_accessory" => {
                    "Pureplay Accessory (beta) — Pureplay's Accessory API (port 9770): Pureplay's club, hand, shots and results in flighthook, and flighthook's club picks in Pureplay. Approve flighthook in Pureplay the first time"
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
    pub(super) fn set_enabled(&mut self, on: bool) {
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
    pub(super) fn sort_key(&self) -> (&str, &str, &str) {
        match self {
            ActorFormEntry::Device(d) => (&d.name, self.type_label(), &d.id),
            ActorFormEntry::Integration(i) => (&i.name, self.type_label(), &i.id),
        }
    }
}

/// A new, unsaved entry from (+) menu `preset` at index `id`, with its
/// type's defaults and the preset's name and address. `None` for a type no
/// (+) menu offers.
pub(super) fn new_actor_entry(preset: &AddPreset, id: String) -> Option<ActorFormEntry> {
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
            pureplay_shots: flighthook::PureplayShots::Auto,
            aim_offset: String::new(),
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
            // A new Open Connect client sets the mode as GSPro needs.
            chipping_clubs: if type_key == "openconnect_client" {
                flighthook::default_chipping_clubs()
            } else {
                Vec::new()
            },
            putting_clubs: if type_key == "openconnect_client" {
                flighthook::default_putting_clubs()
            } else {
                Vec::new()
            },
            dirty: true,
        })
    };
    match type_key {
        "mevo" => Some(device(1)),
        "square" | "r10" | "exputt" | "trackball" | "openconnect_server" | "pureplay_accessory"
        | "camera" => Some(device(0)),
        "openconnect_client" | "pureplay" => Some(integration()),
        _ => None,
    }
}
