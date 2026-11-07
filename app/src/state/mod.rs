pub mod cameras;
pub mod clips;
pub mod config;
mod game;
pub mod local_clients;
pub mod strict;

pub use game::{GameState, GameStateWriter};

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
use strict::StrictRounds;

/// Root entry point for all managed application state.
///
/// Passed as `Arc<SystemState>` to all actors and the web layer.
pub struct SystemState {
    pub system: SystemConfig,
    pub game: GameState,
    /// This process's connected Open Connect clients, which its Open Connect
    /// servers refuse.
    pub local_clients: Arc<LocalClients>,
    /// The Pureplay links in strict mode: Auto-chip does not fire while any
    /// is held.
    pub strict_rounds: StrictRounds,
    /// The replay clips on disk, shared by the camera actors (beta).
    pub clips: ClipStore,
    /// The camera actors' latest frames and the camera list (beta).
    pub cameras: Cameras,
    /// Body tracking's models and recorded-clip queue (alpha).
    pub pose: crate::pose::BodyTracking,
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
    pub fn new(config_path: PathBuf) -> (Self, GameStateWriter) {
        let (game, writer) = GameState::new();
        (
            Self {
                system: SystemConfig::new(config_path),
                game,
                local_clients: Arc::default(),
                strict_rounds: StrictRounds::default(),
                clips: ClipStore::default(),
                cameras: Cameras::default(),
                pose: body_tracking(),
                actors: RwLock::new(HashMap::new()),
                closing: Arc::default(),
                settling: Mutex::default(),
            },
            writer,
        )
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
