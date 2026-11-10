//! Body tracking grades (alpha): a camera's swing numbers placed against
//! bands from published tour averages, as `GET
//! /api/shots/{shot_id}/grades/{camera}` serves them.
//!
//! A band is a center ± one SD for a metric at a key position, a club class
//! and the player's sex. Four impact quantities (hip turn, shoulder turn,
//! shoulder tilt, hip sway) depend on the shot shape: they carry a fade band
//! and a draw band, each the center shifted by a small offset. Where a value
//! sits is its [`Fit`].

use serde::{Deserialize, Serialize};

use crate::{ClubClass, Handedness, Sex};

/// A camera's swing numbers graded against each reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BodyGrades {
    pub shot_id: String,
    /// The camera's actor ID (`camera.0`).
    pub camera: String,
    /// The class of the club the shot was hit with; `None` for a putter or
    /// an unknown club (nothing is graded then).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club_class: Option<ClubClass>,
    /// The player's sex the bands are for.
    pub sex: Sex,
    /// The golfer's hand at the shot. Every metric is measured relative to
    /// the target, so the grades read the same for both hands.
    pub hand: Handedness,
    /// The grades against each reference, in [`ReferenceKind`] order.
    #[serde(default)]
    pub references: Vec<ReferenceGrades>,
}

impl BodyGrades {
    /// The grades against `reference`, if served.
    #[must_use]
    pub fn reference(&self, reference: ReferenceKind) -> Option<&ReferenceGrades> {
        self.references.iter().find(|r| r.reference == reference)
    }
}

/// Which population the bands describe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReferenceKind {
    /// Tour players. The default.
    #[default]
    Tour,
    /// Mid-handicap amateur golfers.
    Amateur,
}

impl ReferenceKind {
    /// Every reference, in display order.
    pub const ALL: [Self; 2] = [Self::Tour, Self::Amateur];

    /// Stable key.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Tour => "tour",
            Self::Amateur => "amateur",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tour => "Tour",
            Self::Amateur => "Amateur",
        }
    }
}

/// The grades against one reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceGrades {
    pub reference: ReferenceKind,
    /// Every metric at every key position with a band.
    #[serde(default)]
    pub grades: Vec<MetricGrade>,
    /// The tempo ratio and the downswing time, where they have bands.
    #[serde(default)]
    pub tempo: Vec<MetricGrade>,
}

impl ReferenceGrades {
    /// The grade of metric `id` at `position` (a [`crate::SwingPosition`]
    /// key such as `"impact"`, or `"swing"` for tempo), if graded.
    #[must_use]
    pub fn grade(&self, id: &str, position: &str) -> Option<&MetricGrade> {
        self.grades
            .iter()
            .chain(&self.tempo)
            .find(|g| g.id == id && g.position == position)
    }
}

/// One graded value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricGrade {
    /// The metric's ID (`hip_turn`), or `tempo_ratio` / `downswing_time`.
    pub id: String,
    /// The key position (`address`, `arm_parallel_back`, `top`,
    /// `arm_parallel_down`, `impact`, `finish`), or `swing` for tempo.
    pub position: String,
    /// The value in the band's unit (`degrees`, `inches`, `ratio` or
    /// `seconds`), signed as the band: turns + open, shifts + toward the
    /// target, the ball or up.
    pub value: f64,
    pub unit: String,
    /// The band: center − SD.
    pub low: f64,
    /// The band: center + SD.
    pub high: f64,
    /// The fade band, `[low, high]`: shape-linked quantities only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade: Option<[f64; 2]>,
    /// The draw band, `[low, high]`: shape-linked quantities only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw: Option<[f64; 2]>,
    /// The distance from the band's center in SDs, signed.
    pub z: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_fade: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_draw: Option<f64>,
    pub fit: Fit,
    pub side: Side,
    /// The direction word of the side the value is off on (`open`,
    /// `closed`, `toward`, `away`, `up`, `down`, `toward_ball`, `back`,
    /// `more`, `less`); `None` on the center.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// The words for a value off the band, after the metric's name: "more
    /// open: fade side", "more open than tour: fade or slice pattern when
    /// the arms lag". `None` for [`Fit::Both`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word: Option<String>,
    /// The band's SD is borrowed from the nearest published spread.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub proxy_sd: bool,
    /// A distance put into inches with an assumed height (no height on the
    /// Player card).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub assumed_scale: bool,
    /// A wedge graded against the mid-iron band.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub club_fallback: bool,
    /// A woman graded against the men's band.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sex_fallback: bool,
    /// The metric's keypoint confidence (1 for tempo).
    pub confidence: f32,
}

/// Where a value sits against its band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Inside the band, or inside both the fade and the draw band.
    Both,
    /// Inside the fade band only.
    Fade,
    /// Inside the draw band only.
    Draw,
    /// Outside, within 2 SD of the nearer band.
    Near,
    /// Further out.
    Far,
}

/// Which side of the band's center a value is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Low,
    High,
    /// Inside the band (inside both sub-bands of a shape-linked one).
    On,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grades_round_trip_and_omit_what_is_unset() {
        let grade = MetricGrade {
            id: "hip_turn".into(),
            position: "impact".into(),
            value: 44.0,
            unit: "degrees".into(),
            low: 33.0,
            high: 43.0,
            fade: Some([36.0, 46.0]),
            draw: Some([30.0, 40.0]),
            z: 1.2,
            z_fade: Some(0.6),
            z_draw: Some(1.8),
            fit: Fit::Fade,
            side: Side::High,
            direction: Some("open".into()),
            word: Some("more open: fade side".into()),
            proxy_sd: true,
            assumed_scale: false,
            club_fallback: false,
            sex_fallback: false,
            confidence: 0.9,
        };
        let grades = BodyGrades {
            shot_id: "abc".into(),
            camera: "camera.0".into(),
            club_class: Some(ClubClass::Driver),
            sex: Sex::Male,
            hand: Handedness::Right,
            references: vec![ReferenceGrades {
                reference: ReferenceKind::Tour,
                grades: vec![grade.clone()],
                tempo: Vec::new(),
            }],
        };
        let json = serde_json::to_string(&grades).unwrap();
        assert!(json.contains(r#""fit":"fade","side":"high""#), "{json}");
        assert!(json.contains(r#""proxy_sd":true"#));
        assert!(!json.contains("assumed_scale"), "{json}");
        assert_eq!(serde_json::from_str::<BodyGrades>(&json).unwrap(), grades);
        let tour = grades.reference(ReferenceKind::Tour).unwrap();
        assert_eq!(tour.grade("hip_turn", "impact"), Some(&grade));
        assert!(tour.grade("hip_turn", "top").is_none());
        assert!(grades.reference(ReferenceKind::Amateur).is_none());
        assert_eq!(
            ReferenceKind::ALL.map(ReferenceKind::label),
            ["Tour", "Amateur"]
        );
    }
}
