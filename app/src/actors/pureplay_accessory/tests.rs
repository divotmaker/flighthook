//! The card's policy, as pure tests of its hooks (one thread, the hooks
//! called as the host calls them), and one smoke test of the card on the
//! driver host with a scripted driver.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use flighthook::{ActorStatus, Club, Handedness, Mode};
use flightrelay::driver::{DeviceEvent, DriverInfo};
use flightrelay::types::roles;
use flightrelay::{
    BusActor, BusReply, BusSync, DeviceInfo, GameAim, GameStroke, GolferSelect, GolferTurn,
    ShotFinished, ShotLaunch, ShotTrigger, Source,
};
use tokio::sync::broadcast;

use super::*;
use crate::actors::scripted::{SOFTWARE, Script, Seen, Software, SoftwareConfig, recv_within};
use crate::actors::simout::{Feed, Routing};
use crate::actors::web::tests::TempDir;
use crate::bus::BusSender;
use crate::state::SystemState;
use crate::state::sent_shots::ECHO_WINDOW;

const ACTOR: &str = "pureplay_accessory.0";
/// Pureplay's address on the card.
const ADDR: &str = "127.0.0.1:9770";

/// The card's hooks with what the host lends them, on a bus the test reads.
struct Rig {
    hooks: PureplayAccessoryHooks,
    section: PureplayAccessorySection,
    state: Arc<SystemState>,
    card: BusSender,
    tx: broadcast::Sender<BusMsg>,
    seen: crate::bus::BusReceiver,
    _dir: TempDir,
}

impl Rig {
    /// The card and a GSPro card (actor `gspro.0`) configured; a link is
    /// up.
    fn new() -> Self {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-pp-accessory-hooks-{}",
            crate::state::config::generate_id()
        )));
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        let section = PureplayAccessorySection {
            actor_id: Some(ACTOR.into()),
            driver: PureplayAccessoryConfig {
                address: Some(ADDR.into()),
                token_file: None,
            },
            ..PureplayAccessorySection::default()
        };
        state.system.update(|c| {
            c.webserver.clear();
            c.frp.actor_id = Some("flighthook".into());
            c.pureplay_accessory.insert("0".into(), section.clone());
            c.openconnect_client.insert(
                "0".into(),
                flighthook::OpenConnectClientSection {
                    actor_id: Some("gspro.0".into()),
                    ..flighthook::OpenConnectClientSection::default()
                },
            );
        });
        state
            .sources
            .register_source(ACTOR, ShotSource::PureplayAccessory);
        let tx = broadcast::channel(1024).0;
        let card = BusSender::new(ACTOR.into(), tx.clone(), Arc::default())
            .with_frp_id(Some(ACTOR.into()));
        let seen = card.subscribe();
        let mut rig = Self {
            hooks: PureplayAccessoryHooks::default(),
            section,
            state: Arc::new(state),
            card,
            tx,
            seen,
            _dir: dir,
        };
        rig.start();
        rig.sent();
        rig
    }

    /// Run `f` on the hooks as the host does; returns what they pushed for
    /// the session.
    fn call(
        &mut self,
        f: impl FnOnce(&mut PureplayAccessoryHooks, &mut HookCtx<'_, PureplayAccessorySection>),
    ) -> Vec<FrpMessage> {
        let mut to_session = Vec::new();
        let mut ctx = HookCtx {
            section: &self.section,
            state: &self.state,
            sender: &self.card,
            status: ActorStatus::Connected,
            to_session: &mut to_session,
        };
        f(&mut self.hooks, &mut ctx);
        to_session
    }

    fn start(&mut self) -> Vec<FrpMessage> {
        self.call(Hooks::<Accessory>::session_started)
    }

    /// The host routes `msg`.
    fn route(&mut self, msg: BusMsg) -> Vec<FrpMessage> {
        self.call(|h, ctx| Hooks::<Accessory>::route(h, &msg, ctx))
    }

    /// The host routes a message of `from`.
    fn from(&mut self, from: &str, body: impl Into<FrpBody>) -> Vec<FrpMessage> {
        self.route(BusMsg::frp(FrpMessage::new(from, body)))
    }

    /// flighthook's turn, caused by `cause`.
    fn turn(&mut self, cause: Option<&str>) -> Vec<FrpMessage> {
        let session = BusSender::new("system".into(), self.tx.clone(), Arc::default())
            .with_frp_id(Some("flighthook".into()));
        let mut rx = session.subscribe();
        session.frp_caused(
            GolferTurn::new(None, Some(Club::Iron8), Handedness::Right, Mode::Full),
            cause.map(str::to_owned),
        );
        let msg = rx.poll().expect("bus").expect("the turn");
        self.route(msg)
    }

    /// The driver sends `body`: what the hooks made of it.
    fn driver_sends(&mut self, mut body: FrpBody) -> (Pass, FrpBody) {
        let mut pass = Pass::Drop;
        self.call(|h, ctx| pass = Hooks::<Accessory>::on_frp(h, &mut body, ctx));
        (pass, body)
    }

    /// The driver reports its shot number `n`: whether it goes on the bus.
    fn shot(&mut self, n: u32) -> Pass {
        self.driver_sends(ShotTrigger::new(key(n)).into()).0
    }

    /// The driver sends Pureplay's stroke for its shot `n`: its `shot_id`
    /// as it goes on the bus.
    fn stroke(&mut self, n: u32) -> Option<String> {
        match self.driver_sends(stroke_for(&key(n)).into()) {
            (Pass::Send, FrpBody::GameStroke(s)) => s.shot_id,
            other => panic!("{other:?}"),
        }
    }

    /// A simulator output sent flighthook's shot `key` to `addr` now.
    fn sent_to(&self, key: &ShotKey, addr: &str) {
        let addr: SocketAddr = addr.parse().expect("addr");
        self.state
            .sent_shots
            .record(key.clone(), SimAddr::of(None, addr), Instant::now());
    }

    /// The card's rows sent since the last call, merged.
    fn rows(&mut self) -> HashMap<String, String> {
        let mut rows = HashMap::new();
        for msg in self.sent() {
            if let BusMsg::Local {
                event: LocalEvent::CardStatus { telemetry, .. },
                ..
            } = msg
            {
                rows.extend(telemetry);
            }
        }
        rows
    }

    fn sent(&mut self) -> Vec<BusMsg> {
        let mut out = Vec::new();
        while let Ok(Some(msg)) = self.seen.poll() {
            out.push(msg);
        }
        out
    }
}

