//! Every JSON example of Pureplay's Accessory API document (version 1,
//! revised 2026-11-10) read by the card's parser, plus the `null` forms of
//! that revision.

use super::*;

/// The server's `hello`.
const HELLO: &str = r#"{ "type": "hello", "data": {
    "protocol": "ppaccessory",
    "version": 1,
    "game": "Pureplay",
    "gameVersion": "1.4.2",
    "paired": false,
    "connectionId": "c-8f21a3c0d4e5",
    "serverInstanceId": "s-3b90f1e2a7b8",
    "capabilities": {
      "events": ["state.snapshot", "context.invalidated",
                 "match.started", "match.ended", "match.paused", "match.resumed",
                 "hole.started", "hole.completed", "turn.changed", "phase.changed",
                 "player.joined", "player.left", "player.changed",
                 "player.club_changed", "player.aim_changed",
                 "player.bag_changed",
                 "shot.initialized", "shot.launched", "shot.completed",
                 "shot.cancelled", "shot.replayed", "shot.ready_changed",
                 "lie.changed", "ruling.required", "ruling.resolved", "score.updated"],
      "commands": ["aim.nudge", "aim.set", "aim.reset",
                   "club.set", "club.next", "club.prev", "club.reset"],
      "snapshotSections": ["session", "course", "hole", "turn",
                           "players", "shot", "readiness", "lie", "ruling"]
    }
} }"#;

/// The client's `pair`, as the document shows it.
const PAIR: &str = r#"{ "type": "pair", "data": {
    "client": "My First Overlay",
    "version": "1.0",
    "token": null
} }"#;

const PAIRING_PENDING: &str = r#"{ "type": "pairing.pending", "data": { "expiresInSec": 60 } }"#;

const PAIRED: &str = r#"{ "type": "paired", "data": {
    "token": "PPA-6F1C2A9B3D4E4F50A1B2C3D4E5F60718",
    "clientId": "B1E7B0C2-9A3F-4C1D-8E2B-5F6A7B8C9D0E",
    "scopes": ["read", "control"]
} }"#;

const SUBSCRIBE: &str = r#"{ "type": "subscribe", "id": "sub-1", "data": {
    "events": ["shot.*", "turn.changed", "hole.*"],
    "snapshot": true
} }"#;

const SUBSCRIBE_ACK: &str = r#"{ "type": "response", "id": "sub-1", "ok": true, "data": {} }"#;

const ENVELOPE: &str = r#"{ "type": "event",
  "event": "shot.completed",
  "seq": 128,
  "time": "2026-07-27T14:03:11.123Z",
  "data": { } }"#;

const STATE_GET: &str = r#"{ "type": "state.get", "id": "s-17" }"#;

const STATE_GET_SNAPSHOT: &str = r#"{ "type": "event", "event": "state.snapshot", "requestId": "s-17",
  "seq": 129, "time": "...", "data": { } }"#;

const READY_CHANGED: &str = r#"{ "type": "event", "event": "shot.ready_changed", "seq": 130, "time": "...", "data": {
    "shotId": "shot-42",
    "playerId": 1,
    "ready": true,
    "controllable": true,
    "allowedCommands": ["aim.nudge", "aim.set", "aim.reset",
                        "club.set", "club.next", "club.prev", "club.reset"],
    "blockers": []
} }"#;

const READINESS_BLOCKED: &str = r#"{ "shotId": "shot-42", "playerId": 1,
  "ready": false, "controllable": false,
  "allowedCommands": [],
  "blockers": ["awaiting_decision"] }"#;

