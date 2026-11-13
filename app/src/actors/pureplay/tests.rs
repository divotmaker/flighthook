use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use tokio::sync::broadcast;

use super::codec::{BallData, ClubData, Frame, kind};
use super::*;
use crate::actors::provenance::ShotSource;
use crate::bus::{BusMsg, LocalEvent};
use flighthook::Mode;
use flighthook::{Launch, ShotKey, Velocity};

/// Longest any test waits for one thing before failing.
const WAIT: Duration = Duration::from_secs(5);

fn key(n: u32) -> ShotKey {
    ShotKey {
        shot_id: format!("shot-{n}"),
        shot_number: n,
    }
}

/// Shot `n` of launch monitor `actor`: shot ids are unique across
/// actors, as every launch monitor's are.
fn key_of(actor: &str, n: u32) -> ShotKey {
    ShotKey {
        shot_id: format!("{actor}:shot-{n}"),
        shot_number: n,
    }
}

/// Put `shot`'s lifecycle on the bus from `monitor`: trigger, launch,
/// flight result, club, impact, finished.
fn emit(monitor: &BusSender, n: u32, shot: &OutputShot) {
    monitor.frp(flightrelay::ShotTrigger::new(key_of(monitor.actor_id(), n)));
    if let Some(launch) = &shot.launch {
        monitor.frp(flightrelay::ShotLaunch {
            key: key_of(monitor.actor_id(), n),
            launch: launch.clone(),
        });
    }
    if let Some(flight) = &shot.flight {
        monitor.frp(flightrelay::ShotFlight {
            key: key_of(monitor.actor_id(), n),
            flight: flight.clone(),
        });
    }
    if let Some(club) = &shot.club {
        monitor.frp(flightrelay::ShotClub {
            key: key_of(monitor.actor_id(), n),
            club: club.clone(),
        });
    }
    if let Some(impact) = &shot.impact {
        monitor.frp(flightrelay::ShotImpact {
            key: key_of(monitor.actor_id(), n),
            impact: impact.clone(),
        });
    }
    monitor.frp(flightrelay::ShotFinished {
        key: key_of(monitor.actor_id(), n),
    });
}

/// The monitor's card status, as a local event of its card.
fn status(monitor: &BusSender, status: ActorStatus) {
    monitor.local(LocalEvent::CardStatus {
        status,
        telemetry: HashMap::new(),
    });
}

/// The monitor's `bus.actor`: `ready` (no blockers when "true") and
/// `battery_pct`.
fn device_telemetry(monitor: &BusSender, pairs: &[(&str, &str)]) {
    let get = |k: &str| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
    let mut actor = hosted_actor(monitor.actor_id());
    actor.blockers.clear();
    if get("ready") != Some("true") {
        actor.blockers.push("not_ready".into());
    }
    if let Some(device) = actor.device.as_mut() {
        device.battery_pct = get("battery_pct").and_then(|p| p.parse().ok());
    }
    monitor.frp(actor);
}

/// The `bus.actor` of the launch monitor `actor` as its host publishes it
/// when it starts: role `launch_monitor`, the source its type declares
/// ([`sources`]), `not_armed` until it connects. Auto-chip has no blockers.
fn hosted_actor(actor: &str) -> flightrelay::BusActor {
    let source = sources().source(actor);
    let mut bus_actor = flightrelay::BusActor::new(actor, 1.0)
        .with_role(flightrelay::types::roles::LAUNCH_MONITOR)
        .with_device(flightrelay::DeviceInfo {
            source: source.map(ShotSource::provenance),
            ..flightrelay::DeviceInfo::default()
        });
    if source != Some(ShotSource::AutoChip) {
        bus_actor.blockers.push("not_armed".into());
    }
    bus_actor
}

/// The launch monitor `monitor` starts: it publishes its `bus.actor`
/// ([`hosted_actor`]), as a hosted actor does before its first shot.
fn announce(monitor: &BusSender) {
    monitor.frp(hosted_actor(monitor.actor_id()));
}

/// A launch-only shot whose spin the launch monitor assumed: the spin
/// rides in its flight result's `assumed`, not in the measured launch.
fn estimated_spin_shot() -> OutputShot {
    OutputShot {
        launch: Some(Launch {
            launch_speed: Some(Velocity::MilesPerHour(20.0)),
            launch_elevation: Some(30.0),
            launch_azimuth: Some(0.5),
            ..Launch::default()
        }),
        flight: Some(flightrelay::FlightResult {
            assumed: Some(flightrelay::AssumedLaunch {
                backspin_rpm: Some(2000),
                sidespin_rpm: Some(0),
                ..flightrelay::AssumedLaunch::default()
            }),
            ..flightrelay::FlightResult::new(flightrelay::Basis::Modeled)
        }),
        ..OutputShot::default()
    }
}

/// A round policy that knows the shot sources these tests use, as the
/// registry records them when the actors start.
fn sources() -> RoundPolicy {
    let round = RoundPolicy::default();
    for (id, source) in [
        ("mevo.0", ShotSource::Mevo),
        ("r10.0", ShotSource::R10),
        ("square.0", ShotSource::Square),
        ("exputt.0", ShotSource::ExPutt),
        ("mock_monitor.0", ShotSource::MockMonitor),
        ("mock_monitor.1", ShotSource::MockMonitor),
        ("openconnect_server.0", ShotSource::OpenConnectServer),
        (flighthook::AUTO_CHIP_ACTOR, ShotSource::AutoChip),
        ("pureplay_accessory.0", ShotSource::PureplayAccessory),
    ] {
        round.register_source(id, source);
    }
    round
}

/// A bus sender for the card `actor`, its FRP actor id the card key. What
/// it is (its source) is what its `bus.actor` says.
fn source_sender(actor: &str, tx: &broadcast::Sender<BusMsg>) -> BusSender {
    BusSender::new(actor.into(), tx.clone(), Arc::default()).with_frp_id(Some(actor.into()))
}

/// A relay with `routing` that knows the usual sources.
fn new_relay(routing: GsProRouting) -> Relay {
    Relay::new(routing, sources(), Some(test_state()))
}

/// An app state whose config holds the launch monitors these tests use,
/// so their card statuses reach the relay.
fn test_state() -> Arc<SystemState> {
    let dir = std::env::temp_dir().join(format!(
        "flighthook-pureplay-{}",
        crate::state::config::generate_id()
    ));
    let (state, _writer) = SystemState::new(dir.join("config.toml"));
    state.system.update(|c| {
        c.webserver.clear();
        c.frp.actor_id = Some("flighthook".into());
        c.frp.auto_chip_actor_id = Some(flighthook::AUTO_CHIP_ACTOR.into());
        c.square.insert(
            "0".into(),
            flighthook::SquareSection {
                actor_id: Some("square.0".into()),
                ..flighthook::SquareSection::default()
            },
        );
        c.mevo.insert(
            "0".into(),
            flighthook::MevoSection {
                actor_id: Some("mevo.0".into()),
                ..flighthook::MevoSection::default()
            },
        );
        for i in ["0", "1"] {
            c.mock_monitor.insert(
                i.into(),
                flighthook::MockMonitorSection {
                    actor_id: Some(format!("mock_monitor.{i}")),
                    ..flighthook::MockMonitorSection::default()
                },
            );
        }
    });
    for id in [
        "mevo.0",
        "r10.0",
        "square.0",
        "exputt.0",
        "mock_monitor.0",
        "mock_monitor.1",
        "openconnect_server.0",
        flighthook::AUTO_CHIP_ACTOR,
        "pureplay_accessory.0",
    ] {
        if let Some(source) = sources().source(id) {
            state.round.register_source(id, source);
        }
    }
    Arc::new(state)
}

/// Why `relay` would be refused a round, `None` when it would be accepted.
fn refusal(relay: &Relay, names: &HashMap<String, String>) -> Option<String> {
    let request = RoundRequest {
        holder: "Pureplay",
        feeds: &relay.feeds(names),
    };
    relay
        .round
        .begin_round(&request)
        .err()
        .map(|r| r.reason().to_owned())
}

/// What the `pureplay.0` card put on the bus, as these tests read it.
#[derive(Debug)]
enum Ev {
    Status {
        status: ActorStatus,
        telemetry: HashMap<String, String>,
    },
    Alert {
        message: String,
    },
    Select(flightrelay::GolferSelect),
    Other,
}

/// `msg` as an [`Ev`], `None` when `pureplay.0` did not send it.
fn ev(msg: BusMsg) -> Option<Ev> {
    match msg {
        BusMsg::Local { card, event } if card == "pureplay.0" => Some(match event {
            LocalEvent::CardStatus { status, telemetry } => Ev::Status { status, telemetry },
            LocalEvent::Alert { message, .. } => Ev::Alert { message },
            _ => Ev::Other,
        }),
        BusMsg::Frp { msg, .. } if msg.from == "pureplay.0" => Some(match msg.body {
            FrpBody::BusAlert(alert) => Ev::Alert {
                message: alert.message,
            },
            FrpBody::GolferSelect(select) => Ev::Select(select),
            _ => Ev::Other,
        }),
        _ => None,
    }
}

// -- Relay (pure) ------------------------------------------------------------

fn relay_bus() -> (BusSender, BusReceiver) {
    let (tx, _) = broadcast::channel(256);
    let monitor = source_sender("square.0", &tx);
    let receiver = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    (monitor, receiver)
}

fn names(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn ready_state_follows_the_monitor_through_a_shot() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = new_relay(GsProRouting::default());
    let now = Instant::now();
    assert_eq!(relay.ready_state(now, None), ReadyState::NotReady);
    assert_eq!(relay.connection_state(None), ConnectionState::Disconnected);

    status(&monitor, ActorStatus::Starting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(None), ConnectionState::Connecting);

    status(&monitor, ActorStatus::Connected);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(None), ConnectionState::Connected);
    assert_eq!(
        relay.ready_state(now, None),
        ReadyState::NotReady,
        "not armed yet"
    );

    device_telemetry(&monitor, &[("ready", "true")]);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now(), None), ReadyState::Ready);

    monitor.frp(flightrelay::ShotTrigger::new(key(1)));
    relay.drain(&mut rx).expect("bus");
    let triggered = Instant::now();
    assert_eq!(relay.ready_state(triggered, None), ReadyState::Processing);
    // A shot that never finishes stops counting as processing.
    assert_eq!(
        relay.ready_state(triggered + PROCESSING_LIMIT, None),
        ReadyState::Ready
    );

    monitor.frp(flightrelay::ShotFinished { key: key(1) });
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now(), None), ReadyState::Ready);

    status(&monitor, ActorStatus::Reconnecting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(
        relay.ready_state(Instant::now(), None),
        ReadyState::NotReady
    );
    assert_eq!(relay.connection_state(None), ConnectionState::Reconnecting);
}

