//! REST API request/response types shared between the app and UI crates.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{ActorStatus, ShotDetectionMode};

/// GET /api/status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    #[serde(default)]
    pub actors: HashMap<String, ActorStatusResponse>,
    #[serde(default)]
    pub mode: Option<ShotDetectionMode>,
}

/// Per-actor status within the status response. Also used as the cached
/// per-actor state in the web layer and UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorStatusResponse {
    #[serde(default)]
    pub name: String,
    pub status: ActorStatus,
    #[serde(default)]
    pub telemetry: HashMap<String, String>,
}

/// POST /api/mode request body.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModeRequest {
    pub mode: ShotDetectionMode,
}

/// POST /api/settings response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostSettingsResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarted: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<String>,
}
