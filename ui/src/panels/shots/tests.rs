//! Tests of the Shots tab, over every part of it.

use super::*;
use crate::types::Launch;

fn row(n: u32, with_ball: bool) -> ShotRow {
    ShotRow {
        golfer: None,
        actor: "mock_monitor.0".into(),
        shot_id: format!("id{n}"),
        shot_number: n,
        launch: with_ball.then(Launch::default),
        flight: None,
        club: None,
        impact: None,
        derived: None,
        club_selected: None,
        handed: None,
        group: None,
        finished: true,
        video: Vec::new(),
    }
}

#[test]
fn a_finished_shot_without_data_is_a_no_read() {
    let entry = |shot: &ShotRow| ListEntry::new(shot, None, true);
    assert!(entry(&row(1, false)).no_read);
    assert!(!entry(&row(1, true)).no_read);
    let club_only = ShotRow {
        club: Some(ClubData::default()),
        ..row(1, false)
    };
    assert!(!entry(&club_only).no_read);
    // Still in flight: no data yet, but not a no-read.
    let in_flight = ShotRow {
        finished: false,
        ..row(1, false)
    };
    assert!(!entry(&in_flight).no_read);
    assert!(!entry(&in_flight).has_data);
}

#[test]
fn cards_show_newest_shot_with_data() {
    let shots = vec![row(1, true), row(2, true), row(3, false)];
    assert_eq!(latest_index(&shots, ShotFilter::ALL), Some(1));
    assert_eq!(latest_index(&[row(1, false)], ShotFilter::ALL), None);
    assert_eq!(latest_index(&[], ShotFilter::ALL), None);
}

#[test]
fn device_names_follow_one_rule_everywhere() {
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let named = |name: &str| ActorStatusResponse::new(name, crate::types::ActorStatus::Connected);
    app.actors.insert("mevo.0".into(), named("Mevo+"));
    app.actors.insert("square.0".into(), named(""));
    let shot = |actor: &str| ShotRow {
        actor: actor.into(),
        ..row(1, true)
    };
    // Name, else the actor id, as the device filter and tooltips label it.
    assert_eq!(app.device_name(&shot("mevo.0"), true), Some("Mevo+"));
    assert_eq!(app.device_name(&shot("square.0"), true), Some("square.0"));
    assert_eq!(app.device_name(&shot("r10.0"), true), Some("r10.0"));
    assert_eq!(app.device_name(&shot("mevo.0"), false), None);
    let ids = vec!["mevo.0".to_owned(), "square.0".to_owned()];
    assert_eq!(
        device_options(&ids, &app.actors),
        Some(vec!["All".into(), "Mevo+".into(), "square.0".into()])
    );
}

/// A remote launch monitor's shots are named by its `bus.actor`
/// name, also after it has left the bus.
#[test]
fn a_remote_monitor_is_named_by_its_bus_actor() {
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.actors.insert(
        "mevo.0".into(),
        ActorStatusResponse::new("Mevo+", crate::types::ActorStatus::Connected),
    );
    let remote = "7d2f9a41c03be856";
    let actor = flighthook::BusActor::new("Trackball", 2.0);
    app.handle_ui_message(flighthook::UiMessage::Frp(Box::new(
        flighthook::BusMessage::new(remote, actor),
    )));
    app.handle_ui_message(flighthook::UiMessage::Frp(Box::new(
        flighthook::BusMessage::new(
            remote,
            flighthook::Removal::of(flighthook::StateKind::BusActor),
        ),
    )));
    let shot = ShotRow {
        actor: remote.into(),
        ..row(1, true)
    };
    assert_eq!(app.device_name(&shot, true), Some("Trackball"));
    let ids = vec!["mevo.0".to_owned(), remote.to_owned()];
    assert_eq!(
        device_options_with(&ids, &app.actors, &app.remote_names),
        Some(vec!["All".into(), "Mevo+".into(), "Trackball".into()])
    );
}

/// After a reload (a new page, the remote actor long gone) a remote
/// monitor's shots are named by the name stored on them.
#[test]
fn a_remote_monitor_is_named_by_its_shot_after_a_reload() {
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let remote = "7d2f9a41c03be856";
    app.pending.lock().expect("lock").shots = Some(vec![crate::types::ShotData {
        shot_number: 1,
        shot_id: Some("s1".into()),
        from: remote.into(),
        from_name: Some("Trackball".into()),
        ..crate::types::ShotData::default()
    }]);
    app.apply_pending();
    let shot = ShotRow {
        actor: remote.into(),
        ..row(1, true)
    };
    assert_eq!(app.device_name(&shot, true), Some("Trackball"));
}

/// After a reload, with the round over and its profile gone, a game
/// golfer's shots are named by the name stored on them, not by the id.
#[test]
fn a_game_golfer_is_named_by_its_shot_after_a_reload() {
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let id = "pureplay.player-1";
    let name = |app: &FlighthookApp| {
        crate::app::golfer_name(
            app.settings.saved_config_ref(),
            &app.bus_states,
            &app.golfer_names,
            id,
        )
    };
    assert_eq!(name(&app), id, "nothing is known yet");
    let turn = flighthook::GolferTurn::new(
        Some(id.parse().expect("id")),
        Some(Club::Iron7),
        flighthook::Handedness::Right,
        flighthook::Mode::Full,
    );
    app.pending.lock().expect("lock").shots = Some(vec![crate::types::ShotData {
        shot_number: 1,
        shot_id: Some("s1".into()),
        turn: Some(turn),
        golfer_name: Some("Joe".into()),
        ..crate::types::ShotData::default()
    }]);
    app.apply_pending();
    assert_eq!(name(&app), "Joe");
}

/// Two shots of one game golfer id under two names (a profile slot
/// renamed between rounds), applied oldest first: the newest name wins.
#[test]
fn the_newest_shots_name_wins_for_a_game_golfer() {
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let id = "pureplay.local-3";
    let turn = flighthook::GolferTurn::new(
        Some(id.parse().expect("id")),
        Some(Club::Iron7),
        flighthook::Handedness::Right,
        flighthook::Mode::Full,
    );
    let shot = |n: u32, name: &str| crate::types::ShotData {
        shot_number: n,
        shot_id: Some(format!("s{n}")),
        turn: Some(turn.clone()),
        golfer_name: Some(name.into()),
        ..crate::types::ShotData::default()
    };
    app.pending.lock().expect("lock").shots = Some(vec![shot(1, "Joe"), shot(2, "Guest")]);
    app.apply_pending();
    assert_eq!(
        crate::app::golfer_name(
            app.settings.saved_config_ref(),
            &app.bus_states,
            &app.golfer_names,
            id,
        ),
        "Guest"
    );
}

#[test]
fn latest_chip_stays_while_a_trigger_has_no_data() {
    let mut shots = vec![row(1, true), row(2, true)];
    assert_eq!(
        header_chip(&shots, 1, true, ShotFilter::ALL),
        HeaderChip::Latest
    );
    // A trigger lands; its data has not.
    shots.push(row(3, false));
    assert_eq!(
        header_chip(&shots, 1, true, ShotFilter::ALL),
        HeaderChip::Latest
    );
    // An older shot pinned.
    assert_eq!(
        header_chip(&shots, 0, false, ShotFilter::ALL),
        HeaderChip::BackToLatest
    );
    assert_eq!(
        header_chip(&shots, 0, true, ShotFilter::ALL),
        HeaderChip::None
    );
    // The data lands: the new shot takes the chip.
    shots[2] = row(3, true);
    assert_eq!(
        header_chip(&shots, 2, true, ShotFilter::ALL),
        HeaderChip::Latest
    );
    assert_eq!(
        header_chip(&shots, 1, true, ShotFilter::ALL),
        HeaderChip::None
    );
}

#[test]
fn pulse_starts_on_new_shot_and_fades() {
    let mut focus = ShotFocus::default();
    let mut shots = vec![row(1, true)];
    focus.sync(&shots, ShotFilter::ALL);
    assert_eq!(focus.strength(0.0), 0.0);

    // A trigger without data yet does not change the latest shot.
    shots.push(row(2, false));
    focus.observe(&shots, ShotFilter::ALL, 10.0);
    assert_eq!(focus.strength(10.0), 0.0);

    shots[1].launch = Some(Launch::default());
    focus.observe(&shots, ShotFilter::ALL, 11.0);
    assert_eq!(focus.strength(11.0), 1.0);
    let mid = focus.strength(11.0 + PULSE_SECS / 2.0);
    assert!(mid > 0.0 && mid < 1.0);
    assert_eq!(focus.strength(11.0 + PULSE_SECS), 0.0);

    // Observing the same shot again does not restart the pulse.
    focus.observe(&shots, ShotFilter::ALL, 20.0);
    assert_eq!(focus.strength(20.0), 0.0);
}

#[test]
fn focus_index_prefers_listed_pin() {
    let shots = vec![row(1, true), row(2, true), row(3, false)];
    let pin = |n| ShotRef::of(&row(n, true));
    assert_eq!(focus_index(&shots, None, ShotFilter::ALL), Some(1));
    assert_eq!(focus_index(&shots, Some(&pin(1)), ShotFilter::ALL), Some(0));
    // A shot without data can be picked.
    assert_eq!(focus_index(&shots, Some(&pin(3)), ShotFilter::ALL), Some(2));
    // A pin that is not listed falls back to the latest.
    assert_eq!(focus_index(&shots, Some(&pin(9)), ShotFilter::ALL), Some(1));
    assert_eq!(focus_index(&[], Some(&pin(1)), ShotFilter::ALL), None);
}

#[test]
fn shot_ref_matches_without_ids() {
    let live = row(5, true);
    let mut loaded = row(5, true);
    loaded.shot_id.clear();
    let pin = ShotRef::of(&live);
    assert!(pin.matches(&live) && pin.matches(&loaded));
    assert!(ShotRef::of(&loaded).matches(&live));
    // Without an id on one side, the same number from another device is
    // not it.
    let mut other = loaded.clone();
    other.actor = "r10.0".into();
    assert!(!pin.matches(&other));
}

#[test]
fn shot_ref_prefers_the_id() {
    let pin = ShotRef::of(&row(5, true));
    // Same actor and number, another id: a different shot.
    let mut reused = row(5, true);
    reused.shot_id = "other".into();
    assert!(!pin.matches(&reused));
    // Same id and actor, different number: the same shot.
    let mut renumbered = row(6, true);
    renumbered.shot_id = "id5".into();
    assert!(pin.matches(&renumbered));
    // Same id from another device: a different shot.
    renumbered.actor = "r10.0".into();
    assert!(!pin.matches(&renumbered));
}

#[test]
fn selection_follows_latest_until_pinned() {
    let mut focus = ShotFocus::default();
    let mut shots = vec![row(1, true), row(2, true)];
    focus.sync(&shots, ShotFilter::ALL);
    assert_eq!(focus.index(&shots, ShotFilter::ALL), Some(1));
    assert!(focus.following(&shots, ShotFilter::ALL));

    // Picking an older shot pins it.
    focus.select(&shots, ShotFilter::ALL, 0);
    assert_eq!(focus.index(&shots, ShotFilter::ALL), Some(0));
    assert!(!focus.following(&shots, ShotFilter::ALL));
    // It stays pinned while a trigger without data comes in.
    shots.push(row(3, false));
    focus.observe(&shots, ShotFilter::ALL, 1.0);
    assert_eq!(focus.index(&shots, ShotFilter::ALL), Some(0));

    // The new shot's data lands: the view jumps to it, glowing.
    shots[2].launch = Some(Launch::default());
    focus.observe(&shots, ShotFilter::ALL, 2.0);
    assert_eq!(focus.index(&shots, ShotFilter::ALL), Some(2));
    assert!(focus.following(&shots, ShotFilter::ALL));
    assert_eq!(focus.strength(2.0), 1.0);
    assert!(focus.take_reveal());
    assert!(!focus.take_reveal());

    // Picking the latest shot, or Back to latest, follows again.
    focus.select(&shots, ShotFilter::ALL, 1);
    focus.select(&shots, ShotFilter::ALL, 2);
    assert!(focus.following(&shots, ShotFilter::ALL));
    focus.select(&shots, ShotFilter::ALL, 0);
    focus.follow();
    assert!(focus.following(&shots, ShotFilter::ALL));
}

#[test]
fn pin_survives_reload_until_shot_is_gone() {
    let mut focus = ShotFocus::default();
    let shots = vec![row(1, true), row(2, true), row(3, true)];
    focus.sync(&shots, ShotFilter::ALL);
    focus.select(&shots, ShotFilter::ALL, 1);

    // A reload that still lists the shot keeps it, even with new shots,
    // at a new index and without shot ids, and does not pulse.
    let mut reloaded = vec![row(2, true), row(3, true), row(4, true)];
    for s in &mut reloaded {
        s.shot_id.clear();
    }
    focus.sync(&reloaded, ShotFilter::ALL);
    assert_eq!(focus.index(&reloaded, ShotFilter::ALL), Some(0));
    assert_eq!(focus.strength(0.0), 0.0);

    // Once it is gone the view falls back to the latest, and stays there.
    let trimmed = vec![row(3, true), row(4, true)];
    focus.sync(&trimmed, ShotFilter::ALL);
    assert_eq!(focus.index(&trimmed, ShotFilter::ALL), Some(1));
    assert!(focus.following(&trimmed, ShotFilter::ALL));
    focus.sync(&reloaded, ShotFilter::ALL);
    assert_eq!(focus.index(&reloaded, ShotFilter::ALL), Some(2));
}

#[test]
fn repeated_shot_numbers_after_a_device_reconnect_are_told_apart_by_id() {
    let shot = |n, id: &str| ShotRow {
        shot_id: id.into(),
        ..row(n, true)
    };
    let mut focus = ShotFocus::default();
    let mut shots = vec![shot(1, "a1"), shot(2, "a2")];
    focus.sync(&shots, ShotFilter::ALL);
    focus.select(&shots, ShotFilter::ALL, 0);

    // The device reconnects and its shot #1 lands: a new latest shot,
    // though the actor and number match an earlier one.
    shots.push(shot(1, "b1"));
    focus.observe(&shots, ShotFilter::ALL, 5.0);
    assert_eq!(focus.strength(5.0), 1.0);
    assert_eq!(focus.index(&shots, ShotFilter::ALL), Some(2));
    assert!(focus.following(&shots, ShotFilter::ALL));

    // A pin on the earlier #1 finds that shot, not the newer #1, and
    // survives a reload.
    focus.select(&shots, ShotFilter::ALL, 0);
    let reloaded = shots.clone();
    focus.sync(&reloaded, ShotFilter::ALL);
    assert_eq!(focus.index(&reloaded, ShotFilter::ALL), Some(0));
    assert!(ShotRef::of(&shots[0]).matches(&shots[0]));
    assert!(!ShotRef::of(&shots[0]).matches(&shots[2]));
    assert!(!ShotRef::of(&shots[2]).matches(&shots[0]));
    // The newer #1 is found as itself too.
    focus.select(&shots, ShotFilter::ALL, 1);
    assert_eq!(focus.index(&shots, ShotFilter::ALL), Some(1));
    assert_eq!(
        focus_index(&shots, Some(&ShotRef::of(&shots[2])), ShotFilter::ALL),
        Some(2)
    );
}

#[test]
fn list_is_newest_first() {
    let shots = vec![row(1, true), row(2, false), row(3, true)];
    assert_eq!(
        list_items(&shots, &ShotGroups::default(), None, ShotFilter::ALL),
        vec![ListItem::Shot(2), ListItem::Shot(1), ListItem::Shot(0)]
    );
}

fn group(id: &str, number: u32, club: Club, open: bool, shots: u32) -> SetInfo {
    SetInfo {
        golfer: None,
        id: id.into(),
        number,
        session: 0,
        club: Some(club),
        started_at: 0,
        open,
        shots,
    }
}

/// `g` in session `session`.
fn sess(session: u32, g: SetInfo) -> SetInfo {
    SetInfo { session, ..g }
}

fn groups(mode: ShotGrouping, list: Vec<SetInfo>) -> ShotGroups {
    ShotGroups::with_sets(mode, list)
}

/// Rows oldest first, each `(set id, has data)`.
fn grouped(rows: &[(Option<&str>, bool)]) -> Vec<ShotRow> {
    rows.iter()
        .zip(1..)
        .map(|(&(g, data), n)| ShotRow {
            group: g.map(str::to_owned),
            ..row(n, data)
        })
        .collect()
}

fn header(id: &str) -> ListItem {
    ListItem::GroupHeader {
        id: id.into(),
        next: false,
    }
}

fn club_header(session: u32, club: Club) -> ListItem {
    ListItem::ClubHeader {
        golfer: None,
        session,
        club: Some(club),
        next: false,
    }
}

#[test]
fn view_none_has_no_headers() {
    let shots = grouped(&[(Some("a"), true), (Some("a"), true), (None, true)]);
    // Sets are recorded in every view; `none` shows no headers.
    let g = groups(
        ShotGrouping::None,
        vec![group("a", 1, Club::Iron7, true, 2)],
    );
    assert_eq!(
        list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL),
        vec![ListItem::Shot(2), ListItem::Shot(1), ListItem::Shot(0)]
    );
}

#[test]
fn by_set_runs_get_one_header_each() {
    let shots = grouped(&[
        (Some("a"), true),
        (Some("a"), true),
        (Some("a"), true),
        (Some("b"), true),
        (Some("b"), true),
    ]);
    let g = groups(
        ShotGrouping::BySet,
        vec![
            group("a", 1, Club::Iron7, false, 3),
            group("b", 2, Club::Driver, true, 2),
        ],
    );
    assert_eq!(
        list_items(&shots, &g, Some(Club::Driver), ShotFilter::ALL),
        vec![
            header("b"),
            ListItem::Shot(4),
            ListItem::Shot(3),
            header("a"),
            ListItem::Shot(2),
            ListItem::Shot(1),
            ListItem::Shot(0),
        ]
    );
}

#[test]
fn by_club_combines_a_clubs_sets_into_one_section() {
    // 7I (a), DR (b), 7I (c): returning to the 7-iron started set c, and
    // By club shows both 7-iron sets together, newest first.
    let shots = grouped(&[
        (Some("a"), true),
        (Some("a"), true),
        (Some("b"), true),
        (Some("c"), true),
        (Some("c"), true),
    ]);
    let g = groups(
        ShotGrouping::ByClub,
        vec![
            group("a", 1, Club::Iron7, false, 2),
            group("b", 2, Club::Driver, false, 1),
            group("c", 3, Club::Iron7, true, 2),
        ],
    );
    let sevens = [
        club_header(0, Club::Iron7),
        ListItem::Shot(4),
        ListItem::Shot(3),
        ListItem::Shot(1),
        ListItem::Shot(0),
    ];
    let driver = [club_header(0, Club::Driver), ListItem::Shot(2)];
    assert_eq!(
        list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL),
        [sevens.as_slice(), &driver].concat()
    );
    // The selected club's section comes first, though its last shot
    // is older.
    assert_eq!(
        list_items(&shots, &g, Some(Club::Driver), ShotFilter::ALL),
        [driver.as_slice(), &sevens].concat()
    );
    // New set does not split the section: an empty open 7-iron set
    // adds no header while the club has a section.
    let mut list = vec![
        group("a", 1, Club::Iron7, false, 2),
        group("b", 2, Club::Driver, false, 1),
        group("c", 3, Club::Iron7, false, 2),
        group("m", 4, Club::Iron7, true, 0),
    ];
    let g = groups(ShotGrouping::ByClub, list.clone());
    assert_eq!(
        list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL),
        [sevens.as_slice(), &driver].concat()
    );
    // Its next shot joins the same section.
    list[3].shots = 1;
    let mut more = shots.clone();
    more.push(ShotRow {
        group: Some("m".into()),
        ..row(6, true)
    });
    let g = groups(ShotGrouping::ByClub, list);
    let items = list_items(&more, &g, Some(Club::Iron7), ShotFilter::ALL);
    assert_eq!(items[..2], [club_header(0, Club::Iron7), ListItem::Shot(5)]);
    assert_eq!(items[6], club_header(0, Club::Driver));
    // A dead trigger stays with the run it is in.
    let mut shots = shots;
    shots.insert(2, row(9, false));
    let g = groups(
        ShotGrouping::ByClub,
        vec![
            group("a", 1, Club::Iron7, false, 2),
            group("b", 2, Club::Driver, false, 1),
            group("c", 3, Club::Iron7, true, 2),
        ],
    );
    let items = list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL);
    assert_eq!(
        items[5..],
        [
            club_header(0, Club::Driver),
            ListItem::Shot(3),
            ListItem::Shot(2)
        ]
    );
}

#[test]
fn by_club_starts_afresh_after_new_session() {
    // Session 0: 7I (a), DR (b). New session. Session 1: 7I (c).
    let shots = grouped(&[
        (Some("a"), true),
        (Some("a"), true),
        (Some("b"), true),
        (Some("c"), true),
        (Some("c"), true),
    ]);
    let mut g = groups(
        ShotGrouping::ByClub,
        vec![
            group("a", 1, Club::Iron7, false, 2),
            group("b", 2, Club::Driver, false, 1),
            sess(1, group("c", 3, Club::Iron7, true, 2)),
        ],
    );
    assert_eq!(g.session(), 1);
    assert_eq!(
        list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL),
        vec![
            club_header(1, Club::Iron7),
            ListItem::Shot(4),
            ListItem::Shot(3),
            club_header(0, Club::Driver),
            ListItem::Shot(2),
            club_header(0, Club::Iron7),
            ListItem::Shot(1),
            ListItem::Shot(0),
        ]
    );
    // Re-selecting a club from the earlier session does not lift its
    // earlier section.
    g.observe_club(Some(Club::Driver), &shots);
    let ids = section_ids(&list_items(
        &shots,
        &g,
        Some(Club::PitchingWedge),
        ShotFilter::ALL,
    ));
    assert_eq!(
        ids,
        [
            (1, Some(Club::Iron7)),
            (0, Some(Club::Driver)),
            (0, Some(Club::Iron7))
        ]
    );
    // Right after New session nothing is combined with the earlier
    // session: every section reads earlier until the next shot.
    g.new_session();
    let items = list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL);
    assert_eq!(items[0], club_header(1, Club::Iron7));
    assert_eq!(
        club_title(Some(Club::Iron7), 1 < g.session(), Some((1, 2))),
        "7 Iron \u{b7} Earlier \u{b7} 2 shots"
    );
}

/// The By club section keys of the headers in `items`, top to bottom.
fn section_ids(items: &[ListItem]) -> Vec<(u32, Option<Club>)> {
    items
        .iter()
        .filter_map(|i| match i {
            ListItem::ClubHeader {
                session,
                club,
                next: false,
                ..
            } => Some((*session, *club)),
            _ => None,
        })
        .collect()
}

/// The first `n` rows of `all`: the history as the UI held it then.
fn upto(all: &[ShotRow], n: usize) -> &[ShotRow] {
    &all[..n]
}

#[test]
fn by_club_sections_follow_last_activity() {
    // 7I ×3 (a), then DR ×2 (b).
    let shots = grouped(&[
        (Some("a"), true),
        (Some("a"), true),
        (Some("a"), true),
        (Some("b"), true),
        (Some("b"), true),
    ]);
    let mut g = groups(
        ShotGrouping::ByClub,
        vec![
            group("a", 1, Club::Iron7, false, 3),
            group("b", 2, Club::Driver, true, 2),
        ],
    );
    g.observe_club(Some(Club::Iron7), upto(&shots, 0));
    g.observe_club(Some(Club::Driver), upto(&shots, 3));
    let ids = |g: &ShotGroups, club| {
        section_ids(&list_items(&shots, g, club, ShotFilter::ALL))
            .iter()
            .map(|(_, c)| c.map_or("-".to_owned(), |c| c.to_string()))
            .collect::<Vec<_>>()
            .join(",")
    };
    // Selected club first; with a club that has no section, the newest
    // activity wins.
    assert_eq!(ids(&g, Some(Club::Driver)), "DR,7I");
    assert_eq!(ids(&g, Some(Club::Iron7)), "7I,DR");
    assert_eq!(ids(&g, Some(Club::PitchingWedge)), "DR,7I");
    // Re-selecting 7I with no new shot bumps its section above DR's
    // newer shots, after a switch away too.
    g.observe_club(Some(Club::Iron7), upto(&shots, 5));
    g.observe_club(Some(Club::PitchingWedge), upto(&shots, 5));
    assert_eq!(ids(&g, Some(Club::PitchingWedge)), "7I,DR");
    // So does selecting DR again.
    g.observe_club(Some(Club::Driver), upto(&shots, 5));
    g.observe_club(Some(Club::PitchingWedge), upto(&shots, 5));
    assert_eq!(ids(&g, Some(Club::PitchingWedge)), "DR,7I");
    // Observing the same club again is not a new selection.
    let before = g.selected_at(None, Some(Club::PitchingWedge), &shots);
    g.observe_club(Some(Club::PitchingWedge), &shots);
    assert_eq!(
        g.selected_at(None, Some(Club::PitchingWedge), &shots),
        before
    );
}

#[test]
fn by_club_loose_rows_gather_last() {
    // History in no set and a row in an unknown set sit in one section
    // after the club sections; pending rows stay on top.
    let shots = grouped(&[
        (None, true),
        (Some("a"), true),
        (Some("b"), true),
        (None, true),
        (Some("c"), true),
        (None, false),
    ]);
    let g = groups(
        ShotGrouping::ByClub,
        vec![
            group("a", 1, Club::Iron7, false, 1),
            group("b", 2, Club::Driver, false, 1),
            group("c", 3, Club::Iron7, true, 1),
        ],
    );
    assert_eq!(
        list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL),
        vec![
            ListItem::Shot(5),
            club_header(0, Club::Iron7),
            ListItem::Shot(4),
            ListItem::Shot(1),
            club_header(0, Club::Driver),
            ListItem::Shot(2),
            ListItem::Ungrouped,
            ListItem::Shot(3),
            ListItem::Shot(0),
        ]
    );
}

#[test]
fn headers_read_per_view() {
    let open = group("a", 3, Club::Iron7, true, 5);
    let closed = group("b", 2, Club::Iron7, false, 4);
    let one = group("c", 4, Club::Driver, true, 1);
    let mut none = group("d", 5, Club::Iron7, true, 2);
    none.club = None;
    assert_eq!(set_title(&open, false), "Set 3 \u{b7} 7I \u{b7} 5 shots");
    assert_eq!(set_title(&closed, false), "Set 2 \u{b7} 7I \u{b7} 4 shots");
    assert_eq!(set_title(&one, true), "Set 4 \u{b7} DR \u{b7} Next shot");
    assert_eq!(
        set_title(&none, false),
        "Set 5 \u{b7} No club \u{b7} 2 shots"
    );

    let i7 = Some(Club::Iron7);
    assert_eq!(club_title(i7, false, Some((1, 5))), "7 Iron \u{b7} 5 shots");
    assert_eq!(
        club_title(i7, false, Some((2, 9))),
        "7 Iron \u{b7} 2 sets \u{b7} 9 shots"
    );
    assert_eq!(
        club_title(i7, true, Some((1, 4))),
        "7 Iron \u{b7} Earlier \u{b7} 4 shots"
    );
    assert_eq!(
        club_title(Some(Club::Driver), false, Some((1, 1))),
        "Driver \u{b7} 1 shot"
    );
    assert_eq!(club_title(i7, false, None), "7 Iron \u{b7} Next shot");
    assert_eq!(
        club_title(None, false, Some((1, 2))),
        "No club \u{b7} 2 shots"
    );
}

#[test]
fn long_list_headers_keep_their_shot_count() {
    let i7 = Some(Club::Iron7);
    assert_eq!(
        club_header_title(i7, true, Some((2, 14))),
        "7 Iron \u{b7} Earlier \u{b7} 14 shots"
    );
    assert_eq!(
        club_header_title(i7, false, Some((2, 14))),
        "7 Iron \u{b7} 2 sets \u{b7} 14 shots"
    );
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        let painter = ctx.layer_painter(egui::LayerId::background());
        // The narrowest default list (212 px): the earlier 7-iron
        // section fits whole.
        let width = *LIST_DEFAULT_WIDTH.start() - 8.0 - 2.0 * f32::from(GLOW_WIDTH) - 10.0;
        let title = club_header_title(i7, true, Some((2, 14)));
        let (head, tail) = header_text(&painter, &title, width, theme::TEXT_MUTED);
        assert!(tail.is_none(), "{title} fits in {width}");
        assert!(head.size().x <= width);
        // A longer one is split before the count, which stays whole.
        let long = club_header_title(Some(Club::PitchingWedge), false, Some((3, 25)));
        let (head, tail) = header_text(&painter, &long, width, theme::TEXT_MUTED);
        let tail = tail.expect("the count is kept apart");
        assert_eq!(tail.text(), " \u{b7} 25 SHOTS");
        assert_eq!(head.text(), "PITCHING WEDGE \u{b7} 3 SETS");
        assert!(head.size().x + tail.size().x <= width + 0.5);
        // Narrower still, the head is cut and the whole still fits.
        let (head, tail) = header_text(&painter, &long, 140.0, theme::TEXT_MUTED);
        let tail = tail.expect("the count is kept apart");
        assert_eq!(tail.text(), " \u{b7} 25 SHOTS");
        assert!(head.size().x + tail.size().x <= 140.5);
    });
}

