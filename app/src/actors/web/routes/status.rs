//! `GET /api/status` and the per-actor status it carries.

use super::*;

/// GET /api/status
pub async fn get_status(State(state): State<Arc<WebState>>) -> Json<StatusResponse> {
    // Names from the config as it is now: a rename shows before the bus
    // event that applied it reaches the cache.
    let names = actor_names(&state.root.system.snapshot());
    let cards = super::super::actor_statuses(&*state.actors.read().await, &names, state.now_ms());
    let actors = status_actors(&state);

    let turn = state.root.session.turn();
    let clip_dir = crate::state::clips::clip_dir(&state.root.system.replay());
    Json(StatusResponse {
        cards,
        actors,
        turn: Some(turn),
        cameras_supported: crate::state::cameras::supported(),
        trackball_supported: thumbswing::device::SUPPORTED,
        clip_dir: Some(clip_dir.display().to_string()),
        config_error: state.root.system.load_error(),
        round: state.root.round.state(),
    })
}

/// Every actor on the bus with its name, roles, blockers, device and local
/// card key.
pub(super) fn status_actors(
    state: &WebState,
) -> std::collections::HashMap<String, flighthook::StatusActor> {
    let Ok(bus) = state.bus_state.lock() else {
        return std::collections::HashMap::new();
    };
    bus.actors()
        .map(|(id, actor)| {
            (
                id.to_owned(),
                flighthook::StatusActor {
                    name: actor.name.clone(),
                    roles: actor.roles.clone(),
                    blockers: actor.blockers.clone(),
                    device: actor.device.clone(),
                    card: state.root.card_of_actor(id),
                },
            )
        })
        .collect()
}
