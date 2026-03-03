use ironsight::protocol::camera::CamConfig;
use ironsight::protocol::config::{
    MODE_CHIPPING, MODE_OUTDOOR, MODE_PUTTING, ParamData, ParamValue, RadarCal,
};
use ironsight::seq::AvrSettings;

use crate::state::config::MevoSection;
use flighthook::MevoConfigEvent;
use flighthook::{Distance, PartialMode, ShotDetectionMode};

/// Mutable session configuration. Distance fields carry both value and unit;
/// wire-protocol conversions happen on demand in `to_avr_settings()`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    pub ball_type: u8, // 0 = RCT, 1 = Standard
    pub tee_height: Distance,
    pub range: Distance,
    pub surface_height: Distance,
    pub track_pct: f64, // 0-100 (user units)
    pub use_partial: PartialMode,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            ball_type: 0,
            tee_height: Distance::Inches(1.5),
            range: Distance::Feet(8.0),
            surface_height: Distance::Inches(0.0),
            track_pct: 80.0,
            use_partial: PartialMode::default(),
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
            use_partial: s.use_partial.unwrap_or(defaults.use_partial),
        }
    }

    pub fn apply_config_event(&mut self, event: &MevoConfigEvent) {
        self.ball_type = event.ball_type;
        self.tee_height = event.tee_height;
        self.range = event.range;
        self.surface_height = event.surface_height;
        self.track_pct = event.track_pct;
        self.use_partial = event.use_partial;
    }

    /// Human-readable label for the detection mode wire value.
    /// Includes both the schema enum name and the commsIndex sent to the device.
    pub fn mode_label(mode: &ShotDetectionMode) -> &'static str {
        match mode {
            ShotDetectionMode::Full => "outdoor(9)",
            ShotDetectionMode::Putting => "putting(3)",
            ShotDetectionMode::Chipping => "chipping(5)",
        }
    }

    /// Full AVR settings for initial configuration: params + RadarCal.
    pub(crate) fn to_avr_settings(&self, mode: &ShotDetectionMode) -> AvrSettings {
        let comms_index = Self::comms_index(mode);

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
            radar_cal: Some(RadarCal {
                range_mm: self.range.as_mm(),
                height_mm: (self.surface_height.as_inches() * 25.4).floor() as u8,
            }),
        }
    }

    /// Minimal AVR settings for mode changes: ModeSet only, no params or
    /// RadarCal. Full params trigger ~29s of ConfigNack retries on ARM;
    /// mode-only significantly reduces (but does not eliminate) retries.
    pub(crate) fn to_avr_settings_mode_only(mode: &ShotDetectionMode) -> AvrSettings {
        AvrSettings {
            mode: Self::comms_index(mode),
            params: vec![],
            radar_cal: None,
        }
    }

    fn comms_index(mode: &ShotDetectionMode) -> u8 {
        match mode {
            ShotDetectionMode::Full => MODE_OUTDOOR,
            ShotDetectionMode::Putting => MODE_PUTTING,
            ShotDetectionMode::Chipping => MODE_CHIPPING,
        }
    }

    pub fn to_config_event(&self) -> MevoConfigEvent {
        MevoConfigEvent {
            ball_type: self.ball_type,
            tee_height: self.tee_height,
            range: self.range,
            surface_height: self.surface_height,
            track_pct: self.track_pct,
            use_partial: self.use_partial,
        }
    }
}

/// Default camera configuration for Mevo+ sessions.
///
/// Uses standard resolution (1024×768) with conservative buffer settings.
/// The `standard_preset()` values from ironsight use -1 sentinel values
/// for unused raw-camera fields, which is correct for the wire protocol.
/// We keep our own explicit config to control ringbuffer timing and
/// sub-sampling independently.
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
        raw_ev_roi_x: 0,
        raw_ev_roi_y: 0,
        raw_ev_roi_width: 0,
        raw_ev_roi_height: 0,
        raw_x_offset: 0,
        raw_bin44: false,
        raw_live_preview_write_interval_ms: 0,
        raw_y_offset: 0,
        buffer_sub_sampling_pre_trigger_div: 1,
        buffer_sub_sampling_post_trigger_div: 1,
        buffer_sub_sampling_switch_time_offset: 0.0,
        buffer_sub_sampling_total_buffer_size: 0,
        buffer_sub_sampling_pre_trigger_buffer_size: 0,
    }
}
