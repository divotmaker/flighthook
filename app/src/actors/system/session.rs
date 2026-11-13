//! The session's part of the system actor: `golfer.select` handling and the golfer turn,
//! flighthook's own picks saved, golfer profiles followed and published.

use super::*;

/// [`handle_session_with`] with nothing before the reply.
#[cfg(test)]
pub(crate) fn handle_session(msg: &BusMsg, writer: &SessionWriter, sender: &BusSender) {
    handle_session_with(msg, writer, sender, || {});
}

/// The session's part of the system actor with flighthook's own picks
/// saved: [`handle_session_with`], where a `golfer.select` from flighthook itself
/// is saved ([`save_own_pick`]) and the golfers followed
/// ([`follow_golfers`]) after the turn and before the `bus.reply`, so the
/// profile the pick changed goes out before the answer.
pub(crate) fn handle_select(
    msg: &BusMsg,
    state: &SystemState,
    writer: &SessionWriter,
    profiles: &mut Profiles,
    sender: &BusSender,
) {
    handle_session_with(msg, writer, sender, || {
        if save_own_pick(msg, state, sender) {
            follow_golfers(state, writer, profiles, sender);
        }
    });
}

/// The session's part of the system actor: publish the turn when a
/// `golfer.select` changed it, with the select's sender as its cause, run
/// `before_reply`, then answer the select with `bus.reply`; republish the
/// turn, with the cause of the select that produced it, on `bus.sync`.
pub(super) fn handle_session_with(
    msg: &BusMsg,
    writer: &SessionWriter,
    sender: &BusSender,
    before_reply: impl FnOnce(),
) {
    let Some(frp) = msg.as_frp() else {
        return;
    };
    match &frp.body {
        FrpBody::GolferSelect(select) => {
            if let Some(turn) = writer.apply_from(&frp.from, select) {
                sender.frp_caused(turn, Some(frp.from.clone()));
            }
            before_reply();
            sender.frp(BusReply::ok(
                frp.from.clone(),
                "golfer.select",
                select.r#ref.clone(),
            ));
        }
        FrpBody::BusSync(_) => publish_session(writer, sender),
        _ => {}
    }
}

/// A game golfer up whose `golfer.profile` goes (removed by every actor
/// that published it, their `bus.actor`s removed, their actors gone) leaves
/// the turn, as flighthook: a linked game id becomes its saved golfer with
/// the club and hand kept, any other goes back to the start golfer
/// ([`SessionWriter::follow_profiles`]).
pub(crate) fn follow_game_golfers(
    msg: &BusMsg,
    state: &SystemState,
    writer: &SessionWriter,
    sender: &BusSender,
) {
    let own = state.flighthook_id();
    let turn = if let Some(frp) = msg.as_frp() {
        writer.follow_profiles(&own, frp)
    } else if let Some((_, LocalEvent::ActorGone { actor })) = msg.as_local() {
        writer.actor_gone(&own, actor)
    } else {
        None
    };
    if let Some(turn) = turn {
        sender.frp_caused(turn, Some(own));
    }
}

/// The golfers' `golfer.profile`s as last published, by own id.
#[derive(Default)]
pub(crate) struct Profiles(std::collections::BTreeMap<String, GolferProfile>);

/// Golfer `golfer`'s `golfer.profile` under its own id: name, hand, bag,
/// sex and height (a height out of range left out). `None` without an own
/// id that is a valid golfer id. Game ids are never published.
pub(crate) fn profile_of(golfer: &flighthook::GolferRecord) -> Option<GolferProfile> {
    let id = golfer.own_id()?.parse().ok()?;
    let mut profile = GolferProfile::new(id, golfer.name.clone());
    profile.handedness = golfer.handed;
    profile.bag.clone_from(&golfer.bag);
    profile.sex = golfer.sex.map(|sex| match sex {
        flighthook::Sex::Male => flightrelay::Sex::Male,
        flighthook::Sex::Female => flightrelay::Sex::Female,
    });
    profile.height = golfer
        .facts()
        .height
        .map(flightrelay::units::Distance::Centimeters);
    Some(profile)
}

impl Profiles {
    /// Each golfer's own id and hand as last published: what the session
    /// compares a change of the golfers against.
    pub(crate) fn hands(
        &self,
    ) -> std::collections::BTreeMap<String, Option<flightrelay::Handedness>> {
        self.0
            .iter()
            .map(|(id, p)| (id.clone(), p.handedness))
            .collect()
    }

