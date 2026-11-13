//! The `[frp]` and `[simulation]` sections: flighthook's own FRP actor ids, and the simulation's
//! mode, surface, altitude, temperature and Auto-chip setting.

use super::*;

/// flighthook's own FRP actors: their ids, generated once and kept so they
/// stay the same actors across restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrpSection {
    /// The flighthook actor (session, collector).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// The Auto-chip launch monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_chip_actor_id: Option<String>,
}

impl FrpSection {
    /// True while no id is set.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
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
    /// Altitude above sea level, any distance unit. Defaults to `"0ft"`;
    /// omitted from the file and the API while zero. Valid range: 0 to
    /// 5000 m.
    #[serde(
        default = "default_sim_altitude",
        skip_serializing_if = "is_zero_altitude"
    )]
    pub altitude: Distance,
    /// Air temperature, degrees Celsius. Defaults to 25; omitted from the
    /// file and the API while at the default. Valid range: -30 to 50.
    #[serde(
        default = "default_sim_temperature_c",
        skip_serializing_if = "is_default_sim_temperature_c"
    )]
    pub temperature_c: f64,
    /// Ground the ball lands and rolls on. Defaults to `fairway`; omitted
    /// from the file and the API while at the default.
    #[serde(default, skip_serializing_if = "SimSurface::is_default")]
    pub surface: SimSurface,
    /// Whether the Auto-chip control is offered (the header chip and
    /// `POST /api/auto-chip`). Defaults to `false`; omitted from the file and
    /// the API while at the default. Auto-chip is simulated under the
    /// conditions above whatever `mode` is.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_chip: bool,
    /// Whether tournament mode is switched on: Auto-chip is off and the
    /// virtual launch monitors hold their shots. Read at start-up and
    /// written only by `POST /api/tournament`: a settings save keeps the
    /// live value whatever its body carries. Defaults to `false`; omitted
    /// from the file and the API while off.
    #[serde(default, skip_serializing_if = "is_false")]
    pub tournament: bool,
    /// The flight model's FRP actor id (the Simulation card), generated once
    /// and kept. Absent until flighthook first runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
}

pub(super) fn is_false(value: &bool) -> bool {
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

pub(super) fn default_sim_altitude() -> Distance {
    Distance::Feet(0.0)
}

pub(super) fn default_sim_temperature_c() -> f64 {
    SimulationSection::DEFAULT_TEMPERATURE_C
}

pub(super) fn is_zero_altitude(v: &Distance) -> bool {
    v.as_meters() == 0.0
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes a reference
pub(super) fn is_default_sim_temperature_c(v: &f64) -> bool {
    *v == SimulationSection::DEFAULT_TEMPERATURE_C
}

impl Default for SimulationSection {
    fn default() -> Self {
        Self {
            actor_id: None,
            mode: SimMode::default(),
            altitude: default_sim_altitude(),
            temperature_c: default_sim_temperature_c(),
            surface: SimSurface::default(),
            auto_chip: false,
            tournament: false,
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

    /// True when every setting is at its default and the flight model has
    /// no actor id yet (an id is kept in the file). Altitude compares by
    /// length, so `"0m"` and `"0ft"` are both default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.mode.is_default()
            && is_zero_altitude(&self.altitude)
            && is_default_sim_temperature_c(&self.temperature_c)
            && self.surface.is_default()
            && !self.auto_chip
            && !self.tournament
            && self.actor_id.is_none()
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
    /// True for the default surface ([`SimSurface::Fairway`]).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

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