/// The card's shot number `n`.
fn key(n: u32) -> ShotKey {
    ShotKey::new(format!("card-{n}"), n)
}

/// Pureplay's stroke for the shot `key`.
fn stroke_for(key: &ShotKey) -> GameStroke {
    let mut stroke: GameStroke = serde_json::from_value(serde_json::json!({
        "stroke_id": format!("stroke-{}", key.shot_number), "golfer": "pureplay.Joe",
        "hole": 7, "stroke_number": 1, "outcome": "green"
    }))
    .expect("a stroke");
    stroke.shot_id = Some(key.shot_id.clone());
    stroke
}

/// Auto takes what is no echo; From Pureplay takes everything without
/// asking; From flighthook takes nothing but still matches echoes.
#[test]
fn the_shots_setting_decides() {
    use PureplayShots::{Auto, FromFlighthook, FromPureplay};
    assert_eq!(decide(Auto, || None), Decision::Take);
    assert_eq!(decide(Auto, || Some(key(1))), Decision::Echo(key(1)));
    assert_eq!(decide(FromFlighthook, || None), Decision::Ignore);
    assert_eq!(
        decide(FromFlighthook, || Some(key(2))),
        Decision::Echo(key(2))
    );
    let asked = std::cell::Cell::new(false);
    let d = decide(FromPureplay, || {
        asked.set(true);
        Some(key(3))
    });
    assert_eq!(d, Decision::Take);
    assert!(!asked.get(), "From Pureplay does not match echoes");
}

