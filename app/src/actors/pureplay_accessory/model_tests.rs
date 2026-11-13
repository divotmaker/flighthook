//! The model's turn: complete selects, the re-assert rule and the mode.

use super::*;
use serde_json::json;

fn event(name: &str, data: serde_json::Value) -> Event {
    Event {
        name: name.into(),
        seq: None,
        request_id: None,
        data,
    }
}

fn player(id: i64, name: &str, local: bool, hand: &str, club: &str) -> serde_json::Value {
    json!({ "playerId": id, "profileId": if local { json!(format!("local-{id}")) } else { json!(null) },
            "name": name, "isLocal": local, "handedness": hand,
            "club": club, "clubId": format!("bag-{id}-{club}"),
            "bag": [ { "clubId": format!("bag-{id}-{club}"), "type": club, "name": club },
                     { "clubId": format!("bag-{id}-pt"), "type": "Putter", "name": "Putter" } ] })
}

fn snapshot(active: i64) -> protocol::Snapshot {
    serde_json::from_value(snapshot_value(active)).expect("snapshot")
}

fn snapshot_value(active: i64) -> serde_json::Value {
    json!({
        "session": { "matchId": "m-1", "matchActive": true, "scoringModel": "Stroke" },
        "course": { "displayName": "St Andrews", "holeCount": 18, "totalPar": 72 },
        "hole": { "number": 7, "par": 4, "yards": 410 },
        "turn": { "activeShooterId": active },
        "players": [ player(1, "Joe", true, "RH", "Iron7"),
                     player(2, "Ann", true, "LH", "Driver"),
                     player(3, "Remote", false, "Unknown", "Putter") ],
        "readiness": { "ready": false, "blockers": ["not_in_play"] }
    })
}

/// The lookup rule with no golfers: every id is its own key.
fn own(id: Option<&str>) -> Option<String> {
    id.map(str::to_owned)
}

/// A model that knows flighthook's start turn: no golfer, no club, right-
/// handed, full.
fn fresh() -> Model {
    let mut model = Model::default();
    model.flighthook_holds(
        &GolferTurn::new(None, None, Handedness::Right, Mode::Full),
        None,
    );
    model
}

fn no_cards(_: &str) -> bool {
    false
}

fn id(s: &str) -> GolferId {
    s.parse().expect("golfer id")
}

/// flighthook's turn for `want`, as the session publishes it.
fn turn_for(want: &Want) -> GolferTurn {
    GolferTurn::new(
        Some(want.golfer.clone()),
        want.club,
        want.hand.unwrap_or(Handedness::Right),
        want.mode,
    )
}

/// The card sent `want` and the session answered: flighthook holds it,
/// caused by the card.
fn answered(model: &mut Model, want: &Want) {
    model.select_sent("card", "c");
    model.flighthook_holds(&turn_for(want), Some("card"));
    assert!(model.select_answered("card", "c"));
}

#[test]
fn the_snapshot_gives_the_local_player_ups_complete_turn() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let want = model.sync(own, no_cards).expect("a select");
    assert_eq!(
        want,
        Want {
            golfer: id("pureplay.Joe"),
            club: Some(Club::Iron7),
            unmapped: false,
            hand: Some(Handedness::Right),
            mode: Mode::Full,
        }
    );
    let select = want.select("r".into());
    assert_eq!(select.golfer, Some(Some(id("pureplay.Joe"))));
    assert_eq!(select.club, Some(Some(Club::Iron7)));
    assert_eq!(select.handedness, Some(Handedness::Right));
    assert_eq!(select.mode, Some(Mode::Full));
    assert_eq!(model.hole_text().as_deref(), Some("7, par 4"));
    // Once flighthook holds it, the same snapshot sends nothing.
    answered(&mut model, &want);
    model.apply_snapshot(&snapshot(1));
    assert_eq!(model.sync(own, no_cards), None);
}

