//! The `[webserver]` section and the launch monitor sections (Mevo, R10, Square, ExPutt,
//! trackball, mock), with the `DeviceSection` trait over their driver configs.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebserverSection {
    #[serde(default)]
    pub name: String,
    pub bind: String,
}

/// A FlightScope device instance: Mevo+, Mevo Gen2 or Xi (ironsight's
/// device driver).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MevoSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Backspin to use, in rpm per mph of ball speed, for a shot whose result
    /// has launch data but no spin and no flight (very short chips on the
    /// Xi, launched at 5° or more). Sidespin is 0. `0` turns it off: such a
    /// shot then goes out with its launch alone and is not simulated. Not
    /// used for a shot whose detection mode at the trigger was Putting (it
    /// goes out as with the setting at 0). Defaults to [`MevoSection::DEFAULT_LAUNCH_ONLY_SPIN`], and is
    /// left out of the config file at the default. A value outside 0 to
    /// [`MevoSection::MAX_LAUNCH_ONLY_SPIN`] (or non-finite) is treated as
    /// `0`.
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
    /// The FlightScope driver's settings: `address`, `ball_type`,
    /// `tee_height`, `range`, `surface_height`, `track_pct`,
    /// `use_estimated`, `camera_mode`, `environment` and `spin_source`
    /// (see [`MevoConfig`]). Each is left out of the file while unset;
    /// `spin_source` while it is `d4`.
    #[serde(flatten)]
    pub driver: MevoConfig,
}

impl DeviceSection for MevoSection {
    type Driver = MevoConfig;
    const KIND: &'static str = "mevo";

    fn driver(&self) -> &MevoConfig {
        &self.driver
    }

    fn sections(config: &FlighthookConfig) -> &std::collections::HashMap<String, Self> {
        &config.mevo
    }
}

pub(super) fn default_launch_only_spin() -> f64 {
    MevoSection::DEFAULT_LAUNCH_ONLY_SPIN
}

pub(super) fn is_default_launch_only_spin(v: &f64) -> bool {
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

/// A Garmin R10 BLE device instance (tenover's device driver).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct R10Section {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
    /// The R10 driver's settings: `range`, the distance from the device to
    /// the tee, sent to the R10 as its tee distance once it wakes up (must be
    /// greater than 0; absent leaves the device's own tee distance alone;
    /// Garmin recommends 6-8 ft behind the ball). Left out of the file while
    /// unset.
    #[serde(flatten)]
    pub driver: R10Config,
}

impl DeviceSection for R10Section {
    type Driver = R10Config;
    const KIND: &'static str = "r10";

    fn driver(&self) -> &R10Config {
        &self.driver
    }

    fn sections(config: &FlighthookConfig) -> &std::collections::HashMap<String, Self> {
        &config.r10
    }
}

/// A Square Golf Omni BLE device instance (allsquare's device driver).
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
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
    /// The Square Golf driver's settings, each left out of the file at its
    /// default: `address` (the device's advertised name, e.g.
    /// `SquareGolf(CD01)`, or its BLE address; unset or blank finds the
    /// first Square Golf device), `club` (the club armed on connect until
    /// the game names one, default `7I`), `advanced_spin` (default true),
    /// `discard_non_putting_zero_spin` (drop shots that read no spin at all
    /// unless the putter is in play, default true) and
    /// `dot_bottom_to_face_center_mm` (per-club face impact calibration in
    /// mm, keyed by club code; `dot_bottom_to_face_centre_mm` is read too).
    #[serde(flatten)]
    pub driver: SquareConfig,
}

impl DeviceSection for SquareSection {
    type Driver = SquareConfig;
    const KIND: &'static str = "square";

    fn driver(&self) -> &SquareConfig {
        &self.driver
    }

    fn sections(config: &FlighthookConfig) -> &std::collections::HashMap<String, Self> {
        &config.square
    }
}

/// An ExPutt RG putting device instance, reached over WiFi (threeputt's
/// device driver). The device must be in its mobile mode on the same
/// network.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExPuttSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Seconds before the shot's trigger time that the stroke's impact
    /// happened, for the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`].
    /// Absent uses the device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
    /// The ExPutt driver's settings: `address`, the device's IP address
    /// (unset or blank finds the device on the local network; anything else
    /// must be an IP address). Left out of the file while unset.
    #[serde(flatten)]
    pub driver: ExPuttConfig,
}

impl DeviceSection for ExPuttSection {
    type Driver = ExPuttConfig;
    const KIND: &'static str = "exputt";

    fn driver(&self) -> &ExPuttConfig {
        &self.driver
    }

    fn sections(config: &FlighthookConfig) -> &std::collections::HashMap<String, Self> {
        &config.exputt
    }
}