/// The window cases: a send older than the window is no echo (the shot is
/// taken); one send is the echo of one shot, which is dropped and whose
/// stroke lands on the shot flighthook sent; a second shot inside the
/// window with no send left is taken.
#[test]
fn the_echo_window_cases() {
    let mut rig = Rig::new();
    let stale = Instant::now()
        .checked_sub(ECHO_WINDOW + Duration::from_millis(10))
        .expect("an instant");
    rig.state.sent_shots.record(
        ShotKey::new("old", 1),
        SimAddr::of(None, "127.0.0.1:921".parse().expect("addr")),
        stale,
    );
    assert_eq!(rig.shot(1), Pass::Send, "the send expired");
    assert_eq!(rig.state.sent_shots.len(), 0);

    let sent = ShotKey::new("sent-2", 7);
    rig.sent_to(&sent, "127.0.0.1:921");
    assert_eq!(rig.shot(2), Pass::Drop, "the echo");
    assert_eq!(rig.shot(3), Pass::Send, "no send left");
    assert_eq!(rig.stroke(2), Some(sent.shot_id), "on flighthook's shot");
    assert_eq!(rig.stroke(3), Some(key(3).shot_id), "on the card's shot");
}

/// One shot sent by two outputs (Open Connect and Pureplay Connect) is
/// recorded twice; its echo takes both records, so the next shot's echo
/// and result land on the next shot, not on the first one's leftover.
#[test]
fn a_shot_two_outputs_sent_is_echoed_once() {
    let mut rig = Rig::new();
    let (a, b) = (ShotKey::new("shot-a", 1), ShotKey::new("shot-b", 2));
    rig.sent_to(&a, "127.0.0.1:921");
    rig.sent_to(&a, "127.0.0.1:9760");
    assert_eq!(rig.shot(1), Pass::Drop);
    assert_eq!(rig.state.sent_shots.len(), 0, "both records of shot a");
    rig.sent_to(&b, "127.0.0.1:921");
    rig.sent_to(&b, "127.0.0.1:9760");
    assert_eq!(rig.shot(2), Pass::Drop);
    assert_eq!(rig.stroke(1), Some(a.shot_id));
    assert_eq!(rig.stroke(2), Some(b.shot_id), "shot b's result on shot b");
    assert_eq!(rig.state.sent_shots.len(), 0);
}

/// "From flighthook" never takes a shot, and still puts Pureplay's result
/// on the shot flighthook sent (none without a send); "From Pureplay"
/// takes every shot, a send in the window included, with no matching.
#[test]
fn from_flighthook_never_takes_and_from_pureplay_never_matches() {
    let mut rig = Rig::new();
    rig.section.shots = PureplayShots::FromFlighthook;
    let sent = ShotKey::new("sent-1", 5);
    rig.sent_to(&sent, "127.0.0.1:921");
    assert_eq!(rig.shot(1), Pass::Drop);
    assert_eq!(rig.shot(2), Pass::Drop);
    assert_eq!(rig.stroke(1), Some(sent.shot_id));
    assert_eq!(rig.stroke(2), None, "no shot of flighthook's");
    assert_eq!(
        rig.rows().get("shots").map(String::as_str),
        Some("from flighthook")
    );

    rig.section.shots = PureplayShots::FromPureplay;
    rig.sent_to(&ShotKey::new("sent-2", 6), "127.0.0.1:921");
    assert_eq!(rig.shot(3), Pass::Send);
    assert_eq!(
        rig.state.sent_shots.len(),
        1,
        "no matching: the send is left"
    );
    assert_eq!(
        rig.rows().get("shots").map(String::as_str),
        Some("from Pureplay")
    );
}

/// The `shots` row follows the Shots setting while Pureplay sends nothing:
/// any bus message the card takes shows it. In Auto it says where the last
/// shot came from.
#[test]
fn the_shots_row_follows_the_setting_while_pureplay_is_quiet() {
    let mut rig = Rig::new();
    rig.section.shots = PureplayShots::FromPureplay;
    rig.from("mevo.0", BusSync::new());
    assert_eq!(
        rig.rows().get("shots").map(String::as_str),
        Some("from Pureplay")
    );
    rig.section.shots = PureplayShots::FromFlighthook;
    rig.from("mevo.0", BusSync::new());
    assert_eq!(
        rig.rows().get("shots").map(String::as_str),
        Some("from flighthook")
    );
    rig.from("mevo.0", BusSync::new());
    assert!(rig.rows().is_empty(), "nothing changed");

    rig.section.shots = PureplayShots::Auto;
    rig.shot(1);
    let rows = rig.rows();
    assert_eq!(
        rows.get("shots").map(String::as_str),
        Some("auto: last from Pureplay")
    );
    assert!(
        rows.get("last_shot")
            .is_some_and(|v| v.starts_with("#1 at "))
    );
    rig.sent_to(&ShotKey::new("sent", 9), ADDR);
    rig.shot(2);
    assert_eq!(
        rig.rows().get("shots").map(String::as_str),
        Some("auto: last from flighthook")
    );
}

