//! Club out against the mock server: a club picked in flighthook goes to
//! Pureplay as a command; Pureplay's replies, readiness and events decide
//! what becomes of it.

use super::*;
use crate::bus::BusMsg;

/// The token every rig pairs with.
const TOKEN: &str = "PPA-CLUBOUT";

/// `origin` picks `club` on bus `tx` (a `golfer.select`); the card's
/// session ([`SessionActor`]) answers with its turn.
fn send_club(tx: &broadcast::Sender<BusMsg>, origin: &str, club: Club) {
    let mut select = flightrelay::GolferSelect::new(format!("{origin}-pick"));
    select.club = Some(Some(club));
    BusSender::new(origin.into(), tx.clone(), Arc::default())
        .with_frp_id(Some(origin.into()))
        .frp(select);
}

/// The readiness of Joe's shot `shot`: ready to play, or not.
fn readiness(shot: &str, ready: bool, allowed: &[&str], blockers: &[&str]) -> Value {
    json!({ "shotId": shot, "playerId": 1, "ready": ready, "controllable": ready,
            "allowedCommands": if ready { allowed.to_vec() } else { Vec::new() },
            "blockers": blockers })
}

/// [`round`] with `readiness` and, when given, Joe's bag and held club.
fn round_with(readiness: Value, bag: Option<(Value, &str, &str)>) -> Value {
    let mut r = round();
    r["readiness"] = readiness;
    if let Some((bag, club, club_id)) = bag {
        r["players"][0]["bag"] = bag;
        r["players"][0]["club"] = json!(club);
        r["players"][0]["clubId"] = json!(club_id);
    }
    r
}

impl Peer {
    /// `hello` from instance `instance` offering `commands`.
    fn hello_offering(&mut self, instance: &str, commands: &[&str]) {
        self.send(&json!({ "type": "hello", "data": {
            "protocol": "ppaccessory", "version": 1, "game": "Pureplay",
            "gameVersion": "1.4.2", "paired": false, "connectionId": "c-1",
            "serverInstanceId": instance,
            "capabilities": { "events": EVENTS_LISTED, "commands": commands }
        }}));
    }

    /// `paired` granting `scopes`.
    fn paired_with(&mut self, token: &str, scopes: &[&str]) {
        self.send(&json!({ "type": "paired", "data": {
            "token": token, "clientId": "client-1", "scopes": scopes } }));
    }

    /// The card's next command; every one carries `playerId`.
    fn command(&mut self) -> Value {
        loop {
            let msg = self.recv();
            if msg["type"] == "command" {
                assert!(
                    msg["data"]["playerId"].is_i64(),
                    "a command carries playerId: {msg}"
                );
                assert_eq!(
                    self.kinds.last(),
                    Some(&FrameType::Text),
                    "commands go in text frames"
                );
                return msg;
            }
        }
    }

