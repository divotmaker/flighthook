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
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::actors::{
    Actor, ReconfigureOutcome, ResolvedActor, actor_names, reconnectable, resolve_actors,
    start_actor,
};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::state::config::{FlighthookConfig, canonical_id};
use crate::state::{GameStateWriter, Stopped, SystemState};
use flighthook::{ActorStatus, FlighthookEvent, FlighthookMessage, Severity};

// ---------------------------------------------------------------------------
// Config reload
// ---------------------------------------------------------------------------

/// What a config command does with an actor it touches once the actors it
/// stopped have ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    /// Removed: it stays stopped.
    Remove,
    /// Restarted: a new instance starts from the settings then current.
    Restart,
    /// New: it starts from the settings then current.
    Start,
}

/// A config command's actor changes, its stops already made: what happens
/// to each actor it touches ([`finish`]) once the stopped ones have ended,
/// each waited for until its deadline.
pub(crate) struct Plan {
    request_id: Option<String>,
    /// Each actor it touches, in order, and what then happens to it.
    actors: Vec<(String, Then)>,
    /// The actors it stopped and until when each is waited for.
    waits: Vec<(String, Instant)>,
    /// The actors its outcome lists as stopped already: removed while
    /// their old instance closes.
    stopped: Vec<String>,
}

/// Compare the current actor set against the new config, stop what goes or
/// restarts, and return the [`Plan`] for the rest:
///
/// - Deleted actors (current but not expected): stopped, then released
/// - Existing actors: call `reconfigure()`, restarted if needed
/// - New actors: created and started
///
/// The actors it stops are stopped together and released by the same rules
/// as a reconnect ([`release`]): an old instance that has not ended in time
/// is retired, and a new one starts beside it only when the old one holds
/// nothing it needs. Otherwise the new instance starts once the old one has
/// ended ([`restart_closed`]), and the restart is listed in neither
/// `restarted` nor `started`.
///
/// An actor whose old instance is still closing, or that another command is
/// still settling, is left to that: removed, it is listed as stopped; still
/// configured, it is not started now and starts from the settings current
/// when its old instance has ended.
///
/// When `scope` is `Some(actor_id)`, only that actor is reconfigured.
/// Other actors are left untouched (no stop/start/reconfigure).
fn apply_config_reload(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    request_id: Option<String>,
    scope: Option<&str>,
) -> Plan {
    let snap = state.system.snapshot();
    let current_mode = state.game.snapshot().mode;
    let resolved = resolve_actors(&snap, current_mode);
    let in_scope = |id: &str| scope.is_none_or(|target| target == id);

    let mut current_ids = state.actor_ids();
    current_ids.sort();
    let mut expected_ids: Vec<String> = resolved.into_iter().map(|ra| ra.id).collect();
    expected_ids.sort();

    // What changes. "system" is always-on, not config-driven.
    let mut removed = Vec::new();
    let mut restarts = Vec::new();
    for id in current_ids
        .iter()
        .filter(|id| *id != "system" && in_scope(id) && !state.is_settling(id))
    {
        if !expected_ids.contains(id) {
            removed.push(id.clone());
            continue;
        }
        let shutdown = Arc::new(AtomicBool::new(false));
        let sender = BusSender::new(id.clone(), bus_tx.clone(), shutdown);
        if matches!(
            state.reconfigure_actor(id, state, &sender),
            Some(ReconfigureOutcome::RestartRequired)
        ) {
            restarts.push(id.clone());
        }
    }
    // An old instance still closing or settling: its card goes now; with
    // the actor still configured, the new instance starts once the old one
    // has ended.
    let mut stopped = Vec::new();
    let mut leaving: Vec<String> = state.closing_ids();
    leaving.extend(state.settling_ids());
    for id in leaving.into_iter().filter(|id| in_scope(id)) {
        if expected_ids.contains(&id) {
            tracing::info!("config reload: '{id}' starts once its old connection has closed");
        } else {
            tracing::info!("config reload: '{id}' is removed while its old connection closes");
            // Its card is gone: the old instance has nothing more to say.
            state.mute_actor(&id);
            stopped.push(id);
        }
    }
    let new = expected_ids.iter().filter(|id| {
        in_scope(id) && !current_ids.contains(id) && !state.is_closing(id) && !state.is_settling(id)
    });

    // Everything that stops, stopped together, so the waits overlap.
    let stopping: Vec<String> = removed.iter().chain(&restarts).cloned().collect();
    let waits = stop_together(state, &stopping);
    let actors = removed
        .into_iter()
        .map(|id| (id, Then::Remove))
        .chain(restarts.into_iter().map(|id| (id, Then::Restart)))
        .chain(new.map(|id| (id.clone(), Then::Start)))
        .collect();
    Plan {
        request_id,
        actors,
        waits,
        stopped,
    }
}

// ---------------------------------------------------------------------------
// Stop waits
// ---------------------------------------------------------------------------

/// The config commands waiting for the actors they stopped to end. Each
/// waits on a `stop-wait:<ids>` helper thread, so the system actor goes on
/// handling game events and further commands; the helper reports back and
/// the system actor [`finish`]es the command.
pub(crate) struct StopWaits {
    done_tx: std_mpsc::Sender<u64>,
    done_rx: std_mpsc::Receiver<u64>,
    waiting: HashMap<u64, Plan>,
    next: u64,
}

impl StopWaits {
    pub(crate) fn new() -> Self {
        let (done_tx, done_rx) = std_mpsc::channel();
        Self {
            done_tx,
            done_rx,
            waiting: HashMap::new(),
            next: 0,
        }
    }

    /// Carry out `plan`: at once when it stopped nothing, otherwise once the
    /// actors it stopped have ended or their waits have run out. Until then
    /// every actor it touches is settling: another command leaves it alone
    /// and a reconnect of it starts nothing more.
    fn run(
        &mut self,
        state: &Arc<SystemState>,
        bus_tx: &broadcast::Sender<FlighthookMessage>,
        sender: &BusSender,
        mut plan: Plan,
    ) {
        if plan.waits.is_empty() {
            finish(state, bus_tx, sender, plan);
            return;
        }
        for (id, _) in &plan.actors {
            state.begin_settling(id);
        }
        let token = self.next;
        self.next += 1;
        let waits = std::mem::take(&mut plan.waits);
        let name = format!(
            "stop-wait:{}",
            waits
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        let (done, watched) = (self.done_tx.clone(), Arc::clone(state));
        let spawned = std::thread::Builder::new().name(name).spawn(move || {
            for (id, deadline) in waits {
                let _ = watched.wait_for_exit(&id, deadline);
            }
            let _ = done.send(token);
        });
        self.waiting.insert(token, plan);
        if let Err(e) = spawned {
            // Nothing can wait for them: release them as they are now.
            tracing::warn!("config command: cannot wait for its stops: {e}");
            let _ = self.done_tx.send(token);
        }
    }

    /// Finish every command whose stop waits are over.
    pub(crate) fn finish_done(
        &mut self,
        state: &Arc<SystemState>,
        bus_tx: &broadcast::Sender<FlighthookMessage>,
        sender: &BusSender,
    ) {
        while let Ok(token) = self.done_rx.try_recv() {
            if let Some(plan) = self.waiting.remove(&token) {
                finish(state, bus_tx, sender, plan);
            }
        }
    }

    /// Wait for every command still waiting and finish it.
    #[cfg(test)]
    fn settle(
        &mut self,
        state: &Arc<SystemState>,
        bus_tx: &broadcast::Sender<FlighthookMessage>,
        sender: &BusSender,
    ) {
        while !self.waiting.is_empty() {
            let token = self
                .done_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("the stop waits end");
            if let Some(plan) = self.waiting.remove(&token) {
                finish(state, bus_tx, sender, plan);
            }
        }
    }
}

/// Carry out `plan` now that the actors it stopped have ended or their waits
/// have run out: release each stopped one ([`release`]), start what is
/// configured now (restarted and new actors, and a removed one configured
/// again meanwhile), and emit the command's `ConfigOutcome`.
fn finish(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    sender: &BusSender,
    plan: Plan,
) {
    let mut resolved = resolve_actors(&state.system.snapshot(), state.game.snapshot().mode);
    let mut restarted = Vec::new();
    let mut stopped = plan.stopped;
    let mut started = Vec::new();
    for (id, then) in plan.actors {
        let released = match then {
            Then::Remove | Then::Restart => release(state, &id, Instant::now()),
            Then::Start => Released::Free,
        };
        state.end_settling(&id);
        if then == Then::Remove {
            tracing::info!("config reload: stopped removed actor '{id}'");
            stopped.push(id.clone());
        }
        // Removed meanwhile: it stays stopped.
        let Some(fresh) = resolved
            .iter()
            .position(|ra| ra.id == id)
            .map(|i| resolved.swap_remove(i))
        else {
            continue;
        };
        match then {
            Then::Start => {
                tracing::info!("config reload: starting new actor '{id}'");
                start_actor(fresh.id, fresh.actor, state, bus_tx);
                started.push(id);
            }
            Then::Restart => {
                if start_released(state, bus_tx, fresh, released) {
                    tracing::info!("restarted '{id}'");
                    restarted.push(id);
                }
            }
            Then::Remove => {
                if start_released(state, bus_tx, fresh, released) {
                    started.push(id);
                }
            }
        }
    }
    sender.send(FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
        request_id: plan.request_id,
        restarted,
        stopped,
        started,
    }));
}

