//! A shot as the Open Connect client sends it.

use flightrelay::types::{ClubData, FaceImpact, FlightResult, Launch};
use flightrelay::{CompletedShot, Source};

/// A shot as the session sends it: the launch, the launch monitor's own
/// flight, club and impact, numbered by the launch monitor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputShot {
    /// The launch monitor, for the log.
    pub actor: String,
    pub shot_number: u32,
    pub shot_id: Option<String>,
    pub launch: Option<Launch>,
    /// The launch monitor's own flight result.
    pub flight: Option<FlightResult>,
    pub club: Option<ClubData>,
    pub impact: Option<FaceImpact>,
    /// What stands behind the shot, from the launch monitor's `bus.actor`.
    pub source: Option<Source>,
}

impl OutputShot {
    /// True when the shot carries launch, flight, club or impact data.
    #[must_use]
    pub fn has_data(&self) -> bool {
        self.launch.is_some()
            || self.flight.is_some()
            || self.club.is_some()
            || self.impact.is_some()
    }
}

/// The shot the session sends: the launch for a receiver that flies the
/// ball itself (measured values, filled from the launch monitor's own
/// flight's `assumed`), the launch monitor's own flight (never another
/// actor's), club and impact. `actor` names the launch monitor in the log.
#[must_use]
pub fn output_shot(shot: &CompletedShot, actor: &str) -> OutputShot {
    OutputShot {
        actor: actor.to_owned(),
        shot_number: shot.key.shot_number,
        shot_id: Some(shot.key.shot_id.clone()),
        launch: shot.launch_for_flight(),
        flight: shot.own_flight().cloned(),
        club: shot.club.clone(),
        impact: shot.impact.clone(),
        source: shot.source,
    }
}
