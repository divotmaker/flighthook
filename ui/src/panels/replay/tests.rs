use super::*;
use crate::replay::tests::track;
use crate::types::CameraPosition;

const MS: u64 = 1_000_000;

pub(crate) fn video(camera: &str) -> ShotVideo {
    ShotVideo {
        camera: camera.to_owned(),
        position: CameraPosition::GolferFront,
        duration_ms: 1000,
        fps: 100.0,
        trigger_offset_ms: 500,
        impact_offset_ms: 500,
        complete: true,
        dropped: 0,
        pose: false,
        swing: false,
        rotation: 0,
        analyzed_with: None,
    }
}

pub(crate) fn shot(videos: &[&str]) -> ShotRow {
    ShotRow {
        golfer: None,
        actor: "mock_monitor.0".into(),
        shot_id: "s1".into(),
        shot_number: 7,
        launch: None,
        flight: None,
        club: None,
        impact: None,
        derived: None,
        club_selected: None,
        handed: None,
        group: None,
        finished: true,
        video: videos.iter().map(|c| video(c)).collect(),
    }
}

fn sidecar(view: &ReplayView, camera: &str, t: &Track) -> ReplayReply {
    let json = serde_json::json!({
        "version": 1, "fourcc": "MJPG", "width": t.width, "height": t.height,
        "fps": {"nominal": null, "measured": 100.0},
        "start_ns": t.start_ns,
        "frames": t.frames,
        "trigger_index": 0, "impact_index": 0, "trigger_ns": 0,
        "impact_ns": i64::try_from(t.impact_ns.unwrap() - t.start_ns).unwrap(),
        "complete": true, "dropped": 0, "truncated": false,
    });
    ReplayReply::Sidecar {
        replay: view.seq,
        camera: camera.to_owned(),
        result: stance::sidecar::Sidecar::from_json(&json.to_string()).map_err(|e| e.to_string()),
    }
}