    /// The commands the card sends within `quiet`.
    fn commands_within(&mut self, quiet: Duration) -> Vec<Value> {
        let until = Instant::now() + quiet;
        let mut out = Vec::new();
        while Instant::now() < until {
            match self.ws.read() {
                Ok(Message::Text(t)) => {
                    let v: Value = serde_json::from_str(t.as_str()).expect("json");
                    if v["type"] == "command" {
                        out.push(v);
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
        }
        out
    }
}

/// A card following [`round`] (or another) on a bus the test can put picks
/// on.
struct Rig {
    mock: Mock,
    card: Card,
    peer: Peer,
    tx: broadcast::Sender<BusMsg>,
    seq: u64,
}

impl Rig {
    /// Configure the game cards the tests pick from (`openconnect_client.0`,
    /// `pureplay.0`, `pureplay_accessory.1`), each with its card key as its
    /// FRP actor id, so the card knows them as game cards.
    fn game_cards(&self) {
        self.card.state.system.update(|c| {
            c.openconnect_client.insert(
                "0".into(),
                flighthook::OpenConnectClientSection {
                    actor_id: Some("openconnect_client.0".into()),
                    ..Default::default()
                },
            );
            c.pureplay.insert(
                "0".into(),
                flighthook::PureplaySection {
                    actor_id: Some("pureplay.0".into()),
                    ..Default::default()
                },
            );
            c.pureplay_accessory.insert(
                "1".into(),
                flighthook::PureplayAccessorySection {
                    actor_id: Some("pureplay_accessory.1".into()),
                    ..Default::default()
                },
            );
        });
    }

    /// Connected to `round`, Pureplay offering `commands` and the pairing
    /// granting `scopes`.
    fn with(round: &Value, commands: &[&str], scopes: &[&str]) -> Self {
        let mock = Mock::new();
        let state = new_state();
        let (tx, _) = broadcast::channel(4096);
        state
            .pureplay_tokens
            .save(&token_key(mock.addr), &Token::new(TOKEN), "client-1")
            .expect("save");
        let tokens = Arc::clone(&state.pureplay_tokens);
        let mut card = Card::start_on(mock.addr, state, tokens, PING_EVERY, &tx);
        let mut peer = mock.next();
        peer.hello_offering("s-1", commands);
        assert_eq!(peer.pair().as_deref(), Some(TOKEN));
        peer.paired_with(TOKEN, scopes);
        peer.subscribe(round, 1);
        card.shows(ActorStatus::Connected, |t| t.contains_key("club_out"));
        card.settle();
        Self {
            mock,
            card,
            peer,
            tx,
            seq: 1,
        }
    }

    /// Connected to [`round`] with `club.set` and `control`.
    fn new() -> Self {
        Self::with(
            &round(),
            &["club.set", "club.next", "club.prev"],
            &["read", "control"],
        )
    }

    /// `origin` puts `club` on the bus.
    fn pick_from(&self, origin: &str, club: Club) {
        send_club(&self.tx, origin, club);
    }

    /// The header picks `club`.
    fn pick(&self, club: Club) {
        self.pick_from("web", club);
    }

    /// Pureplay sends event `name`.
    fn event(&mut self, name: &str, data: &Value) {
        self.seq += 1;
        self.peer.event(name, self.seq, data);
    }

    /// Pureplay's readiness changes.
    fn ready(&mut self, readiness: &Value) {
        self.event("shot.ready_changed", readiness);
    }

    /// A successful reply to `cmd`: Joe holds `club` (`club_id`).
    fn ok(&mut self, cmd: &Value, club: &str, club_id: &str) {
        self.peer
            .send(&json!({ "type": "response", "id": cmd["id"], "ok": true,
            "data": { "playerId": 1, "club": club, "clubId": club_id } }));
    }

    /// A refused reply to `cmd`.
    fn refuse(&mut self, cmd: &Value, code: &str, extra: &Value) {
        let id = cmd["id"].as_str().map(str::to_owned);
        self.peer.refuse(id.as_deref(), code, extra);
    }

    /// No command within [`QUIET`].
    fn no_command(&mut self, why: &str) {
        let sent = self.peer.commands_within(QUIET);
        assert!(sent.is_empty(), "{why}: {sent:?}");
    }

    /// The clubs the card put on the bus from message `from` on.
    fn clubs_since(&mut self, from: usize) -> Vec<Club> {
        self.card.settle();
        let own: Vec<FlighthookMessage> = self.card.sent[from..]
            .iter()
            .filter(|m| m.actor == ACTOR)
            .cloned()
            .collect();
        clubs(&own)
    }

    /// Wait for the card to put `club` on the bus.
    fn snaps_back_to(&mut self, club: Club) {
        self.card.bus_from(ACTOR, |e| is_club(e, club));
    }

    /// Wait until the session's turn holds `club` and the card has had
    /// time to take that turn off the bus.
    fn session_holds(&mut self, club: Club) {
        let deadline = Instant::now() + WAIT;
        while self.card.state.session.turn().club != Some(club) {
            assert!(Instant::now() < deadline, "the session never held {club:?}");
            self.card.step();
        }
        self.card.settle();
    }
}

/// A header pick goes to Pureplay as `club.set` with the bag entry's id and
/// `playerId`, in a text frame. Pureplay holding the pick, nothing goes
/// back on the bus, nor for Pureplay's own report of the change.
#[test]
fn a_header_pick_sets_the_club_in_pureplay_by_its_bag_id() {
    let mut rig = Rig::new();
    rig.card.shows_row(ActorStatus::Connected, "club_out", "on");
    let from = rig.card.sent.len();
    rig.pick(Club::Iron8);
    let cmd = rig.peer.command();
    assert_eq!(cmd["command"], "club.set");
    assert_eq!(cmd["data"], json!({ "playerId": 1, "clubId": "b-8i" }));
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 1, "shotId": "shot-1", "previous": "Iron7", "current": "Iron8",
                 "previousClubId": "b-7i", "currentClubId": "b-8i", "reason": "api_command" }),
    );
    rig.card.shows_row(ActorStatus::Connected, "club", "8I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "flighthook holds the 8 iron already"
    );
    rig.no_command("one pick, one command");
    rig.card.stop();
}

/// Picks from the header and from devices go to Pureplay; a simulator
/// output's club (Pureplay's own coming back), another Pureplay Accessory card's and
/// the card's own never do.
#[test]
fn only_clubs_from_outside_the_simulators_go_to_pureplay() {
    let mut rig = Rig::new();
    rig.game_cards();
    for origin in ["openconnect_client.0", "pureplay.0", "pureplay_accessory.1"] {
        rig.pick_from(origin, Club::Driver);
    }
    rig.no_command("an output's club is never sent back");
    // Pureplay's own change: the card puts it on the bus, and does not send
    // it back.
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 1, "current": "Iron8", "currentClubId": "b-8i",
                 "reason": "user_selected" }),
    );
    rig.card.bus_from(ACTOR, |e| is_club(e, Club::Iron8));
    rig.no_command("the card's own club is never sent back");
    rig.pick_from("square.0", Club::Driver);
    let cmd = rig.peer.command();
    assert_eq!(cmd["data"]["clubId"], "b-dr", "a device's club goes");
    rig.card.stop();
}

/// Pureplay holding a club of the picked type sends nothing: a second sand
/// wedge picked in Pureplay stays, whether its type comes back through an
/// output or is picked again in the header.
#[test]
fn a_club_of_the_held_type_sends_nothing_and_a_second_sand_wedge_stays() {
    let bag = json!([ { "clubId": "b-7i", "type": "Iron7", "name": "7 Iron" },
                      { "clubId": "sw-56", "type": "SandWedge", "name": "56" },
                      { "clubId": "sw-60", "type": "SandWedge", "name": "60" } ]);
    let r = round_with(
        readiness("shot-1", true, &["club.set"], &[]),
        Some((bag, "Iron7", "b-7i")),
    );
    let mut rig = Rig::with(&r, &["club.set"], &["read", "control"]);
    rig.game_cards();
    rig.pick(Club::Iron7);
    rig.no_command("Pureplay holds a 7 iron");
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 1, "current": "SandWedge", "currentClubId": "sw-60",
                 "reason": "user_selected" }),
    );
    rig.card.bus_from(ACTOR, |e| is_club(e, Club::SandWedge));
    rig.pick_from("openconnect_client.0", Club::SandWedge);
    rig.pick(Club::SandWedge);
    rig.no_command("the 60 stays");
    rig.card.stop();
}

