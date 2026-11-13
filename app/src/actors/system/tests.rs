//! Tests of the system actor: config reload, restarts, reconnects, the session, practice and shot-type reset.

use super::*;
use crate::actors::web::tests::TempDir;
use flighthook::{ConfigAction, ShotGrouping};
use flightrelay::{BusSync, Club, FrpMessage, GolferSelect, Handedness, Mode};

/// What the tests read of a bus message: the card (a local event's
/// card, or an FRP message's sender) and the local event. FRP messages
/// carry no local event and are left out by [`local_of`].
#[derive(Debug, Clone)]
struct Seen {
    actor: String,
    event: LocalEvent,
}

/// The local event of `msg` with its card; `None` for an FRP message.
fn local_of(msg: BusMsg) -> Option<Seen> {
    match msg {
        BusMsg::Local { card, event } => Some(Seen { actor: card, event }),
        BusMsg::Frp { .. } => None,
    }
}

/// Game state, its writer and the system actor's sender, with a
/// receiver for what the actor emits.
fn game() -> (
    TempDir,
    SystemState,
    SessionWriter,
    BusSender,
    broadcast::Receiver<BusMsg>,
) {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-system-test-{}",
        crate::state::config::generate_id()
    )));
    let (state, writer) = SystemState::new(dir.0.join("config.toml"));
    let (bus_tx, rx) = broadcast::channel(16);
    let sender = BusSender::new("system".into(), bus_tx, Arc::default())
        .with_frp_id(Some(state.flighthook_id()));
    (dir, state, writer, sender, rx)
}

/// The FRP messages on `rx`.
fn emitted_frp(rx: &mut broadcast::Receiver<BusMsg>) -> Vec<BusMsg> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|m| m.as_frp().is_some())
        .collect()
}

/// The practice actor through the bus: it follows the session's turn,
/// answers `practice.new_block` after publishing the blocks it changed,
/// republishes its open blocks on `bus.sync`, and the flighthook actor
/// claims the role.
#[test]
fn the_practice_actor_follows_the_turn_and_answers_new_block_after_its_blocks() {
    let (_dir, state, writer, sender, mut rx) = game();
    let session = state.flighthook_id();
    let (mut practice, first) = Practice::start(writer.turn(), 0);
    publish_blocks(first, &sender);
    publish_session(&writer, &sender);
    let out = emitted_frp(&mut rx);
    let first = out
        .iter()
        .find_map(|m| match &m.as_frp()?.body {
            FrpBody::PracticeBlock(b) => Some(b.clone()),
            _ => None,
        })
        .expect("first block");
    let actor = out
        .iter()
        .find_map(|m| match &m.as_frp()?.body {
            FrpBody::BusActor(a) => Some(a.clone()),
            _ => None,
        })
        .expect("bus.actor");
    assert!(
        actor.roles.iter().any(|r| r == roles::PRACTICE),
        "{actor:?}"
    );

    // A club change: the session's turn, then the empty block in place.
    let msg = BusMsg::frp(FrpMessage::new(
        "web",
        select(Some(Club::Iron7), None, None),
    ));
    handle_session(&msg, &writer, &sender);
    for m in emitted_frp(&mut rx) {
        handle_practice(&m, &mut practice, &sender);
    }
    let out = emitted_frp(&mut rx);
    assert_eq!(out.len(), 1, "{out:?}");
    let FrpBody::PracticeBlock(moved) = &out[0].as_frp().unwrap().body else {
        panic!("{out:?}");
    };
    assert_eq!(
        (moved.id.as_str(), moved.club),
        (first.id.as_str(), Some(Club::Iron7))
    );
    assert_eq!(out[0].as_frp().unwrap().from, session);

    // A shot, then New set: the old block's removal, the new block, then
    // the reply to the sender.
    let trigger = BusMsg::frp(FrpMessage::new(
        "mevo",
        flightrelay::ShotTrigger {
            key: flightrelay::ShotKey {
                shot_id: "s1".into(),
                shot_number: 1,
            },
            timestamp: None,
        },
    ));
    handle_practice(&trigger, &mut practice, &sender);
    let command = flightrelay::PracticeNewBlock {
        r#ref: "nb-1".into(),
        golfer: None,
        new_session: false,
    };
    handle_practice(
        &BusMsg::frp(FrpMessage::new("web", command)),
        &mut practice,
        &sender,
    );
    let out = emitted_frp(&mut rx);
    let kinds: Vec<&str> = out
        .iter()
        .map(|m| m.as_frp().unwrap().body.kind())
        .collect();
    assert_eq!(
        kinds,
        ["practice.block", "practice.block", "bus.reply"],
        "{out:?}"
    );
    assert!(matches!(
        &out[0].as_frp().unwrap().body,
        FrpBody::Removed(_)
    ));
    let FrpBody::BusReply(reply) = &out[2].as_frp().unwrap().body else {
        panic!("{out:?}");
    };
    assert_eq!((reply.to.as_str(), reply.r#ref.as_str()), ("web", "nb-1"));
    assert!(reply.error.is_none());

    // bus.sync: the open blocks again.
    handle_practice(
        &BusMsg::frp(FrpMessage::new("peer", BusSync::default())),
        &mut practice,
        &sender,
    );
    let out = emitted_frp(&mut rx);
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(matches!(&out[0].as_frp().unwrap().body, FrpBody::PracticeBlock(b) if b.number == 2));
}

/// A golfers save that links the game id up with an open Set to
/// another golfer: the system actor closes that golfer's own open Set,
/// so one open block per golfer is left for the store to match.
#[test]
fn a_golfers_save_keys_the_open_blocks_again() {
    let (_dir, state, writer, sender, mut rx) = game();
    state.system.update(|c| {
        c.golfers = vec![
            flighthook::GolferRecord::new("flighthook.e", "Eric"),
            flighthook::GolferRecord::new("flighthook.s", "Sam"),
        ];
    });
    let mut profiles = Profiles::default();
    follow_golfers(&state, &writer, &mut profiles, &sender);
    let (mut practice, _) = Practice::with_config(state.system.view(), writer.turn(), 0);
    let eric = practice.open_blocks()[0].clone();
    assert_eq!(
        eric.golfer.as_ref().map(flightrelay::GolferId::as_str),
        Some("flighthook.e")
    );
    practice.trigger("s1", "lm", 0);
    let game = flightrelay::GolferSelect {
        golfer: Some(Some("pureplay.local-3".parse().expect("id"))),
        club: Some(Some(Club::Iron7)),
        ..flightrelay::GolferSelect::new("game")
    };
    let turn = writer.apply(&game).expect("turn");
    practice.turn(turn, 1);
    practice.trigger("s2", "lm", 1);
    emitted_frp(&mut rx);
    state.system.update(|c| {
        c.golfers[0].identifiers.push("pureplay.local-3".into());
    });
    after_config_command(&state, &writer, &mut profiles, &mut practice, &sender);
    let removed: Vec<String> = emitted_frp(&mut rx)
        .iter()
        .filter_map(|m| match &m.as_frp()?.body {
            FrpBody::Removed(r) if r.kind == flightrelay::StateKind::PracticeBlock => r.id.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(removed, vec![eric.id.clone()]);
    assert_eq!(practice.open_blocks().len(), 1);
}

/// A card's status, as the card publishes it.
fn card_status(card: &str, status: ActorStatus) -> BusMsg {
    BusMsg::Local {
        card: card.into(),
        event: LocalEvent::CardStatus {
            status,
            telemetry: HashMap::new(),
        },
    }
}

/// Feed `msg` to `reset` as the system actor's loop does.
fn feed(
    reset: &mut ShotTypeReset,
    msg: &BusMsg,
    state: &SystemState,
    writer: &SessionWriter,
    sender: &BusSender,
) {
    if reset.observe(msg) {
        reset.check(state, writer, sender);
    }
}

/// A putt left over from a card that has gone goes back to full, once,
/// when the last thing the Shot Type picker steers goes; not at
/// start-up, not when the picker was already hidden, and nothing is
/// published for a mode that is already full.
#[test]
fn the_shot_type_resets_to_full_once_when_the_picker_goes() {
    let (_dir, state, writer, sender, mut rx) = game();
    let own = state.flighthook_id();
    let putt = select(None, None, Some(Mode::Putting));
    let mode = |state: &SystemState| state.session.turn().mode;
    let mevo = |state: &SystemState, on: bool| {
        state.system.update(|c| {
            c.mevo.clear();
            if on {
                c.mevo
                    .insert("0".into(), flighthook::MevoSection::default());
            }
        });
    };
    // Start-up with the picker hidden: nothing, then or on a command.
    writer.apply(&putt);
    let mut reset = ShotTypeReset::new(&state.system.snapshot());
    reset.check(&state, &writer, &sender);
    assert_eq!(mode(&state), Some(Mode::Putting));
    emitted_frp(&mut rx);

    // The picker shows: nothing.
    mevo(&state, true);
    reset.check(&state, &writer, &sender);
    assert_eq!(mode(&state), Some(Mode::Putting));

    // Its card goes: full, published once, caused by flighthook.
    mevo(&state, false);
    reset.check(&state, &writer, &sender);
    assert_eq!(mode(&state), Some(Mode::Full));
    let out = emitted_frp(&mut rx);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].turn_of(&own).expect("turn").1, Some(own.as_str()));
    // The same state again publishes nothing.
    reset.check(&state, &writer, &sender);
    assert!(emitted_frp(&mut rx).is_empty());
}

/// A GSPro card with the default club lists that
/// is not connected does not hold the mode. Putt, then the launch
/// monitors that take the hint are disabled: full, once.
#[test]
fn a_simulator_card_that_is_not_connected_does_not_keep_the_mode() {
    let (_dir, state, writer, sender, mut rx) = game();
    state.system.update(|c| {
        c.openconnect_client
            .insert("0".into(), flighthook::OpenConnectClientSection::default());
        c.mevo
            .insert("0".into(), flighthook::MevoSection::default());
    });
    let mut reset = ShotTypeReset::new(&state.system.snapshot());
    writer.apply(&select(None, None, Some(Mode::Putting)));
    emitted_frp(&mut rx);
    state.system.update(|c| {
        for s in c.mevo.values_mut() {
            s.enabled = false;
        }
    });
    reset.check(&state, &writer, &sender);
    assert_eq!(state.session.turn().mode, Some(Mode::Full));
    assert_eq!(emitted_frp(&mut rx).len(), 1);
    reset.check(&state, &writer, &sender);
    assert!(emitted_frp(&mut rx).is_empty(), "once");
}

/// A connected card that sets the shot type holds it: no reset while it
/// is connected; when it disconnects, or its card is removed, the mode
/// goes back to full, once.
#[test]
fn a_connected_card_holds_the_mode_until_it_goes() {
    let (_dir, state, writer, sender, mut rx) = game();
    let putt = select(None, None, Some(Mode::Putting));
    let mode = |state: &SystemState| state.session.turn().mode;
    let card = "pureplay_accessory.0";
    state.system.update(|c| {
        c.pureplay_accessory
            .insert("0".into(), flighthook::PureplayAccessorySection::default());
    });
    let mut reset = ShotTypeReset::new(&state.system.snapshot());
    feed(
        &mut reset,
        &card_status(card, ActorStatus::Connected),
        &state,
        &writer,
        &sender,
    );
    writer.apply(&putt);
    emitted_frp(&mut rx);

    // Connected and holding: a command resets nothing.
    reset.check(&state, &writer, &sender);
    assert_eq!(mode(&state), Some(Mode::Putting));

    // It disconnects: full, once.
    feed(
        &mut reset,
        &card_status(card, ActorStatus::Disconnected),
        &state,
        &writer,
        &sender,
    );
    assert_eq!(mode(&state), Some(Mode::Full));
    assert_eq!(emitted_frp(&mut rx).len(), 1);
    feed(
        &mut reset,
        &card_status(card, ActorStatus::Reconnecting),
        &state,
        &writer,
        &sender,
    );
    assert!(emitted_frp(&mut rx).is_empty(), "once");

    // Connected again and the game sets Putt; the card is removed: full,
    // once.
    feed(
        &mut reset,
        &card_status(card, ActorStatus::Connected),
        &state,
        &writer,
        &sender,
    );
    writer.apply(&putt);
    emitted_frp(&mut rx);
    state.system.update(|c| c.pureplay_accessory.clear());
    reset.check(&state, &writer, &sender);
    assert_eq!(mode(&state), Some(Mode::Full));
    assert_eq!(emitted_frp(&mut rx).len(), 1);
}

/// A card turned off while connected leaves the connected set: when it
/// is turned on again, the mode a select set meanwhile is not reset a
/// second time. The same steps with a card that never connected leave
/// the mode alone too.
#[test]
fn a_card_turned_off_while_connected_does_not_reset_the_mode_again_when_turned_on() {
    for connects in [true, false] {
        let (_dir, state, writer, sender, mut rx) = game();
        let mode = |state: &SystemState| state.session.turn().mode;
        let card = "openconnect_client.0";
        let switch = |state: &SystemState, on: bool| {
            state.system.update(|c| {
                for s in c.openconnect_client.values_mut() {
                    s.enabled = on;
                }
            });
        };
        state.system.update(|c| {
            c.openconnect_client
                .insert("0".into(), flighthook::OpenConnectClientSection::default());
        });
        let mut reset = ShotTypeReset::new(&state.system.snapshot());
        if connects {
            feed(
                &mut reset,
                &card_status(card, ActorStatus::Connected),
                &state,
                &writer,
                &sender,
            );
        }
        writer.apply(&select(None, None, Some(Mode::Putting)));
        emitted_frp(&mut rx);

        // Turned off (no last status): full, once, if it held the mode.
        switch(&state, false);
        reset.check(&state, &writer, &sender);
        if connects {
            assert_eq!(mode(&state), Some(Mode::Full));
            assert_eq!(emitted_frp(&mut rx).len(), 1);
        } else {
            assert_eq!(mode(&state), Some(Mode::Putting));
        }

        // Something sets Putt while nothing holds it.
        writer.apply(&select(None, None, Some(Mode::Putting)));
        emitted_frp(&mut rx);

        // Turned on again, the simulator not running: the command, the
        // card's Starting, then its Disconnected leave Putt alone.
        switch(&state, true);
        reset.check(&state, &writer, &sender);
        for status in [ActorStatus::Starting, ActorStatus::Disconnected] {
            feed(
                &mut reset,
                &card_status(card, status),
                &state,
                &writer,
                &sender,
            );
        }
        assert_eq!(mode(&state), Some(Mode::Putting), "connects: {connects}");
        assert!(emitted_frp(&mut rx).is_empty(), "connects: {connects}");
    }
}

fn select(club: Option<Club>, handedness: Option<Handedness>, mode: Option<Mode>) -> GolferSelect {
    GolferSelect {
        club: club.map(Some),
        handedness,
        mode,
        ..GolferSelect::new("r1")
    }
}

/// A select that changes the turn publishes the new turn, caused by the
/// select's sender, and then answers with `bus.reply`, so the requester
/// holds the new turn when it reads the reply; the same select again is
/// answered but publishes no turn.
#[test]
fn a_select_publishes_the_turn_before_its_reply_and_once() {
    let (_dir, state, writer, sender, mut rx) = game();
    let session = state.flighthook_id();
    let msg = BusMsg::frp(FrpMessage::new(
        "web",
        select(None, Some(Handedness::Left), None),
    ));
    handle_session(&msg, &writer, &sender);
    let out = emitted_frp(&mut rx);
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(
        matches!(&out[1].as_frp().unwrap().body, FrpBody::BusReply(_)),
        "{out:?}"
    );
    let (turn, cause) = out[0].turn_of(&session).expect("turn");
    assert_eq!(turn.handedness, Some(Handedness::Left));
    assert_eq!(cause, Some("web"));
    assert_eq!(state.session.turn().handedness, Some(Handedness::Left));

    // The same hand again: answered, no turn.
    handle_session(&msg, &writer, &sender);
    let out = emitted_frp(&mut rx);
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].turn_of(&session).is_none(), "{out:?}");
}

