//! GSPro Open Connect V1 JSON types.
//!
//! These types are used in both directions. The `gspro` actor is a *client*:
//! it sends [`GsProMessage`] and receives [`GsProResponse`]. The
//! `openconnect` actor is a *server*: it receives [`GsProMessage`] from a
//! launch monitor and sends [`GsProResponse`] back. Every type therefore
//! derives both `Serialize` and `Deserialize`.
//!
//! Inbound messages come from third-party launch monitors that populate the
//! schema unevenly, so the optional sub-objects default rather than failing
//! the parse. Only `DeviceID` and `ShotNumber` are treated as required.

use serde::{Deserialize, Serialize};

/// Top-level Open Connect message: flighthook -> GSPro, or launch monitor ->
/// flighthook.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GsProMessage {
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(default)]
    pub units: String,
    pub shot_number: u32,
    #[serde(rename = "APIversion", default)]
    pub api_version: String,
    #[serde(default)]
    pub ball_data: BallData,
    #[serde(default)]
    pub club_data: ClubData,
    #[serde(default)]
    pub shot_data_options: ShotDataOptions,
}

/// Ball launch and flight data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct BallData {
    pub speed: f64,
    pub spin_axis: f64,
    pub total_spin: f64,
    #[serde(rename = "BackSpin")]
    pub back_spin: f64,
    #[serde(rename = "SideSpin")]
    pub side_spin: f64,
    #[serde(rename = "HLA")]
    pub hla: f64,
    #[serde(rename = "VLA")]
    pub vla: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carry_distance: Option<f64>,
}

/// Club head data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
///
/// Complete GSPro Open Connect V1 field set. Fields no launch monitor in this
/// project measures are still sent as `0.0`, matching GSPro's own example
/// payload — the schema is fixed, not sparse.
pub struct ClubData {
    pub speed: f64,
    pub angle_of_attack: f64,
    pub face_to_target: f64,
    /// Club lie angle. Not measured by any supported device.
    pub lie: f64,
    pub loft: f64,
    pub path: f64,
    /// Club speed after impact. Not measured by any supported device.
    pub speed_at_impact: f64,
    /// Face impact height, mm. Positive = above centre.
    pub vertical_face_impact: f64,
    /// Face impact lateral offset, mm. Positive = toward the toe.
    pub horizontal_face_impact: f64,
    /// Face closure rate. Not measured by any supported device.
    pub closure_rate: f64,
}

impl Default for ClubData {
    fn default() -> Self {
        Self {
            speed: 0.0,
            angle_of_attack: 0.0,
            face_to_target: 0.0,
            lie: 0.0,
            loft: 0.0,
            path: 0.0,
            speed_at_impact: 0.0,
            vertical_face_impact: 0.0,
            horizontal_face_impact: 0.0,
            closure_rate: 0.0,
        }
    }
}

/// Flags controlling what GSPro should expect.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ShotDataOptions {
    pub contains_ball_data: bool,
    pub contains_club_data: bool,
    pub launch_monitor_is_ready: bool,
    pub launch_monitor_ball_detected: bool,
    pub is_heart_beat: bool,
}

/// Open Connect response: GSPro -> flighthook, or flighthook -> launch monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GsProResponse {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<PlayerInfo>,
}

impl GsProResponse {
    /// A bare `200 OK` acknowledgement. Open Connect clients wait for this
    /// after each message and will stall or disconnect without it.
    pub fn ok(message: &str) -> Self {
        Self {
            code: 200,
            message: message.into(),
            player: None,
        }
    }
}

/// Player info carried on an Open Connect response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlayerInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club: Option<String>,
}

impl GsProMessage {
    /// Construct a heartbeat with explicit readiness flags.
    pub fn heartbeat_with_readiness(ready: bool, ball_detected: bool) -> Self {
        Self {
            device_id: "Flighthook".into(),
            units: "Yards".into(),
            shot_number: 0,
            api_version: "1".into(),
            ball_data: BallData {
                speed: 0.0,
                spin_axis: 0.0,
                total_spin: 0.0,
                back_spin: 0.0,
                side_spin: 0.0,
                hla: 0.0,
                vla: 0.0,
                carry_distance: None,
            },
            club_data: ClubData::default(),
            shot_data_options: ShotDataOptions {
                contains_ball_data: false,
                contains_club_data: false,
                launch_monitor_is_ready: ready,
                launch_monitor_ball_detected: ball_detected,
                is_heart_beat: true,
            },
        }
    }
}
