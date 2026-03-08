//! Unified `FlighthookMessage` bus types.
//!
//! All events flow through a single `broadcast<FlighthookMessage>` channel.
//! Each message has an actor ID, optional device (FRP physical unit ID),
//! optional raw payload (hex-first policy), and a typed event.
//!
//! ## Wire format (FRP envelope)
//!
//! All messages use the FRP envelope shape with `kind` inside `event`:
//! ```json
//! { "actor": "mevo.0", "device": "FS-M2-XXXXXX", "event": { "kind": "ball_flight", ... } }
//! ```
//!
//! `actor` is a flighthook extension (actor framework ID). `device` is the
//! FRP device identifier (present on device events, absent on extensions).
//! FRP consumers silently ignore unknown `kind` values and extra fields per spec.
//!
//! FRP-compliant events: `ShotTrigger`, `BallFlight`, `ClubPath`, `FaceImpact`,
//! `ShotFinished`, `DeviceTelemetry`, `Alert`, `SetDetectionMode`.
//! Flighthook extensions: everything else.

use std::fmt;

use serde::{Deserialize, Serialize, Serializer};

use crate::{Handedness, ShotDetectionMode};
use std::collections::HashMap;

use crate::{ActorStatus, BallFlight, ClubData, FaceImpact};
use crate::{ClubInfo, PlayerInfo};
use crate::{
    FlighthookConfig, GsProSection, MevoSection, MockMonitorSection, RandomClubSection,
    WebserverSection,
};

// ---------------------------------------------------------------------------
// Top-level message
// ---------------------------------------------------------------------------

/// A single event on the unified bus.
///
/// Uses the FRP envelope shape: `{ actor, device?, event: { kind, ... } }`.
/// `actor` is a flighthook extension field. FRP consumers ignore unknown fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlighthookMessage {
    /// Actor ID of the originator (e.g. "mevo.0", "gspro.0", "system").
    #[serde(default)]
    pub actor: String,
    /// FRP device identifier for the physical unit (e.g. the Mevo SSID).
    /// Present on shot lifecycle and device telemetry events; absent on
    /// system/config/actor-status events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_payload: Option<RawPayload>,
    pub event: FlighthookEvent,
}

#[cfg(not(target_arch = "wasm32"))]
impl FlighthookMessage {
    /// Create a new message. Use `.actor()`, `.device()`, and
    /// `.raw()` / `.raw_binary()` to attach metadata.
    pub fn new(event: impl Into<FlighthookEvent>) -> Self {
        Self {
            actor: String::new(),
            device: None,
            raw_payload: None,
            event: event.into(),
        }
    }

    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = actor.into();
        self
    }

    pub fn device(mut self, device: impl Into<String>) -> Self {
        self.device = Some(device.into());
        self
    }

    pub fn raw(mut self, raw: RawPayload) -> Self {
        self.raw_payload = Some(raw);
        self
    }

    pub fn raw_binary(mut self, raw: Vec<u8>) -> Self {
        self.raw_payload = Some(RawPayload::Binary(raw));
        self
    }
}

// ---------------------------------------------------------------------------
// Raw payload — hex-first policy
// ---------------------------------------------------------------------------

/// Raw wire data attached to a bus message.
///
/// Binary payloads serialize as hex strings (no spaces, lowercase).
/// Text payloads (e.g. GSPro JSON) serialize as-is.
#[derive(Debug, Clone)]
pub enum RawPayload {
    Binary(Vec<u8>),
    Text(String),
}

impl Serialize for RawPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            RawPayload::Binary(bytes) => {
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                serializer.serialize_str(&hex)
            }
            RawPayload::Text(s) => serializer.serialize_str(s),
        }
    }
}

impl<'de> Deserialize<'de> for RawPayload {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(RawPayload::Text(s))
    }
}

impl fmt::Display for RawPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RawPayload::Binary(bytes) => {
                for b in bytes {
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
            RawPayload::Text(s) => write!(f, "{s}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Event variants
// ---------------------------------------------------------------------------

/// Re-export flightrelay's ShotKey as the canonical shot correlation type.
pub use flightrelay::ShotKey;

/// The typed event payload carried by a `FlighthookMessage`.
///
/// FRP-compliant events: `ShotTrigger`, `BallFlight`, `ClubPath`, `FaceImpact`,
/// `ShotFinished`, `DeviceTelemetry`, `Alert`.
///
/// Flighthook extensions: everything else. FRP-only consumers silently ignore
/// unknown `kind` values per spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlighthookEvent {
    // -- FRP: Shot lifecycle (correlated by ShotKey + device) --
    /// Ball strike detected. Emitted immediately — no data yet.
    ShotTrigger { key: ShotKey },
    /// Ball flight data available.
    BallFlight { key: ShotKey, ball: Box<BallFlight> },
    /// Club path data available.
    ClubPath { key: ShotKey, club: Box<ClubData> },
    /// Face impact location available.
    FaceImpact {
        key: ShotKey,
        impact: Box<FaceImpact>,
    },
    /// Shot sequence complete. Accumulators should finalize.
    ShotFinished { key: ShotKey },

    // -- FRP: Device status --
    /// Device telemetry — emitted any time a device-reported value changes.
    ///
    /// Carries device-reported state: identity (manufacturer, model, firmware)
    /// and telemetry (ready, battery_pct, tilt, roll, temp_c, external_power).
    /// `ready` is the single readiness signal (all conditions met for a shot).
    ///
    /// Actor/connection lifecycle (status enum, mode, shooting) is conveyed
    /// separately via `ActorStatus`. The two variants have no overlapping keys.
    DeviceTelemetry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manufacturer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        firmware: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        telemetry: Option<HashMap<String, String>>,
    },
    /// Alert for warn/error/critical conditions.
    Alert {
        severity: flightrelay::Severity,
        message: String,
    },

    // -- FRP: Controller commands --
    /// Set the shot detection mode and/or handedness on the device.
    ///
    /// Both fields are optional and latched independently — the most recent
    /// value for each field is the active value. Omitting a field does not
    /// reset it.
    SetDetectionMode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<ShotDetectionMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handed: Option<Handedness>,
    },

