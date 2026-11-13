//! The Trackball card's settings: every setting of the trackball driver is a
//! field on the card, checked with the driver's own validation.

use std::collections::HashMap;

use flighthook::{TrackballClub, TrackballConfig, TrackballCurveSense, TrackballSection};

use super::{DeviceFormEntry, TEXT_EDIT_MARGIN, field_indent, field_label};
use crate::panels::devices;

/// A number setting of the trackball driver.
pub(crate) struct Number {
    /// The driver's key, which its refusals name (`tuning.power_gamma`).
    pub(crate) key: &'static str,
    label: &'static str,
    unit: &'static str,
    hover: &'static str,
    /// The whole numbers it takes, for a count of milliseconds or a limit;
    /// `None` for any number.
    whole: Option<(f64, f64)>,
    get: fn(&TrackballConfig) -> f64,
    set: fn(&mut TrackballConfig, f64),
}

// `f64::from` is not const; both are exact.
#[allow(clippy::cast_lossless)]
/// Whole numbers that fit a `u32`.
const U32: Option<(f64, f64)> = Some((0.0, u32::MAX as f64));
#[allow(clippy::cast_lossless)]
/// Whole numbers that fit an `i32`.
const I32: Option<(f64, f64)> = Some((i32::MIN as f64, i32::MAX as f64));

/// A speed as a share of the full-power speed.
const SHARE_OF_FULL: &str = "× full";
/// A length as a share of a full-power stroke.
const SHARE_OF_STROKE: &str = "× stroke";

// The setters below take numbers already checked against the field's range
// (`whole`), so the casts neither truncate nor lose a sign.

/// The device's numbers: its resolution and its report limit.
#[allow(clippy::cast_possible_truncation)]
pub(crate) const DEVICE: [Number; 2] = [
    Number {
        key: "cpi",
        label: "CPI:",
        unit: "counts/in",
        hover: "Counts per inch of ball travel the device reports.",
        whole: None,
        get: |d| d.config.cpi,
        set: |d, v| d.config.cpi = v,
    },
    Number {
        key: "report_limit",
        label: "Report limit:",
        unit: "",
        hover: "Largest movement one report can carry: 127 for 8-bit reports, \
                2047 for 12-bit, 32767 for 16-bit. A swing that hits it is \
                logged as saturated.",
        whole: I32,
        get: |d| f64::from(d.config.report_limit),
        set: |d, v| d.config.report_limit = v as i32,
    },
];

/// The tuning: how a swing becomes a shot.
pub(crate) const TUNING: [Number; 10] = [
    Number {
        key: "tuning.full_power_speed",
        label: "Full power speed:",
        unit: "in/s",
        hover: "Forward speed of the ball that counts as 100 % power. Every \
                swing detection threshold scales with it.",
        whole: None,
        get: |d| d.tuning.full_power_speed,
        set: |d, v| d.tuning.full_power_speed = v,
    },
    Number {
        key: "tuning.max_club_speed",
        label: "Max club speed:",
        unit: "mph",
        hover: "Driver club speed at full power; the other clubs scale it.",
        whole: None,
        get: |d| d.tuning.max_club_speed,
        set: |d, v| d.tuning.max_club_speed = v,
    },
    Number {
        key: "tuning.max_putt_speed",
        label: "Max putt speed:",
        unit: "mph",
        hover: "Putter speed at full power.",
        whole: None,
        get: |d| d.tuning.max_putt_speed,
        set: |d, v| d.tuning.max_putt_speed = v,
    },
    Number {
        key: "tuning.angle_gain",
        label: "Face gain:",
        unit: "°/°",
        hover: "Face angle per degree of push direction.",
        whole: None,
        get: |d| d.tuning.angle_gain,
        set: |d, v| d.tuning.angle_gain = v,
    },
    Number {
        key: "tuning.putt_angle_gain",
        label: "Putt line gain:",
        unit: "°/°",
        hover: "Start line of a putt per degree of push direction.",
        whole: None,
        get: |d| d.tuning.putt_angle_gain,
        set: |d, v| d.tuning.putt_angle_gain = v,
    },
    Number {
        key: "tuning.curve_gain",
        label: "Curve gain:",
        unit: "°/°",
        hover: "Face-to-path per degree between the backswing line and the push.",
        whole: None,
        get: |d| d.tuning.curve_gain,
        set: |d, v| d.tuning.curve_gain = v,
    },
    Number {
        key: "tuning.backswing_power",
        label: "Backswing power:",
        unit: "",
        hover: "Share of the power the backswing's length sets, 0 to 1. At 0 \
                the forward speed alone sets it.",
        whole: None,
        get: |d| d.tuning.backswing_power,
        set: |d, v| d.tuning.backswing_power = v,
    },
    Number {
        key: "tuning.power_gamma",
        label: "Power curve:",
        unit: "",
        hover: "Exponent of the power curve: 1 is linear, more than 1 takes \
                more speed for the same power.",
        whole: None,
        get: |d| d.tuning.power_gamma,
        set: |d, v| d.tuning.power_gamma = v,
    },
    Number {
        key: "tuning.face_max",
        label: "Max face angle:",
        unit: "°",
        hover: "Largest face angle a push makes.",
        whole: None,
        get: |d| d.tuning.face_max,
        set: |d, v| d.tuning.face_max = v,
    },
    Number {
        key: "tuning.face_to_path_max",
        label: "Max face-to-path:",
        unit: "°",
        hover: "Largest face-to-path a swing makes.",
        whole: None,
        get: |d| d.tuning.face_to_path_max,
        set: |d, v| d.tuning.face_to_path_max = v,
    },
];

