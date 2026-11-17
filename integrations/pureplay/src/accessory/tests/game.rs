//! The game: complete selects, re-assert, the mode rule, `game.*` and
//! `golfer.profile`, strokes, `game.aim`, the state requests of a second
//! match.

use flightrelay::{AimAction, EndReason, GameBall, RoundStatus, StateKind};

use super::*;

/// The token every rig pairs with.
const TOKEN: &str = "PPA-SHOTS";

/// A card following [`round`], Joe's turn on the bus; what it puts on the
/// bus from here on is the test's.
fn rig() -> Rig {
    let mut rig = connected(TOKEN);
    assert!(is_joe(&rig.turn), "{:?}", rig.turn);
    rig.queued.clear();
    rig
}

fn golfer(turn: &GolferTurn) -> Option<&str> {
    turn.golfer.as_ref().map(GolferId::as_str)
}

/// The turn Joe (player 1, local) holds in [`round`].
fn is_joe(t: &GolferTurn) -> bool {
    golfer(t) == Some("pureplay.Joe")
        && t.club == Some(Club::Iron7)
        && t.handedness == Some(Handedness::Right)
        && t.mode == Some(Mode::Full)
}

/// [`round`] as match `id`.
fn round_of(id: &str) -> Value {
    let mut r = round();
    r["session"]["matchId"] = json!(id);
    r
}

impl Rig {
    /// Step until flighthook's turn satisfies `pred`; that turn.
    fn turn_until(&mut self, pred: impl Fn(&GolferTurn) -> bool) -> GolferTurn {
        self.until("the turn", |rig| pred(&rig.turn));
        self.turn.clone()
    }

    /// `from` sends `select`, answered by flighthook's session.
    fn select_from(&mut self, from: &str, select: GolferSelect) {
        self.hand_from(from, select.clone());
        self.answer(&select);
    }

    /// The card's `game.ball`s so far.
    fn balls(&self) -> Vec<GameBall> {
        self.frps()
            .into_iter()
            .filter_map(|b| match b {
                FrpBody::GameBall(ball) => Some(ball),
                _ => None,
            })
            .collect()
    }

    /// What the card put on the bus from message `from` on.
    fn frps_since(&self, from: usize) -> Vec<FrpBody> {
        self.frps().split_off(from)
    }

    /// Joe takes a shot in match `m-1`, which then ends; flighthook puts
    /// its start golfer back on the turn once Joe's profile is removed.
    fn first_match_with_a_shot(&mut self) {
        self.event("shot.launched", 2, &launched("pp-1", 1));
        self.frp_until(|b| matches!(b, FrpBody::ShotFinished(_)));
        self.event("shot.completed", 3, &completed("pp-1", 1, "InPlay"));
        self.frp_until(|b| matches!(b, FrpBody::GameStroke(_)));
        self.event(
            "match.ended",
            4,
            &json!({ "matchId": "m-1", "reason": "completed", "standings": [] }),
        );
        self.frp_until(|b| matches!(b, FrpBody::Removed(r) if r.kind == StateKind::GameRound));
        self.turn_to(GolferTurn::new(None, None, Handedness::Right, Mode::Full));
        self.caught_up();
    }

    /// The events of a second match with Joe up, as Pureplay sends them (no
    /// snapshot), from `seq`.
    fn second_match_events(&mut self, seq: u64) {
        self.event(
            "match.started",
            seq,
            &json!({ "matchId": "m-2", "courseName": "Course", "holeCount": 18, "totalPar": 72,
                     "scoringModel": "Stroke", "isTeamGame": false, "playerIds": [1, 2, 3] }),
        );
        self.event(
            "hole.started",
            seq + 1,
            &json!({ "matchId": "m-2", "holeNumber": 1, "par": 4, "playerIds": [1, 2, 3] }),
        );
        self.event(
            "turn.changed",
            seq + 2,
            &json!({ "matchId": "m-2", "previousPlayerId": null, "currentPlayerId": 1,
                     "holeNumber": 1, "reason": "next_player" }),
        );
        self.event(
            "shot.initialized",
            seq + 3,
            &json!({ "shotId": "s-2", "playerId": 1, "holeNumber": 1, "strokeNumber": 1,
                     "club": "Iron7", "distanceToHoleM": 300.0 }),
        );
    }

    /// The card's next `state.get`, skipping other messages.
    fn next_state_get(&mut self) -> Value {
        loop {
            let msg = self.recv();
            if msg["type"] == "state.get" {
                return msg;
            }
        }
    }

    /// Answer `get` with the snapshot of match `m-2`, then Joe's shot: the
    /// shot is reported.
    fn answer_and_take_the_shot(&mut self, get: &Value, seq: u64) {
        let triggers = self.triggers().len();
        self.send(
            &json!({ "type": "event", "event": "state.snapshot", "seq": seq,
                     "requestId": get["id"], "data": round_of("m-2") }),
        );
        self.turn_until(is_joe);
        self.event("shot.launched", seq + 1, &launched("pp-2", 1));
        self.frp_until(|b| matches!(b, FrpBody::ShotFinished(_)));
        self.caught_up();
        assert_eq!(self.triggers().len(), triggers + 1, "the shot is reported");
    }

