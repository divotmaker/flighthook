//! Networking — ehttp REST + ewebsock WebSocket.
//!
//! Async results are placed in a shared `Pending` queue (Arc<Mutex>)
//! that the app drains each frame. This avoids touching egui's internal
//! data store from async callbacks.

use std::sync::{Arc, Mutex};

use crate::types::{
    self, Club, FlighthookConfig, FlighthookMessage, GameStateSnapshot, PostSettingsResponse,
    ShotData, ShotGrouping, ShotGroupsResponse, StatusResponse,
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
    ehttp::fetch(http_req, move |result| {
        if let Ok(mut p) = pending.lock() {
            p.settings_save = Some(save_reply(result));
        }
        ctx.request_repaint();
    });
}

/// Longest server error text a save failure carries.
const SAVE_ERROR_MAX_CHARS: usize = 120;

/// A `POST /api/settings` reply as applied (`Ok`) or not (`Err` with a short
/// reason): a network error, a non-2xx status (its JSON `error`, else its
/// text, else the status) or a body that is not a `PostSettingsResponse`.
pub(crate) fn save_reply(
    result: Result<ehttp::Response, String>,
) -> Result<PostSettingsResponse, String> {
    let resp = result?;
    if !resp.ok {
        let json_error = serde_json::from_slice::<serde_json::Value>(&resp.bytes)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(str::to_owned));
        let text = json_error
            .or_else(|| resp.text().map(|t| t.trim().to_owned()))
            .filter(|t| !t.is_empty())
            .map(|t| t.chars().take(SAVE_ERROR_MAX_CHARS).collect::<String>());
        return Err(match text {
            Some(text) => format!("{} {}", resp.status, text),
            None => format!("{} {}", resp.status, resp.status_text),
        });
    }
    serde_json::from_slice::<PostSettingsResponse>(&resp.bytes)
        .map_err(|_| "unexpected reply from the server".to_owned())
}

pub fn post_mode(ctx: &egui::Context, mode: &str) {
    post_json(ctx, "/api/mode", format!(r#"{{"mode":"{mode}"}}"#));
}

/// Select the club in play (`POST /api/club`).
pub fn post_club(ctx: &egui::Context, club: Club) {
    post_json(ctx, "/api/club", format!(r#"{{"club":"{club}"}}"#));
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
}
