//! The body tracking models: found in the models folder, loaded once and
//! shared, and downloaded into it only when the user asks.
//!
//! Each pose model is loaded on a thread of its own the first time it is
//! wanted, together with the person detector, once both files are in the
//! folder and match their SHA-256. A model whose files are not there is
//! [`ModelState::Missing`]: nothing is ever downloaded on its own. One load
//! runs at a time. The loaded weights are shared: each live view and the
//! clip pass make their own instances from them (`PoseModels::fork`), never
//! reading the files again. A model no longer wanted (the setting moved to
//! the other one) is let go once nothing uses it.
//!
//! A download runs only through [`Models::download`] (`POST
//! /api/body_tracking/models/download`, sent by the UI's Download button):
//! one at a time, the files the model lacks from their official sources
//! (stance's `fetch_model_with`, HTTPS, hash-verified), reporting its
//! progress in [`Models::files`]. [`Models::cancel_download`] stops it; the
//! partial file is deleted. Once it is done, the models wanted while their
//! files were missing load on the next request. A download left unfinished
//! by a flighthook that stopped mid-download is deleted before the next
//! one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use flighthook::{
    BodyTrackingModels, ModelDownload, ModelFile, ModelFileProgress, ModelFilesState, PoseModel,
};
use stance::pose::{
    FetchProgress, ModelSpec, PoseConfig, PoseModels, RTMPOSE_M_HALPE26, RTMPOSE_S_HALPE26,
};
use tracing::{debug, info, warn};

/// How long a partial download goes unwritten before it counts as left
/// over (another flighthook may be writing one).
const STALE_PART: Duration = Duration::from_secs(10 * 60);

/// The models folder: `flighthook/models` in the user's local data folder,
/// beside the default clip folder (`%LOCALAPPDATA%\flighthook\models`,
/// `~/.local/share/flighthook/models`).
pub fn default_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flighthook")
        .join("models")
}

/// The stance model behind `model`.
pub fn spec(model: PoseModel) -> ModelSpec {
    match model {
        PoseModel::RtmposeM => RTMPOSE_M_HALPE26,
        PoseModel::RtmposeS => RTMPOSE_S_HALPE26,
    }
}

/// stance's settings for `model`, looking for the golfer every
/// `detect_every` frames; everything else at stance's defaults (person
/// detector YOLOX-Tiny, no smoothing: the live view smooths on its own).
pub fn pose_config(model: PoseModel, detect_every: u32) -> PoseConfig {
    PoseConfig {
        pose_model: spec(model),
        detect_every: detect_every.max(1),
        ..PoseConfig::default()
    }
}

/// The files `model` needs: its pose model, then the person detector.
pub fn specs(model: PoseModel) -> [ModelSpec; 2] {
    let config = pose_config(model, 1);
    [config.pose_model, config.detector]
}

/// What a model file is, for the UI.
fn label(spec: &ModelSpec) -> String {
    match spec.name {
        "rtmpose-m-halpe26" => "RTMPose-m pose model".to_owned(),
        "rtmpose-s-halpe26" => "RTMPose-s pose model".to_owned(),
        "yolox-tiny" => "YOLOX-Tiny person detector".to_owned(),
        "yolox-nano" => "YOLOX-Nano person detector".to_owned(),
        other => other.to_owned(),
    }
}

/// The host a model file is downloaded from.
fn host(spec: &ModelSpec) -> String {
    let rest = spec
        .url
        .split_once("://")
        .map_or(spec.url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_owned()
}

/// Where the model files come from and how they are checked: the official
/// sources ([`Official`]), or a stand-in in tests.
pub trait Source: Send + Sync {
    /// Whether `spec`'s file is in `dir` and matches its SHA-256.
    fn verified(&self, spec: &ModelSpec, dir: &Path) -> bool;

    /// Download `spec` into `dir`, reporting progress and stopping once
    /// `cancel` is set (`stance::Error::Canceled`), its partial file
    /// deleted.
    ///
    /// # Errors
    ///
    /// Why the download failed.
    fn fetch(
        &self,
        spec: &ModelSpec,
        dir: &Path,
        progress: &mut dyn FnMut(FetchProgress),
        cancel: &AtomicBool,
    ) -> stance::Result<()>;
}

/// A file's size and modification time: a file whose stamp is unchanged
/// is not hashed again.
type Stamp = (u64, Option<SystemTime>);

/// The models' official sources (stance's `ModelSpec` URLs), and their
/// SHA-256 check, remembered per file until it changes.
#[derive(Debug, Default)]
pub struct Official {
    checked: Mutex<HashMap<PathBuf, (Stamp, bool)>>,
}

impl Source for Official {
    fn verified(&self, spec: &ModelSpec, dir: &Path) -> bool {
        let path = spec.path_in(dir);
        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        let stamp = (meta.len(), meta.modified().ok());
        let mut checked = self.checked.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((seen, ok)) = checked.get(&path)
            && *seen == stamp
        {
            return *ok;
        }
        let ok = spec.read_verified(dir).is_ok();
        checked.insert(path, (stamp, ok));
        ok
    }

    fn fetch(
        &self,
        spec: &ModelSpec,
        dir: &Path,
        progress: &mut dyn FnMut(FetchProgress),
        cancel: &AtomicBool,
    ) -> stance::Result<()> {
        stance::pose::fetch_model_with(spec, dir, Some(progress), Some(cancel)).map(|_| ())
    }
}

/// Loaded models, shared between threads. stance's models hold their run
/// state beside the weights and are not `Sync`, so each user takes its own
/// instances ([`Shared::fork`]), which share the weights.
#[derive(Debug)]
pub struct Shared(Mutex<PoseModels>);

impl Shared {
    /// New instances on these weights, for one thread.
    ///
    /// # Errors
    ///
    /// As stance's `PoseModels::fork`.
    pub fn fork(&self) -> stance::Result<PoseModels> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).fork()
    }
}

