//! The card as the bus's game, against the mock server and the real
//! session: complete selects, re-assert, the mode rule, `game.*` and
//! `golfer.profile`, a linked profile's shots, `game.aim`.

use super::*;
use crate::bus::BusMsg;
use flightrelay::{FrpMessage, GolferTurn, Mode, ReplyError, RoundStatus, StateKind};

impl Card {
    /// Wait for the next bus message `pred` picks, from anyone.
    fn frp_until(&mut self, pred: impl Fn(&FrpMessage) -> bool) -> FrpMessage {
        let deadline = Instant::now() + WAIT;
        let mut seen = 0;
        loop {
            assert!(Instant::now() < deadline, "no such message on the bus");
            if let Some(found) = self.raw[seen..]
                .iter()
                .filter_map(BusMsg::as_frp)
                .find(|m| pred(m))
            {
                return found.clone();
            }
            seen = self.raw.len();
            if self.step().is_none() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// The session's turns on the bus so far.
    fn turns(&self) -> Vec<GolferTurn> {
        let session = self.state.flighthook_id();
        self.raw
            .iter()
            .filter_map(|m| m.turn_of(&session).map(|(t, _)| t.clone()))
            .collect()
    }

    /// The card's messages of `kind` on the bus so far.
    fn own_bodies(&self) -> Vec<FrpBody> {
        self.raw
            .iter()
            .filter_map(BusMsg::as_frp)
            .filter(|m| m.from == ACTOR)
            .map(|m| m.body.clone())
            .collect()
    }

    /// Wait until the session's turn satisfies `pred`; that turn.
    fn turn_until(&mut self, pred: impl Fn(&GolferTurn) -> bool) -> GolferTurn {
        let session = self.state.flighthook_id();
        let msg = self.frp_until(|m| {
            m.from == session && matches!(&m.body, FrpBody::GolferTurn(t) if pred(t))
        });
        let FrpBody::GolferTurn(turn) = msg.body else {
            unreachable!()
        };
        turn
    }
}

fn golfer(turn: &GolferTurn) -> Option<&str> {
    turn.golfer.as_ref().map(flightrelay::GolferId::as_str)
}

/// `origin` (actor id) sends `select` on bus `tx`.
fn send_select(tx: &broadcast::Sender<BusMsg>, origin: &str, select: flightrelay::GolferSelect) {
    BusSender::new(origin.into(), tx.clone(), Arc::default())
        .with_frp_id(Some(origin.into()))
        .frp(select);
}

/// A card following [`round`] on a bus the test can put messages on.
fn rig() -> (Card, Peer, broadcast::Sender<BusMsg>, Mock) {
    let mock = Mock::new();
    let state = new_state();
    let (tx, _) = broadcast::channel(4096);
    let (card, peer) = connected_on(&mock, state, &tx);
    (card, peer, tx, mock)
}

/// [`rig`] with Joe's profile linked to a saved golfer (Sam, right-handed)
/// who is not the first golfer (Eric, left-handed): when the connection
/// ends, the turn goes to Sam, who is the same golfer as the game's Joe,
/// with the same club, hand and mode.
fn rig_linked() -> (Card, Peer, broadcast::Sender<BusMsg>, Mock) {
    let mock = Mock::new();
    let state = new_state();
    state.system.update(|c| {
        c.golfers = vec![
            flighthook::GolferRecord {
                handed: Some(Handedness::Left),
                ..flighthook::GolferRecord::new("flighthook.eric", "Eric")
            },
            flighthook::GolferRecord {
                handed: Some(Handedness::Right),
                identifiers: vec!["flighthook.sam".into(), "pureplay.Joe".into()],
                ..flighthook::GolferRecord::new("flighthook.sam", "Sam")
            },
        ];
    });
    let (tx, _) = broadcast::channel(4096);
    let (card, peer) = connected_on(&mock, state, &tx);
    (card, peer, tx, mock)
}

/// The turn Joe (player 1, local) holds in [`round`].
fn is_joe(t: &GolferTurn) -> bool {
    golfer(t) == Some("pureplay.Joe")
        && t.club == Some(Club::Iron7)
        && t.handedness == Some(Handedness::Right)
        && t.mode == Some(Mode::Full)
}

/// A round with two local players: the turn to the second switches the
/// golfer, club and hand in one turn change, with no club flip (no turn
/// with the new golfer and the old club, or the old golfer and the new
/// club).
#[test]
fn a_round_with_two_local_players_switches_the_golfer_with_no_club_flip() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.turn_until(is_joe);
    let before = card.turns().len();
    peer.event(
        "turn.changed",
        2,
        &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "holeNumber": 7, "reason": "next_player" }),
    );
    let ann = card.turn_until(|t| golfer(t) == Some("pureplay.Ann"));
    assert_eq!(ann.club, Some(Club::Driver));
    assert_eq!(ann.handedness, Some(Handedness::Left));
    card.settle();
    let after: Vec<_> = card.turns()[before..].to_vec();
    assert_eq!(after, [ann], "one turn change");
    card.stop();
}

/// A header golfer pick (flighthook's own select) while a local player is
/// up is sent straight back.
#[test]
fn a_header_golfer_pick_is_reasserted() {
    let (mut card, _peer, tx, _mock) = rig();
    card.turn_until(is_joe);
    let session = card.state.flighthook_id();
    let mut pick = flightrelay::GolferSelect::new("ui-1");
    pick.golfer = Some(Some("flighthook.someone".parse().expect("id")));
    send_select(&tx, &session, pick);
    card.turn_until(|t| golfer(t) == Some("flighthook.someone"));
    card.turn_until(is_joe);
    card.stop();
}

/// A header club pick with club out off (the pairing grants no control):
/// the card's complete select puts Pureplay's club back.
#[test]
fn a_header_club_pick_is_reasserted_while_club_out_is_off() {
    let (mut card, _peer, tx, _mock) = rig();
    card.turn_until(is_joe);
    let session = card.state.flighthook_id();
    let mut pick = flightrelay::GolferSelect::new("ui-2");
    pick.club = Some(Some(Club::Wood5));
    send_select(&tx, &session, pick);
    card.turn_until(|t| t.club == Some(Club::Wood5));
    card.turn_until(is_joe);
    card.stop();
}

