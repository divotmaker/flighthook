//! The card against a mock Accessory server: `hello` (any
//! `serverInstanceId` and event list), pairing (the queue,
//! `pairing.pending`, `paired`), every refusal as a response then its close
//! code, a refusal without a close frame, `4429` before `hello`, drops
//! without a close frame, malformed and `null` frames, `subscribe` ack then
//! the snapshot then events with `seq`; the server sends binary frames and
//! the card's text frames are read.

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use serde_json::{Value, json};
use tokio::sync::broadcast;
use tungstenite::handshake::server::{Request, Response as HsResponse};
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;

use super::*;
use crate::actors::provenance::ShotSource;
use crate::actors::system::{follow_game_golfers, handle_session, publish_session};
use crate::bus::BusMsg;
use crate::state::SessionWriter;
use flighthook::{ActorStatusResponse, Club, Handedness};
use flightrelay::FrpBody;

/// Longest a test waits for one thing.
const WAIT: Duration = Duration::from_secs(5);
/// How long a test watches for a connection that must not come: many
/// times the test backoff.
const QUIET: Duration = Duration::from_millis(600);

const ACTOR: &str = "pureplay_accessory.0";

mod club;
mod game;

// -- Mock server ---------------------------------------------------------------

/// A WebSocket frame type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameType {
    Text,
    Binary,
}

/// One connection to the mock server.
struct Peer {
    ws: WebSocket<TcpStream>,
    /// The frame type of every message the card sent on it.
    kinds: Vec<FrameType>,
}

impl Peer {
    /// Send `v` in a binary frame, as Pureplay does.
    fn send(&mut self, v: &Value) {
        self.ws
            .send(Message::binary(v.to_string().into_bytes()))
            .expect("send");
    }

    /// Send `v` in a text frame.
    fn send_text(&mut self, v: &Value) {
        self.ws.send(Message::text(v.to_string())).expect("send");
    }

    /// The next message from the card.
    fn recv(&mut self) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "nothing from the card");
            match self.ws.read() {
                Ok(Message::Text(t)) => {
                    self.kinds.push(FrameType::Text);
                    return serde_json::from_str(t.as_str()).expect("json");
                }
                Ok(Message::Binary(b)) => {
                    self.kinds.push(FrameType::Binary);
                    return serde_json::from_slice(&b).expect("json");
                }
                Ok(Message::Close(_)) => panic!("the card closed"),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => panic!("read: {e}"),
            }
        }
    }

    /// Whether the card closes this connection within [`WAIT`].
    fn closed_by_card(&mut self) -> bool {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            match self.ws.read() {
                Ok(Message::Close(_)) => return true,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return true,
            }
        }
        false
    }

    /// Close with `code`.
    fn close(mut self, code: u16) {
        let _ = self.ws.close(Some(CloseFrame {
            code: CloseCode::from(code),
            reason: "".into(),
        }));
        let _ = self.ws.flush();
        // Let the card's reply arrive before the socket goes.
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline && self.ws.read().is_ok() {}
    }

    /// `hello` as Pureplay sends it.
    fn hello(&mut self) {
        self.hello_as("ppaccessory", 1);
    }

    fn hello_as(&mut self, protocol: &str, version: u64) {
        self.hello_with(protocol, version, "s-1", &EVENTS_LISTED);
    }

    /// `hello` from Pureplay instance `instance`.
    fn hello_from(&mut self, instance: &str) {
        self.hello_with("ppaccessory", 1, instance, &EVENTS_LISTED);
    }

    fn hello_with(&mut self, protocol: &str, version: u64, instance: &str, events: &[&str]) {
        self.send(&json!({ "type": "hello", "data": {
            "protocol": protocol, "version": version, "game": "Pureplay",
            "gameVersion": "1.4.2", "paired": false, "connectionId": "c-1",
            "serverInstanceId": instance,
            "capabilities": { "events": events, "commands": ["club.set", "club.next"] }
        }}));
    }

    /// Send a raw WebSocket message: a malformed frame.
    fn send_raw(&mut self, msg: Message) {
        self.ws.send(msg).expect("send");
    }

    /// The card's `pair`: its token (`None` for `null`).
    fn pair(&mut self) -> Option<String> {
        let pair = self.recv();
        assert_eq!(pair["type"], "pair", "the first message is pair: {pair}");
        assert_eq!(pair["data"]["client"], "flighthook");
        assert_eq!(pair["data"]["version"], env!("CARGO_PKG_VERSION"));
        pair["data"]["token"].as_str().map(str::to_owned)
    }

    fn paired(&mut self, token: &str) {
        self.send(&json!({ "type": "paired", "data": {
            "token": token, "clientId": "client-1", "scopes": ["read", "control"] } }));
    }

    /// A refused response to `pair`, echoing its id (or `id: null`).
    fn refuse(&mut self, id: Option<&str>, code: &str, extra: &Value) {
        let mut error = json!({ "code": code, "message": "No." });
        if let (Some(e), Some(x)) = (error.as_object_mut(), extra.as_object()) {
            e.extend(x.clone());
        }
        self.send(&json!({ "type": "response", "id": id, "ok": false, "error": error }));
    }

    /// The card's `subscribe`, acknowledged, then the snapshot at `seq`.
    fn subscribe(&mut self, snapshot: &Value, seq: u64) {
        self.subscribe_to(&protocol::EVENTS, snapshot, seq);
    }

    /// [`subscribe`](Self::subscribe), the card asking for `expected`.
    fn subscribe_to(&mut self, expected: &[&str], snapshot: &Value, seq: u64) {
        let sub = self.recv();
        assert_eq!(sub["type"], "subscribe", "{sub}");
        assert_eq!(sub["data"]["snapshot"], true);
        let events: Vec<&str> = sub["data"]["events"]
            .as_array()
            .expect("events")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(events, expected, "the card's events that Pureplay lists");
        self.send(&json!({ "type": "response", "id": sub["id"], "ok": true, "data": {} }));
        self.send(
            &json!({ "type": "event", "event": "state.snapshot", "seq": seq,
                           "time": "2026-11-10T12:00:00Z", "data": snapshot }),
        );
    }

    fn event(&mut self, name: &str, seq: u64, data: &Value) {
        self.send(&event_json(name, seq, data));
    }
}

fn event_json(name: &str, seq: u64, data: &Value) -> Value {
    json!({ "type": "event", "event": name, "seq": seq, "time": "2026-11-10T12:00:01Z", "data": data })
}

/// What the mock's `hello` lists: every event Pureplay v1 has.
const EVENTS_LISTED: [&str; 26] = [
    "state.snapshot",
    "context.invalidated",
    "match.started",
    "match.ended",
    "match.paused",
    "match.resumed",
    "hole.started",
    "hole.completed",
    "turn.changed",
    "phase.changed",
    "player.joined",
    "player.left",
    "player.changed",
    "player.club_changed",
    "player.aim_changed",
    "player.bag_changed",
    "shot.initialized",
    "shot.launched",
    "shot.completed",
    "shot.cancelled",
    "shot.replayed",
    "shot.ready_changed",
    "lie.changed",
    "ruling.required",
    "ruling.resolved",
    "score.updated",
];

/// A round on hole 7: Joe (local, RH, 7 iron) up, Ann (local, LH, driver)
/// and a remote player.
fn round() -> Value {
    json!({
        "session": { "apiVersion": 1, "matchId": "m-1", "phase": "AwaitShot", "isTeamGame": false,
                     "scoringModel": "Stroke", "matchActive": true, "paused": false },
        "course": { "courseId": "c", "displayName": "Course", "holeCount": 18, "totalPar": 72 },
        "hole": { "number": 7, "par": 4, "strokeIndex": 3, "yards": 410, "pin": [1.0, 2.0, 3.0] },
        "turn": { "activeShooterId": 1, "remainingPlayerIds": [1, 2, 3] },
        "players": [
            { "playerId": 1, "name": "Joe", "handedness": "RH", "isLocal": true,
              "club": "Iron7", "clubId": "b-7i",
              "bag": [ { "clubId": "b-dr", "type": "Driver", "name": "Driver" },
                       { "clubId": "b-7i", "type": "Iron7", "name": "7 Iron" },
                       { "clubId": "b-8i", "type": "Iron8", "name": "8 Iron" } ] },
            { "playerId": 2, "name": "Ann", "handedness": "LH", "isLocal": true,
              "club": "Driver", "clubId": "a-dr",
              "bag": [ { "clubId": "a-dr", "type": "Driver", "name": "Driver" } ] },
            { "playerId": 3, "name": "Remote", "handedness": "Unknown", "isLocal": false,
              "club": "Putter", "clubId": null, "bag": null }
        ],
        "shot": { "shotId": "shot-1", "playerId": 1, "holeNumber": 7, "strokeNumber": 1,
                  "state": "AwaitingLaunch" },
        "readiness": { "shotId": "shot-1", "playerId": 1, "ready": true, "controllable": true,
                       "allowedCommands": ["club.set"], "blockers": [] },
        "lie": null,
        "ruling": null
    })
}

/// A mock Accessory server on a free loopback port.
struct Mock {
    addr: SocketAddr,
    conns: mpsc::Receiver<Peer>,
}

impl Mock {
    // The handshake callback's error type is tungstenite's.
    #[allow(clippy::result_large_err)]
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, conns) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let accepted =
                    tungstenite::accept_hdr(stream, |req: &Request, mut resp: HsResponse| {
                        let asked = req
                            .headers()
                            .get("Sec-WebSocket-Protocol")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        assert_eq!(asked, "binary", "the card asks for the binary subprotocol");
                        resp.headers_mut().insert(
                            "Sec-WebSocket-Protocol",
                            tungstenite::http::HeaderValue::from_static("binary"),
                        );
                        Ok(resp)
                    });
                let Ok(ws) = accepted else { continue };
                ws.get_ref()
                    .set_read_timeout(Some(Duration::from_millis(20)))
                    .expect("timeout");
                if tx
                    .send(Peer {
                        ws,
                        kinds: Vec::new(),
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        Self { addr, conns }
    }

    /// The card's next connection.
    fn next(&self) -> Peer {
        self.conns.recv_timeout(WAIT).expect("the card connects")
    }

    /// Whether the card connects again within [`QUIET`].
    fn reconnects(&self) -> bool {
        self.conns.recv_timeout(QUIET).is_ok()
    }
}

// -- The bus as the tests read it ----------------------------------------------

/// One bus message as the assertions read it: `actor` is the FRP `from` or
/// the local event's card; a `golfer.select` is unpacked into its club and
/// its hand.
#[derive(Debug, Clone)]
struct FlighthookMessage {
    actor: String,
    event: FlighthookEvent,
}

/// The club of a `golfer.select`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClubInfo {
    club: Club,
}

/// The hand of a `golfer.select`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HandInfo {
    handed: Handedness,
}

/// The parts of a bus message the tests look at.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum FlighthookEvent {
    ActorStatus {
        status: ActorStatus,
        telemetry: HashMap<String, String>,
    },
    ClubInfo {
        club_info: ClubInfo,
    },
    HandInfo {
        hand_info: HandInfo,
    },
    /// The card's `bus.actor`: what was its device telemetry.
    BusActor(flightrelay::BusActor),
    ShotTrigger {
        key: ShotKey,
    },
    ShotFinished {
        #[allow(dead_code)] // kept for Debug output in failures
        key: ShotKey,
    },
    /// A `game.stroke`, with its `shot_id`.
    Stroke {
        shot_id: Option<String>,
        stroke: Box<flightrelay::GameStroke>,
    },
    Other,
}

