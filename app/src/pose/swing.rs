//! Golf numbers from body tracking (alpha): the one place flighthook uses
//! stance's golf analysis.
//!
//! - After a recorded swing's keypoints are written, [`write_report`] runs
//!   stance's swing analysis on them and stores `<camera>.swing.json`
//!   beside the clip: swing events, tempo and the camera view's body
//!   metrics.
//! - While a live view is watched, [`LivePostureFilter`] measures each
//!   frame's posture for the camera's view, smoothed, for the frame's
//!   `X-Pose`.
//!
//! The camera's position picks the view: facing the golfer is face-on,
//! down the target line is down-the-line; a camera behind or above the
//! golfer gets neither (the keypoints only). The golfer's hand comes from
//! flighthook's hand state, right-handed when none is known.
//!
//! The clip's estimated impact is a prior: stance moves it to the hands'
//! low point near it (refined), or, finding no swing there, keeps it and
//! reports the impact alone (given). The refined time minus the estimate
//! is returned for the log, to calibrate each launch monitor's camera
//! timing.

use std::path::{Path, PathBuf};

use flighthook::{CameraPosition, Handedness, LivePosture, SwingView, round_deg};
use stance::golf::{
    Guess, Handed, ImpactSource, PostureFilter, SwingOptions, SwingReport, View, analyze_swing,
    unmirror,
};
use stance::pose::{OneEuroParams, Pose, PoseTrack};
use tracing::warn;

/// A handedness guess at least this sure, disagreeing with the player's
/// hand, is logged.
const HANDED_WARN_CONFIDENCE: f32 = 0.6;

/// Whether `camera`'s picture is mirrored (a selfie-style preview). There
/// is no mirror setting yet, so no camera is; a setting plugs in here, and
/// the swing analysis and the live posture both read it.
pub fn mirrored(_camera: &str) -> bool {
    false
}

/// The view a camera at `position` gives: face-on facing the golfer, down
/// the line on the target line, none from behind or above.
pub fn view_for(position: CameraPosition) -> Option<View> {
    match position {
        CameraPosition::GolferFront => Some(View::FaceOn),
        CameraPosition::TargetLine => Some(View::DownTheLine),
        CameraPosition::GolferBack | CameraPosition::Overhead => None,
    }
}

/// stance's handedness for the player's `hand`, right-handed when unknown.
pub fn handed(hand: Option<Handedness>) -> Handed {
    match hand {
        Some(Handedness::Left) => Handed::Left,
        _ => Handed::Right,
    }
}

fn wire_view(view: View) -> SwingView {
    match view {
        View::FaceOn => SwingView::FaceOn,
        View::DownTheLine => SwingView::DownTheLine,
    }
}

fn hand_word(handed: Handed) -> &'static str {
    match handed {
        Handed::Right => "right-handed",
        Handed::Left => "left-handed",
    }
}

/// What the swing analysis of one clip is told.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SwingInput {
    /// Where the camera stands.
    pub position: CameraPosition,
    /// The player's hand when the shot was taken.
    pub hand: Option<Handedness>,
    /// The clip's estimated impact, ns after its first frame (the launch
    /// monitor's impact shifted by the camera's timing).
    pub impact_ns: Option<u64>,
    /// The golfer's height, cm.
    pub height_cm: Option<f64>,
    /// The camera's picture is mirrored ([`mirrored`]).
    pub mirrored: bool,
}

/// A swing report written beside its clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwingWritten {
    /// `<camera>.swing.json`.
    pub path: PathBuf,
    /// A swing was found: the impact is refined or estimated from the
    /// hands. Without one (the given impact unconfirmed) the report holds
    /// the impact alone and the UI shows no swing data.
    pub swing_found: bool,
    /// The refined impact minus the clip's estimated impact, ns, when the
    /// estimate was refined.
    pub impact_shift_ns: Option<i64>,
}

