//! Session tests on the SDK's test host: one thread, virtual time, an
//! in-memory link to a scripted Pureplay; one test over a real socket.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use flightrelay::driver::{
    Activity, Backoff, DeviceEvent, DeviceIdentity, FrpDriver, FrpSession, Live, SessionEnd,
    StatusChange,
};
use flightrelay::testing::TestHost;
use flightrelay::types::{AssumedLaunch, Basis, FlightResult, Launch, roles};
use flightrelay::{
    BusActor, Club, DeviceInfo, FrpBody, FrpMessage, GolferSelect, Handedness, Mode, RawDir,
    Removal, ShotClub, ShotFinished, ShotFlight, ShotImpact, ShotKey, ShotLaunch, ShotTrigger,
    Source, StateKind, Velocity,
};

use super::*;
use crate::connect::codec::FrameReader;
use crate::connect::mapper::tests::{ball_only_shot, full_shot};
use crate::connect::{DEFAULT_ADDRESS, mapper};

// -- The link ------------------------------------------------------------------

/// Both ends of an in-memory link: what Pureplay sent the device, what the
/// device wrote, and whether Pureplay closed it.
#[derive(Default)]
struct Pipe {
    to_device: VecDeque<u8>,
    from_device: Vec<u8>,
    closed: bool,
}

