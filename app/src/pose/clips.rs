//! Body tracking of recorded swings (alpha): one background worker
//! (`pose:clips`) finds the golfer's keypoints on every frame of a clip and
//! stores them beside it as `<camera>.pose.json`, then analyzes the swing
//! in them and stores `<camera>.swing.json` (a face-on or down-the-line
//! camera only; see [`super::swing`]).
//!
//! Which clips are tracked follows `[body_tracking] mode`:
//!
//! - Live: every clip, queued when its camera writes it, no sooner than
//!   [`CLIP_DELAY`] after, so the pass stays clear of the shot itself.
//! - Historical: a clip the first time it is viewed ([`request`]), at once.
//!   A clip already tracked, queued or being tracked is not queued again.
//! - Off: none. Turning it off drops the clips waiting
//!   ([`ClipQueue::clear`]); a pass already running finishes.
//!
//! Clips are done one at a time: a viewed clip before those queued earlier
//! (the clip on screen is the one wanted now), recorded clips oldest first.
//! The queue holds at most `[replay] keep_shots` × the number of cameras
//! (the clips the store keeps); when it is full the clip queued longest
//! ago is dropped with a warning. A clip pruned or cleared before its turn
//! is skipped, and so is a recorded clip whose turn comes after the mode
//! left Live. Each clip runs on a quarter of the computer's logical CPUs
//! ([`clip_threads`]), at below-normal priority ([`super::priority`]), so a
//! simulator on the same computer keeps the CPU it needs. A clip whose pass
//! fails unexpectedly (a panic) is logged and the queue goes on. When its
//! pose file is written the clip store marks it and one `shot_pose` event
//! tells the UI; when its swing file is, one `shot_swing` event. No
//! keypoints or numbers go on the bus.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flighthook::{BodyTrackingMode, CameraPosition, Handedness, PoseModel, ShotKey};
use stance::pose::{PoseConfig, PoseTrack, Rotation};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use super::models::{ModelState, pose_config};
use super::swing::{SwingInput, SwingWritten, write_report};
use crate::bus::{BusMsg, LocalEvent};
use crate::state::SystemState;
use crate::state::clips::ClipSlot;

/// How often a clip waiting for the models or its turn looks again.
const MODEL_POLL: Duration = Duration::from_millis(250);

/// Least time from a clip being written to the start of its pass.
pub const CLIP_DELAY: Duration = Duration::from_secs(3);

/// A finished clip to track.
pub struct ClipJob {
    /// The clip's AVI (`<clip folder>/<camera>.avi`), its sidecar beside it.
    pub avi: PathBuf,
    /// The clip's folder in the store.
    pub slot: ClipSlot,
    /// The camera actor that recorded it (`camera.0`).
    pub camera: String,
    /// The clip's shots.
    pub keys: Vec<ShotKey>,
    /// Where the camera stood.
    pub position: CameraPosition,
    /// The player's hand when the shot triggered.
    pub hand: Option<Handedness>,
    /// The turn's golfer when the shot triggered, whose height the swing
    /// analysis reads.
    pub golfer: Option<String>,
    /// The clip's estimated impact, ns after its first frame (its
    /// sidecar's), when it falls inside the clip.
    pub impact_ns: Option<u64>,
    /// When the clip was written: a recorded clip's pass starts
    /// [`CLIP_DELAY`] after.
    pub written: Instant,
    /// A viewer asked for it ([`request`]): its pass starts at once, and
    /// in any mode but Off.
    pub on_demand: bool,
    /// How the estimate was made, for the impact log line.
    pub timing: ClipTiming,
    /// Where `shot_pose` and `shot_swing` go, with the camera as its actor.
    pub bus: broadcast::Sender<BusMsg>,
}

/// How a clip's impact was estimated: the launch monitor, its camera
/// timing and the camera's latency.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipTiming {
    /// The kind of launch monitor that reported the shot (`flightscope`,
    /// `garmin_r10`, ...).
    pub device: String,
    /// The impact's lead before the trigger, ms.
    pub impact_lead_ms: u64,
    /// The camera's latency, ms.
    pub latency_ms: u64,
}

/// A clip waiting, and when it was queued.
struct Queued {
    job: ClipJob,
    at: Instant,
}

#[derive(Default)]
struct Inner {
    /// In the order they are taken.
    jobs: VecDeque<Queued>,
    /// The worker thread is running.
    working: bool,
    /// The clip being tracked now: its folder id and camera.
    running: Option<(String, String)>,
}

impl Inner {
    /// Where camera `camera`'s clip in folder `id` waits, if it does.
    fn position(&self, id: &str, camera: &str) -> Option<usize> {
        self.jobs
            .iter()
            .position(|q| q.job.slot.id == id && q.job.camera == camera)
    }

    /// Whether that clip is being tracked now.
    fn is_running(&self, id: &str, camera: &str) -> bool {
        self.running
            .as_ref()
            .is_some_and(|(i, c)| i == id && c == camera)
    }
}

/// What [`ClipQueue::push`] did with a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// It was queued.
    Queued,
    /// It was already waiting or being tracked: nothing changed (a
    /// recorded clip keeps its [`CLIP_DELAY`] even when it is viewed).
    Already,
}

/// The clips waiting for body tracking.
#[derive(Default)]
pub struct ClipQueue {
    inner: Arc<Mutex<Inner>>,
    /// A stand-in for the models and the clip pass, in tests.
    #[cfg(test)]
    fake: Mutex<Option<FakePass>>,
}

/// A clip pass without models, for tests.
#[cfg(test)]
pub type FakePass = fn(&std::path::Path, &PoseConfig) -> stance::Result<PoseTrack>;

/// Worker threads for one clip: a quarter of the logical CPUs, at least 1.
pub fn clip_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| (n.get() / 4).max(1))
}

/// The clip pass's settings for `model` on a camera turned by `rotation`:
/// [`clip_threads`] workers, each lowering its own priority first
/// ([`super::priority::worker_hook`]), detecting every 10 frames, every
/// frame turned upright before detection.
pub fn clip_config(model: PoseModel, rotation: Rotation) -> PoseConfig {
    PoseConfig {
        threads: clip_threads(),
        on_worker_start: Some(super::priority::worker_hook()),
        rotation,
        ..pose_config(model, PoseConfig::default().detect_every)
    }
}

