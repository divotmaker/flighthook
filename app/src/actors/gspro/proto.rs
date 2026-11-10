use std::io::Write;

use super::BridgeError;
use super::api;
use crate::bus::BusSender;
use flighthook::{Club, ClubInfo, FlighthookEvent, FlighthookMessage, HandInfo, Severity};

pub(crate) fn send_message(
    stream: &mut impl Write,
    msg: &api::GsProMessage,
) -> Result<(), BridgeError> {
    let json = serde_json::to_vec(msg)
        .map_err(|e| BridgeError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
    stream.write_all(&json).map_err(BridgeError::Io)?;
    stream.flush().map_err(BridgeError::Io)?;
    Ok(())
}

pub(crate) fn log_outbound(msg: &api::GsProMessage) {
    let b = &msg.ball_data;
    tracing::info!(
        "gspro -> shot #{}: {:.1}mph {:.1}vla {:.1}hla {:.0}yd carry {:.0}spin",
        msg.shot_number,
        b.speed,
        b.vla,
        b.hla,
        b.carry_distance.unwrap_or(0.0),
        b.total_spin,
    );
    if msg.shot_data_options.contains_club_data {
        let c = &msg.club_data;
        tracing::info!(
            "gspro ->   club: {:.1}mph aoa={:.1} face={:.1} path={:.1} loft={:.1}",
            c.speed,
            c.angle_of_attack,
            c.face_to_target,
            c.path,
            c.loft,
        );
    }
}

pub(crate) fn parse_response_debug(buf: &[u8]) -> Option<String> {
    let stream = serde_json::Deserializer::from_slice(buf).into_iter::<api::GsProResponse>();
    let mut parts = Vec::new();
    for result in stream {
        match result {
            Ok(resp) => parts.push(format!("{resp:?}")),
            Err(_) => break,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

/// What one read from the simulator carried, for the card's telemetry.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Heard {
    /// The last response's code and message.
    pub last: Option<(i32, String)>,
    /// Whether a response carried player information.
    pub player: bool,
}

/// Handle the responses in `buf`: player and club information goes on the
/// bus, a code outside 2xx raises an alert. Returns what they carried.
pub(crate) fn handle_response(buf: &[u8], sender: &BusSender) -> Heard {
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
                    tracing::info!("gspro <- {}: {}", resp.code, resp.message);
                } else {
                    tracing::warn!("gspro <- GSPro returned {}: {}", resp.code, resp.message);
                    sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                        severity: Severity::Warn,
                        message: format!("GSPro returned {}: {}", resp.code, resp.message),
                    }));
                }

                // Emit player/club info if present
                if let Some(ref player) = resp.player {
                    if let Some(handed) = player.handed.as_deref().and_then(api::hand_from_code) {
                        sender.send(FlighthookMessage::new(FlighthookEvent::HandInfo {
                            hand_info: HandInfo { handed },
                        }));
                    }
                    if let Some(ref club_str) = player.club {
                        if let Some(club) = Club::from_code(club_str) {
                            sender.send(FlighthookMessage::new(FlighthookEvent::ClubInfo {
                                club_info: ClubInfo { club },
                            }));
                        } else {
                            tracing::warn!("gspro: unknown club code '{club_str}', ignoring");
                            sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                                severity: Severity::Warn,
                                message: format!("GSPro: unknown club code '{club_str}'"),
                            }));
                        }
                    }
                }
            }
            Err(e) => {
                if !parsed_any {
                    tracing::warn!("gspro <- parse error: {e}");
                    sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                        severity: Severity::Warn,
                        message: format!("Could not parse GSPro response: {e}"),
                    }));
                }
                break;
            }
        }
    }
    heard
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::broadcast;

    use super::*;

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
            let (bus_tx, mut rx) = broadcast::channel(16);
            let sender = BusSender::new("openconnect_client.0".into(), bus_tx, Arc::default());
            let buf = format!(
                r#"{{"Code":201,"Message":"GSPro Player Information","Player":{{"Club":"{code}"}}}}"#
            );
            let _ = handle_response(buf.as_bytes(), &sender);
            let msg = rx.try_recv().expect("club info");
            let FlighthookEvent::ClubInfo { club_info } = msg.event else {
                panic!("{code}: expected ClubInfo, got {:?}", msg.event);
            };
            assert_eq!(club_info.club, club, "{code}");
            assert!(rx.try_recv().is_err(), "{code}: no alert expected");
        }
    }

    /// GSPro's player message sets the hand the way every source does, with
    /// `hand_info`; it does not command the devices itself. Each repeat is
    /// passed on: the system actor tells the devices only about a change.
    #[test]
    fn the_player_hand_becomes_hand_info() {
        let (bus_tx, mut rx) = broadcast::channel(16);
        let sender = BusSender::new("openconnect_client.0".into(), bus_tx, Arc::default());
        let buf = r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Handed":"LH","Club":"I7"}}"#;
        for _ in 0..2 {
            let _ = handle_response(buf.as_bytes(), &sender);
            let events: Vec<FlighthookEvent> =
                std::iter::from_fn(|| rx.try_recv().ok().map(|m| m.event)).collect();
            assert!(
                matches!(
                    events.as_slice(),
                    [
                        FlighthookEvent::HandInfo {
                            hand_info: HandInfo {
                                handed: flighthook::Handedness::Left
                            }
                        },
                        FlighthookEvent::ClubInfo { club_info },
                    ] if club_info.club == Club::Iron7
                ),
                "{events:?}"
            );
        }
    }

    /// A hand GSPro does not define, or omits, is ignored: no `hand_info`,
    /// and a club in the same message still comes through.
    #[test]
    fn an_unknown_or_omitted_hand_is_ignored() {
        let (bus_tx, mut rx) = broadcast::channel(16);
        let sender = BusSender::new("openconnect_client.0".into(), bus_tx, Arc::default());
        let unknown =
            r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Handed":"XX"}}"#;
        let _ = handle_response(unknown.as_bytes(), &sender);
        assert!(rx.try_recv().is_err());

        let omitted = r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Club":"I7"}}"#;
        let _ = handle_response(omitted.as_bytes(), &sender);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|m| m.event)
            .collect();
        assert!(
            matches!(
                events.as_slice(),
                [FlighthookEvent::ClubInfo { club_info }] if club_info.club == Club::Iron7
            ),
            "{events:?}"
        );
    }
}
