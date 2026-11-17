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
//! - Historical: a clip the first time it is viewed ([`Cameras::request`]),
//!   at once. A clip already tracked, queued or being tracked is not queued
//!   again.
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
//! pose file is written the clip store marks it and one [`Event::Pose`]
//! tells the host; when its swing file is, one [`Event::Swing`]. The
//! keypoints and numbers stay in the files.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flighthook::{BodyTrackingMode, CameraPosition, Handedness, PoseModel, ShotKey};
use stance::pose::{PoseConfig, PoseTrack, Rotation};
use tracing::{debug, info, warn};

use super::models::{ModelState, pose_config};
use super::swing::{SwingInput, SwingWritten, write_report};
use crate::clips::ClipSlot;
use crate::{Cameras, Event, Out, settings};

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
    /// The camera that recorded it (`camera.0`).
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
    /// A viewer asked for it ([`Cameras::request`]): its pass starts at
    /// once, and in any mode but Off.
    pub on_demand: bool,
    /// How the estimate was made, for the impact log line.
    pub timing: ClipTiming,
    /// Where its [`Event::Pose`] and [`Event::Swing`] go, as the camera's.
    pub out: Out,
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
    #[cfg(any(test, feature = "testing"))]
    fake: Mutex<Option<FakePass>>,
}

/// A clip pass without models, for tests.
#[cfg(any(test, feature = "testing"))]
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
pub fn bound(cameras: &Cameras) -> usize {
    let config = cameras.settings.read();
    let keep = usize::try_from(crate::clips::keep(&config.replay)).unwrap_or(1);
    keep.saturating_mul(config.camera.len().max(1))
}

impl ClipQueue {
    /// Queue `job`, starting the worker when it is not running: a viewed
    /// clip (`on_demand`) first, a recorded one last. With `bound` clips
    /// already waiting, the one queued longest ago is dropped first. A clip
    /// already waiting or being tracked is not queued twice.
    pub fn push(&self, cameras: &Arc<Cameras>, job: ClipJob, bound: usize) -> Pushed {
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
        let cameras = Arc::clone(cameras);
        let spawned = std::thread::Builder::new()
            .name("pose:clips".to_owned())
            .spawn(move || work(&cameras, &queue));
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
    #[cfg(any(test, feature = "testing"))]
    pub fn fake(&self, pass: FakePass) {
        *self.fake.lock().unwrap_or_else(|e| e.into_inner()) = Some(pass);
    }

    /// Hold the worker off, as if it were busy with a clip (tests): what
    /// is pushed from now on waits and starts no thread.
    #[cfg(test)]
    pub fn hold(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).working = true;
    }

    /// Clips waiting, in the order they are taken, by their AVI (tests).
    #[cfg(any(test, feature = "testing"))]
    pub fn waiting(&self) -> Vec<PathBuf> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.jobs.iter().map(|q| q.job.avi.clone()).collect()
    }

    /// Whether the worker thread is running (tests).
    #[cfg(any(test, feature = "testing"))]
    pub fn working(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).working
    }
}

/// What [`Cameras::request`] did.
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

impl Cameras {
    /// Track camera `camera`'s clip of shot `shot_id` now, for a viewer:
    /// queued first ([`ClipQueue::push`]), its pass starting at once, unless
    /// its keypoints are there already, it is already queued, or body
    /// tracking is off. With `force`, a clip tracked already has its body
    /// tracking and swing analysis deleted
    /// ([`ClipStore::clear_analysis`](crate::clips::ClipStore::clear_analysis))
    /// and is tracked again. The models load with the first such clip;
    /// while their files are not downloaded, the clip is left untracked.
    /// Its [`Event::Pose`] and [`Event::Swing`] go to `out`.
    pub fn request(
        self: &Arc<Self>,
        shot_id: &str,
        camera: &str,
        force: bool,
        out: Out,
    ) -> Requested {
        let Some(clip) = self.clips.camera_clip(shot_id, camera) else {
            return Requested::NoClip;
        };
        if !self.settings.body_tracking().mode.is_on() {
            return Requested::Off;
        }
        if force {
            self.clips.clear_analysis(&clip.slot, camera);
        } else if clip.video.pose {
            return Requested::Done;
        }
        if self.pose.clips.has(&clip.slot.id, camera) {
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
        let latency = settings::latency(&self.settings.read(), camera);
        let job = ClipJob {
            avi: clip.avi,
            slot: clip.slot,
            camera: camera.to_owned(),
            keys: clip.shots.iter().map(|s| s.key.clone()).collect(),
            position: clip.video.position,
            hand: first.and_then(|s| s.hand),
            golfer: first.and_then(|s| s.golfer.clone()),
            impact_ns: sidecar.as_ref().and_then(crate::clips::clip_impact),
            written: Instant::now(),
            on_demand: true,
            timing: ClipTiming {
                device: first.map(|s| s.device.clone()).unwrap_or_default(),
                impact_lead_ms: first.map_or(0, |s| s.impact_lead_ms),
                latency_ms: u64::try_from(latency.as_millis()).unwrap_or(u64::MAX),
            },
            out,
        };
        self.pose.clips.push(self, job, bound(self));
        Requested::Queued
    }
}

/// Take the jobs one at a time until none is left, at below-normal
/// priority. A job that panics is logged and the next one taken.
fn work(cameras: &Cameras, queue: &Mutex<Inner>) {
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
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(cameras, &job)));
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