/// The club alone never changes the mode or the hand.
#[test]
fn a_club_change_leaves_the_hand_and_the_mode_alone() {
    let (_dir, state, writer, sender, mut rx) = game();
    let session = state.flighthook_id();
    handle_session(
        &BusMsg::frp(FrpMessage::new(
            "web",
            select(None, Some(Handedness::Left), None),
        )),
        &writer,
        &sender,
    );
    emitted_frp(&mut rx);
    handle_session(
        &BusMsg::frp(FrpMessage::new(
            "web",
            select(Some(Club::Putter), None, None),
        )),
        &writer,
        &sender,
    );
    let turn = state.session.turn();
    assert_eq!(turn.handedness, Some(Handedness::Left));
    assert_eq!(turn.mode, Some(Mode::Full));
    assert_eq!(turn.club, Some(Club::Putter));
    let out = emitted_frp(&mut rx);
    let (turn, _) = out.iter().find_map(|m| m.turn_of(&session)).expect("turn");
    assert_eq!(turn.club, Some(Club::Putter));
    assert_eq!(turn.mode, Some(Mode::Full));
}

/// The session's own turn coming back round changes nothing; a
/// `bus.sync` republishes the session's actor and turn.
#[test]
fn its_own_turn_changes_nothing_and_a_sync_republishes_it() {
    let (_dir, state, writer, sender, mut rx) = game();
    let session = state.flighthook_id();
    handle_session(
        &BusMsg::frp(FrpMessage::new(
            "web",
            select(None, None, Some(Mode::Chipping)),
        )),
        &writer,
        &sender,
    );
    let out = emitted_frp(&mut rx);
    let own = out
        .iter()
        .find(|m| m.turn_of(&session).is_some())
        .expect("turn")
        .clone();
    handle_session(&own, &writer, &sender);
    assert!(emitted_frp(&mut rx).is_empty());
    assert_eq!(state.session.turn().mode, Some(Mode::Chipping));

    handle_session(
        &BusMsg::frp(FrpMessage::new("web", BusSync::default())),
        &writer,
        &sender,
    );
    let out = emitted_frp(&mut rx);
    assert!(
        out.iter()
            .any(|m| matches!(&m.as_frp().unwrap().body, FrpBody::BusActor(_))),
        "{out:?}"
    );
    let (turn, _) = out.iter().find_map(|m| m.turn_of(&session)).expect("turn");
    assert_eq!(turn.mode, Some(Mode::Chipping));
}

/// A turn republished on `bus.sync` keeps the cause of the select that
/// produced it.
#[test]
fn a_sync_republishes_the_turn_with_the_cause_of_its_select() {
    let (_dir, state, writer, sender, mut rx) = game();
    let session = state.flighthook_id();
    handle_session(
        &BusMsg::frp(FrpMessage::new(
            "gspro-1",
            select(Some(Club::Iron7), None, None),
        )),
        &writer,
        &sender,
    );
    emitted_frp(&mut rx);
    handle_session(
        &BusMsg::frp(FrpMessage::new("web", BusSync::default())),
        &writer,
        &sender,
    );
    let out = emitted_frp(&mut rx);
    let (turn, cause) = out.iter().find_map(|m| m.turn_of(&session)).expect("turn");
    assert_eq!(turn.club, Some(Club::Iron7));
    assert_eq!(cause, Some("gspro-1"));

    // A select that changes nothing leaves the cause alone.
    handle_session(
        &BusMsg::frp(FrpMessage::new(
            "web",
            select(Some(Club::Iron7), None, None),
        )),
        &writer,
        &sender,
    );
    emitted_frp(&mut rx);
    publish_session(&writer, &sender);
    let out = emitted_frp(&mut rx);
    let (_, cause) = out.iter().find_map(|m| m.turn_of(&session)).expect("turn");
    assert_eq!(cause, Some("gspro-1"));
}

/// The running actor, fed through the bus: a hand sent three times
/// produces one turn, and its own turn does not loop.
#[test]
fn the_running_actor_publishes_a_hand_change_once() {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-system-test-{}",
        crate::state::config::generate_id()
    )));
    let (state, writer) = SystemState::new(dir.0.join("config.toml"));
    let state = Arc::new(state);
    let session = state.flighthook_id();
    let (bus_tx, mut rx) = broadcast::channel(64);
    let shutdown = Arc::new(AtomicBool::new(false));
    start_for_test(writer, &state, &bus_tx, Arc::clone(&shutdown));
    let web = BusSender::new("web".into(), bus_tx.clone(), Arc::default())
        .with_frp_id(Some("web".into()));
    for _ in 0..3 {
        web.frp(select(None, Some(Handedness::Left), None));
    }
    // The actor handles the bus in order, so the turn for this club
    // comes after everything it does for the three repeats.
    web.frp(select(Some(Club::Putter), None, None));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut turns = Vec::new();
    'wait: while std::time::Instant::now() < deadline {
        while let Ok(m) = rx.try_recv() {
            let Some((turn, cause)) = m.turn_of(&session) else {
                continue;
            };
            // The first turn at start is the session's own.
            if cause != Some("web") {
                continue;
            }
            let sentinel = turn.club == Some(Club::Putter);
            turns.push(turn.clone());
            if sentinel {
                break 'wait;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert_eq!(turns[0].handedness, Some(Handedness::Left));
    assert_eq!(turns[1].club, Some(Club::Putter));
    assert_eq!(turns[1].mode, Some(Mode::Full));
    assert_eq!(state.session.turn().handedness, Some(Handedness::Left));
}

/// The tournament toggle writes `[simulation] tournament` (absent from
/// the file while off), applies it before the outcome, and a restart
/// with it on starts in tournament mode.
#[test]
fn set_tournament_persists_the_key_and_a_restart_starts_in_tournament_mode() {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-system-test-{}",
        crate::state::config::generate_id()
    )));
    let path = dir.0.join("config.toml");
    let (state, _writer) = SystemState::new(path.clone());
    let state = Arc::new(state);
    assert!(!state.round.active());
    let (bus_tx, mut rx) = broadcast::channel(16);
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    let set = |on: bool| {
        run_config_command(
            &LocalEvent::ConfigCommand {
                request_id: Some("req-t".into()),
                action: Box::new(ConfigAction::SetTournament { on }),
            },
            &state,
            &bus_tx,
            &sender,
        );
    };

    set(true);
    assert!(state.system.simulation().tournament);
    let toml = std::fs::read_to_string(&path).expect("written");
    assert!(toml.contains("tournament = true"), "{toml}");
    assert!(state.round.state().manual && state.round.active());
    let outcome = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(local_of)
            .any(|m| matches!(m.event, LocalEvent::ConfigOutcome { ref request_id, .. } if request_id.as_deref() == Some("req-t")));
    assert!(outcome, "the outcome goes out");

    // A restart reads it: tournament mode from the start.
    let (again, _w) = SystemState::new(path.clone());
    assert_eq!(
        again.round.state(),
        flighthook::RoundState {
            active: true,
            holder: Some(crate::state::round::MANUAL_HOLDER.into()),
            manual: true,
            held: false,
        }
    );

    set(false);
    assert!(!state.round.active());
    let toml = std::fs::read_to_string(&path).expect("written");
    assert!(!toml.contains("tournament"), "absent while off: {toml}");
    let (off, _w) = SystemState::new(path);
    assert!(!off.round.active(), "a restart with it off starts off");
}

/// Only the toggle writes tournament mode: a `scope=global` save
/// (`ReplaceGlobal`) or a whole-config save (`ReplaceAll`) keeps the live
/// value whatever its body carries, in both directions.
#[test]
fn a_settings_save_never_changes_tournament_mode() {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-system-test-{}",
        crate::state::config::generate_id()
    )));
    let path = dir.0.join("config.toml");
    let (state, _writer) = SystemState::new(path.clone());
    let state = Arc::new(state);
    let (bus_tx, _rx) = broadcast::channel(64);
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    let run = |action: ConfigAction| {
        run_config_command(
            &LocalEvent::ConfigCommand {
                request_id: None,
                action: Box::new(action),
            },
            &state,
            &bus_tx,
            &sender,
        );
    };
    let saves = |body_says: bool| {
        let mut config = state.system.snapshot();
        config.simulation.tournament = body_says;
        // Something else in the save changes, as a real one does.
        config.simulation.temperature_c = if body_says { 10.0 } else { 20.0 };
        [
            ConfigAction::ReplaceGlobal {
                config: Box::new(config.clone()),
            },
            ConfigAction::ReplaceAll {
                config: Box::new(config),
            },
        ]
    };
    for live in [true, false] {
        run(ConfigAction::SetTournament { on: live });
        for action in saves(!live) {
            run(action);
            assert_eq!(state.system.simulation().tournament, live, "live {live}");
            assert_eq!(state.round.state().manual, live, "live {live}");
            let toml = std::fs::read_to_string(&path).expect("written");
            assert_eq!(toml.contains("tournament = true"), live, "{toml}");
        }
        let temperature = if live { 20.0 } else { 10.0 };
        assert!(
            (state.system.simulation().temperature_c - temperature).abs() < 1e-9,
            "the rest of the save is applied"
        );
    }
}

#[test]
fn set_shot_grouping_persists_and_emits_the_outcome() {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-system-test-{}",
        crate::state::config::generate_id()
    )));
    let path = dir.0.join("config.toml");
    let (state, _writer) = SystemState::new(path.clone());
    let state = Arc::new(state);
    // Configured but not running: a reconcile would start it.
    state.system.update(|c| {
        c.webserver.clear();
        c.mock_monitor.insert(
            "0".into(),
            flighthook::MockMonitorSection {
                actor_id: None,
                name: "Mock".into(),
                enabled: true,
                impact_lead_s: None,
                driver: Default::default(),
            },
        );
    });
    let running = state.actor_ids();
    let (bus_tx, mut rx) = broadcast::channel(16);
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());

    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some("req-1".into()),
            action: Box::new(ConfigAction::SetShotGrouping {
                mode: ShotGrouping::ByClub,
            }),
        },
        &state,
        &bus_tx,
        &sender,
    );

    assert_eq!(state.system.snapshot().shot_grouping, ShotGrouping::ByClub);
    let toml = std::fs::read_to_string(&path).expect("written");
    assert!(toml.contains(r#"shot_grouping = "by_club""#), "{toml}");
    let msg = local_of(rx.try_recv().expect("outcome")).expect("a local event");
    assert_eq!(msg.actor, "system");
    let LocalEvent::ConfigOutcome {
        request_id,
        restarted,
        stopped,
        started,
    } = msg.event
    else {
        panic!("not an outcome: {:?}", msg.event);
    };
    assert_eq!(request_id.as_deref(), Some("req-1"));
    assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
    // A view change reconciles no actor.
    assert_eq!(state.actor_ids(), running);
    assert!(!state.actor_ids().contains(&"mock_monitor.0".to_owned()));
}

#[test]
fn replace_global_keeps_every_actor_section() {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-system-test-{}",
        crate::state::config::generate_id()
    )));
    let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
    let state = Arc::new(state);
    // An address no actor can start on, so the reconcile starts nothing.
    let live_client = flighthook::OpenConnectClientSection {
        name: "Saved elsewhere".into(),
        enabled: true,
        address: Some("not an address".into()),
        full_monitor: None,
        chipping_monitor: None,
        putting_monitor: None,
        ..Default::default()
    };
    state.system.update(|c| {
        c.webserver.clear();
        c.openconnect_client.insert("0".into(), live_client.clone());
    });
    // A body from a stale snapshot: other units, no openconnect_client.0,
    // an openconnect_client.1.
    let mut body = state.system.snapshot();
    body.default_units = flighthook::UnitSystem::Metric;
    body.openconnect_client.clear();
    body.openconnect_client
        .insert("1".into(), live_client.clone());
    let (bus_tx, mut rx) = broadcast::channel(16);
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());

    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some("req-2".into()),
            action: Box::new(ConfigAction::ReplaceGlobal {
                config: Box::new(body),
            }),
        },
        &state,
        &bus_tx,
        &sender,
    );

    let saved = state.system.snapshot();
    assert_eq!(saved.default_units, flighthook::UnitSystem::Metric);
    assert_eq!(saved.openconnect_client.len(), 1);
    let mut saved_client = saved
        .openconnect_client
        .get("0")
        .expect("the live client")
        .clone();
    assert!(saved_client.actor_id.is_some(), "the save assigns an id");
    saved_client.actor_id = None;
    assert_eq!(saved_client, live_client);
    let outcome = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .find(|m| matches!(m.event, LocalEvent::ConfigOutcome { .. }));
    assert!(outcome.is_some(), "the outcome is emitted");
}
#[test]
fn replace_global_takes_the_replay_settings() {
    let (_dir, state, _writer, sender, _rx) = game();
    let state = Arc::new(state);
    let mut body = state.system.snapshot();
    body.replay.keep_shots = 3;
    body.replay.pre_roll_s = 2.0;
    body.replay.start_before_impact_s = 1.25;
    body.replay.clip_dir = Some("/srv/swings".into());
    body.replay.shot_cards = false;
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: None,
            action: Box::new(ConfigAction::ReplaceGlobal {
                config: Box::new(body.clone()),
            }),
        },
        &state,
        sender.raw_sender(),
        &sender,
    );
    assert_eq!(state.system.replay(), body.replay);
}