/// Most clips waiting: one per clip the store keeps.
pub fn bound(state: &SystemState) -> usize {
    let keep = usize::try_from(crate::state::clips::keep(&state.system.replay())).unwrap_or(1);
    keep.saturating_mul(state.system.camera_count().max(1))
}

impl ClipQueue {
    /// Queue `job`, starting the worker when it is not running: a viewed
    /// clip (`on_demand`) first, a recorded one last. With `bound` clips
    /// already waiting, the one queued longest ago is dropped first. A clip
    /// already waiting or being tracked is not queued twice.
    pub fn push(&self, state: &Arc<SystemState>, job: ClipJob, bound: usize) -> Pushed {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.is_running(&job.slot.id, &job.camera)
            || inner.position(&job.slot.id, &job.camera).is_some()
        {
            return Pushed::Already;
        }
        while inner.jobs.len() >= bound.max(1) {
            let oldest = inner
                .jobs
                .iter()
                .enumerate()
                .min_by_key(|(_, q)| q.at)
                .map(|(i, _)| i);
            if let Some(dropped) = oldest.and_then(|i| inner.jobs.remove(i)) {
                warn!(
                    "{}: too many clips waiting for body tracking; skipping {}",
                    dropped.job.camera,
                    dropped.job.avi.display()
                );
            }
        }
        let queued = Queued {
            at: Instant::now(),
            job,
        };
        if queued.job.on_demand {
            inner.jobs.push_front(queued);
        } else {
            inner.jobs.push_back(queued);
        }
        if inner.working {
            return Pushed::Queued;
        }
        inner.working = true;
        drop(inner);
        let queue = Arc::clone(&self.inner);
        let state = Arc::clone(state);
        let spawned = std::thread::Builder::new()
            .name("pose:clips".to_owned())
            .spawn(move || work(&state, &queue));
        if let Err(e) = spawned {
            warn!("body tracking: no thread for the recorded clips: {e}");
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.working = false;
            inner.jobs.clear();
        }
        Pushed::Queued
    }

    /// Drop every clip waiting (body tracking turned off). A pass already
    /// running finishes. Returns how many were dropped.
    pub fn clear(&self) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let dropped = inner.jobs.len();
        inner.jobs.clear();
        dropped
    }

    /// Whether camera `camera`'s clip in folder `id` is waiting or being
    /// tracked.
    pub fn has(&self, id: &str, camera: &str) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.is_running(id, camera) || inner.position(id, camera).is_some()
    }

    /// Run `pass` instead of the models from now on (tests).
    #[cfg(test)]
    pub fn fake(&self, pass: FakePass) {
        *self.fake.lock().unwrap_or_else(|e| e.into_inner()) = Some(pass);
    }

    /// Clips waiting, in the order they are taken, by their AVI (tests).
    #[cfg(test)]
    pub fn waiting(&self) -> Vec<PathBuf> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.jobs.iter().map(|q| q.job.avi.clone()).collect()
    }

    /// Whether the worker thread is running (tests).
    #[cfg(test)]
    pub fn working(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).working
    }
}

/// What [`request`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requested {
    /// The clip's keypoints are there already.
    Done,
    /// The clip is queued, or already waiting or being tracked.
    Queued,
    /// Body tracking is off.
    Off,
    /// The shot has no clip from that camera.
    NoClip,
}

/// Track camera `camera`'s clip of shot `shot_id` now, for a viewer: queued
/// first ([`ClipQueue::push`]), its pass starting at once, unless its
/// keypoints are there already, it is already queued, or body tracking is
/// off. With `force`, a clip tracked already has its body tracking and
/// swing analysis deleted ([`ClipStore::clear_analysis`]) and is tracked
/// again. The models load with the first such clip; while their files are
/// not downloaded, the clip is left untracked. Its
/// `shot_pose` and `shot_swing` go out on `bus` with the camera as their
/// actor.
///
/// [`ClipStore::clear_analysis`]: crate::state::clips::ClipStore::clear_analysis
pub fn request(
    state: &Arc<SystemState>,
    shot_id: &str,
    camera: &str,
    force: bool,
    bus: &broadcast::Sender<BusMsg>,
) -> Requested {
    let Some(clip) = state.clips.camera_clip(shot_id, camera) else {
        return Requested::NoClip;
    };
    if !state.system.body_tracking().mode.is_on() {
        return Requested::Off;
    }
    if force {
        state.clips.clear_analysis(&clip.slot, camera);
    } else if clip.video.pose {
        return Requested::Done;
    }
    if state.pose.clips.has(&clip.slot.id, camera) {
        return Requested::Queued;
    }
    let sidecar = match stance::read_sidecar(&clip.avi.with_extension("json")) {
        Ok(sidecar) => Some(sidecar),
        Err(e) => {
            debug!("{camera}: no impact for a clip's swing analysis: {e}");
            None
        }
    };
    let first = clip.shots.first();
    let job = ClipJob {
        avi: clip.avi,
        slot: clip.slot,
        camera: camera.to_owned(),
        keys: clip.shots.iter().map(|s| s.key.clone()).collect(),
        position: clip.video.position,
        hand: first.and_then(|s| s.hand),
        golfer: first.and_then(|s| s.golfer.clone()),
        impact_ns: sidecar
            .as_ref()
            .and_then(crate::actors::camera::clip_impact),
        written: Instant::now(),
        on_demand: true,
        timing: ClipTiming {
            device: first.map(|s| s.device.clone()).unwrap_or_default(),
            impact_lead_ms: first.map_or(0, |s| s.impact_lead_ms),
            latency_ms: u64::try_from(crate::actors::camera::latency(state, camera).as_millis())
                .unwrap_or(u64::MAX),
        },
        bus: bus.clone(),
    };
    state.pose.clips.push(state, job, bound(state));
    Requested::Queued
}

/// Take the jobs one at a time until none is left, at below-normal
/// priority. A job that panics is logged and the next one taken.
fn work(state: &SystemState, queue: &Mutex<Inner>) {
    super::priority::lower_this_thread();
    // Should the thread unwind anyway, the next push starts a new worker.
    let _unwinding = Idle(queue);
    loop {
        let job = {
            let mut inner = queue.lock().unwrap_or_else(|e| e.into_inner());
            match inner.jobs.pop_front() {
                Some(Queued { job, .. }) => {
                    inner.running = Some((job.slot.id.clone(), job.camera.clone()));
                    job
                }
                None => {
                    // Under the lock a push takes: a clip queued from now
                    // on starts a new worker.
                    inner.running = None;
                    inner.working = false;
                    return;
                }
            }
        };
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(state, &job)));
        queue.lock().unwrap_or_else(|e| e.into_inner()).running = None;
        if ran.is_err() {
            warn!(
                "{}: body tracking of {} stopped on an internal error",
                job.camera,
                job.avi.display()
            );
        }
    }
}