/// The mode follows the club and the distance to the hole: unknown is a
/// full swing, 150 m full, 20 m chipping, the putter putting.
#[test]
fn the_mode_follows_the_club_and_the_distance() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.turn_until(is_joe);
    let init = |id: &str, m: f64| {
        json!({ "shotId": id, "matchId": "m-1", "playerId": 1, "holeNumber": 7, "strokeNumber": 2,
                "ballLocation": [1.0, 2.0, 3.0], "club": "Iron7", "clubId": "b-7i",
                "distanceToHoleM": m, "replacesShotId": null })
    };
    peer.event("shot.initialized", 2, &init("s-2", 20.0));
    card.turn_until(|t| t.mode == Some(Mode::Chipping) && t.club == Some(Club::Iron7));
    peer.event("shot.initialized", 3, &init("s-3", 150.0));
    card.turn_until(|t| t.mode == Some(Mode::Full));
    peer.event(
        "player.club_changed",
        4,
        &json!({ "playerId": 1, "current": "Putter", "currentClubId": "b-pt", "reason": "user_selected" }),
    );
    card.turn_until(|t| t.mode == Some(Mode::Putting) && t.club == Some(Club::Putter));
    card.stop();
}

/// A turn a simulator card caused is not answered (the last sender wins);
/// Pureplay's next club event sends the card's select again.
#[test]
fn a_simulator_cards_turn_is_not_reasserted() {
    let (mut card, mut peer, tx, _mock) = rig();
    card.state.system.update(|c| {
        c.openconnect_client.insert(
            "0".into(),
            flighthook::OpenConnectClientSection {
                actor_id: Some("openconnect_client.0".into()),
                ..Default::default()
            },
        );
    });
    card.turn_until(is_joe);
    let mut gspro = flightrelay::GolferSelect::new("gs-1");
    gspro.club = Some(Some(Club::Iron8));
    send_select(&tx, "openconnect_client.0", gspro);
    card.turn_until(|t| t.club == Some(Club::Iron8));
    let before = card.turns().len();
    card.settle();
    assert_eq!(card.turns().len(), before, "no answer to a simulator card");
    peer.event(
        "player.club_changed",
        2,
        &json!({ "playerId": 1, "current": "Driver", "currentClubId": "b-dr", "reason": "user_selected" }),
    );
    card.turn_until(|t| t.club == Some(Club::Driver));
    card.stop();
}

/// `game.aim` to the card is answered `unsupported`.
#[test]
fn game_aim_is_answered_unsupported() {
    let (mut card, _peer, tx, _mock) = rig();
    BusSender::new("tool".into(), tx.clone(), Arc::default())
        .with_frp_id(Some("tool".into()))
        .frp(flightrelay::GameAim {
            r#ref: "aim-1".into(),
            action: flightrelay::AimAction::Nudge,
            degrees: Some(2.0),
            target: None,
        });
    let reply = card.frp_until(|m| m.from == ACTOR && matches!(m.body, FrpBody::BusReply(_)));
    let FrpBody::BusReply(reply) = reply.body else {
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
    card.stop();
}

/// The card publishes every player's profile, the round, the ball and the
/// scores; a player who leaves is removed; a round that ended goes out
/// ended, then removed. Its `bus.actor` lists Pureplay's blockers and the
/// `pureplay` namespace.
#[test]
fn the_card_publishes_profiles_the_round_the_ball_and_the_scores() {
    let (mut card, mut peer, _tx, _mock) = rig();
    let round = card.frp_until(|m| m.from == ACTOR && matches!(m.body, FrpBody::GameRound(_)));
    let FrpBody::GameRound(round) = round.body else {
        unreachable!()
    };
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
    assert_eq!(round.up.as_ref().map(|g| g.as_str()), Some("pureplay.Joe"));
    let hole = round.hole.expect("hole");
    assert_eq!((hole.number, hole.par), (7, Some(4)));
    assert_eq!(
        serde_json::to_value(hole.length).expect("json"),
        json!("410yd")
    );
    let ball = card.frp_until(|m| m.from == ACTOR && matches!(m.body, FrpBody::GameBall(_)));
    let FrpBody::GameBall(ball) = ball.body else {
        unreachable!()
    };
    assert_eq!(
        (ball.golfer.as_str(), ball.hole, ball.stroke_number),
        ("pureplay.Joe", 7, 1)
    );
    assert_eq!(ball.allowed_commands, None, "never game.aim");
    card.settle();
    let profiles: Vec<_> = card
        .own_bodies()
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
    let actor = card.frp_until(|m| m.from == ACTOR && matches!(m.body, FrpBody::BusActor(_)));
    let FrpBody::BusActor(actor) = actor.body else {
        unreachable!()
    };
    assert_eq!(actor.namespaces, ["pureplay"]);
    assert!(actor.has_role(roles::GAME));

    peer.event(
        "shot.ready_changed",
        2,
        &json!({ "shotId": "shot-1", "playerId": 1, "ready": false, "controllable": false,
                 "allowedCommands": [], "blockers": ["shot_in_progress"] }),
    );
    card.frp_until(|m| {
        m.from == ACTOR
            && matches!(&m.body, FrpBody::BusActor(a) if a.blockers == ["shot_in_progress"])
    });
    peer.event(
        "score.updated",
        3,
        &json!({ "matchId": "m-1", "shotId": "shot-1", "holeNumber": 7, "playerId": 1,
                 "teamIndex": null, "holeStrokes": 3, "totalStrokes": 32, "grossToPar": -1,
                 "netToPar": -2, "holeResult": "Birdie", "holedOut": true, "final": true,
                 "reason": "hole_completed" }),
    );
    let scores = card.frp_until(|m| m.from == ACTOR && matches!(m.body, FrpBody::GameScores(_)));
    let FrpBody::GameScores(scores) = scores.body else {
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

    peer.event("player.left", 4, &json!({ "playerId": 3 }));
    card.frp_until(|m| {
        m.from == ACTOR
            && matches!(&m.body, FrpBody::Removed(r)
                if r.kind == StateKind::GolferProfile && r.id.as_deref() == Some("pureplay.Remote"))
    });
    peer.event(
        "match.ended",
        5,
        &json!({ "matchId": "m-1", "reason": "completed", "standings": [] }),
    );
    card.frp_until(|m| {
        m.from == ACTOR
            && matches!(&m.body, FrpBody::GameRound(r)
                if r.status == RoundStatus::Ended
                    && r.end_reason == Some(flightrelay::EndReason::Completed))
    });
    card.frp_until(|m| {
        m.from == ACTOR && matches!(&m.body, FrpBody::Removed(r) if r.kind == StateKind::GameRound)
    });
    card.stop();
}

/// `context.invalidated` removes the round.
#[test]
fn context_invalidated_removes_the_round() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.frp_until(|m| m.from == ACTOR && matches!(m.body, FrpBody::GameRound(_)));
    peer.event(
        "context.invalidated",
        2,
        &json!({ "reason": "ended", "matchId": "m-1" }),
    );
    card.frp_until(|m| {
        m.from == ACTOR && matches!(&m.body, FrpBody::Removed(r) if r.kind == StateKind::GameRound)
    });
    card.stop();
}

/// A golfer listing the player's id: the card's own shot is stored
/// with the player's id on its turn, and the export counts it under the
/// golfer.
#[test]
fn a_linked_profiles_shots_land_under_the_golfer() {
    let mock = Mock::new();
    let state = new_state();
    state.system.update(|c| {
        c.golfers = vec![flighthook::GolferRecord {
            identifiers: vec!["flighthook.eric".into(), "pureplay.Joe".into()],
            ..flighthook::GolferRecord::new("flighthook.eric", "Eric")
        }];
    });
    let (tx, _) = broadcast::channel(4096);
    let (mut card, mut peer) = connected_on(&mock, state.clone(), &tx);
    card.turn_until(|t| t.club == Some(Club::Iron7));
    peer.event("shot.launched", 2, &launched("pp-1", 1));
    card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::ShotFinished { .. }));
    peer.event("shot.completed", 3, &completed("pp-1", 1, "InPlay"));
    card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::Stroke { .. }));
    card.settle();
    let shots = stored(&state, &card.raw);
    assert_eq!(shots.len(), 1);
    let turn = shots[0].turn.as_ref().expect("turn");
    let config = state.system.snapshot();
    assert_eq!(
        config.golfer_key(turn.golfer.as_ref().map(flightrelay::GolferId::as_str)),
        Some("flighthook.eric".into())
    );
    assert_eq!(
        shots[0].stroke.as_ref().map(|s| s.golfer.as_str()),
        Some("pureplay.Joe")
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let lines = rt.block_on(crate::actors::web::tests::export_lines(&config, &card.raw));
    let line: Value = serde_json::from_str(&lines[0]).expect("json");
    assert_eq!(line["golfer_id"], "flighthook.eric");
    card.stop();
}

