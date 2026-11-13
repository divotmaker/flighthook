//! The UI socket `/api/ws`: what the server sends the UI and what the UI
//! sends back.
//!
//! The UI is part of the flighthook actor. On connect the server sends one
//! [`UiMessage::Snapshot`], then every bus message as [`UiMessage::Frp`] or
//! [`UiMessage::Local`], and [`UiMessage::Shot`] whenever the shot store
//! finishes or updates a shot. Each message is one JSON text frame, tagged
//! by its variant: `{"frp": {...}}`, `{"local": {...}}`, `{"shot": {...}}`,
//! `{"snapshot": {...}}`.
//!
//! The UI sends [`UiCommand`]s: an FRP command body (`golfer.select`,
//! `practice.new_block`) with its own `ref`, answered by the matching
//! `bus.reply` (as `{"frp": ...}`, with the UI's `ref`) on that connection
//! only; or a local command.

use std::collections::HashMap;

use flightrelay::{FrpMessage, Severity, ShotKey};

/// FRP messages, as the socket carries them.
pub use flightrelay::FrpMessage as BusMessage;
/// The FRP types the UI reads from the socket and sends on it.
pub use flightrelay::message::KINDS as FRP_KINDS;
pub use flightrelay::types::roles;
pub use flightrelay::{
    BusActor, BusAlert, DeviceInfo, FrpBody, GolferId, GolferProfile, GolferSelect, PracticeBlock,
    PracticeNewBlock, Removal, ShotFinished, ShotTrigger, StateKey, StateKind,
};
use serde::{Deserialize, Serialize};

use crate::{ActorStatus, ActorStatusResponse, CameraPosition, RoundState, ShotData, ShotGrouping};

/// The path of the UI socket.
pub const UI_WS_PATH: &str = "/api/ws";

/// One message from the server to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiMessage {
    /// Sent once, first, on connect.
    Snapshot(Box<UiSnapshot>),
    /// A message on flighthook's bus.
    Frp(Box<FrpMessage>),
    /// A flighthook event that is no FRP message.
    Local(Box<UiLocal>),
    /// A shot the store finished or updated, as `GET /api/shots` lists it.
    Shot(Box<ShotData>),
}

/// The state of things when the UI connects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiSnapshot {
    /// flighthook's own actor id: its `golfer.turn` and `practice.block`
    /// are the ones that count.
    pub actor_id: String,
    /// The local card key of each flighthook actor on the bus, by actor id.
    pub card_keys: HashMap<String, String>,
    /// Every FRP state on the bus (local and remote actors), ordered by
    /// kind, then owner, then id.
    pub states: Vec<FrpMessage>,
    /// Each local card's status, by card key.
    pub cards: HashMap<String, ActorStatusResponse>,
    /// The tournament round.
    pub round: RoundState,
    /// Counts up with every applied configuration change.
    pub config_generation: u64,
}

/// A local event of the card `card` (`mevo.0`, or `system` / `web`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiLocal {
    pub card: String,
    pub event: UiLocalEvent,
}

/// The local events the UI hears.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UiLocalEvent {
    /// The card's connection status and card rows.
    CardStatus {
        status: ActorStatus,
        telemetry: HashMap<String, String>,
    },
    /// An alert of a card that is no FRP actor.
    Alert { severity: Severity, message: String },
    /// The tournament round started or ended.
    RoundState { round: RoundState },
    /// A configuration change was applied.
    ConfigOutcome {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        restarted: Vec<String>,
        stopped: Vec<String>,
        started: Vec<String>,
        /// The configuration generation after it.
        config_generation: u64,
    },
    /// The card key of every local actor, by actor id: sent after every
    /// configuration change, so a new card's actor is known before its
    /// first message.
    CardKeys { card_keys: HashMap<String, String> },
    /// The shot history was deleted.
    ClearShotHistory,
    /// A camera finished a clip of these shots (beta).
    ShotVideo {
        keys: Vec<ShotKey>,
        position: CameraPosition,
        duration_ms: u64,
        fps: f64,
        trigger_offset_ms: i64,
        impact_offset_ms: i64,
        complete: bool,
        dropped: u32,
    },
    /// The clips of these shots were deleted (beta).
    ShotVideoRemoved { keys: Vec<ShotKey> },
    /// Body tracking of a camera's clip is done (alpha).
    ShotPose { keys: Vec<ShotKey>, rotation: u16 },
    /// The swing analysis of a camera's clip is done (alpha).
    ShotSwing { keys: Vec<ShotKey> },
}

/// A `bus.actor`'s values as card rows: `ready` (no blockers),
/// `battery_pct`, `tilt`, `roll` and the actor's own telemetry. A value
/// the actor does not report is empty, which removes the row.
#[must_use]
pub fn bus_actor_rows(actor: &flightrelay::BusActor) -> HashMap<String, String> {
    let device = actor.device.clone().unwrap_or_default();
    let mut rows: HashMap<String, String> = actor
        .telemetry
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    rows.insert("ready".into(), actor.is_clear().to_string());
    for (key, value) in [
        ("battery_pct", device.battery_pct.map(|v| v.to_string())),
        ("tilt", device.tilt.map(|v| v.to_string())),
        ("roll", device.roll.map(|v| v.to_string())),
    ] {
        rows.insert(key.into(), value.unwrap_or_default());
    }
    rows
}

/// One message from the UI to the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiCommand {
    /// An FRP command (`golfer.select`, `practice.new_block`) with the UI's
    /// `ref`. The server stamps `from` with the flighthook actor id, so the
    /// UI's `from` is not read.
    Frp(Box<FrpMessage>),
    /// A local command.
    Local(UiLocalCommand),
}

/// The UI's local commands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UiLocalCommand {
    /// Delete the shot history.
    ClearHistory,
    /// Set the default shot view.
    Grouping { mode: ShotGrouping },
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightrelay::{FrpBody, GolferSelect};

    #[test]
    fn messages_are_tagged_by_variant() {
        let local = UiMessage::Local(Box::new(UiLocal {
            card: "system".into(),
            event: UiLocalEvent::ClearShotHistory,
        }));
        let json = serde_json::to_value(&local).expect("json");
        assert_eq!(
            json,
            serde_json::json!({"local": {"card": "system", "event": {"kind": "clear_shot_history"}}})
        );
        let back: UiMessage = serde_json::from_value(json.clone()).expect("back");
        assert_eq!(serde_json::to_value(&back).expect("json"), json);
    }

    #[test]
    fn commands_read_from_the_wire() {
        let cmd: UiCommand = serde_json::from_str(
            r#"{"frp":{"kind":"golfer.select","from":"ui","ref":"ui-1","club":"7I"}}"#,
        )
        .expect("frp");
        let UiCommand::Frp(body) = cmd else {
            panic!("not frp");
        };
        let FrpBody::GolferSelect(GolferSelect { r#ref, club, .. }) = body.body else {
            panic!("not a select: {body:?}");
        };
        assert_eq!(r#ref, "ui-1");
        assert!(club.is_some());

        let cmd: UiCommand =
            serde_json::from_str(r#"{"local":{"cmd":"grouping","mode":"by_club"}}"#)
                .expect("local");
        assert_eq!(
            cmd,
            UiCommand::Local(UiLocalCommand::Grouping {
                mode: ShotGrouping::ByClub
            })
        );
        let cmd: UiCommand =
            serde_json::from_str(r#"{"local":{"cmd":"clear_history"}}"#).expect("local");
        assert_eq!(cmd, UiCommand::Local(UiLocalCommand::ClearHistory));
    }
}