    /// The link drops and the card reconnects to the same Pureplay, which
    /// sends `snapshot`.
    fn reconnect_to(&mut self, snapshot: &Value) {
        self.drop_link();
        self.link_ends();
        assert_eq!(self.status, Status::Reconnecting);
        self.next();
        self.hello();
        assert_eq!(self.pair().as_deref(), Some(TOKEN));
        self.paired(TOKEN);
        self.subscribe(snapshot, 1);
        self.shows(Status::Connected, |_| true);
        self.caught_up();
    }
}

/// A round with two local players: the turn to the second switches the
/// golfer, club and hand in one turn change, with no club flip (no turn
/// with the new golfer and the old club, or the old golfer and the new
/// club).
#[test]
fn a_round_with_two_local_players_switches_the_golfer_with_no_club_flip() {
    let mut rig = rig();
    let before = rig.turns.len();
    rig.event(
        "turn.changed",
        2,
        &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "holeNumber": 7, "reason": "next_player" }),
    );
    let ann = rig.turn_until(|t| golfer(t) == Some("pureplay.Ann"));
    assert_eq!(ann.club, Some(Club::Driver));
    assert_eq!(ann.handedness, Some(Handedness::Left));
    rig.caught_up();
    assert_eq!(rig.turns[before..], [ann], "one turn change");
}

/// A header golfer pick (flighthook's own select) while a local player is
/// up is sent straight back.
#[test]
fn a_header_golfer_pick_is_reasserted() {
    let mut rig = rig();
    let mut pick = GolferSelect::new("ui-1");
    pick.golfer = Some(Some("flighthook.someone".parse().expect("id")));
    rig.select_from(FLIGHTHOOK, pick);
    assert_eq!(golfer(&rig.turn), Some("flighthook.someone"));
    rig.turn_until(is_joe);
}

/// A golfer linked to the game's player (a saved golfer listing
/// `pureplay.Joe`) is another golfer by id: the card selects Joe again.
#[test]
fn a_linked_golfers_turn_is_reasserted_by_id() {
    let mut rig = rig();
    let mut sam = rig.turn.clone();
    sam.golfer = Some("flighthook.sam".parse().expect("id"));
    rig.turn_to(sam);
    rig.turn_until(is_joe);
    let last = rig.selects().pop().expect("a select");
    assert_eq!(last.golfer, Some(Some(joe())));
}

/// A header club pick for a club Joe's bag lacks: club out takes no pick,
/// and the card's complete select puts Pureplay's club back.
#[test]
fn a_header_club_pick_is_reasserted_while_club_out_is_off() {
    let mut rig = rig();
    rig.pick(Club::Wood5);
    assert_eq!(rig.turn.club, Some(Club::Wood5));
    rig.turn_until(is_joe);
}

/// The mode follows the club and the distance to the hole: unknown is a
/// full swing, 150 m full, 20 m chipping, the putter putting.
#[test]
fn the_mode_follows_the_club_and_the_distance() {
    let mut rig = rig();
    let init = |id: &str, m: f64| {
        json!({ "shotId": id, "matchId": "m-1", "playerId": 1, "holeNumber": 7, "strokeNumber": 2,
                "ballLocation": [1.0, 2.0, 3.0], "club": "Iron7", "clubId": "b-7i",
                "distanceToHoleM": m, "replacesShotId": null })
    };
    rig.event("shot.initialized", 2, &init("s-2", 20.0));
    rig.turn_until(|t| t.mode == Some(Mode::Chipping) && t.club == Some(Club::Iron7));
    rig.event("shot.initialized", 3, &init("s-3", 150.0));
    rig.turn_until(|t| t.mode == Some(Mode::Full));
    rig.event(
        "player.club_changed",
        4,
        &json!({ "playerId": 1, "current": "Putter", "currentClubId": "b-pt", "reason": "user_selected" }),
    );
    rig.turn_until(|t| t.mode == Some(Mode::Putting) && t.club == Some(Club::Putter));
}

/// `game.aim` to the card is answered `unsupported`.
#[test]
fn game_aim_is_answered_unsupported() {
    let mut rig = rig();
    rig.aim(
        "tool",
        GameAim {
            r#ref: "aim-1".into(),
            action: AimAction::Nudge,
            degrees: Some(2.0),
            target: None,
        },
    );
    let FrpBody::BusReply(reply) = rig.frp_until(|b| matches!(b, FrpBody::BusReply(_))) else {
        unreachable!()
    };
    assert_eq!(
        (
            reply.to.as_str(),
            reply.command.as_str(),
            reply.r#ref.as_str()
        ),
        ("tool", "game.aim", "aim-1")
    );
    assert!(!reply.ok);
    assert_eq!(reply.error, Some(ReplyError::Unsupported));
}

