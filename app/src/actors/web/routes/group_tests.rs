//! Tests of the shot history, sets and grouping routes.

use super::super::tests::{Ev, TempDir, feed, test_state, web_state};
use super::super::tests::{feed_local, turn_event};
use super::*;
use flighthook::{Club, Handedness, Launch, ShotGrouping, ShotKey, Velocity};
use flightrelay::FrpBody;

/// The local event `msg` carries, if it is one.
fn local_of(msg: &BusMsg) -> Option<&LocalEvent> {
    msg.as_local().map(|(_, event)| event)
}

fn next_event(rx: &mut broadcast::Receiver<BusMsg>) -> BusMsg {
    rx.try_recv().expect("a bus message")
}

/// The next message on `rx`: a local event of the web API.
fn next_local(rx: &mut broadcast::Receiver<BusMsg>) -> LocalEvent {
    let msg = next_event(rx);
    let (card, event) = msg.as_local().expect("a local event");
    assert_eq!(card, "web");
    event.clone()
}

async fn body_text(resp: Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 4096)
        .await
        .expect("body");
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

/// A session that answers every `golfer.select` on the bus, as the
/// system actor does; returns the selects it saw.
fn answering_session(
    state: &Arc<WebState>,
    writer: crate::state::SessionWriter,
    count: usize,
) -> tokio::task::JoinHandle<Vec<GolferSelect>> {
    let sender = crate::bus::BusSender::new("system".into(), state.bus_tx.clone(), Arc::default())
        .with_frp_id(Some(state.root.flighthook_id()));
    let mut rx = state.bus_tx.subscribe();
    tokio::spawn(async move {
        let mut seen = Vec::new();
        while seen.len() < count {
            let Ok(msg) = rx.recv().await else { break };
            if let Some(frp) = msg.as_frp()
                && let flightrelay::FrpBody::GolferSelect(select) = &frp.body
            {
                seen.push(select.clone());
                crate::actors::system::handle_session(&msg, &writer, &sender);
            }
        }
        seen
    })
}

#[tokio::test]
async fn post_select_answers_after_the_session_applied_it() {
    let (state, writer, _dir) = test_state();
    let state = Arc::new(state);
    let session = answering_session(&state, writer, 3);
    for body in [
        r#"{"club":"7I"}"#,
        r#"{"handedness":"lh"}"#,
        r#"{"mode":"chipping"}"#,
    ] {
        let resp = post_select_within(&state, body.as_bytes(), Duration::from_secs(5)).await;
        assert_eq!(resp.status(), StatusCode::OK, "{body}");
    }
    let turn = state.root.session.turn();
    assert_eq!(turn.club, Some(Club::Iron7));
    assert_eq!(turn.handedness, Some(Handedness::Left));
    assert_eq!(turn.mode, Some(flighthook::Mode::Chipping));
    let seen = session.await.expect("session");
    // The club alone never carries a mode.
    assert_eq!(
        (seen[0].club, seen[0].mode),
        (Some(Some(Club::Iron7)), None)
    );
    assert!(seen.iter().all(|s| s.r#ref.starts_with("web-")));
}

#[tokio::test]
async fn post_select_refuses_what_is_no_select_with_a_400() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    for body in [
        r#"{"club":"banana"}"#,
        r#"{"handedness":"left"}"#,
        r#"{"mode":"driving"}"#,
        r#"{"golfer":"nonamespace"}"#,
        r#"["7I"]"#,
        "not json",
    ] {
        let resp = post_select_within(&state, body.as_bytes(), Duration::from_millis(50)).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            body_text(resp).await,
            r#"{"error":"bad_request"}"#,
            "{body}"
        );
    }
    assert!(rx.try_recv().is_err(), "nothing sent");
    // A select nobody answers: 504.
    let resp = post_select_within(&state, br#"{"club":"DR"}"#, Duration::from_millis(50)).await;
    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn post_find_mat_emits_find_mat_for_a_configured_exputt() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.exputt
            .insert("0".into(), flighthook::ExPuttSection::default());
    });
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    let resp = post_find_mat(
        State(Arc::clone(&state)),
        Json(FindMatRequest {
            actor: "exputt.0".into(),
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(matches!(next_local(&mut rx), LocalEvent::FindMat { target } if target == "exputt.0"));
}

#[tokio::test]
async fn post_find_mat_rejects_an_unknown_exputt() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.exputt
            .insert("0".into(), flighthook::ExPuttSection::default());
    });
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    for actor in ["exputt.1", "square.0", "exputt"] {
        let resp = post_find_mat(
            State(Arc::clone(&state)),
            Json(FindMatRequest {
                actor: actor.into(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{actor}");
        assert_eq!(
            body_text(resp).await,
            format!("no ExPutt with ID '{actor}'")
        );
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
}

/// Every message on `rx` so far.
fn drain(rx: &mut broadcast::Receiver<BusMsg>) -> Vec<BusMsg> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

/// 1, 3 and 5 yd are sent as Auto-chip shots numbered 1, 2, 3: the
/// reply describes the shot put on the bus, and the web layer stores it
/// under the Auto-chip actor with its simulated total.
#[tokio::test]
async fn auto_chip_sends_1_3_and_5_yards() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| c.simulation.auto_chip = true);
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    for (n, yards) in (1..).zip(flighthook::AUTO_CHIP_YARDS) {
        let Json(resp) = post_auto_chip(State(Arc::clone(&state)), Json(AutoChipRequest { yards }))
            .await
            .expect("accepted");
        assert_eq!(resp.actor, flighthook::AUTO_CHIP_ACTOR);
        assert_eq!(resp.shot_number, n);
        assert_eq!(resp.launch_elevation, 45.0);
        #[allow(clippy::cast_possible_truncation)]
        let spin = (300.0 * resp.ball_speed_mph).round() as i32;
        assert_eq!(resp.backspin_rpm, spin);

        // The Auto-chip actor's FRP messages: the web layer's own
        // shot_group events are left out.
        let msgs: Vec<_> = drain(&mut rx)
            .into_iter()
            .filter(|m| m.as_frp().is_some())
            .collect();
        assert!(
            msgs.iter()
                .any(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::ShotTrigger(_)))),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::ShotFinished(_)))),
            "{msgs:?}"
        );
        let mut pending = super::super::PendingShots::new();
        for msg in &msgs {
            assert_eq!(
                msg.as_frp().map(|f| f.from.clone()),
                Some(state.root.auto_chip_id())
            );
            super::super::apply_bus_event(&state, msg, &mut pending).await;
        }
        let shots = state.shots.read().await;
        let stored = shots.back().expect("stored");
        assert_eq!(
            stored.data.card.as_deref(),
            Some(flighthook::AUTO_CHIP_ACTOR)
        );
        assert_eq!(stored.data.shot_id.as_deref(), Some(resp.shot_id.as_str()));
        assert!(stored.data.club.is_none());
        let total = stored
            .data
            .shown()
            .derived
            .and_then(|d| d.total_distance)
            .expect("total")
            .as_yards();
        assert!((total - f64::from(yards)).abs() <= 0.1, "{yards}: {total}");
    }
    assert_eq!(state.shots.read().await.len(), 3);
}

#[tokio::test]
async fn auto_chip_rejects_any_other_distance() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| c.simulation.auto_chip = true);
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    for yards in [0, 2, 4, 6, 10, 100, u32::MAX] {
        let (status, Json(body)) =
            post_auto_chip(State(Arc::clone(&state)), Json(AutoChipRequest { yards }))
                .await
                .expect_err("rejected");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{yards}");
        assert_eq!(body["error"], "yards must be one of 1, 3, 5");
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
}

#[tokio::test]
async fn auto_chip_refuses_while_disabled_in_settings() {
    let (state, _writer, _dir) = test_state();
    assert!(!state.root.system.simulation().auto_chip, "off by default");
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    let (status, Json(body)) = post_auto_chip(
        State(Arc::clone(&state)),
        Json(AutoChipRequest { yards: 3 }),
    )
    .await
    .expect_err("refused");
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "Auto-Chip is disabled in settings");
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(
        state
            .auto_chip_shots
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "no number taken"
    );

    state.root.system.update(|c| c.simulation.auto_chip = true);
    let Json(resp) = post_auto_chip(
        State(Arc::clone(&state)),
        Json(AutoChipRequest { yards: 3 }),
    )
    .await
    .expect("accepted once enabled");
    assert_eq!(resp.shot_number, 1);
}

/// Tournament mode's own round refuses a chip with the same text as a
/// simulator's round, also with only a GSPro (Open Connect client) card
/// and no Pureplay card configured.
#[tokio::test]
async fn auto_chip_names_a_tournament_round_in_tournament_mode() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.simulation.auto_chip = true;
        c.pureplay.clear();
        c.openconnect_client.insert(
            "0".into(),
            flighthook::OpenConnectClientSection {
                name: "GSPro".into(),
                ..Default::default()
            },
        );
    });
    state.root.round.set_manual(true);
    let state = Arc::new(state);
    let (status, Json(body)) = post_auto_chip(
        State(Arc::clone(&state)),
        Json(AutoChipRequest { yards: 3 }),
    )
    .await
    .expect_err("refused");
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "Auto-Chip is off during a tournament round");
}