/// A pick made while the ball is in the air waits; Pureplay's own club for
/// the next shot (`auto`) does not drop it and does not reach the bus; once
/// Pureplay is ready the pick is sent, and wins.
#[test]
fn a_pick_waits_through_the_shot_and_wins_over_pureplays_suggestion() {
    let mut rig = Rig::new();
    rig.ready(&readiness("shot-1", false, &[], &["shot_in_progress"]));
    rig.card
        .shows_row(ActorStatus::Connected, "blockers", "shot in progress");
    let from = rig.card.sent.len();
    rig.pick(Club::Iron8);
    rig.no_command("not while the ball is in the air");
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 1, "shotId": "shot-2", "current": "Driver",
                 "currentClubId": "b-dr", "reason": "auto" }),
    );
    rig.no_command("Pureplay is still not ready");
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    let cmd = rig.peer.command();
    assert_eq!(cmd["data"]["clubId"], "b-8i");
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.card.shows_row(ActorStatus::Connected, "club", "8I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "the pick stays in flighthook throughout: Pureplay's driver never reached the bus"
    );
    rig.card.stop();
}

/// A pending pick is dropped by a turn change, a context reset, and the
/// player's own choice in Pureplay; Pureplay's club goes on the bus, and
/// nothing is sent once Pureplay is ready.
#[test]
fn a_pending_pick_is_dropped_by_a_turn_a_reset_and_the_players_choice() {
    let waiting = readiness("shot-1", false, &[], &["shot_in_progress"]);
    for case in ["turn.changed", "context.invalidated", "user_selected"] {
        let mut rig = Rig::new();
        rig.ready(&waiting);
        rig.card
            .shows_row(ActorStatus::Connected, "blockers", "shot in progress");
        rig.pick(Club::Iron8);
        rig.no_command(case);
        match case {
            "turn.changed" => {
                rig.event(
                    "turn.changed",
                    &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "holeNumber": 7,
                             "reason": "next_player" }),
                );
                rig.snaps_back_to(Club::Driver);
                rig.event(
                    "turn.changed",
                    &json!({ "previousPlayerId": 2, "currentPlayerId": 1, "holeNumber": 7,
                             "reason": "next_player" }),
                );
                rig.snaps_back_to(Club::Iron7);
            }
            "context.invalidated" => {
                rig.event(
                    "context.invalidated",
                    &json!({ "reason": "reset", "matchId": null }),
                );
                let get = rig.peer.recv();
                assert_eq!(get["type"], "state.get", "{get}");
                rig.peer
                    .send(&json!({ "type": "event", "event": "state.snapshot",
                    "seq": rig.seq + 1, "requestId": get["id"], "data": round() }));
                rig.seq += 1;
                rig.snaps_back_to(Club::Iron7);
            }
            _ => {
                rig.event(
                    "player.club_changed",
                    &json!({ "playerId": 1, "current": "Iron9", "reason": "user_selected" }),
                );
                rig.snaps_back_to(Club::Iron9);
            }
        }
        rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
        rig.no_command(case);
        rig.card.stop();
    }
}

/// A pending pick goes with the connection: after a reconnect to the same
/// Pureplay, or to a restarted one, nothing is sent.
#[test]
fn a_pending_pick_is_dropped_with_the_connection() {
    for instance in ["s-1", "s-2"] {
        let mut rig = Rig::new();
        rig.ready(&readiness("shot-1", false, &[], &["shot_in_progress"]));
        rig.card
            .shows_row(ActorStatus::Connected, "blockers", "shot in progress");
        rig.pick(Club::Iron8);
        rig.no_command("not ready");
        drop(rig.peer);
        rig.card.shows(ActorStatus::Reconnecting, |_| true);
        let mut peer = rig.mock.next();
        peer.hello_offering(instance, &["club.set"]);
        assert_eq!(peer.pair().as_deref(), Some(TOKEN));
        peer.paired_with(TOKEN, &["read", "control"]);
        peer.subscribe(&round(), 1);
        rig.card.shows_row(ActorStatus::Connected, "club", "7I");
        let sent = peer.commands_within(QUIET);
        assert!(sent.is_empty(), "{instance}: {sent:?}");
        rig.card.stop();
    }
}

/// Pureplay's `auto` change for the shot right after the pick was set
/// sends the pick once more; a second one is accepted and goes on the bus.
#[test]
fn auto_after_a_set_sends_the_pick_once_more() {
    let mut rig = Rig::new();
    rig.pick(Club::Iron8);
    let cmd = rig.peer.command();
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.card.shows_row(ActorStatus::Connected, "club", "8I");
    let from = rig.card.sent.len();
    let auto = json!({ "playerId": 1, "shotId": "shot-1", "previous": "Iron8", "current": "Driver",
                       "currentClubId": "b-dr", "reason": "auto" });
    rig.event("player.club_changed", &auto);
    let again = rig.peer.command();
    assert_eq!(again["command"], "club.set");
    assert_eq!(again["data"]["clubId"], "b-8i");
    rig.ok(&again, "Iron8", "b-8i");
    rig.card.shows_row(ActorStatus::Connected, "club", "8I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "the driver never reached the bus"
    );
    rig.event("player.club_changed", &auto);
    rig.snaps_back_to(Club::Driver);
    rig.no_command("a second auto is accepted");
    rig.card.stop();
}