#[test]
fn the_inline_player_fetches_each_swing_analysis_once_and_the_modal_none() {
    let swings = |due: &[Fetch]| {
        due.iter()
            .filter_map(|f| match f {
                Fetch::Swing(c) => Some(c.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let mut row = shot(&["camera.0", "camera.1"]);
    row.video[0].swing = true;
    let mut modal = ReplayView::new(1, &row, str::to_owned, false, 0.2);
    modal.mode = BodyTrackingMode::Historical;
    assert!(swings(&modal.due()).is_empty(), "the modal shows none");
    modal.swing_ready("camera.1");
    assert!(swings(&modal.due()).is_empty());
    let mut inline = ReplayView::new(2, &row, str::to_owned, false, 0.2).with_swing();
    inline.mode = BodyTrackingMode::Historical;
    assert_eq!(
        swings(&inline.due()),
        ["camera.0"],
        "the analyzed clip only"
    );
    assert!(swings(&inline.due()).is_empty(), "once");
    inline.swing_ready("camera.1");
    assert_eq!(swings(&inline.due()), ["camera.1"]);
    // A reply for another player is not taken; a failed one leaves none.
    let report: crate::types::SwingReportFile = serde_json::from_str(
        r#"{"version":1,"setup":{"view":"face_on"},"events":{},"metrics":[]}"#,
    )
    .unwrap();
    inline.reply(ReplayReply::Swing {
        replay: 1,
        camera: "camera.0".into(),
        result: Ok(report.clone()),
    });
    assert!(inline.cameras[0].swing.report.is_none());
    inline.reply(ReplayReply::Swing {
        replay: 2,
        camera: "camera.0".into(),
        result: Ok(report),
    });
    inline.reply(ReplayReply::Swing {
        replay: 2,
        camera: "camera.1".into(),
        result: Err("404".into()),
    });
    assert!(inline.cameras[0].swing.report.is_some());
    assert!(inline.cameras[1].swing.report.is_none());
}

#[test]
fn grades_are_fetched_once_the_report_is_in_and_again_when_the_player_card_changes() {
    let grades = |due: &[Fetch]| {
        due.iter()
            .filter_map(|f| match f {
                Fetch::Grades(c) => Some(c.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let mut row = shot(&["camera.0"]);
    row.video[0].swing = true;
    let mut inline = ReplayView::new(2, &row, str::to_owned, false, 0.2).with_swing();
    inline.mode = BodyTrackingMode::Historical;
    inline.set_grades_key("male, 180");
    assert!(grades(&inline.due()).is_empty(), "the report first");
    let report: crate::types::SwingReportFile =
        serde_json::from_str(r#"{"version":1,"view":"face_on","events":{},"metrics":[]}"#).unwrap();
    inline.reply(ReplayReply::Swing {
        replay: 2,
        camera: "camera.0".into(),
        result: Ok(report),
    });
    assert_eq!(grades(&inline.due()), ["camera.0"]);
    assert!(grades(&inline.due()).is_empty(), "once");
    let body = flighthook::BodyGrades {
        shot_id: "s".into(),
        camera: "camera.0".into(),
        club_class: None,
        sex: flighthook::Sex::Male,
        hand: crate::types::Handedness::Right,
        references: Vec::new(),
    };
    inline.reply(ReplayReply::Grades {
        replay: 2,
        camera: "camera.0".into(),
        result: Ok(body),
    });
    assert!(inline.cameras[0].swing.grades.is_some());
    // The same key: nothing; another (a saved Player card): again,
    // keeping the grades shown until the new ones are in.
    inline.set_grades_key("male, 180");
    assert!(grades(&inline.due()).is_empty());
    inline.set_grades_key("female, 165");
    assert_eq!(grades(&inline.due()), ["camera.0"]);
    assert!(inline.cameras[0].swing.grades.is_some());
    // A failed fetch leaves the numbers plain; tracking again forgets them.
    inline.reply(ReplayReply::Grades {
        replay: 2,
        camera: "camera.0".into(),
        result: Err("404".into()),
    });
    assert!(inline.cameras[0].swing.grades.is_none());
    inline.forget_analysis("camera.0");
    assert!(inline.cameras[0].swing.report.is_none());
    assert!(grades(&inline.due()).is_empty());
}

#[test]
fn a_clip_without_keypoints_is_asked_to_be_tracked_once_unless_tracking_is_off() {
    let asks = |due: &[Fetch]| {
        due.iter()
            .filter_map(|f| match f {
                Fetch::Analyze(c) => Some(c.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let fetched = |due: &[Fetch]| {
        due.iter()
            .filter(|f| matches!(f, Fetch::Pose(_) | Fetch::Swing(_)))
            .count()
    };
    let mut row = shot(&["camera.0", "camera.1"]);
    row.video[1].pose = true;
    row.video[1].swing = true;
    // Off: nothing asked for or fetched, whatever is on disk.
    let mut view = ReplayView::new(1, &row, str::to_owned, false, 0.2).with_swing();
    view.mode = BodyTrackingMode::Off;
    let due = view.due();
    assert!(asks(&due).is_empty());
    assert_eq!(fetched(&due), 0);
    // Historical, and Live alike: the clip without keypoints is asked
    // for once; the tracked one's files are fetched.
    for mode in [BodyTrackingMode::Historical, BodyTrackingMode::Live] {
        let mut view = ReplayView::new(1, &row, str::to_owned, false, 0.2).with_swing();
        view.mode = mode;
        let due = view.due();
        assert_eq!(asks(&due), ["camera.0"], "{mode:?}");
        assert_eq!(fetched(&due), 2, "{mode:?}");
        assert!(asks(&view.due()).is_empty(), "once");
        // Queued: its keypoints follow with shot_pose.
        view.reply(ReplayReply::Analyzed {
            replay: 1,
            camera: "camera.0".into(),
            done: false,
        });
        assert_eq!(fetched(&view.due()), 0);
        view.pose_ready("camera.0");
        assert_eq!(fetched(&view.due()), 1);
    }
    // Asked again (the models were just downloaded): only the clip
    // without keypoints, once.
    let mut view = ReplayView::new(2, &row, str::to_owned, false, 0.2).with_swing();
    view.mode = BodyTrackingMode::Historical;
    assert_eq!(asks(&view.due()), ["camera.0"]);
    assert!(asks(&view.due()).is_empty());
    view.ask_again();
    assert_eq!(asks(&view.due()), ["camera.0"]);
    assert!(asks(&view.due()).is_empty());
    // Done already (the row was behind): fetched at once.
    let mut view = ReplayView::new(3, &row, str::to_owned, false, 0.2).with_swing();
    view.mode = BodyTrackingMode::Historical;
    assert_eq!(asks(&view.due()), ["camera.0"]);
    view.reply(ReplayReply::Analyzed {
        replay: 3,
        camera: "camera.0".into(),
        done: true,
    });
    assert_eq!(fetched(&view.due()), 2, "keypoints and swing");
    // A reply for another view changes nothing.
    let mut other = ReplayView::new(4, &row, str::to_owned, false, 0.2);
    other.mode = BodyTrackingMode::Historical;
    let _ = other.due();
    other.reply(ReplayReply::Analyzed {
        replay: 3,
        camera: "camera.0".into(),
        done: true,
    });
    assert!(!other.cameras[0].video.pose);
}

#[test]
fn a_tracked_clips_keypoints_are_fetched_once_and_drawn_on_the_frame_shown() {
    let t = track(1000, 10, 50, 100);
    let mut row = shot(&["camera.0", "camera.1"]);
    row.video[0].pose = true;
    let mut view = ReplayView::new(1, &row, str::to_owned, false, 0.2);
    view.mode = BodyTrackingMode::Historical;
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let poses = |due: &[Fetch]| {
        due.iter()
            .filter_map(|f| match f {
                Fetch::Pose(c) => Some(c.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(poses(&view.due()), ["camera.0"], "the tracked clip only");
    assert!(poses(&view.due()).is_empty(), "once");
    // The other camera's tracking finishes while the replay is open.
    view.pose_ready("camera.1");
    assert_eq!(poses(&view.due()), ["camera.1"]);
    // Keypoints for every other frame (frame k at k × 10 ms).
    let frames: Vec<serde_json::Value> = t
        .frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            if i % 2 == 0 {
                serde_json::json!({"t_ns": f.t_ns, "keypoints": vec![[i as f32, 1.0, 0.9]; 26]})
            } else {
                serde_json::json!({"t_ns": f.t_ns})
            }
        })
        .collect();
    let file = serde_json::json!({"version": 1, "skeleton": "halpe26", "width": 64,
        "height": 48, "frames": frames});
    view.reply(ReplayReply::Pose {
        replay: 1,
        camera: "camera.0".into(),
        result: serde_json::from_value(file).map_err(|e| e.to_string()),
    });
    let ctx = egui::Context::default();
    let tex = ctx.load_texture(
        "t",
        egui::ColorImage::filled([2, 2], egui::Color32::BLACK),
        egui::TextureOptions::LINEAR,
    );
    let cam = &mut view.cameras[0];
    for (frame, nose_x) in [(10, Some(10.0)), (11, None), (24, Some(24.0))] {
        cam.shown = Some((frame, tex.clone()));
        assert_eq!(
            cam.shown_keypoints().map(|k| k[0][0]),
            nose_x,
            "frame {frame}"
        );
    }
    // No keypoints for a camera whose file is not in.
    view.cameras[1].shown = Some((10, tex));
    assert!(view.cameras[1].shown_keypoints().is_none());
}

/// The centers of the circles `out` painted inside `rect`: a skeleton's
/// joints.
fn joints_in(out: &egui::FullOutput, rect: egui::Rect) -> usize {
    fn count(shape: &egui::Shape, rect: egui::Rect) -> usize {
        match shape {
            egui::Shape::Circle(c) => usize::from(rect.contains(c.center)),
            egui::Shape::Vec(v) => v.iter().map(|s| count(s, rect)).sum(),
            _ => 0,
        }
    }
    out.shapes.iter().map(|c| count(&c.shape, rect)).sum()
}

#[test]
fn a_tracked_clip_shows_its_skeleton_only_while_show_skeleton_is_on() {
    let t = track(1000, 10, 50, 100);
    let mut row = shot(&["camera.0"]);
    row.video[0].pose = true;
    let mut view = ReplayView::new(1, &row, str::to_owned, false, 0.2);
    view.mode = BodyTrackingMode::Historical;
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let frames: Vec<serde_json::Value> = t
        .frames
        .iter()
        .map(|f| serde_json::json!({"t_ns": f.t_ns, "keypoints": vec![[32.0, 24.0, 0.9]; 26]}))
        .collect();
    let file = serde_json::json!({"version": 1, "skeleton": "halpe26", "width": 64,
        "height": 48, "frames": frames});
    view.reply(ReplayReply::Pose {
        replay: 1,
        camera: "camera.0".into(),
        result: serde_json::from_value(file).map_err(|e| e.to_string()),
    });
    let ctx = egui::Context::default();
    let tex = ctx.load_texture(
        "t",
        egui::ColorImage::filled([2, 2], egui::Color32::BLACK),
        egui::TextureOptions::LINEAR,
    );
    let playhead = view.playback.as_ref().expect("playback").playhead;
    let frame = t.frame_at(playhead);
    // The inline player and the replay both draw as the app says.
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let mut config = flighthook::FlighthookConfig::default();
    config.body_tracking.mode = BodyTrackingMode::Historical;
    for show in [true, false, true] {
        config.body_tracking.show_skeleton = show;
        app.settings.load_from(&config);
        let draw = app.pose_draw(crate::types::Handedness::Right);
        assert_eq!(draw.overlay, show);
        view.cameras[0].shown = Some((frame, tex.clone()));
        let (marks, out) = columns_pass_drawing(&ctx, &mut view, Vec::new(), 0.0, draw);
        let picture = marks["player:picture:camera.0"].0;
        assert_eq!(joints_in(&out, picture) > 0, show, "show_skeleton {show}");
    }
}

#[test]
fn a_turned_camera_is_drawn_upright_with_its_skeleton_on_it() {
    let t = track(1000, 10, 50, 100);
    let mut row = shot(&["camera.0"]);
    row.video[0].pose = true;
    let mut view = ReplayView::new(1, &row, str::to_owned, false, 0.2);
    view.mode = BodyTrackingMode::Historical;
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    // Keypoints on the upright 48×64 frame of a 64×48 clip.
    let frames: Vec<serde_json::Value> = t
        .frames
        .iter()
        .map(|f| serde_json::json!({"t_ns": f.t_ns, "keypoints": vec![[40.0, 56.0, 0.9]; 26]}))
        .collect();
    let file = serde_json::json!({"version": 1, "skeleton": "halpe26", "width": 48,
        "height": 64, "rotation": 90, "frames": frames});
    view.reply(ReplayReply::Pose {
        replay: 1,
        camera: "camera.0".into(),
        result: serde_json::from_value(file).map_err(|e| e.to_string()),
    });
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let tex = ctx.load_texture(
        "t",
        egui::ColorImage::filled([64, 48], egui::Color32::BLACK),
        egui::TextureOptions::LINEAR,
    );
    let playhead = view.playback.as_ref().expect("playback").playhead;
    let frame = t.frame_at(playhead);
    let draw = PoseDraw {
        overlay: true,
        min_score: 0.3,
        hand: crate::types::Handedness::Right,
    };
    assert!(
        (view.aspect() - 48.0 / 64.0).abs() < 1e-4,
        "landscape until turned"
    );
    // The camera is turned now: the listing's rotation is what the
    // clip was tracked with, the camera's own is what it is shown with.
    view.set_rotations(|v| {
        if v.camera == "camera.0" {
            90
        } else {
            v.rotation
        }
    });
    assert!((view.aspect() - 64.0 / 48.0).abs() < 1e-4);
    view.cameras[0].shown = Some((frame, tex.clone()));
    let (marks, out) = columns_pass_drawing(&ctx, &mut view, Vec::new(), 0.0, draw);
    let picture = marks["player:picture:camera.0"].0;
    assert!((picture.width() - 300.0).abs() < 0.5);
    assert!((picture.height() / picture.width() - 64.0 / 48.0).abs() < 0.01);
    assert!(
        joints_in(&out, picture) > 0,
        "the skeleton is on the picture"
    );
    // A forced re-analysis drops the keypoints until they come again.
    view.forget_analysis("camera.0");
    assert!(view.cameras[0].shown_keypoints().is_none());
    assert!(!view.cameras[0].video.pose);
    assert!(view.asked_to_track("camera.0"), "the forced ask stands");
}

#[test]
fn a_replay_opens_playing_before_the_impact_once_a_sidecar_is_in() {
    // The clip runs 1000 to 2000 ms with the impact at 1500 ms.
    let t = track(1000, 10, 100, 50);
    let impact = t.impact_ns.unwrap();
    let mut view = ReplayView::new(
        3,
        &shot(&["camera.0", "camera.1"]),
        str::to_owned,
        false,
        0.3,
    );
    assert!(view.playback.is_none());
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let p = view.playback.as_ref().expect("playback");
    assert_eq!(p.playhead, impact - 300_000_000);
    assert!(p.playing);
    // A reply for another replay is dropped.
    let mut stale = sidecar(&view, "camera.1", &t);
    if let ReplayReply::Sidecar { replay, .. } = &mut stale {
        *replay = 2;
    }
    view.reply(stale);
    assert!(view.cameras[1].clip.track.is_none());
    // A failed sidecar shows on its camera.
    view.reply(ReplayReply::Sidecar {
        replay: 3,
        camera: "camera.1".into(),
        result: Err("404 no clip".into()),
    });
    assert_eq!(view.cameras[1].clip.error.as_deref(), Some("404 no clip"));
}

/// The reply carrying the frames of `window` of `t`.
fn frames_reply(view: &ReplayView, camera: &str, t: &Track, window: Window) -> ReplayReply {
    let (a, b) = window.bytes(t);
    ReplayReply::Frames {
        replay: view.seq,
        camera: camera.to_owned(),
        window,
        result: Ok(vec![0; usize::try_from(b - a).unwrap()]),
    }
}

/// A view of `cameras` whose first sidecar is in, opened 0.3 s before the
/// impact of a clip from 1000 to 1990 ms; the sidecars of `loaded` are in.
fn opened(cameras: &[&str], loaded: &[&str]) -> (ReplayView, Track, (u64, u64)) {
    let t = track(1000, 10, 100, 50);
    let span = (t.start_ns, t.end_ns());
    let mut view = ReplayView::new(1, &shot(cameras), str::to_owned, false, 0.3);
    // The frames alone.
    view.mode = BodyTrackingMode::Off;
    for camera in loaded {
        let reply = sidecar(&view, camera, &t);
        view.reply(reply);
    }
    (view, t, span)
}

fn playhead(view: &ReplayView) -> u64 {
    view.playback.as_ref().unwrap().playhead
}

#[test]
fn the_playhead_holds_until_the_start_frames_are_in_then_the_clip_plays() {
    let (mut view, t, span) = opened(&["camera.0", "camera.1"], &["camera.0"]);
    let start = playhead(&view);
    assert!(view.playback.as_ref().unwrap().playing, "shown playing");
    // Neither the other camera's sidecar nor the frames are in: it holds.
    for i in 0..10 {
        view.tick(f64::from(i) * 0.1, Some(span));
        assert_eq!(playhead(&view), start);
    }
    // The other camera's sidecar is in; its frames are not.
    let reply = sidecar(&view, "camera.1", &t);
    view.reply(reply);
    let due = view.due();
    let windows: Vec<_> = due
        .iter()
        .filter_map(|f| match f {
            Fetch::Frames(c, w, _) => Some((c.clone(), *w)),
            Fetch::Sidecar(_)
            | Fetch::Pose(_)
            | Fetch::Swing(_)
            | Fetch::Grades(_)
            | Fetch::Analyze(_) => None,
        })
        .collect();
    assert_eq!(windows.len(), 2, "{due:?}");
    view.tick(1.0, Some(span));
    assert_eq!(playhead(&view), start);
    let (camera, window) = windows[0].clone();
    let reply = frames_reply(&view, &camera, &t, window);
    view.reply(reply);
    view.tick(1.1, Some(span));
    assert_eq!(playhead(&view), start, "one camera is still loading");
    let (camera, window) = windows[1].clone();
    let reply = frames_reply(&view, &camera, &t, window);
    view.reply(reply);
    view.tick(1.2, Some(span));
    assert_eq!(playhead(&view), start, "the clock starts on this update");
    view.tick(1.3, Some(span));
    assert_eq!(playhead(&view) - start, 100 * MS);
    assert!(view.playback.as_ref().unwrap().playing);
    assert_eq!(view.autoplay, Autoplay::Done);
}

#[test]
fn a_slow_camera_does_not_hold_the_start_past_the_timeout() {
    let (mut view, _, span) = opened(&["camera.0"], &["camera.0"]);
    let start = playhead(&view);
    view.tick(10.0, Some(span));
    view.tick(10.0 + AUTOPLAY_TIMEOUT_S - 0.1, Some(span));
    assert_eq!(playhead(&view), start);
    view.tick(10.0 + AUTOPLAY_TIMEOUT_S, Some(span));
    assert_eq!(playhead(&view), start);
    view.tick(10.0 + AUTOPLAY_TIMEOUT_S + 0.1, Some(span));
    let moved = playhead(&view) - start;
    assert!(moved.abs_diff(100 * MS) < 1000, "{moved}");
}

#[test]
fn a_camera_that_failed_does_not_hold_the_start() {
    let (mut view, _, span) = opened(&["camera.0", "camera.1"], &["camera.0"]);
    let start = playhead(&view);
    view.reply(ReplayReply::Sidecar {
        replay: 1,
        camera: "camera.1".into(),
        result: Err("404 no clip".into()),
    });
    // camera.0's frames fail twice.
    for _ in 0..WINDOW_TRIES {
        let due = view.due();
        let window = due
            .iter()
            .find_map(|f| match f {
                Fetch::Frames(_, w, _) => Some(*w),
                Fetch::Sidecar(_)
                | Fetch::Pose(_)
                | Fetch::Swing(_)
                | Fetch::Grades(_)
                | Fetch::Analyze(_) => None,
            })
            .expect("a window");
        view.reply(ReplayReply::Frames {
            replay: 1,
            camera: "camera.0".into(),
            window,
            result: Err("timed out".into()),
        });
    }
    assert!(view.start_frames_in());
    view.tick(0.0, Some(span));
    view.tick(0.1, Some(span));
    assert_eq!(playhead(&view) - start, 100 * MS);
}

#[test]
fn a_pause_step_or_scrub_before_the_start_cancels_the_autoplay() {
    for action in [
        ReplayAction::Toggle,
        ReplayAction::Step(true),
        ReplayAction::Step(false),
        ReplayAction::Seek(1500 * MS),
    ] {
        let (mut view, t, span) = opened(&["camera.0"], &["camera.0"]);
        view.tick(0.0, Some(span));
        view.apply(action, span, std::slice::from_ref(&t));
        let p = view.playback.as_ref().unwrap();
        assert!(!p.playing);
        let held = p.playhead;
        // The frames arriving afterward start nothing.
        let due = view.due();
        let [_, Fetch::Frames(camera, window, _)] = due.as_slice() else {
            panic!("{due:?}");
        };
        let reply = frames_reply(&view, camera, &t, *window);
        view.reply(reply);
        for i in 1..30 {
            view.tick(f64::from(i) * 0.1, Some(span));
        }
        assert_eq!(playhead(&view), held);
        assert!(!view.playback.as_ref().unwrap().playing);
    }
    // Speed and Loop leave the wait alone.
    let (mut view, _, span) = opened(&["camera.0"], &["camera.0"]);
    view.apply(ReplayAction::Speed(Speed::Half), span, &[]);
    view.apply(ReplayAction::Loop(false), span, &[]);
    let p = view.playback.as_ref().unwrap();
    assert!(p.playing && !p.looping && p.speed == Speed::Half);
    assert!(matches!(view.autoplay, Autoplay::Waiting { .. }));
}

#[test]
fn the_opening_position_clamps_to_the_clip_start_and_play_restarts_there() {
    let t = track(1000, 10, 100, 50);
    let span = (t.start_ns, t.end_ns());
    // 2 s before an impact 0.5 s into the clip: the clip start.
    let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 2.0);
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let p = view.playback.as_mut().expect("playback");
    assert_eq!(p.playhead, span.0);
    // Zero is the impact itself.
    let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let p = view.playback.as_mut().expect("playback");
    assert_eq!(p.playhead, t.impact_ns.unwrap());
    // Played to the end with looping off, play restarts at the impact,
    // scrubbing still reaches the clip start.
    p.looping = false;
    assert!(p.playing, "it opened playing");
    p.advance(5.0, span);
    assert_eq!((p.playhead, p.playing), (span.1, false));
    p.toggle(span);
    assert_eq!(p.playhead, t.impact_ns.unwrap());
    p.playing = false;
    p.seek(0, span);
    assert_eq!(p.playhead, span.0);
}

#[test]
fn the_start_tick_is_at_the_start_point_clamped_to_the_clip() {
    let span = (1000 * MS, 3000 * MS);
    let x = |t| tick_x(10.0, 200.0, span, t);
    assert_eq!(x(span.0), 10.0);
    assert_eq!(x(2000 * MS), 110.0);
    assert_eq!(x(span.1), 210.0);
    assert_eq!(x(0), 10.0, "before the clip: its start");
    assert_eq!(x(9000 * MS), 210.0, "after the clip: its end");
    // A replay whose start point clamps to the clip start ticks there.
    let t = track(1000, 10, 100, 50);
    let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 2.0);
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let p = view.playback.as_ref().expect("playback");
    assert_eq!(tick_x(10.0, 200.0, (t.start_ns, t.end_ns()), p.start), 10.0);
    // 0.3 s before the 1.5 s impact is 200 ms into the 990 ms clip.
    let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.3);
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let p = view.playback.as_ref().expect("playback");
    let x = tick_x(0.0, 990.0, (t.start_ns, t.end_ns()), p.start);
    assert!((x - 200.0).abs() < 1e-3, "{x}");
}

#[test]
fn a_replay_loops_by_default_and_the_toggle_turns_it_off() {
    let t = track(1000, 10, 100, 50);
    let span = (t.start_ns, t.end_ns());
    let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.3);
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let p = view.playback.as_mut().expect("playback");
    assert!(p.looping, "on whenever a replay opens");
    let start = p.start;
    assert!(p.playing);
    p.advance(5.0, span);
    assert_eq!((p.playhead, p.playing), (start, true));
    // A new replay is on again, whatever the last one was set to.
    p.looping = false;
    let mut again = ReplayView::new(2, &shot(&["camera.0"]), str::to_owned, false, 0.3);
    let reply = sidecar(&again, "camera.0", &t);
    again.reply(reply);
    assert!(again.playback.as_ref().expect("playback").looping);
}

#[test]
fn frames_are_fetched_once_and_held_after_their_reply() {
    let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
    let t = track(0, 10, 100, 50);
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let frame = t.frame_at(view.playback.as_ref().unwrap().playhead);
    let cam = &mut view.cameras[0].clip;
    let window = cam.wanted(frame).expect("a window to fetch");
    assert!(window.contains(frame));
    cam.in_flight.push(window);
    assert!(cam.wanted(frame).is_none(), "already on its way");
    let (a, b) = window.bytes(&t);
    view.reply(ReplayReply::Frames {
        replay: 1,
        camera: "camera.0".into(),
        window,
        result: Ok(vec![0; usize::try_from(b - a).unwrap()]),
    });
    let cam = &view.cameras[0].clip;
    assert!(cam.in_flight.is_empty());
    assert!(cam.bytes.has(frame));
    assert!(cam.wanted(frame).is_none(), "held");
}

#[test]
fn the_view_asks_for_each_sidecar_once_then_the_frames_at_the_playhead() {
    let mut view = ReplayView::new(
        1,
        &shot(&["camera.0", "camera.1"]),
        str::to_owned,
        false,
        0.0,
    );
    view.mode = BodyTrackingMode::Off;
    assert_eq!(
        view.due(),
        [
            Fetch::Sidecar("camera.0".into()),
            Fetch::Sidecar("camera.1".into())
        ]
    );
    assert!(view.due().is_empty(), "asked once");
    let t = track(0, 10, 100, 50);
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    let due = view.due();
    let [Fetch::Frames(camera, window, bytes)] = due.as_slice() else {
        panic!("{due:?}");
    };
    assert_eq!(camera, "camera.0");
    let frame = t.frame_at(view.playback.as_ref().unwrap().playhead);
    assert!(window.contains(frame));
    assert_eq!(*bytes, window.bytes(&t));
    assert!(view.due().is_empty(), "in flight");
    // Playing, it also asks ahead of the playhead.
    let p = view.playback.as_mut().unwrap();
    p.playing = true;
    p.playhead += 600_000_000;
    let due = view.due();
    assert!(!due.is_empty() && due.len() <= MAX_IN_FLIGHT);
}

#[test]
fn a_failed_window_is_retried_once_then_its_error_is_shown() {
    let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
    view.mode = BodyTrackingMode::Off;
    assert_eq!(view.due(), [Fetch::Sidecar("camera.0".into())]);
    let t = track(0, 10, 100, 50);
    let reply = sidecar(&view, "camera.0", &t);
    view.reply(reply);
    // Paused: only the window at the playhead is asked for.
    view.playback.as_mut().unwrap().playing = false;
    let fail = |view: &mut ReplayView, window| {
        view.reply(ReplayReply::Frames {
            replay: 1,
            camera: "camera.0".into(),
            window,
            result: Err("timed out".into()),
        });
    };
    let due = view.due();
    let [Fetch::Frames(_, window, _)] = due.as_slice() else {
        panic!("{due:?}");
    };
    let window = *window;
    fail(&mut view, window);
    assert_eq!(view.camera_error(0), None, "one failure is retried");
    let due = view.due();
    let [Fetch::Frames(_, again, _)] = due.as_slice() else {
        panic!("{due:?}");
    };
    assert_eq!(*again, window);
    fail(&mut view, window);
    assert_eq!(view.camera_error(0).as_deref(), Some("timed out"));
    assert!(view.due().is_empty(), "not asked for a third time");
    // Elsewhere in the clip the camera still loads and shows.
    let p = view.playback.as_mut().unwrap();
    p.playhead = t.time(99);
    assert_eq!(view.camera_error(0), None);
    let due = view.due();
    let [Fetch::Frames(_, other, bytes)] = due.as_slice() else {
        panic!("{due:?}");
    };
    assert!(other.contains(99));
    let (a, b) = *bytes;
    view.reply(ReplayReply::Frames {
        replay: 1,
        camera: "camera.0".into(),
        window: *other,
        result: Ok(vec![0; usize::try_from(b - a).unwrap()]),
    });
    assert_eq!(view.camera_error(0), None);
    // A clip that didn't load shows why at any playhead.
    view.cameras[0].clip.error = Some("404 no clip".into());
    assert_eq!(view.camera_error(0).as_deref(), Some("404 no clip"));
}

/// A clip of `n` test-pattern frames 10 ms apart from 1000 ms (impact
/// half way), and the bytes of the file it indexes.
fn jpeg_clip(n: u64) -> (Track, Vec<u8>) {
    jpeg_clip_at(n, 10)
}

/// A clip of `n` test-pattern frames `period` ms apart from 1000 ms
/// (impact half way), and the bytes of the file it indexes.
fn jpeg_clip_at(n: u64, period: u64) -> (Track, Vec<u8>) {
    use stance::{Source, TestPattern, TestPatternConfig};
    let mut pattern = TestPattern::new(TestPatternConfig {
        width: 64,
        height: 48,
        paced: false,
        ..TestPatternConfig::default()
    })
    .unwrap();
    let mut file = vec![0; 232];
    let mut t = track(1000, period, n, 1);
    for f in &mut t.frames {
        let jpeg = pattern
            .next_frame(std::time::Duration::ZERO)
            .unwrap()
            .unwrap()
            .jpeg;
        f.offset = file.len() as u64;
        f.len = u32::try_from(jpeg.len()).unwrap();
        file.extend_from_slice(&jpeg);
        file.extend_from_slice(&[0; 8]);
    }
    (t, file)
}

/// The card's reply to each window it asks for, from `file`.
fn serve(card: &mut CardClip, t: &Track, file: &[u8]) -> usize {
    let due = card.due();
    let mut served = 0;
    for fetch in due {
        if let Fetch::Frames(camera, window, (a, b)) = fetch {
            assert_eq!(window.bytes(t), (a, b));
            card.reply(ReplayReply::Frames {
                replay: card.seq,
                camera,
                window,
                result: Ok(file[a as usize..b as usize].to_vec()),
            });
            served += 1;
        }
    }
    served
}

#[test]
fn a_camera_card_loops_from_the_start_point_with_frames_cut_to_fill_it() {
    let ctx = egui::Context::default();
    let (t, file) = jpeg_clip(40);
    let mut card = CardClip::new(9, "s1", "camera.0", 0.1);
    assert_eq!(card.due(), [Fetch::Sidecar("camera.0".into())]);
    assert!(card.due().is_empty(), "asked once");
    assert!(!card.decode_next(&ctx));
    assert!(card.frame_texture().is_none());
    let view = ReplayView::new(9, &shot(&["camera.0"]), str::to_owned, false, 0.0);
    let reply = sidecar(&view, "camera.0", &t);
    card.reply(reply);
    // From 0.1 s before the impact to the clip end.
    let (start, end) = card.loop_span().expect("a loop");
    assert_eq!(start, t.impact_ns.unwrap() - 100 * MS);
    assert_eq!(end, t.end_ns() + 10 * MS, "the last frame's end");
    // A tall card: the 64x48 frames are cut to its aspect.
    card.resize([40, 60]);
    assert!(card.keeps_every_frame());
    assert!(card.next_due().is_none(), "nothing on screen yet");
    assert!(serve(&mut card, &t, &file) > 0);
    assert!(card.frame_texture().is_none(), "not decoded yet");
    assert!(card.decode_next(&ctx));
    let tex = card.frame_texture().expect("the start frame");
    assert_eq!(tex.size(), [40, 60]);
    assert!(card.running, "the clock starts with the start frame");
    assert_eq!(card.playback.as_ref().unwrap().playhead, start);
    // 1x from here, one repaint per frame.
    card.tick(5.0);
    card.tick(5.0625);
    let p = card.playback.as_ref().unwrap();
    assert_eq!(p.playhead, start + 62_500_000);
    // The next frame (1170 ms) is due in 7.5 ms, sooner than a 60 Hz
    // display shows it: the repaint waits a display frame.
    let due = card.next_due().expect("playing");
    assert!((due - CARD_REPAINT_MIN_S).abs() < 1e-9, "{due}");
    // The last frame shows until the loop's end, then the start point.
    let p = card.playback.as_mut().unwrap();
    p.playhead = t.end_ns() + 2 * MS;
    assert_eq!(t.frame_at(p.playhead), 39);
    let due = card.next_due().expect("playing");
    assert!((due - CARD_REPAINT_MIN_S).abs() < 1e-9, "{due}");
    card.tick(5.5);
    assert_eq!(card.playback.as_ref().unwrap().playhead, start);
    // Every frame decoded once is kept: no window is asked for again.
    for step in 0..60 {
        card.tick(5.4 + f64::from(step) * 0.01);
        serve(&mut card, &t, &file);
        card.decode_next(&ctx);
        card.frame_texture();
    }
    assert_eq!(serve(&mut card, &t, &file), 0);
    assert_eq!(card.textures.len(), card.loop_frames());
}

#[test]
fn a_camera_cards_decoded_frames_are_bounded() {
    let (t, _) = jpeg_clip(40);
    let mut card = CardClip::new(1, "s1", "camera.0", 0.1);
    let view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
    let reply = sidecar(&view, "camera.0", &t);
    card.reply(reply);
    // 30 frames from the start point (1100 ms) to the end.
    assert_eq!(card.loop_frames(), 30);
    card.resize([400, 400]);
    assert!(card.keeps_every_frame(), "30 x 640 kB");
    assert_eq!(card.textures.capacity(), 30);
    // 48 MiB holds 30 frames of 646 x 646, not of 648 x 648: past it,
    // every other frame (15 of them).
    const { assert!(30 * 646 * 646 * 4 <= CARD_TEXTURE_BYTES) };
    const { assert!(30 * 648 * 648 * 4 > CARD_TEXTURE_BYTES) };
    card.resize([646, 646]);
    assert!(card.keeps_every_frame());
    card.resize([648, 648]);
    assert!(!card.keeps_every_frame());
    assert_eq!(card.stride(), 2);
    assert_eq!(card.textures.capacity(), 15);
    // Another reply's seq is not this card's.
    let mut other = sidecar(&view, "camera.0", &t);
    if let ReplayReply::Sidecar { replay, .. } = &mut other {
        *replay = 2;
    }
    let mut fresh = CardClip::new(1, "s1", "camera.0", 0.1);
    fresh.reply(other);
    assert!(fresh.loop_span().is_none());
}

/// A card of `camera` (seq `seq`) with the sidecar of `t` in and every
/// frame's bytes held.
fn loaded_card(seq: u64, camera: &str, t: &Track, file: &[u8]) -> CardClip {
    let mut card = CardClip::new(seq, "s1", camera, 0.1);
    let view = ReplayView::new(seq, &shot(&[camera]), str::to_owned, false, 0.0);
    card.reply(sidecar(&view, camera, t));
    card.resize([32, 24]);
    // Walk the loop so every window is asked for and served.
    for step in 0..t.frames.len() {
        if let Some(p) = card.playback.as_mut() {
            p.playhead = t.time(step);
        }
        serve(&mut card, t, file);
    }
    if let Some(p) = card.playback.as_mut() {
        p.playhead = p.start;
    }
    card
}

#[test]
fn a_card_decodes_its_loop_in_order_ahead_of_the_playhead() {
    let ctx = egui::Context::default();
    let (t, file) = jpeg_clip(40);
    let mut card = loaded_card(1, "camera.0", &t, &file);
    assert!(card.keeps_every_frame());
    let start = t.frame_at(card.playback.as_ref().unwrap().start);
    // The frame at the playhead first, then on through the loop.
    let mut order = Vec::new();
    while let Some(frame) = card.next_to_decode() {
        order.push(frame);
        assert!(card.decode_next(&ctx));
    }
    assert_eq!(order, (start..40).collect::<Vec<_>>());
    // With the playhead further on, from there, wrapping to the start.
    let mut card = loaded_card(1, "camera.0", &t, &file);
    card.playback.as_mut().unwrap().playhead = t.time(30);
    let mut order = Vec::new();
    while let Some(frame) = card.next_to_decode() {
        order.push(frame);
        assert!(card.decode_next(&ctx));
    }
    let want: Vec<usize> = (30..40).chain(start..30).collect();
    assert_eq!(order, want);
    // Past the texture budget, with a loop longer than the replay
    // view's window: every kept frame the same way, from the kept frame
    // at the playhead.
    let (t, file) = jpeg_clip(80);
    let mut card = loaded_card(1, "camera.0", &t, &file);
    card.resize([648, 648]);
    let start = t.frame_at(card.playback.as_ref().unwrap().start);
    assert!(card.loop_frames() > TEXTURES);
    assert_eq!((start, card.loop_frames(), card.stride()), (30, 50, 2));
    card.playback.as_mut().unwrap().playhead = t.time(41);
    let mut order = Vec::new();
    while let Some(frame) = card.next_to_decode() {
        order.push(frame);
        assert!(card.decode_next(&ctx));
    }
    let want: Vec<usize> = (40..80).step_by(2).chain((30..40).step_by(2)).collect();
    assert_eq!(order, want);
    assert_eq!(card.textures.len(), 25);
}

#[test]
fn a_cards_stride_is_the_smallest_that_fits_and_a_resize_recomputes_it() {
    // A 556-frame loop (frames 244..800).
    let t = track(1000, 1, 800, 1);
    let view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
    let mut card = CardClip::new(1, "s1", "camera.0", 0.156);
    card.reply(sidecar(&view, "camera.0", &t));
    assert_eq!(card.loop_frames(), 556);
    // At 182 x 144 the loop is 58 MB; 48 MiB holds 480 of its frames,
    // so every other frame: 278.
    card.resize([182, 144]);
    const { assert!(556 * 182 * 144 * 4 > CARD_TEXTURE_BYTES) };
    const { assert!(480 * 182 * 144 * 4 <= CARD_TEXTURE_BYTES) };
    const { assert!(481 * 182 * 144 * 4 > CARD_TEXTURE_BYTES) };
    assert_eq!(card.stride(), 2);
    assert_eq!(card.textures.capacity(), 278);
    assert_eq!(card.kept_at(100), Some(244));
    assert_eq!(card.kept_at(245), Some(244));
    assert_eq!(card.kept_at(799), Some(798));
    // A smaller card keeps every frame; a larger one a longer stride,
    // the smallest that fits.
    card.resize([160, 128]);
    const { assert!(556 * 160 * 128 * 4 <= CARD_TEXTURE_BYTES) };
    assert!(card.keeps_every_frame());
    assert_eq!(card.textures.capacity(), 556);
    card.resize([400, 300]);
    let frame = 400 * 300 * 4;
    let stride = card.stride();
    assert_eq!(stride, 6);
    assert!(556_usize.div_ceil(stride) * frame <= CARD_TEXTURE_BYTES);
    assert!(556_usize.div_ceil(stride - 1) * frame > CARD_TEXTURE_BYTES);
    assert_eq!(card.textures.capacity(), 93);
}

/// Frame index shown by `card`.
fn shown_frame(card: &CardClip) -> Option<usize> {
    card.shown.as_ref().map(|(f, _)| *f)
}

#[test]
fn a_long_fast_loop_past_the_budget_plays_through_its_frames() {
    let ctx = egui::Context::default();
    // 250 fps, 400 frames: a 225-frame loop from 0.1 s before the
    // impact, longer than the replay view's texture window.
    let (t, file) = jpeg_clip_at(400, 4);
    let mut card = loaded_card(1, "camera.0", &t, &file);
    card.resize([400, 400]);
    assert_eq!(card.loop_frames(), 225);
    const { assert!(225 * 400 * 400 * 4 > CARD_TEXTURE_BYTES) };
    assert!(!card.keeps_every_frame());
    let start = t.frame_at(card.playback.as_ref().unwrap().start);
    // Screen updates at 60 Hz for 4 s: the playhead moves about four
    // frames each.
    let mut shown = Vec::new();
    for step in 0..240_u32 {
        card.tick(5.0 + f64::from(step) / 60.0);
        serve(&mut card, &t, &file);
        card.frame_texture();
        shown.extend(shown_frame(&card));
        card.decode_next(&ctx);
    }
    assert!(card.running, "the start frame came up");
    let changes = shown.windows(2).filter(|w| w[0] != w[1]).count();
    assert!(
        changes * 10 >= shown.len() * 9,
        "the picture changes nearly every update: {changes} of {}: {shown:?}",
        shown.len()
    );
    // Forward through the loop, back to its start at the end.
    for w in shown.windows(2) {
        assert!(
            w[1] >= w[0] || w[1] < start + 8,
            "{} after {}: {shown:?}",
            w[1],
            w[0]
        );
    }
    let distinct: std::collections::BTreeSet<_> = shown.iter().collect();
    assert!(distinct.len() > 40, "{distinct:?}");
    // Only kept frames, every third, all within the budget.
    assert_eq!(card.stride(), 3);
    assert!(shown.iter().all(|f| (f - start) % 3 == 0), "{shown:?}");
    assert!(card.textures.len() <= card.capacity());
    assert_eq!(card.capacity(), 75);
}

#[test]
fn the_cards_decode_one_frame_per_screen_update_between_them() {
    let ctx = egui::Context::default();
    let (t, file) = jpeg_clip(40);
    let mut app = FlighthookApp::with_pending(net::new_pending());
    for (seq, camera) in [(1, "camera.0"), (2, "camera.1"), (3, "camera.2")] {
        let card = loaded_card(seq, camera, &t, &file);
        app.card_clips.insert(camera.to_owned(), card);
    }
    let decoded = |app: &FlighthookApp| -> Vec<usize> {
        ["camera.0", "camera.1", "camera.2"]
            .map(|c| app.card_clips[c].textures.len())
            .to_vec()
    };
    // One decode per update across all cards, taking turns.
    for n in 1..=9_usize {
        app.decode_card_frame(&ctx);
        let counts = decoded(&app);
        assert_eq!(counts.iter().sum::<usize>(), n, "{counts:?}");
        assert!(counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1);
    }
    assert_eq!(decoded(&app), [3, 3, 3]);
    // A card with nothing left to decode doesn't take a turn.
    let loop_frames = app.card_clips["camera.0"].loop_frames();
    for _ in 0..3 * loop_frames {
        app.decode_card_frame(&ctx);
    }
    assert_eq!(decoded(&app), [loop_frames; 3]);
    app.decode_card_frame(&ctx);
    assert_eq!(decoded(&app), [loop_frames; 3]);
}

#[test]
fn replies_reach_the_card_or_the_replay_their_seq_names() {
    let (t, _) = jpeg_clip(40);
    let mut app = FlighthookApp::with_pending(net::new_pending());
    let row = shot(&["camera.0"]);
    app.card_clips
        .insert("camera.0".into(), CardClip::new(4, "s1", "camera.0", 0.1));
    app.replay = Some(ReplayView::new(
        7,
        &ShotRow {
            shot_id: "other".into(),
            ..row.clone()
        },
        str::to_owned,
        false,
        0.0,
    ));
    let tagged = |seq: u64| {
        let view = ReplayView::new(seq, &row, str::to_owned, false, 0.0);
        sidecar(&view, "camera.0", &t)
    };
    // The card's seq, while a replay with another is open: the card.
    app.pending.lock().unwrap().replay.push(tagged(4));
    app.apply_pending();
    assert!(app.card_clips["camera.0"].loop_span().is_some());
    assert!(app.replay.as_ref().unwrap().tracks().is_empty());
    // The replay's seq: the replay.
    app.pending.lock().unwrap().replay.push(tagged(7));
    app.apply_pending();
    assert_eq!(app.replay.as_ref().unwrap().tracks().len(), 1);
    // A retired seq (a card since replaced): nowhere.
    app.card_clips
        .insert("camera.0".into(), CardClip::new(5, "s1", "camera.0", 0.1));
    app.pending.lock().unwrap().replay.push(tagged(4));
    app.apply_pending();
    assert!(app.card_clips["camera.0"].loop_span().is_none());
    assert_eq!(app.replay.as_ref().unwrap().tracks().len(), 1);
}

#[test]
fn the_texture_cache_is_smaller_on_a_small_screen() {
    let view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, true, 0.0);
    assert_eq!(view.textures.len(), 0);
    assert_eq!(view.textures.capacity(), TEXTURES_SMALL);
    let view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
    assert_eq!(view.textures.capacity(), TEXTURES);
}

// -----------------------------------------------------------------------
// Swing events on the scrubber
// -----------------------------------------------------------------------

/// A swing report with `events` (name, ms after the clip start) and its
/// impact source (`refined` found a swing, `given` did not).
fn swing_report(events: &[(&str, u64)], source: &str) -> crate::types::SwingReportFile {
    let mut map = serde_json::Map::new();
    for (name, ms) in events {
        map.insert(
            (*name).to_owned(),
            serde_json::json!({"t_ns": ms * MS, "frame": 0, "confidence": 0.9}),
        );
    }
    map.insert("impact_source".into(), source.into());
    serde_json::from_value(serde_json::json!({
        "version": 1, "view": "face_on", "handed": "right", "events": map, "metrics": []
    }))
    .expect("report")
}

fn names(markers: &[Marker]) -> Vec<&'static str> {
    markers.iter().map(|m| m.0).collect()
}

/// Each event sits at the mean of the times the cameras found it at, on
/// the shot timeline (a camera's clip start plus its event time); one
/// camera's time stands alone; an event no camera found has no marker;
/// impact is the gold tick's, and a camera that found no swing counts
/// for nothing.
#[test]
fn a_marker_is_the_mean_of_the_cameras_that_found_the_event() {
    let a = swing_report(
        &[
            ("address", 100),
            ("takeaway", 150),
            ("top", 600),
            ("impact", 750),
            ("finish", 900),
        ],
        "refined",
    );
    let b = swing_report(&[("address", 120), ("top", 640)], "refined");
    let nothing = swing_report(&[("address", 5000), ("impact", 750)], "given");

    // Two cameras (clips starting 1000 and 1010 ms), an event missing
    // from one, and a third that found no swing.
    let markers = swing_markers([(1000 * MS, &a), (1010 * MS, &b), (1000 * MS, &nothing)]);
    assert_eq!(
        markers,
        [
            ("Address", 1115 * MS),
            ("Takeaway", 1150 * MS),
            ("Top", 1625 * MS),
            ("Finish", 1900 * MS),
        ]
    );
    // One camera: its own times, in swing order.
    let one = swing_markers([(2000 * MS, &b)]);
    assert_eq!(one, [("Address", 2120 * MS), ("Top", 2640 * MS)]);
    // A mean of times 1 ns apart rounds to the nearest ns.
    let mut c = swing_report(&[("top", 0)], "refined");
    let mut d = c.clone();
    c.events.top = Some(flighthook::SwingEvent {
        t_ns: 10,
        confidence: 1.0,
    });
    d.events.top = Some(flighthook::SwingEvent {
        t_ns: 11,
        confidence: 1.0,
    });
    assert_eq!(swing_markers([(0, &c), (0, &d)]), [("Top", 11)]);
    // No reports, or none that found a swing: no markers.
    assert!(swing_markers(std::iter::empty()).is_empty());
    assert!(swing_markers([(0, &nothing)]).is_empty());
    let empty = swing_report(&[], "refined");
    assert!(swing_markers([(0, &empty)]).is_empty());
}

/// Markers a few pixels apart share one spot, named in swing order and
/// seeking to the first; ones further apart stay apart.
#[test]
fn close_markers_merge_into_one_spot_that_names_both() {
    let markers = [
        ("Address", 1000 * MS),
        ("Takeaway", 1010 * MS),
        ("Top", 1600 * MS),
        ("Finish", 1900 * MS),
    ];
    // 1 px per 10 ms: address and takeaway are 1 px apart.
    let x_of = |t: u64| t as f32 / (10.0 * MS as f32);
    let groups = marker_groups(&markers, x_of);
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].0, ["Address", "Takeaway"]);
    assert_eq!(groups[0].1, 1000 * MS);
    assert_eq!(groups[1].0, ["Top"]);
    assert_eq!(groups[2].0, ["Finish"]);
    // 10 px per 10 ms: all four apart.
    let wide = marker_groups(&markers, |t| t as f32 / MS as f32);
    assert_eq!(wide.len(), 4);
    assert!(marker_groups(&[], x_of).is_empty());
}

/// One pass of `view`'s controls on a 800 wide screen, marked under
/// `player`; the playhead after the pass.
fn controls_pass(
    ctx: &egui::Context,
    view: &mut ReplayView,
    events: Vec<egui::Event>,
    time: f64,
) -> (crate::panels::devices::Marks, Vec<String>) {
    use crate::panels::devices::Marks;
    ctx.data_mut(|d| d.remove::<Marks>(egui::Id::NULL));
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 200.0),
        )),
        time: Some(time),
        events,
        ..Default::default()
    };
    let pending = net::new_pending();
    let out = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let timeline = view.prepare(ctx, &pending, true, BodyTrackingMode::Historical);
            let action = view.controls(ui, "player", &timeline, false);
            view.act(action, &timeline);
        });
    });
    let marks = ctx
        .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
        .unwrap_or_default();
    let mut texts = Vec::new();
    fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
            _ => {}
        }
    }
    for clipped in &out.shapes {
        collect(&clipped.shape, &mut texts);
    }
    (marks, texts)
}