/// `msg` as the tests read it: none, one or (a select with club and hand)
/// two messages.
fn unpack(msg: &BusMsg) -> Vec<FlighthookMessage> {
    let one = |actor: &str, event| {
        vec![FlighthookMessage {
            actor: actor.to_owned(),
            event,
        }]
    };
    match msg {
        BusMsg::Local { card, event } => match event {
            LocalEvent::CardStatus { status, telemetry } => one(
                card,
                FlighthookEvent::ActorStatus {
                    status: *status,
                    telemetry: telemetry.clone(),
                },
            ),
            _ => one(card, FlighthookEvent::Other),
        },
        BusMsg::Frp { msg, .. } => {
            let from = msg.from.as_str();
            match &msg.body {
                FrpBody::GolferSelect(select) => {
                    let mut out = Vec::new();
                    if let Some(handed) = select.handedness {
                        out.extend(one(
                            from,
                            FlighthookEvent::HandInfo {
                                hand_info: HandInfo { handed },
                            },
                        ));
                    }
                    if let Some(Some(club)) = select.club {
                        out.extend(one(
                            from,
                            FlighthookEvent::ClubInfo {
                                club_info: ClubInfo { club },
                            },
                        ));
                    }
                    out
                }
                FrpBody::BusActor(actor) => one(from, FlighthookEvent::BusActor(actor.clone())),
                FrpBody::GameStroke(stroke) => one(
                    from,
                    FlighthookEvent::Stroke {
                        shot_id: stroke.shot_id.clone(),
                        stroke: Box::new(stroke.clone()),
                    },
                ),
                FrpBody::ShotTrigger(t) => {
                    one(from, FlighthookEvent::ShotTrigger { key: t.key.clone() })
                }
                FrpBody::ShotFinished(f) => {
                    one(from, FlighthookEvent::ShotFinished { key: f.key.clone() })
                }
                _ => one(from, FlighthookEvent::Other),
            }
        }
    }
}

/// Whether `e` is a `game.stroke` for the launch monitor's shot `key`.
fn stroke_for(e: &FlighthookEvent, key: &ShotKey) -> bool {
    matches!(e, FlighthookEvent::Stroke { shot_id: Some(id), .. } if *id == key.shot_id)
}

/// Whether a `bus.actor` says the card is ready (no blockers) and names
/// Pureplay's Accessory API as its device.
fn actor_ready(actor: &flightrelay::BusActor) -> bool {
    actor.blockers.is_empty()
}

// -- The card ------------------------------------------------------------------

/// The card's actor on a thread, and what a dashboard would show of it.
struct Card {
    state: TestState,
    rx: broadcast::Receiver<BusMsg>,
    /// The session on the card's bus; it stops with the card.
    session: SessionActor,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The card as the web layer and the UI hold it.
    view: ActorStatusResponse,
    /// Every message the card put on the bus.
    sent: Vec<FlighthookMessage>,
    /// Every bus message as sent, for the checks that read whole messages.
    raw: Vec<BusMsg>,
    /// Unpacked messages not yet returned by `step`.
    queued: std::collections::VecDeque<FlighthookMessage>,
}

/// Removes a test's folder (or file) when dropped, so a failing
/// assertion does not leave it behind.
pub(super) struct Leftover(pub(super) std::path::PathBuf);

impl Drop for Leftover {
    fn drop(&mut self) {
        if std::fs::remove_dir_all(&self.0).is_err() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// A test's state, its config folder removed once the last holder is done
/// with it.
#[derive(Clone)]
pub(super) struct TestState {
    state: Arc<SystemState>,
    /// The session's write side, held by the test's [`SessionActor`].
    writer: Arc<SessionWriter>,
    _folder: Arc<Leftover>,
}

impl std::ops::Deref for TestState {
    type Target = Arc<SystemState>;
    fn deref(&self) -> &Arc<SystemState> {
        &self.state
    }
}

pub(super) fn new_state() -> TestState {
    new_state_at(&config_path())
}

/// A fresh config file path in a folder of its own.
fn config_path() -> std::path::PathBuf {
    std::env::temp_dir()
        .join(format!(
            "flighthook-pp-accessory-test-{}",
            uuid::Uuid::new_v4().simple()
        ))
        .join("config.toml")
}

/// A state with its config at `config`; the config's folder goes with
/// the state.
fn new_state_at(config: &std::path::Path) -> TestState {
    let (state, writer) = SystemState::new(config.to_path_buf());
    TestState {
        state: Arc::new(state),
        writer: Arc::new(writer),
        _folder: Arc::new(Leftover(config.parent().expect("a folder").to_path_buf())),
    }
}

/// The session's part of the system actor on a thread: the real
/// [`publish_session`] at start and [`handle_session`] for every bus
/// message, so each `golfer.select` is answered with `bus.reply` and, when
/// it changed the turn, a `golfer.turn` from flighthook's actor caused by
/// the select's sender.
struct SessionActor {
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SessionActor {
    /// The session of `state` on the bus `tx`.
    fn start(state: &TestState, tx: &broadcast::Sender<BusMsg>) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let id = state.flighthook_id();
        let sender = BusSender::new("system".into(), tx.clone(), Arc::clone(&shutdown))
            .with_frp_id(Some(id));
        let mut receiver = sender.subscribe();
        let writer = Arc::clone(&state.writer);
        let system = Arc::clone(&state.state);
        let thread = std::thread::spawn(move || {
            publish_session(&writer, &sender);
            loop {
                match receiver.poll() {
                    Ok(Some(msg)) => {
                        handle_session(&msg, &writer, &sender);
                        follow_game_golfers(&msg, &system, &writer, &sender);
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                    Err(_) => return,
                }
            }
        });
        Self {
            shutdown,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for SessionActor {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Card {
    fn start(addr: SocketAddr, state: TestState) -> Self {
        let tokens = Arc::clone(&state.pureplay_tokens);
        Self::start_with(addr, state, tokens)
    }

    /// The card with its tokens in `tokens`.
    fn start_with(addr: SocketAddr, state: TestState, tokens: Arc<TokenStore>) -> Self {
        Self::start_pinging(addr, state, tokens, PING_EVERY)
    }

    /// The card pinging a quiet link every `ping_every`.
    fn start_pinging(
        addr: SocketAddr,
        state: TestState,
        tokens: Arc<TokenStore>,
        ping_every: Duration,
    ) -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self::start_on(addr, state, tokens, ping_every, &tx)
    }

    /// The card on the bus `tx`, registered as a shot source and stamping
    /// its provenance as `start_actor` has it, with the session
    /// ([`SessionActor`]) answering its selects and everyone else's.
    fn start_on(
        addr: SocketAddr,
        state: TestState,
        tokens: Arc<TokenStore>,
        ping_every: Duration,
        tx: &broadcast::Sender<BusMsg>,
    ) -> Self {
        let rx = tx.subscribe();
        let session = SessionActor::start(&state, tx);
        let shutdown = Arc::new(AtomicBool::new(false));
        let source = ShotSource::PureplayAccessory;
        state.round.register_source(ACTOR, source);
        let sender = BusSender::new(ACTOR.into(), tx.clone(), Arc::clone(&shutdown))
            .with_frp_id(Some(ACTOR.into()));
        let receiver = sender.subscribe();
        let thread_state = Arc::clone(&state.state);
        let thread = std::thread::spawn(move || {
            run_with(addr, &thread_state, &tokens, &sender, receiver, ping_every);
        });
        Self {
            state,
            rx,
            session,
            shutdown,
            thread: Some(thread),
            view: ActorStatusResponse::new("Pureplay", ActorStatus::Starting),
            sent: Vec::new(),
            raw: Vec::new(),
            queued: std::collections::VecDeque::new(),
        }
    }

    fn tokens(&self) -> &TokenStore {
        &self.state.pureplay_tokens
    }

    fn token_key(addr: SocketAddr) -> String {
        token_key(addr)
    }

    /// Take one message off the bus, folding statuses into the view.
    fn step(&mut self) -> Option<FlighthookMessage> {
        if let Some(msg) = self.queued.pop_front() {
            return Some(msg);
        }
        match self.rx.try_recv() {
            Ok(msg) => {
                self.raw.push(msg.clone());
                // The card's `bus.actor` as the web layer shows it: its
                // device telemetry keys (`ready`) on the card; an empty
                // value removes the key.
                if let Some(flightrelay::FrpMessage {
                    from,
                    body: FrpBody::BusActor(actor),
                }) = msg.as_frp()
                    && from == ACTOR
                {
                    for (k, v) in flighthook::bus_actor_rows(actor) {
                        if v.is_empty() {
                            self.view.telemetry.remove(&k);
                        } else {
                            self.view.telemetry.insert(k, v);
                        }
                    }
                }
                for m in unpack(&msg) {
                    if let FlighthookEvent::ActorStatus { status, telemetry } = &m.event {
                        self.view.apply_status(*status, telemetry, 0);
                    }
                    self.sent.push(m.clone());
                    self.queued.push_back(m);
                }
                self.queued.pop_front().or_else(|| {
                    Some(FlighthookMessage {
                        actor: String::new(),
                        event: FlighthookEvent::Other,
                    })
                })
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(5));
                None
            }
        }
    }

    /// Wait until the card shows `status` and `pred` holds for its rows.
    fn shows(&mut self, status: ActorStatus, pred: impl Fn(&HashMap<String, String>) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            if self.view.status == status && pred(&self.view.telemetry) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "wanted {status:?}, card shows {:?} {:?}",
                self.view.status,
                self.view.telemetry
            );
            self.step();
        }
    }

    /// Wait until the card shows `status` with row `key` = `value`.
    fn shows_row(&mut self, status: ActorStatus, key: &str, value: &str) {
        self.shows(status, |t| t.get(key).map(String::as_str) == Some(value));
    }

    /// Wait for the next bus event `pred` picks.
    fn bus(&mut self, pred: impl Fn(&FlighthookEvent) -> bool) -> FlighthookMessage {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "no such event");
            if let Some(msg) = self.step()
                && pred(&msg.event)
            {
                return msg;
            }
        }
    }

    /// Take what the bus holds now.
    fn settle(&mut self) {
        let until = Instant::now() + Duration::from_millis(100);
        while Instant::now() < until {
            self.step();
        }
    }

    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            t.join().expect("the card's thread ends");
        }
        self.settle();
        self.session.stop();
    }

    /// Stop the card; how long its thread took to end.
    fn stop_timed(&mut self) -> Duration {
        let started = Instant::now();
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            t.join().expect("the card's thread ends");
        }
        started.elapsed()
    }
}

impl Drop for Card {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn clubs(sent: &[FlighthookMessage]) -> Vec<Club> {
    sent.iter()
        .filter_map(|m| match &m.event {
            FlighthookEvent::ClubInfo { club_info } => Some(club_info.club),
            _ => None,
        })
        .collect()
}

fn is_club(event: &FlighthookEvent, club: Club) -> bool {
    matches!(event, FlighthookEvent::ClubInfo { club_info } if club_info.club == club)
}

/// A card paired with token `token` that has taken the round snapshot.
fn connected(mock: &Mock, state: TestState, token: &str) -> (Card, Peer) {
    connected_pinging(mock, state, token, PING_EVERY)
}

/// [`connected`], the card pinging a quiet link every `ping_every`.
fn connected_pinging(
    mock: &Mock,
    state: TestState,
    token: &str,
    ping_every: Duration,
) -> (Card, Peer) {
    state
        .pureplay_tokens
        .save(&token_key(mock.addr), &Token::new(token), "client-1")
        .expect("save");
    let tokens = Arc::clone(&state.pureplay_tokens);
    let mut card = Card::start_pinging(mock.addr, state, tokens, ping_every);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some(token));
    peer.paired(token);
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    (card, peer)
}

// -- Pairing and the round -------------------------------------------------------