/// In Auto, a shot flighthook sent to Pureplay comes back: the card's
/// shot is dropped (nothing is stored of it) and Pureplay's result goes to
/// the launch monitor's shot; the `shots` row says the last came from
/// flighthook.
#[test]
fn an_echo_is_not_stored_and_gets_the_stroke() {
    let mut rig = Rig::new();
    let mevo = ShotKey::new("mevo-shot-1", 41);
    rig.sent_to(&mevo, ADDR);
    assert_eq!(rig.shot(1), Pass::Drop, "the echo is not taken");
    assert_eq!(
        rig.stroke(1),
        Some(mevo.shot_id),
        "the launch monitor's shot"
    );
    assert_eq!(
        rig.rows().get("shots").map(String::as_str),
        Some("auto: last from flighthook")
    );
}

/// A6: the card numbers every Pureplay shot it reports, echoes included;
/// an echo is dropped, so the numbers on the bus skip (#1, #3).
#[test]
fn every_reported_shot_is_numbered_so_a_dropped_echo_skips_one() {
    let mut rig = Rig::new();
    assert_eq!(rig.shot(1), Pass::Send);
    assert!(rig.rows()["last_shot"].starts_with("#1 at "));
    rig.sent_to(&ShotKey::new("sent", 9), ADDR);
    assert_eq!(rig.shot(2), Pass::Drop);
    assert!(
        !rig.rows().contains_key("last_shot"),
        "an echo is not taken"
    );
    assert_eq!(rig.shot(3), Pass::Send);
    assert!(rig.rows()["last_shot"].starts_with("#3 at "));
}

/// A2: a turn another game or simulator card caused is not handed to the
/// driver, so it never re-asserts against it; flighthook's own turns, a
/// device's and the card's own are.
#[test]
fn a_simulator_cards_turn_is_not_reasserted() {
    let mut rig = Rig::new();
    assert!(rig.turn(Some("gspro.0")).is_empty(), "GSPro's turn");
    for cause in [None, Some("flighthook"), Some("mevo.0"), Some(ACTOR)] {
        let handed = rig.turn(cause);
        assert!(
            matches!(&handed[..], [m] if m.from == "flighthook"
                && matches!(m.body, FrpBody::GolferTurn(_))),
            "{cause:?}: {handed:?}"
        );
    }
}

/// A pick is a `golfer.select` with a club from an actor that is neither
/// the card nor a game or simulator card: the header (flighthook) and a
/// device are handed, GSPro's and the card's own are not, nor a select
/// without a club.
#[test]
fn only_clubs_from_outside_the_simulators_go_to_pureplay() {
    let mut rig = Rig::new();
    let pick = |r: &str, club: Option<Club>| GolferSelect {
        club: club.map(Some),
        ..GolferSelect::new(r)
    };
    assert_eq!(
        rig.from("flighthook", pick("h", Some(Club::Iron9))).len(),
        1
    );
    assert_eq!(rig.from("mevo.0", pick("m", Some(Club::Driver))).len(), 1);
    assert!(
        rig.from("gspro.0", pick("g", Some(Club::Driver)))
            .is_empty()
    );
    assert!(rig.from(ACTOR, pick("c", Some(Club::Driver))).is_empty());
    assert!(rig.from("flighthook", pick("n", None)).is_empty());
}

