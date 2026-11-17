//! Session tests on the SDK's test host: one thread, virtual time, an
//! in-memory connection to a scripted monitor; a few over real sockets.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::rc::Rc;
use std::time::Duration;

use flightrelay::driver::{Activity, DeviceEvent, FrpDriver, FrpSession, StatusChange};
use flightrelay::testing::TestHost;
use flightrelay::{Club, FrpBody, FrpMessage, Handedness, ShotAggregator, ShotEvent};

use super::*;
use crate::client::api::GsProMessage;
use crate::client::{OutputShot, output_shot};
use crate::server::DEFAULT_BIND;

// -- The connection ------------------------------------------------------------

/// Both ends of an in-memory connection: what the monitor sent, and what the
/// server wrote, one entry per write.
#[derive(Default)]
struct Pipe {
    to_server: VecDeque<u8>,
    from_server: Vec<Vec<u8>>,
    closed: bool,
}

/// The server's end: reads what the monitor sent (would block when there is
/// nothing), writes into the pipe.
#[derive(Clone, Default)]
struct Wire(Rc<RefCell<Pipe>>);

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut pipe = self.0.borrow_mut();
        if pipe.to_server.is_empty() {
            return if pipe.closed {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let n = buf.len().min(pipe.to_server.len());
        for (slot, byte) in buf.iter_mut().zip(pipe.to_server.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().from_server.push(buf.to_vec());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const HEARTBEAT: &str = r#"{"DeviceID":"Range Bridge","ShotNumber":0,"APIversion":"1","ShotDataOptions":{"IsHeartBeat":true}}"#;
const READY: &str = r#"{"DeviceID":"Range Bridge","ShotNumber":0,"APIversion":"1","ShotDataOptions":{"IsHeartBeat":true,"LaunchMonitorIsReady":true,"LaunchMonitorBallDetected":true}}"#;
const OK_HEARTBEAT: &str = r#"{"Code":200,"Message":"Heartbeat received"}"#;
const MONITOR_SHOT: &str = r#"{"DeviceID":"Range Bridge","Units":"Yards","ShotNumber":1,"APIversion":"1","BallData":{"Speed":100.0},"ShotDataOptions":{"ContainsBallData":true}}"#;
const OK_SHOT: &str = r#"{"Code":200,"Message":"Shot received"}"#;
const SHOT: &str = r#"{"DeviceID":"Range Bridge","Units":"Yards","ShotNumber":1,"APIversion":"1","BallData":{"Speed":100.0,"VLA":14.0,"HLA":1.0,"TotalSpin":3000.0,"SpinAxis":2.0},"ShotDataOptions":{"ContainsBallData":true}}"#;

/// A shot with every lateral field set, as components and as a total and
/// axis.
const LATERAL_SHOTS: [&str; 2] = [
    r#"{"DeviceID":"TestLM","Units":"Yards","ShotNumber":7,"APIversion":"1","BallData":{"Speed":120.0,"SpinAxis":-13.05,"TotalSpin":2878.5,"BackSpin":2800.0,"SideSpin":-650.0,"HLA":4.2,"VLA":14.0},"ClubData":{"Speed":90.0,"FaceToTarget":2.1,"Path":-3.4,"HorizontalFaceImpact":-6.5,"VerticalFaceImpact":3.0},"ShotDataOptions":{"ContainsBallData":true,"ContainsClubData":true}}"#,
    r#"{"DeviceID":"TestLM","Units":"Yards","ShotNumber":8,"APIversion":"1","BallData":{"Speed":120.0,"SpinAxis":-30.0,"TotalSpin":5000.0,"HLA":-2.5,"VLA":14.0},"ClubData":{"Speed":90.0,"FaceToTarget":-1.5,"Path":2.0},"ShotDataOptions":{"ContainsBallData":true,"ContainsClubData":true}}"#,
];

/// A peer for an in-memory connection: an address of its own in a range no
/// real connection uses, so tests that register it as a client of this
/// process never meet each other.
fn new_peer() -> SocketAddr {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(1);
    let port = NEXT.fetch_add(1, Ordering::Relaxed);
    SocketAddr::from(([10, 99, 0, 1], port))
}

/// A server on a socket the test has bound already, so its address is
/// known before the first connect.
fn server_on_bound_socket() -> (Server, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("non-blocking");
    let addr = listener.local_addr().expect("addr");
    let mut server = Server::new(Config {
        bind: Some(addr.to_string()),
    })
    .expect("server");
    server.listener = Some(listener);
    (server, addr)
}

/// One connection under test: the test host, the session over a [`Wire`],
/// and the monitor's end.
struct Link {
    host: TestHost,
    session: Session<Wire>,
    wire: Wire,
    peer: SocketAddr,
}

impl Link {
    fn new() -> Self {
        let host = TestHost::new();
        host.flags.set_wants_raw(true);
        let wire = Wire::default();
        let peer = new_peer();
        let session = Session::new(
            wire.clone(),
            peer,
            DeviceIdentity::new(peer.to_string()).with_model("Range Bridge"),
            Vec::new(),
            Live::default(),
            "no".into(),
            ShotIds::from_seed([1, 2]),
        );
        Self {
            host,
            session,
            wire,
            peer,
        }
    }

    fn try_step(&mut self) -> Result<Activity, Error> {
        let Self { host, session, .. } = self;
        host.with(|now, out| session.step_at(now, out))
    }

    /// Step until the session idles.
    fn settle(&mut self) {
        while self.try_step().expect("the connection holds") == Activity::Active {}
    }

    /// The monitor sends `text`.
    fn send(&mut self, text: &str) {
        self.wire.0.borrow_mut().to_server.extend(text.as_bytes());
    }

    /// The monitor sends `text`; everything the server answers within
    /// [`PLAYER_INFO_GAP`] and just after, one entry per write.
    fn exchange(&mut self, text: &str) -> Vec<String> {
        self.send(text);
        self.settle();
        self.host.advance(PLAYER_INFO_GAP);
        self.settle();
        self.written()
    }

    /// The turn's hand and club.
    fn hints(&mut self, hand: Option<Handedness>, club: Option<Club>) {
        let mut hints = TurnHints::default();
        if let Some(hand) = hand {
            hints = hints.with_handedness(hand);
        }
        if let Some(club) = club {
            hints = hints.with_club(club);
        }
        self.session.set_hints(&hints);
    }

    /// What the server wrote since the last call, one entry per write.
    fn written(&mut self) -> Vec<String> {
        std::mem::take(&mut self.wire.0.borrow_mut().from_server)
            .into_iter()
            .map(|w| String::from_utf8(w).expect("utf-8"))
            .collect()
    }

    /// The events since the last call.
    fn events(&mut self) -> Vec<DeviceEvent> {
        self.host.take()
    }
}

/// The 201 for `hand` and `club`.
fn player_info(hand: Option<&str>, club: Option<&str>) -> String {
    let mut fields = Vec::new();
    if let Some(h) = hand {
        fields.push(format!(r#""Handed":"{h}""#));
    }
    if let Some(c) = club {
        fields.push(format!(r#""Club":"{c}""#));
    }
    format!(
        r#"{{"Code":201,"Message":"GSPro Player Information","Player":{{{}}}}}"#,
        fields.join(",")
    )
}

/// The message bodies in `events`, as `FrpMessage`s from `from`.
fn bodies(events: &[DeviceEvent]) -> Vec<FrpBody> {
    events
        .iter()
        .filter_map(|e| match e {
            DeviceEvent::Frp(body) => Some(body.clone()),
            _ => None,
        })
        .collect()
}

/// The kinds of message in `events`, in order, raw frames as
/// `rx:label`/`tx:label`.
fn kinds(events: &[DeviceEvent]) -> Vec<String> {
    bodies(events)
        .iter()
        .map(|b| match b {
            FrpBody::ShotTrigger(_) => "trigger".into(),
            FrpBody::ShotLaunch(_) => "launch".into(),
            FrpBody::ShotFlight(_) => "flight".into(),
            FrpBody::ShotClub(_) => "club".into(),
            FrpBody::ShotImpact(_) => "impact".into(),
            FrpBody::ShotFinished(_) => "finished".into(),
            FrpBody::ShotRaw(f) => format!("{}:{}", f.dir, f.label.clone().unwrap_or_default()),
            FrpBody::BusAlert(a) => format!("alert:{}", a.message),
            _ => "other".into(),
        })
        .collect()
}

/// The shot a message makes, composed from what the session emitted, as the
/// Open Connect client's output shot.
fn ingested(request: &str) -> OutputShot {
    let mut link = Link::new();
    link.send(request);
    link.settle();
    let mut shots = ShotAggregator::new();
    let mut done = None;
    for body in bodies(&link.events()) {
        if let Some(ShotEvent::Finished(shot)) = shots.feed(&FrpMessage::new("range", body)) {
            done = Some(shot);
        }
    }
    let shot = done.expect("a finished shot");
    output_shot(&shot, &shot.from)
}

fn message(text: &str) -> GsProMessage {
    serde_json::from_str(text).expect("parses")
}

// -- Shots -----------------------------------------------------------------------

#[test]
fn shot_finished_carries_request_and_reply() {
    let mut link = Link::new();
    let request = r#"{"DeviceID":"TestLM","Units":"Yards","ShotNumber":4,"APIversion":"1","BallData":{"Speed":100.0},"ShotDataOptions":{"ContainsBallData":true}}"#;
    link.send(request);
    link.settle();
    let events = link.events();
    // The raw frames sit inside the shot, just before `shot.finished`.
    assert_eq!(
        kinds(&events),
        ["trigger", "launch", "rx:request", "tx:reply", "finished"]
    );
    let frames: Vec<_> = bodies(&events)
        .into_iter()
        .filter_map(|b| match b {
            FrpBody::ShotRaw(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(frames[0].data, request.as_bytes());
    assert_eq!(frames[1].data, OK_SHOT.as_bytes());
    let FrpBody::ShotTrigger(trigger) = &bodies(&events)[0] else {
        panic!("a trigger first");
    };
    assert_eq!(trigger.key.shot_number, 4, "the monitor's own number");
    assert_eq!(link.written(), [OK_SHOT]);
}

/// Only a message that is no heartbeat and says it carries ball data is a
/// shot.
#[test]
fn a_shot_is_a_message_with_ball_data() {
    let options = |ball, heartbeat| ShotDataOptions {
        contains_ball_data: ball,
        is_heart_beat: heartbeat,
        ..ShotDataOptions::default()
    };
    assert!(is_shot(&options(true, false)));
    assert!(!is_shot(&options(false, false)), "the ball block is zeros");
    assert!(!is_shot(&options(true, true)));
    assert!(!is_shot(&message(HEARTBEAT).shot_data_options));
}

/// A shot that does not say it carries club data puts no club data and no
/// face impact on the bus, whatever its ClubData block holds; one that does
/// keeps the block's values as the monitor sent them, zeros included (a
/// face impact of 0/0 is no reading, as always).
#[test]
fn club_data_follows_the_message_flag() {
    const NO_CLUB: &str = r#"{"DeviceID":"TestLM","Units":"Yards","ShotNumber":9,"APIversion":"1","BallData":{"Speed":120.0,"BackSpin":2800.0,"SideSpin":-300.0,"HLA":1.0,"VLA":14.0},"ClubData":{"Speed":0.0,"Path":0.0,"AngleOfAttack":0.0,"FaceToTarget":0.0,"Loft":0.0,"HorizontalFaceImpact":0.0,"VerticalFaceImpact":0.0},"ShotDataOptions":{"ContainsBallData":true,"ContainsClubData":false}}"#;
    let shot = ingested(NO_CLUB);
    assert!(shot.launch.is_some());
    assert_eq!((shot.club, shot.impact), (None, None));

    let with_values = NO_CLUB.replace(r#""Speed":0.0,"Path":0.0"#, r#""Speed":90.0,"Path":2.0"#);
    assert_eq!(
        ingested(&with_values).club,
        None,
        "values without the flag are not taken"
    );

    let flagged = NO_CLUB.replace(r#""ContainsClubData":false"#, r#""ContainsClubData":true"#);
    let shot = ingested(&flagged);
    let club = shot.club.expect("club data");
    assert_eq!(
        (
            club.club_speed,
            club.path,
            club.attack_angle,
            club.face_angle
        ),
        (
            Some(flightrelay::Velocity::MilesPerHour(0.0)),
            Some(0.0),
            Some(0.0),
            Some(0.0)
        ),
        "as reported"
    );
    assert_eq!(shot.impact, None);
}

/// The monitor's values reach the bus with the signs it sent, whichever hand
/// the player has: the hand plays no part in what the session emits.
#[test]
fn left_handed_ingest_puts_the_same_signs_on_the_bus() {
    for request in LATERAL_SHOTS {
        let mut rh = Link::new();
        rh.hints(Some(Handedness::Right), Some(Club::Iron7));
        let mut lh = Link::new();
        lh.hints(Some(Handedness::Left), Some(Club::Iron7));
        for link in [&mut rh, &mut lh] {
            link.send(request);
            link.settle();
        }
        let strip = |events: Vec<DeviceEvent>| {
            bodies(&events)
                .into_iter()
                .filter(|b| !matches!(b, FrpBody::ShotTrigger(_) | FrpBody::ShotFinished(_)))
                .filter(|b| !matches!(b, FrpBody::ShotRaw(_)))
                .map(|b| match b {
                    FrpBody::ShotLaunch(m) => format!("{:?}", m.launch),
                    FrpBody::ShotFlight(m) => format!("{:?}", m.flight),
                    FrpBody::ShotClub(m) => format!("{:?}", m.club),
                    FrpBody::ShotImpact(m) => format!("{:?}", m.impact),
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(strip(lh.events()), strip(rh.events()), "{request}");
    }
    let rh = ingested(LATERAL_SHOTS[0]);
    let (ball, club) = (rh.launch.expect("launch"), rh.club.expect("club"));
    assert_eq!(ball.launch_azimuth, Some(4.2));
    assert_eq!(ball.sidespin_rpm, Some(-650));
    assert_eq!(club.face_angle, Some(2.1));
    assert_eq!(club.path, Some(-3.4));
    let ball = ingested(LATERAL_SHOTS[1]).launch.expect("launch");
    assert_eq!(ball.sidespin_rpm, Some(-2500));
    assert_eq!(ball.spin_axis, Some(-30.0));
}

/// The spin pair the monitor did not send is completed from the one it did:
/// the same reading in the other pair, carried in the launch.
#[test]
fn the_completed_spin_pair_is_measured() {
    for request in LATERAL_SHOTS {
        let shot = ingested(request);
        let ball = shot.launch.as_ref().expect("launch");
        assert!(ball.backspin_rpm.is_some() && ball.total_spin_rpm.is_some());
    }
}

/// A GSPro-protocol monitor passed on through flighthook comes out as it
/// went in, for both hands: ingest, then the client's mapping.
#[test]
fn ingest_then_gspro_output_round_trips_for_both_hands() {
    for request in LATERAL_SHOTS {
        let m = message(request);
        for handed in [Handedness::Right, Handedness::Left] {
            let out = crate::client::mapper::map_shot(&ingested(request));
            let eps = 1e-9;
            let at = format!("{handed:?} {request}");
            assert!((out.ball_data.hla - m.ball_data.hla).abs() < eps, "{at}");
            assert!(
                (out.club_data.face_to_target - m.club_data.face_to_target).abs() < eps,
                "{at}"
            );
            assert!((out.club_data.path - m.club_data.path).abs() < eps, "{at}");
            assert!(
                (out.club_data.horizontal_face_impact - m.club_data.horizontal_face_impact).abs()
                    < eps,
                "{at}"
            );
            if m.ball_data.side_spin != 0.0 {
                assert!(
                    (out.ball_data.side_spin - m.ball_data.side_spin).abs() < eps,
                    "{at}"
                );
            } else {
                // Decomposed from the total and axis, rounded to whole rpm.
                assert!(
                    (out.ball_data.spin_axis - m.ball_data.spin_axis).abs() < 0.02,
                    "{at}"
                );
                assert!(out.ball_data.side_spin < 0.0, "{at}");
            }
        }
    }
}

/// Every message, heartbeat or shot, sets the readiness and ball detection
/// the monitor says; a change is one status change, and a value that did not
/// change is not sent again.
#[test]
fn readiness_and_ball_detection_follow_every_message() {
    let mut link = Link::new();
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
    let events = link.events();
    assert!(events.iter().any(
        |e| matches!(e, DeviceEvent::StatusChange(StatusChange::Blockers(b)) if b == &["not_armed"])
    ));
    assert_eq!(link.exchange(READY), [OK_HEARTBEAT]);
    let events = link.events();
    assert!(events.iter().any(
        |e| matches!(e, DeviceEvent::StatusChange(StatusChange::Blockers(b)) if b.is_empty())
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        DeviceEvent::StatusChange(StatusChange::Telemetry(p))
            if p.get("ball_detected") == Some(&Some("true".into()))
    )));
    assert_eq!(link.exchange(READY), [OK_HEARTBEAT]);
    assert!(
        !link
            .events()
            .iter()
            .any(|e| matches!(e, DeviceEvent::StatusChange(_))),
        "nothing changed"
    );
}

#[test]
fn garbage_is_a_warn_alert_and_the_session_goes_on() {
    let mut link = Link::new();
    link.send("{not json}");
    link.settle();
    let kinds = kinds(&link.events());
    assert_eq!(kinds.len(), 1);
    assert!(
        kinds[0].starts_with("alert:Open Connect parse error"),
        "{kinds:?}"
    );
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
}

// -- Player information ------------------------------------------------------------

#[test]
fn the_monitor_gets_the_hand_and_club_after_its_first_message() {
    let mut link = Link::new();
    link.hints(Some(Handedness::Left), Some(Club::Iron7));
    // Nothing before the monitor has spoken.
    link.settle();
    assert!(link.written().is_empty());
    assert_eq!(
        link.exchange(HEARTBEAT),
        [
            OK_HEARTBEAT.to_string(),
            r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Handed":"LH","Club":"I7"}}"#
                .to_string()
        ]
    );
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
}

#[test]
fn a_change_of_hand_or_club_sends_one_new_201() {
    let mut link = Link::new();
    link.hints(Some(Handedness::Right), Some(Club::Driver));
    assert_eq!(
        link.exchange(HEARTBEAT),
        [
            OK_HEARTBEAT.to_string(),
            player_info(Some("RH"), Some("DR"))
        ]
    );
    // No change, no message, however many passes are made.
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);

    link.hints(Some(Handedness::Left), Some(Club::Driver));
    link.settle();
    assert_eq!(link.written(), [player_info(Some("LH"), Some("DR"))]);
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);

    link.hints(Some(Handedness::Left), Some(Club::Putter));
    link.settle();
    assert_eq!(link.written(), [player_info(Some("LH"), Some("PT"))]);
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);

    // Setting what is already set is no change.
    link.hints(Some(Handedness::Left), Some(Club::Putter));
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
}

/// A hand set in the UI reaches the session in the turn's hints, and the
/// monitor gets a 201 with it.
#[test]
fn a_hand_set_in_the_ui_sends_a_201() {
    let mut link = Link::new();
    link.hints(Some(Handedness::Right), Some(Club::Driver));
    assert_eq!(
        link.exchange(HEARTBEAT),
        [
            OK_HEARTBEAT.to_string(),
            player_info(Some("RH"), Some("DR"))
        ]
    );
    link.hints(Some(Handedness::Left), Some(Club::Driver));
    link.settle();
    assert_eq!(link.written(), [player_info(Some("LH"), Some("DR"))]);
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
}

#[test]
fn a_hand_alone_sends_nothing() {
    let mut link = Link::new();
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
    // GSPro always sends the club with the hand: nothing goes out until the
    // club is known, and then the hand goes with it.
    link.hints(Some(Handedness::Left), None);
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
    link.hints(Some(Handedness::Left), Some(Club::Wood3));
    link.settle();
    assert_eq!(link.written(), [player_info(Some("LH"), Some("W3"))]);
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
}

/// The turn starts right-handed, so a club goes out with that hand; a later
/// hand follows in a new 201.
#[test]
fn a_club_goes_out_with_the_starting_hand_and_a_new_hand_follows() {
    let mut link = Link::new();
    link.hints(Some(Handedness::Right), Some(Club::Driver));
    assert_eq!(
        link.exchange(HEARTBEAT),
        [
            OK_HEARTBEAT.to_string(),
            player_info(Some("RH"), Some("DR"))
        ]
    );
    link.hints(Some(Handedness::Left), Some(Club::Driver));
    link.settle();
    assert_eq!(link.written(), [player_info(Some("LH"), Some("DR"))]);
}

#[test]
fn the_mini_driver_goes_out_as_the_driver() {
    let mut link = Link::new();
    link.hints(Some(Handedness::Right), Some(Club::MiniDriver));
    assert_eq!(
        link.exchange(HEARTBEAT),
        [
            OK_HEARTBEAT.to_string(),
            player_info(Some("RH"), Some("DR"))
        ]
    );
}

/// The 200 and the 201 are writes of their own, [`PLAYER_INFO_GAP`] apart:
/// neither object has a terminator, so a monitor that parses one object per
/// read gets both intact.
#[test]
fn the_200_arrives_alone_in_its_own_read_before_the_201() {
    let mut link = Link::new();
    link.hints(Some(Handedness::Right), Some(Club::Iron7));
    link.send(HEARTBEAT);
    link.settle();
    assert_eq!(link.written(), [OK_HEARTBEAT]);
    link.host
        .advance(PLAYER_INFO_GAP - Duration::from_millis(1));
    link.settle();
    assert!(link.written().is_empty(), "not before the gap has passed");
    link.host.advance(Duration::from_millis(1));
    link.settle();
    assert_eq!(link.written(), [player_info(Some("RH"), Some("I7"))]);

    // A change made later is a write of its own as well.
    link.hints(Some(Handedness::Left), Some(Club::Iron7));
    link.settle();
    assert_eq!(link.written(), [player_info(Some("LH"), Some("I7"))]);
}

#[test]
fn a_first_message_that_is_a_shot_gets_the_200_then_the_201() {
    let mut link = Link::new();
    link.hints(Some(Handedness::Right), Some(Club::Driver));
    assert_eq!(
        link.exchange(MONITOR_SHOT),
        [OK_SHOT.to_string(), player_info(Some("RH"), Some("DR"))]
    );
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
}

/// A shot sent right after a change of hand: the 201 for the change is
/// no part of the shot's raw frames, which are its request and its 200.
#[test]
fn the_201_is_never_part_of_a_shots_raw_frames() {
    let mut link = Link::new();
    link.hints(Some(Handedness::Right), Some(Club::Driver));
    link.exchange(HEARTBEAT);
    link.events();

    link.hints(Some(Handedness::Left), Some(Club::Driver));
    link.send(MONITOR_SHOT);
    link.settle();
    link.host.advance(PLAYER_INFO_GAP);
    link.settle();
    // The 201 is due as soon as the monitor's bytes arrive, so it may come
    // before or after the shot's 200.
    let mut written = link.written();
    written.sort();
    let mut want = [OK_SHOT.to_string(), player_info(Some("LH"), Some("DR"))];
    want.sort();
    assert_eq!(written, want);

    let kinds = kinds(&link.events());
    let first = kinds.iter().position(|k| k == "trigger").expect("a shot");
    let last = kinds
        .iter()
        .position(|k| k == "finished")
        .expect("finished");
    assert_eq!(
        kinds[first..=last],
        ["trigger", "launch", "rx:request", "tx:reply", "finished"]
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "tx:player").count(),
        1,
        "the 201 is a frame of its own, outside the shot"
    );
}

/// A new connection is told the player again, whatever the last one was.
#[test]
fn a_new_connection_is_told_again() {
    for _ in 0..2 {
        let mut link = Link::new();
        link.hints(Some(Handedness::Right), Some(Club::Hybrid4));
        assert_eq!(
            link.exchange(HEARTBEAT),
            [
                OK_HEARTBEAT.to_string(),
                player_info(Some("RH"), Some("H4"))
            ]
        );
    }
}

/// A monitor that connects while the turn already holds a hand and club is
/// told them, though the turn does not change afterwards.
#[test]
fn a_monitor_connecting_under_a_set_turn_is_told_the_player() {
    let (mut server, addr) = server_on_bound_socket();
    let mut host = TestHost::new().with_hints(
        TurnHints::default()
            .with_handedness(Handedness::Right)
            .with_club(Club::Iron7),
    );
    host.flags.set_wants_raw(true);
    let mut monitor = TcpStream::connect(addr).expect("connects");
    monitor.write_all(SHOT.as_bytes()).expect("sends");
    let mut session = host.connect(&mut server).expect("the first message");
    host.with(|now, out| session.step(now, out))
        .expect("the shot");
    host.advance(PLAYER_INFO_GAP);
    host.with(|now, out| session.step(now, out)).expect("steps");
    let kinds = kinds(&host.take());
    assert_eq!(
        kinds.iter().filter(|k| *k == "tx:player").count(),
        1,
        "{kinds:?}"
    );
}

// -- The connection's end ----------------------------------------------------------

/// The monitor closing the connection ends the session with no alert, and
/// the host's next connect is at once; a failed bind is an Error alert and
/// the host's backoff.
#[test]
fn a_monitor_that_leaves_ends_the_session_and_the_next_connect_is_at_once() {
    let mut link = Link::new();
    link.exchange(HEARTBEAT);
    link.wire.0.borrow_mut().closed = true;
    let ended = link.try_step().expect_err("the monitor left");
    assert!(matches!(ended, Error::PeerLeft));

    let mut server = Server::new(Config::default()).expect("server");
    let mut backoff = Backoff::default();
    let mut host = TestHost::new();
    let delay = host.with(|_, out| {
        server.retry_delay(
            &SessionEnd {
                error: Some(&ended),
                connected_for: Some(Duration::from_secs(5)),
                attempts: 0,
            },
            &mut backoff,
            out,
        )
    });
    assert_eq!(delay, Some(Duration::ZERO));
    assert!(kinds(&host.take()).is_empty(), "no alert");
}

#[test]
fn a_bind_that_fails_is_an_error_alert_and_backs_off() {
    let taken = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = taken.local_addr().expect("addr");
    let mut server = Server::new(Config {
        bind: Some(addr.to_string()),
    })
    .expect("server");
    let mut host = TestHost::new();
    let error = host.connect(&mut server).expect_err("the port is taken");
    assert!(matches!(error, Error::Bind { .. }), "{error}");

    let mut backoff = Backoff::default();
    let delays: Vec<_> = (0..3)
        .map(|attempts| {
            host.with(|_, out| {
                server.retry_delay(
                    &SessionEnd {
                        error: Some(&error),
                        connected_for: None,
                        attempts,
                    },
                    &mut backoff,
                    out,
                )
            })
        })
        .collect();
    assert_eq!(delays, [1, 2, 3].map(|s| Some(Duration::from_secs(s))));
    let alerts: Vec<_> = host
        .take()
        .into_iter()
        .filter_map(|e| match e {
            DeviceEvent::Frp(FrpBody::BusAlert(a)) => Some((a.severity, a.message)),
            _ => None,
        })
        .collect();
    assert_eq!(alerts.len(), 3);
    assert_eq!(alerts[0].0, Severity::Error);
    assert_eq!(
        alerts[0].1,
        format!(
            "Open Connect server could not bind {addr}: {}",
            match &error {
                Error::Bind { source, .. } => source.to_string(),
                _ => unreachable!(),
            }
        )
    );
}

#[test]
fn the_bind_must_be_ip_and_port() {
    assert!(Server::new(Config::default()).is_ok());
    assert_eq!(
        Config::default().addr().expect("default"),
        DEFAULT_BIND.parse().expect("addr")
    );
    let bad = Config {
        bind: Some("everywhere".into()),
    };
    assert!(Server::new(bad).is_err());
}

// -- This process's own clients ----------------------------------------------------

/// Refusals of one client less than the burst gap apart are one burst,
/// alerted once; another client's, or one after a quiet gap, is alerted.
#[test]
fn refusals_of_one_client_in_a_burst_are_alerted_once() {
    let mut refusals = Refusals::default();
    let t0 = Instant::now();
    let alerted: Vec<bool> = [0, 1, 3, 6, 36]
        .into_iter()
        .map(|s| refusals.record("oc-a", t0 + Duration::from_secs(s)))
        .collect();
    assert_eq!(alerted, [true, false, false, false, true]);
    assert!(refusals.record("oc-b", t0 + Duration::from_secs(37)));
}

/// A client of this process that connects to the server is told why, its
/// data never reaches the bus, the server serves no one, and the alert names
/// it by its actor id, once.
#[test]
fn a_client_pointed_at_its_own_server_is_refused_and_alerted_once() {
    let (mut server, addr) = server_on_bound_socket();

    // Our own client: it connects, registers, and sends a shot.
    let mut client = TcpStream::connect(addr).expect("connects");
    let registration = local::clients().register(client.local_addr().expect("local"), "oc-actor");
    client.write_all(SHOT.as_bytes()).expect("sends");
    // A second try of the same client, while the first is refused.
    let mut again = TcpStream::connect(addr).expect("connects");
    let again_registration =
        local::clients().register(again.local_addr().expect("local"), "oc-actor");
    again.write_all(SHOT.as_bytes()).expect("sends");

    // Nobody else comes: the connect ends with the host's stop.
    let mut host = TestHost::new().stop_after_ticks(50);
    let error = host.connect(&mut server).expect_err("nothing to serve");
    assert!(matches!(error, Error::Stopped), "{error}");

    let reason = registration.refusal().expect("told why");
    assert_eq!(
        reason,
        format!(
            "Refused by this flighthook's own Open Connect Server on {addr}: this \
             address points at it, which would loop shots back. Use the simulator's \
             port instead."
        )
    );
    assert_eq!(
        again_registration.refusal().as_deref(),
        Some(reason.as_str())
    );
    let events = host.take();
    let kinds = kinds(&events);
    assert_eq!(kinds.len(), 1, "one alert for the burst: {kinds:?}");
    let (start, end) = kinds[0]
        .split_once(" from 127.0.0.1:")
        .expect("the peer address");
    assert_eq!(start, "alert:Refused Open Connect Client \"oc-actor\"");
    assert!(
        end.ends_with(
            ": it is this flighthook's own client, which would loop shots back. \
             Point it at the simulator's port."
        ),
        "{end}"
    );
    // Closed unread.
    let mut chunk = [0u8; 16];
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    assert!(matches!(client.read(&mut chunk), Ok(0) | Err(_)));
}

/// A client registers a moment after it connects: a peer found to be ours
/// after the session has begun is refused before its next read is handled.
#[test]
fn a_client_that_registers_late_is_refused_before_its_data_is_handled() {
    let mut link = Link::new();
    assert_eq!(link.exchange(HEARTBEAT), [OK_HEARTBEAT]);
    link.events();
    let registration = local::clients().register(link.peer, "oc-late");
    link.send(SHOT);
    let error = link.try_step().expect_err("our own client");
    assert!(matches!(error, Error::OwnClient));
    assert_eq!(registration.refusal().as_deref(), Some("no"));
    let kinds = kinds(&link.events());
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert!(kinds[0].starts_with("alert:Refused Open Connect Client \"oc-late\""));
    assert!(link.written().is_empty());
}

// -- Real sockets ------------------------------------------------------------------

/// Over TCP: the first message names the device and is handled in the first
/// step; the monitor leaving ends the session; the listener is kept, so the
/// next monitor is served on the same port.
#[test]
fn a_real_socket_serves_monitors_in_turn_on_one_listener() {
    let (mut server, addr) = server_on_bound_socket();
    let mut host = TestHost::new();
    host.flags.set_wants_raw(true);

    for n in 1..=2 {
        let mut monitor = TcpStream::connect(addr).expect("connects");
        monitor
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("timeout");
        let local = monitor.local_addr().expect("local");
        monitor.write_all(SHOT.as_bytes()).expect("sends");
        let mut session = host.connect(&mut server).expect("the first message");
        assert_eq!(session.identity().model.as_deref(), Some("Range Bridge"));
        assert_eq!(session.identity().device, local.to_string());
        assert_eq!(
            session.identity().label(),
            format!("Range Bridge ({local})")
        );

        host.take();
        host.with(|now, out| session.step(now, out))
            .expect("the shot");
        assert_eq!(
            kinds(&host.take())
                .into_iter()
                .filter(|k| k == "trigger" || k == "finished")
                .count(),
            2,
            "monitor {n}'s shot is handled in the first step"
        );
        let mut reply = [0u8; 64];
        let read = monitor.read(&mut reply).expect("the 200");
        assert_eq!(&reply[..read], OK_SHOT.as_bytes());

        drop(monitor);
        let ended = loop {
            match host.with(|now, out| session.step(now, out)) {
                Ok(_) => {}
                Err(e) => break e,
            }
        };
        assert!(matches!(ended, Error::PeerLeft), "{ended}");
    }
}

/// The monitor, the server, the Open Connect client and a fake simulator on
/// another port: a shot sent to the server comes out of the client at the
/// simulator, and none of it is refused.
#[test]
fn a_chain_through_the_server_to_another_port_works() {
    use crate::client::{Client, Config as ClientConfig};

    let gspro = TcpListener::bind("127.0.0.1:0").expect("bind fake GSPro");
    let (mut server, bind) = server_on_bound_socket();
    let mut client = Client::new(ClientConfig {
        address: Some(gspro.local_addr().expect("addr").to_string()),
        actor: "chain-client".into(),
    })
    .expect("client");
    let mut client_host = TestHost::new();
    let mut server_host = TestHost::new();

    // The client connects to the simulator; its connection is registered as
    // ours and nobody refuses it.
    let mut client_session = client_host.connect(&mut client).expect("client connects");
    let (mut simulator, _) = gspro.accept().expect("client connects to GSPro");
    simulator
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");

    // The monitor sends a shot to the server.
    let mut monitor = TcpStream::connect(bind).expect("monitor connects");
    monitor.write_all(SHOT.as_bytes()).expect("send shot");
    let mut server_session = server_host.connect(&mut server).expect("server serves");
    server_host.take();
    server_host
        .with(|now, out| server_session.step(now, out))
        .expect("the shot");
    let events = server_host.take();
    assert!(
        !kinds(&events).iter().any(|k| k.starts_with("alert")),
        "{events:?}"
    );

    // What the server put on the bus goes to the client, as a host would.
    for body in bodies(&events) {
        if !matches!(body, FrpBody::ShotRaw(_)) {
            client_session.hand(&FrpMessage::new("range-bridge", body));
        }
    }
    client_host
        .with(|now, out| client_session.step(now, out))
        .expect("the client sends");

    // The simulator gets the shot.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let shot = 'found: loop {
        let mut it = serde_json::Deserializer::from_slice(&buf).into_iter::<serde_json::Value>();
        while let Some(Ok(v)) = it.next() {
            if v["ShotDataOptions"]["IsHeartBeat"] == false {
                break 'found v;
            }
        }
        match simulator.read(&mut chunk) {
            Ok(0) => panic!("client closed"),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => panic!("GSPro got no shot: {e}"),
        }
    };
    assert_eq!(shot["BallData"]["Speed"], 100.0);
    assert!(
        client_host
            .take()
            .iter()
            .all(|e| !matches!(e, DeviceEvent::Frp(FrpBody::BusAlert(_))))
    );
}