/// No saved token: the card asks, waits in Pureplay's queue, counts the
/// prompt down, saves the token Pureplay gives, subscribes and follows the
/// round. Every frame it sends is text; the server's binary and text
/// frames are both read. The session holds a left-handed golfer, so the
/// snapshot's right hand and its club both go out.
#[test]
fn a_new_card_pairs_with_a_prompt_and_follows_the_round() {
    let mock = Mock::new();
    let state = new_state();
    let lefty = flightrelay::GolferSelect {
        handedness: Some(Handedness::Left),
        ..flightrelay::GolferSelect::new("lefty")
    };
    assert!(state.writer.apply(&lefty).is_some());
    let mut card = Card::start(mock.addr, state);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None, "no token: the prompt");
    card.shows_row(ActorStatus::Starting, "pairing", WAITING);

    peer.send(&json!({ "type": "pairing.pending", "data": { "expiresInSec": 3 } }));
    card.shows_row(
        ActorStatus::Starting,
        "pairing",
        "Approve flighthook in Pureplay (3 s)",
    );
    card.shows_row(
        ActorStatus::Starting,
        "pairing",
        "Approve flighthook in Pureplay (2 s)",
    );

    peer.paired("PPA-NEW-TOKEN");
    peer.subscribe(&round(), 1);
    card.shows(ActorStatus::Connected, |t| {
        t.get("name").map(String::as_str) == Some("Joe")
            && t.get("club").map(String::as_str) == Some("7I")
            && t.get("handed").map(String::as_str) == Some("rh")
            && t.get("hole").map(String::as_str) == Some("7, par 4")
            && t.get("ready").map(String::as_str) == Some("true")
            && t.get("shots").map(String::as_str) == Some("auto")
            && t.get("pureplay").map(String::as_str) == Some("1.4.2")
            && !t.contains_key("blockers")
            && !t.contains_key("pairing")
            && !t.contains_key("missed_events")
    });
    assert_eq!(
        card.tokens().get(&Card::token_key(mock.addr)),
        Some(Token::new("PPA-NEW-TOKEN"))
    );
    card.settle();
    // The snapshot's hand and club, together in one `golfer.select`.
    let from_snapshot: Vec<&FlighthookMessage> = card
        .sent
        .iter()
        .filter(|m| {
            matches!(
                m.event,
                FlighthookEvent::ClubInfo { .. } | FlighthookEvent::HandInfo { .. }
            )
        })
        .collect();
    assert_eq!(from_snapshot.len(), 2, "{from_snapshot:?}");
    let selects = card
        .raw
        .iter()
        .filter(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::GolferSelect(_))))
        .count();
    assert_eq!(selects, 1, "one select carries both");
    assert!(card.sent.iter().any(|m| matches!(
        &m.event,
        FlighthookEvent::HandInfo { hand_info } if hand_info.handed == Handedness::Right
    )));

    // Another app's club change, in a text frame.
    peer.send_text(&event_json(
        "player.club_changed",
        2,
        &json!({ "playerId": 1, "shotId": "shot-1", "previous": "Iron7", "current": "Iron8",
                 "previousClubId": "b-7i", "currentClubId": "b-8i", "reason": "api_command" }),
    ));
    card.bus(|e| is_club(e, Club::Iron8));
    card.shows_row(ActorStatus::Connected, "club", "8I");

    // Not ready while the ball is in the air.
    peer.event(
        "shot.ready_changed",
        3,
        &json!({ "shotId": "shot-1", "playerId": 1, "ready": false, "controllable": false,
                 "allowedCommands": [], "blockers": ["shot_in_progress"] }),
    );
    card.shows(ActorStatus::Connected, |t| {
        t.get("ready").map(String::as_str) == Some("false")
            && t.get("blockers").map(String::as_str) == Some("shot in progress")
    });

    // A gap in `seq`: counted, and the card asks for the state.
    peer.event("hole.started", 6, &json!({ "holeNumber": 8, "par": 3 }));
    let get = peer.recv();
    assert_eq!(get["type"], "state.get", "{get}");
    card.shows_row(ActorStatus::Connected, "missed_events", "1");
    card.shows_row(ActorStatus::Connected, "hole", "8, par 3");
    peer.send(
        &json!({ "type": "event", "event": "state.snapshot", "requestId": get["id"],
                       "seq": 7, "time": "t", "data": round() }),
    );
    card.shows_row(ActorStatus::Connected, "hole", "7, par 4");

    assert!(
        peer.kinds.iter().all(|k| *k == FrameType::Text),
        "the card sends text: {:?}",
        peer.kinds
    );
    card.stop();
}

/// A drop without a close frame (Pureplay closed, settings changed):
/// Reconnecting, then a new connection pairs with the saved token.
#[test]
fn a_drop_without_a_close_frame_reconnects_with_the_saved_token() {
    let mock = Mock::new();
    let (mut card, peer) = connected(&mock, new_state(), "PPA-KEEP");
    drop(peer);
    card.shows(ActorStatus::Reconnecting, |t| t.is_empty());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(
        peer.pair().as_deref(),
        Some("PPA-KEEP"),
        "the token is kept"
    );
    peer.paired("PPA-KEEP");
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "name", "Joe");
    card.stop();
}

/// A token file that cannot be written: an `error` on the card, the
/// session goes on, and a reconnect pairs with the token Pureplay gave.
#[test]
fn a_token_that_cannot_be_saved_is_an_error_and_kept_for_the_session() {
    let blocker = std::env::temp_dir().join(format!(
        "flighthook-pp-not-a-folder-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&blocker, b"a file").expect("file");
    let _blocker = Leftover(blocker.clone());
    let tokens = Arc::new(TokenStore::new(blocker.join(token::FILE_NAME)));
    let mock = Mock::new();
    let mut card = Card::start_with(mock.addr, new_state(), tokens);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    peer.paired("PPA-UNSAVED");
    peer.subscribe(&round(), 1);
    card.shows(ActorStatus::Connected, |t| {
        t.get("error")
            .is_some_and(|e| e.starts_with("Couldn't save the Pureplay pairing: "))
            && t.get("club").map(String::as_str) == Some("7I")
    });
    drop(peer);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some("PPA-UNSAVED"), "no new prompt");
    peer.paired("PPA-UNSAVED");
    peer.subscribe(&round(), 1);
    card.shows(ActorStatus::Connected, |t| {
        t.get("error")
            .is_some_and(|e| e.starts_with("Couldn't save the Pureplay pairing: "))
            && t.contains_key("club")
    });
    card.stop();
    let _ = std::fs::remove_file(&blocker);
}

/// Nothing listening: Reconnecting, no row, and the card keeps trying.
#[test]
fn a_refused_connection_is_reconnecting_with_no_row() {
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr")
    };
    let mut card = Card::start(addr, new_state());
    card.shows(ActorStatus::Reconnecting, |t| t.is_empty());
    card.stop();
}

#[test]
fn too_many_connections_before_hello_backs_off() {
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    mock.next().close(4429);
    card.shows_row(ActorStatus::Reconnecting, "pairing", TOO_MANY);
    // Tried again after the backoff.
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    card.stop();
}

#[test]
fn a_hello_of_another_protocol_or_version_is_an_error_and_retried() {
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello_as("somethingelse", 1);
    card.shows_row(
        ActorStatus::Reconnecting,
        "error",
        "Not a Pureplay Accessory server",
    );
    let mut peer2 = mock.next();
    peer2.hello_as("ppaccessory", 2);
    card.shows_row(
        ActorStatus::Reconnecting,
        "error",
        "Pureplay speaks Accessory version 2; this flighthook speaks 1",
    );
    // Pureplay gone (a drop before hello): the version error goes.
    drop(mock.next());
    card.shows(ActorStatus::Reconnecting, |t| t.is_empty());
    let mut peer3 = mock.next();
    peer3.hello();
    assert_eq!(peer3.pair(), None);
    drop(peer);
    card.stop();
}

/// `bad_request` to `pair`: another app on this address waits for its
/// prompt. The card closes and asks again later.
#[test]
fn another_app_waiting_closes_and_asks_again_later() {
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    peer.refuse(Some(protocol::PAIR_ID), "bad_request", &json!({}));
    card.shows_row(ActorStatus::Reconnecting, "pairing", ANOTHER_APP);
    assert!(peer.closed_by_card(), "the card closes the socket");
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None, "asked again");
    card.stop();
}

/// Each refusal of a `pair` that closes the socket: Disconnected, the
/// reason in the `pairing` row, and no new attempt by itself.
#[test]
fn a_refusal_is_disconnected_with_its_reason_and_not_retried() {
    for (code, close, text) in [
        ("forbidden_scope", 4403, ASK_OFF),
        ("unauthorised", 4403, DENIED),
        ("unauthorised", 4408, TIMED_OUT),
    ] {
        let mock = Mock::new();
        let mut card = Card::start(mock.addr, new_state());
        let mut peer = mock.next();
        peer.hello();
        assert_eq!(peer.pair(), None);
        peer.refuse(Some(protocol::PAIR_ID), code, &json!({}));
        peer.close(close);
        card.shows_row(ActorStatus::Disconnected, "pairing", text);
        assert!(
            !mock.reconnects(),
            "{code} {close}: no retry until Reconnect"
        );
        card.stop();
    }
}

/// Pureplay's cooldown: the wait counts down, then the card asks once
/// more by itself; a second cooldown answer is not retried.
#[test]
fn a_cooldown_counts_down_and_retries_once() {
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    peer.refuse(
        Some(protocol::PAIR_ID),
        "rate_limited",
        &json!({ "retryAfterMs": 1500 }),
    );
    let refused = Instant::now();
    peer.close(4429);
    card.shows_row(
        ActorStatus::Disconnected,
        "pairing",
        "Pureplay asks to wait (0:02)",
    );
    card.shows_row(
        ActorStatus::Disconnected,
        "pairing",
        "Pureplay asks to wait (0:01)",
    );
    let mut peer = mock.next();
    assert!(
        refused.elapsed() >= Duration::from_millis(1300),
        "after the wait"
    );
    peer.hello();
    assert_eq!(peer.pair(), None, "the attempt, once more");
    peer.refuse(
        Some(protocol::PAIR_ID),
        "rate_limited",
        &json!({ "retryAfterMs": 200 }),
    );
    peer.close(4429);
    card.shows(ActorStatus::Disconnected, |t| {
        t.get("pairing")
            .is_some_and(|p| p.starts_with("Pureplay asks to wait"))
    });
    assert!(!mock.reconnects(), "one retry only");
    card.shows_row(ActorStatus::Disconnected, "pairing", PRESS_RECONNECT);
    card.stop();
}

/// A saved token Pureplay no longer knows: deleted, and the card pairs
/// once without one.
#[test]
fn a_dead_token_is_deleted_and_the_card_pairs_without_one() {
    let mock = Mock::new();
    let state = new_state();
    state
        .pureplay_tokens
        .save(&token_key(mock.addr), &Token::new("PPA-DEAD"), "c")
        .expect("save");
    let mut card = Card::start(mock.addr, state);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some("PPA-DEAD"));
    peer.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
    peer.close(4401);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None, "paired again without a token");
    card.shows_row(ActorStatus::Starting, "pairing", WAITING);
    assert_eq!(card.tokens().get(&token_key(mock.addr)), None);
    card.stop();
}

