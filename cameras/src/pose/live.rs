//! A camera's live body tracking (alpha): a worker thread
//! (`pose:camera.N`) that finds the golfer's keypoints on the newest live
//! frame and puts the frame in the camera's live slot together with them.
//!
//! The camera's loop runs a worker only while a viewer watches the camera's
//! live view and body tracking is in Live mode; it stops it when the watch
//! lapses or the mode changes, which forgets the tracked golfer. Frames reach the worker through a
//! one-frame handoff: the camera's loop offers every frame, each replacing
//! the one waiting, and the worker takes the newest when it is free, at most
//! [`LIVE_FPS`] times a second. A frame replaced before the worker took it
//! is skipped: never shown, never queued. So the live view runs at the pose
//! rate, at most [`LIVE_FPS`], and each frame carries the pose found on it.
//! While the models are not ready, frames go out the same way without a
//! pose.
//!
//! Smoothing (the `smoothing` setting, a One Euro filter per keypoint
//! coordinate) is applied here, to the live output only; a frame dated no
//! later than the one before (the camera's latency was raised) starts it
//! afresh. A face-on or
//! down-the-line camera's frames also carry the golfer's posture for that
//! view ([`super::swing::LivePostureFilter`]), smoothed at the same cutoff.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use flighthook::{BodyTrackingSection, LivePose, LivePosture, PoseModel};
use stance::pose::{
    Image, OneEuro, OneEuroParams, PersonDetector, Pose, PoseEngine, PoseTracker, Rotation,
    decode_jpeg,
};
use tracing::warn;

use super::swing::{LivePostureFilter, LiveSetup};

/// Most frames a second put in a camera's live slot while it is watched;
/// the frames in between are left out.
pub const LIVE_FPS: u32 = 15;

/// Least time from the start of one pose to the start of the next.
const LIVE_EVERY: Duration = Duration::from_nanos(1_000_000_000 / LIVE_FPS as u64);

/// How often a waiting worker looks at its stop flag.
const STOP_POLL: Duration = Duration::from_millis(100);

/// Least time between two attempts to make an estimator while the models
/// are not ready.
const BUILD_RETRY: Duration = Duration::from_millis(500);

/// Finds the golfer on one frame after another.
pub trait Estimate: Send {
    /// The golfer's keypoints on `image`, taken at `t_ns`; `None` when
    /// nobody is found.
    ///
    /// # Errors
    ///
    /// When inference fails.
    fn estimate(&mut self, image: &Image, t_ns: u64) -> stance::Result<Option<Pose>>;
}

impl<P: PoseEngine, D: PersonDetector> Estimate for PoseTracker<P, D> {
    fn estimate(&mut self, image: &Image, t_ns: u64) -> stance::Result<Option<Pose>> {
        self.track(image, t_ns)
    }
}

/// Makes an estimator for the settings and the camera's rotation (the
/// estimator turns each frame by it before looking for the golfer), `None`
/// while the models are not ready.
pub type Build = Box<dyn FnMut(&BodyTrackingSection, Rotation) -> Option<Box<dyn Estimate>> + Send>;

/// Puts a frame (its time and JPEG) in the live slot with its pose, as the
/// `X-Pose` JSON, or none.
pub type Publish = Box<dyn FnMut(u64, Arc<[u8]>, Option<Arc<str>>) + Send>;

/// What a live view follows, read for every frame.
#[derive(Debug, Clone)]
pub struct LiveSettings {
    /// Body tracking's settings.
    pub body: BodyTrackingSection,
    /// The camera's view and the player's hand for the posture; `None`
    /// for a camera without a view (behind or above the golfer).
    pub posture: Option<LiveSetup>,
    /// The camera's rotation: frames are turned by it before body
    /// tracking, so the keypoints and the posture are in the upright frame.
    pub rotation: Rotation,
}

/// The settings in effect, read for every frame.
pub type Settings = Box<dyn Fn() -> LiveSettings + Send>;

/// A frame offered to the worker.
struct Offer {
    time_ns: u64,
    jpeg: Arc<[u8]>,
}

#[derive(Default)]
struct Handoff {
    /// The newest frame not taken yet.
    slot: Mutex<Option<Offer>>,
    offered: Condvar,
    stop: AtomicBool,
}

/// One camera's live body tracking worker. Dropping it stops the thread
/// without waiting for it: it ends on its own within one pose, and puts
/// nothing more in the live slot.
pub struct LiveWorker {
    handoff: Arc<Handoff>,
    thread: Option<JoinHandle<()>>,
}

