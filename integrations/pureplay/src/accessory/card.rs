//! [`Card`]: what the Accessory card keeps across links (pairing, the round
//! model, the shot map, club out and the status it shows), shared by the
//! driver and its session.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use flightrelay::driver::{DeviceStatus, Events, Level, ShotIds};
use flightrelay::{BusReply, Club, FrpBody, FrpMessage, RawDir, ReplyError, blockers};

use super::club_out::{Action, ClubOut};
use super::link::{Link, SEND_BINARY};
use super::model::{Model, Want};
use super::protocol::{self, Paired, ResponseError};
use super::session::*;
use super::shots::{self, Discarded, Flown, ShotMap};
use super::token::{self, Token, TokenStore};
use super::{Config, game};

/// What the card keeps across links: pairing, the round, shots, club out
/// and the status it shows.
#[derive(Debug)]
pub(crate) struct Card {
    /// The token file key of the card's address.
    key: String,
    pub(crate) tokens: Arc<TokenStore>,
    /// A pair without a token may still be sent by itself.
    pub(super) may_pair: bool,
    /// The one attempt after a cooldown is still to come.
    cooldown_retry: bool,
    /// `bad_request` answers to `pair` in a row.
    pub(super) bad_requests: u32,
    /// Every token seen, replaced in every frame logged.
    secrets: Vec<Token>,
    /// The token Pureplay gave this driver, used while the token file could
    /// not keep it.
    held: Option<Token>,
    /// Why the token file could not keep the token, shown as `error`.
    save_error: Option<String>,
    pub(crate) model: Model,
    /// Clubs picked in flighthook, on their way to Pureplay.
    pub(crate) club_out: ClubOut,
    /// The latest pick handed since the last pass, while following.
    pub(super) picked: Option<Club>,
    /// The card's `game.*` and `golfer.profile` as last published.
    published: game::Published,
    /// The round is being followed: picks are taken.
    pub(super) following: bool,
    pub(super) server_instance: Option<String>,
    pub(super) game_version: Option<String>,
    /// The commands `hello.capabilities` lists.
    pub(super) commands: Vec<String>,
    /// The scopes the pairing grants.
    pub(super) scopes: Vec<String>,
    pub(super) missed: u32,
    /// Pureplay's shot ids against the card's keys of the shots they stand
    /// for; kept across a reconnect to the same Pureplay.
    pub(super) shot_map: ShotMap,
    /// What Pureplay's recent shots flew.
    pub(super) flown: Flown,
    /// The shots Pureplay discarded: no stroke goes out for them.
    pub(super) discarded: Discarded,
    /// The card's shot numbers.
    ids: ShotIds,
    /// The selects the card sent, and the prefix of their `ref`s.
    selects: u64,
    select_prefix: String,
    /// The rows the card should show.
    pub(super) rows: BTreeMap<&'static str, String>,
    /// The status the host holds and the card shows.
    pub(super) shown: DeviceStatus,
}

impl Card {
    pub(super) fn new(config: &Config, addr: SocketAddr) -> Self {
        let tokens = config
            .token_file
            .clone()
            .map_or_else(TokenStore::shared, |path| Arc::new(TokenStore::new(path)));
        Self {
            key: token::key(addr),
            tokens,
            may_pair: true,
            cooldown_retry: true,
            bad_requests: 0,
            secrets: Vec::new(),
            held: None,
            save_error: None,
            model: Model::default(),
            club_out: ClubOut::new("pureplay_accessory"),
            picked: None,
            published: game::Published::default(),
            following: false,
            server_instance: None,
            game_version: None,
            commands: Vec::new(),
            scopes: Vec::new(),
            missed: 0,
            shot_map: ShotMap::default(),
            flown: Flown::default(),
            discarded: Discarded::default(),
            ids: ShotIds::new(),
            selects: 0,
            select_prefix: uuid::Uuid::new_v4().simple().to_string(),
            rows: BTreeMap::new(),
            shown: DeviceStatus::not_armed(),
        }
    }