/// Two local players: a turn to the second sends the golfer, club, hand and
/// mode together, so the golfer changes with no club flip in between.
#[test]
fn a_turn_to_a_second_local_player_sends_one_complete_select() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let joe = model.sync(own, no_cards).expect("joe");
    answered(&mut model, &joe);
    let turn = event(
        "turn.changed",
        json!({ "previousPlayerId": 1, "currentPlayerId": 2, "reason": "next_player" }),
    );
    model.apply_event(&turn).expect("turn");
    let ann = model.sync(own, no_cards).expect("ann");
    assert_eq!(ann.golfer, id("pureplay.Ann"));
    assert_eq!(ann.club, Some(Club::Driver));
    assert_eq!(ann.hand, Some(Handedness::Left));
    // Nothing more until it is answered, and nothing after.
    model.select_sent("card", "c2");
    assert_eq!(model.sync(own, no_cards), None);
    model.flighthook_holds(&turn_for(&ann), Some("card"));
    model.select_answered("card", "c2");
    assert_eq!(model.sync(own, no_cards), None);
}

/// A remote player up sends nothing and re-asserts nothing: the turn keeps
/// the last local golfer, and the header is free.
#[test]
fn a_remote_player_up_sends_nothing() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let joe = model.sync(own, no_cards).expect("joe");
    answered(&mut model, &joe);
    let turn = event(
        "turn.changed",
        json!({ "previousPlayerId": 1, "currentPlayerId": 3 }),
    );
    model.apply_event(&turn).expect("turn");
    assert_eq!(model.sync(own, no_cards), None);
    let pick = GolferTurn::new(None, Some(Club::Wood5), Handedness::Right, Mode::Full);
    model.flighthook_holds(&pick, Some("flighthook"));
    assert_eq!(model.sync(own, no_cards), None);
    assert_eq!(model.golfer(3), Some(id("pureplay.Remote")));
}

/// A header golfer or club pick (caused by flighthook) is re-asserted; a
/// turn a game or simulator card caused is not; Pureplay reporting a
/// change sends whatever caused the turn.
#[test]
fn the_card_reasserts_against_changes_no_game_card_made() {
    let gspro = |a: &str| a == "gspro";
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let joe = model.sync(own, gspro).expect("joe");
    answered(&mut model, &joe);

    let header_golfer = GolferTurn {
        golfer: Some(id("flighthook.abc")),
        ..turn_for(&joe)
    };
    model.flighthook_holds(&header_golfer, Some("flighthook"));
    assert_eq!(model.sync(own, gspro), Some(joe.clone()), "golfer pick");
    answered(&mut model, &joe);

    let header_club = GolferTurn {
        club: Some(Club::Wood5),
        ..turn_for(&joe)
    };
    model.flighthook_holds(&header_club, Some("flighthook"));
    assert_eq!(model.sync(own, gspro), Some(joe.clone()), "club pick");
    answered(&mut model, &joe);

    let from_gspro = GolferTurn {
        club: Some(Club::Iron8),
        ..turn_for(&joe)
    };
    model.flighthook_holds(&from_gspro, Some("gspro"));
    assert_eq!(model.sync(own, gspro), None, "last sender wins");

    // Pureplay reports Joe's club change: the card sends.
    let change = event(
        "player.club_changed",
        json!({ "playerId": 1, "current": "Iron9", "currentClubId": "x", "reason": "user_selected" }),
    );
    model.apply_event(&change).expect("club");
    let want = model.sync(own, gspro).expect("reported");
    assert_eq!(want.club, Some(Club::Iron9));
}

/// The golfer compares by the lookup rule: a flighthook golfer listing the
/// player's id is the same golfer.
#[test]
fn a_linked_golfer_is_the_same_golfer() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let joe = model.sync(own, no_cards).expect("joe");
    answered(&mut model, &joe);
    let linked = |id: Option<&str>| {
        id.map(|id| {
            if id == "pureplay.Joe" {
                "flighthook.eric".to_owned()
            } else {
                id.to_owned()
            }
        })
    };
    let picked = GolferTurn {
        golfer: Some(id("flighthook.eric")),
        ..turn_for(&joe)
    };
    model.flighthook_holds(&picked, Some("flighthook"));
    assert_eq!(model.sync(linked, no_cards), None, "same golfer");
    assert_eq!(model.sync(own, no_cards), Some(joe), "another golfer");
}