/// A new golfer saved twice without its id (the first answer lost)
/// keeps the id its first save gave it; a golfer with another name, or
/// one whose id the save still holds elsewhere, gets a new one.
#[test]
fn a_new_golfer_saved_twice_without_its_id_keeps_its_first_id() {
    let (_dir, state, _writer, sender, _rx) = game();
    let state = Arc::new(state);
    let save = |golfers: Vec<flighthook::GolferRecord>| {
        run_config_command(
            &LocalEvent::ConfigCommand {
                request_id: None,
                action: Box::new(ConfigAction::SetGolfers { golfers }),
            },
            &state,
            sender.raw_sender(),
            &sender,
        );
        state.system.snapshot().golfers
    };
    let new = |name: &str| flighthook::GolferRecord {
        name: name.into(),
        ..flighthook::GolferRecord::default()
    };
    let first = save(vec![new("Eric")]);
    let id = first[0].own_id().expect("an id").to_owned();
    let again = save(vec![new("Eric")]);
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].own_id(), Some(id.as_str()), "{again:?}");
    // Another name at that place is another golfer.
    let other = save(vec![new("Sam")]);
    assert_ne!(other[0].own_id(), Some(id.as_str()));
    // A golfer that still holds the saved id keeps it; the id-less one
    // beside it gets its own.
    let sam = other[0].clone();
    let two = save(vec![new("Sam"), sam.clone()]);
    assert_eq!(two[1], sam);
    assert_ne!(two[0].own_id(), sam.own_id());
    assert!(two[0].own_id().is_some());
}

#[test]
fn set_golfers_takes_the_golfers_and_replace_global_leaves_them() {
    let (_dir, state, _writer, sender, _rx) = game();
    let state = Arc::new(state);
    let golfers = vec![flighthook::GolferRecord {
        height: Some(190.0),
        ..flighthook::GolferRecord::new("flighthook.a", "Eric")
    }];
    let mut rx = sender.subscribe();
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some("req-golfers".into()),
            action: Box::new(ConfigAction::SetGolfers {
                golfers: golfers.clone(),
            }),
        },
        &state,
        sender.raw_sender(),
        &sender,
    );
    assert_eq!(state.system.snapshot().golfers, golfers, "persisted");
    assert_eq!(
        state.system.golfer_facts(Some("flighthook.a")).height,
        Some(190.0)
    );
    let outcome = std::iter::from_fn(|| rx.poll().ok().flatten())
        .filter_map(local_of)
        .find_map(|m| match m.event {
            LocalEvent::ConfigOutcome {
                request_id,
                restarted,
                ..
            } => Some((request_id, restarted)),
            _ => None,
        });
    assert_eq!(outcome, Some((Some("req-golfers".into()), Vec::new())));
    // The General cards' save leaves the golfers alone.
    let mut body = state.system.snapshot();
    body.golfers.clear();
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: None,
            action: Box::new(ConfigAction::ReplaceGlobal {
                config: Box::new(body),
            }),
        },
        &state,
        sender.raw_sender(),
        &sender,
    );
    assert_eq!(state.system.snapshot().golfers, golfers);
}

/// The flighthook actor publishes each golfer's profile under its own
/// id, never a game id, on a change only, and `removed` for a golfer
/// taken out; the session follows: the first golfer comes up with its
/// hand.
#[test]
fn golfer_profiles_are_published_and_the_first_golfer_comes_up() {
    let (_dir, state, writer, sender, mut rx) = game();
    let mut profiles = Profiles::default();
    follow_golfers(&state, &writer, &mut profiles, &sender);
    assert!(emitted_frp(&mut rx).is_empty(), "no golfers, nothing");
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.identifiers.push("pureplay.local-3".into());
    eric.handed = Some(Handedness::Left);
    eric.bag = vec![Club::Driver, Club::Iron7];
    eric.height = Some(182.0);
    let sam = flighthook::GolferRecord::new("flighthook.s", "Sam");
    state
        .system
        .update(|c| c.golfers = vec![eric.clone(), sam.clone()]);
    follow_golfers(&state, &writer, &mut profiles, &sender);
    let out = emitted_frp(&mut rx);
    let published: Vec<GolferProfile> = out
        .iter()
        .filter_map(|m| match &m.as_frp()?.body {
            FrpBody::GolferProfile(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(published.len(), 2, "{out:?}");
    let e = &published[0];
    assert_eq!(e.id.as_str(), "flighthook.e");
    assert_eq!(e.name, "Eric");
    assert_eq!(e.handedness, Some(Handedness::Left));
    assert_eq!(e.bag, [Club::Driver, Club::Iron7]);
    assert_eq!(
        serde_json::to_value(e.height).unwrap(),
        serde_json::json!("182cm")
    );
    assert!(
        !format!("{out:?}").contains("local-3"),
        "game ids stay off the wire"
    );
    let (turn, cause) = out
        .iter()
        .find_map(|m| m.turn_of(&state.flighthook_id()))
        .expect("turn");
    assert_eq!(
        turn.golfer.as_ref().map(|g| g.as_str()),
        Some("flighthook.e")
    );
    assert_eq!(turn.handedness, Some(Handedness::Left));
    assert_eq!(cause, Some(state.flighthook_id().as_str()));
    // Nothing changed: nothing published.
    follow_golfers(&state, &writer, &mut profiles, &sender);
    assert!(emitted_frp(&mut rx).is_empty());
    // Sam removed: his removal.
    state.system.update(|c| c.golfers.truncate(1));
    follow_golfers(&state, &writer, &mut profiles, &sender);
    let out = emitted_frp(&mut rx);
    assert!(
        out.iter().any(|m| matches!(&m.as_frp().unwrap().body,
                FrpBody::Removed(r) if r.kind == flightrelay::StateKind::GolferProfile)),
        "{out:?}"
    );
    // bus.sync: every profile again.
    profiles.publish_all(&sender);
    assert_eq!(emitted_frp(&mut rx).len(), 1);
}

/// A pick from flighthook itself saves the golfer as `selected_golfer`
/// and a hand on the golfer up; a select from another actor saves
/// nothing.
#[test]
fn own_picks_save_the_golfer_and_the_hand() {
    let (_dir, state, writer, sender, mut rx) = game();
    let own = state.flighthook_id();
    state.system.update(|c| {
        c.golfers = vec![
            flighthook::GolferRecord::new("flighthook.e", "Eric"),
            flighthook::GolferRecord::new("flighthook.s", "Sam"),
        ];
    });
    let mut profiles = Profiles::default();
    follow_golfers(&state, &writer, &mut profiles, &sender);
    emitted_frp(&mut rx);
    let pick = |from: &str, golfer: Option<&str>, hand: Option<Handedness>| {
        let msg = BusMsg::frp(FrpMessage::new(
            from,
            GolferSelect {
                golfer: golfer.map(|g| Some(g.parse().expect("id"))),
                handedness: hand,
                ..GolferSelect::new("r")
            },
        ));
        handle_session(&msg, &writer, &sender);
        save_own_pick(&msg, &state, &sender)
    };
    assert!(pick(&own, Some("flighthook.s"), None));
    assert_eq!(
        state.system.snapshot().selected_golfer.as_deref(),
        Some("flighthook.s")
    );
    assert!(pick(&own, None, Some(Handedness::Left)));
    assert_eq!(
        state.system.snapshot().golfers[1].handed,
        Some(Handedness::Left)
    );
    assert!(!pick(&own, None, Some(Handedness::Left)), "already saved");
    assert!(!pick(
        "gspro-1",
        Some("flighthook.e"),
        Some(Handedness::Right)
    ));
    let saved = state.system.snapshot();
    assert_eq!(saved.selected_golfer.as_deref(), Some("flighthook.s"));
    assert_eq!(saved.golfers[0].handed, None);
    // Saved to disk: a restart starts with Sam, left-handed.
    let path = std::path::PathBuf::from(&_dir.0).join("config.toml");
    let (again, _) = SystemState::new(path);
    let turn = again.session.turn();
    assert_eq!(
        turn.golfer.as_ref().map(|g| g.as_str()),
        Some("flighthook.s")
    );
    assert_eq!(turn.handedness, Some(Handedness::Left));
}

/// The turns among `out` from flighthook.
fn turns(state: &SystemState, out: &[BusMsg]) -> Vec<flightrelay::GolferTurn> {
    let own = state.flighthook_id();
    out.iter()
        .filter_map(|m| m.turn_of(&own).map(|(t, _)| t.clone()))
        .collect()
}

/// A hand saved on the Golfers card for the golfer up
/// reaches the turn at once; a save that leaves the hand alone keeps
/// a hand another actor picked.
#[test]
fn a_hand_saved_for_the_golfer_up_reaches_the_turn() {
    let (_dir, state, writer, sender, mut rx) = game();
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.handed = Some(Handedness::Left);
    eric.identifiers.push("pureplay.local-3".into());
    state.system.update(|c| {
        c.golfers = vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")];
    });
    let mut profiles = Profiles::default();
    follow_golfers(&state, &writer, &mut profiles, &sender);
    assert_eq!(writer.turn().handedness, Some(Handedness::Left));
    emitted_frp(&mut rx);
    state
        .system
        .update(|c| c.golfers[0].handed = Some(Handedness::Right));
    follow_golfers(&state, &writer, &mut profiles, &sender);
    let turns = turns(&state, &emitted_frp(&mut rx));
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert_eq!(turns[0].handedness, Some(Handedness::Right));
    // A peer picks left; a save of Sam's name leaves it.
    let msg = BusMsg::frp(FrpMessage::new(
        "gspro-1",
        GolferSelect {
            handedness: Some(Handedness::Left),
            ..GolferSelect::new("r")
        },
    ));
    handle_session(&msg, &writer, &sender);
    state.system.update(|c| c.golfers[1].name = "Sammy".into());
    follow_golfers(&state, &writer, &mut profiles, &sender);
    assert_eq!(writer.turn().handedness, Some(Handedness::Left));
    // Eric's linked game id up: his record's hand still reaches it.
    let msg = BusMsg::frp(FrpMessage::new(
        "gspro-1",
        GolferSelect {
            golfer: Some(Some("pureplay.local-3".parse().unwrap())),
            ..GolferSelect::new("r")
        },
    ));
    handle_session(&msg, &writer, &sender);
    emitted_frp(&mut rx);
    state
        .system
        .update(|c| c.golfers[0].handed = Some(Handedness::Left));
    follow_golfers(&state, &writer, &mut profiles, &sender);
    assert_eq!(writer.turn().handedness, Some(Handedness::Left));
    state
        .system
        .update(|c| c.golfers[0].handed = Some(Handedness::Right));
    follow_golfers(&state, &writer, &mut profiles, &sender);
    assert_eq!(writer.turn().handedness, Some(Handedness::Right));
}

/// A golfer cleared with `golfer: null` stays cleared
/// through an unrelated save; the start golfer comes up only when the
/// golfers go from none to some, or the golfer up is removed.
#[test]
fn an_unrelated_save_keeps_a_cleared_golfer() {
    let (_dir, state, writer, sender, _rx) = game();
    state.system.update(|c| {
        c.golfers = vec![
            flighthook::GolferRecord::new("flighthook.e", "Eric"),
            flighthook::GolferRecord::new("flighthook.s", "Sam"),
        ];
    });
    let mut profiles = Profiles::default();
    follow_golfers(&state, &writer, &mut profiles, &sender);
    let clear = BusMsg::frp(FrpMessage::new(
        "game-1",
        GolferSelect {
            golfer: Some(None),
            ..GolferSelect::new("r")
        },
    ));
    handle_session(&clear, &writer, &sender);
    assert_eq!(writer.turn().golfer, None);
    state
        .system
        .update(|c| c.default_units = flighthook::UnitSystem::Metric);
    follow_golfers(&state, &writer, &mut profiles, &sender);
    assert_eq!(writer.turn().golfer, None, "an unrelated save");
    state.system.update(|c| c.golfers.truncate(1));
    follow_golfers(&state, &writer, &mut profiles, &sender);
    assert_eq!(writer.turn().golfer, None, "a golfer not up removed");
    // Every golfer removed, then one added: none to some.
    state.system.update(|c| c.golfers.clear());
    follow_golfers(&state, &writer, &mut profiles, &sender);
    state.system.update(|c| {
        c.golfers = vec![flighthook::GolferRecord::new("flighthook.s", "Sam")];
    });
    follow_golfers(&state, &writer, &mut profiles, &sender);
    assert_eq!(
        writer.turn().golfer.as_ref().map(|g| g.as_str()),
        Some("flighthook.s")
    );
}

/// A header hand pick saved on the golfer up goes out as
/// the turn, then the golfer's profile, then the `bus.reply` (the
/// spec: an owner answers after publishing the state it changed).
#[test]
fn the_profile_a_pick_changed_goes_out_before_the_reply() {
    let (_dir, state, writer, sender, mut rx) = game();
    state.system.update(|c| {
        c.golfers = vec![flighthook::GolferRecord::new("flighthook.e", "Eric")];
    });
    let mut profiles = Profiles::default();
    follow_golfers(&state, &writer, &mut profiles, &sender);
    emitted_frp(&mut rx);
    let msg = BusMsg::frp(FrpMessage::new(
        state.flighthook_id(),
        GolferSelect {
            handedness: Some(Handedness::Left),
            ..GolferSelect::new("r")
        },
    ));
    handle_select(&msg, &state, &writer, &mut profiles, &sender);
    let kinds: Vec<&'static str> = emitted_frp(&mut rx)
        .iter()
        .map(|m| match &m.as_frp().unwrap().body {
            FrpBody::GolferTurn(_) => "turn",
            FrpBody::GolferProfile(_) => "profile",
            FrpBody::BusReply(_) => "reply",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["turn", "profile", "reply"]);
}

/// Removing the selected golfer clears
/// `selected_golfer` in the same save.
#[test]
fn removing_the_selected_golfer_clears_selected_golfer() {
    let (_dir, state, _writer, sender, _rx) = game();
    let state = Arc::new(state);
    let golfers = vec![
        flighthook::GolferRecord::new("flighthook.e", "Eric"),
        flighthook::GolferRecord::new("flighthook.s", "Sam"),
    ];
    state.system.update(|c| {
        c.golfers.clone_from(&golfers);
        c.selected_golfer = Some("flighthook.s".into());
    });
    let set = |golfers: Vec<flighthook::GolferRecord>| {
        run_config_command(
            &LocalEvent::ConfigCommand {
                request_id: None,
                action: Box::new(ConfigAction::SetGolfers { golfers }),
            },
            &state,
            sender.raw_sender(),
            &sender,
        );
    };
    set(golfers.clone());
    assert_eq!(
        state.system.snapshot().selected_golfer.as_deref(),
        Some("flighthook.s")
    );
    set(golfers[..1].to_vec());
    assert_eq!(state.system.snapshot().selected_golfer, None);
}

#[test]
fn a_camera_section_is_upserted_and_removed_with_its_actor() {
    let (_dir, state, _writer, sender, _rx) = game();
    let state = Arc::new(state);
    let section = flighthook::CameraSection {
        camera: flighthook::CameraSection::TEST_PATTERN.into(),
        mode: "160x120@15".into(),
        ..flighthook::CameraSection::default()
    };
    let command = |action| LocalEvent::ConfigCommand {
        request_id: None,
        action: Box::new(action),
    };
    run_config_command(
        &command(ConfigAction::UpsertCamera {
            index: "4".into(),
            section: section.clone(),
        }),
        &state,
        sender.raw_sender(),
        &sender,
    );
    assert_eq!(state.system.camera("4"), Some(section));
    assert!(state.actor_ids().contains(&"camera.4".to_owned()));
    run_config_command(
        &command(ConfigAction::Remove {
            id: "camera.4".into(),
        }),
        &state,
        sender.raw_sender(),
        &sender,
    );
    assert_eq!(state.system.camera("4"), None);
    assert!(!state.actor_ids().contains(&"camera.4".to_owned()));
}

#[test]
fn a_new_clip_folder_is_followed_and_the_old_clips_are_no_longer_served() {
    let (dir, state, _writer, sender, mut rx) = game();
    let state = Arc::new(state);
    let old = dir.0.join("old");
    state
        .system
        .update(|c| c.replay.clip_dir = Some(old.display().to_string()));
    let key = flighthook::ShotKey {
        shot_id: "a".into(),
        shot_number: 1,
    };
    let shot = crate::state::clips::ClipShot {
        key: key.clone(),
        device: "mock".into(),
        impact_lead_ms: 0,
        hand: None,
        golfer: None,
    };
    let slot = state
        .clips
        .begin(&old, &[shot], state.clips.generation())
        .expect("begin");
    let video = flighthook::ShotVideo {
        camera: "camera.0".into(),
        position: flighthook::CameraPosition::GolferFront,
        duration_ms: 1,
        fps: 30.0,
        trigger_offset_ms: 0,
        impact_offset_ms: 0,
        complete: true,
        dropped: 0,
        pose: false,
        swing: false,
        rotation: 0,
        analyzed_with: None,
    };
    assert!(state.clips.finish(&slot, video, 20).kept);

    let mut body = state.system.snapshot();
    body.replay.clip_dir = Some(dir.0.join("new").display().to_string());
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: None,
            action: Box::new(ConfigAction::ReplaceGlobal {
                config: Box::new(body),
            }),
        },
        &state,
        sender.raw_sender(),
        &sender,
    );
    assert!(state.clips.videos("a").is_empty());
    assert!(slot.dir.is_dir(), "the old folder is left as it is");
    let removed = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .find_map(|m| match m.event {
            LocalEvent::ShotVideoRemoved { keys } => Some(keys),
            _ => None,
        })
        .expect("shot_video_removed");
    assert_eq!(removed, [key]);
}

#[test]
fn clear_history_deletes_the_clips() {
    let (dir, state, _writer, _sender, _rx) = game();
    let clips = dir.0.join("clips");
    let shot = crate::state::clips::ClipShot {
        key: flighthook::ShotKey {
            shot_id: "a".into(),
            shot_number: 1,
        },
        device: "mock".into(),
        impact_lead_ms: 0,
        hand: None,
        golfer: None,
    };
    let slot = state
        .clips
        .begin(&clips, &[shot], state.clips.generation())
        .expect("begin");
    let video = flighthook::ShotVideo {
        camera: "camera.0".into(),
        position: flighthook::CameraPosition::GolferFront,
        duration_ms: 1,
        fps: 30.0,
        trigger_offset_ms: 0,
        impact_offset_ms: 0,
        complete: true,
        dropped: 0,
        pose: false,
        swing: false,
        rotation: 0,
        analyzed_with: None,
    };
    assert!(state.clips.finish(&slot, video, 20).kept);
    assert_eq!(state.clips.videos("a").len(), 1);
    clear_clips(&state);
    assert!(state.clips.videos("a").is_empty());
    assert!(!slot.dir.exists());
}

/// A launch monitor's `impact_lead_s` is read live by the cameras, so
/// changing it never restarts the monitor.
#[test]
fn changing_a_monitors_impact_lead_applies_without_a_restart() {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-system-test-{}",
        crate::state::config::generate_id()
    )));
    let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
    let state = Arc::new(state);
    let (bus_tx, _rx) = broadcast::channel(16);
    let ids = [
        "mevo.0",
        "r10.0",
        "square.0",
        "exputt.0",
        "mock_monitor.0",
        "openconnect_server.0",
    ];
    state.system.update(|c| {
        c.webserver.clear();
        c.mevo.insert("0".into(), Default::default());
        c.r10.insert("0".into(), Default::default());
        c.square.insert("0".into(), Default::default());
        c.exputt.insert("0".into(), Default::default());
        c.mock_monitor.insert(
            "0".into(),
            flighthook::MockMonitorSection {
                actor_id: None,
                name: "Mock".into(),
                enabled: true,
                impact_lead_s: None,
                driver: Default::default(),
            },
        );
        c.openconnect_server.insert("0".into(), Default::default());
    });
    let actors = resolve_actors(&state.system.snapshot());
    assert_eq!(actors.len(), ids.len());
    state.system.update(|c| {
        c.mevo.get_mut("0").unwrap().impact_lead_s = Some(0.5);
        c.r10.get_mut("0").unwrap().impact_lead_s = Some(0.5);
        c.square.get_mut("0").unwrap().impact_lead_s = Some(0.5);
        c.exputt.get_mut("0").unwrap().impact_lead_s = Some(0.5);
        c.mock_monitor.get_mut("0").unwrap().impact_lead_s = Some(0.5);
        c.openconnect_server.get_mut("0").unwrap().impact_lead_s = Some(0.5);
    });
    for ra in actors {
        let sender = BusSender::new(ra.id.clone(), bus_tx.clone(), Arc::default());
        assert!(ids.contains(&ra.id.as_str()), "{}", ra.id);
        assert!(
            matches!(
                ra.actor.reconfigure(&state, &sender),
                ReconfigureOutcome::Applied
            ),
            "{}",
            ra.id
        );
    }
}

