//! WebSocket handler — init handshake + unified bus event streaming.
//!
//! Protocol:
//!   1. Client sends:  `{ "type": "start", "name": "My Dashboard" }`
//!   2. Server sends:  `{ "type": "init", "source_id": "ws.abc123", "global_state": { ... } }`
//!   3. Server streams `FlighthookMessage` events

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};

use super::{WebState, emit_telemetry};
use crate::state::SystemState;
use crate::state::config;
use flighthook::ShotDetectionMode;
use flighthook::{FlighthookMessage, GameStateCommandEvent};

/// GET /api/ws — upgrade to WebSocket.
pub async fn ws_upgrade(
    State(state): State<Arc<WebState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: Arc<WebState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Phase 1: Wait for "start" message from client
    let client_name = loop {
        match ws_rx.next().await {
            Some(Ok(Message::Text(text))) => {
                if let Some(name) = parse_start_message(&text) {
                    break name;
                }
                // Not a start message — ignore and keep waiting
            }
            Some(Ok(Message::Close(_))) | None => return,
            _ => continue,
        }
    };

    // Phase 2: Send "init" response with source_id and global state
    let source_id = format!("ws.{}", config::generate_id());
    state.ws_count.fetch_add(1, Ordering::Relaxed);
    emit_telemetry(&state, &state.bus_tx);
    tracing::info!(
        "ws: client '{}' connected (source_id={})",
        client_name,
        source_id
    );

    let global_state = state.root.game.snapshot();
    let init_msg = serde_json::json!({
        "type": "init",
        "source_id": source_id,
        "global_state": global_state,
    });
    if ws_tx
        .send(Message::text(init_msg.to_string()))
        .await
        .is_err()
    {
        return;
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

    let ws_source = source_id.clone();
    let bus_tx = state.bus_tx.clone();
    let system = Arc::clone(&state.root);
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => {
                    handle_ws_command(&text, &ws_source, &bus_tx, &system);
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
        "ws: client '{}' disconnected (source_id={})",
        client_name,
        source_id
    );
}

/// Parse a "start" handshake message. Returns the client name if valid.
fn parse_start_message(text: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct StartMsg {
        #[serde(rename = "type")]
        msg_type: String,
        #[serde(default)]
        name: String,
    }
    let msg: StartMsg = serde_json::from_str(text).ok()?;
    if msg.msg_type == "start" {
        Some(if msg.name.is_empty() {
            "anonymous".to_string()
        } else {
            msg.name
        })
    } else {
        None
    }
}

/// Parse a client command and emit on the bus.
/// Expected formats:
///   `{ "cmd": "mode", "mode": "putting", "device": "mevo.0" }`
fn handle_ws_command(
    text: &str,
    source: &str,
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

    match msg.cmd.as_str() {
        "mode" => {
            let mode = match msg.mode.as_deref() {
                Some("full") => Some(ShotDetectionMode::Full),
                Some("putting") => Some(ShotDetectionMode::Putting),
                Some("chipping") => Some(ShotDetectionMode::Chipping),
                _ => None,
            };
            if let Some(m) = mode {
                let _ = bus_tx.send(
                    FlighthookMessage::new(GameStateCommandEvent::SetMode { mode: m })
                        .source(source),
                );
            }
        }
        _ => {}
    }
}
