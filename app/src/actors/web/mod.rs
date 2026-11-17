//! Axum web server — REST endpoints + WebSocket event streaming.

pub mod blocks;
pub mod export;
pub mod routes;
pub(crate) mod stored;
pub mod types;
pub mod ui_socket;
pub mod ws;

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};

use axum::Router;
use axum::routing::{get, post};
use tokio::sync::{RwLock, broadcast};
use tower_http::cors::CorsLayer;

use crate::actors::{Actor, ReconfigureOutcome, actor_names};
use crate::bus::{BusMsg, BusReceiver, BusSender, LocalEvent};
use crate::state::SystemState;
use blocks::BlockStore;
use flighthook::{
    ActorStatus, ActorStatusResponse, ExportDevice, FlightPath, FlighthookConfig, Mode,
    SHOT_TIMEOUT, ShotData, ShotKey, ShotRaw, ShotVideo, SimulationSection,
};
use flightrelay::{CompletedShot, FrpBody, ShotAggregator, ShotEvent};

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

/// Everything the web layer keeps per shot. `/api/shots` serializes `data`
/// only; the export serializes all of it.
#[derive(Debug, Clone)]
pub struct StoredShot {
    /// When `shot_trigger` reached the web layer.
    pub received_at: DateTime<Utc>,
    pub key: ShotKey,
    /// What was true when the trigger arrived.
    pub context: ExportContext,
    pub data: ShotData,
    pub raw: Option<ShotRaw>,
    /// The shot as composed from its FRP messages, addenda included; `data`
    /// is built from it.
    pub frp: Option<CompletedShot>,
    /// The flight model's path.
    pub path: Option<FlightPath>,
}

impl StoredShot {
    fn raw_len(&self) -> usize {
        self.raw.as_ref().map_or(0, raw_held_bytes)
    }
}

/// Settings and identity in effect when a shot was triggered, for the
/// export.
#[derive(Debug, Clone, Default)]
pub struct ExportContext {
    /// Kind, maker, model and firmware only.
    pub device: ExportDevice,
    pub detection_mode: Option<Mode>,
    /// The actor's config section with `name`, `address` and `bind` removed.
    pub device_section: serde_json::Value,
    pub simulation: SimulationSection,
    /// Identity strings scrubbed from text payloads and device labels on
    /// export: device IDs, configured and peer addresses.
    pub scrub: Vec<String>,
    /// The card key of each local actor at the trigger, by actor id: the
    /// export names local actors by card key.
    pub card_keys: HashMap<String, String>,
}

/// What the web layer keeps for a shot between `shot.trigger` and
/// `shot.finished`, next to its messages in [`PendingShots`].
pub(crate) struct PendingShot {
    received_at: DateTime<Utc>,
    /// The launch monitor's card key (its actor id for a remote one).
    actor: String,
    context: ExportContext,
    /// The session's turn at the trigger, for a shot the aggregator stamped
    /// no turn on.
    turn: Option<flightrelay::GolferTurn>,
    /// The launch monitor arms only for putts (its driver's fixed mode at
    /// the trigger): its shot is a putter shot whatever club is in play.
    putting: bool,
    /// Camera clips that finished before the shot did.
    video: Vec<ShotVideo>,
    /// The game's stroke, when it came before the shot finished.
    stroke: Option<flightrelay::GameStroke>,
    /// The device frames, from the launch monitor card's capture.
    raw: Option<ShotRaw>,
    /// The flight model's path, when it came before the shot finished.
    path: Option<FlightPath>,
    /// When the trigger arrived.
    started: Instant,
}

/// The shots between `shot.trigger` and `shot.finished`: their messages,
/// composed by the SDK's [`ShotAggregator`] (flighthook's
/// [`SHOT_TIMEOUT`], fed only flighthook's own turn), and the web layer's
/// own per-shot state, by shot id.
pub(crate) struct PendingShots {
    data: ShotAggregator,
    /// Each shot's launch monitor: only it sends the shot's parts.
    owners: crate::actors::simout::ShotOwners,
    /// The actors whose last `bus.actor` holds the `game` role: only a
    /// game's `game.stroke` joins a shot.
    games: std::collections::HashSet<String>,
    shots: HashMap<String, PendingShot>,
}

impl Default for PendingShots {
    fn default() -> Self {
        Self {
            data: ShotAggregator::new().with_timeout(SHOT_TIMEOUT),
            owners: crate::actors::simout::ShotOwners::default(),
            games: std::collections::HashSet::new(),
            shots: HashMap::new(),
        }
    }
}