/// Every event payload of the event reference: (event, payload).
const EVENTS: &[(&str, &str)] = &[
    (
        "match.started",
        r#"{ "matchId": "match-123", "courseId": "StAndrews", "courseName": "St Andrews",
  "holeCount": 18, "totalPar": 72, "scoringModel": "Stroke",
  "isTeamGame": false, "playerIds": [1, 2] }"#,
    ),
    (
        "match.ended",
        r#"{ "matchId": "match-123", "reason": "completed",
  "standings": [ { "playerId": 1, "rank": 1, "totalStrokes": 71, "toPar": -1 },
                 { "playerId": 2, "rank": 2, "totalStrokes": 76, "toPar": 4 } ] }"#,
    ),
    ("match.paused", r#"{ "matchId": "match-123" }"#),
    ("match.resumed", r#"{ "matchId": "match-123" }"#),
    (
        "context.invalidated",
        r#"{ "reason": "loading", "matchId": null }"#,
    ),
    (
        "hole.started",
        r#"{ "matchId": "match-123", "holeNumber": 7, "par": 4, "strokeIndex": 3,
  "yards": 410, "pin": [420.0, 91.0, 4.1], "playerIds": [1, 2] }"#,
    ),
    (
        "hole.completed",
        r#"{ "matchId": "match-123", "holeNumber": 7,
  "results": [ { "playerId": 1, "holeStrokes": 3, "grossToPar": -1,
                 "netToPar": -2, "holeResult": "Birdie", "holedOut": true },
               { "playerId": 2, "holeStrokes": 5, "grossToPar": 1,
                 "netToPar": 1, "holeResult": "Bogey", "holedOut": true } ] }"#,
    ),
    (
        "turn.changed",
        r#"{ "matchId": "match-123", "previousPlayerId": 2, "currentPlayerId": 1,
  "holeNumber": 7, "reason": "next_player" }"#,
    ),
    (
        "phase.changed",
        r#"{ "matchId": "match-123", "previous": "ResolveShot", "current": "AwaitShot" }"#,
    ),
    ("player.left", r#"{ "playerId": 3 }"#),
    (
        "player.changed",
        r#"{ "playerId": 1, "current": { }, "reason": "handedness_changed" }"#,
    ),
    (
        "player.club_changed",
        r#"{ "playerId": 1, "shotId": "shot-42",
  "previous": "Iron7", "current": "Iron8",
  "previousClubId": "9b1c0000-0000-0000-0000-000000000001",
  "currentClubId": "9b1c0000-0000-0000-0000-000000000002",
  "reason": "user_selected" }"#,
    ),
    (
        "player.aim_changed",
        r#"{ "playerId": 1, "shotId": "shot-42",
  "previous": [418.0, 90.0, 4.1], "current": [420.0, 91.0, 4.1],
  "reason": "api_command" }"#,
    ),
    (
        "player.bag_changed",
        r#"{ "playerId": 1, "bag": [ { "clubId": "3f2a0000-0000-0000-0000-000000000002", "type": "Driver", "name": "Driver" } ] }"#,
    ),
    (
        "shot.initialized",
        r#"{ "shotId": "shot-42", "matchId": "match-123", "playerId": 1,
  "holeNumber": 7, "strokeNumber": 2,
  "ballLocation": [141.0, 92.7, 3.9], "club": "Iron7",
  "clubId": "9b1c0000-0000-0000-0000-000000000001",
  "distanceToHoleM": 152.0, "replacesShotId": null }"#,
    ),
    (
        "shot.launched",
        r#"{ "shotId": "shot-42", "playerId": 1,
  "ballSpeedMs": 61.2, "launchAngleDeg": 18.4, "directionDeg": -1.5,
  "backSpinRpm": 5200, "sideSpinRpm": -240 }"#,
    ),
    (
        "shot.completed",
        r#"{ "shotId": "shot-42", "matchId": "match-123", "playerId": 1,
  "holeNumber": 7, "strokeNumber": 2,
  "outcome": "Green", "penaltyStrokes": 0, "isPutt": false,
  "carryM": 138.4, "totalM": 145.1, "apexM": 24.6,
  "startDistToHoleM": 152.0, "endDistToHoleM": 7.4,
  "start": [12.4, 88.1, 3.2], "end": [141.0, 92.7, 3.9],
  "lie": "Green", "holeStrokes": 2 }"#,
    ),
    (
        "shot.cancelled",
        r#"{ "shotId": "shot-42", "playerId": 1, "reason": "discarded" }"#,
    ),
    (
        "shot.replayed",
        r#"{ "shotId": "shot-51", "replacesShotId": "shot-42",
  "playerId": 1, "reason": "mulligan" }"#,
    ),
    (
        "lie.changed",
        r#"{ "shotId": "shot-42", "playerId": 1,
  "surface": "Fairway",
  "normalWorld": [0.02, -0.05, 0.998],
  "slopeUpDownDeg": 1.2,
  "slopeLeftRightDeg": -0.4,
  "slopeFrame": "target_line",
  "reason": "ball_at_rest" }"#,
    ),
    (
        "ruling.required",
        r#"{ "rulingId": "ruling-9", "shotId": "shot-42", "playerId": 1,
  "outcome": "Water", "reason": "Shot finished in a hazard; relief has been taken",
  "allowedActions": ["AcceptShot", "TakeDrop"],
  "blocksPlay": false }"#,
    ),
    (
        "ruling.resolved",
        r#"{ "rulingId": "ruling-9", "shotId": "shot-42", "playerId": 1,
  "outcome": "Water", "selectedAction": "TakeDrop",
  "penaltyStrokes": 1, "dropLocation": [130.2, 90.4, 3.8], "resolved": true }"#,
    ),
    (
        "score.updated",
        r#"{ "matchId": "match-123", "shotId": "shot-42",
  "holeNumber": 7, "playerId": 1, "teamIndex": null,
  "holeStrokes": 3, "totalStrokes": 32,
  "grossToPar": -1, "netToPar": -2,
  "holeResult": "Birdie", "holedOut": true,
  "final": true, "reason": "hole_completed" }"#,
    ),
];

