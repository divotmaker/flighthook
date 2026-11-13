//! Networking — ehttp REST + ewebsock WebSocket.
//!
//! Async results are placed in a shared `Pending` queue (Arc<Mutex>)
//! that the app drains each frame. This avoids touching egui's internal
//! data store from async callbacks.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::replay::Window;
use crate::types::{
    self, AutoChipRequest, CameraListResponse, Club, FindMatRequest, FlighthookConfig, LivePose,
    PostSettingsResponse, ShotData, ShotGrouping, StatusResponse,
};

// ---------------------------------------------------------------------------
// Pending results queue — shared between async callbacks and the app.
// Arc<Mutex> instead of Rc<RefCell> because ehttp 0.6 requires Send callbacks.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Pending {
    pub status: Option<StatusResponse>,
    pub shots: Option<Vec<ShotData>>,
    /// The shot history generation `shots` was requested under.
    pub shots_generation: u64,
    pub groups: Option<flighthook::BlocksResponse>,
    /// The shot history generation `groups` was requested under.
    pub groups_generation: u64,
    pub settings: Option<FlighthookConfig>,
    /// The reply to a `POST /api/settings`: the response, or why the save
    /// cannot be taken as applied.
    pub settings_save: Option<Result<PostSettingsResponse, String>>,
    /// A New set or New session sent over REST has been answered: the sets
    /// are fetched again.
    pub groups_posted: bool,
    /// The reply to a `POST /api/auto-chip`: sent, or why not.
    pub auto_chip: Option<Result<(), String>>,
    /// The reply to a `POST /api/tournament`: applied, or why not.
    pub tournament: Option<Result<(), String>>,
    /// The reply to `GET /api/cameras`: the camera list, or why not.
    pub cameras: Option<Result<CameraListResponse, String>>,
    /// Still frames by camera actor: the JPEG, or why there is none.
    pub stills: Vec<(String, Result<Vec<u8>, String>)>,
    /// Answered reconnects, by actor: how, or why not.
    pub reconnects: Vec<(String, Result<Reconnected, String>)>,
    /// Replies for the replay view.
    pub(crate) replay: Vec<ReplayReply>,
    /// Replies for the Live view's cameras: the view's number and its
    /// reply.
    pub(crate) live: Vec<(u64, LiveReply)>,
    /// The reply to a body tracking save made at once: saved, or why not.
    pub(crate) tuning: Option<Result<(), String>>,
    /// Replies about body tracking's model files: which call, and the
    /// state, or why not.
    pub(crate) body_models: Vec<(ModelsCall, Result<flighthook::BodyTrackingModels, String>)>,
}

/// A request about body tracking's model files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelsCall {
    /// `GET /api/body_tracking/models`.
    State,
    /// `POST /api/body_tracking/models/download`.
    Download,
    /// `DELETE /api/body_tracking/models/download`.
    Cancel,
}

impl ModelsCall {
    /// The request's method and URL.
    pub(crate) fn request(self) -> (&'static str, String) {
        let base = api_base();
        match self {
            Self::State => ("GET", format!("{base}/api/body_tracking/models")),
            Self::Download => ("POST", format!("{base}/api/body_tracking/models/download")),
            Self::Cancel => (
                "DELETE",
                format!("{base}/api/body_tracking/models/download"),
            ),
        }
    }
}

/// The reply to a `GET /api/cameras/{actor}/live`.
#[derive(Debug, PartialEq)]
pub(crate) enum LiveReply {
    /// A newer frame, its sequence number, the golfer's keypoints on it when
    /// body tracking ran (`X-Pose`), and body tracking's state.
    Frame {
        seq: u64,
        jpeg: Vec<u8>,
        pose: Option<LivePose>,
        state: PoseState,
    },
    /// None arrived within the server's wait (`204`), and body tracking's
    /// state.
    Nothing(PoseState),
    /// The request failed: why.
    Failed(String),
}

/// Body tracking's state as a live reply carries it (`X-Pose-State`,
/// `X-Pose-Error`): `off`, `loading`, `missing`, `downloading`, `ready` or
/// `failed` and why. Empty from a server without body tracking.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PoseState {
    pub(crate) state: String,
    pub(crate) error: Option<String>,
}

impl PoseState {
    fn of(resp: &ehttp::Response) -> Self {
        Self {
            state: resp
                .headers
                .get("x-pose-state")
                .unwrap_or_default()
                .to_owned(),
            error: resp.headers.get("x-pose-error").map(str::to_owned),
        }
    }
}

/// A reply for the replay view, tagged with the replay it was asked for.
#[derive(Debug)]
pub(crate) enum ReplayReply {
    /// A camera's clip sidecar.
    Sidecar {
        replay: u64,
        camera: String,
        result: Result<stance::sidecar::Sidecar, String>,
    },
    /// The bytes of a run of a camera's frames.
    Frames {
        replay: u64,
        camera: String,
        window: Window,
        result: Result<Vec<u8>, String>,
    },
    /// A camera's clip keypoints (body tracking).
    Pose {
        replay: u64,
        camera: String,
        result: Result<types::PoseTrackFile, String>,
    },
    /// A camera's clip swing analysis.
    Swing {
        replay: u64,
        camera: String,
        result: Result<types::SwingReportFile, String>,
    },
    /// A camera's swing numbers graded against the bands.
    Grades {
        replay: u64,
        camera: String,
        result: Result<flighthook::BodyGrades, String>,
    },
    /// The answer to asking for a camera's clip to be tracked: `done` when
    /// its keypoints were there already.
    Analyzed {
        replay: u64,
        camera: String,
        done: bool,
    },
}

impl ReplayReply {
    /// The replay or camera card clip it was asked for.
    pub(crate) fn seq(&self) -> u64 {
        match self {
            Self::Sidecar { replay, .. }
            | Self::Frames { replay, .. }
            | Self::Pose { replay, .. }
            | Self::Swing { replay, .. }
            | Self::Grades { replay, .. }
            | Self::Analyzed { replay, .. } => *replay,
        }
    }
}

pub type PendingHandle = Arc<Mutex<Pending>>;

