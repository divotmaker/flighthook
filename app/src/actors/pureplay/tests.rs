use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use tokio::sync::broadcast;

use super::codec::{BallData, ClubData, Frame, kind};
use super::*;
use flighthook::{BallFlight, DerivedFlight, Velocity};

/// Longest any test waits for one thing before failing.
const WAIT: Duration = Duration::from_secs(5);

fn key(n: u32) -> ShotKey {
    ShotKey {
        shot_id: format!("shot-{n}"),
        shot_number: n,
    }
}

/// Put `shot`'s lifecycle on the bus from `monitor`: trigger, ball (with
/// its simulated block), club, impact, finished.
fn emit(monitor: &BusSender, n: u32, shot: &ShotData) {
    monitor.send(FlighthookMessage::new(FlighthookEvent::ShotTrigger {
        key: key(n),
    }));
    if let Some(ball) = &shot.ball {
        monitor.send(FlighthookMessage::new(FlighthookEvent::BallFlight {
            key: key(n),
            ball: Box::new(ball.clone()),
            derived: shot.derived.clone().map(Box::new),
        }));
    }
    if let Some(club) = &shot.club {
        monitor.send(FlighthookMessage::new(FlighthookEvent::ClubPath {
            key: key(n),
            club: Box::new(club.clone()),
        }));
    }
    if let Some(impact) = &shot.impact {
        monitor.send(FlighthookMessage::new(FlighthookEvent::FaceImpact {
            key: key(n),
            impact: Box::new(impact.clone()),
        }));
    }
    monitor.send(FlighthookMessage::new(FlighthookEvent::ShotFinished {
        key: key(n),
        raw: None,
    }));
}

fn status(monitor: &BusSender, status: ActorStatus) {
    monitor.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status,
        telemetry: HashMap::new(),
    }));
}

fn device_telemetry(monitor: &BusSender, pairs: &[(&str, &str)]) {
    monitor.send(FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
        manufacturer: None,
        model: None,
        firmware: None,
        telemetry: Some(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ),
    }));
}

/// A launch-only shot whose spin is flighthook's estimate.
fn estimated_spin_shot() -> ShotData {
    let mut derived = DerivedFlight::default();
    derived.spin_estimated = true;
    ShotData {
        ball: Some(BallFlight {
            launch_speed: Some(Velocity::MilesPerHour(20.0)),
            launch_elevation: Some(30.0),
            launch_azimuth: Some(0.5),
            backspin_rpm: Some(2000),
            sidespin_rpm: Some(0),
            ..BallFlight::default()
        }),
        derived: Some(derived),
        ..ShotData::default()
    }
}

// -- Relay (pure) ------------------------------------------------------------

fn relay_bus() -> (BusSender, BusReceiver) {
    let (tx, _) = broadcast::channel(256);
    let monitor = BusSender::new("square.0".into(), tx.clone(), Arc::default());
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
    let mut relay = Relay::new(GsProRouting::default());
    let now = Instant::now();
    assert_eq!(relay.ready_state(now), ReadyState::NotReady);
    assert_eq!(relay.connection_state(), ConnectionState::Disconnected);

    status(&monitor, ActorStatus::Starting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(), ConnectionState::Connecting);

    status(&monitor, ActorStatus::Connected);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(), ConnectionState::Connected);
    assert_eq!(
        relay.ready_state(now),
        ReadyState::NotReady,
        "not armed yet"
    );

    device_telemetry(&monitor, &[("ready", "true")]);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now()), ReadyState::Ready);

    monitor.send(FlighthookMessage::new(FlighthookEvent::ShotTrigger {
        key: key(1),
    }));
    relay.drain(&mut rx).expect("bus");
    let triggered = Instant::now();
    assert_eq!(relay.ready_state(triggered), ReadyState::Processing);
    // A shot that never finishes stops counting as processing.
    assert_eq!(
        relay.ready_state(triggered + PROCESSING_LIMIT),
        ReadyState::Ready
    );

    monitor.send(FlighthookMessage::new(FlighthookEvent::ShotFinished {
        key: key(1),
        raw: None,
    }));
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now()), ReadyState::Ready);

    status(&monitor, ActorStatus::Reconnecting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now()), ReadyState::NotReady);
    assert_eq!(relay.connection_state(), ConnectionState::Reconnecting);
}

