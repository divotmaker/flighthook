//! GSPro Open Connect V1 **server**.
//!
//! This is the inverse of the `openconnect_client` actor, an Open Connect
//! *client* that dials a simulator and pushes shots to it. This actor *listens* and
//! accepts shots pushed to it by a launch monitor that speaks Open Connect —
//! Uneekor, Foresight, SkyTrak, MLM2PRO, and others all emit it.
//!
//! It is therefore a launch monitor actor, not an integration: it publishes
//! the standard shot lifecycle onto the bus, and anything downstream
//! (including an `openconnect_client`) consumes it unchanged.
//!
//! ```text
//!   Uneekor / Foresight / SkyTrak  --TCP 921-->  openconnect_server.0
//!                                                        |
//!                                                        v  bus
//!                                       openconnect_client.0  --TCP 922-->  GSPro
//! ```
//!
//! GSPro listens on 921 as well, but its port is movable: setting
//! `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
//! `C:\GSPro\GSPC\GSPconnect.exe.config` starts GSPConnect on 922 and frees
//! 921 for this actor, so both can share one host. Splitting across two hosts
//! works too, but only if the monitor's connector can target a non-localhost
//! address — Open Connect's documentation specifies `127.0.0.1`.
//!
//! The server refuses a connection from this process's own
//! `openconnect_client` (one pointed at this server's port), which would loop
//! every shot client -> server -> bus -> client. Each client registers its
//! connection's local address in [`LocalClients`]; a peer found there is
//! closed at once, unread, and the server raises one Error alert per burst of
//! that client's reconnect attempts. A connection from anything else, on the
//! same host or not, is served as usual.

pub mod mapper;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::gspro::api;
use super::{Actor, ReconfigureOutcome, actor_names};
use crate::bus::{BusReceiver, BusSender};
use crate::sim;
use crate::state::SystemState;
use crate::state::local_clients::LocalClients;
use flighthook::{
    ActorStatus, FlighthookEvent, FlighthookMessage, Handedness, RawDir, RawPayload, RawRecord,
    Severity, ShotKey, ShotRaw,
};

/// Cap on buffered bytes from a single peer before the connection is dropped.
/// A well-behaved client never approaches this; it exists so a peer that opens
/// a socket and streams garbage cannot grow the buffer without bound.
const MAX_BUFFER: usize = 1 << 20;

/// Refusals of one client closer together than this are one burst of its
/// reconnect attempts, alerted once. The client retries every 1-3 s.
const REFUSAL_BURST_GAP: Duration = Duration::from_secs(30);

/// The client's `error` telemetry when server `server` (its display name)
/// refuses it. The UI shows it after the client's name.
fn refusal_error(server: &str) -> String {
    format!(
        "Refused by this flighthook's own OpenConnect Server \"{server}\": this \
         address points at it, which would loop shots back. Use the simulator's \
         port instead."
    )
}

/// The server's alert when it refuses client `client` (its display name)
/// connecting from `peer`. The Log shows it after the server's name.
fn refusal_alert(client: &str, peer: SocketAddr) -> String {
    format!(
        "Refused OpenConnect Client \"{client}\" from {peer}: it is this \
         flighthook's own client, which would loop shots back. Point it at the \
         simulator's port."
    )
}

/// Refusals so far, by client actor ID: when each was last refused.
#[derive(Default)]
struct Refusals(HashMap<String, Instant>);

impl Refusals {
    /// Record a refusal of `client` at `now`. Returns whether it starts a
    /// new burst (and so is alerted).
    fn record(&mut self, client: &str, now: Instant) -> bool {
        let last = self.0.insert(client.to_string(), now);
        last.is_none_or(|at| now.saturating_duration_since(at) >= REFUSAL_BURST_GAP)
    }
}