impl Card {
    /// The `golfer.select`s the card put on the bus so far.
    fn selects(&self) -> usize {
        self.own_bodies()
            .iter()
            .filter(|b| matches!(b, FrpBody::GolferSelect(_)))
            .count()
    }

    /// Stop the card's thread and start another on the same bus and state
    /// (the card's own restart; the session keeps running).
    fn restart_on(&mut self, mock: &Mock, tx: &broadcast::Sender<BusMsg>) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            t.join().expect("the card's thread ends");
        }
        self.shutdown = Arc::new(AtomicBool::new(false));
        let sender = BusSender::new(ACTOR.into(), tx.clone(), Arc::clone(&self.shutdown))
            .with_frp_id(Some(ACTOR.into()));
        let receiver = sender.subscribe();
        let state = Arc::clone(&self.state.state);
        let tokens = Arc::clone(&self.state.pureplay_tokens);
        let addr = mock.addr;
        self.thread = Some(std::thread::spawn(move || {
            run_with(addr, &state, &tokens, &sender, receiver, PING_EVERY);
        }));
    }
}

/// A dropped connection with the round unchanged: the turn goes back to
/// the same golfer (Eric is Joe) and still matches, so the card sends
/// nothing more.
#[test]
fn a_reconnect_with_an_unchanged_round_sends_no_select() {
    let (mut card, peer, _tx, mock) = rig_linked();
    card.turn_until(is_joe);
    card.settle();
    let before = card.selects();
    assert_eq!(before, 1, "one select for the snapshot");
    drop(peer);
    card.shows(ActorStatus::Reconnecting, |_| true);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some("PPA-SHOTS"));
    peer.paired("PPA-SHOTS");
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    card.settle();
    card.settle();
    assert_eq!(card.selects(), before, "the turn already matches");
    card.stop();
}

/// A restarted card finds the session's turn already matching Pureplay's.
#[test]
fn a_card_restart_with_a_matching_turn_sends_no_select() {
    let (mut card, peer, tx, mock) = rig_linked();
    card.turn_until(is_joe);
    card.settle();
    let before = card.selects();
    drop(peer);
    card.restart_on(&mock, &tx);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some("PPA-SHOTS"));
    peer.paired("PPA-SHOTS");
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    card.settle();
    card.settle();
    assert_eq!(card.selects(), before, "the turn already matches");
    card.stop();
}

/// With the turn gone back to no golfer, the card takes it once on the
/// reconnect, and no more.
#[test]
fn a_reconnect_after_the_turn_went_back_sends_one_select() {
    let (mut card, peer, _tx, mock) = rig();
    card.turn_until(is_joe);
    card.settle();
    let before = card.selects();
    drop(peer);
    card.shows(ActorStatus::Reconnecting, |_| true);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some("PPA-SHOTS"));
    peer.paired("PPA-SHOTS");
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    card.settle();
    card.settle();
    assert_eq!(card.selects(), before + 1, "one, not a stream");
    card.stop();
}

impl Card {
    /// The `game.ball`s the card put on the bus so far.
    fn balls(&self) -> Vec<flightrelay::GameBall> {
        self.own_bodies()
            .into_iter()
            .filter_map(|b| match b {
                FrpBody::GameBall(ball) => Some(ball),
                _ => None,
            })
            .collect()
    }
}

