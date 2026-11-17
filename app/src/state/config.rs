//! Configuration loading, resolution, and persistence.
//!
//! Handles the TOML config file (~/.config/flighthook/config.toml) with
//! type-prefixed sections: `[mevo.<id>]`, `[openconnect_client.<id>]`, etc.
//! `[gspro.<id>]` sections are read as `[openconnect_client.<id>]`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::Deserialize;

// Re-export config types from schemas for crate-internal use
pub use flighthook::FlighthookConfig;

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
        let mut openconnect_client = self.gspro;
        for section in openconnect_client.values_mut() {
            section.chipping_clubs.clone_from(&self.chipping_clubs);
            section.putting_clubs.clone_from(&self.putting_clubs);
        }
        FlighthookConfig {
            default_units: self.default_units,
            frp: flighthook::FrpSection::default(),
            selected_golfer: None,
            shot_grouping: flighthook::ShotGrouping::default(),
            simulation: self.simulation,
            webserver,
            mevo: self.mevo,
            r10: HashMap::new(),
            square: HashMap::new(),
            exputt: HashMap::new(),
            trackball: HashMap::new(),
            mock_monitor: self.mock_monitor,
            openconnect_server: HashMap::new(),
            openconnect_client,
            pureplay: HashMap::new(),
            pureplay_accessory: HashMap::new(),
            random_club: self.random_club,
            replay: flighthook::ReplaySection::default(),
            body_tracking: flighthook::BodyTrackingSection::default(),
            golfers: Vec::new(),
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
                give_actor_ids(&mut config);
                // Output routing that names a card by its key (`mevo.0`)
                // names it by its actor id from here on. A key no card
                // holds stays as it is and routes nothing.
                config.route_by_actor_id();
                bring_up_to_date(path, &contents, &config);
                turn_off_clashing(path, &mut config);
                (config, None)
            }
            Err(error) => (
                with_actor_ids(FlighthookConfig::default()),
                Some(failed(error)),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Written at once, so flighthook's actors keep their ids across
            // restarts from the first start on.
            let defaults = with_actor_ids(FlighthookConfig::default());
            tracing::info!("no config file found, creating {}", path.display());
            save_to(path, &defaults);
            (defaults, None)
        }
        Err(e) => {
            tracing::warn!("failed to read {}: {e}", path.display());
            (
                with_actor_ids(FlighthookConfig::default()),
                Some(failed(e.to_string())),
            )
        }
    }
}

/// Give every actor in `config` that has none, or whose id an earlier actor
/// holds, its FRP actor id, generated once ([`flightrelay::new_actor_id`]).
/// Run at load (the file is rewritten once, with a backup, by
/// [`bring_up_to_date`]) and on every change (a new card gets its id when
/// it is first saved).
pub fn give_actor_ids(config: &mut FlighthookConfig) {
    for key in config.renew_duplicate_actor_ids(flightrelay::new_actor_id) {
        tracing::warn!("'{key}' held an actor id an earlier actor holds; it has a new id");
    }
    config.fill_actor_ids(flightrelay::new_actor_id);
    give_golfer_ids(config);
}

/// Keep each golfer id in the first golfer that lists it (a warning names
/// the golfer that loses it), and give every golfer without a
/// `flighthook.*` identifier one, generated once ([`new_golfer_id`]).
pub fn give_golfer_ids(config: &mut FlighthookConfig) {
    for dup in config.dedupe_golfer_ids() {
        tracing::warn!("{dup}; {} keeps it", dup.first);
    }
    config.fill_golfer_ids(new_golfer_id);
}