/// `not_ready` puts the pick back: a new readiness sends it again at once,
/// ahead of the retry timer.
#[test]
fn not_ready_puts_the_pick_back_until_pureplay_is_ready() {
    let mut rig = Rig::new();
    rig.pick(Club::Iron8);
    let cmd = rig.peer.command();
    rig.refuse(
        &cmd,
        "not_ready",
        &json!({ "blockers": ["shot_in_progress"] }),
    );
    let refused = Instant::now();
    rig.ready(&readiness("shot-1", true, &["club.set"], &[]));
    let again = rig.peer.command();
    assert!(
        refused.elapsed() < club_out::NOT_READY_RETRY,
        "the readiness, not the timer"
    );
    assert_ne!(again["id"], cmd["id"]);
    assert_eq!(again["data"]["clubId"], "b-8i");
    rig.card.stop();
}

/// With no new readiness, a pick answered `not_ready` is sent again on a
/// timer, three times; the fourth `not_ready` drops it and Pureplay's club
/// goes back on the bus.
#[test]
fn not_ready_is_retried_on_a_timer_three_times_then_dropped() {
    let mut rig = Rig::new();
    rig.pick(Club::Iron8);
    let mut cmd = rig.peer.command();
    for retry in 1..=club_out::NOT_READY_RETRIES {
        rig.refuse(&cmd, "not_ready", &json!({}));
        let refused = Instant::now();
        let again = rig.peer.command();
        assert!(
            refused.elapsed() >= club_out::NOT_READY_RETRY - Duration::from_millis(50),
            "retry {retry} waits for the timer"
        );
        assert_eq!(again["data"]["clubId"], "b-8i", "retry {retry}");
        cmd = again;
    }
    rig.refuse(&cmd, "not_ready", &json!({}));
    rig.snaps_back_to(Club::Iron7);
    let sent = rig.peer.commands_within(club_out::NOT_READY_RETRY * 2);
    assert!(sent.is_empty(), "dropped: {sent:?}");
    rig.card.stop();
}

/// `rate_limited` is retried once after `retryAfterMs`; a second one drops
/// the pick and puts Pureplay's club back.
#[test]
fn rate_limited_is_retried_once_after_its_wait() {
    let mut rig = Rig::new();
    rig.pick(Club::Iron8);
    let cmd = rig.peer.command();
    let refused = Instant::now();
    rig.refuse(&cmd, "rate_limited", &json!({ "retryAfterMs": 300 }));
    let again = rig.peer.command();
    assert!(
        refused.elapsed() >= Duration::from_millis(300),
        "after the wait"
    );
    assert_eq!(again["data"]["clubId"], "b-8i");
    rig.refuse(&again, "rate_limited", &json!({ "retryAfterMs": 50 }));
    rig.snaps_back_to(Club::Iron7);
    rig.no_command("one retry");
    rig.card.stop();
}

/// Every refusal drops the pick and puts Pureplay's club back on the bus;
/// so does a club not in the bag and a command left unanswered.
/// `forbidden_scope` also turns club out off.
#[test]
fn every_failure_puts_pureplays_club_back() {
    for code in [
        "forbidden_scope",
        "not_allowed",
        "bad_request",
        "unknown_command",
    ] {
        let mut rig = Rig::new();
        rig.pick(Club::Iron8);
        let cmd = rig.peer.command();
        rig.refuse(&cmd, code, &json!({}));
        rig.snaps_back_to(Club::Iron7);
        if code == "forbidden_scope" {
            rig.card
                .shows_row(ActorStatus::Connected, "club_out", "off in Pureplay");
            rig.pick(Club::Driver);
            rig.snaps_back_to(Club::Iron7);
            rig.no_command("club out is off");
            // The switch on again: a readiness that lets apps steer.
            rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
            rig.card.shows_row(ActorStatus::Connected, "club_out", "on");
            rig.pick(Club::Driver);
            assert_eq!(rig.peer.command()["data"]["clubId"], "b-dr");
        }
        rig.card.stop();
    }
    let mut rig = Rig::new();
    rig.pick(Club::PitchingWedge);
    rig.snaps_back_to(Club::Iron7);
    rig.no_command("not in the bag");
    rig.pick(Club::Iron8);
    let started = Instant::now();
    let _unanswered = rig.peer.command();
    rig.snaps_back_to(Club::Iron7);
    assert!(started.elapsed() >= club_out::COMMAND_TIMEOUT);
    rig.card.stop();
}

/// Without `club.set` the card steps with `club.next` / `club.prev` toward
/// the type, each reply naming the club reached; the steps' own changes do
/// not reach the bus.
#[test]
fn without_club_set_the_card_steps_through_the_bag() {
    let steps = ["club.next", "club.prev"];
    let r = round_with(readiness("shot-1", true, &steps, &[]), None);
    let mut rig = Rig::with(&r, &steps, &["read", "control"]);
    rig.card.shows_row(ActorStatus::Connected, "club_out", "on");
    let from = rig.card.sent.len();
    rig.pick(Club::Iron8);
    let cmd = rig.peer.command();
    assert_eq!(cmd["command"], "club.next");
    assert_eq!(cmd["data"], json!({ "playerId": 1 }));
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.card.shows_row(ActorStatus::Connected, "club", "8I");
    rig.no_command("the 8 iron is reached");

    rig.pick(Club::Driver);
    let first = rig.peer.command();
    assert_eq!(
        first["command"], "club.prev",
        "the driver is before the 8 iron"
    );
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 1, "current": "Iron7", "currentClubId": "b-7i",
                 "reason": "api_command" }),
    );
    rig.ok(&first, "Iron7", "b-7i");
    let second = rig.peer.command();
    assert_eq!(second["command"], "club.prev");
    rig.ok(&second, "Driver", "b-dr");
    rig.card.shows_row(ActorStatus::Connected, "club", "DR");
    rig.no_command("the driver is reached");
    assert_eq!(rig.clubs_since(from), [], "the steps stay off the bus");
    rig.card.stop();
}

