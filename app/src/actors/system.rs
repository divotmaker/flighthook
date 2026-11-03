//! System actor — default actor that always runs and handles internal
//! housekeeping for updating `SystemState`.
//!
//! Subscribes to the bus and processes game state events (PlayerInfo,
//! ClubInfo, HandInfo, SetDetectionMode) to keep `GameState` in sync.
//! The club and the hand are set the same way by every source (a simulator,
//! the UI, the REST API, a WebSocket client): `club_info` and `hand_info`.
//! The system actor stores them and projects them onto the FRP controller
//! command the device actors follow: a club becomes
//! `SetDetectionMode { mode }` (the mode derived from it), and a hand that
//! differs from the stored one becomes `SetDetectionMode { handed }`. From
//! `SetDetectionMode` itself only the mode is stored, so the system actor's
//! own commands change nothing when they come back round.
//!
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
use flighthook::{FlighthookEvent, FlighthookMessage};

// ---------------------------------------------------------------------------
// Config reload
// ---------------------------------------------------------------------------

/// Result of applying a config reload.
pub(crate) struct ConfigReloadOutcome {
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
    let current_mode = state.game.snapshot().mode;
    let resolved = resolve_actors(&snap, current_mode);

    // Pre-build into a map so we can pull actors out for restart/new cases
    let mut resolved_map: HashMap<String, ResolvedActor> =
        resolved.into_iter().map(|ra| (ra.id.clone(), ra)).collect();

    let current_ids = state.actor_ids();
    let expected_ids: Vec<String> = resolved_map.keys().cloned().collect();

    let mut result = ConfigReloadOutcome {
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
                Some(ReconfigureOutcome::Applied) | None => {}
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
                Some(ReconfigureOutcome::Applied) | None => {}
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
            Ok(Some(msg)) => {
                if let FlighthookEvent::ConfigCommand { .. } = &msg.event {
                    handle_config_command(&msg.event, &state, &bus_tx, &sender);
                } else {
                    handle_game_event(&msg.event, &writer, &state, &sender);
                }
            }
        }
    }
}

/// Apply a game state event: store it, and emit the controller command a
/// club or hand change implies.
pub(crate) fn handle_game_event(
    event: &FlighthookEvent,
    writer: &GameStateWriter,
    state: &SystemState,
    sender: &BusSender,
) {
    match event {
        FlighthookEvent::PlayerInfo { player_info } => {
            writer.set_player_info(player_info.clone());
        }
        FlighthookEvent::ClubInfo { club_info } => {
            writer.set_club_info(*club_info);
            // Auto-derive detection mode from club selection
            let mode = state.system.snapshot().club_mode(club_info.club);
            writer.set_mode(mode);
            sender.send(FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
                mode: Some(mode),
                handed: None,
            }));
        }
        FlighthookEvent::HandInfo { hand_info } => {
            let handed = hand_info.handed;
            // The devices are told only when the hand changes: a source
            // that repeats the same hand does not re-command them.
            let changed = state.game.snapshot().handed != Some(handed);
            writer.set_handed(handed);
            if changed {
                sender.send(FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
                    mode: None,
                    handed: Some(handed),
                }));
            }
        }
        // The hand on this command is the projection of `hand_info` for the
        // devices, not a source of game state.
        FlighthookEvent::SetDetectionMode { mode: Some(m), .. } => {
            writer.set_mode(*m);
        }
        _ => {}
    }
}

