use super::*;
use flightrelay::Club;

fn select(
    club: Option<Option<Club>>,
    hand: Option<Handedness>,
    mode: Option<Mode>,
) -> GolferSelect {
    GolferSelect {
        club,
        handedness: hand,
        mode,
        ..GolferSelect::new("r")
    }
}

#[test]
fn the_session_starts_right_handed_in_full_with_no_golfer_or_club() {
    let (session, _writer) = Session::new();
    assert_eq!(session.turn(), start_turn());
    assert_eq!(start_turn().golfer, None);
    assert_eq!(start_turn().club, None);
    assert_eq!(start_turn().handedness, Some(Handedness::Right));
    assert_eq!(start_turn().mode, Some(Mode::Full));
}

#[test]
fn a_club_never_sets_the_mode() {
    let (session, writer) = Session::new();
    let turn = writer
        .apply(&select(Some(Some(Club::Putter)), None, None))
        .expect("changed");
    assert_eq!(turn.club, Some(Club::Putter));
    assert_eq!(turn.mode, Some(Mode::Full));
    assert_eq!(session.turn(), turn);
}

#[test]
fn only_a_select_with_a_mode_changes_the_mode() {
    let (_session, writer) = Session::new();
    let turn = writer
        .apply(&select(
            Some(Some(Club::SandWedge)),
            None,
            Some(Mode::Chipping),
        ))
        .expect("changed");
    assert_eq!(turn.mode, Some(Mode::Chipping));
    // A later club alone (a header pick) leaves the mode the card set.
    let turn = writer
        .apply(&select(Some(Some(Club::Driver)), None, None))
        .expect("changed");
    assert_eq!(turn.club, Some(Club::Driver));
    assert_eq!(turn.mode, Some(Mode::Chipping));
    let turn = writer
        .apply(&select(None, None, Some(Mode::Full)))
        .expect("changed");
    assert_eq!(turn.mode, Some(Mode::Full));
}

#[test]
fn a_select_that_changes_nothing_publishes_nothing() {
    let (_session, writer) = Session::new();
    let all = select(
        Some(Some(Club::SandWedge)),
        Some(Handedness::Left),
        Some(Mode::Chipping),
    );
    assert!(writer.apply(&all).is_some());
    assert!(writer.apply(&all).is_none());
    assert!(writer.apply(&select(None, None, None)).is_none());
}

#[test]
fn each_field_replaces_its_own_and_the_rest_stays() {
    let (_session, writer) = Session::new();
    writer.apply(&select(
        Some(Some(Club::Iron7)),
        Some(Handedness::Left),
        Some(Mode::Chipping),
    ));
    let turn = writer
        .apply(&select(None, None, Some(Mode::Putting)))
        .expect("changed");
    assert_eq!(turn.club, Some(Club::Iron7));
    assert_eq!(turn.handedness, Some(Handedness::Left));
    assert_eq!(turn.mode, Some(Mode::Putting));
    let turn = writer
        .apply(&select(Some(None), None, None))
        .expect("changed");
    assert_eq!(turn.club, None);
    assert_eq!(turn.mode, Some(Mode::Putting));
}

#[test]
fn the_cause_is_the_sender_of_the_last_change() {
    let (_session, writer) = Session::new();
    assert_eq!(writer.cause(), None);
    let iron = select(Some(Some(Club::Iron7)), None, None);
    assert!(writer.apply_from("gspro-1", &iron).is_some());
    assert_eq!(writer.cause().as_deref(), Some("gspro-1"));
    // A select that changes nothing leaves the cause alone.
    assert!(writer.apply_from("web", &iron).is_none());
    assert_eq!(writer.cause().as_deref(), Some("gspro-1"));
    assert!(writer.apply(&select(Some(None), None, None)).is_some());
    assert_eq!(writer.cause(), None);
}

fn golfers() -> FlighthookConfig {
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.identifiers.push("pureplay.local-3".into());
    eric.handed = Some(Handedness::Left);
    FlighthookConfig {
        golfers: vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")],
        ..FlighthookConfig::default()
    }
}

fn pick(golfer: &str) -> GolferSelect {
    GolferSelect {
        golfer: Some(Some(golfer.parse().expect("id"))),
        ..GolferSelect::new("r")
    }
}

#[test]
fn the_session_starts_with_the_selected_golfer_else_the_first_and_its_hand() {
    let config = golfers();
    let turn = start_turn_for(&config);
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("flighthook.e")
    );
    assert_eq!(turn.handedness, Some(Handedness::Left));
    assert_eq!(turn.club, None);
    assert_eq!(turn.mode, Some(Mode::Full));
    let config = FlighthookConfig {
        selected_golfer: Some("flighthook.s".into()),
        ..golfers()
    };
    let turn = start_turn_for(&config);
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("flighthook.s")
    );
    assert_eq!(turn.handedness, Some(Handedness::Right), "Sam has no hand");
    assert_eq!(start_turn_for(&FlighthookConfig::default()), start_turn());
}

