//! The software outputs' hooks on the driver host ([`DriverActor`]): what
//! flighthook decides around the Open Connect client
//! ([`flighthook_openconnect::client`]) and the Pureplay Connect driver
//! ([`flighthook_pureplay::connect`]).
//!
//! [`OutputHooks`] keeps flighthook's policy out of the drivers:
//!
//! - **What the session sees.** Every bus message goes through the card's
//!   [`Feed`] (routing by the mode of flighthook's turn). The session is
//!   handed the [`RoutedView`] (the `bus.actor` of each launch monitor the
//!   card stands for in the current mode, and its removal when it leaves the
//!   view), the parts of every shot of a launch monitor that may feed a
//!   simulator (its launch monitor's messages only; never Auto-chip's
//!   flight, never a flight model's), and a shot's `shot.finished` only when
//!   the routing passes it at its finish. Auto-chip's shots pass whatever
//!   the routing. A new session is handed the whole view first. A simulator
//!   source (the Pureplay Accessory card) feeds no simulator output: none of
//!   its messages reach the session.
//! - **The turn the simulator holds.** The session's `golfer.select` goes
//!   on the bus and into [`Held`]: Pureplay sets club, hand and mode; the
//!   Open Connect client's select carries club and hand, and the card adds
//!   the mode its club lists give for the club
//!   ([`OpenConnectClientSection::club_mode`]), recomputed from the live
//!   lists on every turn. When flighthook's turn then differs from what is
//!   held, unless a game or simulator card changed it, the card sends its
//!   select again ([`Held::reasserts`]). What is held ends with the session.
//! - **What was sent.** Each shot's frame comes back from the driver as a
//!   raw frame with the shot's key, after it was written: the card records
//!   it in the send registry ([`SentShots`](crate::state::sent_shots::SentShots))
//!   under the simulator's configured address, so the Pureplay Accessory
//!   card can tell the shot Pureplay then reports from one of its own, and
//!   shows `shots_sent` (on this link) and `last_shot` (the number the
//!   simulator got and the local time) on the card.
//!
//! When the card starts it asks the actors already up to republish their
//! state (`bus.sync`), so it learns the launch monitors that started
//! before it. An Open Connect card that takes putts from any monitor while
//! an ExPutt is configured logs a warning: a putt another device also sees
//! reaches the simulator twice.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use flighthook::{Club, DeviceSection, Mode, OpenConnectClientSection, PureplaySection};
use flighthook_openconnect::client::{Client, api::GsProMessage};
use flighthook_pureplay::connect::Connect;
use flightrelay::driver::FrpDriver;
use flightrelay::{BusSync, FrpBody, RawDir, RawFrame};

use super::driver::{DriverActor, HookCtx, Hooks, Pass};
use super::provenance::ShotSource;
use super::simout::{Feed, FeedEvent, Held, RoutedView, Routing};
use crate::bus::{BusMsg, LocalEvent};
use crate::state::sent_shots::SimAddr;

/// The Pureplay Connect output card (`pureplay.N`, alpha).
pub type PureplayActor = DriverActor<Connect, PureplayHooks>;

/// The Open Connect client card (`openconnect_client.N`).
pub type OpenConnectClientActor = DriverActor<Client, OpenConnectClientHooks>;

/// flighthook's side of a Pureplay Connect card.
pub type PureplayHooks = OutputHooks<PureplaySection>;

/// flighthook's side of an Open Connect client card.
pub type OpenConnectClientHooks = OutputHooks<OpenConnectClientSection>;

/// What the output hooks read from a simulator output's section.
pub trait OutputSection: DeviceSection {
    /// The card's routing.
    fn routing(&self) -> Routing;

    /// The simulator's configured address, which its sends are recorded
    /// under.
    fn sim_addr(&self) -> Option<SocketAddr>;

    /// The mode the card holds for the simulator's `club`, given the mode
    /// the session's select carried (`sent`).
    fn held_mode(&self, club: Option<Club>, sent: Option<Mode>) -> Option<Mode>;

    /// The number the simulator got for the shot written as `frame`, the
    /// `sent`-th on this link.
    fn shot_number(frame: &RawFrame, sent: u32) -> u32;

    /// Why the routing may send a putt twice, logged at start.
    fn routing_warning(&self, _config: &flighthook::FlighthookConfig) -> Option<&'static str> {
        None
    }
}

impl OutputSection for PureplaySection {
    fn routing(&self) -> Routing {
        Routing {
            full_monitor: self.full_monitor.clone(),
            chipping_monitor: self.chipping_monitor.clone(),
            putting_monitor: self.putting_monitor.clone(),
        }
    }

    fn sim_addr(&self) -> Option<SocketAddr> {
        self.driver.addr().ok()
    }

    /// The mode Pureplay set.
    fn held_mode(&self, _club: Option<Club>, sent: Option<Mode>) -> Option<Mode> {
        sent
    }

    /// The ShotId: 1, 2, ... on each link.
    fn shot_number(_frame: &RawFrame, sent: u32) -> u32 {
        sent
    }
}

impl OutputSection for OpenConnectClientSection {
    fn routing(&self) -> Routing {
        Routing {
            full_monitor: self.full_monitor.clone(),
            chipping_monitor: self.chipping_monitor.clone(),
            putting_monitor: self.putting_monitor.clone(),
        }
    }

