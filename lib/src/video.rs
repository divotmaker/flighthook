//! Swing replay types shared by the app and the UI: a shot's camera clips
//! and the camera list.

use serde::{Deserialize, Serialize};

use crate::CameraPosition;

/// One camera's clip of a shot (beta), as `/api/shots` rows carry it in
/// `video` and `GET /api/shots/{shot_id}/video` lists it.
///
/// The clip itself is served at `/api/shots/{shot_id}/video/{camera}.avi`
/// (MJPEG AVI) with its frame index at `/api/shots/{shot_id}/video/{camera}.json`
/// and, once [`pose`](Self::pose) is set, the golfer's keypoints at
/// `/api/shots/{shot_id}/video/{camera}.pose.json`; once
/// [`swing`](Self::swing) is set, the swing analysis at
/// `/api/shots/{shot_id}/video/{camera}.swing.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShotVideo {
    /// The camera's actor ID (`camera.0`).
    pub camera: String,
    /// Where the camera stands.
    pub position: CameraPosition,
    /// Time from the clip's first frame to its last, ms.
    pub duration_ms: u64,
    /// Frames per second captured over the clip.
    pub fps: f64,
    /// When the launch monitor reported the shot, ms after the first frame.
    pub trigger_offset_ms: i64,
    /// The estimated impact, ms after the first frame (negative when it
    /// precedes the clip).
    pub impact_offset_ms: i64,
    /// The whole clip was captured with no frames missing at the trigger or
    /// the impact.
    pub complete: bool,
    /// Frames missing inside the clip.
    pub dropped: u32,
    /// Body tracking of the clip is done (alpha): its keypoints are served
    /// at `/api/shots/{shot_id}/video/{camera}.pose.json`. Omitted while
    /// false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pose: bool,
    /// The swing analysis of the clip is done (alpha): its events, tempo
    /// and metrics are served at
    /// `/api/shots/{shot_id}/video/{camera}.swing.json`. Omitted while
    /// false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub swing: bool,
}

/// `GET /api/cameras`: the cameras this computer has.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CameraListResponse {
    /// Whether the server can record from cameras on this platform. When
    /// false, `cameras` holds the test picture only.
    pub supported: bool,
    /// Every camera found, then the test picture.
    #[serde(default)]
    pub cameras: Vec<CameraListEntry>,
}

/// A camera in [`CameraListResponse`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CameraListEntry {
    /// Stable id, what `[camera.N] camera` holds.
    pub id: String,
    /// The camera's own name, for display.
    pub name: String,
    /// USB vendor and product id (`046d:085e`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usb: Option<String>,
    /// Its MJPEG video modes, `WIDTHxHEIGHT@FPS`, fastest first.
    #[serde(default)]
    pub modes: Vec<String>,
    /// The camera actor recording from it, if one is (`camera.0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use_by: Option<String>,
    /// Why its modes could not be read (in use by another application).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
