pub mod cameras;
pub mod clips;
pub mod config;
pub mod local_clients;
pub mod practice;
pub mod remotes;
pub mod round;
pub mod sent_shots;
pub mod session;

pub use session::{Session, SessionWriter};

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::actors::{Actor, ReconfigureOutcome};
use crate::bus::BusSender;
use cameras::Cameras;
use clips::ClipStore;
use config::SystemConfig;
use local_clients::LocalClients;
use round::RoundPolicy;
use sent_shots::SentShots;

/// Root entry point for all managed application state.
///
/// Passed as `Arc<SystemState>` to all actors and the web layer.
pub struct SystemState {
    pub system: SystemConfig,
    /// flighthook's `golfer.turn`.
    pub session: Session,
    /// The newest `bus.actor` of every local actor, for the heartbeat timer.
    pub beats: crate::bus::Beats,
    /// This process's connected Open Connect clients, which its Open Connect
    /// servers refuse.
    pub local_clients: Arc<LocalClients>,
    /// The shots the simulator outputs sent, which the Pureplay Accessory card tells
    /// the simulator's echoes by.
    pub sent_shots: Arc<SentShots>,
    /// The tournament round policy, and every shot source's type.
    pub round: RoundPolicy,
    /// The replay clips on disk, shared by the camera actors (beta).
    pub clips: ClipStore,
    /// The camera actors' latest frames and the camera list (beta).
    pub cameras: Cameras,
    /// Body tracking's models and recorded-clip queue (alpha).
    pub pose: crate::pose::BodyTracking,
    /// The Pureplay Accessory cards' pairing tokens (beta), kept out of the config.
    pub pureplay_tokens: Arc<crate::actors::pureplay_accessory::token::TokenStore>,
    /// The mode each running local launch monitor with a fixed mode arms
    /// in (the ExPutt: putting), by actor id.
    fixed_modes: FixedModes,
    actors: RwLock<HashMap<String, Registered>>,
    /// Actors taken out of the registry whose thread is still ending while
    /// it holds their device or port: by ID, whether it has ended since.
    closing: Arc<Mutex<HashMap<String, bool>>>,
    /// Actors a config command has stopped, or will start, once the actors
    /// it stopped have ended: the system actor waits for them off its own
    /// thread.
    settling: Mutex<HashSet<String>>,
}

/// What the registry holds to stop a running actor and see it end.
pub struct ActorHandle {
    /// The actor's shutdown flag, shared with its bus wrappers.
    pub shutdown: Arc<AtomicBool>,
    /// Silences the actor's bus sender once a new instance replaces it.
    pub muted: Arc<AtomicBool>,
    /// The actor's thread, `None` when it is not known.
    pub thread: Option<JoinHandle<()>>,
}

/// A registered actor.
struct Registered {
    actor: Box<dyn Actor>,
    handle: ActorHandle,
}

impl Registered {
    /// Whether the actor's thread has ended (or is not known).
    fn exited(&self) -> bool {
        self.handle
            .thread
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
    }
}

/// How [`SystemState::stop_and_wait`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// No actor is registered under that ID.
    NotRunning,
    /// The actor's thread ended; it is out of the registry.
    Exited,
    /// The thread is still running after the wait. The actor stays
    /// registered with its shutdown flag set.
    Lingering,
}

/// Body tracking with the models in the models folder, from their official
/// sources. Tests never download: each state has an empty folder of its own
/// and a stand-in source (`models::fake`, a "download" of about 0.4 s per
/// file).
fn body_tracking() -> crate::pose::BodyTracking {
    #[cfg(test)]
    {
        crate::pose::BodyTracking::new(
            std::env::temp_dir().join(format!(
                "flighthook-test-models-{}",
                uuid::Uuid::new_v4().simple()
            )),
            crate::pose::models::fake::Fake::new(40, Duration::from_millis(10)),
        )
    }
    #[cfg(not(test))]
    {
        crate::pose::BodyTracking::new(
            crate::pose::models::default_dir(),
            std::sync::Arc::new(crate::pose::models::Official::default()),
        )
    }
}

