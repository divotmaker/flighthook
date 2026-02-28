//! Axum web server — REST endpoints + WebSocket event streaming.

pub mod routes;
pub mod types;
pub mod ws;

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::{get, post};
use tokio::sync::{RwLock, broadcast};
use tower_http::cors::CorsLayer;

use crate::actors::{Actor, ReconfigureOutcome, actor_names};
use crate::bus::{BusReceiver, BusSender};
use crate::state::SystemState;
use flighthook::{
    ActorState, ActorStatus, ActorStatusResponse, FlighthookEvent, FlighthookMessage,
    GameStateCommandEvent, LaunchMonitorEvent, ShotData,
};

const MAX_SHOTS: usize = 1000;

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
    pub shots: RwLock<VecDeque<ShotData>>,
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
    let _ = bus_tx
        .send(FlighthookMessage::new(ActorState::new(status, telemetry)).source(&state.actor_id));
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
            Some(new_addr) if new_addr == self.addr => ReconfigureOutcome::NoChange,
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
        .route("/api/shots/convert", post(routes::post_convert_shot))
        .route("/api/mode", post(routes::post_mode))
        .route(
            "/api/settings",
            get(routes::get_settings).post(routes::post_settings),
        )
        .route("/api/ws", get(ws::ws_upgrade))
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
    loop {
        match bus_rx.recv().await {
            Ok(msg) => apply_bus_event(&state, &msg).await,
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("web state updater: lagged, dropped {n} events");
            }
        }
    }
}

async fn apply_bus_event(state: &WebState, msg: &FlighthookMessage) {
    match &msg.event {
        FlighthookEvent::ActorStatus(update) => {
            let mut actors = state.actors.write().await;
            let actor = actors
                .entry(msg.source.clone())
                .or_insert_with(|| new_actor(String::new()));
            actor.status = update.status;
            actor.telemetry = update.telemetry.clone();
        }
        FlighthookEvent::LaunchMonitor(recv) => match &recv.event {
            LaunchMonitorEvent::ShotResult { shot } => {
                let mut shots = state.shots.write().await;
                if shots.len() >= MAX_SHOTS {
                    shots.pop_front();
                }
                shots.push_back(shot.as_ref().clone());
            }
            LaunchMonitorEvent::ReadyState { .. } => {}
        },
        FlighthookEvent::GameStateCommand(cmd) => {
            match &cmd.event {
                GameStateCommandEvent::SetPlayerInfo { player_info } => {
                    // GameState update handled by SystemActor; cache for UI
                    let mut actors = state.actors.write().await;
                    if let Some(actor) = actors.get_mut(&msg.source) {
                        actor
                            .telemetry
                            .insert("handed".into(), player_info.handed.clone());
                    }
                }
                GameStateCommandEvent::SetClubInfo { club_info } => {
                    // GameState update handled by SystemActor; cache for UI
                    let mut actors = state.actors.write().await;
                    if let Some(actor) = actors.get_mut(&msg.source) {
                        actor
                            .telemetry
                            .insert("club".into(), club_info.club.to_string());
                    }
                }
                GameStateCommandEvent::SetMode { .. } => {
                    // GameState update handled by SystemActor
                }
            }
        }
        FlighthookEvent::ConfigOutcome(result) => {
            if !result.started.is_empty()
                || !result.stopped.is_empty()
                || !result.restarted.is_empty()
            {
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
                // Remove stopped actors from cache
                for id in &result.stopped {
                    actors.remove(id);
                }
            }
        }
        // Alert, GameStateSnapshot, UserData, ConfigCommand — no web state update needed
        _ => {}
    }
}
