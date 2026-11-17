//! Club out: a club picked in flighthook on its way to Pureplay as a
//! command; Pureplay's replies, readiness and events decide what becomes of
//! it.

use super::*;
use crate::accessory::club_out::COMMAND_TIMEOUT;

/// The token every rig pairs with.
const TOKEN: &str = "PPA-CLUBOUT";

/// The readiness of Joe's shot `shot`: ready to play, or not.
fn readiness(shot: &str, ready: bool, allowed: &[&str], blockers: &[&str]) -> Value {
    json!({ "shotId": shot, "playerId": 1, "ready": ready, "controllable": ready,
            "allowedCommands": if ready { allowed.to_vec() } else { Vec::new() },
            "blockers": blockers })
}

/// [`round`] with `readiness` and, when given, Joe's bag and held club.
fn round_with(readiness: Value, bag: Option<(Value, &str, &str)>) -> Value {
    let mut r = round();
    r["readiness"] = readiness;
    if let Some((bag, club, club_id)) = bag {
        r["players"][0]["bag"] = bag;
        r["players"][0]["club"] = json!(club);
        r["players"][0]["clubId"] = json!(club_id);
    }
    r
}

/// The ball in the air: no command at all, so a pick waits.
fn in_the_air() -> Value {
    readiness("shot-1", false, &[], &["shot_in_progress"])
}

/// A card following `round`, Pureplay offering `commands` and the pairing
/// granting `scopes`.
fn rig_with(round: &Value, commands: &[&str], scopes: &[&str]) -> Rig {
    let mut rig = Rig::with_token(TOKEN);
    rig.next();
    rig.hello_offering("s-1", commands);
    assert_eq!(rig.pair().as_deref(), Some(TOKEN));
    rig.paired_with(TOKEN, scopes);
    rig.subscribe(round, 1);
    rig.shows(Status::Connected, |t| t.contains_key("club_out"));
    rig.caught_up();
    // What the card puts on the bus from here on is the test's.
    rig.queued.clear();
    rig
}

/// A card following [`round`] with `club.set` and `control`.
fn rig() -> Rig {
    rig_with(
        &round(),
        &["club.set", "club.next", "club.prev"],
        &["read", "control"],
    )
}

/// [`rig`] with the ball in the air.
fn rig_in_the_air() -> Rig {
    let mut rig = rig();
    rig.ready(&in_the_air());
    rig.shows_row(Status::Connected, "blockers", "shot in progress");
    rig
}

impl Rig {
    /// `hello` from instance `instance` offering `commands`.
    fn hello_offering(&mut self, instance: &str, commands: &[&str]) {
        self.send(&json!({ "type": "hello", "data": {
            "protocol": "ppaccessory", "version": 1, "game": "Pureplay",
            "gameVersion": "1.4.2", "paired": false, "connectionId": "c-1",
            "serverInstanceId": instance,
            "capabilities": { "events": EVENTS_LISTED, "commands": commands }
        }}));
    }

    /// `paired` granting `scopes`.
    fn paired_with(&mut self, token: &str, scopes: &[&str]) {
        self.send(&json!({ "type": "paired", "data": {
            "token": token, "clientId": "client-1", "scopes": scopes } }));
    }

    /// The card's next command; every one carries `playerId`.
    fn command(&mut self) -> Value {
        loop {
            let msg = self.recv();
            if msg["type"] == "command" {
                assert!(
                    msg["data"]["playerId"].is_i64(),
                    "a command carries playerId: {msg}"
                );
                return msg;
            }
        }
    }

    /// The commands the card sent for everything done so far, taken; other
    /// messages stay for [`Rig::recv`].
    fn commands_sent(&mut self) -> Vec<Value> {
        self.caught_up();
        let mut pipe = self.pipe.borrow_mut();
        let (commands, rest) = std::mem::take(&mut pipe.from_card)
            .into_iter()
            .partition(|v| v["type"] == "command");
        pipe.from_card = rest;
        commands.into()
    }

    /// No command for anything done so far.
    fn no_command(&mut self, why: &str) {
        let sent = self.commands_sent();
        assert!(sent.is_empty(), "{why}: {sent:?}");
    }

