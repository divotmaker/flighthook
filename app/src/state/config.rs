//! Configuration loading, resolution, and persistence.
//!
//! Handles the TOML config file (~/.config/flighthook/config.toml) with
//! type-prefixed sections: `[mevo.<id>]`, `[gspro.<id>]`, etc.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::Deserialize;

// Re-export config types from schemas for crate-internal use
pub use flighthook::{FlighthookConfig, MevoSection};

/// Build a global ID from a type prefix and index: `"mevo.0"`, `"gspro.0"`, etc.
pub fn global_id(prefix: &str, index: &str) -> String {
    format!("{prefix}.{index}")
}

/// Generate a short unique ID (8 hex chars from system time).
/// Used for WebSocket source IDs (`ws.{hex}`).
pub fn generate_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:08x}", (ts ^ (seq as u64)) as u32)
}

// ---------------------------------------------------------------------------
// Persistence I/O
// ---------------------------------------------------------------------------

/// Returns `~/.config/flighthook/config.toml`.
pub fn default_config_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flighthook");
    dir.join("config.toml")
}

/// Legacy config format with `webserver` as a singleton `Option<WebserverSection>`.
/// Used for backwards-compatible migration from pre-indexed configs.
#[derive(Deserialize)]
struct LegacyFlighthookConfig {
    #[serde(default)]
    default_units: flighthook::UnitSystem,
    webserver: Option<flighthook::WebserverSection>,
    #[serde(default)]
    mevo: HashMap<String, flighthook::MevoSection>,
    #[serde(default)]
    mock_monitor: HashMap<String, flighthook::MockMonitorSection>,
    #[serde(default)]
    gspro: HashMap<String, flighthook::GsProSection>,
    #[serde(default)]
    random_club: HashMap<String, flighthook::RandomClubSection>,
}

impl LegacyFlighthookConfig {
    fn into_current(self) -> FlighthookConfig {
        let mut webserver = HashMap::new();
        if let Some(mut ws) = self.webserver {
            if ws.name.is_empty() {
                ws.name = "Web Server".into();
            }
            webserver.insert("0".into(), ws);
        }
        FlighthookConfig {
            default_units: self.default_units,
            webserver,
            mevo: self.mevo,
            mock_monitor: self.mock_monitor,
            gspro: self.gspro,
            random_club: self.random_club,
        }
    }
}

/// Load persisted config from disk. If the file does not exist, creates it
/// with all-defaults and returns that. Migrates legacy singleton `[webserver]`
/// to the indexed `[webserver.0]` format.
pub fn load(path: &Path) -> FlighthookConfig {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            match toml::from_str::<FlighthookConfig>(&contents) {
                Ok(config) => {
                    tracing::info!("loaded config from {}", path.display());
                    config
                }
                Err(e) => {
                    // Try legacy format (singleton [webserver] instead of [webserver.0])
                    match toml::from_str::<LegacyFlighthookConfig>(&contents) {
                        Ok(legacy) => {
                            tracing::info!("migrating legacy config from {}", path.display());
                            let config = legacy.into_current();
                            save_to(path, &config);
                            config
                        }
                        Err(_) => {
                            tracing::warn!("failed to parse {}: {e}", path.display());
                            FlighthookConfig::default()
                        }
                    }
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let defaults = FlighthookConfig::default();
            tracing::info!("no config file found, creating {}", path.display());
            save_to(path, &defaults);
            defaults
        }
        Err(e) => {
            tracing::warn!("failed to read {}: {e}", path.display());
            FlighthookConfig::default()
        }
    }
}

/// Write config to a specific path. Creates parent dirs if needed. Never panics.
pub fn save_to(path: &Path, config: &FlighthookConfig) {
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        tracing::warn!("failed to create config dir {}: {e}", dir.display());
        return;
    }
    match toml::to_string_pretty(config) {
        Ok(contents) => {
            if let Err(e) = std::fs::write(path, contents) {
                tracing::warn!("failed to write {}: {e}", path.display());
            }
        }
        Err(e) => {
            tracing::warn!("failed to serialize config: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Cached config (replaces Mutex<()> + load/save on every access)
// ---------------------------------------------------------------------------

/// Cached configuration backed by a TOML file.
///
/// Reads are cheap (RwLock read guard + clone). Writes acquire the write lock,
/// mutate the cached copy, and persist to disk atomically.
///
/// Config mutations are serialized through the bus (processed by SystemActor
/// one at a time on its thread), so no external reload lock is needed.
pub struct SystemConfig {
    path: PathBuf,
    inner: RwLock<FlighthookConfig>,
}

impl SystemConfig {
    /// Load config from disk (or create defaults) and cache it.
    pub fn new(path: PathBuf) -> Self {
        let config = load(&path);
        Self {
            path,
            inner: RwLock::new(config),
        }
    }

    /// Clone the current cached config.
    pub fn snapshot(&self) -> FlighthookConfig {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Mutate the cached config in place, then persist to disk.
    pub fn update(&self, f: impl FnOnce(&mut FlighthookConfig)) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        f(&mut guard);
        save_to(&self.path, &guard);
    }

    /// Replace the entire cached config and persist to disk.
    pub fn replace(&self, new: FlighthookConfig) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = new;
        save_to(&self.path, &guard);
    }
}