/// Removed in Pureplay's Settings mid-session: an unprompted
/// `unauthorised`, then `4401`. The token goes, and nothing is retried.
#[test]
fn a_removal_in_pureplay_deletes_the_token_and_waits_for_reconnect() {
    let mock = Mock::new();
    let (mut card, mut peer) = connected(&mock, new_state(), "PPA-GONE");
    peer.refuse(None, "unauthorised", &json!({}));
    peer.close(4401);
    card.shows_row(ActorStatus::Disconnected, "pairing", REMOVED);
    assert_eq!(card.tokens().get(&token_key(mock.addr)), None);
    assert!(!mock.reconnects(), "a removal starts Pureplay's cooldown");
    card.stop();
}

/// Club in: a turn to a second local player gives their name, hand and
/// club; a repeated club gives nothing; a remote player's turn and club
/// give nothing.
#[test]
fn club_and_hand_follow_the_active_local_player() {
    let mock = Mock::new();
    let (mut card, mut peer) = connected(&mock, new_state(), "PPA-CLUB");
    card.settle();
    let before = card.sent.len();
    peer.event(
        "player.club_changed",
        2,
        &json!({ "playerId": 1, "current": "Iron7", "currentClubId": "b-7i", "reason": "auto" }),
    );
    card.settle();
    assert_eq!(clubs(&card.sent[before..]), [], "the club flighthook holds");

    peer.event(
        "turn.changed",
        3,
        &json!({ "previousPlayerId": 1, "currentPlayerId": 2, "holeNumber": 7, "reason": "next_player" }),
    );
    card.bus(|e| is_club(e, Club::Driver));
    card.shows(ActorStatus::Connected, |t| {
        t.get("name").map(String::as_str) == Some("Ann")
            && t.get("handed").map(String::as_str) == Some("lh")
            && t.get("club").map(String::as_str) == Some("DR")
    });
    assert!(card.sent.iter().any(|m| matches!(
        &m.event,
        FlighthookEvent::HandInfo { hand_info } if hand_info.handed == Handedness::Left
    )));

    let before = card.sent.len();
    peer.event(
        "turn.changed",
        4,
        &json!({ "previousPlayerId": 2, "currentPlayerId": 3, "holeNumber": 7, "reason": "next_player" }),
    );
    peer.event(
        "player.club_changed",
        5,
        &json!({ "playerId": 3, "current": "Wood3", "reason": "user_selected" }),
    );
    card.shows(ActorStatus::Connected, |t| !t.contains_key("name"));
    card.settle();
    let given: Vec<&FlighthookEvent> = card.sent[before..]
        .iter()
        .map(|m| &m.event)
        .filter(|e| {
            matches!(
                e,
                FlighthookEvent::ClubInfo { .. } | FlighthookEvent::HandInfo { .. }
            )
        })
        .collect();
    assert!(given.is_empty(), "a remote player gives nothing: {given:?}");
    card.stop();
}

// -- The token never leaks -------------------------------------------------------

/// A session with a known token, through a prompt, a reconnect and a
/// removal: the token is in no log line, no bus message (raw frames,
/// telemetry: what the WebSocket and the Log tab are built from), no
/// `/api/status` or `/api/settings` body, no config file, no `Debug` of the
/// parsed frame; only in the token file. The card puts no shots on the bus,
/// so no export carries anything of it. The log is captured at INFO;
/// tungstenite's trace lines, which print whole frames, are held at `warn`
/// by the app's log filter (`main.rs`).
#[test]
fn the_token_appears_only_in_the_token_file() {
    let log = crate::actors::driver::tests::process_log();
    let token = format!("PPA-LEAK-{}", uuid::Uuid::new_v4().simple());
    let mock = Mock::new();
    let config = config_path();
    let state = new_state_at(&config);
    // The card's section, saved as a card is.
    state.system.update(|c| {
        c.pureplay_accessory.insert(
            "0".into(),
            flighthook::PureplayAccessorySection {
                address: Some(mock.addr.to_string()),
                ..flighthook::PureplayAccessorySection::default()
            },
        );
    });
    let mut card = Card::start(mock.addr, state.clone());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    peer.paired(&token);
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    peer.event(
        "player.club_changed",
        2,
        &json!({ "playerId": 1, "current": "Iron8", "reason": "user_selected" }),
    );
    card.bus(|e| is_club(e, Club::Iron8));
    drop(peer);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some(token.as_str()));
    peer.paired(&token);
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    peer.refuse(None, "unauthorised", &json!({}));
    peer.close(4401);
    card.shows_row(ActorStatus::Disconnected, "pairing", REMOVED);
    card.stop();

    // The token file held it until the removal deleted it.
    assert_eq!(state.pureplay_tokens.get(&token_key(mock.addr)), None);
    for msg in &card.raw {
        let text = format!("{msg:?}");
        assert!(!text.contains(&token), "bus: {text}");
    }
    let lines = log.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains(ACTOR) && l.contains(token::REDACTED)),
        "the frames are logged, redacted"
    );
    let leaked: Vec<&String> = lines.iter().filter(|l| l.contains(&token)).collect();
    assert!(leaked.is_empty(), "log: {leaked:?}");
    let text = std::fs::read_to_string(&config).expect("config file");
    assert!(text.contains("[pureplay_accessory.0]"), "{text}");
    assert!(!text.contains(&token), "config file");
    let (status, settings) = web_bodies(&state, &card.raw);
    assert!(status.contains(REMOVED), "{status}");
    assert!(!status.contains(&token), "/api/status: {status}");
    assert!(settings.contains("pureplay_accessory"), "{settings}");
    assert!(!settings.contains(&token), "/api/settings: {settings}");
    // The parsed `paired` frame's Debug.
    let frame = json!({ "type": "paired", "data": {
        "token": token, "clientId": "c", "scopes": ["read"] } })
    .to_string();
    let parsed = format!("{:?}", protocol::parse(&frame));
    assert!(parsed.contains(token::REDACTED), "{parsed}");
    assert!(!parsed.contains(&token), "{parsed}");
    // While paired, only the token file had it.
    let mock = Mock::new();
    let state = new_state();
    let (mut card, _peer) = connected(&mock, state.clone(), &token);
    card.settle();
    let (status, _) = web_bodies(&state, &card.raw);
    assert!(status.contains("\"club\":\"7I\""), "{status}");
    assert!(!status.contains(&token), "/api/status while connected");
    card.stop();
    let file = std::fs::read_to_string(state.pureplay_tokens.path()).expect("token file");
    assert!(file.contains(&token));
    for msg in &card.raw {
        assert!(!format!("{msg:?}").contains(&token));
    }
}

/// The `GET /api/status` and `GET /api/settings` bodies of a web server
/// that took `sent` off the bus.
fn web_bodies(state: &Arc<SystemState>, sent: &[BusMsg]) -> (String, String) {
    use axum::extract::State;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let web = crate::actors::web::tests::folded(Arc::clone(state), sent).await;
        let status = crate::actors::web::routes::get_status(State(Arc::clone(&web))).await;
        let settings = crate::actors::web::routes::get_settings(State(web)).await;
        (
            serde_json::to_string(&status.0).expect("status"),
            serde_json::to_string(&settings.0).expect("settings"),
        )
    })
}

// -- Refusals without a close frame -----------------------------------------------

/// Denied, and the socket drops before a close frame arrives: the response
/// decides. Disconnected, and the player is not asked again.
#[test]
fn a_denial_without_a_close_frame_is_not_asked_again() {
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    peer.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
    std::thread::sleep(Duration::from_millis(100));
    drop(peer);
    card.shows_row(ActorStatus::Disconnected, "pairing", DENIED);
    assert!(!mock.reconnects(), "denied: no new pair by itself");
    card.stop();
}

/// Removed in Pureplay mid-session, the socket dropped without a close
/// frame: the token goes, Disconnected, no reconnect.
#[test]
fn a_removal_without_a_close_frame_is_not_asked_again() {
    let mock = Mock::new();
    let (mut card, mut peer) = connected(&mock, new_state(), "PPA-GONE");
    peer.refuse(None, "unauthorised", &json!({}));
    std::thread::sleep(Duration::from_millis(100));
    drop(peer);
    card.shows_row(ActorStatus::Disconnected, "pairing", REMOVED);
    assert_eq!(card.tokens().get(&token_key(mock.addr)), None);
    assert!(!mock.reconnects(), "removed: no reconnect by itself");
    card.stop();
}

/// The other refusals without a close frame: dropped, or left open (the
/// card waits a moment, then closes and decides itself).
#[test]
fn other_refusals_without_a_close_frame_decide_by_the_response() {
    for keep_open in [false, true] {
        // forbidden_scope: Ask about new apps is off.
        let mock = Mock::new();
        let mut card = Card::start(mock.addr, new_state());
        let mut peer = mock.next();
        peer.hello();
        assert_eq!(peer.pair(), None);
        peer.refuse(Some(protocol::PAIR_ID), "forbidden_scope", &json!({}));
        if keep_open {
            assert!(peer.closed_by_card(), "the card closes");
        } else {
            drop(peer);
        }
        card.shows_row(ActorStatus::Disconnected, "pairing", ASK_OFF);
        assert!(!mock.reconnects(), "forbidden_scope, open {keep_open}");
        card.stop();

        // rate_limited: the cooldown.
        let mock = Mock::new();
        let mut card = Card::start(mock.addr, new_state());
        let mut peer = mock.next();
        peer.hello();
        assert_eq!(peer.pair(), None);
        peer.refuse(
            Some(protocol::PAIR_ID),
            "rate_limited",
            &json!({ "retryAfterMs": 60_000 }),
        );
        if keep_open {
            assert!(peer.closed_by_card(), "the card closes");
        } else {
            drop(peer);
        }
        card.shows_row(
            ActorStatus::Disconnected,
            "pairing",
            "Pureplay asks to wait (1:00)",
        );
        card.stop();

        // unauthorised to a saved token: the dead-token rule.
        let mock = Mock::new();
        let state = new_state();
        state
            .pureplay_tokens
            .save(&token_key(mock.addr), &Token::new("PPA-STALE"), "c")
            .expect("save");
        let mut card = Card::start(mock.addr, state);
        let mut peer = mock.next();
        peer.hello();
        assert_eq!(peer.pair().as_deref(), Some("PPA-STALE"));
        peer.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
        if keep_open {
            assert!(peer.closed_by_card(), "the card closes");
        } else {
            drop(peer);
        }
        let mut peer = mock.next();
        peer.hello();
        assert_eq!(peer.pair(), None, "paired once without a token");
        assert_eq!(card.tokens().get(&token_key(mock.addr)), None);
        card.stop();
    }
}

// -- The token in a frame that does not read --------------------------------------

/// A `paired` frame whose `scopes` is `null`: the token is learned before
/// the frame is logged, and the card still pairs.
#[test]
fn an_unreadable_paired_frame_does_not_leak_the_token() {
    let log = crate::actors::driver::tests::process_log();
    let token = format!("PPA-SCRATCH-{}", uuid::Uuid::new_v4().simple());
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    peer.send(&json!({ "type": "paired", "data": {
        "token": token, "clientId": "c", "scopes": null } }));
    let sub = peer.recv();
    assert_eq!(sub["type"], "subscribe", "paired: {sub}");
    assert_eq!(
        card.tokens().get(&token_key(mock.addr)),
        Some(Token::new(token.as_str()))
    );
    card.stop();
    let lines = log.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"type\":\"paired\"") && l.contains(token::REDACTED)),
        "the paired frame is logged, redacted"
    );
    let leaked: Vec<&String> = lines.iter().filter(|l| l.contains(&token)).collect();
    assert!(leaked.is_empty(), "log: {leaked:?}");
}