/// While a tournament round is active no chip fires, neither through
/// the route nor through `autochip::send`; once it ends the user's
/// setting applies again.
#[tokio::test]
async fn auto_chip_does_not_fire_during_a_strict_round() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| c.simulation.auto_chip = true);
    let round = state
        .root
        .round
        .begin_round(&crate::state::round::RoundRequest {
            holder: "Pureplay",
            feeds: &[],
        })
        .expect("a round");
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    let (status, Json(body)) = post_auto_chip(
        State(Arc::clone(&state)),
        Json(AutoChipRequest { yards: 3 }),
    )
    .await
    .expect_err("refused");
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "Auto-Chip is off during a tournament round");
    assert_eq!(
        crate::autochip::send(&state.root, &state.bus_tx, 3, &state.auto_chip_shots),
        Err(crate::autochip::SolveError::RoundActive)
    );
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(
        state
            .auto_chip_shots
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "no number taken"
    );

    drop(round);
    let Json(resp) = post_auto_chip(
        State(Arc::clone(&state)),
        Json(AutoChipRequest { yards: 3 }),
    )
    .await
    .expect("fires again once the round ends");
    assert_eq!(resp.shot_number, 1);
}

#[tokio::test]
async fn auto_chip_needs_a_valid_simulation_environment() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| c.simulation.auto_chip = true);
    state
        .root
        .system
        .update(|c| c.simulation.temperature_c = 99.0);
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    let (status, Json(body)) = post_auto_chip(
        State(Arc::clone(&state)),
        Json(AutoChipRequest { yards: 3 }),
    )
    .await
    .expect_err("rejected");
    assert_eq!(status, StatusCode::CONFLICT);
    let error = body["error"].as_str().expect("error");
    assert!(
        error.starts_with("simulation environment invalid"),
        "{error}"
    );
    assert!(rx.try_recv().is_err(), "nothing emitted");

    // The refused chip took no number: the next one sent is #1.
    assert_eq!(
        state
            .auto_chip_shots
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    state
        .root
        .system
        .update(|c| c.simulation.temperature_c = 25.0);
    let Json(resp) = post_auto_chip(
        State(Arc::clone(&state)),
        Json(AutoChipRequest { yards: 3 }),
    )
    .await
    .expect("accepted");
    assert_eq!(resp.shot_number, 1);
}

#[tokio::test]
async fn post_grouping_answers_after_the_outcome() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    // Stand-in for the system actor: apply and answer.
    let mut rx = state.bus_tx.subscribe();
    let responder = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                let msg = rx.recv().await.expect("bus");
                if let BusMsg::Local {
                    event: LocalEvent::ConfigCommand { request_id, action },
                    ..
                } = msg
                {
                    let ConfigAction::SetShotGrouping { mode } = *action else {
                        panic!("unexpected action {action:?}");
                    };
                    state.root.system.update(|c| c.shot_grouping = mode);
                    let _ = state.bus_tx.send(BusMsg::local(
                        "system",
                        LocalEvent::ConfigOutcome {
                            request_id,
                            restarted: Vec::new(),
                            stopped: Vec::new(),
                            started: Vec::new(),
                        },
                    ));
                    return;
                }
            }
        })
    };
    let Json(body) = post_shot_grouping(
        State(Arc::clone(&state)),
        Json(GroupingRequest {
            mode: ShotGrouping::ByClub,
        }),
    )
    .await
    .expect("applied");
    responder.await.expect("responder");
    assert_eq!(body.mode, ShotGrouping::ByClub);
    assert_eq!(
        state.root.system.snapshot().shot_grouping,
        ShotGrouping::ByClub
    );
}

/// POST /api/tournament goes through the system actor's command path
/// and answers the round with the manual flag.
#[tokio::test]
async fn post_tournament_answers_the_round_after_the_outcome() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    let responder = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                let msg = rx.recv().await.expect("bus");
                if let BusMsg::Local {
                    event: event @ LocalEvent::ConfigCommand { .. },
                    ..
                } = msg
                {
                    let root = Arc::clone(&state.root);
                    let bus_tx = state.bus_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let sender = crate::bus::BusSender::new(
                            "system".into(),
                            bus_tx.clone(),
                            Arc::default(),
                        );
                        crate::actors::system::run_config_command(&event, &root, &bus_tx, &sender);
                    })
                    .await
                    .expect("applied");
                    return;
                }
            }
        })
    };
    let Json(round) = post_tournament(
        State(Arc::clone(&state)),
        Json(flighthook::TournamentRequest { on: true }),
    )
    .await
    .expect("applied");
    responder.await.expect("responder");
    assert!(round.active && round.manual && !round.held, "{round:?}");
    assert_eq!(round.holder.as_deref(), Some("Manual"));
    assert!(state.root.system.simulation().tournament);
}

/// An API client's `scope=global` or whole-config save whose body has no
/// `tournament` key leaves tournament mode as it was (only
/// `POST /api/tournament` writes it).
#[tokio::test]
async fn a_settings_save_without_the_key_leaves_tournament_mode() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    state.root.system.update(|c| c.simulation.tournament = true);
    state.root.round.set_manual(true);
    let mut rx = state.bus_tx.subscribe();
    let responder = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            for _ in 0..2 {
                let event = loop {
                    if let BusMsg::Local {
                        event: event @ LocalEvent::ConfigCommand { .. },
                        ..
                    } = rx.recv().await.expect("bus")
                    {
                        break event;
                    }
                };
                let root = Arc::clone(&state.root);
                let bus_tx = state.bus_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let sender =
                        crate::bus::BusSender::new("system".into(), bus_tx.clone(), Arc::default());
                    crate::actors::system::run_config_command(&event, &root, &bus_tx, &sender);
                })
                .await
                .expect("applied");
            }
        })
    };
    let mut body = serde_json::to_value(state.root.system.snapshot()).expect("json");
    body["simulation"]
        .as_object_mut()
        .expect("simulation")
        .remove("tournament");
    let body: FlighthookConfig = serde_json::from_value(body).expect("config");
    assert!(!body.simulation.tournament, "the body has no key");
    for scope in [Some("global"), None] {
        let _saved = save_settings(&state, scope, body.clone(), CONFIG_TIMEOUT)
            .await
            .expect("saved");
        assert!(state.root.system.simulation().tournament, "{scope:?}");
        assert!(state.root.round.state().manual, "{scope:?}");
    }
    responder.await.expect("responder");
}

#[tokio::test]
async fn get_blocks_lists_every_block_open_and_closed_with_shot_counts() {
    let (state, writer, _dir) = test_state();
    let state = Arc::new(state);
    let mut up = super::super::tests::Updater::new(&state);
    up.club(&writer, Club::Iron7).await;
    up.shot(1).await;
    up.club(&writer, Club::Driver).await;
    let Json(listed) = get_blocks(State(Arc::clone(&state))).await;
    assert_eq!(listed.session, 1);
    let seen: Vec<_> = listed
        .blocks
        .iter()
        .map(|b| (b.block.number, b.block.club, b.open, b.shots))
        .collect();
    assert_eq!(
        seen,
        vec![
            (1, Some(Club::Iron7), false, 1),
            (2, Some(Club::Driver), true, 0)
        ]
    );
    let v = serde_json::to_value(&listed.blocks[0]).expect("ser");
    assert_eq!(v["club"], "7I");
    assert_eq!(v["open"], false);
    assert!(v["id"].is_string() && v["started_at"].is_u64(), "{v}");
}

/// `POST /api/blocks` sends `practice.new_block` as flighthook and
/// answers once the practice actor replied; 504 when nothing answers.
#[tokio::test]
async fn post_blocks_answers_after_the_practice_actors_reply() {
    let (state, writer, _dir) = test_state();
    let state = Arc::new(state);
    let sender = crate::bus::BusSender::new("system".into(), state.bus_tx.clone(), Arc::default())
        .with_frp_id(Some(state.root.flighthook_id()));
    let mut rx = state.bus_tx.subscribe();
    let commands = tokio::spawn(async move {
        let (mut practice, _) = crate::state::practice::Practice::start(writer.turn(), 0);
        let mut seen = Vec::new();
        while let Ok(msg) = rx.recv().await {
            if let Some(frp) = msg.as_frp()
                && let flightrelay::FrpBody::PracticeNewBlock(c) = &frp.body
            {
                seen.push((frp.from.clone(), c.new_session));
                crate::actors::system::handle_practice(&msg, &mut practice, &sender);
                return seen;
            }
        }
        seen
    });
    let res = post_blocks_within(
        &state,
        flighthook::NewBlockRequest {
            golfer: None,
            new_session: true,
        },
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        commands.await.expect("responder"),
        vec![(state.root.flighthook_id(), true)]
    );

    let res = post_blocks_within(
        &state,
        flighthook::NewBlockRequest::default(),
        Duration::from_millis(50),
    )
    .await;
    assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
}