/// A monitor that reports `ready=true` before this actor saw any status
/// from it (the output was added while the monitor was connected) is
/// Connected and Ready; its next drop still ends both.
#[test]
fn ready_before_any_status_counts_as_ready_and_connected() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = new_relay(GsProRouting::default());
    device_telemetry(&monitor, &[("ready", "true"), ("battery_pct", "70")]);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now(), None), ReadyState::Ready);
    assert_eq!(relay.connection_state(None), ConnectionState::Connected);
    assert_eq!(relay.battery(&names(&[("square.0", "Omni")])), Some(70));

    status(&monitor, ActorStatus::Reconnecting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(
        relay.ready_state(Instant::now(), None),
        ReadyState::NotReady
    );
    assert_eq!(relay.connection_state(None), ConnectionState::Reconnecting);

    // `ready=false` with no status seen says nothing about the connection.
    let (monitor, mut rx) = relay_bus();
    let mut relay = new_relay(GsProRouting::default());
    device_telemetry(&monitor, &[("ready", "false")]);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(None), ConnectionState::Disconnected);
}

/// An `error` a monitor reported holds until a status that ends its
/// session without the key: Starting, or a drop from Connected.
#[test]
fn a_monitor_error_ends_with_the_session_that_had_it() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = new_relay(GsProRouting::default());
    let error_status = |status| {
        monitor.local(LocalEvent::CardStatus {
            status,
            telemetry: HashMap::from([("error".into(), "no adapter".into())]),
        });
    };
    error_status(ActorStatus::Reconnecting);
    // A repeat without the key changes nothing.
    status(&monitor, ActorStatus::Reconnecting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(None), ConnectionState::Error);

    status(&monitor, ActorStatus::Connected);
    status(&monitor, ActorStatus::Reconnecting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(None), ConnectionState::Reconnecting);

    error_status(ActorStatus::Reconnecting);
    status(&monitor, ActorStatus::Starting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(None), ConnectionState::Connecting);
}

#[test]
fn host_errors_are_alerted_once_per_burst() {
    let mut relay = new_relay(GsProRouting::default());
    let start = Instant::now();
    assert!(relay.host_error(start));
    assert!(!relay.host_error(start + Duration::from_secs(3)));
    assert!(!relay.host_error(start + Duration::from_secs(30)));
    assert!(relay.host_error(start + Duration::from_secs(61)));
}

/// A section that still matches is Applied; a new address, a routing
/// change or a removed section restarts the actor.
#[test]
fn reconfigure_restarts_on_an_address_or_routing_change() {
    let dir = std::env::temp_dir().join(format!(
        "flighthook-pureplay-reconfigure-{}",
        crate::state::config::generate_id()
    ));
    let (state, _writer) = SystemState::new(dir.join("config.toml"));
    let state = Arc::new(state);
    let set = |section: Option<flighthook::PureplaySection>| {
        state.system.update(|c| match section {
            Some(s) => {
                c.pureplay.insert("0".into(), s);
            }
            None => {
                c.pureplay.remove("0");
            }
        });
    };
    let section = flighthook::PureplaySection {
        name: "Pureplay".into(),
        enabled: true,
        address: None,
        full_monitor: Some("mevo.0".into()),
        chipping_monitor: None,
        putting_monitor: None,
        actor_id: None,
    };
    let (tx, _rx) = broadcast::channel(16);
    let sender = BusSender::new("pureplay.0".into(), tx, Arc::new(AtomicBool::new(false)));
    let actor = PureplayActor {
        addr: DEFAULT_ADDRESS.parse().expect("addr"),
        routing: GsProRouting {
            full_monitor: Some("mevo.0".into()),
            ..GsProRouting::default()
        },
    };
    let outcome = || actor.reconfigure(&state, &sender);
    let restarts = || matches!(outcome(), ReconfigureOutcome::RestartRequired);

    set(Some(section.clone()));
    assert!(matches!(outcome(), ReconfigureOutcome::Applied));
    set(Some(flighthook::PureplaySection {
        address: Some(DEFAULT_ADDRESS.into()),
        name: "Renamed".into(),
        ..section.clone()
    }));
    assert!(
        matches!(outcome(), ReconfigureOutcome::Applied),
        "default spelled out"
    );
    set(Some(flighthook::PureplaySection {
        address: Some("192.168.1.20:9760".into()),
        ..section.clone()
    }));
    assert!(restarts(), "address");
    set(Some(flighthook::PureplaySection {
        full_monitor: None,
        ..section.clone()
    }));
    assert!(restarts(), "full_monitor");
    set(Some(flighthook::PureplaySection {
        chipping_monitor: Some("mevo.0".into()),
        ..section.clone()
    }));
    assert!(restarts(), "chipping_monitor");
    set(Some(flighthook::PureplaySection {
        putting_monitor: Some("exputt.0".into()),
        ..section.clone()
    }));
    assert!(restarts(), "putting_monitor");
    set(None);
    assert!(restarts(), "removed");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_monitor_error_reads_as_the_error_state_until_it_connects() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = new_relay(GsProRouting::default());
    monitor.local(LocalEvent::CardStatus {
        status: ActorStatus::Reconnecting,
        telemetry: HashMap::from([("error".into(), "no adapter".into())]),
    });
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(None), ConnectionState::Error);
    status(&monitor, ActorStatus::Connected);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(None), ConnectionState::Connected);
}

/// Routed to the Omni, the device's state is the Omni's alone; outputs
/// and other monitors do not count.
#[test]
fn the_routed_monitor_alone_sets_the_state() {
    let (tx, _) = broadcast::channel(256);
    let omni = source_sender("square.0", &tx);
    let mevo = source_sender("mevo.0", &tx);
    let gspro = source_sender("openconnect_client.0", &tx);
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = new_relay(GsProRouting {
        full_monitor: Some("square.0".into()),
        ..GsProRouting::default()
    });
    status(&mevo, ActorStatus::Connected);
    device_telemetry(&mevo, &[("ready", "true"), ("battery_pct", "50")]);
    status(&gspro, ActorStatus::Connected);
    status(&omni, ActorStatus::Starting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(
        relay.ready_state(Instant::now(), None),
        ReadyState::NotReady
    );
    assert_eq!(relay.connection_state(None), ConnectionState::Connecting);
    let n = names(&[("square.0", "Omni"), ("mevo.0", "Mevo")]);
    assert_eq!(relay.battery(&n), None);
    assert!(!relay.monitors.contains_key("openconnect_client.0"));

    status(&omni, ActorStatus::Connected);
    device_telemetry(&omni, &[("ready", "true"), ("battery_pct", "64")]);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now(), None), ReadyState::Ready);
    assert_eq!(relay.battery(&n), Some(64));
}

/// With Any routing, the best state of every monitor counts, and the
/// battery and name are those of the only configured monitor.
#[test]
fn any_routing_takes_the_best_monitor_and_names_a_lone_one() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = new_relay(GsProRouting::default());
    status(&monitor, ActorStatus::Connected);
    device_telemetry(&monitor, &[("ready", "true"), ("battery_pct", "90")]);
    relay.drain(&mut rx).expect("bus");
    let one = names(&[("square.0", "Square Golf Omni")]);
    let Message::Hello {
        device_caps,
        device_name,
        firmware,
        protocol_version,
    } = relay.hello(&one)
    else {
        panic!("hello");
    };
    assert_eq!(protocol_version, 1);
    assert_eq!(device_caps, codec::CAP_BATTERY_POWERED);
    assert_eq!(device_name, "flighthook (Square Golf Omni)");
    assert_eq!(firmware, env!("CARGO_PKG_VERSION"));
    assert_eq!(relay.battery(&one), Some(90));

    // Two monitors configured: neither stands for the device.
    let two = names(&[("square.0", "Square Golf Omni"), ("mevo.0", "Mevo")]);
    let Message::Hello {
        device_caps,
        device_name,
        ..
    } = relay.hello(&two)
    else {
        panic!("hello");
    };
    assert_eq!(device_caps, 0);
    assert_eq!(device_name, "flighthook");
    assert_eq!(relay.battery(&two), None);
    assert_eq!(relay.ready_state(Instant::now(), None), ReadyState::Ready);
}

/// A monitor that never reported a battery gives no battery capability.
#[test]
fn no_battery_report_means_no_battery_capability() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = new_relay(GsProRouting::default());
    status(&monitor, ActorStatus::Connected);
    relay.drain(&mut rx).expect("bus");
    let Message::Hello { device_caps, .. } = relay.hello(&names(&[("square.0", "Omni")])) else {
        panic!("hello");
    };
    assert_eq!(device_caps, 0);
}

#[test]
fn shots_routed_elsewhere_or_finished_without_a_link_are_dropped() {
    let (tx, _) = broadcast::channel(256);
    let omni = source_sender("square.0", &tx);
    let mevo = source_sender("mevo.0", &tx);
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = new_relay(GsProRouting {
        full_monitor: Some("square.0".into()),
        ..GsProRouting::default()
    });
    emit(&omni, 1, &mapper::tests::full_shot());
    relay.drain(&mut rx).expect("bus");
    assert!(relay.next_shot(Instant::now()).is_none(), "no link");

    relay.connect(Instant::now());
    emit(&mevo, 2, &mapper::tests::full_shot());
    emit(&omni, 3, &mapper::tests::full_shot());
    relay.drain(&mut rx).expect("bus");
    let shot = relay.next_shot(Instant::now()).expect("the Omni's shot");
    assert_eq!((shot.actor.as_str(), shot.shot_number), ("square.0", 3));
    assert!(relay.next_shot(Instant::now()).is_none());

    emit(&omni, 4, &mapper::tests::full_shot());
    relay.drain(&mut rx).expect("bus");
    assert!(
        relay
            .next_shot(Instant::now() + STALE_SHOT + Duration::from_millis(1))
            .is_none(),
        "stale"
    );
}