    fn sim_addr(&self) -> Option<SocketAddr> {
        self.driver.addr().ok()
    }

    /// The mode the club lists give for the held club; none without one.
    fn held_mode(&self, club: Option<Club>, _sent: Option<Mode>) -> Option<Mode> {
        club.and_then(|c| self.club_mode(Some(c)))
    }

    /// The frame's `ShotNumber`.
    fn shot_number(frame: &RawFrame, sent: u32) -> u32 {
        serde_json::from_slice::<GsProMessage>(&frame.data).map_or(sent, |m| m.shot_number)
    }

    fn routing_warning(&self, config: &flighthook::FlighthookConfig) -> Option<&'static str> {
        putting_routing_warning(config, &self.routing())
    }
}

/// Why the routing may send unwanted putts: an ExPutt is configured but the
/// Putting Monitor accepts any device, so a putt another device also sees
/// reaches the simulator twice.
pub(crate) fn putting_routing_warning(
    config: &flighthook::FlighthookConfig,
    routing: &Routing,
) -> Option<&'static str> {
    (!config.exputt.is_empty() && routing.putting_monitor.is_none()).then_some(
        "an ExPutt is configured but the Putting Monitor is Any, so putts from every \
         device are sent; set the Putting Monitor to the ExPutt",
    )
}

/// flighthook's side of a simulator output card with section `S`. See the
/// [module docs](self).
pub struct OutputHooks<S> {
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
    _section: std::marker::PhantomData<fn() -> S>,
}

impl<S> Default for OutputHooks<S> {
    fn default() -> Self {
        Self {
            feed: None,
            view: RoutedView::default(),
            held: Held::default(),
            linked: false,
            sent: 0,
            reasserts: 0,
            _section: std::marker::PhantomData,
        }
    }
}

/// A shot sent, for the card: its number and the local time.
fn last_shot_text(number: u32) -> String {
    #[cfg(debug_assertions)]
    if let Some(clock) = crate::screens::clock() {
        return format!("#{number} at {clock}");
    }
    format!("#{number} at {}", chrono::Local::now().format("%H:%M:%S"))
}

/// The card's feed, built at its first call (which asks the actors up to
/// republish their state), with the section's routing as it is now.
fn feed<'a, S: OutputSection>(slot: &'a mut Option<Feed>, ctx: &HookCtx<'_, S>) -> &'a mut Feed {
    let feed = slot.get_or_insert_with(|| {
        ctx.sender.frp(BusSync::new());
        if let Some(warning) = ctx.section.routing_warning(&ctx.state.system.snapshot()) {
            tracing::warn!("{}: {warning}", ctx.sender.actor_id());
        }
        let mut feed = Feed::new(ctx.section.routing(), Some(Arc::clone(ctx.state)));
        feed.turn = Some((ctx.state.session.turn(), None));
        feed
    });
    feed.routing = ctx.section.routing();
    feed
}

/// Rows for the card, with the status the host last sent.
fn rows<S>(ctx: &HookCtx<'_, S>, telemetry: HashMap<String, String>) {
    ctx.sender.local(LocalEvent::CardStatus {
        status: ctx.status,
        telemetry,
    });
}

impl<S, D> Hooks<D> for OutputHooks<S>
where
    S: OutputSection,
    D: FrpDriver<Config = S::Driver>,
{
    type Section = S;
    const SOURCE: Option<ShotSource> = None;

    fn on_frp(&mut self, body: &mut FrpBody, ctx: &mut HookCtx<'_, S>) -> Pass {
        match body {
            FrpBody::GolferSelect(select) => {
                let club = select.club.flatten();
                self.held = Held {
                    club,
                    hand: select.handedness,
                    mode: ctx.section.held_mode(club, select.mode),
                };
                select.mode = self.held.mode;
            }
            FrpBody::ShotRaw(frame) if frame.dir == RawDir::Tx => {
                let Some(key) = &frame.key else {
                    return Pass::Send;
                };
                if let Some(addr) = ctx.section.sim_addr() {
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
                        (
                            "last_shot".into(),
                            last_shot_text(S::shot_number(frame, self.sent)),
                        ),
                    ]),
                );
            }
            _ => {}
        }
        Pass::Send
    }

    fn route(&mut self, msg: &BusMsg, ctx: &mut HookCtx<'_, S>) {
        let feed = feed(&mut self.feed, ctx);
        match feed.handle(msg) {
            Some(FeedEvent::Part | FeedEvent::Finished) => {
                if let Some(frp) = msg.as_frp() {
                    ctx.to_session.push(frp.clone());
                }
            }
            Some(FeedEvent::Turn) => {
                self.held.mode = ctx.section.held_mode(self.held.club, self.held.mode);
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
            None => {}
        }
        self.view.update(feed, ctx.to_session);
    }

    fn session_started(&mut self, ctx: &mut HookCtx<'_, S>) {
        self.linked = true;
        self.sent = 0;
        let feed = feed(&mut self.feed, ctx);
        self.view.clear();
        self.view.update(feed, ctx.to_session);
        rows(ctx, HashMap::from([("shots_sent".into(), "0".into())]));
    }

    fn session_ended(&mut self, _ctx: &mut HookCtx<'_, S>) {
        self.linked = false;
        self.held = Held::default();
    }
}

#[cfg(test)]
mod tests;