impl LiveWorker {
    /// Start camera `actor`'s worker.
    ///
    /// # Errors
    ///
    /// When the thread can't be started.
    pub fn start(
        actor: &str,
        settings: Settings,
        build: Build,
        publish: Publish,
    ) -> std::io::Result<Self> {
        Self::start_on(actor, settings, build, publish, Instant::now)
    }

    /// [`LiveWorker::start`], pacing frames on the clock `now`.
    fn start_on(
        actor: &str,
        settings: Settings,
        build: Build,
        publish: Publish,
        now: impl Fn() -> Instant + Send + 'static,
    ) -> std::io::Result<Self> {
        let handoff = Arc::new(Handoff::default());
        let shared = Arc::clone(&handoff);
        let name = actor.to_owned();
        let thread = std::thread::Builder::new()
            .name(format!("pose:{actor}"))
            .spawn(move || run(&name, &shared, &settings, build, publish, &now))?;
        Ok(Self {
            handoff,
            thread: Some(thread),
        })
    }

    /// Offer a frame, taken at `time_ns`: it replaces a frame still waiting,
    /// which is skipped. Returns whether one was.
    pub fn offer(&self, time_ns: u64, jpeg: Arc<[u8]>) -> bool {
        let mut slot = self.handoff.slot.lock().unwrap_or_else(|e| e.into_inner());
        let skipped = slot.replace(Offer { time_ns, jpeg }).is_some();
        drop(slot);
        self.handoff.offered.notify_one();
        skipped
    }