// ----- Reconnect -----

/// State whose config `edit` sets up, its bus with a receiver, and every
/// configured actor started, as `main` starts them.
fn running(
    edit: impl FnOnce(&mut FlighthookConfig),
) -> (
    TempDir,
    Arc<SystemState>,
    broadcast::Sender<BusMsg>,
    broadcast::Receiver<BusMsg>,
) {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-system-test-{}",
        crate::state::config::generate_id()
    )));
    let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
    let state = Arc::new(state);
    state.system.update(|c| {
        c.webserver.clear();
        edit(c);
    });
    let (bus_tx, rx) = broadcast::channel(4096);
    for ra in resolve_actors(&state.system.snapshot()) {
        start_actor(ra.id, ra.actor, &state, &bus_tx);
    }
    (dir, state, bus_tx, rx)
}

fn mock(name: &str) -> flighthook::MockMonitorSection {
    flighthook::MockMonitorSection {
        actor_id: None,
        name: name.into(),
        enabled: true,
        impact_lead_s: None,
        driver: Default::default(),
    }
}

/// Reconnect `id` through the system actor's command handler. Returns
/// the outcome's restarted IDs and everything on the bus up to it.
fn reconnect_through_command(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    rx: &mut broadcast::Receiver<BusMsg>,
    id: &str,
) -> (Vec<String>, Vec<Seen>) {
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some("reconnect-1".into()),
            action: Box::new(ConfigAction::Reconnect { id: id.into() }),
        },
        state,
        bus_tx,
        &sender,
    );
    let seen: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    let restarted = outcome_of(&seen, "reconnect-1").0;
    (restarted, seen)
}

/// The restarted, stopped and started IDs of the outcome for
/// `request_id` in `seen`.
fn outcome_of(seen: &[Seen], request_id: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    seen.iter()
        .find_map(|m| match &m.event {
            LocalEvent::ConfigOutcome {
                request_id: Some(rid),
                restarted,
                stopped,
                started,
            } if rid == request_id => {
                assert_eq!(m.actor, "system");
                Some((restarted.clone(), stopped.clone(), started.clone()))
            }
            _ => None,
        })
        .expect("the outcome is emitted")
}

/// How many times the system started `actor` afresh in `seen`: its
/// Starting statuses that clear the card's `error` (an actor's own
/// Starting carries no such key).
fn fresh_starts(seen: &[Seen], actor: &str) -> usize {
    seen.iter()
        .filter(|m| m.actor == actor)
        .filter(|m| {
            matches!(&m.event, LocalEvent::CardStatus { status: ActorStatus::Starting, telemetry }
                    if telemetry.contains_key("error"))
        })
        .count()
}

/// Statuses `actor` sent, in order.
fn statuses(seen: &[Seen], actor: &str) -> Vec<ActorStatus> {
    seen.iter()
        .filter(|m| m.actor == actor)
        .filter_map(|m| match &m.event {
            LocalEvent::CardStatus { status, .. } => Some(*status),
            _ => None,
        })
        .collect()
}