/// A view of two cameras with the clips from 1000 to 1990 ms (the
/// second a 20 ms later start), each with its swing report.
fn view_with_swings() -> ReplayView {
    let mut view = ReplayView::new(
        1,
        &shot(&["camera.0", "camera.1"]),
        str::to_owned,
        false,
        0.3,
    );
    view.mode = BodyTrackingMode::Historical;
    let (first, second) = (track(1000, 10, 100, 50), track(1020, 10, 100, 50));
    for (camera, t) in [("camera.0", &first), ("camera.1", &second)] {
        let reply = sidecar(&view, camera, t);
        view.reply(reply);
    }
    view
}

/// In the portrait layout the transport buttons are touch-sized and
/// Loop wraps under them, inside a phone's width.
#[test]
fn portrait_controls_are_touch_sized_with_loop_on_a_second_row() {
    use crate::panels::devices::Marks;
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut view = view_with_swings();
    let pending = net::new_pending();
    let mut marks = Marks::default();
    for time in [0.0, 0.1] {
        ctx.data_mut(|d| d.remove::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(390.0, 400.0),
            )),
            time: Some(time),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let timeline = view.prepare(ctx, &pending, true, BodyTrackingMode::Historical);
                let action = view.controls(ui, "player", &timeline, true);
                view.act(action, &timeline);
            });
        });
        marks = ctx
            .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default();
    }
    let play = marks["player:play"].0;
    let looping = marks["player:loop"].0;
    let speed = marks["player:speed"].0;
    assert_eq!(play.size(), egui::vec2(44.0, 44.0));
    assert_eq!(looping.height(), 44.0);
    assert!(looping.top() >= play.bottom(), "{looping:?} under {play:?}");
    assert!(speed.right() <= 390.0, "{speed:?}");
}

