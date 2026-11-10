//! Configuration loading, resolution, and persistence.
//!
//! Handles the TOML config file (~/.config/flighthook/config.toml) with
//! type-prefixed sections: `[mevo.<id>]`, `[openconnect_client.<id>]`, etc.
//! `[gspro.<id>]` sections are read as `[openconnect_client.<id>]`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

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
            exputt: HashMap::new(),
            mock_monitor: self.mock_monitor,
            openconnect_server: HashMap::new(),
            openconnect_client: self.gspro,
            pureplay: HashMap::new(),
            random_club: self.random_club,
            replay: flighthook::ReplaySection::default(),
            body_tracking: flighthook::BodyTrackingSection::default(),
            player: flighthook::PlayerSection::default(),
            camera: HashMap::new(),
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
/// [`bring_up_to_date`]). A file that fails to parse, or cannot be read, is
/// left alone and the defaults are used; the second value says why, for the
/// UI.
pub fn load(path: &Path) -> (FlighthookConfig, Option<flighthook::ConfigLoadError>) {
    let failed = |error: String| flighthook::ConfigLoadError {
        path: path.display().to_string(),
        error,
    };
    match std::fs::read_to_string(path) {
        Ok(contents) => match parse(path, &contents) {
            Ok(mut config) => {
                bring_up_to_date(path, &contents, &config);
                turn_off_clashing(path, &mut config);
                (config, None)
            }
            Err(error) => (FlighthookConfig::default(), Some(failed(error))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let defaults = FlighthookConfig::default();
            tracing::info!("no config file found, creating {}", path.display());
            save_to(path, &defaults);
            (defaults, None)
        }
        Err(e) => {
            tracing::warn!("failed to read {}: {e}", path.display());
            (FlighthookConfig::default(), Some(failed(e.to_string())))
        }
    }
}

/// Turn off, in `config` loaded from `path`, every enabled card whose
/// address an enabled card of its type earlier in index order already holds,
/// logging one warning per card. The file is left as it is: the next save
/// writes `enabled = false` for these cards with everything else.
fn turn_off_clashing(path: &Path, config: &mut FlighthookConfig) {
    for clash in config.demote_clashing(None) {
        tracing::warn!(
            "{}: '{}' is off: {} ('{}' holds it)",
            path.display(),
            clash.id,
            clash.message(),
            clash.holder
        );
    }
}

/// Parse config text `contents` read from `path`, in the current format or
/// the singleton-`[webserver]` one. The fault's line, column and message
/// (after logging them) when it is neither.
fn parse(path: &Path, contents: &str) -> Result<FlighthookConfig, String> {
    match toml::from_str::<FlighthookConfig>(contents) {
        Ok(config) => {
            tracing::info!("loaded config from {}", path.display());
            toml::from_str::<ClientSectionIndices>(contents)
                .unwrap_or_default()
                .warn(&path.display().to_string());
            if let Some(error) = config.impact_lead_error() {
                tracing::warn!("{}: {error}; the device default is used", path.display());
            }
            if let Some(error) = config.device_error() {
                tracing::warn!(
                    "{}: {error}; that card does not start until it is fixed",
                    path.display()
                );
            }
            if let Some(error) = config.camera_error() {
                tracing::warn!(
                    "{}: {error}; that setting is not used (latency 0, no rotation, 0 degrees)",
                    path.display()
                );
            }
            if let Some(error) = config.player.error() {
                tracing::warn!("{}: [player] {error}; no height is used", path.display());
            }
            if let Some(error) = config.body_tracking.error() {
                tracing::warn!(
                    "{}: [body_tracking] {error}; the default is used",
                    path.display()
                );
            }
            if let Some(error) = config.replay.error() {
                tracing::warn!(
                    "{}: [replay] {error}; keep_shots is held to 1 to {}, each roll to 0 to {} s, and the replay start to 0 to {} s",
                    path.display(),
                    flighthook::ReplaySection::MAX_KEEP_SHOTS,
                    flighthook::ReplaySection::MAX_ROLL_S,
                    flighthook::ReplaySection::MAX_START_BEFORE_IMPACT_S
                );
            }
            Ok(config)
        }
        Err(e) => match singleton_webserver(contents)
            .then(|| toml::from_str::<LegacyFlighthookConfig>(contents))
            .and_then(Result::ok)
        {
            Some(legacy) => {
                tracing::info!(
                    "loaded config from {} (singleton [webserver])",
                    path.display()
                );
                Ok(legacy.into_current())
            }
            None => {
                let e = located_error(contents, e);
                let fault = driver_type_error::<flighthook::MockMonitorSection>(contents)
                    .or_else(|| driver_type_error::<flighthook::R10Section>(contents))
                    .or_else(|| driver_type_error::<flighthook::SquareSection>(contents))
                    .or_else(|| driver_type_error::<flighthook::ExPuttSection>(contents))
                    .unwrap_or_else(|| fault(contents, &e));
                tracing::warn!("failed to parse {}: {fault}", path.display());
                Err(fault)
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

/// The first type error, by index, in the driver settings of a card of
/// type `S` in config text `contents`, as `"line L, column C: <id>:
/// <field>: <message>"` with the line of the field's key (of the card's
/// table when the key is not found as a plain `key = value` line). Driver
/// settings are read through the section's flattened buffer, which loses
/// the position, so each section is read again into the driver's config on
/// its own. `None` when every card of the type reads.
fn driver_type_error<S: flighthook::DeviceSection>(contents: &str) -> Option<String> {
    use flighthook::DriverConfig;
    let table: toml::Table = contents.parse().ok()?;
    let sections = table.get(S::KIND)?.as_table()?;
    let mut indices: Vec<&String> = sections.keys().collect();
    indices.sort_by(|a, b| flighthook::index_order(a, b));
    indices.into_iter().find_map(|index| {
        let serde_json::Value::Object(map) = serde_json::to_value(&sections[index]).ok()? else {
            return None;
        };
        if serde_json::from_value::<S::Driver>(serde_json::Value::Object(map.clone())).is_ok() {
            return None;
        }
        let error = S::Driver::from_map(map).err()?;
        let id = format!("{}.{index}", S::KIND);
        let (line, column) = key_position(contents, &id, error.field.as_deref())?;
        Some(format!("line {line}, column {column}: {id}: {error}"))
    })
}

/// The line and column (1-based) of `key = ...` in table `[id]` of config
/// text `contents`, else of the table's header; `None` without the header.
/// The header is matched as the exact line `[id]` (no quoted index, no
/// trailing comment) and the key as a plain `key = ...` line; anything else
/// falls back to the header, then to `None`.
fn key_position(contents: &str, id: &str, key: Option<&str>) -> Option<(usize, usize)> {
    let header = format!("[{id}]");
    let lines: Vec<&str> = contents.lines().collect();
    let start = lines.iter().position(|l| l.trim() == header)?;
    let found = key.and_then(|key| {
        lines[start + 1..]
            .iter()
            .take_while(|l| !l.trim_start().starts_with('['))
            .position(|l| {
                l.trim_start()
                    .strip_prefix(key)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
            .map(|n| {
                let line = lines[start + 1 + n];
                (start + 2 + n, line.len() - line.trim_start().len() + 1)
            })
    });
    Some(found.unwrap_or((start + 1, 1)))
}

/// Whether config text `contents` has the older singleton `[webserver]`
/// table (its settings directly under `webserver`) rather than indexed
/// `[webserver.<id>]` ones. Only such a file is read in the older format: the
/// older format knows no later section, so it would load any other file whose
/// current-format parse failed with those sections dropped.
fn singleton_webserver(contents: &str) -> bool {
    contents
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| match t.get("webserver") {
            Some(toml::Value::Table(ws)) => Some(ws.values().any(|v| !v.is_table())),
            _ => None,
        })
        .unwrap_or(false)
}

/// The fault `error` in config text `contents` as one line: its line and
/// column, then its message (just the message when it has no position).
fn fault(contents: &str, error: &toml::de::Error) -> String {
    let message = error.message().trim();
    let Some(span) = error.span() else {
        return message.to_string();
    };
    let before = contents.get(..span.start).unwrap_or(contents);
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    format!("line {line}, column {column}: {message}")
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
    /// Why the file was not used at startup, until a save has backed it up
    /// and replaced it.
    load_error: Mutex<Option<flighthook::ConfigLoadError>>,
}

impl SystemConfig {
    /// Load config from disk (or create defaults) and cache it.
    pub fn new(path: PathBuf) -> Self {
        let (config, load_error) = load(&path);
        Self {
            path,
            inner: RwLock::new(config),
            load_error: Mutex::new(load_error),
        }
    }

    /// Why the config file was not used at startup, while it still sits
    /// unreplaced: the first save backs it up and clears this.
    pub fn load_error(&self) -> Option<flighthook::ConfigLoadError> {
        self.load_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Before the first save over a config file that failed to load: copy
    /// it to a backup next to it (see [`write_backup`]) and clear
    /// [`Self::load_error`]. Returns the backup's path, `None` when there
    /// was nothing to keep (the file loaded, was backed up already, or is
    /// gone). `Err` with the reason when the file cannot be read or the
    /// backup cannot be written: the file is then left alone and still
    /// reported, and the caller must not save.
    pub fn back_up_unusable_file(&self) -> Result<Option<PathBuf>, String> {
        let mut held = self.load_error.lock().unwrap_or_else(|e| e.into_inner());
        if held.is_none() {
            return Ok(None);
        }
        let backup = match std::fs::read(&self.path) {
            Ok(bytes) => write_backup(&self.path, &bytes).map(Some).map_err(|e| {
                format!(
                    "the config file {} could not be read at startup and could not be \
                     backed up before it is replaced: {e}",
                    self.path.display()
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(format!(
                    "the config file {} could not be read at startup and still cannot be, \
                     so it cannot be backed up before it is replaced: {e}",
                    self.path.display()
                ));
            }
        };
        *held = None;
        drop(held);
        if let Some(backup) = &backup {
            tracing::warn!(
                "saving over the config file {} that could not be used at startup; \
                 the original is kept as {}",
                self.path.display(),
                backup.display()
            );
        }
        Ok(backup)
    }

    /// Write `config` to the config file, after backing up a file that
    /// failed to load. A file that cannot be backed up is left alone.
    fn persist(&self, config: &FlighthookConfig) {
        if let Err(e) = self.back_up_unusable_file() {
            tracing::warn!("not saving the config: {e}");
            return;
        }
        save_to(&self.path, config);
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

    /// The impact lead in effect for launch monitor `actor` (e.g. `mevo.0`),
    /// seconds: its section's `impact_lead_s` when valid, else the monitor
    /// type's default. `None` for a source that is not a swing (Auto-chip).
    /// Camera actors read it at every trigger, so a change applies to the
    /// next shot without restarting anything.
    pub fn impact_lead_s(&self, actor: &str) -> Option<f64> {
        let prefix = actor.split_once('.').map_or(actor, |(prefix, _)| prefix);
        let configured = self
            .inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .configured_impact_lead_s(actor);
        flighthook::impact_lead_in_effect_s(prefix, configured)
    }

    /// How far before a trigger's arrival the cameras' history has to
    /// reach for the configured launch monitors, seconds (0 with none): the
    /// largest impact lead in effect plus the monitor type's trigger delay
    /// ([`flighthook::trigger_delay_s`]).
    pub fn longest_reach_s(&self) -> f64 {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .launch_monitor_leads()
            .into_iter()
            .filter_map(|(kind, _, lead)| {
                flighthook::impact_lead_in_effect_s(kind, lead)
                    .map(|lead| lead + flighthook::trigger_delay_s(kind))
            })
            .fold(0.0, f64::max)
    }

    /// The latency in effect for camera `actor` (e.g. `camera.0`), seconds:
    /// its section's `latency_s` when valid, else 0. The camera actor reads
    /// it for every frame, so a change applies at once without reopening
    /// the camera.
    pub fn camera_latency_s(&self, actor: &str) -> f64 {
        let configured = actor.strip_prefix("camera.").and_then(|index| {
            self.inner
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .camera
                .get(index)
                .and_then(|c| c.latency_s)
        });
        flighthook::latency_in_effect_s(configured)
    }

    /// The rotation in effect for camera `actor` (e.g. `camera.0`),
    /// clockwise degrees: its section's `rotation_deg` when valid, else 0.
    /// Read for every live frame and every clip pass, so a change applies at
    /// once without reopening the camera.
    pub fn camera_rotation(&self, actor: &str) -> u16 {
        let configured = actor.strip_prefix("camera.").and_then(|index| {
            self.inner
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .camera
                .get(index)
                .map(|c| c.rotation_deg)
        });
        configured.map_or(0, flighthook::rotation_in_effect_deg)
    }

    /// The `[player]` settings in effect: a height out of range is no
    /// height. Read by the swing analysis of every clip.
    pub fn player(&self) -> flighthook::PlayerSection {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .in_effect()
    }

    /// The `[body_tracking]` settings in effect: a value out of range is its
    /// default. Read live by the camera actors and the body tracking
    /// workers, so every change applies without a restart.
    pub fn body_tracking(&self) -> flighthook::BodyTrackingSection {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .body_tracking
            .in_effect()
    }

    /// Number of configured cameras.
    pub fn camera_count(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .camera
            .len()
    }

    /// Clone just the `[replay]` section. Camera actors read it at every
    /// trigger and clip, so changes apply without restarting them.
    pub fn replay(&self) -> flighthook::ReplaySection {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .replay
            .clone()
    }

    /// The `[camera.<index>]` section, or `None` when it is absent.
    pub fn camera(&self, index: &str) -> Option<flighthook::CameraSection> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .camera
            .get(index)
            .cloned()
    }

    /// Mutate the cached config in place, then persist to disk.
    pub fn update(&self, f: impl FnOnce(&mut FlighthookConfig)) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        f(&mut guard);
        self.persist(&guard);
    }

    /// Replace the entire cached config and persist to disk.
    pub fn replace(&self, new: FlighthookConfig) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = new;
        self.persist(&guard);
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

    /// `auto_chip` is written only when enabled, and a file without it
    /// leaves Auto-chip disabled.
    #[test]
    fn auto_chip_setting_round_trips_in_toml() {
        let c: FlighthookConfig = toml::from_str(GOLDEN_DEFAULT_TOML).expect("parses");
        assert!(!c.simulation.auto_chip, "disabled when absent");
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(!out.contains("auto_chip"), "{out}");

        let file = format!("[simulation]\nauto_chip = true\n{GOLDEN_DEFAULT_TOML}");
        let c: FlighthookConfig = toml::from_str(&file).expect("parses");
        assert!(c.simulation.auto_chip);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(out.contains("[simulation]"), "{out}");
        assert!(out.contains("auto_chip = true\n"), "{out}");
        let back: FlighthookConfig = toml::from_str(&out).expect("parses");
        assert_eq!(back, c);
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
    fn a_hand_edited_zero_keep_shots_loads_and_keeps_one_clip() {
        let dir = std::env::temp_dir().join(format!("flighthook-replay-load-{}", generate_id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[replay]\nkeep_shots = 0\n").expect("write");
        let c = load(&path).0;
        assert_eq!(c.replay.keep_shots, 0, "the file is not rewritten");
        assert!(c.replay.error().is_some());
        assert_eq!(crate::state::clips::keep(&c.replay), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn impact_lead_s_is_omitted_when_absent_and_kept_when_set_in_toml() {
        for kind in [
            "mevo",
            "r10",
            "square",
            "exputt",
            "mock_monitor",
            "openconnect_server",
        ] {
            let plain = format!("[{kind}.0]\nname = \"X\"\n");
            let c: FlighthookConfig = toml::from_str(&plain).expect("parse");
            let out = toml::to_string_pretty(&c).expect("ser");
            assert!(!out.contains("impact_lead_s"), "{kind}: {out}");
            // What was saved without the field parses and saves the same.
            let again: FlighthookConfig = toml::from_str(&out).expect("parse");
            assert_eq!(toml::to_string_pretty(&again).expect("ser"), out);

            let set = format!("[{kind}.0]\nname = \"X\"\nimpact_lead_s = 0.75\n");
            let c: FlighthookConfig = toml::from_str(&set).expect("parse");
            assert_eq!(c.configured_impact_lead_s(&format!("{kind}.0")), Some(0.75));
            let out = toml::to_string_pretty(&c).expect("ser");
            assert!(out.contains("impact_lead_s = 0.75"), "{kind}: {out}");
        }
    }

    #[test]
    fn a_file_with_an_invalid_impact_lead_loads_and_uses_the_default() {
        let dir = std::env::temp_dir().join(format!("flighthook-lead-{}", generate_id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[square.0]\nname = \"Omni\"\nimpact_lead_s = 7.5\n").expect("write");
        let cfg = SystemConfig::new(path);
        assert!(cfg.snapshot().impact_lead_error().is_some());
        assert_eq!(cfg.impact_lead_s("square.0"), Some(1.0));
        assert_eq!(cfg.longest_reach_s(), 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn camera_latency_s_is_omitted_when_absent_and_kept_when_set_in_toml() {
        let plain = "[camera.0]\nname = \"Cam\"\ncamera = \"test-pattern\"\n";
        let c: FlighthookConfig = toml::from_str(plain).expect("parse");
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(!out.contains("latency_s"), "{out}");
        // What was saved without the field parses and saves the same.
        let again: FlighthookConfig = toml::from_str(&out).expect("parse");
        assert_eq!(toml::to_string_pretty(&again).expect("ser"), out);

        let set = format!("{plain}latency_s = 2.5\n");
        let c: FlighthookConfig = toml::from_str(&set).expect("parse");
        assert_eq!(c.camera["0"].latency_s, Some(2.5));
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(out.contains("latency_s = 2.5"), "{out}");
    }

    #[test]
    fn a_file_with_an_invalid_camera_latency_loads_and_uses_zero() {
        let dir = std::env::temp_dir().join(format!("flighthook-latency-{}", generate_id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[camera.0]\nname = \"A\"\ncamera = \"test-pattern\"\nlatency_s = 12\n\n\
             [camera.1]\nname = \"B\"\ncamera = \"test-pattern\"\nlatency_s = 1.5\n",
        )
        .expect("write");
        let cfg = SystemConfig::new(path);
        assert!(cfg.snapshot().latency_error().is_some());
        assert_eq!(cfg.camera_latency_s("camera.0"), 0.0);
        assert_eq!(cfg.camera_latency_s("camera.1"), 1.5);
        assert_eq!(cfg.camera_latency_s("camera.9"), 0.0);
        assert_eq!(cfg.camera_latency_s("mevo.0"), 0.0);
        let _ = std::fs::remove_dir_all(&dir);
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

    /// A type error in a card's driver settings, which serde reads through
    /// the section's flattened buffer, is located by its own key.
    #[test]
    fn a_driver_settings_type_error_names_its_key_and_line() {
        let text = "[webserver.0]\n\
                    name = \"Web Server\"\n\
                    bind = \"127.0.0.1:5880\"\n\
                    \n\
                    [mock_monitor.0]\n\
                    name = \"Mock\"\n\
                    \n\
                    [mock_monitor.1]\n\
                    name = \"Mock 2\"\n\
                    shot_interval_s = \"x\"\n";
        let error = kept_error(text);
        assert_eq!(
            error.error,
            "line 10, column 1: mock_monitor.1: shot_interval_s: invalid type: string \"x\", expected f64"
        );
    }

    /// A mock card's section in a file reads and writes as before its
    /// driver settings were flattened in: a file with only its name loads
    /// with the driver's defaults and is not rewritten; driver settings
    /// read from the file (an integer into a seconds field) and are written
    /// back only when they differ from the defaults.
    #[test]
    fn a_mock_section_file_round_trips_unchanged() {
        let mut written = FlighthookConfig::default();
        written.mock_monitor.insert(
            "0".into(),
            flighthook::MockMonitorSection {
                name: "Mock".into(),
                ..flighthook::MockMonitorSection::default()
            },
        );
        let text = toml::to_string_pretty(&written).expect("toml");
        assert!(
            text.ends_with("[mock_monitor.0]\nname = \"Mock\"\n"),
            "{text}"
        );
        let (dir, path) = config_dir(&text);
        let (config, error) = load(&path);
        assert_eq!(error, None);
        assert_eq!(config, written);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            text,
            "not rewritten"
        );
        assert_eq!(files(&dir.0), ["config.toml"], "no backup");

        let text = "[mock_monitor.0]\nname = \"Mock\"\nshot_interval_s = 5\n";
        let config: FlighthookConfig = toml::from_str(text).expect("parses");
        assert_eq!(config.mock_monitor["0"].driver.shot_interval_s, 5.0);
        let saved = toml::to_string_pretty(&config).expect("toml");
        assert!(
            saved.contains("[mock_monitor.0]\nname = \"Mock\"\nshot_interval_s = 5.0\n"),
            "{saved}"
        );
        assert_eq!(
            toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
            config
        );
    }

    /// An R10 section as flighthook 0.1.24 saved it (range, camera timing,
    /// a second card that is off) loads into the same values, the range now
    /// in the driver's settings, and the file is not rewritten.
    #[test]
    fn an_r10_section_file_from_0_1_24_loads_unchanged() {
        let text = "default_units = \"imperial\"\n\
                    chipping_clubs = [\n    \"GW\",\n    \"SW\",\n    \"LW\",\n]\n\
                    putting_clubs = [\"PT\"]\n\
                    \n\
                    [r10.0]\n\
                    name = \"Garmin R10\"\n\
                    range = \"7ft\"\n\
                    impact_lead_s = 2.5\n\
                    \n\
                    [r10.1]\n\
                    name = \"Bay 2\"\n\
                    enabled = false\n\
                    range = \"2.1m\"\n";
        let (dir, path) = config_dir(text);
        let (config, error) = load(&path);
        assert_eq!(error, None);
        let first = &config.r10["0"];
        assert_eq!(first.name, "Garmin R10");
        assert_eq!(first.driver.range, Some(flighthook::Distance::Feet(7.0)));
        assert_eq!(first.impact_lead_s, Some(2.5));
        assert!(first.enabled);
        let second = &config.r10["1"];
        assert!(!second.enabled);
        assert_eq!(second.driver.range, Some(flighthook::Distance::Meters(2.1)));
        assert_eq!(config.device_error(), None);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            text,
            "not rewritten"
        );
        assert_eq!(files(&dir.0), ["config.toml"], "no backup");
        let saved = toml::to_string_pretty(&config).expect("toml");
        assert!(
            saved
                .contains("[r10.0]\nname = \"Garmin R10\"\nimpact_lead_s = 2.5\nrange = \"7ft\"\n"),
            "{saved}"
        );
        assert_eq!(
            toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
            config
        );
    }

    /// Square sections as flighthook 0.1.24 saved them load into the same
    /// values, now in the driver's settings, with no device error; a file
    /// whose values are not at their defaults is not rewritten and gets no
    /// backup, and the saved form round-trips.
    #[test]
    fn a_square_section_file_from_0_1_24_loads_unchanged() {
        let text = "default_units = \"imperial\"\n\
                    chipping_clubs = [\n    \"GW\",\n    \"SW\",\n    \"LW\",\n]\n\
                    putting_clubs = [\"PT\"]\n\
                    \n\
                    [square.0]\n\
                    name = \"Square Golf Omni\"\n\
                    address = \"SquareGolf(CD01)\"\n\
                    club = \"PW\"\n\
                    advanced_spin = false\n\
                    discard_non_putting_zero_spin = false\n\
                    impact_lead_s = 1.5\n\
                    \n\
                    [square.0.dot_bottom_to_face_center_mm]\n\
                    7I = 21.5\n\
                    DR = 19.0\n\
                    \n\
                    [square.1]\n\
                    name = \"Bay 2\"\n\
                    enabled = false\n\
                    address = \"02:00:00:AB:CD:01\"\n";
        let (dir, path) = config_dir(text);
        let (config, error) = load(&path);
        assert_eq!(error, None);
        let first = &config.square["0"];
        assert_eq!(first.name, "Square Golf Omni");
        assert!(first.enabled);
        assert_eq!(first.impact_lead_s, Some(1.5));
        assert_eq!(first.driver.address.as_deref(), Some("SquareGolf(CD01)"));
        assert_eq!(first.driver.club, "PW");
        assert!(!first.driver.advanced_spin);
        assert!(!first.driver.discard_non_putting_zero_spin);
        assert_eq!(
            first.driver.dot_bottom_to_face_center_mm,
            std::collections::BTreeMap::from([("7I".into(), 21.5), ("DR".into(), 19.0)])
        );
        let second = &config.square["1"];
        assert!(!second.enabled);
        assert_eq!(second.driver.address.as_deref(), Some("02:00:00:AB:CD:01"));
        assert_eq!(
            second.driver,
            flighthook::SquareConfig {
                address: Some("02:00:00:AB:CD:01".into()),
                ..flighthook::SquareConfig::default()
            }
        );
        assert_eq!(config.device_error(), None);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            text,
            "not rewritten"
        );
        assert_eq!(files(&dir.0), ["config.toml"], "no backup");
        let saved = toml::to_string_pretty(&config).expect("toml");
        assert_eq!(
            toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
            config
        );
    }

    /// What the 0.1.24 card saved for a new Omni writes the zero-spin switch
    /// out at its default: it loads into the same values, and the file is
    /// rewritten once without the default, with a backup of the original.
    #[test]
    fn a_square_section_with_explicit_defaults_is_rewritten_once_with_a_backup() {
        let text = "[square.0]\n\
                    name = \"Square Golf Omni\"\n\
                    discard_non_putting_zero_spin = true\n";
        let (dir, path) = config_dir(text);
        let (config, error) = load(&path);
        assert_eq!(error, None);
        assert_eq!(config.square["0"], flighthook::SquareSection::default());
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dir.0.join(backup_name())).expect("backup"),
            text
        );
        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(saved.contains("[square.0]"), "{saved}");
        assert!(!saved.contains("discard_non_putting_zero_spin"), "{saved}");
        // Loading the rewritten file changes nothing more.
        let (again, _) = load(&path);
        assert_eq!(again, config);
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    }

    /// A Square setting of the wrong type names its key's line.
    #[test]
    fn a_square_setting_type_error_names_its_key_and_line() {
        let text = "[square.0]\n\
                    name = \"Omni\"\n\
                    \n\
                    [square.3]\n\
                    name = \"Bay 2\"\n\
                    advanced_spin = \"maybe\"\n";
        let error = kept_error(text);
        assert!(
            error
                .error
                .starts_with("line 6, column 1: square.3: advanced_spin: "),
            "{}",
            error.error
        );
    }

    /// A range the R10 driver refuses (0 or less) loads as written and names
    /// the card, which does not start; a range of the wrong type names its
    /// key's line.
    #[test]
    fn a_refused_r10_range_loads_and_is_reported() {
        let text = "[r10.0]\nname = \"Garmin R10\"\nrange = \"0ft\"\n";
        let (_dir, path) = config_dir(text);
        let (config, error) = load(&path);
        assert_eq!(error, None);
        assert_eq!(
            config.r10["0"].driver.range,
            Some(flighthook::Distance::Feet(0.0))
        );
        assert_eq!(
            config.device_error().as_deref(),
            Some("r10.0: range: must be a distance greater than 0")
        );

        let text = "[r10.0]\n\
                    name = \"Garmin R10\"\n\
                    \n\
                    [r10.2]\n\
                    name = \"Bay 2\"\n\
                    range = 7\n";
        let error = kept_error(text);
        assert!(
            error.error.starts_with("line 6, column 1: r10.2: range: "),
            "{}",
            error.error
        );
    }

    /// ExPutt sections as flighthook 0.1.24 saved them (an address and a
    /// camera timing, a second card that is off and finds its device) load
    /// into the same values, the address now in the driver's settings, and
    /// the file is not rewritten.
    #[test]
    fn an_exputt_section_file_from_0_1_24_loads_unchanged() {
        let text = "default_units = \"imperial\"\n\
                    chipping_clubs = [\n    \"GW\",\n    \"SW\",\n    \"LW\",\n]\n\
                    putting_clubs = [\"PT\"]\n\
                    \n\
                    [exputt.0]\n\
                    name = \"ExPutt\"\n\
                    address = \"192.168.1.50\"\n\
                    impact_lead_s = 0.75\n\
                    \n\
                    [exputt.1]\n\
                    name = \"Practice\"\n\
                    enabled = false\n";
        let (dir, path) = config_dir(text);
        let (config, error) = load(&path);
        assert_eq!(error, None);
        let first = &config.exputt["0"];
        assert_eq!(first.name, "ExPutt");
        assert_eq!(first.driver.address.as_deref(), Some("192.168.1.50"));
        assert_eq!(first.impact_lead_s, Some(0.75));
        assert!(first.enabled);
        let second = &config.exputt["1"];
        assert!(!second.enabled);
        assert_eq!(second.driver.address, None);
        assert_eq!(config.device_error(), None);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            text,
            "not rewritten"
        );
        assert_eq!(files(&dir.0), ["config.toml"], "no backup");
        let saved = toml::to_string_pretty(&config).expect("toml");
        assert!(
            saved.contains(
                "[exputt.0]\nname = \"ExPutt\"\nimpact_lead_s = 0.75\naddress = \"192.168.1.50\"\n"
            ),
            "{saved}"
        );
        assert_eq!(
            toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
            config
        );
    }

    /// An ExPutt address the driver refuses (not an IP) loads as written and
    /// names the card, which does not start; an address of the wrong type
    /// names its key's line.
    #[test]
    fn a_refused_exputt_address_loads_and_is_reported() {
        let text = "[exputt.0]\nname = \"ExPutt\"\naddress = \"exputt.local\"\n";
        let (_dir, path) = config_dir(text);
        let (config, error) = load(&path);
        assert_eq!(error, None);
        assert_eq!(
            config.exputt["0"].driver.address.as_deref(),
            Some("exputt.local")
        );
        let refused = config.device_error().expect("refused");
        assert!(
            refused.starts_with("exputt.0: address: ") && refused.contains("exputt.local"),
            "{refused}"
        );

        let text = "[exputt.0]\n\
                    name = \"ExPutt\"\n\
                    \n\
                    [exputt.3]\n\
                    name = \"Practice\"\n\
                    address = 50\n";
        let error = kept_error(text);
        assert!(
            error
                .error
                .starts_with("line 6, column 1: exputt.3: address: "),
            "{}",
            error.error
        );
    }

    /// A value the mock driver refuses does not fail the load: the config
    /// loads as written and names the card, which does not start.
    #[test]
    fn refused_driver_settings_load_and_are_reported() {
        let text = "[mock_monitor.0]\nname = \"Mock\"\nshot_interval_s = -1\n";
        let (_dir, path) = config_dir(text);
        let (config, error) = load(&path);
        assert_eq!(error, None);
        assert_eq!(config.mock_monitor["0"].driver.shot_interval_s, -1.0);
        assert_eq!(
            config.device_error().as_deref(),
            Some("mock_monitor.0: shot_interval_s: must be 0 to 86400")
        );
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
        let loaded = load(&path).0;

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
        let loaded = load(&path).0;
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
    fn a_file_with_the_british_face_impact_key_loads_and_is_rewritten_with_a_backup() {
        let src = format!(
            "{GOLDEN_DEFAULT_TOML}\n[square.0]\nname = \"Omni\"\n\n\
             [square.0.dot_bottom_to_face_centre_mm]\nDR = 15.0\n7i = 18.5\n"
        );
        let (dir, path) = config_dir(&src);
        let loaded = load(&path).0;
        let overrides = &loaded.square["0"].driver.dot_bottom_to_face_center_mm;
        assert_eq!(overrides.get("DR"), Some(&15.0));
        assert_eq!(overrides.get("7i"), Some(&18.5));

        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dir.0.join(backup_name())).expect("backup"),
            src
        );
        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(
            saved.contains("[square.0.dot_bottom_to_face_center_mm]"),
            "{saved}"
        );
        assert!(!saved.contains("centre"), "{saved}");
        let back: FlighthookConfig = toml::from_str(&saved).expect("parses");
        assert_eq!(back, loaded);
    }

    #[test]
    fn a_body_tracking_section_loads_as_written_and_bad_values_default_in_effect() {
        // Only the values off their defaults: read and left byte-identical.
        let src = format!(
            "{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmodel = \"rtmpose-s\"\ndetect_every = 5\n"
        );
        let src = src.as_str();
        let (dir, path) = config_dir(src);
        let loaded = load(&path).0;
        assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
        assert_eq!(files(&dir.0), ["config.toml"]);
        assert_eq!(loaded.body_tracking.model, flighthook::PoseModel::RtmposeS);
        assert_eq!(loaded.body_tracking.detect_every, 5);
        assert_eq!(loaded.body_tracking.mode, flighthook::BodyTrackingMode::Off);
        // Out of range: loaded as written (the file is not rewritten), the
        // default in effect.
        let src =
            format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmin_score = 3.0\nsmoothing = 0.5\n");
        let src = src.as_str();
        let (dir, path) = config_dir(src);
        let cfg = SystemConfig::new(path.clone());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
        assert_eq!(files(&dir.0), ["config.toml"]);
        let in_effect = cfg.body_tracking();
        assert_eq!(
            in_effect.min_score,
            flighthook::BodyTrackingSection::DEFAULT_MIN_SCORE
        );
        assert_eq!(in_effect.smoothing, 0.5, "the valid value is kept");
        // A change is read live.
        cfg.update(|c| c.body_tracking.mode = flighthook::BodyTrackingMode::Live);
        assert_eq!(cfg.body_tracking().mode, flighthook::BodyTrackingMode::Live);
    }

    #[test]
    fn show_skeleton_reads_on_without_the_field_and_off_as_written() {
        // A config from before the field: the skeleton shows and the file
        // is left byte-identical, with no backup.
        for src in [
            GOLDEN_DEFAULT_TOML.to_owned(),
            format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmode = \"live\"\nmin_score = 0.45\n"),
        ] {
            let (dir, path) = config_dir(&src);
            let loaded = load(&path).0;
            assert!(loaded.body_tracking.show_skeleton, "{src}");
            assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
            assert_eq!(files(&dir.0), ["config.toml"]);
        }
        // Off: loaded and left as written; a change is read live.
        let src = format!(
            "{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmode = \"live\"\nshow_skeleton = false\n"
        );
        let (dir, path) = config_dir(&src);
        let cfg = SystemConfig::new(path.clone());
        assert!(!cfg.body_tracking().show_skeleton);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
        assert_eq!(files(&dir.0), ["config.toml"]);
        cfg.update(|c| c.body_tracking.show_skeleton = true);
        assert!(cfg.body_tracking().show_skeleton);
    }

    #[test]
    fn shot_cards_read_on_without_the_key_and_off_as_written() {
        // A config without the key: the camera cards show and the file is
        // left byte-identical, with no backup.
        let replay = "[replay]\nkeep_shots = 5\npre_roll_s = 4.0\npost_roll_s = 1.5\nstart_before_impact_s = 2.0\n";
        for src in [
            GOLDEN_DEFAULT_TOML.to_owned(),
            format!("{GOLDEN_DEFAULT_TOML}\n{replay}"),
        ] {
            let (dir, path) = config_dir(&src);
            let loaded = load(&path).0;
            assert!(loaded.replay.shot_cards, "{src}");
            assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
            assert_eq!(files(&dir.0), ["config.toml"]);
        }
        // Off: loaded and left as written; a change is read live.
        let src = format!("{GOLDEN_DEFAULT_TOML}\n{replay}shot_cards = false\n");
        let (dir, path) = config_dir(&src);
        let cfg = SystemConfig::new(path.clone());
        assert!(!cfg.replay().shot_cards);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
        assert_eq!(files(&dir.0), ["config.toml"]);
        cfg.update(|c| c.replay.shot_cards = true);
        assert!(cfg.replay().shot_cards);
    }

    #[test]
    fn a_body_tracking_mode_loads_byte_identical_and_the_older_switch_becomes_one() {
        use flighthook::BodyTrackingMode;
        // Without the section (Off, the default), and with a mode other
        // than the default written: left as it is.
        for (src, mode) in [
            (GOLDEN_DEFAULT_TOML.to_owned(), BodyTrackingMode::Off),
            (
                format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmode = \"live\"\n"),
                BodyTrackingMode::Live,
            ),
            (
                format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmode = \"historical\"\n"),
                BodyTrackingMode::Historical,
            ),
        ] {
            let (dir, path) = config_dir(&src);
            let loaded = load(&path).0;
            assert_eq!(loaded.body_tracking.mode, mode);
            assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
            assert_eq!(files(&dir.0), ["config.toml"]);
        }
        // `enabled = true` is Historical, written as the mode, the original
        // backed up.
        let src = format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nenabled = true\n");
        let (dir, path) = config_dir(&src);
        assert_eq!(
            load(&path).0.body_tracking.mode,
            BodyTrackingMode::Historical
        );
        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(saved.contains("mode = \"historical\""), "{saved}");
        assert!(!saved.contains("enabled"), "{saved}");
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        // `enabled = false` is the default, Off: the section goes.
        let src = format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nenabled = false\n");
        let (_dir, path) = config_dir(&src);
        assert_eq!(load(&path).0.body_tracking.mode, BodyTrackingMode::Off);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            GOLDEN_DEFAULT_TOML
        );
    }

    /// A card that is on writes no `enabled`; one that is off writes
    /// `enabled = false` in its own table, and a file holding it loads
    /// untouched.
    #[test]
    fn enabled_is_written_only_when_off_and_such_a_file_is_left_alone() {
        let c: FlighthookConfig = toml::from_str(EVERY_SECTION_TOML).expect("parses");
        assert!(c.disabled_actors().is_empty(), "absent is on");
        assert!(!toml::to_string_pretty(&c).expect("ser").contains("enabled"));

        let mut off = c.clone();
        let (index, mock) = off.mock_monitor.iter_mut().next().expect("a mock");
        mock.enabled = false;
        let table = format!("[mock_monitor.{index}]");
        let out = toml::to_string_pretty(&off).expect("ser");
        assert_eq!(out.matches("enabled").count(), 1, "{out}");
        let at = out.find(&table).expect("mock table");
        let rest = &out[at..];
        let end = rest[table.len()..]
            .find("\n[")
            .map_or(rest.len(), |e| e + table.len());
        assert!(rest[..end].contains("enabled = false\n"), "{out}");

        let (dir, path) = config_dir(&out);
        let loaded = load(&path).0;
        assert_eq!(std::fs::read_to_string(&path).expect("read"), out);
        assert_eq!(files(&dir.0), ["config.toml"]);
        assert_eq!(loaded, off);
    }

    /// A file with two enabled cards on one address: the first in index
    /// order keeps running, the later ones load off. The file itself is left
    /// as it is; the next save writes `enabled = false` for them.
    #[test]
    fn a_file_with_two_enabled_cards_on_one_address_loads_the_later_one_off() {
        let contents = format!(
            r#"{GOLDEN_DEFAULT_TOML}
[openconnect_client.2]
name = "Open Connect"
address = "localhost:921"

[openconnect_client.1]
name = "GSPro"

[openconnect_client.3]
name = "Muni Golf"
address = "127.0.0.1:9210"
"#
        );
        let (dir, path) = config_dir(&contents);
        let system = SystemConfig::new(path.clone());
        let config = system.snapshot();
        assert!(config.openconnect_client["1"].enabled, "first in order");
        assert!(!config.openconnect_client["2"].enabled);
        assert!(config.openconnect_client["3"].enabled, "another port");
        assert_eq!(config.disabled_actors(), ["openconnect_client.2"]);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), contents);
        assert_eq!(files(&dir.0), ["config.toml"], "no rewrite, no backup");

        // Any save writes the card off with everything else.
        system.update(|_| {});
        let written = std::fs::read_to_string(&path).expect("read");
        assert_eq!(written.matches("enabled = false").count(), 1, "{written}");
        let again = SystemConfig::new(path).snapshot();
        assert_eq!(again, config);
    }

    #[test]
    fn a_current_file_is_left_byte_identical_with_no_backup() {
        let c: FlighthookConfig = toml::from_str(EVERY_SECTION_TOML).expect("parses");
        let current = toml::to_string_pretty(&c).expect("ser");
        for contents in [GOLDEN_DEFAULT_TOML, current.as_str()] {
            let (dir, path) = config_dir(contents);
            let loaded = load(&path).0;
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
        let loaded = load(&path).0;
        assert!((loaded.simulation.temperature_c - 12.0).abs() < f64::EPSILON);
        assert_eq!(loaded.openconnect_client["0"].name, "Bay GSPro");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
        assert_eq!(files(&dir.0), ["config.toml"]);
    }

    /// A value written out at its default, where the app leaves it out, is
    /// normalized away once; the file is stable from then on.
    #[test]
    fn explicit_defaults_are_normalized_once() {
        let src = format!("{GOLDEN_DEFAULT_TOML}\n[mevo.0]\nname = \"Xi\"\nspin_source = \"d4\"\n");
        let (dir, path) = config_dir(&src);
        let loaded = load(&path).0;
        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(!saved.contains("spin_source"), "{saved}");
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        assert_eq!(load(&path).0, loaded);
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
            let first = load(&path).0;
            let migrated = std::fs::read(&path).expect("read");
            assert_ne!(migrated, contents.as_bytes(), "{contents}");
            let after_first = files(&dir.0);
            assert_eq!(after_first, [backup_name(), "config.toml".to_string()]);

            let second = load(&path).0;
            assert_eq!(second, first);
            assert_eq!(std::fs::read(&path).expect("read"), migrated);
            assert_eq!(files(&dir.0), after_first);
        }
    }

    #[test]
    fn a_golfer_height_moves_to_the_player_once_with_a_backup() {
        let src = "[body_tracking]\nmode = \"live\"\ngolfer_height = 182.5\n";
        let (dir, path) = config_dir(src);
        let first = load(&path).0;
        assert_eq!(first.player.height, Some(182.5));
        assert_eq!(first.body_tracking.mode, flighthook::BodyTrackingMode::Live);
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        let backup = std::fs::read(dir.0.join(backup_name())).expect("backup");
        assert_eq!(backup, src.as_bytes());
        let migrated = std::fs::read_to_string(&path).expect("read");
        assert!(!migrated.contains("golfer_height"), "{migrated}");
        assert!(migrated.contains("[player]\nheight = 182.5"), "{migrated}");
        assert!(
            migrated.contains("[body_tracking]\nmode = \"live\""),
            "{migrated}"
        );

        // The second load reads the same and leaves the file alone.
        let second = load(&path).0;
        assert_eq!(second, first);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), migrated);
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        let cfg = SystemConfig::new(path);
        assert_eq!(cfg.player().height, Some(182.5));
    }

    #[test]
    fn camera_rotation_and_player_are_read_in_effect() {
        let (_dir, path) = config_dir(
            "[camera.0]\nname = \"Face\"\ncamera = \"test-pattern\"\nrotation_deg = 90\n\n\
             [camera.1]\nname = \"Side\"\ncamera = \"test-pattern\"\nrotation_deg = 45\n\n\
             [player]\nsex = \"female\"\nheight = 99\n",
        );
        let cfg = SystemConfig::new(path);
        assert!(cfg.snapshot().camera_error().is_some());
        assert_eq!(cfg.camera_rotation("camera.0"), 90);
        assert_eq!(cfg.camera_rotation("camera.1"), 0, "not one of the four");
        assert_eq!(cfg.camera_rotation("camera.7"), 0, "no such camera");
        assert_eq!(cfg.camera_rotation("mevo.0"), 0);
        assert_eq!(
            cfg.player(),
            flighthook::PlayerSection {
                sex: flighthook::Sex::Female,
                height: None,
            }
        );
        // The out-of-range values stay in the file for the user to fix.
        assert_eq!(cfg.snapshot().camera["1"].rotation_deg, 45);
        assert_eq!(cfg.snapshot().player.height, Some(99.0));
    }

    #[test]
    fn an_existing_backup_is_never_overwritten() {
        let (dir, path) = config_dir(GSPRO_TOML);
        let older = dir.0.join(backup_name());
        std::fs::write(&older, "older backup").expect("write");
        let _ = load(&path);
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

    /// The error `load` keeps for config text `contents`.
    fn kept_error(contents: &str) -> flighthook::ConfigLoadError {
        let (dir, path) = config_dir(contents);
        let (config, error) = load(&path);
        assert_eq!(config, FlighthookConfig::default());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), contents);
        drop(dir);
        error.expect("an error")
    }

    #[test]
    fn a_file_that_fails_to_parse_keeps_its_located_error() {
        let bad_mode = "default_units = \"metric\"\n\n[body_tracking]\nmode = \"liv\"\n";
        let error = kept_error(bad_mode);
        assert!(error.path.ends_with("config.toml"), "{}", error.path);
        assert!(error.error.starts_with("line 4, column 8: "), "{error:?}");
        assert!(error.error.contains("liv"), "{error:?}");

        let syntax = kept_error("default_units = \"metric\"\n[simulation\n");
        assert!(syntax.error.starts_with("line 2, "), "{syntax:?}");

        let gspro = kept_error("[mevo.0]\nname = \"Mevo+\"\n\n[gspro.0]\nname = 7\n");
        assert!(gspro.error.starts_with("line 5, "), "{gspro:?}");
    }

    #[test]
    fn a_clean_file_and_a_missing_file_keep_no_error() {
        let (_dir, path) = config_dir(GSPRO_TOML);
        assert_eq!(SystemConfig::new(path).load_error(), None);
        let (dir, _) = config_dir("");
        let missing = dir.0.join("elsewhere.toml");
        assert_eq!(SystemConfig::new(missing).load_error(), None);
    }

    #[test]
    fn a_file_that_cannot_be_read_keeps_the_reason() {
        // A directory where the file should be: not NotFound.
        let (dir, _) = config_dir("");
        let path = dir.0.join("folder.toml");
        std::fs::create_dir(&path).expect("mkdir");
        let cfg = SystemConfig::new(path.clone());
        let error = cfg.load_error().expect("an error");
        assert!(error.path.ends_with("folder.toml"), "{}", error.path);
        assert!(!error.error.is_empty());

        // It cannot be backed up either: the save is refused and the file
        // stays.
        let refused = cfg.back_up_unusable_file().expect_err("refused");
        assert!(refused.contains("cannot be backed up"), "{refused}");
        cfg.update(|c| c.simulation.mode = SimMode::Off);
        assert!(path.is_dir());
        assert!(cfg.load_error().is_some(), "still reported");
        assert_eq!(files(&dir.0), ["config.toml", "folder.toml"]);
    }

    #[test]
    fn the_first_save_backs_up_a_file_that_failed_to_parse_and_clears_the_error() {
        let broken = "default_units = \"metric\"\n[body_tracking]\nmode = \"liv\"\n";
        let (dir, path) = config_dir(broken);
        let cfg = SystemConfig::new(path.clone());
        assert!(cfg.load_error().is_some());
        assert_eq!(files(&dir.0), ["config.toml"], "start leaves it alone");

        cfg.update(|c| c.simulation.mode = SimMode::Off);
        assert_eq!(cfg.load_error(), None);
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dir.0.join(backup_name())).expect("read"),
            broken
        );
        let saved = std::fs::read_to_string(&path).expect("read");
        let back: FlighthookConfig = toml::from_str(&saved).expect("parses");
        assert_eq!(back.simulation.mode, SimMode::Off);

        // Later saves back up nothing more.
        cfg.update(|c| c.simulation.mode = SimMode::On);
        assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    }

    #[test]
    fn backing_up_ahead_of_a_save_clears_the_error_once() {
        let broken = "[body_tracking]\nmode = \"liv\"\n";
        let (dir, path) = config_dir(broken);
        let cfg = SystemConfig::new(path.clone());
        let backup = cfg.back_up_unusable_file().expect("backed up");
        assert_eq!(backup, Some(dir.0.join(backup_name())));
        assert_eq!(cfg.load_error(), None);
        assert_eq!(cfg.back_up_unusable_file(), Ok(None));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            broken,
            "the file itself is the save's to replace"
        );
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

[player]
sex = "female"
height = 170

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

[square.0.dot_bottom_to_face_center_mm]
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

[pureplay.0]
name = "Pureplay"
address = "127.0.0.1:9760"
full_monitor = "mevo.0"

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
        assert_eq!(c.square["0"].driver.dot_bottom_to_face_center_mm.len(), 2);
        assert_eq!(c.mock_monitor["0"].name, "Mock");
        assert_eq!(
            c.openconnect_server["0"].bind.as_deref(),
            Some("0.0.0.0:921")
        );
        assert_eq!(c.openconnect_client["0"].name, "GSPro");
        assert_eq!(c.openconnect_client["1"].name, "Muni Golf");
        assert_eq!(c.random_club["0"].name, "Clubs");
        assert_eq!(c.pureplay["0"].address.as_deref(), Some("127.0.0.1:9760"));
        assert_eq!(c.pureplay["0"].full_monitor.as_deref(), Some("mevo.0"));
        assert_eq!(c.player.sex, flighthook::Sex::Female);
        assert_eq!(c.player.height, Some(170.0));

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
