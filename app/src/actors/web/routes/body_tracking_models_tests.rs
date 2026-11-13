//! Tests of the body tracking model routes.

use super::super::tests::test_state;
use super::*;
use flighthook::{BodyTrackingModels, ModelFilesState};

async fn read(resp: Response) -> (StatusCode, BodyTrackingModels) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("models json"))
}

async fn get(state: &Arc<WebState>) -> BodyTrackingModels {
    read(get_body_tracking_models(State(Arc::clone(state))).await)
        .await
        .1
}

async fn settle(state: &Arc<WebState>) -> BodyTrackingModels {
    for _ in 0..500 {
        let m = get(state).await;
        if m.state != ModelFilesState::Downloading {
            return m;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the download did not end");
}

#[tokio::test]
async fn the_models_download_only_when_posted_and_stop_when_deleted() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let models_dir = state.root.pose.models.dir();
    // Missing, nothing downloading, nothing written.
    let m = get(&state).await;
    assert_eq!(m.state, ModelFilesState::Missing);
    assert_eq!(m.model, flighthook::PoseModel::RtmposeM);
    assert_eq!(m.files.len(), 2);
    assert!(m.download.is_none() && m.error.is_none());
    assert_eq!(m.dir, models_dir.display().to_string());
    assert!(!models_dir.exists());
    // Started; asking again starts nothing new.
    let (code, m) = read(post_body_tracking_models_download(State(Arc::clone(&state))).await).await;
    assert_eq!(code, StatusCode::ACCEPTED);
    assert_eq!(m.state, ModelFilesState::Downloading);
    let (code, again) =
        read(post_body_tracking_models_download(State(Arc::clone(&state))).await).await;
    assert_eq!(code, StatusCode::ACCEPTED);
    assert_eq!(again.state, ModelFilesState::Downloading);
    // Canceled: missing at once, the partial file goes.
    let (code, m) =
        read(delete_body_tracking_models_download(State(Arc::clone(&state))).await).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(m.state, ModelFilesState::Missing);
    assert!(m.error.is_none());
    for _ in 0..200 {
        let parts = std::fs::read_dir(&models_dir)
            .map(|d| d.count())
            .unwrap_or(0);
        if parts == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::read_dir(&models_dir)
            .map(|d| d.count())
            .unwrap_or(0),
        0,
        "no partial file left"
    );
    // Canceling with nothing running is fine.
    let (code, _) =
        read(delete_body_tracking_models_download(State(Arc::clone(&state))).await).await;
    assert_eq!(code, StatusCode::OK);
    // Downloaded to the end: ready, and a POST then has nothing to do.
    read(post_body_tracking_models_download(State(Arc::clone(&state))).await).await;
    let m = settle(&state).await;
    assert_eq!(m.state, ModelFilesState::Ready);
    assert!(m.files.iter().all(|f| f.present));
    let (code, m) = read(post_body_tracking_models_download(State(Arc::clone(&state))).await).await;
    assert_eq!((code, m.state), (StatusCode::OK, ModelFilesState::Ready));
    let _ = std::fs::remove_dir_all(&models_dir);
}

#[tokio::test]
async fn the_state_follows_the_configured_model() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    state
        .root
        .system
        .update(|c| c.body_tracking.model = flighthook::PoseModel::RtmposeS);
    let m = get(&state).await;
    assert_eq!(m.model, flighthook::PoseModel::RtmposeS);
    assert_eq!(m.files[0].name, "rtmpose-s-halpe26.onnx");
    assert_eq!(m.files[1].host, "github.com");
}