/// Where a pose model is.
#[derive(Debug, Clone)]
pub enum ModelState {
    /// Being read and verified.
    Loading,
    /// A file it needs is not in the models folder (or does not match):
    /// it loads once the user has downloaded it.
    Missing,
    /// Loaded.
    Ready(Arc<Shared>),
    /// It could not be loaded: why.
    Failed(String),
}

impl ModelState {
    /// The loaded models, when ready.
    pub fn ready(&self) -> Option<Arc<Shared>> {
        match self {
            Self::Ready(models) => Some(Arc::clone(models)),
            _ => None,
        }
    }

    /// The state as the live route's `X-Pose-State` names it.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Missing => "missing",
            Self::Ready(_) => "ready",
            Self::Failed(_) => "failed",
        }
    }
}

/// How [`Models::download`] answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStart {
    /// A download started.
    Started,
    /// One was already running; nothing new started.
    Running,
    /// Every file is there already; nothing to download.
    Ready,
}

/// The download running, and the last one's failure.
#[derive(Default)]
struct DownloadSlot {
    running: Option<Running>,
    /// The model whose download failed last, and why. Cleared by the next
    /// download and by a cancel.
    failed: Option<(PoseModel, String)>,
}

/// A running download.
struct Running {
    model: PoseModel,
    /// Set to stop it; also tells its thread apart from a later one's.
    cancel: Arc<AtomicBool>,
    files: Vec<FileProgress>,
    current: usize,
}

struct FileProgress {
    spec: ModelSpec,
    done: u64,
    total: u64,
}

/// The pose models: where each is, loaded once and shared, and their
/// downloads.
pub struct Models {
    dir: PathBuf,
    source: Arc<dyn Source>,
    entries: Arc<Mutex<HashMap<PoseModel, ModelState>>>,
    /// Held by the thread loading, so one runs at a time (two models share
    /// the person detector's file).
    loading: Arc<Mutex<()>>,
    /// Counted up by [`Models::unload`]: a load started before it keeps
    /// nothing.
    generation: Arc<AtomicU64>,
    download: Arc<Mutex<DownloadSlot>>,
}

impl std::fmt::Debug for Models {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Models").field("dir", &self.dir).finish()
    }
}

impl Models {
    /// Models in folder `dir`, checked and downloaded through `source`.
    /// Nothing is read until a model is wanted, and nothing is downloaded
    /// until [`Models::download`].
    pub fn new(dir: PathBuf, source: Arc<dyn Source>) -> Self {
        Self {
            dir,
            source,
            entries: Arc::default(),
            loading: Arc::default(),
            generation: Arc::default(),
            download: Arc::default(),
        }
    }

