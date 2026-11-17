//! Camera pictures: a camera card's still frame, the replay view, a modal
//! over the Shots tab that plays a shot's clips side by side, the same
//! player inline in the Cameras tab (with each clip's swing
//! analysis), and the camera cards in the Cards view's row beside the range
//! bars, which loop the shown shot's clip.
//!
//! The replay fetches each clip's sidecar, then the bytes of the frames
//! around the playhead with HTTP range requests, and decodes only the frames
//! it shows: at most one per camera per screen update. Playback follows
//! the wall clock, so when decoding can't keep up (1× on a phone) frames
//! are skipped and the clip still plays in time ([`crate::replay`] holds the
//! timeline and cache logic). A range request that fails is retried once;
//! a second failure is shown over that camera's picture.

use std::collections::HashMap;
use std::sync::Arc;

use crate::app::FlighthookApp;
use crate::net::{self, ReplayReply};
use crate::panels::{devices, impact};
use crate::replay::{
    ByteCache, Lru, Playback, Speed, Track, Window, card_pixels, crop, crop_scale, decode,
    window_for,
};
use crate::theme;
use crate::types::{BodyTrackingMode, CameraPosition, ShotRow, ShotVideo};

/// Decoded frames the replay keeps as textures.
const TEXTURES: usize = 32;
/// Decoded frames kept on a small screen (a phone).
const TEXTURES_SMALL: usize = 8;
/// Screens narrower than this (points) count as small.
const SMALL_SCREEN: f32 = 800.0;
/// Fetched clip bytes kept per camera.
const BYTES_PER_CAMERA: usize = 24 << 20;
/// Range requests one camera has out at a time.
const MAX_IN_FLIGHT: usize = 2;
/// Times a window is asked for before its error is shown.
const WINDOW_TRIES: u8 = 2;
/// How far ahead of the playhead the next window is fetched while playing
/// (ns).
const LOOKAHEAD_NS: u64 = 300_000_000;
/// How long the opening play waits for the frames at the start point (s).
const AUTOPLAY_TIMEOUT_S: f64 = 1.5;
/// Widest the replay view gets, as a share of the screen's width.
const VIEW_SHARE: f32 = 0.9;
/// Tallest a camera's picture gets, as a share of the screen's height.
const PICTURE_SHARE: f32 = 0.6;

/// A JPEG made into a texture, or `None` when it does not decode.
pub(crate) fn texture(
    ctx: &egui::Context,
    name: String,
    jpeg: &[u8],
) -> Option<egui::TextureHandle> {
    let (w, h, rgb) = decode(jpeg)?;
    let image = egui::ColorImage::from_rgb([w, h], &rgb);
    Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
}

/// A picture turned clockwise by `rotation` degrees and scaled to `width`,
/// keeping its aspect.
fn picture(
    ui: &mut egui::Ui,
    tex: &egui::TextureHandle,
    width: f32,
    rotation: u16,
) -> egui::Response {
    let size = crate::pose::turned_size(tex.size_vec2(), rotation);
    let scale = if size.x > 0.0 { width / size.x } else { 1.0 };
    let (rect, response) = ui.allocate_exact_size(size * scale, egui::Sense::hover());
    crate::pose::paint_turned(ui, tex.id(), rect, rotation, egui::CornerRadius::ZERO);
    response
}

// ---------------------------------------------------------------------------
// Still frame
// ---------------------------------------------------------------------------

/// A camera card's still frame.
#[derive(Default)]
pub(crate) struct Still {
    /// A request is out.
    pub(crate) loading: bool,
    /// The last frame received, not decoded yet.
    jpeg: Option<Vec<u8>>,
    texture: Option<egui::TextureHandle>,
    /// Why there is no frame.
    pub(crate) error: Option<String>,
}

