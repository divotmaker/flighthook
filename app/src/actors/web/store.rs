//! The shot store and its collector: the session's shot history
//! ([`StoredShot`], the raw frame budget), the shots between `shot.trigger`
//! and `shot.finished` ([`PendingShots`]), and the `state_updater` task that
//! drains the bus into them and into the cards' web state.

use super::*;

/// Shots kept in the session history.
pub const MAX_SHOTS: usize = 1000;

/// Memory held by raw frames across the session history, as counted by
/// [`raw_held_bytes`]. Beyond this, the oldest shots lose their raw frames
/// (their decoded data stays).
pub const RAW_BUDGET_BYTES: usize = 16 * 1024 * 1024;

/// Estimated memory per stored frame beyond its hex or text: the record and
/// its metadata strings (type, src, dst, channel) with allocator overhead.
pub const RAW_RECORD_OVERHEAD: usize = 256;

/// Memory a shot's raw frames hold, as charged to [`RAW_BUDGET_BYTES`]: the
/// length of each frame's hex (two characters per payload byte) and text,
/// plus [`RAW_RECORD_OVERHEAD`] per frame.
pub(super) fn raw_held_bytes(raw: &ShotRaw) -> usize {
    raw.frames
        .iter()
        .map(|r| {
            r.hex.as_ref().map_or(0, String::len)
                + r.text.as_ref().map_or(0, String::len)
                + RAW_RECORD_OVERHEAD
        })
        .sum()
}

/// `ShotRaw.unavailable` for a shot whose frames were evicted.
pub const RAW_EVICTED: &str = "evicted: session raw budget";

/// Everything the web layer keeps per shot. `/api/shots` serializes `data`
/// only; the export serializes all of it.
#[derive(Debug, Clone)]
pub struct StoredShot {
    /// When `shot_trigger` reached the web layer.
    pub received_at: DateTime<Utc>,
    pub key: ShotKey,
    /// What was true when the trigger arrived.
    pub context: ExportContext,
    pub data: ShotData,
    pub raw: Option<ShotRaw>,
    /// The shot as composed from its FRP messages, addenda included; `data`
    /// is built from it.
    pub frp: Option<CompletedShot>,
    /// The flight model's path.
    pub path: Option<FlightPath>,
}

impl StoredShot {
    fn raw_len(&self) -> usize {
        self.raw.as_ref().map_or(0, raw_held_bytes)
    }
}

/// Settings and identity in effect when a shot was triggered, for the
/// export.
#[derive(Debug, Clone, Default)]
pub struct ExportContext {
    /// Kind, maker, model and firmware only.
    pub device: ExportDevice,
    pub detection_mode: Option<Mode>,
    /// The actor's config section with `name`, `address` and `bind` removed.
    pub device_section: serde_json::Value,
    pub simulation: SimulationSection,
    /// Identity strings scrubbed from text payloads and device labels on
    /// export: device IDs, configured and peer addresses.
    pub scrub: Vec<String>,
    /// The card key of each local actor at the trigger, by actor id: the
    /// export names local actors by card key.
    pub card_keys: HashMap<String, String>,
}

/// What the web layer keeps for a shot between `shot.trigger` and
/// `shot.finished`, next to its messages in [`PendingShots`].
pub(crate) struct PendingShot {
    received_at: DateTime<Utc>,
    /// The launch monitor's card key (its actor id for a remote one).
    actor: String,
    context: ExportContext,
    /// The session's turn at the trigger, for a shot the accumulator stamped
    /// no turn on.
    turn: Option<flightrelay::GolferTurn>,
    /// The launch monitor arms only for putts (its driver's fixed mode at
    /// the trigger): its shot is a putter shot whatever club is in play.
    putting: bool,
    /// Camera clips that finished before the shot did.
    video: Vec<ShotVideo>,
    /// The game's stroke, when it came before the shot finished.
    stroke: Option<flightrelay::GameStroke>,
    /// The device frames, from the launch monitor card's capture.
    raw: Option<ShotRaw>,
    /// The flight model's path, when it came before the shot finished.
    path: Option<FlightPath>,
    /// When the trigger arrived.
    started: Instant,
}