/// The card publishes every player's profile, the round, the ball and the
/// scores; a player who leaves is removed; a round that ended goes out
/// ended, then removed. Its status lists Pureplay's blockers, and the
/// driver issues the `pureplay` namespace as the bus's game.
#[test]
fn the_card_publishes_profiles_the_round_the_ball_and_the_scores() {
    let info = Accessory::info(&Config::default());
    assert_eq!(info.namespaces, [model::NAMESPACE]);
    assert!(info.roles.contains(&flightrelay::types::roles::GAME));
    let mut rig = rig();
    let round = rig
        .frps()
        .into_iter()
        .find_map(|b| match b {
            FrpBody::GameRound(r) => Some(r),
            _ => None,
        })
        .expect("the round");
    assert_eq!(round.id, "m-1");
    assert_eq!(round.status, RoundStatus::Playing);
    assert_eq!(round.scoring.as_deref(), Some("stroke"));
    assert_eq!(round.course.and_then(|c| c.name).as_deref(), Some("Course"));
    let local: Vec<_> = round
        .players
        .iter()
        .map(|p| (p.golfer.as_str().to_owned(), p.local))
        .collect();
    assert_eq!(
        local,
        [
            ("pureplay.Joe".to_owned(), true),
            ("pureplay.Ann".to_owned(), true),
            ("pureplay.Remote".to_owned(), false)
        ]
    );
    assert_eq!(
        round.up.as_ref().map(GolferId::as_str),
        Some("pureplay.Joe")
    );
    let hole = round.hole.expect("hole");
    assert_eq!((hole.number, hole.par), (7, Some(4)));
    assert_eq!(
        serde_json::to_value(hole.length).expect("json"),
        json!("410yd")
    );
    let ball = rig.balls().pop().expect("the ball");
    assert_eq!(
        (ball.golfer.as_str(), ball.hole, ball.stroke_number),
        ("pureplay.Joe", 7, 1)
    );
    assert_eq!(ball.allowed_commands, None, "never game.aim");
    let profiles: Vec<_> = rig
        .frps()
        .into_iter()
        .filter_map(|b| match b {
            FrpBody::GolferProfile(p) => Some(p),
            _ => None,
        })
        .collect();
    let joe = profiles
        .iter()
        .find(|p| p.id.as_str() == "pureplay.Joe")
        .expect("joe");
    assert_eq!(joe.name, "Joe");
    assert_eq!(joe.handedness, Some(Handedness::Right));
    assert_eq!(joe.bag, [Club::Driver, Club::Iron7, Club::Iron8]);
    let remote = profiles
        .iter()
        .find(|p| p.id.as_str() == "pureplay.Remote")
        .expect("remote");
    assert_eq!(remote.handedness, None, "Unknown is omitted");

    rig.event(
        "shot.ready_changed",
        2,
        &json!({ "shotId": "shot-1", "playerId": 1, "ready": false, "controllable": false,
                 "allowedCommands": [], "blockers": ["shot_in_progress"] }),
    );
    rig.until("Pureplay's blockers", |rig| {
        rig.device.blockers == ["shot_in_progress"]
    });
    rig.event(
        "score.updated",
        3,
        &json!({ "matchId": "m-1", "shotId": "shot-1", "holeNumber": 7, "playerId": 1,
                 "teamIndex": null, "holeStrokes": 3, "totalStrokes": 32, "grossToPar": -1,
                 "netToPar": -2, "holeResult": "Birdie", "holedOut": true, "final": true,
                 "reason": "hole_completed" }),
    );
    let FrpBody::GameScores(scores) = rig.frp_until(|b| matches!(b, FrpBody::GameScores(_))) else {
        unreachable!()
    };
    assert_eq!(scores.round, "m-1");
    let joe = &scores.players[0];
    assert_eq!(
        (joe.golfer.as_str(), joe.strokes),
        ("pureplay.Joe", Some(32))
    );
    assert_eq!(joe.holes[0].number, 7);
    assert_eq!(
        (
            joe.holes[0].strokes,
            joe.holes[0].to_par,
            joe.holes[0].r#final
        ),
        (Some(3), Some(-1), Some(true))
    );

    rig.event("player.left", 4, &json!({ "playerId": 3 }));
    rig.frp_until(|b| {
        matches!(b, FrpBody::Removed(r)
            if r.kind == StateKind::GolferProfile && r.id.as_deref() == Some("pureplay.Remote"))
    });
    rig.event(
        "match.ended",
        5,
        &json!({ "matchId": "m-1", "reason": "completed", "standings": [] }),
    );
    rig.frp_until(|b| {
        matches!(b, FrpBody::GameRound(r)
            if r.status == RoundStatus::Ended && r.end_reason == Some(EndReason::Completed))
    });
    rig.frp_until(|b| matches!(b, FrpBody::Removed(r) if r.kind == StateKind::GameRound));
}