/// A gimme, the next tee and the driver: the turn changes once, to the
/// driver in a full swing, with no chipping on the way.
#[test]
fn a_gimme_then_the_next_tee_changes_the_turn_once() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.turn_until(is_joe);
    card.settle();
    let before = card.turns().len();
    peer.event(
        "shot.completed",
        2,
        &json!({ "shotId": "s-9", "matchId": "m-1", "playerId": 1, "holeNumber": 7,
                 "strokeNumber": 3, "outcome": "Gimme", "endDistToHoleM": 0.8,
                 "lie": "Green" }),
    );
    peer.event(
        "hole.started",
        3,
        &json!({ "matchId": "m-1", "holeNumber": 8, "par": 5 }),
    );
    peer.event(
        "player.club_changed",
        4,
        &json!({ "playerId": 1, "current": "Driver", "currentClubId": "b-dr", "reason": "auto" }),
    );
    peer.event(
        "shot.initialized",
        5,
        &json!({ "shotId": "s-10", "matchId": "m-1", "playerId": 1, "holeNumber": 8,
                 "strokeNumber": 1, "ballLocation": [0.0, 0.0, 0.0], "club": "Driver",
                 "clubId": "b-dr", "distanceToHoleM": 375.0 }),
    );
    card.turn_until(|t| t.club == Some(Club::Driver) && t.mode == Some(Mode::Full));
    card.settle();
    let turns: Vec<_> = card.turns()[before..]
        .iter()
        .map(|t| (t.club, t.mode))
        .collect();
    assert_eq!(turns, [(Some(Club::Driver), Some(Mode::Full))]);
    card.stop();
}

/// The tee ball of the next hole carries no lie from the last stroke of
/// the hole before; a `lie.changed` of the shot up gives it one, another
/// shot's does not.
#[test]
fn the_ball_carries_only_the_lie_of_its_own_shot() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.turn_until(is_joe);
    peer.event(
        "shot.completed",
        2,
        &json!({ "shotId": "s-9", "matchId": "m-1", "playerId": 1, "holeNumber": 7,
                 "strokeNumber": 3, "outcome": "Holed", "endDistToHoleM": 0.0, "lie": "Green" }),
    );
    peer.event(
        "hole.started",
        3,
        &json!({ "matchId": "m-1", "holeNumber": 8, "par": 5 }),
    );
    peer.event(
        "shot.initialized",
        4,
        &json!({ "shotId": "s-10", "matchId": "m-1", "playerId": 1, "holeNumber": 8,
                 "strokeNumber": 1, "ballLocation": [0.0, 0.0, 0.0], "club": "Iron7",
                 "clubId": "b-7i", "distanceToHoleM": 137.0 }),
    );
    card.frp_until(|m| m.from == ACTOR && matches!(&m.body, FrpBody::GameBall(b) if b.hole == 8));
    card.settle();
    let tee = card
        .balls()
        .into_iter()
        .rev()
        .find(|b| b.hole == 8)
        .expect("the tee ball");
    assert_eq!(tee.lie, None, "no surface from the last putt");
    peer.event(
        "lie.changed",
        5,
        &json!({ "shotId": "s-77", "playerId": 1, "surface": "Rough" }),
    );
    peer.event(
        "lie.changed",
        6,
        &json!({ "shotId": "s-10", "playerId": 1, "surface": "Tee", "slopeUpDownDeg": 1.5 }),
    );
    let lie = |b: &flightrelay::GameBall| b.lie.clone();
    card.frp_until(|m| {
        m.from == ACTOR && matches!(&m.body, FrpBody::GameBall(b) if lie(b).is_some())
    });
    card.settle();
    let with = card
        .balls()
        .into_iter()
        .rev()
        .find_map(|b| lie(&b))
        .expect("a lie");
    assert_eq!(with.surface.as_deref(), Some("tee"));
    assert_eq!(with.slope_up_down, Some(1.5));
    assert!(
        card.balls()
            .iter()
            .filter_map(lie)
            .all(|l| l.surface.as_deref() != Some("rough")),
        "another shot's lie"
    );
    card.stop();
}

/// A mulligan: the stroke of the replaying shot names the one it replaces.
#[test]
fn a_mulligan_stroke_names_the_stroke_it_replaces() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.turn_until(is_joe);
    peer.event("shot.launched", 2, &launched("shot-42", 1));
    peer.event("shot.completed", 3, &completed("shot-42", 1, "InPlay"));
    peer.event(
        "shot.replayed",
        4,
        &json!({ "shotId": "shot-51", "replacesShotId": "shot-42", "playerId": 1,
                 "reason": "mulligan" }),
    );
    peer.event("shot.launched", 5, &launched("shot-51", 1));
    peer.event("shot.completed", 6, &completed("shot-51", 1, "Green"));
    card.settle();
    let strokes = card.strokes();
    let named: Vec<_> = strokes
        .iter()
        .map(|(_, s)| (s.stroke_id.as_str(), s.replaces.as_deref()))
        .collect();
    assert_eq!(
        named,
        [("shot-42", None), ("shot-51", Some("shot-42"))],
        "{named:?}"
    );
    card.stop();
}

