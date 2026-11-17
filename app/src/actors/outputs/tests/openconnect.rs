//! The Open Connect client card's policy: its hooks called as the host
//! calls them, and one smoke test of the card on the driver host.

use std::sync::atomic::AtomicBool;

use flighthook::{OpenConnectClientConfig, OpenConnectClientSection};
use flighthook_openconnect::client::{Client, mapper, output_shot};
use flightrelay::{ShotAggregator, ShotEvent};

use super::*;

/// The card's key and FRP actor id in these tests.
const CARD: &str = "openconnect_client.0";

impl Rig<OpenConnectClientSection> {
    /// An Open Connect card with `routing` (full, chipping, putting) and the
    /// default club lists, flighthook's turn in `mode`.
    fn oc(routing: [Option<&str>; 3], mode: Mode) -> Self {
        Self::with(section(routing), CARD, mode)
    }
}

/// An Open Connect section with `routing`, dialing 127.0.0.1:921.
fn section(routing: [Option<&str>; 3]) -> OpenConnectClientSection {
    let [full, chipping, putting] = routing.map(|r| r.map(str::to_owned));
    OpenConnectClientSection {
        driver: OpenConnectClientConfig {
            address: Some("127.0.0.1:921".into()),
            ..OpenConnectClientConfig::default()
        },
        full_monitor: full,
        chipping_monitor: chipping,
        putting_monitor: putting,
        ..OpenConnectClientSection::default()
    }
}

/// The session's select for the simulator's `club` and `hand`.
fn player(club: Option<Club>, hand: Handedness) -> FrpBody {
    GolferSelect {
        club: club.map(Some),
        handedness: Some(hand),
        ..GolferSelect::new("select-1")
    }
    .into()
}

/// The select the card puts on the bus for the session's `body`.
fn published(rig: &mut Rig<OpenConnectClientSection>, body: FrpBody) -> GolferSelect {
    let (pass, body) = rig.driver_sends(body);
    assert_eq!(pass, Pass::Send);
    let FrpBody::GolferSelect(select) = body else {
        panic!("a select");
    };
    select
}

/// The selects the card sent again on its own since the last call.
fn reasserted(rig: &mut Rig<OpenConnectClientSection>) -> Vec<GolferSelect> {
    let me = rig.card.frp_id().expect("an FRP id").to_owned();
    rig.sent()
        .into_iter()
        .filter_map(|m| match m.as_frp() {
            Some(FrpMessage {
                from,
                body: FrpBody::GolferSelect(s),
            }) if *from == me && s.r#ref.starts_with("reassert-") => Some(s.clone()),
            _ => None,
        })
        .collect()
}

// -- The club-list mode ----------------------------------------------------------

/// The card's default club lists: GW/SW/LW chip, PT putts, any other club
/// is a full swing; the mode goes out in the card's select with its club
/// and hand.
#[test]
fn the_default_club_lists_give_the_mode_in_the_select() {
    let mut rig = Rig::oc([None, None, None], Mode::Full);
    for (club, mode) in [
        (Club::GapWedge, Mode::Chipping),
        (Club::SandWedge, Mode::Chipping),
        (Club::LobWedge, Mode::Chipping),
        (Club::Putter, Mode::Putting),
        (Club::Iron7, Mode::Full),
        (Club::Driver, Mode::Full),
    ] {
        let select = published(&mut rig, player(Some(club), Handedness::Left));
        assert_eq!(select.club, Some(Some(club)));
        assert_eq!(select.handedness, Some(Handedness::Left));
        assert_eq!(select.mode, Some(mode), "{club:?}");
    }
}

