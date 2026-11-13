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
        .route("/api/tournament", post(routes::post_tournament))
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
    if let Some(actor) = state.actors.read().await.get(actor)
        && let Some(v) = actor.telemetry.get("peer")
    {
        scrub.push(v.clone());
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
            FrpBody::BusActor(actor)
                if actor.has_role(flightrelay::types::roles::LAUNCH_MONITOR) =>
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

/// Keep the web state current from a launch monitor's `bus.actor`: its
/// rows (`ready`, battery, the actor's own telemetry) on its card.
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
pub(crate) mod tests {
    use super::*;
    use crate::actors::web::blocks::BlockRecord;
    use crate::state::SessionWriter;
    use flighthook::{Club, Handedness};
    use flighthook::{
        ConnectionState, DerivedFlight, Distance, Launch, MevoSection, RawDir, RawRecord,
        ShotGrouping, Velocity,
    };

    /// Removes the directory on drop, so a failing assertion doesn't leak it.
    pub(crate) struct TempDir(pub(crate) std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One test message, as [`Updater::send`] and [`feed`] put it on the bus
    /// for a card.
    #[derive(Clone)]
    #[allow(clippy::large_enum_variant)] // test messages, built and sent at once
    pub(crate) enum Ev {
        /// An FRP message from the card's actor (its id = the card key).
        Frp(FrpBody),
        /// A local event of the card.
        Local(LocalEvent),
        /// `shot.finished`, after the card's capture of the shot's raw
        /// frames when there is one.
        Finished(ShotKey, Option<Box<ShotRaw>>),
        /// The session's `golfer.turn`, from flighthook.
        Turn(flightrelay::GolferTurn),
    }

    impl Ev {
        pub(crate) fn frp(body: impl Into<FrpBody>) -> Self {
            Self::Frp(body.into())
        }

        pub(crate) fn trigger(key: ShotKey) -> Self {
            Self::frp(flightrelay::ShotTrigger::new(key))
        }

        /// The card's launch monitor `bus.actor` with `device` and
        /// `telemetry`, ready (no blockers).
        pub(crate) fn device(device: flightrelay::DeviceInfo, telemetry: &[(&str, &str)]) -> Self {
            let mut actor = flightrelay::BusActor::new("test", 5.0)
                .with_role(flightrelay::types::roles::LAUNCH_MONITOR);
            actor.device = Some(device);
            actor.telemetry = (!telemetry.is_empty()).then(|| {
                telemetry
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect()
            });
            Self::frp(actor)
        }

        /// The bus messages of `self` sent for card `card`.
        pub(crate) fn bus(self, root: &SystemState, card: &str) -> Vec<BusMsg> {
            let frp = |body: FrpBody| BusMsg::frp(flightrelay::FrpMessage::new(card, body));
            match self {
                Self::Frp(body) => vec![frp(body)],
                Self::Local(event) => vec![BusMsg::local(card, event)],
                Self::Finished(key, raw) => {
                    let mut out = Vec::new();
                    if let Some(raw) = raw {
                        out.push(BusMsg::local(
                            card,
                            LocalEvent::ShotCapture {
                                key: key.clone(),
                                raw,
                                scrub: Vec::new(),
                            },
                        ));
                    }
                    out.push(frp(flightrelay::ShotFinished { key }.into()));
                    out
                }
                Self::Turn(turn) => vec![BusMsg::frp(flightrelay::FrpMessage::new(
                    root.flighthook_id(),
                    turn,
                ))],
            }
        }
    }

    /// The session's turn after `writer` selects `club`, as the session's
    /// own `golfer.turn` on the bus.
    pub(crate) fn turn_to(root: &SystemState, writer: &SessionWriter, club: Club) -> BusMsg {
        let select = flightrelay::GolferSelect {
            club: Some(Some(club)),
            ..flightrelay::GolferSelect::new("test")
        };
        writer.apply(&select);
        BusMsg::frp(flightrelay::FrpMessage::new(
            root.flighthook_id(),
            writer.turn(),
        ))
    }

    fn kv(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// A connected `mevo.0` holding status keys, device-reported keys (and the
    /// identity), and the club and hand its actor reported.
    async fn connected_mevo(up: &mut Updater<'_>) {
        up.send(
            "mevo.0",
            Ev::Local(LocalEvent::CardStatus {
                status: ActorStatus::Connected,
                telemetry: kv(&[("device_info", "FlightScope Xi")]),
            }),
        )
        .await;
        let device = flightrelay::DeviceInfo {
            manufacturer: Some("FlightScope".into()),
            model: Some("Mevo+".into()),
            battery_pct: Some(80),
            ..flightrelay::DeviceInfo::default()
        };
        up.send("mevo.0", Ev::device(device, &[("tilt_state", "ok")]))
            .await;
        up.send(
            "mevo.0",
            Ev::Local(LocalEvent::CardStatus {
                status: ActorStatus::Connected,
                telemetry: kv(&[("club", "7I"), ("handed", "lh")]),
            }),
        )
        .await;
    }

    async fn held(state: &WebState) -> HashMap<String, String> {
        state.actors.read().await["mevo.0"].telemetry.clone()
    }

    #[tokio::test]
    async fn a_lost_connection_clears_the_actors_telemetry() {
        for lost in [ActorStatus::Disconnected, ActorStatus::Reconnecting] {
            let (state, _writer, _dir) = test_state();
            let mut up = Updater::new(&state);
            connected_mevo(&mut up).await;
            // device_info, club, handed and the bus.actor's rows: ready,
            // battery_pct, tilt_state (no tilt or roll: empty rows are
            // left out).
            assert_eq!(held(&state).await.len(), 6, "{:?}", held(&state).await);

            up.send(
                "mevo.0",
                Ev::Local(LocalEvent::CardStatus {
                    status: lost,
                    telemetry: kv(&[("error", "gone")]),
                }),
            )
            .await;
            assert_eq!(held(&state).await, kv(&[("error", "gone")]), "{lost:?}");
            assert_eq!(state.actors.read().await["mevo.0"].status, lost);
            // The export keeps the device's identity: its `bus.actor`.
            let model = state.bus_state.lock().ok().and_then(|bus| {
                bus.actors()
                    .find(|(id, _)| *id == "mevo.0")
                    .and_then(|(_, a)| a.device.clone()?.model)
            });
            assert_eq!(model.as_deref(), Some("Mevo+"));
        }
    }

    #[tokio::test]
    async fn a_new_instance_starts_without_the_old_telemetry() {
        let (state, _writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        connected_mevo(&mut up).await;
        up.status("mevo.0", ActorStatus::Disconnected).await;
        // Values reported after the loss are held until the next start.
        up.send(
            "mevo.0",
            Ev::Local(LocalEvent::CardStatus {
                status: ActorStatus::Disconnected,
                telemetry: kv(&[("club", "DR")]),
            }),
        )
        .await;
        up.send(
            "mevo.0",
            Ev::Local(LocalEvent::CardStatus {
                status: ActorStatus::Starting,
                telemetry: kv(&[("address", "1.2.3.4:5")]),
            }),
        )
        .await;
        assert_eq!(held(&state).await, kv(&[("address", "1.2.3.4:5")]));
    }

    #[tokio::test]
    async fn repeated_statuses_that_are_not_a_loss_merge() {
        let (state, _writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        connected_mevo(&mut up).await;
        up.send(
            "mevo.0",
            Ev::Local(LocalEvent::CardStatus {
                status: ActorStatus::Connected,
                telemetry: kv(&[("shot_count", "1")]),
            }),
        )
        .await;
        assert_eq!(held(&state).await.len(), 7);
        up.send(
            "mevo.0",
            Ev::Local(LocalEvent::CardStatus {
                status: ActorStatus::Disconnected,
                telemetry: kv(&[("error", "a")]),
            }),
        )
        .await;
        for status in [ActorStatus::Reconnecting, ActorStatus::Disconnected] {
            up.send(
                "mevo.0",
                Ev::Local(LocalEvent::CardStatus {
                    status,
                    telemetry: kv(&[("phase", "connecting")]),
                }),
            )
            .await;
        }
        assert_eq!(
            held(&state).await,
            kv(&[("error", "a"), ("phase", "connecting")])
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

    /// A free local port.
    fn free_addr() -> SocketAddr {
        std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port")
    }

    /// GET `path` until `done` holds for the parsed body, at most 5 s.
    fn get_until<T: serde::de::DeserializeOwned>(
        addr: SocketAddr,
        path: &str,
        done: impl Fn(&T) -> bool,
    ) -> T {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let body = http_get(addr, path);
            if let Ok(value) = serde_json::from_str::<T>(&body)
                && done(&value)
            {
                return value;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{path} never as wanted: {body}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// The (owner, kind) of flighthook's own states in `states`.
    fn own_kinds(states: &[flightrelay::FrpMessage], me: &str) -> Vec<String> {
        let mut kinds: Vec<String> = states
            .iter()
            .filter(|m| m.from == me)
            .map(|m| m.body.kind().to_owned())
            .collect();
        kinds.sort();
        kinds
    }

    /// The UI snapshot `/api/ws` sends first.
    fn ui_snapshot(addr: SocketAddr) -> flighthook::UiSnapshot {
        let url = format!("ws://{addr}{}", flighthook::UI_WS_PATH);
        let (mut ws, _) = tungstenite::connect(url).expect("connect");
        loop {
            if let tungstenite::Message::Text(text) = ws.read().expect("read") {
                match serde_json::from_str(&text).expect("a UI message") {
                    flighthook::UiMessage::Snapshot(snapshot) => return *snapshot,
                    other => panic!("not a snapshot first: {other:?}"),
                }
            }
        }
    }

    /// The `/frp` greeting, up to its `bus.sync`.
    fn frp_greeting(addr: SocketAddr) -> Vec<flightrelay::FrpMessage> {
        let url = format!("ws://{addr}{}", flighthook::FRP_PATH);
        let (mut ws, _) = tungstenite::connect(url).expect("connect");
        let mut out = Vec::new();
        loop {
            if let tungstenite::Message::Text(text) = ws.read().expect("read") {
                let msg: flightrelay::FrpMessage = serde_json::from_str(&text).expect("FRP");
                if msg.body.kind() == "bus.sync" {
                    return out;
                }
                out.push(msg);
            }
        }
    }

    /// What a client of the server at `addr` sees of the session: the first
    /// block known, a shot from a launch monitor `lm` filed in Set 1, and
    /// flighthook's actor, turn and block in the UI snapshot and the `/frp`
    /// greeting.
    fn sees_the_session(
        addr: SocketAddr,
        root: &SystemState,
        bus_tx: &broadcast::Sender<BusMsg>,
        shot: u32,
    ) {
        let me = root.flighthook_id();
        let blocks: flighthook::BlocksResponse =
            get_until(addr, "/api/blocks", |b: &flighthook::BlocksResponse| {
                !b.blocks.is_empty()
            });
        let first = &blocks.blocks[0].block;
        assert_eq!(first.number, 1, "the first Set shown is Set 1");
        let key = ShotKey::new(format!("lm-shot-{shot}"), shot);
        for body in [
            FrpBody::from(flightrelay::ShotTrigger::new(key.clone())),
            flightrelay::ShotLaunch {
                key: key.clone(),
                launch: ball(),
            }
            .into(),
            flightrelay::ShotFinished { key: key.clone() }.into(),
        ] {
            let _ = bus_tx.send(BusMsg::frp(flightrelay::FrpMessage::new("lm", body)));
        }
        let shots: Vec<ShotData> = get_until(addr, "/api/shots", |s: &Vec<ShotData>| {
            s.iter().any(|d| d.shot_number == shot)
        });
        let stored = shots.iter().find(|d| d.shot_number == shot).expect("shot");
        assert_eq!(stored.block.as_deref(), Some(first.id.as_str()));
        let want = ["bus.actor", "golfer.turn", "practice.block"];
        assert_eq!(own_kinds(&ui_snapshot(addr).states, &me), want);
        assert_eq!(own_kinds(&frp_greeting(addr), &me), want);
    }

    /// Started as `main` starts it (the system actor, then every actor
    /// from the config, the web server first; nothing seeded by hand), the
    /// web server knows the session; again after a bind-address save, and
    /// local actors outlive the 6 s expiry on their heartbeats.
    #[test]
    fn started_in_main_order_the_web_server_knows_the_session() {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-main-order-{}",
            crate::state::config::generate_id()
        )));
        let (root, writer) = SystemState::new(dir.0.join("config.toml"));
        let root = Arc::new(root);
        let addr = free_addr();
        root.system.update(|c| {
            c.webserver.insert(
                "0".into(),
                flighthook::WebserverSection {
                    name: "Web Server".into(),
                    bind: addr.to_string(),
                },
            );
        });
        let (bus_tx, _) = broadcast::channel(1024);
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::actors::system::start_for_test(writer, &root, &bus_tx, Arc::clone(&shutdown));
        for ra in crate::actors::resolve_actors(&root.system.snapshot()) {
            crate::actors::start_actor(ra.id, ra.actor, &root, &bus_tx);
        }
        sees_the_session(addr, &root, &bus_tx, 1);

        // Past the expiry: the heartbeats keep flighthook's states.
        std::thread::sleep(flightrelay::DEFAULT_HEARTBEAT * 3 + std::time::Duration::from_secs(1));
        let me = root.flighthook_id();
        let want = ["bus.actor", "golfer.turn", "practice.block"];
        assert_eq!(own_kinds(&ui_snapshot(addr).states, &me), want);
        assert_eq!(own_kinds(&frp_greeting(addr), &me), want);

        // A bind-address save starts a new server with an empty store.
        let moved = free_addr();
        let _ = bus_tx.send(BusMsg::local(
            "web",
            LocalEvent::ConfigCommand {
                request_id: None,
                action: Box::new(flighthook::ConfigAction::UpsertWebserver {
                    index: "0".into(),
                    section: flighthook::WebserverSection {
                        name: "Web Server".into(),
                        bind: moved.to_string(),
                    },
                }),
            },
        ));
        sees_the_session(moved, &root, &bus_tx, 2);
        let _ = root.stop_all_and_wait(std::time::Duration::from_secs(3));
        shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
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
        monitor.local(LocalEvent::CardStatus {
            status: ActorStatus::Starting,
            telemetry: HashMap::from([("bind".into(), "first".into())]),
        });

        let body = http_get(addr, "/api/status");
        web.stop();
        let status: flighthook::StatusResponse = serde_json::from_str(&body).expect("json");
        let monitor = &status.cards["mock_monitor.0"];
        assert_eq!(monitor.status, ActorStatus::Starting);
        assert_eq!(monitor.telemetry["bind"], "first");
    }

    fn status_event(status: ActorStatus) -> Ev {
        Ev::Local(LocalEvent::CardStatus {
            status,
            telemetry: HashMap::new(),
        })
    }

    fn outcome(started: &[&str], stopped: &[&str], restarted: &[&str]) -> Ev {
        let ids = |ids: &[&str]| ids.iter().map(|s| (*s).to_owned()).collect();
        Ev::Local(LocalEvent::ConfigOutcome {
            request_id: None,
            restarted: ids(restarted),
            stopped: ids(stopped),
            started: ids(started),
        })
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
            server.local(LocalEvent::CardStatus {
                status,
                telemetry: HashMap::new(),
            });
        }

        // Each page load (a UI connecting) reads the same history, the red
        // time counting down between them.
        let red = |body: &str| {
            let status: flighthook::StatusResponse = serde_json::from_str(body).expect("json");
            status
                .cards
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
        bus_tx: broadcast::Sender<BusMsg>,
        actor_id: &str,
    ) -> WebState {
        WebState {
            root,
            bus_tx,
            actors: RwLock::new(HashMap::new()),
            epoch: std::time::Instant::now(),
            shots: RwLock::new(VecDeque::new()),
            blocks: RwLock::new(BlockStore::new()),
            raw_bytes: AtomicUsize::new(0),
            bus_state: Mutex::default(),
            ui_out: broadcast::channel(1024).0,
            config_generation: AtomicU64::new(0),
            addr: "127.0.0.1:0".parse().expect("addr"),
            actor_id: actor_id.into(),
            ws_count: AtomicU64::new(0),
            request_count: AtomicU64::new(0),
            auto_chip_shots: AtomicU32::new(0),
            reconnecting: std::sync::Mutex::default(),
            looped_links: std::sync::Mutex::default(),
        }
    }

    /// A web state over `root` that has taken `msgs` off the bus, as the
    /// running web server folds them.
    pub(crate) async fn folded(root: Arc<SystemState>, msgs: &[BusMsg]) -> Arc<WebState> {
        let (bus_tx, _) = broadcast::channel(1024);
        let state = Arc::new(web_state(root, bus_tx, "webserver.0"));
        let mut accumulators = PendingShots::new();
        for msg in msgs {
            apply_bus_event(&state, msg, &mut accumulators).await;
        }
        state
    }

    pub(crate) fn test_state() -> (WebState, SessionWriter, TempDir) {
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

    /// `shot_video` from `camera` for `keys`, lasting `duration_ms`.
    fn shot_video(keys: &[&ShotKey], duration_ms: u64) -> Ev {
        Ev::Local(LocalEvent::ShotVideo {
            keys: keys.iter().map(|k| (*k).clone()).collect(),
            position: flighthook::CameraPosition::GolferFront,
            duration_ms,
            fps: 120.0,
            trigger_offset_ms: 4000,
            impact_offset_ms: 4000,
            complete: true,
            dropped: 0,
        })
    }

    /// The camera actors of the stored shot `shot_id`'s clips, with their
    /// durations.
    async fn stored_video(state: &WebState, shot_id: &str) -> Vec<(String, u64)> {
        state
            .shots
            .read()
            .await
            .iter()
            .find(|s| s.key.shot_id == shot_id)
            .map(|s| {
                s.data
                    .video
                    .iter()
                    .map(|v| (v.camera.clone(), v.duration_ms))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A stored shot keeps the source its launch monitor declared in its
    /// `bus.actor`; one from a launch monitor that declared none is stored
    /// without it.
    #[tokio::test]
    async fn a_stored_shot_keeps_its_provenance() {
        let (state, _writer, _dir) = test_state();
        let mut pending = PendingShots::new();
        for (n, actor, provenance) in [
            (1, "square.0", Some(flighthook::Source::Measured)),
            (2, "mock_monitor.0", Some(flighthook::Source::Virtual)),
            (3, "exputt.0", None),
        ] {
            let key = ShotKey {
                shot_id: format!("uuid-{n}"),
                shot_number: n,
            };
            let mut declared = flightrelay::BusActor::new(actor, 5.0)
                .with_role(flightrelay::types::roles::LAUNCH_MONITOR);
            declared.device = Some(flightrelay::DeviceInfo {
                source: provenance,
                ..flightrelay::DeviceInfo::default()
            });
            let declared = BusMsg::frp(flightrelay::FrpMessage::new(actor, declared));
            apply_bus_event(&state, &declared, &mut pending).await;
            apply_ev(&state, actor, Ev::trigger(key.clone()), &mut pending).await;
            for event in [
                Ev::frp(flightrelay::ShotLaunch {
                    key: key.clone(),
                    launch: ball(),
                }),
                Ev::Finished(key, None),
            ] {
                apply_ev(&state, actor, event, &mut pending).await;
            }
        }
        let stored: Vec<_> = state
            .shots
            .read()
            .await
            .iter()
            .map(|s| (s.key.shot_number, s.data.source))
            .collect();
        assert_eq!(
            stored,
            [
                (1, Some(flighthook::Source::Measured)),
                (2, Some(flighthook::Source::Virtual)),
                (3, None),
            ]
        );
    }

    #[tokio::test]
    async fn shot_video_stamps_every_listed_shot_pending_or_stored() {
        let (state, _writer, _dir) = test_state();
        let mut pending = PendingShots::new();
        let a = ShotKey {
            shot_id: "uuid-a".into(),
            shot_number: 1,
        };
        let b = ShotKey {
            shot_id: "uuid-b".into(),
            shot_number: 1,
        };
        let mut apply = async |actor: &str, event: Ev| {
            apply_ev(&state, actor, event, &mut pending).await;
        };
        // a: stored before its clip; b (another device): still pending.
        apply("mevo.0", Ev::trigger(a.clone())).await;
        apply(
            "mevo.0",
            Ev::frp(flightrelay::ShotLaunch {
                key: a.clone(),
                launch: ball(),
            }),
        )
        .await;
        apply("mevo.0", Ev::Finished(a.clone(), None)).await;
        apply("r10.0", Ev::trigger(b.clone())).await;
        apply(
            "r10.0",
            Ev::frp(flightrelay::ShotLaunch {
                key: b.clone(),
                launch: ball(),
            }),
        )
        .await;

        apply("camera.0", shot_video(&[&a, &b], 5000)).await;
        apply("camera.1", shot_video(&[&a], 5100)).await;
        // A camera's newer clip of the shot replaces its earlier one.
        apply("camera.0", shot_video(&[&a], 5200)).await;
        // A shot the history doesn't hold is ignored.
        let gone = ShotKey {
            shot_id: "uuid-gone".into(),
            shot_number: 9,
        };
        apply("camera.0", shot_video(&[&gone], 1)).await;
        assert_eq!(
            stored_video(&state, "uuid-a").await,
            [("camera.0".to_owned(), 5200), ("camera.1".to_owned(), 5100)]
        );
        assert!(stored_video(&state, "uuid-b").await.is_empty(), "pending");

        apply("r10.0", Ev::Finished(b.clone(), None)).await;
        assert_eq!(
            stored_video(&state, "uuid-b").await,
            [("camera.0".to_owned(), 5000)]
        );

        // `/api/shots` carries it; pruned clips leave the rows.
        let rows = serde_json::to_value(
            state
                .shots
                .read()
                .await
                .iter()
                .map(|s| s.data.clone())
                .collect::<Vec<_>>(),
        )
        .expect("ser");
        assert_eq!(rows[0]["video"][1]["camera"], "camera.1");
        assert_eq!(rows[1]["video"][0]["duration_ms"], 5000);
        apply(
            "camera.0",
            Ev::Local(LocalEvent::ShotVideoRemoved {
                keys: vec![a.clone()],
            }),
        )
        .await;
        assert!(stored_video(&state, "uuid-a").await.is_empty());
        assert_eq!(stored_video(&state, "uuid-b").await.len(), 1);
    }

    #[tokio::test]
    async fn shot_pose_marks_that_cameras_clip_of_every_listed_shot() {
        let (state, _writer, _dir) = test_state();
        let mut pending = PendingShots::new();
        let a = ShotKey {
            shot_id: "uuid-a".into(),
            shot_number: 1,
        };
        let mut apply = async |actor: &str, event: Ev| {
            apply_ev(&state, actor, event, &mut pending).await;
        };
        apply("mevo.0", Ev::trigger(a.clone())).await;
        apply(
            "mevo.0",
            Ev::frp(flightrelay::ShotLaunch {
                key: a.clone(),
                launch: ball(),
            }),
        )
        .await;
        apply("camera.0", shot_video(&[&a], 5000)).await;
        apply("camera.1", shot_video(&[&a], 5000)).await;
        // Pending still: marked, and stored with the mark.
        apply(
            "camera.1",
            Ev::Local(LocalEvent::ShotPose {
                keys: vec![a.clone()],
                rotation: 0,
            }),
        )
        .await;
        apply("mevo.0", Ev::Finished(a.clone(), None)).await;
        let poses = |state: &WebState| {
            let shots = state.shots.try_read().expect("shots");
            shots
                .iter()
                .find(|s| s.key.shot_id == "uuid-a")
                .map(|s| s.data.video.iter().map(|v| v.pose).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        assert_eq!(poses(&state), [false, true]);
        apply(
            "camera.0",
            Ev::Local(LocalEvent::ShotPose {
                keys: vec![a.clone()],
                rotation: 0,
            }),
        )
        .await;
        assert_eq!(poses(&state), [true, true]);
        let rows = serde_json::to_value(&state.shots.read().await[0].data).expect("ser");
        assert_eq!(rows["video"][0]["pose"], true);
        assert!(
            rows["video"][0].get("swing").is_none(),
            "omitted while false"
        );
        apply(
            "camera.1",
            Ev::Local(LocalEvent::ShotSwing {
                keys: vec![a.clone()],
            }),
        )
        .await;
        let rows = serde_json::to_value(&state.shots.read().await[0].data).expect("ser");
        assert!(rows["video"][0].get("swing").is_none());
        assert_eq!(rows["video"][1]["swing"], true);
    }

    /// The shot export's JSONL lines (exported at "t", native units, raw
    /// frames included) for the shots `messages` complete, with `config` in
    /// effect, as the web layer stores them.
    pub(crate) async fn export_lines(
        config: &flighthook::FlighthookConfig,
        messages: &[BusMsg],
    ) -> Vec<String> {
        let (state, _writer, _dir) = test_state();
        state.root.system.update(|c| *c = config.clone());
        let mut acc = PendingShots::new();
        for msg in messages {
            apply_bus_event(&state, msg, &mut acc).await;
        }
        let shots = state.shots.read().await;
        shots
            .iter()
            .map(|s| super::export::jsonl_line(s, "t", None, true, config))
            .collect()
    }

    /// One message of card `actor`, folded.
    pub(crate) async fn apply_ev(
        state: &WebState,
        actor: &str,
        event: Ev,
        accumulators: &mut PendingShots,
    ) {
        for msg in event.bus(&state.root, actor) {
            apply_bus_event(state, &msg, accumulators).await;
        }
    }

    /// Messages of card `actor`, folded in order.
    pub(crate) async fn feed(state: &WebState, actor: &str, events: Vec<Ev>) {
        let mut acc = PendingShots::new();
        for event in events {
            for msg in event.bus(&state.root, actor) {
                apply_bus_event(state, &msg, &mut acc).await;
            }
        }
    }

    /// Local events of `card`, folded in order.
    pub(crate) async fn feed_local(state: &WebState, card: &str, events: Vec<LocalEvent>) {
        let mut acc = PendingShots::new();
        for event in events {
            apply_bus_event(state, &BusMsg::local(card, event), &mut acc).await;
        }
    }

    /// The session's turn with `club` in play, as the session's
    /// `golfer.turn`.
    pub(crate) fn turn_event(club: Club) -> Ev {
        Ev::Turn(flightrelay::GolferTurn::new(
            None,
            Some(club),
            flighthook::Handedness::Right,
            Mode::Full,
        ))
    }

    fn ball() -> Launch {
        Launch {
            launch_speed: Some(Velocity::MetersPerSecond(40.0)),
            launch_elevation: Some(20.0),
            ..Launch::default()
        }
    }

    /// A message from the shot's own launch monitor after its finish logs a
    /// warning; a flight model's addendum does not.
    #[test]
    fn only_the_launch_monitors_own_late_message_warns() {
        let mut agg = flightrelay::ShotAggregator::new();
        let from = "monitor-id";
        agg.feed(&flightrelay::FrpMessage::new(
            from,
            flightrelay::ShotTrigger::new(key(1)),
        ));
        let Some(flightrelay::ShotEvent::Finished(shot)) = agg.feed(&flightrelay::FrpMessage::new(
            from,
            flightrelay::ShotFinished { key: key(1) },
        )) else {
            panic!("finished");
        };
        let body: FrpBody = flightrelay::ShotFlight {
            key: key(1),
            flight: flightrelay::FlightResult::new(flightrelay::Basis::Modeled),
        }
        .into();
        let warning = late_warning(&shot, from, &body).expect("a warning");
        assert_eq!(
            warning,
            "shot.flight from monitor-id arrived after shot 1 (shot-1) finished"
        );
        assert_eq!(late_warning(&shot, "flight-model-id", &body), None);
    }

    /// The device's own id that a card's capture carries is scrubbed from
    /// the exported frames, even when the card's `device_info` text does
    /// not name it.
    #[tokio::test]
    async fn a_raw_frame_with_the_bare_device_id_is_scrubbed_on_export() {
        const SSID: &str = "FS M2-123456";
        const BLE: &str = "C4:3A:11:22:33:44";
        let (state, _writer, _dir) = test_state();
        let mut acc = PendingShots::new();
        apply_bus_event(
            &state,
            &BusMsg::local(
                "mevo.0",
                LocalEvent::CardStatus {
                    status: ActorStatus::Connected,
                    telemetry: HashMap::from([("device_info".into(), "FlightScope Xi".into())]),
                },
            ),
            &mut acc,
        )
        .await;
        apply_ev(&state, "mevo.0", Ev::trigger(key(1)), &mut acc).await;
        let raw = ShotRaw {
            transport: "flightscope-tcp".into(),
            frames: vec![
                RawRecord::text(RawDir::Rx, format!("ssid={SSID}")),
                RawRecord::text(RawDir::Rx, format!("peer {BLE} up")),
            ],
            ..ShotRaw::default()
        };
        apply_bus_event(
            &state,
            &BusMsg::local(
                "mevo.0",
                LocalEvent::ShotCapture {
                    key: key(1),
                    raw: Box::new(raw),
                    scrub: vec![SSID.into(), BLE.into()],
                },
            ),
            &mut acc,
        )
        .await;
        apply_bus_event(
            &state,
            &BusMsg::frp(flightrelay::FrpMessage::new(
                "mevo.0",
                flightrelay::ShotFinished { key: key(1) },
            )),
            &mut acc,
        )
        .await;

        let shots = state.shots.read().await;
        let line = export::build_export_line(
            &shots[0],
            "now",
            None,
            true,
            &flighthook::FlighthookConfig::default(),
        );
        let json = serde_json::to_string(&line).expect("json");
        assert!(!json.contains(SSID), "{json}");
        assert!(!json.contains(BLE), "{json}");
        let texts: Vec<_> = line
            .raw
            .as_ref()
            .expect("raw")
            .frames
            .iter()
            .filter_map(|f| f.text.as_deref())
            .collect();
        assert_eq!(texts, ["ssid=<redacted>", "peer <redacted> up"]);
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
                Ev::trigger(key(1)),
                Ev::frp(flightrelay::ShotLaunch {
                    key: key(1),
                    launch: ball(),
                }),
                Ev::Finished(key(1), None),
            ],
        )
        .await;
        // The flight model flies the finished shot from its own actor.
        feed(
            &state,
            &state.root.flight_model_id(),
            vec![Ev::frp(flightrelay::ShotFlight {
                key: key(1),
                flight: flighthook::FlightResult {
                    carry_distance: Some(Distance::Meters(150.9)),
                    ..flighthook::FlightResult::new(flighthook::Basis::Modeled)
                },
            })],
        )
        .await;
        let shots = state.shots.read().await;
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].data.shown().derived, Some(derived));
        assert!(shots[0].raw.is_none());
    }

    #[tokio::test]
    async fn trigger_snapshots_context_and_finish_stores_raw() {
        let (state, writer, _dir) = test_state();
        let iron8 = turn_to(&state.root, &writer, Club::Iron8);
        state.root.system.update(|c| {
            c.mevo.insert(
                "0".into(),
                MevoSection {
                    name: "Range Mevo".into(),
                    driver: flighthook::MevoConfig {
                        address: Some("10.0.0.7:5100".into()),
                        ball_type: Some(1),
                        track_pct: Some(80.0),
                        ..flighthook::MevoConfig::default()
                    },
                    ..MevoSection::default()
                },
            );
        });
        let mut acc = PendingShots::new();
        // The card's session: Connected with its card text (here the
        // device's id), then its `bus.actor` with the device's identity.
        apply_bus_event(
            &state,
            &BusMsg::local(
                "mevo.0",
                LocalEvent::CardStatus {
                    status: ActorStatus::Connected,
                    telemetry: HashMap::from([("device_info".into(), "FS M2-000001".into())]),
                },
            ),
            &mut acc,
        )
        .await;
        let mut declared = flightrelay::BusActor::new("mevo.0", 5.0)
            .with_role(flightrelay::types::roles::LAUNCH_MONITOR);
        declared.device = Some(flightrelay::DeviceInfo {
            manufacturer: Some("FlightScope".into()),
            model: Some("Xi".into()),
            ..flightrelay::DeviceInfo::default()
        });
        apply_bus_event(
            &state,
            &BusMsg::frp(flightrelay::FrpMessage::new("mevo.0", declared)),
            &mut acc,
        )
        .await;

        apply_bus_event(&state, &iron8, &mut acc).await;
        apply_ev(&state, "mevo.0", Ev::trigger(key(1)), &mut acc).await;
        // A club change after the trigger does not change the shot's club.
        let driver = turn_to(&state.root, &writer, Club::Driver);
        apply_bus_event(&state, &driver, &mut acc).await;
        for event in [
            Ev::frp(flightrelay::ShotLaunch {
                key: key(1),
                launch: ball(),
            }),
            Ev::Finished(key(1), Some(Box::new(raw(3, 4)))),
        ] {
            apply_ev(&state, "mevo.0", event, &mut acc).await;
        }

        let shots = state.shots.read().await;
        let s = &shots[0];
        assert!(s.received_at <= Utc::now());
        assert_eq!(s.key, key(1));
        assert_eq!(s.data.club_selected(), Some(Club::Iron8));
        assert_eq!(s.context.detection_mode, Some(Mode::Full));
        // `/api/shots` carries the turn on the shot itself.
        assert_eq!(s.data.handed(), Some(Handedness::Right));
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
                Ev::trigger(key(1)),
                Ev::Finished(key(1), Some(Box::new(raw(2, 8)))),
                Ev::trigger(key(2)),
                Ev::Finished(key(2), None),
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
                Ev::trigger(key(1)),
                Ev::frp(flightrelay::ShotLaunch {
                    key: key(1),
                    launch: ball(),
                }),
                Ev::Finished(key(1), Some(Box::new(raw(2, 2)))),
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
            context: ExportContext::default(),
            data: ShotData {
                from: "mevo-id".into(),
                card: Some("mevo.0".into()),
                shot_number: n,
                launch: Some(ball()),
                ..ShotData::default()
            },
            raw,
            frp: None,
            path: None,
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
    /// shot's trigger and finish can be split around other events, with
    /// the practice actor running beside the web layer: it sees every
    /// message the web layer folds, and what it publishes is folded next.
    pub(crate) struct Updater<'a> {
        state: &'a WebState,
        pending: PendingShots,
        practice: crate::state::practice::Practice,
        system: BusSender,
        rx: broadcast::Receiver<BusMsg>,
    }

    impl<'a> Updater<'a> {
        pub(crate) fn new(state: &'a WebState) -> Self {
            let rx = state.bus_tx.subscribe();
            let system = BusSender::new("system".into(), state.bus_tx.clone(), Arc::default())
                .with_frp_id(Some(state.root.flighthook_id()));
            let (practice, first) = crate::state::practice::Practice::with_config(
                state.root.system.view(),
                state.root.session.turn(),
                0,
            );
            crate::actors::system::publish_blocks(first, &system);
            Self {
                state,
                pending: PendingShots::new(),
                practice,
                system,
                rx,
            }
        }

        pub(crate) async fn send(&mut self, actor: &str, event: Ev) {
            for m in event.bus(&self.state.root, actor) {
                apply_bus_event(self.state, &m, &mut self.pending).await;
                crate::actors::system::handle_practice(&m, &mut self.practice, &self.system);
            }
            self.settle().await;
        }

        /// New set (or New session) from flighthook, as `POST /api/blocks`
        /// sends it.
        pub(crate) async fn new_block(&mut self, new_session: bool) {
            send_new_block(self.state, new_session);
            self.settle().await;
        }

        /// A bus message as it is.
        async fn bus(&mut self, msg: &BusMsg) {
            apply_bus_event(self.state, msg, &mut self.pending).await;
            crate::actors::system::handle_practice(msg, &mut self.practice, &self.system);
            self.settle().await;
        }

        /// Fold what is on the bus (the practice actor's blocks, the web
        /// layer's own commands) until nothing new arrives.
        async fn settle(&mut self) {
            while let Ok(msg) = self.rx.try_recv() {
                if msg.as_local().is_some() {
                    continue;
                }
                apply_bus_event(self.state, &msg, &mut self.pending).await;
                crate::actors::system::handle_practice(&msg, &mut self.practice, &self.system);
            }
        }

        async fn trigger(&mut self, n: u32) {
            self.trigger_as("mevo.0", key(n)).await;
        }

        async fn finish(&mut self, n: u32) {
            self.finish_as("mevo.0", key(n)).await;
        }

        pub(crate) async fn shot(&mut self, n: u32) {
            self.trigger(n).await;
            self.finish(n).await;
        }

        async fn trigger_as(&mut self, actor: &str, key: ShotKey) {
            self.send(actor, Ev::trigger(key)).await;
        }

        /// Ball data, then the finish.
        async fn finish_as(&mut self, actor: &str, key: ShotKey) {
            self.send(
                actor,
                Ev::frp(flightrelay::ShotLaunch {
                    key: key.clone(),
                    launch: ball(),
                }),
            )
            .await;
            self.send(actor, Ev::Finished(key, None)).await;
        }

        async fn shot_as(&mut self, actor: &str, key: ShotKey) {
            self.trigger_as(actor, key.clone()).await;
            self.finish_as(actor, key).await;
        }

        async fn status(&mut self, actor: &str, status: ActorStatus) {
            self.send(
                actor,
                Ev::Local(LocalEvent::CardStatus {
                    status,
                    telemetry: HashMap::new(),
                }),
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

        /// A club change as the bus carries it: the session's turn (the
        /// system actor's write) and its `golfer.turn`.
        pub(crate) async fn club(&mut self, writer: &SessionWriter, club: Club) {
            let turn = turn_to(&self.state.root, writer, club);
            self.bus(&turn).await;
        }

        /// A golfer and club pick as the bus carries it: the session's
        /// turn and its `golfer.turn`.
        async fn pick(&mut self, writer: &SessionWriter, golfer: &str, club: Club) {
            let select = flightrelay::GolferSelect {
                golfer: Some(Some(golfer.parse().expect("golfer id"))),
                club: Some(Some(club)),
                ..flightrelay::GolferSelect::new("test")
            };
            writer.apply(&select);
            let turn = BusMsg::frp(flightrelay::FrpMessage::new(
                self.state.root.flighthook_id(),
                writer.turn(),
            ));
            self.bus(&turn).await;
        }

        /// A golfers save as the system actor follows it: the practice
        /// actor keys its open blocks again.
        async fn golfers_saved(&mut self) {
            let changes = self.practice.rekey(0);
            crate::actors::system::publish_blocks(changes, &self.system);
            self.settle().await;
        }

        /// A persisted grouping change, as the system actor applies it.
        async fn grouping(&mut self, mode: ShotGrouping) {
            self.state.root.system.update(|c| c.shot_grouping = mode);
            self.send(
                "system",
                Ev::Local(LocalEvent::ConfigOutcome {
                    request_id: None,
                    restarted: Vec::new(),
                    stopped: Vec::new(),
                    started: Vec::new(),
                }),
            )
            .await;
        }
    }

    /// A practice block change on the bus.
    #[derive(Debug, Clone, PartialEq)]
    enum BlockEvent {
        /// `practice.block`: opened, or republished in place.
        Open(flightrelay::PracticeBlock),
        /// The block's removal: closed.
        Closed(String),
    }

    impl BlockEvent {
        fn open(&self) -> Option<&flightrelay::PracticeBlock> {
            match self {
                Self::Open(b) => Some(b),
                Self::Closed(_) => None,
            }
        }
    }

    /// The practice actor's block changes on the bus so far, as
    /// `(from, change)`.
    fn group_events(rx: &mut broadcast::Receiver<BusMsg>) -> Vec<(String, BlockEvent)> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            let Some(frp) = msg.as_frp() else { continue };
            match &frp.body {
                FrpBody::PracticeBlock(b) => {
                    out.push((frp.from.clone(), BlockEvent::Open(b.clone())));
                }
                FrpBody::Removed(r) if r.kind == flightrelay::StateKind::PracticeBlock => {
                    let id = r.id.clone().unwrap_or_default();
                    out.push((frp.from.clone(), BlockEvent::Closed(id)));
                }
                _ => {}
            }
        }
        out
    }

    /// A set as the store holds it.
    #[derive(Debug, Clone, PartialEq)]
    struct Set {
        id: String,
        number: u32,
        session: u32,
        club: Option<Club>,
        open: bool,
        shots: u32,
    }

    impl From<&BlockRecord> for Set {
        fn from(r: &BlockRecord) -> Self {
            Self {
                id: r.block.id.clone(),
                number: r.block.number,
                session: r.block.session,
                club: r.block.club,
                open: r.open,
                shots: r.shots,
            }
        }
    }

    /// The sets as the UI is told of them: every block that is open or has
    /// stored shots, and the open block of the golfer up.
    struct GroupsView {
        list: Vec<Set>,
        current: Option<Set>,
        session: u32,
    }

    impl GroupsView {
        fn list(&self) -> Vec<Set> {
            self.list.clone()
        }

        fn current(&self) -> Option<Set> {
            self.current.clone()
        }

        fn session(&self) -> u32 {
            self.session
        }
    }

    async fn groups_view(state: &WebState) -> GroupsView {
        let golfer = state.root.session.turn().golfer;
        let blocks = state.blocks.read().await;
        GroupsView {
            list: blocks
                .all()
                .iter()
                .filter(|r| r.open || r.shots > 0)
                .map(Set::from)
                .collect(),
            current: blocks
                .all()
                .iter()
                .find(|r| r.open && r.block.golfer == golfer)
                .map(Set::from),
            session: blocks.session(),
        }
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
            .block
            .clone()
    }

    async fn group_info(state: &WebState, id: &str) -> Set {
        groups_view(state)
            .await
            .list()
            .into_iter()
            .find(|g| g.id == id)
            .expect("group kept")
    }

    #[tokio::test]
    async fn stored_shot_is_stamped_with_id_club_block_and_turn() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::ByClub).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        // Dataless, raw only: stored for the export, never counted.
        up.trigger(2).await;
        up.send("mevo.0", Ev::Finished(key(2), Some(Box::new(raw(1, 2)))))
            .await;

        let shots = state.shots.read().await.clone();
        let current = groups_view(&state).await.current().expect("open");
        let s1 = &shots[0].data;
        assert_eq!(s1.shot_id.as_deref(), Some("shot-1"));
        assert_eq!(s1.club_selected(), Some(Club::Iron7));
        assert_eq!(s1.block.as_deref(), Some(current.id.as_str()));
        let s2 = &shots[1].data;
        assert_eq!(s2.shot_id.as_deref(), Some("shot-2"));
        assert_eq!(s2.club_selected(), Some(Club::Iron7));
        assert_eq!(s2.block, None);
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
        assert_eq!(v["block"], current.id.as_str());
        // The turn the shot was hit in (no golfer yet).
        assert_eq!(v["turn"]["club"], "7I");
        assert!(v["turn"]["golfer"].is_null());
    }

    #[tokio::test]
    async fn mode_none_still_records_sets() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::None).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        up.new_block(false).await;
        up.shot(2).await;
        up.club(&writer, Club::Driver).await;
        up.shot(3).await;
        up.new_block(true).await;
        up.shot(4).await;

        let mut ids = Vec::new();
        for n in 1..=4 {
            ids.push(group_of(&state, n).await.expect("grouped"));
        }
        // New set, a club change and New session each start a set.
        for pair in ids.windows(2) {
            assert_ne!(pair[0], pair[1]);
        }
        let list = groups_view(&state).await.list();
        assert_eq!(list.len(), 4);
        assert_eq!(
            list.iter().map(|g| g.session).collect::<Vec<_>>(),
            [1, 1, 1, 2]
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
            assert_eq!(shot.block.as_deref(), Some(id.as_str()));
        }
    }

    #[tokio::test]
    async fn clear_history_empties_the_shots_and_sets_and_starts_a_new_session() {
        let (state, writer, _dir) = test_state();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        up.new_block(false).await;
        up.trigger(2).await;
        up.send("mevo.0", Ev::Finished(key(2), Some(Box::new(raw(3, 64)))))
            .await;
        up.club(&writer, Club::Driver).await;
        up.shot(3).await;
        assert_eq!(state.shots.read().await.len(), 3);
        assert!(state.raw_bytes.load(Ordering::Relaxed) > 0);
        assert_eq!(groups_view(&state).await.list().len(), 2);
        let _ = group_events(&mut rx);

        up.send("web", Ev::Local(LocalEvent::ClearShotHistory))
            .await;
        assert!(state.shots.read().await.is_empty());
        assert_eq!(state.raw_bytes.load(Ordering::Relaxed), 0);
        {
            // The store forgot every block; the practice actor opened the
            // new session's first block, still empty.
            let groups = groups_view(&state).await;
            let current = groups.current().expect("open");
            assert_eq!(groups.list(), vec![current.clone()]);
            assert_eq!((current.number, current.session, current.shots), (1, 2, 0));
            assert_eq!(groups.session(), 2);
        }
        // The practice actor closes the open block and opens the new
        // session's.
        let events = group_events(&mut rx);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[0].1, BlockEvent::Closed(_)));
        assert_eq!(
            events[1].1.open().map(|b| (b.number, b.session)),
            Some((1, 2))
        );

        // The next shot joins set 1 of the new session, with the club in
        // play kept.
        up.shot(4).await;
        let shots = state.shots.read().await.clone();
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].data.club_selected(), Some(Club::Driver));
        let id = shots[0].data.block.clone().expect("grouped");
        let info = group_info(&state, &id).await;
        assert_eq!((info.number, info.session, info.shots), (1, 2, 1));
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
            Ev::frp(flightrelay::ShotLaunch {
                key: key(2),
                launch: ball(),
            }),
        )
        .await;
        let _ = group_events(&mut rx);

        up.send("web", Ev::Local(LocalEvent::ClearShotHistory))
            .await;
        let _ = group_events(&mut rx);
        // The device finishes the shot it triggered before the clear.
        up.finish(2).await;
        assert!(state.shots.read().await.is_empty(), "nothing stored");
        let list = groups_view(&state).await.list();
        assert_eq!(list.len(), 1, "the new session's empty block: {list:?}");
        assert_eq!(list[0].shots, 0);
        assert!(group_events(&mut rx).is_empty(), "no set comes back");
        // A dropout afterward has nothing to end.
        up.reconnect("mevo.0").await;
        assert!(group_events(&mut rx).is_empty());

        // The next trigger is a fresh shot in a fresh set.
        up.shot(3).await;
        let shots = state.shots.read().await.clone();
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].key.shot_number, 3);
        let id = shots[0].data.block.clone().expect("grouped");
        assert_eq!(group_info(&state, &id).await.number, 1);
    }

    #[tokio::test]
    async fn exported_lines_carry_the_block_in_every_view() {
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
            let id = stored.data.block.clone().expect("grouped");
            let line: serde_json::Value = serde_json::from_str(&export::jsonl_line(
                &stored,
                "t",
                None,
                false,
                &flighthook::FlighthookConfig::default(),
            ))
            .expect("json");
            assert_eq!(line["block"], id.as_str(), "{view:?}");
            assert_eq!(line["data"]["block"], id.as_str(), "{view:?}");
            let row = export::to_csv_row(
                &stored,
                flighthook::UnitSystem::Imperial,
                false,
                &flighthook::FlighthookConfig::default(),
            );
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
        up.new_block(false).await;
        up.finish(1).await;

        let old = group_of(&state, 1).await.expect("grouped");
        let old_info = group_info(&state, &old).await;
        assert_eq!(old_info.shots, 1);
        assert!(!old_info.open, "New group closed it");
        let new = groups_view(&state).await.current().expect("open");
        assert_ne!(new.id, old);
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
            state.shots.read().await[0].data.club_selected(),
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
        assert_eq!(groups_view(&state).await.list().len(), 3);
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
        let set = groups_view(&state).await.current().expect("open");
        group_events(&mut rx);
        for view in [
            ShotGrouping::None,
            ShotGrouping::ByClub,
            ShotGrouping::BySet,
        ] {
            up.grouping(view).await;
            assert!(group_events(&mut rx).is_empty(), "{view:?}");
            let current = groups_view(&state).await.current();
            assert_eq!(current.as_ref(), Some(&set), "{view:?}");
        }
        up.shot(2).await;
        assert_eq!(group_of(&state, 2).await, Some(set.id));
    }

    #[tokio::test]
    async fn practice_blocks_on_create_retarget_and_session() {
        let (state, writer, _dir) = test_state();
        let me = state.root.flighthook_id();
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::ByClub).await;
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;

        // The first block opens at start with no club and takes the 7-iron
        // in place while empty; the shot joining it is the store's count.
        let events = group_events(&mut rx);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events.iter().all(|(from, _)| *from == me), "{events:?}");
        let created = events[0].1.open().expect("open").clone();
        assert_eq!((created.number, created.club), (1, None));
        let moved = events[1].1.open().expect("open");
        assert_eq!((moved.id.as_str(), moved.number), (created.id.as_str(), 1));
        assert_eq!(moved.club, Some(Club::Iron7));
        let joined = group_info(&state, &created.id).await;
        assert_eq!((joined.shots, joined.open), (1, true));

        // DR closes the 7-iron set and starts set 2.
        up.club(&writer, Club::Driver).await;
        up.shot(2).await;
        let events = group_events(&mut rx);
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].1, BlockEvent::Closed(created.id.clone()));
        let second = events[1].1.open().expect("open").clone();
        assert_eq!((second.number, second.club), (2, Some(Club::Driver)));
        assert_eq!(group_info(&state, &second.id).await.shots, 1);
        assert!(!group_info(&state, &created.id).await.open);

        up.new_block(true).await;
        let events = group_events(&mut rx);
        assert_eq!(
            events.len(),
            2,
            "the open set closes, set 1 of session 2 opens"
        );
        assert_eq!(events[0].1, BlockEvent::Closed(second.id.clone()));
        let closed = group_info(&state, &second.id).await;
        assert_eq!(closed.session, 1, "closed in the session it belongs to");
        let opened = events[1].1.open().expect("open").clone();
        assert_eq!((opened.number, opened.session), (1, 2));
        // The next shot joins it.
        up.shot(3).await;
        assert!(group_events(&mut rx).is_empty());
        assert_eq!(group_of(&state, 3).await, Some(opened.id.clone()));
        assert_eq!(group_info(&state, &opened.id).await.shots, 1);
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
        // The bus message alone moves the selection; the session is not read.
        up.send(
            "system",
            Ev::Turn(flightrelay::GolferTurn::new(
                None,
                Some(Club::Driver),
                flighthook::Handedness::Right,
                Mode::Full,
            )),
        )
        .await;
        // The turn alone rolls the set: the 7-iron set has a shot.
        let events = group_events(&mut rx);
        let events = &events[events.len() - 2..];
        assert_eq!(events[0].1, BlockEvent::Closed(i7), "the 7-iron set closes");
        assert_eq!(events[1].1.open().and_then(|b| b.club), Some(Club::Driver));
        // New set on the empty driver set changes nothing.
        up.new_block(false).await;
        assert!(group_events(&mut rx).is_empty());
    }

    /// A card turned off shows Disabled, with nothing held and never red,
    /// even right after losing its connection; turned on, the new
    /// instance's statuses take over.
    #[tokio::test]
    async fn a_card_turned_off_shows_disabled_and_never_red() {
        let (state, _writer, _dir) = test_state();
        let id = "mock_monitor.0";
        let set = |enabled: bool| {
            state.root.system.update(|c| {
                c.mock_monitor.insert(
                    "0".into(),
                    flighthook::MockMonitorSection {
                        name: "Mock".into(),
                        enabled,
                        actor_id: None,
                        impact_lead_s: None,
                        driver: Default::default(),
                    },
                );
            });
        };
        let outcome = |stopped: &[&str], started: &[&str]| {
            Ev::Local(LocalEvent::ConfigOutcome {
                request_id: None,
                restarted: Vec::new(),
                stopped: stopped.iter().map(|s| (*s).to_owned()).collect(),
                started: started.iter().map(|s| (*s).to_owned()).collect(),
            })
        };
        let shown = |state: &WebState| {
            let names = actor_names(&state.root.system.snapshot());
            let actors = state.actors.try_read().expect("not locked");
            actor_statuses(&actors, &names, state.now_ms())
        };
        set(true);
        let mut up = Updater::new(&state);
        up.send("system", outcome(&[], &[id])).await;
        for status in [
            ActorStatus::Starting,
            ActorStatus::Connected,
            ActorStatus::Reconnecting,
        ] {
            up.status(id, status).await;
        }
        assert!(shown(&state)[id].red_for_ms > 0, "red after the drop");

        set(false);
        up.send("system", outcome(&[id], &[])).await;
        let off = &shown(&state)[id];
        assert_eq!(off.state, ConnectionState::Disabled);
        assert_eq!(off.red_for_ms, 0);
        assert_eq!(off.name, "Mock");
        assert!(off.telemetry.is_empty());

        set(true);
        up.status(id, ActorStatus::Starting).await;
        up.send("system", outcome(&[], &[id])).await;
        assert_eq!(shown(&state)[id].state, ConnectionState::Connecting);
    }

    /// The web server starts with the cards that are off already Disabled.
    #[test]
    fn cards_off_at_startup_are_disabled() {
        let mut names = HashMap::new();
        names.insert("mock_monitor.0".to_owned(), "Mock".to_owned());
        names.insert("mevo.0".to_owned(), "Mevo".to_owned());
        let mut actors: HashMap<String, ActorStatusResponse> = names
            .iter()
            .map(|(id, name)| (id.clone(), new_actor(name.clone())))
            .collect();
        let off = follow_disabled(&mut actors, &names, &["mock_monitor.0".to_owned()]);
        assert_eq!(off, ["mock_monitor.0"]);
        assert!(actors["mock_monitor.0"].is_disabled());
        assert!(!actors["mevo.0"].is_disabled());
        // Nothing changes when called again.
        assert!(follow_disabled(&mut actors, &names, &["mock_monitor.0".to_owned()]).is_empty());
        // On again, before any new instance spoke: a new actor.
        assert!(follow_disabled(&mut actors, &names, &[]).is_empty());
        assert_eq!(
            actors["mock_monitor.0"].state(),
            ConnectionState::Connecting
        );
    }

    #[tokio::test]
    async fn the_sets_come_from_flighthooks_practice_actor_whatever_the_webserver() {
        let (state, writer, _dir) = test_state();
        let two = web_state(Arc::clone(&state.root), state.bus_tx.clone(), "webserver.1");
        let mut rx = state.bus_tx.subscribe();
        let mut up = Updater::new(&two);
        up.club(&writer, Club::Iron7).await;
        up.shot(1).await;
        let events = group_events(&mut rx);
        assert!(!events.is_empty());
        let me = state.root.flighthook_id();
        assert!(events.iter().all(|(from, _)| *from == me), "{events:?}");
        let open = events.last().and_then(|(_, e)| e.open()).expect("open");
        assert_eq!(group_of(&two, 1).await.as_deref(), Some(open.id.as_str()));
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
        let before = group_info(&state, &second).await.shots;
        up.shot(5000).await;
        // The store counts; the practice actor's blocks are untouched.
        assert!(group_events(&mut rx).is_empty());
        assert_eq!(group_info(&state, &second).await.shots, before + 1);
        let released = state
            .blocks
            .read()
            .await
            .all()
            .iter()
            .find(|r| r.block.id == first)
            .map(Set::from)
            .expect("kept");
        assert!(!released.open);
        assert_eq!(released.shots, 0, "dropped");
        assert!(
            groups_view(&state)
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
        up.send("mevo.0", Ev::Finished(key(0), Some(Box::new(raw(1, 1)))))
            .await;
        for n in 1..u32::try_from(MAX_SHOTS).expect("fits") {
            up.shot(n).await;
        }
        let group = group_of(&state, 1).await.expect("grouped");
        let mut rx = state.bus_tx.subscribe();
        up.shot(5000).await;
        assert!(group_events(&mut rx).is_empty());
        assert_eq!(
            group_info(&state, &group).await.shots,
            u32::try_from(MAX_SHOTS).expect("fits"),
            "the join only"
        );
    }

    /// A putting device's shot is a putter shot, filed in a putter set,
    /// whatever club is in play; other devices keep the club in play.
    #[tokio::test]
    async fn an_exputt_shot_is_a_putt_whatever_club_is_in_play() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.club(&writer, Club::Iron7).await;
        writer.apply(&flightrelay::GolferSelect {
            mode: Some(flighthook::Mode::Full),
            ..flightrelay::GolferSelect::new("test")
        });
        // The ExPutt's driver arms only for putts (its card's thread holds
        // the fixed mode); the card key decides nothing.
        let _putting = state.root.set_fixed_mode("putter-actor", Mode::Putting);
        up.shot_as("square.0", key_id(1, "omni")).await;
        up.shot_as("putter-actor", key_id(1, "putt")).await;
        up.shot_as("exputt.1", key_id(2, "no-fixed-mode")).await;

        let shots = state.shots.read().await;
        let omni = &shots[0];
        let putt = &shots[1];
        assert_eq!(omni.data.club_selected(), Some(Club::Iron7));
        assert_eq!(omni.context.detection_mode, Some(Mode::Full));
        assert_eq!(putt.data.club_selected(), Some(Club::Putter));
        assert_eq!(putt.data.club_selected, Some(Club::Putter), "stamped");
        assert_eq!(putt.context.detection_mode, Some(Mode::Putting));
        assert_eq!(
            omni.data.block, putt.data.block,
            "blocks follow the turn, whatever the device"
        );
        assert_eq!(shots[2].data.club_selected(), Some(Club::Iron7));
        // `/api/shots` carries the stamped club.
        let json = serde_json::to_value(&putt.data).expect("json");
        assert_eq!(json["club_selected"], "PT");
    }

    #[tokio::test]
    async fn a_new_trigger_ends_the_devices_unfinished_shot() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.new_block(false).await;
        let set = groups_view(&state).await.current().expect("open");
        // Shot 1 never finishes; shot 2 does.
        up.trigger(1).await;
        up.shot(2).await;
        // Another device's shot in flight is not this device's to end.
        up.send("square.0", Ev::trigger(key(7))).await;
        up.trigger(3).await;
        up.send("square.0", Ev::Finished(key(7), Some(Box::new(raw(1, 1)))))
            .await;
        up.finish(3).await;
        assert_eq!(group_info(&state, &set.id).await.shots, 2);
        assert_eq!(group_of(&state, 2).await.as_deref(), Some(set.id.as_str()));
        assert!(up.pending.is_empty(), "nothing is left pending");
        assert_eq!(state.shots.read().await.len(), 3);

        // Re-triggering the same shot ends its first assignment too.
        up.new_block(false).await;
        let fresh = groups_view(&state).await.current().expect("open");
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
        up.new_block(false).await;
        let set = groups_view(&state).await.current().expect("open");
        for status in [
            ActorStatus::Reconnecting,
            ActorStatus::Disconnected,
            ActorStatus::Starting,
        ] {
            up.trigger(2).await;
            up.send(
                "mevo.0",
                Ev::Local(LocalEvent::CardStatus {
                    status,
                    telemetry: HashMap::new(),
                }),
            )
            .await;
            assert!(up.pending.is_empty(), "{status:?}");
            // Nothing was stored for the set, and the set stays open.
            let current = groups_view(&state).await.current();
            assert_eq!(
                current.map(|g| (g.id, g.shots)),
                Some((set.id.clone(), 0)),
                "{status:?}"
            );
        }
        group_events(&mut rx);
        // The trigger counted for the practice actor: the set is not empty,
        // so another club starts the next one.
        up.club(&writer, Club::Driver).await;
        up.shot(3).await;
        let set = groups_view(&state).await.current().expect("open");
        assert_eq!(group_of(&state, 3).await.as_deref(), Some(set.id.as_str()));
        let info = group_info(&state, &set.id).await;
        assert_eq!(info.club, Some(Club::Driver));
        assert_eq!(info.shots, 1);
        // A Connected status (heartbeat or telemetry) leaves a flight alone.
        up.trigger(4).await;
        up.send(
            "mevo.0",
            Ev::Local(LocalEvent::CardStatus {
                status: ActorStatus::Connected,
                telemetry: HashMap::new(),
            }),
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
        // 7I as the bus carries it before the session reads it.
        up.send(
            "system",
            Ev::Turn(flightrelay::GolferTurn::new(
                None,
                Some(Club::Iron7),
                flighthook::Handedness::Right,
                Mode::Full,
            )),
        )
        .await;
        assert_eq!(
            state.root.session.turn().club,
            Some(Club::Driver),
            "the session still lags"
        );
        up.new_block(false).await;
        let set = groups_view(&state).await.current().expect("open");
        assert_eq!(set.club, Some(Club::Iron7));
        up.shot(2).await;
        let stored = state.shots.read().await[1].data.clone();
        assert_eq!(stored.club_selected(), Some(Club::Iron7));
        assert_eq!(stored.block.as_deref(), Some(set.id.as_str()));
        let info = group_info(&state, &set.id).await;
        assert_eq!(info.club, Some(Club::Iron7), "not re-keyed to DR");
        assert_eq!(info.shots, 1);
    }

    /// The UI gets the store's shot before its `shot.finished`, so a row
    /// with data never reads as no read for a moment.
    #[tokio::test]
    async fn the_ui_gets_the_shot_before_its_finish() {
        let (state, _writer, _dir) = test_state();
        let mut ui = state.ui_out.subscribe();
        let mut up = Updater::new(&state);
        up.shot(1).await;
        let mut order = Vec::new();
        while let Ok(msg) = ui.try_recv() {
            match &*msg {
                flighthook::UiMessage::Shot(_) => order.push("shot"),
                flighthook::UiMessage::Frp(m) if m.body.kind() == "shot.finished" => {
                    order.push("finished");
                }
                _ => {}
            }
        }
        assert_eq!(order, ["shot", "finished"]);
    }

    #[tokio::test]
    async fn dropped_trigger_finishes_its_assignment_uncounted() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        // No data and no raw: nothing is stored.
        up.trigger(1).await;
        up.send("mevo.0", Ev::Finished(key(1), None)).await;
        assert!(state.shots.read().await.is_empty());
        let current = groups_view(&state).await.current().expect("open");
        assert_eq!(current.shots, 0, "nothing stored for it");
        // A no-read gives its count back at its finish, so the set is
        // still empty: a different club takes it over in place, and the
        // first set shown stays Set 1.
        up.club(&writer, Club::Driver).await;
        up.shot(2).await;
        let next = group_of(&state, 2).await.expect("grouped");
        assert_eq!(next, current.id);
        let info = group_info(&state, &next).await;
        assert_eq!((info.club, info.number), (Some(Club::Driver), 1));
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
            .block
            .clone()
    }

    async fn selected_club(state: &WebState) -> Option<Club> {
        state.root.session.turn().club
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
        let set = groups_view(&state).await.current().expect("open");
        assert_eq!(set.shots, 3);
        group_events(&mut rx);

        up.reconnect("mevo.0").await;
        assert!(
            group_events(&mut rx).is_empty(),
            "no set is closed, rotated or created"
        );
        assert_eq!(selected_club(&state).await, Some(Club::Iron7));
        let current = groups_view(&state).await.current();
        assert_eq!(current.map(|g| g.id), Some(set.id.clone()));

        up.shot(4).await;
        assert_eq!(group_of(&state, 4).await.as_deref(), Some(set.id.as_str()));
        let info = group_info(&state, &set.id).await;
        assert_eq!(info.shots, 4);
        assert!(info.open);
        assert_eq!(groups_view(&state).await.list().len(), 1);
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
        let set = groups_view(&state).await.current().expect("open");
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
        assert_eq!(groups_view(&state).await.list().len(), 1);
        assert_eq!(selected_club(&state).await, Some(Club::Iron7));

        // A set opened by the dropped shot's own trigger stays open, and the
        // next shot joins it.
        up.new_block(true).await;
        up.trigger(5).await;
        let opened = groups_view(&state).await.current().expect("open");
        assert_ne!(opened.id, set.id);
        up.reconnect("mevo.0").await;
        let current = groups_view(&state).await.current();
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
            assert_eq!(groups_view(&state).await.list().len(), 2);
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
        // Each shot joined by its own id.
        let joined: Vec<String> = state
            .shots
            .read()
            .await
            .iter()
            .filter(|s| s.data.block.as_deref() == Some(second.id.as_str()))
            .map(|s| s.key.shot_id.clone())
            .collect();
        assert_eq!(joined, ["b2", "b3"]);
        let _ = group_events(&mut rx);
        assert!(up.pending.is_empty());
    }

    #[tokio::test]
    async fn one_device_dropping_mid_shot_leaves_anothers_shot_alone() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.grouping(ShotGrouping::BySet).await;
        up.club(&writer, Club::Iron7).await;
        up.shot_as("mevo.0", key_id(1, "a1")).await;
        let set = groups_view(&state).await.current().expect("open");
        // Both in flight; A drops before its finish, B finishes.
        up.trigger_as("mevo.0", key_id(2, "a2")).await;
        up.trigger_as("square.0", key_id(1, "b1")).await;
        up.status("mevo.0", ActorStatus::Disconnected).await;
        let left: Vec<&str> = up.pending.keys().map(String::as_str).collect();
        assert_eq!(left, ["b1"], "only A's shot is dropped");
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

    // -- Remote actors gone ---------------------------------------------------

    /// A message from a remote actor, as it arrives on a hub link.
    fn from_peer(from: &str, body: impl Into<FrpBody>) -> BusMsg {
        BusMsg::from_peer(
            flightrelay::FrpMessage::new(from, body),
            crate::bus::PeerId(42),
        )
    }

    /// Run the system actor's remote tracker over `msgs` at `heard`, then
    /// look for gone actors at `now`; the `ActorGone` events it sends.
    fn track(root: &SystemState, msgs: &[BusMsg], heard: Instant, now: Instant) -> Vec<BusMsg> {
        let (tx, mut rx) = broadcast::channel(16);
        let sender = BusSender::new("system".into(), tx, Arc::default())
            .with_frp_id(Some(root.flighthook_id()));
        let mut remotes = crate::state::remotes::Remotes::new();
        for m in msgs {
            if let BusMsg::Frp {
                msg, peer: Some(_), ..
            } = m
            {
                remotes.hear(msg, heard);
            }
            let _ = crate::actors::system::follow_remote(root, &mut remotes, m);
        }
        crate::actors::system::expire_remotes(root, &mut remotes, &sender, now, now);
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// The timers fall more than 6 s behind (an injected clock): no
    /// local actor expires, only the silent remote one. flighthook's turn
    /// and Set stay, and the next shot lands in Set 1 with its turn.
    #[tokio::test]
    async fn a_stall_past_the_expiry_keeps_the_local_turn_and_set() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        let mut seen = state.bus_tx.subscribe();
        up.club(&writer, Club::Iron7).await;
        up.send(
            "mevo.0",
            Ev::device(flightrelay::DeviceInfo::default(), &[]),
        )
        .await;
        let lm = flightrelay::BusActor::new("Trackball", 2.0)
            .with_role(flightrelay::types::roles::LAUNCH_MONITOR);
        up.bus(&from_peer("aaaa000000000001", lm)).await;
        let mut msgs: Vec<BusMsg> = std::iter::from_fn(|| seen.try_recv().ok()).collect();
        msgs.push(from_peer(
            "aaaa000000000001",
            flightrelay::BusActor::new("Trackball", 2.0),
        ));
        let t0 = Instant::now();
        let gone = track(
            &state.root,
            &msgs,
            t0,
            t0 + std::time::Duration::from_secs(8),
        );
        let gone_ids: Vec<&str> = gone
            .iter()
            .filter_map(|m| match m.as_local() {
                Some((_, LocalEvent::ActorGone { actor })) => Some(actor.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(gone_ids, ["aaaa000000000001"], "local actors never expire");
        for m in &gone {
            up.bus(m).await;
        }
        let own = state.root.flighthook_id();
        {
            let bus = state.bus_state.lock().unwrap();
            assert!(bus.actor("aaaa000000000001").is_none());
            assert!(
                bus.actor("mevo.0").is_some(),
                "the local launch monitor stays"
            );
            assert!(
                bus.of_kind(flightrelay::StateKind::GolferTurn)
                    .any(|m| m.from == own)
            );
            assert!(
                bus.of_kind(flightrelay::StateKind::PracticeBlock)
                    .any(|m| m.from == own)
            );
        }
        up.shot(1).await;
        let current = groups_view(&state).await.current().expect("open");
        let shots = state.shots.read().await.clone();
        assert_eq!(shots[0].data.block.as_deref(), Some(current.id.as_str()));
        assert_eq!(current.number, 1);
        assert_eq!(shots[0].data.club_selected(), Some(Club::Iron7));
    }

    /// A remote launch monitor dies mid-shot (trigger, then silence):
    /// once it is gone the count goes back, so the next club's Set is still
    /// Set 1.
    #[tokio::test]
    async fn the_first_set_stays_set_1_after_a_remote_monitor_died_mid_shot() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.club(&writer, Club::Iron7).await;
        let lm = flightrelay::BusActor::new("Trackball", 2.0)
            .with_role(flightrelay::types::roles::LAUNCH_MONITOR);
        let msgs = [
            from_peer("aaaa000000000001", lm),
            from_peer("aaaa000000000001", flightrelay::ShotTrigger::new(key(1))),
        ];
        for m in &msgs {
            up.bus(m).await;
        }
        let t0 = Instant::now();
        for m in track(
            &state.root,
            &msgs,
            t0,
            t0 + std::time::Duration::from_secs(7),
        ) {
            up.bus(&m).await;
        }
        up.club(&writer, Club::Driver).await;
        let view = groups_view(&state).await;
        let list = view.list();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!((list[0].number, list[0].club), (1, Some(Club::Driver)));
    }

    /// The club changes while the remote
    /// monitor's shot is in flight, then the monitor dies. The new Set is
    /// still Set 1.
    #[tokio::test]
    async fn a_club_change_before_a_remote_monitor_dies_leaves_set_1() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.club(&writer, Club::Iron7).await;
        let lm = flightrelay::BusActor::new("Trackball", 2.0)
            .with_role(flightrelay::types::roles::LAUNCH_MONITOR);
        let msgs = [
            from_peer("aaaa000000000001", lm),
            from_peer("aaaa000000000001", flightrelay::ShotTrigger::new(key(1))),
        ];
        for m in &msgs {
            up.bus(m).await;
        }
        up.club(&writer, Club::Driver).await;
        let t0 = Instant::now();
        for m in track(
            &state.root,
            &msgs,
            t0,
            t0 + std::time::Duration::from_secs(7),
        ) {
            up.bus(&m).await;
        }
        let view = groups_view(&state).await;
        let list = view.list();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!((list[0].number, list[0].club), (1, Some(Club::Driver)));
    }

    /// A game id linked to Eric while it is up with an open Set: the store
    /// files later shots in the Sets the club changes open, never in a stale
    /// one, and every Set's count is its own shots.
    #[tokio::test]
    async fn after_a_link_shots_land_in_the_new_sets() {
        let (state, writer, _dir) = test_state();
        state.root.system.update(|c| {
            c.golfers = vec![
                flighthook::GolferRecord::new("flighthook.e", "Eric"),
                flighthook::GolferRecord::new("flighthook.s", "Sam"),
            ];
        });
        let mut up = Updater::new(&state);
        up.pick(&writer, "flighthook.e", Club::Driver).await;
        up.shot(1).await;
        up.pick(&writer, "pureplay.local-3", Club::Iron7).await;
        up.shot(2).await;
        up.shot(3).await;
        state.root.system.update(|c| {
            c.golfers[0].identifiers.push("pureplay.local-3".into());
        });
        up.golfers_saved().await;
        up.club(&writer, Club::Iron8).await;
        for n in 4..10 {
            up.shot(n).await;
        }
        up.club(&writer, Club::Iron9).await;
        for n in 10..14 {
            up.shot(n).await;
        }
        let view = groups_view(&state).await;
        let sets: Vec<(u32, Option<Club>, bool, u32)> = view
            .list()
            .iter()
            .map(|s| (s.number, s.club, s.open, s.shots))
            .collect();
        assert_eq!(
            sets,
            vec![
                (1, Some(Club::Driver), false, 1),
                (1, Some(Club::Iron7), false, 2),
                (2, Some(Club::Iron8), false, 6),
                (3, Some(Club::Iron9), true, 4),
            ]
        );
    }

    /// A game golfer's name is stored on its shot while the profile is on
    /// the bus, so the shot keeps its name after the round: a later list of
    /// shots (a UI that connects then) carries it.
    #[tokio::test]
    async fn a_game_golfers_name_stays_on_its_shot_after_the_round() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        let id: flightrelay::GolferId = "pureplay.player-1".parse().expect("id");
        up.bus(&BusMsg::frp(flightrelay::FrpMessage::new(
            "pureplay.0",
            flightrelay::GolferProfile::new(id.clone(), "Joe"),
        )))
        .await;
        up.pick(&writer, "pureplay.player-1", Club::Iron7).await;
        up.shot(1).await;
        // The round ends: the profile goes.
        up.bus(&BusMsg::frp(flightrelay::FrpMessage::new(
            "pureplay.0",
            flightrelay::Removal::of_id(flightrelay::StateKind::GolferProfile, id.as_str()),
        )))
        .await;
        up.shot(2).await;
        let shots = state.shots.read().await.clone();
        assert_eq!(shots[0].data.golfer_name.as_deref(), Some("Joe"));
        assert_eq!(shots[1].data.golfer_name, None, "no profile, no name");
        let json = serde_json::to_string(&shots[0].data).expect("json");
        assert!(json.contains(r#""golfer_name":"Joe""#), "{json}");
        // A flighthook golfer is named by its record, not stored.
        up.pick(&writer, "flighthook.sam", Club::Iron7).await;
        up.shot(3).await;
        assert_eq!(state.shots.read().await[2].data.golfer_name, None);
    }

    /// Another actor's `shot.launch` for a local shot is dropped by the
    /// store, before and after the shot is stored; a flight is kept.
    #[tokio::test]
    async fn the_store_takes_a_shots_parts_from_its_launch_monitor_only() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.club(&writer, Club::Iron7).await;
        let fast = Launch {
            launch_speed: Some(Velocity::MetersPerSecond(99.0)),
            ..Launch::default()
        };
        up.trigger_as("mevo.0", key(1)).await;
        up.bus(&from_peer(
            "intruder",
            flightrelay::ShotLaunch {
                key: key(1),
                launch: fast.clone(),
            },
        ))
        .await;
        up.finish_as("mevo.0", key(1)).await;
        up.bus(&from_peer(
            "intruder",
            flightrelay::ShotLaunch {
                key: key(1),
                launch: fast,
            },
        ))
        .await;
        let shots = state.shots.read().await.clone();
        let frp = shots[0].frp.as_ref().expect("shot");
        assert_eq!(
            frp.launch.as_ref().and_then(|l| l.launch_speed),
            Some(Velocity::MetersPerSecond(40.0))
        );
    }

    /// A remote launch monitor's shot is exported with device kind
    /// `frp`, never its actor id.
    #[tokio::test]
    async fn a_remote_shot_exports_device_kind_frp() {
        let (state, writer, _dir) = test_state();
        let mut up = Updater::new(&state);
        up.club(&writer, Club::Iron7).await;
        let remote = "aaaa000000000001";
        let lm = from_peer(
            remote,
            flightrelay::BusActor::new("Trackball", 2.0)
                .with_role(flightrelay::types::roles::LAUNCH_MONITOR),
        );
        super::ui_socket::follow(&state, &lm);
        up.bus(&lm).await;
        up.bus(&from_peer(remote, flightrelay::ShotTrigger::new(key(1))))
            .await;
        up.bus(&from_peer(
            remote,
            flightrelay::ShotLaunch {
                key: key(1),
                launch: ball(),
            },
        ))
        .await;
        up.bus(&from_peer(
            remote,
            flightrelay::ShotFinished { key: key(1) },
        ))
        .await;
        let stored = state.shots.read().await[0].clone();
        let line: serde_json::Value = serde_json::from_str(&export::jsonl_line(
            &stored,
            "t",
            None,
            false,
            &flighthook::FlighthookConfig::default(),
        ))
        .expect("json");
        assert_eq!(line["device"]["kind"], "frp", "{line}");
        // The remote actor's name is kept on the shot for the UI, never in
        // the export.
        assert_eq!(stored.data.from_name.as_deref(), Some("Trackball"));
        assert!(!line.to_string().contains("Trackball"), "{line}");
        // A local card's shot carries no name.
        up.shot(2).await;
        let local = state.shots.read().await[1].clone();
        assert_eq!(local.data.from_name, None);
    }
}
