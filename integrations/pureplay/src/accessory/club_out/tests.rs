use super::*;
use serde_json::json;

fn model_with(bag: serde_json::Value, held: &str, readiness: serde_json::Value) -> Model {
    let mut model = Model::default();
    let snap: protocol::Snapshot = serde_json::from_value(json!({
        "session": { "matchId": "m-1", "matchActive": true },
        "turn": { "activeShooterId": 1 },
        "players": [ { "playerId": 1, "name": "Joe", "isLocal": true, "handedness": "RH",
                       "club": held, "clubId": null, "bag": bag } ],
        "readiness": readiness,
    }))
    .expect("snapshot");
    model.apply_snapshot(&snap);
    model
}

fn ready() -> serde_json::Value {
    json!({ "shotId": "shot-1", "playerId": 1, "ready": true, "controllable": true,
            "allowedCommands": ["club.set", "club.next", "club.prev"], "blockers": [] })
}

fn bag() -> serde_json::Value {
    json!([ { "clubId": "dr", "type": "Driver", "name": "Driver" },
            { "clubId": "7i", "type": "Iron7", "name": "7 Iron" },
            { "clubId": "sw-1", "type": "SandWedge", "name": "56" },
            { "clubId": "sw-2", "type": "SandWedge", "name": "60" } ])
}

fn linked(commands: &[&str]) -> ClubOut {
    let mut out = ClubOut::new("pureplay_accessory.0");
    let commands: Vec<String> = commands.iter().map(|c| (*c).to_owned()).collect();
    out.link(&["read".into(), "control".into()], &commands);
    out
}

fn sent(actions: &[Action]) -> Vec<serde_json::Value> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Send(t) => Some(serde_json::from_str(t).expect("json")),
            Action::SnapBack => None,
        })
        .collect()
}

#[test]
fn the_state_follows_scope_capabilities_the_switch_and_the_bag() {
    let none = Model::default();
    let mut out = ClubOut::new("a");
    assert_eq!(out.state(&none), State::NotSupported);
    out.link(&["read".into()], &["club.set".into()]);
    assert_eq!(out.state(&none), State::ReadOnly);
    out.link(&["read".into(), "control".into()], &["club.next".into()]);
    assert_eq!(out.state(&none), State::On, "nobody here is up");
    assert_eq!(out.state(&model_with(bag(), "Iron7", ready())), State::On);
    let off = model_with(bag(), "Iron7", json!({ "blockers": ["controls_disabled"] }));
    assert_eq!(out.state(&off), State::Off);
    for empty in [json!(null), json!([])] {
        let model = model_with(empty.clone(), "Iron7", ready());
        assert_eq!(out.state(&model), State::NotSupported, "bag {empty}");
    }
    out.link(&["read".into(), "control".into()], &["aim.set".into()]);
    assert_eq!(out.state(&none), State::NotSupported);
}

#[test]
fn a_pick_resolves_to_the_first_bag_entry_of_its_type() {
    let model = model_with(bag(), "Iron7", ready());
    let mut out = linked(&["club.set"]);
    let cmds = sent(&out.pick(Club::SandWedge, &model, Instant::now()));
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0]["command"], "club.set");
    assert_eq!(cmds[0]["data"]["clubId"], "sw-1");
    assert_eq!(cmds[0]["data"]["playerId"], 1);
}

fn step_reply(n: usize, club: &str, club_id: &str) -> Response {
    Response {
        id: Some(format!("c-{n}")),
        ok: true,
        data: json!({ "playerId": 1, "club": club, "clubId": club_id }),
        error: None,
    }
}

