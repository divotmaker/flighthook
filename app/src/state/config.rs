//! Configuration loading, resolution, and persistence.
//!
//! Handles the TOML config file (~/.config/flighthook/config.toml) with
//! type-prefixed sections: `[mevo.<id>]`, `[openconnect_client.<id>]`, etc.
//! `[gspro.<id>]` sections are read as `[openconnect_client.<id>]`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::Deserialize;

// Re-export config types from schemas for crate-internal use
pub use flighthook::{FlighthookConfig, MevoSection};

/// Build a global ID from a type prefix and index: `"mevo.0"`,
/// `"openconnect_client.0"`, etc.
pub fn global_id(prefix: &str, index: &str) -> String {
    format!("{prefix}.{index}")
}

/// The type prefix of Open Connect client sections and actor IDs.
pub const OPENCONNECT_CLIENT: &str = "openconnect_client";

/// The other accepted spelling of [`OPENCONNECT_CLIENT`], in config files,
/// API bodies and actor IDs.
pub const GSPRO_ALIAS: &str = "gspro";

/// `id` with a [`GSPRO_ALIAS`] type prefix spelled [`OPENCONNECT_CLIENT`]:
/// `"gspro.1"` -> `"openconnect_client.1"`. Any other ID is returned as is.
pub fn canonical_id(id: &str) -> String {
    match id.split_once('.') {
        Some((GSPRO_ALIAS, index)) => global_id(OPENCONNECT_CLIENT, index),
        _ => id.to_string(),
    }
}

/// Indices a config document names as both a `gspro` and an
/// `openconnect_client` section, sorted. The `openconnect_client` section is
/// the one used.
#[derive(Deserialize, Default)]
pub struct ClientSectionIndices {
    #[serde(default)]
    gspro: std::collections::BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    openconnect_client: std::collections::BTreeMap<String, serde::de::IgnoredAny>,
}

impl ClientSectionIndices {
    pub fn collisions(&self) -> Vec<String> {
        self.gspro
            .keys()
            .filter(|i| self.openconnect_client.contains_key(*i))
            .cloned()
            .collect()
    }

    /// Log a warning for every collision; `source` names the document.
    pub fn warn(&self, source: &str) {
        for index in self.collisions() {
            tracing::warn!(
                "{source}: [gspro.{index}] and [openconnect_client.{index}] are both set; \
                 using [openconnect_client.{index}]"
            );
        }
    }
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
    gspro: HashMap<String, flighthook::OpenConnectClientSection>,
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
            shot_grouping: flighthook::ShotGrouping::default(),
            simulation: self.simulation,
            webserver,
            mevo: self.mevo,
            r10: HashMap::new(),
            square: HashMap::new(),
            mock_monitor: self.mock_monitor,
            openconnect_server: HashMap::new(),
            openconnect_client: self.gspro,
            random_club: self.random_club,
        }
    }
}