/// A body that is no new-block request (an invalid golfer, not JSON) is
/// 400 `bad_request`, and nothing goes on the bus.
#[tokio::test]
async fn post_blocks_with_an_invalid_body_is_400_bad_request() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    for body in [
        &br#"{"golfer":"not an id"}"#[..],
        b"nope",
        br#"{"new_session":3}"#,
    ] {
        let res = post_blocks(
            State(Arc::clone(&state)),
            axum::body::Bytes::from_static(body),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{body:?}");
        let bytes = axum::body::to_bytes(res.into_body(), 1024)
            .await
            .expect("body");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(v["error"], "bad_request");
    }
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn post_grouping_times_out_with_504() {
    let (state, _writer, _dir) = test_state();
    // No system actor answers the command.
    let res = set_shot_grouping(
        &state,
        GroupingRequest {
            mode: ShotGrouping::ByClub,
        },
        Duration::from_millis(50),
    )
    .await;
    assert_eq!(res.err(), Some(StatusCode::GATEWAY_TIMEOUT));
    assert_eq!(
        state.root.system.snapshot().shot_grouping,
        ShotGrouping::BySet
    );
}

#[tokio::test]
async fn settings_save_with_an_unknown_scope_is_400_with_a_json_error() {
    let (state, _writer, _dir) = test_state();
    let mut rx = state.bus_tx.subscribe();
    let body = state.root.system.snapshot();
    let (status, Json(error)) = save_settings(
        &state,
        Some("nosuchtype.0"),
        body,
        Duration::from_millis(50),
    )
    .await
    .expect_err("rejected");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "unknown scope 'nosuchtype.0'");
    assert!(rx.try_recv().is_err(), "nothing emitted");
}

#[tokio::test]
async fn settings_save_that_removes_the_web_server_or_adds_one_is_400() {
    let (state, _writer, _dir) = test_state();
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    assert_eq!(live.webserver.len(), 1);
    let mut without = live.clone();
    without.webserver.clear();
    let mut second = live.clone();
    second.webserver.insert(
        "1".into(),
        flighthook::WebserverSection {
            name: "Web Server".into(),
            bind: "0.0.0.0:5881".into(),
        },
    );
    for (scope, body, message) in [
        (
            Some("webserver.0"),
            without.clone(),
            "the web server can't be removed",
        ),
        (None, without, "the web server can't be removed"),
        (
            Some("webserver.1"),
            second.clone(),
            "flighthook runs one web server; edit the existing one",
        ),
        (
            None,
            second,
            "flighthook runs one web server; edit the existing one",
        ),
    ] {
        let (status, Json(error)) = save_settings(&state, scope, body, Duration::from_millis(50))
            .await
            .expect_err("rejected");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{scope:?}");
        assert_eq!(error["error"], message, "{scope:?}");
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");
}

/// Turning on, or adding as on, a card whose address an enabled card of
/// its type has is `409` with the reason and saves nothing, for the
/// whole config and for the card's scope. A card that is off, or on a
/// free address, saves.
#[tokio::test]
async fn settings_save_that_enables_a_duplicate_address_is_409() {
    let (state, _writer, _dir) = test_state();
    let client = |address: &str, enabled| flighthook::OpenConnectClientSection {
        name: "Open Connect".into(),
        enabled,
        address: Some(address.into()),
        full_monitor: None,
        chipping_monitor: None,
        putting_monitor: None,
        ..flighthook::OpenConnectClientSection::default()
    };
    state.root.system.update(|c| {
        c.openconnect_client
            .insert("0".into(), client("127.0.0.1:921", true));
        c.openconnect_client
            .insert("1".into(), client("localhost:921", false));
    });
    let live = state.root.system.snapshot();
    let mut on = live.clone();
    on.openconnect_client.get_mut("1").unwrap().enabled = true;
    let mut added = live.clone();
    added
        .openconnect_client
        .insert("2".into(), client("127.0.0.1:921", true));
    let mut rx = state.bus_tx.subscribe();
    for (scope, body) in [
        (None, on.clone()),
        (Some("openconnect_client.1"), on),
        (Some("openconnect_client.2"), added.clone()),
        (None, added),
    ] {
        let (status, Json(error)) = save_settings(&state, scope, body, Duration::from_millis(50))
            .await
            .expect_err("refused");
        assert_eq!(status, StatusCode::CONFLICT, "{scope:?}");
        assert_eq!(
            error["error"], "An Open Connect card already uses 127.0.0.1:921",
            "{scope:?}"
        );
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");

    // A card that stays off, and one turned on at a free address, save
    // (here the command times out: no system actor runs).
    let mut free = live.clone();
    free.openconnect_client.get_mut("1").unwrap().address = Some("127.0.0.1:9210".into());
    free.openconnect_client.get_mut("1").unwrap().enabled = true;
    for (scope, body) in [
        (Some("openconnect_client.1"), live.clone()),
        (Some("openconnect_client.1"), free),
    ] {
        let res = save_settings(&state, scope, body, Duration::from_millis(50)).await;
        assert_eq!(res.err().map(|e| e.0), Some(StatusCode::GATEWAY_TIMEOUT));
    }
}

/// A whole save without card ids that routes an output to a Pureplay
/// Accessory card by its actor id is 400.
#[tokio::test]
async fn a_whole_save_without_ids_routing_to_a_pureplay_card_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.pureplay_accessory
            .insert("0".into(), flighthook::PureplayAccessorySection::default());
        c.openconnect_client
            .insert("0".into(), flighthook::OpenConnectClientSection::default());
    });
    let live = state.root.system.snapshot();
    let card_id = live.actor_of_card("pureplay_accessory.0").expect("card id");
    let mut body = live.clone();
    body.pureplay_accessory.get_mut("0").unwrap().actor_id = None;
    body.openconnect_client.get_mut("0").unwrap().actor_id = None;
    body.openconnect_client.get_mut("0").unwrap().full_monitor = Some(card_id);
    let (status, _) = save_settings(&state, None, body, Duration::from_millis(50))
        .await
        .expect_err("refused");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");
}

/// A save that routes an output (Open Connect or Pureplay Connect) to
/// a Pureplay Accessory card, in any mode, whole or scoped to the output, is 400
/// and saves nothing; routing to a launch monitor or "Any" saves, and so
/// does the card's own save.
#[tokio::test]
async fn settings_save_routing_an_output_to_a_pureplay_card_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.pureplay_accessory
            .insert("0".into(), flighthook::PureplayAccessorySection::default());
        c.openconnect_client
            .insert("0".into(), flighthook::OpenConnectClientSection::default());
        c.pureplay
            .insert("0".into(), flighthook::PureplaySection::default());
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    let card = Some("pureplay_accessory.0".to_owned());
    let mut to_card_oc = live.clone();
    to_card_oc
        .openconnect_client
        .get_mut("0")
        .unwrap()
        .putting_monitor
        .clone_from(&card);
    let mut to_card_pp = live.clone();
    to_card_pp.pureplay.get_mut("0").unwrap().full_monitor = card;
    for (scope, body) in [
        (None, to_card_oc.clone()),
        (Some("openconnect_client.0"), to_card_oc),
        (None, to_card_pp.clone()),
        (Some("pureplay.0"), to_card_pp),
    ] {
        let (status, Json(error)) = save_settings(&state, scope, body, Duration::from_millis(50))
            .await
            .expect_err("refused");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{scope:?}");
        assert_eq!(
            error["error"], "An output can't take shots from Pureplay",
            "{scope:?}"
        );
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");

    // Routed to a launch monitor, or the card's own section: saved (the
    // command times out here: no system actor runs).
    let mut to_mock = live.clone();
    to_mock
        .openconnect_client
        .get_mut("0")
        .unwrap()
        .full_monitor = Some("mock_monitor.0".into());
    let mut card_lead = live.clone();
    card_lead
        .pureplay_accessory
        .get_mut("0")
        .unwrap()
        .impact_lead_s = Some(3.0);
    for (scope, body) in [
        (Some("openconnect_client.0"), to_mock),
        (Some("pureplay_accessory.0"), card_lead.clone()),
    ] {
        let res = save_settings(&state, scope, body, Duration::from_millis(50)).await;
        assert_eq!(res.err().map(|e| e.0), Some(StatusCode::GATEWAY_TIMEOUT));
    }
    // The card's impact lead is checked like any launch monitor's.
    card_lead
        .pureplay_accessory
        .get_mut("0")
        .unwrap()
        .impact_lead_s = Some(7.0);
    let (status, _) = save_settings(
        &state,
        Some("pureplay_accessory.0"),
        card_lead,
        Duration::from_millis(50),
    )
    .await
    .expect_err("refused");
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A whole-config or `mevo.N`-scoped save with a `launch_only_spin` below
/// 0, above 1000 or non-finite is 400 and saves nothing; a global save
/// does not write Mevo sections, so it is not checked.
#[tokio::test]
async fn settings_save_with_an_invalid_launch_only_spin_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.mevo
            .insert("0".into(), flighthook::MevoSection::default());
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    for bad in [-1.0, 1000.5, f64::NAN, f64::INFINITY] {
        let mut body = live.clone();
        if let Some(m) = body.mevo.get_mut("0") {
            m.launch_only_spin = bad;
        }
        for scope in [None, Some("mevo.0")] {
            let (status, Json(error)) =
                save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                    .await
                    .expect_err("rejected");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} {scope:?}");
            let error = error["error"].as_str().expect("error");
            assert!(
                error.starts_with("mevo.0: launch_only_spin must be 0 to 1000 rpm per mph"),
                "{error}"
            );
        }
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");

    // In range, including both ends, it goes through to the system actor.
    for ok in [0.0, 100.0, 1000.0] {
        let mut section = live.mevo["0"].clone();
        section.launch_only_spin = ok;
        let action = ConfigAction::UpsertMevo {
            index: "0".into(),
            section,
        };
        assert_eq!(invalid_setting(&action), None, "{ok}");
    }
    let mut body = live.clone();
    if let Some(m) = body.mevo.get_mut("0") {
        m.launch_only_spin = -1.0;
    }
    assert_eq!(
        invalid_setting(&ConfigAction::ReplaceGlobal {
            config: Box::new(body)
        }),
        None
    );
}

