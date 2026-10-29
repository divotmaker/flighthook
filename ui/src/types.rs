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
    ConnectionState,
    DerivedFlight,
    // Config types
    Distance,
    DistanceExt,
    FaceImpact,
    FlightPath,
    FlightScopeEnvironment,
    FlighthookConfig,
    FlighthookEvent,
    FlighthookMessage,
    GameStateSnapshot,
    Handedness,
    MevoSection,
    MockMonitorSection,
    OpenConnectClientSection,
    OpenConnectServerSection,
    PlayerInfo,
    PostSettingsResponse,
    R10Section,
    RECONNECT_ALERT_MS,
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
/// then populated as `BallFlight`, `ClubPath` and `FaceImpact` events
/// arrive.
#[derive(Debug, Clone)]
pub struct ShotRow {
    pub actor: String,
    pub shot_id: String,
    pub shot_number: u32,
    pub ball: Option<BallFlight>,
    pub club: Option<ClubData>,
    /// Where the ball met the club face (golfer-relative, never mirrored).
    pub impact: Option<FaceImpact>,
    /// Simulated values for fields the device did not report (beta).
    pub derived: Option<DerivedFlight>,
    /// Club in play when the shot was triggered.
    pub club_selected: Option<Club>,
    /// Player handedness when the shot was triggered.
    pub handed: Option<Handedness>,
    /// Id of the shot group the shot joined (`ShotGroupInfo::id`).
    pub group: Option<String>,
    /// The shot's `shot_finished` arrived. Always true for a row loaded
    /// from `/api/shots`.
    pub finished: bool,
}

impl ShotRow {
    /// Convert all distance and velocity fields to the given unit system.
    pub fn to_unit_system(&self, system: UnitSystem) -> ShotRow {
        // Delegate to ShotData conversion for the populated fields. Face
        // impact is millimetres in every unit system and is kept as it is.
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
            impact: self.impact.clone(),
            derived: converted.derived,
            club_selected: self.club_selected,
            handed: self.handed,
            group: self.group.clone(),
            finished: self.finished,
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
            impact: shot.impact,
            derived: shot.derived,
            club_selected: shot.club_selected,
            handed: shot.handed,
            group: shot.group,
            finished: true,
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