    /// Pureplay's readiness changes.
    fn ready(&mut self, readiness: &Value) {
        self.ev("shot.ready_changed", readiness);
    }

    /// Pureplay suggests `club` for Joe's next shot.
    fn auto(&mut self, club: &str, club_id: &str) {
        self.ev(
            "player.club_changed",
            &json!({ "playerId": 1, "shotId": "shot-2", "current": club,
                     "currentClubId": club_id, "reason": "auto" }),
        );
    }

    /// A successful reply to `cmd`: Joe holds `club` (`club_id`).
    fn ok(&mut self, cmd: &Value, club: &str, club_id: &str) {
        self.send(&json!({ "type": "response", "id": cmd["id"], "ok": true,
            "data": { "playerId": 1, "club": club, "clubId": club_id } }));
    }

    /// A refused reply to `cmd`.
    fn refuse_command(&mut self, cmd: &Value, code: &str, extra: &Value) {
        let id = cmd["id"].as_str().map(str::to_owned);
        self.refuse(id.as_deref(), code, extra);
    }

    /// The clubs of the card's selects from select `from` on, once it
    /// caught up.
    fn clubs_since(&mut self, from: usize) -> Vec<Club> {
        self.caught_up();
        self.clubs().split_off(
            self.selects()[..from]
                .iter()
                .filter(|s| matches!(s.club, Some(Some(_))))
                .count(),
        )
    }

    /// Step until the card puts `club` on the bus.
    fn snaps_back_to(&mut self, club: Club) {
        self.frp_until(|b| matches!(b, FrpBody::GolferSelect(s) if s.club == Some(Some(club))));
    }
}

/// A header pick goes to Pureplay as `club.set` with the bag entry's id and
/// `playerId`. Pureplay holding the pick, nothing goes back on the bus, nor
/// for Pureplay's own report of the change.
#[test]
fn a_header_pick_sets_the_club_in_pureplay_by_its_bag_id() {
    let mut rig = rig();
    rig.shows_row(Status::Connected, "club_out", "on");
    let from = rig.selects().len();
    rig.pick(Club::Iron8);
    let cmd = rig.command();
    assert_eq!(cmd["command"], "club.set");
    assert_eq!(cmd["data"], json!({ "playerId": 1, "clubId": "b-8i" }));
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.ev(
        "player.club_changed",
        &json!({ "playerId": 1, "shotId": "shot-1", "previous": "Iron7", "current": "Iron8",
                 "previousClubId": "b-7i", "currentClubId": "b-8i", "reason": "api_command" }),
    );
    rig.shows_row(Status::Connected, "club", "8I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "flighthook holds the 8 iron already"
    );
    rig.no_command("one pick, one command");
}

/// Pureplay holding a club of the picked type sends nothing: a second sand
/// wedge picked in Pureplay stays, whether its type is picked by another
/// actor or again in the header.
#[test]
fn a_club_of_the_held_type_sends_nothing_and_a_second_sand_wedge_stays() {
    let bag = json!([ { "clubId": "b-7i", "type": "Iron7", "name": "7 Iron" },
                      { "clubId": "sw-56", "type": "SandWedge", "name": "56" },
                      { "clubId": "sw-60", "type": "SandWedge", "name": "60" } ]);
    let r = round_with(
        readiness("shot-1", true, &["club.set"], &[]),
        Some((bag, "Iron7", "b-7i")),
    );
    let mut rig = rig_with(&r, &["club.set"], &["read", "control"]);
    rig.pick(Club::Iron7);
    rig.no_command("Pureplay holds a 7 iron");
    rig.ev(
        "player.club_changed",
        &json!({ "playerId": 1, "current": "SandWedge", "currentClubId": "sw-60",
                 "reason": "user_selected" }),
    );
    rig.snaps_back_to(Club::SandWedge);
    rig.pick_from("square.0", Club::SandWedge);
    rig.pick(Club::SandWedge);
    rig.no_command("the 60 stays");
}

