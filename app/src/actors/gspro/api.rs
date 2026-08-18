//! GSPro Open Connect V1 JSON types.

use serde::{Deserialize, Serialize};

/// Top-level outbound message to GSPro.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct GsProMessage {
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    pub units: String,
    pub shot_number: u32,
    #[serde(rename = "APIversion")]
    pub api_version: String,
    pub ball_data: BallData,
    pub club_data: ClubData,
    pub shot_data_options: ShotDataOptions,
}

/// Ball launch and flight data.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
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
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
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
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ShotDataOptions {
    pub contains_ball_data: bool,
    pub contains_club_data: bool,
    pub launch_monitor_is_ready: bool,
    pub launch_monitor_ball_detected: bool,
    pub is_heart_beat: bool,
}

/// Inbound response from GSPro.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GsProResponse {
    pub code: i32,
    pub message: String,
    pub player: Option<PlayerInfo>,
}

/// Player info from GSPro response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlayerInfo {
    pub handed: Option<String>,
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
