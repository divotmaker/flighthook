//! Actor infrastructure — shared trait, bus helpers, and actor resolution.

pub(crate) mod backoff;
pub mod camera;
pub mod exputt;
pub mod gspro;
pub mod mevo;
pub mod mock;
pub mod openconnect;
pub mod r10;
pub mod square;
pub mod system;
pub mod web;

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::broadcast;

use crate::bus::{BusReceiver, BusSender};
use crate::state::config::{FlighthookConfig, global_id};
use crate::state::{ActorHandle, SystemState};
use flighthook::Club;

// ---------------------------------------------------------------------------
// Actor trait
// ---------------------------------------------------------------------------

/// Outcome of calling `Actor::reconfigure()` after a config change.
pub enum ReconfigureOutcome {
    /// Config applied (or unchanged) — no restart needed.
    Applied,
    /// Must stop and recreate the actor (e.g. address changed).
    RestartRequired,
}

/// How long a stopped actor is given to end, unless it says otherwise
/// ([`Actor::stop_wait`]).
pub const STOP_WAIT: Duration = Duration::from_secs(3);

/// The longest [`Actor::stop_wait`] of any actor: the ExPutt's. A config
/// command stops the actors it restarts or removes together, so this bounds
/// how long it waits for them.
pub const MAX_STOP_WAIT: Duration = exputt::STOP_WAIT;

const _: () = assert!(STOP_WAIT.as_millis() <= MAX_STOP_WAIT.as_millis());
const _: () = assert!(square::STOP_WAIT.as_millis() <= MAX_STOP_WAIT.as_millis());

/// Common trait for self-managed actors. Each actor struct holds its own config;
/// `start()` clones what it needs and spawns a thread.
pub trait Actor: Send + Sync {
    /// Spawn the actor's run loop. Returns its thread, which ends when the
    /// actor has let go of its device or connection.
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> JoinHandle<()>;

    /// Request the actor to stop. Default: no-op (actors check the shutdown
    /// flag via `BusReceiver::is_shutdown()`).
    fn stop(&self) {}

    /// How long the actor's thread may take to end once stopped, at most.
    fn stop_wait(&self) -> Duration {
        STOP_WAIT
    }

    /// Whether a new instance may start while this one is still ending: what
    /// it holds does not keep a new instance from its device or port.
    /// Default: no.
    fn replaceable_while_stopping(&self) -> bool {
        false
    }

