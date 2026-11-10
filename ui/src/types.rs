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
    AutoChipRequest,
    Basis,
    BodyTrackingMode,
    BodyTrackingSection,
    CameraListEntry,
    CameraListResponse,
    CameraMode,
    CameraPosition,
    CameraSection,
    // Global state types
    Club,
    ClubData,
    ClubInfo,
    ConfigLoadError,
    ConnectionState,
    DerivedFlight,
    // Config types
    Distance,
    DistanceExt,
    FaceImpact,
    FindMatRequest,
    FlightPath,
    FlightResult,
    FlightScopeEnvironment,
    FlighthookConfig,
    FlighthookEvent,
    FlighthookMessage,
    GameStateSnapshot,
    HandInfo,
    HandRequest,
    Handedness,
    Launch,
    LivePose,
    LivePosture,
    MevoSection,
    MockMonitorSection,
    OpenConnectClientSection,
    OpenConnectServerSection,
    PlayerInfo,
    PoseTrackFile,
    PostSettingsResponse,
    PureplaySection,
    R10Section,
    RECONNECT_ALERT_MS,
    RandomClubSection,
    ReplaySection,
    RoundState,
    Severity,
    ShotData,
    ShotDetectionMode,
    ShotGroupInfo,
    ShotGrouping,
    ShotGroupsResponse,
    ShotKey,
    ShotVideo,
    SimSurface,
    SimulationSection,
    SpinSource,
    SquareSection,
    Stats,
    StatusResponse,
    SwingMetric,
    SwingPositions,
    SwingReportFile,
    SwingTempo,
    SwingView,
    UnitSystem,
    Velocity,
    WebserverSection,
};

// ---------------------------------------------------------------------------
// ShotRow — incrementally populated shot display row
// ---------------------------------------------------------------------------

/// A shot row in the UI grid. Created on `ShotTrigger` with just the key,
/// then populated as `Launch`, `FlightResult`, `ClubPath` and `FaceImpact`
/// events arrive.
#[derive(Debug, Clone)]
pub struct ShotRow {
    pub actor: String,
    pub shot_id: String,
    pub shot_number: u32,
    /// The measured launch.
    pub launch: Option<Launch>,
    /// The device's flight result (carry, total, apex ...), when it sent one.
    pub flight: Option<FlightResult>,
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
    /// The cameras' clips of the shot, one per camera.
    pub video: Vec<ShotVideo>,
}

impl ShotRow {
    /// Take in a `flight_result` of this shot. A device's goes in
    /// [`flight`](Self::flight), the first one kept. flighthook's own
    /// ([`flighthook::is_flighthook_flight`], added on its FRP output when the
    /// device sent none) repeats the simulated values the row already holds
    /// in `derived`; only its `assumed` launch is taken, for the estimated
    /// spin that the measured launch leaves out, so the row matches the shot
    /// `/api/shots` serves.
    pub fn apply_flight(&mut self, flight: FlightResult) {
        if flighthook::is_flighthook_flight(&flight) {
            if let Some(launch) = self.launch.as_mut() {
                launch.fill_assumed(&flight);
            }
        } else {
            self.flight.get_or_insert(flight);
        }
    }

    /// Whether the device reported launch, flight or club data for the shot.
    pub fn has_data(&self) -> bool {
        self.launch.is_some() || self.flight.is_some() || self.club.is_some()
    }

    /// Convert all distance and velocity fields to the given unit system.
    pub fn to_unit_system(&self, system: UnitSystem) -> ShotRow {
        // Delegate to ShotData conversion for the populated fields. Face
        // impact is millimeters in every unit system and is kept as it is.
        let tmp = ShotData {
            launch: self.launch.clone(),
            flight: self.flight.clone(),
            club: self.club.clone(),
            derived: self.derived.clone(),
            ..ShotData::default()
        };
        let converted = tmp.to_unit_system(system);
        ShotRow {
            actor: self.actor.clone(),
            shot_id: self.shot_id.clone(),
            shot_number: self.shot_number,
            launch: converted.launch,
            flight: converted.flight,
            club: converted.club,
            impact: self.impact.clone(),
            derived: converted.derived,
            club_selected: self.club_selected,
            handed: self.handed,
            group: self.group.clone(),
            finished: self.finished,
            video: self.video.clone(),
        }
    }
}

#[cfg(test)]
impl ShotRow {
    /// The row's launch, an empty one put in when there is none.
    pub fn launch_mut(&mut self) -> &mut Launch {
        self.launch.get_or_insert_with(Launch::default)
    }

