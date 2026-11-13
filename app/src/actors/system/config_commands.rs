//! Config commands: apply a `ConfigCommand` (persist, reconcile actors, turn off clashing
//! cards) and what follows it (golfers, practice blocks, body tracking).

use super::*;

/// After a config command: follow the golfers ([`follow_golfers`]) and key
/// the practice actor's open blocks again by the saved golfers
/// ([`Practice::rekey`]), publishing what that changed.
pub(crate) fn after_config_command(
    state: &SystemState,
    writer: &SessionWriter,
    profiles: &mut Profiles,
    practice: &mut Practice,
    sender: &BusSender,
) {
    follow_golfers(state, writer, profiles, sender);
    publish_blocks(practice.rekey(now_ms()), sender);
}

/// Apply config command `event`. The actors it stops are waited for on a
/// helper thread (`waits`); its `ConfigOutcome` goes out once they have
/// ended and what it starts has started, at once when it stops nothing.
/// Body tracking then follows its mode ([`crate::pose::BodyTracking::follow`]).
pub(crate) fn handle_config_command(
    event: &LocalEvent,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    sender: &BusSender,
    waits: &mut StopWaits,
) {
    apply_config_command(event, state, bus_tx, sender, waits);
    state.pose.follow(&state.system.body_tracking());
    state.round.set_manual(state.system.simulation().tournament);
}