/// How swings are found in the device's movement.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) const THRESHOLDS: [Number; 15] = [
    Number {
        key: "thresholds.speed_window_ms",
        label: "Speed window:",
        unit: "ms",
        hover: "Time the ball's speed is measured over.",
        whole: U32,
        get: |d| f64::from(d.config.thresholds.speed_window_ms),
        set: |d, v| d.config.thresholds.speed_window_ms = v as u32,
    },
    Number {
        key: "thresholds.gap_ms",
        label: "Gap:",
        unit: "ms",
        hover: "Silence after which the ball's movement counts as a new one.",
        whole: U32,
        get: |d| f64::from(d.config.thresholds.gap_ms),
        set: |d, v| d.config.thresholds.gap_ms = v as u32,
    },
    Number {
        key: "thresholds.v_start",
        label: "Start speed:",
        unit: SHARE_OF_FULL,
        hover: "Speed that starts a backswing, as a share of the full power speed.",
        whole: None,
        get: |d| d.config.thresholds.v_start,
        set: |d, v| d.config.thresholds.v_start = v,
    },
    Number {
        key: "thresholds.v_stop",
        label: "Stop speed:",
        unit: SHARE_OF_FULL,
        hover: "Speed below which the ball counts as stopped, as a share of the \
                full power speed. Below the start speed.",
        whole: None,
        get: |d| d.config.thresholds.v_stop,
        set: |d, v| d.config.thresholds.v_stop = v,
    },
    Number {
        key: "thresholds.v_min_swing",
        label: "Swing speed:",
        unit: SHARE_OF_FULL,
        hover: "Speed a push must reach to be a swing, as a share of the full \
                power speed.",
        whole: None,
        get: |d| d.config.thresholds.v_min_swing,
        set: |d, v| d.config.thresholds.v_min_swing = v,
    },
    Number {
        key: "thresholds.dwell_min_ms",
        label: "Stop time:",
        unit: "ms",
        hover: "How long the ball must stay below the stop speed to count as \
                stopped.",
        whole: U32,
        get: |d| f64::from(d.config.thresholds.dwell_min_ms),
        set: |d, v| d.config.thresholds.dwell_min_ms = v as u32,
    },
    Number {
        key: "thresholds.back_min",
        label: "Shortest backswing:",
        unit: SHARE_OF_STROKE,
        hover: "As a share of a full stroke, the distance the ball travels in \
                100 ms at full power.",
        whole: None,
        get: |d| d.config.thresholds.back_min,
        set: |d, v| d.config.thresholds.back_min = v,
    },
    Number {
        key: "thresholds.full_cock",
        label: "Full backswing:",
        unit: SHARE_OF_STROKE,
        hover: "Backswing length that counts as fully cocked, as a share of a \
                full stroke.",
        whole: None,
        get: |d| d.config.thresholds.full_cock,
        set: |d, v| d.config.thresholds.full_cock = v,
    },
    Number {
        key: "thresholds.fwd_min",
        label: "Shortest push:",
        unit: SHARE_OF_STROKE,
        hover: "Forward travel that makes a push a swing, as a share of a full \
                stroke.",
        whole: None,
        get: |d| d.config.thresholds.fwd_min,
        set: |d, v| d.config.thresholds.fwd_min = v,
    },
    Number {
        key: "thresholds.back_max_s",
        label: "Longest backswing:",
        unit: "s",
        hover: "A backswing still moving after this long is dropped.",
        whole: None,
        get: |d| d.config.thresholds.back_max_s,
        set: |d, v| d.config.thresholds.back_max_s = v,
    },
    Number {
        key: "thresholds.coil_timeout_s",
        label: "Wait at the top:",
        unit: "s",
        hover: "How long a finished backswing waits for the push.",
        whole: None,
        get: |d| d.config.thresholds.coil_timeout_s,
        set: |d, v| d.config.thresholds.coil_timeout_s = v,
    },
    Number {
        key: "thresholds.fwd_window_ms",
        label: "Longest push:",
        unit: "ms",
        hover: "Time the push is measured over at most.",
        whole: U32,
        get: |d| f64::from(d.config.thresholds.fwd_window_ms),
        set: |d, v| d.config.thresholds.fwd_window_ms = v as u32,
    },
    Number {
        key: "thresholds.idle_settle_ms",
        label: "Settle time:",
        unit: "ms",
        hover: "Quiet time after a shot before the next backswing can start.",
        whole: U32,
        get: |d| f64::from(d.config.thresholds.idle_settle_ms),
        set: |d, v| d.config.thresholds.idle_settle_ms = v as u32,
    },
    Number {
        key: "thresholds.cooldown_max_s",
        label: "Longest cooldown:",
        unit: "s",
        hover: "Longest wait after a shot while the ball still rolls.",
        whole: None,
        get: |d| d.config.thresholds.cooldown_max_s,
        set: |d, v| d.config.thresholds.cooldown_max_s = v,
    },
    Number {
        key: "thresholds.pull_cone_deg",
        label: "Pull cone:",
        unit: "°",
        hover: "Widest angle from straight back that still counts as a pull, \
                1 to 180.",
        whole: None,
        get: |d| d.config.thresholds.pull_cone_deg,
        set: |d, v| d.config.thresholds.pull_cone_deg = v,
    },
];