    /// Let every model go (body tracking turned off): the next
    /// [`Models::want`] loads afresh. Instances already made keep their
    /// weights until they are dropped; a load running now finishes and
    /// keeps nothing.
    pub fn unload(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::SeqCst);
        if entries.values().any(|s| !matches!(s, ModelState::Missing)) {
            info!("body tracking: models let go");
        }
        entries.clear();
    }

    /// The models folder.
    pub fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    /// Where `model` is, without asking for it.
    pub fn state(&self, model: PoseModel) -> Option<ModelState> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&model)
            .cloned()
    }

    /// Whether every file `model` needs is in the folder and verified.
    fn present(&self, model: PoseModel) -> bool {
        specs(model)
            .iter()
            .all(|spec| self.source.verified(spec, &self.dir))
    }

    /// Where `model` is, starting its load on a thread of its own the first
    /// time. A model whose files were missing loads once they are there.
    /// The other model is let go when loaded and no longer in use. Never
    /// downloads.
    pub fn want(&self, model: PoseModel) -> ModelState {
        let missing = matches!(self.state(model), Some(ModelState::Missing));
        if missing && !self.present(model) {
            return ModelState::Missing;
        }
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // The map's own `Arc` is the last one: no live view or clip is
        // making instances from it (made instances keep their weights).
        entries.retain(|m, state| {
            *m == model || !matches!(state, ModelState::Ready(s) if Arc::strong_count(s) == 1)
        });
        if let Some(state) = entries.get(&model)
            && !(missing && matches!(state, ModelState::Missing))
        {
            return state.clone();
        }
        entries.insert(model, ModelState::Loading);
        let generation = self.generation.load(Ordering::SeqCst);
        drop(entries);
        let job = Load {
            model,
            dir: self.dir(),
            source: Arc::clone(&self.source),
            entries: Arc::clone(&self.entries),
            loading: Arc::clone(&self.loading),
            generation,
            current: Arc::clone(&self.generation),
        };
        let spawned = std::thread::Builder::new()
            .name(format!("pose-models:{}", model.key()))
            .spawn(move || job.run());
        if let Err(e) = spawned {
            let error = format!("no thread to load the models: {e}");
            set(&self.entries, model, ModelState::Failed(error), None);
        }
        self.state(model).unwrap_or(ModelState::Loading)
    }

    /// Whether a download is running.
    pub fn downloading(&self) -> bool {
        self.download
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running
            .is_some()
    }

    /// The state of `model`'s files: what `GET /api/body_tracking/models`
    /// answers. While a download runs it is `downloading` with that
    /// download's progress, whichever model it fetches.
    pub fn files(&self, model: PoseModel) -> BodyTrackingModels {
        let files: Vec<ModelFile> = specs(model)
            .iter()
            .map(|spec| ModelFile {
                name: spec.file_name(),
                label: label(spec),
                host: host(spec),
                size: spec.size,
                present: self.source.verified(spec, &self.dir),
            })
            .collect();
        let slot = self.download.lock().unwrap_or_else(|e| e.into_inner());
        let download = slot.running.as_ref().map(|r| ModelDownload {
            files: r
                .files
                .iter()
                .map(|f| ModelFileProgress {
                    name: f.spec.file_name(),
                    label: label(&f.spec),
                    host: host(&f.spec),
                    done: f.done,
                    total: f.total,
                })
                .collect(),
            current: r.current,
            done: r.files.iter().map(|f| f.done).sum(),
            total: r.files.iter().map(|f| f.total).sum(),
        });
        let error = match &slot.failed {
            Some((m, e)) if *m == model => Some(e.clone()),
            _ => None,
        };
        drop(slot);
        let state = if download.is_some() {
            ModelFilesState::Downloading
        } else if files.iter().all(|f| f.present) {
            ModelFilesState::Ready
        } else if error.is_some() {
            ModelFilesState::Failed
        } else {
            ModelFilesState::Missing
        };
        BodyTrackingModels {
            state,
            model,
            dir: self.dir.display().to_string(),
            files,
            download,
            error: error.filter(|_| state == ModelFilesState::Failed),
        }
    }

    /// Download the files `model` lacks, on a thread of its own. Does
    /// nothing while a download runs, or when every file is there.
    pub fn download(&self, model: PoseModel) -> DownloadStart {
        let needed: Vec<ModelSpec> = specs(model)
            .into_iter()
            .filter(|spec| !self.source.verified(spec, &self.dir))
            .collect();
        let mut slot = self.download.lock().unwrap_or_else(|e| e.into_inner());
        if slot.running.is_some() {
            return DownloadStart::Running;
        }
        slot.failed = None;
        if needed.is_empty() {
            return DownloadStart::Ready;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        slot.running = Some(Running {
            model,
            cancel: Arc::clone(&cancel),
            files: needed
                .iter()
                .map(|spec| FileProgress {
                    spec: *spec,
                    done: 0,
                    total: spec.size,
                })
                .collect(),
            current: 0,
        });
        drop(slot);
        let job = Download {
            model,
            specs: needed,
            dir: self.dir(),
            source: Arc::clone(&self.source),
            slot: Arc::clone(&self.download),
            entries: Arc::clone(&self.entries),
            cancel,
        };
        let spawned = std::thread::Builder::new()
            .name("pose-download".to_owned())
            .spawn(move || job.run());
        if let Err(e) = spawned {
            let mut slot = self.download.lock().unwrap_or_else(|e| e.into_inner());
            slot.running = None;
            slot.failed = Some((model, format!("no thread for the download: {e}")));
        }
        DownloadStart::Started
    }

    /// Stop the download running, if any: its partial file is deleted as
    /// soon as its thread notices (between two reads). Returns whether one
    /// was running. A canceled download leaves no failure behind.
    pub fn cancel_download(&self) -> bool {
        let mut slot = self.download.lock().unwrap_or_else(|e| e.into_inner());
        slot.failed = None;
        let Some(running) = slot.running.take() else {
            return false;
        };
        running.cancel.store(true, Ordering::SeqCst);
        info!(
            "body tracking: download of {} canceled",
            running.model.label()
        );
        true
    }

    /// Put `model` in `state` (tests).
    #[cfg(test)]
    pub fn set_state(&self, model: PoseModel, state: ModelState) {
        set(&self.entries, model, state, None);
    }

    /// The models held (tests).
    #[cfg(test)]
    fn held(&self) -> Vec<PoseModel> {
        let mut held: Vec<PoseModel> = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect();
        held.sort_by_key(|m| m.key());
        held
    }
}