impl Still {
    /// Take the server's reply.
    pub(crate) fn reply(&mut self, reply: Result<Vec<u8>, String>) {
        self.loading = false;
        match reply {
            Ok(jpeg) => {
                self.jpeg = Some(jpeg);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Whether [`still_picture`] shows a frame or why there is none.
    pub(crate) fn shown(&self) -> bool {
        self.jpeg.is_some() || self.texture.is_some() || self.error.is_some()
    }

    /// The frame as a texture, decoding a newly received one.
    fn texture(&mut self, ctx: &egui::Context, actor: &str) -> Option<&egui::TextureHandle> {
        if let Some(jpeg) = self.jpeg.take() {
            match texture(ctx, format!("still:{actor}"), &jpeg) {
                Some(tex) => self.texture = Some(tex),
                None => self.error = Some("the frame did not decode".to_owned()),
            }
        }
        self.texture.as_ref()
    }
}

/// A camera page's Still frame icon, for its header. It needs a
/// running camera and no request out. Returns whether it was clicked: the
/// caller fetches the frame.
pub(crate) fn still_button(
    ui: &mut egui::Ui,
    actor: &str,
    still: &mut Still,
    running: bool,
) -> bool {
    let clicked = devices::header_icon(
        ui,
        theme::Icon::Camera,
        "Still frame",
        &format!("still:{actor}"),
        running && !still.loading,
    );
    if clicked {
        still.loading = true;
        still.error = None;
    }
    clicked
}

/// A camera card's still frame, once fetched, turned clockwise by
/// `rotation` degrees at the card's width, or why there is none.
pub(crate) fn still_picture(ui: &mut egui::Ui, actor: &str, still: &mut Still, rotation: u16) {
    let ctx = ui.ctx().clone();
    if let Some(tex) = still.texture(&ctx, actor) {
        ui.add_space(4.0);
        let shown = picture(ui, tex, ui.available_width(), rotation);
        devices::mark(ui, || format!("still_image:{actor}"), &shown);
    }
    if let Some(error) = &still.error {
        ui.label(
            egui::RichText::new(error)
                .size(11.0)
                .color(theme::TEXT_MUTED),
        );
    }
}

// ---------------------------------------------------------------------------
// Replay view
// ---------------------------------------------------------------------------

/// A request the replay view needs sent.
#[derive(Debug, PartialEq)]
enum Fetch {
    /// A camera's sidecar.
    Sidecar(String),
    /// A camera's frames: the window and its bytes in the AVI.
    Frames(String, Window, (u64, u64)),
    /// A camera's clip keypoints (body tracking).
    Pose(String),
    /// A camera's clip swing analysis.
    Swing(String),
    /// A camera's swing numbers graded against the bands.
    Grades(String),
    /// Track a camera's clip now (body tracking not off, no keypoints yet).
    Analyze(String),
}

/// Whether the play a replay opens with has begun.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Autoplay {
    /// The playhead holds at the start point until the frames there are in,
    /// or [`AUTOPLAY_TIMEOUT_S`] after `since` (the first screen update with
    /// a playhead).
    Waiting { since: Option<f64> },
    /// Playing, or the user took over.
    Done,
}

/// One camera's clip as it is fetched: its sidecar, then windows of its
/// frames with range requests, at most [`MAX_IN_FLIGHT`] out at a time.
pub(crate) struct Clip {
    camera: String,
    track: Option<Track>,
    /// Why its clip can't be shown (the sidecar didn't load).
    error: Option<String>,
    sidecar_requested: bool,
    bytes: ByteCache,
    in_flight: Vec<Window>,
    /// Windows whose requests failed: how often, and the last error.
    failed: HashMap<Window, (u8, String)>,
}

impl Clip {
    fn new(camera: &str) -> Self {
        Self {
            camera: camera.to_owned(),
            track: None,
            error: None,
            sidecar_requested: false,
            bytes: ByteCache::new(BYTES_PER_CAMERA),
            in_flight: Vec::new(),
            failed: HashMap::new(),
        }
    }

    /// The window to fetch for `frame`, unless it is held, on its way, or
    /// failed [`WINDOW_TRIES`] times.
    fn wanted(&self, frame: usize) -> Option<Window> {
        let track = self.track.as_ref()?;
        if self.bytes.has(frame)
            || self.in_flight.iter().any(|w| w.contains(frame))
            || self.in_flight.len() >= MAX_IN_FLIGHT
        {
            return None;
        }
        window_for(track, frame).filter(|w| {
            self.failed
                .get(w)
                .is_none_or(|(tries, _)| *tries < WINDOW_TRIES)
        })
    }

    /// Why frame `frame` can't be shown: the clip didn't load, or its
    /// bytes failed [`WINDOW_TRIES`] times.
    fn frame_error(&self, frame: usize) -> Option<&str> {
        if let Some(error) = &self.error {
            return Some(error);
        }
        if self.bytes.has(frame) {
            return None;
        }
        self.failed
            .iter()
            .find(|(w, (tries, _))| *tries >= WINDOW_TRIES && w.contains(frame))
            .map(|(_, (_, error))| error.as_str())
    }

    /// What to ask the server for: the sidecar once, then the windows of
    /// `frames` it lacks. Each is recorded as asked for.
    fn due(&mut self, frames: &[usize]) -> Vec<Fetch> {
        let mut due = Vec::new();
        if !self.sidecar_requested {
            self.sidecar_requested = true;
            due.push(Fetch::Sidecar(self.camera.clone()));
        }
        if self.error.is_some() {
            return due;
        }
        for &frame in frames {
            if let (Some(window), Some(track)) = (self.wanted(frame), &self.track) {
                let bytes = window.bytes(track);
                self.in_flight.push(window);
                due.push(Fetch::Frames(self.camera.clone(), window, bytes));
            }
        }
        due
    }

    /// Take the sidecar, or why it didn't load.
    fn take_sidecar(&mut self, result: Result<stance::sidecar::Sidecar, String>) {
        match result {
            Ok(sidecar) => self.track = Some(Track::from_sidecar(&sidecar)),
            Err(e) => self.error = Some(e),
        }
    }

    /// Take the reply for `window`.
    fn take_frames(&mut self, window: Window, result: Result<Vec<u8>, String>) {
        self.in_flight.retain(|w| *w != window);
        match result {
            Ok(bytes) => {
                self.failed.remove(&window);
                self.bytes.insert(window, bytes);
            }
            Err(e) => {
                let failed = self.failed.entry(window).or_insert((0, String::new()));
                failed.0 = failed.0.saturating_add(1);
                failed.1 = e;
            }
        }
    }
}

/// Send `fetches` for `shot_id`'s clips, tagged `seq`.
#[cfg_attr(test, allow(dead_code))]
fn send(
    ctx: &egui::Context,
    pending: &net::PendingHandle,
    seq: u64,
    shot_id: &str,
    fetches: Vec<Fetch>,
) {
    for fetch in fetches {
        match fetch {
            Fetch::Sidecar(camera) => {
                net::fetch_sidecar(ctx, pending, seq, shot_id, &camera);
            }
            Fetch::Frames(camera, window, bytes) => {
                net::fetch_frames(ctx, pending, seq, shot_id, &camera, window, bytes);
            }
            Fetch::Pose(camera) => net::fetch_pose(ctx, pending, seq, shot_id, &camera),
            Fetch::Swing(camera) => net::fetch_swing(ctx, pending, seq, shot_id, &camera),
            Fetch::Grades(camera) => net::fetch_grades(ctx, pending, seq, shot_id, &camera),
            Fetch::Analyze(camera) => net::post_analyze(ctx, pending, seq, shot_id, &camera),
        }
    }
}

/// One camera's clip keypoints (body tracking), fetched once.
#[derive(Default)]
struct PoseClip {
    requested: bool,
    track: Option<crate::types::PoseTrackFile>,
}

impl PoseClip {
    /// The request to send for a clip whose body tracking is done
    /// (`ready`): once.
    fn due(&mut self, camera: &str, ready: bool) -> Option<Fetch> {
        if !ready || self.requested {
            return None;
        }
        self.requested = true;
        Some(Fetch::Pose(camera.to_owned()))
    }
}

/// One camera's clip swing analysis, fetched once, and its grades,
/// fetched again whenever what they depend on changes.
#[derive(Default)]
struct SwingClip {
    requested: bool,
    report: Option<Arc<crate::types::SwingReportFile>>,
    /// The grades key the grades were last asked for with.
    graded_for: Option<String>,
    grades: Option<Arc<flighthook::BodyGrades>>,
}

impl SwingClip {
    /// The request to send for a clip whose swing analysis is done
    /// (`ready`): once; then, once its report is in, its grades for
    /// `grades_key` (the settings they depend on), again when the key
    /// changes.
    fn due(&mut self, camera: &str, ready: bool, grades_key: &str) -> Option<Fetch> {
        if !ready {
            return None;
        }
        if !self.requested {
            self.requested = true;
            return Some(Fetch::Swing(camera.to_owned()));
        }
        if self.report.is_some() && self.graded_for.as_deref() != Some(grades_key) {
            self.graded_for = Some(grades_key.to_owned());
            return Some(Fetch::Grades(camera.to_owned()));
        }
        None
    }
}

/// How the replay draws skeletons: whether at all (the Cameras pane's `B`),
/// below which score a keypoint is hidden, and the golfer's hand.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PoseDraw {
    pub(crate) overlay: bool,
    pub(crate) min_score: f32,
    pub(crate) hand: crate::types::Handedness,
}

/// One camera in the replay view.
struct Camera {
    video: ShotVideo,
    /// Its card's name, or its actor ID.
    name: String,
    clip: Clip,
    pose: PoseClip,
    swing: SwingClip,
    /// The clockwise degrees its pictures are turned by: its camera's
    /// rotation now.
    rotation: u16,
    /// Its clip was asked to be tracked.
    analyze_requested: bool,
    /// The frame on screen while the next one loads.
    shown: Option<(usize, egui::TextureHandle)>,
}

impl Camera {
    /// The keypoints of the frame on screen: the pose frame at its exact
    /// time on the sidecar's time base, when there is one.
    fn shown_keypoints(&self) -> Option<&[flighthook::Keypoint]> {
        let (frame, _) = self.shown.as_ref()?;
        let t_ns = self.clip.track.as_ref()?.frames.get(*frame)?.t_ns;
        self.pose.track.as_ref()?.keypoints_at(t_ns)
    }
}

/// The open replay of one shot.
pub(crate) struct ReplayView {
    /// Which replay this is: replies for another are dropped.
    seq: u64,
    shot_id: String,
    shot_number: u32,
    cameras: Vec<Camera>,
    /// Seconds before the estimated impact where the playhead starts.
    start_before_impact_s: f64,
    pub(crate) playback: Option<Playback>,
    autoplay: Autoplay,
    /// Decoded frames by (camera, frame).
    textures: Lru<(usize, usize), egui::TextureHandle>,
    /// The input time of the last frame drawn (s).
    last_time: Option<f64>,
    /// The golfer's hand, for the skeleton's lead side.
    pub(crate) hand: crate::types::Handedness,
    /// The clips' swing analyses are fetched too (the inline player).
    swing: bool,
    /// When body tracking runs: nothing is fetched or asked for while it
    /// is off.
    mode: BodyTrackingMode,
    /// What the grades depend on besides the clip (the saved Player
    /// card): a change fetches them again.
    grades_key: String,
}

impl ReplayView {
    /// The replay of `shot`'s clips, numbered `seq`; `name` gives a camera
    /// actor's display name.
    pub(crate) fn new(
        seq: u64,
        shot: &ShotRow,
        name: impl Fn(&str) -> String,
        small: bool,
        start_before_impact_s: f64,
    ) -> Self {
        let cameras = shot
            .video
            .iter()
            .map(|v| Camera {
                video: v.clone(),
                name: name(&v.camera),
                clip: Clip::new(&v.camera),
                pose: PoseClip::default(),
                swing: SwingClip::default(),
                rotation: v.rotation,
                analyze_requested: false,
                shown: None,
            })
            .collect();
        Self {
            seq,
            shot_id: shot.shot_id.clone(),
            shot_number: shot.shot_number,
            cameras,
            start_before_impact_s,
            playback: None,
            autoplay: Autoplay::Waiting { since: None },
            textures: Lru::new(if small { TEXTURES_SMALL } else { TEXTURES }),
            last_time: None,
            hand: shot.handed.unwrap_or(crate::types::Handedness::Right),
            swing: false,
            mode: BodyTrackingMode::default(),
            grades_key: String::new(),
        }
    }

    /// Fetch the grades again whenever `key` (what they depend on besides
    /// the clip) differs from the one they were fetched for.
    pub(crate) fn set_grades_key(&mut self, key: &str) {
        if self.grades_key != key {
            key.clone_into(&mut self.grades_key);
        }
    }

    /// Turn each camera's pictures by `rotation` of its clip's listing:
    /// its camera's rotation now.
    pub(crate) fn set_rotations(&mut self, rotation: impl Fn(&ShotVideo) -> u16) {
        for cam in &mut self.cameras {
            cam.rotation = rotation(&cam.video);
        }
    }