/// The mode: the putter putts; under 30 yd chips; otherwise, or with no
/// distance, a full swing. A new distance that changes the mode sends.
#[test]
fn the_club_and_distance_give_the_mode() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    assert_eq!(model.want().expect("joe").mode, Mode::Full, "unknown");
    let joe = model.sync(own, no_cards).expect("joe");
    answered(&mut model, &joe);
    for (distance, mode) in [(150.0, Mode::Full), (20.0, Mode::Chipping)] {
        let init = event(
            "shot.initialized",
            json!({ "shotId": format!("s-{distance}"), "playerId": 1, "holeNumber": 7,
                    "strokeNumber": 2, "distanceToHoleM": distance }),
        );
        model.apply_event(&init).expect("init");
        assert_eq!(model.want().expect("joe").mode, mode, "{distance} m");
        if let Some(want) = model.sync(own, no_cards) {
            answered(&mut model, &want);
        }
    }
    let putter = event(
        "player.club_changed",
        json!({ "playerId": 1, "current": "Putter", "currentClubId": "p", "reason": "auto" }),
    );
    model.apply_event(&putter).expect("club");
    let want = model.sync(own, no_cards).expect("putter");
    assert_eq!((want.club, want.mode), (Some(Club::Putter), Mode::Putting));
}

/// While a pick holds the club, the card neither sends nor compares it;
/// the mode follows the club picked.
#[test]
fn a_held_pick_keeps_the_club_out_of_the_select() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let joe = model.sync(own, no_cards).expect("joe");
    answered(&mut model, &joe);
    model.hold_club(true);
    let picked = GolferTurn {
        club: Some(Club::Wood5),
        ..turn_for(&joe)
    };
    model.flighthook_holds(&picked, Some("flighthook"));
    assert_eq!(model.sync(own, no_cards), None);
    // The pick ended with Pureplay keeping the 7 iron: back it goes.
    model.hold_club(false);
    assert_eq!(model.sync(own, no_cards), Some(joe));
}

/// Falling behind forgets the unanswered selects and compares the current
/// turn at once.
#[test]
fn falling_behind_compares_the_current_turn() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let joe = model.sync(own, no_cards).expect("joe");
    answered(&mut model, &joe);
    model.select_sent("web", "w-1");
    assert_eq!(model.sync(own, no_cards), None, "unanswered");
    let other = GolferTurn {
        club: Some(Club::Driver),
        ..turn_for(&joe)
    };
    model.lagged(&other, None);
    assert_eq!(model.sync(own, no_cards), Some(joe));
    assert!(!model.select_answered("web", "w-1"));
}

/// An unknown hand is not sent and not compared; a club type with no code
/// is sent as `club: null`, and a turn that holds a club differs.
#[test]
fn an_unknown_hand_or_club_is_left_out() {
    let mut model = fresh();
    let mut snap = snapshot(1);
    snap.players[0].handedness = Some("Unknown".into());
    snap.players[0].club = Some("Chipper".into());
    model.apply_snapshot(&snap);
    let want = model.sync(own, no_cards).expect("joe");
    assert_eq!((want.club, want.hand), (None, None));
    assert!(want.unmapped);
    let select = want.select("r".into());
    assert_eq!((select.club, select.handedness), (Some(None), None));
    // A turn that already holds no club is the same.
    let mut turn = turn_for(&want);
    turn.club = None;
    model.select_sent("card", "c");
    model.flighthook_holds(&turn, Some("card"));
    model.select_answered("card", "c");
    assert_eq!(model.sync(own, no_cards), None);
    // The header's club on it is put back to none.
    turn.club = Some(Club::Driver);
    model.flighthook_holds(&turn, Some("flighthook"));
    assert!(model.sync(own, no_cards).is_some());
}

