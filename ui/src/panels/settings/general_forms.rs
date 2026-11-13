//! The General and Cameras sections' settings-only card forms: Simulation, Replay and Body tracking.

use super::*;

/// Altitude units offered for the simulation.
pub(super) const ALTITUDE_UNITS: &[(&str, &str)] = &[("feet", "ft"), ("meters", "m")];

pub(super) fn celsius_to_fahrenheit(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

pub(super) fn fahrenheit_to_celsius(f: f64) -> f64 {
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
    pub(super) loaded: SimulationSection,
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

    pub(super) fn temperature_text(c: f64, fahrenheit: bool) -> String {
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

    pub(super) fn altitude(&self) -> Option<Distance> {
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

    pub(super) fn temperature_c(&self) -> Option<f64> {
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
            tournament: self.loaded.tournament,
            actor_id: self.loaded.actor_id.clone(),
        };
        s.environment_error().is_none().then_some(s)
    }

    /// The section to save, falling back to the loaded one while invalid.
    pub(super) fn to_section_or_loaded(&self) -> SimulationSection {
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
    pub(super) loaded: ReplaySection,
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
    pub(super) fn seconds(text: &str, loaded: f64) -> Option<f64> {
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
    pub(super) fn to_section_or_loaded(&self) -> ReplaySection {
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
    pub(super) loaded: BodyTrackingSection,
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
    pub(super) fn decimal(text: &str, loaded: f64) -> Option<f64> {
        if text == format_distance_value(loaded) {
            return Some(loaded);
        }
        text.trim().parse().ok()
    }

    /// Whether the defaults with `set` applied are valid: one field checked
    /// by [`BodyTrackingSection::error`], as the server checks it.
    pub(super) fn valid(set: impl FnOnce(&mut BodyTrackingSection)) -> bool {
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
    pub(super) fn to_section_or_loaded(&self) -> BodyTrackingSection {
        self.to_section().unwrap_or_else(|| self.loaded.clone())
    }
}

/// Hover text for a simulation mode in the settings dropdown.
pub(super) fn sim_mode_hover(mode: SimMode) -> &'static str {
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