    /// Forget camera `camera`'s keypoints and swing analysis: its clip is
    /// being tracked again, and they follow once it is.
    pub(crate) fn forget_analysis(&mut self, camera: &str) {
        for cam in self.cameras.iter_mut().filter(|c| c.video.camera == camera) {
            cam.video.pose = false;
            cam.video.swing = false;
            cam.pose = PoseClip::default();
            cam.swing = SwingClip::default();
            cam.analyze_requested = true;
        }
    }

    /// The same view, fetching each clip's swing analysis once it is done.
    pub(crate) fn with_swing(mut self) -> Self {
        self.swing = true;
        self
    }

    /// The swing events its clips' reports found, for the scrubber: none
    /// while body tracking is off.
    fn swing_markers(&self) -> Vec<Marker> {
        if !self.mode.is_on() {
            return Vec::new();
        }
        swing_markers(self.cameras.iter().filter_map(|c| {
            let track = c.clip.track.as_ref()?;
            Some((track.start_ns, c.swing.report.as_deref()?))
        }))
    }

    /// The cameras it plays, in order.
    pub(crate) fn cameras(&self) -> impl Iterator<Item = &str> {
        self.cameras.iter().map(|c| c.video.camera.as_str())
    }

    /// Whether camera `camera`'s swing analysis is fetched (or fetched):
    /// this view fetches them and the analysis is done.
    #[cfg(test)]
    pub(crate) fn wants_swing_of(&self, camera: &str) -> bool {
        self.swing
            && self
                .cameras
                .iter()
                .any(|c| c.video.camera == camera && c.video.swing)
    }

    /// Whether camera `camera`'s clip was asked to be tracked.
    #[cfg(test)]
    pub(crate) fn asked_to_track(&self, camera: &str) -> bool {
        self.cameras
            .iter()
            .any(|c| c.video.camera == camera && c.analyze_requested)
    }

    /// Follow body tracking's `mode` and take what is due, as a frame does
    /// (tests).
    #[cfg(test)]
    pub(crate) fn step(&mut self, mode: BodyTrackingMode) {
        self.mode = mode;
        let _ = self.due();
    }

    /// Ask again for every clip without keypoints to be tracked (the models
    /// were just downloaded, so the first ask found none).
    pub(crate) fn ask_again(&mut self) {
        for cam in self.cameras.iter_mut().filter(|c| !c.video.pose) {
            cam.analyze_requested = false;
        }
    }

    /// Camera `camera`'s swing analysis of the clip is done: it is fetched
    /// with the next requests (when this view fetches them).
    pub(crate) fn swing_ready(&mut self, camera: &str) {
        for cam in self.cameras.iter_mut().filter(|c| c.video.camera == camera) {
            cam.video.swing = true;
        }
    }

    /// The shot the replay shows.
    pub(crate) fn shot_id(&self) -> &str {
        &self.shot_id
    }

    /// The number its replies carry.
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    /// Camera `camera`'s body tracking of the clip is done: its keypoints
    /// are fetched with the next requests.
    pub(crate) fn pose_ready(&mut self, camera: &str) {
        for cam in self.cameras.iter_mut().filter(|c| c.video.camera == camera) {
            cam.video.pose = true;
        }
    }

    /// The tracks loaded so far.
    fn tracks(&self) -> Vec<Track> {
        self.cameras
            .iter()
            .filter_map(|c| c.clip.track.clone())
            .collect()
    }

    /// Take a reply from the server.
    pub(crate) fn reply(&mut self, reply: ReplayReply) {
        match reply {
            ReplayReply::Sidecar {
                replay,
                camera,
                result,
            } if replay == self.seq => {
                let Some(cam) = self.cameras.iter_mut().find(|c| c.video.camera == camera) else {
                    return;
                };
                cam.clip.take_sidecar(result);
                // Open at the start point once the first clip is in; the clock
                // runs when the frames there are in ([`ReplayView::tick`]).
                let tracks = self.tracks();
                if self.playback.is_none()
                    && let Some(span) = crate::replay::span(&tracks)
                {
                    let at = crate::replay::start_point(&tracks, span, self.start_before_impact_s);
                    let mut playback = Playback::paused_at(at);
                    playback.playing = true;
                    self.playback = Some(playback);
                }
            }
            ReplayReply::Frames {
                replay,
                camera,
                window,
                result,
            } if replay == self.seq => {
                let Some(cam) = self.cameras.iter_mut().find(|c| c.video.camera == camera) else {
                    return;
                };
                cam.clip.take_frames(window, result);
            }
            ReplayReply::Pose {
                replay,
                camera,
                result,
            } if replay == self.seq => {
                if let Some(cam) = self.cameras.iter_mut().find(|c| c.video.camera == camera) {
                    // A clip whose keypoints don't load is shown without them.
                    cam.pose.track = result.ok();
                }
            }
            ReplayReply::Swing {
                replay,
                camera,
                result,
            } if replay == self.seq => {
                if let Some(cam) = self.cameras.iter_mut().find(|c| c.video.camera == camera) {
                    // A report that doesn't load leaves the clip without one.
                    cam.swing.report = result.ok().map(Arc::new);
                }
            }
            ReplayReply::Grades {
                replay,
                camera,
                result,
            } if replay == self.seq => {
                if let Some(cam) = self.cameras.iter_mut().find(|c| c.video.camera == camera) {
                    // Grades that don't load leave the numbers plain.
                    cam.swing.grades = result.ok().map(Arc::new);
                }
            }
            ReplayReply::Analyzed {
                replay,
                camera,
                done: true,
            } if replay == self.seq => {
                // Tracked before this view knew: fetch what is there.
                self.pose_ready(&camera);
                self.swing_ready(&camera);
            }
            _ => {}
        }
    }

    /// Whether every camera that is loading normally holds the frame at the
    /// playhead. A camera whose clip or frames failed, or that has no
    /// frames, does not count.
    fn start_frames_in(&self) -> bool {
        let Some(p) = &self.playback else {
            return false;
        };
        self.cameras.iter().all(|cam| match &cam.clip.track {
            None => cam.clip.error.is_some(),
            Some(track) if track.frames.is_empty() => true,
            Some(track) => {
                let frame = track.frame_at(p.playhead);
                cam.clip.bytes.has(frame) || cam.clip.frame_error(frame).is_some()
            }
        })
    }

    /// One screen update at input time `now`: while the opening play waits,
    /// the playhead holds at the start point; then the clock runs from here.
    fn tick(&mut self, now: f64, span: Option<(u64, u64)>) {
        let last = self.last_time.replace(now);
        let (Some(span), true) = (span, self.playback.is_some()) else {
            return;
        };
        if let Autoplay::Waiting { since } = self.autoplay {
            let since = since.unwrap_or(now);
            if now - since < AUTOPLAY_TIMEOUT_S && !self.start_frames_in() {
                self.autoplay = Autoplay::Waiting { since: Some(since) };
            } else {
                self.autoplay = Autoplay::Done;
            }
            return;
        }
        if let (Some(p), Some(last)) = (&mut self.playback, last) {
            p.advance(now - last, span);
        }
    }

    /// Carry out a click. A pause, scrub or frame step before the opening
    /// play has begun cancels it.
    fn apply(&mut self, action: ReplayAction, span: (u64, u64), tracks: &[Track]) {
        let Some(p) = &mut self.playback else {
            return;
        };
        let waiting = matches!(self.autoplay, Autoplay::Waiting { .. });
        match action {
            ReplayAction::Toggle if waiting => p.playing = false,
            ReplayAction::Toggle => p.toggle(span),
            ReplayAction::Step(forward) => p.step(tracks, forward),
            ReplayAction::Speed(speed) => p.speed = speed,
            ReplayAction::Loop(on) => p.looping = on,
            ReplayAction::Seek(t) => {
                p.playing = false;
                p.seek(t, span);
            }
            ReplayAction::Close => {}
        }
        if matches!(
            action,
            ReplayAction::Toggle | ReplayAction::Step(_) | ReplayAction::Seek(_)
        ) {
            self.autoplay = Autoplay::Done;
        }
    }

