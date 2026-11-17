//! Pairing, refusals, reconnects, the round's rows, state requests and
//! pings.

use super::*;

/// No saved token: the card asks, waits in Pureplay's queue, counts the
/// prompt down, saves the token Pureplay gives, subscribes and follows the
/// round. Every frame it sends is text; the server's binary and text
/// frames are both read. flighthook holds a left-handed golfer, so the
/// snapshot's right hand and its club go out in one select.
#[test]
fn a_new_card_pairs_with_a_prompt_and_follows_the_round() {
    let mut rig = Rig::new();
    rig.turn.handedness = Some(Handedness::Left);
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None, "no token: the prompt");
    rig.shows_row(Status::Starting, "pairing", WAITING);

    rig.send(&json!({ "type": "pairing.pending", "data": { "expiresInSec": 3 } }));
    rig.shows_row(
        Status::Starting,
        "pairing",
        "Approve flighthook in Pureplay (3 s)",
    );
    rig.shows_row(
        Status::Starting,
        "pairing",
        "Approve flighthook in Pureplay (2 s)",
    );

    rig.paired("PPA-NEW-TOKEN");
    rig.subscribe(&round(), 1);
    rig.shows(Status::Connected, |t| {
        t.get("name").map(String::as_str) == Some("Joe")
            && t.get("club").map(String::as_str) == Some("7I")
            && t.get("handed").map(String::as_str) == Some("rh")
            && t.get("hole").map(String::as_str) == Some("7, par 4")
            && t.get("ready").map(String::as_str) == Some("true")
            && t.get("pureplay").map(String::as_str) == Some("1.4.2")
            && !t.contains_key("blockers")
            && !t.contains_key("pairing")
            && !t.contains_key("missed_events")
    });
    assert_eq!(rig.token(), Some(Token::new("PPA-NEW-TOKEN")));
    rig.caught_up();
    // The snapshot's hand and club, together in one `golfer.select`.
    let selects = rig.selects();
    assert_eq!(selects.len(), 1, "one select carries both: {selects:?}");
    assert_eq!(selects[0].club, Some(Some(Club::Iron7)));
    assert_eq!(selects[0].handedness, Some(Handedness::Right));

    // Another app's club change, in a text frame.
    rig.send_text(&event_json(
        "player.club_changed",
        2,
        &json!({ "playerId": 1, "shotId": "shot-1", "previous": "Iron7", "current": "Iron8",
                 "previousClubId": "b-7i", "currentClubId": "b-8i", "reason": "api_command" }),
    ));
    rig.frp_until(|b| matches!(b, FrpBody::GolferSelect(s) if s.club == Some(Some(Club::Iron8))));
    rig.shows_row(Status::Connected, "club", "8I");

    // Not ready while the ball is in the air.
    rig.event(
        "shot.ready_changed",
        3,
        &json!({ "shotId": "shot-1", "playerId": 1, "ready": false, "controllable": false,
                 "allowedCommands": [], "blockers": ["shot_in_progress"] }),
    );
    rig.shows(Status::Connected, |t| {
        t.get("ready").map(String::as_str) == Some("false")
            && t.get("blockers").map(String::as_str) == Some("shot in progress")
    });

    // A gap in `seq`: counted, and the card asks for the state.
    rig.event("hole.started", 6, &json!({ "holeNumber": 8, "par": 3 }));
    let get = rig.state_get();
    rig.shows_row(Status::Connected, "missed_events", "1");
    rig.shows_row(Status::Connected, "hole", "8, par 3");
    rig.send(
        &json!({ "type": "event", "event": "state.snapshot", "requestId": get["id"],
                       "seq": 7, "time": "t", "data": round() }),
    );
    rig.shows_row(Status::Connected, "hole", "7, par 4");
}

/// A drop without a close frame (Pureplay closed, settings changed):
/// Reconnecting, then a new link pairs with the saved token.
#[test]
fn a_drop_without_a_close_frame_reconnects_with_the_saved_token() {
    let mut rig = connected("PPA-KEEP");
    rig.drop_link();
    rig.shows(Status::Reconnecting, BTreeMap::is_empty);
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some("PPA-KEEP"), "the token is kept");
    rig.paired("PPA-KEEP");
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "name", "Joe");
}

/// A token file that cannot be written: an `error` on the card, the link
/// goes on, and a reconnect pairs with the token Pureplay gave.
#[test]
fn a_token_that_cannot_be_saved_is_an_error_and_kept_for_the_session() {
    let blocker = std::env::temp_dir().join(format!(
        "flighthook-pp-not-a-folder-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&blocker, b"a file").expect("file");
    let _blocker = Folder(blocker.clone());
    let mut rig = Rig::new();
    rig.driver = Accessory::new(Config {
        address: Some(ADDR.into()),
        token_file: Some(blocker.join(token::FILE_NAME)),
    })
    .expect("config");
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    rig.paired("PPA-UNSAVED");
    rig.subscribe(&round(), 1);
    rig.shows(Status::Connected, |t| {
        t.get("error")
            .is_some_and(|e| e.starts_with("Couldn't save the Pureplay pairing: "))
            && t.get("club").map(String::as_str) == Some("7I")
    });
    rig.drop_link();
    rig.link_ends();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some("PPA-UNSAVED"), "no new prompt");
    rig.paired("PPA-UNSAVED");
    rig.subscribe(&round(), 1);
    rig.shows(Status::Connected, |t| {
        t.get("error")
            .is_some_and(|e| e.starts_with("Couldn't save the Pureplay pairing: "))
            && t.contains_key("club")
    });
    let _ = std::fs::remove_file(&blocker);
}

/// Nothing listening: the connect's error on the card, and the card keeps
/// trying.
#[test]
fn a_refused_connection_is_reconnecting_with_no_row() {
    let mut rig = Rig::new();
    rig.unreachable();
    assert_eq!(rig.status, Status::Starting);
    assert_eq!(
        rig.view.keys().collect::<Vec<_>>(),
        ["error"],
        "only the connect's error"
    );
    assert_eq!(rig.delay, Some(MIN_BACKOFF));
    rig.unreachable();
    assert_eq!(rig.delay, Some(MIN_BACKOFF * 2));
}

#[test]
fn too_many_connections_before_hello_backs_off() {
    let mut rig = Rig::new();
    rig.next();
    rig.close(4429);
    rig.link_ends();
    assert_eq!(rig.view.get("pairing").map(String::as_str), Some(TOO_MANY));
    assert_eq!(rig.delay, Some(MIN_BACKOFF));
    // Tried again after the backoff.
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
}

#[test]
fn a_hello_of_another_protocol_or_version_is_an_error_and_retried() {
    let mut rig = Rig::new();
    rig.next();
    rig.hello_as("somethingelse", 1);
    rig.link_ends();
    assert_eq!(
        rig.view.get("error").map(String::as_str),
        Some("Not a Pureplay Accessory server")
    );
    assert_eq!(rig.delay, Some(VERSION_RETRY));
    rig.next();
    rig.hello_as("ppaccessory", 2);
    rig.link_ends();
    assert_eq!(
        rig.view.get("error").map(String::as_str),
        Some("Pureplay speaks Accessory version 2; this flighthook speaks 1")
    );
    // Pureplay gone (a drop before hello): the connect's error replaces
    // the version's.
    rig.next();
    rig.drop_link();
    rig.link_ends();
    assert_eq!(
        rig.view.get("error").map(String::as_str),
        Some("connection lost")
    );
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
}

/// `bad_request` to `pair`: another app on this address waits for its
/// prompt. The card closes and asks again later.
#[test]
fn another_app_waiting_closes_and_asks_again_later() {
    let mut rig = Rig::new();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    rig.refuse(Some(protocol::PAIR_ID), "bad_request", &json!({}));
    let pipe = Rc::clone(&rig.pipe);
    rig.link_ends();
    assert_eq!(
        rig.view.get("pairing").map(String::as_str),
        Some(ANOTHER_APP)
    );
    assert!(pipe.borrow().closed, "the card closes the socket");
    assert_eq!(rig.delay, Some(BAD_REQUEST_RETRY));
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None, "asked again");
}

