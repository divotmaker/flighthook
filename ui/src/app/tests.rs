use super::*;

/// flighthook's `golfer.turn` on the UI socket (the app's
/// `flighthook_id` is empty until a snapshot names it).
fn turn_msg(
    club: Option<Club>,
    handedness: Option<Handedness>,
    mode: Option<flighthook::Mode>,
) -> flighthook::UiMessage {
    flighthook::UiMessage::Frp(Box::new(flighthook::BusMessage::new(
        "",
        flighthook::GolferTurn {
            golfer: None,
            club,
            handedness,
            mode,
        },
    )))
}

/// One pass of the main content on a 1600×900 screen. Returns the
/// marked controls.
fn content_marks(ctx: &egui::Context, app: &mut FlighthookApp) -> crate::panels::devices::Marks {
    use crate::panels::devices::Marks;
    ctx.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 900.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.render_content(ui));
    });
    ctx.data(|d| d.get_temp::<Marks>(egui::Id::NULL))
        .unwrap_or_default()
}

#[test]
fn a_config_error_shows_in_a_frame_on_every_tab_until_the_status_drops_it() {
    let ctx = egui::Context::default();
    let mut app = FlighthookApp::with_pending(net::new_pending());
    assert!(!content_marks(&ctx, &mut app).contains_key("config_error"));

    let status = |config_error| crate::types::StatusResponse {
        cards: HashMap::new(),
        actors: HashMap::new(),
        turn: None,
        cameras_supported: false,
        trackball_supported: false,
        clip_dir: None,
        config_error,
    };
    let error = crate::types::ConfigLoadError {
        path: "/home/golfer/.config/flighthook/config.toml".into(),
        error: "line 4, column 8: unknown variant `liv`".into(),
    };
    app.pending.lock().unwrap().status = Some(status(Some(error.clone())));
    app.apply_pending();
    assert_eq!(app.config_error.as_ref(), Some(&error));
    for tab in [Tab::Devices, Tab::Shots, Tab::Log] {
        app.active_tab = tab;
        let marks = content_marks(&ctx, &mut app);
        let (rect, _) = marks["config_error"];
        assert!(
            rect.width() > 1200.0,
            "{tab:?}: across the content {rect:?}"
        );
        assert!(rect.min.y < 40.0, "{tab:?}: at the top {rect:?}");
    }

    app.pending.lock().unwrap().status = Some(status(None));
    app.apply_pending();
    assert!(!content_marks(&ctx, &mut app).contains_key("config_error"));
}

/// A `/api/blocks` reply listing `(id, session, club, open, shots)`.
fn blocks(
    session: u32,
    sets: &[(&str, u32, Option<Club>, bool, u32)],
) -> flighthook::BlocksResponse {
    flighthook::BlocksResponse {
        session,
        blocks: sets
            .iter()
            .enumerate()
            .map(
                |(i, &(id, session, club, open, shots))| flighthook::BlockInfo {
                    block: flighthook::PracticeBlock {
                        id: id.into(),
                        golfer: None,
                        club,
                        number: u32::try_from(i).unwrap_or(0) + 1,
                        session,
                        started_at: 1_793_000_000_000,
                    },
                    open,
                    shots,
                    first_shot_at: None,
                },
            )
            .collect(),
    }
}

#[test]
fn the_view_follows_the_saved_settings_and_a_pick_saves_it() {
    use crate::types::ShotGrouping;
    let mut app = FlighthookApp::with_pending(net::new_pending());
    let config = |mode| crate::types::FlighthookConfig {
        shot_grouping: mode,
        ..crate::types::FlighthookConfig::default()
    };
    app.pending.lock().expect("lock").settings = Some(config(ShotGrouping::ByClub));
    app.apply_pending();
    assert_eq!(app.groups.mode, ShotGrouping::ByClub);
    // A sets reply leaves the view alone.
    app.pending.lock().expect("lock").groups = Some(blocks(1, &[]));
    app.apply_pending();
    assert_eq!(app.groups.mode, ShotGrouping::ByClub);
    // Another client's save reloads the settings, and the view follows.
    app.pending.lock().expect("lock").settings = Some(config(ShotGrouping::None));
    app.apply_pending();
    assert_eq!(app.groups.mode, ShotGrouping::None);
    assert_eq!(app.settings.shot_grouping, ShotGrouping::None);
}

/// By set and By club from `/api/blocks`: sets in session and number
/// order, the session from the reply, the open set the next shot joins.
#[test]
fn the_sets_come_from_the_blocks_reply() {
    use crate::types::ShotGrouping;
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.pending.lock().expect("lock").groups = Some(blocks(
        2,
        &[
            ("a", 1, Some(Club::Iron7), false, 3),
            ("b", 2, Some(Club::Iron7), false, 2),
            ("c", 2, Some(Club::Driver), true, 1),
        ],
    ));
    let rev = app.groups.rev();
    app.apply_pending();
    assert!(app.groups.rev() > rev);
    assert_eq!(app.groups.session(), 2);
    let ids: Vec<&str> = app.groups.iter_sets().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "c"]);
    assert_eq!(app.groups.open_set().map(|g| g.id.as_str()), Some("c"));
    assert_eq!(
        app.groups
            .current_group(Some(Club::Driver))
            .map(|g| g.id.as_str()),
        Some("c")
    );
    let b = app.groups.get("b").expect("b");
    assert_eq!(
        (b.session, b.club, b.shots, b.open),
        (2, Some(Club::Iron7), 2, false)
    );
    for mode in [ShotGrouping::BySet, ShotGrouping::ByClub] {
        app.groups.mode = mode;
        let strip = app.groups.strip(Some(Club::Driver));
        assert!(
            strip.actions && strip.new_group && strip.new_session,
            "{mode:?}"
        );
    }
}