/// `bus.sync` from another actor gets the card's game state again: the
/// profiles and the round.
#[test]
fn bus_sync_gets_the_game_state_again() {
    let mut rig = rig();
    let from = rig.frps().len();
    rig.sync();
    rig.caught_up();
    let again = rig.frps_since(from);
    assert!(
        again.iter().any(|b| matches!(b, FrpBody::GameRound(_))),
        "{again:?}"
    );
    assert_eq!(
        again
            .iter()
            .filter(|b| matches!(b, FrpBody::GolferProfile(_)))
            .count(),
        3
    );
}

/// `context.invalidated` removes the round.
#[test]
fn context_invalidated_removes_the_round() {
    let mut rig = rig();
    rig.event(
        "context.invalidated",
        2,
        &json!({ "reason": "ended", "matchId": "m-1" }),
    );
    rig.frp_until(|b| matches!(b, FrpBody::Removed(r) if r.kind == StateKind::GameRound));
}

/// A dropped link with the round unchanged and flighthook's turn still
/// Joe's: the turn matches, so the card sends nothing more.
#[test]
fn a_reconnect_with_an_unchanged_round_sends_no_select() {
    let mut rig = rig();
    let before = rig.selects().len();
    assert_eq!(before, 1, "one select for the snapshot");
    rig.reconnect_to(&round());
    assert_eq!(rig.selects().len(), before, "the turn already matches");
}

/// A restarted card finds flighthook's turn already matching Pureplay's.
#[test]
fn a_card_restart_with_a_matching_turn_sends_no_select() {
    let mut rig = rig();
    let before = rig.selects().len();
    rig.drop_link();
    rig.link_ends();
    rig.reconnect();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some(TOKEN));
    rig.paired(TOKEN);
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "club", "7I");
    rig.caught_up();
    assert_eq!(rig.selects().len(), before, "the turn already matches");
}

/// With the turn gone back to no golfer while the link was down, the card
/// takes it once on the reconnect, and no more.
#[test]
fn a_reconnect_after_the_turn_went_back_sends_one_select() {
    let mut rig = rig();
    let before = rig.selects().len();
    rig.drop_link();
    rig.link_ends();
    rig.turn = GolferTurn::new(None, None, Handedness::Right, Mode::Full);
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some(TOKEN));
    rig.paired(TOKEN);
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "club", "7I");
    rig.caught_up();
    assert_eq!(rig.selects().len(), before + 1, "one, not a stream");
    assert!(is_joe(&rig.turn));
}

/// A gimme, the next tee and the driver: the turn changes once, to the
/// driver in a full swing, with no chipping on the way.
#[test]
fn a_gimme_then_the_next_tee_changes_the_turn_once() {
    let mut rig = rig();
    let before = rig.turns.len();
    rig.event(
        "shot.completed",
        2,
        &json!({ "shotId": "s-9", "matchId": "m-1", "playerId": 1, "holeNumber": 7,
                 "strokeNumber": 3, "outcome": "Gimme", "endDistToHoleM": 0.8,
                 "lie": "Green" }),
    );
    rig.event(
        "hole.started",
        3,
        &json!({ "matchId": "m-1", "holeNumber": 8, "par": 5 }),
    );
    rig.event(
        "player.club_changed",
        4,
        &json!({ "playerId": 1, "current": "Driver", "currentClubId": "b-dr", "reason": "auto" }),
    );
    rig.event(
        "shot.initialized",
        5,
        &json!({ "shotId": "s-10", "matchId": "m-1", "playerId": 1, "holeNumber": 8,
                 "strokeNumber": 1, "ballLocation": [0.0, 0.0, 0.0], "club": "Driver",
                 "clubId": "b-dr", "distanceToHoleM": 375.0 }),
    );
    rig.turn_until(|t| t.club == Some(Club::Driver) && t.mode == Some(Mode::Full));
    rig.caught_up();
    let turns: Vec<_> = rig.turns[before..]
        .iter()
        .map(|t| (t.club, t.mode))
        .collect();
    assert_eq!(turns, [(Some(Club::Driver), Some(Mode::Full))]);
}

