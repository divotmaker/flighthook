use flightrelay::{Club, Handedness, Mode};

use super::*;

/// A shot with data for the golfer up's open block; the block it
/// counted for.
fn hit(p: &mut Practice) -> Option<String> {
    let id = crate::state::config::generate_id();
    let block = p.trigger(&id, "lm", 0);
    p.data(&id, "lm", false);
    p.finished(&id, "lm");
    block
}

/// A trigger that finishes with no data counts for nothing: the empty
/// block is republished in place for another club.
#[test]
fn a_no_read_leaves_the_block_empty() {
    let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    let first = opened(&start)[0].clone();
    assert_eq!(p.trigger("miss", "lm", 0), Some(first.id.clone()));
    p.finished("miss", "lm");
    let changes = p.turn(turn(None, Some(Club::Driver)), 1);
    let again = &opened(&changes)[0];
    assert_eq!(changes.len(), 1, "no block closed");
    assert_eq!((again.id.as_str(), again.number), (first.id.as_str(), 1));
    // A shot with data keeps its count past its finish.
    assert_eq!(p.trigger("hit", "lm", 0), Some(first.id.clone()));
    p.data("hit", "lm", false);
    p.data("hit", "lm", false);
    p.finished("hit", "lm");
    let changes = p.turn(turn(None, Some(Club::Iron7)), 2);
    assert_eq!(closed(&changes), vec![first.id.clone()]);
    assert_eq!(opened(&changes)[0].number, 2);
}

/// A shot that never finishes gives its count back when its launch
/// monitor is gone, or after the shot timeout, even with data; another
/// actor's launch or finish does not count as the launch monitor's.
#[test]
fn a_shot_that_never_finishes_leaves_no_gap() {
    let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    let first = opened(&start)[0].clone();
    p.trigger("dies", "remote-lm", 0);
    p.data("dies", "remote-lm", false);
    p.gone("remote-lm");
    let changes = p.turn(turn(None, Some(Club::Driver)), 1);
    assert_eq!(changes.len(), 1, "no block closed");
    assert_eq!(opened(&changes)[0].number, 1);

    p.trigger("stalls", "lm", 1000);
    p.data("stalls", "other", false);
    p.finished("stalls", "other");
    p.expire(1000 + 44_000);
    p.data("stalls", "other", true);
    assert!(
        p.pending.iter().any(|q| q.shot_id == "stalls" && q.data),
        "a flight counts from anyone, and only the launch monitor finishes"
    );
    p.expire(1000 + 46_000);
    let changes = p.turn(turn(None, Some(Club::Iron7)), 2);
    assert_eq!(changes.len(), 1, "still empty: no block closed");
    assert_eq!(opened(&changes)[0].id, first.id);
}

fn turn(golfer: Option<&str>, club: Option<Club>) -> GolferTurn {
    GolferTurn::new(
        golfer.map(|g| g.parse().unwrap()),
        club,
        Handedness::Right,
        Mode::Full,
    )
}

fn opened(changes: &[BlockChange]) -> Vec<PracticeBlock> {
    changes
        .iter()
        .filter_map(|c| match c {
            BlockChange::Open(b) => Some(b.clone()),
            BlockChange::Closed(_) => None,
        })
        .collect()
}

fn closed(changes: &[BlockChange]) -> Vec<String> {
    changes
        .iter()
        .filter_map(|c| match c {
            BlockChange::Closed(id) => Some(id.clone()),
            BlockChange::Open(_) => None,
        })
        .collect()
}

/// The start with no golfer, then the first golfer up: the empty
/// no-golfer block becomes theirs (same id, number 1), so no empty block
/// stays behind; with shots it stays the no-golfer block.
#[test]
fn the_first_golfer_takes_over_the_empty_no_golfer_block() {
    let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    let first = opened(&start)[0].clone();
    let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Iron7)), 1);
    assert_eq!(changes.len(), 1, "{changes:?}");
    let b = &opened(&changes)[0];
    assert_eq!((b.id.as_str(), b.number), (first.id.as_str(), 1));
    assert_eq!(
        b.golfer.as_ref().map(GolferId::as_str),
        Some("flighthook.e")
    );
    assert_eq!(p.open_blocks().len(), 1);
    // Their next block is number 2.
    hit(&mut p);
    let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Driver)), 2);
    assert_eq!(opened(&changes)[0].number, 2);

    // With a shot, the no-golfer block stays as it is.
    let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    hit(&mut p);
    let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Iron7)), 1);
    let b = &opened(&changes)[0];
    assert_eq!(b.number, 1);
    assert_eq!(p.open_blocks().len(), 2);
}

