//! The `[replay]`, `[body_tracking]` and `[camera.N]` sections, with their limits and validation
//! (latency, rotation, yaw/pitch).

use super::*;

/// Settings for the swing replay clips the cameras record (beta).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplaySection {
    /// Clips kept on disk, newest first; older ones are deleted. Counts
    /// clips, not shots: a swing two launch monitors saw is one clip, and a
    /// trigger that read nothing records one too. Defaults to 20.
    #[serde(default = "default_keep_shots")]
    pub keep_shots: u32,
    /// Seconds recorded before the estimated impact. Defaults to 4.
    #[serde(default = "default_pre_roll_s")]
    pub pre_roll_s: f64,
    /// Seconds recorded after the launch monitor reports the shot. Defaults
    /// to 1.5.
    #[serde(default = "default_post_roll_s")]
    pub post_roll_s: f64,
    /// Seconds before the estimated impact where a replay opens, and where it
    /// restarts once it played to the end. Defaults to 2.
    #[serde(default = "default_start_before_impact_s")]
    pub start_before_impact_s: f64,
    /// Folder the clips are written to. Defaults to `flighthook/clips` in
    /// the user's local data folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_dir: Option<String>,
    /// The Shots tab's Cards view shows camera cards that loop the shown
    /// shot's clip. Off, the card row has no camera cards; clips are still
    /// recorded and replayed. Defaults to true.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub shot_cards: bool,
}

pub(super) fn default_keep_shots() -> u32 {
    ReplaySection::DEFAULT_KEEP_SHOTS
}

pub(super) fn default_pre_roll_s() -> f64 {
    ReplaySection::DEFAULT_PRE_ROLL_S
}

pub(super) fn default_post_roll_s() -> f64 {
    ReplaySection::DEFAULT_POST_ROLL_S
}

pub(super) fn default_start_before_impact_s() -> f64 {
    ReplaySection::DEFAULT_START_BEFORE_IMPACT_S
}

impl Default for ReplaySection {
    fn default() -> Self {
        Self {
            keep_shots: Self::DEFAULT_KEEP_SHOTS,
            pre_roll_s: Self::DEFAULT_PRE_ROLL_S,
            post_roll_s: Self::DEFAULT_POST_ROLL_S,
            start_before_impact_s: Self::DEFAULT_START_BEFORE_IMPACT_S,
            clip_dir: None,
            shot_cards: true,
        }
    }
}

impl ReplaySection {
    /// Default clips kept.
    pub const DEFAULT_KEEP_SHOTS: u32 = 20;
    /// Most clips kept.
    pub const MAX_KEEP_SHOTS: u32 = 1000;
    /// Default seconds before the estimated impact.
    pub const DEFAULT_PRE_ROLL_S: f64 = 4.0;
    /// Default seconds after the shot is reported.
    pub const DEFAULT_POST_ROLL_S: f64 = 1.5;
    /// Longest pre-roll or post-roll, seconds.
    pub const MAX_ROLL_S: f64 = 10.0;
    /// Default seconds before the estimated impact where a replay starts: a
    /// backswing and downswing take about 1.2 to 1.5 s, plus a buffer.
    pub const DEFAULT_START_BEFORE_IMPACT_S: f64 = 2.0;
    /// Longest start before the impact, seconds.
    pub const MAX_START_BEFORE_IMPACT_S: f64 = 10.0;