/// A refused `pair` whose message repeats the saved token: the frame and
/// the log line about the refusal show `<redacted>`.
#[test]
fn a_refused_pair_repeating_the_token_does_not_leak_it() {
    let log = crate::actors::driver::tests::process_log();
    let token = format!("PPA-ECHO-{}", uuid::Uuid::new_v4().simple());
    let mock = Mock::new();
    let state = new_state();
    state
        .pureplay_tokens
        .save(&token_key(mock.addr), &Token::new(token.as_str()), "c")
        .expect("save");
    let mut card = Card::start(mock.addr, state);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some(token.as_str()));
    peer.refuse(
        Some(protocol::PAIR_ID),
        &format!("bad_{token}"),
        &json!({ "message": format!("token {token} is not known") }),
    );
    peer.close(4401);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    card.stop();
    let lines = log.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("pair refused") && l.contains(token::REDACTED)),
        "the refusal is logged, redacted"
    );
    let leaked: Vec<&String> = lines.iter().filter(|l| l.contains(&token)).collect();
    assert!(leaked.is_empty(), "log: {leaked:?}");
    for msg in &card.raw {
        assert!(!format!("{msg:?}").contains(&token));
    }
}

// -- state.get pacing -------------------------------------------------------------

/// Five gaps in a burst: one `state.get`, the others wait for its answer.
#[test]
fn gaps_in_a_burst_ask_for_the_state_once() {
    let mock = Mock::new();
    let (mut card, mut peer) = connected(&mock, new_state(), "PPA-GAPS");
    for seq in [3u64, 5, 7, 9, 11] {
        peer.event("hole.started", seq, &json!({ "holeNumber": 8, "par": 3 }));
    }
    let mut gets = Vec::new();
    let until = Instant::now() + Duration::from_millis(600);
    while Instant::now() < until {
        if let Ok(Message::Text(t)) = peer.ws.read()
            && t.as_str().contains("state.get")
        {
            gets.push(serde_json::from_str::<Value>(t.as_str()).expect("json"));
        }
    }
    assert_eq!(gets.len(), 1, "one state.get outstanding at a time");
    card.shows_row(ActorStatus::Connected, "missed_events", "5");
    // Answered: the next gap asks again.
    peer.send(
        &json!({ "type": "event", "event": "state.snapshot", "requestId": gets[0]["id"],
                 "seq": 12, "time": "t", "data": round() }),
    );
    peer.event("hole.started", 14, &json!({ "holeNumber": 9, "par": 4 }));
    let get = peer.recv();
    assert_eq!(get["type"], "state.get", "{get}");
    assert_ne!(get["id"], gets[0]["id"], "a new request id");
    card.stop();
}

/// `context.invalidated`: the round's rows go and the card asks for the
/// state; the answer brings them back.
#[test]
fn context_invalidated_clears_the_rows_and_asks_for_the_state() {
    let mock = Mock::new();
    let (mut card, mut peer) = connected(&mock, new_state(), "PPA-CTX");
    peer.event(
        "context.invalidated",
        2,
        &json!({ "reason": "loading", "matchId": null }),
    );
    let get = peer.recv();
    assert_eq!(get["type"], "state.get", "{get}");
    card.shows(ActorStatus::Connected, |t| {
        ["name", "club", "handed", "hole", "blockers"]
            .iter()
            .all(|k| !t.contains_key(*k))
            && t.get("ready").map(String::as_str) == Some("false")
            && t.get("pureplay").map(String::as_str) == Some("1.4.2")
    });
    peer.send(
        &json!({ "type": "event", "event": "state.snapshot", "requestId": get["id"],
                 "seq": 3, "time": "t", "data": round() }),
    );
    card.shows(ActorStatus::Connected, |t| {
        t.get("club").map(String::as_str) == Some("7I")
            && t.get("hole").map(String::as_str) == Some("7, par 4")
    });
    card.stop();
}

// -- Pairing deadlines ------------------------------------------------------------

/// A `pair` with a token that gets no answer: the card gives up and
/// reconnects.
#[test]
fn an_unanswered_pair_with_a_token_gives_up() {
    let mock = Mock::new();
    let state = new_state();
    state
        .pureplay_tokens
        .save(&token_key(mock.addr), &Token::new("PPA-HANG"), "c")
        .expect("save");
    let mut card = Card::start(mock.addr, state);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some("PPA-HANG"));
    // No answer: four times the test pair timeout.
    let mut again = mock
        .conns
        .recv_timeout(Duration::from_secs(4))
        .expect("the card gives up and reconnects");
    again.hello();
    assert_eq!(again.pair().as_deref(), Some("PPA-HANG"), "token kept");
    card.stop();
}

/// `bad_request` six times in a row: the card stops asking and shows
/// Pureplay's message.
#[test]
fn bad_request_six_times_stops_with_pureplays_message() {
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    for n in 1..=BAD_REQUEST_TRIES {
        let mut peer = mock.next();
        peer.hello();
        assert_eq!(peer.pair(), None, "try {n}");
        peer.refuse(
            Some(protocol::PAIR_ID),
            "bad_request",
            &json!({ "message": "A request is already pending." }),
        );
        assert!(peer.closed_by_card(), "try {n}: the card closes");
        if n < BAD_REQUEST_TRIES {
            card.shows_row(ActorStatus::Reconnecting, "pairing", ANOTHER_APP);
        }
    }
    card.shows_row(
        ActorStatus::Disconnected,
        "pairing",
        "A request is already pending.",
    );
    assert!(!mock.reconnects(), "no seventh try");
    card.stop();
}

/// A drop without a close frame while Pureplay's prompt is up: Reconnecting,
/// then a new `pair` without a token (the automatic one was not spent).
#[test]
fn a_drop_while_the_prompt_is_up_asks_again() {
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    peer.send(&json!({ "type": "pairing.pending", "data": { "expiresInSec": 60 } }));
    card.shows_row(
        ActorStatus::Starting,
        "pairing",
        "Approve flighthook in Pureplay (60 s)",
    );
    drop(peer);
    card.shows(ActorStatus::Reconnecting, |t| !t.contains_key("pairing"));
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None, "asked again");
    card.stop();
}

/// A dead token after the one automatic pair of this start was spent:
/// deleted, Disconnected, "Removed in Pureplay", no new prompt by itself.
#[test]
fn a_dead_token_after_the_automatic_pair_waits_for_reconnect() {
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    peer.paired("PPA-ONCE");
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    drop(peer);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some("PPA-ONCE"));
    peer.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
    peer.close(4401);
    card.shows_row(ActorStatus::Disconnected, "pairing", REMOVED);
    assert_eq!(card.tokens().get(&token_key(mock.addr)), None);
    assert!(!mock.reconnects(), "no second prompt by itself");
    card.stop();
}

// -- Pureplay restarted, events offered --------------------------------------------

/// A new `serverInstanceId` (Pureplay restarted) drops the cached round; the
/// same one keeps it.
#[test]
fn a_new_server_instance_drops_the_cached_round() {
    const DROPPED: &str = "Pureplay restarted, cached state dropped";
    let log = crate::actors::driver::tests::process_log();
    let dropped = || {
        log.lines()
            .iter()
            .filter(|l| l.contains(ACTOR) && l.contains(DROPPED))
            .count()
    };
    let mock = Mock::new();
    let (mut card, peer) = connected(&mock, new_state(), "PPA-INST");
    let before = dropped();
    drop(peer);
    let mut peer = mock.next();
    peer.hello_from("s-1");
    assert_eq!(peer.pair().as_deref(), Some("PPA-INST"));
    peer.paired("PPA-INST");
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    assert_eq!(dropped(), before, "the same instance keeps it");
    drop(peer);
    let mut peer = mock.next();
    peer.hello_from("s-2");
    assert_eq!(peer.pair().as_deref(), Some("PPA-INST"));
    peer.paired("PPA-INST");
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    assert_eq!(dropped(), before + 1, "a new instance drops it");
    card.stop();
}

/// The card subscribes to the events of its list that Pureplay offers; a
/// list offering none of them is an `error`, and nothing is subscribed.
#[test]
fn only_the_offered_events_are_subscribed() {
    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello_with(
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
    assert_eq!(peer.pair(), None);
    peer.paired("PPA-SUBSET");
    peer.subscribe_to(
        &[
            "hole.started",
            "turn.changed",
            "player.club_changed",
            "lie.changed",
        ],
        &round(),
        1,
    );
    card.shows_row(ActorStatus::Connected, "club", "7I");
    card.stop();

    let mock = Mock::new();
    let state = new_state();
    state
        .pureplay_tokens
        .save(&token_key(mock.addr), &Token::new("PPA-NONE"), "c")
        .expect("save");
    let mut card = Card::start(mock.addr, state);
    let mut peer = mock.next();
    peer.hello_with(
        "ppaccessory",
        1,
        "s-1",
        &["state.snapshot", "ruling.required"],
    );
    assert_eq!(peer.pair().as_deref(), Some("PPA-NONE"));
    peer.paired("PPA-NONE");
    card.shows_row(ActorStatus::Reconnecting, "error", NO_EVENTS);
    assert!(peer.closed_by_card(), "closed without a subscribe");
    card.stop();
}

/// Malformed frames (not JSON, not UTF-8, `null`, wrong types) are ignored
/// and the round goes on; a frame that is not JSON is logged by its length
/// only.
#[test]
fn malformed_frames_are_ignored() {
    let log = crate::actors::driver::tests::process_log();
    let junk = format!("junk-{}", uuid::Uuid::new_v4().simple());
    let mock = Mock::new();
    let (mut card, mut peer) = connected(&mock, new_state(), "PPA-JUNK");
    peer.send_raw(Message::text(format!("{junk} not json")));
    peer.send_raw(Message::binary(vec![0xff, 0xfe, 0x00]));
    peer.send_raw(Message::text("null"));
    peer.send_raw(Message::binary(b"{\"type\":null}".to_vec()));
    peer.send(&json!({ "type": "event", "event": null, "seq": null, "data": null }));
    peer.send(&json!({ "type": "response", "id": null, "ok": false, "error": "not an object" }));
    peer.send(&json!({ "type": "paired", "data": null }));
    peer.send(
        &json!({ "type": "event", "event": "player.club_changed", "seq": 2,
                       "data": { "playerId": "one", "current": 7 } }),
    );
    peer.event(
        "player.club_changed",
        3,
        &json!({ "playerId": 1, "current": "Iron8", "reason": "user_selected" }),
    );
    card.bus(|e| is_club(e, Club::Iron8));
    card.shows_row(ActorStatus::Connected, "club", "8I");
    card.stop();
    let lines = log.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains(ACTOR) && l.contains("not JSON (not shown)")),
        "the frame's length is logged"
    );
    assert!(!lines.iter().any(|l| l.contains(&junk)), "its text is not");
}

// -- Liveness and stopping --------------------------------------------------------

/// A link that has gone quiet is pinged; two unanswered pings and it is lost.
#[test]
fn a_quiet_link_is_lost_after_two_unanswered_pings() {
    let mock = Mock::new();
    let (mut card, peer) =
        connected_pinging(&mock, new_state(), "PPA-QUIET", Duration::from_millis(100));
    // The peer reads nothing, so nothing answers the pings.
    let started = Instant::now();
    let mut again = mock.next();
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "lost only after two unanswered pings"
    );
    again.hello();
    assert_eq!(again.pair().as_deref(), Some("PPA-QUIET"));
    drop(peer);
    card.stop();
}

/// Pings answered (any frame counts): the link holds.
#[test]
fn answered_pings_keep_the_link() {
    let mock = Mock::new();
    let (mut card, mut peer) =
        connected_pinging(&mock, new_state(), "PPA-ALIVE", Duration::from_millis(100));
    let mut pings = 0;
    let until = Instant::now() + Duration::from_millis(800);
    while Instant::now() < until {
        // Reading answers each ping with a pong.
        if let Ok(Message::Ping(_)) = peer.ws.read() {
            pings += 1;
        }
    }
    assert!(pings >= 2, "pinged while quiet: {pings}");
    assert!(mock.conns.try_recv().is_err(), "no reconnect");
    card.settle();
    assert_eq!(card.view.status, ActorStatus::Connected);
    card.stop();
}