#[test]
fn club_names_are_spelled_out() {
    let names: Vec<_> = [
        Club::Driver,
        Club::MiniDriver,
        Club::Wood3,
        Club::Hybrid4,
        Club::Iron7,
        Club::PitchingWedge,
        Club::GapWedge,
        Club::SandWedge,
        Club::LobWedge,
        Club::Putter,
    ]
    .map(club_name)
    .into();
    assert_eq!(
        names,
        [
            "Driver",
            "Mini Driver",
            "3 Wood",
            "4 Hybrid",
            "7 Iron",
            "Pitching Wedge",
            "Gap Wedge",
            "Sand Wedge",
            "Lob Wedge",
            "Putter",
        ]
    );
    // Every club has its own name.
    let mut all: Vec<_> = Club::ALL.iter().map(|&c| club_name(c)).collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), Club::ALL.len());
}

#[test]
fn empty_open_set_heads_the_list() {
    let shots = grouped(&[(Some("a"), true), (Some("a"), true)]);
    let list = vec![
        group("a", 1, Club::Iron7, false, 2),
        group("c", 3, Club::Iron7, true, 0),
    ];
    let next = ListItem::GroupHeader {
        id: "c".into(),
        next: true,
    };
    let g = groups(ShotGrouping::BySet, list.clone());
    let items = list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL);
    assert_eq!(
        items,
        vec![
            next.clone(),
            header("a"),
            ListItem::Shot(1),
            ListItem::Shot(0)
        ]
    );
    assert_eq!(
        set_title(g.get("c").unwrap(), true),
        "Set 3 \u{b7} 7I \u{b7} Next shot"
    );
    // It takes any club: a different club in play is still its header.
    assert_eq!(
        list_items(&shots, &g, Some(Club::Driver), ShotFilter::ALL)[0],
        next
    );

    // By club: the club in play already has a section, which is the
    // first line; no extra header.
    let g = groups(ShotGrouping::ByClub, list);
    let items = list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL);
    assert_eq!(items[0], club_header(0, Club::Iron7));
    assert_eq!(items.len(), 3);
    // A club with no section yet this session gets a next header.
    let items = list_items(&shots, &g, Some(Club::Driver), ShotFilter::ALL);
    assert_eq!(
        items[0],
        ListItem::ClubHeader {
            golfer: None,
            session: 0,
            club: Some(Club::Driver),
            next: true
        }
    );
    assert_eq!(items[1], club_header(0, Club::Iron7));
}

#[test]
fn pending_and_ungrouped_rows_come_first() {
    // Oldest first: history in no set, the 7I set with a dead trigger
    // inside it, a row whose set event has not arrived yet, and a shot
    // in flight.
    let shots = grouped(&[
        (None, true),
        (Some("a"), true),
        (None, false),
        (Some("a"), true),
        (None, true),
        (Some("unknown"), true),
        (None, false),
    ]);
    let g = groups(
        ShotGrouping::BySet,
        vec![group("a", 1, Club::Iron7, true, 2)],
    );
    assert_eq!(
        list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL),
        vec![
            ListItem::Shot(6),
            ListItem::Shot(5),
            ListItem::Shot(4),
            header("a"),
            ListItem::Shot(3),
            ListItem::Shot(2),
            ListItem::Shot(1),
            ListItem::Ungrouped,
            ListItem::Shot(0),
        ]
    );

    // Under an empty open set's header, the pending row stays on top
    // and the run in no set gets its own header.
    let g = groups(
        ShotGrouping::BySet,
        vec![
            group("a", 1, Club::Iron7, false, 2),
            group("c", 2, Club::Iron7, true, 0),
        ],
    );
    let items = list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL);
    assert_eq!(
        items[..5],
        [
            ListItem::GroupHeader {
                id: "c".into(),
                next: true
            },
            ListItem::Shot(6),
            ListItem::Ungrouped,
            ListItem::Shot(5),
            ListItem::Shot(4),
        ]
    );
}

#[test]
fn new_set_hint_follows_the_view() {
    assert!(new_group_hint(ShotGrouping::ByClub).contains("until New session"));
    assert_eq!(
        new_group_hint(ShotGrouping::BySet),
        "Close this set; the next shot starts a fresh one."
    );
}

#[test]
fn foot_is_pinned_below_the_list_and_hidden_in_view_none() {
    let open7 = group("a", 1, Club::Iron7, true, 3);
    let strip = |mode, club| groups(mode, vec![open7.clone()]).strip(club);
    // None: no buttons, no hint, no foot.
    let none = strip(ShotGrouping::None, Some(Club::Iron7));
    assert!(!none.actions && none.hint.is_none());
    assert_eq!(list_foot_height(&none), 0.0);
    // By set and By club: the buttons, plus the hint line when there is one.
    let plain = strip(ShotGrouping::BySet, Some(Club::Iron7));
    let hinted = strip(ShotGrouping::BySet, Some(Club::Driver));
    assert!(plain.actions && plain.hint.is_none());
    assert!(hinted.hint.is_some());
    assert!(list_foot_height(&plain) >= STRIP_BUTTONS_HEIGHT);
    assert_eq!(
        list_foot_height(&hinted),
        list_foot_height(&plain) + STRIP_HINT_HEIGHT + FOOT_SPACING
    );
    let by_club = strip(ShotGrouping::ByClub, Some(Club::Iron7));
    assert_eq!(list_foot_height(&by_club), list_foot_height(&plain));
}

#[test]
fn table_insets_its_first_column_past_the_stripe() {
    // One device: no Device column, so `#` is first and clears the stripe.
    assert_eq!(column_inset(LeadColumn::Number, false), ROW_INSET);
    // Two devices: Device is first; `#` sits in the grid as usual.
    assert_eq!(column_inset(LeadColumn::Device, true), ROW_INSET);
    assert_eq!(column_inset(LeadColumn::Number, true), 0.0);
}

#[test]
fn smash_falls_back_to_speed_ratio() {
    let mph = Velocity::MilesPerHour;
    assert_eq!(
        smash(Some(1.45), Some(mph(150.0)), Some(mph(100.0))),
        Some(1.45)
    );
    assert_eq!(smash(None, Some(mph(150.0)), Some(mph(100.0))), Some(1.5));
    // Mixed units are compared like for like.
    let s = smash(
        None,
        Some(Velocity::MetersPerSecond(67.056)),
        Some(mph(100.0)),
    );
    assert!((s.unwrap() - 1.5).abs() < 1e-4);
    assert_eq!(smash(None, None, Some(mph(100.0))), None);
    assert_eq!(smash(None, Some(mph(150.0)), None), None);
    assert_eq!(smash(None, Some(mph(150.0)), Some(mph(0.0))), None);
}

/// Every card of `layout`, headline first.
fn all_cards(layout: &CardsLayout) -> Vec<egui::Rect> {
    let details = layout.groups.iter().flat_map(|(_, cards)| cards.iter());
    layout.headline.iter().chain(details).copied().collect()
}

fn assert_fits(layout: &CardsLayout, width: f32) {
    let cards = all_cards(layout);
    for (i, a) in cards.iter().enumerate() {
        assert!(
            a.left() >= 0.0 && a.right() <= width + 0.01,
            "{a:?} overflows"
        );
        assert!(a.top() >= layout.header.bottom() && a.bottom() <= layout.height + 0.01);
        for b in &cards[i + 1..] {
            assert!(!a.shrink(0.5).intersects(*b), "{a:?} overlaps {b:?}");
        }
    }
}

#[test]
fn cards_sit_side_by_side_when_wide() {
    let size = egui::vec2(1560.0, 760.0);
    let layout = cards_layout(size, 5, &[2, 4, 6], None, 1);
    assert_fits(&layout, size.x);
    // One headline row, and all group labels on one line.
    assert!(
        layout
            .headline
            .iter()
            .all(|r| r.top() == layout.headline[0].top())
    );
    let label_y = layout.groups[0].0.top();
    assert!(layout.groups.iter().all(|(l, _)| l.top() == label_y));
    // Headline cards are taller, and the spare height is split.
    assert!(layout.headline[0].height() > layout.groups[0].1[0].height());
    assert!(layout.header.top() > 0.0 && layout.height < size.y);
}

#[test]
fn cards_stack_and_wrap_when_narrow() {
    let size = egui::vec2(420.0, 700.0);
    let layout = cards_layout(size, 5, &[2, 4, 6], None, 1);
    assert_fits(&layout, size.x);
    // Headline wraps into balanced rows (3 + 2).
    let first_row = layout.headline[0].top();
    let in_first = layout
        .headline
        .iter()
        .filter(|r| r.top() == first_row)
        .count();
    assert_eq!(in_first, 3);
    // Groups stack, each label below the previous group's cards.
    for pair in layout.groups.windows(2) {
        let prev_bottom = pair[0].1.iter().map(|r| r.bottom()).fold(0.0, f32::max);
        assert!(pair[1].0.top() > prev_bottom);
    }
    // Too short to fit: cards keep their compact height and scroll.
    let short = cards_layout(egui::vec2(420.0, 300.0), 5, &[2, 4, 6], None, 1);
    assert_fits(&short, 420.0);
    assert!(short.height > 300.0 && short.scrolls);
    assert_eq!(short.headline[0].height(), COMPACT.headline);
    assert_eq!(short.groups[0].1[0].height(), COMPACT.detail);
}

#[test]
fn cards_pair_groups_when_medium() {
    // Too narrow for six cards side by side, wide enough for five
    // headline cards in a row.
    let size = egui::vec2(660.0, 620.0);
    let layout = cards_layout(size, 5, &[2, 4, 6], None, 1);
    assert_fits(&layout, size.x);
    assert!(layout.height <= size.y);
    // One headline row of five.
    assert!(
        layout
            .headline
            .iter()
            .all(|r| r.top() == layout.headline[0].top())
    );
    // Ball over Flight on the left, Club on the right, two cards wide.
    let [ball, flight, club] = &layout.groups[..] else {
        panic!("three groups");
    };
    assert_eq!(ball.0.left(), flight.0.left());
    assert!(flight.0.top() > ball.1[0].bottom());
    assert!(club.0.left() > flight.0.right());
    assert_eq!(club.0.top(), ball.0.top());
    assert_eq!(club.1[1].top(), club.1[0].top());
    assert!(club.1[2].top() > club.1[0].top());
    // Detail cards stay shorter than the headline cards.
    assert!(club.1[0].height() < layout.headline[0].height());
}

fn stats(values: &[f64]) -> Option<Stats> {
    Stats::of(values.iter().copied())
}

#[test]
fn distance_domain_is_shared_and_padded() {
    // Total 230-250 and carry 210-225 share 210-250; 15 % of 40 is
    // under the 10 yd floor.
    let total = stats(&[230.0, 250.0]);
    let carry = stats(&[210.0, 225.0]);
    assert_eq!(distance_domain(&[total, carry], true), Some((200.0, 260.0)));
    // Meters have a 9 m floor.
    assert_eq!(
        distance_domain(&[total, carry], false),
        Some((201.0, 259.0))
    );
    // A wide spread pads by 15 % of its range.
    let wide = stats(&[100.0, 300.0]);
    assert_eq!(distance_domain(&[wide, None], true), Some((70.0, 330.0)));
    // One bar with data sets the domain alone; neither gives none.
    assert_eq!(distance_domain(&[None, carry], true), Some((200.0, 235.0)));
    assert_eq!(distance_domain(&[None, None], true), None);
    // A single value still gets the floor on both sides.
    let one = stats(&[150.0]);
    assert_eq!(distance_domain(&[one, None], true), Some((140.0, 160.0)));
}

#[test]
fn lateral_domain_is_symmetric_with_a_floor() {
    let close = |(lo, hi): (f64, f64), want: f64| {
        assert!(
            (lo + want).abs() < 1e-9 && (hi - want).abs() < 1e-9,
            "{lo} {hi}"
        );
    };
    // The widest side sets both, with the margin.
    close(lateral_domain(stats(&[-3.0, 8.0]).as_ref()), 9.6);
    close(lateral_domain(stats(&[-12.0, 1.0]).as_ref()), 14.4);
    // A tight group, or no data, falls back to the floor.
    close(lateral_domain(stats(&[-1.0, 2.0]).as_ref()), 6.0);
    close(lateral_domain(None), 6.0);
    // The target line is the middle of the plot.
    let plot = egui::Rangef::new(100.0, 300.0);
    assert_eq!(bar_x(lateral_domain(None), plot, 0.0), 200.0);
}

#[test]
fn one_value_draws_a_pip() {
    let plot = egui::Rangef::new(0.0, 200.0);
    let pip = band_x((0.0, 100.0), plot, 50.0, 50.0);
    assert_eq!((pip.min, pip.max), (92.0, 108.0));
    // A narrow range widens to the pip about its middle; a wide one is
    // drawn as is.
    let narrow = band_x((0.0, 100.0), plot, 50.0, 51.0);
    assert_eq!((narrow.min, narrow.max), (93.0, 109.0));
    let wide = band_x((0.0, 100.0), plot, 20.0, 60.0);
    assert!((wide.min - 40.0).abs() < 1e-3 && (wide.max - 120.0).abs() < 1e-3);
    // Values outside the domain are clamped to the plot.
    assert_eq!(bar_x((0.0, 100.0), plot, 150.0), 200.0);
}

#[test]
fn coincident_markers_nest_with_expected_sizes() {
    // The average dot fits inside the median ring's hollow center, so a
    // value shared by every marker reads as a ring filled by the dot,
    // with the this-shot line drawn through both.
    let ring_inner_radius = MEDIAN_RING_RADIUS - MEDIAN_RING_STROKE / 2.0;
    assert!(AVG_DOT_RADIUS < ring_inner_radius);
    // The ring sits just inside the track, not poking out of it; only
    // the this-shot line sticks out past the bar. Both hold by
    // construction, so the compiler checks them as a build-time
    // invariant rather than a runtime assertion on constants.
    const { assert!(MEDIAN_RING_RADIUS < TRACK_H / 2.0) };
    const { assert!(TRACK_H / 2.0 + MARKER_OVERHANG > TRACK_H / 2.0) };
}

/// A row with data whose total, carry and offline are measured (`m`)
/// and simulated (`d`), in yards.
fn flight(
    n: u32,
    m: (Option<f64>, Option<f64>),
    d: (Option<f64>, Option<f64>, Option<f64>),
) -> ShotRow {
    let yd = |v: Option<f64>| v.map(Distance::Yards);
    let flight = FlightResult {
        total_distance: yd(m.0),
        carry_distance: yd(m.1),
        ..FlightResult::new(Basis::Modeled)
    };
    let mut derived = crate::types::DerivedFlight::default();
    derived.total_distance = yd(d.0);
    derived.carry_distance = yd(d.1);
    derived.carry_offline = yd(d.2);
    ShotRow {
        launch: Some(Launch::default()),
        flight: Some(flight),
        derived: Some(derived),
        group: Some("a".into()),
        ..row(n, true)
    }
}

#[test]
fn bar_values_pick_each_shots_value_and_count_missing() {
    let rows = vec![
        // Measured only.
        flight(1, (Some(250.0), Some(230.0)), (None, None, Some(-4.0))),
        // Simulated only.
        flight(2, (None, None), (Some(245.0), Some(225.0), Some(6.0))),
        // No total or offline at all (e.g. the sim was gated).
        flight(3, (None, Some(228.0)), (None, None, None)),
        // A trigger with no data yet is not counted.
        row(4, false),
    ];
    let total = bar_values(&rows, BarKind::Total, true);
    assert_eq!(
        total,
        BarValues {
            values: vec![250.0, 245.0],
            rows: 3
        }
    );
    let carry = bar_values(&rows, BarKind::Carry, true);
    assert_eq!(carry.values, vec![230.0, 225.0, 228.0]);
    // Lateral is the simulated carry offline, always.
    let lateral = bar_values(&rows, BarKind::Lateral, true);
    assert_eq!(lateral.values, vec![-4.0, 6.0]);
    // Metric converts.
    let m = bar_values(&rows[..1], BarKind::Total, false);
    assert!((m.values[0] - 228.6).abs() < 1e-9);
}

#[test]
fn bars_mark_the_shot_and_report_spread() {
    let mut rows: Vec<ShotRow> = [240.0, 250.0, 245.0]
        .iter()
        .zip(1..)
        .map(|(&t, n)| flight(n, (Some(t), None), (None, Some(t - 20.0), Some(1.5))))
        .collect();
    // A shot of another group, and one in this group without a total.
    rows.push(ShotRow {
        group: Some("b".into()),
        ..flight(4, (Some(300.0), None), (None, None, None))
    });
    rows.push(flight(5, (None, None), (None, Some(222.0), None)));
    // The members of set "a".
    let a: Vec<usize> = vec![0, 1, 2, 4];
    let [total, carry, lateral] = bars_for(&rows, &a, Some(1), &[], true);
    assert_eq!(total.values.values, vec![240.0, 250.0, 245.0]);
    assert_eq!(total.this, Some(250.0));
    let row = |avg: &str, med: &str, sd: &str, count: Option<&str>| StatsRow {
        avg: avg.into(),
        med: med.into(),
        sd: sd.into(),
        count: count.map(Into::into),
    };
    assert_eq!(StatsRow::of(&total), row("245", "245", "5.0", Some("3/4")));
    // Total and Carry share a domain; lateral has its own.
    assert_eq!(total.domain, carry.domain);
    assert_eq!(total.domain, Some((210.0, 260.0)));
    assert_eq!(
        StatsRow::of(&lateral),
        row("1.5 R", "1.5 R", "0.0", Some("3/4"))
    );
    // A shown shot from another group gets no dot.
    let [total, ..] = bars_for(&rows, &a, Some(3), &[], true);
    assert_eq!(total.this, None);

    // One shot: no sd; no group or no values: no data and no domain.
    let one = bars_for(&rows[..1], &[0], Some(0), &[], true);
    assert_eq!(StatsRow::of(&one[0]), row("240", "240", NO_VALUE, None));
    assert_eq!(one[0].this, Some(240.0));
    let none = bars_for(&rows, &[], Some(0), &[], true);
    assert!(none.iter().all(|b| b.stats.is_none() && b.domain.is_none()));
    assert_eq!(
        StatsRow::of(&none[0]),
        row(NO_VALUE, NO_VALUE, NO_VALUE, None)
    );
}

#[test]
fn bars_show_the_shown_shots_set() {
    let list = vec![
        group("a", 1, Club::Iron7, false, 3),
        group("b", 2, Club::Driver, false, 2),
        group("c", 3, Club::Iron7, true, 1),
    ];
    let in_a = ShotRow {
        group: Some("a".into()),
        ..row(1, true)
    };
    let loose = row(2, true);
    let stale = ShotRow {
        group: Some("gone".into()),
        ..row(3, true)
    };
    let id = |g: Option<&SetInfo>| g.map(|g| g.id.clone());
    // The same in every view: the shot's own set, even a closed one;
    // else the set the next shot joins.
    for mode in [
        ShotGrouping::BySet,
        ShotGrouping::ByClub,
        ShotGrouping::None,
    ] {
        let g = groups(mode, list.clone());
        assert_eq!(
            id(bar_group(&g, Some(&in_a), Some(Club::Driver))),
            Some("a".into())
        );
        assert_eq!(
            id(bar_group(&g, Some(&loose), Some(Club::Iron7))),
            Some("c".into())
        );
        // A different club starts a new set: nothing to show yet.
        assert_eq!(id(bar_group(&g, Some(&loose), Some(Club::Driver))), None);
        assert_eq!(id(bar_group(&g, Some(&stale), None)), None);
        assert_eq!(id(bar_group(&g, None, Some(Club::Iron7))), Some("c".into()));
    }
}

#[test]
fn bars_sit_between_headline_and_details() {
    let size = egui::vec2(1560.0, 760.0);
    let plain = cards_layout(size, 5, &[2, 4, 6], None, 1);
    assert!(plain.bars.is_none());
    let layout = cards_layout(size, 5, &[2, 4, 6], Some(Legend::Beside), 0);
    assert_fits(&layout, size.x);
    assert!(layout.height <= size.y + 1e-3, "{}", layout.height);
    let BarsLayout {
        controls,
        label,
        legend,
        rows,
    } = layout.bars.expect("bars");
    assert_eq!(legend, label, "the legend sits on the label line");
    let headline_bottom = layout.headline[0].bottom();
    assert_eq!(controls.top(), headline_bottom + ROOMY.controls_gap);
    assert_eq!(label.top(), controls.bottom());
    assert_eq!(rows[0].top(), label.bottom());
    assert_eq!(
        rows[2].bottom() - controls.top(),
        ROOMY.bars(Legend::Beside)
    );
    assert_eq!(
        layout.groups[0].0.top(),
        controls.top() + ROOMY.bars(Legend::Beside) + ROOMY.group_gap
    );
    assert!(rows.iter().all(|r| r.width() == size.x));
    assert_eq!(controls.width(), size.x);
    // The cards give up height to make room.
    assert!(layout.headline[0].height() < plain.headline[0].height());
}

#[test]
fn bars_label_only_the_band_ends() {
    use egui::{Pos2, Rect, vec2};

    // Totals 163..181, carries 153..171: the shared domain is 143..191
    // (10 yd of padding), lateral -10.04..4.9 (domain +-12.05).
    let rows = vec![
        device_shot(1, "mevo.0", "a", 163.0, -10.04),
        device_shot(2, "mevo.0", "a", 181.0, 4.9),
    ];
    let bars = bars_for(&rows, &[0, 1], Some(1), &[], true);
    assert_eq!(bars[0].domain, Some((143.0, 191.0)));
    let title = ("Set 1 \u{b7} 7I \u{b7} 2 shots".to_owned(), theme::ACCENT);
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let width = 1000.0;
    let out = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new())
                .show(ctx, |ui| {
                    let legend = legend_place(ui.painter(), width, Some(&title.0), &[]);
                    let at = cards_layout(vec2(width, 760.0), 5, &[2, 4, 6], Some(legend), 0);
                    let at = at.bars.expect("bars");
                    range_bars(ui, &at, Some(&title), &bars, &[], "");
                });
        },
    );
    let texts: Vec<String> = painted_texts(&out.shapes)
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    // The band ends are labeled ...
    for label in ["163", "181", "153", "171", "10.0 L", "4.9 R"] {
        assert!(texts.iter().any(|t| t == label), "{label} in {texts:?}");
    }
    // ... and the track ends carry no axis extent or direction letters.
    for label in ["143", "191", "L", "R"] {
        assert!(!texts.iter().any(|t| t == label), "{label} in {texts:?}");
    }
}

#[test]
fn bars_legend_wraps_below_a_title_it_would_overlap() {
    use egui::{Pos2, Rect, vec2};

    let chips = vec![
        ("Open Connect".to_owned(), theme::ACCENT),
        ("Omni Bridge".to_owned(), theme::HEADING),
    ];
    let title = ("Set 2 \u{b7} 7I \u{b7} 10 shots".to_owned(), theme::ACCENT);
    let bars = bars_for(&[], &[], None, &[], true);
    let ctx = egui::Context::default();
    theme::install(&ctx);
    // Cards widths: 1600 with both panels, then about 455 px, then
    // narrower still; each with room to spare and at compact sizes.
    for (width, expect, height) in [
        (1000.0, Legend::Beside, 760.0),
        (455.0, Legend::Below, 760.0),
        (300.0, Legend::Below, 760.0),
        (1000.0, Legend::Beside, 300.0),
        (455.0, Legend::Below, 300.0),
    ] {
        let mut place = None;
        let mut layout = None;
        let out = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show(ctx, |ui| {
                        let legend = legend_place(ui.painter(), width, Some(&title.0), &chips);
                        let at = cards_layout(vec2(width, height), 5, &[2, 4, 6], Some(legend), 0);
                        let bars_at = at.bars.expect("bars");
                        range_bars(ui, &bars_at, Some(&title), &bars, &chips, "");
                        place = Some(legend);
                        layout = Some(at);
                    });
            },
        );
        assert_eq!(place, Some(expect), "at {width}");
        let layout = layout.expect("laid out");
        let at = layout.bars.expect("bars");
        assert_fits(&layout, width);
        // Every painted text, by position.
        let mut texts: Vec<(String, Rect)> = Vec::new();
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape {
                texts.push((t.galley.text().to_owned(), t.visual_bounding_rect()));
            }
        }
        let title_rect = texts
            .iter()
            .find(|(t, _)| t.starts_with("SET 2"))
            .map(|(_, r)| *r)
            .expect("title painted");
        let legend: Vec<Rect> = texts
            .iter()
            .filter(|(t, _)| {
                [
                    "this shot",
                    "median",
                    "avg",
                    "range",
                    "Open Connect",
                    "Omni Bridge",
                ]
                .contains(&t.as_str())
            })
            .map(|(_, r)| *r)
            .collect();
        assert_eq!(legend.len(), 6, "at {width}");
        for r in &legend {
            assert!(
                !r.intersects(title_rect),
                "at {width}: {r:?} over {title_rect:?}"
            );
            assert!(r.bottom() <= at.rows[0].top(), "above the bars at {width}");
        }
        match expect {
            Legend::Beside => {
                assert_eq!(at.legend, at.label);
                assert!(legend.iter().all(|r| r.left() > title_rect.right()));
            }
            Legend::Below => {
                assert!(legend.iter().all(|r| r.top() >= title_rect.bottom()));
                assert!(at.legend.top() > at.label.top());
                assert_eq!(at.rows[0].top(), at.legend.bottom());
            }
        }
    }
    // Without a title (a note instead) the legend is not drawn.
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        let painter = ctx.layer_painter(egui::LayerId::background());
        assert_eq!(legend_place(&painter, 100.0, None, &chips), Legend::Beside);
    });
}

/// The windows the Cards view must fit without scrolling.
const FIT_WINDOWS: [(f32, f32); 5] = [
    (1920.0, 1080.0),
    (1600.0, 900.0),
    (1440.0, 900.0),
    (1280.0, 800.0),
    (1024.0, 768.0),
];

/// Space the app takes around the Shots tab: its header bar (53 px and a
/// 1 px separator) and the 14 px content margin.
const TAB_MARGIN: egui::Margin = egui::Margin {
    left: 14,
    right: 14,
    top: 54 + 14,
    bottom: 14,
};

/// Filled bar-thin rects within 12 px left of `right`: a solid scroll
/// bar's track and handle.
fn scroll_bars(out: &egui::FullOutput, right: f32) -> usize {
    out.shapes
        .iter()
        .filter(|c| match &c.shape {
            egui::Shape::Rect(r) => {
                r.rect.width() <= 10.0
                    && r.rect.height() > 40.0
                    && r.rect.left() >= right - 12.0
                    && r.rect.right() <= right + 0.5
                    && r.fill.a() > 0
            }
            _ => false,
        })
        .count()
}

/// One frame of the Shots tab in a `window`-sized app.
fn shots_frame(
    ctx: &egui::Context,
    app: &mut FlighthookApp,
    window: egui::Vec2,
) -> egui::FullOutput {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, window)),
        ..Default::default()
    };
    ctx.run(input, |ctx| {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(TAB_MARGIN))
            .show(ctx, |ui| app.render_shots_panel(ui));
    })
}

/// An app on the Cards view with a set of 12 shots from two devices.
fn cards_app() -> FlighthookApp {
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.shots = thousand_rows()
        .into_iter()
        .take(12)
        .map(|r| ShotRow {
            group: Some("a".into()),
            ..r
        })
        .collect();
    app.groups = groups(
        ShotGrouping::BySet,
        vec![group("a", 1, Club::Iron7, true, 12)],
    );
    app.shots_rev = 1;
    app.shots_view = ShotsView::Cards;
    app
}