/// A missed read (a shot that finished with no data) is never queued.
#[test]
fn a_no_read_is_not_sent() {
    let (tx, _) = broadcast::channel(256);
    let omni = source_sender("square.0", &tx);
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = new_relay(GsProRouting::default());
    relay.connect(Instant::now());
    emit(&omni, 1, &OutputShot::default());
    emit(&omni, 2, &mapper::tests::full_shot());
    relay.drain(&mut rx).expect("bus");
    let shot = relay.next_shot(Instant::now()).expect("the read shot");
    assert_eq!(shot.shot_number, 2);
    assert!(relay.next_shot(Instant::now()).is_none());
}

/// Strict mode is accepted only when every launch monitor routed to the
/// output, in any detection mode, is measured; the reason names the first
/// one that is not, and its class.
#[test]
fn strict_mode_needs_every_routed_monitor_measured() {
    let any = new_relay(GsProRouting::default());
    assert_eq!(refusal(&any, &names(&[])), None, "no monitor at all");
    let measured = names(&[
        ("square.0", "Omni"),
        ("mevo.0", "Mevo+"),
        ("r10.0", "R10"),
        ("exputt.0", "ExPutt"),
    ]);
    assert_eq!(refusal(&any, &measured), None);

    let with_mock = names(&[("square.0", "Omni"), ("mock_monitor.0", "Mock")]);
    assert_eq!(
        refusal(&any, &with_mock).as_deref(),
        Some("Mock is a virtual launch monitor"),
        "Any routing takes the mock's shots"
    );
    let omni_only = new_relay(GsProRouting {
        full_monitor: Some("square.0".into()),
        chipping_monitor: Some("square.0".into()),
        putting_monitor: Some("square.0".into()),
    });
    assert_eq!(
        refusal(&omni_only, &with_mock),
        None,
        "a configured mock routed nowhere does not count"
    );

    let with_input = new_relay(GsProRouting {
        full_monitor: Some("square.0".into()),
        chipping_monitor: Some("mock_monitor.0".into()),
        putting_monitor: Some("openconnect_server.0".into()),
    });
    let input_and_mock = names(&[
        ("square.0", "Omni"),
        ("mock_monitor.0", "Mock"),
        ("openconnect_server.0", "Open Connect input"),
    ]);
    assert_eq!(
        refusal(&with_input, &input_and_mock).as_deref(),
        Some("Mock is a virtual launch monitor"),
        "routed in one mode is enough"
    );
    let with_input = new_relay(GsProRouting {
        putting_monitor: Some("openconnect_server.0".into()),
        ..GsProRouting::default()
    });
    assert_eq!(
        refusal(
            &with_input,
            &names(&[
                ("square.0", "Omni"),
                ("openconnect_server.0", "Open Connect input"),
            ])
        ),
        None,
        "the Open Connect input is measured"
    );

    // A launch monitor seen on the bus counts for Any too, named by its ID
    // when it has no name.
    let (tx, _) = broadcast::channel(16);
    let mock = BusSender::new("mock_monitor.1".into(), tx.clone(), Arc::default());
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut seen = new_relay(GsProRouting::default());
    status(&mock, ActorStatus::Connected);
    seen.drain(&mut rx).expect("bus");
    assert_eq!(
        refusal(&seen, &names(&[("square.0", "Omni")])).as_deref(),
        Some("mock_monitor.1 is a virtual launch monitor")
    );
    assert_eq!(
        refusal(&any, &names(&[("mock_monitor.0", " ")])).as_deref(),
        Some("mock_monitor.0 is a virtual launch monitor")
    );
}

/// During a round the ready state and the connection state count only the
/// launch monitors the policy counts: a virtual monitor that is ready
/// under "Any" makes the device Ready outside a round only.
#[test]
fn readiness_counts_only_measured_monitors_during_a_round() {
    let (tx, _) = broadcast::channel(64);
    let round = sources();
    let omni = source_sender("square.0", &tx);
    let mock = source_sender("mock_monitor.0", &tx);
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = Relay::new(GsProRouting::default(), round.clone(), Some(test_state()));
    status(&mock, ActorStatus::Connected);
    device_telemetry(&mock, &[("ready", "true")]);
    relay.drain(&mut rx).expect("bus");
    let guard = round
        .begin_round(&RoundRequest {
            holder: "Pureplay",
            feeds: &[],
        })
        .expect("a round");
    let now = Instant::now();
    assert_eq!(relay.ready_state(now, None), ReadyState::Ready);
    assert_eq!(relay.ready_state(now, Some(&guard)), ReadyState::NotReady);
    assert_eq!(
        relay.connection_state(Some(&guard)),
        ConnectionState::Disconnected,
        "no measured monitor seen"
    );

    status(&omni, ActorStatus::Connected);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(
        relay.ready_state(Instant::now(), Some(&guard)),
        ReadyState::NotReady,
        "the Omni is not ready"
    );
    assert_eq!(
        relay.connection_state(Some(&guard)),
        ConnectionState::Connected
    );
    device_telemetry(&omni, &[("ready", "true")]);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(
        relay.ready_state(Instant::now(), Some(&guard)),
        ReadyState::Ready
    );
}

/// A launch monitor seen on the bus and then removed from the config (so
/// not a known source) does not feed the output, and does not refuse a
/// round.
#[test]
fn a_removed_monitor_does_not_refuse_a_round() {
    let (tx, _) = broadcast::channel(16);
    let round = sources();
    let mock = source_sender("mock_monitor.1", &tx);
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = Relay::new(GsProRouting::default(), round.clone(), Some(test_state()));
    status(&mock, ActorStatus::Connected);
    relay.drain(&mut rx).expect("bus");
    let configured = names(&[("square.0", "Omni")]);
    assert_eq!(
        refusal(&relay, &configured).as_deref(),
        Some("mock_monitor.1 is a virtual launch monitor")
    );
    round.forget_source("mock_monitor.1");
    status(&mock, ActorStatus::Disconnected);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(refusal(&relay, &configured), None);
    assert!(!relay.monitors.contains_key("mock_monitor.1"));
}

/// Routed to "Any", a virtual card refuses strict mode until it is turned
/// off: its `bus.actor` removal and the round forgetting its card (by its
/// actor ID) leave the Mevo alone, and the round starts.
#[test]
fn a_virtual_card_turned_off_lets_a_strict_round_start() {
    const MEVO: &str = "7d2f9a41c03be856";
    const MOCK: &str = "c19e04b7a2d5f318";
    let (tx, _) = broadcast::channel(64);
    let round = RoundPolicy::default();
    round.register_card_source("mevo.0", MEVO, ShotSource::Mevo);
    round.register_card_source("mock_monitor.0", MOCK, ShotSource::MockMonitor);
    let mevo = source_sender(MEVO, &tx);
    let mock = source_sender(MOCK, &tx);
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = Relay::new(GsProRouting::default(), round.clone(), Some(test_state()));
    announce(&mevo);
    announce(&mock);
    relay.drain(&mut rx).expect("bus");
    let configured = names(&[(MEVO, "Mevo"), (MOCK, "Mock")]);
    assert_eq!(
        refusal(&relay, &configured).as_deref(),
        Some("Mock is a virtual launch monitor")
    );

    round.forget_card("mock_monitor.0");
    mock.frp(flightrelay::Removal::of(flightrelay::StateKind::BusActor));
    relay.drain(&mut rx).expect("bus");
    let configured = names(&[(MEVO, "Mevo")]);
    assert_eq!(refusal(&relay, &configured), None);
    assert!(!relay.monitors.contains_key(MOCK));
}

/// A launch monitor turned off is no feed: it is left out of the
/// configured names and forgotten as a source when the system stops it, and
/// a mode routed to it by ID counts for nothing. So a mock that is off does
/// not refuse a round.
#[test]
fn a_monitor_turned_off_does_not_refuse_a_round() {
    let dir = crate::actors::web::tests::TempDir(std::env::temp_dir().join(format!(
        "flighthook-pureplay-disabled-{}",
        crate::state::config::generate_id()
    )));
    let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
    state.system.update(|c| {
        c.webserver.clear();
        c.square.insert(
            "0".into(),
            flighthook::SquareSection {
                actor_id: Some("square.0".into()),
                ..flighthook::SquareSection::default()
            },
        );
        c.mock_monitor.insert(
            "0".into(),
            flighthook::MockMonitorSection {
                name: "Mock".into(),
                enabled: false,
                impact_lead_s: None,
                driver: Default::default(),
                actor_id: Some("mock_monitor.0".into()),
            },
        );
    });
    // Still registered (its stop not finished yet): left out all the same.
    state.round.register_source("square.0", ShotSource::Square);
    state
        .round
        .register_source("mock_monitor.0", ShotSource::MockMonitor);
    let names = monitor_names(&state);
    assert_eq!(names.keys().collect::<Vec<_>>(), ["square.0"]);

    let round = sources();
    round.forget_source("mock_monitor.0");
    let any = Relay::new(GsProRouting::default(), round.clone(), Some(test_state()));
    assert_eq!(refusal(&any, &names), None);
    let to_the_mock = Relay::new(
        GsProRouting {
            putting_monitor: Some("mock_monitor.0".into()),
            ..GsProRouting::default()
        },
        round,
        Some(test_state()),
    );
    assert_eq!(refusal(&to_the_mock, &names), None, "routed by ID");
}

// -- Against a mock Pureplay host -------------------------------------------

/// A running `pureplay.0` wired to a mock host it dials.
struct Bridge {
    host: TcpListener,
    tx: broadcast::Sender<BusMsg>,
    round: RoundPolicy,
    monitor: BusSender,
    chip: BusSender,
    mock: BusSender,
    events: broadcast::Receiver<BusMsg>,
    shutdown: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
    /// The send registry the bridge records its shots in.
    sent: Arc<SentShots>,
}

impl Bridge {
    /// Start the bridge with the Omni (`square.0`, "Square Golf Omni") as
    /// the only launch monitor. `before` puts messages on the bus ahead of
    /// the bridge's first look at it.
    fn start(before: impl FnOnce(&BusSender)) -> Self {
        Self::start_with(GsProRouting::default(), Mode::Full, before)
    }

    /// [`start`](Self::start) with `routing`, the app in detection mode
    /// `mode` when the bridge starts.
    fn start_with(routing: GsProRouting, mode: Mode, before: impl FnOnce(&BusSender)) -> Self {
        Self::start_configured(routing, mode, &[("square.0", "Square Golf Omni")], before)
    }