    /// True when every setting is at its default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Why a setting is out of range, or `None` when all are valid:
    /// `keep_shots` 1 to [`MAX_KEEP_SHOTS`](Self::MAX_KEEP_SHOTS), each roll
    /// 0 to [`MAX_ROLL_S`](Self::MAX_ROLL_S) seconds, `start_before_impact_s`
    /// 0 to [`MAX_START_BEFORE_IMPACT_S`](Self::MAX_START_BEFORE_IMPACT_S)
    /// seconds.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        if !(1..=Self::MAX_KEEP_SHOTS).contains(&self.keep_shots) {
            return Some(format!(
                "keep_shots {} is outside 1 to {}",
                self.keep_shots,
                Self::MAX_KEEP_SHOTS
            ));
        }
        for (name, v, max) in [
            ("pre_roll_s", self.pre_roll_s, Self::MAX_ROLL_S),
            ("post_roll_s", self.post_roll_s, Self::MAX_ROLL_S),
            (
                "start_before_impact_s",
                self.start_before_impact_s,
                Self::MAX_START_BEFORE_IMPACT_S,
            ),
        ] {
            if !v.is_finite() || !(0.0..=max).contains(&v) {
                return Some(format!("{name} {v} is outside 0 to {max} s"));
            }
        }
        None
    }
}

/// The body tracking pose model (alpha).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoseModel {
    /// RTMPose-m: the more accurate, about twice the work of `-s`.
    #[default]
    #[serde(rename = "rtmpose-m")]
    RtmposeM,
    /// RTMPose-s: lighter and faster.
    #[serde(rename = "rtmpose-s")]
    RtmposeS,
}

impl PoseModel {
    /// Stable key, as the config file holds it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::RtmposeM => "rtmpose-m",
            Self::RtmposeS => "rtmpose-s",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::RtmposeM => "RTMPose-m",
            Self::RtmposeS => "RTMPose-s",
        }
    }

    /// Every model, in display order.
    #[must_use]
    pub fn all() -> [Self; 2] {
        [Self::RtmposeM, Self::RtmposeS]
    }
}

/// When body tracking runs (alpha).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyTrackingMode {
    /// No body tracking: the cameras record and replay clips only. The
    /// default.
    #[default]
    Off,
    /// A clip is tracked the first time it is viewed, and the result kept
    /// beside it. No live tracking.
    Historical,
    /// The live views are tracked while watched, and every clip right
    /// after it is recorded.
    Live,
}

impl BodyTrackingMode {
    /// True for the default mode ([`BodyTrackingMode::Off`]).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Stable key, as the config file holds it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Historical => "historical",
            Self::Live => "live",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Historical => "Historical",
            Self::Live => "Live",
        }
    }

    /// Every mode, in display order.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Off, Self::Historical, Self::Live]
    }

    /// Whether body tracking runs at all (not [`BodyTrackingMode::Off`]).
    #[must_use]
    pub fn is_on(self) -> bool {
        self != Self::Off
    }
}

/// Body tracking on the cameras' pictures (alpha): a skeleton drawn over
/// the live views and over the shots' clips, and the swing analysis of
/// each clip.
///
/// Every field is read live; a change never restarts a camera. A field at
/// its default is left out of the file and the API. A section with
/// `enabled` and no `mode` reads `enabled = false` as
/// [`BodyTrackingMode::Off`] and `enabled = true` as
/// [`BodyTrackingMode::Historical`]; only `mode` is written. The golfer's
/// height is [`PlayerSection::height`]; a `golfer_height` in the section is
/// read and moved there when the config loads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "BodyTrackingRaw")]
pub struct BodyTrackingSection {
    /// When body tracking runs. Defaults to [`BodyTrackingMode::Off`].
    #[serde(skip_serializing_if = "BodyTrackingMode::is_default")]
    pub mode: BodyTrackingMode,
    /// The pose model. Defaults to RTMPose-m.
    #[serde(default, skip_serializing_if = "is_default_pose_model")]
    pub model: PoseModel,
    /// Keypoints scoring below this are not drawn, 0 to 1. Defaults to 0.3.
    #[serde(
        default = "default_min_score",
        skip_serializing_if = "is_default_min_score"
    )]
    pub min_score: f64,
    /// Live smoothing: the One Euro filter's cutoff at rest in Hz, 0 (off)
    /// or [`MIN_SMOOTHING_HZ`](Self::MIN_SMOOTHING_HZ) to
    /// [`MAX_SMOOTHING_HZ`](Self::MAX_SMOOTHING_HZ). Lower is smoother and
    /// lags more. Defaults to 1 Hz. Live views only; recorded swings keep the
    /// model's raw keypoints.
    #[serde(
        default = "default_smoothing",
        skip_serializing_if = "is_default_smoothing"
    )]
    pub smoothing: f64,
    /// The live view looks for the golfer afresh at least every this many
    /// frames, 1 to [`MAX_DETECT_EVERY`](Self::MAX_DETECT_EVERY) (and
    /// whenever it loses them); in between it follows them from the last
    /// frame. Defaults to 10.
    #[serde(
        default = "default_detect_every",
        skip_serializing_if = "is_default_detect_every"
    )]
    pub detect_every: u32,
    /// The skeleton is drawn over the live views, the shots' clips and the
    /// replay. Defaults to true.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub show_skeleton: bool,
    /// A `golfer_height` read from the section, in centimeters, until the
    /// config moves it to [`PlayerSection::height`]. Never written, never
    /// compared.
    #[serde(skip)]
    #[doc(hidden)]
    pub legacy_golfer_height: Option<f64>,
}