pub fn new_pending() -> PendingHandle {
    Arc::new(Mutex::new(Pending::default()))
}

// ---------------------------------------------------------------------------
// Helpers — platform-specific URL resolution
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
fn api_base() -> String {
    let location = web_sys::window().unwrap().location();
    let origin = location.origin().unwrap_or_default();
    if origin.is_empty() || origin == "null" {
        "http://127.0.0.1:5880".to_string()
    } else {
        origin
    }
}

#[cfg(target_arch = "wasm32")]
fn ws_url() -> String {
    let location = web_sys::window().unwrap().location();
    let protocol = location.protocol().unwrap_or_default();
    let host = location.host().unwrap_or_default();
    let ws_proto = if protocol == "https:" { "wss:" } else { "ws:" };
    if host.is_empty() {
        format!("ws://127.0.0.1:5880{}", flighthook::UI_WS_PATH)
    } else {
        format!("{ws_proto}//{host}{}", flighthook::UI_WS_PATH)
    }
}

#[cfg(not(target_arch = "wasm32"))]
static NATIVE_BASE_URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set the base URL for native builds (e.g. "http://127.0.0.1:5880").
/// Must be called before the app is created.
#[cfg(not(target_arch = "wasm32"))]
pub fn set_base_url(url: String) {
    NATIVE_BASE_URL.set(url).ok();
}

#[cfg(not(target_arch = "wasm32"))]
fn api_base() -> String {
    NATIVE_BASE_URL
        .get()
        .cloned()
        .unwrap_or_else(|| "http://127.0.0.1:5880".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn ws_url() -> String {
    let base = api_base();
    let ws_base = base
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    format!("{ws_base}{}", flighthook::UI_WS_PATH)
}

/// Download URL for the session export (`format`: `"jsonl"` or `"csv"`).
pub(crate) fn export_url(format: &str, units: types::UnitSystem) -> String {
    let units = match units {
        types::UnitSystem::Imperial => "imperial",
        types::UnitSystem::Metric => "metric",
    };
    format!(
        "{}/api/shots/export?format={format}&units={units}",
        api_base()
    )
}

/// URL of a shot's clip file from `camera`: `ext` is `avi` (the clip) or
/// `json` (its sidecar).
pub(crate) fn clip_url(shot_id: &str, camera: &str, ext: &str) -> String {
    format!("{}/api/shots/{shot_id}/video/{camera}.{ext}", api_base())
}

// ---------------------------------------------------------------------------
// REST fetches — results go into the Pending queue
// ---------------------------------------------------------------------------

pub fn fetch_status(ctx: &egui::Context, pending: &PendingHandle) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let url = format!("{}/api/status", api_base());
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        if let Ok(resp) = result
            && let Ok(status) = serde_json::from_slice::<StatusResponse>(&resp.bytes)
            && let Ok(mut p) = pending.lock()
        {
            p.status = Some(status);
            ctx.request_repaint();
        }
    });
}

/// Shots the UI loads: the server's whole history (`MAX_SHOTS`).
pub const SHOTS_LIMIT: usize = 1000;

/// Fetch the shot history, tagged with the history `generation` it is
/// requested under.
pub fn fetch_shots(ctx: &egui::Context, pending: &PendingHandle, generation: u64) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let url = format!("{}/api/shots?limit={SHOTS_LIMIT}", api_base());
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        if let Ok(resp) = result
            && let Ok(shots) = serde_json::from_slice::<Vec<ShotData>>(&resp.bytes)
            && let Ok(mut p) = pending.lock()
        {
            p.shots = Some(shots);
            p.shots_generation = generation;
            ctx.request_repaint();
        }
    });
}

/// Fetch the shot sets (`GET /api/blocks`), tagged with the history
/// `generation` they are requested under.
pub fn fetch_groups(ctx: &egui::Context, pending: &PendingHandle, generation: u64) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let url = format!("{}/api/blocks", api_base());
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        if let Ok(resp) = result
            && let Ok(groups) = serde_json::from_slice::<flighthook::BlocksResponse>(&resp.bytes)
            && let Ok(mut p) = pending.lock()
        {
            p.groups = Some(groups);
            p.groups_generation = generation;
            ctx.request_repaint();
        }
    });
}

pub fn fetch_settings(ctx: &egui::Context, pending: &PendingHandle) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let url = format!("{}/api/settings", api_base());
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        if let Ok(resp) = result
            && let Ok(settings) = serde_json::from_slice::<FlighthookConfig>(&resp.bytes)
            && let Ok(mut p) = pending.lock()
        {
            p.settings = Some(settings);
            ctx.request_repaint();
        }
    });
}

pub fn post_settings(
    ctx: &egui::Context,
    pending: &PendingHandle,
    req: &FlighthookConfig,
    scope: Option<&str>,
) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let url = if let Some(s) = scope {
        format!("{}/api/settings?scope={}", api_base(), s)
    } else {
        format!("{}/api/settings", api_base())
    };
    let body = serde_json::to_vec(req).unwrap_or_default();
    let mut http_req = ehttp::Request::post(&url, body);
    http_req
        .headers
        .insert("Content-Type".to_string(), "application/json".to_string());
    http_req.timeout = Some(CONFIG_REQUEST_TIMEOUT);
    ehttp::fetch(http_req, move |result| {
        if let Ok(mut p) = pending.lock() {
            p.settings_save = Some(save_reply(result));
        }
        ctx.request_repaint();
    });
}

/// Longest server error text a save failure carries.
const SAVE_ERROR_MAX_CHARS: usize = 120;

/// How long the dashboard waits for a settings save or a reconnect: longer
/// than the server waits for either, so the server's answer always arrives
/// first.
pub const CONFIG_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// A `POST /api/settings` reply as applied (`Ok`) or not (`Err` with a short
/// reason): a network error, a non-2xx status (its JSON `error`, else its
/// text, else the status) or a body that is not a `PostSettingsResponse`.
pub(crate) fn save_reply(
    result: Result<ehttp::Response, String>,
) -> Result<PostSettingsResponse, String> {
    let resp = result?;
    if !resp.ok {
        return Err(error_reason(&resp));
    }
    serde_json::from_slice::<PostSettingsResponse>(&resp.bytes)
        .map_err(|_| "unexpected reply from the server".to_owned())
}