    /// [`start_with`](Self::start_with) with `monitors` (actor ID, name)
    /// as the configured launch monitors.
    fn start_configured(
        routing: GsProRouting,
        mode: Mode,
        monitors: &[(&str, &str)],
        before: impl FnOnce(&BusSender),
    ) -> Self {
        let configured = names(monitors);
        let round = sources();
        let host = TcpListener::bind("127.0.0.1:0").expect("bind mock host");
        host.set_nonblocking(true).expect("nonblocking");
        let addr = host.local_addr().expect("addr");
        let (tx, events) = broadcast::channel(1024);
        let shutdown = Arc::new(AtomicBool::new(false));
        let sender = BusSender::new("pureplay.0".into(), tx.clone(), Arc::clone(&shutdown))
            .with_frp_id(Some("pureplay.0".into()));
        let receiver = sender.subscribe();
        let monitor = source_sender("square.0", &tx);
        let chip = source_sender(flighthook::AUTO_CHIP_ACTOR, &tx);
        let mock = source_sender("mock_monitor.0", &tx);
        host_the_omni(&monitor, Arc::clone(&shutdown));
        before(&monitor);
        let mut synced = tx.subscribe();
        let (done_tx, done) = mpsc::channel();
        let shared_round = round.clone();
        let sent = Arc::new(SentShots::default());
        let shared_sent = Arc::clone(&sent);
        std::thread::spawn(move || {
            let names = move || configured.clone();
            let holder = || "Pureplay".to_owned();
            run_with(
                addr,
                routing,
                GolferTurn::new(None, None, flighthook::Handedness::Right, mode),
                Backoff::new(Duration::from_millis(20), Duration::from_millis(20)),
                Shared {
                    names: &names,
                    holder: &holder,
                    round: shared_round,
                    sent: Some(shared_sent),
                    state: Some(test_state()),
                },
                &sender,
                receiver,
            );
            let _ = done_tx.send(());
        });
        // The bridge's `bus.sync` and the Omni's answer have gone out
        // before the test goes on.
        let deadline = Instant::now() + WAIT;
        let mut asked = false;
        loop {
            match synced.try_recv() {
                Ok(BusMsg::Frp { msg, .. }) => match msg.body {
                    FrpBody::BusSync(_) if msg.from == "pureplay.0" => asked = true,
                    FrpBody::BusActor(_) if asked && msg.from == "square.0" => break,
                    _ => {}
                },
                Ok(_) => {}
                Err(_) => {
                    assert!(
                        Instant::now() < deadline,
                        "the Omni never answered bus.sync"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
        Self {
            host,
            tx,
            round,
            monitor,
            chip,
            mock,
            events,
            shutdown,
            done,
            sent,
        }
    }

    /// A bus sender for the actor `actor`, to put its shots on the bus.
    /// The actor starts now: it publishes its `bus.actor` first.
    fn source(&self, actor: &str) -> BusSender {
        let sender = source_sender(actor, &self.tx);
        announce(&sender);
        sender
    }

    /// The `pureplay.0` alerts on the bus so far.
    fn alerts(&mut self) -> Vec<String> {
        let mut alerts = Vec::new();
        while let Ok(msg) = self.events.try_recv() {
            if let Some(Ev::Alert { message }) = ev(msg) {
                alerts.push(message);
            }
        }
        alerts
    }

    /// Accept the bridge's next connection.
    fn accept(&self) -> Host {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.host.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).expect("blocking");
                    stream
                        .set_read_timeout(Some(Duration::from_millis(20)))
                        .expect("timeout");
                    return Host {
                        stream,
                        reader: FrameReader::new(),
                    };
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "bridge never connected");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept: {e}"),
            }
        }
    }

    /// Accept and complete the handshake: returns the Hello and the host.
    fn connect(&self) -> (Message, Host) {
        let mut host = self.accept();
        let hello = host.next();
        host.send(&Message::HelloAck {
            protocol_version: 1,
            host_caps: 0,
        });
        (hello, host)
    }

    /// Wait for a `pureplay.0` event matching `want`.
    fn wait_for(&mut self, what: &str, want: impl Fn(&Ev) -> bool) -> Ev {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.events.try_recv() {
                Ok(msg) => {
                    if let Some(e) = ev(msg)
                        && want(&e)
                    {
                        return e;
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "timed out waiting for {what}");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("bus: {e}"),
            }
        }
    }

    /// Wait for a status update that sets `key` to a value `want`
    /// accepts; returns the value.
    fn telemetry_where(&mut self, key: &str, want: impl Fn(&str) -> bool) -> String {
        let event = self.wait_for(key, |e| {
            matches!(e, Ev::Status { telemetry, .. }
                if telemetry.get(key).is_some_and(|v| want(v)))
        });
        let Ev::Status { telemetry, .. } = event else {
            unreachable!()
        };
        telemetry[key].clone()
    }

    /// Wait for a status update that sets `key` to `value`.
    fn telemetry_is(&mut self, key: &str, value: &str) {
        self.telemetry_where(key, |v| v == value);
    }

    fn stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.done
            .recv_timeout(Duration::from_secs(2))
            .expect("bridge stops");
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Plays the Omni's host, which was up before the bridge: on every
/// `bus.sync` it republishes the Omni's latest `bus.actor` (its first one,
/// [`hosted_actor`], until the test publishes another), until `shutdown`.
fn host_the_omni(omni: &BusSender, shutdown: Arc<AtomicBool>) {
    let omni = omni.clone();
    let mut rx = omni.raw_sender().subscribe();
    std::thread::spawn(move || {
        let mut latest = hosted_actor(omni.actor_id());
        while !shutdown.load(Ordering::Relaxed) {
            match rx.try_recv() {
                Ok(BusMsg::Frp { msg, .. }) => match msg.body {
                    FrpBody::BusActor(actor) if msg.from == omni.actor_id() => latest = actor,
                    FrpBody::BusSync(_) => omni.frp(latest.clone()),
                    _ => {}
                },
                Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(broadcast::error::TryRecvError::Closed) => return,
            }
        }
    });
}

/// The mock host's end of one link.
struct Host {
    stream: TcpStream,
    reader: FrameReader,
}

impl Host {
    fn send(&mut self, msg: &Message) {
        self.stream.write_all(&msg.to_frame()).expect("host write");
    }