pub(crate) fn handle_config_command(
    event: &FlighthookEvent,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    sender: &BusSender,
) {
    use flighthook::ConfigAction;

    let FlighthookEvent::ConfigCommand {
        ref request_id,
        ref action,
    } = *event
    else {
        return;
    };

    // Determine scope for actor reconciliation
    let scope: Option<String>;

    match action.as_ref() {
        ConfigAction::ReplaceAll { config } => {
            state.system.replace((**config).clone());
            scope = None;
        }
        ConfigAction::ReplaceGlobal { config } => {
            state.system.update(|p| {
                p.default_units = config.default_units;
                p.chipping_clubs.clone_from(&config.chipping_clubs);
                p.putting_clubs.clone_from(&config.putting_clubs);
                p.shot_grouping = config.shot_grouping;
                p.simulation = config.simulation.clone();
            });
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
        ConfigAction::UpsertR10 { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.r10.insert(idx, section.clone());
            });
            scope = Some(format!("r10.{index}"));
        }
        ConfigAction::UpsertSquare { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.square.insert(idx, section.clone());
            });
            scope = Some(format!("square.{index}"));
        }
        ConfigAction::UpsertExPutt { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.exputt.insert(idx, section.clone());
            });
            scope = Some(format!("exputt.{index}"));
        }
        ConfigAction::UpsertOpenConnectServer { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.openconnect_server.insert(idx, section.clone());
            });
            scope = Some(format!("openconnect_server.{index}"));
        }
        ConfigAction::UpsertOpenConnectClient { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.openconnect_client.insert(idx, section.clone());
            });
            scope = Some(format!("openconnect_client.{index}"));
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
            let id = crate::state::config::canonical_id(id);
            if let Some((prefix, index)) = id.split_once('.') {
                let idx = index.to_string();
                state.system.update(|p| match prefix {
                    "mevo" => {
                        p.mevo.remove(&idx);
                    }
                    "r10" => {
                        p.r10.remove(&idx);
                    }
                    "square" => {
                        p.square.remove(&idx);
                    }
                    "exputt" => {
                        p.exputt.remove(&idx);
                    }
                    "openconnect_server" => {
                        p.openconnect_server.remove(&idx);
                    }
                    "openconnect_client" => {
                        p.openconnect_client.remove(&idx);
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
        ConfigAction::SetShotGrouping { mode } => {
            // No actor reads the default view: persist it and reconcile
            // nothing.
            let mode = *mode;
            state.system.update(|p| p.shot_grouping = mode);
            sender.send(FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
                request_id: request_id.clone(),
                restarted: Vec::new(),
                stopped: Vec::new(),
                started: Vec::new(),
            }));
            return;
        }
    }

    // Reconcile actors (webserver included — its reconfigure() handles bind changes)
    let result = apply_config_reload(state, bus_tx, scope.as_deref());

    sender.send(FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
        request_id: request_id.clone(),
        restarted: result.restarted,
        stopped: result.stopped,
        started: result.started,
    }));
}

