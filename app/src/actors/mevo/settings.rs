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

    pub(crate) fn to_avr_settings(&self, mode: &ShotDetectionMode) -> AvrSettings {
        let comms_index = match mode {
            ShotDetectionMode::Full => MODE_OUTDOOR,
            ShotDetectionMode::Putting => MODE_PUTTING,
            ShotDetectionMode::Chipping => MODE_CHIPPING,
        };

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
            radar_cal: RadarCal {
                range_mm: self.range.as_mm(),
                height_mm: (self.surface_height.as_inches() * 25.4).floor() as u8,
            },
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