/// The tee ball of the next hole carries no lie from the last stroke of
/// the hole before; a `lie.changed` of the shot up gives it one, another
/// shot's does not.
#[test]
fn the_ball_carries_only_the_lie_of_its_own_shot() {
    let mut rig = rig();
    rig.event(
        "shot.completed",
        2,
        &json!({ "shotId": "s-9", "matchId": "m-1", "playerId": 1, "holeNumber": 7,
                 "strokeNumber": 3, "outcome": "Holed", "endDistToHoleM": 0.0, "lie": "Green" }),
    );
    rig.event(
        "hole.started",
        3,
        &json!({ "matchId": "m-1", "holeNumber": 8, "par": 5 }),
    );
    rig.event(
        "shot.initialized",
        4,
        &json!({ "shotId": "s-10", "matchId": "m-1", "playerId": 1, "holeNumber": 8,
                 "strokeNumber": 1, "ballLocation": [0.0, 0.0, 0.0], "club": "Iron7",
                 "clubId": "b-7i", "distanceToHoleM": 137.0 }),
    );
    rig.frp_until(|b| matches!(b, FrpBody::GameBall(ball) if ball.hole == 8));
    rig.caught_up();
    let tee = rig
        .balls()
        .into_iter()
        .rev()
        .find(|b| b.hole == 8)
        .expect("the tee ball");
    assert_eq!(tee.lie, None, "no surface from the last putt");
    rig.event(
        "lie.changed",
        5,
        &json!({ "shotId": "s-77", "playerId": 1, "surface": "Rough" }),
    );
    rig.event(
        "lie.changed",
        6,
        &json!({ "shotId": "s-10", "playerId": 1, "surface": "Tee", "slopeUpDownDeg": 1.5 }),
    );
    rig.frp_until(|b| matches!(b, FrpBody::GameBall(ball) if ball.lie.is_some()));
    rig.caught_up();
    let with = rig
        .balls()
        .into_iter()
        .rev()
        .find_map(|b| b.lie)
        .expect("a lie");
    assert_eq!(with.surface.as_deref(), Some("tee"));
    assert_eq!(with.slope_up_down, Some(1.5));
    assert!(
        rig.balls()
            .into_iter()
            .filter_map(|b| b.lie)
            .all(|l| l.surface.as_deref() != Some("rough")),
        "another shot's lie"
    );
}

/// A mulligan: the stroke of the replaying shot names the one it replaces.
#[test]
fn a_mulligan_stroke_names_the_stroke_it_replaces() {
    let mut rig = rig();
    rig.event("shot.launched", 2, &launched("shot-42", 1));
    rig.event("shot.completed", 3, &completed("shot-42", 1, "InPlay"));
    rig.event(
        "shot.replayed",
        4,
        &json!({ "shotId": "shot-51", "replacesShotId": "shot-42", "playerId": 1,
                 "reason": "mulligan" }),
    );
    rig.event("shot.launched", 5, &launched("shot-51", 1));
    rig.event("shot.completed", 6, &completed("shot-51", 1, "Green"));
    rig.caught_up();
    let strokes = rig.strokes();
    let named: Vec<_> = strokes
        .iter()
        .map(|s| (s.stroke_id.as_str(), s.replaces.as_deref()))
        .collect();
    assert_eq!(
        named,
        [("shot-42", None), ("shot-51", Some("shot-42"))],
        "{named:?}"
    );
}

/// A discard of a shot the card never mapped (a remote player's) still
/// stops a stroke that arrives after it.
#[test]
fn a_discard_is_remembered_for_a_shot_the_card_never_mapped() {
    let mut rig = rig();
    rig.event("shot.launched", 2, &launched("pp-9", 3));
    rig.event(
        "shot.cancelled",
        3,
        &json!({ "shotId": "pp-9", "playerId": 3, "reason": "discarded" }),
    );
    rig.event("shot.completed", 4, &completed("pp-9", 3, "InPlay"));
    rig.event("shot.launched", 5, &launched("pp-10", 1));
    rig.event("shot.completed", 6, &completed("pp-10", 1, "InPlay"));
    rig.caught_up();
    let ids: Vec<_> = rig.strokes().into_iter().map(|s| s.stroke_id).collect();
    assert_eq!(ids, ["pp-10"]);
}

/// A round that ends takes its golfers and itself off the bus; the card
/// sends no select of its own (the turn going back to the start golfer is
/// flighthook's).
#[test]
fn the_end_of_the_round_takes_its_golfers_off_the_bus() {
    let mut rig = rig();
    rig.event(
        "shot.initialized",
        2,
        &json!({ "shotId": "s-2", "playerId": 1, "holeNumber": 7, "strokeNumber": 2,
                 "club": "Iron7", "distanceToHoleM": 20.0 }),
    );
    rig.turn_until(|t| t.mode == Some(Mode::Chipping));
    rig.caught_up();
    let (from, selects) = (rig.frps().len(), rig.selects().len());
    rig.event(
        "match.ended",
        3,
        &json!({ "matchId": "m-1", "reason": "completed", "standings": [] }),
    );
    rig.frp_until(|b| matches!(b, FrpBody::Removed(r) if r.kind == StateKind::GameRound));
    rig.caught_up();
    let mut removed: Vec<String> = rig
        .frps_since(from)
        .into_iter()
        .filter_map(|b| match b {
            FrpBody::Removed(r) if r.kind == StateKind::GolferProfile => r.id,
            _ => None,
        })
        .collect();
    removed.sort();
    assert_eq!(removed, ["pureplay.Ann", "pureplay.Joe", "pureplay.Remote"]);
    assert_eq!(rig.selects().len(), selects, "no select");
}

