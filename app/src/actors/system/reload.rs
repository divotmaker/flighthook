//! Config reload: the plan a config command makes (which actors stop, restart or start)
//! and `StopWaits`, which waits for stopped actors off the system thread and finishes the plan.

use super::*;

/// What a config command does with an actor it touches once the actors it
/// stopped have ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Then {
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
    pub(super) request_id: Option<String>,
    /// Each actor it touches, in order, and what then happens to it.
    pub(super) actors: Vec<(String, Then)>,
    /// The actors it stopped and until when each is waited for.
    pub(super) waits: Vec<(String, Instant)>,
    /// The actors its outcome lists as stopped already: removed while
    /// their old instance closes.
    pub(super) stopped: Vec<String>,
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
/// Other actors are left untouched (no stop/start/reconfigure). A card
/// whose FRP actor id differs from the one it had in `before` restarts.
pub(super) fn apply_config_reload(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    request_id: Option<String>,
    scope: Option<&str>,
    before: &FlighthookConfig,
) -> Plan {
    let snap = state.system.snapshot();
    let resolved = resolve_actors(&snap);
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
        if before.actor_of_card(id) != snap.actor_of_card(id)
            || matches!(
                state.reconfigure_actor(id, state, &sender),
                Some(ReconfigureOutcome::RestartRequired)
            )
        {
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
    pub(super) done_tx: std_mpsc::Sender<u64>,
    pub(super) done_rx: std_mpsc::Receiver<u64>,
    pub(super) waiting: HashMap<u64, Plan>,
    pub(super) next: u64,
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
    pub(super) fn run(
        &mut self,
        state: &Arc<SystemState>,
        bus_tx: &broadcast::Sender<BusMsg>,
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
        bus_tx: &broadcast::Sender<BusMsg>,
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
    pub(super) fn settle(
        &mut self,
        state: &Arc<SystemState>,
        bus_tx: &broadcast::Sender<BusMsg>,
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
pub(super) fn finish(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    sender: &BusSender,
    plan: Plan,
) {
    let mut resolved = resolve_actors(&state.system.snapshot());
    let configured: Vec<String> = resolved.iter().map(|ra| ra.id.clone()).collect();
    let mut restarted = Vec::new();
    // Instances muted before they could say they left: the ones removed
    // while closing, and the ones that outlived their stop wait.
    let mut silenced = plan.stopped.clone();
    let mut stopped = plan.stopped;
    let mut started = Vec::new();
    for (id, then) in plan.actors {
        let released = match then {
            Then::Remove | Then::Restart => release(state, &id, Instant::now()),
            Then::Start => Released::Free,
        };
        if !matches!(released, Released::Free) {
            silenced.push(id.clone());
            // Still configured: the muted instance's removal goes out for
            // it now, so no output keeps its last `bus.actor` while the new
            // instance waits or starts (it announces itself again).
            if configured.contains(&id) {
                send_removal_for(state, bus_tx, &id);
            }
        }
        state.end_settling(&id);
        if then == Then::Remove {
            if state.system.snapshot().actor_enabled(&id) {
                tracing::info!("config reload: stopped removed actor '{id}'");
            } else {
                tracing::info!("config reload: stopped '{id}', turned off");
            }
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
    // A shot source turned off or removed feeds no output. A muted
    // instance's own removal was dropped: it goes out for it.
    let snap = state.system.snapshot();
    for id in stopped.iter().filter(|id| !configured.contains(id)) {
        let actor = state
            .round
            .forget_card(id)
            .or_else(|| snap.actor_of_card(id))
            .or_else(|| state.beats.actor_of_card(id));
        if (silenced.contains(id) || crate::actors::is_output_card(id))
            && let Some(actor) = actor
        {
            let _ = bus_tx.send(BusMsg::frp(flightrelay::FrpMessage::new(
                actor,
                flightrelay::Removal::of(flightrelay::StateKind::BusActor),
            )));
        }
    }
    sender.local(LocalEvent::ConfigOutcome {
        request_id: plan.request_id,
        restarted,
        stopped,
        started,
    });
}
