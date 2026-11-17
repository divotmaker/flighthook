//! Club out: a club picked in flighthook goes to Pureplay.
//!
//! **Which picks.** A `golfer.select` on the bus carrying a club whose
//! sender is neither this card nor another game or simulator card: the
//! header's picker and the API (the flighthook actor), random club, another
//! program. A simulator output's select (`openconnect_client.*`,
//! `pureplay.*`) is Pureplay's own club coming back; answering it would undo
//! a second club of one type.
//!
//! **When.** Club out is usable ([`State::On`]) with the `control` scope,
//! `club.set` (or `club.next` / `club.prev`) in `hello.capabilities`,
//! Pureplay's "Let Apps Change Aim and Club" switch not known to be off
//! (`controls_disabled`, or `forbidden_scope` on a command), and, while a
//! local player is up, a bag for that player. A pick while it is not
//! usable is not sent, and the card's complete select puts Pureplay's club
//! back. A pick while nobody at this computer is up is not sent and changes
//! nothing.
//!
//! **Sending.** The pick is for the active local player. It resolves to the
//! first entry of its type in that player's bag, at the time it is sent; a
//! type the bag does not hold is not sent. A command goes out only while
//! the readiness is `controllable` and names that player; until then the
//! pick is **pending** ([`PICKS_WAIT`]). A readiness that is `controllable`
//! and names the player but does not list the command drops the pick at
//! once: Pureplay does not take club changes there. When the command could
//! go, Pureplay holding a club of the pick's type already sends nothing (a
//! second sand wedge stays). Every command carries `playerId`. One command
//! is in flight at a time; a pick made meanwhile replaces the pending one,
//! so the latest pick wins.
//!
//! **The pending pick** is dropped on `turn.changed` to another player,
//! `context.invalidated`, a connection's end (and so a new
//! `serverInstanceId`), and the player's own choice in Pureplay
//! (`user_selected`). A `turn.changed` naming the pick's own player (a new
//! hole in a one-player round) keeps it. Pureplay's start-of-turn `auto`
//! change does not drop it. An `auto` change for the shot a `club.set` just
//! succeeded for sends the pick once more, unless a newer pick or the
//! player's own choice came since; a second one is accepted.
//!
//! **Replies.** Success: the reply's club is Pureplay's. `not_ready`: back
//! to pending, sent again after [`NOT_READY_RETRY`] or at Pureplay's next
//! readiness, whichever comes first, at most [`NOT_READY_RETRIES`] times.
//! `rate_limited`: one retry after `retryAfterMs`. Anything else, or no
//! reply within [`COMMAND_TIMEOUT`], drops the pick (`forbidden_scope` also
//! marks the switch off, until a readiness lets apps steer the shot again).
//!
//! **Snap back.** While a pick is pending or in flight, the card neither
//! sends nor compares the club ([`ClubOut::holds_club`]); once the pick is
//! applied, refused or dropped ([`Action::SnapBack`]), its complete select
//! puts Pureplay's club back when flighthook holds another.
//!
//! **Stepping.** Without `club.set`, `club.next` / `club.prev` step toward
//! the type: one step in flight, at most as many steps as the bag holds,
//! each reply naming the club reached. A step that does not move (the
//! reply names the club held before it) turns the other way once, when
//! Pureplay offers that command; a second one gives up.

use std::time::{Duration, Instant};

use flightrelay::Club;
use serde::Deserialize;

use super::model::Model;
use super::protocol::{self, Readiness, Response};

/// Whether a pick Pureplay cannot take yet (the ball in the air, the shot
/// not set up) waits and is sent once Pureplay can take it, ahead of
/// Pureplay's own start-of-turn club. `false` drops such a pick and puts
/// Pureplay's club back.
pub const PICKS_WAIT: bool = true;

/// Longest a command waits for its reply before the pick is dropped.
pub const COMMAND_TIMEOUT: Duration = Duration::from_millis(5_000);

/// After `not_ready`, the pick is sent again this much later, or at
/// Pureplay's next readiness if that comes first.
pub const NOT_READY_RETRY: Duration = Duration::from_millis(1_000);