/// A pick made while the ball is in the air waits; Pureplay's own club for
/// the next shot (`auto`) does not drop it and does not reach the bus; once
/// Pureplay is ready the pick is sent, and wins.
#[test]
fn a_pick_waits_through_the_shot_and_wins_over_pureplays_suggestion() {
    let mut rig = rig_in_the_air();
    let from = rig.selects().len();
    rig.pick(Club::Iron8);
    rig.no_command("not while the ball is in the air");
    rig.auto("Driver", "b-dr");
    rig.no_command("Pureplay is still not ready");
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    let cmd = rig.command();
    assert_eq!(cmd["data"]["clubId"], "b-8i");
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.shows_row(Status::Connected, "club", "8I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "the pick stays in flighthook throughout: Pureplay's driver never reached the bus"
    );
}

/// A pending pick dropped by a turn change: the card puts each player's
/// club in Pureplay on the bus. (What drops a pick is `club_out`'s.)
#[test]
fn a_pick_dropped_by_a_turn_puts_each_players_club_back() {
    let mut rig = rig_in_the_air();
    rig.pick(Club::Iron8);
    rig.ev(
        "turn.changed",
        &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "holeNumber": 7,
                 "reason": "next_player" }),
    );
    rig.snaps_back_to(Club::Driver);
    rig.ev(
        "turn.changed",
        &json!({ "previousPlayerId": 2, "currentPlayerId": 1, "holeNumber": 7,
                 "reason": "next_player" }),
    );
    rig.snaps_back_to(Club::Iron7);
}

/// A pending pick goes with the link: after a reconnect to the same
/// Pureplay, or to a restarted one, nothing is sent.
#[test]
fn a_pending_pick_is_dropped_with_the_connection() {
    for instance in ["s-1", "s-2"] {
        let mut rig = rig_in_the_air();
        rig.pick(Club::Iron8);
        rig.caught_up();
        assert_eq!(rig.turn.club, Some(Club::Iron8));
        rig.drop_link();
        rig.link_ends();
        rig.next();
        rig.hello_offering(instance, &["club.set"]);
        assert_eq!(rig.pair().as_deref(), Some(TOKEN));
        rig.paired_with(TOKEN, &["read", "control"]);
        rig.subscribe(&round(), 1);
        rig.shows_row(Status::Connected, "club", "7I");
        // The first command is a later pick's, not the dropped one's.
        rig.pick(Club::Driver);
        assert_eq!(rig.command()["data"]["clubId"], "b-dr", "{instance}");
    }
}

/// Pureplay's `auto` change for the shot right after the pick was set
/// sends the pick once more; a second one is accepted and goes on the bus.
#[test]
fn auto_after_a_set_sends_the_pick_once_more() {
    let mut rig = rig();
    rig.pick(Club::Iron8);
    let cmd = rig.command();
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.shows_row(Status::Connected, "club", "8I");
    let from = rig.selects().len();
    let auto = json!({ "playerId": 1, "shotId": "shot-1", "previous": "Iron8", "current": "Driver",
                       "currentClubId": "b-dr", "reason": "auto" });
    rig.ev("player.club_changed", &auto);
    let again = rig.command();
    assert_eq!(again["command"], "club.set");
    assert_eq!(again["data"]["clubId"], "b-8i");
    rig.ok(&again, "Iron8", "b-8i");
    rig.shows_row(Status::Connected, "club", "8I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "the driver never reached the bus"
    );
    rig.ev("player.club_changed", &auto);
    rig.snaps_back_to(Club::Driver);
    rig.no_command("a second auto is accepted");
}

/// `not_ready` puts the pick back: a new readiness sends it again at once,
/// ahead of the retry timer.
#[test]
fn not_ready_puts_the_pick_back_until_pureplay_is_ready() {
    let mut rig = rig();
    rig.pick(Club::Iron8);
    let cmd = rig.command();
    rig.refuse_command(
        &cmd,
        "not_ready",
        &json!({ "blockers": ["shot_in_progress"] }),
    );
    rig.caught_up();
    let refused = rig.host.now();
    rig.ready(&readiness("shot-1", true, &["club.set"], &[]));
    let again = rig.command();
    assert!(
        rig.host.now() - refused < crate::accessory::club_out::NOT_READY_RETRY,
        "ahead of the retry timer"
    );
    assert_ne!(again["id"], cmd["id"]);
    assert_eq!(again["data"]["clubId"], "b-8i");
}

