//! GSPro Open Connect V1 **server**.
//!
//! This is the inverse of the `gspro` actor. `gspro` is an Open Connect
//! *client* that dials GSPro and pushes shots to it. This actor *listens* and
//! accepts shots pushed to it by a launch monitor that speaks Open Connect —
//! Uneekor, Foresight, SkyTrak, MLM2PRO, and others all emit it.
//!
//! It is therefore a launch monitor actor, not an integration: it publishes
//! the standard shot lifecycle onto the bus, and anything downstream
//! (including the `gspro` client) consumes it unchanged.
//!
//! ```text
//!   Uneekor / Foresight / SkyTrak  --TCP 921-->  openconnect_server.0
//!                                                        |
//!                                                        v  bus
//!                                                  gspro.0  --TCP 922-->  GSPro
//! ```
//!
//! GSPro listens on 921 as well, but its port is movable: setting
//! `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
//! `C:\GSPro\GSPC\GSPconnect.exe.config` starts GSPConnect on 922 and frees
//! 921 for this actor, so both can share one host. Splitting across two hosts
//! works too, but only if the monitor's connector can target a non-localhost
//! address — Open Connect's documentation specifies `127.0.0.1`.

pub mod mapper;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use super::gspro::api;
use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender};
use crate::state::SystemState;
use flighthook::{ActorStatus, FlighthookEvent, FlighthookMessage, RawPayload, Severity, ShotKey};

/// Cap on buffered bytes from a single peer before the connection is dropped.
/// A well-behaved client never approaches this; it exists so a peer that opens
/// a socket and streams garbage cannot grow the buffer without bound.
const MAX_BUFFER: usize = 1 << 20;

/// Open Connect server actor. Accepts inbound shot data from launch monitors
/// that speak GSPro Open Connect V1 as a client.
pub struct OpenConnectServerActor {
    pub bind: SocketAddr,
}

impl Actor for OpenConnectServerActor {
    fn start(&self, _state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let bind = self.bind;
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(bind, sender, receiver))
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
fn run(bind: SocketAddr, sender: BusSender, mut receiver: BusReceiver) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(15);

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
        tracing::info!("openconnect server: listening on {bind}");
        sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
            status: ActorStatus::Disconnected,
            telemetry: HashMap::from([("bind".into(), bind.to_string())]),
        }));

        // Accept one monitor at a time. Open Connect is a single-peer protocol;
        // a second monitor should be a second actor on its own port.
        loop {
            if receiver.is_shutdown() {
                tracing::info!("openconnect server: shutting down");
                return;
            }
            match listener.accept() {
                Ok((stream, peer)) => {
                    tracing::info!("openconnect server: {peer} connected");
                    serve(stream, peer, &sender, &mut receiver);
                    if receiver.is_shutdown() {
                        return;
                    }
                    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                        status: ActorStatus::Disconnected,
                        telemetry: HashMap::from([("bind".into(), bind.to_string())]),
                    }));
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

/// Per-connection state that persists across messages from one monitor.
struct Peer {
    /// Device identity announced via `DeviceID`, emitted once.
    announced: bool,
    /// Last readiness we published, so telemetry is emitted only on change.
    ready: Option<bool>,
}

/// Serve a single connected monitor until it disconnects or we shut down.
fn serve(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    sender: &BusSender,
    receiver: &mut BusReceiver,
) {
    let name = sender.actor_id();

    if let Err(e) = stream.set_read_timeout(Some(Duration::from_millis(100))) {
        tracing::warn!("openconnect server: set_read_timeout failed: {e}");
    }
    let _ = stream.set_nodelay(true);

    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status: ActorStatus::Connected,
        telemetry: HashMap::from([("peer".into(), peer_addr.to_string())]),
    }));

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

        match stream.read(&mut chunk) {
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
                if !drain(&mut buf, &mut stream, &mut peer, sender, name) {
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
                if !handle(msg, raw, stream, peer, sender, name) {
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
    if is_shot {
        emit_shot(&msg, &payload, sender);
    }

    let reply = api::GsProResponse::ok(if is_shot {
        "Shot received"
    } else {
        "Heartbeat received"
    });
    match serde_json::to_vec(&reply) {
        Ok(bytes) => {
            if let Ok(text) = std::str::from_utf8(&bytes) {
                tracing::info!(target: "audit", "{name} sent {text} | {reply:?}");
            }
            if let Err(e) = stream.write_all(&bytes).and_then(|()| stream.flush()) {
                tracing::warn!("openconnect server: write failed: {e}");
                return false;
            }
        }
        Err(e) => tracing::warn!("openconnect server: could not encode response: {e}"),
    }
    true
}

/// Publish the standard shot lifecycle for one Open Connect shot message.
fn emit_shot(msg: &api::GsProMessage, payload: &RawPayload, sender: &BusSender) {
    let device = msg.device_id.as_str();
    let key = ShotKey {
        shot_id: uuid::Uuid::new_v4().to_string(),
        shot_number: msg.shot_number,
    };

    let ball = mapper::map_ball(msg);
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
        FlighthookMessage::new(FlighthookEvent::BallFlight {
            key: key.clone(),
            ball: Box::new(ball),
        })
        .raw(payload.clone())
        .device(device),
    );

    if msg.shot_data_options.contains_club_data {
        sender.send(
            FlighthookMessage::new(FlighthookEvent::ClubPath {
                key: key.clone(),
                club: Box::new(mapper::map_club(msg)),
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
        FlighthookMessage::new(FlighthookEvent::ShotFinished { key })
            .raw(payload.clone())
            .device(device),
    );
}