#[test]
fn a_practice_block_fetches_the_sets_again() {
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.flighthook_id = "me".into();
    let block = |session| flighthook::PracticeBlock {
        id: "b1".into(),
        golfer: None,
        club: Some(Club::Iron7),
        session,
        number: 1,
        started_at: 0,
    };
    crate::socket::feed::frp(&mut app, "other", block(1));
    assert!(!app.needs_groups_refresh);
    crate::socket::feed::frp(&mut app, "me", block(2));
    assert!(app.needs_groups_refresh);
    app.needs_groups_refresh = false;
    crate::socket::feed::frp(
        &mut app,
        "me",
        flighthook::Removal {
            kind: flighthook::StateKind::PracticeBlock,
            id: Some("b1".into()),
        },
    );
    assert!(app.needs_groups_refresh);
}

/// Without the WebSocket, the hand follows the status replies; with it,
/// the session's turn is the source and the reply's hand is ignored.
#[test]
fn the_hand_follows_status_replies_and_hand_info() {
    let status = |handed: Option<flighthook::Handedness>| crate::types::StatusResponse {
        cards: HashMap::new(),
        actors: HashMap::new(),
        turn: handed.map(|h| flighthook::GolferTurn::new(None, None, h, flighthook::Mode::Full)),
        cameras_supported: false,
        trackball_supported: false,
        clip_dir: None,
        config_error: None,
    };
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Left)));
    app.apply_pending();
    assert_eq!(app.handed, Some(Handedness::Left));

    app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Right)));
    app.apply_pending();
    assert_eq!(app.handed, Some(Handedness::Right));

    // With the WebSocket up the reply does not move it; the session's
    // turn does.
    app.ws_connected = true;
    app.pending.lock().expect("lock").status = Some(status(Some(Handedness::Left)));
    app.apply_pending();
    assert_eq!(app.handed, Some(Handedness::Right));
    crate::socket::feed::frp(
        &mut app,
        "",
        flighthook::GolferTurn::new(None, None, Handedness::Left, flighthook::Mode::Full),
    );
    assert_eq!(app.handed, Some(Handedness::Left));
}

#[test]
fn hand_codes_are_rh_and_lh() {
    assert_eq!(
        [Handedness::Right, Handedness::Left].map(hand_code),
        ["RH", "LH"]
    );
}

#[test]
fn a_status_reply_keeps_the_red_and_live_statuses_carry_it_on() {
    use crate::panels::settings_alert;
    use crate::types::{ConnectionState, RECONNECT_ALERT_MS};
    let status = |status| crate::socket::feed::card_msg("openconnect_server.0", status, &[]);
    let mut app = FlighthookApp::with_pending(net::new_pending());
    // The UI opened 5 minutes after the drop, its clock unrelated to the
    // server's: the red time counts from receipt.
    app.now_ms = 7_000;
    app.pending.lock().expect("lock").status = Some(
        serde_json::from_str(
            r#"{"cards":{"openconnect_server.0":{"name":"Range Bridge",
                "status":"disconnected","telemetry":{},
                "state":"reconnecting","red_for_ms":300000}}}"#,
        )
        .expect("reply"),
    );
    app.apply_pending();
    let server = &app.actors["openconnect_server.0"];
    assert_eq!(server.state(), ConnectionState::Reconnecting);
    assert!(server.red(7_000 + 299_999));
    assert!(!server.red(7_000 + 300_000));
    assert!(settings_alert(&app.actors, 7_000));

    // The same Disconnected again is no new drop.
    app.handle_ui_message(status(ActorStatus::Disconnected));
    assert!(!app.actors["openconnect_server.0"].red(7_000 + 300_000));
    // A peer connects and leaves: a new ten minutes.
    app.now_ms = 400_000;
    app.handle_ui_message(status(ActorStatus::Connected));
    assert!(!settings_alert(&app.actors, app.now_ms));
    app.handle_ui_message(status(ActorStatus::Disconnected));
    assert!(app.actors["openconnect_server.0"].red(400_000 + RECONNECT_ALERT_MS - 1));
    // A restart from a config change starts again at Connecting.
    app.handle_ui_message(crate::socket::feed::local_msg(
        "system",
        flighthook::UiLocalEvent::ConfigOutcome {
            request_id: None,
            restarted: vec!["openconnect_server.0".into()],
            stopped: Vec::new(),
            started: Vec::new(),
            config_generation: 0,
        },
    ));
    let server = &app.actors["openconnect_server.0"];
    assert_eq!(server.state(), ConnectionState::Connecting);
    assert!(!settings_alert(&app.actors, app.now_ms));
}

#[test]
fn a_cards_rows_stay_through_a_lost_connection_and_flags_follow_its_bus_actor() {
    let mut app = FlighthookApp::with_pending(net::new_pending());
    let status = |status, telemetry: &[(&str, &str)]| {
        crate::socket::feed::card_msg("mevo.0", status, telemetry)
    };
    let device = |pairs: &[(&str, &str)]| crate::socket::feed::bus_actor_msg("lm", pairs);
    let held = |app: &FlighthookApp| {
        let mut rows: Vec<(String, String)> = app.actors["mevo.0"]
            .telemetry
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        rows.sort();
        rows
    };
    let row = |k: &str, v: &str| (k.to_owned(), v.to_owned());

    app.card_keys.insert("lm".into(), "mevo.0".into());
    app.handle_ui_message(status(ActorStatus::Connected, &[("device_info", "Xi")]));
    app.handle_ui_message(device(&[("tilt", "6.5°")]));
    let flighthook::UiMessage::Frp(mut flagged) = device(&[("tilt", "7.0°")]) else {
        unreachable!()
    };
    if let flighthook::FrpBody::BusActor(actor) = &mut flagged.body {
        actor.flagged = vec!["tilt".into()];
    }
    app.handle_ui_message(flighthook::UiMessage::Frp(flagged));
    assert_eq!(app.actors["mevo.0"].flagged, ["tilt"]);

    // Lost: every row stays, each at its latest value; not connected, so
    // not ready.
    app.handle_ui_message(status(ActorStatus::Reconnecting, &[("error", "gone")]));
    app.handle_ui_message(device(&[("tilt", "unknown")]));
    app.handle_ui_message(status(
        ActorStatus::Reconnecting,
        &[("device_info", "none")],
    ));
    assert_eq!(
        held(&app),
        [
            row("device_info", "none"),
            row("error", "gone"),
            row("ready", "false"),
            row("tilt", "unknown"),
        ]
    );
    assert!(app.actors["mevo.0"].flagged.is_empty(), "replaced whole");

    // A new instance keeps them too, until each is sent again.
    app.handle_ui_message(status(ActorStatus::Starting, &[("error", "none")]));
    assert_eq!(held(&app).len(), 4);
    assert_eq!(app.actors["mevo.0"].error(), None);
}