/// The card stops at once: while Pureplay's prompt is up, during a
/// WebSocket handshake a silent server never answers, and when it waits for
/// the user's Reconnect.
#[test]
fn the_card_stops_at_once() {
    const AT_ONCE: Duration = Duration::from_millis(1_000);

    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    card.shows_row(ActorStatus::Starting, "pairing", WAITING);
    let took = card.stop_timed();
    assert!(took < AT_ONCE, "during the prompt: {took:?}");

    // A server that accepts the connection and says nothing.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, accepted) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let _ = tx.send(());
            std::thread::sleep(Duration::from_secs(10));
            drop(stream);
        }
    });
    let mut card = Card::start(addr, new_state());
    accepted.recv_timeout(WAIT).expect("the card connects");
    std::thread::sleep(Duration::from_millis(100));
    let took = card.stop_timed();
    assert!(took < AT_ONCE, "during the handshake: {took:?}");

    let mock = Mock::new();
    let mut card = Card::start(mock.addr, new_state());
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair(), None);
    peer.refuse(Some(protocol::PAIR_ID), "unauthorised", &json!({}));
    peer.close(4403);
    card.shows_row(ActorStatus::Disconnected, "pairing", DENIED);
    let took = card.stop_timed();
    assert!(took < AT_ONCE, "waiting for Reconnect: {took:?}");
}

// -- reconfigure ---------------------------------------------------------------------

/// A changed address restarts the card; a changed name does not.
#[test]
fn only_an_address_change_restarts_the_card() {
    let state = new_state();
    let section = |name: &str, address: Option<&str>| flighthook::PureplayAccessorySection {
        name: name.into(),
        address: address.map(str::to_owned),
        ..flighthook::PureplayAccessorySection::default()
    };
    state.system.update(|c| {
        c.pureplay_accessory
            .insert("0".into(), section("Pureplay", None));
    });
    let (tx, _rx) = broadcast::channel(16);
    let sender = BusSender::new(ACTOR.into(), tx, Arc::new(AtomicBool::new(false)));
    let actor = PureplayAccessoryActor {
        addr: DEFAULT_ADDRESS.parse().expect("addr"),
    };
    assert!(matches!(
        actor.reconfigure(&state, &sender),
        ReconfigureOutcome::Applied
    ));
    state.system.update(|c| {
        c.pureplay_accessory
            .insert("0".into(), section("Range PC", Some("127.0.0.1:9770")));
    });
    assert!(
        matches!(
            actor.reconfigure(&state, &sender),
            ReconfigureOutcome::Applied
        ),
        "a name change"
    );
    state.system.update(|c| {
        c.pureplay_accessory
            .insert("0".into(), section("Range PC", Some("192.168.1.20:9770")));
    });
    assert!(
        matches!(
            actor.reconfigure(&state, &sender),
            ReconfigureOutcome::RestartRequired
        ),
        "an address change"
    );
    state.system.update(|c| {
        c.pureplay_accessory.clear();
    });
    assert!(
        matches!(
            actor.reconfigure(&state, &sender),
            ReconfigureOutcome::RestartRequired
        ),
        "removed"
    );
}

// -- Shots in (phase B) ------------------------------------------------------------

/// A mock simulator taking Open Connect: every message a client writes,
/// as JSON, on any connection.
struct OcHost {
    addr: SocketAddr,
    messages: mpsc::Receiver<Value>,
    accepted: mpsc::Receiver<()>,
}

impl OcHost {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, messages) = mpsc::channel();
        let (accepted_tx, accepted) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                if accepted_tx.send(()).is_err() {
                    return;
                }
                let tx = tx.clone();
                std::thread::spawn(move || {
                    use std::io::Read as _;
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                        let mut values =
                            serde_json::Deserializer::from_slice(&buf).into_iter::<Value>();
                        let mut used = 0;
                        while let Some(Ok(v)) = values.next() {
                            used = values.byte_offset();
                            if tx.send(v).is_err() {
                                return;
                            }
                        }
                        buf.drain(..used);
                    }
                });
            }
        });
        Self {
            addr,
            messages,
            accepted,
        }
    }

    /// Wait for the client's connection.
    fn accepts(&self) {
        self.accepted
            .recv_timeout(WAIT)
            .expect("the client connects");
    }

    /// Every message the clients wrote within `quiet`.
    fn within(&self, quiet: Duration) -> Vec<Value> {
        let until = Instant::now() + quiet;
        let mut out = Vec::new();
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match self.messages.recv_timeout(left) {
                Ok(v) => out.push(v),
                Err(_) => break,
            }
        }
        out
    }

    /// The next shot a client wrote.
    fn shot(&self) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .expect("a shot reaches the simulator");
            let v = self.messages.recv_timeout(left).expect("a message");
            if is_shot(&v) {
                return v;
            }
        }
    }
}

fn is_shot(v: &Value) -> bool {
    v["ShotDataOptions"]["ContainsBallData"] == true
}

/// An Open Connect client `actor` with `routing` dialing `addr`, on the bus
/// `tx`, recording its sends in the state's registry as the app's does.
/// Stops when the returned flag is set.
fn oc_client(
    state: &TestState,
    tx: &broadcast::Sender<BusMsg>,
    actor: &str,
    addr: SocketAddr,
    routing: crate::actors::gspro::GsProRouting,
) -> Arc<AtomicBool> {
    use crate::actors::backoff::Backoff;
    use crate::actors::gspro::{Registry, run_shared};
    let shutdown = Arc::new(AtomicBool::new(false));
    let sender = BusSender::new(actor.into(), tx.clone(), Arc::clone(&shutdown));
    let receiver = sender.subscribe();
    let registry = Registry {
        state: Some(Arc::clone(&state.state)),
        sent: Some(Arc::clone(&state.sent_shots)),
    };
    let clients = Arc::clone(&state.local_clients);
    std::thread::spawn(move || {
        run_shared(
            addr,
            routing,
            registry,
            Backoff::new(Duration::from_millis(20), Duration::from_millis(20)),
            &clients,
            &sender,
            receiver,
        );
    });
    shutdown
}

/// Stops the flags it holds when dropped.
struct Stops(Vec<Arc<AtomicBool>>);

impl Drop for Stops {
    fn drop(&mut self) {
        for s in &self.0 {
            s.store(true, Ordering::SeqCst);
        }
    }
}

/// A card paired and following the round on the bus `tx`.
fn connected_on(mock: &Mock, state: TestState, tx: &broadcast::Sender<BusMsg>) -> (Card, Peer) {
    state
        .pureplay_tokens
        .save(&token_key(mock.addr), &Token::new("PPA-SHOTS"), "client-1")
        .expect("save");
    let tokens = Arc::clone(&state.pureplay_tokens);
    let mut card = Card::start_on(mock.addr, state, tokens, PING_EVERY, tx);
    let mut peer = mock.next();
    peer.hello();
    assert_eq!(peer.pair().as_deref(), Some("PPA-SHOTS"));
    peer.paired("PPA-SHOTS");
    peer.subscribe(&round(), 1);
    card.shows_row(ActorStatus::Connected, "club", "7I");
    (card, peer)
}

/// Wait for each of `actors`' Connected status on the card's view of the
/// bus.
fn until_connected(card: &mut Card, actors: &[&str]) {
    let mut waiting: Vec<&str> = actors.to_vec();
    let deadline = Instant::now() + WAIT;
    while !waiting.is_empty() {
        assert!(Instant::now() < deadline, "not connected: {waiting:?}");
        if let Some(msg) = card.step()
            && matches!(
                msg.event,
                FlighthookEvent::ActorStatus {
                    status: ActorStatus::Connected,
                    ..
                }
            )
        {
            waiting.retain(|a| *a != msg.actor);
        }
    }
}

impl Card {
    /// Wait for the next bus event from `actor` that `pred` picks.
    fn bus_from(
        &mut self,
        actor: &str,
        pred: impl Fn(&FlighthookEvent) -> bool,
    ) -> FlighthookMessage {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "nothing such from {actor}");
            if let Some(msg) = self.step()
                && msg.actor == actor
                && pred(&msg.event)
            {
                return msg;
            }
        }
    }

    /// The card's own messages so far.
    fn own(&self) -> Vec<&FlighthookMessage> {
        self.sent.iter().filter(|m| m.actor == ACTOR).collect()
    }

    /// The shot keys the card triggered so far.
    fn triggers(&self) -> Vec<ShotKey> {
        self.own()
            .into_iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::ShotTrigger { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect()
    }

    /// The strokes the card put on the bus so far, with their shot ids.
    fn strokes(&self) -> Vec<(Option<String>, flightrelay::GameStroke)> {
        self.own()
            .into_iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::Stroke { shot_id, stroke } => {
                    Some((shot_id.clone(), (**stroke).clone()))
                }
                _ => None,
            })
            .collect()
    }
}

/// Put a measured shot from `actor` on the bus: trigger, launch, finished.
fn monitor_shot(tx: &broadcast::Sender<BusMsg>, actor: &str, key: &ShotKey) {
    let sender =
        BusSender::new(actor.into(), tx.clone(), Arc::default()).with_frp_id(Some(actor.into()));
    sender.frp(flightrelay::ShotTrigger::new(key.clone()));
    sender.frp(flightrelay::ShotLaunch {
        key: key.clone(),
        launch: flighthook::Launch {
            launch_speed: Some(flighthook::Velocity::MilesPerHour(134.0)),
            launch_elevation: Some(14.0),
            launch_azimuth: Some(0.0),
            backspin_rpm: Some(3000),
            sidespin_rpm: Some(1500),
            ..flighthook::Launch::default()
        },
    });
    sender.frp(flightrelay::ShotFinished { key: key.clone() });
}

/// Pureplay's `shot.launched` for `player`.
fn launched(shot_id: &str, player: i64) -> Value {
    json!({ "shotId": shot_id, "playerId": player,
            "ballSpeedMs": 59.9, "launchAngleDeg": 14.0, "directionDeg": 0.0,
            "backSpinRpm": 3000, "sideSpinRpm": 1500 })
}

/// Pureplay's `shot.completed` with `outcome`.
fn completed(shot_id: &str, player: i64, outcome: &str) -> Value {
    json!({ "shotId": shot_id, "matchId": "m-1", "playerId": player,
            "holeNumber": 7, "strokeNumber": 1, "outcome": outcome,
            "penaltyStrokes": 0, "isPutt": false,
            "carryM": 210.4, "totalM": 230.1, "apexM": 30.5,
            "startDistToHoleM": 370.0, "endDistToHoleM": 140.2,
            "start": [0.0, 0.0, 0.0], "end": [230.0, 3.0, 0.0],
            "lie": "Fairway", "holeStrokes": 1 })
}

/// The shots a web server that took `sent` off the bus stores.
fn stored(state: &Arc<SystemState>, sent: &[BusMsg]) -> Vec<flighthook::ShotData> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let web = crate::actors::web::tests::folded(Arc::clone(state), sent).await;
        let shots = web.shots.read().await;
        shots.iter().map(|s| s.data.clone()).collect()
    })
}

/// Set the card's Shots setting.
fn set_shots(state: &Arc<SystemState>, shots: PureplayShots) {
    state.system.update(|c| {
        c.pureplay_accessory.insert(
            "0".into(),
            flighthook::PureplayAccessorySection {
                shots,
                ..flighthook::PureplayAccessorySection::default()
            },
        );
    });
}

