//! Unified `FlighthookMessage` bus types.
//!
//! All events flow through a single `broadcast<FlighthookMessage>` channel.
//! Each message has a source (actor ID), optional device (FRP physical unit ID),
//! optional raw payload (hex-first policy), and a typed event.
//!
//! The shot lifecycle events (`ShotTrigger`, `BallFlight`, `ClubPath`,
//! `FaceImpact`, `ShotFinished`, `DeviceInfo`, `Alert`) are FRP-compliant.
//! All other variants are flighthook extensions. FRP-only consumers ignore
//! unknown `kind` values per spec.

use std::fmt;

use serde::{Deserialize, Serialize, Serializer};

use crate::ShotDetectionMode;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlighthookMessage {
    /// Actor ID of the originator (e.g. "mevo.0", "gspro.0", "system").
    #[serde(default)]
    pub source: String,
    /// FRP device identifier for the physical unit (e.g. the Mevo SSID).
    /// Present on shot lifecycle and device info events; absent on system/config events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_payload: Option<RawPayload>,
    pub event: FlighthookEvent,
}

#[cfg(not(target_arch = "wasm32"))]
impl FlighthookMessage {
    /// Create a new message. Use `.source()`, `.device()`, and
    /// `.raw()` / `.raw_binary()` to attach metadata.
    pub fn new(event: impl Into<FlighthookEvent>) -> Self {
        Self {
            source: String::new(),
            device: None,
            raw_payload: None,
            event: event.into(),
        }
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
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
/// `ShotFinished`, `DeviceInfo`, `Alert`.
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
    /// Device identification and telemetry.
    DeviceInfo {
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
    /// Set the shot detection mode on the device.
    SetDetectionMode { mode: ShotDetectionMode },

    // -- Flighthook extensions --
    /// Player info update (handedness).
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
    /// Generic actor status update (lifecycle + telemetry).
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
