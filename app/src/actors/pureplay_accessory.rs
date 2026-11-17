//! The Pureplay Accessory card (beta, `pureplay_accessory.N`): the
//! Accessory driver ([`flighthook_pureplay::accessory`]) on the driver host
//! ([`DriverActor`]), with flighthook's policy in its hooks.
//!
//! The driver pairs with Pureplay, follows the round, publishes it as the
//! bus's `game` (the `pureplay` golfer namespace), sends the turn of the
//! local player up as a complete `golfer.select`, sends a club picked in
//! flighthook to Pureplay, and reports every local Pureplay shot as the
//! card's own and Pureplay's result as `game.stroke`. The hooks decide:
//!
//! - **What the driver is handed.** flighthook's turns, except one another
//!   game or simulator card caused (the card's own pass); the replies to
//!   `golfer.select`s; a `golfer.select` carrying a club from an actor that
//!   is neither this card nor a game or simulator card (a pick); `bus.sync`;
//!   `game.aim` from any other actor. A new link is handed flighthook's
//!   current turn first.
//! - **The Shots setting** ([`PureplayShots`]), at each shot's
//!   `shot.trigger`: **Auto** drops a shot that echoes one a simulator
//!   output sent (the send registry,
//!   [`SentShots::take_echo`](crate::state::sent_shots::SentShots), matched
//!   against the card's configured address) and takes any other; **From
//!   Pureplay** takes every shot; **From flighthook** drops every shot, an
//!   echo still matched. A dropped shot puts nothing on the bus; Pureplay's
//!   `game.stroke` for it carries the echoed shot's `shot_id`, or none.
//! - **The card's rows** `shots` (the setting, and in Auto where the last
//!   shot came from) and `last_shot` (the shot the card took last and when).

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use flighthook::{PureplayAccessoryConfig, PureplayAccessorySection, PureplayShots, ShotKey};
use flighthook_pureplay::accessory::Accessory;
use flighthook_pureplay::accessory::SHOTS_KEPT;
use flightrelay::driver::FrpDriver;
use flightrelay::{FrpBody, FrpMessage};

use super::driver::{DriverActor, HookCtx, Hooks, Pass};
use super::provenance::ShotSource;
use crate::bus::{BusMsg, LocalEvent};
use crate::state::sent_shots::SimAddr;

/// The Pureplay Accessory card.
pub type PureplayAccessoryActor = DriverActor<Accessory, PureplayAccessoryHooks>;

/// What becomes of one of the card's shots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The card takes it: a flighthook shot of its own.
    Take,
    /// It echoes flighthook's shot `ShotKey`: not taken; its result goes
    /// to that shot.
    Echo(ShotKey),
    /// Not taken, and no shot of flighthook's: "From flighthook" with no
    /// matching send.
    Ignore,
}

/// What becomes of a shot in mode `shots`. `echo` finds (and consumes) the
/// send the shot echoes; it is asked only when the mode matches echoes
/// ("From Pureplay" takes every shot as the card's own).
pub fn decide(shots: PureplayShots, echo: impl FnOnce() -> Option<ShotKey>) -> Decision {
    if shots == PureplayShots::FromPureplay {
        return Decision::Take;
    }
    match echo() {
        Some(key) => Decision::Echo(key),
        None if shots == PureplayShots::FromFlighthook => Decision::Ignore,
        None => Decision::Take,
    }
}

/// The `shots` row: the setting, and in Auto where the last shot came from
/// (`Some(true)`: Pureplay).
fn shots_text(shots: PureplayShots, last_from_pureplay: Option<bool>) -> String {
    match shots {
        PureplayShots::Auto => match last_from_pureplay {
            Some(true) => "auto: last from Pureplay".into(),
            Some(false) => "auto: last from flighthook".into(),
            None => "auto".into(),
        },
        PureplayShots::FromPureplay => "from Pureplay".into(),
        PureplayShots::FromFlighthook => "from flighthook".into(),
    }
}

/// A shot the card took, for the `last_shot` row: its number and the local
/// time.
fn last_shot_text(number: u32) -> String {
    #[cfg(debug_assertions)]
    if let Some(clock) = crate::screens::clock() {
        return format!("#{number} at {clock}");
    }
    format!("#{number} at {}", chrono::Local::now().format("%H:%M:%S"))
}

/// flighthook's side of a Pureplay Accessory card. See the
/// [module docs](self).
#[derive(Debug, Default)]
pub struct PureplayAccessoryHooks {
    /// flighthook's actor id, read at the first call.
    flighthook: Option<String>,
    /// A link is up.
    linked: bool,
    /// The card's dropped shots by shot id, with the shot each echoes
    /// (`None`: none), the latest last.
    dropped: VecDeque<(String, Option<ShotKey>)>,
    /// Where the last shot came from in Auto: `true` for Pureplay, `false`
    /// for flighthook (an echo).
    last_from_pureplay: Option<bool>,
    /// The `shots` row as last sent on this link.
    shots_row: Option<String>,
}