/// Messages until `want` matches one, waiting up to 8 s.
fn until(
    rx: &mut broadcast::Receiver<BusMsg>,
    what: &str,
    mut want: impl FnMut(&Seen) -> bool,
) -> Vec<Seen> {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut seen = Vec::new();
    while std::time::Instant::now() < deadline {
        match rx.try_recv() {
            Ok(msg) => {
                let Some(msg) = local_of(msg) else {
                    continue;
                };
                let done = want(&msg);
                seen.push(msg);
                if done {
                    return seen;
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("{what} not seen");
}

/// Every message (FRP ones included) until `want` matches one,
/// waiting up to 8 s.
fn until_msg(
    rx: &mut broadcast::Receiver<BusMsg>,
    what: &str,
    mut want: impl FnMut(&BusMsg) -> bool,
) -> Vec<BusMsg> {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut seen = Vec::new();
    while std::time::Instant::now() < deadline {
        match rx.try_recv() {
            Ok(msg) => {
                let done = want(&msg);
                seen.push(msg);
                if done {
                    return seen;
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("{what} not seen");
}

fn stop_all(state: &SystemState) {
    for id in state.actor_ids() {
        state.stop_and_wait(&id);
    }
}

/// An output card is on the bus as an actor with its card's name:
/// its `bus.actor` as it starts, again on `bus.sync`, removed with the
/// card.
#[test]
fn an_output_card_is_on_the_bus_with_its_name() {
    const ID: &str = "c0ffee0000000001";
    let (_dir, state, bus_tx, mut rx) = running(|c| {
        c.random_club.insert(
            "0".into(),
            flighthook::RandomClubSection {
                actor_id: Some(ID.into()),
                name: "Random club".into(),
                enabled: true,
            },
        );
    });
    let named = |m: &BusMsg| {
        matches!(m.as_frp(), Some(f) if f.from == ID
                && matches!(&f.body, FrpBody::BusActor(a) if a.name == "Random club" && a.roles.is_empty()))
    };
    until_msg(&mut rx, "the output's bus.actor", named);
    resend_output_actors(&state, &bus_tx);
    until_msg(&mut rx, "the output's bus.actor on bus.sync", named);
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default())
        .with_frp_id(Some(state.flighthook_id()));
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: None,
            action: Box::new(ConfigAction::Remove {
                id: "random_club.0".into(),
            }),
        },
        &state,
        &bus_tx,
        &sender,
    );
    until_msg(&mut rx, "the output's removal", |m| {
        matches!(m.as_frp(), Some(f) if f.from == ID
                && matches!(&f.body, FrpBody::Removed(r) if r.kind == flightrelay::StateKind::BusActor))
    });
    stop_all(&state);
}

#[test]
fn a_reconnect_starts_a_new_instance_and_leaves_the_others_alone() {
    let (_dir, state, bus_tx, mut rx) = running(|c| {
        c.mock_monitor.insert("0".into(), mock("Mock A"));
        c.mock_monitor.insert("1".into(), mock("Mock B"));
        c.random_club.insert(
            "0".into(),
            flighthook::RandomClubSection {
                actor_id: None,
                name: "Random club".into(),
                enabled: true,
            },
        );
    });
    let thread = |id: &str| state.actor_thread(id).expect("running");
    let (old, other, club) = (
        thread("mock_monitor.0"),
        thread("mock_monitor.1"),
        thread("random_club.0"),
    );
    let config = serde_json::to_value(state.system.snapshot()).expect("config");

    let (restarted, seen) = reconnect_through_command(&state, &bus_tx, &mut rx, "mock_monitor.0");
    assert_eq!(restarted, ["mock_monitor.0"]);
    assert_ne!(thread("mock_monitor.0"), old, "a new instance");
    assert_eq!(thread("mock_monitor.1"), other);
    assert_eq!(thread("random_club.0"), club);
    assert_eq!(
        serde_json::to_value(state.system.snapshot()).expect("config"),
        config,
        "the settings are untouched"
    );

    // The old instance said goodbye before the card started again.
    let card = statuses(&seen, "mock_monitor.0");
    let stopped = card
        .iter()
        .position(|s| *s == ActorStatus::Disconnected)
        .expect("the old instance stopped");
    let started = card
        .iter()
        .rposition(|s| *s == ActorStatus::Starting)
        .expect("a new start");
    assert!(stopped < started, "{card:?}");
    for id in ["mock_monitor.1", "random_club.0"] {
        assert!(
            !statuses(&seen, id).contains(&ActorStatus::Disconnected),
            "{id} was not stopped"
        );
    }
    // The new instance runs: it reports itself connected, possibly
    // before the command's outcome.
    let connected = |m: &Seen| {
        m.actor == "mock_monitor.0"
            && matches!(
                m.event,
                LocalEvent::CardStatus {
                    status: ActorStatus::Connected,
                    ..
                }
            )
    };
    let start = seen
        .iter()
        .rposition(|m| {
            m.actor == "mock_monitor.0"
                && matches!(
                    m.event,
                    LocalEvent::CardStatus {
                        status: ActorStatus::Starting,
                        ..
                    }
                )
        })
        .expect("a new start");
    if !seen[start..].iter().any(connected) {
        until(&mut rx, "the new instance", connected);
    }
    stop_all(&state);
}

#[test]
fn only_a_configured_launch_monitor_or_output_can_be_reconnected() {
    let (_dir, state, bus_tx, mut rx) = running(|c| {
        c.mock_monitor.insert("0".into(), mock("Mock"));
    });
    state.system.update(|c| {
        c.camera.insert(
            "0".into(),
            flighthook::CameraSection {
                camera: flighthook::CameraSection::TEST_PATTERN.into(),
                ..flighthook::CameraSection::default()
            },
        );
        c.webserver.insert(
            "0".into(),
            flighthook::WebserverSection {
                name: "Web Server".into(),
                bind: "127.0.0.1:0".into(),
            },
        );
    });
    let config = state.system.snapshot();
    for id in ["camera.0", "webserver.0", "system"] {
        assert!(
            matches!(
                reconnect_target(&config, id),
                Err(ReconnectRefusal::NotReconnectable(_))
            ),
            "{id}"
        );
    }
    for id in [
        "mock_monitor.7",
        "mevo.0",
        "openconnect_client.0",
        "nonsense",
    ] {
        assert!(
            matches!(
                reconnect_target(&config, id),
                Err(ReconnectRefusal::NotFound(_))
            ),
            "{id}"
        );
    }
    assert_eq!(
        reconnect_target(&config, "mock_monitor.0").as_deref(),
        Ok("mock_monitor.0")
    );

    let running = state.actor_thread("mock_monitor.0");
    for id in ["camera.0", "webserver.0", "system", "mock_monitor.7"] {
        let (restarted, _) = reconnect_through_command(&state, &bus_tx, &mut rx, id);
        assert!(restarted.is_empty(), "{id}");
    }
    assert_eq!(state.actor_ids(), ["mock_monitor.0"], "nothing started");
    assert_eq!(state.actor_thread("mock_monitor.0"), running);
    stop_all(&state);
}

#[test]
fn the_gspro_id_names_its_open_connect_client() {
    let mut config = FlighthookConfig::default();
    config
        .openconnect_client
        .insert("2".into(), flighthook::OpenConnectClientSection::default());
    assert_eq!(
        reconnect_target(&config, "gspro.2").as_deref(),
        Ok("openconnect_client.2")
    );
}

/// A simulator that accepts every connection and says nothing. The
/// peer addresses it accepted, in order.
struct Simulator {
    addr: std::net::SocketAddr,
    peers: Arc<Mutex<Vec<std::net::SocketAddr>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Simulator {
    fn spawn() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let peers = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (peers, stop) = (Arc::clone(&peers), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut held = Vec::new();
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, peer)) => {
                            peers.lock().unwrap().push(peer);
                            held.push(stream);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        Self {
            addr,
            peers,
            stop,
            thread: Some(thread),
        }
    }

    fn peers(&self) -> Vec<std::net::SocketAddr> {
        self.peers.lock().unwrap().clone()
    }
}

impl Drop for Simulator {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn connected(m: &Seen, actor: &str) -> bool {
    m.actor == actor
        && matches!(
            m.event,
            LocalEvent::CardStatus {
                status: ActorStatus::Connected,
                ..
            }
        )
}

#[test]
fn an_open_connect_client_reconnects_and_is_not_refused_as_its_old_self() {
    let sim = Simulator::spawn();
    let (_dir, state, bus_tx, mut rx) = running(|c| {
        c.openconnect_client.insert(
            "0".into(),
            flighthook::OpenConnectClientSection {
                name: "GSPro".into(),
                address: Some(sim.addr.to_string()),
                ..flighthook::OpenConnectClientSection::default()
            },
        );
        // This process's own server, which refuses its own clients.
        c.openconnect_server.insert(
            "0".into(),
            flighthook::OpenConnectServerSection {
                actor_id: None,
                name: "Range Bridge".into(),
                enabled: true,
                bind: Some("127.0.0.1:0".into()),
                impact_lead_s: None,
            },
        );
    });
    let id = "openconnect_client.0";
    until(&mut rx, "connected", |m| connected(m, id));
    let first = sim.peers()[0];
    assert_eq!(
        state.local_clients.client_at(first).as_deref(),
        Some(id),
        "registered as this process's client"
    );

    let (restarted, _) = reconnect_through_command(&state, &bus_tx, &mut rx, id);
    assert_eq!(restarted, [id]);
    assert_eq!(
        state.local_clients.client_at(first),
        None,
        "the old connection's registration is released"
    );

    let seen = until(&mut rx, "connected again", |m| connected(m, id));
    let refused = seen.iter().any(|m| {
        m.actor == id
            && matches!(&m.event, LocalEvent::CardStatus { telemetry, .. }
                    if telemetry.get("error").is_some_and(|e| !e.is_empty()))
    });
    assert!(!refused, "no refusal");
    let peers = sim.peers();
    assert_eq!(peers.len(), 2, "{peers:?}");
    assert_eq!(state.local_clients.client_at(peers[1]).as_deref(), Some(id));
    stop_all(&state);
}

/// An actor whose thread ignores its stop for `holds`, then sends an
/// alert and ends. It holds nothing unless `replaceable` is false, and
/// needs a restart for any settings change.
struct Stubborn {
    holds: Duration,
    replaceable: bool,
}

/// How long a [`Stubborn`] is waited for.
const STUBBORN_WAIT: Duration = Duration::from_millis(100);

impl Actor for Stubborn {
    fn start(
        &self,
        _state: Arc<SystemState>,
        sender: BusSender,
        _receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let holds = self.holds;
        std::thread::spawn(move || {
            std::thread::sleep(holds);
            sender.local(LocalEvent::Alert {
                severity: Severity::Warn,
                message: "late".into(),
            });
        })
    }

    fn stop_wait(&self) -> Duration {
        STUBBORN_WAIT
    }

    fn replaceable_while_stopping(&self) -> bool {
        self.replaceable
    }

    fn reconfigure(&self, _state: &Arc<SystemState>, _sender: &BusSender) -> ReconfigureOutcome {
        ReconfigureOutcome::RestartRequired
    }
}

/// Running mock monitor `mock_monitor.N` for each `(N, stubborn)`, the
/// configured ones a [`Stubborn`] in place of the mock.
fn with_stubborn(
    mocks: &[(&str, Option<Stubborn>)],
) -> (
    TempDir,
    Arc<SystemState>,
    broadcast::Sender<BusMsg>,
    broadcast::Receiver<BusMsg>,
) {
    let (dir, state, bus_tx, rx) = running(|_| {});
    for (n, stubborn) in mocks {
        state
            .system
            .update(|c| drop(c.mock_monitor.insert((*n).into(), mock("Mock"))));
        let actor: Box<dyn Actor> = match stubborn {
            Some(s) => Box::new(Stubborn {
                holds: s.holds,
                replaceable: s.replaceable,
            }),
            None => Box::new(crate::actors::mock::MockActor::new(
                flighthook::MockMonitorSection::default(),
            )),
        };
        start_actor(format!("mock_monitor.{n}"), actor, &state, &bus_tx);
    }
    (dir, state, bus_tx, rx)
}

fn stubborn(holds_ms: u64, replaceable: bool) -> Option<Stubborn> {
    Some(Stubborn {
        holds: Duration::from_millis(holds_ms),
        replaceable,
    })
}

/// Whether `seen` has `actor`'s card saying its old connection is still
/// closing, and an Error alert saying the same.
fn still_closing_reported(seen: &[Seen], actor: &str) -> bool {
    let on_card = seen.iter().any(|m| {
        m.actor == actor
            && matches!(&m.event, LocalEvent::CardStatus { telemetry, .. }
                    if telemetry.get("error").map(String::as_str) == Some(STILL_CLOSING))
    });
    let alerted = seen.iter().any(|m| {
        m.actor == actor
            && matches!(&m.event, LocalEvent::Alert { severity: Severity::Error, message }
                    if message == STILL_CLOSING)
    });
    on_card && alerted
}

fn error_cleared(seen: &[Seen], actor: &str) -> bool {
    seen.iter().any(|m| {
            m.actor == actor
                && matches!(&m.event, LocalEvent::CardStatus { status: ActorStatus::Starting, telemetry }
                    if telemetry.get("error").is_some_and(String::is_empty))
        })
}

/// Whether a [`Stubborn`] `actor` had its last word in `seen`.
fn late_from(seen: &[Seen], actor: &str) -> bool {
    seen.iter().any(|m| {
        m.actor == actor
            && matches!(&m.event, LocalEvent::Alert { message, .. } if message == "late")
    })
}

fn late(seen: &[Seen]) -> bool {
    seen.iter()
        .any(|m| matches!(&m.event, LocalEvent::Alert { message, .. } if message == "late"))
}

/// The running system actor: a reconnect that finds the old connection
/// still closing says so, a second press meanwhile starts nothing, and
/// once the old connection has closed it reconnects by itself, clearing
/// the card's error.
#[test]
fn a_reconnect_waiting_on_the_old_connection_completes_by_itself() {
    let id = "mock_monitor.0";
    let (dir, state, bus_tx, mut rx) = with_stubborn(&[("0", stubborn(600, false))]);
    let (_, writer) = SystemState::new(dir.0.join("unused.toml"));
    let shutdown = Arc::new(AtomicBool::new(false));
    start_for_test(writer, &state, &bus_tx, Arc::clone(&shutdown));
    let web = BusSender::new("web".into(), bus_tx.clone(), Arc::default());
    let press = |n: u32| {
        web.local(LocalEvent::ConfigCommand {
            request_id: Some(format!("press-{n}")),
            action: Box::new(ConfigAction::Reconnect { id: id.into() }),
        });
    };
    press(1);
    let mut seen = until(
        &mut rx,
        "the first outcome",
        |m| matches!(&m.event, LocalEvent::ConfigOutcome { request_id: Some(r), .. } if r == "press-1"),
    );
    assert!(outcome_of(&seen, "press-1").0.is_empty(), "not started yet");
    assert!(
        still_closing_reported(&seen, id),
        "the card and an alert say why"
    );
    assert!(state.is_closing(id));
    assert_eq!(state.actor_thread(id), None, "nothing started beside it");

    // Pressed again meanwhile: refused, nothing more reported.
    press(2);
    let again = until(
        &mut rx,
        "the second outcome",
        |m| matches!(&m.event, LocalEvent::ConfigOutcome { request_id: Some(r), .. } if r == "press-2"),
    );
    assert!(outcome_of(&again, "press-2").0.is_empty());
    assert!(!still_closing_reported(&again, id), "reported once");
    seen.extend(again);

    // The old connection closes: it reconnects by itself.
    let done = until(&mut rx, "the restart", |m| {
        matches!(&m.event, LocalEvent::ConfigOutcome { request_id: None, restarted, .. }
                if restarted == &[id.to_owned()])
    });
    assert!(error_cleared(&done, id), "the error is cleared");
    assert!(!late(&done), "the old instance is muted");
    assert!(!state.is_closing(id));
    assert!(state.actor_thread(id).is_some(), "the new instance runs");
    seen.extend(done);
    let instance = state.actor_thread(id);
    seen.extend(until(&mut rx, "the new instance connects", |m| {
        connected(m, id)
    }));
    std::thread::sleep(Duration::from_millis(300));
    seen.extend(std::iter::from_fn(|| rx.try_recv().ok()).filter_map(local_of));
    let starts = fresh_starts(&seen, id);
    assert_eq!(starts, 1, "one restart, for two presses");
    assert_eq!(state.actor_thread(id), instance, "and no other after it");
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    stop_all(&state);
}

#[test]
fn an_old_instance_holding_nothing_is_replaced_and_silenced() {
    let (_dir, state, bus_tx, mut rx) = with_stubborn(&[("0", stubborn(400, true))]);
    let stubborn = state.actor_thread("mock_monitor.0");

    let (restarted, _) = reconnect_through_command(&state, &bus_tx, &mut rx, "mock_monitor.0");
    assert_eq!(restarted, ["mock_monitor.0"]);
    assert_ne!(state.actor_thread("mock_monitor.0"), stubborn);
    assert!(!state.is_closing("mock_monitor.0"));

    // The old instance's last word never reaches the bus.
    std::thread::sleep(Duration::from_millis(600));
    let seen: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    assert!(!late(&seen), "the replaced instance is muted");
    stop_all(&state);
}

/// Save `mock_monitor.N` (a scoped upsert) through the command handler.
/// Returns what it emitted.
fn save_mock(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    rx: &mut broadcast::Receiver<BusMsg>,
    n: &str,
) -> Vec<Seen> {
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some("save".into()),
            action: Box::new(ConfigAction::UpsertMockMonitor {
                index: n.into(),
                section: mock("Renamed"),
            }),
        },
        state,
        bus_tx,
        &sender,
    );
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect()
}

/// A Save that restarts an actor goes through the same release as a
/// reconnect: one still holding its device is not started beside, and
/// starts once it has ended; one holding nothing is replaced and muted.
#[test]
fn a_save_restarts_an_actor_by_the_reconnect_rules() {
    let (_dir, state, bus_tx, mut rx) =
        with_stubborn(&[("0", stubborn(500, false)), ("1", stubborn(1500, true))]);
    let held = "mock_monitor.0";

    let seen = save_mock(&state, &bus_tx, &mut rx, "0");
    let (restarted, stopped, started) = outcome_of(&seen, "save");
    assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
    assert!(still_closing_reported(&seen, held));
    assert!(state.is_closing(held));
    assert_eq!(state.actor_thread(held), None, "nothing beside it");
    // A second save meanwhile starts nothing either.
    let seen = save_mock(&state, &bus_tx, &mut rx, "0");
    let (restarted, _, started) = outcome_of(&seen, "save");
    assert!(restarted.is_empty() && started.is_empty());
    assert_eq!(state.actor_thread(held), None);

    // Once it has ended, it starts again from the saved settings.
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    wait_until("closed", || {
        restart_closed(&state, &bus_tx, &sender);
        !state.is_closing(held)
    });
    assert!(state.actor_thread(held).is_some());
    let seen: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    assert!(error_cleared(&seen, held));
    assert!(!late_from(&seen, held), "the old instance is muted");

    // Holding nothing: replaced at once.
    let free = "mock_monitor.1";
    let before = state.actor_thread(free);
    let seen = save_mock(&state, &bus_tx, &mut rx, "1");
    assert_eq!(outcome_of(&seen, "save").0, [free]);
    assert_ne!(state.actor_thread(free), before);
    std::thread::sleep(Duration::from_millis(1200));
    let seen: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    assert!(!late_from(&seen, free), "the replaced instance is muted");
    stop_all(&state);
}

/// An instance muted while its card stays
/// configured (still closing during a restart, or replaced beside)
/// leaves no `bus.actor` in an output's feed: its removal goes out for
/// it, before the new instance starts.
#[test]
fn a_muted_instance_leaves_no_bus_actor_in_the_feeds() {
    let (_dir, state, bus_tx, mut rx) =
        with_stubborn(&[("0", stubborn(500, false)), ("1", stubborn(1500, true))]);
    let mut feed = crate::actors::simout::Feed::new(
        crate::actors::gspro::GsProRouting::default(),
        Some(Arc::clone(&state)),
    );
    let ids: Vec<String> = ["mock_monitor.0", "mock_monitor.1"]
        .iter()
        .map(|card| state.system.actor_of_card(card).expect("actor id"))
        .collect();
    for id in &ids {
        feed.handle(&BusMsg::frp(flightrelay::FrpMessage::new(
            id.clone(),
            flightrelay::BusActor::new("Mock", 2.0)
                .with_role(flightrelay::types::roles::LAUNCH_MONITOR),
        )));
    }
    let removed = |msgs: &[BusMsg], id: &str| {
        msgs.iter().any(|m| {
            m.as_frp().is_some_and(|f| {
                f.from == id
                    && matches!(&f.body, FrpBody::Removed(r)
                            if r.kind == flightrelay::StateKind::BusActor)
            })
        })
    };
    // Still closing: no new instance yet, and no bus.actor left.
    let mut raw = bus_tx.subscribe();
    save_mock(&state, &bus_tx, &mut rx, "0");
    assert!(state.is_closing("mock_monitor.0"));
    let msgs: Vec<BusMsg> = std::iter::from_fn(|| raw.try_recv().ok()).collect();
    assert!(removed(&msgs, &ids[0]), "its removal went out");
    for msg in &msgs {
        feed.handle(msg);
    }
    assert!(feed.monitor(&ids[0]).is_none());
    assert!(feed.monitor(&ids[1]).is_some(), "the other stays");
    // Replaced beside: the removal goes out before the new instance.
    save_mock(&state, &bus_tx, &mut rx, "1");
    let msgs: Vec<BusMsg> = std::iter::from_fn(|| raw.try_recv().ok()).collect();
    assert!(removed(&msgs, &ids[1]), "its removal went out");
    stop_all(&state);
}

#[test]
fn a_removal_releases_an_actor_by_the_reconnect_rules() {
    let (_dir, state, bus_tx, mut rx) =
        with_stubborn(&[("0", stubborn(400, false)), ("1", stubborn(400, true))]);
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    for n in ["0", "1"] {
        let card = format!("mock_monitor.{n}");
        state.round.register_card_source(
            &card,
            &card,
            crate::actors::provenance::ShotSource::MockMonitor,
        );
    }
    for n in ["0", "1"] {
        run_config_command(
            &LocalEvent::ConfigCommand {
                request_id: Some(format!("remove-{n}")),
                action: Box::new(ConfigAction::Remove {
                    id: format!("mock_monitor.{n}"),
                }),
            },
            &state,
            &bus_tx,
            &sender,
        );
    }
    let seen: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    assert_eq!(outcome_of(&seen, "remove-0").1, ["mock_monitor.0"]);
    assert_eq!(outcome_of(&seen, "remove-1").1, ["mock_monitor.1"]);
    assert!(state.actor_ids().is_empty(), "both unregistered");
    for id in ["mock_monitor.0", "mock_monitor.1"] {
        assert_eq!(state.round.source(id), None, "{id} is not a source");
    }
    assert!(
        state.is_closing("mock_monitor.0"),
        "the one holding its device"
    );
    assert!(!state.is_closing("mock_monitor.1"));

    // Once it has ended, nothing starts: it was removed.
    wait_until("closed", || {
        restart_closed(&state, &bus_tx, &sender);
        !state.is_closing("mock_monitor.0")
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(state.actor_ids().is_empty());
    let seen: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    assert!(!late(&seen), "neither speaks after its removal");
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m.event, LocalEvent::ConfigOutcome { .. })),
        "no restart"
    );
}

/// Upsert `mock_monitor.N` named `name`, on or off, through the command
/// handler as `request_id`. Returns what it emitted.
fn switch_mock(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    rx: &mut broadcast::Receiver<BusMsg>,
    n: &str,
    enabled: bool,
    request_id: &str,
) -> Vec<Seen> {
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some(request_id.into()),
            action: Box::new(ConfigAction::UpsertMockMonitor {
                index: n.into(),
                section: flighthook::MockMonitorSection {
                    enabled,
                    ..mock("Mock")
                },
            }),
        },
        state,
        bus_tx,
        &sender,
    );
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect()
}

/// Turning a card off stops its actor as a removal does, keeping its
/// section; turning it on starts it like a new card. The other actors
/// are left alone, and a card that is off cannot be reconnected.
#[test]
fn turning_a_card_off_stops_it_and_on_starts_it_again() {
    let (_dir, state, bus_tx, mut rx) = running(|c| {
        c.mock_monitor.insert("0".into(), mock("Mock"));
        c.mock_monitor.insert("1".into(), mock("Other"));
    });
    let id = "mock_monitor.0";
    let other = state.actor_thread("mock_monitor.1");
    assert!(state.actor_thread(id).is_some());

    let seen = switch_mock(&state, &bus_tx, &mut rx, "0", false, "off");
    let (restarted, stopped, started) = outcome_of(&seen, "off");
    assert!(restarted.is_empty() && started.is_empty());
    assert_eq!(stopped, [id]);
    assert!(!state.actor_ids().iter().any(|a| a == id), "not running");
    assert!(!state.is_closing(id));
    let config = state.system.snapshot();
    assert!(!config.mock_monitor["0"].enabled, "the section stays, off");
    assert_eq!(config.disabled_actors(), [id]);
    assert_eq!(state.actor_thread("mock_monitor.1"), other, "left alone");

    // Off: a reconnect starts nothing and says why.
    let (restarted, _) = reconnect_through_command(&state, &bus_tx, &mut rx, id);
    assert!(restarted.is_empty());
    assert_eq!(not_reconnected(&state, id), format!("'{id}' is off"));
    assert!(!state.actor_ids().iter().any(|a| a == id));
    // A save of its other settings while off starts nothing either.
    let seen = switch_mock(&state, &bus_tx, &mut rx, "0", false, "rename");
    let (restarted, stopped, started) = outcome_of(&seen, "rename");
    assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());

    let seen = switch_mock(&state, &bus_tx, &mut rx, "0", true, "on");
    let (restarted, stopped, started) = outcome_of(&seen, "on");
    assert!(restarted.is_empty() && stopped.is_empty());
    assert_eq!(started, [id]);
    assert!(state.actor_thread(id).is_some(), "running again");
    until(&mut rx, "a status from the new instance", |m| {
        m.actor == id && matches!(m.event, LocalEvent::CardStatus { .. })
    });
    assert!(state.system.snapshot().disabled_actors().is_empty());
    assert_eq!(state.actor_thread("mock_monitor.1"), other);
    stop_all(&state);
}