/// The shots between `shot.trigger` and `shot.finished`: their messages,
/// composed by the SDK's [`ShotAccumulator`] (flighthook's
/// [`SHOT_TIMEOUT`], fed only flighthook's own turn), and the web layer's
/// own per-shot state, by shot id.
pub(crate) struct PendingShots {
    data: ShotAccumulator,
    /// Each shot's launch monitor: only it sends the shot's parts.
    owners: crate::actors::simout::ShotOwners,
    /// The actors whose last `bus.actor` holds the `game` role: only a
    /// game's `game.stroke` joins a shot.
    games: std::collections::HashSet<String>,
    shots: HashMap<String, PendingShot>,
}

impl Default for PendingShots {
    fn default() -> Self {
        Self {
            data: ShotAccumulator::new().with_timeout(SHOT_TIMEOUT),
            owners: crate::actors::simout::ShotOwners::default(),
            games: std::collections::HashSet::new(),
            shots: HashMap::new(),
        }
    }
}

impl PendingShots {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Every pending shot's state, with its shot id.
    fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut PendingShot)> {
        self.shots.iter_mut()
    }

    /// End `actor`'s pending shots (a card key), except the shot `keep` (a
    /// shot id). The accumulator discards each of them as on a timeout, so
    /// their later messages and finish are ignored.
    fn discard_actor(&mut self, actor: &str, keep: Option<&str>) {
        let ended: Vec<String> = self
            .shots
            .iter()
            .filter(|(id, p)| p.actor == actor && Some(id.as_str()) != keep)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ended {
            self.data.discard(&id);
            self.shots.remove(&id);
        }
    }

    /// Discard every open shot of the gone actor `actor_id` (card key
    /// `card`), so nothing it sent before it went is stored.
    fn forget_actor(&mut self, card: &str, actor_id: &str) {
        self.discard_actor(card, None);
        self.data.forget_actor(actor_id);
        self.owners.forget_actor(actor_id);
        self.games.remove(actor_id);
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.shots.is_empty() && self.data.pending_count() == 0
    }

    /// The shot id of every pending shot.
    #[cfg(test)]
    pub(super) fn keys(&self) -> impl Iterator<Item = &String> {
        self.shots.keys()
    }

    /// End the pending shots that have not finished within the shot timeout.
    fn discard_expired(&mut self, now: Instant) {
        self.data.discard_expired();
        let expired: Vec<String> = self
            .shots
            .iter()
            .filter(|(_, p)| now.saturating_duration_since(p.started) > SHOT_TIMEOUT)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.data.discard(&id);
            self.shots.remove(&id);
        }
    }
}

/// Background task that drains the bus and keeps WebState current.
pub(super) async fn state_updater(state: Arc<WebState>, mut bus_rx: broadcast::Receiver<BusMsg>) {
    let mut accumulators = PendingShots::new();
    loop {
        match bus_rx.recv().await {
            Ok(msg) => apply_bus_event(&state, &msg, &mut accumulators).await,
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("web state updater: lagged, dropped {n} events");
            }
        }
    }
}

/// A remote actor is gone ([`LocalEvent::ActorGone`]): forget its state
/// (the UI is told) and its open shots.
fn actor_gone(state: &WebState, actor: &str, accumulators: &mut PendingShots) {
    ui_socket::gone(state, actor);
    accumulators.forget_actor(&stored::card_of(&state.root, actor), actor);
}

/// Whether the launch monitor `from` (actor id) arms only for putts: its
/// driver's fixed mode while its card runs. A remote actor's `bus.actor`
/// carries no fixed mode, so a remote launch monitor never is one.
fn putting_device(state: &WebState, from: &str) -> bool {
    state.root.fixed_mode(from) == Some(Mode::Putting)
}

/// Stable device kind for an actor type prefix.
pub(crate) fn device_kind(prefix: &str) -> &str {
    match prefix {
        "mevo" => "flightscope",
        "square" => "square_golf",
        "exputt" => "exputt",
        "trackball" => "trackball",
        "r10" => "garmin_r10",
        "openconnect_server" => "openconnect",
        "mock_monitor" => "mock",
        "pureplay_accessory" => "pureplay",
        other => other,
    }
}