impl PureplayAccessoryHooks {
    /// What becomes of the card's shot `key` under the section's setting:
    /// whether it goes on the bus, and the rows.
    fn trigger(&mut self, key: &ShotKey, ctx: &HookCtx<'_, PureplayAccessorySection>) -> Pass {
        let shots = ctx.section.shots;
        let here = ctx.section.driver.addr().ok().map(|a| SimAddr::of(None, a));
        let decision = decide(shots, || {
            here.and_then(|here| ctx.state.sent_shots.take_echo(here, Instant::now()))
        });
        let card = ctx.sender.actor_id();
        let mut rows = HashMap::new();
        let pass = match decision {
            Decision::Take => {
                tracing::info!("{card}: Pureplay's shot taken as shot #{}", key.shot_number);
                if shots == PureplayShots::Auto {
                    self.last_from_pureplay = Some(true);
                }
                rows.insert("last_shot".into(), last_shot_text(key.shot_number));
                Pass::Send
            }
            Decision::Echo(echo) => {
                tracing::info!(
                    "{card}: Pureplay's shot echoes shot #{} ({}) flighthook sent; not taken",
                    echo.shot_number,
                    echo.shot_id
                );
                if shots == PureplayShots::Auto {
                    self.last_from_pureplay = Some(false);
                }
                self.remember(key, Some(echo));
                Pass::Drop
            }
            Decision::Ignore => {
                tracing::info!(
                    "{card}: Pureplay's shot not taken (shots from flighthook), and no shot \
                     flighthook sent matches it"
                );
                self.remember(key, None);
                Pass::Drop
            }
        };
        self.shots_row(ctx, rows);
        pass
    }

    /// The card's shot `key` was dropped, echoing `echo`.
    fn remember(&mut self, key: &ShotKey, echo: Option<ShotKey>) {
        if self.dropped.len() >= SHOTS_KEPT {
            self.dropped.pop_front();
        }
        self.dropped.push_back((key.shot_id.clone(), echo));
    }

    /// Send `rows` with the `shots` row when it changed, while a link is
    /// up.
    fn shots_row(
        &mut self,
        ctx: &HookCtx<'_, PureplayAccessorySection>,
        mut rows: HashMap<String, String>,
    ) {
        if self.linked {
            let text = shots_text(ctx.section.shots, self.last_from_pureplay);
            if self.shots_row.as_ref() != Some(&text) {
                rows.insert("shots".into(), text.clone());
                self.shots_row = Some(text);
            }
        }
        if !rows.is_empty() {
            ctx.sender.local(LocalEvent::CardStatus {
                status: ctx.status,
                telemetry: rows,
            });
        }
    }

    /// flighthook's actor id.
    fn flighthook<'a>(&'a mut self, ctx: &HookCtx<'_, PureplayAccessorySection>) -> &'a str {
        self.flighthook
            .get_or_insert_with(|| ctx.state.flighthook_id())
    }
}

/// Whether the driver is handed `msg`, given flighthook's actor id
/// `flighthook`, the card's own actor id `own` and which actors are game or
/// simulator cards.
fn handed(
    msg: &BusMsg,
    flighthook: &str,
    own: Option<&str>,
    is_game_card: impl Fn(&str) -> bool,
) -> bool {
    if let Some((_, cause)) = msg.turn_of(flighthook) {
        return cause.is_none_or(|c| Some(c) == own || !is_game_card(c));
    }
    let Some(FrpMessage { from, body }) = msg.as_frp() else {
        return false;
    };
    let other = Some(from.as_str()) != own;
    match body {
        FrpBody::BusReply(reply) => from == flighthook && reply.command == "golfer.select",
        FrpBody::GolferSelect(select) => {
            matches!(select.club, Some(Some(_))) && other && !is_game_card(from)
        }
        FrpBody::BusSync(_) => true,
        FrpBody::GameAim(_) => other,
        _ => false,
    }
}

impl<D> Hooks<D> for PureplayAccessoryHooks
where
    D: FrpDriver<Config = PureplayAccessoryConfig>,
{
    type Section = PureplayAccessorySection;
    const SOURCE: Option<ShotSource> = Some(ShotSource::PureplayAccessory);

    fn on_frp(&mut self, body: &mut FrpBody, ctx: &mut HookCtx<'_, Self::Section>) -> Pass {
        match body {
            FrpBody::ShotTrigger(trigger) => {
                let key = trigger.key.clone();
                self.trigger(&key, ctx)
            }
            FrpBody::GameStroke(stroke) => {
                if let Some(id) = &stroke.shot_id
                    && let Some((_, echo)) = self.dropped.iter().find(|(d, _)| d == id)
                {
                    stroke.shot_id = echo.as_ref().map(|k| k.shot_id.clone());
                }
                Pass::Send
            }
            _ => Pass::Send,
        }
    }

    fn route(&mut self, msg: &BusMsg, ctx: &mut HookCtx<'_, Self::Section>) {
        let flighthook = self.flighthook(ctx).to_owned();
        if handed(msg, &flighthook, ctx.sender.frp_id(), |a| {
            ctx.state.is_game_card(a)
        }) && let Some(frp) = msg.as_frp()
        {
            ctx.to_session.push(frp.clone());
        }
        // The setting changes without a word from Pureplay.
        self.shots_row(ctx, HashMap::new());
    }

    fn session_started(&mut self, ctx: &mut HookCtx<'_, Self::Section>) {
        self.linked = true;
        self.shots_row = None;
        let flighthook = self.flighthook(ctx).to_owned();
        ctx.to_session
            .push(FrpMessage::new(flighthook, ctx.state.session.turn()));
        self.shots_row(ctx, HashMap::new());
    }

    fn session_ended(&mut self, _ctx: &mut HookCtx<'_, Self::Section>) {
        self.linked = false;
    }
}

#[cfg(test)]
mod tests;