/// Marks the queue's worker as gone when its thread unwinds.
struct Idle<'a>(&'a Mutex<Inner>);

impl Drop for Idle<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
            inner.working = false;
            inner.running = None;
        }
    }
}

/// Sleep until `job`'s turn: [`CLIP_DELAY`] after a recorded clip was
/// written, at once for a viewed one. False when the clip is pruned or
/// cleared meanwhile.
fn wait_turn(state: &SystemState, job: &ClipJob) -> bool {
    let due = if job.on_demand {
        job.written
    } else {
        job.written + CLIP_DELAY
    };
    loop {
        if !state.clips.is_current(&job.slot) {
            return false;
        }
        let now = Instant::now();
        if now >= due {
            return true;
        }
        std::thread::sleep((due - now).min(MODEL_POLL));
    }
}

/// Whether `job` is still wanted in `mode`: none in Off, a recorded clip
/// only in Live.
fn wanted(job: &ClipJob, mode: BodyTrackingMode) -> bool {
    match mode {
        BodyTrackingMode::Off => false,
        BodyTrackingMode::Historical => job.on_demand,
        BodyTrackingMode::Live => true,
    }
}

/// Track one clip with the models of the settings in effect, once its turn
/// has come, waiting while they load. Returns whether its pose file is
/// kept.
fn run(state: &SystemState, job: &ClipJob) -> bool {
    if !wait_turn(state, job) {
        return false;
    }
    let settings = state.system.body_tracking();
    if !wanted(job, settings.mode) {
        return false;
    }
    #[cfg(test)]
    if let Some(pass) = *state
        .pose
        .clips
        .fake
        .lock()
        .unwrap_or_else(|e| e.into_inner())
    {
        return process(state, job, settings.model, &pass);
    }
    let models = loop {
        if !state.clips.is_current(&job.slot) || !wanted(job, state.system.body_tracking().mode) {
            return false;
        }
        match state.pose.models.want(settings.model) {
            ModelState::Ready(models) => break models,
            ModelState::Failed(e) => {
                // The failed load was logged once already.
                debug!("{}: no body tracking for a clip: {e}", job.camera);
                return false;
            }
            ModelState::Missing => {
                debug!(
                    "{}: no body tracking for a clip: the models are not downloaded",
                    job.camera
                );
                return false;
            }
            ModelState::Loading => std::thread::sleep(MODEL_POLL),
        }
    };
    let models = match models.fork() {
        Ok(models) => models,
        Err(e) => {
            warn!("{}: no body tracking for a clip: {e}", job.camera);
            return false;
        }
    };
    process(state, job, settings.model, &|avi, config| {
        stance::pose::analyze_clip(&models, avi, config)
    })
}

/// The clip pass: stance's `analyze_clip` on loaded models, or a stand-in
/// in tests.
type Analyze<'a> = &'a dyn Fn(&std::path::Path, &PoseConfig) -> stance::Result<PoseTrack>;

/// Track one clip with `model` and store its keypoints, then announce
/// them. Returns whether the pose file is kept.
fn process(state: &SystemState, job: &ClipJob, model: PoseModel, analyze: Analyze<'_>) -> bool {
    let camera = &job.camera;
    if !state.clips.is_current(&job.slot) {
        return false;
    }
    let rotation = state.system.camera_rotation(camera);
    let config = clip_config(model, Rotation::from_degrees(rotation).unwrap_or_default());
    let threads = config.threads;
    let started = Instant::now();
    let track = match analyze(&job.avi, &config) {
        Ok(track) => track,
        Err(e) => {
            warn!(
                "{camera}: body tracking of {} failed: {e}",
                job.avi.display()
            );
            return false;
        }
    };
    let took = started.elapsed();
    let path = match track.write_beside(&job.avi) {
        Ok(path) => path,
        Err(e) => {
            // The folder may have gone (pruned, cleared) while it ran.
            if state.clips.is_current(&job.slot) {
                warn!("{camera}: writing the body tracking of a clip: {e}");
            }
            return false;
        }
    };
    if !state.clips.set_pose(&job.slot, camera, rotation) {
        let _ = std::fs::remove_file(&path);
        return false;
    }
    let found = track
        .frames
        .iter()
        .filter(|f| !f.keypoints.is_empty())
        .count();
    let shot_id = job.keys.first().map_or("-", |k| k.shot_id.as_str());
    info!(
        "{}",
        timing_line(camera, shot_id, track.frames.len(), took, threads, found)
    );
    announce(
        job,
        LocalEvent::ShotPose {
            keys: job.keys.clone(),
            rotation,
        },
    );
    swing(state, job, &track);
    true
}

/// The log line of one clip pass, to size a computer for body tracking:
/// `body tracking: <camera> shot <id>: <frames> frames in <s> s (<threads>
/// threads)`, then the time a frame and the frames the golfer was found on.
fn timing_line(
    camera: &str,
    shot_id: &str,
    frames: usize,
    took: Duration,
    threads: usize,
    found: usize,
) -> String {
    // Frame counts are far below 2^52.
    #[allow(clippy::cast_precision_loss)]
    let per_frame = took.as_secs_f64() * 1000.0 / frames.max(1) as f64;
    format!(
        "body tracking: {camera} shot {shot_id}: {frames} frames in {:.1} s ({threads} threads) · {per_frame:.1} ms a frame, golfer on {found} frames",
        took.as_secs_f64(),
    )
}

/// Send `event` for `job`'s clip, with its camera as the card.
fn announce(job: &ClipJob, event: LocalEvent) {
    let _ = job.bus.send(BusMsg::local(job.camera.clone(), event));
}

/// The facts of a clip's golfer (`golfer`, recorded at the trigger), by the
/// lookup rule, for its swing report and its grades alike: a clip with no
/// golfer has no height and the default sex, never the golfer up's.
pub(crate) fn clip_golfer_facts(
    state: &SystemState,
    golfer: Option<&str>,
) -> flighthook::PlayerSection {
    state.system.golfer_facts(golfer)
}

