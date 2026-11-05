//! Body tracking types shared by the app and the UI (alpha): the pose and
//! posture that ride on a live frame, the pose and swing files of a
//! recorded clip, and the Halpe-26 keypoint layout for drawing a skeleton.
//!
//! A keypoint is `[x, y, score]`: pixels from the picture's left and top
//! edges, and the model's confidence, roughly 0 to 1. "Left" and "right" are
//! the golfer's own sides.

use serde::{Deserialize, Serialize};

/// A keypoint: `[x, y, score]` in source-picture pixels.
pub type Keypoint = [f32; 3];

/// The skeleton name both wire formats carry for the Halpe-26 layout.
pub const HALPE26: &str = "halpe26";

/// Number of Halpe-26 keypoints.
pub const HALPE26_KEYPOINTS: usize = 26;

/// The Halpe-26 bones to draw, as pairs of keypoint indices: legs to the hip
/// centre, head and neck, arms from the neck, the face, and each foot from
/// its ankle.
pub const HALPE26_BONES: [(usize, usize); 27] = [
    (15, 13),
    (13, 11),
    (11, 19),
    (16, 14),
    (14, 12),
    (12, 19),
    (17, 18),
    (18, 19),
    (18, 5),
    (5, 7),
    (7, 9),
    (18, 6),
    (6, 8),
    (8, 10),
    (1, 2),
    (0, 1),
    (0, 2),
    (1, 3),
    (2, 4),
    (3, 5),
    (4, 6),
    (15, 20),
    (15, 22),
    (15, 24),
    (16, 21),
    (16, 23),
    (16, 25),
];

/// Which side of the golfer's body a keypoint is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodySide {
    /// The golfer's left.
    Left,
    /// The golfer's right.
    Right,
    /// On the midline: nose, head, neck, hip centre.
    Center,
}

/// The side of Halpe-26 keypoint `i`: among the 17 COCO points the odd
/// indices after the nose are the left side and the even ones the right;
/// among the six foot points (20 to 25) the even ones are the left; head
/// (17), neck (18), hip centre (19) and the nose are on the midline.
#[must_use]
pub fn halpe26_side(i: usize) -> BodySide {
    match i {
        1..=16 if i % 2 == 1 => BodySide::Left,
        20..=25 if i.is_multiple_of(2) => BodySide::Left,
        1..=16 | 20..=25 => BodySide::Right,
        _ => BodySide::Center,
    }
}

/// A bone's side: the side its two ends share, else the midline.
#[must_use]
pub fn halpe26_bone_side((a, b): (usize, usize)) -> BodySide {
    let (sa, sb) = (halpe26_side(a), halpe26_side(b));
    if sa == sb { sa } else { BodySide::Center }
}

/// The pose found on one live frame (`X-Pose` on
/// `GET /api/cameras/{actor}/live`), compact: coordinates to 0.1 px, scores
/// to 0.01.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LivePose {
    /// The keypoint layout ([`HALPE26`]).
    pub skeleton: String,
    /// One `[x, y, score]` per keypoint in frame pixels; empty when nobody
    /// was found in the frame.
    #[serde(default)]
    pub keypoints: Vec<Keypoint>,
    /// The box the pose model looked in, `[x1, y1, x2, y2]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f32; 4]>,
    /// How long body tracking took on the frame, ms.
    pub ms: f32,
    /// The golfer's posture on the frame, for a face-on or down-the-line
    /// camera that found them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posture: Option<LivePosture>,
}

/// The camera view a swing report or a live posture is measured for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwingView {
    /// Facing the golfer's chest.
    FaceOn,
    /// Behind the golfer's hands, down the target line.
    DownTheLine,
}