/// The actor's config section with `name`, `address`, `bind`, a trackball's
/// `device` (a device name, which can carry a serial number), the card's
/// FRP `actor_id` (no shot condition) and unset fields removed, plus the
/// removed address values (to scrub on export).
pub(super) fn device_section(
    config: &FlighthookConfig,
    prefix: &str,
    index: &str,
) -> (serde_json::Value, Vec<String>) {
    fn value<T: serde::Serialize>(section: Option<&T>) -> serde_json::Value {
        section
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()))
    }
    let mut section = match prefix {
        "mevo" => value(config.mevo.get(index)),
        "square" => value(config.square.get(index)),
        "exputt" => value(config.exputt.get(index)),
        "trackball" => value(config.trackball.get(index)),
        "r10" => value(config.r10.get(index)),
        "openconnect_server" => value(config.openconnect_server.get(index)),
        "mock_monitor" => value(config.mock_monitor.get(index)),
        "pureplay_accessory" => value(config.pureplay_accessory.get(index)),
        _ => value::<()>(None),
    };
    let mut removed = Vec::new();
    if let Some(map) = section.as_object_mut() {
        map.retain(|_, v| !v.is_null());
        map.remove("name");
        map.remove("actor_id");
        // Only the trackball's `device` names a unit; no other section's is
        // removed.
        let device = (prefix == "trackball").then_some("device");
        for key in ["address", "bind"].into_iter().chain(device) {
            if let Some(serde_json::Value::String(v)) = map.remove(key) {
                removed.push(v);
            }
        }
    }
    (section, removed)
}

/// Add the host of each address in `scrub` (an address may also appear
/// without its port), then sort and dedup.
fn normalize_scrub(scrub: &mut Vec<String>) {
    let hosts: Vec<String> = scrub
        .iter()
        .filter_map(|s| s.parse::<SocketAddr>().ok())
        .map(|a| a.ip())
        .filter(|ip| !ip.is_unspecified())
        .map(|ip| ip.to_string())
        .collect();
    scrub.extend(hosts);
    scrub.sort();
    scrub.dedup();
}

/// Snapshot the settings and identity in effect for a shot from the card
/// `actor` (actor id `from`; `remote` when it came in on a hub link, whose
/// device kind is `frp`). The device identity comes from the actor's
/// `bus.actor`.
async fn shot_context(
    state: &WebState,
    actor: &str,
    from: &str,
    putting: bool,
    remote: bool,
) -> ExportContext {
    let turn = state.root.session.turn();
    let config = state.root.system.snapshot();
    let (prefix, index) = actor.split_once('.').unwrap_or((actor, ""));

    let identity = state.bus_state.lock().ok().and_then(|bus| {
        bus.actors()
            .find(|(id, _)| *id == from)
            .and_then(|(_, a)| a.device.clone())
    });
    // A remote actor's shot (`remote`): its id says nothing about the device.
    let device = ExportDevice {
        kind: if remote { "frp" } else { device_kind(prefix) }.to_string(),
        manufacturer: identity.as_ref().and_then(|d| d.manufacturer.clone()),
        model: identity.as_ref().and_then(|d| d.model.clone()),
        firmware: identity.and_then(|d| d.firmware),
    };

    let (device_section, mut scrub) = device_section(&config, prefix, index);
    // The device id identifies the unit (network name, BLE name or
    // address). Open Connect's is the monitor's product label, and is kept.
    if prefix != "openconnect_server"
        && let Some(id) = state
            .actors
            .read()
            .await
            .get(actor)
            .and_then(|a| a.telemetry.get("device_info"))
    {
        scrub.push(id.clone());
    }
    normalize_scrub(&mut scrub);

    ExportContext {
        device,
        // An Auto-chip is simulated as a chip whatever the mode, and a
        // putting device's shot as a putt.
        detection_mode: if actor == flighthook::AUTO_CHIP_ACTOR {
            Some(Mode::Chipping)
        } else if putting {
            Some(Mode::Putting)
        } else {
            turn.mode
        },
        device_section,
        simulation: config.simulation,
        scrub,
        card_keys: state.root.card_keys(),
    }
}