/// A refusal puts Pureplay's club back on the bus, and `forbidden_scope`
/// shows club out off until a readiness lets apps steer again; a command
/// left unanswered puts it back too. (Which refusals drop a pick is
/// `club_out`'s.)
#[test]
fn a_refusal_and_an_unanswered_command_put_pureplays_club_back() {
    let mut rig = rig();
    rig.pick(Club::Iron8);
    let cmd = rig.command();
    rig.refuse_command(&cmd, "forbidden_scope", &json!({}));
    rig.snaps_back_to(Club::Iron7);
    rig.shows_row(Status::Connected, "club_out", "off in Pureplay");
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    rig.shows_row(Status::Connected, "club_out", "on");
    rig.pick(Club::Iron8);
    let _unanswered = rig.command();
    let started = rig.host.now();
    rig.snaps_back_to(Club::Iron7);
    assert!(rig.host.now() - started >= COMMAND_TIMEOUT);
}

/// Without `club.set` the card steps with `club.next` / `club.prev` toward
/// the type, each reply naming the club reached; the steps' own changes do
/// not reach the bus.
#[test]
fn without_club_set_the_card_steps_through_the_bag() {
    let steps = ["club.next", "club.prev"];
    let r = round_with(readiness("shot-1", true, &steps, &[]), None);
    let mut rig = rig_with(&r, &steps, &["read", "control"]);
    rig.shows_row(Status::Connected, "club_out", "on");
    let from = rig.selects().len();
    rig.pick(Club::Iron8);
    let cmd = rig.command();
    assert_eq!(cmd["command"], "club.next");
    assert_eq!(cmd["data"], json!({ "playerId": 1 }));
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.shows_row(Status::Connected, "club", "8I");
    rig.no_command("the 8 iron is reached");

    rig.pick(Club::Driver);
    let first = rig.command();
    assert_eq!(
        first["command"], "club.prev",
        "the driver is before the 8 iron"
    );
    rig.ev(
        "player.club_changed",
        &json!({ "playerId": 1, "current": "Iron7", "currentClubId": "b-7i",
                 "reason": "api_command" }),
    );
    rig.ok(&first, "Iron7", "b-7i");
    let second = rig.command();
    assert_eq!(second["command"], "club.prev");
    rig.ok(&second, "Driver", "b-dr");
    rig.shows_row(Status::Connected, "club", "DR");
    rig.no_command("the driver is reached");
    assert_eq!(rig.clubs_since(from), [], "the steps stay off the bus");
}

/// One command in flight: picks made meanwhile replace each other, and the
/// latest goes once the reply is in.
#[test]
fn one_command_in_flight_and_the_latest_pick_wins() {
    let mut rig = rig();
    rig.pick(Club::Iron8);
    let cmd = rig.command();
    rig.pick(Club::Driver);
    rig.caught_up();
    rig.pick(Club::Iron7);
    rig.no_command("one in flight");
    rig.ok(&cmd, "Iron8", "b-8i");
    let latest = rig.command();
    assert_eq!(latest["data"]["clubId"], "b-7i", "the latest pick");
    rig.ok(&latest, "Iron7", "b-7i");
    rig.no_command("the driver was replaced");
}

/// The `club_out` row: `read only` without `control`, `not supported`
/// without a club command, `off in Pureplay` while Pureplay's switch is
/// off. In each a pick sends nothing and Pureplay's club goes back.
#[test]
fn the_club_out_row_says_whether_picks_reach_pureplay() {
    let off = readiness("shot-1", true, &[], &["controls_disabled"]);
    let mut off = round_with(off, None);
    off["readiness"]["controllable"] = json!(false);
    for (r, commands, scopes, row) in [
        (round(), &["club.set"][..], &["read"][..], "read only"),
        (
            round(),
            &["aim.set"][..],
            &["read", "control"][..],
            "not supported",
        ),
        (
            off,
            &["club.set"][..],
            &["read", "control"][..],
            "off in Pureplay",
        ),
    ] {
        let mut rig = rig_with(&r, commands, scopes);
        rig.shows_row(Status::Connected, "club_out", row);
        rig.pick(Club::Iron8);
        rig.snaps_back_to(Club::Iron7);
        rig.no_command(row);
    }
}