#[test]
fn club_picker_rows_cover_the_bag_by_kind() {
    let rows: Vec<String> = CLUB_PICKER_ROWS
        .iter()
        .map(|r| {
            Club::ALL[r.clone()]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    assert_eq!(
        rows,
        [
            "DR MD",
            "2W 3W 4W 5W 6W 7W 8W 9W",
            "1H 2H 3H 4H 5H 6H 7H 8H 9H",
            "1I 2I 3I 4I 5I 6I 7I 8I 9I",
            "PW GW SW LW",
            "PT",
        ]
    );
    assert_eq!(
        CLUB_PICKER_ROWS.last().map(|r| r.end),
        Some(Club::ALL.len())
    );
}

#[test]
fn shots_rev_bumps_at_every_shot_change_and_keys_the_caches() {
    use crate::types::{ClubData, Launch};
    use flighthook::{ShotData, ShotKey};

    let mut app = FlighthookApp::with_pending(net::new_pending());
    let key = ShotKey {
        shot_id: "a1".into(),
        shot_number: 1,
    };
    let rev = std::cell::Cell::new(app.shots_rev);
    let bumped = |app: &FlighthookApp, what: &str| {
        assert!(app.shots_rev > rev.get(), "{what}");
        rev.set(app.shots_rev);
    };

    // REST load.
    app.pending.lock().expect("lock").shots = Some(vec![ShotData {
        card: Some("mevo.0".into()),
        shot_number: 9,
        launch: Some(Launch::default()),
        ..ShotData::default()
    }]);
    app.apply_pending();
    bumped(&app, "REST load");
    app.refresh_shot_caches();
    assert_eq!(
        app.display_rows
            .peek(&(app.shots_rev, app.units()))
            .map(Vec::len),
        Some(1)
    );

    let stored = |launch: Option<Launch>, club: Option<ClubData>| ShotData {
        card: Some("square.0".into()),
        shot_id: Some(key.shot_id.clone()),
        shot_number: key.shot_number,
        launch,
        club,
        block: Some("s1".into()),
        ..ShotData::default()
    };
    crate::socket::feed::shot(&mut app, stored(None, None));
    bumped(&app, "a new shot");
    crate::socket::feed::shot(&mut app, stored(Some(Launch::default()), None));
    bumped(&app, "its ball");
    crate::socket::feed::shot(
        &mut app,
        stored(Some(Launch::default()), Some(ClubData::default())),
    );
    bumped(&app, "its club");
    assert_eq!(app.shots.len(), 2);
    assert_eq!(app.shots[1].group.as_deref(), Some("s1"));

    // The caches follow: the display rows and the device ids.
    app.refresh_shot_caches();
    let rows = app.display_rows.peek(&(app.shots_rev, app.units()));
    assert_eq!(rows.map(Vec::len), Some(2));
    assert_eq!(rows.and_then(|r| r[1].group.as_deref()), Some("s1"));
    assert_eq!(
        app.device_ids.peek(&app.shots_rev).cloned(),
        Some(vec!["mevo.0".to_owned(), "square.0".to_owned()])
    );
    // A unit flip rebuilds the rows without touching the shots.
    let rev = rev.get();
    app.settings.default_units = UnitSystem::Metric;
    assert!(app.display_rows.peek(&(rev, UnitSystem::Metric)).is_none());
    app.refresh_shot_caches();
    assert!(app.display_rows.peek(&(rev, UnitSystem::Metric)).is_some());
    assert_eq!(app.shots_rev, rev);
}

#[test]
fn clear_history_on_the_bus_empties_the_shots_tab() {
    use crate::types::Launch;
    use flighthook::ShotData;

    let stored = |actor: &str, n| ShotData {
        card: Some(actor.into()),
        shot_number: n,
        launch: Some(Launch::default()),
        block: Some("s1".into()),
        ..ShotData::default()
    };
    let mut app = FlighthookApp::with_pending(net::new_pending());
    {
        let mut p = app.pending.lock().expect("lock");
        p.shots = Some(vec![
            stored("mevo.0", 1),
            stored("square.0", 1),
            stored("mevo.0", 2),
        ]);
        p.groups = Some(blocks(2, &[("s1", 2, Some(Club::Iron7), true, 3)]));
    }
    app.apply_pending();
    // A live shot arrives and pulses.
    crate::socket::feed::shot(
        &mut app,
        ShotData {
            card: Some("mevo.0".into()),
            shot_id: Some("m3".into()),
            shot_number: 3,
            launch: Some(Launch::default()),
            ..ShotData::default()
        },
    );
    // The store's shot asks for the sets; that fetch is not the point.
    app.needs_groups_refresh = false;
    app.observe_shots(10.0);
    app.device_filter = Some("square.0".into());
    app.confirm_clear = true;
    app.refresh_shot_caches();
    assert!(app.device_ids.peek(&app.shots_rev).is_some());
    let (shots_rev, groups_rev) = (app.shots_rev, app.groups.rev());
    assert!(app.shot_focus.progress(10.0).is_some(), "pulsing");
    // A reply fetched before the clear, not drained yet.
    app.pending.lock().expect("lock").shots = Some(vec![stored("mevo.0", 1)]);

    // Pressed in another tab: every UI sees the command.
    app.handle_ui_message(crate::socket::feed::local_msg(
        "web",
        flighthook::UiLocalEvent::ClearShotHistory,
    ));
    assert!(app.shots.is_empty());
    assert!(app.groups.is_empty());
    assert!(app.shots_rev > shots_rev);
    assert!(app.groups.rev() > groups_rev);
    assert_eq!(app.device_filter, None);
    assert!(!app.confirm_clear, "an open confirmation closes");
    assert_eq!(app.shot_focus.progress(10.0), None);
    assert!(app.device_ids.peek(&shots_rev).is_none(), "caches emptied");
    app.apply_pending();
    assert!(app.shots.is_empty(), "the stale reply is dropped");
    assert!(!app.needs_shots_refresh && !app.needs_groups_refresh);
    assert_eq!(
        app.log_entries.last().map(|e| e.message_type.as_str()),
        Some("clear_shot_history")
    );

    // The next shot starts afresh.
    app.handle_ui_message(crate::socket::feed::shot_msg(
        "mevo.0",
        flighthook::ShotKey {
            shot_id: "n1".into(),
            shot_number: 1,
        },
    ));
    assert_eq!(app.shots.len(), 1);
}

#[test]
fn replies_requested_before_a_clear_are_dropped() {
    use crate::types::Launch;
    use flighthook::ShotData;

    let stored = |n| ShotData {
        card: Some("mevo.0".into()),
        shot_number: n,
        launch: Some(Launch::default()),
        block: Some("s1".into()),
        ..ShotData::default()
    };
    let sets = |session| {
        Some(blocks(
            session,
            &[("s1", session, Some(Club::Iron7), true, 2)],
        ))
    };
    let land = |app: &mut FlighthookApp, generation, shots: Option<Vec<ShotData>>, groups| {
        {
            let mut p = app.pending.lock().expect("lock");
            p.shots = shots;
            p.shots_generation = generation;
            p.groups = groups;
            p.groups_generation = generation;
        }
        app.apply_pending();
    };
    let mut app = FlighthookApp::with_pending(net::new_pending());
    land(&mut app, 0, Some(vec![stored(1), stored(2)]), sets(4));
    assert_eq!((app.shots.len(), app.groups.session()), (2, 4));

    // A refetch goes out, then the history is cleared before it lands.
    let asked = app.history_generation;
    app.handle_ui_message(crate::socket::feed::local_msg(
        "web",
        flighthook::UiLocalEvent::ClearShotHistory,
    ));
    land(&mut app, asked, Some(vec![stored(1), stored(2)]), sets(4));
    assert!(app.shots.is_empty(), "no pre-clear shot comes back");
    assert!(app.groups.is_empty(), "no pre-clear set comes back");
    assert!(!app.needs_shots_refresh && !app.needs_groups_refresh);

    // One requested after the clear applies.
    let asked = app.history_generation;
    land(&mut app, asked, Some(vec![stored(1)]), sets(5));
    assert_eq!((app.shots.len(), app.groups.session()), (1, 5));

    assert!(app.groups.get("s1").is_some());
}

#[test]
fn device_filter_falls_back_to_all_when_its_device_leaves() {
    use crate::types::Launch;
    use flighthook::ShotData;

    let stored = |actor: &str, n| ShotData {
        card: Some(actor.into()),
        shot_number: n,
        launch: Some(Launch::default()),
        ..ShotData::default()
    };
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.pending.lock().expect("lock").shots =
        Some(vec![stored("mevo.0", 1), stored("square.0", 1)]);
    app.apply_pending();
    app.device_filter = Some("square.0".into());
    app.refresh_shot_caches();
    assert_eq!(app.device_filter.as_deref(), Some("square.0"));
    // The Omni's shots age out of the history.
    app.pending.lock().expect("lock").shots = Some(vec![stored("mevo.0", 1)]);
    app.apply_pending();
    app.refresh_shot_caches();
    assert_eq!(app.device_filter, None);
}

#[test]
fn face_impact_and_handedness_reach_the_rows() {
    use crate::types::{Distance, FaceImpact, Handedness};
    use flighthook::Mode;
    use flighthook::{ShotData, ShotKey};

    let mut app = FlighthookApp::with_pending(net::new_pending());
    // Handedness from the WebSocket handshake, then from the bus; a
    // mode-only change keeps it.
    app.handle_ui_message(turn_msg(Some(Club::Iron7), Some(Handedness::Left), None));
    assert_eq!(app.handed, Some(Handedness::Left));
    assert_eq!(app.selected_club, Some(Club::Iron7));
    app.handle_ui_message(turn_msg(
        Some(Club::Iron7),
        Some(Handedness::Right),
        Some(Mode::Full),
    ));
    assert_eq!(app.handed, Some(Handedness::Right));
    app.handle_ui_message(turn_msg(
        Some(Club::Iron7),
        Some(Handedness::Right),
        Some(Mode::Chipping),
    ));
    assert_eq!(app.handed, Some(Handedness::Right));
    assert_eq!(app.current_mode, Mode::Chipping.to_string());
    app.handle_ui_message(turn_msg(
        Some(Club::Iron7),
        Some(Handedness::Left),
        Some(Mode::Chipping),
    ));

    // The store's shot carries the hand at its trigger and its face
    // impact.
    let key = ShotKey {
        shot_id: "s1".into(),
        shot_number: 1,
    };
    let impact = FaceImpact {
        lateral: Some(Distance::Inches(0.31)),
        vertical: Some(Distance::Millimeters(-2.0)),
    };
    let rev = app.shots_rev;
    crate::socket::feed::shot(
        &mut app,
        ShotData {
            card: Some("square.0".into()),
            shot_id: Some(key.shot_id.clone()),
            shot_number: key.shot_number,
            impact: Some(impact.clone()),
            turn: Some(flighthook::GolferTurn {
                golfer: None,
                club: None,
                handedness: Some(Handedness::Left),
                mode: None,
            }),
            ..ShotData::default()
        },
    );
    assert!(app.shots_rev > rev);
    assert_eq!(app.shots[0].handed, Some(Handedness::Left));
    assert_eq!(app.shots[0].impact, Some(impact.clone()));

    // Rows from `/api/shots` keep both, and a unit change leaves the
    // impact as sent (it is read in millimeters everywhere).
    let row = ShotRow::from(ShotData {
        card: Some("square.0".into()),
        shot_number: 2,
        impact: Some(impact.clone()),
        turn: Some(flighthook::GolferTurn {
            golfer: None,
            club: None,
            handedness: Some(Handedness::Left),
            mode: None,
        }),
        ..ShotData::default()
    });
    assert_eq!(row.handed, Some(Handedness::Left));
    for units in [UnitSystem::Imperial, UnitSystem::Metric] {
        let converted = row.to_unit_system(units);
        assert_eq!(converted.impact, Some(impact.clone()));
        assert_eq!(converted.handed, Some(Handedness::Left));
        let lateral = converted.impact.unwrap().lateral.unwrap();
        assert!((lateral.as_millimeters() - 7.874).abs() < 1e-9);
    }
}

#[test]
fn a_clip_stamps_every_row_of_its_shots_and_a_removal_clears_them() {
    use crate::types::{CameraPosition, ShotKey};

    let key = |id: &str, n: u32| ShotKey {
        shot_id: id.into(),
        shot_number: n,
    };
    let clip = |camera: &str, keys: Vec<ShotKey>, duration_ms| {
        crate::socket::feed::local_msg(
            camera,
            flighthook::UiLocalEvent::ShotVideo {
                keys,
                position: CameraPosition::GolferFront,
                duration_ms,
                fps: 120.0,
                trigger_offset_ms: 4000,
                impact_offset_ms: 4000,
                complete: true,
                dropped: 0,
            },
        )
    };
    let mut app = FlighthookApp::with_pending(net::new_pending());
    // Two launch monitors saw one swing; a third shot has no clip.
    for (actor, k) in [
        ("mevo.0", key("a", 1)),
        ("square.0", key("b", 1)),
        ("mevo.0", key("c", 2)),
    ] {
        app.handle_ui_message(crate::socket::feed::shot_msg(actor, k));
    }
    // The clip lands before either shot finished.
    let rev = app.shots_rev;
    app.handle_ui_message(clip("camera.0", vec![key("a", 1), key("b", 1)], 5500));
    assert!(app.shots_rev > rev);
    assert_eq!(app.shots[0].video.len(), 1);
    assert_eq!(app.shots[1].video[0].camera, "camera.0");
    assert!(app.shots[2].video.is_empty());
    // Finishing keeps it; a second camera adds its own; the same camera
    // again replaces its clip.
    app.handle_ui_message(clip("camera.1", vec![key("a", 1)], 5500));
    app.handle_ui_message(clip("camera.0", vec![key("a", 1)], 4000));
    let cameras: Vec<(&str, u64)> = app.shots[0]
        .video
        .iter()
        .map(|v| (v.camera.as_str(), v.duration_ms))
        .collect();
    assert_eq!(cameras, [("camera.0", 4000), ("camera.1", 5500)]);
    // A clip after its shot finished stamps it too.
    app.handle_ui_message(clip("camera.0", vec![key("c", 2)], 5500));
    assert_eq!(app.shots[2].video.len(), 1);
    // A clip of a shot the UI doesn't hold changes nothing.
    let rev = app.shots_rev;
    app.handle_ui_message(clip("camera.0", vec![key("zzz", 9)], 5500));
    assert_eq!(app.shots_rev, rev);

    app.handle_ui_message(crate::socket::feed::local_msg(
        "camera.0",
        flighthook::UiLocalEvent::ShotVideoRemoved {
            keys: vec![key("a", 1), key("b", 1)],
        },
    ));
    assert!(app.shots[0].video.is_empty() && app.shots[1].video.is_empty());
    assert_eq!(app.shots[2].video.len(), 1);
    assert_eq!(
        app.log_entries.last().unwrap().message_type,
        "shot_video_removed"
    );
}

#[test]
fn an_open_replay_closes_when_its_clips_are_removed() {
    use crate::types::{CameraPosition, ShotKey};

    let key = |id: &str| ShotKey {
        shot_id: id.into(),
        shot_number: 1,
    };
    let mut app = FlighthookApp::with_pending(net::new_pending());
    for (actor, id) in [("mevo.0", "a"), ("mevo.1", "b")] {
        app.handle_ui_message(crate::socket::feed::shot_msg(actor, key(id)));
        app.handle_ui_message(crate::socket::feed::local_msg(
            "camera.0",
            flighthook::UiLocalEvent::ShotVideo {
                keys: vec![key(id)],
                position: CameraPosition::GolferFront,
                duration_ms: 5500,
                fps: 60.0,
                trigger_offset_ms: 4000,
                impact_offset_ms: 4000,
                complete: true,
                dropped: 0,
            },
        ));
    }
    let ctx = egui::Context::default();
    app.open_replay(&ctx, 0);
    assert_eq!(app.replay.as_ref().map(|r| r.shot_id()), Some("a"));
    let removed = |id: &str| {
        crate::socket::feed::local_msg(
            "camera.0",
            flighthook::UiLocalEvent::ShotVideoRemoved {
                keys: vec![key(id)],
            },
        )
    };
    app.handle_ui_message(removed("b"));
    assert!(app.replay.is_some(), "another shot's clips");
    app.handle_ui_message(removed("a"));
    assert!(app.replay.is_none());
}

#[test]
fn rows_from_the_server_carry_their_clips_and_the_camera_flags() {
    use crate::types::{CameraPosition, ShotData, ShotVideo, StatusResponse};

    let video = ShotVideo {
        camera: "camera.0".into(),
        position: CameraPosition::TargetLine,
        duration_ms: 5500,
        fps: 60.0,
        trigger_offset_ms: 4000,
        impact_offset_ms: 2500,
        complete: false,
        dropped: 3,
        pose: false,
        swing: false,
        rotation: 0,
        analyzed_with: None,
    };
    let row: ShotData = serde_json::from_value(serde_json::json!({
        "actor": "mevo.0", "shot_number": 4, "shot_id": "s4",
        "ball": {}, "video": [video],
    }))
    .unwrap();
    let mut app = FlighthookApp::with_pending(net::new_pending());
    {
        let mut p = app.pending.lock().unwrap();
        p.shots = Some(vec![row]);
        p.status = Some(StatusResponse {
            cards: HashMap::new(),
            actors: HashMap::new(),
            turn: None,
            cameras_supported: true,
            trackball_supported: false,
            clip_dir: Some("/clips".into()),
            config_error: None,
        });
    }
    app.apply_pending();
    assert_eq!(app.shots[0].video, [video]);
    assert!(app.cameras_supported);
    assert_eq!(app.clip_dir.as_deref(), Some("/clips"));
    // Converting units keeps the clips.
    let metric = app.shots[0].to_unit_system(UnitSystem::Metric);
    assert_eq!(metric.video.len(), 1);
}

// -- Header ----------------------------------------------------------------

/// An app with Auto-chip on, GSPro and a Pureplay output connected and
/// an Omni at 74 %: the fullest header.
fn full_header_app() -> FlighthookApp {
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let mut config = crate::types::FlighthookConfig::default();
    config.simulation.auto_chip = true;
    app.settings.load_from(&config);
    app.actors.insert(
        "openconnect_client.0".into(),
        ActorStatusResponse::new("GSPro", ActorStatus::Connected),
    );
    app.actors.insert(
        "pureplay.0".into(),
        ActorStatusResponse::new("Pureplay", ActorStatus::Connected),
    );
    let mut omni = ActorStatusResponse::new("Square Omni", ActorStatus::Connected);
    omni.telemetry.insert("battery_pct".into(), "74".into());
    app.actors.insert("square.0".into(), omni);
    app
}

/// Two golfers, Eric (bag DR and PW, profile published) up.
fn golfers_app() -> FlighthookApp {
    let mut app = full_header_app();
    let mut config = app.settings.saved_config_ref().cloned().expect("loaded");
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.identifiers.push("pureplay.local-3".into());
    eric.bag = vec![Club::Driver, Club::PitchingWedge];
    config.golfers = vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")];
    app.settings.load_from(&config);
    let mut profile = flighthook::GolferProfile::new("flighthook.e".parse().unwrap(), "Eric");
    profile.bag = vec![Club::Driver, Club::PitchingWedge];
    let msg = flighthook::BusMessage::new("fh", profile);
    app.bus_states.insert(msg.state_key().unwrap(), msg);
    let game = flighthook::GolferProfile::new("pureplay.player-7".parse().unwrap(), "Guest");
    let msg = flighthook::BusMessage::new("pp", game);
    app.bus_states.insert(msg.state_key().unwrap(), msg);
    app.turn_golfer = Some("flighthook.e".into());
    app.selected_club = Some(Club::Iron7);
    app
}

#[test]
fn the_golfer_pill_names_the_golfer_up_by_the_lookup_rule() {
    let app = golfers_app();
    let config = app.settings.saved_config_ref();
    assert_eq!(
        golfer_name(
            config,
            &app.bus_states,
            &app.golfer_names,
            "pureplay.local-3"
        ),
        "Eric"
    );
    assert_eq!(
        golfer_name(
            config,
            &app.bus_states,
            &app.golfer_names,
            "pureplay.player-7"
        ),
        "Guest"
    );
    assert_eq!(
        golfer_name(config, &app.bus_states, &app.golfer_names, "x.y"),
        "x.y"
    );
    // The bag limits the clubs; the club in play still shows.
    assert_eq!(
        offered_clubs(
            config,
            &app.bus_states,
            Some("flighthook.e"),
            Some(Club::Iron7)
        ),
        [Club::Driver, Club::Iron7, Club::PitchingWedge]
    );
    // A linked game id with no profile of its own: its golfer's bag.
    assert_eq!(
        offered_clubs(config, &app.bus_states, Some("pureplay.local-3"), None),
        [Club::Driver, Club::PitchingWedge]
    );
    // No bag: every club.
    assert_eq!(
        offered_clubs(config, &app.bus_states, Some("flighthook.s"), None).len(),
        Club::ALL.len()
    );
    assert_eq!(
        offered_clubs(config, &app.bus_states, None, None).len(),
        Club::ALL.len()
    );
}

/// Another actor's `golfer.profile` under a
/// `flighthook.*` id never supplies the club popup's bag: a flighthook
/// golfer's bag is its saved record's.
#[test]
fn another_actors_profile_under_a_flighthook_id_gives_no_bag() {
    let mut app = golfers_app();
    let mut fake = flighthook::GolferProfile::new("flighthook.s".parse().unwrap(), "Sam");
    fake.bag = vec![Club::Putter];
    let msg = flighthook::BusMessage::new("intruder", fake);
    app.bus_states.insert(msg.state_key().unwrap(), msg);
    let config = app.settings.saved_config_ref();
    assert_eq!(
        offered_clubs(config, &app.bus_states, Some("flighthook.s"), None).len(),
        Club::ALL.len()
    );
    assert_eq!(
        offered_clubs(config, &app.bus_states, Some("flighthook.e"), None),
        [Club::Driver, Club::PitchingWedge]
    );
}

/// A status reply that says whether the server reads a trackball.
fn welcome_status(trackball: bool) -> crate::types::StatusResponse {
    crate::types::StatusResponse {
        cards: HashMap::new(),
        actors: HashMap::new(),
        turn: None,
        cameras_supported: false,
        trackball_supported: trackball,
        clip_dir: None,
        config_error: None,
    }
}

/// An app on a first run: the default config, with no user actors, and
/// a status that says whether the server reads a trackball.
fn welcome_app(trackball: bool) -> FlighthookApp {
    let mut app = FlighthookApp::with_pending(net::new_pending());
    {
        let mut p = app.pending.lock().expect("pending");
        p.settings = Some(crate::types::FlighthookConfig::default());
        p.status = Some(welcome_status(trackball));
    }
    app.apply_pending();
    app
}

/// Draws the app's welcome screen (or what stands in for it) in a
/// window of the given size, at zoom 1.
struct Welcome(egui::Context, egui::Vec2);

impl Welcome {
    fn sized(width: f32, height: f32) -> Self {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        Self(ctx, egui::vec2(width, height))
    }

    /// One pass with `events`. Returns the marked controls.
    fn frame(
        &self,
        app: &mut FlighthookApp,
        events: Vec<egui::Event>,
    ) -> crate::panels::devices::Marks {
        self.0
            .data_mut(|d| d.remove_temp::<crate::panels::devices::Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.1)),
            events,
            ..Default::default()
        };
        let _ = self.0.run(input, |ctx| {
            if app.show_wizard() {
                app.render_wizard(ctx);
            }
        });
        self.0
            .data(|d| d.get_temp::<crate::panels::devices::Marks>(egui::Id::NULL))
            .unwrap_or_default()
    }

    /// Click the middle of control `name`, then draw once more.
    fn click(&self, app: &mut FlighthookApp, name: &str) -> crate::panels::devices::Marks {
        let marks = self.frame(app, Vec::new());
        let at = marks
            .get(name)
            .unwrap_or_else(|| panic!("no {name}: {:?}", marks.keys()))
            .0
            .center();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(app, vec![egui::Event::PointerMoved(at)]);
        self.frame(app, vec![button(true)]);
        self.frame(app, vec![button(false)]);
        self.frame(app, Vec::new())
    }
}

