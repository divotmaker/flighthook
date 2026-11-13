//! Practice block routes: `GET /api/blocks` and `POST /api/blocks`.

use super::*;

/// GET /api/blocks — every practice block this run, open and closed, with
/// the stored shots in each.
pub async fn get_blocks(State(state): State<Arc<WebState>>) -> Json<flighthook::BlocksResponse> {
    let blocks = state.blocks.read().await;
    Json(flighthook::BlocksResponse {
        session: blocks.session(),
        blocks: blocks
            .all()
            .iter()
            .map(|r| flighthook::BlockInfo {
                block: r.block.clone(),
                open: r.open,
                shots: r.shots,
                first_shot_at: r.first_shot_at,
            })
            .collect(),
    })
}

/// POST /api/blocks — `practice.new_block` from flighthook: 200 once the
/// practice actor answered, 400 with its error (or `bad_request` for a body
/// that is no request), 504 when it did not answer.
pub async fn post_blocks(State(state): State<Arc<WebState>>, body: axum::body::Bytes) -> Response {
    let Ok(body) = serde_json::from_slice::<flighthook::NewBlockRequest>(&body) else {
        return settings_error(StatusCode::BAD_REQUEST, "bad_request").into_response();
    };
    post_blocks_within(&state, body, REPLY_TIMEOUT).await
}

pub(crate) async fn post_blocks_within(
    state: &WebState,
    body: flighthook::NewBlockRequest,
    wait: Duration,
) -> Response {
    let command = |r#ref| {
        flightrelay::FrpBody::from(flightrelay::PracticeNewBlock {
            r#ref,
            golfer: body.golfer.map(Some),
            new_session: body.new_session,
        })
    };
    reply_response(command_as_flighthook(state, command, wait).await)
}
