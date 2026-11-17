//! The simulator output cards' policy, as pure tests of their hooks (one
//! thread, the hooks called as the host calls them), and one smoke test of
//! each card on the driver host with a scripted driver. Pureplay Connect
//! here, the Open Connect client in [`openconnect`].

use std::collections::VecDeque;
use std::net::SocketAddr;

use flighthook::{ActorStatus, Club, Handedness, Mode};
use flighthook_pureplay::connect::Config;
use flightrelay::driver::{DeviceEvent, DriverInfo};
use flightrelay::types::roles;
use flightrelay::{
    BusActor, DeviceInfo, FlightResult, FrpMessage, GolferSelect, GolferTurn, RawFrame, Removal,
    ShotFinished, ShotFlight, ShotKey, ShotLaunch, ShotTrigger, Source, StateKind,
};
use tokio::sync::broadcast;

use super::*;
use crate::actors::scripted::{SOFTWARE, Script, Seen, Software, SoftwareConfig, recv_within};
use crate::actors::web::tests::TempDir;
use crate::bus::{BusReceiver, BusSender, PeerId};
use crate::state::SystemState;

/// A simulator output's section, with its driver for the hooks' calls.
trait Card: OutputSection {
    type D: FrpDriver<Config = <Self as flighthook::DeviceSection>::Driver>;
}

impl Card for PureplaySection {
    type D = Connect;
}

impl Card for flighthook::OpenConnectClientSection {
    type D = flighthook_openconnect::client::Client;
}

/// The card's hooks with what the host lends them: the card's section,
/// flighthook's state and the card's sender, on a bus the test reads.
struct Rig<S: Card> {
    hooks: OutputHooks<S>,
    section: S,
    state: Arc<SystemState>,
    card: BusSender,
    /// The status the host lends the hooks.
    status: ActorStatus,
    tx: broadcast::Sender<BusMsg>,
    /// What the hooks put on the bus.
    seen: BusReceiver,
    _dir: Option<TempDir>,
}

/// The actors these tests use, with their shot sources.
const SOURCES: [(&str, ShotSource); 7] = [
    ("mevo.0", ShotSource::Mevo),
    ("r10.0", ShotSource::R10),
    ("square.0", ShotSource::Square),
    ("mock_monitor.0", ShotSource::MockMonitor),
    ("openconnect_server.0", ShotSource::OpenConnectServer),
    (flighthook::AUTO_CHIP_ACTOR, ShotSource::AutoChip),
    ("pureplay_accessory.0", ShotSource::PureplayAccessory),
];

/// The app's state for these tests, with flighthook's turn in `mode`, and
/// its session writer.
fn rig_state(mode: Mode) -> (SystemState, crate::state::SessionWriter, TempDir) {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "flighthook-output-hooks-{}",
        crate::state::config::generate_id()
    )));
    let (state, writer) = SystemState::new(dir.0.join("config.toml"));
    state.system.update(|c| {
        c.webserver.clear();
        c.frp.actor_id = Some("flighthook".into());
        c.frp.auto_chip_actor_id = Some(flighthook::AUTO_CHIP_ACTOR.into());
        c.pureplay_accessory.insert(
            "0".into(),
            flighthook::PureplayAccessorySection {
                actor_id: Some("pureplay_accessory.0".into()),
                ..flighthook::PureplayAccessorySection::default()
            },
        );
    });
    for (id, source) in SOURCES {
        state.sources.register_source(id, source);
    }
    writer.apply(&GolferSelect {
        mode: Some(mode),
        ..GolferSelect::new("start")
    });
    (state, writer, dir)
}

impl<S: Card> Rig<S> {
    /// Card `key` (its FRP actor id too) with `section`, on `state` and the
    /// bus `tx`.
    fn on(section: S, key: &str, state: Arc<SystemState>, tx: broadcast::Sender<BusMsg>) -> Self {
        let card =
            BusSender::new(key.into(), tx.clone(), Arc::default()).with_frp_id(Some(key.into()));
        let seen = card.subscribe();
        Self {
            hooks: OutputHooks::default(),
            section,
            state,
            card,
            status: ActorStatus::Connected,
            tx,
            seen,
            _dir: None,
        }
    }

