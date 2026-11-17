//! Tests of the clip routes over a clip in the store: no camera runs.

use axum::http::Method;
use flighthook::{BodyTrackingMode, ShotKey};
use flighthook_cameras::testing::store_clip;

use super::super::tests::{TempDir, test_state};
use super::*;

/// A clip pass without models: the golfer standing at address on every
/// sidecar frame, so the swing analysis has a track to measure.
fn fake_pass(
    avi: &std::path::Path,
    _config: &stance::pose::PoseConfig,
) -> stance::Result<stance::pose::PoseTrack> {
    use stance::pose::{PoseFrame, PoseTrack, Skeleton};
    let name = avi.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let (_, sidecar) = stance::open_clip(avi.parent().expect("folder"), name)?;
    let pose = flighthook_cameras::pose::swing::golfer((300.0, 260.0));
    Ok(PoseTrack {
        version: stance::pose::POSE_TRACK_VERSION,
        skeleton: Skeleton::Halpe26,
        model: "fake".into(),
        detector: "fake".into(),
        width: sidecar.width,
        height: sidecar.height,
        rotation: stance::pose::Rotation::None,
        frames: sidecar
            .frames
            .iter()
            .map(|f| PoseFrame::with_pose(f.t_ns, &pose))
            .collect(),
    })
}

/// A web state with a stored clip from `camera.0` of one shot, body
/// tracking in `mode`.
fn with_clip(mode: BodyTrackingMode) -> (Arc<WebState>, ShotKey, TempDir) {
    let (state, _writer, dir) = test_state();
    state.root.system.update(|c| {
        c.replay.clip_dir = Some(dir.0.join("clips").display().to_string());
        c.body_tracking.mode = mode;
    });
    state.root.cameras.pose.clips.fake(fake_pass);
    let key = ShotKey {
        shot_id: "shot-1".into(),
        shot_number: 1,
    };
    store_clip(&state.root.cameras, std::slice::from_ref(&key));
    (Arc::new(state), key, dir)
}

/// GET of a shot's clip file.
async fn file(state: &Arc<WebState>, shot_id: &str, file: &str) -> Response {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .expect("request");
    get_shot_video_file(
        State(Arc::clone(state)),
        Path((shot_id.to_owned(), file.to_owned())),
        request,
    )
    .await
}

/// POST analyze of `camera`'s clip of the shot: the status and the body.
async fn analyze(state: &Arc<WebState>, shot_id: &str, camera: &str) -> (StatusCode, Vec<u8>) {
    let resp = post_analyze_clip(
        State(Arc::clone(state)),
        Path((shot_id.to_owned(), camera.to_owned())),
        Query(AnalyzeQuery::default()),
    )
    .await;
    read(resp).await
}

/// GET of the grades of `camera`'s clip of the shot.
async fn grades(state: &Arc<WebState>, shot_id: &str, camera: &str) -> (StatusCode, Vec<u8>) {
    let resp = get_shot_grades(
        State(Arc::clone(state)),
        Path((shot_id.to_owned(), camera.to_owned())),
    )
    .await;
    read(resp).await
}

async fn read(resp: Response) -> (StatusCode, Vec<u8>) {
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec();
    (status, body)
}

fn json(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body).unwrap_or_default()
}

/// Wait for the next bus event `want` picks, and check it is the camera's:
/// the web layer finds the clip's row by the card.
async fn next_event(rx: &mut broadcast::Receiver<BusMsg>, want: fn(&LocalEvent) -> bool) {
    let card = tokio::time::timeout(crate::actors::GIVE_UP, async {
        loop {
            let msg = rx.recv().await.expect("the bus");
            if let Some((card, e)) = msg.as_local()
                && want(e)
            {
                return card.to_owned();
            }
        }
    })
    .await
    .expect("the event");
    assert_eq!(card, "camera.0");
}

