//! Unified `FlighthookMessage` bus types.
//!
//! All events flow through a single `broadcast<FlighthookMessage>` channel.
//! Each message has a source (global ID of the originator), timestamp,
//! optional raw payload (hex-first policy), and a typed event.
//! Producers create messages; consumers subscribe and filter.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, Serializer};

use crate::ShotDetectionMode;
use std::collections::HashMap;

use crate::{ActorStatus, BallFlight, ClubData};
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
    #[serde(default)]
    pub source: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_payload: Option<RawPayload>,
    pub event: FlighthookEvent,
}

#[cfg(not(target_arch = "wasm32"))]
impl FlighthookMessage {
    /// Create a new message with the current UTC timestamp. Use `.source()` and
    /// `.raw()` / `.raw_binary()` to attach metadata.
    pub fn new(event: impl Into<FlighthookEvent>) -> Self {
        Self {
            source: String::new(),
            timestamp: Utc::now(),
            raw_payload: None,
            event: event.into(),
        }
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
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
// ShotKey — correlates shot lifecycle events
// ---------------------------------------------------------------------------

/// Globally unique shot identifier. Generated once by the producer at trigger
/// time, then carried on every event in the shot lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShotKey {
    /// Unique shot ID (UUID v4 string). Survives across sessions, databases, replays.
    pub shot_id: String,
    /// Session-level shot number from the launch monitor (monotonic per-source).
    pub shot_number: i32,
}

// ---------------------------------------------------------------------------
// Event variants
// ---------------------------------------------------------------------------

/// The typed event payload carried by a `FlighthookMessage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlighthookEvent {
    // -- Shot lifecycle (correlated by ShotKey + message source) --
    /// Ball strike detected. Emitted immediately — no data yet.
    ShotTrigger { key: ShotKey },
    /// Ball flight data available. `estimated` = true means the shot may not have fully read, but could still be usable in-game.
    BallFlight {
        key: ShotKey,
        ball: Box<BallFlight>,
        estimated: bool,
    },
    /// Club path data available.
    ClubPath { key: ShotKey, club: Box<ClubData> },
    /// Shot sequence complete. Accumulators should finalize.
    ShotFinished { key: ShotKey },

    // -- Launch monitor state --
    /// Armed/ready/ball state from a launch monitor.
    LaunchMonitorState { armed: bool, ball_detected: bool },

    // -- Game state --
    /// Player info update (handedness).
    PlayerInfo { player_info: PlayerInfo },
    /// Club selection update.
    ClubInfo { club_info: ClubInfo },
    /// Shot detection mode update.
    ShotDetectionMode { mode: ShotDetectionMode },

    // -- Config --
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

    // -- Infrastructure --
    /// Generic actor status update (lifecycle + telemetry).
    ActorStatus {
        status: ActorStatus,
        #[serde(default)]
        telemetry: HashMap<String, String>,
    },
    /// Alert for user-visible warn/error conditions.
    Alert { level: AlertLevel, message: String },
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

// ---------------------------------------------------------------------------
// AlertLevel
// ---------------------------------------------------------------------------

/// Severity level for alert messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertLevel {
    Warn,
    Error,
}

impl fmt::Display for AlertLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AlertLevel::Warn => write!(f, "warn"),
            AlertLevel::Error => write!(f, "error"),
        }
    }
}