    /// What to ask the server for now: every sidecar not asked for yet,
    /// and the frames around the playhead (and, while playing, a little
    /// ahead) each camera lacks. Unless body tracking is off, also each clip's
    /// keypoints (and swing analysis) once done, and once each, tracking
    /// for a clip without keypoints. Each is recorded as asked for.
    fn due(&mut self) -> Vec<Fetch> {
        let playhead = self.playback.as_ref().map(|p| (p.playhead, p.playing));
        let mut due = Vec::new();
        for cam in &mut self.cameras {
            let frames = match (playhead, &cam.clip.track) {
                (Some((t, playing)), Some(track)) => {
                    let mut frames = vec![track.frame_at(t)];
                    if playing {
                        frames.push(track.frame_at(t.saturating_add(LOOKAHEAD_NS)));
                    }
                    frames
                }
                _ => Vec::new(),
            };
            due.extend(cam.clip.due(&frames));
            if !self.mode.is_on() {
                continue;
            }
            if !cam.video.pose && !cam.analyze_requested {
                cam.analyze_requested = true;
                due.push(Fetch::Analyze(cam.video.camera.clone()));
            }
            due.extend(cam.pose.due(&cam.video.camera, cam.video.pose));
            if self.swing {
                due.extend(
                    cam.swing
                        .due(&cam.video.camera, cam.video.swing, &self.grades_key),
                );
            }
        }
        due
    }

    /// Send `fetches` ([`ReplayView::due`]).
    #[cfg_attr(test, allow(dead_code))]
    fn send(&self, ctx: &egui::Context, pending: &net::PendingHandle, fetches: Vec<Fetch>) {
        send(ctx, pending, self.seq, &self.shot_id, fetches);
    }

    /// Why camera `i` can't show the frame at the playhead, if it can't.
    fn camera_error(&self, i: usize) -> Option<String> {
        let cam = &self.cameras[i].clip;
        let frame = match (&self.playback, &cam.track) {
            (Some(p), Some(track)) => track.frame_at(p.playhead),
            _ => return cam.error.clone(),
        };
        cam.frame_error(frame).map(str::to_owned)
    }

    /// Camera `i`'s picture at the playhead: the picture on screen when it
    /// is that frame, else decoded from its bytes when they are in (the only
    /// decode), else the last picture it showed.
    fn frame_texture(&mut self, ctx: &egui::Context, i: usize) -> Option<egui::TextureHandle> {
        let playhead = self.playback.as_ref()?.playhead;
        let cam = &mut self.cameras[i];
        let track = cam.clip.track.as_ref()?;
        let frame = track.frame_at(playhead);
        if let Some((shown, tex)) = &cam.shown
            && *shown == frame
        {
            return Some(tex.clone());
        }
        if let Some(tex) = self.textures.get(&(i, frame)) {
            cam.shown = Some((frame, tex.clone()));
        } else if let Some(jpeg) = cam.clip.bytes.frame(track, frame)
            && let Some(tex) = texture(ctx, format!("replay:{i}:{frame}"), jpeg)
        {
            self.textures.insert((i, frame), tex.clone());
            cam.shown = Some((frame, tex));
        }
        cam.shown.as_ref().map(|(_, tex)| tex.clone())
    }
}

/// What a click in the replay view asks for.
#[derive(Clone, Copy)]
enum ReplayAction {
    Close,
    Toggle,
    Step(bool),
    Speed(Speed),
    Loop(bool),
    Seek(u64),
}

/// Where host time `t` falls on a scrubber track `left` and `width` wide:
/// clamped to the clip's `span`.
fn tick_x(left: f32, width: f32, span: (u64, u64), t: u64) -> f32 {
    let len = span.1.saturating_sub(span.0).max(1) as f64;
    let t = t.clamp(span.0, span.1.max(span.0));
    left + (t.saturating_sub(span.0) as f64 / len) as f32 * width
}

/// A swing event on the scrubber: its name and its time on the shot
/// timeline.
type Marker = (&'static str, u64);

/// Pixels apart under which two event markers share one spot (and one
/// hover, one click).
const MARKER_MERGE_PX: f32 = 6.0;

/// Width of an event marker's hover and click area.
const MARKER_HIT_PX: f32 = 10.0;

/// The mean of `n` values summing to `sum` (ns), to the nearest ns: it lies
/// between its terms. The scrubber's markers and the swing tempo average
/// the cameras with it.
pub(crate) fn rounded_mean(sum: u128, n: u128) -> u64 {
    u64::try_from((sum + n / 2) / n).unwrap_or(u64::MAX)
}

/// The events a shot's swing reports found, on the shot timeline: one per
/// event, in swing order, at the mean of the cameras' times. Each item is a
/// camera's track start (host ns) and its swing report; impact is left out
/// (it is the scrubber's gold tick) and so is a camera with no swing found.
fn swing_markers<'a>(
    reports: impl IntoIterator<Item = (u64, &'a crate::types::SwingReportFile)>,
) -> Vec<Marker> {
    const EVENTS: [&str; 4] = ["Address", "Takeaway", "Top", "Finish"];
    let mut sums = [(0_u128, 0_u128); EVENTS.len()];
    for (start_ns, report) in reports {
        if !report.swing_found() {
            continue;
        }
        for (name, event) in report.events.named() {
            let slot = EVENTS.iter().position(|n| *n == name);
            if let (Some(slot), Some(event)) = (slot, event) {
                sums[slot].0 += u128::from(start_ns) + u128::from(event.t_ns);
                sums[slot].1 += 1;
            }
        }
    }
    EVENTS
        .into_iter()
        .zip(sums)
        .filter(|(_, (_, n))| *n > 0)
        .map(|(name, (sum, n))| (name, rounded_mean(sum, n)))
        .collect()
}

/// `markers` drawn at `x_of`, with markers closer than [`MARKER_MERGE_PX`]
/// joined: each group is its events' names, the first one's time, and its x.
fn marker_groups(
    markers: &[Marker],
    x_of: impl Fn(u64) -> f32,
) -> Vec<(Vec<&'static str>, u64, f32)> {
    let mut groups: Vec<(Vec<&'static str>, u64, f32)> = Vec::new();
    for &(name, t) in markers {
        let x = x_of(t);
        match groups.last_mut() {
            Some((names, _, at)) if x - *at < MARKER_MERGE_PX => names.push(name),
            _ => groups.push((vec![name], t, x)),
        }
    }
    groups
}

/// The scrubber: the clip span as a track, the start point as a green tick,
/// the estimated impact as a gold tick, the swing events as small gray
/// ticks (hover names the event, a click seeks to it), and the playhead as
/// a handle. A click or drag seeks. Its mark is `<prefix>:scrubber`; an
/// event's is `<prefix>:event:<name>` in lower case, events sharing a spot
/// joined with `+`.
fn scrubber(
    ui: &mut egui::Ui,
    prefix: &str,
    span: (u64, u64),
    start: u64,
    impact: Option<u64>,
    events: &[Marker],
    playhead: u64,
) -> Option<u64> {
    let size = egui::vec2(ui.available_width(), 22.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    devices::mark(ui, || format!("{prefix}:scrubber"), &response);
    let track = rect.shrink2(egui::vec2(8.0, 8.0));
    let len = span.1.saturating_sub(span.0).max(1) as f64;
    let x_of = |t: u64| tick_x(track.left(), track.width(), span, t);
    let painter = ui.painter();
    painter.rect_filled(track, 3.0, theme::SURFACE_RAISED);
    let done = egui::Rect::from_min_max(track.min, egui::pos2(x_of(playhead), track.max.y));
    painter.rect_filled(done, 3.0, theme::ACCENT.gamma_multiply(0.5));
    let x = x_of(start);
    painter.line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(2.0, theme::ACCENT),
    );
    if let Some(impact) = impact {
        let x = x_of(impact);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(2.0, theme::GOLD),
        );
    }
    let mut seek = None;
    for (names, t, x) in marker_groups(events, x_of) {
        let hit = egui::Rect::from_center_size(
            egui::pos2(x, track.center().y),
            egui::vec2(MARKER_HIT_PX, rect.height()),
        );
        let id = ui.id().with(("event", names[0]));
        let marker = ui
            .interact(hit, id, egui::Sense::click())
            .on_hover_text(names.join(", "))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        devices::mark(
            ui,
            || {
                let names: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
                format!("{prefix}:event:{}", names.join("+"))
            },
            &marker,
        );
        let color = if marker.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_MUTED
        };
        painter.line_segment(
            [
                egui::pos2(x, track.center().y - 5.0),
                egui::pos2(x, track.center().y + 5.0),
            ],
            egui::Stroke::new(1.5, color),
        );
        if marker.clicked() {
            seek = Some(t);
        }
    }
    painter.circle_filled(
        egui::pos2(x_of(playhead), track.center().y),
        6.0,
        theme::TEXT,
    );
    if seek.is_some() {
        return seek;
    }
    let pos = response.interact_pointer_pos()?;
    if !(response.clicked() || response.dragged()) {
        return None;
    }
    let share = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
    // A share of the span: rounding to the nearest ns is exact enough.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(span.0 + (f64::from(share) * len) as u64)
}

/// The clips' timeline for one screen update: the tracks loaded, their
/// span and the estimated impact.
struct Timeline {
    tracks: Vec<Track>,
    span: Option<(u64, u64)>,
    impact: Option<u64>,
    /// The swing events the clips' reports found (none without reports).
    events: Vec<Marker>,
}

impl ReplayView {
    /// Get ready to draw at input time `now`, body tracking in `mode`:
    /// while `active`, send what is due and run the clock; otherwise
    /// (another player is in front) the clock holds and nothing is fetched.
    fn prepare(
        &mut self,
        ctx: &egui::Context,
        pending: &net::PendingHandle,
        active: bool,
        mode: BodyTrackingMode,
    ) -> Timeline {
        self.mode = mode;
        let tracks = self.tracks();
        let span = crate::replay::span(&tracks);
        let impact = crate::replay::impact(&tracks);
        if active {
            let fetches = self.due();
            #[cfg(not(test))]
            self.send(ctx, pending, fetches);
            #[cfg(test)]
            drop((fetches, pending));
            self.tick(ctx.input(|i| i.time), span);
            if self.playback.as_ref().is_some_and(|p| p.playing) {
                ctx.request_repaint();
            }
        } else {
            self.last_time = None;
        }
        let events = self.swing_markers();
        Timeline {
            tracks,
            span,
            impact,
            events,
        }
    }

