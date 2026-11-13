//! Body tracking model routes: the models' state, and starting or canceling their download.

use super::*;

/// GET /api/body_tracking/models — the state of the files the configured
/// pose model needs: `missing`, `downloading` (with the download's
/// progress), `ready` or `failed` (with why), the files, their hosts and
/// sizes, and the models folder. Checking a file hashes it the first time
/// (a few hundred ms); later calls only look at its size and time.
pub async fn get_body_tracking_models(State(state): State<Arc<WebState>>) -> Response {
    let root = Arc::clone(&state.root);
    match tokio::task::spawn_blocking(move || {
        root.pose.models.files(root.system.body_tracking().model)
    })
    .await
    {
        Ok(files) => Json(files).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/body_tracking/models/download — download the files the
/// configured pose model lacks, from their official sources: `202` with
/// the state when a download starts or is already running (one at a time;
/// asking again starts nothing), `200` with the state when every file is
/// there already. The only way flighthook downloads a model.
pub async fn post_body_tracking_models_download(State(state): State<Arc<WebState>>) -> Response {
    use crate::pose::models::DownloadStart;
    let root = Arc::clone(&state.root);
    let answered = tokio::task::spawn_blocking(move || {
        let model = root.system.body_tracking().model;
        let started = root.pose.models.download(model);
        (started, root.pose.models.files(model))
    })
    .await;
    match answered {
        Ok((DownloadStart::Ready, files)) => (StatusCode::OK, Json(files)).into_response(),
        Ok((DownloadStart::Started | DownloadStart::Running, files)) => {
            (StatusCode::ACCEPTED, Json(files)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// DELETE /api/body_tracking/models/download — stop the download running,
/// if any; its partial file is deleted. `200` with the state either way.
pub async fn delete_body_tracking_models_download(State(state): State<Arc<WebState>>) -> Response {
    let root = Arc::clone(&state.root);
    let answered = tokio::task::spawn_blocking(move || {
        root.pose.models.cancel_download();
        root.pose.models.files(root.system.body_tracking().model)
    })
    .await;
    match answered {
        Ok(files) => Json(files).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
