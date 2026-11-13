//! Commands to actors: `POST /api/select` (a `golfer.select`), find mat, Auto-chip, and sending
//! a command as flighthook and waiting for its reply.

use super::*;

/// POST /api/select — a `golfer.select` from the flighthook actor with the
/// body's fields (`golfer`, `club`, `handedness`, `mode`; each optional):
/// 200 once the session answered, 400 `{"error":"bad_request"}` for a body
/// that is no select (or a value it does not know), 504 when nothing
/// answered. A connected simulator card that holds a club, hand or mode
/// sends its own back.
pub async fn post_select(State(state): State<Arc<WebState>>, body: axum::body::Bytes) -> Response {
    post_select_within(&state, &body, REPLY_TIMEOUT).await
}

pub(crate) async fn post_select_within(state: &WebState, body: &[u8], wait: Duration) -> Response {
    let Some(select) = parse_select(body) else {
        return settings_error(StatusCode::BAD_REQUEST, "bad_request").into_response();
    };
    let command = |r#ref| {
        let mut select = select;
        select.r#ref = r#ref;
        flightrelay::FrpBody::from(select)
    };
    reply_response(command_as_flighthook(state, command, wait).await)
}

/// The select in `body`, its `ref` empty; `None` when it is no JSON object,
/// or a field present with a value reads as unknown.
pub(super) fn parse_select(body: &[u8]) -> Option<GolferSelect> {
    let serde_json::Value::Object(mut fields) = serde_json::from_slice(body).ok()? else {
        return None;
    };
    let given = |key: &str| fields.get(key).is_some_and(|v| !v.is_null());
    let (golfer, club, hand, mode) = (
        given("golfer"),
        given("club"),
        given("handedness"),
        given("mode"),
    );
    fields.insert("ref".into(), String::new().into());
    let select: GolferSelect = serde_json::from_value(serde_json::Value::Object(fields)).ok()?;
    let known = (!golfer || matches!(select.golfer, Some(Some(_))))
        && (!club || matches!(select.club, Some(Some(_))))
        && (!hand || select.handedness.is_some())
        && (!mode || select.mode.is_some());
    known.then_some(select)
}

/// The answer to a command whose `bus.reply` was `reply`: 200 for ok, 400
/// with its error code, 504 for none.
pub(super) fn reply_response(reply: Option<flightrelay::BusReply>) -> Response {
    match reply {
        None => settings_error(StatusCode::GATEWAY_TIMEOUT, "no reply").into_response(),
        Some(reply) => match reply.error {
            None => StatusCode::OK.into_response(),
            Some(error) => settings_error(
                StatusCode::BAD_REQUEST,
                serde_json::to_value(error)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .as_deref()
                    .unwrap_or("bad_request"),
            )
            .into_response(),
        },
    }
}

/// POST /api/find-mat — have the ExPutt `actor` locate its mat again.
///
/// Emits `find_mat` on the bus (actor `web`); the ExPutt actor acts on it
/// while it is measuring. 404 with a text body when no ExPutt has that ID.
pub async fn post_find_mat(
    State(state): State<Arc<WebState>>,
    Json(body): Json<FindMatRequest>,
) -> Response {
    let configured = body
        .actor
        .strip_prefix("exputt.")
        .is_some_and(|index| state.root.system.snapshot().exputt.contains_key(index));
    if !configured {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )
            .body(Body::from(format!("no ExPutt with ID '{}'", body.actor)))
            .unwrap();
    }
    let _ = state.bus_tx.send(BusMsg::local(
        "web",
        LocalEvent::FindMat { target: body.actor },
    ));
    StatusCode::ACCEPTED.into_response()
}

/// POST /api/auto-chip — send a short, straight chip of `yards` total
/// (one of [`flighthook::AUTO_CHIP_YARDS`]) as the next Auto-chip shot.
///
/// The ball speed is solved against the live `[simulation]` environment and
/// surface ([`crate::autochip`]); the shot goes on the bus as actor
/// [`flighthook::AUTO_CHIP_ACTOR`], so every output forwards it and it is
/// stored like a device's shot. 409 when Auto-chip is disabled in the
/// settings (`[simulation] auto_chip`), a tournament round is active (the
/// round policy turns Auto-chip off) or the simulation environment is
/// invalid, 400 for any other distance; all with a JSON `{"error"}` body.
pub async fn post_auto_chip(
    State(state): State<Arc<WebState>>,
    Json(body): Json<AutoChipRequest>,
) -> Result<Json<AutoChipResponse>, SettingsError> {
    if !state.root.system.simulation().auto_chip {
        return Err(settings_error(
            StatusCode::CONFLICT,
            "Auto-Chip is disabled in settings",
        ));
    }
    let yards = body.yards;
    if !flighthook::AUTO_CHIP_YARDS.contains(&yards) {
        let allowed: Vec<String> = flighthook::AUTO_CHIP_YARDS
            .iter()
            .map(ToString::to_string)
            .collect();
        return Err(settings_error(
            StatusCode::BAD_REQUEST,
            &format!("yards must be one of {}", allowed.join(", ")),
        ));
    }
    let chip_state = Arc::clone(&state);
    let sent = tokio::task::spawn_blocking(move || {
        crate::autochip::send(
            &chip_state.root,
            &chip_state.bus_tx,
            yards,
            &chip_state.auto_chip_shots,
        )
    })
    .await
    .map_err(|_| settings_error(StatusCode::INTERNAL_SERVER_ERROR, "auto-chip failed"))?;
    match sent {
        Ok((key, ball)) => Ok(Json(AutoChipResponse {
            actor: flighthook::AUTO_CHIP_ACTOR.to_owned(),
            shot_id: key.shot_id,
            shot_number: key.shot_number,
            ball_speed_mph: ball.launch_speed.map_or(0.0, |v| v.as_mph()),
            launch_elevation: ball.launch_elevation.unwrap_or_default(),
            backspin_rpm: ball.backspin_rpm.unwrap_or_default(),
        })),
        Err(e) => Err(settings_error(StatusCode::CONFLICT, &e.to_string())),
    }
}

/// How long a command to a local actor (`practice.new_block`) waits for its
/// `bus.reply`.
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Send the command `body(ref)` as the flighthook actor and wait up to
/// `wait` for the `bus.reply` to it. `None` when none came.
pub(crate) async fn command_as_flighthook(
    state: &WebState,
    body: impl FnOnce(String) -> flightrelay::FrpBody,
    wait: Duration,
) -> Option<flightrelay::BusReply> {
    let r#ref = format!("web-{}", crate::state::config::generate_id());
    let me = state.root.flighthook_id();
    let mut rx = state.bus_tx.subscribe();
    let _ = state.bus_tx.send(BusMsg::frp(FrpMessage::new(
        me.clone(),
        body(r#ref.clone()),
    )));
    let reply = async {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if let Some(frp) = msg.as_frp()
                        && let flightrelay::FrpBody::BusReply(reply) = &frp.body
                        && reply.to == me
                        && reply.r#ref == r#ref
                    {
                        return Some(reply.clone());
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    };
    tokio::time::timeout(wait, reply).await.ok().flatten()
}