/// Empty chipping and putting lists: the card sends no mode.
#[test]
fn empty_club_lists_send_no_mode() {
    let empty = OpenConnectClientSection {
        chipping_clubs: Vec::new(),
        putting_clubs: Vec::new(),
        ..OpenConnectClientSection::default()
    };
    assert_eq!(empty.club_mode(Some(Club::Putter)), None);
    assert_eq!(empty.club_mode(Some(Club::SandWedge)), None);
    let default = OpenConnectClientSection::default();
    assert_eq!(default.club_mode(Some(Club::Putter)), Some(Mode::Putting));
    assert_eq!(default.club_mode(None), Some(Mode::Full));
    let mut rig = Rig::with(empty, CARD, Mode::Full);
    let select = published(&mut rig, player(Some(Club::Putter), Handedness::Right));
    assert_eq!(select.mode, None);
}

/// A player message with a hand and no club holds the hand and no mode:
/// the card asserts no Full it was never told.
#[test]
fn a_hand_without_a_club_holds_no_mode() {
    let mut rig = Rig::oc([None, None, None], Mode::Full);
    let select = published(&mut rig, player(None, Handedness::Left));
    assert_eq!(select.handedness, Some(Handedness::Left));
    assert_eq!(select.mode, None);
    assert_eq!(select.club, None);
}

/// The mode the card re-asserts comes from the club lists in the live
/// config at the time, not from the lists when the club was held.
#[test]
fn a_reassert_recomputes_the_mode_from_the_live_club_lists() {
    let mut rig = Rig::oc([None, None, None], Mode::Full);
    rig.start();
    let select = published(&mut rig, player(Some(Club::SandWedge), Handedness::Right));
    assert_eq!(select.mode, Some(Mode::Chipping));
    rig.sent();
    rig.section.chipping_clubs.clear();
    rig.turn(Club::SandWedge, Mode::Chipping, "flighthook");
    let [again] = &reasserted(&mut rig)[..] else {
        panic!("the turn's Chipping differs from Full: one select");
    };
    assert_eq!(again.mode, Some(Mode::Full));
}

/// While connected, the card sends the select its session sent again
/// (club, hand and the lists' mode) when flighthook's turn differs from
/// it, unless a game or simulator card changed the turn, and not when it
/// matches; nothing once the session ended.
#[test]
fn the_card_reasserts_its_held_select_with_the_mode() {
    let mut rig = Rig::oc([None, None, None], Mode::Full);
    rig.start();
    published(&mut rig, player(Some(Club::SandWedge), Handedness::Right));
    rig.sent();

    rig.turn(Club::SandWedge, Mode::Chipping, "flighthook");
    assert!(reasserted(&mut rig).is_empty(), "the turn matches");
    rig.turn(Club::SandWedge, Mode::Full, "flighthook");
    let [again] = &reasserted(&mut rig)[..] else {
        panic!("only the mode differs: one select again");
    };
    assert_eq!(
        (again.club, again.handedness, again.mode),
        (
            Some(Some(Club::SandWedge)),
            Some(Handedness::Right),
            Some(Mode::Chipping)
        )
    );
    rig.turn(Club::Driver, Mode::Full, "pureplay_accessory.0");
    assert!(reasserted(&mut rig).is_empty(), "a game card changed it");

    rig.end();
    rig.turn(Club::Driver, Mode::Full, "flighthook");
    assert!(
        reasserted(&mut rig).is_empty(),
        "nothing held after the session"
    );
}

