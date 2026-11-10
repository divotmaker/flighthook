//! REST endpoint handlers.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::broadcast;

use super::types::{
    AutoChipRequest, AutoChipResponse, ClubRequest, FindMatRequest, GroupingRequest, HandRequest,
    ModeRequest, PostSettingsResponse, ShotGroupsResponse, StatusResponse,
};
use super::{MAX_SHOTS, StoredShot, WebState, export};
use crate::actors::actor_names;
use crate::state::config::FlighthookConfig;
use flighthook::{
    CameraListResponse, Club, ClubInfo, ConfigAction, FlighthookEvent, FlighthookMessage, HandInfo,
    ShotData, ShotVideo, UnitSystem,
};

// ---------------------------------------------------------------------------
// Embedded UI assets (built by `make ui` in flighthook/ui/)
// ---------------------------------------------------------------------------

const UI_HTML: &str = include_str!("../../../../ui/dist/index.html");
const UI_JS: &[u8] = include_bytes!("../../../../ui/dist/flighthook-ui.js");
const UI_WASM: &[u8] = include_bytes!("../../../../ui/dist/flighthook-ui_bg.wasm");
const UI_FAVICON_ICO: &[u8] = include_bytes!("../../../../ui/dist/favicon.ico");
const UI_FAVICON_PNG: &[u8] = include_bytes!("../../../../ui/dist/favicon-32.png");
const UI_APPLE_TOUCH_ICON: &[u8] = include_bytes!("../../../../ui/dist/apple-touch-icon.png");

/// GET / — serve the egui dashboard HTML.
pub async fn get_ui_html() -> Response {
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        )
        .body(Body::from(UI_HTML))
        .unwrap()
}

/// GET /flighthook-ui.js — serve the WASM JS glue.
pub async fn get_ui_js() -> Response {
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript"),
        )
        .body(Body::from(UI_JS))
        .unwrap()
}

/// GET /flighthook-ui_bg.wasm — serve the WASM binary.
pub async fn get_ui_wasm() -> Response {
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/wasm"),
        )
        .body(Body::from(UI_WASM))
        .unwrap()
}

/// GET /favicon.ico — the browser tab icon.
pub async fn get_favicon_ico() -> Response {
    icon_response("image/x-icon", UI_FAVICON_ICO)
}

/// GET /favicon-32.png — the tab icon for browsers that prefer PNG.
pub async fn get_favicon_png() -> Response {
    icon_response("image/png", UI_FAVICON_PNG)
}

/// GET /apple-touch-icon.png — the home-screen icon on iOS.
pub async fn get_apple_touch_icon() -> Response {
    icon_response("image/png", UI_APPLE_TOUCH_ICON)
}

fn icon_response(content_type: &'static str, body: &'static [u8]) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .body(Body::from(body))
        .unwrap()
}

// ---------------------------------------------------------------------------
// REST API
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ShotsQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub units: Option<String>,
}

fn default_limit() -> usize {
    50
}

/// GET /api/status
pub async fn get_status(State(state): State<Arc<WebState>>) -> Json<StatusResponse> {
    // Names from the config as it is now: a rename shows before the bus
    // event that applied it reaches the cache.
    let names = actor_names(&state.root.system.snapshot());
    let actors = super::actor_statuses(&*state.actors.read().await, &names, state.now_ms());

    let game = state.root.game.snapshot();
    let clip_dir = crate::state::clips::clip_dir(&state.root.system.replay());
    Json(StatusResponse {
        actors,
        mode: game.mode,
        club: game.club_info.map(|c| c.club),
        handed: game.handed,
        cameras_supported: crate::state::cameras::supported(),
        clip_dir: Some(clip_dir.display().to_string()),
        config_error: state.root.system.load_error(),
        round: state.root.round.state(),
    })
}

/// GET /api/cameras — the cameras this computer has, and the test picture.
///
/// Enumerates on a blocking thread, at most every few seconds (the result is
/// reused meanwhile). A camera a camera actor records from is not opened
/// again to list it and is marked `in_use_by`. `500` with a text body when
/// the enumeration thread fails.
pub async fn get_cameras(State(state): State<Arc<WebState>>) -> Response {
    let root = Arc::clone(&state.root);
    match tokio::task::spawn_blocking(move || root.cameras.list()).await {
        Ok(list) => Json::<CameraListResponse>(list).into_response(),
        Err(e) => {
            tracing::warn!("cameras: listing failed: {e}");
            text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("listing the cameras failed: {e}"),
            )
        }
    }
}

/// A response with status `status` and a short text body.
fn text_response(status: StatusCode, text: String) -> Response {
    Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(Body::from(text))
        .unwrap()
}

/// A `404` with a short text body.
fn not_found(text: String) -> Response {
    text_response(StatusCode::NOT_FOUND, text)
}

