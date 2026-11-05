//! What the camera actors share with the web layer (beta): each actor's
//! latest frame, for the device card's still, its live frame for the
//! Shots tab's Live view, and the camera it holds; and the camera list, enumerated at
//! most every few seconds.
//!
//! Frames never travel on the bus: an actor puts its newest JPEG here at
//! most twice a second, and `GET /api/cameras/{actor}/still` reads it. While
//! a viewer has asked within [`LIVE_WATCH`], the actor also puts frames in
//! the live slot, which `GET /api/cameras/{actor}/live` long-polls.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use flighthook::{CameraListEntry, CameraListResponse};
use stance::CameraInfo;
use tokio::sync::watch;

/// How long an enumeration is reused.
pub const LIST_CACHE: Duration = Duration::from_secs(3);

/// How long after a viewer's last request the live slot is kept up to date.
pub const LIVE_WATCH: Duration = Duration::from_secs(3);

/// Longest a live request waits for a frame newer than the one it has.
pub const LIVE_WAIT: Duration = Duration::from_secs(1);

/// Most live requests waiting on one camera at a time.
pub const LIVE_WAITERS: usize = 8;

/// Whether this platform has a camera backend. The test picture works
/// everywhere.
pub fn supported() -> bool {
    stance::camera::SUPPORTED
}

/// A frame in a camera's live slot.
#[derive(Debug)]
pub struct LiveFrame {
    /// Counts up with every live frame of every camera, so it also grows
    /// across a camera's restart.
    pub seq: u64,
    /// When the frame reached the computer, less the camera's latency, in
    /// nanoseconds on the host clock (`stance::clock`): the clock of the
    /// clips' sidecars.
    pub time_ns: u64,
    /// The frame, as the camera sent it.
    pub jpeg: Arc<[u8]>,
    /// The golfer's keypoints on this frame (the `X-Pose` JSON), when body
    /// tracking ran on it.
    pub pose: Option<Arc<str>>,
}

/// What a live request got.
#[derive(Debug)]
pub enum LiveWait {
    /// A frame newer than the one it asked after.
    Frame(Arc<LiveFrame>),
    /// None arrived in time.
    Timeout,
    /// [`LIVE_WAITERS`] requests are already waiting on the camera.
    Busy,
}

/// One camera actor's slot.
#[derive(Debug)]
struct Slot {
    /// The camera id it has open.
    camera: Option<String>,
    /// Its newest frame, as the camera sent it.
    still: Option<Arc<[u8]>>,
    /// Its live frame, while it is watched.
    live: watch::Sender<Option<Arc<LiveFrame>>>,
    /// When a viewer last asked for its live frames.
    watched_at: Option<Instant>,
    /// Live requests waiting on it.
    waiters: Arc<AtomicUsize>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            camera: None,
            still: None,
            live: watch::Sender::new(None),
            watched_at: None,
            waiters: Arc::default(),
        }
    }
}

/// A live request counted against its camera's [`LIVE_WAITERS`] while it
/// waits.
struct Waiter(Arc<AtomicUsize>);

impl Waiter {
    /// Count a request on `waiters`, unless [`LIVE_WAITERS`] already wait.
    fn enter(waiters: &Arc<AtomicUsize>) -> Option<Self> {
        waiters
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < LIVE_WAITERS).then_some(n + 1)
            })
            .ok()
            .map(|_| Self(Arc::clone(waiters)))
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// An enumeration: when it ran, the cameras it found, and why it failed.
type Listing = (Instant, Vec<CameraInfo>, Option<String>);

/// The camera actors' slots and the cached camera list.
#[derive(Debug, Default)]
pub struct Cameras {
    slots: RwLock<HashMap<String, Slot>>,
    list: Mutex<Option<Listing>>,
    /// The last live frame's sequence number.
    live_seq: AtomicU64,
}