/// Append a shot to the history, keeping it within [`MAX_SHOTS`] and the
/// raw byte `budget`. Over budget, the oldest shots lose their raw frames
/// first; their decoded data is kept. Returns the shot that fell out of the
/// history, if any.
pub(super) fn store_shot(
    shots: &mut VecDeque<StoredShot>,
    raw_bytes: &AtomicUsize,
    shot: StoredShot,
    budget: usize,
) -> Option<StoredShot> {
    let mut total = raw_bytes.load(Ordering::Relaxed);
    let evicted = if shots.len() >= MAX_SHOTS {
        shots.pop_front()
    } else {
        None
    };
    if let Some(old) = &evicted {
        total = total.saturating_sub(old.raw_len());
    }
    total += shot.raw_len();
    shots.push_back(shot);
    for s in shots.iter_mut() {
        if total <= budget {
            break;
        }
        if let Some(raw) = s.raw.as_mut()
            && !raw.frames.is_empty()
        {
            total = total.saturating_sub(raw_held_bytes(raw));
            raw.frames = Vec::new();
            raw.unavailable = Some(RAW_EVICTED.into());
        }
    }
    raw_bytes.store(total, Ordering::Relaxed);
    evicted
}

/// End `actor`'s pending shots uncounted.
fn drop_pending(accumulators: &mut PendingShots, actor: &str, keep: Option<&str>) {
    accumulators.discard_actor(actor, keep);
}

/// Put a camera's clip on shot `shot_id`, replacing that camera's earlier
/// one, or with `None` drop every clip of the shot. The shot is found by its
/// id whatever device took it, pending or stored; a shot the history no
/// longer holds is left alone.
fn stamp_video(
    shots: &mut VecDeque<StoredShot>,
    pending: &mut PendingShots,
    shot_id: &str,
    video: Option<&ShotVideo>,
) {
    let apply = |list: &mut Vec<ShotVideo>| match video {
        Some(v) => list.stamp_video(v),
        None => list.clear(),
    };
    for (id, p) in pending.iter_mut() {
        if id == shot_id {
            apply(&mut p.video);
        }
    }
    if let Some(stored) = shots.iter_mut().rev().find(|s| s.key.shot_id == shot_id) {
        apply(&mut stored.data.video);
    }
}

/// Mark camera `camera`'s clip of shot `shot_id` with `set` (body tracked,
/// swing analyzed), pending or stored, whatever device took the shot.
/// Forget what the stored shots `keys` say about camera `camera`'s body
/// tracking and swing analysis: its clip is being tracked again.
pub(crate) async fn forget_analysis(state: &WebState, keys: &[ShotKey], camera: &str) {
    let mut shots = state.shots.write().await;
    for stored in shots
        .iter_mut()
        .filter(|s| keys.iter().any(|k| k.shot_id == s.key.shot_id))
    {
        stored.data.video.forget_analysis(camera);
    }
}

fn mark_video(
    shots: &mut VecDeque<StoredShot>,
    pending: &mut PendingShots,
    shot_id: &str,
    camera: &str,
    set: impl Fn(&mut ShotVideo),
) {
    let apply = |list: &mut Vec<ShotVideo>| {
        list.mark_video(camera, &set);
    };
    for (id, p) in pending.iter_mut() {
        if id == shot_id {
            apply(&mut p.video);
        }
    }
    if let Some(stored) = shots.iter_mut().rev().find(|s| s.key.shot_id == shot_id) {
        apply(&mut stored.data.video);
    }
}

pub(super) async fn apply_bus_event(
    state: &WebState,
    msg: &BusMsg,
    accumulators: &mut PendingShots,
) {
    // A `shot.finished` reaches the UI after the store's shot, so a row
    // with data never reads as no read for a moment.
    if matches!(
        msg.as_frp().map(|m| &m.body),
        Some(FrpBody::ShotFinished(_))
    ) {
        store_bus_event(state, msg, accumulators).await;
        ui_socket::follow(state, msg);
    } else {
        ui_socket::follow(state, msg);
        store_bus_event(state, msg, accumulators).await;
    }
}

