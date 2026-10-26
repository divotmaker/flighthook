//! Global state types — player info and club selection.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Handedness, ShotDetectionMode};

/// Golf club.
///
/// Variants serialize to GSPro wire codes (`"DR"`, `"7I"`, etc.) via serde rename.
/// `Display` returns the same code. `from_code()` parses case-insensitively.
/// `mode()` maps to the appropriate `ShotDetectionMode`. Ordering is bag
/// order, as in [`Club::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Club {
    #[serde(rename = "DR")]
    Driver,
    #[serde(rename = "MD")]
    MiniDriver,
    #[serde(rename = "2W")]
    Wood2,
    #[serde(rename = "3W")]
    Wood3,
    #[serde(rename = "4W")]
    Wood4,
    #[serde(rename = "5W")]
    Wood5,
    #[serde(rename = "6W")]
    Wood6,
    #[serde(rename = "7W")]
    Wood7,
    #[serde(rename = "8W")]
    Wood8,
    #[serde(rename = "9W")]
    Wood9,
    #[serde(rename = "1H")]
    Hybrid1,
    #[serde(rename = "2H")]
    Hybrid2,
    #[serde(rename = "3H")]
    Hybrid3,
    #[serde(rename = "4H")]
    Hybrid4,
    #[serde(rename = "5H")]
    Hybrid5,
    #[serde(rename = "6H")]
    Hybrid6,
    #[serde(rename = "7H")]
    Hybrid7,
    #[serde(rename = "8H")]
    Hybrid8,
    #[serde(rename = "9H")]
    Hybrid9,
    #[serde(rename = "1I")]
    Iron1,
    #[serde(rename = "2I")]
    Iron2,
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
        Club::MiniDriver,
        Club::Wood2,
        Club::Wood3,
        Club::Wood4,
        Club::Wood5,
        Club::Wood6,
        Club::Wood7,
        Club::Wood8,
        Club::Wood9,
        Club::Hybrid1,
        Club::Hybrid2,
        Club::Hybrid3,
        Club::Hybrid4,
        Club::Hybrid5,
        Club::Hybrid6,
        Club::Hybrid7,
        Club::Hybrid8,
        Club::Hybrid9,
        Club::Iron1,
        Club::Iron2,
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

    /// Parse a club code case-insensitively, ignoring surrounding
    /// whitespace. Returns `None` for unknown codes.
    ///
    /// Accepts both digit-first (`"7I"`) and letter-first (`"I7"`) forms for
    /// numbered clubs — GSPro uses letter-first (e.g. `"W2"`, `"H6"`, `"I1"`)
    /// on the wire. Hybrids also accept the `nHY` form (e.g. `"1HY"`).
    /// The driver also accepts `"D"`, `"DRIVER"`, `"1W"` and `"W1"`; the mini
    /// driver accepts `"MD"`, `"MINI"`, `"MINIDRIVER"` and `"MINI DRIVER"`.
    /// Wedges also accept loft-based codes (46/48 → PW, 50/52 → GW, 54/56 →
    /// SW, 58/60/62/64 → LW), a trailing `A`/`AW`/`UW` for the gap wedge, `S`
    /// for the sand wedge, and `L`/`ULW` for the lob wedge. A Trackman-style
    /// loft code with a trailing `W` (e.g. `"52W"`) parses the same as the
    /// bare loft. The putter accepts `"PT"`, `"P"` and `"PUTTER"`.
    pub fn from_code(s: &str) -> Option<Club> {
        let upper = s.trim().to_uppercase();
        let bytes = upper.as_bytes();
        // Trackman-style loft abbreviation: two digits plus a trailing `W`
        // (e.g. "52W") parses the same as the bare loft.
        let code = if bytes.len() == 3
            && bytes[2] == b'W'
            && bytes[0].is_ascii_digit()
            && bytes[1].is_ascii_digit()
        {
            &upper[..2]
        } else {
            upper.as_str()
        };
        match code {
            "DR" | "D" | "DRIVER" | "1W" | "W1" => Some(Club::Driver),
            "MD" | "MINI" | "MINIDRIVER" | "MINI DRIVER" => Some(Club::MiniDriver),
            "2W" | "W2" => Some(Club::Wood2),
            "3W" | "W3" => Some(Club::Wood3),
            "4W" | "W4" => Some(Club::Wood4),
            "5W" | "W5" => Some(Club::Wood5),
            "6W" | "W6" => Some(Club::Wood6),
            "7W" | "W7" => Some(Club::Wood7),
            "8W" | "W8" => Some(Club::Wood8),
            "9W" | "W9" => Some(Club::Wood9),
            "1H" | "H1" | "1HY" => Some(Club::Hybrid1),
            "2H" | "H2" | "2HY" => Some(Club::Hybrid2),
            "3H" | "H3" | "3HY" => Some(Club::Hybrid3),
            "4H" | "H4" | "4HY" => Some(Club::Hybrid4),
            "5H" | "H5" | "5HY" => Some(Club::Hybrid5),
            "6H" | "H6" | "6HY" => Some(Club::Hybrid6),
            "7H" | "H7" | "7HY" => Some(Club::Hybrid7),
            "8H" | "H8" | "8HY" => Some(Club::Hybrid8),
            "9H" | "H9" | "9HY" => Some(Club::Hybrid9),
            "1I" | "I1" => Some(Club::Iron1),
            "2I" | "I2" => Some(Club::Iron2),
            "3I" | "I3" => Some(Club::Iron3),
            "4I" | "I4" => Some(Club::Iron4),
            "5I" | "I5" => Some(Club::Iron5),
            "6I" | "I6" => Some(Club::Iron6),
            "7I" | "I7" => Some(Club::Iron7),
            "8I" | "I8" => Some(Club::Iron8),
            "9I" | "I9" => Some(Club::Iron9),
            "PW" | "46" | "48" => Some(Club::PitchingWedge),
            "GW" | "AW" | "UW" | "A" | "50" | "52" => Some(Club::GapWedge),
            "SW" | "S" | "54" | "56" => Some(Club::SandWedge),
            "LW" | "L" | "ULW" | "58" | "60" | "62" | "64" => Some(Club::LobWedge),
            "PT" | "P" | "PUTTER" => Some(Club::Putter),
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
            Club::MiniDriver => "MD",
            Club::Wood2 => "2W",
            Club::Wood3 => "3W",
            Club::Wood4 => "4W",
            Club::Wood5 => "5W",
            Club::Wood6 => "6W",
            Club::Wood7 => "7W",
            Club::Wood8 => "8W",
            Club::Wood9 => "9W",
            Club::Hybrid1 => "1H",
            Club::Hybrid2 => "2H",
            Club::Hybrid3 => "3H",
            Club::Hybrid4 => "4H",
            Club::Hybrid5 => "5H",
            Club::Hybrid6 => "6H",
            Club::Hybrid7 => "7H",
            Club::Hybrid8 => "8H",
            Club::Hybrid9 => "9H",
            Club::Iron1 => "1I",
            Club::Iron2 => "2I",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_round_trips_both_forms() {
        for &club in Club::ALL {
            let code = club.to_string();
            assert_eq!(Club::from_code(&code), Some(club), "{code}");
            assert_eq!(Club::from_code(&code.to_lowercase()), Some(club), "{code}");
            let json = serde_json::to_string(&club).unwrap();
            assert_eq!(json, format!("\"{code}\""));
            assert_eq!(serde_json::from_str::<Club>(&json).unwrap(), club);

            let b = code.as_bytes();
            if b[0].is_ascii_digit() {
                let flipped = format!("{}{}", b[1] as char, b[0] as char);
                assert_eq!(Club::from_code(&flipped), Some(club), "{flipped}");
            }
        }
    }

    #[test]
    fn full_bag_codes_in_order() {
        let codes: Vec<String> = Club::ALL.iter().map(ToString::to_string).collect();
        assert_eq!(
            codes,
            [
                "DR", "MD", "2W", "3W", "4W", "5W", "6W", "7W", "8W", "9W", "1H", "2H", "3H", "4H",
                "5H", "6H", "7H", "8H", "9H", "1I", "2I", "3I", "4I", "5I", "6I", "7I", "8I", "9I",
                "PW", "GW", "SW", "LW", "PT",
            ]
        );
    }

    #[test]
    fn all_is_in_ord_order() {
        let mut sorted = Club::ALL.to_vec();
        sorted.sort();
        assert_eq!(sorted, Club::ALL);
        assert!(Club::ALL.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn gspro_letter_first_codes_parse() {
        let cases = [
            ("W2", Club::Wood2),
            ("W4", Club::Wood4),
            ("W6", Club::Wood6),
            ("W9", Club::Wood9),
            ("H2", Club::Hybrid2),
            ("H6", Club::Hybrid6),
            ("H7", Club::Hybrid7),
            ("I1", Club::Iron1),
            ("I2", Club::Iron2),
        ];
        for (code, club) in cases {
            assert_eq!(Club::from_code(code), Some(club), "{code}");
        }
        assert_eq!(Club::from_code("I0"), None);
        assert_eq!(Club::from_code("W0"), None);
        assert_eq!(Club::from_code("H0"), None);
    }

    #[test]
    fn driver_aliases() {
        for code in ["D", "DRIVER", "1W", "W1", "d", "driver"] {
            assert_eq!(Club::from_code(code), Some(Club::Driver), "{code}");
        }
    }

    #[test]
    fn mini_driver_aliases() {
        for code in ["MD", "MINI", "MINIDRIVER", "MINI DRIVER", "mini driver"] {
            assert_eq!(Club::from_code(code), Some(Club::MiniDriver), "{code}");
        }
    }

    #[test]
    fn hybrid_hy_aliases() {
        let hybrids = &Club::ALL[10..19];
        for (n, club) in (1..=9).zip(hybrids) {
            assert_eq!(Club::from_code(&format!("{n}HY")), Some(*club));
            assert_eq!(Club::from_code(&format!("{n}hy")), Some(*club));
            assert_eq!(Club::from_code(&format!("{n}H")), Some(*club));
            assert_eq!(Club::from_code(&format!("H{n}")), Some(*club));
        }
    }

    #[test]
    fn numbered_wood_and_iron_codes_parse_both_ways() {
        let woods = &Club::ALL[2..10];
        for (n, club) in (2..=9).zip(woods) {
            assert_eq!(Club::from_code(&format!("{n}W")), Some(*club));
            assert_eq!(Club::from_code(&format!("W{n}")), Some(*club));
        }
        let irons = &Club::ALL[19..28];
        for (n, club) in (1..=9).zip(irons) {
            assert_eq!(Club::from_code(&format!("{n}I")), Some(*club));
            assert_eq!(Club::from_code(&format!("I{n}")), Some(*club));
        }
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(Club::from_code(" 7I"), Some(Club::Iron7));
        assert_eq!(Club::from_code("3hy\n"), Some(Club::Hybrid3));
        assert_eq!(Club::from_code("  mini driver "), Some(Club::MiniDriver));
        assert_eq!(Club::from_code("   "), None);
    }

    #[test]
    fn wedge_loft_aliases() {
        assert_eq!(Club::from_code("46"), Some(Club::PitchingWedge));
        assert_eq!(Club::from_code("48"), Some(Club::PitchingWedge));
        assert_eq!(Club::from_code("GW"), Some(Club::GapWedge));
        assert_eq!(Club::from_code("AW"), Some(Club::GapWedge));
        assert_eq!(Club::from_code("UW"), Some(Club::GapWedge));
        assert_eq!(Club::from_code("A"), Some(Club::GapWedge));
        assert_eq!(Club::from_code("50"), Some(Club::GapWedge));
        assert_eq!(Club::from_code("52"), Some(Club::GapWedge));
        assert_eq!(Club::from_code("S"), Some(Club::SandWedge));
        assert_eq!(Club::from_code("54"), Some(Club::SandWedge));
        assert_eq!(Club::from_code("56"), Some(Club::SandWedge));
        assert_eq!(Club::from_code("L"), Some(Club::LobWedge));
        assert_eq!(Club::from_code("ULW"), Some(Club::LobWedge));
        assert_eq!(Club::from_code("58"), Some(Club::LobWedge));
        assert_eq!(Club::from_code("60"), Some(Club::LobWedge));
        assert_eq!(Club::from_code("62"), Some(Club::LobWedge));
        assert_eq!(Club::from_code("64"), Some(Club::LobWedge));
        assert_eq!(Club::from_code("P"), Some(Club::Putter));
        assert_eq!(Club::from_code("PUTTER"), Some(Club::Putter));
    }

    #[test]
    fn trackman_loft_w_suffix_aliases() {
        assert_eq!(Club::from_code("52W"), Some(Club::GapWedge));
        assert_eq!(Club::from_code("46W"), Some(Club::PitchingWedge));
        assert_eq!(Club::from_code("60W"), Some(Club::LobWedge));
    }

    #[test]
    fn mode_per_club() {
        for club in [
            Club::MiniDriver,
            Club::Wood2,
            Club::Wood4,
            Club::Wood6,
            Club::Wood8,
            Club::Wood9,
            Club::Hybrid1,
            Club::Hybrid2,
            Club::Hybrid6,
            Club::Hybrid7,
            Club::Hybrid8,
            Club::Hybrid9,
            Club::Iron1,
            Club::Iron2,
            Club::PitchingWedge,
        ] {
            assert_eq!(club.mode(), ShotDetectionMode::Full, "{club}");
        }
        for club in [Club::GapWedge, Club::SandWedge, Club::LobWedge] {
            assert_eq!(club.mode(), ShotDetectionMode::Chipping, "{club}");
        }
        assert_eq!(Club::Putter.mode(), ShotDetectionMode::Putting);
    }
}
