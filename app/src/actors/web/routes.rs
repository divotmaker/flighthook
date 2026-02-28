//! REST endpoint handlers.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::broadcast;

use super::WebState;
use super::types::{ModeRequest, PostSettingsResponse, StatusResponse};
use crate::state::config::FlighthookConfig;
use flighthook::{
    ConfigAction, ConfigCommand, FlighthookEvent, FlighthookMessage, GameStateCommandEvent,
    ShotData, UnitSystem,
};

// ---------------------------------------------------------------------------
// Embedded UI assets (built by `make ui` in flighthook/ui/)
// ---------------------------------------------------------------------------

const UI_HTML: &str = include_str!("../../../../ui/dist/index.html");
const UI_JS: &[u8] = include_bytes!("../../../../ui/dist/flighthook-ui.js");
const UI_WASM: &[u8] = include_bytes!("../../../../ui/dist/flighthook-ui_bg.wasm");

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
    let actors_guard = state.actors.read().await;
    let actors = actors_guard
        .iter()
        .map(|(id, a)| (id.clone(), a.clone()))
        .collect();

    let mode = state.root.game.snapshot().mode;
    Json(StatusResponse { actors, mode })
}

/// GET /api/shots?limit=50&units=imperial|metric
pub async fn get_shots(
    State(state): State<Arc<WebState>>,
    Query(query): Query<ShotsQuery>,
) -> Json<Vec<ShotData>> {
    let shots = state.shots.read().await;
    let start = shots.len().saturating_sub(query.limit);
    let unit_system = query.units.as_deref().and_then(|u| match u {
        "imperial" => Some(UnitSystem::Imperial),
        "metric" => Some(UnitSystem::Metric),
        _ => None,
    });
    let result: Vec<ShotData> = shots
        .iter()
        .skip(start)
        .map(|s| match unit_system {
            Some(system) => s.to_unit_system(system),
            None => s.clone(),
        })
        .collect();
    Json(result)
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
    Ok(Json(shot.to_unit_system(system)))
}

/// POST /api/mode
pub async fn post_mode(
    State(state): State<Arc<WebState>>,
    Json(body): Json<ModeRequest>,
) -> StatusCode {
    let mode = body.mode;
    let _ = state
        .bus_tx
        .send(FlighthookMessage::new(GameStateCommandEvent::SetMode { mode }).source("web"));
    StatusCode::ACCEPTED
}

/// GET /api/settings — returns the full persisted config.
pub async fn get_settings(State(state): State<Arc<WebState>>) -> Json<FlighthookConfig> {
    Json(state.root.system.snapshot())
}

/// POST /api/settings — config replacement via bus request-reply.
///
/// Emits a `ConfigCommand` on the bus, waits for `ConfigOutcome` with a
/// matching `request_id`, then returns the response. SystemActor handles
/// persistence and actor reconciliation.
pub async fn post_settings(
    State(state): State<Arc<WebState>>,
    Json(new_config): Json<FlighthookConfig>,
) -> Json<PostSettingsResponse> {
    let request_id = crate::state::config::generate_id();
    let mut bus_rx = state.bus_tx.subscribe();

    // Emit ConfigCommand on the bus
    let _ = state.bus_tx.send(
        FlighthookMessage::new(ConfigCommand {
            request_id: Some(request_id.clone()),
            action: ConfigAction::ReplaceAll { config: new_config },
        })
        .source("web"),
    );

    // Wait for ConfigOutcome with matching request_id (10s timeout)
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match bus_rx.recv().await {
                Ok(msg) => {
                    if let FlighthookEvent::ConfigOutcome(ref r) = msg.event {
                        if r.request_id == request_id {
                            return Some(r.clone());
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    })
    .await;

    match result {
        Ok(Some(r)) => Json(PostSettingsResponse {
            restarted: r.restarted,
            stopped: r.stopped,
        }),
        _ => {
            tracing::warn!("config update: timed out waiting for ConfigOutcome");
            Json(PostSettingsResponse {
                restarted: Vec::new(),
                stopped: Vec::new(),
            })
        }
    }
}