/// A live frame's posture: 2D angles in the picture, degrees to 0.1,
/// smoothed over the frames. Each is the view's own set: face-on the
/// spine's tilt, the shoulders' tilt and the lead arm; down the line the
/// spine's forward bend, both knees' flex and the trail elbow. An angle
/// whose keypoints were not found is left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LivePosture {
    /// The view the angles are measured for.
    pub view: SwingView,
    /// Spine (hips to shoulders) from vertical: face-on the sideways tilt,
    /// positive with the shoulders away from the target; down the line the
    /// forward bend, positive towards the ball.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spine_angle: Option<f32>,
    /// Shoulder line from horizontal, positive with the lead shoulder
    /// higher (face-on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shoulder_tilt: Option<f32>,
    /// Lead shoulder-elbow-wrist angle, 180 straight (face-on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_arm_angle: Option<f32>,
    /// Lead knee flex, 0 straight (down the line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_knee_flex: Option<f32>,
    /// Trail knee flex, 0 straight (down the line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail_knee_flex: Option<f32>,
    /// Trail shoulder-elbow-wrist angle, 180 straight (down the line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail_elbow_angle: Option<f32>,
}

/// Rounds an angle to 0.1°, as the live wire carries it.
#[must_use]
pub fn round_deg(v: f64) -> f32 {
    // Angles are within ±360°.
    #[allow(clippy::cast_possible_truncation)]
    let deg = ((v * 10.0).round() / 10.0) as f32;
    deg
}

/// Rounds a pixel coordinate to 0.1 px, as the live wire carries it.
#[must_use]
pub fn round_px(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

/// Rounds a score to 0.01, as the live wire carries it.
#[must_use]
pub fn round_score(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// A recorded clip's keypoints (`<camera>.pose.json` beside the clip):
/// what the UI reads of stance's pose track. One frame per clip frame, on
/// the sidecar's time base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoseTrackFile {
    /// Format version (1).
    pub version: u32,
    /// The keypoint layout ([`HALPE26`]).
    pub skeleton: String,
    /// The pose model's name.
    #[serde(default)]
    pub model: String,
    /// Frame size in pixels.
    pub width: u32,
    pub height: u32,
    /// Every clip frame in clip order.
    #[serde(default)]
    pub frames: Vec<PoseTrackFrame>,
}

/// One frame of a [`PoseTrackFile`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoseTrackFrame {
    /// The frame's time after the clip's first frame, ns (the sidecar's
    /// `t_ns`).
    pub t_ns: u64,
    /// `[x, y, score]` per keypoint; empty where nobody was found.
    #[serde(default)]
    pub keypoints: Vec<Keypoint>,
}

impl PoseTrackFile {
    /// The keypoints of the frame at `t_ns` after the clip's first frame:
    /// the frame with exactly that time, `None` when there is none or nobody
    /// was found in it.
    #[must_use]
    pub fn keypoints_at(&self, t_ns: u64) -> Option<&[Keypoint]> {
        let i = self.frames.binary_search_by_key(&t_ns, |f| f.t_ns).ok()?;
        let frame = &self.frames[i];
        (!frame.keypoints.is_empty()).then_some(frame.keypoints.as_slice())
    }
}

/// A recorded clip's swing analysis (`<camera>.swing.json` beside the
/// clip): what the UI reads of stance's swing report. Times are on the
/// clip's time base (ns after its first frame).
///
/// Reads both layouts stance has written: the view at the top level, and
/// an earlier one with it in a `setup` object (whose `given` impact was
/// used as is, so its impact source reads as unknown). The metric id
/// `hip_width_ratio` reads as `hip_width_proxy`; the ids `sway` and
/// `slide` (one direction each of the pelvis shift `hip_sway` measures)
/// are left out, from the metrics and the series. A report without a
/// per-frame series reads with an empty one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "SwingReportRaw")]
pub struct SwingReportFile {
    /// Format version (1).
    pub version: u32,
    /// The view the metrics are measured for; `None` when stance couldn't
    /// tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<SwingView>,
    /// The golfer's height (cm), when it was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_cm: Option<f64>,
    /// The swing's key moments.
    pub events: SwingEventsFile,
    /// Backswing against downswing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo: Option<SwingTempo>,
    /// The view's metrics, each at the key positions it was measured at.
    #[serde(default)]
    pub metrics: Vec<SwingMetric>,
    /// The body scale distances are measured in, when the torso was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<SwingScale>,
    /// Every metric at every frame of the clip.
    #[serde(default, skip_serializing_if = "SwingSeries::is_empty")]
    pub series: SwingSeries,
}