/// Set `model`'s state. With `load` = (the generation a load started in,
/// the current one), nothing is set once they differ: the models were let
/// go since.
fn set(
    entries: &Mutex<HashMap<PoseModel, ModelState>>,
    model: PoseModel,
    state: ModelState,
    load: Option<(u64, &AtomicU64)>,
) {
    let mut entries = entries.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((started, current)) = load
        && started != current.load(Ordering::SeqCst)
    {
        return;
    }
    entries.insert(model, state);
}

/// Why a load did not give models.
enum LoadError {
    /// A file is not there (or does not match).
    Missing,
    /// The files are there but could not be loaded: why.
    Failed(String),
}

/// One model's load.
struct Load {
    model: PoseModel,
    dir: PathBuf,
    source: Arc<dyn Source>,
    entries: Arc<Mutex<HashMap<PoseModel, ModelState>>>,
    loading: Arc<Mutex<()>>,
    /// The generation it started in, and the models' current one.
    generation: u64,
    current: Arc<AtomicU64>,
}

impl Load {
    /// Set the model's state, unless the models were let go since the load
    /// started.
    fn set(&self, state: ModelState) {
        set(
            &self.entries,
            self.model,
            state,
            Some((self.generation, &self.current)),
        );
    }

    /// Whether the models were let go since the load started.
    fn stale(&self) -> bool {
        self.generation != self.current.load(Ordering::SeqCst)
    }

    fn run(self) {
        let _one_at_a_time = self.loading.lock().unwrap_or_else(|e| e.into_inner());
        if self.stale() {
            return;
        }
        let label = self.model.label();
        match self.load() {
            Ok(models) => {
                let ready = ModelState::Ready(Arc::new(Shared(Mutex::new(models))));
                self.set(ready);
            }
            Err(_) if self.stale() => {}
            Err(LoadError::Missing) => {
                info!(
                    "body tracking: {label} is not downloaded yet (models folder {}); it is downloaded once the download is confirmed in the UI",
                    self.dir.display()
                );
                self.set(ModelState::Missing);
            }
            Err(LoadError::Failed(error)) => {
                warn!("body tracking: {label} could not be loaded: {error}");
                self.set(ModelState::Failed(error));
            }
        }
    }

    fn load(&self) -> Result<PoseModels, LoadError> {
        let config = pose_config(self.model, 1);
        for spec in [config.pose_model, config.detector] {
            if !self.source.verified(&spec, &self.dir) {
                return Err(LoadError::Missing);
            }
        }
        let started = Instant::now();
        let models =
            PoseModels::load(&self.dir, &config).map_err(|e| LoadError::Failed(e.to_string()))?;
        info!(
            "body tracking: {} loaded in {} ms",
            self.model.label(),
            started.elapsed().as_millis()
        );
        Ok(models)
    }
}

/// One download: the files a model lacks, one after the other.
struct Download {
    model: PoseModel,
    specs: Vec<ModelSpec>,
    dir: PathBuf,
    source: Arc<dyn Source>,
    slot: Arc<Mutex<DownloadSlot>>,
    entries: Arc<Mutex<HashMap<PoseModel, ModelState>>>,
    cancel: Arc<AtomicBool>,
}

impl Download {
    /// Update this download's entry with `f`, unless it was canceled (the
    /// slot then holds no download, or a later one).
    fn update(&self, f: impl FnOnce(&mut Running)) {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = slot.running.as_mut()
            && Arc::ptr_eq(&running.cancel, &self.cancel)
        {
            f(running);
        }
    }