/// Steps that move but never reach the type: given up after as many
/// steps as the bag holds.
#[test]
fn stepping_goes_toward_the_type_and_gives_up_after_the_bag() {
    let mut model = model_with(bag(), "SandWedge", ready());
    let mut out = linked(&["club.next", "club.prev"]);
    let now = Instant::now();
    let cmds = sent(&out.pick(Club::Driver, &model, now));
    assert_eq!(
        cmds[0]["command"], "club.prev",
        "the driver is before the wedge"
    );
    // Pureplay moves each time, never to the driver.
    let replies = [
        ("Iron7", "7i"),
        ("SandWedge", "sw-1"),
        ("SandWedge", "sw-2"),
        ("Iron7", "7i"),
    ];
    for (n, (club, id)) in replies.into_iter().enumerate() {
        let acts = out
            .reply(&step_reply(n + 1, club, id), &mut model, now)
            .expect("ours");
        if n < 3 {
            assert_eq!(sent(&acts)[0]["command"], "club.prev", "step {n}");
        } else {
            assert_eq!(acts, [Action::SnapBack], "given up after the bag's four");
        }
    }
    assert!(!out.holds_club());
}

/// A step that does not move (the reply names the club held before it)
/// turns the other way once; a second gives up at once.
#[test]
fn a_step_that_does_not_move_turns_once_then_gives_up() {
    let mut model = model_with(bag(), "SandWedge", ready());
    let mut out = linked(&["club.next", "club.prev"]);
    let now = Instant::now();
    assert_eq!(
        sent(&out.pick(Club::Driver, &model, now))[0]["command"],
        "club.prev"
    );
    let acts = out
        .reply(&step_reply(1, "Iron7", "7i"), &mut model, now)
        .expect("ours");
    assert_eq!(sent(&acts)[0]["command"], "club.prev", "it moved");
    let acts = out
        .reply(&step_reply(2, "Iron7", "7i"), &mut model, now)
        .expect("ours");
    assert_eq!(
        sent(&acts)[0]["command"],
        "club.next",
        "stuck: the other way"
    );
    let acts = out
        .reply(&step_reply(3, "Iron7", "7i"), &mut model, now)
        .expect("ours");
    assert_eq!(acts, [Action::SnapBack], "stuck again: given up");
    assert!(!out.holds_club());
}

/// With only one step command, a step that does not move gives up at
/// once: there is no other way to turn. Without ids the clubs are told
/// apart by type, without regard to case.
#[test]
fn a_step_that_does_not_move_with_one_command_gives_up() {
    let mut model = model_with(bag(), "SandWedge", ready());
    let mut out = linked(&["club.next"]);
    let now = Instant::now();
    assert_eq!(
        sent(&out.pick(Club::Driver, &model, now))[0]["command"],
        "club.next",
        "only next is offered"
    );
    let acts = out
        .reply(&step_reply(1, "sandwedge", "sw-1"), &mut model, now)
        .expect("ours");
    assert_eq!(acts, [Action::SnapBack], "still the wedge: given up");
    assert!(!out.holds_club());
}

/// The held club's place in the bag is found by type without regard to
/// case, as every other lookup.
#[test]
fn stepping_finds_the_held_club_by_type_in_any_case() {
    let model = model_with(bag(), "sandwedge", ready());
    let mut out = linked(&["club.next", "club.prev"]);
    let cmds = sent(&out.pick(Club::Iron7, &model, Instant::now()));
    assert_eq!(cmds[0]["command"], "club.prev", "the 7 iron is before");
}

/// A pick made while stepping takes over at the next reply, stepping
/// from where the last one got to.
#[test]
fn a_newer_pick_takes_over_from_stepping() {
    let mut model = model_with(bag(), "SandWedge", ready());
    let mut out = linked(&["club.next", "club.prev"]);
    let now = Instant::now();
    let first = sent(&out.pick(Club::Driver, &model, now));
    assert_eq!(first[0]["command"], "club.prev");
    assert!(
        sent(&out.pick(Club::SandWedge, &model, now)).is_empty(),
        "one in flight"
    );
    let r = Response {
        id: Some("c-1".into()),
        ok: true,
        data: json!({ "playerId": 1, "club": "Iron7", "clubId": "7i" }),
        error: None,
    };
    let next = sent(&out.reply(&r, &mut model, now).expect("ours"));
    assert_eq!(next.len(), 1);
    assert_eq!(
        next[0]["command"], "club.next",
        "from the 7 iron toward the wedge"
    );
}