/// If `peer` is one of this process's own Open Connect clients, refuse it:
/// tell the client why, and alert once per burst. Returns whether it was
/// refused; the caller closes the connection.
fn refuse_own_client(
    state: &SystemState,
    clients: &LocalClients,
    peer: SocketAddr,
    refusals: &mut Refusals,
    sender: &BusSender,
) -> bool {
    let Some(client) = clients.client_at(peer) else {
        return false;
    };
    let names = actor_names(&state.system.snapshot());
    let client_name = display_name(&names, &client);
    let server_name = display_name(&names, sender.actor_id());
    clients.refuse(peer, &refusal_error(&server_name));
    tracing::warn!("openconnect server: refused {peer}, from {client} ({client_name})");
    if refusals.record(&client, Instant::now()) {
        sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
            severity: Severity::Error,
            message: refusal_alert(&client_name, peer),
        }));
    }
    true
}

/// The name actor `actor`'s card shows, from [`actor_names`], or its actor
/// ID when that is blank.
fn display_name(names: &HashMap<String, String>, actor: &str) -> String {
    names
        .get(actor)
        .filter(|n| !n.trim().is_empty())
        .map_or_else(|| actor.to_string(), String::clone)
}

/// Close a refused connection without reading from it.
fn close(stream: &TcpStream) {
    let _ = stream.shutdown(Shutdown::Both);
}

/// Open Connect server actor. Accepts inbound shot data from launch monitors
/// that speak GSPro Open Connect V1 as a client.
pub struct OpenConnectServerActor {
    pub bind: SocketAddr,
}

impl Actor for OpenConnectServerActor {
    fn start(&self, state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let bind = self.bind;
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(&state, bind, sender, receiver))
            .expect("failed to spawn openconnect thread");
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let actor_id = sender.actor_id();
        let Some((_, index)) = actor_id.split_once('.') else {
            return ReconfigureOutcome::Applied;
        };

        let snap = state.system.snapshot();
        let Some(section) = snap.openconnect_server.get(index) else {
            return ReconfigureOutcome::RestartRequired; // section removed
        };

        let bind_str = section.bind.as_deref().unwrap_or("0.0.0.0:921");
        match bind_str.parse::<SocketAddr>() {
            Ok(new_bind) if new_bind == self.bind => ReconfigureOutcome::Applied,
            _ => ReconfigureOutcome::RestartRequired,
        }
    }
}

/// Listener loop. Rebinds with linear backoff if the bind fails (typically
/// because GSPro or another instance already holds the port — see the module
/// docs for moving GSPConnect to 922).
fn run(state: &SystemState, bind: SocketAddr, sender: BusSender, mut receiver: BusReceiver) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(15);
    let clients = Arc::clone(&state.local_clients);
    let mut refusals = Refusals::default();

    loop {
        if receiver.is_shutdown() {
            tracing::info!("openconnect server: shutting down");
            return;
        }

        sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
            status: ActorStatus::Starting,
            telemetry: HashMap::from([("bind".into(), bind.to_string())]),
        }));

        let listener = match TcpListener::bind(bind) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    "openconnect server: bind {bind} failed: {e}, retrying in {backoff:?}"
                );
                sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                    severity: Severity::Error,
                    message: format!("OpenConnect server could not bind {bind}: {e}"),
                }));
                std::thread::sleep(backoff);
                backoff = (backoff + Duration::from_secs(1)).min(max_backoff);
                continue;
            }
        };
        backoff = Duration::from_secs(1);

        if let Err(e) = listener.set_nonblocking(true) {
            tracing::warn!("openconnect server: set_nonblocking failed: {e}");
        }
        // The address actually bound (port 0 picks one).
        let bound = listener.local_addr().unwrap_or(bind);
        tracing::info!("openconnect server: listening on {bound}");
        sender.send(listening_status(bound));

        // Accept one monitor at a time. Open Connect is a single-peer protocol;
        // a second monitor should be a second actor on its own port.
        loop {
            if receiver.is_shutdown() {
                tracing::info!("openconnect server: shutting down");
                return;
            }
            match listener.accept() {
                Ok((stream, peer)) => {
                    if refuse_own_client(state, &clients, peer, &mut refusals, &sender) {
                        close(&stream);
                        continue;
                    }
                    tracing::info!("openconnect server: {peer} connected");
                    serve(state, stream, peer, &mut refusals, &sender, &mut receiver);
                    if receiver.is_shutdown() {
                        return;
                    }
                    sender.send(listening_status(bound));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    tracing::warn!("openconnect server: accept failed: {e}");
                    break; // rebind
                }
            }
        }
    }
}