/// A monitor that reports `ready=true` before this actor saw any status
/// from it (the output was added while the monitor was connected) is
/// Connected and Ready; its next drop still ends both.
#[test]
fn ready_before_any_status_counts_as_ready_and_connected() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = Relay::new(GsProRouting::default());
    device_telemetry(&monitor, &[("ready", "true"), ("battery_pct", "70")]);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now()), ReadyState::Ready);
    assert_eq!(relay.connection_state(), ConnectionState::Connected);
    assert_eq!(relay.battery(&names(&[("square.0", "Omni")])), Some(70));

    status(&monitor, ActorStatus::Reconnecting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now()), ReadyState::NotReady);
    assert_eq!(relay.connection_state(), ConnectionState::Reconnecting);

    // `ready=false` with no status seen says nothing about the connection.
    let (monitor, mut rx) = relay_bus();
    let mut relay = Relay::new(GsProRouting::default());
    device_telemetry(&monitor, &[("ready", "false")]);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(), ConnectionState::Disconnected);
}

/// An `error` a monitor reported holds until a status that ends its
/// session without the key: Starting, or a drop from Connected.
#[test]
fn a_monitor_error_ends_with_the_session_that_had_it() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = Relay::new(GsProRouting::default());
    let error_status = |status| {
        monitor.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
            status,
            telemetry: HashMap::from([("error".into(), "no adapter".into())]),
        }));
    };
    error_status(ActorStatus::Reconnecting);
    // A repeat without the key changes nothing.
    status(&monitor, ActorStatus::Reconnecting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(), ConnectionState::Error);

    status(&monitor, ActorStatus::Connected);
    status(&monitor, ActorStatus::Reconnecting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(), ConnectionState::Reconnecting);

    error_status(ActorStatus::Reconnecting);
    status(&monitor, ActorStatus::Starting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(), ConnectionState::Connecting);
}

/// A shot that never finishes is let go at a trigger a minute later.
#[test]
fn unfinished_shots_are_let_go() {
    let mut relay = Relay::new(GsProRouting::default());
    let trigger = |n| {
        let mut msg = FlighthookMessage::new(FlighthookEvent::ShotTrigger { key: key(n) });
        msg.actor = "square.0".into();
        msg
    };
    let start = Instant::now();
    relay.handle(trigger(1), start);
    relay.handle(trigger(2), start + Duration::from_secs(1));
    assert_eq!(relay.accumulators.len(), 2);
    relay.handle(
        trigger(3),
        start + UNFINISHED_SHOT + Duration::from_millis(500),
    );
    let mut left: Vec<u32> = relay
        .accumulators
        .keys()
        .map(|(_, k)| k.shot_number)
        .collect();
    left.sort_unstable();
    assert_eq!(left, [2, 3]);
}

/// Errors from Pureplay less than 30 s apart are one burst.
#[test]
fn host_errors_are_alerted_once_per_burst() {
    let mut relay = Relay::new(GsProRouting::default());
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
        address: None,
        full_monitor: Some("mevo.0".into()),
        chipping_monitor: None,
        putting_monitor: None,
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
    let mut relay = Relay::new(GsProRouting::default());
    monitor.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status: ActorStatus::Reconnecting,
        telemetry: HashMap::from([("error".into(), "no adapter".into())]),
    }));
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(), ConnectionState::Error);
    status(&monitor, ActorStatus::Connected);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.connection_state(), ConnectionState::Connected);
}