/// Falling behind seeds the cause too: a turn a simulator card caused is
/// not answered.
#[test]
fn falling_behind_keeps_the_cause_of_the_turn() {
    let gspro = |a: &str| a == "gspro";
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let joe = model.sync(own, gspro).expect("joe");
    answered(&mut model, &joe);
    let from_gspro = GolferTurn {
        club: Some(Club::Iron8),
        ..turn_for(&joe)
    };
    model.select_sent("web", "w-1");
    model.lagged(&from_gspro, Some("gspro"));
    assert_eq!(model.sync(own, gspro), None, "last sender wins");
    model.lagged(&from_gspro, Some("flighthook"));
    assert_eq!(model.sync(own, gspro), Some(joe));
}

/// No turn known is no difference: nothing is sent until the card holds
/// flighthook's turn, and a turn kept across a reset still counts.
#[test]
fn no_known_turn_sends_nothing_and_a_reset_keeps_it() {
    let mut model = Model::default();
    model.apply_snapshot(&snapshot(1));
    assert_eq!(model.sync(own, no_cards), None);
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let joe = model.sync(own, no_cards).expect("joe");
    answered(&mut model, &joe);
    model.forget_held();
    model.reset();
    model.apply_snapshot(&snapshot(1));
    assert_eq!(model.sync(own, no_cards), None, "the turn is still held");
}

/// A stroke's end distance is not the next shot's: across a gimme, the
/// next hole and the driver, the mode is `Full` from the new club on and
/// is never chipping on the way.
#[test]
fn the_mode_takes_its_distance_only_from_the_shot_set_up() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    let joe = model.sync(own, no_cards).expect("joe");
    answered(&mut model, &joe);
    let mut modes = Vec::new();
    let mut step = |model: &mut Model, name: &str, data: serde_json::Value| {
        model.apply_event(&event(name, data)).expect(name);
        if let Some(want) = model.sync(own, no_cards) {
            modes.push((want.club, want.mode));
            answered(model, &want);
        }
    };
    step(
        &mut model,
        "shot.completed",
        json!({ "shotId": "s-1", "playerId": 1, "holeNumber": 7, "strokeNumber": 3,
                "outcome": "Gimme", "endDistToHoleM": 0.8, "lie": "Green" }),
    );
    step(
        &mut model,
        "hole.started",
        json!({ "matchId": "m-1", "holeNumber": 8, "par": 5 }),
    );
    step(
        &mut model,
        "player.club_changed",
        json!({ "playerId": 1, "current": "Driver", "currentClubId": "d" }),
    );
    step(
        &mut model,
        "shot.initialized",
        json!({ "shotId": "s-2", "playerId": 1, "holeNumber": 8, "strokeNumber": 1,
                "club": "Driver", "distanceToHoleM": 375.0 }),
    );
    assert_eq!(modes, [(Some(Club::Driver), Mode::Full)]);
}

