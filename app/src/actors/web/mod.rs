//! Axum web server — REST endpoints + WebSocket event streaming.

pub mod export;
pub mod groups;
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
    FlighthookMessage, GroupKey, GroupedShot, Handedness, ShotAccumulator, ShotData,
    ShotDetectionMode, ShotGroupInfo, ShotKey, ShotRaw, SimulationSection,
};
use groups::GroupState;

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
    /// The last club seen on the bus (`club_info`) when the shot triggered.
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
    /// Set assigned at the trigger.
    group: String,
}

pub(crate) type PendingShots = HashMap<(String, ShotKey), PendingShot>;

fn new_actor(name: String) -> ActorStatusResponse {
    ActorStatusResponse::new(name, ActorStatus::Disconnected)
}

/// Give each actor in `actors` its name from `names` (from
/// [`actor_names`]); an actor `names` leaves out keeps its own.
fn rename_actors(
    actors: &mut HashMap<String, ActorStatusResponse>,
    names: &HashMap<String, String>,
) {
    for (id, actor) in actors.iter_mut() {
        if let Some(name) = names.get(id) {
            actor.name.clone_from(name);
        }
    }
}

/// The actors as `/api/status` reports them at `now_ms`: each with its name
/// from `names` (the current config) and `state` and `red_for_ms` from its
/// history.
pub(crate) fn actor_statuses(
    actors: &HashMap<String, ActorStatusResponse>,
    names: &HashMap<String, String>,
    now_ms: u64,
) -> HashMap<String, ActorStatusResponse> {
    let mut actors = actors.clone();
    rename_actors(&mut actors, names);
    for a in actors.values_mut() {
        a.stamp(now_ms);
    }
    actors
}

/// Shared state for the web layer.
pub struct WebState {
    pub root: Arc<SystemState>,
    pub bus_tx: broadcast::Sender<FlighthookMessage>,
    /// Per-actor state, with each actor's connection history since it last
    /// started (kept across UI connections).
    pub actors: RwLock<HashMap<String, ActorStatusResponse>>,
    /// Origin of the connection-history clock ([`WebState::now_ms`]).
    pub epoch: std::time::Instant,
    pub shots: RwLock<VecDeque<StoredShot>>,
    /// Shot sets over `shots`. Lock order where both are held: `groups`,
    /// then `shots`.
    pub groups: RwLock<GroupState>,
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

impl WebState {
    /// Milliseconds since [`epoch`](Self::epoch): the clock of the actors'
    /// connection histories.
    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
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
    // An empty value removes the key, so a successful bind clears the error.
    let error = if status == ActorStatus::Disconnected {
        "bind failed"
    } else {
        ""
    };
    telemetry.insert("error".into(), error.into());
    let _ = bus_tx.send(
        FlighthookMessage::new(FlighthookEvent::ActorStatus { status, telemetry })
            .actor(&state.actor_id),
    );
}

/// Emit a `shot_group` event for a changed group record, as this webserver.
fn emit_group(state: &WebState, group: ShotGroupInfo, shot: Option<GroupedShot>) {
    let _ = state.bus_tx.send(
        FlighthookMessage::new(FlighthookEvent::ShotGroup { group, shot }).actor(&state.actor_id),
    );
}

/// Host-clock RFC 3339 UTC timestamp for a group's `started_at`.
fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A `GroupState` for the club in play.
fn initial_groups(root: &SystemState) -> GroupState {
    let mut groups = GroupState::new();
    groups.select(root.game.snapshot().club_info.map(|c| c.club));
    groups
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
        // Subscribed here, before `start` returns, so no status an actor
        // started after this one emits is missed.
        let bus_rx = bus_tx.subscribe();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        *self.shutdown_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(shutdown_tx);

        let thread_name = actor_id.clone();
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let rt = tokio::runtime::Runtime::new()
                    .expect("failed to create webserver tokio runtime");
                rt.block_on(run(addr, actor_id, state, bus_tx, bus_rx, shutdown_rx));
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
    bus_rx: broadcast::Receiver<FlighthookMessage>,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    // Pre-populate per-actor state from config
    let snap = root.system.snapshot();
    let names = actor_names(&snap);
    let mut actors = HashMap::new();
    for (id, name) in &names {
        actors.insert(id.clone(), new_actor(name.clone()));
    }

    let groups = initial_groups(&root);
    let state = Arc::new(WebState {
        root,
        bus_tx: bus_tx.clone(),
        actors: RwLock::new(actors),
        epoch: std::time::Instant::now(),
        shots: RwLock::new(VecDeque::with_capacity(MAX_SHOTS)),
        groups: RwLock::new(groups),
        raw_bytes: AtomicUsize::new(0),
        device_identity: RwLock::new(HashMap::new()),
        cached_actor_status: RwLock::new(HashMap::new()),
        cached_device_telemetry: RwLock::new(HashMap::new()),
        addr,
        actor_id,
        ws_count: AtomicU64::new(0),
        request_count: AtomicU64::new(0),
    });

    // Background task: keep web state current from the bus
    let updater_state = Arc::clone(&state);
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
        .route(
            "/api/shots/groups",
            get(routes::get_shot_groups).post(routes::post_new_shot_group),
        )
        .route("/api/shots/session", post(routes::post_new_shot_session))
        .route("/api/shots/clear", post(routes::post_clear_shot_history))
        .route("/api/shots/grouping", post(routes::post_shot_grouping))
        .route("/api/club", post(routes::post_club))
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

/// Background task that drains the bus and keeps WebState current.
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

/// Snapshot the settings and identity in effect for a shot from `msg.actor`,
/// hit with `club_selected`.
async fn shot_context(
    state: &WebState,
    msg: &FlighthookMessage,
    club_selected: Option<Club>,
) -> ShotContext {
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
        club_selected,
        handed: game.handed,
        detection_mode: game.mode,
        device_section,
        simulation: config.simulation,
        scrub,
    }
}

/// Append a shot to the history, keeping it within [`MAX_SHOTS`] and the
/// raw byte `budget`. Over budget, the oldest shots lose their raw frames
/// first; their decoded data is kept. Returns the shot that fell out of the
/// history, if any.
fn store_shot(
    shots: &mut VecDeque<StoredShot>,
    raw_bytes: &AtomicUsize,
    shot: StoredShot,
    budget: usize,
) -> Option<StoredShot> {
    let mut total = raw_bytes.load(Ordering::Relaxed);
    let evicted = if shots.len() >= MAX_SHOTS {
        shots.pop_front()
    } else {
        None
    };
    if let Some(old) = &evicted {
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
    evicted
}

/// End `actor`'s pending shots uncounted, emitting the group records that
/// changed.
async fn drop_pending(state: &WebState, accumulators: &mut PendingShots, actor: &str) {
    let groups: Vec<String> = accumulators
        .extract_if(|(a, _), _| a == actor)
        .map(|(_, p)| p.group)
        .collect();
    if groups.is_empty() {
        return;
    }
    let changes: Vec<ShotGroupInfo> = {
        let mut state_groups = state.groups.write().await;
        groups
            .iter()
            .filter_map(|id| state_groups.finish(id, false))
            .collect()
    };
    for group in changes {
        emit_group(state, group, None);
    }
}

/// Merge an `actor_status` event's telemetry into an actor's: each key
/// replaces the one held, and an empty value removes the key.
fn merge_status_telemetry(held: &mut HashMap<String, String>, update: &HashMap<String, String>) {
    for (k, v) in update {
        if v.is_empty() {
            held.remove(k);
        } else {
            held.insert(k.clone(), v.clone());
        }
    }
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
            actor.observe(*status, state.now_ms());
            merge_status_telemetry(&mut actor.telemetry, telemetry);
            // Cache the raw message for WS replay
            state
                .cached_actor_status
                .write()
                .await
                .insert(msg.actor.clone(), msg.clone());
            drop(actors);
            // Out of session, the device finishes none of its pending shots.
            if *status != ActorStatus::Connected {
                drop_pending(state, accumulators, &msg.actor).await;
            }
        }
        FlighthookEvent::ShotTrigger { key } => {
            // A device runs one shot at a time: a new trigger (or a repeat of
            // the same one) ends its earlier unfinished shots uncounted.
            drop_pending(state, accumulators, &msg.actor).await;
            // The set and the stored shot's club are decided now, from the
            // club the sets last saw on the bus, so commands and club
            // changes during the flight apply to the next shot.
            let (club, (group, changes)) = {
                let mut groups = state.groups.write().await;
                let club = groups.selected_club();
                let key_for_group = GroupKey { golfer: None, club };
                (club, groups.assign(key_for_group, &now_rfc3339()))
            };
            let context = shot_context(state, msg, club).await;
            let pending = PendingShot {
                acc: ShotAccumulator::new(msg.actor.clone(), key.clone()),
                received_at: Utc::now(),
                context,
                group,
            };
            accumulators.insert((msg.actor.clone(), key.clone()), pending);
            for group in changes {
                emit_group(state, group, None);
            }
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
                group,
            }) = accumulators.remove(&(msg.actor.clone(), key.clone()))
            else {
                return;
            };
            acc.set_raw(raw.as_deref().cloned());
            let (data, raw) = acc.into_parts();
            // A shot the device read nothing for is kept for the export when
            // its raw frames are; `/api/shots` leaves it out.
            let mut data = match (data, &raw) {
                (Some(data), _) => data,
                (None, Some(_)) => ShotData {
                    actor: msg.actor.clone(),
                    shot_number: key.shot_number,
                    ..ShotData::default()
                },
                (None, None) => {
                    if let Some(dropped) = state.groups.write().await.finish(&group, false) {
                        emit_group(state, dropped, None);
                    }
                    return;
                }
            };
            let counted = data.has_data();
            data.shot_id = Some(key.shot_id.clone());
            data.club_selected = context.club_selected;
            data.handed = context.handed;
            if counted {
                data.group = Some(group.clone());
            }
            let shot = StoredShot {
                received_at,
                key: key.clone(),
                context,
                data,
                raw,
            };