/// The empty no-golfer block the first golfer takes over
/// becomes their Set 1 even after no-golfer Sets with shots, and the
/// no-golfer numbers go on where they were.
#[test]
fn the_first_golfers_sets_start_at_set_1() {
    let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    hit(&mut p);
    let changes = p.turn(turn(None, Some(Club::Driver)), 1);
    let dr = opened(&changes)[0].clone();
    assert_eq!(dr.number, 2);
    let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Driver)), 2);
    assert_eq!(changes.len(), 1, "{changes:?}");
    let b = &opened(&changes)[0];
    assert_eq!((b.id.as_str(), b.number), (dr.id.as_str(), 1));
    hit(&mut p);
    let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Iron7)), 3);
    assert_eq!(opened(&changes)[0].number, 2, "Eric's Set 2");
    // No golfer again: the no-golfer Set 1 exists, so this is Set 2.
    let changes = p.turn(turn(None, Some(Club::Iron7)), 4);
    let b = &opened(&changes)[0];
    assert_eq!((b.golfer.clone(), b.number), (None, 2));
}

/// A trigger, a club change (the block
/// closes with the shot in flight), then the launch monitor dies or the
/// shot times out. The closed block ends empty, so the new block takes
/// its number back: no gap.
#[test]
fn a_club_change_before_the_monitor_dies_leaves_no_gap() {
    let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    p.trigger("dies", "remote-lm", 0);
    let changes = p.turn(turn(None, Some(Club::Driver)), 1);
    let dr = opened(&changes)[0].clone();
    assert_eq!(dr.number, 2);
    let changes = p.gone("remote-lm");
    assert_eq!(changes.len(), 1, "{changes:?}");
    let b = &opened(&changes)[0];
    assert_eq!((b.id.as_str(), b.number), (dr.id.as_str(), 1));
    hit(&mut p);
    let changes = p.turn(turn(None, Some(Club::Iron7)), 2);
    assert_eq!(opened(&changes)[0].number, 2);

    // The shot timeout gives it back the same way.
    let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    p.trigger("stalls", "lm", 0);
    let dr = opened(&p.turn(turn(None, Some(Club::Driver)), 1))[0].clone();
    let changes = p.expire(46_000);
    let b = &opened(&changes)[0];
    assert_eq!((b.id.as_str(), b.number), (dr.id.as_str(), 1));

    // A no-read finish too; one with data keeps the closed block.
    let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    p.trigger("miss", "lm", 0);
    let dr = opened(&p.turn(turn(None, Some(Club::Driver)), 1))[0].clone();
    let b = &opened(&p.finished("miss", "lm"))[0];
    assert_eq!((b.id.as_str(), b.number), (dr.id.as_str(), 1));
    let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    p.trigger("hit", "lm", 0);
    p.turn(turn(None, Some(Club::Driver)), 1);
    p.data("hit", "lm", false);
    assert!(p.finished("hit", "lm").is_empty());
    // The new block has a shot already: the gap stays, nothing moves.
    let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    p.trigger("dies", "remote-lm", 0);
    p.turn(turn(None, Some(Club::Driver)), 1);
    hit(&mut p);
    assert!(p.gone("remote-lm").is_empty());
}

#[test]
fn start_opens_block_one_of_session_one_for_the_golfer_up() {
    let (p, changes) = Practice::start(turn(None, Some(Club::Iron7)), 5);
    let b = &opened(&changes)[0];
    assert_eq!(changes.len(), 1);
    assert_eq!(
        (b.session, b.number, b.club, b.golfer.clone()),
        (1, 1, Some(Club::Iron7), None)
    );
    assert_eq!(b.started_at, 5);
    assert_eq!(p.session(), 1);
    assert_eq!(p.open_blocks(), vec![b.clone()]);
}

#[test]
fn club_scrolling_before_a_shot_republishes_the_empty_block_in_place() {
    let (mut p, start) = Practice::start(turn(None, None), 0);
    let first = opened(&start)[0].clone();
    for club in [Club::Driver, Club::Iron7, Club::PitchingWedge] {
        let changes = p.turn(turn(None, Some(club)), 1);
        let b = &opened(&changes)[0];
        assert_eq!(changes.len(), 1);
        assert_eq!(
            (b.id.as_str(), b.number, b.club),
            (first.id.as_str(), 1, Some(club))
        );
    }
}