    fn run(self) {
        let started = Instant::now();
        let mut result = Ok(());
        for (i, spec) in self.specs.iter().enumerate() {
            remove_stale_parts(spec, &self.dir);
            self.update(|r| r.current = i);
            info!(
                "body tracking: downloading {} ({:.1} MB) from {}",
                spec.name,
                mb(spec.size),
                spec.url
            );
            let file_started = Instant::now();
            let mut progress = |p: FetchProgress| {
                self.update(|r| {
                    let file = &mut r.files[i];
                    file.done = p.done;
                    file.total = p.total.unwrap_or(file.total).max(p.done);
                });
            };
            if let Err(e) = self
                .source
                .fetch(spec, &self.dir, &mut progress, &self.cancel)
            {
                result = Err(e);
                break;
            }
            info!(
                "body tracking: {} downloaded in {:.1} s to {} (sha256 {})",
                spec.name,
                file_started.elapsed().as_secs_f64(),
                spec.path_in(&self.dir).display(),
                spec.sha256
            );
        }
        let canceled =
            self.cancel.load(Ordering::SeqCst) || matches!(result, Err(stance::Error::Canceled));
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        let ours = slot
            .running
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(&r.cancel, &self.cancel));
        if ours {
            slot.running = None;
        }
        match result {
            _ if canceled => debug!("body tracking: the canceled download stopped"),
            Ok(()) => {
                info!(
                    "body tracking: {} downloaded in {:.1} s",
                    self.model.label(),
                    started.elapsed().as_secs_f64()
                );
                drop(slot);
                // Models wanted while their files were missing (or that
                // failed on them) load on the next request.
                self.entries
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|_, s| !matches!(s, ModelState::Missing | ModelState::Failed(_)));
            }
            Err(e) => {
                let error = e.to_string();
                warn!("body tracking: the model download failed: {error}");
                if ours {
                    slot.failed = Some((self.model, error));
                }
            }
        }
    }
}

/// Delete `spec`'s partial downloads in `dir` (`<file>.part-...`) left by a
/// flighthook stopped mid-download: those unwritten for [`STALE_PART`].
fn remove_stale_parts(spec: &ModelSpec, dir: &Path) {
    let prefix = format!("{}.part-", spec.file_name());
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= STALE_PART);
        if stale {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => info!(
                    "body tracking: deleted an unfinished download, {}",
                    entry.path().display()
                ),
                Err(e) => debug!("body tracking: {} not deleted: {e}", entry.path().display()),
            }
        }
    }
}

/// Bytes in megabytes, for logs.
fn mb(bytes: u64) -> f64 {
    // Model sizes are far below 2^52.
    #[allow(clippy::cast_precision_loss)]
    let mb = bytes as f64 / 1e6;
    mb
}

/// A stand-in for the official sources (tests): a file counts as verified
/// when it holds [`Fake::CONTENT`]; a download writes it in steps into a
/// `<file>.part-fake` file, honoring the cancel flag, or fails.
#[cfg(test)]
pub mod fake {
    use super::*;

    /// How the next downloads go.
    #[derive(Debug, Clone)]
    pub enum Outcome {
        /// They succeed.
        Succeed,
        /// They fail with this message after the first step.
        Fail(String),
    }

    #[derive(Debug)]
    pub struct Fake {
        /// Steps per file.
        pub steps: u64,
        /// Pause after each step.
        pub step: Duration,
        pub outcome: Mutex<Outcome>,
        /// Every fetch started, by file name.
        pub fetched: Mutex<Vec<String>>,
    }

    impl Fake {
        pub const CONTENT: &'static [u8] = b"fake model";

        pub fn new(steps: u64, step: Duration) -> Arc<Self> {
            Arc::new(Self {
                steps,
                step,
                outcome: Mutex::new(Outcome::Succeed),
                fetched: Mutex::default(),
            })
        }

        /// Put every file `model` needs in `dir`, as if downloaded.
        pub fn install(dir: &Path, model: PoseModel) {
            std::fs::create_dir_all(dir).expect("dir");
            for spec in specs(model) {
                std::fs::write(spec.path_in(dir), Self::CONTENT).expect("write");
            }
        }