/// `game.aim` from another actor is handed (the driver refuses it), the
/// card's own is not. Replies to selects come only from flighthook;
/// `bus.sync` from anyone.
#[test]
fn game_aim_is_answered_unsupported() {
    let mut rig = Rig::new();
    let aim: GameAim = serde_json::from_value(serde_json::json!({
        "ref": "a-1", "action": "nudge", "degrees": 1.0
    }))
    .expect("aim");
    assert_eq!(rig.from("gspro.0", aim.clone()).len(), 1);
    assert!(rig.from(ACTOR, aim).is_empty());
    let reply = BusReply::ok(ACTOR, "golfer.select", "r-1");
    assert_eq!(rig.from("flighthook", reply.clone()).len(), 1);
    assert!(rig.from("gspro.0", reply).is_empty());
    assert!(
        rig.from(
            "flighthook",
            BusReply::ok(ACTOR, "practice.new_block", "r-2")
        )
        .is_empty()
    );
    assert_eq!(rig.from("mevo.0", BusSync::new()).len(), 1);
}

/// A new link is handed flighthook's turn first.
#[test]
fn a_new_link_is_handed_the_current_turn() {
    let mut rig = Rig::new();
    let handed = rig.start();
    let turn = rig.state.session.turn();
    assert!(
        matches!(&handed[..], [m] if m.from == "flighthook"
            && matches!(&m.body, FrpBody::GolferTurn(t) if *t == turn)),
        "{handed:?}"
    );
}

/// The card's readiness is its `bus.actor` without blockers; it is no
/// launch monitor a simulator output stands for, on "Any" or routed to the
/// card by id. A launch monitor's still is.
#[test]
fn the_cards_ready_never_reaches_an_output() {
    let rig = Rig::new();
    let card = BusActor::new("Pureplay", 1.0)
        .with_role(roles::GAME)
        .with_role(roles::LAUNCH_MONITOR)
        .with_device(DeviceInfo {
            source: Some(Source::Virtual),
            ..DeviceInfo::default()
        });
    let mevo = BusActor::new("Mevo+", 1.0).with_role(roles::LAUNCH_MONITOR);
    rig.state
        .sources
        .register_source("mevo.0", ShotSource::Mevo);
    let by_id = Routing {
        full_monitor: Some(ACTOR.into()),
        ..Routing::default()
    };
    for routing in [Routing::default(), by_id] {
        let mut feed = Feed::new(routing, Some(Arc::clone(&rig.state)));
        feed.handle(&BusMsg::frp(FrpMessage::new(ACTOR, card.clone())));
        feed.handle(&BusMsg::frp(FrpMessage::new("mevo.0", mevo.clone())));
        let view = feed.in_view();
        assert!(!view.contains_key(ACTOR), "{view:?}");
    }
    let mut any = Feed::new(Routing::default(), Some(Arc::clone(&rig.state)));
    any.handle(&BusMsg::frp(FrpMessage::new("mevo.0", mevo)));
    assert!(any.in_view().contains_key("mevo.0"));
}

/// A configured, connected card is a launch monitor, yet not one a
/// simulator output stands for.
#[test]
fn the_connected_card_is_no_feed_of_a_pureplay_connect_output() {
    let rig = Rig::new();
    assert!(
        rig.state
            .sources
            .source(ACTOR)
            .is_some_and(ShotSource::is_launch_monitor)
    );
    let feed = Feed::new(Routing::default(), Some(Arc::clone(&rig.state)));
    assert!(!feed.feeds_simulators(ACTOR));
}

// -- On the driver host ----------------------------------------------------------

impl SoftwareConfig for PureplayAccessoryConfig {
    fn info(&self) -> DriverInfo {
        <Accessory as FrpDriver>::info(self)
    }

    fn key(&self) -> String {
        self.address.clone().unwrap_or_default()
    }
}