/// A save with a launch monitor `impact_lead_s` below 0, above 5 or
/// non-finite is 400 and saves nothing, for every launch monitor type, in
/// a whole-config and a scoped save; 0 and 5 are accepted.
#[tokio::test]
async fn settings_save_with_an_invalid_impact_lead_is_400() {
    let (state, _writer, _dir) = test_state();
    let ids = [
        "mevo.0",
        "r10.0",
        "square.0",
        "exputt.0",
        "mock_monitor.0",
        "openconnect_server.0",
    ];
    state.root.system.update(|c| {
        c.mevo.insert("0".into(), Default::default());
        c.r10.insert("0".into(), Default::default());
        c.square.insert("0".into(), Default::default());
        c.exputt.insert("0".into(), Default::default());
        c.mock_monitor.insert(
            "0".into(),
            flighthook::MockMonitorSection {
                name: "Mock".into(),
                enabled: true,
                actor_id: None,
                impact_lead_s: None,
                driver: Default::default(),
            },
        );
        c.openconnect_server.insert("0".into(), Default::default());
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    let set = |config: &mut FlighthookConfig, id: &str, v: Option<f64>| match id {
        "mevo.0" => config.mevo.get_mut("0").unwrap().impact_lead_s = v,
        "r10.0" => config.r10.get_mut("0").unwrap().impact_lead_s = v,
        "square.0" => config.square.get_mut("0").unwrap().impact_lead_s = v,
        "exputt.0" => config.exputt.get_mut("0").unwrap().impact_lead_s = v,
        "mock_monitor.0" => config.mock_monitor.get_mut("0").unwrap().impact_lead_s = v,
        _ => {
            config
                .openconnect_server
                .get_mut("0")
                .unwrap()
                .impact_lead_s = v
        }
    };
    for id in ids {
        for bad in [-0.1, 5.1, f64::NAN, f64::INFINITY] {
            let mut body = live.clone();
            set(&mut body, id, Some(bad));
            for scope in [None, Some(id)] {
                let (status, Json(error)) =
                    save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                        .await
                        .expect_err("rejected");
                assert_eq!(status, StatusCode::BAD_REQUEST, "{id} {bad} {scope:?}");
                let error = error["error"].as_str().expect("error");
                assert!(
                    error.starts_with(&format!("{id}: impact_lead_s "))
                        && error.ends_with("is outside 0 to 5 s"),
                    "{error}"
                );
            }
        }
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");
    for id in ids {
        for ok in [None, Some(0.0), Some(1.5), Some(5.0)] {
            let mut body = live.clone();
            set(&mut body, id, ok);
            let action = ConfigAction::ReplaceAll {
                config: Box::new(body),
            };
            assert_eq!(invalid_setting(&action), None, "{id} {ok:?}");
        }
    }
}

/// A save with mock driver settings the driver refuses is 400 naming the
/// card and the field, and saves nothing, in a whole-config and a
/// scoped save; the driver's own limits are accepted.
#[tokio::test]
async fn settings_save_with_refused_driver_settings_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.mock_monitor
            .insert("0".into(), flighthook::MockMonitorSection::default());
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    type Field = fn(&mut flighthook::MockConfig) -> &mut f64;
    let fields: [(&str, Field); 4] = [
        ("shot_interval_s", |c| &mut c.shot_interval_s),
        ("arm_delay_s", |c| &mut c.arm_delay_s),
        ("ball_ready_delay_s", |c| &mut c.ball_ready_delay_s),
        ("post_shot_delay_s", |c| &mut c.post_shot_delay_s),
    ];
    for (field, value) in fields {
        for bad in [-1.0, 86_400.5, f64::NAN] {
            let mut body = live.clone();
            *value(&mut body.mock_monitor.get_mut("0").unwrap().driver) = bad;
            for scope in [None, Some("mock_monitor.0")] {
                let (status, Json(error)) =
                    save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                        .await
                        .expect_err("rejected");
                assert_eq!(status, StatusCode::BAD_REQUEST, "{field} {bad} {scope:?}");
                assert_eq!(
                    error["error"],
                    format!("mock_monitor.0: {field}: must be 0 to 86400"),
                    "{scope:?}"
                );
            }
        }
        for ok in [0.0, 86_400.0] {
            let mut body = live.clone();
            *value(&mut body.mock_monitor.get_mut("0").unwrap().driver) = ok;
            let action = ConfigAction::ReplaceAll {
                config: Box::new(body),
            };
            assert_eq!(invalid_driver_setting(&live, &action), None, "{field} {ok}");
        }
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");

    // Saved refused settings: a rename or the on/off switch passes the
    // checks (here the command times out: no system actor runs).
    state.root.system.update(|c| {
        c.mock_monitor.get_mut("0").unwrap().driver.shot_interval_s = -1.0;
    });
    let stored = state.root.system.snapshot();
    let mut renamed = stored.clone();
    renamed.mock_monitor.get_mut("0").unwrap().name = "Renamed".into();
    let mut off = stored.clone();
    off.mock_monitor.get_mut("0").unwrap().enabled = false;
    for body in [renamed, off] {
        for scope in [None, Some("mock_monitor.0")] {
            let res = save_settings(&state, scope, body.clone(), Duration::from_millis(50)).await;
            assert_eq!(res.err().map(|e| e.0), Some(StatusCode::GATEWAY_TIMEOUT));
        }
    }
}

/// A save with an R10 range the driver refuses (0 or less, not finite)
/// is 400 and saves nothing, whole and scoped; a range greater than 0
/// and no range pass.
#[tokio::test]
async fn settings_save_with_a_refused_r10_range_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.r10.insert("0".into(), flighthook::R10Section::default());
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    for bad in [
        flighthook::Distance::Feet(0.0),
        flighthook::Distance::Meters(-1.0),
        flighthook::Distance::Feet(f64::INFINITY),
    ] {
        let mut body = live.clone();
        body.r10.get_mut("0").unwrap().driver.range = Some(bad);
        for scope in [None, Some("r10.0")] {
            let (status, Json(error)) =
                save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                    .await
                    .expect_err("rejected");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?} {scope:?}");
            assert_eq!(
                error["error"],
                "r10.0: range: must be a distance greater than 0"
            );
        }
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");
    for ok in [Some(flighthook::Distance::Feet(7.0)), None] {
        let mut body = live.clone();
        body.r10.get_mut("0").unwrap().driver.range = ok;
        let action = ConfigAction::UpsertR10 {
            index: "0".into(),
            section: body.r10["0"].clone(),
        };
        assert_eq!(invalid_driver_setting(&live, &action), None, "{ok:?}");
    }
}

/// A save that changes a FlightScope setting into one its driver refuses
/// (track % outside 0 to 100, a negative distance, a ball type other
/// than 0 or 1, an address that is not an IP address and port) is
/// refused, whole or scoped, before anything is emitted; a stored
/// refused value still lets a rename through.
#[tokio::test]
async fn settings_save_with_a_refused_mevo_setting_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.mevo
            .insert("0".into(), flighthook::MevoSection::default());
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    type Case = (fn(&mut flighthook::MevoConfig), &'static str);
    let cases: [Case; 4] = [
        (
            |d| d.track_pct = Some(101.0),
            "mevo.0: track_pct: must be 0 to 100",
        ),
        (
            |d| d.tee_height = Some(flighthook::Distance::Inches(-1.0)),
            "mevo.0: tee_height: must be a distance of 0 or more",
        ),
        (
            |d| d.ball_type = Some(3),
            "mevo.0: ball_type: must be 0 or 1",
        ),
        (
            |d| d.address = Some("mevo.local".into()),
            "mevo.0: address: ",
        ),
    ];
    for (set, want) in cases {
        let mut body = live.clone();
        set(&mut body.mevo.get_mut("0").unwrap().driver);
        for scope in [None, Some("mevo.0")] {
            let (status, Json(error)) =
                save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                    .await
                    .expect_err("rejected");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{want} {scope:?}");
            let error = error["error"].as_str().expect("text");
            assert!(error.starts_with(want), "{error}");
        }
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");

    let mut stored = live.clone();
    stored.mevo.get_mut("0").unwrap().driver.track_pct = Some(150.0);
    let mut renamed = stored.mevo["0"].clone();
    renamed.name = "Bay 1".into();
    let action = ConfigAction::UpsertMevo {
        index: "0".into(),
        section: renamed,
    };
    assert_eq!(invalid_driver_setting(&stored, &action), None);
}

/// A save that changes an ExPutt's address into one that is not an IP
/// is refused, whole or scoped, before anything is emitted; blank (find
/// the device) and an IP address pass.
#[tokio::test]
async fn settings_save_with_a_refused_exputt_address_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.exputt
            .insert("0".into(), flighthook::ExPuttSection::default());
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    for bad in ["exputt.local", "192.168.1.50:8889"] {
        let mut body = live.clone();
        body.exputt.get_mut("0").unwrap().driver.address = Some(bad.into());
        for scope in [None, Some("exputt.0")] {
            let (status, Json(error)) =
                save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                    .await
                    .expect_err("rejected");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?} {scope:?}");
            let error = error["error"].as_str().expect("text");
            assert!(error.starts_with("exputt.0: address: "), "{error}");
            assert!(error.contains(bad), "{error}");
        }
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");
    for ok in [Some("192.168.1.50"), Some(" "), None] {
        let mut body = live.clone();
        body.exputt.get_mut("0").unwrap().driver.address = ok.map(str::to_owned);
        let action = ConfigAction::UpsertExPutt {
            index: "0".into(),
            section: body.exputt["0"].clone(),
        };
        assert_eq!(invalid_driver_setting(&live, &action), None, "{ok:?}");
    }
}

/// A save that changes a Trackball setting into one the driver refuses
/// is 400, whole or scoped, naming the card and the key; a stored
/// refused value with a rename passes.
#[tokio::test]
async fn settings_save_with_a_refused_trackball_setting_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.trackball
            .insert("0".into(), flighthook::TrackballSection::default());
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    type Refuse = fn(&mut flighthook::TrackballConfig);
    let refusals: [(Refuse, &str); 3] = [
        (|d| d.config.cpi = 0.0, "trackball.0: cpi: "),
        (
            |d| d.config.thresholds.v_stop = 1.0,
            "trackball.0: thresholds.v_stop: ",
        ),
        (
            |d| d.tuning.backswing_power = 1.5,
            "trackball.0: tuning.backswing_power: ",
        ),
    ];
    for (refuse, prefix) in refusals {
        let mut body = live.clone();
        refuse(&mut body.trackball.get_mut("0").unwrap().driver);
        for scope in [None, Some("trackball.0")] {
            let (status, Json(error)) =
                save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                    .await
                    .expect_err("rejected");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{prefix} {scope:?}");
            let error = error["error"].as_str().expect("text");
            assert!(error.starts_with(prefix), "{error}");
        }
        let mut saved = live.clone();
        refuse(&mut saved.trackball.get_mut("0").unwrap().driver);
        let mut renamed = saved.trackball["0"].clone();
        renamed.name = "Arcade".into();
        let action = ConfigAction::UpsertTrackball {
            index: "0".into(),
            section: renamed,
        };
        assert_eq!(invalid_driver_setting(&saved, &action), None, "{prefix}");
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");
}

/// The Square driver reads its settings leniently (an unknown club or a
/// negative calibration is ignored with a warning when the device
/// connects), so a save never refuses them, whole or scoped.
#[test]
fn square_settings_are_never_refused_by_the_driver() {
    let saved = FlighthookConfig::default();
    let section: flighthook::SquareSection = serde_json::from_value(serde_json::json!({
        "name": "Omni",
        "address": "SquareGolf(CD01)",
        "club": "no such club",
        "advanced_spin": false,
        "discard_non_putting_zero_spin": false,
        "dot_bottom_to_face_centre_mm": { "7I": -3.0, "ZZ": 4.0 },
    }))
    .expect("a Square section");
    assert_eq!(
        section.driver.dot_bottom_to_face_center_mm.get("7I"),
        Some(&-3.0)
    );
    let mut whole = saved.clone();
    whole.square.insert("0".into(), section.clone());
    for action in [
        ConfigAction::ReplaceAll {
            config: Box::new(whole),
        },
        ConfigAction::UpsertSquare {
            index: "0".into(),
            section,
        },
    ] {
        assert_eq!(invalid_driver_setting(&saved, &action), None);
    }
}

/// A card whose saved driver settings are refused still saves its host
/// fields and its on/off switch, whole and scoped, as long as the save
/// keeps the refused driver settings as they are (a NaN read from the
/// file included); a change into other refused settings is refused, one
/// into usable settings is not.
#[test]
fn a_card_with_refused_saved_driver_settings_saves_its_other_fields() {
    for stored in [-1.0, f64::NAN] {
        let mut saved = FlighthookConfig::default();
        let mut section = flighthook::MockMonitorSection::default();
        section.driver.shot_interval_s = stored;
        saved.mock_monitor.insert("0".into(), section);
        let edit = |f: fn(&mut flighthook::MockMonitorSection)| {
            let mut config = saved.clone();
            f(config.mock_monitor.get_mut("0").unwrap());
            config
        };
        let check = |config: FlighthookConfig| {
            let scoped = ConfigAction::UpsertMockMonitor {
                index: "0".into(),
                section: config.mock_monitor["0"].clone(),
            };
            let whole = ConfigAction::ReplaceAll {
                config: Box::new(config),
            };
            let (whole, scoped) = (
                invalid_driver_setting(&saved, &whole),
                invalid_driver_setting(&saved, &scoped),
            );
            assert_eq!(whole, scoped);
            whole
        };
        assert_eq!(check(edit(|s| s.name = "Renamed".into())), None);
        assert_eq!(check(edit(|s| s.enabled = false)), None);
        assert_eq!(check(edit(|s| s.impact_lead_s = Some(1.0))), None);
        assert_eq!(
            check(edit(|s| s.driver.shot_interval_s = -2.0)).as_deref(),
            Some("mock_monitor.0: shot_interval_s: must be 0 to 86400")
        );
        assert_eq!(
            check(edit(|s| s.driver.arm_delay_s = -1.0)).as_deref(),
            Some("mock_monitor.0: shot_interval_s: must be 0 to 86400"),
            "the stored refusal still names its field"
        );
        assert_eq!(check(edit(|s| s.driver.shot_interval_s = 10.0)), None);
    }
    let mut added = FlighthookConfig::default();
    let mut section = flighthook::MockMonitorSection::default();
    section.driver.shot_interval_s = -1.0;
    added.mock_monitor.insert("1".into(), section);
    assert!(
        invalid_driver_setting(
            &FlighthookConfig::default(),
            &ConfigAction::ReplaceAll {
                config: Box::new(added)
            }
        )
        .is_some(),
        "a new card's refused settings are refused"
    );
}

/// A save with a camera `latency_s` below 0, above 10 or non-finite is 400
/// and saves nothing, in a whole-config and a scoped save; 0 and 10 are
/// accepted.
#[tokio::test]
async fn settings_save_with_an_invalid_camera_latency_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.camera.insert(
            "0".into(),
            flighthook::CameraSection {
                camera: flighthook::CameraSection::TEST_PATTERN.into(),
                ..flighthook::CameraSection::default()
            },
        );
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    for bad in [-0.1, 10.1, f64::NAN, f64::INFINITY] {
        let mut body = live.clone();
        body.camera.get_mut("0").unwrap().latency_s = Some(bad);
        for scope in [None, Some("camera.0")] {
            let (status, Json(error)) =
                save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                    .await
                    .expect_err("rejected");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} {scope:?}");
            let error = error["error"].as_str().expect("error");
            assert!(
                error.starts_with("camera.0: latency_s ")
                    && error.ends_with("is outside 0 to 10 s"),
                "{error}"
            );
        }
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");
    for ok in [None, Some(0.0), Some(2.5), Some(10.0)] {
        let mut body = live.clone();
        body.camera.get_mut("0").unwrap().latency_s = ok;
        let action = ConfigAction::ReplaceAll {
            config: Box::new(body.clone()),
        };
        assert_eq!(invalid_setting(&action), None, "{ok:?}");
        let upsert = ConfigAction::UpsertCamera {
            index: "0".into(),
            section: body.camera["0"].clone(),
        };
        assert_eq!(invalid_setting(&upsert), None, "{ok:?}");
    }
}

/// A camera `rotation_deg` other than 0/90/180/270, or a `yaw_deg` or
/// `pitch_deg` outside -30 to 30, is 400 and saves nothing, in a
/// whole-config and a scoped save.
#[tokio::test]
async fn settings_save_with_an_invalid_camera_rotation_or_angle_is_400() {
    let (state, _writer, _dir) = test_state();
    state.root.system.update(|c| {
        c.camera.insert(
            "0".into(),
            flighthook::CameraSection {
                camera: flighthook::CameraSection::TEST_PATTERN.into(),
                ..flighthook::CameraSection::default()
            },
        );
    });
    let mut rx = state.bus_tx.subscribe();
    let live = state.root.system.snapshot();
    type Set = fn(&mut flighthook::CameraSection);
    let cases: [(Set, &str); 4] = [
        (
            |c| c.rotation_deg = 45,
            "camera.0: rotation_deg 45 is not 0, 90, 180 or 270",
        ),
        (
            |c| c.yaw_deg = Some(40.0),
            "camera.0: yaw_deg 40 is outside -30 to 30",
        ),
        (
            |c| c.pitch_deg = Some(-31.0),
            "camera.0: pitch_deg -31 is outside -30 to 30",
        ),
        (
            |c| c.yaw_deg = Some(f64::NAN),
            "camera.0: yaw_deg NaN is outside -30 to 30",
        ),
    ];
    for (set, message) in cases {
        let mut body = live.clone();
        set(body.camera.get_mut("0").unwrap());
        for scope in [None, Some("camera.0")] {
            let (status, Json(error)) =
                save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                    .await
                    .expect_err("rejected");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{message} {scope:?}");
            assert_eq!(error["error"], message);
        }
    }
    assert!(rx.try_recv().is_err(), "nothing emitted");
    assert_eq!(state.root.system.snapshot(), live, "nothing saved");
    for rotation in [0, 90, 180, 270] {
        let mut body = live.clone();
        let camera = body.camera.get_mut("0").unwrap();
        camera.rotation_deg = rotation;
        camera.yaw_deg = Some(-30.0);
        camera.pitch_deg = Some(30.0);
        let upsert = ConfigAction::UpsertCamera {
            index: "0".into(),
            section: body.camera["0"].clone(),
        };
        assert_eq!(invalid_setting(&upsert), None, "{rotation}");
    }
}

/// A new golfer's id is given once: the golfers save answers with the
/// golfers as saved (ids included), and a second save that sends them
/// back keeps the same id.
#[tokio::test]
async fn a_golfers_save_answers_with_the_new_golfers_id() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let mut rx = state.bus_tx.subscribe();
    let system = Arc::clone(&state);
    tokio::spawn(async move {
        let sender =
            crate::bus::BusSender::new("system".into(), system.bus_tx.clone(), Arc::default());
        while let Ok(msg) = rx.recv().await {
            if let Some(event @ LocalEvent::ConfigCommand { .. }) = local_of(&msg) {
                crate::actors::system::run_config_command(
                    event,
                    &system.root,
                    &system.bus_tx,
                    &sender,
                );
            }
        }
    });
    let mut body = state.root.system.snapshot();
    body.golfers = vec![flighthook::GolferRecord {
        name: "Sam".into(),
        ..flighthook::GolferRecord::default()
    }];
    let Ok(Json(first)) =
        save_settings(&state, Some("golfers"), body.clone(), CONFIG_TIMEOUT).await
    else {
        panic!("first save refused");
    };
    let id = first.golfers[0].own_id().expect("issued id").to_owned();
    assert_eq!(state.root.system.snapshot().golfers, first.golfers);
    // The second save carries what the first answered.
    body.golfers.clone_from(&first.golfers);
    body.golfers[0].height = Some(170.0);
    let Ok(Json(second)) = save_settings(&state, Some("golfers"), body, CONFIG_TIMEOUT).await
    else {
        panic!("second save refused");
    };
    assert_eq!(second.golfers[0].own_id(), Some(id.as_str()));
    assert_eq!(second.golfers[0].identifiers.len(), 1);
    // Other saves answer without golfers.
    let global = state.root.system.snapshot();
    let Ok(Json(other)) = save_settings(&state, Some("global"), global, CONFIG_TIMEOUT).await
    else {
        panic!("global save refused");
    };
    assert!(other.golfers.is_empty());
}

