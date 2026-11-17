//! The Accessory driver against a fake Pureplay on the driver test host:
//! one thread, virtual time, a fake [`Link`], no sockets (one real-socket
//! smoke test of [`WsLink`] at the end).
//!
//! [`Rig`] stands in for both sides of the card: Pureplay's end of the link
//! (`hello`, pairing, refusals, close codes, the snapshot, events), and the
//! host around the driver as flighthook runs it: the card's status
//! (Starting, Connected, Reconnecting, Disconnected) and rows as a dashboard
//! shows them, the retry delays the driver asks for, the turn handed to a
//! new link, and flighthook's session, which answers every `golfer.select`
//! with its new turn and a `bus.reply`.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::rc::Rc;

use flightrelay::driver::DeviceEvent;
use flightrelay::testing::TestHost;
use flightrelay::{
    BusReply, BusSync, Club, FrpBody, GameAim, GolferId, GolferSelect, GolferTurn, Handedness,
    Mode, ReplyError, ShotKey,
};
use serde_json::{Value, json};

use super::*;
use crate::accessory::token::{self, TokenStore};

#[path = "tests/card.rs"]
mod card;
#[path = "tests/club.rs"]
mod club;
#[path = "tests/game.rs"]
mod game;

/// The card's address.
const ADDR: &str = "127.0.0.1:9770";
/// flighthook's actor id, the sender of its turns and replies.
const FLIGHTHOOK: &str = "flighthook";
/// Longest a test steps the card waiting for one thing (virtual time).
const WAIT: Duration = Duration::from_secs(120);

// -- The fake link ---------------------------------------------------------------

/// Pureplay's end of one link.
#[derive(Debug, Default)]
struct Pipe {
    /// What Pureplay sent, not read by the card yet; `Err` ends the link
    /// without a close frame.
    to_card: VecDeque<io::Result<Frame>>,
    /// What the card sent, not taken by the test yet.
    from_card: VecDeque<Value>,
    /// The card closed the link.
    closed: bool,
    /// Pings the card sent, and whether each gets a pong.
    pings: u32,
    pong: bool,
}

/// The card's end of a [`Pipe`].
#[derive(Debug)]
struct FakeLink(Rc<RefCell<Pipe>>);

impl Link for FakeLink {
    fn send(&mut self, text: &str) -> io::Result<()> {
        let mut pipe = self.0.borrow_mut();
        if pipe.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        pipe.from_card
            .push_back(serde_json::from_str(text).expect("the card sends JSON"));
        Ok(())
    }

    fn recv(&mut self) -> io::Result<Option<Frame>> {
        self.0.borrow_mut().to_card.pop_front().transpose()
    }

    fn ping(&mut self) -> io::Result<()> {
        let mut pipe = self.0.borrow_mut();
        pipe.pings += 1;
        if pipe.pong {
            pipe.to_card.push_back(Ok(Frame::Control));
        }
        Ok(())
    }

    fn close(&mut self) {
        self.0.borrow_mut().closed = true;
    }
}

// -- The card's status, as the host shows it --------------------------------------

/// The card's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Starting,
    Connected,
    Reconnecting,
    Disconnected,
}

/// What the card did, in order. One per event, taken apart at once: not
/// worth a box.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
enum Ev {
    /// A message on the bus.
    Frp(FrpBody),
    /// The card's status (blockers and rows), as the driver sent it.
    Status(DeviceStatus),
}