    fn frame(&mut self) -> Frame {
        let deadline = Instant::now() + WAIT;
        let mut buf = [0u8; 4096];
        loop {
            if let Some(f) = self.reader.next_frame() {
                assert!(self.reader.take_dropped().is_empty(), "clean framing");
                assert_eq!(f.version, 1);
                return f;
            }
            assert!(Instant::now() < deadline, "timed out waiting for a frame");
            match self.stream.read(&mut buf) {
                Ok(0) => panic!("bridge closed the link"),
                Ok(n) => self.reader.push(&buf[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => panic!("read: {e}"),
            }
        }
    }

    /// The next message from the bridge.
    fn next(&mut self) -> Message {
        let f = self.frame();
        Message::decode(f.kind, &f.payload).expect("device messages decode")
    }

    /// The next message other than a ReadyState within `wait`, if any.
    fn next_within(&mut self, wait: Duration) -> Option<Message> {
        let until = Instant::now() + wait;
        let mut buf = [0u8; 4096];
        loop {
            while let Some(f) = self.reader.next_frame() {
                let msg = Message::decode(f.kind, &f.payload).expect("device messages decode");
                if !matches!(msg, Message::ReadyState(_)) {
                    return Some(msg);
                }
            }
            if Instant::now() >= until {
                return None;
            }
            match self.stream.read(&mut buf) {
                Ok(0) => panic!("bridge closed the link"),
                Ok(n) => self.reader.push(&buf[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => panic!("read: {e}"),
            }
        }
    }

    /// The next message other than a ReadyState.
    fn next_not_ready(&mut self) -> Message {
        loop {
            match self.next() {
                Message::ReadyState(_) => {}
                other => return other,
            }
        }
    }

    /// Nothing but ReadyState frames arrives for `quiet`.
    fn assert_quiet(&mut self, quiet: Duration) {
        let until = Instant::now() + quiet;
        let mut buf = [0u8; 4096];
        while Instant::now() < until {
            while let Some(f) = self.reader.next_frame() {
                assert_eq!(f.kind, kind::READY_STATE, "unexpected frame {f:?}");
            }
            match self.stream.read(&mut buf) {
                Ok(0) => panic!("bridge closed the link"),
                Ok(n) => self.reader.push(&buf[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => panic!("read: {e}"),
            }
        }
    }

    fn ball(&mut self) -> BallData {
        match self.next_not_ready() {
            Message::BallData(b) => b,
            other => panic!("expected BallData, got {other:?}"),
        }
    }

    fn club(&mut self) -> ClubData {
        match self.next_not_ready() {
            Message::ClubData(c) => c,
            other => panic!("expected ClubData, got {other:?}"),
        }
    }

    /// Ask for strict mode and return its ack.
    fn strict_ack(&mut self, on: bool) -> Message {
        self.send(&Message::SetStrictMode {
            strict: u8::from(on),
        });
        self.next_not_ready()
    }

    /// Ask for strict mode and check it is accepted.
    fn strict(&mut self, on: bool) {
        assert_eq!(
            self.strict_ack(on),
            Message::StrictModeAck {
                accepted: true,
                reason: String::new()
            }
        );
    }
}

/// The handshake, the first ReadyState and a Ping answered with the
/// monitor's state and battery.
#[test]
fn handshake_ready_state_and_heartbeat() {
    let mut bridge = Bridge::start(|omni| {
        status(omni, ActorStatus::Connected);
        device_telemetry(omni, &[("ready", "true"), ("battery_pct", "80")]);
    });
    let (hello, mut host) = bridge.connect();
    assert_eq!(
        hello,
        Message::Hello {
            protocol_version: 1,
            device_caps: codec::CAP_BATTERY_POWERED,
            device_name: "flighthook (Square Golf Omni)".into(),
            firmware: env!("CARGO_PKG_VERSION").into(),
        }
    );
    assert_eq!(host.next(), Message::ReadyState(ReadyState::Ready.byte()));
    bridge.wait_for("Connected", |e| {
        matches!(
            e,
            Ev::Status {
                status: ActorStatus::Connected,
                ..
            }
        )
    });

    host.send(&Message::Ping);
    assert_eq!(
        host.next(),
        Message::Heartbeat {
            connection_state: ConnectionState::Connected.byte(),
            ready_state: ReadyState::Ready.byte(),
            battery: 80,
        }
    );

    // A transition is pushed at once: the monitor drops.
    status(&bridge.monitor, ActorStatus::Reconnecting);
    assert_eq!(
        host.next(),
        Message::ReadyState(ReadyState::NotReady.byte())
    );
    host.send(&Message::Ping);
    assert_eq!(
        host.next(),
        Message::Heartbeat {
            connection_state: ConnectionState::Reconnecting.byte(),
            ready_state: ReadyState::NotReady.byte(),
            battery: codec::BATTERY_UNKNOWN,
        }
    );
    bridge.stop();
}

/// A shot with ball and club data goes out as BallData then ClubData with
/// the same ShotId; a ball-only shot as BallData alone with the next.
#[test]
fn shots_go_out_as_ball_then_club_data() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    assert_eq!(
        host.next(),
        Message::ReadyState(ReadyState::NotReady.byte())
    );
    bridge.telemetry_is(SHOTS_SENT, "0");

    emit(&bridge.monitor, 30, &mapper::tests::full_shot());
    let ball = host.ball();
    assert_eq!(ball.shot_id, 1);
    assert_eq!(ball.valid, 0x7F);
    assert_eq!(ball.ball_speed_ms, 64.0);
    assert_eq!(ball.hla, -1.5);
    let club = host.club();
    assert_eq!(club.shot_id, 1);
    assert_eq!(club.valid, 0xFF);
    assert_eq!(club.impact_point_x_mm, 5.0);

    emit(&bridge.monitor, 31, &mapper::tests::ball_only_shot());
    let ball = host.ball();
    assert_eq!(ball.shot_id, 2);
    assert_eq!(
        ball.valid & BallData::SPIN,
        BallData::BACK_SPIN | BallData::SIDE_SPIN
    );
    // No ClubData follows: the next frame is the answer to a Ping.
    host.send(&Message::Ping);
    assert!(matches!(host.next_not_ready(), Message::Heartbeat { .. }));

    bridge.wait_for("the second shot on the card", |e| {
        matches!(e, Ev::Status { telemetry, .. }
            if telemetry.get(SHOTS_SENT).is_some_and(|v| v == "2")
                && telemetry.get(LAST_SHOT).is_some_and(|v| v.starts_with("#2 at ")))
    });
    bridge.stop();
}

/// SetClubInfo makes the card send a complete golfer.select: the hand,
/// the club and the mode they give with the last distance (none yet: full);
/// an unknown club is ignored and the hand still set.
#[test]
fn set_club_info_sets_the_hand_and_club() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.send(&Message::SetClubInfo {
        handedness: 1,
        club: 23,
    });
    let select = bridge.wait_for("select", |e| matches!(e, Ev::Select(_)));
    let Ev::Select(select) = select else {
        unreachable!()
    };
    assert_eq!(select.handedness, Some(flighthook::Handedness::Left));
    assert_eq!(select.club, Some(Some(flighthook::Club::Iron7)));
    assert_eq!(select.mode, Some(Mode::Full));
    // The card shows the club and hand it holds on its own card.
    let (club, hand) = (
        flighthook::Club::Iron7.to_string(),
        flighthook::Handedness::Left.to_string(),
    );
    bridge.wait_for("club and hand", |e| {
        matches!(e, Ev::Status { telemetry, .. }
            if telemetry.get("club") == Some(&club) && telemetry.get("handed") == Some(&hand))
    });

    host.send(&Message::SetClubInfo {
        handedness: 0,
        club: 200,
    });
    bridge.wait_for("right hand", |e| {
        matches!(e, Ev::Select(s)
            if s.handedness == Some(flighthook::Handedness::Right)
                && s.club == Some(Some(flighthook::Club::Iron7)))
    });
    // The next club comes from the next valid command, not the unknown one.
    host.send(&Message::SetClubInfo {
        handedness: 0,
        club: 0,
    });
    bridge.wait_for(
        "driver",
        |e| matches!(e, Ev::Select(s) if s.club == Some(Some(flighthook::Club::Driver))),
    );
    bridge.stop();
}

/// SetShotContext gives the mode with the club in play: under 30 yd to the
/// target a chip, the putter a putt; the card sends its select again when
/// the mode changes.
#[test]
fn the_shot_context_sets_the_mode_in_the_select() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.send(&Message::SetClubInfo {
        handedness: 0,
        club: 23,
    });
    bridge.wait_for(
        "full",
        |e| matches!(e, Ev::Select(s) if s.mode == Some(Mode::Full)),
    );
    host.send(&Message::SetShotContext {
        distance_to_target_m: 20.0,
    });
    bridge.wait_for("chipping", |e| {
        matches!(e, Ev::Select(s)
            if s.mode == Some(Mode::Chipping)
                && s.club == Some(Some(flighthook::Club::Iron7)))
    });
    host.send(&Message::SetClubInfo {
        handedness: 0,
        club: flighthook_club_code(flighthook::Club::Putter),
    });
    bridge.wait_for(
        "putting",
        |e| matches!(e, Ev::Select(s) if s.mode == Some(Mode::Putting)),
    );
    bridge.stop();
}

/// The Pureplay code of `club`.
fn flighthook_club_code(club: flighthook::Club) -> u8 {
    (0..=30)
        .find(|&c| mapper::club_from_pureplay(c) == Some(club))
        .expect("club has a Pureplay code")
}

/// Detect, ball type and shot context are taken without a reply; an
/// unknown type, and a device-to-host type, get Error code 0.
#[test]
fn advisory_commands_are_quiet_and_unknown_types_get_an_error() {
    let bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.send(&Message::SetShotContext {
        distance_to_target_m: 142.5,
    });
    host.send(&Message::SetDetect { mode: 1 });
    host.send(&Message::SetBallType { ball_type: 1 });
    host.assert_quiet(Duration::from_millis(200));

    host.stream
        .write_all(&codec::encode_frame(0xA5, &[1, 2]).expect("frame"))
        .expect("write");
    let Message::Error { code, message } = host.next_not_ready() else {
        panic!("expected Error");
    };
    assert_eq!(code, 0);
    assert!(message.contains("0xA5"), "{message}");

    host.send(&Message::ReadyState(1));
    let Message::Error { code, .. } = host.next_not_ready() else {
        panic!("expected Error");
    };
    assert_eq!(code, 0);

    // The link carries on.
    host.send(&Message::Ping);
    assert!(matches!(host.next_not_ready(), Message::Heartbeat { .. }));
    bridge.stop();
}

/// Strict mode with only measured monitors routed: accepted; while on, an
/// estimated spin goes out with its spin bits clear and an Auto-chip shot
/// is not sent (and takes no ShotId). Off again, both go out as before.
#[test]
fn strict_mode_clears_estimated_spin_and_suppresses_auto_chip() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    bridge.telemetry_is(STRICT_MODE, "on");

    emit(&bridge.monitor, 1, &estimated_spin_shot());
    let ball = host.ball();
    assert_eq!(ball.shot_id, 1);
    assert_eq!(
        ball.valid,
        BallData::BALL_SPEED | BallData::VLA | BallData::HLA
    );
    assert_eq!(ball.back_spin_rpm, 0.0);

    emit(&bridge.chip, 1, &estimated_spin_shot());
    emit(&bridge.monitor, 2, &mapper::tests::ball_only_shot());
    let ball = host.ball();
    assert_eq!(ball.shot_id, 2, "the chip took no ShotId");
    assert!(
        (ball.ball_speed_ms - 44.704).abs() < 1e-3,
        "the monitor's shot"
    );

    host.strict(false);
    emit(&bridge.chip, 2, &estimated_spin_shot());
    let ball = host.ball();
    assert_eq!(ball.shot_id, 3, "Auto-chip goes out with strict mode off");
    assert_eq!(ball.valid & BallData::BACK_SPIN, BallData::BACK_SPIN);
    assert_eq!(ball.back_spin_rpm, 2000.0);
    bridge.stop();
}

/// Tournament mode alone makes no output measured-only: an estimated spin
/// still goes out with its spin bits. Strict mode on top keeps its own
/// rule (spin bits clear), and its end leaves tournament mode on.
#[test]
fn tournament_mode_is_not_strict_mode() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    bridge.round.set_manual(true);

    emit(&bridge.monitor, 1, &estimated_spin_shot());
    let ball = host.ball();
    assert_eq!(ball.valid & BallData::BACK_SPIN, BallData::BACK_SPIN);
    assert_eq!(ball.back_spin_rpm, 2000.0, "the estimate goes out");

    host.strict(true);
    bridge.telemetry_is(STRICT_MODE, "on");
    emit(&bridge.monitor, 2, &estimated_spin_shot());
    let ball = host.ball();
    assert_eq!(
        ball.valid,
        BallData::BALL_SPEED | BallData::VLA | BallData::HLA,
        "the simulator's measured-only rule"
    );
    host.strict(false);
    assert!(bridge.round.state().manual && bridge.round.active());
    bridge.round.set_manual(false);
    bridge.stop();
}

/// A new link starts with strict mode off and ShotId 1, and repeats the
/// handshake.
#[test]
fn a_reconnect_resets_strict_mode_and_the_shot_id() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    emit(&bridge.monitor, 1, &mapper::tests::ball_only_shot());
    assert_eq!(host.ball().shot_id, 1);
    emit(&bridge.monitor, 2, &mapper::tests::ball_only_shot());
    assert_eq!(host.ball().shot_id, 2);
    drop(host);