#[tokio::test]
async fn a_golfers_save_writes_the_golfers_alone() {
    let (state, _writer, _dir) = test_state();
    let mut rx = state.bus_tx.subscribe();
    let mut body = state.root.system.snapshot();
    body.default_units = flighthook::UnitSystem::Metric;
    body.golfers = vec![flighthook::GolferRecord {
        height: Some(168.0),
        ..flighthook::GolferRecord::new("flighthook.a", "Eric")
    }];
    let _ = save_settings(
        &state,
        Some("golfers"),
        body.clone(),
        Duration::from_millis(10),
    )
    .await;
    let msg = next_event(&mut rx);
    let Some(LocalEvent::ConfigCommand { action, .. }) = local_of(&msg) else {
        panic!("not a config command: {msg:?}");
    };
    assert!(matches!(
        action.as_ref(),
        ConfigAction::SetGolfers { golfers } if *golfers == body.golfers
    ));
    let before = state.root.system.snapshot();
    let sender = crate::bus::BusSender::new("system".into(), state.bus_tx.clone(), Arc::default());
    crate::actors::system::run_config_command(
        local_of(&msg).expect("local"),
        &state.root,
        &state.bus_tx,
        &sender,
    );
    let after = state.root.system.snapshot();
    assert_eq!(after.golfers, body.golfers);
    assert_eq!(after.default_units, before.default_units);
    // A height out of range is refused, here and in a whole save.
    body.golfers[0].height = Some(99.0);
    for scope in [Some("golfers"), None] {
        let (status, Json(error)) =
            save_settings(&state, scope, body.clone(), Duration::from_millis(10))
                .await
                .expect_err("refused");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error["error"],
            "golfer Eric: height 99 is outside 100 to 250 cm"
        );
    }
}