/// The card's `bus.actor` is the first thing it says, before a profile in
/// its namespace.
#[test]
fn the_bus_actor_comes_before_the_first_profile() {
    let (mut card, _peer, _tx, _mock) = rig();
    card.settle();
    let kinds: Vec<_> = card
        .own_bodies()
        .into_iter()
        .filter_map(|b| match b {
            FrpBody::BusActor(_) => Some("actor"),
            FrpBody::GolferProfile(_) => Some("profile"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds.first(), Some(&"actor"), "{kinds:?}");
    card.stop();
}

/// A discard of a shot the card never mapped (a remote player's) still
/// stops a stroke that arrives after it.
#[test]
fn a_discard_is_remembered_for_a_shot_the_card_never_mapped() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.turn_until(is_joe);
    peer.event("shot.launched", 2, &launched("pp-9", 3));
    peer.event(
        "shot.cancelled",
        3,
        &json!({ "shotId": "pp-9", "playerId": 3, "reason": "discarded" }),
    );
    peer.event("shot.completed", 4, &completed("pp-9", 3, "InPlay"));
    // A later shot's stroke proves the card read all of it.
    peer.event("shot.launched", 5, &launched("pp-10", 1));
    peer.event("shot.completed", 6, &completed("pp-10", 1, "InPlay"));
    card.settle();
    let ids: Vec<_> = card
        .strokes()
        .into_iter()
        .map(|(_, s)| s.stroke_id)
        .collect();
    assert_eq!(ids, ["pp-10"]);
    card.stop();
}

/// The store joins a `game.stroke` to its shot only when the sender holds
/// `game` (its `bus.actor` says so).
#[test]
fn a_stroke_joins_its_shot_only_from_the_actor_holding_game() {
    let mock = Mock::new();
    let state = new_state();
    let (tx, _) = broadcast::channel(4096);
    let (mut card, mut peer) = connected_on(&mock, state.clone(), &tx);
    card.turn_until(is_joe);
    peer.event("shot.launched", 2, &launched("pp-1", 1));
    card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::ShotFinished { .. }));
    peer.event("shot.completed", 3, &completed("pp-1", 1, "InPlay"));
    card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::Stroke { .. }));
    card.settle();
    let shots = stored(&state, &card.raw);
    assert_eq!(shots.len(), 1);
    assert!(shots[0].stroke.is_some(), "from the game");
    let silent: Vec<_> = card
        .raw
        .iter()
        .filter(|m| !matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::BusActor(_))))
        .cloned()
        .collect();
    let shots = stored(&state, &silent);
    assert_eq!(shots.len(), 1);
    assert!(
        shots[0].stroke.is_none(),
        "from an actor that never said game"
    );
    // A stroke that arrives while its shot is still pending joins it too.
    let mut late = card.raw.clone();
    let finished = late
        .iter()
        .position(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::ShotFinished(_))))
        .expect("finished");
    let finished = late.remove(finished);
    let stroke = late
        .iter()
        .position(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::GameStroke(_))))
        .expect("stroke");
    late.insert(stroke + 1, finished);
    let shots = stored(&state, &late);
    assert_eq!(shots.len(), 1);
    assert!(shots[0].stroke.is_some(), "the stroke before the finish");
    card.stop();
}

impl Card {
    /// Wait for a turn after the first `seen` of them; the first of those.
    fn turn_after(&mut self, seen: usize) -> GolferTurn {
        let deadline = Instant::now() + WAIT;
        while self.turns().len() <= seen {
            assert!(Instant::now() < deadline, "no turn after {seen}");
            if self.step().is_none() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        self.turns()[seen].clone()
    }
}

/// The golfer a test starts with when the config lists Eric.
fn with_eric(state: &TestState) {
    state.system.update(|c| {
        let mut eric = flighthook::GolferRecord::new("flighthook.eric", "Eric");
        eric.handed = Some(Handedness::Left);
        c.golfers = vec![eric];
    });
}

/// A round that ends takes its golfers off the bus, and the turn goes back
/// to the start golfer (here, none), mode full.
#[test]
fn the_end_of_the_round_returns_the_turn_to_the_start_golfer() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.turn_until(is_joe);
    peer.event(
        "shot.initialized",
        2,
        &json!({ "shotId": "s-2", "playerId": 1, "holeNumber": 7, "strokeNumber": 2,
                 "club": "Iron7", "distanceToHoleM": 20.0 }),
    );
    card.turn_until(|t| t.mode == Some(Mode::Chipping));
    card.settle();
    let before = card.turns().len();
    peer.event(
        "match.ended",
        3,
        &json!({ "matchId": "m-1", "reason": "completed", "standings": [] }),
    );
    card.turn_after(before);
    card.settle();
    let after = card.turns()[before..].to_vec();
    assert_eq!(after.len(), 1, "one turn change, which stays");
    assert_eq!(golfer(&after[0]), None);
    assert_eq!(after[0].mode, Some(Mode::Full));
    assert_eq!(after[0].handedness, Some(Handedness::Right));
    card.stop();
}

/// The connection ending does the same, with the saved golfer's hand.
#[test]
fn a_closed_connection_returns_the_turn_to_the_start_golfer() {
    let mock = Mock::new();
    let state = new_state();
    with_eric(&state);
    let (tx, _) = broadcast::channel(4096);
    let (mut card, peer) = connected_on(&mock, state, &tx);
    card.turn_until(is_joe);
    card.settle();
    let before = card.turns().len();
    drop(peer);
    let back = card.turn_after(before);
    assert_eq!(golfer(&back), Some("flighthook.eric"));
    assert_eq!(back.handedness, Some(Handedness::Left));
    assert_eq!(back.mode, Some(Mode::Full));
    card.settle();
    assert_eq!(
        card.turns().len(),
        before + 1,
        "the card does not take the turn back while it is not connected"
    );
    card.stop();
}

// -- a second match in the same Pureplay run ----------------------------------

impl Peer {
    /// The card's next `state.get`, skipping other messages.
    fn state_get(&mut self) -> Value {
        loop {
            let msg = self.recv();
            if msg["type"] == "state.get" {
                return msg;
            }
        }
    }

    /// Whether the card sends a `state.get` within [`QUIET`].
    fn asks_for_state(&mut self) -> bool {
        let until = Instant::now() + QUIET;
        while Instant::now() < until {
            match self.ws.read() {
                Ok(Message::Text(t)) if t.as_str().contains("state.get") => return true,
                Ok(Message::Binary(b)) if String::from_utf8_lossy(&b).contains("state.get") => {
                    return true;
                }
                _ => {}
            }
        }
        false
    }
}

/// [`round`] as match `id`.
fn round_of(id: &str) -> Value {
    let mut r = round();
    r["session"]["matchId"] = json!(id);
    r
}

/// Joe takes a shot in match `m-1`, which then ends.
fn first_match_with_a_shot(card: &mut Card, peer: &mut Peer) {
    card.turn_until(is_joe);
    peer.event("shot.launched", 2, &launched("pp-1", 1));
    card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::ShotFinished { .. }));
    peer.event("shot.completed", 3, &completed("pp-1", 1, "InPlay"));
    card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::Stroke { .. }));
    peer.event(
        "match.ended",
        4,
        &json!({ "matchId": "m-1", "reason": "completed", "standings": [] }),
    );
    card.frp_until(|m| {
        m.from == ACTOR && matches!(&m.body, FrpBody::Removed(r) if r.kind == StateKind::GameRound)
    });
    card.settle();
}