/// A refusal of command `n` with `error`.
fn refused(n: usize, error: serde_json::Value) -> Response {
    Response {
        id: Some(format!("c-{n}")),
        ok: false,
        data: json!(null),
        error: Some(serde_json::from_value(error).expect("error")),
    }
}

/// Pureplay's readiness changes to `readiness`: the model takes the
/// event after club out has seen it, as the card does.
fn ready_changed(out: &mut ClubOut, model: &mut Model, readiness: serde_json::Value) {
    let event = protocol::Event {
        name: "shot.ready_changed".into(),
        data: readiness,
        ..protocol::Event::default()
    };
    assert!(out.before_event(&event).is_empty());
    model.apply_event(&event).expect("event");
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// With no new readiness, a pick answered `not_ready` goes again
/// [`NOT_READY_RETRY`] later, [`NOT_READY_RETRIES`] times; the next
/// `not_ready` drops it.
#[test]
fn not_ready_is_retried_on_the_timer_then_dropped() {
    let mut model = model_with(bag(), "Iron7", ready());
    let mut out = linked(&["club.set"]);
    let mut now = Instant::now();
    assert_eq!(sent(&out.pick(Club::SandWedge, &model, now)).len(), 1);
    for n in 1..=NOT_READY_RETRIES as usize {
        let acts = out
            .reply(&refused(n, json!({ "code": "not_ready" })), &mut model, now)
            .expect("ours");
        assert!(acts.is_empty(), "retry {n} waits");
        assert!(out.holds_club(), "retry {n} pending");
        assert!(out.drive(&model, now + NOT_READY_RETRY - ms(1)).is_empty());
        now += NOT_READY_RETRY;
        let again = sent(&out.drive(&model, now));
        assert_eq!(again.len(), 1, "retry {n}");
        assert_eq!(again[0]["data"]["clubId"], "sw-1");
    }
    let n = NOT_READY_RETRIES as usize + 1;
    let acts = out
        .reply(&refused(n, json!({ "code": "not_ready" })), &mut model, now)
        .expect("ours");
    assert_eq!(acts, [Action::SnapBack]);
    assert!(!out.holds_club());
    assert!(out.drive(&model, now + NOT_READY_RETRY * 2).is_empty());
}

/// After `not_ready`, a new readiness sends the pick at once: the
/// readiness, not the timer.
#[test]
fn not_ready_goes_again_at_the_next_readiness_ahead_of_the_timer() {
    let mut model = model_with(bag(), "Iron7", ready());
    let mut out = linked(&["club.set"]);
    let now = Instant::now();
    assert_eq!(sent(&out.pick(Club::SandWedge, &model, now)).len(), 1);
    let not_ready = json!({ "code": "not_ready", "blockers": ["shot_in_progress"] });
    assert!(
        out.reply(&refused(1, not_ready), &mut model, now)
            .expect("ours")
            .is_empty()
    );
    ready_changed(&mut out, &mut model, ready());
    let again = sent(&out.drive(&model, now));
    assert_eq!(again.len(), 1);
    assert_eq!(again[0]["id"], "c-2");
    assert_eq!(again[0]["data"]["clubId"], "sw-1");
}

/// `rate_limited` is retried once, after `retryAfterMs` or a short wait
/// when it names none; a second one drops the pick.
#[test]
fn rate_limited_is_retried_once_after_its_wait() {
    for (error, wait) in [
        (
            json!({ "code": "rate_limited", "retryAfterMs": 300 }),
            ms(300),
        ),
        (json!({ "code": "rate_limited" }), RATE_LIMIT_WAIT),
    ] {
        let mut model = model_with(bag(), "Iron7", ready());
        let mut out = linked(&["club.set"]);
        let now = Instant::now();
        assert_eq!(sent(&out.pick(Club::SandWedge, &model, now)).len(), 1);
        let acts = out
            .reply(&refused(1, error.clone()), &mut model, now)
            .expect("ours");
        assert!(acts.is_empty(), "{error}");
        assert!(out.drive(&model, now + wait - ms(1)).is_empty(), "{error}");
        let again = sent(&out.drive(&model, now + wait));
        assert_eq!(again.len(), 1, "{error}");
        let acts = out
            .reply(&refused(2, error.clone()), &mut model, now + wait)
            .expect("ours");
        assert_eq!(acts, [Action::SnapBack], "{error}: one retry");
        assert!(out.drive(&model, now + wait * 3).is_empty(), "{error}");
    }
}

/// A command unanswered for [`COMMAND_TIMEOUT`] drops the pick; a reply
/// after that answers nothing of the card's.
#[test]
fn an_unanswered_command_drops_the_pick_and_a_late_reply_is_ignored() {
    let mut model = model_with(bag(), "Iron7", ready());
    let mut out = linked(&["club.set"]);
    let now = Instant::now();
    assert_eq!(sent(&out.pick(Club::SandWedge, &model, now)).len(), 1);
    assert!(out.drive(&model, now + COMMAND_TIMEOUT - ms(1)).is_empty());
    assert!(out.holds_club());
    let later = now + COMMAND_TIMEOUT;
    assert_eq!(out.drive(&model, later), [Action::SnapBack]);
    assert!(!out.holds_club());
    assert_eq!(
        out.reply(&step_reply(1, "SandWedge", "sw-1"), &mut model, later),
        None
    );
    let held = model.player(1).and_then(|p| p.club.as_deref());
    assert_eq!(held, Some("Iron7"), "the late reply changes nothing");
}

/// Pureplay sends event `name` with `data`: club out sees it first,
/// then the model, as the card does. Returns club out's actions.
fn event(out: &mut ClubOut, model: &mut Model, name: &str, data: serde_json::Value) -> Vec<Action> {
    let event = protocol::Event {
        name: name.into(),
        data,
        ..protocol::Event::default()
    };
    let actions = out.before_event(&event);
    model.apply_event(&event).expect("event");
    actions
}

/// A pending pick is dropped by a turn passing to another player, a
/// context reset, and the player's own choice; once Pureplay is ready,
/// nothing is sent.
#[test]
fn a_pending_pick_is_dropped_by_a_turn_a_reset_and_the_players_choice() {
    let waiting = json!({ "shotId": "shot-1", "playerId": 1, "ready": false,
                          "controllable": false, "allowedCommands": [],
                          "blockers": ["shot_in_progress"] });
    let drops = [
        (
            "turn.changed",
            json!({ "previousPlayerId": 1, "currentPlayerId": 2, "reason": "next_player" }),
        ),
        (
            "context.invalidated",
            json!({ "reason": "reset", "matchId": null }),
        ),
        (
            "player.club_changed",
            json!({ "playerId": 1, "current": "Iron9", "reason": "user_selected" }),
        ),
    ];
    for (name, data) in drops {
        let mut model = model_with(bag(), "Iron7", waiting.clone());
        let mut out = linked(&["club.set"]);
        let now = Instant::now();
        assert!(
            out.pick(Club::SandWedge, &model, now).is_empty(),
            "{name}: waits"
        );
        assert!(out.holds_club(), "{name}: pending");
        assert_eq!(
            event(&mut out, &mut model, name, data),
            [Action::SnapBack],
            "{name}"
        );
        assert!(!out.holds_club(), "{name}");
        ready_changed(&mut out, &mut model, ready());
        assert!(
            out.drive(&model, now).is_empty(),
            "{name}: nothing once ready"
        );
    }
}

/// A command in flight when the turn passes to another player: a late
/// success, a late `not_ready` or no reply at all sends nothing more.
#[test]
fn a_command_in_flight_across_a_turn_sends_nothing_more() {
    let turn = json!({ "previousPlayerId": 1, "currentPlayerId": 2, "reason": "next_player" });
    for case in ["success", "not_ready", "no reply"] {
        let mut model = model_with(bag(), "Iron7", ready());
        let mut out = linked(&["club.set"]);
        let now = Instant::now();
        assert_eq!(sent(&out.pick(Club::SandWedge, &model, now)).len(), 1);
        let acts = event(&mut out, &mut model, "turn.changed", turn.clone());
        assert_eq!(acts, [Action::SnapBack], "{case}");
        assert!(!out.holds_club(), "{case}");
        let later = now + COMMAND_TIMEOUT;
        let acts = match case {
            "success" => out
                .reply(&step_reply(1, "SandWedge", "sw-1"), &mut model, now)
                .expect("ours"),
            "not_ready" => out
                .reply(&refused(1, json!({ "code": "not_ready" })), &mut model, now)
                .expect("ours"),
            _ => out.drive(&model, later),
        };
        assert!(sent(&acts).is_empty(), "{case}: {acts:?}");
        assert!(
            out.drive(&model, later + NOT_READY_RETRY).is_empty(),
            "{case}"
        );
        assert!(!out.holds_club(), "{case}");
    }
}

/// Every refusal but `not_ready` and `rate_limited` drops the pick, and
/// so does a type the bag does not hold. `forbidden_scope` also turns
/// club out off, until a readiness lets apps steer the shot again.
#[test]
fn every_failure_drops_the_pick_and_forbidden_scope_turns_club_out_off() {
    for code in [
        "forbidden_scope",
        "not_allowed",
        "bad_request",
        "unknown_command",
    ] {
        let mut model = model_with(bag(), "Iron7", ready());
        let mut out = linked(&["club.set"]);
        let now = Instant::now();
        assert_eq!(sent(&out.pick(Club::SandWedge, &model, now)).len(), 1);
        let acts = out
            .reply(&refused(1, json!({ "code": code })), &mut model, now)
            .expect("ours");
        assert_eq!(acts, [Action::SnapBack], "{code}");
        assert!(!out.holds_club(), "{code}");
        if code == "forbidden_scope" {
            assert_eq!(out.state(&model), State::Off);
            assert_eq!(out.pick(Club::Driver, &model, now), [Action::SnapBack]);
            ready_changed(&mut out, &mut model, ready());
            assert_eq!(out.state(&model), State::On);
            assert_eq!(sent(&out.pick(Club::Driver, &model, now)).len(), 1);
        } else {
            assert_eq!(out.state(&model), State::On, "{code}");
        }
    }
    let model = model_with(bag(), "Iron7", ready());
    let mut out = linked(&["club.set"]);
    let acts = out.pick(Club::PitchingWedge, &model, Instant::now());
    assert_eq!(acts, [Action::SnapBack], "not in the bag");
}

/// While club out is not usable a pick sends nothing and puts
/// Pureplay's club back.
#[test]
fn a_pick_while_club_out_is_not_on_only_snaps_back() {
    let model = model_with(bag(), "Iron7", ready());
    let off = model_with(bag(), "Iron7", json!({ "blockers": ["controls_disabled"] }));
    let read_only = {
        let mut out = ClubOut::new("a");
        out.link(&["read".into()], &["club.set".into()]);
        out
    };
    for (mut out, model, state) in [
        (read_only, &model, State::ReadOnly),
        (linked(&["aim.set"]), &model, State::NotSupported),
        (linked(&["club.set"]), &off, State::Off),
    ] {
        assert_eq!(out.state(model), state);
        assert_eq!(
            out.pick(Club::SandWedge, model, Instant::now()),
            [Action::SnapBack]
        );
        assert!(!out.holds_club(), "{state:?}");
    }
}

/// A pending pick goes with the connection: after the next connection
/// nothing is sent for it.
#[test]
fn a_pending_pick_is_dropped_with_the_connection() {
    let waiting = json!({ "shotId": "shot-1", "playerId": 1, "ready": false,
                          "controllable": false, "allowedCommands": [],
                          "blockers": ["shot_in_progress"] });
    let mut model = model_with(bag(), "Iron7", waiting);
    let mut out = linked(&["club.set"]);
    let now = Instant::now();
    assert!(out.pick(Club::SandWedge, &model, now).is_empty());
    assert!(out.holds_club());
    out.clear();
    assert!(!out.holds_club());
    out.link(&["read".into(), "control".into()], &["club.set".into()]);
    ready_changed(&mut out, &mut model, ready());
    assert!(out.drive(&model, now).is_empty());
}