/// The status of a server listening on `bound` with no monitor connected.
/// The empty `peer` removes the last monitor's address from the telemetry.
fn listening_status(bound: SocketAddr) -> FlighthookMessage {
    FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status: ActorStatus::Disconnected,
        telemetry: HashMap::from([
            ("bind".into(), bound.to_string()),
            ("peer".into(), String::new()),
        ]),
    })
}

/// Per-connection state that persists across messages from one monitor.
struct Peer {
    /// Device identity announced via `DeviceID`, emitted once.
    announced: bool,
    /// Last readiness we published, so telemetry is emitted only on change.
    ready: Option<bool>,
}

/// Serve a single connected monitor until it disconnects or we shut down.
///
/// A client of ours registers its connection a moment after connecting, so
/// the peer is looked up again after each read, before the read is handled:
/// a connection found to be ours is closed before any of its data reaches the
/// bus. The client registers before it sends anything, so that look-up is
/// what guards the data, whatever the timing of the accept. The Connected
/// status waits until the first read (which blocks up to the read timeout)
/// has passed the look-up too, so our client has that long to register
/// before the connection shows as connected.
fn serve(
    state: &SystemState,
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    refusals: &mut Refusals,
    sender: &BusSender,
    receiver: &mut BusReceiver,
) {
    let name = sender.actor_id();
    let clients = &state.local_clients;

    // Some platforms (Windows, the BSDs) hand out accepted sockets in the
    // listener's non-blocking mode; reads here block, up to the timeout.
    if let Err(e) = stream.set_nonblocking(false) {
        tracing::warn!("openconnect server: set_nonblocking failed: {e}");
    }
    if let Err(e) = stream.set_read_timeout(Some(Duration::from_millis(100))) {
        tracing::warn!("openconnect server: set_read_timeout failed: {e}");
    }
    let _ = stream.set_nodelay(true);

    let mut announced = false;
    let mut peer = Peer {
        announced: false,
        ready: None,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = vec![0u8; 4096];

    loop {
        if receiver.is_shutdown() {
            return;
        }
        // Drain the bus so this actor's queue cannot grow unbounded. An
        // Open Connect monitor is push-only: there is nothing to act on.
        while let Ok(Some(_)) = receiver.poll() {}

        let read = stream.read(&mut chunk);
        if refuse_own_client(state, clients, peer_addr, refusals, sender) {
            close(&stream);
            return;
        }
        if !announced {
            announced = true;
            sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                status: ActorStatus::Connected,
                telemetry: HashMap::from([("peer".into(), peer_addr.to_string())]),
            }));
        }
        match read {
            Ok(0) => {
                tracing::info!("openconnect server: {peer_addr} disconnected");
                return;
            }
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_BUFFER {
                    tracing::warn!(
                        "openconnect server: {peer_addr} exceeded {MAX_BUFFER} buffered bytes, dropping"
                    );
                    sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                        severity: Severity::Warn,
                        message: "OpenConnect peer sent unparsable data, dropping connection"
                            .into(),
                    }));
                    return;
                }
                if !drain(state, &mut buf, &mut stream, &mut peer, sender, name) {
                    return;
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                tracing::warn!("openconnect server: {peer_addr} read error: {e}");
                sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                    severity: Severity::Warn,
                    message: format!("OpenConnect read error: {e}"),
                }));
                return;
            }
        }
    }
}