/// Analyse the swing in `track` (the keypoints of clip `avi`) and write
/// `<camera>.swing.json` beside the clip. `Ok(None)` when the camera's
/// position gets no swing analysis. A confident handedness guess that
/// disagrees with the player's hand is logged (a mirrored webcam picture
/// makes a right-hander look left-handed).
///
/// # Errors
///
/// Why the analysis or the write failed.
pub fn write_report(
    camera: &str,
    track: &PoseTrack,
    avi: &Path,
    input: &SwingInput,
) -> Result<Option<SwingWritten>, String> {
    let Some(view) = view_for(input.position) else {
        return Ok(None);
    };
    let handed = handed(input.hand);
    let options = SwingOptions {
        view: Some(view),
        handed,
        impact_ns: input.impact_ns,
        height_cm: input.height_cm,
        mirrored: input.mirrored,
        ..SwingOptions::default()
    };
    let report = analyze_swing(track, &options).map_err(|e| e.to_string())?;
    if let Some(guess) = handed_mismatch(&report, handed) {
        warn!(
            "{camera}: the swing looks {} ({:.0} % sure) but the player is {}; is the camera picture mirrored?",
            hand_word(guess.value),
            guess.confidence * 100.0,
            hand_word(handed)
        );
    }
    let path = report.write_beside(avi).map_err(|e| e.to_string())?;
    Ok(Some(SwingWritten {
        path,
        swing_found: report.events.impact_source != ImpactSource::Given,
        impact_shift_ns: impact_shift_ns(&report, input.impact_ns),
    }))
}

/// `report`'s refined impact minus the `given` estimate, ns; `None` unless
/// the estimate was refined.
fn impact_shift_ns(report: &SwingReport, given: Option<u64>) -> Option<i64> {
    let given = given?;
    let found = report.events.impact?;
    (report.events.impact_source == ImpactSource::Refined)
        .then(|| i64::try_from(i128::from(found.t_ns) - i128::from(given)).ok())
        .flatten()
}

/// The handedness `report`'s swing suggests, when it disagrees with
/// `handed` and is at least [`HANDED_WARN_CONFIDENCE`] sure.
pub fn handed_mismatch(report: &SwingReport, handed: Handed) -> Option<Guess<Handed>> {
    report
        .handed_guess
        .filter(|g| g.value != handed && g.confidence >= HANDED_WARN_CONFIDENCE)
}

/// What a live view's posture is measured for: the camera's view, the
/// player's hand and whether the picture is mirrored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveSetup {
    view: View,
    handed: Handed,
    mirrored: bool,
}

/// The live posture setup of a camera at `position` for the player's
/// `hand`, its picture `mirrored` or not; `None` for a position without a
/// view.
pub fn live_setup(
    position: CameraPosition,
    hand: Option<Handedness>,
    mirrored: bool,
) -> Option<LiveSetup> {
    Some(LiveSetup {
        view: view_for(position)?,
        handed: handed(hand),
        mirrored,
    })
}

/// A live view's posture, smoothed frame to frame with stance's posture
/// filter (One Euro, at the live smoothing cutoff; raw when it is off). A
/// new cutoff or setup starts it afresh.
#[derive(Debug, Default)]
pub struct LivePostureFilter {
    filter: Option<(PostureFilter, f64, LiveSetup)>,
}

impl LivePostureFilter {
    /// Forget the history (the golfer was lost).
    pub fn reset(&mut self) {
        if let Some((filter, ..)) = &mut self.filter {
            filter.reset();
        }
    }