    /// Card `key` with `section`, flighthook's turn in `mode`.
    fn with(section: S, key: &str, mode: Mode) -> Self {
        let (state, _writer, dir) = rig_state(mode);
        let mut rig = Self::on(section, key, Arc::new(state), broadcast::channel(1024).0);
        rig._dir = Some(dir);
        rig
    }
}

impl Rig<PureplaySection> {
    /// A card with `routing` (full, chipping, putting), flighthook's turn
    /// in `mode`.
    fn new(routing: [Option<&str>; 3], mode: Mode) -> Self {
        let [full, chipping, putting] = routing.map(|r| r.map(str::to_owned));
        Self::with(
            PureplaySection {
                driver: Config {
                    address: Some("127.0.0.1:9760".into()),
                },
                full_monitor: full,
                chipping_monitor: chipping,
                putting_monitor: putting,
                ..PureplaySection::default()
            },
            "pureplay.0",
            mode,
        )
    }

    /// A card that routes every mode to "Any", in Full.
    fn any() -> Self {
        Self::new([None, None, None], Mode::Full)
    }
}

impl<S: Card> Rig<S> {
    /// Run `f` on the hooks as the host does; returns what they pushed for
    /// the session.
    fn call(
        &mut self,
        f: impl FnOnce(&mut OutputHooks<S>, &mut HookCtx<'_, S>),
    ) -> Vec<FrpMessage> {
        let mut to_session = Vec::new();
        let mut ctx = HookCtx {
            section: &self.section,
            state: &self.state,
            sender: &self.card,
            status: self.status,
            to_session: &mut to_session,
        };
        f(&mut self.hooks, &mut ctx);
        to_session
    }

    /// The host routes `msg`.
    fn route(&mut self, msg: BusMsg) -> Vec<FrpMessage> {
        self.call(|h, ctx| Hooks::<S::D>::route(h, &msg, ctx))
    }

    /// The host routes a message of local actor `from`.
    fn from(&mut self, from: &str, body: impl Into<FrpBody>) -> Vec<FrpMessage> {
        self.route(BusMsg::frp(FrpMessage::new(from, body)))
    }

    /// A session connected.
    fn start(&mut self) -> Vec<FrpMessage> {
        self.call(Hooks::<S::D>::session_started)
    }

    /// The session ended.
    fn end(&mut self) {
        self.call(Hooks::<S::D>::session_ended);
    }

    /// The driver sent `body`.
    fn driver_sent(&mut self, body: FrpBody) -> Pass {
        self.driver_sends(body).0
    }

    /// The driver sent `body`: what the hooks made of it.
    fn driver_sends(&mut self, mut body: FrpBody) -> (Pass, FrpBody) {
        let mut pass = Pass::Drop;
        self.call(|h, ctx| pass = Hooks::<S::D>::on_frp(h, &mut body, ctx));
        (pass, body)
    }

    /// flighthook's turn, caused by `cause`.
    fn turn(&mut self, club: Club, mode: Mode, cause: &str) -> Vec<FrpMessage> {
        let session = BusSender::new("system".into(), self.tx.clone(), Arc::default())
            .with_frp_id(Some("flighthook".into()));
        let mut rx = session.subscribe();
        session.frp_caused(
            GolferTurn::new(None, Some(club), Handedness::Right, mode),
            Some(cause.into()),
        );
        let msg = rx.poll().expect("bus").expect("the turn");
        self.route(msg)
    }

    /// What the hooks put on the bus since the last call.
    fn sent(&mut self) -> Vec<BusMsg> {
        let mut out = Vec::new();
        while let Ok(Some(msg)) = self.seen.poll() {
            out.push(msg);
        }
        out
    }
}

/// The `bus.actor` of launch monitor `actor` as its host publishes it: role
/// `launch_monitor` and the source its type declares, ready.
fn hosted(actor: &str) -> BusActor {
    let source = SOURCES
        .iter()
        .find(|(id, _)| *id == actor)
        .map(|(_, s)| s.provenance());
    BusActor::new(actor, 1.0)
        .with_role(roles::LAUNCH_MONITOR)
        .with_device(DeviceInfo {
            source,
            ..DeviceInfo::default()
        })
}

/// Shot `n` of launch monitor `actor`.
fn key_of(actor: &str, n: u32) -> ShotKey {
    ShotKey::new(format!("{actor}:shot-{n}"), n)
}

fn launch(key: &ShotKey) -> ShotLaunch {
    ShotLaunch {
        key: key.clone(),
        launch: flighthook::Launch {
            launch_speed: Some(flighthook::Velocity::MetersPerSecond(60.0)),
            ..flighthook::Launch::default()
        },
    }
}

/// Each message's sender and kind (`"<kind> removed"` for a removal).
fn kinds(msgs: &[FrpMessage]) -> Vec<(String, String)> {
    msgs.iter()
        .map(|m| {
            let kind = match &m.body {
                FrpBody::Removed(_) => format!("{} removed", m.body.kind()),
                body => body.kind().to_owned(),
            };
            (m.from.clone(), kind)
        })
        .collect()
}

fn pair(from: &str, kind: &str) -> (String, String) {
    (from.to_owned(), kind.to_owned())
}

/// Put shot `n` of `actor` through the hooks (trigger, launch, finished);
/// returns what the session was handed.
fn shot<S: Card>(rig: &mut Rig<S>, actor: &str, n: u32) -> Vec<FrpMessage> {
    let key = key_of(actor, n);
    let mut handed = rig.from(actor, ShotTrigger::new(key.clone()));
    handed.extend(rig.from(actor, launch(&key)));
    handed.extend(rig.from(actor, ShotFinished { key }));
    handed
}

// -- What the session sees -------------------------------------------------------

/// Routed by ID, the session sees the routed monitor alone; under "Any",
/// every launch monitor that may feed a simulator (never Auto-chip, never
/// an output). A turn that routes elsewhere takes the monitor out of view.
#[test]
fn the_routed_monitor_alone_sets_the_state() {
    let mut rig = Rig::new([Some("square.0"), Some("mevo.0"), None], Mode::Full);
    assert!(rig.from("mevo.0", hosted("mevo.0")).is_empty());
    assert!(
        rig.from("openconnect_client.0", BusActor::new("GSPro", 1.0))
            .is_empty()
    );
    let handed = rig.from("square.0", hosted("square.0"));
    assert_eq!(kinds(&handed), [pair("square.0", "bus.actor")]);
    // Its next `bus.actor` is handed when it changes, not when it repeats.
    assert!(rig.from("square.0", hosted("square.0")).is_empty());
    let mut busy = hosted("square.0");
    busy.blockers.push("no_ball".into());
    assert_eq!(rig.from("square.0", busy).len(), 1);

    let handed = rig.turn(Club::SandWedge, Mode::Chipping, "flighthook");
    assert_eq!(
        kinds(&handed),
        [
            pair("square.0", "bus.actor removed"),
            pair("mevo.0", "bus.actor")
        ]
    );

    let mut any = Rig::any();
    for actor in ["mevo.0", "square.0", flighthook::AUTO_CHIP_ACTOR] {
        any.from(actor, hosted(actor));
    }
    let view = any.start();
    assert_eq!(
        kinds(&view),
        [pair("mevo.0", "bus.actor"), pair("square.0", "bus.actor")]
    );
}

/// The parts of every shot of a monitor that feeds a simulator are handed;
/// its finish only when the routing passes the shot. Auto-chip's shots
/// pass, without its flight.
#[test]
fn shots_routed_elsewhere_are_not_handed() {
    let mut rig = Rig::new([Some("square.0"), None, None], Mode::Full);
    let handed = shot(&mut rig, "mevo.0", 1);
    assert_eq!(
        kinds(&handed),
        [
            pair("mevo.0", "shot.trigger"),
            pair("mevo.0", "shot.launch")
        ],
        "no finish: routed elsewhere"
    );
    let handed = shot(&mut rig, "square.0", 2);
    assert_eq!(handed.len(), 3);
    assert!(matches!(handed[2].body, FrpBody::ShotFinished(_)));

    let chip = flighthook::AUTO_CHIP_ACTOR;
    let key = key_of(chip, 1);
    let mut handed = rig.from(chip, ShotTrigger::new(key.clone()));
    handed.extend(rig.from(
        chip,
        ShotFlight {
            key: key.clone(),
            flight: FlightResult::new(flighthook::Basis::Modeled),
        },
    ));
    handed.extend(rig.from(chip, ShotFinished { key }));
    assert_eq!(
        kinds(&handed),
        [pair(chip, "shot.trigger"), pair(chip, "shot.finished")]
    );
}

/// Another actor's launch or finish of a monitor's shot is not handed, and
/// neither is a flight model's flight: the session composes the shot from
/// its launch monitor's own messages.
#[test]
fn strict_mode_sends_the_launch_monitors_own_launch_only() {
    let mut rig = Rig::any();
    let key = key_of("r10.0", 1);
    rig.from("r10.0", ShotTrigger::new(key.clone()));
    assert!(rig.from("intruder", launch(&key)).is_empty());
    assert!(
        rig.from("intruder", ShotFinished { key: key.clone() })
            .is_empty()
    );
    let flight = ShotFlight {
        key: key.clone(),
        flight: FlightResult::new(flighthook::Basis::Modeled),
    };
    assert!(rig.from("simulation", flight.clone()).is_empty());
    assert_eq!(rig.from("r10.0", flight).len(), 1, "its own flight");
    assert_eq!(rig.from("r10.0", launch(&key)).len(), 1);
    assert_eq!(rig.from("r10.0", ShotFinished { key }).len(), 1);
}

/// The Pureplay Accessory card's `bus.actor` and shots never reach the
/// session, under "Any" and routed to it by ID.
#[test]
fn a_pureplay_card_feeds_no_pureplay_connect_output() {
    let card = "pureplay_accessory.0";
    for routing in [[None, None, None], [Some(card), Some(card), Some(card)]] {
        let mut rig = Rig::new(routing, Mode::Full);
        let mut accessory = hosted(card);
        accessory.roles.push(roles::GAME.into());
        assert!(rig.from(card, accessory).is_empty(), "{routing:?}");
        assert!(shot(&mut rig, card, 1).is_empty(), "{routing:?}");
        assert!(rig.start().is_empty(), "{routing:?}");
    }
}

/// A monitor that leaves the bus leaves the view: its `bus.actor` removal,
/// or a remote one that went silent (the system actor says it is gone).
#[test]
fn a_virtual_card_turned_off_is_forgotten() {
    let mut rig = Rig::any();
    rig.from("mock_monitor.0", hosted("mock_monitor.0"));
    rig.from("mevo.0", hosted("mevo.0"));
    let handed = rig.from("mock_monitor.0", Removal::of(StateKind::BusActor));
    assert_eq!(
        kinds(&handed),
        [pair("mock_monitor.0", "bus.actor removed")]
    );
    assert_eq!(kinds(&rig.start()), [pair("mevo.0", "bus.actor")]);
}

/// A remote launch monitor seen under "Any" is handed, and taken out of
/// view once it went silent.
#[test]
fn a_gone_remote_monitor_is_forgotten() {
    const REMOTE: &str = "aaaa000000000001";
    let mut rig = Rig::any();
    let mut remote = hosted(REMOTE);
    remote.device = Some(DeviceInfo {
        source: Some(Source::Virtual),
        ..DeviceInfo::default()
    });
    let handed = rig.route(BusMsg::from_peer(
        FrpMessage::new(REMOTE, remote),
        PeerId(77),
    ));
    assert_eq!(kinds(&handed), [pair(REMOTE, "bus.actor")]);
    let handed = rig.route(BusMsg::local(
        "system",
        LocalEvent::ActorGone {
            actor: REMOTE.into(),
        },
    ));
    assert_eq!(kinds(&handed), [pair(REMOTE, "bus.actor removed")]);
}

/// A card started while flighthook is in Putting routes for Putting from
/// its first message.
#[test]
fn the_detection_mode_at_start_routes_the_first_shots() {
    let mut rig = Rig::new([Some("mevo.0"), None, Some("square.0")], Mode::Putting);
    assert!(rig.from("mevo.0", hosted("mevo.0")).is_empty());
    assert_eq!(rig.from("square.0", hosted("square.0")).len(), 1);
    assert_eq!(shot(&mut rig, "square.0", 1).len(), 3);
}

/// The card's first call asks the actors up to republish their state, once;
/// a session that connects is handed the whole view first.
#[test]
fn a_card_added_after_the_omni_connected_sends_its_first_shot_in_strict_mode() {
    let mut rig = Rig::any();
    assert!(rig.start().is_empty(), "nothing in view yet");
    let syncs = |sent: &[BusMsg]| {
        sent.iter()
            .filter(|m| {
                m.as_frp()
                    .is_some_and(|f| matches!(f.body, FrpBody::BusSync(_)))
            })
            .count()
    };
    assert_eq!(syncs(&rig.sent()), 1);
    rig.from("square.0", hosted("square.0"));
    rig.end();
    assert_eq!(kinds(&rig.start()), [pair("square.0", "bus.actor")]);
    assert_eq!(syncs(&rig.sent()), 0, "asked once");
}

// -- What comes back -------------------------------------------------------------

/// Each BallData the driver wrote with a shot's key is recorded in the
/// send registry under Pureplay's configured address, and counted on the
/// card, with the status the host has sent for it; other frames are not.
#[test]
fn each_written_shot_is_recorded_as_sent() {
    let mut rig = Rig::any();
    rig.start();
    rig.status = ActorStatus::Reconnecting;
    rig.sent();
    let frame = |key: Option<ShotKey>, dir| {
        FrpBody::ShotRaw(RawFrame {
            key,
            ..RawFrame::new(dir, 0, vec![0x50, 0x4C])
        })
    };
    assert_eq!(rig.driver_sent(frame(None, RawDir::Tx)), Pass::Send);
    assert_eq!(
        rig.driver_sent(frame(Some(key_of("square.0", 7)), RawDir::Rx)),
        Pass::Send
    );
    assert_eq!(rig.state.sent_shots.len(), 0);
    assert_eq!(
        rig.driver_sent(frame(Some(key_of("square.0", 7)), RawDir::Tx)),
        Pass::Send
    );
    let pureplay = SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 9760)));
    assert_eq!(
        rig.state.sent_shots.take_echo(pureplay, Instant::now()),
        Some(key_of("square.0", 7))
    );
    let rows: Vec<_> = rig
        .sent()
        .into_iter()
        .filter_map(|m| match m {
            BusMsg::Local {
                event: LocalEvent::CardStatus { status, telemetry },
                ..
            } => Some((status, telemetry)),
            _ => None,
        })
        .collect();
    let [(ActorStatus::Reconnecting, telemetry)] = &rows[..] else {
        panic!("{rows:?}");
    };
    assert_eq!(telemetry["shots_sent"], "1");
    assert!(
        telemetry["last_shot"].starts_with("#1 at "),
        "{telemetry:?}"
    );
}