/// A pick waits while the readiness names no player (between shots) and
/// goes once it names the player and lists the command.
#[test]
fn a_pick_waits_for_a_readiness_naming_the_player() {
    let mut rig = rig();
    rig.ready(
        &json!({ "shotId": null, "playerId": null, "ready": false, "controllable": true,
                 "allowedCommands": ["club.set"], "blockers": [] }),
    );
    rig.caught_up();
    rig.pick(Club::Iron8);
    rig.no_command("no player named");
    rig.ready(&readiness("shot-2", true, &["aim.set", "club.set"], &[]));
    let cmd = rig.command();
    assert_eq!(cmd["data"], json!({ "playerId": 1, "clubId": "b-8i" }));
}

/// Pureplay ready for the player but not listing the command (a drill, a
/// mode that keeps the club): the pick is dropped at once and Pureplay's
/// club goes back on the bus; a later readiness that lists it sends
/// nothing.
#[test]
fn a_pick_pureplay_does_not_take_here_is_dropped_at_once() {
    let mut rig = rig();
    rig.ready(&readiness("shot-2", true, &["aim.set"], &[]));
    rig.caught_up();
    rig.pick(Club::Iron8);
    rig.snaps_back_to(Club::Iron7);
    rig.no_command("club.set not allowed");
    rig.ready(&readiness("shot-2", true, &["aim.set", "club.set"], &[]));
    rig.no_command("the pick was dropped");
    // Pureplay's club changes reach the bus again at once.
    rig.ev(
        "player.club_changed",
        &json!({ "playerId": 1, "current": "Driver", "currentClubId": "b-dr", "reason": "auto" }),
    );
    rig.snaps_back_to(Club::Driver);
}

/// An `auto` change seen while a newer pick is in flight came before that
/// command: the older pick is not sent again, and the newer one stays.
#[test]
fn an_auto_change_during_a_newer_pick_does_not_resend_the_older_one() {
    let mut rig = rig();
    rig.pick(Club::Iron8);
    let first = rig.command();
    rig.ok(&first, "Iron8", "b-8i");
    rig.shows_row(Status::Connected, "club", "8I");
    rig.pick(Club::Driver);
    let second = rig.command();
    assert_eq!(second["data"]["clubId"], "b-dr");
    let from = rig.selects().len();
    rig.ev(
        "player.club_changed",
        &json!({ "playerId": 1, "shotId": "shot-1", "current": "Iron9", "reason": "auto" }),
    );
    rig.ok(&second, "Driver", "b-dr");
    rig.shows_row(Status::Connected, "club", "DR");
    rig.no_command("the 8 iron is not sent again");
    assert_eq!(rig.clubs_since(from), [], "the header stays on the driver");
}

/// The player's own choice in Pureplay after a pick was set: a later
/// `auto` change for the shot does not send the old pick over it.
#[test]
fn the_players_choice_after_a_set_ends_the_auto_resend() {
    let mut rig = rig();
    rig.pick(Club::Iron8);
    let cmd = rig.command();
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.shows_row(Status::Connected, "club", "8I");
    rig.ev(
        "player.club_changed",
        &json!({ "playerId": 1, "shotId": "shot-1", "current": "Iron9", "reason": "user_selected" }),
    );
    rig.snaps_back_to(Club::Iron9);
    rig.ev(
        "player.club_changed",
        &json!({ "playerId": 1, "shotId": "shot-1", "previous": "Iron9", "current": "Driver",
                 "currentClubId": "b-dr", "reason": "auto" }),
    );
    rig.snaps_back_to(Club::Driver);
    rig.no_command("the player chose since");
}