// ---------------------------------------------------------------------------
// Restart
// ---------------------------------------------------------------------------

/// The `error` on a card whose old connection outlasted its stop wait.
pub(crate) const STILL_CLOSING: &str =
    "The previous connection has not closed yet. It reconnects when it does.";

/// What became of an actor that was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Released {
    /// It ended, or was not running: a new instance may start.
    Free,
    /// It is still ending but holds nothing a new instance needs
    /// ([`Actor::replaceable_while_stopping`]): retired, muted, its thread
    /// left to end. A new instance may start.
    Retired,
    /// It is still ending and holds its device or port: retired, muted and
    /// watched ([`SystemState::close_in_background`]). No new instance
    /// starts until it has ended.
    Closing,
}

/// Stop the actors `ids` at once. Returns each with the time its stop wait
/// ([`Actor::stop_wait`]) runs out.
fn stop_together(state: &SystemState, ids: &[String]) -> Vec<(String, Instant)> {
    let now = Instant::now();
    ids.iter()
        .map(|id| {
            let wait = state.stop_actor(id).unwrap_or_default();
            (id.clone(), now + wait)
        })
        .collect()
}

/// Wait until `deadline` for stopped actor `id` to end, then retire it if
/// it has not.
fn release(state: &SystemState, id: &str, deadline: Instant) -> Released {
    if state.is_closing(id) {
        return Released::Closing;
    }
    match state.wait_for_exit(id, deadline) {
        Stopped::NotRunning | Stopped::Exited => Released::Free,
        Stopped::Lingering => {
            let replaceable = state
                .with_actor(id, |a| a.replaceable_while_stopping())
                .unwrap_or(true);
            if replaceable {
                tracing::warn!("'{id}' is still stopping; it is replaced beside it");
                state.retire_actor(id);
                Released::Retired
            } else {
                tracing::warn!("'{id}' is still stopping; it starts again once it has ended");
                state.close_in_background(id);
                Released::Closing
            }
        }
    }
}

/// Start `fresh` in place of its old instance, `released` as it was
/// ([`release`]): at once when the old instance has ended or holds nothing
/// the new one needs; otherwise its card and an Error alert say the old
/// connection is still closing, and [`restart_closed`] starts it once it
/// has ended. Returns whether the new instance started. Config-change
/// restarts and reconnects both go through here.
fn start_released(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    fresh: ResolvedActor,
    released: Released,
) -> bool {
    match released {
        Released::Free | Released::Retired => {
            start_fresh(state, bus_tx, fresh);
            true
        }
        Released::Closing => {
            let card = BusSender::new(fresh.id.clone(), bus_tx.clone(), Arc::default());
            card.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
                status: ActorStatus::Disconnected,
                telemetry: HashMap::from([("error".into(), STILL_CLOSING.into())]),
            }));
            card.send(FlighthookMessage::new(FlighthookEvent::Alert {
                severity: Severity::Error,
                message: STILL_CLOSING.into(),
            }));
            false
        }
    }
}

/// Start `fresh` as a new instance of its actor. Its card starts over:
/// `Starting`, without an error an earlier instance left.
fn start_fresh(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    fresh: ResolvedActor,
) {
    let card = BusSender::new(fresh.id.clone(), bus_tx.clone(), Arc::default());
    card.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status: ActorStatus::Starting,
        telemetry: HashMap::from([("error".into(), String::new())]),
    }));
    start_actor(fresh.id, fresh.actor, state, bus_tx);
}

/// Start again every actor whose old instance was still closing and has
/// ended since, from the current settings, and emit one `ConfigOutcome`
/// (no `request_id`) listing them as restarted. An actor removed meanwhile
/// is not started. SystemActor runs this between bus messages.
pub(crate) fn restart_closed(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    sender: &BusSender,
) {
    let closed = state.take_closed();
    if closed.is_empty() {
        return;
    }
    let mut resolved = resolve_actors(&state.system.snapshot(), state.game.snapshot().mode);
    let mut restarted = Vec::new();
    for id in closed {
        match resolved.iter().position(|ra| ra.id == id) {
            Some(i) => {
                tracing::info!("'{id}': its old connection has closed, starting it again");
                start_fresh(state, bus_tx, resolved.swap_remove(i));
                restarted.push(id);
            }
            None => tracing::info!("'{id}': its old connection has closed"),
        }
    }
    if !restarted.is_empty() {
        sender.send(FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
            request_id: None,
            restarted,
            stopped: Vec::new(),
            started: Vec::new(),
        }));
    }
}

// ---------------------------------------------------------------------------
// Reconnect
// ---------------------------------------------------------------------------

/// Why a reconnect was refused before anything was stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReconnectRefusal {
    /// The ID names a camera, the web server or the system actor.
    NotReconnectable(String),
    /// No launch monitor or output has the ID in the saved settings.
    NotFound(String),
}

impl ReconnectRefusal {
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::NotReconnectable(m) | Self::NotFound(m) => m,
        }
    }
}

/// The actor a reconnect of `id` is for: its canonical ID ("gspro.N" names
/// "openconnect_client.N"), when `config` has a launch monitor or output
/// with that ID.
pub(crate) fn reconnect_target(
    config: &FlighthookConfig,
    id: &str,
) -> Result<String, ReconnectRefusal> {
    let id = canonical_id(id);
    let type_key = id.split_once('.').map_or(id.as_str(), |(t, _)| t);
    if !reconnectable(type_key) {
        return Err(ReconnectRefusal::NotReconnectable(format!(
            "'{id}' is not a launch monitor or output"
        )));
    }
    if !actor_names(config).contains_key(&id) {
        return Err(ReconnectRefusal::NotFound(format!(
            "no launch monitor or output '{id}'"
        )));
    }
    Ok(id)
}

/// What a reconnect of `id` would start, or why it cannot run now: not a
/// launch monitor or output, its old connection still closing, a command
/// already restarting it, or its settings cannot start it.
fn reconnect_check(state: &SystemState, id: &str) -> Result<ResolvedActor, String> {
    let config = state.system.snapshot();
    let id = reconnect_target(&config, id).map_err(|r| r.message().to_owned())?;
    if state.is_closing(&id) {
        return Err(STILL_CLOSING.into());
    }
    if state.is_settling(&id) {
        return Err(format!("'{id}' is already restarting"));
    }
    resolve_actors(&config, state.game.snapshot().mode)
        .into_iter()
        .find(|ra| ra.id == id)
        .ok_or_else(|| format!("'{id}' cannot start from its saved settings"))
}

/// Why a reconnect of `id` did not start it (its outcome listed nothing as
/// restarted), as far as can be told now.
pub(crate) fn not_reconnected(state: &SystemState, id: &str) -> String {
    match reconnect_check(state, id) {
        Err(reason) => reason,
        Ok(_) => format!("'{id}' was not reconnected"),
    }
}