/// A trackball launch monitor instance (alpha; thumbswing's device driver):
/// a trackball or mouse read on Linux, where pulling the ball back and
/// pushing it forward makes a full shot. Every value is synthesized from the
/// swing, so it is a virtual launch monitor. It has no `impact_lead_s`: the
/// cameras cut no clip for its shots.
///
/// flighthook never grabs the trackball, so the desktop pointer keeps
/// following the ball: the driver always runs with its `grab` off
/// ([`DeviceSection::driver_settings`]), a `grab` key read from a file or a
/// request is ignored, and none is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "TrackballTable", into = "TrackballTable")]
pub struct TrackballSection {
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    pub actor_id: Option<String>,
    /// The trackball driver's settings, each left out of the file at its
    /// default: `device` (a `/dev/input` path, a `/dev/input/by-id` name or
    /// the name the device reports; unset or blank uses the only trackball
    /// or mouse), `club` (until the game names one, default `7I`), `cpi`,
    /// `report_limit`, a `thresholds` table and a `tuning` table. Its
    /// `config.grab` is always false, and its `hand` is always the default:
    /// the hand comes from the turn. A `hand` key in the file is ignored.
    pub driver: TrackballConfig,
}

/// A [`TrackballSection`] as the file and the REST JSON hold it.
#[derive(Serialize, Deserialize)]
pub(super) struct TrackballTable {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub(super) enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) actor_id: Option<String>,
    #[serde(flatten)]
    pub(super) driver: TrackballConfig,
}

impl From<TrackballTable> for TrackballSection {
    /// A `grab` or `hand` the table holds is dropped: the card never grabs,
    /// and the hand comes from the turn.
    fn from(table: TrackballTable) -> Self {
        let mut driver = table.driver;
        driver.config.grab = false;
        driver.hand = thumbswing::Hand::default();
        Self {
            name: table.name,
            enabled: table.enabled,
            actor_id: table.actor_id,
            driver,
        }
    }
}

impl From<TrackballSection> for TrackballTable {
    /// The driver leaves a field at its default out of the table, so the
    /// grab and the hand are set to the driver's defaults to leave the keys
    /// out.
    fn from(section: TrackballSection) -> Self {
        let mut driver = section.driver;
        driver.config.grab = thumbswing::Config::default().grab;
        driver.hand = thumbswing::Hand::default();
        Self {
            name: section.name,
            enabled: section.enabled,
            actor_id: section.actor_id,
            driver,
        }
    }
}

impl DeviceSection for TrackballSection {
    type Driver = TrackballConfig;
    const KIND: &'static str = "trackball";

    fn driver(&self) -> &TrackballConfig {
        &self.driver
    }

    /// The driver's settings with the grab off.
    fn driver_settings(&self) -> TrackballConfig {
        let mut settings = self.driver.clone();
        settings.config.grab = false;
        settings
    }

    fn sections(config: &FlighthookConfig) -> &std::collections::HashMap<String, Self> {
        &config.trackball
    }
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

    /// The settings the driver runs with: [`driver`](Self::driver), with
    /// the settings the host decides for itself put in. Default: the
    /// driver's settings as they are.
    #[must_use]
    fn driver_settings(&self) -> Self::Driver {
        self.driver().clone()
    }

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

    /// [`driver_error`](Self::driver_error) for a save: `None` also when
    /// the driver settings are those of `saved`, the section as it is saved
    /// now, so a card whose saved driver settings are refused can still
    /// save its other fields. Only a change into refused settings counts.
    #[must_use]
    fn changed_driver_error(&self, saved: Option<&Self>, id: &str) -> Option<String> {
        // Compared as JSON too, so a NaN read from the file equals itself.
        if saved.is_some_and(|s| {
            s.driver() == self.driver()
                || serde_json::to_value(s.driver()).ok() == serde_json::to_value(self.driver()).ok()
        }) {
            return None;
        }
        self.driver_error(id)
    }
}

/// The first section of type `S` in `config`, in index order
/// ([`index_order`](crate::index_order)), whose driver settings are not
/// usable ([`DeviceSection::driver_error`]); with `saved`, only one whose
/// driver settings differ from that config's
/// ([`DeviceSection::changed_driver_error`]).
pub(super) fn first_driver_error<S: DeviceSection>(
    config: &FlighthookConfig,
    saved: Option<&FlighthookConfig>,
) -> Option<String> {
    let mut sections: Vec<_> = S::sections(config).iter().collect();
    sections.sort_by(|a, b| crate::index_order(a.0, b.0));
    sections.into_iter().find_map(|(index, s)| {
        let id = format!("{}.{index}", S::KIND);
        match saved {
            Some(saved) => s.changed_driver_error(S::sections(saved).get(index), &id),
            None => s.driver_error(&id),
        }
    })
}

/// A mock launch monitor instance: a virtual device that makes a shot at a
/// fixed interval (flightrelay's mock driver).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockMonitorSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
    /// The mock driver's settings: `shot_interval_s`, `arm_delay_s`,
    /// `ball_ready_delay_s`, `post_shot_delay_s`, `serial`. Each one is left
    /// out of the file while at its default.
    #[serde(flatten)]
    pub driver: MockConfig,
}

impl Default for MockMonitorSection {
    fn default() -> Self {
        Self {
            actor_id: None,
            name: String::new(),
            enabled: true,
            impact_lead_s: None,
            driver: MockConfig::default(),
        }
    }
}

impl DeviceSection for MockMonitorSection {
    type Driver = MockConfig;
    const KIND: &'static str = "mock_monitor";

    fn driver(&self) -> &MockConfig {
        &self.driver
    }

    fn sections(config: &FlighthookConfig) -> &std::collections::HashMap<String, Self> {
        &config.mock_monitor
    }
}