#[test]
fn a_turn_that_changes_neither_club_nor_golfer_publishes_nothing() {
    let (mut p, _) = Practice::start(turn(None, Some(Club::Driver)), 0);
    assert!(p.turn(turn(None, Some(Club::Driver)), 1).is_empty());
    hit(&mut p);
    assert!(p.turn(turn(None, Some(Club::Driver)), 1).is_empty());
}

#[test]
fn another_club_after_a_shot_closes_the_block_and_opens_the_next_number() {
    let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    let first = opened(&start)[0].clone();
    assert_eq!(hit(&mut p), Some(first.id.clone()));
    let changes = p.turn(turn(None, Some(Club::Driver)), 1);
    assert_eq!(closed(&changes), vec![first.id.clone()]);
    let next = &opened(&changes)[0];
    assert_eq!((next.number, next.club), (2, Some(Club::Driver)));
    assert_ne!(next.id, first.id);
    assert!(matches!(changes[0], BlockChange::Closed(_)), "closed first");
}

#[test]
fn returning_to_a_club_without_a_shot_gives_a_new_block() {
    let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    hit(&mut p);
    let dr = opened(&p.turn(turn(None, Some(Club::Driver)), 1))[0].clone();
    let back = p.turn(turn(None, Some(Club::Iron7)), 2);
    // The DR block is empty: republished in place as 7I, number 2.
    let b = &opened(&back)[0];
    assert_eq!(
        (b.id.as_str(), b.number, b.club),
        (dr.id.as_str(), 2, Some(Club::Iron7))
    );
    assert!(closed(&back).is_empty());
}

#[test]
fn new_block_on_an_empty_block_is_a_no_op() {
    let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    assert!(p.new_block(None, false, 1).is_empty());
    assert!(p.new_block(Some(None), false, 1).is_empty());
}

#[test]
fn new_block_after_a_shot_rolls_to_the_next_number_on_the_same_club() {
    let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    hit(&mut p);
    let changes = p.new_block(None, false, 9);
    assert_eq!(closed(&changes), vec![opened(&start)[0].id.clone()]);
    let b = &opened(&changes)[0];
    assert_eq!(
        (b.number, b.club, b.session, b.started_at),
        (2, Some(Club::Iron7), 1, 9)
    );
}

#[test]
fn new_block_for_a_golfer_not_up_uses_their_own_club() {
    let (mut p, _) = Practice::start(turn(Some("pureplay.local-1"), Some(Club::Iron7)), 0);
    hit(&mut p);
    p.turn(turn(Some("pureplay.local-2"), Some(Club::Driver)), 1);
    let one: GolferId = "pureplay.local-1".parse().unwrap();
    let changes = p.new_block(Some(Some(one.clone())), false, 2);
    let b = &opened(&changes)[0];
    assert_eq!(
        (b.golfer.clone(), b.club, b.number),
        (Some(one), Some(Club::Iron7), 2)
    );
    // A golfer with no block gets the turn's club.
    let three: GolferId = "pureplay.local-3".parse().unwrap();
    let b = &opened(&p.new_block(Some(Some(three.clone())), false, 3))[0];
    assert_eq!(
        (b.golfer.clone(), b.club, b.number),
        (Some(three), Some(Club::Driver), 1)
    );
}

#[test]
fn other_golfers_blocks_stay_open_and_a_golfer_coming_back_continues() {
    let (mut p, start) = Practice::start(turn(Some("pureplay.local-1"), Some(Club::Iron7)), 0);
    let one = opened(&start)[0].clone();
    hit(&mut p);
    let two = p.turn(turn(Some("pureplay.local-2"), Some(Club::Driver)), 1);
    assert!(closed(&two).is_empty());
    assert_eq!(opened(&two)[0].number, 1, "numbers count per golfer");
    hit(&mut p);
    let back = p.turn(turn(Some("pureplay.local-1"), Some(Club::Iron7)), 2);
    assert!(back.is_empty(), "same club, same block: {back:?}");
    assert_eq!(hit(&mut p), Some(one.id.clone()));
    assert_eq!(p.open_blocks().len(), 2);
}