/// Every number setting, in card order.
pub(crate) fn numbers() -> impl Iterator<Item = &'static Number> {
    DEVICE.iter().chain(TUNING.iter()).chain(THRESHOLDS.iter())
}

/// A number as a field shows it.
fn text(v: f64) -> String {
    format!("{v}")
}

/// The number `typed` in field `n`: blank is the driver's default, else a
/// finite number (whole and in range for a whole-number setting). `Err`
/// says why it is not one.
fn value(n: &Number, typed: &str) -> Result<f64, &'static str> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Ok((n.get)(&TrackballConfig::default()));
    }
    let v = typed
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or("not a number")?;
    match n.whole {
        Some((lo, hi)) if v.fract() != 0.0 || !(lo..=hi).contains(&v) => Err("not a whole number"),
        _ => Ok(v),
    }
}

/// The Trackball card's fields.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackballForm {
    /// The driver's settings as loaded (a new card: a new card's). A field
    /// still showing its loaded value keeps it, so a card saved unchanged
    /// writes its section as it was.
    pub(crate) loaded: TrackballConfig,
    pub(crate) device: String,
    pub(crate) club: TrackballClub,
    pub(crate) curve_sense: TrackballCurveSense,
    pub(crate) mirror: bool,
    /// The number fields as typed, by key.
    pub(crate) numbers: HashMap<&'static str, String>,
}

impl Default for TrackballForm {
    /// A new card's fields.
    fn default() -> Self {
        Self::new(&TrackballSection::default().driver)
    }
}

impl TrackballForm {
    /// The fields of settings `loaded`.
    pub(crate) fn new(loaded: &TrackballConfig) -> Self {
        Self {
            loaded: loaded.clone(),
            device: loaded.device.clone().unwrap_or_default(),
            club: loaded.club,
            curve_sense: loaded.tuning.curve_sense,
            mirror: loaded.tuning.mirror_for_left_hand,
            numbers: numbers().map(|n| (n.key, text((n.get)(loaded)))).collect(),
        }
    }

    fn typed(&self, n: &Number) -> &str {
        self.numbers.get(n.key).map_or("", String::as_str)
    }

    /// The driver's settings from the fields, over the settings as loaded.
    /// A number that is not one keeps its loaded value
    /// ([`unparsable`](Self::unparsable) refuses it).
    pub(crate) fn config(&self) -> TrackballConfig {
        let mut out = self.loaded.clone();
        let device = self.device.trim();
        if device != self.loaded.device.as_deref().unwrap_or_default().trim() {
            out.device = (!device.is_empty()).then(|| device.to_owned());
        }
        out.club = self.club;
        out.tuning.curve_sense = self.curve_sense;
        out.tuning.mirror_for_left_hand = self.mirror;
        for n in numbers() {
            let typed = self.typed(n).trim();
            if typed == text((n.get)(&self.loaded)) {
                continue;
            }
            if let Ok(v) = value(n, typed) {
                (n.set)(&mut out, v);
            }
        }
        out
    }

    /// The first number field that holds no number, as `"<key>: <why>"`.
    pub(crate) fn unparsable(&self) -> Option<String> {
        numbers().find_map(|n| {
            value(n, self.typed(n))
                .err()
                .map(|why| format!("{}: {why}", n.key))
        })
    }
}

