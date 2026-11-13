//! Camera routes: `GET /api/cameras`, a camera's still frame and its live view (with the pose headers).

use super::*;

/// GET /api/cameras — the cameras this computer has, and the test picture.
///
/// Enumerates on a blocking thread, at most every few seconds (the result is
/// reused meanwhile). A camera a camera actor records from is not opened
/// again to list it and is marked `in_use_by`. `500` with a text body when
/// the enumeration thread fails.
pub async fn get_cameras(State(state): State<Arc<WebState>>) -> Response {
    let root = Arc::clone(&state.root);
    match tokio::task::spawn_blocking(move || root.cameras.list()).await {
        Ok(list) => Json::<CameraListResponse>(list).into_response(),
        Err(e) => {
            tracing::warn!("cameras: listing failed: {e}");
            text_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("listing the cameras failed: {e}"),
            )
        }
    }
}

/// A response with status `status` and a short text body.
pub(super) fn text_response(status: StatusCode, text: String) -> Response {
    Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(Body::from(text))
        .unwrap()
}

/// A `404` with a short text body.
pub(super) fn not_found(text: String) -> Response {
    text_response(StatusCode::NOT_FOUND, text)
}

/// GET /api/cameras/{actor}/still — the camera actor's newest frame, a JPEG
/// as the camera sent it (at most half a second old while it records).
/// `404` when the actor has no frame.
pub async fn get_camera_still(
    State(state): State<Arc<WebState>>,
    Path(actor): Path<String>,
) -> Response {
    let Some(jpeg) = state.root.cameras.still(&actor) else {
        return not_found(format!("no frame from '{actor}'"));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"))
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .body(Body::from(jpeg.to_vec()))
        .unwrap()
}

/// Query of `GET /api/cameras/{actor}/live`.
#[derive(Debug, Deserialize)]
pub struct LiveQuery {
    /// The sequence number of the frame the viewer has; 0 for none.
    #[serde(default)]
    pub(super) after: u64,
}

/// GET /api/cameras/{actor}/live?after=<seq> — the camera's live frame
/// newer than `after`, a JPEG as the camera sent it, with its sequence
/// number (`X-Frame-Seq`) and time (`X-Frame-Time-Ns`, see
/// [`LiveFrame::time_ns`](crate::state::cameras::LiveFrame)), and the
/// golfer's keypoints on that frame (`X-Pose`, compact JSON) when body
/// tracking ran on it. Waits up to
/// [`LIVE_WAIT`](crate::state::cameras::LIVE_WAIT) for one, then `204`.
/// Both carry body tracking's state (`X-Pose-State`: `off`, `loading`,
/// `downloading`, `ready` or `failed`, the failure in `X-Pose-Error`).
/// Each request keeps the camera's live slot updated for the next
/// [`LIVE_WATCH`](crate::state::cameras::LIVE_WATCH). `404` when `actor` is
/// not a configured camera; `503` when the camera already has
/// [`LIVE_WAITERS`](crate::state::cameras::LIVE_WAITERS) requests waiting.
pub async fn get_camera_live(
    State(state): State<Arc<WebState>>,
    Path(actor): Path<String>,
    Query(query): Query<LiveQuery>,
) -> Response {
    get_camera_live_with(
        &state,
        &actor,
        query.after,
        crate::state::cameras::LIVE_WAIT,
    )
    .await
}

/// [`get_camera_live`] waiting at most `wait`.
pub(super) async fn get_camera_live_with(
    state: &WebState,
    actor: &str,
    after: u64,
    wait: Duration,
) -> Response {
    use crate::state::cameras::LiveWait;
    let configured = actor
        .strip_prefix("camera.")
        .is_some_and(|index| state.root.system.camera(index).is_some());
    if !configured {
        return not_found(format!("no camera '{actor}'"));
    }
    let cameras = &state.root.cameras;
    cameras.watch_live(actor, std::time::Instant::now());
    let answer = cameras.next_live(actor, after, wait).await;
    let pose_state = || {
        let settings = state.root.system.body_tracking();
        state.root.pose.live_state(&settings, actor)
    };
    match answer {
        LiveWait::Frame(frame) => {
            let mut builder = Response::builder()
                .header(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"))
                .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
                .header("X-Frame-Seq", frame.seq)
                .header("X-Frame-Time-Ns", frame.time_ns);
            if let Some(pose) = frame.pose.as_deref() {
                match HeaderValue::from_str(pose) {
                    Ok(value) => builder = builder.header("X-Pose", value),
                    Err(_) => {
                        tracing::debug!("{actor}: live pose left out, not a valid header: {pose}")
                    }
                }
            }
            pose_headers(builder, pose_state())
                .body(Body::from(frame.jpeg.to_vec()))
                .unwrap()
        }
        LiveWait::Timeout => {
            let builder = Response::builder()
                .status(StatusCode::NO_CONTENT)
                .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            pose_headers(builder, pose_state())
                .body(Body::empty())
                .unwrap()
        }
        LiveWait::Busy => text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("too many live requests waiting on '{actor}'"),
        ),
    }
}

/// Add body tracking's `state` (`X-Pose-State`) and its error, if any
/// (`X-Pose-Error`, printable ASCII, at most 300 characters).
pub(super) fn pose_headers(
    builder: axum::http::response::Builder,
    (state, error): (&'static str, Option<String>),
) -> axum::http::response::Builder {
    let builder = builder.header("X-Pose-State", state);
    let Some(error) = error else {
        return builder;
    };
    let text: String = error
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .take(300)
        .collect();
    match HeaderValue::from_str(&text) {
        Ok(value) => builder.header("X-Pose-Error", value),
        Err(_) => builder,
    }
}
