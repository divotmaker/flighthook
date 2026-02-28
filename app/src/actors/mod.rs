//! Actor infrastructure — shared trait, bus helpers, and actor resolution.

pub mod gspro;
pub mod mevo;
pub mod mock;
pub mod system;
pub mod web;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::broadcast;

use crate::bus::{BusReceiver, BusSender};
use crate::state::SystemState;
use crate::state::config::{FlighthookConfig, global_id};

// ---------------------------------------------------------------------------
// Actor trait
// ---------------------------------------------------------------------------

/// Outcome of calling `Actor::reconfigure()` after a config change.
pub enum ReconfigureOutcome {
    /// Config unchanged — no action needed.
    NoChange,
    /// Applied in-place (e.g. sent UpdateConfig on bus).
    Applied,
    /// Must stop and recreate the actor (e.g. address changed).
    RestartRequired,
}

/// Common trait for self-managed actors. Each actor struct holds its own config;
/// `start()` clones what it needs and spawns a thread.
pub trait Actor: Send + Sync {
    /// Spawn the actor's run loop.
    fn start(&self, state: Arc<SystemState>, sender: BusSender, receiver: BusReceiver);

    /// Request the actor to stop. Default: no-op (actors check the shutdown
    /// flag via `BusReceiver::is_shutdown()`).
    fn stop(&self) {}

    /// React to a config change. Implementations compare the current config
    /// snapshot against the actor's construction params and return the
    /// appropriate action. Default: `NoChange`.
    fn reconfigure(&self, _state: &Arc<SystemState>, _sender: &BusSender) -> ReconfigureOutcome {
        ReconfigureOutcome::NoChange
    }
}

// ---------------------------------------------------------------------------
// Actor resolution
// ---------------------------------------------------------------------------

/// A concrete actor ready to be started, resolved from config.
pub struct ResolvedActor {
    pub id: String,
    pub name: String,
    pub actor: Box<dyn Actor>,
}

/// Build a flat list of all actors from the persisted config.
///
/// Iterates all config sections (mevo, mock_monitor, gspro, random_club,
/// webserver) and constructs the appropriate concrete actor for each.
/// Invalid addresses are logged and skipped.
pub fn resolve_actors(config: &FlighthookConfig) -> Vec<ResolvedActor> {
    use flighthook::ShotDetectionMode;

    let mode = ShotDetectionMode::Full;
    let mut actors = Vec::new();

    // Mevo devices
    for (index, section) in &config.mevo {
        let id = global_id("mevo", index);
        let addr_str = section.address.as_deref().unwrap_or("192.168.2.1:5100");
        match addr_str.parse::<SocketAddr>() {
            Ok(addr) => {
                let session_config = mevo::SessionConfig::from_mevo_section(section);
                actors.push(ResolvedActor {
                    id,
                    name: section.name.clone(),
                    actor: Box::new(mevo::MevoActor {
                        addr,
                        initial_mode: mode,
                        session_config,
                    }),
                });
            }
            Err(e) => {
                tracing::warn!("device '{id}': invalid address '{addr_str}': {e}");
            }
        }
    }

    // Mock monitors
    for (index, section) in &config.mock_monitor {
        let id = global_id("mock_monitor", index);
        actors.push(ResolvedActor {
            id,
            name: section.name.clone(),
            actor: Box::new(mock::launch::MockLaunchActor { initial_mode: mode }),
        });
    }

    // GSPro integrations
    for (index, section) in &config.gspro {
        let id = global_id("gspro", index);
        let addr_str = section.address.as_deref().unwrap_or("127.0.0.1:921");
        match addr_str.parse::<SocketAddr>() {
            Ok(addr) => {
                let routing = gspro::GsProRouting {
                    full_monitor: section.full_monitor.clone(),
                    chipping_monitor: section.chipping_monitor.clone(),
                    putting_monitor: section.putting_monitor.clone(),
                };
                actors.push(ResolvedActor {
                    id,
                    name: section.name.clone(),
                    actor: Box::new(gspro::GsProActor { addr, routing }),
                });
            }
            Err(e) => {
                tracing::warn!("integration '{id}': invalid address '{addr_str}': {e}");
            }
        }
    }

    // Random club integrations
    for (index, section) in &config.random_club {
        let id = global_id("random_club", index);
        actors.push(ResolvedActor {
            id,
            name: section.name.clone(),
            actor: Box::new(mock::randomclub::RandomClubActor),
        });
    }

    // Webservers
    for (index, ws) in &config.webserver {
        let id = global_id("webserver", index);
        match ws.bind.parse::<SocketAddr>() {
            Ok(addr) => {
                actors.push(ResolvedActor {
                    id,
                    name: ws.name.clone(),
                    actor: Box::new(web::WebActor::new(addr)),
                });
            }
            Err(e) => {
                tracing::warn!("webserver '{id}': invalid bind address '{}': {e}", ws.bind);
            }
        }
    }

    actors
}

/// Start a resolved actor: create bus wrappers, call start(), register in state.
pub fn start_actor(
    id: String,
    actor: Box<dyn Actor>,
    state: &Arc<SystemState>,
    bus_tx: &broadcast::Sender<flighthook::FlighthookMessage>,
) {
    let shutdown = Arc::new(AtomicBool::new(false));
    let sender = BusSender::new(id.clone(), bus_tx.clone(), Arc::clone(&shutdown));
    let receiver = sender.subscribe();
    actor.start(Arc::clone(state), sender, receiver);
    state.register_actor(id, actor, shutdown);
}

/// Build a map of actor IDs to display names from config (for UI display).
pub fn actor_names(config: &FlighthookConfig) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for (index, section) in &config.mevo {
        names.insert(global_id("mevo", index), section.name.clone());
    }
    for (index, section) in &config.mock_monitor {
        names.insert(global_id("mock_monitor", index), section.name.clone());
    }
    for (index, section) in &config.gspro {
        names.insert(global_id("gspro", index), section.name.clone());
    }
    for (index, section) in &config.random_club {
        names.insert(global_id("random_club", index), section.name.clone());
    }
    for (index, ws) in &config.webserver {
        names.insert(global_id("webserver", index), ws.name.clone());
    }
    names
}
