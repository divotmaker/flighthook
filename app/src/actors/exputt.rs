//! The ExPutt RG card: threeputt's device driver ([`ExPutt`]) on
//! [`DriverActor`].
//!
//! The driver owns the device: it finds the ExPutt on the local network (or
//! reaches the IP the card's `address` names), starts measuring with the
//! player's hand, turns every putt into a putter shot (ball speed and launch
//! direction, putter path and face when tracked), follows hand changes with
//! a new round, and reports readiness and the device state. It also keeps a
//! device that stalls from being torn down too early or hammered with new
//! connections: the silence limits, the spacing of mat searches, the wait
//! after a stall, quick retries after a measuring session, and one "not
//! answering" alert per outage. Putts are simulated in putting mode
//! whatever the game's mode is (the driver's fixed mode), and each shot's
//! trigger carries the time the ball was struck, which the cameras use.
//!
//! The card adds its section ([`ExPuttSection`]), its shot source, the
//! Find mat button (a `find_mat` for this card asks the driver to locate the
//! mat again) and the card's words for the discovery hint. The device takes
//! one connection at a time, so a new instance waits for the old one to
//! close (the driver is exclusive).

use flighthook::ExPuttSection;
use flightrelay::FrpBody;
use threeputt::driver::{Action, ExPutt};

use crate::actors::driver::{DriverActor, HookCtx, Hooks, Pass};
use crate::actors::provenance::ShotSource;
use crate::bus::{BusMsg, LocalEvent};

/// The alert after 30 s of discovery without the device, in the card's
/// words: the address is the card's IP Address field.
const DISCOVERY_HINT: &str = "ExPutt not found on this network. If it is on another network, \
     or a firewall blocks discovery, enter its IP address on the device card.";

/// The ExPutt card: the Find mat button and the discovery hint.
#[derive(Debug, Default)]
pub struct ExPuttHooks;

impl Hooks<ExPutt> for ExPuttHooks {
    type Section = ExPuttSection;
    const SOURCE: Option<ShotSource> = Some(ShotSource::ExPutt);

    fn on_frp(&mut self, body: &mut FrpBody, _ctx: &mut HookCtx<'_, ExPuttSection>) -> Pass {
        card_words(body);
        Pass::Send
    }

    fn action(msg: &BusMsg, card: &str) -> Option<Action> {
        find_mat(msg, card)
    }
}

/// The driver's discovery hint in the card's words ([`DISCOVERY_HINT`]).
fn card_words(body: &mut FrpBody) {
    if let FrpBody::BusAlert(alert) = body
        && alert.message == threeputt::driver::DISCOVERY_HINT
    {
        DISCOVERY_HINT.clone_into(&mut alert.message);
    }
}

/// [`Action::FindMat`] when `msg` is a Find mat for the card `card`.
fn find_mat(msg: &BusMsg, card: &str) -> Option<Action> {
    match msg.as_local() {
        Some((_, LocalEvent::FindMat { target })) if target == card => Some(Action::FindMat),
        _ => None,
    }
}

/// The ExPutt actor (`exputt.N`).
pub type ExPuttActor = DriverActor<ExPutt, ExPuttHooks>;

#[cfg(test)]
mod tests;