/// GET /api/cameras/{actor}/still — the camera actor's newest frame, a JPEG
/// as the camera sent it (at most half a second old while it records).
/// `404` when the actor has no frame.
pub async fn get_camera_still(
    State(state): State<Arc<WebState>>,
    Path(actor): Path<String>,
) -> Response {
    let Some(jpeg) = state.root.cameras.still(&actor) else {
        return not_found(format!("no frame from '{actor}'"));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"))
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .body(Body::from(jpeg.to_vec()))
        .unwrap()
}

/// Query of `GET /api/cameras/{actor}/live`.
#[derive(Debug, Deserialize)]
pub struct LiveQuery {
    /// The sequence number of the frame the viewer has; 0 for none.
    #[serde(default)]
    after: u64,
}

/// GET /api/cameras/{actor}/live?after=<seq> — the camera's live frame
/// newer than `after`, a JPEG as the camera sent it, with its sequence
/// number (`X-Frame-Seq`) and time (`X-Frame-Time-Ns`, see
/// [`LiveFrame::time_ns`](crate::state::cameras::LiveFrame)), and the
/// golfer's keypoints on that frame (`X-Pose`, compact JSON) when body
/// tracking ran on it. Waits up to
/// [`LIVE_WAIT`](crate::state::cameras::LIVE_WAIT) for one, then `204`.
/// Both carry body tracking's state (`X-Pose-State`: `off`, `loading`,
/// `downloading`, `ready` or `failed`, the failure in `X-Pose-Error`).
/// Each request keeps the camera's live slot updated for the next
/// [`LIVE_WATCH`](crate::state::cameras::LIVE_WATCH). `404` when `actor` is
/// not a configured camera; `503` when the camera already has
/// [`LIVE_WAITERS`](crate::state::cameras::LIVE_WAITERS) requests waiting.
pub async fn get_camera_live(
    State(state): State<Arc<WebState>>,
    Path(actor): Path<String>,
    Query(query): Query<LiveQuery>,
) -> Response {
    get_camera_live_with(
        &state,
        &actor,
        query.after,
        crate::state::cameras::LIVE_WAIT,
    )
    .await
}

/// [`get_camera_live`] waiting at most `wait`.
async fn get_camera_live_with(
    state: &WebState,
    actor: &str,
    after: u64,
    wait: Duration,
) -> Response {
    use crate::state::cameras::LiveWait;
    let configured = actor
        .strip_prefix("camera.")
        .is_some_and(|index| state.root.system.camera(index).is_some());
    if !configured {
        return not_found(format!("no camera '{actor}'"));
    }
    let cameras = &state.root.cameras;
    cameras.watch_live(actor, std::time::Instant::now());
    let answer = cameras.next_live(actor, after, wait).await;
    let pose_state = || {
        let settings = state.root.system.body_tracking();
        state.root.pose.live_state(&settings, actor)
    };
    match answer {
        LiveWait::Frame(frame) => {
            let mut builder = Response::builder()
                .header(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"))
                .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
                .header("X-Frame-Seq", frame.seq)
                .header("X-Frame-Time-Ns", frame.time_ns);
            if let Some(pose) = frame.pose.as_deref() {
                match HeaderValue::from_str(pose) {
                    Ok(value) => builder = builder.header("X-Pose", value),
                    Err(_) => {
                        tracing::debug!("{actor}: live pose left out, not a valid header: {pose}")
                    }
                }
            }
            pose_headers(builder, pose_state())
                .body(Body::from(frame.jpeg.to_vec()))
                .unwrap()
        }
        LiveWait::Timeout => {
            let builder = Response::builder()
                .status(StatusCode::NO_CONTENT)
                .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            pose_headers(builder, pose_state())
                .body(Body::empty())
                .unwrap()
        }
        LiveWait::Busy => text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("too many live requests waiting on '{actor}'"),
        ),
    }
}

/// Add body tracking's `state` (`X-Pose-State`) and its error, if any
/// (`X-Pose-Error`, printable ASCII, at most 300 characters).
fn pose_headers(
    builder: axum::http::response::Builder,
    (state, error): (&'static str, Option<String>),
) -> axum::http::response::Builder {
    let builder = builder.header("X-Pose-State", state);
    let Some(error) = error else {
        return builder;
    };
    let text: String = error
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .take(300)
        .collect();
    match HeaderValue::from_str(&text) {
        Ok(value) => builder.header("X-Pose-Error", value),
        Err(_) => builder,
    }
}

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
            .map_or((None, None), |s| {
                (
                    s.data.club_selected.or(s.context.club_selected),
                    s.data.handed.or(s.context.handed),
                )
            })
    };
    let input = GradeInput {
        club,
        hand,
        player: state.root.system.player(),
    };
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
            super::forget_analysis(&state, &keys, &camera).await;
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

/// GET /api/body_tracking/models — the state of the files the configured
/// pose model needs: `missing`, `downloading` (with the download's
/// progress), `ready` or `failed` (with why), the files, their hosts and
/// sizes, and the models folder. Checking a file hashes it the first time
/// (a few hundred ms); later calls only look at its size and time.
pub async fn get_body_tracking_models(State(state): State<Arc<WebState>>) -> Response {
    let root = Arc::clone(&state.root);
    match tokio::task::spawn_blocking(move || {
        root.pose.models.files(root.system.body_tracking().model)
    })
    .await
    {
        Ok(files) => Json(files).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/body_tracking/models/download — download the files the
/// configured pose model lacks, from their official sources: `202` with
/// the state when a download starts or is already running (one at a time;
/// asking again starts nothing), `200` with the state when every file is
/// there already. The only way flighthook downloads a model.
pub async fn post_body_tracking_models_download(State(state): State<Arc<WebState>>) -> Response {
    use crate::pose::models::DownloadStart;
    let root = Arc::clone(&state.root);
    let answered = tokio::task::spawn_blocking(move || {
        let model = root.system.body_tracking().model;
        let started = root.pose.models.download(model);
        (started, root.pose.models.files(model))
    })
    .await;
    match answered {
        Ok((DownloadStart::Ready, files)) => (StatusCode::OK, Json(files)).into_response(),
        Ok((DownloadStart::Started | DownloadStart::Running, files)) => {
            (StatusCode::ACCEPTED, Json(files)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// DELETE /api/body_tracking/models/download — stop the download running,
/// if any; its partial file is deleted. `200` with the state either way.
pub async fn delete_body_tracking_models_download(State(state): State<Arc<WebState>>) -> Response {
    let root = Arc::clone(&state.root);
    let answered = tokio::task::spawn_blocking(move || {
        root.pose.models.cancel_download();
        root.pose.models.files(root.system.body_tracking().model)
    })
    .await;
    match answered {
        Ok(files) => Json(files).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/shots?limit=50&units=imperial|metric
///
/// Shots the device produced no data for are kept for the export but not
/// listed here.
pub async fn get_shots(
    State(state): State<Arc<WebState>>,
    Query(query): Query<ShotsQuery>,
) -> Json<Vec<ShotData>> {
    let shots = state.shots.read().await;
    let unit_system = query.units.as_deref().and_then(parse_units);
    let mut result: Vec<ShotData> = shots
        .iter()
        .rev()
        .filter(|s| s.data.has_data())
        .take(query.limit)
        .map(|s| match unit_system {
            Some(system) => s.data.to_unit_system(system),
            None => s.data.clone(),
        })
        .collect();
    result.reverse();
    Json(result)
}

fn parse_units(u: &str) -> Option<UnitSystem> {
    match u {
        "imperial" => Some(UnitSystem::Imperial),
        "metric" => Some(UnitSystem::Metric),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Shot export
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ExportQuery {
    /// `jsonl` (default) or `csv`.
    pub format: Option<String>,
    /// `imperial` or `metric`. JSONL defaults to native units, CSV to the
    /// configured `default_units`.
    pub units: Option<String>,
    /// Most recent N shots, 1..=1000. Default: all stored.
    pub limit: Option<usize>,
    /// JSONL only: include raw frames. Default true.
    pub raw: Option<bool>,
}

fn bad_request(msg: impl Into<Body>) -> Response {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(msg.into())
        .unwrap()
}

/// GET /api/shots/export?format=jsonl|csv&units=imperial|metric&limit=N&raw=true|false
///
/// Downloads the session's shots: JSONL with the raw device frames behind
/// each shot, or CSV without them. One shot per line, streamed.
pub async fn get_shots_export(
    State(state): State<Arc<WebState>>,
    Query(query): Query<ExportQuery>,
) -> Response {
    let csv = match query.format.as_deref().unwrap_or("jsonl") {
        "jsonl" => false,
        "csv" => true,
        _ => return bad_request("format must be jsonl or csv"),
    };
    let units = match query.units.as_deref() {
        None => None,
        Some(u) => match parse_units(u) {
            Some(units) => Some(units),
            None => return bad_request("units must be imperial or metric"),
        },
    };
    if query.limit.is_some_and(|n| n == 0 || n > MAX_SHOTS) {
        return bad_request("limit must be 1..=1000");
    }
    let include_raw = query.raw.unwrap_or(true);

    let shots: Vec<StoredShot> = {
        let shots = state.shots.read().await;
        let start = shots
            .len()
            .saturating_sub(query.limit.unwrap_or(shots.len()));
        shots.iter().skip(start).cloned().collect()
    };

    let now = chrono::Utc::now();
    let exported_at = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let ext = if csv { "csv" } else { "jsonl" };
    let filename = format!("flighthook-shots-{}.{ext}", now.format("%Y%m%d-%H%M%S"));

    let (content_type, body) = if csv {
        let system = units.unwrap_or(state.root.system.snapshot().default_units);
        let rows = std::iter::once(export::csv_header(system)).chain(
            shots
                .into_iter()
                .map(move |s| export::to_csv_row(&s, system)),
        );
        (
            "text/csv; charset=utf-8",
            Body::from_stream(futures_util::stream::iter(
                rows.map(Ok::<_, std::convert::Infallible>),
            )),
        )
    } else {
        let lines = shots
            .into_iter()
            .map(move |s| export::jsonl_line(&s, &exported_at, units, include_raw));
        (
            "application/x-ndjson",
            Body::from_stream(futures_util::stream::iter(
                lines.map(Ok::<_, std::convert::Infallible>),
            )),
        )
    };

    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .body(body)
        .unwrap()
}

// ---------------------------------------------------------------------------
// Shot conversion utility
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ConvertQuery {
    pub units: String,
}

/// POST /api/shots/convert?units=imperial|metric
///
/// Stateless unit conversion utility for WebSocket consumers. Accepts a
/// `ShotData` body (as received on the WS) and returns it with all distance
/// and velocity fields converted to the requested unit system.
pub async fn post_convert_shot(
    Query(query): Query<ConvertQuery>,
    Json(shot): Json<ShotData>,
) -> Result<Json<ShotData>, StatusCode> {
    let system = match query.units.as_str() {
        "imperial" => UnitSystem::Imperial,
        "metric" => UnitSystem::Metric,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    // Camera clips stay with the server's history.
    let mut shot = shot.to_unit_system(system);
    shot.video = Vec::new();
    Ok(Json(shot))
}

/// POST /api/mode
pub async fn post_mode(
    State(state): State<Arc<WebState>>,
    Json(body): Json<ModeRequest>,
) -> StatusCode {
    let mode = body.mode;
    let _ = state.bus_tx.send(
        FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
            mode: Some(mode),
            handed: None,
            club: None,
        })
        .actor("web"),
    );
    StatusCode::ACCEPTED
}

/// POST /api/club — select the club in play.
///
/// Emits `club_info` on the bus (actor `web`); the system actor stores it and
/// derives the detection mode from it, as for a club from GSPro. A connected
/// GSPro replaces the selection with its next player message.
pub async fn post_club(
    State(state): State<Arc<WebState>>,
    Json(body): Json<ClubRequest>,
) -> Response {
    let Some(club) = Club::from_code(&body.club) else {
        let codes: Vec<String> = Club::ALL.iter().map(ToString::to_string).collect();
        return bad_request(format!("club must be one of {}", codes.join(", ")));
    };
    let _ = state.bus_tx.send(
        FlighthookMessage::new(FlighthookEvent::ClubInfo {
            club_info: ClubInfo { club },
        })
        .actor("web"),
    );
    StatusCode::ACCEPTED.into_response()
}

/// POST /api/hand — set the player's hand.
///
/// Emits `hand_info` on the bus (actor `web`); the system actor stores it and,
/// when the hand changed, tells the devices, as for a hand from GSPro. A
/// connected GSPro replaces it with the hand of its next player message.
/// 400 with a text body for anything but `{"handed":"rh"|"lh"}`.
pub async fn post_hand(
    State(state): State<Arc<WebState>>,
    body: Result<Json<HandRequest>, JsonRejection>,
) -> Response {
    let Some(hand_info) = body
        .ok()
        .and_then(|Json(body)| HandInfo::from_code(&body.handed))
    else {
        return bad_request(r#"body must be {"handed":"rh"} or {"handed":"lh"}"#);
    };
    let _ = state
        .bus_tx
        .send(FlighthookMessage::new(FlighthookEvent::HandInfo { hand_info }).actor("web"));
    StatusCode::ACCEPTED.into_response()
}

/// POST /api/find-mat — have the ExPutt `actor` locate its mat again.
///
/// Emits `find_mat` on the bus (actor `web`); the ExPutt actor acts on it
/// while it is measuring. 404 with a text body when no ExPutt has that ID.
pub async fn post_find_mat(
    State(state): State<Arc<WebState>>,
    Json(body): Json<FindMatRequest>,
) -> Response {
    let configured = body
        .actor
        .strip_prefix("exputt.")
        .is_some_and(|index| state.root.system.snapshot().exputt.contains_key(index));
    if !configured {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )
            .body(Body::from(format!("no ExPutt with ID '{}'", body.actor)))
            .unwrap();
    }
    let _ = state
        .bus_tx
        .send(FlighthookMessage::new(FlighthookEvent::FindMat { target: body.actor }).actor("web"));
    StatusCode::ACCEPTED.into_response()
}

/// GET /api/shots/groups — this webserver's shot sets, the current session
/// and the default view.
pub async fn get_shot_groups(State(state): State<Arc<WebState>>) -> Json<ShotGroupsResponse> {
    let mode = state.root.system.snapshot().shot_grouping;
    let groups = state.groups.read().await;
    Json(ShotGroupsResponse {
        server: state.actor_id.clone(),
        mode,
        session: groups.session(),
        current: groups.current().map(|g| g.id.clone()),
        groups: groups.list(),
    })
}

/// POST /api/auto-chip — send a short, straight chip of `yards` total
/// (one of [`flighthook::AUTO_CHIP_YARDS`]) as the next Auto-chip shot.
///
/// The ball speed is solved against the live `[simulation]` environment and
/// surface ([`crate::autochip`]); the shot goes on the bus as actor
/// [`flighthook::AUTO_CHIP_ACTOR`], so every output forwards it and it is
/// stored like a device's shot. 409 when Auto-chip is disabled in the
/// settings (`[simulation] auto_chip`), a tournament round is active (the
/// round policy turns Auto-chip off) or the simulation environment is
/// invalid, 400 for any other distance; all with a JSON `{"error"}` body.
pub async fn post_auto_chip(
    State(state): State<Arc<WebState>>,
    Json(body): Json<AutoChipRequest>,
) -> Result<Json<AutoChipResponse>, SettingsError> {
    if !state.root.system.simulation().auto_chip {
        return Err(settings_error(
            StatusCode::CONFLICT,
            "Auto-chip is disabled in settings",
        ));
    }
    let yards = body.yards;
    if !flighthook::AUTO_CHIP_YARDS.contains(&yards) {
        let allowed: Vec<String> = flighthook::AUTO_CHIP_YARDS
            .iter()
            .map(ToString::to_string)
            .collect();
        return Err(settings_error(
            StatusCode::BAD_REQUEST,
            &format!("yards must be one of {}", allowed.join(", ")),
        ));
    }
    let chip_state = Arc::clone(&state);
    let sent = tokio::task::spawn_blocking(move || {
        crate::autochip::send(
            &chip_state.root,
            &chip_state.bus_tx,
            yards,
            &chip_state.auto_chip_shots,
        )
    })
    .await
    .map_err(|_| settings_error(StatusCode::INTERNAL_SERVER_ERROR, "auto-chip failed"))?;
    match sent {
        Ok((key, ball)) => Ok(Json(AutoChipResponse {
            actor: flighthook::AUTO_CHIP_ACTOR.to_owned(),
            shot_id: key.shot_id,
            shot_number: key.shot_number,
            ball_speed_mph: ball.launch_speed.map_or(0.0, |v| v.as_mph()),
            launch_elevation: ball.launch_elevation.unwrap_or_default(),
            backspin_rpm: ball.backspin_rpm.unwrap_or_default(),
        })),
        Err(e) => Err(settings_error(StatusCode::CONFLICT, &e.to_string())),
    }
}

/// POST /api/shots/groups — start a new set for the selected club.
pub async fn post_new_shot_group(State(state): State<Arc<WebState>>) -> StatusCode {
    let _ = state
        .bus_tx
        .send(FlighthookMessage::new(FlighthookEvent::NewShotGroup).actor("web"));
    StatusCode::ACCEPTED
}

/// POST /api/shots/session — close the open set and start a new session.
pub async fn post_new_shot_session(State(state): State<Arc<WebState>>) -> StatusCode {
    let _ = state
        .bus_tx
        .send(FlighthookMessage::new(FlighthookEvent::NewShotSession).actor("web"));
    StatusCode::ACCEPTED
}

/// POST /api/shots/clear — delete the shot history and every set, and start
/// a new session.
pub async fn post_clear_shot_history(State(state): State<Arc<WebState>>) -> StatusCode {
    let _ = state
        .bus_tx
        .send(FlighthookMessage::new(FlighthookEvent::ClearShotHistory).actor("web"));
    StatusCode::ACCEPTED
}

/// POST /api/shots/grouping — persist the default shot view. Sets are
/// assigned the same way in every view.
///
/// Rides the same `ConfigCommand` path as a Settings save and answers once
/// the system actor has applied it; `504` when it does not within
/// [`CONFIG_TIMEOUT`].
pub async fn post_shot_grouping(
    State(state): State<Arc<WebState>>,
    Json(body): Json<GroupingRequest>,
) -> Result<Json<GroupingRequest>, StatusCode> {
    set_shot_grouping(&state, body, CONFIG_TIMEOUT).await
}

/// Persist `body.mode`, waiting up to `timeout` for the system actor.
async fn set_shot_grouping(
    state: &WebState,
    body: GroupingRequest,
    timeout: Duration,
) -> Result<Json<GroupingRequest>, StatusCode> {
    let action = ConfigAction::SetShotGrouping { mode: body.mode };
    match send_config_command_within(state, action, timeout).await {
        Some(_) => Ok(Json(body)),
        None => {
            tracing::warn!("shot grouping: timed out waiting for ConfigOutcome");
            Err(StatusCode::GATEWAY_TIMEOUT)
        }
    }
}

/// GET /api/settings — returns the full persisted config.
pub async fn get_settings(State(state): State<Arc<WebState>>) -> Json<FlighthookConfig> {
    Json(state.root.system.snapshot())
}

#[derive(Deserialize)]
pub struct SettingsQuery {
    /// Restrict the save to one actor, by global ID (`"mevo.0"`).
    pub scope: Option<String>,
}

/// Build the scoped `ConfigAction` for a `?scope=<global_id>` save.
///
/// The body carries the whole config, but a scoped save means only the named
/// section: it becomes an `Upsert*` for that section alone, leaving every other
/// section of the live config untouched. A scope naming a section the body does
/// not carry is a removal. A `gspro.N` scope names `openconnect_client.N`.
///
/// Returns `None` for a malformed ID or an unknown type prefix.
fn scoped_action(scope: &str, config: &FlighthookConfig) -> Option<ConfigAction> {
    let scope = crate::state::config::canonical_id(scope);
    let (prefix, index) = scope.split_once('.')?;
    let idx = index.to_string();
    let id = scope.clone();

    // Each arm moves `idx`/`id`, which is fine — only one arm ever runs.
    macro_rules! upsert {
        ($section:ident, $variant:ident) => {
            config
                .$section
                .get(index)
                .map_or(ConfigAction::Remove { id }, |section| {
                    ConfigAction::$variant {
                        index: idx,
                        section: section.clone(),
                    }
                })
        };
    }

    Some(match prefix {
        "webserver" => upsert!(webserver, UpsertWebserver),
        "mevo" => upsert!(mevo, UpsertMevo),
        "r10" => upsert!(r10, UpsertR10),
        "square" => upsert!(square, UpsertSquare),
        "exputt" => upsert!(exputt, UpsertExPutt),
        "openconnect_server" => upsert!(openconnect_server, UpsertOpenConnectServer),
        "openconnect_client" => upsert!(openconnect_client, UpsertOpenConnectClient),
        "pureplay" => upsert!(pureplay, UpsertPureplay),
        "mock_monitor" => upsert!(mock_monitor, UpsertMockMonitor),
        "random_club" => upsert!(random_club, UpsertRandomClub),
        "camera" => upsert!(camera, UpsertCamera),
        _ => return None,
    })
}

/// Why a save (`scope`, over the `live` config) of `new_config` is refused
/// with `409`, or `None`: it turns a card on, or adds one that is on, while
/// an enabled card of the same type has its address
/// ([`FlighthookConfig::address_clash`]). Editing a card that is on onto a
/// held address is no refusal; the system actor saves that card as off.
fn enable_refusal(
    live: &FlighthookConfig,
    scope: Option<&str>,
    new_config: &FlighthookConfig,
) -> Option<String> {
    let after = match scope {
        None => new_config.clone(),
        Some("global" | "body_tracking" | "player") => return None,
        Some(scope) => {
            let scope = crate::state::config::canonical_id(scope);
            let (prefix, index) = scope.split_once('.')?;
            let mut after = live.clone();
            macro_rules! copy {
                ($section:ident) => {
                    match new_config.$section.get(index) {
                        Some(section) => {
                            after.$section.insert(index.to_owned(), section.clone());
                        }
                        None => return None,
                    }
                };
            }
            match prefix {
                "mevo" => copy!(mevo),
                "square" => copy!(square),
                "exputt" => copy!(exputt),
                "openconnect_server" => copy!(openconnect_server),
                "openconnect_client" => copy!(openconnect_client),
                "pureplay" => copy!(pureplay),
                _ => return None,
            }
            after
        }
    };
    after
        .enable_clashes(live)
        .first()
        .map(flighthook::AddressClash::message)
}

/// Why `action` breaks the one-web-server rule, against the `live` config:
/// no save removes a web server, and none adds one while the config holds
/// one already. A config holding several keeps them all; each stays
/// editable.
fn webserver_violation(live: &FlighthookConfig, action: &ConfigAction) -> Option<&'static str> {
    const NO_REMOVE: &str = "the web server can't be removed";
    const ONLY_ONE: &str = "flighthook runs one web server; edit the existing one";
    match action {
        ConfigAction::Remove { id } => id
            .split_once('.')
            .is_some_and(|(prefix, _)| prefix == "webserver")
            .then_some(NO_REMOVE),
        ConfigAction::UpsertWebserver { index, .. } => {
            (!live.webserver.is_empty() && !live.webserver.contains_key(index)).then_some(ONLY_ONE)
        }
        ConfigAction::ReplaceAll { config } => {
            if live
                .webserver
                .keys()
                .any(|k| !config.webserver.contains_key(k))
            {
                Some(NO_REMOVE)
            } else if config.webserver.len() > live.webserver.len().max(1) {
                Some(ONLY_ONE)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Why `action` would save an invalid setting, or `None`: a
/// `launch_only_spin` outside 0 to
/// [`MevoSection::MAX_LAUNCH_ONLY_SPIN`](flighthook::MevoSection::MAX_LAUNCH_ONLY_SPIN)
/// in a Mevo section the action writes, a camera section without a camera
/// or with a video mode that is neither `auto` nor `WIDTHxHEIGHT@FPS`, a
/// launch monitor `impact_lead_s` outside 0 to
/// [`MAX_IMPACT_LEAD_S`](flighthook::MAX_IMPACT_LEAD_S), a camera setting
/// out of range ([`CameraSection::error`](flighthook::CameraSection::error):
/// `latency_s`, `rotation_deg`, `yaw_deg`, `pitch_deg`), a
/// `[replay]` value out of range ([`ReplaySection::error`](flighthook::ReplaySection::error)),
/// a `[body_tracking]` one
/// ([`BodyTrackingSection::error`](flighthook::BodyTrackingSection::error)),
/// or a `[player]` one ([`PlayerSection::error`](flighthook::PlayerSection::error)).
fn invalid_setting(action: &ConfigAction) -> Option<String> {
    if let ConfigAction::ReplaceAll { config } | ConfigAction::ReplaceGlobal { config } = action {
        if let Some(error) = config.replay.error() {
            return Some(format!("replay: {error}"));
        }
        if let Some(error) = config.body_tracking.error() {
            return Some(format!("body_tracking: {error}"));
        }
        if let Some(error) = config.player.error() {
            return Some(format!("player: {error}"));
        }
    }
    if let ConfigAction::SetBodyTracking { section } = action
        && let Some(error) = section.error()
    {
        return Some(format!("body_tracking: {error}"));
    }
    if let ConfigAction::SetPlayer { section } = action
        && let Some(error) = section.error()
    {
        return Some(format!("player: {error}"));
    }
    let cameras: Vec<(&String, &flighthook::CameraSection)> = match action {
        ConfigAction::ReplaceAll { config } => config.camera.iter().collect(),
        ConfigAction::UpsertCamera { index, section } => vec![(index, section)],
        _ => Vec::new(),
    };
    for (index, camera) in cameras {
        if camera.camera.trim().is_empty() {
            return Some(format!("camera.{index}: no camera picked"));
        }
        if let Err(e) = camera.mode.parse::<stance::FormatRequest>() {
            return Some(format!("camera.{index}: {e}"));
        }
    }
    if let Some(error) = invalid_impact_lead(action) {
        return Some(error);
    }
    if let Some(error) = invalid_camera_setting(action) {
        return Some(error);
    }
    invalid_mevo_setting(action)
}

/// Why `action` would save device driver settings the driver refuses (its
/// own validation, with the field named), or `None`. Only a card whose
/// driver settings the action changes counts, measured against `saved`, the
/// settings saved now: a card whose saved driver settings are refused still
/// saves its other fields and its on/off switch
/// ([`flighthook::DeviceSection::changed_driver_error`]).
fn invalid_driver_setting(saved: &FlighthookConfig, action: &ConfigAction) -> Option<String> {
    use flighthook::DeviceSection;
    match action {
        ConfigAction::ReplaceAll { config } => config.changed_device_error(saved),
        ConfigAction::UpsertMockMonitor { index, section } => section.changed_driver_error(
            saved.mock_monitor.get(index),
            &format!("mock_monitor.{index}"),
        ),
        _ => None,
    }
}

/// [`invalid_setting`] for a camera's `latency_s`, `rotation_deg`, `yaw_deg`
/// and `pitch_deg`.
fn invalid_camera_setting(action: &ConfigAction) -> Option<String> {
    match action {
        ConfigAction::ReplaceAll { config } => config.camera_error(),
        ConfigAction::UpsertCamera { index, section } => section.error(&format!("camera.{index}")),
        _ => None,
    }
}

/// [`invalid_setting`] for a launch monitor's `impact_lead_s`.
fn invalid_impact_lead(action: &ConfigAction) -> Option<String> {
    let (kind, index, lead) = match action {
        ConfigAction::ReplaceAll { config } => return config.impact_lead_error(),
        ConfigAction::UpsertMevo { index, section } => ("mevo", index, section.impact_lead_s),
        ConfigAction::UpsertR10 { index, section } => ("r10", index, section.impact_lead_s),
        ConfigAction::UpsertSquare { index, section } => ("square", index, section.impact_lead_s),
        ConfigAction::UpsertExPutt { index, section } => ("exputt", index, section.impact_lead_s),
        ConfigAction::UpsertMockMonitor { index, section } => {
            ("mock_monitor", index, section.impact_lead_s)
        }
        ConfigAction::UpsertOpenConnectServer { index, section } => {
            ("openconnect_server", index, section.impact_lead_s)
        }
        _ => return None,
    };
    flighthook::impact_lead_error(&format!("{kind}.{index}"), lead)
}

/// [`invalid_setting`] for Mevo sections.
fn invalid_mevo_setting(action: &ConfigAction) -> Option<String> {
    let mevo: Vec<(&String, &flighthook::MevoSection)> = match action {
        ConfigAction::ReplaceAll { config } => config.mevo.iter().collect(),
        ConfigAction::UpsertMevo { index, section } => vec![(index, section)],
        _ => Vec::new(),
    };
    mevo.into_iter()
        .find(|(_, s)| !flighthook::MevoSection::launch_only_spin_valid(s.launch_only_spin))
        .map(|(index, s)| {
            format!(
                "mevo.{index}: launch_only_spin must be 0 to {} rpm per mph, not {}",
                flighthook::MevoSection::MAX_LAUNCH_ONLY_SPIN,
                s.launch_only_spin
            )
        })
}

/// POST /api/settings — config replacement via bus request-reply.
///
/// Emits a `ConfigCommand` on the bus, waits for `ConfigOutcome` with a
/// matching `request_id`, then returns the response. SystemActor handles
/// persistence and actor reconciliation.
///
/// `?scope=<global_id>` narrows the save to one actor: only that section is
/// written and only that actor is reconciled. `?scope=global` writes only the
/// global settings, keeping every actor section of the live config.
/// `?scope=body_tracking` writes only `[body_tracking]` and reconciles no
/// actor; `?scope=player` likewise writes only `[player]`. Without a scope the whole config is replaced and every actor is
/// reconciled.
///
/// An unknown scope, a removed web server, a second web server or an
/// invalid device setting ([`invalid_setting`]) is `400` and a missing `ConfigOutcome` `504`, each with a JSON `{"error": "..."}`
/// body; nothing is reported as saved.
pub async fn post_settings(
    State(state): State<Arc<WebState>>,
    Query(query): Query<SettingsQuery>,
    Json(new_config): Json<FlighthookConfig>,
) -> Result<Json<PostSettingsResponse>, SettingsError> {
    save_settings(&state, query.scope.as_deref(), new_config, CONFIG_TIMEOUT).await
}

/// A failed `POST /api/settings`: its status and a JSON `{"error": "..."}`
/// body.
pub type SettingsError = (StatusCode, Json<serde_json::Value>);

fn settings_error(status: StatusCode, message: &str) -> SettingsError {
    (status, Json(serde_json::json!({ "error": message })))
}

/// Apply `new_config` under `scope`, waiting up to `timeout` for the system
/// actor.
async fn save_settings(
    state: &WebState,
    scope: Option<&str>,
    new_config: FlighthookConfig,
    timeout: Duration,
) -> Result<Json<PostSettingsResponse>, SettingsError> {
    let refusal = enable_refusal(&state.root.system.snapshot(), scope, &new_config);
    let action = match scope {
        None => ConfigAction::ReplaceAll {
            config: Box::new(new_config),
        },
        Some("global") => ConfigAction::ReplaceGlobal {
            config: Box::new(new_config),
        },
        Some("body_tracking") => ConfigAction::SetBodyTracking {
            section: new_config.body_tracking,
        },
        Some("player") => ConfigAction::SetPlayer {
            section: new_config.player,
        },
        Some(scope) => match scoped_action(scope, &new_config) {
            Some(action) => action,
            // Applying the body wholesale here would save far more than the
            // caller asked for, so reject instead.
            None => {
                tracing::warn!("config update: unknown scope '{scope}', ignoring request");
                return Err(settings_error(
                    StatusCode::BAD_REQUEST,
                    &format!("unknown scope '{scope}'"),
                ));
            }
        },
    };
    // Guards REST only: the WS path accepts no `ConfigAction`. If it ever
    // does, this check moves into the system actor's `handle_config_command`.
    if let Some(reason) = webserver_violation(&state.root.system.snapshot(), &action) {
        tracing::warn!("config update: {reason}, ignoring request");
        return Err(settings_error(StatusCode::BAD_REQUEST, reason));
    }
    if let Some(reason) = invalid_setting(&action)
        .or_else(|| invalid_driver_setting(&state.root.system.snapshot(), &action))
    {
        tracing::warn!("config update: {reason}, ignoring request");
        return Err(settings_error(StatusCode::BAD_REQUEST, &reason));
    }
    if let Some(reason) = refusal {
        tracing::warn!("config update: {reason}, ignoring request");
        return Err(settings_error(StatusCode::CONFLICT, &reason));
    }
    // A config file that failed to load is copied aside before the first
    // save replaces it; without the copy nothing is saved.
    if let Err(reason) = state.root.system.back_up_unusable_file() {
        tracing::warn!("config update: {reason}, ignoring request");
        return Err(settings_error(StatusCode::INTERNAL_SERVER_ERROR, &reason));
    }

    match send_config_command_within(state, action, timeout).await {
        Some(Outcome {
            restarted, stopped, ..
        }) => Ok(Json(PostSettingsResponse { restarted, stopped })),
        None => {
            tracing::warn!("config update: timed out waiting for ConfigOutcome");
            Err(settings_error(
                StatusCode::GATEWAY_TIMEOUT,
                "the settings were not applied in time",
            ))
        }
    }
}

/// How long a config command (a save, a view change, a reconnect) waits for
/// its `ConfigOutcome`. The actors a command stops are stopped together and
/// given up to [`MAX_STOP_WAIT`](crate::actors::MAX_STOP_WAIT), with
/// [`START_ROOM`] for the rest. The system actor waits for them off its own
/// thread, so a command never waits behind another one's stops.
const CONFIG_TIMEOUT: Duration =
    Duration::from_secs(crate::actors::MAX_STOP_WAIT.as_secs() + START_ROOM.as_secs());

/// What a config command takes beyond its stop waits: the config write and
/// the starts.
const START_ROOM: Duration = Duration::from_secs(4);

// The dashboard waits longer for a save or a reconnect than the server
// does, so it always gets the server's answer.
#[cfg(feature = "gui")]
const _: () =
    assert!(flighthook_ui::net::CONFIG_REQUEST_TIMEOUT.as_secs() > CONFIG_TIMEOUT.as_secs() + 1);

/// How long a reconnect still holds its actor's in-flight guard after the
/// request was answered `202`, waiting for the outcome.
const RECONNECT_LINGER: Duration = CONFIG_TIMEOUT;

/// POST /api/actors/{id}/reconnect — stop the launch monitor or output `id`,
/// wait for its connection to close, and start it again from the saved
/// settings. Nothing else is touched.
///
/// `200` with `{"restarted": [id]}` once it has started again; `202` with
/// nothing restarted when it starts again by itself later: the reconnect is
/// still running after [`CONFIG_TIMEOUT`] (the actor stays in flight until
/// it ends), its old connection has not closed yet, or another command is
/// restarting it; `400` for a camera, the web server or any other actor that
/// is not a launch monitor or output; `404` for an ID no launch monitor or
/// output has; `409` while a reconnect of it is in flight, or when it was
/// not started again for any other reason (it was removed meanwhile).
/// Errors carry a JSON `{"error": "..."}` body.
pub async fn post_reconnect(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<PostSettingsResponse>), SettingsError> {
    reconnect(&state, &id, CONFIG_TIMEOUT).await
}

/// Reconnect actor `id`, waiting up to `timeout` for the system actor.
async fn reconnect(
    state: &Arc<WebState>,
    id: &str,
    timeout: Duration,
) -> Result<(StatusCode, Json<PostSettingsResponse>), SettingsError> {
    use crate::actors::system::{ReconnectRefusal, not_reconnected, reconnect_target};
    let id = match reconnect_target(&state.root.system.snapshot(), id) {
        Ok(id) => id,
        Err(r @ ReconnectRefusal::NotReconnectable(_)) => {
            return Err(settings_error(StatusCode::BAD_REQUEST, r.message()));
        }
        Err(r @ ReconnectRefusal::NotFound(_)) => {
            return Err(settings_error(StatusCode::NOT_FOUND, r.message()));
        }
    };
    let Some(in_flight) = InFlight::begin(state, &id) else {
        return Err(settings_error(
            StatusCode::CONFLICT,
            &format!("a reconnect of '{id}' is already in progress"),
        ));
    };
    let mut outcome = Box::pin(send_config_command(
        state,
        ConfigAction::Reconnect { id: id.clone() },
    ));
    match tokio::time::timeout(timeout, &mut outcome).await {
        Ok(Some(Outcome { restarted, stopped })) if restarted.contains(&id) => Ok((
            StatusCode::OK,
            Json(PostSettingsResponse { restarted, stopped }),
        )),
        // It starts again by itself once its old connection has closed,
        // or the command restarting it is done.
        Ok(Some(_)) if state.root.is_closing(&id) || state.root.is_settling(&id) => Ok((
            StatusCode::ACCEPTED,
            Json(PostSettingsResponse {
                restarted: Vec::new(),
                stopped: Vec::new(),
            }),
        )),
        Ok(Some(_)) => Err(settings_error(
            StatusCode::CONFLICT,
            &not_reconnected(&state.root, &id),
        )),
        Ok(None) => Err(settings_error(
            StatusCode::GATEWAY_TIMEOUT,
            "the reconnect did not finish",
        )),
        Err(_) => {
            tracing::warn!("reconnect '{id}': still running, answered 202");
            // Still queued or running: the guard holds until it ends, so a
            // second request cannot restart the actor again behind it.
            tokio::spawn(async move {
                let _ = tokio::time::timeout(RECONNECT_LINGER, outcome).await;
                drop(in_flight);
            });
            Ok((
                StatusCode::ACCEPTED,
                Json(PostSettingsResponse {
                    restarted: Vec::new(),
                    stopped: Vec::new(),
                }),
            ))
        }
    }
}

/// A reconnect in flight: its actor ID is in the web state's set while this
/// lives.
struct InFlight {
    state: Arc<WebState>,
    id: String,
}

impl InFlight {
    /// Mark `id` in flight; `None` when it already is.
    fn begin(state: &Arc<WebState>, id: &str) -> Option<Self> {
        state
            .reconnecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_owned())
            .then(|| Self {
                state: Arc::clone(state),
                id: id.to_owned(),
            })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.state
            .reconnecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// A `ConfigOutcome`'s actor lists.
#[derive(Debug)]
struct Outcome {
    restarted: Vec<String>,
    stopped: Vec<String>,
}

/// Emit a `ConfigCommand` for `action` and wait up to `timeout` for the
/// system actor's matching `ConfigOutcome`. Returns it, or `None` on
/// timeout or a closed bus.
async fn send_config_command_within(
    state: &WebState,
    action: ConfigAction,
    timeout: Duration,
) -> Option<Outcome> {
    tokio::time::timeout(timeout, send_config_command(state, action))
        .await
        .ok()
        .flatten()
}

/// Emit a `ConfigCommand` for `action` now. The future resolves to the
/// system actor's matching `ConfigOutcome`, or `None` once the bus closes.
fn send_config_command(
    state: &WebState,
    action: ConfigAction,
) -> impl Future<Output = Option<Outcome>> + Send + 'static {
    let request_id = crate::state::config::generate_id();
    // Subscribed before the command goes out, so its outcome cannot be
    // missed.
    let mut bus_rx = state.bus_tx.subscribe();
    let _ = state.bus_tx.send(
        FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some(request_id.clone()),
            action: Box::new(action),
        })
        .actor("web"),
    );
    async move {
        loop {
            match bus_rx.recv().await {
                Ok(msg) => {
                    if let FlighthookEvent::ConfigOutcome {
                        request_id: Some(ref rid),
                        ref restarted,
                        ref stopped,
                        ..
                    } = msg.event
                        && *rid == request_id
                    {
                        return Some(Outcome {
                            restarted: restarted.clone(),
                            stopped: stopped.clone(),
                        });
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flighthook::MevoSection;

    fn config() -> FlighthookConfig {
        serde_json::from_str("{}").expect("empty config")
    }

    #[tokio::test]
    async fn the_page_icons_are_served_with_their_types() {
        for (response, content_type, magic) in [
            (get_favicon_ico().await, "image/x-icon", &[0, 0, 1, 0][..]),
            (get_favicon_png().await, "image/png", &b"\x89PNG"[..]),
            (get_apple_touch_icon().await, "image/png", &b"\x89PNG"[..]),
        ] {
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert!(body.starts_with(magic), "{content_type}");
        }
    }

    fn mevo_section(name: &str) -> MevoSection {
        let mut s: MevoSection = serde_json::from_str("{}").expect("empty section");
        s.name = name.to_string();
        s
    }

    #[test]
    fn scope_upserts_only_the_named_section() {
        let mut c = config();
        c.mevo.insert("0".into(), mevo_section("Mevo WiFi"));
        c.openconnect_client.insert(
            "0".into(),
            serde_json::from_str("{}").expect("empty section"),
        );

        let action = scoped_action("mevo.0", &c).expect("known scope");
        match action {
            ConfigAction::UpsertMevo { index, section } => {
                assert_eq!(index, "0");
                assert_eq!(section.name, "Mevo WiFi");
            }
            other => panic!("expected UpsertMevo, got {other:?}"),
        }
    }

    #[test]
    fn a_camera_scope_upserts_or_removes_its_section() {
        let mut c = config();
        c.camera.insert(
            "2".into(),
            flighthook::CameraSection {
                camera: "test-pattern".into(),
                ..flighthook::CameraSection::default()
            },
        );
        match scoped_action("camera.2", &c).expect("known scope") {
            ConfigAction::UpsertCamera { index, section } => {
                assert_eq!(index, "2");
                assert_eq!(section.camera, "test-pattern");
            }
            other => panic!("expected UpsertCamera, got {other:?}"),
        }
        match scoped_action("camera.3", &c).expect("known scope") {
            ConfigAction::Remove { id } => assert_eq!(id, "camera.3"),
            other => panic!("expected Remove, got {other:?}"),
        }
    }

    #[test]
    fn a_camera_without_a_camera_or_with_a_bad_mode_or_bad_replay_values_is_invalid() {
        let camera = |camera: &str, mode: &str| ConfigAction::UpsertCamera {
            index: "0".into(),
            section: flighthook::CameraSection {
                camera: camera.into(),
                mode: mode.into(),
                ..flighthook::CameraSection::default()
            },
        };
        assert_eq!(invalid_setting(&camera("test-pattern", "auto")), None);
        assert_eq!(invalid_setting(&camera("usb-1", "1280x720@120")), None);
        assert_eq!(
            invalid_setting(&camera(" ", "auto")).as_deref(),
            Some("camera.0: no camera picked")
        );
        let bad_mode = invalid_setting(&camera("usb-1", "fast")).expect("rejected");
        assert!(
            bad_mode.starts_with("camera.0: invalid argument: video mode `fast`"),
            "{bad_mode}"
        );

        let mut c = config();
        c.replay.keep_shots = 0;
        for action in [
            ConfigAction::ReplaceGlobal {
                config: Box::new(c.clone()),
            },
            ConfigAction::ReplaceAll {
                config: Box::new(c.clone()),
            },
        ] {
            let error = invalid_setting(&action).expect("rejected");
            assert!(error.starts_with("replay: keep_shots 0"), "{error}");
        }
        c.replay.keep_shots = 5;
        c.replay.pre_roll_s = 2.0;
        assert_eq!(
            invalid_setting(&ConfigAction::ReplaceGlobal {
                config: Box::new(c.clone())
            }),
            None
        );
        // Body tracking values out of range are refused the same way.
        for (field, set) in [
            (
                "min_score",
                (|b: &mut flighthook::BodyTrackingSection| b.min_score = 2.0)
                    as fn(&mut flighthook::BodyTrackingSection),
            ),
            ("smoothing", |b| b.smoothing = 50.0),
            ("detect_every", |b| b.detect_every = 0),
        ] {
            let mut bad = c.clone();
            set(&mut bad.body_tracking);
            for action in [
                ConfigAction::ReplaceGlobal {
                    config: Box::new(bad.clone()),
                },
                ConfigAction::ReplaceAll {
                    config: Box::new(bad.clone()),
                },
            ] {
                let error = invalid_setting(&action).expect("rejected");
                assert!(
                    error.starts_with(&format!("body_tracking: {field}")),
                    "{error}"
                );
            }
        }
        c.body_tracking.model = flighthook::PoseModel::RtmposeS;
        c.body_tracking.detect_every = 60;
        assert_eq!(
            invalid_setting(&ConfigAction::ReplaceGlobal {
                config: Box::new(c)
            }),
            None
        );
    }

    #[test]
    fn scope_naming_an_absent_section_is_a_removal() {
        let action = scoped_action("mevo.0", &config()).expect("known scope");
        match action {
            ConfigAction::Remove { id } => assert_eq!(id, "mevo.0"),
            other => panic!("expected Remove, got {other:?}"),
        }
    }

    #[test]
    fn gspro_scope_names_the_openconnect_client_section() {
        let mut c = config();
        c.openconnect_client.insert(
            "1".into(),
            serde_json::from_str(r#"{"name":"Muni Golf"}"#).expect("section"),
        );
        match scoped_action("gspro.1", &c).expect("known scope") {
            ConfigAction::UpsertOpenConnectClient { index, section } => {
                assert_eq!(index, "1");
                assert_eq!(section.name, "Muni Golf");
            }
            other => panic!("expected UpsertOpenConnectClient, got {other:?}"),
        }
        match scoped_action("gspro.2", &c).expect("known scope") {
            ConfigAction::Remove { id } => assert_eq!(id, "openconnect_client.2"),
            other => panic!("expected Remove, got {other:?}"),
        }
    }

    #[test]
    fn a_pureplay_scope_upserts_or_removes_its_section() {
        let mut c = config();
        c.pureplay.insert(
            "0".into(),
            serde_json::from_str(r#"{"name":"Pureplay","address":"127.0.0.1:9760"}"#)
                .expect("section"),
        );
        match scoped_action("pureplay.0", &c).expect("known scope") {
            ConfigAction::UpsertPureplay { index, section } => {
                assert_eq!(index, "0");
                assert_eq!(section.address.as_deref(), Some("127.0.0.1:9760"));
            }
            other => panic!("expected UpsertPureplay, got {other:?}"),
        }
        match scoped_action("pureplay.1", &c).expect("known scope") {
            ConfigAction::Remove { id } => assert_eq!(id, "pureplay.1"),
            other => panic!("expected Remove, got {other:?}"),
        }
    }

    fn with_webservers(indices: &[&str]) -> FlighthookConfig {
        let mut c = config();
        for i in indices {
            c.webserver.insert(
                (*i).to_string(),
                flighthook::WebserverSection {
                    name: "Web Server".into(),
                    bind: format!("0.0.0.0:588{i}"),
                },
            );
        }
        c
    }

    #[test]
    fn no_save_removes_a_web_server_or_adds_a_second() {
        let one = with_webservers(&["0"]);
        let remove = |id: &str| ConfigAction::Remove { id: id.into() };
        let upsert = |index: &str| ConfigAction::UpsertWebserver {
            index: index.into(),
            section: with_webservers(&[index]).webserver[index].clone(),
        };
        let replace = |c: FlighthookConfig| ConfigAction::ReplaceAll {
            config: Box::new(c),
        };
        let no_remove = Some("the web server can't be removed");
        let only_one = Some("flighthook runs one web server; edit the existing one");

        assert_eq!(webserver_violation(&one, &remove("webserver.0")), no_remove);
        assert_eq!(webserver_violation(&one, &remove("webserver.7")), no_remove);
        assert_eq!(webserver_violation(&one, &remove("mevo.0")), None);
        assert_eq!(webserver_violation(&one, &upsert("0")), None);
        assert_eq!(webserver_violation(&one, &upsert("1")), only_one);
        assert_eq!(webserver_violation(&config(), &upsert("0")), None);
        assert_eq!(
            webserver_violation(&one, &replace(with_webservers(&["0"]))),
            None
        );
        assert_eq!(webserver_violation(&one, &replace(config())), no_remove);
        assert_eq!(
            webserver_violation(&one, &replace(with_webservers(&["1"]))),
            no_remove
        );
        assert_eq!(
            webserver_violation(&one, &replace(with_webservers(&["0", "1"]))),
            only_one
        );
        assert_eq!(
            webserver_violation(&config(), &replace(with_webservers(&["0"]))),
            None
        );

        // A config that already holds two keeps both, and each stays
        // editable, but gets no third.
        let two = with_webservers(&["0", "1"]);
        assert_eq!(webserver_violation(&two, &upsert("1")), None);
        assert_eq!(webserver_violation(&two, &upsert("2")), only_one);
        assert_eq!(
            webserver_violation(&two, &replace(with_webservers(&["0", "1"]))),
            None
        );
        assert_eq!(
            webserver_violation(&two, &replace(with_webservers(&["0"]))),
            no_remove
        );
        assert_eq!(
            webserver_violation(&two, &replace(with_webservers(&["0", "1", "2"]))),
            only_one
        );
    }

    #[test]
    fn malformed_and_unknown_scopes_are_rejected() {
        assert!(scoped_action("mevo", &config()).is_none());
        assert!(scoped_action("nosuchtype.0", &config()).is_none());
    }
}

#[cfg(test)]
mod convert_tests {
    use super::*;
    use flighthook::{DerivedFlight, Distance, Launch, ShotData, Velocity};

    fn shot() -> ShotData {
        let mut derived = DerivedFlight::default();
        derived.carry_distance = Some(Distance::Meters(150.9));
        derived.carry_offline = Some(Distance::Meters(-5.5));
        derived.max_height = Some(Distance::Meters(26.0));
        derived.flight_time = Some(6.1);
        derived.descent_angle = Some(50.2);
        derived.roll_distance = Some(Distance::Meters(-1.2));
        derived.total_distance = Some(Distance::Meters(149.7));
        ShotData {
            actor: "square.0".into(),
            shot_number: 3,
            launch: Some(Launch {
                launch_speed: Some(Velocity::MetersPerSecond(53.6)),
                launch_elevation: Some(17.0),
                launch_azimuth: Some(-1.2),
                backspin_rpm: Some(6500),
                sidespin_rpm: Some(-300),
                ..Launch::default()
            }),
            club: None,
            impact: None,
            derived: Some(derived),
            ..ShotData::default()
        }
    }

    async fn convert(shot: ShotData, units: &str) -> ShotData {
        // Through JSON both ways, as a WS consumer would send it.
        let body: ShotData =
            serde_json::from_str(&serde_json::to_string(&shot).expect("ser")).expect("de");
        let Json(out) = post_convert_shot(
            Query(ConvertQuery {
                units: units.into(),
            }),
            Json(body),
        )
        .await
        .expect("converts");
        serde_json::from_str(&serde_json::to_string(&out).expect("ser")).expect("de")
    }

    #[tokio::test]
    async fn convert_drops_camera_clips() {
        let mut shot = shot();
        shot.video = vec![ShotVideo {
            camera: "camera.0".into(),
            position: flighthook::CameraPosition::GolferFront,
            duration_ms: 1,
            fps: 30.0,
            trigger_offset_ms: 0,
            impact_offset_ms: 0,
            complete: true,
            dropped: 0,
            pose: false,
            swing: false,
            rotation: 0,
            analyzed_with: None,
        }];
        for units in ["imperial", "metric"] {
            assert!(
                convert(shot.clone(), units).await.video.is_empty(),
                "{units}"
            );
        }
    }

    #[tokio::test]
    async fn convert_round_trips_derived_values() {
        let original = shot();
        let imperial = convert(original.clone(), "imperial").await;
        let d = imperial.derived.clone().expect("derived kept");
        assert!(matches!(d.carry_distance, Some(Distance::Yards(_))));
        assert!(matches!(d.carry_offline, Some(Distance::Yards(_))));
        assert!(matches!(d.max_height, Some(Distance::Feet(_))));
        assert!(matches!(d.roll_distance, Some(Distance::Yards(v)) if v < 0.0));
        assert_eq!(d.flight_time, Some(6.1));
        assert_eq!(d.descent_angle, Some(50.2));

        let back = convert(imperial, "metric")
            .await
            .derived
            .expect("derived kept");
        let want = original.derived.expect("derived");
        let close = |a: Option<Distance>, b: Option<Distance>| {
            (a.expect("a").as_meters() - b.expect("b").as_meters()).abs() < 1e-9
        };
        assert!(close(back.carry_distance, want.carry_distance));
        assert!(close(back.carry_offline, want.carry_offline));
        assert!(close(back.max_height, want.max_height));
        assert!(close(back.roll_distance, want.roll_distance));
        assert!(close(back.total_distance, want.total_distance));
        assert_eq!(back.roll_offline, None);
        assert_eq!(back.total_offline, None);
    }
}

#[cfg(test)]
mod shot_fields_tests {
    use super::super::tests::{feed, test_state};
    use super::*;
    use flighthook::{Basis, ClubData, Distance, FlightResult, Launch, ShotKey, Velocity};

    async fn shots_json(state: &Arc<WebState>, units: Option<&str>) -> serde_json::Value {
        let Json(shots) = get_shots(
            State(Arc::clone(state)),
            Query(ShotsQuery {
                limit: 50,
                units: units.map(str::to_string),
            }),
        )
        .await;
        serde_json::to_value(shots).expect("ser")
    }

    /// A finished shot's measured carry offline and descent angle and its
    /// computed metrics reach `/api/shots`, converted by `?units=`.
    #[tokio::test]
    async fn api_shots_carry_offline_and_metrics() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let key = ShotKey {
            shot_id: "lat".into(),
            shot_number: 4,
        };
        let ball = Launch {
            launch_speed: Some(Velocity::MetersPerSecond(40.0)),
            launch_azimuth: Some(2.0),
            launch_elevation: Some(20.0),
            backspin_rpm: Some(6000),
            sidespin_rpm: Some(-800),
            ..Launch::default()
        };
        let flight = FlightResult {
            carry_distance: Some(Distance::Meters(100.0)),
            carry_offline: Some(Distance::Meters(-1.5)),
            descent_angle: Some(44.2),
            ..FlightResult::new(Basis::Modeled)
        };
        let club = ClubData {
            path: Some(3.0),
            face_angle: Some(1.0),
            dynamic_loft: Some(25.0),
            attack_angle: Some(-4.0),
            ..ClubData::default()
        };
        feed(
            &state,
            "mevo.0",
            vec![
                FlighthookEvent::shot_trigger(key.clone()),
                FlighthookEvent::Launch {
                    key: key.clone(),
                    launch: Box::new(ball),
                    derived: None,
                },
                FlighthookEvent::FlightResult {
                    key: key.clone(),
                    flight: Box::new(flight),
                },
                FlighthookEvent::ClubPath {
                    key: key.clone(),
                    club: Box::new(club),
                },
                FlighthookEvent::ShotFinished { key, raw: None },
            ],
        )
        .await;

        let json = shots_json(&state, None).await;
        let shot = &json[0];
        assert_eq!(shot["flight"]["carry_offline"], "-1.5m", "{shot}");
        assert_eq!(shot["flight"]["descent_angle"], 44.2, "{shot}");
        assert_eq!(shot["flight"]["basis"], "modeled", "{shot}");
        assert!(shot.get("ball").is_none(), "{shot}");
        let m = &shot["metrics"];
        assert_eq!(m["total_spin_rpm"], 6053, "{m}");
        let axis = m["spin_axis"].as_f64().expect("axis");
        assert!((axis + 7.595).abs() < 1e-3, "{axis}");
        assert_eq!(m["face_to_path"], -2.0, "{m}");
        assert_eq!(m["spin_loft"], 29.0, "{m}");
        // Started right (2 deg = +3.49 m at 100 m), landed 1.5 m left: curved
        // left by about 4.99 m.
        let curve: Distance = serde_json::from_value(m["curve"].clone()).expect("curve");
        assert!(matches!(curve, Distance::Meters(_)), "{curve:?}");
        let want = -1.5 - 100.0 * 2.0_f64.to_radians().tan();
        assert!((curve.as_meters() - want).abs() < 1e-9, "{curve:?}");

        let json = shots_json(&state, Some("imperial")).await;
        let shot = &json[0];
        let offline: Distance =
            serde_json::from_value(shot["flight"]["carry_offline"].clone()).expect("offline");
        assert!(matches!(offline, Distance::Yards(v) if (v + 1.640_42).abs() < 1e-4));
        let curve: Distance =
            serde_json::from_value(shot["metrics"]["curve"].clone()).expect("curve");
        assert!(matches!(curve, Distance::Yards(_)), "{curve:?}");
        assert!((curve.as_meters() - want).abs() < 1e-9, "{curve:?}");
        assert_eq!(shot["metrics"]["face_to_path"], -2.0);
    }

    /// No inputs, no metrics: a shot with only launch data has no `metrics`.
    #[tokio::test]
    async fn api_shots_omit_metrics_without_inputs() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let key = ShotKey {
            shot_id: "bare".into(),
            shot_number: 1,
        };
        feed(
            &state,
            "mevo.0",
            vec![
                FlighthookEvent::shot_trigger(key.clone()),
                FlighthookEvent::Launch {
                    key: key.clone(),
                    launch: Box::new(Launch {
                        launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                        backspin_rpm: Some(6000),
                        ..Launch::default()
                    }),
                    derived: None,
                },
                FlighthookEvent::ShotFinished { key, raw: None },
            ],
        )
        .await;
        let json = shots_json(&state, None).await;
        assert!(json[0].get("metrics").is_none(), "{json}");
        assert!(json[0]["ball"].get("carry_offline").is_none(), "{json}");
    }
}

#[cfg(test)]
mod group_tests {
    use super::super::tests::{TempDir, feed, test_state, web_state};
    use super::*;
    use flighthook::{Handedness, Launch, ShotGrouping, ShotKey, Velocity};

    fn next_event(rx: &mut broadcast::Receiver<FlighthookMessage>) -> FlighthookMessage {
        rx.try_recv().expect("a bus message")
    }

    async fn body_text(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 4096)
            .await
            .expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf-8")
    }

    #[tokio::test]
    async fn post_club_emits_club_info_as_web() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        let resp = post_club(
            State(Arc::clone(&state)),
            Json(ClubRequest { club: "i7".into() }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let msg = next_event(&mut rx);
        assert_eq!(msg.actor, "web");
        assert!(matches!(
            msg.event,
            FlighthookEvent::ClubInfo {
                club_info: ClubInfo { club: Club::Iron7 }
            }
        ));
    }

    /// `POST /api/hand` with `body`, as axum extracts it.
    async fn hand_request(state: &Arc<WebState>, body: &str) -> Response {
        post_hand(
            State(Arc::clone(state)),
            Json::<HandRequest>::from_bytes(body.as_bytes()),
        )
        .await
    }

    #[tokio::test]
    async fn post_hand_emits_hand_info_as_web() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        for (body, handed) in [
            (r#"{"handed":"lh"}"#, Handedness::Left),
            (r#"{"handed":"rh"}"#, Handedness::Right),
        ] {
            let resp = hand_request(&state, body).await;
            assert_eq!(resp.status(), StatusCode::ACCEPTED, "{body}");
            let msg = next_event(&mut rx);
            assert_eq!(msg.actor, "web");
            assert!(
                matches!(
                    msg.event,
                    FlighthookEvent::HandInfo { hand_info } if hand_info.handed == handed
                ),
                "{body}: {:?}",
                msg.event
            );
        }
    }

    #[tokio::test]
    async fn post_hand_rejects_anything_else_with_a_400() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        for body in [
            r#"{"handed":"left"}"#,
            r#"{"handed":""}"#,
            r#"{"hand":"lh"}"#,
            r#"{"handed":1}"#,
            "not json",
        ] {
            let resp = hand_request(&state, body).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(
                body_text(resp).await,
                r#"body must be {"handed":"rh"} or {"handed":"lh"}"#,
                "{body}"
            );
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
    }

    #[tokio::test]
    async fn post_club_rejects_an_unknown_code() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        let resp = post_club(
            State(Arc::clone(&state)),
            Json(ClubRequest {
                club: "banana".into(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let text = body_text(resp).await;
        assert!(
            text.starts_with("club must be one of DR, MD, 2W, "),
            "{text}"
        );
        assert!(text.ends_with(", LW, PT"), "{text}");
        assert!(rx.try_recv().is_err(), "nothing emitted");
    }

    #[tokio::test]
    async fn post_find_mat_emits_find_mat_for_a_configured_exputt() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| {
            c.exputt
                .insert("0".into(), flighthook::ExPuttSection::default());
        });
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        let resp = post_find_mat(
            State(Arc::clone(&state)),
            Json(FindMatRequest {
                actor: "exputt.0".into(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let msg = next_event(&mut rx);
        assert_eq!(msg.actor, "web");
        assert!(matches!(msg.event, FlighthookEvent::FindMat { target } if target == "exputt.0"));
    }

    #[tokio::test]
    async fn post_find_mat_rejects_an_unknown_exputt() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| {
            c.exputt
                .insert("0".into(), flighthook::ExPuttSection::default());
        });
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        for actor in ["exputt.1", "square.0", "exputt"] {
            let resp = post_find_mat(
                State(Arc::clone(&state)),
                Json(FindMatRequest {
                    actor: actor.into(),
                }),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{actor}");
            assert_eq!(
                body_text(resp).await,
                format!("no ExPutt with ID '{actor}'")
            );
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
    }

    /// Every message on `rx` so far.
    fn drain(rx: &mut broadcast::Receiver<FlighthookMessage>) -> Vec<FlighthookMessage> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// 1, 3 and 5 yd are sent as Auto-chip shots numbered 1, 2, 3: the
    /// reply describes the shot put on the bus, and the web layer stores it
    /// under the Auto-chip actor with its simulated total.
    #[tokio::test]
    async fn auto_chip_sends_1_3_and_5_yards() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| c.simulation.auto_chip = true);
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        for (n, yards) in (1..).zip(flighthook::AUTO_CHIP_YARDS) {
            let Json(resp) =
                post_auto_chip(State(Arc::clone(&state)), Json(AutoChipRequest { yards }))
                    .await
                    .expect("accepted");
            assert_eq!(resp.actor, flighthook::AUTO_CHIP_ACTOR);
            assert_eq!(resp.shot_number, n);
            assert_eq!(resp.launch_elevation, 28.0);
            #[allow(clippy::cast_possible_truncation)]
            let spin = (100.0 * resp.ball_speed_mph).round() as i32;
            assert_eq!(resp.backspin_rpm, spin);

            // The web layer's own shot_group events are left out.
            let msgs: Vec<_> = drain(&mut rx)
                .into_iter()
                .filter(|m| m.actor != state.actor_id)
                .collect();
            // The source declaration, then trigger, launch, finished.
            assert_eq!(msgs.len(), 4, "{msgs:?}");
            assert!(matches!(
                msgs[0].event,
                FlighthookEvent::DeviceTelemetry {
                    source: Some(flighthook::Provenance::Virtual),
                    ..
                }
            ));
            let mut pending = super::super::PendingShots::new();
            for msg in &msgs {
                assert_eq!(msg.actor, flighthook::AUTO_CHIP_ACTOR);
                super::super::apply_bus_event(&state, msg, &mut pending).await;
            }
            let shots = state.shots.read().await;
            let stored = shots.back().expect("stored");
            assert_eq!(stored.data.actor, flighthook::AUTO_CHIP_ACTOR);
            assert_eq!(stored.data.shot_id.as_deref(), Some(resp.shot_id.as_str()));
            assert!(stored.data.club.is_none());
            let total = stored
                .data
                .derived
                .as_ref()
                .and_then(|d| d.total_distance)
                .expect("total")
                .as_yards();
            assert!((total - f64::from(yards)).abs() <= 0.1, "{yards}: {total}");
        }
        assert_eq!(state.shots.read().await.len(), 3);
    }

    #[tokio::test]
    async fn auto_chip_rejects_any_other_distance() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| c.simulation.auto_chip = true);
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        for yards in [0, 2, 4, 6, 10, 100, u32::MAX] {
            let (status, Json(body)) =
                post_auto_chip(State(Arc::clone(&state)), Json(AutoChipRequest { yards }))
                    .await
                    .expect_err("rejected");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{yards}");
            assert_eq!(body["error"], "yards must be one of 1, 3, 5");
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
    }

    #[tokio::test]
    async fn auto_chip_refuses_while_disabled_in_settings() {
        let (state, _writer, _dir) = test_state();
        assert!(!state.root.system.simulation().auto_chip, "off by default");
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        let (status, Json(body)) = post_auto_chip(
            State(Arc::clone(&state)),
            Json(AutoChipRequest { yards: 3 }),
        )
        .await
        .expect_err("refused");
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "Auto-chip is disabled in settings");
        assert!(rx.try_recv().is_err(), "nothing emitted");
        assert_eq!(
            state
                .auto_chip_shots
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "no number taken"
        );

        state.root.system.update(|c| c.simulation.auto_chip = true);
        let Json(resp) = post_auto_chip(
            State(Arc::clone(&state)),
            Json(AutoChipRequest { yards: 3 }),
        )
        .await
        .expect("accepted once enabled");
        assert_eq!(resp.shot_number, 1);
    }

    /// While a tournament round is active no chip fires, neither through
    /// the route nor through `autochip::send`; once it ends the user's
    /// setting applies again.
    #[tokio::test]
    async fn auto_chip_does_not_fire_during_a_strict_round() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| c.simulation.auto_chip = true);
        let round = state
            .root
            .round
            .begin_round(&crate::state::round::RoundRequest {
                holder: "Pureplay",
                feeds: &[],
            })
            .expect("a round");
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        let (status, Json(body)) = post_auto_chip(
            State(Arc::clone(&state)),
            Json(AutoChipRequest { yards: 3 }),
        )
        .await
        .expect_err("refused");
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"],
            "Auto-chip is off during a Pureplay strict-mode round"
        );
        assert_eq!(
            crate::autochip::send(&state.root, &state.bus_tx, 3, &state.auto_chip_shots),
            Err(crate::autochip::SolveError::RoundActive)
        );
        assert!(rx.try_recv().is_err(), "nothing emitted");
        assert_eq!(
            state
                .auto_chip_shots
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "no number taken"
        );

        drop(round);
        let Json(resp) = post_auto_chip(
            State(Arc::clone(&state)),
            Json(AutoChipRequest { yards: 3 }),
        )
        .await
        .expect("fires again once the round ends");
        assert_eq!(resp.shot_number, 1);
    }

    #[tokio::test]
    async fn auto_chip_needs_a_valid_simulation_environment() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| c.simulation.auto_chip = true);
        state
            .root
            .system
            .update(|c| c.simulation.temperature_c = 99.0);
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        let (status, Json(body)) = post_auto_chip(
            State(Arc::clone(&state)),
            Json(AutoChipRequest { yards: 3 }),
        )
        .await
        .expect_err("rejected");
        assert_eq!(status, StatusCode::CONFLICT);
        let error = body["error"].as_str().expect("error");
        assert!(
            error.starts_with("simulation environment invalid"),
            "{error}"
        );
        assert!(rx.try_recv().is_err(), "nothing emitted");

        // The refused chip took no number: the next one sent is #1.
        assert_eq!(
            state
                .auto_chip_shots
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        state
            .root
            .system
            .update(|c| c.simulation.temperature_c = 25.0);
        let Json(resp) = post_auto_chip(
            State(Arc::clone(&state)),
            Json(AutoChipRequest { yards: 3 }),
        )
        .await
        .expect("accepted");
        assert_eq!(resp.shot_number, 1);
    }

    #[tokio::test]
    async fn new_group_and_session_endpoints_emit_their_commands() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let mut rx = state.bus_tx.subscribe();
        assert_eq!(
            post_new_shot_group(State(Arc::clone(&state))).await,
            StatusCode::ACCEPTED
        );
        let msg = next_event(&mut rx);
        assert_eq!(msg.actor, "web");
        assert!(matches!(msg.event, FlighthookEvent::NewShotGroup));
        assert_eq!(
            post_new_shot_session(State(Arc::clone(&state))).await,
            StatusCode::ACCEPTED
        );
        let msg = next_event(&mut rx);
        assert_eq!(msg.actor, "web");
        assert!(matches!(msg.event, FlighthookEvent::NewShotSession));
    }

    #[tokio::test]
    async fn post_grouping_answers_after_the_outcome() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        // Stand-in for the system actor: apply and answer.
        let mut rx = state.bus_tx.subscribe();
        let responder = {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                loop {
                    let msg = rx.recv().await.expect("bus");
                    if let FlighthookEvent::ConfigCommand { request_id, action } = msg.event {
                        let ConfigAction::SetShotGrouping { mode } = *action else {
                            panic!("unexpected action {action:?}");
                        };
                        state.root.system.update(|c| c.shot_grouping = mode);
                        let _ = state.bus_tx.send(
                            FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
                                request_id,
                                restarted: Vec::new(),
                                stopped: Vec::new(),
                                started: Vec::new(),
                            })
                            .actor("system"),
                        );
                        return;
                    }
                }
            })
        };
        let Json(body) = post_shot_grouping(
            State(Arc::clone(&state)),
            Json(GroupingRequest {
                mode: ShotGrouping::ByClub,
            }),
        )
        .await
        .expect("applied");
        responder.await.expect("responder");
        assert_eq!(body.mode, ShotGrouping::ByClub);
        assert_eq!(
            state.root.system.snapshot().shot_grouping,
            ShotGrouping::ByClub
        );
    }

    #[tokio::test]
    async fn get_shot_groups_lists_ascending_with_server_mode_and_current() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let empty = get_shot_groups(State(Arc::clone(&state))).await.0;
        assert_eq!(empty.server, "webserver.0");
        assert_eq!(empty.mode, ShotGrouping::BySet);
        assert_eq!(empty.current, None);
        assert_eq!(empty.session, 0);
        assert!(empty.groups.is_empty());

        let mut events = Vec::new();
        let shot = |n: u32| {
            let key = ShotKey {
                shot_id: format!("s{n}"),
                shot_number: n,
            };
            vec![
                FlighthookEvent::shot_trigger(key.clone()),
                FlighthookEvent::Launch {
                    key: key.clone(),
                    launch: Box::new(Launch {
                        launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                        ..Launch::default()
                    }),
                    derived: None,
                },
                FlighthookEvent::ShotFinished { key, raw: None },
            ]
        };
        events.extend(shot(1));
        feed(&state, "mevo.0", events).await;
        let driver = FlighthookEvent::ClubInfo {
            club_info: ClubInfo { club: Club::Driver },
        };
        feed(&state, "openconnect_client.0", vec![driver]).await;
        feed(&state, "mevo.0", shot(2)).await;

        let listed = get_shot_groups(State(Arc::clone(&state))).await.0;
        assert_eq!(listed.mode, ShotGrouping::BySet);
        assert_eq!(listed.groups.len(), 2);
        assert_eq!(
            listed.groups.iter().map(|g| g.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(!listed.groups[0].open);
        assert_eq!(listed.groups[1].club, Some(Club::Driver));
        assert_eq!(
            listed.current.as_deref(),
            Some(listed.groups[1].id.as_str())
        );
    }

    #[tokio::test]
    async fn post_grouping_times_out_with_504() {
        let (state, _writer, _dir) = test_state();
        // No system actor answers the command.
        let res = set_shot_grouping(
            &state,
            GroupingRequest {
                mode: ShotGrouping::ByClub,
            },
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(res.err(), Some(StatusCode::GATEWAY_TIMEOUT));
        assert_eq!(
            state.root.system.snapshot().shot_grouping,
            ShotGrouping::BySet
        );
    }

    #[tokio::test]
    async fn settings_save_with_an_unknown_scope_is_400_with_a_json_error() {
        let (state, _writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let body = state.root.system.snapshot();
        let (status, Json(error)) = save_settings(
            &state,
            Some("nosuchtype.0"),
            body,
            Duration::from_millis(50),
        )
        .await
        .expect_err("rejected");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"], "unknown scope 'nosuchtype.0'");
        assert!(rx.try_recv().is_err(), "nothing emitted");
    }

    #[tokio::test]
    async fn settings_save_that_removes_the_web_server_or_adds_one_is_400() {
        let (state, _writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let live = state.root.system.snapshot();
        assert_eq!(live.webserver.len(), 1);
        let mut without = live.clone();
        without.webserver.clear();
        let mut second = live.clone();
        second.webserver.insert(
            "1".into(),
            flighthook::WebserverSection {
                name: "Web Server".into(),
                bind: "0.0.0.0:5881".into(),
            },
        );
        for (scope, body, message) in [
            (
                Some("webserver.0"),
                without.clone(),
                "the web server can't be removed",
            ),
            (None, without, "the web server can't be removed"),
            (
                Some("webserver.1"),
                second.clone(),
                "flighthook runs one web server; edit the existing one",
            ),
            (
                None,
                second,
                "flighthook runs one web server; edit the existing one",
            ),
        ] {
            let (status, Json(error)) =
                save_settings(&state, scope, body, Duration::from_millis(50))
                    .await
                    .expect_err("rejected");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{scope:?}");
            assert_eq!(error["error"], message, "{scope:?}");
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
        assert_eq!(state.root.system.snapshot(), live, "nothing saved");
    }

    /// Turning on, or adding as on, a card whose address an enabled card of
    /// its type has is `409` with the reason and saves nothing, for the
    /// whole config and for the card's scope. A card that is off, or on a
    /// free address, saves.
    #[tokio::test]
    async fn settings_save_that_enables_a_duplicate_address_is_409() {
        let (state, _writer, _dir) = test_state();
        let client = |address: &str, enabled| flighthook::OpenConnectClientSection {
            name: "Open Connect".into(),
            enabled,
            address: Some(address.into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
        };
        state.root.system.update(|c| {
            c.openconnect_client
                .insert("0".into(), client("127.0.0.1:921", true));
            c.openconnect_client
                .insert("1".into(), client("localhost:921", false));
        });
        let live = state.root.system.snapshot();
        let mut on = live.clone();
        on.openconnect_client.get_mut("1").unwrap().enabled = true;
        let mut added = live.clone();
        added
            .openconnect_client
            .insert("2".into(), client("127.0.0.1:921", true));
        let mut rx = state.bus_tx.subscribe();
        for (scope, body) in [
            (None, on.clone()),
            (Some("openconnect_client.1"), on),
            (Some("openconnect_client.2"), added.clone()),
            (None, added),
        ] {
            let (status, Json(error)) =
                save_settings(&state, scope, body, Duration::from_millis(50))
                    .await
                    .expect_err("refused");
            assert_eq!(status, StatusCode::CONFLICT, "{scope:?}");
            assert_eq!(
                error["error"], "An Open Connect card already uses 127.0.0.1:921",
                "{scope:?}"
            );
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
        assert_eq!(state.root.system.snapshot(), live, "nothing saved");

        // A card that stays off, and one turned on at a free address, save
        // (here the command times out: no system actor runs).
        let mut free = live.clone();
        free.openconnect_client.get_mut("1").unwrap().address = Some("127.0.0.1:9210".into());
        free.openconnect_client.get_mut("1").unwrap().enabled = true;
        for (scope, body) in [
            (Some("openconnect_client.1"), live.clone()),
            (Some("openconnect_client.1"), free),
        ] {
            let res = save_settings(&state, scope, body, Duration::from_millis(50)).await;
            assert_eq!(res.err().map(|e| e.0), Some(StatusCode::GATEWAY_TIMEOUT));
        }
    }

    /// A whole-config or `mevo.N`-scoped save with a `launch_only_spin` below
    /// 0, above 1000 or non-finite is 400 and saves nothing; a global save
    /// does not write Mevo sections, so it is not checked.
    #[tokio::test]
    async fn settings_save_with_an_invalid_launch_only_spin_is_400() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| {
            c.mevo
                .insert("0".into(), flighthook::MevoSection::default());
        });
        let mut rx = state.bus_tx.subscribe();
        let live = state.root.system.snapshot();
        for bad in [-1.0, 1000.5, f64::NAN, f64::INFINITY] {
            let mut body = live.clone();
            if let Some(m) = body.mevo.get_mut("0") {
                m.launch_only_spin = bad;
            }
            for scope in [None, Some("mevo.0")] {
                let (status, Json(error)) =
                    save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                        .await
                        .expect_err("rejected");
                assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} {scope:?}");
                let error = error["error"].as_str().expect("error");
                assert!(
                    error.starts_with("mevo.0: launch_only_spin must be 0 to 1000 rpm per mph"),
                    "{error}"
                );
            }
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
        assert_eq!(state.root.system.snapshot(), live, "nothing saved");

        // In range, including both ends, it goes through to the system actor.
        for ok in [0.0, 100.0, 1000.0] {
            let mut section = live.mevo["0"].clone();
            section.launch_only_spin = ok;
            let action = ConfigAction::UpsertMevo {
                index: "0".into(),
                section,
            };
            assert_eq!(invalid_setting(&action), None, "{ok}");
        }
        let mut body = live.clone();
        if let Some(m) = body.mevo.get_mut("0") {
            m.launch_only_spin = -1.0;
        }
        assert_eq!(
            invalid_setting(&ConfigAction::ReplaceGlobal {
                config: Box::new(body)
            }),
            None
        );
    }

    /// A save with a launch monitor `impact_lead_s` below 0, above 5 or
    /// non-finite is 400 and saves nothing, for every launch monitor type, in
    /// a whole-config and a scoped save; 0 and 5 are accepted.
    #[tokio::test]
    async fn settings_save_with_an_invalid_impact_lead_is_400() {
        let (state, _writer, _dir) = test_state();
        let ids = [
            "mevo.0",
            "r10.0",
            "square.0",
            "exputt.0",
            "mock_monitor.0",
            "openconnect_server.0",
        ];
        state.root.system.update(|c| {
            c.mevo.insert("0".into(), Default::default());
            c.r10.insert("0".into(), Default::default());
            c.square.insert("0".into(), Default::default());
            c.exputt.insert("0".into(), Default::default());
            c.mock_monitor.insert(
                "0".into(),
                flighthook::MockMonitorSection {
                    name: "Mock".into(),
                    enabled: true,
                    impact_lead_s: None,
                    driver: Default::default(),
                },
            );
            c.openconnect_server.insert("0".into(), Default::default());
        });
        let mut rx = state.bus_tx.subscribe();
        let live = state.root.system.snapshot();
        let set = |config: &mut FlighthookConfig, id: &str, v: Option<f64>| match id {
            "mevo.0" => config.mevo.get_mut("0").unwrap().impact_lead_s = v,
            "r10.0" => config.r10.get_mut("0").unwrap().impact_lead_s = v,
            "square.0" => config.square.get_mut("0").unwrap().impact_lead_s = v,
            "exputt.0" => config.exputt.get_mut("0").unwrap().impact_lead_s = v,
            "mock_monitor.0" => config.mock_monitor.get_mut("0").unwrap().impact_lead_s = v,
            _ => {
                config
                    .openconnect_server
                    .get_mut("0")
                    .unwrap()
                    .impact_lead_s = v
            }
        };
        for id in ids {
            for bad in [-0.1, 5.1, f64::NAN, f64::INFINITY] {
                let mut body = live.clone();
                set(&mut body, id, Some(bad));
                for scope in [None, Some(id)] {
                    let (status, Json(error)) =
                        save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                            .await
                            .expect_err("rejected");
                    assert_eq!(status, StatusCode::BAD_REQUEST, "{id} {bad} {scope:?}");
                    let error = error["error"].as_str().expect("error");
                    assert!(
                        error.starts_with(&format!("{id}: impact_lead_s "))
                            && error.ends_with("is outside 0 to 5 s"),
                        "{error}"
                    );
                }
            }
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
        assert_eq!(state.root.system.snapshot(), live, "nothing saved");
        for id in ids {
            for ok in [None, Some(0.0), Some(1.5), Some(5.0)] {
                let mut body = live.clone();
                set(&mut body, id, ok);
                let action = ConfigAction::ReplaceAll {
                    config: Box::new(body),
                };
                assert_eq!(invalid_setting(&action), None, "{id} {ok:?}");
            }
        }
    }

    /// A save with mock driver settings the driver refuses is 400 naming the
    /// card and the field, and saves nothing, in a whole-config and a
    /// scoped save; the driver's own limits are accepted.
    #[tokio::test]
    async fn settings_save_with_refused_driver_settings_is_400() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| {
            c.mock_monitor
                .insert("0".into(), flighthook::MockMonitorSection::default());
        });
        let mut rx = state.bus_tx.subscribe();
        let live = state.root.system.snapshot();
        type Field = fn(&mut flighthook::MockConfig) -> &mut f64;
        let fields: [(&str, Field); 4] = [
            ("shot_interval_s", |c| &mut c.shot_interval_s),
            ("arm_delay_s", |c| &mut c.arm_delay_s),
            ("ball_ready_delay_s", |c| &mut c.ball_ready_delay_s),
            ("post_shot_delay_s", |c| &mut c.post_shot_delay_s),
        ];
        for (field, value) in fields {
            for bad in [-1.0, 86_400.5, f64::NAN] {
                let mut body = live.clone();
                *value(&mut body.mock_monitor.get_mut("0").unwrap().driver) = bad;
                for scope in [None, Some("mock_monitor.0")] {
                    let (status, Json(error)) =
                        save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                            .await
                            .expect_err("rejected");
                    assert_eq!(status, StatusCode::BAD_REQUEST, "{field} {bad} {scope:?}");
                    assert_eq!(
                        error["error"],
                        format!("mock_monitor.0: {field}: must be 0 to 86400"),
                        "{scope:?}"
                    );
                }
            }
            for ok in [0.0, 86_400.0] {
                let mut body = live.clone();
                *value(&mut body.mock_monitor.get_mut("0").unwrap().driver) = ok;
                let action = ConfigAction::ReplaceAll {
                    config: Box::new(body),
                };
                assert_eq!(invalid_driver_setting(&live, &action), None, "{field} {ok}");
            }
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
        assert_eq!(state.root.system.snapshot(), live, "nothing saved");

        // Saved refused settings: a rename or the on/off switch passes the
        // checks (here the command times out: no system actor runs).
        state.root.system.update(|c| {
            c.mock_monitor.get_mut("0").unwrap().driver.shot_interval_s = -1.0;
        });
        let stored = state.root.system.snapshot();
        let mut renamed = stored.clone();
        renamed.mock_monitor.get_mut("0").unwrap().name = "Renamed".into();
        let mut off = stored.clone();
        off.mock_monitor.get_mut("0").unwrap().enabled = false;
        for body in [renamed, off] {
            for scope in [None, Some("mock_monitor.0")] {
                let res =
                    save_settings(&state, scope, body.clone(), Duration::from_millis(50)).await;
                assert_eq!(res.err().map(|e| e.0), Some(StatusCode::GATEWAY_TIMEOUT));
            }
        }
    }

    /// A card whose saved driver settings are refused still saves its host
    /// fields and its on/off switch, whole and scoped, as long as the save
    /// keeps the refused driver settings as they are (a NaN read from the
    /// file included); a change into other refused settings is refused, one
    /// into usable settings is not.
    #[test]
    fn a_card_with_refused_saved_driver_settings_saves_its_other_fields() {
        for stored in [-1.0, f64::NAN] {
            let mut saved = FlighthookConfig::default();
            let mut section = flighthook::MockMonitorSection::default();
            section.driver.shot_interval_s = stored;
            saved.mock_monitor.insert("0".into(), section);
            let edit = |f: fn(&mut flighthook::MockMonitorSection)| {
                let mut config = saved.clone();
                f(config.mock_monitor.get_mut("0").unwrap());
                config
            };
            let check = |config: FlighthookConfig| {
                let scoped = ConfigAction::UpsertMockMonitor {
                    index: "0".into(),
                    section: config.mock_monitor["0"].clone(),
                };
                let whole = ConfigAction::ReplaceAll {
                    config: Box::new(config),
                };
                let (whole, scoped) = (
                    invalid_driver_setting(&saved, &whole),
                    invalid_driver_setting(&saved, &scoped),
                );
                assert_eq!(whole, scoped);
                whole
            };
            assert_eq!(check(edit(|s| s.name = "Renamed".into())), None);
            assert_eq!(check(edit(|s| s.enabled = false)), None);
            assert_eq!(check(edit(|s| s.impact_lead_s = Some(1.0))), None);
            assert_eq!(
                check(edit(|s| s.driver.shot_interval_s = -2.0)).as_deref(),
                Some("mock_monitor.0: shot_interval_s: must be 0 to 86400")
            );
            assert_eq!(
                check(edit(|s| s.driver.arm_delay_s = -1.0)).as_deref(),
                Some("mock_monitor.0: shot_interval_s: must be 0 to 86400"),
                "the stored refusal still names its field"
            );
            assert_eq!(check(edit(|s| s.driver.shot_interval_s = 10.0)), None);
        }
        let mut added = FlighthookConfig::default();
        let mut section = flighthook::MockMonitorSection::default();
        section.driver.shot_interval_s = -1.0;
        added.mock_monitor.insert("1".into(), section);
        assert!(
            invalid_driver_setting(
                &FlighthookConfig::default(),
                &ConfigAction::ReplaceAll {
                    config: Box::new(added)
                }
            )
            .is_some(),
            "a new card's refused settings are refused"
        );
    }

    /// A save with a camera `latency_s` below 0, above 10 or non-finite is 400
    /// and saves nothing, in a whole-config and a scoped save; 0 and 10 are
    /// accepted.
    #[tokio::test]
    async fn settings_save_with_an_invalid_camera_latency_is_400() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| {
            c.camera.insert(
                "0".into(),
                flighthook::CameraSection {
                    camera: flighthook::CameraSection::TEST_PATTERN.into(),
                    ..flighthook::CameraSection::default()
                },
            );
        });
        let mut rx = state.bus_tx.subscribe();
        let live = state.root.system.snapshot();
        for bad in [-0.1, 10.1, f64::NAN, f64::INFINITY] {
            let mut body = live.clone();
            body.camera.get_mut("0").unwrap().latency_s = Some(bad);
            for scope in [None, Some("camera.0")] {
                let (status, Json(error)) =
                    save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                        .await
                        .expect_err("rejected");
                assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} {scope:?}");
                let error = error["error"].as_str().expect("error");
                assert!(
                    error.starts_with("camera.0: latency_s ")
                        && error.ends_with("is outside 0 to 10 s"),
                    "{error}"
                );
            }
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
        assert_eq!(state.root.system.snapshot(), live, "nothing saved");
        for ok in [None, Some(0.0), Some(2.5), Some(10.0)] {
            let mut body = live.clone();
            body.camera.get_mut("0").unwrap().latency_s = ok;
            let action = ConfigAction::ReplaceAll {
                config: Box::new(body.clone()),
            };
            assert_eq!(invalid_setting(&action), None, "{ok:?}");
            let upsert = ConfigAction::UpsertCamera {
                index: "0".into(),
                section: body.camera["0"].clone(),
            };
            assert_eq!(invalid_setting(&upsert), None, "{ok:?}");
        }
    }

    /// A camera `rotation_deg` other than 0/90/180/270, or a `yaw_deg` or
    /// `pitch_deg` outside -30 to 30, is 400 and saves nothing, in a
    /// whole-config and a scoped save.
    #[tokio::test]
    async fn settings_save_with_an_invalid_camera_rotation_or_angle_is_400() {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| {
            c.camera.insert(
                "0".into(),
                flighthook::CameraSection {
                    camera: flighthook::CameraSection::TEST_PATTERN.into(),
                    ..flighthook::CameraSection::default()
                },
            );
        });
        let mut rx = state.bus_tx.subscribe();
        let live = state.root.system.snapshot();
        type Set = fn(&mut flighthook::CameraSection);
        let cases: [(Set, &str); 4] = [
            (
                |c| c.rotation_deg = 45,
                "camera.0: rotation_deg 45 is not 0, 90, 180 or 270",
            ),
            (
                |c| c.yaw_deg = Some(40.0),
                "camera.0: yaw_deg 40 is outside -30 to 30",
            ),
            (
                |c| c.pitch_deg = Some(-31.0),
                "camera.0: pitch_deg -31 is outside -30 to 30",
            ),
            (
                |c| c.yaw_deg = Some(f64::NAN),
                "camera.0: yaw_deg NaN is outside -30 to 30",
            ),
        ];
        for (set, message) in cases {
            let mut body = live.clone();
            set(body.camera.get_mut("0").unwrap());
            for scope in [None, Some("camera.0")] {
                let (status, Json(error)) =
                    save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                        .await
                        .expect_err("rejected");
                assert_eq!(status, StatusCode::BAD_REQUEST, "{message} {scope:?}");
                assert_eq!(error["error"], message);
            }
        }
        assert!(rx.try_recv().is_err(), "nothing emitted");
        assert_eq!(state.root.system.snapshot(), live, "nothing saved");
        for rotation in [0, 90, 180, 270] {
            let mut body = live.clone();
            let camera = body.camera.get_mut("0").unwrap();
            camera.rotation_deg = rotation;
            camera.yaw_deg = Some(-30.0);
            camera.pitch_deg = Some(30.0);
            let upsert = ConfigAction::UpsertCamera {
                index: "0".into(),
                section: body.camera["0"].clone(),
            };
            assert_eq!(invalid_setting(&upsert), None, "{rotation}");
        }
    }

    #[tokio::test]
    async fn a_player_save_writes_that_section_alone() {
        let (state, _writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut body = state.root.system.snapshot();
        body.default_units = flighthook::UnitSystem::Metric;
        body.player = flighthook::PlayerSection {
            sex: flighthook::Sex::Female,
            height: Some(168.0),
        };
        let _ = save_settings(
            &state,
            Some("player"),
            body.clone(),
            Duration::from_millis(10),
        )
        .await;
        let msg = next_event(&mut rx);
        let FlighthookEvent::ConfigCommand { action, .. } = &msg.event else {
            panic!("not a config command: {:?}", msg.event);
        };
        assert!(matches!(
            action.as_ref(),
            ConfigAction::SetPlayer { section } if *section == body.player
        ));
        let before = state.root.system.snapshot();
        let sender =
            crate::bus::BusSender::new("system".into(), state.bus_tx.clone(), Arc::default());
        crate::actors::system::run_config_command(&msg.event, &state.root, &state.bus_tx, &sender);
        let after = state.root.system.snapshot();
        assert_eq!(after.player, body.player);
        assert_eq!(after.default_units, before.default_units);
        // A height out of range is refused, here and in a global save.
        body.player.height = Some(99.0);
        for scope in ["player", "global"] {
            let (status, Json(error)) =
                save_settings(&state, Some(scope), body.clone(), Duration::from_millis(10))
                    .await
                    .expect_err("refused");
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error["error"], "player: height 99 is outside 100 to 250 cm");
        }
    }

    /// An old UI's body tracking save that still carries `golfer_height`
    /// is accepted, and the height is not written back into the section.
    #[tokio::test]
    async fn a_body_tracking_save_with_a_golfer_height_drops_it() {
        let (state, _writer, dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let body: FlighthookConfig = serde_json::from_str(
            r#"{"body_tracking":{"mode":"historical","golfer_height":182.5}}"#,
        )
        .expect("body");
        let _ = save_settings(
            &state,
            Some("body_tracking"),
            body,
            Duration::from_millis(10),
        )
        .await;
        let msg = next_event(&mut rx);
        let sender =
            crate::bus::BusSender::new("system".into(), state.bus_tx.clone(), Arc::default());
        crate::actors::system::run_config_command(&msg.event, &state.root, &state.bus_tx, &sender);
        let after = state.root.system.snapshot();
        assert_eq!(
            after.body_tracking.mode,
            flighthook::BodyTrackingMode::Historical
        );
        assert_eq!(after.player.height, None, "only [body_tracking] is written");
        let text = std::fs::read_to_string(dir.0.join("config.toml")).expect("config");
        assert!(!text.contains("golfer_height"), "{text}");
    }

    #[tokio::test]
    async fn settings_save_without_an_outcome_is_504_with_a_json_error() {
        let (state, _writer, _dir) = test_state();
        // No system actor answers the command.
        let body = state.root.system.snapshot();
        for scope in [None, Some("global"), Some("webserver.0")] {
            let (status, Json(error)) =
                save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                    .await
                    .expect_err("timed out");
            assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{scope:?}");
            assert!(error["error"].is_string(), "{scope:?}");
        }
    }

    /// A client still on the `gspro` spelling: body section and scope.
    #[tokio::test]
    async fn settings_save_accepts_the_gspro_spelling() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        // Stand in for the system actor: apply every config command.
        let mut rx = state.bus_tx.subscribe();
        let (root, bus_tx) = (Arc::clone(&state.root), state.bus_tx.clone());
        let system = tokio::spawn(async move {
            let sender =
                crate::bus::BusSender::new("system".into(), bus_tx.clone(), Arc::default());
            let mut actions = Vec::new();
            while actions.len() < 2 {
                let msg = rx.recv().await.expect("bus open");
                if let FlighthookEvent::ConfigCommand { action, .. } = &msg.event {
                    actions.push((**action).clone());
                    crate::actors::system::run_config_command(&msg.event, &root, &bus_tx, &sender);
                }
            }
            actions
        });
        let post = |body: &str, scope: &str| {
            // As the `Json` extractor reads it.
            let body: FlighthookConfig = serde_json::from_str(body).expect("body parses");
            post_settings(
                State(Arc::clone(&state)),
                Query(SettingsQuery {
                    scope: Some(scope.into()),
                }),
                Json(body),
            )
        };

        // An address no actor can start on, so nothing dials out.
        let upsert = r#"{"gspro":{"0":{"name":"Muni Golf","address":"no address"}}}"#;
        let _ = post(upsert, "gspro.0").await.expect("saved");
        let saved = state.root.system.snapshot();
        assert_eq!(saved.openconnect_client["0"].name, "Muni Golf");

        let _ = post("{}", "gspro.0").await.expect("removed");
        assert!(state.root.system.snapshot().openconnect_client.is_empty());

        let actions = system.await.expect("system task");
        assert!(matches!(
            &actions[0],
            ConfigAction::UpsertOpenConnectClient { index, .. } if index == "0"
        ));
        assert!(matches!(
            &actions[1],
            ConfigAction::Remove { id } if id == "openconnect_client.0"
        ));
    }

    #[tokio::test]
    async fn a_rename_shows_in_the_status_without_a_restart() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        // The system actor: apply each config command and keep its outcome.
        let mut rx = state.bus_tx.subscribe();
        let (root, bus_tx) = (Arc::clone(&state.root), state.bus_tx.clone());
        let system = tokio::spawn(async move {
            let sender =
                crate::bus::BusSender::new("system".into(), bus_tx.clone(), Arc::default());
            let mut outcomes = Vec::new();
            while outcomes.len() < 2 {
                let msg = rx.recv().await.expect("bus open");
                match &msg.event {
                    FlighthookEvent::ConfigCommand { .. } => {
                        crate::actors::system::run_config_command(
                            &msg.event, &root, &bus_tx, &sender,
                        );
                    }
                    FlighthookEvent::ConfigOutcome { .. } => outcomes.push(msg.event),
                    _ => {}
                }
            }
            outcomes
        });
        let post = |name: &str| {
            let body: FlighthookConfig = serde_json::from_str(&format!(
                r#"{{"mock_monitor":{{"0":{{"name":"{name}"}}}}}}"#
            ))
            .expect("body parses");
            post_settings(
                State(Arc::clone(&state)),
                Query(SettingsQuery {
                    scope: Some("mock_monitor.0".into()),
                }),
                Json(body),
            )
        };
        let shown = |status: &StatusResponse| status.actors["mock_monitor.0"].name.clone();

        let _ = post("Bay Mock").await.expect("added");
        let outcome = |started: &[&str]| FlighthookEvent::ConfigOutcome {
            request_id: None,
            restarted: Vec::new(),
            stopped: Vec::new(),
            started: started.iter().map(|s| (*s).to_owned()).collect(),
        };
        feed(&state, "system", vec![outcome(&["mock_monitor.0"])]).await;
        let before = get_status(State(Arc::clone(&state))).await.0;
        assert_eq!(shown(&before), "Bay Mock");

        // Renamed: nothing restarts, and the status has the new name before
        // the web layer has seen the outcome.
        let Json(reply) = post("Range Mock").await.expect("renamed");
        assert!(reply.restarted.is_empty() && reply.stopped.is_empty());
        let now = get_status(State(Arc::clone(&state))).await.0;
        assert_eq!(shown(&now), "Range Mock");
        assert_eq!(
            now.actors["mock_monitor.0"].state,
            before.actors["mock_monitor.0"].state
        );

        let outcomes = system.await.expect("system task");
        assert!(matches!(
            &outcomes[1],
            FlighthookEvent::ConfigOutcome { restarted, stopped, started, .. }
                if restarted.is_empty() && stopped.is_empty() && started.is_empty()
        ));
        // The web layer's own copy takes the name from the outcome.
        feed(&state, "system", vec![outcomes[1].clone()]).await;
        assert_eq!(
            state.actors.read().await["mock_monitor.0"].name,
            "Range Mock"
        );
        state.root.stop_actor("mock_monitor.0");
    }

    #[tokio::test]
    async fn settings_save_with_the_global_scope_sends_replace_global() {
        let (state, _writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let body = state.root.system.snapshot();
        let _ = save_settings(&state, Some("global"), body, Duration::from_millis(10)).await;
        let msg = next_event(&mut rx);
        let FlighthookEvent::ConfigCommand { action, .. } = msg.event else {
            panic!("not a config command: {:?}", msg.event);
        };
        assert!(matches!(*action, ConfigAction::ReplaceGlobal { .. }));
    }

    #[tokio::test]
    async fn a_body_tracking_save_writes_that_section_alone() {
        let (state, _writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        // The UI's copy is out of date on everything else.
        let mut body = state.root.system.snapshot();
        body.default_units = flighthook::UnitSystem::Metric;
        body.replay.keep_shots = 5;
        body.body_tracking.min_score = 0.55;
        let _ = save_settings(
            &state,
            Some("body_tracking"),
            body.clone(),
            Duration::from_millis(10),
        )
        .await;
        let msg = next_event(&mut rx);
        let FlighthookEvent::ConfigCommand { action, .. } = &msg.event else {
            panic!("not a config command: {:?}", msg.event);
        };
        assert!(matches!(
            action.as_ref(),
            ConfigAction::SetBodyTracking { section } if *section == body.body_tracking
        ));
        // Applied: body tracking changes, nothing else does.
        let before = state.root.system.snapshot();
        let sender =
            crate::bus::BusSender::new("system".into(), state.bus_tx.clone(), Arc::default());
        crate::actors::system::run_config_command(&msg.event, &state.root, &state.bus_tx, &sender);
        let after = state.root.system.snapshot();
        assert_eq!(after.body_tracking, body.body_tracking);
        assert_eq!(after.default_units, before.default_units);
        assert_eq!(after.replay, before.replay);
        // A value out of range is refused, nothing sent.
        body.body_tracking.min_score = 2.0;
        let (status, Json(error)) = save_settings(
            &state,
            Some("body_tracking"),
            body,
            Duration::from_millis(10),
        )
        .await
        .expect_err("refused");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|e| e.starts_with("body_tracking: min_score")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn clear_endpoint_emits_its_command_and_the_history_and_export_empty() {
        let (state, writer, _dir) = test_state();
        let state = Arc::new(state);
        writer.set_club_info(ClubInfo { club: Club::Iron7 });
        let key = ShotKey {
            shot_id: "s1".into(),
            shot_number: 1,
        };
        feed(
            &state,
            "mevo.0",
            vec![
                FlighthookEvent::ClubInfo {
                    club_info: ClubInfo { club: Club::Iron7 },
                },
                FlighthookEvent::shot_trigger(key.clone()),
                FlighthookEvent::Launch {
                    key: key.clone(),
                    launch: Box::new(Launch {
                        launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                        ..Launch::default()
                    }),
                    derived: None,
                },
                FlighthookEvent::ShotFinished { key, raw: None },
            ],
        )
        .await;
        let export = |format: &str| {
            get_shots_export(
                State(Arc::clone(&state)),
                Query(ExportQuery {
                    format: Some(format.into()),
                    units: None,
                    limit: None,
                    raw: None,
                }),
            )
        };
        assert_eq!(body_text(export("jsonl").await).await.lines().count(), 1);

        let mut rx = state.bus_tx.subscribe();
        assert_eq!(
            post_clear_shot_history(State(Arc::clone(&state))).await,
            StatusCode::ACCEPTED
        );
        let msg = next_event(&mut rx);
        assert_eq!(msg.actor, "web");
        assert!(matches!(msg.event, FlighthookEvent::ClearShotHistory));
        assert_eq!(
            serde_json::to_string(&msg).expect("ser"),
            r#"{"actor":"web","event":{"kind":"clear_shot_history"}}"#
        );
        assert!(rx.try_recv().is_err(), "one event");

        // The web layer applies the command it put on the bus.
        feed(&state, "web", vec![msg.event]).await;
        let shots = get_shots(
            State(Arc::clone(&state)),
            Query(ShotsQuery {
                limit: 50,
                units: None,
            }),
        )
        .await
        .0;
        assert!(shots.is_empty());
        let groups = get_shot_groups(State(Arc::clone(&state))).await.0;
        assert!(groups.groups.is_empty());
        assert_eq!(groups.current, None);
        assert_eq!(groups.session, 1);
        assert_eq!(body_text(export("jsonl").await).await, "");
        let csv = body_text(export("csv").await).await;
        assert_eq!(csv.lines().count(), 1, "the header only: {csv}");
    }

    #[tokio::test]
    async fn get_shot_groups_reports_the_default_view_and_session_and_changes_no_set() {
        let (state, writer, _dir) = test_state();
        let state = Arc::new(state);
        writer.set_club_info(ClubInfo { club: Club::Iron7 });
        let key = ShotKey {
            shot_id: "s1".into(),
            shot_number: 1,
        };
        feed(
            &state,
            "mevo.0",
            vec![
                FlighthookEvent::ClubInfo {
                    club_info: ClubInfo { club: Club::Iron7 },
                },
                FlighthookEvent::shot_trigger(key.clone()),
                FlighthookEvent::Launch {
                    key: key.clone(),
                    launch: Box::new(Launch {
                        launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                        ..Launch::default()
                    }),
                    derived: None,
                },
                FlighthookEvent::ShotFinished { key, raw: None },
            ],
        )
        .await;
        let before = get_shot_groups(State(Arc::clone(&state))).await.0;
        assert_eq!(before.session, 0);
        assert_eq!(before.groups.len(), 1);

        // The default view is read from config; the open set is untouched.
        let mut rx = state.bus_tx.subscribe();
        for mode in [ShotGrouping::ByClub, ShotGrouping::None] {
            state.root.system.update(|c| c.shot_grouping = mode);
            let listed = get_shot_groups(State(Arc::clone(&state))).await.0;
            assert_eq!(listed.mode, mode);
            assert_eq!(listed.groups, before.groups);
            assert_eq!(listed.current, before.current);
        }
        assert!(rx.try_recv().is_err(), "no set closed or created");

        feed(&state, "web", vec![FlighthookEvent::NewShotSession]).await;
        let after = get_shot_groups(State(Arc::clone(&state))).await.0;
        assert_eq!(after.session, 1);
        assert_eq!(after.current, None);
        assert!(after.groups.iter().all(|g| !g.open && g.session == 0));
    }

    #[tokio::test]
    async fn get_status_says_where_clips_go_and_whether_cameras_work() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let Json(status) = get_status(State(Arc::clone(&state))).await;
        assert_eq!(
            status.clip_dir.as_deref(),
            Some(
                crate::state::clips::default_dir()
                    .display()
                    .to_string()
                    .as_str()
            )
        );
        assert_eq!(status.cameras_supported, crate::state::cameras::supported());
        state
            .root
            .system
            .update(|c| c.replay.clip_dir = Some("/srv/swings".into()));
        let Json(status) = get_status(State(Arc::clone(&state))).await;
        assert_eq!(status.clip_dir.as_deref(), Some("/srv/swings"));
    }

    /// A web state over a config file holding `contents`.
    fn state_over(contents: &str) -> (WebState, std::path::PathBuf, TempDir) {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-web-config-test-{}",
            crate::state::config::generate_id()
        )));
        std::fs::create_dir_all(&dir.0).expect("mkdir");
        let path = dir.0.join("config.toml");
        std::fs::write(&path, contents).expect("write");
        let (root, _writer) = crate::state::SystemState::new(path.clone());
        let (bus_tx, _) = broadcast::channel(1024);
        let state = web_state(Arc::new(root), bus_tx, "webserver.0");
        (state, path, dir)
    }

    /// The status carries the round while one is active, with its holder,
    /// and leaves it out otherwise.
    #[tokio::test]
    async fn get_status_carries_the_round_while_one_is_active() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let json =
            serde_json::to_value(get_status(State(Arc::clone(&state))).await.0).expect("ser");
        assert!(json.get("round").is_none(), "{json}");
        let round = state
            .root
            .round
            .begin_round(&crate::state::round::RoundRequest {
                holder: "Pureplay",
                feeds: &[],
            })
            .expect("a round");
        let json =
            serde_json::to_value(get_status(State(Arc::clone(&state))).await.0).expect("ser");
        assert_eq!(
            json["round"],
            serde_json::json!({"active": true, "holder": "Pureplay"})
        );
        drop(round);
        assert!(!get_status(State(state)).await.0.round.active);
    }

    #[tokio::test]
    async fn get_status_carries_the_config_error_until_a_save_backs_the_file_up() {
        let (state, _writer, _dir) = test_state();
        let status = get_status(State(Arc::new(state))).await.0;
        assert_eq!(status.config_error, None);
        assert!(
            !serde_json::to_string(&status)
                .expect("ser")
                .contains("config_error")
        );

        let broken = "[body_tracking]\nmode = \"liv\"\n";
        let (state, path, dir) = state_over(broken);
        let state = Arc::new(state);
        let status = get_status(State(Arc::clone(&state))).await.0;
        let error = status.config_error.expect("the config error");
        assert_eq!(error.path, path.display().to_string());
        assert!(error.error.starts_with("line 2, column 8: "), "{error:?}");
        let json =
            serde_json::to_value(get_status(State(Arc::clone(&state))).await.0).expect("ser");
        assert_eq!(json["config_error"]["path"], error.path);

        // No system actor here: the save times out, after the backup.
        let saved = save_settings(
            &state,
            Some("global"),
            state.root.system.snapshot(),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(saved.err().map(|e| e.0), Some(StatusCode::GATEWAY_TIMEOUT));
        assert_eq!(get_status(State(state)).await.0.config_error, None);
        let backup = dir
            .0
            .join(format!("config.pre-{}.toml", env!("CARGO_PKG_VERSION")));
        assert_eq!(std::fs::read_to_string(backup).expect("backup"), broken);
    }

    #[tokio::test]
    async fn a_save_is_refused_when_the_unusable_file_cannot_be_backed_up() {
        let (_state, _path, dir) = state_over("");
        // A directory in the file's place cannot be read, so not copied.
        let folder = dir.0.join("folder.toml");
        std::fs::create_dir(&folder).expect("mkdir");
        let (root, _writer) = crate::state::SystemState::new(folder.clone());
        let (bus_tx, _) = broadcast::channel(1024);
        let state = web_state(Arc::new(root), bus_tx, "webserver.0");
        let saved = save_settings(
            &state,
            Some("global"),
            state.root.system.snapshot(),
            Duration::from_millis(50),
        )
        .await;
        let Err(refused) = saved else {
            panic!("the save was not refused");
        };
        assert_eq!(refused.0, StatusCode::INTERNAL_SERVER_ERROR);
        let message = refused.1.0["error"].as_str().expect("message").to_string();
        assert!(message.contains("cannot be backed up"), "{message}");
        assert!(message.contains("folder.toml"), "{message}");
        assert!(state.root.system.load_error().is_some());
        assert!(folder.is_dir());
    }

    #[tokio::test]
    async fn get_status_carries_the_club() {
        let (state, writer, _dir) = test_state();
        let state = Arc::new(state);
        let none = get_status(State(Arc::clone(&state))).await.0;
        assert_eq!(none.club, None);
        assert!(!serde_json::to_string(&none).expect("ser").contains("club"));
        writer.set_club_info(ClubInfo { club: Club::Iron7 });
        let set = get_status(State(Arc::clone(&state))).await.0;
        assert_eq!(set.club, Some(Club::Iron7));
    }

    #[tokio::test]
    async fn get_status_carries_the_hand() {
        let (state, writer, _dir) = test_state();
        let state = Arc::new(state);
        let none = get_status(State(Arc::clone(&state))).await.0;
        assert_eq!(none.handed, None);
        assert!(
            !serde_json::to_string(&none)
                .expect("ser")
                .contains("handed")
        );
        writer.set_handed(Handedness::Left);
        let set = get_status(State(Arc::clone(&state))).await.0;
        assert_eq!(set.handed, Some(Handedness::Left));
        assert!(
            serde_json::to_string(&set)
                .expect("ser")
                .contains(r#""handed":"lh""#)
        );
    }
}

#[cfg(test)]
mod reconnect_tests {
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::super::tests::test_state;
    use super::*;
    use crate::actors::Actor as _;
    use crate::bus::{BusReceiver, BusSender};
    use crate::state::SystemState;

    /// A web state whose config holds a mock monitor and a camera.
    fn configured() -> (
        Arc<WebState>,
        crate::state::GameStateWriter,
        super::super::tests::TempDir,
    ) {
        let (state, writer, dir) = test_state();
        state.root.system.update(|c| {
            c.mock_monitor.insert(
                "0".into(),
                flighthook::MockMonitorSection {
                    name: "Mock".into(),
                    enabled: true,
                    impact_lead_s: None,
                    driver: Default::default(),
                },
            );
            c.camera
                .insert("0".into(), flighthook::CameraSection::default());
        });
        (Arc::new(state), writer, dir)
    }

    /// Stand-in for the system actor: answers every reconnect after
    /// `delay`, as restarted unless `refuse`. Counts the commands.
    fn answer(state: &Arc<WebState>, delay: Duration, refuse: bool) -> Arc<AtomicUsize> {
        let mut rx = state.bus_tx.subscribe();
        let bus_tx = state.bus_tx.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&count);
        tokio::spawn(async move {
            while let Ok(msg) = rx.recv().await {
                if let FlighthookEvent::ConfigCommand { request_id, action } = msg.event {
                    let ConfigAction::Reconnect { id } = *action else {
                        panic!("unexpected action {action:?}");
                    };
                    counted.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    let restarted = if refuse { Vec::new() } else { vec![id] };
                    let _ = bus_tx.send(
                        FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
                            request_id,
                            restarted,
                            stopped: Vec::new(),
                            started: Vec::new(),
                        })
                        .actor("system"),
                    );
                }
            }
        });
        count
    }

    type Reply = Result<(StatusCode, Json<PostSettingsResponse>), SettingsError>;

    fn code(result: &Reply) -> StatusCode {
        match result {
            Ok((status, _)) => *status,
            Err((status, _)) => *status,
        }
    }

    fn error_of(result: Reply) -> serde_json::Value {
        let Err((_, Json(body))) = result else {
            panic!("not an error")
        };
        body["error"].clone()
    }

    #[tokio::test]
    async fn a_reconnect_answers_with_the_restarted_actor() {
        let (state, _writer, _dir) = configured();
        let count = answer(&state, Duration::ZERO, false);
        let (status, Json(body)) = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT)
            .await
            .expect("reconnected");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.restarted, ["mock_monitor.0"]);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(state.reconnecting.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cameras_general_cards_and_unknown_ids_are_refused_without_a_command() {
        let (state, _writer, _dir) = configured();
        let mut rx = state.bus_tx.subscribe();
        for (id, status) in [
            ("camera.0", StatusCode::BAD_REQUEST),
            ("webserver.0", StatusCode::BAD_REQUEST),
            ("system", StatusCode::BAD_REQUEST),
            ("mock_monitor.4", StatusCode::NOT_FOUND),
            ("mevo.0", StatusCode::NOT_FOUND),
        ] {
            let result = reconnect(&state, id, Duration::from_millis(50)).await;
            assert_eq!(code(&result), status, "{id}");
            let error = error_of(result);
            assert!(error.as_str().is_some_and(|e| e.contains(id)), "{error}");
        }
        assert!(rx.try_recv().is_err(), "nothing reached the bus");
    }

    #[tokio::test]
    async fn a_second_reconnect_of_the_same_actor_is_refused_while_one_is_in_flight() {
        let (state, _writer, _dir) = configured();
        let count = answer(&state, Duration::from_millis(200), false);
        let (first, second) = tokio::join!(
            reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT),
            reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT),
        );
        assert_eq!(code(&first), StatusCode::OK);
        assert_eq!(code(&second), StatusCode::CONFLICT);
        assert_eq!(count.load(Ordering::SeqCst), 1, "one reconnect ran");
        // Once it is done, another may start.
        let again = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
        assert_eq!(code(&again), StatusCode::OK);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    /// A reconnect still running when the request has waited its time is
    /// `202`, and the actor stays in flight until the outcome arrives: a
    /// second press meanwhile cannot queue a second restart.
    #[tokio::test]
    async fn a_reconnect_still_running_is_accepted_and_keeps_its_guard() {
        let (state, _writer, _dir) = configured();
        let count = answer(&state, Duration::from_millis(400), false);
        let first = reconnect(&state, "mock_monitor.0", Duration::from_millis(50)).await;
        assert_eq!(code(&first), StatusCode::ACCEPTED);
        assert!(first.expect("accepted").1.restarted.is_empty());
        let second = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
        assert_eq!(code(&second), StatusCode::CONFLICT);
        assert!(
            error_of(second)
                .as_str()
                .is_some_and(|e| e.contains("already in progress"))
        );
        assert_eq!(count.load(Ordering::SeqCst), 1, "one command on the bus");

        // The outcome arrives: the guard goes with it.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !state.reconnecting.lock().unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline, "guard released");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let third = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
        assert_eq!(code(&third), StatusCode::OK);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_reconnect_that_starts_nothing_is_a_conflict_with_the_reason() {
        let (state, _writer, _dir) = configured();
        let _count = answer(&state, Duration::ZERO, true);
        let result = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
        assert_eq!(code(&result), StatusCode::CONFLICT);
        assert_eq!(error_of(result), "'mock_monitor.0' was not reconnected");
    }

    /// A reconnect that starts its actor again by itself later, once the
    /// old connection has closed or another command is done restarting it,
    /// is accepted.
    #[tokio::test]
    async fn a_reconnect_that_completes_by_itself_is_accepted() {
        let (state, _writer, _dir) = configured();
        let _count = answer(&state, Duration::ZERO, true);
        state.root.begin_settling("mock_monitor.0");
        let result = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
        assert_eq!(code(&result), StatusCode::ACCEPTED);
        state.root.end_settling("mock_monitor.0");

        crate::actors::start_actor(
            "mock_monitor.0".into(),
            Box::new(crate::actors::mock::MockActor::new(
                flighthook::MockMonitorSection::default(),
            )),
            &state.root,
            &state.bus_tx,
        );
        state.root.close_in_background("mock_monitor.0");
        let result = reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await;
        assert_eq!(code(&result), StatusCode::ACCEPTED);
        assert!(result.expect("accepted").1.restarted.is_empty());
        assert!(state.reconnecting.lock().unwrap().is_empty());
    }

    #[test]
    fn the_config_timeout_outlasts_the_longest_stop_wait() {
        use crate::actors::{MAX_STOP_WAIT, STOP_WAIT};
        let exputt = crate::actors::exputt::ExPuttActor { address: None };
        assert_eq!(exputt.stop_wait(), MAX_STOP_WAIT);
        for wait in [STOP_WAIT, crate::actors::square::STOP_WAIT] {
            assert!(wait <= MAX_STOP_WAIT, "{wait:?}");
        }
        assert!(CONFIG_TIMEOUT >= MAX_STOP_WAIT + START_ROOM);
        assert!(RECONNECT_LINGER >= CONFIG_TIMEOUT);
    }

    /// An actor that holds its device for `holds` after a stop, waited for
    /// `wait`.
    struct Lingers {
        holds: Duration,
        wait: Duration,
    }

    impl crate::actors::Actor for Lingers {
        fn start(
            &self,
            _state: Arc<SystemState>,
            _sender: BusSender,
            receiver: BusReceiver,
        ) -> std::thread::JoinHandle<()> {
            let holds = self.holds;
            std::thread::spawn(move || {
                while !receiver.is_shutdown() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(holds);
            })
        }

        fn stop_wait(&self) -> Duration {
            self.wait
        }
    }

    /// A save sent while a reconnect waits out its actor's whole stop wait
    /// is answered at once, not behind the reconnect; the reconnect, which
    /// finds the old connection still closing, is accepted: it completes by
    /// itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_save_during_a_long_reconnect_is_answered_at_once() {
        let (state, writer, _dir) = configured();
        let shutdown = Arc::new(AtomicBool::new(false));
        crate::actors::system::start_for_test(
            writer,
            &state.root,
            &state.bus_tx,
            Arc::clone(&shutdown),
        );
        let wait = Duration::from_millis(800);
        crate::actors::start_actor(
            "mock_monitor.0".into(),
            Box::new(Lingers {
                holds: Duration::from_secs(2),
                wait,
            }),
            &state.root,
            &state.bus_tx,
        );
        let at = std::time::Instant::now();
        let reconnecting = {
            let state = Arc::clone(&state);
            tokio::spawn(async move { reconnect(&state, "mock_monitor.0", CONFIG_TIMEOUT).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let saved = save_settings(
            &state,
            Some("mock_monitor.0"),
            state.root.system.snapshot(),
            CONFIG_TIMEOUT,
        )
        .await;
        let took = at.elapsed();
        assert!(saved.is_ok(), "{:?}", saved.err());
        assert!(took < wait, "not behind the reconnect: {took:?}");
        let result = reconnecting.await.expect("reconnect");
        assert_eq!(code(&result), StatusCode::ACCEPTED);
        assert!(at.elapsed() >= wait, "after the stop wait");
        shutdown.store(true, Ordering::SeqCst);
        state.root.stop_all_and_wait(Duration::from_secs(3));
    }

    /// POST `path` with no body: status code and body.
    fn post(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
        let mut stream = std::net::TcpStream::connect(addr).expect("connect");
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).expect("request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response");
        let code = response
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .expect("status");
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_owned())
            .unwrap_or_default();
        (code, body)
    }

    #[test]
    fn the_route_reconnects_a_running_monitor() {
        let (state, writer, _dir) = configured();
        let root = Arc::clone(&state.root);
        let bus = state.bus_tx.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        crate::actors::system::start_for_test(writer, &root, &bus, Arc::clone(&shutdown));
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port");
        let web = super::super::WebActor::new(addr);
        let web_sender = BusSender::new("webserver.0".into(), bus.clone(), Arc::default());
        let web_thread = web.start(
            Arc::clone(&root),
            web_sender.clone(),
            web_sender.subscribe(),
        );
        crate::actors::start_actor(
            "mock_monitor.0".into(),
            Box::new(crate::actors::mock::MockActor::new(
                flighthook::MockMonitorSection::default(),
            )),
            &root,
            &bus,
        );
        let before = root.actor_thread("mock_monitor.0");
        // The server is up once it answers.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::net::TcpStream::connect(addr).is_err() {
            assert!(std::time::Instant::now() < deadline, "web server up");
            std::thread::sleep(Duration::from_millis(20));
        }

        let (status, body) = post(addr, "/api/actors/mock_monitor.0/reconnect");
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""restarted":["mock_monitor.0"]"#), "{body}");
        assert_ne!(root.actor_thread("mock_monitor.0"), before);
        assert_eq!(post(addr, "/api/actors/camera.0/reconnect").0, 400);
        assert_eq!(post(addr, "/api/actors/mock_monitor.3/reconnect").0, 404);
        assert_eq!(post(addr, "/api/actors/mock_monitor.0/reconnect/x").0, 404);

        web.stop();
        let _ = web_thread.join();
        root.stop_and_wait("mock_monitor.0");
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod live_tests {
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
            let resp =
                get_camera_live_with(&state, "camera.0", seq - 1, Duration::from_secs(5)).await;
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
}

#[cfg(test)]
mod body_tracking_models_tests {
    use super::super::tests::test_state;
    use super::*;
    use flighthook::{BodyTrackingModels, ModelFilesState};

    async fn read(resp: Response) -> (StatusCode, BodyTrackingModels) {
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, serde_json::from_slice(&bytes).expect("models json"))
    }

    async fn get(state: &Arc<WebState>) -> BodyTrackingModels {
        read(get_body_tracking_models(State(Arc::clone(state))).await)
            .await
            .1
    }

    async fn settle(state: &Arc<WebState>) -> BodyTrackingModels {
        for _ in 0..500 {
            let m = get(state).await;
            if m.state != ModelFilesState::Downloading {
                return m;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the download did not end");
    }

    #[tokio::test]
    async fn the_models_download_only_when_posted_and_stop_when_deleted() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        let models_dir = state.root.pose.models.dir();
        // Missing, nothing downloading, nothing written.
        let m = get(&state).await;
        assert_eq!(m.state, ModelFilesState::Missing);
        assert_eq!(m.model, flighthook::PoseModel::RtmposeM);
        assert_eq!(m.files.len(), 2);
        assert!(m.download.is_none() && m.error.is_none());
        assert_eq!(m.dir, models_dir.display().to_string());
        assert!(!models_dir.exists());
        // Started; asking again starts nothing new.
        let (code, m) =
            read(post_body_tracking_models_download(State(Arc::clone(&state))).await).await;
        assert_eq!(code, StatusCode::ACCEPTED);
        assert_eq!(m.state, ModelFilesState::Downloading);
        let (code, again) =
            read(post_body_tracking_models_download(State(Arc::clone(&state))).await).await;
        assert_eq!(code, StatusCode::ACCEPTED);
        assert_eq!(again.state, ModelFilesState::Downloading);
        // Canceled: missing at once, the partial file goes.
        let (code, m) =
            read(delete_body_tracking_models_download(State(Arc::clone(&state))).await).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(m.state, ModelFilesState::Missing);
        assert!(m.error.is_none());
        for _ in 0..200 {
            let parts = std::fs::read_dir(&models_dir)
                .map(|d| d.count())
                .unwrap_or(0);
            if parts == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            std::fs::read_dir(&models_dir)
                .map(|d| d.count())
                .unwrap_or(0),
            0,
            "no partial file left"
        );
        // Canceling with nothing running is fine.
        let (code, _) =
            read(delete_body_tracking_models_download(State(Arc::clone(&state))).await).await;
        assert_eq!(code, StatusCode::OK);
        // Downloaded to the end: ready, and a POST then has nothing to do.
        read(post_body_tracking_models_download(State(Arc::clone(&state))).await).await;
        let m = settle(&state).await;
        assert_eq!(m.state, ModelFilesState::Ready);
        assert!(m.files.iter().all(|f| f.present));
        let (code, m) =
            read(post_body_tracking_models_download(State(Arc::clone(&state))).await).await;
        assert_eq!((code, m.state), (StatusCode::OK, ModelFilesState::Ready));
        let _ = std::fs::remove_dir_all(&models_dir);
    }

    #[tokio::test]
    async fn the_state_follows_the_configured_model() {
        let (state, _writer, _dir) = test_state();
        let state = Arc::new(state);
        state
            .root
            .system
            .update(|c| c.body_tracking.model = flighthook::PoseModel::RtmposeS);
        let m = get(&state).await;
        assert_eq!(m.model, flighthook::PoseModel::RtmposeS);
        assert_eq!(m.files[0].name, "rtmpose-s-halpe26.onnx");
        assert_eq!(m.files[1].host, "github.com");
    }
}