impl PartialEq for BodyTrackingSection {
    fn eq(&self, other: &Self) -> bool {
        self.mode == other.mode
            && self.model == other.model
            && self.min_score == other.min_score
            && self.smoothing == other.smoothing
            && self.detect_every == other.detect_every
            && self.show_skeleton == other.show_skeleton
    }
}

/// `[body_tracking]` as written: `mode`, or the `enabled` switch of a
/// section from before the modes.
#[derive(Deserialize)]
pub(super) struct BodyTrackingRaw {
    #[serde(default)]
    pub(super) mode: Option<BodyTrackingMode>,
    #[serde(default)]
    pub(super) enabled: Option<bool>,
    #[serde(default)]
    pub(super) model: PoseModel,
    #[serde(default = "default_min_score")]
    pub(super) min_score: f64,
    #[serde(default = "default_smoothing")]
    pub(super) smoothing: f64,
    #[serde(default = "default_detect_every")]
    pub(super) detect_every: u32,
    #[serde(default)]
    pub(super) golfer_height: Option<f64>,
    #[serde(default = "default_true")]
    pub(super) show_skeleton: bool,
}

impl From<BodyTrackingRaw> for BodyTrackingSection {
    fn from(raw: BodyTrackingRaw) -> Self {
        let mode = raw.mode.unwrap_or(match raw.enabled {
            Some(true) => BodyTrackingMode::Historical,
            Some(false) | None => BodyTrackingMode::Off,
        });
        Self {
            mode,
            model: raw.model,
            min_score: raw.min_score,
            smoothing: raw.smoothing,
            detect_every: raw.detect_every,
            show_skeleton: raw.show_skeleton,
            legacy_golfer_height: raw.golfer_height,
        }
    }
}

pub(super) fn is_default_pose_model(v: &PoseModel) -> bool {
    *v == PoseModel::default()
}

pub(super) fn default_min_score() -> f64 {
    BodyTrackingSection::DEFAULT_MIN_SCORE
}

pub(super) fn is_default_min_score(v: &f64) -> bool {
    v.to_bits() == BodyTrackingSection::DEFAULT_MIN_SCORE.to_bits()
}

pub(super) fn default_smoothing() -> f64 {
    BodyTrackingSection::DEFAULT_SMOOTHING_HZ
}

pub(super) fn is_default_smoothing(v: &f64) -> bool {
    v.to_bits() == BodyTrackingSection::DEFAULT_SMOOTHING_HZ.to_bits()
}

pub(super) fn default_detect_every() -> u32 {
    BodyTrackingSection::DEFAULT_DETECT_EVERY
}