// -- What follows a link's end (`retry_delay`) ------------------------------------

/// The driver's retry delay after a link ended with `error`, having held
/// for `held` (`None`: a connect that failed).
fn retry(rig: &mut Rig, error: &Error, held: Option<Duration>) -> Option<Duration> {
    let ended = SessionEnd {
        error: Some(error),
        connected_for: held,
        attempts: 1,
    };
    let mut backoff = HostBackoff::default();
    let driver = &mut rig.driver;
    rig.host
        .with(|_, out| driver.retry_delay(&ended, &mut backoff, out))
}

/// A refusal, a removal or six `bad_request`s end in [`Error::Refused`]: no
/// retry until the user's Reconnect, after a long link too.
#[test]
fn a_stopped_card_waits_for_reconnect() {
    let mut rig = Rig::new();
    assert_eq!(retry(&mut rig, &Error::Refused, None), None);
    assert_eq!(
        retry(&mut rig, &Error::Refused, Some(STABLE)),
        None,
        "after a long link too"
    );
}

/// Pureplay's cooldown: the next connect waits until its end; whether it
/// then tries once more is the cooldown's own `retry`.
#[test]
fn a_cooldown_waits_until_its_end_then_retries_only_when_it_says() {
    let wait = Duration::from_secs(2);
    for retry in [true, false] {
        let mut rig = Rig::new();
        rig.ended(Error::Cooldown { wait, retry });
        assert_eq!(rig.delay, Some(Duration::ZERO), "the connect waits it out");
        let start = rig.host.now();
        rig.next();
        assert!(rig.host.now() - start >= wait, "{retry}: until its end");
        assert_eq!(rig.session.is_some(), retry, "{retry}");
        if !retry {
            assert_eq!(rig.delay, None);
            assert_eq!(rig.status, Status::Disconnected);
            assert_eq!(
                rig.view.get("pairing").map(String::as_str),
                Some(PRESS_RECONNECT)
            );
        }
    }
}

/// Lost, or too many apps before `hello`: a wait that doubles from
/// [`MIN_BACKOFF`] up to [`MAX_BACKOFF`].
#[test]
fn a_lost_connection_reconnects_after_a_doubling_backoff() {
    let mut rig = Rig::new();
    let mut want = MIN_BACKOFF;
    for n in 0..8 {
        let error = if n % 2 == 0 {
            Error::Lost("connection lost".into())
        } else {
            Error::TooMany
        };
        assert_eq!(retry(&mut rig, &error, None), Some(want), "{error:?}");
        want = (want * 2).min(MAX_BACKOFF);
    }
    assert_eq!(want, MAX_BACKOFF, "the cap is reached");
}

/// A link that held for [`STABLE`] starts the backoff over; one that ended
/// sooner keeps doubling it.
#[test]
fn a_connection_that_held_for_stable_starts_the_backoff_over() {
    let mut rig = Rig::new();
    let lost = Error::Lost("connection lost".into());
    assert_eq!(retry(&mut rig, &lost, None), Some(MIN_BACKOFF));
    let short = STABLE - Duration::from_millis(1);
    assert_eq!(retry(&mut rig, &lost, Some(short)), Some(MIN_BACKOFF * 2));
    assert_eq!(
        retry(&mut rig, &lost, Some(STABLE)),
        Some(MIN_BACKOFF),
        "held for STABLE"
    );
}

/// A retry the card set itself keeps its own wait; a dead token's one pair
/// without it goes at once. Neither touches the backoff.
#[test]
fn a_set_retry_keeps_its_wait_and_a_pair_without_the_token_goes_at_once() {
    let mut rig = Rig::new();
    let delay = Duration::from_millis(1234);
    let set = Error::Retry {
        delay,
        reason: "later".into(),
    };
    assert_eq!(retry(&mut rig, &set, None), Some(delay));
    assert_eq!(retry(&mut rig, &Error::Again, None), Some(Duration::ZERO));
    assert_eq!(
        retry(&mut rig, &Error::Lost("connection lost".into()), None),
        Some(MIN_BACKOFF),
        "neither touched the backoff"
    );
}

// -- Refusals ------------------------------------------------------------------------

/// Each refusal of a `pair` that closes the socket: Disconnected, the
/// reason in the `pairing` row, "Press Reconnect" as the `error`, and no
/// new attempt by itself.
#[test]
fn a_refusal_is_disconnected_with_its_reason_and_not_retried() {
    for (code, close, text) in [
        ("forbidden_scope", 4403, ASK_OFF),
        ("unauthorised", 4403, DENIED),
        ("unauthorised", 4408, TIMED_OUT),
    ] {
        let mut rig = Rig::new();
        rig.next();
        rig.hello();
        assert_eq!(rig.pair(), None);
        rig.refuse(Some(protocol::PAIR_ID), code, &json!({}));
        rig.close(close);
        rig.shows_row(Status::Disconnected, "pairing", text);
        assert_eq!(
            rig.view.get("error").map(String::as_str),
            Some(PRESS_RECONNECT)
        );
        assert_eq!(rig.delay, None, "{code} {close}");
    }
}

/// A refused card waits for Reconnect: denied while it asks, and refused
/// on a paired link (the token kept, so only the wait keeps it from
/// asking).
#[test]
fn a_refused_card_does_not_connect_again() {
    let mut rig = Rig::new();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    rig.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
    rig.close(4403);
    rig.shows_row(Status::Disconnected, "pairing", DENIED);
    assert_eq!(rig.delay, None);

    let mut rig = connected("PPA-KEPT");
    rig.close(4403);
    rig.shows_row(Status::Disconnected, "pairing", DENIED);
    assert!(rig.token().is_some());
    assert_eq!(rig.delay, None);
}