/// The state snapshot.
const SNAPSHOT: &str = r#"{ "session": { "apiVersion": 1, "matchId": "match-123", "phase": "AwaitShot",
               "isTeamGame": false, "scoringModel": "Stroke", "matchActive": true,
               "paused": false },
  "course":  { "courseId": "StAndrews", "displayName": "St Andrews",
               "holeCount": 18, "totalPar": 72 },
  "hole":    { "number": 7, "par": 4, "strokeIndex": 3, "yards": 410,
               "pin": [420.0, 91.0, 4.1] },
  "turn":    { "activeShooterId": 1, "remainingPlayerIds": [1, 2] },
  "players": [ { "playerId": 1,
                 "profileId": "p-77c1",
                 "name": "Joe",
                 "handedness": "RH",
                 "isLocal": true,
                 "teamIndex": 0,
                 "handicap": 8.0,
                 "totalStrokes": 31,
                 "toPar": 2,
                 "finished": false,
                 "hole": { "strokes": 2, "putts": 0, "holedOut": false,
                           "onGreen": true, "handicapStrokes": 1 },
                 "ball": [141.0, 92.7, 3.9],
                 "aim": [420.0, 91.0, 4.1],
                 "club": "Iron7",
                 "clubId": "9b1c0000-0000-0000-0000-000000000001",
                 "lie": "Green",
                 "distanceToHoleM": 7.4,
                 "bag": [
                   { "clubId": "3f2a0000-0000-0000-0000-000000000002", "type": "Driver", "name": "Driver" },
                   { "clubId": "9b1c0000-0000-0000-0000-000000000001", "type": "Iron7", "name": "7 Iron" } ] } ],
  "shot":    { "shotId": "shot-42", "playerId": 1, "holeNumber": 7,
               "strokeNumber": 2, "state": "AwaitingLaunch" },
  "readiness": { "shotId": "shot-42", "playerId": 1,
                 "ready": true, "controllable": true,
                 "allowedCommands": ["aim.nudge", "aim.set", "aim.reset",
                                     "club.set", "club.next", "club.prev", "club.reset"],
                 "blockers": [] },
  "lie":     { "surface": "Green",
               "normalWorld": [0.02, -0.05, 0.998],
               "slopeUpDownDeg": 1.2,
               "slopeLeftRightDeg": -0.4,
               "slopeFrame": "target_line" },
  "ruling":  null }"#;