/// A reply's JSON `error`, cut to [`SAVE_ERROR_MAX_CHARS`].
fn json_error(resp: &ehttp::Response) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(&resp.bytes)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
        .filter(|e| !e.is_empty())
        .map(|e| e.chars().take(SAVE_ERROR_MAX_CHARS).collect())
}

/// A short reason for a non-2xx reply: the status and its JSON `error`, else
/// its text, else the status text.
fn error_reason(resp: &ehttp::Response) -> String {
    let text = json_error(resp)
        .or_else(|| resp.text().map(|t| t.trim().to_owned()))
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(SAVE_ERROR_MAX_CHARS).collect::<String>());
    match text {
        Some(text) => format!("{} {}", resp.status, text),
        None => format!("{} {}", resp.status, resp.status_text),
    }
}

/// A `POST /api/auto-chip` or `POST /api/tournament` reply as applied
/// (`Ok`) or not (`Err` with a short reason: a network error or a non-2xx
/// status).
pub(crate) fn auto_chip_reply(result: Result<ehttp::Response, String>) -> Result<(), String> {
    let resp = result?;
    if resp.ok {
        Ok(())
    } else {
        Err(error_reason(&resp))
    }
}

/// The `POST /api/auto-chip` request for a chip of `yards`.
pub(crate) fn auto_chip_request(yards: u32) -> ehttp::Request {
    let url = format!("{}/api/auto-chip", api_base());
    let body = serde_json::to_vec(&AutoChipRequest { yards }).unwrap_or_default();
    ehttp::Request {
        headers: ehttp::Headers::new(&[("Accept", "*/*"), ("Content-Type", "application/json")]),
        ..ehttp::Request::post(&url, body)
    }
}

/// Send an Auto-chip of `yards` (`POST /api/auto-chip`); the reply lands in
/// `auto_chip`.
pub fn post_auto_chip(ctx: &egui::Context, pending: &PendingHandle, yards: u32) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    ehttp::fetch(auto_chip_request(yards), move |result| {
        if let Ok(mut p) = pending.lock() {
            p.auto_chip = Some(auto_chip_reply(result));
        }
        ctx.request_repaint();
    });
}

/// The `POST /api/actors/{id}/reconnect` request for actor `id`.
pub(crate) fn reconnect_request(id: &str) -> ehttp::Request {
    let url = format!("{}/api/actors/{id}/reconnect", api_base());
    ehttp::Request {
        headers: ehttp::Headers::new(&[("Accept", "*/*")]),
        timeout: Some(CONFIG_REQUEST_TIMEOUT),
        ..ehttp::Request::post(&url, Vec::new())
    }
}

/// How the server answered a reconnect it did not refuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconnected {
    /// `200`: the new instance has started.
    Started,
    /// `202`: it starts later, by itself.
    Later,
}

/// A reconnect reply as started (`200`) or starting later (`202`), or not
/// (`Err` with the short reason its card shows: the server's `error`, else
/// the status, or the network error).
pub(crate) fn reconnect_reply(
    result: Result<ehttp::Response, String>,
) -> Result<Reconnected, String> {
    let resp = result.map_err(|e| e.chars().take(SAVE_ERROR_MAX_CHARS).collect::<String>())?;
    if resp.status == 202 {
        return Ok(Reconnected::Later);
    }
    if resp.ok {
        return Ok(Reconnected::Started);
    }
    Err(json_error(&resp).unwrap_or_else(|| format!("{} {}", resp.status, resp.status_text)))
}

/// Reconnect actor `id` (`POST /api/actors/{id}/reconnect`); the reply
/// lands in `reconnects`.
pub fn post_reconnect(ctx: &egui::Context, pending: &PendingHandle, id: &str) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let id = id.to_owned();
    ehttp::fetch(reconnect_request(&id), move |result| {
        let reply = reconnect_reply(result);
        if let Ok(mut p) = pending.lock() {
            p.reconnects.push((id, reply));
        }
        ctx.request_repaint();
    });
}

/// The `POST /api/actors/{id}/forget-pairing` request for Pureplay Accessory card
/// `id`.
pub(crate) fn forget_pairing_request(id: &str) -> ehttp::Request {
    let url = format!("{}/api/actors/{id}/forget-pairing", api_base());
    ehttp::Request {
        headers: ehttp::Headers::new(&[("Accept", "*/*")]),
        timeout: Some(CONFIG_REQUEST_TIMEOUT),
        ..ehttp::Request::post(&url, Vec::new())
    }
}

/// Forget Pureplay Accessory card `id`'s pairing and reconnect it (`POST
/// /api/actors/{id}/forget-pairing`); the reply lands in `reconnects`, as a
/// reconnect's does.
pub fn post_forget_pairing(ctx: &egui::Context, pending: &PendingHandle, id: &str) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let id = id.to_owned();
    ehttp::fetch(forget_pairing_request(&id), move |result| {
        let reply = reconnect_reply(result);
        if let Ok(mut p) = pending.lock() {
            p.reconnects.push((id, reply));
        }
        ctx.request_repaint();
    });
}

/// Fetch the camera list (`GET /api/cameras`) into `cameras`.
pub fn fetch_cameras(ctx: &egui::Context, pending: &PendingHandle) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let url = format!("{}/api/cameras", api_base());
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        let reply = result.and_then(|resp| {
            if !resp.ok {
                return Err(error_reason(&resp));
            }
            serde_json::from_slice::<CameraListResponse>(&resp.bytes)
                .map_err(|_| "unexpected reply from the server".to_owned())
        });
        if let Ok(mut p) = pending.lock() {
            p.cameras = Some(reply);
        }
        ctx.request_repaint();
    });
}