#[tokio::test]
async fn a_stored_clip_is_served_and_tracked_when_it_is_viewed_and_only_then() {
    let (state, key, _dir) = with_clip(BodyTrackingMode::Historical);
    let id = key.shot_id.as_str();
    let mut rx = state.bus_tx.subscribe();

    // Served as soon as the store has it: the list and the sidecar.
    assert_eq!(state.root.cameras.clips.videos(id).len(), 1);
    let (code, body) = read(file(&state, id, "camera.0.json").await).await;
    assert_eq!(code, StatusCode::OK);
    let sidecar = stance::sidecar::Sidecar::from_json(std::str::from_utf8(&body).expect("utf-8"))
        .expect("sidecar");
    assert!(sidecar.impact_frame().is_some());
    assert_eq!(file(&state, id, "camera.1.json").await.status(), 404);

    // Not tracked on its own: nothing queued, no model asked for, no pose
    // or grades.
    let clips = &state.root.cameras.pose.clips;
    assert!(clips.waiting().is_empty() && !clips.working());
    let model = state.root.system.body_tracking().model;
    assert!(state.root.cameras.pose.models.state(model).is_none());
    assert_eq!(file(&state, id, "camera.0.pose.json").await.status(), 404);
    let (code, body) = grades(&state, id, "camera.0").await;
    assert_eq!(code, StatusCode::NOT_FOUND, "no grades before the analysis");
    assert!(
        json(&body)["error"]
            .as_str()
            .is_some_and(|e| e.contains("camera.0"))
    );

    // Viewed: queued, then tracked at once, its pose, then its swing.
    let (code, body) = analyze(&state, id, "camera.0").await;
    assert_eq!(code, StatusCode::ACCEPTED, "{}", json(&body));
    assert_eq!(json(&body)["state"], "queued");
    next_event(&mut rx, |e| matches!(e, LocalEvent::ShotPose { .. })).await;
    next_event(&mut rx, |e| matches!(e, LocalEvent::ShotSwing { .. })).await;
    assert_eq!(file(&state, id, "camera.0.pose.json").await.status(), 200);
    assert_eq!(file(&state, id, "camera.0.swing.json").await.status(), 200);

    // Graded once analyzed: the clip's shot names no club, so nothing is
    // graded.
    let (code, body) = grades(&state, id, "camera.0").await;
    assert_eq!(code, StatusCode::OK);
    let grades_file: flighthook::BodyGrades = serde_json::from_slice(&body).expect("grades");
    assert_eq!(
        (grades_file.shot_id.as_str(), grades_file.camera.as_str()),
        (id, "camera.0")
    );
    assert!(grades_file.club_class.is_none() && grades_file.references.is_empty());
    assert_eq!(
        grades(&state, id, "camera.7").await.0,
        StatusCode::NOT_FOUND
    );

    // Viewed again: done, nothing queued.
    let (code, body) = analyze(&state, id, "camera.0").await;
    assert_eq!(
        (code, json(&body)["state"].as_str()),
        (StatusCode::OK, Some("done"))
    );
    assert!(clips.waiting().is_empty());

    // No such clip.
    let (code, body) = analyze(&state, id, "camera.7").await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert!(
        json(&body)["error"]
            .as_str()
            .is_some_and(|e| e.contains("camera.7"))
    );
    assert_eq!(
        analyze(&state, "nope", "camera.0").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn analyzing_is_refused_while_body_tracking_is_off_and_the_clip_is_still_served() {
    let (state, key, _dir) = with_clip(BodyTrackingMode::Off);
    let id = key.shot_id.as_str();
    assert_eq!(file(&state, id, "camera.0.avi").await.status(), 200);
    let (code, body) = analyze(&state, id, "camera.0").await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(json(&body)["error"], "body tracking is off");
    let clips = &state.root.cameras.pose.clips;
    assert!(clips.waiting().is_empty() && !clips.working());
    let model = state.root.system.body_tracking().model;
    assert!(
        state.root.cameras.pose.models.state(model).is_none(),
        "no model asked for"
    );
}