    bridge.wait_for("Reconnecting", |e| {
        matches!(
            e,
            Ev::Status {
                status: ActorStatus::Reconnecting,
                ..
            }
        )
    });
    let (hello, mut host) = bridge.connect();
    assert!(matches!(hello, Message::Hello { .. }));
    host.next();
    bridge.wait_for("Connected", |e| {
        matches!(
            e,
            Ev::Status {
                status: ActorStatus::Connected,
                ..
            }
        )
    });
    emit(&bridge.monitor, 3, &estimated_spin_shot());
    let ball = host.ball();
    assert_eq!(ball.shot_id, 1, "ShotId starts again");
    assert_eq!(
        ball.valid & BallData::BACK_SPIN,
        BallData::BACK_SPIN,
        "strict mode is off on the new link"
    );
    bridge.stop();
}

/// No HelloAck: the card says why and the bridge retries; the error clears
/// once a handshake succeeds.
#[test]
fn a_host_that_never_answers_hello_is_an_error_until_one_does() {
    let mut bridge = Bridge::start(|_| {});
    let mut silent = bridge.accept();
    assert!(matches!(silent.next(), Message::Hello { .. }));
    let error = bridge.telemetry_where(ERROR, |v| !v.is_empty());
    assert!(error.contains("no reply to Hello"), "{error}");
    drop(silent);

    let (_, mut host) = bridge.connect();
    host.next();
    bridge.telemetry_is(ERROR, "");
    bridge.stop();
}

/// A HelloAck choosing a version flighthook does not speak ends the link
/// with the reason on the card.
#[test]
fn an_unsupported_version_ends_the_link() {
    let mut bridge = Bridge::start(|_| {});
    let mut host = bridge.accept();
    host.next();
    host.send(&Message::HelloAck {
        protocol_version: 2,
        host_caps: 0,
    });
    let error = bridge.telemetry_where(ERROR, |v| !v.is_empty());
    assert!(error.contains("version 2"), "{error}");
    bridge.stop();
}

/// The Connected status carries the link's telemetry; the address is set
/// from the start.
#[test]
fn the_card_shows_the_address_and_the_link() {
    let mut bridge = Bridge::start(|_| {});
    let addr = bridge.host.local_addr().expect("addr").to_string();
    bridge.telemetry_is(ADDRESS, &addr);
    let (_, mut host) = bridge.connect();
    host.next();
    let event = bridge.wait_for("Connected", |e| {
        matches!(
            e,
            Ev::Status {
                status: ActorStatus::Connected,
                ..
            }
        )
    });
    let Ev::Status { telemetry, .. } = event else {
        unreachable!()
    };
    assert_eq!(telemetry.get(SHOTS_SENT).map(String::as_str), Some("0"));
    assert_eq!(telemetry.get(STRICT_MODE).map(String::as_str), Some("off"));
    assert_eq!(
        telemetry.get(READY_STATE).map(String::as_str),
        Some("not ready")
    );
    bridge.stop();
}

/// A stop during the handshake ends the bridge at once.
#[test]
fn a_stop_during_the_handshake_returns_promptly() {
    let bridge = Bridge::start(|_| {});
    let mut host = bridge.accept();
    assert!(matches!(host.next(), Message::Hello { .. }));
    let at = Instant::now();
    bridge.stop();
    assert!(at.elapsed() < Duration::from_secs(1));
}

/// A virtual launch monitor that turns up after strict mode was accepted
/// (here under "Any" routing) has its shots kept back while the Omni's go;
/// once the round ends they go out again, and the next request is refused
/// because of it.
#[test]
fn a_virtual_monitor_that_turns_up_during_strict_mode_is_not_sent() {
    let bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    announce(&bridge.mock);
    status(&bridge.mock, ActorStatus::Connected);
    emit(&bridge.mock, 1, &mapper::tests::ball_only_shot());
    emit(&bridge.mock, 2, &mapper::tests::ball_only_shot());
    emit(&bridge.monitor, 1, &mapper::tests::full_shot());
    let ball = host.ball();
    assert_eq!(ball.shot_id, 1, "the mock's shots took no ShotId");
    assert_eq!(ball.ball_speed_ms, 64.0, "the Omni's shot");
    host.club();

    host.strict(false);
    emit(&bridge.mock, 3, &mapper::tests::ball_only_shot());
    assert_eq!(host.ball().shot_id, 2, "sent with strict mode off");
    assert_eq!(
        host.strict_ack(true),
        Message::StrictModeAck {
            accepted: false,
            reason: "mock_monitor.0 is a virtual launch monitor".into(),
        }
    );
    bridge.stop();
}

/// Bytes that are not a frame are skipped, with or without a frame behind
/// them, and the link carries on.
#[test]
fn garbage_from_the_host_is_skipped() {
    let bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.stream.write_all(&[0x13; 5000]).expect("write");
    host.assert_quiet(Duration::from_millis(150));
    let mut garbage_then_ping = vec![0x00, 0x50, 0x4C, 0x00, 0x99];
    garbage_then_ping.extend(Message::Ping.to_frame());
    host.stream.write_all(&garbage_then_ping).expect("write");
    assert!(matches!(host.next_not_ready(), Message::Heartbeat { .. }));
    bridge.stop();
}

/// Errors from Pureplay in quick succession raise one alert.
#[test]
fn host_errors_in_a_burst_raise_one_alert() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    for _ in 0..3 {
        host.send(&Message::Error {
            code: codec::ERROR_STRICT_MODE_REQUIRED,
            message: "Strict mode required".into(),
        });
    }
    host.send(&Message::Ping);
    assert!(matches!(host.next_not_ready(), Message::Heartbeat { .. }));
    // Everything the link sent before the Heartbeat is on the bus now.
    let mut alerts = Vec::new();
    while let Ok(msg) = bridge.events.try_recv() {
        if let Some(Ev::Alert { message }) = ev(msg) {
            alerts.push(message);
        }
    }
    assert_eq!(
        alerts,
        ["Pureplay error 2 (strict mode required): Strict mode required"]
    );
    bridge.stop();
}

/// An output started while the app is in Putting routes for Putting from
/// its first shot, not for Full.
#[test]
fn the_detection_mode_at_start_routes_the_first_shots() {
    let routing = GsProRouting {
        full_monitor: Some("mevo.0".into()),
        putting_monitor: Some("square.0".into()),
        ..GsProRouting::default()
    };
    let bridge = Bridge::start_with(routing, Mode::Putting, |omni| {
        status(omni, ActorStatus::Connected);
        device_telemetry(omni, &[("ready", "true")]);
    });
    let (_, mut host) = bridge.connect();
    assert_eq!(
        host.next(),
        Message::ReadyState(ReadyState::Ready.byte()),
        "the Omni's readiness, routed for Putting"
    );
    emit(&bridge.monitor, 1, &mapper::tests::full_shot());
    assert_eq!(host.ball().shot_id, 1);
    bridge.stop();
}

/// After a link ends, the Reconnecting status removes the link's keys and
/// the club and hand Pureplay set, and keeps the address.
#[test]
fn a_lost_link_clears_its_keys_from_the_card() {
    let mut bridge = Bridge::start(|_| {});
    let addr = bridge.host.local_addr().expect("addr").to_string();
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    bridge.telemetry_is(STRICT_MODE, "on");
    host.send(&Message::SetClubInfo {
        handedness: 0,
        club: 23,
    });
    bridge.wait_for("select", |e| matches!(e, Ev::Select(_)));
    // The link has handled SetClubInfo once a later Ping is answered.
    host.send(&Message::Ping);
    assert!(matches!(host.next_not_ready(), Message::Heartbeat { .. }));
    drop(host);

    let event = bridge.wait_for("Reconnecting", |e| {
        matches!(
            e,
            Ev::Status {
                status: ActorStatus::Reconnecting,
                ..
            }
        )
    });
    let Ev::Status { telemetry, .. } = event else {
        unreachable!()
    };
    for key in [SHOTS_SENT, READY_STATE, STRICT_MODE, "club", "handed"] {
        assert_eq!(telemetry.get(key).map(String::as_str), Some(""), "{key}");
    }
    assert_eq!(telemetry.get(ADDRESS), Some(&addr));
    bridge.stop();
}

/// A Ping that comes before the HelloAck is answered.
#[test]
fn a_ping_before_the_hello_ack_is_answered() {
    let bridge = Bridge::start(|omni| {
        status(omni, ActorStatus::Connected);
        device_telemetry(omni, &[("ready", "true")]);
    });
    let mut host = bridge.accept();
    assert!(matches!(host.next(), Message::Hello { .. }));
    host.send(&Message::Ping);
    assert_eq!(
        host.next(),
        Message::Heartbeat {
            connection_state: ConnectionState::Connected.byte(),
            ready_state: ReadyState::Ready.byte(),
            battery: codec::BATTERY_UNKNOWN,
        }
    );
    host.send(&Message::HelloAck {
        protocol_version: 1,
        host_caps: 0,
    });
    assert_eq!(host.next(), Message::ReadyState(ReadyState::Ready.byte()));
    bridge.stop();
}

/// A virtual launch monitor routed to the output: strict mode is refused
/// with the reason, nothing is held for Auto-chip, one alert says why, and
/// Pureplay's StrictModeRequired error that follows raises no second one.
#[test]
fn strict_mode_is_refused_when_a_routed_monitor_is_virtual() {
    let mut bridge = Bridge::start_configured(
        GsProRouting::default(),
        Mode::Full,
        &[("square.0", "Square Golf Omni"), ("mock_monitor.0", "Mock")],
        |_| {},
    );
    let (_, mut host) = bridge.connect();
    host.next();
    assert_eq!(
        host.strict_ack(true),
        Message::StrictModeAck {
            accepted: false,
            reason: "Mock is a virtual launch monitor".into(),
        }
    );
    assert!(!bridge.round.active(), "no strict round held");
    host.send(&Message::Error {
        code: codec::ERROR_STRICT_MODE_REQUIRED,
        message: "Strict mode required".into(),
    });
    host.send(&Message::Ping);
    assert!(matches!(host.next_not_ready(), Message::Heartbeat { .. }));
    assert_eq!(
        bridge.alerts(),
        [
            "Pureplay strict mode refused: Mock is a virtual launch monitor. Tournament rounds \
          take only shots a launch monitor measured."
        ]
    );

    // Strict mode stays off: the mock's shots go out as usual.
    emit(&bridge.mock, 1, &mapper::tests::ball_only_shot());
    assert_eq!(host.ball().shot_id, 1);

    // Pureplay closes the link; the next one asks again and is refused
    // again, without another alert in the same burst.
    drop(host);
    let (_, mut host) = bridge.connect();
    host.next();
    assert!(matches!(
        host.strict_ack(true),
        Message::StrictModeAck {
            accepted: false,
            ..
        }
    ));
    host.send(&Message::Ping);
    assert!(matches!(host.next_not_ready(), Message::Heartbeat { .. }));
    assert!(bridge.alerts().is_empty());
    bridge.stop();
}