impl Cameras {
    /// Record that actor `actor` has camera `camera` open, or none. With
    /// none its live frame goes: the camera has stopped.
    pub fn set_open(&self, actor: &str, camera: Option<&str>) {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        let slot = slots.entry(actor.to_owned()).or_default();
        slot.camera = camera.map(str::to_owned);
        if camera.is_none() {
            slot.live.send_replace(None);
        }
    }

    /// A viewer asked for actor `actor`'s live frames at `now`: the actor
    /// keeps its live slot up to date until [`LIVE_WATCH`] after this. A
    /// frame left from an earlier viewing is dropped, so a new viewer is
    /// never served an old picture.
    pub fn watch_live(&self, actor: &str, now: Instant) {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        let slot = slots.entry(actor.to_owned()).or_default();
        let lapsed = slot
            .watched_at
            .is_none_or(|at| now.saturating_duration_since(at) >= LIVE_WATCH);
        if lapsed {
            slot.live.send_replace(None);
        }
        slot.watched_at = Some(now);
    }

    /// Whether a viewer asked for actor `actor`'s live frames within
    /// [`LIVE_WATCH`] before `now`.
    pub fn live_wanted(&self, actor: &str, now: Instant) -> bool {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        slots
            .get(actor)
            .and_then(|s| s.watched_at)
            .is_some_and(|at| now.saturating_duration_since(at) < LIVE_WATCH)
    }

    /// Put actor `actor`'s live frame, taken at `time_ns` (see
    /// [`LiveFrame::time_ns`]), with the pose found on it, and wake the
    /// requests waiting for it. Returns its sequence number.
    pub fn set_live(
        &self,
        actor: &str,
        time_ns: u64,
        jpeg: Arc<[u8]>,
        pose: Option<Arc<str>>,
    ) -> u64 {
        let seq = self.live_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let frame = Arc::new(LiveFrame {
            seq,
            time_ns,
            jpeg,
            pose,
        });
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        slots
            .entry(actor.to_owned())
            .or_default()
            .live
            .send_replace(Some(frame));
        seq
    }