impl SwingReportFile {
    /// Whether the analysis found a swing. Without one (the given impact
    /// time unconfirmed by the hands) the report holds the impact alone.
    #[must_use]
    pub fn swing_found(&self) -> bool {
        self.events.impact_source != Some(SwingImpactSource::Given)
    }

    /// Metric `id` at the frame nearest `t_ns` after the clip's first
    /// frame: `None` outside the series, for a metric without one, or where
    /// the frame has no value.
    #[must_use]
    pub fn sample(&self, id: &str, t_ns: u64) -> Option<SwingSample> {
        let i = self.series.frame_at(t_ns)?;
        let m = self.series.metrics.iter().find(|m| m.id == id)?;
        let value = m.values.get(i).copied().flatten()?;
        let cm = match (m.unit.as_str(), self.scale) {
            ("torso", Some(s)) => s.cm_per_px.map(|c| value * s.torso_px * c),
            _ => None,
        };
        Some(SwingSample {
            unit: m.unit.clone(),
            value,
            cm,
        })
    }
}

/// The body scale of a [`SwingReportFile`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SwingScale {
    /// The torso length at address in pixels: the `torso` unit.
    pub torso_px: f64,
    /// Centimetres per pixel, when the golfer's height was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cm_per_px: Option<f64>,
}

/// Every metric of a [`SwingReportFile`] at every frame of the clip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "SwingSeriesRaw")]
pub struct SwingSeries {
    /// The frame times, ns after the clip's first frame (the sidecar's
    /// `t_ns`), in order.
    pub t_ns: Vec<u64>,
    /// One value list per metric, aligned to [`SwingSeries::t_ns`].
    pub metrics: Vec<SwingMetricSeries>,
}

impl SwingSeries {
    /// Whether no metric has a series.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.metrics.is_empty()
    }

    /// The index of the frame nearest `t_ns` (ties go to the earlier one);
    /// `None` before the first frame or after the last.
    #[must_use]
    pub fn frame_at(&self, t_ns: u64) -> Option<usize> {
        let (first, last) = (*self.t_ns.first()?, *self.t_ns.last()?);
        if t_ns < first || t_ns > last {
            return None;
        }
        let i = self.t_ns.partition_point(|&t| t < t_ns);
        if i == 0 {
            return Some(0);
        }
        let (before, after) = (self.t_ns[i - 1], self.t_ns[i]);
        Some(if t_ns - before <= after - t_ns {
            i - 1
        } else {
            i
        })
    }
}

/// One metric's value at each frame of a [`SwingSeries`]; `None` where it
/// was not measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwingMetricSeries {
    /// Which metric (`hip_sway`, `hip_turn`, ...).
    pub id: String,
    /// `degrees`, `torso` or `ratio`.
    pub unit: String,
    pub values: Vec<Option<f64>>,
}

/// A metric's value at one frame (and its centimetres, for a `torso`
/// metric when the golfer's height was given).
#[derive(Debug, Clone, PartialEq)]
pub struct SwingSample {
    pub unit: String,
    pub value: f64,
    pub cm: Option<f64>,
}

/// [`SwingSeries`] as written: a metric whose length is not the number of
/// frames is left out.
#[derive(Deserialize)]
struct SwingSeriesRaw {
    #[serde(default)]
    t_ns: Vec<u64>,
    #[serde(default)]
    metrics: Vec<SwingMetricSeries>,
}

impl From<SwingSeriesRaw> for SwingSeries {
    fn from(raw: SwingSeriesRaw) -> Self {
        let n = raw.t_ns.len();
        Self {
            metrics: raw
                .metrics
                .into_iter()
                .filter(|m| m.values.len() == n)
                .collect(),
            t_ns: raw.t_ns,
        }
    }
}

