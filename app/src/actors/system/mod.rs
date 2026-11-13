//! System actor: the always-on actor that hosts flighthook's session and
//! practice actor.
//!
//! The session owns `golfer.turn` (golfer, club, hand, mode): it applies
//! every `golfer.select` (from a simulator card, the UI, the REST API, an
//! FRP peer), publishes the turn when it changes, with the select's sender
//! as its cause, replies to the select after the turn, and republishes the
//! turn on `bus.sync`. Only a select that carries `mode` changes the mode.
//!
//! The practice actor ([`crate::state::practice::Practice`]) follows the
//! turn, every shot's trigger, data and finish, and `practice.new_block`,
//! and publishes the `practice.block`s.
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
use crate::bus::{BusMsg, BusReceiver, BusSender, LocalEvent, PollError};
use crate::state::config::{FlighthookConfig, canonical_id};
use crate::state::practice::{BlockChange, Practice, now_ms};
use crate::state::remotes::{EXPIRY_TICK, Remotes};
use crate::state::{SessionWriter, Stopped, SystemState};
use flighthook::{ActorStatus, Severity};
use flightrelay::types::roles;
use flightrelay::{BusActor, BusReply, DEFAULT_HEARTBEAT, FrpBody, GolferProfile};

mod config_commands;
mod practice;
mod reload;
mod restart;
mod session;
mod shot_type;
#[cfg(test)]
mod tests;

pub(crate) use config_commands::*;
pub(crate) use practice::*;
pub(crate) use reload::*;
pub(crate) use restart::*;
pub(crate) use session::*;
pub(crate) use shot_type::*;

/// System actor. Always-on internal housekeeping — not config-driven.
///
/// Holds the sole `SessionWriter`, enforcing that all game state mutations
/// flow through bus events processed here. Also processes `ConfigCommand`
/// events for config mutations.
pub struct SystemActor {
    writer: Mutex<Option<SessionWriter>>,
    state: Mutex<Option<Arc<SystemState>>>,
    bus_tx: Mutex<Option<broadcast::Sender<BusMsg>>>,
    ready_tx: Mutex<Option<std_mpsc::SyncSender<()>>>,
}

impl SystemActor {
    pub fn new(
        writer: SessionWriter,
        state: Arc<SystemState>,
        bus_tx: broadcast::Sender<BusMsg>,
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

        // The flight model (the Simulation card) runs beside the system
        // actor and stops with it.
        let model = sender.for_card(
            crate::actors::flight_model::CARD,
            Some(sys_state.flight_model_id()),
        );
        let model_rx = model.subscribe();
        crate::actors::flight_model::start(Arc::clone(&sys_state), model, model_rx);
        std::thread::Builder::new()
            .name("system".into())
            .spawn(move || run(writer, sys_state, bus_tx, sender, receiver, ready_tx))
            .expect("failed to spawn system thread")
    }
}

