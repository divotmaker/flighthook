//! Session tests on the SDK's test host: one thread, virtual time, an
//! in-memory connection to a scripted simulator; one test over a real
//! socket.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::time::Duration;

use flightrelay::driver::{
    Activity, Backoff, DeviceEvent, FrpDriver, FrpSession, Live, SessionEnd,
};
use flightrelay::testing::{TestHost, status};
use flightrelay::{
    BusActor, Club, FrpBody, FrpMessage, GolferSelect, Handedness, RawDir, Removal, ShotFinished,
    ShotKey, ShotLaunch, ShotTrigger, StateKind, roles,
};

use super::*;
use crate::client::mapper::tests::{GOLDEN_RH, square_like_shot};
use crate::client::{Config, DEFAULT_ADDRESS};

// -- The connection ------------------------------------------------------------

/// Both ends of an in-memory connection: what the simulator sent, what the
/// client wrote, and whether the simulator closed it or stopped reading.
#[derive(Default)]
struct Pipe {
    to_client: VecDeque<u8>,
    from_client: Vec<u8>,
    closed: bool,
    /// Every write fails.
    broken: bool,
    /// The client ended its side with a close (a FIN), after reading what
    /// the simulator had sent.
    finished: bool,
}

/// The client's end: reads what the simulator sent (would block when there
/// is nothing), writes into the pipe.
#[derive(Clone, Default)]
struct Wire(Rc<RefCell<Pipe>>);

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut pipe = self.0.borrow_mut();
        if pipe.to_client.is_empty() {
            return if pipe.closed {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let n = buf.len().min(pipe.to_client.len());
        for (slot, byte) in buf.iter_mut().zip(pipe.to_client.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

impl Close for Wire {
    fn close(&mut self) {
        let mut pipe = self.0.borrow_mut();
        pipe.to_client.clear();
        pipe.finished = true;
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut pipe = self.0.borrow_mut();
        if pipe.broken {
            return Err(io::ErrorKind::TimedOut.into());
        }
        pipe.from_client.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One connection under test: the test host, the session over a [`Wire`],
/// and the simulator's end.
struct Link {
    host: TestHost,
    session: Session<Wire>,
    wire: Wire,
}

impl Link {
    /// A connection numbering shots after `last_number`, as a driver's
    /// next connection does.
    fn numbered(last_number: Live<u32>) -> Self {
        let host = TestHost::new();
        let wire = Wire::default();
        let session = Session::new(
            wire.clone(),
            DeviceIdentity::new(DEFAULT_ADDRESS),
            last_number,
            host.now(),
        );
        Self {
            host,
            session,
            wire,
        }
    }

    fn new() -> Self {
        Self::numbered(Live::new(0))
    }

    fn try_step(&mut self) -> Result<Activity, Error> {
        let Self { host, session, .. } = self;
        host.with(|now, out| session.step(now, out))
    }

    /// Step until the session idles.
    fn settle(&mut self) {
        while self.try_step().expect("the connection holds") == Activity::Active {}
    }

    /// Hand the session a message from actor `from`.
    fn hand(&mut self, from: &str, body: impl Into<FrpBody>) {
        let Self { host, session, .. } = self;
        host.bus(session, &FrpMessage::new(from, body))
            .expect("handed");
    }

    /// Hand the session shot `key` of `from`: trigger, the golden ball,
    /// finished.
    fn shot(&mut self, from: &str, key: ShotKey) {
        let launch = square_like_shot().launch.expect("golden ball");
        self.hand(from, ShotTrigger::new(key.clone()));
        self.hand(
            from,
            ShotLaunch {
                key: key.clone(),
                launch,
            },
        );
        self.hand(from, ShotFinished { key });
    }

    /// The simulator sends `text`.
    fn send(&mut self, text: &str) {
        self.wire.0.borrow_mut().to_client.extend(text.as_bytes());
    }

    /// Every object the client wrote since the last call, raw text.
    fn written(&mut self) -> Vec<String> {
        let wire = std::mem::take(&mut self.wire.0.borrow_mut().from_client);
        let mut it = serde_json::Deserializer::from_slice(&wire).into_iter::<serde_json::Value>();
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(value) = it.next() {
            value.expect("valid JSON");
            out.push(String::from_utf8(wire[start..it.byte_offset()].to_vec()).expect("utf-8"));
            start = it.byte_offset();
        }
        out
    }

    /// Step and advance the clock by [`SHOT_GAP`], enough times for every
    /// shot waiting to go out.
    fn flush(&mut self) {
        for _ in 0..8 {
            self.settle();
            self.host.advance(SHOT_GAP);
        }
        self.settle();
    }

    /// The shot objects written since the last call (heartbeats and
    /// readiness messages left out).
    fn shots(&mut self) -> Vec<String> {
        self.written().into_iter().filter(|o| is_shot(o)).collect()
    }

    /// The `ShotNumber`s written since the last call.
    fn numbers(&mut self) -> Vec<u64> {
        self.shots()
            .iter()
            .map(|o| {
                serde_json::from_str::<serde_json::Value>(o).expect("JSON")["ShotNumber"]
                    .as_u64()
                    .expect("a number")
            })
            .collect()
    }

    /// The events since the last call.
    fn events(&mut self) -> Vec<DeviceEvent> {
        self.host.take()
    }
}

fn is_shot(object: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(object).expect("JSON")["ShotDataOptions"]["ContainsBallData"]
        == true
}

fn is_heartbeat(object: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(object).expect("JSON")["ShotDataOptions"]["IsHeartBeat"]
        == true
}

/// The readiness a heartbeat carries.
fn readiness(object: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(object).expect("JSON")["ShotDataOptions"]["LaunchMonitorIsReady"]
        == true
}

fn key(n: u32) -> ShotKey {
    ShotKey::new(format!("shot-{n}"), n)
}

/// The golden JSON for a shot with `ShotNumber` `n`.
fn golden(n: u32) -> String {
    GOLDEN_RH.replace("\"ShotNumber\":12,", &format!("\"ShotNumber\":{n},"))
}

/// A launch monitor's `bus.actor`, ready or not.
fn monitor(ready: bool) -> BusActor {
    let mut actor = BusActor::new("Mevo+", 1.0).with_role(roles::LAUNCH_MONITOR);
    if !ready {
        actor.blockers.push("not_ready".into());
    }
    actor
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

/// The keys of the shot frames the client handed back as sent.
fn sent_keys(events: &[DeviceEvent]) -> Vec<ShotKey> {
    events
        .iter()
        .filter_map(|e| match e {
            DeviceEvent::Frp(FrpBody::ShotRaw(f)) if f.dir == RawDir::Tx => f.key.clone(),
            _ => None,
        })
        .collect()
}

// -- Shots -----------------------------------------------------------------------

#[test]
fn shots_finished_in_one_drain_are_all_queued_in_order() {
    let mut link = Link::new();
    for n in 1..=3 {
        link.shot("mevo.0", key(n));
    }
    link.flush();
    assert_eq!(link.shots(), [golden(1), golden(2), golden(3)]);
}

/// One device's increasing numbers, gaps included, go out unchanged.
#[test]
fn an_increasing_sequence_keeps_its_shot_numbers() {
    let mut link = Link::new();
    for n in [1, 2, 5, 6, 40] {
        link.shot("mevo.0", key(n));
    }
    link.flush();
    assert_eq!(link.numbers(), [1, 2, 5, 6, 40]);
}

/// Device #1, then Auto-chip #1, then device #2 go out as 1, 2, 3: a
/// repeated or lower number is bumped past the last one sent.
#[test]
fn an_auto_chip_between_device_shots_never_repeats_a_shot_number() {
    let mut link = Link::new();
    link.shot("mevo.0", key(1));
    link.shot("autochip", ShotKey::new("chip-1", 1));
    link.shot("mevo.0", key(2));
    link.flush();
    assert_eq!(link.numbers(), [1, 2, 3]);
}

/// A device whose count starts again (it reconnected), with the simulator
/// reconnecting too, still gets increasing numbers: the driver keeps the
/// last number across connections.
#[test]
fn a_device_count_reset_after_a_reconnect_still_increases() {
    let last = Live::new(0);
    let mut first = Link::numbered(last.clone());
    for n in 1..=3 {
        first.shot("mevo.0", key(n));
    }
    first.flush();
    assert_eq!(first.numbers(), [1, 2, 3]);
    let mut second = Link::numbered(last);
    for n in 1..=4 {
        second.shot("mevo.0", ShotKey::new(format!("again-{n}"), n));
    }
    second.flush();
    assert_eq!(second.numbers(), [4, 5, 6, 7]);
}

/// A shot goes out as the golden JSON, byte for byte; the session sees no
/// turn, so the player's hand changes nothing.
#[test]
fn a_shot_goes_out_as_the_golden_json() {
    let mut link = Link::new();
    link.shot("mevo.0", key(1));
    link.settle();
    let shots = link.shots();
    assert_eq!(shots.len(), 1);
    assert_eq!(shots[0].trim_end(), golden(1));
}

/// A missed read (a shot that finished with no data) writes nothing and
/// takes no `ShotNumber`: the next shot goes out with its own number.
#[test]
fn a_no_read_writes_nothing_and_uses_no_shot_number() {
    let mut link = Link::new();
    link.shot("mevo.0", key(12));
    link.hand("mevo.0", ShotTrigger::new(key(13)));
    link.hand("mevo.0", ShotFinished { key: key(13) });
    link.shot("mevo.0", ShotKey::new("shot-13b", 13));
    link.flush();
    assert_eq!(link.shots(), [golden(12), golden(13)]);
}

/// Each shot written is handed back as a raw frame with the key it had on
/// the bus (before renumbering), after the write; other frames carry no key.
#[test]
fn each_written_shot_is_handed_back_with_its_bus_key() {
    let mut link = Link::new();
    link.shot("mevo.0", key(3));
    link.shot("r10.0", ShotKey::new("r10-2", 2));
    link.flush();
    assert_eq!(link.numbers(), [3, 4]);
    assert_eq!(
        sent_keys(&link.events()),
        [key(3), ShotKey::new("r10-2", 2)]
    );
}

/// A write that fails ends the session; the shot it was writing is not
/// handed back as sent, and the shots behind it go with the session.
#[test]
fn a_failed_write_takes_its_record_back() {
    let mut link = Link::new();
    link.settle();
    link.events();
    link.wire.0.borrow_mut().broken = true;
    for n in 1..=3 {
        link.shot("mevo.0", key(n));
    }
    let failed = link.try_step();
    assert!(
        matches!(failed, Err(Error::Io(ref e)) if e.kind() == io::ErrorKind::TimedOut),
        "a failed write ends the session: {failed:?}"
    );
    assert!(sent_keys(&link.events()).is_empty());
}

// -- Heartbeats and readiness ----------------------------------------------------

/// The first heartbeat goes out a second after the session starts, then
/// every 10 s. A readiness change goes out at once as a message of its own:
/// not a heartbeat, no ball or club data, the readiness set; the heartbeats
/// keep their own clock.
#[test]
fn heartbeats_follow_the_clock_and_the_readiness_goes_out_by_itself() {
    let options = |object: &str| -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(object).expect("JSON")["ShotDataOptions"].clone()
    };
    let mut link = Link::new();
    link.settle();
    assert!(link.written().is_empty(), "nothing before the first second");
    link.host.advance(FIRST_HEARTBEAT);
    link.settle();
    let first = link.written();
    assert_eq!(first.len(), 1);
    assert!(is_heartbeat(&first[0]) && !readiness(&first[0]));

    link.hand("mevo.0", monitor(true));
    link.settle();
    let ready = link.written();
    assert_eq!(ready.len(), 1, "at once");
    let o = options(&ready[0]);
    assert_eq!(o["IsHeartBeat"], false);
    assert_eq!(o["ContainsBallData"], false);
    assert_eq!(o["ContainsClubData"], false);
    assert_eq!(o["LaunchMonitorIsReady"], true);

    link.host
        .advance(HEARTBEAT_EVERY - Duration::from_millis(1));
    link.settle();
    assert!(link.written().is_empty());
    link.host.advance(Duration::from_millis(1));
    link.settle();
    let beat = link.written();
    assert_eq!(beat.len(), 1, "10 s after the last heartbeat");
    assert!(is_heartbeat(&beat[0]) && readiness(&beat[0]));

    // Out of view: not ready.
    link.hand("mevo.0", Removal::of(StateKind::BusActor));
    link.settle();
    let gone = link.written();
    assert_eq!(gone.len(), 1);
    assert!(!is_heartbeat(&gone[0]) && !readiness(&gone[0]));
}

/// A shot and a readiness change in the same step go out as two writes,
/// the shot first and the readiness [`SHOT_GAP`] after it.
#[test]
fn a_shot_and_a_readiness_change_in_one_step_go_out_250_ms_apart() {
    let mut link = Link::new();
    link.hand("mevo.0", monitor(true));
    link.shot("mevo.0", key(1));
    link.settle();
    let first = link.written();
    assert_eq!(first.len(), 1, "one message per write");
    assert!(is_shot(&first[0]));
    link.host.advance(SHOT_GAP - Duration::from_millis(1));
    link.settle();
    assert!(link.written().is_empty(), "held after a shot");
    link.host.advance(Duration::from_millis(1));
    link.settle();
    let second = link.written();
    assert_eq!(second.len(), 1);
    assert!(!is_heartbeat(&second[0]) && !is_shot(&second[0]) && readiness(&second[0]));
}

/// Two shots queued go out [`SHOT_GAP`] apart, never in one write.
#[test]
fn queued_shots_go_out_250_ms_apart() {
    let mut link = Link::new();
    link.shot("mevo.0", key(1));
    link.shot("mevo.0", key(2));
    link.settle();
    assert_eq!(link.numbers(), [1]);
    link.host.advance(SHOT_GAP);
    link.settle();
    assert_eq!(link.numbers(), [2]);
}

/// A shot with no `200` after 1 s sets the card's `error` and logs one
/// warning; a later `200` clears the row.
#[test]
fn a_shot_the_simulator_does_not_answer_sets_the_error_until_a_200() {
    let declared = Client::info(&Config::default()).telemetry;
    assert!(declared.iter().any(|(key, _)| *key == "error"));
    let mut link = Link::new();
    link.shot("mevo.0", key(1));
    link.settle();
    link.events();
    link.host.advance(REPLY_WITHIN - Duration::from_millis(1));
    link.settle();
    assert!(!status(&link.events()).telemetry.contains_key("error"));

    link.host.advance(Duration::from_millis(1));
    link.settle();
    let events = link.events();
    assert_eq!(status(&events).telemetry["error"], NOT_READING);
    let warnings = |events: &[DeviceEvent]| {
        events
            .iter()
            .filter(|e| matches!(e, DeviceEvent::Log(Level::Warn, line) if line == NOT_READING))
            .count()
    };
    assert_eq!(warnings(&events), 1);

    link.host.advance(Duration::from_secs(5));
    link.settle();
    assert_eq!(warnings(&link.events()), 0, "logged once");

    link.send(r#"{"Code":200,"Message":"Shot received"}"#);
    link.settle();
    assert_eq!(status(&link.events()).telemetry["error"], "none");
}

/// A shot answered in time never sets the error.
#[test]
fn an_answered_shot_sets_no_error() {
    let mut link = Link::new();
    link.shot("mevo.0", key(1));
    link.settle();
    link.send(r#"{"Code":200,"Message":"Shot received"}"#);
    link.host.advance(REPLY_WITHIN * 2);
    link.settle();
    assert!(!status(&link.events()).telemetry.contains_key("error"));
}

/// A session that ends normally closes its end after reading what the
/// simulator still had to say.
#[test]
fn a_normal_close_reads_what_is_left_and_ends_with_a_fin() {
    let Link {
        mut host,
        session,
        wire,
    } = Link::new();
    wire.0.borrow_mut().to_client.extend(b"{\"Code\":200}");
    host.with(|_, out| session.close(out));
    let pipe = wire.0.borrow();
    assert!(pipe.finished);
    assert!(pipe.to_client.is_empty());
}

/// The session's telemetry: the readiness sent, the simulator's last
/// response, and the club and hand it set.
#[test]
fn the_card_shows_the_connection_and_its_shots() {
    let mut link = Link::new();
    link.hand("mevo.0", monitor(true));
    link.send(
        r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Handed":"RH","Club":"DR"}}"#,
    );
    link.settle();
    let rows = status(&link.events()).telemetry;
    assert_eq!(rows["response"], "201 GSPro Player Information");
    assert_eq!(rows["monitor_ready"], "true");
    assert_eq!(rows["club"], Club::Driver.to_string());
    assert_eq!(rows["handed"], Handedness::Right.to_string());
}

/// Player information becomes one select with the club and hand the
/// session holds: a hand or club left out stays; a hand alone selects no
/// club.
#[test]
fn player_information_becomes_one_select_with_what_is_held() {
    let mut link = Link::new();
    link.send(r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Handed":"LH"}}"#);
    link.settle();
    let [hand] = &selects(&link.events())[..] else {
        panic!("one select");
    };
    assert_eq!((hand.club, hand.handedness), (None, Some(Handedness::Left)));
    link.send(r#"{"Code":201,"Message":"GSPro Player Information","Player":{"Club":"SW"}}"#);
    link.settle();
    let [both] = &selects(&link.events())[..] else {
        panic!("one select");
    };
    assert_eq!(
        (both.club, both.handedness, both.mode),
        (Some(Some(Club::SandWedge)), Some(Handedness::Left), None)
    );
}

/// The simulator closing the connection ends the session.
#[test]
fn a_closed_connection_ends_the_session() {
    let mut link = Link::new();
    link.wire.0.borrow_mut().closed = true;
    assert!(matches!(link.try_step(), Err(Error::Io(_))));
}

// -- The driver ------------------------------------------------------------------

/// Retries back off 1 s, 2 s, 3 s, 3 s, with no alert (a simulator not
/// running is normal); a connection that lasted starts over at 1 s.
#[test]
fn a_lost_connection_is_a_status_not_an_alert() {
    let mut driver = Client::new(Config::default()).expect("driver");
    let mut host = TestHost::new();
    let error = Error::Io(io::ErrorKind::ConnectionRefused.into());
    let mut delays = Vec::new();
    for connected_for in [None, None, None, None, Some(Duration::from_secs(60))] {
        let ended = SessionEnd::new(&error, host.now(), connected_for, 1);
        let delay = host.with(|_, out| driver.retry_delay(&ended, &mut Backoff::default(), out));
        delays.push(delay.expect("retries").as_secs());
    }
    assert_eq!(delays, [1, 2, 3, 3, 1]);
    assert!(host.take().is_empty(), "no alert, no row");
}

#[test]
fn the_address_must_be_ip_and_port() {
    assert!(Client::new(Config::default()).is_ok());
    let bad = Config {
        address: Some("gspro".into()),
        ..Config::default()
    };
    assert!(Client::new(bad).is_err());
}

// -- A real socket ---------------------------------------------------------------

/// Over TCP: the driver connects and registers the connection as this
/// process's own until the session ends; shots handed right after the
/// connect all go out in order, with unchanged JSON.
#[test]
fn shots_right_after_connect_are_all_sent_in_order_with_unchanged_json() {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let mut driver = Client::new(Config {
        address: Some(listener.local_addr().expect("addr").to_string()),
        actor: "oc-test".into(),
    })
    .expect("driver");
    let mut host = TestHost::new();
    let mut session = host.connect(&mut driver).expect("connects");
    let (mut peer, client) = listener.accept().expect("the driver connects");
    assert_eq!(local::client_at(client).as_deref(), Some("oc-test"));
    peer.set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");

    for n in [12, 13, 14] {
        let key = key(n);
        for body in [
            FrpBody::ShotTrigger(ShotTrigger::new(key.clone())),
            FrpBody::ShotLaunch(ShotLaunch {
                key: key.clone(),
                launch: square_like_shot().launch.expect("golden ball"),
            }),
            FrpBody::ShotFinished(ShotFinished { key }),
        ] {
            host.bus(&mut session, &FrpMessage::new("mevo.0", body))
                .expect("takes it");
        }
    }
    for _ in 0..3 {
        host.advance(SHOT_GAP);
        host.with(|now, out| session.step(now, out)).expect("steps");
    }
    let mut shots = Vec::new();
    let mut buf = Vec::new();
    while shots.len() < 3 {
        let mut chunk = [0u8; 4096];
        let n = peer.read(&mut chunk).expect("the client writes");
        assert!(n > 0, "the client closed the connection");
        buf.extend_from_slice(&chunk[..n]);
        let mut it = serde_json::Deserializer::from_slice(&buf).into_iter::<serde_json::Value>();
        let mut start = 0;
        shots.clear();
        while let Some(Ok(_)) = it.next() {
            let text = String::from_utf8(buf[start..it.byte_offset()].to_vec()).expect("utf-8");
            start = it.byte_offset();
            if !is_heartbeat(&text) {
                shots.push(text);
            }
        }
    }
    assert_eq!(shots, [golden(12), golden(13), golden(14)]);
    drop(session);
    assert_eq!(local::client_at(client), None, "released with the session");
}

/// Over TCP: closing a session whose simulator has said something the
/// client has not read still ends with a FIN, which the simulator reads as
/// the end of the stream. (Dropping the connection with that unread would
/// reset it.)
#[test]
fn closing_a_real_connection_ends_it_with_a_fin_not_a_reset() {
    use std::net::{Shutdown, TcpListener};
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let mut driver = Client::new(Config {
        address: Some(listener.local_addr().expect("addr").to_string()),
        actor: "oc-close".into(),
    })
    .expect("driver");
    let mut host = TestHost::new();
    let session = host.connect(&mut driver).expect("connects");
    let (mut peer, _) = listener.accept().expect("the driver connects");
    peer.set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    peer.write_all(br#"{"Code":200,"Message":"late"}"#)
        .expect("the simulator speaks");
    peer.shutdown(Shutdown::Write).expect("the simulator ends");
    host.with(|_, out| session.close(out));
    let mut rest = Vec::new();
    peer.read_to_end(&mut rest)
        .expect("the simulator reads the end of the stream");
    assert!(rest.is_empty());
}