/// The link ending takes the card's golfers and round off the bus, and the
/// card sends no select while no link is up.
#[test]
fn a_closed_connection_takes_the_cards_game_state_off_the_bus() {
    let mut rig = rig();
    let (from, selects) = (rig.frps().len(), rig.selects().len());
    rig.drop_link();
    rig.link_ends();
    let gone = rig.frps_since(from);
    for id in ["pureplay.Ann", "pureplay.Joe", "pureplay.Remote"] {
        assert!(
            gone.iter().any(|b| matches!(b, FrpBody::Removed(r)
                if r.kind == StateKind::GolferProfile && r.id.as_deref() == Some(id))),
            "{id}: {gone:?}"
        );
    }
    assert!(
        gone.iter()
            .any(|b| matches!(b, FrpBody::Removed(r) if r.kind == StateKind::GameRound))
    );
    rig.turn_to(GolferTurn::new(None, None, Handedness::Left, Mode::Full));
    rig.run_for(Duration::from_millis(500));
    assert_eq!(rig.selects().len(), selects, "nothing while no link is up");
}

// -- a second match in the same Pureplay run ----------------------------------

/// A match that starts after another one ended, announced by events only:
/// the card asks for the state, the answer brings the players back, and
/// the match's shot is reported and the turn selected once.
#[test]
fn a_second_match_announced_by_events_is_taken_once_the_state_answers() {
    let mut rig = rig();
    rig.first_match_with_a_shot();
    let (triggers, selects) = (rig.triggers().len(), rig.selects().len());
    rig.second_match_events(5);
    let get = rig.next_state_get();
    rig.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 9,
                 "requestId": get["id"], "data": round_of("m-2") }),
    );
    rig.turn_until(is_joe);
    rig.event("shot.launched", 10, &launched("pp-2", 1));
    rig.frp_until(|b| matches!(b, FrpBody::ShotFinished(_)));
    rig.caught_up();
    assert_eq!(rig.triggers().len(), triggers + 1, "the shot is reported");
    assert_eq!(
        rig.selects().len(),
        selects + 1,
        "one select for the new match"
    );
}

/// The same with no answer to `state.get`: the card reports no shot and
/// sends no select.
#[test]
fn a_second_match_with_no_state_answer_stays_quiet() {
    let mut rig = rig();
    rig.first_match_with_a_shot();
    let (triggers, selects, turns) = (rig.triggers().len(), rig.selects().len(), rig.turns.len());
    rig.second_match_events(5);
    let _ = rig.next_state_get();
    rig.event("shot.launched", 9, &launched("pp-2", 1));
    rig.event("shot.completed", 10, &completed("pp-2", 1, "InPlay"));
    rig.caught_up();
    assert_eq!(rig.triggers().len(), triggers, "no shot is reported");
    assert_eq!(rig.selects().len(), selects, "no select is sent");
    assert_eq!(rig.turns.len(), turns, "the turn stays");
}

/// A `state.get` that gets no answer at a second match marks nobody: it
/// goes again after the timeout and the wait, and the shot is reported once
/// a snapshot brings the player.
#[test]
fn an_unanswered_state_get_asks_for_the_player_again() {
    let mut rig = rig();
    rig.first_match_with_a_shot();
    rig.second_match_events(5);
    let first = rig.next_state_get();
    let again = rig.next_state_get();
    assert_ne!(again["id"], first["id"], "a new request");
    rig.answer_and_take_the_shot(&again, 9);
}

/// The same with the request refused (`rate_limited`): it is asked again
/// after the wait, and the shot is reported once a snapshot answers.
#[test]
fn a_refused_state_get_is_asked_again_after_a_wait() {
    let mut rig = rig();
    rig.first_match_with_a_shot();
    rig.second_match_events(5);
    let first = rig.next_state_get();
    rig.send(&json!({ "type": "response", "id": first["id"], "ok": false,
                      "error": { "code": "rate_limited", "message": "Slow down.",
                                 "retryAfterMs": 50 } }));
    let refused = rig.host.now();
    let again = rig.next_state_get();
    assert_ne!(again["id"], first["id"], "a new request");
    assert!(
        rig.host.now() - refused >= STATE_GET_RETRY[0],
        "after the wait"
    );
    rig.answer_and_take_the_shot(&again, 9);
}

/// A player named while a request is out is asked for by the next request
/// when the answer does not bring them, not marked.
#[test]
fn a_player_named_while_a_request_is_out_is_asked_for_by_the_next() {
    let mut rig = rig();
    rig.first_match_with_a_shot();
    rig.event(
        "match.started",
        5,
        &json!({ "matchId": "m-2", "courseName": "Course", "holeCount": 18, "totalPar": 72,
                 "scoringModel": "Stroke", "isTeamGame": false, "playerIds": [1, 2, 3] }),
    );
    let first = rig.next_state_get();
    rig.event(
        "turn.changed",
        6,
        &json!({ "matchId": "m-2", "previousPlayerId": null, "currentPlayerId": 1,
                 "holeNumber": 1, "reason": "next_player" }),
    );
    let mut empty = round_of("m-2");
    empty["players"] = json!([]);
    empty["turn"] = json!({ "activeShooterId": null, "remainingPlayerIds": [] });
    empty["shot"] = Value::Null;
    empty["readiness"] = Value::Null;
    rig.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 7,
                 "requestId": first["id"], "data": empty }),
    );
    let next = rig.next_state_get();
    rig.answer_and_take_the_shot(&next, 8);
}

