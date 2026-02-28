//! Networking — ehttp REST + ewebsock WebSocket.
//!
//! Async results are placed in a shared `Pending` queue (Arc<Mutex>)
//! that the app drains each frame. This avoids touching egui's internal
//! data store from async callbacks.

use std::sync::{Arc, Mutex};

use crate::types::{
    self, FlighthookConfig, FlighthookMessage, PostSettingsResponse, ShotData, StatusResponse,
};

// ---------------------------------------------------------------------------
// Pending results queue — shared between async callbacks and the app.
// Arc<Mutex> instead of Rc<RefCell> because ehttp 0.6 requires Send callbacks.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Pending {
    pub status: Option<StatusResponse>,
    pub shots: Option<Vec<ShotData>>,
    pub settings: Option<FlighthookConfig>,
    pub settings_save: Option<PostSettingsResponse>,
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
        "http://127.0.0.1:3030".to_string()
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
        "ws://127.0.0.1:3030/api/ws".to_string()
    } else {
        format!("{ws_proto}//{host}/api/ws")
    }
}

#[cfg(not(target_arch = "wasm32"))]
static NATIVE_BASE_URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set the base URL for native builds (e.g. "http://127.0.0.1:3030").
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
        .unwrap_or_else(|| "http://127.0.0.1:3030".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn ws_url() -> String {
    let base = api_base();
    let ws_base = base
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    format!("{ws_base}/api/ws")
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

pub fn fetch_shots(ctx: &egui::Context, pending: &PendingHandle) {
    let ctx = ctx.clone();
    let pending = Arc::clone(pending);
    let url = format!("{}/api/shots?limit=100", api_base());
    ehttp::fetch(ehttp::Request::get(&url), move |result| {
        if let Ok(resp) = result
            && let Ok(shots) = serde_json::from_slice::<Vec<ShotData>>(&resp.bytes)
            && let Ok(mut p) = pending.lock()
        {
            p.shots = Some(shots);
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
        if let Ok(resp) = result
            && let Ok(save_resp) = serde_json::from_slice::<PostSettingsResponse>(&resp.bytes)
            && let Ok(mut p) = pending.lock()
        {
            p.settings_save = Some(save_resp);
            ctx.request_repaint();
        }
    });
}

pub fn post_mode(ctx: &egui::Context, mode: &str) {
    let ctx = ctx.clone();
    let url = format!("{}/api/mode", api_base());
    let body = format!(r#"{{"mode":"{mode}"}}"#).into_bytes();
    let mut req = ehttp::Request::post(&url, body);
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
        r#"{"type":"start","name":"Flighthook Dashboard"}"#.into(),
    ));
}

/// Result from polling the WebSocket.
pub enum WsPollEvent {
    /// Browser WebSocket opened (WASM: onopen fired).
    Opened,
    /// Init handshake completed — server assigned a source_id.
    Init { source_id: String },
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
                // Try init response first (has "type" field)
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

/// Parse a WS init response: `{ "type": "init", "source_id": "..." }`
fn parse_init_response(text: &str) -> Option<WsPollEvent> {
    #[derive(serde::Deserialize)]
    struct InitMsg {
        #[serde(rename = "type")]
        msg_type: String,
        source_id: Option<String>,
    }
    let msg: InitMsg = serde_json::from_str(text).ok()?;
    if msg.msg_type == "init" {
        Some(WsPollEvent::Init {
            source_id: msg.source_id.unwrap_or_default(),
        })
    } else {
        None
    }
}