/// A lie belongs to its shot: the snapshot's to the shot it names,
/// `lie.changed`'s to its `shotId`; the ball of another shot has none.
#[test]
fn a_lie_belongs_to_its_shot() {
    let mut model = fresh();
    let mut snap = snapshot(1);
    snap.shot = Some(protocol::SnapshotShot {
        shot_id: Some("s-1".into()),
        player_id: Some(1),
        hole_number: Some(7),
        stroke_number: Some(1),
    });
    snap.lie = Some(protocol::SnapshotLie {
        surface: Some("Tee".into()),
        slope_up_down_deg: Some(3.0),
        slope_left_right_deg: None,
    });
    model.apply_snapshot(&snap);
    assert_eq!(
        model.lie().and_then(|l| l.slope_up_down_deg),
        Some(3.0),
        "the snapshot's shot"
    );
    let apply = |model: &mut Model, name: &str, data: serde_json::Value| {
        model.apply_event(&event(name, data)).expect(name);
    };
    apply(
        &mut model,
        "shot.initialized",
        json!({ "shotId": "s-2", "playerId": 1, "holeNumber": 7, "strokeNumber": 2 }),
    );
    assert_eq!(model.lie(), None, "no lie of the shot up yet");
    apply(
        &mut model,
        "lie.changed",
        json!({ "shotId": "s-9", "playerId": 1, "surface": "Rough" }),
    );
    assert_eq!(model.lie(), None, "another shot's lie");
    apply(
        &mut model,
        "lie.changed",
        json!({ "shotId": "s-2", "playerId": 1, "surface": "Fairway", "slopeLeftRightDeg": -0.4 }),
    );
    let lie = model.lie().expect("s-2's lie");
    assert_eq!(lie.surface.as_deref(), Some("Fairway"));
    assert_eq!(lie.slope_left_right_deg, Some(-0.4));
}

/// A mulligan: `shot.replayed` and `shot.initialized` name the shot
/// replaced; the map is bounded.
#[test]
fn a_mulligan_names_the_shot_it_replaces() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    model
        .apply_event(&event(
            "shot.replayed",
            json!({ "shotId": "shot-51", "replacesShotId": "shot-42", "playerId": 1 }),
        ))
        .expect("replayed");
    assert_eq!(model.replaces("shot-51"), Some("shot-42"));
    model
        .apply_event(&event(
            "shot.initialized",
            json!({ "shotId": "shot-61", "playerId": 1, "replacesShotId": "shot-51" }),
        ))
        .expect("initialized");
    assert_eq!(model.replaces("shot-61"), Some("shot-51"));
    assert_eq!(model.replaces("shot-1"), None);
    for n in 0..=MAP_CAPACITY {
        model
            .apply_event(&event(
                "shot.replayed",
                json!({ "shotId": format!("n-{n}"), "replacesShotId": format!("o-{n}") }),
            ))
            .expect("replayed");
    }
    assert_eq!(model.replaces("shot-51"), None, "the oldest is gone");
    assert_eq!(model.replaces("n-0"), None);
    let newest = format!("n-{MAP_CAPACITY}");
    assert_eq!(
        model.replaces(&newest),
        Some(format!("o-{MAP_CAPACITY}").as_str())
    );
}

/// A total to par is only what Pureplay gave with the total; a snapshot of
/// another match starts the scores again.
#[test]
fn scores_keep_to_par_with_their_total_and_belong_to_their_match() {
    let mut model = fresh();
    let mut snap = snapshot(1);
    snap.players[0].total_strokes = Some(30);
    snap.players[0].to_par = Some(2);
    model.apply_snapshot(&snap);
    assert_eq!(model.scores()[&1].to_par, Some(2));
    model
        .apply_event(&event(
            "score.updated",
            json!({ "playerId": 1, "holeNumber": 7, "holeStrokes": 4, "totalStrokes": 34,
                    "grossToPar": 0, "final": true }),
        ))
        .expect("score");
    let score = &model.scores()[&1];
    assert_eq!((score.total, score.to_par), (Some(34), None));
    assert_eq!(score.holes[&7].strokes, Some(4));
    // The same match again keeps the holes; another match drops them.
    model.apply_snapshot(&snap);
    assert!(model.scores()[&1].holes.contains_key(&7));
    let mut next = snapshot(1);
    next.session.as_mut().expect("session").match_id = Some("m-2".into());
    next.players[0].total_strokes = Some(3);
    model.apply_snapshot(&next);
    assert!(model.scores()[&1].holes.is_empty(), "m-2 starts clean");
}

