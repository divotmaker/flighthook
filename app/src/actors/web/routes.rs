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
use flighthook::{ConfigAction, FlighthookEvent, FlighthookMessage, ShotData, UnitSystem};

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
    let _ = state.bus_tx.send(
        FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
            mode: Some(mode),
            handed: None,
        })
        .actor("web"),
    );
    StatusCode::ACCEPTED
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
/// not carry is a removal.
///
/// Returns `None` for a malformed ID or an unknown type prefix.
fn scoped_action(scope: &str, config: &FlighthookConfig) -> Option<ConfigAction> {
    let (prefix, index) = scope.split_once('.')?;
    let idx = index.to_string();
    let id = scope.to_string();

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
        "openconnect_server" => upsert!(openconnect_server, UpsertOpenConnectServer),
        "gspro" => upsert!(gspro, UpsertGsPro),
        "mock_monitor" => upsert!(mock_monitor, UpsertMockMonitor),
        "random_club" => upsert!(random_club, UpsertRandomClub),
        _ => return None,
    })
}

/// POST /api/settings — config replacement via bus request-reply.
///
/// Emits a `ConfigCommand` on the bus, waits for `ConfigOutcome` with a
/// matching `request_id`, then returns the response. SystemActor handles
/// persistence and actor reconciliation.
///
/// `?scope=<global_id>` narrows the save to one actor: only that section is
/// written and only that actor is reconciled. Without it the whole config is
/// replaced and every actor is reconciled.
pub async fn post_settings(
    State(state): State<Arc<WebState>>,
    Query(query): Query<SettingsQuery>,
    Json(new_config): Json<FlighthookConfig>,
) -> Json<PostSettingsResponse> {
    let action = match query.scope.as_deref() {
        None => ConfigAction::ReplaceAll { config: new_config },
        Some(scope) => match scoped_action(scope, &new_config) {
            Some(action) => action,
            // Applying the body wholesale here would save far more than the
            // caller asked for, so reject instead.
            None => {
                tracing::warn!("config update: unknown scope '{scope}', ignoring request");
                return Json(PostSettingsResponse {
                    restarted: Vec::new(),
                    stopped: Vec::new(),
                });
            }
        },
    };

    let request_id = crate::state::config::generate_id();
    let mut bus_rx = state.bus_tx.subscribe();

    // Emit ConfigCommand on the bus
    let _ = state.bus_tx.send(
        FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some(request_id.clone()),
            action: Box::new(action),
        })
        .actor("web"),
    );

    // Wait for ConfigOutcome with matching request_id (10s timeout)
    let result = tokio::time::timeout(Duration::from_secs(10), async {
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
                        return Some((restarted.clone(), stopped.clone()));
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    })
    .await;

    match result {
        Ok(Some((restarted, stopped))) => Json(PostSettingsResponse { restarted, stopped }),
        _ => {
            tracing::warn!("config update: timed out waiting for ConfigOutcome");
            Json(PostSettingsResponse {
                restarted: Vec::new(),
                stopped: Vec::new(),
            })
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

    fn mevo_section(name: &str) -> MevoSection {
        let mut s: MevoSection = serde_json::from_str("{}").expect("empty section");
        s.name = name.to_string();
        s
    }

    #[test]
    fn scope_upserts_only_the_named_section() {
        let mut c = config();
        c.mevo.insert("0".into(), mevo_section("Mevo WiFi"));
        c.gspro.insert(
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
    fn scope_naming_an_absent_section_is_a_removal() {
        let action = scoped_action("mevo.0", &config()).expect("known scope");
        match action {
            ConfigAction::Remove { id } => assert_eq!(id, "mevo.0"),
            other => panic!("expected Remove, got {other:?}"),
        }
    }

    #[test]
    fn malformed_and_unknown_scopes_are_rejected() {
        assert!(scoped_action("mevo", &config()).is_none());
        assert!(scoped_action("nosuchtype.0", &config()).is_none());
    }
}