/// A token store in a folder of its own, removed when dropped.
struct Folder(std::path::PathBuf);

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The card, its link, and the host around it. See the [module docs](self).
struct Rig {
    host: TestHost,
    driver: Accessory,
    pipe: Rc<RefCell<Pipe>>,
    session: Option<Session<FakeLink>>,
    /// The link's session became Connected.
    connected_at: Option<Instant>,
    status: Status,
    /// The card's rows as a dashboard shows them.
    view: BTreeMap<String, String>,
    /// The device status the host holds.
    device: DeviceStatus,
    /// Everything the card did.
    record: Vec<Ev>,
    /// Records not yet taken by [`Rig::bus`].
    queued: VecDeque<Ev>,
    /// The retry delay the driver asked for last, waited at the next
    /// connect; `None` once it gave up.
    delay: Option<Duration>,
    /// Connects that failed since the last session.
    attempts: u32,
    /// The host shows the last failed connect's error.
    connect_error: bool,
    /// flighthook's turn.
    turn: GolferTurn,
    /// Selects the test made.
    picks: u32,
    /// Every turn flighthook's session published, in order.
    turns: Vec<GolferTurn>,
    /// Every log line the driver wrote.
    logs: Vec<String>,
    /// flighthook's session answers the card's selects.
    answers: bool,
    /// The `seq` of Pureplay's last event sent with [`Rig::ev`].
    seq: u64,
    tokens: Arc<TokenStore>,
    _folder: Folder,
}

