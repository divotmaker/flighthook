//! Axum web server — REST endpoints + WebSocket event streaming.

pub mod export;
pub mod routes;
pub mod types;
pub mod ws;

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use axum::Router;
use axum::routing::{get, post};
use tokio::sync::{RwLock, broadcast};
use tower_http::cors::CorsLayer;

use crate::actors::{Actor, ReconfigureOutcome, actor_names};
use crate::bus::{BusReceiver, BusSender};
use crate::state::SystemState;
use flighthook::{
    ActorStatus, ActorStatusResponse, Club, ExportDevice, FlighthookConfig, FlighthookEvent,
    FlighthookMessage, Handedness, ShotAccumulator, ShotData, ShotDetectionMode, ShotKey, ShotRaw,
    SimulationSection,
};

/// Shots kept in the session history.
pub const MAX_SHOTS: usize = 1000;

/// Memory held by raw frames across the session history, as counted by
/// [`raw_held_bytes`]. Beyond this, the oldest shots lose their raw frames
/// (their decoded data stays).
pub const RAW_BUDGET_BYTES: usize = 16 * 1024 * 1024;

/// Estimated memory per stored frame beyond its hex or text: the record and
/// its metadata strings (type, src, dst, channel) with allocator overhead.
pub const RAW_RECORD_OVERHEAD: usize = 256;

/// Memory a shot's raw frames hold, as charged to [`RAW_BUDGET_BYTES`]: the
/// length of each frame's hex (two characters per payload byte) and text,
/// plus [`RAW_RECORD_OVERHEAD`] per frame.
fn raw_held_bytes(raw: &ShotRaw) -> usize {
    raw.frames
        .iter()
        .map(|r| {
            r.hex.as_ref().map_or(0, String::len)
                + r.text.as_ref().map_or(0, String::len)
                + RAW_RECORD_OVERHEAD
        })
        .sum()
}

/// `ShotRaw.unavailable` for a shot whose frames were evicted.
pub const RAW_EVICTED: &str = "evicted: session raw budget";

/// Everything the web layer keeps per shot. `/api/shots` serialises `data`
/// only; the export serialises all of it.
#[derive(Debug, Clone)]
pub struct StoredShot {
    /// When `shot_trigger` reached the web layer.
    pub received_at: DateTime<Utc>,
    pub key: ShotKey,
    /// What was true when the trigger arrived.
    pub context: ShotContext,
    pub data: ShotData,
    pub raw: Option<ShotRaw>,
}

impl StoredShot {
    fn raw_len(&self) -> usize {
        self.raw.as_ref().map_or(0, raw_held_bytes)
    }
}

/// Settings and identity in effect when a shot was triggered.
#[derive(Debug, Clone, Default)]
pub struct ShotContext {
    /// Kind, maker, model and firmware only.
    pub device: ExportDevice,
    /// Club in play from game state.
    pub club_selected: Option<Club>,
    pub handed: Option<Handedness>,
    pub detection_mode: Option<ShotDetectionMode>,
    /// The actor's config section with `name`, `address` and `bind` removed.
    pub device_section: serde_json::Value,
    pub simulation: SimulationSection,
    /// Identity strings scrubbed from text payloads and device labels on
    /// export: device IDs, configured and peer addresses.
    pub scrub: Vec<String>,
}

/// A shot between `shot_trigger` and `shot_finished`.
pub(crate) struct PendingShot {
    acc: ShotAccumulator,
    received_at: DateTime<Utc>,
    context: ShotContext,
}

pub(crate) type PendingShots = HashMap<(String, ShotKey), PendingShot>;

fn new_actor(name: String) -> ActorStatusResponse {
    ActorStatusResponse {
        name,
        status: ActorStatus::Disconnected,
        telemetry: HashMap::new(),
    }
}