/// The Open Connect input routed in one mode is measured, so strict mode
/// is accepted; so it is with no launch monitor at all.
#[test]
fn strict_mode_is_accepted_with_the_open_connect_input_routed() {
    let routing = GsProRouting {
        full_monitor: Some("square.0".into()),
        chipping_monitor: Some("square.0".into()),
        putting_monitor: Some("openconnect_server.0".into()),
    };
    let bridge = Bridge::start_configured(
        routing,
        Mode::Full,
        &[
            ("square.0", "Square Golf Omni"),
            ("openconnect_server.0", "Open Connect input"),
        ],
        |_| {},
    );
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    assert!(bridge.round.active());
    bridge.stop();

    let bridge = Bridge::start_configured(GsProRouting::default(), Mode::Full, &[], |_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    assert!(bridge.round.active());
    bridge.stop();
}

/// In strict mode only measured sources' shots go out: the mock and
/// Auto-chip take no ShotId; the Open Connect input, the R10 and the Omni
/// do.
#[test]
fn strict_mode_sends_measured_sources_only() {
    let bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    let input = bridge.source("openconnect_server.0");
    let r10 = bridge.source("r10.0");
    emit(&bridge.mock, 1, &mapper::tests::ball_only_shot());
    emit(&bridge.chip, 1, &mapper::tests::ball_only_shot());
    emit(&input, 1, &mapper::tests::ball_only_shot());
    let ball = host.ball();
    assert_eq!(
        ball.shot_id, 1,
        "the Open Connect input's shot is the first sent"
    );
    assert!((ball.ball_speed_ms - 44.704).abs() < 1e-3);
    emit(&r10, 1, &mapper::tests::ball_only_shot());
    assert_eq!(host.ball().shot_id, 2, "the R10's");
    emit(&bridge.monitor, 1, &mapper::tests::full_shot());
    let ball = host.ball();
    assert_eq!((ball.shot_id, ball.ball_speed_ms), (3, 64.0), "the Omni's");
    bridge.stop();
}

/// In a strict round, another actor's `shot.launch` that comes
/// between the launch monitor's trigger and its own launch is dropped, so
/// Pureplay gets the launch monitor's values as the measured shot.
#[test]
fn strict_mode_sends_the_launch_monitors_own_launch_only() {
    let bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    let r10 = bridge.source("r10.0");
    let intruder = bridge.source("intruder.0");
    let shot = mapper::tests::ball_only_shot();
    let key = key_of(r10.actor_id(), 1);
    r10.frp(flightrelay::ShotTrigger::new(key.clone()));
    intruder.frp(flightrelay::ShotLaunch {
        key: key.clone(),
        launch: flightrelay::Launch {
            launch_speed: Some(flighthook::Velocity::MetersPerSecond(99.0)),
            ..flightrelay::Launch::default()
        },
    });
    intruder.frp(flightrelay::ShotFinished { key: key.clone() });
    r10.frp(flightrelay::ShotLaunch {
        key: key.clone(),
        launch: shot.launch.clone().expect("launch"),
    });
    r10.frp(flightrelay::ShotFinished { key });
    let ball = host.ball();
    assert_eq!(ball.shot_id, 1);
    assert!((ball.ball_speed_ms - 44.704).abs() < 1e-3, "{ball:?}");
    bridge.stop();
}

/// A card added after the Omni connected learns the Omni's `bus.actor`
/// (and so its measured source) from the answer to the `bus.sync` it sends
/// at start, so the Omni's first shot goes out in strict mode.
#[test]
fn a_card_added_after_the_omni_connected_sends_its_first_shot_in_strict_mode() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    emit(&bridge.monitor, 1, &mapper::tests::full_shot());
    let ball = host.ball();
    assert_eq!((ball.shot_id, ball.ball_speed_ms), (1, 64.0), "the Omni's");
    // The bridge asked; the Omni's host answered.
    let mut asked = false;
    let mut answered = false;
    while let Ok(msg) = bridge.events.try_recv() {
        if let BusMsg::Frp { msg, .. } = msg {
            match msg.body {
                FrpBody::BusSync(_) if msg.from == "pureplay.0" => asked = true,
                FrpBody::BusActor(_) if asked && msg.from == "square.0" => answered = true,
                _ => {}
            }
        }
    }
    assert!(asked && answered, "bus.sync and the Omni's answer");
    bridge.stop();
}

/// The strict round is held for Auto-chip from the accepted ack until
/// strict mode ends, the link drops (the new link starts without it) or the
/// actor stops.
#[test]
fn strict_mode_holds_auto_chip_off_until_it_ends_or_the_link_drops() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    assert!(!bridge.round.active());
    host.strict(true);
    assert!(bridge.round.active(), "held once acked");
    host.strict(false);
    assert!(!bridge.round.active(), "released when the round ends");
    host.strict(true);
    assert!(bridge.round.active());

    drop(host);
    bridge.wait_for("Reconnecting", |e| {
        matches!(
            e,
            Ev::Status {
                status: ActorStatus::Reconnecting,
                ..
            }
        )
    });
    assert!(!bridge.round.active(), "released when the link drops");
    let (_, mut host) = bridge.connect();
    host.next();
    bridge.telemetry_is(STRICT_MODE, "off");
    assert!(!bridge.round.active(), "a new link starts without it");

    host.strict(true);
    assert!(bridge.round.active());
    bridge.stop();
    assert!(!bridge.round.active(), "released when the actor stops");
}

/// On the wire: a shot whose back and side spin are a model's (assumed in
/// the launch monitor's flight result) goes out with total and axis only in
/// strict mode, the measured-only receiver, and with all four outside it.
#[test]
fn strict_mode_withholds_assumed_spin_on_the_wire() {
    let bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    let r10 = bridge.source("r10.0");
    let mut shot = mapper::tests::full_shot();
    // The launch monitor measured total and axis; back and side are a
    // model's values, assumed in its flight result.
    let launch = shot.launch.as_mut().expect("launch");
    let assumed = flightrelay::AssumedLaunch {
        backspin_rpm: launch.backspin_rpm.take(),
        sidespin_rpm: launch.sidespin_rpm.take(),
        ..flightrelay::AssumedLaunch::default()
    };
    let mut flight = shot
        .flight
        .take()
        .unwrap_or_else(|| flightrelay::FlightResult::new(flightrelay::Basis::Tracked));
    flight.assumed = Some(assumed);
    shot.flight = Some(flight);

    emit(&r10, 1, &shot);
    let ball = host.ball();
    assert_eq!(ball.valid & BallData::SPIN, BallData::SPIN, "strict off");
    host.club();

    host.strict(true);
    emit(&r10, 2, &shot);
    let ball = host.ball();
    assert_eq!(
        ball.valid & BallData::SPIN,
        BallData::TOTAL_SPIN | BallData::SPIN_AXIS
    );
    assert_eq!((ball.back_spin_rpm, ball.side_spin_rpm), (0.0, 0.0));
    assert_eq!(ball.total_spin_rpm, 2468.0);
    bridge.stop();
}

/// On the wire: a virtual monitor that turns up ready during a round under
/// "Any" routing leaves the ready state and the heartbeat at the Omni's
/// (not ready); once the round ends it counts again.
#[test]
fn a_round_reports_readiness_from_measured_monitors_only() {
    let bridge = Bridge::start(|omni| status(omni, ActorStatus::Connected));
    let (_, mut host) = bridge.connect();
    assert_eq!(
        host.next(),
        Message::ReadyState(ReadyState::NotReady.byte())
    );
    host.strict(true);
    status(&bridge.mock, ActorStatus::Connected);
    device_telemetry(&bridge.mock, &[("ready", "true")]);
    host.assert_quiet(Duration::from_millis(300));
    host.send(&Message::Ping);
    assert_eq!(
        host.next(),
        Message::Heartbeat {
            connection_state: ConnectionState::Connected.byte(),
            ready_state: ReadyState::NotReady.byte(),
            battery: codec::BATTERY_UNKNOWN,
        }
    );
    host.send(&Message::SetStrictMode { strict: 0 });
    let mut seen = Vec::new();
    while seen.len() < 2 {
        seen.push(host.next());
    }
    assert!(
        seen.contains(&Message::ReadyState(ReadyState::Ready.byte())),
        "{seen:?}"
    );
    bridge.stop();
}

// -- The Pureplay Accessory card (a simulator source) --------------------------------------

/// The Pureplay Accessory card's shot, readiness, connection and battery never reach
/// the output, under "Any" and routed to it by ID, registered or not yet;
/// and it is never a feed a round names.
#[test]
fn a_pureplay_card_feeds_no_pureplay_connect_output() {
    let by_id = GsProRouting {
        full_monitor: Some("pureplay_accessory.0".into()),
        chipping_monitor: Some("pureplay_accessory.0".into()),
        putting_monitor: Some("pureplay_accessory.0".into()),
    };
    for (routing, round) in [
        (GsProRouting::default(), sources()),
        (by_id.clone(), sources()),
        (GsProRouting::default(), RoundPolicy::default()),
        (by_id, RoundPolicy::default()),
    ] {
        let (tx, _) = broadcast::channel(64);
        let card = source_sender("pureplay_accessory.0", &tx);
        let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
        let mut relay = Relay::new(routing.clone(), round.clone(), Some(test_state()));
        relay.connect(Instant::now());
        status(&card, ActorStatus::Connected);
        device_telemetry(&card, &[("ready", "true"), ("battery_pct", "90")]);
        emit(&card, 1, &mapper::tests::ball_only_shot());
        relay.drain(&mut rx).expect("bus");
        let now = Instant::now();
        assert!(relay.next_shot(now).is_none(), "{routing:?}");
        assert_eq!(relay.ready_state(now, None), ReadyState::NotReady);
        assert_eq!(relay.connection_state(None), ConnectionState::Disconnected);
        assert!(relay.monitors.is_empty());
        let configured = names(&[("pureplay_accessory.0", "Pureplay")]);
        assert!(relay.feeds(&configured).is_empty(), "{routing:?}");
        assert_eq!(refusal(&relay, &configured), None);
    }
}

/// Strict mode is accepted with the Pureplay Accessory card connected and ready, on
/// "Any" and with a mode routed to the card; its shots stay off the link
/// and the state Pureplay is told is the launch monitors' alone.
#[test]
fn strict_mode_is_accepted_with_the_pureplay_card_connected() {
    let by_id = GsProRouting {
        putting_monitor: Some("pureplay_accessory.0".into()),
        ..GsProRouting::default()
    };
    for routing in [GsProRouting::default(), by_id] {
        let bridge = Bridge::start_with(routing, Mode::Full, |_| {});
        let card = bridge.source("pureplay_accessory.0");
        status(&card, ActorStatus::Connected);
        device_telemetry(&card, &[("ready", "true"), ("battery_pct", "90")]);
        let (hello, mut host) = bridge.connect();
        assert!(
            matches!(&hello, Message::Hello { device_name, device_caps, .. }
                if device_name == "flighthook (Square Golf Omni)" && *device_caps == 0),
            "{hello:?}"
        );
        assert_eq!(
            host.next(),
            Message::ReadyState(ReadyState::NotReady.byte())
        );
        host.strict(true);
        assert!(bridge.round.active());
        emit(&card, 1, &mapper::tests::ball_only_shot());
        host.assert_quiet(Duration::from_millis(300));
        host.send(&Message::Ping);
        assert_eq!(
            host.next_not_ready(),
            Message::Heartbeat {
                connection_state: ConnectionState::Disconnected.byte(),
                ready_state: ReadyState::NotReady.byte(),
                battery: codec::BATTERY_UNKNOWN,
            }
        );
        bridge.stop();
    }
}