pub(super) fn is_default_detect_every(v: &u32) -> bool {
    *v == BodyTrackingSection::DEFAULT_DETECT_EVERY
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn is_true(value: &bool) -> bool {
    *value
}

impl Default for BodyTrackingSection {
    fn default() -> Self {
        Self {
            mode: BodyTrackingMode::default(),
            model: PoseModel::default(),
            min_score: Self::DEFAULT_MIN_SCORE,
            smoothing: Self::DEFAULT_SMOOTHING_HZ,
            detect_every: Self::DEFAULT_DETECT_EVERY,
            show_skeleton: true,
            legacy_golfer_height: None,
        }
    }
}

impl BodyTrackingSection {
    /// Default keypoint drawing threshold.
    pub const DEFAULT_MIN_SCORE: f64 = 0.3;
    /// Default live smoothing, Hz.
    pub const DEFAULT_SMOOTHING_HZ: f64 = 1.0;
    /// Lowest live smoothing cutoff other than off, Hz: the smoothest.
    pub const MIN_SMOOTHING_HZ: f64 = 0.1;
    /// Highest live smoothing cutoff, Hz: the least smoothing.
    pub const MAX_SMOOTHING_HZ: f64 = 10.0;
    /// Default frames between two looks for the golfer.
    pub const DEFAULT_DETECT_EVERY: u32 = 10;
    /// Most frames between two looks for the golfer.
    pub const MAX_DETECT_EVERY: u32 = 60;

    /// True when every setting is at its default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Why a setting is out of range, or `None` when all are valid:
    /// `min_score` 0 to 1, `smoothing` 0 or
    /// [`MIN_SMOOTHING_HZ`](Self::MIN_SMOOTHING_HZ) to
    /// [`MAX_SMOOTHING_HZ`](Self::MAX_SMOOTHING_HZ), `detect_every` 1 to
    /// [`MAX_DETECT_EVERY`](Self::MAX_DETECT_EVERY).
    #[must_use]
    pub fn error(&self) -> Option<String> {
        if !self.min_score.is_finite() || !(0.0..=1.0).contains(&self.min_score) {
            return Some(format!("min_score {} is outside 0 to 1", self.min_score));
        }
        if !smoothing_valid(self.smoothing) {
            return Some(format!(
                "smoothing {} is neither 0 (off) nor {} to {} Hz",
                self.smoothing,
                Self::MIN_SMOOTHING_HZ,
                Self::MAX_SMOOTHING_HZ
            ));
        }
        if !(1..=Self::MAX_DETECT_EVERY).contains(&self.detect_every) {
            return Some(format!(
                "detect_every {} is outside 1 to {}",
                self.detect_every,
                Self::MAX_DETECT_EVERY
            ));
        }
        None
    }

    /// The settings in effect: every value out of range replaced by its
    /// default.
    #[must_use]
    pub fn in_effect(&self) -> Self {
        let defaults = Self::default();
        Self {
            mode: self.mode,
            model: self.model,
            min_score: if self.min_score.is_finite() && (0.0..=1.0).contains(&self.min_score) {
                self.min_score
            } else {
                defaults.min_score
            },
            smoothing: if smoothing_valid(self.smoothing) {
                self.smoothing
            } else {
                defaults.smoothing
            },
            detect_every: if (1..=Self::MAX_DETECT_EVERY).contains(&self.detect_every) {
                self.detect_every
            } else {
                defaults.detect_every
            },
            show_skeleton: self.show_skeleton,
            legacy_golfer_height: None,
        }
    }
}

/// Whether `hz` is an acceptable live smoothing: 0 (off), or finite and
/// from [`BodyTrackingSection::MIN_SMOOTHING_HZ`] to
/// [`BodyTrackingSection::MAX_SMOOTHING_HZ`].
pub(super) fn smoothing_valid(hz: f64) -> bool {
    hz == 0.0
        || (hz.is_finite()
            && (BodyTrackingSection::MIN_SMOOTHING_HZ..=BodyTrackingSection::MAX_SMOOTHING_HZ)
                .contains(&hz))
}

/// Where a camera stands, as seen from the golfer's stance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraPosition {
    /// Looking down the target line from behind the golfer's hands.
    TargetLine,
    /// Facing the golfer's chest. The default.
    #[default]
    GolferFront,
    /// Looking at the golfer's back.
    GolferBack,
    /// Above the golfer, looking down.
    Overhead,
}