/// Two Open Connect cards whose simulators disagree, through the real
/// session (the system actor): the last sender wins, neither re-asserts
/// over the other, a `bus.sync` (the session republishing its turn with its
/// cause) starts no re-assert, and a header pick has both re-assert.
#[test]
fn two_disagreeing_simulator_cards_and_a_sync_through_the_session() {
    let (state, writer, _dir) = rig_state(Mode::Full);
    state.system.update(|c| {
        for (index, id) in [("0", "oc-a"), ("1", "oc-b")] {
            c.openconnect_client.insert(
                index.into(),
                OpenConnectClientSection {
                    actor_id: Some(id.into()),
                    ..section([None, None, None])
                },
            );
        }
    });
    let state = Arc::new(state);
    let (tx, _) = broadcast::channel(1024);
    let mut watch = tx.subscribe();
    let shutdown = Arc::new(AtomicBool::new(false));
    crate::actors::system::start_for_test(writer, &state, &tx, Arc::clone(&shutdown));
    let me = state.flighthook_id();
    let mut a = Rig::on(
        section([None, None, None]),
        "oc-a",
        Arc::clone(&state),
        tx.clone(),
    );
    let mut b = Rig::on(
        section([None, None, None]),
        "oc-b",
        Arc::clone(&state),
        tx.clone(),
    );
    a.start();
    b.start();
    // The session's next turn with `club`, routed to both cards; its cause.
    let mut turned = |a: &mut Rig<_>, b: &mut Rig<_>, club: Club| -> Option<String> {
        loop {
            let msg = recv_within(&mut watch).expect("the session's turn");
            if let Some((turn, cause)) = msg.turn_of(&me)
                && turn.club == Some(club)
            {
                let cause = cause.map(str::to_owned);
                a.route(msg.clone());
                b.route(msg);
                return cause;
            }
        }
    };
    let select = published(&mut a, player(Some(Club::SandWedge), Handedness::Right));
    a.card.frp(select);
    assert_eq!(
        turned(&mut a, &mut b, Club::SandWedge).as_deref(),
        Some("oc-a")
    );
    let select = published(&mut b, player(Some(Club::Driver), Handedness::Right));
    b.card.frp(select);
    assert_eq!(
        turned(&mut a, &mut b, Club::Driver).as_deref(),
        Some("oc-b")
    );
    assert!(
        reasserted(&mut a).is_empty(),
        "the other simulator caused it: last sender wins"
    );
    assert!(reasserted(&mut b).is_empty(), "it holds the turn");

    let _ = tx.send(BusMsg::frp(FrpMessage::new("web", BusSync::new())));
    assert_eq!(
        turned(&mut a, &mut b, Club::Driver).as_deref(),
        Some("oc-b"),
        "republished with its cause"
    );
    assert!(
        reasserted(&mut a).is_empty() && reasserted(&mut b).is_empty(),
        "a sync starts no re-assert"
    );

    // A header pick (flighthook's own select): both simulators win it back.
    let pick = GolferSelect {
        club: Some(Some(Club::Iron7)),
        ..GolferSelect::new("header")
    };
    let _ = tx.send(BusMsg::frp(FrpMessage::new(me.clone(), pick)));
    turned(&mut a, &mut b, Club::Iron7);
    assert_eq!(reasserted(&mut a).len(), 1);
    assert_eq!(reasserted(&mut b).len(), 1);
    shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
}

// -- Routing ---------------------------------------------------------------------

/// Omni for full swings and chips, ExPutt for putts: each mode hands only
/// its own monitor's shot finish, and the view (the readiness) follows that
/// monitor.
#[test]
fn full_shots_and_putts_come_from_their_own_monitors() {
    let mut rig = Rig::oc(
        [Some("square.0"), Some("square.0"), Some("exputt.0")],
        Mode::Full,
    );
    let finishes = |handed: &[FrpMessage]| -> Vec<String> {
        handed
            .iter()
            .filter(|m| matches!(m.body, FrpBody::ShotFinished(_)))
            .map(|m| m.from.clone())
            .collect()
    };
    for (n, (mode, from)) in (1..).zip([
        (Mode::Full, "square.0"),
        (Mode::Chipping, "square.0"),
        (Mode::Putting, "exputt.0"),
    ]) {
        rig.turn(Club::Iron7, mode, "flighthook");
        let mut handed = shot(&mut rig, "square.0", 2 * n);
        handed.extend(shot(&mut rig, "exputt.0", 2 * n + 1));
        assert_eq!(finishes(&handed), [from], "{mode:?}");
    }

    let mut busy = hosted("square.0");
    busy.blockers.push("not_ready".into());
    rig.from("square.0", busy);
    rig.from("exputt.0", hosted("exputt.0"));
    let ready = |view: &[FrpMessage]| {
        view.iter()
            .any(|m| matches!(&m.body, FrpBody::BusActor(a) if a.is_clear()))
    };
    assert!(ready(&rig.start()), "putting follows the ExPutt");
    let handed = rig.turn(Club::Iron7, Mode::Full, "flighthook");
    assert_eq!(
        kinds(&handed),
        [
            pair("exputt.0", "bus.actor removed"),
            pair("square.0", "bus.actor")
        ]
    );
    assert!(!ready(&handed), "full follows the Omni");
}