/// A launch monitor turned off is forgotten by its actor ID: its host
/// sends its `bus.actor` removal and the round stops counting it as a
/// source; the other card stays one.
#[test]
fn turning_a_card_off_forgets_it_by_actor_id() {
    const OFF: &str = "a1b2c3d4e5f60718";
    const ON: &str = "0f1e2d3c4b5a6978";
    let with_id = |name: &str, id: &str| flighthook::MockMonitorSection {
        actor_id: Some(id.into()),
        ..mock(name)
    };
    let (_dir, state, bus_tx, mut rx) = running(|c| {
        c.mock_monitor.insert("0".into(), with_id("Mock", OFF));
        c.mock_monitor.insert("1".into(), with_id("Other", ON));
    });
    assert!(state.round.source(OFF).is_some());
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some("off".into()),
            action: Box::new(ConfigAction::UpsertMockMonitor {
                index: "0".into(),
                section: flighthook::MockMonitorSection {
                    enabled: false,
                    ..with_id("Mock", OFF)
                },
            }),
        },
        &state,
        &bus_tx,
        &sender,
    );
    let mut removed = false;
    while let Ok(msg) = rx.try_recv() {
        if let BusMsg::Frp { msg, .. } = &msg
            && msg.from == OFF
            && matches!(
                &msg.body,
                flightrelay::FrpBody::Removed(r) if r.kind == flightrelay::StateKind::BusActor
            )
        {
            removed = true;
        }
    }
    assert!(removed, "its bus.actor is removed");
    assert_eq!(state.round.source(OFF), None, "not a source");
    assert!(state.round.source(ON).is_some(), "the other stays one");
    stop_all(&state);
}

/// A launch monitor that outlives its stop wait and is muted.
struct Lingers;

impl Actor for Lingers {
    fn start(
        &self,
        _state: Arc<SystemState>,
        _sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            while !receiver.is_shutdown() {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(1500));
        })
    }

    fn stop_wait(&self) -> Duration {
        Duration::from_millis(50)
    }

    fn source(&self) -> Option<crate::actors::provenance::ShotSource> {
        Some(crate::actors::provenance::ShotSource::MockMonitor)
    }
}

/// A launch monitor turned off while its old instance lingers (muted,
/// so its own removal is dropped): the system actor removes its
/// `bus.actor`, and it is no source any more.
#[test]
fn a_lingering_card_turned_off_has_its_bus_actor_removed() {
    const OFF: &str = "a1b2c3d4e5f60719";
    let with_id = |enabled: bool| flighthook::MockMonitorSection {
        actor_id: Some(OFF.into()),
        enabled,
        ..mock("Mock")
    };
    let (_dir, state, bus_tx, mut rx) = running(|c| {
        c.mock_monitor.insert("0".into(), with_id(true));
    });
    state.stop_and_wait("mock_monitor.0");
    start_actor("mock_monitor.0".into(), Box::new(Lingers), &state, &bus_tx);
    assert!(state.round.source(OFF).is_some());
    while rx.try_recv().is_ok() {}
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some("off".into()),
            action: Box::new(ConfigAction::UpsertMockMonitor {
                index: "0".into(),
                section: with_id(false),
            }),
        },
        &state,
        &bus_tx,
        &sender,
    );
    let mut removed = 0;
    while let Ok(msg) = rx.try_recv() {
        if let BusMsg::Frp { msg, .. } = &msg
            && msg.from == OFF
            && matches!(
                &msg.body,
                flightrelay::FrpBody::Removed(r) if r.kind == flightrelay::StateKind::BusActor
            )
        {
            removed += 1;
        }
    }
    assert_eq!(removed, 1, "its bus.actor is removed once");
    assert_eq!(state.round.source(OFF), None, "not a source");
    stop_all(&state);
}