/// The scrubber carries a marker for each event the reports found, at
/// the mean time, none for impact, hover names the event, and a click
/// seeks to it.
#[test]
fn the_scrubber_marks_the_events_names_them_on_hover_and_seeks_on_a_click() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut view = view_with_swings();
    let events = [("address", 100), ("takeaway", 150), ("top", 500)];
    view.cameras[0].swing.report = Some(Arc::new(swing_report(&events, "refined")));
    let mut other = events.to_vec();
    other.pop();
    other.push(("finish", 800));
    view.cameras[1].swing.report = Some(Arc::new(swing_report(&other, "refined")));

    controls_pass(&ctx, &mut view, Vec::new(), 0.0);
    let (marks, _) = controls_pass(&ctx, &mut view, Vec::new(), 0.1);
    let mut found: Vec<&str> = marks
        .keys()
        .filter_map(|k| k.strip_prefix("player:event:"))
        .collect();
    found.sort_unstable();
    assert_eq!(found, ["address", "finish", "takeaway", "top"]);
    assert!(!marks.contains_key("player:event:impact"), "the gold tick");

    // The scrubber spans 1000 to 2010 ms; the track is inset 8 px.
    let scrubber = marks["player:scrubber"].0;
    let (left, width) = (scrubber.left() + 8.0, scrubber.width() - 16.0);
    let at = |ms: f32| left + (ms - 1000.0) / 1010.0 * width;
    let x = |name: &str| marks[&format!("player:event:{name}")].0.center().x;
    // Address: the mean of 1000+100 and 1020+100 = 1110 ms. Top: one
    // camera, 1000+500. Finish: one camera, 1020+800.
    assert!((x("address") - at(1110.0)).abs() < 0.6, "{}", x("address"));
    assert!((x("top") - at(1500.0)).abs() < 0.6);
    assert!((x("finish") - at(1820.0)).abs() < 0.6);
    assert!(x("address") < x("takeaway") && x("takeaway") < x("top"));
    for name in ["address", "takeaway", "top", "finish"] {
        let rect = marks[&format!("player:event:{name}")].0;
        assert!(rect.height() >= scrubber.height() - 0.5, "{name}");
    }

    // Hover names the event and nothing else.
    let top = marks["player:event:top"].0.center();
    let mut texts = Vec::new();
    for (i, t) in [0.0, 0.3, 1.2, 1.4].into_iter().enumerate() {
        let moved = if i == 0 {
            vec![egui::Event::PointerMoved(top)]
        } else {
            Vec::new()
        };
        texts = controls_pass(&ctx, &mut view, moved, 2.0 + t).1;
    }
    assert!(texts.iter().any(|t| t == "Top"), "{texts:?}");
    for other in ["Address", "Takeaway", "Finish", "Impact"] {
        assert!(!texts.iter().any(|t| t == other), "{other}: {texts:?}");
    }

    // A click seeks there.
    let button = |pressed| egui::Event::PointerButton {
        pos: top,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    controls_pass(&ctx, &mut view, vec![button(true)], 3.0);
    controls_pass(&ctx, &mut view, vec![button(false)], 3.1);
    assert_eq!(playhead(&view), 1500 * MS);
}