/// The Cards area (inside the glow inset and the foot) at each of
/// [`FIT_WINDOWS`], then at 900×700, with the shot list at its default
/// width; and the narrower width each takes with the list at its
/// widest default (opened at 1920, then resized).
const FIT_AREAS: [(f32, f32, f32); 6] = [
    (1212.0, 606.0, 1212.0),
    (962.0, 476.0, 928.0),
    (842.0, 476.0, 806.0),
    (721.0, 406.0, 685.0),
    (772.0, 404.0, 736.0),
    (640.0, 336.0, 604.0),
];

#[test]
fn cards_fit_every_listed_window_without_scrolling() {
    for (i, &(w, h, narrow)) in FIT_AREAS[..FIT_WINDOWS.len()].iter().enumerate() {
        for width in [w, narrow] {
            for (legend, want) in [None, Some(Legend::Beside), Some(Legend::Below)]
                .into_iter()
                .flat_map(|legend| (1..=MAX_CARDS).map(move |want| (legend, want)))
            {
                let layout = cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], legend, want);
                let at = format!("{:?} {width}x{h} {legend:?} {want}", FIT_WINDOWS[i]);
                assert!(!layout.scrolls, "{at}");
                assert!(layout.height <= h + 0.01, "{at}: {}", layout.height);
                assert_fits(&layout, width);
                // One headline row, and the details in two rows.
                let head = layout.headline[0].top();
                assert!(layout.headline.iter().all(|r| r.top() == head), "{at}");
                let rows: std::collections::BTreeSet<i32> = layout
                    .groups
                    .iter()
                    .flat_map(|(_, c)| c.iter().map(|r| r.top().round() as i32))
                    .collect();
                assert_eq!(rows.len(), 2, "{at}");
                // Never under the compact sizes.
                assert!(layout.headline[0].height() >= COMPACT.headline, "{at}");
                assert!(layout.groups[0].1[0].height() >= COMPACT.detail, "{at}");
            }
        }
    }
}

#[test]
fn numerals_stay_readable_at_the_smallest_cards() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    // Wide numerals and units in every card.
    let mut shot = thousand_rows().remove(0);
    shot.club = Some(ClubData {
        club_speed: Some(Velocity::MilesPerHour(108.8)),
        path: Some(-10.8),
        attack_angle: Some(-10.8),
        face_angle: Some(-10.8),
        dynamic_loft: Some(48.8),
        ..Default::default()
    });
    let (headline, groups) = card_metrics(&shot, true, Handedness::Right);
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        let painter = ctx.layer_painter(egui::LayerId::background());
        for &(w, h, narrow) in &FIT_AREAS {
            for width in [w, narrow] {
                let layout =
                    cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], Some(Legend::Below), 1);
                let head = layout.headline[0];
                let look = CardLook::headline(head.size()).fit(&painter, &headline, head.width());
                assert!(
                    look.value >= HEADLINE_VALUE_MIN,
                    "{width}x{h}: {}",
                    look.value
                );
                let detail = layout.groups[0].1[0];
                let all = groups.iter().flat_map(|(_, m)| m);
                let look = CardLook::detail(detail.size()).fit(&painter, all, detail.width());
                assert!(
                    look.value >= DETAIL_VALUE_MIN,
                    "{width}x{h}: {}",
                    look.value
                );
            }
        }
    });
    // The floors hold before fitting, however short the card.
    assert_eq!(
        CardLook::headline(egui::vec2(200.0, 40.0)).value,
        HEADLINE_VALUE_MIN
    );
    assert_eq!(
        CardLook::detail(egui::vec2(140.0, 30.0)).value,
        DETAIL_VALUE_MIN
    );
}

#[test]
fn short_of_room_every_size_shrinks_together() {
    assert_eq!(Rhythm::between(0.0), COMPACT);
    assert_eq!(Rhythm::between(1.0), ROOMY);
    assert_eq!(Rhythm::between(-1.0), COMPACT);
    assert_eq!(Rhythm::between(2.0), ROOMY);
    // Every compact size is the smaller one.
    let field = |r: Rhythm| {
        [
            r.header,
            r.header_gap,
            r.headline,
            r.controls_gap,
            r.controls,
            r.label,
            r.legend_overlap,
            r.bar_row,
            r.bar_gap,
            r.group_gap,
            r.row_gap,
            r.detail,
        ]
    };
    for (c, r) in field(COMPACT).into_iter().zip(field(ROOMY)) {
        assert!(c < r, "{c} < {r}");
    }
    // Halfway: the bar rows, the label lines and the cards all shrank.
    let (w, h, _) = FIT_AREAS[1];
    let roomy = cards_layout(egui::vec2(w, 900.0), 5, &[2, 4, 6], Some(Legend::Beside), 1);
    let fit = cards_layout(egui::vec2(w, h), 5, &[2, 4, 6], Some(Legend::Beside), 1);
    let (a, b) = (roomy.bars.expect("bars"), fit.bars.expect("bars"));
    assert!(b.rows[0].height() < a.rows[0].height());
    assert!(b.rows[0].height() > COMPACT.bar_row);
    assert!(b.label.height() < a.label.height());
    assert!(fit.headline[0].height() < roomy.headline[0].height());
    assert!(fit.groups[0].1[0].height() < roomy.groups[0].1[0].height());
    // It fills the height: no spare room above the header.
    assert_eq!(fit.header.top(), 0.0);
    assert!((fit.height - h).abs() < 0.5, "{}", fit.height);
}

#[test]
fn cards_resize_smoothly_with_the_height() {
    // A pixel of height never moves anything by much more than a pixel,
    // and cards never get shorter as the view gets taller.
    for width in [1212.0, 772.0, 640.0, 440.0] {
        let mut last: Option<CardsLayout> = None;
        for h in 200..=1000 {
            let layout = cards_layout(
                egui::vec2(width, h as f32),
                5,
                &[2, 4, 6],
                Some(Legend::Beside),
                1,
            );
            if let Some(prev) = &last {
                let moved = |a: egui::Rect, b: egui::Rect| {
                    (a.top() - b.top())
                        .abs()
                        .max((a.bottom() - b.bottom()).abs())
                };
                for (a, b) in all_cards(prev).into_iter().zip(all_cards(&layout)) {
                    assert!(moved(a, b) <= 2.0, "{width}x{h}: {a:?} -> {b:?}");
                    assert!(b.height() >= a.height() - 1e-3, "{width}x{h}");
                }
            }
            last = Some(layout);
        }
    }
}

#[test]
fn scroll_engages_only_below_the_compact_height() {
    for (w, _, _) in FIT_AREAS {
        for legend in [Some(Legend::Beside), Some(Legend::Below)] {
            // The compact height: what the cards need at their smallest.
            let least = cards_layout(egui::vec2(w, 1.0), 5, &[2, 4, 6], legend, 1).height;
            let at = |h: f32| cards_layout(egui::vec2(w, h), 5, &[2, 4, 6], legend, 1);
            assert!(!at(least).scrolls, "{w}: {least}");
            assert!(at(least).height <= least + 0.01);
            let short = at(least - 2.0);
            assert!(short.scrolls, "{w}: {least}");
            // Scrolling, every size is compact.
            assert_eq!(short.headline[0].height(), COMPACT.headline);
            assert_eq!(short.groups[0].1[0].height(), COMPACT.detail);
            assert_eq!(short.bars.expect("bars").rows[0].height(), COMPACT.bar_row);
        }
    }
    // 900×700 is below it, the listed windows are not.
    let (w, h, _) = FIT_AREAS[5];
    assert!(cards_layout(egui::vec2(w, h), 5, &[2, 4, 6], Some(Legend::Beside), 1).scrolls);
}

#[test]
fn a_short_window_scrolls_with_a_bar_and_room_under_the_last_row() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = cards_app();
    let window = egui::vec2(900.0, 700.0);
    for _ in 0..3 {
        shots_frame(&ctx, &mut app, window);
    }
    let out = shots_frame(&ctx, &mut app, window);
    let right = window.x - f32::from(TAB_MARGIN.right);
    assert_eq!(scroll_bars(&out, right), 2, "the bar's track and handle");
    // Scrolled to the end, the last row stands clear of the panel.
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, window)),
        events: vec![
            egui::Event::PointerMoved(egui::pos2(500.0, 300.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -2000.0),
                modifiers: egui::Modifiers::NONE,
            },
        ],
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(TAB_MARGIN))
            .show(ctx, |ui| app.render_shots_panel(ui));
    });
    let mut out = shots_frame(&ctx, &mut app, window);
    for _ in 0..30 {
        out = shots_frame(&ctx, &mut app, window);
    }
    let (row, _) = chart_slots(ShotsView::Cards, window)
        .bottom
        .expect("a bottom row");
    let panel_top = window.y - f32::from(TAB_MARGIN.bottom) - row;
    let lowest = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Rect(r)
                if r.stroke.color == theme::BORDER
                    && r.rect.height() >= 40.0
                    && r.rect.left() > LIST_MIN_WIDTH =>
            {
                Some(r.rect.bottom())
            }
            _ => None,
        })
        .fold(0.0, f32::max);
    assert!(lowest > panel_top - 200.0, "not scrolled: {lowest}");
    assert!(
        lowest + CHART_GAP <= panel_top + 0.5,
        "{lowest} reaches the panel at {panel_top}"
    );
}

#[test]
fn cards_fit_above_flight_at_common_window_sizes() {
    let labels = [
        "BALL SPEED",
        "CARRY",
        "TOTAL",
        "LAUNCH",
        "BACKSPIN",
        "DIRECTION",
        "SIDESPIN",
        "APEX",
        "OFFLINE",
        "DESCENT",
        "ROLL",
        "CLUB SPEED",
        "SMASH",
        "PATH",
        "AOA",
        "FACE",
        "LOFT",
    ];
    // Each size freshly opened, then the same sizes in turn in one
    // window, so the shot list keeps the width it took at 1920.
    let shared = egui::Context::default();
    theme::install(&shared);
    let runs = FIT_WINDOWS
        .iter()
        .map(|&s| (s, None))
        .chain(FIT_WINDOWS.iter().map(|&s| (s, Some(shared.clone()))));
    for ((w, h), ctx) in runs {
        let ctx = ctx.unwrap_or_else(|| {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            ctx
        });
        let window = egui::vec2(w, h);
        let mut app = cards_app();
        for _ in 0..3 {
            shots_frame(&ctx, &mut app, window);
        }
        let out = shots_frame(&ctx, &mut app, window);
        let slots = chart_slots(ShotsView::Cards, window);
        let (row, _) = slots.bottom.expect("a bottom row");
        // The top of the panel holding Flight.
        let panel_top = h - f32::from(TAB_MARGIN.bottom) - row;
        let right = w - f32::from(TAB_MARGIN.right) - slots.right.unwrap_or(0.0);
        // Every card: its outline, unclipped, above the panel.
        let cards: Vec<egui::Rect> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r)
                    if r.stroke.color == theme::BORDER
                        && r.rect.height() >= 40.0
                        && r.rect.left() > LIST_MIN_WIDTH
                        && r.rect.right() <= right =>
                {
                    assert!(
                        c.clip_rect.contains_rect(r.rect),
                        "{w}x{h}: {:?} clipped to {:?}",
                        r.rect,
                        c.clip_rect
                    );
                    Some(r.rect)
                }
                _ => None,
            })
            .collect();
        // The tiles, and the stats card beside the bars.
        assert_eq!(cards.len(), labels.len() + 1, "{w}x{h}: {cards:?}");
        for r in &cards {
            assert!(
                r.bottom() + CHART_GAP <= panel_top + 0.5,
                "{w}x{h}: {r:?} reaches the panel at {panel_top}"
            );
        }
        assert_eq!(scroll_bars(&out, right), 0, "{w}x{h}: a scroll bar");
        let texts = painted_texts(&out.shapes);
        for label in labels {
            assert!(
                texts.iter().any(|(t, _)| t == label),
                "{w}x{h}: {label} not painted"
            );
        }
    }
}

#[test]
fn the_bottom_row_budgets_the_dispersion_header() {
    // The floors hold a plot of the least height under a one-row header.
    assert_eq!(*SHARED_ROW_HEIGHT.start(), 210.0);
    assert_eq!(*TABLE_DISPERSION_HEIGHT.start(), 266.0);
    // Cards: Dispersion is widened so its header keeps one row at
    // 1024 px, with the list at its default or widest width; Flight
    // keeps the rest.
    for width in [748.0, 785.0] {
        let (flight, slot) = row_split(width);
        assert_eq!(slot, dispersion::ONE_ROW_MIN_WIDTH, "{width}");
        assert_eq!(flight + CHART_GAP + slot, width);
        assert!(flight >= 400.0, "{width}: {flight}");
    }
    // Wide rows keep the 38 % share; narrow ones give at most half.
    assert_eq!(row_split(1014.0).1, 380.0);
    assert_eq!(row_split(414.0), (200.0, 200.0));
    // Every narrow window, any row width: the plot keeps its minimum
    // under however many header rows the slot draws.
    for w in (600..1200).step_by(25) {
        for h in (400..1200).step_by(20) {
            let window = egui::vec2(w as f32, h as f32);
            for view in [ShotsView::Cards, ShotsView::Table] {
                let (height, row) = chart_slots(view, window).bottom.expect("a bottom row");
                for width in (300..1200).step_by(7) {
                    let width = width as f32;
                    let (slot, least) = match row {
                        BottomRow::FlightAndDispersion => (row_split(width).1, SHARED_PLOT_MIN),
                        _ => (width, TABLE_PLOT_MIN),
                    };
                    let row_height = bottom_height(height, row, width);
                    assert!(row_height >= height);
                    let plot = row_height
                        - CHART_GAP
                        - dispersion::header_height(slot)
                        - dispersion::PLOT_PAD;
                    let at = format!("{view:?} {w}x{h} row {width}");
                    assert!(plot >= least, "{at}: {plot}");
                    // Only the extra header row is added.
                    if row_height > height {
                        assert_eq!(plot, least, "{at}");
                    }
                }
            }
        }
    }
    // The fit windows keep their Cards bottom row.
    for &(w, h) in &FIT_WINDOWS {
        let window = egui::vec2(w, h);
        let (height, row) = chart_slots(ShotsView::Cards, window).bottom.unwrap();
        if row == BottomRow::FlightAndDispersion {
            for width in [748.0, 785.0] {
                assert_eq!(bottom_height(height, row, width), height);
            }
        }
    }
}

#[test]
fn the_dispersion_plot_keeps_its_least_height_in_narrow_windows() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let one_device = |app: &mut FlighthookApp| {
        for shot in &mut app.shots {
            shot.actor = "mevo.0".into();
        }
    };
    // (view, window, two devices)
    let cases = [
        (ShotsView::Cards, (1024.0, 768.0), true),
        (ShotsView::Cards, (900.0, 700.0), true),
        (ShotsView::Cards, (760.0, 600.0), true),
        (ShotsView::Table, (1024.0, 768.0), false),
        (ShotsView::Table, (1024.0, 768.0), true),
        (ShotsView::Table, (900.0, 600.0), false),
        (ShotsView::Table, (900.0, 600.0), true),
        (ShotsView::Table, (640.0, 480.0), true),
    ];
    for (view, (w, h), two) in cases {
        let at = format!("{view:?} {w}x{h} two devices {two}");
        let ctx = ctx.clone();
        let mut app = cards_app();
        app.shots_view = view;
        if !two {
            one_device(&mut app);
        }
        let window = egui::vec2(w, h);
        for _ in 0..3 {
            shots_frame(&ctx, &mut app, window);
        }
        let out = shots_frame(&ctx, &mut app, window);
        let (plot, area) = app.dispersion_plot().expect("a Dispersion plot");
        let least = match view {
            ShotsView::Cards => SHARED_PLOT_MIN,
            ShotsView::Table => TABLE_PLOT_MIN,
        };
        assert!(plot.height() >= least - 0.5, "{at}: {plot:?}");
        // The header sits above the plot's allocation, every switch in
        // it.
        let top = h - f32::from(TAB_MARGIN.bottom) - area;
        let texts = painted_text_rects(&out.shapes);
        let title = texts
            .iter()
            .find(|(t, _, _)| t == "DISPERSION")
            .map(|(_, _, r)| *r)
            .expect("the title");
        let header: Vec<_> = texts
            .iter()
            .filter(|(_, _, r)| r.left() >= title.left() - 0.5 && r.top() >= title.top() - 20.0)
            .filter(|(t, _, r)| {
                [
                    "DISPERSION",
                    "2\u{3c3}",
                    "95 %",
                    "Total",
                    "Carry",
                    "DEVICE",
                    "All",
                ]
                .contains(&t.as_str())
                    && r.top() < top
            })
            .collect();
        for (t, _, r) in &header {
            assert!(r.bottom() <= top + 0.5, "{at}: {t} at {r:?} under {top}");
        }
        let shown = |label: &str| header.iter().any(|(t, _, _)| t == label);
        for label in ["DISPERSION", "2\u{3c3}", "95 %", "Total", "Carry"] {
            assert!(shown(label), "{at}: {label}");
        }
        // The device filter is the navigation's, never the header's.
        assert!(!shown("DEVICE") && !shown("All"), "{at}");
    }
}

#[test]
fn pick_prefers_derived_when_both_are_present() {
    // Measured only, derived only: the one there is.
    assert_eq!(pick(Some(1.0), None), Some((1.0, false)));
    assert_eq!(pick(None, Some(2.0)), Some((2.0, true)));
    assert_eq!(pick::<f64>(None, None), None);
    // Both: derived wins, shown as simulated.
    assert_eq!(pick(Some(1.0), Some(2.0)), Some((2.0, true)));
    // A simulated 0 (a ground ball) over a device's carry.
    assert_eq!(pick(Some(12.0), Some(0.0)), Some((0.0, true)));
}

#[test]
fn metric_shows_the_picked_value_and_its_source() {
    let m = Metric::new("Carry", "yd", pick(Some(200.04), None), 1, false);
    assert_eq!(
        (m.text.as_str(), m.source == Source::Measured),
        ("200.0", true)
    );
    let m = Metric::new("Carry", "yd", pick(Some(200.04), Some(190.0)), 1, false);
    assert_eq!(
        (m.text.as_str(), m.source == Source::Simulated),
        ("190.0", true)
    );
    let m = Metric::new("Offline", "yd", pick(None, Some(3.0)), 1, true);
    assert_eq!(
        (m.text.as_str(), m.source == Source::Simulated),
        ("+3.0", true)
    );
    let m = Metric::new("Descent", "\u{b0}", None, 1, false);
    assert!(m.source == Source::Missing);
}

/// A row whose device measured carry 171 and apex 30, simulated as
/// carry 165, total 180 and no apex, with `replaces` as the shot's flag.
fn forced(replaces: bool) -> ShotRow {
    let mut shot = flight(
        1,
        (None, Some(171.0)),
        (Some(180.0), Some(165.0), Some(2.0)),
    );
    shot.flight_mut().max_height = Some(Distance::Feet(30.0));
    shot.derived.as_mut().unwrap().replaces_measured = replaces;
    shot
}

/// The text of the Cards metric `label` and whether it is simulated.
fn card_text(shot: &ShotRow, label: &str) -> (String, bool) {
    let (headline, groups) = card_metrics(shot, true, Handedness::Right);
    let m = headline
        .into_iter()
        .chain(groups.into_iter().flat_map(|(_, g)| g))
        .find(|m| m.label == label)
        .unwrap();
    (m.text, m.source == Source::Simulated)
}

#[test]
fn a_field_in_both_shows_the_simulated_value_everywhere() {
    // Force or not, a field the server set in both shows the simulated
    // value; a field measured only shows the measured one.
    let shots = [forced(false), forced(true)];
    for shot in &shots {
        assert_eq!(card_text(shot, "Carry"), ("165.0".into(), true));
        assert_eq!(card_text(shot, "Total"), ("180.0".into(), true));
        assert_eq!(card_text(shot, "Apex"), ("30.0".into(), false));
        let entry = ListEntry::new(shot, None, true);
        assert_eq!(entry.carry.text, "165.0");
        assert!(entry.carry.source == Source::Simulated);
        assert_eq!(BarKind::Carry.value(shot, true), Some(165.0));
        assert_eq!(BarKind::Total.value(shot, true), Some(180.0));
    }

    // The bars' stats run over the picked values.
    let carry = bar_values(&shots, BarKind::Carry, true);
    assert_eq!(carry.values, vec![165.0, 165.0]);
}

#[test]
fn a_ground_ball_shows_zero_carry_over_the_devices_carry() {
    // The device sent carry 12 yd, apex 1 ft and 0.4 s on a bladed
    // shot; the server rolled it: carry, apex and flight time 0.
    let mut shot = flight(1, (None, Some(12.0)), (Some(9.0), Some(0.0), Some(0.0)));
    let flight = shot.flight_mut();
    flight.max_height = Some(Distance::Feet(1.0));
    flight.flight_time = Some(0.4);
    let d = shot.derived.as_mut().unwrap();
    d.max_height = Some(Distance::Feet(0.0));
    d.flight_time = Some(0.0);
    assert_eq!(card_text(&shot, "Carry"), ("0.0".into(), true));
    assert_eq!(card_text(&shot, "Apex"), ("0.0".into(), true));
    assert_eq!(card_text(&shot, "Total"), ("9.0".into(), true));
    assert_eq!(ListEntry::new(&shot, None, true).carry.text, "0.0");
    assert_eq!(BarKind::Carry.value(&shot, true), Some(0.0));
    let r = crate::panels::flight::readings(&shot, UnitSystem::Imperial);
    assert_eq!(
        (r.carry, r.apex, r.total),
        (Some(0.0), Some(0.0), Some(9.0))
    );
}

/// A launch-only chip whose spin flighthook estimated, simulated from
/// it: 15.7 mph, 1570 rpm backspin, no sidespin, a simulated carry.
fn estimated_spin_chip(n: u32) -> ShotRow {
    let mut derived = DerivedFlight::default();
    derived.carry_distance = Some(Distance::Yards(3.9));
    derived.spin_estimated = true;
    ShotRow {
        launch: Some(Launch {
            launch_speed: Some(Velocity::MilesPerHour(15.7)),
            launch_elevation: Some(22.8),
            launch_azimuth: Some(-0.9),
            backspin_rpm: Some(1570),
            sidespin_rpm: Some(0),
            ..Default::default()
        }),
        derived: Some(derived),
        ..row(n, true)
    }
}

#[test]
fn estimated_spin_styles_as_simulated_on_the_cards_and_in_the_table() {
    use TableCell::Spin;

    let columns = table_columns(false, true);
    let name = |n: &str| columns.iter().position(|c| c.name == n).expect(n);

    let chip = estimated_spin_chip(1);
    assert_eq!(card_text(&chip, "Backspin"), ("1570".into(), true));
    assert_eq!(card_text(&chip, "Sidespin"), ("0".into(), true));
    let cells = table_cells(&chip, None, None, false);
    assert_eq!(cells[name("Back")], Spin(Some((1570.0, true))));
    assert_eq!(cells[name("Side")], Spin(Some((0.0, true))));

    // The device's spin, simulated shot or not: plain.
    let measured = thousand_rows().remove(4);
    let mut unsimulated = measured.clone();
    unsimulated.derived = None;
    for shot in [&measured, &unsimulated] {
        assert_eq!(card_text(shot, "Backspin"), ("6200".into(), false));
        assert_eq!(card_text(shot, "Sidespin"), ("300".into(), false));
        let cells = table_cells(shot, None, None, false);
        assert_eq!(cells[name("Back")], Spin(Some((6200.0, false))));
        assert_eq!(cells[name("Side")], Spin(Some((-300.0, false))));
    }
    // No spin reading: missing, as before.
    let mut none = measured.clone();
    none.launch_mut().backspin_rpm = None;
    assert!(card_metrics(&none, true, Handedness::Right).0[4].source == Source::Missing);
    assert_eq!(
        table_cells(&none, None, None, false)[name("Back")],
        Spin(None)
    );

    // A total spin with no axis and no components: all of it shows as
    // backspin, and no sidespin is made up.
    let mut total_only = measured.clone();
    let ball = total_only.launch_mut();
    (ball.backspin_rpm, ball.sidespin_rpm) = (None, None);
    (ball.total_spin_rpm, ball.spin_axis) = (Some(6897), None);
    assert_eq!(card_text(&total_only, "Backspin"), ("6897".into(), false));
    assert_eq!(card_text(&total_only, "Sidespin").0, "\u{2013}", "missing");
    let cells = table_cells(&total_only, None, None, false);
    assert_eq!(cells[name("Back")], Spin(Some((6897.0, false))));
    assert_eq!(cells[name("Side")], Spin(None));

    // A putt with no launch angle shows none.
    let mut putt = measured.clone();
    putt.launch_mut().launch_elevation = None;
    assert_eq!(card_text(&putt, "Launch").0, "\u{2013}", "missing");
    assert_eq!(
        table_cells(&putt, None, None, false)[name("VLA")],
        TableCell::Text("-".into())
    );
}

/// The painted Cards view tags the estimated spin `SIM`, and the
/// painted Table view sets it in italics.
#[test]
fn estimated_spin_is_painted_as_simulated() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let window = egui::vec2(1600.0, 900.0);
    let paint = |app: &mut FlighthookApp| {
        let mut out = shots_frame(&ctx, app, window);
        for _ in 0..3 {
            out = shots_frame(&ctx, app, window);
        }
        painted_texts(&out.shapes)
    };
    let sim_tags = |texts: &[(String, bool)]| texts.iter().filter(|(t, _)| t == "SIM").count();

    // The latest shot (the one the cards show) becomes the chip, with
    // and without the flag.
    let app = |spin_estimated: bool| {
        let mut app = cards_app();
        let chip = estimated_spin_chip(0);
        let latest = app.shots.last_mut().expect("shots");
        latest.launch = chip.launch;
        latest.flight = chip.flight;
        latest.derived = chip.derived;
        latest.derived.as_mut().expect("derived").spin_estimated = spin_estimated;
        app
    };
    let (mut flagged, mut unflagged) = (app(true), app(false));

    // Backspin and Sidespin add a tag each; Carry is simulated in both.
    let with = paint(&mut flagged);
    let without = paint(&mut unflagged);
    assert!(with.iter().any(|(t, _)| t == "1570"), "{with:?}");
    assert_eq!(sim_tags(&with), sim_tags(&without) + 2);

    flagged.shots_view = ShotsView::Table;
    unflagged.shots_view = ShotsView::Table;
    let with = paint(&mut flagged);
    let without = paint(&mut unflagged);
    assert!(with.iter().any(|(t, i)| t == "1570" && *i), "{with:?}");
    assert!(
        without.iter().any(|(t, i)| t == "1570" && !*i),
        "{without:?}"
    );
    assert!(!without.iter().any(|(t, i)| t == "1570" && *i));
    // The measured rows' spin stays upright.
    assert!(with.iter().any(|(t, i)| t == "6200" && !*i));
    assert!(!with.iter().any(|(t, i)| t == "6200" && *i));
}

#[test]
fn simulated_legend_only_in_table() {
    assert!(ShotsView::Table.shows_simulated_legend());
    assert!(!ShotsView::Cards.shows_simulated_legend());
}

#[test]
fn chart_slots_follow_the_window() {
    use egui::vec2;
    let slots = |view, w, h| chart_slots(view, vec2(w, h));
    let cards = ShotsView::Cards;
    let table = ShotsView::Table;
    // Wide Cards: Dispersion on the right (24 % of the width, 260-420),
    // Flight along the bottom (30 % of the height, 180-320).
    assert_eq!(
        slots(cards, 1920.0, 1080.0),
        ChartSlots {
            right: Some(420.0),
            bottom: Some((320.0, BottomRow::Flight)),
        }
    );
    assert_eq!(
        slots(cards, 1600.0, 900.0),
        ChartSlots {
            right: Some(384.0),
            bottom: Some((270.0, BottomRow::Flight)),
        }
    );
    assert_eq!(
        slots(cards, 1280.0, 800.0),
        ChartSlots {
            right: Some(307.0),
            bottom: Some((240.0, BottomRow::Flight)),
        }
    );
    assert_eq!(
        slots(cards, 1200.0, 500.0).bottom,
        Some((180.0, BottomRow::Flight))
    );
    assert_eq!(slots(cards, 1200.0, 500.0).right, Some(288.0));
    // Narrow Cards: both charts share the bottom row (27 % of the
    // height, 210-320), which stops shrinking below about 780 px tall;
    // Flight keeps at least 180 px under the row's top gap.
    assert_eq!(
        slots(cards, 1199.0, 900.0),
        ChartSlots {
            right: None,
            bottom: Some((243.0, BottomRow::FlightAndDispersion)),
        }
    );
    assert_eq!(
        slots(cards, 1024.0, 768.0).bottom,
        Some((210.0, BottomRow::FlightAndDispersion))
    );
    assert_eq!(
        slots(cards, 900.0, 700.0).bottom,
        Some((210.0, BottomRow::FlightAndDispersion))
    );
    assert!(*SHARED_ROW_HEIGHT.start() - CHART_GAP >= 180.0);
    // Table: Dispersion on the right when wide, below when narrow.
    assert_eq!(
        slots(table, 1600.0, 900.0),
        ChartSlots {
            right: Some(384.0),
            bottom: None,
        }
    );
    assert_eq!(
        slots(table, 1024.0, 768.0),
        ChartSlots {
            right: None,
            bottom: Some((269.0, BottomRow::Dispersion)),
        }
    );
    assert_eq!(
        slots(table, 900.0, 600.0).bottom,
        Some((266.0, BottomRow::Dispersion))
    );
    // Every chart is always somewhere.
    for &view in ShotsView::ALL {
        for (w, h) in [
            (1920.0, 1080.0),
            (1440.0, 900.0),
            (1024.0, 768.0),
            (900.0, 700.0),
        ] {
            let s = slots(view, w, h);
            let charts = usize::from(s.right.is_some())
                + s.bottom.map_or(0, |(_, row)| match row {
                    BottomRow::FlightAndDispersion => 2,
                    BottomRow::Flight | BottomRow::Dispersion => 1,
                });
            let expected = if view == cards { 2 } else { 1 };
            assert_eq!(charts, expected, "{view:?} {w}x{h}");
        }
    }
}