/// Shared state for the web layer.
pub struct WebState {
    pub root: Arc<SystemState>,
    pub bus_tx: broadcast::Sender<FlighthookMessage>,
    pub actors: RwLock<HashMap<String, ActorStatusResponse>>,
    pub shots: RwLock<VecDeque<StoredShot>>,
    /// Raw frame memory held in `shots`, as counted by [`raw_held_bytes`].
    pub raw_bytes: AtomicUsize,
    /// Device identity per actor, merged from `DeviceTelemetry` (which sends
    /// it once, on connect).
    pub device_identity: RwLock<HashMap<String, ExportDevice>>,
    /// Last `ActorStatus` message per actor — replayed to new WS clients.
    pub cached_actor_status: RwLock<HashMap<String, FlighthookMessage>>,
    /// Last `DeviceTelemetry` message per actor — replayed to new WS clients.
    pub cached_device_telemetry: RwLock<HashMap<String, FlighthookMessage>>,
    pub addr: SocketAddr,
    pub actor_id: String,
    pub ws_count: AtomicU64,
    pub request_count: AtomicU64,
}

/// Emit current telemetry as an ActorStatus event on the bus.
fn emit_status(
    status: ActorStatus,
    state: &WebState,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
) {
    let mut telemetry = HashMap::from([
        ("bind".into(), state.addr.to_string()),
        (
            "websockets".into(),
            state.ws_count.load(Ordering::Relaxed).to_string(),
        ),
        (
            "requests".into(),
            state.request_count.load(Ordering::Relaxed).to_string(),
        ),
    ]);
    if status == ActorStatus::Disconnected {
        telemetry.insert("error".into(), "bind failed".into());
    }
    let _ = bus_tx.send(
        FlighthookMessage::new(FlighthookEvent::ActorStatus { status, telemetry })
            .actor(&state.actor_id),
    );
}

/// Emit Connected telemetry (convenience for periodic emitter + ws handlers).
pub(super) fn emit_telemetry(state: &WebState, bus_tx: &broadcast::Sender<FlighthookMessage>) {
    emit_status(ActorStatus::Connected, state, bus_tx);
}

// ---------------------------------------------------------------------------
// WebActor — wraps the axum web server as a normal actor
// ---------------------------------------------------------------------------

/// Web server actor. Spawns a dedicated thread with its own tokio runtime
/// to run the axum server and state_updater task.
pub struct WebActor {
    addr: SocketAddr,
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl WebActor {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            shutdown_tx: Mutex::new(None),
        }
    }
}

impl Actor for WebActor {
    fn start(&self, state: Arc<SystemState>, sender: BusSender, _receiver: BusReceiver) {
        let addr = self.addr;
        let actor_id = sender.actor_id().to_string();
        let bus_tx = sender.raw_sender().clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        *self.shutdown_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(shutdown_tx);

        let thread_name = actor_id.clone();
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let rt = tokio::runtime::Runtime::new()
                    .expect("failed to create webserver tokio runtime");
                rt.block_on(run(addr, actor_id, state, bus_tx, shutdown_rx));
            })
            .expect("failed to spawn webserver thread");
    }

    fn stop(&self) {
        if let Some(tx) = self
            .shutdown_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = tx.send(());
        }
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let snap = state.system.snapshot();
        let index = sender.actor_id().strip_prefix("webserver.").unwrap_or("0");
        let new_bind = snap.webserver.get(index).map(|w| w.bind.as_str());
        match new_bind.and_then(|b| b.parse::<SocketAddr>().ok()) {
            Some(new_addr) if new_addr == self.addr => ReconfigureOutcome::Applied,
            _ => ReconfigureOutcome::RestartRequired,
        }
    }
}

// ---------------------------------------------------------------------------
// Web server run loop
// ---------------------------------------------------------------------------