/// No marker without a swing report that found a swing, nor with body
/// tracking off.
#[test]
fn no_markers_without_a_report_that_found_a_swing() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut view = view_with_swings();
    let has_events = |marks: &crate::panels::devices::Marks| {
        marks.keys().any(|k| k.starts_with("player:event:"))
    };
    let pass = |view: &mut ReplayView| {
        controls_pass(&ctx, view, Vec::new(), 0.0);
        controls_pass(&ctx, view, Vec::new(), 0.1).0
    };
    // Still analyzing, failed, or not fetched: no reports.
    let marks = pass(&mut view);
    assert!(marks.contains_key("player:scrubber"));
    assert!(!has_events(&marks));
    // A report with no swing found: none.
    view.cameras[0].swing.report = Some(Arc::new(swing_report(
        &[("address", 100), ("top", 500)],
        "given",
    )));
    assert!(!has_events(&pass(&mut view)));
    // One that found a swing: markers; and not in Off.
    view.cameras[0].swing.report = Some(Arc::new(swing_report(
        &[("address", 100), ("top", 500)],
        "refined",
    )));
    assert!(has_events(&pass(&mut view)));
    assert_eq!(names(&view.swing_markers()), ["Address", "Top"]);
    view.mode = BodyTrackingMode::Off;
    assert!(view.swing_markers().is_empty());
}

