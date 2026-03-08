//! WebSocket handler — FRP-compliant init handshake + unified bus event streaming.
//!
//! Protocol (FRP-compliant with flighthook extensions):
//!   1. Client sends:  `{ "kind": "start", "version": ["0.1.0"], "name": "My Dashboard" }`
//!   2. Server negotiates version (highest mutually supported)
//!   3. Server sends:  `{ "kind": "init", "version": "0.1.0", "actor_id": "ws.abc123", "global_state": { ... } }`
//!   4. Server streams `FlighthookMessage` events

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};

use super::{WebState, emit_telemetry};
use crate::state::SystemState;
use crate::state::config;
use flighthook::{FRP_VERSION, FlighthookEvent, FlighthookMessage, ShotDetectionMode};

/// GET /frp — upgrade to WebSocket.
pub async fn ws_upgrade(
    State(state): State<Arc<WebState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: Arc<WebState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Phase 1: Wait for "start" message from client and negotiate version
    let start = loop {
        match ws_rx.next().await {
            Some(Ok(Message::Text(text))) => {
                if let Some(result) = parse_start_message(&text) {
                    break result;
                }
                // Not a start message — ignore and keep waiting
            }
            Some(Ok(Message::Close(_))) | None => return,
            _ => continue,
        }
    };

    // If no compatible version, send critical alert and close.
    let Some(negotiated_version) = start.version else {
        tracing::warn!("ws: client '{}' has no compatible FRP version", start.name);
        let alert = serde_json::json!({
            "kind": "alert",
            "severity": "critical",
            "message": format!("No compatible FRP version. Server supports: {}", SUPPORTED_VERSIONS.join(", ")),
        });
        let _ = ws_tx.send(Message::text(alert.to_string())).await;
        let _ = ws_tx.close().await;
        return;
    };

    let client_name = start.name;

    // Phase 2: Send "init" response with actor_id and global state
    let actor_id = format!("ws.{}", config::generate_id());
    state.ws_count.fetch_add(1, Ordering::Relaxed);
    emit_telemetry(&state, &state.bus_tx);
    tracing::info!(
        "ws: client '{}' connected (actor_id={}, version={})",
        client_name,
        actor_id,
        negotiated_version
    );

    let global_state = state.root.game.snapshot();
    let init_msg = serde_json::json!({
        "kind": "init",
        "version": negotiated_version,
        "actor_id": actor_id,
        "global_state": global_state,
    });
    if ws_tx
        .send(Message::text(init_msg.to_string()))
        .await
        .is_err()
    {
        return;
    }

    // Replay cached ActorStatus messages (last per actor)
    {
        let cache = state.cached_actor_status.read().await;
        for msg in cache.values() {
            if let Ok(json) = serde_json::to_string(msg)
                && ws_tx.send(Message::text(json)).await.is_err()
            {
                return;
            }
        }
    }

    // Replay cached DeviceTelemetry messages (last per actor)
    {
        let cache = state.cached_device_telemetry.read().await;
        for msg in cache.values() {
            if let Ok(json) = serde_json::to_string(msg)
                && ws_tx.send(Message::text(json)).await.is_err()
            {
                return;
            }
        }
    }

    // Phase 3: Stream bus events + receive commands
    let mut bus_rx = state.bus_tx.subscribe();

    let mut send_task = tokio::spawn(async move {
        loop {
            match bus_rx.recv().await {
                Ok(msg) => {
                    if let Ok(json) = serde_json::to_string(&msg)
                        && ws_tx.send(Message::text(json)).await.is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("ws: lagged {n}");
                }
            }
        }
    });

    let ws_actor = actor_id.clone();
    let bus_tx = state.bus_tx.clone();
    let system = Arc::clone(&state.root);
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => {
                    handle_ws_command(&text, &ws_actor, &bus_tx, &system);
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    state.ws_count.fetch_sub(1, Ordering::Relaxed);
    emit_telemetry(&state, &state.bus_tx);
    tracing::info!(
        "ws: client '{}' disconnected (actor_id={})",
        client_name,
        actor_id
    );
}

/// Parsed result from a "start" handshake message.
struct StartResult {
    name: String,
    /// Negotiated FRP version, or `None` if no compatible version found.
    version: Option<String>,
}

/// Supported FRP versions, in ascending order.
const SUPPORTED_VERSIONS: &[&str] = &[FRP_VERSION];

/// Parse a "start" handshake message and negotiate the FRP version.
///
/// Expects `{"kind": "start", "version": ["0.1.0"], "name": "..."}`.
/// Selects the highest mutually supported version from the client's array.
fn parse_start_message(text: &str) -> Option<StartResult> {
    #[derive(serde::Deserialize)]
    struct StartMsg {
        kind: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        version: Vec<String>,
    }
    let msg: StartMsg = serde_json::from_str(text).ok()?;
    if msg.kind != "start" {
        return None;
    }

    let name = if msg.name.is_empty() {
        "anonymous".to_string()
    } else {
        msg.name
    };

    // Select the highest mutually supported version.
    let negotiated = SUPPORTED_VERSIONS
        .iter()
        .rev()
        .find(|v| msg.version.iter().any(|cv| cv == **v))
        .map(|v| v.to_string());

    Some(StartResult {
        name,
        version: negotiated,
    })
}

/// Parse a client command and emit on the bus.
/// Expected formats:
///   `{ "cmd": "mode", "mode": "putting", "device": "mevo.0" }`
fn handle_ws_command(
    text: &str,
    actor: &str,
    bus_tx: &tokio::sync::broadcast::Sender<FlighthookMessage>,
    _system: &SystemState,
) {
    #[derive(serde::Deserialize)]
    struct WsCmd {
        cmd: String,
        mode: Option<String>,
    }

    let Ok(msg) = serde_json::from_str::<WsCmd>(text) else {
        return;
    };

    if msg.cmd.as_str() == "mode" {
        let mode = match msg.mode.as_deref() {
            Some("full") => Some(ShotDetectionMode::Full),
            Some("putting") => Some(ShotDetectionMode::Putting),
            Some("chipping") => Some(ShotDetectionMode::Chipping),
            _ => None,
        };
        if let Some(m) = mode {
            let _ = bus_tx.send(
                FlighthookMessage::new(FlighthookEvent::SetDetectionMode { mode: Some(m), handed: None })
                    .actor(actor),
            );
        }
    }
}