#[test]
fn hand_and_last_club_follow_the_golfer() {
    let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
    // Eric (lh) is up: 7I.
    writer.apply(&select(Some(Some(Club::Iron7)), None, None));
    // Sam: no last club yet, keeps the club in play; no hand of his own.
    let turn = writer.apply(&pick("flighthook.s")).expect("changed");
    assert_eq!(turn.club, Some(Club::Iron7));
    assert_eq!(turn.handedness, Some(Handedness::Left));
    writer.apply(&select(
        Some(Some(Club::Driver)),
        Some(Handedness::Right),
        None,
    ));
    // Back to Eric: his own club and hand.
    let turn = writer.apply(&pick("flighthook.e")).expect("changed");
    assert_eq!(turn.club, Some(Club::Iron7));
    assert_eq!(turn.handedness, Some(Handedness::Left));
    // Sam again: the driver.
    let turn = writer.apply(&pick("flighthook.s")).expect("changed");
    assert_eq!(turn.club, Some(Club::Driver));
    // Eric's linked game id is Eric: his last club, shared.
    let turn = writer.apply(&pick("pureplay.local-3")).expect("changed");
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("pureplay.local-3")
    );
    assert_eq!(turn.club, Some(Club::Iron7));
    // Eric's own id after his linked one is the same golfer: no club or
    // hand change, only the raw id.
    writer.apply(&select(Some(Some(Club::Putter)), None, None));
    let turn = writer.apply(&pick("flighthook.e")).expect("changed");
    assert_eq!(turn.club, Some(Club::Putter));
    // A select carrying club and hand with the golfer: those win.
    let mut s = pick("flighthook.s");
    s.club = Some(Some(Club::Wood3));
    s.handedness = Some(Handedness::Left);
    let turn = writer.apply(&s).expect("changed");
    assert_eq!(turn.club, Some(Club::Wood3));
    assert_eq!(turn.handedness, Some(Handedness::Left));
    // An unknown id stands on its own, and is taken.
    let turn = writer.apply(&pick("backnine.p7")).expect("changed");
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("backnine.p7")
    );
    assert_eq!(turn.club, Some(Club::Wood3), "no last club of its own");
}

/// Each golfer's own id and hand in `config`: what
/// [`SessionWriter::follow_golfers`] compares against.
fn hands(config: &FlighthookConfig) -> BTreeMap<String, Option<Handedness>> {
    config
        .golfers
        .iter()
        .filter_map(|g| Some((g.own_id()?.to_owned(), g.handed)))
        .collect()
}

#[test]
fn a_golfer_change_in_the_config_is_followed() {
    let view = ConfigView::detached(FlighthookConfig::default());
    let (_session, writer) = Session::with_config(view.clone());
    // Follow the change from what the config held before it.
    let follow = |change: &dyn Fn(&mut FlighthookConfig)| {
        let before = hands(&view.read());
        change(&mut view.write());
        writer.follow_golfers("fh", &before)
    };
    assert_eq!(writer.turn().golfer, None);
    assert!(follow(&|_| {}).is_none(), "still no golfer");
    // The first saved golfer becomes the golfer up, with its hand.
    let turn = follow(&|c| *c = golfers()).expect("changed");
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("flighthook.e")
    );
    assert_eq!(turn.handedness, Some(Handedness::Left));
    assert_eq!(writer.cause().as_deref(), Some("fh"));
    // Another golfer added: the golfer up stays.
    assert!(
        follow(&|c| c
            .golfers
            .push(flighthook::GolferRecord::new("flighthook.x", "X")))
        .is_none()
    );
    // The golfer up removed: the start golfer comes up.
    let turn = follow(&|c| {
        c.golfers.remove(0);
    })
    .expect("changed");
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("flighthook.s")
    );
    // A game id up is never replaced.
    writer.apply(&pick("pureplay.local-3"));
    assert!(follow(&|c| c.golfers.truncate(2)).is_none());
    // Every golfer removed: no golfer.
    writer.apply(&pick("flighthook.s"));
    let turn = follow(&|c| c.golfers.clear()).expect("changed");
    assert_eq!(turn.golfer, None);
}

fn profile(from: &str, id: &str) -> FrpMessage {
    FrpMessage::new(
        from,
        flightrelay::GolferProfile::new(id.parse().expect("id"), "G"),
    )
}