/// Fetch camera `actor`'s latest frame (`GET /api/cameras/{actor}/still`)
/// into `stills`.
pub fn fetch_still(ctx: &egui::Context, pending: &PendingHandle, actor: &str) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let actor = actor.to_owned();
    let url = format!("{}/api/cameras/{actor}/still", api_base());
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        let reply = result.and_then(|resp| {
            if resp.ok {
                Ok(resp.bytes)
            } else if resp.status == 404 {
                Err("no frame yet".to_owned())
            } else {
                Err(error_reason(&resp))
            }
        });
        if let Ok(mut p) = pending.lock() {
            p.stills.push((actor, reply));
        }
        ctx.request_repaint();
    });
}

/// The `GET` of camera `actor`'s live frame newer than `after`.
pub(crate) fn live_request(actor: &str, after: u64) -> ehttp::Request {
    ehttp::Request::get(format!(
        "{}/api/cameras/{actor}/live?after={after}",
        api_base()
    ))
}

/// A reply to [`live_request`].
pub(crate) fn live_reply(result: Result<ehttp::Response, String>) -> LiveReply {
    let resp = match result {
        Ok(resp) => resp,
        Err(e) => return LiveReply::Failed(e),
    };
    match resp.status {
        200 => match resp.headers.get("x-frame-seq").map(str::parse::<u64>) {
            Some(Ok(seq)) => LiveReply::Frame {
                seq,
                pose: resp
                    .headers
                    .get("x-pose")
                    .and_then(|p| serde_json::from_str(p).ok()),
                state: PoseState::of(&resp),
                jpeg: resp.bytes,
            },
            _ => LiveReply::Failed("unexpected reply from the server".to_owned()),
        },
        204 => LiveReply::Nothing(PoseState::of(&resp)),
        _ => LiveReply::Failed(error_reason(&resp)),
    }
}

/// Save `req`'s body tracking settings at once (nothing else in `req` is
/// written): the reply goes to [`Pending::tuning`].
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn post_tuning(ctx: &egui::Context, pending: &PendingHandle, req: &FlighthookConfig) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let url = format!("{}/api/settings?scope=body_tracking", api_base());
    let body = serde_json::to_vec(req).unwrap_or_default();
    let mut http_req = ehttp::Request::post(&url, body);
    http_req
        .headers
        .insert("Content-Type".to_string(), "application/json".to_string());
    http_req.timeout = Some(CONFIG_REQUEST_TIMEOUT);
    ehttp::fetch(http_req, move |result| {
        if let Ok(mut p) = pending.lock() {
            p.tuning = Some(save_reply(result).map(|_| ()));
        }
        ctx.request_repaint();
    });
}

/// Fetch the keypoints of `shot_id`'s clip from `camera`
/// (`<camera>.pose.json`), for replay `replay`.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn fetch_pose(
    ctx: &egui::Context,
    pending: &PendingHandle,
    replay: u64,
    shot_id: &str,
    camera: &str,
) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let camera = camera.to_owned();
    let url = clip_url(shot_id, &camera, "pose.json");
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        let result = pose_reply(result);
        if let Ok(mut p) = pending.lock() {
            p.replay.push(ReplayReply::Pose {
                replay,
                camera,
                result,
            });
        }
        ctx.request_repaint();
    });
}

/// Fetch the grades of `shot_id`'s swing numbers from `camera`
/// (`GET /api/shots/{shot_id}/grades/{camera}`), for replay `replay`.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn fetch_grades(
    ctx: &egui::Context,
    pending: &PendingHandle,
    replay: u64,
    shot_id: &str,
    camera: &str,
) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let camera = camera.to_owned();
    let url = grades_url(shot_id, &camera);
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        let result = json_reply(result);
        if let Ok(mut p) = pending.lock() {
            p.replay.push(ReplayReply::Grades {
                replay,
                camera,
                result,
            });
        }
        ctx.request_repaint();
    });
}

/// The URL of `shot_id`'s grades from `camera`.
pub(crate) fn grades_url(shot_id: &str, camera: &str) -> String {
    format!("{}/api/shots/{shot_id}/grades/{camera}", api_base())
}

/// Fetch the swing analysis of `shot_id`'s clip from `camera`
/// (`<camera>.swing.json`), for replay `replay`.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn fetch_swing(
    ctx: &egui::Context,
    pending: &PendingHandle,
    replay: u64,
    shot_id: &str,
    camera: &str,
) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let camera = camera.to_owned();
    let url = clip_url(shot_id, &camera, "swing.json");
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        let result = json_reply(result);
        if let Ok(mut p) = pending.lock() {
            p.replay.push(ReplayReply::Swing {
                replay,
                camera,
                result,
            });
        }
        ctx.request_repaint();
    });
}

/// Ask the server to track `shot_id`'s clip from `camera` now
/// (`POST .../video/{camera}/analyze`), for replay `replay`. Its keypoints
/// follow with `shot_pose`; the reply only says whether they were there
/// already.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn post_analyze(
    ctx: &egui::Context,
    pending: &PendingHandle,
    replay: u64,
    shot_id: &str,
    camera: &str,
) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let camera = camera.to_owned();
    let url = analyze_url(shot_id, &camera);
    ehttp::fetch(ehttp::Request::post(url, Vec::new()), move |result| {
        let done = analyze_done(&result);
        if let Ok(mut p) = pending.lock() {
            p.replay.push(ReplayReply::Analyzed {
                replay,
                camera,
                done,
            });
        }
        ctx.request_repaint();
    });
}

/// Ask the server to delete `shot_id`'s body tracking and swing analysis
/// of the clip from `camera` and track it again
/// (`POST .../video/{camera}/analyze?force=1`). Its keypoints follow with
/// `shot_pose`; the reply is not needed.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn post_retrack(ctx: &egui::Context, shot_id: &str, camera: &str) {
    let ctx = ctx.clone();
    ehttp::fetch(
        ehttp::Request::post(retrack_url(shot_id, camera), Vec::new()),
        move |_| ctx.request_repaint(),
    );
}

/// The URL that asks for `shot_id`'s clip from `camera` to be tracked again.
pub(crate) fn retrack_url(shot_id: &str, camera: &str) -> String {
    format!("{}?force=1", analyze_url(shot_id, camera))
}