/// The welcome screen's checkboxes, by (+) menu label.
fn welcome_boxes(marks: &crate::panels::devices::Marks) -> Vec<&str> {
    let mut boxes: Vec<&str> = marks
        .keys()
        .filter_map(|k| k.strip_prefix("wizard:"))
        .filter(|k| !matches!(*k, "get_started" | "skip"))
        .collect();
    boxes.sort_unstable();
    boxes
}

/// The welcome screen lists what the Devices tab's (+) menus offer for
/// launch monitors and Software (the Pureplay Accessory card once,
/// among the launch monitors), without the cameras, and the Trackball
/// only where the server reads one.
#[test]
fn the_welcome_screen_offers_every_preset_of_the_add_menus() {
    for trackball in [false, true] {
        let welcome = Welcome::sized(1600.0, 900.0);
        let mut app = welcome_app(trackball);
        let marks = welcome.frame(&mut app, Vec::new());
        let mut want: Vec<&str> = [CardGroup::LaunchMonitors, CardGroup::Software]
            .into_iter()
            .flat_map(|g| cards::offered(g, trackball))
            .map(|p| p.label)
            .collect();
        want.sort_unstable();
        assert_eq!(welcome_boxes(&marks), want, "trackball {trackball}");
        for label in [
            "Mevo",
            "R10",
            "Square Golf Omni",
            "ExPutt",
            "Open Connect Server",
            "Pureplay Accessory",
            "GSPro (Open Connect)",
            "Pureplay (Open Connect)",
            "Muni Golf (Open Connect)",
        ] {
            assert!(marks.contains_key(&format!("wizard:{label}")), "{label}");
        }
        assert!(!marks.contains_key("wizard:Pureplay (Pureplay Connect)"));
        assert_eq!(marks.contains_key("wizard:Trackball"), trackball);
        assert!(!marks.contains_key("wizard:Camera"));
        assert!(!enabled_mark(&marks, "wizard:get_started"));
        assert!(marks.contains_key("wizard:skip"));
    }
}