/// Metric ids a [`SwingReportFile`] leaves out: one direction each of the
/// pelvis shift `hip_sway` measures.
const LEFT_OUT: &[&str] = &["sway", "slide"];

/// [`SwingReportFile`] as written, in either layout.
#[derive(Deserialize)]
struct SwingReportRaw {
    version: u32,
    #[serde(default)]
    view: Option<SwingView>,
    #[serde(default)]
    setup: Option<SwingSetupRaw>,
    #[serde(default)]
    height_cm: Option<f64>,
    events: SwingEventsFile,
    #[serde(default)]
    tempo: Option<SwingTempo>,
    #[serde(default)]
    metrics: Vec<SwingMetric>,
    #[serde(default)]
    scale: Option<SwingScale>,
    #[serde(default)]
    series: SwingSeries,
}

/// The earlier layout's `setup` object.
#[derive(Deserialize)]
struct SwingSetupRaw {
    view: SwingView,
}

impl From<SwingReportRaw> for SwingReportFile {
    fn from(raw: SwingReportRaw) -> Self {
        let mut events = raw.events;
        if raw.setup.is_some() && events.impact_source == Some(SwingImpactSource::Given) {
            events.impact_source = None;
        }
        let mut metrics = raw.metrics;
        metrics.retain(|m| !LEFT_OUT.contains(&m.id.as_str()));
        for m in &mut metrics {
            if m.id == "hip_width_ratio" {
                m.id = "hip_width_proxy".to_owned();
            }
        }
        let mut series = raw.series;
        series
            .metrics
            .retain(|m| !LEFT_OUT.contains(&m.id.as_str()));
        Self {
            version: raw.version,
            view: raw.view.or(raw.setup.map(|s| s.view)),
            height_cm: raw.height_cm,
            events,
            tempo: raw.tempo,
            metrics,
            scale: raw.scale,
            series,
        }
    }
}

/// Where a swing's impact time came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwingImpactSource {
    /// The given time (the launch monitor's impact), unconfirmed: no swing
    /// was found near it, so the report holds the impact alone.
    Given,
    /// The given time moved to the hands' low point near it.
    Refined,
    /// Estimated from the hands alone.
    Estimated,
    /// A source this version doesn't know.
    #[serde(other)]
    Other,
}

/// A swing event's time and how sure the analysis is (0 to 1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SwingEvent {
    /// ns after the clip's first frame.
    pub t_ns: u64,
    #[serde(default)]
    pub confidence: f32,
}

/// The key moments of a swing; a moment the clip does not show is absent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwingEventsFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<SwingEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takeaway: Option<SwingEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<SwingEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<SwingEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<SwingEvent>,
    /// Where the impact time came from; `None` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_source: Option<SwingImpactSource>,
}

impl SwingEventsFile {
    /// The events in swing order with their names.
    #[must_use]
    pub fn named(&self) -> [(&'static str, Option<SwingEvent>); 5] {
        [
            ("Address", self.address),
            ("Takeaway", self.takeaway),
            ("Top", self.top),
            ("Impact", self.impact),
            ("Finish", self.finish),
        ]
    }
}

/// Swing tempo: backswing (takeaway to top) against downswing (top to
/// impact).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SwingTempo {
    pub backswing_ns: u64,
    pub downswing_ns: u64,
    /// `backswing_ns / downswing_ns`.
    pub ratio: f64,
}

/// One body metric of a [`SwingReportFile`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwingMetric {
    /// Which metric (`hip_sway`, `lead_arm_angle`, ...).
    pub id: String,
    /// `degrees`, `torso` (fractions of the torso length) or `ratio`.
    pub unit: String,
    /// The value at each key position.
    #[serde(default)]
    pub values: SwingPositions,
    /// A `torso` metric's values in centimetres, when the golfer's height
    /// was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cm: Option<SwingPositions>,
}

/// A metric's value at each key position, absent where it was not
/// measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SwingPositions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<f64>,
}