/// The events of a second match with Joe up, as Pureplay sends them (no
/// snapshot), from `seq`.
fn second_match_events(peer: &mut Peer, seq: u64) {
    peer.event(
        "match.started",
        seq,
        &json!({ "matchId": "m-2", "courseName": "Course", "holeCount": 18, "totalPar": 72,
                 "scoringModel": "Stroke", "isTeamGame": false, "playerIds": [1, 2, 3] }),
    );
    peer.event(
        "hole.started",
        seq + 1,
        &json!({ "matchId": "m-2", "holeNumber": 1, "par": 4, "playerIds": [1, 2, 3] }),
    );
    peer.event(
        "turn.changed",
        seq + 2,
        &json!({ "matchId": "m-2", "previousPlayerId": null, "currentPlayerId": 1,
                 "holeNumber": 1, "reason": "next_player" }),
    );
    peer.event(
        "shot.initialized",
        seq + 3,
        &json!({ "shotId": "s-2", "playerId": 1, "holeNumber": 1, "strokeNumber": 1,
                 "club": "Iron7", "distanceToHoleM": 300.0 }),
    );
}

/// A match that starts after another one ended, announced by events only:
/// the card asks for the state, the answer brings the players back, and
/// the match's shot is taken and the turn selected once.
#[test]
fn a_second_match_announced_by_events_is_taken_once_the_state_answers() {
    let (mut card, mut peer, _tx, _mock) = rig();
    first_match_with_a_shot(&mut card, &mut peer);
    let (triggers, selects) = (card.triggers().len(), card.selects());
    second_match_events(&mut peer, 5);
    let get = peer.state_get();
    peer.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 9,
                       "requestId": get["id"], "data": round_of("m-2") }),
    );
    card.turn_until(is_joe);
    peer.event("shot.launched", 10, &launched("pp-2", 1));
    card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::ShotFinished { .. }));
    card.settle();
    assert_eq!(card.triggers().len(), triggers + 1, "the shot is taken");
    assert_eq!(card.selects(), selects + 1, "one select for the new match");
    card.stop();
}

/// The same with no answer to `state.get`: the card takes no shot and
/// sends no select.
#[test]
fn a_second_match_with_no_state_answer_stays_quiet() {
    let (mut card, mut peer, _tx, _mock) = rig();
    first_match_with_a_shot(&mut card, &mut peer);
    let (triggers, selects, turns) = (card.triggers().len(), card.selects(), card.turns().len());
    second_match_events(&mut peer, 5);
    let _ = peer.state_get();
    peer.event("shot.launched", 9, &launched("pp-2", 1));
    peer.event("shot.completed", 10, &completed("pp-2", 1, "InPlay"));
    card.settle();
    card.settle();
    assert_eq!(card.triggers().len(), triggers, "no shot is taken");
    assert_eq!(card.selects(), selects, "no select is sent");
    assert_eq!(card.turns().len(), turns, "the turn stays");
    card.stop();
}

/// `turn.changed` for player 1 in match `m-2`, from `seq`, `n` times.
fn player_one_named(peer: &mut Peer, seq: u64, n: u64) {
    for i in 0..n {
        peer.event(
            "turn.changed",
            seq + i,
            &json!({ "matchId": "m-2", "previousPlayerId": null, "currentPlayerId": 1,
                     "holeNumber": 1, "reason": "next_player" }),
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Answer `get` with the snapshot of match `m-2`, then Joe's shot: the
/// shot is taken.
fn answer_and_take_the_shot(card: &mut Card, peer: &mut Peer, get: &Value, seq: u64) {
    let triggers = card.triggers().len();
    peer.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": seq,
                       "requestId": get["id"], "data": round_of("m-2") }),
    );
    card.turn_until(is_joe);
    peer.event("shot.launched", seq + 1, &launched("pp-2", 1));
    card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::ShotFinished { .. }));
    card.settle();
    assert_eq!(card.triggers().len(), triggers + 1, "the shot is taken");
}

/// A `state.get` that gets no answer at a second match marks nobody:
/// events naming player 1 after it timed out ask for the state again, and
/// the shot is taken once a snapshot brings the player.
#[test]
fn an_unanswered_state_get_asks_for_the_player_again() {
    let (mut card, mut peer, _tx, _mock) = rig();
    first_match_with_a_shot(&mut card, &mut peer);
    second_match_events(&mut peer, 5);
    let first = peer.state_get();
    // Past the timeout.
    std::thread::sleep(SNAPSHOT_TIMEOUT + Duration::from_millis(100));
    player_one_named(&mut peer, 9, 10);
    let again = peer.state_get();
    assert_ne!(again["id"], first["id"], "a new request");
    answer_and_take_the_shot(&mut card, &mut peer, &again, 19);
    card.stop();
}

/// The same with the request refused (`rate_limited`): it is asked again
/// after the wait, and the shot is taken once a snapshot answers.
#[test]
fn a_refused_state_get_is_asked_again_after_a_wait() {
    let (mut card, mut peer, _tx, _mock) = rig();
    first_match_with_a_shot(&mut card, &mut peer);
    second_match_events(&mut peer, 5);
    let first = peer.state_get();
    peer.send(&json!({ "type": "response", "id": first["id"], "ok": false,
                       "error": { "code": "rate_limited", "message": "Slow down.",
                                  "retryAfterMs": 50 } }));
    player_one_named(&mut peer, 9, 3);
    let again = peer.state_get();
    assert_ne!(again["id"], first["id"], "a new request");
    answer_and_take_the_shot(&mut card, &mut peer, &again, 12);
    card.stop();
}

/// Requests that get no snapshot are paced by a growing wait, never one
/// per event.
#[test]
fn unanswered_state_gets_are_paced() {
    let (mut card, mut peer, _tx, _mock) = rig();
    first_match_with_a_shot(&mut card, &mut peer);
    second_match_events(&mut peer, 5);
    let _ = peer.state_get();
    let mut at = vec![Instant::now()];
    let deadline = Instant::now() + WAIT * 2;
    let mut seq = 9;
    while at.len() < 4 {
        assert!(Instant::now() < deadline, "no request after {at:?}");
        player_one_named(&mut peer, seq, 1);
        seq += 1;
        if peer.asks_for_state() {
            at.push(Instant::now());
        }
    }
    let gaps: Vec<Duration> = at.windows(2).map(|w| w[1] - w[0]).collect();
    // The test sees a request up to [`QUIET`] late.
    for (gap, retry) in gaps.iter().zip(STATE_GET_RETRY) {
        assert!(
            *gap + QUIET >= SNAPSHOT_TIMEOUT + retry,
            "{gaps:?}: a request goes out only after the timeout and the wait"
        );
    }
    assert!(gaps[2] > gaps[0], "{gaps:?}: the wait grows");
    card.stop();
}