#[test]
fn an_exputt_with_putting_routed_to_any_is_warned_about() {
    let mut config = flighthook::FlighthookConfig::default();
    let any = Routing::default();
    assert_eq!(putting_routing_warning(&config, &any), None, "no ExPutt");
    config.exputt.insert(
        "0".into(),
        flighthook::ExPuttSection {
            name: "ExPutt".into(),
            ..flighthook::ExPuttSection::default()
        },
    );
    assert!(putting_routing_warning(&config, &any).is_some());
    let routed = Routing {
        putting_monitor: Some("exputt.0".into()),
        ..Routing::default()
    };
    assert_eq!(putting_routing_warning(&config, &routed), None);
}

/// An ExPutt shot in Full mode is handed while the Full Monitor accepts any
/// device, and not when it names another device.
#[test]
fn an_exputt_shot_in_full_mode_follows_the_full_monitor() {
    for (full, handed) in [(None, true), (Some("square.0"), false)] {
        let mut rig = Rig::oc([full, None, Some("exputt.0")], Mode::Full);
        let finished = shot(&mut rig, "exputt.0", 1)
            .iter()
            .any(|m| matches!(m.body, FrpBody::ShotFinished(_)));
        assert_eq!(finished, handed, "{full:?}");
    }
}

/// A card started while flighthook is in Putting routes the first shot as
/// a putt.
#[test]
fn a_card_restarted_while_putting_routes_as_putting_at_once() {
    let mut rig = Rig::oc([Some("mevo.0"), None, Some("putter.0")], Mode::Putting);
    let finishes = |handed: Vec<FrpMessage>| {
        handed
            .iter()
            .any(|m| matches!(m.body, FrpBody::ShotFinished(_)))
    };
    assert!(!finishes(shot(&mut rig, "mevo.0", 12)));
    assert!(finishes(shot(&mut rig, "putter.0", 13)));
}

/// An Auto-chip is handed whatever monitor each mode is routed to, without
/// its flight, and goes out as a straight 45 deg chip with its solved
/// speed, speed-scaled backspin, no sidespin, no carry and zeroed club
/// data; under the same routing another monitor's shot finish is not
/// handed.
#[test]
fn an_auto_chip_is_sent_whatever_the_routing() {
    let mut rig = Rig::oc([Some("mevo.0"), Some("mevo.0"), Some("mevo.0")], Mode::Full);
    let chips = std::sync::atomic::AtomicU32::new(0);
    for (n, mode) in (1..).zip([Mode::Full, Mode::Chipping, Mode::Putting]) {
        rig.turn(Club::Iron7, mode, "flighthook");
        rig.sent();
        let (key, ball) = crate::autochip::send(&rig.state, &rig.tx, 3, &chips).expect("solved");
        assert_eq!(key.shot_number, n);
        let mut handed = Vec::new();
        for msg in rig.sent() {
            handed.extend(rig.route(msg));
        }
        assert!(
            !handed
                .iter()
                .any(|m| matches!(m.body, FrpBody::ShotFlight(_))),
            "{mode:?}: no Auto-chip flight"
        );
        let mut shots = ShotAggregator::new();
        let done = handed
            .iter()
            .find_map(|m| match shots.feed(m) {
                Some(ShotEvent::Finished(shot)) => Some(shot),
                _ => None,
            })
            .expect("the chip is handed whole");
        let msg = mapper::map_shot(&output_shot(&done, &done.from));
        assert_eq!(msg.shot_number, n);
        let speed = ball.launch_speed.expect("speed").as_mph();
        assert!((msg.ball_data.speed - speed).abs() < 1e-9);
        assert!(speed > 5.0 && speed < 12.0, "{speed}");
        assert_eq!(msg.ball_data.vla, 45.0);
        assert_eq!(msg.ball_data.hla, 0.0);
        assert_eq!(msg.ball_data.back_spin, (300.0 * speed).round());
        assert_eq!(msg.ball_data.side_spin, 0.0);
        assert_eq!(msg.ball_data.spin_axis, 0.0);
        assert_eq!(msg.ball_data.carry_distance, None);
        assert_eq!(msg.club_data.speed, 0.0);
        assert_eq!(msg.club_data.loft, 0.0);

        let other = shot(&mut rig, "square.0", n);
        assert!(
            !other
                .iter()
                .any(|m| matches!(m.body, FrpBody::ShotFinished(_))),
            "{mode:?}"
        );
    }
}