fn removed(from: &str, id: &str) -> FrpMessage {
    FrpMessage::new(
        from,
        flightrelay::Removal::of_id(StateKind::GolferProfile, id),
    )
}

/// The game golfer up whose profile goes: the turn goes back to the
/// start golfer with that golfer's hand and mode full. A user with no
/// saved golfers gets no golfer.
#[test]
fn a_game_golfer_up_whose_profile_goes_returns_the_turn_to_the_start_golfer() {
    let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
    // Eric (lh) is the start golfer. A game golfer comes up in the
    // putter and chipping.
    assert!(
        writer
            .follow_profiles("fh", &profile("pp", "pureplay.player-1"))
            .is_none()
    );
    writer.apply_from(
        "pp",
        &GolferSelect {
            golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
            club: Some(Some(Club::Putter)),
            handedness: Some(Handedness::Right),
            mode: Some(Mode::Putting),
            ..GolferSelect::new("r")
        },
    );
    // Another golfer's profile going changes nothing.
    assert!(
        writer
            .follow_profiles("fh", &profile("pp", "pureplay.player-2"))
            .is_none()
    );
    assert!(
        writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-2"))
            .is_none()
    );
    let turn = writer
        .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
        .expect("back");
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("flighthook.e")
    );
    assert_eq!(turn.handedness, Some(Handedness::Left));
    assert_eq!(turn.mode, Some(Mode::Full));
    assert_eq!(writer.cause().as_deref(), Some("fh"));
    // Nothing more when it is already back.
    assert!(
        writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .is_none()
    );
}

#[test]
fn a_gone_card_takes_its_golfers_with_it() {
    let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
    writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
    writer.apply_from(
        "pp",
        &GolferSelect {
            golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
            mode: Some(Mode::Chipping),
            ..GolferSelect::new("r")
        },
    );
    // Another actor going leaves the turn.
    assert!(writer.actor_gone("fh", "other").is_none());
    let turn = writer.actor_gone("fh", "pp").expect("back");
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("flighthook.e")
    );
    assert_eq!(turn.mode, Some(Mode::Full));
    // The removal of its `bus.actor` does the same.
    writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
    writer.apply_from("pp", &pick("pureplay.player-1"));
    let bye = FrpMessage::new("pp", flightrelay::Removal::of(StateKind::BusActor));
    assert!(writer.follow_profiles("fh", &bye).is_some());
}

#[test]
fn with_no_saved_golfers_the_turn_goes_back_to_no_golfer() {
    let (_session, writer) = Session::new();
    writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
    writer.apply_from("pp", &pick("pureplay.player-1"));
    let turn = writer
        .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
        .expect("back");
    assert_eq!(turn.golfer, None);
    assert_eq!(turn.handedness, Some(Handedness::Right));
    assert_eq!(turn.mode, Some(Mode::Full));
}

/// A flighthook golfer up is never sent back by someone's removal.
#[test]
fn a_flighthook_golfer_is_not_a_game_golfer() {
    let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
    writer.apply_from("web", &pick("flighthook.s"));
    assert!(
        writer
            .follow_profiles("fh", &removed("pp", "flighthook.s"))
            .is_none()
    );
}

/// Linking a game id to a golfer moves the id's last club to that
/// golfer: a header pick of the golfer then gives the club just hit
/// under the game's id.
#[test]
fn linking_a_game_id_rekeys_its_last_club() {
    let view = ConfigView::detached(FlighthookConfig {
        golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
        ..FlighthookConfig::default()
    });
    let (_session, writer) = Session::with_config(view.clone());
    // Eric had the 7 iron; then the game's golfer took the 9 iron.
    writer.apply(&select(Some(Some(Club::Iron7)), None, None));
    writer.apply(&pick("pureplay.local-3"));
    writer.apply(&select(Some(Some(Club::Iron9)), None, None));
    writer.apply(&pick("flighthook.s"));
    // The link: the game's golfer is Eric now.
    view.write().golfers[0]
        .identifiers
        .push("pureplay.local-3".into());
    writer.follow_golfers("fh", &hands(&view.read()));
    let turn = writer.apply(&pick("flighthook.e")).expect("changed");
    assert_eq!(turn.club, Some(Club::Iron9), "the club just hit");
}