/// Routed to the Omni, the device's state is the Omni's alone; outputs
/// and other monitors do not count.
#[test]
fn the_routed_monitor_alone_sets_the_state() {
    let (tx, _) = broadcast::channel(256);
    let omni = BusSender::new("square.0".into(), tx.clone(), Arc::default());
    let mevo = BusSender::new("mevo.0".into(), tx.clone(), Arc::default());
    let gspro = BusSender::new("openconnect_client.0".into(), tx.clone(), Arc::default());
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = Relay::new(GsProRouting {
        full_monitor: Some("square.0".into()),
        ..GsProRouting::default()
    });
    status(&mevo, ActorStatus::Connected);
    device_telemetry(&mevo, &[("ready", "true"), ("battery_pct", "50")]);
    status(&gspro, ActorStatus::Connected);
    status(&omni, ActorStatus::Starting);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now()), ReadyState::NotReady);
    assert_eq!(relay.connection_state(), ConnectionState::Connecting);
    let n = names(&[("square.0", "Omni"), ("mevo.0", "Mevo")]);
    assert_eq!(relay.battery(&n), None);
    assert!(!relay.monitors.contains_key("openconnect_client.0"));

    status(&omni, ActorStatus::Connected);
    device_telemetry(&omni, &[("ready", "true"), ("battery_pct", "64")]);
    relay.drain(&mut rx).expect("bus");
    assert_eq!(relay.ready_state(Instant::now()), ReadyState::Ready);
    assert_eq!(relay.battery(&n), Some(64));
}

/// With Any routing, the best state of every monitor counts, and the
/// battery and name are those of the only configured monitor.
#[test]
fn any_routing_takes_the_best_monitor_and_names_a_lone_one() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = Relay::new(GsProRouting::default());
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
    assert_eq!(relay.ready_state(Instant::now()), ReadyState::Ready);
}