/// A pick of the club type Pureplay holds, while Pureplay can't take a
/// pick, still waits: when Pureplay suggests another club meanwhile, the
/// pick goes once Pureplay is ready.
#[test]
fn a_pick_of_the_held_type_waits_and_wins_over_a_suggestion() {
    let mut rig = rig_in_the_air();
    let from = rig.selects().len();
    rig.pick(Club::Iron8);
    rig.caught_up();
    rig.pick(Club::Iron7);
    rig.no_command("the ball is in the air");
    rig.auto("Driver", "b-dr");
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    let cmd = rig.command();
    assert_eq!(cmd["data"]["clubId"], "b-7i", "the latest pick");
    rig.ok(&cmd, "Iron7", "b-7i");
    rig.shows_row(Status::Connected, "club", "7I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "Pureplay's driver never reached the bus"
    );
}

/// A pick of the club type Pureplay holds, while Pureplay can't take a
/// pick: when Pureplay still holds that type once it is ready, nothing is
/// sent (a second club of the type stays).
#[test]
fn a_pick_of_the_held_type_sends_nothing_once_pureplay_is_ready() {
    let mut rig = rig_in_the_air();
    rig.pick(Club::Iron7);
    rig.no_command("the ball is in the air");
    let from = rig.selects().len();
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    rig.no_command("Pureplay holds a 7 iron");
    assert_eq!(rig.clubs_since(from), [], "flighthook holds the 7 iron");
}

/// A `turn.changed` naming the pick's own player (a new hole in a
/// one-player round) keeps the pick: it goes at the next shot, over
/// Pureplay's suggestion.
#[test]
fn a_new_hole_for_the_same_player_keeps_the_pick() {
    let mut rig = rig_in_the_air();
    let from = rig.selects().len();
    rig.pick(Club::Iron8);
    rig.no_command("the ball is in the air");
    rig.ev(
        "turn.changed",
        &json!({ "previousPlayerId": 1, "currentPlayerId": 1, "holeNumber": 8,
                 "reason": "new_hole" }),
    );
    rig.auto("Driver", "b-dr");
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    let cmd = rig.command();
    assert_eq!(cmd["data"]["clubId"], "b-8i");
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.shows_row(Status::Connected, "club", "8I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "the driver never reached the bus"
    );
}

/// A player with no bag: `club_out` says `not supported`, and a pick
/// sends nothing and puts Pureplay's club back.
#[test]
fn a_player_without_a_bag_has_no_club_out() {
    for bag in [json!(null), json!([])] {
        let mut r = round();
        r["players"][0]["bag"] = bag.clone();
        let mut rig = rig_with(&r, &["club.set"], &["read", "control"]);
        rig.shows_row(Status::Connected, "club_out", "not supported");
        rig.pick(Club::Iron8);
        rig.snaps_back_to(Club::Iron7);
        rig.no_command(&format!("bag {bag}"));
        // Ann, up next, has a bag.
        rig.ev(
            "turn.changed",
            &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "reason": "next_player" }),
        );
        rig.shows_row(Status::Connected, "club_out", "on");
    }
}

/// A late success for a command sent before the turn passed to another
/// player leaves the new player's club on the bus. (That nothing more is
/// sent is `club_out`'s.)
#[test]
fn a_late_success_after_a_turn_leaves_the_new_players_club() {
    let mut rig = rig();
    rig.pick(Club::Iron8);
    let cmd = rig.command();
    rig.ev(
        "turn.changed",
        &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "reason": "next_player" }),
    );
    rig.snaps_back_to(Club::Driver);
    rig.caught_up();
    assert_eq!(rig.turn.club, Some(Club::Driver));
    let from = rig.selects().len();
    rig.ok(&cmd, "Iron8", "b-8i");
    assert_eq!(rig.clubs_since(from), [], "Ann's driver stays");
    rig.shows_row(Status::Connected, "club", "DR");
}