/// The launch monitor's shot goes to Pureplay through the Open
/// Connect output; Pureplay reports it back. In Auto the echo is not
/// stored, and Pureplay's result lands on the launch monitor's shot.
#[test]
fn an_echo_is_not_stored_and_gets_the_stroke() {
    let mock = Mock::new();
    let state = new_state();
    let (tx, _) = broadcast::channel(4096);
    let (mut card, mut peer) = connected_on(&mock, state.clone(), &tx);
    let oc = OcHost::new();
    let _stops = Stops(vec![oc_client(
        &state,
        &tx,
        "openconnect_client.0",
        oc.addr,
        Default::default(),
    )]);
    oc.accepts();
    until_connected(&mut card, &["openconnect_client.0"]);

    let mevo = ShotKey::new("mevo-shot-1", 41);
    monitor_shot(&tx, "mevo.0", &mevo);
    let sent = oc.shot();
    assert_eq!(sent["BallData"]["BackSpin"], 3000.0, "{sent}");

    peer.event("shot.launched", 2, &launched("pp-1", 1));
    peer.event("shot.completed", 3, &completed("pp-1", 1, "Green"));
    let msg = card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::Stroke { .. }));
    let FlighthookEvent::Stroke { shot_id, stroke } = &msg.event else {
        unreachable!()
    };
    assert_eq!(
        shot_id.as_deref(),
        Some(mevo.shot_id.as_str()),
        "the launch monitor's shot"
    );
    assert_eq!(stroke.outcome, flightrelay::StrokeOutcome::Green);
    assert_eq!(
        stroke.flight.as_ref().and_then(|f| f.carry_distance),
        Some(flighthook::Distance::Meters(210.4))
    );
    assert_eq!(stroke.golfer.as_str(), "pureplay.Joe");
    card.shows_row(
        ActorStatus::Connected,
        "shots",
        "auto: last from flighthook",
    );
    card.settle();
    assert!(card.triggers().is_empty(), "the echo is not taken");

    let shots = stored(&state, &card.raw);
    assert_eq!(shots.len(), 1, "{shots:?}");
    assert_eq!(shots[0].card_key(), "mevo.0");
    assert_eq!(
        shots[0].stroke.as_ref().map(|s| s.outcome),
        Some(flightrelay::StrokeOutcome::Green)
    );
    assert!(
        oc.within(Duration::from_millis(200))
            .iter()
            .all(|v| !is_shot(v))
    );
    card.stop();
}

/// A local player's shot in Pureplay is stored as the card's own
/// shot and gets Pureplay's result; it reaches neither the Open Connect
/// output on "Any" nor one routed to the card by ID.
#[test]
fn a_pureplay_shot_is_stored_and_reaches_no_output() {
    let mock = Mock::new();
    let state = new_state();
    let (tx, _) = broadcast::channel(4096);
    let (mut card, mut peer) = connected_on(&mock, state.clone(), &tx);
    let any = OcHost::new();
    let by_id = OcHost::new();
    let routed = crate::actors::gspro::GsProRouting {
        full_monitor: Some(ACTOR.into()),
        chipping_monitor: Some(ACTOR.into()),
        putting_monitor: Some(ACTOR.into()),
    };
    let _stops = Stops(vec![
        oc_client(
            &state,
            &tx,
            "openconnect_client.0",
            any.addr,
            Default::default(),
        ),
        oc_client(&state, &tx, "openconnect_client.1", by_id.addr, routed),
    ]);
    any.accepts();
    by_id.accepts();
    until_connected(&mut card, &["openconnect_client.0", "openconnect_client.1"]);

    peer.event("shot.launched", 2, &launched("pp-2", 1));
    let trigger = card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::ShotTrigger { .. }));
    let FlighthookEvent::ShotTrigger { key } = &trigger.event else {
        unreachable!()
    };
    assert_eq!(key.shot_number, 1, "the card's own count");
    assert_ne!(key.shot_id, "pp-2", "flighthook's own id");
    assert_eq!(
        state.round.source(ACTOR).map(ShotSource::provenance),
        Some(flighthook::Source::Virtual)
    );
    let key = key.clone();
    card.bus_from(ACTOR, |e| matches!(e, FlighthookEvent::ShotFinished { .. }));
    peer.event("shot.completed", 3, &completed("pp-2", 1, "OB"));
    let stroke = card.bus_from(ACTOR, |e| stroke_for(e, &key));
    assert!(matches!(&stroke.event,
        FlighthookEvent::Stroke { stroke, .. }
            if stroke.outcome == flightrelay::StrokeOutcome::OutOfBounds));
    card.shows(ActorStatus::Connected, |t| {
        t.get("shots").map(String::as_str) == Some("auto: last from Pureplay")
            && t.get("last_shot").is_some_and(|v| v.starts_with("#1 at "))
    });

    for host in [&any, &by_id] {
        let wrote = host.within(Duration::from_millis(400));
        assert!(wrote.iter().all(|v| !is_shot(v)), "{wrote:?}");
    }
    card.settle();
    let shots = stored(&state, &card.raw);
    assert_eq!(shots.len(), 1, "{shots:?}");
    assert_eq!(shots[0].card_key(), ACTOR);
    assert_eq!(shots[0].shot_id.as_deref(), Some(key.shot_id.as_str()));
    assert_eq!(shots[0].source, Some(flighthook::Source::Virtual));
    let launch = shots[0].launch.as_ref().expect("launch");
    assert_eq!(launch.backspin_rpm, Some(3000));
    assert_eq!(launch.sidespin_rpm, Some(1500));
    assert_eq!(
        shots[0].stroke.as_ref().map(|s| s.outcome),
        Some(flightrelay::StrokeOutcome::OutOfBounds)
    );
    // The export names the source and carries the stroke, never the
    // token or the card's address.
    let mut config = state.system.snapshot();
    config.pureplay_accessory.insert(
        "0".into(),
        flighthook::PureplayAccessorySection {
            address: Some(mock.addr.to_string()),
            ..flighthook::PureplayAccessorySection::default()
        },
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let lines = rt.block_on(crate::actors::web::tests::export_lines(&config, &card.raw));
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).expect("json");
    assert_eq!(line["device"]["kind"], "pureplay");
    assert_eq!(line["device"]["manufacturer"], "Pureplay");
    assert_eq!(line["data"]["stroke"]["outcome"], "out_of_bounds");
    assert!(!lines[0].contains("PPA-SHOTS"), "{}", lines[0]);
    assert!(!lines[0].contains(&mock.addr.to_string()), "{}", lines[0]);
    card.stop();
}

/// The card's readiness is its `bus.actor` without blockers; an Open Connect
/// output on "Any" or routed to the card by ID never reports it to the
/// simulator. A launch monitor's still does.
#[test]
fn the_cards_ready_never_reaches_an_output() {
    let mock = Mock::new();
    let state = new_state();
    let (tx, _) = broadcast::channel(4096);
    let (mut card, _peer) = connected_on(&mock, state.clone(), &tx);
    card.bus_from(ACTOR, |e| {
        matches!(e, FlighthookEvent::BusActor(a)
            if actor_ready(a)
                && a.device.as_ref().and_then(|d| d.manufacturer.as_deref()) == Some("Pureplay")
                && a.device.as_ref().and_then(|d| d.model.as_deref()) == Some("Accessory API"))
    });
    let any = OcHost::new();
    let by_id = OcHost::new();
    let routed = crate::actors::gspro::GsProRouting {
        full_monitor: Some(ACTOR.into()),
        ..Default::default()
    };
    let _stops = Stops(vec![
        oc_client(
            &state,
            &tx,
            "openconnect_client.0",
            any.addr,
            Default::default(),
        ),
        oc_client(&state, &tx, "openconnect_client.1", by_id.addr, routed),
    ]);
    any.accepts();
    by_id.accepts();
    // The card says it is ready again while both outputs run.
    let card_sender =
        BusSender::new(ACTOR.into(), tx.clone(), Arc::default()).with_frp_id(Some(ACTOR.into()));
    card_sender.frp(
        flightrelay::BusActor::new("Pureplay", 1.0)
            .with_role(roles::GAME)
            .with_role(roles::LAUNCH_MONITOR),
    );
    let heartbeats = |host: &OcHost| -> Vec<Value> {
        host.within(Duration::from_millis(1500))
            .into_iter()
            .filter(|v| v["ShotDataOptions"]["ContainsBallData"] == false)
            .collect()
    };
    for host in [&any, &by_id] {
        let beats = heartbeats(host);
        assert!(!beats.is_empty(), "a heartbeat");
        assert!(
            beats
                .iter()
                .all(|v| v["ShotDataOptions"]["LaunchMonitorIsReady"] == false),
            "{beats:?}"
        );
    }
    // A launch monitor's readiness reaches the output on "Any" as before.
    let monitor = BusSender::new("mevo.0".into(), tx.clone(), Arc::default())
        .with_frp_id(Some("mevo.0".into()));
    monitor.frp(flightrelay::BusActor::new("Mevo+", 1.0).with_role(roles::LAUNCH_MONITOR));
    let beats = any.within(Duration::from_millis(500));
    assert!(
        beats
            .iter()
            .any(|v| v["ShotDataOptions"]["LaunchMonitorIsReady"] == true),
        "{beats:?}"
    );
    card.stop();
}

/// A shot of a player at another machine is never stored; its stroke
/// names no shot.
#[test]
fn a_remote_players_shot_is_ignored() {
    let mock = Mock::new();
    let state = new_state();
    let (mut card, mut peer) = connected(&mock, state.clone(), "PPA-REMOTE");
    peer.event("shot.launched", 2, &launched("pp-r", 3));
    peer.event("shot.completed", 3, &completed("pp-r", 3, "Green"));
    // A local shot after it is taken, so the card read the two above.
    peer.event("shot.launched", 4, &launched("pp-l", 1));
    card.bus(|e| matches!(e, FlighthookEvent::ShotTrigger { .. }));
    card.settle();
    assert_eq!(card.triggers().len(), 1);
    let strokes = card.strokes();
    assert_eq!(strokes.len(), 1, "{strokes:?}");
    assert_eq!(strokes[0].0, None);
    assert_eq!(strokes[0].1.golfer.as_str(), "pureplay.Remote");
    card.stop();
}

/// `Gimme`, `AutoPutt` and `PenaltyOnly` have no launch: nothing is stored
/// and their strokes name no shot.
#[test]
fn launchless_outcomes_store_nothing() {
    let mock = Mock::new();
    let state = new_state();
    let (mut card, mut peer) = connected(&mock, state.clone(), "PPA-GIMME");
    let mut seq = 2;
    for (id, outcome) in [
        ("pp-g", "Gimme"),
        ("pp-a", "AutoPutt"),
        ("pp-p", "PenaltyOnly"),
    ] {
        peer.event("shot.completed", seq, &completed(id, 1, outcome));
        seq += 1;
    }
    // A stored shot's result after them, so the card read the three above.
    peer.event("shot.launched", seq, &launched("pp-x", 1));
    peer.event("shot.completed", seq + 1, &completed("pp-x", 1, "Holed"));
    card.bus(|e| {
        matches!(
            e,
            FlighthookEvent::Stroke {
                shot_id: Some(_),
                ..
            }
        )
    });
    card.settle();
    assert_eq!(card.triggers().len(), 1);
    let strokes: Vec<_> = card
        .strokes()
        .into_iter()
        .map(|(id, s)| (id.is_some(), s.outcome))
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
    let shots = stored(&state, &card.raw);
    assert_eq!(shots.len(), 1);
    card.stop();
}