/// The store's part of [`apply_bus_event`].
async fn store_bus_event(state: &WebState, msg: &BusMsg, accumulators: &mut PendingShots) {
    let now = Instant::now();
    accumulators.discard_expired(now);
    // Who holds `game` is what the actor last said.
    if let Some(m) = msg.as_frp()
        && let FrpBody::BusActor(actor) = &m.body
    {
        if actor.has_role(flightrelay::roles::GAME) {
            accumulators.games.insert(m.from.clone());
        } else {
            accumulators.games.remove(&m.from);
        }
    }
    match msg {
        BusMsg::Local { card, event } => match event {
            LocalEvent::ActorGone { actor } => actor_gone(state, actor, accumulators),
            LocalEvent::ShotCapture { key, raw, scrub } => {
                if let Some(p) = accumulators.shots.get_mut(&key.shot_id) {
                    p.raw = Some((**raw).clone());
                    p.context.scrub.extend(scrub.iter().cloned());
                    normalize_scrub(&mut p.context.scrub);
                }
            }
            LocalEvent::ShotPath { key, path } => {
                if let Some(p) = accumulators.shots.get_mut(&key.shot_id) {
                    p.path = Some(path.clone());
                } else {
                    attach(state, &key.shot_id, |stored| {
                        stored.path = Some(path.clone());
                    })
                    .await;
                }
            }
            _ => apply_local(state, card, event, accumulators).await,
        },
        BusMsg::Frp { msg: frp, .. } if !accumulators.owners.admits(frp, now) => {}
        BusMsg::Frp { msg: frp, peer, .. } => match &frp.body {
            // A hosted card's `bus.actor` carries its device.
            FrpBody::BusActor(actor)
                if actor.has_role(flightrelay::roles::LAUNCH_MONITOR) || actor.device.is_some() =>
            {
                accumulators.data.feed(frp);
                apply_device(state, &frp.from, actor).await;
            }
            FrpBody::ShotTrigger(trigger) => {
                let id = trigger.key.shot_id.clone();
                if stored(state, &id).await || accumulators.shots.contains_key(&id) {
                    return;
                }
                let card = stored::card_of(&state.root, &frp.from);
                // A device runs one shot at a time: a new trigger ends its
                // earlier unfinished shots uncounted.
                drop_pending(accumulators, &card, Some(&id));
                // The accumulator stamps the turn and the golfer's open block
                // now, so commands and club changes during the flight apply
                // to the next shot.
                accumulators.data.feed(frp);
                let putting = putting_device(state, &frp.from);
                let context = shot_context(state, &card, &frp.from, putting, peer.is_some()).await;
                accumulators.shots.insert(
                    id,
                    PendingShot {
                        received_at: Utc::now(),
                        actor: card,
                        context,
                        turn: Some(state.root.session.turn()),
                        putting,
                        video: Vec::new(),
                        stroke: None,
                        raw: None,
                        path: None,
                        started: now,
                    },
                );
            }
            FrpBody::ShotLaunch(_)
            | FrpBody::ShotClub(_)
            | FrpBody::ShotImpact(_)
            | FrpBody::ShotFlight(_) => {
                let id = frp
                    .body
                    .shot_key()
                    .map(|k| k.shot_id.clone())
                    .unwrap_or_default();
                // One attach path: a message for a stored shot attaches
                // there, whenever it arrives.
                if stored(state, &id).await {
                    attach(state, &id, |s| {
                        if let Some(shot) = &mut s.frp
                            && (frp.from == shot.from || matches!(frp.body, FrpBody::ShotFlight(_)))
                        {
                            if let Some(warning) = late_warning(shot, &frp.from, &frp.body) {
                                tracing::warn!("{warning}");
                            }
                            attach_message(shot, &frp.from, &frp.body);
                        }
                    })
                    .await;
                } else {
                    accumulators.data.feed(frp);
                }
            }
            FrpBody::ShotFinished(finished) => {
                if let Some(ShotEvent::Finished(shot)) = accumulators.data.feed(frp) {
                    finish_shot(state, accumulators, &finished.key, shot).await;
                }
            }
            FrpBody::GameStroke(stroke) => {
                // One attach path: the stroke joins the shot by `shot_id`,
                // pending or stored, whenever it arrives, from the actor
                // holding `game` only.
                if !accumulators.games.contains(&frp.from) {
                    return;
                }
                let Some(id) = stroke.shot_id.clone() else {
                    return;
                };
                if let Some(p) = accumulators.shots.get_mut(&id) {
                    p.stroke = Some(stroke.clone());
                } else {
                    attach(state, &id, |s| s.data.stroke = Some(stroke.clone())).await;
                }
            }
            FrpBody::GolferTurn(_) if frp.from == state.root.flighthook_id() => {
                accumulators.data.feed(frp);
            }
            FrpBody::PracticeBlock(_) | FrpBody::Removed(_)
                if frp.from == state.root.flighthook_id() =>
            {
                accumulators.data.feed(frp);
                state.blocks.write().await.apply(&frp.body);
            }
            FrpBody::GolferTurn(_) | FrpBody::PracticeBlock(_) | FrpBody::GolferSelect(_) => {}
            _ => {
                accumulators.data.feed(frp);
            }
        },
    }
}