/// A player named while a request is out is asked for by the next request
/// when the answer does not bring them, not marked.
#[test]
fn a_player_named_while_a_request_is_out_is_asked_for_by_the_next() {
    let (mut card, mut peer, _tx, _mock) = rig();
    first_match_with_a_shot(&mut card, &mut peer);
    peer.event(
        "match.started",
        5,
        &json!({ "matchId": "m-2", "courseName": "Course", "holeCount": 18, "totalPar": 72,
                 "scoringModel": "Stroke", "isTeamGame": false, "playerIds": [1, 2, 3] }),
    );
    let first = peer.state_get();
    player_one_named(&mut peer, 6, 1);
    let mut empty = round_of("m-2");
    empty["players"] = json!([]);
    empty["turn"] = json!({ "activeShooterId": null, "remainingPlayerIds": [] });
    empty["shot"] = Value::Null;
    empty["readiness"] = Value::Null;
    peer.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 7,
                       "requestId": first["id"], "data": empty }),
    );
    let next = peer.state_get();
    answer_and_take_the_shot(&mut card, &mut peer, &next, 8);
    card.stop();
}

/// An event that names a player the card does not hold asks for the state,
/// with no `match.started` before it.
#[test]
fn an_event_naming_an_unknown_player_asks_for_the_state() {
    let (mut card, mut peer, _tx, _mock) = rig();
    first_match_with_a_shot(&mut card, &mut peer);
    assert!(
        !peer.asks_for_state(),
        "nothing is asked while nothing is wanted"
    );
    peer.event(
        "turn.changed",
        5,
        &json!({ "previousPlayerId": null, "currentPlayerId": 1, "holeNumber": 1,
                 "reason": "next_player" }),
    );
    let _ = peer.state_get();
    card.stop();
}

/// A linked golfer who is not the start golfer stays that golfer when the
/// connection ends: Sam's own id, right-handed, not Eric.
#[test]
fn a_closed_connection_returns_a_linked_golfer_to_that_golfer() {
    let (mut card, peer, _tx, _mock) = rig_linked();
    card.turn_until(is_joe);
    card.settle();
    let before = card.turns().len();
    drop(peer);
    let back = card.turn_after(before);
    assert_eq!(golfer(&back), Some("flighthook.sam"));
    assert_eq!(back.handedness, Some(Handedness::Right));
    assert_eq!(back.club, Some(Club::Iron7));
    assert_eq!(back.mode, Some(Mode::Full));
    card.stop();
}

/// What the card published from bus message `from` on.
fn published_since(card: &Card, from: usize) -> Vec<FrpBody> {
    card.raw[from..]
        .iter()
        .filter_map(BusMsg::as_frp)
        .filter(|m| m.from == ACTOR)
        .map(|m| m.body.clone())
        .collect()
}

/// A stroke after its round ended is dropped: it would be under an id the
/// player never had.
#[test]
fn a_shot_completed_after_the_round_ended_sends_no_stroke() {
    let (mut card, mut peer, _tx, _mock) = rig();
    first_match_with_a_shot(&mut card, &mut peer);
    let from = card.raw.len();
    peer.event("shot.completed", 5, &completed("pp-late", 1, "InPlay"));
    card.settle();
    card.settle();
    let own = published_since(&card, from);
    assert!(
        own.iter().all(|b| !matches!(
            b,
            FrpBody::GameStroke(_) | FrpBody::GolferProfile(_) | FrpBody::GameRound(_)
        )),
        "{own:?}"
    );
    card.stop();
}

/// A reply to `club.set` that lands after the player left publishes no
/// golfer for them: a player the card no longer holds is not made up.
#[test]
fn a_late_club_set_reply_for_a_player_who_left_publishes_no_golfer() {
    let (mut card, mut peer, tx, _mock) = rig();
    card.turn_until(is_joe);
    card.settle();
    send_select(&tx, &card.state.flighthook_id(), {
        let mut pick = flightrelay::GolferSelect::new("ui-1");
        pick.club = Some(Some(Club::Iron8));
        pick
    });
    let cmd = loop {
        let msg = peer.recv();
        if msg["command"] == "club.set" {
            break msg;
        }
    };
    peer.event("player.left", 2, &json!({ "playerId": 1 }));
    card.frp_until(|m| {
        m.from == ACTOR
            && matches!(&m.body, FrpBody::Removed(r)
                if r.kind == StateKind::GolferProfile && r.id.as_deref() == Some("pureplay.Joe"))
    });
    card.settle();
    let from = card.raw.len();
    peer.send(&json!({ "type": "response", "id": cmd["id"], "ok": true,
                       "data": { "playerId": 1, "club": "Iron8", "clubId": "b-8i" } }));
    // A reply publishes nothing by itself: the next event shows what the
    // card holds.
    peer.event(
        "hole.started",
        3,
        &json!({ "matchId": "m-1", "holeNumber": 8, "par": 4, "playerIds": [2, 3] }),
    );
    card.settle();
    card.settle();
    let own = published_since(&card, from);
    assert!(
        own.iter().all(|b| !matches!(b, FrpBody::GolferProfile(_))),
        "{own:?}"
    );
    card.stop();
}