/// A save that puts one golfer id in two golfers is refused, naming
/// both, and nothing is written.
#[tokio::test]
async fn a_duplicate_golfer_id_is_refused() {
    let (state, _writer, _dir) = test_state();
    let mut body = state.root.system.snapshot();
    let mut a = flighthook::GolferRecord::new("flighthook.a", "Eric");
    a.identifiers.push("pureplay.local-3".into());
    let mut b = flighthook::GolferRecord::new("flighthook.b", "Sam");
    b.identifiers.push("pureplay.local-3".into());
    body.golfers = vec![a, b];
    for scope in [Some("golfers"), None] {
        let (status, Json(error)) =
            save_settings(&state, scope, body.clone(), Duration::from_millis(10))
                .await
                .expect_err("refused");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error["error"],
            "'pureplay.local-3' is in two golfers: Eric and Sam"
        );
    }
    assert!(state.root.system.snapshot().golfers.is_empty());
}

/// A clip is graded with the facts of the golfer who hit (the clip's
/// golfer, by the lookup rule), not the golfer up's; a clip with no
/// golfer with none, as its swing report is measured.
#[tokio::test]
async fn a_clip_is_graded_with_its_own_golfers_facts() {
    let (state, writer, _dir) = test_state();
    let mut a = flighthook::GolferRecord::new("flighthook.a", "Eric");
    a.identifiers.push("pureplay.local-2".into());
    a.height = Some(180.0);
    a.sex = Some(flighthook::Sex::Male);
    let mut b = flighthook::GolferRecord::new("flighthook.b", "Sam");
    b.height = Some(165.0);
    b.sex = Some(flighthook::Sex::Female);
    state.root.system.update(|c| {
        c.golfers = vec![a, b];
        c.selected_golfer = Some("flighthook.b".into());
    });
    let facts = |clip: Option<&str>| crate::pose::clips::clip_golfer_facts(&state.root, clip);
    let eric = facts(Some("pureplay.local-2"));
    assert_eq!(
        (eric.height, eric.sex),
        (Some(180.0), flighthook::Sex::Male)
    );
    writer.apply(&flightrelay::GolferSelect {
        golfer: Some(Some("flighthook.b".parse().expect("id"))),
        ..flightrelay::GolferSelect::new("test")
    });
    assert_eq!(state.root.golfer_up_facts().height, Some(165.0));
    let none = facts(None);
    assert_eq!(
        none.height, None,
        "no golfer: no height, as the swing report"
    );
}

/// A golfer with no name, or an identifier that is not a
/// golfer id (`namespace.id`, the namespace lowercase), is refused,
/// here and in a whole save, and nothing is written.
#[tokio::test]
async fn a_golfer_without_a_name_or_with_a_bad_identifier_is_refused() {
    let (state, _writer, _dir) = test_state();
    let mut body = state.root.system.snapshot();
    let cases = [
        ("  ", vec!["flighthook.a"], "golfer 1 has no name"),
        (
            "Eric",
            vec!["flighthook.a", "nodot"],
            "golfer Eric: 'nodot' is not a golfer id",
        ),
        (
            "Eric",
            vec!["flighthook.a", "Pureplay.X"],
            "golfer Eric: 'Pureplay.X' is not a golfer id",
        ),
    ];
    for (name, ids, expected) in cases {
        let mut golfer = flighthook::GolferRecord::new("flighthook.a", name);
        golfer.identifiers = ids.into_iter().map(str::to_owned).collect();
        body.golfers = vec![golfer];
        for scope in [Some("golfers"), None] {
            let (status, Json(error)) =
                save_settings(&state, scope, body.clone(), Duration::from_millis(10))
                    .await
                    .expect_err("refused");
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error["error"], expected);
        }
    }
    assert!(state.root.system.snapshot().golfers.is_empty());
}