/// Pureplay's cooldown: the wait counts down in the `pairing` row, then the
/// card asks once more by itself; a second cooldown answer is not retried.
/// No session has come up, so the card stays Starting while it waits.
#[test]
fn a_cooldown_counts_down_and_retries_once() {
    let mut rig = Rig::new();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    rig.refuse(
        Some(protocol::PAIR_ID),
        "rate_limited",
        &json!({ "retryAfterMs": 1500 }),
    );
    rig.close(4429);
    rig.link_ends();
    let refused = rig.host.now();
    rig.next();
    assert!(
        rig.host.now() - refused >= Duration::from_millis(1500),
        "after the wait"
    );
    let rows = pairing_rows(&rig);
    for text in [
        "Pureplay asks to wait (0:02)",
        "Pureplay asks to wait (0:01)",
    ] {
        assert!(rows.iter().any(|r| r == text), "{text}: {rows:?}");
    }
    assert_eq!(rig.status, Status::Starting);
    rig.hello();
    assert_eq!(rig.pair(), None, "the attempt, once more");
    rig.refuse(
        Some(protocol::PAIR_ID),
        "rate_limited",
        &json!({ "retryAfterMs": 200 }),
    );
    rig.close(4429);
    rig.link_ends();
    let counted = pairing_rows(&rig).len();
    rig.next();
    assert!(
        pairing_rows(&rig)[counted..]
            .iter()
            .any(|r| r.starts_with("Pureplay asks to wait")),
        "the second wait counts down too"
    );
    rig.shows_row(Status::Disconnected, "pairing", PRESS_RECONNECT);
    assert_eq!(rig.delay, None);
}

/// Every `pairing` row the card showed, in order.
fn pairing_rows(rig: &Rig) -> Vec<String> {
    rig.statuses()
        .iter()
        .filter_map(|s| s.telemetry.get("pairing").cloned())
        .collect()
}

/// A8: while the card waits out a cooldown the host shows Starting before
/// the first session and Reconnecting after one, never Disconnected; the
/// `pairing` row counts down meanwhile.
#[test]
fn a_cooldown_counts_down_while_starting_then_reconnecting() {
    let cooldown = |rig: &mut Rig, token: Option<&str>| {
        rig.hello();
        assert_eq!(rig.pair().as_deref(), token);
        rig.refuse(
            Some(protocol::PAIR_ID),
            "rate_limited",
            &json!({ "retryAfterMs": 3000 }),
        );
        rig.close(4429);
        rig.link_ends();
        let counted = pairing_rows(rig).len();
        let status = rig.status;
        rig.next();
        let rows = pairing_rows(rig)[counted..].to_vec();
        for text in [
            "Pureplay asks to wait (0:03)",
            "Pureplay asks to wait (0:02)",
            "Pureplay asks to wait (0:01)",
        ] {
            assert!(rows.iter().any(|r| r == text), "{text}: {rows:?}");
        }
        assert_eq!(rig.status, status, "the wait leaves the status alone");
        assert!(rig.session.is_some(), "then it tries once more");
    };
    let mut rig = Rig::new();
    rig.next();
    cooldown(&mut rig, None);
    assert_eq!(rig.status, Status::Starting, "before the first session");

    let mut rig = connected("PPA-WAIT");
    rig.drop_link();
    rig.link_ends();
    rig.next();
    cooldown(&mut rig, Some("PPA-WAIT"));
    assert_eq!(rig.status, Status::Reconnecting, "after a session");
}

/// A saved token Pureplay does not know: deleted, and the card pairs
/// once without one.
#[test]
fn a_dead_token_is_deleted_and_the_card_pairs_without_one() {
    let mut rig = Rig::with_token("PPA-DEAD");
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some("PPA-DEAD"));
    rig.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
    rig.close(4401);
    rig.link_ends();
    assert_eq!(rig.delay, Some(Duration::ZERO), "at once");
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None, "paired again without a token");
    rig.shows_row(Status::Starting, "pairing", WAITING);
    assert_eq!(rig.token(), None);
}

/// Removed in Pureplay's Settings mid-session: an unprompted
/// `unauthorised`, then `4401`. The token goes, and nothing is retried.
#[test]
fn a_removal_in_pureplay_deletes_the_token_and_waits_for_reconnect() {
    let mut rig = connected("PPA-GONE");
    rig.refuse(None, "unauthorised", &json!({}));
    rig.close(4401);
    rig.shows_row(Status::Disconnected, "pairing", REMOVED);
    assert_eq!(rig.token(), None);
    assert_eq!(rig.delay, None);
}

/// Denied, and the socket drops before a close frame arrives: the response
/// decides. Disconnected, and the player is not asked again.
#[test]
fn a_denial_without_a_close_frame_is_not_asked_again() {
    let mut rig = Rig::new();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    rig.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
    rig.drop_link();
    rig.shows_row(Status::Disconnected, "pairing", DENIED);
    assert_eq!(rig.delay, None);
}

/// Removed in Pureplay mid-session, the socket dropped without a close
/// frame: the token goes, Disconnected, no reconnect.
#[test]
fn a_removal_without_a_close_frame_is_not_asked_again() {
    let mut rig = connected("PPA-GONE");
    rig.refuse(None, "unauthorised", &json!({}));
    rig.drop_link();
    rig.shows_row(Status::Disconnected, "pairing", REMOVED);
    assert_eq!(rig.token(), None);
    assert_eq!(rig.delay, None);
}

/// The other refusals without a close frame: dropped, or left open (the
/// card waits a moment, then closes and decides itself).
#[test]
fn other_refusals_without_a_close_frame_decide_by_the_response() {
    /// The link drops, or the card closes it after its grace.
    fn ends(rig: &mut Rig, keep_open: bool) {
        let pipe = Rc::clone(&rig.pipe);
        if !keep_open {
            rig.drop_link();
        }
        rig.link_ends();
        if keep_open {
            assert!(pipe.borrow().closed, "the card closes");
        }
    }
    for keep_open in [false, true] {
        // forbidden_scope: Ask about new apps is off.
        let mut rig = Rig::new();
        rig.next();
        rig.hello();
        assert_eq!(rig.pair(), None);
        rig.refuse(Some(protocol::PAIR_ID), "forbidden_scope", &json!({}));
        ends(&mut rig, keep_open);
        rig.shows_row(Status::Disconnected, "pairing", ASK_OFF);

        // rate_limited: the cooldown.
        let mut rig = Rig::new();
        rig.next();
        rig.hello();
        assert_eq!(rig.pair(), None);
        rig.refuse(
            Some(protocol::PAIR_ID),
            "rate_limited",
            &json!({ "retryAfterMs": 60_000 }),
        );
        ends(&mut rig, keep_open);
        rig.next();
        assert!(
            pairing_rows(&rig)
                .iter()
                .any(|r| r == "Pureplay asks to wait (1:00)"),
            "{keep_open}"
        );

        // unauthorised to a saved token: the dead-token rule.
        let mut rig = Rig::with_token("PPA-STALE");
        rig.next();
        rig.hello();
        assert_eq!(rig.pair().as_deref(), Some("PPA-STALE"));
        rig.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
        ends(&mut rig, keep_open);
        rig.next();
        rig.hello();
        assert_eq!(rig.pair(), None, "paired once without a token");
        assert_eq!(rig.token(), None);
    }
}