    /// Height over width of the tallest clip as shown, turned by its
    /// camera's rotation, 16:9 until one is known.
    fn aspect(&self) -> f32 {
        self.cameras
            .iter()
            .filter_map(|c| {
                let track = c
                    .clip
                    .track
                    .as_ref()
                    .filter(|t| t.width > 0 && t.height > 0)?;
                let [w, h] = crate::pose::turned(track.width, track.height, c.rotation);
                Some(h as f32 / w as f32)
            })
            .fold(9.0 / 16.0, f32::max)
    }

    /// The cameras' columns side by side, each `column` wide, `gap` apart,
    /// at the playhead, marked under `prefix`; one under the other when
    /// `stacked`.
    fn columns(
        &mut self,
        ui: &mut egui::Ui,
        prefix: &str,
        (column, gap): (f32, f32),
        stacked: bool,
        draw: PoseDraw,
    ) {
        let ctx = ui.ctx().clone();
        let textures: Vec<Option<egui::TextureHandle>> = (0..self.cameras.len())
            .map(|i| self.frame_texture(&ctx, i))
            .collect();
        let errors: Vec<Option<String>> = (0..self.cameras.len())
            .map(|i| self.camera_error(i))
            .collect();
        let body = |ui: &mut egui::Ui| {
            ui.spacing_mut().item_spacing.x = gap;
            if stacked {
                ui.spacing_mut().item_spacing.y = gap;
            }
            for ((cam, tex), error) in self.cameras.iter().zip(&textures).zip(&errors) {
                ui.allocate_ui_with_layout(
                    egui::vec2(column, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(column);
                        camera_column(
                            ui,
                            prefix,
                            &self.shot_id,
                            cam,
                            tex.as_ref(),
                            error.as_deref(),
                            column,
                            draw,
                        );
                    },
                );
            }
        };
        if stacked {
            ui.vertical(body);
        } else {
            ui.horizontal_top(body);
        }
    }

    /// The scrubber and the transport row (play, frame steps, speed, loop),
    /// marked under `prefix`; a spinner while the clips load, or a line when
    /// none could be. In the portrait layout (`portrait`) the buttons are
    /// touch-sized and Loop wraps to a second row. Returns what was clicked.
    fn controls(
        &self,
        ui: &mut egui::Ui,
        prefix: &str,
        timeline: &Timeline,
        portrait: bool,
    ) -> Option<ReplayAction> {
        let button = if portrait {
            crate::topbar::TOUCH_TARGET
        } else {
            26.0
        };
        let mut action = None;
        if let (Some(p), Some(span)) = (&self.playback, timeline.span) {
            if let Some(t) = scrubber(
                ui,
                prefix,
                span,
                p.start,
                timeline.impact,
                &timeline.events,
                p.playhead,
            ) {
                action = Some(ReplayAction::Seek(t));
            }
            ui.add_space(6.0);
            let transport = |ui: &mut egui::Ui| {
                let (icon, hover) = if p.playing {
                    (theme::Icon::Pause, "Pause")
                } else {
                    (theme::Icon::Play, "Play")
                };
                let play = theme::icon(ui, icon, button, theme::ACCENT, true).on_hover_text(hover);
                devices::mark(ui, || format!("{prefix}:play"), &play);
                if play.clicked() {
                    action = Some(ReplayAction::Toggle);
                }
                for (forward, icon, hover) in [
                    (false, theme::Icon::ChevronLeft, "Previous frame"),
                    (true, theme::Icon::ChevronRight, "Next frame"),
                ] {
                    let step =
                        theme::icon(ui, icon, button, theme::ACCENT, true).on_hover_text(hover);
                    devices::mark(ui, || format!("{prefix}:step:{forward}"), &step);
                    if step.clicked() {
                        action = Some(ReplayAction::Step(forward));
                    }
                }
                ui.add_space(8.0);
                let labels = Speed::ALL.map(Speed::label);
                let selected = Speed::ALL.iter().position(|s| *s == p.speed).unwrap_or(0);
                let speed = theme::segmented(ui, &labels, selected, None);
                devices::mark(ui, || format!("{prefix}:speed"), &speed.response);
                if let Some(i) = speed.inner {
                    action = Some(ReplayAction::Speed(Speed::ALL[i]));
                }
                if portrait {
                    // Loop on the row under the transport.
                    ui.end_row();
                } else {
                    ui.add_space(8.0);
                }
                let looping = if portrait {
                    theme::pill_sized(ui, p.looping, "Loop", egui::vec2(0.0, button))
                } else {
                    theme::pill(ui, p.looping, "Loop")
                };
                devices::mark(ui, || format!("{prefix}:loop"), &looping);
                if looping.clicked() {
                    action = Some(ReplayAction::Loop(!p.looping));
                }
            };
            if portrait {
                ui.horizontal_wrapped(transport);
            } else {
                ui.horizontal(transport);
            }
        } else if self.cameras.iter().all(|c| c.clip.error.is_some()) {
            ui.label(
                egui::RichText::new("The clips could not be loaded.").color(theme::TEXT_MUTED),
            );
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
            });
        }
        action
    }

    /// Carry out `action` against this update's `timeline`.
    fn act(&mut self, action: Option<ReplayAction>, timeline: &Timeline) {
        if let (Some(action), Some(span)) = (action, timeline.span) {
            self.apply(action, span, &timeline.tracks);
        }
    }
}

/// One camera's column of the inline player: its actor ID, the column's
/// width and its swing analysis once fetched.
pub(crate) struct PlayerColumn {
    pub(crate) camera: String,
    pub(crate) width: f32,
    pub(crate) swing: Option<Arc<crate::types::SwingReportFile>>,
    /// Its swing numbers' grades once fetched.
    pub(crate) grades: Option<Arc<flighthook::BodyGrades>>,
    /// The frame its picture is of: its time after the clip's first frame
    /// (the time base of the swing report), `None` while the playhead is
    /// outside the clip or the clip hasn't loaded.
    pub(crate) frame_t_ns: Option<u64>,
}

/// How the inline player laid out its columns: the space before the first,
/// the gap between them, and each column.
pub(crate) struct PlayerLayout {
    pub(crate) pad: f32,
    pub(crate) gap: f32,
    pub(crate) columns: Vec<PlayerColumn>,
}

/// Space between two columns of a player.
pub(crate) const PLAYER_GAP: f32 = 12.0;

impl FlighthookApp {
    /// Open the replay of shot row `i`.
    pub(crate) fn open_replay(&mut self, ctx: &egui::Context, i: usize) {
        let Some(view) = self.new_player(ctx, i) else {
            return;
        };
        self.replay = Some(view);
    }