/// While linked, the card sends the select its session sent again (club,
/// hand and mode) when flighthook's turn differs from it, unless a game or
/// simulator card changed the turn, and not when the turn matches.
#[test]
fn the_card_reasserts_its_held_select_with_the_mode() {
    let mut rig = Rig::any();
    let selects = |rig: &mut Rig<PureplaySection>| -> Vec<GolferSelect> {
        rig.sent()
            .into_iter()
            .filter_map(|m| match m.as_frp().map(|f| f.body.clone()) {
                Some(FrpBody::GolferSelect(s)) => Some(s),
                _ => None,
            })
            .collect()
    };
    let held = GolferSelect {
        club: Some(Some(Club::SandWedge)),
        handedness: Some(Handedness::Right),
        mode: Some(Mode::Chipping),
        ..GolferSelect::new("select-1")
    };
    rig.start();
    assert_eq!(rig.driver_sent(held.clone().into()), Pass::Send);
    rig.sent();

    rig.turn(Club::SandWedge, Mode::Chipping, "flighthook");
    assert!(selects(&mut rig).is_empty(), "the turn matches");
    rig.turn(Club::SandWedge, Mode::Full, "flighthook");
    let [again] = &selects(&mut rig)[..] else {
        panic!("one select again");
    };
    assert_eq!(
        (again.club, again.handedness, again.mode),
        (held.club, held.handedness, held.mode)
    );
    rig.turn(Club::Driver, Mode::Full, "pureplay_accessory.0");
    assert!(selects(&mut rig).is_empty(), "a game card changed it");

    rig.end();
    rig.turn(Club::Driver, Mode::Full, "flighthook");
    assert!(
        selects(&mut rig).is_empty(),
        "nothing held after the session"
    );
}

