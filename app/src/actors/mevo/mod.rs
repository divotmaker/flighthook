//! The FlightScope card (Mevo+, Mevo Gen2, Xi): ironsight's device driver
//! ([`Mevo`]) on [`DriverActor`].
//!
//! The driver owns the device: it connects over TCP, runs the handshake,
//! configures and arms the device for the detected model in the detection
//! mode in effect, follows mode changes (a reconfigure and re-arm when the
//! change reaches the device), applies the Fusion camera config after the
//! camera's warmup, and reports readiness, tilt, roll, battery, shots and
//! the frames of each shot. The ball goes out as soon as the device's full
//! result arrives. The Xi's spin source is applied live, without a new
//! session; any other setting change takes a new session (a restart).
//!
//! The card adds its section ([`MevoSection`]), its shot source and the
//! **launch-only spin estimate** (beta): when the device reports a launch
//! with speed and launch angle but no spin and sends no flight result (very
//! short chips on the Xi), the card publishes its own flight result just
//! before the shot's `shot.finished`: `basis: modeled`, model
//! [`LAUNCH_ONLY_MODEL`], with only `assumed` holding a backspin of
//! `launch_only_spin` rpm per mph of ball speed and no sidespin. The launch
//! itself carries only what the device measured. A flight model and the
//! simulator outputs take the assumed spin as the launch's
//! (`CompletedShot::launch_for_flight`). Not for a shot whose mode at the
//! trigger was Putting, a launch under [`LAUNCH_ONLY_MIN_LAUNCH_DEG`] (it
//! rolls as a ground ball), or a `launch_only_spin` of 0.

use flighthook::{AssumedLaunch, Basis, FlightResult, Launch, MevoSection, Mode};
use flightrelay::{FrpBody, ShotFlight};
use ironsight::driver::Mevo;
use ironsight::driver::convert::LAUNCH_ONLY_MIN_LAUNCH_DEG;
use tracing::info;

use crate::actors::driver::{DriverActor, HookCtx, Hooks, ShotSoFar};
use crate::actors::provenance::ShotSource;

/// The model named on the card's launch-only spin estimate.
pub const LAUNCH_ONLY_MODEL: &str = "flighthook launch-only spin";

/// The FlightScope card: the launch-only spin estimate.
#[derive(Debug, Default)]
pub struct MevoHooks;

impl Hooks<Mevo> for MevoHooks {
    type Section = MevoSection;
    const SOURCE: Option<ShotSource> = Some(ShotSource::Mevo);

    fn before_finished(
        &mut self,
        shot: &ShotSoFar,
        ctx: &mut HookCtx<'_, MevoSection>,
    ) -> Vec<FrpBody> {
        estimate(shot, ctx.section).into_iter().collect()
    }
}

/// The card's launch-only spin estimate for `shot`, as its own flight
/// result, when it applies.
fn estimate(shot: &ShotSoFar, section: &MevoSection) -> Option<FrpBody> {
    if shot.flight.is_some() {
        return None;
    }
    let rpm_per_mph = section.launch_only_spin_rpm_per_mph();
    let back = launch_only_spin(shot.launch.as_ref()?, rpm_per_mph, shot.mode)?;
    info!(
        "shot #{}: launch only, estimated backspin {back} rpm ({rpm_per_mph} rpm per mph)",
        shot.key.shot_number,
    );
    Some(
        ShotFlight {
            key: shot.key.clone(),
            flight: FlightResult {
                model: Some(LAUNCH_ONLY_MODEL.into()),
                assumed: Some(AssumedLaunch {
                    backspin_rpm: Some(back),
                    sidespin_rpm: Some(0),
                    ..AssumedLaunch::default()
                }),
                ..FlightResult::new(Basis::Modeled)
            },
        }
        .into(),
    )
}

/// The estimated backspin (rpm) for `launch`, a launch the device sent no
/// flight result for: `rpm_per_mph` times its ball speed in mph, rounded.
/// `None` (no estimate) when `rpm_per_mph` is not above 0, `mode` (the mode
/// at the trigger) is Putting, the launch has any spin, its launch angle is
/// under [`LAUNCH_ONLY_MIN_LAUNCH_DEG`] or unknown, or its speed is not
/// positive or the product is out of range for an rpm.
fn launch_only_spin(launch: &Launch, rpm_per_mph: f64, mode: Option<Mode>) -> Option<i32> {
    if rpm_per_mph <= 0.0 || mode == Some(Mode::Putting) {
        return None;
    }
    if launch.backspin_rpm.unwrap_or(0) != 0
        || launch.sidespin_rpm.unwrap_or(0) != 0
        || launch.total_spin_rpm.unwrap_or(0) != 0
    {
        return None;
    }
    if !launch
        .launch_elevation
        .is_some_and(|vla| vla >= LAUNCH_ONLY_MIN_LAUNCH_DEG)
    {
        return None;
    }
    let mph = launch.launch_speed?.as_mph();
    if !(mph.is_finite() && mph > 0.0) {
        return None;
    }
    let rpm = (rpm_per_mph * mph).round();
    if rpm > f64::from(i32::MAX) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)] // 0 to i32::MAX, checked above
    Some(rpm as i32)
}

/// The FlightScope actor (`mevo.N`).
pub type MevoActor = DriverActor<Mevo, MevoHooks>;

#[cfg(test)]
use ironsight as iron;
#[cfg(test)]
mod scenario;

#[cfg(test)]
mod tests;
