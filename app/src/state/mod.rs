pub mod config;
mod game;

pub use game::{GameState, GameStateWriter};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::actors::{Actor, ReconfigureOutcome};
use crate::bus::BusSender;
use config::SystemConfig;

/// Root entry point for all managed application state.
///
/// Passed as `Arc<SystemState>` to all actors and the web layer.
pub struct SystemState {
    pub system: SystemConfig,
    pub game: GameState,
    actors: RwLock<HashMap<String, (Box<dyn Actor>, Arc<AtomicBool>)>>,
}

impl SystemState {
    pub fn new(config_path: PathBuf) -> (Self, GameStateWriter) {
        let (game, writer) = GameState::new();
        (
            Self {
                system: SystemConfig::new(config_path),
                game,
                actors: RwLock::new(HashMap::new()),
            },
            writer,
        )
    }

    // ----- Actor registry -----

    /// Register an actor in the registry with its shutdown flag.
    pub fn register_actor(&self, id: String, actor: Box<dyn Actor>, shutdown: Arc<AtomicBool>) {
        self.actors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, (actor, shutdown));
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
        guard.get(id).map(|(a, _)| f(a.as_ref()))
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
    pub fn stop_actor(&self, id: &str) {
        let guard = self.actors.read().unwrap_or_else(|e| e.into_inner());
        if let Some((actor, shutdown)) = guard.get(id) {
            shutdown.store(true, Ordering::Relaxed);
            actor.stop();
        }
    }

    /// Remove an actor from the registry, returning it.
    pub fn remove_actor(&self, id: &str) -> Option<Box<dyn Actor>> {
        self.actors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
            .map(|(a, _)| a)
    }
}