    /// The row's device flight result, an empty modeled one put in when
    /// there is none.
    pub fn flight_mut(&mut self) -> &mut FlightResult {
        self.flight
            .get_or_insert_with(|| FlightResult::new(Basis::Modeled))
    }
}

impl From<ShotData> for ShotRow {
    fn from(shot: ShotData) -> Self {
        Self {
            actor: shot.actor,
            shot_id: shot.shot_id.unwrap_or_default(),
            shot_number: shot.shot_number,
            launch: shot.launch,
            flight: shot.flight,
            club: shot.club,
            impact: shot.impact,
            derived: shot.derived,
            club_selected: shot.club_selected,
            handed: shot.handed,
            group: shot.group,
            finished: true,
            video: shot.video,
        }
    }
}

/// Stamp `video`, one camera's clip, on every row whose shot is in `keys`
/// (by shot id, whatever device the row is from), replacing that camera's
/// earlier clip. Returns whether a row changed.
pub fn stamp_video(rows: &mut [ShotRow], keys: &[ShotKey], video: &ShotVideo) -> bool {
    let mut changed = false;
    for row in rows
        .iter_mut()
        .filter(|r| !r.shot_id.is_empty() && keys.iter().any(|k| k.shot_id == r.shot_id))
    {
        row.video.retain(|v| v.camera != video.camera);
        row.video.push(video.clone());
        row.video.sort_by(|a, b| a.camera.cmp(&b.camera));
        changed = true;
    }
    changed
}

/// Mark camera `camera`'s clip of the rows whose shot is in `keys` as body
/// tracked with the camera turned by `rotation` degrees: its keypoints can
/// be fetched. Returns whether a row changed.
pub fn mark_pose(rows: &mut [ShotRow], keys: &[ShotKey], camera: &str, rotation: u16) -> bool {
    let mut changed = mark_video(rows, keys, camera, |v| &mut v.pose);
    for row in rows
        .iter_mut()
        .filter(|r| !r.shot_id.is_empty() && keys.iter().any(|k| k.shot_id == r.shot_id))
    {
        for video in row.video.iter_mut().filter(|v| v.camera == camera) {
            changed |= video.rotation != rotation;
            video.rotation = rotation;
        }
    }
    changed
}

/// Forget camera `camera`'s body tracking and swing analysis of shot
/// `shot_id`'s rows: its clip is being tracked again. Returns whether a row
/// changed.
pub fn forget_analysis(rows: &mut [ShotRow], shot_id: &str, camera: &str) -> bool {
    let mut changed = false;
    for row in rows
        .iter_mut()
        .filter(|r| !r.shot_id.is_empty() && r.shot_id == shot_id)
    {
        for video in row.video.iter_mut().filter(|v| v.camera == camera) {
            changed |= video.pose || video.swing;
            video.pose = false;
            video.swing = false;
        }
    }
    changed
}

/// Mark camera `camera`'s clip of the rows whose shot is in `keys` as swing
/// analyzed: its report can be fetched. Returns whether a row changed.
pub fn mark_swing(rows: &mut [ShotRow], keys: &[ShotKey], camera: &str) -> bool {
    mark_video(rows, keys, camera, |v| &mut v.swing)
}

/// Set the flag `flag` picks on camera `camera`'s clip of the rows whose
/// shot is in `keys`. Returns whether a row changed.
fn mark_video(
    rows: &mut [ShotRow],
    keys: &[ShotKey],
    camera: &str,
    flag: fn(&mut ShotVideo) -> &mut bool,
) -> bool {
    let mut changed = false;
    for row in rows
        .iter_mut()
        .filter(|r| !r.shot_id.is_empty() && keys.iter().any(|k| k.shot_id == r.shot_id))
    {
        for video in row.video.iter_mut().filter(|v| v.camera == camera) {
            let f = flag(video);
            changed |= !*f;
            *f = true;
        }
    }
    changed
}

/// Drop every clip from the rows whose shot is in `keys`. Returns whether a
/// row changed.
pub fn remove_video(rows: &mut [ShotRow], keys: &[ShotKey]) -> bool {
    let mut changed = false;
    for row in rows
        .iter_mut()
        .filter(|r| !r.video.is_empty() && keys.iter().any(|k| k.shot_id == r.shot_id))
    {
        row.video.clear();
        changed = true;
    }
    changed
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