    /// A player of shot row `i`'s clips, numbered afresh; `None` when it
    /// has none.
    fn new_player(&mut self, ctx: &egui::Context, i: usize) -> Option<ReplayView> {
        let shot = self.shots.get(i)?;
        if shot.video.is_empty() {
            return None;
        }
        self.replay_seq += 1;
        let actors = &self.actors;
        let name = |id: &str| {
            actors
                .get(id)
                .map(|a| a.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| id.to_owned())
        };
        let small = ctx.content_rect().width() < SMALL_SCREEN;
        let start = self.settings.replay.start_before_impact_s();
        let mut view = ReplayView::new(self.replay_seq, shot, name, small, start);
        // A shot without its own hand takes the game's, as elsewhere.
        if shot.handed.is_none()
            && let Some(hand) = self.handed
        {
            view.hand = hand;
        }
        Some(view)
    }

    /// How skeletons are drawn now, for a golfer playing with `hand`: not
    /// at all while body tracking is off or the skeleton is hidden.
    pub(crate) fn pose_draw(&self, hand: crate::types::Handedness) -> PoseDraw {
        let settings = self.body_tracking();
        PoseDraw {
            overlay: settings.show_skeleton && settings.mode.is_on(),
            min_score: self.pose_min_score(),
            hand,
        }
    }

    /// The replay view over the Shots tab, while one is open.
    pub(crate) fn render_replay(&mut self, ctx: &egui::Context) {
        let Some(hand) = self.replay.as_ref().map(|v| v.hand) else {
            return;
        };
        let draw = self.pose_draw(hand);
        let mode = self.body_tracking().mode;
        let portrait = self.portrait;
        let settings = &self.settings;
        let Some(view) = &mut self.replay else {
            return;
        };
        view.set_rotations(|v| settings.saved_rotation(&v.camera).unwrap_or(v.rotation));
        let timeline = view.prepare(ctx, &self.pending, true, mode);

        // As wide as the screen allows, unless the pictures would be taller
        // than their share of its height.
        let n = view.cameras.len().max(1);
        let gap = PLAYER_GAP;
        let screen = ctx.content_rect().size();
        let aspect = view.aspect();
        // In the portrait layout: the screen's width less the margins.
        let share = if portrait {
            screen.x - 2.0 * crate::topbar::SIDE_MARGIN - 32.0
        } else {
            screen.x * VIEW_SHARE
        };
        let tallest = (share - gap * (n as f32 - 1.0)) / n as f32;
        let column = tallest.min(screen.y * PICTURE_SHARE / aspect);
        let width = column * n as f32 + gap * (n as f32 - 1.0);
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::new("replay"))
            .frame(theme::card().inner_margin(egui::Margin::same(16)))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Replay · Shot #{}", view.shot_number))
                            .size(17.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let close = ui.button("Close");
                        devices::mark(ui, || "replay:close".to_owned(), &close);
                        if close.clicked() {
                            action = Some(ReplayAction::Close);
                        }
                    });
                });
                ui.add_space(8.0);
                view.columns(ui, "replay", (column, gap), false, draw);
                ui.add_space(10.0);
                if let Some(a) = view.controls(ui, "replay", &timeline, portrait) {
                    action = Some(a);
                }
            });
        let close = modal.should_close() || matches!(action, Some(ReplayAction::Close));
        view.act(action, &timeline);
        if close {
            self.replay = None;
        }
    }

    /// Keep the inline player on shot row `i`'s clips: a new one when the
    /// shot or its cameras change.
    pub(crate) fn sync_shot_player(&mut self, ctx: &egui::Context, i: usize) {
        let Some(shot) = self.shots.get(i) else {
            self.shot_player = None;
            return;
        };
        let same = self.shot_player.as_ref().is_some_and(|p| {
            p.shot_id == shot.shot_id
                && p.cameras().eq(shot.video.iter().map(|v| v.camera.as_str()))
        });
        if same {
            // Keep it told what has finished since.
            if let Some(player) = &mut self.shot_player {
                for (cam, video) in player.cameras.iter_mut().zip(&shot.video) {
                    cam.video.pose |= video.pose;
                    cam.video.swing |= video.swing;
                }
            }
            return;
        }
        self.shot_player = self.new_player(ctx, i).map(ReplayView::with_swing);
    }

    /// The inline player in `ui`: the shot's clips side by side, each
    /// picture at most `max_picture` tall, then the scrubber and transport
    /// across them. It fetches nothing and holds its clock while the replay
    /// view is open. Returns its columns, for what goes under them.
    pub(crate) fn render_shot_player(
        &mut self,
        ui: &mut egui::Ui,
        max_picture: f32,
    ) -> Option<PlayerLayout> {
        let hand = self.shot_player.as_ref()?.hand;
        let portrait = self.portrait;
        let draw = self.pose_draw(hand);
        let mode = self.body_tracking().mode;
        let active = self.replay.is_none();
        let ctx = ui.ctx().clone();
        let settings = &self.settings;
        // The golfer who hit the shot, else the golfer up (as the server
        // grades a clip that recorded none).
        let golfer = self
            .shot_player
            .as_ref()
            .and_then(|v| self.shots.iter().rev().find(|r| r.shot_id == v.shot_id))
            .and_then(|r| r.golfer.as_deref())
            .or(self.turn_golfer.as_deref());
        let grades_key = settings.grades_key(golfer);
        let view = self.shot_player.as_mut()?;
        view.set_rotations(|v| settings.saved_rotation(&v.camera).unwrap_or(v.rotation));
        view.set_grades_key(&grades_key);
        let timeline = view.prepare(&ctx, &self.pending, active, mode);
        // The playhead the pictures below are drawn at.
        let playhead = view.playback.as_ref().map(|p| p.playhead);
        let n = view.cameras.len().max(1) as f32;
        let gap = PLAYER_GAP;
        let available = ui.available_width();
        let aspect = view.aspect();
        // In the portrait layout each picture takes the full width, the
        // cameras one under the other.
        let column = if portrait {
            available.max(1.0)
        } else {
            ((available - gap * (n - 1.0)) / n)
                .min(max_picture / aspect)
                .max(1.0)
        };
        let row = if portrait {
            column
        } else {
            column * n + gap * (n - 1.0)
        };
        let pad = ((available - row) / 2.0).max(0.0);
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add_space(pad);
            ui.vertical(|ui| {
                ui.set_width(row);
                view.columns(ui, "player", (column, gap), portrait, draw);
                ui.add_space(8.0);
                action = view.controls(ui, "player", &timeline, portrait);
            });
        });
        view.act(action, &timeline);
        let columns = view
            .cameras
            .iter()
            .map(|c| PlayerColumn {
                camera: c.video.camera.clone(),
                width: column,
                swing: c.swing.report.clone().filter(|_| mode.is_on()),
                grades: c.swing.grades.clone().filter(|_| mode.is_on()),
                frame_t_ns: c
                    .clip
                    .track
                    .as_ref()
                    .zip(playhead)
                    .and_then(|(track, t)| track.shown_t_ns(t)),
            })
            .collect();
        Some(PlayerLayout { pad, gap, columns })
    }
}

/// A camera's name and position, the position left out when the name reads
/// the same.
pub(crate) fn camera_heading(ui: &mut egui::Ui, name: &str, position: CameraPosition) {
    ui.label(egui::RichText::new(name).strong().color(theme::HEADING));
    let position = position.label();
    if !position.eq_ignore_ascii_case(name.trim()) {
        ui.label(egui::RichText::new(position).color(theme::TEXT_MUTED));
    }
}

