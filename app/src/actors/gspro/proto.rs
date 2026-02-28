use std::io::Write;
use std::net::TcpStream;

use super::BridgeError;
use super::api;
use crate::bus::BusSender;
use flighthook::{
    AlertLevel, AlertMessage, Club, ClubInfo, FlighthookMessage, GameStateCommandEvent, PlayerInfo,
};

pub(crate) fn send_message(
    stream: &mut TcpStream,
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

pub(crate) fn handle_response(buf: &[u8], sender: &BusSender) {
    let stream = serde_json::Deserializer::from_slice(buf).into_iter::<api::GsProResponse>();
    let mut parsed_any = false;
    for result in stream {
        match result {
            Ok(resp) => {
                parsed_any = true;
                if resp.code >= 200 && resp.code < 300 {
                    tracing::info!("gspro <- {}: {}", resp.code, resp.message);
                } else {
                    tracing::warn!("gspro <- GSPro returned {}: {}", resp.code, resp.message);
                    sender.send(FlighthookMessage::new(AlertMessage {
                        level: AlertLevel::Warn,
                        message: format!("GSPro returned {}: {}", resp.code, resp.message),
                    }));
                }

                // Emit player info if present
                if let Some(ref player) = resp.player {
                    if let Some(ref handed) = player.handed {
                        sender.send(FlighthookMessage::new(
                            GameStateCommandEvent::SetPlayerInfo {
                                player_info: PlayerInfo {
                                    handed: handed.clone(),
                                },
                            },
                        ));
                    }
                    if let Some(ref club_str) = player.club {
                        if let Some(club) = Club::from_code(club_str) {
                            sender.send(FlighthookMessage::new(
                                GameStateCommandEvent::SetClubInfo {
                                    club_info: ClubInfo { club },
                                },
                            ));
                        } else {
                            tracing::warn!("gspro: unknown club code '{club_str}', ignoring");
                            sender.send(FlighthookMessage::new(AlertMessage {
                                level: AlertLevel::Warn,
                                message: format!("GSPro: unknown club code '{club_str}'"),
                            }));
                        }
                    }
                }
            }
            Err(e) => {
                if !parsed_any {
                    tracing::warn!("gspro <- parse error: {e}");
                    sender.send(FlighthookMessage::new(AlertMessage {
                        level: AlertLevel::Warn,
                        message: format!("Could not parse GSPro response: {e}"),
                    }));
                }
                break;
            }
        }
    }
}