/// Start a system actor on `bus_tx` holding `writer`, as `main` does, and
/// wait until it polls the bus. It stops when `shutdown` is set.
#[cfg(test)]
pub(crate) fn start_for_test(
    writer: GameStateWriter,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    shutdown: Arc<AtomicBool>,
) {
    let sender = BusSender::new("system".into(), bus_tx.clone(), shutdown);
    let receiver = sender.subscribe();
    let (actor, ready) = SystemActor::new(writer, Arc::clone(state), bus_tx.clone());
    actor.start(Arc::clone(state), sender, receiver);
    ready.recv().expect("system actor up");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::web::tests::TempDir;
    use flighthook::{
        Club, ClubInfo, ConfigAction, HandInfo, Handedness, ShotDetectionMode, ShotGrouping,
    };

    /// Game state, its writer and the system actor's sender, with a
    /// receiver for what the actor emits.
    fn game() -> (
        TempDir,
        SystemState,
        GameStateWriter,
        BusSender,
        broadcast::Receiver<FlighthookMessage>,
    ) {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-system-test-{}",
            crate::state::config::generate_id()
        )));
        let (state, writer) = SystemState::new(dir.0.join("config.toml"));
        let (bus_tx, rx) = broadcast::channel(16);
        let sender = BusSender::new("system".into(), bus_tx, Arc::default());
        (dir, state, writer, sender, rx)
    }

    fn hand_info(handed: Handedness) -> FlighthookEvent {
        FlighthookEvent::HandInfo {
            hand_info: HandInfo { handed },
        }
    }

    fn emitted(rx: &mut broadcast::Receiver<FlighthookMessage>) -> Vec<FlighthookEvent> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .map(|m| {
                assert_eq!(m.actor, "system");
                m.event
            })
            .collect()
    }

    fn is_hand_command(event: &FlighthookEvent, hand: Handedness) -> bool {
        matches!(
            event,
            FlighthookEvent::SetDetectionMode { mode: None, handed: Some(h) } if *h == hand
        )
    }

    #[test]
    fn hand_info_is_stored_and_commands_the_devices_once() {
        let (_dir, state, writer, sender, mut rx) = game();
        handle_game_event(&hand_info(Handedness::Left), &writer, &state, &sender);
        assert_eq!(state.game.snapshot().handed, Some(Handedness::Left));
        let out = emitted(&mut rx);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(is_hand_command(&out[0], Handedness::Left), "{out:?}");

        // The same hand again: stored, nothing emitted.
        handle_game_event(&hand_info(Handedness::Left), &writer, &state, &sender);
        assert_eq!(state.game.snapshot().handed, Some(Handedness::Left));
        assert!(emitted(&mut rx).is_empty());

        // A change is one command again.
        handle_game_event(&hand_info(Handedness::Right), &writer, &state, &sender);
        assert_eq!(state.game.snapshot().handed, Some(Handedness::Right));
        let out = emitted(&mut rx);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(is_hand_command(&out[0], Handedness::Right), "{out:?}");
    }

    /// The first hand is a change from none, the right hand included.
    #[test]
    fn a_first_right_hand_commands_the_devices() {
        let (_dir, state, writer, sender, mut rx) = game();
        handle_game_event(&hand_info(Handedness::Right), &writer, &state, &sender);
        let out = emitted(&mut rx);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(is_hand_command(&out[0], Handedness::Right), "{out:?}");
    }

    #[test]
    fn a_club_change_leaves_the_hand_alone() {
        let (_dir, state, writer, sender, mut rx) = game();
        handle_game_event(&hand_info(Handedness::Left), &writer, &state, &sender);
        emitted(&mut rx);
        handle_game_event(
            &FlighthookEvent::ClubInfo {
                club_info: ClubInfo { club: Club::Putter },
            },
            &writer,
            &state,
            &sender,
        );
        let game = state.game.snapshot();
        assert_eq!(game.handed, Some(Handedness::Left));
        assert_eq!(game.mode, Some(ShotDetectionMode::Putting));
        let out = emitted(&mut rx);
        assert!(
            matches!(
                out.as_slice(),
                [FlighthookEvent::SetDetectionMode {
                    mode: Some(ShotDetectionMode::Putting),
                    handed: None
                }]
            ),
            "{out:?}"
        );
    }

    /// The controller command is not a source of game state: its own
    /// command coming back round, or any other, stores only the mode and
    /// emits nothing.
    #[test]
    fn a_hand_on_the_device_command_is_not_stored() {
        let (_dir, state, writer, sender, mut rx) = game();
        handle_game_event(&hand_info(Handedness::Right), &writer, &state, &sender);
        let own = emitted(&mut rx);
        handle_game_event(&own[0], &writer, &state, &sender);
        handle_game_event(
            &FlighthookEvent::SetDetectionMode {
                mode: Some(ShotDetectionMode::Chipping),
                handed: Some(Handedness::Left),
            },
            &writer,
            &state,
            &sender,
        );
        let game = state.game.snapshot();
        assert_eq!(game.handed, Some(Handedness::Right));
        assert_eq!(game.mode, Some(ShotDetectionMode::Chipping));
        assert!(emitted(&mut rx).is_empty());
    }

    /// The running actor, fed through the bus: a hand sent once and then
    /// repeated produces one command, and its own command does not loop.
    #[test]
    fn the_running_actor_commands_a_hand_change_once() {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-system-test-{}",
            crate::state::config::generate_id()
        )));
        let (state, writer) = SystemState::new(dir.0.join("config.toml"));
        let state = Arc::new(state);
        let (bus_tx, mut rx) = broadcast::channel(64);
        let shutdown = Arc::new(AtomicBool::new(false));
        start_for_test(writer, &state, &bus_tx, Arc::clone(&shutdown));
        let web = BusSender::new("web".into(), bus_tx.clone(), Arc::default());
        for _ in 0..3 {
            web.send(FlighthookMessage::new(hand_info(Handedness::Left)));
        }
        // The actor handles the bus in order, so its mode command for this
        // club comes after everything it does for the three repeats.
        web.send(FlighthookMessage::new(FlighthookEvent::ClubInfo {
            club_info: ClubInfo { club: Club::Putter },
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut commands = Vec::new();
        'wait: while std::time::Instant::now() < deadline {
            while let Ok(m) = rx.try_recv() {
                if m.actor != "system" {
                    continue;
                }
                let sentinel = matches!(
                    m.event,
                    FlighthookEvent::SetDetectionMode { mode: Some(_), .. }
                );
                commands.push(m);
                if sentinel {
                    break 'wait;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(commands.len(), 2, "{commands:?}");
        assert!(is_hand_command(&commands[0].event, Handedness::Left));
        assert!(matches!(
            commands[1].event,
            FlighthookEvent::SetDetectionMode {
                mode: Some(ShotDetectionMode::Putting),
                handed: None
            }
        ));
        assert_eq!(state.game.snapshot().handed, Some(Handedness::Left));
    }

    #[test]
    fn set_shot_grouping_persists_and_emits_the_outcome() {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-system-test-{}",
            crate::state::config::generate_id()
        )));
        let path = dir.0.join("config.toml");
        let (state, _writer) = SystemState::new(path.clone());
        let state = Arc::new(state);
        // Configured but not running: a reconcile would start it.
        state.system.update(|c| {
            c.webserver.clear();
            c.mock_monitor.insert(
                "0".into(),
                flighthook::MockMonitorSection {
                    name: "Mock".into(),
                },
            );
        });
        let running = state.actor_ids();
        let (bus_tx, mut rx) = broadcast::channel(16);
        let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());

        handle_config_command(
            &FlighthookEvent::ConfigCommand {
                request_id: Some("req-1".into()),
                action: Box::new(ConfigAction::SetShotGrouping {
                    mode: ShotGrouping::ByClub,
                }),
            },
            &state,
            &bus_tx,
            &sender,
        );

        assert_eq!(state.system.snapshot().shot_grouping, ShotGrouping::ByClub);
        let toml = std::fs::read_to_string(&path).expect("written");
        assert!(toml.contains(r#"shot_grouping = "by_club""#), "{toml}");
        let msg = rx.try_recv().expect("outcome");
        assert_eq!(msg.actor, "system");
        let FlighthookEvent::ConfigOutcome {
            request_id,
            restarted,
            stopped,
            started,
        } = msg.event
        else {
            panic!("not an outcome: {:?}", msg.event);
        };
        assert_eq!(request_id.as_deref(), Some("req-1"));
        assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
        // A view change reconciles no actor.
        assert_eq!(state.actor_ids(), running);
        assert!(!state.actor_ids().contains(&"mock_monitor.0".to_owned()));
    }

    #[test]
    fn replace_global_keeps_every_actor_section() {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-system-test-{}",
            crate::state::config::generate_id()
        )));
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        let state = Arc::new(state);
        // An address no actor can start on, so the reconcile starts nothing.
        let live_client = flighthook::OpenConnectClientSection {
            name: "Saved elsewhere".into(),
            address: Some("not an address".into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
        };
        state.system.update(|c| {
            c.webserver.clear();
            c.openconnect_client.insert("0".into(), live_client.clone());
        });
        // A body from a stale snapshot: other units, no openconnect_client.0,
        // an openconnect_client.1.
        let mut body = state.system.snapshot();
        body.default_units = flighthook::UnitSystem::Metric;
        body.chipping_clubs = vec![flighthook::Club::SandWedge];
        body.openconnect_client.clear();
        body.openconnect_client
            .insert("1".into(), live_client.clone());
        let (bus_tx, mut rx) = broadcast::channel(16);
        let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());

        handle_config_command(
            &FlighthookEvent::ConfigCommand {
                request_id: Some("req-2".into()),
                action: Box::new(ConfigAction::ReplaceGlobal {
                    config: Box::new(body),
                }),
            },
            &state,
            &bus_tx,
            &sender,
        );

        let saved = state.system.snapshot();
        assert_eq!(saved.default_units, flighthook::UnitSystem::Metric);
        assert_eq!(saved.chipping_clubs, vec![flighthook::Club::SandWedge]);
        assert_eq!(saved.openconnect_client.len(), 1);
        assert_eq!(saved.openconnect_client.get("0"), Some(&live_client));
        let outcome = std::iter::from_fn(|| rx.try_recv().ok())
            .find(|m| matches!(m.event, FlighthookEvent::ConfigOutcome { .. }));
        assert!(outcome.is_some(), "the outcome is emitted");
    }
}