#[test]
fn apex_card_is_labeled_feet_in_imperial_and_meters_in_metric() {
    let mut shot = row(1, true);
    shot.flight_mut().max_height = Some(Distance::Meters(31.2));

    let imperial = shot.to_unit_system(UnitSystem::Imperial);
    let (_, groups) = card_metrics(&imperial, true, Handedness::Right);
    let apex = groups[1].1.iter().find(|m| m.label == "Apex").unwrap();
    assert_eq!(apex.unit, "ft");

    let metric = shot.to_unit_system(UnitSystem::Metric);
    let (_, groups) = card_metrics(&metric, false, Handedness::Right);
    let apex = groups[1].1.iter().find(|m| m.label == "Apex").unwrap();
    assert_eq!(apex.unit, "m");
}

/// A device's measured carry offline, descent angle and total offline
/// fill the existing Offline and Descent cards, Table columns and the
/// Lateral bar, as measured values; a simulated one still wins when both
/// are there.
#[test]
fn measured_offline_and_descent_feed_cards_table_and_bars() {
    let mut shot = row(1, true);
    let flight = shot.flight_mut();
    flight.carry_offline = Some(Distance::Yards(-4.2));
    flight.total_offline = Some(Distance::Yards(-5.0));
    flight.descent_angle = Some(43.1);

    let (_, groups) = card_metrics(&shot, true, Handedness::Right);
    let card = |label: &str| {
        groups[1]
            .1
            .iter()
            .find(|m| m.label == label)
            .map(|m| (m.text.clone(), m.source == Source::Measured))
            .unwrap()
    };
    // Offline shows its magnitude and side.
    assert_eq!(card("Offline"), (fmt_num(4.2, 1, false), true));
    let offline = groups[1].1.iter().find(|m| m.label == "Offline").unwrap();
    assert_eq!(offline.word, Some("L"));
    assert_eq!(card("Descent"), (fmt_num(43.1, 1, false), true));

    let columns = table_columns(false, true);
    let name = |n: &str| columns.iter().position(|c| c.name == n).expect(n);
    let cells = table_cells(&shot, None, None, false);
    assert_eq!(
        cells[name("Offline")],
        TableCell::Picked(Some((-4.2, false)), true)
    );
    assert_eq!(
        cells[name("Desc")],
        TableCell::Picked(Some((43.1, false)), false)
    );
    assert_eq!(
        cells[name("T.Offline")],
        TableCell::Picked(Some((-5.0, false)), true)
    );

    assert_eq!(BarKind::Lateral.value(&shot, true), Some(-4.2));

    let mut d = DerivedFlight::default();
    d.carry_offline = Some(Distance::Yards(-3.0));
    shot.derived = Some(d);
    assert_eq!(BarKind::Lateral.value(&shot, true), Some(-3.0));
}

#[test]
fn table_header_apex_unit_is_feet_in_imperial_not_yards() {
    assert_eq!(table_header_units(true), ("mph", "yd", "ft"));
    assert_eq!(table_header_units(false), ("m/s", "m", "m"));
}

#[test]
fn progress_is_linear_over_the_pulse() {
    let mut focus = ShotFocus::default();
    assert_eq!(focus.progress(0.0), None, "no shot, no pulse");
    let shots = vec![row(1, true)];
    focus.observe(&shots, ShotFilter::ALL, 10.0);
    assert_eq!(focus.progress(10.0), Some(0.0));
    let quarter = focus.progress(10.0 + PULSE_SECS / 4.0).unwrap();
    assert!((quarter - 0.25).abs() < 1e-6);
    let half = focus.progress(10.0 + PULSE_SECS / 2.0).unwrap();
    assert!((half - 0.5).abs() < 1e-6);
    // The eased strength stays tied to it.
    assert!((focus.strength(10.0 + PULSE_SECS / 2.0) - 0.125).abs() < 1e-6);
    assert_eq!(focus.progress(10.0 + PULSE_SECS), None, "over");
    // A bulk load starts no pulse.
    focus.sync(&shots, ShotFilter::ALL);
    assert_eq!(focus.progress(10.1), None);
}

/// A row from `actor` in set `group` with a measured total and carry and
/// a simulated offline, in yards.
fn device_shot(n: u32, actor: &str, group: &str, total: f64, offline: f64) -> ShotRow {
    ShotRow {
        actor: actor.into(),
        group: Some(group.into()),
        ..flight(
            n,
            (Some(total), Some(total - 10.0)),
            (None, None, Some(offline)),
        )
    }
}

#[test]
fn bars_split_into_device_sub_bands() {
    let rows = vec![
        device_shot(1, "square.0", "a", 240.0, -3.0),
        device_shot(2, "mevo.0", "a", 250.0, 2.0),
        device_shot(3, "square.0", "a", 244.0, -1.0),
        device_shot(4, "mevo.0", "a", 252.0, 4.0),
    ];
    let all: Vec<usize> = (0..4).collect();
    let ids = chart::device_ids(&rows);
    assert_eq!(ids, vec!["mevo.0", "square.0"]);
    let [total, carry, lateral] = bars_for(&rows, &all, Some(3), &ids, true);
    // The whole bar is unchanged by the split.
    assert_eq!(total.stats.map(|s| (s.min, s.max)), Some((240.0, 252.0)));
    assert_eq!(total.this, Some(252.0));
    // One band per device, in sorted id order and its device color.
    let bands = |b: &Bar| {
        b.devices
            .iter()
            .map(|d| (d.color, d.stats.map(|s| (s.min, s.max))))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        bands(&total),
        vec![
            (theme::DEVICE_COLORS[0], Some((250.0, 252.0))),
            (theme::DEVICE_COLORS[1], Some((240.0, 244.0))),
        ]
    );
    assert_eq!(carry.devices.len(), 2);
    assert_eq!(
        bands(&lateral),
        vec![
            (theme::DEVICE_COLORS[0], Some((2.0, 4.0))),
            (theme::DEVICE_COLORS[1], Some((-3.0, -1.0))),
        ]
    );
    // A device with no value in a bar keeps its slot, empty.
    let mut gap = rows.clone();
    gap[1].derived = None;
    gap[3].derived = None;
    let [_, _, lateral] = bars_for(&gap, &all, None, &ids, true);
    assert_eq!(lateral.devices.len(), 2);
    assert!(lateral.devices[0].stats.is_none());
    // One device, or a device filter (no ids passed): a single band.
    let [total, ..] = bars_for(&rows, &all, None, &ids[..1], true);
    assert!(total.devices.is_empty());
    let [total, ..] = bars_for(&rows, &[1, 3], None, &[], true);
    assert!(total.devices.is_empty());
    assert_eq!(total.stats.map(|s| (s.min, s.max)), Some((250.0, 252.0)));
}

#[test]
fn sub_bands_share_the_track() {
    let track = egui::Rangef::new(100.0, 100.0 + TRACK_H);
    for n in 1..=4 {
        let bands = sub_bands(track, n);
        assert_eq!(bands.len(), n);
        assert_eq!(bands[0].min, track.min);
        for b in &bands {
            assert!((b.span() - TRACK_H / n as f32).abs() < 1e-5, "{n}: {b:?}");
        }
        // Stacked, touching, filling the track.
        for w in bands.windows(2) {
            assert!((w[0].max - w[1].min).abs() < 1e-5);
        }
        assert!((bands[n - 1].max - track.max).abs() < 1e-4);
    }
    // Many devices: the 2 px floor.
    let bands = sub_bands(track, 9);
    assert!(bands.iter().all(|b| b.span() == SUB_BAND_MIN_H));
    assert!(sub_bands(track, 0).is_empty());
}

#[test]
fn device_filter_shows_only_with_two_devices() {
    let named = |name: &str| ActorStatusResponse::new(name, crate::types::ActorStatus::Connected);
    let actors: HashMap<String, ActorStatusResponse> = [
        ("mevo.0".to_owned(), named("Mevo+")),
        ("openconnect_server".to_owned(), named("")),
    ]
    .into_iter()
    .collect();
    assert_eq!(device_options(&[], &actors), None);
    assert_eq!(device_options(&["mevo.0".into()], &actors), None);
    // An unnamed or unknown device is labeled by its id.
    let ids: Vec<String> = ["mevo.0", "openconnect_server", "square.0"]
        .map(String::from)
        .to_vec();
    assert_eq!(
        device_options(&ids, &actors),
        Some(
            ["All", "Mevo+", "openconnect_server", "square.0"]
                .map(String::from)
                .to_vec()
        )
    );
    // The ids are exact: an empty-actor row adds no device and merges
    // none (unlike the name-hiding rule's `same_actor`).
    let rows = vec![
        ShotRow {
            actor: String::new(),
            ..row(1, true)
        },
        row(2, true),
    ];
    assert_eq!(chart::device_ids(&rows), vec!["mock_monitor.0"]);
    assert_eq!(device_options(&chart::device_ids(&rows), &actors), None);
}

/// 8 seven-iron shots, and the same 8 with an Auto-chip hit among them
/// (the server counts it in the set: 9 shots).
fn irons_with_and_without_a_chip() -> (Vec<ShotRow>, ShotGroups, Vec<ShotRow>, ShotGroups) {
    let i7 = |n: u32| ShotRow {
        club_selected: Some(Club::Iron7),
        ..device_shot(
            n,
            "mevo.0",
            "a",
            150.0 + 4.0 * f64::from(n),
            f64::from(n % 3) * 3.0 - 3.0,
        )
    };
    let irons: Vec<ShotRow> = (1..=8).map(i7).collect();
    let mut with_chip = irons.clone();
    with_chip.insert(
        4,
        ShotRow {
            club_selected: Some(Club::Iron7),
            ..device_shot(50, flighthook::AUTO_CHIP_ACTOR, "a", 3.0, 0.0)
        },
    );
    let g = |shots| {
        groups(
            ShotGrouping::BySet,
            vec![group("a", 1, Club::Iron7, true, shots)],
        )
    };
    (irons, g(8), with_chip, g(9))
}

/// A set of 8 iron shots plus 1 Auto-chip has the same range bars,
/// title and device filter as the 8 alone, in either view, whether an
/// iron or the chip is shown; a shown chip gets no "this shot" mark.
#[test]
fn an_auto_chip_leaves_the_set_stats_unchanged() {
    let (irons, g8, with_chip, g9) = irons_with_and_without_a_chip();
    let actors = HashMap::new();
    let summary = |rows: &[ShotRow], g: &ShotGroups, view, shown: usize| {
        let set = bar_group(g, Some(&rows[shown]), Some(Club::Iron7));
        let m = chart::members_for(rows, g, view, set, Some(shown), ShotFilter::ALL);
        let devices = chart::device_ids(rows);
        let bars = bars_for(rows, &m, Some(shown), &devices, true);
        let stats: Vec<_> = bars.iter().map(|b| (b.stats, b.values.rows)).collect();
        let texts: Vec<_> = bars.iter().map(StatsRow::of).collect();
        let this: Vec<_> = bars.iter().map(|b| b.this).collect();
        let title = bars_title(view, set, g.session(), rows, &m, false).map(|t| t.0);
        (stats, texts, title, device_options(&devices, &actors), this)
    };
    for view in [ShotGrouping::BySet, ShotGrouping::ByClub] {
        let alone = summary(&irons, &g8, view, 7);
        assert_eq!(alone.0[0].0.map(|s| s.count), Some(8), "{view:?}");
        assert_eq!(alone.3, None, "one device: no filter");
        assert_eq!(summary(&with_chip, &g9, view, 8), alone, "{view:?}");
        // The chip shown: same set, same stats, no mark.
        let chip = summary(&with_chip, &g9, view, 4);
        assert_eq!(chip.0, alone.0, "{view:?}");
        assert_eq!((&chip.1, &chip.2, &chip.3), (&alone.1, &alone.2, &alone.3));
        assert_eq!(chip.4, vec![None; 3], "{view:?}");
    }
    assert_eq!(
        summary(&with_chip, &g9, ShotGrouping::BySet, 8)
            .2
            .as_deref(),
        Some("Set 1 \u{b7} 7I \u{b7} 8 shots")
    );
    assert_eq!(
        summary(&with_chip, &g9, ShotGrouping::ByClub, 8)
            .2
            .as_deref(),
        Some("7 Iron \u{b7} 8 shots")
    );
    // With a second device, the filter lists it but not Auto-chip.
    let mut two = with_chip.clone();
    two[0].actor = "square.0".into();
    assert_eq!(
        device_options(&chart::device_ids(&two), &actors),
        Some(vec!["All".into(), "mevo.0".into(), "square.0".into()])
    );
}

/// Painted, the Cards view of that set shows the same range bars, set
/// header and shot count with the chip as without it, and no device
/// filter; the chip is still in the shot list.
#[test]
fn an_auto_chip_leaves_the_painted_set_stats_unchanged() {
    let (irons, g8, with_chip, g9) = irons_with_and_without_a_chip();
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let window = egui::vec2(1600.0, 900.0);
    let paint = |shots: Vec<ShotRow>, g: ShotGroups| {
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.shots = shots;
        app.groups = g;
        app.shots_rev = 1;
        app.shots_view = ShotsView::Cards;
        let mut out = shots_frame(&ctx, &mut app, window);
        for _ in 0..3 {
            out = shots_frame(&ctx, &mut app, window);
        }
        painted_texts(&out.shapes)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<String>>()
    };
    // The stats card's texts (from its title to the next section's) and
    // every shot count.
    let stat_texts = |texts: &[String]| -> Vec<String> {
        let card = texts.iter().position(|t| t == "STATS");
        let card = card.map_or(&[][..], |at| {
            let end = texts[at..]
                .iter()
                .position(|t| t == "BALL")
                .map_or(texts.len(), |n| at + n);
            &texts[at..end]
        });
        card.iter()
            .chain(texts.iter().filter(|t| t.to_uppercase().contains("SHOT")))
            .cloned()
            .collect()
    };
    let alone = paint(irons, g8);
    let chip = paint(with_chip, g9);
    let stats = stat_texts(&alone);
    assert!(stats.iter().any(|t| t == "AVG"), "{alone:?}");
    assert!(
        stats.iter().any(|t| t.to_uppercase().contains("8 SHOTS")),
        "{alone:?}"
    );
    assert_eq!(stat_texts(&chip), stats);
    assert!(!chip.iter().any(|t| t == "All"), "no device filter");
    assert!(
        chip.iter().any(|t| t == flighthook::AUTO_CHIP_NAME),
        "the chip is listed: {chip:?}"
    );
}

#[test]
fn the_view_sets_the_bars_and_titles_them() {
    let i7 = |n, g: &str, total| ShotRow {
        club_selected: Some(Club::Iron7),
        ..device_shot(n, "mevo.0", g, total, 0.0)
    };
    // 7I (a), DR (b), 7I (c), then New session and 7I (d).
    let rows = vec![
        i7(1, "a", 160.0),
        i7(2, "a", 164.0),
        ShotRow {
            club_selected: Some(Club::Driver),
            ..device_shot(3, "mevo.0", "b", 250.0, 0.0)
        },
        i7(4, "c", 170.0),
        ShotRow {
            actor: "square.0".into(),
            ..i7(5, "c", 174.0)
        },
        i7(6, "d", 180.0),
    ];
    let first = vec![
        group("a", 1, Club::Iron7, false, 2),
        group("b", 2, Club::Driver, false, 1),
        group("c", 3, Club::Iron7, true, 2),
    ];
    let bars = |g: &ShotGroups, view, shown: usize, device| {
        let set = bar_group(g, Some(&rows[shown]), Some(Club::Iron7));
        let m = chart::members_for(&rows, g, view, set, Some(shown), device);
        let [total, ..] = bars_for(&rows, &m, Some(shown), &[], true);
        let title = bars_title(view, set, g.session(), &rows, &m, device.narrows());
        (
            total.stats.map(|s| (s.min, s.max)),
            total.this,
            title.map(|t| t.0),
        )
    };
    let g = groups(ShotGrouping::BySet, first.clone());
    assert_eq!(
        bars(&g, ShotGrouping::BySet, 4, ShotFilter::ALL),
        (
            Some((170.0, 174.0)),
            Some(174.0),
            Some("Set 3 \u{b7} 7I \u{b7} 2 shots".to_owned())
        )
    );
    // By club: both 7-iron sets of the session; New set does not reset it.
    assert_eq!(
        bars(&g, ShotGrouping::ByClub, 4, ShotFilter::ALL),
        (
            Some((160.0, 174.0)),
            Some(174.0),
            Some("7 Iron \u{b7} 2 sets \u{b7} 4 shots".to_owned())
        )
    );
    // The device filter in each view: the members narrow, the title
    // counts what is shown.
    assert_eq!(
        bars(
            &g,
            ShotGrouping::BySet,
            4,
            ShotFilter::device(Some("square.0"))
        ),
        (
            Some((174.0, 174.0)),
            Some(174.0),
            Some("Set 3 \u{b7} 7I \u{b7} 1 shot".to_owned())
        )
    );
    assert_eq!(
        bars(
            &g,
            ShotGrouping::ByClub,
            4,
            ShotFilter::device(Some("mevo.0"))
        ),
        (
            Some((160.0, 170.0)),
            None,
            Some("7 Iron \u{b7} 2 sets \u{b7} 3 shots".to_owned())
        )
    );

    // After New session: By club only has the new session's 7-iron.
    let mut later = first;
    later[2].open = false;
    later.push(sess(1, group("d", 4, Club::Iron7, true, 1)));
    let g = groups(ShotGrouping::ByClub, later);
    assert_eq!(
        bars(&g, ShotGrouping::ByClub, 5, ShotFilter::ALL),
        (
            Some((180.0, 180.0)),
            Some(180.0),
            Some("7 Iron \u{b7} 1 shot".to_owned())
        )
    );
    // An earlier session's shot shows its own session's club, muted.
    let set = bar_group(&g, Some(&rows[0]), None);
    let m = chart::members_for(
        &rows,
        &g,
        ShotGrouping::ByClub,
        set,
        Some(0),
        ShotFilter::ALL,
    );
    assert_eq!(m, vec![0, 1, 3, 4]);
    assert_eq!(
        bars_title(ShotGrouping::ByClub, set, g.session(), &rows, &m, false),
        Some((
            "7 Iron \u{b7} Earlier \u{b7} 2 sets \u{b7} 4 shots".to_owned(),
            theme::TEXT_MUTED
        ))
    );

    // None: no bars and no device filter; the shown shot alone.
    assert!(!shows_bars(ShotGrouping::None));
    assert!(shows_bars(ShotGrouping::BySet) && shows_bars(ShotGrouping::ByClub));
    let m = chart::members_for(
        &rows,
        &g,
        ShotGrouping::None,
        set,
        Some(2),
        ShotFilter::device(Some("x")),
    );
    assert_eq!(m, vec![2]);
    assert_eq!(
        bars_title(ShotGrouping::None, set, g.session(), &rows, &m, false),
        None
    );
    // No set: the note instead of a title.
    assert_eq!(
        bars_title(ShotGrouping::BySet, None, 0, &rows, &[], false),
        None
    );
}

#[test]
fn clear_history_is_disabled_without_shots_and_confirms_with_them() {
    use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, vec2};

    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    let frame = |app: &mut FlighthookApp, events: Vec<Event>| {
        ctx.data_mut(|d| d.remove_temp::<devices::Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| app.render_shots_panel(ui));
        });
        ctx.data(|d| d.get_temp::<devices::Marks>(egui::Id::NULL))
            .unwrap_or_default()
    };
    // The trash icon at the right end of the navigation's header row.
    let marks = frame(&mut app, Vec::new());
    let at = marks["clear_history"].0.center();
    assert!(
        at.x < 400.0 && at.y < 40.0,
        "in the navigation's header: {at:?}"
    );
    let button = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    let click = |app: &mut FlighthookApp| {
        frame(app, vec![Event::PointerMoved(at)]);
        frame(app, vec![button(true)]);
        frame(app, vec![button(false)]);
    };

    frame(&mut app, Vec::new());
    click(&mut app);
    assert!(!app.confirm_clear, "no shots: disabled, no confirmation");

    app.shots = vec![row(1, true), row(2, true)];
    app.shots_rev += 1;
    click(&mut app);
    assert!(app.confirm_clear, "the confirmation opens");
    frame(&mut app, Vec::new());
    assert!(app.confirm_clear, "and stays open");
    frame(
        &mut app,
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(!app.confirm_clear, "Escape cancels");
    assert_eq!(app.shots.len(), 2, "nothing cleared");

    // Cleared elsewhere while open: it closes.
    click(&mut app);
    assert!(app.confirm_clear);
    app.shots.clear();
    frame(&mut app, Vec::new());
    assert!(!app.confirm_clear);
}

#[test]
fn the_export_icon_opens_a_menu_of_the_formats_left_of_the_trash() {
    use egui::{Event, Modifiers, PointerButton, Pos2, Rect, vec2};

    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.shots = vec![row(1, true)];
    app.shots_rev += 1;
    let frame = |app: &mut FlighthookApp, events: Vec<Event>| {
        ctx.data_mut(|d| d.remove_temp::<devices::Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
            events,
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| app.render_shots_panel(ui));
        });
        let marks = ctx
            .data(|d| d.get_temp::<devices::Marks>(egui::Id::NULL))
            .unwrap_or_default();
        (marks, out)
    };
    let click = |app: &mut FlighthookApp, at: Pos2| {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        frame(app, vec![Event::PointerMoved(at)]);
        frame(app, vec![button(true)]);
        frame(app, vec![button(false)]);
        frame(app, Vec::new())
    };

    // One icon, immediately left of the trash and the same size; no
    // caption, links or units control.
    let (marks, out) = frame(&mut app, Vec::new());
    let (export, trash) = (marks["export"].0, marks["clear_history"].0);
    assert_eq!(export.size(), trash.size(), "the trash's size");
    assert!(export.right() < trash.left() && trash.left() - export.right() < 16.0);
    assert!((export.center().y - trash.center().y).abs() < 0.5);
    assert!(marks["export"].1, "enabled");
    let texts: Vec<String> = painted_text_rects(&out.shapes)
        .into_iter()
        .map(|(t, _, _)| t)
        .collect();
    for gone in [
        "EXPORT SESSION",
        "UNITS",
        "VIEW",
        "CSV",
        "JSONL",
        "Imperial",
        "Metric",
    ] {
        assert!(!texts.iter().any(|t| t == gone), "{gone}");
    }
    assert!(!marks.contains_key("export:jsonl"), "closed");

    // A click opens the menu: JSONL then CSV, under the icon.
    let (marks, out) = click(&mut app, export.center());
    let (jsonl, csv) = (marks["export:jsonl"].0, marks["export:csv"].0);
    assert!(jsonl.bottom() <= csv.top() + 0.5, "JSONL first");
    assert!(jsonl.top() >= export.bottom() - 0.5, "under the icon");
    let texts: Vec<String> = painted_text_rects(&out.shapes)
        .into_iter()
        .map(|(t, _, _)| t)
        .collect();
    assert!(texts.iter().any(|t| t == "JSONL") && texts.iter().any(|t| t == "CSV"));

    // Picking one opens that export, in the shown units, and closes it.
    let url_of = |out: &egui::FullOutput| {
        out.platform_output.commands.iter().find_map(|c| match c {
            egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
            _ => None,
        })
    };
    let at = csv.center();
    let button = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    frame(&mut app, vec![Event::PointerMoved(at)]);
    frame(&mut app, vec![button(true)]);
    let (_, out) = frame(&mut app, vec![button(false)]);
    assert_eq!(url_of(&out), Some(net::export_url("csv", app.units())));
    let marks = frame(&mut app, Vec::new()).0;
    assert!(!marks.contains_key("export:csv"), "the menu closed");
    // The metric setting changes what the export asks for.
    app.set_default_units(UnitSystem::Metric);
    let (marks, _) = click(&mut app, export.center());
    let at = marks["export:jsonl"].0.center();
    let button = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    frame(&mut app, vec![Event::PointerMoved(at)]);
    frame(&mut app, vec![button(true)]);
    let (_, out) = frame(&mut app, vec![button(false)]);
    let url = url_of(&out).expect("the export opens");
    assert!(
        url.contains("format=jsonl") && url.contains("units=metric"),
        "{url}"
    );
}

/// 1000 simulated shots from two devices, each with every table field.
fn thousand_rows() -> Vec<ShotRow> {
    let yd = Distance::Yards;
    (0..1000_u32)
        .map(|n| {
            let carry = 130.0 + f64::from(n % 17);
            let ball = Launch {
                launch_speed: Some(Velocity::MilesPerHour(118.0 + f64::from(n % 7))),
                launch_elevation: Some(16.5),
                launch_azimuth: Some(f64::from(n % 9) - 4.0),
                backspin_rpm: Some(6200),
                sidespin_rpm: Some(-300),
                ..Default::default()
            };
            let flight = FlightResult {
                carry_distance: Some(yd(carry)),
                ..FlightResult::new(Basis::Modeled)
            };
            let mut derived = DerivedFlight::default();
            derived.max_height = Some(yd(31.0));
            derived.carry_offline = Some(yd(2.5));
            derived.descent_angle = Some(48.0);
            derived.roll_distance = Some(yd(11.0));
            derived.total_distance = Some(yd(carry + 11.0));
            derived.total_offline = Some(yd(3.1));
            ShotRow {
                actor: if n % 2 == 0 { "mevo.0" } else { "square.0" }.into(),
                launch: Some(ball),
                flight: Some(flight),
                derived: Some(derived),
                club_selected: Some(Club::Iron7),
                ..row(n, true)
            }
        })
        .collect()
}

/// Every text the frame painted, with whether it is in italics.
fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, bool)> {
    painted_text_rects(shapes)
        .into_iter()
        .map(|(text, italics, _)| (text, italics))
        .collect()
}

