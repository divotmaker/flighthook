//! Reading the simulator's responses.

use flightrelay::driver::{Events, Level};
use flightrelay::{Club, Handedness, Severity};

use super::api;

/// What one read from the simulator carried, for the card's telemetry.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Heard {
    /// The last response's code and message.
    pub last: Option<(i32, String)>,
    /// Whether a response carried player information.
    pub player: bool,
    /// The simulator's player information, the last of the read: its hand
    /// and club, each when it named one flighthook knows.
    pub info: Option<(Option<Handedness>, Option<Club>)>,
}

/// Handle the responses in `buf`: a code outside 2xx, an unknown club code
/// or text that is not a response raises an alert. Returns what they
/// carried; the player information is the session's to send.
pub(crate) fn handle_response(buf: &[u8], out: &mut Events<'_>) -> Heard {
    let stream = serde_json::Deserializer::from_slice(buf).into_iter::<api::GsProResponse>();
    let mut parsed_any = false;
    let mut heard = Heard::default();
    for result in stream {
        match result {
            Ok(resp) => {
                parsed_any = true;
                heard.last = Some((resp.code, resp.message.clone()));
                heard.player |= resp.player.is_some();
                if resp.code >= 200 && resp.code < 300 {
                    out.log(Level::Info, format!("<- {}: {}", resp.code, resp.message));
                } else {
                    out.log(
                        Level::Warn,
                        format!("<- GSPro returned {}: {}", resp.code, resp.message),
                    );
                    out.alert(
                        Severity::Warn,
                        format!("GSPro returned {}: {}", resp.code, resp.message),
                    );
                }

                if let Some(ref player) = resp.player {
                    let handed = player.handed.as_deref().and_then(api::hand_from_code);
                    let club = player.club.as_deref().and_then(|code| {
                        let club = api::club_from_code(code);
                        if club.is_none() {
                            out.log(Level::Warn, format!("unknown club code '{code}', ignoring"));
                            out.alert(Severity::Warn, format!("GSPro: unknown club code '{code}'"));
                        }
                        club
                    });
                    heard.info = Some((handed, club));
                }
            }
            Err(e) => {
                if !parsed_any {
                    out.log(Level::Warn, format!("<- parse error: {e}"));
                    out.alert(
                        Severity::Warn,
                        format!("Could not parse GSPro response: {e}"),
                    );
                }
                break;
            }
        }
    }
    heard
}

#[cfg(test)]
mod tests {
    use flightrelay::FrpBody;
    use flightrelay::driver::DeviceEvent;
    use flightrelay::testing::TestHost;

    use super::*;

    /// What `buf` carried, and whether reading it raised an alert.
    fn heard(buf: &str) -> (Heard, bool) {
        let mut host = TestHost::new();
        let heard = host.with(|_, out| handle_response(buf.as_bytes(), out));
        let alerted = host
            .take_frp()
            .iter()
            .any(|e| matches!(e, DeviceEvent::Frp(FrpBody::BusAlert(_))));
        (heard, alerted)
    }

    /// GSPro sends club codes letter-first; every club in its index is
    /// recognized and forwarded as a club change.
    #[test]
    fn letter_first_club_codes_become_club_info() {
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
            ("I7", Club::Iron7),
        ];
        for (code, club) in cases {
            let buf = format!(
                r#"{{"Code":201,"Message":"GSPro Player Information","Player":{{"Club":"{code}"}}}}"#
            );
            let (heard, alerted) = heard(&buf);
            assert_eq!(heard.info, Some((None, Some(club))), "{code}");
            assert!(heard.player, "{code}");
            assert!(!alerted, "{code}: no alert expected");
        }
    }

    /// GSPro's player message gives the hand and club to the session, which
    /// sends them in its own select; reading it raises nothing. Each repeat
    /// is reported again.
    #[test]
    fn the_player_hand_is_heard() {
        let buf = r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Handed":"LH","Club":"I7"}}"#;
        for _ in 0..2 {
            let (heard, alerted) = heard(buf);
            assert_eq!(
                heard.info,
                Some((Some(Handedness::Left), Some(Club::Iron7)))
            );
            assert!(!alerted);
        }
    }

    /// A hand GSPro does not define, or omits, is ignored, and a club in the
    /// same message still comes through.
    #[test]
    fn an_unknown_or_omitted_hand_is_ignored() {
        let unknown =
            r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Handed":"XX"}}"#;
        let (heard_unknown, alerted) = heard(unknown);
        assert_eq!(heard_unknown.info, Some((None, None)));
        assert!(!alerted);

        let omitted = r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Club":"I7"}}"#;
        let (heard_omitted, alerted) = heard(omitted);
        assert_eq!(heard_omitted.info, Some((None, Some(Club::Iron7))));
        assert!(!alerted);
    }
}