/// Load persisted config from disk. If the file does not exist, creates it
/// with all-defaults and returns that.
///
/// Older formats are read too: a singleton `[webserver]` loads as
/// `[webserver.0]`, and `[gspro.N]` sections load as
/// `[openconnect_client.N]` (see [`FlighthookConfig`]). A file that does not
/// hold what the app would save for it is then brought up to date (see
/// [`bring_up_to_date`]). A file that fails to parse is left alone and the
/// defaults are used.
pub fn load(path: &Path) -> FlighthookConfig {
    match std::fs::read_to_string(path) {
        Ok(contents) => match parse(path, &contents) {
            Some(config) => {
                bring_up_to_date(path, &contents, &config);
                config
            }
            None => FlighthookConfig::default(),
        },
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

/// Parse config text `contents` read from `path`, in the current format or
/// the singleton-`[webserver]` one. `None` (after logging why) when it is
/// neither.
fn parse(path: &Path, contents: &str) -> Option<FlighthookConfig> {
    match toml::from_str::<FlighthookConfig>(contents) {
        Ok(config) => {
            tracing::info!("loaded config from {}", path.display());
            toml::from_str::<ClientSectionIndices>(contents)
                .unwrap_or_default()
                .warn(&path.display().to_string());
            Some(config)
        }
        Err(e) => match toml::from_str::<LegacyFlighthookConfig>(contents) {
            Ok(legacy) => {
                tracing::info!(
                    "loaded config from {} (singleton [webserver])",
                    path.display()
                );
                Some(legacy.into_current())
            }
            Err(_) => {
                let e = located_error(contents, e);
                tracing::warn!("failed to parse {}: {e}", path.display());
                None
            }
        },
    }
}

/// Rewrite the config file at `path`, holding `contents`, in the current
/// format when it differs from what the app saves for `config` (the config
/// `contents` loaded as).
///
/// The comparison is of TOML values, so whitespace, key order and comments
/// never cause a rewrite. Everything else that differs does, once: old
/// section names, unknown keys, and values written out at their defaults
/// where the app leaves them out. An integer where a float goes counts as
/// the same value. A file the app saved itself always matches, so a
/// rewritten file is not rewritten again.
///
/// The original bytes are first copied to a new backup file next to it
/// (see [`write_backup`]); the new content then replaces the file atomically.
/// Any I/O failure is logged as a warning and the file is left as it was
/// (the backup may remain).
fn bring_up_to_date(path: &Path, contents: &str, config: &FlighthookConfig) {
    let saved = match toml::to_string_pretty(config) {
        Ok(saved) => saved,
        Err(e) => {
            tracing::warn!("failed to serialize config: {e}");
            return;
        }
    };
    let (Ok(file), Ok(current)) = (
        contents.parse::<toml::Value>(),
        saved.parse::<toml::Value>(),
    ) else {
        return;
    };
    if same_value(&file, &current) {
        return;
    }
    let backup = match write_backup(path, contents.as_bytes()) {
        Ok(backup) => backup,
        Err(e) => {
            tracing::warn!(
                "not rewriting {} in the current format: failed to write a backup: {e}",
                path.display()
            );
            return;
        }
    };
    match write_atomic(path, saved.as_bytes()) {
        Ok(()) => tracing::info!(
            "migrated {} to the current format; the original is kept as {}",
            path.display(),
            backup.display()
        ),
        Err(e) => tracing::warn!(
            "failed to rewrite {} in the current format: {e}",
            path.display()
        ),
    }
}

/// Whether TOML values `a` and `b` are the same: tables compare by key
/// (order ignored), an integer equals the float of the same value, and NaN
/// equals NaN.
fn same_value(a: &toml::Value, b: &toml::Value) -> bool {
    use toml::Value;
    match (a, b) {
        (Value::Table(a), Value::Table(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|w| same_value(v, w)))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(v, w)| same_value(v, w))
        }
        (Value::Float(a), Value::Float(b)) => a == b || (a.is_nan() && b.is_nan()),
        (Value::Integer(i), Value::Float(f)) | (Value::Float(f), Value::Integer(i)) => {
            #[allow(clippy::cast_precision_loss)] // compared, not stored
            let i = *i as f64;
            i == *f
        }
        _ => a == b,
    }
}