/// Every text the frame painted: the text, whether it is in italics, and
/// where.
fn painted_text_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, bool, egui::Rect)> {
    fn walk(shape: &egui::Shape, out: &mut Vec<(String, bool, egui::Rect)>) {
        match shape {
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
            egui::Shape::Text(t) => {
                let italics = t.galley.job.sections.iter().any(|s| s.format.italics);
                out.push((
                    t.galley.text().to_owned(),
                    italics,
                    t.visual_bounding_rect(),
                ));
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in shapes {
        walk(&clipped.shape, &mut out);
    }
    out
}

/// Gold rectangles the frame painted, as `(width, height)`.
fn gold_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<egui::Vec2> {
    shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Rect(r) if r.fill == theme::GOLD => Some(r.rect.size()),
            _ => None,
        })
        .collect()
}

#[test]
fn table_lays_out_only_the_rows_in_view() {
    use egui::{Pos2, Rect, pos2, vec2};

    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.shots = thousand_rows();
    app.shots_rev = 1;
    app.shots_view = ShotsView::Table;
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let frame = |app: &mut FlighthookApp, events: Vec<egui::Event>| {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
            events,
            ..Default::default()
        };
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.render_shots_panel(ui));
        })
    };
    // The first frames size the columns and settle the scroll.
    for _ in 0..5 {
        frame(&mut app, Vec::new());
    }
    let out = frame(&mut app, Vec::new());
    let header_at = |out: &egui::FullOutput| {
        painted_text_rects(&out.shapes)
            .into_iter()
            .find(|(t, _, _)| t == "T.Offline")
            .map(|(_, _, r)| r)
    };
    let header = header_at(&out).expect("header painted");
    let texts = painted_texts(&out.shapes);
    let has = |t: &str| texts.iter().any(|(s, _)| s == t);
    let numbers: Vec<u32> = texts.iter().filter_map(|(s, _)| s.parse().ok()).collect();

    // The sticky header, with the Device column for two devices.
    for name in [
        "Device",
        "#",
        "Club",
        "Carry",
        "Height",
        "T.Offline",
        "Smash",
    ] {
        assert!(has(name), "header {name}");
    }
    assert!(has("ft"), "apex in feet");
    // Stuck to the bottom: the latest shot is in view, the first is not,
    // and only a screenful of rows is laid out.
    assert!(numbers.contains(&999), "{numbers:?}");
    assert!(!numbers.contains(&500) && !numbers.contains(&3));
    assert!(texts.len() < 1500, "{} texts for 1000 rows", texts.len());
    // Simulated values in italics, measured ones not.
    assert!(
        texts.iter().any(|(s, i)| s == "+2.5" && *i),
        "simulated offline"
    );
    assert!(
        texts.iter().any(|(s, i)| s == "16.5" && !*i),
        "measured VLA"
    );
    // The latest row's gold stripe, a row tall.
    let stripe = gold_rects(&out.shapes);
    assert!(
        stripe
            .iter()
            .any(|s| s.x == STRIPE_WIDTH
                && s.y >= ctx.style().text_styles[&egui::TextStyle::Body].size),
        "{stripe:?}"
    );

    // Scrolled up, the middle rows are laid out instead.
    for _ in 0..10 {
        frame(
            &mut app,
            vec![
                egui::Event::PointerMoved(pos2(600.0, 450.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(0.0, 1300.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    let out = frame(&mut app, Vec::new());
    let texts = painted_texts(&out.shapes);
    let numbers: Vec<u32> = texts.iter().filter_map(|(s, _)| s.parse().ok()).collect();
    assert!(!numbers.contains(&999), "the latest row scrolled away");
    assert!(
        numbers.iter().any(|n| (300..700).contains(n)),
        "{numbers:?}"
    );
    assert_eq!(header_at(&out), Some(header), "the header stays put");
    assert!(gold_rects(&out.shapes).iter().all(|s| s.x != STRIPE_WIDTH));
}

/// A shot picked in the shot list beside the Table: its row is marked
/// with an accent stripe and scrolled into view.
#[test]
fn a_list_pick_marks_and_reveals_the_table_row() {
    use egui::{Pos2, Rect, vec2};

    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.shots = thousand_rows();
    app.shots_rev = 1;
    app.shots_view = ShotsView::Table;
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let frame = |app: &mut FlighthookApp| {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
            ..Default::default()
        };
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.render_shots_panel(ui));
        })
    };
    for _ in 0..5 {
        frame(&mut app);
    }
    let accent = |out: &egui::FullOutput| {
        out.shapes.iter().any(|c| {
            matches!(&c.shape, egui::Shape::Rect(r)
                if r.fill == theme::ACCENT && r.rect.width() == STRIPE_WIDTH)
        })
    };
    let numbers = |out: &egui::FullOutput| -> Vec<u32> {
        painted_texts(&out.shapes)
            .iter()
            .filter_map(|(s, _)| s.parse().ok())
            .collect()
    };
    assert!(!accent(&frame(&mut app)), "nothing picked");
    // The list's pick, as its click does it.
    app.select_shot(500);
    app.shot_focus.reveal();
    for _ in 0..3 {
        frame(&mut app);
    }
    let out = frame(&mut app);
    assert!(accent(&out), "the picked row is marked");
    assert!(numbers(&out).contains(&501), "{:?}", numbers(&out));
    assert_eq!(app.dispersion_shown(), Some(500), "Dispersion follows");
}

#[test]
fn table_cells_follow_the_columns() {
    use TableCell::{Muted, Number, Picked, Text};

    let rows = thousand_rows();
    for show_device in [false, true] {
        let columns = table_columns(show_device, true);
        assert_eq!(columns.len(), TABLE_COLUMNS + usize::from(show_device));
        let device = show_device.then_some("Mevo+");
        assert_eq!(
            table_cells(&rows[0], device, None, false).len(),
            columns.len()
        );
        assert_eq!(
            table_cells(&row(1, false), device, None, false).len(),
            columns.len()
        );
        // The first column clears the latest row's stripe.
        assert_eq!(columns[0].inset, ROW_INSET);
        assert!(columns[1..].iter().all(|c| c.inset == 0.0));
    }
    let columns = table_columns(true, false);
    let name = |n: &str| columns.iter().position(|c| c.name == n).expect(n);
    assert_eq!(columns[name("Height")].unit, "m");
    assert_eq!(columns[name("Club Spd")].unit, "m/s");

    let cells = table_cells(&rows[4], Some("Mevo+"), Some(3), true);
    assert_eq!(cells[0], Muted("Mevo+".into(), None));
    assert_eq!(cells[1], Number(4, true));
    assert_eq!(cells[2], Muted("7I".into(), Some("Set 3".into())));
    assert_eq!(cells[name("VLA")], Text("16.5".into()));
    assert_eq!(cells[name("Carry")], Picked(Some((134.0, false)), false));
    assert_eq!(cells[name("Offline")], Picked(Some((2.5, true)), true));
    assert_eq!(cells[name("Roll")], Picked(Some((11.0, true)), true));
    assert_eq!(cells[name("Smash")], Text("-".into()), "no club data");
    // A device without a name shows a dash.
    assert_eq!(
        table_cells(&rows[4], Some(""), None, false)[0],
        Text("-".into())
    );
    // A carry in both, with or without force: the simulated one, in
    // italics.
    for replaces in [false, true] {
        let mut both = rows[4].clone();
        let d = both.derived.as_mut().expect("derived");
        d.carry_distance = Some(Distance::Yards(150.0));
        d.replaces_measured = replaces;
        let cells = table_cells(&both, None, None, false);
        assert_eq!(cells[name("Carry") - 1], Picked(Some((150.0, true)), false));
    }
}

#[test]
fn table_rows_alternate_and_the_latest_is_tinted() {
    let (latest, stripe) = (egui::Color32::GOLD, egui::Color32::GRAY);
    assert_eq!(table_row_fill(0, 4, latest, stripe), None);
    assert_eq!(table_row_fill(1, 4, latest, stripe), Some(stripe));
    assert_eq!(table_row_fill(2, 4, latest, stripe), None);
    assert_eq!(table_row_fill(3, 4, latest, stripe), Some(stripe));
    assert_eq!(table_row_fill(4, 4, latest, stripe), Some(latest));
    assert_eq!(TABLE_COLUMNS, 22);
}

/// Mean frame time of the Table view with 1000 shots (and the Dispersion
/// column), scrolled to the bottom and to the middle. Run with
/// `cargo test --release -p flighthook-ui table_frame_time -- --ignored --nocapture`.
#[test]
#[ignore = "timing, not a check"]
fn table_frame_time_with_1000_shots() {
    use egui::{Pos2, Rect, pos2, vec2};
    use std::time::Instant;

    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.shots = thousand_rows();
    app.shots_rev = 1;
    app.shots_view = ShotsView::Table;
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let frames = 200;
    let run = |app: &mut FlighthookApp, scroll: f32| {
        let mut total = std::time::Duration::ZERO;
        for n in 0..frames + 20 {
            let mut events = vec![egui::Event::PointerMoved(pos2(
                300.0 + (n % 50) as f32 * 16.0,
                450.0,
            ))];
            if n < 10 && scroll != 0.0 {
                events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(0.0, scroll),
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
                events,
                ..Default::default()
            };
            let start = Instant::now();
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.render_shots_panel(ui));
            });
            let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
            std::hint::black_box(prims);
            if n >= 20 {
                total += start.elapsed();
            }
        }
        total / frames
    };
    let bottom = run(&mut app, 0.0);
    // 10 wheel steps of 1500 pt up from the bottom: about the middle.
    let middle = run(&mut app, 1500.0);
    println!(
        "Table, 1000 shots, 1600x900: frame {bottom:?} at the bottom, {middle:?} \
             scrolled up (run + tessellate, mean of {frames})"
    );
}

#[test]
fn clear_confirm_counts_the_shots() {
    assert_eq!(
        clear_confirm_title(12, false),
        "Delete all 12 shots and sets?"
    );
    assert_eq!(
        clear_confirm_title(1, false),
        "Delete the 1 shot and its set?"
    );
    assert_eq!(clear_confirm_title(12, true), "Clear every golfer's shots?");
}

fn card<'a>(groups: &'a [MetricGroup; 3], label: &str) -> &'a Metric {
    groups
        .iter()
        .flat_map(|(_, g)| g)
        .find(|m| m.label == label)
        .expect(label)
}

/// A shot with every directional value set to `sign` × a value.
fn signed_shot(sign: f64) -> ShotRow {
    let mut shot = row(1, true);
    let ball = shot.launch_mut();
    ball.launch_azimuth = Some(2.4 * sign);
    // Rounds to 0 for the near-zero signs.
    #[allow(clippy::cast_possible_truncation)]
    let sidespin = (644.0 * sign).round() as i32;
    ball.sidespin_rpm = Some(sidespin);
    shot.flight_mut().carry_offline = Some(Distance::Yards(22.4 * sign));
    shot.club = Some(ClubData {
        path: Some(1.2 * sign),
        attack_angle: Some(0.9 * sign),
        face_angle: Some(1.8 * sign),
        ..Default::default()
    });
    shot
}

#[test]
fn directional_tiles_show_a_magnitude_and_a_word() {
    use Handedness::{Left, Right};
    let words = |sign: f64, handed| {
        let (_, groups) = card_metrics(&signed_shot(sign), true, handed);
        ["Direction", "Sidespin", "Offline", "Path", "AoA", "Face"].map(|l| {
            let m = card(&groups, l);
            (m.text.clone(), m.word)
        })
    };
    let text = |w: [(String, Option<&str>); 6]| w.map(|(t, _)| t);
    let word = |w: [(String, Option<&'static str>); 6]| w.map(|(_, w)| w);
    // Values are magnitudes, the same for both signs and both hands.
    let magnitudes = ["2.4", "644", "22.4", "1.2", "0.9", "1.8"].map(String::from);
    for sign in [1.0, -1.0] {
        for handed in [Right, Left] {
            assert_eq!(text(words(sign, handed)), magnitudes);
        }
    }
    assert_eq!(
        word(words(1.0, Right)),
        [
            Some("R"),
            Some("Fade"),
            Some("R"),
            Some("In-to-out"),
            Some("Up"),
            Some("Open")
        ]
    );
    assert_eq!(
        word(words(-1.0, Right)),
        [
            Some("L"),
            Some("Draw"),
            Some("L"),
            Some("Out-to-in"),
            Some("Down"),
            Some("Closed")
        ]
    );
    assert_eq!(
        word(words(1.0, Left)),
        [
            Some("R"),
            Some("Draw"),
            Some("R"),
            Some("Out-to-in"),
            Some("Up"),
            Some("Closed")
        ]
    );
    assert_eq!(
        word(words(-1.0, Left)),
        [
            Some("L"),
            Some("Fade"),
            Some("L"),
            Some("In-to-out"),
            Some("Down"),
            Some("Open")
        ]
    );
    // Zero, or a value shown as zero, has no word.
    for sign in [0.0, 0.0005, -0.0005] {
        let w = words(sign, Right);
        assert!(w.iter().all(|(_, w)| w.is_none()), "{sign}: {w:?}");
        assert_eq!(w[0].0, "0.0");
    }
    // Roll keeps its sign: it is not a direction.
    let (_, groups) = card_metrics(&signed_shot(-1.0), true, Right);
    assert_eq!(card(&groups, "Roll").word, None);
}

#[test]
fn sign_words_fit_their_tiles_at_1024x768() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    // The widest realistic readings with the longest words.
    let mut shot = signed_shot(-1.0);
    shot.club.as_mut().unwrap().path = Some(-10.8);
    shot.club.as_mut().unwrap().face_angle = Some(-10.8);
    shot.club.as_mut().unwrap().attack_angle = Some(-10.8);
    shot.launch_mut().sidespin_rpm = Some(-1644);
    let (_, groups) = card_metrics(&shot, true, Handedness::Right);
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        let painter = ctx.layer_painter(egui::LayerId::background());
        for (i, &(w, h, narrow)) in FIT_AREAS.iter().enumerate() {
            for width in [w, narrow] {
                let layout =
                    cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], Some(Legend::Beside), 1);
                let detail = layout.groups[0].1[0];
                let all = groups.iter().flat_map(|(_, m)| m);
                let look = CardLook::detail(detail.size()).fit(&painter, all, detail.width());
                let room = detail.width() - 2.0 * look.pad;
                for m in groups.iter().flat_map(|(_, m)| m) {
                    let (reading, size) = fitted_reading(&painter, m, look.value, room);
                    let at = format!("{width}x{h} {}", m.label);
                    assert!(reading.width() <= room + 0.01, "{at} overflows");
                    assert!(m.word.is_none() || reading.word.is_some(), "{at}");
                    if FIT_WINDOWS.get(i) == Some(&(1024.0, 768.0)) {
                        // At 1024x768 no numeral gives way to its word.
                        assert_eq!(size, look.value, "{at}");
                    }
                }
            }
        }
    });
}

#[test]
fn the_lateral_bar_reads_left_and_right() {
    let k = BarKind::Lateral;
    assert_eq!(k.format(-3.24, 0), "3.2 L");
    assert_eq!(k.format(11.9, 0), "11.9 R");
    assert_eq!(k.format(0.04, 0), "0.0");
    assert_eq!(k.format(-0.04, 0), "0.0");
    assert_eq!(BarKind::Total.format(201.4, 0), "201");
}

#[test]
fn the_table_shows_face_impact_in_words() {
    use crate::types::FaceImpact;
    let mut shot = row(7, true);
    shot.impact = Some(FaceImpact {
        lateral: Some(Distance::Millimeters(4.4)),
        vertical: Some(Distance::Millimeters(-2.2)),
    });
    let columns = table_columns(false, true);
    let at = |n: &str| columns.iter().position(|c| c.name == n).expect(n);
    assert_eq!(at("Impact H"), at("Smash") + 1);
    assert_eq!(at("Impact V"), at("Smash") + 2);
    assert_eq!(columns[at("Impact H")].unit, "mm");
    let cells = table_cells(&shot, None, None, false);
    assert_eq!(cells.len(), columns.len());
    assert_eq!(cells[at("Impact H")], TableCell::Text("4 toe".into()));
    assert_eq!(cells[at("Impact V")], TableCell::Text("2 low".into()));
    // No reading: dashes; and the same with the device column.
    let cells = table_cells(&row(8, true), Some("Omni"), None, false);
    let columns = table_columns(true, true);
    assert_eq!(cells.len(), columns.len());
    assert_eq!(cells[columns.len() - 1], TableCell::Text("-".into()));
}

/// The most cards beside the bars at each of [`FIT_AREAS`]' widths.
const FIT_CARDS: [usize; 6] = [3, 3, 2, 2, 2, 1];

/// The headline tiles' x ranges, and their bottom.
fn head_columns(layout: &CardsLayout) -> (Vec<egui::Rangef>, f32) {
    let top = layout.headline[0].top();
    let columns = layout
        .headline
        .iter()
        .filter(|c| c.top() == top)
        .map(|c| c.x_range())
        .collect();
    let bottom = layout
        .headline
        .iter()
        .map(|c| c.bottom())
        .fold(0.0, f32::max);
    (columns, bottom)
}

#[test]
fn the_card_row_takes_headline_columns_right_of_the_bars() {
    for (i, &(w, h, narrow)) in FIT_AREAS[..FIT_WINDOWS.len()].iter().enumerate() {
        for width in [w, narrow] {
            for legend in [Legend::Beside, Legend::Below] {
                // No cards at all: what the card row must leave as it is.
                let none = cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], Some(legend), 0);
                assert!(none.cards.is_empty());
                for sources in 1..=5 {
                    let at = format!("{:?} {width}x{h} {legend:?} {sources}", FIT_WINDOWS[i]);
                    let layout =
                        cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], Some(legend), sources);
                    let cards = &layout.cards;
                    assert_eq!(cards.len(), sources.min(FIT_CARDS[i]), "{at}");
                    let bars = layout.bars.expect("bars");
                    // One headline column wide each, under the rightmost
                    // columns, flush right, CARD_GAP from the headline
                    // cards and from the detail section's hairline.
                    let (columns, head_bottom) = head_columns(&layout);
                    let rule = layout.groups[0].0.top() + GROUP_RULE_Y;
                    let under = &columns[columns.len() - cards.len()..];
                    for (card, column) in cards.iter().zip(under) {
                        assert!((card.left() - column.min).abs() < 0.01, "{at}");
                        assert!((card.right() - column.max).abs() < 0.01, "{at}");
                        assert!((card.top() - (head_bottom + CARD_GAP)).abs() < 0.01, "{at}");
                        assert!((card.bottom() - (rule - CARD_GAP)).abs() < 0.01, "{at}");
                        assert!(!card.intersects(bars.controls), "{at}");
                        assert!(layout.headline.iter().all(|c| !c.intersects(*card)), "{at}");
                        for (label, details) in &layout.groups {
                            assert!(!label.intersects(*card), "{at}");
                            assert!(details.iter().all(|c| !c.intersects(*card)), "{at}");
                        }
                    }
                    assert!((cards[cards.len() - 1].right() - width).abs() < 0.01);
                    for pair in cards.windows(2) {
                        assert!((pair[1].left() - pair[0].right() - CARD_GAP).abs() < 0.01);
                    }
                    // The bars end FACE_GAP short of the leftmost card,
                    // keep the two leftmost columns and the least plot.
                    for r in [bars.controls, bars.label, bars.legend]
                        .into_iter()
                        .chain(bars.rows)
                    {
                        assert!(
                            (r.right() + FACE_GAP - cards[0].left()).abs() < 0.01,
                            "{at}"
                        );
                        assert!(!r.intersects(cards[0]), "{at}");
                    }
                    assert!(
                        bars.rows[0].right() + FACE_GAP >= columns[2].min - 0.01,
                        "{at}"
                    );
                    assert!(
                        bar_plot_width(bars.rows[0].width()) >= FACE_MIN_PLOT,
                        "{at}"
                    );
                    // Nothing else moves with the cards.
                    assert_eq!(layout.height, none.height, "{at}");
                    assert!(!layout.scrolls, "{at}");
                    assert_eq!(layout.headline, none.headline, "{at}");
                    assert_eq!(layout.groups, none.groups, "{at}");
                    let none_bars = none.bars.expect("bars");
                    assert_eq!(
                        bars.rows.map(|r| r.y_range()),
                        none_bars.rows.map(|r| r.y_range())
                    );
                }
            }
        }
    }
    // No bars, no cards.
    assert!(
        cards_layout(egui::vec2(962.0, 476.0), 5, &[2, 4, 6], None, 3)
            .cards
            .is_empty()
    );
}

#[test]
fn a_narrow_row_falls_back_to_a_square_then_to_what_the_bars_leave() {
    let (mut columns, mut squares, mut shrunk, mut zero) = (0, 0, 0, 0);
    for &(_, h, _) in &FIT_AREAS {
        for legend in [Legend::Beside, Legend::Below] {
            for width in (300..=1300).step_by(3) {
                let width = width as f32;
                for sources in 1..=5 {
                    let layout =
                        cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], Some(legend), sources);
                    let at = format!("{width}x{h} {legend:?} {sources}");
                    let bars = layout.bars.expect("bars");
                    let cards = &layout.cards;
                    assert!(!cards.is_empty() && cards.len() <= sources.min(3), "{at}");
                    let head_w = layout.headline[0].width();
                    let square = bars.rows[2].bottom() - bars.label.top();
                    let plot = bar_plot_width(bars.rows[0].width());
                    assert!(
                        (cards[cards.len() - 1].right() - width).abs() < 0.01,
                        "{at}"
                    );
                    if (cards[0].width() - head_w).abs() < 0.01 {
                        columns += 1;
                        assert!(plot >= FACE_MIN_PLOT, "{at}");
                    } else {
                        assert_eq!(cards.len(), 1, "{at}");
                        if (cards[0].width() - square).abs() < 0.01 {
                            squares += 1;
                            assert!(plot >= FACE_MIN_PLOT, "{at}");
                        } else if cards[0].width() > 0.0 {
                            // The bars keep the least plot, the card the rest.
                            shrunk += 1;
                            assert!((plot - FACE_MIN_PLOT).abs() < 0.01, "{at}: {plot}");
                            assert!(cards[0].width() < square, "{at}");
                        } else {
                            // Nothing left for the card, which takes
                            // no gap from the bars.
                            assert_eq!(cards[0].width(), 0.0, "{at}");
                            assert!(plot < FACE_MIN_PLOT + FACE_GAP + 1.0, "{at}: {plot}");
                            assert!((bars.rows[0].right() - width).abs() < 0.01, "{at}");
                            zero += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        columns > 0 && squares > 0 && shrunk > 0 && zero > 0,
        "{columns} {squares} {shrunk} {zero}"
    );
}

#[test]
fn the_legend_settles_where_it_fits_beside_the_cards() {
    let (mut beside, mut below) = (0, 0);
    for &(_, h, _) in &FIT_AREAS {
        let mut width = 560.0;
        while width <= 1300.0 {
            for sources in 1..=5 {
                let at = |legend: Legend| {
                    cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], Some(legend), sources)
                };
                // The legend fits beside the title in `fits` and up.
                for fits in [200.0, 320.0, 450.0, 700.0, 2000.0] {
                    let place = |w: f32| {
                        if w >= fits {
                            Legend::Beside
                        } else {
                            Legend::Below
                        }
                    };
                    let layout = settle_legend(place, at);
                    let bars = layout.bars.expect("bars");
                    let at_ = format!("{width}x{h} {sources}, beside from {fits}");
                    // The cards don't depend on where the legend goes.
                    assert_eq!(layout.cards.len(), at(Legend::Beside).cards.len());
                    if bars.legend == bars.label {
                        beside += 1;
                        assert!(bars.label.width() >= fits, "{at_}");
                    } else {
                        below += 1;
                        let narrow = at(Legend::Beside).bars.expect("bars").label.width();
                        assert!(narrow < fits, "{at_}");
                    }
                }
            }
            width += 2.0;
        }
    }
    assert!(beside > 0 && below > 0);
}

/// A bar of `kind` over `values` among `rows` shots with data.
fn stats_bar(kind: BarKind, values: &[f64], rows: usize) -> Bar {
    Bar {
        kind,
        unit: "yd",
        values: BarValues {
            values: values.to_vec(),
            rows,
        },
        stats: stats(values),
        devices: Vec::new(),
        this: None,
        domain: None,
    }
}

/// Wide stats in every column: three-digit distances, a lateral with
/// its side, two-digit spreads and an `11/12` on every row.
fn wide_stats_bars() -> [Bar; 3] {
    let spread = |base: f64, step: f64| -> Vec<f64> {
        (0..11).map(|i| base + step * f64::from(i)).collect()
    };
    [
        stats_bar(BarKind::Total, &spread(248.0, 9.7), 12),
        stats_bar(BarKind::Carry, &spread(228.0, 8.8), 12),
        stats_bar(BarKind::Lateral, &spread(-48.0, 4.3), 12),
    ]
}

/// Paint `stats_card` alone at `rect` and return its texts and rects.
fn painted_stats_card(
    rect: egui::Rect,
    bars: &[Bar; 3],
    rows: &[egui::Rect; 3],
) -> Vec<(String, bool, egui::Rect)> {
    use egui::{Pos2, Rect, vec2};
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let paint = || {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 800.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show(ctx, |ui| stats_card(ui.painter(), rect, bars, rows, 13.0));
            },
        )
    };
    // The second pass has the fonts in.
    paint();
    painted_text_rects(&paint().shapes)
}

#[test]
fn the_stats_card_is_a_titled_table_of_the_bars() {
    let mut bars = [
        stats_bar(BarKind::Total, &[163.0, 181.0, 175.0], 3),
        stats_bar(BarKind::Carry, &[150.0, 168.0], 3),
        stats_bar(BarKind::Lateral, &[], 3),
    ];
    bars[0].unit = "yd";
    let layout = cards_layout(
        egui::vec2(962.0, 476.0),
        5,
        &[2, 4, 6],
        Some(Legend::Below),
        3,
    );
    let at = layout.bars.expect("bars");
    let rect = layout.cards[0];
    let texts = painted_stats_card(rect, &bars, &at.rows);
    let find = |t: &str| {
        texts
            .iter()
            .find(|(x, _, _)| x == t)
            .map(|(_, _, r)| *r)
            .unwrap_or_else(|| panic!("{t} in {texts:?}"))
    };
    let left = rect.left() + impact::PAD;
    // The title in the top-left, then the column headers over the rows.
    let title = find("STATS");
    assert!((title.left() - left).abs() < 1.0, "{title:?}");
    assert!(title.top() >= rect.top() && title.top() <= rect.top() + 12.0);
    let heads = ["AVG", "MED", "SD"].map(find);
    for pair in heads.windows(2) {
        assert!(pair[0].right() < pair[1].left(), "{heads:?}");
    }
    assert!(heads.iter().all(|h| h.top() >= title.bottom()), "{heads:?}");
    // One row per bar in bar order, named like the bars, unit under.
    let names = ["TOTAL", "CARRY", "LATERAL"].map(find);
    for (name, head) in names.iter().zip([heads[0]; 3]) {
        assert!((name.left() - left).abs() < 1.0, "{name:?}");
        assert!(name.top() >= head.bottom(), "{name:?} under {head:?}");
    }
    for pair in names.windows(2) {
        assert!(pair[0].bottom() <= pair[1].top(), "{names:?}");
    }
    let units: Vec<egui::Rect> = texts
        .iter()
        .filter(|(t, _, _)| t == "yd")
        .map(|(_, _, r)| *r)
        .collect();
    assert_eq!(units.len(), 3);
    // Values right-aligned under their headers, on their row's line.
    let row_of = |r: egui::Rect| {
        names
            .iter()
            .position(|n| r.center().y > n.top() && r.center().y < n.bottom() + 14.0)
    };
    for (text, row, col) in [
        ("173", 0, 0),
        ("175", 0, 1),
        ("9.2", 0, 2),
        ("159", 1, 0),
        ("159", 1, 1),
        ("12.7", 1, 2),
    ] {
        let at = texts
            .iter()
            .filter(|(t, _, r)| t == text && row_of(*r) == Some(row))
            .map(|(_, _, r)| *r)
            .find(|r| (r.right() - heads[col].right()).abs() < 1.0)
            .unwrap_or_else(|| panic!("{text} in row {row}, column {col}: {texts:?}"));
        assert!(rect.contains_rect(at), "{at:?}");
    }
    // The row missing a value says k/n at its end; no data, a dash in
    // each column and no k/n.
    let count = find("2/3");
    assert_eq!(row_of(count), Some(1));
    assert!((count.right() - heads[2].right()).abs() < 1.0, "{count:?}");
    let dashes = texts
        .iter()
        .filter(|(t, _, r)| t == NO_VALUE && row_of(*r) == Some(2))
        .count();
    assert_eq!(dashes, 3);
    assert!(!texts.iter().any(|(t, _, _)| t.contains('/') && t != "2/3"));
    // A card too narrow for the table is clipped to the card.
    let thin = egui::Rect::from_min_size(rect.min, egui::vec2(60.0, rect.height()));
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let out = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ctx, |ui| {
                stats_card(ui.painter(), thin, &bars, &at.rows, 13.0)
            });
    });
    for clipped in &out.shapes {
        if matches!(clipped.shape, egui::Shape::Text(_)) {
            assert!(
                thin.contains_rect(clipped.clip_rect),
                "{:?}",
                clipped.clip_rect
            );
        }
    }
}

