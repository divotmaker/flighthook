//! A shot as Pureplay Connect sends it.

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

/// The shot outside strict mode: the launch for a receiver that flies the
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

/// The shot as a measured-only receiver takes it (strict mode): the launch,
/// club and impact as the launch monitor sent them, no flight, and the
/// launch monitor's declared source.
#[must_use]
pub fn measured_shot(shot: &CompletedShot, actor: &str) -> OutputShot {
    OutputShot {
        actor: actor.to_owned(),
        shot_number: shot.key.shot_number,
        shot_id: Some(shot.key.shot_id.clone()),
        launch: shot.launch.clone(),
        club: shot.club.clone(),
        impact: shot.impact.clone(),
        source: shot.source,
        ..OutputShot::default()
    }
}

/// Whether a shot whose launch monitor declares `source` is sent (and the
/// monitor counted for readiness): everything outside strict mode, in strict
/// mode only a measured one.
#[must_use]
pub fn sends(strict: bool, source: Option<Source>) -> bool {
    !strict || source == Some(Source::Measured)
}