/// An old UI's body tracking save that still carries `golfer_height`
/// is accepted, and the height is not written back into the section.
#[tokio::test]
async fn a_body_tracking_save_with_a_golfer_height_drops_it() {
    let (state, _writer, dir) = test_state();
    let mut rx = state.bus_tx.subscribe();
    let body: FlighthookConfig =
        serde_json::from_str(r#"{"body_tracking":{"mode":"historical","golfer_height":182.5}}"#)
            .expect("body");
    let _ = save_settings(
        &state,
        Some("body_tracking"),
        body,
        Duration::from_millis(10),
    )
    .await;
    let msg = next_event(&mut rx);
    let sender = crate::bus::BusSender::new("system".into(), state.bus_tx.clone(), Arc::default());
    crate::actors::system::run_config_command(
        local_of(&msg).expect("local"),
        &state.root,
        &state.bus_tx,
        &sender,
    );
    let after = state.root.system.snapshot();
    assert_eq!(
        after.body_tracking.mode,
        flighthook::BodyTrackingMode::Historical
    );
    assert!(after.golfers.is_empty(), "only [body_tracking] is written");
    let text = std::fs::read_to_string(dir.0.join("config.toml")).expect("config");
    assert!(!text.contains("golfer_height"), "{text}");
}

#[tokio::test]
async fn settings_save_without_an_outcome_is_504_with_a_json_error() {
    let (state, _writer, _dir) = test_state();
    // No system actor answers the command.
    let body = state.root.system.snapshot();
    for scope in [None, Some("global"), Some("webserver.0")] {
        let (status, Json(error)) =
            save_settings(&state, scope, body.clone(), Duration::from_millis(50))
                .await
                .expect_err("timed out");
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{scope:?}");
        assert!(error["error"].is_string(), "{scope:?}");
    }
}

/// A client still on the `gspro` spelling: body section and scope.
#[tokio::test]
async fn settings_save_accepts_the_gspro_spelling() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    // Stand in for the system actor: apply every config command.
    let mut rx = state.bus_tx.subscribe();
    let (root, bus_tx) = (Arc::clone(&state.root), state.bus_tx.clone());
    let system = tokio::spawn(async move {
        let sender = crate::bus::BusSender::new("system".into(), bus_tx.clone(), Arc::default());
        let mut actions = Vec::new();
        while actions.len() < 2 {
            let msg = rx.recv().await.expect("bus open");
            if let Some(event @ LocalEvent::ConfigCommand { action, .. }) = local_of(&msg) {
                actions.push((**action).clone());
                crate::actors::system::run_config_command(event, &root, &bus_tx, &sender);
            }
        }
        actions
    });
    let post = |body: &str, scope: &str| {
        // As the `Json` extractor reads it.
        let body: FlighthookConfig = serde_json::from_str(body).expect("body parses");
        post_settings(
            State(Arc::clone(&state)),
            Query(SettingsQuery {
                scope: Some(scope.into()),
            }),
            Json(body),
        )
    };

    // An address no actor can start on, so nothing dials out.
    let upsert = r#"{"gspro":{"0":{"name":"Muni Golf","address":"no address"}}}"#;
    let _ = post(upsert, "gspro.0").await.expect("saved");
    let saved = state.root.system.snapshot();
    assert_eq!(saved.openconnect_client["0"].name, "Muni Golf");

    let _ = post("{}", "gspro.0").await.expect("removed");
    assert!(state.root.system.snapshot().openconnect_client.is_empty());

    let actions = system.await.expect("system task");
    assert!(matches!(
        &actions[0],
        ConfigAction::UpsertOpenConnectClient { index, .. } if index == "0"
    ));
    assert!(matches!(
        &actions[1],
        ConfigAction::Remove { id } if id == "openconnect_client.0"
    ));
}

#[tokio::test]
async fn a_rename_shows_in_the_status_without_a_restart() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    // The system actor: apply each config command and keep its outcome.
    let mut rx = state.bus_tx.subscribe();
    let (root, bus_tx) = (Arc::clone(&state.root), state.bus_tx.clone());
    let system = tokio::spawn(async move {
        let sender = crate::bus::BusSender::new("system".into(), bus_tx.clone(), Arc::default());
        let mut outcomes = Vec::new();
        while outcomes.len() < 2 {
            let msg = rx.recv().await.expect("bus open");
            match local_of(&msg) {
                Some(event @ LocalEvent::ConfigCommand { .. }) => {
                    crate::actors::system::run_config_command(event, &root, &bus_tx, &sender);
                }
                Some(event @ LocalEvent::ConfigOutcome { .. }) => outcomes.push(event.clone()),
                _ => {}
            }
        }
        outcomes
    });
    let post = |name: &str| {
        let body: FlighthookConfig = serde_json::from_str(&format!(
            r#"{{"mock_monitor":{{"0":{{"name":"{name}"}}}}}}"#
        ))
        .expect("body parses");
        post_settings(
            State(Arc::clone(&state)),
            Query(SettingsQuery {
                scope: Some("mock_monitor.0".into()),
            }),
            Json(body),
        )
    };
    let shown = |status: &StatusResponse| status.cards["mock_monitor.0"].name.clone();

    let _ = post("Bay Mock").await.expect("added");
    let outcome = |started: &[&str]| LocalEvent::ConfigOutcome {
        request_id: None,
        restarted: Vec::new(),
        stopped: Vec::new(),
        started: started.iter().map(|s| (*s).to_owned()).collect(),
    };
    feed_local(&state, "system", vec![outcome(&["mock_monitor.0"])]).await;
    let before = get_status(State(Arc::clone(&state))).await.0;
    assert_eq!(shown(&before), "Bay Mock");

    // Renamed: nothing restarts, and the status has the new name before
    // the web layer has seen the outcome.
    let Json(reply) = post("Range Mock").await.expect("renamed");
    assert!(reply.restarted.is_empty() && reply.stopped.is_empty());
    let now = get_status(State(Arc::clone(&state))).await.0;
    assert_eq!(shown(&now), "Range Mock");
    assert_eq!(
        now.cards["mock_monitor.0"].state,
        before.cards["mock_monitor.0"].state
    );

    let outcomes = system.await.expect("system task");
    assert!(matches!(
        &outcomes[1],
        LocalEvent::ConfigOutcome { restarted, stopped, started, .. }
            if restarted.is_empty() && stopped.is_empty() && started.is_empty()
    ));
    // The web layer's own copy takes the name from the outcome.
    feed_local(&state, "system", vec![outcomes[1].clone()]).await;
    assert_eq!(
        state.actors.read().await["mock_monitor.0"].name,
        "Range Mock"
    );
    state.root.stop_actor("mock_monitor.0");
}

#[tokio::test]
async fn settings_save_with_the_global_scope_sends_replace_global() {
    let (state, _writer, _dir) = test_state();
    let mut rx = state.bus_tx.subscribe();
    let body = state.root.system.snapshot();
    let _ = save_settings(&state, Some("global"), body, Duration::from_millis(10)).await;
    let msg = next_event(&mut rx);
    let Some(LocalEvent::ConfigCommand { action, .. }) = local_of(&msg) else {
        panic!("not a config command: {msg:?}");
    };
    assert!(matches!(**action, ConfigAction::ReplaceGlobal { .. }));
}

#[tokio::test]
async fn a_body_tracking_save_writes_that_section_alone() {
    let (state, _writer, _dir) = test_state();
    let mut rx = state.bus_tx.subscribe();
    // The UI's copy is out of date on everything else.
    let mut body = state.root.system.snapshot();
    body.default_units = flighthook::UnitSystem::Metric;
    body.replay.keep_shots = 5;
    body.body_tracking.min_score = 0.55;
    let _ = save_settings(
        &state,
        Some("body_tracking"),
        body.clone(),
        Duration::from_millis(10),
    )
    .await;
    let msg = next_event(&mut rx);
    let Some(LocalEvent::ConfigCommand { action, .. }) = local_of(&msg) else {
        panic!("not a config command: {msg:?}");
    };
    assert!(matches!(
        action.as_ref(),
        ConfigAction::SetBodyTracking { section } if *section == body.body_tracking
    ));
    // Applied: body tracking changes, nothing else does.
    let before = state.root.system.snapshot();
    let sender = crate::bus::BusSender::new("system".into(), state.bus_tx.clone(), Arc::default());
    crate::actors::system::run_config_command(
        local_of(&msg).expect("local"),
        &state.root,
        &state.bus_tx,
        &sender,
    );
    let after = state.root.system.snapshot();
    assert_eq!(after.body_tracking, body.body_tracking);
    assert_eq!(after.default_units, before.default_units);
    assert_eq!(after.replay, before.replay);
    // A value out of range is refused, nothing sent.
    body.body_tracking.min_score = 2.0;
    let (status, Json(error)) = save_settings(
        &state,
        Some("body_tracking"),
        body,
        Duration::from_millis(10),
    )
    .await
    .expect_err("refused");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("body_tracking: min_score")),
        "{error}"
    );
}

#[tokio::test]
async fn clear_endpoint_emits_its_command_and_the_history_and_export_empty() {
    let (state, writer, _dir) = test_state();
    let state = Arc::new(state);
    let _ = super::super::tests::turn_to(&state.root, &writer, Club::Iron7);
    let key = ShotKey {
        shot_id: "s1".into(),
        shot_number: 1,
    };
    feed(
        &state,
        "mevo.0",
        vec![
            turn_event(Club::Iron7),
            Ev::trigger(key.clone()),
            Ev::frp(flightrelay::ShotLaunch {
                key: key.clone(),
                launch: Launch {
                    launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                    ..Launch::default()
                },
            }),
            Ev::Finished(key, None),
        ],
    )
    .await;
    let export = |format: &str| {
        get_shots_export(
            State(Arc::clone(&state)),
            Query(ExportQuery {
                format: Some(format.into()),
                units: None,
                limit: None,
                raw: None,
            }),
        )
    };
    assert_eq!(body_text(export("jsonl").await).await.lines().count(), 1);

    let mut rx = state.bus_tx.subscribe();
    assert_eq!(
        post_clear_shot_history(State(Arc::clone(&state))).await,
        StatusCode::ACCEPTED
    );
    let msg = next_event(&mut rx);
    assert!(matches!(local_of(&msg), Some(LocalEvent::ClearShotHistory)));
    // The UI socket carries it as a local event.
    let mut ui = state.ui_out.subscribe();
    super::super::ui_socket::follow(&state, &msg);
    assert_eq!(
        serde_json::to_string(&*ui.try_recv().expect("ui message")).expect("ser"),
        r#"{"local":{"card":"web","event":{"kind":"clear_shot_history"}}}"#
    );
    assert!(rx.try_recv().is_err(), "one event");

    // The web layer applies the command it put on the bus.
    let mut pending = super::super::PendingShots::new();
    super::super::apply_bus_event(&state, &msg, &mut pending).await;
    let shots = get_shots(
        State(Arc::clone(&state)),
        Query(ShotsQuery {
            limit: 50,
            units: None,
        }),
    )
    .await
    .0;
    assert!(shots.is_empty());
    let blocks = get_blocks(State(Arc::clone(&state))).await.0;
    assert!(blocks.blocks.is_empty());
    assert_eq!(blocks.session, 1);
    assert_eq!(body_text(export("jsonl").await).await, "");
    let csv = body_text(export("csv").await).await;
    assert_eq!(csv.lines().count(), 1, "the header only: {csv}");
}

