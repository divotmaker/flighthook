//! The Trackball card (alpha): thumbswing's device driver ([`Thumbswing`])
//! on [`DriverActor`].
//!
//! The driver owns the device: it opens the trackball or mouse the card's
//! `device` setting names (or the only one there is) on Linux, turns each
//! swing (pull the ball back, push it forward) into a full shot (launch with
//! both spin pairs, then club data), follows the club, the hand and the
//! putting mode the game names, and reports readiness. Every value is
//! synthesized from the swing, so the card is a virtual launch monitor. When the device is unplugged the
//! session stays up, reports `ready=false` and reopens the device by
//! itself. Off Linux the driver reports that the trackball is not supported
//! there and gives up, so a section copied to another computer shows that
//! error on its card. The tuning, the thresholds and the club apply to
//! the next swing; a change of `device`, `cpi` or `report_limit`
//! restarts the card. The card never grabs the device, so the desktop
//! pointer keeps following the ball, and a new instance does not wait for
//! the old one to let the device go.
//!
//! The card adds its section ([`TrackballSection`]) and its shot source.
//! Its shots cut no camera clip (no impact lead).

use flighthook::TrackballSection;
use thumbswing::driver::Thumbswing;

use crate::actors::driver::{DriverActor, Hooks};
use crate::actors::provenance::ShotSource;

/// The Trackball card: nothing to add to its driver.
#[derive(Debug, Default)]
pub struct TrackballHooks;

impl Hooks<Thumbswing> for TrackballHooks {
    type Section = TrackballSection;
    const SOURCE: Option<ShotSource> = Some(ShotSource::Trackball);
}

/// The Trackball actor (`trackball.N`).
pub type TrackballActor = DriverActor<Thumbswing, TrackballHooks>;

#[cfg(test)]
mod tests;