// -- On the driver host ----------------------------------------------------------

impl SoftwareConfig for Config {
    fn info(&self) -> DriverInfo {
        <Connect as FrpDriver>::info(self)
    }

    fn key(&self) -> String {
        self.address.clone().unwrap_or_default()
    }
}

/// The card on the driver host, as `start_actor` runs it, with a scripted
/// driver: the session is handed a launch monitor's `bus.actor`, and the
/// BallData it writes with a shot's key lands in the app's send registry.
#[test]
fn the_started_actor_records_its_shots_in_the_apps_registry() {
    let address = "127.0.0.1:59760";
    let key = key_of("square.0", 1);
    let script = Script {
        steps: VecDeque::from([vec![DeviceEvent::Frp(FrpBody::ShotRaw(RawFrame {
            key: Some(key.clone()),
            ..RawFrame::new(RawDir::Tx, 0, vec![0x50, 0x4C])
        }))]]),
        ..Script::default()
    };
    let seen = Arc::clone(&script.seen);
    SOFTWARE.put(address.into(), script);
    let rig = Rig::any();
    let mut bus = rig.tx.subscribe();
    crate::actors::start_actor_as(
        "pureplay.0".into(),
        Some("pureplay.0".into()),
        Box::new(DriverActor::<Software<Config>, PureplayHooks>::new(
            PureplaySection {
                driver: Config {
                    address: Some(address.into()),
                },
                ..PureplaySection::default()
            },
        )),
        &rig.state,
        &rig.tx,
    );
    // The card asks the actors up for their state; the Omni answers.
    while !matches!(
        recv_within(&mut bus).expect("the card's bus.sync"),
        BusMsg::Frp { msg, .. } if msg.from == "pureplay.0" && matches!(msg.body, FrpBody::BusSync(_))
    ) {}
    let monitor = BusSender::new("square.0".into(), rig.tx.clone(), Arc::default())
        .with_frp_id(Some("square.0".into()));
    monitor.frp(hosted("square.0"));
    let pureplay = SimAddr::of(None, address.parse().expect("addr"));
    // The card's `shots_sent` row follows the record of the shot.
    while !matches!(
        recv_within(&mut bus).expect("the card's shots_sent row"),
        BusMsg::Local { card, event: LocalEvent::CardStatus { telemetry, .. } }
            if card == "pureplay.0" && telemetry.get("shots_sent").is_some_and(|n| n == "1")
    ) {}
    seen.wait_until(|seen| {
        seen.iter()
            .any(|s| matches!(s, Seen::Bus(m) if m.from == "square.0"))
    });
    assert_eq!(
        rig.state.sent_shots.take_echo(pureplay, Instant::now()),
        Some(key)
    );
    assert!(!matches!(
        rig.state.stop_and_wait("pureplay.0"),
        crate::state::Stopped::Lingering
    ));
}

mod openconnect;