/// A number field: label, text, unit; marked `trackball:<key>:<card>`.
fn number_field(ui: &mut egui::Ui, dev: &mut DeviceFormEntry, n: &Number, field_width: f32) {
    let card = format!("{}.{}", dev.monitor_type, dev.id);
    let form = dev.trackball_form();
    let hint = text((n.get)(&TrackballConfig::default()));
    let mut changed = false;
    ui.horizontal(|ui| {
        field_label(ui, n.label).on_hover_text(n.hover);
        let typed = form.numbers.entry(n.key).or_default();
        let field = ui
            .add(
                egui::TextEdit::singleline(typed)
                    .desired_width(field_width - TEXT_EDIT_MARGIN)
                    .hint_text(hint),
            )
            .on_hover_text(n.hover);
        devices::mark(ui, || format!("trackball:{}:{card}", n.key), &field);
        changed = field.changed();
        if !n.unit.is_empty() {
            ui.label(n.unit);
        }
    });
    if changed {
        dev.dirty = true;
    }
}

/// A dropdown of `options` (value, label) for `current`.
fn choice<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    salt: String,
    width: f32,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    let shown = options
        .iter()
        .find(|(v, _)| v == current)
        .map_or("", |(_, label)| label);
    let mut changed = false;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(shown)
        .width(width)
        .show_ui(ui, |ui| {
            for (v, label) in options {
                if ui.selectable_label(v == current, *label).clicked() {
                    *current = *v;
                    changed = true;
                }
            }
        });
    changed
}

/// The Trackball card's fields: the device, the club it starts with, the
/// numbers of the device, the tuning, then the swing
/// detection thresholds in a section of their own.
pub(super) fn fields(ui: &mut egui::Ui, dev: &mut DeviceFormEntry, field_width: f32, wide: f32) {
    let id = dev.id.clone();
    ui.horizontal(|ui| {
        field_label(ui, "Device:").on_hover_text(
            "The trackball or mouse to read: its /dev/input/by-id name, a /dev/input path, \
             or the name it reports. Leave blank to use the only one; with more than one, \
             name it here.",
        );
        let field = ui.add(
            egui::TextEdit::singleline(&mut dev.trackball_form().device)
                .desired_width(field_width - TEXT_EDIT_MARGIN)
                .hint_text("the only trackball or mouse"),
        );
        devices::mark(ui, || format!("trackball:device:trackball.{id}"), &field);
        if field.changed() {
            dev.dirty = true;
        }
    });

    let clubs: Vec<(TrackballClub, &str)> =
        TrackballClub::ALL.iter().map(|c| (*c, c.code())).collect();
    let mut changed = false;
    ui.horizontal(|ui| {
        field_label(ui, "Starting club:")
            .on_hover_text("The club in play until the game names one.");
        changed |= choice(
            ui,
            format!("trackball_club_{id}"),
            field_width,
            &mut dev.trackball_form().club,
            &clubs,
        );
    });
    for n in &DEVICE {
        number_field(ui, dev, n, field_width);
    }
    for n in &TUNING {
        number_field(ui, dev, n, field_width);
    }
    ui.horizontal(|ui| {
        field_label(ui, "Curve:").on_hover_text(
            "Arcade: the ball starts where you push and curves toward the line you \
             pulled back from. Physical: the backswing line is the club path, and the \
             ball curves away from it.",
        );
        changed |= choice(
            ui,
            format!("trackball_curve_{id}"),
            wide,
            &mut dev.trackball_form().curve_sense,
            &[
                (TrackballCurveSense::Arcade, "Arcade"),
                (TrackballCurveSense::Physical, "Physical"),
            ],
        );
    });
    ui.horizontal(|ui| {
        field_indent(ui);
        changed |= ui
            .checkbox(
                &mut dev.trackball_form().mirror,
                "Mirror for left-handed players",
            )
            .on_hover_text("Mirror the stroke directions while the player is left-handed.")
            .changed();
    });
    if changed {
        dev.dirty = true;
    }
    egui::CollapsingHeader::new("Swing detection")
        .id_salt(format!("trackball_thresholds_{id}"))
        .show(ui, |ui| {
            for n in &THRESHOLDS {
                number_field(ui, dev, n, field_width);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_number_reads_and_writes_its_own_setting() {
        let mut d = TrackballConfig::default();
        let mut seen = std::collections::BTreeSet::new();
        for (i, n) in numbers().enumerate() {
            assert!(seen.insert(n.key), "{} twice", n.key);
            let v = 3.0 + f64::from(u8::try_from(i).unwrap());
            (n.set)(&mut d, v);
            assert_eq!((n.get)(&d), v, "{}", n.key);
        }
        // Each wrote a field of its own: none was overwritten by another.
        for (i, n) in numbers().enumerate() {
            assert_eq!(
                (n.get)(&d),
                3.0 + f64::from(u8::try_from(i).unwrap()),
                "{}",
                n.key
            );
        }
        assert_eq!(seen.len(), 27);
    }
}
