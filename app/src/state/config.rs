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
/// Used for WebSocket actor IDs (`ws.{hex}`).
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
    #[serde(default = "flighthook::default_chipping_clubs")]
    chipping_clubs: Vec<flighthook::Club>,
    #[serde(default = "flighthook::default_putting_clubs")]
    putting_clubs: Vec<flighthook::Club>,
    #[serde(default)]
    simulation: flighthook::SimulationSection,
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
            chipping_clubs: self.chipping_clubs,
            putting_clubs: self.putting_clubs,
            simulation: self.simulation,
            webserver,
            mevo: self.mevo,
            r10: HashMap::new(),
            square: HashMap::new(),
            mock_monitor: self.mock_monitor,
            openconnect_server: HashMap::new(),
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
// Cached config — RwLock-backed, no external lock
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

    /// Clone just the `[simulation]` section. Device threads read it once per
    /// shot, so changes apply on the next shot without restarting actors.
    pub fn simulation(&self) -> flighthook::SimulationSection {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .simulation
            .clone()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `FlighthookConfig::default()` as written to disk, with no
    /// `[simulation]` table. An untouched config must not gain one.
    const GOLDEN_DEFAULT_TOML: &str = r#"default_units = "imperial"
chipping_clubs = [
    "GW",
    "SW",
    "LW",
]
putting_clubs = ["PT"]

[webserver.0]
name = "Web Server"
bind = "0.0.0.0:5880"
"#;

    #[test]
    fn default_config_file_is_unchanged() {
        let toml = toml::to_string_pretty(&FlighthookConfig::default()).expect("ser");
        assert_eq!(toml, GOLDEN_DEFAULT_TOML);
    }

    #[test]
    fn file_without_simulation_parses_to_defaults() {
        let c: FlighthookConfig = toml::from_str(GOLDEN_DEFAULT_TOML).expect("parses");
        assert_eq!(c.simulation, flighthook::SimulationSection::default());
    }

    #[test]
    fn simulation_table_round_trips() {
        let src = "[simulation]\nenabled = false\naltitude = \"5280ft\"\ntemperature_c = 30.0\nsurface = \"green\"\n";
        let file =
            format!("{src}\n[webserver.0]\nname = \"Web Server\"\nbind = \"0.0.0.0:5880\"\n");
        let c: FlighthookConfig = toml::from_str(&file).expect("parses");
        assert!(!c.simulation.enabled);
        assert_eq!(c.simulation.surface, flighthook::SimSurface::Green);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(out.contains(src), "{out}");
        // The table sits before the per-actor tables.
        let sim = out.find("[simulation]").expect("simulation table");
        let web = out.find("[webserver.0]").expect("webserver table");
        assert!(sim < web, "{out}");
    }

    #[test]
    fn legacy_migration_carries_simulation_through() {
        let src = r#"
[simulation]
enabled = false
altitude = "5280ft"
temperature_c = 30.0
surface = "green"

[webserver]
name = "Web Server"
bind = "0.0.0.0:5880"
"#;
        let legacy: LegacyFlighthookConfig = toml::from_str(src).expect("parses");
        let config = legacy.into_current();
        assert!(!config.simulation.enabled);
        assert_eq!(config.simulation.surface, flighthook::SimSurface::Green);
        assert!((config.simulation.temperature_c - 30.0).abs() < 1e-9);
    }

    #[test]
    fn legacy_config_without_simulation_migrates_to_defaults() {
        let src = r#"
[webserver]
name = "Web Server"
bind = "0.0.0.0:5880"
"#;
        let legacy: LegacyFlighthookConfig = toml::from_str(src).expect("parses");
        let config = legacy.into_current();
        assert_eq!(config.simulation, flighthook::SimulationSection::default());
    }

    #[test]
    fn mevo_environment_is_optional_in_toml() {
        let without = "[mevo.0]\nname = \"Mevo\"\naddress = \"192.168.2.1:5100\"\n";
        let c: FlighthookConfig = toml::from_str(without).expect("parses");
        assert_eq!(c.mevo["0"].environment, None);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(!out.contains("environment"), "{out}");

        let with = format!("{without}environment = \"outdoor\"\n");
        let c: FlighthookConfig = toml::from_str(&with).expect("parses");
        assert_eq!(
            c.mevo["0"].environment,
            Some(flighthook::FlightScopeEnvironment::Outdoor)
        );
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(out.contains("environment = \"outdoor\""), "{out}");
    }

    /// Removes the directory on drop, so a failing assertion doesn't leak it.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn simulation_reads_the_live_section() {
        let dir =
            TempDir(std::env::temp_dir().join(format!("flighthook-config-test-{}", generate_id())));
        let cfg = SystemConfig::new(dir.0.join("config.toml"));
        assert!(cfg.simulation().enabled);
        cfg.update(|c| c.simulation.enabled = false);
        assert!(!cfg.simulation().enabled);
    }
}
