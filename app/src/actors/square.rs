//! The Square Golf Omni card: allsquare's device driver ([`Omni`]) on
//! [`DriverActor`].
//!
//! The driver owns the device: it finds the Omni over BLE (or the one the
//! card's `address` pins), connects without pairing, arms it with the club
//! and hand in play (the card's `club` until the game names one), follows
//! club and hand changes with one club select each, drops zero-spin
//! misreads unless the putter is in play, and reports readiness, device
//! state, battery and shots with the per-club face impact calibration
//! applied. The Omni takes one central at a time, so stopping disarms and
//! disconnects it, and a new instance waits for that (the driver is
//! exclusive). The card adds its section ([`SquareSection`]) and its shot
//! source; it has no processing of its own. Any change of the driver's
//! settings takes a new session (a restart).
//!
//! The original Square / Square Home is not supported: it uses a different
//! club-code scheme.

use allsquare::driver::Omni;
use flighthook::SquareSection;

use crate::actors::driver::{DriverActor, Hooks};
use crate::actors::provenance::ShotSource;

/// The Square Golf card: no processing of its own.
#[derive(Debug, Default)]
pub struct SquareHooks;

impl Hooks<Omni> for SquareHooks {
    type Section = SquareSection;
    const SOURCE: Option<ShotSource> = Some(ShotSource::Square);
    const BATTERY: bool = true;
}

/// The Square Golf actor (`square.N`).
pub type SquareActor = DriverActor<Omni, SquareHooks>;

#[cfg(test)]
mod tests;