/// One command in flight: picks made meanwhile replace each other, and the
/// latest goes once the reply is in.
#[test]
fn one_command_in_flight_and_the_latest_pick_wins() {
    let mut rig = Rig::new();
    rig.pick(Club::Iron8);
    let cmd = rig.peer.command();
    rig.pick(Club::Driver);
    rig.card.settle();
    rig.pick(Club::Iron7);
    rig.no_command("one in flight");
    rig.ok(&cmd, "Iron8", "b-8i");
    let latest = rig.peer.command();
    assert_eq!(latest["data"]["clubId"], "b-7i", "the latest pick");
    rig.ok(&latest, "Iron7", "b-7i");
    rig.no_command("the driver was replaced");
    rig.card.stop();
}

/// The `club_out` row: `read only` without `control`, `not supported`
/// without a club command, `off in Pureplay` while Pureplay's switch is
/// off. In each a pick sends nothing and Pureplay's club goes back.
#[test]
fn the_club_out_row_says_whether_picks_reach_pureplay() {
    let off = readiness("shot-1", true, &[], &["controls_disabled"]);
    let mut off = round_with(off, None);
    off["readiness"]["controllable"] = json!(false);
    for (r, commands, scopes, row) in [
        (round(), &["club.set"][..], &["read"][..], "read only"),
        (
            round(),
            &["aim.set"][..],
            &["read", "control"][..],
            "not supported",
        ),
        (
            off,
            &["club.set"][..],
            &["read", "control"][..],
            "off in Pureplay",
        ),
    ] {
        let mut rig = Rig::with(&r, commands, scopes);
        rig.card.shows_row(ActorStatus::Connected, "club_out", row);
        rig.pick(Club::Iron8);
        rig.snaps_back_to(Club::Iron7);
        rig.no_command(row);
        rig.card.stop();
    }
}

/// A pick waits while the readiness names no player (between shots) and
/// goes once it names the player and lists the command.
#[test]
fn a_pick_waits_for_a_readiness_naming_the_player() {
    let mut rig = Rig::new();
    rig.ready(
        &json!({ "shotId": null, "playerId": null, "ready": false, "controllable": true,
                       "allowedCommands": ["club.set"], "blockers": [] }),
    );
    rig.card.settle();
    rig.pick(Club::Iron8);
    rig.no_command("no player named");
    rig.ready(&readiness("shot-2", true, &["aim.set", "club.set"], &[]));
    let cmd = rig.peer.command();
    assert_eq!(cmd["data"], json!({ "playerId": 1, "clubId": "b-8i" }));
    rig.card.stop();
}

/// Pureplay ready for the player but not listing the command (a drill, a
/// mode that keeps the club): the pick is dropped at once and Pureplay's
/// club goes back on the bus; a later readiness that lists it sends
/// nothing.
#[test]
fn a_pick_pureplay_does_not_take_here_is_dropped_at_once() {
    let mut rig = Rig::new();
    rig.ready(&readiness("shot-2", true, &["aim.set"], &[]));
    rig.card.settle();
    rig.pick(Club::Iron8);
    rig.snaps_back_to(Club::Iron7);
    rig.no_command("club.set not allowed");
    rig.ready(&readiness("shot-2", true, &["aim.set", "club.set"], &[]));
    rig.no_command("the pick was dropped");
    // Pureplay's club changes reach the bus again at once.
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 1, "current": "Driver", "currentClubId": "b-dr", "reason": "auto" }),
    );
    rig.snaps_back_to(Club::Driver);
    rig.card.stop();
}

/// The ball in the air: no command at all, so a pick waits.
fn in_the_air() -> Value {
    readiness("shot-1", false, &[], &["shot_in_progress"])
}

impl Rig {
    /// Connected to [`round`] with the ball in the air.
    fn in_the_air() -> Self {
        let mut rig = Self::new();
        rig.ready(&in_the_air());
        rig.card
            .shows_row(ActorStatus::Connected, "blockers", "shot in progress");
        rig
    }

    /// Pureplay suggests `club` for Joe's next shot.
    fn auto(&mut self, club: &str, club_id: &str) {
        self.event(
            "player.club_changed",
            &json!({ "playerId": 1, "shotId": "shot-2", "current": club,
                     "currentClubId": club_id, "reason": "auto" }),
        );
    }
}

/// An `auto` change seen while a newer pick is in flight came before that
/// command: the older pick is not sent again, and the newer one stays.
#[test]
fn an_auto_change_during_a_newer_pick_does_not_resend_the_older_one() {
    let mut rig = Rig::new();
    rig.pick(Club::Iron8);
    let first = rig.peer.command();
    rig.ok(&first, "Iron8", "b-8i");
    rig.card.shows_row(ActorStatus::Connected, "club", "8I");
    rig.pick(Club::Driver);
    let second = rig.peer.command();
    assert_eq!(second["data"]["clubId"], "b-dr");
    let from = rig.card.sent.len();
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 1, "shotId": "shot-1", "current": "Iron9", "reason": "auto" }),
    );
    rig.ok(&second, "Driver", "b-dr");
    rig.card.shows_row(ActorStatus::Connected, "club", "DR");
    rig.no_command("the 8 iron is not sent again");
    assert_eq!(rig.clubs_since(from), [], "the header stays on the driver");
    rig.card.stop();
}