impl PendingShots {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Every pending shot's state, with its shot id.
    fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut PendingShot)> {
        self.shots.iter_mut()
    }

    /// End `actor`'s pending shots (a card key), except the shot `keep` (a
    /// shot id). The aggregator discards each of them as on a timeout, so
    /// their later messages and finish are ignored.
    fn discard_actor(&mut self, actor: &str, keep: Option<&str>) {
        let ended: Vec<String> = self
            .shots
            .iter()
            .filter(|(id, p)| p.actor == actor && Some(id.as_str()) != keep)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ended {
            self.data.discard(&id);
            self.shots.remove(&id);
        }
    }

    /// Discard every open shot of the gone actor `actor_id` (card key
    /// `card`), so nothing it sent before it went is stored.
    fn forget_actor(&mut self, card: &str, actor_id: &str) {
        self.discard_actor(card, None);
        self.data.forget_actor(actor_id);
        self.owners.forget_actor(actor_id);
        self.games.remove(actor_id);
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.shots.is_empty() && self.data.pending_count() == 0
    }

    /// The shot id of every pending shot.
    #[cfg(test)]
    fn keys(&self) -> impl Iterator<Item = &String> {
        self.shots.keys()
    }

    /// End the pending shots that have not finished within the shot timeout.
    fn discard_expired(&mut self, now: Instant) {
        self.data.discard_expired();
        let expired: Vec<String> = self
            .shots
            .iter()
            .filter(|(_, p)| now.saturating_duration_since(p.started) > SHOT_TIMEOUT)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.data.discard(&id);
            self.shots.remove(&id);
        }
    }
}

fn new_actor(name: String) -> ActorStatusResponse {
    ActorStatusResponse::new(name, ActorStatus::Disconnected)
}

/// Bring `actors` in line with the sections `disabled` (from
/// `FlighthookConfig::disabled_actors`) lists as off: each such actor shows
/// as Disabled, with nothing held, and one that is on again but has not
/// started a new instance yet shows as a new actor. `names` are the
/// configured ones ([`actor_names`]). Returns the actors made Disabled.
fn follow_disabled(
    actors: &mut HashMap<String, ActorStatusResponse>,
    names: &HashMap<String, String>,
    disabled: &[String],
) -> Vec<String> {
    let mut turned_off = Vec::new();
    for (id, name) in names {
        let off = disabled.contains(id);
        match actors.get(id) {
            Some(a) if a.is_disabled() == off => {}
            None if !off => {}
            _ if off => {
                actors.insert(id.clone(), ActorStatusResponse::disabled(name.clone()));
                turned_off.push(id.clone());
            }
            _ => {
                actors.insert(id.clone(), new_actor(name.clone()));
            }
        }
    }
    turned_off
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
    pub bus_tx: broadcast::Sender<BusMsg>,
    /// Per-actor state, with each actor's connection history since it last
    /// started (kept across UI connections).
    pub actors: RwLock<HashMap<String, ActorStatusResponse>>,
    /// Origin of the connection-history clock ([`WebState::now_ms`]).
    pub epoch: std::time::Instant,
    pub shots: RwLock<VecDeque<StoredShot>>,
    /// The practice blocks seen this run, with their shots in `shots`.
    /// Lock order where both are held: `blocks`, then `shots`.
    pub blocks: RwLock<BlockStore>,
    /// Raw frame memory held in `shots`, as counted by [`raw_held_bytes`].
    pub raw_bytes: AtomicUsize,
    /// Every FRP state on the bus, local and remote actors.
    pub bus_state: Mutex<flightrelay::BusState>,
    /// What `/api/ws` sends, in bus order ([`ui_socket`]).
    pub ui_out: broadcast::Sender<Arc<flighthook::UiMessage>>,
    /// Counts up with every applied configuration change.
    pub config_generation: AtomicU64,
    pub addr: SocketAddr,
    pub actor_id: String,
    pub ws_count: AtomicU64,
    pub request_count: AtomicU64,
    /// Auto-chip shots sent so far: the last Auto-chip shot number.
    pub auto_chip_shots: AtomicU32,
    /// Actors with a reconnect in flight (`POST /api/actors/{id}/reconnect`).
    pub reconnecting: std::sync::Mutex<std::collections::HashSet<String>>,
    /// The inbound `/frp` links that sent this process's own messages back
    /// (the own-id guard): the Web Server card shows an error while any is open.
    pub looped_links: std::sync::Mutex<std::collections::HashSet<crate::bus::PeerId>>,
}

impl WebState {
    /// The own-id guard dropped a message of the link `peer`. Returns true
    /// the first time for that link.
    pub(crate) fn own_back(&self, peer: crate::bus::PeerId) -> bool {
        self.looped_links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(peer)
    }

    /// The hub link `peer` closed.
    pub(crate) fn link_closed(&self, peer: crate::bus::PeerId) {
        self.looped_links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&peer);
    }

    fn looped(&self) -> bool {
        !self
            .looped_links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }

    /// Milliseconds since [`epoch`](Self::epoch): the clock of the actors'
    /// connection histories.
    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// Emit current telemetry as an ActorStatus event on the bus.
