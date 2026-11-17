//! Axum web server — REST endpoints + WebSocket event streaming.

pub mod blocks;
pub mod export;
pub mod routes;
mod store;
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
    SHOT_TIMEOUT, ShotData, ShotKey, ShotRaw, ShotVideo, ShotVideos as _, SimulationSection,
};
use flightrelay::{CompletedShot, FrpBody, ShotAccumulator, ShotEvent};
pub use store::*;

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

#[cfg(test)]
pub(crate) mod tests;