/// The player's own choice in Pureplay after a pick was set: a later
/// `auto` change for the shot does not send the old pick over it.
#[test]
fn the_players_choice_after_a_set_ends_the_auto_resend() {
    let mut rig = Rig::new();
    rig.pick(Club::Iron8);
    let cmd = rig.peer.command();
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.card.shows_row(ActorStatus::Connected, "club", "8I");
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 1, "shotId": "shot-1", "current": "Iron9", "reason": "user_selected" }),
    );
    rig.snaps_back_to(Club::Iron9);
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 1, "shotId": "shot-1", "previous": "Iron9", "current": "Driver",
                 "currentClubId": "b-dr", "reason": "auto" }),
    );
    rig.snaps_back_to(Club::Driver);
    rig.no_command("the player chose since");
    rig.card.stop();
}

/// A pick of the club type Pureplay holds, while Pureplay can't take a
/// pick, still waits: when Pureplay suggests another club meanwhile, the
/// pick goes once Pureplay is ready.
#[test]
fn a_pick_of_the_held_type_waits_and_wins_over_a_suggestion() {
    let mut rig = Rig::in_the_air();
    let from = rig.card.sent.len();
    rig.pick(Club::Iron8);
    rig.card.settle();
    rig.pick(Club::Iron7);
    rig.no_command("the ball is in the air");
    rig.auto("Driver", "b-dr");
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    let cmd = rig.peer.command();
    assert_eq!(cmd["data"]["clubId"], "b-7i", "the latest pick");
    rig.ok(&cmd, "Iron7", "b-7i");
    rig.card.shows_row(ActorStatus::Connected, "club", "7I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "Pureplay's driver never reached the bus"
    );
    rig.card.stop();
}

/// A pick of the club type Pureplay holds, while Pureplay can't take a
/// pick: when Pureplay still holds that type once it is ready, nothing is
/// sent (a second club of the type stays).
#[test]
fn a_pick_of_the_held_type_sends_nothing_once_pureplay_is_ready() {
    let mut rig = Rig::in_the_air();
    rig.pick(Club::Iron7);
    rig.no_command("the ball is in the air");
    let from = rig.card.sent.len();
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    rig.no_command("Pureplay holds a 7 iron");
    assert_eq!(rig.clubs_since(from), [], "flighthook holds the 7 iron");
    rig.card.stop();
}

/// A `turn.changed` naming the pick's own player (a new hole in a
/// one-player round) keeps the pick: it goes at the next shot, over
/// Pureplay's suggestion.
#[test]
fn a_new_hole_for_the_same_player_keeps_the_pick() {
    let mut rig = Rig::in_the_air();
    let from = rig.card.sent.len();
    rig.pick(Club::Iron8);
    rig.no_command("the ball is in the air");
    rig.event(
        "turn.changed",
        &json!({ "previousPlayerId": 1, "currentPlayerId": 1, "holeNumber": 8,
                 "reason": "new_hole" }),
    );
    rig.auto("Driver", "b-dr");
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    let cmd = rig.peer.command();
    assert_eq!(cmd["data"]["clubId"], "b-8i");
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.card.shows_row(ActorStatus::Connected, "club", "8I");
    assert_eq!(
        rig.clubs_since(from),
        [],
        "the driver never reached the bus"
    );
    rig.card.stop();
}

/// A player with no bag: `club_out` says `not supported`, and a pick
/// sends nothing and puts Pureplay's club back.
#[test]
fn a_player_without_a_bag_has_no_club_out() {
    for bag in [json!(null), json!([])] {
        let mut r = round();
        r["players"][0]["bag"] = bag.clone();
        let mut rig = Rig::with(&r, &["club.set"], &["read", "control"]);
        rig.card
            .shows_row(ActorStatus::Connected, "club_out", "not supported");
        rig.pick(Club::Iron8);
        rig.snaps_back_to(Club::Iron7);
        rig.no_command(&format!("bag {bag}"));
        // Ann, up next, has a bag.
        rig.event(
            "turn.changed",
            &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "reason": "next_player" }),
        );
        rig.card.shows_row(ActorStatus::Connected, "club_out", "on");
        rig.card.stop();
    }
}

/// A command in flight when the turn passes to another player: a late
/// success, a late `not_ready` or no reply at all sends nothing more, and
/// the new player's club stays on the bus.
#[test]
fn a_command_in_flight_across_a_turn_sends_nothing_more() {
    for case in ["success", "not_ready", "no reply"] {
        let mut rig = Rig::new();
        rig.pick(Club::Iron8);
        let cmd = rig.peer.command();
        rig.event(
            "turn.changed",
            &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "reason": "next_player" }),
        );
        rig.snaps_back_to(Club::Driver);
        rig.session_holds(Club::Driver);
        let from = rig.card.sent.len();
        match case {
            "success" => rig.ok(&cmd, "Iron8", "b-8i"),
            "not_ready" => rig.refuse(&cmd, "not_ready", &json!({})),
            _ => std::thread::sleep(club_out::COMMAND_TIMEOUT),
        }
        rig.no_command(case);
        assert_eq!(rig.clubs_since(from), [], "{case}: Ann's driver stays");
        rig.card.shows_row(ActorStatus::Connected, "club", "DR");
        rig.card.stop();
    }
}