/// Send `call` about body tracking's model files; its reply goes into
/// [`Pending::body_models`].
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn body_models(ctx: &egui::Context, pending: &PendingHandle, call: ModelsCall) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let (method, url) = call.request();
    let request = ehttp::Request {
        method: method.to_owned(),
        ..ehttp::Request::get(url)
    };
    ehttp::fetch(request, move |result| {
        let reply = json_reply(result);
        if let Ok(mut p) = pending.lock() {
            p.body_models.push((call, reply));
        }
        ctx.request_repaint();
    });
}

/// The URL that asks for `shot_id`'s clip from `camera` to be tracked.
pub(crate) fn analyze_url(shot_id: &str, camera: &str) -> String {
    format!("{}/api/shots/{shot_id}/video/{camera}/analyze", api_base())
}

/// Whether an analyze reply says the keypoints are there already: `200`.
/// `202` (queued), an error or no reply at all is not.
pub(crate) fn analyze_done(result: &Result<ehttp::Response, String>) -> bool {
    result.as_ref().is_ok_and(|r| r.status == 200)
}

/// A reply to a JSON file request, parsed, or why not.
fn json_reply<T: serde::de::DeserializeOwned>(
    result: Result<ehttp::Response, String>,
) -> Result<T, String> {
    let resp = result?;
    if !resp.ok {
        return Err(error_reason(&resp));
    }
    serde_json::from_slice(&resp.bytes).map_err(|e| e.to_string())
}

/// A reply to a `<camera>.pose.json` request: the keypoints, or why not.
pub(crate) fn pose_reply(
    result: Result<ehttp::Response, String>,
) -> Result<types::PoseTrackFile, String> {
    json_reply(result)
}

/// Fetch camera `actor`'s live frame newer than `after`, for live view
/// `view`. Only a frame asks for a repaint: the next request goes out on the
/// next screen update.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn fetch_live(
    ctx: &egui::Context,
    pending: &PendingHandle,
    view: u64,
    actor: &str,
    after: u64,
) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    ehttp::fetch(live_request(actor, after), move |result| {
        let reply = live_reply(result);
        let frame = matches!(reply, LiveReply::Frame { .. });
        if let Ok(mut p) = pending.lock() {
            p.live.push((view, reply));
        }
        if frame {
            ctx.request_repaint();
        }
    });
}

/// Fetch the sidecar of `shot_id`'s clip from `camera`, for replay `replay`.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn fetch_sidecar(
    ctx: &egui::Context,
    pending: &PendingHandle,
    replay: u64,
    shot_id: &str,
    camera: &str,
) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let camera = camera.to_owned();
    let url = clip_url(shot_id, &camera, "json");
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        let result = result.and_then(|resp| {
            if !resp.ok {
                return Err(error_reason(&resp));
            }
            let text = resp.text().unwrap_or_default();
            stance::sidecar::Sidecar::from_json(text).map_err(|e| e.to_string())
        });
        if let Ok(mut p) = pending.lock() {
            p.replay.push(ReplayReply::Sidecar {
                replay,
                camera,
                result,
            });
        }
        ctx.request_repaint();
    });
}

/// The `GET` of bytes `start..end` of `url`.
pub(crate) fn range_request(url: &str, start: u64, end: u64) -> ehttp::Request {
    let range = format!("bytes={start}-{}", end.saturating_sub(1));
    ehttp::Request {
        headers: ehttp::Headers::new(&[("Accept", "*/*"), ("Range", range.as_str())]),
        ..ehttp::Request::get(url)
    }
}

/// The bytes `start..end` from a reply to [`range_request`]: the body of a
/// `206`, or that part of a whole file sent with `200`.
pub(crate) fn range_reply(
    result: Result<ehttp::Response, String>,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, String> {
    let resp = result?;
    match resp.status {
        206 => Ok(resp.bytes),
        200 => {
            let a = usize::try_from(start).map_err(|e| e.to_string())?;
            let b = usize::try_from(end).map_err(|e| e.to_string())?;
            resp.bytes
                .get(a..b.min(resp.bytes.len()))
                .map(<[u8]>::to_vec)
                .ok_or_else(|| "the clip is shorter than its index".to_owned())
        }
        _ => Err(error_reason(&resp)),
    }
}

/// Fetch the frames of `window` of `shot_id`'s clip from `camera` with one
/// range request, for replay `replay`.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn fetch_frames(
    ctx: &egui::Context,
    pending: &PendingHandle,
    replay: u64,
    shot_id: &str,
    camera: &str,
    window: Window,
    bytes: (u64, u64),
) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let camera = camera.to_owned();
    let (start, end) = bytes;
    let req = range_request(&clip_url(shot_id, &camera, "avi"), start, end);
    ehttp::fetch(req, move |result| {
        let result = range_reply(result, start, end);
        if let Ok(mut p) = pending.lock() {
            p.replay.push(ReplayReply::Frames {
                replay,
                camera,
                window,
                result,
            });
        }
        ctx.request_repaint();
    });
}

/// Select the mode (`POST /api/select`).
pub fn post_mode(ctx: &egui::Context, mode: &str) {
    post_json(ctx, "/api/select", select_body("mode", mode));
}

/// Select the club in play (`POST /api/select`).
pub fn post_club(ctx: &egui::Context, club: Club) {
    post_json(ctx, "/api/select", select_body("club", &club.to_string()));
}

/// The `POST /api/select` body selecting `value` for `field`.
pub(crate) fn select_body(field: &str, value: &str) -> String {
    serde_json::json!({ field: value }).to_string()
}

/// Have ExPutt `actor` locate its mat again (`POST /api/find-mat`).
pub fn post_find_mat(ctx: &egui::Context, actor: &str) {
    let body = serde_json::to_string(&FindMatRequest {
        actor: actor.to_string(),
    })
    .unwrap_or_default();
    post_json(ctx, "/api/find-mat", body);
}

