use ironsight::protocol::camera::CamConfig;
use ironsight::protocol::config::{
    MODE_CHIPPING, MODE_FULL_OUTDOOR, MODE_LIMITED_FLIGHT, MODE_PUTTING, RadarCal,
};
use ironsight::seq::{AvrSettings, MevoSettings, XiSettings};

use crate::state::config::MevoSection;
use flighthook::{CameraMode, Distance, DistanceExt, FlightScopeEnvironment, ShotDetectionMode};

/// Mutable session configuration. Distance fields carry both value and unit;
/// they are converted to ironsight's units on demand in `to_avr_settings()`
/// and `to_xi_settings()`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    pub ball_type: u8, // 0 = Standard, 1 = RCT
    pub tee_height: Distance,
    pub range: Distance,
    pub surface_height: Distance,
    pub track_pct: f64, // 0-100 (user units)
    /// Picks the full-swing detection mode (Mevo+ and Xi).
    pub environment: FlightScopeEnvironment,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            ball_type: 1,
            tee_height: Distance::Inches(1.5),
            range: Distance::Feet(8.0),
            surface_height: Distance::Inches(0.0),
            track_pct: 80.0,
            environment: FlightScopeEnvironment::LimitedFlight,
        }
    }
}

impl SessionConfig {
    /// Build from a Mevo config section.
    /// Falls back to defaults for any field that is None.
    pub fn from_mevo_section(s: &MevoSection) -> Self {
        let defaults = Self::default();
        Self {
            ball_type: s.ball_type.unwrap_or(defaults.ball_type),
            tee_height: s.tee_height.unwrap_or(defaults.tee_height),
            range: s.range.unwrap_or(defaults.range),
            surface_height: s.surface_height.unwrap_or(defaults.surface_height),
            track_pct: s.track_pct.unwrap_or(defaults.track_pct),
            environment: s.environment.unwrap_or(defaults.environment),
        }
    }

    /// Human-readable label for the detection mode wire value.
    /// Includes both the mode name and the commsIndex sent to the device.
    pub fn mode_label(&self, mode: &ShotDetectionMode) -> &'static str {
        match mode {
            ShotDetectionMode::Full => self.environment_label(),
            ShotDetectionMode::Putting => "putting(3)",
            ShotDetectionMode::Chipping => "chipping(5)",
        }
    }

    /// Label for the environment's detection mode, with its commsIndex.
    pub fn environment_label(&self) -> &'static str {
        match self.environment {
            FlightScopeEnvironment::LimitedFlight => "limited-flight(9)",
            FlightScopeEnvironment::Outdoor => "outdoor(8)",
        }
    }

    /// Detection mode commsIndex for the environment: the full-swing mode of
    /// the Mevo+ and the Xi.
    pub fn environment_mode(&self) -> u8 {
        match self.environment {
            FlightScopeEnvironment::LimitedFlight => MODE_LIMITED_FLIGHT,
            FlightScopeEnvironment::Outdoor => MODE_FULL_OUTDOOR,
        }
    }

    /// Full Mevo+ settings for the initial configuration: mode, ball type,
    /// track percentage, tee height and radar calibration
    /// (ironsight's `MevoSettings`).
    pub(crate) fn to_avr_settings(&self, mode: &ShotDetectionMode) -> AvrSettings {
        MevoSettings::new(
            self.comms_index(mode),
            self.ball_type,
            self.track_pct / 100.0,
            self.tee_height.as_meters(),
            self.radar_cal(),
        )
        .into()
    }

    /// Radar calibration from `range` and `surface_height`.
    fn radar_cal(&self) -> RadarCal {
        RadarCal {
            range_mm: self.range.to_mm(),
            height_mm: (self.surface_height.as_inches() * 25.4).floor() as u8,
        }
    }

    /// Mevo+ settings for a mode change: the detection mode alone
    /// (ironsight's `AvrSettings::mode_only`).
    pub(crate) fn to_avr_settings_mode_only(&self, mode: &ShotDetectionMode) -> AvrSettings {
        AvrSettings::mode_only(self.comms_index(mode))
    }

    /// Xi configuration for a detection mode: Full is a full-swing session
    /// in the environment's mode; Chipping and Putting are the Xi's pitching
    /// shot type. The radar calibration comes from `range` and
    /// `surface_height`.
    pub(crate) fn to_xi_settings(&self, mode: &ShotDetectionMode) -> XiSettings {
        match mode {
            ShotDetectionMode::Full => {
                XiSettings::full_swing(self.environment_mode(), self.radar_cal())
            }
            ShotDetectionMode::Chipping | ShotDetectionMode::Putting => {
                XiSettings::pitching(self.radar_cal())
            }
        }
    }

    /// Label for the Xi shot type a detection mode selects, with the
    /// detection mode sent to the device.
    pub fn xi_label(&self, mode: &ShotDetectionMode) -> &'static str {
        match (mode, self.environment) {
            (ShotDetectionMode::Full, FlightScopeEnvironment::LimitedFlight) => {
                "full-swing limited-flight(9)"
            }
            (ShotDetectionMode::Full, FlightScopeEnvironment::Outdoor) => "full-swing outdoor(8)",
            (ShotDetectionMode::Chipping | ShotDetectionMode::Putting, _) => {
                "pitching limited-flight(9)"
            }
        }
    }

    fn comms_index(&self, mode: &ShotDetectionMode) -> u8 {
        match mode {
            ShotDetectionMode::Full => self.environment_mode(),
            ShotDetectionMode::Putting => MODE_PUTTING,
            ShotDetectionMode::Chipping => MODE_CHIPPING,
        }
    }
}