/// Write `bytes` to a new backup file next to the config file at `path`:
/// `config.pre-<version>.toml` (for `config.toml` and this build's version),
/// or `config.pre-<version>.<n>.toml` with the first free `n` when that is
/// taken. An existing file is never overwritten. Returns the backup's path.
fn write_backup(path: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let stem = path
        .file_stem()
        .map_or_else(|| "config".into(), |s| s.to_string_lossy());
    let ext = path
        .extension()
        .map_or_else(|| "toml".into(), |s| s.to_string_lossy());
    let version = env!("CARGO_PKG_VERSION");
    for n in 0u32.. {
        let name = if n == 0 {
            format!("{stem}.pre-{version}.{ext}")
        } else {
            format!("{stem}.pre-{version}.{n}.{ext}")
        };
        let backup = path.with_file_name(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(mut file) => {
                let written = file.write_all(bytes).and_then(|()| file.sync_all());
                if let Err(e) = written {
                    drop(file);
                    let _ = std::fs::remove_file(&backup);
                    return Err(e);
                }
                return Ok(backup);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free backup name"))
}

/// Replace the file at `path` with `bytes` atomically: write a temporary
/// file in the same directory, flush it to disk, then rename it over `path`
/// (which replaces an existing file on Windows too). On failure the
/// temporary file is removed and `path` is untouched.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let name = path
        .file_name()
        .map_or_else(|| "config.toml".into(), |s| s.to_string_lossy());
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", generate_id()));
    let result = std::fs::File::create(&tmp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The error for config text `contents` that failed to parse with `error`,
/// with the line and column of the fault. Every section but `gspro` is read
/// through a flattened buffer, which loses the position, so the text is read
/// again without it: by [`FlighthookConfig`]'s derived deserializer, which
/// skips `gspro` sections. A fault it doesn't see (one in a `gspro` section)
/// is reported as `error`, which has its position.
fn located_error(contents: &str, error: toml::de::Error) -> toml::de::Error {
    FlighthookConfig::deserialize(toml::Deserializer::new(contents))
        .err()
        .unwrap_or(error)
}

/// Write config to a specific path, atomically (see [`write_atomic`]).
/// Creates parent dirs if needed. Never panics.
pub fn save_to(path: &Path, config: &FlighthookConfig) {
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        tracing::warn!("failed to create config dir {}: {e}", dir.display());
        return;
    }
    match toml::to_string_pretty(config) {
        Ok(contents) => {
            if let Err(e) = write_atomic(path, contents.as_bytes()) {
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

    /// The spin source of the `[mevo.<index>]` section, or `None` when that
    /// section is absent. Device threads read it once per shot, so a change
    /// applies on the next shot without reconnecting.
    pub fn mevo_spin_source(&self, index: &str) -> Option<flighthook::SpinSource> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mevo
            .get(index)
            .map(|m| m.spin_source)
    }

    /// The launch-only spin in effect for the `[mevo.<index>]` section, rpm
    /// per mph (`0` = off), or `None` when that section is absent. Device
    /// threads read it once per shot, so a change applies on the next shot
    /// without reconnecting.
    pub fn mevo_launch_only_spin(&self, index: &str) -> Option<f64> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mevo
            .get(index)
            .map(flighthook::MevoSection::launch_only_spin_rpm_per_mph)
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
    use flighthook::SimMode;

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
    fn shot_grouping_parses_from_toml_and_writes_back() {
        let c: FlighthookConfig = toml::from_str(GOLDEN_DEFAULT_TOML).expect("parses");
        assert_eq!(c.shot_grouping, flighthook::ShotGrouping::BySet);

        let file = format!("shot_grouping = \"by_club\"\n{GOLDEN_DEFAULT_TOML}");
        let c: FlighthookConfig = toml::from_str(&file).expect("parses");
        assert_eq!(c.shot_grouping, flighthook::ShotGrouping::ByClub);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(out.contains("shot_grouping = \"by_club\"\n"), "{out}");
        let back: FlighthookConfig = toml::from_str(&out).expect("parses");
        assert_eq!(back, c);
    }

    #[test]
    fn simulation_table_round_trips() {
        let src = "[simulation]\nmode = \"off\"\naltitude = \"5280ft\"\ntemperature_c = 30.0\nsurface = \"green\"\n";
        let file =
            format!("{src}\n[webserver.0]\nname = \"Web Server\"\nbind = \"0.0.0.0:5880\"\n");
        let c: FlighthookConfig = toml::from_str(&file).expect("parses");
        assert_eq!(c.simulation.mode, SimMode::Off);
        assert_eq!(c.simulation.surface, flighthook::SimSurface::Green);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(out.contains(src), "{out}");
        // The table sits before the per-actor tables.
        let sim = out.find("[simulation]").expect("simulation table");
        let web = out.find("[webserver.0]").expect("webserver table");
        assert!(sim < web, "{out}");
    }

    /// A `[simulation]` table carrying unknown keys (e.g. `enabled`) loads,
    /// ignoring them and keeping the default mode.
    #[test]
    fn unknown_simulation_keys_are_ignored_in_toml() {
        let src = "[simulation]\nenabled = false\ntemperature_c = 30.0\n";
        let c: FlighthookConfig = toml::from_str(src).expect("parses");
        assert_eq!(c.simulation.mode, SimMode::On);
        assert!((c.simulation.temperature_c - 30.0).abs() < 1e-9);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(!out.contains("enabled"), "{out}");
        let back: FlighthookConfig = toml::from_str(&out).expect("parses");
        assert_eq!(back, c);
    }

    #[test]
    fn simulation_mode_round_trips_and_is_omitted_at_default() {
        for mode in SimMode::all() {
            let c = FlighthookConfig {
                simulation: flighthook::SimulationSection {
                    mode,
                    ..flighthook::SimulationSection::default()
                },
                ..FlighthookConfig::default()
            };
            let out = toml::to_string_pretty(&c).expect("ser");
            if mode == SimMode::On {
                assert!(!out.contains("[simulation]"), "{out}");
            } else {
                let want = format!("[simulation]\nmode = \"{}\"\n", mode.key());
                assert!(out.contains(&want), "{out}");
            }
            let back: FlighthookConfig = toml::from_str(&out).expect("parses");
            assert_eq!(back, c);
        }
    }

    #[test]
    fn legacy_migration_carries_simulation_through() {
        let src = r#"
[simulation]
mode = "off"
altitude = "5280ft"
temperature_c = 30.0
surface = "green"

[webserver]
name = "Web Server"
bind = "0.0.0.0:5880"
"#;
        let legacy: LegacyFlighthookConfig = toml::from_str(src).expect("parses");
        let config = legacy.into_current();
        assert_eq!(config.simulation.mode, SimMode::Off);
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

    #[test]
    fn mevo_spin_source_is_omitted_at_default_in_toml() {
        let without = "[mevo.0]\nname = \"Xi\"\naddress = \"192.168.2.1:5100\"\n";
        let c: FlighthookConfig = toml::from_str(without).expect("parses");
        assert_eq!(c.mevo["0"].spin_source, flighthook::SpinSource::D4);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(!out.contains("spin_source"), "{out}");

        let with = format!("{without}spin_source = \"e8\"\n");
        let c: FlighthookConfig = toml::from_str(&with).expect("parses");
        assert_eq!(c.mevo["0"].spin_source, flighthook::SpinSource::E8);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(out.contains("spin_source = \"e8\"\n"), "{out}");
        let back: FlighthookConfig = toml::from_str(&out).expect("parses");
        assert_eq!(back, c);
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
        assert_eq!(cfg.simulation().mode, SimMode::On);
        cfg.update(|c| c.simulation.mode = SimMode::Off);
        assert_eq!(cfg.simulation().mode, SimMode::Off);
    }

    #[test]
    fn mevo_spin_source_reads_the_live_section() {
        let dir =
            TempDir(std::env::temp_dir().join(format!("flighthook-config-test-{}", generate_id())));
        let cfg = SystemConfig::new(dir.0.join("config.toml"));
        assert_eq!(cfg.mevo_spin_source("0"), None);
        cfg.update(|c| {
            c.mevo
                .insert("0".into(), flighthook::MevoSection::default());
        });
        assert_eq!(cfg.mevo_spin_source("0"), Some(flighthook::SpinSource::D4));
        cfg.update(|c| {
            if let Some(m) = c.mevo.get_mut("0") {
                m.spin_source = flighthook::SpinSource::E8;
            }
        });
        assert_eq!(cfg.mevo_spin_source("0"), Some(flighthook::SpinSource::E8));
    }

    #[test]
    fn mevo_launch_only_spin_reads_the_live_section() {
        let dir =
            TempDir(std::env::temp_dir().join(format!("flighthook-config-test-{}", generate_id())));
        let cfg = SystemConfig::new(dir.0.join("config.toml"));
        assert_eq!(cfg.mevo_launch_only_spin("0"), None);
        cfg.update(|c| {
            c.mevo
                .insert("0".into(), flighthook::MevoSection::default());
        });
        assert_eq!(cfg.mevo_launch_only_spin("0"), Some(100.0));
        for (set, read) in [
            (0.0, 0.0),
            (80.0, 80.0),
            (1000.0, 1000.0),
            (1500.0, 0.0),
            (-5.0, 0.0),
            (f64::NAN, 0.0),
        ] {
            cfg.update(|c| {
                if let Some(m) = c.mevo.get_mut("0") {
                    m.launch_only_spin = set;
                }
            });
            assert_eq!(cfg.mevo_launch_only_spin("0"), Some(read), "{set}");
        }
    }

    #[test]
    fn mevo_launch_only_spin_is_omitted_at_default_in_toml() {
        let without = "[mevo.0]\nname = \"Xi\"\naddress = \"192.168.2.1:5100\"\n";
        let c: FlighthookConfig = toml::from_str(without).expect("parses");
        assert_eq!(c.mevo["0"].launch_only_spin, 100.0);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(!out.contains("launch_only_spin"), "{out}");

        for (line, v) in [
            ("launch_only_spin = 0\n", 0.0),
            ("launch_only_spin = 75.5\n", 75.5),
        ] {
            let c: FlighthookConfig = toml::from_str(&format!("{without}{line}")).expect("parses");
            assert_eq!(c.mevo["0"].launch_only_spin, v);
            let out = toml::to_string_pretty(&c).expect("ser");
            assert!(out.contains("launch_only_spin = "), "{out}");
            let back: FlighthookConfig = toml::from_str(&out).expect("parses");
            assert_eq!(back, c);
        }
    }

    /// A config file from before `openconnect_client`: one GSPro output
    /// routed to a Mevo.
    const GSPRO_TOML: &str = r#"default_units = "imperial"
chipping_clubs = ["GW", "SW", "LW"]
putting_clubs = ["PT"]

[webserver.0]
name = "Web Server"
bind = "0.0.0.0:5880"

[mevo.0]
name = "Mevo+"
address = "192.168.2.1:5100"

[gspro.0]
name = "Bay GSPro"
address = "192.168.1.20:921"
full_monitor = "mevo.0"
"#;

    /// The error `load` logs for config text `contents`.
    fn load_error(contents: &str) -> String {
        let error = toml::from_str::<FlighthookConfig>(contents).expect_err("a bad config");
        located_error(contents, error).to_string()
    }

    #[test]
    fn a_parse_error_names_the_line_of_the_bad_value() {
        let simulation = "default_units = \"metric\"\n\
                          \n\
                          [webserver.0]\n\
                          name = \"Web Server\"\n\
                          bind = \"127.0.0.1:5880\"\n\
                          \n\
                          [simulation]\n\
                          temperature_c = \"hot\"\n";
        let error = load_error(simulation);
        assert!(error.contains("line 8"), "{error}");

        let client = "[mevo.0]\n\
                      name = \"Mevo+\"\n\
                      \n\
                      [openconnect_client.0]\n\
                      name = \"GSPro\"\n\
                      full_monitor = 7\n";
        let error = load_error(client);
        assert!(error.contains("line 6"), "{error}");

        let gspro = "[mevo.0]\n\
                     name = \"Mevo+\"\n\
                     \n\
                     [gspro.0]\n\
                     name = 7\n";
        let error = load_error(gspro);
        assert!(error.contains("line 5"), "{error}");
    }

    #[test]
    fn gspro_config_loads_as_openconnect_client_and_saves_the_new_name() {
        let dir =
            TempDir(std::env::temp_dir().join(format!("flighthook-config-test-{}", generate_id())));
        std::fs::create_dir_all(&dir.0).expect("mkdir");
        let path = dir.0.join("config.toml");
        std::fs::write(&path, GSPRO_TOML).expect("write");

        let cfg = SystemConfig::new(path.clone());
        let c = cfg.snapshot();
        let client = &c.openconnect_client["0"];
        assert_eq!(client.name, "Bay GSPro");
        assert_eq!(client.address.as_deref(), Some("192.168.1.20:921"));
        assert_eq!(client.full_monitor.as_deref(), Some("mevo.0"));
        assert_eq!(c.mevo["0"].name, "Mevo+");

        // The same actor, under its new ID, as the old file started.
        let ids: Vec<_> = crate::actors::resolve_actors(&c, None)
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert!(ids.contains(&"openconnect_client.0".to_string()), "{ids:?}");
        assert!(!ids.iter().any(|id| id.starts_with("gspro.")), "{ids:?}");

        // Loading wrote the new name; so does every later save.
        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(saved.contains("[openconnect_client.0]"), "{saved}");
        assert!(!saved.contains("gspro"), "{saved}");
        cfg.update(|c| c.simulation.mode = SimMode::Off);
        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(saved.contains("[openconnect_client.0]"), "{saved}");
        assert!(!saved.contains("gspro"), "{saved}");
        let back: FlighthookConfig = toml::from_str(&saved).expect("parses");
        assert_eq!(back.openconnect_client, c.openconnect_client);
    }

    /// A fresh directory holding `config.toml` with `contents`.
    fn config_dir(contents: &str) -> (TempDir, PathBuf) {
        let dir =
            TempDir(std::env::temp_dir().join(format!("flighthook-config-test-{}", generate_id())));
        std::fs::create_dir_all(&dir.0).expect("mkdir");
        let path = dir.0.join("config.toml");
        std::fs::write(&path, contents).expect("write");
        (dir, path)
    }

    /// The names of the files in `dir`, sorted.
    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The name of the first backup of `config.toml`.
    fn backup_name() -> String {
        format!("config.pre-{}.toml", env!("CARGO_PKG_VERSION"))
    }

    #[test]
    fn a_gspro_file_is_rewritten_with_a_backup_of_the_original() {
        let (dir, path) = config_dir(GSPRO_TOML);
        let loaded = load(&path);

        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        let backup = std::fs::read(dir.0.join(backup_name())).expect("backup");
        assert_eq!(backup, GSPRO_TOML.as_bytes());

        let saved = std::fs::read_to_string(&path).expect("read");
        assert_eq!(saved, toml::to_string_pretty(&loaded).expect("ser"));
        assert!(saved.contains("[openconnect_client.0]"), "{saved}");
        assert!(!saved.contains("gspro"), "{saved}");
        let back: FlighthookConfig = toml::from_str(&saved).expect("parses");
        assert_eq!(back, loaded);
        assert_eq!(loaded.openconnect_client["0"].name, "Bay GSPro");
    }

    #[test]
    fn a_singleton_webserver_file_is_rewritten_with_a_backup() {
        let src = "[webserver]\nname = \"\"\nbind = \"0.0.0.0:5881\"\n";
        let (dir, path) = config_dir(src);
        let loaded = load(&path);
        assert_eq!(loaded.webserver["0"].name, "Web Server");
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dir.0.join(backup_name())).expect("backup"),
            src
        );
        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(saved.contains("[webserver.0]"), "{saved}");
    }

    #[test]
    fn a_current_file_is_left_byte_identical_with_no_backup() {
        let c: FlighthookConfig = toml::from_str(EVERY_SECTION_TOML).expect("parses");
        let current = toml::to_string_pretty(&c).expect("ser");
        for contents in [GOLDEN_DEFAULT_TOML, current.as_str()] {
            let (dir, path) = config_dir(contents);
            let loaded = load(&path);
            assert_eq!(std::fs::read_to_string(&path).expect("read"), contents);
            assert_eq!(files(&dir.0), ["config.toml"]);
            let want: FlighthookConfig = toml::from_str(contents).expect("parses");
            assert_eq!(loaded, want);
        }
    }

    #[test]
    fn comments_order_and_whitespace_alone_do_not_rewrite() {
        let src = r#"# My bay.
putting_clubs = [ "PT" ]
chipping_clubs = ["GW",
  "SW", "LW"]
default_units = "imperial"

[mevo.0]
address = "192.168.2.1:5100"   # the Mevo's own WiFi
name = "Mevo+"

[openconnect_client.0]
full_monitor = "mevo.0"
address = "192.168.1.20:921"
name = "Bay GSPro"

# web
[webserver.0]
bind = "0.0.0.0:5880"
name = "Web Server"

[simulation]
temperature_c = 12
altitude = "0ft"
surface = "firm_green"
"#;
        let (dir, path) = config_dir(src);
        let loaded = load(&path);
        assert!((loaded.simulation.temperature_c - 12.0).abs() < f64::EPSILON);
        assert_eq!(loaded.openconnect_client["0"].name, "Bay GSPro");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
        assert_eq!(files(&dir.0), ["config.toml"]);
    }

    /// A value written out at its default, where the app leaves it out, is
    /// normalised away once; the file is stable from then on.
    #[test]
    fn explicit_defaults_are_normalised_once() {
        let src = format!("{GOLDEN_DEFAULT_TOML}\n[mevo.0]\nname = \"Xi\"\nspin_source = \"d4\"\n");
        let (dir, path) = config_dir(&src);
        let loaded = load(&path);
        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(!saved.contains("spin_source"), "{saved}");
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        assert_eq!(load(&path), loaded);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), saved);
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    }

    #[test]
    fn a_second_start_after_a_migration_changes_nothing() {
        let legacy = "[webserver]\nname = \"Web\"\nbind = \"0.0.0.0:5881\"\n\n\
                      [gspro.0]\nname = \"GSPro\"\n";
        let unknown = "[simulation]\nenabled = false\ntemperature_c = 30.0\n";
        for contents in [GSPRO_TOML, EVERY_SECTION_TOML, legacy, unknown] {
            let (dir, path) = config_dir(contents);
            let first = load(&path);
            let migrated = std::fs::read(&path).expect("read");
            assert_ne!(migrated, contents.as_bytes(), "{contents}");
            let after_first = files(&dir.0);
            assert_eq!(after_first, [backup_name(), "config.toml".to_string()]);

            let second = load(&path);
            assert_eq!(second, first);
            assert_eq!(std::fs::read(&path).expect("read"), migrated);
            assert_eq!(files(&dir.0), after_first);
        }
    }

    #[test]
    fn an_existing_backup_is_never_overwritten() {
        let (dir, path) = config_dir(GSPRO_TOML);
        let older = dir.0.join(backup_name());
        std::fs::write(&older, "older backup").expect("write");
        load(&path);
        let next = format!("config.pre-{}.1.toml", env!("CARGO_PKG_VERSION"));
        assert_eq!(
            files(&dir.0),
            [next.clone(), backup_name(), "config.toml".to_string()]
        );
        assert_eq!(
            std::fs::read_to_string(&older).expect("read"),
            "older backup"
        );
        assert_eq!(
            std::fs::read_to_string(dir.0.join(next)).expect("read"),
            GSPRO_TOML
        );
    }

    #[test]
    fn a_file_that_fails_to_parse_is_left_alone() {
        for contents in [
            "[gspro.0]\nname = 7\n",
            "default_units = \"metric\"\n[simulation\n",
            "[mevo.0]\nname = \"Mevo+\"\ntrack_pct = \"lots\"\n",
        ] {
            let (dir, path) = config_dir(contents);
            let cfg = SystemConfig::new(path.clone());
            assert_eq!(cfg.snapshot(), FlighthookConfig::default());
            assert_eq!(std::fs::read_to_string(&path).expect("read"), contents);
            assert_eq!(files(&dir.0), ["config.toml"]);
        }
    }

    /// A directory the app may not write to: the file is left as it was and
    /// the migrated config is used.
    #[cfg(unix)]
    #[test]
    fn a_failed_rewrite_keeps_the_file_and_runs_on_the_loaded_config() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = config_dir(GSPRO_TOML);
        let set_mode = |mode| {
            std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        set_mode(0o555);
        // Root writes anyway; nothing to check then.
        let probe = dir.0.join("probe");
        if std::fs::write(&probe, "").is_ok() {
            let _ = std::fs::remove_file(&probe);
            set_mode(0o755);
            return;
        }

        let cfg = SystemConfig::new(path.clone());
        let c = cfg.snapshot();
        set_mode(0o755);
        assert_eq!(c.openconnect_client["0"].name, "Bay GSPro");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), GSPRO_TOML);
        assert_eq!(files(&dir.0), ["config.toml"]);
    }

    /// The backup is written but the file can't be replaced: the file is
    /// left as it was.
    #[cfg(unix)]
    #[test]
    fn a_failed_replace_leaves_the_file_as_it_was() {
        let (dir, path) = config_dir(GSPRO_TOML);
        // A directory in the way of the temporary file's rename target.
        let blocked = dir.0.join("blocked");
        std::fs::create_dir_all(blocked.join("config.toml")).expect("mkdir");
        let blocked_path = blocked.join("config.toml");
        let c: FlighthookConfig = toml::from_str(GSPRO_TOML).expect("parses");
        bring_up_to_date(&blocked_path, GSPRO_TOML, &c);
        assert!(blocked_path.is_dir());
        assert_eq!(
            files(&blocked),
            [backup_name(), "config.toml".to_string()],
            "the backup stays, no temporary file is left"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read"), GSPRO_TOML);
    }

    #[test]
    fn same_value_ignores_order_and_number_spelling_only() {
        let v = |s: &str| s.parse::<toml::Value>().expect("toml");
        assert!(same_value(&v("a = 1\nb = 2\n"), &v("b = 2\na = 1\n")));
        assert!(same_value(&v("t = 12\n"), &v("t = 12.0\n")));
        assert!(same_value(&v("t = nan\n"), &v("t = nan\n")));
        assert!(!same_value(&v("t = 12\n"), &v("t = 12.5\n")));
        assert!(!same_value(&v("a = [1, 2]\n"), &v("a = [2, 1]\n")));
        assert!(!same_value(&v("a = 1\n"), &v("a = 1\nb = 2\n")));
        assert!(!same_value(&v("a = \"1\"\n"), &v("a = 1\n")));
    }

    #[test]
    fn mixed_client_sections_merge_with_openconnect_client_winning() {
        let src = r#"
[gspro.0]
name = "old 0"

[gspro.2]
name = "old 2"
address = "127.0.0.1:9210"

[openconnect_client.0]
name = "new 0"

[openconnect_client.1]
name = "new 1"
"#;
        let c: FlighthookConfig = toml::from_str(src).expect("parses");
        let mut names: Vec<_> = c
            .openconnect_client
            .iter()
            .map(|(i, s)| (i.as_str(), s.name.as_str()))
            .collect();
        names.sort_unstable();
        assert_eq!(names, [("0", "new 0"), ("1", "new 1"), ("2", "old 2")]);
        assert_eq!(
            c.openconnect_client["2"].address.as_deref(),
            Some("127.0.0.1:9210")
        );
        // The collision is what `load` warns about.
        let indices: ClientSectionIndices = toml::from_str(src).expect("parses");
        assert_eq!(indices.collisions(), ["0"]);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(!out.contains("gspro"), "{out}");
    }

    /// A hand-written file with every section, integers where floats go and
    /// a nested table: the shapes the file format allows.
    const EVERY_SECTION_TOML: &str = r#"default_units = "metric"
chipping_clubs = ["SW"]
putting_clubs = ["PT"]
shot_grouping = "by_club"

[simulation]
mode = "force"
altitude = "250m"
temperature_c = 12
surface = "green"

[webserver.0]
name = "Web"
bind = "0.0.0.0:5881"

[mevo.0]
name = "Mevo+"
address = "192.168.2.1:5100"
ball_type = 1
tee_height = "1.5in"
range = "8ft"
track_pct = 80

[r10.0]
name = "R10"

[square.0]
name = "Omni"

[square.0.dot_bottom_to_face_centre_mm]
DR = 15
7i = 18.5

[mock_monitor.0]
name = "Mock"

[openconnect_server.0]
name = "Uneekor"
bind = "0.0.0.0:921"

[openconnect_client.0]
name = "GSPro"
address = "127.0.0.1:922"
full_monitor = "openconnect_server.0"

[gspro.1]
name = "Muni Golf"
address = "127.0.0.1:9210"

[random_club.0]
name = "Clubs"
"#;

    #[test]
    fn every_section_of_a_file_loads_and_round_trips() {
        let c: FlighthookConfig = toml::from_str(EVERY_SECTION_TOML).expect("parses");
        assert_eq!(c.default_units, flighthook::UnitSystem::Metric);
        assert_eq!(c.shot_grouping, flighthook::ShotGrouping::ByClub);
        assert_eq!(c.simulation.mode, SimMode::Force);
        assert!((c.simulation.temperature_c - 12.0).abs() < f64::EPSILON);
        assert_eq!(c.webserver["0"].bind, "0.0.0.0:5881");
        assert_eq!(c.mevo["0"].ball_type, Some(1));
        assert_eq!(c.mevo["0"].track_pct, Some(80.0));
        assert_eq!(c.r10["0"].name, "R10");
        assert_eq!(
            c.square["0"]
                .dot_bottom_to_face_centre_mm
                .as_ref()
                .map(|m| m.len()),
            Some(2)
        );
        assert_eq!(c.mock_monitor["0"].name, "Mock");
        assert_eq!(
            c.openconnect_server["0"].bind.as_deref(),
            Some("0.0.0.0:921")
        );
        assert_eq!(c.openconnect_client["0"].name, "GSPro");
        assert_eq!(c.openconnect_client["1"].name, "Muni Golf");
        assert_eq!(c.random_club["0"].name, "Clubs");

        let out = toml::to_string_pretty(&c).expect("ser");
        let back: FlighthookConfig = toml::from_str(&out).expect("parses");
        assert_eq!(back, c);
    }

    #[test]
    fn a_file_with_two_web_servers_keeps_both() {
        let src = "[webserver.0]\nname = \"Web Server\"\nbind = \"0.0.0.0:5880\"\n\n\
                   [webserver.1]\nname = \"LAN\"\nbind = \"0.0.0.0:5881\"\n";
        let c: FlighthookConfig = toml::from_str(src).expect("parses");
        assert_eq!(c.webserver.len(), 2);
        assert_eq!(c.webserver["1"].bind, "0.0.0.0:5881");
        let out = toml::to_string_pretty(&c).expect("ser");
        let back: FlighthookConfig = toml::from_str(&out).expect("parses");
        assert_eq!(back, c);
    }

    #[test]
    fn canonical_id_spells_gspro_as_openconnect_client() {
        assert_eq!(canonical_id("gspro.3"), "openconnect_client.3");
        assert_eq!(canonical_id("openconnect_client.3"), "openconnect_client.3");
        assert_eq!(canonical_id("mevo.0"), "mevo.0");
        assert_eq!(canonical_id("gspro"), "gspro");
    }
}