/// A pick while a player at another computer is up is not sent and puts
/// nothing back; when a player here is up again, their club in Pureplay
/// goes on the bus.
#[test]
fn a_pick_while_nobody_here_is_up_is_not_sent() {
    let mut rig = Rig::new();
    rig.event(
        "turn.changed",
        &json!({ "previousPlayerId": 1, "currentPlayerId": 3, "reason": "next_player" }),
    );
    rig.card
        .shows(ActorStatus::Connected, |t| !t.contains_key("club"));
    let from = rig.card.sent.len();
    rig.pick(Club::Iron8);
    rig.no_command("nobody here is up");
    assert_eq!(rig.clubs_since(from), [], "nothing put back");
    rig.event(
        "turn.changed",
        &json!({ "previousPlayerId": 3, "currentPlayerId": 1, "reason": "next_player" }),
    );
    rig.snaps_back_to(Club::Iron7);
    rig.no_command("the pick was never kept");
    rig.card.stop();
}

/// A `seq` gap while a pick waits: the fresh snapshot keeps the pick (and
/// Pureplay's club stays off the bus) while the same player is up, and
/// drops it when it names another player.
#[test]
fn a_snapshot_after_a_gap_keeps_or_drops_the_pending_pick() {
    for same in [true, false] {
        let mut rig = Rig::in_the_air();
        let from = rig.card.sent.len();
        rig.pick(Club::Iron8);
        rig.no_command("the ball is in the air");
        rig.seq += 2;
        rig.peer.event(
            "hole.started",
            rig.seq,
            &json!({ "holeNumber": 7, "par": 4 }),
        );
        let get = rig.peer.recv();
        assert_eq!(get["type"], "state.get", "{get}");
        let mut snap = round_with(in_the_air(), None);
        snap["players"][0]["club"] = json!("Driver");
        snap["players"][0]["clubId"] = json!("b-dr");
        if !same {
            snap["turn"]["activeShooterId"] = json!(2);
        }
        rig.seq += 1;
        rig.peer.send(
            &json!({ "type": "event", "event": "state.snapshot", "seq": rig.seq,
                           "requestId": get["id"], "data": snap }),
        );
        if same {
            rig.card.settle();
            assert_eq!(rig.clubs_since(from), [], "Joe's driver stays off the bus");
            rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
            assert_eq!(rig.peer.command()["data"]["clubId"], "b-8i");
        } else {
            rig.snaps_back_to(Club::Driver);
            rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
            rig.no_command("Ann is up");
        }
        rig.card.stop();
    }
}

/// A reply after the command timed out is ignored: nothing is sent and
/// nothing goes on the bus for it.
#[test]
fn a_reply_after_the_timeout_is_ignored() {
    let mut rig = Rig::new();
    rig.pick(Club::Iron8);
    let cmd = rig.peer.command();
    rig.snaps_back_to(Club::Iron7);
    let from = rig.card.sent.len();
    rig.ok(&cmd, "Iron8", "b-8i");
    rig.no_command("the late reply");
    assert_eq!(rig.clubs_since(from), []);
    rig.card.shows_row(ActorStatus::Connected, "club", "7I");
    rig.card.stop();
}

/// `rate_limited` without `retryAfterMs` is retried after a short wait.
#[test]
fn rate_limited_without_a_wait_is_retried() {
    let mut rig = Rig::new();
    rig.pick(Club::Iron8);
    let cmd = rig.peer.command();
    rig.refuse(&cmd, "rate_limited", &json!({}));
    let again = rig.peer.command();
    assert_ne!(again["id"], cmd["id"]);
    assert_eq!(again["data"]["clubId"], "b-8i");
    rig.card.stop();
}

/// A club picked while the card is between connections is not sent at the
/// reconnect: nothing goes to Pureplay at connect.
#[test]
fn a_pick_between_connections_is_not_sent() {
    let mut rig = Rig::new();
    drop(rig.peer);
    rig.card.shows(ActorStatus::Reconnecting, |_| true);
    send_club(&rig.tx, "web", Club::Iron8);
    let mut peer = rig.mock.next();
    peer.hello_offering("s-1", &["club.set"]);
    assert_eq!(peer.pair().as_deref(), Some(TOKEN));
    peer.paired_with(TOKEN, &["read", "control"]);
    peer.subscribe(&round(), 1);
    rig.card.shows_row(ActorStatus::Connected, "club", "7I");
    let sent = peer.commands_within(QUIET);
    assert!(sent.is_empty(), "{sent:?}");
    rig.card.stop();
}

/// The bag changes between the pick and the send: the bag entry is looked
/// up when the command goes.
#[test]
fn the_bag_entry_is_looked_up_when_the_command_goes() {
    let mut rig = Rig::in_the_air();
    rig.pick(Club::Iron8);
    rig.no_command("the ball is in the air");
    rig.event(
        "player.bag_changed",
        &json!({ "playerId": 1, "bag": [
            { "clubId": "n-dr", "type": "Driver", "name": "Driver" },
            { "clubId": "n-7i", "type": "Iron7", "name": "7 Iron" },
            { "clubId": "n-8i", "type": "Iron8", "name": "8 Iron" } ] }),
    );
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    assert_eq!(rig.peer.command()["data"]["clubId"], "n-8i");
    rig.card.stop();
}