/// What the card on the driver host (as `start_actor` runs it, a scripted
/// driver at `address` in place of the Accessory driver) put on the bus
/// until its `game.stroke`, from its start, with the rig; `prepare` sets the
/// config first. The script reports the card's shot #1 with a launch, a
/// `golfer.profile` of Joe and the shot's stroke.
fn hosted(
    address: &str,
    prepare: impl FnOnce(&mut flighthook::FlighthookConfig),
) -> (Rig, Vec<BusMsg>, Arc<crate::actors::scripted::SeenLog>) {
    let shot = key(1);
    let launch: flighthook::Launch = serde_json::from_value(serde_json::json!({
        "launch_speed": "59.9mps", "launch_elevation": 14.0, "backspin_rpm": 3000
    }))
    .expect("launch");
    let profile: flightrelay::GolferProfile =
        serde_json::from_value(serde_json::json!({ "id": "pureplay.Joe", "name": "Joe" }))
            .expect("profile");
    let script = Script {
        steps: VecDeque::from([vec![
            DeviceEvent::Frp(profile.into()),
            DeviceEvent::Frp(ShotTrigger::new(shot.clone()).into()),
            DeviceEvent::Frp(
                ShotLaunch {
                    key: shot.clone(),
                    launch,
                }
                .into(),
            ),
            DeviceEvent::Frp(ShotFinished { key: shot.clone() }.into()),
            DeviceEvent::Frp(stroke_for(&shot).into()),
        ]]),
        ..Script::default()
    };
    let seen = Arc::clone(&script.seen);
    SOFTWARE.put(address.into(), script);
    let rig = Rig::new();
    let mut bus = rig.tx.subscribe();
    let section = PureplayAccessorySection {
        driver: PureplayAccessoryConfig {
            address: Some(address.into()),
            token_file: None,
        },
        ..rig.section.clone()
    };
    rig.state.system.update(|c| {
        c.pureplay_accessory.insert("0".into(), section.clone());
        prepare(c);
    });
    crate::actors::start_actor_as(
        ACTOR.into(),
        Some(ACTOR.into()),
        Box::new(DriverActor::<
            Software<PureplayAccessoryConfig>,
            PureplayAccessoryHooks,
        >::new(section)),
        &rig.state,
        &rig.tx,
    );
    let mut taken = Vec::new();
    loop {
        let msg = recv_within(&mut bus).expect("the card's stroke");
        let done = matches!(msg.as_frp(), Some(m) if m.from == ACTOR
            && matches!(m.body, FrpBody::GameStroke(_)));
        taken.push(msg);
        if done {
            break;
        }
    }
    assert!(!matches!(
        rig.state.stop_and_wait(ACTOR),
        crate::state::Stopped::Lingering
    ));
    (rig, taken, seen)
}

/// The shots a web server that took `msgs` off the bus stores.
fn stored(state: &Arc<SystemState>, msgs: &[BusMsg]) -> Vec<flighthook::ShotData> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let web = crate::actors::web::tests::folded(Arc::clone(state), msgs).await;
        let shots = web.shots.read().await;
        shots.iter().map(|s| s.data.clone()).collect()
    })
}

/// The export lines of `msgs` under `config`.
fn exported(config: &flighthook::FlighthookConfig, msgs: &[BusMsg]) -> Vec<String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(crate::actors::web::tests::export_lines(config, msgs))
}

/// The card on the driver host: the link is handed flighthook's turn
/// first; Pureplay's shot goes on the bus as the card's own and is stored
/// with its stroke as a virtual launch monitor's shot, which feeds no
/// simulator output; the export carries no address of the link.
#[test]
fn a_pureplay_shot_is_stored_and_reaches_no_output() {
    let address = "127.0.0.1:59770";
    let (rig, taken, seen) = hosted(address, |_| {});
    let handed = seen.lock().clone();
    let first = handed.iter().find_map(|s| match s {
        Seen::Bus(m) => Some(m),
        _ => None,
    });
    assert!(
        first.is_some_and(|m| matches!(m.body, FrpBody::GolferTurn(_))),
        "{handed:?}"
    );
    assert!(taken.iter().any(|m| matches!(m, BusMsg::Local {
        event: LocalEvent::CardStatus { telemetry, .. }, ..
    } if telemetry.get("last_shot").is_some_and(|v| v.starts_with("#1 at ")))));
    let shots = stored(&rig.state, &taken);
    assert_eq!(shots.len(), 1, "{shots:?}");
    assert_eq!(shots[0].card_key(), ACTOR);
    assert_eq!(shots[0].source, Some(flighthook::Source::Virtual));
    assert_eq!(
        shots[0].stroke.as_ref().map(|s| s.outcome),
        Some(flightrelay::StrokeOutcome::Green)
    );
    let lines = exported(&rig.state.system.snapshot(), &taken);
    assert_eq!(lines.len(), 1);
    assert!(!lines[0].contains(address), "{}", lines[0]);
    let feed = Feed::new(Routing::default(), Some(Arc::clone(&rig.state)));
    assert!(!feed.feeds_simulators(ACTOR));
}