#[test]
fn the_stats_card_fits_every_listed_window() {
    let bars = wide_stats_bars();
    let texts_of = |b: &Bar| StatsRow::of(b);
    assert_eq!(
        texts_of(&bars[2]),
        StatsRow {
            avg: "26.5 L".into(),
            med: "26.5 L".into(),
            sd: "14.3".into(),
            count: Some("11/12".into()),
        }
    );
    let ctx = egui::Context::default();
    theme::install(&ctx);
    for (i, &(w, h, narrow)) in FIT_AREAS[..FIT_WINDOWS.len()].iter().enumerate() {
        for width in [w, narrow] {
            for legend in [Legend::Beside, Legend::Below] {
                for want in 1..=MAX_CARDS {
                    let at = format!("{:?} {width} {legend:?} {want}", FIT_WINDOWS[i]);
                    let layout =
                        cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], Some(legend), want);
                    let rows = layout.bars.expect("bars").rows;
                    let rect = layout.cards[0];
                    // The whole table at its widths, no gap closed up.
                    let inner = rect.width() - 2.0 * impact::PAD;
                    let _ = ctx.run(egui::RawInput::default(), |ctx| {
                        let texts = bars.each_ref().map(StatsRow::of);
                        let table = stats_table(
                            &ctx.layer_painter(egui::LayerId::background()),
                            &bars,
                            &texts,
                            inner,
                        );
                        assert!(table.width <= inner + 0.01, "{at}: {table:?} in {inner}");
                        assert!(table.value >= STATS_VALUE_MIN, "{at}");
                    });
                    // Every text inside the card's padding, none cut.
                    let texts = painted_stats_card(rect, &bars, &rows);
                    assert!(texts.len() >= 4 + 3 + 3 + 3 * 4, "{at}: {texts:?}");
                    let pad = rect.shrink2(egui::vec2(impact::PAD - 0.5, 0.0));
                    for (text, _, r) in &texts {
                        assert!(pad.contains_rect(*r), "{at}: {text} {r:?} in {rect:?}");
                        assert!(!text.contains('\u{2026}'), "{at}: {text}");
                    }
                    // Title, headers and rows don't overlap.
                    for (n, (a, _, ra)) in texts.iter().enumerate() {
                        for (b, _, rb) in &texts[n + 1..] {
                            assert!(
                                ra.intersect(*rb).area() < 1.0,
                                "{at}: {a} {ra:?} over {b} {rb:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn the_face_readout_fits_the_smallest_panel() {
    use crate::types::FaceImpact;
    use egui::{Pos2, Rect, vec2};
    // The smallest panel: a square as tall as the shortest span.
    let side = COMPACT.bars(Legend::Beside) - COMPACT.controls;
    let rect = Rect::from_min_size(Pos2::new(20.0, 20.0), vec2(side, side));
    let mut shot = row(1, true);
    shot.impact = Some(FaceImpact {
        lateral: Some(Distance::Millimeters(-30.0)),
        vertical: Some(Distance::Millimeters(30.0)),
    });
    let shots = [shot];
    let panel = impact::FacePanel::new(&shots, &[0], 0, None, None, |_| theme::ACCENT, |_| None);
    assert_eq!(panel.readout, "30 heel \u{b7} 30 high");
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let out = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0))),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new())
                .show(ctx, |ui| {
                    let _ = impact::paint_face_panel(ui, rect, &panel);
                });
        },
    );
    let (_, _, at) = painted_text_rects(&out.shapes)
        .into_iter()
        .find(|(t, _, _)| *t == panel.readout)
        .expect("the readout, whole");
    assert!(
        rect.shrink(impact::PAD).x_range().contains(at.left())
            && rect.shrink(impact::PAD).x_range().contains(at.right()),
        "{at:?} in {rect:?}"
    );
}

/// The Cards app with face impact on some of its shots.
fn impact_app() -> FlighthookApp {
    use crate::types::FaceImpact;
    let mut app = cards_app();
    for (n, shot) in (0_u32..).zip(app.shots.iter_mut()) {
        let skip = n % 3 == 1;
        let n = f64::from(n);
        shot.impact = (!skip).then(|| FaceImpact {
            lateral: Some(Distance::Millimeters(n - 5.0)),
            vertical: Some(Distance::Millimeters(2.0 - n / 3.0)),
        });
    }
    app
}

#[test]
fn the_face_panel_shows_while_any_shot_has_impact() {
    for (w, h) in FIT_WINDOWS {
        let window = egui::vec2(w, h);
        for (with_impact, mut app) in [(true, impact_app()), (false, cards_app())] {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            for _ in 0..3 {
                shots_frame(&ctx, &mut app, window);
            }
            let out = shots_frame(&ctx, &mut app, window);
            let texts = painted_texts(&out.shapes);
            let painted = |t: &str| texts.iter().any(|(x, _)| x == t);
            assert_eq!(painted("TOE") && painted("HEEL"), with_impact, "{w}x{h}");
            let right = w
                - f32::from(TAB_MARGIN.right)
                - chart_slots(ShotsView::Cards, window).right.unwrap_or(0.0);
            assert_eq!(scroll_bars(&out, right), 0, "{w}x{h}: a scroll bar");
        }
    }
    // History-wide: the shown shot without impact still shows it, with
    // a dash for its reading.
    let mut app = impact_app();
    let last = app.shots.len() - 1;
    app.shots[last].impact = None;
    app.shots_rev += 1;
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let window = egui::vec2(1600.0, 900.0);
    for _ in 0..3 {
        shots_frame(&ctx, &mut app, window);
    }
    let texts = painted_texts(&shots_frame(&ctx, &mut app, window).shapes);
    assert!(texts.iter().any(|(t, _)| t == "TOE"));
    assert!(texts.iter().any(|(t, _)| t == "-"));
    // Gone once no shot has any.
    for shot in &mut app.shots {
        shot.impact = None;
    }
    app.shots_rev += 1;
    for _ in 0..3 {
        shots_frame(&ctx, &mut app, window);
    }
    let texts = painted_texts(&shots_frame(&ctx, &mut app, window).shapes);
    assert!(!texts.iter().any(|(t, _)| t == "TOE"));
}

/// A clip of the shot from `camera`.
fn clip(camera: &str) -> crate::types::ShotVideo {
    crate::types::ShotVideo {
        camera: camera.into(),
        position: crate::types::CameraPosition::GolferFront,
        duration_ms: 1000,
        fps: 100.0,
        trigger_offset_ms: 500,
        impact_offset_ms: 500,
        complete: true,
        dropped: 0,
        pose: false,
        swing: false,
        rotation: 0,
        analyzed_with: None,
    }
}

/// One pass of the Shots tab at 1600×900 with `events`; returns the
/// marked controls and the output.
fn marked_frame(
    ctx: &egui::Context,
    app: &mut FlighthookApp,
    events: Vec<egui::Event>,
) -> (devices::Marks, egui::FullOutput) {
    ctx.data_mut(|d| d.remove_temp::<devices::Marks>(egui::Id::NULL));
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let out = ctx.run(input, |ctx| {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(TAB_MARGIN))
            .show(ctx, |ui| app.render_shots_panel(ui));
    });
    let marks = ctx
        .data(|d| d.get_temp::<devices::Marks>(egui::Id::NULL))
        .unwrap_or_default();
    (marks, out)
}

/// Click the middle of control `name`; returns the marks after.
fn click_marked(
    ctx: &egui::Context,
    app: &mut FlighthookApp,
    marks: &devices::Marks,
    name: &str,
) -> devices::Marks {
    use egui::{Event, Modifiers, PointerButton};
    let at = marks[name].0.center();
    let button = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    marked_frame(ctx, app, vec![Event::PointerMoved(at)]);
    marked_frame(ctx, app, vec![button(true)]);
    marked_frame(ctx, app, vec![button(false)]);
    marked_frame(ctx, app, Vec::new()).0
}

#[test]
fn a_shot_with_clips_has_a_replay_chip_that_opens_the_replay() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = cards_app();
    let marks = marked_frame(&ctx, &mut app, Vec::new()).0;
    assert!(!marks.contains_key("replay"), "no clips, no chip");

    let last = app.shots.len() - 1;
    app.shots[last].video = vec![clip("camera.0"), clip("camera.1")];
    app.shots_rev += 1;
    marked_frame(&ctx, &mut app, Vec::new());
    let marks = marked_frame(&ctx, &mut app, Vec::new()).0;
    let marks = click_marked(&ctx, &mut app, &marks, "replay");
    assert!(app.replay.is_some());
    for camera in ["camera.0", "camera.1"] {
        assert!(marks.contains_key(&format!("replay:download:{camera}")));
    }
    assert!(!marks.contains_key("replay:play"), "no sidecar yet");

    // The sidecars arrive: playing from 2 s before the impact, which the
    // clip (1.0 to 2.0 s, impact at 1.5 s) starts later than, so from its
    // start; the playhead holds there while the frames are on their way.
    let seq = app.replay_seq;
    for camera in ["camera.0", "camera.1"] {
        let t = crate::replay::tests::track(1000, 10, 100, 50);
        let json = serde_json::json!({
            "version": 1, "fourcc": "MJPG", "width": 64, "height": 48,
            "fps": {"nominal": null, "measured": 100.0},
            "start_ns": t.start_ns, "frames": t.frames,
            "trigger_index": 50, "impact_index": 50,
            "trigger_ns": 500_000_000, "impact_ns": 500_000_000,
            "complete": true, "dropped": 0, "truncated": false,
        });
        app.pending
            .lock()
            .unwrap()
            .replay
            .push(crate::net::ReplayReply::Sidecar {
                replay: seq,
                camera: camera.into(),
                result: stance::sidecar::Sidecar::from_json(&json.to_string())
                    .map_err(|e| e.to_string()),
            });
    }
    app.apply_pending();
    marked_frame(&ctx, &mut app, Vec::new());
    let marks = marked_frame(&ctx, &mut app, Vec::new()).0;
    for name in [
        "replay:play",
        "replay:step:true",
        "replay:step:false",
        "replay:speed",
        "replay:scrubber",
    ] {
        assert!(marks.contains_key(name), "{name}");
    }
    let start = app
        .replay
        .as_ref()
        .unwrap()
        .playback
        .as_ref()
        .unwrap()
        .playhead;
    assert_eq!(start, 1_000_000_000, "clamped to the clip start");
    assert!(
        app.replay
            .as_ref()
            .unwrap()
            .playback
            .as_ref()
            .unwrap()
            .playing,
        "opens playing"
    );
    // Pausing before the frames are in cancels the autoplay.
    let marks = click_marked(&ctx, &mut app, &marks, "replay:play");
    let p = app.replay.as_ref().unwrap().playback.clone().unwrap();
    assert_eq!((p.playhead, p.playing), (start, false));
    let marks = click_marked(&ctx, &mut app, &marks, "replay:step:true");
    let p = app.replay.as_ref().unwrap().playback.clone().unwrap();
    assert_eq!(p.playhead, 1_010_000_000, "one frame on");
    let marks = click_marked(&ctx, &mut app, &marks, "replay:play");
    assert!(
        app.replay
            .as_ref()
            .unwrap()
            .playback
            .as_ref()
            .unwrap()
            .playing
    );

    let _ = click_marked(&ctx, &mut app, &marks, "replay:close");
    assert!(app.replay.is_none());
}

/// `app` with connected cameras `camera.0` "Face-on" and `camera.1`
/// "Down the line".
fn with_cameras(mut app: FlighthookApp) -> FlighthookApp {
    use crate::types::ActorStatus;
    for (id, name) in [("camera.0", "Face-on"), ("camera.1", "Down the line")] {
        app.actors.insert(
            id.into(),
            ActorStatusResponse::new(name, ActorStatus::Connected),
        );
    }
    app
}

/// The card row's rects as last painted.
fn row_cards(ctx: &egui::Context) -> Vec<egui::Rect> {
    ctx.data(|d| d.get_temp::<Vec<egui::Rect>>(egui::Id::new("row_cards")))
        .unwrap_or_default()
}

/// Click at `at` in a `window`-sized app, then settle a frame.
fn click_at(ctx: &egui::Context, app: &mut FlighthookApp, window: egui::Vec2, at: egui::Pos2) {
    use egui::{Event, Modifiers, PointerButton};
    let frame = |app: &mut FlighthookApp, events: Vec<Event>| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, window)),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(TAB_MARGIN))
                .show(ctx, |ui| app.render_shots_panel(ui));
        });
    };
    let button = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    frame(app, vec![Event::PointerMoved(at)]);
    frame(app, vec![button(true)]);
    frame(app, vec![button(false)]);
    frame(app, Vec::new());
}

#[test]
fn the_card_row_shows_stats_cameras_and_face_impact_and_cycles_on_a_click() {
    use card_row::CardSource::{Camera, Face, Stats};
    let cam = |n: u8| Camera(format!("camera.{n}"));
    // Stats only, with nothing else to show.
    for (w, h) in FIT_WINDOWS {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = cards_app();
        for _ in 0..3 {
            shots_frame(&ctx, &mut app, egui::vec2(w, h));
        }
        assert_eq!(app.card_row.shown, [Stats], "{w}x{h}");
        assert_eq!(row_cards(&ctx).len(), 1, "{w}x{h}");
    }
    // Two cameras and face impact: three cards at 1600x900, two at
    // 1024x768.
    for (window, expect) in [
        ((1600.0, 900.0), vec![Stats, cam(0), Face]),
        ((1024.0, 768.0), vec![Stats, Face]),
    ] {
        let window = egui::vec2(window.0, window.1);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = with_cameras(impact_app());
        for _ in 0..3 {
            shots_frame(&ctx, &mut app, window);
        }
        assert_eq!(app.card_row.shown, expect, "{window:?}");
        let texts = painted_texts(&shots_frame(&ctx, &mut app, window).shapes);
        let painted = |t: &str| texts.iter().any(|(x, _)| x == t);
        assert!(painted("TOE"), "{window:?}");
        assert_eq!(painted("FACE-ON"), expect.contains(&cam(0)), "{window:?}");
        assert!(!painted("DOWN THE LINE"), "{window:?}");
        // A click on the second card moves it on to what is not shown.
        let rects = row_cards(&ctx);
        assert_eq!(rects.len(), expect.len());
        click_at(&ctx, &mut app, window, rects[1].center());
        let after = if expect.len() == 3 {
            vec![Stats, cam(1), Face]
        } else {
            vec![Stats, cam(0)]
        };
        assert_eq!(app.card_row.shown, after, "{window:?}");
        let texts = painted_texts(&shots_frame(&ctx, &mut app, window).shapes);
        let painted = |t: &str| texts.iter().any(|(x, _)| x == t);
        assert_eq!(painted("DOWN THE LINE"), expect.len() == 3, "{window:?}");
        // A click on the stats card too.
        click_at(&ctx, &mut app, window, rects[0].center());
        assert_ne!(app.card_row.shown[0], Stats, "{window:?}");
    }
    // A camera that disconnects frees its card.
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let window = egui::vec2(1600.0, 900.0);
    let mut app = with_cameras(impact_app());
    for _ in 0..3 {
        shots_frame(&ctx, &mut app, window);
    }
    app.actors.get_mut("camera.0").unwrap().status = crate::types::ActorStatus::Disconnected;
    shots_frame(&ctx, &mut app, window);
    assert_eq!(app.card_row.shown, [Stats, cam(1), Face]);
    // The view None has no bars and no row.
    app.groups.mode = ShotGrouping::None;
    shots_frame(&ctx, &mut app, window);
    let texts = painted_texts(&shots_frame(&ctx, &mut app, window).shapes);
    assert!(
        !texts
            .iter()
            .any(|(t, _)| t == "TOE" || t == "DOWN THE LINE")
    );
    assert_eq!(app.card_row.shown, [Stats, cam(1), Face], "kept for later");
}

#[test]
fn a_camera_card_plays_the_shown_shots_clip_only_when_it_has_one() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let window = egui::vec2(1600.0, 900.0);
    let mut app = with_cameras(cards_app());
    let last = app.shots.len() - 1;
    app.shots[last].video = vec![clip("camera.0")];
    app.shots[3].video = vec![clip("camera.0"), clip("camera.1")];
    app.shots_rev += 1;
    for _ in 0..3 {
        shots_frame(&ctx, &mut app, window);
    }
    // Stats, camera.0, camera.1: only camera.0 has a clip of the shot.
    assert_eq!(app.card_row.shown.len(), 3);
    assert!(app.card_clips.contains_key("camera.0"));
    assert!(
        !app.card_clips.contains_key("camera.1"),
        "no clip, no fetch"
    );
    let seq = app.card_clips["camera.0"].seq();
    assert!(app.card_clips["camera.0"].shows(&app.shots[last].shot_id));
    // A camera card's label is its name; the body stays empty until the
    // frames are in.
    let texts = painted_texts(&shots_frame(&ctx, &mut app, window).shapes);
    assert!(texts.iter().any(|(t, _)| t == "FACE-ON"));
    assert!(texts.iter().any(|(t, _)| t == "DOWN THE LINE"));
    // Another shot shown: the clip goes, and that shot's come in.
    app.select_shot(3);
    shots_frame(&ctx, &mut app, window);
    assert!(app.card_clips["camera.0"].shows(&app.shots[3].shot_id));
    assert_ne!(app.card_clips["camera.0"].seq(), seq);
    assert!(app.card_clips.contains_key("camera.1"));
    // A shot without clips: none.
    app.select_shot(4);
    shots_frame(&ctx, &mut app, window);
    assert!(app.card_clips.is_empty());
    // Clear history drops them too.
    app.select_shot(3);
    shots_frame(&ctx, &mut app, window);
    assert!(!app.card_clips.is_empty());
    app.clear_shot_history();
    assert!(app.card_clips.is_empty());
}

#[test]
fn the_shot_cards_setting_shows_or_drops_the_camera_cards() {
    use card_row::CardSource::{Camera, Stats};
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let window = egui::vec2(1600.0, 900.0);
    let mut app = with_cameras(cards_app());
    app.active_tab = crate::panels::Tab::Shots;
    let last = app.shots.len() - 1;
    app.shots[last].video = vec![clip("camera.0"), clip("camera.1")];
    app.shots_rev += 1;
    let set = |app: &mut FlighthookApp, on: bool| {
        let mut saved = app.settings.to_request();
        saved.replay.shot_cards = on;
        app.settings.load_from(&saved);
        assert_eq!(app.settings.shot_cards_enabled(), on);
    };
    let settle = |app: &mut FlighthookApp| {
        for _ in 0..3 {
            app.drop_hidden_card_clips();
            shots_frame(&ctx, app, window);
        }
    };
    let camera_names = |app: &mut FlighthookApp| {
        let texts = painted_texts(&shots_frame(&ctx, app, window).shapes);
        ["FACE-ON", "DOWN THE LINE"].map(|n| texts.iter().any(|(t, _)| t == n))
    };
    // On, the default: the camera cards as before, each with its clip.
    assert!(app.settings.shot_cards_enabled());
    settle(&mut app);
    let both = [Stats, Camera("camera.0".into()), Camera("camera.1".into())];
    assert_eq!(app.card_row.shown, both);
    assert_eq!(app.card_clips.len(), 2);
    assert!(app.card_clips["camera.0"].shows(&app.shots[last].shot_id));
    assert_eq!(camera_names(&mut app), [true, true]);
    // Off: the stats card alone, no clips held or decoding.
    set(&mut app, false);
    settle(&mut app);
    assert_eq!(app.card_row.shown, [Stats]);
    assert!(app.card_clips.is_empty());
    assert_eq!(camera_names(&mut app), [false, false]);
    assert_eq!(row_cards(&ctx).len(), 1);
    // Another shot with clips picked while off: still none.
    app.shots[3].video = vec![clip("camera.0")];
    app.shots_rev += 1;
    app.select_shot(3);
    settle(&mut app);
    assert!(app.card_clips.is_empty());
    // The clips are still there to replay.
    app.open_replay(&ctx, 3);
    assert!(app.replay.is_some());
    app.replay = None;
    // On again: the cards come back for the shown shot.
    set(&mut app, true);
    settle(&mut app);
    assert_eq!(app.card_row.shown, both);
    assert_eq!(app.card_clips.len(), 1, "only camera.0 has a clip of #3");
    assert!(app.card_clips["camera.0"].shows(&app.shots[3].shot_id));
    assert_eq!(camera_names(&mut app), [true, true]);
}

#[test]
fn the_table_shows_a_replay_column_only_while_a_shot_has_clips() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = cards_app();
    app.shots_view = ShotsView::Table;
    let header = |app: &mut FlighthookApp| {
        marked_frame(&ctx, app, Vec::new());
        let out = marked_frame(&ctx, app, Vec::new()).1;
        painted_texts(&out.shapes)
            .into_iter()
            .any(|(text, _)| text == "Replay")
    };
    assert!(!header(&mut app));
    app.shots[3].video = vec![clip("camera.0")];
    app.shots_rev += 1;
    assert!(header(&mut app));
}

#[test]
fn camera_cards_drop_their_clips_while_the_row_is_hidden() {
    use card_row::CardSource::{Camera, Face, Stats};
    let cam = |n: u8| Camera(format!("camera.{n}"));
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let (big, small) = (egui::vec2(1600.0, 900.0), egui::vec2(1024.0, 768.0));
    let mut app = with_cameras(impact_app());
    app.active_tab = crate::panels::Tab::Shots;
    let last = app.shots.len() - 1;
    app.shots[last].video = vec![clip("camera.0"), clip("camera.1")];
    app.shots_rev += 1;
    let settle = |app: &mut FlighthookApp, window| {
        for _ in 0..3 {
            shots_frame(&ctx, app, window);
        }
    };
    let clips = |app: &FlighthookApp| {
        let mut ids: Vec<String> = app.card_clips.keys().cloned().collect();
        ids.sort();
        ids
    };
    settle(&mut app, big);
    assert_eq!(app.card_row.shown, [Stats, cam(0), Face]);
    assert_eq!(clips(&app), ["camera.0"]);
    // Cycling camera.0's card on to camera.1 drops camera.0's clip.
    let rects = row_cards(&ctx);
    click_at(&ctx, &mut app, big, rects[1].center());
    assert_eq!(app.card_row.shown, [Stats, cam(1), Face]);
    assert_eq!(clips(&app), ["camera.1"]);
    // A smaller window: two cards, face impact kept rightmost, and the
    // camera's clip goes with its card.
    settle(&mut app, small);
    assert_eq!(app.card_row.shown, [Stats, Face]);
    assert_eq!(row_cards(&ctx).len(), 2);
    assert!(app.card_clips.is_empty());
    settle(&mut app, big);
    assert_eq!(app.card_row.shown.len(), 3);
    assert_eq!(clips(&app), ["camera.0"]);
    // The Table view and another tab drop the clips; back on the row,
    // they come again.
    for hide in [
        |app: &mut FlighthookApp| app.shots_view = ShotsView::Table,
        |app: &mut FlighthookApp| app.active_tab = crate::panels::Tab::Devices,
    ] {
        hide(&mut app);
        app.drop_hidden_card_clips();
        assert!(app.card_clips.is_empty());
        app.shots_view = ShotsView::Cards;
        app.active_tab = crate::panels::Tab::Shots;
        app.drop_hidden_card_clips();
        settle(&mut app, big);
        assert_eq!(clips(&app), ["camera.0"]);
    }
    // The view None has no row.
    app.groups.mode = ShotGrouping::None;
    settle(&mut app, big);
    assert!(app.card_clips.is_empty());
    app.groups.mode = ShotGrouping::BySet;
    settle(&mut app, big);
    assert_eq!(clips(&app), ["camera.0"]);
    // The replay of the shown shot covers the row.
    app.open_replay(&ctx, last);
    settle(&mut app, big);
    assert!(app.card_clips.is_empty());
    app.replay = None;
    settle(&mut app, big);
    assert_eq!(clips(&app), ["camera.0"]);
}

#[test]
fn the_navigation_carries_the_device_dropdown_with_two_devices() {
    for (view, grouping) in [
        (ShotsView::Cards, ShotGrouping::BySet),
        (ShotsView::Cards, ShotGrouping::None),
        (ShotsView::Table, ShotGrouping::BySet),
    ] {
        let at = format!("{view:?} {grouping:?}");
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = cards_app();
        app.shots_view = view;
        app.groups.mode = grouping;
        marked_frame(&ctx, &mut app, Vec::new());
        let (marks, out) = marked_frame(&ctx, &mut app, Vec::new());
        let filter = marks.get("device-filter").expect("the filter").0;
        // In the navigation, under the view selector.
        let selector = marks["view_selector"].0;
        assert!(filter.top() >= selector.bottom(), "{at}");
        assert!(filter.right() <= selector.right() + 1.0, "{at}");
        assert!(
            !marks.contains_key("view_toggle") || marks["view_toggle"].0.left() > filter.right()
        );
        // Auto-chip lives in the header, not here.
        assert!(!marks.contains_key("auto-chip"), "{at}");
        // Its DEVICE label is the only one: none over the bars or in the
        // Dispersion header.
        let texts = painted_text_rects(&out.shapes);
        let labels: Vec<egui::Rect> = texts
            .iter()
            .filter(|(t, _, _)| t == "DEVICE")
            .map(|(_, _, r)| *r)
            .collect();
        assert_eq!(labels.len(), 1, "{at}");
        assert!(labels[0].right() <= filter.left() + 0.5, "{at}");
        assert!(
            (labels[0].center().y - filter.center().y).abs() < 3.0,
            "{at}"
        );
        // A compact dropdown: only the choice shows until it opens.
        assert_eq!(texts.iter().filter(|(t, _, _)| t == "All").count(), 1);
        assert!(!marks.contains_key("device-filter:1"), "{at}: closed");
        // Open, then a pick sets the filter.
        let open = click_marked(&ctx, &mut app, &marks, "device-filter");
        assert!(open.contains_key("device-filter:2"), "{at}: open");
        assert!(open["device-filter:1"].0.top() >= filter.bottom() - 0.5);
        click_marked(&ctx, &mut app, &open, "device-filter:1");
        assert_eq!(app.device_filter.as_deref(), Some("mevo.0"), "{at}");
        // One device: no filter, and All again.
        for shot in &mut app.shots {
            shot.actor = "mevo.0".into();
        }
        app.shots[0].actor = "square.0".into();
        app.shots_rev += 1;
        marked_frame(&ctx, &mut app, Vec::new());
        assert!(
            marked_frame(&ctx, &mut app, Vec::new())
                .0
                .contains_key("device-filter")
        );
        app.shots[0].actor = "mevo.0".into();
        app.shots_rev += 1;
        app.device_filter = Some("square.0".into());
        marked_frame(&ctx, &mut app, Vec::new());
        let (marks, out) = marked_frame(&ctx, &mut app, Vec::new());
        assert!(!marks.contains_key("device-filter"), "{at}");
        assert!(
            !painted_texts(&out.shapes)
                .iter()
                .any(|(t, _)| t == "DEVICE")
        );
        assert_eq!(app.device_filter, None, "{at}: its device left");
    }
}

/// The numbers of the shot list's entries as painted.
fn listed(texts: &[(String, bool)]) -> std::collections::BTreeSet<u32> {
    texts
        .iter()
        .filter_map(|(t, _)| t.strip_prefix('#')?.parse().ok())
        .collect()
}