/// One camera's column: its name and position (the position left out when
/// the name reads the same), its picture at the playhead with `error` over
/// it when the frame can't be shown, and the clip's download icon beside the
/// name; marked under `prefix`.
#[allow(clippy::too_many_arguments)]
fn camera_column(
    ui: &mut egui::Ui,
    prefix: &str,
    shot_id: &str,
    cam: &Camera,
    tex: Option<&egui::TextureHandle>,
    error: Option<&str>,
    width: f32,
    draw: PoseDraw,
) {
    ui.horizontal(|ui| {
        camera_heading(ui, &cam.name, cam.video.position);
        let download = theme::icon(ui, theme::Icon::Download, 20.0, theme::ACCENT, true)
            .on_hover_text("Download");
        devices::mark(
            ui,
            || format!("{prefix}:download:{}", cam.video.camera),
            &download,
        );
        if download.clicked() {
            ui.ctx().open_url(egui::OpenUrl::same_tab(net::clip_url(
                shot_id,
                &cam.video.camera,
                "avi",
            )));
        }
    });
    let aspect = cam
        .clip
        .track
        .as_ref()
        .filter(|t| t.width > 0 && t.height > 0)
        .map_or(9.0 / 16.0, |t| {
            let [w, h] = crate::pose::turned(t.width, t.height, cam.rotation);
            h as f32 / w as f32
        });
    let rect = match tex {
        Some(tex) => {
            let shown = picture(ui, tex, width, cam.rotation);
            devices::mark(
                ui,
                || format!("{prefix}:picture:{}", cam.video.camera),
                &shown,
            );
            if let (true, Some(keypoints), Some(track)) =
                (draw.overlay, cam.shown_keypoints(), &cam.clip.track)
            {
                crate::pose::paint(
                    &ui.painter_at(shown.rect),
                    crate::pose::turned(track.width, track.height, cam.rotation),
                    shown.rect,
                    keypoints,
                    draw.min_score,
                    draw.hand,
                );
            }
            shown.rect
        }
        None => {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, width * aspect), egui::Sense::hover());
            devices::mark(
                ui,
                || format!("{prefix}:picture:{}", cam.video.camera),
                &response,
            );
            ui.painter().rect_filled(rect, 6.0, theme::SURFACE_RAISED);
            if error.is_none() {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Loading…",
                    egui::FontId::proportional(13.0),
                    theme::TEXT_MUTED,
                );
            }
            rect
        }
    };
    if let Some(error) = error {
        let painter = ui.painter_at(rect);
        let galley = painter.layout(
            error.to_owned(),
            egui::FontId::proportional(13.0),
            theme::TEXT,
            (rect.width() - 24.0).max(40.0),
        );
        let band = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width(), galley.size().y + 16.0),
        );
        painter.rect_filled(band, 0.0, theme::BG.gamma_multiply(0.85));
        painter.galley(band.center() - galley.size() / 2.0, galley, theme::TEXT);
    }
}

// ---------------------------------------------------------------------------
// Camera card
// ---------------------------------------------------------------------------

/// Most bytes of decoded frames a camera card keeps. A card keeps every
/// k-th frame of its loop, k the smallest stride whose frames fit this at
/// the card's pixel size (every frame when the whole loop fits), and
/// decodes each kept frame once.
pub(crate) const CARD_TEXTURE_BYTES: usize = 48 << 20;

/// A camera card's clip of the shown shot: played at 1×, looping from the
/// start point to the clip end, each frame cut to the card's aspect and
/// scaled to its pixel size.
pub(crate) struct CardClip {
    /// The number its replies carry.
    seq: u64,
    shot_id: String,
    clip: Clip,
    start_before_impact_s: f64,
    playback: Option<Playback>,
    /// The clock runs: the frame at the start point is on screen (or could
    /// not be fetched).
    running: bool,
    /// The input time of the last update while running (s).
    last_time: Option<f64>,
    /// The pixel size frames are scaled to.
    px: [usize; 2],
    /// Decoded frames by frame index, at `px`.
    textures: Lru<usize, egui::TextureHandle>,
    /// Frames whose bytes did not decode.
    undecodable: Vec<usize>,
    /// The frame on screen while the next one loads.
    shown: Option<(usize, egui::TextureHandle)>,
    /// Screen updates it has wanted a decode and not had one, which puts
    /// it first in line for the next ([`FlighthookApp::decode_card_frame`]).
    waiting: u32,
}

impl CardClip {
    /// The clip of shot `shot_id` from `camera`, numbered `seq`.
    pub(crate) fn new(seq: u64, shot_id: &str, camera: &str, start_before_impact_s: f64) -> Self {
        Self {
            seq,
            shot_id: shot_id.to_owned(),
            clip: Clip::new(camera),
            start_before_impact_s,
            playback: None,
            running: false,
            last_time: None,
            px: [2, 2],
            textures: Lru::new(TEXTURES),
            undecodable: Vec::new(),
            shown: None,
            waiting: 0,
        }
    }

    /// The number its replies carry.
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    /// Whether it plays shot `shot_id`'s clip.
    pub(crate) fn shows(&self, shot_id: &str) -> bool {
        self.shot_id == shot_id
    }

    /// The clip's span, host ns, once its sidecar is in.
    fn span(&self) -> Option<(u64, u64)> {
        crate::replay::span(std::slice::from_ref(self.clip.track.as_ref()?))
    }

    /// What it loops: from the start point to the end of the clip's last
    /// frame (its time plus one frame interval), host ns.
    pub(crate) fn loop_span(&self) -> Option<(u64, u64)> {
        let track = self.clip.track.as_ref()?;
        let n = track.frames.len();
        let interval = if n >= 2 {
            track.time(n - 1).saturating_sub(track.time(n - 2))
        } else {
            0
        };
        Some((
            self.playback.as_ref()?.start,
            self.span()?.1.saturating_add(interval),
        ))
    }

    /// Frames from the start point to the end.
    fn loop_frames(&self) -> usize {
        match (&self.clip.track, &self.playback) {
            (Some(track), Some(p)) => track.frames.len() - track.frame_at(p.start),
            _ => 0,
        }
    }

    /// Every how many frames of the loop it keeps: the smallest stride
    /// whose frames fit [`CARD_TEXTURE_BYTES`] at `px`, 1 when the whole
    /// loop fits. Kept are the start point's frame and every `stride`-th
    /// after it.
    fn stride(&self) -> usize {
        let frame_bytes = (self.px[0] * self.px[1] * 4).max(1);
        let fit = (CARD_TEXTURE_BYTES / frame_bytes).max(1);
        self.loop_frames().div_ceil(fit).max(1)
    }

    /// How many decoded frames it keeps: the kept frames of the loop.
    fn capacity(&self) -> usize {
        self.loop_frames().div_ceil(self.stride())
    }

    /// The kept frame at or before `frame` (the start point's frame for a
    /// frame before it).
    fn kept_at(&self, frame: usize) -> Option<usize> {
        let p = self.playback.as_ref()?;
        let start = self.clip.track.as_ref()?.frame_at(p.start);
        let stride = self.stride();
        Some(start + frame.saturating_sub(start) / stride * stride)
    }

    /// Whether every frame of the loop is kept once decoded.
    #[cfg(test)]
    pub(crate) fn keeps_every_frame(&self) -> bool {
        self.loop_frames() > 0 && self.stride() == 1
    }

    /// Take a reply from the server.
    pub(crate) fn reply(&mut self, reply: ReplayReply) {
        match reply {
            ReplayReply::Sidecar { replay, result, .. } if replay == self.seq => {
                self.clip.take_sidecar(result);
                if let (Some(span), Some(track)) = (self.span(), &self.clip.track) {
                    let start = crate::replay::start_point(
                        std::slice::from_ref(track),
                        span,
                        self.start_before_impact_s,
                    );
                    let mut playback = Playback::paused_at(start);
                    playback.playing = true;
                    self.playback = Some(playback);
                    self.textures = Lru::new(self.capacity());
                }
            }
            ReplayReply::Frames {
                replay,
                window,
                result,
                ..
            } if replay == self.seq => self.clip.take_frames(window, result),
            _ => {}
        }
    }

    /// Scale frames to `px` from now on; a new size drops the decoded
    /// frames and the picture on screen.
    pub(crate) fn resize(&mut self, px: [usize; 2]) {
        if px != self.px {
            self.px = px;
            self.textures = Lru::new(self.capacity());
            self.shown = None;
        }
    }

    /// One screen update at input time `now`: the clock runs once the start
    /// frame is on screen.
    fn tick(&mut self, now: f64) {
        let span = self.span().zip(self.loop_span()).map(|(s, l)| (s.0, l.1));
        let (Some(p), Some(span), true) = (&mut self.playback, span, self.running) else {
            self.last_time = None;
            return;
        };
        if let Some(last) = self.last_time.replace(now) {
            p.advance(now - last, span);
        }
    }

    /// What to ask the server for: the sidecar, then the kept frames at the
    /// playhead and a little ahead (from the start point again past the
    /// end) that are not decoded yet.
    fn due(&mut self) -> Vec<Fetch> {
        let frames: Vec<usize> = match (&self.playback, &self.clip.track, self.loop_span()) {
            (Some(p), Some(track), Some((start, end))) => {
                let ahead = p.playhead.saturating_add(LOOKAHEAD_NS);
                let ahead = if ahead > end {
                    start.saturating_add(ahead - end).min(end)
                } else {
                    ahead
                };
                [track.frame_at(p.playhead), track.frame_at(ahead)]
                    .into_iter()
                    .filter_map(|f| self.kept_at(f))
                    .filter(|f| !self.textures.contains(f))
                    .collect()
            }
            _ => Vec::new(),
        };
        self.clip.due(&frames)
    }