/// A reply to `club.set` that lands after the round ended publishes no
/// golfer: the players of an ended round are not made up again.
#[test]
fn a_late_club_set_reply_after_the_round_ended_publishes_no_golfer() {
    let (mut card, mut peer, tx, _mock) = rig();
    card.turn_until(is_joe);
    card.settle();
    send_select(&tx, &card.state.flighthook_id(), {
        let mut pick = flightrelay::GolferSelect::new("ui-1");
        pick.club = Some(Some(Club::Iron8));
        pick
    });
    let cmd = loop {
        let msg = peer.recv();
        if msg["command"] == "club.set" {
            break msg;
        }
    };
    peer.event(
        "match.ended",
        2,
        &json!({ "matchId": "m-1", "reason": "completed", "standings": [] }),
    );
    card.frp_until(|m| {
        m.from == ACTOR && matches!(&m.body, FrpBody::Removed(r) if r.kind == StateKind::GameRound)
    });
    card.settle();
    let from = card.raw.len();
    peer.send(&json!({ "type": "response", "id": cmd["id"], "ok": true,
                       "data": { "playerId": 1, "club": "Iron8", "clubId": "b-8i" } }));
    peer.event(
        "hole.started",
        3,
        &json!({ "matchId": "m-1", "holeNumber": 8, "par": 4, "playerIds": [] }),
    );
    card.settle();
    card.settle();
    let own = published_since(&card, from);
    assert!(
        own.iter().all(|b| !matches!(b, FrpBody::GolferProfile(_))),
        "{own:?}"
    );
    card.stop();
}

/// `player.changed` with the fields it does not report sent as `null`
/// (Joe chipping with a sand wedge changes hands): the club and the mode
/// stay, the hand changes.
#[test]
fn a_hand_change_with_null_club_fields_keeps_the_club_and_the_mode() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.turn_until(is_joe);
    peer.event(
        "shot.initialized",
        2,
        &json!({ "shotId": "s-2", "matchId": "m-1", "playerId": 1, "holeNumber": 7,
                 "strokeNumber": 2, "ballLocation": [1.0, 2.0, 3.0], "club": "SandWedge",
                 "clubId": "b-sw", "distanceToHoleM": 15.0, "replacesShotId": null }),
    );
    card.turn_until(|t| t.mode == Some(Mode::Chipping) && t.club == Some(Club::SandWedge));
    peer.event(
        "player.changed",
        3,
        &json!({ "playerId": 1, "reason": "handedness_changed",
                 "current": { "playerId": 1, "profileId": null, "name": "Joe", "isLocal": true,
                              "handedness": "LH", "teamIndex": null, "handicap": null,
                              "club": null, "clubId": null, "bag": null, "ball": null,
                              "aim": null, "distanceToHoleM": null, "totalStrokes": null,
                              "toPar": null } }),
    );
    let turn = card.turn_until(|t| t.handedness == Some(Handedness::Left));
    assert_eq!(turn.club, Some(Club::SandWedge));
    assert_eq!(turn.mode, Some(Mode::Chipping));
    card.settle();
    let joe = card
        .own_bodies()
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
    card.stop();
}

/// The results screen after a round (`matchActive` false for the match that
/// just ended) brings no players and no shooter: no game golfer goes back
/// on the turn.
#[test]
fn the_results_screen_puts_no_game_golfer_back_on_the_turn() {
    let (mut card, mut peer, _tx, _mock) = rig();
    first_match_with_a_shot(&mut card, &mut peer);
    let (from, selects, turns) = (card.raw.len(), card.selects(), card.turns().len());
    peer.event(
        "player.club_changed",
        5,
        &json!({ "playerId": 1, "current": "Driver", "currentClubId": "b-dr", "reason": "user_selected" }),
    );
    let get = peer.state_get();
    let mut results = round();
    results["session"]["matchActive"] = json!(false);
    peer.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 6,
                       "requestId": get["id"], "data": results }),
    );
    card.settle();
    card.settle();
    let own = published_since(&card, from);
    assert!(
        own.iter().all(|b| !matches!(b, FrpBody::GolferProfile(_))),
        "{own:?}"
    );
    assert_eq!(card.selects(), selects, "no select");
    assert!(
        card.turns()[turns..]
            .iter()
            .all(|t| golfer(t) != Some("pureplay.Joe")),
        "{:?}",
        &card.turns()[turns..]
    );
    card.stop();
}

/// A reconnect straight into the results screen (the same match, no
/// longer active, its `match.ended` lost with the connection) brings no
/// players and no shooter: no select, and the turn does not go back to the
/// game's player.
#[test]
fn a_reconnect_into_the_results_screen_leaves_the_turn_alone() {
    let (mut card, peer, _tx, mock) = rig();
    card.turn_until(is_joe);
    card.settle();
    let selects = card.selects();
    drop(peer);
    card.shows(ActorStatus::Reconnecting, |_| true);
    let turns = card.turns().len();
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some("PPA-SHOTS"));
    peer.paired("PPA-SHOTS");
    let mut results = round();
    results["session"]["matchActive"] = json!(false);
    peer.subscribe(&results, 1);
    card.shows(ActorStatus::Connected, |_| true);
    card.settle();
    card.settle();
    assert_eq!(card.selects(), selects, "no select");
    assert!(
        card.turns()[turns..]
            .iter()
            .all(|t| golfer(t) != Some("pureplay.Joe")),
        "{:?}",
        &card.turns()[turns..]
    );
    card.stop();
}

/// A player the last snapshot did not bring is asked for once: a stream of
/// events naming them sends one `state.get` until a new match, a context
/// change, a join or a reconnect.
#[test]
fn a_player_the_snapshot_did_not_bring_is_asked_for_once() {
    let (mut card, mut peer, _tx, _mock) = rig();
    card.turn_until(is_joe);
    card.settle();
    let names_nine = |peer: &mut Peer, seq: u64| {
        peer.event(
            "player.club_changed",
            seq,
            &json!({ "playerId": 9, "current": "Driver", "currentClubId": "x", "reason": "user_selected" }),
        );
    };
    names_nine(&mut peer, 2);
    let get = peer.state_get();
    peer.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 3,
                       "requestId": get["id"], "data": round() }),
    );
    for seq in 4..14 {
        names_nine(&mut peer, seq);
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(!peer.asks_for_state(), "asked for player 9 again");
    // A context change clears it: once its own request is answered, the
    // next event naming player 9 asks for them again.
    peer.event("context.invalidated", 14, &json!({ "reason": "loading" }));
    let get = peer.state_get();
    peer.send(
        &json!({ "type": "event", "event": "state.snapshot", "seq": 15,
                       "requestId": get["id"], "data": round() }),
    );
    std::thread::sleep(Duration::from_millis(200));
    names_nine(&mut peer, 16);
    assert!(
        peer.asks_for_state(),
        "player 9 is asked for again after the context change"
    );
    card.stop();
}
