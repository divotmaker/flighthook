use ironsight::protocol::camera::CamConfig;
use ironsight::protocol::config::{
    MODE_CHIPPING, MODE_FULL_OUTDOOR, MODE_LIMITED_FLIGHT, MODE_PUTTING, ParamData, ParamValue,
    RadarCal,
};
use ironsight::seq::{AvrSettings, XiSettings, XiShotType};

use crate::state::config::MevoSection;
use flighthook::{CameraMode, Distance, DistanceExt, FlightScopeEnvironment, ShotDetectionMode};

/// Mutable session configuration. Distance fields carry both value and unit;
/// wire-protocol conversions happen on demand in `to_avr_settings()`.
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

    /// Full AVR settings for initial configuration: params + RadarCal.
    pub(crate) fn to_avr_settings(&self, mode: &ShotDetectionMode) -> AvrSettings {
        let comms_index = self.comms_index(mode);

        AvrSettings {
            mode: comms_index,
            params: vec![
                ParamValue {
                    param_id: 0x06,
                    value: ParamData::Int24(i32::from(self.ball_type)),
                },
                ParamValue {
                    param_id: 0x0F,
                    value: ParamData::Float40(self.track_pct / 100.0),
                },
                ParamValue {
                    param_id: 0x26,
                    value: ParamData::Float40(self.tee_height.as_meters()),
                },
            ],
            radar_cal: Some(self.radar_cal()),
        }
    }

    /// Radar calibration from `range` and `surface_height`.
    fn radar_cal(&self) -> RadarCal {
        RadarCal {
            range_mm: self.range.to_mm(),
            height_mm: (self.surface_height.as_inches() * 25.4).floor() as u8,
        }
    }

    /// Minimal AVR settings for mode changes: ModeSet only, no params or
    /// RadarCal. Full params trigger ~29s of ConfigNack retries on ARM;
    /// mode-only significantly reduces (but does not eliminate) retries.
    pub(crate) fn to_avr_settings_mode_only(&self, mode: &ShotDetectionMode) -> AvrSettings {
        AvrSettings {
            mode: self.comms_index(mode),
            params: vec![],
            radar_cal: None,
        }
    }

    /// Xi configuration for a detection mode, set up as FS Golf sets up the
    /// Xi: Full is a full-swing session in the environment's mode; Chipping
    /// and Putting are FS Golf's Pitching session (limited flight, mode 9).
    /// The radar calibration comes from `range` and `surface_height`.
    pub(crate) fn to_xi_settings(&self, mode: &ShotDetectionMode) -> XiSettings {
        let (comms_index, shot_type) = match mode {
            ShotDetectionMode::Full => (self.environment_mode(), XiShotType::FullSwing),
            ShotDetectionMode::Chipping | ShotDetectionMode::Putting => {
                (MODE_LIMITED_FLIGHT, XiShotType::Pitching)
            }
        };
        XiSettings {
            mode: comms_index,
            shot_type,
            radar_cal: self.radar_cal(),
        }
    }

    /// Label for the Xi session a detection mode selects, with the
    /// commsIndex sent to the device.
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

/// Camera configuration for the warmup phase of a Mevo+ session.
///
/// Standard resolution (1024x768) with conservative buffer settings. The Pi
/// camera subsystem boots in this mode; a Fusion config is only accepted once
/// warmup has finished.
///
/// Unused raw-camera fields carry `-1`, matching the native app's wire
/// encoding. A `0` there reads as a real value to the Pi and disturbs SHM
/// buffer allocation, which is what Fusion capture depends on.
pub(crate) fn cam_config() -> CamConfig {
    CamConfig {
        dynamic_config: true,
        resolution_width: 1024,
        resolution_height: 768,
        rotation: 0,
        ev: 0,
        quality: 80,
        framerate: 20,
        streaming_framerate: 1,
        ringbuffer_pretime_ms: 1000,
        ringbuffer_posttime_ms: 4000,
        raw_camera_mode: 0,
        fusion_camera_mode: false,
        raw_shutter_speed_max: 0.0,
        raw_ev_roi_x: -1,
        raw_ev_roi_y: -1,
        raw_ev_roi_width: -1,
        raw_ev_roi_height: -1,
        raw_x_offset: -1,
        raw_bin44: false,
        raw_live_preview_write_interval_ms: -1,
        raw_y_offset: -1,
        buffer_sub_sampling_pre_trigger_div: -1,
        buffer_sub_sampling_post_trigger_div: -1,
        buffer_sub_sampling_switch_time_offset: -1.0,
        buffer_sub_sampling_total_buffer_size: -1,
        buffer_sub_sampling_pre_trigger_buffer_size: -1,
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