/// How often a pick answered `not_ready` is sent again; the next
/// `not_ready` drops it.
pub const NOT_READY_RETRIES: u32 = 3;

/// The wait after `rate_limited` when Pureplay names none.
const RATE_LIMIT_WAIT: Duration = Duration::from_millis(100);

/// The commands the card sends.
const SET: &str = "club.set";
const NEXT: &str = "club.next";
const PREV: &str = "club.prev";

/// Whether club out is usable: the card's `club_out` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    On,
    /// The pairing grants no `control`.
    ReadOnly,
    /// Pureplay's "Let Apps Change Aim and Club" is off.
    Off,
    /// Pureplay offers no club command.
    NotSupported,
}

impl State {
    /// The `club_out` row.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::ReadOnly => "read only",
            Self::Off => "off in Pureplay",
            Self::NotSupported => "not supported",
        }
    }
}

/// What the card does after a call.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send this command's JSON.
    Send(String),
    /// A pick ended without Pureplay taking it: the card's complete select
    /// puts Pureplay's club back, when flighthook holds another and no pick
    /// is outstanding.
    SnapBack,
}

/// A step's direction through the bag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Next,
    Prev,
}

impl Dir {
    fn command(self) -> &'static str {
        match self {
            Self::Next => NEXT,
            Self::Prev => PREV,
        }
    }

    fn turned(self) -> Self {
        match self {
            Self::Next => Self::Prev,
            Self::Prev => Self::Next,
        }
    }
}

/// A pick waiting to be sent.
#[derive(Debug, Clone, PartialEq)]
struct Pick {
    club: Club,
    player: i64,
    /// The turn it belongs to ([`ClubOut::epoch`]).
    epoch: u64,
    /// Not sent before (after `rate_limited`).
    not_before: Option<Instant>,
    /// After `not_ready`: sent again at this time, or at the next
    /// readiness if that comes first.
    retry_at: Option<Instant>,
    /// `not_ready` answers so far.
    not_ready: u32,
    /// The one retry after `rate_limited` is used.
    retried: bool,
    /// This is the one re-send after an `auto` change.
    resent: bool,
}

impl Pick {
    fn new(club: Club, player: i64, epoch: u64) -> Self {
        Self {
            club,
            player,
            epoch,
            not_before: None,
            retry_at: None,
            not_ready: 0,
            retried: false,
            resent: false,
        }
    }
}

/// Stepping progress.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Steps {
    dir: Dir,
    taken: usize,
    max: usize,
    /// The club held when the step went out: its id and its type.
    from_id: Option<String>,
    from_kind: Option<String>,
    /// The direction was turned once, after a step that did not move.
    turned: bool,
}

/// Whether two clubs are one: by id when both have one, else by type.
fn same_club(
    a_id: Option<&str>,
    a_kind: Option<&str>,
    b_id: Option<&str>,
    b_kind: Option<&str>,
) -> bool {
    match (a_id, b_id) {
        (Some(a), Some(b)) => a == b,
        _ => a_kind
            .zip(b_kind)
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)),
    }
}

/// The command in flight.
#[derive(Debug, Clone)]
struct Flight {
    id: String,
    pick: Pick,
    steps: Option<Steps>,
    sent_at: Instant,
    /// The readiness `shotId` when it was sent.
    shot_id: Option<String>,
}

/// The last `club.set` (or stepping) that reached its club.
#[derive(Debug, Clone, PartialEq)]
struct Done {
    club: Club,
    player: i64,
    shot_id: Option<String>,
    /// The `auto` re-send is used.
    resent: bool,
}

/// A command reply's data.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Reply {
    player_id: Option<i64>,
    club: Option<String>,
    club_id: Option<String>,
}

/// Club out for one connection.
#[derive(Debug)]
pub struct ClubOut {
    actor: String,
    control: bool,
    set: bool,
    next: bool,
    prev: bool,
    /// `forbidden_scope` came back on a command with `control` granted;
    /// until a readiness lets apps steer the shot again.
    switch_off: bool,
    /// Bumped on `turn.changed` and `context.invalidated`.
    epoch: u64,
    pending: Option<Pick>,
    flight: Option<Flight>,
    done: Option<Done>,
    sent: u32,
}