/// A new golfer's own id, without its namespace: a random UUID.
pub fn new_golfer_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn with_actor_ids(mut config: FlighthookConfig) -> FlighthookConfig {
    give_actor_ids(&mut config);
    config
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
            for output in config.simulator_routes() {
                tracing::warn!(
                    "{}: {output} is routed to a Pureplay Accessory card, whose shots no output \
                     takes; that route passes nothing",
                    path.display()
                );
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
            if let Some(error) = config.golfer_error() {
                tracing::warn!("{}: {error}; no height is used", path.display());
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
                let fault = driver_type_error::<flighthook::MevoSection>(contents)
                    .or_else(|| driver_type_error::<flighthook::MockMonitorSection>(contents))
                    .or_else(|| driver_type_error::<flighthook::R10Section>(contents))
                    .or_else(|| driver_type_error::<flighthook::SquareSection>(contents))
                    .or_else(|| driver_type_error::<flighthook::ExPuttSection>(contents))
                    .or_else(|| driver_type_error::<flighthook::TrackballSection>(contents))
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
/// rewritten file is not rewritten again. A Trackball section's `grab` key
/// does not count ([`IGNORED_KEYS`]): it is read and ignored, and the next
/// save leaves it out.
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
    let (Ok(mut file), Ok(current)) = (
        contents.parse::<toml::Value>(),
        saved.parse::<toml::Value>(),
    ) else {
        return;
    };
    drop_ignored_keys(&mut file);
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

/// Keys a section type reads and ignores, by section type: the Trackball
/// never grabs the pointer, so its `grab` is no setting.
const IGNORED_KEYS: &[(&str, &str)] = &[("trackball", "grab")];

/// Remove the [`IGNORED_KEYS`] from every section of their type in `file`,
/// a config file's TOML value.
fn drop_ignored_keys(file: &mut toml::Value) {
    for (kind, key) in IGNORED_KEYS {
        if let Some(sections) = file.get_mut(*kind).and_then(toml::Value::as_table_mut) {
            for (_, section) in sections.iter_mut() {
                if let Some(section) = section.as_table_mut() {
                    section.remove(*key);
                }
            }
        }
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
    inner: Arc<RwLock<FlighthookConfig>>,
    /// Why the file was not used at startup, until a save has backed it up
    /// and replaced it.
    load_error: Mutex<Option<flighthook::ConfigLoadError>>,
    /// Every card's key (`mevo.0`) and actor id, rebuilt on each change.
    cards: RwLock<CardActors>,
}

/// The cards of a config by key and by actor id, and flighthook's own
/// actor ids.
#[derive(Default)]
struct CardActors {
    by_key: std::collections::HashMap<String, String>,
    by_id: std::collections::HashMap<String, String>,
    own: OwnActorIds,
}

/// The actor ids of flighthook's own actors, `None` while one has none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnActorIds {
    /// `[frp] actor_id`.
    pub flighthook: Option<String>,
    /// `[frp] auto_chip_actor_id`.
    pub auto_chip: Option<String>,
    /// `[simulation] actor_id`.
    pub flight_model: Option<String>,
}

impl CardActors {
    fn of(config: &FlighthookConfig) -> Self {
        let mut out = Self {
            own: OwnActorIds {
                flighthook: config.frp.actor_id.clone(),
                auto_chip: config.frp.auto_chip_actor_id.clone(),
                flight_model: config.simulation.actor_id.clone(),
            },
            ..Self::default()
        };
        for (key, id) in config.card_actors() {
            if let Some(id) = id {
                out.by_key.insert(key.clone(), id.to_owned());
                out.by_id.entry(id.to_owned()).or_insert(key);
            }
        }
        out
    }
}

/// A read-only handle on the live config, for code that reads it where it
/// is used (the session's golfer rules).
#[derive(Clone)]
pub struct ConfigView(Arc<RwLock<FlighthookConfig>>);

impl std::fmt::Debug for ConfigView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConfigView")
    }
}

impl ConfigView {
    /// A view of `config` alone, not tied to any [`SystemConfig`].
    #[cfg(test)]
    pub fn detached(config: FlighthookConfig) -> Self {
        Self(Arc::new(RwLock::new(config)))
    }

    /// The config, to change it in a test.
    #[cfg(test)]
    pub fn write(&self) -> std::sync::RwLockWriteGuard<'_, FlighthookConfig> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }

    /// The config as it is now.
    pub fn read(&self) -> std::sync::RwLockReadGuard<'_, FlighthookConfig> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The live config itself, which the cameras read.
    pub fn shared(&self) -> Arc<RwLock<FlighthookConfig>> {
        Arc::clone(&self.0)
    }
}

impl SystemConfig {
    /// A read-only handle that follows every change.
    pub fn view(&self) -> ConfigView {
        ConfigView(Arc::clone(&self.inner))
    }

    /// Load config from disk (or create defaults) and cache it.
    pub fn new(path: PathBuf) -> Self {
        let (config, load_error) = load(&path);
        Self {
            path,
            cards: RwLock::new(CardActors::of(&config)),
            inner: Arc::new(RwLock::new(config)),
            load_error: Mutex::new(load_error),
        }
    }

    /// The actor id of the card `key` (`mevo.0`), if it has one.
    pub fn actor_of_card(&self, key: &str) -> Option<String> {
        self.cards
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_key
            .get(key)
            .cloned()
    }

    /// The actor ids of flighthook's own actors.
    pub fn own_actor_ids(&self) -> OwnActorIds {
        self.cards
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .own
            .clone()
    }

    /// The key of the card whose actor id is `actor_id`, if one is.
    pub fn card_of_actor(&self, actor_id: &str) -> Option<String> {
        self.cards
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_id
            .get(actor_id)
            .cloned()
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

    /// Rebuild the card map from `config` and write `config` to the config
    /// file, after backing up a file that failed to load. A file that cannot
    /// be backed up is left alone.
    fn persist(&self, config: &FlighthookConfig) {
        *self.cards.write().unwrap_or_else(|e| e.into_inner()) = CardActors::of(config);
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
        let before = guard.clone();
        f(&mut guard);
        guard.keep_actor_ids_from(&before);
        give_actor_ids(&mut guard);
        self.persist(&guard);
    }

    /// Replace the entire cached config and persist to disk. The running
    /// process keeps its own actor ids (flighthook, Auto-chip, the flight
    /// model) whatever `new` holds; a card keeps its id when `new` gives it
    /// none.
    pub fn replace(&self, new: FlighthookConfig) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let before = std::mem::replace(&mut *guard, new);
        let own = |slot: &mut Option<String>, old: &Option<String>| {
            if old.is_some() {
                slot.clone_from(old);
            }
        };
        own(&mut guard.frp.actor_id, &before.frp.actor_id);
        own(
            &mut guard.frp.auto_chip_actor_id,
            &before.frp.auto_chip_actor_id,
        );
        own(&mut guard.simulation.actor_id, &before.simulation.actor_id);
        guard.keep_actor_ids_from(&before);
        give_actor_ids(&mut guard);
        self.persist(&guard);
    }
}

#[cfg(test)]
mod tests;