#[test]
fn new_session_closes_every_block_and_counts_the_session_up() {
    let (mut p, _) = Practice::start(turn(Some("pureplay.local-1"), Some(Club::Iron7)), 0);
    hit(&mut p);
    p.turn(turn(Some("pureplay.local-2"), Some(Club::Driver)), 1);
    let changes = p.new_block(Some(None), true, 2);
    assert_eq!(closed(&changes).len(), 2);
    let b = &opened(&changes)[0];
    assert_eq!((b.session, b.number, b.club), (2, 1, Some(Club::Driver)));
    assert_eq!(p.session(), 2);
    assert_eq!(p.open_blocks(), vec![b.clone()]);
    // An empty block's new_session still rolls (it is not new_block).
    assert_eq!(closed(&p.new_session(3)).len(), 1);
    assert_eq!(p.session(), 3);
}

#[test]
fn the_null_golfer_to_a_golfer_leaves_no_gaps() {
    let (mut p, _) = Practice::start(turn(None, None), 0);
    let changes = p.turn(turn(Some("flighthook.a"), Some(Club::Driver)), 1);
    let b = &opened(&changes)[0];
    assert_eq!(b.number, 1);
}

/// Eric (`flighthook.e`, linked to `pureplay.local-3`) and Sam.
fn linked() -> ConfigView {
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.identifiers.push("pureplay.local-3".into());
    ConfigView::detached(flighthook::FlighthookConfig {
        golfers: vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")],
        ..flighthook::FlighthookConfig::default()
    })
}

/// A linked profile shares its golfer's block run: Eric's game id
/// continues the block Eric opened (republished under the game id),
/// and numbers count once for both ids.
#[test]
fn a_linked_profile_shares_its_golfers_block_run() {
    let (mut p, start) =
        Practice::with_config(linked(), turn(Some("flighthook.e"), Some(Club::Iron7)), 0);
    let one = opened(&start)[0].clone();
    hit(&mut p);
    let game = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 1);
    let b = &opened(&game)[0];
    assert_eq!(game.len(), 1, "republished in place: {game:?}");
    assert_eq!((b.id.as_str(), b.number), (one.id.as_str(), 1));
    assert_eq!(
        b.golfer.as_ref().map(GolferId::as_str),
        Some("pureplay.local-3"),
        "the wire carries the raw id it was last republished under"
    );
    assert_eq!(hit(&mut p), Some(one.id.clone()));
    // A club change under the game id rolls Eric's run to number 2.
    let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Driver)), 2);
    assert_eq!(closed(&next), vec![one.id.clone()]);
    assert_eq!(opened(&next)[0].number, 2);
    hit(&mut p);
    // New set for Eric by his own id while the game id is up: the new
    // block carries the turn's raw id, on his club.
    let roll = p.new_block(Some(Some("flighthook.e".parse().unwrap())), false, 3);
    let b = &opened(&roll)[0];
    assert_eq!(
        (b.number, b.club, b.golfer.as_ref().map(GolferId::as_str)),
        (3, Some(Club::Driver), Some("pureplay.local-3"))
    );
    assert_eq!(p.open_blocks().len(), 1, "one open block for Eric");
    // Unlinked, the same ids are two golfers.
    let (mut p, _) = Practice::start(turn(Some("flighthook.e"), Some(Club::Iron7)), 0);
    hit(&mut p);
    let game = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 1);
    assert_ne!(opened(&game)[0].id, one.id);
    assert_eq!(p.open_blocks().len(), 2);
}

/// Two golfers alternating keep their own blocks, with one club and
/// with different clubs.
#[test]
fn two_golfers_alternating_each_keep_their_block() {
    for (eric_club, sam_club) in [(Club::Iron7, Club::Iron7), (Club::Iron7, Club::Driver)] {
        let (mut p, start) =
            Practice::with_config(linked(), turn(Some("flighthook.e"), Some(eric_club)), 0);
        let eric = opened(&start)[0].clone();
        hit(&mut p);
        let sam = opened(&p.turn(turn(Some("flighthook.s"), Some(sam_club)), 1))[0].clone();
        assert_eq!(sam.number, 1);
        for round in 0..3 {
            hit(&mut p);
            let back = p.turn(turn(Some("pureplay.local-3"), Some(eric_club)), 2 + round);
            assert!(closed(&back).is_empty(), "{back:?}");
            assert_eq!(hit(&mut p), Some(eric.id.clone()));
            let back = p.turn(turn(Some("flighthook.s"), Some(sam_club)), 2 + round);
            assert!(back.is_empty(), "{back:?}");
            assert_eq!(p.current().map(|b| b.id.clone()), Some(sam.id.clone()));
        }
        assert_eq!(p.open_blocks().len(), 2);
    }
}