/// What a clip does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    /// Its turn comes in this long.
    Wait(Duration),
    /// Its turn has come and it is wanted: track it.
    Run,
    /// Its turn has come and it is no longer wanted ([`wanted`]).
    Skip,
}

/// What `job` does at `now` in `mode`: a recorded clip's turn comes
/// [`CLIP_DELAY`] after it was written, a viewed one's at once; then it
/// runs if it is still [`wanted`].
fn turn(job: &ClipJob, mode: BodyTrackingMode, now: Instant) -> Turn {
    let due = if job.on_demand {
        job.written
    } else {
        job.written + CLIP_DELAY
    };
    if now < due {
        Turn::Wait(due - now)
    } else if wanted(job, mode) {
        Turn::Run
    } else {
        Turn::Skip
    }
}

/// Sleep until `job`'s [`turn`]. True when it runs; false when it is
/// skipped, or pruned or cleared meanwhile.
fn wait_turn(cameras: &Cameras, job: &ClipJob) -> bool {
    wait_turn_on(cameras, job, Instant::now, std::thread::sleep)
}

/// [`wait_turn`] on the clock `now`, sleeping with `sleep`.
fn wait_turn_on(
    cameras: &Cameras,
    job: &ClipJob,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> bool {
    loop {
        if !cameras.clips.is_current(&job.slot) {
            return false;
        }
        match turn(job, cameras.settings.body_tracking().mode, now()) {
            Turn::Wait(left) => sleep(left.min(MODEL_POLL)),
            Turn::Run => return true,
            Turn::Skip => return false,
        }
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
fn run(cameras: &Cameras, job: &ClipJob) -> bool {
    if !wait_turn(cameras, job) {
        return false;
    }
    let settings = cameras.settings.body_tracking();
    #[cfg(any(test, feature = "testing"))]
    if let Some(pass) = *cameras
        .pose
        .clips
        .fake
        .lock()
        .unwrap_or_else(|e| e.into_inner())
    {
        return process(cameras, job, settings.model, &pass);
    }
    let models = loop {
        if !cameras.clips.is_current(&job.slot)
            || !wanted(job, cameras.settings.body_tracking().mode)
        {
            return false;
        }
        match cameras.pose.models.want(settings.model) {
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
    process(cameras, job, settings.model, &|avi, config| {
        stance::pose::analyze_clip(&models, avi, config)
    })
}

/// The clip pass: stance's `analyze_clip` on loaded models, or a stand-in
/// in tests.
type Analyze<'a> = &'a dyn Fn(&std::path::Path, &PoseConfig) -> stance::Result<PoseTrack>;

/// Track one clip with `model` and store its keypoints, then announce
/// them. Returns whether the pose file is kept.
fn process(cameras: &Cameras, job: &ClipJob, model: PoseModel, analyze: Analyze<'_>) -> bool {
    let camera = &job.camera;
    if !cameras.clips.is_current(&job.slot) {
        return false;
    }
    let rotation = settings::camera_rotation(&cameras.settings.read(), camera);
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
            if cameras.clips.is_current(&job.slot) {
                warn!("{camera}: writing the body tracking of a clip: {e}");
            }
            return false;
        }
    };
    if !cameras.clips.set_pose(&job.slot, camera, rotation) {
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
    (job.out)(Event::Pose {
        keys: job.keys.clone(),
        rotation,
    });
    swing(cameras, job, &track);
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

/// The facts of a clip's golfer (`golfer`, recorded at the trigger), by the
/// lookup rule, for its swing report and its grades alike: a clip with no
/// golfer has no height and the default sex, never the golfer up's.
pub fn clip_golfer_facts(cameras: &Cameras, golfer: Option<&str>) -> flighthook::PlayerSection {
    settings::golfer_facts(&cameras.settings.read(), golfer)
}

/// Analyze the swing in `track`, `job`'s keypoints, store it beside the
/// clip and announce it. Returns whether the swing file is kept.
fn swing(cameras: &Cameras, job: &ClipJob, track: &PoseTrack) -> bool {
    let camera = &job.camera;
    let input = SwingInput {
        position: job.position,
        hand: job.hand,
        impact_ns: job.impact_ns,
        height_cm: clip_golfer_facts(cameras, job.golfer.as_deref()).height,
        mirrored: super::swing::mirrored(camera),
    };
    let written = match write_report(camera, track, &job.avi, &input) {
        Ok(Some(written)) => written,
        Ok(None) => return false,
        Err(e) => {
            if cameras.clips.is_current(&job.slot) {
                info!("{camera}: no swing analysis of {}: {e}", job.avi.display());
            }
            return false;
        }
    };
    if !cameras.clips.set_swing(&job.slot, camera) {
        let _ = std::fs::remove_file(&written.path);
        return false;
    }
    log_impact(job, &written);
    (job.out)(Event::Swing {
        keys: job.keys.clone(),
    });
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
mod tests;