/// Analyze the swing in `track`, `job`'s keypoints, store it beside the
/// clip and announce it. Returns whether the swing file is kept.
fn swing(state: &SystemState, job: &ClipJob, track: &PoseTrack) -> bool {
    let camera = &job.camera;
    let input = SwingInput {
        position: job.position,
        hand: job.hand,
        impact_ns: job.impact_ns,
        height_cm: clip_golfer_facts(state, job.golfer.as_deref()).height,
        mirrored: super::swing::mirrored(camera),
    };
    let written = match write_report(camera, track, &job.avi, &input) {
        Ok(Some(written)) => written,
        Ok(None) => return false,
        Err(e) => {
            if state.clips.is_current(&job.slot) {
                info!("{camera}: no swing analysis of {}: {e}", job.avi.display());
            }
            return false;
        }
    };
    if !state.clips.set_swing(&job.slot, camera) {
        let _ = std::fs::remove_file(&written.path);
        return false;
    }
    log_impact(job, &written);
    announce(
        job,
        LocalEvent::ShotSwing {
            keys: job.keys.clone(),
        },
    );
    true
}

/// Logs how far the swing's impact is from the clip's estimate
/// ([`impact_line`]).
fn log_impact(job: &ClipJob, written: &SwingWritten) {
    if let Some(line) = impact_line(job, written) {
        info!("{line}");
    }
}