/// Whether the history holds shot `shot_id`.
async fn stored(state: &WebState, shot_id: &str) -> bool {
    state
        .shots
        .read()
        .await
        .iter()
        .rev()
        .any(|s| s.key.shot_id == shot_id)
}

/// The warning for a message from the shot's own launch monitor after its
/// `shot.finished`, which is out of order. A message from any other actor,
/// such as a flight model's `shot.flight`, is an addendum and gets none.
pub(super) fn late_warning(shot: &CompletedShot, from: &str, body: &FrpBody) -> Option<String> {
    (from == shot.from).then(|| {
        format!(
            "{} from {from} arrived after shot {} ({}) finished",
            body.kind(),
            shot.key.shot_number,
            shot.key.shot_id
        )
    })
}

/// One message for a shot already composed: a flight always attaches; a
/// launch, club or impact only when the shot has none.
fn attach_message(shot: &mut CompletedShot, from: &str, body: &FrpBody) {
    match body {
        FrpBody::ShotLaunch(m) => {
            shot.launch.get_or_insert_with(|| m.launch.clone());
        }
        FrpBody::ShotClub(m) => {
            shot.club.get_or_insert_with(|| m.club.clone());
        }
        FrpBody::ShotImpact(m) => {
            shot.impact.get_or_insert_with(|| m.impact.clone());
        }
        FrpBody::ShotFlight(m) => shot.flights.push(flightrelay::Flight {
            from: from.to_owned(),
            flight: m.flight.clone(),
        }),
        _ => {}
    }
}

/// Change the stored shot `shot_id` with `change`, rebuild its data, and
/// send the UI its ball again. A shot the history no longer holds is left
/// alone.
async fn attach(state: &WebState, shot_id: &str, change: impl FnOnce(&mut StoredShot)) {
    let mut shots = state.shots.write().await;
    let Some(stored) = shots.iter_mut().rev().find(|s| s.key.shot_id == shot_id) else {
        return;
    };
    change(stored);
    rebuild(stored);
    let data = stored.data.clone();
    drop(shots);
    ui_socket::send_shot(state, data);
}

/// Rebuild `stored.data` from its composed shot, keeping what the web layer
/// stamped on it.
fn rebuild(stored: &mut StoredShot) {
    let Some(shot) = &stored.frp else {
        return;
    };
    let fresh = stored::stored_data(shot, stored.data.card.as_deref(), stored.path.as_ref());
    let data = &mut stored.data;
    data.launch = fresh.launch;
    data.club = fresh.club;
    data.impact = fresh.impact;
    data.metrics = fresh.metrics;
    data.source = fresh.source;
    data.flights = fresh.flights;
    data.path = fresh.path;
}