/// Run the web server. Blocks until shutdown signal or bus close.
async fn run(
    addr: SocketAddr,
    actor_id: String,
    root: Arc<SystemState>,
    bus_tx: broadcast::Sender<FlighthookMessage>,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    // Pre-populate per-actor state from config
    let snap = root.system.snapshot();
    let names = actor_names(&snap);
    let mut actors = HashMap::new();
    for (id, name) in &names {
        actors.insert(id.clone(), new_actor(name.clone()));
    }

    let state = Arc::new(WebState {
        root,
        bus_tx: bus_tx.clone(),
        actors: RwLock::new(actors),
        shots: RwLock::new(VecDeque::with_capacity(MAX_SHOTS)),
        raw_bytes: AtomicUsize::new(0),
        device_identity: RwLock::new(HashMap::new()),
        cached_actor_status: RwLock::new(HashMap::new()),
        cached_device_telemetry: RwLock::new(HashMap::new()),
        addr,
        actor_id,
        ws_count: AtomicU64::new(0),
        request_count: AtomicU64::new(0),
    });

    // Background task: subscribe to bus and update web state
    let updater_state = Arc::clone(&state);
    let bus_rx = bus_tx.subscribe();
    tokio::spawn(state_updater(updater_state, bus_rx));

    // Periodic telemetry emitter (every 5s)
    let telemetry_state = Arc::clone(&state);
    let telemetry_bus = bus_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            emit_telemetry(&telemetry_state, &telemetry_bus);
        }
    });

    // Request counter middleware
    let counter_state = Arc::clone(&state);
    let count_middleware = axum::middleware::from_fn(move |req, next: axum::middleware::Next| {
        let st = Arc::clone(&counter_state);
        async move {
            st.request_count.fetch_add(1, Ordering::Relaxed);
            next.run(req).await
        }
    });

    let app = Router::new()
        .route("/", get(routes::get_ui_html))
        .route("/flighthook-ui.js", get(routes::get_ui_js))
        .route("/flighthook-ui_bg.wasm", get(routes::get_ui_wasm))
        .route("/api/status", get(routes::get_status))
        .route("/api/shots", get(routes::get_shots))
        .route("/api/shots/export", get(routes::get_shots_export))
        .route("/api/shots/convert", post(routes::post_convert_shot))
        .route("/api/mode", post(routes::post_mode))
        .route(
            "/api/settings",
            get(routes::get_settings).post(routes::post_settings),
        )
        .route(flighthook::FRP_PATH, get(ws::ws_upgrade))
        .layer(count_middleware)
        .layer(CorsLayer::permissive())
        .with_state(Arc::clone(&state));

    // Retry bind until success or shutdown
    let mut shutdown_rx = shutdown_rx;
    let listener = loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => break l,
            Err(e) => {
                tracing::warn!("web server: failed to bind {addr}: {e}, retrying in 3s");
                emit_status(ActorStatus::Disconnected, &state, &bus_tx);
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => continue,
                    _ = &mut shutdown_rx => return,
                }
            }
        }
    };

    tracing::info!("web server listening on {addr}");
    emit_status(ActorStatus::Connected, &state, &bus_tx);

    axum::serve(listener, app)
        .with_graceful_shutdown(async { drop(shutdown_rx.await) })
        .await
        .ok();
}

/// Background task that subscribes to the bus and keeps WebState current.
async fn state_updater(state: Arc<WebState>, mut bus_rx: broadcast::Receiver<FlighthookMessage>) {
    let mut accumulators = PendingShots::new();

    loop {
        match bus_rx.recv().await {
            Ok(msg) => apply_bus_event(&state, &msg, &mut accumulators).await,
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("web state updater: lagged, dropped {n} events");
            }
        }
    }
}

/// Stable device kind for an actor type prefix.
fn device_kind(prefix: &str) -> &str {
    match prefix {
        "mevo" => "flightscope",
        "square" => "square_golf",
        "r10" => "garmin_r10",
        "openconnect_server" => "openconnect",
        "mock_monitor" => "mock",
        other => other,
    }
}

/// The actor's config section with `name`, `address`, `bind` and unset
/// fields removed, plus the removed address values (to scrub on export).
fn device_section(
    config: &FlighthookConfig,
    prefix: &str,
    index: &str,
) -> (serde_json::Value, Vec<String>) {
    fn value<T: serde::Serialize>(section: Option<&T>) -> serde_json::Value {
        section
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()))
    }
    let mut section = match prefix {
        "mevo" => value(config.mevo.get(index)),
        "square" => value(config.square.get(index)),
        "r10" => value(config.r10.get(index)),
        "openconnect_server" => value(config.openconnect_server.get(index)),
        "mock_monitor" => value(config.mock_monitor.get(index)),
        _ => value::<()>(None),
    };
    let mut removed = Vec::new();
    if let Some(map) = section.as_object_mut() {
        map.retain(|_, v| !v.is_null());
        map.remove("name");
        for key in ["address", "bind"] {
            if let Some(serde_json::Value::String(v)) = map.remove(key) {
                removed.push(v);
            }
        }
    }
    (section, removed)
}

