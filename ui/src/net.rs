//! Networking — ehttp REST + ewebsock WebSocket.
//!
//! Async results are placed in a shared `Pending` queue (Arc<Mutex>)
//! that the app drains each frame. This avoids touching egui's internal
//! data store from async callbacks.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::replay::Window;
use crate::types::{
    self, AutoChipRequest, CameraListResponse, Club, FindMatRequest, FlighthookConfig,
    FlighthookMessage, GameStateSnapshot, HandRequest, Handedness, PostSettingsResponse, ShotData,
    ShotGrouping, ShotGroupsResponse, StatusResponse,
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
    pub groups: Option<ShotGroupsResponse>,
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
    /// The reply to `GET /api/cameras`: the camera list, or why not.
    pub cameras: Option<Result<CameraListResponse, String>>,
    /// Still frames by camera actor: the JPEG, or why there is none.
    pub stills: Vec<(String, Result<Vec<u8>, String>)>,
    /// Answered reconnects, by actor: how, or why not.
    pub reconnects: Vec<(String, Result<Reconnected, String>)>,
    /// Replies for the replay view.
    pub(crate) replay: Vec<ReplayReply>,
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
}

impl ReplayReply {
    /// The replay or camera card clip it was asked for.
    pub(crate) fn seq(&self) -> u64 {
        match self {
            Self::Sidecar { replay, .. } | Self::Frames { replay, .. } => *replay,
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
        "ws://127.0.0.1:5880/frp".to_string()
    } else {
        format!("{ws_proto}//{host}/frp")
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
    format!("{ws_base}/frp")
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

/// Fetch the shot sets, tagged with the history `generation` they are
/// requested under.
pub fn fetch_groups(ctx: &egui::Context, pending: &PendingHandle, generation: u64) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let url = format!("{}/api/shots/groups", api_base());
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        if let Ok(resp) = result
            && let Ok(groups) = serde_json::from_slice::<ShotGroupsResponse>(&resp.bytes)
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

/// A `POST /api/auto-chip` reply as sent (`Ok`) or not (`Err` with a short
/// reason: a network error or a non-2xx status).
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

pub fn post_mode(ctx: &egui::Context, mode: &str) {
    post_json(ctx, "/api/mode", format!(r#"{{"mode":"{mode}"}}"#));
}

/// Select the club in play (`POST /api/club`).
pub fn post_club(ctx: &egui::Context, club: Club) {
    post_json(ctx, "/api/club", format!(r#"{{"club":"{club}"}}"#));
}

/// Set the player's hand (`POST /api/hand`).
pub fn post_hand(ctx: &egui::Context, handed: Handedness) {
    post_json(ctx, "/api/hand", hand_body(handed));
}

/// The `POST /api/hand` body for `handed`.
pub(crate) fn hand_body(handed: Handedness) -> String {
    serde_json::to_string(&HandRequest {
        handed: handed.to_string(),
    })
    .unwrap_or_default()
}

/// Have ExPutt `actor` locate its mat again (`POST /api/find-mat`).
pub fn post_find_mat(ctx: &egui::Context, actor: &str) {
    let body = serde_json::to_string(&FindMatRequest {
        actor: actor.to_string(),
    })
    .unwrap_or_default();
    post_json(ctx, "/api/find-mat", body);
}

/// Start a new shot group (`POST /api/shots/groups`); sets
/// `groups_posted` once answered.
pub fn post_new_group(ctx: &egui::Context, pending: &PendingHandle) {
    post_groups_change(ctx, pending, "/api/shots/groups");
}

/// Close every open shot group (`POST /api/shots/session`); sets
/// `groups_posted` once answered.
pub fn post_new_session(ctx: &egui::Context, pending: &PendingHandle) {
    post_groups_change(ctx, pending, "/api/shots/session");
}

/// Bodiless POST to `path` that sets `groups_posted` on a success reply.
fn post_groups_change(ctx: &egui::Context, pending: &PendingHandle, path: &str) {
    let ctx = ctx.clone();
    let pending = pending.clone();
    let url = format!("{}{path}", api_base());
    ehttp::fetch(ehttp::Request::post(&url, Vec::new()), move |result| {
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
        Ok((tx, rx)) => {
            // Don't send the start handshake yet — in WASM the WebSocket is
            // still CONNECTING. Send it after we receive WsEvent::Opened.
            Some((tx, rx))
        }
        Err(e) => {
            log::error!("WebSocket connect failed: {e}");
            None
        }
    }
}

/// Send the init handshake after the WebSocket is confirmed open.
pub fn send_ws_start(tx: &mut ewebsock::WsSender) {
    tx.send(ewebsock::WsMessage::Text(
        r#"{"kind":"start","version":["0.1.0"],"name":"Flighthook Dashboard"}"#.into(),
    ));
}

/// Result from polling the WebSocket.
pub enum WsPollEvent {
    /// Browser WebSocket opened (WASM: onopen fired).
    Opened,
    /// Init handshake completed — server assigned a actor_id and sent the
    /// global state.
    Init {
        actor_id: String,
        global_state: Option<GameStateSnapshot>,
    },
    /// A bus event from the server.
    Message(FlighthookMessage),
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
                // Try init response first (has "kind" field)
                if let Some(init) = parse_init_response(&text) {
                    events.push(init);
                } else if let Some(msg) = types::parse_ws_message(&text) {
                    events.push(WsPollEvent::Message(msg));
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

/// Parse a WS init response:
/// `{ "kind": "init", "version": "0.1.0", "actor_id": "...", "global_state": {...} }`
fn parse_init_response(text: &str) -> Option<WsPollEvent> {
    #[derive(serde::Deserialize)]
    struct InitMsg {
        kind: String,
        actor_id: Option<String>,
        global_state: Option<GameStateSnapshot>,
    }
    let msg: InitMsg = serde_json::from_str(text).ok()?;
    if msg.kind == "init" {
        Some(WsPollEvent::Init {
            actor_id: msg.actor_id.unwrap_or_default(),
            global_state: msg.global_state,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_carries_the_global_club() {
        let text = r#"{"kind":"init","version":"0.1.0","actor_id":"ws.1",
            "global_state":{"player_info":null,"club_info":{"club":"7I"},"mode":"full","handed":null}}"#;
        let Some(WsPollEvent::Init {
            actor_id,
            global_state,
        }) = parse_init_response(text)
        else {
            panic!("not an init");
        };
        assert_eq!(actor_id, "ws.1");
        assert_eq!(global_state.unwrap().club_info.unwrap().club, Club::Iron7);
        // A bus message is not an init.
        assert!(
            parse_init_response(r#"{"actor":"x","event":{"kind":"new_shot_group"}}"#).is_none()
        );
    }

    /// The hand reaches the UI the way the server sends it: in the init's
    /// game state, as `hand_info` on the bus and in the status reply; and
    /// the UI posts it in the body the server takes.
    #[test]
    fn the_hand_round_trips_on_the_wire() {
        use crate::types::{FlighthookEvent, HandInfo};

        let init = r#"{"kind":"init","version":"0.1.0","actor_id":"ws.1",
            "global_state":{"player_info":null,"club_info":null,"mode":null,"handed":"lh"}}"#;
        let Some(WsPollEvent::Init { global_state, .. }) = parse_init_response(init) else {
            panic!("not an init");
        };
        assert_eq!(global_state.unwrap().handed, Some(Handedness::Left));

        let sent = FlighthookMessage::new(FlighthookEvent::HandInfo {
            hand_info: HandInfo {
                handed: Handedness::Left,
            },
        })
        .actor("web");
        let text = serde_json::to_string(&sent).expect("ser");
        assert!(
            text.contains(r#""kind":"hand_info","hand_info":{"handed":"lh"}"#),
            "{text}"
        );
        assert!(parse_init_response(&text).is_none());
        let got = types::parse_ws_message(&text).expect("a bus message");
        assert_eq!(got.actor, "web");
        assert!(matches!(
            got.event,
            FlighthookEvent::HandInfo {
                hand_info: HandInfo {
                    handed: Handedness::Left
                }
            }
        ));

        let status: StatusResponse =
            serde_json::from_str(r#"{"actors":{},"mode":"full","club":"7I","handed":"rh"}"#)
                .expect("status");
        assert_eq!(status.handed, Some(Handedness::Right));

        assert_eq!(hand_body(Handedness::Left), r#"{"handed":"lh"}"#);
        assert_eq!(hand_body(Handedness::Right), r#"{"handed":"rh"}"#);
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
        use crate::types::{CameraPosition, FlighthookEvent, ShotKey};

        let status: StatusResponse = serde_json::from_str(
            r#"{"actors":{},"mode":null,"cameras_supported":true,"clip_dir":"/data/clips"}"#,
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

        let key = |n: u32| ShotKey {
            shot_id: format!("s{n}"),
            shot_number: n,
        };
        let sent = FlighthookMessage::new(FlighthookEvent::ShotVideo {
            keys: vec![key(1), key(2)],
            position: CameraPosition::GolferFront,
            duration_ms: 5500,
            fps: 120.0,
            trigger_offset_ms: 4000,
            impact_offset_ms: 4000,
            complete: true,
            dropped: 0,
        })
        .actor("camera.0");
        let text = serde_json::to_string(&sent).expect("ser");
        assert!(text.contains(r#""kind":"shot_video""#), "{text}");
        assert!(text.contains(r#""position":"golfer_front""#), "{text}");
        let got = types::parse_ws_message(&text).expect("a bus message");
        assert_eq!(got.actor, "camera.0");
        let FlighthookEvent::ShotVideo { keys, fps, .. } = got.event else {
            panic!("not shot_video");
        };
        assert_eq!((keys.len(), fps), (2, 120.0));

        let text = r#"{"actor":"camera.0","event":{"kind":"shot_video_removed","keys":[{"shot_id":"s1","shot_number":1}]}}"#;
        let got = types::parse_ws_message(text).expect("a bus message");
        assert!(
            matches!(got.event, FlighthookEvent::ShotVideoRemoved { ref keys } if keys == &[key(1)])
        );
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