/// A card whose actor id changed (a restore) leaves no source under the
/// old id.
#[test]
fn a_new_actor_id_replaces_the_cards_old_source() {
    let (_dir, state, _bus_tx, _rx) = running(|_| {});
    let source = crate::actors::provenance::ShotSource::MockMonitor;
    state
        .round
        .register_card_source("mock_monitor.0", "old-id", source);
    state
        .round
        .register_card_source("mock_monitor.0", "new-id", source);
    assert_eq!(state.round.source("old-id"), None);
    assert_eq!(state.round.source("new-id"), Some(source));
    stop_all(&state);
}

/// A card that is off is not started with the others.
#[test]
fn a_card_that_is_off_is_not_started() {
    let (_dir, state, _bus_tx, _rx) = running(|c| {
        c.mock_monitor.insert(
            "0".into(),
            flighthook::MockMonitorSection {
                enabled: false,
                ..mock("Mock")
            },
        );
        c.mock_monitor.insert("1".into(), mock("Other"));
    });
    assert_eq!(state.actor_ids(), ["mock_monitor.1"]);
    stop_all(&state);
}

fn open_connect(address: &str, enabled: bool) -> flighthook::OpenConnectClientSection {
    flighthook::OpenConnectClientSection {
        name: "Open Connect".into(),
        enabled,
        address: Some(address.into()),
        full_monitor: None,
        chipping_monitor: None,
        putting_monitor: None,
        ..Default::default()
    }
}

fn upsert_open_connect(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    rx: &mut broadcast::Receiver<BusMsg>,
    n: &str,
    section: flighthook::OpenConnectClientSection,
    request_id: &str,
) -> Vec<Seen> {
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some(request_id.into()),
            action: Box::new(ConfigAction::UpsertOpenConnectClient {
                index: n.into(),
                section,
            }),
        },
        state,
        bus_tx,
        &sender,
    );
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect()
}

/// Editing a card that is on onto an address another enabled card of its
/// type has saves the edit and turns that card off (written
/// `enabled = false`, its actor stopped); the other card keeps running.
/// Moving to a free address leaves it on.
#[test]
fn editing_a_card_onto_a_held_address_saves_it_off() {
    let (dir, state, bus_tx, mut rx) = running(|c| {
        c.openconnect_client
            .insert("0".into(), open_connect("127.0.0.1:1", true));
        c.openconnect_client
            .insert("1".into(), open_connect("127.0.0.1:2", true));
    });
    let other = state.actor_thread("openconnect_client.0");
    assert!(other.is_some() && state.actor_thread("openconnect_client.1").is_some());

    // A free address: still on, restarted for the new address.
    let seen = upsert_open_connect(
        &state,
        &bus_tx,
        &mut rx,
        "1",
        open_connect("127.0.0.1:3", true),
        "free",
    );
    let (restarted, stopped, _) = outcome_of(&seen, "free");
    assert_eq!(restarted, ["openconnect_client.1"]);
    assert!(stopped.is_empty());
    assert!(state.system.snapshot().openconnect_client["1"].enabled);

    // The first card's address, spelled another way: saved, and off.
    let seen = upsert_open_connect(
        &state,
        &bus_tx,
        &mut rx,
        "1",
        open_connect("localhost:1", true),
        "held",
    );
    let (restarted, stopped, started) = outcome_of(&seen, "held");
    assert!(restarted.is_empty() && started.is_empty());
    assert_eq!(stopped, ["openconnect_client.1"]);
    let config = state.system.snapshot();
    assert!(!config.openconnect_client["1"].enabled);
    assert_eq!(
        config.openconnect_client["1"].address.as_deref(),
        Some("localhost:1"),
        "the edit is kept"
    );
    assert!(config.openconnect_client["0"].enabled);
    assert_eq!(config.disabled_actors(), ["openconnect_client.1"]);
    assert_eq!(state.actor_thread("openconnect_client.0"), other);
    assert!(state.actor_thread("openconnect_client.1").is_none());
    let written = std::fs::read_to_string(dir.0.join("config.toml")).expect("config file");
    assert_eq!(written.matches("enabled = false").count(), 1, "{written}");
    stop_all(&state);
}

/// Turning off an actor still holding its device follows the removal
/// rules: it is retired, and nothing starts once it has ended.
#[test]
fn turning_off_an_actor_holding_its_device_starts_nothing_when_it_ends() {
    let (_dir, state, bus_tx, mut rx) = with_stubborn(&[("0", stubborn(400, false))]);
    let id = "mock_monitor.0";
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    let seen = switch_mock(&state, &bus_tx, &mut rx, "0", false, "off");
    assert_eq!(outcome_of(&seen, "off").1, [id]);
    assert!(state.actor_ids().is_empty(), "unregistered");
    assert!(state.is_closing(id), "the one holding its device");
    wait_until("closed", || {
        restart_closed(&state, &bus_tx, &sender);
        !state.is_closing(id)
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(state.actor_ids().is_empty(), "off: not started again");
    let seen: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    assert!(!late(&seen), "it does not speak after being turned off");
}

/// A full save that gives a card another actor id restarts that card
/// and leaves the others running.
#[test]
fn a_full_save_with_another_card_id_restarts_that_card() {
    let (_dir, state, bus_tx, mut rx) = with_stubborn(&[("0", None), ("1", None)]);
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    let mut restored = state.system.snapshot();
    restored.mock_monitor.get_mut("1").expect("card").actor_id = Some("5555555555555555".into());
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some("ids".into()),
            action: Box::new(ConfigAction::ReplaceAll {
                config: Box::new(restored),
            }),
        },
        &state,
        &bus_tx,
        &sender,
    );
    let seen: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    assert_eq!(outcome_of(&seen, "ids").0, ["mock_monitor.1"]);
    assert_eq!(
        state
            .system
            .snapshot()
            .actor_of_card("mock_monitor.1")
            .as_deref(),
        Some("5555555555555555")
    );
    stop_all(&state);
}

/// A full save stops what it restarts together: the stop waits
/// overlap, so one command waits at most the longest of them.
#[test]
fn a_full_save_waits_for_its_stops_together() {
    let (_dir, state, bus_tx, mut rx) = with_stubborn(&[
        ("0", stubborn(1500, true)),
        ("1", stubborn(1500, true)),
        ("2", stubborn(1500, true)),
    ]);
    let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
    let at = std::time::Instant::now();
    run_config_command(
        &LocalEvent::ConfigCommand {
            request_id: Some("all".into()),
            action: Box::new(ConfigAction::ReplaceAll {
                config: Box::new(state.system.snapshot()),
            }),
        },
        &state,
        &bus_tx,
        &sender,
    );
    let took = at.elapsed();
    let seen: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    let mut restarted = outcome_of(&seen, "all").0;
    restarted.sort();
    assert_eq!(
        restarted,
        ["mock_monitor.0", "mock_monitor.1", "mock_monitor.2"]
    );
    assert!(took >= STUBBORN_WAIT, "{took:?}");
    assert!(
        took < STUBBORN_WAIT * 2,
        "not one wait after another: {took:?}"
    );
    stop_all(&state);
}

/// An actor that ends `takes` after its stop, then sets `ended`.
struct Slow {
    takes: Duration,
    ended: Arc<AtomicBool>,
}