fn enabled_mark(marks: &crate::panels::devices::Marks, name: &str) -> bool {
    marks.get(name).is_some_and(|(_, enabled)| *enabled)
}

/// The welcome screen waits for the status, so the Trackball is listed
/// from its first frame; a later status keeps a checked Trackball.
#[test]
fn the_welcome_screen_waits_for_the_status() {
    let welcome = Welcome::sized(1600.0, 900.0);
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.pending.lock().expect("pending").settings = Some(crate::types::FlighthookConfig::default());
    app.apply_pending();
    assert!(app.wizard_waiting());
    assert!(!app.show_wizard(), "not before the status");
    assert!(welcome.frame(&mut app, Vec::new()).is_empty());

    app.pending.lock().expect("pending").status = Some(welcome_status(true));
    app.apply_pending();
    assert!(app.show_wizard());
    let marks = welcome.click(&mut app, "wizard:Trackball");
    assert!(app.wizard_picked.contains("Trackball"));
    assert!(enabled_mark(&marks, "wizard:get_started"));

    app.pending.lock().expect("pending").status = Some(welcome_status(true));
    app.apply_pending();
    let marks = welcome.frame(&mut app, Vec::new());
    assert!(marks.contains_key("wizard:Trackball"));
    assert!(app.wizard_picked.contains("Trackball"));

    // Status first, settings second: the same screen.
    let mut app = FlighthookApp::with_pending(net::new_pending());
    app.pending.lock().expect("pending").status = Some(welcome_status(false));
    app.apply_pending();
    assert!(!app.show_wizard());
    app.pending.lock().expect("pending").settings = Some(crate::types::FlighthookConfig::default());
    app.apply_pending();
    let marks = welcome.frame(&mut app, Vec::new());
    assert!(marks.contains_key("wizard:ExPutt"));
    assert!(!marks.contains_key("wizard:Trackball"));
}

