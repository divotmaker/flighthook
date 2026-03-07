//! Game state — player info and club selection for the current round.
//!
//! Schema types are re-exported from the `flighthook` lib. The
//! `GameState` runtime store stays here (it uses `RwLock` and is
//! not part of the wire schema).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub use flighthook::{ClubInfo, GameStateSnapshot, PlayerInfo, ShotDetectionMode};

/// Per-launch-monitor armed/ball-detected state.
#[derive(Debug, Clone, Copy)]
pub struct LaunchMonitorSnapshot {
    pub armed: bool,
    pub ball_detected: bool,
}

/// Shared interior state backing both `GameState` (read) and `GameStateWriter` (write).
struct GameStateInner {
    player_info: RwLock<Option<PlayerInfo>>,
    club_info: RwLock<Option<ClubInfo>>,
    mode: RwLock<Option<ShotDetectionMode>>,
    launch_monitors: RwLock<HashMap<String, LaunchMonitorSnapshot>>,
}

/// Read-only game state — player info, club selection, and detection mode.
///
/// Exposes only `snapshot()`. Lives on `SystemState.game` and is accessible
/// to all actors and the web layer.
pub struct GameState {
    inner: Arc<GameStateInner>,
}

/// Write handle for game state mutations.
///
/// Only the `SystemActor` holds this. All game state mutations flow through
/// bus events processed by `SystemActor`, enforced at the type level.
pub struct GameStateWriter {
    inner: Arc<GameStateInner>,
}

impl GameState {
    /// Create a new `GameState` and its companion `GameStateWriter`.
    pub fn new() -> (Self, GameStateWriter) {
        let inner = Arc::new(GameStateInner {
            player_info: RwLock::new(None),
            club_info: RwLock::new(None),
            mode: RwLock::new(None),
            launch_monitors: RwLock::new(HashMap::new()),
        });
        (
            Self {
                inner: Arc::clone(&inner),
            },
            GameStateWriter { inner },
        )
    }

    /// Take an immutable snapshot of the current global state.
    pub fn snapshot(&self) -> GameStateSnapshot {
        let player_info = self
            .inner
            .player_info
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let club_info = *self
            .inner
            .club_info
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let mode = *self.inner.mode.read().unwrap_or_else(|e| e.into_inner());
        GameStateSnapshot {
            player_info,
            club_info,
            mode,
        }
    }

    /// Current launch monitor states keyed by actor ID.
    pub fn launch_monitor_states(&self) -> HashMap<String, LaunchMonitorSnapshot> {
        self.inner
            .launch_monitors
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl GameStateWriter {
    pub fn set_player_info(&self, info: PlayerInfo) {
        *self
            .inner
            .player_info
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(info);
    }

    pub fn set_club_info(&self, info: ClubInfo) {
        *self
            .inner
            .club_info
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(info);
    }

    pub fn set_mode(&self, mode: ShotDetectionMode) {
        *self.inner.mode.write().unwrap_or_else(|e| e.into_inner()) = Some(mode);
    }

    pub fn set_launch_monitor_state(&self, id: String, armed: bool, ball_detected: bool) {
        self.inner
            .launch_monitors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, LaunchMonitorSnapshot { armed, ball_detected });
    }

    pub fn remove_launch_monitor(&self, id: &str) {
        self.inner
            .launch_monitors
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
}