/// The window cases: a send older than the window is no echo (the shot is
/// taken); one send is the echo of one shot, and a second shot inside the
/// window with no send left is taken; a discarded shot keeps its stored
/// shot; a result after the discard sends no stroke, and a stroke sent
/// before it is not retracted.
#[test]
fn the_echo_window_cases() {
    use crate::state::sent_shots::ECHO_WINDOW;
    let mock = Mock::new();
    let state = new_state();
    let (mut card, mut peer) = connected(&mock, state.clone(), "PPA-WINDOW");
    let peer_addr = SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 921)));

    // A send with no echo, older than the window.
    let stale = Instant::now()
        .checked_sub(ECHO_WINDOW + Duration::from_millis(10))
        .expect("an instant");
    state
        .sent_shots
        .record(ShotKey::new("old", 1), peer_addr, stale);
    peer.event("shot.launched", 2, &launched("pp-1", 1));
    card.bus(|e| matches!(e, FlighthookEvent::ShotTrigger { .. }));
    assert_eq!(state.sent_shots.len(), 0, "expired");

    // One send, two shots: the first is the echo, the second is taken.
    let sent = ShotKey::new("sent-2", 7);
    state
        .sent_shots
        .record(sent.clone(), peer_addr, Instant::now());
    peer.event("shot.launched", 3, &launched("pp-2", 1));
    peer.event("shot.launched", 4, &launched("pp-3", 1));
    peer.event("shot.completed", 5, &completed("pp-2", 1, "Green"));
    card.bus(|e| stroke_for(e, &sent));
    card.settle();
    assert_eq!(card.triggers().len(), 2, "pp-1 and pp-3");

    // Scramble shots the team did not pick: pp-3 discarded before its
    // result, pp-1 after it.
    let discarded = |id: &str| json!({ "shotId": id, "playerId": 1, "reason": "discarded" });
    peer.event("shot.cancelled", 6, &discarded("pp-3"));
    peer.event("shot.completed", 7, &completed("pp-3", 1, "InPlay"));
    peer.event("shot.completed", 8, &completed("pp-1", 1, "Water"));
    peer.event("shot.cancelled", 9, &discarded("pp-1"));
    let taken = card.triggers();
    let first = taken[0].clone();
    card.bus(|e| stroke_for(e, &first));
    // A local shot after them, so the card read the discard of pp-1.
    peer.event("shot.launched", 10, &launched("pp-4", 1));
    card.bus(|e| matches!(e, FlighthookEvent::ShotTrigger { key } if key.shot_number == 3));
    card.settle();
    let kinds: Vec<_> = card.strokes().iter().map(|(_, o)| o.outcome).collect();
    assert_eq!(
        kinds,
        [
            flightrelay::StrokeOutcome::Green,
            flightrelay::StrokeOutcome::Water
        ],
        "no stroke for pp-3's result after its discard"
    );
    let shots = stored(&state, &card.raw);
    assert_eq!(shots.len(), 3, "the discarded shots stay");
    let water = shots
        .iter()
        .find(|s| s.shot_id.as_deref() == Some(first.shot_id.as_str()))
        .expect("pp-1");
    assert_eq!(
        water.stroke.as_ref().map(|s| s.outcome),
        Some(flightrelay::StrokeOutcome::Water),
        "not retracted"
    );
    assert!(
        shots
            .iter()
            .filter(|s| s.shot_id.as_deref() != Some(first.shot_id.as_str()))
            .all(|s| s.stroke.is_none())
    );
    card.stop();
}

/// A `shot.completed` after a reconnect to the same Pureplay still finds
/// its shot; after Pureplay restarted (a new `serverInstanceId`) the old
/// shot ids are forgotten.
#[test]
fn a_result_after_a_reconnect_to_the_same_pureplay_attaches() {
    let mock = Mock::new();
    let state = new_state();
    let (mut card, mut peer) = connected(&mock, state.clone(), "PPA-AGAIN");
    peer.event("shot.launched", 2, &launched("pp-1", 1));
    let trigger = card.bus(|e| matches!(e, FlighthookEvent::ShotTrigger { .. }));
    let FlighthookEvent::ShotTrigger { key, .. } = trigger.event else {
        unreachable!()
    };
    peer.event("shot.launched", 3, &launched("pp-2", 1));
    card.bus(|e| matches!(e, FlighthookEvent::ShotTrigger { .. }));
    drop(peer);

    let mut peer = mock.next();
    peer.hello_from("s-1");
    assert!(peer.pair().is_some());
    peer.paired("PPA-AGAIN");
    peer.subscribe(&round(), 1);
    card.shows(ActorStatus::Connected, |t| t.contains_key("club"));
    peer.event("shot.completed", 2, &completed("pp-1", 1, "Green"));
    card.bus(|e| stroke_for(e, &key));
    let key_1 = key.clone();
    drop(peer);

    let mut peer = mock.next();
    peer.hello_from("s-2");
    assert!(peer.pair().is_some());
    peer.paired("PPA-AGAIN");
    peer.subscribe(&round(), 1);
    card.shows(ActorStatus::Connected, |t| t.contains_key("club"));
    peer.event("shot.completed", 2, &completed("pp-2", 1, "Green"));
    // A shot of the new instance, so the card read the result above.
    peer.event("shot.launched", 3, &launched("pp-9", 1));
    let trigger = card.bus(|e| matches!(e, FlighthookEvent::ShotTrigger { .. }));
    let FlighthookEvent::ShotTrigger { key, .. } = trigger.event else {
        unreachable!()
    };
    assert_eq!(key.shot_number, 3, "the count goes on across reconnects");
    card.settle();
    let ids: Vec<_> = card.strokes().into_iter().map(|(id, _)| id).collect();
    assert_eq!(
        ids,
        [Some(key_1.shot_id.clone()), None],
        "pp-2 is forgotten"
    );
    card.stop();
}

/// "From flighthook" never takes a shot, and still puts Pureplay's result
/// on the shot flighthook sent; "From Pureplay" takes every local shot,
/// a send in the window included, with no matching.
#[test]
fn from_flighthook_never_takes_and_from_pureplay_never_matches() {
    let mock = Mock::new();
    let state = new_state();
    let peer_addr = SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 921)));
    set_shots(&state, PureplayShots::FromFlighthook);
    let (mut card, mut peer) = connected(&mock, state.clone(), "PPA-MODES");
    card.shows_row(ActorStatus::Connected, "shots", "from flighthook");
    let sent = ShotKey::new("sent-1", 5);
    state
        .sent_shots
        .record(sent.clone(), peer_addr, Instant::now());
    peer.event("shot.launched", 2, &launched("pp-1", 1));
    peer.event("shot.launched", 3, &launched("pp-2", 1));
    peer.event("shot.completed", 4, &completed("pp-1", 1, "Green"));
    card.bus(|e| stroke_for(e, &sent));
    card.settle();
    assert!(card.triggers().is_empty(), "nothing taken");

    set_shots(&state, PureplayShots::FromPureplay);
    state
        .sent_shots
        .record(ShotKey::new("sent-2", 6), peer_addr, Instant::now());
    peer.event("shot.launched", 5, &launched("pp-3", 1));
    card.bus(|e| matches!(e, FlighthookEvent::ShotTrigger { .. }));
    card.shows_row(ActorStatus::Connected, "shots", "from Pureplay");
    assert_eq!(state.sent_shots.len(), 1, "no matching: the send is left");
    card.stop();
}

/// A configured, connected card is a launch monitor, yet not one of the
/// launch monitors a Pureplay Connect output names (in Hello, for its
/// battery, in a round request). The output's own strict-mode test is in
/// its module.
#[test]
fn the_connected_card_is_no_feed_of_a_pureplay_connect_output() {
    let mock = Mock::new();
    let state = new_state();
    state.system.update(|c| {
        c.pureplay_accessory
            .insert("0".into(), flighthook::PureplayAccessorySection::default());
    });
    let (mut card, _peer) = connected(&mock, state.clone(), "PPA-STRICT");
    card.bus(|e| matches!(e, FlighthookEvent::BusActor(_)));
    assert!(state.round.is_launch_monitor(ACTOR));
    let names = crate::actors::pureplay::monitor_names(&state);
    assert!(!names.contains_key(ACTOR), "{names:?}");
    card.stop();
}

/// One shot sent by two outputs (Open Connect and Pureplay Connect) is
/// recorded twice; its echo takes both records, so the next shot's echo
/// and result land on the next shot, not on the first one's leftover.
#[test]
fn a_shot_two_outputs_sent_is_echoed_once() {
    let mock = Mock::new();
    let state = new_state();
    let (mut card, mut peer) = connected(&mock, state.clone(), "PPA-TWO");
    let oc = SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 921)));
    let ppc = SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 9760)));
    let a = ShotKey::new("shot-a", 1);
    let b = ShotKey::new("shot-b", 2);
    state.sent_shots.record(a.clone(), oc, Instant::now());
    state.sent_shots.record(a.clone(), ppc, Instant::now());
    peer.event("shot.launched", 2, &launched("pp-a", 1));
    peer.event("shot.completed", 3, &completed("pp-a", 1, "Green"));
    card.bus(|e| stroke_for(e, &a));
    assert_eq!(state.sent_shots.len(), 0, "both records of shot a");
    state.sent_shots.record(b.clone(), oc, Instant::now());
    state.sent_shots.record(b.clone(), ppc, Instant::now());
    peer.event("shot.launched", 4, &launched("pp-b", 1));
    peer.event("shot.completed", 5, &completed("pp-b", 1, "Water"));
    let msg = card.bus(|e| {
        matches!(e, FlighthookEvent::Stroke { stroke, .. }
            if stroke.outcome == flightrelay::StrokeOutcome::Water)
    });
    let FlighthookEvent::Stroke { shot_id, .. } = &msg.event else {
        unreachable!()
    };
    assert_eq!(
        shot_id.as_deref(),
        Some(b.shot_id.as_str()),
        "shot b's result on shot b"
    );
    assert_eq!(state.sent_shots.len(), 0);
    card.settle();
    assert!(card.triggers().is_empty(), "both were echoes");
    card.stop();
}

/// The `shots` row follows the Shots setting while Pureplay sends
/// nothing.
#[test]
fn the_shots_row_follows_the_setting_while_pureplay_is_quiet() {
    let mock = Mock::new();
    let state = new_state();
    let (mut card, _peer) = connected(&mock, state.clone(), "PPA-QUIET");
    card.settle();
    set_shots(&state, PureplayShots::FromPureplay);
    card.shows_row(ActorStatus::Connected, "shots", "from Pureplay");
    set_shots(&state, PureplayShots::FromFlighthook);
    card.shows_row(ActorStatus::Connected, "shots", "from flighthook");
    card.stop();
}

/// A `shot.launched` repeating a `shotId` the card already holds is not
/// taken again and consumes no send: the next shot is still the echo of
/// the send.
#[test]
fn a_repeated_launch_is_not_taken_again() {
    let mock = Mock::new();
    let state = new_state();
    let (mut card, mut peer) = connected(&mock, state.clone(), "PPA-REPEAT");
    peer.event("shot.launched", 2, &launched("pp-1", 1));
    card.bus(|e| matches!(e, FlighthookEvent::ShotFinished { .. }));
    let sent = ShotKey::new("sent-1", 5);
    state.sent_shots.record(
        sent.clone(),
        SimAddr::of(None, SocketAddr::from(([127, 0, 0, 1], 921))),
        Instant::now(),
    );
    peer.event("shot.launched", 3, &launched("pp-1", 1));
    peer.event("shot.launched", 4, &launched("pp-2", 1));
    peer.event("shot.completed", 5, &completed("pp-2", 1, "Green"));
    card.bus(|e| stroke_for(e, &sent));
    card.settle();
    assert_eq!(card.triggers().len(), 1, "pp-1 once; pp-2 the echo");
    card.stop();
}

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