/// The session and the practice actor together. Two
/// golfers alternating by golfer-only picks (the header's), with one
/// club and with different clubs: each gets their own club back and
/// keeps their block; Eric's linked game id shares his set run.
#[test]
fn alternating_golfers_get_their_club_and_their_block_back() {
    use crate::state::session::Session;
    let pick = |golfer: &str| flightrelay::GolferSelect {
        golfer: Some(Some(golfer.parse().expect("id"))),
        ..flightrelay::GolferSelect::new("r")
    };
    let club = |c: Club| flightrelay::GolferSelect {
        club: Some(Some(c)),
        ..flightrelay::GolferSelect::new("r")
    };
    for (eric_club, sam_club) in [(Club::Iron7, Club::Iron7), (Club::Iron7, Club::Driver)] {
        let config = linked();
        let (_session, writer) = Session::with_config(config.clone());
        let (mut p, start) = Practice::with_config(config, writer.turn(), 0);
        let follow = |p: &mut Practice, turn: Option<GolferTurn>| {
            turn.map(|t| p.turn(t, 1)).unwrap_or_default()
        };
        follow(&mut p, writer.apply(&club(eric_club)));
        let eric = p.current().expect("eric's block").clone();
        assert_eq!(eric.id, opened(&start)[0].id, "empty block republished");
        hit(&mut p);
        follow(&mut p, writer.apply(&pick("flighthook.s")));
        follow(&mut p, writer.apply(&club(sam_club)));
        let sam = p.current().expect("sam's block").clone();
        assert_ne!(sam.id, eric.id);
        hit(&mut p);
        for _ in 0..3 {
            let changes = follow(&mut p, writer.apply(&pick("pureplay.local-3")));
            assert_eq!(writer.turn().club, Some(eric_club), "Eric's club back");
            assert!(closed(&changes).is_empty(), "{changes:?}");
            assert_eq!(hit(&mut p), Some(eric.id.clone()));
            let changes = follow(&mut p, writer.apply(&pick("flighthook.s")));
            assert_eq!(writer.turn().club, Some(sam_club), "Sam's club back");
            assert!(closed(&changes).is_empty(), "{changes:?}");
            assert_eq!(hit(&mut p), Some(sam.id.clone()));
            follow(&mut p, writer.apply(&pick("flighthook.e")));
            assert_eq!(p.current().map(|b| b.id.clone()), Some(eric.id.clone()));
        }
        assert_eq!(p.open_blocks().len(), 2);
        assert!(p.open_blocks().iter().all(|b| b.number == 1));
    }
}

/// Eric and Sam, nobody linked.
fn unlinked() -> ConfigView {
    ConfigView::detached(flighthook::FlighthookConfig {
        golfers: vec![
            flighthook::GolferRecord::new("flighthook.e", "Eric"),
            flighthook::GolferRecord::new("flighthook.s", "Sam"),
        ],
        ..flighthook::FlighthookConfig::default()
    })
}

/// The open blocks by wire golfer: at most one each.
fn one_per_golfer(p: &Practice) {
    let mut golfers: Vec<_> = p.open_blocks().into_iter().map(|b| b.golfer).collect();
    let all = golfers.len();
    golfers.sort();
    golfers.dedup();
    assert_eq!(golfers.len(), all, "{:?}", p.open_blocks());
}

/// A game id linked to Eric while it is up with an open Set: Eric's
/// own open Set closes, the game id's Set goes on as his, and a club
/// change then rolls it, so later shots land in the new Set.
#[test]
fn a_link_while_the_game_id_is_up_leaves_one_open_set() {
    let config = unlinked();
    let (mut p, start) = Practice::with_config(
        config.clone(),
        turn(Some("flighthook.e"), Some(Club::Driver)),
        0,
    );
    let eric = opened(&start)[0].clone();
    hit(&mut p);
    let game = opened(&p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 1))[0].clone();
    assert_eq!(game.number, 1);
    hit(&mut p);
    hit(&mut p);
    config.write().golfers[0]
        .identifiers
        .push("pureplay.local-3".into());
    let saved = p.rekey(2);
    assert_eq!(saved, vec![BlockChange::Closed(eric.id.clone())]);
    assert_eq!(p.open_blocks().len(), 1);
    assert_eq!(p.current().map(|b| b.id.clone()), Some(game.id.clone()));
    // The club change rolls the game id's Set, Eric's run counting on.
    let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron8)), 3);
    assert_eq!(closed(&next), vec![game.id.clone()]);
    let eight = opened(&next)[0].clone();
    assert_eq!((eight.number, eight.club), (2, Some(Club::Iron8)));
    for _ in 0..6 {
        assert_eq!(hit(&mut p), Some(eight.id.clone()));
    }
    let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron9)), 4);
    assert_eq!(closed(&next), vec![eight.id.clone()]);
    let nine = opened(&next)[0].clone();
    assert_eq!(nine.number, 3);
    assert_eq!(hit(&mut p), Some(nine.id));
    assert_eq!(p.open_blocks().len(), 1);
    one_per_golfer(&p);
}