    /// Publish the profile of every golfer of `config` that is new or
    /// changed since the last publish, and `removed` for every golfer no
    /// longer in it.
    pub(crate) fn sync(&mut self, config: &FlighthookConfig, sender: &BusSender) {
        let now: std::collections::BTreeMap<String, GolferProfile> = config
            .golfers
            .iter()
            .filter_map(profile_of)
            .map(|p| (p.id.as_str().to_owned(), p))
            .collect();
        for (id, profile) in &now {
            if self.0.get(id) != Some(profile) {
                sender.frp(profile.clone());
            }
        }
        for id in self.0.keys().filter(|id| !now.contains_key(*id)) {
            sender.frp(flightrelay::Removal::of_id(
                flightrelay::StateKind::GolferProfile,
                id.clone(),
            ));
        }
        self.0 = now;
    }

    /// Publish every profile again (`bus.sync`).
    pub(crate) fn publish_all(&self, sender: &BusSender) {
        for profile in self.0.values() {
            sender.frp(profile.clone());
        }
    }
}

/// When `msg` is a `golfer.select` from flighthook itself (its UI and
/// API): save the golfer it picked as `selected_golfer` (a golfer of the
/// config) and the hand it gave on the golfer up's record. True when the
/// config changed.
pub(crate) fn save_own_pick(msg: &BusMsg, state: &SystemState, sender: &BusSender) -> bool {
    let Some(frp) = msg.as_frp() else {
        return false;
    };
    let FrpBody::GolferSelect(select) = &frp.body else {
        return false;
    };
    if sender.frp_id() != Some(frp.from.as_str()) {
        return false;
    }
    let config = state.system.snapshot();
    let picked = select
        .golfer
        .as_ref()
        .and_then(|g| g.as_ref())
        .and_then(|id| config.golfer_of(id.as_str()))
        .and_then(|g| g.own_id())
        .map(str::to_owned);
    let turn = state.session.turn();
    let up = turn
        .golfer
        .as_ref()
        .and_then(|id| config.golfers.iter().position(|g| g.is(id.as_str())));
    let hand = select
        .handedness
        .zip(up)
        .filter(|(hand, i)| config.golfers[*i].handed != Some(*hand));
    let select_changes = picked.is_some() && picked != config.selected_golfer;
    if !select_changes && hand.is_none() {
        return false;
    }
    state.system.update(|c| {
        if select_changes {
            c.selected_golfer.clone_from(&picked);
        }
        if let Some((hand, i)) = hand
            && let Some(golfer) = c.golfers.get_mut(i)
        {
            golfer.handed = Some(hand);
        }
    });
    true
}

/// After the golfers may have changed: publish their profiles' changes and
/// let the session follow them ([`SessionWriter::follow_golfers`]), as
/// flighthook.
pub(crate) fn follow_golfers(
    state: &SystemState,
    writer: &SessionWriter,
    profiles: &mut Profiles,
    sender: &BusSender,
) {
    let before = profiles.hands();
    profiles.sync(&state.system.snapshot(), sender);
    let own = state.flighthook_id();
    if let Some(turn) = writer.follow_golfers(&own, &before) {
        sender.frp_caused(turn, Some(own));
    }
}

/// A golfer is given its id once: a golfer in a save (`golfers`) without a
/// `flighthook.*` id, at the position of a saved golfer (`saved`) with the
/// same name whose id no golfer in the save holds, takes that id rather
/// than a new one. A second save of a new golfer whose first save's answer
/// was lost so keeps the first id.
pub(super) fn keep_saved_ids(
    saved: &[flighthook::GolferRecord],
    golfers: &mut [flighthook::GolferRecord],
) {
    for i in 0..golfers.len() {
        if golfers[i].own_id().is_some() {
            continue;
        }
        let Some(old) = saved.get(i) else {
            continue;
        };
        let Some(id) = old.own_id() else {
            continue;
        };
        if old.name.trim() != golfers[i].name.trim() || golfers.iter().any(|g| g.is(id)) {
            continue;
        }
        let id = id.to_owned();
        golfers[i].identifiers.insert(0, id);
    }
}

/// Publish the session's actor and its turn (start and `bus.sync`).
pub(crate) fn publish_session(writer: &SessionWriter, sender: &BusSender) {
    sender.frp(
        BusActor::new("flighthook", DEFAULT_HEARTBEAT.as_secs_f64())
            .with_role(roles::SESSION)
            .with_role(roles::PRACTICE)
            .with_role(roles::COLLECTOR)
            .with_namespace(flighthook::GOLFER_NAMESPACE),
    );
    let cause = writer
        .cause()
        .or_else(|| sender.frp_id().map(str::to_owned));
    sender.frp_caused(writer.turn(), cause);
}