/// Start a new set, or with `new_session` a new session (`POST
/// /api/blocks`); sets `groups_posted` once answered.
pub fn post_new_block(
    ctx: &egui::Context,
    pending: &PendingHandle,
    golfer: Option<flighthook::GolferId>,
    new_session: bool,
) {
    let ctx = ctx.clone();
    let pending = pending.clone();
    let url = format!("{}/api/blocks", api_base());
    let body = serde_json::to_vec(&flighthook::NewBlockRequest {
        golfer,
        new_session,
    })
    .unwrap_or_default();
    let mut req = ehttp::Request::post(&url, body);
    req.headers
        .insert("Content-Type".to_string(), "application/json".to_string());
    ehttp::fetch(req, move |result| {
        if result.is_ok_and(|r| r.ok)
            && let Ok(mut p) = pending.lock()
        {
            p.groups_posted = true;
        }
        ctx.request_repaint();
    });
}

/// Delete the shot history and every set, and start a new session
/// (`POST /api/shots/clear`).
pub fn post_clear_history(ctx: &egui::Context) {
    post_json(ctx, "/api/shots/clear", String::new());
}

/// Set the shot grouping mode (`POST /api/shots/grouping`).
pub fn post_grouping(ctx: &egui::Context, mode: ShotGrouping) {
    let body = serde_json::json!({ "mode": mode }).to_string();
    post_json(ctx, "/api/shots/grouping", body);
}

/// The `POST /api/tournament` request switching tournament mode to `on`.
/// The server answers once the config command is applied, so it gets the
/// config requests' timeout.
pub(crate) fn tournament_request(on: bool) -> ehttp::Request {
    let url = format!("{}/api/tournament", api_base());
    let body = serde_json::json!({ "on": on }).to_string().into_bytes();
    ehttp::Request {
        headers: ehttp::Headers::new(&[("Accept", "*/*"), ("Content-Type", "application/json")]),
        timeout: Some(CONFIG_REQUEST_TIMEOUT),
        ..ehttp::Request::post(&url, body)
    }
}

/// Switch tournament mode on or off (`POST /api/tournament`); the reply
/// lands in `tournament` (a failure shows in the error banner, like
/// Auto-chip's). The server's `round_state` event redraws the header.
pub fn post_tournament(ctx: &egui::Context, pending: &PendingHandle, on: bool) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    ehttp::fetch(tournament_request(on), move |result| {
        if let Ok(mut p) = pending.lock() {
            p.tournament = Some(auto_chip_reply(result));
        }
        ctx.request_repaint();
    });
}

/// Fire-and-forget POST of a JSON body (`body` may be empty) to `path`.
fn post_json(ctx: &egui::Context, path: &str, body: String) {
    let ctx = ctx.clone();
    let url = format!("{}{path}", api_base());
    let mut req = ehttp::Request::post(&url, body.into_bytes());
    req.headers
        .insert("Content-Type".to_string(), "application/json".to_string());
    ehttp::fetch(req, move |_| {
        ctx.request_repaint();
    });
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

pub fn connect_ws() -> Option<(ewebsock::WsSender, ewebsock::WsReceiver)> {
    let url = ws_url();
    match ewebsock::connect(&url, ewebsock::Options::default()) {
        Ok(pair) => Some(pair),
        Err(e) => {
            log::error!("WebSocket connect failed: {e}");
            None
        }
    }
}

/// Send `command` on the UI socket.
pub fn send_command(tx: &mut ewebsock::WsSender, command: &flighthook::UiCommand) {
    if let Ok(text) = serde_json::to_string(command) {
        tx.send(ewebsock::WsMessage::Text(text));
    }
}

/// A `ref` for a command this UI sends.
fn next_ref() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .to_string()
}

/// A `golfer.select` with `change` applied, as the UI socket takes it (the
/// server stamps `from`).
#[must_use]
pub fn select_command(change: impl FnOnce(&mut flighthook::GolferSelect)) -> flighthook::UiCommand {
    let mut select = flighthook::GolferSelect::new(next_ref());
    change(&mut select);
    flighthook::UiCommand::Frp(Box::new(flighthook::BusMessage::new("ui", select)))
}

/// A `practice.new_block` for `golfer` (`None` = the golfer up; a new
/// session with `new_session`).
#[must_use]
pub fn new_block_command(
    golfer: Option<flighthook::GolferId>,
    new_session: bool,
) -> flighthook::UiCommand {
    let block = flighthook::PracticeNewBlock {
        r#ref: next_ref(),
        golfer: golfer.map(Some),
        new_session,
    };
    flighthook::UiCommand::Frp(Box::new(flighthook::BusMessage::new("ui", block)))
}

/// Result from polling the WebSocket.
pub enum WsPollEvent {
    /// Browser WebSocket opened (WASM: onopen fired).
    Opened,
    /// A message of the UI socket.
    Message(Box<flighthook::UiMessage>),
    /// WebSocket error.
    Error(String),
    /// WebSocket connection lost.
    Disconnected,
}