// -- Pairing deadlines ---------------------------------------------------------------

/// A `pair` with a token that gets no answer: the card gives up and
/// reconnects.
#[test]
fn an_unanswered_pair_with_a_token_gives_up() {
    let mut rig = Rig::with_token("PPA-HANG");
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some("PPA-HANG"));
    let asked = rig.host.now();
    rig.link_ends();
    assert!(rig.host.now() - asked >= PAIR_TIMEOUT, "after the timeout");
    assert_eq!(rig.delay, Some(MIN_BACKOFF));
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some("PPA-HANG"), "token kept");
}

/// `bad_request` six times in a row: the card stops asking and shows
/// Pureplay's message.
#[test]
fn bad_request_six_times_stops_with_pureplays_message() {
    let mut rig = Rig::new();
    for n in 1..=BAD_REQUEST_TRIES {
        rig.next();
        rig.hello();
        assert_eq!(rig.pair(), None, "try {n}");
        rig.refuse(
            Some(protocol::PAIR_ID),
            "bad_request",
            &json!({ "message": "A request is already pending." }),
        );
        let pipe = Rc::clone(&rig.pipe);
        rig.link_ends();
        assert!(pipe.borrow().closed, "try {n}: the card closes");
        if n < BAD_REQUEST_TRIES {
            assert_eq!(rig.status, Status::Starting, "try {n}");
            assert_eq!(
                rig.view.get("pairing").map(String::as_str),
                Some(ANOTHER_APP)
            );
        }
    }
    rig.shows_row(
        Status::Disconnected,
        "pairing",
        "A request is already pending.",
    );
    assert_eq!(rig.delay, None);
}

/// A drop without a close frame while Pureplay's prompt is up: the
/// `pairing` row goes, then a new `pair` without a token (the automatic
/// one was not spent).
#[test]
fn a_drop_while_the_prompt_is_up_asks_again() {
    let mut rig = Rig::new();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    rig.send(&json!({ "type": "pairing.pending", "data": { "expiresInSec": 60 } }));
    rig.shows_row(
        Status::Starting,
        "pairing",
        "Approve flighthook in Pureplay (60 s)",
    );
    rig.drop_link();
    rig.link_ends();
    rig.shows(Status::Starting, |t| !t.contains_key("pairing"));
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None, "asked again");
}

/// A dead token after the one automatic pair of this start was spent:
/// deleted, Disconnected, "Removed in Pureplay", no new prompt by itself.
#[test]
fn a_dead_token_after_the_automatic_pair_waits_for_reconnect() {
    let mut rig = Rig::new();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    rig.paired("PPA-ONCE");
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "club", "7I");
    rig.drop_link();
    rig.link_ends();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some("PPA-ONCE"));
    rig.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
    rig.close(4401);
    rig.shows_row(Status::Disconnected, "pairing", REMOVED);
    assert_eq!(rig.token(), None);
    assert_eq!(rig.delay, None);
}

// -- The round's rows ----------------------------------------------------------------

/// Club in: a turn to a second local player gives their name, hand and
/// club; a repeated club gives nothing; a remote player's turn and club
/// give nothing.
#[test]
fn club_and_hand_follow_the_active_local_player() {
    let mut rig = connected("PPA-CLUB");
    let before = rig.selects().len();
    rig.event(
        "player.club_changed",
        2,
        &json!({ "playerId": 1, "current": "Iron7", "currentClubId": "b-7i", "reason": "auto" }),
    );
    rig.caught_up();
    assert_eq!(rig.selects().len(), before, "the club flighthook holds");

    rig.event(
        "turn.changed",
        3,
        &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "holeNumber": 7, "reason": "next_player" }),
    );
    rig.frp_until(|b| matches!(b, FrpBody::GolferSelect(s) if s.club == Some(Some(Club::Driver))));
    rig.shows(Status::Connected, |t| {
        t.get("name").map(String::as_str) == Some("Ann")
            && t.get("handed").map(String::as_str) == Some("lh")
            && t.get("club").map(String::as_str) == Some("DR")
    });
    assert!(
        rig.selects()
            .iter()
            .any(|s| s.handedness == Some(Handedness::Left))
    );

    let before = rig.selects().len();
    rig.event(
        "turn.changed",
        4,
        &json!({ "previousPlayerId": 2, "currentPlayerId": 3, "holeNumber": 7, "reason": "next_player" }),
    );
    rig.event(
        "player.club_changed",
        5,
        &json!({ "playerId": 3, "current": "Wood3", "reason": "user_selected" }),
    );
    rig.shows(Status::Connected, |t| !t.contains_key("name"));
    rig.caught_up();
    let given = rig.selects()[before..].to_vec();
    assert!(given.is_empty(), "a remote player gives nothing: {given:?}");
}

/// Five gaps in a burst: one `state.get`, the others wait for its answer.
#[test]
fn gaps_in_a_burst_ask_for_the_state_once() {
    let mut rig = connected("PPA-GAPS");
    for seq in [3u64, 5, 7, 9, 11] {
        rig.event("hole.started", seq, &json!({ "holeNumber": 8, "par": 3 }));
    }
    let get = rig.state_get();
    rig.run_for(SNAPSHOT_TIMEOUT / 2);
    assert!(
        rig.pipe
            .borrow()
            .from_card
            .iter()
            .all(|m| m["type"] != "state.get"),
        "one state.get outstanding at a time"
    );
    rig.shows_row(Status::Connected, "missed_events", "5");
    // Answered: the next gap asks again.
    rig.send(
        &json!({ "type": "event", "event": "state.snapshot", "requestId": get["id"],
                 "seq": 12, "time": "t", "data": round() }),
    );
    rig.event("hole.started", 14, &json!({ "holeNumber": 9, "par": 4 }));
    let next = rig.state_get();
    assert_ne!(next["id"], get["id"], "a new request id");
}

/// `context.invalidated`: the round's rows go and the card asks for the
/// state; the answer brings them back.
#[test]
fn context_invalidated_clears_the_rows_and_asks_for_the_state() {
    let mut rig = connected("PPA-CTX");
    rig.event(
        "context.invalidated",
        2,
        &json!({ "reason": "loading", "matchId": null }),
    );
    let get = rig.state_get();
    rig.shows(Status::Connected, |t| {
        ["name", "club", "handed", "hole", "blockers"]
            .iter()
            .all(|k| !t.contains_key(*k))
            && t.get("ready").map(String::as_str) == Some("false")
            && t.get("pureplay").map(String::as_str) == Some("1.4.2")
    });
    rig.send(
        &json!({ "type": "event", "event": "state.snapshot", "requestId": get["id"],
                 "seq": 3, "time": "t", "data": round() }),
    );
    rig.shows(Status::Connected, |t| {
        t.get("club").map(String::as_str) == Some("7I")
            && t.get("hole").map(String::as_str) == Some("7, par 4")
    });
}

