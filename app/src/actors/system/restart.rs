//! The shared restart rules (release a stopped actor, start it fresh or report it still closing)
//! and the Reconnect button's checks and plan.

use super::*;

/// The `error` on a card whose old connection outlasted its stop wait.
pub(crate) const STILL_CLOSING: &str =
    "The previous connection has not closed yet. It reconnects when it does.";

/// Send the `bus.actor` removal of card `card`'s muted instance in its
/// name: the actor id its last `bus.actor` went out under, else its
/// card's.
pub(super) fn send_removal_for(
    state: &SystemState,
    bus_tx: &broadcast::Sender<BusMsg>,
    card: &str,
) {
    let Some(actor) = state
        .beats
        .forget_card(card)
        .or_else(|| state.system.actor_of_card(card))
    else {
        return;
    };
    let _ = bus_tx.send(BusMsg::frp(flightrelay::FrpMessage::new(
        actor,
        flightrelay::Removal::of(flightrelay::StateKind::BusActor),
    )));
}

/// What became of an actor that was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Released {
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
pub(super) fn stop_together(state: &SystemState, ids: &[String]) -> Vec<(String, Instant)> {
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
pub(super) fn release(state: &SystemState, id: &str, deadline: Instant) -> Released {
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
pub(super) fn start_released(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
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
            card.local(LocalEvent::CardStatus {
                status: ActorStatus::Disconnected,
                telemetry: HashMap::from([("error".into(), STILL_CLOSING.into())]),
            });
            card.local(LocalEvent::Alert {
                severity: Severity::Error,
                message: STILL_CLOSING.into(),
            });
            false
        }
    }
}

/// Start `fresh` as a new instance of its actor. Its card starts over:
/// `Starting`, without an error an earlier instance left.
pub(super) fn start_fresh(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    fresh: ResolvedActor,
) {
    let card = BusSender::new(fresh.id.clone(), bus_tx.clone(), Arc::default());
    card.local(LocalEvent::CardStatus {
        status: ActorStatus::Starting,
        telemetry: HashMap::from([("error".into(), String::new())]),
    });
    start_actor(fresh.id, fresh.actor, state, bus_tx);
}

/// Start again every actor whose old instance was still closing and has
/// ended since, from the current settings, and emit one `ConfigOutcome`
/// (no `request_id`) listing them as restarted. An actor removed meanwhile
/// is not started. SystemActor runs this between bus messages.
pub(crate) fn restart_closed(
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    sender: &BusSender,
) {
    let closed = state.take_closed();
    if closed.is_empty() {
        return;
    }
    let mut resolved = resolve_actors(&state.system.snapshot());
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
        sender.local(LocalEvent::ConfigOutcome {
            request_id: None,
            restarted,
            stopped: Vec::new(),
            started: Vec::new(),
        });
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
/// already restarting it, turned off, or its settings cannot start it.
pub(super) fn reconnect_check(state: &SystemState, id: &str) -> Result<ResolvedActor, String> {
    let config = state.system.snapshot();
    let id = reconnect_target(&config, id).map_err(|r| r.message().to_owned())?;
    if state.is_closing(&id) {
        return Err(STILL_CLOSING.into());
    }
    if state.is_settling(&id) {
        return Err(format!("'{id}' is already restarting"));
    }
    if !config.actor_enabled(&id) {
        return Err(format!("'{id}' is off"));
    }
    resolve_actors(&config)
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
pub(super) fn reconnect(
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