const COMMAND: &str = r#"{ "type": "command", "id": "c-17", "command": "club.set",
  "data": { "playerId": 1, "clubId": "9b1c0000-0000-0000-0000-000000000001" } }"#;

const REPLY_OK: &str = r#"{ "type": "response", "id": "c-17", "ok": true,
  "data": { "playerId": 1, "club": "Iron7", "clubId": "9b1c0000-0000-0000-0000-000000000001" } }"#;

const REPLY_OK_AIM: &str = r#"{ "type": "response", "id": "c-18", "ok": true,
  "data": { "playerId": 1, "club": "Iron7", "clubId": "9b1c0000-0000-0000-0000-000000000001", "aim": [420.0, 91.0, 4.1], "deltaDeg": 5 } }"#;

const REPLY_ERROR: &str = r#"{ "type": "response", "id": "c-17", "ok": false,
  "error": { "code": "not_ready",
             "message": "Not accepting commands.",
             "blockers": ["shot_in_progress"] } }"#;

const RATE_LIMITED: &str = r#"{ "type": "response", "id": "c-18", "ok": false,
  "error": { "code": "rate_limited", "message": "Slow down.", "retryAfterMs": 50 } }"#;

fn event(text: &str) -> Event {
    match parse(text).expect("parses") {
        Incoming::Event(e) => e,
        other => panic!("not an event: {other:?}"),
    }
}

fn wrap(name: &str, data: &str) -> String {
    format!(
        r#"{{ "type": "event", "event": "{name}", "seq": 1, "time": "2026-07-27T14:03:11.123Z", "data": {data} }}"#
    )
}

#[test]
fn hello_is_read_with_its_capabilities() {
    let Incoming::Hello(h) = parse(HELLO).expect("hello") else {
        panic!("not hello");
    };
    assert_eq!(h.protocol.as_deref(), Some(PROTOCOL));
    assert_eq!(h.version, Some(VERSION));
    assert_eq!(h.game_version.as_deref(), Some("1.4.2"));
    assert_eq!(h.server_instance_id.as_deref(), Some("s-3b90f1e2a7b8"));
    let caps = h.capabilities.expect("capabilities");
    assert_eq!(caps.events.len(), 26);
    assert!(caps.commands.iter().any(|c| c == "club.set"));
    // Every event the card subscribes to is one this build lists.
    assert_eq!(events_to_subscribe(Some(&caps)), EVENTS_SUBSCRIBED);
}

/// [`super::EVENTS`], under another name in this module.
const EVENTS_SUBSCRIBED: [&str; 18] = super::EVENTS;

#[test]
fn only_listed_events_are_subscribed_and_all_when_none_are_listed() {
    let caps = Capabilities {
        events: vec![
            "TURN.CHANGED".into(),
            "player.club_changed".into(),
            "ruling.required".into(),
        ],
        commands: Vec::new(),
    };
    assert_eq!(
        events_to_subscribe(Some(&caps)),
        ["turn.changed", "player.club_changed"]
    );
    assert_eq!(events_to_subscribe(None).len(), 18);
    assert_eq!(
        events_to_subscribe(Some(&Capabilities::default())).len(),
        18
    );
    // A list without any of the card's events: none, never the empty
    // subscribe that Pureplay reads as every event.
    let caps = Capabilities {
        events: vec!["ruling.required".into(), "ruling.resolved".into()],
        commands: Vec::new(),
    };
    assert!(events_to_subscribe(Some(&caps)).is_empty());
}