/// The golfer a game id is linked to is removed while the game id is
/// up: its Set goes on as the game id's own, and a club change opens
/// Set 2, never a second Set 1.
#[test]
fn removing_a_linked_golfer_keeps_one_open_set_for_the_game_id() {
    let config = linked();
    let (mut p, _) = Practice::with_config(
        config.clone(),
        turn(Some("pureplay.local-3"), Some(Club::Iron7)),
        0,
    );
    let one = hit(&mut p).expect("block");
    config.write().golfers.retain(|g| g.name != "Eric");
    assert!(p.rekey(1).is_empty(), "nothing on the wire changes");
    let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron8)), 2);
    assert_eq!(closed(&next), vec![one.clone()]);
    let two = opened(&next)[0].clone();
    assert_eq!((two.number, two.club), (2, Some(Club::Iron8)));
    assert_eq!(hit(&mut p), Some(two.id.clone()));
    assert_eq!(p.open_blocks().len(), 1);
    one_per_golfer(&p);
}

/// A game id moved from Eric to Sam while Sam is up: the game id's Set
/// closes, Sam's stays open, and the game id coming up continues Sam's
/// run.
#[test]
fn moving_a_game_id_to_another_golfer_merges_into_their_run() {
    let config = linked();
    let (mut p, _) = Practice::with_config(
        config.clone(),
        turn(Some("pureplay.local-3"), Some(Club::Iron7)),
        0,
    );
    let game = hit(&mut p).expect("block");
    let sam = opened(&p.turn(turn(Some("flighthook.s"), Some(Club::Iron7)), 1))[0].clone();
    hit(&mut p);
    {
        let mut c = config.write();
        c.golfers[0].identifiers.clear();
        c.golfers[1].identifiers.push("pureplay.local-3".into());
    }
    assert_eq!(p.rekey(2), vec![BlockChange::Closed(game)]);
    let up = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 3);
    let b = &opened(&up)[0];
    assert_eq!((b.id.as_str(), b.number), (sam.id.as_str(), 1));
    assert_eq!(hit(&mut p), Some(sam.id.clone()));
    let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Driver)), 4);
    assert_eq!(opened(&next)[0].number, 2);
    // Eric gets a block of his own again when he comes up.
    let eric = p.turn(turn(Some("flighthook.e"), Some(Club::Iron7)), 5);
    assert_eq!(opened(&eric)[0].number, 2, "Eric's own count goes on");
    one_per_golfer(&p);
}

/// An empty duplicate is dropped and gives its number back; the Set
/// with shots is kept and republished under the turn's raw id.
#[test]
fn an_empty_duplicate_is_dropped_without_a_gap() {
    let config = unlinked();
    let (mut p, _) = Practice::with_config(
        config.clone(),
        turn(Some("flighthook.e"), Some(Club::Iron7)),
        0,
    );
    let eric = hit(&mut p).expect("block");
    let game = opened(&p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 1))[0].clone();
    config.write().golfers[0]
        .identifiers
        .push("pureplay.local-3".into());
    let saved = p.rekey(2);
    assert_eq!(saved.first(), Some(&BlockChange::Closed(game.id.clone())));
    let b = &opened(&saved)[0];
    assert_eq!(
        (
            b.id.as_str(),
            b.number,
            b.golfer.as_ref().map(GolferId::as_str)
        ),
        (eric.as_str(), 1, Some("pureplay.local-3"))
    );
    assert_eq!(hit(&mut p), Some(eric.clone()));
    let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Driver)), 3);
    assert_eq!(opened(&next)[0].number, 2, "no gap");
    // A save that changes nothing changes nothing.
    assert!(p.rekey(4).is_empty());
}

#[test]
fn a_trigger_counts_for_the_golfer_up() {
    let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
    assert_eq!(
        p.current().map(|b| b.id.clone()),
        Some(opened(&start)[0].id.clone())
    );
    assert_eq!(hit(&mut p), Some(opened(&start)[0].id.clone()));
}
