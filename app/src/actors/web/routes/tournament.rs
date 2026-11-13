//! `POST /api/tournament`: switch tournament mode.

use super::*;

/// POST /api/tournament — switch tournament mode on or off
/// (`[simulation] tournament`, persisted). Never refused: while it is on,
/// Auto-chip is off and the virtual launch monitors hold their shots. A
/// simulator's round is untouched either way. Answers the round as it is
/// once the system actor has applied it; `504` when it does not within
/// [`CONFIG_TIMEOUT`].
pub async fn post_tournament(
    State(state): State<Arc<WebState>>,
    Json(body): Json<flighthook::TournamentRequest>,
) -> Result<Json<flighthook::RoundState>, StatusCode> {
    set_tournament(&state, body, CONFIG_TIMEOUT).await
}

/// Switch tournament mode to `body.on`, waiting up to `timeout` for the
/// system actor.
pub(super) async fn set_tournament(
    state: &WebState,
    body: flighthook::TournamentRequest,
    timeout: Duration,
) -> Result<Json<flighthook::RoundState>, StatusCode> {
    let action = ConfigAction::SetTournament { on: body.on };
    match send_config_command_within(state, action, timeout).await {
        Some(_) => Ok(Json(state.root.round.state())),
        None => {
            tracing::warn!("tournament: timed out waiting for ConfigOutcome");
            Err(StatusCode::GATEWAY_TIMEOUT)
        }
    }
}