/// A list or flag sent as `null` reads as empty or `false`; the rest of the
/// message is kept.
#[test]
fn null_lists_and_flags_read_as_empty() {
    let Incoming::Hello(h) = parse(
        r#"{ "type": "hello", "data": { "protocol": "ppaccessory", "version": 1,
             "serverInstanceId": "s-1", "capabilities": { "events": null, "commands": null } } }"#,
    )
    .expect("hello") else {
        panic!("not hello");
    };
    let caps = h.capabilities.expect("capabilities");
    assert!(caps.events.is_empty() && caps.commands.is_empty());
    assert_eq!(h.server_instance_id.as_deref(), Some("s-1"));
    assert_eq!(events_to_subscribe(Some(&caps)).len(), 18);

    let Incoming::Paired(p) = parse(
        r#"{ "type": "paired", "data": { "token": "PPA-X", "clientId": "c", "scopes": null } }"#,
    )
    .expect("paired") else {
        panic!("not paired");
    };
    assert_eq!(p.token.as_deref(), Some("PPA-X"));
    assert!(p.scopes.is_empty());

    let r: Readiness = serde_json::from_str(
        r#"{ "shotId": "s", "playerId": 1, "ready": null, "controllable": null,
             "allowedCommands": null, "blockers": null }"#,
    )
    .expect("readiness");
    assert!(!r.ready && !r.controllable);
    assert!(r.allowed_commands.is_empty() && r.blockers.is_empty());
    assert_eq!(r.player_id, Some(1));

    let snap: Snapshot = serde_json::from_str(
        r#"{ "hole": { "number": 3, "par": 5 }, "turn": null, "players": null, "readiness": null }"#,
    )
    .expect("snapshot");
    assert!(snap.players.is_empty());
    assert_eq!(snap.hole.and_then(|h| h.number), Some(3));

    let Incoming::Response(r) = parse(
        r#"{ "type": "response", "id": "pair-1", "ok": false,
             "error": { "code": null, "message": null, "retryAfterMs": null, "blockers": null } }"#,
    )
    .expect("response") else {
        panic!("not a response");
    };
    let e = r.error.expect("error");
    assert!(e.code.is_empty() && e.blockers.is_empty());
}