/// `player.changed` changes what it carries and leaves the rest.
#[test]
fn a_partial_player_changed_leaves_the_other_fields() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    model
        .apply_event(&event(
            "player.changed",
            json!({ "playerId": 1, "current": {}, "reason": "x" }),
        ))
        .expect("changed");
    let joe = model.player(1).expect("joe");
    assert!(joe.local);
    assert_eq!(joe.name.as_deref(), Some("Joe"));
    assert_eq!(joe.handedness.as_deref(), Some("RH"));
    assert_eq!(joe.club.as_deref(), Some("Iron7"));
    assert!(joe.bag.is_some());
    assert_eq!(model.golfer(1), Some(id("pureplay.Joe")));
    model
        .apply_event(&event(
            "player.changed",
            json!({ "playerId": 1, "current": { "name": "Joey", "handedness": "LH" } }),
        ))
        .expect("changed");
    let joe = model.player(1).expect("joe");
    assert_eq!(joe.name.as_deref(), Some("Joey"));
    assert_eq!(joe.handedness.as_deref(), Some("LH"));
    assert!(joe.local, "isLocal not given: unchanged");
}

/// An event for a player the model does not know creates nobody.
#[test]
fn events_for_unknown_players_are_ignored() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    for (name, data) in [
        (
            "player.club_changed",
            json!({ "playerId": 9, "current": "Putter" }),
        ),
        ("player.bag_changed", json!({ "playerId": 9, "bag": [] })),
        (
            "player.changed",
            json!({ "playerId": 9, "current": { "name": "Z" } }),
        ),
        (
            "shot.initialized",
            json!({ "shotId": "s", "playerId": 9, "distanceToHoleM": 3.0 }),
        ),
        (
            "shot.completed",
            json!({ "shotId": "s", "playerId": 9, "end": [1.0, 2.0, 3.0] }),
        ),
        (
            "score.updated",
            json!({ "playerId": 9, "holeNumber": 1, "totalStrokes": 4 }),
        ),
    ] {
        model.apply_event(&event(name, data)).expect(name);
    }
    assert_eq!(model.players().len(), 3);
    assert!(!model.scores().contains_key(&9));
}

/// Players are listed in the snapshot's order; a joiner comes last, a
/// leaver goes.
#[test]
fn the_round_lists_players_in_playing_order() {
    let mut model = fresh();
    let mut snap = snapshot(1);
    snap.players.reverse();
    model.apply_snapshot(&snap);
    assert_eq!(model.order(), [3, 2, 1]);
    model
        .apply_event(&event(
            "player.joined",
            player(4, "New", true, "RH", "Iron7"),
        ))
        .expect("joined");
    model
        .apply_event(&event("player.left", json!({ "playerId": 2 })))
        .expect("left");
    assert_eq!(model.order(), [3, 1, 4]);
}

/// A command's reply or a late event for a player the model does not hold
/// makes nobody, and no golfer id is made up for them.
#[test]
fn a_player_the_model_does_not_hold_is_made_up_nowhere() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    model.set_club(9, Some("Putter".into()), Some("c".into()));
    assert_eq!(model.players().len(), 3);
    assert!(model.player(9).is_none());
    assert_eq!(model.golfer(9), None);
    model.end_round();
    assert_eq!(model.golfer(1), None, "a player of an ended round");
    model.set_club(1, Some("Putter".into()), None);
    assert!(model.players().is_empty());
}

/// `player.changed` with an explicit `null` clears that field; a field it
/// leaves out stays. A player without a name has the `player-<id>` golfer.
#[test]
fn a_player_changed_null_clears_the_field() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    assert_eq!(model.golfer(1), Some(id("pureplay.Joe")));
    model
        .apply_event(&event(
            "player.changed",
            json!({ "playerId": 1, "current": { "name": null, "handedness": null } }),
        ))
        .expect("changed");
    let joe = model.player(1).expect("joe");
    assert_eq!(joe.name, None);
    assert_eq!(joe.handedness, None);
    assert_eq!(model.golfer(1), Some(id("pureplay.player-1")));
}