/// Where body tracking's model files are (`GET
/// /api/body_tracking/models`): the state of the files the configured pose
/// model needs, where they are kept, and a download's progress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BodyTrackingModels {
    /// `missing`, `downloading`, `ready` or `failed`.
    pub state: ModelFilesState,
    /// The configured pose model.
    pub model: crate::PoseModel,
    /// The models folder.
    pub dir: String,
    /// The files that model needs: the pose model, then the person
    /// detector.
    pub files: Vec<ModelFile>,
    /// The download running, when `state` is `downloading`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<ModelDownload>,
    /// Why the last download failed, when `state` is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BodyTrackingModels {
    /// The files not in the folder yet: what a download fetches.
    #[must_use]
    pub fn missing(&self) -> Vec<&ModelFile> {
        self.files.iter().filter(|f| !f.present).collect()
    }
}

/// The state of body tracking's model files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFilesState {
    /// A file is not in the folder (or does not match its SHA-256), and no
    /// download is running.
    #[default]
    Missing,
    /// A download is running.
    Downloading,
    /// Every file is in the folder and matches its SHA-256.
    Ready,
    /// The last download failed; `error` says why.
    Failed,
}

/// One model file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFile {
    /// File name in the models folder, e.g. `rtmpose-m-halpe26.onnx`.
    pub name: String,
    /// What it is, e.g. `RTMPose-m pose model`.
    pub label: String,
    /// The host it is downloaded from.
    pub host: String,
    /// Its size in bytes.
    pub size: u64,
    /// In the folder and matching its SHA-256.
    pub present: bool,
}

/// A download's progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDownload {
    /// The files it fetches, in order.
    pub files: Vec<ModelFileProgress>,
    /// Index in `files` of the one being fetched.
    pub current: usize,
    /// Bytes fetched over all its files.
    pub done: u64,
    /// Bytes to fetch over all its files.
    pub total: u64,
}

impl ModelDownload {
    /// The file being fetched.
    #[must_use]
    pub fn current_file(&self) -> Option<&ModelFileProgress> {
        self.files.get(self.current)
    }
}

/// One file of a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFileProgress {
    /// File name in the models folder.
    pub name: String,
    /// What it is.
    pub label: String,
    /// The host it comes from.
    pub host: String,
    /// Bytes fetched.
    pub done: u64,
    /// Bytes to fetch: the server's length once the response starts, the
    /// model's size until then.
    pub total: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halpe26_sides() {
        assert_eq!(halpe26_side(0), BodySide::Center);
        assert_eq!(halpe26_side(5), BodySide::Left, "left shoulder");
        assert_eq!(halpe26_side(10), BodySide::Right, "right wrist");
        for i in 17..=19 {
            assert_eq!(halpe26_side(i), BodySide::Center);
        }
        assert_eq!(halpe26_side(24), BodySide::Left, "left heel");
        assert_eq!(halpe26_side(25), BodySide::Right, "right heel");
        assert_eq!(halpe26_bone_side((15, 13)), BodySide::Left);
        assert_eq!(halpe26_bone_side((18, 6)), BodySide::Center);
        assert!(
            HALPE26_BONES
                .iter()
                .all(|&(a, b)| a < HALPE26_KEYPOINTS && b < HALPE26_KEYPOINTS)
        );
    }

    #[test]
    fn a_live_pose_is_compact() {
        let pose = LivePose {
            skeleton: HALPE26.into(),
            keypoints: vec![[round_px(651.234), round_px(160.36), round_score(0.934)]],
            bbox: Some([512.0, 96.5, 790.0, 700.0]),
            ms: 19.0,
            posture: None,
        };
        let json = serde_json::to_string(&pose).unwrap();
        assert_eq!(
            json,
            r#"{"skeleton":"halpe26","keypoints":[[651.2,160.4,0.93]],"bbox":[512.0,96.5,790.0,700.0],"ms":19.0}"#
        );
        assert_eq!(serde_json::from_str::<LivePose>(&json).unwrap(), pose);
        let nobody: LivePose =
            serde_json::from_str(r#"{"skeleton":"halpe26","keypoints":[],"ms":7.5}"#).unwrap();
        assert!(nobody.keypoints.is_empty() && nobody.bbox.is_none());
    }

    #[test]
    fn a_track_frame_is_found_by_its_exact_time() {
        let json = r#"{"version":1,"skeleton":"halpe26","model":"rtmpose-m-halpe26",
            "detector":"yolox-tiny","width":640,"height":480,"frames":[
            {"t_ns":0,"score":0.8,"bbox":{"x1":1.0,"y1":2.0,"x2":3.0,"y2":4.0},"keypoints":[[1.0,2.0,0.9]]},
            {"t_ns":8333333},
            {"t_ns":16666666,"score":0.7,"keypoints":[[5.0,6.0,0.4]]}]}"#;
        let track: PoseTrackFile = serde_json::from_str(json).unwrap();
        assert_eq!(track.width, 640);
        assert_eq!(track.keypoints_at(0), Some(&[[1.0, 2.0, 0.9]][..]));
        assert_eq!(track.keypoints_at(8_333_333), None, "nobody found");
        assert_eq!(track.keypoints_at(16_666_666).map(|k| k[0][0]), Some(5.0));
        assert_eq!(track.keypoints_at(16_666_667), None, "no frame then");
    }
}