/// Parse every complete JSON value buffered so far and handle it.
///
/// Open Connect is a stream of bare JSON objects with no framing — no newline,
/// no length prefix — so completeness is determined by the parser. Trailing
/// bytes of a partial object stay buffered for the next read.
///
/// Returns false if the connection should be dropped.
fn drain(
    state: &SystemState,
    buf: &mut Vec<u8>,
    stream: &mut TcpStream,
    peer: &mut Peer,
    sender: &BusSender,
    name: &str,
) -> bool {
    loop {
        let mut iter = serde_json::Deserializer::from_slice(buf).into_iter::<api::GsProMessage>();
        let Some(result) = iter.next() else {
            buf.clear();
            return true;
        };
        match result {
            Ok(msg) => {
                let consumed = iter.byte_offset();
                let raw = String::from_utf8_lossy(&buf[..consumed]).into_owned();
                buf.drain(..consumed);
                if !handle(state, msg, raw, stream, peer, sender, name) {
                    return false;
                }
            }
            Err(e) if e.is_eof() => return true, // partial object, wait for more
            Err(e) => {
                tracing::warn!("openconnect server: malformed message: {e}");
                sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                    severity: Severity::Warn,
                    message: format!("OpenConnect parse error: {e}"),
                }));
                buf.clear();
                return true;
            }
        }
    }
}

/// Handle one decoded Open Connect message and acknowledge it.
///
/// Returns false if the connection should be dropped.
fn handle(
    state: &SystemState,
    msg: api::GsProMessage,
    raw: String,
    stream: &mut TcpStream,
    peer: &mut Peer,
    sender: &BusSender,
    name: &str,
) -> bool {
    // Hex-first policy: for a JSON protocol the raw text *is* the canonical
    // wire form, so it rides along on every event derived from it.
    let payload = RawPayload::Text(raw.clone());
    let device = msg.device_id.clone();

    tracing::info!(target: "audit", "{name} received {raw} | {msg:?}");

    if !peer.announced {
        peer.announced = true;
        sender.send(
            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                manufacturer: None,
                model: Some(msg.device_id.clone()),
                firmware: None,
                telemetry: None,
            })
            .device(device.as_str()),
        );
    }

    // Readiness rides on every message, heartbeat or not.
    let ready = msg.shot_data_options.launch_monitor_is_ready;
    if peer.ready != Some(ready) {
        peer.ready = Some(ready);
        sender.send(
            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                manufacturer: None,
                model: None,
                firmware: None,
                telemetry: Some(HashMap::from([
                    ("ready".into(), ready.to_string()),
                    (
                        "ball_detected".into(),
                        msg.shot_data_options
                            .launch_monitor_ball_detected
                            .to_string(),
                    ),
                ])),
            })
            .device(device.as_str()),
        );
    }

    let is_shot = !msg.shot_data_options.is_heart_beat && msg.shot_data_options.contains_ball_data;

    let reply = api::GsProResponse::ok(if is_shot {
        "Shot received"
    } else {
        "Heartbeat received"
    });
    let reply_text = match serde_json::to_string(&reply) {
        Ok(text) => Some(text),
        Err(e) => {
            tracing::warn!("openconnect server: could not encode response: {e}");
            None
        }
    };

    if is_shot {
        emit_shot(
            state,
            &msg,
            &payload,
            shot_raw(&raw, reply_text.as_deref()),
            sender,
        );
    }

    if let Some(text) = reply_text {
        tracing::info!(target: "audit", "{name} sent {text} | {reply:?}");
        if let Err(e) = stream
            .write_all(text.as_bytes())
            .and_then(|()| stream.flush())
        {
            tracing::warn!("openconnect server: write failed: {e}");
            return false;
        }
    }
    true
}

/// The raw frames behind one shot: the monitor's message and our reply.
fn shot_raw(request: &str, reply: Option<&str>) -> ShotRaw {
    let mut frames = vec![RawRecord {
        channel: Some("tcp".into()),
        ..RawRecord::text(RawDir::Rx, request)
    }];
    if let Some(reply) = reply {
        frames.push(RawRecord {
            channel: Some("tcp".into()),
            ..RawRecord::text(RawDir::Tx, reply)
        });
    }
    ShotRaw {
        transport: "openconnect-json".into(),
        frames,
        ..ShotRaw::default()
    }
}