impl CameraPosition {
    /// Stable key used by the settings UI and config file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::TargetLine => "target_line",
            Self::GolferFront => "golfer_front",
            Self::GolferBack => "golfer_back",
            Self::Overhead => "overhead",
        }
    }

    /// User-facing label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::TargetLine => "Target Line",
            Self::GolferFront => "Golfer Front",
            Self::GolferBack => "Golfer Back",
            Self::Overhead => "Overhead",
        }
    }

    /// Every position, in display order.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [
            Self::TargetLine,
            Self::GolferFront,
            Self::GolferBack,
            Self::Overhead,
        ]
    }
}

/// A swing camera instance (beta): records a clip around every shot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The camera's stable id, from the camera list (`GET /api/cameras`):
    /// it survives replugging into the same USB port and reboots.
    /// [`CameraSection::TEST_PATTERN`] is a generated test picture.
    #[serde(default)]
    pub camera: String,
    /// The camera's own name when it was picked, for display, and to find
    /// the camera when its id is not connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_name: Option<String>,
    /// Where the camera stands.
    #[serde(default)]
    pub position: CameraPosition,
    /// Video mode: `auto` (the highest frame rate, then the largest size)
    /// or `WIDTHxHEIGHT@FPS`, e.g. `1280x720@120`.
    #[serde(default = "default_video_mode")]
    pub mode: String,
    /// Seconds a frame is old when it arrives, 0 to [`MAX_LATENCY_S`]: a
    /// camera that delivers late, such as a buffered network stream. Every
    /// frame is dated that much earlier than its arrival. Absent is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_s: Option<f64>,
    /// Quarter turns applied to the camera's frames before body tracking
    /// and display: 0, 90, 180 or 270 degrees clockwise. Clips are stored as
    /// the camera sent them. Defaults to 0.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub rotation_deg: u16,
    /// How far the camera points off square, degrees, -[`MAX_CAMERA_ANGLE_DEG`]
    /// to [`MAX_CAMERA_ANGLE_DEG`]: positive when it points to the right of
    /// the target line for a right-handed golfer. Absent is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw_deg: Option<f64>,
    /// The camera's tilt down from level, degrees, -[`MAX_CAMERA_ANGLE_DEG`]
    /// to [`MAX_CAMERA_ANGLE_DEG`]. Absent is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch_deg: Option<f64>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
pub(super) fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

pub(super) fn default_video_mode() -> String {
    CameraSection::AUTO_MODE.to_owned()
}

impl CameraSection {
    /// The [`camera`](Self::camera) id of the generated test picture.
    pub const TEST_PATTERN: &'static str = "test-pattern";
    /// The automatic [`mode`](Self::mode).
    pub const AUTO_MODE: &'static str = "auto";
}

impl Default for CameraSection {
    fn default() -> Self {
        Self {
            name: "Camera".into(),
            enabled: true,
            camera: String::new(),
            camera_name: None,
            position: CameraPosition::default(),
            mode: default_video_mode(),
            latency_s: None,
            rotation_deg: 0,
            yaw_deg: None,
            pitch_deg: None,
        }
    }
}

/// Longest camera `latency_s`, seconds.
pub const MAX_LATENCY_S: f64 = 10.0;

/// Whether `v`, a camera's `latency_s`, is acceptable: absent, or finite and
/// from 0 to [`MAX_LATENCY_S`].
#[must_use]
pub fn latency_valid(v: Option<f64>) -> bool {
    v.is_none_or(|v| v.is_finite() && (0.0..=MAX_LATENCY_S).contains(&v))
}

/// The latency in effect for a camera, seconds: its `configured` value when
/// it is valid, else 0.
#[must_use]
pub fn latency_in_effect_s(configured: Option<f64>) -> f64 {
    configured
        .filter(|v| latency_valid(Some(*v)))
        .unwrap_or(0.0)
}

