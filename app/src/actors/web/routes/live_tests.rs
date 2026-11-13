//! Tests of the camera live view route.

use std::time::Instant;

use super::super::tests::test_state;
use super::*;
use flighthook::CameraSection;

/// A web state with one configured camera, `camera.0`.
fn with_camera() -> (WebState, super::super::tests::TempDir) {
    let (state, _writer, dir) = test_state();
    state.root.system.update(|c| {
        c.camera.insert(
            "0".into(),
            CameraSection {
                name: "Face".into(),
                camera: CameraSection::TEST_PATTERN.into(),
                ..CameraSection::default()
            },
        );
    });
    (state, dir)
}

fn jpeg() -> Arc<[u8]> {
    Arc::from(vec![0xFF, 0xD8, 0xFF, 0xD9].as_slice())
}

async fn body(resp: Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec()
}

#[tokio::test]
async fn a_newer_frame_is_answered_at_once_with_its_number_and_time() {
    let (state, _dir) = with_camera();
    // Watched (the camera only puts frames then) before the frame came.
    state.root.cameras.watch_live("camera.0", Instant::now());
    let seq = state
        .root
        .cameras
        .set_live("camera.0", 123_456, jpeg(), None);
    let started = Instant::now();
    let resp = get_camera_live_with(&state, "camera.0", 0, Duration::from_secs(5)).await;
    assert!(started.elapsed() < Duration::from_secs(1), "no wait");
    assert_eq!(resp.status(), StatusCode::OK);
    let headers = resp.headers().clone();
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers["x-frame-seq"], seq.to_string().as_str());
    assert_eq!(headers["x-frame-time-ns"], "123456");
    assert_eq!(body(resp).await, jpeg().to_vec());
}

#[tokio::test]
async fn a_live_frame_carries_its_pose_and_body_trackings_state() {
    use flighthook::BodyTrackingMode;
    let (state, _dir) = with_camera();
    state
        .root
        .system
        .update(|c| c.body_tracking.mode = BodyTrackingMode::Live);
    state.root.cameras.watch_live("camera.0", Instant::now());
    let pose: Arc<str> = Arc::from(r#"{"skeleton":"halpe26","keypoints":[],"ms":4.0}"#);
    let seq = state
        .root
        .cameras
        .set_live("camera.0", 5, jpeg(), Some(Arc::clone(&pose)));
    let resp = get_camera_live_with(&state, "camera.0", 0, Duration::from_secs(5)).await;
    let headers = resp.headers().clone();
    assert_eq!(headers["x-frame-seq"], seq.to_string().as_str());
    assert_eq!(headers["x-pose"], &*pose);
    // Nothing asked for the models yet.
    assert_eq!(headers["x-pose-state"], "loading");
    assert!(!headers.contains_key("x-pose-error"));
    // A frame without a pose has no X-Pose; out of Live mode, off says
    // so.
    let mut seq = 0;
    for mode in [BodyTrackingMode::Historical, BodyTrackingMode::Off] {
        state.root.system.update(|c| c.body_tracking.mode = mode);
        seq = state.root.cameras.set_live("camera.0", 6, jpeg(), None);
        let resp = get_camera_live_with(&state, "camera.0", seq - 1, Duration::from_secs(5)).await;
        assert!(!resp.headers().contains_key("x-pose"));
        assert_eq!(resp.headers()["x-pose-state"], "off", "{mode:?}");
    }
    // The 204 carries the state too: missing (the test models folder is
    // empty), downloading while a download runs, then a failed load and
    // why (the stand-in files are no models).
    state
        .root
        .system
        .update(|c| c.body_tracking.mode = BodyTrackingMode::Live);
    let model = state.root.system.body_tracking().model;
    let mut headers = Vec::new();
    for want in ["missing", "downloading", "failed"] {
        if want == "downloading" {
            state.root.pose.models.download(model);
        }
        let mut seen = None;
        for _ in 0..300 {
            state.root.pose.models.want(model);
            let resp =
                get_camera_live_with(&state, "camera.0", seq, Duration::from_millis(1)).await;
            assert_eq!(resp.status(), StatusCode::NO_CONTENT);
            if resp.headers()["x-pose-state"] == want {
                seen = Some(resp.headers().clone());
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        headers.push(seen.unwrap_or_else(|| panic!("never {want}")));
    }
    assert!(!headers[0].contains_key("x-pose-error"));
    let error = headers[2]["x-pose-error"].to_str().expect("ascii");
    assert!(error.contains("model"), "{error}");
}

#[tokio::test]
async fn nothing_newer_in_time_is_a_204() {
    let (state, _dir) = with_camera();
    state.root.cameras.watch_live("camera.0", Instant::now());
    let seq = state.root.cameras.set_live("camera.0", 1, jpeg(), None);
    let wait = Duration::from_millis(80);
    let started = Instant::now();
    let resp = get_camera_live_with(&state, "camera.0", seq, wait).await;
    assert!(started.elapsed() >= wait);
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
    assert!(body(resp).await.is_empty());
}

#[tokio::test]
async fn a_frame_arriving_while_it_waits_is_answered() {
    let (state, _dir) = with_camera();
    let state = Arc::new(state);
    let root = Arc::clone(&state.root);
    let put = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        root.cameras.set_live("camera.0", 9, jpeg(), None)
    });
    let resp = get_camera_live(
        State(Arc::clone(&state)),
        Path("camera.0".into()),
        Query(LiveQuery { after: 0 }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let seq = put.await.expect("put").to_string();
    assert_eq!(resp.headers()["x-frame-seq"], seq.as_str());
}

#[tokio::test]
async fn an_unknown_actor_is_a_404() {
    let (state, _dir) = with_camera();
    for actor in ["camera.1", "mevo.0", "camera", "webserver.0"] {
        let resp = get_camera_live_with(&state, actor, 0, Duration::from_secs(5)).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{actor}");
        assert!(
            !state.root.cameras.live_wanted(actor, Instant::now()),
            "{actor}: no slot is watched"
        );
    }
}

#[tokio::test]
async fn a_request_keeps_the_live_slot_updated_only_for_a_while() {
    let (state, _dir) = with_camera();
    let cams = &state.root.cameras;
    assert!(!cams.live_wanted("camera.0", Instant::now()));
    let asked = Instant::now();
    let resp = get_camera_live_with(&state, "camera.0", 0, Duration::from_millis(1)).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(cams.live_wanted("camera.0", Instant::now()));
    let lapsed = asked + crate::state::cameras::LIVE_WATCH + Duration::from_millis(50);
    assert!(!cams.live_wanted("camera.0", lapsed));
}

#[tokio::test]
async fn too_many_waiting_requests_are_refused() {
    let (state, _dir) = with_camera();
    let state = Arc::new(state);
    let waits: Vec<_> = (0..crate::state::cameras::LIVE_WAITERS)
        .map(|_| {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                get_camera_live_with(&state, "camera.0", 0, Duration::from_secs(5))
                    .await
                    .status()
            })
        })
        .collect();
    let cams = &state.root.cameras;
    for _ in 0..400 {
        if cams.live_waiters("camera.0") == crate::state::cameras::LIVE_WAITERS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let resp = get_camera_live_with(&state, "camera.0", 0, Duration::from_secs(5)).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    state.root.cameras.set_live("camera.0", 1, jpeg(), None);
    for wait in waits {
        assert_eq!(wait.await.expect("wait"), StatusCode::OK);
    }
}