/// A monitor that never reported a battery gives no battery capability.
#[test]
fn no_battery_report_means_no_battery_capability() {
    let (monitor, mut rx) = relay_bus();
    let mut relay = Relay::new(GsProRouting::default());
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
    let omni = BusSender::new("square.0".into(), tx.clone(), Arc::default());
    let mevo = BusSender::new("mevo.0".into(), tx.clone(), Arc::default());
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut relay = Relay::new(GsProRouting {
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

/// Strict mode is accepted only when every launch monitor routed to the
/// output, in any detection mode, is measured; the reason names the first
/// one that is not, and its class.
#[test]
fn strict_mode_needs_every_routed_monitor_measured() {
    let any = Relay::new(GsProRouting::default());
    assert_eq!(any.strict_refusal(&names(&[])), None, "no monitor at all");
    let measured = names(&[
        ("square.0", "Omni"),
        ("mevo.0", "Mevo+"),
        ("r10.0", "R10"),
        ("exputt.0", "ExPutt"),
    ]);
    assert_eq!(any.strict_refusal(&measured), None);

    let with_mock = names(&[("square.0", "Omni"), ("mock_monitor.0", "Mock")]);
    assert_eq!(
        any.strict_refusal(&with_mock).as_deref(),
        Some("Mock is a virtual launch monitor"),
        "Any routing takes the mock's shots"
    );
    let omni_only = Relay::new(GsProRouting {
        full_monitor: Some("square.0".into()),
        chipping_monitor: Some("square.0".into()),
        putting_monitor: Some("square.0".into()),
    });
    assert_eq!(
        omni_only.strict_refusal(&with_mock),
        None,
        "a configured mock routed nowhere does not count"
    );

    let relayed = Relay::new(GsProRouting {
        full_monitor: Some("square.0".into()),
        chipping_monitor: Some("square.0".into()),
        putting_monitor: Some("openconnect_server.0".into()),
    });
    assert_eq!(
        relayed
            .strict_refusal(&names(&[
                ("square.0", "Omni"),
                ("openconnect_server.0", "OpenConnect input"),
            ]))
            .as_deref(),
        Some("OpenConnect input relays shots from another app"),
        "routed in one mode is enough"
    );

    // A launch monitor seen on the bus counts for Any too, named by its ID
    // when it has no name.
    let (tx, _) = broadcast::channel(16);
    let mock = BusSender::new("mock_monitor.1".into(), tx.clone(), Arc::default());
    let mut rx = BusSender::new("pureplay.0".into(), tx, Arc::default()).subscribe();
    let mut seen = Relay::new(GsProRouting::default());
    status(&mock, ActorStatus::Connected);
    seen.drain(&mut rx).expect("bus");
    assert_eq!(
        seen.strict_refusal(&names(&[("square.0", "Omni")]))
            .as_deref(),
        Some("mock_monitor.1 is a virtual launch monitor")
    );
    assert_eq!(
        any.strict_refusal(&names(&[("mock_monitor.0", " ")]))
            .as_deref(),
        Some("mock_monitor.0 is a virtual launch monitor")
    );
}

// -- Against a mock Pureplay host -------------------------------------------

/// A running `pureplay.0` wired to a mock host it dials.
struct Bridge {
    host: TcpListener,
    tx: broadcast::Sender<FlighthookMessage>,
    rounds: StrictRounds,
    monitor: BusSender,
    chip: BusSender,
    mock: BusSender,
    events: broadcast::Receiver<FlighthookMessage>,
    shutdown: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
}

impl Bridge {
    /// Start the bridge with the Omni (`square.0`, "Square Golf Omni") as
    /// the only launch monitor. `before` puts messages on the bus ahead of
    /// the bridge's first look at it.
    fn start(before: impl FnOnce(&BusSender)) -> Self {
        Self::start_with(GsProRouting::default(), ShotDetectionMode::Full, before)
    }

    /// [`start`](Self::start) with `routing`, the app in detection mode
    /// `mode` when the bridge starts.
    fn start_with(
        routing: GsProRouting,
        mode: ShotDetectionMode,
        before: impl FnOnce(&BusSender),
    ) -> Self {
        Self::start_configured(routing, mode, &[("square.0", "Square Golf Omni")], before)
    }

    /// [`start_with`](Self::start_with) with `monitors` (actor ID, name)
    /// as the configured launch monitors.
    fn start_configured(
        routing: GsProRouting,
        mode: ShotDetectionMode,
        monitors: &[(&str, &str)],
        before: impl FnOnce(&BusSender),
    ) -> Self {
        let configured = names(monitors);
        let rounds = StrictRounds::default();
        let host = TcpListener::bind("127.0.0.1:0").expect("bind mock host");
        host.set_nonblocking(true).expect("nonblocking");
        let addr = host.local_addr().expect("addr");
        let (tx, events) = broadcast::channel(1024);
        let shutdown = Arc::new(AtomicBool::new(false));
        let sender = BusSender::new("pureplay.0".into(), tx.clone(), Arc::clone(&shutdown));
        let receiver = sender.subscribe();
        let monitor = BusSender::new("square.0".into(), tx.clone(), Arc::default());
        let chip = BusSender::new(
            flighthook::AUTO_CHIP_ACTOR.into(),
            tx.clone(),
            Arc::default(),
        );
        let mock = BusSender::new("mock_monitor.0".into(), tx.clone(), Arc::default());
        before(&monitor);
        let (done_tx, done) = mpsc::channel();
        let shared_rounds = rounds.clone();
        std::thread::spawn(move || {
            let names = move || configured.clone();
            run_with(
                addr,
                routing,
                mode,
                Backoff::new(Duration::from_millis(20), Duration::from_millis(20)),
                Shared {
                    names: &names,
                    rounds: shared_rounds,
                },
                &sender,
                receiver,
            );
            let _ = done_tx.send(());
        });
        Self {
            host,
            tx,
            rounds,
            monitor,
            chip,
            mock,
            events,
            shutdown,
            done,
        }
    }

    /// A bus sender for the actor `actor`, to put its shots on the bus.
    fn source(&self, actor: &str) -> BusSender {
        BusSender::new(actor.into(), self.tx.clone(), Arc::default())
    }

    /// The `pureplay.0` alerts on the bus so far.
    fn alerts(&mut self) -> Vec<String> {
        let mut alerts = Vec::new();
        while let Ok(msg) = self.events.try_recv() {
            if let (true, FlighthookEvent::Alert { message, .. }) =
                (msg.actor == "pureplay.0", msg.event)
            {
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
    fn wait_for(&mut self, what: &str, want: impl Fn(&FlighthookEvent) -> bool) -> FlighthookEvent {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.events.try_recv() {
                Ok(msg) if msg.actor == "pureplay.0" && want(&msg.event) => return msg.event,
                Ok(_) => {}
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
            matches!(e, FlighthookEvent::ActorStatus { telemetry, .. }
                if telemetry.get(key).is_some_and(|v| want(v)))
        });
        let FlighthookEvent::ActorStatus { telemetry, .. } = event else {
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
            FlighthookEvent::ActorStatus {
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
        matches!(e, FlighthookEvent::ActorStatus { telemetry, .. }
            if telemetry.get(SHOTS_SENT).is_some_and(|v| v == "2")
                && telemetry.get(LAST_SHOT).is_some_and(|v| v.starts_with("#2 at ")))
    });
    bridge.stop();
}

/// SetClubInfo sets the hand and club on the bus, like a simulator's
/// player information; an unknown club is ignored and the hand still set.
#[test]
fn set_club_info_sets_the_hand_and_club() {
    let mut bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.send(&Message::SetClubInfo {
        handedness: 1,
        club: 23,
    });
    let hand = bridge.wait_for("hand", |e| matches!(e, FlighthookEvent::HandInfo { .. }));
    assert!(matches!(
        hand,
        FlighthookEvent::HandInfo {
            hand_info: HandInfo {
                handed: flighthook::Handedness::Left
            }
        }
    ));
    let club = bridge.wait_for("club", |e| matches!(e, FlighthookEvent::ClubInfo { .. }));
    assert!(
        matches!(club, FlighthookEvent::ClubInfo { ref club_info } if club_info.club == flighthook::Club::Iron7),
        "{club:?}"
    );

    host.send(&Message::SetClubInfo {
        handedness: 0,
        club: 200,
    });
    bridge.wait_for("hand", |e| {
        matches!(
            e,
            FlighthookEvent::HandInfo {
                hand_info: HandInfo {
                    handed: flighthook::Handedness::Right
                }
            }
        )
    });
    // The next club comes from the next valid command, not the unknown one.
    host.send(&Message::SetClubInfo {
        handedness: 0,
        club: 0,
    });
    let club = bridge.wait_for("club", |e| matches!(e, FlighthookEvent::ClubInfo { .. }));
    assert!(
        matches!(club, FlighthookEvent::ClubInfo { ref club_info } if club_info.club == flighthook::Club::Driver),
        "{club:?}"
    );
    bridge.stop();
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
            FlighthookEvent::ActorStatus {
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
            FlighthookEvent::ActorStatus {
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
            FlighthookEvent::ActorStatus {
                status: ActorStatus::Connected,
                ..
            }
        )
    });
    let FlighthookEvent::ActorStatus { telemetry, .. } = event else {
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
        if let (true, FlighthookEvent::Alert { message, .. }) =
            (msg.actor == "pureplay.0", msg.event)
        {
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
    let bridge = Bridge::start_with(routing, ShotDetectionMode::Putting, |omni| {
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
    bridge.wait_for("club", |e| matches!(e, FlighthookEvent::ClubInfo { .. }));
    // The link has handled SetClubInfo once a later Ping is answered.
    host.send(&Message::Ping);
    assert!(matches!(host.next_not_ready(), Message::Heartbeat { .. }));
    drop(host);

    let event = bridge.wait_for("Reconnecting", |e| {
        matches!(
            e,
            FlighthookEvent::ActorStatus {
                status: ActorStatus::Reconnecting,
                ..
            }
        )
    });
    let FlighthookEvent::ActorStatus { telemetry, .. } = event else {
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
        ShotDetectionMode::Full,
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
    assert!(!bridge.rounds.any(), "no strict round held");
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
          take shots from a launch monitor flighthook reads directly."
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

/// The Open Connect input routed in one mode refuses strict mode; with no
/// launch monitor at all strict mode is accepted.
#[test]
fn strict_mode_is_refused_with_the_open_connect_input_routed() {
    let routing = GsProRouting {
        full_monitor: Some("square.0".into()),
        chipping_monitor: Some("square.0".into()),
        putting_monitor: Some("openconnect_server.0".into()),
    };
    let bridge = Bridge::start_configured(
        routing,
        ShotDetectionMode::Full,
        &[
            ("square.0", "Square Golf Omni"),
            ("openconnect_server.0", "OpenConnect input"),
        ],
        |_| {},
    );
    let (_, mut host) = bridge.connect();
    host.next();
    assert_eq!(
        host.strict_ack(true),
        Message::StrictModeAck {
            accepted: false,
            reason: "OpenConnect input relays shots from another app".into(),
        }
    );
    bridge.stop();

    let bridge = Bridge::start_configured(
        GsProRouting::default(),
        ShotDetectionMode::Full,
        &[],
        |_| {},
    );
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    assert!(bridge.rounds.any());
    bridge.stop();
}

/// In strict mode only measured sources' shots go out: the mock, Auto-chip
/// and the Open Connect input take no ShotId, the R10 and the Omni do.
#[test]
fn strict_mode_sends_measured_sources_only() {
    let bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    host.strict(true);
    let relayed = bridge.source("openconnect_server.0");
    let r10 = bridge.source("r10.0");
    emit(&bridge.mock, 1, &mapper::tests::ball_only_shot());
    emit(&bridge.chip, 1, &mapper::tests::ball_only_shot());
    emit(&relayed, 1, &mapper::tests::ball_only_shot());
    emit(&relayed, 2, &mapper::tests::ball_only_shot());
    emit(&r10, 1, &mapper::tests::ball_only_shot());
    let ball = host.ball();
    assert_eq!(ball.shot_id, 1, "the R10's shot is the first sent");
    assert!((ball.ball_speed_ms - 44.704).abs() < 1e-3);
    emit(&bridge.monitor, 1, &mapper::tests::full_shot());
    let ball = host.ball();
    assert_eq!((ball.shot_id, ball.ball_speed_ms), (2, 64.0), "the Omni's");
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
    assert!(!bridge.rounds.any());
    host.strict(true);
    assert!(bridge.rounds.any(), "held once acked");
    host.strict(false);
    assert!(!bridge.rounds.any(), "released when the round ends");
    host.strict(true);
    assert!(bridge.rounds.any());

    drop(host);
    bridge.wait_for("Reconnecting", |e| {
        matches!(
            e,
            FlighthookEvent::ActorStatus {
                status: ActorStatus::Reconnecting,
                ..
            }
        )
    });
    assert!(!bridge.rounds.any(), "released when the link drops");
    let (_, mut host) = bridge.connect();
    host.next();
    bridge.telemetry_is(STRICT_MODE, "off");
    assert!(!bridge.rounds.any(), "a new link starts without it");

    host.strict(true);
    assert!(bridge.rounds.any());
    bridge.stop();
    assert!(!bridge.rounds.any(), "released when the actor stops");
}

/// On the wire: an R10 shot (back and side spin computed from the measured
/// total and axis) goes out with total and axis only in strict mode, and
/// with all four outside it.
#[test]
fn strict_mode_withholds_computed_spin_on_the_wire() {
    let bridge = Bridge::start(|_| {});
    let (_, mut host) = bridge.connect();
    host.next();
    let r10 = bridge.source("r10.0");
    let mut shot = mapper::tests::full_shot();
    let mut derived = None;
    DerivedFlight::mark_computed(
        &mut derived,
        &[
            flighthook::ShotField::BackspinRpm,
            flighthook::ShotField::SidespinRpm,
        ],
    );
    shot.derived = derived.map(|d| *d);

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