/// Why `latency_s` of camera `id` (e.g. `camera.0`) is not acceptable, or
/// `None` when it is ([`latency_valid`]).
#[must_use]
pub fn latency_error(id: &str, v: Option<f64>) -> Option<String> {
    if latency_valid(v) {
        return None;
    }
    v.map(|v| format!("{id}: latency_s {v} is outside 0 to {MAX_LATENCY_S} s"))
}

/// The camera rotations accepted, clockwise degrees.
pub const CAMERA_ROTATIONS: [u16; 4] = [0, 90, 180, 270];

/// Largest camera `yaw_deg` or `pitch_deg` either way, degrees.
pub const MAX_CAMERA_ANGLE_DEG: f64 = 30.0;

/// Whether `v`, a camera's `rotation_deg`, is acceptable: 0, 90, 180 or 270.
#[must_use]
pub fn rotation_valid(v: u16) -> bool {
    CAMERA_ROTATIONS.contains(&v)
}

/// The rotation in effect for a camera, clockwise degrees: its `configured`
/// value when it is valid, else 0.
#[must_use]
pub fn rotation_in_effect_deg(configured: u16) -> u16 {
    if rotation_valid(configured) {
        configured
    } else {
        0
    }
}

/// Why `rotation_deg` of camera `id` (e.g. `camera.0`) is not acceptable, or
/// `None` when it is ([`rotation_valid`]).
#[must_use]
pub fn rotation_error(id: &str, v: u16) -> Option<String> {
    (!rotation_valid(v)).then(|| format!("{id}: rotation_deg {v} is not 0, 90, 180 or 270"))
}

/// Whether `v`, a camera's `yaw_deg` or `pitch_deg`, is acceptable: absent,
/// or finite and from -[`MAX_CAMERA_ANGLE_DEG`] to [`MAX_CAMERA_ANGLE_DEG`].
#[must_use]
pub fn camera_angle_valid(v: Option<f64>) -> bool {
    v.is_none_or(|v| v.is_finite() && (-MAX_CAMERA_ANGLE_DEG..=MAX_CAMERA_ANGLE_DEG).contains(&v))
}

/// The angle in effect for a camera's `yaw_deg` or `pitch_deg`, degrees: its
/// `configured` value when it is valid, else 0.
#[must_use]
pub fn camera_angle_in_effect_deg(configured: Option<f64>) -> f64 {
    configured
        .filter(|v| camera_angle_valid(Some(*v)))
        .unwrap_or(0.0)
}

/// Why `yaw_deg` of camera `id` is not acceptable, or `None` when it is
/// ([`camera_angle_valid`]).
#[must_use]
pub fn yaw_error(id: &str, v: Option<f64>) -> Option<String> {
    angle_error(id, "yaw_deg", v)
}

/// Why `pitch_deg` of camera `id` is not acceptable, or `None` when it is
/// ([`camera_angle_valid`]).
#[must_use]
pub fn pitch_error(id: &str, v: Option<f64>) -> Option<String> {
    angle_error(id, "pitch_deg", v)
}

pub(super) fn angle_error(id: &str, field: &str, v: Option<f64>) -> Option<String> {
    if camera_angle_valid(v) {
        return None;
    }
    v.map(|v| {
        format!("{id}: {field} {v} is outside -{MAX_CAMERA_ANGLE_DEG} to {MAX_CAMERA_ANGLE_DEG}")
    })
}

impl CameraSection {
    /// Why a setting of this section, camera `id` (e.g. `camera.0`), is out
    /// of range, or `None` when all are valid: the first of `latency_s`
    /// ([`latency_error`]), `rotation_deg` ([`rotation_error`]), `yaw_deg`
    /// ([`yaw_error`]) and `pitch_deg` ([`pitch_error`]).
    #[must_use]
    pub fn error(&self, id: &str) -> Option<String> {
        latency_error(id, self.latency_s)
            .or_else(|| rotation_error(id, self.rotation_deg))
            .or_else(|| yaw_error(id, self.yaw_deg))
            .or_else(|| pitch_error(id, self.pitch_deg))
    }
}
