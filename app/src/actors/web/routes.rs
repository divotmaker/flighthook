//! REST endpoint handlers.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::broadcast;

use super::types::{
    ClubRequest, GroupingRequest, ModeRequest, PostSettingsResponse, ShotGroupsResponse,
    StatusResponse,
};
use super::{MAX_SHOTS, StoredShot, WebState, export};
use crate::actors::actor_names;
use crate::state::config::FlighthookConfig;
use flighthook::{
    Club, ClubInfo, ConfigAction, FlighthookEvent, FlighthookMessage, ShotData, UnitSystem,
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
    // Names from the config as it is now: a rename shows before the bus
    // event that applied it reaches the cache.
    let names = actor_names(&state.root.system.snapshot());
    let actors = super::actor_statuses(&*state.actors.read().await, &names, state.now_ms());

    let game = state.root.game.snapshot();
    Json(StatusResponse {
        actors,
        mode: game.mode,
        club: game.club_info.map(|c| c.club),
    })
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
/// the system actor has applied it; `504` when it does not within 10 s.
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
        "openconnect_server" => upsert!(openconnect_server, UpsertOpenConnectServer),
        "openconnect_client" => upsert!(openconnect_client, UpsertOpenConnectClient),
        "mock_monitor" => upsert!(mock_monitor, UpsertMockMonitor),
        "random_club" => upsert!(random_club, UpsertRandomClub),
        _ => return None,
    })
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

/// POST /api/settings — config replacement via bus request-reply.
///
/// Emits a `ConfigCommand` on the bus, waits for `ConfigOutcome` with a
/// matching `request_id`, then returns the response. SystemActor handles
/// persistence and actor reconciliation.
///
/// `?scope=<global_id>` narrows the save to one actor: only that section is
/// written and only that actor is reconciled. `?scope=global` writes only the
/// global settings, keeping every actor section of the live config. Without
/// a scope the whole config is replaced and every actor is reconciled.
///
/// An unknown scope, a removed web server or a second web server is `400`
/// and a missing `ConfigOutcome` `504`, each with a JSON `{"error": "..."}`
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
    let action = match scope {
        None => ConfigAction::ReplaceAll {
            config: Box::new(new_config),
        },
        Some("global") => ConfigAction::ReplaceGlobal {
            config: Box::new(new_config),
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

    match send_config_command_within(state, action, timeout).await {
        Some((restarted, stopped)) => Ok(Json(PostSettingsResponse { restarted, stopped })),
        None => {
            tracing::warn!("config update: timed out waiting for ConfigOutcome");
            Err(settings_error(
                StatusCode::GATEWAY_TIMEOUT,
                "the settings were not applied in time",
            ))
        }
    }
}

/// How long a config command waits for its `ConfigOutcome`.
const CONFIG_TIMEOUT: Duration = Duration::from_secs(10);

/// Emit a `ConfigCommand` for `action` and wait up to `timeout` for the
/// system actor's matching `ConfigOutcome`. Returns its `(restarted,
/// stopped)` actor IDs, or `None` on timeout or a closed bus.
async fn send_config_command_within(
    state: &WebState,
    action: ConfigAction,
    timeout: Duration,
) -> Option<(Vec<String>, Vec<String>)> {
    let request_id = crate::state::config::generate_id();
    let mut bus_rx = state.bus_tx.subscribe();

    let _ = state.bus_tx.send(
        FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some(request_id.clone()),
            action: Box::new(action),
        })
        .actor("web"),
    );

    let result = tokio::time::timeout(timeout, async {
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
    result.ok().flatten()
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
    use flighthook::{BallFlight, DerivedFlight, Distance, ShotData, Velocity};

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
            ball: Some(BallFlight {
                launch_speed: Some(Velocity::MetersPerSecond(53.6)),
                launch_elevation: Some(17.0),
                launch_azimuth: Some(-1.2),
                carry_distance: None,
                total_distance: None,
                roll_distance: None,
                max_height: None,
                flight_time: None,
                backspin_rpm: Some(6500),
                sidespin_rpm: Some(-300),
                ..BallFlight::default()
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
    use flighthook::{BallFlight, ClubData, Distance, ShotKey, Velocity};

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
        let ball = BallFlight {
            launch_speed: Some(Velocity::MetersPerSecond(40.0)),
            launch_azimuth: Some(2.0),
            launch_elevation: Some(20.0),
            carry_distance: Some(Distance::Meters(100.0)),
            carry_offline: Some(Distance::Meters(-1.5)),
            descent_angle: Some(44.2),
            backspin_rpm: Some(6000),
            sidespin_rpm: Some(-800),
            ..BallFlight::default()
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
                FlighthookEvent::ShotTrigger { key: key.clone() },
                FlighthookEvent::BallFlight {
                    key: key.clone(),
                    ball: Box::new(ball),
                    derived: None,
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
        assert_eq!(shot["ball"]["carry_offline"], "-1.5m", "{shot}");
        assert_eq!(shot["ball"]["descent_angle"], 44.2, "{shot}");
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
            serde_json::from_value(shot["ball"]["carry_offline"].clone()).expect("offline");
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
                FlighthookEvent::ShotTrigger { key: key.clone() },
                FlighthookEvent::BallFlight {
                    key: key.clone(),
                    ball: Box::new(BallFlight {
                        launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                        backspin_rpm: Some(6000),
                        ..BallFlight::default()
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
    use super::super::tests::{feed, test_state};
    use super::*;
    use flighthook::{BallFlight, ShotGrouping, ShotKey, Velocity};

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
                FlighthookEvent::ShotTrigger { key: key.clone() },
                FlighthookEvent::BallFlight {
                    key: key.clone(),
                    ball: Box::new(BallFlight {
                        launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                        ..BallFlight::default()
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
                    crate::actors::system::handle_config_command(
                        &msg.event, &root, &bus_tx, &sender,
                    );
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
                        crate::actors::system::handle_config_command(
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
                FlighthookEvent::ShotTrigger { key: key.clone() },
                FlighthookEvent::BallFlight {
                    key: key.clone(),
                    ball: Box::new(BallFlight {
                        launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                        ..BallFlight::default()
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
                FlighthookEvent::ShotTrigger { key: key.clone() },
                FlighthookEvent::BallFlight {
                    key: key.clone(),
                    ball: Box::new(BallFlight {
                        launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                        ..BallFlight::default()
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
}