/// One pass of `view`'s columns, 300 px each, on a 800×400 screen,
/// without skeletons.
fn columns_pass(
    ctx: &egui::Context,
    view: &mut ReplayView,
    events: Vec<egui::Event>,
    time: f64,
) -> (crate::panels::devices::Marks, egui::FullOutput) {
    let draw = PoseDraw {
        overlay: false,
        min_score: 0.3,
        hand: crate::types::Handedness::Right,
    };
    columns_pass_drawing(ctx, view, events, time, draw)
}

/// One pass of `view`'s columns as [`columns_pass`], skeletons drawn as
/// `draw` says.
fn columns_pass_drawing(
    ctx: &egui::Context,
    view: &mut ReplayView,
    events: Vec<egui::Event>,
    time: f64,
    draw: PoseDraw,
) -> (crate::panels::devices::Marks, egui::FullOutput) {
    use crate::panels::devices::Marks;
    ctx.data_mut(|d| d.remove::<Marks>(egui::Id::NULL));
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 400.0),
        )),
        time: Some(time),
        events,
        ..Default::default()
    };
    let out = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            view.columns(ui, "player", (300.0, 12.0), false, draw);
        });
    });
    let marks = ctx
        .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
        .unwrap_or_default();
    (marks, out)
}

/// Each camera's title row ends in a download icon right beside the
/// title, not a link at the far right; it says "Download" on hover and a
/// click opens the clip's URL.
#[test]
fn a_download_icon_follows_each_camera_title_and_opens_the_clip() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut view = ReplayView::new(
        1,
        &shot(&["camera.0", "camera.1"]),
        |id| format!("Cam {id}"),
        false,
        0.3,
    );
    columns_pass(&ctx, &mut view, Vec::new(), 0.0);
    let (marks, _) = columns_pass(&ctx, &mut view, Vec::new(), 0.1);
    for camera in ["camera.0", "camera.1"] {
        let icon = marks[&format!("player:download:{camera}")].0;
        let picture = marks[&format!("player:picture:{camera}")].0;
        assert!(icon.bottom() <= picture.top() + 0.5, "{camera}: title row");
        assert!(
            icon.left() > picture.left() && icon.left() - picture.left() < 200.0,
            "{camera}: beside the title, not at the far right: {icon:?}"
        );
        assert!(
            icon.right() < picture.right() - 40.0,
            "{camera}: not at the right edge"
        );
        assert!((icon.width() - icon.height()).abs() < 0.5, "an icon");
    }

    // Hover: just "Download".
    let over = marks["player:download:camera.0"].0.center();
    let mut shapes = Vec::new();
    for (i, t) in [1.0, 1.3, 2.2, 2.4].into_iter().enumerate() {
        let moved = if i == 0 {
            vec![egui::Event::PointerMoved(over)]
        } else {
            Vec::new()
        };
        shapes = columns_pass(&ctx, &mut view, moved, t).1.shapes;
    }
    let mut texts = Vec::new();
    fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
            _ => {}
        }
    }
    for clipped in &shapes {
        collect(&clipped.shape, &mut texts);
    }
    assert!(texts.iter().any(|t| t == "Download"), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("Download ")), "{texts:?}");

    // A click opens the clip.
    let button = |pressed| egui::Event::PointerButton {
        pos: over,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    columns_pass(&ctx, &mut view, vec![button(true)], 3.0);
    let (_, out) = columns_pass(&ctx, &mut view, vec![button(false)], 3.1);
    let opened: Vec<String> = out
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(opened, [net::clip_url("s1", "camera.0", "avi")]);
}