/// The `bus.actor` name of the remote actor `from`, while it is on the bus.
fn remote_name(state: &WebState, from: &str) -> Option<String> {
    state.bus_state.lock().ok().and_then(|bus| {
        bus.actors()
            .find(|(id, _)| *id == from)
            .map(|(_, a)| a.name.trim().to_owned())
            .filter(|name| !name.is_empty())
    })
}

/// The name a game's `golfer.profile` gives golfer `id` while it is on the
/// bus. A flighthook golfer is named by its saved record.
fn game_golfer_name(state: &WebState, id: Option<&flightrelay::GolferId>) -> Option<String> {
    let id = id.filter(|id| id.namespace() != flighthook::GOLFER_NAMESPACE)?;
    state.bus_state.lock().ok().and_then(|bus| {
        bus.of_kind(flightrelay::StateKind::GolferProfile)
            .find_map(|m| match &m.body {
                FrpBody::GolferProfile(p) if p.id == *id => Some(p.name.trim().to_owned()),
                _ => None,
            })
            .filter(|name| !name.is_empty())
    })
}

/// Store the shot `key` that just finished, composed as `shot`, count it in
/// its block and send it to the UI sockets.
async fn finish_shot(
    state: &WebState,
    accumulators: &mut PendingShots,
    key: &ShotKey,
    shot: CompletedShot,
) {
    let Some(PendingShot {
        received_at,
        context,
        turn,
        putting,
        video,
        stroke,
        raw,
        path,
        ..
    }) = accumulators.shots.remove(&key.shot_id)
    else {
        return;
    };
    let card = state.root.card_of_actor(&shot.from);
    let mut data = stored::stored_data(&shot, card.as_deref(), path.as_ref());
    // A shot the device read nothing for is kept for the export when its
    // raw frames are; `/api/shots` leaves it out.
    let counted = data.has_data();
    if !counted && raw.is_none() {
        return;
    }
    let block = shot.block.clone();
    data.turn = shot.turn.clone().or(turn);
    data.club_selected = if putting {
        Some(flighthook::Club::Putter)
    } else {
        data.turn.as_ref().and_then(|t| t.club)
    };
    data.video = video;
    data.stroke = stroke;
    if card.is_none() {
        data.from_name = remote_name(state, &shot.from);
    }
    data.golfer_name = game_golfer_name(state, data.turn.as_ref().and_then(|t| t.golfer.as_ref()));
    if counted {
        data.block.clone_from(&block);
    }
    let stored = StoredShot {
        received_at,
        key: key.clone(),
        context,
        data,
        raw,
        frp: Some(shot),
        path,
    };
    let data = stored.data.clone();

    {
        let mut blocks = state.blocks.write().await;
        if counted && let Some(id) = block.as_deref() {
            let at = u64::try_from(received_at.timestamp_millis()).unwrap_or(0);
            blocks.count(id, at);
        }
        let mut shots = state.shots.write().await;
        let evicted = store_shot(&mut shots, &state.raw_bytes, stored, RAW_BUDGET_BYTES);
        if let Some(id) = evicted.and_then(|old| old.data.block) {
            blocks.release(&id);
        }
    }
    ui_socket::send_shot(state, data);
}

/// Keep the web state current from a launch monitor's or a hosted card's
/// `bus.actor`: its rows and flagged values on its card
/// ([`ActorStatusResponse::apply_actor`](flighthook::ActorStatusResponse::apply_actor)).
async fn apply_device(state: &WebState, from: &str, actor: &flightrelay::BusActor) {
    let card = stored::card_of(&state.root, from);
    let mut actors = state.actors.write().await;
    if let Some(entry) = actors.get_mut(&card) {
        entry.apply_actor(actor);
    }
}

