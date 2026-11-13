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
    BlockInfo,
    BlocksResponse,
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
    ClubExt,
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
    Handedness,
    Launch,
    LivePose,
    LivePosture,
    MevoSection,
    MockMonitorSection,
    Mode,
    OpenConnectClientSection,
    OpenConnectServerSection,
    PoseTrackFile,
    PostSettingsResponse,
    PureplayAccessorySection,
    PureplaySection,
    R10Section,
    RECONNECT_ALERT_MS,
    RandomClubSection,
    ReplaySection,
    RoundState,
    Severity,
    ShotData,
    ShotGrouping,
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
    /// Id of the practice block (set) the shot belongs to.
    pub group: Option<String>,
    /// The shot's `shot_finished` arrived. Always true for a row loaded
    /// from `/api/shots`.
    pub finished: bool,
    /// The cameras' clips of the shot, one per camera.
    pub video: Vec<ShotVideo>,
    /// The turn's golfer id when the shot was triggered, as the turn named
    /// it (resolve it with the lookup rule).
    pub golfer: Option<String>,
}

impl ShotRow {
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
            flights: self
                .flight
                .iter()
                .map(|flight| flighthook::Flight {
                    from: String::new(),
                    flight: flight.clone(),
                })
                .collect(),
            club: self.club.clone(),
            ..ShotData::default()
        };
        let converted = tmp.to_unit_system(system);
        let derived = self.derived.as_ref().map(|d| match system {
            UnitSystem::Imperial => d.to_imperial(),
            UnitSystem::Metric => d.to_metric(),
        });
        ShotRow {
            golfer: self.golfer.clone(),
            actor: self.actor.clone(),
            shot_id: self.shot_id.clone(),
            shot_number: self.shot_number,
            launch: converted.launch,
            flight: converted.flights.into_iter().next().map(|f| f.flight),
            club: converted.club,
            impact: self.impact.clone(),
            derived,
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

/// A stored shot as a row: what [`ShotData::shown`] composes, under the
/// card key (else the launch monitor's actor id).
impl From<ShotData> for ShotRow {
    fn from(shot: ShotData) -> Self {
        let shown = shot.shown();
        let club_selected = shot.club_selected();
        let handed = shot.handed();
        let golfer = shot
            .turn
            .as_ref()
            .and_then(|t| t.golfer.as_ref())
            .map(|g| g.as_str().to_owned());
        Self {
            golfer,
            actor: shot.card_key().to_owned(),
            shot_id: shot.shot_id.unwrap_or_default(),
            shot_number: shot.shot_number,
            launch: shown.launch,
            flight: shown.flight,
            club: shot.club,
            impact: shot.impact,
            derived: shown.derived,
            club_selected,
            handed,
            group: shot.block,
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

/// Log entry — constructed from the UI socket's messages.
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