    /// React to a config change. Implementations compare the current config
    /// snapshot against the actor's construction params and return the
    /// appropriate action. Default: `Applied` (no change needed).
    fn reconfigure(&self, _state: &Arc<SystemState>, _sender: &BusSender) -> ReconfigureOutcome {
        ReconfigureOutcome::Applied
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
/// Iterates all config sections (webserver, mevo, mock_monitor,
/// openconnect_client, random_club, ...) and constructs the appropriate
/// concrete actor for each, webservers first.
/// Invalid addresses are logged and skipped.
///
/// `current_mode` is the active detection mode from game state. Falls back
/// to `Full` when no mode has been set yet (e.g. first startup).
pub fn resolve_actors(
    config: &FlighthookConfig,
    current_mode: Option<flighthook::ShotDetectionMode>,
) -> Vec<ResolvedActor> {
    use flighthook::ShotDetectionMode;

    let mode = current_mode.unwrap_or(ShotDetectionMode::Full);
    let mut actors = Vec::new();

    // Webservers first: each subscribes to the bus in `start`, so the
    // statuses the other actors emit as they start reach its state.
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

    // Mevo devices
    for (index, section) in &config.mevo {
        let id = global_id("mevo", index);
        let addr_str = section.address.as_deref().unwrap_or("192.168.2.1:5100");
        match addr_str.parse::<SocketAddr>() {
            Ok(addr) => {
                let session_config = mevo::SessionConfig::from_mevo_section(section);
                let use_estimated = section.use_estimated.unwrap_or(false);
                let camera_mode = section.camera_mode.unwrap_or_default();
                if !flighthook::MevoSection::launch_only_spin_valid(section.launch_only_spin) {
                    tracing::warn!(
                        "device '{id}': invalid launch_only_spin {} (must be 0 to {}), using 0 (off)",
                        section.launch_only_spin,
                        flighthook::MevoSection::MAX_LAUNCH_ONLY_SPIN,
                    );
                }
                actors.push(ResolvedActor {
                    id,
                    name: section.name.clone(),
                    actor: Box::new(mevo::MevoActor {
                        addr,
                        initial_mode: mode,
                        session_config,
                        use_estimated,
                        camera_mode,
                        spin_source: section.spin_source,
                        launch_only_spin: section.launch_only_spin_rpm_per_mph(),
                    }),
                });
            }
            Err(e) => {
                tracing::warn!("device '{id}': invalid address '{addr_str}': {e}");
            }
        }
    }

    // R10 devices
    for (index, section) in &config.r10 {
        let id = global_id("r10", index);
        actors.push(ResolvedActor {
            id,
            name: section.name.clone(),
            actor: Box::new(r10::R10Actor {
                initial_mode: mode,
                // The R10 protocol carries tee distance in yards.
                #[allow(clippy::cast_possible_truncation)]
                tee_range_yards: section.range.map(|d| d.as_yards() as f32),
            }),
        });
    }

    // Square Golf devices
    for (index, section) in &config.square {
        let id = global_id("square", index);
        let club = section
            .club
            .as_deref()
            .and_then(Club::from_code)
            .unwrap_or(Club::Iron7);
        let impact_calibration =
            square::build_impact_calibration(section.dot_bottom_to_face_centre_mm.as_ref());
        actors.push(ResolvedActor {
            id,
            name: section.name.clone(),
            actor: Box::new(square::SquareActor {
                address: section.address.clone(),
                club,
                advanced_spin: section.advanced_spin.unwrap_or(true),
                discard_non_putting_zero_spin: section
                    .discard_non_putting_zero_spin
                    .unwrap_or(true),
                impact_calibration,
            }),
        });
    }

    // ExPutt putting devices
    for (index, section) in &config.exputt {
        let id = global_id("exputt", index);
        match exputt::parse_address(section.address.as_deref()) {
            Ok(address) => {
                actors.push(ResolvedActor {
                    id,
                    name: section.name.clone(),
                    actor: Box::new(exputt::ExPuttActor { address }),
                });
            }
            Err(e) => {
                tracing::warn!("device '{id}': {e}");
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

    // OpenConnect servers (inbound launch monitors: Uneekor, Foresight, ...)
    for (index, section) in &config.openconnect_server {
        let id = global_id("openconnect_server", index);
        let bind_str = section.bind.as_deref().unwrap_or("0.0.0.0:921");
        match bind_str.parse::<SocketAddr>() {
            Ok(bind) => {
                actors.push(ResolvedActor {
                    id,
                    name: section.name.clone(),
                    actor: Box::new(openconnect::OpenConnectServerActor { bind }),
                });
            }
            Err(e) => {
                tracing::warn!("device '{id}': invalid bind address '{bind_str}': {e}");
            }
        }
    }

    // Open Connect clients (GSPro, Muni Golf, ...)
    for (index, section) in &config.openconnect_client {
        let id = global_id("openconnect_client", index);
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

    // Swing cameras
    for (index, section) in &config.camera {
        let id = global_id("camera", index);
        if let Err(e) = section.mode.parse::<stance::FormatRequest>() {
            tracing::warn!("camera '{id}': {e}");
        }
        actors.push(ResolvedActor {
            id,
            name: section.name.clone(),
            actor: Box::new(camera::CameraActor {
                section: section.clone(),
            }),
        });
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
    let muted = sender.muted();
    let receiver = sender.subscribe();
    let thread = actor.start(Arc::clone(state), sender, receiver);
    state.register_actor(
        id,
        actor,
        ActorHandle {
            shutdown,
            muted,
            thread: Some(thread),
        },
    );
}

/// Whether actors of this type can be reconnected: every launch monitor and
/// output, not a camera, the web server or the system actor.
pub(crate) fn reconnectable(type_key: &str) -> bool {
    !matches!(type_key, "camera" | "webserver" | "system")
}

/// How often [`connect_tcp`] looks at the shutdown flag.
const CONNECT_POLL: Duration = Duration::from_millis(50);

/// Connect actor `actor` to `addr` within `timeout`, giving up as soon as
/// `receiver`'s actor is told to stop: `Ok(None)`. The connect runs on a
/// thread of its own; one that completes after the actor gave up closes the
/// connection at once.
pub(crate) fn connect_tcp(
    actor: &str,
    addr: SocketAddr,
    timeout: Duration,
    receiver: &BusReceiver,
) -> std::io::Result<Option<TcpStream>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(format!("connect:{actor}"))
        .spawn(move || {
            // Nobody waits for the result once the actor stopped: the
            // stream is dropped, closing it.
            let _ = tx.send(TcpStream::connect_timeout(&addr, timeout));
        })?;
    loop {
        if receiver.is_shutdown() {
            return Ok(None);
        }
        match rx.recv_timeout(CONNECT_POLL) {
            Ok(result) => return result.map(Some),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(std::io::Error::other("connect thread ended"));
            }
        }
    }
}

/// Build a map of actor IDs to display names from config (for UI display).
pub fn actor_names(config: &FlighthookConfig) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for (index, section) in &config.mevo {
        names.insert(global_id("mevo", index), section.name.clone());
    }
    for (index, section) in &config.r10 {
        names.insert(global_id("r10", index), section.name.clone());
    }
    for (index, section) in &config.square {
        names.insert(global_id("square", index), section.name.clone());
    }
    for (index, section) in &config.exputt {
        names.insert(global_id("exputt", index), section.name.clone());
    }
    for (index, section) in &config.mock_monitor {
        names.insert(global_id("mock_monitor", index), section.name.clone());
    }
    for (index, section) in &config.openconnect_client {
        names.insert(global_id("openconnect_client", index), section.name.clone());
    }
    for (index, section) in &config.random_club {
        names.insert(global_id("random_club", index), section.name.clone());
    }
    for (index, section) in &config.camera {
        names.insert(global_id("camera", index), section.name.clone());
    }
    for (index, ws) in &config.webserver {
        names.insert(global_id("webserver", index), ws.name.clone());
    }
    names.extend(openconnect_names(&config.openconnect_server));
    names
}

/// Display names of the Open Connect servers, keyed by actor id: each one's
/// configured name, or "Open Connect" when that is blank. Servers that end
/// up sharing a name are numbered in index order ("Open Connect 1",
/// "Open Connect 2"), so each one reads distinctly in the device filter and
/// legend.
fn openconnect_names(
    sections: &HashMap<String, flighthook::OpenConnectServerSection>,
) -> Vec<(String, String)> {
    let mut indices: Vec<&String> = sections.keys().collect();
    indices.sort_by(|a, b| match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        _ => a.cmp(b),
    });
    let base: Vec<&str> = indices
        .iter()
        .map(|i| {
            let name = sections[*i].name.trim();
            if name.is_empty() {
                "Open Connect"
            } else {
                name
            }
        })
        .collect();
    indices
        .iter()
        .zip(&base)
        .enumerate()
        .map(|(k, (index, name))| {
            let shared = base.iter().filter(|b| *b == name).count() > 1;
            let name = if shared {
                let n = base[..=k].iter().filter(|b| *b == name).count();
                format!("{name} {n}")
            } else {
                (*name).to_owned()
            };
            (global_id("openconnect_server", index), name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connect_returns_the_stream() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let receiver =
            BusSender::new("mevo.0".into(), broadcast::channel(1).0, Arc::default()).subscribe();
        let stream = connect_tcp("mevo.0", addr, Duration::from_secs(2), &receiver)
            .expect("connect")
            .expect("not stopped");
        assert_eq!(stream.peer_addr().expect("peer"), addr);
    }

    #[test]
    fn a_stopped_actor_gives_up_its_connect_at_once() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let shutdown = Arc::new(AtomicBool::new(true));
        let receiver =
            BusSender::new("mevo.0".into(), broadcast::channel(1).0, shutdown).subscribe();
        let at = std::time::Instant::now();
        let stream =
            connect_tcp("mevo.0", addr, Duration::from_secs(5), &receiver).expect("no error");
        assert!(stream.is_none());
        assert!(at.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn only_launch_monitors_and_outputs_are_reconnectable() {
        for t in [
            "mevo",
            "r10",
            "square",
            "exputt",
            "mock_monitor",
            "openconnect_server",
            "openconnect_client",
            "random_club",
        ] {
            assert!(reconnectable(t), "{t}");
        }
        for t in ["camera", "webserver", "system"] {
            assert!(!reconnectable(t), "{t}");
        }
    }

    fn with_openconnect(names: &[(&str, &str)]) -> FlighthookConfig {
        let mut config = FlighthookConfig::default();
        for (index, name) in names {
            config.openconnect_server.insert(
                (*index).to_owned(),
                flighthook::OpenConnectServerSection {
                    name: (*name).to_owned(),
                    bind: None,
                    impact_lead_s: None,
                },
            );
        }
        config
    }

    #[test]
    fn openconnect_servers_are_named_from_config() {
        let names = actor_names(&with_openconnect(&[("0", "Uneekor"), ("1", "Omni Bridge")]));
        assert_eq!(names["openconnect_server.0"], "Uneekor");
        assert_eq!(names["openconnect_server.1"], "Omni Bridge");
    }

    #[test]
    fn blank_openconnect_names_read_open_connect() {
        let names = actor_names(&with_openconnect(&[("0", "")]));
        assert_eq!(names["openconnect_server.0"], "Open Connect");
        let names = actor_names(&with_openconnect(&[("0", " "), ("1", "Omni Bridge")]));
        assert_eq!(names["openconnect_server.0"], "Open Connect");
        assert_eq!(names["openconnect_server.1"], "Omni Bridge");
    }

    #[test]
    fn shared_openconnect_names_are_numbered_in_index_order() {
        let names = actor_names(&with_openconnect(&[("10", ""), ("2", ""), ("3", "Bay")]));
        assert_eq!(names["openconnect_server.2"], "Open Connect 1");
        assert_eq!(names["openconnect_server.10"], "Open Connect 2");
        assert_eq!(names["openconnect_server.3"], "Bay");
        let names = actor_names(&with_openconnect(&[
            ("0", "OpenConnect Server"),
            ("1", "OpenConnect Server"),
        ]));
        assert_eq!(names["openconnect_server.0"], "OpenConnect Server 1");
        assert_eq!(names["openconnect_server.1"], "OpenConnect Server 2");
    }
}