/// The device's end of the link: reads what Pureplay sent (would block when
/// there is nothing), writes into the pipe.
#[derive(Clone, Default)]
struct Wire(Rc<RefCell<Pipe>>);

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut pipe = self.0.borrow_mut();
        if pipe.to_device.is_empty() {
            return if pipe.closed {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let n = buf.len().min(pipe.to_device.len());
        for (slot, byte) in buf.iter_mut().zip(pipe.to_device.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().from_device.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One link under test: the test host, the session over a [`Wire`], and
/// Pureplay's end.
struct Link {
    host: TestHost,
    session: Session<Wire>,
    wire: Wire,
    reader: FrameReader,
}

impl Link {
    /// A link whose session has not stepped yet, with errors from Pureplay
    /// counted in `host_errors`.
    fn unstarted(host: TestHost, host_errors: Live<Option<Instant>>) -> Self {
        let wire = Wire::default();
        let session = Session::new(
            wire.clone(),
            DeviceIdentity::new("127.0.0.1:9760"),
            host_errors,
            host.now(),
        );
        Self {
            host,
            session,
            wire,
            reader: FrameReader::new(),
        }
    }

    /// [`unstarted`](Self::unstarted), after the first step: Hello sent.
    fn with_errors(host: TestHost, host_errors: Live<Option<Instant>>) -> Self {
        let mut link = Self::unstarted(host, host_errors);
        link.step();
        link
    }

    /// A link whose session has sent Hello.
    fn new() -> Self {
        Self::with_errors(TestHost::new(), Live::new(None))
    }

    /// A linked session: Hello answered, the first ReadyState read.
    fn linked() -> Self {
        let mut link = Self::new();
        link.shake();
        link
    }

    /// One step of the session.
    fn try_step(&mut self) -> Result<Activity, Error> {
        let Self { host, session, .. } = self;
        host.with(|now, out| session.step_at(now, out))
    }

    fn step(&mut self) {
        self.try_step().expect("the link holds");
    }

    /// Step until the session idles.
    fn settle(&mut self) {
        while self.try_step().expect("the link holds") == Activity::Active {}
    }

    /// Hand the session a message from actor `from`.
    fn hand(&mut self, from: &str, body: impl Into<FrpBody>) {
        self.session.hand(&FrpMessage::new(from, body));
    }

    /// Pureplay sends `msg`.
    fn send(&mut self, msg: &Message) {
        self.send_bytes(&msg.to_frame());
    }

    fn send_bytes(&mut self, bytes: &[u8]) {
        self.wire.0.borrow_mut().to_device.extend(bytes);
    }

    /// Everything the session wrote since the last call.
    fn written(&mut self) -> Vec<Message> {
        let bytes = std::mem::take(&mut self.wire.0.borrow_mut().from_device);
        self.reader.push(&bytes);
        let mut out = Vec::new();
        while let Some(f) = self.reader.next_frame() {
            assert!(self.reader.take_dropped().is_empty(), "clean framing");
            assert_eq!(f.version, 1);
            out.push(Message::decode(f.kind, &f.payload).expect("device messages decode"));
        }
        out
    }

    /// What the session wrote since the last call, ReadyState pushes left
    /// out.
    fn written_not_ready(&mut self) -> Vec<Message> {
        self.written()
            .into_iter()
            .filter(|m| !matches!(m, Message::ReadyState(_)))
            .collect()
    }

    /// Answer Hello and step: the link is up. Returns the Hello.
    fn shake(&mut self) -> Message {
        let hello = self.written().remove(0);
        self.send(&Message::HelloAck {
            protocol_version: 1,
            host_caps: 0,
        });
        self.settle();
        hello
    }

    /// Pureplay pings; the session's answer.
    fn ping(&mut self) -> Message {
        self.send(&Message::Ping);
        self.settle();
        self.written_not_ready()
            .pop()
            .expect("the ping is answered")
    }

    /// Ask for strict mode and check it is accepted.
    fn strict(&mut self, on: bool) {
        self.send(&Message::SetStrictMode {
            strict: u8::from(on),
        });
        self.settle();
        assert_eq!(
            self.written_not_ready(),
            [Message::StrictModeAck {
                accepted: true,
                reason: String::new()
            }]
        );
    }

    /// The BallData (and ClubData) the session wrote since the last call.
    fn balls(&mut self) -> Vec<BallData> {
        self.written()
            .into_iter()
            .filter_map(|m| match m {
                Message::BallData(b) => Some(b),
                _ => None,
            })
            .collect()
    }

    /// Hand `shot` (trigger, launch, flight, club, impact, finished) as shot
    /// `n` of launch monitor `actor`, then settle.
    fn shot(&mut self, actor: &str, n: u32, shot: &OutputShot) {
        let key = key_of(actor, n);
        self.hand(actor, ShotTrigger::new(key.clone()));
        if let Some(launch) = &shot.launch {
            self.hand(
                actor,
                ShotLaunch {
                    key: key.clone(),
                    launch: launch.clone(),
                },
            );
        }
        if let Some(flight) = &shot.flight {
            self.hand(
                actor,
                ShotFlight {
                    key: key.clone(),
                    flight: flight.clone(),
                },
            );
        }
        if let Some(club) = &shot.club {
            self.hand(
                actor,
                ShotClub {
                    key: key.clone(),
                    club: club.clone(),
                },
            );
        }
        if let Some(impact) = &shot.impact {
            self.hand(
                actor,
                ShotImpact {
                    key: key.clone(),
                    impact: impact.clone(),
                },
            );
        }
        self.hand(actor, ShotFinished { key });
        self.settle();
    }

    /// The events since the last call.
    fn events(&mut self) -> Vec<DeviceEvent> {
        self.host.take()
    }
}

/// Shot `n` of launch monitor `actor`.
fn key_of(actor: &str, n: u32) -> ShotKey {
    ShotKey::new(format!("{actor}:shot-{n}"), n)
}

/// A launch monitor's `bus.actor` named `name`, declaring `source`, ready
/// (no blockers) or not (no ball), with `battery`.
fn monitor(name: &str, source: Source, ready: bool, battery: Option<u8>) -> BusActor {
    let mut actor = BusActor::new(name, 2.0)
        .with_role(roles::LAUNCH_MONITOR)
        .with_device(DeviceInfo {
            source: Some(source),
            battery_pct: battery,
            ..DeviceInfo::default()
        });
    if !ready {
        actor.blockers.push("no_ball".into());
    }
    actor
}

/// A launch monitor that is not connected: blocked by `not_armed`.
fn not_armed(name: &str, source: Source) -> BusActor {
    let mut actor = monitor(name, source, true, None);
    actor.blockers.push("not_armed".into());
    actor
}

/// The telemetry set in `events`, the last value of each key (an empty
/// value for a cleared key).
fn telemetry(events: &[DeviceEvent]) -> std::collections::HashMap<String, String> {
    let mut rows = std::collections::HashMap::new();
    for e in events {
        if let DeviceEvent::StatusChange(StatusChange::Telemetry(pairs)) = e {
            for (k, v) in pairs {
                rows.insert(k.clone(), v.clone().unwrap_or_default());
            }
        }
    }
    rows
}

/// The `golfer.select`s in `events`.
fn selects(events: &[DeviceEvent]) -> Vec<GolferSelect> {
    events
        .iter()
        .filter_map(|e| match e {
            DeviceEvent::Frp(FrpBody::GolferSelect(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// The alerts in `events`.
fn alerts(events: &[DeviceEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            DeviceEvent::Frp(FrpBody::BusAlert(a)) => Some(a.message.clone()),
            _ => None,
        })
        .collect()
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
        flight: Some(FlightResult {
            assumed: Some(AssumedLaunch {
                backspin_rpm: Some(2000),
                sidespin_rpm: Some(0),
                ..AssumedLaunch::default()
            }),
            ..FlightResult::new(Basis::Modeled)
        }),
        ..OutputShot::default()
    }
}

/// The Pureplay code of `club`.
fn club_code(club: Club) -> u8 {
    (0..=30)
        .find(|&c| mapper::club_from_pureplay(c) == Some(club))
        .expect("club has a Pureplay code")
}

// -- Pure rules ----------------------------------------------------------------

#[test]
fn the_lie_gives_the_mode() {
    assert_eq!(mode_for_lie(Some(Club::Putter), Some(150.0)), Mode::Putting);
    assert_eq!(
        mode_for_lie(Some(Club::SandWedge), Some(20.0)),
        Mode::Chipping
    );
    assert_eq!(
        mode_for_lie(Some(Club::SandWedge), Some(27.432)),
        Mode::Full
    );
    assert_eq!(mode_for_lie(Some(Club::Driver), None), Mode::Full);
    assert_eq!(mode_for_lie(Some(Club::SandWedge), Some(0.0)), Mode::Full);
    assert_eq!(mode_for_lie(Some(Club::SandWedge), Some(-5.0)), Mode::Full);
    assert_eq!(mode_for_lie(None, Some(5.0)), Mode::Chipping);
}

/// Strict mode sends a measured shot and holds back any other (virtual, or
/// a source that declares none); outside strict mode every shot is sent.
#[test]
fn strict_mode_sends_only_measured_shots() {
    assert!(sends(true, Some(Source::Measured)));
    assert!(!sends(true, Some(Source::Virtual)));
    assert!(!sends(true, None));
    for source in [Some(Source::Measured), Some(Source::Virtual), None] {
        assert!(sends(false, source), "{source:?}");
    }
}

/// An address that is not `ip:port` is refused; none is the default.
#[test]
fn the_address_must_be_ip_and_port() {
    let config = |a: Option<&str>| Config {
        address: a.map(str::to_owned),
    };
    assert_eq!(
        config(None).addr().map(|a| a.to_string()),
        Ok(DEFAULT_ADDRESS.to_owned())
    );
    assert!(config(Some("192.168.1.20:9760")).validate().is_ok());
    assert!(config(Some("pureplay.local")).validate().is_err());
    assert!(Connect::new(config(Some(""))).is_err());
}

/// The retry delay grows by a second up to 3 s with no alert; a failed
/// handshake puts its reason in `error`.
#[test]
fn retries_back_off_one_to_three_seconds_and_a_handshake_failure_shows() {
    let mut driver = Connect::new(Config::default()).expect("driver");
    let mut host = TestHost::new();
    let refused = Error::Io(io::ErrorKind::ConnectionRefused.into());
    let delays: Vec<_> = (0..4)
        .map(|_| {
            host.with(|_, out| {
                driver.retry_delay(
                    &SessionEnd {
                        error: Some(&refused),
                        connected_for: None,
                        attempts: 1,
                    },
                    &mut Backoff::default(),
                    out,
                )
            })
        })
        .collect();
    let secs = Duration::from_secs;
    assert_eq!(
        delays,
        [Some(secs(1)), Some(secs(2)), Some(secs(3)), Some(secs(3))]
    );
    assert!(host.take().is_empty(), "no alert, no row");

    let failed = Error::Handshake("no reply".into());
    host.with(|_, out| {
        driver.retry_delay(
            &SessionEnd {
                error: Some(&failed),
                connected_for: Some(Duration::ZERO),
                attempts: 0,
            },
            &mut Backoff::default(),
            out,
        )
    });
    let events = host.take();
    assert_eq!(telemetry(&events)["error"], "no reply");
    assert!(alerts(&events).is_empty());
}

// -- The handshake -------------------------------------------------------------

/// The handshake, the first ReadyState and a Ping answered with the
/// monitor's state and battery; a drop of the monitor is pushed at once.
#[test]
fn handshake_ready_state_and_heartbeat() {
    let mut link = Link::unstarted(TestHost::new(), Live::new(None));
    link.hand(
        "omni",
        monitor("Square Golf Omni", Source::Measured, true, Some(80)),
    );
    link.step();
    let hello = link.shake();
    assert_eq!(
        hello,
        Message::Hello {
            protocol_version: 1,
            device_caps: codec::CAP_BATTERY_POWERED,
            device_name: "flighthook (Square Golf Omni)".into(),
            firmware: env!("CARGO_PKG_VERSION").into(),
        }
    );
    assert_eq!(
        link.written(),
        [Message::ReadyState(ReadyState::Ready.byte())]
    );
    assert_eq!(
        link.ping(),
        Message::Heartbeat {
            connection_state: ConnectionState::Connected.byte(),
            ready_state: ReadyState::Ready.byte(),
            battery: 80,
        }
    );

    link.hand("omni", not_armed("Square Golf Omni", Source::Measured));
    link.settle();
    assert_eq!(
        link.written(),
        [Message::ReadyState(ReadyState::NotReady.byte())]
    );
    assert_eq!(
        link.ping(),
        Message::Heartbeat {
            connection_state: ConnectionState::Connecting.byte(),
            ready_state: ReadyState::NotReady.byte(),
            battery: codec::BATTERY_UNKNOWN,
        }
    );
}

/// A Ping that comes before the HelloAck is answered.
#[test]
fn a_ping_before_the_hello_ack_is_answered() {
    let mut link = Link::new();
    link.hand("omni", monitor("Omni", Source::Measured, true, None));
    assert!(matches!(link.written()[..], [Message::Hello { .. }]));
    link.send(&Message::Ping);
    link.settle();
    assert_eq!(
        link.written(),
        [Message::Heartbeat {
            connection_state: ConnectionState::Connected.byte(),
            ready_state: ReadyState::Ready.byte(),
            battery: codec::BATTERY_UNKNOWN,
        }]
    );
    link.send(&Message::HelloAck {
        protocol_version: 1,
        host_caps: 0,
    });
    link.settle();
    assert_eq!(
        link.written(),
        [Message::ReadyState(ReadyState::Ready.byte())]
    );
}

/// No HelloAck within the timeout ends the link with the reason; the next
/// link's handshake takes the reason off the card.
#[test]
fn a_host_that_never_answers_hello_is_an_error_until_one_does() {
    let mut link = Link::new();
    link.host.advance(HELLO_TIMEOUT - Duration::from_millis(1));
    link.step();
    link.host.advance(Duration::from_millis(1));
    let Err(Error::Handshake(reason)) = link.try_step() else {
        panic!("the handshake times out");
    };
    assert!(reason.contains("no reply to Hello"), "{reason}");

    let mut next = Link::linked();
    let rows = telemetry(&next.events());
    assert_eq!(rows["error"], "", "the reason leaves the card");
    assert_eq!(rows[STRICT_MODE_KEY], "off");
}

/// A HelloAck choosing a version flighthook does not speak ends the link
/// with the reason.
#[test]
fn an_unsupported_version_ends_the_link() {
    let mut link = Link::new();
    link.send(&Message::HelloAck {
        protocol_version: 2,
        host_caps: 0,
    });
    let Err(Error::Handshake(reason)) = link.try_step() else {
        panic!("the link ends");
    };
    assert!(reason.contains("version 2"), "{reason}");
}

/// The link's rows once linked: strict mode off and the ready state; the
/// address is the device's label (the card's `device_info`).
#[test]
fn the_card_shows_the_address_and_the_link() {
    let mut link = Link::linked();
    let rows = telemetry(&link.events());
    assert_eq!(rows[STRICT_MODE_KEY], "off");
    assert_eq!(rows["ready_state"], "not ready");
    assert_eq!(link.session.identity.label(), "127.0.0.1:9760");
}

// -- The state Pureplay is told ------------------------------------------------

/// The ready state follows the monitor: not armed, armed but not ready,
/// ready, processing a shot (until it finishes or for at most the
/// processing limit), and back.
#[test]
fn ready_state_follows_the_monitor_through_a_shot() {
    let mut link = Link::linked();
    let now = link.host.now();
    assert_eq!(link.session.ready_state(now), ReadyState::NotReady);
    assert_eq!(
        link.session.connection_state(),
        ConnectionState::Disconnected
    );

    link.hand("omni", not_armed("Omni", Source::Measured));
    assert_eq!(link.session.connection_state(), ConnectionState::Connecting);
    link.hand("omni", monitor("Omni", Source::Measured, false, None));
    assert_eq!(link.session.connection_state(), ConnectionState::Connected);
    assert_eq!(link.session.ready_state(now), ReadyState::NotReady);
    link.hand("omni", monitor("Omni", Source::Measured, true, None));
    assert_eq!(link.session.ready_state(now), ReadyState::Ready);

    link.hand("omni", ShotTrigger::new(key_of("omni", 1)));
    assert_eq!(link.session.ready_state(now), ReadyState::Processing);
    assert_eq!(
        link.session.ready_state(now + PROCESSING_LIMIT),
        ReadyState::Ready,
        "a shot that never finishes stops counting"
    );
    link.hand(
        "omni",
        ShotFinished {
            key: key_of("omni", 1),
        },
    );
    assert_eq!(link.session.ready_state(now), ReadyState::Ready);

    link.hand("omni", Removal::of(StateKind::BusActor));
    assert_eq!(link.session.ready_state(now), ReadyState::NotReady);
    assert_eq!(
        link.session.connection_state(),
        ConnectionState::Disconnected
    );

    // A shot started out of view does not count once the monitor is back.
    link.hand("omni", ShotTrigger::new(key_of("omni", 2)));
    link.hand("omni", monitor("Omni", Source::Measured, true, None));
    assert_eq!(link.session.ready_state(now), ReadyState::Ready);
}

/// With several monitors in view the best state counts, and no monitor
/// stands for the device; with one, its battery and name do.
#[test]
fn any_routing_takes_the_best_monitor_and_names_a_lone_one() {
    let mut link = Link::linked();
    link.hand(
        "omni",
        monitor("Square Golf Omni", Source::Measured, true, Some(90)),
    );
    assert_eq!(link.session.battery(), Some(90));
    let Message::Hello {
        device_caps,
        device_name,
        ..
    } = link.session.hello()
    else {
        panic!("hello");
    };
    assert_eq!(device_caps, codec::CAP_BATTERY_POWERED);
    assert_eq!(device_name, "flighthook (Square Golf Omni)");

    link.hand("mevo", not_armed("Mevo", Source::Measured));
    let Message::Hello {
        device_caps,
        device_name,
        ..
    } = link.session.hello()
    else {
        panic!("hello");
    };
    assert_eq!((device_caps, device_name.as_str()), (0, "flighthook"));
    assert_eq!(link.session.battery(), None);
    assert_eq!(link.session.ready_state(link.host.now()), ReadyState::Ready);
    assert_eq!(link.session.connection_state(), ConnectionState::Connected);
}

/// A monitor that never reported a battery gives no battery capability.
#[test]
fn no_battery_report_means_no_battery_capability() {
    let mut link = Link::new();
    link.hand("omni", monitor("Omni", Source::Measured, true, None));
    let Message::Hello { device_caps, .. } = link.session.hello() else {
        panic!("hello");
    };
    assert_eq!(device_caps, 0);
}

/// In strict mode the ready state and the connection state count only
/// measured monitors.
#[test]
fn readiness_counts_only_measured_monitors_in_strict_mode() {
    let mut link = Link::linked();
    link.hand("mock", monitor("Mock", Source::Virtual, true, None));
    let now = link.host.now();
    assert_eq!(link.session.ready_state(now), ReadyState::Ready);
    link.strict(true);
    assert_eq!(link.session.ready_state(now), ReadyState::NotReady);
    assert_eq!(
        link.session.connection_state(),
        ConnectionState::Disconnected,
        "no measured monitor in view"
    );
    link.hand("omni", monitor("Omni", Source::Measured, false, None));
    assert_eq!(link.session.ready_state(now), ReadyState::NotReady);
    assert_eq!(link.session.connection_state(), ConnectionState::Connected);
    link.hand("omni", monitor("Omni", Source::Measured, true, None));
    assert_eq!(link.session.ready_state(now), ReadyState::Ready);
}

/// On the wire: a virtual monitor that turns up ready in strict mode leaves
/// the ready state and the heartbeat at the Omni's (not ready); once strict
/// mode ends it counts again.
#[test]
fn strict_mode_reports_readiness_from_measured_monitors_only() {
    let mut link = Link::new();
    link.hand("omni", monitor("Omni", Source::Measured, false, None));
    link.shake();
    assert_eq!(
        link.written(),
        [Message::ReadyState(ReadyState::NotReady.byte())]
    );
    link.strict(true);
    link.hand("mock", monitor("Mock", Source::Virtual, true, None));
    link.settle();
    assert_eq!(
        link.ping(),
        Message::Heartbeat {
            connection_state: ConnectionState::Connected.byte(),
            ready_state: ReadyState::NotReady.byte(),
            battery: codec::BATTERY_UNKNOWN,
        }
    );
    link.send(&Message::SetStrictMode { strict: 0 });
    link.settle();
    assert!(
        link.written()
            .contains(&Message::ReadyState(ReadyState::Ready.byte()))
    );
}

// -- Shots ---------------------------------------------------------------------

/// A shot with ball and club data goes out as BallData then ClubData with
/// the same ShotId; a ball-only shot as BallData alone with the next. Each
/// BallData goes to the host as a raw frame with the shot's key.
#[test]
fn shots_go_out_as_ball_then_club_data() {
    let mut link = Link::linked();
    link.written();
    link.events();
    link.shot("omni", 30, &full_shot());
    let written = link.written();
    let [Message::BallData(ball), Message::ClubData(club)] = &written[..] else {
        panic!("{written:?}");
    };
    assert_eq!(
        (ball.shot_id, ball.valid, ball.ball_speed_ms),
        (1, 0x7F, 64.0)
    );
    assert_eq!(ball.hla, -1.5);
    assert_eq!((club.shot_id, club.valid), (1, 0xFF));
    assert_eq!(club.impact_point_x_mm, 5.0);
    let keyed: Vec<_> = link
        .events()
        .into_iter()
        .filter_map(|e| match e {
            DeviceEvent::Frp(FrpBody::ShotRaw(f)) if f.key.is_some() => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(keyed.len(), 1, "only the BallData is keyed");
    assert_eq!(keyed[0].key, Some(key_of("omni", 30)));
    assert_eq!(keyed[0].dir, RawDir::Tx);
    assert_eq!(keyed[0].label.as_deref(), Some("BallData"));
    assert_eq!(keyed[0].data, Message::BallData(*ball).to_frame());

    link.shot("omni", 31, &ball_only_shot());
    let written = link.written();
    let [Message::BallData(ball)] = &written[..] else {
        panic!("{written:?}");
    };
    assert_eq!(ball.shot_id, 2);
    assert_eq!(
        ball.valid & BallData::SPIN,
        BallData::BACK_SPIN | BallData::SIDE_SPIN
    );
}

/// A shot that finished before the handshake is dropped.
#[test]
fn shots_finished_before_the_handshake_are_dropped() {
    let mut link = Link::new();
    link.shot("omni", 1, &full_shot());
    link.shake();
    assert!(link.balls().is_empty(), "not linked when it finished");

    link.shot("omni", 2, &ball_only_shot());
    assert_eq!(link.balls()[0].shot_id, 1);
}

/// A missed read (a shot that finished with no data) takes no ShotId.
#[test]
fn a_no_read_is_not_sent() {
    let mut link = Link::linked();
    link.shot("omni", 1, &OutputShot::default());
    link.shot("omni", 2, &full_shot());
    let balls = link.balls();
    assert_eq!(balls.len(), 1);
    assert_eq!(balls[0].shot_id, 1);
}

/// A new link starts with strict mode off and ShotId 1, and repeats the
/// handshake.
#[test]
fn a_reconnect_resets_strict_mode_and_the_shot_id() {
    let mut link = Link::linked();
    link.strict(true);
    link.hand("r10", monitor("R10", Source::Measured, true, None));
    link.shot("r10", 1, &ball_only_shot());
    link.shot("r10", 2, &ball_only_shot());
    assert_eq!(link.balls().last().map(|b| b.shot_id), Some(2));

    let mut next = Link::linked();
    next.hand("r10", monitor("R10", Source::Measured, true, None));
    next.shot("r10", 3, &estimated_spin_shot());
    let ball = next.balls().remove(0);
    assert_eq!(ball.shot_id, 1, "ShotId starts again");
    assert_eq!(
        ball.valid & BallData::BACK_SPIN,
        BallData::BACK_SPIN,
        "strict mode is off on the new link"
    );
}

// -- Strict mode ---------------------------------------------------------------

/// Strict mode: an estimated spin goes out with its spin bits clear, and
/// a virtual monitor's shot (Auto-chip) is not sent and takes no ShotId.
/// Off again, both go out as before.
#[test]
fn strict_mode_clears_estimated_spin_and_suppresses_auto_chip() {
    let mut link = Link::linked();
    link.hand("omni", monitor("Omni", Source::Measured, true, None));
    link.hand("chip", monitor("Auto-chip", Source::Virtual, true, None));
    link.strict(true);
    assert_eq!(telemetry(&link.events())[STRICT_MODE_KEY], "on");

    link.shot("omni", 1, &estimated_spin_shot());
    let ball = link.balls().remove(0);
    assert_eq!(ball.shot_id, 1);
    assert_eq!(
        ball.valid,
        BallData::BALL_SPEED | BallData::VLA | BallData::HLA
    );
    assert_eq!(ball.back_spin_rpm, 0.0);

    link.shot("chip", 1, &estimated_spin_shot());
    link.shot("omni", 2, &ball_only_shot());
    let balls = link.balls();
    assert_eq!(balls.len(), 1);
    assert_eq!(balls[0].shot_id, 2, "the chip took no ShotId");

    link.strict(false);
    link.shot("chip", 2, &estimated_spin_shot());
    let ball = link.balls().remove(0);
    assert_eq!(ball.shot_id, 3, "Auto-chip goes out with strict mode off");
    assert_eq!(ball.valid & BallData::BACK_SPIN, BallData::BACK_SPIN);
    assert_eq!(ball.back_spin_rpm, 2000.0);
}

/// In strict mode only measured sources' shots go out: a virtual monitor
/// and a source that declares none take no ShotId.
#[test]
fn strict_mode_sends_measured_sources_only() {
    let mut link = Link::linked();
    link.hand("mock", monitor("Mock", Source::Virtual, true, None));
    link.hand(
        "input",
        monitor("Open Connect", Source::Measured, true, None),
    );
    link.hand(
        "unknown",
        BusActor::new("Unknown", 2.0).with_role(roles::LAUNCH_MONITOR),
    );
    link.strict(true);
    link.shot("mock", 1, &ball_only_shot());
    link.shot("unknown", 1, &ball_only_shot());
    link.shot("input", 1, &ball_only_shot());
    let balls = link.balls();
    assert_eq!(balls.len(), 1);
    assert_eq!(
        balls[0].shot_id, 1,
        "the measured input's shot is the first sent"
    );
    assert!((balls[0].ball_speed_ms - 44.704).abs() < 1e-3);
}

/// A virtual monitor that turns up during strict mode has its shots kept
/// back while a measured one's go; once strict mode ends they go out, and
/// the next request is accepted all the same.
#[test]
fn a_virtual_monitor_that_turns_up_during_strict_mode_is_not_sent() {
    let mut link = Link::linked();
    link.hand("omni", monitor("Omni", Source::Measured, true, None));
    link.strict(true);
    link.hand("mock", monitor("Mock", Source::Virtual, true, None));
    link.shot("mock", 1, &ball_only_shot());
    link.shot("mock", 2, &ball_only_shot());
    link.shot("omni", 1, &full_shot());
    let balls = link.balls();
    assert_eq!(balls.len(), 1);
    assert_eq!((balls[0].shot_id, balls[0].ball_speed_ms), (1, 64.0));

    link.strict(false);
    link.shot("mock", 3, &ball_only_shot());
    assert_eq!(link.balls()[0].shot_id, 2, "sent with strict mode off");
    link.strict(true);
}

/// A virtual monitor in view: strict mode is accepted all the same, with
/// no alert, and its shots stay off the link while it is on.
#[test]
fn strict_mode_is_accepted_with_a_virtual_monitor_routed() {
    let mut link = Link::linked();
    link.hand("mock", monitor("Mock", Source::Virtual, true, None));
    link.hand("omni", monitor("Omni", Source::Measured, true, None));
    link.strict(true);
    link.shot("mock", 1, &ball_only_shot());
    link.shot("omni", 1, &full_shot());
    let balls = link.balls();
    assert_eq!(balls.len(), 1);
    assert_eq!((balls[0].shot_id, balls[0].ball_speed_ms), (1, 64.0));
    assert!(alerts(&link.events()).is_empty());
}

/// On the wire: a shot whose back and side spin are a model's (assumed in
/// the launch monitor's flight result) goes out with total and axis only in
/// strict mode, and with all four outside it.
#[test]
fn strict_mode_withholds_assumed_spin_on_the_wire() {
    let mut link = Link::linked();
    link.hand("r10", monitor("R10", Source::Measured, true, None));
    let mut shot = full_shot();
    let launch = shot.launch.as_mut().expect("launch");
    let assumed = AssumedLaunch {
        backspin_rpm: launch.backspin_rpm.take(),
        sidespin_rpm: launch.sidespin_rpm.take(),
        ..AssumedLaunch::default()
    };
    let mut flight = shot
        .flight
        .take()
        .unwrap_or_else(|| FlightResult::new(Basis::Tracked));
    flight.assumed = Some(assumed);
    shot.flight = Some(flight);

    link.shot("r10", 1, &shot);
    let ball = link.balls().remove(0);
    assert_eq!(ball.valid & BallData::SPIN, BallData::SPIN, "strict off");

    link.strict(true);
    link.shot("r10", 2, &shot);
    let ball = link.balls().remove(0);
    assert_eq!(
        ball.valid & BallData::SPIN,
        BallData::TOTAL_SPIN | BallData::SPIN_AXIS
    );
    assert_eq!((ball.back_spin_rpm, ball.side_spin_rpm), (0.0, 0.0));
    assert_eq!(ball.total_spin_rpm, 2468.0);
}

// -- Commands ------------------------------------------------------------------

/// SetClubInfo makes the session send a complete `golfer.select`: the hand,
/// the club and the mode they give with the last distance (none yet: full);
/// an unknown club is ignored and the hand still set.
#[test]
fn set_club_info_sets_the_hand_and_club() {
    let mut link = Link::linked();
    link.events();
    link.send(&Message::SetClubInfo {
        handedness: 1,
        club: 23,
    });
    link.settle();
    let events = link.events();
    let [select] = &selects(&events)[..] else {
        panic!("{events:?}");
    };
    assert_eq!(select.handedness, Some(Handedness::Left));
    assert_eq!(select.club, Some(Some(Club::Iron7)));
    assert_eq!(select.mode, Some(Mode::Full));
    let rows = telemetry(&events);
    assert_eq!(rows["club"], Club::Iron7.to_string());
    assert_eq!(rows["handed"], Handedness::Left.to_string());

    link.send(&Message::SetClubInfo {
        handedness: 0,
        club: 200,
    });
    link.settle();
    let select = selects(&link.events()).remove(0);
    assert_eq!(select.handedness, Some(Handedness::Right));
    assert_eq!(select.club, Some(Some(Club::Iron7)));
    link.send(&Message::SetClubInfo {
        handedness: 0,
        club: 0,
    });
    link.settle();
    assert_eq!(
        selects(&link.events()).remove(0).club,
        Some(Some(Club::Driver))
    );
}

/// SetShotContext gives the mode with the club in play: under 30 yd to the
/// target a chip, the putter a putt; the select goes again when the mode
/// changes.
#[test]
fn the_shot_context_sets_the_mode_in_the_select() {
    let mut link = Link::linked();
    link.send(&Message::SetClubInfo {
        handedness: 0,
        club: 23,
    });
    link.settle();
    assert_eq!(selects(&link.events())[0].mode, Some(Mode::Full));
    link.send(&Message::SetShotContext {
        distance_to_target_m: 20.0,
    });
    link.settle();
    let select = selects(&link.events()).remove(0);
    assert_eq!(
        (select.mode, select.club),
        (Some(Mode::Chipping), Some(Some(Club::Iron7)))
    );
    link.send(&Message::SetShotContext {
        distance_to_target_m: 18.0,
    });
    link.settle();
    assert!(selects(&link.events()).is_empty(), "the mode held");
    link.send(&Message::SetClubInfo {
        handedness: 0,
        club: club_code(Club::Putter),
    });
    link.settle();
    assert_eq!(selects(&link.events())[0].mode, Some(Mode::Putting));
}

/// The session holds a mode only while it holds a club: a hand alone, with
/// or without a distance, asserts no mode.
#[test]
fn a_hand_without_a_club_holds_no_mode() {
    let mut link = Link::linked();
    link.send(&Message::SetClubInfo {
        handedness: 1,
        club: 200,
    });
    link.send(&Message::SetShotContext {
        distance_to_target_m: 20.0,
    });
    link.settle();
    let events = link.events();
    let [select] = &selects(&events)[..] else {
        panic!("one select, for the hand: {events:?}");
    };
    assert_eq!((select.club, select.mode), (None, None));
    link.send(&Message::SetClubInfo {
        handedness: 1,
        club: club_code(Club::SandWedge),
    });
    link.settle();
    assert_eq!(
        selects(&link.events())[0].mode,
        Some(Mode::Chipping),
        "the distance held"
    );
}

/// A distance of 0 or less is unknown: the club decides (Full unless the
/// putter), and a later real distance changes the mode.
#[test]
fn a_distance_of_zero_or_less_is_unknown() {
    let mut link = Link::linked();
    link.send(&Message::SetShotContext {
        distance_to_target_m: 0.0,
    });
    link.send(&Message::SetClubInfo {
        handedness: 0,
        club: club_code(Club::SandWedge),
    });
    link.send(&Message::SetShotContext {
        distance_to_target_m: -1.0,
    });
    link.send(&Message::SetShotContext {
        distance_to_target_m: 12.0,
    });
    link.settle();
    let modes: Vec<_> = selects(&link.events()).iter().map(|s| s.mode).collect();
    assert_eq!(modes, [Some(Mode::Full), Some(Mode::Chipping)]);
}

/// Detect, ball type and shot context are taken without a reply; an
/// unknown type, and a device-to-host type, get Error code 0.
#[test]
fn advisory_commands_are_quiet_and_unknown_types_get_an_error() {
    let mut link = Link::linked();
    link.written();
    link.send(&Message::SetShotContext {
        distance_to_target_m: 142.5,
    });
    link.send(&Message::SetDetect { mode: 1 });
    link.send(&Message::SetBallType { ball_type: 1 });
    link.settle();
    assert!(link.written_not_ready().is_empty());

    link.send_bytes(&codec::encode_frame(0xA5, &[1, 2]).expect("frame"));
    link.send(&Message::ReadyState(1));
    link.settle();
    let written = link.written_not_ready();
    let [
        Message::Error {
            code: 0,
            message: unknown,
        },
        Message::Error { code: 0, .. },
    ] = &written[..]
    else {
        panic!("{written:?}");
    };
    assert!(unknown.contains("0xA5"), "{unknown}");
    assert!(matches!(link.ping(), Message::Heartbeat { .. }));
}

/// Bytes that are not a frame are skipped, with or without a frame behind
/// them, and the link carries on.
#[test]
fn garbage_from_the_host_is_skipped() {
    let mut link = Link::linked();
    link.written();
    link.send_bytes(&[0x13; 5000]);
    link.settle();
    assert!(link.written_not_ready().is_empty());
    let mut garbage_then_ping = vec![0x00, 0x50, 0x4C, 0x00, 0x99];
    garbage_then_ping.extend(Message::Ping.to_frame());
    link.send_bytes(&garbage_then_ping);
    link.settle();
    assert!(matches!(
        link.written_not_ready()[..],
        [Message::Heartbeat { .. }]
    ));
}

/// Errors from Pureplay in quick succession raise one alert.
#[test]
fn host_errors_in_a_burst_raise_one_alert() {
    let mut link = Link::linked();
    link.events();
    for _ in 0..3 {
        link.send(&Message::Error {
            code: codec::ERROR_STRICT_MODE_REQUIRED,
            message: "Strict mode required".into(),
        });
    }
    link.settle();
    assert_eq!(
        alerts(&link.events()),
        ["Pureplay error 2 (strict mode required): Strict mode required"]
    );
}

/// A burst counts across links: errors less than the burst gap apart are
/// one burst, alerted once; the next error after a quiet gap is alerted.
#[test]
fn host_errors_are_alerted_once_per_burst() {
    let errors = Live::new(None);
    let error = Message::Error {
        code: 1,
        message: "busy".into(),
    };
    let mut first = Link::with_errors(TestHost::new(), errors.clone());
    first.send(&error);
    first.settle();
    assert_eq!(alerts(&first.events()).len(), 1);
    for gap in [3, 27, HOST_ERROR_BURST_GAP.as_secs() + 1] {
        first.host.advance(Duration::from_secs(gap));
        // A new link of the same driver at that time.
        let mut host = TestHost::new();
        host.advance(first.host.now() - host.now());
        let mut link = Link::with_errors(host, errors.clone());
        link.send(&error);
        link.settle();
        let alerted = alerts(&link.events()).len();
        assert_eq!(alerted, usize::from(gap > 30), "after {gap} s");
    }
}

// -- A real socket ---------------------------------------------------------------

/// Over TCP: the driver connects, the session says Hello, takes HelloAck,
/// pushes its ready state and writes a shot it is handed.
#[test]
fn a_real_link_shakes_hands_and_sends_a_shot() {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mut driver = Connect::new(Config {
        address: Some(addr.to_string()),
    })
    .expect("driver");
    let mut host = TestHost::new();
    let mut session = host.connect(&mut driver).expect("connects");
    let (mut peer, _) = listener.accept().expect("the driver connects");
    peer.set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    let mut reader = FrameReader::new();
    let mut next = |peer: &mut std::net::TcpStream| loop {
        if let Some(f) = reader.next_frame() {
            return Message::decode(f.kind, &f.payload).expect("decodes");
        }
        let mut buf = [0u8; 512];
        let n = peer.read(&mut buf).expect("the device writes");
        assert!(n > 0, "the device closed the link");
        reader.push(&buf[..n]);
    };

    host.with(|now, out| session.step(now, out)).expect("steps");
    assert!(matches!(next(&mut peer), Message::Hello { .. }));
    peer.write_all(
        &Message::HelloAck {
            protocol_version: 1,
            host_caps: 0,
        }
        .to_frame(),
    )
    .expect("write");
    // The ack arrives in some step; each step reads what has arrived.
    while !session.handshaken {
        host.with(|now, out| session.step(now, out)).expect("steps");
    }
    assert_eq!(
        next(&mut peer),
        Message::ReadyState(ReadyState::NotReady.byte())
    );

    let key = key_of("omni", 1);
    for body in [
        FrpBody::BusActor(monitor("Omni", Source::Measured, true, None)),
        FrpBody::ShotTrigger(ShotTrigger::new(key.clone())),
        FrpBody::ShotLaunch(ShotLaunch {
            key: key.clone(),
            launch: ball_only_shot().launch.expect("launch"),
        }),
        FrpBody::ShotFinished(ShotFinished { key }),
    ] {
        host.bus(&mut session, &FrpMessage::new("omni", body))
            .expect("takes it");
    }
    host.with(|now, out| session.step(now, out)).expect("steps");
    let Message::BallData(ball) = next(&mut peer) else {
        panic!("BallData");
    };
    assert_eq!(ball.shot_id, 1);
}

/// The `strict_mode` row's key.
const STRICT_MODE_KEY: &str = "strict_mode";