// -- The token never leaks -----------------------------------------------------------

/// Whether `token` shows anywhere the card puts it out: a bus message (raw
/// frames included), a status row, a log line.
fn leaks(rig: &Rig, token: &str) -> Vec<String> {
    let mut out: Vec<String> = rig
        .record
        .iter()
        .map(|e| format!("{e:?}"))
        .filter(|t| t.contains(token))
        .collect();
    out.extend(rig.logs.iter().filter(|l| l.contains(token)).cloned());
    out.extend(rig.view.values().filter(|v| v.contains(token)).cloned());
    out
}

/// A link with a known token, through a prompt, a reconnect and a removal:
/// the token is in no log line, no bus message (raw frames, status rows:
/// what the WebSocket and the Log tab are built from), no `Debug` of the
/// parsed frame; only in the token file.
#[test]
fn the_token_appears_only_in_the_token_file() {
    let token = format!("PPA-LEAK-{}", uuid::Uuid::new_v4().simple());
    let mut rig = Rig::new();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    rig.paired(&token);
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "club", "7I");
    rig.event(
        "player.club_changed",
        2,
        &json!({ "playerId": 1, "current": "Iron8", "reason": "user_selected" }),
    );
    rig.frp_until(|b| matches!(b, FrpBody::GolferSelect(s) if s.club == Some(Some(Club::Iron8))));
    rig.drop_link();
    rig.link_ends();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some(token.as_str()));
    rig.paired(&token);
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "club", "7I");
    rig.refuse(None, "unauthorised", &json!({}));
    rig.close(4401);
    rig.shows_row(Status::Disconnected, "pairing", REMOVED);

    // The token file held it until the removal deleted it.
    assert_eq!(rig.token(), None);
    assert_eq!(leaks(&rig, &token), Vec::<String>::new());
    assert!(
        rig.raw_text().iter().any(|f| f.contains(token::REDACTED)),
        "the frames are logged, redacted"
    );
    // The parsed `paired` frame's Debug.
    let frame = json!({ "type": "paired", "data": {
        "token": token, "clientId": "c", "scopes": ["read"] } })
    .to_string();
    let parsed = format!("{:?}", protocol::parse(&frame));
    assert!(parsed.contains(token::REDACTED), "{parsed}");
    assert!(!parsed.contains(&token), "{parsed}");

    // While paired, only the token file has it.
    let mut rig = connected(&token);
    assert_eq!(leaks(&rig, &token), Vec::<String>::new());
    let file = std::fs::read_to_string(rig.tokens.path()).expect("token file");
    assert!(file.contains(&token));
    rig.caught_up();
}

/// A `paired` frame whose `scopes` is `null`: the token is learned before
/// the frame is logged, and the card still pairs.
#[test]
fn an_unreadable_paired_frame_does_not_leak_the_token() {
    let token = format!("PPA-SCRATCH-{}", uuid::Uuid::new_v4().simple());
    let mut rig = Rig::new();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    rig.send(&json!({ "type": "paired", "data": {
        "token": token, "clientId": "c", "scopes": null } }));
    let sub = rig.recv();
    assert_eq!(sub["type"], "subscribe", "paired: {sub}");
    assert_eq!(rig.token(), Some(Token::new(token.as_str())));
    assert!(
        rig.raw_text()
            .iter()
            .any(|f| f.contains("\"type\":\"paired\"") && f.contains(token::REDACTED)),
        "the paired frame is logged, redacted"
    );
    assert_eq!(leaks(&rig, &token), Vec::<String>::new());
}

/// A refused `pair` whose message repeats the saved token: the frame and
/// the log line about the refusal show `<redacted>`.
#[test]
fn a_refused_pair_repeating_the_token_does_not_leak_it() {
    let token = format!("PPA-ECHO-{}", uuid::Uuid::new_v4().simple());
    let mut rig = Rig::with_token(&token);
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some(token.as_str()));
    rig.refuse(
        Some(protocol::PAIR_ID),
        &format!("bad_{token}"),
        &json!({ "message": format!("token {token} is not known") }),
    );
    rig.close(4401);
    rig.link_ends();
    rig.next();
    rig.hello();
    assert_eq!(rig.pair(), None);
    assert!(
        rig.logs
            .iter()
            .any(|l| l.contains("pair refused") && l.contains(token::REDACTED)),
        "the refusal is logged, redacted"
    );
    assert_eq!(leaks(&rig, &token), Vec::<String>::new());
}

// -- Pureplay restarted, events offered, frames that do not read ---------------------

/// A new `serverInstanceId` (Pureplay restarted) drops the cached round; the
/// same one keeps it.
#[test]
fn a_new_server_instance_drops_the_cached_round() {
    const DROPPED: &str = "Pureplay restarted, cached state dropped";
    let dropped = |rig: &Rig| rig.logs.iter().filter(|l| l.contains(DROPPED)).count();
    let mut rig = connected("PPA-INST");
    for (instance, times) in [("s-1", 0), ("s-2", 1)] {
        rig.drop_link();
        rig.link_ends();
        rig.next();
        rig.hello_from(instance);
        assert_eq!(rig.pair().as_deref(), Some("PPA-INST"));
        rig.paired("PPA-INST");
        rig.subscribe(&round(), 1);
        rig.shows_row(Status::Connected, "club", "7I");
        assert_eq!(dropped(&rig), times, "{instance}");
    }
}

/// The card subscribes to the events of its list that Pureplay offers; a
/// list offering none of them is the connect's `error`, and nothing is
/// subscribed.
#[test]
fn only_the_offered_events_are_subscribed() {
    let mut rig = Rig::new();
    rig.next();
    rig.hello_with(
        "ppaccessory",
        1,
        "s-1",
        &[
            "state.snapshot",
            "player.club_changed",
            "hole.started",
            "turn.changed",
            "lie.changed",
        ],
    );
    assert_eq!(rig.pair(), None);
    rig.paired("PPA-SUBSET");
    rig.subscribe_to(
        &[
            "hole.started",
            "turn.changed",
            "player.club_changed",
            "lie.changed",
        ],
        &round(),
        1,
    );
    rig.shows_row(Status::Connected, "club", "7I");

    let mut rig = Rig::with_token("PPA-NONE");
    rig.next();
    rig.hello_with(
        "ppaccessory",
        1,
        "s-1",
        &["state.snapshot", "ruling.required"],
    );
    assert_eq!(rig.pair().as_deref(), Some("PPA-NONE"));
    rig.paired("PPA-NONE");
    let pipe = Rc::clone(&rig.pipe);
    rig.link_ends();
    assert_eq!(rig.view.get("error").map(String::as_str), Some(NO_EVENTS));
    assert_eq!(rig.status, Status::Starting);
    assert!(pipe.borrow().closed, "closed");
    assert!(pipe.borrow().from_card.is_empty(), "without a subscribe");
    assert!(rig.closed_by_card(), "the link is gone");
    assert_eq!(rig.delay, Some(VERSION_RETRY));
}

