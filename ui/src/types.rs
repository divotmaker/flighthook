//! API response types and bus message types.
//!
//! Bus message types are re-exported from `flighthook` (the shared
//! schema crate). REST response types are also shared via schemas.

// ---------------------------------------------------------------------------
// Re-exports from schemas — shared with the app crate
// ---------------------------------------------------------------------------

pub use flighthook::{
    // Domain types
    ActorStatus,
    // API types
    ActorStatusResponse,
    BallFlight,
    ClubData,
    // Global state types
    Club,
    ClubInfo,
    // Config types
    Distance,
    DistanceExt,
    FlighthookConfig,
    FlighthookEvent,
    FlighthookMessage,
    GameStateSnapshot,
    Handedness,
    GsProSection,
    MevoSection,
    MockMonitorSection,
    PlayerInfo,
    PostSettingsResponse,
    RandomClubSection,
    Severity,
    ShotData,
    ShotDetectionMode,
    ShotKey,
    StatusResponse,
    UnitSystem,
    Velocity,
    WebserverSection,
};

// ---------------------------------------------------------------------------
// ShotRow — incrementally populated shot display row
// ---------------------------------------------------------------------------

/// A shot row in the UI grid. Created on `ShotTrigger` with just the key,
/// then populated as `BallFlight` and `ClubPath` events arrive.
#[derive(Debug, Clone)]
pub struct ShotRow {
    pub actor: String,
    pub shot_id: String,
    pub shot_number: u32,
    pub ball: Option<BallFlight>,
    pub club: Option<ClubData>,
}

impl ShotRow {
    /// Convert all distance and velocity fields to the given unit system.
    pub fn to_unit_system(&self, system: UnitSystem) -> ShotRow {
        // Delegate to ShotData conversion for the populated fields
        let tmp = ShotData {
            actor: String::new(),
            shot_number: 0,
            ball: self.ball.clone(),
            club: self.club.clone(),
            impact: None,
        };
        let converted = tmp.to_unit_system(system);
        ShotRow {
            actor: self.actor.clone(),
            shot_id: self.shot_id.clone(),
            shot_number: self.shot_number,
            ball: converted.ball,
            club: converted.club,
        }
    }
}

impl From<ShotData> for ShotRow {
    fn from(shot: ShotData) -> Self {
        Self {
            actor: shot.actor,
            shot_id: String::new(),
            shot_number: shot.shot_number,
            ball: shot.ball,
            club: shot.club,
        }
    }
}

// ---------------------------------------------------------------------------
// Log entry (UI-local)
// ---------------------------------------------------------------------------

/// Log entry — constructed from FlighthookMessage bus events.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub actor_name: String,
    pub actor_id: String,
    pub message_type: String,
    pub event_debug: String,
    pub raw: String,
    /// Alert severity (only set for alert events).
    pub alert_severity: Option<Severity>,
}

/// Parse a WS JSON message into a FlighthookMessage.
pub fn parse_ws_message(text: &str) -> Option<FlighthookMessage> {
    serde_json::from_str(text).ok()
}

/// Synthetic WS event for connection loss.
pub const WS_DISCONNECTED: &str = "__ws_disconnected__";