fn emit_status(status: ActorStatus, state: &WebState, bus_tx: &broadcast::Sender<BusMsg>) {
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
    } else if state.looped() {
        ws::OWN_MESSAGES_BACK
    } else {
        ""
    };
    telemetry.insert("error".into(), error.into());
    let _ = bus_tx.send(BusMsg::local(
        state.actor_id.clone(),
        LocalEvent::CardStatus { status, telemetry },
    ));
}

/// Send `practice.new_block` to the practice actor as the flighthook actor.
pub(crate) fn send_new_block(state: &WebState, new_session: bool) {
    let command = flightrelay::PracticeNewBlock {
        r#ref: format!("web-{}", crate::state::config::generate_id()),
        golfer: None,
        new_session,
    };
    let _ = state.bus_tx.send(BusMsg::frp(flightrelay::FrpMessage::new(
        state.root.flighthook_id(),
        command,
    )));
}

/// Emit Connected telemetry (convenience for periodic emitter + ws handlers).
pub(super) fn emit_telemetry(state: &WebState, bus_tx: &broadcast::Sender<BusMsg>) {
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
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        _receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let addr = self.addr;
        let actor_id = sender.actor_id().to_string();
        let bus_tx = sender.raw_sender().clone();
        // Subscribed here, before `start` returns, so no status an actor
        // started after this one emits is missed.
        let bus_rx = bus_tx.subscribe();
        // Every actor republishes its state for this server's store and
        // greetings: what was published before it subscribed (the
        // session's first turn and block at startup, everything after a
        // restart for a new address). The web server is no FRP actor, so
        // the sync goes out as the flighthook actor.
        let _ = bus_tx.send(BusMsg::frp(flightrelay::FrpMessage::new(
            state.flighthook_id(),
            flightrelay::BusSync::new(),
        )));
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
            .expect("failed to spawn webserver thread")
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

    /// The save that changes the bind address is answered by this server,
    /// which does not end until it has answered: nothing waits for it.
    fn stop_wait(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    /// The new server retries its bind until the old one has let go of the
    /// port.
    fn replaceable_while_stopping(&self) -> bool {
        true
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
    bus_tx: broadcast::Sender<BusMsg>,
    bus_rx: broadcast::Receiver<BusMsg>,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    // Pre-populate per-actor state from config: a card that is off shows
    // as Disabled.
    let snap = root.system.snapshot();
    let names = actor_names(&snap);
    let mut actors = HashMap::new();
    for (id, name) in &names {
        actors.insert(id.clone(), new_actor(name.clone()));
    }
    follow_disabled(&mut actors, &names, &snap.disabled_actors());

    let state = Arc::new(WebState {
        root,
        bus_tx: bus_tx.clone(),
        actors: RwLock::new(actors),
        epoch: std::time::Instant::now(),
        shots: RwLock::new(VecDeque::with_capacity(MAX_SHOTS)),
        blocks: RwLock::new(BlockStore::new()),
        raw_bytes: AtomicUsize::new(0),
        bus_state: Mutex::default(),
        ui_out: broadcast::channel(1024).0,
        config_generation: AtomicU64::new(0),
        addr,
        actor_id,
        ws_count: AtomicU64::new(0),
        request_count: AtomicU64::new(0),
        auto_chip_shots: AtomicU32::new(0),
        reconnecting: std::sync::Mutex::default(),
        looped_links: std::sync::Mutex::default(),
    });

    // Background task: every local actor's `bus.actor` again each heartbeat
    let beat_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(flightrelay::DEFAULT_HEARTBEAT);
        // After a stall, one heartbeat, not one per missed tick.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let root = &beat_state.root;
            root.beats
                .beat(&beat_state.bus_tx, |from| root.is_live_local(from));
        }
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
        .route("/favicon.ico", get(routes::get_favicon_ico))
        .route("/favicon-32.png", get(routes::get_favicon_png))
        .route("/apple-touch-icon.png", get(routes::get_apple_touch_icon))
        .route("/api/status", get(routes::get_status))
        .route("/api/shots", get(routes::get_shots))
        .route("/api/shots/export", get(routes::get_shots_export))
        .route("/api/shots/convert", post(routes::post_convert_shot))
        .route("/api/shots/{shot_id}/video", get(routes::get_shot_video))
        .route(
            "/api/shots/{shot_id}/video/{file}",
            get(routes::get_shot_video_file),
        )
        .route(
            "/api/shots/{shot_id}/video/{camera}/analyze",
            post(routes::post_analyze_clip),
        )
        .route(
            "/api/shots/{shot_id}/grades/{camera}",
            get(routes::get_shot_grades),
        )
        .route(
            "/api/body_tracking/models",
            get(routes::get_body_tracking_models),
        )
        .route(
            "/api/body_tracking/models/download",
            post(routes::post_body_tracking_models_download)
                .delete(routes::delete_body_tracking_models_download),
        )
        .route("/api/cameras", get(routes::get_cameras))
        .route("/api/cameras/{actor}/still", get(routes::get_camera_still))
        .route("/api/cameras/{actor}/live", get(routes::get_camera_live))
        .route(
            "/api/blocks",
            get(routes::get_blocks).post(routes::post_blocks),
        )
        .route("/api/shots/clear", post(routes::post_clear_shot_history))
        .route("/api/shots/grouping", post(routes::post_shot_grouping))
        .route("/api/select", post(routes::post_select))
        .route("/api/auto-chip", post(routes::post_auto_chip))
        .route("/api/find-mat", post(routes::post_find_mat))
        .route("/api/actors/{id}/reconnect", post(routes::post_reconnect))
        .route(
            "/api/actors/{id}/forget-pairing",
            post(routes::post_forget_pairing),
        )
        .route(
            "/api/settings",
            get(routes::get_settings).post(routes::post_settings),
        )
        .route(flighthook::FRP_PATH, get(ws::ws_upgrade))
        .route(flighthook::UI_WS_PATH, get(ui_socket::upgrade))
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
async fn state_updater(state: Arc<WebState>, mut bus_rx: broadcast::Receiver<BusMsg>) {
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

/// A remote actor is gone ([`LocalEvent::ActorGone`]): forget its state
/// (the UI is told) and its open shots.
fn actor_gone(state: &WebState, actor: &str, accumulators: &mut PendingShots) {
    ui_socket::gone(state, actor);
    accumulators.forget_actor(&stored::card_of(&state.root, actor), actor);
}

/// Whether the launch monitor `from` (actor id) arms only for putts: its
/// driver's fixed mode while its card runs. A remote actor's `bus.actor`
/// carries no fixed mode, so a remote launch monitor never is one.
fn putting_device(state: &WebState, from: &str) -> bool {
    state.root.fixed_mode(from) == Some(Mode::Putting)
}

/// Stable device kind for an actor type prefix.
pub(crate) fn device_kind(prefix: &str) -> &str {
    match prefix {
        "mevo" => "flightscope",
        "square" => "square_golf",
        "exputt" => "exputt",
        "trackball" => "trackball",
        "r10" => "garmin_r10",
        "openconnect_server" => "openconnect",
        "mock_monitor" => "mock",
        "pureplay_accessory" => "pureplay",
        other => other,
    }
}

/// The actor's config section with `name`, `address`, `bind`, a trackball's
/// `device` (a device name, which can carry a serial number), the card's
/// FRP `actor_id` (no shot condition) and unset fields removed, plus the
/// removed address values (to scrub on export).
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
        "exputt" => value(config.exputt.get(index)),
        "trackball" => value(config.trackball.get(index)),
        "r10" => value(config.r10.get(index)),
        "openconnect_server" => value(config.openconnect_server.get(index)),
        "mock_monitor" => value(config.mock_monitor.get(index)),
        "pureplay_accessory" => value(config.pureplay_accessory.get(index)),
        _ => value::<()>(None),
    };
    let mut removed = Vec::new();
    if let Some(map) = section.as_object_mut() {
        map.retain(|_, v| !v.is_null());
        map.remove("name");
        map.remove("actor_id");
        // Only the trackball's `device` names a unit; no other section's is
        // removed.
        let device = (prefix == "trackball").then_some("device");
        for key in ["address", "bind"].into_iter().chain(device) {
            if let Some(serde_json::Value::String(v)) = map.remove(key) {
                removed.push(v);
            }
        }
    }
    (section, removed)
}

/// Add the host of each address in `scrub` (an address may also appear
/// without its port), then sort and dedup.
fn normalize_scrub(scrub: &mut Vec<String>) {
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
}

/// Snapshot the settings and identity in effect for a shot from the card
/// `actor` (actor id `from`; `remote` when it came in on a hub link, whose
/// device kind is `frp`). The device identity comes from the actor's
/// `bus.actor`.
async fn shot_context(
    state: &WebState,
    actor: &str,
    from: &str,
    putting: bool,
    remote: bool,
) -> ExportContext {
    let turn = state.root.session.turn();
    let config = state.root.system.snapshot();
    let (prefix, index) = actor.split_once('.').unwrap_or((actor, ""));

    let identity = state.bus_state.lock().ok().and_then(|bus| {
        bus.actors()
            .find(|(id, _)| *id == from)
            .and_then(|(_, a)| a.device.clone())
    });
    // A remote actor's shot (`remote`): its id says nothing about the device.
    let device = ExportDevice {
        kind: if remote { "frp" } else { device_kind(prefix) }.to_string(),
        manufacturer: identity.as_ref().and_then(|d| d.manufacturer.clone()),
        model: identity.as_ref().and_then(|d| d.model.clone()),
        firmware: identity.and_then(|d| d.firmware),
    };

    let (device_section, mut scrub) = device_section(&config, prefix, index);
    // The device id identifies the unit (network name, BLE name or
    // address). Open Connect's is the monitor's product label, and is kept.
    if prefix != "openconnect_server"
        && let Some(id) = state
            .actors
            .read()
            .await
            .get(actor)
            .and_then(|a| a.telemetry.get("device_info"))
    {
        scrub.push(id.clone());
    }
    normalize_scrub(&mut scrub);

    ExportContext {
        device,
        // An Auto-chip is simulated as a chip whatever the mode, and a
        // putting device's shot as a putt.
        detection_mode: if actor == flighthook::AUTO_CHIP_ACTOR {
            Some(Mode::Chipping)
        } else if putting {
            Some(Mode::Putting)
        } else {
            turn.mode
        },
        device_section,
        simulation: config.simulation,
        scrub,
        card_keys: state.root.card_keys(),
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

/// End `actor`'s pending shots uncounted.
fn drop_pending(accumulators: &mut PendingShots, actor: &str, keep: Option<&str>) {
    accumulators.discard_actor(actor, keep);
}

/// Put a camera's clip on shot `shot_id`, replacing that camera's earlier
/// one, or with `None` drop every clip of the shot. The shot is found by its
/// id whatever device took it, pending or stored; a shot the history no
/// longer holds is left alone.
fn stamp_video(
    shots: &mut VecDeque<StoredShot>,
    pending: &mut PendingShots,
    shot_id: &str,
    video: Option<&ShotVideo>,
) {
    let apply = |list: &mut Vec<ShotVideo>| match video {
        Some(v) => {
            list.retain(|old| old.camera != v.camera);
            list.push(v.clone());
            list.sort_by(|a, b| a.camera.cmp(&b.camera));
        }
        None => list.clear(),
    };
    for (id, p) in pending.iter_mut() {
        if id == shot_id {
            apply(&mut p.video);
        }
    }
    if let Some(stored) = shots.iter_mut().rev().find(|s| s.key.shot_id == shot_id) {
        apply(&mut stored.data.video);
    }
}

/// Mark camera `camera`'s clip of shot `shot_id` with `set` (body tracked,
/// swing analyzed), pending or stored, whatever device took the shot.
/// Forget what the stored shots `keys` say about camera `camera`'s body
/// tracking and swing analysis: its clip is being tracked again.
pub(crate) async fn forget_analysis(state: &WebState, keys: &[ShotKey], camera: &str) {
    let mut shots = state.shots.write().await;
    for stored in shots
        .iter_mut()
        .filter(|s| keys.iter().any(|k| k.shot_id == s.key.shot_id))
    {
        for video in stored.data.video.iter_mut().filter(|v| v.camera == camera) {
            video.pose = false;
            video.swing = false;
            video.rotation = 0;
            video.analyzed_with = None;
        }
    }
}

fn mark_video(
    shots: &mut VecDeque<StoredShot>,
    pending: &mut PendingShots,
    shot_id: &str,
    camera: &str,
    set: impl Fn(&mut ShotVideo),
) {
    let apply = |list: &mut Vec<ShotVideo>| {
        for video in list.iter_mut().filter(|v| v.camera == camera) {
            set(video);
        }
    };
    for (id, p) in pending.iter_mut() {
        if id == shot_id {
            apply(&mut p.video);
        }
    }
    if let Some(stored) = shots.iter_mut().rev().find(|s| s.key.shot_id == shot_id) {
        apply(&mut stored.data.video);
    }
}

async fn apply_bus_event(state: &WebState, msg: &BusMsg, accumulators: &mut PendingShots) {
    // A `shot.finished` reaches the UI after the store's shot, so a row
    // with data never reads as no read for a moment.
    if matches!(
        msg.as_frp().map(|m| &m.body),
        Some(FrpBody::ShotFinished(_))
    ) {
        store_bus_event(state, msg, accumulators).await;
        ui_socket::follow(state, msg);
    } else {
        ui_socket::follow(state, msg);
        store_bus_event(state, msg, accumulators).await;
    }
}

/// The store's part of [`apply_bus_event`].
async fn store_bus_event(state: &WebState, msg: &BusMsg, accumulators: &mut PendingShots) {
    let now = Instant::now();
    accumulators.discard_expired(now);
    // Who holds `game` is what the actor last said.
    if let Some(m) = msg.as_frp()
        && let FrpBody::BusActor(actor) = &m.body
    {
        if actor.has_role(flightrelay::types::roles::GAME) {
            accumulators.games.insert(m.from.clone());
        } else {
            accumulators.games.remove(&m.from);
        }
    }
    match msg {
        BusMsg::Local { card, event } => match event {
            LocalEvent::ActorGone { actor } => actor_gone(state, actor, accumulators),
            LocalEvent::ShotCapture { key, raw, scrub } => {
                if let Some(p) = accumulators.shots.get_mut(&key.shot_id) {
                    p.raw = Some((**raw).clone());
                    p.context.scrub.extend(scrub.iter().cloned());
                    normalize_scrub(&mut p.context.scrub);
                }
            }
            LocalEvent::ShotPath { key, path } => {
                if let Some(p) = accumulators.shots.get_mut(&key.shot_id) {
                    p.path = Some(path.clone());
                } else {
                    attach(state, &key.shot_id, |stored| {
                        stored.path = Some(path.clone());
                    })
                    .await;
                }
            }
            _ => apply_local(state, card, event, accumulators).await,
        },
        BusMsg::Frp { msg: frp, .. } if !accumulators.owners.admits(frp, now) => {}
        BusMsg::Frp { msg: frp, peer, .. } => match &frp.body {
            // A hosted card's `bus.actor` carries its device.
            FrpBody::BusActor(actor)
                if actor.has_role(flightrelay::types::roles::LAUNCH_MONITOR)
                    || actor.device.is_some() =>
            {
                accumulators.data.feed(frp);
                apply_device(state, &frp.from, actor).await;
            }
            FrpBody::ShotTrigger(trigger) => {
                let id = trigger.key.shot_id.clone();
                if stored(state, &id).await || accumulators.shots.contains_key(&id) {
                    return;
                }
                let card = stored::card_of(&state.root, &frp.from);
                // A device runs one shot at a time: a new trigger ends its
                // earlier unfinished shots uncounted.
                drop_pending(accumulators, &card, Some(&id));
                // The aggregator stamps the turn and the golfer's open block
                // now, so commands and club changes during the flight apply
                // to the next shot.
                accumulators.data.feed(frp);
                let putting = putting_device(state, &frp.from);
                let context = shot_context(state, &card, &frp.from, putting, peer.is_some()).await;
                accumulators.shots.insert(
                    id,
                    PendingShot {
                        received_at: Utc::now(),
                        actor: card,
                        context,
                        turn: Some(state.root.session.turn()),
                        putting,
                        video: Vec::new(),
                        stroke: None,
                        raw: None,
                        path: None,
                        started: now,
                    },
                );
            }
            FrpBody::ShotLaunch(_)
            | FrpBody::ShotClub(_)
            | FrpBody::ShotImpact(_)
            | FrpBody::ShotFlight(_) => {
                let id = frp
                    .body
                    .shot_key()
                    .map(|k| k.shot_id.clone())
                    .unwrap_or_default();
                // One attach path: a message for a stored shot attaches
                // there, whenever it arrives.
                if stored(state, &id).await {
                    attach(state, &id, |s| {
                        if let Some(shot) = &mut s.frp
                            && (frp.from == shot.from || matches!(frp.body, FrpBody::ShotFlight(_)))
                        {
                            if let Some(warning) = late_warning(shot, &frp.from, &frp.body) {
                                tracing::warn!("{warning}");
                            }
                            attach_message(shot, &frp.from, &frp.body);
                        }
                    })
                    .await;
                } else {
                    accumulators.data.feed(frp);
                }
            }
            FrpBody::ShotFinished(finished) => {
                if let Some(ShotEvent::Finished(shot)) = accumulators.data.feed(frp) {
                    finish_shot(state, accumulators, &finished.key, shot).await;
                }
            }
            FrpBody::GameStroke(stroke) => {
                // One attach path: the stroke joins the shot by `shot_id`,
                // pending or stored, whenever it arrives, from the actor
                // holding `game` only.
                if !accumulators.games.contains(&frp.from) {
                    return;
                }
                let Some(id) = stroke.shot_id.clone() else {
                    return;
                };
                if let Some(p) = accumulators.shots.get_mut(&id) {
                    p.stroke = Some(stroke.clone());
                } else {
                    attach(state, &id, |s| s.data.stroke = Some(stroke.clone())).await;
                }
            }
            FrpBody::GolferTurn(_) if frp.from == state.root.flighthook_id() => {
                accumulators.data.feed(frp);
            }
            FrpBody::PracticeBlock(_) | FrpBody::Removed(_)
                if frp.from == state.root.flighthook_id() =>
            {
                accumulators.data.feed(frp);
                state.blocks.write().await.apply(&frp.body);
            }
            FrpBody::GolferTurn(_) | FrpBody::PracticeBlock(_) | FrpBody::GolferSelect(_) => {}
            _ => {
                accumulators.data.feed(frp);
            }
        },
    }
}

/// Whether the history holds shot `shot_id`.
async fn stored(state: &WebState, shot_id: &str) -> bool {
    state
        .shots
        .read()
        .await
        .iter()
        .rev()
        .any(|s| s.key.shot_id == shot_id)
}

/// The warning for a message from the shot's own launch monitor after its
/// `shot.finished`, which is out of order. A message from any other actor,
/// such as a flight model's `shot.flight`, is an addendum and gets none.
fn late_warning(shot: &CompletedShot, from: &str, body: &FrpBody) -> Option<String> {
    (from == shot.from).then(|| {
        format!(
            "{} from {from} arrived after shot {} ({}) finished",
            body.kind(),
            shot.key.shot_number,
            shot.key.shot_id
        )
    })
}

/// One message for a shot already composed: a flight always attaches; a
/// launch, club or impact only when the shot has none.
fn attach_message(shot: &mut CompletedShot, from: &str, body: &FrpBody) {
    match body {
        FrpBody::ShotLaunch(m) => {
            shot.launch.get_or_insert_with(|| m.launch.clone());
        }
        FrpBody::ShotClub(m) => {
            shot.club.get_or_insert_with(|| m.club.clone());
        }
        FrpBody::ShotImpact(m) => {
            shot.impact.get_or_insert_with(|| m.impact.clone());
        }
        FrpBody::ShotFlight(m) => shot.flights.push(flightrelay::Flight {
            from: from.to_owned(),
            flight: m.flight.clone(),
        }),
        _ => {}
    }
}

/// Change the stored shot `shot_id` with `change`, rebuild its data, and
/// send the UI its ball again. A shot the history no longer holds is left
/// alone.
async fn attach(state: &WebState, shot_id: &str, change: impl FnOnce(&mut StoredShot)) {
    let mut shots = state.shots.write().await;
    let Some(stored) = shots.iter_mut().rev().find(|s| s.key.shot_id == shot_id) else {
        return;
    };
    change(stored);
    rebuild(stored);
    let data = stored.data.clone();
    drop(shots);
    ui_socket::send_shot(state, data);
}

/// Rebuild `stored.data` from its composed shot, keeping what the web layer
/// stamped on it.
fn rebuild(stored: &mut StoredShot) {
    let Some(shot) = &stored.frp else {
        return;
    };
    let fresh = stored::stored_data(shot, stored.data.card.as_deref(), stored.path.as_ref());
    let data = &mut stored.data;
    data.launch = fresh.launch;
    data.club = fresh.club;
    data.impact = fresh.impact;
    data.metrics = fresh.metrics;
    data.source = fresh.source;
    data.flights = fresh.flights;
    data.path = fresh.path;
}

/// The `bus.actor` name of the remote actor `from`, while it is on the bus.
fn remote_name(state: &WebState, from: &str) -> Option<String> {
    state.bus_state.lock().ok().and_then(|bus| {
        bus.actors()
            .find(|(id, _)| *id == from)
            .map(|(_, a)| a.name.trim().to_owned())
            .filter(|name| !name.is_empty())
    })
}

/// The name a game's `golfer.profile` gives golfer `id` while it is on the
/// bus. A flighthook golfer is named by its saved record.
fn game_golfer_name(state: &WebState, id: Option<&flightrelay::GolferId>) -> Option<String> {
    let id = id.filter(|id| id.namespace() != flighthook::GOLFER_NAMESPACE)?;
    state.bus_state.lock().ok().and_then(|bus| {
        bus.of_kind(flightrelay::StateKind::GolferProfile)
            .find_map(|m| match &m.body {
                FrpBody::GolferProfile(p) if p.id == *id => Some(p.name.trim().to_owned()),
                _ => None,
            })
            .filter(|name| !name.is_empty())
    })
}

/// Store the shot `key` that just finished, composed as `shot`, count it in
/// its block and send it to the UI sockets.
async fn finish_shot(
    state: &WebState,
    accumulators: &mut PendingShots,
    key: &ShotKey,
    shot: CompletedShot,
) {
    let Some(PendingShot {
        received_at,
        context,
        turn,
        putting,
        video,
        stroke,
        raw,
        path,
        ..
    }) = accumulators.shots.remove(&key.shot_id)
    else {
        return;
    };
    let card = state.root.card_of_actor(&shot.from);
    let mut data = stored::stored_data(&shot, card.as_deref(), path.as_ref());
    // A shot the device read nothing for is kept for the export when its
    // raw frames are; `/api/shots` leaves it out.
    let counted = data.has_data();
    if !counted && raw.is_none() {
        return;
    }
    let block = shot.block.clone();
    data.turn = shot.turn.clone().or(turn);
    data.club_selected = if putting {
        Some(flighthook::Club::Putter)
    } else {
        data.turn.as_ref().and_then(|t| t.club)
    };
    data.video = video;
    data.stroke = stroke;
    if card.is_none() {
        data.from_name = remote_name(state, &shot.from);
    }
    data.golfer_name = game_golfer_name(state, data.turn.as_ref().and_then(|t| t.golfer.as_ref()));
    if counted {
        data.block.clone_from(&block);
    }
    let stored = StoredShot {
        received_at,
        key: key.clone(),
        context,
        data,
        raw,
        frp: Some(shot),
        path,
    };
    let data = stored.data.clone();

    {
        let mut blocks = state.blocks.write().await;
        if counted && let Some(id) = block.as_deref() {
            let at = u64::try_from(received_at.timestamp_millis()).unwrap_or(0);
            blocks.count(id, at);
        }
        let mut shots = state.shots.write().await;
        let evicted = store_shot(&mut shots, &state.raw_bytes, stored, RAW_BUDGET_BYTES);
        if let Some(id) = evicted.and_then(|old| old.data.block) {
            blocks.release(&id);
        }
    }
    ui_socket::send_shot(state, data);
}

/// Keep the web state current from a launch monitor's or a hosted card's
/// `bus.actor`: its rows (`ready`, battery, the actor's own telemetry) on
/// its card.
async fn apply_device(state: &WebState, from: &str, actor: &flightrelay::BusActor) {
    let card = stored::card_of(&state.root, from);
    let rows = flighthook::bus_actor_rows(actor);
    let mut actors = state.actors.write().await;
    if let Some(entry) = actors.get_mut(&card) {
        // An empty row is one the actor no longer has.
        for (k, v) in rows {
            if v.is_empty() {
                entry.telemetry.remove(&k);
            } else {
                entry.telemetry.insert(k, v);
            }
        }
    }
}

/// Keep the web state current from one local event of card `card` (card
/// statuses, cameras, Clear history and config outcomes).
async fn apply_local(
    state: &WebState,
    card: &str,
    event: &LocalEvent,
    accumulators: &mut PendingShots,
) {
    match event {
        LocalEvent::CardStatus { status, telemetry } => {
            let mut actors = state.actors.write().await;
            let actor = actors.entry(card.to_owned()).or_insert_with(|| {
                let snap = state.root.system.snapshot();
                let names = actor_names(&snap);
                let name = names.get(card).cloned().unwrap_or_default();
                new_actor(name)
            });
            // A lost connection or a new instance drops the telemetry held
            // for the actor.
            actor.apply_status(*status, telemetry, state.now_ms());
            drop(actors);
            // Out of session, the device finishes none of its pending shots.
            if *status != ActorStatus::Connected {
                drop_pending(accumulators, card, None);
            }
        }
        LocalEvent::ShotVideo {
            keys,
            position,
            duration_ms,
            fps,
            trigger_offset_ms,
            impact_offset_ms,
            complete,
            dropped,
        } => {
            let video = ShotVideo {
                camera: card.to_owned(),
                position: *position,
                duration_ms: *duration_ms,
                fps: *fps,
                trigger_offset_ms: *trigger_offset_ms,
                impact_offset_ms: *impact_offset_ms,
                complete: *complete,
                dropped: *dropped,
                pose: false,
                swing: false,
                rotation: 0,
                analyzed_with: None,
            };
            let mut shots = state.shots.write().await;
            for key in keys {
                stamp_video(&mut shots, accumulators, &key.shot_id, Some(&video));
            }
        }
        LocalEvent::ShotVideoRemoved { keys } => {
            let mut shots = state.shots.write().await;
            for key in keys {
                stamp_video(&mut shots, accumulators, &key.shot_id, None);
            }
        }
        LocalEvent::ShotPose { keys, rotation } => {
            let mut shots = state.shots.write().await;
            for key in keys {
                mark_video(&mut shots, accumulators, &key.shot_id, card, |v| {
                    v.pose = true;
                    v.rotation = *rotation;
                    v.analyzed_with = Some(stance::VERSION.to_owned());
                });
            }
        }
        LocalEvent::ShotSwing { keys } => {
            let mut shots = state.shots.write().await;
            for key in keys {
                mark_video(&mut shots, accumulators, &key.shot_id, card, |v| {
                    v.swing = true;
                });
            }
        }
        LocalEvent::ClearShotHistory => {
            // Shots in flight belong to the history being cleared: they end
            // here, so a late `shot.finished` finds nothing to store. The
            // store forgets its shots and blocks, then asks for a new
            // session. The aggregator keeps the turn and the open blocks it
            // follows.
            for (id, _) in accumulators.shots.drain() {
                accumulators.data.discard(&id);
            }
            {
                let mut blocks = state.blocks.write().await;
                blocks.clear();
                let mut shots = state.shots.write().await;
                shots.clear();
                state.raw_bytes.store(0, Ordering::Relaxed);
            }
            send_new_block(state, true);
        }
        LocalEvent::ConfigOutcome {
            started,
            stopped,
            restarted,
            ..
        } => {
            // Names come from the config, and a rename is applied without a
            // restart: every outcome refreshes them.
            let snap = state.root.system.snapshot();
            let names = actor_names(&snap);
            let disabled = snap.disabled_actors();
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
                // connection its old instance lost. The outcome can arrive
                // after the new instance's first statuses (`Starting`, even
                // `Connected`); `restart` keeps the latest status, so the
                // order does not matter.
                for id in restarted {
                    if let Some(entry) = actors.get_mut(id) {
                        entry.restart(now);
                    }
                }
                // Remove stopped actors; one turned off stays, Disabled
                // (below).
                for id in stopped {
                    actors.remove(id);
                }
            }
            // A card turned off shows as Disabled, whether or not an
            // instance of it was running.
            follow_disabled(&mut actors, &names, &disabled);
        }
        _ => {}
    }
}

#[cfg(test)]
pub(crate) mod tests;
