//! Settings routes: `GET /api/settings`, `POST /api/settings` (scope, validation, the enable
//! refusal, the single web server rule) and the config command timeout.

use super::*;

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
pub(super) fn scoped_action(scope: &str, config: &FlighthookConfig) -> Option<ConfigAction> {
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
        "trackball" => upsert!(trackball, UpsertTrackball),
        "openconnect_server" => upsert!(openconnect_server, UpsertOpenConnectServer),
        "openconnect_client" => upsert!(openconnect_client, UpsertOpenConnectClient),
        "pureplay" => upsert!(pureplay, UpsertPureplay),
        "pureplay_accessory" => upsert!(pureplay_accessory, UpsertPureplayAccessory),
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
pub(super) fn enable_refusal(
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
                "trackball" => copy!(trackball),
                "openconnect_server" => copy!(openconnect_server),
                "openconnect_client" => copy!(openconnect_client),
                "pureplay" => copy!(pureplay),
                "pureplay_accessory" => copy!(pureplay_accessory),
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
pub(super) fn webserver_violation(
    live: &FlighthookConfig,
    action: &ConfigAction,
) -> Option<&'static str> {
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
/// a golfer's height out of range ([`GolferRecord::error`](flighthook::GolferRecord::error)),
/// a golfer with no name or an identifier that is not a golfer id
/// ([`golfer_identity_error`](FlighthookConfig::golfer_identity_error)),
/// one golfer id in two golfers
/// ([`duplicate_golfer_id`](FlighthookConfig::duplicate_golfer_id), naming both),
/// or an output routed to a Pureplay Accessory card
/// ([`SIMULATOR_ROUTE_ERROR`](flighthook::SIMULATOR_ROUTE_ERROR)).
pub(super) fn invalid_setting(action: &ConfigAction) -> Option<String> {
    if let ConfigAction::ReplaceAll { config } | ConfigAction::ReplaceGlobal { config } = action {
        if let Some(error) = config.replay.error() {
            return Some(format!("replay: {error}"));
        }
        if let Some(error) = config.body_tracking.error() {
            return Some(format!("body_tracking: {error}"));
        }
        if let Some(error) = config.golfer_error() {
            return Some(error);
        }
        if let Some(error) = config.golfer_identity_error() {
            return Some(error);
        }
        if let Some(dup) = config.duplicate_golfer_id() {
            return Some(dup.to_string());
        }
    }
    if let ConfigAction::SetBodyTracking { section } = action
        && let Some(error) = section.error()
    {
        return Some(format!("body_tracking: {error}"));
    }
    if let ConfigAction::SetGolfers { golfers } = action {
        let config = FlighthookConfig {
            golfers: golfers.clone(),
            ..FlighthookConfig::default()
        };
        if let Some(error) = config.golfer_error() {
            return Some(error);
        }
        if let Some(error) = config.golfer_identity_error() {
            return Some(error);
        }
        if let Some(dup) = config.duplicate_golfer_id() {
            return Some(dup.to_string());
        }
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
pub(super) fn invalid_driver_setting(
    saved: &FlighthookConfig,
    action: &ConfigAction,
) -> Option<String> {
    use flighthook::DeviceSection;
    match action {
        ConfigAction::ReplaceAll { config } => config.changed_device_error(saved),
        ConfigAction::UpsertMevo { index, section } => {
            section.changed_driver_error(saved.mevo.get(index), &format!("mevo.{index}"))
        }
        ConfigAction::UpsertMockMonitor { index, section } => section.changed_driver_error(
            saved.mock_monitor.get(index),
            &format!("mock_monitor.{index}"),
        ),
        ConfigAction::UpsertR10 { index, section } => {
            section.changed_driver_error(saved.r10.get(index), &format!("r10.{index}"))
        }
        ConfigAction::UpsertSquare { index, section } => {
            section.changed_driver_error(saved.square.get(index), &format!("square.{index}"))
        }
        ConfigAction::UpsertExPutt { index, section } => {
            section.changed_driver_error(saved.exputt.get(index), &format!("exputt.{index}"))
        }
        ConfigAction::UpsertTrackball { index, section } => {
            section.changed_driver_error(saved.trackball.get(index), &format!("trackball.{index}"))
        }
        _ => None,
    }
}

/// [`invalid_setting`] for a camera's `latency_s`, `rotation_deg`, `yaw_deg`
/// and `pitch_deg`.
pub(super) fn invalid_camera_setting(action: &ConfigAction) -> Option<String> {
    match action {
        ConfigAction::ReplaceAll { config } => config.camera_error(),
        ConfigAction::UpsertCamera { index, section } => section.error(&format!("camera.{index}")),
        _ => None,
    }
}

/// Whether `action` would route an output to a Pureplay Accessory card, whose shots
/// no simulator output takes.
pub(super) fn routes_to_simulator(saved: &FlighthookConfig, action: &ConfigAction) -> bool {
    match action {
        ConfigAction::ReplaceAll { config } => {
            // The cards as the save leaves them: with the ids the posted
            // copy leaves out.
            let mut result = (**config).clone();
            result.keep_actor_ids_from(saved);
            !result.simulator_routes().is_empty()
        }
        ConfigAction::UpsertOpenConnectClient { section, .. } => section.routes_to_simulator(saved),
        ConfigAction::UpsertPureplay { section, .. } => section.routes_to_simulator(saved),
        _ => false,
    }
}

/// [`invalid_setting`] for a launch monitor's `impact_lead_s`.
pub(super) fn invalid_impact_lead(action: &ConfigAction) -> Option<String> {
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
        ConfigAction::UpsertPureplayAccessory { index, section } => {
            ("pureplay_accessory", index, section.impact_lead_s)
        }
        _ => return None,
    };
    flighthook::impact_lead_error(&format!("{kind}.{index}"), lead)
}

/// [`invalid_setting`] for Mevo sections.
pub(super) fn invalid_mevo_setting(action: &ConfigAction) -> Option<String> {
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
/// actor; `?scope=golfers` likewise writes only `[[golfers]]`. Without a scope the whole config is replaced and every actor is
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

pub(super) fn settings_error(status: StatusCode, message: &str) -> SettingsError {
    (status, Json(serde_json::json!({ "error": message })))
}

/// Apply `new_config` under `scope`, waiting up to `timeout` for the system
/// actor.
pub(super) async fn save_settings(
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
        Some("golfers") => ConfigAction::SetGolfers {
            golfers: new_config.golfers,
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
    let saved = state.root.system.snapshot();
    if let Some(reason) = invalid_setting(&action)
        .or_else(|| invalid_driver_setting(&saved, &action))
        .or_else(|| {
            routes_to_simulator(&saved, &action).then(|| flighthook::SIMULATOR_ROUTE_ERROR.into())
        })
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

    let golfers_saved = matches!(action, ConfigAction::SetGolfers { .. });
    match send_config_command_within(state, action, timeout).await {
        Some(Outcome {
            restarted, stopped, ..
        }) => Ok(Json(PostSettingsResponse {
            restarted,
            stopped,
            // A new golfer's id is given by the save: the caller keeps it.
            golfers: if golfers_saved {
                state.root.system.snapshot().golfers
            } else {
                Vec::new()
            },
        })),
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
pub(super) const CONFIG_TIMEOUT: Duration =
    Duration::from_secs(crate::actors::MAX_STOP_WAIT.as_secs() + START_ROOM.as_secs());

/// What a config command takes beyond its stop waits: the config write and
/// the starts.
pub(super) const START_ROOM: Duration = Duration::from_secs(4);

// The dashboard waits longer for a save or a reconnect than the server
// does, so it always gets the server's answer.
#[cfg(feature = "gui")]
const _: () =
    assert!(flighthook_ui::net::CONFIG_REQUEST_TIMEOUT.as_secs() > CONFIG_TIMEOUT.as_secs() + 1);