    // -- Flighthook extensions --
    /// Player info update (name).
    PlayerInfo { player_info: PlayerInfo },
    /// Club selection update.
    ClubInfo { club_info: ClubInfo },
    /// Config mutation request (emitted by POST handler, processed by SystemActor).
    ConfigCommand {
        /// Opaque correlation ID for request-reply pattern.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        action: Box<ConfigAction>,
    },
    /// Config mutation outcome (emitted by SystemActor after processing).
    ConfigOutcome {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restarted: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stopped: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        started: Vec<String>,
    },
    /// Actor/connection lifecycle update.
    ///
    /// Carries actor-framework state: connection status enum and actor-specific
    /// telemetry (detection_mode, radar_mode, shot_count, tracking_mode, device_info label).
    ///
    /// Device-reported state (ready, battery, tilt, temp) is conveyed
    /// separately via `DeviceTelemetry`. The two variants have no overlapping keys.
    ActorStatus {
        status: ActorStatus,
        #[serde(default)]
        telemetry: HashMap<String, String>,
    },
}

impl FlighthookEvent {
    /// Returns true if this is an ActorStatus event containing telemetry
    /// (battery_pct key in state map). Used for heartbeat filtering in audit.
    pub fn is_actor_status_with_telemetry(&self) -> bool {
        matches!(self, FlighthookEvent::ActorStatus { telemetry, .. } if telemetry.contains_key("battery_pct"))
    }
}

// ---------------------------------------------------------------------------
// ConfigAction — specific config mutations
// ---------------------------------------------------------------------------

/// The specific config mutation to apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigAction {
    /// Replace the entire config. Used by POST /api/settings.
    ReplaceAll {
        config: FlighthookConfig,
    },
    /// Per-section upserts.
    UpsertWebserver {
        index: String,
        section: WebserverSection,
    },
    UpsertMevo {
        index: String,
        section: MevoSection,
    },
    UpsertGsPro {
        index: String,
        section: GsProSection,
    },
    UpsertMockMonitor {
        index: String,
        section: MockMonitorSection,
    },
    UpsertRandomClub {
        index: String,
        section: RandomClubSection,
    },
    /// Remove a section by global ID ("mevo.0", "gspro.1", "webserver.0").
    Remove {
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_event_envelope() {
        let msg = FlighthookMessage {
            actor: "mevo.0".into(),
            device: Some("FS-M2-XXXXXX".into()),
            raw_payload: None,
            event: FlighthookEvent::DeviceTelemetry {
                manufacturer: Some("FlightScope".into()),
                model: None,
                firmware: None,
                telemetry: None,
            },
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["event"]["kind"], "device_telemetry");
        assert_eq!(json["actor"], "mevo.0");
        assert_eq!(json["device"], "FS-M2-XXXXXX");
    }

    #[test]
    fn non_device_event_envelope() {
        let msg = FlighthookMessage {
            actor: "webserver.0".into(),
            device: None,
            raw_payload: None,
            event: FlighthookEvent::ActorStatus {
                status: crate::ActorStatus::Connected,
                telemetry: HashMap::from([("bind".into(), "0.0.0.0:5880".into())]),
            },
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        // kind inside event even without device
        assert_eq!(json["event"]["kind"], "actor_status");
        assert_eq!(json["actor"], "webserver.0");
        assert!(json.get("device").is_none());
    }

    #[test]
    fn roundtrip_with_device() {
        let msg = FlighthookMessage {
            actor: "mevo.0".into(),
            device: Some("FS-M2-XXXXXX".into()),
            raw_payload: None,
            event: FlighthookEvent::ShotFinished {
                key: ShotKey {
                    shot_id: "abc".into(),
                    shot_number: 1,
                },
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.actor, "mevo.0");
        assert_eq!(back.device.as_deref(), Some("FS-M2-XXXXXX"));
        assert!(matches!(back.event, FlighthookEvent::ShotFinished { .. }));
    }

    #[test]
    fn roundtrip_without_device() {
        let msg = FlighthookMessage {
            actor: "system".into(),
            device: None,
            raw_payload: None,
            event: FlighthookEvent::ActorStatus {
                status: crate::ActorStatus::Connected,
                telemetry: HashMap::new(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.actor, "system");
        assert!(back.device.is_none());
        assert!(matches!(
            back.event,
            FlighthookEvent::ActorStatus { status: crate::ActorStatus::Connected, .. }
        ));
    }
}