#[cfg(test)]
mod swing_tests {
    use super::*;

    #[test]
    fn a_live_posture_rides_in_the_pose_and_leaves_out_what_it_lacks() {
        let pose = LivePose {
            skeleton: HALPE26.into(),
            keypoints: Vec::new(),
            bbox: None,
            ms: 20.0,
            posture: Some(LivePosture {
                view: SwingView::DownTheLine,
                spine_angle: Some(round_deg(38.04)),
                shoulder_tilt: None,
                lead_arm_angle: None,
                lead_knee_flex: Some(round_deg(21.96)),
                trail_knee_flex: None,
                trail_elbow_angle: Some(round_deg(171.25)),
            }),
        };
        let json = serde_json::to_string(&pose).unwrap();
        assert_eq!(
            json,
            r#"{"skeleton":"halpe26","keypoints":[],"ms":20.0,"posture":{"view":"down_the_line","spine_angle":38.0,"lead_knee_flex":22.0,"trail_elbow_angle":171.3}}"#
        );
        assert_eq!(serde_json::from_str::<LivePose>(&json).unwrap(), pose);
    }

    #[test]
    fn a_swing_report_reads_what_the_ui_shows_and_ignores_the_rest() {
        let json = r#"{"version":1,"view":"face_on","handed":"right",
            "view_guess":{"value":"face_on","confidence":0.8},
            "handed_guess":{"value":"right","confidence":1.0},
            "height_cm":180.0,"scale":{"torso_px":200.0,"cm_per_px":0.25},
            "smoothing":{"min_score":0.3,"max_gap_ns":150000000,"cutoff_hz":8.0,"order":2},
            "events":{"address":{"t_ns":1000000000,"frame":120,"confidence":0.8},
              "top":{"t_ns":1850000000,"frame":222,"confidence":0.9},
              "impact":{"t_ns":2150000000,"frame":258,"confidence":0.9},
              "impact_source":"refined"},
            "tempo":{"backswing_ns":841666667,"downswing_ns":300000000,"ratio":2.81},
            "metrics":[{"id":"hip_sway","view":"face_on","unit":"torso",
              "values":{"top":-0.05,"impact":0.12},"cm":{"top":-2.5,"impact":6.0},"confidence":0.8},
              {"id":"some_new_metric","view":null,"unit":"degrees","values":{"impact":3.0},"confidence":0.5}]}"#;
        let r: SwingReportFile = serde_json::from_str(json).unwrap();
        assert_eq!(r.view, Some(SwingView::FaceOn));
        assert_eq!(r.events.impact_source, Some(SwingImpactSource::Refined));
        assert!(r.swing_found());
        assert_eq!(r.height_cm, Some(180.0));
        assert!(r.events.takeaway.is_none());
        assert_eq!(r.events.top.map(|e| e.t_ns), Some(1_850_000_000));
        assert_eq!(r.events.named()[3].0, "Impact");
        assert!((r.tempo.unwrap().ratio - 2.81).abs() < 1e-9);
        assert_eq!(r.metrics.len(), 2);
        assert_eq!(r.metrics[0].id, "hip_sway");
        assert_eq!(r.metrics[0].cm.unwrap().top, Some(-2.5));
        assert_eq!(r.metrics[0].cm.unwrap().impact, Some(6.0));
        assert_eq!(r.metrics[1].id, "some_new_metric");
        // Written back and read again, it is the same.
        let again: SwingReportFile =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(again, r);
    }

    #[test]
    fn an_unconfirmed_impact_is_no_swing_and_an_unknown_view_or_source_reads() {
        let json = r#"{"version":1,"handed":"right","mirrored":true,
            "events":{"impact":{"t_ns":2150000000,"frame":258,"confidence":0.5},
              "impact_source":"given"},
            "metrics":[{"id":"head_vertical","view":null,"unit":"torso","values":{"impact":0.01},"confidence":0.5}]}"#;
        let r: SwingReportFile = serde_json::from_str(json).unwrap();
        assert_eq!(r.view, None);
        assert_eq!(r.events.impact_source, Some(SwingImpactSource::Given));
        assert!(!r.swing_found());
        let r: SwingReportFile = serde_json::from_str(
            r#"{"version":1,"view":"down_the_line","events":{"impact_source":"guessed"}}"#,
        )
        .unwrap();
        assert_eq!(r.events.impact_source, Some(SwingImpactSource::Other));
        assert!(r.swing_found());
    }

    #[test]
    fn an_earlier_swing_file_still_reads() {
        // The layout written before the view moved to the top level: the
        // view in `setup`, `given` meaning the impact was used as is, and
        // `hip_width_ratio`.
        let json = r#"{"version":1,"setup":{"view":"down_the_line","handed":"left"},
            "events":{"address":{"t_ns":1000000000,"frame":120,"confidence":0.8},
              "top":{"t_ns":1850000000,"frame":222,"confidence":0.9},
              "impact":{"t_ns":2150000000,"frame":258,"confidence":1.0},
              "impact_source":"given"},
            "tempo":{"backswing_ns":841666667,"downswing_ns":300000000,"ratio":2.81},
            "metrics":[{"id":"hip_width_ratio","view":"down_the_line","unit":"ratio",
              "values":{"address":0.5},"confidence":0.8}]}"#;
        let r: SwingReportFile = serde_json::from_str(json).unwrap();
        assert_eq!(r.view, Some(SwingView::DownTheLine));
        assert_eq!(r.events.impact_source, None);
        assert!(r.swing_found());
        assert!(r.tempo.is_some());
        assert_eq!(r.metrics[0].id, "hip_width_proxy");

        // `sway` and `slide` are left out, metrics and series; the rest
        // reads.
        let json = r#"{"version":1,"view":"face_on","events":{"impact_source":"refined"},
            "metrics":[{"id":"sway","view":"face_on","unit":"torso","values":{"top":0.05},"confidence":0.8},
              {"id":"slide","view":"face_on","unit":"torso","values":{"impact":0.15},"confidence":0.8},
              {"id":"head_lateral","view":"face_on","unit":"torso","values":{"top":0.01},"confidence":0.8}],
            "series":{"t_ns":[0,10],"metrics":[
              {"id":"sway","unit":"torso","values":[0.0,0.01]},
              {"id":"slide","unit":"torso","values":[0.0,-0.01]},
              {"id":"head_lateral","unit":"torso","values":[0.0,0.02]}]}}"#;
        let r: SwingReportFile = serde_json::from_str(json).unwrap();
        let ids: Vec<&str> = r.metrics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["head_lateral"]);
        let ids: Vec<&str> = r.series.metrics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["head_lateral"]);
        assert!(r.sample("sway", 10).is_none() && r.sample("hip_sway", 10).is_none());
        assert_eq!(r.sample("head_lateral", 10).unwrap().value, 0.02);
    }

    #[test]
    fn a_swing_reports_series_reads_with_its_scale() {
        let json = r#"{"version":1,"view":"face_on","handed":"right",
            "scale":{"torso_px":200.0,"cm_per_px":0.25},
            "events":{"impact_source":"refined"},
            "metrics":[{"id":"hip_turn","view":"face_on","unit":"degrees",
              "values":{"top":40.0},"confidence":0.8}],
            "series":{"t_ns":[0,10,20,30],"metrics":[
              {"id":"hip_turn","unit":"degrees","values":[null,10.0,40.0,20.0]},
              {"id":"hip_sway","unit":"torso","values":[0.0,0.01,-0.02,null]},
              {"id":"x_factor","unit":"degrees","values":[1.0,2.0,3.0,4.0]},
              {"id":"broken","unit":"degrees","values":[1.0]}]}}"#;
        let r: SwingReportFile = serde_json::from_str(json).unwrap();
        assert_eq!(r.series.t_ns, [0, 10, 20, 30]);
        let ids: Vec<&str> = r.series.metrics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            ["hip_turn", "hip_sway", "x_factor"],
            "a wrong length is left out"
        );
        assert_eq!(
            r.scale,
            Some(SwingScale {
                torso_px: 200.0,
                cm_per_px: Some(0.25)
            })
        );
        // The nearest frame, ties to the earlier one; none outside.
        assert_eq!(r.series.frame_at(14), Some(1));
        assert_eq!(r.series.frame_at(15), Some(1));
        assert_eq!(r.series.frame_at(16), Some(2));
        assert_eq!(r.series.frame_at(31), None);
        let s = r.sample("hip_turn", 20).unwrap();
        assert_eq!((s.unit.as_str(), s.value, s.cm), ("degrees", 40.0, None));
        // A torso share with its centimetres from the scale.
        let s = r.sample("hip_sway", 21).unwrap();
        assert_eq!((s.value, s.cm), (-0.02, Some(-0.02 * 200.0 * 0.25)));
        assert!(r.sample("hip_turn", 0).is_none(), "no value at that frame");
        assert!(r.sample("hip_sway", 30).is_none());
        assert!(r.sample("head_lateral", 10).is_none(), "no series");
        // Written back and read again, it is the same.
        let again: SwingReportFile =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(again, r);

        // An earlier report has neither: it reads, and samples nothing.
        let old: SwingReportFile = serde_json::from_str(
            r#"{"version":1,"view":"face_on","events":{"impact_source":"refined"},"metrics":[]}"#,
        )
        .unwrap();
        assert!(old.series.is_empty() && old.scale.is_none());
        assert!(old.sample("hip_turn", 0).is_none());
        assert!(!serde_json::to_string(&old).unwrap().contains("series"));
    }

    #[test]
    fn the_model_files_state_reads_back_and_omits_what_is_not_there() {
        let m = BodyTrackingModels {
            state: ModelFilesState::Missing,
            model: crate::PoseModel::RtmposeM,
            dir: "/models".into(),
            files: vec![
                ModelFile {
                    name: "rtmpose-m-halpe26.onnx".into(),
                    label: "RTMPose-m pose model".into(),
                    host: "download.openmmlab.com".into(),
                    size: 55_685_444,
                    present: false,
                },
                ModelFile {
                    name: "yolox-tiny.onnx".into(),
                    label: "YOLOX-Tiny person detector".into(),
                    host: "github.com".into(),
                    size: 20_219_662,
                    present: true,
                },
            ],
            download: None,
            error: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(
            json.starts_with(r#"{"state":"missing","model":"rtmpose-m","#),
            "{json}"
        );
        assert!(
            !json.contains(r#""download""#) && !json.contains(r#""error""#),
            "{json}"
        );
        assert_eq!(
            serde_json::from_str::<BodyTrackingModels>(&json).unwrap(),
            m
        );
        assert_eq!(m.missing().len(), 1);
        let d = ModelDownload {
            files: vec![],
            current: 0,
            done: 0,
            total: 0,
        };
        assert!(d.current_file().is_none());
    }
}