/// Another player's own choice in Pureplay keeps the pick.
#[test]
fn another_players_choice_keeps_the_pick() {
    let mut rig = Rig::in_the_air();
    rig.pick(Club::Iron8);
    rig.no_command("the ball is in the air");
    rig.event(
        "player.club_changed",
        &json!({ "playerId": 2, "current": "Wood3", "reason": "user_selected" }),
    );
    rig.ready(&readiness("shot-2", true, &["club.set"], &[]));
    assert_eq!(rig.peer.command()["data"]["clubId"], "b-8i");
    rig.card.stop();
}

// -- Read Only and Aim and Club ----------------------------------------------------

/// `paired` with no scopes at all (`null`) is Read Only: `read only`, and
/// a pick (the header, the API) sends nothing and puts Pureplay's club
/// back.
#[test]
fn scopes_null_is_read_only() {
    let mock = Mock::new();
    let state = new_state();
    let (tx, _) = broadcast::channel(4096);
    state
        .pureplay_tokens
        .save(&token_key(mock.addr), &Token::new(TOKEN), "client-1")
        .expect("save");
    let tokens = Arc::clone(&state.pureplay_tokens);
    let mut card = Card::start_on(mock.addr, state, tokens, PING_EVERY, &tx);
    let mut peer = mock.next();
    peer.hello_offering("s-1", &["club.set"]);
    assert_eq!(peer.pair().as_deref(), Some(TOKEN));
    peer.send(&json!({ "type": "paired", "data": {
        "token": TOKEN, "clientId": "client-1", "scopes": null } }));
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club_out", "read only");
    send_club(&tx, "web", Club::Iron8);
    card.bus_from(ACTOR, |e| is_club(e, Club::Iron7));
    let sent = peer.commands_within(QUIET);
    assert!(sent.is_empty(), "{sent:?}");
    card.stop();
}

/// The grant changes with a new pairing, without restarting flighthook:
/// Read Only, removed in Pureplay (the saved token refused at the next
/// connection), approved again as Aim and Club, `club_out` `on`, and a
/// pick goes; then the reverse.
#[test]
fn a_new_pairing_with_another_grant_updates_club_out() {
    for (before, after, row_before, row_after) in [
        (&["read"][..], &["read", "control"][..], "read only", "on"),
        (&["read", "control"][..], &["read"][..], "on", "read only"),
    ] {
        let mut rig = Rig::with(&round(), &["club.set"], before);
        rig.card
            .shows_row(ActorStatus::Connected, "club_out", row_before);
        // Pureplay restarts its server; the token is refused next time.
        drop(rig.peer);
        let mut peer = rig.mock.next();
        peer.hello_offering("s-1", &["club.set"]);
        assert_eq!(peer.pair().as_deref(), Some(TOKEN));
        peer.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
        peer.close(4401);
        let mut peer = rig.mock.next();
        peer.hello_offering("s-1", &["club.set"]);
        assert_eq!(peer.pair(), None, "asks again without a token");
        peer.send(&json!({ "type": "pairing.pending", "data": { "expiresInSec": 60 } }));
        rig.card.shows(ActorStatus::Starting, |t| {
            t.get("pairing")
                .is_some_and(|p| p.starts_with("Approve flighthook in Pureplay"))
        });
        peer.paired_with("PPA-NEW", after);
        peer.subscribe(&round(), 1);
        rig.card
            .shows_row(ActorStatus::Connected, "club_out", row_after);
        send_club(&rig.tx, "web", Club::Iron8);
        let sent = if row_after == "on" {
            vec![peer.command()]
        } else {
            rig.card.bus_from(ACTOR, |e| is_club(e, Club::Iron7));
            peer.commands_within(QUIET)
        };
        assert_eq!(sent.len(), usize::from(row_after == "on"), "{row_after}");
        rig.card.stop();
    }
}

/// Removed in Pureplay while connected, then Reconnect: Pureplay's
/// cooldown counts down, the card asks again by itself when it ends, and
/// the new grant sets `club_out`.
#[test]
fn removed_then_reconnect_waits_out_the_cooldown_and_pairs_with_the_new_grant() {
    let mut rig = Rig::with(&round(), &["club.set"], &["read"]);
    rig.card
        .shows_row(ActorStatus::Connected, "club_out", "read only");
    rig.peer.refuse(None, "unauthorised", &json!({}));
    let state = rig.card.state.clone();
    let mock = rig.mock;
    rig.peer.close(4401);
    rig.card
        .shows_row(ActorStatus::Disconnected, "pairing", REMOVED);
    assert!(!mock.reconnects(), "no retry until Reconnect");
    rig.card.stop();
    // Reconnect: the actor starts again on the same state.
    let tokens = Arc::clone(&state.pureplay_tokens);
    let mut card = Card::start_on(mock.addr, state, tokens, PING_EVERY, &rig.tx);
    let mut peer = mock.next();
    peer.hello_offering("s-1", &["club.set"]);
    assert_eq!(peer.pair(), None, "the token went with the removal");
    peer.refuse(
        Some(protocol::PAIR_ID),
        "rate_limited",
        &json!({ "retryAfterMs": 1200 }),
    );
    peer.close(4429);
    card.shows_row(
        ActorStatus::Disconnected,
        "pairing",
        "Pureplay asks to wait (0:01)",
    );
    let mut peer = mock.next();
    peer.hello_offering("s-1", &["club.set"]);
    assert_eq!(peer.pair(), None, "asks again by itself");
    peer.send(&json!({ "type": "pairing.pending", "data": { "expiresInSec": 60 } }));
    card.shows_row(
        ActorStatus::Starting,
        "pairing",
        "Approve flighthook in Pureplay (60 s)",
    );
    peer.paired_with("PPA-NEW", &["read", "control"]);
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club_out", "on");
    card.stop();
}
