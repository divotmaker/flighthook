//! Clubs and handedness: flighthook's helpers on the FRP types.

use serde::{Deserialize, Serialize};

pub use flightrelay::Club;

use crate::Handedness;

/// flighthook's helpers on [`Club`] (FRP's club codes, `"DR"`, `"7I"`, ...;
/// `Display` gives the code, ordering is bag order as in [`Club::ALL`]).
pub trait ClubExt: Sized {
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
    fn from_code(s: &str) -> Option<Club>;

    /// GSPro's letter-first code for this club (`DR`, `W3`, `H4`, `I7`, `PW`,
    /// `PT`), the form [`ClubExt::from_code`] accepts back. GSPro has no mini
    /// driver, so `MiniDriver` is sent as `DR`.
    fn gspro_code(self) -> &'static str;

    /// The class of this club the swing analysis compares with: the driver
    /// and mini driver are [`ClubClass::Driver`], every wood, hybrid and iron
    /// [`ClubClass::MidIron`], PW/GW/SW/LW [`ClubClass::Wedge`], and the
    /// putter has none.
    fn class(&self) -> Option<ClubClass>;
}

impl ClubExt for Club {
    fn from_code(s: &str) -> Option<Club> {
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

    fn gspro_code(self) -> &'static str {
        match self {
            Club::Driver => "DR",
            Club::MiniDriver => "DR",
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
            // A club code newer than this table: the driver, as for the
            // mini driver.
            _ => "DR",
        }
    }

    fn class(&self) -> Option<ClubClass> {
        match self {
            Club::Driver | Club::MiniDriver => Some(ClubClass::Driver),
            Club::PitchingWedge | Club::GapWedge | Club::SandWedge | Club::LobWedge => {
                Some(ClubClass::Wedge)
            }
            Club::Putter => None,
            _ => Some(ClubClass::MidIron),
        }
    }
}

/// A class of clubs with one set of swing averages ([`Club::class`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClubClass {
    /// The driver and the mini driver.
    Driver,
    /// Every wood, hybrid and iron.
    MidIron,
    /// The wedges: PW, GW, SW and LW.
    Wedge,
}

/// Parse a hand code, `rh` or `lh`, case-insensitively.
#[must_use]
pub fn hand_from_code(code: &str) -> Option<Handedness> {
    [Handedness::Right, Handedness::Left]
        .into_iter()
        .find(|h| h.to_string().eq_ignore_ascii_case(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_codes_parse_in_any_case() {
        for (code, handed) in [
            ("rh", Handedness::Right),
            ("RH", Handedness::Right),
            ("lh", Handedness::Left),
            ("Lh", Handedness::Left),
        ] {
            assert_eq!(hand_from_code(code), Some(handed), "{code}");
        }
        for code in ["", "r", "left", "rhh", " lh"] {
            assert_eq!(hand_from_code(code), None, "{code:?}");
        }
    }

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
    fn gspro_code_round_trips_and_is_letter_first() {
        for &club in Club::ALL {
            let code = club.gspro_code();
            // GSPro has no mini driver; it goes out as the driver.
            let back = if club == Club::MiniDriver {
                Club::Driver
            } else {
                club
            };
            assert_eq!(Club::from_code(code), Some(back), "{code}");
            let b = code.as_bytes();
            let numbered = b[1].is_ascii_digit();
            assert!(!numbered || b[0].is_ascii_alphabetic(), "{code}");
        }
        assert_eq!(Club::Driver.gspro_code(), "DR");
        assert_eq!(Club::MiniDriver.gspro_code(), "DR");
        assert_eq!(Club::Wood3.gspro_code(), "W3");
        assert_eq!(Club::Hybrid4.gspro_code(), "H4");
        assert_eq!(Club::Iron7.gspro_code(), "I7");
        assert_eq!(Club::PitchingWedge.gspro_code(), "PW");
        assert_eq!(Club::Putter.gspro_code(), "PT");
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
    fn class_per_club() {
        for club in Club::ALL {
            let code = club.to_string();
            let expected = match code.as_str() {
                "DR" | "MD" => Some(ClubClass::Driver),
                "PW" | "GW" | "SW" | "LW" => Some(ClubClass::Wedge),
                "PT" => None,
                c if c.ends_with('W') || c.ends_with('H') || c.ends_with('I') => {
                    Some(ClubClass::MidIron)
                }
                c => panic!("unclassified club {c}"),
            };
            assert_eq!(club.class(), expected, "{code}");
        }
        assert_eq!(Club::Wood3.class(), Some(ClubClass::MidIron));
        assert_eq!(Club::Hybrid4.class(), Some(ClubClass::MidIron));
        assert_eq!(Club::Iron7.class(), Some(ClubClass::MidIron));
        assert_eq!(
            serde_json::to_value(ClubClass::MidIron).unwrap(),
            serde_json::json!("mid_iron")
        );
    }
}