            let mut changes = Vec::new();
            {
                let mut groups = state.groups.write().await;
                if let Some(change) = groups.finish(&group, counted) {
                    let joined = counted.then(|| GroupedShot {
                        actor: msg.actor.clone(),
                        key: key.clone(),
                    });
                    changes.push((change, joined));
                }
                let mut shots = state.shots.write().await;
                let evicted = store_shot(&mut shots, &state.raw_bytes, shot, RAW_BUDGET_BYTES);
                if let Some(id) = evicted.and_then(|old| old.data.group)
                    && let Some(change) = groups.release(&id)
                {
                    changes.push((change, None));
                }
            }
            for (group, shot) in changes {
                emit_group(state, group, shot);
            }
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
            state.groups.write().await.select(Some(club_info.club));
            let mut actors = state.actors.write().await;
            if let Some(actor) = actors.get_mut(&msg.actor) {
                actor
                    .telemetry
                    .insert("club".into(), club_info.club.to_string());
            }
        }
        FlighthookEvent::NewShotGroup => {
            let changes = state.groups.write().await.new_group(&now_rfc3339());
            for group in changes {
                emit_group(state, group, None);
            }
        }
        FlighthookEvent::NewShotSession => {
            let changes = state.groups.write().await.new_session();
            for group in changes {
                emit_group(state, group, None);
            }
        }
        FlighthookEvent::ClearShotHistory => {
            // Shots in flight belong to the history being cleared: they end
            // here, so a late `shot_finished` finds nothing to store. No
            // `shot_group` is emitted; UIs clear on this event.
            accumulators.clear();
            let mut groups = state.groups.write().await;
            groups.clear();
            let mut shots = state.shots.write().await;
            shots.clear();
            state.raw_bytes.store(0, Ordering::Relaxed);
        }
        FlighthookEvent::ConfigOutcome {
            started,
            stopped,
            restarted,
            ..
        } => {
            // Names come from the config, and a rename is applied without a
            // restart: every outcome refreshes them.
            let names = actor_names(&state.root.system.snapshot());
            let mut actors = state.actors.write().await;
            rename_actors(&mut actors, &names);
            if !started.is_empty() || !stopped.is_empty() || !restarted.is_empty() {
                let now = state.now_ms();
                for (id, name) in &names {
                    actors
                        .entry(id.clone())
                        .or_insert_with(|| new_actor(name.clone()));
                }
                // A restarted actor begins a new history: no alert for the
                // connection its old instance lost.
                for id in restarted {
                    if let Some(entry) = actors.get_mut(id) {
                        entry.restart(now);
                    }
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
        // Alert, SetDetectionMode, ConfigCommand, ShotGroup — no web state
        // update needed
        _ => {}
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::state::GameStateWriter;
    use flighthook::{
        BallFlight, ClubInfo, DerivedFlight, Distance, MevoSection, RawDir, RawRecord,
        ShotGrouping, Velocity,
    };

    /// Removes the directory on drop, so a failing assertion doesn't leak it.
    pub(crate) struct TempDir(pub(crate) std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn status_telemetry_replaces_keys_and_an_empty_value_removes_one() {
        let mut held = HashMap::from([
            ("club".to_string(), "DR".to_string()),
            ("error".to_string(), "refused".to_string()),
        ]);
        merge_status_telemetry(
            &mut held,
            &HashMap::from([
                ("club".to_string(), "7I".to_string()),
                ("error".to_string(), String::new()),
                ("handed".to_string(), "rh".to_string()),
            ]),
        );
        assert_eq!(
            held,
            HashMap::from([
                ("club".to_string(), "7I".to_string()),
                ("handed".to_string(), "rh".to_string()),
            ])
        );
    }

    #[tokio::test]
    async fn a_successful_bind_clears_the_bind_error() {
        let (state, _writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        emit_status(ActorStatus::Disconnected, &state, &state.bus_tx);
        emit_status(ActorStatus::Connected, &state, &state.bus_tx);
        let mut pending = PendingShots::new();
        let msg = rx.recv().await.expect("disconnected");
        apply_bus_event(&state, &msg, &mut pending).await;
        assert_eq!(
            state.actors.read().await["webserver.0"].telemetry["error"],
            "bind failed"
        );
        let msg = rx.recv().await.expect("connected");
        apply_bus_event(&state, &msg, &mut pending).await;
        let actors = state.actors.read().await;
        let web = actors.get("webserver.0").expect("webserver status");
        assert_eq!(web.status, ActorStatus::Connected);
        assert!(!web.telemetry.contains_key("error"), "{:?}", web.telemetry);
        assert_eq!(web.telemetry["bind"], "127.0.0.1:0");
    }

    /// GET `path` from the server at `addr`, retrying the connect until it
    /// listens. Returns the response body.
    fn http_get(addr: SocketAddr, path: &str) -> String {
        use std::io::{Read, Write};
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut stream = loop {
            match std::net::TcpStream::connect(addr) {
                Ok(s) => break s,
                Err(e) => {
                    assert!(std::time::Instant::now() < deadline, "connect: {e}");
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        };
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
        )
        .expect("request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response");
        let (_, body) = response.split_once("\r\n\r\n").expect("body");
        body.to_string()
    }

    #[test]
    fn a_status_sent_right_after_the_web_server_starts_is_in_api_status() {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-web-start-{}",
            crate::state::config::generate_id()
        )));
        let (root, _writer) = SystemState::new(dir.0.join("config.toml"));
        let root = Arc::new(root);
        let (bus_tx, _) = broadcast::channel(1024);
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port");
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let web_sender = BusSender::new("webserver.0".into(), bus_tx.clone(), shutdown);
        let web = WebActor::new(addr);
        web.start(
            Arc::clone(&root),
            web_sender.clone(),
            web_sender.subscribe(),
        );

        // The next actor's first status, sent as soon as `start` returns.
        let monitor = BusSender::new(
            "mock_monitor.0".into(),
            bus_tx,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        monitor.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
            status: ActorStatus::Starting,
            telemetry: HashMap::from([("bind".into(), "first".into())]),
        }));

        let body = http_get(addr, "/api/status");
        web.stop();
        let status: flighthook::StatusResponse = serde_json::from_str(&body).expect("json");
        let monitor = &status.actors["mock_monitor.0"];
        assert_eq!(monitor.status, ActorStatus::Starting);
        assert_eq!(monitor.telemetry["bind"], "first");
    }

    fn status_event(status: ActorStatus) -> FlighthookEvent {
        FlighthookEvent::ActorStatus {
            status,
            telemetry: HashMap::new(),
        }
    }

    fn outcome(started: &[&str], stopped: &[&str], restarted: &[&str]) -> FlighthookEvent {
        let ids = |ids: &[&str]| ids.iter().map(|s| (*s).to_owned()).collect();
        FlighthookEvent::ConfigOutcome {
            request_id: None,
            restarted: ids(restarted),
            stopped: ids(stopped),
            started: ids(started),
        }
    }

    #[tokio::test]
    async fn a_lost_connection_is_red_reconnecting_for_ten_minutes() {
        let (state, _writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        let id = "openconnect_server.0";
        let shown = |at: u64, actors: &HashMap<String, ActorStatusResponse>| {
            let a = &actor_statuses(actors, &HashMap::new(), at)[id];
            (a.state, a.red_for_ms)
        };

        // Listening, no peer yet: Connecting.
        up.send(id, status_event(ActorStatus::Starting)).await;
        up.send(id, status_event(ActorStatus::Disconnected)).await;
        let now = state.now_ms();
        assert_eq!(
            shown(now, &*state.actors.read().await),
            (flighthook::ConnectionState::Connecting, 0)
        );

        // A peer connected and left.
        up.send(id, status_event(ActorStatus::Connected)).await;
        up.send(id, status_event(ActorStatus::Disconnected)).await;
        let now = state.now_ms();
        let (shown_state, red) = shown(now, &*state.actors.read().await);
        assert_eq!(shown_state, flighthook::ConnectionState::Reconnecting);
        assert!(red > 0 && red <= flighthook::RECONNECT_ALERT_MS, "{red}");
        assert_eq!(
            shown(
                now + flighthook::RECONNECT_ALERT_MS,
                &*state.actors.read().await
            ),
            (flighthook::ConnectionState::Reconnecting, 0)
        );

        // A restart from a config change starts again at Connecting.
        up.send("system", outcome(&[], &[], &[id])).await;
        assert_eq!(
            shown(state.now_ms(), &*state.actors.read().await),
            (flighthook::ConnectionState::Connecting, 0)
        );

        // Removed and added again: a new history.
        up.send(id, status_event(ActorStatus::Connected)).await;
        up.send(id, status_event(ActorStatus::Disconnected)).await;
        up.send("system", outcome(&[], &[id], &[])).await;
        assert!(!state.actors.read().await.contains_key(id));
        up.send(id, status_event(ActorStatus::Disconnected)).await;
        assert_eq!(
            shown(state.now_ms(), &*state.actors.read().await),
            (flighthook::ConnectionState::Connecting, 0)
        );
    }

    #[test]
    fn a_lost_connection_stays_red_in_api_status_across_ui_connections() {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-web-red-{}",
            crate::state::config::generate_id()
        )));
        let (root, _writer) = SystemState::new(dir.0.join("config.toml"));
        let root = Arc::new(root);
        let (bus_tx, _) = broadcast::channel(1024);
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port");
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let web_sender = BusSender::new("webserver.0".into(), bus_tx.clone(), shutdown);
        let web = WebActor::new(addr);
        web.start(
            Arc::clone(&root),
            web_sender.clone(),
            web_sender.subscribe(),
        );
        let server = BusSender::new(
            "openconnect_server.0".into(),
            bus_tx,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        for status in [
            ActorStatus::Starting,
            ActorStatus::Connected,
            ActorStatus::Disconnected,
        ] {
            server.send(FlighthookMessage::new(status_event(status)));
        }

        // Each page load (a UI connecting) reads the same history, the red
        // time counting down between them.
        let red = |body: &str| {
            let status: flighthook::StatusResponse = serde_json::from_str(body).expect("json");
            status
                .actors
                .get("openconnect_server.0")
                .filter(|a| a.state == flighthook::ConnectionState::Reconnecting)
                .map(|a| a.red_for_ms)
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let first = loop {
            if let Some(ms) = red(&http_get(addr, "/api/status")) {
                break ms;
            }
            assert!(std::time::Instant::now() < deadline, "no Reconnecting");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        std::thread::sleep(std::time::Duration::from_millis(30));
        let second = red(&http_get(addr, "/api/status")).expect("still Reconnecting");
        web.stop();
        assert!(
            first > 0 && first <= flighthook::RECONNECT_ALERT_MS,
            "{first}"
        );
        assert!(second > 0 && second < first, "{first} then {second}");
    }

    /// A webserver's state over `root`, on `bus_tx`, as `actor_id`.
    pub(crate) fn web_state(
        root: Arc<SystemState>,
        bus_tx: broadcast::Sender<FlighthookMessage>,
        actor_id: &str,
    ) -> WebState {
        let groups = initial_groups(&root);
        WebState {
            root,
            bus_tx,
            actors: RwLock::new(HashMap::new()),
            epoch: std::time::Instant::now(),
            shots: RwLock::new(VecDeque::new()),
            groups: RwLock::new(groups),
            raw_bytes: AtomicUsize::new(0),
            device_identity: RwLock::new(HashMap::new()),
            cached_actor_status: RwLock::new(HashMap::new()),
            cached_device_telemetry: RwLock::new(HashMap::new()),
            addr: "127.0.0.1:0".parse().expect("addr"),
            actor_id: actor_id.into(),
            ws_count: AtomicU64::new(0),
            request_count: AtomicU64::new(0),
        }
    }

    pub(crate) fn test_state() -> (WebState, GameStateWriter, TempDir) {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-web-test-{}",
            crate::state::config::generate_id()
        )));
        let (root, writer) = SystemState::new(dir.0.join("config.toml"));
        let (bus_tx, _) = broadcast::channel(1024);
        let state = web_state(Arc::new(root), bus_tx, "webserver.0");
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
                    spin_source: flighthook::SpinSource::D4,
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
        let club = |club| {
            FlighthookMessage::new(FlighthookEvent::ClubInfo {
                club_info: ClubInfo { club },
            })
            .actor("openconnect_client.0")
        };
        apply_bus_event(&state, &club(Club::Iron8), &mut acc).await;
        apply_bus_event(
            &state,
            &msg(FlighthookEvent::ShotTrigger { key: key(1) }),
            &mut acc,
        )
        .await;
        // A club change after the trigger does not change the shot's club.
        writer.set_club_info(ClubInfo { club: Club::Driver });
        apply_bus_event(&state, &club(Club::Driver), &mut acc).await;
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
        // `/api/shots` carries it on the shot itself.
        assert_eq!(s.data.handed, Some(Handedness::Right));
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
                ..ShotData::default()
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
        let mut evicted = Vec::new();
        for n in 0..=u32::try_from(MAX_SHOTS).expect("fits") {
            evicted.extend(store_shot(
                &mut shots,
                &bytes,
                stored(n, Some(raw(1, 1))),
                RAW_BUDGET_BYTES,
            ));
        }
        assert_eq!(shots.len(), MAX_SHOTS);
        assert_eq!(shots[0].key.shot_number, 1);
        assert_eq!(evicted.len(), 1, "the shot that fell out is returned");
        assert_eq!(evicted[0].key.shot_number, 0);
        assert_eq!(
            bytes.load(Ordering::Relaxed),
            MAX_SHOTS * (2 + RAW_RECORD_OVERHEAD)
        );
    }

    // -- Shot groups ---------------------------------------------------------

    /// Feeds bus messages through one shared set of pending shots, so a
    /// shot's trigger and finish can be split around other events.
    struct Updater<'a> {
        state: &'a WebState,
        pending: PendingShots,
    }

    impl<'a> Updater<'a> {
        fn new(state: &'a WebState) -> Self {
            Self {
                state,
                pending: PendingShots::new(),
            }
        }

        async fn send(&mut self, actor: &str, event: FlighthookEvent) {
            let msg = FlighthookMessage::new(event).actor(actor);
            apply_bus_event(self.state, &msg, &mut self.pending).await;
        }

        async fn trigger(&mut self, n: u32) {
            self.trigger_as("mevo.0", key(n)).await;
        }

        async fn finish(&mut self, n: u32) {
            self.finish_as("mevo.0", key(n)).await;
        }

        async fn shot(&mut self, n: u32) {
            self.trigger(n).await;
            self.finish(n).await;
        }

        async fn trigger_as(&mut self, actor: &str, key: ShotKey) {
            self.send(actor, FlighthookEvent::ShotTrigger { key }).await;
        }

        /// Ball data, then the finish.
        async fn finish_as(&mut self, actor: &str, key: ShotKey) {
            self.send(
                actor,
                FlighthookEvent::BallFlight {
                    key: key.clone(),
                    ball: Box::new(ball()),
                    derived: None,
                },
            )
            .await;
            self.send(actor, FlighthookEvent::ShotFinished { key, raw: None })
                .await;
        }

        async fn shot_as(&mut self, actor: &str, key: ShotKey) {
            self.trigger_as(actor, key.clone()).await;
            self.finish_as(actor, key).await;
        }

        async fn status(&mut self, actor: &str, status: ActorStatus) {
            self.send(
                actor,
                FlighthookEvent::ActorStatus {
                    status,
                    telemetry: HashMap::new(),
                },
            )
            .await;
        }

        /// The statuses a device actor reports as its connection drops and
        /// comes back.
        async fn reconnect(&mut self, actor: &str) {
            for status in [
                ActorStatus::Disconnected,
                ActorStatus::Reconnecting,
                ActorStatus::Starting,
                ActorStatus::Connected,
            ] {
                self.status(actor, status).await;
            }
        }

        /// A club change as the bus carries it: game state (the system
        /// actor's write) and the `club_info` message.
        async fn club(&mut self, writer: &GameStateWriter, club: Club) {
            writer.set_club_info(ClubInfo { club });
            self.send(
                "web",
                FlighthookEvent::ClubInfo {
                    club_info: ClubInfo { club },
                },
            )
            .await;
        }

        /// A persisted grouping change, as the system actor applies it.
        async fn grouping(&mut self, mode: ShotGrouping) {
            self.state.root.system.update(|c| c.shot_grouping = mode);
            self.send(
                "system",
                FlighthookEvent::ConfigOutcome {
                    request_id: None,
                    restarted: Vec::new(),
                    stopped: Vec::new(),
                    started: Vec::new(),
                },
            )
            .await;
        }
    }

    /// `shot_group` events on the bus so far, as `(actor, group, shot)`.
    fn group_events(
        rx: &mut broadcast::Receiver<FlighthookMessage>,
    ) -> Vec<(String, ShotGroupInfo, Option<GroupedShot>)> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let FlighthookEvent::ShotGroup { group, shot } = msg.event {
                out.push((msg.actor, group, shot));
            }
        }
        out
    }

    async fn group_of(state: &WebState, n: u32) -> Option<String> {
        state
            .shots
            .read()
            .await
            .iter()
            .find(|s| s.key.shot_number == n)
            .expect("stored")
            .data
            .group
            .clone()
    }

    async fn group_info(state: &WebState, id: &str) -> ShotGroupInfo {
        state
            .groups
            .read()
            .await
            .list()
            .into_iter()
            .find(|g| g.id == id)
            .expect("group kept")
    }

    #[tokio::test]
    async fn stored_shot_is_stamped_with_id_club_and_group() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::ByClub).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        // Dataless, raw only: stored for the export, never counted.
        up.trigger(2).await;
        up.send(
            "mevo.0",
            FlighthookEvent::ShotFinished {
                key: key(2),
                raw: Some(Box::new(raw(1, 2))),
            },
        )
        .await;

        let shots = state.shots.read().await.clone();
        let current = state.groups.read().await.current().cloned().expect("open");
        let s1 = &shots[0].data;
        assert_eq!(s1.shot_id.as_deref(), Some("shot-1"));
        assert_eq!(s1.club_selected, Some(Club::Iron7));
        assert_eq!(s1.group.as_deref(), Some(current.id.as_str()));
        let s2 = &shots[1].data;
        assert_eq!(s2.shot_id.as_deref(), Some("shot-2"));
        assert_eq!(s2.club_selected, Some(Club::Iron7));
        assert_eq!(s2.group, None);
        assert_eq!(current.shots, 1, "the dataless shot is not counted");

        let listed = routes::get_shots(
            axum::extract::State(Arc::new(state)),
            axum::extract::Query(routes::ShotsQuery {
                limit: 50,
                units: Some("metric".into()),
            }),
        )
        .await
        .0;
        assert_eq!(listed.len(), 1);
        let v = serde_json::to_value(&listed[0]).expect("ser");
        assert_eq!(v["shot_id"], "shot-1");
        assert_eq!(v["club_selected"], "7I");
        assert_eq!(v["group"], current.id.as_str());
    }

    #[tokio::test]
    async fn mode_none_still_records_sets() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::None).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        up.send("web", FlighthookEvent::NewShotGroup).await;
        up.shot(2).await;
        up.club(&writer, Club::Driver).await;
        up.shot(3).await;
        up.send("web", FlighthookEvent::NewShotSession).await;
        up.shot(4).await;

        let mut ids = Vec::new();
        for n in 1..=4 {
            ids.push(group_of(&state, n).await.expect("grouped"));
        }
        // New set, a club change and New session each start a set.
        for pair in ids.windows(2) {
            assert_ne!(pair[0], pair[1]);
        }
        let list = state.groups.read().await.list();
        assert_eq!(list.len(), 4);
        assert_eq!(
            list.iter().map(|g| g.session).collect::<Vec<_>>(),
            [0, 0, 0, 1]
        );
        assert!(!group_events(&mut rx).is_empty(), "shot_group events flow");

        // `/api/shots` carries the set.
        let listed = routes::get_shots(
            axum::extract::State(Arc::new(state)),
            axum::extract::Query(routes::ShotsQuery {
                limit: 50,
                units: None,
            }),
        )
        .await
        .0;
        assert_eq!(listed.len(), 4);
        for (shot, id) in listed.iter().zip(&ids) {
            assert_eq!(shot.group.as_deref(), Some(id.as_str()));
        }
    }

    #[tokio::test]
    async fn clear_history_empties_the_shots_and_sets_and_starts_a_new_session() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        up.send("web", FlighthookEvent::NewShotGroup).await;
        up.trigger(2).await;
        up.send(
            "mevo.0",
            FlighthookEvent::ShotFinished {
                key: key(2),
                raw: Some(Box::new(raw(3, 64))),
            },
        )
        .await;
        up.club(&writer, Club::Driver).await;
        up.shot(3).await;
        assert_eq!(state.shots.read().await.len(), 3);
        assert!(state.raw_bytes.load(Ordering::Relaxed) > 0);
        assert_eq!(state.groups.read().await.list().len(), 2);
        let _ = group_events(&mut rx);

        up.send("web", FlighthookEvent::ClearShotHistory).await;
        assert!(state.shots.read().await.is_empty());
        assert_eq!(state.raw_bytes.load(Ordering::Relaxed), 0);
        {
            let groups = state.groups.read().await;
            assert!(groups.list().is_empty());
            assert!(groups.current().is_none());
            assert_eq!(groups.session(), 1);
        }
        assert!(group_events(&mut rx).is_empty(), "UIs clear on the command");

        // The next shot starts set 1 in the new session, with the club in
        // play kept.
        up.shot(4).await;
        let shots = state.shots.read().await.clone();
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].data.club_selected, Some(Club::Driver));
        let id = shots[0].data.group.clone().expect("grouped");
        let info = group_info(&state, &id).await;
        assert_eq!((info.number, info.session, info.shots), (1, 1, 1));
        assert_eq!(info.club, Some(Club::Driver));
    }

    #[tokio::test]
    async fn a_shot_in_flight_at_clear_history_stays_cleared() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        up.trigger(2).await;
        up.send(
            "mevo.0",
            FlighthookEvent::BallFlight {
                key: key(2),
                ball: Box::new(ball()),
                derived: None,
            },
        )
        .await;
        let _ = group_events(&mut rx);

        up.send("web", FlighthookEvent::ClearShotHistory).await;
        // The device finishes the shot it triggered before the clear.
        up.finish(2).await;
        assert!(state.shots.read().await.is_empty(), "nothing stored");
        assert!(state.groups.read().await.list().is_empty());
        assert!(group_events(&mut rx).is_empty(), "no set comes back");
        // A dropout afterwards has nothing to end.
        up.reconnect("mevo.0").await;
        assert!(group_events(&mut rx).is_empty());

        // The next trigger is a fresh shot in a fresh set.
        up.shot(3).await;
        let shots = state.shots.read().await.clone();
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].key.shot_number, 3);
        let id = shots[0].data.group.clone().expect("grouped");
        assert_eq!(group_info(&state, &id).await.number, 1);
    }

    #[tokio::test]
    async fn exported_lines_carry_the_set_in_every_view() {
        for view in [
            ShotGrouping::BySet,
            ShotGrouping::ByClub,
            ShotGrouping::None,
        ] {
            let (state, writer, _dir) = test_state();
            let mut up = Updater::new(&state);
            up.grouping(view).await;
            up.club(&writer, Club::Iron7).await;
            up.shot(1).await;
            let stored = state.shots.read().await[0].clone();
            let id = stored.data.group.clone().expect("grouped");
            let line: serde_json::Value =
                serde_json::from_str(&export::jsonl_line(&stored, "t", None, false)).expect("json");
            assert_eq!(line["group"], id.as_str(), "{view:?}");
            assert_eq!(line["data"]["group"], id.as_str(), "{view:?}");
            let row = export::to_csv_row(&stored, flighthook::UnitSystem::Imperial);
            assert!(row.split(',').any(|cell| cell == id), "{view:?}: {row}");
        }
    }

    #[tokio::test]
    async fn new_group_during_a_flight_applies_to_the_next_shot() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.trigger(1).await;
        up.send("web", FlighthookEvent::NewShotGroup).await;
        up.finish(1).await;

        let old = group_of(&state, 1).await.expect("grouped");
        let old_info = group_info(&state, &old).await;
        assert_eq!(old_info.shots, 1);
        assert!(!old_info.open, "New group closed it");
        let new = state.groups.read().await.current().cloned().expect("open");
        assert_ne!(new.id, old);
        assert!(new.manual);
        assert_eq!(new.shots, 0, "the new group is still empty");

        up.shot(2).await;
        assert_eq!(group_of(&state, 2).await, Some(new.id));
    }

    #[tokio::test]
    async fn club_change_during_a_flight_applies_to_the_next_shot() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.trigger(1).await;
        up.club(&writer, Club::Driver).await;
        up.finish(1).await;
        let first = group_of(&state, 1).await.expect("grouped");
        let info = group_info(&state, &first).await;
        assert_eq!(info.club, Some(Club::Iron7));
        assert_eq!(
            state.shots.read().await[0].data.club_selected,
            Some(Club::Iron7)
        );

        up.shot(2).await;
        let second = group_of(&state, 2).await.expect("grouped");
        assert_ne!(second, first);
        assert_eq!(group_info(&state, &second).await.club, Some(Club::Driver));
        assert!(!group_info(&state, &first).await.open);
    }

    /// 7I, 7I, DR, 7I with the default view switched to `views[n]` before
    /// each shot; returns each shot's set.
    async fn sets_for(views: [ShotGrouping; 4]) -> Vec<String> {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        let clubs = [Club::Iron7, Club::Iron7, Club::Driver, Club::Iron7];
        let mut out = Vec::new();
        for (n, (club, view)) in (1..).zip(clubs.into_iter().zip(views)) {
            up.grouping(view).await;
            up.club(&writer, club).await;
            up.shot(n).await;
            out.push(group_of(&state, n).await.expect("grouped"));
        }
        assert_eq!(state.groups.read().await.list().len(), 3);
        out
    }

    #[tokio::test]
    async fn club_change_starts_a_new_set_in_every_view() {
        use ShotGrouping::{ByClub, BySet, None as LastShot};
        for views in [
            [BySet; 4],
            [ByClub; 4],
            [LastShot; 4],
            [BySet, ByClub, LastShot, ByClub],
            [LastShot, BySet, ByClub, BySet],
        ] {
            let sets = sets_for(views).await;
            assert_eq!(sets[0], sets[1], "{views:?}: same club continues");
            assert_ne!(sets[1], sets[2], "{views:?}: DR starts a set");
            assert_ne!(sets[2], sets[3], "{views:?}: back to 7I starts a set");
            assert_ne!(
                sets[0], sets[3],
                "{views:?}: the earlier 7I set stays closed"
            );
        }
    }

    #[tokio::test]
    async fn changing_the_default_view_closes_and_creates_nothing() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        let set = state.groups.read().await.current().cloned().expect("open");
        group_events(&mut rx);
        for view in [
            ShotGrouping::None,
            ShotGrouping::ByClub,
            ShotGrouping::BySet,
        ] {
            up.grouping(view).await;
            assert!(group_events(&mut rx).is_empty(), "{view:?}");
            let current = state.groups.read().await.current().cloned();
            assert_eq!(current.as_ref(), Some(&set), "{view:?}");
        }
        up.shot(2).await;
        assert_eq!(group_of(&state, 2).await, Some(set.id));
    }

    #[tokio::test]
    async fn shot_group_events_on_create_join_and_session() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::ByClub).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;

        let events = group_events(&mut rx);
        assert_eq!(events.len(), 2, "{events:?}");
        let (actor, created, shot) = &events[0];
        assert_eq!(actor, "webserver.0");
        assert_eq!(created.shots, 0);
        assert!(created.open);
        assert_eq!(created.number, 1);
        assert_eq!(*shot, None, "create carries no shot");
        let (actor, joined, shot) = &events[1];
        assert_eq!(actor, "webserver.0");
        assert_eq!(joined.id, created.id);
        assert_eq!(joined.shots, 1);
        assert_eq!(
            *shot,
            Some(GroupedShot {
                actor: "mevo.0".into(),
                key: key(1),
            })
        );

        // DR closes the 7-iron set and starts set 2.
        up.club(&writer, Club::Driver).await;
        up.shot(2).await;
        let events = group_events(&mut rx);
        assert_eq!(events.len(), 3, "{events:?}");
        assert_eq!(events[0].1.number, 1);
        assert!(!events[0].1.open);
        assert_eq!((events[1].1.number, events[1].1.open), (2, true));
        assert_eq!(events[2].1.shots, 1);
        up.send("web", FlighthookEvent::NewShotSession).await;
        let closed = group_events(&mut rx);
        assert_eq!(closed.len(), 1, "the open set");
        let (a, g, s) = &closed[0];
        assert_eq!(a, "webserver.0");
        assert!(!g.open && s.is_none());
        assert_eq!(g.number, 2);
        assert_eq!(g.session, 0, "closed in the session it belongs to");
        // The next shot starts set 3 in session 1.
        up.shot(3).await;
        let created = &group_events(&mut rx)[0].1;
        assert_eq!((created.number, created.session), (3, 1));
    }

    #[tokio::test]
    async fn club_info_selects_the_club_for_new_group() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::ByClub).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        let i7 = group_of(&state, 1).await.expect("grouped");
        // The bus message alone moves the selection; game state is not read.
        up.send(
            "openconnect_client.0",
            FlighthookEvent::ClubInfo {
                club_info: ClubInfo { club: Club::Driver },
            },
        )
        .await;
        group_events(&mut rx);
        up.send("web", FlighthookEvent::NewShotGroup).await;
        let events = group_events(&mut rx);
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].1.id, i7);
        assert!(!events[0].1.open, "the 7-iron set closes");
        assert_eq!(events[1].1.club, Some(Club::Driver));
        assert!(events[1].1.manual && events[1].1.open);
    }

    #[tokio::test]
    async fn every_webserver_groups_independently_under_its_own_actor_id() {
        let (one, writer, _dir) = test_state();
        let two = web_state(Arc::clone(&one.root), one.bus_tx.clone(), "webserver.1");
        let mut rx = one.bus_tx.subscribe();
        writer.set_club_info(ClubInfo { club: Club::Iron7 });
        one.root
            .system
            .update(|c| c.shot_grouping = ShotGrouping::BySet);
        let outcome = FlighthookEvent::ConfigOutcome {
            request_id: None,
            restarted: Vec::new(),
            stopped: Vec::new(),
            started: Vec::new(),
        };
        let mut a = Updater::new(&one);
        let mut b = Updater::new(&two);
        a.send("system", outcome.clone()).await;
        b.send("system", outcome).await;
        a.shot(1).await;
        b.shot(1).await;

        let events = group_events(&mut rx);
        let by = |actor: &str| -> Vec<ShotGroupInfo> {
            events
                .iter()
                .filter(|(a, _, _)| a == actor)
                .map(|(_, g, _)| g.clone())
                .collect()
        };
        let (e1, e2) = (by("webserver.0"), by("webserver.1"));
        assert_eq!(e1.len(), 2);
        assert_eq!(e2.len(), 2);
        assert_eq!(e1.len() + e2.len(), events.len());
        assert_ne!(e1[0].id, e2[0].id, "each server has its own ids");
        assert_eq!(group_of(&one, 1).await.as_deref(), Some(e1[0].id.as_str()));
        assert_eq!(group_of(&two, 1).await.as_deref(), Some(e2[0].id.as_str()));
    }

    #[tokio::test]
    async fn eviction_releases_the_oldest_shots_group() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(0).await;
        up.club(&writer, Club::Driver).await;
        for n in 1..u32::try_from(MAX_SHOTS).expect("fits") {
            up.shot(n).await;
        }
        let first = group_of(&state, 0).await.expect("grouped");
        let second = group_of(&state, 1).await.expect("grouped");
        assert_eq!(state.shots.read().await.len(), MAX_SHOTS);

        let mut rx = state.bus_tx.subscribe();
        up.shot(5000).await;
        let events = group_events(&mut rx);
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].1.id, second);
        assert!(events[0].2.is_some(), "join");
        let (_, released, shot) = &events[1];
        assert_eq!(released.id, first);
        assert_eq!(*shot, None);
        assert!(!released.open);
        assert_eq!(released.shots, 0, "dropped");
        assert!(
            state
                .groups
                .read()
                .await
                .list()
                .iter()
                .all(|g| g.id != first)
        );
    }

    #[tokio::test]
    async fn evicting_a_dataless_shot_releases_nothing() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.trigger(0).await;
        up.send(
            "mevo.0",
            FlighthookEvent::ShotFinished {
                key: key(0),
                raw: Some(Box::new(raw(1, 1))),
            },
        )
        .await;
        for n in 1..u32::try_from(MAX_SHOTS).expect("fits") {
            up.shot(n).await;
        }
        let group = group_of(&state, 1).await.expect("grouped");
        let mut rx = state.bus_tx.subscribe();
        up.shot(5000).await;
        let events = group_events(&mut rx);
        assert_eq!(events.len(), 1, "the join only: {events:?}");
        assert_eq!(events[0].1.id, group);
        assert_eq!(events[0].1.shots, u32::try_from(MAX_SHOTS).expect("fits"));
    }

    #[tokio::test]
    async fn a_new_trigger_ends_the_devices_unfinished_shot() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.send("web", FlighthookEvent::NewShotGroup).await;
        let set = state.groups.read().await.current().cloned().expect("open");
        // Shot 1 never finishes; shot 2 does.
        up.trigger(1).await;
        up.shot(2).await;
        // Another device's shot in flight is not this device's to end.
        up.send("square.0", FlighthookEvent::ShotTrigger { key: key(7) })
            .await;
        up.trigger(3).await;
        up.send(
            "square.0",
            FlighthookEvent::ShotFinished {
                key: key(7),
                raw: Some(Box::new(raw(1, 1))),
            },
        )
        .await;
        up.finish(3).await;
        assert_eq!(group_info(&state, &set.id).await.shots, 2);
        assert_eq!(group_of(&state, 2).await.as_deref(), Some(set.id.as_str()));
        assert!(up.pending.is_empty(), "nothing is left pending");
        assert_eq!(state.shots.read().await.len(), 3);

        // Re-triggering the same shot ends its first assignment too.
        up.send("web", FlighthookEvent::NewShotGroup).await;
        let fresh = state.groups.read().await.current().cloned().expect("open");
        up.trigger(4).await;
        up.trigger(4).await;
        up.finish(4).await;
        assert_eq!(group_info(&state, &fresh.id).await.shots, 1);
        assert!(up.pending.is_empty());
    }

    #[tokio::test]
    async fn a_device_dropping_out_ends_its_unfinished_shots() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        up.send("web", FlighthookEvent::NewShotGroup).await;
        let set = state.groups.read().await.current().cloned().expect("open");
        for status in [
            ActorStatus::Reconnecting,
            ActorStatus::Disconnected,
            ActorStatus::Starting,
        ] {
            up.trigger(2).await;
            up.send(
                "mevo.0",
                FlighthookEvent::ActorStatus {
                    status,
                    telemetry: HashMap::new(),
                },
            )
            .await;
            assert!(up.pending.is_empty(), "{status:?}");
            // The set is empty again: New set is a no-op...
            group_events(&mut rx);
            up.send("web", FlighthookEvent::NewShotGroup).await;
            assert!(group_events(&mut rx).is_empty(), "{status:?}");
            let current = state.groups.read().await.current().cloned();
            assert_eq!(current.map(|g| g.id), Some(set.id.clone()), "{status:?}");
        }
        // ...and it still takes the next shot's club.
        up.club(&writer, Club::Driver).await;
        up.shot(3).await;
        assert_eq!(group_of(&state, 3).await.as_deref(), Some(set.id.as_str()));
        let info = group_info(&state, &set.id).await;
        assert_eq!(info.club, Some(Club::Driver));
        assert_eq!(info.shots, 1);
        // A Connected status (heartbeat or telemetry) leaves a flight alone.
        up.trigger(4).await;
        up.send(
            "mevo.0",
            FlighthookEvent::ActorStatus {
                status: ActorStatus::Connected,
                telemetry: HashMap::new(),
            },
        )
        .await;
        up.finish(4).await;
        assert_eq!(group_info(&state, &set.id).await.shots, 2);
    }

    #[tokio::test]
    async fn trigger_takes_the_club_from_the_bus_not_game_state() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Driver).await;
        up.shot(1).await;
        // 7I as the bus carries it before the system actor writes game state.
        up.send(
            "web",
            FlighthookEvent::ClubInfo {
                club_info: ClubInfo { club: Club::Iron7 },
            },
        )
        .await;
        assert_eq!(
            state.root.game.snapshot().club_info.map(|c| c.club),
            Some(Club::Driver),
            "game state still lags"
        );
        up.send("web", FlighthookEvent::NewShotGroup).await;
        let set = state.groups.read().await.current().cloned().expect("open");
        assert_eq!(set.club, Some(Club::Iron7));
        up.shot(2).await;
        let stored = state.shots.read().await[1].data.clone();
        assert_eq!(stored.club_selected, Some(Club::Iron7));
        assert_eq!(stored.group.as_deref(), Some(set.id.as_str()));
        let info = group_info(&state, &set.id).await;
        assert_eq!(info.club, Some(Club::Iron7), "not re-keyed to DR");
        assert_eq!(info.shots, 1);
    }

    #[tokio::test]
    async fn dropped_trigger_finishes_its_assignment_uncounted() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        // No data and no raw: nothing is stored.
        up.trigger(1).await;
        up.send(
            "mevo.0",
            FlighthookEvent::ShotFinished {
                key: key(1),
                raw: None,
            },
        )
        .await;
        assert!(state.shots.read().await.is_empty());
        let current = state.groups.read().await.current().cloned().expect("open");
        assert_eq!(current.shots, 0);
        // Nothing pending: a different club adopts the empty group.
        up.club(&writer, Club::Driver).await;
        up.shot(2).await;
        assert_eq!(group_of(&state, 2).await, Some(current.id));
    }

    // -- A device reconnecting keeps its set ---------------------------------

    fn key_id(n: u32, id: &str) -> ShotKey {
        ShotKey {
            shot_id: id.into(),
            shot_number: n,
        }
    }

    async fn group_of_id(state: &WebState, shot_id: &str) -> Option<String> {
        state
            .shots
            .read()
            .await
            .iter()
            .find(|s| s.key.shot_id == shot_id)
            .expect("stored")
            .data
            .group
            .clone()
    }

    async fn selected_club(state: &WebState) -> Option<Club> {
        state.groups.read().await.selected_club()
    }

    #[tokio::test]
    async fn reconnect_between_shots_keeps_the_set() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        for n in 1..=3 {
            up.shot(n).await;
        }
        let set = state.groups.read().await.current().cloned().expect("open");
        assert_eq!(set.shots, 3);
        group_events(&mut rx);

        up.reconnect("mevo.0").await;
        assert!(
            group_events(&mut rx).is_empty(),
            "no set is closed, rotated or created"
        );
        assert_eq!(selected_club(&state).await, Some(Club::Iron7));
        let current = state.groups.read().await.current().cloned();
        assert_eq!(current.map(|g| g.id), Some(set.id.clone()));

        up.shot(4).await;
        assert_eq!(group_of(&state, 4).await.as_deref(), Some(set.id.as_str()));
        let info = group_info(&state, &set.id).await;
        assert_eq!(info.shots, 4);
        assert!(info.open);
        assert_eq!(state.groups.read().await.list().len(), 1);
    }

    #[tokio::test]
    async fn reconnect_mid_shot_drops_only_the_unfinished_shot() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        up.shot(2).await;
        let set = state.groups.read().await.current().cloned().expect("open");
        // Shot 3 is in flight when the connection drops.
        up.trigger(3).await;
        group_events(&mut rx);
        up.status("mevo.0", ActorStatus::Disconnected).await;
        assert!(up.pending.is_empty(), "the unfinished shot is dropped");
        assert!(group_events(&mut rx).is_empty(), "the set is untouched");
        up.reconnect("mevo.0").await;
        // Data for the dropped shot after the reconnect is not its finish.
        up.finish(3).await;
        up.shot(4).await;

        assert_eq!(group_of(&state, 4).await.as_deref(), Some(set.id.as_str()));
        let info = group_info(&state, &set.id).await;
        assert_eq!(info.shots, 3, "shots 1, 2 and 4");
        assert!(info.open);
        let numbers: Vec<u32> = state
            .shots
            .read()
            .await
            .iter()
            .map(|s| s.key.shot_number)
            .collect();
        assert_eq!(numbers, [1, 2, 4]);
        assert_eq!(state.groups.read().await.list().len(), 1);
        assert_eq!(selected_club(&state).await, Some(Club::Iron7));

        // A set opened by the dropped shot's own trigger stays open, and the
        // next shot joins it.
        up.send("web", FlighthookEvent::NewShotSession).await;
        up.trigger(5).await;
        let opened = state.groups.read().await.current().cloned().expect("open");
        assert_ne!(opened.id, set.id);
        up.reconnect("mevo.0").await;
        let current = state.groups.read().await.current().cloned();
        assert_eq!(current.map(|g| g.id), Some(opened.id.clone()));
        up.shot(6).await;
        assert_eq!(group_of(&state, 6).await, Some(opened.id.clone()));
        let info = group_info(&state, &opened.id).await;
        assert_eq!(info.shots, 1);
        assert!(info.open);
        assert!(up.pending.is_empty());
    }

    #[tokio::test]
    async fn reconnect_keeps_the_set_in_every_view() {
        for view in [
            ShotGrouping::BySet,
            ShotGrouping::ByClub,
            ShotGrouping::None,
        ] {
            let (state, writer, _dir) = test_state();
            let mut rx = state.bus_tx.subscribe();
            let mut up = Updater::new(&state);
            up.grouping(view).await;
            up.club(&writer, Club::Iron7).await;
            up.shot_as("mevo.0", key_id(1, "m1")).await;
            up.club(&writer, Club::Driver).await;
            up.shot_as("square.0", key_id(1, "s1")).await;
            let iron = group_of_id(&state, "m1").await.expect("grouped");
            let driver = group_of_id(&state, "s1").await.expect("grouped");
            assert_ne!(iron, driver, "{view:?}");
            group_events(&mut rx);

            up.reconnect("square.0").await;
            up.reconnect("mevo.0").await;
            assert!(group_events(&mut rx).is_empty(), "{view:?}");
            assert_eq!(selected_club(&state).await, Some(Club::Driver));

            up.shot_as("square.0", key_id(1, "s2")).await;
            up.shot_as("mevo.0", key_id(1, "m2")).await;
            assert_eq!(group_of_id(&state, "s2").await, Some(driver.clone()));
            assert_eq!(group_of_id(&state, "m2").await, Some(driver.clone()));
            let info = group_info(&state, &driver).await;
            assert_eq!((info.shots, info.open), (3, true), "{view:?}");
            assert_eq!(state.groups.read().await.list().len(), 2);
        }
    }

    #[tokio::test]
    async fn shot_numbers_restarting_after_a_reconnect_stay_distinct() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.shot_as("mevo.0", key_id(1, "a1")).await;
        up.shot_as("mevo.0", key_id(2, "a2")).await;
        // In flight when the connection drops: dropped, not finished by the
        // next connection's shot of the same number.
        up.trigger_as("mevo.0", key_id(3, "a3")).await;
        let first = group_of_id(&state, "a1").await.expect("grouped");
        up.reconnect("mevo.0").await;
        // The new connection counts from 1 again.
        up.shot_as("mevo.0", key_id(1, "b1")).await;
        up.club(&writer, Club::Driver).await;
        group_events(&mut rx);
        up.shot_as("mevo.0", key_id(2, "b2")).await;
        up.shot_as("mevo.0", key_id(3, "b3")).await;

        assert_eq!(group_of_id(&state, "b1").await, Some(first.clone()));
        assert_eq!(group_of_id(&state, "a1").await, Some(first.clone()));
        let second = group_of_id(&state, "b2").await.expect("grouped");
        assert_ne!(second, first, "DR starts a new set");
        assert_eq!(group_of_id(&state, "b3").await, Some(second.clone()));
        assert_eq!(group_of_id(&state, "a2").await, Some(first.clone()));

        let shots = state.shots.read().await;
        let ids: Vec<(u32, &str)> = shots
            .iter()
            .map(|s| (s.key.shot_number, s.key.shot_id.as_str()))
            .collect();
        assert_eq!(ids, [(1, "a1"), (2, "a2"), (1, "b1"), (2, "b2"), (3, "b3")]);
        for s in shots.iter() {
            assert_eq!(s.data.shot_id.as_deref(), Some(s.key.shot_id.as_str()));
        }
        drop(shots);
        let first = group_info(&state, &first).await;
        assert_eq!((first.shots, first.open), (3, false));
        let second = group_info(&state, &second).await;
        assert_eq!((second.shots, second.open), (2, true));
        // Each join names the shot by its own id.
        let joined: Vec<String> = group_events(&mut rx)
            .into_iter()
            .filter_map(|(_, _, shot)| shot.map(|s| s.key.shot_id))
            .collect();
        assert_eq!(joined, ["b2", "b3"]);
        assert!(up.pending.is_empty());
    }

    #[tokio::test]
    async fn one_device_dropping_mid_shot_leaves_anothers_shot_alone() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.shot_as("mevo.0", key_id(1, "a1")).await;
        let set = state.groups.read().await.current().cloned().expect("open");
        // Both in flight; A drops before its finish, B finishes.
        up.trigger_as("mevo.0", key_id(2, "a2")).await;
        up.trigger_as("square.0", key_id(1, "b1")).await;
        up.status("mevo.0", ActorStatus::Disconnected).await;
        let left: Vec<&str> = up.pending.keys().map(|(a, _)| a.as_str()).collect();
        assert_eq!(left, ["square.0"], "only A's shot is dropped");
        up.finish_as("square.0", key_id(1, "b1")).await;
        // B's next trigger lands while A is still away.
        up.status("mevo.0", ActorStatus::Reconnecting).await;
        up.shot_as("square.0", key_id(2, "b2")).await;
        up.status("mevo.0", ActorStatus::Connected).await;
        up.shot_as("mevo.0", key_id(1, "a3")).await;

        for id in ["a1", "b1", "b2", "a3"] {
            assert_eq!(
                group_of_id(&state, id).await.as_deref(),
                Some(set.id.as_str()),
                "{id}"
            );
        }
        let info = group_info(&state, &set.id).await;
        assert_eq!(info.shots, 4);
        assert!(info.open);
        assert_eq!(
            state.shots.read().await.len(),
            4,
            "A's dropped shot is not stored"
        );
        assert!(up.pending.is_empty());
    }
}