        pub fn fetched(&self) -> Vec<String> {
            self.fetched
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    impl Source for Fake {
        fn verified(&self, spec: &ModelSpec, dir: &Path) -> bool {
            std::fs::read(spec.path_in(dir)).is_ok_and(|b| b == Self::CONTENT)
        }

        fn fetch(
            &self,
            spec: &ModelSpec,
            dir: &Path,
            progress: &mut dyn FnMut(FetchProgress),
            cancel: &AtomicBool,
        ) -> stance::Result<()> {
            self.fetched
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(spec.file_name());
            std::fs::create_dir_all(dir).expect("dir");
            let part = dir.join(format!("{}.part-fake", spec.file_name()));
            let total = spec.size;
            progress(FetchProgress {
                done: 0,
                total: Some(total),
            });
            for n in 1..=self.steps {
                std::fs::write(&part, format!("{n}")).expect("part");
                std::thread::sleep(self.step);
                if cancel.load(Ordering::SeqCst) {
                    let _ = std::fs::remove_file(&part);
                    return Err(stance::Error::Canceled);
                }
                let outcome = self
                    .outcome
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Outcome::Fail(message) = outcome {
                    let _ = std::fs::remove_file(&part);
                    return Err(stance::Error::Download {
                        url: spec.url.to_owned(),
                        message,
                    });
                }
                progress(FetchProgress {
                    done: total * n / self.steps,
                    total: Some(total),
                });
            }
            std::fs::write(&part, Self::CONTENT).expect("part");
            std::fs::rename(&part, spec.path_in(dir)).expect("rename");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Fake, Outcome};
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "flighthook-models-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn wait_settled(models: &Models, model: PoseModel, within: Duration) -> ModelState {
        let until = Instant::now() + within;
        while Instant::now() < until {
            match models.state(model) {
                Some(ModelState::Loading) | None => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Some(state) => return state,
            }
        }
        panic!("the load did not finish");
    }

    fn wait_until(within: Duration, mut done: impl FnMut() -> bool) {
        let until = Instant::now() + within;
        while !done() {
            assert!(Instant::now() < until, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_missing_model_is_missing_and_never_downloaded_on_its_own() {
        let dir = temp_dir("missing");
        let fake = Fake::new(2, Duration::ZERO);
        let models = Models::new(dir.clone(), fake.clone());
        assert!(models.state(PoseModel::RtmposeS).is_none(), "not asked yet");
        assert!(matches!(
            models.want(PoseModel::RtmposeS),
            ModelState::Loading | ModelState::Missing
        ));
        assert!(matches!(
            wait_settled(&models, PoseModel::RtmposeS, Duration::from_secs(5)),
            ModelState::Missing
        ));
        for _ in 0..20 {
            assert!(matches!(
                models.want(PoseModel::RtmposeS),
                ModelState::Missing
            ));
        }
        assert!(models.state(PoseModel::RtmposeM).is_none(), "per model");
        assert!(!models.downloading());
        assert!(fake.fetched().is_empty(), "no download without asking");
        assert!(!dir.exists(), "nothing written");
        let files = models.files(PoseModel::RtmposeS);
        assert_eq!(files.state, ModelFilesState::Missing);
        assert_eq!(
            files.files.iter().map(|f| &f.name[..]).collect::<Vec<_>>(),
            ["rtmpose-s-halpe26.onnx", "yolox-tiny.onnx"]
        );
        assert!(!dir.exists(), "still nothing written");
    }

    #[test]
    fn files_already_there_are_ready_and_never_downloaded() {
        let dir = temp_dir("ready");
        let fake = Fake::new(1, Duration::ZERO);
        Fake::install(&dir, PoseModel::RtmposeM);
        let models = Models::new(dir.clone(), fake.clone());
        let m = models.files(PoseModel::RtmposeM);
        assert_eq!(m.state, ModelFilesState::Ready);
        assert!(m.missing().is_empty());
        assert_eq!(models.download(PoseModel::RtmposeM), DownloadStart::Ready);
        assert!(!models.downloading());
        assert!(fake.fetched().is_empty());
        // The other model shares the detector: only its pose model is
        // missing.
        let s = models.files(PoseModel::RtmposeS);
        assert_eq!(s.state, ModelFilesState::Missing);
        assert_eq!(s.missing().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_files_name_what_is_downloaded_and_from_where() {
        let models = Models::new(temp_dir("names"), Fake::new(1, Duration::ZERO));
        let m = models.files(PoseModel::RtmposeM);
        let named: Vec<_> = m
            .files
            .iter()
            .map(|f| (&f.label[..], &f.host[..], f.size, f.present))
            .collect();
        assert_eq!(
            named,
            [
                (
                    "RTMPose-m pose model",
                    "download.openmmlab.com",
                    55_685_444,
                    false
                ),
                (
                    "YOLOX-Tiny person detector",
                    "github.com",
                    20_219_662,
                    false
                ),
            ]
        );
        assert_eq!(m.files.iter().map(|f| f.size).sum::<u64>() / 1_000_000, 75);
        assert!(m.dir.contains("flighthook-models-names"));
        assert_eq!(models.files(PoseModel::RtmposeS).files[0].size, 22_793_379);
    }

    #[test]
    fn a_download_reports_its_progress_and_the_model_then_loads() {
        let dir = temp_dir("progress");
        let fake = Fake::new(20, Duration::from_millis(15));
        let models = Models::new(dir.clone(), fake.clone());
        let m = PoseModel::RtmposeS;
        models.want(m);
        assert!(matches!(
            wait_settled(&models, m, Duration::from_secs(5)),
            ModelState::Missing
        ));
        assert_eq!(models.download(m), DownloadStart::Started);
        assert_eq!(models.download(m), DownloadStart::Running, "idempotent");
        // Mid-download: per file and overall.
        let mut mid = None;
        wait_until(Duration::from_secs(10), || {
            let f = models.files(m);
            if let Some(d) = &f.download
                && d.done > 0
                && d.done < d.total
            {
                mid = Some(f);
                return true;
            }
            false
        });
        let mid = mid.expect("progress");
        assert_eq!(mid.state, ModelFilesState::Downloading);
        let d = mid.download.as_ref().expect("download");
        assert_eq!(d.files.len(), 2);
        assert_eq!(d.total, 22_793_379 + 20_219_662);
        assert_eq!(d.done, d.files.iter().map(|f| f.done).sum::<u64>());
        assert_eq!(
            d.current_file().map(|f| &f.host[..]),
            Some(if d.current == 0 {
                "download.openmmlab.com"
            } else {
                "github.com"
            })
        );
        assert!(models.downloading());
        wait_until(Duration::from_secs(10), || !models.downloading());
        let done = models.files(m);
        assert_eq!(done.state, ModelFilesState::Ready);
        assert!(done.download.is_none() && done.error.is_none());
        assert_eq!(
            fake.fetched(),
            ["rtmpose-s-halpe26.onnx", "yolox-tiny.onnx"],
            "each file once"
        );
        // The model wanted while missing now loads (the fake files fail to
        // load as models, which shows the load was tried).
        models.want(m);
        assert!(matches!(
            wait_settled(&models, m, Duration::from_secs(5)),
            ModelState::Failed(_)
        ));
        // Nothing left to download.
        assert_eq!(models.download(m), DownloadStart::Ready);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_files_missing_are_downloaded() {
        let dir = temp_dir("partial");
        let fake = Fake::new(1, Duration::ZERO);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("yolox-tiny.onnx"), Fake::CONTENT).expect("write");
        let models = Models::new(dir.clone(), fake.clone());
        let files = models.files(PoseModel::RtmposeM);
        assert_eq!(files.missing().len(), 1);
        assert_eq!(models.download(PoseModel::RtmposeM), DownloadStart::Started);
        wait_until(Duration::from_secs(5), || !models.downloading());
        assert_eq!(fake.fetched(), ["rtmpose-m-halpe26.onnx"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_canceled_download_removes_its_partial_file_and_leaves_no_failure() {
        let dir = temp_dir("cancel");
        let fake = Fake::new(1000, Duration::from_millis(5));
        let models = Models::new(dir.clone(), fake.clone());
        let m = PoseModel::RtmposeM;
        assert_eq!(models.download(m), DownloadStart::Started);
        let part = dir.join("rtmpose-m-halpe26.onnx.part-fake");
        wait_until(Duration::from_secs(5), || part.exists());
        assert!(models.cancel_download());
        assert!(!models.cancel_download(), "nothing left to cancel");
        assert_eq!(models.files(m).state, ModelFilesState::Missing, "at once");
        wait_until(Duration::from_secs(5), || !part.exists());
        // The thread has ended: nothing in the folder, no failure.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
        let after = models.files(m);
        assert_eq!(after.state, ModelFilesState::Missing);
        assert!(after.error.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_download_says_why_until_retried() {
        let dir = temp_dir("fail");
        let fake = Fake::new(3, Duration::from_millis(1));
        *fake.outcome.lock().unwrap() = Outcome::Fail("connection refused".into());
        let models = Models::new(dir.clone(), fake.clone());
        let m = PoseModel::RtmposeS;
        assert_eq!(models.download(m), DownloadStart::Started);
        wait_until(Duration::from_secs(5), || !models.downloading());
        let failed = models.files(m);
        assert_eq!(failed.state, ModelFilesState::Failed);
        assert!(
            failed
                .error
                .as_deref()
                .unwrap()
                .contains("connection refused"),
            "{failed:?}"
        );
        // The other model's state does not carry it.
        assert_eq!(
            models.files(PoseModel::RtmposeM).state,
            ModelFilesState::Missing
        );
        // Retry: it works this time.
        *fake.outcome.lock().unwrap() = Outcome::Succeed;
        assert_eq!(models.download(m), DownloadStart::Started);
        assert!(models.files(m).error.is_none(), "cleared by the retry");
        wait_until(Duration::from_secs(5), || !models.downloading());
        assert_eq!(models.files(m).state, ModelFilesState::Ready);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_models_match_the_settings() {
        assert_eq!(spec(PoseModel::RtmposeM), RTMPOSE_M_HALPE26);
        assert_eq!(spec(PoseModel::RtmposeS), RTMPOSE_S_HALPE26);
        let c = pose_config(PoseModel::RtmposeS, 0);
        assert_eq!(c.detect_every, 1);
        assert_eq!(c.pose_model, RTMPOSE_S_HALPE26);
        assert_eq!(c.detector, PoseConfig::default().detector);
        assert!(c.smoothing.is_none());
        assert_eq!(specs(PoseModel::RtmposeM)[1], c.detector);
        assert!(default_dir().ends_with("flighthook/models"));
        assert_eq!(
            default_dir().parent(),
            crate::state::clips::default_dir().parent(),
            "beside the clips"
        );
    }

    #[test]
    fn the_official_check_hashes_a_file_once_until_it_changes() {
        let dir = temp_dir("official");
        std::fs::create_dir_all(&dir).expect("dir");
        let official = Official::default();
        let spec = stance::pose::YOLOX_NANO;
        assert!(!official.verified(&spec, &dir), "absent");
        std::fs::write(spec.path_in(&dir), b"not the model").expect("write");
        assert!(!official.verified(&spec, &dir), "wrong bytes");
        assert_eq!(
            official.checked.lock().unwrap().len(),
            1,
            "remembered by stamp"
        );
        assert!(!official.verified(&spec, &dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unloading_lets_every_model_go_and_a_load_running_keeps_nothing() {
        let models = Models::new(temp_dir("unload"), Fake::new(1, Duration::ZERO));
        let (s, m) = (PoseModel::RtmposeS, PoseModel::RtmposeM);
        models.set_state(s, ModelState::Loading);
        assert!(matches!(models.state(s), Some(ModelState::Loading)));
        // A load started now ...
        let started = models.generation.load(Ordering::SeqCst);
        models.unload();
        assert!(models.held().is_empty());
        // ... finishing after the models were let go keeps nothing.
        let failed = ModelState::Failed("broken".into());
        set(
            &models.entries,
            m,
            failed,
            Some((started, &models.generation)),
        );
        assert!(models.state(m).is_none());
        // A load started after it is kept.
        let now = models.generation.load(Ordering::SeqCst);
        set(
            &models.entries,
            m,
            ModelState::Loading,
            Some((now, &models.generation)),
        );
        assert!(matches!(models.state(m), Some(ModelState::Loading)));
    }

    #[test]
    fn unfinished_downloads_left_over_are_deleted() {
        let dir = temp_dir("parts");
        std::fs::create_dir_all(&dir).expect("dir");
        let spec = spec(PoseModel::RtmposeS);
        let old = dir.join(format!("{}.part-123-0", spec.file_name()));
        let fresh = dir.join(format!("{}.part-456-0", spec.file_name()));
        let other = dir.join("yolox-tiny.onnx.part-123-0");
        let model = dir.join(spec.file_name());
        for path in [&old, &fresh, &other, &model] {
            std::fs::write(path, b"x").expect("write");
        }
        let long_ago = std::time::SystemTime::now() - STALE_PART - Duration::from_secs(60);
        for path in [&old, &other, &model] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .and_then(|f| f.set_modified(long_ago))
                .expect("mtime");
        }
        remove_stale_parts(&spec, &dir);
        assert!(!old.exists(), "left over");
        assert!(fresh.exists(), "maybe being written");
        assert!(other.exists(), "another model's");
        assert!(model.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With the real models (`STANCE_MODEL_DIR`): a model no longer wanted
    /// is let go once unused.
    #[test]
    fn the_model_no_longer_wanted_is_let_go_when_unused() {
        let Some(dir) = std::env::var_os("STANCE_MODEL_DIR") else {
            eprintln!("skipped: STANCE_MODEL_DIR is not set");
            return;
        };
        let models = Models::new(PathBuf::from(dir), Arc::new(Official::default()));
        let (s, m) = (PoseModel::RtmposeS, PoseModel::RtmposeM);
        models.want(s);
        let held_s = wait_settled(&models, s, Duration::from_secs(300))
            .ready()
            .expect("ready");
        // In use (a live view making instances): kept.
        models.want(m);
        assert!(
            wait_settled(&models, m, Duration::from_secs(300))
                .ready()
                .is_some()
        );
        models.want(m);
        assert_eq!(models.held(), [m, s]);
        drop(held_s);
        models.want(m);
        assert_eq!(models.held(), [m]);
    }

    /// The real models from `STANCE_MODEL_DIR`, when it is set.
    #[test]
    fn the_real_models_load_from_a_folder_when_there_are_some() {
        let Some(dir) = std::env::var_os("STANCE_MODEL_DIR") else {
            eprintln!("skipped: STANCE_MODEL_DIR is not set");
            return;
        };
        let models = Models::new(PathBuf::from(dir), Arc::new(Official::default()));
        assert_eq!(
            models.files(PoseModel::RtmposeS).state,
            ModelFilesState::Ready
        );
        models.want(PoseModel::RtmposeS);
        let state = wait_settled(&models, PoseModel::RtmposeS, Duration::from_secs(300));
        assert!(state.ready().is_some(), "{state:?}");
        assert_eq!(state.key(), "ready");
    }
}