#[test]
fn a_device_filter_applies_to_the_list_the_shown_shot_and_the_table() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = cards_app();
    let frame = |app: &mut FlighthookApp| {
        app.refresh_shot_caches();
        app.observe_shots(1.0);
        marked_frame(&ctx, app, Vec::new());
        painted_texts(&marked_frame(&ctx, app, Vec::new()).1.shapes)
    };
    let count = |texts: &[(String, bool)], t: &str| texts.iter().filter(|(x, _)| x == t).count();
    // The list scrolls: the entries in view, both devices'.
    let texts = frame(&mut app);
    let shown = listed(&texts);
    assert!(shown.iter().any(|n| n % 2 == 0) && shown.iter().any(|n| n % 2 == 1));
    assert!(texts.iter().any(|(t, _)| t == "SHOTS"));
    let i = texts.iter().position(|(t, _)| t == "SHOTS").unwrap();
    assert_eq!(texts[i + 1].0, "12", "the unfiltered total");
    assert!(texts.iter().any(|(t, _)| t == "Shot #11"));
    // The Mevo alone: its shots in the list, under a header counting
    // them, a total of its own; its newest shot on show; the bars over its shots.
    app.device_filter = Some("mevo.0".into());
    let texts = frame(&mut app);
    let shown = listed(&texts);
    assert!(
        shown.len() >= 4 && shown.iter().all(|n| n % 2 == 0),
        "{shown:?}"
    );
    assert_eq!(
        list_items(&app.shots, &app.groups, None, app.filter()),
        std::iter::once(header("a"))
            .chain((0..12).step_by(2).rev().map(ListItem::Shot))
            .collect::<Vec<_>>()
    );
    // The total counts the Mevo's rows alone (6 of the 12).
    let total = |texts: &[(String, bool)]| {
        let i = texts.iter().position(|(t, _)| t == "SHOTS").unwrap();
        texts[i + 1].0.clone()
    };
    assert_eq!(total(&texts), "6", "{texts:?}");
    assert_eq!(shot_total(&app.shots, app.filter()), 6);
    assert_eq!(shot_total(&app.shots, ShotFilter::ALL), 12);
    assert_eq!(
        count(&texts, "SET 1 \u{b7} 7I \u{b7} 6 SHOTS"),
        2,
        "list and bars"
    );
    assert_eq!(app.shown_index(), Some(10));
    assert!(texts.iter().any(|(t, _)| t == "Shot #10"));
    assert!(texts.iter().any(|(t, _)| t == "LATEST"));
    // Device names: the dropdown's choice, and the Mevo's six rows.
    assert_eq!(count(&texts, "square.0"), 0, "{texts:?}");
    assert_eq!(count(&texts, "mevo.0"), 1 + 6 + 1, "dropdown, list, header");
    // The members, Flight and Dispersion: the Mevo's shots, cached on
    // a key that holds the filter.
    let key = app.refresh_members(10);
    assert_eq!(key.device.as_deref(), Some("mevo.0"));
    let members = app.view_members.peek(&key).cloned().unwrap();
    assert_eq!(members, (0..12).step_by(2).collect::<Vec<usize>>());
    // The Table: the Mevo's rows only.
    app.shots_view = ShotsView::Table;
    let texts = frame(&mut app);
    assert_eq!(
        app.filtered_rows(),
        (0..12).step_by(2).collect::<Vec<usize>>()
    );
    assert_eq!(count(&texts, "square.0"), 0, "{texts:?}");
    assert_eq!(
        count(&texts, "mevo.0"),
        1 + 6 + 6,
        "dropdown, list and rows"
    );
    // The Omni: its rows, its members, under keys of their own.
    app.device_filter = Some("square.0".into());
    let texts = frame(&mut app);
    assert_eq!(
        app.filtered_rows(),
        (1..12).step_by(2).collect::<Vec<usize>>()
    );
    assert_eq!(
        count(&texts, "square.0"),
        1 + 6 + 6,
        "dropdown, list and rows"
    );
    let omni = app.refresh_members(11);
    assert_ne!(omni, key);
    assert_eq!(
        app.view_members.peek(&omni).cloned().unwrap(),
        (1..12).step_by(2).collect::<Vec<usize>>()
    );
    // Export stays the whole session: its links carry no filter.
    assert!(!net::export_url("csv", app.units()).contains("square"));
}

#[test]
fn a_filter_picks_its_devices_newest_shot_unless_one_of_its_shots_is_shown() {
    let mut app = cards_app();
    let observe = |app: &mut FlighthookApp, now: f64| {
        app.observe_shots(now);
    };
    observe(&mut app, 0.0);
    // An older Omni shot on show; the Mevo filter moves to the Mevo's
    // newest, without a pulse.
    app.select_shot(5);
    assert_eq!(app.shown_index(), Some(5));
    app.device_filter = Some("mevo.0".into());
    assert_eq!(app.shown_index(), Some(10), "at once");
    observe(&mut app, 1.0);
    assert_eq!(app.shown_index(), Some(10));
    assert!(app.following());
    assert_eq!(app.shot_focus.progress(1.0), None, "no pulse");
    // All again: the Omni shot is not brought back.
    app.device_filter = None;
    observe(&mut app, 2.0);
    assert_eq!(app.shown_index(), Some(11));
    // An Omni shot on show stays under the Omni's filter.
    app.select_shot(5);
    app.device_filter = Some("square.0".into());
    observe(&mut app, 3.0);
    assert_eq!(app.shown_index(), Some(5));
    assert!(!app.following());
}

#[test]
fn a_new_shot_from_another_device_does_not_move_the_shown_shot() {
    let mut app = cards_app();
    app.device_filter = Some("mevo.0".into());
    app.observe_shots(0.0);
    assert_eq!(app.shown_index(), Some(10));
    let shot = |n: u32, actor: &str| ShotRow {
        actor: actor.into(),
        group: Some("a".into()),
        shot_number: n,
        shot_id: format!("id{n}"),
        ..thousand_rows().remove(0)
    };
    // The Omni's shot: no pulse, no move, not listed.
    app.shots.push(shot(12, "square.0"));
    app.shots_rev += 1;
    app.observe_shots(5.0);
    assert_eq!(app.shown_index(), Some(10));
    assert_eq!(app.shot_focus.progress(5.0), None);
    let items = list_items(&app.shots, &app.groups, None, app.filter());
    assert!(!items.contains(&ListItem::Shot(12)));
    // An older Mevo shot picked stays on show through it.
    app.select_shot(4);
    app.shots.push(shot(13, "square.0"));
    app.shots_rev += 1;
    app.observe_shots(6.0);
    assert_eq!(app.shown_index(), Some(4));
    // The Mevo's next shot is the one on show, pulsing.
    app.shots.push(shot(14, "mevo.0"));
    app.shots_rev += 1;
    app.observe_shots(7.0);
    assert_eq!(app.shown_index(), Some(14));
    assert!(app.shot_focus.progress(7.0).is_some());
}

#[test]
fn the_list_under_a_filter_has_only_that_devices_shots_and_sections() {
    // Set a: Mevo and Omni; set b: Omni only; set c: Mevo only.
    let rows = vec![
        device_shot(1, "mevo.0", "a", 150.0, 1.0),
        device_shot(2, "square.0", "a", 151.0, 1.0),
        device_shot(3, "square.0", "b", 230.0, 1.0),
        device_shot(4, "mevo.0", "c", 152.0, 1.0),
        device_shot(5, "square.0", "c", 153.0, 1.0),
    ];
    let list = vec![
        group("a", 1, Club::Iron7, false, 2),
        group("b", 2, Club::Driver, false, 1),
        group("c", 3, Club::Iron7, true, 2),
    ];
    let by_set = groups(ShotGrouping::BySet, list.clone());
    assert_eq!(
        list_items(
            &rows,
            &by_set,
            Some(Club::Iron7),
            ShotFilter::device(Some("mevo.0"))
        ),
        [
            header("c"),
            ListItem::Shot(3),
            header("a"),
            ListItem::Shot(0)
        ]
    );
    assert_eq!(
        list_items(
            &rows,
            &by_set,
            Some(Club::Iron7),
            ShotFilter::device(Some("square.0"))
        ),
        [
            header("c"),
            ListItem::Shot(4),
            header("b"),
            ListItem::Shot(2),
            header("a"),
            ListItem::Shot(1)
        ]
    );
    let by_club = groups(ShotGrouping::ByClub, list.clone());
    assert_eq!(
        list_items(
            &rows,
            &by_club,
            Some(Club::Iron7),
            ShotFilter::device(Some("mevo.0"))
        ),
        [
            club_header(0, Club::Iron7),
            ListItem::Shot(3),
            ListItem::Shot(0)
        ]
    );
    let none = groups(ShotGrouping::None, list.clone());
    assert_eq!(
        list_items(&rows, &none, None, ShotFilter::device(Some("square.0"))),
        [ListItem::Shot(4), ListItem::Shot(2), ListItem::Shot(1)]
    );
    // The headers count the filtered device's shots.
    let a = by_set.get("a").unwrap();
    assert_eq!(chart::set_shot_count(a, &rows, ShotFilter::ALL), 2);
    assert_eq!(
        chart::set_shot_count(a, &rows, ShotFilter::device(Some("mevo.0"))),
        1
    );
}

/// Eric (`flighthook.e`, linked to `pureplay.local-3`) and Sam
/// (`flighthook.s`).
fn two_golfers() -> flighthook::FlighthookConfig {
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.identifiers.push("pureplay.local-3".into());
    flighthook::FlighthookConfig {
        golfers: vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")],
        ..Default::default()
    }
}

/// Set `id` of golfer id `golfer`.
fn golfers_set(id: &str, golfer: &str, number: u32, club: Club, open: bool, n: u32) -> SetInfo {
    SetInfo {
        golfer: Some(golfer.into()),
        ..group(id, number, club, open, n)
    }
}

/// Rows oldest first, each `(set id, golfer id)`, all with data.
fn golfers_rows(rows: &[(&str, &str)]) -> Vec<ShotRow> {
    rows.iter()
        .zip(1..)
        .map(|(&(set, golfer), n)| ShotRow {
            group: Some(set.into()),
            golfer: Some(golfer.into()),
            ..row(n, true)
        })
        .collect()
}

#[test]
fn each_golfer_has_their_open_set_and_the_foot_follows_the_golfer_up() {
    let config = two_golfers();
    let mut g = groups(
        ShotGrouping::BySet,
        vec![
            golfers_set("a", "flighthook.e", 1, Club::Iron7, true, 2),
            golfers_set("b", "flighthook.s", 1, Club::Driver, true, 1),
        ],
    );
    let keys = crate::groups::GolferKeys::of(Some(&config));
    let rev = g.rev();
    g.set_golfers(keys.clone(), Some("flighthook.s"));
    assert_ne!(g.rev(), rev, "a golfer change rebuilds the views");
    let open = |g: &ShotGroups| g.open_set().map(|s| s.id.clone());
    assert_eq!(open(&g).as_deref(), Some("b"));
    assert_eq!(
        g.current_group(Some(Club::Driver)).map(|s| s.id.as_str()),
        Some("b")
    );
    // Sam on Eric's club starts Sam's own new set.
    assert_eq!(g.current_group(Some(Club::Iron7)), None);
    assert_eq!(
        g.strip(Some(Club::Iron7)).hint.as_deref(),
        Some("Next shot starts a new set (7I)")
    );
    assert_eq!(g.strip(Some(Club::Driver)).hint, None);
    // Eric's linked game id is Eric.
    g.set_golfers(keys.clone(), Some("pureplay.local-3"));
    assert_eq!(open(&g).as_deref(), Some("a"));
    assert_eq!(g.strip(Some(Club::Iron7)).hint, None);
    // A golfer with no open set: the next shot starts one.
    g.set_golfers(keys, Some("pureplay.player-7"));
    assert_eq!(open(&g), None);
    let rev = g.rev();
    g.set_golfers(
        crate::groups::GolferKeys::of(Some(&config)),
        Some("pureplay.player-7"),
    );
    assert_eq!(g.rev(), rev, "nothing changed");
}

#[test]
fn by_club_combines_sets_of_one_golfer_club_and_session() {
    let config = two_golfers();
    // Eric and Sam alternate on the 7I; Eric's game id hit set c.
    let shots = golfers_rows(&[
        ("c", "pureplay.local-3"),
        ("a", "flighthook.e"),
        ("b", "flighthook.s"),
        ("a", "flighthook.e"),
        ("b", "flighthook.s"),
    ]);
    let mut g = groups(
        ShotGrouping::ByClub,
        vec![
            golfers_set("c", "pureplay.local-3", 1, Club::Iron7, false, 1),
            golfers_set("a", "flighthook.e", 2, Club::Iron7, true, 2),
            golfers_set("b", "flighthook.s", 1, Club::Iron7, true, 2),
        ],
    );
    g.set_golfers(
        crate::groups::GolferKeys::of(Some(&config)),
        Some("flighthook.e"),
    );
    let header = |golfer: &str| ListItem::ClubHeader {
        golfer: Some(golfer.into()),
        session: 0,
        club: Some(Club::Iron7),
        next: false,
    };
    // The golfer up's section first; Eric's game-id set joins Eric's.
    assert_eq!(
        list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL),
        vec![
            header("flighthook.e"),
            ListItem::Shot(3),
            ListItem::Shot(1),
            ListItem::Shot(0),
            header("flighthook.s"),
            ListItem::Shot(4),
            ListItem::Shot(2),
        ]
    );
    // The bars and charts take the same members as the list.
    let members = |shown: usize| {
        let set = bar_group(&g, Some(&shots[shown]), Some(Club::Iron7));
        chart::members_for(
            &shots,
            &g,
            ShotGrouping::ByClub,
            set,
            Some(shown),
            ShotFilter::ALL,
        )
    };
    assert_eq!(members(3), vec![0, 1, 3]);
    assert_eq!(members(0), vec![0, 1, 3]);
    assert_eq!(members(4), vec![2, 4]);
    let set = bar_group(&g, Some(&shots[3]), Some(Club::Iron7));
    let title = bars_title(ShotGrouping::ByClub, set, 0, &shots, &members(3), false);
    assert_eq!(
        title.map(|t| t.0).as_deref(),
        Some("7 Iron \u{b7} 2 sets \u{b7} 3 shots")
    );
    // Sam up: Sam's section first.
    g.set_golfers(
        crate::groups::GolferKeys::of(Some(&config)),
        Some("flighthook.s"),
    );
    assert_eq!(
        list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL)[0],
        header("flighthook.s")
    );
    // By set keeps one header per set.
    g.mode = ShotGrouping::BySet;
    let items = list_items(&shots, &g, Some(Club::Iron7), ShotFilter::ALL);
    assert_eq!(
        items
            .iter()
            .filter(|i| !matches!(i, ListItem::Shot(_)))
            .count(),
        5
    );
}

/// By club: a section moves up by its own golfer's activity only.
/// Eric picking the 7I does not lift Sam's 7 Iron section above Sam's
/// newer Driver section.
#[test]
fn another_golfers_club_pick_does_not_reorder_a_section() {
    let keys = crate::groups::GolferKeys::of(Some(&two_golfers()));
    // Sam: 7I ×2 (b), DR ×2 (d); then Eric up on the 7I, no shot yet.
    let shots = golfers_rows(&[
        ("b", "flighthook.s"),
        ("b", "flighthook.s"),
        ("d", "flighthook.s"),
        ("d", "flighthook.s"),
    ]);
    let mut g = groups(
        ShotGrouping::ByClub,
        vec![
            golfers_set("b", "flighthook.s", 1, Club::Iron7, false, 2),
            golfers_set("d", "flighthook.s", 2, Club::Driver, true, 2),
        ],
    );
    g.set_golfers(keys.clone(), Some("flighthook.s"));
    g.observe_club(Some(Club::Iron7), upto(&shots, 0));
    g.observe_club(Some(Club::Driver), upto(&shots, 2));
    g.set_golfers(keys.clone(), Some("flighthook.e"));
    g.observe_club(Some(Club::Iron7), upto(&shots, 4));
    let order = |g: &ShotGroups| {
        section_ids(&list_items(&shots, g, Some(Club::Iron7), ShotFilter::ALL))
            .iter()
            .map(|(_, c)| c.map_or("-".to_owned(), |c| c.to_string()))
            .collect::<Vec<_>>()
            .join(",")
    };
    assert_eq!(order(&g), "DR,7I");
    // Sam's own pick of the 7I does lift it.
    g.set_golfers(keys.clone(), Some("flighthook.s"));
    g.observe_club(Some(Club::Iron7), upto(&shots, 4));
    g.set_golfers(keys, Some("flighthook.e"));
    g.observe_club(Some(Club::Iron7), upto(&shots, 4));
    assert_eq!(order(&g), "7I,DR");
}

/// Filter on Eric while Sam is up and has asked for a new set: Sam's
/// empty "next" header and the foot hint about Sam's club stay out of
/// Eric's list; under Sam's filter or All they show.
#[test]
fn another_golfers_filter_hides_the_next_header_and_hint() {
    let keys = crate::groups::GolferKeys::of(Some(&two_golfers()));
    let shots = golfers_rows(&[("a", "flighthook.e"), ("b", "flighthook.s")]);
    for mode in [ShotGrouping::BySet, ShotGrouping::ByClub] {
        let mut g = groups(
            mode,
            vec![
                golfers_set("a", "flighthook.e", 1, Club::Iron7, true, 1),
                golfers_set("b", "flighthook.s", 1, Club::Iron7, false, 1),
                golfers_set("c", "flighthook.s", 2, Club::Driver, true, 0),
            ],
        );
        g.set_golfers(keys.clone(), Some("flighthook.s"));
        let next = |g: &ShotGroups| {
            list_items(&shots, g, Some(Club::Driver), ShotFilter::ALL)
                .iter()
                .any(|i| {
                    matches!(
                        i,
                        ListItem::GroupHeader { next: true, .. }
                            | ListItem::ClubHeader { next: true, .. }
                    )
                })
        };
        // Viewing Sam, the golfer up: Sam's empty next set.
        assert!(next(&g), "{mode:?}");
        assert!(shown_strip(&g, None, false).hint.is_some());
        // Viewing Eric while Sam is up: Eric's open set has a shot, so
        // no next set, and the hint (about the golfer up) is left out.
        g.set_view(Some(Some("flighthook.e".into())));
        assert!(!next(&g), "{mode:?}");
        assert_eq!(shown_strip(&g, None, false).hint, None);
        assert!(shown_strip(&g, None, false).actions);
    }
}

#[test]
fn the_view_passes_a_golfers_shots_by_the_lookup_rule() {
    let keys = crate::groups::GolferKeys::of(Some(&two_golfers()));
    let mut shots = golfers_rows(&[
        ("a", "flighthook.e"),
        ("a", "pureplay.local-3"),
        ("b", "flighthook.s"),
        ("b", "pureplay.player-7"),
    ]);
    shots.push(ShotRow {
        golfer: None,
        ..row(5, true)
    });
    let passed = |filter: ShotFilter<'_>| -> Vec<usize> {
        (0..shots.len())
            .filter(|&i| filter.passes(&shots[i]))
            .collect()
    };
    assert_eq!(passed(ShotFilter::ALL), vec![0, 1, 2, 3, 4]);
    let eric = ShotFilter::ALL.golfer(Some(Some("flighthook.e")), &keys);
    assert_eq!(passed(eric), vec![0, 1]);
    // An id no golfer lists stands on its own; no golfer is a key too.
    assert_eq!(
        passed(ShotFilter::ALL.golfer(Some(Some("pureplay.player-7")), &keys)),
        vec![3]
    );
    assert_eq!(passed(ShotFilter::ALL.golfer(Some(None), &keys)), vec![4]);
    // With the device filter, both apply.
    let mevo = ShotFilter::device(Some("mevo.0")).golfer(Some(Some("flighthook.e")), &keys);
    assert_eq!(
        passed(mevo),
        vec![0, 1]
            .into_iter()
            .filter(|&i| shots[i].actor == "mevo.0")
            .collect::<Vec<_>>()
    );
    // Only the device filter narrows; the golfer is the view itself.
    assert!(mevo.narrows() && !eric.narrows() && !ShotFilter::ALL.narrows());
    // A set's count under a golfer view: its own count for its
    // golfer, else the rows of it that golfer hit.
    let a = golfers_set("a", "flighthook.e", 1, Club::Iron7, true, 2);
    assert_eq!(chart::set_shot_count(&a, &shots, eric), 2);
    let sam = ShotFilter::ALL.golfer(Some(Some("flighthook.s")), &keys);
    assert_eq!(chart::set_shot_count(&a, &shots, sam), 0);
}

/// `app` with Eric and Sam saved and Eric up: even rows are Eric's
/// (Set 1 then Set 2, three shots each), odd rows Sam's (Set 3, six).
fn golfers_cards_app() -> FlighthookApp {
    let mut app = cards_app();
    app.settings.load_from(&two_golfers());
    for (i, shot) in app.shots.iter_mut().enumerate() {
        let (golfer, set) = match (i % 2, i < 6) {
            (0, true) => ("flighthook.e", "a"),
            (0, false) => ("flighthook.e", "c"),
            _ => ("flighthook.s", "b"),
        };
        shot.golfer = Some(golfer.into());
        shot.group = Some(set.into());
    }
    app.groups = groups(
        ShotGrouping::BySet,
        vec![
            golfers_set("a", "flighthook.e", 1, Club::Iron7, false, 3),
            golfers_set("c", "flighthook.e", 2, Club::Iron7, true, 3),
            golfers_set("b", "flighthook.s", 3, Club::Iron7, true, 6),
        ],
    );
    app.turn_golfer = Some("flighthook.e".into());
    app.shots_rev += 1;
    app
}

/// Every view of the Shots tab shows only `golfer`'s shots: the list,
/// the Table rows, the shot on show (their latest) and the members the
/// bars, stats, face and charts take.
fn assert_viewing(app: &mut FlighthookApp, golfer: &str) {
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some(golfer));
    let of = |app: &FlighthookApp, i: usize| app.shots[i].golfer.clone();
    let rows = app.filtered_rows().to_vec();
    assert_eq!(rows.len(), 6, "{golfer}: the Table");
    assert!(rows.iter().all(|&i| of(app, i).as_deref() == Some(golfer)));
    let shown = app.shown_index().expect("a shot on show");
    assert_eq!(Some(shown), rows.last().copied(), "{golfer}: their latest");
    let key = app.refresh_members(shown);
    let members = app.view_members.peek(&key).cloned().unwrap_or_default();
    assert!(!members.is_empty(), "{golfer}: members");
    assert!(
        members
            .iter()
            .all(|&i| of(app, i).as_deref() == Some(golfer)),
        "{golfer}: members {members:?}"
    );
    let filter = app.filter();
    for item in list_items(&app.shots, &app.groups, app.selected_club, filter) {
        if let ListItem::Shot(i) = item {
            assert_eq!(of(app, i).as_deref(), Some(golfer), "{golfer}: the list");
        }
    }
}

/// The golfer a sent `golfer.select` names.
fn selected_golfer(command: &flighthook::UiCommand) -> String {
    let text = serde_json::to_string(command).expect("json");
    assert!(text.contains("golfer.select"), "{text}");
    let at = text.find("\"golfer\":\"").expect("a golfer") + 10;
    text[at..].split('"').next().unwrap_or_default().to_owned()
}

/// Follow turns on: a game's turn and a Golfer card pick (which sends
/// `golfer.select { golfer }` and becomes the turn) switch every view.
#[test]
fn follow_on_switches_every_view_with_the_turn() {
    let mut app = golfers_cards_app();
    assert_viewing(&mut app, "flighthook.e");
    // A game's turn names Sam.
    app.turn_golfer = Some("flighthook.s".into());
    assert_viewing(&mut app, "flighthook.s");
    // A card pick of Eric sends the select; the view follows the turn.
    app.pick_golfer("flighthook.e");
    assert_eq!(app.sent_selects.len(), 1);
    assert_eq!(selected_golfer(&app.sent_selects[0]), "flighthook.e");
    app.turn_golfer = Some("flighthook.e".into());
    assert_viewing(&mut app, "flighthook.e");
}

/// The selection moves to the newly viewed golfer's latest shot, also
/// off a pinned shot; a golfer with no shots shows none.
#[test]
fn the_selection_moves_on_a_view_change() {
    let mut app = golfers_cards_app();
    app.refresh_shot_caches();
    app.select_shot(2);
    assert_eq!(app.shown_index(), Some(2), "pinned");
    app.turn_golfer = Some("flighthook.s".into());
    app.refresh_shot_caches();
    assert_eq!(app.shown_index(), Some(11), "Sam's latest");
    app.turn_golfer = Some("flighthook.e".into());
    app.refresh_shot_caches();
    assert_eq!(
        app.shown_index(),
        Some(10),
        "Eric's latest, not the old pin"
    );
    let mut config = two_golfers();
    config
        .golfers
        .push(flighthook::GolferRecord::new("flighthook.j", "Jordan"));
    app.settings.load_from(&config);
    app.turn_golfer = Some("flighthook.j".into());
    app.refresh_shot_caches();
    assert_eq!(app.shown_index(), None, "Jordan has no shots");
}

/// Follow turns off: a game's turn leaves the view, a card pick sends
/// nothing and moves only the view, turning it on snaps the view to the
/// golfer up.
#[test]
fn follow_off_keeps_the_view_and_a_pick_moves_only_the_view() {
    let mut app = golfers_cards_app();
    app.refresh_shot_caches();
    app.follow_turns = false;
    app.select_shot(2);
    app.turn_golfer = Some("flighthook.s".into());
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.e"));
    assert_eq!(app.shown_index(), Some(2), "the pin stays");
    assert!(app.viewing_other());
    // Picks: the view moves, nothing is sent, the turn stays.
    app.pick_golfer("flighthook.s");
    assert_viewing(&mut app, "flighthook.s");
    assert!(!app.viewing_other());
    app.pick_golfer("flighthook.e");
    assert_viewing(&mut app, "flighthook.e");
    assert!(app.sent_selects.is_empty());
    assert_eq!(app.turn_golfer.as_deref(), Some("flighthook.s"));
    // Turning it on snaps to the golfer up.
    app.follow_turns = true;
    app.follow_view();
    assert_viewing(&mut app, "flighthook.s");
}

/// Follow turns off and the viewed golfer's profile goes away: the view
/// falls back to the golfer up.
#[test]
fn a_removed_viewed_profile_falls_back_to_the_golfer_up() {
    let mut app = golfers_cards_app();
    let mut config = two_golfers();
    config
        .golfers
        .push(flighthook::GolferRecord::new("flighthook.j", "Jordan"));
    app.settings.load_from(&config);
    app.follow_turns = false;
    app.refresh_shot_caches();
    app.pick_golfer("flighthook.j");
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.j"));
    app.settings.load_from(&two_golfers());
    app.refresh_shot_caches();
    assert!(!app.follow_turns, "Follow stays off");
    assert_viewing(&mut app, "flighthook.e");
}

/// A game golfer no saved golfer lists is viewed by its own id; a
/// linked game id counts under its saved golfer.
#[test]
fn a_game_named_unlinked_golfer_is_viewed_by_its_id() {
    let mut app = golfers_cards_app();
    app.shots[0].golfer = Some("pureplay.player-7".into());
    app.shots[2].golfer = Some("pureplay.local-3".into());
    app.shots_rev += 1;
    app.turn_golfer = Some("pureplay.player-7".into());
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some("pureplay.player-7"));
    assert_eq!(app.filtered_rows().to_vec(), vec![0]);
    app.turn_golfer = Some("pureplay.local-3".into());
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.e"));
    assert_eq!(app.filtered_rows().to_vec(), vec![2, 4, 6, 8, 10]);
}

/// No golfer saved and none up: every shot shows, as with one person.
#[test]
fn the_no_golfer_setup_shows_every_shot() {
    let mut app = cards_app();
    app.refresh_shot_caches();
    assert!(app.follows_turns());
    assert_eq!(app.viewed_golfer, None);
    assert_eq!(app.filtered_rows().len(), 12);
    assert_eq!(app.shown_index(), Some(11));
    assert!(!app.filter().narrows());
}

/// The toolbar has no golfer filter, and headers carry no golfer's
/// name: By set shows one header per Set of the viewed golfer, its
/// count matching the rows under it.
#[test]
fn by_set_shows_one_plain_header_per_set_of_the_viewed_golfer() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = golfers_cards_app();
    let texts = |app: &mut FlighthookApp| -> (devices::Marks, Vec<String>) {
        marked_frame(&ctx, app, Vec::new());
        let (marks, out) = marked_frame(&ctx, app, Vec::new());
        let texts = painted_texts(&out.shapes)
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        (marks, texts)
    };
    // The list's headers and the rows under each, for the viewed golfer.
    let runs = |app: &mut FlighthookApp| -> Vec<(String, usize)> {
        app.refresh_shot_caches();
        let filter = app.filter();
        let mut runs: Vec<(String, usize)> = Vec::new();
        for item in list_items(&app.shots, &app.groups, app.selected_club, filter) {
            match item {
                ListItem::GroupHeader { id, .. } => runs.push((id, 0)),
                ListItem::Shot(_) => runs.last_mut().expect("under a header").1 += 1,
                _ => {}
            }
        }
        runs
    };
    let (marks, all) = texts(&mut app);
    assert!(!marks.contains_key("golfer-filter"));
    assert!(!all.iter().any(|t| t == "GOLFER"), "{all:?}");
    let mut eric = runs(&mut app);
    eric.sort();
    assert_eq!(eric, vec![("a".to_owned(), 3), ("c".to_owned(), 3)]);
    for (id, rows) in &eric {
        let set = app.groups.get(id).expect("the set");
        let count = chart::set_shot_count(set, &app.shots, app.filter());
        assert_eq!(count as usize, *rows, "{id}: the header's count");
    }
    let headers: Vec<&String> = all.iter().filter(|t| t.contains("SET ")).collect();
    assert!(
        headers
            .iter()
            .any(|t| t.starts_with("SET 1 \u{b7} 7I \u{b7} 3 SHOTS")),
        "{headers:?}"
    );
    assert!(
        !headers.iter().any(|t| t.starts_with("SET 3")),
        "{headers:?}"
    );
    assert!(!all.iter().any(|t| t.contains("ERIC \u{b7}")), "{all:?}");
    // Sam's view: one header, Set 3 with six shots.
    app.turn_golfer = Some("flighthook.s".into());
    assert_eq!(runs(&mut app), vec![("b".to_owned(), 6)]);
    let (_, all) = texts(&mut app);
    assert!(
        all.iter()
            .any(|t| t.starts_with("SET 3 \u{b7} 7I \u{b7} 6 SHOTS")),
        "{all:?}"
    );
    // By club: plain headers too.
    app.groups.mode = ShotGrouping::ByClub;
    let (_, all) = texts(&mut app);
    assert!(all.iter().any(|t| t.starts_with("7 IRON")), "{all:?}");
    assert!(!all.iter().any(|t| t.contains("SAM \u{b7}")), "{all:?}");
}