/// Stop the launch monitor or output `id` for a reconnect: the [`Plan`]
/// that waits for it to let go of its device or connection and starts it
/// again from the saved settings, as a flighthook restart does for it.
/// Every other actor is left alone. Returns why it cannot be reconnected
/// now instead: while an old instance of it is still closing, or a command
/// is restarting it, nothing more is done, and it starts again once that is
/// over.
fn reconnect(
    state: &Arc<SystemState>,
    request_id: Option<String>,
    id: &str,
) -> Result<Plan, String> {
    let fresh = reconnect_check(state, id)?;
    let id = fresh.id;
    tracing::info!("reconnect: stopping '{id}'");
    let waits = stop_together(state, std::slice::from_ref(&id));
    Ok(Plan {
        request_id,
        actors: vec![(id, Then::Restart)],
        waits,
        stopped: Vec::new(),
    })
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
    fn start(
        &self,
        _state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
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
            .expect("failed to spawn system thread")
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

    let mut waits = StopWaits::new();
    loop {
        // An old instance found still closing has ended: start it again.
        restart_closed(&state, &bus_tx, &sender);
        // A command's stop waits are over: carry it out.
        waits.finish_done(&state, &bus_tx, &sender);
        match receiver.poll() {
            Err(PollError::Shutdown) => return,
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(Some(msg)) => match &msg.event {
                FlighthookEvent::ConfigCommand { .. } => {
                    handle_config_command(&msg.event, &state, &bus_tx, &sender, &mut waits);
                }
                // The clips belong to the shots being cleared.
                FlighthookEvent::ClearShotHistory => clear_clips(&state),
                _ => handle_game_event(&msg.event, &writer, &state, &sender),
            },
        }
    }
}

/// Delete every replay clip (Clear history). Folders that can't be deleted
/// now are retried later.
pub(crate) fn clear_clips(state: &SystemState) {
    let removed = state.clips.clear();
    if !removed.is_empty() {
        tracing::info!(
            "clear history: deleted the clips of {} shot(s)",
            removed.len()
        );
    }
}

/// After a config change: when the clip folder is no longer `before`, the
/// clip store forgets the old folder's clips (left on disk as they are) and
/// indexes the new one; the shots whose clips are no longer served go out
/// in one `shot_video_removed`.
fn follow_clip_dir(state: &SystemState, sender: &BusSender, before: &std::path::Path) {
    let replay = state.system.replay();
    let dir = crate::state::clips::clip_dir(&replay);
    if dir == before {
        return;
    }
    let removed = state
        .clips
        .reindex(&dir, crate::state::clips::keep(&replay));
    tracing::info!(
        "clips: now in {} ({} shot(s) no longer have clips; {} is left as it is)",
        dir.display(),
        removed.len(),
        before.display()
    );
    if !removed.is_empty() {
        sender.send(FlighthookMessage::new(FlighthookEvent::ShotVideoRemoved {
            keys: removed,
        }));
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

/// Apply config command `event`. The actors it stops are waited for on a
/// helper thread (`waits`); its `ConfigOutcome` goes out once they have
/// ended and what it starts has started, at once when it stops nothing.
pub(crate) fn handle_config_command(
    event: &FlighthookEvent,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    sender: &BusSender,
    waits: &mut StopWaits,
) {
    use flighthook::ConfigAction;

    let FlighthookEvent::ConfigCommand {
        ref request_id,
        ref action,
    } = *event
    else {
        return;
    };

    let clip_dir = crate::state::clips::clip_dir(&state.system.replay());

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
                p.replay = config.replay.clone();
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
        ConfigAction::UpsertCamera { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.camera.insert(idx, section.clone());
            });
            scope = Some(format!("camera.{index}"));
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
                    "camera" => {
                        p.camera.remove(&idx);
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
        ConfigAction::Reconnect { id } => {
            // The config is unchanged: only this actor is touched. A
            // refused one lists nothing as restarted.
            match reconnect(state, request_id.clone(), id) {
                Ok(plan) => waits.run(state, bus_tx, sender, plan),
                Err(e) => {
                    tracing::warn!("reconnect '{id}': {e}");
                    sender.send(FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
                        request_id: request_id.clone(),
                        restarted: Vec::new(),
                        stopped: Vec::new(),
                        started: Vec::new(),
                    }));
                }
            }
            return;
        }
        other => {
            tracing::warn!("config command: {other:?} is not handled");
            sender.send(FlighthookMessage::new(FlighthookEvent::ConfigOutcome {
                request_id: request_id.clone(),
                restarted: Vec::new(),
                stopped: Vec::new(),
                started: Vec::new(),
            }));
            return;
        }
    }

    follow_clip_dir(state, sender, &clip_dir);

    // Reconcile actors (webserver included — its reconfigure() handles bind changes)
    let plan = apply_config_reload(state, bus_tx, request_id.clone(), scope.as_deref());
    waits.run(state, bus_tx, sender, plan);
}