/// Publish the standard shot lifecycle for one Open Connect shot message.
fn emit_shot(
    state: &SystemState,
    msg: &api::GsProMessage,
    payload: &RawPayload,
    raw: ShotRaw,
    sender: &BusSender,
) {
    let device = msg.device_id.as_str();
    let key = ShotKey {
        shot_id: uuid::Uuid::new_v4().to_string(),
        shot_number: msg.shot_number,
    };

    let handed = state.game.snapshot().handed.unwrap_or(Handedness::Right);
    let ball = mapper::map_ball(msg, handed);
    tracing::info!(
        "openconnect <- shot #{}: {:.1}mph VLA={:.1} HLA={:.1} spin={:?}/{:?}",
        msg.shot_number,
        msg.ball_data.speed,
        msg.ball_data.vla,
        msg.ball_data.hla,
        ball.backspin_rpm.unwrap_or(0),
        ball.sidespin_rpm.unwrap_or(0),
    );

    sender.send(
        FlighthookMessage::new(FlighthookEvent::ShotTrigger { key: key.clone() })
            .raw(payload.clone())
            .device(device),
    );
    sender.send(
        FlighthookMessage::new(sim::ball_flight_event(state, key.clone(), ball))
            .raw(payload.clone())
            .device(device),
    );

    if msg.shot_data_options.contains_club_data {
        sender.send(
            FlighthookMessage::new(FlighthookEvent::ClubPath {
                key: key.clone(),
                club: Box::new(mapper::map_club(msg, handed)),
            })
            .raw(payload.clone())
            .device(device),
        );
        if let Some(impact) = mapper::map_face_impact(msg) {
            sender.send(
                FlighthookMessage::new(FlighthookEvent::FaceImpact {
                    key: key.clone(),
                    impact: Box::new(impact),
                })
                .raw(payload.clone())
                .device(device),
            );
        }
    }

    sender.send(
        FlighthookMessage::new(FlighthookEvent::ShotFinished {
            key,
            raw: Some(Box::new(raw)),
        })
        .raw(payload.clone())
        .device(device),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use tokio::sync::broadcast;

    use super::*;

    #[test]
    fn shot_finished_carries_request_and_reply() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-oc-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let (tx, mut rx) = broadcast::channel(16);
        let sender = BusSender::new(
            "openconnect_server.0".into(),
            tx,
            Arc::new(AtomicBool::new(false)),
        );

        let request = r#"{"DeviceID":"TestLM","Units":"Yards","ShotNumber":4,"APIversion":"1","BallData":{"Speed":100.0},"ShotDataOptions":{"ContainsBallData":true}}"#;
        let msg: api::GsProMessage = serde_json::from_str(request).expect("parses");
        let reply = r#"{"Code":200,"Message":"Shot received"}"#;
        emit_shot(
            &state,
            &msg,
            &RawPayload::Text(request.into()),
            shot_raw(request, Some(reply)),
            &sender,
        );

        let mut finished = None;
        while let Ok(m) = rx.try_recv() {
            if let FlighthookEvent::ShotFinished { raw, .. } = m.event {
                finished = raw;
            }
        }
        let raw = finished.expect("shot_finished with raw");
        assert_eq!(raw.transport, "openconnect-json");
        assert_eq!(raw.frames.len(), 2);
        assert_eq!(raw.frames[0].dir, RawDir::Rx);
        assert_eq!(raw.frames[0].text.as_deref(), Some(request));
        assert_eq!(raw.frames[1].dir, RawDir::Tx);
        assert_eq!(raw.frames[1].text.as_deref(), Some(reply));
        assert!(
            raw.frames
                .iter()
                .all(|f| f.channel.as_deref() == Some("tcp"))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bind_change_restarts_the_server() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-oc-bind-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let state = Arc::new(state);
        let set_bind = |bind: Option<&str>| {
            state.system.update(|c| {
                c.openconnect_server.insert(
                    "0".into(),
                    flighthook::OpenConnectServerSection {
                        name: "Uneekor".into(),
                        bind: bind.map(str::to_string),
                    },
                );
            });
        };
        let (tx, _rx) = broadcast::channel(16);
        let sender = BusSender::new(
            "openconnect_server.0".into(),
            tx,
            Arc::new(AtomicBool::new(false)),
        );
        let actor = OpenConnectServerActor {
            bind: "0.0.0.0:921".parse().expect("addr"),
        };
        let outcome = |state: &Arc<SystemState>| actor.reconfigure(state, &sender);

        set_bind(Some("0.0.0.0:921"));
        assert!(matches!(outcome(&state), ReconfigureOutcome::Applied));
        // Blank is the default bind.
        set_bind(None);
        assert!(matches!(outcome(&state), ReconfigureOutcome::Applied));
        set_bind(Some("0.0.0.0:9921"));
        assert!(matches!(
            outcome(&state),
            ReconfigureOutcome::RestartRequired
        ));
        set_bind(Some("127.0.0.1:921"));
        assert!(matches!(
            outcome(&state),
            ReconfigureOutcome::RestartRequired
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    // -- Self-loop guard, end to end ------------------------------------------

    use std::net::TcpListener;
    use std::sync::atomic::Ordering;

    use crate::actors::backoff::Backoff;
    use crate::actors::gspro::{self, GsProRouting};

    /// Longest any test waits for one thing.
    const WAIT: Duration = Duration::from_secs(8);

    /// A process's worth of actors on one bus, sharing one `SystemState`.
    struct Rig {
        state: Arc<SystemState>,
        tx: broadcast::Sender<FlighthookMessage>,
        events: broadcast::Receiver<FlighthookMessage>,
        flags: Vec<Arc<AtomicBool>>,
        dir: std::path::PathBuf,
    }

    impl Rig {
        /// A rig whose config names Open Connect client 0 `client_name` and
        /// Open Connect server 0 "Bay Uneekor".
        fn new(client_name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flighthook-oc-loop-{}",
                crate::state::config::generate_id()
            ));
            let (state, _writer) = SystemState::new(dir.join("config.toml"));
            state.system.update(|c| {
                c.openconnect_client.insert(
                    "0".into(),
                    flighthook::OpenConnectClientSection {
                        name: client_name.into(),
                        ..Default::default()
                    },
                );
                c.openconnect_server.insert(
                    "0".into(),
                    flighthook::OpenConnectServerSection {
                        name: "Bay Uneekor".into(),
                        bind: None,
                    },
                );
            });
            let (tx, events) = broadcast::channel(4096);
            Self {
                state: Arc::new(state),
                tx,
                events,
                flags: Vec::new(),
                dir,
            }
        }

        fn sender(&mut self, id: &str) -> BusSender {
            let flag = Arc::new(AtomicBool::new(false));
            self.flags.push(Arc::clone(&flag));
            BusSender::new(id.into(), self.tx.clone(), flag)
        }

        /// Start `openconnect_server.0` on `ip`, on a port the system
        /// picks. Returns the address it listens on, once it does.
        fn server(&mut self, ip: [u8; 4]) -> SocketAddr {
            let sender = self.sender("openconnect_server.0");
            let receiver = sender.subscribe();
            let state = Arc::clone(&self.state);
            let bind = SocketAddr::from((ip, 0));
            std::thread::spawn(move || run(&state, bind, sender, receiver));
            let seen = self.until("the server listening", |m| listening(m).is_some());
            seen.last().and_then(listening).expect("listening")
        }

        /// Start `openconnect_client.0` dialing `addr`.
        fn client(&mut self, addr: SocketAddr) {
            let sender = self.sender("openconnect_client.0");
            let receiver = sender.subscribe();
            let state = Arc::clone(&self.state);
            std::thread::spawn(move || {
                gspro::run_with(
                    addr,
                    GsProRouting::default(),
                    Backoff::new(Duration::from_secs(1), Duration::from_secs(1)),
                    &state.local_clients,
                    &sender,
                    receiver,
                );
            });
        }

        /// Every message until `done` holds for one or [`WAIT`] runs out.
        fn until(
            &mut self,
            what: &str,
            done: impl Fn(&FlighthookMessage) -> bool,
        ) -> Vec<FlighthookMessage> {
            let deadline = Instant::now() + WAIT;
            let mut seen = Vec::new();
            loop {
                match self.events.try_recv() {
                    Ok(msg) => {
                        let stop = done(&msg);
                        seen.push(msg);
                        if stop {
                            return seen;
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
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            for flag in &self.flags {
                flag.store(true, Ordering::Relaxed);
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The address the server reports listening on, if `msg` says so.
    fn listening(msg: &FlighthookMessage) -> Option<SocketAddr> {
        match &msg.event {
            FlighthookEvent::ActorStatus {
                status: ActorStatus::Disconnected,
                telemetry,
            } if msg.actor == "openconnect_server.0" => {
                telemetry.get("bind").and_then(|b| b.parse().ok())
            }
            _ => None,
        }
    }

    fn alerts(seen: &[FlighthookMessage], actor: &str) -> Vec<String> {
        seen.iter()
            .filter(|m| m.actor == actor)
            .filter_map(|m| match &m.event {
                FlighthookEvent::Alert { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    /// The client's `error` telemetry on `msg`, if it carries one.
    fn client_error(msg: &FlighthookMessage) -> Option<&str> {
        match &msg.event {
            FlighthookEvent::ActorStatus { telemetry, .. }
                if msg.actor == "openconnect_client.0" =>
            {
                telemetry
                    .get("error")
                    .map(String::as_str)
                    .filter(|e| !e.is_empty())
            }
            _ => None,
        }
    }

    fn server_shot(msg: &FlighthookMessage) -> bool {
        msg.actor == "openconnect_server.0"
            && matches!(msg.event, FlighthookEvent::ShotTrigger { .. })
    }

    const SHOT: &str = r#"{"DeviceID":"Uneekor","Units":"Yards","ShotNumber":1,"APIversion":"1","BallData":{"Speed":100.0,"VLA":14.0,"HLA":1.0,"TotalSpin":3000.0,"SpinAxis":2.0},"ShotDataOptions":{"ContainsBallData":true}}"#;

    #[test]
    fn a_client_pointed_at_its_own_server_is_refused_and_alerted_once() {
        let mut rig = Rig::new("Bay GSPro");
        // The product owner's setup: the server on 0.0.0.0, the client on
        // loopback, nothing else listening.
        let port = rig.server([0, 0, 0, 0]).port();
        rig.client(SocketAddr::from(([127, 0, 0, 1], port)));

        // Refused, with the reason on the client's card.
        let mut seen = rig.until("the first refusal", |m| client_error(m).is_some());
        let error = seen
            .iter()
            .find_map(client_error)
            .expect("error")
            .to_string();
        assert_eq!(
            error,
            "Refused by this flighthook's own OpenConnect Server \"Bay Uneekor\": this \
             address points at it, which would loop shots back. Use the simulator's \
             port instead."
        );

        // A shot while the loop is set up never comes back round.
        let monitor = rig.sender("mevo.0");
        let key = ShotKey {
            shot_id: "s1".into(),
            shot_number: 1,
        };
        monitor.send(FlighthookMessage::new(FlighthookEvent::ShotTrigger {
            key: key.clone(),
        }));
        monitor.send(FlighthookMessage::new(FlighthookEvent::BallFlight {
            key: key.clone(),
            ball: Box::new(flighthook::BallFlight {
                launch_speed: Some(flighthook::Velocity::MilesPerHour(100.0)),
                launch_elevation: Some(14.0),
                launch_azimuth: Some(1.0),
                backspin_rpm: Some(3000),
                sidespin_rpm: Some(100),
                ..Default::default()
            }),
            derived: None,
        }));
        monitor.send(FlighthookMessage::new(FlighthookEvent::ShotFinished {
            key,
            raw: None,
        }));

        // The retries are refused too, quietly: one alert for the burst.
        let mut refusals = 1;
        while refusals < 3 {
            let more = rig.until("another refusal", |m| client_error(m).is_some());
            refusals += 1;
            seen.extend(more);
        }
        let alerts = alerts(&seen, "openconnect_server.0");
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        let (start, end) = alerts[0]
            .split_once(" from 127.0.0.1:")
            .expect("the peer address");
        assert_eq!(start, "Refused OpenConnect Client \"Bay GSPro\"");
        assert!(
            end.ends_with(
                ": it is this flighthook's own client, which would loop shots back. \
                 Point it at the simulator's port."
            ),
            "{end}"
        );
        assert!(!seen.iter().any(server_shot), "a shot looped back");
        // Once refused, the client's card stays on the error: it does not
        // flash Connected on each retry.
        let first = seen
            .iter()
            .position(|m| client_error(m).is_some())
            .expect("refused");
        assert!(
            !seen[first..]
                .iter()
                .any(|m| m.actor == "openconnect_client.0"
                    && matches!(
                        m.event,
                        FlighthookEvent::ActorStatus {
                            status: ActorStatus::Connected,
                            ..
                        }
                    )),
            "the client reported Connected after a refusal"
        );
        assert!(
            !seen.iter().any(|m| m.actor == "openconnect_server.0"
                && matches!(
                    m.event,
                    FlighthookEvent::ActorStatus {
                        status: ActorStatus::Connected,
                        ..
                    }
                )),
            "the server never serves its own client"
        );
    }

    #[test]
    fn a_foreign_peer_is_served() {
        let mut rig = Rig::new("GSPro");
        let bind = rig.server([127, 0, 0, 1]);
        let mut uneekor = TcpStream::connect(bind).expect("monitor connects");
        uneekor.write_all(SHOT.as_bytes()).expect("send shot");
        let seen = rig.until("the shot", server_shot);
        assert!(alerts(&seen, "openconnect_server.0").is_empty());
    }

    /// The `peer` telemetry the server reports on `msg`: `Some("")` when it
    /// removes the key.
    fn server_peer(msg: &FlighthookMessage) -> Option<&str> {
        match &msg.event {
            FlighthookEvent::ActorStatus { telemetry, .. }
                if msg.actor == "openconnect_server.0" =>
            {
                telemetry.get("peer").map(String::as_str)
            }
            _ => None,
        }
    }

    #[test]
    fn the_peer_is_cleared_when_the_monitor_disconnects() {
        let mut rig = Rig::new("GSPro");
        let bind = rig.server([127, 0, 0, 1]);
        let mut uneekor = TcpStream::connect(bind).expect("monitor connects");
        let local = uneekor.local_addr().expect("monitor address").to_string();
        uneekor.write_all(SHOT.as_bytes()).expect("send shot");
        let seen = rig.until("the shot", server_shot);
        assert!(
            seen.iter().any(|m| server_peer(m) == Some(local.as_str())),
            "the card shows the peer"
        );

        drop(uneekor);
        let seen = rig.until("the server listening again", |m| listening(m).is_some());
        assert_eq!(seen.last().and_then(server_peer), Some(""), "{seen:?}");
    }

    #[test]
    fn a_chain_through_the_server_to_another_port_works() {
        let mut rig = Rig::new("GSPro");
        // GSPro on 922, the server on 921's stand-in, the client dialing GSPro.
        let gspro = TcpListener::bind("127.0.0.1:0").expect("bind fake GSPro");
        let bind = rig.server([127, 0, 0, 1]);
        rig.client(gspro.local_addr().expect("addr"));
        let (mut gspro, _) = gspro.accept().expect("client connects to GSPro");
        gspro
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("timeout");
        rig.until("the client connected", |m| {
            m.actor == "openconnect_client.0"
                && matches!(
                    m.event,
                    FlighthookEvent::ActorStatus {
                        status: ActorStatus::Connected,
                        ..
                    }
                )
        });

        let mut uneekor = TcpStream::connect(bind).expect("monitor connects");
        uneekor.write_all(SHOT.as_bytes()).expect("send shot");
        let seen = rig.until("the shot on the bus", server_shot);

        // GSPro gets the shot.
        let deadline = Instant::now() + WAIT;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let shot = 'found: loop {
            let mut it =
                serde_json::Deserializer::from_slice(&buf).into_iter::<serde_json::Value>();
            while let Some(Ok(v)) = it.next() {
                if v["ShotDataOptions"]["IsHeartBeat"] == false {
                    break 'found v;
                }
            }
            assert!(Instant::now() < deadline, "GSPro got no shot");
            match gspro.read(&mut chunk) {
                Ok(0) => panic!("client closed"),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => {}
            }
        };
        assert_eq!(shot["BallData"]["Speed"], 100.0);
        assert!(alerts(&seen, "openconnect_server.0").is_empty());
        assert!(!seen.iter().any(|m| client_error(m).is_some()));
    }
}