/// An event that names a player the card does not hold asks for the state,
/// with no `match.started` before it.
#[test]
fn an_event_naming_an_unknown_player_asks_for_the_state() {
    let mut rig = rig();
    rig.first_match_with_a_shot();
    rig.event(
        "turn.changed",
        5,
        &json!({ "previousPlayerId": null, "currentPlayerId": 1, "holeNumber": 1,
                 "reason": "next_player" }),
    );
    let _ = rig.next_state_get();
}

/// A stroke after its round ended is dropped: it would be under an id the
/// player never had.
#[test]
fn a_shot_completed_after_the_round_ended_sends_no_stroke() {
    let mut rig = rig();
    rig.first_match_with_a_shot();
    let from = rig.frps().len();
    rig.event("shot.completed", 5, &completed("pp-late", 1, "InPlay"));
    rig.caught_up();
    let own = rig.frps_since(from);
    assert!(
        own.iter().all(|b| !matches!(
            b,
            FrpBody::GameStroke(_) | FrpBody::GolferProfile(_) | FrpBody::GameRound(_)
        )),
        "{own:?}"
    );
}

/// Joe's `club.set` for a header pick of the 8 iron.
fn club_set(rig: &mut Rig) -> Value {
    rig.pick(Club::Iron8);
    loop {
        let msg = rig.recv();
        if msg["command"] == "club.set" {
            return msg;
        }
    }
}

/// A reply to `club.set` that lands after the player left publishes no
/// golfer for them: a player who left is not made up.
#[test]
fn a_late_club_set_reply_for_a_player_who_left_publishes_no_golfer() {
    let mut rig = rig();
    let cmd = club_set(&mut rig);
    rig.event("player.left", 2, &json!({ "playerId": 1 }));
    rig.frp_until(|b| {
        matches!(b, FrpBody::Removed(r)
            if r.kind == StateKind::GolferProfile && r.id.as_deref() == Some("pureplay.Joe"))
    });
    rig.caught_up();
    let from = rig.frps().len();
    rig.send(&json!({ "type": "response", "id": cmd["id"], "ok": true,
                      "data": { "playerId": 1, "club": "Iron8", "clubId": "b-8i" } }));
    // A reply publishes nothing by itself: the next event shows what the
    // card holds.
    rig.event(
        "hole.started",
        3,
        &json!({ "matchId": "m-1", "holeNumber": 8, "par": 4, "playerIds": [2, 3] }),
    );
    rig.caught_up();
    let own = rig.frps_since(from);
    assert!(
        own.iter().all(|b| !matches!(b, FrpBody::GolferProfile(_))),
        "{own:?}"
    );
}

/// A reply to `club.set` that lands after the round ended publishes no
/// golfer: the players of an ended round are not made up again.
#[test]
fn a_late_club_set_reply_after_the_round_ended_publishes_no_golfer() {
    let mut rig = rig();
    let cmd = club_set(&mut rig);
    rig.event(
        "match.ended",
        2,
        &json!({ "matchId": "m-1", "reason": "completed", "standings": [] }),
    );
    rig.frp_until(|b| matches!(b, FrpBody::Removed(r) if r.kind == StateKind::GameRound));
    rig.caught_up();
    let from = rig.frps().len();
    rig.send(&json!({ "type": "response", "id": cmd["id"], "ok": true,
                      "data": { "playerId": 1, "club": "Iron8", "clubId": "b-8i" } }));
    rig.event(
        "hole.started",
        3,
        &json!({ "matchId": "m-1", "holeNumber": 8, "par": 4, "playerIds": [] }),
    );
    rig.caught_up();
    let own = rig.frps_since(from);
    assert!(
        own.iter().all(|b| !matches!(b, FrpBody::GolferProfile(_))),
        "{own:?}"
    );
}

/// `player.changed` with the fields it does not report sent as `null`
/// (Joe chipping with a sand wedge changes hands): the club and the mode
/// stay, the hand changes.
#[test]
fn a_hand_change_with_null_club_fields_keeps_the_club_and_the_mode() {
    let mut rig = rig();
    rig.event(
        "shot.initialized",
        2,
        &json!({ "shotId": "s-2", "matchId": "m-1", "playerId": 1, "holeNumber": 7,
                 "strokeNumber": 2, "ballLocation": [1.0, 2.0, 3.0], "club": "SandWedge",
                 "clubId": "b-sw", "distanceToHoleM": 15.0, "replacesShotId": null }),
    );
    rig.turn_until(|t| t.mode == Some(Mode::Chipping) && t.club == Some(Club::SandWedge));
    rig.event(
        "player.changed",
        3,
        &json!({ "playerId": 1, "reason": "handedness_changed",
                 "current": { "playerId": 1, "profileId": null, "name": "Joe", "isLocal": true,
                              "handedness": "LH", "teamIndex": null, "handicap": null,
                              "club": null, "clubId": null, "bag": null, "ball": null,
                              "aim": null, "distanceToHoleM": null, "totalStrokes": null,
                              "toPar": null } }),
    );
    let turn = rig.turn_until(|t| t.handedness == Some(Handedness::Left));
    assert_eq!(turn.club, Some(Club::SandWedge));
    assert_eq!(turn.mode, Some(Mode::Chipping));
    rig.caught_up();
    let joe = rig
        .frps()
        .into_iter()
        .filter_map(|b| match b {
            FrpBody::GolferProfile(p) if p.id.as_str() == "pureplay.Joe" => Some(p),
            _ => None,
        })
        .next_back()
        .expect("joe");
    assert_eq!(
        joe.bag,
        [Club::Driver, Club::Iron7, Club::Iron8],
        "the bag stays"
    );
}

