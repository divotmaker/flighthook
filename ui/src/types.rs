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
    AlertLevel,
    BallFlight,
    ClubData,
    // Global state types
    Club,
    ClubInfo,
    // Config types
    Distance,
    FlighthookConfig,
    FlighthookEvent,
    FlighthookMessage,
    GameStateSnapshot,
    GsProSection,
    MevoSection,
    MockMonitorSection,
    EstimatedMode,
    PlayerInfo,
    PostSettingsResponse,
    RandomClubSection,
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
    pub source: String,
    pub shot_id: String,
    pub shot_number: i32,
    pub ball: Option<BallFlight>,
    pub club: Option<ClubData>,
    pub estimated: bool,
}

impl ShotRow {
    /// Convert all distance and velocity fields to the given unit system.
    pub fn to_unit_system(&self, system: UnitSystem) -> ShotRow {
        // Delegate to ShotData conversion for the populated fields
        let ball = self.ball.as_ref().map(|b| {
            let tmp = ShotData {
                source: String::new(),
                shot_number: 0,
                ball: b.clone(),
                club: None,
                estimated: false,
            };
            tmp.to_unit_system(system).ball
        });
        let club = self.club.as_ref().map(|c| {
            let tmp = ShotData {
                source: String::new(),
                shot_number: 0,
                ball: BallFlight {
                    launch_speed: Velocity::MetersPerSecond(0.0),
                    launch_elevation: 0.0,
                    launch_azimuth: 0.0,
                    carry_distance: None,
                    total_distance: None,
                    max_height: None,
                    flight_time: None,
                    roll_distance: None,
                    backspin_rpm: None,
                    sidespin_rpm: None,
                },
                club: Some(c.clone()),
                estimated: false,
            };
            tmp.to_unit_system(system).club.unwrap()
        });
        ShotRow {
            source: self.source.clone(),
            shot_id: self.shot_id.clone(),
            shot_number: self.shot_number,
            ball,
            club,
            estimated: self.estimated,
        }
    }
}

impl From<ShotData> for ShotRow {
    fn from(shot: ShotData) -> Self {
        Self {
            source: shot.source,
            shot_id: String::new(),
            shot_number: shot.shot_number,
            ball: Some(shot.ball),
            club: shot.club,
            estimated: shot.estimated,
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