    /// The posture of `pose`, found at `t_ns` in a picture `width` pixels
    /// wide, for `setup`, smoothed at a cutoff of `hz` (0: raw), as the live
    /// wire carries it.
    pub fn measure(
        &mut self,
        pose: &Pose,
        width: u32,
        t_ns: u64,
        setup: LiveSetup,
        hz: f64,
    ) -> LivePosture {
        let raw = if setup.mirrored {
            stance::golf::posture(&unmirror(pose, width as f32), setup.view, setup.handed)
        } else {
            stance::golf::posture(pose, setup.view, setup.handed)
        };
        let smoothed = if hz > 0.0 && hz.is_finite() {
            let same = |(_, h, s): &(PostureFilter, f64, LiveSetup)| {
                h.to_bits() == hz.to_bits() && *s == setup
            };
            if !self.filter.as_ref().is_some_and(same) {
                let params = OneEuroParams {
                    min_cutoff: hz,
                    ..OneEuroParams::default()
                };
                self.filter = Some((PostureFilter::new(params), hz, setup));
            }
            match &mut self.filter {
                Some((filter, ..)) => filter.filter(t_ns, &raw),
                None => raw,
            }
        } else {
            self.filter = None;
            raw
        };
        let deg = |v: Option<f64>| v.map(round_deg);
        match setup.view {
            View::FaceOn => LivePosture {
                view: wire_view(setup.view),
                spine_angle: deg(smoothed.spine_angle),
                shoulder_tilt: deg(smoothed.shoulder_tilt),
                lead_arm_angle: deg(smoothed.lead_elbow_angle),
                lead_knee_flex: None,
                trail_knee_flex: None,
                trail_elbow_angle: None,
            },
            View::DownTheLine => LivePosture {
                view: wire_view(setup.view),
                spine_angle: deg(smoothed.spine_angle),
                shoulder_tilt: None,
                lead_arm_angle: None,
                lead_knee_flex: deg(smoothed.lead_knee_flex),
                trail_knee_flex: deg(smoothed.trail_knee_flex),
                trail_elbow_angle: deg(smoothed.trail_elbow_angle),
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use stance::pose::{BBox, Keypoint, PoseFrame, Skeleton};

    /// A right-handed golfer face-on at address, about 300 px tall: the
    /// Halpe-26 keypoints, all scoring 0.9, with the hands at `hands`.
    pub(crate) fn golfer(hands: (f32, f32)) -> Pose {
        let at = |x: f32, y: f32| Keypoint { x, y, score: 0.9 };
        let mut k = vec![at(0.0, 0.0); 26];
        k[0] = at(300.0, 110.0); // nose
        k[1] = at(305.0, 105.0);
        k[2] = at(295.0, 105.0);
        k[3] = at(312.0, 108.0);
        k[4] = at(288.0, 108.0);
        k[5] = at(335.0, 150.0); // left (lead) shoulder
        k[6] = at(265.0, 150.0);
        k[7] = at(330.0, 200.0);
        k[8] = at(270.0, 200.0);
        k[9] = at(hands.0 + 4.0, hands.1); // left wrist
        k[10] = at(hands.0 - 4.0, hands.1);
        k[11] = at(322.0, 250.0); // left hip
        k[12] = at(278.0, 250.0);
        k[13] = at(325.0, 320.0);
        k[14] = at(275.0, 320.0);
        k[15] = at(330.0, 390.0); // left ankle
        k[16] = at(270.0, 390.0);
        k[17] = at(300.0, 85.0); // head
        k[18] = at(300.0, 140.0); // neck
        k[19] = at(300.0, 250.0); // hip centre
        for (i, x) in [(20, 340.0), (21, 260.0), (22, 345.0), (23, 255.0)] {
            k[i] = at(x, 398.0);
        }
        k[24] = at(325.0, 395.0);
        k[25] = at(275.0, 395.0);
        Pose::new(Skeleton::Halpe26, k, BBox::new(240.0, 80.0, 360.0, 400.0))
    }

    /// A face-on swing at 60 fps: still hands at address, a backswing to
    /// the trail side and up, a downswing through the address point, and a
    /// follow-through.
    pub(crate) fn swing_track() -> PoseTrack {
        let dt = 16_666_667u64;
        let mut frames = Vec::new();
        let mut hands = Vec::new();
        // 0.5 s at address.
        hands.extend((0..30).map(|_| (300.0, 260.0)));
        // 0.8 s backswing: up and to the trail side (image left).
        hands.extend((1..=48).map(|i| {
            let s = i as f32 / 48.0;
            (300.0 - 90.0 * s, 260.0 - 200.0 * s)
        }));
        // 0.25 s downswing back to the address point.
        hands.extend((1..=15).map(|i| {
            let s = i as f32 / 15.0;
            (210.0 + 90.0 * s, 60.0 + 200.0 * s)
        }));
        // 0.5 s follow-through, up to the lead side, then still.
        hands.extend((1..=30).map(|i| {
            let s = i as f32 / 30.0;
            (300.0 + 90.0 * s, 260.0 - 200.0 * s)
        }));
        hands.extend((0..30).map(|_| (390.0, 60.0)));
        for (i, h) in hands.into_iter().enumerate() {
            frames.push(PoseFrame::with_pose(i as u64 * dt, &golfer(h)));
        }
        PoseTrack {
            version: stance::pose::POSE_TRACK_VERSION,
            skeleton: Skeleton::Halpe26,
            model: "fake".into(),
            detector: "fake".into(),
            width: 600,
            height: 480,
            frames,
        }
    }

    #[test]
    fn the_cameras_position_picks_the_view() {
        assert_eq!(view_for(CameraPosition::GolferFront), Some(View::FaceOn));
        assert_eq!(
            view_for(CameraPosition::TargetLine),
            Some(View::DownTheLine)
        );
        assert_eq!(view_for(CameraPosition::GolferBack), None);
        assert_eq!(view_for(CameraPosition::Overhead), None);
        assert!(live_setup(CameraPosition::Overhead, None, false).is_none());
        assert_eq!(handed(None), Handed::Right);
        assert_eq!(handed(Some(Handedness::Left)), Handed::Left);
        assert_eq!(handed(Some(Handedness::Right)), Handed::Right);
    }

    fn dir() -> crate::actors::web::tests::TempDir {
        let dir = crate::actors::web::tests::TempDir(std::env::temp_dir().join(format!(
            "flighthook-swing-{}",
            uuid::Uuid::new_v4().simple()
        )));
        std::fs::create_dir_all(&dir.0).expect("dir");
        dir
    }

    #[test]
    fn a_report_is_written_beside_the_clip_with_the_given_hand_impact_and_height() {
        let dir = dir();
        let avi = dir.0.join("camera.0.avi");
        let track = swing_track();
        let impact_ns = 93 * 16_666_667;
        let input = SwingInput {
            position: CameraPosition::GolferFront,
            hand: Some(Handedness::Right),
            impact_ns: Some(impact_ns),
            height_cm: Some(180.0),
            mirrored: false,
        };
        let written = write_report("camera.0", &track, &avi, &input)
            .expect("analysed")
            .expect("face-on gets a report");
        let path = written.path.clone();
        assert_eq!(path, dir.0.join("camera.0.swing.json"));
        let report = SwingReport::read(&path).expect("report");
        assert_eq!(report.view, Some(View::FaceOn));
        assert_eq!(report.handed, Handed::Right);
        assert!(!report.mirrored);
        assert_eq!(report.height_cm, Some(180.0));
        // The given impact is refined to the hands' low point (frame 93),
        // 25 ms after it, and the shift comes back for the log.
        assert_eq!(report.events.impact_source, ImpactSource::Refined);
        let impact = report.events.impact.expect("impact").t_ns;
        assert!(written.swing_found);
        assert_eq!(
            written.impact_shift_ns,
            Some(impact as i64 - impact_ns as i64)
        );
        assert!(impact.abs_diff(impact_ns) < 50_000_000, "{impact}");
        assert!(report.events.top.is_some(), "{:?}", report.events);
        assert!(report.tempo.is_some());
        // The UI reads it.
        let ui: flighthook::SwingReportFile =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).expect("ui");
        assert_eq!(ui.view, Some(SwingView::FaceOn));
        assert!(ui.swing_found());
        assert!(!ui.metrics.is_empty());
        // With its per-frame series, which holds the metric's value at the
        // event's frame.
        assert_eq!(ui.series.t_ns.len(), track.frames.len());
        let top = report.events.top.expect("top").t_ns;
        let lead_arm = ui
            .metrics
            .iter()
            .find(|m| m.id == "lead_arm_angle")
            .expect("lead arm");
        assert_eq!(
            ui.sample("lead_arm_angle", top).map(|s| s.value),
            lead_arm.values.top
        );
        assert!(ui.series.metrics.iter().any(|m| m.id == "hip_turn"));
        // Hip sway, at the top and impact, read the same way.
        let hip_sway = ui
            .metrics
            .iter()
            .find(|m| m.id == "hip_sway")
            .expect("hip sway");
        assert_eq!(
            ui.sample("hip_sway", impact).map(|s| s.value),
            hip_sway.values.impact
        );
        assert!(hip_sway.values.top.is_some() && hip_sway.values.impact.is_some());

        // The left hand is passed through: the report is a left-hander's,
        // and this right-handed swing disagrees with it.
        let left = SwingInput {
            hand: Some(Handedness::Left),
            ..input
        };
        let path = write_report("camera.0", &track, &avi, &left)
            .unwrap()
            .unwrap()
            .path;
        let report = SwingReport::read(&path).unwrap();
        assert_eq!(report.handed, Handed::Left);
        let guess = handed_mismatch(&report, Handed::Left).expect("a confident disagreement");
        assert_eq!(guess.value, Handed::Right);
        // The right hand agrees with the swing: no warning.
        let right = analyze_swing(
            &track,
            &SwingOptions {
                view: Some(View::FaceOn),
                ..SwingOptions::default()
            },
        )
        .unwrap();
        assert!(handed_mismatch(&right, Handed::Right).is_none());
    }

    #[test]
    fn a_camera_behind_or_above_gets_no_report_and_an_empty_track_fails() {
        let dir = dir();
        let avi = dir.0.join("camera.1.avi");
        let input = SwingInput {
            position: CameraPosition::Overhead,
            hand: None,
            impact_ns: None,
            height_cm: None,
            mirrored: false,
        };
        assert_eq!(
            write_report("camera.1", &swing_track(), &avi, &input),
            Ok(None)
        );
        let mut empty = swing_track();
        for f in &mut empty.frames {
            *f = PoseFrame::empty(f.t_ns);
        }
        let input = SwingInput {
            position: CameraPosition::TargetLine,
            ..input
        };
        assert!(write_report("camera.1", &empty, &avi, &input).is_err());
        assert!(!dir.0.join("camera.1.swing.json").exists());
    }

    #[test]
    fn an_impact_with_no_swing_near_it_is_kept_alone_and_not_a_shift() {
        let dir = dir();
        let avi = dir.0.join("camera.0.avi");
        // Given at address, 1 s before the swing's low point.
        let given = 33 * 16_666_667;
        let input = SwingInput {
            position: CameraPosition::GolferFront,
            hand: None,
            impact_ns: Some(given),
            height_cm: None,
            mirrored: false,
        };
        let written = write_report("camera.0", &swing_track(), &avi, &input)
            .unwrap()
            .unwrap();
        assert!(!written.swing_found);
        assert_eq!(written.impact_shift_ns, None);
        let report = SwingReport::read(&written.path).unwrap();
        assert_eq!(report.events.impact_source, ImpactSource::Given);
        assert_eq!(report.events.impact.map(|e| e.t_ns), Some(given));
        assert!(report.events.top.is_none() && report.tempo.is_none());
        let ui: flighthook::SwingReportFile =
            serde_json::from_str(&std::fs::read_to_string(&written.path).unwrap()).unwrap();
        assert!(!ui.swing_found());
    }

    #[test]
    fn a_mirrored_live_picture_measures_the_golfers_true_posture() {
        // A golfer with the lead shoulder raised, and the same golfer in a
        // mirrored picture 600 px wide.
        let mut pose = golfer((300.0, 260.0));
        pose.keypoints[5].y = 135.0;
        let picture = unmirror(&pose, 600.0);
        let fo = |mirrored| live_setup(CameraPosition::GolferFront, None, mirrored).unwrap();
        let true_posture = LivePostureFilter::default().measure(&pose, 600, 0, fo(false), 0.0);
        let unmirrored = LivePostureFilter::default().measure(&picture, 600, 0, fo(true), 0.0);
        let as_is = LivePostureFilter::default().measure(&picture, 600, 0, fo(false), 0.0);
        assert_eq!(unmirrored, true_posture);
        assert!(
            true_posture.shoulder_tilt.unwrap() > 0.0,
            "{true_posture:?}"
        );
        assert_ne!(as_is.shoulder_tilt, true_posture.shoulder_tilt);
        assert!(!mirrored("camera.0"), "no mirror setting yet");
    }

    #[test]
    fn the_live_posture_is_the_views_own_set_and_smoothing_steadies_it() {
        let fo = live_setup(CameraPosition::GolferFront, None, false).unwrap();
        let mut filter = LivePostureFilter::default();
        let p = filter.measure(&golfer((300.0, 260.0)), 600, 0, fo, 0.0);
        assert_eq!(p.view, SwingView::FaceOn);
        assert!(p.spine_angle.is_some() && p.shoulder_tilt.is_some());
        assert!(p.lead_arm_angle.is_some());
        assert!(p.lead_knee_flex.is_none() && p.trail_elbow_angle.is_none());
        let dtl = live_setup(CameraPosition::TargetLine, Some(Handedness::Left), false).unwrap();
        let p = filter.measure(&golfer((300.0, 260.0)), 600, 0, dtl, 0.0);
        assert_eq!(p.view, SwingView::DownTheLine);
        assert!(p.lead_knee_flex.is_some() && p.trail_knee_flex.is_some());
        assert!(p.trail_elbow_angle.is_some());
        assert!(p.shoulder_tilt.is_none() && p.lead_arm_angle.is_none());

        // A shoulder jittering up and down: smoothed, the tilt moves less.
        let jitter = |i: u64| {
            let mut pose = golfer((300.0, 260.0));
            pose.keypoints[5].y = if i.is_multiple_of(2) { 140.0 } else { 160.0 };
            pose
        };
        let spread = |hz: f64| {
            let mut f = LivePostureFilter::default();
            let tilts: Vec<f32> = (0..60u64)
                .map(|i| {
                    f.measure(&jitter(i), 600, i * 66_666_667, fo, hz)
                        .shoulder_tilt
                        .unwrap()
                })
                .collect();
            let tail = &tilts[30..];
            tail.iter().copied().fold(f32::MIN, f32::max)
                - tail.iter().copied().fold(f32::MAX, f32::min)
        };
        assert!(
            spread(1.0) < spread(0.0) * 0.8,
            "{} vs {}",
            spread(1.0),
            spread(0.0)
        );
    }
}