    /// Actor `actor`'s live frame, if it has one.
    #[cfg(test)]
    pub fn live(&self, actor: &str) -> Option<Arc<LiveFrame>> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        slots.get(actor).and_then(|s| s.live.borrow().clone())
    }

    /// Live requests waiting on actor `actor`.
    #[cfg(test)]
    pub fn live_waiters(&self, actor: &str) -> usize {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        slots
            .get(actor)
            .map_or(0, |s| s.waiters.load(Ordering::Acquire))
    }

    /// Actor `actor`'s first live frame with a sequence number above
    /// `after`: at once when the slot holds one, else as soon as one is
    /// put, waiting at most `wait`. An `after` above every number given
    /// out (a viewer from before a restart, whose numbers began afresh)
    /// counts as 0. At most [`LIVE_WAITERS`] requests wait on one camera;
    /// one more is answered [`LiveWait::Busy`] at once.
    pub async fn next_live(&self, actor: &str, after: u64, wait: Duration) -> LiveWait {
        let after = if after > self.live_seq.load(Ordering::Relaxed) {
            0
        } else {
            after
        };
        let (mut rx, waiters) = {
            let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
            let slot = slots.entry(actor.to_owned()).or_default();
            (slot.live.subscribe(), Arc::clone(&slot.waiters))
        };
        let newer = |f: &Option<Arc<LiveFrame>>| f.as_ref().is_some_and(|f| f.seq > after);
        if let Some(frame) = rx.borrow_and_update().as_ref().filter(|f| f.seq > after) {
            return LiveWait::Frame(Arc::clone(frame));
        }
        let Some(_waiter) = Waiter::enter(&waiters) else {
            return LiveWait::Busy;
        };
        match tokio::time::timeout(wait, rx.wait_for(newer)).await {
            Ok(Ok(frame)) => frame.clone().map_or(LiveWait::Timeout, LiveWait::Frame),
            // The slot went (the actor stopped), or the time is up.
            Ok(Err(_)) | Err(_) => LiveWait::Timeout,
        }
    }

    /// Put actor `actor`'s newest frame.
    pub fn set_still(&self, actor: &str, jpeg: Arc<[u8]>) {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        slots.entry(actor.to_owned()).or_default().still = Some(jpeg);
    }

    /// Actor `actor`'s newest frame.
    pub fn still(&self, actor: &str) -> Option<Arc<[u8]>> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        slots.get(actor).and_then(|s| s.still.clone())
    }

    /// Forget actor `actor` (it stopped).
    pub fn remove(&self, actor: &str) {
        self.slots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(actor);
    }

    /// The actor holding camera `camera`, if one does.
    pub fn holder(&self, camera: &str) -> Option<String> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        let mut holders: Vec<&String> = slots
            .iter()
            .filter(|(_, s)| s.camera.as_deref() == Some(camera))
            .map(|(actor, _)| actor)
            .collect();
        holders.sort();
        holders.first().map(|a| (*a).clone())
    }

    /// The camera list (`GET /api/cameras`), from an enumeration at most
    /// [`LIST_CACHE`] old; blocks while it enumerates. stance never
    /// opens a camera this process has open (an actor recording from it) to
    /// list it, on any platform: its entry is the one read when it was
    /// opened.
    pub fn list(&self) -> CameraListResponse {
        self.list_with(stance::list)
    }

    /// [`Cameras::list`] with `enumerate` in place of the platform's.
    pub fn list_with(
        &self,
        enumerate: impl FnOnce() -> stance::Result<Vec<CameraInfo>>,
    ) -> CameraListResponse {
        let (infos, error) = {
            let mut cache = self.list.lock().unwrap_or_else(|e| e.into_inner());
            match cache.as_ref() {
                Some((at, infos, error)) if at.elapsed() < LIST_CACHE => {
                    (infos.clone(), error.clone())
                }
                _ => {
                    let (infos, error) = match enumerate() {
                        Ok(infos) => (infos, None),
                        Err(stance::Error::Unsupported) => (Vec::new(), None),
                        Err(e) => (Vec::new(), Some(e.to_string())),
                    };
                    *cache = Some((Instant::now(), infos.clone(), error.clone()));
                    (infos, error)
                }
            }
        };
        if let Some(error) = error {
            tracing::warn!("cameras: listing failed: {error}");
        }
        let mut cameras: Vec<CameraListEntry> = infos.iter().map(|i| self.entry(i)).collect();
        let mut pattern = self.entry(&stance::TestPattern::camera_info());
        // Every test-pattern camera renders its own picture.
        pattern.in_use_by = None;
        cameras.push(pattern);
        CameraListResponse {
            supported: supported(),
            cameras,
        }
    }

    fn entry(&self, info: &CameraInfo) -> CameraListEntry {
        let mut modes: Vec<&stance::Format> = info.mjpeg_formats().collect();
        modes.sort_by(|a, b| b.fps().total_cmp(&a.fps()).then(b.area().cmp(&a.area())));
        let mut names: Vec<String> = modes
            .iter()
            .map(|f| {
                stance::FormatRequest::Mode {
                    width: f.width,
                    height: f.height,
                    fps: f.fps(),
                }
                .to_string()
            })
            .collect();
        names.dedup();
        CameraListEntry {
            id: info.id.clone(),
            name: info.name.clone(),
            usb: info.usb.map(|u| u.to_string()),
            modes: names,
            in_use_by: self.holder(&info.id),
            error: info.probe_error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_hold_the_still_and_the_camera() {
        let cams = Cameras::default();
        assert!(cams.still("camera.0").is_none());
        cams.set_open("camera.0", Some("usb-1"));
        cams.set_still("camera.0", Arc::from(vec![0xFF, 0xD8].as_slice()));
        assert_eq!(cams.still("camera.0").as_deref(), Some(&[0xFF, 0xD8][..]));
        assert_eq!(cams.holder("usb-1").as_deref(), Some("camera.0"));
        assert_eq!(cams.holder("usb-2"), None);
        cams.remove("camera.0");
        assert!(cams.still("camera.0").is_none());
        assert_eq!(cams.holder("usb-1"), None);
    }

    fn jpeg(byte: u8) -> Arc<[u8]> {
        Arc::from(vec![0xFF, 0xD8, byte].as_slice())
    }

    #[test]
    fn the_live_slot_is_wanted_for_a_few_seconds_after_each_request() {
        let cams = Cameras::default();
        let t0 = Instant::now();
        assert!(!cams.live_wanted("camera.0", t0), "nobody asked");
        cams.watch_live("camera.0", t0);
        assert!(cams.live_wanted("camera.0", t0));
        assert!(cams.live_wanted("camera.0", t0 + LIVE_WATCH - Duration::from_millis(1)));
        assert!(!cams.live_wanted("camera.0", t0 + LIVE_WATCH));
        assert!(!cams.live_wanted("camera.1", t0), "per camera");
        // Every request moves the window on.
        cams.watch_live("camera.0", t0 + Duration::from_secs(2));
        assert!(cams.live_wanted("camera.0", t0 + LIVE_WATCH));
        assert!(!cams.live_wanted("camera.0", t0 + Duration::from_secs(2) + LIVE_WATCH));
    }

    #[test]
    fn a_new_viewing_never_gets_the_last_viewings_frame() {
        let cams = Cameras::default();
        let t0 = Instant::now();
        cams.watch_live("camera.0", t0);
        let first = cams.set_live("camera.0", 5, jpeg(1), None);
        // Still watched: the frame stays.
        cams.watch_live("camera.0", t0 + Duration::from_secs(1));
        assert_eq!(cams.live("camera.0").map(|f| f.seq), Some(first));
        // Lapsed, then asked again: gone.
        cams.watch_live("camera.0", t0 + Duration::from_secs(1) + LIVE_WATCH);
        assert!(cams.live("camera.0").is_none());
        // Numbers keep counting up, across cameras too.
        let second = cams.set_live("camera.1", 6, jpeg(2), None);
        assert!(second > first);
        let third = cams.set_live("camera.0", 7, jpeg(3), None);
        assert!(third > second);
        // A camera that stops loses its live frame.
        cams.set_open("camera.0", None);
        assert!(cams.live("camera.0").is_none());
    }

    #[tokio::test]
    async fn a_live_request_gets_a_newer_frame_at_once_or_waits_for_one() {
        let cams = Arc::new(Cameras::default());
        let seq = cams.set_live("camera.0", 42, jpeg(1), None);
        let started = Instant::now();
        match cams.next_live("camera.0", 0, Duration::from_secs(5)).await {
            LiveWait::Frame(f) => assert_eq!((f.seq, f.time_ns, f.jpeg[2]), (seq, 42, 1)),
            other => panic!("{other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(1), "no wait");
        // Nothing newer: the time runs out.
        let started = Instant::now();
        let wait = Duration::from_millis(60);
        assert!(matches!(
            cams.next_live("camera.0", seq, wait).await,
            LiveWait::Timeout
        ));
        assert!(started.elapsed() >= wait);
        // A frame put while it waits answers it.
        let putter = Arc::clone(&cams);
        let put = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            putter.set_live("camera.0", 43, jpeg(2), None)
        });
        match cams
            .next_live("camera.0", seq, Duration::from_secs(5))
            .await
        {
            LiveWait::Frame(f) => assert_eq!(f.seq, put.await.expect("put")),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_viewer_from_before_a_restart_gets_the_new_frames() {
        // A fresh process: its numbers start again at 1.
        let cams = Arc::new(Cameras::default());
        let seq = cams.set_live("camera.0", 42, jpeg(1), None);
        match cams
            .next_live("camera.0", 18_234, Duration::from_secs(5))
            .await
        {
            LiveWait::Frame(f) => assert_eq!(f.seq, seq),
            other => panic!("{other:?}"),
        }
        // Waiting for the next one works the same.
        let putter = Arc::clone(&cams);
        let put = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            putter.set_live("camera.0", 43, jpeg(2), None)
        });
        cams.set_open("camera.0", Some("cam"));
        cams.set_open("camera.0", None);
        match cams
            .next_live("camera.0", 18_234, Duration::from_secs(5))
            .await
        {
            LiveWait::Frame(f) => assert_eq!(f.seq, put.await.expect("put")),
            other => panic!("{other:?}"),
        }
        // A number given out is still asked after as usual.
        assert!(matches!(
            cams.next_live("camera.0", seq + 1, Duration::from_millis(30))
                .await,
            LiveWait::Timeout
        ));
    }

    #[tokio::test]
    async fn live_requests_waiting_on_a_camera_are_bounded() {
        let cams = Arc::new(Cameras::default());
        let waits: Vec<_> = (0..LIVE_WAITERS)
            .map(|_| {
                let cams = Arc::clone(&cams);
                tokio::spawn(
                    async move { cams.next_live("camera.0", 0, Duration::from_secs(5)).await },
                )
            })
            .collect();
        // Let them all start waiting.
        for _ in 0..400 {
            if cams.live_waiters("camera.0") == LIVE_WAITERS {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(matches!(
            cams.next_live("camera.0", 0, Duration::from_secs(5)).await,
            LiveWait::Busy
        ));
        // Another camera has its own count.
        assert!(matches!(
            cams.next_live("camera.1", 0, Duration::from_millis(1))
                .await,
            LiveWait::Timeout
        ));
        // A frame answers every waiter, and the count goes back to 0.
        cams.set_live("camera.0", 1, jpeg(1), None);
        for wait in waits {
            assert!(matches!(wait.await.expect("wait"), LiveWait::Frame(_)));
        }
        assert_eq!(cams.live_waiters("camera.0"), 0);
    }

    fn info(id: &str, formats: Vec<stance::Format>) -> CameraInfo {
        CameraInfo {
            id: id.into(),
            name: "HD Pro Webcam C920".into(),
            usb: Some(stance::camera::UsbId {
                vid: 0x046d,
                pid: 0x082d,
            }),
            formats,
            controls: Vec::new(),
            exposure_unit: None,
            probe_error: None,
        }
    }

    #[test]
    fn the_list_names_modes_fastest_first_marks_held_cameras_and_ends_with_the_test_pattern() {
        use stance::Format;
        let cams = Cameras::default();
        cams.set_open("camera.1", Some("usb-1"));
        let yuyv = Format {
            fourcc: "YUY2".into(),
            ..Format::mjpeg(640, 480, 30)
        };
        let list = cams.list_with(|| {
            Ok(vec![
                info(
                    "usb-1",
                    vec![
                        Format::mjpeg(1920, 1080, 30),
                        yuyv,
                        Format::mjpeg(1280, 720, 60),
                        Format::mjpeg(640, 480, 60),
                    ],
                ),
                info("usb-2", Vec::new()),
            ])
        });
        assert_eq!(list.supported, supported());
        assert_eq!(list.cameras.len(), 3);
        let first = &list.cameras[0];
        assert_eq!(first.modes, ["1280x720@60", "640x480@60", "1920x1080@30"]);
        assert_eq!(first.usb.as_deref(), Some("046d:082d"));
        assert_eq!(first.in_use_by.as_deref(), Some("camera.1"));
        assert_eq!(list.cameras[1].in_use_by, None);
        let last = list.cameras.last().expect("test pattern");
        assert_eq!(last.id, flighthook::CameraSection::TEST_PATTERN);
        assert_eq!(last.id, stance::TEST_PATTERN_ID);
        assert_eq!(last.name, stance::TEST_PATTERN_NAME);
        assert_eq!(last.modes, ["1280x720@60", "640x360@30"]);
        // Reused within the cache time: no second enumeration.
        let again = cams.list_with(|| panic!("enumerated twice"));
        assert_eq!(again.cameras.len(), 3);
    }
}