/// New set rolls the viewed golfer's set: for the golfer up the command
/// names nobody (the server's default), for someone else it names them.
#[test]
fn new_set_acts_on_the_viewed_golfer() {
    let mut app = golfers_cards_app();
    app.refresh_shot_caches();
    assert!(!app.viewing_other());
    assert_eq!(app.new_set_golfer(), None);
    app.follow_turns = false;
    app.turn_golfer = Some("flighthook.s".into());
    app.refresh_shot_caches();
    assert!(app.viewing_other());
    let text =
        serde_json::to_string(&net::new_block_command(app.new_set_golfer(), false)).expect("json");
    assert!(text.contains("\"golfer\":\"flighthook.e\""), "{text}");
    // The list's open set is the viewed golfer's.
    assert_eq!(app.groups.open_set().map(|g| g.id.as_str()), Some("c"));
    let text = serde_json::to_string(&net::new_block_command(None, false)).expect("json");
    assert!(!text.contains("\"golfer\""), "{text}");
}
/// The Golfer card's views (every known golfer and every view-only
/// row) reach every shot exactly once, so Clear history's count is what
/// the views can show.
fn assert_views_reach_every_shot(app: &FlighthookApp) {
    let keys = app.groups.golfers();
    let mut views: Vec<Option<&str>> = app
        .golfer_list
        .known
        .iter()
        .map(|g| Some(g.id.as_str()))
        .collect();
    views.extend(app.golfer_list.past.iter().map(|p| p.key.as_deref()));
    let reached: usize = views
        .iter()
        .map(|&key| {
            let filter = ShotFilter::ALL.golfer(Some(key), keys);
            app.shots.iter().filter(|r| filter.passes(r)).count()
        })
        .sum();
    assert_eq!(reached, app.shots.len(), "views {views:?}");
}

/// A Pureplay guest's shots stay viewable from the Golfer card after
/// the game drops the guest's profile: a view-only row named by the
/// newest shot's name. Picking it sends nothing, with Follow off or on;
/// with Follow on the next turn change moves the view to the golfer up.
#[test]
fn a_departed_guests_shots_stay_viewable_from_the_golfer_card() {
    let guest = "pureplay.player-7";
    let mut app = golfers_cards_app();
    app.shots[0].golfer = Some(guest.into());
    app.shots[2].golfer = Some(guest.into());
    app.golfer_names.insert(guest.into(), "Pat Guest".into());
    app.shots_rev += 1;
    // Pat is up and viewed with Follow off.
    app.follow_turns = false;
    app.turn_golfer = Some(guest.into());
    app.refresh_shot_caches();
    app.pick_golfer(guest);
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some(guest));
    // The round ends: Eric is up, Pat is no longer a known golfer.
    app.turn_golfer = Some("flighthook.e".into());
    app.refresh_shot_caches();
    assert!(!app.known_golfers().iter().any(|g| g.id == guest));
    assert_eq!(
        app.golfer_list.past,
        vec![crate::topbar::PastGolfer {
            key: Some(guest.into()),
            name: "Pat Guest".into(),
        }]
    );
    assert_eq!(app.viewed_golfer.as_deref(), Some(guest), "the view stays");
    assert_eq!(app.filtered_rows().to_vec(), vec![0, 2]);
    assert!(app.viewing_other());
    assert_views_reach_every_shot(&app);
    // Off Pat and back through the view-only row: nothing is sent.
    app.pick_golfer("flighthook.s");
    app.refresh_shot_caches();
    app.pick_view_only(Some(guest.into()));
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some(guest));
    // Follow on: the pick holds until the turn changes, then the view
    // is the golfer up again.
    app.follow_turns = true;
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.e"));
    app.pick_view_only(Some(guest.into()));
    app.refresh_shot_caches();
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some(guest), "held");
    assert_eq!(app.filtered_rows().to_vec(), vec![0, 2]);
    app.turn_golfer = Some("flighthook.s".into());
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.s"));
    assert!(app.sent_selects.is_empty(), "{:?}", app.sent_selects);
}

/// The first golfer saved mid-run: the shots from before show under
/// "No golfer", which only changes the view and sends nothing.
#[test]
fn the_first_golfer_saved_mid_run_shows_the_earlier_shots_under_no_golfer() {
    let mut app = cards_app();
    app.refresh_shot_caches();
    assert!(app.golfer_list.past.is_empty(), "no golfer: no extra row");
    app.settings.load_from(&flighthook::FlighthookConfig {
        golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
        ..Default::default()
    });
    app.turn_golfer = Some("flighthook.e".into());
    app.shots[11].golfer = Some("flighthook.e".into());
    app.shots_rev += 1;
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.e"));
    assert_eq!(app.filtered_rows().to_vec(), vec![11]);
    assert_eq!(
        app.golfer_list.past,
        vec![crate::topbar::PastGolfer {
            key: None,
            name: crate::topbar::NO_GOLFER.into(),
        }]
    );
    assert_views_reach_every_shot(&app);
    // One known golfer: Follow is hidden and counts as on.
    assert!(app.follows_turns());
    app.pick_view_only(None);
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer, None);
    assert_eq!(app.filtered_rows().len(), 11, "the earlier shots");
    assert!(app.viewing_other());
    assert!(app.sent_selects.is_empty());
    // New set has no golfer to act on: it is off, not the golfer up's.
    assert!(app.viewing_view_only());
    assert!(!shown_strip(&app.groups, Some(Club::Iron7), app.viewing_view_only()).new_group);
}

/// An event of the golfer up, named, for the view-only pick tests.
type ViewEvent = (&'static str, fn(&mut FlighthookApp));

/// A turn event from flighthook naming `golfer`, with a Driver.
fn feed_turn(app: &mut FlighthookApp, golfer: &str) {
    let from = app.flighthook_id.clone();
    let turn = flighthook::GolferTurn::new(
        Some(golfer.parse().expect("golfer id")),
        Some(Club::Driver),
        flighthook::Handedness::Right,
        flighthook::Mode::Full,
    );
    crate::socket::feed::frp(app, &from, turn);
}

/// A new stored shot `id` by `golfer`.
fn feed_shot(app: &mut FlighthookApp, id: &str, golfer: &str) {
    crate::socket::feed::shot(
        app,
        flighthook::ShotData {
            card: Some("mevo.0".into()),
            shot_id: Some(id.into()),
            shot_number: 99,
            turn: Some(flighthook::GolferTurn::new(
                Some(golfer.parse().expect("golfer id")),
                Some(Club::Driver),
                flighthook::Handedness::Right,
                flighthook::Mode::Full,
            )),
            ..Default::default()
        },
    );
}

/// One known golfer, "No golfer" picked: the golfer up's next shot, a
/// click on their own row, or a turn event naming them brings the view
/// back to them.
#[test]
fn a_view_only_pick_ends_at_the_next_event_of_the_only_golfer() {
    let mut app = cards_app();
    app.settings.load_from(&flighthook::FlighthookConfig {
        golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
        ..Default::default()
    });
    app.turn_golfer = Some("flighthook.e".into());
    app.shots[11].golfer = Some("flighthook.e".into());
    app.shots_rev += 1;
    app.refresh_shot_caches();
    let events: [ViewEvent; 3] = [
        ("new shot", |app| feed_shot(app, "new", "flighthook.e")),
        ("own row clicked", |app| app.pick_golfer("flighthook.e")),
        ("turn event", |app| feed_turn(app, "flighthook.e")),
    ];
    for (name, event) in events {
        app.pick_view_only(None);
        app.refresh_shot_caches();
        assert_eq!(app.viewed_golfer, None, "{name}: no golfer viewed");
        event(&mut app);
        app.refresh_shot_caches();
        assert_eq!(
            app.viewed_golfer.as_deref(),
            Some("flighthook.e"),
            "{name}: back on the golfer up"
        );
    }
}

/// Two known golfers and a departed guest: with Follow on a view-only
/// pick ends at the next shot, turn event or click on the golfer up's row;
/// with Follow off it stays.
#[test]
fn a_view_only_pick_ends_at_the_next_event_only_while_following() {
    let guest = "pureplay.player-7";
    let mut app = golfers_cards_app();
    app.shots[0].golfer = Some(guest.into());
    app.golfer_names.insert(guest.into(), "Pat Guest".into());
    app.turn_golfer = Some("flighthook.e".into());
    app.shots_rev += 1;
    app.refresh_shot_caches();
    let events: [ViewEvent; 3] = [
        ("new shot", |app| feed_shot(app, "new", "flighthook.e")),
        ("turn event", |app| feed_turn(app, "flighthook.e")),
        ("own row clicked", |app| app.pick_golfer("flighthook.e")),
    ];
    for follow in [true, false] {
        for (name, event) in events {
            app.follow_turns = follow;
            app.refresh_shot_caches();
            app.pick_view_only(Some(guest.into()));
            app.refresh_shot_caches();
            assert_eq!(app.viewed_golfer.as_deref(), Some(guest), "{name}");
            event(&mut app);
            app.refresh_shot_caches();
            let want = if follow || name == "own row clicked" {
                "flighthook.e"
            } else {
                guest
            };
            assert_eq!(
                app.viewed_golfer.as_deref(),
                Some(want),
                "{name}, follow {follow}"
            );
        }
    }
}

/// New set's hover while off says why: the golfer up while a view-only
/// row is viewed, else the empty open set.
#[test]
fn new_set_off_says_why() {
    assert_eq!(
        new_group_off_hint(true),
        "New set applies to the golfer up."
    );
    assert_eq!(new_group_off_hint(false), "The open set has no shots yet.");
}

/// Clear history asks about every golfer once the shots belong to more
/// than one golfer, "No golfer" included, even with one known golfer.
#[test]
fn clear_history_covers_every_golfer_with_shots() {
    let mut app = cards_app();
    app.refresh_shot_caches();
    assert!(!app.clear_covers_golfers(), "every shot with no golfer");
    app.settings.load_from(&flighthook::FlighthookConfig {
        golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
        ..Default::default()
    });
    app.refresh_shot_caches();
    assert!(
        !app.clear_covers_golfers(),
        "one known golfer, no shots of theirs"
    );
    app.shots[11].golfer = Some("flighthook.e".into());
    app.shots_rev += 1;
    app.refresh_shot_caches();
    assert!(
        app.clear_covers_golfers(),
        "Eric's shot and the earlier ones"
    );
}

/// Deleted saved golfers whose names are unknown read "Unknown golfer 1",
/// "Unknown golfer 2", and keep their numbers for the session.
#[test]
fn unknown_golfers_are_numbered_for_the_session() {
    let a = "flighthook.00000000-0000-0000-0000-00000000000a";
    let b = "flighthook.00000000-0000-0000-0000-00000000000b";
    let mut app = cards_app();
    app.settings.load_from(&flighthook::FlighthookConfig {
        golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
        ..Default::default()
    });
    app.turn_golfer = Some("flighthook.e".into());
    app.shots[10].golfer = Some(a.into());
    app.shots[11].golfer = Some(b.into());
    app.shots_rev += 1;
    app.refresh_shot_caches();
    let names = |app: &FlighthookApp| -> Vec<(Option<String>, String)> {
        let mut v: Vec<_> = app
            .golfer_list
            .past
            .iter()
            .map(|p| (p.key.clone(), p.name.clone()))
            .collect();
        v.sort();
        v
    };
    let want = vec![
        (None, crate::topbar::NO_GOLFER.to_owned()),
        (Some(a.to_owned()), "Unknown golfer 2".to_owned()),
        (Some(b.to_owned()), "Unknown golfer 1".to_owned()),
    ];
    assert_eq!(names(&app), want);
    // A newer shot of the first one keeps every number.
    app.shots[9].golfer = Some(b.into());
    app.shots[11].golfer = Some(a.into());
    app.shots_rev += 1;
    app.refresh_shot_caches();
    assert_eq!(names(&app), want);
    app.pick_view_only(Some(a.into()));
    app.refresh_shot_caches();
    assert_eq!(app.viewed_name(), "Unknown golfer 2");
}

/// Follow's "two or more golfers" rule counts known golfers only, not
/// view-only rows.
#[test]
fn follow_counts_only_known_golfers() {
    let mut app = cards_app();
    app.settings.load_from(&flighthook::FlighthookConfig {
        golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
        ..Default::default()
    });
    app.turn_golfer = Some("flighthook.e".into());
    app.shots[0].golfer = Some("pureplay.player-7".into());
    app.shots[11].golfer = Some("flighthook.e".into());
    app.shots_rev += 1;
    app.follow_turns = false;
    app.refresh_shot_caches();
    assert_eq!(app.golfer_list.past.len(), 2, "{:?}", app.golfer_list.past);
    assert_eq!(app.known_golfers().len(), 1);
    assert!(app.follows_turns(), "hidden with one known golfer");
    assert_views_reach_every_shot(&app);
}

/// By club with the view off the golfer up: the golfer up's club
/// change leaves the viewed golfer's list as it was.
#[test]
fn by_club_ignores_the_golfer_ups_club_while_viewing_someone_else() {
    let mut app = golfers_cards_app();
    // Eric's latest set is a driver set; his earlier one 7 irons.
    app.groups = groups(
        ShotGrouping::ByClub,
        vec![
            golfers_set("a", "flighthook.e", 1, Club::Iron7, false, 3),
            golfers_set("c", "flighthook.e", 2, Club::Driver, true, 3),
            golfers_set("b", "flighthook.s", 3, Club::Iron7, true, 6),
        ],
    );
    app.follow_turns = false;
    app.selected_club = Some(Club::Driver);
    app.refresh_shot_caches();
    app.turn_golfer = Some("flighthook.s".into());
    app.refresh_shot_caches();
    assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.e"));
    let headers = |app: &FlighthookApp| -> Vec<Option<Club>> {
        list_items(&app.shots, &app.groups, app.selected_club, app.filter())
            .into_iter()
            .filter_map(|item| match item {
                ListItem::ClubHeader { club, .. } => Some(club),
                _ => None,
            })
            .collect()
    };
    let before = headers(&app);
    assert_eq!(before, vec![Some(Club::Driver), Some(Club::Iron7)]);
    // Sam, who is up, picks 7I.
    app.selected_club = Some(Club::Iron7);
    app.refresh_shot_caches();
    assert_eq!(headers(&app), before);
}

/// New set over the socket for a viewed golfer who is not up: the reply's
/// empty set shows as the gold "next" header for that golfer.
#[test]
fn new_set_over_the_socket_shows_the_viewed_golfers_next_set() {
    let ctx = egui::Context::default();
    let mut app = golfers_cards_app();
    app.refresh_shot_caches();
    app.follow_turns = false;
    app.turn_golfer = Some("flighthook.s".into());
    app.refresh_shot_caches();
    assert!(app.viewing_other());
    // A sender to a port nothing listens on: the command goes nowhere.
    let (tx, _rx) =
        ewebsock::connect("ws://127.0.0.1:9/", ewebsock::Options::default()).expect("a sender");
    app.ws_sender = Some(tx);
    app.send_group_command(&ctx, GroupCommand::NewGroup);
    app.ws_sender = None;
    // The practice actor's reply: Eric's set c closed, an empty set d.
    let block = |set: SetInfo| flighthook::BlockInfo {
        block: flighthook::PracticeBlock {
            id: set.id.clone(),
            golfer: set.golfer.as_deref().map(|g| g.parse().expect("id")),
            club: set.club,
            number: set.number,
            session: set.session,
            started_at: 1_000,
        },
        open: set.open,
        shots: set.shots,
        first_shot_at: None,
    };
    app.groups.replace(flighthook::BlocksResponse {
        session: 1,
        blocks: vec![
            block(golfers_set("a", "flighthook.e", 1, Club::Iron7, false, 3)),
            block(golfers_set("c", "flighthook.e", 2, Club::Iron7, false, 3)),
            block(golfers_set("b", "flighthook.s", 3, Club::Iron7, true, 6)),
            block(golfers_set("d", "flighthook.e", 4, Club::Iron7, true, 0)),
        ],
    });
    app.refresh_shot_caches();
    let items = list_items(&app.shots, &app.groups, app.selected_club, app.filter());
    assert_eq!(
        items.first(),
        Some(&ListItem::GroupHeader {
            id: "d".into(),
            next: true,
        }),
        "{items:?}"
    );
}

/// Follow on: a `golfer.turn` through the socket handler moves every
/// view.
#[test]
fn a_golfer_turn_over_the_socket_moves_the_view() {
    let mut app = golfers_cards_app();
    app.flighthook_id = "fh".into();
    assert_viewing(&mut app, "flighthook.e");
    let turn = flighthook::GolferTurn::new(
        Some("flighthook.s".parse().expect("id")),
        Some(Club::Iron7),
        flighthook::Handedness::Right,
        flighthook::Mode::Full,
    );
    app.handle_ui_message(flighthook::UiMessage::Frp(Box::new(
        flighthook::BusMessage::new("fh", turn),
    )));
    assert_eq!(app.turn_golfer.as_deref(), Some("flighthook.s"));
    assert_viewing(&mut app, "flighthook.s");
}

/// A viewed golfer with no shots while someone else is up: the empty
/// state names them and does not wait for "the first shot".
#[test]
fn the_empty_state_names_a_viewed_golfer_with_no_shots() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = golfers_cards_app();
    let mut config = two_golfers();
    config
        .golfers
        .push(flighthook::GolferRecord::new("flighthook.j", "Jordan"));
    app.settings.load_from(&config);
    app.follow_turns = false;
    app.turn_golfer = Some("flighthook.s".into());
    app.refresh_shot_caches();
    app.pick_golfer("flighthook.j");
    let texts = |app: &mut FlighthookApp| -> Vec<String> {
        marked_frame(&ctx, app, Vec::new());
        let (_, out) = marked_frame(&ctx, app, Vec::new());
        painted_texts(&out.shapes)
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    };
    let all = texts(&mut app);
    assert!(
        all.iter().any(|t| t == "Jordan has no shots yet."),
        "{all:?}"
    );
    assert!(!all.iter().any(|t| t == WAITING_FOR_FIRST_SHOT), "{all:?}");
    // Jordan up, with no shots: the text for the golfer up.
    app.turn_golfer = Some("flighthook.j".into());
    let all = texts(&mut app);
    assert!(all.iter().any(|t| t == WAITING_FOR_FIRST_SHOT), "{all:?}");
}

/// The Cameras pane on a view change: Live when the new view has no
/// shots, else that golfer's latest shot.
#[test]
fn the_cameras_pane_follows_its_opening_rule_on_a_view_change() {
    let mut app = golfers_cards_app();
    let mut config = two_golfers();
    config
        .golfers
        .push(flighthook::GolferRecord::new("flighthook.j", "Jordan"));
    config.camera.insert(
        "0".into(),
        flighthook::CameraSection {
            name: "Cam 0".into(),
            camera: flighthook::CameraSection::TEST_PATTERN.into(),
            ..flighthook::CameraSection::default()
        },
    );
    app.settings.load_from(&config);
    app.cameras_supported = true;
    app.refresh_shot_caches();
    app.active_tab = crate::panels::Tab::Shots;
    app.set_shots_pane(crate::panels::cameras::ShotsPane::Cameras);
    app.sync_cameras_pane();
    assert!(app.cameras_pane_shown());
    assert!(!app.live_selected, "Eric has shots");
    app.turn_golfer = Some("flighthook.j".into());
    app.refresh_shot_caches();
    assert!(app.live_selected, "Jordan has none: Live");
    app.turn_golfer = Some("flighthook.s".into());
    app.refresh_shot_caches();
    assert!(!app.live_selected, "Sam's latest");
    assert_eq!(app.shown_index(), Some(11));
}
/// Clear history's confirm in a frame: with two golfers, or one golfer
/// and earlier shots with no golfer, it asks to clear every golfer's
/// shots; with every shot one golfer's it counts them.
#[test]
fn the_clear_confirm_title_in_a_frame() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let texts = |app: &mut FlighthookApp| -> Vec<String> {
        app.confirm_clear = true;
        marked_frame(&ctx, app, Vec::new());
        let (_, out) = marked_frame(&ctx, app, Vec::new());
        painted_texts(&out.shapes)
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    };
    let mut app = golfers_cards_app();
    let all = texts(&mut app);
    assert!(
        all.iter().any(|t| t == "Clear every golfer's shots?"),
        "{all:?}"
    );
    let mut app = cards_app();
    app.settings.load_from(&flighthook::FlighthookConfig {
        golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
        ..Default::default()
    });
    app.turn_golfer = Some("flighthook.e".into());
    app.shots[11].golfer = Some("flighthook.e".into());
    app.shots_rev += 1;
    let all = texts(&mut app);
    assert!(
        all.iter().any(|t| t == "Clear every golfer's shots?"),
        "{all:?}"
    );
    for shot in &mut app.shots {
        shot.golfer = Some("flighthook.e".into());
    }
    app.shots_rev += 1;
    let all = texts(&mut app);
    assert!(
        all.iter().any(|t| t == "Delete all 12 shots and sets?"),
        "{all:?}"
    );
    assert_views_reach_every_shot(&app);
}

/// A pick made in Cards is marked and in view when the Table opens.
#[test]
fn a_cards_pick_is_revealed_when_the_table_opens() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.shots = thousand_rows();
    app.shots_rev = 1;
    app.shots_view = ShotsView::Cards;
    for _ in 0..4 {
        marked_frame(&ctx, &mut app, Vec::new());
    }
    // The card list's pick, as its click does it.
    app.select_shot(500);
    app.shot_focus.reveal();
    for _ in 0..3 {
        marked_frame(&ctx, &mut app, Vec::new());
    }
    let marks = marked_frame(&ctx, &mut app, Vec::new()).0;
    let toggle = marks["view_toggle"].0;
    click_at(
        &ctx,
        &mut app,
        egui::vec2(1600.0, 900.0),
        egui::pos2(toggle.right() - 20.0, toggle.center().y),
    );
    assert_eq!(app.shots_view, ShotsView::Table);
    for _ in 0..3 {
        marked_frame(&ctx, &mut app, Vec::new());
    }
    let out = marked_frame(&ctx, &mut app, Vec::new()).1;
    let numbers: Vec<u32> = painted_texts(&out.shapes)
        .iter()
        .filter_map(|(s, _)| s.parse().ok())
        .collect();
    assert!(numbers.contains(&501), "{numbers:?}");
}

/// One pass of the Shots tab in the portrait layout, 390×740 (a phone's
/// body under the two-row header); returns the marked controls.
fn portrait_frame(ctx: &egui::Context, app: &mut FlighthookApp) -> devices::Marks {
    app.portrait = true;
    ctx.data_mut(|d| d.remove_temp::<devices::Marks>(egui::Id::NULL));
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(390.0, 740.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::same(12)))
            .show(ctx, |ui| app.render_shots_panel(ui));
    });
    ctx.data(|d| d.get_temp::<devices::Marks>(egui::Id::NULL))
        .unwrap_or_default()
}

/// In the portrait layout the export and trash icons and the Clear
/// confirmation's buttons are touch targets.
#[test]
fn the_portrait_shot_list_icons_and_clear_confirm_are_touch_sized() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.shots = vec![row(1, true), row(2, true)];
    app.shots_rev += 1;
    let size = crate::topbar::TOUCH_TARGET;
    portrait_frame(&ctx, &mut app);
    let marks = portrait_frame(&ctx, &mut app);
    let (export, trash) = (marks["export"].0, marks["clear_history"].0);
    for rect in [export, trash] {
        assert!(rect.width() >= size && rect.height() >= size, "{rect:?}");
    }
    assert!(export.right() <= trash.left(), "{export:?} {trash:?}");
    app.confirm_clear = true;
    portrait_frame(&ctx, &mut app);
    let marks = portrait_frame(&ctx, &mut app);
    for key in ["clear_confirm:delete", "clear_confirm:cancel"] {
        let (rect, _) = marks[key];
        assert!(rect.height() >= size, "{key} {rect:?}");
    }
}

/// In the portrait layout the shot list sits on top, under half the body,
/// and the toolbar under it; nothing runs off the right edge, and neither
/// view has a chart beside or under it.
#[test]
fn the_portrait_shots_tab_has_the_list_on_top_and_no_chart_slots() {
    for view in [ShotsView::Cards, ShotsView::Table] {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = cards_app();
        app.shots_view = view;
        portrait_frame(&ctx, &mut app);
        let marks = portrait_frame(&ctx, &mut app);
        let toggle = marks["view_toggle"].0;
        let clear = marks["clear_history"].0;
        let export = marks["export"].0;
        assert!(
            toggle.top() > clear.bottom(),
            "{view:?}: the data under the list"
        );
        assert!(
            toggle.top() > 716.0 * 0.25,
            "{view:?}: under a third: {toggle:?}"
        );
        for r in [toggle, clear, export] {
            assert!(r.left() >= 12.0 && r.right() <= 378.0, "{view:?}: {r:?}");
        }
        let slots = app.chart_slots(view, &ctx);
        assert_eq!((slots.right, slots.bottom), (None, None), "{view:?}");
    }
}

/// In the portrait layout Cards draws Flight (then Dispersion) after the
/// last card group, in the cards' scroll; Table draws no chart.
#[test]
fn portrait_cards_end_with_the_charts_in_their_scroll() {
    for view in [ShotsView::Cards, ShotsView::Table] {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = cards_app();
        app.shots_view = view;
        portrait_frame(&ctx, &mut app);
        portrait_frame(&ctx, &mut app);
        let mut texts = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(390.0, 740.0),
            )),
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(egui::Margin::same(12)))
                .show(ctx, |ui| app.render_shots_panel(ui));
        });
        let mut tops = Vec::new();
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape {
                texts.push(t.galley.text().to_owned());
                tops.push((t.galley.text().to_owned(), t.pos.y));
            }
        }
        // The Flight chart's unit line; under every card in Cards, absent
        // in Table (Dispersion follows it, below this screen).
        let flight = tops.iter().find(|(t, _)| t == "yd · ft").map(|(_, y)| *y);
        let speed = tops
            .iter()
            .find(|(t, _)| t == "BALL SPEED")
            .map(|(_, y)| *y);
        match view {
            ShotsView::Cards => {
                let (flight, speed) = (flight.expect("Flight"), speed.expect("a card"));
                assert!(
                    flight > speed + 150.0,
                    "Flight {flight} under the cards ({speed})"
                );
            }
            ShotsView::Table => assert_eq!(flight, None, "{texts:?}"),
        }
    }
}

/// At a phone's 390x844 (a body 740 tall) the shot list of one device
/// opens tall enough for the set header and two whole entries above its
/// foot and the panel's margin.
#[test]
fn the_portrait_shot_list_shows_two_whole_entries() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut app = cards_app();
    for shot in &mut app.shots {
        shot.actor = "mevo.0".into();
    }
    for _ in 0..3 {
        portrait_frame(&ctx, &mut app);
    }
    let marks = portrait_frame(&ctx, &mut app);
    assert!(!marks.contains_key("device-filter"));
    let toggle = marks["view_toggle"].0;
    let foot = STRIP_BUTTONS_HEIGHT + FOOT_RULE + 2.0 * FOOT_SPACING + 8.0;
    let second = marks["shot_entry:10"].0;
    assert!(
        second.bottom() + foot <= toggle.top(),
        "{second:?} {toggle:?}"
    );
}

/// In the portrait layout the list can be shorter than a set header and
/// the entry under it: revealing the newest shot leaves the list at its top,
/// header in view, as it is with no reveal.
#[test]
fn the_portrait_reveal_keeps_the_set_header_in_view() {
    let top_entry = |reveal: bool| {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = cards_app();
        app.shot_focus.reveal = reveal;
        for _ in 0..60 {
            portrait_frame(&ctx, &mut app);
        }
        portrait_frame(&ctx, &mut app)["shot_entry:11"].0
    };
    let (still, revealed) = (top_entry(false), top_entry(true));
    assert_eq!(revealed.top(), still.top(), "{revealed:?} vs {still:?}");
}