#[test]
fn the_cards_pair_and_subscribe_have_the_documents_shape() {
    let ours: serde_json::Value = serde_json::from_str(&pair(None)).expect("json");
    let theirs: serde_json::Value = serde_json::from_str(PAIR).expect("json");
    assert_eq!(ours["type"], theirs["type"]);
    for key in ["client", "version", "token"] {
        assert!(ours["data"].get(key).is_some(), "{key}");
    }
    assert_eq!(ours["data"]["client"], "flighthook");
    assert_eq!(ours["data"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(ours["data"]["token"].is_null());
    let with: serde_json::Value = serde_json::from_str(&pair(Some("PPA-1"))).expect("json");
    assert_eq!(with["data"]["token"], "PPA-1");

    let ours: serde_json::Value =
        serde_json::from_str(&subscribe(&["turn.changed"])).expect("json");
    let theirs: serde_json::Value = serde_json::from_str(SUBSCRIBE).expect("json");
    assert_eq!(ours["type"], theirs["type"]);
    assert_eq!(ours["id"], theirs["id"]);
    assert_eq!(ours["data"]["snapshot"], true);
    assert_eq!(ours["data"]["events"], serde_json::json!(["turn.changed"]));

    let ours: serde_json::Value = serde_json::from_str(&state_get("s-17")).expect("json");
    let theirs: serde_json::Value = serde_json::from_str(STATE_GET).expect("json");
    assert_eq!(ours, theirs);
    // The command example is a message the card does not send yet; it is
    // valid JSON of the same envelope.
    let command: serde_json::Value = serde_json::from_str(COMMAND).expect("json");
    assert_eq!(command["type"], "command");
}

#[test]
fn pairing_messages_are_read() {
    assert_eq!(
        parse(PAIRING_PENDING).expect("pending"),
        Incoming::PairingPending {
            expires_in_sec: Some(60)
        }
    );
    let Incoming::Paired(p) = parse(PAIRED).expect("paired") else {
        panic!("not paired");
    };
    assert_eq!(
        p.token.as_deref(),
        Some("PPA-6F1C2A9B3D4E4F50A1B2C3D4E5F60718")
    );
    assert_eq!(
        p.client_id.as_deref(),
        Some("B1E7B0C2-9A3F-4C1D-8E2B-5F6A7B8C9D0E")
    );
    assert_eq!(p.scopes, ["read", "control"]);
    assert!(!format!("{p:?}").contains("PPA-"), "Debug hides the token");
}

#[test]
fn responses_are_read_ok_and_refused() {
    let Incoming::Response(ack) = parse(SUBSCRIBE_ACK).expect("ack") else {
        panic!("not a response");
    };
    assert_eq!(ack.id.as_deref(), Some("sub-1"));
    assert!(ack.ok);
    for text in [REPLY_OK, REPLY_OK_AIM] {
        let Incoming::Response(r) = parse(text).expect("reply") else {
            panic!("not a response");
        };
        assert!(r.ok);
        assert_eq!(r.data["club"], "Iron7");
    }
    let Incoming::Response(r) = parse(REPLY_ERROR).expect("error") else {
        panic!("not a response");
    };
    assert!(!r.ok);
    let e = r.error.expect("error");
    assert_eq!(e.code, "not_ready");
    assert_eq!(e.blockers, ["shot_in_progress"]);
    let Incoming::Response(r) = parse(RATE_LIMITED).expect("rate limited") else {
        panic!("not a response");
    };
    assert_eq!(r.error.expect("error").retry_after_ms, Some(50));
}

#[test]
fn the_envelope_and_snapshots_are_read() {
    let e = event(ENVELOPE);
    assert_eq!((e.name.as_str(), e.seq), ("shot.completed", Some(128)));
    let e = event(STATE_GET_SNAPSHOT);
    assert_eq!(e.name, "state.snapshot");
    assert_eq!(e.request_id.as_deref(), Some("s-17"));
    assert_eq!(e.seq, Some(129));
    let empty: Snapshot = payload(&e).expect("empty snapshot");
    assert_eq!(empty, Snapshot::default());

    let snap: Snapshot = serde_json::from_str(SNAPSHOT).expect("snapshot");
    assert_eq!(
        snap.hole.as_ref().map(|h| (h.number, h.par, h.yards)),
        Some((Some(7), Some(4), Some(410)))
    );
    assert_eq!(snap.players[0].profile_id.as_deref(), Some("p-77c1"));
    assert_eq!(snap.players[0].distance_to_hole_m, Some(7.4));
    assert_eq!(snap.turn.and_then(|t| t.active_shooter_id), Some(1));
    let joe = &snap.players[0];
    assert_eq!(joe.name.as_deref(), Some("Joe"));
    assert_eq!(joe.is_local, Some(true));
    assert_eq!(
        joe.club.as_deref().and_then(club),
        Some(flighthook::Club::Iron7)
    );
    assert_eq!(
        joe.handedness.as_deref().and_then(hand),
        Some(flighthook::Handedness::Right)
    );
    assert_eq!(joe.bag.as_ref().map(Vec::len), Some(2));
    let r = snap.readiness.expect("readiness");
    assert!(r.ready && r.controllable);
    assert_eq!(blockers_text(&r), "");
}

#[test]
fn both_readiness_examples_are_read() {
    let e = event(READY_CHANGED);
    let r: Readiness = payload(&e).expect("ready");
    assert_eq!(r.shot_id.as_deref(), Some("shot-42"));
    assert_eq!(r.player_id, Some(1));
    assert_eq!(r.allowed_commands.len(), 7);
    let r: Readiness = serde_json::from_str(READINESS_BLOCKED).expect("blocked");
    assert!(!r.ready);
    assert_eq!(blockers_text(&r), "waiting on a ruling");
}

#[test]
fn every_event_payload_is_read() {
    let mut model = super::super::model::Model::default();
    for (name, data) in EVENTS {
        let e = event(&wrap(name, data));
        assert_eq!(e.name, *name);
        assert_eq!(e.seq, Some(1));
        model
            .apply_event(&e)
            .unwrap_or_else(|err| panic!("{name}: {err}"));
    }
    // A complete player object for `player.joined` (the snapshot's shape).
    let snap: serde_json::Value = serde_json::from_str(SNAPSHOT).expect("snapshot");
    let joined = event(&wrap("player.joined", &snap["players"][0].to_string()));
    model.apply_event(&joined).expect("player.joined");
    // The typed payloads carry the document's values.
    let get = |name: &str| {
        event(&wrap(
            name,
            EVENTS.iter().find(|(n, _)| *n == name).expect(name).1,
        ))
    };
    let t: TurnChanged = payload(&get("turn.changed")).expect("turn");
    assert_eq!(
        (t.previous_player_id, t.current_player_id),
        (Some(2), Some(1))
    );
    let c: ClubChanged = payload(&get("player.club_changed")).expect("club");
    assert_eq!(c.current.as_deref(), Some("Iron8"));
    assert_eq!(c.reason.as_deref(), Some("user_selected"));
    let h: HoleStarted = payload(&get("hole.started")).expect("hole");
    assert_eq!((h.hole_number, h.par), (Some(7), Some(4)));
    let b: BagChanged = payload(&get("player.bag_changed")).expect("bag");
    assert_eq!(b.bag.map(|b| b.len()), Some(1));
    let x: ContextInvalidated = payload(&get("context.invalidated")).expect("context");
    assert_eq!(x.reason.as_deref(), Some("loading"));
    assert_eq!(x.match_id, None);
}

/// The 2026-11-10 revision: absent values are `null`.
#[test]
fn the_null_forms_are_read() {
    let r: Readiness = serde_json::from_str(
        r#"{ "shotId": null, "playerId": null, "ready": false, "controllable": false,
             "allowedCommands": [], "blockers": ["not_in_play"] }"#,
    )
    .expect("readiness");
    assert_eq!((r.shot_id, r.player_id), (None, None));
    let snap: Snapshot = serde_json::from_str(
        r#"{ "session": { "apiVersion": 1, "matchId": null, "phase": null, "isTeamGame": false,
                          "scoringModel": null, "matchActive": false, "paused": false },
             "course": null, "hole": null, "turn": null,
             "players": [ { "playerId": 1, "name": "Joe", "handedness": "Unknown", "isLocal": true,
                            "ball": null, "aim": null, "club": null, "clubId": null, "bag": null,
                            "profileId": null, "lie": null } ],
             "shot": null,
             "readiness": { "shotId": null, "playerId": null, "ready": false, "controllable": false,
                            "allowedCommands": [], "blockers": ["not_in_play"] },
             "lie": null, "ruling": null }"#,
    )
    .expect("snapshot");
    assert_eq!(snap.hole, None);
    assert_eq!(snap.turn, None);
    assert_eq!(snap.players[0].club, None);
    let Incoming::Response(r) = parse(
        r#"{ "type": "response", "id": null, "ok": false,
             "error": { "code": "unauthorised", "message": "Removed." } }"#,
    )
    .expect("response") else {
        panic!("not a response");
    };
    assert_eq!(r.id, None);
    let e = event(&wrap(
        "hole.started",
        r#"{ "matchId": "m", "holeNumber": 1, "par": 4, "strokeIndex": 1, "yards": null,
             "pin": [1.0, 2.0, 3.0], "playerIds": [1] }"#,
    ));
    let h: HoleStarted = payload(&e).expect("hole");
    assert_eq!(h.hole_number, Some(1));
}

#[test]
fn names_compare_without_regard_to_case_and_unknown_types_are_ignored() {
    let e = event(r#"{ "type": "EVENT", "event": "Turn.Changed", "seq": 3, "data": {} }"#);
    assert_eq!(e.name, "turn.changed");
    assert_eq!(
        parse(r#"{ "type": "something.new", "data": { "x": 1 } }"#).expect("other"),
        Incoming::Other("something.new".into())
    );
    assert!(parse("not json").is_err());
}