/// The Pureplay Accessory card feeds no client: its shots and `bus.actor`
/// are not handed under "Any" or routed to it by id, known by the `game`
/// role in its `bus.actor` or registered as a simulator source; a launch
/// monitor, Auto-chip and an output still feed the clients.
#[test]
fn a_simulator_source_feeds_no_open_connect_client() {
    let card = "pureplay_accessory.0";
    for routing in [[None, None, None], [Some(card), Some(card), Some(card)]] {
        let mut rig = Rig::oc(routing, Mode::Full);
        // Registered, it is refused before its `bus.actor` arrives.
        assert!(shot(&mut rig, card, 1).is_empty(), "{routing:?}");
        let mut accessory = hosted(card);
        accessory.roles.push(roles::GAME.into());
        assert!(rig.from(card, accessory).is_empty(), "{routing:?}");
        assert!(shot(&mut rig, card, 2).is_empty(), "{routing:?}");
        assert!(rig.start().is_empty(), "{routing:?}");
    }
    let feed = Feed::new(Routing::default(), None);
    assert!(feed.feeds_simulators("mevo.0"));
    assert!(feed.feeds_simulators(flighthook::AUTO_CHIP_ACTOR));
    assert!(feed.feeds_simulators("pureplay.0"));
}

/// The card's first call asks the actors up to republish their state, once;
/// a launch monitor that answers is handed, and a session that connects is
/// handed the whole view first.
#[test]
fn a_card_added_after_the_monitor_is_up_reports_ready() {
    let mut rig = Rig::oc([None, None, None], Mode::Full);
    assert!(rig.start().is_empty(), "nothing in view yet");
    let syncs = rig
        .sent()
        .iter()
        .filter(|m| {
            m.as_frp()
                .is_some_and(|f| matches!(f.body, FrpBody::BusSync(_)))
        })
        .count();
    assert_eq!(syncs, 1);
    assert_eq!(
        kinds(&rig.from("mevo.1", hosted("mevo.1"))),
        [pair("mevo.1", "bus.actor")]
    );
    rig.end();
    let view = rig.start();
    assert!(matches!(&view[..], [m] if matches!(&m.body, FrpBody::BusActor(a) if a.is_clear())));
}

// -- What comes back -------------------------------------------------------------

