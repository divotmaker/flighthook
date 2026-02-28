//! API response types and bus message types.
//!
//! Bus message types are re-exported from `flighthook` (the shared
//! schema crate). REST response types are also shared via schemas.

// ---------------------------------------------------------------------------
// Re-exports from schemas — shared with the app crate
// ---------------------------------------------------------------------------

pub use flighthook::{
    // Bus message types
    ActorState,
    // Domain types
    ActorStatus,
    // API types
    ActorStatusResponse,
    AlertLevel,
    BallFlight,
    ClubData,
    // Global state types
    ClubInfo,
    // Config types
    Distance,
    FlighthookConfig,
    FlighthookEvent,
    FlighthookMessage,
    GameStateCommand,
    GameStateCommandEvent,
    GameStateSnapshot,
    GsProSection,
    LaunchMonitorEvent,
    LaunchMonitorRecv,
    MevoSection,
    MockMonitorSection,
    PartialMode,
    PlayerInfo,
    PostSettingsResponse,
    RandomClubSection,
    ShotData,
    ShotDetectionMode,
    SpinData,
    StatusResponse,
    UnitSystem,
    UserDataMessage,
    Velocity,
    WebserverSection,
};

// ---------------------------------------------------------------------------
// Log entry (UI-local)
// ---------------------------------------------------------------------------

/// Log entry — constructed from FlighthookMessage bus events.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub source_name: String,
    pub source_id: String,
    pub message_type: String,
    pub event_debug: String,
    pub raw: String,
    /// Alert severity (only set for alert events).
    pub alert_level: Option<AlertLevel>,
}

/// Parse a WS JSON message into a FlighthookMessage.
pub fn parse_ws_message(text: &str) -> Option<FlighthookMessage> {
    serde_json::from_str(text).ok()
}

/// Synthetic WS event for connection loss.
pub const WS_DISCONNECTED: &str = "__ws_disconnected__";