/// Malformed frames (not JSON, not UTF-8, `null`, wrong types) are ignored
/// and the round goes on; a frame that is not JSON is logged by its length
/// only.
#[test]
fn malformed_frames_are_ignored() {
    let junk = format!("junk-{}", uuid::Uuid::new_v4().simple());
    let mut rig = connected("PPA-JUNK");
    rig.send_frame(Frame::Text(format!("{junk} not json")));
    rig.send_frame(Frame::Binary(vec![0xff, 0xfe, 0x00]));
    rig.send_frame(Frame::Text("null".into()));
    rig.send_frame(Frame::Binary(b"{\"type\":null}".to_vec()));
    rig.send(&json!({ "type": "event", "event": null, "seq": null, "data": null }));
    rig.send(&json!({ "type": "response", "id": null, "ok": false, "error": "not an object" }));
    rig.send(&json!({ "type": "paired", "data": null }));
    rig.send(
        &json!({ "type": "event", "event": "player.club_changed", "seq": 2,
                 "data": { "playerId": "one", "current": 7 } }),
    );
    rig.event(
        "player.club_changed",
        3,
        &json!({ "playerId": 1, "current": "Iron8", "reason": "user_selected" }),
    );
    rig.frp_until(|b| matches!(b, FrpBody::GolferSelect(s) if s.club == Some(Some(Club::Iron8))));
    rig.shows_row(Status::Connected, "club", "8I");
    assert!(
        rig.logs.iter().any(|l| l.contains("not JSON (not shown)")),
        "the frame's length is logged"
    );
    assert_eq!(leaks(&rig, &junk), Vec::<String>::new(), "its text is not");
}

// -- Liveness and stopping -----------------------------------------------------------

/// A link that has gone quiet is pinged; two unanswered pings and it is
/// lost.
#[test]
fn a_quiet_link_is_lost_after_two_unanswered_pings() {
    let mut rig = connected("PPA-QUIET");
    let every = Duration::from_secs(1);
    rig.session.as_mut().expect("a link").ping_every = every;
    let pipe = Rc::clone(&rig.pipe);
    let quiet = rig.host.now();
    rig.link_ends();
    assert_eq!(pipe.borrow().pings, 2, "two pings");
    assert!(
        rig.host.now() - quiet >= every * 3,
        "lost only after two unanswered pings"
    );
    assert_eq!(rig.status, Status::Reconnecting);
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some("PPA-QUIET"));
}

/// Pings answered (any frame counts): the link holds.
#[test]
fn answered_pings_keep_the_link() {
    let mut rig = connected("PPA-ALIVE");
    let every = Duration::from_secs(1);
    rig.session.as_mut().expect("a link").ping_every = every;
    rig.pipe.borrow_mut().pong = true;
    rig.run_for(every * 10);
    let pings = rig.pipe.borrow().pings;
    assert!(pings >= 2, "pinged while quiet: {pings}");
    assert!(rig.session.is_some(), "no reconnect");
    assert_eq!(rig.status, Status::Connected);
}

/// The card stops at once: a stop ends Pureplay's cooldown wait and the
/// link's opening at their next tick, with no time gone by.
#[test]
fn the_card_stops_at_once() {
    let mut rig = Rig::new();
    rig.ended(Error::Cooldown {
        wait: DEFAULT_COOLDOWN,
        retry: true,
    });
    rig.host.stop().stop();
    let start = rig.host.now();
    let driver = &mut rig.driver;
    let begun = rig.host.connect_with(|ctx| driver.before_open(ctx));
    assert!(matches!(begun, Err(Error::Stopped)), "{begun:?}");
    assert_eq!(rig.host.now(), start, "during the cooldown");

    // Pureplay's prompt is up: the opening waits for `paired`.
    rig.hello();
    let pipe = Rc::clone(&rig.pipe);
    let driver = &mut rig.driver;
    let opened = rig
        .host
        .connect_with(|ctx| driver.open(ctx, FakeLink(pipe)).map(|_| ()));
    assert!(matches!(opened, Err(Error::Stopped)), "{opened:?}");
    assert_eq!(rig.host.now(), start, "during the opening");
}

// -- Configuration -------------------------------------------------------------------

/// Only another address restarts the card: an unset address is the
/// default one.
#[test]
fn only_an_address_change_restarts_the_card() {
    use flightrelay::config::DriverConfig as _;
    let at = |address: Option<&str>| Config {
        address: address.map(str::to_owned),
        token_file: None,
    };
    assert!(
        !Config::restart_needed(&at(None), &at(Some(crate::accessory::DEFAULT_ADDRESS))),
        "the default address"
    );
    assert!(!Config::restart_needed(
        &at(Some("127.0.0.1:9770")),
        &at(Some("127.0.0.1:9770"))
    ));
    assert!(
        Config::restart_needed(&at(Some("127.0.0.1:9770")), &at(Some("192.168.1.20:9770"))),
        "an address change"
    );
}

// -- Shots ---------------------------------------------------------------------------

/// The shot keys of the card's `shot.launch`es, with their launches.
fn launches(rig: &Rig) -> Vec<(ShotKey, flightrelay::Launch)> {
    rig.frps()
        .into_iter()
        .filter_map(|b| match b {
            FrpBody::ShotLaunch(l) => Some((l.key, l.launch)),
            _ => None,
        })
        .collect()
}

/// A6: a local player's shot in Pureplay is reported as the card's own
/// shot (`shot.trigger`, `shot.launch`, `shot.finished` under the card's
/// own key and count, a virtual source), and Pureplay's result goes out as
/// a stroke with that key's `shot_id`.
#[test]
fn a_local_shot_is_reported_as_the_cards_own() {
    let mut rig = connected("PPA-SHOTS");
    assert_eq!(Accessory::info(&Config::default()).source, Source::Virtual);
    rig.event("shot.launched", 2, &launched("pp-2", 1));
    let FrpBody::ShotTrigger(trigger) = rig.frp_until(|b| matches!(b, FrpBody::ShotTrigger(_)))
    else {
        unreachable!()
    };
    let key = trigger.key;
    assert_eq!(key.shot_number, 1, "the card's own count");
    assert_ne!(key.shot_id, "pp-2", "flighthook's own id");
    rig.frp_until(|b| matches!(b, FrpBody::ShotFinished(f) if f.key == key));
    let launch = launches(&rig);
    assert_eq!(launch.len(), 1);
    assert_eq!(launch[0].0, key);
    assert_eq!(launch[0].1.backspin_rpm, Some(3000));
    assert_eq!(launch[0].1.sidespin_rpm, Some(1500));
    rig.event("shot.completed", 3, &completed("pp-2", 1, "OB"));
    rig.frp_until(|b| matches!(b, FrpBody::GameStroke(_)));
    let strokes = rig.strokes();
    assert_eq!(strokes.len(), 1);
    assert_eq!(strokes[0].shot_id.as_deref(), Some(key.shot_id.as_str()));
    assert_eq!(strokes[0].outcome, flightrelay::StrokeOutcome::OutOfBounds);
    assert_eq!(strokes[0].golfer.as_str(), "pureplay.Joe");
}