    /// Whether its thread still runs. A thread that ended without being
    /// stopped (a panic) takes no more frames.
    pub fn alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for LiveWorker {
    fn drop(&mut self) {
        self.handoff.stop.store(true, Ordering::Release);
        self.handoff.offered.notify_one();
        // A thread already ended is joined to report a panic; a running one
        // is left to end by itself, so the camera's capture never waits on
        // a pose or a model being made.
        if let Some(thread) = self.thread.take()
            && thread.is_finished()
            && thread.join().is_err()
        {
            warn!("a live body tracking worker panicked");
        }
    }
}

/// The newest offered frame, waiting for one; `None` once stopped.
fn take(handoff: &Handoff) -> Option<Offer> {
    let mut slot = handoff.slot.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if handoff.stop.load(Ordering::Acquire) {
            return None;
        }
        if let Some(offer) = slot.take() {
            return Some(offer);
        }
        slot = handoff
            .offered
            .wait_timeout(slot, STOP_POLL)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// Sleep until `until` on the clock `now`, or less once stopped. False
/// when stopped.
fn pace(handoff: &Handoff, until: Instant, now: &dyn Fn() -> Instant) -> bool {
    loop {
        if handoff.stop.load(Ordering::Acquire) {
            return false;
        }
        let now = now();
        if now >= until {
            return true;
        }
        std::thread::sleep((until - now).min(STOP_POLL));
    }
}

/// The estimator in use and the settings it was made for.
struct Current {
    model: PoseModel,
    detect_every: u32,
    rotation: Rotation,
    estimator: Box<dyn Estimate>,
}

/// The width of a `width × height` frame once turned by `rotation`.
fn upright_width(width: u32, height: u32, rotation: Rotation) -> u32 {
    rotation.dimensions(width, height).0
}

fn run(
    actor: &str,
    handoff: &Handoff,
    settings: &Settings,
    mut build: Build,
    mut publish: Publish,
    now: &dyn Fn() -> Instant,
) {
    let mut current: Option<Current> = None;
    let mut build_after: Option<Instant> = None;
    let mut smoother = Smoother::default();
    let mut posture = LivePostureFilter::default();
    let mut last_start: Option<Instant> = None;
    let mut last_time: Option<u64> = None;
    let mut warned = false;
    loop {
        if let Some(start) = last_start
            && !pace(handoff, start + LIVE_EVERY, now)
        {
            return;
        }
        let Some(offer) = take(handoff) else {
            return;
        };
        last_start = Some(now());
        // The filters need time to move forwards.
        if last_time.is_some_and(|t| offer.time_ns <= t) {
            smoother.reset();
            posture.reset();
        }
        last_time = Some(offer.time_ns);
        let LiveSettings {
            body: s,
            posture: setup,
            rotation,
        } = settings();
        let made_for = |c: &Current| {
            c.model == s.model && c.detect_every == s.detect_every && c.rotation == rotation
        };
        if !current.as_ref().is_some_and(made_for) {
            current = None;
            if build_after.is_none_or(|at| Instant::now() >= at) {
                current = build(&s, rotation).map(|estimator| Current {
                    model: s.model,
                    detect_every: s.detect_every,
                    rotation,
                    estimator,
                });
                smoother.reset();
                posture.reset();
                build_after = current.is_none().then(|| Instant::now() + BUILD_RETRY);
            }
        }
        let pose = match current
            .as_mut()
            .filter(|_| s.mode == flighthook::BodyTrackingMode::Live)
        {
            None => None,
            Some(c) => {
                let started = Instant::now();
                let found = decode_jpeg(&offer.jpeg).and_then(|image| {
                    let width = upright_width(image.width(), image.height(), c.rotation);
                    c.estimator
                        .estimate(&image, offer.time_ns)
                        .map(|pose| pose.map(|pose| (pose, width)))
                });
                let ms = started.elapsed().as_secs_f32() * 1000.0;
                match found {
                    Ok(found) => {
                        warned = false;
                        let found = found.map(|(mut pose, width)| {
                            smoother.apply(&mut pose, s.smoothing, offer.time_ns);
                            (pose, width)
                        });
                        let measured = found.as_ref().zip(setup).map(|((pose, width), setup)| {
                            posture.measure(pose, *width, offer.time_ns, setup, s.smoothing)
                        });
                        let found = found.map(|(pose, _)| pose);
                        if found.is_none() {
                            smoother.reset();
                            posture.reset();
                        }
                        Some(wire(found.as_ref(), ms, measured))
                    }
                    Err(e) => {
                        if !warned {
                            warn!("{actor}: body tracking failed on a live frame: {e}");
                            warned = true;
                        }
                        None
                    }
                }
            }
        };
        if handoff.stop.load(Ordering::Acquire) {
            return;
        }
        publish(offer.time_ns, offer.jpeg, pose);
    }
}

/// The `X-Pose` JSON for a frame's pose (`None`: nobody found), found in
/// `ms` milliseconds, with the golfer's posture on it.
pub fn wire(pose: Option<&Pose>, ms: f32, posture: Option<LivePosture>) -> Arc<str> {
    let live = LivePose {
        skeleton: flighthook::HALPE26.to_owned(),
        keypoints: pose.map_or_else(Vec::new, |p| {
            p.keypoints
                .iter()
                .map(|k| {
                    [
                        flighthook::round_px(k.x),
                        flighthook::round_px(k.y),
                        flighthook::round_score(k.score),
                    ]
                })
                .collect()
        }),
        bbox: pose.map(|p| [p.bbox.x1, p.bbox.y1, p.bbox.x2, p.bbox.y2].map(flighthook::round_px)),
        ms: (ms * 10.0).round() / 10.0,
        posture,
    };
    // Rounded finite numbers always serialize; a non-finite one (a broken
    // model) leaves the frame without a pose.
    Arc::from(serde_json::to_string(&live).unwrap_or_default())
}

/// One Euro smoothing of a live view's keypoints at a cutoff of `hz` (0:
/// none). A new cutoff, or a frame without the golfer, starts it afresh.
#[derive(Default)]
struct Smoother {
    hz: f64,
    filters: Vec<[OneEuro; 2]>,
}

impl Smoother {
    fn reset(&mut self) {
        self.filters.clear();
    }

    fn apply(&mut self, pose: &mut Pose, hz: f64, t_ns: u64) {
        if hz.to_bits() != self.hz.to_bits() {
            self.hz = hz;
            self.reset();
        }
        if hz <= 0.0 || !hz.is_finite() {
            return;
        }
        if self.filters.len() != pose.keypoints.len() {
            let params = OneEuroParams {
                min_cutoff: hz,
                ..OneEuroParams::default()
            };
            self.filters = vec![[OneEuro::new(params), OneEuro::new(params)]; pose.keypoints.len()];
        }
        // Host nanoseconds stay exact in f64 for months of uptime.
        #[allow(clippy::cast_precision_loss)]
        let t = t_ns as f64 / 1e9;
        for (k, [fx, fy]) in pose.keypoints.iter_mut().zip(&mut self.filters) {
            // Pixel coordinates round-trip through f64.
            #[allow(clippy::cast_possible_truncation)]
            {
                k.x = fx.filter(t, f64::from(k.x)) as f32;
                k.y = fy.filter(t, f64::from(k.y)) as f32;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