/// A pick while a player at another computer is up is not sent and puts
/// nothing back; when a player here is up again, their club in Pureplay
/// goes on the bus.
#[test]
fn a_pick_while_nobody_here_is_up_is_not_sent() {
    let mut rig = rig();
    rig.ev(
        "turn.changed",
        &json!({ "previousPlayerId": 1, "currentPlayerId": 3, "reason": "next_player" }),
    );
    rig.shows(Status::Connected, |t| !t.contains_key("club"));
    let from = rig.selects().len();
    rig.pick(Club::Iron8);
    rig.no_command("nobody here is up");
    assert_eq!(rig.clubs_since(from), [], "nothing put back");
    rig.ev(
        "turn.changed",
        &json!({ "previousPlayerId": 3, "currentPlayerId": 1, "reason": "next_player" }),
    );
    rig.snaps_back_to(Club::Iron7);
    rig.no_command("the pick was never kept");
}

/// A `seq` gap while a pick waits: the fresh snapshot keeps the pick (and
/// Pureplay's club stays off the bus) while the same player is up, and
/// drops it when it names another player.
#[test]
fn a_snapshot_after_a_gap_keeps_or_drops_the_pending_pick() {
    for same in [true, false] {
        let mut rig = rig_in_the_air();
        let from = rig.selects().len();
        rig.pick(Club::Iron8);
        rig.no_command("the ball is in the air");
        rig.seq += 2;
        let seq = rig.seq;
        rig.event("hole.started", seq, &json!({ "holeNumber": 7, "par": 4 }));
        let get = rig.state_get();
        let mut snap = round_with(in_the_air(), None);
        snap["players"][0]["club"] = json!("Driver");
        snap["players"][0]["clubId"] = json!("b-dr");
        if !same {
            snap["turn"]["activeShooterId"] = json!(2);
        }
        rig.seq += 1;
        rig.send(
            &json!({ "type": "event", "event": "state.snapshot", "seq": rig.seq,
                     "requestId": get["id"], "data": snap }),
        );
        if same {
            rig.caught_up();
            assert_eq!(rig.clubs_since(from), [], "Joe's driver stays off the bus");
            rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
            assert_eq!(rig.command()["data"]["clubId"], "b-8i");
        } else {
            rig.snaps_back_to(Club::Driver);
            rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
            rig.no_command("Ann is up");
        }
    }
}

/// A club picked while the card is between links is not sent at the
/// reconnect: nothing goes to Pureplay at connect.
#[test]
fn a_pick_between_connections_is_not_sent() {
    let mut rig = rig();
    rig.drop_link();
    rig.link_ends();
    rig.pick(Club::Iron8);
    rig.next();
    rig.hello_offering("s-1", &["club.set"]);
    assert_eq!(rig.pair().as_deref(), Some(TOKEN));
    rig.paired_with(TOKEN, &["read", "control"]);
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "club", "7I");
    assert!(rig.sent_nothing(), "nothing at connect");
}

/// The bag changes between the pick and the send: the bag entry is looked
/// up when the command goes.
#[test]
fn the_bag_entry_is_looked_up_when_the_command_goes() {
    let mut rig = rig_in_the_air();
    rig.pick(Club::Iron8);
    rig.no_command("the ball is in the air");
    rig.ev(
        "player.bag_changed",
        &json!({ "playerId": 1, "bag": [
            { "clubId": "n-dr", "type": "Driver", "name": "Driver" },
            { "clubId": "n-7i", "type": "Iron7", "name": "7 Iron" },
            { "clubId": "n-8i", "type": "Iron8", "name": "8 Iron" } ] }),
    );
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    assert_eq!(rig.command()["data"]["clubId"], "n-8i");
}

/// Another player's own choice in Pureplay keeps the pick.
#[test]
fn another_players_choice_keeps_the_pick() {
    let mut rig = rig_in_the_air();
    rig.pick(Club::Iron8);
    rig.no_command("the ball is in the air");
    rig.ev(
        "player.club_changed",
        &json!({ "playerId": 2, "current": "Wood3", "reason": "user_selected" }),
    );
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    assert_eq!(rig.command()["data"]["clubId"], "b-8i");
}

// -- Read Only and Aim and Club ----------------------------------------------------