/// Snapshot the settings and identity in effect for a shot from `msg.actor`.
async fn shot_context(state: &WebState, msg: &FlighthookMessage) -> ShotContext {
    let game = state.root.game.snapshot();
    let config = state.root.system.snapshot();
    let (prefix, index) = msg
        .actor
        .split_once('.')
        .unwrap_or((msg.actor.as_str(), ""));

    let mut device = state
        .device_identity
        .read()
        .await
        .get(&msg.actor)
        .cloned()
        .unwrap_or_default();
    device.kind = device_kind(prefix).to_string();

    let (device_section, mut scrub) = device_section(&config, prefix, index);
    // The FRP device ID identifies the unit (network name, BLE name or
    // address). Open Connect's is the monitor's product label, and is kept.
    if prefix != "openconnect_server"
        && let Some(id) = &msg.device
    {
        scrub.push(id.clone());
    }
    if let Some(actor) = state.actors.read().await.get(&msg.actor) {
        for key in ["ble_address", "peer"] {
            if let Some(v) = actor.telemetry.get(key) {
                scrub.push(v.clone());
            }
        }
    }
    // An address may also appear without its port.
    let hosts: Vec<String> = scrub
        .iter()
        .filter_map(|s| s.parse::<SocketAddr>().ok())
        .map(|a| a.ip())
        .filter(|ip| !ip.is_unspecified())
        .map(|ip| ip.to_string())
        .collect();
    scrub.extend(hosts);
    scrub.sort();
    scrub.dedup();

    ShotContext {
        device,
        club_selected: game.club_info.map(|c| c.club),
        handed: game.handed,
        detection_mode: game.mode,
        device_section,
        simulation: config.simulation,
        scrub,
    }
}

/// Append a shot to the history, keeping it within [`MAX_SHOTS`] and the
/// raw byte `budget`. Over budget, the oldest shots lose their raw frames
/// first; their decoded data is kept.
fn store_shot(
    shots: &mut VecDeque<StoredShot>,
    raw_bytes: &AtomicUsize,
    shot: StoredShot,
    budget: usize,
) {
    let mut total = raw_bytes.load(Ordering::Relaxed);
    if shots.len() >= MAX_SHOTS
        && let Some(old) = shots.pop_front()
    {
        total = total.saturating_sub(old.raw_len());
    }
    total += shot.raw_len();
    shots.push_back(shot);
    for s in shots.iter_mut() {
        if total <= budget {
            break;
        }
        if let Some(raw) = s.raw.as_mut()
            && !raw.frames.is_empty()
        {
            total = total.saturating_sub(raw_held_bytes(raw));
            raw.frames = Vec::new();
            raw.unavailable = Some(RAW_EVICTED.into());
        }
    }
    raw_bytes.store(total, Ordering::Relaxed);
}

