//! GSPro Open Connect V1 JSON types.
//!
//! These types are used in both directions. An Open Connect *client* (the
//! [client driver](super)) sends [`GsProMessage`] and receives
//! [`GsProResponse`]; an Open Connect *server* receives [`GsProMessage`] from
//! a launch monitor and sends [`GsProResponse`] back. Every type therefore
//! derives both `Serialize` and `Deserialize`.
//!
//! Inbound messages come from third-party launch monitors that populate the
//! schema unevenly, so the optional sub-objects default rather than failing
//! the parse. Only `DeviceID` and `ShotNumber` are treated as required.

use flightrelay::{Club, Handedness};
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
    /// Face impact height, mm. Positive = above center.
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
    /// A bare `200 OK` acknowledgment. Open Connect clients wait for this
    /// after each message and will stall or disconnect without it.
    pub fn ok(message: &str) -> Self {
        Self {
            code: 200,
            message: message.into(),
            player: None,
        }
    }

    /// GSPro's player information message (`201`): the player's hand
    /// (`RH`/`LH`) and club (`DR`, `W3`, `I7`, `PT`, ...). A field that is
    /// `None` is left out.
    pub fn player(handed: Option<Handedness>, club: Option<Club>) -> Self {
        Self {
            code: 201,
            message: "GSPro Player Information".into(),
            player: Some(PlayerInfo {
                handed: handed.map(|h| hand_code(h).to_string()),
                club: club.map(|c| club_code(c).to_string()),
            }),
        }
    }
}

/// GSPro's letter-first code for `club` (`DR`, `W3`, `H4`, `I7`, `PW`,
/// `PT`). GSPro has no mini driver, so the mini driver is `DR`.
#[must_use]
pub fn club_code(club: Club) -> &'static str {
    match club {
        Club::Driver | Club::MiniDriver => "DR",
        Club::Wood2 => "W2",
        Club::Wood3 => "W3",
        Club::Wood4 => "W4",
        Club::Wood5 => "W5",
        Club::Wood6 => "W6",
        Club::Wood7 => "W7",
        Club::Wood8 => "W8",
        Club::Wood9 => "W9",
        Club::Hybrid1 => "H1",
        Club::Hybrid2 => "H2",
        Club::Hybrid3 => "H3",
        Club::Hybrid4 => "H4",
        Club::Hybrid5 => "H5",
        Club::Hybrid6 => "H6",
        Club::Hybrid7 => "H7",
        Club::Hybrid8 => "H8",
        Club::Hybrid9 => "H9",
        Club::Iron1 => "I1",
        Club::Iron2 => "I2",
        Club::Iron3 => "I3",
        Club::Iron4 => "I4",
        Club::Iron5 => "I5",
        Club::Iron6 => "I6",
        Club::Iron7 => "I7",
        Club::Iron8 => "I8",
        Club::Iron9 => "I9",
        Club::PitchingWedge => "PW",
        Club::GapWedge => "GW",
        Club::SandWedge => "SW",
        Club::LobWedge => "LW",
        Club::Putter => "PT",
        // A club code newer than this table: the driver, as for the mini
        // driver.
        _ => "DR",
    }
}

/// The club for a GSPro club code, case-insensitively and ignoring
/// surrounding whitespace: GSPro's letter-first code ([`club_code`]) or the
/// FRP code (`7I`). `None` for any other.
#[must_use]
pub fn club_from_code(code: &str) -> Option<Club> {
    let code = code.trim().to_ascii_uppercase();
    Club::ALL
        .iter()
        .copied()
        .find(|&c| c != Club::MiniDriver && club_code(c) == code)
        .or_else(|| code.parse().ok())
}

/// GSPro's code for a hand: `RH` or `LH`.
pub fn hand_code(hand: Handedness) -> &'static str {
    match hand {
        Handedness::Right => "RH",
        Handedness::Left => "LH",
    }
}

/// The hand for GSPro's code, case-insensitively; `None` for anything but
/// `RH` or `LH`.
pub fn hand_from_code(code: &str) -> Option<Handedness> {
    [Handedness::Right, Handedness::Left]
        .into_iter()
        .find(|&h| hand_code(h).eq_ignore_ascii_case(code))
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
        Self::status(ready, ball_detected, true)
    }

    /// A readiness update: not a heartbeat, no ball data and no club data,
    /// with the readiness flags set.
    pub fn readiness(ready: bool, ball_detected: bool) -> Self {
        Self::status(ready, ball_detected, false)
    }

    /// A message with no data that carries only the readiness flags.
    fn status(ready: bool, ball_detected: bool, heartbeat: bool) -> Self {
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
                is_heart_beat: heartbeat,
            },
        }
    }
}