/// [`handle_config_command`] but for body tracking.
pub(super) fn apply_config_command(
    event: &LocalEvent,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    sender: &BusSender,
    waits: &mut StopWaits,
) {
    use flighthook::ConfigAction;

    let LocalEvent::ConfigCommand {
        ref request_id,
        ref action,
    } = *event
    else {
        return;
    };

    let clip_dir = crate::state::clips::clip_dir(&state.system.replay());

    // Determine scope for actor reconciliation
    let scope: Option<String>;
    let before = state.system.snapshot();

    match action.as_ref() {
        ConfigAction::ReplaceAll { config } => {
            // Only `SetTournament` writes tournament mode: a whole-config
            // save keeps the live value whatever its body carries.
            let mut config = (**config).clone();
            config.simulation.tournament = before.simulation.tournament;
            state.system.replace(config);
            scope = None;
        }
        ConfigAction::ReplaceGlobal { config } => {
            state.system.update(|p| {
                p.default_units = config.default_units;
                p.shot_grouping = config.shot_grouping;
                // Tournament mode stays as it is (only `SetTournament`
                // writes it).
                let tournament = p.simulation.tournament;
                p.simulation = config.simulation.clone();
                p.simulation.tournament = tournament;
                p.replay = config.replay.clone();
                p.body_tracking = config.body_tracking.clone();
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
        ConfigAction::UpsertTrackball { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.trackball.insert(idx, section.clone());
            });
            scope = Some(format!("trackball.{index}"));
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
        ConfigAction::UpsertPureplay { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.pureplay.insert(idx, section.clone());
            });
            scope = Some(format!("pureplay.{index}"));
        }
        ConfigAction::UpsertPureplayAccessory { index, section } => {
            let idx = index.clone();
            state.system.update(|p| {
                p.pureplay_accessory.insert(idx, section.clone());
            });
            scope = Some(format!("pureplay_accessory.{index}"));
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
                    "trackball" => {
                        p.trackball.remove(&idx);
                    }
                    "openconnect_server" => {
                        p.openconnect_server.remove(&idx);
                    }
                    "openconnect_client" => {
                        p.openconnect_client.remove(&idx);
                    }
                    "pureplay" => {
                        p.pureplay.remove(&idx);
                    }
                    "pureplay_accessory" => {
                        p.pureplay_accessory.remove(&idx);
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
            sender.local(LocalEvent::ConfigOutcome {
                request_id: request_id.clone(),
                restarted: Vec::new(),
                stopped: Vec::new(),
                started: Vec::new(),
            });
            return;
        }
        ConfigAction::SetBodyTracking { section } => {
            // Body tracking is read live everywhere: persist it and
            // reconcile nothing.
            state.system.update(|p| p.body_tracking = section.clone());
            sender.local(LocalEvent::ConfigOutcome {
                request_id: request_id.clone(),
                restarted: Vec::new(),
                stopped: Vec::new(),
                started: Vec::new(),
            });
            return;
        }
        ConfigAction::SetTournament { on } => {
            // Persisted, then applied before the outcome goes out, so the
            // caller reads the new round when it hears the outcome.
            let on = *on;
            state.system.update(|p| p.simulation.tournament = on);
            state.round.set_manual(on);
            sender.local(LocalEvent::ConfigOutcome {
                request_id: request_id.clone(),
                restarted: Vec::new(),
                stopped: Vec::new(),
                started: Vec::new(),
            });
            return;
        }
        ConfigAction::SetGolfers { golfers } => {
            // The golfers are read where they are used (the lookup rule):
            // persist them and reconcile nothing. A golfer saved without an
            // id keeps the one already saved for it ([`keep_saved_ids`]).
            // A `selected_golfer` no golfer holds any more is cleared.
            state.system.update(|p| {
                let mut golfers = golfers.clone();
                keep_saved_ids(&p.golfers, &mut golfers);
                p.golfers = golfers;
                if p.selected_golfer
                    .as_deref()
                    .is_some_and(|id| p.golfer_of(id).is_none())
                {
                    p.selected_golfer = None;
                }
            });
            sender.local(LocalEvent::ConfigOutcome {
                request_id: request_id.clone(),
                restarted: Vec::new(),
                stopped: Vec::new(),
                started: Vec::new(),
            });
            return;
        }
        ConfigAction::Reconnect { id } => {
            // The config is unchanged: only this actor is touched. A
            // refused one lists nothing as restarted.
            match reconnect(state, request_id.clone(), id) {
                Ok(plan) => waits.run(state, bus_tx, sender, plan),
                Err(e) => {
                    tracing::warn!("reconnect '{id}': {e}");
                    sender.local(LocalEvent::ConfigOutcome {
                        request_id: request_id.clone(),
                        restarted: Vec::new(),
                        stopped: Vec::new(),
                        started: Vec::new(),
                    });
                }
            }
            return;
        }
        other => {
            tracing::warn!("config command: {other:?} is not handled");
            sender.local(LocalEvent::ConfigOutcome {
                request_id: request_id.clone(),
                restarted: Vec::new(),
                stopped: Vec::new(),
                started: Vec::new(),
            });
            return;
        }
    }

    turn_off_clashing(state, &before);
    follow_clip_dir(state, sender, &clip_dir);

    // Reconcile actors (webserver included — its reconfigure() handles bind changes)
    let plan = apply_config_reload(state, bus_tx, request_id.clone(), scope.as_deref(), &before);
    waits.run(state, bus_tx, sender, plan);
}

/// After a save over `before`: turn off, and save as off, every enabled card
/// whose address another enabled card of its type holds. A card whose
/// address or switch the save changed loses to the cards it did not touch,
/// so editing a card onto a held address turns that card off.
pub(super) fn turn_off_clashing(state: &SystemState, before: &FlighthookConfig) {
    let mut config = state.system.snapshot();
    let clashes = config.demote_clashing(Some(before));
    for clash in &clashes {
        tracing::warn!(
            "'{}' is off: {} ('{}' holds it)",
            clash.id,
            clash.message(),
            clash.holder
        );
    }
    if !clashes.is_empty() {
        state.system.replace(config);
    }
}

/// Apply config command `event` as the system actor does and wait for it
/// to finish, its stop waits included.
#[cfg(test)]
pub(crate) fn run_config_command(
    event: &LocalEvent,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<BusMsg>,
    sender: &BusSender,
) {
    let mut waits = StopWaits::new();
    handle_config_command(event, state, bus_tx, sender, &mut waits);
    waits.settle(state, bus_tx, sender);
}
