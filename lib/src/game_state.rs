//! Global state types — player info and club selection.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Handedness, ShotDetectionMode};

/// Golf club.
///
/// Variants serialize to GSPro wire codes (`"DR"`, `"7I"`, etc.) via serde rename.
/// `Display` returns the same code. `from_code()` parses case-insensitively.
/// `mode()` maps to the appropriate `ShotDetectionMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Club {
    #[serde(rename = "DR")]
    Driver,
    #[serde(rename = "3W")]
    Wood3,
    #[serde(rename = "5W")]
    Wood5,
    #[serde(rename = "7W")]
    Wood7,
    #[serde(rename = "3H")]
    Hybrid3,
    #[serde(rename = "4H")]
    Hybrid4,
    #[serde(rename = "5H")]
    Hybrid5,
    #[serde(rename = "3I")]
    Iron3,
    #[serde(rename = "4I")]
    Iron4,
    #[serde(rename = "5I")]
    Iron5,
    #[serde(rename = "6I")]
    Iron6,
    #[serde(rename = "7I")]
    Iron7,
    #[serde(rename = "8I")]
    Iron8,
    #[serde(rename = "9I")]
    Iron9,
    #[serde(rename = "PW")]
    PitchingWedge,
    #[serde(rename = "GW")]
    GapWedge,
    #[serde(rename = "SW")]
    SandWedge,
    #[serde(rename = "LW")]
    LobWedge,
    #[serde(rename = "PT")]
    Putter,
}

impl Club {
    /// All variants in bag order (driver through putter).
    pub const ALL: &[Club] = &[
        Club::Driver,
        Club::Wood3,
        Club::Wood5,
        Club::Wood7,
        Club::Hybrid3,
        Club::Hybrid4,
        Club::Hybrid5,
        Club::Iron3,
        Club::Iron4,
        Club::Iron5,
        Club::Iron6,
        Club::Iron7,
        Club::Iron8,
        Club::Iron9,
        Club::PitchingWedge,
        Club::GapWedge,
        Club::SandWedge,
        Club::LobWedge,
        Club::Putter,
    ];

    /// Parse a club code case-insensitively. Returns `None` for unknown codes.
    ///
    /// Accepts both digit-first (`"7I"`) and letter-first (`"I7"`) forms for
    /// numbered clubs — GSPro uses letter-first (e.g. `"I7"`) on the wire.
    /// Wedges also accept loft-based codes: 50/52 → GW, 54/56 → SW, 58/60 → LW.
    pub fn from_code(s: &str) -> Option<Club> {
        match s.to_uppercase().as_str() {
            "DR" => Some(Club::Driver),
            "3W" | "W3" => Some(Club::Wood3),
            "5W" | "W5" => Some(Club::Wood5),
            "7W" | "W7" => Some(Club::Wood7),
            "3H" | "H3" => Some(Club::Hybrid3),
            "4H" | "H4" => Some(Club::Hybrid4),
            "5H" | "H5" => Some(Club::Hybrid5),
            "3I" | "I3" => Some(Club::Iron3),
            "4I" | "I4" => Some(Club::Iron4),
            "5I" | "I5" => Some(Club::Iron5),
            "6I" | "I6" => Some(Club::Iron6),
            "7I" | "I7" => Some(Club::Iron7),
            "8I" | "I8" => Some(Club::Iron8),
            "9I" | "I9" => Some(Club::Iron9),
            "PW" => Some(Club::PitchingWedge),
            "GW" | "50" | "52" => Some(Club::GapWedge),
            "SW" | "54" | "56" => Some(Club::SandWedge),
            "LW" | "58" | "60" => Some(Club::LobWedge),
            "PT" | "P" => Some(Club::Putter),
            _ => None,
        }
    }

    /// Map this club to a shot detection mode.
    ///
    /// Putter → Putting, wedges (GW/SW/LW) → Chipping, everything else → Full.
    pub fn mode(&self) -> ShotDetectionMode {
        match self {
            Club::Putter => ShotDetectionMode::Putting,
            Club::GapWedge | Club::SandWedge | Club::LobWedge => ShotDetectionMode::Chipping,
            _ => ShotDetectionMode::Full,
        }
    }
}

impl fmt::Display for Club {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Club::Driver => "DR",
            Club::Wood3 => "3W",
            Club::Wood5 => "5W",
            Club::Wood7 => "7W",
            Club::Hybrid3 => "3H",
            Club::Hybrid4 => "4H",
            Club::Hybrid5 => "5H",
            Club::Iron3 => "3I",
            Club::Iron4 => "4I",
            Club::Iron5 => "5I",
            Club::Iron6 => "6I",
            Club::Iron7 => "7I",
            Club::Iron8 => "8I",
            Club::Iron9 => "9I",
            Club::PitchingWedge => "PW",
            Club::GapWedge => "GW",
            Club::SandWedge => "SW",
            Club::LobWedge => "LW",
            Club::Putter => "PT",
        };
        f.write_str(code)
    }
}

/// Player info (name).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Club selection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClubInfo {
    pub club: Club,
}

/// Immutable snapshot of the current global state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameStateSnapshot {
    #[serde(default)]
    pub player_info: Option<PlayerInfo>,
    #[serde(default)]
    pub club_info: Option<ClubInfo>,
    #[serde(default)]
    pub mode: Option<ShotDetectionMode>,
    #[serde(default)]
    pub handed: Option<Handedness>,
}
