//! Camera pictures: a camera card's still frame, the replay view, a modal
//! over the Shots tab that plays a shot's clips side by side, the same
//! player inline in the Shots tab's Cameras pane (with each clip's swing
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

/// A picture scaled to `width`, keeping its aspect.
fn picture(ui: &mut egui::Ui, tex: &egui::TextureHandle, width: f32) -> egui::Response {
    let size = tex.size_vec2();
    let scale = if size.x > 0.0 { width / size.x } else { 1.0 };
    ui.add(egui::Image::new((tex.id(), size * scale)))
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

/// A camera card's Still frame button, for its action row. It needs a
/// running camera and no request out. Returns whether it was clicked: the
/// caller fetches the frame.
pub(crate) fn still_button(
    ui: &mut egui::Ui,
    actor: &str,
    still: &mut Still,
    running: bool,
) -> bool {
    let clicked = crate::panels::settings::action_button(
        ui,
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

/// A camera card's still frame, once fetched, at the card's width, or why
/// there is none.
pub(crate) fn still_picture(ui: &mut egui::Ui, actor: &str, still: &mut Still) {
    let ctx = ui.ctx().clone();
    if let Some(tex) = still.texture(&ctx, actor) {
        ui.add_space(4.0);
        let shown = picture(ui, tex, ui.available_width());
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

/// One camera's clip swing analysis, fetched once.
#[derive(Default)]
struct SwingClip {
    requested: bool,
    report: Option<Arc<crate::types::SwingReportFile>>,
}

impl SwingClip {
    /// The request to send for a clip whose swing analysis is done
    /// (`ready`): once.
    fn due(&mut self, camera: &str, ready: bool) -> Option<Fetch> {
        if !ready || self.requested {
            return None;
        }
        self.requested = true;
        Some(Fetch::Swing(camera.to_owned()))
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
                due.extend(cam.swing.due(&cam.video.camera, cam.video.swing));
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
/// the estimated impact as a gold tick, the swing events as small grey
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
        let colour = if marker.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_MUTED
        };
        painter.line_segment(
            [
                egui::pos2(x, track.center().y - 5.0),
                egui::pos2(x, track.center().y + 5.0),
            ],
            egui::Stroke::new(1.5, colour),
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

    /// Height over width of the tallest clip, 16:9 until one is known.
    fn aspect(timeline: &Timeline) -> f32 {
        timeline
            .tracks
            .iter()
            .filter(|t| t.width > 0)
            .map(|t| t.height as f32 / t.width as f32)
            .fold(9.0 / 16.0, f32::max)
    }

    /// The cameras' columns side by side, each `column` wide, `gap` apart,
    /// at the playhead, marked under `prefix`.
    fn columns(&mut self, ui: &mut egui::Ui, prefix: &str, column: f32, gap: f32, draw: PoseDraw) {
        let ctx = ui.ctx().clone();
        let textures: Vec<Option<egui::TextureHandle>> = (0..self.cameras.len())
            .map(|i| self.frame_texture(&ctx, i))
            .collect();
        let errors: Vec<Option<String>> = (0..self.cameras.len())
            .map(|i| self.camera_error(i))
            .collect();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
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
        });
    }

    /// The scrubber and the transport row (play, frame steps, speed, loop),
    /// marked under `prefix`; a spinner while the clips load, or a line when
    /// none could be. Returns what was clicked.
    fn controls(
        &self,
        ui: &mut egui::Ui,
        prefix: &str,
        timeline: &Timeline,
    ) -> Option<ReplayAction> {
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
            ui.horizontal(|ui| {
                let (icon, hover) = if p.playing {
                    (theme::Icon::Pause, "Pause")
                } else {
                    (theme::Icon::Play, "Play")
                };
                let play = theme::icon(ui, icon, 26.0, theme::ACCENT, true).on_hover_text(hover);
                devices::mark(ui, || format!("{prefix}:play"), &play);
                if play.clicked() {
                    action = Some(ReplayAction::Toggle);
                }
                for (forward, icon, hover) in [
                    (false, theme::Icon::ChevronLeft, "Previous frame"),
                    (true, theme::Icon::ChevronRight, "Next frame"),
                ] {
                    let step =
                        theme::icon(ui, icon, 26.0, theme::ACCENT, true).on_hover_text(hover);
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
                ui.add_space(8.0);
                let looping = theme::pill(ui, p.looping, "Loop");
                devices::mark(ui, || format!("{prefix}:loop"), &looping);
                if looping.clicked() {
                    action = Some(ReplayAction::Loop(!p.looping));
                }
            });
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
        let Some(view) = &mut self.replay else {
            return;
        };
        let timeline = view.prepare(ctx, &self.pending, true, mode);

        // As wide as the screen allows, unless the pictures would be taller
        // than their share of its height.
        let n = view.cameras.len().max(1);
        let gap = PLAYER_GAP;
        let screen = ctx.content_rect().size();
        let aspect = ReplayView::aspect(&timeline);
        let tallest = (screen.x * VIEW_SHARE - gap * (n as f32 - 1.0)) / n as f32;
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
                view.columns(ui, "replay", column, gap, draw);
                ui.add_space(10.0);
                if let Some(a) = view.controls(ui, "replay", &timeline) {
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
        let draw = self.pose_draw(hand);
        let mode = self.body_tracking().mode;
        let active = self.replay.is_none();
        let ctx = ui.ctx().clone();
        let view = self.shot_player.as_mut()?;
        let timeline = view.prepare(&ctx, &self.pending, active, mode);
        // The playhead the pictures below are drawn at.
        let playhead = view.playback.as_ref().map(|p| p.playhead);
        let n = view.cameras.len().max(1) as f32;
        let gap = PLAYER_GAP;
        let available = ui.available_width();
        let aspect = ReplayView::aspect(&timeline);
        let column = ((available - gap * (n - 1.0)) / n)
            .min(max_picture / aspect)
            .max(1.0);
        let row = column * n + gap * (n - 1.0);
        let pad = ((available - row) / 2.0).max(0.0);
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add_space(pad);
            ui.vertical(|ui| {
                ui.set_width(row);
                view.columns(ui, "player", column, gap, draw);
                ui.add_space(8.0);
                action = view.controls(ui, "player", &timeline);
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
        .filter(|t| t.width > 0)
        .map_or(9.0 / 16.0, |t| t.height as f32 / t.width as f32);
    let rect = match tex {
        Some(tex) => {
            let shown = picture(ui, tex, width);
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
                    [track.width, track.height],
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

/// Most bytes of decoded frames a camera card keeps. Within it every frame
/// of the loop is decoded once and kept; past it the card keeps the replay
/// view's [`TEXTURES`] most recent frames.
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

    /// How many decoded frames it keeps: the whole loop when it fits
    /// [`CARD_TEXTURE_BYTES`] at `px`, else [`TEXTURES`].
    fn capacity(&self) -> usize {
        let frames = self.loop_frames();
        let bytes = frames.saturating_mul(self.px[0] * self.px[1] * 4);
        if frames > 0 && bytes <= CARD_TEXTURE_BYTES {
            frames
        } else {
            TEXTURES
        }
    }

    /// Whether every frame of the loop is kept once decoded.
    #[cfg(test)]
    pub(crate) fn keeps_every_frame(&self) -> bool {
        self.loop_frames() > 0 && self.capacity() == self.loop_frames()
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

    /// What to ask the server for: the sidecar, then the frames at the
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
                    .filter(|f| !self.textures.contains(f))
                    .collect()
            }
            _ => Vec::new(),
        };
        self.clip.due(&frames)
    }

    /// The frame to decode next, when its bytes are in. While the whole
    /// loop is kept, the first frame not decoded yet from the playhead on,
    /// in loop order (from the start point again past the end), so the
    /// loop fills in ahead of the playhead; else the frame at the playhead.
    pub(crate) fn next_to_decode(&self) -> Option<usize> {
        let p = self.playback.as_ref()?;
        let track = self.clip.track.as_ref()?;
        let frame = track.frame_at(p.playhead);
        let wanted = |f: &usize| {
            !self.textures.contains(f) && !self.undecodable.contains(f) && self.clip.bytes.has(*f)
        };
        if self.capacity() < self.loop_frames() {
            return Some(frame).filter(wanted);
        }
        let start = track.frame_at(p.start);
        let n = track.frames.len();
        (frame..n).chain(start..frame).find(wanted)
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

    /// The picture at the playhead: its decoded frame when it is kept, else
    /// the last picture shown. The clock starts once the start frame is on
    /// screen.
    fn frame_texture(&mut self) -> Option<egui::TextureHandle> {
        let p = self.playback.as_ref()?;
        let track = self.clip.track.as_ref()?;
        let frame = track.frame_at(p.playhead);
        let start = track.frame_at(p.start);
        if self.shown.as_ref().is_none_or(|(shown, _)| *shown != frame)
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

    /// Seconds until the next frame is due (the start point's again at the
    /// end of the loop), while the clock runs; never under
    /// [`CARD_REPAINT_MIN_S`].
    fn next_due(&self) -> Option<f64> {
        let (p, track) = (self.playback.as_ref()?, self.clip.track.as_ref()?);
        let (start, end) = self.loop_span()?;
        if !self.running || end <= start {
            return None;
        }
        let frame = track.frame_at(p.playhead);
        let next = if frame + 1 < track.frames.len() {
            track.time(frame + 1)
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
/// `tex` filling the body under it.
pub(crate) fn paint_camera_card(
    ui: &egui::Ui,
    rect: egui::Rect,
    name: &str,
    tex: Option<&egui::TextureHandle>,
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
        egui::Image::new((tex.id(), body.size()))
            .corner_radius(egui::CornerRadius {
                nw: 0,
                ne: 0,
                sw: r,
                se: r,
            })
            .paint_at(ui, body);
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
        let tex = self.card_clips.get_mut(camera).and_then(|card| {
            let size = card_body(rect).size();
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
        paint_camera_card(ui, rect, &name, tex.as_ref());
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
pub(crate) mod tests {
    use super::*;
    use crate::replay::tests::track;
    use crate::types::CameraPosition;

    const MS: u64 = 1_000_000;

    pub(crate) fn video(camera: &str) -> ShotVideo {
        ShotVideo {
            camera: camera.to_owned(),
            position: CameraPosition::GolferFront,
            duration_ms: 1000,
            fps: 100.0,
            trigger_offset_ms: 500,
            impact_offset_ms: 500,
            complete: true,
            dropped: 0,
            pose: false,
            swing: false,
        }
    }

    pub(crate) fn shot(videos: &[&str]) -> ShotRow {
        ShotRow {
            actor: "mock_monitor.0".into(),
            shot_id: "s1".into(),
            shot_number: 7,
            ball: None,
            club: None,
            impact: None,
            derived: None,
            club_selected: None,
            handed: None,
            group: None,
            finished: true,
            video: videos.iter().map(|c| video(c)).collect(),
        }
    }

    fn sidecar(view: &ReplayView, camera: &str, t: &Track) -> ReplayReply {
        let json = serde_json::json!({
            "version": 1, "fourcc": "MJPG", "width": t.width, "height": t.height,
            "fps": {"nominal": null, "measured": 100.0},
            "start_ns": t.start_ns,
            "frames": t.frames,
            "trigger_index": 0, "impact_index": 0, "trigger_ns": 0,
            "impact_ns": i64::try_from(t.impact_ns.unwrap() - t.start_ns).unwrap(),
            "complete": true, "dropped": 0, "truncated": false,
        });
        ReplayReply::Sidecar {
            replay: view.seq,
            camera: camera.to_owned(),
            result: stance::sidecar::Sidecar::from_json(&json.to_string())
                .map_err(|e| e.to_string()),
        }
    }

    #[test]
    fn the_inline_player_fetches_each_swing_analysis_once_and_the_modal_none() {
        let swings = |due: &[Fetch]| {
            due.iter()
                .filter_map(|f| match f {
                    Fetch::Swing(c) => Some(c.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let mut row = shot(&["camera.0", "camera.1"]);
        row.video[0].swing = true;
        let mut modal = ReplayView::new(1, &row, str::to_owned, false, 0.2);
        modal.mode = BodyTrackingMode::Historical;
        assert!(swings(&modal.due()).is_empty(), "the modal shows none");
        modal.swing_ready("camera.1");
        assert!(swings(&modal.due()).is_empty());
        let mut inline = ReplayView::new(2, &row, str::to_owned, false, 0.2).with_swing();
        inline.mode = BodyTrackingMode::Historical;
        assert_eq!(
            swings(&inline.due()),
            ["camera.0"],
            "the analysed clip only"
        );
        assert!(swings(&inline.due()).is_empty(), "once");
        inline.swing_ready("camera.1");
        assert_eq!(swings(&inline.due()), ["camera.1"]);
        // A reply for another player is not taken; a failed one leaves none.
        let report: crate::types::SwingReportFile = serde_json::from_str(
            r#"{"version":1,"setup":{"view":"face_on"},"events":{},"metrics":[]}"#,
        )
        .unwrap();
        inline.reply(ReplayReply::Swing {
            replay: 1,
            camera: "camera.0".into(),
            result: Ok(report.clone()),
        });
        assert!(inline.cameras[0].swing.report.is_none());
        inline.reply(ReplayReply::Swing {
            replay: 2,
            camera: "camera.0".into(),
            result: Ok(report),
        });
        inline.reply(ReplayReply::Swing {
            replay: 2,
            camera: "camera.1".into(),
            result: Err("404".into()),
        });
        assert!(inline.cameras[0].swing.report.is_some());
        assert!(inline.cameras[1].swing.report.is_none());
    }

    #[test]
    fn a_clip_without_keypoints_is_asked_to_be_tracked_once_unless_tracking_is_off() {
        let asks = |due: &[Fetch]| {
            due.iter()
                .filter_map(|f| match f {
                    Fetch::Analyze(c) => Some(c.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let fetched = |due: &[Fetch]| {
            due.iter()
                .filter(|f| matches!(f, Fetch::Pose(_) | Fetch::Swing(_)))
                .count()
        };
        let mut row = shot(&["camera.0", "camera.1"]);
        row.video[1].pose = true;
        row.video[1].swing = true;
        // Off: nothing asked for or fetched, whatever is on disk.
        let mut view = ReplayView::new(1, &row, str::to_owned, false, 0.2).with_swing();
        view.mode = BodyTrackingMode::Off;
        let due = view.due();
        assert!(asks(&due).is_empty());
        assert_eq!(fetched(&due), 0);
        // Historical, and Live alike: the clip without keypoints is asked
        // for once; the tracked one's files are fetched.
        for mode in [BodyTrackingMode::Historical, BodyTrackingMode::Live] {
            let mut view = ReplayView::new(1, &row, str::to_owned, false, 0.2).with_swing();
            view.mode = mode;
            let due = view.due();
            assert_eq!(asks(&due), ["camera.0"], "{mode:?}");
            assert_eq!(fetched(&due), 2, "{mode:?}");
            assert!(asks(&view.due()).is_empty(), "once");
            // Queued: its keypoints follow with shot_pose.
            view.reply(ReplayReply::Analyzed {
                replay: 1,
                camera: "camera.0".into(),
                done: false,
            });
            assert_eq!(fetched(&view.due()), 0);
            view.pose_ready("camera.0");
            assert_eq!(fetched(&view.due()), 1);
        }
        // Asked again (the models were just downloaded): only the clip
        // without keypoints, once.
        let mut view = ReplayView::new(2, &row, str::to_owned, false, 0.2).with_swing();
        view.mode = BodyTrackingMode::Historical;
        assert_eq!(asks(&view.due()), ["camera.0"]);
        assert!(asks(&view.due()).is_empty());
        view.ask_again();
        assert_eq!(asks(&view.due()), ["camera.0"]);
        assert!(asks(&view.due()).is_empty());
        // Done already (the row was behind): fetched at once.
        let mut view = ReplayView::new(3, &row, str::to_owned, false, 0.2).with_swing();
        view.mode = BodyTrackingMode::Historical;
        assert_eq!(asks(&view.due()), ["camera.0"]);
        view.reply(ReplayReply::Analyzed {
            replay: 3,
            camera: "camera.0".into(),
            done: true,
        });
        assert_eq!(fetched(&view.due()), 2, "keypoints and swing");
        // A reply for another view changes nothing.
        let mut other = ReplayView::new(4, &row, str::to_owned, false, 0.2);
        other.mode = BodyTrackingMode::Historical;
        let _ = other.due();
        other.reply(ReplayReply::Analyzed {
            replay: 3,
            camera: "camera.0".into(),
            done: true,
        });
        assert!(!other.cameras[0].video.pose);
    }

    #[test]
    fn a_tracked_clips_keypoints_are_fetched_once_and_drawn_on_the_frame_shown() {
        let t = track(1000, 10, 50, 100);
        let mut row = shot(&["camera.0", "camera.1"]);
        row.video[0].pose = true;
        let mut view = ReplayView::new(1, &row, str::to_owned, false, 0.2);
        view.mode = BodyTrackingMode::Historical;
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let poses = |due: &[Fetch]| {
            due.iter()
                .filter_map(|f| match f {
                    Fetch::Pose(c) => Some(c.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(poses(&view.due()), ["camera.0"], "the tracked clip only");
        assert!(poses(&view.due()).is_empty(), "once");
        // The other camera's tracking finishes while the replay is open.
        view.pose_ready("camera.1");
        assert_eq!(poses(&view.due()), ["camera.1"]);
        // Keypoints for every other frame (frame k at k × 10 ms).
        let frames: Vec<serde_json::Value> = t
            .frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                if i % 2 == 0 {
                    serde_json::json!({"t_ns": f.t_ns, "keypoints": vec![[i as f32, 1.0, 0.9]; 26]})
                } else {
                    serde_json::json!({"t_ns": f.t_ns})
                }
            })
            .collect();
        let file = serde_json::json!({"version": 1, "skeleton": "halpe26", "width": 64,
            "height": 48, "frames": frames});
        view.reply(ReplayReply::Pose {
            replay: 1,
            camera: "camera.0".into(),
            result: serde_json::from_value(file).map_err(|e| e.to_string()),
        });
        let ctx = egui::Context::default();
        let tex = ctx.load_texture(
            "t",
            egui::ColorImage::filled([2, 2], egui::Color32::BLACK),
            egui::TextureOptions::LINEAR,
        );
        let cam = &mut view.cameras[0];
        for (frame, nose_x) in [(10, Some(10.0)), (11, None), (24, Some(24.0))] {
            cam.shown = Some((frame, tex.clone()));
            assert_eq!(
                cam.shown_keypoints().map(|k| k[0][0]),
                nose_x,
                "frame {frame}"
            );
        }
        // No keypoints for a camera whose file is not in.
        view.cameras[1].shown = Some((10, tex));
        assert!(view.cameras[1].shown_keypoints().is_none());
    }

    /// The centres of the circles `out` painted inside `rect`: a skeleton's
    /// joints.
    fn joints_in(out: &egui::FullOutput, rect: egui::Rect) -> usize {
        fn count(shape: &egui::Shape, rect: egui::Rect) -> usize {
            match shape {
                egui::Shape::Circle(c) => usize::from(rect.contains(c.center)),
                egui::Shape::Vec(v) => v.iter().map(|s| count(s, rect)).sum(),
                _ => 0,
            }
        }
        out.shapes.iter().map(|c| count(&c.shape, rect)).sum()
    }

    #[test]
    fn a_tracked_clip_shows_its_skeleton_only_while_show_skeleton_is_on() {
        let t = track(1000, 10, 50, 100);
        let mut row = shot(&["camera.0"]);
        row.video[0].pose = true;
        let mut view = ReplayView::new(1, &row, str::to_owned, false, 0.2);
        view.mode = BodyTrackingMode::Historical;
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let frames: Vec<serde_json::Value> = t
            .frames
            .iter()
            .map(|f| serde_json::json!({"t_ns": f.t_ns, "keypoints": vec![[32.0, 24.0, 0.9]; 26]}))
            .collect();
        let file = serde_json::json!({"version": 1, "skeleton": "halpe26", "width": 64,
            "height": 48, "frames": frames});
        view.reply(ReplayReply::Pose {
            replay: 1,
            camera: "camera.0".into(),
            result: serde_json::from_value(file).map_err(|e| e.to_string()),
        });
        let ctx = egui::Context::default();
        let tex = ctx.load_texture(
            "t",
            egui::ColorImage::filled([2, 2], egui::Color32::BLACK),
            egui::TextureOptions::LINEAR,
        );
        let playhead = view.playback.as_ref().expect("playback").playhead;
        let frame = t.frame_at(playhead);
        // The inline player and the replay both draw as the app says.
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        let mut config = flighthook::FlighthookConfig::default();
        config.body_tracking.mode = BodyTrackingMode::Historical;
        for show in [true, false, true] {
            config.body_tracking.show_skeleton = show;
            app.settings.load_from(&config);
            let draw = app.pose_draw(crate::types::Handedness::Right);
            assert_eq!(draw.overlay, show);
            view.cameras[0].shown = Some((frame, tex.clone()));
            let (marks, out) = columns_pass_drawing(&ctx, &mut view, Vec::new(), 0.0, draw);
            let picture = marks["player:picture:camera.0"].0;
            assert_eq!(joints_in(&out, picture) > 0, show, "show_skeleton {show}");
        }
    }

    #[test]
    fn a_replay_opens_playing_before_the_impact_once_a_sidecar_is_in() {
        // The clip runs 1000 to 2000 ms with the impact at 1500 ms.
        let t = track(1000, 10, 100, 50);
        let impact = t.impact_ns.unwrap();
        let mut view = ReplayView::new(
            3,
            &shot(&["camera.0", "camera.1"]),
            str::to_owned,
            false,
            0.3,
        );
        assert!(view.playback.is_none());
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let p = view.playback.as_ref().expect("playback");
        assert_eq!(p.playhead, impact - 300_000_000);
        assert!(p.playing);
        // A reply for another replay is dropped.
        let mut stale = sidecar(&view, "camera.1", &t);
        if let ReplayReply::Sidecar { replay, .. } = &mut stale {
            *replay = 2;
        }
        view.reply(stale);
        assert!(view.cameras[1].clip.track.is_none());
        // A failed sidecar shows on its camera.
        view.reply(ReplayReply::Sidecar {
            replay: 3,
            camera: "camera.1".into(),
            result: Err("404 no clip".into()),
        });
        assert_eq!(view.cameras[1].clip.error.as_deref(), Some("404 no clip"));
    }

    /// The reply carrying the frames of `window` of `t`.
    fn frames_reply(view: &ReplayView, camera: &str, t: &Track, window: Window) -> ReplayReply {
        let (a, b) = window.bytes(t);
        ReplayReply::Frames {
            replay: view.seq,
            camera: camera.to_owned(),
            window,
            result: Ok(vec![0; usize::try_from(b - a).unwrap()]),
        }
    }

    /// A view of `cameras` whose first sidecar is in, opened 0.3 s before the
    /// impact of a clip from 1000 to 1990 ms; the sidecars of `loaded` are in.
    fn opened(cameras: &[&str], loaded: &[&str]) -> (ReplayView, Track, (u64, u64)) {
        let t = track(1000, 10, 100, 50);
        let span = (t.start_ns, t.end_ns());
        let mut view = ReplayView::new(1, &shot(cameras), str::to_owned, false, 0.3);
        // The frames alone.
        view.mode = BodyTrackingMode::Off;
        for camera in loaded {
            let reply = sidecar(&view, camera, &t);
            view.reply(reply);
        }
        (view, t, span)
    }

    fn playhead(view: &ReplayView) -> u64 {
        view.playback.as_ref().unwrap().playhead
    }

    #[test]
    fn the_playhead_holds_until_the_start_frames_are_in_then_the_clip_plays() {
        let (mut view, t, span) = opened(&["camera.0", "camera.1"], &["camera.0"]);
        let start = playhead(&view);
        assert!(view.playback.as_ref().unwrap().playing, "shown playing");
        // Neither the other camera's sidecar nor the frames are in: it holds.
        for i in 0..10 {
            view.tick(f64::from(i) * 0.1, Some(span));
            assert_eq!(playhead(&view), start);
        }
        // The other camera's sidecar is in; its frames are not.
        let reply = sidecar(&view, "camera.1", &t);
        view.reply(reply);
        let due = view.due();
        let windows: Vec<_> = due
            .iter()
            .filter_map(|f| match f {
                Fetch::Frames(c, w, _) => Some((c.clone(), *w)),
                Fetch::Sidecar(_) | Fetch::Pose(_) | Fetch::Swing(_) | Fetch::Analyze(_) => None,
            })
            .collect();
        assert_eq!(windows.len(), 2, "{due:?}");
        view.tick(1.0, Some(span));
        assert_eq!(playhead(&view), start);
        let (camera, window) = windows[0].clone();
        let reply = frames_reply(&view, &camera, &t, window);
        view.reply(reply);
        view.tick(1.1, Some(span));
        assert_eq!(playhead(&view), start, "one camera is still loading");
        let (camera, window) = windows[1].clone();
        let reply = frames_reply(&view, &camera, &t, window);
        view.reply(reply);
        view.tick(1.2, Some(span));
        assert_eq!(playhead(&view), start, "the clock starts on this update");
        view.tick(1.3, Some(span));
        assert_eq!(playhead(&view) - start, 100 * MS);
        assert!(view.playback.as_ref().unwrap().playing);
        assert_eq!(view.autoplay, Autoplay::Done);
    }

    #[test]
    fn a_slow_camera_does_not_hold_the_start_past_the_timeout() {
        let (mut view, _, span) = opened(&["camera.0"], &["camera.0"]);
        let start = playhead(&view);
        view.tick(10.0, Some(span));
        view.tick(10.0 + AUTOPLAY_TIMEOUT_S - 0.1, Some(span));
        assert_eq!(playhead(&view), start);
        view.tick(10.0 + AUTOPLAY_TIMEOUT_S, Some(span));
        assert_eq!(playhead(&view), start);
        view.tick(10.0 + AUTOPLAY_TIMEOUT_S + 0.1, Some(span));
        let moved = playhead(&view) - start;
        assert!(moved.abs_diff(100 * MS) < 1000, "{moved}");
    }

    #[test]
    fn a_camera_that_failed_does_not_hold_the_start() {
        let (mut view, _, span) = opened(&["camera.0", "camera.1"], &["camera.0"]);
        let start = playhead(&view);
        view.reply(ReplayReply::Sidecar {
            replay: 1,
            camera: "camera.1".into(),
            result: Err("404 no clip".into()),
        });
        // camera.0's frames fail twice.
        for _ in 0..WINDOW_TRIES {
            let due = view.due();
            let window = due
                .iter()
                .find_map(|f| match f {
                    Fetch::Frames(_, w, _) => Some(*w),
                    Fetch::Sidecar(_) | Fetch::Pose(_) | Fetch::Swing(_) | Fetch::Analyze(_) => {
                        None
                    }
                })
                .expect("a window");
            view.reply(ReplayReply::Frames {
                replay: 1,
                camera: "camera.0".into(),
                window,
                result: Err("timed out".into()),
            });
        }
        assert!(view.start_frames_in());
        view.tick(0.0, Some(span));
        view.tick(0.1, Some(span));
        assert_eq!(playhead(&view) - start, 100 * MS);
    }

    #[test]
    fn a_pause_step_or_scrub_before_the_start_cancels_the_autoplay() {
        for action in [
            ReplayAction::Toggle,
            ReplayAction::Step(true),
            ReplayAction::Step(false),
            ReplayAction::Seek(1500 * MS),
        ] {
            let (mut view, t, span) = opened(&["camera.0"], &["camera.0"]);
            view.tick(0.0, Some(span));
            view.apply(action, span, std::slice::from_ref(&t));
            let p = view.playback.as_ref().unwrap();
            assert!(!p.playing);
            let held = p.playhead;
            // The frames arriving afterwards start nothing.
            let due = view.due();
            let [_, Fetch::Frames(camera, window, _)] = due.as_slice() else {
                panic!("{due:?}");
            };
            let reply = frames_reply(&view, camera, &t, *window);
            view.reply(reply);
            for i in 1..30 {
                view.tick(f64::from(i) * 0.1, Some(span));
            }
            assert_eq!(playhead(&view), held);
            assert!(!view.playback.as_ref().unwrap().playing);
        }
        // Speed and Loop leave the wait alone.
        let (mut view, _, span) = opened(&["camera.0"], &["camera.0"]);
        view.apply(ReplayAction::Speed(Speed::Half), span, &[]);
        view.apply(ReplayAction::Loop(false), span, &[]);
        let p = view.playback.as_ref().unwrap();
        assert!(p.playing && !p.looping && p.speed == Speed::Half);
        assert!(matches!(view.autoplay, Autoplay::Waiting { .. }));
    }

    #[test]
    fn the_opening_position_clamps_to_the_clip_start_and_play_restarts_there() {
        let t = track(1000, 10, 100, 50);
        let span = (t.start_ns, t.end_ns());
        // 2 s before an impact 0.5 s into the clip: the clip start.
        let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 2.0);
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let p = view.playback.as_mut().expect("playback");
        assert_eq!(p.playhead, span.0);
        // Zero is the impact itself.
        let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let p = view.playback.as_mut().expect("playback");
        assert_eq!(p.playhead, t.impact_ns.unwrap());
        // Played to the end with looping off, play restarts at the impact,
        // scrubbing still reaches the clip start.
        p.looping = false;
        assert!(p.playing, "it opened playing");
        p.advance(5.0, span);
        assert_eq!((p.playhead, p.playing), (span.1, false));
        p.toggle(span);
        assert_eq!(p.playhead, t.impact_ns.unwrap());
        p.playing = false;
        p.seek(0, span);
        assert_eq!(p.playhead, span.0);
    }

    #[test]
    fn the_start_tick_is_at_the_start_point_clamped_to_the_clip() {
        let span = (1000 * MS, 3000 * MS);
        let x = |t| tick_x(10.0, 200.0, span, t);
        assert_eq!(x(span.0), 10.0);
        assert_eq!(x(2000 * MS), 110.0);
        assert_eq!(x(span.1), 210.0);
        assert_eq!(x(0), 10.0, "before the clip: its start");
        assert_eq!(x(9000 * MS), 210.0, "after the clip: its end");
        // A replay whose start point clamps to the clip start ticks there.
        let t = track(1000, 10, 100, 50);
        let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 2.0);
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let p = view.playback.as_ref().expect("playback");
        assert_eq!(tick_x(10.0, 200.0, (t.start_ns, t.end_ns()), p.start), 10.0);
        // 0.3 s before the 1.5 s impact is 200 ms into the 990 ms clip.
        let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.3);
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let p = view.playback.as_ref().expect("playback");
        let x = tick_x(0.0, 990.0, (t.start_ns, t.end_ns()), p.start);
        assert!((x - 200.0).abs() < 1e-3, "{x}");
    }

    #[test]
    fn a_replay_loops_by_default_and_the_toggle_turns_it_off() {
        let t = track(1000, 10, 100, 50);
        let span = (t.start_ns, t.end_ns());
        let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.3);
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let p = view.playback.as_mut().expect("playback");
        assert!(p.looping, "on whenever a replay opens");
        let start = p.start;
        assert!(p.playing);
        p.advance(5.0, span);
        assert_eq!((p.playhead, p.playing), (start, true));
        // A new replay is on again, whatever the last one was set to.
        p.looping = false;
        let mut again = ReplayView::new(2, &shot(&["camera.0"]), str::to_owned, false, 0.3);
        let reply = sidecar(&again, "camera.0", &t);
        again.reply(reply);
        assert!(again.playback.as_ref().expect("playback").looping);
    }

    #[test]
    fn frames_are_fetched_once_and_held_after_their_reply() {
        let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
        let t = track(0, 10, 100, 50);
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let frame = t.frame_at(view.playback.as_ref().unwrap().playhead);
        let cam = &mut view.cameras[0].clip;
        let window = cam.wanted(frame).expect("a window to fetch");
        assert!(window.contains(frame));
        cam.in_flight.push(window);
        assert!(cam.wanted(frame).is_none(), "already on its way");
        let (a, b) = window.bytes(&t);
        view.reply(ReplayReply::Frames {
            replay: 1,
            camera: "camera.0".into(),
            window,
            result: Ok(vec![0; usize::try_from(b - a).unwrap()]),
        });
        let cam = &view.cameras[0].clip;
        assert!(cam.in_flight.is_empty());
        assert!(cam.bytes.has(frame));
        assert!(cam.wanted(frame).is_none(), "held");
    }

    #[test]
    fn the_view_asks_for_each_sidecar_once_then_the_frames_at_the_playhead() {
        let mut view = ReplayView::new(
            1,
            &shot(&["camera.0", "camera.1"]),
            str::to_owned,
            false,
            0.0,
        );
        view.mode = BodyTrackingMode::Off;
        assert_eq!(
            view.due(),
            [
                Fetch::Sidecar("camera.0".into()),
                Fetch::Sidecar("camera.1".into())
            ]
        );
        assert!(view.due().is_empty(), "asked once");
        let t = track(0, 10, 100, 50);
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        let due = view.due();
        let [Fetch::Frames(camera, window, bytes)] = due.as_slice() else {
            panic!("{due:?}");
        };
        assert_eq!(camera, "camera.0");
        let frame = t.frame_at(view.playback.as_ref().unwrap().playhead);
        assert!(window.contains(frame));
        assert_eq!(*bytes, window.bytes(&t));
        assert!(view.due().is_empty(), "in flight");
        // Playing, it also asks ahead of the playhead.
        let p = view.playback.as_mut().unwrap();
        p.playing = true;
        p.playhead += 600_000_000;
        let due = view.due();
        assert!(!due.is_empty() && due.len() <= MAX_IN_FLIGHT);
    }

    #[test]
    fn a_failed_window_is_retried_once_then_its_error_is_shown() {
        let mut view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
        view.mode = BodyTrackingMode::Off;
        assert_eq!(view.due(), [Fetch::Sidecar("camera.0".into())]);
        let t = track(0, 10, 100, 50);
        let reply = sidecar(&view, "camera.0", &t);
        view.reply(reply);
        // Paused: only the window at the playhead is asked for.
        view.playback.as_mut().unwrap().playing = false;
        let fail = |view: &mut ReplayView, window| {
            view.reply(ReplayReply::Frames {
                replay: 1,
                camera: "camera.0".into(),
                window,
                result: Err("timed out".into()),
            });
        };
        let due = view.due();
        let [Fetch::Frames(_, window, _)] = due.as_slice() else {
            panic!("{due:?}");
        };
        let window = *window;
        fail(&mut view, window);
        assert_eq!(view.camera_error(0), None, "one failure is retried");
        let due = view.due();
        let [Fetch::Frames(_, again, _)] = due.as_slice() else {
            panic!("{due:?}");
        };
        assert_eq!(*again, window);
        fail(&mut view, window);
        assert_eq!(view.camera_error(0).as_deref(), Some("timed out"));
        assert!(view.due().is_empty(), "not asked for a third time");
        // Elsewhere in the clip the camera still loads and shows.
        let p = view.playback.as_mut().unwrap();
        p.playhead = t.time(99);
        assert_eq!(view.camera_error(0), None);
        let due = view.due();
        let [Fetch::Frames(_, other, bytes)] = due.as_slice() else {
            panic!("{due:?}");
        };
        assert!(other.contains(99));
        let (a, b) = *bytes;
        view.reply(ReplayReply::Frames {
            replay: 1,
            camera: "camera.0".into(),
            window: *other,
            result: Ok(vec![0; usize::try_from(b - a).unwrap()]),
        });
        assert_eq!(view.camera_error(0), None);
        // A clip that didn't load shows why at any playhead.
        view.cameras[0].clip.error = Some("404 no clip".into());
        assert_eq!(view.camera_error(0).as_deref(), Some("404 no clip"));
    }

    /// A clip of `n` test-pattern frames 10 ms apart from 1000 ms (impact
    /// half way), and the bytes of the file it indexes.
    fn jpeg_clip(n: u64) -> (Track, Vec<u8>) {
        use stance::{Source, TestPattern, TestPatternConfig};
        let mut pattern = TestPattern::new(TestPatternConfig {
            width: 64,
            height: 48,
            paced: false,
            ..TestPatternConfig::default()
        })
        .unwrap();
        let mut file = vec![0; 232];
        let mut t = track(1000, 10, n, 1);
        for f in &mut t.frames {
            let jpeg = pattern
                .next_frame(std::time::Duration::ZERO)
                .unwrap()
                .unwrap()
                .jpeg;
            f.offset = file.len() as u64;
            f.len = u32::try_from(jpeg.len()).unwrap();
            file.extend_from_slice(&jpeg);
            file.extend_from_slice(&[0; 8]);
        }
        (t, file)
    }

    /// The card's reply to each window it asks for, from `file`.
    fn serve(card: &mut CardClip, t: &Track, file: &[u8]) -> usize {
        let due = card.due();
        let mut served = 0;
        for fetch in due {
            if let Fetch::Frames(camera, window, (a, b)) = fetch {
                assert_eq!(window.bytes(t), (a, b));
                card.reply(ReplayReply::Frames {
                    replay: card.seq,
                    camera,
                    window,
                    result: Ok(file[a as usize..b as usize].to_vec()),
                });
                served += 1;
            }
        }
        served
    }

    #[test]
    fn a_camera_card_loops_from_the_start_point_with_frames_cut_to_fill_it() {
        let ctx = egui::Context::default();
        let (t, file) = jpeg_clip(40);
        let mut card = CardClip::new(9, "s1", "camera.0", 0.1);
        assert_eq!(card.due(), [Fetch::Sidecar("camera.0".into())]);
        assert!(card.due().is_empty(), "asked once");
        assert!(!card.decode_next(&ctx));
        assert!(card.frame_texture().is_none());
        let view = ReplayView::new(9, &shot(&["camera.0"]), str::to_owned, false, 0.0);
        let reply = sidecar(&view, "camera.0", &t);
        card.reply(reply);
        // From 0.1 s before the impact to the clip end.
        let (start, end) = card.loop_span().expect("a loop");
        assert_eq!(start, t.impact_ns.unwrap() - 100 * MS);
        assert_eq!(end, t.end_ns() + 10 * MS, "the last frame's end");
        // A tall card: the 64x48 frames are cut to its aspect.
        card.resize([40, 60]);
        assert!(card.keeps_every_frame());
        assert!(card.next_due().is_none(), "nothing on screen yet");
        assert!(serve(&mut card, &t, &file) > 0);
        assert!(card.frame_texture().is_none(), "not decoded yet");
        assert!(card.decode_next(&ctx));
        let tex = card.frame_texture().expect("the start frame");
        assert_eq!(tex.size(), [40, 60]);
        assert!(card.running, "the clock starts with the start frame");
        assert_eq!(card.playback.as_ref().unwrap().playhead, start);
        // 1x from here, one repaint per frame.
        card.tick(5.0);
        card.tick(5.0625);
        let p = card.playback.as_ref().unwrap();
        assert_eq!(p.playhead, start + 62_500_000);
        // The next frame (1170 ms) is due in 7.5 ms, sooner than a 60 Hz
        // display shows it: the repaint waits a display frame.
        let due = card.next_due().expect("playing");
        assert!((due - CARD_REPAINT_MIN_S).abs() < 1e-9, "{due}");
        // The last frame shows until the loop's end, then the start point.
        let p = card.playback.as_mut().unwrap();
        p.playhead = t.end_ns() + 2 * MS;
        assert_eq!(t.frame_at(p.playhead), 39);
        let due = card.next_due().expect("playing");
        assert!((due - CARD_REPAINT_MIN_S).abs() < 1e-9, "{due}");
        card.tick(5.5);
        assert_eq!(card.playback.as_ref().unwrap().playhead, start);
        // Every frame decoded once is kept: no window is asked for again.
        for step in 0..60 {
            card.tick(5.4 + f64::from(step) * 0.01);
            serve(&mut card, &t, &file);
            card.decode_next(&ctx);
            card.frame_texture();
        }
        assert_eq!(serve(&mut card, &t, &file), 0);
        assert_eq!(card.textures.len(), card.loop_frames());
    }

    #[test]
    fn a_camera_cards_decoded_frames_are_bounded() {
        let (t, _) = jpeg_clip(40);
        let mut card = CardClip::new(1, "s1", "camera.0", 0.1);
        let view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
        let reply = sidecar(&view, "camera.0", &t);
        card.reply(reply);
        // 30 frames from the start point (1100 ms) to the end.
        assert_eq!(card.loop_frames(), 30);
        card.resize([400, 400]);
        assert!(card.keeps_every_frame(), "30 x 640 kB");
        assert_eq!(card.textures.capacity(), 30);
        // 48 MiB holds 30 frames of 646 x 646, not of 648 x 648: past it,
        // the replay view's window instead.
        const { assert!(30 * 646 * 646 * 4 <= CARD_TEXTURE_BYTES) };
        const { assert!(30 * 648 * 648 * 4 > CARD_TEXTURE_BYTES) };
        card.resize([646, 646]);
        assert!(card.keeps_every_frame());
        card.resize([648, 648]);
        assert!(!card.keeps_every_frame());
        assert_eq!(card.textures.capacity(), TEXTURES);
        // Another reply's seq is not this card's.
        let mut other = sidecar(&view, "camera.0", &t);
        if let ReplayReply::Sidecar { replay, .. } = &mut other {
            *replay = 2;
        }
        let mut fresh = CardClip::new(1, "s1", "camera.0", 0.1);
        fresh.reply(other);
        assert!(fresh.loop_span().is_none());
    }

    /// A card of `camera` (seq `seq`) with the sidecar of `t` in and every
    /// frame's bytes held.
    fn loaded_card(seq: u64, camera: &str, t: &Track, file: &[u8]) -> CardClip {
        let mut card = CardClip::new(seq, "s1", camera, 0.1);
        let view = ReplayView::new(seq, &shot(&[camera]), str::to_owned, false, 0.0);
        card.reply(sidecar(&view, camera, t));
        card.resize([32, 24]);
        // Walk the loop so every window is asked for and served.
        for step in 0..t.frames.len() {
            if let Some(p) = card.playback.as_mut() {
                p.playhead = t.time(step);
            }
            serve(&mut card, t, file);
        }
        if let Some(p) = card.playback.as_mut() {
            p.playhead = p.start;
        }
        card
    }

    #[test]
    fn a_card_decodes_its_loop_in_order_ahead_of_the_playhead() {
        let ctx = egui::Context::default();
        let (t, file) = jpeg_clip(40);
        let mut card = loaded_card(1, "camera.0", &t, &file);
        assert!(card.keeps_every_frame());
        let start = t.frame_at(card.playback.as_ref().unwrap().start);
        // The frame at the playhead first, then on through the loop.
        let mut order = Vec::new();
        while let Some(frame) = card.next_to_decode() {
            order.push(frame);
            assert!(card.decode_next(&ctx));
        }
        assert_eq!(order, (start..40).collect::<Vec<_>>());
        // With the playhead further on, from there, wrapping to the start.
        let mut card = loaded_card(1, "camera.0", &t, &file);
        card.playback.as_mut().unwrap().playhead = t.time(30);
        let mut order = Vec::new();
        while let Some(frame) = card.next_to_decode() {
            order.push(frame);
            assert!(card.decode_next(&ctx));
        }
        let want: Vec<usize> = (30..40).chain(start..30).collect();
        assert_eq!(order, want);
        // Past the texture budget, with a loop longer than the replay
        // view's window, only the frame at the playhead.
        let (t, file) = jpeg_clip(80);
        let mut card = loaded_card(1, "camera.0", &t, &file);
        card.resize([648, 648]);
        assert!(!card.keeps_every_frame() && card.loop_frames() > TEXTURES);
        card.playback.as_mut().unwrap().playhead = t.time(30);
        assert_eq!(card.next_to_decode(), Some(30));
        assert!(card.decode_next(&ctx));
        assert_eq!(card.next_to_decode(), None);
    }

    #[test]
    fn the_cards_decode_one_frame_per_screen_update_between_them() {
        let ctx = egui::Context::default();
        let (t, file) = jpeg_clip(40);
        let mut app = FlighthookApp::with_pending(net::new_pending());
        for (seq, camera) in [(1, "camera.0"), (2, "camera.1"), (3, "camera.2")] {
            let card = loaded_card(seq, camera, &t, &file);
            app.card_clips.insert(camera.to_owned(), card);
        }
        let decoded = |app: &FlighthookApp| -> Vec<usize> {
            ["camera.0", "camera.1", "camera.2"]
                .map(|c| app.card_clips[c].textures.len())
                .to_vec()
        };
        // One decode per update across all cards, taking turns.
        for n in 1..=9_usize {
            app.decode_card_frame(&ctx);
            let counts = decoded(&app);
            assert_eq!(counts.iter().sum::<usize>(), n, "{counts:?}");
            assert!(counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1);
        }
        assert_eq!(decoded(&app), [3, 3, 3]);
        // A card with nothing left to decode doesn't take a turn.
        let loop_frames = app.card_clips["camera.0"].loop_frames();
        for _ in 0..3 * loop_frames {
            app.decode_card_frame(&ctx);
        }
        assert_eq!(decoded(&app), [loop_frames; 3]);
        app.decode_card_frame(&ctx);
        assert_eq!(decoded(&app), [loop_frames; 3]);
    }

    #[test]
    fn replies_reach_the_card_or_the_replay_their_seq_names() {
        let (t, _) = jpeg_clip(40);
        let mut app = FlighthookApp::with_pending(net::new_pending());
        let row = shot(&["camera.0"]);
        app.card_clips
            .insert("camera.0".into(), CardClip::new(4, "s1", "camera.0", 0.1));
        app.replay = Some(ReplayView::new(
            7,
            &ShotRow {
                shot_id: "other".into(),
                ..row.clone()
            },
            str::to_owned,
            false,
            0.0,
        ));
        let tagged = |seq: u64| {
            let view = ReplayView::new(seq, &row, str::to_owned, false, 0.0);
            sidecar(&view, "camera.0", &t)
        };
        // The card's seq, while a replay with another is open: the card.
        app.pending.lock().unwrap().replay.push(tagged(4));
        app.apply_pending();
        assert!(app.card_clips["camera.0"].loop_span().is_some());
        assert!(app.replay.as_ref().unwrap().tracks().is_empty());
        // The replay's seq: the replay.
        app.pending.lock().unwrap().replay.push(tagged(7));
        app.apply_pending();
        assert_eq!(app.replay.as_ref().unwrap().tracks().len(), 1);
        // A retired seq (a card since replaced): nowhere.
        app.card_clips
            .insert("camera.0".into(), CardClip::new(5, "s1", "camera.0", 0.1));
        app.pending.lock().unwrap().replay.push(tagged(4));
        app.apply_pending();
        assert!(app.card_clips["camera.0"].loop_span().is_none());
        assert_eq!(app.replay.as_ref().unwrap().tracks().len(), 1);
    }

    #[test]
    fn the_texture_cache_is_smaller_on_a_small_screen() {
        let view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, true, 0.0);
        assert_eq!(view.textures.len(), 0);
        assert_eq!(view.textures.capacity(), TEXTURES_SMALL);
        let view = ReplayView::new(1, &shot(&["camera.0"]), str::to_owned, false, 0.0);
        assert_eq!(view.textures.capacity(), TEXTURES);
    }

    // -----------------------------------------------------------------------
    // Swing events on the scrubber
    // -----------------------------------------------------------------------

    /// A swing report with `events` (name, ms after the clip start) and its
    /// impact source (`refined` found a swing, `given` did not).
    fn swing_report(events: &[(&str, u64)], source: &str) -> crate::types::SwingReportFile {
        let mut map = serde_json::Map::new();
        for (name, ms) in events {
            map.insert(
                (*name).to_owned(),
                serde_json::json!({"t_ns": ms * MS, "frame": 0, "confidence": 0.9}),
            );
        }
        map.insert("impact_source".into(), source.into());
        serde_json::from_value(serde_json::json!({
            "version": 1, "view": "face_on", "handed": "right", "events": map, "metrics": []
        }))
        .expect("report")
    }

    fn names(markers: &[Marker]) -> Vec<&'static str> {
        markers.iter().map(|m| m.0).collect()
    }

    /// Each event sits at the mean of the times the cameras found it at, on
    /// the shot timeline (a camera's clip start plus its event time); one
    /// camera's time stands alone; an event no camera found has no marker;
    /// impact is the gold tick's, and a camera that found no swing counts
    /// for nothing.
    #[test]
    fn a_marker_is_the_mean_of_the_cameras_that_found_the_event() {
        let a = swing_report(
            &[
                ("address", 100),
                ("takeaway", 150),
                ("top", 600),
                ("impact", 750),
                ("finish", 900),
            ],
            "refined",
        );
        let b = swing_report(&[("address", 120), ("top", 640)], "refined");
        let nothing = swing_report(&[("address", 5000), ("impact", 750)], "given");

        // Two cameras (clips starting 1000 and 1010 ms), an event missing
        // from one, and a third that found no swing.
        let markers = swing_markers([(1000 * MS, &a), (1010 * MS, &b), (1000 * MS, &nothing)]);
        assert_eq!(
            markers,
            [
                ("Address", 1115 * MS),
                ("Takeaway", 1150 * MS),
                ("Top", 1625 * MS),
                ("Finish", 1900 * MS),
            ]
        );
        // One camera: its own times, in swing order.
        let one = swing_markers([(2000 * MS, &b)]);
        assert_eq!(one, [("Address", 2120 * MS), ("Top", 2640 * MS)]);
        // A mean of times 1 ns apart rounds to the nearest ns.
        let mut c = swing_report(&[("top", 0)], "refined");
        let mut d = c.clone();
        c.events.top = Some(flighthook::SwingEvent {
            t_ns: 10,
            confidence: 1.0,
        });
        d.events.top = Some(flighthook::SwingEvent {
            t_ns: 11,
            confidence: 1.0,
        });
        assert_eq!(swing_markers([(0, &c), (0, &d)]), [("Top", 11)]);
        // No reports, or none that found a swing: no markers.
        assert!(swing_markers(std::iter::empty()).is_empty());
        assert!(swing_markers([(0, &nothing)]).is_empty());
        let empty = swing_report(&[], "refined");
        assert!(swing_markers([(0, &empty)]).is_empty());
    }

    /// Markers a few pixels apart share one spot, named in swing order and
    /// seeking to the first; ones further apart stay apart.
    #[test]
    fn close_markers_merge_into_one_spot_that_names_both() {
        let markers = [
            ("Address", 1000 * MS),
            ("Takeaway", 1010 * MS),
            ("Top", 1600 * MS),
            ("Finish", 1900 * MS),
        ];
        // 1 px per 10 ms: address and takeaway are 1 px apart.
        let x_of = |t: u64| t as f32 / (10.0 * MS as f32);
        let groups = marker_groups(&markers, x_of);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].0, ["Address", "Takeaway"]);
        assert_eq!(groups[0].1, 1000 * MS);
        assert_eq!(groups[1].0, ["Top"]);
        assert_eq!(groups[2].0, ["Finish"]);
        // 10 px per 10 ms: all four apart.
        let wide = marker_groups(&markers, |t| t as f32 / MS as f32);
        assert_eq!(wide.len(), 4);
        assert!(marker_groups(&[], x_of).is_empty());
    }

    /// One pass of `view`'s controls on a 800 wide screen, marked under
    /// `player`; the playhead after the pass.
    fn controls_pass(
        ctx: &egui::Context,
        view: &mut ReplayView,
        events: Vec<egui::Event>,
        time: f64,
    ) -> (crate::panels::devices::Marks, Vec<String>) {
        use crate::panels::devices::Marks;
        ctx.data_mut(|d| d.remove::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 200.0),
            )),
            time: Some(time),
            events,
            ..Default::default()
        };
        let pending = net::new_pending();
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let timeline = view.prepare(ctx, &pending, true, BodyTrackingMode::Historical);
                let action = view.controls(ui, "player", &timeline);
                view.act(action, &timeline);
            });
        });
        let marks = ctx
            .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default();
        let mut texts = Vec::new();
        fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
                _ => {}
            }
        }
        for clipped in &out.shapes {
            collect(&clipped.shape, &mut texts);
        }
        (marks, texts)
    }

    /// A view of two cameras with the clips from 1000 to 1990 ms (the
    /// second a 20 ms later start), each with its swing report.
    fn view_with_swings() -> ReplayView {
        let mut view = ReplayView::new(
            1,
            &shot(&["camera.0", "camera.1"]),
            str::to_owned,
            false,
            0.3,
        );
        view.mode = BodyTrackingMode::Historical;
        let (first, second) = (track(1000, 10, 100, 50), track(1020, 10, 100, 50));
        for (camera, t) in [("camera.0", &first), ("camera.1", &second)] {
            let reply = sidecar(&view, camera, t);
            view.reply(reply);
        }
        view
    }

    /// The scrubber carries a marker for each event the reports found, at
    /// the mean time, none for impact, hover names the event, and a click
    /// seeks to it.
    #[test]
    fn the_scrubber_marks_the_events_names_them_on_hover_and_seeks_on_a_click() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut view = view_with_swings();
        let events = [("address", 100), ("takeaway", 150), ("top", 500)];
        view.cameras[0].swing.report = Some(Arc::new(swing_report(&events, "refined")));
        let mut other = events.to_vec();
        other.pop();
        other.push(("finish", 800));
        view.cameras[1].swing.report = Some(Arc::new(swing_report(&other, "refined")));

        controls_pass(&ctx, &mut view, Vec::new(), 0.0);
        let (marks, _) = controls_pass(&ctx, &mut view, Vec::new(), 0.1);
        let mut found: Vec<&str> = marks
            .keys()
            .filter_map(|k| k.strip_prefix("player:event:"))
            .collect();
        found.sort_unstable();
        assert_eq!(found, ["address", "finish", "takeaway", "top"]);
        assert!(!marks.contains_key("player:event:impact"), "the gold tick");

        // The scrubber spans 1000 to 2010 ms; the track is inset 8 px.
        let scrubber = marks["player:scrubber"].0;
        let (left, width) = (scrubber.left() + 8.0, scrubber.width() - 16.0);
        let at = |ms: f32| left + (ms - 1000.0) / 1010.0 * width;
        let x = |name: &str| marks[&format!("player:event:{name}")].0.center().x;
        // Address: the mean of 1000+100 and 1020+100 = 1110 ms. Top: one
        // camera, 1000+500. Finish: one camera, 1020+800.
        assert!((x("address") - at(1110.0)).abs() < 0.6, "{}", x("address"));
        assert!((x("top") - at(1500.0)).abs() < 0.6);
        assert!((x("finish") - at(1820.0)).abs() < 0.6);
        assert!(x("address") < x("takeaway") && x("takeaway") < x("top"));
        for name in ["address", "takeaway", "top", "finish"] {
            let rect = marks[&format!("player:event:{name}")].0;
            assert!(rect.height() >= scrubber.height() - 0.5, "{name}");
        }

        // Hover names the event and nothing else.
        let top = marks["player:event:top"].0.center();
        let mut texts = Vec::new();
        for (i, t) in [0.0, 0.3, 1.2, 1.4].into_iter().enumerate() {
            let moved = if i == 0 {
                vec![egui::Event::PointerMoved(top)]
            } else {
                Vec::new()
            };
            texts = controls_pass(&ctx, &mut view, moved, 2.0 + t).1;
        }
        assert!(texts.iter().any(|t| t == "Top"), "{texts:?}");
        for other in ["Address", "Takeaway", "Finish", "Impact"] {
            assert!(!texts.iter().any(|t| t == other), "{other}: {texts:?}");
        }

        // A click seeks there.
        let button = |pressed| egui::Event::PointerButton {
            pos: top,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        controls_pass(&ctx, &mut view, vec![button(true)], 3.0);
        controls_pass(&ctx, &mut view, vec![button(false)], 3.1);
        assert_eq!(playhead(&view), 1500 * MS);
    }

    /// No marker without a swing report that found a swing, nor with body
    /// tracking off.
    #[test]
    fn no_markers_without_a_report_that_found_a_swing() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut view = view_with_swings();
        let has_events = |marks: &crate::panels::devices::Marks| {
            marks.keys().any(|k| k.starts_with("player:event:"))
        };
        let pass = |view: &mut ReplayView| {
            controls_pass(&ctx, view, Vec::new(), 0.0);
            controls_pass(&ctx, view, Vec::new(), 0.1).0
        };
        // Still analysing, failed, or not fetched: no reports.
        let marks = pass(&mut view);
        assert!(marks.contains_key("player:scrubber"));
        assert!(!has_events(&marks));
        // A report with no swing found: none.
        view.cameras[0].swing.report = Some(Arc::new(swing_report(
            &[("address", 100), ("top", 500)],
            "given",
        )));
        assert!(!has_events(&pass(&mut view)));
        // One that found a swing: markers; and not in Off.
        view.cameras[0].swing.report = Some(Arc::new(swing_report(
            &[("address", 100), ("top", 500)],
            "refined",
        )));
        assert!(has_events(&pass(&mut view)));
        assert_eq!(names(&view.swing_markers()), ["Address", "Top"]);
        view.mode = BodyTrackingMode::Off;
        assert!(view.swing_markers().is_empty());
    }

    /// One pass of `view`'s columns, 300 px each, on a 800×400 screen,
    /// without skeletons.
    fn columns_pass(
        ctx: &egui::Context,
        view: &mut ReplayView,
        events: Vec<egui::Event>,
        time: f64,
    ) -> (crate::panels::devices::Marks, egui::FullOutput) {
        let draw = PoseDraw {
            overlay: false,
            min_score: 0.3,
            hand: crate::types::Handedness::Right,
        };
        columns_pass_drawing(ctx, view, events, time, draw)
    }

    /// One pass of `view`'s columns as [`columns_pass`], skeletons drawn as
    /// `draw` says.
    fn columns_pass_drawing(
        ctx: &egui::Context,
        view: &mut ReplayView,
        events: Vec<egui::Event>,
        time: f64,
        draw: PoseDraw,
    ) -> (crate::panels::devices::Marks, egui::FullOutput) {
        use crate::panels::devices::Marks;
        ctx.data_mut(|d| d.remove::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 400.0),
            )),
            time: Some(time),
            events,
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                view.columns(ui, "player", 300.0, 12.0, draw);
            });
        });
        let marks = ctx
            .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default();
        (marks, out)
    }

    /// Each camera's title row ends in a download icon right beside the
    /// title, not a link at the far right; it says "Download" on hover and a
    /// click opens the clip's URL.
    #[test]
    fn a_download_icon_follows_each_camera_title_and_opens_the_clip() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut view = ReplayView::new(
            1,
            &shot(&["camera.0", "camera.1"]),
            |id| format!("Cam {id}"),
            false,
            0.3,
        );
        columns_pass(&ctx, &mut view, Vec::new(), 0.0);
        let (marks, _) = columns_pass(&ctx, &mut view, Vec::new(), 0.1);
        for camera in ["camera.0", "camera.1"] {
            let icon = marks[&format!("player:download:{camera}")].0;
            let picture = marks[&format!("player:picture:{camera}")].0;
            assert!(icon.bottom() <= picture.top() + 0.5, "{camera}: title row");
            assert!(
                icon.left() > picture.left() && icon.left() - picture.left() < 200.0,
                "{camera}: beside the title, not at the far right: {icon:?}"
            );
            assert!(
                icon.right() < picture.right() - 40.0,
                "{camera}: not at the right edge"
            );
            assert!((icon.width() - icon.height()).abs() < 0.5, "an icon");
        }

        // Hover: just "Download".
        let over = marks["player:download:camera.0"].0.center();
        let mut shapes = Vec::new();
        for (i, t) in [1.0, 1.3, 2.2, 2.4].into_iter().enumerate() {
            let moved = if i == 0 {
                vec![egui::Event::PointerMoved(over)]
            } else {
                Vec::new()
            };
            shapes = columns_pass(&ctx, &mut view, moved, t).1.shapes;
        }
        let mut texts = Vec::new();
        fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
                _ => {}
            }
        }
        for clipped in &shapes {
            collect(&clipped.shape, &mut texts);
        }
        assert!(texts.iter().any(|t| t == "Download"), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("Download ")), "{texts:?}");

        // A click opens the clip.
        let button = |pressed| egui::Event::PointerButton {
            pos: over,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        columns_pass(&ctx, &mut view, vec![button(true)], 3.0);
        let (_, out) = columns_pass(&ctx, &mut view, vec![button(false)], 3.1);
        let opened: Vec<String> = out
            .platform_output
            .commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(opened, [net::clip_url("s1", "camera.0", "avi")]);
    }
}