impl Rig {
    /// A card at [`ADDR`] with an empty token file, flighthook's turn the
    /// start turn (no golfer, no club, right-handed, full).
    fn new() -> Self {
        let folder = std::env::temp_dir().join(format!(
            "flighthook-pp-accessory-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let file = folder.join(token::FILE_NAME);
        let driver = Accessory::new(Config {
            address: Some(ADDR.into()),
            token_file: Some(file.clone()),
        })
        .expect("config");
        let host = TestHost::new();
        host.flags.set_wants_raw(true);
        host.flags.set_min_level(Level::Trace);
        Self {
            host,
            driver,
            pipe: Rc::default(),
            session: None,
            connected_at: None,
            status: Status::Starting,
            view: BTreeMap::new(),
            device: DeviceStatus::not_armed(),
            record: Vec::new(),
            queued: VecDeque::new(),
            delay: Some(Duration::ZERO),
            attempts: 0,
            connect_error: false,
            turn: GolferTurn::new(None, None, Handedness::Right, Mode::Full),
            picks: 0,
            turns: Vec::new(),
            logs: Vec::new(),
            answers: true,
            seq: 1,
            tokens: Arc::new(TokenStore::new(file)),
            _folder: Folder(folder),
        }
    }

    /// A card whose token file holds `token` for its address.
    fn with_token(token: &str) -> Self {
        let rig = Self::new();
        rig.save_token(token);
        rig
    }

    fn save_token(&self, token: &str) {
        self.tokens
            .save(&self.key(), &Token::new(token), "client-1")
            .expect("save");
    }

    /// The card's token file key.
    fn key(&self) -> String {
        token::key(ADDR.parse().expect("addr"))
    }

    /// The token saved for the card.
    fn token(&self) -> Option<Token> {
        self.tokens.get(&self.key())
    }

    // -- The host --------------------------------------------------------------

    /// Take what the driver emitted: the record, the status and rows, and
    /// flighthook's answer to each `golfer.select`.
    fn take_events(&mut self) {
        for event in self.host.take() {
            match event {
                DeviceEvent::Frp(body) => {
                    if let FrpBody::GolferSelect(select) = &body
                        && self.answers
                    {
                        self.answer(select);
                    }
                    self.push(Ev::Frp(body));
                }
                DeviceEvent::Status(status) => self.device_status(status),
                DeviceEvent::StatusChange(change) => {
                    let mut status = self.device.clone();
                    status.apply(change);
                    self.device_status(status);
                }
                DeviceEvent::Log(_, line) => self.logs.push(line),
                _ => {}
            }
        }
    }

    fn push(&mut self, ev: Ev) {
        self.record.push(ev.clone());
        self.queued.push_back(ev);
    }

    /// The driver's status: its rows on the card (`ready` too, while
    /// connected).
    fn device_status(&mut self, status: DeviceStatus) {
        for (k, v) in &status.telemetry {
            self.view.insert(k.clone(), v.clone());
        }
        for k in self.device.telemetry.keys() {
            if !status.telemetry.contains_key(k) {
                self.view.remove(k);
            }
        }
        if self.status == Status::Connected {
            self.view
                .insert("ready".into(), status.blockers.is_empty().to_string());
        }
        self.device = status.clone();
        self.push(Ev::Status(status));
    }

    /// The host's status `status`: Starting and the end of a session clear
    /// the rows, as a dashboard does.
    fn set_status(&mut self, status: Status) {
        if status == Status::Starting
            || (self.status == Status::Connected
                && matches!(status, Status::Reconnecting | Status::Disconnected))
        {
            self.view.clear();
        }
        self.status = status;
    }

    /// flighthook's session answers `select`: its new turn (when it
    /// changed), then the reply.
    fn answer(&mut self, select: &GolferSelect) {
        let mut turn = self.turn.clone();
        if let Some(golfer) = &select.golfer {
            turn.golfer.clone_from(golfer);
        }
        if let Some(club) = select.club {
            turn.club = club;
        }
        if let Some(hand) = select.handedness {
            turn.handedness = Some(hand);
        }
        if let Some(mode) = select.mode {
            turn.mode = Some(mode);
        }
        if turn != self.turn {
            self.turn = turn.clone();
            self.turns.push(turn.clone());
            self.hand(turn);
        }
        self.hand(BusReply::ok("x", "golfer.select", select.r#ref.clone()));
    }

    /// Hand the link a message from flighthook.
    fn hand(&mut self, body: impl Into<FrpBody>) {
        self.hand_from(FLIGHTHOOK, body);
    }

    /// Hand the link a message from `from`.
    fn hand_from(&mut self, from: &str, body: impl Into<FrpBody>) {
        let msg = FrpMessage::new(from, body);
        if let Some(session) = &mut self.session {
            self.host.bus(session, &msg).expect("handed");
        }
    }

    /// The link ended with `error` (or the card gave up connecting): the
    /// host's status and the driver's retry delay.
    fn ended(&mut self, error: Error) {
        let now = self.host.now();
        let connected_for = self.connected_at.take().map(|t| now - t);
        self.session = None;
        if connected_for.is_some() {
            self.attempts = 0;
            self.device = DeviceStatus::not_armed();
            self.set_status(Status::Reconnecting);
        } else {
            self.attempts += 1;
            self.connect_error = true;
            self.view.insert("error".into(), error.to_string());
        }
        let ended = SessionEnd::new(&error, self.host.now(), connected_for, self.attempts);
        let mut backoff = HostBackoff::default();
        let driver = &mut self.driver;
        self.delay = self
            .host
            .with(|_, out| driver.retry_delay(&ended, &mut backoff, out));
        self.take_events();
        if self.delay.is_none() {
            self.set_status(Status::Disconnected);
            self.view.insert("error".into(), error.to_string());
        }
    }

    /// The card's next connect: the retry delay goes by, then the connect
    /// starts (the rows, the cooldown) and Pureplay accepts the WebSocket.
    ///
    /// # Panics
    ///
    /// When the card gave up and waits for Reconnect.
    fn next(&mut self) {
        assert!(self.session.is_none(), "a link is up");
        let delay = self.delay.expect("the card waits for Reconnect");
        self.host.advance(delay);
        let driver = &mut self.driver;
        let begun = self.host.connect_with(|ctx| driver.before_open(ctx));
        self.take_events();
        if let Err(e) = begun {
            self.ended(e);
            return;
        }
        self.pipe = Rc::default();
        let card = Arc::clone(&self.driver.card);
        self.session = Some(Session::new(
            FakeLink(Rc::clone(&self.pipe)),
            card,
            ADDR.parse().expect("addr"),
        ));
    }

    /// The user's Reconnect: the host starts the card over, a new driver on
    /// the same token file, and shows it Starting.
    fn reconnect(&mut self) {
        assert!(self.session.is_none(), "a link is up");
        self.driver = Accessory::new(Config {
            address: Some(ADDR.into()),
            token_file: Some(self.tokens.path().to_path_buf()),
        })
        .expect("config");
        self.delay = Some(Duration::ZERO);
        self.attempts = 0;
        self.connect_error = false;
        self.device = DeviceStatus::not_armed();
        self.set_status(Status::Starting);
    }

    /// The card's connect fails: Pureplay is unreachable.
    fn unreachable(&mut self) {
        assert!(self.session.is_none(), "a link is up");
        let delay = self.delay.expect("the card waits for Reconnect");
        self.host.advance(delay);
        let driver = &mut self.driver;
        let begun = self.host.connect_with(|ctx| driver.before_open(ctx));
        self.take_events();
        self.ended(begun.err().unwrap_or_else(|| {
            Error::Lost(format!("no connection to {ADDR}: connection refused"))
        }));
    }

    /// One step of the link at the host's time; `false` when there is no
    /// link or it ended.
    fn step(&mut self) -> Option<Activity> {
        let session = self.session.as_mut()?;
        let stepped = self.host.with(|now, out| session.step(now, out));
        let following = session.following();
        self.take_events();
        match stepped {
            Ok(activity) => {
                if following && self.connected_at.is_none() {
                    // The host's session start: Connected, then the turn
                    // flighthook holds now.
                    self.connected_at = Some(self.host.now());
                    if std::mem::take(&mut self.connect_error) {
                        self.view.remove("error");
                    }
                    self.set_status(Status::Connected);
                    self.view
                        .insert("ready".into(), self.device.blockers.is_empty().to_string());
                    let turn = self.turn.clone();
                    self.hand(turn);
                }
                Some(activity)
            }
            Err(e) => {
                self.ended(e);
                None
            }
        }
    }

    /// Step until the card has acted on everything so far: a step that
    /// finds nothing to do, with nothing handed meanwhile. Time moves only
    /// by the driver contract's idle wait.
    fn caught_up(&mut self) {
        let mut busy = 0;
        loop {
            match self.step() {
                Some(Activity::Idle) => {
                    if self.session.as_ref().is_some_and(|s| s.inbox.is_empty()) {
                        return;
                    }
                }
                Some(_) => {
                    busy += 1;
                    assert!(busy < 10_000, "the card never idles");
                }
                None => return,
            }
        }
    }

    /// Let `d` of virtual time go by, stepping the card as the host does.
    fn run_for(&mut self, d: Duration) {
        let end = self.host.now() + d;
        while self.host.now() < end {
            match self.step() {
                Some(Activity::Idle) => self.host.advance(IDLE),
                Some(_) => {}
                None => {
                    let left = end.saturating_duration_since(self.host.now());
                    self.host.advance(left);
                }
            }
        }
    }

    /// Step, letting time go by, until `done` holds.
    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let end = self.host.now() + WAIT;
        while !done(self) {
            assert!(self.host.now() < end, "never: {what}");
            match self.step() {
                Some(Activity::Idle) | None => self.host.advance(IDLE),
                Some(_) => {}
            }
        }
    }

    /// Step until the card shows `status` and `pred` holds of its rows.
    fn shows(&mut self, status: Status, pred: impl Fn(&BTreeMap<String, String>) -> bool) {
        self.until(&format!("{status:?}"), |rig| {
            rig.status == status && pred(&rig.view)
        });
    }

    /// Step until the card shows `status` with row `key` = `value`.
    fn shows_row(&mut self, status: Status, key: &str, value: &str) {
        self.shows(status, |t| t.get(key).map(String::as_str) == Some(value));
    }

    /// Step until the card ends the link and the host takes the end.
    fn link_ends(&mut self) {
        self.until("the link ends", |rig| rig.session.is_none());
    }

    /// The next record `pred` picks, stepping until one comes.
    fn bus(&mut self, pred: impl Fn(&Ev) -> bool) -> Ev {
        let end = self.host.now() + WAIT;
        loop {
            while let Some(ev) = self.queued.pop_front() {
                if pred(&ev) {
                    return ev;
                }
            }
            assert!(self.host.now() < end, "no such event");
            match self.step() {
                Some(Activity::Idle) | None => self.host.advance(IDLE),
                Some(_) => {}
            }
        }
    }

    /// The next bus message `pred` picks.
    fn frp_until(&mut self, pred: impl Fn(&FrpBody) -> bool) -> FrpBody {
        match self.bus(|e| matches!(e, Ev::Frp(b) if pred(b))) {
            Ev::Frp(body) => body,
            Ev::Status(_) => unreachable!(),
        }
    }

    /// Every bus message the card sent.
    fn frps(&self) -> Vec<FrpBody> {
        self.record
            .iter()
            .filter_map(|e| match e {
                Ev::Frp(b) => Some(b.clone()),
                Ev::Status(_) => None,
            })
            .collect()
    }

    /// The card's selects.
    fn selects(&self) -> Vec<GolferSelect> {
        self.frps()
            .into_iter()
            .filter_map(|b| match b {
                FrpBody::GolferSelect(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    /// The clubs of the card's selects.
    fn clubs(&self) -> Vec<Club> {
        self.selects()
            .iter()
            .filter_map(|s| s.club.flatten())
            .collect()
    }

    /// The card's shot triggers.
    fn triggers(&self) -> Vec<ShotKey> {
        self.frps()
            .into_iter()
            .filter_map(|b| match b {
                FrpBody::ShotTrigger(t) => Some(t.key),
                _ => None,
            })
            .collect()
    }

    /// The card's strokes.
    fn strokes(&self) -> Vec<flightrelay::GameStroke> {
        self.frps()
            .into_iter()
            .filter_map(|b| match b {
                FrpBody::GameStroke(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    /// Every status the card sent, in order.
    fn statuses(&self) -> Vec<DeviceStatus> {
        self.record
            .iter()
            .filter_map(|e| match e {
                Ev::Status(s) => Some(s.clone()),
                Ev::Frp(_) => None,
            })
            .collect()
    }

    /// The raw frames the card logged, as text.
    fn raw_text(&self) -> Vec<String> {
        self.frps()
            .into_iter()
            .filter_map(|b| match b {
                FrpBody::ShotRaw(f) => Some(String::from_utf8_lossy(&f.data).into_owned()),
                _ => None,
            })
            .collect()
    }

    // -- flighthook's bus --------------------------------------------------------

    /// A pick of `club` in flighthook's header: the select, then the
    /// session's turn and reply.
    fn pick(&mut self, club: Club) {
        self.pick_from(FLIGHTHOOK, club);
    }

    /// A `golfer.select` with `club` from actor `from`, answered by
    /// flighthook's session.
    fn pick_from(&mut self, from: &str, club: Club) {
        self.picks += 1;
        let select = GolferSelect {
            club: Some(Some(club)),
            ..GolferSelect::new(format!("pick-{}", self.picks))
        };
        self.hand_from(from, select.clone());
        self.answer(&select);
    }

    /// flighthook's turn changes to `turn` (a header golfer pick).
    fn turn_to(&mut self, turn: GolferTurn) {
        self.turn = turn.clone();
        self.turns.push(turn.clone());
        self.hand(turn);
    }

    /// `bus.sync` from another actor.
    fn sync(&mut self) {
        self.hand_from("test", BusSync::new());
    }

    /// `game.aim` from `from`.
    fn aim(&mut self, from: &str, aim: GameAim) {
        self.hand_from(from, aim);
    }

    // -- Pureplay ----------------------------------------------------------------

    /// Send `v` in a binary frame, as Pureplay does.
    fn send(&mut self, v: &Value) {
        self.send_frame(Frame::Binary(v.to_string().into_bytes()));
    }

    /// Send `v` in a text frame.
    fn send_text(&mut self, v: &Value) {
        self.send_frame(Frame::Text(v.to_string()));
    }

    fn send_frame(&mut self, frame: Frame) {
        self.pipe.borrow_mut().to_card.push_back(Ok(frame));
    }

    /// Close with `code`.
    fn close(&mut self, code: u16) {
        self.send_frame(Frame::Close {
            code: Some(code),
            reason: String::new(),
        });
    }

    /// The link drops without a close frame.
    fn drop_link(&mut self) {
        self.pipe
            .borrow_mut()
            .to_card
            .push_back(Err(io::ErrorKind::ConnectionReset.into()));
    }

    /// The next message from the card, stepping until one comes.
    fn recv(&mut self) -> Value {
        self.until("a message from the card", |rig| {
            !rig.pipe.borrow().from_card.is_empty()
        });
        self.pipe
            .borrow_mut()
            .from_card
            .pop_front()
            .expect("a message")
    }

    /// The card sent nothing it has not been asked for: after catching up,
    /// nothing waits from it.
    fn sent_nothing(&mut self) -> bool {
        self.caught_up();
        self.pipe.borrow().from_card.is_empty()
    }

    /// Whether the card closed the link.
    fn closed_by_card(&mut self) -> bool {
        self.caught_up();
        self.pipe.borrow().closed
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

    /// Event `name` at the `seq` after the last one sent this way.
    fn ev(&mut self, name: &str, data: &Value) {
        self.seq += 1;
        let seq = self.seq;
        self.event(name, seq, data);
    }

    /// The card's next `state.get`.
    fn state_get(&mut self) -> Value {
        let get = self.recv();
        assert_eq!(get["type"], "state.get", "{get}");
        get
    }
}

fn event_json(name: &str, seq: u64, data: &Value) -> Value {
    json!({ "type": "event", "event": name, "seq": seq, "time": "2026-11-10T12:00:01Z", "data": data })
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

/// What the fake's `hello` lists: every event Pureplay v1 has.
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

/// A card paired with token `token` that has taken the round snapshot.
fn connected(token: &str) -> Rig {
    let mut rig = Rig::with_token(token);
    rig.next();
    rig.hello();
    assert_eq!(rig.pair().as_deref(), Some(token));
    rig.paired(token);
    rig.subscribe(&round(), 1);
    rig.shows_row(Status::Connected, "club", "7I");
    rig.caught_up();
    rig
}

/// Joe's golfer id.
fn joe() -> GolferId {
    "pureplay.Joe".parse().expect("golfer id")
}

/// The one real-socket test: [`WsLink`] against a WebSocket server on
/// loopback, asking for the `binary` subprotocol, sending commands in text
/// frames, reading binary, a ping answered, and the server's close code.
#[test]
fn the_websocket_link_talks_to_a_real_server() {
    use std::net::TcpListener;
    use tungstenite::Message;
    use tungstenite::handshake::server::{Request, Response as HsResponse};
    use tungstenite::protocol::CloseFrame;
    use tungstenite::protocol::frame::coding::CloseCode;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        #[allow(clippy::result_large_err)]
        let mut ws = tungstenite::accept_hdr(stream, |req: &Request, mut resp: HsResponse| {
            let asked = req
                .headers()
                .get("Sec-WebSocket-Protocol")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            assert_eq!(asked, "binary");
            resp.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                tungstenite::http::HeaderValue::from_static("binary"),
            );
            Ok(resp)
        })
        .expect("handshake");
        // The first data frame, as text or as the bytes of a binary frame.
        let got = loop {
            match ws.read().expect("read") {
                Message::Text(t) => break Ok(t.as_str().to_owned()),
                Message::Binary(b) => break Err(b.to_vec()),
                _ => {}
            }
        };
        ws.send(Message::binary(br#"{"type":"hello"}"#.to_vec()))
            .expect("send");
        ws.close(Some(CloseFrame {
            code: CloseCode::from(4401),
            reason: "gone".into(),
        }))
        .expect("close");
        // Answer the ping and take the close reply.
        while ws.read().is_ok() {}
        got
    });
    let stream = std::net::TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_millis(5)))
        .expect("timeout");
    let mut link =
        WsLink::open(stream, addr, Duration::from_secs(5), || true).expect("the handshake");
    link.send(r#"{"type":"pair"}"#).expect("send");
    link.ping().expect("ping");
    let mut frames = Vec::new();
    while !matches!(frames.last(), Some(Frame::Close { .. })) {
        match link.recv() {
            Ok(Some(Frame::Control)) | Ok(None) => {}
            Ok(Some(frame)) => frames.push(frame),
            Err(e) => panic!("the link failed: {e}"),
        }
    }
    assert_eq!(
        frames,
        [
            Frame::Binary(br#"{"type":"hello"}"#.to_vec()),
            Frame::Close {
                code: Some(4401),
                reason: "gone".into()
            }
        ]
    );
    link.close();
    assert_eq!(
        server.join().expect("server"),
        Ok(r#"{"type":"pair"}"#.to_owned()),
        "a command goes in a text frame"
    );
}