/// The results screen after a round (`matchActive` false for the match that
/// just ended) brings no players and no shooter: no game golfer goes back
/// on the turn.
#[test]
fn the_results_screen_puts_no_game_golfer_back_on_the_turn() {
    let mut rig = rig();
    rig.first_match_with_a_shot();
    let (from, selects, turns) = (rig.frps().len(), rig.selects().len(), rig.turns.len());
    rig.event(
        "player.club_changed",
        5,
        &json!({ "playerId": 1, "current": "Driver", "currentClubId": "b-dr", "reason": "user_selected" }),
    );
    let get = rig.next_state_get();
    let mut results = round();
    results["session"]["matchActive"] = json!(false);
    rig.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 6,
                 "requestId": get["id"], "data": results }),
    );
    rig.caught_up();
    let own = rig.frps_since(from);
    assert!(
        own.iter().all(|b| !matches!(b, FrpBody::GolferProfile(_))),
        "{own:?}"
    );
    assert_eq!(rig.selects().len(), selects, "no select");
    assert!(
        rig.turns[turns..]
            .iter()
            .all(|t| golfer(t) != Some("pureplay.Joe")),
        "{:?}",
        &rig.turns[turns..]
    );
}

/// A reconnect straight into the results screen (the same match, ended,
/// its `match.ended` lost with the link) brings no players
/// and no shooter: no select, and the turn does not go back to the game's
/// player.
#[test]
fn a_reconnect_into_the_results_screen_leaves_the_turn_alone() {
    let mut rig = rig();
    let selects = rig.selects().len();
    rig.drop_link();
    rig.link_ends();
    // flighthook's turn left Joe while the link was down.
    rig.turn = GolferTurn::new(None, None, Handedness::Right, Mode::Full);
    let turns = rig.turns.len();
    let mut results = round();
    results["session"]["matchActive"] = json!(false);
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some(TOKEN));
    rig.paired(TOKEN);
    rig.subscribe(&results, 1);
    rig.shows(Status::Connected, |_| true);
    rig.caught_up();
    assert_eq!(rig.selects().len(), selects, "no select");
    assert!(
        rig.turns[turns..]
            .iter()
            .all(|t| golfer(t) != Some("pureplay.Joe")),
        "{:?}",
        &rig.turns[turns..]
    );
}

/// An event naming a player the card does not hold asks for them, and a
/// context change lets the card ask for them again after a snapshot left
/// them out. That nothing more is asked before it is
/// `a_player_the_snapshot_did_not_bring_is_asked_for_once_until_cleared`.
#[test]
fn a_player_the_snapshot_did_not_bring_is_asked_for_again_after_a_context_change() {
    let mut rig = rig();
    let names_nine = |rig: &mut Rig, seq: u64| {
        rig.event(
            "player.club_changed",
            seq,
            &json!({ "playerId": 9, "current": "Driver", "currentClubId": "x", "reason": "user_selected" }),
        );
    };
    names_nine(&mut rig, 2);
    let get = rig.next_state_get();
    rig.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 3,
                 "requestId": get["id"], "data": round() }),
    );
    // Once the context change's own request is answered, the next event
    // naming player 9 asks for them again.
    rig.event("context.invalidated", 4, &json!({ "reason": "loading" }));
    let get = rig.next_state_get();
    rig.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 5,
                 "requestId": get["id"], "data": round() }),
    );
    names_nine(&mut rig, 6);
    let again = rig.next_state_get();
    assert_ne!(again["id"], get["id"], "player 9 asked for again");
}

/// Another simulator card's select already made flighthook's turn match
/// Pureplay's change, and the host does not hand the card that turn. The
/// card's own select then changes nothing; its `ok` reply means flighthook
/// holds the turn, so the card sends it once.
#[test]
fn a_turn_another_card_already_matched_is_selected_once() {
    let mut rig = rig();
    let mut turn = rig.turn.clone();
    turn.club = Some(Club::Putter);
    turn.mode = Some(Mode::Putting);
    rig.turn = turn;
    let before = rig.selects().len();
    rig.event(
        "player.club_changed",
        2,
        &json!({ "playerId": 1, "current": "Putter", "currentClubId": "b-pt", "reason": "user_selected" }),
    );
    rig.run_for(Duration::from_secs(1));
    assert_eq!(rig.selects().len() - before, 1);
}