/// A6: a shot of a player at another machine is not reported; its stroke
/// names no shot.
#[test]
fn a_remote_players_shot_is_ignored() {
    let mut rig = connected("PPA-REMOTE");
    rig.event("shot.launched", 2, &launched("pp-r", 3));
    rig.event("shot.completed", 3, &completed("pp-r", 3, "Green"));
    rig.event("shot.launched", 4, &launched("pp-l", 1));
    rig.frp_until(|b| matches!(b, FrpBody::ShotTrigger(_)));
    rig.caught_up();
    assert_eq!(rig.triggers().len(), 1);
    let strokes = rig.strokes();
    assert_eq!(strokes.len(), 1, "{strokes:?}");
    assert_eq!(strokes[0].shot_id, None);
    assert_eq!(strokes[0].golfer.as_str(), "pureplay.Remote");
}

/// `Gimme`, `AutoPutt` and `PenaltyOnly` have no launch: no shot is
/// reported and their strokes name no shot.
#[test]
fn launchless_outcomes_report_no_shot() {
    let mut rig = connected("PPA-GIMME");
    let mut seq = 2;
    for (id, outcome) in [
        ("pp-g", "Gimme"),
        ("pp-a", "AutoPutt"),
        ("pp-p", "PenaltyOnly"),
    ] {
        rig.event("shot.completed", seq, &completed(id, 1, outcome));
        seq += 1;
    }
    rig.event("shot.launched", seq, &launched("pp-x", 1));
    rig.event("shot.completed", seq + 1, &completed("pp-x", 1, "Holed"));
    rig.frp_until(|b| matches!(b, FrpBody::GameStroke(s) if s.shot_id.is_some()));
    rig.caught_up();
    assert_eq!(rig.triggers().len(), 1);
    let strokes: Vec<_> = rig
        .strokes()
        .into_iter()
        .map(|s| (s.shot_id.is_some(), s.outcome))
        .collect();
    assert_eq!(
        strokes,
        [
            (false, flightrelay::StrokeOutcome::Gimme),
            (false, flightrelay::StrokeOutcome::AutoPutt),
            (false, flightrelay::StrokeOutcome::PenaltyOnly),
            (true, flightrelay::StrokeOutcome::Holed),
        ]
    );
}

/// A6: a `shot.launched` repeating a `shotId` the card already holds is
/// not reported again; the next shot is.
#[test]
fn a_repeated_launch_is_not_taken_again() {
    let mut rig = connected("PPA-REPEAT");
    rig.event("shot.launched", 2, &launched("pp-1", 1));
    rig.frp_until(|b| matches!(b, FrpBody::ShotFinished(_)));
    rig.event("shot.launched", 3, &launched("pp-1", 1));
    rig.event("shot.launched", 4, &launched("pp-2", 1));
    rig.frp_until(|b| matches!(b, FrpBody::ShotFinished(_)));
    rig.caught_up();
    let numbers: Vec<u32> = rig.triggers().iter().map(|k| k.shot_number).collect();
    assert_eq!(numbers, [1, 2], "pp-1 once, then pp-2");
}

/// A6: a shot Pureplay discards sends no stroke for a result after the
/// discard; a stroke sent before it stands.
#[test]
fn a_discard_stops_a_later_stroke_and_keeps_an_earlier_one() {
    let mut rig = connected("PPA-DISCARD");
    rig.event("shot.launched", 2, &launched("pp-1", 1));
    rig.event("shot.launched", 3, &launched("pp-3", 1));
    let discarded = |id: &str| json!({ "shotId": id, "playerId": 1, "reason": "discarded" });
    rig.event("shot.cancelled", 4, &discarded("pp-3"));
    rig.event("shot.completed", 5, &completed("pp-3", 1, "InPlay"));
    rig.event("shot.completed", 6, &completed("pp-1", 1, "Water"));
    rig.event("shot.cancelled", 7, &discarded("pp-1"));
    rig.event("shot.launched", 8, &launched("pp-4", 1));
    rig.frp_until(|b| matches!(b, FrpBody::ShotTrigger(t) if t.key.shot_number == 3));
    rig.caught_up();
    let first = rig.triggers()[0].clone();
    let strokes: Vec<_> = rig
        .strokes()
        .into_iter()
        .map(|s| (s.shot_id, s.outcome))
        .collect();
    assert_eq!(
        strokes,
        [(Some(first.shot_id), flightrelay::StrokeOutcome::Water)],
        "no stroke for pp-3's result after its discard"
    );
}

/// A `shot.completed` after a reconnect to the same Pureplay still finds
/// its shot; after Pureplay restarted (a new `serverInstanceId`) the old
/// shot ids are forgotten. The card's count goes on across links.
#[test]
fn a_result_after_a_reconnect_to_the_same_pureplay_attaches() {
    let mut rig = connected("PPA-AGAIN");
    rig.event("shot.launched", 2, &launched("pp-1", 1));
    let FrpBody::ShotTrigger(first) = rig.frp_until(|b| matches!(b, FrpBody::ShotTrigger(_)))
    else {
        unreachable!()
    };
    rig.event("shot.launched", 3, &launched("pp-2", 1));
    rig.frp_until(|b| matches!(b, FrpBody::ShotTrigger(_)));
    rig.drop_link();
    rig.link_ends();

    for (instance, shot) in [("s-1", "pp-1"), ("s-2", "pp-2")] {
        rig.next();
        rig.hello_from(instance);
        assert!(rig.pair().is_some());
        rig.paired("PPA-AGAIN");
        rig.subscribe(&round(), 1);
        rig.shows(Status::Connected, |t| t.contains_key("club"));
        rig.event("shot.completed", 2, &completed(shot, 1, "Green"));
        rig.frp_until(|b| matches!(b, FrpBody::GameStroke(_)));
        if instance == "s-1" {
            rig.drop_link();
            rig.link_ends();
        }
    }
    rig.event("shot.launched", 3, &launched("pp-9", 1));
    let FrpBody::ShotTrigger(last) = rig.frp_until(|b| matches!(b, FrpBody::ShotTrigger(_))) else {
        unreachable!()
    };
    assert_eq!(
        last.key.shot_number, 3,
        "the count goes on across reconnects"
    );
    rig.caught_up();
    let ids: Vec<_> = rig.strokes().into_iter().map(|s| s.shot_id).collect();
    assert_eq!(ids, [Some(first.key.shot_id), None], "pp-2 is forgotten");
}