    /// The frame to decode next, when its bytes are in: the first kept
    /// frame not decoded yet from the playhead on, in loop order (from the
    /// start point again past the end), so the loop fills in ahead of the
    /// playhead.
    pub(crate) fn next_to_decode(&self) -> Option<usize> {
        let p = self.playback.as_ref()?;
        let track = self.clip.track.as_ref()?;
        let at = self.kept_at(track.frame_at(p.playhead))?;
        let wanted = |f: &usize| {
            !self.textures.contains(f) && !self.undecodable.contains(f) && self.clip.bytes.has(*f)
        };
        let start = track.frame_at(p.start);
        let (n, stride) = (track.frames.len(), self.stride());
        (at..n)
            .step_by(stride)
            .chain((start..at).step_by(stride))
            .find(wanted)
    }

    /// Decode [`CardClip::next_to_decode`]: cut to the card's aspect and
    /// scaled to its pixel size, then kept. Returns whether it decoded one.
    pub(crate) fn decode_next(&mut self, ctx: &egui::Context) -> bool {
        let Some(frame) = self.next_to_decode() else {
            return false;
        };
        let Some(track) = self.clip.track.as_ref() else {
            return false;
        };
        let Some(jpeg) = self.clip.bytes.frame(track, frame) else {
            return false;
        };
        let name = format!("card:{}:{frame}", self.clip.camera);
        match card_texture(ctx, name, jpeg, self.px) {
            Some(tex) => {
                self.textures.insert(frame, tex);
                true
            }
            None => {
                self.undecodable.push(frame);
                false
            }
        }
    }

    /// The picture at the playhead: the decoded kept frame at or nearest
    /// before it in the loop, else the last picture shown. The clock starts
    /// once the start frame is on screen.
    fn frame_texture(&mut self) -> Option<egui::TextureHandle> {
        let p = self.playback.as_ref()?;
        let track = self.clip.track.as_ref()?;
        let at = self.kept_at(track.frame_at(p.playhead))?;
        let start = track.frame_at(p.start);
        let decoded = (start..=at)
            .rev()
            .step_by(self.stride())
            .find(|f| self.textures.contains(f));
        if let Some(frame) = decoded
            && self.shown.as_ref().is_none_or(|(shown, _)| *shown != frame)
            && let Some(tex) = self.textures.get(&frame)
        {
            self.shown = Some((frame, tex.clone()));
        }
        if !self.running
            && (self.shown.as_ref().is_some_and(|(f, _)| *f == start)
                || self.clip.frame_error(start).is_some()
                || self.undecodable.contains(&start))
        {
            self.running = true;
        }
        self.shown.as_ref().map(|(_, tex)| tex.clone())
    }

    /// Seconds until the next kept frame is due (the start point's again
    /// at the end of the loop), while the clock runs; never under
    /// [`CARD_REPAINT_MIN_S`].
    fn next_due(&self) -> Option<f64> {
        let (p, track) = (self.playback.as_ref()?, self.clip.track.as_ref()?);
        let (start, end) = self.loop_span()?;
        if !self.running || end <= start {
            return None;
        }
        let next = self.kept_at(track.frame_at(p.playhead))? + self.stride();
        let next = if next < track.frames.len() {
            track.time(next)
        } else {
            end
        }
        .max(p.playhead);
        #[allow(clippy::cast_precision_loss)]
        let due = next.saturating_sub(p.playhead) as f64 / 1e9;
        Some(due.max(CARD_REPAINT_MIN_S))
    }
}

/// The shortest wait between a camera card's repaints: a 60 Hz display's
/// frame. A faster clip skips frames to keep its time.
pub(crate) const CARD_REPAINT_MIN_S: f64 = 1.0 / 60.0;

/// A JPEG cut to the aspect of `px` and scaled to it, as a texture; `None`
/// when it does not decode.
fn card_texture(
    ctx: &egui::Context,
    name: String,
    jpeg: &[u8],
    px: [usize; 2],
) -> Option<egui::TextureHandle> {
    let (w, h, rgb) = decode(jpeg)?;
    let cut = crop([w, h], px);
    let rgb = crop_scale(&rgb, w, cut, px);
    let image = egui::ColorImage::from_rgb(px, &rgb);
    Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
}

/// The part of a camera card at `rect` the picture fills: under the name
/// line, inside the border.
pub(crate) fn card_body(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.left() + 1.0, rect.top() + impact::HEAD_H),
        egui::pos2(rect.right() - 1.0, rect.bottom() - 1.0),
    )
}

/// Paint a camera card at `rect`: `name` in its top-left, and the picture
/// `tex`, turned clockwise by `rotation` degrees, filling the body under it.
pub(crate) fn paint_camera_card(
    ui: &egui::Ui,
    rect: egui::Rect,
    name: &str,
    tex: Option<&egui::TextureHandle>,
    rotation: u16,
) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    impact::paint_card(painter, rect);
    let label = impact::label_galley(
        painter,
        name,
        rect.width() - 2.0 * impact::PAD,
        theme::TEXT_MUTED,
    );
    painter.galley(
        egui::pos2(rect.left() + impact::PAD, rect.top() + 8.0),
        label,
        theme::TEXT_MUTED,
    );
    let body = card_body(rect);
    if let Some(tex) = tex
        && body.width() >= 1.0
        && body.height() >= 1.0
    {
        let r = theme::CARD_RADIUS.saturating_sub(1);
        let corners = egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: r,
            se: r,
        };
        crate::pose::paint_turned(ui, tex.id(), body, rotation, corners);
    }
}

impl FlighthookApp {
    /// Keep a camera card's clip for each of `cameras` with a clip of shot
    /// row `i`, and drop every other.
    pub(crate) fn sync_card_clips(&mut self, i: usize, cameras: &[&str]) {
        let Some(shot) = self.shots.get(i) else {
            self.card_clips.clear();
            return;
        };
        let has = |camera: &str| shot.video.iter().any(|v| v.camera == camera);
        self.card_clips.retain(|camera, card| {
            card.shows(&shot.shot_id) && cameras.contains(&camera.as_str()) && has(camera)
        });
        for &camera in cameras {
            if has(camera) && !self.card_clips.contains_key(camera) {
                self.replay_seq += 1;
                let start = self.settings.replay.start_before_impact_s();
                let card = CardClip::new(self.replay_seq, &shot.shot_id, camera, start);
                self.card_clips.insert(camera.to_owned(), card);
            }
        }
    }

    /// Camera `camera`'s card at `rect`, playing its clip of the shown shot
    /// when it has one. It fetches nothing while the replay view is open.
    pub(crate) fn camera_card(&mut self, ui: &egui::Ui, rect: egui::Rect, camera: &str) {
        let ctx = ui.ctx().clone();
        let name = self
            .actors
            .get(camera)
            .map(|a| a.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| camera.to_owned());
        let modal = self.replay.is_some();
        let rotation = self.camera_rotation(camera);
        let tex = self.card_clips.get_mut(camera).and_then(|card| {
            // Frames are cut to the turned card's shape, then turned.
            let size = crate::pose::turned_size(card_body(rect).size(), rotation);
            card.resize(card_pixels([size.x, size.y], ctx.pixels_per_point()));
            card.tick(ctx.input(|i| i.time));
            if !modal {
                let fetches = card.due();
                #[cfg(not(test))]
                send(&ctx, &self.pending, card.seq, &card.shot_id, fetches);
                #[cfg(test)]
                drop(fetches);
            }
            let tex = card.frame_texture();
            if let Some(due) = card.next_due().filter(|_| !modal) {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(due));
            }
            tex
        });
        paint_camera_card(ui, rect, &name, tex.as_ref(), rotation);
    }

    /// Decode at most one camera card frame for this screen update, for
    /// the card that has waited longest for one; the others wait their
    /// turn. A decoded frame shows on the next update, asked for at once.
    pub(crate) fn decode_card_frame(&mut self, ctx: &egui::Context) {
        let mut wanting: Vec<(&String, &mut CardClip)> = self
            .card_clips
            .iter_mut()
            .filter(|(_, card)| card.next_to_decode().is_some())
            .collect();
        // Longest wait first, then camera order, so the pick is stable.
        wanting.sort_by(|a, b| b.1.waiting.cmp(&a.1.waiting).then(a.0.cmp(b.0)));
        let mut cards = wanting.into_iter().map(|(_, card)| card);
        let Some(first) = cards.next() else {
            return;
        };
        first.waiting = 0;
        if first.decode_next(ctx) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(CARD_REPAINT_MIN_S));
        }
        for card in cards {
            card.waiting = card.waiting.saturating_add(1);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