pub fn poll_ws(rx: &mut ewebsock::WsReceiver) -> (Vec<WsPollEvent>, u64) {
    let mut events = Vec::new();
    let mut raw_count: u64 = 0;
    while let Some(event) = rx.try_recv() {
        raw_count += 1;
        match event {
            ewebsock::WsEvent::Opened => {
                log::info!("WebSocket opened");
                events.push(WsPollEvent::Opened);
            }
            ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(text)) => {
                match serde_json::from_str::<flighthook::UiMessage>(&text) {
                    Ok(msg) => events.push(WsPollEvent::Message(Box::new(msg))),
                    Err(e) => log::warn!("UI socket: unreadable message: {e}"),
                }
            }
            ewebsock::WsEvent::Error(e) => {
                log::error!("WebSocket error: {e}");
                events.push(WsPollEvent::Error(e));
            }
            ewebsock::WsEvent::Closed => {
                log::warn!("WebSocket closed");
                events.push(WsPollEvent::Disconnected);
                break;
            }
            _ => {}
        }
    }
    (events, raw_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Handedness;

    /// The UI socket's commands: a select and a new block go as FRP
    /// messages with a fresh `ref`; the server stamps `from`.
    #[test]
    fn commands_go_as_frp_messages_with_a_fresh_ref() {
        let a = serde_json::to_value(select_command(|s| s.club = Some(Some(Club::Iron7))))
            .expect("json");
        assert_eq!(a["frp"]["kind"], "golfer.select");
        assert_eq!(a["frp"]["club"], "7I");
        let b = serde_json::to_value(new_block_command(None, true)).expect("json");
        assert_eq!(b["frp"]["kind"], "practice.new_block");
        assert_eq!(b["frp"]["new_session"], true);
        assert_ne!(a["frp"]["ref"], b["frp"]["ref"]);
    }

    /// The hand reaches the UI the way the server sends it: in flighthook's
    /// `golfer.turn` on the socket and in the status reply; and the UI posts
    /// it in the body the server takes.
    #[test]
    fn the_hand_round_trips_on_the_wire() {
        let text = r#"{"frp":{"kind":"golfer.turn","from":"me","golfer":null,"club":null,
            "handedness":"lh","mode":"full"}}"#;
        let msg: flighthook::UiMessage = serde_json::from_str(text).expect("socket message");
        let flighthook::UiMessage::Frp(frp) = msg else {
            panic!("not frp");
        };
        let flighthook::FrpBody::GolferTurn(turn) = frp.body else {
            panic!("not a turn");
        };
        assert_eq!(turn.handedness, Some(Handedness::Left));

        let status: StatusResponse = serde_json::from_str(
            r#"{"cards":{},"turn":{"golfer":null,"club":"7I","handedness":"rh","mode":"full"}}"#,
        )
        .expect("status");
        assert_eq!(
            status.turn.and_then(|t| t.handedness),
            Some(Handedness::Right)
        );

        assert_eq!(
            select_body("club", &Club::Iron7.to_string()),
            r#"{"club":"7I"}"#
        );
    }

    fn response(status: u16, status_text: &str, body: &str) -> ehttp::Response {
        ehttp::Response {
            url: "http://127.0.0.1:5880/api/settings".into(),
            ok: (200..300).contains(&status),
            status,
            status_text: status_text.into(),
            headers: ehttp::Headers::default(),
            bytes: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn an_analyze_reply_is_done_only_when_the_keypoints_were_there() {
        assert!(analyze_done(&Ok(response(
            200,
            "OK",
            r#"{"state":"done"}"#
        ))));
        assert!(!analyze_done(&Ok(response(
            202,
            "Accepted",
            r#"{"state":"queued"}"#
        ))));
        assert!(!analyze_done(&Ok(response(
            409,
            "Conflict",
            r#"{"error":"body tracking is off"}"#
        ))));
        assert!(!analyze_done(&Err("offline".into())));
        assert!(analyze_url("s1", "camera.0").ends_with("/api/shots/s1/video/camera.0/analyze"));
        assert!(
            retrack_url("s1", "camera.0").ends_with("/api/shots/s1/video/camera.0/analyze?force=1")
        );
    }

    #[test]
    fn a_reconnect_reply_is_started_later_or_why_not() {
        assert_eq!(
            reconnect_reply(Ok(response(200, "OK", r#"{"restarted":["mevo.0"]}"#))),
            Ok(Reconnected::Started)
        );
        assert_eq!(
            reconnect_reply(Ok(response(202, "Accepted", r#"{"restarted":[]}"#))),
            Ok(Reconnected::Later)
        );
        assert_eq!(
            reconnect_reply(Ok(response(
                409,
                "Conflict",
                r#"{"error":"a reconnect of 'mevo.0' is already in progress"}"#
            ))),
            Err("a reconnect of 'mevo.0' is already in progress".into())
        );
        assert_eq!(
            reconnect_reply(Ok(response(504, "Gateway Timeout", ""))),
            Err("504 Gateway Timeout".into())
        );
        assert_eq!(
            reconnect_reply(Err("connection refused".into())),
            Err("connection refused".into())
        );
    }

    #[test]
    fn a_save_or_a_reconnect_waits_as_long_as_the_server_may() {
        let req = reconnect_request("mevo.0");
        assert_eq!(req.timeout, Some(CONFIG_REQUEST_TIMEOUT));
        assert!(CONFIG_REQUEST_TIMEOUT > ehttp::Request::DEFAULT_TIMEOUT);
    }

    #[test]
    fn a_reconnect_posts_to_its_actor() {
        let req = reconnect_request("openconnect_client.1");
        assert_eq!(req.method, "POST");
        assert!(
            req.url
                .ends_with("/api/actors/openconnect_client.1/reconnect"),
            "{}",
            req.url
        );
        assert!(req.body.is_empty());
    }

    #[test]
    fn a_save_reply_is_applied_only_on_a_2xx_settings_response() {
        let ok = save_reply(Ok(response(200, "OK", r#"{"restarted":["webserver.0"]}"#)))
            .expect("applied");
        assert_eq!(ok.restarted, ["webserver.0"]);
        assert!(save_reply(Ok(response(200, "OK", "{}"))).is_ok());

        let failures = [
            (
                save_reply(Err("Failed to fetch".into())),
                "Failed to fetch".to_owned(),
            ),
            (
                save_reply(Ok(response(
                    504,
                    "Gateway Timeout",
                    r#"{"error":"the settings were not applied in time"}"#,
                ))),
                "504 the settings were not applied in time".to_owned(),
            ),
            (
                save_reply(Ok(response(
                    422,
                    "Unprocessable Entity",
                    "missing field `name`\n",
                ))),
                "422 missing field `name`".to_owned(),
            ),
            (
                save_reply(Ok(response(502, "Bad Gateway", ""))),
                "502 Bad Gateway".to_owned(),
            ),
            (
                save_reply(Ok(response(200, "OK", "<html>proxy</html>"))),
                "unexpected reply from the server".to_owned(),
            ),
        ];
        for (reply, want) in failures {
            assert_eq!(reply.err(), Some(want));
        }
        let long = "x".repeat(500);
        let reply = save_reply(Ok(response(500, "Internal Server Error", &long)));
        assert_eq!(reply.err().map(|e| e.len()), Some("500 ".len() + 120));
    }

    #[test]
    fn a_live_reply_is_a_frame_nothing_or_why_not() {
        let req = live_request("camera.1", 41);
        assert!(
            req.url.ends_with("/api/cameras/camera.1/live?after=41"),
            "{}",
            req.url
        );
        assert_eq!(req.method, "GET");
        let mut frame = response(200, "OK", "jpeg");
        frame.headers = ehttp::Headers::new(&[("X-Frame-Seq", "42"), ("X-Frame-Time-Ns", "7")]);
        assert_eq!(
            live_reply(Ok(frame)),
            LiveReply::Frame {
                seq: 42,
                jpeg: b"jpeg".to_vec(),
                pose: None,
                state: PoseState::default(),
            }
        );
        assert_eq!(
            live_reply(Ok(response(200, "OK", "jpeg"))),
            LiveReply::Failed("unexpected reply from the server".into()),
            "a frame without its number"
        );
        assert_eq!(
            live_reply(Ok(response(204, "No Content", ""))),
            LiveReply::Nothing(PoseState::default())
        );
        // Body tracking: the pose of the frame, and the models' state.
        let mut frame = response(200, "OK", "jpeg");
        frame.headers = ehttp::Headers::new(&[
            ("X-Frame-Seq", "43"),
            (
                "X-Pose",
                r#"{"skeleton":"halpe26","keypoints":[[1.5,2.0,0.9]],"ms":18.5}"#,
            ),
            ("X-Pose-State", "ready"),
        ]);
        let LiveReply::Frame { pose, state, .. } = live_reply(Ok(frame)) else {
            panic!("not a frame");
        };
        let pose = pose.expect("pose");
        assert_eq!(pose.keypoints, [[1.5, 2.0, 0.9]]);
        assert!((pose.ms - 18.5).abs() < f32::EPSILON);
        assert_eq!(state.state, "ready");
        let mut nothing = response(204, "No Content", "");
        nothing.headers = ehttp::Headers::new(&[
            ("X-Pose-State", "failed"),
            ("X-Pose-Error", "rtmpose-m-halpe26.onnx is missing"),
        ]);
        assert_eq!(
            live_reply(Ok(nothing)),
            LiveReply::Nothing(PoseState {
                state: "failed".into(),
                error: Some("rtmpose-m-halpe26.onnx is missing".into()),
            })
        );
        assert_eq!(
            live_reply(Ok(response(404, "Not Found", "no camera 'camera.1'"))),
            LiveReply::Failed("404 no camera 'camera.1'".into())
        );
        assert_eq!(
            live_reply(Err("connection refused".into())),
            LiveReply::Failed("connection refused".into())
        );
    }

    #[test]
    fn a_frame_window_asks_for_its_byte_range() {
        let req = range_request("http://h/api/shots/s1/video/camera.0.avi", 232, 41_262);
        assert_eq!(req.method, "GET");
        assert_eq!(req.headers.get("Range"), Some("bytes=232-41261"));
        assert!(req.url.ends_with("/video/camera.0.avi"));
        assert!(clip_url("s1", "camera.0", "json").ends_with("/api/shots/s1/video/camera.0.json"));

        let mut partial = response(206, "Partial Content", "abcd");
        partial.bytes = vec![1, 2, 3];
        assert_eq!(range_reply(Ok(partial), 10, 13), Ok(vec![1, 2, 3]));
        // A server that ignores the range sends the whole file.
        let mut whole = response(200, "OK", "");
        whole.bytes = (0..20).collect();
        assert_eq!(range_reply(Ok(whole.clone()), 10, 13), Ok(vec![10, 11, 12]));
        assert!(range_reply(Ok(whole), 30, 33).is_err());
        assert_eq!(
            range_reply(Ok(response(404, "Not Found", "no such clip")), 0, 1),
            Err("404 no such clip".to_owned())
        );
    }

    /// The camera additions reach the UI the way the server sends them.
    #[test]
    fn camera_replies_and_events_round_trip_on_the_wire() {
        let status: StatusResponse = serde_json::from_str(
            r#"{"cards":{},"cameras_supported":true,"clip_dir":"/data/clips"}"#,
        )
        .expect("status");
        assert!(status.cameras_supported);
        assert_eq!(status.clip_dir.as_deref(), Some("/data/clips"));
        let status: StatusResponse =
            serde_json::from_str(r#"{"actors":{},"mode":null}"#).expect("status");
        assert!(!status.cameras_supported, "absent means unsupported");

        let list: CameraListResponse = serde_json::from_str(
            r#"{"supported":true,"cameras":[
                {"id":"/dev/v4l/by-id/usb-cam","name":"HD Pro Webcam C920","usb":"046d:085e",
                 "modes":["1920x1080@30","1280x720@30"],"in_use_by":"camera.0"},
                {"id":"test-pattern","name":"Test pattern","modes":["640x360@30"]}]}"#,
        )
        .expect("list");
        assert_eq!(list.cameras.len(), 2);
        assert_eq!(list.cameras[0].in_use_by.as_deref(), Some("camera.0"));
        assert_eq!(list.cameras[1].usb, None);
    }

    #[test]
    fn an_auto_chip_posts_its_yards_as_json() {
        for yards in [1, 3, 5] {
            let req = auto_chip_request(yards);
            assert_eq!(req.method, "POST");
            assert!(req.url.ends_with("/api/auto-chip"), "{}", req.url);
            assert_eq!(
                req.headers.get("Content-Type"),
                Some("application/json"),
                "{:?}",
                req.headers
            );
            let body: AutoChipRequest = serde_json::from_slice(&req.body).expect("json");
            assert_eq!(body, AutoChipRequest { yards });
        }
    }

    #[test]
    fn an_auto_chip_reply_fails_on_a_network_error_or_a_non_2xx_status() {
        assert_eq!(
            auto_chip_reply(Ok(response(200, "OK", r#"{"shot_number":1}"#))),
            Ok(())
        );
        assert_eq!(
            auto_chip_reply(Err("Failed to fetch".into())),
            Err("Failed to fetch".to_owned())
        );
        assert_eq!(
            auto_chip_reply(Ok(response(
                409,
                "Conflict",
                r#"{"error":"simulation environment invalid: temperature 99 C is outside -30 to 50 C"}"#,
            ))),
            Err(
                "409 simulation environment invalid: temperature 99 C is outside -30 to 50 C"
                    .to_owned()
            )
        );
    }
}
