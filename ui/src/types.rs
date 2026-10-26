//! API response types and bus message types.
//!
//! Both are re-exported from the `flighthook` lib crate, which holds the types
//! shared between the app and the UI.

// ---------------------------------------------------------------------------
// Re-exports from the lib crate — shared with the app crate
// ---------------------------------------------------------------------------

pub use flighthook::{
    // Domain types
    ActorStatus,
    // API types
    ActorStatusResponse,
    BallFlight,
    CameraMode,
    // Global state types
    Club,
    ClubData,
    ClubInfo,
    DerivedFlight,
    // Config types
    Distance,
    DistanceExt,
    FlightPath,
    FlightScopeEnvironment,
    FlighthookConfig,
    FlighthookEvent,
    FlighthookMessage,
    GameStateSnapshot,
    GsProSection,
    Handedness,
    MevoSection,
    MockMonitorSection,
    OpenConnectServerSection,
    PlayerInfo,
    PostSettingsResponse,
    R10Section,
    RandomClubSection,
    Severity,
    ShotData,
    ShotDetectionMode,
    ShotGroupInfo,
    ShotGrouping,
    ShotGroupsResponse,
    ShotKey,
    SimSurface,
    SimulationSection,
    SpinSource,
    SquareSection,
    Stats,
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
    /// Simulated values for fields the device did not report (beta).
    pub derived: Option<DerivedFlight>,
    /// Club in play when the shot was triggered.
    pub club_selected: Option<Club>,
    /// Id of the shot group the shot joined (`ShotGroupInfo::id`).
    pub group: Option<String>,
}

impl ShotRow {
    /// Convert all distance and velocity fields to the given unit system.
    pub fn to_unit_system(&self, system: UnitSystem) -> ShotRow {
        // Delegate to ShotData conversion for the populated fields
        let tmp = ShotData {
            ball: self.ball.clone(),
            club: self.club.clone(),
            derived: self.derived.clone(),
            ..ShotData::default()
        };
        let converted = tmp.to_unit_system(system);
        ShotRow {
            actor: self.actor.clone(),
            shot_id: self.shot_id.clone(),
            shot_number: self.shot_number,
            ball: converted.ball,
            club: converted.club,
            derived: converted.derived,
            club_selected: self.club_selected,
            group: self.group.clone(),
        }
    }
}

impl From<ShotData> for ShotRow {
    fn from(shot: ShotData) -> Self {
        Self {
            actor: shot.actor,
            shot_id: shot.shot_id.unwrap_or_default(),
            shot_number: shot.shot_number,
            ball: shot.ball,
            club: shot.club,
            derived: shot.derived,
            club_selected: shot.club_selected,
            group: shot.group,
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