impl Actor for Slow {
    fn start(
        &self,
        _state: Arc<SystemState>,
        _sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let (takes, ended) = (self.takes, Arc::clone(&self.ended));
        std::thread::spawn(move || {
            while !receiver.is_shutdown() {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(takes);
            ended.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    }
}

/// At exit every actor is stopped and waited for, together, so one that
/// lets go of its device on the way out gets to; the wait is capped.
#[test]
fn exit_waits_for_the_actors_to_let_go() {
    let (_dir, state, bus_tx, _rx) = running(|_| {});
    let slow: Vec<Arc<AtomicBool>> = (0..3).map(|_| Arc::default()).collect();
    for (n, ended) in slow.iter().enumerate() {
        start_actor(
            format!("square.{n}"),
            Box::new(Slow {
                takes: Duration::from_millis(300),
                ended: Arc::clone(ended),
            }),
            &state,
            &bus_tx,
        );
    }
    // One whose thread outlives the cap, and one still closing.
    start_actor(
        "mock_monitor.0".into(),
        Box::new(Stubborn {
            holds: Duration::from_secs(3),
            replaceable: false,
        }),
        &state,
        &bus_tx,
    );
    start_actor(
        "mock_monitor.1".into(),
        Box::new(Stubborn {
            holds: Duration::from_secs(3),
            replaceable: false,
        }),
        &state,
        &bus_tx,
    );
    state.close_in_background("mock_monitor.1");

    let cap = Duration::from_millis(800);
    let at = std::time::Instant::now();
    let running = state.stop_all_and_wait(cap);
    let took = at.elapsed();
    for ended in &slow {
        assert!(
            ended.load(std::sync::atomic::Ordering::SeqCst),
            "waited for"
        );
    }
    assert!(
        took < Duration::from_millis(1200),
        "together, capped: {took:?}"
    );
    assert_eq!(running, ["mock_monitor.0", "mock_monitor.1"]);
}

/// Two reconnects of one actor back to back: the second, arriving while
/// the first still waits for the old instance, is answered with nothing
/// restarted and starts nothing; the first starts exactly one new
/// instance.
#[test]
fn a_reconnect_during_another_of_the_same_actor_starts_nothing_more() {
    let id = "mock_monitor.0";
    let (_dir, state, mut rx, web, shutdown) = slow_to_close(Duration::from_millis(800));
    let old = state.actor_thread(id);
    web.local(LocalEvent::ConfigCommand {
        request_id: Some("first".into()),
        action: Box::new(ConfigAction::Reconnect { id: id.into() }),
    });
    wait_until("the first is waiting", || state.is_settling(id));
    web.local(LocalEvent::ConfigCommand {
        request_id: Some("second".into()),
        action: Box::new(ConfigAction::Reconnect { id: id.into() }),
    });
    let mut seen = until(&mut rx, "the second", |m| outcome_for(m, "second"));
    assert!(
        !seen.iter().any(|m| outcome_for(m, "first")),
        "answered during the first's wait"
    );
    let (restarted, stopped, started) = outcome_of(&seen, "second");
    assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
    assert!(state.is_settling(id), "the first still waits");
    assert_eq!(state.actor_thread(id), old, "nothing started beside it");

    seen.extend(until(&mut rx, "the first", |m| outcome_for(m, "first")));
    assert_eq!(outcome_of(&seen, "first").0, [id]);
    std::thread::sleep(Duration::from_millis(200));
    seen.extend(std::iter::from_fn(|| rx.try_recv().ok()).filter_map(local_of));
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    let card = statuses(&seen, id);
    assert_eq!(fresh_starts(&seen, id), 1, "{card:?}");
    assert!(!state.is_settling(id));
    assert_ne!(state.actor_thread(id), old, "one new instance runs");
    stop_all(&state);
}

/// An actor that ends `holds` after its stop and may be waited for as
/// long as an ExPutt.
struct SlowToClose {
    holds: Duration,
}

impl Actor for SlowToClose {
    fn start(
        &self,
        _state: Arc<SystemState>,
        _sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let holds = self.holds;
        std::thread::spawn(move || {
            while !receiver.is_shutdown() {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(holds);
        })
    }

    fn stop_wait(&self) -> Duration {
        crate::actors::MAX_STOP_WAIT
    }

    fn reconfigure(&self, _state: &Arc<SystemState>, _sender: &BusSender) -> ReconfigureOutcome {
        ReconfigureOutcome::RestartRequired
    }
}

/// The running system actor with `mock_monitor.0` a [`SlowToClose`]
/// holding for `holds`, and a sender for commands and game events.
fn slow_to_close(
    holds: Duration,
) -> (
    TempDir,
    Arc<SystemState>,
    broadcast::Receiver<BusMsg>,
    BusSender,
    Arc<AtomicBool>,
) {
    let (dir, state, bus_tx, rx) = running(|c| {
        c.mock_monitor.insert("0".into(), mock("Mock"));
    });
    state.stop_and_wait("mock_monitor.0");
    start_actor(
        "mock_monitor.0".into(),
        Box::new(SlowToClose { holds }),
        &state,
        &bus_tx,
    );
    let (_, writer) = SystemState::new(dir.0.join("unused.toml"));
    let shutdown = Arc::new(AtomicBool::new(false));
    start_for_test(writer, &state, &bus_tx, Arc::clone(&shutdown));
    let web = BusSender::new("web".into(), bus_tx, Arc::default()).with_frp_id(Some("web".into()));
    (dir, state, rx, web, shutdown)
}

fn outcome_for(m: &Seen, request: &str) -> bool {
    matches!(&m.event, LocalEvent::ConfigOutcome { request_id: Some(r), .. } if r == request)
}

/// While a reconnect waits for an actor that takes an ExPutt-length
/// stop, the system actor goes on: a club change is applied at once,
/// and the reconnect still completes once the old instance has ended.
#[test]
fn a_club_change_during_a_long_reconnect_is_applied_at_once() {
    let id = "mock_monitor.0";
    let holds = Duration::from_millis(1500);
    let (_dir, state, mut rx, web, shutdown) = slow_to_close(holds);
    let at = std::time::Instant::now();
    web.local(LocalEvent::ConfigCommand {
        request_id: Some("reconnect".into()),
        action: Box::new(ConfigAction::Reconnect { id: id.into() }),
    });
    wait_until("the reconnect is waiting", || state.is_settling(id));
    web.frp(select(Some(Club::PitchingWedge), None, None));
    let session = state.flighthook_id();
    let seen = until_msg(&mut rx, "the club's turn", |m| {
        m.turn_of(&session)
            .is_some_and(|(turn, _)| turn.club == Some(Club::PitchingWedge))
    });
    assert!(at.elapsed() < holds, "applied during the wait");
    assert!(
        !seen
            .into_iter()
            .filter_map(local_of)
            .any(|m| outcome_for(&m, "reconnect")),
        "before the reconnect finished"
    );
    assert!(state.is_settling(id));

    let done = until(&mut rx, "the reconnect", |m| outcome_for(m, "reconnect"));
    assert!(at.elapsed() >= holds, "it waited for the old instance");
    assert_eq!(outcome_of(&done, "reconnect").0, [id]);
    assert!(!state.is_settling(id));
    assert!(state.actor_thread(id).is_some(), "the new instance runs");
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    stop_all(&state);
}

/// A save of an actor a reconnect is still waiting for follows the
/// closing rules: answered at once, it starts nothing itself, and the
/// reconnect starts one instance from the saved settings.
#[test]
fn a_save_during_a_long_reconnect_follows_the_closing_rules() {
    let id = "mock_monitor.0";
    let holds = Duration::from_millis(1200);
    let (_dir, state, mut rx, web, shutdown) = slow_to_close(holds);
    let at = std::time::Instant::now();
    web.local(LocalEvent::ConfigCommand {
        request_id: Some("reconnect".into()),
        action: Box::new(ConfigAction::Reconnect { id: id.into() }),
    });
    wait_until("the reconnect is waiting", || state.is_settling(id));
    let old = state.actor_thread(id);
    web.local(LocalEvent::ConfigCommand {
        request_id: Some("save".into()),
        action: Box::new(ConfigAction::UpsertMockMonitor {
            index: "0".into(),
            section: mock("Renamed"),
        }),
    });
    let mut seen = until(&mut rx, "the save", |m| outcome_for(m, "save"));
    assert!(at.elapsed() < holds, "answered during the wait");
    let (restarted, stopped, started) = outcome_of(&seen, "save");
    assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
    assert_eq!(state.actor_thread(id), old, "nothing started beside it");
    assert_eq!(state.system.snapshot().mock_monitor["0"].name, "Renamed");

    seen.extend(until(&mut rx, "the reconnect", |m| {
        outcome_for(m, "reconnect")
    }));
    assert_eq!(outcome_of(&seen, "reconnect").0, [id]);
    std::thread::sleep(Duration::from_millis(200));
    seen.extend(std::iter::from_fn(|| rx.try_recv().ok()).filter_map(local_of));
    let starts = fresh_starts(&seen, id);
    assert_eq!(starts, 1, "one new instance");
    assert_ne!(state.actor_thread(id), old);
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    stop_all(&state);
}

/// A removal of an actor a reconnect is still waiting for lists it as
/// stopped at once and silences the old instance; the reconnect then
/// starts nothing.
#[test]
fn a_removal_during_a_long_reconnect_starts_nothing() {
    let id = "mock_monitor.0";
    let (_dir, state, mut rx, web, shutdown) = slow_to_close(Duration::from_millis(800));
    web.local(LocalEvent::ConfigCommand {
        request_id: Some("reconnect".into()),
        action: Box::new(ConfigAction::Reconnect { id: id.into() }),
    });
    wait_until("the reconnect is waiting", || state.is_settling(id));
    web.local(LocalEvent::ConfigCommand {
        request_id: Some("remove".into()),
        action: Box::new(ConfigAction::Remove { id: id.into() }),
    });
    let mut seen = until(&mut rx, "the removal", |m| outcome_for(m, "remove"));
    assert_eq!(outcome_of(&seen, "remove").1, [id]);
    seen.extend(until(&mut rx, "the reconnect", |m| {
        outcome_for(m, "reconnect")
    }));
    let (restarted, stopped, started) = outcome_of(&seen, "reconnect");
    assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
    assert!(state.actor_ids().iter().all(|a| a != id));
    assert_eq!(fresh_starts(&seen, id), 0);
    assert_eq!(state.round.source(id), None, "not a shot source");
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    stop_all(&state);
}

// ----- Stopping each kind of connection -----

/// Stop `id` and wait for it, as a reconnect does. Returns how long it
/// took.
fn stop_timed(state: &SystemState, id: &str) -> Duration {
    let at = std::time::Instant::now();
    assert_eq!(state.stop_and_wait(id), Stopped::Exited, "{id} ended");
    at.elapsed()
}

/// Well under the default stop wait: the actor did not sit out a
/// timeout.
const PROMPT: Duration = Duration::from_secs(1);

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_mevo_waiting_on_a_silent_device_stops_at_once() {
    let device = Simulator::spawn();
    let (_dir, state, _bus_tx, _rx) = running(|c| {
        let mut section: flighthook::MevoSection =
            serde_json::from_str("{}").expect("empty section");
        section.driver.address = Some(device.addr.to_string());
        c.mevo.insert("0".into(), section);
    });
    // Connected, its handshake unanswered.
    wait_until("connected", || !device.peers().is_empty());
    std::thread::sleep(Duration::from_millis(200));
    let took = stop_timed(&state, "mevo.0");
    assert!(took < PROMPT, "{took:?}");
}

#[test]
fn an_open_connect_client_on_a_silent_simulator_stops_at_once_and_unregisters() {
    let sim = Simulator::spawn();
    let (_dir, state, _bus_tx, mut rx) = running(|c| {
        c.openconnect_client.insert(
            "0".into(),
            flighthook::OpenConnectClientSection {
                name: "GSPro".into(),
                address: Some(sim.addr.to_string()),
                ..flighthook::OpenConnectClientSection::default()
            },
        );
    });
    let id = "openconnect_client.0";
    until(&mut rx, "connected", |m| connected(m, id));
    let peer = sim.peers()[0];
    let took = stop_timed(&state, id);
    assert!(took < PROMPT, "{took:?}");
    assert_eq!(state.local_clients.client_at(peer), None);
}

#[test]
fn an_open_connect_client_before_its_verdict_stops_at_once() {
    let sim = Simulator::spawn();
    let (_dir, state, _bus_tx, _rx) = running(|c| {
        c.openconnect_client.insert(
            "0".into(),
            flighthook::OpenConnectClientSection {
                name: "GSPro".into(),
                address: Some(sim.addr.to_string()),
                ..flighthook::OpenConnectClientSection::default()
            },
        );
    });
    // Inside its first second, before it reports itself connected.
    wait_until("connected", || !sim.peers().is_empty());
    let took = stop_timed(&state, "openconnect_client.0");
    assert!(took < PROMPT, "{took:?}");
}

fn free_port() -> std::net::SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
}

#[test]
fn an_open_connect_server_with_a_silent_monitor_stops_at_once_and_frees_its_port() {
    use std::io::Read as _;
    let bind = free_port();
    let (_dir, state, _bus_tx, mut rx) = running(|c| {
        c.openconnect_server.insert(
            "0".into(),
            flighthook::OpenConnectServerSection {
                actor_id: None,
                name: "Range Bridge".into(),
                enabled: true,
                bind: Some(bind.to_string()),
                impact_lead_s: None,
            },
        );
    });
    let id = "openconnect_server.0";
    let mut monitor = loop {
        if let Ok(stream) = std::net::TcpStream::connect(bind) {
            break stream;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    until(&mut rx, "monitor connected", |m| connected(m, id));
    let took = stop_timed(&state, id);
    assert!(took < PROMPT, "{took:?}");
    monitor
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    assert_eq!(monitor.read(&mut [0; 16]).ok(), Some(0), "closed");
    assert!(std::net::TcpListener::bind(bind).is_ok(), "port released");
}

#[test]
fn an_open_connect_server_waiting_to_bind_stops_at_once() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let bind = taken.local_addr().expect("addr");
    let (_dir, state, _bus_tx, mut rx) = running(|c| {
        c.openconnect_server.insert(
            "0".into(),
            flighthook::OpenConnectServerSection {
                actor_id: None,
                name: "Range Bridge".into(),
                enabled: true,
                bind: Some(bind.to_string()),
                impact_lead_s: None,
            },
        );
    });
    until_msg(&mut rx, "bind failed", |m| match m {
        BusMsg::Frp {
            msg:
                FrpMessage {
                    body: FrpBody::BusAlert(alert),
                    ..
                },
            ..
        } => alert.message.contains("could not bind"),
        BusMsg::Local {
            event: LocalEvent::Alert { message, .. },
            ..
        } => message.contains("could not bind"),
        _ => false,
    });
    // In its wait before binding again.
    let took = stop_timed(&state, "openconnect_server.0");
    assert!(took < PROMPT, "{took:?}");
}

/// A stand-in for an Open Connect server on `bind` whose thread still
/// owns the listener: it lets go of the port `port_held` after its stop,
/// and ends `thread_lingers` after that. Its replaceable probe is the
/// real server's.
struct HeldPort {
    bind: std::net::SocketAddr,
    listener: Mutex<Option<std::net::TcpListener>>,
    port_held: Duration,
    thread_lingers: Duration,
}

impl Actor for HeldPort {
    fn start(
        &self,
        _state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let listener = self.listener.lock().expect("listener").take();
        let (held, lingers) = (self.port_held, self.thread_lingers);
        std::thread::spawn(move || {
            while !receiver.is_shutdown() {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(held);
            drop(listener);
            std::thread::sleep(lingers);
            sender.local(LocalEvent::Alert {
                severity: Severity::Warn,
                message: "late".into(),
            });
        })
    }

    fn stop_wait(&self) -> Duration {
        STUBBORN_WAIT
    }

    fn replaceable_while_stopping(&self) -> bool {
        crate::actors::openconnect::OpenConnectServerActor { bind: self.bind }
            .replaceable_while_stopping()
    }

    fn reconfigure(&self, _state: &Arc<SystemState>, _sender: &BusSender) -> ReconfigureOutcome {
        ReconfigureOutcome::RestartRequired
    }
}

/// Running `openconnect_server.0` configured on a free port, with a
/// [`HeldPort`] in place of the server.
fn with_held_port(
    port_held: Duration,
    thread_lingers: Duration,
) -> (
    TempDir,
    Arc<SystemState>,
    broadcast::Sender<BusMsg>,
    broadcast::Receiver<BusMsg>,
    std::net::SocketAddr,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let bind = listener.local_addr().expect("addr");
    let (dir, state, bus_tx, rx) = running(|c| {
        c.openconnect_server.insert(
            "0".into(),
            flighthook::OpenConnectServerSection {
                actor_id: None,
                name: "Range Bridge".into(),
                enabled: true,
                bind: Some(bind.to_string()),
                impact_lead_s: None,
            },
        );
    });
    // `running` started the real server, which found the port taken:
    // put the stand-in in its place.
    state.stop_and_wait("openconnect_server.0");
    start_actor(
        "openconnect_server.0".into(),
        Box::new(HeldPort {
            bind,
            listener: Mutex::new(Some(listener)),
            port_held,
            thread_lingers,
        }),
        &state,
        &bus_tx,
    );
    (dir, state, bus_tx, rx, bind)
}

/// An Open Connect server whose old thread still holds the listen port
/// through the stop wait cannot be replaced: the reconnect says the old
/// connection is still closing, and the server starts by itself, on the
/// same port, once the port is free and the thread has ended.
#[test]
fn an_open_connect_server_holding_its_port_is_restarted_when_it_lets_go() {
    let id = "openconnect_server.0";
    let (dir, state, bus_tx, mut rx, bind) =
        with_held_port(Duration::from_millis(600), Duration::ZERO);
    let (_, writer) = SystemState::new(dir.0.join("unused.toml"));
    let shutdown = Arc::new(AtomicBool::new(false));
    start_for_test(writer, &state, &bus_tx, Arc::clone(&shutdown));
    let web = BusSender::new("web".into(), bus_tx.clone(), Arc::default());
    web.local(LocalEvent::ConfigCommand {
        request_id: Some("press".into()),
        action: Box::new(ConfigAction::Reconnect { id: id.into() }),
    });
    let seen = until(&mut rx, "the outcome", |m| outcome_for(m, "press"));
    assert!(outcome_of(&seen, "press").0.is_empty(), "not started yet");
    assert!(still_closing_reported(&seen, id));
    assert!(state.is_closing(id));
    assert_eq!(state.actor_thread(id), None, "nothing started beside it");
    assert!(
        std::net::TcpListener::bind(bind).is_err(),
        "the old thread still owns the port"
    );

    let done = until(&mut rx, "the restart", |m| {
        matches!(&m.event, LocalEvent::ConfigOutcome { request_id: None, restarted, .. }
                if restarted == &[id.to_owned()])
    });
    assert!(error_cleared(&done, id), "the error is cleared");
    assert!(!late(&done), "the old instance is muted");
    wait_until("the new server listens", || {
        std::net::TcpStream::connect(bind).is_ok()
    });
    assert!(!state.is_closing(id));
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    stop_all(&state);
}

/// Once the old thread has let go of the port, a server that has not
/// ended yet is replaced at once and its last word is muted.
#[test]
fn an_open_connect_server_that_freed_its_port_is_replaced_at_once() {
    let id = "openconnect_server.0";
    let (_dir, state, bus_tx, mut rx, bind) =
        with_held_port(Duration::ZERO, Duration::from_millis(600));
    let old = state.actor_thread(id);

    let (restarted, seen) = reconnect_through_command(&state, &bus_tx, &mut rx, id);
    assert_eq!(restarted, [id]);
    assert!(!still_closing_reported(&seen, id));
    assert_ne!(state.actor_thread(id), old);
    assert!(!state.is_closing(id));
    wait_until("the new server listens", || {
        std::net::TcpStream::connect(bind).is_ok()
    });

    std::thread::sleep(Duration::from_millis(700));
    let after: Vec<Seen> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(local_of)
        .collect();
    assert!(!late(&after), "the replaced instance is muted");
    stop_all(&state);
}

#[test]
fn mock_actors_stop_at_once() {
    let (_dir, state, _bus_tx, _rx) = running(|c| {
        c.mock_monitor.insert("0".into(), mock("Mock"));
        c.random_club.insert(
            "0".into(),
            flighthook::RandomClubSection {
                actor_id: None,
                name: "Random club".into(),
                enabled: true,
            },
        );
    });
    for id in ["mock_monitor.0", "random_club.0"] {
        let took = stop_timed(&state, id);
        assert!(took < PROMPT, "{id}: {took:?}");
    }
}