/// The golfer is keyed by the player's name, whatever the profile slot; a
/// player with no name (or a blank one) is keyed by the player id.
#[test]
fn a_golfer_is_keyed_by_the_players_name() {
    let mut model = fresh();
    let mut snap = snapshot(1);
    snap.players = serde_json::from_value(json!([
        { "playerId": 1, "profileId": "local-0", "name": "thill", "isLocal": true },
        { "playerId": 2, "profileId": "local-1", "isLocal": true },
        { "playerId": 3, "profileId": null, "name": "  ", "isLocal": false },
        { "playerId": 4, "profileId": null, "name": "abc123", "isLocal": false }
    ]))
    .expect("players");
    model.apply_snapshot(&snap);
    assert_eq!(model.golfer(1), Some(id("pureplay.thill")));
    assert_eq!(model.golfer(2), Some(id("pureplay.player-2")));
    assert_eq!(model.golfer(3), Some(id("pureplay.player-3")));
    assert_eq!(model.golfer(4), Some(id("pureplay.abc123")));
}

/// `player.changed` clears only the identity fields it reports: a `null`
/// club, club id, bag, ball, aim, distance, strokes or to-par in `current`
/// leaves them as they are.
#[test]
fn a_player_changed_null_keeps_the_club_bag_and_mode() {
    let mut model = fresh();
    model.apply_snapshot(&snapshot(1));
    model
        .apply_event(&event(
            "shot.initialized",
            json!({ "shotId": "s-2", "matchId": "m-1", "playerId": 1, "holeNumber": 7,
                    "strokeNumber": 2, "ballLocation": [1.0, 2.0, 3.0], "club": "SandWedge",
                    "clubId": "bag-1-SandWedge", "distanceToHoleM": 15.0, "replacesShotId": null }),
        ))
        .expect("initialized");
    let want = model.sync(own, no_cards).expect("a select");
    assert_eq!(
        (want.club, want.mode),
        (Some(Club::SandWedge), Mode::Chipping)
    );
    answered(&mut model, &want);
    let bag = model.player(1).expect("joe").bag.clone();
    model
        .apply_event(&event(
            "player.changed",
            json!({ "playerId": 1, "reason": "handedness_changed",
                    "current": { "playerId": 1, "profileId": "local-1", "name": "Joe",
                                 "isLocal": true, "handedness": "LH", "teamIndex": null,
                                 "handicap": null, "club": null, "clubId": null, "bag": null,
                                 "ball": null, "aim": null, "distanceToHoleM": null,
                                 "totalStrokes": null, "toPar": null } }),
        ))
        .expect("changed");
    let joe = model.player(1).expect("joe");
    assert_eq!(joe.club.as_deref(), Some("SandWedge"));
    assert_eq!(joe.club_id.as_deref(), Some("bag-1-SandWedge"));
    assert_eq!(joe.bag, bag);
    assert!(joe.distance.is_some());
    assert_eq!(joe.hand(), Some(Handedness::Left));
    let want = model.sync(own, no_cards).expect("the new hand");
    assert_eq!(
        (want.club, want.hand, want.mode),
        (
            Some(Club::SandWedge),
            Some(Handedness::Left),
            Mode::Chipping
        )
    );
}

/// A snapshot with `"session": null`, or with no `session`, is no match:
/// no players and no shooter, as with `matchActive` false.
#[test]
fn a_snapshot_without_a_session_brings_no_players() {
    let full = snapshot_value(1);
    for session in [Some(serde_json::Value::Null), None] {
        let mut value = full.clone();
        match &session {
            Some(v) => value["session"] = v.clone(),
            None => {
                value.as_object_mut().expect("object").remove("session");
            }
        }
        let snap: protocol::Snapshot = serde_json::from_value(value).expect("snapshot");
        let mut model = fresh();
        model.apply_snapshot(&snap);
        assert!(model.players().is_empty(), "{session:?}: no players");
        assert_eq!(model.active(), None, "{session:?}: no shooter");
    }
}