async fn apply_bus_event(
    state: &WebState,
    msg: &FlighthookMessage,
    accumulators: &mut PendingShots,
) {
    match &msg.event {
        FlighthookEvent::ActorStatus { status, telemetry } => {
            let mut actors = state.actors.write().await;
            let actor = actors.entry(msg.actor.clone()).or_insert_with(|| {
                let snap = state.root.system.snapshot();
                let names = actor_names(&snap);
                let name = names.get(&msg.actor).cloned().unwrap_or_default();
                new_actor(name)
            });
            actor.status = *status;
            for (k, v) in telemetry {
                actor.telemetry.insert(k.clone(), v.clone());
            }
            // Cache the raw message for WS replay
            state
                .cached_actor_status
                .write()
                .await
                .insert(msg.actor.clone(), msg.clone());
        }
        FlighthookEvent::ShotTrigger { key } => {
            let pending = PendingShot {
                acc: ShotAccumulator::new(msg.actor.clone(), key.clone()),
                received_at: Utc::now(),
                context: shot_context(state, msg).await,
            };
            accumulators.insert((msg.actor.clone(), key.clone()), pending);
        }
        FlighthookEvent::BallFlight {
            key, ball, derived, ..
        } => {
            if let Some(p) = accumulators.get_mut(&(msg.actor.clone(), key.clone())) {
                p.acc.set_ball(*ball.clone());
                p.acc.set_derived(derived.as_deref().cloned());
            }
        }
        FlighthookEvent::FaceImpact { key, impact } => {
            if let Some(p) = accumulators.get_mut(&(msg.actor.clone(), key.clone())) {
                p.acc.set_impact(*impact.clone());
            }
        }
        FlighthookEvent::ClubPath { key, club } => {
            if let Some(p) = accumulators.get_mut(&(msg.actor.clone(), key.clone())) {
                p.acc.set_club(*club.clone());
            }
        }
        FlighthookEvent::ShotFinished { key, raw } => {
            let Some(PendingShot {
                mut acc,
                received_at,
                context,
            }) = accumulators.remove(&(msg.actor.clone(), key.clone()))
            else {
                return;
            };
            acc.set_raw(raw.as_deref().cloned());
            let (data, raw) = acc.into_parts();
            // A shot the device read nothing for is kept for the export when
            // its raw frames are; `/api/shots` leaves it out.
            let data = match (data, &raw) {
                (Some(data), _) => data,
                (None, Some(_)) => ShotData {
                    actor: msg.actor.clone(),
                    shot_number: key.shot_number,
                    ball: None,
                    club: None,
                    impact: None,
                    derived: None,
                },
                (None, None) => return,
            };
            let shot = StoredShot {
                received_at,
                key: key.clone(),
                context,
                data,
                raw,
            };
            let mut shots = state.shots.write().await;
            store_shot(&mut shots, &state.raw_bytes, shot, RAW_BUDGET_BYTES);
        }
        FlighthookEvent::PlayerInfo { player_info } => {
            if let Some(ref name) = player_info.name {
                let mut actors = state.actors.write().await;
                if let Some(actor) = actors.get_mut(&msg.actor) {
                    actor.telemetry.insert("name".into(), name.clone());
                }
            }
        }
        FlighthookEvent::ClubInfo { club_info } => {
            let mut actors = state.actors.write().await;
            if let Some(actor) = actors.get_mut(&msg.actor) {
                actor
                    .telemetry
                    .insert("club".into(), club_info.club.to_string());
            }
        }
        FlighthookEvent::ConfigOutcome {
            started,
            stopped,
            restarted,
            ..
        } => {
            if !started.is_empty() || !stopped.is_empty() || !restarted.is_empty() {
                // Refresh actor names from config
                let snap = state.root.system.snapshot();
                let names = actor_names(&snap);

                let mut actors = state.actors.write().await;
                for (id, name) in &names {
                    let entry = actors
                        .entry(id.clone())
                        .or_insert_with(|| new_actor(name.clone()));
                    entry.name = name.clone();
                }
                // Remove stopped actors
                for id in stopped {
                    actors.remove(id);
                }
                drop(actors);
                // Clean up cached messages for stopped actors
                for id in stopped {
                    state.cached_actor_status.write().await.remove(id);
                    state.cached_device_telemetry.write().await.remove(id);
                    state.device_identity.write().await.remove(id);
                }
            }
        }
        FlighthookEvent::DeviceTelemetry {
            manufacturer,
            model,
            firmware,
            telemetry,
        } if manufacturer.is_some() || model.is_some() || firmware.is_some() => {
            {
                let mut ids = state.device_identity.write().await;
                let id = ids.entry(msg.actor.clone()).or_default();
                if manufacturer.is_some() {
                    id.manufacturer.clone_from(manufacturer);
                }
                if model.is_some() {
                    id.model.clone_from(model);
                }
                if firmware.is_some() {
                    id.firmware.clone_from(firmware);
                }
            }
            if let Some(tel) = telemetry {
                let mut actors = state.actors.write().await;
                if let Some(actor) = actors.get_mut(&msg.actor) {
                    for (k, v) in tel {
                        actor.telemetry.insert(k.clone(), v.clone());
                    }
                }
            }
            state
                .cached_device_telemetry
                .write()
                .await
                .insert(msg.actor.clone(), msg.clone());
        }
        FlighthookEvent::DeviceTelemetry {
            telemetry: Some(tel),
            ..
        } => {
            let mut actors = state.actors.write().await;
            if let Some(actor) = actors.get_mut(&msg.actor) {
                for (k, v) in tel {
                    actor.telemetry.insert(k.clone(), v.clone());
                }
            }
            // Cache the raw message for WS replay
            state
                .cached_device_telemetry
                .write()
                .await
                .insert(msg.actor.clone(), msg.clone());
        }
        FlighthookEvent::DeviceTelemetry {
            telemetry: None, ..
        } => {
            // Cache identity-only DeviceTelemetry too
            state
                .cached_device_telemetry
                .write()
                .await
                .insert(msg.actor.clone(), msg.clone());
        }
        // Alert, SetDetectionMode, ConfigCommand — no web state update needed
        _ => {}
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::state::GameStateWriter;
    use flighthook::{
        BallFlight, ClubInfo, DerivedFlight, Distance, MevoSection, RawDir, RawRecord, Velocity,
    };

    /// Removes the directory on drop, so a failing assertion doesn't leak it.
    pub(crate) struct TempDir(pub(crate) std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn test_state() -> (WebState, GameStateWriter, TempDir) {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-web-test-{}",
            crate::state::config::generate_id()
        )));
        let (root, writer) = SystemState::new(dir.0.join("config.toml"));
        let (bus_tx, _) = broadcast::channel(16);
        let state = WebState {
            root: Arc::new(root),
            bus_tx,
            actors: RwLock::new(HashMap::new()),
            shots: RwLock::new(VecDeque::new()),
            raw_bytes: AtomicUsize::new(0),
            device_identity: RwLock::new(HashMap::new()),
            cached_actor_status: RwLock::new(HashMap::new()),
            cached_device_telemetry: RwLock::new(HashMap::new()),
            addr: "127.0.0.1:0".parse().expect("addr"),
            actor_id: "webserver.0".into(),
            ws_count: AtomicU64::new(0),
            request_count: AtomicU64::new(0),
        };
        (state, writer, dir)
    }

    fn key(n: u32) -> ShotKey {
        ShotKey {
            shot_id: format!("shot-{n}"),
            shot_number: n,
        }
    }

    fn raw(frames: usize, bytes_each: usize) -> ShotRaw {
        ShotRaw {
            transport: "flightscope-tcp".into(),
            frames: (0..frames)
                .map(|_| RawRecord {
                    r#type: Some("0xD4".into()),
                    ..RawRecord::binary(RawDir::Rx, &vec![0x5a; bytes_each])
                })
                .collect(),
            ..ShotRaw::default()
        }
    }

    pub(crate) async fn feed(state: &WebState, actor: &str, events: Vec<FlighthookEvent>) {
        let mut acc = PendingShots::new();
        for event in events {
            let msg = FlighthookMessage::new(event)
                .actor(actor)
                .device("FS M2-000001");
            apply_bus_event(state, &msg, &mut acc).await;
        }
    }

    fn ball() -> BallFlight {
        BallFlight {
            launch_speed: Some(Velocity::MetersPerSecond(40.0)),
            launch_elevation: Some(20.0),
            ..BallFlight::default()
        }
    }

    #[tokio::test]
    async fn shot_history_keeps_derived_values() {
        let (state, _writer, _dir) = test_state();
        let mut derived = DerivedFlight::default();
        derived.carry_distance = Some(Distance::Meters(150.9));
        feed(
            &state,
            "square.0",
            vec![
                FlighthookEvent::ShotTrigger { key: key(1) },
                FlighthookEvent::BallFlight {
                    key: key(1),
                    ball: Box::new(BallFlight::default()),
                    derived: Some(Box::new(derived.clone())),
                },
                FlighthookEvent::ShotFinished {
                    key: key(1),
                    raw: None,
                },
            ],
        )
        .await;
        let shots = state.shots.read().await;
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].data.derived, Some(derived));
        assert!(shots[0].raw.is_none());
    }

    #[tokio::test]
    async fn trigger_snapshots_context_and_finish_stores_raw() {
        let (state, writer, _dir) = test_state();
        writer.set_club_info(ClubInfo { club: Club::Iron8 });
        writer.set_mode(ShotDetectionMode::Full);
        writer.set_handed(Handedness::Right);
        state.root.system.update(|c| {
            c.mevo.insert(
                "0".into(),
                MevoSection {
                    name: "Range Mevo".into(),
                    address: Some("10.0.0.7:5100".into()),
                    ball_type: Some(1),
                    tee_height: None,
                    range: None,
                    surface_height: None,
                    track_pct: Some(80.0),
                    use_estimated: None,
                    camera_mode: None,
                    environment: None,
                },
            );
        });
        apply_bus_event(
            &state,
            &FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                manufacturer: Some("FlightScope".into()),
                model: Some("Xi".into()),
                firmware: None,
                telemetry: None,
            })
            .actor("mevo.0"),
            &mut PendingShots::new(),
        )
        .await;

        let mut acc = PendingShots::new();
        let msg = |event| {
            FlighthookMessage::new(event)
                .actor("mevo.0")
                .device("FS M2-000001")
        };
        apply_bus_event(
            &state,
            &msg(FlighthookEvent::ShotTrigger { key: key(1) }),
            &mut acc,
        )
        .await;
        // A club change after the trigger does not change the shot's club.
        writer.set_club_info(ClubInfo { club: Club::Driver });
        for event in [
            FlighthookEvent::BallFlight {
                key: key(1),
                ball: Box::new(ball()),
                derived: None,
            },
            FlighthookEvent::ShotFinished {
                key: key(1),
                raw: Some(Box::new(raw(3, 4))),
            },
        ] {
            apply_bus_event(&state, &msg(event), &mut acc).await;
        }

        let shots = state.shots.read().await;
        let s = &shots[0];
        assert!(s.received_at <= Utc::now());
        assert_eq!(s.key, key(1));
        assert_eq!(s.context.club_selected, Some(Club::Iron8));
        assert_eq!(s.context.detection_mode, Some(ShotDetectionMode::Full));
        assert_eq!(s.context.handed, Some(Handedness::Right));
        assert_eq!(s.context.device.kind, "flightscope");
        assert_eq!(
            s.context.device.manufacturer.as_deref(),
            Some("FlightScope")
        );
        assert_eq!(s.context.device.model.as_deref(), Some("Xi"));
        let section = s.context.device_section.as_object().expect("object");
        assert!(section.get("name").is_none());
        assert!(section.get("address").is_none());
        assert!(
            section.get("tee_height").is_none(),
            "unset fields are left out"
        );
        assert_eq!(section["ball_type"], 1);
        for id in ["FS M2-000001", "10.0.0.7:5100", "10.0.0.7"] {
            assert!(s.context.scrub.iter().any(|x| x == id), "scrub {id}");
        }
        assert_eq!(s.raw.as_ref().expect("raw").frames.len(), 3);
        assert_eq!(
            state.raw_bytes.load(Ordering::Relaxed),
            3 * (8 + RAW_RECORD_OVERHEAD)
        );
    }

    #[tokio::test]
    async fn dataless_shot_with_raw_is_stored_but_hidden_from_api_shots() {
        let (state, _writer, _dir) = test_state();
        feed(
            &state,
            "mevo.0",
            vec![
                FlighthookEvent::ShotTrigger { key: key(1) },
                FlighthookEvent::ShotFinished {
                    key: key(1),
                    raw: Some(Box::new(raw(2, 8))),
                },
                FlighthookEvent::ShotTrigger { key: key(2) },
                FlighthookEvent::ShotFinished {
                    key: key(2),
                    raw: None,
                },
            ],
        )
        .await;
        {
            let shots = state.shots.read().await;
            assert_eq!(shots.len(), 1, "no data and no raw: nothing stored");
            assert!(!shots[0].data.has_data());
            assert_eq!(shots[0].data.shot_number, 1);
        }
        let state = Arc::new(state);
        let shots = routes::get_shots(
            axum::extract::State(Arc::clone(&state)),
            axum::extract::Query(routes::ShotsQuery {
                limit: 50,
                units: None,
            }),
        )
        .await;
        assert!(shots.0.is_empty());
    }

    #[tokio::test]
    async fn api_shots_json_is_the_shot_data() {
        let (state, _writer, _dir) = test_state();
        feed(
            &state,
            "mevo.0",
            vec![
                FlighthookEvent::ShotTrigger { key: key(1) },
                FlighthookEvent::BallFlight {
                    key: key(1),
                    ball: Box::new(ball()),
                    derived: None,
                },
                FlighthookEvent::ShotFinished {
                    key: key(1),
                    raw: Some(Box::new(raw(2, 2))),
                },
            ],
        )
        .await;
        let data = state.shots.read().await[0].data.clone();
        let state = Arc::new(state);
        let shots = routes::get_shots(
            axum::extract::State(state),
            axum::extract::Query(routes::ShotsQuery {
                limit: 50,
                units: None,
            }),
        )
        .await;
        assert_eq!(
            serde_json::to_string(&shots.0).expect("ser"),
            format!("[{}]", serde_json::to_string(&data).expect("ser"))
        );
        assert!(
            !serde_json::to_string(&shots.0)
                .expect("ser")
                .contains("raw")
        );
    }

    fn stored(n: u32, raw: Option<ShotRaw>) -> StoredShot {
        StoredShot {
            received_at: Utc::now(),
            key: key(n),
            context: ShotContext::default(),
            data: ShotData {
                actor: "mevo.0".into(),
                shot_number: n,
                ball: Some(ball()),
                club: None,
                impact: None,
                derived: None,
            },
            raw,
        }
    }

    #[test]
    fn raw_budget_evicts_oldest_raw_and_keeps_data() {
        let mut shots = VecDeque::new();
        let bytes = AtomicUsize::new(0);
        // Each shot holds 10 frames of 10 payload bytes: 20 hex characters
        // plus the per-record overhead. The budget fits two.
        let per_shot = 10 * (20 + RAW_RECORD_OVERHEAD);
        assert_eq!(raw_held_bytes(&raw(10, 10)), per_shot);
        for n in 1..=3 {
            store_shot(
                &mut shots,
                &bytes,
                stored(n, Some(raw(10, 10))),
                2 * per_shot + per_shot / 2,
            );
        }
        assert_eq!(shots.len(), 3);
        let first = shots[0].raw.as_ref().expect("raw kept");
        assert!(first.frames.is_empty());
        assert_eq!(first.unavailable.as_deref(), Some(RAW_EVICTED));
        assert_eq!(first.transport, "flightscope-tcp");
        assert!(shots[0].data.has_data());
        assert_eq!(shots[1].raw.as_ref().expect("raw").frames.len(), 10);
        assert_eq!(shots[2].raw.as_ref().expect("raw").frames.len(), 10);
        assert_eq!(bytes.load(Ordering::Relaxed), 2 * per_shot);
    }

    #[test]
    fn shot_cap_releases_raw_bytes() {
        let mut shots = VecDeque::new();
        let bytes = AtomicUsize::new(0);
        for n in 0..=u32::try_from(MAX_SHOTS).expect("fits") {
            store_shot(
                &mut shots,
                &bytes,
                stored(n, Some(raw(1, 1))),
                RAW_BUDGET_BYTES,
            );
        }
        assert_eq!(shots.len(), MAX_SHOTS);
        assert_eq!(shots[0].key.shot_number, 1);
        assert_eq!(
            bytes.load(Ordering::Relaxed),
            MAX_SHOTS * (2 + RAW_RECORD_OVERHEAD)
        );
    }
}