/// Get Started adds each checked preset as its (+) menu entry does and
/// posts the whole config; the form takes the cards once it is saved.
#[test]
fn get_started_adds_the_checked_presets_as_the_add_menu_does() {
    let welcome = Welcome::sized(1600.0, 900.0);
    let mut app = welcome_app(false);
    for label in ["ExPutt", "Muni Golf (Open Connect)"] {
        welcome.click(&mut app, &format!("wizard:{label}"));
    }
    let marks = welcome.frame(&mut app, Vec::new());
    assert!(!marks.contains_key("wizard:skip"));
    welcome.click(&mut app, "wizard:get_started");
    assert!(app.wizard_saving);

    let (target, posted) = app.settings.posted().expect("posted");
    assert!(matches!(target, crate::panels::settings::SaveTarget::Full));
    let exputt = &posted.exputt["0"];
    assert_eq!(exputt.name, "ExPutt");
    assert!(exputt.enabled);
    let muni = &posted.openconnect_client["0"];
    assert_eq!(muni.name, "Muni Golf");
    assert_eq!(muni.driver.address.as_deref(), Some("127.0.0.1:9210"));
    assert!(muni.enabled);
    assert_eq!(posted.exputt.len(), 1);
    assert_eq!(posted.openconnect_client.len(), 1);
    assert!(posted.pureplay.is_empty());
    assert!(posted.mevo.is_empty() && posted.r10.is_empty() && posted.square.is_empty());
    assert!(posted.openconnect_server.is_empty() && posted.trackball.is_empty());
    assert!(posted.webserver.contains_key("0"), "the web server stays");
    assert!(
        app.settings.position("exputt.0").is_none(),
        "the form waits for the save"
    );

    app.pending.lock().expect("pending").settings_save =
        Some(Ok(crate::types::PostSettingsResponse::default()));
    app.apply_pending();
    assert!(app.wizard_dismissed);
    for id in ["exputt.0", "openconnect_client.0"] {
        let i = app.settings.position(id).expect(id);
        assert!(!app.settings.actors[i].dirty(), "{id}");
    }
    assert!(!app.settings.locked());
}

