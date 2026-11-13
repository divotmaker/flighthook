//! A shot's clips: the video list and files, the body-metric grades and `POST .../analyze`.

use super::*;

/// GET /api/shots/{shot_id}/video — the shot's finished camera clips.
pub async fn get_shot_video(
    State(state): State<Arc<WebState>>,
    Path(shot_id): Path<String>,
) -> Json<Vec<ShotVideo>> {
    Json(state.root.clips.videos(&shot_id))
}

/// GET /api/shots/{shot_id}/video/{camera}.avi | .json | .pose.json |
/// .swing.json — a camera's clip of the shot (MJPEG AVI), its frame index
/// (the sidecar), the golfer's keypoints on every frame (body tracking) or
/// the swing analysis, with HTTP `Range` support. Only finished clips, and
/// finished body tracking and swing analysis, are served; `404` otherwise. Sent with
/// `Cache-Control: private, no-cache`: a finished file never changes, but a
/// pruned clip's URL must not be served from a cache, and a camera that
/// records the same swing again replaces its files.
pub async fn get_shot_video_file(
    State(state): State<Arc<WebState>>,
    Path((shot_id, file)): Path<(String, String)>,
    request: Request,
) -> Response {
    use tower::ServiceExt as _;

    let path = file
        .strip_suffix(".pose.json")
        .map(|camera| (camera, "pose.json"))
        .or_else(|| {
            file.strip_suffix(".swing.json")
                .map(|camera| (camera, "swing.json"))
        })
        .or_else(|| file.rsplit_once('.'))
        .and_then(|(camera, ext)| state.root.clips.file(&shot_id, camera, ext));
    let Some(path) = path else {
        return not_found(format!("no clip '{file}' for shot '{shot_id}'"));
    };
    match tower_http::services::ServeFile::new(path)
        .oneshot(request)
        .await
    {
        Ok(response) => {
            let mut response = response.map(Body::new);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-cache"),
            );
            response
        }
        Err(never) => match never {},
    }
}

/// GET /api/shots/{shot_id}/grades/{camera} — camera `camera`'s swing
/// numbers of the shot graded against the bands from published tour
/// averages, for every reference ([`flighthook::BodyGrades`]): computed
/// from the clip's swing analysis, the shot's club and the Player card as
/// they are now, never stored. `404` until the swing analysis is done;
/// errors carry `{"error": "..."}`.
pub async fn get_shot_grades(
    State(state): State<Arc<WebState>>,
    Path((shot_id, camera)): Path<(String, String)>,
) -> Response {
    use crate::pose::grades::{GradeInput, grade_file};
    let missing = || {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("no swing analysis from '{camera}' for shot '{shot_id}'")
            })),
        )
            .into_response()
    };
    let Some(path) = state.root.clips.file(&shot_id, &camera, "swing.json") else {
        return missing();
    };
    let (club, hand) = {
        let shots = state.shots.read().await;
        shots
            .iter()
            .rev()
            .find(|s| s.key.shot_id == shot_id)
            .map_or((None, None), |s| (s.data.club_selected(), s.data.handed()))
    };
    let clip_golfer = state
        .root
        .clips
        .camera_clip(&shot_id, &camera)
        .and_then(|c| c.shots.first().and_then(|s| s.golfer.clone()));
    let player = crate::pose::clips::clip_golfer_facts(&state.root, clip_golfer.as_deref());
    let input = GradeInput { club, hand, player };
    let (shot, cam) = (shot_id.clone(), camera.clone());
    match tokio::task::spawn_blocking(move || grade_file(&path, &shot, &cam, input)).await {
        Ok(Some(grades)) => Json(grades).into_response(),
        Ok(None) => missing(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// The query of `POST /api/shots/{shot_id}/video/{camera}/analyze`.
#[derive(Debug, Default, Deserialize)]
pub struct AnalyzeQuery {
    /// `1` (or `true`): delete the clip's body tracking and swing analysis
    /// and track it again.
    pub force: Option<String>,
}

impl AnalyzeQuery {
    /// Whether the clip is tracked again even when it is done.
    pub fn forced(&self) -> bool {
        matches!(self.force.as_deref(), Some("1" | "true"))
    }
}

/// POST /api/shots/{shot_id}/video/{camera}/analyze — track camera
/// `camera`'s clip of the shot now (body tracking, for a viewer), unless it
/// is done or already queued ([`crate::pose::clips::request`]): `202
/// {"state":"queued"}` (its `shot_pose`, then `shot_swing`, follow on the
/// bus), `200 {"state":"done"}` when its keypoints are there, `409` while
/// body tracking is off, `404` when the shot has no clip from the camera;
/// errors carry `{"error": "..."}`. With `?force=1` a clip that is done has
/// its body tracking and swing analysis deleted and is queued again (`202`).
pub async fn post_analyze_clip(
    State(state): State<Arc<WebState>>,
    Path((shot_id, camera)): Path<(String, String)>,
    Query(query): Query<AnalyzeQuery>,
) -> Response {
    use crate::pose::clips::{Requested, request};
    let root = Arc::clone(&state.root);
    let bus = state.bus_tx.clone();
    let (shot, cam) = (shot_id.clone(), camera.clone());
    let force = query.forced();
    if force && state.root.system.body_tracking().mode.is_on() {
        let keys: Option<Vec<flighthook::ShotKey>> = state
            .root
            .clips
            .camera_clip(&shot_id, &camera)
            .map(|clip| clip.shots.into_iter().map(|s| s.key).collect());
        if let Some(keys) = keys {
            super::super::forget_analysis(&state, &keys, &camera).await;
        }
    }
    let requested =
        tokio::task::spawn_blocking(move || request(&root, &shot, &cam, force, &bus)).await;
    let json = |status: StatusCode, body: serde_json::Value| (status, Json(body)).into_response();
    match requested {
        Ok(Requested::Queued) => json(
            StatusCode::ACCEPTED,
            serde_json::json!({ "state": "queued" }),
        ),
        Ok(Requested::Done) => json(StatusCode::OK, serde_json::json!({ "state": "done" })),
        Ok(Requested::Off) => json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": "body tracking is off" }),
        ),
        Ok(Requested::NoClip) => json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("no clip from '{camera}' for shot '{shot_id}'") }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}