/// Apply config command `event` as the system actor does and wait for it
/// to finish, its stop waits included.
#[cfg(test)]
pub(crate) fn run_config_command(
    event: &FlighthookEvent,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<FlighthookMessage>,
    sender: &BusSender,
) {
    let mut waits = StopWaits::new();
    handle_config_command(event, state, bus_tx, sender, &mut waits);
    waits.settle(state, bus_tx, sender);
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

        run_config_command(
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

        run_config_command(
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
    #[test]
    fn replace_global_takes_the_replay_settings() {
        let (_dir, state, _writer, sender, _rx) = game();
        let state = Arc::new(state);
        let mut body = state.system.snapshot();
        body.replay.keep_shots = 3;
        body.replay.pre_roll_s = 2.0;
        body.replay.start_before_impact_s = 1.25;
        body.replay.clip_dir = Some("/srv/swings".into());
        run_config_command(
            &FlighthookEvent::ConfigCommand {
                request_id: None,
                action: Box::new(ConfigAction::ReplaceGlobal {
                    config: Box::new(body.clone()),
                }),
            },
            &state,
            sender.raw_sender(),
            &sender,
        );
        assert_eq!(state.system.replay(), body.replay);
    }

    #[test]
    fn a_camera_section_is_upserted_and_removed_with_its_actor() {
        let (_dir, state, _writer, sender, _rx) = game();
        let state = Arc::new(state);
        let section = flighthook::CameraSection {
            camera: flighthook::CameraSection::TEST_PATTERN.into(),
            mode: "160x120@15".into(),
            ..flighthook::CameraSection::default()
        };
        let command = |action| FlighthookEvent::ConfigCommand {
            request_id: None,
            action: Box::new(action),
        };
        run_config_command(
            &command(ConfigAction::UpsertCamera {
                index: "4".into(),
                section: section.clone(),
            }),
            &state,
            sender.raw_sender(),
            &sender,
        );
        assert_eq!(state.system.camera("4"), Some(section));
        assert!(state.actor_ids().contains(&"camera.4".to_owned()));
        run_config_command(
            &command(ConfigAction::Remove {
                id: "camera.4".into(),
            }),
            &state,
            sender.raw_sender(),
            &sender,
        );
        assert_eq!(state.system.camera("4"), None);
        assert!(!state.actor_ids().contains(&"camera.4".to_owned()));
    }

    #[test]
    fn a_new_clip_folder_is_followed_and_the_old_clips_are_no_longer_served() {
        let (dir, state, _writer, sender, mut rx) = game();
        let state = Arc::new(state);
        let old = dir.0.join("old");
        state
            .system
            .update(|c| c.replay.clip_dir = Some(old.display().to_string()));
        let key = flighthook::ShotKey {
            shot_id: "a".into(),
            shot_number: 1,
        };
        let shot = crate::state::clips::ClipShot {
            key: key.clone(),
            device: "mock".into(),
            impact_lead_ms: 0,
        };
        let slot = state
            .clips
            .begin(&old, &[shot], state.clips.generation())
            .expect("begin");
        let video = flighthook::ShotVideo {
            camera: "camera.0".into(),
            position: flighthook::CameraPosition::GolferFront,
            duration_ms: 1,
            fps: 30.0,
            trigger_offset_ms: 0,
            impact_offset_ms: 0,
            complete: true,
            dropped: 0,
        };
        assert!(state.clips.finish(&slot, video, 20).kept);

        let mut body = state.system.snapshot();
        body.replay.clip_dir = Some(dir.0.join("new").display().to_string());
        run_config_command(
            &FlighthookEvent::ConfigCommand {
                request_id: None,
                action: Box::new(ConfigAction::ReplaceGlobal {
                    config: Box::new(body),
                }),
            },
            &state,
            sender.raw_sender(),
            &sender,
        );
        assert!(state.clips.videos("a").is_empty());
        assert!(slot.dir.is_dir(), "the old folder is left as it is");
        let removed = std::iter::from_fn(|| rx.try_recv().ok())
            .find_map(|m| match m.event {
                FlighthookEvent::ShotVideoRemoved { keys } => Some(keys),
                _ => None,
            })
            .expect("shot_video_removed");
        assert_eq!(removed, [key]);
    }

    #[test]
    fn clear_history_deletes_the_clips() {
        let (dir, state, _writer, _sender, _rx) = game();
        let clips = dir.0.join("clips");
        let shot = crate::state::clips::ClipShot {
            key: flighthook::ShotKey {
                shot_id: "a".into(),
                shot_number: 1,
            },
            device: "mock".into(),
            impact_lead_ms: 0,
        };
        let slot = state
            .clips
            .begin(&clips, &[shot], state.clips.generation())
            .expect("begin");
        let video = flighthook::ShotVideo {
            camera: "camera.0".into(),
            position: flighthook::CameraPosition::GolferFront,
            duration_ms: 1,
            fps: 30.0,
            trigger_offset_ms: 0,
            impact_offset_ms: 0,
            complete: true,
            dropped: 0,
        };
        assert!(state.clips.finish(&slot, video, 20).kept);
        assert_eq!(state.clips.videos("a").len(), 1);
        clear_clips(&state);
        assert!(state.clips.videos("a").is_empty());
        assert!(!slot.dir.exists());
    }

    // ----- Reconnect -----

    /// State whose config `edit` sets up, its bus with a receiver, and every
    /// configured actor started, as `main` starts them.
    fn running(
        edit: impl FnOnce(&mut FlighthookConfig),
    ) -> (
        TempDir,
        Arc<SystemState>,
        broadcast::Sender<FlighthookMessage>,
        broadcast::Receiver<FlighthookMessage>,
    ) {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-system-test-{}",
            crate::state::config::generate_id()
        )));
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        let state = Arc::new(state);
        state.system.update(|c| {
            c.webserver.clear();
            edit(c);
        });
        let (bus_tx, rx) = broadcast::channel(4096);
        for ra in resolve_actors(&state.system.snapshot(), None) {
            start_actor(ra.id, ra.actor, &state, &bus_tx);
        }
        (dir, state, bus_tx, rx)
    }

    fn mock(name: &str) -> flighthook::MockMonitorSection {
        flighthook::MockMonitorSection { name: name.into() }
    }

    /// Reconnect `id` through the system actor's command handler. Returns
    /// the outcome's restarted IDs and everything on the bus up to it.
    fn reconnect_through_command(
        state: &Arc<SystemState>,
        bus_tx: &broadcast::Sender<FlighthookMessage>,
        rx: &mut broadcast::Receiver<FlighthookMessage>,
        id: &str,
    ) -> (Vec<String>, Vec<FlighthookMessage>) {
        let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
        run_config_command(
            &FlighthookEvent::ConfigCommand {
                request_id: Some("reconnect-1".into()),
                action: Box::new(ConfigAction::Reconnect { id: id.into() }),
            },
            state,
            bus_tx,
            &sender,
        );
        let seen: Vec<FlighthookMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let restarted = outcome_of(&seen, "reconnect-1").0;
        (restarted, seen)
    }

    /// The restarted, stopped and started IDs of the outcome for
    /// `request_id` in `seen`.
    fn outcome_of(
        seen: &[FlighthookMessage],
        request_id: &str,
    ) -> (Vec<String>, Vec<String>, Vec<String>) {
        seen.iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::ConfigOutcome {
                    request_id: Some(rid),
                    restarted,
                    stopped,
                    started,
                } if rid == request_id => {
                    assert_eq!(m.actor, "system");
                    Some((restarted.clone(), stopped.clone(), started.clone()))
                }
                _ => None,
            })
            .expect("the outcome is emitted")
    }

    /// Statuses `actor` sent, in order.
    fn statuses(seen: &[FlighthookMessage], actor: &str) -> Vec<ActorStatus> {
        seen.iter()
            .filter(|m| m.actor == actor)
            .filter_map(|m| match &m.event {
                FlighthookEvent::ActorStatus { status, .. } => Some(*status),
                _ => None,
            })
            .collect()
    }

    /// Messages until `want` matches one, waiting up to 8 s.
    fn until(
        rx: &mut broadcast::Receiver<FlighthookMessage>,
        what: &str,
        mut want: impl FnMut(&FlighthookMessage) -> bool,
    ) -> Vec<FlighthookMessage> {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(msg) => {
                    let done = want(&msg);
                    seen.push(msg);
                    if done {
                        return seen;
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        panic!("{what} not seen");
    }

    fn stop_all(state: &SystemState) {
        for id in state.actor_ids() {
            state.stop_and_wait(&id);
        }
    }

    #[test]
    fn a_reconnect_starts_a_new_instance_and_leaves_the_others_alone() {
        let (_dir, state, bus_tx, mut rx) = running(|c| {
            c.mock_monitor.insert("0".into(), mock("Mock A"));
            c.mock_monitor.insert("1".into(), mock("Mock B"));
            c.random_club.insert(
                "0".into(),
                flighthook::RandomClubSection {
                    name: "Random club".into(),
                },
            );
        });
        let thread = |id: &str| state.actor_thread(id).expect("running");
        let (old, other, club) = (
            thread("mock_monitor.0"),
            thread("mock_monitor.1"),
            thread("random_club.0"),
        );
        let config = serde_json::to_value(state.system.snapshot()).expect("config");

        let (restarted, seen) =
            reconnect_through_command(&state, &bus_tx, &mut rx, "mock_monitor.0");
        assert_eq!(restarted, ["mock_monitor.0"]);
        assert_ne!(thread("mock_monitor.0"), old, "a new instance");
        assert_eq!(thread("mock_monitor.1"), other);
        assert_eq!(thread("random_club.0"), club);
        assert_eq!(
            serde_json::to_value(state.system.snapshot()).expect("config"),
            config,
            "the settings are untouched"
        );

        // The old instance said goodbye before the card started again.
        let card = statuses(&seen, "mock_monitor.0");
        let stopped = card
            .iter()
            .position(|s| *s == ActorStatus::Disconnected)
            .expect("the old instance stopped");
        let started = card
            .iter()
            .rposition(|s| *s == ActorStatus::Starting)
            .expect("a new start");
        assert!(stopped < started, "{card:?}");
        for id in ["mock_monitor.1", "random_club.0"] {
            assert!(
                !statuses(&seen, id).contains(&ActorStatus::Disconnected),
                "{id} was not stopped"
            );
        }
        // The new instance runs: it reports itself connected.
        until(&mut rx, "the new instance", |m| {
            m.actor == "mock_monitor.0"
                && matches!(
                    m.event,
                    FlighthookEvent::ActorStatus {
                        status: ActorStatus::Connected,
                        ..
                    }
                )
        });
        stop_all(&state);
    }

    #[test]
    fn only_a_configured_launch_monitor_or_output_can_be_reconnected() {
        let (_dir, state, bus_tx, mut rx) = running(|c| {
            c.mock_monitor.insert("0".into(), mock("Mock"));
        });
        state.system.update(|c| {
            c.camera.insert(
                "0".into(),
                flighthook::CameraSection {
                    camera: flighthook::CameraSection::TEST_PATTERN.into(),
                    ..flighthook::CameraSection::default()
                },
            );
            c.webserver.insert(
                "0".into(),
                flighthook::WebserverSection {
                    name: "Web Server".into(),
                    bind: "127.0.0.1:0".into(),
                },
            );
        });
        let config = state.system.snapshot();
        for id in ["camera.0", "webserver.0", "system"] {
            assert!(
                matches!(
                    reconnect_target(&config, id),
                    Err(ReconnectRefusal::NotReconnectable(_))
                ),
                "{id}"
            );
        }
        for id in [
            "mock_monitor.7",
            "mevo.0",
            "openconnect_client.0",
            "nonsense",
        ] {
            assert!(
                matches!(
                    reconnect_target(&config, id),
                    Err(ReconnectRefusal::NotFound(_))
                ),
                "{id}"
            );
        }
        assert_eq!(
            reconnect_target(&config, "mock_monitor.0").as_deref(),
            Ok("mock_monitor.0")
        );

        let running = state.actor_thread("mock_monitor.0");
        for id in ["camera.0", "webserver.0", "system", "mock_monitor.7"] {
            let (restarted, _) = reconnect_through_command(&state, &bus_tx, &mut rx, id);
            assert!(restarted.is_empty(), "{id}");
        }
        assert_eq!(state.actor_ids(), ["mock_monitor.0"], "nothing started");
        assert_eq!(state.actor_thread("mock_monitor.0"), running);
        stop_all(&state);
    }

    #[test]
    fn the_gspro_id_names_its_open_connect_client() {
        let mut config = FlighthookConfig::default();
        config
            .openconnect_client
            .insert("2".into(), flighthook::OpenConnectClientSection::default());
        assert_eq!(
            reconnect_target(&config, "gspro.2").as_deref(),
            Ok("openconnect_client.2")
        );
    }

    /// A simulator that accepts every connection and says nothing. The
    /// peer addresses it accepted, in order.
    struct Simulator {
        addr: std::net::SocketAddr,
        peers: Arc<Mutex<Vec<std::net::SocketAddr>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Simulator {
        fn spawn() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let addr = listener.local_addr().expect("addr");
            let peers = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let (peers, stop) = (Arc::clone(&peers), Arc::clone(&stop));
                std::thread::spawn(move || {
                    let mut held = Vec::new();
                    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((stream, peer)) => {
                                peers.lock().unwrap().push(peer);
                                held.push(stream);
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(5)),
                        }
                    }
                })
            };
            Self {
                addr,
                peers,
                stop,
                thread: Some(thread),
            }
        }

        fn peers(&self) -> Vec<std::net::SocketAddr> {
            self.peers.lock().unwrap().clone()
        }
    }

    impl Drop for Simulator {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn connected(m: &FlighthookMessage, actor: &str) -> bool {
        m.actor == actor
            && matches!(
                m.event,
                FlighthookEvent::ActorStatus {
                    status: ActorStatus::Connected,
                    ..
                }
            )
    }

    #[test]
    fn an_open_connect_client_reconnects_and_is_not_refused_as_its_old_self() {
        let sim = Simulator::spawn();
        let (_dir, state, bus_tx, mut rx) = running(|c| {
            c.openconnect_client.insert(
                "0".into(),
                flighthook::OpenConnectClientSection {
                    name: "GSPro".into(),
                    address: Some(sim.addr.to_string()),
                    ..flighthook::OpenConnectClientSection::default()
                },
            );
            // This process's own server, which refuses its own clients.
            c.openconnect_server.insert(
                "0".into(),
                flighthook::OpenConnectServerSection {
                    name: "Uneekor".into(),
                    bind: Some("127.0.0.1:0".into()),
                },
            );
        });
        let id = "openconnect_client.0";
        until(&mut rx, "connected", |m| connected(m, id));
        let first = sim.peers()[0];
        assert_eq!(
            state.local_clients.client_at(first).as_deref(),
            Some(id),
            "registered as this process's client"
        );

        let (restarted, _) = reconnect_through_command(&state, &bus_tx, &mut rx, id);
        assert_eq!(restarted, [id]);
        assert_eq!(
            state.local_clients.client_at(first),
            None,
            "the old connection's registration is released"
        );

        let seen = until(&mut rx, "connected again", |m| connected(m, id));
        let refused = seen.iter().any(|m| {
            m.actor == id
                && matches!(&m.event, FlighthookEvent::ActorStatus { telemetry, .. }
                    if telemetry.get("error").is_some_and(|e| !e.is_empty()))
        });
        assert!(!refused, "no refusal");
        let peers = sim.peers();
        assert_eq!(peers.len(), 2, "{peers:?}");
        assert_eq!(state.local_clients.client_at(peers[1]).as_deref(), Some(id));
        stop_all(&state);
    }

    /// An actor whose thread ignores its stop for `holds`, then sends an
    /// alert and ends. It holds nothing unless `replaceable` is false, and
    /// needs a restart for any settings change.
    struct Stubborn {
        holds: Duration,
        replaceable: bool,
    }

    /// How long a [`Stubborn`] is waited for.
    const STUBBORN_WAIT: Duration = Duration::from_millis(100);

    impl Actor for Stubborn {
        fn start(
            &self,
            _state: Arc<SystemState>,
            sender: BusSender,
            _receiver: BusReceiver,
        ) -> std::thread::JoinHandle<()> {
            let holds = self.holds;
            std::thread::spawn(move || {
                std::thread::sleep(holds);
                sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                    severity: Severity::Warn,
                    message: "late".into(),
                }));
            })
        }

        fn stop_wait(&self) -> Duration {
            STUBBORN_WAIT
        }

        fn replaceable_while_stopping(&self) -> bool {
            self.replaceable
        }

        fn reconfigure(
            &self,
            _state: &Arc<SystemState>,
            _sender: &BusSender,
        ) -> ReconfigureOutcome {
            ReconfigureOutcome::RestartRequired
        }
    }

    /// Running mock monitor `mock_monitor.N` for each `(N, stubborn)`, the
    /// configured ones a [`Stubborn`] in place of the mock.
    fn with_stubborn(
        mocks: &[(&str, Option<Stubborn>)],
    ) -> (
        TempDir,
        Arc<SystemState>,
        broadcast::Sender<FlighthookMessage>,
        broadcast::Receiver<FlighthookMessage>,
    ) {
        let (dir, state, bus_tx, rx) = running(|_| {});
        for (n, stubborn) in mocks {
            state
                .system
                .update(|c| drop(c.mock_monitor.insert((*n).into(), mock("Mock"))));
            let actor: Box<dyn Actor> = match stubborn {
                Some(s) => Box::new(Stubborn {
                    holds: s.holds,
                    replaceable: s.replaceable,
                }),
                None => Box::new(crate::actors::mock::launch::MockLaunchActor {
                    initial_mode: flighthook::ShotDetectionMode::Full,
                }),
            };
            start_actor(format!("mock_monitor.{n}"), actor, &state, &bus_tx);
        }
        (dir, state, bus_tx, rx)
    }

    fn stubborn(holds_ms: u64, replaceable: bool) -> Option<Stubborn> {
        Some(Stubborn {
            holds: Duration::from_millis(holds_ms),
            replaceable,
        })
    }

    /// Whether `seen` has `actor`'s card saying its old connection is still
    /// closing, and an Error alert saying the same.
    fn still_closing_reported(seen: &[FlighthookMessage], actor: &str) -> bool {
        let on_card = seen.iter().any(|m| {
            m.actor == actor
                && matches!(&m.event, FlighthookEvent::ActorStatus { telemetry, .. }
                    if telemetry.get("error").map(String::as_str) == Some(STILL_CLOSING))
        });
        let alerted = seen.iter().any(|m| {
            m.actor == actor
                && matches!(&m.event, FlighthookEvent::Alert { severity: Severity::Error, message }
                    if message == STILL_CLOSING)
        });
        on_card && alerted
    }

    fn error_cleared(seen: &[FlighthookMessage], actor: &str) -> bool {
        seen.iter().any(|m| {
            m.actor == actor
                && matches!(&m.event, FlighthookEvent::ActorStatus { status: ActorStatus::Starting, telemetry }
                    if telemetry.get("error").is_some_and(String::is_empty))
        })
    }

    /// Whether a [`Stubborn`] `actor` had its last word in `seen`.
    fn late_from(seen: &[FlighthookMessage], actor: &str) -> bool {
        seen.iter().any(|m| {
            m.actor == actor
                && matches!(&m.event, FlighthookEvent::Alert { message, .. } if message == "late")
        })
    }

    fn late(seen: &[FlighthookMessage]) -> bool {
        seen.iter().any(
            |m| matches!(&m.event, FlighthookEvent::Alert { message, .. } if message == "late"),
        )
    }

    /// The running system actor: a reconnect that finds the old connection
    /// still closing says so, a second press meanwhile starts nothing, and
    /// once the old connection has closed it reconnects by itself, clearing
    /// the card's error.
    #[test]
    fn a_reconnect_waiting_on_the_old_connection_completes_by_itself() {
        let id = "mock_monitor.0";
        let (dir, state, bus_tx, mut rx) = with_stubborn(&[("0", stubborn(600, false))]);
        let (_, writer) = SystemState::new(dir.0.join("unused.toml"));
        let shutdown = Arc::new(AtomicBool::new(false));
        start_for_test(writer, &state, &bus_tx, Arc::clone(&shutdown));
        let web = BusSender::new("web".into(), bus_tx.clone(), Arc::default());
        let press = |n: u32| {
            web.send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
                request_id: Some(format!("press-{n}")),
                action: Box::new(ConfigAction::Reconnect { id: id.into() }),
            }));
        };
        press(1);
        let mut seen = until(
            &mut rx,
            "the first outcome",
            |m| matches!(&m.event, FlighthookEvent::ConfigOutcome { request_id: Some(r), .. } if r == "press-1"),
        );
        assert!(outcome_of(&seen, "press-1").0.is_empty(), "not started yet");
        assert!(
            still_closing_reported(&seen, id),
            "the card and an alert say why"
        );
        assert!(state.is_closing(id));
        assert_eq!(state.actor_thread(id), None, "nothing started beside it");

        // Pressed again meanwhile: refused, nothing more reported.
        press(2);
        let again = until(
            &mut rx,
            "the second outcome",
            |m| matches!(&m.event, FlighthookEvent::ConfigOutcome { request_id: Some(r), .. } if r == "press-2"),
        );
        assert!(outcome_of(&again, "press-2").0.is_empty());
        assert!(!still_closing_reported(&again, id), "reported once");
        seen.extend(again);

        // The old connection closes: it reconnects by itself.
        let done = until(&mut rx, "the restart", |m| {
            matches!(&m.event, FlighthookEvent::ConfigOutcome { request_id: None, restarted, .. }
                if restarted == &[id.to_owned()])
        });
        assert!(error_cleared(&done, id), "the error is cleared");
        assert!(!late(&done), "the old instance is muted");
        assert!(!state.is_closing(id));
        assert!(state.actor_thread(id).is_some(), "the new instance runs");
        seen.extend(done);
        let instance = state.actor_thread(id);
        seen.extend(until(&mut rx, "the new instance connects", |m| {
            connected(m, id)
        }));
        std::thread::sleep(Duration::from_millis(300));
        seen.extend(std::iter::from_fn(|| rx.try_recv().ok()));
        let starts = statuses(&seen, id)
            .into_iter()
            .filter(|s| *s == ActorStatus::Starting)
            .count();
        assert_eq!(starts, 1, "one restart, for two presses");
        assert_eq!(state.actor_thread(id), instance, "and no other after it");
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        stop_all(&state);
    }

    #[test]
    fn an_old_instance_holding_nothing_is_replaced_and_silenced() {
        let (_dir, state, bus_tx, mut rx) = with_stubborn(&[("0", stubborn(400, true))]);
        let stubborn = state.actor_thread("mock_monitor.0");

        let (restarted, _) = reconnect_through_command(&state, &bus_tx, &mut rx, "mock_monitor.0");
        assert_eq!(restarted, ["mock_monitor.0"]);
        assert_ne!(state.actor_thread("mock_monitor.0"), stubborn);
        assert!(!state.is_closing("mock_monitor.0"));

        // The old instance's last word never reaches the bus.
        std::thread::sleep(Duration::from_millis(600));
        let seen: Vec<FlighthookMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(!late(&seen), "the replaced instance is muted");
        stop_all(&state);
    }

    /// Save `mock_monitor.N` (a scoped upsert) through the command handler.
    /// Returns what it emitted.
    fn save_mock(
        state: &Arc<SystemState>,
        bus_tx: &broadcast::Sender<FlighthookMessage>,
        rx: &mut broadcast::Receiver<FlighthookMessage>,
        n: &str,
    ) -> Vec<FlighthookMessage> {
        let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
        run_config_command(
            &FlighthookEvent::ConfigCommand {
                request_id: Some("save".into()),
                action: Box::new(ConfigAction::UpsertMockMonitor {
                    index: n.into(),
                    section: mock("Renamed"),
                }),
            },
            state,
            bus_tx,
            &sender,
        );
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// A Save that restarts an actor goes through the same release as a
    /// reconnect: one still holding its device is not started beside, and
    /// starts once it has ended; one holding nothing is replaced and muted.
    #[test]
    fn a_save_restarts_an_actor_by_the_reconnect_rules() {
        let (_dir, state, bus_tx, mut rx) =
            with_stubborn(&[("0", stubborn(500, false)), ("1", stubborn(1500, true))]);
        let held = "mock_monitor.0";

        let seen = save_mock(&state, &bus_tx, &mut rx, "0");
        let (restarted, stopped, started) = outcome_of(&seen, "save");
        assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
        assert!(still_closing_reported(&seen, held));
        assert!(state.is_closing(held));
        assert_eq!(state.actor_thread(held), None, "nothing beside it");
        // A second save meanwhile starts nothing either.
        let seen = save_mock(&state, &bus_tx, &mut rx, "0");
        let (restarted, _, started) = outcome_of(&seen, "save");
        assert!(restarted.is_empty() && started.is_empty());
        assert_eq!(state.actor_thread(held), None);

        // Once it has ended, it starts again from the saved settings.
        let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
        wait_until("closed", || {
            restart_closed(&state, &bus_tx, &sender);
            !state.is_closing(held)
        });
        assert!(state.actor_thread(held).is_some());
        let seen: Vec<FlighthookMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(error_cleared(&seen, held));
        assert!(!late_from(&seen, held), "the old instance is muted");

        // Holding nothing: replaced at once.
        let free = "mock_monitor.1";
        let before = state.actor_thread(free);
        let seen = save_mock(&state, &bus_tx, &mut rx, "1");
        assert_eq!(outcome_of(&seen, "save").0, [free]);
        assert_ne!(state.actor_thread(free), before);
        std::thread::sleep(Duration::from_millis(1200));
        let seen: Vec<FlighthookMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(!late_from(&seen, free), "the replaced instance is muted");
        stop_all(&state);
    }

    #[test]
    fn a_removal_releases_an_actor_by_the_reconnect_rules() {
        let (_dir, state, bus_tx, mut rx) =
            with_stubborn(&[("0", stubborn(400, false)), ("1", stubborn(400, true))]);
        let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
        for n in ["0", "1"] {
            run_config_command(
                &FlighthookEvent::ConfigCommand {
                    request_id: Some(format!("remove-{n}")),
                    action: Box::new(ConfigAction::Remove {
                        id: format!("mock_monitor.{n}"),
                    }),
                },
                &state,
                &bus_tx,
                &sender,
            );
        }
        let seen: Vec<FlighthookMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(outcome_of(&seen, "remove-0").1, ["mock_monitor.0"]);
        assert_eq!(outcome_of(&seen, "remove-1").1, ["mock_monitor.1"]);
        assert!(state.actor_ids().is_empty(), "both unregistered");
        assert!(
            state.is_closing("mock_monitor.0"),
            "the one holding its device"
        );
        assert!(!state.is_closing("mock_monitor.1"));

        // Once it has ended, nothing starts: it was removed.
        wait_until("closed", || {
            restart_closed(&state, &bus_tx, &sender);
            !state.is_closing("mock_monitor.0")
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(state.actor_ids().is_empty());
        let seen: Vec<FlighthookMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(!late(&seen), "neither speaks after its removal");
        assert!(
            !seen
                .iter()
                .any(|m| matches!(m.event, FlighthookEvent::ConfigOutcome { .. })),
            "no restart"
        );
    }

    /// A full save stops what it restarts together: the stop waits
    /// overlap, so one command waits at most the longest of them.
    #[test]
    fn a_full_save_waits_for_its_stops_together() {
        let (_dir, state, bus_tx, mut rx) = with_stubborn(&[
            ("0", stubborn(1500, true)),
            ("1", stubborn(1500, true)),
            ("2", stubborn(1500, true)),
        ]);
        let sender = BusSender::new("system".into(), bus_tx.clone(), Arc::default());
        let at = std::time::Instant::now();
        run_config_command(
            &FlighthookEvent::ConfigCommand {
                request_id: Some("all".into()),
                action: Box::new(ConfigAction::ReplaceAll {
                    config: Box::new(state.system.snapshot()),
                }),
            },
            &state,
            &bus_tx,
            &sender,
        );
        let took = at.elapsed();
        let seen: Vec<FlighthookMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let mut restarted = outcome_of(&seen, "all").0;
        restarted.sort();
        assert_eq!(
            restarted,
            ["mock_monitor.0", "mock_monitor.1", "mock_monitor.2"]
        );
        assert!(took >= STUBBORN_WAIT, "{took:?}");
        assert!(
            took < STUBBORN_WAIT * 2,
            "not one wait after another: {took:?}"
        );
        stop_all(&state);
    }

    /// An actor that ends `takes` after its stop, then sets `ended`.
    struct Slow {
        takes: Duration,
        ended: Arc<AtomicBool>,
    }

    impl Actor for Slow {
        fn start(
            &self,
            _state: Arc<SystemState>,
            _sender: BusSender,
            receiver: BusReceiver,
        ) -> std::thread::JoinHandle<()> {
            let (takes, ended) = (self.takes, Arc::clone(&self.ended));
            std::thread::spawn(move || {
                while !receiver.is_shutdown() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(takes);
                ended.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        }
    }

    /// At exit every actor is stopped and waited for, together, so one that
    /// lets go of its device on the way out gets to; the wait is capped.
    #[test]
    fn exit_waits_for_the_actors_to_let_go() {
        let (_dir, state, bus_tx, _rx) = running(|_| {});
        let slow: Vec<Arc<AtomicBool>> = (0..3).map(|_| Arc::default()).collect();
        for (n, ended) in slow.iter().enumerate() {
            start_actor(
                format!("square.{n}"),
                Box::new(Slow {
                    takes: Duration::from_millis(300),
                    ended: Arc::clone(ended),
                }),
                &state,
                &bus_tx,
            );
        }
        // One whose thread outlives the cap, and one still closing.
        start_actor(
            "mock_monitor.0".into(),
            Box::new(Stubborn {
                holds: Duration::from_secs(3),
                replaceable: false,
            }),
            &state,
            &bus_tx,
        );
        start_actor(
            "mock_monitor.1".into(),
            Box::new(Stubborn {
                holds: Duration::from_secs(3),
                replaceable: false,
            }),
            &state,
            &bus_tx,
        );
        state.close_in_background("mock_monitor.1");

        let cap = Duration::from_millis(800);
        let at = std::time::Instant::now();
        let running = state.stop_all_and_wait(cap);
        let took = at.elapsed();
        for ended in &slow {
            assert!(
                ended.load(std::sync::atomic::Ordering::SeqCst),
                "waited for"
            );
        }
        assert!(
            took < Duration::from_millis(1200),
            "together, capped: {took:?}"
        );
        assert_eq!(running, ["mock_monitor.0", "mock_monitor.1"]);
    }

    /// Two reconnects of one actor back to back: the second, arriving while
    /// the first still waits for the old instance, is answered with nothing
    /// restarted and starts nothing; the first starts exactly one new
    /// instance.
    #[test]
    fn a_reconnect_during_another_of_the_same_actor_starts_nothing_more() {
        let id = "mock_monitor.0";
        let (_dir, state, mut rx, web, shutdown) = slow_to_close(Duration::from_millis(800));
        let old = state.actor_thread(id);
        web.send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some("first".into()),
            action: Box::new(ConfigAction::Reconnect { id: id.into() }),
        }));
        wait_until("the first is waiting", || state.is_settling(id));
        web.send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some("second".into()),
            action: Box::new(ConfigAction::Reconnect { id: id.into() }),
        }));
        let mut seen = until(&mut rx, "the second", |m| outcome_for(m, "second"));
        assert!(
            !seen.iter().any(|m| outcome_for(m, "first")),
            "answered during the first's wait"
        );
        let (restarted, stopped, started) = outcome_of(&seen, "second");
        assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
        assert!(state.is_settling(id), "the first still waits");
        assert_eq!(state.actor_thread(id), old, "nothing started beside it");

        seen.extend(until(&mut rx, "the first", |m| outcome_for(m, "first")));
        assert_eq!(outcome_of(&seen, "first").0, [id]);
        std::thread::sleep(Duration::from_millis(200));
        seen.extend(std::iter::from_fn(|| rx.try_recv().ok()));
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        let card = statuses(&seen, id);
        let starts = card.iter().filter(|s| **s == ActorStatus::Starting).count();
        assert_eq!(starts, 1, "{card:?}");
        assert!(!state.is_settling(id));
        assert_ne!(state.actor_thread(id), old, "one new instance runs");
        stop_all(&state);
    }

    /// An actor that ends `holds` after its stop and may be waited for as
    /// long as an ExPutt.
    struct SlowToClose {
        holds: Duration,
    }

    impl Actor for SlowToClose {
        fn start(
            &self,
            _state: Arc<SystemState>,
            _sender: BusSender,
            receiver: BusReceiver,
        ) -> std::thread::JoinHandle<()> {
            let holds = self.holds;
            std::thread::spawn(move || {
                while !receiver.is_shutdown() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(holds);
            })
        }

        fn stop_wait(&self) -> Duration {
            crate::actors::MAX_STOP_WAIT
        }

        fn reconfigure(
            &self,
            _state: &Arc<SystemState>,
            _sender: &BusSender,
        ) -> ReconfigureOutcome {
            ReconfigureOutcome::RestartRequired
        }
    }

    /// The running system actor with `mock_monitor.0` a [`SlowToClose`]
    /// holding for `holds`, and a sender for commands and game events.
    fn slow_to_close(
        holds: Duration,
    ) -> (
        TempDir,
        Arc<SystemState>,
        broadcast::Receiver<FlighthookMessage>,
        BusSender,
        Arc<AtomicBool>,
    ) {
        let (dir, state, bus_tx, rx) = running(|c| {
            c.mock_monitor.insert("0".into(), mock("Mock"));
        });
        state.stop_and_wait("mock_monitor.0");
        start_actor(
            "mock_monitor.0".into(),
            Box::new(SlowToClose { holds }),
            &state,
            &bus_tx,
        );
        let (_, writer) = SystemState::new(dir.0.join("unused.toml"));
        let shutdown = Arc::new(AtomicBool::new(false));
        start_for_test(writer, &state, &bus_tx, Arc::clone(&shutdown));
        let web = BusSender::new("web".into(), bus_tx, Arc::default());
        (dir, state, rx, web, shutdown)
    }

    fn outcome_for(m: &FlighthookMessage, request: &str) -> bool {
        matches!(&m.event, FlighthookEvent::ConfigOutcome { request_id: Some(r), .. } if r == request)
    }

    /// While a reconnect waits for an actor that takes an ExPutt-length
    /// stop, the system actor goes on: a club change is applied at once,
    /// and the reconnect still completes once the old instance has ended.
    #[test]
    fn a_club_change_during_a_long_reconnect_is_applied_at_once() {
        let id = "mock_monitor.0";
        let holds = Duration::from_millis(1500);
        let (_dir, state, mut rx, web, shutdown) = slow_to_close(holds);
        let at = std::time::Instant::now();
        web.send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some("reconnect".into()),
            action: Box::new(ConfigAction::Reconnect { id: id.into() }),
        }));
        wait_until("the reconnect is waiting", || state.is_settling(id));
        web.send(FlighthookMessage::new(FlighthookEvent::ClubInfo {
            club_info: ClubInfo {
                club: Club::PitchingWedge,
            },
        }));
        let seen = until(&mut rx, "the club's mode command", |m| {
            m.actor == "system"
                && matches!(
                    m.event,
                    FlighthookEvent::SetDetectionMode { mode: Some(_), .. }
                )
        });
        assert!(at.elapsed() < holds, "applied during the wait");
        assert!(
            !seen.iter().any(|m| outcome_for(m, "reconnect")),
            "before the reconnect finished"
        );
        assert!(state.is_settling(id));

        let done = until(&mut rx, "the reconnect", |m| outcome_for(m, "reconnect"));
        assert!(at.elapsed() >= holds, "it waited for the old instance");
        assert_eq!(outcome_of(&done, "reconnect").0, [id]);
        assert!(!state.is_settling(id));
        assert!(state.actor_thread(id).is_some(), "the new instance runs");
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        stop_all(&state);
    }

    /// A save of an actor a reconnect is still waiting for follows the
    /// closing rules: answered at once, it starts nothing itself, and the
    /// reconnect starts one instance from the saved settings.
    #[test]
    fn a_save_during_a_long_reconnect_follows_the_closing_rules() {
        let id = "mock_monitor.0";
        let holds = Duration::from_millis(1200);
        let (_dir, state, mut rx, web, shutdown) = slow_to_close(holds);
        let at = std::time::Instant::now();
        web.send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some("reconnect".into()),
            action: Box::new(ConfigAction::Reconnect { id: id.into() }),
        }));
        wait_until("the reconnect is waiting", || state.is_settling(id));
        let old = state.actor_thread(id);
        web.send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some("save".into()),
            action: Box::new(ConfigAction::UpsertMockMonitor {
                index: "0".into(),
                section: mock("Renamed"),
            }),
        }));
        let mut seen = until(&mut rx, "the save", |m| outcome_for(m, "save"));
        assert!(at.elapsed() < holds, "answered during the wait");
        let (restarted, stopped, started) = outcome_of(&seen, "save");
        assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
        assert_eq!(state.actor_thread(id), old, "nothing started beside it");
        assert_eq!(state.system.snapshot().mock_monitor["0"].name, "Renamed");

        seen.extend(until(&mut rx, "the reconnect", |m| {
            outcome_for(m, "reconnect")
        }));
        assert_eq!(outcome_of(&seen, "reconnect").0, [id]);
        std::thread::sleep(Duration::from_millis(200));
        seen.extend(std::iter::from_fn(|| rx.try_recv().ok()));
        let starts = statuses(&seen, id)
            .into_iter()
            .filter(|s| *s == ActorStatus::Starting)
            .count();
        assert_eq!(starts, 1, "one new instance");
        assert_ne!(state.actor_thread(id), old);
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        stop_all(&state);
    }

    /// A removal of an actor a reconnect is still waiting for lists it as
    /// stopped at once and silences the old instance; the reconnect then
    /// starts nothing.
    #[test]
    fn a_removal_during_a_long_reconnect_starts_nothing() {
        let id = "mock_monitor.0";
        let (_dir, state, mut rx, web, shutdown) = slow_to_close(Duration::from_millis(800));
        web.send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some("reconnect".into()),
            action: Box::new(ConfigAction::Reconnect { id: id.into() }),
        }));
        wait_until("the reconnect is waiting", || state.is_settling(id));
        web.send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some("remove".into()),
            action: Box::new(ConfigAction::Remove { id: id.into() }),
        }));
        let mut seen = until(&mut rx, "the removal", |m| outcome_for(m, "remove"));
        assert_eq!(outcome_of(&seen, "remove").1, [id]);
        seen.extend(until(&mut rx, "the reconnect", |m| {
            outcome_for(m, "reconnect")
        }));
        let (restarted, stopped, started) = outcome_of(&seen, "reconnect");
        assert!(restarted.is_empty() && stopped.is_empty() && started.is_empty());
        assert!(state.actor_ids().iter().all(|a| a != id));
        assert!(!statuses(&seen, id).contains(&ActorStatus::Starting));
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        stop_all(&state);
    }

    // ----- Stopping each kind of connection -----

    /// Stop `id` and wait for it, as a reconnect does. Returns how long it
    /// took.
    fn stop_timed(state: &SystemState, id: &str) -> Duration {
        let at = std::time::Instant::now();
        assert_eq!(state.stop_and_wait(id), Stopped::Exited, "{id} ended");
        at.elapsed()
    }

    /// Well under the default stop wait: the actor did not sit out a
    /// timeout.
    const PROMPT: Duration = Duration::from_secs(1);

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_mevo_waiting_on_a_silent_device_stops_at_once() {
        let device = Simulator::spawn();
        let (_dir, state, _bus_tx, _rx) = running(|c| {
            let mut section: flighthook::MevoSection =
                serde_json::from_str("{}").expect("empty section");
            section.address = Some(device.addr.to_string());
            c.mevo.insert("0".into(), section);
        });
        // Connected, its handshake unanswered.
        wait_until("connected", || !device.peers().is_empty());
        std::thread::sleep(Duration::from_millis(200));
        let took = stop_timed(&state, "mevo.0");
        assert!(took < PROMPT, "{took:?}");
    }

    #[test]
    fn an_open_connect_client_on_a_silent_simulator_stops_at_once_and_unregisters() {
        let sim = Simulator::spawn();
        let (_dir, state, _bus_tx, mut rx) = running(|c| {
            c.openconnect_client.insert(
                "0".into(),
                flighthook::OpenConnectClientSection {
                    name: "GSPro".into(),
                    address: Some(sim.addr.to_string()),
                    ..flighthook::OpenConnectClientSection::default()
                },
            );
        });
        let id = "openconnect_client.0";
        until(&mut rx, "connected", |m| connected(m, id));
        let peer = sim.peers()[0];
        let took = stop_timed(&state, id);
        assert!(took < PROMPT, "{took:?}");
        assert_eq!(state.local_clients.client_at(peer), None);
    }

    #[test]
    fn an_open_connect_client_before_its_verdict_stops_at_once() {
        let sim = Simulator::spawn();
        let (_dir, state, _bus_tx, _rx) = running(|c| {
            c.openconnect_client.insert(
                "0".into(),
                flighthook::OpenConnectClientSection {
                    name: "GSPro".into(),
                    address: Some(sim.addr.to_string()),
                    ..flighthook::OpenConnectClientSection::default()
                },
            );
        });
        // Inside its first second, before it reports itself connected.
        wait_until("connected", || !sim.peers().is_empty());
        let took = stop_timed(&state, "openconnect_client.0");
        assert!(took < PROMPT, "{took:?}");
    }

    fn free_port() -> std::net::SocketAddr {
        std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port")
    }

    #[test]
    fn an_open_connect_server_with_a_silent_monitor_stops_at_once_and_frees_its_port() {
        use std::io::Read as _;
        let bind = free_port();
        let (_dir, state, _bus_tx, mut rx) = running(|c| {
            c.openconnect_server.insert(
                "0".into(),
                flighthook::OpenConnectServerSection {
                    name: "Uneekor".into(),
                    bind: Some(bind.to_string()),
                },
            );
        });
        let id = "openconnect_server.0";
        let mut monitor = loop {
            if let Ok(stream) = std::net::TcpStream::connect(bind) {
                break stream;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        until(&mut rx, "monitor connected", |m| connected(m, id));
        let took = stop_timed(&state, id);
        assert!(took < PROMPT, "{took:?}");
        monitor
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        assert_eq!(monitor.read(&mut [0; 16]).ok(), Some(0), "closed");
        assert!(std::net::TcpListener::bind(bind).is_ok(), "port released");
    }

    #[test]
    fn an_open_connect_server_waiting_to_bind_stops_at_once() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let bind = taken.local_addr().expect("addr");
        let (_dir, state, _bus_tx, mut rx) = running(|c| {
            c.openconnect_server.insert(
                "0".into(),
                flighthook::OpenConnectServerSection {
                    name: "Uneekor".into(),
                    bind: Some(bind.to_string()),
                },
            );
        });
        until(&mut rx, "bind failed", |m| {
            matches!(&m.event, FlighthookEvent::Alert { message, .. }
                if message.contains("could not bind"))
        });
        // In its wait before binding again.
        let took = stop_timed(&state, "openconnect_server.0");
        assert!(took < PROMPT, "{took:?}");
    }

    /// A stand-in for an Open Connect server on `bind` whose thread still
    /// owns the listener: it lets go of the port `port_held` after its stop,
    /// and ends `thread_lingers` after that. Its replaceable probe is the
    /// real server's.
    struct HeldPort {
        bind: std::net::SocketAddr,
        listener: Mutex<Option<std::net::TcpListener>>,
        port_held: Duration,
        thread_lingers: Duration,
    }

    impl Actor for HeldPort {
        fn start(
            &self,
            _state: Arc<SystemState>,
            sender: BusSender,
            receiver: BusReceiver,
        ) -> std::thread::JoinHandle<()> {
            let listener = self.listener.lock().expect("listener").take();
            let (held, lingers) = (self.port_held, self.thread_lingers);
            std::thread::spawn(move || {
                while !receiver.is_shutdown() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(held);
                drop(listener);
                std::thread::sleep(lingers);
                sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
                    severity: Severity::Warn,
                    message: "late".into(),
                }));
            })
        }

        fn stop_wait(&self) -> Duration {
            STUBBORN_WAIT
        }

        fn replaceable_while_stopping(&self) -> bool {
            crate::actors::openconnect::OpenConnectServerActor { bind: self.bind }
                .replaceable_while_stopping()
        }

        fn reconfigure(
            &self,
            _state: &Arc<SystemState>,
            _sender: &BusSender,
        ) -> ReconfigureOutcome {
            ReconfigureOutcome::RestartRequired
        }
    }

    /// Running `openconnect_server.0` configured on a free port, with a
    /// [`HeldPort`] in place of the server.
    fn with_held_port(
        port_held: Duration,
        thread_lingers: Duration,
    ) -> (
        TempDir,
        Arc<SystemState>,
        broadcast::Sender<FlighthookMessage>,
        broadcast::Receiver<FlighthookMessage>,
        std::net::SocketAddr,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let bind = listener.local_addr().expect("addr");
        let (dir, state, bus_tx, rx) = running(|c| {
            c.openconnect_server.insert(
                "0".into(),
                flighthook::OpenConnectServerSection {
                    name: "Uneekor".into(),
                    bind: Some(bind.to_string()),
                },
            );
        });
        // `running` started the real server, which found the port taken:
        // put the stand-in in its place.
        state.stop_and_wait("openconnect_server.0");
        start_actor(
            "openconnect_server.0".into(),
            Box::new(HeldPort {
                bind,
                listener: Mutex::new(Some(listener)),
                port_held,
                thread_lingers,
            }),
            &state,
            &bus_tx,
        );
        (dir, state, bus_tx, rx, bind)
    }

    /// An Open Connect server whose old thread still holds the listen port
    /// through the stop wait cannot be replaced: the reconnect says the old
    /// connection is still closing, and the server starts by itself, on the
    /// same port, once the port is free and the thread has ended.
    #[test]
    fn an_open_connect_server_holding_its_port_is_restarted_when_it_lets_go() {
        let id = "openconnect_server.0";
        let (dir, state, bus_tx, mut rx, bind) =
            with_held_port(Duration::from_millis(600), Duration::ZERO);
        let (_, writer) = SystemState::new(dir.0.join("unused.toml"));
        let shutdown = Arc::new(AtomicBool::new(false));
        start_for_test(writer, &state, &bus_tx, Arc::clone(&shutdown));
        let web = BusSender::new("web".into(), bus_tx.clone(), Arc::default());
        web.send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
            request_id: Some("press".into()),
            action: Box::new(ConfigAction::Reconnect { id: id.into() }),
        }));
        let seen = until(&mut rx, "the outcome", |m| outcome_for(m, "press"));
        assert!(outcome_of(&seen, "press").0.is_empty(), "not started yet");
        assert!(still_closing_reported(&seen, id));
        assert!(state.is_closing(id));
        assert_eq!(state.actor_thread(id), None, "nothing started beside it");
        assert!(
            std::net::TcpListener::bind(bind).is_err(),
            "the old thread still owns the port"
        );

        let done = until(&mut rx, "the restart", |m| {
            matches!(&m.event, FlighthookEvent::ConfigOutcome { request_id: None, restarted, .. }
                if restarted == &[id.to_owned()])
        });
        assert!(error_cleared(&done, id), "the error is cleared");
        assert!(!late(&done), "the old instance is muted");
        wait_until("the new server listens", || {
            std::net::TcpStream::connect(bind).is_ok()
        });
        assert!(!state.is_closing(id));
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        stop_all(&state);
    }

    /// Once the old thread has let go of the port, a server that has not
    /// ended yet is replaced at once and its last word is muted.
    #[test]
    fn an_open_connect_server_that_freed_its_port_is_replaced_at_once() {
        let id = "openconnect_server.0";
        let (_dir, state, bus_tx, mut rx, bind) =
            with_held_port(Duration::ZERO, Duration::from_millis(600));
        let old = state.actor_thread(id);

        let (restarted, seen) = reconnect_through_command(&state, &bus_tx, &mut rx, id);
        assert_eq!(restarted, [id]);
        assert!(!still_closing_reported(&seen, id));
        assert_ne!(state.actor_thread(id), old);
        assert!(!state.is_closing(id));
        wait_until("the new server listens", || {
            std::net::TcpStream::connect(bind).is_ok()
        });

        std::thread::sleep(Duration::from_millis(700));
        let after: Vec<FlighthookMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(!late(&after), "the replaced instance is muted");
        stop_all(&state);
    }

    #[test]
    fn mock_actors_stop_at_once() {
        let (_dir, state, _bus_tx, _rx) = running(|c| {
            c.mock_monitor.insert("0".into(), mock("Mock"));
            c.random_club.insert(
                "0".into(),
                flighthook::RandomClubSection {
                    name: "Random club".into(),
                },
            );
        });
        for id in ["mock_monitor.0", "random_club.0"] {
            let took = stop_timed(&state, id);
            assert!(took < PROMPT, "{id}: {took:?}");
        }
    }
}