/// Keep the web state current from one local event of card `card` (card
/// statuses, cameras, Clear history and config outcomes).
async fn apply_local(
    state: &WebState,
    card: &str,
    event: &LocalEvent,
    accumulators: &mut PendingShots,
) {
    match event {
        LocalEvent::CardStatus { status, telemetry } => {
            let mut actors = state.actors.write().await;
            let actor = actors.entry(card.to_owned()).or_insert_with(|| {
                let snap = state.root.system.snapshot();
                let names = actor_names(&snap);
                let name = names.get(card).cloned().unwrap_or_default();
                new_actor(name)
            });
            actor.apply_status(*status, telemetry, state.now_ms());
            drop(actors);
            // Out of session, the device finishes none of its pending shots.
            if *status != ActorStatus::Connected {
                drop_pending(accumulators, card, None);
            }
        }
        LocalEvent::ShotVideo {
            keys,
            position,
            duration_ms,
            fps,
            trigger_offset_ms,
            impact_offset_ms,
            complete,
            dropped,
        } => {
            let video = ShotVideo {
                camera: card.to_owned(),
                position: *position,
                duration_ms: *duration_ms,
                fps: *fps,
                trigger_offset_ms: *trigger_offset_ms,
                impact_offset_ms: *impact_offset_ms,
                complete: *complete,
                dropped: *dropped,
                pose: false,
                swing: false,
                rotation: 0,
                analyzed_with: None,
            };
            let mut shots = state.shots.write().await;
            for key in keys {
                stamp_video(&mut shots, accumulators, &key.shot_id, Some(&video));
            }
        }
        LocalEvent::ShotVideoRemoved { keys } => {
            let mut shots = state.shots.write().await;
            for key in keys {
                stamp_video(&mut shots, accumulators, &key.shot_id, None);
            }
        }
        LocalEvent::ShotPose { keys, rotation } => {
            let mut shots = state.shots.write().await;
            for key in keys {
                mark_video(&mut shots, accumulators, &key.shot_id, card, |v| {
                    v.pose = true;
                    v.rotation = *rotation;
                    v.analyzed_with = Some(stance::VERSION.to_owned());
                });
            }
        }
        LocalEvent::ShotSwing { keys } => {
            let mut shots = state.shots.write().await;
            for key in keys {
                mark_video(&mut shots, accumulators, &key.shot_id, card, |v| {
                    v.swing = true;
                });
            }
        }
        LocalEvent::ClearShotHistory => {
            // Shots in flight belong to the history being cleared: they end
            // here, so a late `shot.finished` finds nothing to store. The
            // store forgets its shots and blocks, then asks for a new
            // session. The accumulator keeps the turn and the open blocks it
            // follows.
            for (id, _) in accumulators.shots.drain() {
                accumulators.data.discard(&id);
            }
            {
                let mut blocks = state.blocks.write().await;
                blocks.clear();
                let mut shots = state.shots.write().await;
                shots.clear();
                state.raw_bytes.store(0, Ordering::Relaxed);
            }
            send_new_block(state, true);
        }
        LocalEvent::ConfigOutcome {
            started,
            stopped,
            restarted,
            ..
        } => {
            // Names come from the config, and a rename is applied without a
            // restart: every outcome refreshes them.
            let snap = state.root.system.snapshot();
            let names = actor_names(&snap);
            let disabled = snap.disabled_actors();
            let mut actors = state.actors.write().await;
            rename_actors(&mut actors, &names);
            if !started.is_empty() || !stopped.is_empty() || !restarted.is_empty() {
                let now = state.now_ms();
                for (id, name) in &names {
                    actors
                        .entry(id.clone())
                        .or_insert_with(|| new_actor(name.clone()));
                }
                // A restarted actor begins a new history: no alert for the
                // connection its old instance lost. The outcome can arrive
                // after the new instance's first statuses (`Starting`, even
                // `Connected`); `restart` keeps the latest status, so the
                // order does not matter.
                for id in restarted {
                    if let Some(entry) = actors.get_mut(id) {
                        entry.restart(now);
                    }
                }
                // Remove stopped actors; one turned off stays, Disabled
                // (below).
                for id in stopped {
                    actors.remove(id);
                }
            }
            // A card turned off shows as Disabled, whether or not an
            // instance of it was running.
            follow_disabled(&mut actors, &names, &disabled);
        }
        _ => {}
    }
}