/// The camera configuration to apply once warmup has finished, for a mode that
/// asks for Fusion processing. `Standard` keeps the warmup config and needs no
/// second phase.
pub(crate) fn fusion_cam_config(mode: CameraMode) -> Option<CamConfig> {
    match mode {
        CameraMode::Standard => None,
        CameraMode::Fusion => Some(CamConfig::fusion_preset()),
        CameraMode::RawFusion => Some(CamConfig::raw_fusion_preset()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironsight::seq::XiShotType;

    fn session(environment: FlightScopeEnvironment) -> SessionConfig {
        SessionConfig {
            environment,
            ..SessionConfig::default()
        }
    }

    #[test]
    fn environment_sets_full_swing_mode_only() {
        let limited = session(FlightScopeEnvironment::LimitedFlight);
        let outdoor = session(FlightScopeEnvironment::Outdoor);
        assert_eq!(limited.to_avr_settings(&ShotDetectionMode::Full).mode, 9);
        assert_eq!(outdoor.to_avr_settings(&ShotDetectionMode::Full).mode, 8);
        for s in [&limited, &outdoor] {
            assert_eq!(
                s.to_avr_settings_mode_only(&ShotDetectionMode::Chipping)
                    .mode,
                5
            );
            assert_eq!(
                s.to_avr_settings_mode_only(&ShotDetectionMode::Putting)
                    .mode,
                3
            );
            assert_eq!(s.to_avr_settings(&ShotDetectionMode::Chipping).mode, 5);
            assert_eq!(s.to_avr_settings(&ShotDetectionMode::Putting).mode, 3);
        }
        assert_eq!(
            outdoor
                .to_avr_settings_mode_only(&ShotDetectionMode::Full)
                .mode,
            8
        );
        assert_eq!(
            limited.mode_label(&ShotDetectionMode::Full),
            "limited-flight(9)"
        );
        assert_eq!(outdoor.mode_label(&ShotDetectionMode::Full), "outdoor(8)");
        assert_eq!(
            outdoor.mode_label(&ShotDetectionMode::Chipping),
            "chipping(5)"
        );
    }

    #[test]
    fn xi_full_swing_follows_environment() {
        for (env, mode, label) in [
            (
                FlightScopeEnvironment::LimitedFlight,
                9,
                "full-swing limited-flight(9)",
            ),
            (FlightScopeEnvironment::Outdoor, 8, "full-swing outdoor(8)"),
        ] {
            let s = session(env);
            let xi = s.to_xi_settings(&ShotDetectionMode::Full);
            assert_eq!(xi.mode, mode);
            assert_eq!(xi.shot_type, XiShotType::FullSwing);
            assert_eq!(s.xi_label(&ShotDetectionMode::Full), label);
        }
    }

    #[test]
    fn xi_chipping_and_putting_are_pitching_in_limited_flight() {
        for env in [
            FlightScopeEnvironment::LimitedFlight,
            FlightScopeEnvironment::Outdoor,
        ] {
            let s = session(env);
            for mode in [ShotDetectionMode::Chipping, ShotDetectionMode::Putting] {
                let xi = s.to_xi_settings(&mode);
                assert_eq!(xi.mode, MODE_LIMITED_FLIGHT);
                assert_eq!(xi.shot_type, XiShotType::Pitching);
                assert_eq!(s.xi_label(&mode), "pitching limited-flight(9)");
            }
        }
    }

    #[test]
    fn xi_radar_cal_comes_from_range_and_surface_height() {
        let s = SessionConfig {
            range: Distance::Feet(10.0),
            surface_height: Distance::Inches(1.0),
            ..SessionConfig::default()
        };
        let cal = s.to_xi_settings(&ShotDetectionMode::Full).radar_cal;
        assert_eq!(cal.range_mm, 3048);
        assert_eq!(cal.height_mm, 25);
        // Same calibration as the Mevo+ gets.
        assert_eq!(
            Some(cal),
            s.to_avr_settings(&ShotDetectionMode::Full).radar_cal
        );
        // Defaults: 8 ft, 0 in.
        let cal = SessionConfig::default()
            .to_xi_settings(&ShotDetectionMode::Chipping)
            .radar_cal;
        assert_eq!((cal.range_mm, cal.height_mm), (2438, 0));
    }

    #[test]
    fn absent_environment_is_limited_flight() {
        let section = MevoSection::default();
        assert_eq!(section.environment, None);
        let s = SessionConfig::from_mevo_section(&section);
        assert_eq!(s.environment, FlightScopeEnvironment::LimitedFlight);
        assert_eq!(s, SessionConfig::default());
    }
}