/// `paired` with no scopes at all (`null`) is Read Only: `read only`, and
/// a pick sends nothing and puts Pureplay's club back.
#[test]
fn scopes_null_is_read_only() {
    let mut rig = Rig::with_token(TOKEN);
    rig.next();
    rig.hello_offering("s-1", &["club.set"]);
    assert_eq!(rig.pair().as_deref(), Some(TOKEN));
    rig.send(&json!({ "type": "paired", "data": {
        "token": TOKEN, "clientId": "client-1", "scopes": null } }));
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "club_out", "read only");
    rig.pick(Club::Iron8);
    rig.snaps_back_to(Club::Iron7);
    rig.no_command("read only");
}

/// The grant changes with a new pairing, without restarting the card:
/// Read Only, removed in Pureplay (the saved token refused at the next
/// link), approved again as Aim and Club, `club_out` `on`, and a pick
/// goes; then the reverse. The pairing after a session is Reconnecting.
#[test]
fn a_new_pairing_with_another_grant_updates_club_out() {
    for (before, after, row_before, row_after) in [
        (&["read"][..], &["read", "control"][..], "read only", "on"),
        (&["read", "control"][..], &["read"][..], "on", "read only"),
    ] {
        let mut rig = rig_with(&round(), &["club.set"], before);
        rig.shows_row(Status::Connected, "club_out", row_before);
        // Pureplay restarts its server; the token is refused next time.
        rig.drop_link();
        rig.link_ends();
        rig.next();
        rig.hello_offering("s-1", &["club.set"]);
        assert_eq!(rig.pair().as_deref(), Some(TOKEN));
        rig.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
        rig.close(4401);
        rig.link_ends();
        rig.next();
        rig.hello_offering("s-1", &["club.set"]);
        assert_eq!(rig.pair(), None, "asks again without a token");
        rig.send(&json!({ "type": "pairing.pending", "data": { "expiresInSec": 60 } }));
        rig.shows(Status::Reconnecting, |t| {
            t.get("pairing")
                .is_some_and(|p| p.starts_with("Approve flighthook in Pureplay"))
        });
        rig.paired_with("PPA-NEW", after);
        rig.subscribe(&round(), 1);
        rig.shows_row(Status::Connected, "club_out", row_after);
        rig.pick(Club::Iron8);
        let sent = if row_after == "on" {
            vec![rig.command()]
        } else {
            rig.snaps_back_to(Club::Iron7);
            rig.commands_sent()
        };
        assert_eq!(sent.len(), usize::from(row_after == "on"), "{row_after}");
    }
}

/// Removed in Pureplay while connected, then Reconnect: Pureplay's
/// cooldown counts down while the card is Starting, the card asks again by
/// itself when it ends, and the new grant sets `club_out`.
#[test]
fn removed_then_reconnect_waits_out_the_cooldown_and_pairs_with_the_new_grant() {
    let mut rig = rig_with(&round(), &["club.set"], &["read"]);
    rig.shows_row(Status::Connected, "club_out", "read only");
    rig.refuse(None, "unauthorised", &json!({}));
    rig.close(4401);
    rig.shows_row(Status::Disconnected, "pairing", REMOVED);
    rig.reconnect();
    rig.next();
    rig.hello_offering("s-1", &["club.set"]);
    assert_eq!(rig.pair(), None, "the token went with the removal");
    rig.refuse(
        Some(protocol::PAIR_ID),
        "rate_limited",
        &json!({ "retryAfterMs": 1200 }),
    );
    rig.close(4429);
    rig.link_ends();
    rig.next();
    assert!(
        rig.statuses()
            .iter()
            .any(|s| s.telemetry.get("pairing").map(String::as_str)
                == Some("Pureplay asks to wait (0:01)")),
        "the countdown"
    );
    assert_eq!(rig.status, Status::Starting);
    rig.hello_offering("s-1", &["club.set"]);
    assert_eq!(rig.pair(), None, "asks again by itself");
    rig.send(&json!({ "type": "pairing.pending", "data": { "expiresInSec": 60 } }));
    rig.shows_row(
        Status::Starting,
        "pairing",
        "Approve flighthook in Pureplay (60 s)",
    );
    rig.paired_with("PPA-NEW", &["read", "control"]);
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "club_out", "on");
}