fn run(
    writer: SessionWriter,
    state: Arc<SystemState>,
    bus_tx: broadcast::Sender<BusMsg>,
    sender: BusSender,
    mut receiver: BusReceiver,
    ready_tx: std_mpsc::SyncSender<()>,
) {
    // The session's actor, its first turn and the golfers, then the
    // practice actor's first block.
    publish_session(&writer, &sender);
    let mut profiles = Profiles::default();
    profiles.sync(&state.system.snapshot(), &sender);
    let (mut practice, first) = Practice::with_config(state.system.view(), writer.turn(), now_ms());
    publish_blocks(first, &sender);
    // Signal main thread that we're up and polling.
    let _ = ready_tx.send(());
    drop(ready_tx);

    let mut waits = StopWaits::new();
    let mut remotes = Remotes::new();
    let mut shot_type = ShotTypeReset::new(&state.system.snapshot());
    let mut next_expiry = Instant::now() + EXPIRY_TICK;
    loop {
        // An old instance found still closing has ended: start it again.
        restart_closed(&state, &bus_tx, &sender);
        // A command's stop waits are over: carry it out.
        waits.finish_done(&state, &bus_tx, &sender);
        match receiver.poll() {
            Err(PollError::Shutdown) => return,
            Ok(None) => {
                // Judged only with the bus drained, so heartbeats that
                // waited out a stall count first; a look that comes late
                // after a stall judges no one (`Remotes::check`); the next
                // look is a tick after this one, never a burst of missed
                // ones.
                let now = Instant::now();
                if now >= next_expiry {
                    expire_remotes(&state, &mut remotes, &sender, now, next_expiry);
                    next_expiry = now + EXPIRY_TICK;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(Some(msg)) => match follow_remote(&state, &mut remotes, &msg) {
                Some((_, event @ LocalEvent::ConfigCommand { .. })) => {
                    handle_config_command(event, &state, &bus_tx, &sender, &mut waits);
                    after_config_command(&state, &writer, &mut profiles, &mut practice, &sender);
                    shot_type.check(&state, &writer, &sender);
                }
                // The clips belong to the shots being cleared.
                Some((_, LocalEvent::ClearShotHistory)) => clear_clips(&state),
                _ => {
                    if shot_type.observe(&msg) {
                        shot_type.check(&state, &writer, &sender);
                    }
                    handle_select(&msg, &state, &writer, &mut profiles, &sender);
                    follow_game_golfers(&msg, &state, &writer, &sender);
                    if msg
                        .as_frp()
                        .is_some_and(|m| matches!(m.body, FrpBody::BusSync(_)))
                    {
                        profiles.publish_all(&sender);
                        resend_output_actors(&state, &bus_tx);
                    }
                    handle_practice(&msg, &mut practice, &sender);
                }
            },
        }
    }
}

/// The running output cards' `bus.actor`s again, for a `bus.sync`: the
/// output actors themselves do not answer it.
pub(crate) fn resend_output_actors(state: &SystemState, bus_tx: &broadcast::Sender<BusMsg>) {
    state.beats.send_again(bus_tx, |from| {
        state
            .card_of_actor(from)
            .is_some_and(|card| crate::actors::is_output_card(&card) && state.is_live_local(from))
    });
}

/// Keep the remote actors' heartbeat clocks and the round policy's record
/// of each remote launch monitor's declared source from a message
/// that came in on a hub link. Returns the message's local event, if any.
pub(crate) fn follow_remote<'a>(
    state: &SystemState,
    remotes: &mut Remotes,
    msg: &'a BusMsg,
) -> Option<(&'a str, &'a LocalEvent)> {
    if let BusMsg::Frp {
        msg: frp,
        peer: Some(_),
        ..
    } = msg
    {
        remotes.hear(frp, Instant::now());
        match &frp.body {
            FrpBody::BusActor(actor) if actor.has_role(roles::LAUNCH_MONITOR) => {
                let source = actor.device.as_ref().and_then(|d| d.source);
                state.round.set_remote(&frp.from, source);
            }
            FrpBody::Removed(r) if r.kind == flightrelay::StateKind::BusActor => {
                state.round.forget_remote(&frp.from);
            }
            _ => {}
        }
    }
    msg.as_local()
}

/// The look due at `due`, made at `now`: forget every remote actor silent
/// past its heartbeat ([`Remotes::check`]) and announce each on the bus as
/// [`LocalEvent::ActorGone`].
pub(crate) fn expire_remotes(
    state: &SystemState,
    remotes: &mut Remotes,
    sender: &BusSender,
    now: Instant,
    due: Instant,
) {
    for actor in remotes.check(now, due) {
        tracing::info!("bus: actor {actor} went silent, forgotten");
        state.round.forget_remote(&actor);
        sender.local(LocalEvent::ActorGone { actor });
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
        sender.local(LocalEvent::ShotVideoRemoved { keys: removed });
    }
}

/// Start a system actor on `bus_tx` holding `writer`, as `main` does, and
/// wait until it polls the bus. It stops when `shutdown` is set.
#[cfg(test)]
pub(crate) fn start_for_test(
    writer: SessionWriter,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    shutdown: Arc<AtomicBool>,
) {
    let sender = BusSender::new("system".into(), bus_tx.clone(), shutdown)
        .with_frp_id(Some(state.flighthook_id()))
        .with_beats(state.beats.clone());
    let receiver = sender.subscribe();
    let (actor, ready) = SystemActor::new(writer, Arc::clone(state), bus_tx.clone());
    actor.start(Arc::clone(state), sender, receiver);
    ready.recv().expect("system actor up");
}