impl ClubOut {
    #[must_use]
    pub fn new(actor: &str) -> Self {
        Self {
            actor: actor.to_owned(),
            control: false,
            set: false,
            next: false,
            prev: false,
            switch_off: false,
            epoch: 0,
            pending: None,
            flight: None,
            done: None,
            sent: 0,
        }
    }

    /// A connection is paired: what it grants and offers.
    pub fn link(&mut self, scopes: &[String], commands: &[String]) {
        let has = |list: &[String], name: &str| list.iter().any(|c| c.eq_ignore_ascii_case(name));
        self.clear();
        self.control = has(scopes, "control");
        self.set = has(commands, SET);
        self.next = has(commands, NEXT);
        self.prev = has(commands, PREV);
        self.switch_off = false;
    }

    /// The connection ended: every pick goes, nothing is usable.
    pub fn clear(&mut self) {
        if self.pending.is_some() || self.flight.is_some() {
            tracing::info!("{}: club pick dropped with the connection", self.actor);
        }
        self.pending = None;
        self.flight = None;
        self.done = None;
        self.control = false;
        self.set = false;
        self.next = false;
        self.prev = false;
    }

    /// The `club_out` row. A local player who is up with no bag (or an
    /// empty one) has nothing a pick could select: not supported.
    #[must_use]
    pub fn state(&self, model: &Model) -> State {
        if !(self.set || self.next || self.prev) {
            State::NotSupported
        } else if !self.control {
            State::ReadOnly
        } else if self.switch_off
            || model
                .readiness()
                .is_some_and(|r| r.blockers.iter().any(|b| b == "controls_disabled"))
        {
            State::Off
        } else if model
            .active_local()
            .is_some_and(|p| p.bag.as_ref().is_none_or(Vec::is_empty))
        {
            State::NotSupported
        } else {
            State::On
        }
    }

    /// Whether Pureplay offers stepping in `dir`.
    fn offers(&self, dir: Dir) -> bool {
        match dir {
            Dir::Next => self.next,
            Dir::Prev => self.prev,
        }
    }

    /// Whether a pick is pending or in flight for this turn: Pureplay's own
    /// club changes stay off the bus meanwhile.
    #[must_use]
    pub fn holds_club(&self) -> bool {
        self.pending.is_some()
            || self
                .flight
                .as_ref()
                .is_some_and(|f| f.pick.epoch == self.epoch)
    }

    /// flighthook's club became `club` through a pick.
    pub fn pick(&mut self, club: Club, model: &Model, now: Instant) -> Vec<Action> {
        let Some(player) = model.active_local_id() else {
            tracing::info!(
                "{}: club {club} picked while nobody here is up; not sent",
                self.actor
            );
            return Vec::new();
        };
        // A newer pick: an `auto` change no longer re-sends an older one.
        self.done = None;
        let state = self.state(model);
        if state != State::On {
            tracing::info!(
                "{}: club {club} picked, club out {}; Pureplay's club stays",
                self.actor,
                state.text()
            );
            self.pending = None;
            return vec![Action::SnapBack];
        }
        tracing::info!("{}: club {club} picked for player {player}", self.actor);
        self.pending = Some(Pick::new(club, player, self.epoch));
        self.drive(model, now)
    }