/// Each shot the driver wrote with a shot's key is recorded in the send
/// registry under the simulator's configured address, and shown on the
/// card with the `ShotNumber` the simulator got.
#[test]
fn each_written_shot_is_recorded_with_its_shot_number() {
    let mut rig = Rig::oc([None, None, None], Mode::Full);
    rig.start();
    rig.sent();
    let json = br#"{"DeviceID":"Flighthook","Units":"Yards","ShotNumber":42,"APIversion":"1"}"#;
    let frame = FrpBody::ShotRaw(RawFrame {
        key: Some(key_of("mevo.0", 7)),
        label: Some("shot".into()),
        ..RawFrame::new(RawDir::Tx, 0, json.to_vec())
    });
    assert_eq!(rig.driver_sent(frame), Pass::Send);
    let gspro = SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 921)));
    assert_eq!(
        rig.state.sent_shots.take_echo(gspro, Instant::now()),
        Some(key_of("mevo.0", 7))
    );
    let rows: Vec<_> = rig
        .sent()
        .into_iter()
        .filter_map(|m| match m {
            BusMsg::Local {
                event: LocalEvent::CardStatus { telemetry, .. },
                ..
            } => Some(telemetry),
            _ => None,
        })
        .collect();
    let [telemetry] = &rows[..] else {
        panic!("{rows:?}");
    };
    assert_eq!(telemetry["shots_sent"], "1");
    assert!(
        telemetry["last_shot"].starts_with("#42 at "),
        "{telemetry:?}"
    );
}

// -- On the driver host ----------------------------------------------------------

impl SoftwareConfig for OpenConnectClientConfig {
    fn info(&self) -> DriverInfo {
        <Client as FrpDriver>::info(self)
    }

    fn key(&self) -> String {
        self.address.clone().unwrap_or_default()
    }
}

/// The card on the driver host, as `start_actor` runs it, with a scripted
/// driver: the session is handed a launch monitor's `bus.actor`, and the
/// shot it writes with a shot's key lands in the app's send registry.
#[test]
fn the_started_actor_records_its_shots_in_the_apps_registry() {
    let address = "127.0.0.1:59921";
    let key = key_of("mevo.0", 12);
    let script = Script {
        steps: VecDeque::from([vec![DeviceEvent::Frp(FrpBody::ShotRaw(RawFrame {
            key: Some(key.clone()),
            ..RawFrame::new(RawDir::Tx, 0, br#"{"ShotNumber":12}"#.to_vec())
        }))]]),
        ..Script::default()
    };
    let seen = Arc::clone(&script.seen);
    SOFTWARE.put(address.into(), script);
    let rig = Rig::oc([None, None, None], Mode::Full);
    let mut bus = rig.tx.subscribe();
    crate::actors::start_actor_as(
        CARD.into(),
        Some(CARD.into()),
        Box::new(DriverActor::<
            Software<OpenConnectClientConfig>,
            OpenConnectClientHooks,
        >::new(OpenConnectClientSection {
            driver: OpenConnectClientConfig {
                address: Some(address.into()),
                ..OpenConnectClientConfig::default()
            },
            ..OpenConnectClientSection::default()
        })),
        &rig.state,
        &rig.tx,
    );
    // The card asks the actors up for their state; the Mevo answers.
    while !matches!(
        recv_within(&mut bus).expect("the card's bus.sync"),
        BusMsg::Frp { msg, .. } if msg.from == CARD && matches!(msg.body, FrpBody::BusSync(_))
    ) {}
    let monitor = BusSender::new("mevo.0".into(), rig.tx.clone(), Arc::default())
        .with_frp_id(Some("mevo.0".into()));
    monitor.frp(hosted("mevo.0"));
    // The card's `shots_sent` row follows the record of the shot.
    while !matches!(
        recv_within(&mut bus).expect("the card's shots_sent row"),
        BusMsg::Local { card, event: LocalEvent::CardStatus { telemetry, .. } }
            if card == CARD && telemetry.get("shots_sent").is_some_and(|n| n == "1")
    ) {}
    seen.wait_until(|seen| {
        seen.iter()
            .any(|s| matches!(s, Seen::Bus(m) if m.from == "mevo.0"))
    });
    let gspro = SimAddr::of(None, address.parse().expect("addr"));
    assert_eq!(
        rig.state.sent_shots.take_echo(gspro, Instant::now()),
        Some(key)
    );
    assert!(!matches!(
        rig.state.stop_and_wait(CARD),
        crate::state::Stopped::Lingering
    ));
}