// -- state.get pacing (the requests' own rules) --------------------------------------

/// At a match start a player named while the request is out, and brought
/// by its answer, leaves nothing to ask for: no second `state.get`.
#[test]
fn a_snapshot_that_brings_the_named_player_ends_the_requests() {
    let mut gets = StateGets::default();
    let start = Instant::now();
    gets.want();
    let id = gets.due(start).expect("no timeout").expect("a request");
    gets.want_player(1);
    gets.answered(Some(&id), |p| p == 1);
    let later = start + STATE_GET_GAP * 3;
    assert_eq!(gets.due(later), Ok(None), "nothing missing, nothing asked");
}

/// A snapshot that comes after its request timed out and brings every
/// player it was asked for ends the retries too.
#[test]
fn a_late_snapshot_that_brings_everyone_ends_the_retries() {
    let mut gets = StateGets::default();
    let start = Instant::now();
    gets.want_player(1);
    let id = gets.due(start).expect("no timeout").expect("a request");
    let timed_out = start + SNAPSHOT_TIMEOUT;
    assert_eq!(gets.due(timed_out), Err(id.clone()), "given up");
    gets.answered(Some(&id), |p| p == 1);
    let much_later = timed_out + SNAPSHOT_TIMEOUT * 20;
    assert_eq!(gets.due(much_later), Ok(None), "nothing asked again");
    assert_eq!((gets.failures, gets.not_before), (0, None));
}

const MS: Duration = Duration::from_millis(1);

/// Nothing wanted, nothing asked; one request at a time, the next no sooner
/// than [`STATE_GET_GAP`] after the last, and a want while one is out is
/// answered by it.
#[test]
fn state_gets_go_one_at_a_time_at_least_the_gap_apart() {
    let mut gets = StateGets::default();
    let start = Instant::now();
    assert_eq!(gets.due(start), Ok(None), "nothing wanted");
    gets.want();
    let id = gets.due(start).expect("no timeout").expect("a request");
    gets.want();
    assert_eq!(gets.due(start + STATE_GET_GAP * 2), Ok(None), "one is out");
    gets.answered(Some(&id), |_| true);
    assert_eq!(gets.due(start + STATE_GET_GAP * 3), Ok(None), "answered");
    gets.want();
    let early = start + STATE_GET_GAP - MS;
    assert_eq!(gets.due(early), Ok(None), "within the gap");
    let next = gets.due(start + STATE_GET_GAP).expect("no timeout");
    assert!(next.is_some_and(|n| n != id), "a new request after the gap");
}

/// A request unanswered for [`SNAPSHOT_TIMEOUT`] is given up and sent again
/// after the next [`STATE_GET_RETRY`] wait, the last one repeating, with
/// the players it asked for.
#[test]
fn an_unanswered_state_get_is_retried_after_a_growing_wait() {
    let mut gets = StateGets::default();
    let mut now = Instant::now();
    gets.want_player(1);
    let mut id = gets.due(now).expect("no timeout").expect("a request");
    let waits = [
        STATE_GET_RETRY[0],
        STATE_GET_RETRY[1],
        STATE_GET_RETRY[2],
        STATE_GET_RETRY[2],
    ];
    for wait in waits {
        assert_eq!(gets.due(now + SNAPSHOT_TIMEOUT - MS), Ok(None), "waiting");
        now += SNAPSHOT_TIMEOUT;
        assert_eq!(gets.due(now), Err(id.clone()), "given up");
        assert_eq!(gets.due(now + wait - MS), Ok(None), "the wait {wait:?}");
        now += wait;
        let again = gets.due(now).expect("no timeout").expect("sent again");
        assert_ne!(again, id);
        let asked = gets.pending.as_ref().map(|(_, _, p)| p.clone());
        assert_eq!(asked, Some(BTreeSet::from([1])), "player 1 again");
        id = again;
    }
}

/// A refused request goes again after the next wait, or after
/// `retryAfterMs` when that is longer.
#[test]
fn a_refused_state_get_waits_the_longer_of_the_retry_and_retry_after() {
    for (retry_after, wait) in [
        (None, STATE_GET_RETRY[0]),
        (Some(MS), STATE_GET_RETRY[0]),
        (Some(STATE_GET_RETRY[2] * 2), STATE_GET_RETRY[2] * 2),
    ] {
        let mut gets = StateGets::default();
        let start = Instant::now();
        gets.want();
        let _ = gets.due(start);
        gets.refused(start, retry_after);
        assert_eq!(gets.due(start + wait - MS), Ok(None), "{retry_after:?}");
        let again = gets.due(start + wait).expect("no timeout");
        assert!(again.is_some(), "{retry_after:?}: sent after {wait:?}");
    }
}

/// A player a real snapshot asked for and did not bring is not asked for
/// again until [`StateGets::clear_not_brought`].
#[test]
fn a_player_the_snapshot_did_not_bring_is_asked_for_once_until_cleared() {
    let mut gets = StateGets::default();
    let mut now = Instant::now();
    gets.want_player(9);
    let id = gets.due(now).expect("no timeout").expect("a request");
    gets.answered(Some(&id), |_| false);
    for _ in 0..10 {
        gets.want_player(9);
        now += STATE_GET_GAP;
        assert_eq!(gets.due(now), Ok(None), "player 9 again");
    }
    gets.clear_not_brought();
    gets.want_player(9);
    now += STATE_GET_GAP;
    assert!(
        gets.due(now).expect("no timeout").is_some(),
        "after a clear"
    );
}

// -- Selects (A10) -------------------------------------------------------------------

/// A10: a `golfer.select` that gets no `bus.reply` holds back the card's
/// next select for [`model::SELECT_FORGOTTEN`]; then it is forgotten and
/// the card asserts its turn again.
#[test]
fn an_unanswered_select_blocks_the_cards_selects_until_it_is_forgotten() {
    let mut rig = connected("PPA-A10");
    let before = rig.selects().len();
    rig.answers = false;
    rig.event(
        "player.club_changed",
        2,
        &json!({ "playerId": 1, "current": "Iron8", "currentClubId": "b-8i",
                 "reason": "user_selected" }),
    );
    rig.frp_until(|b| matches!(b, FrpBody::GolferSelect(s) if s.club == Some(Some(Club::Iron8))));
    let sent = rig.host.now();
    rig.run_for(model::SELECT_FORGOTTEN - Duration::from_millis(50));
    assert_eq!(
        rig.selects().len(),
        before + 1,
        "nothing while the select waits for its reply"
    );
    rig.answers = true;
    rig.frp_until(|b| matches!(b, FrpBody::GolferSelect(s) if s.club == Some(Some(Club::Iron8))));
    assert!(
        rig.host.now() - sent >= model::SELECT_FORGOTTEN,
        "once forgotten"
    );
    rig.caught_up();
    assert_eq!(rig.selects().len(), before + 2, "asserted once more");
    assert_eq!(rig.turn.club, Some(Club::Iron8));
}