#[tokio::test]
async fn get_blocks_lists_every_block_with_its_shots_and_changes_none() {
    let (state, writer, _dir) = test_state();
    let state = Arc::new(state);
    let mut up = super::super::tests::Updater::new(&state);
    up.club(&writer, Club::Iron7).await;
    up.shot(1).await;
    let before = get_blocks(State(Arc::clone(&state))).await.0;
    assert_eq!(before.session, 1);
    assert_eq!(before.blocks.len(), 1);
    assert_eq!((before.blocks[0].open, before.blocks[0].shots), (true, 1));

    // Reading changes nothing on the bus.
    let mut rx = state.bus_tx.subscribe();
    let again = get_blocks(State(Arc::clone(&state))).await.0;
    assert_eq!(again, before);
    assert!(rx.try_recv().is_err(), "no block closed or created");

    up.new_block(true).await;
    let after = get_blocks(State(Arc::clone(&state))).await.0;
    assert_eq!(after.session, 2);
    assert_eq!(after.blocks.len(), 2);
    assert!(!after.blocks[0].open && after.blocks[0].block.session == 1);
    assert_eq!(after.blocks[0].shots, 1);
    assert!(after.blocks[1].open && after.blocks[1].block.session == 2);
}

#[tokio::test]
async fn get_status_says_where_clips_go_and_whether_cameras_work() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let Json(status) = get_status(State(Arc::clone(&state))).await;
    assert_eq!(
        status.clip_dir.as_deref(),
        Some(
            crate::state::clips::default_dir()
                .display()
                .to_string()
                .as_str()
        )
    );
    assert_eq!(status.cameras_supported, crate::state::cameras::supported());
    assert_eq!(status.trackball_supported, cfg!(target_os = "linux"));
    state
        .root
        .system
        .update(|c| c.replay.clip_dir = Some("/srv/swings".into()));
    let Json(status) = get_status(State(Arc::clone(&state))).await;
    assert_eq!(status.clip_dir.as_deref(), Some("/srv/swings"));
}

/// A web state over a config file holding `contents`.
fn state_over(contents: &str) -> (WebState, std::path::PathBuf, TempDir) {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-web-config-test-{}",
        crate::state::config::generate_id()
    )));
    std::fs::create_dir_all(&dir.0).expect("mkdir");
    let path = dir.0.join("config.toml");
    std::fs::write(&path, contents).expect("write");
    let (root, _writer) = crate::state::SystemState::new(path.clone());
    let (bus_tx, _) = broadcast::channel(1024);
    let state = web_state(Arc::new(root), bus_tx, "webserver.0");
    (state, path, dir)
}

/// The status carries the round while one is active, with its holder,
/// and leaves it out otherwise.
#[tokio::test]
async fn get_status_carries_the_round_while_one_is_active() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let json = serde_json::to_value(get_status(State(Arc::clone(&state))).await.0).expect("ser");
    assert!(json.get("round").is_none(), "{json}");
    let round = state
        .root
        .round
        .begin_round(&crate::state::round::RoundRequest {
            holder: "Pureplay",
            feeds: &[],
        })
        .expect("a round");
    let json = serde_json::to_value(get_status(State(Arc::clone(&state))).await.0).expect("ser");
    assert_eq!(
        json["round"],
        serde_json::json!({"active": true, "holder": "Pureplay", "held": true})
    );
    // Tournament mode underneath: both flags.
    state.root.round.set_manual(true);
    let json = serde_json::to_value(get_status(State(Arc::clone(&state))).await.0).expect("ser");
    assert_eq!(
        json["round"],
        serde_json::json!({"active": true, "holder": "Pureplay", "held": true, "manual": true})
    );
    state.root.round.set_manual(false);
    drop(round);
    assert!(!get_status(State(state)).await.0.round.active);
}

#[tokio::test]
async fn get_status_carries_the_config_error_until_a_save_backs_the_file_up() {
    let (state, _writer, _dir) = test_state();
    let status = get_status(State(Arc::new(state))).await.0;
    assert_eq!(status.config_error, None);
    assert!(
        !serde_json::to_string(&status)
            .expect("ser")
            .contains("config_error")
    );

    let broken = "[body_tracking]\nmode = \"liv\"\n";
    let (state, path, dir) = state_over(broken);
    let state = Arc::new(state);
    let status = get_status(State(Arc::clone(&state))).await.0;
    let error = status.config_error.expect("the config error");
    assert_eq!(error.path, path.display().to_string());
    assert!(error.error.starts_with("line 2, column 8: "), "{error:?}");
    let json = serde_json::to_value(get_status(State(Arc::clone(&state))).await.0).expect("ser");
    assert_eq!(json["config_error"]["path"], error.path);

    // No system actor here: the save times out, after the backup.
    let saved = save_settings(
        &state,
        Some("global"),
        state.root.system.snapshot(),
        Duration::from_millis(50),
    )
    .await;
    assert_eq!(saved.err().map(|e| e.0), Some(StatusCode::GATEWAY_TIMEOUT));
    assert_eq!(get_status(State(state)).await.0.config_error, None);
    let backup = dir
        .0
        .join(format!("config.pre-{}.toml", env!("CARGO_PKG_VERSION")));
    assert_eq!(std::fs::read_to_string(backup).expect("backup"), broken);
}

#[tokio::test]
async fn a_save_is_refused_when_the_unusable_file_cannot_be_backed_up() {
    let (_state, _path, dir) = state_over("");
    // A directory in the file's place cannot be read, so not copied.
    let folder = dir.0.join("folder.toml");
    std::fs::create_dir(&folder).expect("mkdir");
    let (root, _writer) = crate::state::SystemState::new(folder.clone());
    let (bus_tx, _) = broadcast::channel(1024);
    let state = web_state(Arc::new(root), bus_tx, "webserver.0");
    let saved = save_settings(
        &state,
        Some("global"),
        state.root.system.snapshot(),
        Duration::from_millis(50),
    )
    .await;
    let Err(refused) = saved else {
        panic!("the save was not refused");
    };
    assert_eq!(refused.0, StatusCode::INTERNAL_SERVER_ERROR);
    let message = refused.1.0["error"].as_str().expect("message").to_string();
    assert!(message.contains("cannot be backed up"), "{message}");
    assert!(message.contains("folder.toml"), "{message}");
    assert!(state.root.system.load_error().is_some());
    assert!(folder.is_dir());
}

#[tokio::test]
async fn get_status_carries_the_turn() {
    let (state, writer, _dir) = test_state();
    let state = Arc::new(state);
    let none = get_status(State(Arc::clone(&state))).await.0;
    assert_eq!(none.turn.as_ref().and_then(|t| t.club), None);
    let json = serde_json::to_value(&none).expect("ser");
    assert!(json["turn"]["club"].is_null(), "{json}");
    let _ = super::super::tests::turn_to(&state.root, &writer, Club::Iron7);
    let set = get_status(State(Arc::clone(&state))).await.0;
    assert_eq!(set.turn.and_then(|t| t.club), Some(Club::Iron7));
}

/// `actors` lists every FRP actor on the bus with its local card key.
#[tokio::test]
async fn get_status_lists_the_bus_actors_with_their_card() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let me = state.root.flighthook_id();
    let mut actor = flightrelay::BusActor::new("flighthook", 2.0);
    actor.roles = vec!["session".into(), "practice".into()];
    super::super::ui_socket::follow(
        &state,
        &BusMsg::frp(flightrelay::FrpMessage::new(me.clone(), actor)),
    );
    super::super::ui_socket::follow(
        &state,
        &BusMsg::frp(flightrelay::FrpMessage::new(
            "remote1",
            flightrelay::BusActor::new("Sim", 2.0),
        )),
    );
    let status = get_status(State(Arc::clone(&state))).await.0;
    assert_eq!(status.actors[&me].name, "flighthook");
    assert_eq!(status.actors[&me].roles, vec!["session", "practice"]);
    assert_eq!(status.actors[&me].card.as_deref(), Some("system"));
    assert_eq!(status.actors["remote1"].card, None);
}

#[tokio::test]
async fn get_status_carries_the_hand() {
    let (state, writer, _dir) = test_state();
    let state = Arc::new(state);
    // The session starts right-handed.
    let start = get_status(State(Arc::clone(&state))).await.0;
    assert_eq!(
        start.turn.and_then(|t| t.handedness),
        Some(Handedness::Right)
    );
    writer.apply(&GolferSelect {
        handedness: Some(Handedness::Left),
        ..GolferSelect::new("test")
    });
    let set = get_status(State(Arc::clone(&state))).await.0;
    assert_eq!(
        set.turn.as_ref().and_then(|t| t.handedness),
        Some(Handedness::Left)
    );
    assert!(
        serde_json::to_string(&set)
            .expect("ser")
            .contains(r#""handedness":"lh""#)
    );
}