    pub(super) fn set(&mut self, key: &'static str, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            self.rows.remove(key);
        } else {
            self.rows.insert(key, value);
        }
    }

    /// Pureplay's readiness blockers, as it names them: none when ready.
    /// Not ready with none named, no readiness known yet, or no round
    /// followed is `not_armed`.
    fn blockers(&self) -> Vec<String> {
        match self.model.readiness() {
            Some(r) if self.following && r.ready => Vec::new(),
            Some(r) if self.following && !r.blockers.is_empty() => r.blockers.clone(),
            _ => vec![blockers::NOT_ARMED.into()],
        }
    }

    /// Send the card's status (blockers and rows) when it changed.
    pub(super) fn show(&mut self, out: &mut Events<'_>) {
        let mut status = DeviceStatus::new().with_blockers(self.blockers());
        for (k, v) in &self.rows {
            status = status.with_telemetry(*k, v.clone());
        }
        if status != self.shown {
            self.shown = status.clone();
            out.status(status);
        }
    }

    /// A connect starts: the host holds no status, while the card still
    /// shows the rows the last connect left. They are put back first, so
    /// that one the card no longer has leaves the card.
    pub(super) fn restore(&mut self, out: &mut Events<'_>) {
        if !self.shown.telemetry.is_empty() {
            out.status(self.shown.clone());
        }
    }

    /// Keep `token` out of every frame logged.
    pub(super) fn learn(&mut self, token: &str) {
        if !token.is_empty() && !self.secrets.iter().any(|t| t.expose() == token) {
            self.secrets.push(Token::new(token));
        }
    }

    /// `text` with every token the card knows replaced.
    pub(super) fn redact(&self, text: &str) -> String {
        token::redact(text, &self.secrets)
    }

    /// The token to pair with: the token file's, else, while the file
    /// could not keep it, the one Pureplay gave this driver.
    pub(super) fn token(&self) -> Option<Token> {
        self.tokens
            .get(&self.key)
            .or_else(|| self.save_error.as_ref().and(self.held.clone()))
    }

    fn forget_token(&mut self, out: &mut Events<'_>) {
        self.held = None;
        match self.tokens.forget(&self.key) {
            Ok(true) => out.log(Level::Info, "saved pairing deleted"),
            Ok(false) => {}
            Err(e) => out.log(
                Level::Warn,
                format!("couldn't delete the saved pairing: {e}"),
            ),
        }
    }

    /// Remove what held for the link that ended: the card's game state
    /// from the bus, the link's rows, a club pick.
    pub(super) fn end_link(&mut self, out: &mut Events<'_>) {
        for body in self.published.clear() {
            out.frp(body);
        }
        for key in PER_LINK {
            self.rows.remove(key);
        }
        self.model.forget_held();
        self.club_out.clear();
        self.model.hold_club(false);
        self.following = false;
        self.picked = None;
    }

    /// The rows of a link, from the model.
    pub(super) fn link_rows(&mut self) {
        let player = self.model.active_local().cloned();
        self.set(
            NAME,
            player
                .as_ref()
                .and_then(|p| p.name.clone())
                .unwrap_or_default(),
        );
        self.set(
            CLUB,
            player
                .as_ref()
                .and_then(|p| p.club.as_deref())
                .and_then(protocol::club)
                .map(|c| c.to_string())
                .unwrap_or_default(),
        );
        self.set(
            HANDED,
            player
                .as_ref()
                .and_then(|p| p.handedness.as_deref())
                .and_then(protocol::hand)
                .map(|h| h.to_string())
                .unwrap_or_default(),
        );
        let hole = self.model.hole_text().unwrap_or_default();
        self.set(HOLE, hole);
        let blockers = self
            .model
            .readiness()
            .map(protocol::blockers_text)
            .unwrap_or_default();
        self.set(BLOCKERS, blockers);
        let club_out = self.club_out.state(&self.model).text();
        self.set(CLUB_OUT, club_out);
        let missed = if self.missed > 0 {
            self.missed.to_string()
        } else {
            String::new()
        };
        self.set(MISSED, missed);
        let version = self.game_version.clone().unwrap_or_default();
        self.set(PUREPLAY, version);
    }

    /// Send the card's complete `golfer.select` when the model says so.
    pub(super) fn sync(&mut self, now: Instant, out: &mut Events<'_>) {
        if let Some(want) = self.model.sync(now) {
            self.emit(&want, now, out);
        }
    }

    /// Put `want` on the bus in one `golfer.select`, which then awaits its
    /// `bus.reply`.
    fn emit(&mut self, want: &Want, now: Instant, out: &mut Events<'_>) {
        self.selects += 1;
        let select = want.select(format!("{}-{}", self.select_prefix, self.selects));
        out.log(Level::Info, format!("{want:?} from Pureplay"));
        self.model
            .select_sent(&select.r#ref, Some(want.clone()), now);
        out.frp(select);
    }

    /// Publish what changed of the card's game state.
    pub(super) fn game_changes(&mut self, out: &mut Events<'_>) {
        for body in self.published.changes(&mut self.model) {
            out.frp(body);
        }
    }

    /// A bus message handed to the link: flighthook's turn, the reply to a
    /// select, a pick from outside the game and simulator cards (while the
    /// round is followed), `bus.sync`, or a `game.aim` to refuse.
    pub(super) fn bus(&mut self, msg: &FrpMessage, now: Instant, out: &mut Events<'_>) {
        match &msg.body {
            FrpBody::GolferTurn(turn) => self.model.flighthook_holds(turn),
            FrpBody::BusReply(reply) if reply.command == "golfer.select" => {
                self.model.select_answered(&reply.r#ref, reply.ok);
            }
            FrpBody::GolferSelect(select) => {
                if let Some(Some(club)) = select.club
                    && self.following
                {
                    self.model.select_sent(&select.r#ref, None, now);
                    self.picked = Some(club);
                }
            }
            FrpBody::BusSync(_) => {
                for body in self.published.all() {
                    out.frp(body);
                }
            }
            FrpBody::GameAim(aim) => {
                out.log(
                    Level::Info,
                    format!("game.aim from {}, unsupported", msg.from),
                );
                out.frp(BusReply::refused(
                    msg.from.clone(),
                    "game.aim",
                    aim.r#ref.clone(),
                    ReplyError::Unsupported,
                ));
            }
            _ => {}
        }
    }

    /// Carry out what club out asks: send its commands. Once no pick holds
    /// the club, the card's select goes out when flighthook's club differs
    /// from Pureplay's (`raw`, the frame that ended the pick, is logged).
    pub(super) fn club_actions<L: Link>(
        &mut self,
        actions: Vec<Action>,
        link: &mut L,
        raw: Option<&str>,
        now: Instant,
        out: &mut Events<'_>,
    ) {
        for action in actions {
            match action {
                // A failed send ends the link, which drops the pick.
                Action::Send(text) => {
                    let _ = self.send(link, &text, out);
                }
                Action::SnapBack => {
                    if let Some(raw) = raw {
                        out.log(Level::Debug, format!("pick ended by {raw}"));
                    }
                    out.log(Level::Info, "Pureplay's club stays");
                }
            }
        }
        self.model.hold_club(self.club_out.holds_club());
        self.sync(now, out);
    }

    /// Send `text` on the link as a raw frame with every token replaced. A
    /// failed send is logged and is the caller's to end the link with.
    pub(super) fn send<L: Link>(
        &self,
        link: &mut L,
        text: &str,
        out: &mut Events<'_>,
    ) -> Result<(), ()> {
        let kind = if SEND_BINARY { "binary" } else { "text" };
        out.wire(
            RawDir::Tx,
            kind,
            self.redact(text).as_bytes().to_vec(),
            flightrelay::utc_ms(),
            false,
        );
        link.send(text).map_err(|e| {
            out.log(Level::Info, format!("send failed: {e}"));
        })
    }

    /// Take a snapshot event: the model and the bus.
    pub(super) fn snapshot(&mut self, e: &protocol::Event, out: &mut Events<'_>) {
        match protocol::payload::<protocol::Snapshot>(e) {
            Ok(snap) => {
                self.model.hold_club(self.club_out.holds_club());
                self.model.apply_snapshot(&snap);
                self.game_changes(out);
            }
            Err(err) => out.log(
                Level::Warn,
                format!("snapshot not read: {}", self.redact(&err.to_string())),
            ),
        }
    }

    /// Take a live shot event: a local `shot.launched` reported as the
    /// card's shot, a `shot.completed` sent as `game.stroke`, a discarded
    /// shot forgotten.
    pub(super) fn shot_event(&mut self, e: &protocol::Event, raw: &str, out: &mut Events<'_>) {
        let not_read = |err: protocol::ParseError, out: &mut Events<'_>| {
            out.log(
                Level::Warn,
                format!("{} not read: {}", e.name, self.redact(&err.to_string())),
            );
        };
        match e.name.as_str() {
            "shot.launched" => match protocol::payload::<protocol::ShotLaunched>(e) {
                Ok(shot) => self.shot_launched(&shot, raw, out),
                Err(err) => not_read(err, out),
            },
            "shot.completed" => match protocol::payload::<protocol::ShotCompleted>(e) {
                Ok(done) => self.shot_completed(&done, raw, out),
                Err(err) => not_read(err, out),
            },
            "shot.cancelled" => match protocol::payload::<protocol::ShotCancelled>(e) {
                Ok(c) => {
                    if c.reason.as_deref() == Some("discarded")
                        && let Some(id) = c.shot_id.as_deref()
                    {
                        // Whatever the card held of it: a shot it did not
                        // map (a remote player's, one launched before the
                        // card connected) is discarded too.
                        self.discarded.insert(id);
                        self.shot_map.remove(id);
                        out.log(Level::Info, format!("Pureplay discarded shot {id}"));
                        out.log(Level::Debug, format!("discard {raw}"));
                    }
                }
                Err(err) => not_read(err, out),
            },
            _ => {}
        }
    }

    /// A `shot.launched`: a local player's shot is reported as the card's
    /// own (`shot.trigger` with no timestamp, `shot.launch`,
    /// `shot.finished`); the host decides whether it goes on the bus. A
    /// `shotId` the card already holds is not reported again.
    fn shot_launched(&mut self, shot: &protocol::ShotLaunched, raw: &str, out: &mut Events<'_>) {
        let id = shot.shot_id.clone().unwrap_or_default();
        let launch = shots::launch(shot);
        if !id.is_empty()
            && let Some(launch) = &launch
        {
            self.flown.insert(&id, launch.clone());
        }
        let Some(player) = shot.player_id.filter(|p| self.model.is_local(*p)) else {
            out.log(
                Level::Info,
                format!("shot {id} of a player at another machine, not taken"),
            );
            return;
        };
        if !id.is_empty()
            && let Some(key) = self.shot_map.get(&id)
        {
            out.log(
                Level::Info,
                format!(
                    "shot {id} again, already shot #{} ({}); not taken",
                    key.shot_number, key.shot_id
                ),
            );
            return;
        }
        let Some(launch) = launch else {
            out.log(
                Level::Warn,
                format!("shot {id} has no ball speed, not taken"),
            );
            return;
        };
        let key = self.ids.next_key();
        out.log(
            Level::Info,
            format!(
                "shot {id} of player {player} reported as shot #{}",
                key.shot_number
            ),
        );
        out.log(Level::Debug, format!("shot from {raw}"));
        out.shot_trigger(key.clone());
        out.launch(key.clone(), launch);
        out.shot_finished(key.clone());
        if !id.is_empty() {
            self.shot_map.insert(&id, key);
        }
    }

    /// A `shot.completed`: Pureplay's stroke as `game.stroke`, with the
    /// card's `shot_id` of the shot its `shotId` stands for. A discarded
    /// shot sends none.
    fn shot_completed(&self, done: &protocol::ShotCompleted, raw: &str, out: &mut Events<'_>) {
        let id = done.shot_id.as_deref().unwrap_or_default();
        if self.discarded.contains(id) {
            out.log(Level::Info, format!("shot {id} was discarded; no stroke"));
            return;
        }
        let Some(golfer) = done.player_id.and_then(|p| self.model.golfer(p)) else {
            out.log(
                Level::Info,
                format!("the card does not hold the player of shot {id}; no stroke"),
            );
            return;
        };
        let key = self.shot_map.get(id);
        let replaces = self.model.replaces(id);
        let Some(stroke) = shots::stroke(done, key, golfer, self.flown.get(id), replaces) else {
            out.log(
                Level::Warn,
                format!(
                    "shot {id} ended {} at hole {:?}, stroke {:?}; no stroke",
                    done.outcome.as_deref().unwrap_or("(no outcome)"),
                    done.hole_number,
                    done.stroke_number
                ),
            );
            return;
        };
        out.log(
            Level::Info,
            format!(
                "shot {id} ended {}{}",
                done.outcome.as_deref().unwrap_or("(no outcome)"),
                stroke
                    .shot_id
                    .as_deref()
                    .map(|s| format!(", shot {s}"))
                    .unwrap_or_default()
            ),
        );
        out.log(Level::Debug, format!("stroke from {raw}"));
        out.frp(stroke);
    }

    /// Keep what pairing gave: the token (saved when new) and the scopes.
    pub(super) fn keep_pairing(
        &mut self,
        paired: &Paired,
        saved: Option<&Token>,
        out: &mut Events<'_>,
    ) {
        if saved.is_none() {
            // The one pair without a token this driver may send was used.
            self.may_pair = false;
        }
        self.bad_requests = 0;
        self.rows.remove(PAIRING);
        if let Some(t) = paired.token.as_deref().filter(|t| !t.is_empty()) {
            self.learn(t);
            let t = Token::new(t);
            self.held = Some(t.clone());
            if saved != Some(&t) {
                let client_id = paired.client_id.as_deref().unwrap_or_default();
                match self.tokens.save(&self.key, &t, client_id) {
                    Ok(()) => {
                        self.save_error = None;
                        self.rows.remove(ERROR);
                        out.log(
                            Level::Info,
                            format!("paired, saved in {}", self.tokens.path().display()),
                        );
                    }
                    Err(e) => {
                        out.log(Level::Warn, format!("couldn't save the pairing: {e}"));
                        let text = format!("Couldn't save the Pureplay pairing: {e}");
                        self.set(ERROR, text.clone());
                        self.save_error = Some(text);
                    }
                }
            }
        }
        self.scopes.clone_from(&paired.scopes);
        out.log(
            Level::Info,
            format!(
                "paired with scopes {}",
                self.redact(&self.scopes.join(", "))
            ),
        );
    }

    /// Why a `pair` ended with close `code` (`None`: the socket ended
    /// without one, or the card gave up waiting), after the refused
    /// response's `error`, and what the card does next. Without a code the
    /// response decides.
    pub(super) fn refused(
        &mut self,
        code: Option<u16>,
        error: Option<&ResponseError>,
        had_token: bool,
        out: &mut Events<'_>,
    ) -> Error {
        let said = |c: &str| error.is_some_and(|e| e.code == c);
        match code {
            Some(4401) => self.dead_token(had_token, out),
            Some(4403) => {
                let text = if said("forbidden_scope") {
                    ASK_OFF
                } else {
                    DENIED
                };
                self.stop(text)
            }
            Some(4408) => self.stop(TIMED_OUT),
            Some(4429) if said("rate_limited") => self.cooldown(error),
            Some(4429) => {
                self.set(PAIRING, TOO_MANY);
                Error::TooMany
            }
            None if said("unauthorised") && had_token => self.dead_token(true, out),
            // Denied, or the prompt timed out: the close code would have
            // told which.
            None if said("unauthorised") => self.stop(DENIED),
            None if said("forbidden_scope") => self.stop(ASK_OFF),
            None if said("rate_limited") => self.cooldown(error),
            _ => Error::Lost("pair refused".into()),
        }
    }

    /// No retry until the user's Reconnect, the `pairing` row saying `why`.
    pub(super) fn stop(&mut self, why: &str) -> Error {
        self.may_pair = false;
        self.set(PAIRING, why);
        Error::Refused
    }

    /// The saved token was refused: pair once without one when that is
    /// still allowed, else wait for the user's Reconnect.
    fn dead_token(&mut self, had_token: bool, out: &mut Events<'_>) -> Error {
        self.forget_token(out);
        if had_token && self.may_pair {
            out.log(Level::Info, "the saved pairing is no longer valid");
            Error::Again
        } else {
            self.stop(REMOVED)
        }
    }

    /// Pureplay's cooldown, for the wait `error` names.
    fn cooldown(&mut self, error: Option<&ResponseError>) -> Error {
        let wait = error
            .and_then(|e| e.retry_after_ms)
            .map_or(DEFAULT_COOLDOWN, Duration::from_millis);
        let retry = std::mem::take(&mut self.cooldown_retry);
        Error::Cooldown { wait, retry }
    }

    /// How a paired link ended with close `code`; `removal` when an
    /// unprompted `unauthorised` came first, which decides by itself when
    /// the socket ended without a code.
    pub(super) fn lost(&mut self, code: Option<u16>, removal: bool, out: &mut Events<'_>) -> Error {
        match code {
            Some(4401) => self.removed(out),
            None if removal => {
                out.log(Level::Info, "removed in Pureplay (no close frame)");
                self.removed(out)
            }
            Some(code @ (4403 | 4408 | 4429)) => self.refused(Some(code), None, true, out),
            _ => Error::Lost("connection lost".into()),
        }
    }

    /// Removed in Pureplay: the token goes; no retry until Reconnect.
    fn removed(&mut self, out: &mut Events<'_>) -> Error {
        out.log(Level::Info, "removed in Pureplay");
        self.forget_token(out);
        self.stop(REMOVED)
    }
}