/// GSPro and Pureplay over Open Connect add the same connection: only
/// one can be checked. Any other pair of welcome presets saves both
/// cards switched on.
#[test]
fn gspro_and_pureplay_over_open_connect_exclude_each_other() {
    let welcome = Welcome::sized(1600.0, 900.0);
    let mut app = welcome_app(true);
    welcome.click(&mut app, "wizard:GSPro (Open Connect)");
    welcome.click(&mut app, "wizard:Muni Golf (Open Connect)");
    welcome.click(&mut app, "wizard:Pureplay (Open Connect)");
    assert!(app.wizard_picked.contains("Pureplay (Open Connect)"));
    assert!(!app.wizard_picked.contains("GSPro (Open Connect)"));
    assert!(app.wizard_picked.contains("Muni Golf (Open Connect)"));
    welcome.click(&mut app, "wizard:GSPro (Open Connect)");
    assert!(app.wizard_picked.contains("GSPro (Open Connect)"));
    assert!(!app.wizard_picked.contains("Pureplay (Open Connect)"));
    welcome.click(&mut app, "wizard:GSPro (Open Connect)");
    assert!(app.wizard_picked.contains("Muni Golf (Open Connect)"));
    assert!(!app.wizard_picked.contains("GSPro (Open Connect)"));

    // Everything that can be checked together, checked: every card on.
    let mut app = welcome_app(true);
    let marks = welcome.frame(&mut app, Vec::new());
    for label in welcome_boxes(&marks) {
        if label != "Pureplay (Open Connect)" {
            welcome.click(&mut app, &format!("wizard:{label}"));
        }
    }
    assert_eq!(app.wizard_picked.len(), welcome_boxes(&marks).len() - 1);
    welcome.click(&mut app, "wizard:get_started");
    let (_, posted) = app.settings.posted().expect("posted");
    let on = [
        posted.mevo.values().map(|s| s.enabled).collect::<Vec<_>>(),
        posted.r10.values().map(|s| s.enabled).collect(),
        posted.square.values().map(|s| s.enabled).collect(),
        posted.exputt.values().map(|s| s.enabled).collect(),
        posted.trackball.values().map(|s| s.enabled).collect(),
        posted
            .openconnect_server
            .values()
            .map(|s| s.enabled)
            .collect(),
        posted
            .openconnect_client
            .values()
            .map(|s| s.enabled)
            .collect(),
    ];
    assert_eq!(
        on.iter().map(Vec::len).collect::<Vec<_>>(),
        [1, 1, 1, 1, 1, 1, 2]
    );
    assert!(posted.pureplay.is_empty());
    assert!(on.iter().flatten().all(|e| *e), "every card on: {on:?}");
}

/// The welcome screen fits a 1024×768 window without scrolling, with
/// every launch monitor and simulator listed, both before anything is
/// checked (Skip shown) and after a failed Get Started (error shown).
#[test]
fn the_welcome_screen_fits_1024_by_768() {
    let welcome = Welcome::sized(1024.0, 768.0);
    let mut app = welcome_app(true);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0));
    let inside = |marks: &crate::panels::devices::Marks| {
        for (name, (rect, _)) in marks {
            assert!(screen.contains_rect(*rect), "{name} at {rect:?}");
        }
    };
    let marks = welcome.frame(&mut app, Vec::new());
    assert_eq!(welcome_boxes(&marks).len(), 10);
    inside(&marks);
    assert!(marks.contains_key("wizard:skip"));

    welcome.click(&mut app, "wizard:Trackball");
    welcome.click(&mut app, "wizard:get_started");
    app.pending.lock().expect("pending").settings_save = Some(Err("Failed to fetch".to_owned()));
    app.apply_pending();
    let marks = welcome.frame(&mut app, Vec::new());
    assert!(marks.contains_key("error:wizard"));
    inside(&marks);
}
