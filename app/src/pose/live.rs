//! A camera's live body tracking (alpha): a worker thread
//! (`pose:camera.N`) that finds the golfer's keypoints on the newest live
//! frame and puts the frame in the camera's live slot together with them.
//!
//! The camera actor runs a worker only while a viewer watches the camera's
//! live view and body tracking is in Live mode; it stops it when the watch
//! lapses or the mode changes, which forgets the tracked golfer. Frames reach the worker through a
//! one-frame handoff: the camera actor offers every frame, each replacing
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
use crate::actors::camera::LIVE_FPS;

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
        let handoff = Arc::new(Handoff::default());
        let shared = Arc::clone(&handoff);
        let name = actor.to_owned();
        let thread = std::thread::Builder::new()
            .name(format!("pose:{actor}"))
            .spawn(move || run(&name, &shared, &settings, build, publish))?;
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

/// Sleep until `until`, or less once stopped. False when stopped.
fn pace(handoff: &Handoff, until: Instant) -> bool {
    loop {
        if handoff.stop.load(Ordering::Acquire) {
            return false;
        }
        let now = Instant::now();
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
            && !pace(handoff, start + LIVE_EVERY)
        {
            return;
        }
        let Some(offer) = take(handoff) else {
            return;
        };
        last_start = Some(Instant::now());
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
pub(crate) mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    use stance::pose::{BBox, Detection, Keypoint, Skeleton};

    use super::*;

    /// Body tracking's settings in Live mode, the rest at their defaults.
    pub(crate) fn live_section() -> BodyTrackingSection {
        BodyTrackingSection {
            mode: flighthook::BodyTrackingMode::Live,
            ..BodyTrackingSection::default()
        }
    }

    /// A frame's JPEG with its number in the first pixel row's brightness:
    /// the fake pose puts the number in the nose's x, so a pose names the
    /// frame it was found on.
    pub(crate) fn numbered_jpeg(n: u8) -> Arc<[u8]> {
        let (w, h) = (32u16, 24u16);
        let pixels = vec![n; usize::from(w) * usize::from(h) * 3];
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 100)
            .encode(&pixels, w, h, jpeg_encoder::ColorType::Rgb)
            .expect("encode");
        Arc::from(out)
    }

    /// A pose engine whose nose sits at x = the frame's brightness, taking
    /// `delay` per frame; it counts its calls.
    pub(crate) struct FakePose {
        pub(crate) delay: Duration,
        pub(crate) calls: Arc<AtomicUsize>,
        pub(crate) scores: VecDeque<f32>,
    }

    impl PoseEngine for FakePose {
        fn skeleton(&self) -> Skeleton {
            Skeleton::Halpe26
        }
        fn model_name(&self) -> &'static str {
            "fake-pose"
        }
        fn estimate(&mut self, image: &Image, bbox: &BBox) -> stance::Result<Pose> {
            std::thread::sleep(self.delay);
            self.calls.fetch_add(1, Ordering::SeqCst);
            let shade = f32::from(image.pixel(0, 0).map_or(0, |p| p[0]));
            let score = self.scores.pop_front().unwrap_or(0.9);
            let kps = (0..26u8)
                .map(|i| Keypoint {
                    x: if i == 0 { shade } else { 10.0 + f32::from(i) },
                    y: 12.0,
                    score,
                })
                .collect();
            Ok(Pose::new(Skeleton::Halpe26, kps, *bbox))
        }
        fn fork(&self) -> stance::Result<Self> {
            Ok(Self {
                delay: self.delay,
                calls: Arc::clone(&self.calls),
                scores: self.scores.clone(),
            })
        }
    }

    /// A detector that finds one person filling the frame, or nobody.
    pub(crate) struct FakeDetector {
        pub(crate) found: bool,
    }

    impl PersonDetector for FakeDetector {
        fn model_name(&self) -> &'static str {
            "fake-detector"
        }
        fn detect(&mut self, image: &Image) -> stance::Result<Vec<Detection>> {
            #[allow(clippy::cast_precision_loss)]
            let (w, h) = (image.width() as f32, image.height() as f32);
            Ok(if self.found {
                vec![Detection {
                    bbox: BBox::new(0.0, 0.0, w, h),
                    score: 0.9,
                }]
            } else {
                Vec::new()
            })
        }
        fn fork(&self) -> stance::Result<Self> {
            Ok(Self { found: self.found })
        }
    }

    /// A fake tracker.
    pub(crate) fn fake_tracker(
        delay: Duration,
        calls: &Arc<AtomicUsize>,
        found: bool,
        detect_every: u32,
    ) -> Box<dyn Estimate> {
        Box::new(PoseTracker::new(
            FakePose {
                delay,
                calls: Arc::clone(calls),
                scores: VecDeque::new(),
            },
            FakeDetector { found },
            stance::pose::PoseConfig {
                detect_every,
                ..stance::pose::PoseConfig::default()
            },
        ))
    }

    type Published = Arc<Mutex<Vec<(u64, Option<LivePose>)>>>;

    fn publisher() -> (Publish, Published) {
        let out: Published = Arc::default();
        let sink = Arc::clone(&out);
        let publish: Publish = Box::new(move |t, _jpeg, pose| {
            let pose = pose.map(|p| serde_json::from_str(&p).expect("X-Pose JSON"));
            sink.lock().unwrap().push((t, pose));
        });
        (publish, out)
    }

    fn wait_for(published: &Published, n: usize) {
        for _ in 0..500 {
            if published.lock().unwrap().len() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "{} of {n} frames published",
            published.lock().unwrap().len()
        );
    }

    fn settings(s: BodyTrackingSection) -> Settings {
        Box::new(move || LiveSettings {
            body: s.clone(),
            posture: None,
            rotation: Rotation::None,
        })
    }

    #[test]
    fn each_frame_goes_out_with_the_pose_found_on_it() {
        let calls = Arc::new(AtomicUsize::new(0));
        let built = Arc::clone(&calls);
        let build: Build =
            Box::new(move |s, _| Some(fake_tracker(Duration::ZERO, &built, true, s.detect_every)));
        let (publish, published) = publisher();
        let section = BodyTrackingSection {
            smoothing: 0.0,
            ..live_section()
        };
        let worker = LiveWorker::start("camera.0", settings(section), build, publish).unwrap();
        for n in [40u8, 120, 200] {
            worker.offer(u64::from(n), numbered_jpeg(n));
            let before = published.lock().unwrap().len();
            wait_for(&published, before + 1);
        }
        drop(worker);
        let out = published.lock().unwrap();
        assert_eq!(out.len(), 3);
        for (t, pose) in out.iter() {
            let pose = pose.as_ref().expect("a pose");
            assert_eq!(pose.keypoints.len(), 26);
            // The nose's x is the frame's shade (JPEG rounding aside).
            assert!(
                (pose.keypoints[0][0] - *t as f32).abs() <= 3.0,
                "frame {t}: nose at {}",
                pose.keypoints[0][0]
            );
            assert_eq!(pose.skeleton, "halpe26");
        }
    }

    #[test]
    fn a_face_on_or_down_the_line_camera_sends_the_posture_with_the_frame() {
        use flighthook::{CameraPosition, SwingView};
        for (position, view) in [
            (CameraPosition::GolferFront, Some(SwingView::FaceOn)),
            (CameraPosition::TargetLine, Some(SwingView::DownTheLine)),
            (CameraPosition::Overhead, None),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let built = Arc::clone(&calls);
            let build: Build = Box::new(move |s, _| {
                Some(fake_tracker(Duration::ZERO, &built, true, s.detect_every))
            });
            let (publish, published) = publisher();
            let settings: Settings = Box::new(move || LiveSettings {
                body: live_section(),
                posture: super::super::swing::live_setup(position, None, false),
                rotation: Rotation::None,
            });
            let worker = LiveWorker::start("camera.0", settings, build, publish).unwrap();
            worker.offer(1, numbered_jpeg(40));
            wait_for(&published, 1);
            drop(worker);
            let out = published.lock().unwrap();
            let pose = out[0].1.as_ref().expect("a pose");
            assert_eq!(pose.keypoints.len(), 26, "{position:?}");
            assert_eq!(pose.posture.as_ref().map(|p| p.view), view, "{position:?}");
        }
    }

    #[test]
    fn the_newest_frame_wins_and_frames_offered_while_busy_are_skipped() {
        let calls = Arc::new(AtomicUsize::new(0));
        let built = Arc::clone(&calls);
        // A slow model: 120 ms a frame.
        let build: Build = Box::new(move |s, _| {
            Some(fake_tracker(
                Duration::from_millis(120),
                &built,
                true,
                s.detect_every,
            ))
        });
        let (publish, published) = publisher();
        let worker =
            LiveWorker::start("camera.0", settings(live_section()), build, publish).unwrap();
        // The first frame starts a pose; four more arrive while it runs.
        assert!(!worker.offer(1, numbered_jpeg(10)));
        std::thread::sleep(Duration::from_millis(30));
        let mut skipped = 0;
        for n in 2..=5u8 {
            skipped += usize::from(worker.offer(u64::from(n), numbered_jpeg(n * 10)));
        }
        assert_eq!(skipped, 3, "each replaced the one waiting");
        wait_for(&published, 2);
        std::thread::sleep(Duration::from_millis(200));
        drop(worker);
        let times: Vec<u64> = published.lock().unwrap().iter().map(|(t, _)| *t).collect();
        // Frame 1, then straight to the newest: 2 to 4 never went out.
        assert_eq!(times, [1, 5]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_rate_is_held_to_the_live_rate() {
        let calls = Arc::new(AtomicUsize::new(0));
        let built = Arc::clone(&calls);
        let build: Build =
            Box::new(move |s, _| Some(fake_tracker(Duration::ZERO, &built, true, s.detect_every)));
        let (publish, published) = publisher();
        let worker =
            LiveWorker::start("camera.0", settings(live_section()), build, publish).unwrap();
        // A 120 fps camera for 0.5 s.
        let started = Instant::now();
        let mut n = 0u64;
        while started.elapsed() < Duration::from_millis(500) {
            n += 1;
            worker.offer(n, numbered_jpeg(1));
            std::thread::sleep(Duration::from_micros(8_333));
        }
        drop(worker);
        let out = published.lock().unwrap().len();
        // 15 a second: 8 in half a second, give or take one.
        assert!((6..=9).contains(&out), "{out} frames");
    }

    #[test]
    fn without_models_frames_go_out_without_a_pose_and_nobody_found_is_an_empty_pose() {
        let (publish, published) = publisher();
        let tries = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&tries);
        let build: Build = Box::new(move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            None
        });
        let worker =
            LiveWorker::start("camera.0", settings(live_section()), build, publish).unwrap();
        worker.offer(1, numbered_jpeg(1));
        wait_for(&published, 1);
        worker.offer(2, numbered_jpeg(1));
        wait_for(&published, 2);
        drop(worker);
        assert!(published.lock().unwrap().iter().all(|(_, p)| p.is_none()));
        assert_eq!(tries.load(Ordering::SeqCst), 1, "retried after a pause");

        // Models, but nobody in the frame.
        let calls = Arc::new(AtomicUsize::new(0));
        let built = Arc::clone(&calls);
        let build: Build =
            Box::new(move |s, _| Some(fake_tracker(Duration::ZERO, &built, false, s.detect_every)));
        let (publish, published) = publisher();
        let worker =
            LiveWorker::start("camera.0", settings(live_section()), build, publish).unwrap();
        worker.offer(1, numbered_jpeg(1));
        wait_for(&published, 1);
        drop(worker);
        let pose = published.lock().unwrap()[0].1.clone().expect("a pose");
        assert!(pose.keypoints.is_empty() && pose.bbox.is_none());
    }

    /// A pose engine that reports what it was given: the nose's x is the
    /// first pixel's brightness, keypoint 1 sits at (width, height).
    struct SizePose;

    impl PoseEngine for SizePose {
        fn skeleton(&self) -> Skeleton {
            Skeleton::Halpe26
        }
        fn model_name(&self) -> &'static str {
            "size-pose"
        }
        fn estimate(&mut self, image: &Image, bbox: &BBox) -> stance::Result<Pose> {
            #[allow(clippy::cast_precision_loss)]
            let (w, h) = (image.width() as f32, image.height() as f32);
            let shade = f32::from(image.pixel(0, 0).map_or(0, |p| p[0]));
            let kps = (0..26u8)
                .map(|i| match i {
                    0 => Keypoint {
                        x: shade,
                        y: 0.0,
                        score: 0.9,
                    },
                    1 => Keypoint {
                        x: w,
                        y: h,
                        score: 0.9,
                    },
                    _ => Keypoint {
                        x: 1.0,
                        y: 1.0,
                        score: 0.9,
                    },
                })
                .collect();
            Ok(Pose::new(Skeleton::Halpe26, kps, *bbox))
        }
        fn fork(&self) -> stance::Result<Self> {
            Ok(Self)
        }
    }

    /// A 32×24 JPEG, its top half bright and its bottom half dark.
    fn top_bright_jpeg() -> Arc<[u8]> {
        let (w, h) = (32u16, 24u16);
        let mut pixels = Vec::new();
        for y in 0..h {
            let v = if y < h / 2 { 220 } else { 20 };
            pixels.extend(std::iter::repeat_n(v, usize::from(w) * 3));
        }
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 100)
            .encode(&pixels, w, h, jpeg_encoder::ColorType::Rgb)
            .expect("encode");
        Arc::from(out)
    }

    #[test]
    fn live_frames_are_turned_by_the_cameras_rotation_before_tracking() {
        let rotation = Arc::new(Mutex::new(Rotation::None));
        let read = Arc::clone(&rotation);
        let settings: Settings = Box::new(move || LiveSettings {
            body: BodyTrackingSection {
                smoothing: 0.0,
                ..live_section()
            },
            posture: None,
            rotation: *read.lock().unwrap(),
        });
        let builds = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&builds);
        let build: Build = Box::new(move |s, r| {
            log.lock().unwrap().push(r);
            Some(Box::new(PoseTracker::new(
                SizePose,
                FakeDetector { found: true },
                stance::pose::PoseConfig {
                    rotation: r,
                    ..crate::pose::models::pose_config(s.model, s.detect_every)
                },
            )) as Box<dyn Estimate>)
        });
        let (publish, published) = publisher();
        let worker = LiveWorker::start("camera.0", settings, build, publish).unwrap();
        let frame = |n: u64| {
            worker.offer(n, top_bright_jpeg());
            wait_for(&published, usize::try_from(n).unwrap());
        };
        frame(1);
        *rotation.lock().unwrap() = Rotation::Cw90;
        frame(2);
        *rotation.lock().unwrap() = Rotation::Half;
        frame(3);
        drop(worker);
        assert_eq!(
            *builds.lock().unwrap(),
            [Rotation::None, Rotation::Cw90, Rotation::Half],
            "a new rotation makes a new tracker"
        );
        let poses: Vec<LivePose> = published
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone().expect("a pose"))
            .collect();
        // Upright: 32×24 and the top-left pixel bright.
        assert_eq!(poses[0].keypoints[1][..2], [32.0, 24.0]);
        assert!(
            poses[0].keypoints[0][0] > 150.0,
            "{:?}",
            poses[0].keypoints[0]
        );
        // A quarter turn clockwise: 24×32, the old bottom-left corner on top.
        assert_eq!(poses[1].keypoints[1][..2], [24.0, 32.0]);
        assert!(
            poses[1].keypoints[0][0] < 100.0,
            "{:?}",
            poses[1].keypoints[0]
        );
        // A half turn: 32×24 again, the dark bottom now on top.
        assert_eq!(poses[2].keypoints[1][..2], [32.0, 24.0]);
        assert!(
            poses[2].keypoints[0][0] < 100.0,
            "{:?}",
            poses[2].keypoints[0]
        );
    }

    #[test]
    fn the_posture_width_is_the_turned_frames() {
        assert_eq!(upright_width(1280, 720, Rotation::None), 1280);
        assert_eq!(upright_width(1280, 720, Rotation::Half), 1280);
        assert_eq!(upright_width(1280, 720, Rotation::Cw90), 720);
        assert_eq!(upright_width(1280, 720, Rotation::Ccw90), 720);
    }

    #[test]
    fn a_new_model_or_detection_interval_makes_a_new_tracker() {
        let section = Arc::new(Mutex::new(live_section()));
        let read = Arc::clone(&section);
        let settings: Settings = Box::new(move || LiveSettings {
            body: read.lock().unwrap().clone(),
            posture: None,
            rotation: Rotation::None,
        });
        let builds = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&builds);
        let calls = Arc::new(AtomicUsize::new(0));
        let build: Build = Box::new(move |s, _| {
            log.lock().unwrap().push((s.model, s.detect_every));
            Some(fake_tracker(Duration::ZERO, &calls, true, s.detect_every))
        });
        let (publish, published) = publisher();
        let worker = LiveWorker::start("camera.0", settings, build, publish).unwrap();
        let frame = |n: u64| {
            worker.offer(n, numbered_jpeg(1));
            wait_for(&published, usize::try_from(n).unwrap());
        };
        frame(1);
        frame(2);
        section.lock().unwrap().model = PoseModel::RtmposeS;
        frame(3);
        section.lock().unwrap().detect_every = 30;
        frame(4);
        section.lock().unwrap().min_score = 0.5;
        frame(5);
        drop(worker);
        assert_eq!(
            *builds.lock().unwrap(),
            [
                (PoseModel::RtmposeM, 10),
                (PoseModel::RtmposeS, 10),
                (PoseModel::RtmposeS, 30)
            ]
        );
    }

    #[test]
    fn a_frame_dated_earlier_than_the_last_starts_the_smoothing_afresh() {
        let calls = Arc::new(AtomicUsize::new(0));
        let built = Arc::clone(&calls);
        let build: Build =
            Box::new(move |s, _| Some(fake_tracker(Duration::ZERO, &built, true, s.detect_every)));
        let (publish, published) = publisher();
        // Heavy smoothing: a jump would barely move the nose.
        let section = BodyTrackingSection {
            smoothing: 0.1,
            ..live_section()
        };
        let worker = LiveWorker::start("camera.0", settings(section), build, publish).unwrap();
        let second = 1_000_000_000;
        worker.offer(10 * second, numbered_jpeg(40));
        wait_for(&published, 1);
        // The latency went up by half a second: the next frame is older.
        worker.offer(10 * second - second / 2, numbered_jpeg(200));
        wait_for(&published, 2);
        drop(worker);
        let out = published.lock().unwrap();
        let nose = out[1].1.as_ref().expect("a pose").keypoints[0][0];
        assert!((nose - 200.0).abs() <= 3.0, "nose at {nose}");
    }

    #[test]
    fn a_worker_whose_thread_ended_says_so_and_drop_does_not_wait() {
        let build: Build = Box::new(|_, _| panic!("a broken model"));
        let (publish, _published) = publisher();
        let worker =
            LiveWorker::start("camera.0", settings(live_section()), build, publish).unwrap();
        assert!(worker.alive());
        worker.offer(1, numbered_jpeg(1));
        let deadline = Instant::now() + Duration::from_secs(5);
        while worker.alive() {
            assert!(Instant::now() < deadline, "still running");
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(worker);
        // A slow model: dropping the worker mid-pose returns at once.
        let calls = Arc::new(AtomicUsize::new(0));
        let built = Arc::clone(&calls);
        let build: Build = Box::new(move |s, _| {
            Some(fake_tracker(
                Duration::from_millis(400),
                &built,
                true,
                s.detect_every,
            ))
        });
        let (publish, published) = publisher();
        let worker =
            LiveWorker::start("camera.0", settings(live_section()), build, publish).unwrap();
        worker.offer(1, numbered_jpeg(1));
        // Mid-pose.
        std::thread::sleep(Duration::from_millis(100));
        let dropped = Instant::now();
        drop(worker);
        assert!(dropped.elapsed() < Duration::from_millis(100));
        // It ends by itself and publishes nothing after being dropped.
        while calls.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the pose did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(published.lock().unwrap().is_empty());
    }

    #[test]
    fn smoothing_steadies_a_jittering_keypoint_and_zero_is_raw() {
        let pose = |x: f32| {
            Pose::new(
                Skeleton::Halpe26,
                vec![
                    Keypoint {
                        x,
                        y: 100.0,
                        score: 0.9
                    };
                    26
                ],
                BBox::default(),
            )
        };
        let mut smoother = Smoother::default();
        let mut xs = Vec::new();
        for i in 0..60u64 {
            let mut p = pose(if i % 2 == 0 { 106.0 } else { 94.0 });
            smoother.apply(&mut p, 1.0, i * 66_666_667);
            xs.push(p.keypoints[0].x);
        }
        let tail = &xs[30..];
        let spread = tail.iter().copied().fold(f32::MIN, f32::max)
            - tail.iter().copied().fold(f32::MAX, f32::min);
        assert!(spread < 9.0, "spread {spread}, raw 12");
        let mut raw = pose(94.0);
        smoother.apply(&mut raw, 0.0, 61 * 66_666_667);
        assert!((raw.keypoints[0].x - 94.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_wire_rounds_to_a_tenth_of_a_pixel() {
        let p = Pose::new(
            Skeleton::Halpe26,
            vec![
                Keypoint {
                    x: 651.234,
                    y: 160.36,
                    score: 0.9349
                };
                26
            ],
            BBox::new(512.04, 96.0, 790.0, 700.0),
        );
        let json = wire(Some(&p), 19.26, None);
        let back: LivePose = serde_json::from_str(&json).unwrap();
        assert_eq!(back.keypoints[0], [651.2, 160.4, 0.93]);
        assert_eq!(back.bbox, Some([512.0, 96.0, 790.0, 700.0]));
        assert!((back.ms - 19.3).abs() < 1e-4);
        assert!(json.len() < 700, "{} bytes", json.len());
        assert_eq!(
            &*wire(None, 3.0, None),
            r#"{"skeleton":"halpe26","keypoints":[],"ms":3.0}"#
        );
    }

    /// The real models (`STANCE_MODEL_DIR`) on the stance test image
    /// (`STANCE_TEST_IMAGE`, else `golfer.jpg` in the models folder), through
    /// the live worker; skipped without them.
    #[test]
    fn the_real_models_find_the_golfer_on_a_live_frame() {
        use crate::pose::models::{ModelState, Models, Official, pose_config};
        let Some(dir) = std::env::var_os("STANCE_MODEL_DIR").map(std::path::PathBuf::from) else {
            eprintln!("skipped: STANCE_MODEL_DIR is not set");
            return;
        };
        let image = std::env::var_os("STANCE_TEST_IMAGE")
            .map_or_else(|| dir.join("golfer.jpg"), std::path::PathBuf::from);
        let Ok(jpeg) = std::fs::read(&image) else {
            eprintln!("skipped: no test image at {}", image.display());
            return;
        };
        let models = Models::new(dir, std::sync::Arc::new(Official::default()));
        let model = PoseModel::RtmposeS;
        models.want(model);
        let deadline = Instant::now() + Duration::from_secs(600);
        let shared = loop {
            match models.state(model) {
                Some(ModelState::Ready(shared)) => break shared,
                Some(ModelState::Failed(e)) => panic!("{e}"),
                _ => {
                    assert!(Instant::now() < deadline, "the models did not load");
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };
        let build: Build = Box::new(move |s, _| {
            let tracker = shared
                .fork()
                .and_then(|m| m.tracker(pose_config(s.model, s.detect_every)))
                .expect("tracker");
            Some(Box::new(tracker) as Box<dyn Estimate>)
        });
        let (publish, published) = publisher();
        let section = BodyTrackingSection {
            model,
            ..live_section()
        };
        let worker = LiveWorker::start("camera.0", settings(section), build, publish).unwrap();
        worker.offer(7, Arc::from(jpeg));
        for _ in 0..12_000 {
            if !published.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(worker);
        let out = published.lock().unwrap();
        let (t, pose) = out.first().expect("a frame");
        assert_eq!(*t, 7);
        let pose = pose.as_ref().expect("a pose");
        let confident = pose.keypoints.iter().filter(|k| k[2] >= 0.3).count();
        assert_eq!(pose.keypoints.len(), 26);
        assert!(
            confident >= 20,
            "{confident} keypoints of 26 score 0.3 or more"
        );
        eprintln!("live pose in {} ms (debug build)", pose.ms);
    }
}