/// The card's `bus.actor` (a game issuing the `pureplay` namespace) goes on
/// the bus before the first `golfer.profile` that names the namespace.
#[test]
fn the_bus_actor_comes_before_the_first_profile() {
    let (_rig, taken, _) = hosted("127.0.0.1:59771", |_| {});
    let at = |pred: &dyn Fn(&FrpBody) -> bool| {
        taken
            .iter()
            .position(|m| m.as_frp().is_some_and(|f| f.from == ACTOR && pred(&f.body)))
            .expect("on the bus")
    };
    let actor = at(&|b| {
        matches!(b, FrpBody::BusActor(a)
        if a.roles.iter().any(|r| r == roles::GAME)
            && a.namespaces.iter().any(|n| n == "pureplay"))
    });
    let profile = at(&|b| matches!(b, FrpBody::GolferProfile(_)));
    assert!(actor < profile, "{actor} < {profile}");
}

/// The store joins a `game.stroke` to its shot only when the sender holds
/// `game` (its `bus.actor` says so); a stroke that arrives while its shot
/// is still pending joins it too.
#[test]
fn a_stroke_joins_its_shot_only_from_the_actor_holding_game() {
    let (rig, taken, _) = hosted("127.0.0.1:59772", |_| {});
    let body = |m: &BusMsg| m.as_frp().map(|f| f.body.clone());
    let shots = stored(&rig.state, &taken);
    assert!(shots[0].stroke.is_some(), "from the game");
    let silent: Vec<_> = taken
        .iter()
        .filter(|m| !matches!(body(m), Some(FrpBody::BusActor(_))))
        .cloned()
        .collect();
    let shots = stored(&rig.state, &silent);
    assert_eq!(shots.len(), 1);
    assert!(
        shots[0].stroke.is_none(),
        "from an actor that never said game"
    );
    let mut late = taken.clone();
    let finished = late
        .iter()
        .position(|m| matches!(body(m), Some(FrpBody::ShotFinished(_))))
        .expect("finished");
    let finished = late.remove(finished);
    late.push(finished);
    let shots = stored(&rig.state, &late);
    assert_eq!(shots.len(), 1);
    assert!(shots[0].stroke.is_some(), "the stroke before the finish");
}

/// A golfer listing the player's id: the card's own shot is stored with the
/// player's id on its turn, and the export counts it under the golfer.
#[test]
fn a_linked_profiles_shots_land_under_the_golfer() {
    let (rig, taken, _) = hosted("127.0.0.1:59773", |c| {
        c.golfers = vec![flighthook::GolferRecord {
            identifiers: vec!["flighthook.eric".into(), "pureplay.Joe".into()],
            ..flighthook::GolferRecord::new("flighthook.eric", "Eric")
        }];
    });
    // flighthook's turn names Joe, as the card's select made it.
    let joe = GolferTurn::new(
        Some("pureplay.Joe".parse().expect("id")),
        Some(Club::Iron7),
        Handedness::Right,
        Mode::Full,
    );
    let mut msgs = vec![BusMsg::frp(FrpMessage::new("flighthook", joe))];
    msgs.extend(taken);
    let shots = stored(&rig.state, &msgs);
    assert_eq!(shots.len(), 1);
    let config = rig.state.system.snapshot();
    let turn = shots[0].turn.as_ref().expect("turn");
    assert_eq!(
        config.golfer_key(turn.golfer.as_ref().map(flightrelay::GolferId::as_str)),
        Some("flighthook.eric".into())
    );
    assert_eq!(
        shots[0].stroke.as_ref().map(|s| s.golfer.as_str()),
        Some("pureplay.Joe")
    );
    let lines = exported(&config, &msgs);
    let line: serde_json::Value = serde_json::from_str(&lines[0]).expect("json");
    assert_eq!(line["golfer_id"], "flighthook.eric");
}
