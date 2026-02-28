//! System actor — default actor that always runs and handles internal
//! housekeeping for updating `SystemState`.
//!
//! Subscribes to the bus and processes `GameStateCommand` events to keep
//! `GameState` (player info, club selection, detection mode) in sync.
//! Also processes `ConfigCommand` events for config mutations (from the
//! REST API). This runs independently of the web server, so `SystemState`
//! is always consistent even in headless mode.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::broadcast;

use crate::actors::{Actor, ReconfigureOutcome, ResolvedActor, resolve_actors, start_actor};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::{GameStateWriter, SystemState};
use flighthook::{FlighthookEvent, FlighthookMessage, GameStateCommandEvent};

// ---------------------------------------------------------------------------
// Config reload
// ---------------------------------------------------------------------------

/// Result of applying a config reload.
pub(crate) struct ConfigReloadOutcome {
    pub applied: Vec<String>,
    pub restarted: Vec<String>,
    pub stopped: Vec<String>,
    pub started: Vec<String>,
}

/// Compare the current actor set against the new config and apply changes.
///
/// - Deleted actors (current but not expected): shutdown + remove
/// - Existing actors: call `reconfigure()` — restart if needed
/// - New actors: create and start
///
/// When `scope` is `Some(actor_id)`, only that actor is reconfigured.
/// Other actors are left untouched (no stop/start/reconfigure).
pub(crate) fn apply_config_reload(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    scope: Option<&str>,
) -> ConfigReloadOutcome {
    let snap = state.system.snapshot();
    let resolved = resolve_actors(&snap);

    // Pre-build into a map so we can pull actors out for restart/new cases
    let mut resolved_map: HashMap<String, ResolvedActor> =
        resolved.into_iter().map(|ra| (ra.id.clone(), ra)).collect();

    let current_ids = state.actor_ids();
    let expected_ids: Vec<String> = resolved_map.keys().cloned().collect();

    let mut result = ConfigReloadOutcome {
        applied: Vec::new(),
        restarted: Vec::new(),
        stopped: Vec::new(),
        started: Vec::new(),
    };

    if let Some(target) = scope {
        // Scoped reload: only reconfigure the target actor
        if current_ids.contains(&target.to_string()) && expected_ids.contains(&target.to_string()) {
            let shutdown = Arc::new(AtomicBool::new(false));
            let sender = BusSender::new(target.to_string(), bus_tx.clone(), shutdown);
            let reconf = state.reconfigure_actor(target, state, &sender);

            match reconf {
                Some(ReconfigureOutcome::Applied) => {
                    tracing::info!("config reload (scoped): applied in-place for '{target}'");
                    result.applied.push(target.to_string());
                }
                Some(ReconfigureOutcome::RestartRequired) => {
                    tracing::info!("config reload (scoped): restarting '{target}'");
                    state.stop_actor(target);
                    state.remove_actor(target);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if let Some(ra) = resolved_map.remove(target) {
                        start_actor(ra.id, ra.actor, state, bus_tx);
                        result.restarted.push(target.to_string());
                    }
                }
                Some(ReconfigureOutcome::NoChange) | None => {}
            }
        } else if !current_ids.contains(&target.to_string())
            && expected_ids.contains(&target.to_string())
        {
            // New actor
            tracing::info!("config reload (scoped): starting new actor '{target}'");
            if let Some(ra) = resolved_map.remove(target) {
                start_actor(ra.id, ra.actor, state, bus_tx);
                result.started.push(target.to_string());
            }
        } else if current_ids.contains(&target.to_string())
            && !expected_ids.contains(&target.to_string())
        {
            // Removed actor
            tracing::info!("config reload (scoped): stopping removed actor '{target}'");
            state.stop_actor(target);
            state.remove_actor(target);
            result.stopped.push(target.to_string());
        }
    } else {
        // Full reload: process all actors

        // 1. Deleted actors — in current but not in expected
        //    (skip "system" — it's always-on, not config-driven)
        for id in &current_ids {
            if id == "system" || expected_ids.contains(id) {
                continue;
            }
            tracing::info!("config reload: stopping removed actor '{id}'");
            state.stop_actor(id);
            state.remove_actor(id);
            result.stopped.push(id.clone());
        }

        // 2. Existing actors — in both current and expected
        for id in &current_ids {
            if id == "system" || !expected_ids.contains(id) || result.stopped.contains(id) {
                continue;
            }

            let shutdown = Arc::new(AtomicBool::new(false));
            let sender = BusSender::new(id.clone(), bus_tx.clone(), shutdown);
            let reconf = state.reconfigure_actor(id, state, &sender);

            match reconf {
                Some(ReconfigureOutcome::Applied) => {
                    tracing::info!("config reload: applied in-place for '{id}'");
                    result.applied.push(id.clone());
                }
                Some(ReconfigureOutcome::RestartRequired) => {
                    tracing::info!("config reload: restarting '{id}'");
                    state.stop_actor(id);
                    state.remove_actor(id);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if let Some(ra) = resolved_map.remove(id.as_str()) {
                        start_actor(ra.id, ra.actor, state, bus_tx);
                        result.restarted.push(id.clone());
                    }
                }
                Some(ReconfigureOutcome::NoChange) | None => {}
            }
        }

        // 3. New actors — in expected but not in current
        for id in &expected_ids {
            if current_ids.contains(id) {
                continue;
            }
            tracing::info!("config reload: starting new actor '{id}'");
            if let Some(ra) = resolved_map.remove(id.as_str()) {
                start_actor(ra.id, ra.actor, state, bus_tx);
                result.started.push(id.clone());
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// System actor
// ---------------------------------------------------------------------------

/// System actor. Always-on internal housekeeping — not config-driven.
///
/// Holds the sole `GameStateWriter`, enforcing that all game state mutations
/// flow through bus events processed here. Also processes `ConfigCommand`
/// events for config mutations.
pub struct SystemActor {
    writer: Mutex<Option<GameStateWriter>>,
    state: Mutex<Option<Arc<SystemState>>>,
    bus_tx: Mutex<Option<broadcast::Sender<FlighthookMessage>>>,
    ready_tx: Mutex<Option<std_mpsc::SyncSender<()>>>,
}

impl SystemActor {
    pub fn new(
        writer: GameStateWriter,
        state: Arc<SystemState>,
        bus_tx: broadcast::Sender<FlighthookMessage>,
    ) -> (Self, std_mpsc::Receiver<()>) {
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(0);
        let actor = Self {
            writer: Mutex::new(Some(writer)),
            state: Mutex::new(Some(state)),
            bus_tx: Mutex::new(Some(bus_tx)),
            ready_tx: Mutex::new(Some(ready_tx)),
        };
        (actor, ready_rx)
    }
}

impl Actor for SystemActor {
    fn start(&self, _state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver) {
        let writer = self
            .writer
            .lock()
            .unwrap()
            .take()
            .expect("SystemActor::start() called more than once");
        let sys_state = self
            .state
            .lock()
            .unwrap()
            .take()
            .expect("SystemActor::start() called more than once");
        let bus_tx = self
            .bus_tx
            .lock()
            .unwrap()
            .take()
            .expect("SystemActor::start() called more than once");
        let ready_tx = self
            .ready_tx
            .lock()
            .unwrap()
            .take()
            .expect("SystemActor::start() called more than once");

        std::thread::Builder::new()
            .name("system".into())
            .spawn(move || run(writer, sys_state, bus_tx, sender, receiver, ready_tx))
            .expect("failed to spawn system thread");
    }
}

fn run(
    writer: GameStateWriter,
    state: Arc<SystemState>,
    bus_tx: broadcast::Sender<FlighthookMessage>,
    sender: BusSender,
    mut receiver: BusReceiver,
    ready_tx: std_mpsc::SyncSender<()>,
) {
    // Signal main thread that we're up and polling.
    let _ = ready_tx.send(());
    drop(ready_tx);

    loop {
        match receiver.poll() {
            Err(PollError::Shutdown) => return,
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(Some(msg)) => match &msg.event {
                FlighthookEvent::GameStateCommand(cmd) => match &cmd.event {
                    GameStateCommandEvent::SetPlayerInfo { player_info } => {
                        writer.set_player_info(player_info.clone());
                    }
                    GameStateCommandEvent::SetClubInfo { club_info } => {
                        writer.set_club_info(club_info.clone());
                        // Auto-derive detection mode from club selection
                        let mode = club_info.club.mode();
                        writer.set_mode(mode);
                        sender.send(FlighthookMessage::new(GameStateCommandEvent::SetMode {
                            mode,
                        }));
                    }
                    GameStateCommandEvent::SetMode { mode } => {
                        writer.set_mode(*mode);
                    }
                },
                FlighthookEvent::ConfigCommand(cmd) => {
                    handle_config_command(cmd, &state, &bus_tx, &sender);
                }
                _ => {}
            },
        }
    }
}

fn handle_config_command(
    cmd: &flighthook::ConfigCommand,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    sender: &BusSender,
) {
    use flighthook::{ConfigAction, ConfigOutcome};

    // Determine scope for actor reconciliation
    let scope: Option<String>;

    match &cmd.action {
        ConfigAction::ReplaceAll { config } => {
            state.system.replace(config.clone());
            scope = None;
        }
        ConfigAction::UpsertWebserver { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.webserver.insert(idx, section.clone());
            });
            scope = Some(format!("webserver.{index}"));
        }
        ConfigAction::UpsertMevo { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.mevo.insert(idx, section.clone());
            });
            scope = Some(format!("mevo.{index}"));
        }
        ConfigAction::UpsertGsPro { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.gspro.insert(idx, section.clone());
            });
            scope = Some(format!("gspro.{index}"));
        }
        ConfigAction::UpsertMockMonitor { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.mock_monitor.insert(idx, section.clone());
            });
            scope = Some(format!("mock_monitor.{index}"));
        }
        ConfigAction::UpsertRandomClub { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.random_club.insert(idx, section.clone());
            });
            scope = Some(format!("random_club.{index}"));
        }
        ConfigAction::Remove { id } => {
            if let Some((prefix, index)) = id.split_once('.') {
                let idx = index.to_string();
                state.system.update(|p| match prefix {
                    "mevo" => {
                        p.mevo.remove(&idx);
                    }
                    "gspro" => {
                        p.gspro.remove(&idx);
                    }
                    "mock_monitor" => {
                        p.mock_monitor.remove(&idx);
                    }
                    "random_club" => {
                        p.random_club.remove(&idx);
                    }
                    "webserver" => {
                        p.webserver.remove(&idx);
                    }
                    _ => tracing::warn!("config remove: unknown prefix '{prefix}'"),
                });
                scope = Some(id.clone());
            } else {
                tracing::warn!("config remove: invalid id '{id}'");
                return;
            }
        }
    }

    // Reconcile actors (webserver included — its reconfigure() handles bind changes)
    let result = apply_config_reload(state, bus_tx, scope.as_deref());

    // Emit result if request_id is present
    if let Some(request_id) = &cmd.request_id {
        sender.send(FlighthookMessage::new(ConfigOutcome {
            request_id: request_id.clone(),
            restarted: result.restarted,
            stopped: result.stopped,
            started: result.started,
        }));
    }
}