/// Each shot the output writes is recorded in the send registry with
/// Pureplay's address and the shot's own key; a shot strict mode keeps
/// back is not.
#[test]
fn each_written_shot_is_recorded_as_sent() {
    let bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    emit(&bridge.monitor, 7, &mapper::tests::full_shot());
    assert_eq!(host.ball().shot_id, 1);
    assert_eq!(host.club().shot_id, 1);
    let loopback =
        crate::state::sent_shots::SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 8765)));
    assert_eq!(
        bridge.sent.take_echo(loopback, Instant::now()),
        Some(key_of("square.0", 7))
    );
    host.strict(true);
    emit(&bridge.mock, 8, &mapper::tests::ball_only_shot());
    host.assert_quiet(Duration::from_millis(200));
    assert_eq!(bridge.sent.take_echo(loopback, Instant::now()), None);
    bridge.stop();
}

/// `PureplayActor::start`, as `start_actor` runs it, records each shot it
/// writes in the app's send registry (`SystemState::sent_shots`), where the
/// Pureplay Accessory card looks for it.
#[test]
fn the_started_actor_records_its_shots_in_the_apps_registry() {
    let dir = crate::actors::web::tests::TempDir(std::env::temp_dir().join(format!(
        "flighthook-pureplay-registry-{}",
        crate::state::config::generate_id()
    )));
    let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
    let state = Arc::new(state);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock host");
    let addr = listener.local_addr().expect("addr");
    let (tx, _rx) = broadcast::channel(1024);
    crate::actors::start_actor(
        "pureplay.0".into(),
        Box::new(PureplayActor {
            addr,
            routing: GsProRouting::default(),
        }),
        &state,
        &tx,
    );
    let (stream, _) = listener.accept().expect("the actor connects");
    stream
        .set_read_timeout(Some(Duration::from_millis(20)))
        .expect("timeout");
    let mut host = Host {
        stream,
        reader: FrameReader::new(),
    };
    host.next();
    host.send(&Message::HelloAck {
        protocol_version: 1,
        host_caps: 0,
    });
    state.round.register_source("square.0", ShotSource::Square);
    let monitor = source_sender("square.0", &tx);
    // The link counts from the handshake: shots before it are not sent.
    let deadline = Instant::now() + WAIT;
    let mut n = 0;
    loop {
        n += 1;
        emit(&monitor, n, &mapper::tests::ball_only_shot());
        if let Some(Message::BallData(_)) = host.next_within(Duration::from_millis(100)) {
            break;
        }
        assert!(Instant::now() < deadline, "no shot reached the host");
    }
    let loopback =
        crate::state::sent_shots::SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 8765)));
    // The shot written: the newest one put on the bus, or one before it
    // that was still on its way.
    let echo = state.sent_shots.take_echo(loopback, Instant::now());
    assert!(
        (1..=n).any(|m| echo == Some(key_of(monitor.actor_id(), m))),
        "{echo:?} is none of shots 1..={n}"
    );
    assert!(!matches!(
        state.stop_and_wait("pureplay.0"),
        crate::state::Stopped::Lingering
    ));
}

/// The output's holder name without a card name is "Pureplay Connect", so
/// the Tournament strip never reads as the Pureplay Accessory card's.
#[test]
fn the_holder_name_defaults_to_pureplay_connect() {
    let dir = std::env::temp_dir().join(format!(
        "flighthook-pureplay-holder-{}",
        crate::state::config::generate_id()
    ));
    let (state, _writer) = SystemState::new(dir.join("config.toml"));
    state.system.update(|c| {
        c.pureplay.insert(
            "0".into(),
            flighthook::PureplaySection {
                name: String::new(),
                ..flighthook::PureplaySection::default()
            },
        );
        c.pureplay.insert(
            "1".into(),
            flighthook::PureplaySection {
                name: "Bay 2".into(),
                ..flighthook::PureplaySection::default()
            },
        );
    });
    assert_eq!(holder_name(&state, "pureplay.0"), "Pureplay Connect");
    assert_eq!(holder_name(&state, "pureplay.1"), "Bay 2");
    let _ = std::fs::remove_dir_all(dir);
}

/// While linked, the card sends its held select again (club, hand and
/// mode) when flighthook's turn differs from it, and not when the turn
/// matches.
#[test]
fn the_card_reasserts_its_held_select_with_the_mode() {
    let (tx, _) = broadcast::channel(64);
    let session = BusSender::new("system".into(), tx.clone(), Arc::default())
        .with_frp_id(Some("flighthook".into()));
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = new_relay(GsProRouting::default());
    relay.connect(Instant::now());
    relay.hold_club(
        Some(flighthook::Handedness::Right),
        Some(flighthook::Club::SandWedge),
    );
    assert!(relay.hold_distance(20.0), "the distance makes it a chip");
    assert_eq!(relay.held.mode, Some(Mode::Chipping));

    let turn = |club, mode| GolferTurn::new(None, Some(club), flighthook::Handedness::Right, mode);
    session.frp_caused(
        turn(flighthook::Club::SandWedge, Mode::Chipping),
        Some("flighthook".into()),
    );
    relay.drain(&mut rx).expect("bus");
    assert!(!relay.reassert, "the turn matches");

    session.frp_caused(
        turn(flighthook::Club::SandWedge, Mode::Full),
        Some("flighthook".into()),
    );
    relay.drain(&mut rx).expect("bus");
    assert!(relay.reassert, "the mode differs");
    let select = relay.held.select("r".into());
    assert_eq!(select.club, Some(Some(flighthook::Club::SandWedge)));
    assert_eq!(select.handedness, Some(flighthook::Handedness::Right));
    assert_eq!(select.mode, Some(Mode::Chipping));
}

/// The card holds a mode only while it holds a club: a hand alone, with
/// or without a distance, asserts no mode.
#[test]
fn a_hand_without_a_club_holds_no_mode() {
    let mut relay = new_relay(GsProRouting::default());
    relay.hold_club(Some(flighthook::Handedness::Left), None);
    assert_eq!(relay.held.mode, None);
    assert!(!relay.hold_distance(20.0), "no club, no mode to change");
    assert_eq!(relay.held.mode, None);
    relay.hold_club(None, Some(flighthook::Club::SandWedge));
    assert_eq!(relay.held.mode, Some(Mode::Chipping), "the distance held");
}

/// A distance of 0 or less is unknown: the club decides (Full unless the
/// putter), and a later real distance updates the held mode.
#[test]
fn a_distance_of_zero_or_less_is_unknown() {
    let mut relay = new_relay(GsProRouting::default());
    assert!(!relay.hold_distance(0.0));
    relay.hold_club(
        Some(flighthook::Handedness::Right),
        Some(flighthook::Club::SandWedge),
    );
    assert_eq!(relay.held.mode, Some(Mode::Full));
    assert!(!relay.hold_distance(-1.0));
    assert_eq!(relay.held.mode, Some(Mode::Full));
    assert!(relay.hold_distance(12.0), "a chip");
    assert_eq!(relay.held.mode, Some(Mode::Chipping));
}

/// The re-assert follows the latest turn of a drain: a turn that differs,
/// then one that matches, re-asserts nothing.
#[test]
fn the_latest_turn_of_a_drain_decides_the_reassert() {
    let (tx, _) = broadcast::channel(64);
    let session = BusSender::new("system".into(), tx.clone(), Arc::default())
        .with_frp_id(Some("flighthook".into()));
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = new_relay(GsProRouting::default());
    relay.connect(Instant::now());
    relay.hold_club(
        Some(flighthook::Handedness::Right),
        Some(flighthook::Club::SandWedge),
    );
    let turn = |club, mode| GolferTurn::new(None, Some(club), flighthook::Handedness::Right, mode);
    session.frp_caused(
        turn(flighthook::Club::Driver, Mode::Full),
        Some("flighthook".into()),
    );
    session.frp_caused(
        turn(flighthook::Club::SandWedge, Mode::Full),
        Some("flighthook".into()),
    );
    relay.drain(&mut rx).expect("bus");
    assert!(!relay.reassert);
}

// -- Remote launch monitors ----------------------------------------------------

/// A remote launch monitor's `bus.actor`, as it arrives on a hub link.
fn remote_monitor(id: &str, name: &str, source: flighthook::Source) -> BusMsg {
    let actor = flightrelay::BusActor::new(name, 2.0)
        .with_role(flightrelay::types::roles::LAUNCH_MONITOR)
        .with_device(flightrelay::DeviceInfo {
            source: Some(source),
            ..flightrelay::DeviceInfo::default()
        });
    BusMsg::from_peer(
        flightrelay::FrpMessage::new(id, actor),
        crate::bus::PeerId(77),
    )
}

/// A remote virtual launch monitor refuses a strict round under
/// "Any", named by its `bus.actor` name; once it went silent (the system
/// actor forgets its source and announces it gone) the round is accepted.
#[test]
fn a_gone_remote_monitor_no_longer_refuses_a_round() {
    const REMOTE: &str = "aaaa000000000001";
    let (tx, _) = broadcast::channel(64);
    let mut rx = BusSender::new("pureplay.0".into(), tx.clone(), Arc::default()).subscribe();
    let mut relay = Relay::new(
        GsProRouting::default(),
        RoundPolicy::default(),
        Some(test_state()),
    );
    tx.send(remote_monitor(
        REMOTE,
        "Trackball",
        flighthook::Source::Virtual,
    ))
    .unwrap();
    relay
        .round
        .set_remote(REMOTE, Some(flighthook::Source::Virtual));
    relay.drain(&mut rx).expect("bus");
    assert_eq!(
        refusal(&relay, &names(&[])).as_deref(),
        Some("Trackball is a virtual launch monitor")
    );
    relay.round.forget_remote(REMOTE);
    tx.send(BusMsg::local(
        "system",
        LocalEvent::ActorGone {
            actor: REMOTE.into(),
        },
    ))
    .unwrap();
    relay.drain(&mut rx).expect("bus");
    assert_eq!(refusal(&relay, &names(&[])), None);
    assert!(relay.monitors.is_empty());
}