    /// Before the model takes `event`: what drops the pending pick, and the
    /// `auto` re-send.
    pub fn before_event(&mut self, event: &protocol::Event) -> Vec<Action> {
        match event.name.as_str() {
            "turn.changed" | "context.invalidated" => self.new_turn(event),
            "player.club_changed" => {
                let Ok(c) = protocol::payload::<protocol::ClubChanged>(event) else {
                    return Vec::new();
                };
                match c.reason.as_deref() {
                    Some("user_selected") => {
                        // The player's own choice: an `auto` change no
                        // longer re-sends the last pick over it.
                        if self
                            .done
                            .as_ref()
                            .is_some_and(|d| Some(d.player) == c.player_id)
                        {
                            self.done = None;
                        }
                        let mine = self
                            .pending
                            .as_ref()
                            .is_some_and(|p| Some(p.player) == c.player_id);
                        if mine {
                            self.pending = None;
                            tracing::info!(
                                "{}: club pick dropped: the player chose in Pureplay",
                                self.actor
                            );
                            return vec![Action::SnapBack];
                        }
                        Vec::new()
                    }
                    Some("auto") => {
                        self.auto_change(&c);
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            "shot.ready_changed" => {
                let readiness = protocol::payload::<Readiness>(event).ok();
                self.readiness_changed(readiness.as_ref());
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// `turn.changed` or `context.invalidated`: a new turn. A pick pending
    /// or in flight for this turn is dropped, except on a `turn.changed`
    /// naming its own player (a new hole in a one-player round), which
    /// keeps it for the new turn. A flight from an earlier turn drops
    /// nothing.
    fn new_turn(&mut self, event: &protocol::Event) -> Vec<Action> {
        let next = if event.name == "turn.changed" {
            protocol::payload::<protocol::TurnChanged>(event)
                .ok()
                .and_then(|t| t.current_player_id)
        } else {
            None
        };
        let old = self.epoch;
        self.epoch += 1;
        self.done = None;
        let live_flight = self.flight.as_ref().filter(|f| f.pick.epoch == old);
        let Some(player) = self
            .pending
            .as_ref()
            .map(|p| p.player)
            .or_else(|| live_flight.map(|f| f.pick.player))
        else {
            return Vec::new();
        };
        if next == Some(player) {
            let epoch = self.epoch;
            if let Some(p) = &mut self.pending {
                p.epoch = epoch;
            }
            if let Some(f) = self.flight.as_mut().filter(|f| f.pick.epoch == old) {
                f.pick.epoch = epoch;
            }
            tracing::info!(
                "{}: club pick kept: player {player} is up again ({})",
                self.actor,
                event.name
            );
            return Vec::new();
        }
        self.pending = None;
        tracing::info!("{}: club pick dropped ({})", self.actor, event.name);
        vec![Action::SnapBack]
    }

    /// A fresh readiness (a snapshot, `shot.ready_changed`): a pick Pureplay
    /// was not ready for may go again, and a readiness that lets apps
    /// steer the shot says Pureplay's switch is on again.
    pub fn readiness_changed(&mut self, readiness: Option<&Readiness>) {
        if let Some(p) = &mut self.pending {
            p.retry_at = None;
        }
        if readiness
            .is_some_and(|r| r.controllable && !r.blockers.iter().any(|b| b == "controls_disabled"))
        {
            self.switch_off = false;
        }
    }

    /// Pureplay's `auto` change: right after a `club.set` for the same
    /// shot, the pick is sent once more. During a command, events and the
    /// reply come in order, so an `auto` change seen then came before the
    /// command and needs no answer.
    fn auto_change(&mut self, c: &protocol::ClubChanged) {
        if self.flight.is_some() {
            return;
        }
        let Some(done) = &mut self.done else { return };
        let same_shot = c.shot_id.is_some() && c.shot_id == done.shot_id;
        let other = c.current.as_deref().and_then(protocol::club) != Some(done.club);
        if done.resent || !same_shot || Some(done.player) != c.player_id || !other {
            return;
        }
        done.resent = true;
        if self.pending.is_none() {
            tracing::info!(
                "{}: Pureplay's own club came after club {} was set; sent once more",
                self.actor,
                done.club
            );
            let mut pick = Pick::new(done.club, done.player, self.epoch);
            pick.resent = true;
            self.pending = Some(pick);
        }
    }

    /// A `response`: `None` when it answers no command of this card.
    pub fn reply(&mut self, r: &Response, model: &mut Model, now: Instant) -> Option<Vec<Action>> {
        let id = r.id.as_deref()?;
        if self.flight.as_ref().is_none_or(|f| f.id != id) {
            return None;
        }
        let flight = self.flight.take()?;
        let current = flight.pick.epoch == self.epoch;
        let mut actions = Vec::new();
        if r.ok {
            let reply: Reply = serde_json::from_value(r.data.clone()).unwrap_or_default();
            let player = reply.player_id.unwrap_or(flight.pick.player);
            if reply.club.is_some() {
                model.set_club(player, reply.club.clone(), reply.club_id.clone());
            }
            let reached = reply.club.as_deref().and_then(protocol::club);
            match flight.steps {
                // A newer pick steps from where this one got to.
                Some(_)
                    if current && reached != Some(flight.pick.club) && self.pending.is_some() =>
                {
                    actions.push(Action::SnapBack);
                }
                Some(mut steps) if current && reached != Some(flight.pick.club) => {
                    steps.taken += 1;
                    let stuck = same_club(
                        steps.from_id.as_deref(),
                        steps.from_kind.as_deref(),
                        reply.club_id.as_deref(),
                        reply.club.as_deref(),
                    );
                    let turn = stuck && !steps.turned && self.offers(steps.dir.turned());
                    if stuck && !turn {
                        tracing::info!(
                            "{}: club {}: {} did not move; given up",
                            self.actor,
                            flight.pick.club,
                            steps.dir.command()
                        );
                        actions.push(Action::SnapBack);
                    } else if steps.taken >= steps.max {
                        tracing::info!(
                            "{}: club {} not reached in {} steps; given up",
                            self.actor,
                            flight.pick.club,
                            steps.max
                        );
                        actions.push(Action::SnapBack);
                    } else {
                        if turn {
                            tracing::info!(
                                "{}: club {}: {} did not move; stepping the other way",
                                self.actor,
                                flight.pick.club,
                                steps.dir.command()
                            );
                            steps.dir = steps.dir.turned();
                            steps.turned = true;
                        }
                        steps.from_id.clone_from(&reply.club_id);
                        steps.from_kind.clone_from(&reply.club);
                        let command = steps.dir.command();
                        actions.push(self.send(
                            flight.pick,
                            command,
                            None,
                            Some(steps),
                            model,
                            now,
                        ));
                        return Some(actions);
                    }
                }
                _ => {
                    if current {
                        tracing::info!(
                            "{}: club {} set in Pureplay, holding {}",
                            self.actor,
                            flight.pick.club,
                            reply.club.as_deref().unwrap_or("(none)")
                        );
                        self.done = Some(Done {
                            club: flight.pick.club,
                            player,
                            shot_id: flight.shot_id.clone(),
                            resent: flight.pick.resent,
                        });
                    }
                    actions.push(Action::SnapBack);
                }
            }
        } else {
            let error = r.error.clone().unwrap_or_default();
            let mut pick = flight.pick;
            match error.code.as_str() {
                "not_ready" if current && self.pending.is_none() => {
                    if pick.not_ready >= NOT_READY_RETRIES {
                        tracing::info!(
                            "{}: club {} dropped: Pureplay is still not ready",
                            self.actor,
                            pick.club
                        );
                        actions.push(Action::SnapBack);
                    } else {
                        pick.not_ready += 1;
                        tracing::info!(
                            "{}: club {} waits: Pureplay is not ready (retry {} of {})",
                            self.actor,
                            pick.club,
                            pick.not_ready,
                            NOT_READY_RETRIES
                        );
                        pick.retry_at = Some(now + NOT_READY_RETRY);
                        self.pending = Some(pick);
                    }
                }
                "rate_limited" if current && self.pending.is_none() && !pick.retried => {
                    let wait = error
                        .retry_after_ms
                        .map_or(RATE_LIMIT_WAIT, Duration::from_millis);
                    tracing::info!("{}: club {} retried in {wait:?}", self.actor, pick.club);
                    pick.retried = true;
                    pick.not_before = Some(now + wait);
                    self.pending = Some(pick);
                }
                code => {
                    if code == "forbidden_scope" && self.control {
                        self.switch_off = true;
                    }
                    tracing::info!(
                        "{}: club {} refused ({code}); Pureplay's club stays",
                        self.actor,
                        pick.club
                    );
                    actions.push(Action::SnapBack);
                }
            }
        }
        actions.extend(self.drive(model, now));
        Some(actions)
    }

    /// Send the pending pick when Pureplay can take it; give up on a
    /// command unanswered too long.
    pub fn drive(&mut self, model: &Model, now: Instant) -> Vec<Action> {
        if let Some(f) = &self.flight {
            if now.duration_since(f.sent_at) < COMMAND_TIMEOUT {
                return Vec::new();
            }
            tracing::warn!("{}: no reply to {}; club pick dropped", self.actor, f.id);
            self.flight = None;
            return vec![Action::SnapBack];
        }
        let Some(pick) = self.pending.clone() else {
            return Vec::new();
        };
        let drop = |out: &mut Self, why: &str| {
            tracing::info!("{}: club {} not sent: {why}", out.actor, pick.club);
            out.pending = None;
            vec![Action::SnapBack]
        };
        if model.active_local_id() != Some(pick.player) {
            return drop(self, "the player is no longer up");
        }
        if self.state(model) != State::On {
            return drop(self, "club out is off");
        }
        if pick.retry_at.is_some_and(|t| now < t) || pick.not_before.is_some_and(|t| now < t) {
            return Vec::new();
        }
        let Some(player) = model.player(pick.player) else {
            return drop(self, "Pureplay does not know the player");
        };
        let bag = player.bag.as_deref().unwrap_or_default();
        let kind = |e: &protocol::BagEntry| e.kind.as_deref().and_then(protocol::club);
        let Some(target) = bag.iter().position(|e| kind(e) == Some(pick.club)) else {
            return drop(self, "not in the player's bag");
        };
        let held = player.club.as_deref().and_then(protocol::club);
        let (command, club_id, steps) = if self.set {
            (SET, bag[target].club_id.clone(), None)
        } else {
            let at = bag
                .iter()
                .position(|e| e.club_id.is_some() && e.club_id == player.club_id)
                .or_else(|| held.and_then(|h| bag.iter().position(|e| kind(e) == Some(h))));
            let want = if at.is_some_and(|h| h > target) {
                Dir::Prev
            } else {
                Dir::Next
            };
            let dir = if self.offers(want) {
                want
            } else {
                want.turned()
            };
            let steps = Steps {
                dir,
                taken: 0,
                max: bag.len(),
                from_id: player.club_id.clone(),
                from_kind: player.club.clone(),
                turned: false,
            };
            (dir.command(), None, Some(steps))
        };
        let readiness = model.readiness();
        let up = readiness.is_some_and(|r| r.controllable && r.player_id == Some(pick.player));
        if !up {
            if PICKS_WAIT {
                return Vec::new();
            }
            return drop(self, "Pureplay cannot take it now");
        }
        let listed = readiness.is_some_and(|r| {
            r.allowed_commands
                .iter()
                .any(|c| c.eq_ignore_ascii_case(command))
        });
        if !listed {
            return drop(self, "Pureplay does not take club changes here");
        }
        if held == Some(pick.club) {
            tracing::info!("{}: Pureplay holds a {} already", self.actor, pick.club);
            self.pending = None;
            return vec![Action::SnapBack];
        }
        if command == SET && club_id.is_none() {
            return drop(self, "its bag entry has no id");
        }
        self.pending = None;
        vec![self.send(pick, command, club_id.as_deref(), steps, model, now)]
    }

    /// Put `pick` in flight as `command`.
    fn send(
        &mut self,
        pick: Pick,
        command: &str,
        club_id: Option<&str>,
        steps: Option<Steps>,
        model: &Model,
        now: Instant,
    ) -> Action {
        self.sent += 1;
        let id = format!("c-{}", self.sent);
        let text = protocol::command(&id, command, pick.player, club_id);
        self.flight = Some(Flight {
            id,
            shot_id: model.readiness().and_then(|r| r.shot_id.clone()),
            pick,
            steps,
            sent_at: now,
        });
        Action::Send(text)
    }
}

#[cfg(test)]
mod tests;