/// How far the swing's impact is from the clip's estimate, to calibrate
/// each launch monitor's camera timing: the refined impact minus the
/// estimate in ms, or that no swing was found near the estimate. `None`
/// without an estimate.
fn impact_line(job: &ClipJob, written: &SwingWritten) -> Option<String> {
    job.impact_ns?;
    let shots = job
        .keys
        .iter()
        .map(|k| format!("#{} ({})", k.shot_number, k.shot_id))
        .collect::<Vec<_>>()
        .join(", ");
    let ClipTiming {
        device,
        impact_lead_ms,
        latency_ms,
    } = &job.timing;
    let timing = format!("{device}, camera timing {impact_lead_ms} ms, latency {latency_ms} ms");
    let camera = &job.camera;
    match written.impact_shift_ns {
        Some(ns) => Some(format!(
            "{camera}: shot {shots}: swing impact {:+.0} ms from the clip's ({timing})",
            ns as f64 / 1e6
        )),
        None if !written.swing_found => Some(format!(
            "{camera}: shot {shots}: no swing found near the clip's impact ({timing})"
        )),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::web::tests::TempDir;
    use crate::state::clips::ClipShot;
    use flighthook::{CameraPosition, ShotVideo};

    fn state() -> (TempDir, Arc<SystemState>) {
        state_with("")
    }

    /// A state loaded from config text `toml`, body tracking Live.
    fn state_with(toml: &str) -> (TempDir, Arc<SystemState>) {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-pose-queue-{}",
            uuid::Uuid::new_v4().simple()
        )));
        std::fs::create_dir_all(&dir.0).expect("dir");
        std::fs::write(dir.0.join("config.toml"), toml).expect("config");
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        state
            .system
            .update(|c| c.body_tracking.mode = BodyTrackingMode::Live);
        (dir, Arc::new(state))
    }

    fn set_mode(state: &SystemState, mode: BodyTrackingMode) {
        state.system.update(|c| c.body_tracking.mode = mode);
    }

    fn key(n: u32) -> ShotKey {
        ShotKey {
            shot_id: format!("shot-{n}"),
            shot_number: n,
        }
    }

    /// A finished camera.0 clip of shot `n` in the store, with a stand-in
    /// AVI.
    fn clip(state: &SystemState, dir: &TempDir, n: u32) -> ClipJob {
        let shot = ClipShot {
            key: key(n),
            device: "mock".into(),
            impact_lead_ms: 0,
            hand: None,
            golfer: None,
        };
        let slot = state
            .clips
            .begin(&dir.0.join("clips"), &[shot], state.clips.generation())
            .expect("begin");
        let avi = slot.dir.join("camera.0.avi");
        std::fs::write(&avi, b"RIFF").expect("avi");
        let video = ShotVideo {
            camera: "camera.0".into(),
            position: CameraPosition::GolferFront,
            duration_ms: 1000,
            fps: 30.0,
            trigger_offset_ms: 500,
            impact_offset_ms: 500,
            complete: true,
            dropped: 0,
            pose: false,
            swing: false,
            rotation: 0,
            analyzed_with: None,
        };
        assert!(state.clips.finish(&slot, video, 20).kept);
        ClipJob {
            avi,
            slot,
            camera: "camera.0".into(),
            keys: vec![key(n)],
            position: CameraPosition::GolferFront,
            hand: None,
            golfer: None,
            impact_ns: None,
            written: Instant::now()
                .checked_sub(CLIP_DELAY)
                .unwrap_or_else(Instant::now),
            on_demand: false,
            timing: ClipTiming::default(),
            bus: broadcast::channel(16).0,
        }
    }

    fn track(frames: usize) -> PoseTrack {
        PoseTrack {
            version: stance::pose::POSE_TRACK_VERSION,
            skeleton: stance::pose::Skeleton::Halpe26,
            model: "fake".into(),
            detector: "fake".into(),
            width: 64,
            height: 48,
            rotation: Rotation::None,
            frames: (0..frames as u64)
                .map(|i| stance::pose::PoseFrame::empty(i * 33_333_333))
                .collect(),
        }
    }

    #[test]
    fn the_queue_is_bounded_and_drops_the_oldest() {
        let (dir, state) = state();
        let queue = ClipQueue::default();
        // Hold the worker off, as if it were busy with a clip.
        queue.inner.lock().unwrap().working = true;
        for n in 1..=4 {
            queue.push(&state, clip(&state, &dir, n), 3);
        }
        let waiting: Vec<String> = queue
            .waiting()
            .iter()
            .map(|p| p.parent().unwrap().display().to_string())
            .collect();
        assert_eq!(waiting.len(), 3);
        // Oldest first, the first clip dropped.
        let dirs: Vec<String> = (2..=4)
            .map(|n| {
                state
                    .clips
                    .file(&key(n).shot_id, "camera.0", "avi")
                    .unwrap()
                    .parent()
                    .unwrap()
                    .display()
                    .to_string()
            })
            .collect();
        assert_eq!(waiting, dirs);
        // The bound follows the clips kept: 20 by default, one camera.
        assert_eq!(bound(&state), 20);
    }

    #[test]
    fn the_impact_line_gives_the_refined_shift_per_shot_and_camera() {
        let (dir, state) = state();
        let mut job = clip(&state, &dir, 3);
        job.timing = ClipTiming {
            device: "garmin_r10".into(),
            impact_lead_ms: 2000,
            latency_ms: 150,
        };
        let written = |found, shift| SwingWritten {
            path: job.avi.with_extension("swing.json"),
            swing_found: found,
            impact_shift_ns: shift,
        };
        assert_eq!(impact_line(&job, &written(true, Some(-41_600_000))), None);
        job.impact_ns = Some(1_500_000_000);
        assert_eq!(
            impact_line(&job, &written(true, Some(-41_600_000))).as_deref(),
            Some(
                "camera.0: shot #3 (shot-3): swing impact -42 ms from the clip's \
                 (garmin_r10, camera timing 2000 ms, latency 150 ms)"
            )
        );
        assert_eq!(
            impact_line(&job, &written(false, None)).as_deref(),
            Some(
                "camera.0: shot #3 (shot-3): no swing found near the clip's impact \
                 (garmin_r10, camera timing 2000 ms, latency 150 ms)"
            )
        );
        assert_eq!(impact_line(&job, &written(true, None)), None, "estimated");
    }

    #[test]
    fn the_cameras_rotation_reaches_the_pass_and_is_recorded() {
        let (dir, state) = state();
        state.system.update(|c| {
            c.camera.insert(
                "0".into(),
                flighthook::CameraSection {
                    camera: flighthook::CameraSection::TEST_PATTERN.into(),
                    rotation_deg: 90,
                    ..flighthook::CameraSection::default()
                },
            );
        });
        let mut job = clip(&state, &dir, 1);
        let (tx, mut rx) = broadcast::channel(16);
        job.bus = tx;
        let seen = Mutex::new(None);
        assert!(process(&state, &job, PoseModel::RtmposeM, &|_, config| {
            *seen.lock().unwrap() = Some(config.rotation);
            Ok(track(3))
        }));
        assert_eq!(*seen.lock().unwrap(), Some(Rotation::Cw90));
        let video = &state.clips.videos("shot-1")[0];
        assert_eq!(video.rotation, 90);
        assert_eq!(video.analyzed_with.as_deref(), Some(stance::VERSION));
        let msg = rx.try_recv().expect("shot_pose");
        assert!(matches!(
            msg.as_local(),
            Some((_, crate::bus::LocalEvent::ShotPose { rotation: 90, .. }))
        ));
        // A rotation that is not one of the four is no rotation.
        state
            .system
            .update(|c| c.camera.get_mut("0").unwrap().rotation_deg = 45);
        assert!(process(&state, &job, PoseModel::RtmposeM, &|_, config| {
            *seen.lock().unwrap() = Some(config.rotation);
            Ok(track(3))
        }));
        assert_eq!(*seen.lock().unwrap(), Some(Rotation::None));
        assert_eq!(state.clips.videos("shot-1")[0].rotation, 0);
    }

    #[test]
    fn clip_config_carries_the_rotation() {
        for rotation in [
            Rotation::None,
            Rotation::Cw90,
            Rotation::Half,
            Rotation::Ccw90,
        ] {
            let config = clip_config(PoseModel::RtmposeS, rotation);
            assert_eq!(config.rotation, rotation);
            assert_eq!(config.threads, clip_threads());
        }
    }

    #[test]
    fn the_timing_line_names_the_camera_shot_frames_time_and_threads() {
        let line = timing_line(
            "camera.0",
            "4f2c9e1a-77d0-4c43-9b54-2e1f0d6c8a11",
            240,
            Duration::from_millis(2304),
            12,
            238,
        );
        assert_eq!(
            line,
            "body tracking: camera.0 shot 4f2c9e1a-77d0-4c43-9b54-2e1f0d6c8a11: 240 frames in 2.3 s (12 threads) · 9.6 ms a frame, golfer on 238 frames"
        );
        // The prefix A5 asks for, whatever follows it.
        let prefix = regex_lite_prefix(&line);
        assert_eq!(
            prefix,
            Some((
                "camera.0".to_owned(),
                "4f2c9e1a-77d0-4c43-9b54-2e1f0d6c8a11".to_owned(),
                240,
                "2.3".to_owned(),
                12
            ))
        );
        // No frames: no division by zero.
        let empty = timing_line("camera.1", "s", 0, Duration::from_millis(40), 1, 0);
        assert!(
            empty.starts_with("body tracking: camera.1 shot s: 0 frames in 0.0 s (1 threads)"),
            "{empty}"
        );
    }

    /// `body tracking: <camera> shot <id>: <frames> frames in <s> s
    /// (<threads> threads)` read back from the start of `line`.
    fn regex_lite_prefix(line: &str) -> Option<(String, String, usize, String, usize)> {
        let rest = line.strip_prefix("body tracking: ")?;
        let (camera, rest) = rest.split_once(" shot ")?;
        let (id, rest) = rest.split_once(": ")?;
        let (frames, rest) = rest.split_once(" frames in ")?;
        let (secs, rest) = rest.split_once(" s (")?;
        let (threads, _) = rest.split_once(" threads)")?;
        Some((
            camera.to_owned(),
            id.to_owned(),
            frames.parse().ok()?,
            secs.to_owned(),
            threads.parse().ok()?,
        ))
    }

    #[test]
    fn a_tracked_clip_gets_its_pose_file_and_one_shot_pose_event() {
        let (dir, state) = state();
        let mut job = clip(&state, &dir, 1);
        let (tx, mut rx) = broadcast::channel(16);
        job.bus = tx;
        let seen = Mutex::new(None);
        let kept = process(&state, &job, PoseModel::RtmposeM, &|avi, config| {
            *seen.lock().unwrap() = Some((
                avi.to_owned(),
                config.threads,
                config.on_worker_start.is_some(),
            ));
            Ok(track(3))
        });
        assert!(kept);
        let (avi, threads, hooked) = seen.lock().unwrap().clone().unwrap();
        assert_eq!(avi, job.avi);
        assert_eq!(threads, clip_threads());
        assert!(threads >= 1);
        assert!(hooked, "the workers lower their priority");
        let pose = job.slot.dir.join("camera.0.pose.json");
        let written = PoseTrack::read(&pose).expect("pose file");
        assert_eq!(written.frames.len(), 3);
        assert_eq!(
            state.clips.file("shot-1", "camera.0", "pose.json"),
            Some(pose)
        );
        let msg = rx.try_recv().expect("shot_pose");
        assert!(
            matches!(msg.as_local(), Some(("camera.0", crate::bus::LocalEvent::ShotPose { keys, .. })) if keys == &[key(1)])
        );
        assert!(rx.try_recv().is_err(), "one event");
    }

    #[test]
    fn the_swing_file_follows_the_pose_file_with_the_shots_hand_impact_and_height() {
        use stance::golf::{Handed, SwingReport, View};
        // The height of the clip's golfer (the turn's at the trigger, here
        // A's linked game id) is the one used, not the golfer up's (B).
        let (dir, state) = state_with(
            "selected_golfer = \"flighthook.b\"\n\
             [[golfers]]\nidentifiers = [\"flighthook.a\", \"pureplay.local-2\"]\nname = \"A\"\nheight = 175\n\
             [[golfers]]\nidentifiers = [\"flighthook.b\"]\nname = \"B\"\nheight = 160\n",
        );
        assert_eq!(state.golfer_up_facts().height, Some(160.0));
        let mut job = clip(&state, &dir, 1);
        job.golfer = Some("pureplay.local-2".into());
        let (tx, mut rx) = broadcast::channel(16);
        job.bus = tx;
        job.position = CameraPosition::TargetLine;
        job.hand = Some(Handedness::Left);
        job.impact_ns = Some(93 * 16_666_667);
        let kept = process(&state, &job, PoseModel::RtmposeM, &|_, _| {
            Ok(crate::pose::swing::tests::swing_track())
        });
        assert!(kept);
        // shot_pose first, then shot_swing, one each.
        let first = rx.try_recv().expect("shot_pose");
        assert!(matches!(
            first.as_local(),
            Some((_, crate::bus::LocalEvent::ShotPose { .. }))
        ));
        let second = rx.try_recv().expect("shot_swing");
        assert!(
            matches!(second.as_local(), Some(("camera.0", crate::bus::LocalEvent::ShotSwing { keys })) if keys == &[key(1)])
        );
        assert!(rx.try_recv().is_err());
        let path = job.slot.dir.join("camera.0.swing.json");
        assert_eq!(
            state.clips.file("shot-1", "camera.0", "swing.json"),
            Some(path.clone())
        );
        assert!(state.clips.videos("shot-1")[0].swing);
        let report = SwingReport::read(&path).expect("swing file");
        assert_eq!(report.view, Some(View::DownTheLine));
        assert_eq!(report.handed, Handed::Left);
        assert_eq!(report.height_cm, Some(175.0));
        // The clip's impact is stance's prior, refined against the hands.
        let impact = report.events.impact.expect("impact").t_ns;
        assert!(impact.abs_diff(job.impact_ns.unwrap()) <= 300_000_000);
        // A camera behind the golfer: keypoints only.
        let mut behind = clip(&state, &dir, 2);
        behind.position = CameraPosition::GolferBack;
        assert!(process(&state, &behind, PoseModel::RtmposeM, &|_, _| {
            Ok(crate::pose::swing::tests::swing_track())
        }));
        assert!(behind.slot.dir.join("camera.0.pose.json").exists());
        assert!(!behind.slot.dir.join("camera.0.swing.json").exists());
        assert!(
            state
                .clips
                .file("shot-2", "camera.0", "swing.json")
                .is_none()
        );
    }

    #[test]
    fn a_clip_cleared_or_without_models_or_with_tracking_off_is_skipped() {
        let (dir, state) = state();
        // No models in the test folder: the load fails, the clip is skipped.
        let job = clip(&state, &dir, 1);
        assert!(!run(&state, &job));
        assert!(!job.slot.dir.join("camera.0.pose.json").exists());
        // Cleared before its turn.
        let job = clip(&state, &dir, 2);
        state.clips.clear();
        let m = PoseModel::RtmposeM;
        assert!(!process(&state, &job, m, &|_, _| panic!("cleared")));
        // Cleared while it ran: the file it wrote goes too.
        let job = clip(&state, &dir, 3);
        let kept = process(&state, &job, m, &|_, _| {
            state.clips.clear();
            Ok(track(1))
        });
        assert!(!kept);
        assert!(!job.slot.dir.join("camera.0.pose.json").exists());
        // Body tracking off: nothing runs.
        set_mode(&state, BodyTrackingMode::Off);
        let job = clip(&state, &dir, 4);
        assert!(!run(&state, &job));
    }

    static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn counting_pass(_: &std::path::Path, _: &PoseConfig) -> stance::Result<PoseTrack> {
        RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(track(2))
    }

    #[test]
    fn a_recorded_clip_runs_only_in_live_and_a_viewed_one_in_any_mode_but_off() {
        let (dir, state) = state();
        state.pose.clips.fake(counting_pass);
        for (mode, recorded, viewed) in [
            (BodyTrackingMode::Live, true, true),
            (BodyTrackingMode::Historical, false, true),
            (BodyTrackingMode::Off, false, false),
        ] {
            set_mode(&state, mode);
            let job = clip(&state, &dir, 1);
            assert_eq!(run(&state, &job), recorded, "{mode:?} recorded");
            let mut job = clip(&state, &dir, 2);
            job.on_demand = true;
            assert_eq!(run(&state, &job), viewed, "{mode:?} viewed");
        }
    }

    #[test]
    fn a_viewed_clip_starts_at_once() {
        let (dir, state) = state();
        set_mode(&state, BodyTrackingMode::Historical);
        state.pose.clips.fake(counting_pass);
        let mut job = clip(&state, &dir, 1);
        job.written = Instant::now();
        job.on_demand = true;
        let started = Instant::now();
        assert!(run(&state, &job));
        assert!(
            started.elapsed() < CLIP_DELAY / 2,
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_clip_is_queued_once_and_a_viewed_one_goes_first() {
        let (dir, state) = state();
        let queue = ClipQueue::default();
        // Hold the worker off, as if it were busy with a clip.
        queue.inner.lock().unwrap().working = true;
        let recorded: Vec<ClipJob> = (1..=3).map(|n| clip(&state, &dir, n)).collect();
        let avis: Vec<PathBuf> = recorded.iter().map(|j| j.avi.clone()).collect();
        for job in recorded {
            assert_eq!(queue.push(&state, job, 8), Pushed::Queued);
        }
        assert_eq!(queue.waiting(), avis);
        // The same clip again: nothing changes.
        let again = |n: u32, on_demand: bool| {
            let found = state
                .clips
                .camera_clip(&key(n).shot_id, "camera.0")
                .expect("clip");
            let mut job = clip(&state, &dir, 9);
            job.slot = found.slot;
            job.avi = found.avi;
            job.on_demand = on_demand;
            job
        };
        assert_eq!(queue.push(&state, again(2, false), 8), Pushed::Already);
        assert_eq!(queue.waiting(), avis);
        // Viewed while it waits: it keeps its place and its delay.
        assert_eq!(queue.push(&state, again(3, true), 8), Pushed::Already);
        assert_eq!(queue.waiting(), avis);
        assert!(!queue.inner.lock().unwrap().jobs[2].job.on_demand);
        // A new viewed clip goes first too.
        let mut viewed = clip(&state, &dir, 4);
        viewed.on_demand = true;
        let viewed_avi = viewed.avi.clone();
        assert_eq!(queue.push(&state, viewed, 8), Pushed::Queued);
        assert_eq!(queue.waiting()[0], viewed_avi);
        assert!(
            queue.has(
                &state
                    .clips
                    .camera_clip("shot-4", "camera.0")
                    .unwrap()
                    .slot
                    .id,
                "camera.0"
            )
        );
        // Full: the clip queued longest ago goes, wherever it waits.
        let more = clip(&state, &dir, 5);
        assert_eq!(queue.push(&state, more, 4), Pushed::Queued);
        let waiting = queue.waiting();
        assert_eq!(waiting.len(), 4);
        assert!(!waiting.contains(&avis[0]), "{waiting:?}");
        // The clip being tracked is not queued again.
        queue.inner.lock().unwrap().running = Some((
            state
                .clips
                .camera_clip("shot-2", "camera.0")
                .unwrap()
                .slot
                .id,
            "camera.0".into(),
        ));
        assert_eq!(queue.push(&state, again(2, true), 8), Pushed::Already);
        // Off: every waiting clip dropped.
        assert_eq!(queue.clear(), 4);
        assert!(queue.waiting().is_empty());
    }

    #[test]
    fn a_viewed_clip_is_requested_once_and_tracked_with_its_events() {
        let (dir, state) = state();
        set_mode(&state, BodyTrackingMode::Historical);
        state.pose.clips.fake(counting_pass);
        let job = clip(&state, &dir, 1);
        let (tx, mut rx) = broadcast::channel(16);
        assert_eq!(
            request(&state, "shot-9", "camera.0", false, &tx),
            Requested::NoClip
        );
        assert_eq!(
            request(&state, "shot-1", "camera.1", false, &tx),
            Requested::NoClip
        );
        set_mode(&state, BodyTrackingMode::Off);
        assert_eq!(
            request(&state, "shot-1", "camera.0", false, &tx),
            Requested::Off
        );
        assert!(!state.pose.clips.working());
        set_mode(&state, BodyTrackingMode::Historical);
        // Hold the worker off to see the queue.
        state.pose.clips.inner.lock().unwrap().working = true;
        assert_eq!(
            request(&state, "shot-1", "camera.0", false, &tx),
            Requested::Queued
        );
        assert_eq!(
            request(&state, "shot-1", "camera.0", false, &tx),
            Requested::Queued
        );
        assert_eq!(state.pose.clips.waiting(), std::slice::from_ref(&job.avi));
        // Let it run.
        let queued = state
            .pose
            .clips
            .inner
            .lock()
            .unwrap()
            .jobs
            .pop_front()
            .unwrap();
        state.pose.clips.inner.lock().unwrap().working = false;
        assert!(queued.job.on_demand);
        state.pose.clips.push(&state, queued.job, 8);
        let first = rx.blocking_recv().expect("shot_pose");
        assert!(
            matches!(first.as_local(), Some(("camera.0", crate::bus::LocalEvent::ShotPose { keys, .. })) if keys == &[key(1)])
        );
        assert!(job.slot.dir.join("camera.0.pose.json").exists());
        assert_eq!(
            request(&state, "shot-1", "camera.0", false, &tx),
            Requested::Done
        );
    }

    #[test]
    fn a_forced_request_clears_a_done_clip_and_queues_it_again() {
        let (dir, state) = state();
        set_mode(&state, BodyTrackingMode::Historical);
        let job = clip(&state, &dir, 1);
        let (tx, _rx) = broadcast::channel(16);
        assert!(process(&state, &job, PoseModel::RtmposeM, &|_, _| Ok(
            crate::pose::swing::tests::swing_track()
        )));
        let pose = job.slot.dir.join("camera.0.pose.json");
        assert!(pose.exists());
        assert!(state.clips.videos("shot-1")[0].pose);
        assert_eq!(
            request(&state, "shot-1", "camera.0", false, &tx),
            Requested::Done
        );
        // Off: nothing is cleared.
        set_mode(&state, BodyTrackingMode::Off);
        assert_eq!(
            request(&state, "shot-1", "camera.0", true, &tx),
            Requested::Off
        );
        assert!(pose.exists());
        set_mode(&state, BodyTrackingMode::Historical);
        // Hold the worker off to see the queue.
        state.pose.clips.inner.lock().unwrap().working = true;
        assert_eq!(
            request(&state, "shot-1", "camera.0", true, &tx),
            Requested::Queued
        );
        assert!(!pose.exists(), "the old keypoints are gone");
        assert!(!job.slot.dir.join("camera.0.swing.json").exists());
        let video = &state.clips.videos("shot-1")[0];
        assert!(!video.pose && !video.swing);
        assert_eq!(state.pose.clips.waiting(), std::slice::from_ref(&job.avi));
        // Asked again while it waits: still queued once.
        assert_eq!(
            request(&state, "shot-1", "camera.0", true, &tx),
            Requested::Queued
        );
        assert_eq!(state.pose.clips.waiting().len(), 1);
    }

    #[test]
    fn a_clip_waits_its_turn_after_it_was_written() {
        let (dir, state) = state();
        static STARTED: Mutex<Option<Instant>> = Mutex::new(None);
        fn pass(_: &std::path::Path, _: &PoseConfig) -> stance::Result<PoseTrack> {
            *STARTED.lock().unwrap() = Some(Instant::now());
            Ok(track(2))
        }
        state.pose.clips.fake(pass);
        let mut job = clip(&state, &dir, 1);
        job.written = Instant::now() - CLIP_DELAY + Duration::from_millis(300);
        assert!(run(&state, &job));
        let started = STARTED.lock().unwrap().expect("ran");
        assert!(started >= job.written + CLIP_DELAY);
        // Cleared while it waits: skipped without running.
        let mut job = clip(&state, &dir, 2);
        job.written = Instant::now();
        let cleared = Instant::now();
        state.clips.clear();
        *STARTED.lock().unwrap() = None;
        assert!(!run(&state, &job));
        assert!(cleared.elapsed() < CLIP_DELAY);
        assert!(STARTED.lock().unwrap().is_none());
    }

    #[test]
    fn a_pass_that_panics_is_logged_and_the_queue_goes_on() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (dir, state) = state();
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn pass(_: &std::path::Path, _: &PoseConfig) -> stance::Result<PoseTrack> {
            assert!(
                CALLS.fetch_add(1, Ordering::SeqCst) > 0,
                "a broken first clip"
            );
            Ok(track(2))
        }
        state.pose.clips.fake(pass);
        let first = clip(&state, &dir, 1);
        let second = clip(&state, &dir, 2);
        let pose = second.slot.dir.join("camera.0.pose.json");
        state.pose.clips.push(&state, first, 8);
        state.pose.clips.push(&state, second, 8);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pose.exists() {
            assert!(Instant::now() < deadline, "the second clip was not tracked");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        // The worker ends with the queue, and the next clip starts a new one.
        while state.pose.clips.inner.lock().unwrap().working {
            assert!(Instant::now() < deadline, "the worker did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
        let third = clip(&state, &dir, 3);
        let pose = third.slot.dir.join("camera.0.pose.json");
        state.pose.clips.push(&state, third, 8);
        while !pose.exists() {
            assert!(Instant::now() < deadline, "the third clip was not tracked");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_clip_runs_on_a_quarter_of_the_cpus() {
        let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        assert_eq!(clip_threads(), (cpus / 4).max(1));
    }

    /// A detector that finds one person filling the frame and records the
    /// priority of the thread each call runs on.
    #[cfg(any(target_os = "linux", windows))]
    struct PriorityDetector(Arc<Mutex<Vec<i32>>>);

    #[cfg(any(target_os = "linux", windows))]
    impl stance::pose::PersonDetector for PriorityDetector {
        fn model_name(&self) -> &'static str {
            "priority"
        }
        fn detect(
            &mut self,
            image: &stance::pose::Image,
        ) -> stance::Result<Vec<stance::pose::Detection>> {
            self.0
                .lock()
                .unwrap()
                .push(super::super::priority::current());
            #[allow(clippy::cast_precision_loss)]
            let (w, h) = (image.width() as f32, image.height() as f32);
            Ok(vec![stance::pose::Detection {
                bbox: stance::pose::BBox::new(0.0, 0.0, w, h),
                score: 0.9,
            }])
        }
        fn fork(&self) -> stance::Result<Self> {
            Ok(Self(Arc::clone(&self.0)))
        }
    }

    /// stance's workers for a clip run below normal even when the thread
    /// that starts the pass does not.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn the_clip_pass_workers_run_below_normal() {
        use super::super::live::tests::{FakePose, numbered_jpeg};
        use super::super::priority::{current, lowered_from};
        let base = current();
        let seen: Arc<Mutex<Vec<i32>>> = Arc::default();
        let models = stance::pose::PoseModels::new(
            FakePose {
                delay: Duration::ZERO,
                calls: Arc::default(),
                scores: VecDeque::new(),
            },
            PriorityDetector(Arc::clone(&seen)),
        );
        let frames: Vec<(u64, Arc<[u8]>)> = (0..30u8)
            .map(|n| (u64::from(n) * 1_000_000, numbered_jpeg(n)))
            .collect();
        let config = PoseConfig {
            threads: 2,
            ..clip_config(PoseModel::RtmposeM, Rotation::None)
        };
        let caller = std::thread::spawn(move || {
            stance::pose::analyze_frames(&models, &frames, &config).expect("pass");
            current()
        })
        .join()
        .expect("pass thread");
        let seen = seen.lock().unwrap().clone();
        // Key frames 0, 10, 20 and the last.
        assert_eq!(seen.len(), 4);
        assert!(seen.iter().all(|&p| p == lowered_from(base)), "{seen:?}");
        assert_eq!(caller, base, "the calling thread is left alone");
    }

    /// The real pass on a stance clip (`STANCE_TEST_CLIP`, an `.avi` with
    /// its sidecar) with the models in `STANCE_MODEL_DIR`, at the clip
    /// pass's threads and priority; prints its wall time. Skipped without
    /// them.
    #[test]
    fn the_real_pass_tracks_a_clip_when_there_is_one() {
        use crate::pose::models::{Models, Official};
        let (Some(models), Some(avi)) = (
            std::env::var_os("STANCE_MODEL_DIR"),
            std::env::var_os("STANCE_TEST_CLIP"),
        ) else {
            eprintln!("skipped: STANCE_MODEL_DIR or STANCE_TEST_CLIP is not set");
            return;
        };
        let avi = PathBuf::from(avi);
        let models = Models::new(PathBuf::from(models), Arc::new(Official::default()));
        let model = PoseModel::RtmposeM;
        let deadline = Instant::now() + Duration::from_secs(600);
        let shared = loop {
            match models.want(model) {
                ModelState::Ready(shared) => break shared,
                ModelState::Failed(e) => panic!("{e}"),
                _ => {
                    assert!(Instant::now() < deadline, "the models did not load");
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };
        let forked = shared.fork().expect("fork");
        let config = clip_config(model, Rotation::None);
        let (took, frames) = std::thread::spawn(move || {
            super::super::priority::lower_this_thread();
            let started = Instant::now();
            let track = stance::pose::analyze_clip(&forked, &avi, &config).expect("pass");
            (started.elapsed(), track.frames.len())
        })
        .join()
        .expect("pass thread");
        assert!(frames > 0);
        eprintln!(
            "clip pass: {frames} frames in {:.2} s on {} threads",
            took.as_secs_f64(),
            clip_threads()
        );
    }
}