/// The Pureplay token file: the user's data folder; in tests a file of
/// each state's own in a fresh temporary folder.
fn pureplay_tokens() -> Arc<crate::actors::pureplay_accessory::token::TokenStore> {
    use crate::actors::pureplay_accessory::token::{FILE_NAME, TokenStore, default_path};
    let path = if cfg!(test) {
        std::env::temp_dir()
            .join(format!(
                "flighthook-test-pp-tokens-{}",
                uuid::Uuid::new_v4().simple()
            ))
            .join(FILE_NAME)
    } else {
        default_path()
    };
    Arc::new(TokenStore::new(path))
}

/// A test state's token folder goes with the state.
#[cfg(test)]
impl Drop for SystemState {
    fn drop(&mut self) {
        if let Some(dir) = self.pureplay_tokens.path().parent()
            && dir.file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .starts_with("flighthook-test-pp-tokens-")
            })
        {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// How often [`SystemState::wait_for_exit`] looks at the thread.
const EXIT_POLL: Duration = Duration::from_millis(10);

/// Join an actor's thread that has ended (or is about to), saying so when it
/// panicked.
fn join(id: &str, thread: JoinHandle<()>) {
    if thread.join().is_err() {
        tracing::warn!("actor '{id}' panicked");
    }
}

impl SystemState {
    /// The facts (sex, height) of the golfer up, by the lookup rule: the
    /// defaults while the turn names no golfer of the config.
    #[cfg(test)]
    pub fn golfer_up_facts(&self) -> flighthook::PlayerSection {
        let turn = self.session.turn();
        self.system
            .golfer_facts(turn.golfer.as_ref().map(flightrelay::GolferId::as_str))
    }

    pub fn new(config_path: PathBuf) -> (Self, SessionWriter) {
        let system = SystemConfig::new(config_path);
        let (session, writer) = Session::with_config(system.view());
        // Tournament mode starts as the file says.
        let round = RoundPolicy::default();
        round.set_manual(system.simulation().tournament);
        (
            Self {
                system,
                session,
                local_clients: Arc::default(),
                sent_shots: Arc::default(),
                round,
                clips: ClipStore::default(),
                cameras: Cameras::default(),
                pose: body_tracking(),
                pureplay_tokens: pureplay_tokens(),
                fixed_modes: FixedModes::default(),
                beats: crate::bus::Beats::default(),
                actors: RwLock::new(HashMap::new()),
                closing: Arc::default(),
                settling: Mutex::default(),
            },
            writer,
        )
    }

    // ----- FRP actor ids -----

    /// Whether `actor_id` is one of this process's own actors that is
    /// running: flighthook, Auto-chip, the flight model or a started card.
    pub fn is_live_local(&self, actor_id: &str) -> bool {
        match self.card_of_actor(actor_id).as_deref() {
            None => false,
            Some(flighthook::AUTO_CHIP_ACTOR | crate::actors::flight_model::CARD | "system") => {
                true
            }
            Some(card) => self.with_actor(card, |_| ()).is_some(),
        }
    }

    /// The flighthook actor's id (session, collector): `[frp] actor_id`.
    pub fn flighthook_id(&self) -> String {
        self.system
            .snapshot()
            .frp
            .actor_id
            .unwrap_or_else(|| "flighthook".into())
    }

    /// Auto-chip's actor id: `[frp] auto_chip_actor_id`.
    pub fn auto_chip_id(&self) -> String {
        self.system
            .snapshot()
            .frp
            .auto_chip_actor_id
            .unwrap_or_else(|| "autochip".into())
    }

    /// The flight model's actor id (the Simulation card):
    /// `[simulation] actor_id`.
    pub fn flight_model_id(&self) -> String {
        self.system
            .simulation()
            .actor_id
            .unwrap_or_else(|| "simulation".into())
    }

    /// The actor id of the card `key` (`mevo.0`); flighthook's own actors
    /// by their card key (`autochip`, `simulation`, `system`).
    pub fn actor_of_card(&self, key: &str) -> Option<String> {
        match key {
            flighthook::AUTO_CHIP_ACTOR => Some(self.auto_chip_id()),
            crate::actors::flight_model::CARD => Some(self.flight_model_id()),
            "system" | "web" => Some(self.flighthook_id()),
            _ => self.system.actor_of_card(key),
        }
    }

    /// The card key of every local actor, by actor id: the configured
    /// cards, Auto-chip and the flight model.
    pub fn card_keys(&self) -> std::collections::HashMap<String, String> {
        let config = self.system.snapshot();
        let mut keys: std::collections::HashMap<String, String> = config
            .card_actors()
            .into_iter()
            .filter_map(|(card, id)| Some((id?.to_owned(), card)))
            .collect();
        let own = self.system.own_actor_ids();
        if let Some(id) = own.auto_chip {
            keys.insert(id, flighthook::AUTO_CHIP_ACTOR.into());
        }
        if let Some(id) = own.flight_model {
            keys.insert(id, crate::actors::flight_model::CARD.into());
        }
        keys
    }

    /// Whether `key` is the key of one of this process's cards (`mevo.0`,
    /// `system`, the Simulation card, Auto-chip, the web server).
    /// A configured card's key is `<type>.<index>`, so a key with no dot
    /// (a generated actor id) is only checked against the fixed names, and
    /// the config is read in place, never cloned.
    pub fn is_card_key(&self, key: &str) -> bool {
        matches!(
            key,
            "system" | "web" | flighthook::AUTO_CHIP_ACTOR | crate::actors::flight_model::CARD
        ) || (key.contains('.')
            && crate::actors::actor_names(&self.system.view().read()).contains_key(key))
    }

    /// The card key of the local actor `actor_id`, if it is one.
    pub fn card_of_actor(&self, actor_id: &str) -> Option<String> {
        let own = self.system.own_actor_ids();
        if own.auto_chip.as_deref() == Some(actor_id) {
            return Some(flighthook::AUTO_CHIP_ACTOR.into());
        }
        if own.flight_model.as_deref() == Some(actor_id) {
            return Some(crate::actors::flight_model::CARD.into());
        }
        if own.flighthook.as_deref() == Some(actor_id) {
            return Some("system".into());
        }
        self.system.card_of_actor(actor_id)
    }

    /// Record that the launch monitor `actor_id` always arms in `mode`,
    /// while the returned guard lives (the card's actor thread).
    #[must_use]
    pub fn set_fixed_mode(&self, actor_id: &str, mode: flighthook::Mode) -> FixedModeGuard {
        let token = self.fixed_modes.next.fetch_add(1, Ordering::Relaxed);
        self.fixed_modes
            .map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(actor_id.to_owned(), (mode, token));
        FixedModeGuard {
            map: Arc::clone(&self.fixed_modes.map),
            actor_id: actor_id.to_owned(),
            token,
        }
    }

    /// The mode the launch monitor `actor_id` always arms in, if it has one.
    pub fn fixed_mode(&self, actor_id: &str) -> Option<flighthook::Mode> {
        self.fixed_modes
            .map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(actor_id)
            .map(|(mode, _)| *mode)
    }

    /// Whether `actor_id` is a game or simulator card: an Open Connect
    /// client, a Pureplay Connect output or a Pureplay Accessory card.
    pub fn is_game_card(&self, actor_id: &str) -> bool {
        self.card_of_actor(actor_id).is_some_and(|card| {
            matches!(
                card.split_once('.').map(|(kind, _)| kind),
                Some("openconnect_client" | "pureplay" | "pureplay_accessory")
            )
        })
    }

    // ----- Actor registry -----

    /// Register an actor in the registry with its handle.
    pub fn register_actor(&self, id: String, actor: Box<dyn Actor>, handle: ActorHandle) {
        self.actors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Registered { actor, handle });
    }

    /// Get the list of all registered actor IDs.
    pub fn actor_ids(&self) -> Vec<String> {
        self.actors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// Run a closure with a reference to the actor for `id`.
    /// Returns `None` if the actor is not registered.
    pub fn with_actor<F, R>(&self, id: &str, f: F) -> Option<R>
    where
        F: FnOnce(&dyn Actor) -> R,
    {
        let guard = self.actors.read().unwrap_or_else(|e| e.into_inner());
        guard.get(id).map(|r| f(r.actor.as_ref()))
    }

    /// Call `reconfigure()` on the actor identified by `id`.
    /// Returns `None` if the actor is not registered.
    pub fn reconfigure_actor(
        &self,
        id: &str,
        state: &Arc<SystemState>,
        sender: &BusSender,
    ) -> Option<ReconfigureOutcome> {
        self.with_actor(id, |actor| actor.reconfigure(state, sender))
    }

    /// Stop an actor by setting its shutdown flag and calling `stop()`.
    /// Returns how long the actor may take to end ([`Actor::stop_wait`]),
    /// `None` when it is not registered.
    pub fn stop_actor(&self, id: &str) -> Option<Duration> {
        let guard = self.actors.read().unwrap_or_else(|e| e.into_inner());
        guard.get(id).map(|r| {
            r.handle.shutdown.store(true, Ordering::Relaxed);
            r.actor.stop();
            r.actor.stop_wait()
        })
    }

    /// Stop actor `id` and wait, up to its [`Actor::stop_wait`], for its
    /// thread to end. An actor that ended is taken out of the registry; one
    /// still running stays in it, flagged for shutdown. For tests; the
    /// system actor stops several actors at once ([`Self::wait_for_exit`]).
    #[cfg(test)]
    pub fn stop_and_wait(&self, id: &str) -> Stopped {
        match self.stop_actor(id) {
            Some(wait) => self.wait_for_exit(id, Instant::now() + wait),
            None => Stopped::NotRunning,
        }
    }

    /// Wait until `deadline` for the thread of stopped actor `id` to end.
    /// An actor that ended is taken out of the registry and its thread
    /// joined; one still running stays in it.
    pub fn wait_for_exit(&self, id: &str, deadline: Instant) -> Stopped {
        loop {
            let exited = self
                .actors
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(id)
                .map(Registered::exited);
            match exited {
                None => return Stopped::NotRunning,
                Some(true) => break,
                Some(false) if Instant::now() >= deadline => return Stopped::Lingering,
                Some(false) => std::thread::sleep(EXIT_POLL),
            }
        }
        let gone = self
            .actors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(thread) = gone.and_then(|r| r.handle.thread) {
            join(id, thread);
        }
        Stopped::Exited
    }

    /// Silence registered actor `id`'s bus sender, leaving it registered.
    pub fn mute_actor(&self, id: &str) {
        if let Some(r) = self
            .actors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
        {
            r.handle.muted.store(true, Ordering::Relaxed);
        }
    }

    /// Take a stopping actor out of the registry and silence it, leaving
    /// its thread to end on its own: a new instance takes over its ID.
    pub fn retire_actor(&self, id: &str) {
        drop(self.take_retired(id));
    }

    /// Unregister `id`, flagged for shutdown and muted. Returns its entry.
    fn take_retired(&self, id: &str) -> Option<Registered> {
        let gone = self
            .actors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(r) = &gone {
            r.handle.shutdown.store(true, Ordering::Relaxed);
            r.handle.muted.store(true, Ordering::Relaxed);
        }
        gone
    }

    /// Retire stopping actor `id` whose thread still holds its device or
    /// port, and watch that thread: until it ends, [`Self::is_closing`]
    /// says so and no new instance of `id` may start; once it has,
    /// [`Self::take_closed`] hands `id` to whoever starts it again.
    pub fn close_in_background(&self, id: &str) {
        let Some(entry) = self.take_retired(id) else {
            return;
        };
        let closing = Arc::clone(&self.closing);
        let ended = |closing: &Mutex<HashMap<String, bool>>, id: &str| {
            closing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_owned(), true);
        };
        closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_owned(), false);
        let Some(thread) = entry.handle.thread else {
            ended(&closing, id);
            return;
        };
        let watched = id.to_owned();
        let spawned = std::thread::Builder::new()
            .name(format!("closing:{id}"))
            .spawn(move || {
                join(&watched, thread);
                ended(&closing, &watched);
            });
        if let Err(e) = spawned {
            // Nobody can tell when it ends: let the next start go ahead.
            tracing::warn!("actor '{id}': cannot watch its thread: {e}");
            ended(&self.closing, id);
        }
    }

    /// Whether an earlier instance of `id` is still closing (or has closed
    /// and is waiting for [`Self::take_closed`]).
    pub fn is_closing(&self, id: &str) -> bool {
        self.closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }

    /// The IDs with an old instance still closing (or closed and waiting
    /// for [`Self::take_closed`]), in ID order.
    pub fn closing_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    /// The IDs whose closing instance has ended since, in ID order. They
    /// are no longer closing.
    pub fn take_closed(&self) -> Vec<String> {
        let mut closing = self.closing.lock().unwrap_or_else(|e| e.into_inner());
        let mut ended: Vec<String> = closing
            .iter()
            .filter(|(_, ended)| **ended)
            .map(|(id, _)| id.clone())
            .collect();
        ended.sort();
        for id in &ended {
            closing.remove(id);
        }
        ended
    }

    /// Mark `id` settling: a config command waits for the actors it stopped
    /// before it starts or releases `id`.
    pub fn begin_settling(&self, id: &str) {
        self.settling
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_owned());
    }

    /// `id` is no longer settling.
    pub fn end_settling(&self, id: &str) {
        self.settling
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    /// Whether a config command is still waiting before it starts or
    /// releases `id` ([`Self::begin_settling`]).
    pub fn is_settling(&self, id: &str) -> bool {
        self.settling
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(id)
    }

    /// The settling IDs, in ID order.
    pub fn settling_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .settling
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    /// Stop every actor at once and wait for their threads, each up to its
    /// [`Actor::stop_wait`] but no longer than `max` in all, and for the
    /// instances still closing. Returns the IDs still running at the end.
    pub fn stop_all_and_wait(&self, max: Duration) -> Vec<String> {
        let start = Instant::now();
        let deadline = start + max;
        let mut waits: Vec<(String, Instant)> = self
            .actor_ids()
            .into_iter()
            .filter_map(|id| {
                let wait = self.stop_actor(&id)?;
                Some((id, (start + wait).min(deadline)))
            })
            .collect();
        waits.sort();
        let mut running: Vec<String> = waits
            .into_iter()
            .filter(|(id, until)| self.wait_for_exit(id, *until) == Stopped::Lingering)
            .map(|(id, _)| id)
            .collect();
        loop {
            let closing: Vec<String> = self
                .closing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|(_, ended)| !**ended)
                .map(|(id, _)| id.clone())
                .collect();
            if closing.is_empty() || Instant::now() >= deadline {
                running.extend(closing);
                break;
            }
            std::thread::sleep(EXIT_POLL);
        }
        running.sort();
        running
    }

    /// The thread of registered actor `id`, for tests that tell one instance
    /// from the next.
    #[cfg(test)]
    pub fn actor_thread(&self, id: &str) -> Option<std::thread::ThreadId> {
        self.actors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .and_then(|r| r.handle.thread.as_ref())
            .map(|t| t.thread().id())
    }
}

/// The fixed modes of the running local launch monitors, by actor id, each
/// with the token of the guard that holds it.
#[derive(Default)]
struct FixedModes {
    map: Arc<RwLock<HashMap<String, (flighthook::Mode, u64)>>>,
    next: std::sync::atomic::AtomicU64,
}

/// Holds a launch monitor's fixed mode: dropping it (the card's actor
/// thread ends) forgets the mode, unless a newer thread of the same actor
/// set it since.
pub struct FixedModeGuard {
    map: Arc<RwLock<HashMap<String, (flighthook::Mode, u64)>>>,
    actor_id: String,
    token: u64,
}

impl Drop for FixedModeGuard {
    fn drop(&mut self) {
        let mut map = self.map.write().unwrap_or_else(|e| e.into_inner());
        if map
            .get(&self.actor_id)
            .is_some_and(|(_, t)| *t == self.token)
        {
            map.remove(&self.actor_id);
        }
    }
}