/// A game golfer linked to a saved golfer who is not the start golfer:
/// the turn goes to that golfer, not to the start golfer.
#[test]
fn a_linked_golfer_whose_profile_goes_stays_that_golfer() {
    let mut sam = flighthook::GolferRecord::new("flighthook.s", "Sam");
    sam.identifiers.push("pureplay.player-1".into());
    sam.handed = Some(Handedness::Right);
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.handed = Some(Handedness::Left);
    let config = FlighthookConfig {
        golfers: vec![eric, sam],
        ..FlighthookConfig::default()
    };
    let (_session, writer) = Session::with_config(ConfigView::detached(config));
    writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
    writer.apply_from(
        "pp",
        &GolferSelect {
            golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
            club: Some(Some(Club::Iron7)),
            handedness: Some(Handedness::Right),
            mode: Some(Mode::Chipping),
            ..GolferSelect::new("r")
        },
    );
    let turn = writer
        .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
        .expect("changed");
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("flighthook.s")
    );
    assert_eq!(turn.handedness, Some(Handedness::Right));
    assert_eq!(turn.club, Some(Club::Iron7));
    assert_eq!(turn.mode, Some(Mode::Full));
}

/// With no saved hand to go back to, the turn keeps the hand it has.
#[test]
fn the_fallback_keeps_the_hand_when_there_is_no_saved_hand() {
    let (_session, writer) = Session::new();
    writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
    writer.apply_from(
        "pp",
        &GolferSelect {
            golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
            handedness: Some(Handedness::Left),
            ..GolferSelect::new("r")
        },
    );
    let turn = writer
        .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
        .expect("changed");
    assert_eq!(turn.golfer, None);
    assert_eq!(turn.handedness, Some(Handedness::Left));
    // A saved golfer with no hand of their own is the same.
    let config = FlighthookConfig {
        golfers: vec![flighthook::GolferRecord::new("flighthook.s", "Sam")],
        ..FlighthookConfig::default()
    };
    let (_session, writer) = Session::with_config(ConfigView::detached(config));
    writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
    writer.apply_from(
        "pp",
        &GolferSelect {
            golfer: Some(Some("pureplay.player-1".parse().expect("id"))),
            handedness: Some(Handedness::Left),
            ..GolferSelect::new("r")
        },
    );
    let turn = writer
        .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
        .expect("changed");
    assert_eq!(
        turn.golfer.as_ref().map(GolferId::as_str),
        Some("flighthook.s")
    );
    assert_eq!(turn.handedness, Some(Handedness::Left));
}

/// Only the actor that published a golfer's profile takes it away.
#[test]
fn another_actors_removal_of_a_profile_does_nothing() {
    let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
    writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
    writer.apply_from("pp", &pick("pureplay.player-1"));
    assert!(
        writer
            .follow_profiles("fh", &removed("someone-else", "pureplay.player-1"))
            .is_none()
    );
    assert_eq!(
        writer.turn().golfer.as_ref().map(GolferId::as_str),
        Some("pureplay.player-1")
    );
    // The sender's own removal still counts, and the record is gone.
    assert!(
        writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .is_some()
    );
    writer.apply_from("pp", &pick("pureplay.player-1"));
    assert!(
        writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .is_none(),
        "a removal of an id nobody holds is nothing"
    );
}

/// A second publisher of an id cannot take it away by removing its own
/// copy: the golfer stays while the first publisher still holds it.
#[test]
fn a_second_publishers_removal_leaves_the_first_ones_golfer() {
    let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
    writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
    writer.apply_from("pp", &pick("pureplay.player-1"));
    writer.follow_profiles("fh", &profile("other", "pureplay.player-1"));
    assert!(
        writer
            .follow_profiles("fh", &removed("other", "pureplay.player-1"))
            .is_none()
    );
    assert_eq!(
        writer.turn().golfer.as_ref().map(GolferId::as_str),
        Some("pureplay.player-1")
    );
    assert!(
        writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .is_some(),
        "the last sender's removal counts"
    );
}

/// Two publishers of one id: the golfer is gone only once both removed it.
#[test]
fn a_golfer_held_by_two_senders_goes_when_both_removed_it() {
    let (_session, writer) = Session::with_config(ConfigView::detached(golfers()));
    writer.follow_profiles("fh", &profile("other", "pureplay.player-1"));
    writer.follow_profiles("fh", &profile("pp", "pureplay.player-1"));
    writer.apply_from("pp", &pick("pureplay.player-1"));
    assert!(
        writer
            .follow_profiles("fh", &removed("pp", "pureplay.player-1"))
            .is_none(),
        "still held by the other sender"
    );
    assert_eq!(
        writer.turn().golfer.as_ref().map(GolferId::as_str),
        Some("pureplay.player-1")
    );
    let back = writer
        .follow_profiles("fh", &removed("other", "pureplay.player-1"))
        .expect("gone with its last sender");
    assert_ne!(
        back.golfer.as_ref().map(GolferId::as_str),
        Some("pureplay.player-1")
    );
}
