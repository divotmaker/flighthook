//! The software outputs' hooks on the driver host ([`DriverActor`]): what
//! flighthook decides around the Pureplay Connect driver
//! ([`flighthook_pureplay::connect`]).
//!
//! [`PureplayHooks`] keeps flighthook's policy out of the driver:
//!
//! - **What the session sees.** Every bus message goes through the card's
//!   [`Feed`] (routing by the mode of flighthook's turn, as for the Open
//!   Connect client). The session is handed the [`RoutedView`] (the
//!   `bus.actor` of each launch monitor the card stands for in the current
//!   mode, and its removal when it leaves the view), the parts of every shot
//!   of a launch monitor that may feed a simulator (its launch monitor's
//!   messages only; never Auto-chip's flight, never a flight model's), and
//!   a shot's `shot.finished` only when the routing passes it at its finish.
//!   A new session is handed the whole view first. A simulator source (the
//!   Pureplay Accessory card) feeds no simulator output: none of its
//!   messages reach the session.
//! - **The turn the simulator holds.** The session's `golfer.select` (club,
//!   hand and mode, as Pureplay set them) goes on the bus and into [`Held`];
//!   when flighthook's turn then differs from it, unless a game or
//!   simulator card changed it, the card sends its select again
//!   ([`Held::reasserts`]). What is held ends with the session.
//! - **What was sent.** Each shot's BallData comes back from the driver as
//!   a raw frame with the shot's key, after it was written: the card records
//!   it in the send registry ([`SentShots`](crate::state::sent_shots::SentShots))
//!   under Pureplay's configured address, so the Pureplay Accessory card
//!   can tell the shot Pureplay then reports from one of its own, and shows
//!   `shots_sent` (on this link) and `last_shot` (the ShotId and the local
//!   time) on the card.
//!
//! When the card starts it asks the actors already up to republish their
//! state (`bus.sync`), so it learns the launch monitors that started
//! before it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use flighthook::{PureplayConfig, PureplaySection};
use flighthook_pureplay::connect::Connect;
use flightrelay::driver::FrpDriver;
use flightrelay::{BusSync, FrpBody, RawDir};

use super::driver::{DriverActor, HookCtx, Hooks, Pass};
use super::gspro::GsProRouting;
use super::provenance::ShotSource;
use super::simout::{Feed, FeedEvent, Held, RoutedView};
use crate::bus::{BusMsg, LocalEvent};
use crate::state::sent_shots::SimAddr;

/// The Pureplay Connect output card (`pureplay.N`, alpha).
pub type PureplayActor = DriverActor<Connect, PureplayHooks>;

/// flighthook's side of a Pureplay Connect card. See the
/// [module docs](self).
#[derive(Default)]
pub struct PureplayHooks {
    /// Shots, routing and flighthook's turn from the bus, from the card's
    /// first message on.
    feed: Option<Feed>,
    /// What the session was handed of the launch monitors.
    view: RoutedView,
    /// What the session's simulator set for the turn.
    held: Held,
    /// A session is up.
    linked: bool,
    /// Shots written on this link.
    sent: u32,
    /// Selects the card sent again.
    reasserts: u64,
}

/// The card's routing.
fn routing(section: &PureplaySection) -> GsProRouting {
    GsProRouting {
        full_monitor: section.full_monitor.clone(),
        chipping_monitor: section.chipping_monitor.clone(),
        putting_monitor: section.putting_monitor.clone(),
    }
}

/// A shot sent, for the card: its ShotId and the local time.
fn last_shot_text(shot_id: u32) -> String {
    #[cfg(debug_assertions)]
    if let Some(clock) = crate::screens::clock() {
        return format!("#{shot_id} at {clock}");
    }
    format!("#{shot_id} at {}", chrono::Local::now().format("%H:%M:%S"))
}

/// The card's feed, built at its first call (which asks the actors up to
/// republish their state), with the section's routing as it is now.
fn feed<'a>(slot: &'a mut Option<Feed>, ctx: &HookCtx<'_, PureplaySection>) -> &'a mut Feed {
    let feed = slot.get_or_insert_with(|| {
        ctx.sender.frp(BusSync::new());
        let mut feed = Feed::new(routing(ctx.section), Some(Arc::clone(ctx.state)));
        feed.turn = Some((ctx.state.session.turn(), None));
        feed
    });
    feed.routing = routing(ctx.section);
    feed
}

/// Rows for the card, with the status the host last sent.
fn rows(ctx: &HookCtx<'_, PureplaySection>, telemetry: HashMap<String, String>) {
    ctx.sender.local(LocalEvent::CardStatus {
        status: ctx.status,
        telemetry,
    });
}

impl<D: FrpDriver<Config = PureplayConfig>> Hooks<D> for PureplayHooks {
    type Section = PureplaySection;
    const SOURCE: Option<ShotSource> = None;

    fn on_frp(&mut self, body: &mut FrpBody, ctx: &mut HookCtx<'_, PureplaySection>) -> Pass {
        match body {
            FrpBody::GolferSelect(select) => {
                self.held = Held {
                    club: select.club.flatten(),
                    hand: select.handedness,
                    mode: select.mode,
                };
            }
            FrpBody::ShotRaw(frame) if frame.dir == RawDir::Tx => {
                let Some(key) = &frame.key else {
                    return Pass::Send;
                };
                if let Ok(addr) = ctx.section.driver.addr() {
                    ctx.state.sent_shots.record(
                        key.clone(),
                        SimAddr::of(None, addr),
                        Instant::now(),
                    );
                }
                self.sent += 1;
                rows(
                    ctx,
                    HashMap::from([
                        ("shots_sent".into(), self.sent.to_string()),
                        ("last_shot".into(), last_shot_text(self.sent)),
                    ]),
                );
            }
            _ => {}
        }
        Pass::Send
    }

    fn route(&mut self, msg: &BusMsg, ctx: &mut HookCtx<'_, PureplaySection>) {
        let feed = feed(&mut self.feed, ctx);
        match feed.handle(msg) {
            Some(FeedEvent::Part | FeedEvent::Shot(_) | FeedEvent::NoData(_)) => {
                if let Some(frp) = msg.as_frp() {
                    ctx.to_session.push(frp.clone());
                }
            }
            Some(FeedEvent::Turn) => {
                let reasserts = self.linked
                    && feed.turn.as_ref().is_some_and(|(turn, cause)| {
                        self.held
                            .reasserts(turn, cause.as_deref(), |a| ctx.state.is_game_card(a))
                    });
                if reasserts {
                    self.reasserts += 1;
                    ctx.sender
                        .frp(self.held.select(format!("reassert-{}", self.reasserts)));
                }
            }
            Some(FeedEvent::Skipped(_)) | None => {}
        }
        self.view.update(feed, ctx.to_session);
    }

    fn session_started(&mut self, ctx: &mut HookCtx<'_, PureplaySection>) {
        self.linked = true;
        self.sent = 0;
        let feed = feed(&mut self.feed, ctx);
        self.view.clear();
        self.view.update(feed, ctx.to_session);
        rows(ctx, HashMap::from([("shots_sent".into(), "0".into())]));
    }

    fn session_ended(&mut self, _ctx: &mut HookCtx<'_, PureplaySection>) {
        self.linked = false;
        self.held = Held::default();
    }
}

#[cfg(test)]
mod tests;
