//! Actor routes: `POST /api/actors/{id}/reconnect` (with its in-flight guard) and
//! `POST /api/actors/{id}/forget-pairing`.

use super::*;

/// How long a reconnect still holds its actor's in-flight guard after the
/// request was answered `202`, waiting for the outcome.
pub(super) const RECONNECT_LINGER: Duration = CONFIG_TIMEOUT;

/// POST /api/actors/{id}/reconnect — stop the launch monitor or output `id`,
/// wait for its connection to close, and start it again from the saved
/// settings. Nothing else is touched.
///
/// `200` with `{"restarted": [id]}` once it has started again; `202` with
/// nothing restarted when it starts again by itself later: the reconnect is
/// still running after [`CONFIG_TIMEOUT`] (the actor stays in flight until
/// it ends), its old connection has not closed yet, or another command is
/// restarting it; `400` for a camera, the web server or any other actor that
/// is not a launch monitor or output; `404` for an ID no launch monitor or
/// output has; `409` while a reconnect of it is in flight, or when it was
/// not started again for any other reason (it was removed meanwhile).
/// Errors carry a JSON `{"error": "..."}` body.
pub async fn post_reconnect(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<PostSettingsResponse>), SettingsError> {
    reconnect(&state, &id, CONFIG_TIMEOUT).await
}

/// POST /api/actors/{id}/forget-pairing — delete Pureplay Accessory card `id`'s
/// saved pairing token, then reconnect it as
/// `POST /api/actors/{id}/reconnect` does, so it asks Pureplay to pair again
/// with a prompt. Answers like the reconnect route; `404` for an ID no
/// Pureplay Accessory card has (another type included), `409` while a reconnect of the
/// card is in progress (the token is kept), `500` when the token file cannot
/// be written. Pureplay keeps its old entry until the player removes it in
/// Settings > Accessories.
pub async fn post_forget_pairing(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<PostSettingsResponse>), SettingsError> {
    forget_pairing(&state, &id, CONFIG_TIMEOUT).await
}

/// Forget Pureplay Accessory card `id`'s pairing and reconnect it, waiting up to
/// `timeout` for the system actor.
pub(super) async fn forget_pairing(
    state: &Arc<WebState>,
    id: &str,
    timeout: Duration,
) -> Result<(StatusCode, Json<PostSettingsResponse>), SettingsError> {
    use crate::actors::pureplay_accessory::{DEFAULT_ADDRESS, token_key};
    let id = crate::state::config::canonical_id(id);
    let config = state.root.system.snapshot();
    let section = id
        .split_once('.')
        .filter(|(prefix, _)| *prefix == "pureplay_accessory")
        .and_then(|(_, index)| config.pureplay_accessory.get(index));
    let Some(section) = section else {
        return Err(settings_error(
            StatusCode::NOT_FOUND,
            &format!("no Pureplay Accessory card '{id}'"),
        ));
    };
    let address = section.address.as_deref().unwrap_or(DEFAULT_ADDRESS);
    let key = address
        .parse()
        .map_or_else(|_| flighthook::address_key(address), token_key);
    // The guard first: a reconnect already under way keeps the token.
    let (id, in_flight) = begin_reconnect(state, &id)?;
    let tokens = Arc::clone(&state.root.pureplay_tokens);
    let forgotten = tokio::task::spawn_blocking(move || tokens.forget(&key))
        .await
        .unwrap_or_else(|e| Err(std::io::Error::other(e)));
    match forgotten {
        Ok(had) => tracing::info!(
            "{id}: pairing {}",
            if had { "forgotten" } else { "was not saved" }
        ),
        Err(e) => {
            return Err(settings_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("couldn't delete the Pureplay pairing: {e}"),
            ));
        }
    }
    run_reconnect(state, id, in_flight, timeout).await
}

/// Reconnect actor `id`, waiting up to `timeout` for the system actor.
pub(super) async fn reconnect(
    state: &Arc<WebState>,
    id: &str,
    timeout: Duration,
) -> Result<(StatusCode, Json<PostSettingsResponse>), SettingsError> {
    let (id, in_flight) = begin_reconnect(state, id)?;
    run_reconnect(state, id, in_flight, timeout).await
}

/// Check that actor `id` can be reconnected and mark it in flight: its
/// canonical ID and the guard. `400` / `404` from the target check, `409`
/// while a reconnect of it is in progress.
pub(super) fn begin_reconnect(
    state: &Arc<WebState>,
    id: &str,
) -> Result<(String, InFlight), SettingsError> {
    use crate::actors::system::{ReconnectRefusal, reconnect_target};
    let id = match reconnect_target(&state.root.system.snapshot(), id) {
        Ok(id) => id,
        Err(r @ ReconnectRefusal::NotReconnectable(_)) => {
            return Err(settings_error(StatusCode::BAD_REQUEST, r.message()));
        }
        Err(r @ ReconnectRefusal::NotFound(_)) => {
            return Err(settings_error(StatusCode::NOT_FOUND, r.message()));
        }
    };
    let Some(in_flight) = InFlight::begin(state, &id) else {
        return Err(settings_error(
            StatusCode::CONFLICT,
            &format!("a reconnect of '{id}' is already in progress"),
        ));
    };
    Ok((id, in_flight))
}

/// Reconnect actor `id`, marked in flight by `in_flight`, waiting up to
/// `timeout` for the system actor.
pub(super) async fn run_reconnect(
    state: &Arc<WebState>,
    id: String,
    in_flight: InFlight,
    timeout: Duration,
) -> Result<(StatusCode, Json<PostSettingsResponse>), SettingsError> {
    use crate::actors::system::not_reconnected;
    let mut outcome = Box::pin(send_config_command(
        state,
        ConfigAction::Reconnect { id: id.clone() },
    ));
    match tokio::time::timeout(timeout, &mut outcome).await {
        Ok(Some(Outcome { restarted, stopped })) if restarted.contains(&id) => Ok((
            StatusCode::OK,
            Json(PostSettingsResponse {
                restarted,
                stopped,
                ..PostSettingsResponse::default()
            }),
        )),
        // It starts again by itself once its old connection has closed,
        // or the command restarting it is done.
        Ok(Some(_)) if state.root.is_closing(&id) || state.root.is_settling(&id) => {
            Ok((StatusCode::ACCEPTED, Json(PostSettingsResponse::default())))
        }
        Ok(Some(_)) => Err(settings_error(
            StatusCode::CONFLICT,
            &not_reconnected(&state.root, &id),
        )),
        Ok(None) => Err(settings_error(
            StatusCode::GATEWAY_TIMEOUT,
            "the reconnect did not finish",
        )),
        Err(_) => {
            tracing::warn!("reconnect '{id}': still running, answered 202");
            // Still queued or running: the guard holds until it ends, so a
            // second request cannot restart the actor again behind it.
            tokio::spawn(async move {
                let _ = tokio::time::timeout(RECONNECT_LINGER, outcome).await;
                drop(in_flight);
            });
            Ok((StatusCode::ACCEPTED, Json(PostSettingsResponse::default())))
        }
    }
}

/// A reconnect in flight: its actor ID is in the web state's set while this
/// lives.
pub(super) struct InFlight {
    pub(super) state: Arc<WebState>,
    pub(super) id: String,
}

impl InFlight {
    /// Mark `id` in flight; `None` when it already is.
    pub(super) fn begin(state: &Arc<WebState>, id: &str) -> Option<Self> {
        state
            .reconnecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_owned())
            .then(|| Self {
                state: Arc::clone(state),
                id: id.to_owned(),
            })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.state
            .reconnecting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}
