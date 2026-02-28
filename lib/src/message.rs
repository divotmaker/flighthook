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
use crate::{ActorState, MevoConfigEvent, ShotData};
use crate::{ClubInfo, GameStateSnapshot, PlayerInfo};
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
// From impls — inner event types -> FlighthookEvent
// ---------------------------------------------------------------------------

impl From<LaunchMonitorEvent> for FlighthookEvent {
    fn from(event: LaunchMonitorEvent) -> Self {
        FlighthookEvent::LaunchMonitor(LaunchMonitorRecv { event })
    }
}

impl From<GameStateCommandEvent> for FlighthookEvent {
    fn from(event: GameStateCommandEvent) -> Self {
        FlighthookEvent::GameStateCommand(GameStateCommand { event })
    }
}

impl From<ConfigChanged> for FlighthookEvent {
    fn from(changed: ConfigChanged) -> Self {
        FlighthookEvent::ConfigChanged(changed)
    }
}

impl From<ActorState> for FlighthookEvent {
    fn from(state: ActorState) -> Self {
        FlighthookEvent::ActorStatus(state)
    }
}

impl From<ConfigCommand> for FlighthookEvent {
    fn from(cmd: ConfigCommand) -> Self {
        FlighthookEvent::ConfigCommand(cmd)
    }
}

impl From<ConfigOutcome> for FlighthookEvent {
    fn from(result: ConfigOutcome) -> Self {
        FlighthookEvent::ConfigOutcome(result)
    }
}

impl From<AlertMessage> for FlighthookEvent {
    fn from(alert: AlertMessage) -> Self {
        FlighthookEvent::Alert(alert)
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

/// The typed event payload carried by a `FlighthookMessage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlighthookEvent {
    /// Data from a launch monitor (shots).
    LaunchMonitor(LaunchMonitorRecv),
    /// Configuration changed (emitted by reconfigure, consumed by integrations).
    ConfigChanged(ConfigChanged),
    /// Global state command (from an integration or WS client).
    GameStateCommand(GameStateCommand),
    /// Global state snapshot (emitted after GameStateCommand is applied).
    GameStateSnapshot(GameStateSnapshot),
    /// Third-party user data (from WS clients via integrations).
    UserData(UserDataMessage),
    /// Generic actor status update (replaces per-type status events).
    ActorStatus(ActorState),
    /// Config mutation request (emitted by POST handler).
    ConfigCommand(ConfigCommand),
    /// Config mutation outcome (emitted by SystemActor after processing).
    ConfigOutcome(ConfigOutcome),
    /// Alert for user-visible warn/error conditions.
    Alert(AlertMessage),
}

impl FlighthookEvent {
    /// Returns true if this is an ActorStatus event containing telemetry
    /// (battery_pct key in state map). Used for heartbeat filtering in audit.
    pub fn is_actor_status_with_telemetry(&self) -> bool {
        matches!(self, FlighthookEvent::ActorStatus(state) if state.telemetry.contains_key("battery_pct"))
    }
}

// ---------------------------------------------------------------------------
// LaunchMonitor — shot data from a launch monitor
// ---------------------------------------------------------------------------

/// Envelope for events from a launch monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchMonitorRecv {
    pub event: LaunchMonitorEvent,
}

/// Individual events from a launch monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LaunchMonitorEvent {
    /// Completed shot.
    ShotResult { shot: Box<ShotData> },
    /// Armed/ready state change.
    ReadyState { armed: bool, ball_detected: bool },
}

// ---------------------------------------------------------------------------
// ConfigChanged — generic configuration update notification
// ---------------------------------------------------------------------------

/// Configuration changed notification. Emitted by `reconfigure()` when an
/// actor's settings change; consumed by integrations (e.g. GSPro bridge).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChanged {
    pub config: MevoConfigEvent,
}

// ---------------------------------------------------------------------------
// GameStateCommand — from integrations / WS clients
// ---------------------------------------------------------------------------

/// A command to update global state, originating from an integration or WS client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameStateCommand {
    pub event: GameStateCommandEvent,
}

/// The specific global state mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GameStateCommandEvent {
    SetPlayerInfo { player_info: PlayerInfo },
    SetClubInfo { club_info: ClubInfo },
    SetMode { mode: ShotDetectionMode },
}

// ---------------------------------------------------------------------------
// UserData — third-party WS integrations
// ---------------------------------------------------------------------------

/// Opaque data from a third-party WS client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDataMessage {
    #[serde(default)]
    pub integration_type: String,
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

// ---------------------------------------------------------------------------
// ConfigCommand — config mutation request
// ---------------------------------------------------------------------------

/// A request to mutate the system configuration.
///
/// Emitted on the bus by the POST handler. Processed exclusively by
/// `SystemActor`, which applies the mutation, reconciles actors, and
/// optionally emits a `ConfigOutcome`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigCommand {
    /// Opaque correlation ID. When present, SystemActor emits a ConfigOutcome
    /// with the same ID after processing (request-reply pattern).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub action: ConfigAction,
}

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
// ConfigOutcome — config mutation acknowledgment
// ---------------------------------------------------------------------------

/// Acknowledgment of a config mutation, emitted by SystemActor after
/// processing a `ConfigCommand` that had a `request_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigOutcome {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarted: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub started: Vec<String>,
}

// ---------------------------------------------------------------------------
// AlertMessage — user-visible warn/error notifications
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

/// A user-visible alert. Info/debug/trace stays in the tracing backend;
/// warn/error conditions surface here for the UI log panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertMessage {
    pub level: AlertLevel,
    pub message: String,
}
