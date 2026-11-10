//! The Shots tab's Cameras pane (beta): with body tracking (alpha), the
//! golfer's skeleton over every camera's picture, and the golf numbers
//! measured on it. The Shots tab switches between the shots' data (in the
//! Cards or Table view) and the shown shot's cameras (the Data | Cameras
//! toggle, [`ShotsPane`]); in the Cameras pane the toolbar's Live switch
//! shows the cameras live.
//!
//! - **A shot:** its clips play inline, side by side, with the replay's
//!   transport controls ([`replay`]); under each, that camera's swing
//!   analysis: tempo and the view's metrics at address, top and impact; the
//!   swing events are marked on the scrubber. A shot without clips shows each
//!   camera's status instead.
//! - **Live:** each configured camera's live picture with its skeleton and,
//!   under it, the golfer's posture for that camera's view, measured on the
//!   server with each frame. Each view runs its own fetch loop on
//!   `GET /api/cameras/{actor}/live`: a request goes out only once the last
//!   one is answered, asking for a frame newer than the one shown, and the
//!   server holds it until there is one (up to a second). A frame asks for
//!   a repaint; it is decoded on the next screen update into the view's
//!   texture, and the keypoints and posture that came with it (`X-Pose`) are
//!   drawn with that picture, so they always match it. The loops run only
//!   while Live is on screen: elsewhere, or with the window minimized or
//!   the browser tab hidden, the views are dropped, and the server stops
//!   updating the live frames a few seconds later.
//!
//! Body tracking's settings are on the Devices tab's Body tracking card; a
//! single line under the live views reads out the values and each camera's
//! measured rate and pose time.

use std::collections::{HashMap, VecDeque};

use crate::app::{FlighthookApp, new_actor};
use crate::net::{LiveReply, PoseState};
use crate::panels::shots::{ShotsLayout, ShotsView};
use crate::panels::{Tab, devices, monitoring, replay, shots, signs};
use crate::replay::decode;
use crate::theme;
use crate::types::{
    BodyTrackingMode, BodyTrackingSection, CameraPosition, CameraSection, ConnectionState,
    Handedness, LivePose, LivePosture, SwingReportFile, SwingTempo, SwingView, UnitSystem,
};
use flighthook::{Fit, MetricGrade, ReferenceGrades, ReferenceKind, SwingPosition};

/// Space over the Last shot strip.
const LAST_SHOT_GAP: f32 = 10.0;

/// How long a view waits after a failed request before the next (s).
const RETRY_S: f64 = 1.0;
/// Space between two cameras' columns.
const GAP: f32 = 12.0;
/// Height of a column's heading row.
const HEADING_H: f32 = 24.0;
/// A picture's height over its width until the camera's size is known.
const DEFAULT_ASPECT: f32 = 9.0 / 16.0;
/// Height of the readout line under the live views, with its gap.
const READOUT_H: f32 = 26.0;
/// Height of the pane's title row (the shot's header, or Live).
const TITLE_H: f32 = 40.0;
/// Height of a row of number tiles (a live posture, a tempo), with its gap
/// above.
const TILES_H: f32 = 52.0;
/// Tallest a shot's clips get, as a share of the pane's height.
const PLAYER_SHARE: f32 = 0.62;
/// Where the Shots tab's pane choice is kept on this device.
const PANE_KEY: &str = "shots_pane";
/// Shown for a number that was not measured.
const NONE: &str = "\u{2014}";
/// The span a view's frame rate is measured over (s).
const FPS_WINDOW_S: f64 = 2.0;
/// What the readout says about one camera.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CameraReading {
    pub(crate) name: String,
    /// Live frames a second, measured.
    pub(crate) fps: Option<f64>,
    /// Pose time per frame (ms), while poses arrive.
    pub(crate) pose_ms: Option<f32>,
}

/// The readout line, shown in Live mode: the body tracking values in
/// effect (`s`), whether the skeleton is drawn, and each camera's measured
/// live rate and pose time, named when there are several.
pub(crate) fn readout(s: &BodyTrackingSection, cameras: &[CameraReading]) -> String {
    let mut parts: Vec<String> = vec![
        s.model.label().to_owned(),
        format!("min score {:.2}", s.min_score),
        if s.smoothing > 0.0 {
            format!("smoothing {:.1}", s.smoothing)
        } else {
            "smoothing off".to_owned()
        },
        format!("detect every {}", s.detect_every),
    ];
    if !s.show_skeleton {
        parts.push("skeleton off".to_owned());
    }
    let named = cameras.len() > 1;
    for camera in cameras {
        let mut reading = Vec::new();
        if named {
            reading.push(camera.name.clone());
        }
        if let Some(fps) = camera.fps {
            reading.push(format!("{fps:.1} fps"));
        }
        if let Some(ms) = camera.pose_ms {
            reading.push(format!("{ms:.0} ms"));
        }
        if named {
            parts.push(reading.join(" "));
        } else {
            parts.extend(reading);
        }
    }
    parts.join(" · ")
}

/// The words over a live picture while body tracking's models are not
/// ready, `None` once they are (or body tracking is off).
pub(crate) fn pose_status_text(state: &PoseState) -> Option<String> {
    match state.state.as_str() {
        "downloading" => Some("Downloading body tracking models".to_owned()),
        "missing" => Some("Body tracking models not downloaded".to_owned()),
        "loading" => Some("Loading body tracking models".to_owned()),
        "failed" => Some(match &state.error {
            Some(e) => format!("Body tracking failed: {e}"),
            None => "Body tracking failed".to_owned(),
        }),
        _ => None,
    }
}

/// Body tracking values saved at once, outside the Body tracking card's
/// Save (turning body tracking on from the model download prompt), and
/// their saves: one at a time, the newest values winning. Until the saved
/// settings hold them, they are the values in effect.
#[derive(Debug, Default)]
pub(crate) struct Tuning {
    /// The values set, until the saved settings hold them.
    wanted: Option<BodyTrackingSection>,
    /// The values of the save that is out.
    in_flight: Option<BodyTrackingSection>,
    /// The values of the last save the server took.
    confirmed: Option<BodyTrackingSection>,
    /// Every save sent, in tests.
    #[cfg(test)]
    pub(crate) sent: Vec<BodyTrackingSection>,
}

impl Tuning {
    /// The values in effect: the wanted ones, else the saved ones.
    pub(crate) fn current(&self, saved: &BodyTrackingSection) -> BodyTrackingSection {
        self.wanted.clone().unwrap_or_else(|| saved.clone())
    }

    fn set(&mut self, values: BodyTrackingSection) {
        self.wanted = Some(values);
    }

    /// The values to save now, marked as out: the wanted ones, unless a
    /// save is out or the server already holds them.
    fn due(&mut self, saved: &BodyTrackingSection) -> Option<BodyTrackingSection> {
        let wanted = self.wanted.as_ref()?;
        if self.in_flight.is_some() || wanted == saved || self.confirmed.as_ref() == Some(wanted) {
            return None;
        }
        self.in_flight = Some(wanted.clone());
        self.in_flight.clone()
    }

    /// The reply to the save that was out. A failed save drops the wanted
    /// values: the saved ones are shown again.
    pub(crate) fn reply(&mut self, result: Result<(), String>) {
        let sent = self.in_flight.take();
        match result {
            Ok(()) => self.confirmed = sent,
            Err(_) => {
                self.wanted = None;
                self.confirmed = None;
            }
        }
    }

    /// Forget the wanted values once the saved settings hold them. Not
    /// while the last save the server took holds other values: the saved
    /// settings then still hold what was there before it, and the wanted
    /// values go out once they are read again.
    fn settle(&mut self, saved: &BodyTrackingSection) {
        let newer_taken = self
            .confirmed
            .as_ref()
            .is_some_and(|c| Some(c) != self.wanted.as_ref());
        if self.in_flight.is_none() && !newer_taken && self.wanted.as_ref() == Some(saved) {
            self.wanted = None;
            self.confirmed = None;
        }
    }
}

/// One camera's live view: its fetch loop and the picture on screen.
pub(crate) struct LiveView {
    /// The number its replies carry.
    id: u64,
    /// A request is out.
    in_flight: bool,
    /// The sequence number of the frame received last.
    after: u64,
    /// No request before this time (egui input time, s).
    retry_at: f64,
    /// The newest frame received, not decoded yet, and its pose.
    jpeg: Option<(Vec<u8>, Option<LivePose>)>,
    /// The picture on screen.
    texture: Option<egui::TextureHandle>,
    /// The pose found on the picture on screen.
    shown_pose: Option<LivePose>,
    /// Body tracking's state, as the last reply had it.
    pose_state: PoseState,
    /// When the frames of the last [`FPS_WINDOW_S`] arrived (s).
    arrivals: VecDeque<f64>,
    /// Pose time per frame, averaged (ms).
    pose_ms: Option<f32>,
}

impl LiveView {
    fn new(id: u64) -> Self {
        Self {
            id,
            in_flight: false,
            after: 0,
            retry_at: 0.0,
            jpeg: None,
            texture: None,
            shown_pose: None,
            pose_state: PoseState::default(),
            arrivals: VecDeque::new(),
            pose_ms: None,
        }
    }

    /// Whether the next request is due at `now`: none is out and no retry
    /// is pending. Marks it sent, returning the frame number to ask after.
    fn due(&mut self, now: f64) -> Option<u64> {
        if self.in_flight || now < self.retry_at {
            return None;
        }
        self.in_flight = true;
        Some(self.after)
    }

    /// Take the reply to the request out, answered at `now`. A frame
    /// numbered below the last one is taken too: the server restarted and
    /// numbers its frames afresh.
    fn reply(&mut self, reply: LiveReply, now: f64) {
        self.in_flight = false;
        match reply {
            LiveReply::Frame {
                seq,
                jpeg,
                pose,
                state,
            } => {
                self.pose_state = state;
                if seq != self.after {
                    self.after = seq;
                    if let Some(p) = &pose {
                        self.pose_ms = Some(match self.pose_ms {
                            Some(avg) => avg * 0.8 + p.ms * 0.2,
                            None => p.ms,
                        });
                    } else {
                        self.pose_ms = None;
                    }
                    self.jpeg = Some((jpeg, pose));
                    self.arrivals.push_back(now);
                }
            }
            LiveReply::Nothing(state) => self.pose_state = state,
            LiveReply::Failed(_) => self.retry_at = now + RETRY_S,
        }
        while self
            .arrivals
            .front()
            .is_some_and(|t| now - t > FPS_WINDOW_S)
        {
            self.arrivals.pop_front();
        }
    }

    /// Live frames a second over the last [`FPS_WINDOW_S`] before `now`.
    fn fps(&self, now: f64) -> Option<f64> {
        let first = self.arrivals.iter().find(|t| now - **t <= FPS_WINDOW_S)?;
        let last = self.arrivals.back()?;
        let n = self.arrivals.iter().filter(|t| *t >= first).count();
        (n >= 2 && last > first).then(|| (n - 1) as f64 / (last - first))
    }

    /// The picture, decoding a newly received frame into it (its pose goes
    /// with it). A frame that does not decode leaves the last picture up,
    /// with its own pose.
    fn texture(&mut self, ctx: &egui::Context, actor: &str) -> Option<&egui::TextureHandle> {
        if let Some((jpeg, pose)) = self.jpeg.take()
            && let Some((w, h, rgb)) = decode(&jpeg)
        {
            let image = egui::ColorImage::from_rgb([w, h], &rgb);
            match &mut self.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => {
                    let name = format!("live:{actor}");
                    self.texture =
                        Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR));
                }
            }
            self.shown_pose = pose;
        }
        self.texture.as_ref()
    }

    /// Height over width of the picture, once one is shown.
    fn aspect(&self) -> Option<f32> {
        let [w, h] = self.texture.as_ref()?.size();
        (w > 0).then(|| h as f32 / w as f32)
    }
}

/// Whether the app's window is on screen: not minimized, and in a browser
/// not a hidden tab.
fn page_visible(ctx: &egui::Context) -> bool {
    let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
    #[cfg(target_arch = "wasm32")]
    let hidden = web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| d.hidden());
    #[cfg(not(target_arch = "wasm32"))]
    let hidden = false;
    !minimized && !hidden
}

/// Height over width of a camera reporting `resolution` (`1280x720`).
fn resolution_aspect(resolution: &str) -> Option<f32> {
    let (w, h) = resolution.split_once('x')?;
    let (w, h) = (w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?);
    (w > 0.0 && h > 0.0).then_some(h / w)
}

/// The width of each of `n` side-by-side pictures of height-over-width
/// `aspect` in `available`: as wide as the pane allows, unless a picture
/// and its heading would be taller than the pane.
fn column_width(n: usize, available: egui::Vec2, aspect: f32) -> f32 {
    let n = n.max(1) as f32;
    let widest = (available.x - GAP * (n - 1.0)) / n;
    let tallest = (available.y - HEADING_H) / aspect;
    widest.min(tallest).max(1.0)
}

impl FlighthookApp {
    /// Whether the cameras have a place in the Shots tab (the Data |
    /// Cameras toggle): the server can record from cameras
    /// (as for the Devices tab's Cameras (+)) and at least one is
    /// configured.
    pub(crate) fn cameras_shown(&self) -> bool {
        self.cameras_supported && !self.settings.saved_cameras().is_empty()
    }

    /// Whether the Cameras pane is on screen: the Shots tab with cameras
    /// and Cameras chosen, in either view, showing Live or a shot's
    /// cameras.
    pub(crate) fn cameras_pane_shown(&self) -> bool {
        self.active_tab == Tab::Shots
            && self.cameras_shown()
            && self.shots_pane == ShotsPane::Cameras
    }

    /// What the Shots tab lays out: the Cameras pane while it is chosen
    /// ([`FlighthookApp::cameras_pane_shown`]), else the Data pane in the
    /// Cards | Table choice.
    pub(crate) fn shots_layout(&self) -> ShotsLayout {
        if self.cameras_pane_shown() {
            return ShotsLayout::Cameras;
        }
        match self.shots_view {
            ShotsView::Cards => ShotsLayout::Cards,
            ShotsView::Table => ShotsLayout::Table,
        }
    }

    /// Whether the live views are on screen.
    pub(crate) fn live_shown(&self) -> bool {
        self.live_selected && self.cameras_pane_shown()
    }

    /// Show the shot's data or its cameras, remembered on this device.
    /// Data leaves Live: Live is part of the Cameras pane.
    pub(crate) fn set_shots_pane(&mut self, pane: ShotsPane) {
        if pane == ShotsPane::Data {
            self.live_selected = false;
        }
        self.shots_pane = pane;
        crate::local::store(PANE_KEY, pane.key());
    }

    /// Turn Live on: the Cameras pane shows the live views. Live belongs
    /// to the Cameras pane, so the pane is chosen with it.
    pub(crate) fn select_live(&mut self) {
        if self.shots_pane != ShotsPane::Cameras {
            self.set_shots_pane(ShotsPane::Cameras);
        }
        self.live_selected = true;
        self.cameras_pane_was_shown = self.cameras_pane_shown();
    }

    /// Each time the Cameras pane comes on screen (Cameras clicked, the
    /// Shots tab shown again with Cameras chosen, or the shot history
    /// cleared while it shows): Live when there is no shot to
    /// show under the device filter, else the latest shot, any pin dropped.
    /// Called before anything reads the pane's state in a frame; a call
    /// with no change does nothing.
    pub(crate) fn sync_cameras_pane(&mut self) {
        let shown = self.cameras_pane_shown();
        if shown && !self.cameras_pane_was_shown {
            self.shot_focus.follow();
            self.live_selected = self.shown_index().is_none();
        }
        self.cameras_pane_was_shown = shown;
    }

    /// The body tracking values in effect: those being saved at once
    /// ([`Tuning`]), else the saved ones.
    pub(crate) fn body_tracking(&self) -> BodyTrackingSection {
        self.tuning.current(&self.settings.saved_body_tracking())
    }

    /// Keypoints scoring below this are not drawn.
    pub(crate) fn pose_min_score(&self) -> f32 {
        self.body_tracking().min_score as f32
    }

    /// Change the body tracking values with `f`; the change is saved on the
    /// next screen update ([`FlighthookApp::send_tuning`]).
    pub(crate) fn tune(&mut self, f: impl FnOnce(&mut BodyTrackingSection)) {
        let mut values = self.body_tracking();
        f(&mut values);
        self.tuning.set(values);
    }

    /// Save the tuned values when a save is due: one at a time, through the
    /// settings (`POST /api/settings?scope=body_tracking`).
    pub(crate) fn send_tuning(&mut self, ctx: &egui::Context) {
        let saved = self.settings.saved_body_tracking();
        self.tuning.settle(&saved);
        let Some(values) = self.tuning.due(&saved) else {
            return;
        };
        let request = self.settings.tuning_request(&values);
        #[cfg(not(test))]
        crate::net::post_tuning(ctx, &self.pending, &request);
        #[cfg(test)]
        {
            let _ = ctx;
            self.tuning.sent.push(request.body_tracking);
        }
    }

    /// Keep one live view per saved camera while Live is on screen, and
    /// none otherwise. Live is deselected once no camera is left.
    pub(crate) fn sync_live_views(&mut self, ctx: &egui::Context) {
        if self.settings.loaded && !self.cameras_shown() {
            self.live_selected = false;
        }
        if !self.live_shown() || !page_visible(ctx) {
            self.live.clear();
            return;
        }
        let cameras = self.settings.cameras_on();
        self.live
            .retain(|actor, _| cameras.iter().any(|(id, _)| id == actor));
        for (actor, _) in cameras {
            if !self.live.contains_key(&actor) {
                self.live_seq += 1;
                self.live.insert(actor, LiveView::new(self.live_seq));
            }
        }
    }

    /// Hand the live replies, received at `now` (egui input time, s), to
    /// the views that asked; a reply for a view since dropped is ignored.
    pub(crate) fn live_replies(&mut self, replies: Vec<(u64, LiveReply)>, now: f64) {
        for (id, reply) in replies {
            if let Some(view) = self.live.values_mut().find(|v| v.id == id) {
                view.reply(reply, now);
            }
        }
    }

    /// Send each live view's next request, when it is due.
    fn fetch_live_frames(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        for (actor, view) in &mut self.live {
            if let Some(after) = view.due(now) {
                #[cfg(not(test))]
                crate::net::fetch_live(ctx, &self.pending, view.id, actor, after);
                #[cfg(test)]
                let _ = (ctx, actor, after);
            }
        }
    }

    /// The Cameras pane in `ui`: Live, or the shown shot's cameras, or
    /// the empty state before the first shot.
    pub(crate) fn render_cameras_pane(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.send_tuning(&ctx);
        if self.live_selected {
            self.render_live(ui);
        } else if let Some(i) = self.shown_index() {
            self.render_shot_cameras(ui, i);
        } else {
            crate::panels::shots::empty_state(
                ui,
                "The latest shot's clips appear here as soon as it lands.",
            );
        }
    }

    /// Live: the saved cameras side by side, each with its name and position
    /// over its live picture, and in Live mode its posture under it and the
    /// readout line under them all; in the other modes the pictures alone.
    /// Under the pictures, the newest shot's Last shot strip once its swing
    /// is analyzed ([`last_shot`](crate::panels::last_shot)).
    fn render_live(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.fetch_live_frames(&ctx);
        self.sync_last_shot(&ctx);
        let cameras = self.settings.cameras_on();
        if cameras.is_empty() {
            return;
        }
        let tracking = self.body_tracking().mode == BodyTrackingMode::Live;
        let title = pane_title(ui, "Live");
        devices::mark(ui, || "live_title".to_owned(), &title);
        // The tallest picture sets the row's height, so every column fits.
        let aspect = cameras
            .iter()
            .map(|(actor, _)| self.camera_aspect(actor))
            .fold(DEFAULT_ASPECT, f32::max);
        let available = ui.available_size();
        let n = cameras.len();
        let postures = tracking
            && cameras
                .iter()
                .any(|(_, s)| !posture_tiles(s.position, None).is_empty());
        // The readout and the posture tiles, and the spacing around them.
        let spacing = ui.spacing().item_spacing.y;
        let below = if tracking { READOUT_H + spacing } else { 0.0 }
            + if postures {
                TILES_H + 2.0 * spacing
            } else {
                0.0
            }
            + if self.live_strip.shown().is_some() {
                crate::panels::last_shot::LAST_SHOT_H + LAST_SHOT_GAP + spacing
            } else {
                0.0
            };
        let budget = egui::vec2(available.x, (available.y - below).max(1.0));
        let width = column_width(n, budget, aspect);
        let row = width * n as f32 + GAP * (n as f32 - 1.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.add_space(((available.x - row) / 2.0 - GAP).max(0.0));
            for (actor, section) in &cameras {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        self.camera_column(ui, actor, section, width);
                        if !tracking {
                            return;
                        }
                        let posture = self
                            .live
                            .get(actor.as_str())
                            .and_then(|v| v.shown_pose.as_ref())
                            .and_then(|p| p.posture.as_ref());
                        let tiles = posture_tiles(section.position, posture);
                        if !tiles.is_empty() {
                            ui.add_space(8.0);
                            let row = number_tiles(ui, &tiles, width);
                            devices::mark(ui, || format!("posture:{actor}"), &row);
                        }
                    },
                );
            }
        });
        if self.live_strip.shown().is_some() {
            ui.add_space(LAST_SHOT_GAP);
            // Placed as the pictures' row is, so it lines up under them.
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                ui.add_space(((available.x - row) / 2.0 - GAP).max(0.0));
                self.render_last_shot(ui, row);
            });
        }
        if !tracking {
            return;
        }
        ui.add_space(6.0);
        let text = self.pose_readout(&cameras, ctx.input(|i| i.time));
        let line = ui
            .vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(text)
                        .size(13.0)
                        .color(theme::TEXT_MUTED),
                )
            })
            .inner;
        devices::mark(ui, || "pose_readout".to_owned(), &line);
    }

    /// The readout line for `cameras` at `now` (egui input time, s).
    fn pose_readout(&self, cameras: &[(String, CameraSection)], now: f64) -> String {
        let readings: Vec<CameraReading> = cameras
            .iter()
            .map(|(actor, section)| {
                let view = self.live.get(actor);
                CameraReading {
                    name: section.name.clone(),
                    fps: view.and_then(|v| v.fps(now)),
                    pose_ms: view.and_then(|v| v.pose_ms),
                }
            })
            .collect();
        readout(&self.body_tracking(), &readings)
    }

    /// Height over width of camera `actor`'s picture, turned by its
    /// rotation: the frame shown, else the resolution it reports, else
    /// 16:9.
    fn camera_aspect(&self, actor: &str) -> f32 {
        let aspect = self
            .live
            .get(actor)
            .and_then(LiveView::aspect)
            .or_else(|| {
                let telemetry = &self.actors.get(actor)?.telemetry;
                resolution_aspect(telemetry.get("resolution")?)
            })
            .unwrap_or(DEFAULT_ASPECT);
        if crate::pose::quarter_turn(self.camera_rotation(actor)) {
            1.0 / aspect
        } else {
            aspect
        }
    }

    /// The rotation camera `actor`'s pictures are shown with, clockwise
    /// degrees: its saved setting, 0 for a camera not configured.
    pub(crate) fn camera_rotation(&self, actor: &str) -> u16 {
        self.settings.saved_rotation(actor).unwrap_or(0)
    }

    /// One camera's column, `width` wide: its heading, then its live
    /// picture with the golfer's skeleton over it, or while it is not
    /// Connected its status as its Devices card words it.
    fn camera_column(
        &mut self,
        ui: &mut egui::Ui,
        actor: &str,
        section: &CameraSection,
        width: f32,
    ) {
        ui.allocate_ui_with_layout(
            egui::vec2(width, HEADING_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| replay::camera_heading(ui, &section.name, section.position),
        );
        let status = self
            .actors
            .get(actor)
            .cloned()
            .unwrap_or_else(|| new_actor(section.name.clone()));
        let connected = status.state() == ConnectionState::Connected;
        let aspect = self.camera_aspect(actor);
        let ctx = ui.ctx().clone();
        let texture = self
            .live
            .get_mut(actor)
            .and_then(|view| view.texture(&ctx, actor).cloned())
            .filter(|_| connected);
        let rotation = flighthook::rotation_in_effect_deg(section.rotation_deg);
        let shown = match &texture {
            Some(tex) => {
                let size = crate::pose::turned_size(tex.size_vec2(), rotation);
                let scale = if size.x > 0.0 { width / size.x } else { 1.0 };
                let (rect, shown) = ui.allocate_exact_size(size * scale, egui::Sense::hover());
                crate::pose::paint_turned(ui, tex.id(), rect, rotation, egui::CornerRadius::ZERO);
                let [w, h] = tex.size();
                let frame = crate::pose::turned(
                    u32::try_from(w).unwrap_or(u32::MAX),
                    u32::try_from(h).unwrap_or(u32::MAX),
                    rotation,
                );
                self.pose_over(ui, actor, shown.rect, frame);
                shown
            }
            None => {
                let size = egui::vec2(width, width * aspect);
                let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
                ui.painter().rect_filled(rect, 6.0, theme::SURFACE_RAISED);
                if !connected {
                    let badge = status_over(ui, rect, &status, self.now_ms);
                    devices::mark(ui, || format!("camera_status:{actor}"), &badge);
                }
                response
            }
        };
        devices::mark(ui, || format!("camera_live:{actor}"), &shown);
    }

    /// Body tracking over camera `actor`'s picture in `rect` (a `frame`
    /// = `[width, height]` pixel picture, turned upright): the skeleton
    /// found on it, unless the skeleton is hidden, or while the models are
    /// not ready, what they are doing.
    fn pose_over(&self, ui: &egui::Ui, actor: &str, rect: egui::Rect, frame: [u32; 2]) {
        let Some(view) = self.live.get(actor) else {
            return;
        };
        let settings = self.body_tracking();
        if settings.mode != BodyTrackingMode::Live {
            return;
        }
        if let Some(text) = pose_status_text(&view.pose_state) {
            let band = status_band(ui, rect, &text);
            devices::mark(ui, || format!("pose_status:{actor}"), &band);
            return;
        }
        let Some(pose) = view.shown_pose.as_ref().filter(|_| settings.show_skeleton) else {
            return;
        };
        crate::pose::paint(
            &ui.painter_at(rect),
            frame,
            rect,
            &pose.keypoints,
            settings.min_score as f32,
            self.handed.unwrap_or(Handedness::Right),
        );
    }
}

/// `text` on a band across the bottom of `rect`. Returns the band's
/// response.
fn status_band(ui: &egui::Ui, rect: egui::Rect, text: &str) -> egui::Response {
    let painter = ui.painter_at(rect);
    let galley = painter.layout(
        text.to_owned(),
        egui::FontId::proportional(13.0),
        theme::TEXT,
        (rect.width() - 24.0).max(40.0),
    );
    let band = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.bottom() - galley.size().y - 16.0),
        rect.right_bottom(),
    );
    painter.rect_filled(band, 0.0, theme::BG.gamma_multiply(0.85));
    painter.galley(band.center() - galley.size() / 2.0, galley, theme::TEXT);
    ui.interact(
        band,
        ui.id().with(("pose_status", text)),
        egui::Sense::hover(),
    )
}

/// A camera's status over the middle of `rect`: the badge its Devices card
/// shows, and the `error` it reports. Returns the badge.
fn status_over(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    status: &crate::types::ActorStatusResponse,
    now_ms: u64,
) -> egui::Response {
    let color = monitoring::status_color(status, now_ms);
    let error = status
        .telemetry
        .get("error")
        .filter(|e| !e.is_empty())
        .cloned();
    let inner = rect.shrink(16.0);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    let height = 24.0 + if error.is_some() { 24.0 } else { 0.0 };
    child.add_space(((inner.height() - height) / 2.0).max(0.0));
    let badge = theme::badge(&mut child, status.state().label(), color);
    if let Some(error) = error {
        child.add_space(6.0);
        child.label(
            egui::RichText::new(error)
                .size(12.0)
                .color(theme::TEXT_MUTED),
        );
    }
    badge
}

/// The live views by camera actor ID, as the app holds them.
pub(crate) type LiveViews = HashMap<String, LiveView>;

// ---------------------------------------------------------------------------
// The pane toggle
// ---------------------------------------------------------------------------

/// What the Shots tab shows under the toolbar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ShotsPane {
    /// The shots' numbers and charts, in the Cards | Table choice.
    #[default]
    Data,
    /// The shot's clips and swing analysis.
    Cameras,
}

impl ShotsPane {
    /// The toggle's options, in order.
    pub(crate) const ALL: [Self; 2] = [Self::Data, Self::Cameras];

    fn key(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Cameras => "cameras",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Data => "Data",
            Self::Cameras => "Cameras",
        }
    }

    /// The choice kept on this device, Data when there is none.
    pub(crate) fn stored() -> Self {
        match crate::local::load(PANE_KEY).as_deref() {
            Some("cameras") => Self::Cameras,
            _ => Self::Data,
        }
    }
}

impl FlighthookApp {
    /// The toolbar's Data | Cameras toggle, in both views while there are
    /// cameras ([`FlighthookApp::cameras_shown`]).
    pub(crate) fn render_pane_toggle(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.cameras_shown() {
            return false;
        }
        let selected = ShotsPane::ALL
            .iter()
            .position(|p| *p == self.shots_pane)
            .unwrap_or(0);
        let labels = ShotsPane::ALL.map(ShotsPane::label);
        let toggle = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
        devices::mark(ui, || "pane_toggle".to_owned(), &toggle.response);
        let (clicked, pills) = toggle.inner;
        for (pane, pill) in ShotsPane::ALL.iter().zip(&pills) {
            devices::mark(ui, || format!("pane:{}", pane.key()), pill);
        }
        if let Some(i) = clicked {
            self.set_shots_pane(ShotsPane::ALL[i]);
        }
        true
    }

    /// The toolbar's Live switch, while the Cameras pane shows: on, the
    /// live views; off, the shot the list shows.
    pub(crate) fn render_live_toggle(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.cameras_pane_shown() {
            return false;
        }
        let toggle = theme::switch_pill(ui, self.live_selected, "Live", theme::DANGER);
        devices::mark(ui, || "live_toggle".to_owned(), &toggle.response);
        if toggle.inner.clicked() {
            if self.live_selected {
                self.live_selected = false;
            } else {
                self.select_live();
            }
        }
        true
    }
}

impl FlighthookApp {
    /// The toolbar's reference picker, while the Cameras pane shows: which
    /// population's bands the swing numbers are colored against (marks
    /// `body_reference`, `body_reference:<key>`). Kept on this device.
    pub(crate) fn render_reference_picker(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.cameras_pane_shown() {
            return false;
        }
        let selected = ReferenceKind::ALL
            .iter()
            .position(|r| *r == self.body_reference)
            .unwrap_or(0);
        let labels = ReferenceKind::ALL.map(ReferenceKind::label);
        let picker = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
        devices::mark(ui, || "body_reference".to_owned(), &picker.response);
        let (clicked, pills) = picker.inner;
        for (reference, pill) in ReferenceKind::ALL.iter().zip(&pills) {
            devices::mark(ui, || format!("body_reference:{}", reference.key()), pill);
        }
        if let Some(i) = clicked {
            self.set_body_reference(ReferenceKind::ALL[i]);
        }
        true
    }

    /// Color the swing numbers against `reference`'s bands, and keep the
    /// choice on this device.
    pub(crate) fn set_body_reference(&mut self, reference: ReferenceKind) {
        self.body_reference = reference;
        crate::local::store(REFERENCE_KEY, reference.key());
    }
}

/// Column `c`'s grades against `reference`, once fetched.
fn column_grades(c: &replay::PlayerColumn, reference: ReferenceKind) -> Option<&ReferenceGrades> {
    c.grades.as_deref()?.reference(reference)
}

/// Where the reference picker's choice is kept.
const REFERENCE_KEY: &str = "body_reference";

/// The reference kept on this device, Tour when there is none.
pub(crate) fn stored_reference() -> ReferenceKind {
    let kept = crate::local::load(REFERENCE_KEY);
    ReferenceKind::ALL
        .into_iter()
        .find(|r| kept.as_deref() == Some(r.key()))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// A shot's cameras
// ---------------------------------------------------------------------------

impl FlighthookApp {
    /// Shot row `i`'s cameras: its header, then its clips playing side by
    /// side with each camera's swing analysis under its clip; for a shot
    /// without clips, each camera's status. The pane scrolls when they
    /// don't fit.
    fn render_shot_cameras(&mut self, ui: &mut egui::Ui, i: usize) {
        let ctx = ui.ctx().clone();
        let following = self.following();
        let header = ui
            .allocate_ui_with_layout(
                egui::vec2(ui.available_width(), TITLE_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| self.render_shot_header(ui, i, following),
            )
            .inner;
        if header.back {
            self.shot_focus.follow();
        }
        if header.replay {
            self.open_replay(&ctx, i);
        }
        let max_picture = (ui.available_height() * PLAYER_SHARE).max(120.0);
        if self.shots[i].video.is_empty() {
            self.shot_player = None;
            self.render_clip_status(ui, i);
            return;
        }
        self.sync_shot_player(&ctx, i);
        self.retrack_turned_clips(&ctx, i);
        let imperial = self.units() == UnitSystem::Imperial;
        let reference = self.body_reference;
        // A solid bar, beside the columns rather than over them.
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        egui::ScrollArea::vertical()
            .id_salt("shot_cameras")
            .auto_shrink(false)
            .show(ui, |ui| {
                let Some(layout) = self.render_shot_player(ui, max_picture) else {
                    return;
                };
                ui.add_space(12.0);
                // Each camera's swing, at the frame it shows, graded
                // against the picked reference.
                let grades = |c| column_grades(c, reference);
                let swings: Vec<Swing> = layout
                    .columns
                    .iter()
                    .filter_map(|c| {
                        Some(Swing {
                            report: c.swing.as_deref()?,
                            t_ns: c.frame_t_ns,
                            grades: grades(c),
                        })
                    })
                    .filter(|s| s.report.swing_found())
                    .collect();
                if !swings.is_empty() {
                    let n = layout.columns.len() as f32;
                    let row = layout.columns.first().map_or(0.0, |c| c.width) * n
                        + layout.gap * (n - 1.0);
                    // One tempo for the shot, across its cameras' columns.
                    let tempo = tempo_values(&swings);
                    ui.horizontal_top(|ui| {
                        ui.add_space(layout.pad);
                        tempo_row(ui, &tempo, row);
                    });
                    // The body at the frame each camera shows.
                    let frame = frame_tiles(&swings, imperial);
                    ui.horizontal_top(|ui| {
                        ui.add_space(layout.pad);
                        frame_row(ui, &frame, row);
                    });
                    let mut misses = miss_entries(&tempo);
                    misses.extend(miss_entries(&frame));
                    let graded = swings.iter().any(|s| {
                        s.grades
                            .is_some_and(|g| !g.grades.is_empty() || !g.tempo.is_empty())
                    });
                    ui.horizontal_top(|ui| {
                        ui.add_space(layout.pad);
                        miss_line(ui, &misses, row, graded);
                    });
                }
                ui.horizontal_top(|ui| {
                    // The same offset as the clips' row (`pad`, then the
                    // row's item spacing), then the clips' gap.
                    ui.add_space(layout.pad);
                    ui.spacing_mut().item_spacing.x = layout.gap;
                    for column in &layout.columns {
                        ui.allocate_ui_with_layout(
                            egui::vec2(column.width, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(column.width);
                                if let Some(report) = &column.swing {
                                    let panel = if report.swing_found() {
                                        if let Some(text) = self.yaw_warning(&column.camera) {
                                            let line = muted_line(ui, &text, column.width);
                                            devices::mark(
                                                ui,
                                                || format!("yaw_warning:{}", column.camera),
                                                &line,
                                            );
                                        }
                                        ui.scope(|ui| {
                                            swing_metrics(
                                                ui,
                                                &column.camera,
                                                report,
                                                imperial,
                                                column.width,
                                                grades(column),
                                                reference,
                                            );
                                        })
                                        .response
                                    } else {
                                        no_swing_line(ui, column.width)
                                    };
                                    devices::mark(
                                        ui,
                                        || format!("swing:{}", column.camera),
                                        &panel,
                                    );
                                }
                            },
                        );
                    }
                });
                ui.add_space(8.0);
            });
    }

    /// Ask once per session for each of shot row `i`'s clips tracked with
    /// a rotation other than its camera's now to be tracked again
    /// ([`retrack_due`]); its old keypoints and analysis are dropped
    /// meanwhile. Nothing while body tracking is off.
    fn retrack_turned_clips(&mut self, ctx: &egui::Context, i: usize) {
        if !self.body_tracking().mode.is_on() {
            return;
        }
        let Some(shot) = self.shots.get(i) else {
            return;
        };
        let shot_id = shot.shot_id.clone();
        let due = retrack_due(
            shot,
            |camera| self.settings.saved_rotation(camera),
            &self.retracked,
        );
        for camera in due {
            self.retracked.insert((shot_id.clone(), camera.clone()));
            #[cfg(not(test))]
            crate::net::post_retrack(ctx, &shot_id, &camera);
            #[cfg(test)]
            let _ = ctx;
            if crate::types::forget_analysis(&mut self.shots, &shot_id, &camera) {
                self.shots_rev += 1;
            }
            if let Some(player) = self.shot_player.as_mut().filter(|p| p.shot_id() == shot_id) {
                player.forget_analysis(&camera);
            }
        }
    }

    /// The line over camera `camera`'s swing analysis when it is a Golfer
    /// Front camera pointing [`YAW_WARNING_DEG`] or more off square
    /// ([`yaw_warning`]).
    fn yaw_warning(&self, camera: &str) -> Option<String> {
        let (_, section) = self
            .settings
            .saved_cameras()
            .into_iter()
            .find(|(actor, _)| actor == camera)?;
        yaw_warning(section.position, section.yaw_deg)
    }

    /// For shot row `i`, which has no clips: each saved camera's column
    /// with its status where the picture would be, for the cameras that are
    /// on.
    fn render_clip_status(&mut self, ui: &mut egui::Ui, i: usize) {
        let cameras = self.settings.cameras_on();
        if cameras.is_empty() {
            return;
        }
        let latest = crate::panels::shots::latest_index(&self.shots, self.filter()) == Some(i);
        let aspect = cameras
            .iter()
            .map(|(actor, _)| self.camera_aspect(actor))
            .fold(DEFAULT_ASPECT, f32::max);
        let available = ui.available_size();
        let n = cameras.len();
        let width = column_width(n, available, aspect);
        let row = width * n as f32 + GAP * (n as f32 - 1.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.add_space(((available.x - row) / 2.0 - GAP).max(0.0));
            for (actor, section) in &cameras {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        ui.allocate_ui_with_layout(
                            egui::vec2(width, HEADING_H),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| replay::camera_heading(ui, &section.name, section.position),
                        );
                        let size = egui::vec2(width, width * aspect);
                        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
                        ui.painter().rect_filled(rect, 6.0, theme::SURFACE_RAISED);
                        let status = self
                            .actors
                            .get(actor.as_str())
                            .cloned()
                            .unwrap_or_else(|| new_actor(section.name.clone()));
                        if status.state() == ConnectionState::Connected {
                            let text = clip_status_text(latest);
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                text,
                                egui::FontId::proportional(14.0),
                                theme::TEXT_MUTED,
                            );
                        } else {
                            status_over(ui, rect, &status, self.now_ms);
                        }
                        devices::mark(ui, || format!("clip_status:{actor}"), &response);
                    },
                );
            }
        });
    }
}

/// What a camera's column says under its clip when the swing analysis
/// found no swing near the shot's impact.
pub(crate) const NO_SWING: &str = "No swing found in this clip";

/// The [`NO_SWING`] line, `width` wide, in the clip status lines' style.
/// Degrees off square from which a Golfer Front camera's one-camera turn
/// and sway are flagged as approximate.
pub(crate) const YAW_WARNING_DEG: f64 = 5.0;

/// The line flagging a Golfer Front camera `yaw` degrees off square, at
/// [`YAW_WARNING_DEG`] or more either way; `None` for any other camera or a
/// smaller (or invalid) yaw.
pub(crate) fn yaw_warning(position: CameraPosition, yaw: Option<f64>) -> Option<String> {
    let yaw = flighthook::camera_angle_in_effect_deg(yaw).abs();
    (position == CameraPosition::GolferFront && yaw >= YAW_WARNING_DEG).then(|| {
        format!(
            "Camera {}° off square: one-camera turn and sway are approximate",
            yaw.round()
        )
    })
}

/// The cameras of `shot` whose clip was tracked with a rotation other than
/// the camera's now (`rotation_of`; `None` for a camera no longer
/// configured, which is left alone) and not asked about yet (`asked`, by
/// (shot ID, camera)).
pub(crate) fn retrack_due(
    shot: &crate::types::ShotRow,
    rotation_of: impl Fn(&str) -> Option<u16>,
    asked: &std::collections::HashSet<(String, String)>,
) -> Vec<String> {
    shot.video
        .iter()
        .filter(|v| v.pose)
        .filter(|v| rotation_of(&v.camera).is_some_and(|r| r != v.rotation))
        .filter(|v| !asked.contains(&(shot.shot_id.clone(), v.camera.clone())))
        .map(|v| v.camera.clone())
        .collect()
}

/// A muted line of `text`, `width` wide, wrapped.
fn muted_line(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .size(14.0)
                        .color(theme::TEXT_MUTED),
                )
                .wrap(),
            )
        },
    )
    .inner
}

fn no_swing_line(ui: &mut egui::Ui, width: f32) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, TABLE_ROW_H), egui::Sense::hover());
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        NO_SWING,
        egui::FontId::proportional(14.0),
        theme::TEXT_MUTED,
    );
    response
}

/// What a connected camera's column says for a shot without its clip: the
/// latest shot's may still be on its way.
pub(crate) fn clip_status_text(latest: bool) -> &'static str {
    if latest {
        "Waiting for the clip"
    } else {
        "No clip of this shot"
    }
}

/// The pane's title row: `title` large, as a shot's header reads.
fn pane_title(ui: &mut egui::Ui, title: &str) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), TITLE_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(
                egui::RichText::new(title)
                    .size(22.0)
                    .strong()
                    .color(theme::TEXT),
            )
        },
    )
    .inner
}

/// A row of number tiles across `width`: each a name over its value.
/// Returns the row.
fn number_tiles(ui: &mut egui::Ui, tiles: &[(&str, String)], width: f32) -> egui::Response {
    let tiles: Vec<(&str, &str, Option<&str>, egui::Color32)> = tiles
        .iter()
        .map(|(name, value)| (*name, value.as_str(), None, theme::TEXT))
        .collect();
    worded_tiles(ui, &tiles, width)
}

/// A row of number tiles across `width`: each a name over its value, and
/// the value's direction word after it when it has one, both in the
/// value's color. Returns the row.
fn worded_tiles(
    ui: &mut egui::Ui,
    tiles: &[(&str, &str, Option<&str>, egui::Color32)],
    width: f32,
) -> egui::Response {
    let height = TILES_H - 8.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) || tiles.is_empty() {
        return response;
    }
    let painter = ui.painter_at(rect);
    for ((name, value, word, color), tile) in tiles.iter().zip(tile_rects(rect, tiles.len())) {
        painter.rect_filled(tile, 6.0, theme::SURFACE);
        let name = painter.layout_no_wrap(
            name.to_uppercase(),
            egui::FontId::proportional(10.0),
            theme::TEXT_MUTED,
        );
        painter.galley(
            egui::pos2(tile.left() + 8.0, tile.top() + 5.0),
            name,
            theme::TEXT_MUTED,
        );
        let (value, word) = worded_value(&painter, value, *word, 18.0, tile.width() - 16.0);
        let at = egui::pos2(tile.left() + 8.0, tile.bottom() - value.size().y - 4.0);
        painter.galley(at, value, *color);
        if let Some((word, offset)) = word {
            painter.galley(at + offset, word, *color);
        }
    }
    response
}

/// A value laid out at `size` with its direction `word` (if any) after it
/// on the same baseline, fitted into `room` as the Cards view fits its sign
/// words: the word starts at the cards' small size and shrinks to their
/// floor first, and only then does the numeral shrink. Returns the value
/// and the word with its offset from the value's top-left corner, to paint
/// in their color.
fn worded_value(
    painter: &egui::Painter,
    text: &str,
    word: Option<&str>,
    size: f32,
    room: f32,
) -> (
    std::sync::Arc<egui::Galley>,
    Option<(std::sync::Arc<egui::Galley>, egui::Vec2)>,
) {
    // Laid out without a color: the painter gives each its own.
    let layout = |text: &str, size: f32| {
        painter.layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(size),
            egui::Color32::PLACEHOLDER,
        )
    };
    let mut size = size;
    loop {
        let value = layout(text, size);
        let Some(word) = word else {
            return (value, None);
        };
        let gap = (size * 0.16).max(4.0);
        let mut word_size = shots::small_size(size);
        loop {
            let galley = layout(word, word_size);
            let fits = value.size().x + gap + galley.size().x <= room;
            if fits || (word_size <= shots::WORD_MIN && size <= shots::NUMERAL_FLOOR) {
                let baseline_gap = shots::DESCENT * (size - word_size);
                let offset = egui::vec2(
                    value.size().x + gap,
                    value.size().y - baseline_gap - galley.size().y,
                );
                return (value, Some((galley, offset)));
            }
            if word_size <= shots::WORD_MIN {
                break;
            }
            word_size = (word_size - 0.5).max(shots::WORD_MIN);
        }
        size = (size - 1.0).max(shots::NUMERAL_FLOOR);
    }
}

/// The rects of `n` tiles side by side across a row's `rect`.
fn tile_rects(rect: egui::Rect, n: usize) -> Vec<egui::Rect> {
    let gap = 6.0;
    let count = n as f32;
    let tile_w = ((rect.width() - gap * (count - 1.0)) / count).max(1.0);
    (0..n)
        .map(|k| {
            let left = rect.left() + k as f32 * (tile_w + gap);
            egui::Rect::from_min_size(
                egui::pos2(left, rect.top()),
                egui::vec2(tile_w, rect.height()),
            )
        })
        .collect()
}

/// An angle in whole degrees.
fn degrees(v: f64) -> String {
    format!("{}\u{b0}", fixed(v, 0))
}

/// `v` to `places` decimals, without the sign of a value that rounds to
/// zero ("0", never "-0").
fn fixed(v: f64, places: usize) -> String {
    let text = format!("{v:.places$}");
    match text.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_owned(),
        _ => text,
    }
}

/// The live posture tiles of a camera at `position`, from `posture` (the
/// one that came with the frame shown): face-on the spine's side bend, the
/// shoulders' tilt and the lead elbow; down the line the spine's forward
/// bend, both knees' flex and the trail elbow. None for a camera behind or
/// above the golfer.
pub(crate) fn posture_tiles(
    position: CameraPosition,
    posture: Option<&LivePosture>,
) -> Vec<(&'static str, String)> {
    let value = |f: fn(&LivePosture) -> Option<f32>| {
        posture
            .and_then(f)
            .map_or_else(|| NONE.to_owned(), |v| degrees(f64::from(v)))
    };
    let view = match position {
        CameraPosition::GolferFront => SwingView::FaceOn,
        CameraPosition::TargetLine => SwingView::DownTheLine,
        CameraPosition::GolferBack | CameraPosition::Overhead => return Vec::new(),
    };
    match view {
        SwingView::FaceOn => vec![
            ("Side bend", value(|p| p.spine_angle)),
            ("Shoulder tilt", value(|p| p.shoulder_tilt)),
            ("Lead elbow", value(|p| p.lead_arm_angle)),
        ],
        SwingView::DownTheLine => vec![
            ("Forward bend", value(|p| p.spine_angle)),
            ("Lead knee flex", value(|p| p.lead_knee_flex)),
            ("Trail knee flex", value(|p| p.trail_knee_flex)),
            ("Trail elbow", value(|p| p.trail_elbow_angle)),
        ],
    }
}

// ---------------------------------------------------------------------------
// Swing analysis
// ---------------------------------------------------------------------------

/// Milliseconds of a duration in ns, rounded.
fn ms(ns: u64) -> String {
    format!("{} ms", (ns as f64 / 1e6).round())
}

/// The color of a value graded `fit`: aqua inside its band (inside both
/// the fade and the draw band of a shot-shape band), blue inside the fade
/// band only, green inside the draw band only, yellow near, red far.
pub(crate) fn grade_color(fit: Fit) -> egui::Color32 {
    match fit {
        Fit::Both => theme::GRADE_BOTH,
        Fit::Fade => theme::GRADE_FADE,
        Fit::Draw => theme::GRADE_DRAW,
        Fit::Near => theme::GRADE_NEAR,
        Fit::Far => theme::GRADE_FAR,
    }
}

/// One camera's swing analysis as the shot's tiles read it: its report,
/// the frame it shows (its time after the clip's first frame, `None`
/// outside the clip) and its grades against the picked reference.
#[derive(Clone, Copy)]
pub(crate) struct Swing<'a> {
    pub(crate) report: &'a SwingReportFile,
    pub(crate) t_ns: Option<u64>,
    pub(crate) grades: Option<&'a ReferenceGrades>,
}

/// The key position whose event is at the frame of `report` at `t_ns`, or
/// one frame either side of it; `None` elsewhere.
pub(crate) fn key_position(report: &SwingReportFile, t_ns: u64) -> Option<SwingPosition> {
    let at = report.series.frame_at(t_ns)?;
    SwingPosition::ALL
        .into_iter()
        .filter_map(|p| {
            let frame = report.series.frame_at(report.events.at(p)?.t_ns)?;
            let apart = frame.abs_diff(at);
            (apart <= 1).then_some((apart, p))
        })
        .min_by_key(|(apart, _)| *apart)
        .map(|(_, p)| p)
}

/// The one grade a tile shows for several cameras' `grades` of it: theirs
/// when every camera has one and they agree, else none.
fn shared_grade<'a>(
    mut grades: impl Iterator<Item = Option<&'a MetricGrade>>,
) -> Option<MetricGrade> {
    let first = grades.next()??;
    grades
        .all(|g| g.is_some_and(|g| g.fit == first.fit))
        .then(|| first.clone())
}

/// Whether metric `id` is a turn: it reads `Open` or `Closed` when graded.
fn is_turn(id: &str) -> bool {
    matches!(id, "hip_turn" | "shoulder_turn")
}

/// A graded value's tile on screen: its own text and direction word; a
/// graded turn reads `Open` or `Closed`, and any other value off its band
/// that has no direction word of its own takes the word of the side it is
/// off on ("29° Less").
pub(crate) fn graded_shown(
    id: &str,
    unit: &str,
    v: f64,
    cm: Option<f64>,
    imperial: bool,
    grade: Option<&MetricGrade>,
) -> Shown {
    let mut shown = metric_shown(id, unit, v, cm, imperial);
    let Some(grade) = grade else {
        return shown;
    };
    shown.color = grade_color(grade.fit);
    if direction_word(id).is_none() {
        shown.word = if is_turn(id) {
            signs::turn(grade.value)
        } else if grade.fit == Fit::Both {
            None
        } else {
            grade.direction.as_deref().and_then(signs::direction)
        };
    }
    shown
}

/// The tempo tiles of a shot: the ratio, the backswing and the downswing,
/// averaged over the `reports` that carry a tempo (the cameras see one
/// swing). The backswing and downswing are the means of the cameras'
/// durations and the ratio is the mean backswing over the mean downswing, so
/// the three agree; one camera shows its own numbers. Dashes when no report
/// has a tempo.
pub(crate) fn tempo_tiles(reports: &[&SwingReportFile]) -> Vec<(&'static str, String)> {
    let tempos: Vec<SwingTempo> = reports.iter().filter_map(|r| r.tempo).collect();
    match average_tempo(&tempos) {
        Some(t) => vec![
            ("Tempo", format!("{:.1} : 1", t.ratio)),
            ("Backswing", ms(t.backswing_ns)),
            ("Downswing", ms(t.downswing_ns)),
        ],
        None => vec![
            ("Tempo", NONE.to_owned()),
            ("Backswing", NONE.to_owned()),
            ("Downswing", NONE.to_owned()),
        ],
    }
}

/// The tempo tiles of a shot ([`tempo_tiles`]) with the ratio's and the
/// downswing's grades. The tiles average the cameras, the grades are per
/// camera: a tile takes the face-on cameras' grade (none when two disagree),
/// or the first camera's in column order when no face-on camera has one.
pub(crate) fn tempo_values<'a>(swings: &[Swing<'a>]) -> Vec<FrameValue> {
    let reports: Vec<&SwingReportFile> = swings.iter().map(|s| s.report).collect();
    let timed: Vec<&Swing> = swings.iter().filter(|s| s.report.tempo.is_some()).collect();
    let grade = |id: &str| {
        let of = |s: &&Swing<'a>| s.grades.and_then(move |g| g.grade(id, "swing"));
        let face_on: Vec<&&Swing> = timed
            .iter()
            .filter(|s| s.report.view == Some(SwingView::FaceOn))
            .collect();
        if face_on.iter().any(|s| of(s).is_some()) {
            shared_grade(face_on.iter().map(|s| of(s)))
        } else {
            timed.iter().find_map(of).cloned()
        }
    };
    tempo_tiles(&reports)
        .into_iter()
        .map(|(label, text)| {
            let id = match label {
                "Tempo" => "tempo_ratio",
                "Downswing" => "downswing_time",
                _ => "backswing",
            };
            let grade = (text != NONE).then(|| grade(id)).flatten();
            FrameValue {
                id,
                label,
                color: grade.as_ref().map_or(theme::TEXT, |g| grade_color(g.fit)),
                text,
                word: None,
                grade,
            }
        })
        .collect()
}

/// The tempo of one swing seen by several cameras: the mean durations and
/// their ratio. A single tempo is returned as it is.
fn average_tempo(tempos: &[SwingTempo]) -> Option<SwingTempo> {
    let [first, rest @ ..] = tempos else {
        return None;
    };
    if rest.is_empty() {
        return Some(*first);
    }
    let n = tempos.len() as u128;
    let mean = |pick: fn(&SwingTempo) -> u64| {
        let sum: u128 = tempos.iter().map(|t| u128::from(pick(t))).sum();
        replay::rounded_mean(sum, n)
    };
    let (backswing_ns, downswing_ns) = (mean(|t| t.backswing_ns), mean(|t| t.downswing_ns));
    (downswing_ns > 0).then(|| SwingTempo {
        backswing_ns,
        downswing_ns,
        ratio: backswing_ns as f64 / downswing_ns as f64,
    })
}

/// A metric's name on screen: what it measures.
pub(crate) fn metric_label(id: &str) -> String {
    let known = match id {
        "hip_sway" => "Hip sway",
        "head_lateral" => "Head sway",
        "head_vertical" => "Head height",
        "lateral_spine_tilt" => "Side bend",
        "shoulder_tilt" => "Shoulder tilt",
        "lead_arm_angle" => "Lead elbow",
        "stance_width" => "Stance width",
        "hip_turn" => "Hip turn",
        "shoulder_turn" => "Shoulder turn",
        "spine_forward_bend" => "Forward bend",
        "early_extension" => "Hip depth",
        "lead_knee_flex" => "Lead knee flex",
        "trail_knee_flex" => "Trail knee flex",
        "trail_elbow_angle" => "Trail elbow",
        "hand_height" => "Hand height",
        other => {
            let words = other.replace('_', " ");
            let mut chars = words.chars();
            return chars.next().map_or_else(String::new, |c| {
                c.to_uppercase().collect::<String>() + chars.as_str()
            });
        }
    };
    known.to_owned()
}

/// A value `v` of a metric in `unit` (and its centimeters `cm`, when the
/// golfer's height was given) on screen: degrees whole, distances in inches
/// or centimeters when known, else as a share of the torso length, ratios
/// to 0.01.
pub(crate) fn metric_text(unit: &str, v: f64, cm: Option<f64>, imperial: bool) -> String {
    match unit {
        "degrees" => degrees(v),
        "torso" => match cm {
            Some(cm) if imperial => format!("{} in", fixed(cm / 2.54, 1)),
            Some(cm) => format!("{} cm", fixed(cm, 1)),
            None => format!("{}% torso", fixed(v * 100.0, 0)),
        },
        _ => fixed(v, 2),
    }
}

/// The word for the direction of a metric shown as a distance and a word
/// instead of a sign: the body's shifts from address. Hip sway and head
/// sway are measured toward the target ([`signs::target`]), hip depth
/// toward the ball ([`signs::ball`]) and head height up ([`signs::rise`]).
fn direction_word(id: &str) -> Option<fn(f64) -> Option<&'static str>> {
    match id {
        "hip_sway" | "head_lateral" => Some(signs::target),
        "early_extension" => Some(signs::ball),
        "head_vertical" => Some(signs::rise),
        _ => None,
    }
}

/// A metric's value on screen: its text, for a shift from address the word
/// for its direction, and its color (its grade's, else plain).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Shown {
    pub(crate) text: String,
    pub(crate) word: Option<&'static str>,
    pub(crate) color: egui::Color32,
}

impl Shown {
    /// A dash: not measured.
    fn none() -> Self {
        Self {
            text: NONE.to_owned(),
            word: None,
            color: theme::TEXT,
        }
    }
}

/// Metric `id`'s value `v` in `unit` (and its centimeters `cm`) on screen,
/// as [`metric_text`] writes it; a shift from address ([`direction_word`])
/// shows its distance with the word for its direction instead of a sign
/// (`Toward` / `Away`, `Toward ball` / `Back`, `Up` / `Down`), and no word
/// when the distance shows as 0. Plain (ungraded).
pub(crate) fn metric_shown(id: &str, unit: &str, v: f64, cm: Option<f64>, imperial: bool) -> Shown {
    let Some(direction) = direction_word(id) else {
        return Shown {
            text: metric_text(unit, v, cm, imperial),
            word: None,
            color: theme::TEXT,
        };
    };
    let text = metric_text(unit, v.abs(), cm.map(f64::abs), imperial);
    let zero = text.chars().filter(char::is_ascii_digit).all(|c| c == '0');
    Shown {
        word: if zero { None } else { direction(v) },
        text,
        color: theme::TEXT,
    }
}

/// Metrics the table leaves out: the hip width ratio (the hip turn shows
/// it as an angle) and the loss of posture (the forward bend's change from
/// address, which its own row shows).
const NOT_IN_TABLE: &[&str] = &["hip_width_proxy", "loss_of_posture"];

/// The metrics table's positions in a column `width` wide: address, top,
/// impact and finish, and from [`WIDE_TABLE`] the lead arm parallel back
/// and down too.
pub(crate) fn table_positions(width: f32) -> &'static [SwingPosition] {
    const NARROW: [SwingPosition; 4] = [
        SwingPosition::Address,
        SwingPosition::Top,
        SwingPosition::Impact,
        SwingPosition::Finish,
    ];
    if width >= WIDE_TABLE {
        &SwingPosition::ALL
    } else {
        &NARROW
    }
}

/// Column width from which the metrics table shows the lead arm
/// positions.
pub(crate) const WIDE_TABLE: f32 = 560.0;

/// A cell of the metrics table: its value on screen (colored by its
/// grade), its grade, and the value as a tile reads it ([`graded_shown`],
/// for the hover text).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cell {
    pub(crate) shown: Shown,
    pub(crate) grade: Option<MetricGrade>,
    pub(crate) worded: Shown,
}

/// The metrics table: each metric measured at any of `positions`, with its
/// value at each (a dash where it was not measured), colored by its grade
/// in `grades`.
pub(crate) fn metric_rows(
    report: &SwingReportFile,
    imperial: bool,
    positions: &[SwingPosition],
    grades: Option<&ReferenceGrades>,
) -> Vec<(String, Vec<Cell>)> {
    report
        .metrics
        .iter()
        .filter(|m| !NOT_IN_TABLE.contains(&m.id.as_str()))
        .filter_map(|m| {
            let cells: Vec<Option<Cell>> = positions
                .iter()
                .map(|&p| {
                    let v = m.values.at(p)?;
                    let cm = m.cm.as_ref().and_then(|c| c.at(p));
                    let grade = grades.and_then(|g| g.grade(&m.id, p.key())).cloned();
                    let worded = graded_shown(&m.id, &m.unit, v, cm, imperial, grade.as_ref());
                    let shown = Shown {
                        color: worded.color,
                        ..metric_shown(&m.id, &m.unit, v, cm, imperial)
                    };
                    Some(Cell {
                        shown,
                        grade,
                        worded,
                    })
                })
                .collect();
            cells.iter().any(Option::is_some).then(|| {
                (
                    metric_label(&m.id),
                    cells
                        .into_iter()
                        .map(|c| {
                            c.unwrap_or_else(|| Cell {
                                shown: Shown::none(),
                                grade: None,
                                worded: Shown::none(),
                            })
                        })
                        .collect(),
                )
            })
        })
        .collect()
}

/// A band `low`–`high` in `unit` on screen: degrees whole, inches to 0.1
/// (centimeters to 0.1 in metric units, rounded after the conversion),
/// seconds to 0.01, ratios to 0.1.
pub(crate) fn band_text(low: f64, high: f64, unit: &str, imperial: bool) -> String {
    match unit {
        "degrees" => format!("{}\u{2013}{}\u{b0}", fixed(low, 0), fixed(high, 0)),
        "inches" if imperial => format!("{}\u{2013}{} in", fixed(low, 1), fixed(high, 1)),
        "inches" => format!(
            "{}\u{2013}{} cm",
            fixed(low * 2.54, 1),
            fixed(high * 2.54, 1)
        ),
        "seconds" => format!("{}\u{2013}{} s", fixed(low, 2), fixed(high, 2)),
        _ => format!("{}\u{2013}{}", fixed(low, 1), fixed(high, 1)),
    }
}

/// A graded cell's hover text: the reference and its band (the fade and
/// the draw band of a shot-shape band), the value as its tile reads it
/// (`shown`), and the words for a value off the band.
pub(crate) fn band_tip(
    reference: ReferenceKind,
    grade: &MetricGrade,
    shown: &Shown,
    imperial: bool,
) -> String {
    let band = |b: [f64; 2]| band_text(b[0], b[1], &grade.unit, imperial);
    let mut parts = vec![match (grade.fade, grade.draw) {
        (None, None) => format!("{} {}", reference.label(), band([grade.low, grade.high])),
        (fade, draw) => {
            let mut s = reference.label().to_owned();
            if let Some(f) = fade {
                s.push_str(&format!(" \u{b7} fade {}", band(f)));
            }
            if let Some(d) = draw {
                s.push_str(&format!(" \u{b7} draw {}", band(d)));
            }
            s
        }
    }];
    parts.push(match shown.word {
        Some(word) => format!("{} {word}", shown.text),
        None => shown.text.clone(),
    });
    if let Some(word) = &grade.word {
        parts.push(word.clone());
    }
    parts.join(" \u{b7} ")
}

/// Height of a row of the swing tables.
const TABLE_ROW_H: f32 = 22.0;

/// A shot's tempo tiles across `width`, once for all its cameras
/// ([`tempo_values`]).
fn tempo_row(ui: &mut egui::Ui, tiles: &[FrameValue], width: f32) {
    let tempo = value_tiles(ui, tiles, width);
    devices::mark(ui, || "swing_tempo".to_owned(), &tempo);
}

/// A tile of the selected-frame row: its name and the metric it shows,
/// from the cameras of which view.
struct FrameTile {
    label: &'static str,
    view: SwingView,
    id: &'static str,
}

/// The selected-frame row, left to right.
const FRAME_TILES: [FrameTile; 6] = [
    FrameTile {
        label: "Hip turn",
        view: SwingView::FaceOn,
        id: "hip_turn",
    },
    FrameTile {
        label: "Shoulder turn",
        view: SwingView::FaceOn,
        id: "shoulder_turn",
    },
    FrameTile {
        label: "Hip sway",
        view: SwingView::FaceOn,
        id: "hip_sway",
    },
    FrameTile {
        label: "Head sway",
        view: SwingView::FaceOn,
        id: "head_lateral",
    },
    FrameTile {
        label: "Forward bend",
        view: SwingView::DownTheLine,
        id: "spine_forward_bend",
    },
    FrameTile {
        label: "Hip depth",
        view: SwingView::DownTheLine,
        id: "early_extension",
    },
];

/// The spine tile when no camera is down the line.
const SPINE_FACE_ON: FrameTile = FrameTile {
    label: "Side bend",
    view: SwingView::FaceOn,
    id: "lateral_spine_tilt",
};

/// One tile's content: the metric's id, its name, its value on screen
/// (with a direction word) in its grade's color, and its grade.
pub(crate) struct FrameValue {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) text: String,
    pub(crate) word: Option<&'static str>,
    pub(crate) color: egui::Color32,
    pub(crate) grade: Option<MetricGrade>,
}

/// The body at the replay's position: per tile, the metric from each
/// camera whose report is of the tile's view, at the frame that camera
/// shows, the mean when several have a value. A dash where none has. The
/// spine is the down-the-line forward bend, or the face-on side bend when
/// no report is down the line. Values read as in the metrics table.
///
/// A tile is graded only where the frame shown is a key position's (or
/// next to it, [`key_position`]): with that position's grade, the grade the
/// cameras showing it share ([`shared_grade`]). Elsewhere it is plain.
pub(crate) fn frame_tiles(swings: &[Swing], imperial: bool) -> Vec<FrameValue> {
    let down_the_line = swings
        .iter()
        .any(|s| s.report.view == Some(SwingView::DownTheLine));
    FRAME_TILES
        .iter()
        .map(|tile| {
            let tile = if tile.id == "spine_forward_bend" && !down_the_line {
                &SPINE_FACE_ON
            } else {
                tile
            };
            let samples: Vec<_> = swings
                .iter()
                .filter(|s| s.report.view == Some(tile.view))
                .filter_map(|s| {
                    let t = s.t_ns?;
                    let sample = s.report.sample(tile.id, t)?;
                    let grade =
                        key_position(s.report, t).and_then(|p| s.grades?.grade(tile.id, p.key()));
                    Some((sample, grade))
                })
                .collect();
            let (shown, grade) = match samples.as_slice() {
                [] => (Shown::none(), None),
                [(first, _), ..] => {
                    let n = samples.len() as f64;
                    let value = samples.iter().map(|(s, _)| s.value).sum::<f64>() / n;
                    let cm = samples
                        .iter()
                        .map(|(s, _)| s.cm)
                        .sum::<Option<f64>>()
                        .map(|sum| sum / n);
                    let grade = shared_grade(samples.iter().map(|(_, g)| *g));
                    let shown =
                        graded_shown(tile.id, &first.unit, value, cm, imperial, grade.as_ref());
                    (shown, grade)
                }
            };
            FrameValue {
                id: tile.id,
                label: tile.label,
                text: shown.text,
                word: shown.word,
                color: shown.color,
                grade,
            }
        })
        .collect()
}

/// The selected-frame tiles across `width`, under the tempo row (marks
/// `frame_metrics`, and `frame_metric:<id>` per tile).
fn frame_row(ui: &mut egui::Ui, tiles: &[FrameValue], width: f32) {
    let row = value_tiles(ui, tiles, width);
    devices::mark(ui, || "frame_metrics".to_owned(), &row);
    if cfg!(test) {
        for (tile, rect) in tiles.iter().zip(tile_rects(row.rect, tiles.len())) {
            let shown = ui.interact(rect, ui.id().with(tile.id), egui::Sense::hover());
            devices::mark(ui, || format!("frame_metric:{}", tile.id), &shown);
        }
    }
}

/// `tiles` across `width`, each value in its color.
pub(crate) fn value_tiles(ui: &mut egui::Ui, tiles: &[FrameValue], width: f32) -> egui::Response {
    let shown: Vec<(&str, &str, Option<&str>, egui::Color32)> = tiles
        .iter()
        .map(|t| (t.label, t.text.as_str(), t.word, t.color))
        .collect();
    worded_tiles(ui, &shown, width)
}

/// The miss line's entries: each tile off its band (graded, not
/// [`Fit::Both`]) with its name, its words and its fit.
pub(crate) fn miss_entries(tiles: &[FrameValue]) -> Vec<(&'static str, String, Fit)> {
    tiles
        .iter()
        .filter_map(|t| {
            let grade = t.grade.as_ref().filter(|g| g.fit != Fit::Both)?;
            Some((t.label, grade.word.clone()?, grade.fit))
        })
        .collect()
}

/// Size of the miss line's text.
const MISS_SIZE: f32 = 13.0;

/// The miss line's text: each entry's name muted and its words in its fit's
/// color, joined by middle dots.
pub(crate) fn miss_job(entries: &[(&str, String, Fit)], width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat {
        font_id: egui::FontId::proportional(MISS_SIZE),
        color,
        ..Default::default()
    };
    for (k, (label, words, fit)) in entries.iter().enumerate() {
        if k > 0 {
            job.append(" \u{b7} ", 0.0, format(theme::TEXT_MUTED));
        }
        job.append(label, 0.0, format(theme::TEXT_MUTED));
        job.append(&format!(" {words}"), 0.0, format(grade_color(*fit)));
    }
    job.wrap.max_width = width;
    job.wrap.max_rows = 2;
    job
}

/// The miss line under the tiles, `width` wide: the tiles off their band
/// ([`miss_entries`]), at most two lines. With none, a line's height is
/// kept empty while the shot has grades (`reserve`), so the tables below
/// hold still as the playhead passes a key position. Mark `frame_misses`.
pub(crate) fn miss_line(
    ui: &mut egui::Ui,
    entries: &[(&str, String, Fit)],
    width: f32,
    reserve: bool,
) {
    if entries.is_empty() && !reserve {
        return;
    }
    let galley = ui.fonts_mut(|f| f.layout_job(miss_job(entries, width)));
    let line = ui.fonts_mut(|f| f.row_height(&egui::FontId::proportional(MISS_SIZE)));
    let height = galley.size().y.max(line) + 6.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !entries.is_empty() {
        ui.painter()
            .galley(rect.left_top() + egui::vec2(0.0, 4.0), galley, theme::TEXT);
    }
    devices::mark(ui, || "frame_misses".to_owned(), &response);
}

/// One camera's swing metrics, `width` wide: its body measurements at the
/// key positions the width has room for ([`table_positions`]), colored by
/// their grades against `reference` (`grades`), each graded cell's band and
/// words on hover. Nothing when none was measured.
fn swing_metrics(
    ui: &mut egui::Ui,
    camera: &str,
    report: &SwingReportFile,
    imperial: bool,
    width: f32,
    grades: Option<&ReferenceGrades>,
    reference: ReferenceKind,
) {
    let positions = table_positions(width);
    let metrics = metric_rows(report, imperial, positions, grades);
    if metrics.is_empty() {
        return;
    }
    ui.add_space(8.0);
    let n = positions.len() as f32;
    let share = if positions.len() > 4 { 0.22 } else { 0.27 };
    let name_w = (width * share).max(100.0);
    let value_w = ((width - name_w) / n).max(1.0);
    let columns: Vec<(f32, f32)> = (0..positions.len())
        .map(|i| (name_w + i as f32 * value_w, value_w))
        .collect();
    let rows: Vec<TableRow> = metrics
        .iter()
        .map(|(name, cells)| {
            (
                Some(name.as_str()),
                cells
                    .iter()
                    .map(|c| TableCell {
                        text: c.shown.text.as_str(),
                        word: c.shown.word,
                        color: c.shown.color,
                        tip: c
                            .grade
                            .as_ref()
                            .map(|g| band_tip(reference, g, &c.worded, imperial)),
                    })
                    .collect(),
            )
        })
        .collect();
    let heading: Vec<&str> = positions.iter().map(|p| p.label()).collect();
    table(ui, camera, width, &columns, Some(&heading), &rows);
}

/// A cell of [`table`]: its value, its direction word (if any), its color
/// and its hover text (if any).
struct TableCell<'a> {
    text: &'a str,
    word: Option<&'a str>,
    color: egui::Color32,
    tip: Option<String>,
}

/// A row of [`table`]: its name, if any, and its cells.
type TableRow<'a> = (Option<&'a str>, Vec<TableCell<'a>>);

/// A heading over a column `room` wide, in small capitals: one line, or two
/// split at a space when it doesn't fit on one.
fn heading_lines(painter: &egui::Painter, heading: &str, room: f32) -> Vec<String> {
    let upper = heading.to_uppercase();
    let width = |s: &str| {
        painter
            .layout_no_wrap(
                s.to_owned(),
                egui::FontId::proportional(10.0),
                theme::TEXT_MUTED,
            )
            .size()
            .x
    };
    if width(&upper) <= room {
        return vec![upper];
    }
    match upper.rsplit_once(' ') {
        Some((first, second)) => vec![first.to_owned(), second.to_owned()],
        None => vec![upper],
    }
}

/// A table `width` wide: an optional heading row of small capitals over
/// `columns` (left edge and width of each value column), then `rows`, each
/// an optional name at the left edge and its cells (each with its direction
/// word, if any, in its color, and its hover text), on alternate stripes.
/// `salt` keeps its hover areas apart from another table's.
fn table(
    ui: &mut egui::Ui,
    salt: &str,
    width: f32,
    columns: &[(f32, f32)],
    heading: Option<&[&str]>,
    rows: &[TableRow],
) {
    let painter = ui.painter().clone();
    let headings: Vec<Vec<String>> = heading.map_or_else(Vec::new, |h| {
        columns
            .iter()
            .zip(h)
            .map(|((_, w), h)| heading_lines(&painter, h, w - 8.0))
            .collect()
    });
    let lines = headings.iter().map(Vec::len).max().unwrap_or(0);
    let head_h = match lines {
        0 => 0.0,
        1 => 16.0,
        _ => 28.0,
    };
    let height = head_h + rows.len() as f32 * TABLE_ROW_H;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    let text = |at: egui::Pos2, s: &str, size: f32, color: egui::Color32| {
        let galley = painter.layout_no_wrap(s.to_owned(), egui::FontId::proportional(size), color);
        let y = at.y - galley.size().y / 2.0;
        painter.galley(egui::pos2(at.x, y), galley, color);
    };
    for ((left, _), lines) in columns.iter().zip(&headings) {
        let line_h = 12.0;
        let top = rect.top() + head_h / 2.0 - (lines.len() as f32 - 1.0) * line_h / 2.0;
        for (k, line) in lines.iter().enumerate() {
            let at = egui::pos2(rect.left() + left + 8.0, top + k as f32 * line_h);
            text(at, line, 10.0, theme::TEXT_MUTED);
        }
    }
    for (k, (name, cells)) in rows.iter().enumerate() {
        let top = rect.top() + head_h + k as f32 * TABLE_ROW_H;
        let row =
            egui::Rect::from_min_size(egui::pos2(rect.left(), top), egui::vec2(width, TABLE_ROW_H));
        if k % 2 == 0 && name.is_some() {
            painter.rect_filled(row, 4.0, theme::SURFACE);
        }
        let y = row.center().y;
        if let Some(name) = name {
            text(
                egui::pos2(rect.left() + 8.0, y),
                name,
                13.0,
                theme::TEXT_MUTED,
            );
        }
        for (j, ((left, column_w), cell)) in columns.iter().zip(cells).enumerate() {
            let (value, word) = worded_value(&painter, cell.text, cell.word, 14.0, column_w - 12.0);
            let at = egui::pos2(rect.left() + left + 8.0, y - value.size().y / 2.0);
            painter.galley(at, value, cell.color);
            if let Some((word, offset)) = word {
                painter.galley(at + offset, word, cell.color);
            }
            if let Some(tip) = &cell.tip {
                let area = egui::Rect::from_min_size(
                    egui::pos2(rect.left() + left, top),
                    egui::vec2(*column_w, TABLE_ROW_H),
                );
                let hover = ui.interact(area, ui.id().with((salt, k, j)), egui::Sense::hover());
                devices::mark(ui, || format!("cell:{salt}:{k}:{j}"), &hover);
                hover.on_hover_text(tip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::devices::Marks;
    use crate::panels::settings::SettingsForm;
    use crate::types::{
        ActorStatus, ActorStatusResponse, CameraPosition, FlighthookConfig, ShotKey, ShotRow,
        ShotVideo,
    };
    use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, vec2};

    #[test]
    fn values_that_round_to_zero_show_no_minus_sign() {
        assert_eq!(fixed(-0.004, 2), "0.00");
        assert_eq!(fixed(-0.3, 0), "0");
        assert_eq!(fixed(-0.6, 0), "-1");
        assert_eq!(degrees(-0.2), "0\u{b0}");
        assert_eq!(fixed(1.25, 1), "1.2");
    }

    /// A saved config with cameras `indices` (camera.N named "Cam N"):
    /// camera.0 faces the golfer, the others look down the target line.
    /// A config with cameras `indices`, body tracking in Live mode (the
    /// mode most of these tests are about).
    fn config(indices: &[u32]) -> FlighthookConfig {
        let mut config = FlighthookConfig::default();
        config.body_tracking.mode = BodyTrackingMode::Live;
        for i in indices {
            config.camera.insert(
                i.to_string(),
                CameraSection {
                    name: format!("Cam {i}"),
                    camera: CameraSection::TEST_PATTERN.into(),
                    position: if *i == 0 {
                        CameraPosition::GolferFront
                    } else {
                        CameraPosition::TargetLine
                    },
                    ..CameraSection::default()
                },
            );
        }
        config
    }

    /// The app on the Shots tab's Cards view, showing the shot's data, with
    /// cameras `cameras` connected.
    fn app(cameras: &[u32], supported: bool) -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        let mut form = SettingsForm::default();
        form.load_from(&config(cameras));
        app.settings = form;
        app.cameras_supported = supported;
        app.active_tab = Tab::Shots;
        app.shots_view = ShotsView::Cards;
        app.shots_pane = ShotsPane::Data;
        for i in cameras {
            app.actors.insert(
                format!("camera.{i}"),
                ActorStatusResponse::new(format!("Cam {i}"), ActorStatus::Connected),
            );
        }
        app
    }

    /// Draws the tab bar and the active tab's panel (the Shots tab, or
    /// nothing), as the app does; keeps what the last frame painted as text
    /// and the centers of the circles it painted.
    struct Screen(
        egui::Context,
        egui::Vec2,
        std::cell::RefCell<Vec<String>>,
        std::cell::RefCell<Vec<Pos2>>,
        std::cell::RefCell<Vec<(String, egui::Color32)>>,
    );

    impl Screen {
        fn new() -> Self {
            Self::sized(1600.0, 900.0)
        }

        fn sized(w: f32, h: f32) -> Self {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            Self(
                ctx,
                vec2(w, h),
                std::cell::RefCell::default(),
                std::cell::RefCell::default(),
                std::cell::RefCell::default(),
            )
        }

        fn frame(&self, app: &mut FlighthookApp, events: Vec<Event>) -> Marks {
            self.0.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.1)),
                events,
                ..Default::default()
            };
            let out = self.0.run(input, |ctx| {
                app.sync_cameras_pane();
                app.drop_hidden_card_clips();
                app.sync_live_views(ctx);
                egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
                    let _ = app.render_tabs(ui);
                });
                egui::CentralPanel::default().show(ctx, |ui| {
                    if app.active_tab == Tab::Shots {
                        app.render_shots_panel(ui);
                    }
                });
            });
            *self.2.borrow_mut() = painted_texts(&out.shapes);
            *self.3.borrow_mut() = painted_circles(&out.shapes);
            *self.4.borrow_mut() = painted_colored(&out.shapes);
            self.0
                .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
                .unwrap_or_default()
        }

        /// What the last frame painted as text.
        fn texts(&self) -> Vec<String> {
            self.2.borrow().clone()
        }

        /// The color of the last frame's text `text` (the first painted).
        fn color_of(&self, text: &str) -> Option<egui::Color32> {
            self.4
                .borrow()
                .iter()
                .find(|(t, _)| t == text)
                .map(|(_, c)| *c)
        }

        /// How many circles the last frame painted inside `rect`.
        fn circles_in(&self, rect: Rect) -> usize {
            self.3
                .borrow()
                .iter()
                .filter(|c| rect.contains(**c))
                .count()
        }

        fn click(&self, app: &mut FlighthookApp, at: Pos2) -> Marks {
            let button = |pressed| Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(app, vec![Event::PointerMoved(at)]);
            self.frame(app, vec![button(true)]);
            self.frame(app, vec![button(false)]);
            self.frame(app, Vec::new())
        }
    }

    #[test]
    fn the_toggle_and_live_show_with_a_configured_camera_where_cameras_work() {
        let screen = Screen::new();
        for (cameras, supported, shown) in [
            (&[0][..], true, true),
            (&[0, 1][..], true, true),
            (&[][..], true, false),
            (&[0][..], false, false),
        ] {
            let mut app = app(cameras, supported);
            assert_eq!(app.cameras_shown(), shown, "{cameras:?} {supported}");
            for view in [ShotsView::Table, ShotsView::Cards] {
                app.shots_view = view;
                let marks = screen.frame(&mut app, Vec::new());
                assert_eq!(
                    marks.contains_key("pane_toggle"),
                    shown,
                    "{view:?} {cameras:?} {supported}"
                );
                assert!(marks.contains_key("view_toggle"), "{view:?}");
                assert!(!marks.contains_key("live_toggle"), "not in the Data pane");
            }
            app.set_shots_pane(ShotsPane::Cameras);
            let marks = screen.frame(&mut app, Vec::new());
            assert_eq!(
                marks.contains_key("live_toggle"),
                shown,
                "Live in the Cameras pane: {cameras:?} {supported}"
            );
            assert!(!marks.contains_key("live_entry"), "nothing over the list");
        }
        // Not before the settings have loaded.
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.cameras_supported = true;
        assert!(!app.cameras_shown());
        // No top-level tab for the cameras.
        assert_eq!(
            crate::panels::TABS.map(|(t, _)| t),
            [Tab::Devices, Tab::Shots, Tab::Log]
        );
    }

    #[test]
    fn the_toggle_leads_the_toolbar_in_both_views() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        let marks = screen.frame(&mut app, Vec::new());
        let toggle = marks["pane_toggle"].0;
        let data = marks["pane:data"].0;
        let cameras = marks["pane:cameras"].0;
        assert!(data.right() <= cameras.left() + 0.5, "Data | Cameras");
        assert!(toggle.top() < 80.0, "in the toolbar");
        // The shot list starts with the view selector, under the toolbar.
        assert!(!marks.contains_key("live_entry"));
        let selector = marks["view_selector"].0;
        assert!(selector.top() > toggle.bottom());
        assert!(selector.right() < 400.0, "on the left");
        // The Table view: the same toggle in the same place, Cards | Table
        // after it, no shot list and no Live.
        let view = marks["view_toggle"].0;
        app.shots_view = ShotsView::Table;
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(marks["pane_toggle"].0, toggle);
        assert_eq!(marks["view_toggle"].0, view);
        assert!(!marks.contains_key("view_selector"));
        assert!(!marks.contains_key("live_toggle"));
        assert!(!app.cameras_pane_shown());
        // Cameras chosen in the Table view shows the Cameras pane.
        app.set_shots_pane(ShotsPane::Cameras);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.cameras_pane_shown());
        assert!(marks.contains_key("live_toggle"));
        assert!(!marks.contains_key("view_toggle"));
        assert!(marks.contains_key("view_selector"));
    }

    #[test]
    fn the_toolbar_starts_with_data_cameras_and_cards_table_shows_only_for_data() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        let marks = screen.frame(&mut app, Vec::new());
        let (pane, view, export, trash) = (
            marks["pane_toggle"].0,
            marks["view_toggle"].0,
            marks["export"].0,
            marks["clear_history"].0,
        );
        // Data | Cameras, Cards | Table, then the export and trash icons.
        assert!(pane.right() <= view.left() + 0.5);
        assert!(view.right() <= export.left() + 0.5);
        assert!(export.right() <= trash.left() + 0.5);
        assert!(pane.left() < 12.0, "at the toolbar's left edge: {pane:?}");
        assert!(!marks.contains_key("units"), "no units in the toolbar");

        assert!(
            !marks.contains_key("live_toggle"),
            "no Live in the Data pane"
        );

        // Cameras: Cards | Table is not drawn and the choice is kept; the
        // Live switch takes its slot, as tall as the pills.
        screen.click(&mut app, marks["pane:cameras"].0.center());
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.cameras_pane_shown());
        assert!(!marks.contains_key("view_toggle"));
        let live = marks["live_toggle"].0;
        assert!((live.left() - view.left()).abs() < 2.0, "{live:?} {view:?}");
        assert!(
            (live.center().y - view.center().y).abs() < 1.0,
            "{live:?} {view:?} {pane:?}"
        );
        assert!((live.height() - view.height()).abs() < 1.0);
        assert!(live.right() <= export.left() + 0.5);
        assert_eq!(
            marks["pane_toggle"].0.left(),
            pane.left(),
            "Data | Cameras stays first"
        );
        assert_eq!(app.shots_view, ShotsView::Cards);
        assert!(marks.contains_key("export") && marks.contains_key("clear_history"));

        // Back to Data: it returns where it was, with the same choice.
        screen.click(&mut app, marks["pane:data"].0.center());
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(marks["view_toggle"].0, view);
        assert_eq!(app.shots_view, ShotsView::Cards);

        assert!(!marks.contains_key("live_toggle"));

        // The Table view: Data | Cameras still leads, Cards | Table follows
        // it where it is in the Cards view.
        app.shots_view = ShotsView::Table;
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(marks["pane_toggle"].0, pane);
        assert_eq!(marks["view_toggle"].0, view);
    }

    /// Cameras picked from the Table view gives the screen it gives from
    /// the Cards view: the shot list, the Cameras pane and the Live switch
    /// in the Cards | Table slot. Data brings the Table view back.
    #[test]
    fn cameras_from_the_table_view_is_the_cameras_pane_of_the_cards_view() {
        let from = |view: ShotsView| {
            let screen = Screen::new();
            let mut app = app_with_shots(3);
            app.shots_view = view;
            let marks = screen.frame(&mut app, Vec::new());
            let before = (marks["pane_toggle"].0, marks["view_toggle"].0);
            assert_eq!(
                marks.contains_key("view_selector"),
                view == ShotsView::Cards,
                "the shot list only in the Cards view"
            );
            screen.click(&mut app, marks["pane:cameras"].0.center());
            let marks = screen.frame(&mut app, Vec::new());
            let texts = screen.texts();
            (screen, app, marks, texts, before)
        };
        let (_, _, cards, cards_texts, _) = from(ShotsView::Cards);
        let (screen, mut app, table, table_texts, (pane, view)) = from(ShotsView::Table);

        assert!(app.cameras_pane_shown());
        assert_eq!(app.shots_layout(), ShotsLayout::Cameras);
        assert_eq!(app.shots_view, ShotsView::Table, "the choice is kept");
        assert!(!table.contains_key("view_toggle"));
        assert!(table.contains_key("view_selector"), "the shot list");
        assert!(!app.live_selected);
        assert_eq!(app.shown_index(), Some(2), "the latest shot");
        for mark in [
            "pane_toggle",
            "live_toggle",
            "view_selector",
            "shot_entry:0",
        ] {
            assert_eq!(table[mark].0, cards[mark].0, "{mark}");
        }
        assert_eq!(table.len(), cards.len());
        assert_eq!(table_texts, cards_texts, "the same screen");

        // Data: the Table view again, Cards | Table where it was.
        screen.click(&mut app, table["pane:data"].0.center());
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.cameras_pane_shown());
        assert_eq!(app.shots_layout(), ShotsLayout::Table);
        assert_eq!(app.shots_view, ShotsView::Table);
        assert_eq!(marks["pane_toggle"].0, pane);
        assert_eq!(marks["view_toggle"].0, view);
        assert!(!marks.contains_key("live_toggle"));
        assert!(!marks.contains_key("view_selector"), "no shot list");
    }

    /// A kept Cameras choice opens on the Cameras pane in the Table view.
    #[test]
    fn a_kept_cameras_choice_opens_the_cameras_pane_in_the_table_view() {
        let screen = Screen::new();
        app(&[0], true).set_shots_pane(ShotsPane::Cameras);
        let kept = ShotsPane::stored();
        // Data is stored again before anything is checked, so a failure
        // leaves no Cameras choice for later tests on this thread.
        crate::local::store(PANE_KEY, ShotsPane::Data.key());
        assert_eq!(kept, ShotsPane::Cameras);
        let mut app = app(&[0], true);
        app.shots_pane = kept;
        app.shots_view = ShotsView::Table;
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.cameras_pane_shown());
        assert!(app.live_selected, "no shots: Live");
        assert!(marks.contains_key("live_toggle"));
        assert!(marks.contains_key("live_title"));
        assert!(!marks.contains_key("view_toggle"));
    }

    #[test]
    fn the_pane_choice_is_kept_and_data_leaves_live() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        let marks = screen.frame(&mut app, Vec::new());
        let at = marks["pane:cameras"].0.center();
        screen.click(&mut app, at);
        assert_eq!(app.shots_pane, ShotsPane::Cameras);
        assert!(app.cameras_pane_shown());
        assert_eq!(
            ShotsPane::stored(),
            ShotsPane::Cameras,
            "kept on this device"
        );
        // No shots: Live; Data leaves it.
        assert!(app.live_selected);
        let marks = screen.frame(&mut app, Vec::new());
        let at = marks["pane:data"].0.center();
        screen.click(&mut app, at);
        assert!(!app.live_selected);
        assert_eq!(app.shots_pane, ShotsPane::Data);
        assert!(!app.cameras_pane_shown());
        assert_eq!(ShotsPane::stored(), ShotsPane::Data);
    }

    /// The app of [`app`] with shots #1..=`n` from the mock monitor, the
    /// first one pinned in the shot list.
    fn app_with_shots(n: u32) -> FlighthookApp {
        let mut app = app(&[0], true);
        app.shots = (1..=n).map(|i| shot_row(i, &[])).collect();
        app.shots_rev += 1;
        app.observe_shots(0.0);
        app.select_shot(0);
        app
    }

    #[test]
    fn entering_the_cameras_pane_without_shots_turns_live_on() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.live_selected);
        let marks = screen.click(&mut app, marks["pane:cameras"].0.center());
        assert!(app.live_selected, "no shot history: Live");
        assert!(marks.contains_key("live_title"));
        assert!(marks.contains_key("live_toggle"));
    }

    #[test]
    fn entering_the_cameras_pane_with_shots_shows_the_latest_shot() {
        let screen = Screen::new();
        let mut app = app_with_shots(3);
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(app.shown_index(), Some(0), "#1 pinned");
        screen.click(&mut app, marks["pane:cameras"].0.center());
        assert!(!app.live_selected);
        assert_eq!(app.shown_index(), Some(2), "the latest, pin dropped");
        assert!(app.following());

        // Leaving and coming back applies the rule again: another tab ...
        app.select_live();
        app.select_shot(0);
        assert!(!app.live_selected, "picking a shot leaves Live");
        app.select_live();
        app.active_tab = Tab::Log;
        screen.frame(&mut app, Vec::new());
        assert!(app.live_selected, "kept off screen");
        app.active_tab = Tab::Shots;
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.live_selected);
        assert!(!marks.contains_key("live_title"));
        assert_eq!(app.shown_index(), Some(2));

        // ... and Data -> Cameras in the Table view.
        app.select_shot(0);
        app.set_shots_pane(ShotsPane::Data);
        app.shots_view = ShotsView::Table;
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.cameras_pane_shown());
        screen.click(&mut app, marks["pane:cameras"].0.center());
        assert_eq!(app.shots_view, ShotsView::Table);
        assert!(app.cameras_pane_shown());
        assert!(!app.live_selected);
        assert_eq!(app.shown_index(), Some(2), "pin dropped");

        // Showing the pane again with no shots: Live.
        app.shots.clear();
        app.shots_rev += 1;
        app.active_tab = Tab::Log;
        screen.frame(&mut app, Vec::new());
        app.active_tab = Tab::Shots;
        screen.frame(&mut app, Vec::new());
        assert!(app.live_selected);
    }

    #[test]
    fn entering_the_cameras_pane_on_a_device_without_shots_turns_live_on() {
        let screen = Screen::new();
        let mut app = app_with_shots(2);
        let mut trigger = shot_row(3, &[]);
        trigger.actor = "mock_monitor.1".into();
        trigger.launch = None;
        app.shots.push(trigger);
        app.shots_rev += 1;
        app.device_filter = Some("mock_monitor.1".into());
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(app.shown_index(), None, "no shot with data on it");
        screen.click(&mut app, marks["pane:cameras"].0.center());
        assert!(app.live_selected);
    }

    #[test]
    fn the_live_switch_turns_live_on_and_off() {
        let screen = Screen::new();
        let mut app = app_with_shots(3);
        app.set_shots_pane(ShotsPane::Cameras);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!app.live_selected);
        let live = marks["live_toggle"].0.center();
        let marks = screen.click(&mut app, live);
        assert!(app.live_selected);
        assert!(marks.contains_key("live_title"));
        // Off: the latest shot.
        let marks = screen.click(&mut app, live);
        assert!(!app.live_selected);
        assert!(!marks.contains_key("live_title"));
        assert_eq!(app.shown_index(), Some(2));
        // Picking a shot leaves Live; off again shows the picked shot.
        screen.click(&mut app, live);
        screen.click(&mut app, marks["shot_entry:0"].0.center());
        assert!(!app.live_selected);
        assert_eq!(app.shown_index(), Some(0));
        screen.click(&mut app, live);
        assert!(app.live_selected);
        let marks = screen.click(&mut app, live);
        assert_eq!(app.shown_index(), Some(0), "the pinned shot");
        // A new shot keeps Live.
        screen.click(&mut app, live);
        app.shots.push(shot_row(4, &[]));
        app.shots_rev += 1;
        app.observe_shots(5.0);
        screen.frame(&mut app, Vec::new());
        assert!(app.live_selected);
        // Data leaves Live.
        screen.click(&mut app, marks["pane:data"].0.center());
        assert!(!app.live_selected);
        assert!(!app.cameras_pane_shown());
    }

    #[test]
    fn clear_history_in_the_cameras_pane_turns_live_on() {
        let screen = Screen::new();
        for view in [ShotsView::Cards, ShotsView::Table] {
            let mut app = app_with_shots(2);
            app.shots_view = view;
            screen.frame(&mut app, Vec::new());
            // In the Data pane it changes nothing.
            app.clear_shot_history();
            screen.frame(&mut app, Vec::new());
            assert!(!app.live_selected, "{view:?}");
            app.shots = vec![shot_row(1, &[])];
            app.shots_rev += 1;
            app.set_shots_pane(ShotsPane::Cameras);
            let marks = screen.frame(&mut app, Vec::new());
            assert!(app.cameras_pane_shown(), "{view:?}");
            assert!(!app.live_selected, "{view:?}: the shot");
            assert!(!marks.contains_key("live_title"), "{view:?}");
            app.clear_shot_history();
            let marks = screen.frame(&mut app, Vec::new());
            assert!(app.live_selected, "{view:?}");
            assert!(marks.contains_key("live_title"), "{view:?}");
            assert_eq!(app.shots_view, view, "the Data pane's view is kept");
            app.set_shots_pane(ShotsPane::Data);
        }
    }

    #[test]
    fn clicking_live_shows_a_live_view_per_camera_side_by_side() {
        let screen = Screen::new();
        let mut app = app(&[0, 2], true);
        app.shots = vec![shot_row(1, &[])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("camera_live:camera.0"), "the shot");
        assert!(app.live.is_empty(), "no live fetch off Live");
        let at = marks["live_toggle"].0.center();
        let marks = screen.click(&mut app, at);
        assert!(app.live_selected);
        assert!(marks.contains_key("live_title"));
        let left = marks["camera_live:camera.0"].0;
        let right = marks["camera_live:camera.2"].0;
        assert!(left.right() < right.left(), "side by side");
        assert!((left.width() - right.width()).abs() < 0.5);
        assert!((left.height() - left.width() * DEFAULT_ASPECT).abs() < 0.5);
        assert!(
            left.left() > marks["view_selector"].0.right(),
            "right of the list"
        );
        assert_eq!(app.live.len(), 2);
    }

    #[test]
    fn the_views_fetch_one_request_at_a_time_and_stop_off_live() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.select_live();
        screen.frame(&mut app, Vec::new());
        let view = &app.live["camera.0"];
        assert!(view.in_flight, "the first request went out");
        let id = view.id;
        // No second request while it is out.
        screen.frame(&mut app, Vec::new());
        assert!(app.live["camera.0"].in_flight);
        // A frame lands: the next asks after it.
        app.live_replies(
            vec![(
                id,
                LiveReply::Frame {
                    seq: 7,
                    jpeg: vec![1],
                    pose: None,
                    state: PoseState::default(),
                },
            )],
            0.0,
        );
        let view = app.live.get_mut("camera.0").unwrap();
        assert!(!view.in_flight);
        assert_eq!(view.due(0.0), Some(7));
        // A failure waits before the next.
        view.reply(LiveReply::Failed("refused".into()), 10.0);
        assert_eq!(view.due(10.5), None);
        assert_eq!(view.due(11.0), Some(7));
        // The same frame again is not taken.
        view.due(11.0);
        view.reply(
            LiveReply::Frame {
                seq: 7,
                jpeg: vec![2],
                pose: None,
                state: PoseState::default(),
            },
            11.0,
        );
        assert_eq!(view.jpeg.as_ref().map(|(j, _)| j.clone()), Some(vec![1]));
        // A lower number is a restarted server: taken, and asked after.
        view.due(11.0);
        view.reply(
            LiveReply::Frame {
                seq: 3,
                jpeg: vec![3],
                pose: None,
                state: PoseState::default(),
            },
            11.0,
        );
        assert_eq!(view.after, 3);
        assert_eq!(view.jpeg.as_ref().map(|(j, _)| j.clone()), Some(vec![3]));
        assert_eq!(view.due(11.0), Some(3));
        // Another tab drops the views; a late reply is ignored.
        app.active_tab = Tab::Log;
        screen.frame(&mut app, Vec::new());
        assert!(app.live.is_empty());
        app.live_replies(vec![(id, LiveReply::Nothing(PoseState::default()))], 12.0);
        assert!(app.live.is_empty());
        // Back on Live: new views, numbered afresh.
        app.active_tab = Tab::Shots;
        screen.frame(&mut app, Vec::new());
        assert!(app.live["camera.0"].id > id);
        // The Data pane drops them too.
        app.set_shots_pane(ShotsPane::Data);
        screen.frame(&mut app, Vec::new());
        assert!(app.live.is_empty());
    }

    #[test]
    fn removing_the_cameras_leaves_live() {
        let screen = Screen::new();
        for view in [ShotsView::Cards, ShotsView::Table] {
            let mut app = app(&[0], true);
            app.shots_view = view;
            app.select_live();
            let marks = screen.frame(&mut app, Vec::new());
            assert!(app.cameras_pane_shown(), "{view:?}");
            assert!(marks.contains_key("live_title"), "{view:?}");
            assert_eq!(app.live.len(), 1, "{view:?}");
            app.settings.load_from(&config(&[]));
            let marks = screen.frame(&mut app, Vec::new());
            assert!(!app.live_selected, "{view:?}");
            assert!(!app.cameras_pane_shown(), "{view:?}");
            assert!(!marks.contains_key("live_title"), "{view:?}");
            assert!(marks.contains_key("view_toggle"), "{view:?}: Cards | Table");
            assert_eq!(app.shots_view, view, "the Data pane's view is kept");
            assert_eq!(app.active_tab, Tab::Shots);
            assert!(app.live.is_empty(), "{view:?}");
            app.set_shots_pane(ShotsPane::Data);
        }
    }

    /// A 64×48 JPEG, as a camera sends one.
    fn frame_jpeg() -> Vec<u8> {
        use stance::{Source, TestPattern, TestPatternConfig};
        let mut pattern = TestPattern::new(TestPatternConfig {
            width: 64,
            height: 48,
            paced: false,
            ..TestPatternConfig::default()
        })
        .unwrap();
        pattern
            .next_frame(std::time::Duration::ZERO)
            .unwrap()
            .unwrap()
            .jpeg
            .to_vec()
    }

    #[test]
    fn a_frame_shows_at_its_own_aspect() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.select_live();
        screen.frame(&mut app, Vec::new());
        let id = app.live["camera.0"].id;
        app.live_replies(
            vec![(
                id,
                LiveReply::Frame {
                    seq: 1,
                    jpeg: frame_jpeg(),
                    pose: None,
                    state: PoseState::default(),
                },
            )],
            0.0,
        );
        let marks = screen.frame(&mut app, Vec::new());
        let picture = marks["camera_live:camera.0"].0;
        assert!((picture.height() / picture.width() - 0.75).abs() < 0.01);
        assert!(app.live["camera.0"].jpeg.is_none(), "decoded");
        assert!(!marks.contains_key("camera_status:camera.0"));
    }

    #[test]
    fn a_camera_not_connected_shows_its_status_and_no_picture() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.select_live();
        screen.frame(&mut app, Vec::new());
        let id = app.live["camera.0"].id;
        app.live_replies(
            vec![(
                id,
                LiveReply::Frame {
                    seq: 1,
                    jpeg: frame_jpeg(),
                    pose: None,
                    state: PoseState::default(),
                },
            )],
            0.0,
        );
        screen.frame(&mut app, Vec::new());
        let actor = app.actors.get_mut("camera.0").unwrap();
        actor.observe(ActorStatus::Reconnecting, 0);
        actor
            .telemetry
            .insert("error".into(), "camera unplugged".into());
        let marks = screen.frame(&mut app, Vec::new());
        // The last frame gives way to the status, worded as on the card.
        assert!(marks.contains_key("camera_status:camera.0"));
        let picture = marks["camera_live:camera.0"].0;
        assert!(picture.contains_rect(marks["camera_status:camera.0"].0));
        assert_eq!(app.actors["camera.0"].state().label(), "RECONNECTING");
        // No actor yet: like the card of a camera that has not started.
        app.actors.clear();
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("camera_status:camera.0"));
        assert_eq!(new_actor(String::new()).state().label(), "CONNECTING");
    }

    #[test]
    fn columns_fit_the_panel_keeping_their_aspect() {
        let panel = vec2(1600.0, 900.0);
        // Two 16:9 cameras: limited by the width.
        let w = column_width(2, panel, 9.0 / 16.0);
        assert!((w - (1600.0 - GAP) / 2.0).abs() < 0.01);
        // One: limited by the height.
        let w = column_width(1, panel, 9.0 / 16.0);
        assert!((w * 9.0 / 16.0 + HEADING_H - 900.0).abs() < 0.01);
        // A portrait camera (16:9 on its side): limited by the height.
        let w = column_width(2, panel, 16.0 / 9.0);
        assert!((w * 16.0 / 9.0 + HEADING_H - 900.0).abs() < 0.01);
        assert_eq!(resolution_aspect("1280x720"), Some(0.5625));
        assert_eq!(resolution_aspect("640x0"), None);
        assert_eq!(resolution_aspect(""), None);
    }

    fn press(key: Key) -> Vec<Event> {
        vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }]
    }

    #[test]
    fn the_readout_names_every_value_and_each_cameras_rate() {
        let s = BodyTrackingSection::default();
        let one = [CameraReading {
            name: "Face On".into(),
            fps: Some(14.83),
            pose_ms: Some(19.4),
        }];
        assert_eq!(
            readout(&s, &one),
            "RTMPose-m · min score 0.30 · smoothing 1.0 · detect every 10 · 14.8 fps · 19 ms"
        );
        let two = [
            one[0].clone(),
            CameraReading {
                name: "Down the Line".into(),
                fps: Some(14.6),
                pose_ms: None,
            },
        ];
        let tuned = BodyTrackingSection {
            model: flighthook::PoseModel::RtmposeS,
            min_score: 0.45,
            smoothing: 0.0,
            detect_every: 30,
            show_skeleton: false,
            ..s.clone()
        };
        assert_eq!(
            readout(&tuned, &two),
            "RTMPose-s · min score 0.45 · smoothing off · detect every 30 · skeleton off · \
             Face On 14.8 fps 19 ms · Down the Line 14.6 fps"
        );
    }

    fn pose(x: f32) -> LivePose {
        LivePose {
            skeleton: "halpe26".into(),
            keypoints: vec![[x, 10.0, 0.9]; 26],
            bbox: None,
            ms: 20.0,
            posture: None,
        }
    }

    #[test]
    fn the_pose_drawn_is_the_one_that_came_with_the_picture() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.select_live();
        screen.frame(&mut app, Vec::new());
        let id = app.live["camera.0"].id;
        let frame = |seq, x| LiveReply::Frame {
            seq,
            jpeg: frame_jpeg(),
            pose: Some(pose(x)),
            state: PoseState {
                state: "ready".into(),
                error: None,
            },
        };
        app.live_replies(vec![(id, frame(1, 5.0))], 1.0);
        screen.frame(&mut app, Vec::new());
        assert_eq!(app.live["camera.0"].shown_pose, Some(pose(5.0)));
        // A newer frame waits to be decoded: the shown pose stays with the
        // shown picture until then.
        app.live_replies(vec![(id, frame(2, 7.0))], 1.1);
        assert_eq!(app.live["camera.0"].shown_pose, Some(pose(5.0)));
        screen.frame(&mut app, Vec::new());
        assert_eq!(app.live["camera.0"].shown_pose, Some(pose(7.0)));
        // A frame without a pose (models not ready) shows none.
        app.live_replies(
            vec![(
                id,
                LiveReply::Frame {
                    seq: 3,
                    jpeg: frame_jpeg(),
                    pose: None,
                    state: PoseState {
                        state: "loading".into(),
                        error: None,
                    },
                },
            )],
            1.2,
        );
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(app.live["camera.0"].shown_pose, None);
        // And the models' state shows over the picture.
        let band = marks["pose_status:camera.0"].0;
        assert!(marks["camera_live:camera.0"].0.contains_rect(band));
        assert_eq!(
            pose_status_text(&app.live["camera.0"].pose_state).as_deref(),
            Some("Loading body tracking models")
        );
    }

    #[test]
    fn the_models_state_reads_plainly() {
        let state = |s: &str, e: Option<&str>| PoseState {
            state: s.into(),
            error: e.map(str::to_owned),
        };
        assert_eq!(
            pose_status_text(&state("downloading", None)).as_deref(),
            Some("Downloading body tracking models")
        );
        assert_eq!(
            pose_status_text(&state("missing", None)).as_deref(),
            Some("Body tracking models not downloaded")
        );
        assert_eq!(
            pose_status_text(&state("failed", Some("no network"))).as_deref(),
            Some("Body tracking failed: no network")
        );
        for quiet in ["ready", "off", ""] {
            assert_eq!(pose_status_text(&state(quiet, None)), None, "{quiet}");
        }
    }

    #[test]
    fn a_views_rate_and_pose_time_are_measured() {
        let mut view = LiveView::new(1);
        assert_eq!(view.fps(0.0), None);
        for i in 0..16 {
            view.in_flight = true;
            view.reply(
                LiveReply::Frame {
                    seq: i + 1,
                    jpeg: Vec::new(),
                    pose: Some(pose(1.0)),
                    state: PoseState::default(),
                },
                10.0 + i as f64 / 15.0,
            );
        }
        let fps = view.fps(11.0).unwrap();
        assert!((fps - 15.0).abs() < 0.01, "{fps}");
        assert!((view.pose_ms.unwrap() - 20.0).abs() < 1e-4);
        // Long after the last frame: no rate.
        assert_eq!(view.fps(20.0), None);
    }

    #[test]
    fn room_is_left_for_the_readout_line() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.select_live();
        let marks = screen.frame(&mut app, Vec::new());
        let picture = marks["camera_live:camera.0"].0;
        let line = marks["pose_readout"].0;
        assert!(line.top() >= picture.bottom(), "under the live views");
        assert!(line.bottom() <= 900.0, "on screen");
        // The posture tiles sit between the picture and the line.
        let tiles = marks["posture:camera.0"].0;
        assert!(tiles.top() >= picture.bottom() && tiles.bottom() <= line.top());
    }

    // -----------------------------------------------------------------------
    // Phase C: a shot's cameras, swing data, live posture
    // -----------------------------------------------------------------------

    /// A face-on swing report, as stance writes it, with the golfer's height
    /// when `cm`.
    fn report(cm: bool) -> SwingReportFile {
        let cm_part = |top: f64, impact: f64| {
            if cm {
                format!(r#","cm":{{"top":{top},"impact":{impact}}}"#)
            } else {
                String::new()
            }
        };
        let json = format!(
            r#"{{"version":1,"view":"face_on","handed":"right",
            "events":{{"address":{{"t_ns":1000000000,"frame":120,"confidence":0.8}},
              "takeaway":{{"t_ns":1100000000,"frame":132,"confidence":0.8}},
              "top":{{"t_ns":1850000000,"frame":222,"confidence":0.9}},
              "impact":{{"t_ns":2150000000,"frame":258,"confidence":0.9}},
              "impact_source":"refined",
              "finish":{{"t_ns":2800000000,"frame":336,"confidence":0.7}}}},
            "tempo":{{"backswing_ns":750000000,"downswing_ns":300000000,"ratio":2.5}},
            "metrics":[
              {{"id":"hip_sway","view":"face_on","unit":"torso","values":{{"top":-0.05,"impact":0.12}}{},"confidence":0.8}},
              {{"id":"lateral_spine_tilt","view":"face_on","unit":"degrees","values":{{"address":8.4,"top":-3.6,"impact":21.2}},"confidence":0.8}},
              {{"id":"stance_width","view":"face_on","unit":"ratio","values":{{"address":1.234}},"confidence":0.8}},
              {{"id":"shoulder_tilt","view":"face_on","unit":"degrees","values":{{"finish":30.0}},"confidence":0.8}}
            ]}}"#,
            cm_part(-2.54, 6.096)
        );
        serde_json::from_str(&json).expect("report")
    }

    /// A face-on report whose given impact found no swing: the impact alone
    /// and the metrics at it, as stance writes it.
    fn no_swing_report() -> SwingReportFile {
        serde_json::from_str(
            r#"{"version":1,"view":"face_on","handed":"right",
            "events":{"impact":{"t_ns":2150000000,"frame":258,"confidence":0.5},
              "impact_source":"given"},
            "metrics":[{"id":"lateral_spine_tilt","view":"face_on","unit":"degrees",
              "values":{"impact":21.2},"confidence":0.8}]}"#,
        )
        .expect("report")
    }

    #[test]
    fn the_swing_numbers_read_from_a_report_and_follow_the_units() {
        let r = report(false);
        assert_eq!(
            tempo_tiles(&[&r]),
            [
                ("Tempo", "2.5 : 1".to_owned()),
                ("Backswing", "750 ms".to_owned()),
                ("Downswing", "300 ms".to_owned()),
            ]
        );
        // Body-relative without the golfer's height; a finish-only metric
        // has no row.
        let rows = strings(&rows3(&r, true));
        assert_eq!(
            rows,
            [
                (
                    "Hip sway".to_owned(),
                    [
                        NONE.to_owned(),
                        "5% torso Away".to_owned(),
                        "12% torso Toward".to_owned()
                    ]
                ),
                (
                    "Side bend".to_owned(),
                    [
                        "8\u{b0}".to_owned(),
                        "-4\u{b0}".to_owned(),
                        "21\u{b0}".to_owned()
                    ]
                ),
                (
                    "Stance width".to_owned(),
                    ["1.23".to_owned(), NONE.to_owned(), NONE.to_owned()]
                ),
            ]
        );
        // With the height: inches in imperial, centimeters in metric.
        let r = report(true);
        assert_eq!(
            strings(&rows3(&r, true))[0].1,
            [NONE, "1.0 in Away", "2.4 in Toward"]
        );
        assert_eq!(
            strings(&rows3(&r, false))[0].1,
            [NONE, "2.5 cm Away", "6.1 cm Toward"]
        );
        assert_eq!(
            strings(&rows3(&r, false))[1].1[0],
            "8\u{b0}",
            "angles are units-free"
        );
        // No tempo, no impact: dashes.
        let mut bare = report(false);
        bare.tempo = None;
        bare.events.impact = None;
        assert!(tempo_tiles(&[&bare]).iter().all(|(_, v)| v == NONE));
        assert!(tempo_tiles(&[]).iter().all(|(_, v)| v == NONE));
    }

    #[test]
    fn every_metric_is_named_for_what_it_measures_and_an_unknown_one_reads_plainly() {
        for (id, name) in [
            ("hip_sway", "Hip sway"),
            ("head_lateral", "Head sway"),
            ("head_vertical", "Head height"),
            ("lateral_spine_tilt", "Side bend"),
            ("shoulder_tilt", "Shoulder tilt"),
            ("lead_arm_angle", "Lead elbow"),
            ("stance_width", "Stance width"),
            ("hip_turn", "Hip turn"),
            ("shoulder_turn", "Shoulder turn"),
            ("spine_forward_bend", "Forward bend"),
            ("early_extension", "Hip depth"),
            ("lead_knee_flex", "Lead knee flex"),
            ("trail_knee_flex", "Trail knee flex"),
            ("trail_elbow_angle", "Trail elbow"),
            ("hand_height", "Hand height"),
        ] {
            assert_eq!(metric_label(id), name, "{id}");
        }
        assert_eq!(metric_label("pelvis_drift"), "Pelvis drift");
        // The hip width ratio and the loss of posture stay in the file but
        // not in the table.
        let r: SwingReportFile = serde_json::from_str(
            r#"{"version":1,"view":"face_on","events":{"impact_source":"refined"},
            "metrics":[{"id":"hip_width_proxy","unit":"ratio","values":{"top":0.8}},
              {"id":"hip_turn","unit":"degrees","values":{"top":36.87,"impact":25.84}},
              {"id":"shoulder_turn","unit":"degrees","values":{"top":70.0,"impact":30.0}}]}"#,
        )
        .unwrap();
        let rows = strings(&rows3(&r, true));
        let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Hip turn", "Shoulder turn"]);
        assert_eq!(rows[0].1, [NONE, "37°", "26°"]);
        let r: SwingReportFile = serde_json::from_str(
            r#"{"version":1,"view":"down_the_line","events":{"impact_source":"refined"},
            "metrics":[{"id":"spine_forward_bend","unit":"degrees","values":{"address":31.0,"top":30.0,"impact":26.0}},
              {"id":"loss_of_posture","unit":"degrees","values":{"top":1.0,"impact":5.0}},
              {"id":"early_extension","unit":"torso","values":{"top":0.0,"impact":-0.04}}]}"#,
        )
        .unwrap();
        assert!(r.metrics.iter().any(|m| m.id == "loss_of_posture"), "read");
        let rows = strings(&rows3(&r, true));
        let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["Forward bend", "Hip depth"],
            "no loss of posture row"
        );
        assert_eq!(rows[1].1, [NONE, "0% torso", "4% torso Back"]);
    }

    /// The table rows at address, top and impact, ungraded.
    fn rows3(r: &SwingReportFile, imperial: bool) -> Vec<(String, Vec<Cell>)> {
        use SwingPosition::{Address, Impact, Top};
        metric_rows(r, imperial, &[Address, Top, Impact], None)
    }

    /// Three-column table rows as text, each value with its direction word
    /// after it.
    fn strings(rows: &[(String, Vec<Cell>)]) -> Vec<(String, [String; 3])> {
        rows.iter()
            .map(|(name, cells)| {
                let texts: Vec<String> = cells
                    .iter()
                    .map(|c| joined(&c.shown.text, c.shown.word))
                    .collect();
                (name.clone(), texts.try_into().expect("three columns"))
            })
            .collect()
    }

    /// Each report at the frame it shows, ungraded.
    fn plain<'a>(at: &[(&'a SwingReportFile, Option<u64>)]) -> Vec<Swing<'a>> {
        at.iter()
            .map(|&(report, t_ns)| Swing {
                report,
                t_ns,
                grades: None,
            })
            .collect()
    }

    /// A value and its direction word as one line of text.
    fn joined(text: &str, word: Option<&str>) -> String {
        word.map_or_else(|| text.to_owned(), |w| format!("{text} {w}"))
    }

    #[test]
    fn shifts_from_address_read_as_a_distance_and_a_direction() {
        // Hip sway and head sway: the distance and a word, never a sign.
        let shown = |id, v, cm, imperial| {
            let s = metric_shown(id, "torso", v, cm, imperial);
            joined(&s.text, s.word)
        };
        assert_eq!(shown("hip_sway", 0.12, Some(9.652), true), "3.8 in Toward");
        assert_eq!(shown("hip_sway", -0.04, Some(-2.794), true), "1.1 in Away");
        assert_eq!(shown("hip_sway", -0.04, Some(-2.794), false), "2.8 cm Away");
        assert_eq!(shown("head_lateral", -0.16, None, true), "16% torso Away");
        assert_eq!(shown("head_lateral", 0.03, None, false), "3% torso Toward");
        // A distance that shows as 0 has no word.
        assert_eq!(shown("hip_sway", 0.0, Some(0.0), true), "0.0 in");
        assert_eq!(shown("hip_sway", -0.001, Some(-0.05), true), "0.0 in");
        assert_eq!(shown("head_lateral", 0.004, None, true), "0% torso");
        // Hip depth: toward the ball or back from it.
        assert_eq!(
            shown("early_extension", -0.02, Some(-2.032), true),
            "0.8 in Back"
        );
        assert_eq!(
            shown("early_extension", 0.03, Some(3.048), true),
            "1.2 in Toward ball"
        );
        assert_eq!(
            shown("early_extension", 0.03, Some(3.048), false),
            "3.0 cm Toward ball"
        );
        assert_eq!(shown("early_extension", -0.05, None, true), "5% torso Back");
        assert_eq!(shown("early_extension", 0.0, Some(0.0), true), "0.0 in");
        assert_eq!(shown("early_extension", -0.002, Some(-0.1), true), "0.0 in");
        // Head height: up (a rise) or down.
        assert_eq!(shown("head_vertical", 0.01, Some(0.762), true), "0.3 in Up");
        assert_eq!(
            shown("head_vertical", -0.04, Some(-5.334), true),
            "2.1 in Down"
        );
        assert_eq!(shown("head_vertical", 0.001, None, true), "0% torso");
        // Other metrics keep their sign.
        assert_eq!(
            metric_shown("lateral_spine_tilt", "degrees", -3.6, None, true),
            Shown {
                text: "-4\u{b0}".to_owned(),
                word: None,
                color: theme::TEXT,
            }
        );

        // In the table: Top and Impact of the face-on rows.
        let r: SwingReportFile = serde_json::from_str(
            r#"{"version":1,"view":"face_on","events":{"impact_source":"refined"},
            "metrics":[
              {"id":"hip_sway","unit":"torso","values":{"top":-0.0236,"impact":0.0828},
                "cm":{"top":-2.95,"impact":10.35}},
              {"id":"head_lateral","unit":"torso","values":{"top":0.0,"impact":-0.0544},
                "cm":{"top":0.0,"impact":-6.8}}]}"#,
        )
        .unwrap();
        assert_eq!(
            strings(&rows3(&r, true)),
            [
                (
                    "Hip sway".to_owned(),
                    [
                        NONE.to_owned(),
                        "1.2 in Away".to_owned(),
                        "4.1 in Toward".to_owned()
                    ]
                ),
                (
                    "Head sway".to_owned(),
                    [
                        NONE.to_owned(),
                        "0.0 in".to_owned(),
                        "2.7 in Away".to_owned()
                    ]
                ),
            ]
        );
    }

    #[test]
    fn an_earlier_swing_file_with_sway_and_slide_reads_without_them() {
        let r: SwingReportFile = serde_json::from_str(
            r#"{"version":1,"view":"face_on","events":{"impact_source":"refined"},
            "metrics":[
              {"id":"sway","view":"face_on","unit":"torso","values":{"top":0.05},"confidence":0.8},
              {"id":"slide","view":"face_on","unit":"torso","values":{"impact":0.15},"confidence":0.8},
              {"id":"hip_turn","view":"face_on","unit":"degrees","values":{"top":40.0},"confidence":0.8}],
            "series":{"t_ns":[0,16666667],"metrics":[
              {"id":"sway","unit":"torso","values":[0.0,0.01]},
              {"id":"hip_turn","unit":"degrees","values":[0.0,1.0]}]}}"#,
        )
        .unwrap();
        let rows = strings(&rows3(&r, true));
        let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Hip turn"], "no Sway or Slide row");
        let texts = frame_texts(&frame_tiles(&plain(&[(&r, Some(16_666_667))]), true));
        assert_eq!(texts[0], ("Hip turn", "1°".to_owned()));
        assert_eq!(texts[2], ("Hip sway", NONE.to_owned()));
    }

    /// Frames of the test clips ([`give_sidecar`]): 300 at 60 fps.
    const FRAME_NS: u64 = 16_666_667;

    /// A metric of a test series: its id, unit and value at frame `i`.
    type SeriesSpec<'a> = (&'a str, &'a str, fn(u64) -> f64);

    /// A swing report of `view` with a series over [`give_sidecar`]'s 300
    /// frames: each metric `(id, unit, value at frame i)`, the body scale of
    /// a 180 cm golfer (torso 200 px, 0.25 cm/px) when `cm`.
    fn series_report(view: &str, metrics: &[SeriesSpec], cm: bool) -> SwingReportFile {
        let t_ns: Vec<u64> = (0..300).map(|i| i * FRAME_NS).collect();
        let series: Vec<serde_json::Value> = metrics
            .iter()
            .map(|(id, unit, f)| {
                let values: Vec<f64> = (0..300).map(f).collect();
                serde_json::json!({ "id": id, "unit": unit, "values": values })
            })
            .collect();
        let mut json = serde_json::json!({
            "version": 1, "view": view, "handed": "right",
            "events": {
                "address": {"t_ns": 60 * FRAME_NS, "frame": 60, "confidence": 0.8},
                "takeaway": {"t_ns": 61 * FRAME_NS, "frame": 61, "confidence": 0.8},
                "top": {"t_ns": 150 * FRAME_NS, "frame": 150, "confidence": 0.9},
                "impact": {"t_ns": 180 * FRAME_NS, "frame": 180, "confidence": 0.9},
                "impact_source": "refined"
            },
            "tempo": {"backswing_ns": 89 * FRAME_NS, "downswing_ns": 30 * FRAME_NS, "ratio": 2.97},
            "metrics": [],
            "series": { "t_ns": t_ns, "metrics": series },
        });
        json["scale"] = if cm {
            serde_json::json!({"torso_px": 200.0, "cm_per_px": 0.25})
        } else {
            serde_json::json!({"torso_px": 200.0})
        };
        serde_json::from_value(json).expect("report")
    }

    /// The test hip sway at frame `i`: from address (frame 60) away from
    /// the target to the top (frame 150, -0.036), then toward it (impact,
    /// frame 180: 0.054).
    fn hip_sway_at(i: u64) -> f64 {
        let i = i as f64;
        if i <= 150.0 {
            -(i - 60.0) * 0.0004
        } else {
            -0.036 + (i - 150.0) * 0.003
        }
    }

    fn face_on_series(cm: bool) -> SwingReportFile {
        series_report(
            "face_on",
            &[
                ("hip_sway", "torso", hip_sway_at),
                ("head_lateral", "torso", |i| -(i as f64) * 0.0005),
                ("lateral_spine_tilt", "degrees", |i| 5.2 + i as f64 * 0.01),
                ("hip_turn", "degrees", |i| i as f64 * 0.1),
                ("shoulder_turn", "degrees", |i| i as f64 * 0.2),
                ("some_new_metric", "degrees", |_| 1.0),
            ],
            cm,
        )
    }

    /// The test hip depth at frame `i`: 0 at address (frame 60), back from
    /// the ball before it, toward the ball after it (frame 150: 0.036).
    fn hip_depth_at(i: u64) -> f64 {
        (i as f64 - 60.0) * 0.0004
    }

    fn down_the_line_series() -> SwingReportFile {
        series_report(
            "down_the_line",
            &[
                ("head_vertical", "torso", |i| i as f64 * 0.0001),
                ("spine_forward_bend", "degrees", |i| 30.0 + i as f64 * 0.01),
                ("early_extension", "torso", hip_depth_at),
            ],
            false,
        )
    }

    /// The tiles' names and values (each with its direction word).
    fn frame_texts(tiles: &[FrameValue]) -> Vec<(&'static str, String)> {
        tiles
            .iter()
            .map(|t| (t.label, joined(&t.text, t.word)))
            .collect()
    }

    #[test]
    fn the_frame_tiles_read_each_cameras_series_at_its_frame() {
        let (fo, dtl) = (face_on_series(true), down_the_line_series());
        let at = |i: u64| Some(i * FRAME_NS);
        // Two cameras at frame 150: four tiles face-on, two down the line.
        let tiles = frame_tiles(&plain(&[(&fo, at(150)), (&dtl, at(150))]), false);
        assert_eq!(
            frame_texts(&tiles),
            vec![
                ("Hip turn", "15°".to_owned()),
                ("Shoulder turn", "30°".to_owned()),
                ("Hip sway", "1.8 cm Away".to_owned()),
                ("Head sway", "3.8 cm Away".to_owned()),
                ("Forward bend", "32°".to_owned()),
                ("Hip depth", "4% torso Toward ball".to_owned()),
            ]
        );
        let ids: Vec<&str> = tiles.iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            [
                "hip_turn",
                "shoulder_turn",
                "hip_sway",
                "head_lateral",
                "spine_forward_bend",
                "early_extension"
            ]
        );
        // Imperial, and each camera at its own frame.
        let tiles = frame_tiles(&plain(&[(&fo, at(100)), (&dtl, at(260))]), true);
        let texts = frame_texts(&tiles);
        assert_eq!(texts[0].1, "10°");
        assert_eq!(texts[2].1, "0.3 in Away", "-0.016 × 50 cm = -0.8 cm");
        assert_eq!(texts[4].1, "33°");
        assert_eq!(texts[5].1, "8% torso Toward ball");
        // Between two frames: the nearest one.
        let tiles = frame_tiles(&plain(&[(&fo, Some(100 * FRAME_NS + 3))]), false);
        assert_eq!(tiles[0].text, "10°");

        // One face-on camera: the spine tile is its side bend, hip depth a
        // dash; one down-the-line camera: the face-on tiles are dashes.
        let texts = frame_texts(&frame_tiles(&plain(&[(&fo, at(150))]), false));
        assert_eq!(texts.len(), 6);
        assert_eq!(texts[4], ("Side bend", "7°".to_owned()));
        assert_eq!(texts[5], ("Hip depth", NONE.to_owned()));
        let texts = frame_texts(&frame_tiles(&plain(&[(&dtl, at(150))]), false));
        assert!(texts[..4].iter().all(|(_, v)| v == NONE), "{texts:?}");
        assert_eq!(texts[4], ("Forward bend", "32°".to_owned()));

        // Outside a camera's clip, or past its series: dashes for its tiles.
        let texts = frame_texts(&frame_tiles(&plain(&[(&fo, None), (&dtl, at(150))]), false));
        assert!(texts[..4].iter().all(|(_, v)| v == NONE));
        assert_eq!(texts[5].1, "4% torso Toward ball");
        let texts = frame_texts(&frame_tiles(&plain(&[(&fo, at(400))]), false));
        assert!(texts.iter().all(|(_, v)| v == NONE));

        // Two face-on cameras: the mean.
        let other = series_report("face_on", &[("hip_turn", "degrees", |_| 25.0)], false);
        let tiles = frame_tiles(&plain(&[(&fo, at(150)), (&other, at(150))]), false);
        assert_eq!(tiles[0].text, "20°");
        // Without the golfer's height, distances are shares of the torso.
        let tiles = frame_tiles(&plain(&[(&face_on_series(false), at(150))]), false);
        assert_eq!(joined(&tiles[2].text, tiles[2].word), "4% torso Away");

        // Each side of address: away before the top, toward the target at
        // impact, no word at address.
        let sway = |i: u64| {
            frame_texts(&frame_tiles(&plain(&[(&fo, at(i))]), false))[2]
                .1
                .clone()
        };
        assert_eq!(sway(150), "1.8 cm Away");
        assert_eq!(sway(180), "2.7 cm Toward");
        assert_eq!(sway(60), "0.0 cm");
        let imperial = frame_texts(&frame_tiles(&plain(&[(&fo, at(180))]), true));
        assert_eq!(imperial[2].1, "1.1 in Toward");

        // Hip depth each side of address: back before it, toward the ball
        // after it, no word at address.
        let depth = |i: u64| {
            frame_texts(&frame_tiles(&plain(&[(&dtl, at(i))]), false))[5]
                .1
                .clone()
        };
        assert_eq!(depth(20), "2% torso Back");
        assert_eq!(depth(60), "0% torso");
        assert_eq!(depth(180), "5% torso Toward ball");

        // A report without a series (an earlier file): dashes.
        let texts = frame_texts(&frame_tiles(&plain(&[(&report(true), at(150))]), false));
        assert!(texts.iter().all(|(_, v)| v == NONE));
        assert!(frame_tiles(&[], false).iter().all(|t| t.text == NONE));
    }

    /// Every direction word a value can have.
    const DIRECTION_WORDS: [&str; 6] = ["Toward", "Away", "Toward ball", "Back", "Up", "Down"];

    /// The selected-frame row's painted values, left to right, each with
    /// its direction word.
    fn row_values(screen: &Screen) -> Vec<String> {
        let texts = screen.texts();
        let start = texts
            .iter()
            .position(|t| t == "HIP TURN")
            .expect("the selected-frame row");
        let mut texts = texts[start..].iter();
        let mut values = Vec::new();
        while values.len() < 6 {
            texts.next().expect("a tile's name");
            let mut value = texts.next().expect("a tile's value").clone();
            if let Some(word) = texts
                .clone()
                .next()
                .filter(|w| DIRECTION_WORDS.contains(&w.as_str()))
            {
                value = format!("{value} {word}");
                texts.next();
            }
            values.push(value);
        }
        values
    }

    /// Put the inline player's playhead on frame `frame` of the test clips.
    fn seek_to_frame(app: &mut FlighthookApp, frame: u64) {
        let p = app.shot_player.as_mut().unwrap().playback.as_mut().unwrap();
        p.playing = false;
        p.playhead = 1_000_000_000 + frame * FRAME_NS;
    }

    #[test]
    fn the_frame_row_sits_under_the_tempo_and_follows_the_playhead() {
        let screen = Screen::new();
        let mut app = app(&[0, 1], true);
        app.settings.default_units = UnitSystem::Metric;
        app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_report(&mut app, "camera.0", face_on_series(true));
        give_report(&mut app, "camera.1", down_the_line_series());
        give_sidecar(&mut app, "camera.0");
        give_sidecar(&mut app, "camera.1");
        let marks = screen.frame(&mut app, Vec::new());
        let (tempo, row) = (marks["swing_tempo"].0, marks["frame_metrics"].0);
        assert!(
            row.top() > tempo.bottom() && row.top() - tempo.bottom() < 12.0,
            "under the tempo"
        );
        assert!(
            (row.left() - tempo.left()).abs() < 0.5 && (row.width() - tempo.width()).abs() < 0.5
        );
        assert!(
            (row.height() - tempo.height()).abs() < 0.5,
            "the same tiles"
        );
        for camera in ["camera.0", "camera.1"] {
            let table = marks[format!("swing:{camera}").as_str()].0;
            assert!(table.top() > row.bottom(), "the metrics under it");
        }
        let tiles: Vec<Rect> = [
            "hip_turn",
            "shoulder_turn",
            "hip_sway",
            "head_lateral",
            "spine_forward_bend",
            "early_extension",
        ]
        .iter()
        .map(|id| marks[format!("frame_metric:{id}").as_str()].0)
        .collect();
        assert!(
            tiles.windows(2).all(|w| w[0].right() < w[1].left()),
            "one row, in order"
        );
        assert!(tiles.iter().all(|t| row.contains_rect(*t)));
        // Opening: the row reads the frame the playhead opened on.
        let playhead = app
            .shot_player
            .as_ref()
            .unwrap()
            .playback
            .as_ref()
            .unwrap()
            .playhead;
        let frame = (playhead - 1_000_000_000).div_ceil(FRAME_NS);
        let opening = row_values(&screen);
        assert_eq!(opening[0], degrees(frame as f64 * 0.1), "{opening:?}");

        // Moved to frame 150, then 250: the row reads those frames.
        seek_to_frame(&mut app, 150);
        screen.frame(&mut app, Vec::new());
        assert_eq!(
            row_values(&screen),
            [
                "15°",
                "30°",
                "1.8 cm Away",
                "3.8 cm Away",
                "32°",
                "4% torso Toward ball"
            ]
        );
        let texts = screen.texts();
        for name in ["HEAD SWAY", "FORWARD BEND", "HIP DEPTH"] {
            assert!(texts.contains(&name.to_owned()), "{name}: {texts:?}");
        }
        // At impact the hips have moved toward the target and the ball; at
        // address no word; before it, back from the ball.
        seek_to_frame(&mut app, 180);
        screen.frame(&mut app, Vec::new());
        assert_eq!(row_values(&screen)[2], "2.7 cm Toward");
        assert_eq!(row_values(&screen)[5], "5% torso Toward ball");
        seek_to_frame(&mut app, 60);
        screen.frame(&mut app, Vec::new());
        assert_eq!(row_values(&screen)[2], "0.0 cm");
        assert_eq!(row_values(&screen)[5], "0% torso");
        seek_to_frame(&mut app, 20);
        screen.frame(&mut app, Vec::new());
        assert_eq!(row_values(&screen)[5], "2% torso Back");
        seek_to_frame(&mut app, 250);
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(row_values(&screen)[..2], ["25°", "50°"]);

        // A frame step moves it one frame.
        let step = marks["player:step:true"].0.center();
        let marks = screen.click(&mut app, step);
        assert_eq!(row_values(&screen)[1], "50°", "frame 251: 50.2°");
        assert_eq!(
            app.shot_player
                .as_ref()
                .unwrap()
                .playback
                .as_ref()
                .unwrap()
                .playhead,
            1_000_000_000 + 251 * FRAME_NS
        );
        // A click on the top's marker seeks there: frame 150.
        let top = marks["player:event:top"].0.center();
        screen.click(&mut app, top);
        assert_eq!(row_values(&screen)[0], "15°");

        // Playing moves it with the clip.
        seek_to_frame(&mut app, 120);
        app.shot_player
            .as_mut()
            .unwrap()
            .playback
            .as_mut()
            .unwrap()
            .playing = true;
        screen.frame(&mut app, Vec::new());
        assert_eq!(row_values(&screen)[0], "12°");
    }

    /// The width a name takes in a tile, and the sizes a value and its
    /// word are drawn at in `room`.
    fn tile_fit(
        ctx: &egui::Context,
        name: &str,
        value: &str,
        word: &str,
        room: f32,
    ) -> (f32, f32, f32, f32) {
        let mut fit = (0.0, 0.0, 0.0, 0.0);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let name = painter.layout_no_wrap(
                name.to_uppercase(),
                egui::FontId::proportional(10.0),
                theme::TEXT_MUTED,
            );
            let (v, w) = worded_value(&painter, value, Some(word), 18.0, room);
            let (w, offset) = w.expect("a word");
            let size = |g: &egui::Galley| g.job.sections[0].format.font_id.size;
            fit = (name.size().x, size(&v), size(&w), offset.x + w.size().x);
        });
        fit
    }

    /// At 1280 wide (the README's Cameras pane), the renamed tiles and
    /// their words fit at full size: the frame row's names, a long value
    /// with each word, and the live posture tiles' names.
    #[test]
    fn the_names_and_words_fit_their_tiles_at_1280_wide() {
        let screen = Screen::sized(1280.0, 948.0);
        let mut app = app(&[0, 1], true);
        app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_report(&mut app, "camera.0", face_on_series(true));
        give_report(&mut app, "camera.1", down_the_line_series());
        give_sidecar(&mut app, "camera.0");
        give_sidecar(&mut app, "camera.1");
        seek_to_frame(&mut app, 150);
        let marks = screen.frame(&mut app, Vec::new());
        let tile = marks["frame_metric:early_extension"].0.width();
        let room = tile - 16.0;
        for (name, value, word) in [
            ("Hip sway", "10.5 in", "Toward"),
            ("Head sway", "10.5 in", "Away"),
            ("Forward bend", "32°", "Up"),
            ("Hip depth", "10.5 in", "Toward ball"),
            ("Hip depth", "26.7 cm", "Back"),
            ("Hip depth", "25% torso", "Toward ball"),
        ] {
            let (name_w, value_size, word_size, line_w) =
                tile_fit(&screen.0, name, value, word, room);
            assert!(name_w <= room, "{name}: {name_w} in {room}");
            assert_eq!(
                value_size, 18.0,
                "{value} {word}: the numeral keeps its size"
            );
            assert_eq!(
                word_size,
                shots::small_size(18.0),
                "{value} {word}: the word keeps its size in {room}"
            );
            assert!(line_w <= room, "{value} {word}: {line_w} in {room}");
        }
        // A distance as long as the torso: the word shrinks, still on the
        // line.
        let (_, value_size, word_size, line_w) =
            tile_fit(&screen.0, "Hip depth", "100% torso", "Toward ball", room);
        assert_eq!(value_size, 18.0);
        assert!(word_size >= shots::WORD_MIN && line_w <= room);
        // Live: the down-the-line camera's four posture tiles.
        app.select_live();
        let marks = screen.frame(&mut app, Vec::new());
        let row = marks["posture:camera.1"].0;
        let room = tile_rects(row, 4)[0].width() - 16.0;
        for name in [
            "Forward bend",
            "Lead knee flex",
            "Trail knee flex",
            "Lead elbow",
            "Side bend",
        ] {
            let (name_w, ..) = tile_fit(&screen.0, name, "0°", "Up", room);
            assert!(name_w <= room, "{name}: {name_w} in {room}");
        }
    }

    #[test]
    fn one_camera_keeps_the_row_with_dashes_for_the_other_view() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.shots = vec![shot_row(1, &["camera.0"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_report(&mut app, "camera.0", face_on_series(false));
        give_sidecar(&mut app, "camera.0");
        seek_to_frame(&mut app, 150);
        let marks = screen.frame(&mut app, Vec::new());
        let (tempo, row) = (marks["swing_tempo"].0, marks["frame_metrics"].0);
        assert!(
            (row.left() - tempo.left()).abs() < 0.5 && (row.width() - tempo.width()).abs() < 0.5
        );
        assert!(marks.contains_key("frame_metric:lateral_spine_tilt"));
        assert!(marks.contains_key("frame_metric:early_extension"));
        assert_eq!(
            row_values(&screen),
            ["15°", "30°", "4% torso Away", "8% torso Away", "7°", NONE]
        );
        let texts = screen.texts();
        assert!(texts.contains(&"SIDE BEND".to_owned()));
        assert!(!texts.contains(&"FORWARD BEND".to_owned()));

        // An earlier report without a series: the row of dashes.
        let mut app2 = self::app(&[0], true);
        app2.shots = vec![shot_row(1, &["camera.0"])];
        app2.shots_rev += 1;
        app2.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app2, Vec::new());
        give_report(&mut app2, "camera.0", report(false));
        give_sidecar(&mut app2, "camera.0");
        let marks = screen.frame(&mut app2, Vec::new());
        assert!(marks.contains_key("frame_metrics"));
        assert!(row_values(&screen).iter().all(|v| v == NONE));

        // A report that found no swing: no tempo, no frame row.
        let mut app3 = self::app(&[0], true);
        app3.shots = vec![shot_row(1, &["camera.0"])];
        app3.shots_rev += 1;
        app3.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app3, Vec::new());
        give_report(&mut app3, "camera.0", no_swing_report());
        let marks = screen.frame(&mut app3, Vec::new());
        assert!(!marks.contains_key("swing_tempo") && !marks.contains_key("frame_metrics"));
    }

    #[test]
    fn the_live_posture_shows_the_cameras_view() {
        let p = LivePosture {
            view: SwingView::FaceOn,
            spine_angle: Some(12.4),
            shoulder_tilt: Some(-31.0),
            lead_arm_angle: Some(171.6),
            lead_knee_flex: None,
            trail_knee_flex: None,
            trail_elbow_angle: None,
        };
        assert_eq!(
            posture_tiles(CameraPosition::GolferFront, Some(&p)),
            [
                ("Side bend", "12\u{b0}".to_owned()),
                ("Shoulder tilt", "-31\u{b0}".to_owned()),
                ("Lead elbow", "172\u{b0}".to_owned()),
            ]
        );
        let names: Vec<&str> = posture_tiles(CameraPosition::TargetLine, None)
            .into_iter()
            .map(|(n, v)| {
                assert_eq!(v, NONE);
                n
            })
            .collect();
        assert_eq!(
            names,
            [
                "Forward bend",
                "Lead knee flex",
                "Trail knee flex",
                "Trail elbow"
            ]
        );
        assert!(posture_tiles(CameraPosition::GolferBack, Some(&p)).is_empty());
        assert!(posture_tiles(CameraPosition::Overhead, None).is_empty());
    }

    #[test]
    fn the_posture_that_came_with_the_frame_shows_under_its_picture() {
        let screen = Screen::new();
        let mut app = app(&[0, 1], true);
        app.select_live();
        screen.frame(&mut app, Vec::new());
        let id = app.live["camera.0"].id;
        let mut posed = pose(5.0);
        posed.posture = Some(LivePosture {
            view: SwingView::FaceOn,
            spine_angle: Some(10.0),
            shoulder_tilt: Some(20.0),
            lead_arm_angle: Some(170.0),
            lead_knee_flex: None,
            trail_knee_flex: None,
            trail_elbow_angle: None,
        });
        app.live_replies(
            vec![(
                id,
                LiveReply::Frame {
                    seq: 1,
                    jpeg: frame_jpeg(),
                    pose: Some(posed.clone()),
                    state: PoseState::default(),
                },
            )],
            0.0,
        );
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(app.live["camera.0"].shown_pose, Some(posed));
        let picture = marks["camera_live:camera.0"].0;
        let tiles = marks["posture:camera.0"].0;
        assert!(tiles.top() >= picture.bottom());
        assert!((tiles.width() - picture.width()).abs() < 0.5);
        // The down-the-line camera has its tiles too, dashes until a posture
        // arrives.
        assert!(marks.contains_key("posture:camera.1"));
    }

    /// `app(cameras, true)` with `edit` applied to its saved config.
    fn app_with(cameras: &[u32], edit: impl FnOnce(&mut FlighthookConfig)) -> FlighthookApp {
        let mut app = app(cameras, true);
        let mut saved = config(cameras);
        edit(&mut saved);
        app.settings.load_from(&saved);
        app
    }

    #[test]
    fn a_portrait_live_view_is_drawn_upright_with_its_skeleton_on_it() {
        for (rotation, portrait) in [(0, false), (90, true), (180, false), (270, true)] {
            let screen = Screen::new();
            let mut app = app_with(&[0], |c| {
                c.camera.get_mut("0").unwrap().rotation_deg = rotation
            });
            assert_eq!(app.camera_rotation("camera.0"), rotation);
            app.select_live();
            screen.frame(&mut app, Vec::new());
            let id = app.live["camera.0"].id;
            // A 64×48 frame, the skeleton found on the upright 48×64 one
            // when turned.
            app.live_replies(
                vec![(
                    id,
                    LiveReply::Frame {
                        seq: 1,
                        jpeg: frame_jpeg(),
                        pose: Some(pose(40.0)),
                        state: PoseState {
                            state: "ready".into(),
                            error: None,
                        },
                    },
                )],
                1.0,
            );
            screen.frame(&mut app, Vec::new());
            let marks = screen.frame(&mut app, Vec::new());
            let picture = marks["camera_live:camera.0"].0;
            let aspect = picture.height() / picture.width();
            let want = if portrait { 64.0 / 48.0 } else { 48.0 / 64.0 };
            assert!((aspect - want).abs() < 0.02, "{rotation}: {aspect}");
            assert!(
                (app.camera_aspect("camera.0") - want).abs() < 0.02,
                "{rotation}: the column is laid out for the turned picture"
            );
            assert!(screen.circles_in(picture) > 0, "{rotation}: the skeleton");
        }
    }

    #[test]
    fn a_clip_tracked_at_another_rotation_is_tracked_again_once() {
        let screen = Screen::new();
        let mut app = app_with(&[0, 1], |c| {
            c.camera.get_mut("0").unwrap().rotation_deg = 90
        });
        app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        // camera.0 was tracked unturned and is turned now; camera.1 matches.
        assert_eq!(
            app.retracked,
            std::collections::HashSet::from([("id1".to_owned(), "camera.0".to_owned())])
        );
        let video = &app.shots[0].video;
        assert!(!video[0].pose && !video[0].swing, "dropped meanwhile");
        assert!(video[1].pose && video[1].swing);
        let player = app.shot_player.as_ref().expect("the inline player");
        assert!(!player.wants_swing_of("camera.0"));
        assert!(player.wants_swing_of("camera.1"));
        // An old pass finishing at the old rotation is not asked about
        // again this session.
        assert!(crate::types::mark_pose(
            &mut app.shots,
            &[ShotKey {
                shot_id: "id1".into(),
                shot_number: 1,
            }],
            "camera.0",
            0,
        ));
        screen.frame(&mut app, Vec::new());
        assert_eq!(app.retracked.len(), 1);
        assert!(app.shots[0].video[0].pose, "kept");
        // Tracked at the camera's rotation: nothing to do.
        let mut fresh = app_with(&[0, 1], |c| {
            c.camera.get_mut("0").unwrap().rotation_deg = 90
        });
        let mut row = shot_row(2, &["camera.0", "camera.1"]);
        row.video[0].rotation = 90;
        fresh.shots = vec![row];
        fresh.shots_rev += 1;
        fresh.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut fresh, Vec::new());
        assert!(fresh.retracked.is_empty());
        // Body tracking off: nothing either.
        let mut off = app_with(&[0], |c| {
            c.camera.get_mut("0").unwrap().rotation_deg = 90;
            c.body_tracking.mode = BodyTrackingMode::Off;
        });
        off.shots = vec![shot_row(3, &["camera.0"])];
        off.shots_rev += 1;
        off.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut off, Vec::new());
        assert!(off.retracked.is_empty());
    }

    #[test]
    fn only_tracked_clips_of_configured_cameras_at_another_rotation_are_due() {
        let mut row = shot_row(1, &["camera.0", "camera.1", "camera.2"]);
        row.video[1].pose = false;
        let rotation_of = |camera: &str| match camera {
            "camera.0" | "camera.1" => Some(270),
            _ => None,
        };
        let none = std::collections::HashSet::new();
        // camera.1 has no keypoints yet; camera.2 is not configured.
        assert_eq!(retrack_due(&row, rotation_of, &none), ["camera.0"]);
        let asked = std::collections::HashSet::from([("id1".to_owned(), "camera.0".to_owned())]);
        assert!(retrack_due(&row, rotation_of, &asked).is_empty());
        row.video[0].rotation = 270;
        assert!(retrack_due(&row, rotation_of, &none).is_empty());
    }

    #[test]
    fn a_golfer_front_camera_five_degrees_off_square_is_flagged() {
        let line = "Camera 7° off square: one-camera turn and sway are approximate";
        let front = CameraPosition::GolferFront;
        assert_eq!(yaw_warning(front, Some(7.2)).as_deref(), Some(line));
        assert_eq!(yaw_warning(front, Some(-6.6)).as_deref(), Some(line));
        assert!(yaw_warning(front, Some(5.0)).is_some());
        assert!(yaw_warning(front, Some(-5.0)).is_some());
        assert_eq!(yaw_warning(front, Some(4.9)), None);
        assert_eq!(yaw_warning(front, None), None);
        assert_eq!(
            yaw_warning(front, Some(40.0)),
            None,
            "out of range: not in effect"
        );
        for other in [
            CameraPosition::TargetLine,
            CameraPosition::GolferBack,
            CameraPosition::Overhead,
        ] {
            assert_eq!(yaw_warning(other, Some(20.0)), None);
        }
    }

    #[test]
    fn the_yaw_line_sits_over_the_golfer_front_cameras_analysis_only() {
        let screen = Screen::new();
        let mut app = app_with(&[0, 1], |c| {
            c.camera.get_mut("0").unwrap().yaw_deg = Some(-8.0);
            c.camera.get_mut("1").unwrap().yaw_deg = Some(12.0);
        });
        app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_report(&mut app, "camera.0", report(false));
        give_report(&mut app, "camera.1", report(false));
        let marks = screen.frame(&mut app, Vec::new());
        let line = marks["yaw_warning:camera.0"].0;
        let swing = marks["swing:camera.0"].0;
        assert!(line.bottom() <= swing.top() + 0.5, "above its table");
        assert!(
            line.right() <= marks["swing:camera.1"].0.left(),
            "in its column"
        );
        assert!(
            screen
                .texts()
                .iter()
                .any(|t| t == "Camera 8° off square: one-camera turn and sway are approximate")
        );
        assert!(!marks.contains_key("yaw_warning:camera.1"), "target line");
        // Square enough: no line.
        let mut square = app_with(&[0], |c| c.camera.get_mut("0").unwrap().yaw_deg = Some(3.0));
        square.shots = vec![shot_row(2, &["camera.0"])];
        square.shots_rev += 1;
        square.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut square, Vec::new());
        give_report(&mut square, "camera.0", report(false));
        let marks = screen.frame(&mut square, Vec::new());
        assert!(marks.contains_key("swing:camera.0"));
        assert!(!marks.contains_key("yaw_warning:camera.0"));
    }

    fn shot_row(n: u32, cameras: &[&str]) -> ShotRow {
        ShotRow {
            actor: "mock_monitor.0".into(),
            shot_id: format!("id{n}"),
            shot_number: n,
            launch: Some(flighthook::Launch::default()),
            flight: None,
            club: None,
            impact: None,
            derived: None,
            club_selected: None,
            handed: None,
            group: None,
            finished: true,
            video: cameras
                .iter()
                .map(|c| ShotVideo {
                    camera: (*c).to_owned(),
                    position: if *c == "camera.0" {
                        CameraPosition::GolferFront
                    } else {
                        CameraPosition::TargetLine
                    },
                    duration_ms: 5000,
                    fps: 60.0,
                    trigger_offset_ms: 3000,
                    impact_offset_ms: 3000,
                    complete: true,
                    dropped: 0,
                    pose: true,
                    swing: true,
                    rotation: 0,
                    analyzed_with: None,
                })
                .collect(),
        }
    }

    /// Hand camera `camera`'s swing report to the inline player.
    fn give_report(app: &mut FlighthookApp, camera: &str, r: SwingReportFile) {
        let player = app.shot_player.as_mut().expect("a player");
        let seq = player.seq();
        player.reply(crate::net::ReplayReply::Swing {
            replay: seq,
            camera: camera.to_owned(),
            result: Ok(r),
        });
    }

    #[test]
    fn a_shot_in_cameras_mode_plays_its_clips_with_the_swing_data_under_them() {
        let screen = Screen::new();
        let mut app = app(&[0, 1], true);
        app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        let marks = screen.frame(&mut app, Vec::new());
        let player = app.shot_player.as_ref().expect("the inline player");
        assert_eq!(player.shot_id(), "id1");
        assert!(app.card_clips.is_empty(), "no card row clips");
        let left = marks["player:picture:camera.0"].0;
        let right = marks["player:picture:camera.1"].0;
        assert!(left.right() < right.left(), "side by side");
        assert!(!marks.contains_key("swing:camera.0"), "no report yet");
        give_report(&mut app, "camera.0", report(false));
        let marks = screen.frame(&mut app, Vec::new());
        let swing = marks["swing:camera.0"].0;
        assert!(swing.top() > left.bottom(), "under its clip");
        assert!((swing.left() - left.left()).abs() < 1.0, "in its column");
        assert!(swing.right() <= left.right() + 1.0);
        assert!(marks.contains_key("swing_tempo"));
        assert!(!marks.contains_key("swing:camera.1"));
        // A report that found no swing near the impact: the one line under
        // its clip, no tempo, events or metrics.
        give_report(&mut app, "camera.1", no_swing_report());
        let marks = screen.frame(&mut app, Vec::new());
        let line = marks["swing:camera.1"].0;
        assert!(line.top() > right.bottom(), "under its clip");
        assert!((line.left() - right.left()).abs() < 1.0, "in its column");
        assert!((line.height() - TABLE_ROW_H).abs() < 0.5, "one line");
        assert!(marks["swing:camera.0"].0.height() > line.height() * 3.0);
        assert!(!no_swing_report().swing_found() && report(false).swing_found());
        assert_eq!(NO_SWING, "No swing found in this clip");
        // The Data pane drops the player; the shot's cards come back.
        app.set_shots_pane(ShotsPane::Data);
        screen.frame(&mut app, Vec::new());
        assert!(app.shot_player.is_none());
    }

    #[test]
    fn picking_a_shot_shows_its_clips_and_the_replay_still_opens() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.shots = vec![shot_row(1, &["camera.0"]), shot_row(2, &["camera.0"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        assert_eq!(
            app.shot_player.as_ref().unwrap().shot_id(),
            "id2",
            "the latest"
        );
        app.select_shot(0);
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(app.shot_player.as_ref().unwrap().shot_id(), "id1");
        // The header's Replay opens the modal over the pane, which holds.
        let at = marks["replay"].0.center();
        screen.click(&mut app, at);
        assert_eq!(app.replay.as_ref().map(|r| r.shot_id()), Some("id1"));
        assert!(app.shot_player.is_some());
    }

    #[test]
    fn a_shot_without_clips_shows_each_cameras_status() {
        let screen = Screen::new();
        let mut app = app(&[0, 1], true);
        app.shots = vec![shot_row(1, &[]), shot_row(2, &[])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        app.actors
            .get_mut("camera.1")
            .unwrap()
            .observe(ActorStatus::Reconnecting, 0);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(app.shot_player.is_none());
        assert!(marks.contains_key("clip_status:camera.0"));
        assert!(marks.contains_key("clip_status:camera.1"));
        assert_eq!(clip_status_text(true), "Waiting for the clip");
        assert_eq!(clip_status_text(false), "No clip of this shot");
    }

    /// Put body tracking in `mode`, as saved settings.
    fn set_mode(app: &mut FlighthookApp, mode: BodyTrackingMode) {
        let mut saved = app.settings.to_request();
        saved.body_tracking.mode = mode;
        app.settings.load_from(&saved);
    }

    #[test]
    fn live_shows_the_posture_and_readout_in_live_mode_and_the_pictures_alone_otherwise() {
        let screen = Screen::new();
        let mut app = app(&[0, 1], true);
        app.select_live();
        screen.frame(&mut app, Vec::new());
        for mode in BodyTrackingMode::all() {
            set_mode(&mut app, mode);
            // A frame with the models still loading, as the server says in
            // Live mode.
            let id = app.live["camera.0"].id;
            app.live_replies(
                vec![(
                    id,
                    LiveReply::Frame {
                        seq: 1,
                        jpeg: frame_jpeg(),
                        pose: None,
                        state: PoseState {
                            state: "loading".into(),
                            error: None,
                        },
                    },
                )],
                0.0,
            );
            let marks = screen.frame(&mut app, Vec::new());
            let live = mode == BodyTrackingMode::Live;
            assert!(marks.contains_key("camera_live:camera.0"), "{mode:?}");
            assert!(marks.contains_key("camera_live:camera.1"), "{mode:?}");
            assert_eq!(marks.contains_key("posture:camera.0"), live, "{mode:?}");
            assert_eq!(marks.contains_key("posture:camera.1"), live, "{mode:?}");
            assert_eq!(marks.contains_key("pose_readout"), live, "{mode:?}");
            assert_eq!(marks.contains_key("pose_status:camera.0"), live, "{mode:?}");
        }
    }

    /// Show or hide the skeleton, as saved settings.
    fn set_show_skeleton(app: &mut FlighthookApp, show: bool) {
        let mut saved = app.settings.to_request();
        saved.body_tracking.show_skeleton = show;
        app.settings.load_from(&saved);
    }

    #[test]
    fn the_old_body_tracking_keys_change_nothing_on_the_cameras_pane() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.shots = vec![shot_row(1, &["camera.0"])];
        app.shots_rev += 1;
        set_mode(&mut app, BodyTrackingMode::Live);
        let saved = app.settings.saved_body_tracking();
        let keys = [
            Key::B,
            Key::M,
            Key::OpenBracket,
            Key::CloseBracket,
            Key::Comma,
            Key::Period,
            Key::D,
        ];
        // Live, then a shot's clips.
        for live in [true, false] {
            if live {
                app.select_live();
            } else {
                app.select_shot(0);
                app.set_shots_pane(ShotsPane::Cameras);
            }
            screen.frame(&mut app, Vec::new());
            assert!(app.cameras_pane_shown());
            assert_eq!(app.live_shown(), live);
            for key in keys {
                screen.frame(&mut app, press(key));
                screen.frame(&mut app, Vec::new());
            }
            assert_eq!(app.body_tracking(), saved, "live {live}");
            assert!(app.tuning.sent.is_empty(), "live {live}");
            assert!(!app.settings.global_dirty, "live {live}");
            assert_eq!(app.settings.body_tracking.to_section(), Some(saved.clone()));
            assert!(app.pose_draw(Handedness::Right).overlay, "live {live}");
        }
    }

    #[test]
    fn show_skeleton_off_hides_the_skeleton_on_a_live_view() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        set_mode(&mut app, BodyTrackingMode::Live);
        app.select_live();
        screen.frame(&mut app, Vec::new());
        let id = app.live["camera.0"].id;
        app.live_replies(
            vec![(
                id,
                LiveReply::Frame {
                    seq: 1,
                    jpeg: frame_jpeg(),
                    pose: Some(pose(5.0)),
                    state: PoseState {
                        state: "ready".into(),
                        error: None,
                    },
                },
            )],
            1.0,
        );
        let marks = screen.frame(&mut app, Vec::new());
        assert!(
            screen.circles_in(marks["camera_live:camera.0"].0) > 0,
            "drawn by default"
        );
        assert!(!screen.texts().iter().any(|t| t.contains("skeleton off")));
        set_show_skeleton(&mut app, false);
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(app.live["camera.0"].shown_pose, Some(pose(5.0)));
        assert_eq!(screen.circles_in(marks["camera_live:camera.0"].0), 0);
        assert!(screen.texts().iter().any(|t| t.contains("skeleton off")));
        set_show_skeleton(&mut app, true);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(screen.circles_in(marks["camera_live:camera.0"].0) > 0);
    }

    #[test]
    fn show_skeleton_off_hides_the_skeleton_in_the_player_and_the_replay() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.shots = vec![shot_row(1, &["camera.0"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        for mode in [BodyTrackingMode::Historical, BodyTrackingMode::Live] {
            set_mode(&mut app, mode);
            screen.frame(&mut app, Vec::new());
            assert!(app.pose_draw(Handedness::Right).overlay, "{mode:?}");
            set_show_skeleton(&mut app, false);
            let marks = screen.frame(&mut app, Vec::new());
            assert!(marks.contains_key("player:picture:camera.0"), "{mode:?}");
            assert!(!app.pose_draw(Handedness::Right).overlay, "{mode:?}");
            set_show_skeleton(&mut app, true);
        }
    }

    #[test]
    fn off_shows_no_skeleton_or_swing_data_and_asks_for_no_tracking() {
        let screen = Screen::new();
        let mut app = app(&[0, 1], true);
        set_mode(&mut app, BodyTrackingMode::Off);
        let mut row = shot_row(1, &["camera.0", "camera.1"]);
        row.video[1].pose = false;
        row.video[1].swing = false;
        app.shots = vec![row];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        // Files on disk from before: not shown; an untracked clip is not
        // asked for.
        let player = app.shot_player.as_ref().expect("the inline player");
        assert!(!player.asked_to_track("camera.1"));
        give_report(&mut app, "camera.0", report(false));
        let marks = screen.frame(&mut app, Vec::new());
        assert!(
            marks.contains_key("player:picture:camera.0"),
            "the clip plays"
        );
        assert!(!marks.contains_key("swing:camera.0"));
        assert!(!app.pose_draw(Handedness::Right).overlay);
        // Historical: the same report shows, the skeleton is drawn.
        set_mode(&mut app, BodyTrackingMode::Historical);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("swing:camera.0"));
        assert!(app.pose_draw(Handedness::Right).overlay);
        let player = app.shot_player.as_ref().expect("the inline player");
        assert!(player.asked_to_track("camera.1"));
        assert!(!player.asked_to_track("camera.0"), "tracked already");
    }

    #[test]
    fn a_new_shot_keeps_live_and_picking_a_shot_leaves_it() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.shots = vec![shot_row(1, &[])];
        app.shots_rev += 1;
        app.select_live();
        screen.frame(&mut app, Vec::new());
        app.shots.push(shot_row(2, &[]));
        app.shots_rev += 1;
        app.observe_shots(1.0);
        screen.frame(&mut app, Vec::new());
        assert!(app.live_selected, "tuning is not interrupted");
        assert_eq!(app.shown_index(), Some(1), "the newest is selected beneath");
        app.select_shot(0);
        assert!(!app.live_selected);
        screen.frame(&mut app, Vec::new());
        assert!(app.live.is_empty());
    }

    #[test]
    fn a_swing_done_later_is_fetched_by_the_player() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        let mut row = shot_row(1, &["camera.0"]);
        row.video[0].swing = false;
        app.shots = vec![row];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        assert!(!app.shot_player.as_ref().unwrap().wants_swing_of("camera.0"));
        app.handle_bus_event(
            flighthook::FlighthookMessage::new(flighthook::FlighthookEvent::ShotSwing {
                keys: vec![ShotKey {
                    shot_id: "id1".into(),
                    shot_number: 1,
                }],
            })
            .actor("camera.0"),
        );
        assert!(app.shots[0].video[0].swing);
        assert!(app.shot_player.as_ref().unwrap().wants_swing_of("camera.0"));
    }

    /// Hand camera `camera`'s clip sidecar (a 1280×720 clip, 5 s at 60 fps)
    /// to the inline player.
    fn give_sidecar(app: &mut FlighthookApp, camera: &str) {
        let player = app.shot_player.as_mut().expect("a player");
        let frames: Vec<serde_json::Value> = (0..300u64)
            .map(|i| serde_json::json!({"t_ns": i * 16_666_667, "offset": 4096 + i * 1000, "len": 1000}))
            .collect();
        let json = serde_json::json!({
            "version": 1, "fourcc": "MJPG", "width": 1280, "height": 720,
            "fps": {"nominal": 60.0, "measured": 60.0},
            "start_ns": 1_000_000_000u64,
            "frames": frames,
            "trigger_index": 180, "impact_index": 180, "trigger_ns": 3_000_000_000i64,
            "impact_ns": 3_000_000_000i64,
            "complete": true, "dropped": 0, "truncated": false,
        });
        let seq = player.seq();
        player.reply(crate::net::ReplayReply::Sidecar {
            replay: seq,
            camera: camera.to_owned(),
            result: stance::sidecar::Sidecar::from_json(&json.to_string())
                .map_err(|e| e.to_string()),
        });
    }

    /// Two cameras with a shot and both swing reports, at `w`×`h`: nothing
    /// overlaps, the pictures fill their columns on screen and the swing
    /// data follows under them; Live fits the same way.
    fn layout_at(w: f32, h: f32) {
        let screen = Screen::sized(w, h);
        let mut app = app(&[0, 1], true);
        app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_report(&mut app, "camera.0", report(true));
        give_report(&mut app, "camera.1", report(true));
        give_sidecar(&mut app, "camera.0");
        give_sidecar(&mut app, "camera.1");
        let marks = screen.frame(&mut app, Vec::new());
        let at = format!("{w}x{h}");
        let list = marks["view_selector"].0;
        let toggle = marks["pane_toggle"].0;
        let (a, b) = (
            marks["player:picture:camera.0"].0,
            marks["player:picture:camera.1"].0,
        );
        assert!(a.left() > list.right(), "{at}: right of the list");
        assert!(a.top() > toggle.bottom(), "{at}: under the toolbar");
        assert!(a.right() < b.left(), "{at}: side by side");
        assert!(b.right() <= w, "{at}: on screen");
        assert!(
            a.bottom() <= h && b.bottom() <= h,
            "{at}: whole pictures on screen"
        );
        let pane = w - list.right();
        assert!(
            a.width() + b.width() > pane * 0.85,
            "{at}: the pictures fill the pane"
        );
        let scrubber = marks["player:scrubber"].0;
        assert!(
            scrubber.top() >= a.bottom(),
            "{at}: transport under the clips"
        );
        for (camera, pic) in [("camera.0", a), ("camera.1", b)] {
            let swing = marks[format!("swing:{camera}").as_str()].0;
            assert!(
                swing.top() >= scrubber.bottom(),
                "{at}: {camera} data under the transport"
            );
            assert!(swing.left() >= pic.left() - 1.0 && swing.right() <= pic.right() + 1.0);
        }
        let (sa, sb) = (marks["swing:camera.0"].0, marks["swing:camera.1"].0);
        assert!(
            sa.right() < sb.left(),
            "{at}: the data columns don't overlap"
        );

        // Live: pictures, posture tiles and the readout fit on screen.
        app.select_live();
        let marks = screen.frame(&mut app, Vec::new());
        let (a, b) = (
            marks["camera_live:camera.0"].0,
            marks["camera_live:camera.1"].0,
        );
        assert!(
            a.left() > list.right() && a.right() < b.left() && b.right() <= w,
            "{at}"
        );
        let line = marks["pose_readout"].0;
        for camera in ["camera.0", "camera.1"] {
            let tiles = marks[format!("posture:{camera}").as_str()].0;
            assert!(
                tiles.top() >= a.bottom() && tiles.bottom() <= line.top(),
                "{at}"
            );
        }
        assert!(line.bottom() <= h, "{at}: the readout on screen");
    }

    #[test]
    fn the_cameras_pane_fits_1600x900_with_two_cameras() {
        layout_at(1600.0, 900.0);
    }

    #[test]
    fn the_cameras_pane_fits_1024x768_with_two_cameras() {
        layout_at(1024.0, 768.0);
    }

    fn painted_circles(shapes: &[egui::epaint::ClippedShape]) -> Vec<Pos2> {
        fn collect(shape: &egui::Shape, centers: &mut Vec<Pos2>) {
            match shape {
                egui::Shape::Circle(c) => centers.push(c.center),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, centers)),
                _ => {}
            }
        }
        let mut centers = Vec::new();
        for clipped in shapes {
            collect(&clipped.shape, &mut centers);
        }
        centers
    }

    /// Each text painted with the color it shows in.
    fn painted_colored(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Color32)> {
        fn collect(shape: &egui::Shape, texts: &mut Vec<(String, egui::Color32)>) {
            match shape {
                egui::Shape::Text(t) => {
                    let own = t
                        .galley
                        .job
                        .sections
                        .first()
                        .map_or(t.fallback_color, |s| s.format.color);
                    let color = if own == egui::Color32::PLACEHOLDER {
                        t.fallback_color
                    } else {
                        own
                    };
                    texts.push((t.galley.text().to_owned(), color));
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in shapes {
            collect(&clipped.shape, &mut texts);
        }
        texts
    }

    fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, texts)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in shapes {
            collect(&clipped.shape, &mut texts);
        }
        texts
    }

    /// `report(false)` with its tempo changed to `backswing` and `downswing`
    /// (ns); `None` removes it.
    fn with_tempo(tempo: Option<(u64, u64)>) -> SwingReportFile {
        let mut r = report(false);
        r.tempo = tempo.map(|(backswing_ns, downswing_ns)| SwingTempo {
            backswing_ns,
            downswing_ns,
            ratio: backswing_ns as f64 / downswing_ns as f64,
        });
        r
    }

    fn tiles(reports: &[&SwingReportFile]) -> Vec<String> {
        tempo_tiles(reports).into_iter().map(|(_, v)| v).collect()
    }

    #[test]
    fn the_tempo_is_the_average_over_the_cameras_that_have_one() {
        // 3.0 : 1 (900 / 300 ms) and 2.0 : 1 (600 / 300 ms): the mean
        // durations 750 / 300 ms and their ratio.
        let (a, b) = (
            with_tempo(Some((900_000_000, 300_000_000))),
            with_tempo(Some((600_000_000, 300_000_000))),
        );
        assert_eq!(tiles(&[&a, &b]), ["2.5 : 1", "750 ms", "300 ms"]);
        let (a, b) = (
            with_tempo(Some((800_000_000, 200_000_000))),
            with_tempo(Some((600_000_000, 400_000_000))),
        );
        // Mean durations 700 / 300 ms = 2.3 : 1; the mean of 4.0 and 1.5 is
        // 2.75.
        assert_eq!(tiles(&[&a, &b]), ["2.3 : 1", "700 ms", "300 ms"]);
        // A camera without a tempo is left out.
        let none = with_tempo(None);
        assert_eq!(tiles(&[&a, &none]), tiles(&[&a]));
        assert_eq!(tiles(&[&none, &a]), ["4.0 : 1", "800 ms", "200 ms"]);
        // One camera shows its own numbers, ratio as reported.
        let mut one = with_tempo(Some((841_666_667, 300_000_000)));
        one.tempo.as_mut().unwrap().ratio = 2.85;
        assert_eq!(tiles(&[&one]), ["2.9 : 1", "842 ms", "300 ms"]);
        // None has one: dashes.
        assert!(tiles(&[&none, &none]).iter().all(|v| v == NONE));
        // A sum above u64 keeps its mean.
        let big = with_tempo(Some((u64::MAX, u64::MAX)));
        assert_eq!(average_tempo(&[big.tempo.unwrap(); 2]).unwrap().ratio, 1.0);
    }

    #[test]
    fn two_cameras_show_one_tempo_row_across_both_columns() {
        let screen = Screen::new();
        let mut app = app(&[0, 1], true);
        app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_report(&mut app, "camera.0", report(false));
        let marks = screen.frame(&mut app, Vec::new());
        let one = marks["swing_tempo"].0;
        let left = marks["player:picture:camera.0"].0;
        let right = marks["player:picture:camera.1"].0;
        // One camera with data: the row is still the whole width of the
        // columns, above the metrics.
        assert!((one.left() - left.left()).abs() < 1.0);
        assert!((one.right() - right.right()).abs() < 1.0);
        assert!(one.bottom() <= marks["swing:camera.0"].0.top());
        give_report(
            &mut app,
            "camera.1",
            with_tempo(Some((630_000_000, 270_000_000))),
        );
        let marks = screen.frame(&mut app, Vec::new());
        let row = marks["swing_tempo"].0;
        assert!((row.left() - left.left()).abs() < 1.0);
        assert!((row.right() - right.right()).abs() < 1.0, "full width");
        assert!(row.top() > left.bottom(), "under the clips");
        for camera in ["camera.0", "camera.1"] {
            let metrics = marks[&format!("swing:{camera}")].0;
            assert!(metrics.top() >= row.bottom(), "metrics under the row");
        }
        // Both cameras' metrics keep their own column.
        assert!(marks["swing:camera.0"].0.right() <= marks["swing:camera.1"].0.left());
        // Only one tempo row on screen: the average, 690 / 285 ms.
        let texts = screen.texts();
        assert_eq!(texts.iter().filter(|t| *t == "TEMPO").count(), 1);
        assert!(texts.iter().any(|t| t == "690 ms"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "285 ms"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "2.4 : 1"), "{texts:?}");
    }

    #[test]
    fn a_single_camera_keeps_its_own_tempo_and_layout() {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.shots = vec![shot_row(1, &["camera.0"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_report(&mut app, "camera.0", report(false));
        let marks = screen.frame(&mut app, Vec::new());
        let row = marks["swing_tempo"].0;
        let metrics = marks["swing:camera.0"].0;
        let picture = marks["player:picture:camera.0"].0;
        assert!((row.left() - picture.left()).abs() < 1.0,);
        assert!((row.width() - picture.width()).abs() < 1.0, "the column");
        assert!(metrics.top() >= row.bottom());
        let texts = screen.texts();
        assert!(texts.iter().any(|t| t == "2.5 : 1"), "{texts:?}");
        // A camera that found no swing has no tempo row at all.
        let mut app = self::app(&[0], true);
        app.shots = vec![shot_row(1, &["camera.0"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_report(&mut app, "camera.0", no_swing_report());
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("swing_tempo"));
        assert!(marks.contains_key("swing:camera.0"));
    }

    /// The swing data under a clip is the tempo tiles and the metrics table:
    /// no row of event times (those are the scrubber's markers).
    #[test]
    fn the_swing_panel_has_no_event_times_row() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let r = report(false);
        let out = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    tempo_row(ui, &tempo_values(&plain(&[(&r, None)])), 600.0);
                    swing_metrics(ui, "camera.0", &r, true, 600.0, None, ReferenceKind::Tour);
                });
            },
        );
        let texts = painted_texts(&out.shapes);
        for gone in ["TAKEAWAY", "-1.15 s", "-0.30 s", "0.00 s", "+0.65 s"] {
            assert!(!texts.iter().any(|t| t == gone), "{gone}: {texts:?}");
        }
        // Finish is the table's column, once.
        assert_eq!(texts.iter().filter(|t| *t == "FINISH").count(), 1);
        // The tempo tiles and the metrics table are still there.
        for kept in [
            "2.5 : 1",
            "750 ms",
            "300 ms",
            "Hip sway",
            "5% torso",
            "Away",
            "12% torso",
            "Toward",
            "Side bend",
            "ADDRESS",
        ] {
            assert!(
                texts.iter().any(|t| t.eq_ignore_ascii_case(kept)),
                "{kept}: {texts:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Grades: colors, words, key positions, the reference
    // -----------------------------------------------------------------------

    /// A grade of `id` at `position` with `fit`, its value and its words.
    fn grade_of(id: &str, position: &str, fit: Fit, value: f64, word: Option<&str>) -> MetricGrade {
        let side = match fit {
            Fit::Both => flighthook::Side::On,
            _ if value < 0.0 => flighthook::Side::Low,
            _ => flighthook::Side::High,
        };
        MetricGrade {
            id: id.into(),
            position: position.into(),
            value,
            unit: "degrees".into(),
            low: 33.0,
            high: 43.0,
            fade: None,
            draw: None,
            z: 0.0,
            z_fade: None,
            z_draw: None,
            fit,
            side,
            direction: (fit != Fit::Both).then(|| "less".to_owned()),
            word: word.map(str::to_owned),
            proxy_sd: false,
            assumed_scale: false,
            club_fallback: false,
            sex_fallback: false,
            confidence: 0.9,
        }
    }

    fn reference_grades(
        reference: ReferenceKind,
        grades: Vec<MetricGrade>,
        tempo: Vec<MetricGrade>,
    ) -> ReferenceGrades {
        ReferenceGrades {
            reference,
            grades,
            tempo,
        }
    }

    #[test]
    fn the_five_fits_have_five_colors_apart_from_plain_text() {
        let fits = [Fit::Both, Fit::Fade, Fit::Draw, Fit::Near, Fit::Far];
        let colors: Vec<egui::Color32> = fits.iter().map(|f| grade_color(*f)).collect();
        assert_eq!(
            colors,
            [
                theme::GRADE_BOTH,
                theme::GRADE_FADE,
                theme::GRADE_DRAW,
                theme::GRADE_NEAR,
                theme::GRADE_FAR
            ]
        );
        for (i, a) in colors.iter().enumerate() {
            assert_ne!(*a, theme::TEXT);
            for b in &colors[i + 1..] {
                assert_ne!(a, b);
            }
        }
        // The fade blue is not the headings' blue.
        assert_ne!(theme::GRADE_FADE, theme::HEADING);
    }

    #[test]
    fn a_graded_value_takes_its_color_and_its_word() {
        let shown = |id, unit, v, cm, g: Option<&MetricGrade>| {
            let s = graded_shown(id, unit, v, cm, true, g);
            (joined(&s.text, s.word), s.color)
        };
        // Ungraded: plain, as before, a turn without a word.
        assert_eq!(
            shown("hip_turn", "degrees", 44.0, None, None),
            ("44°".to_owned(), theme::TEXT)
        );
        // A graded turn reads Open or Closed by its signed value, in every fit.
        for fit in [Fit::Both, Fit::Fade, Fit::Draw, Fit::Near, Fit::Far] {
            let g = grade_of(
                "hip_turn",
                "impact",
                fit,
                44.0,
                Some("more open: fade side"),
            );
            assert_eq!(
                shown("hip_turn", "degrees", 44.0, None, Some(&g)),
                ("44° Open".to_owned(), grade_color(fit)),
                "{fit:?}"
            );
        }
        let closed = grade_of("shoulder_turn", "impact", Fit::Near, -12.0, None);
        assert_eq!(
            shown("shoulder_turn", "degrees", 12.0, None, Some(&closed)).0,
            "12° Closed"
        );
        // A shift keeps its own word from address in its color.
        let g = grade_of(
            "hip_sway",
            "impact",
            Fit::Fade,
            2.1,
            Some("further away from the target: fade side"),
        );
        assert_eq!(
            shown("hip_sway", "torso", 0.05, Some(5.334), Some(&g)),
            ("2.1 in Toward".to_owned(), theme::GRADE_FADE)
        );
        // Anything else off its band takes its side's word; inside, none.
        let g = grade_of(
            "spine_forward_bend",
            "impact",
            Fit::Near,
            29.0,
            Some("less than tour: stood up"),
        );
        assert_eq!(
            shown("spine_forward_bend", "degrees", 29.0, None, Some(&g)),
            ("29° Less".to_owned(), theme::GRADE_NEAR)
        );
        let g = grade_of("spine_forward_bend", "impact", Fit::Both, 35.0, None);
        assert_eq!(
            shown("spine_forward_bend", "degrees", 35.0, None, Some(&g)),
            ("35°".to_owned(), theme::GRADE_BOTH)
        );
    }

    #[test]
    fn a_tile_is_colored_only_on_a_key_positions_frame() {
        let fo = face_on_series(true);
        // The one-camera turn has a grade at impact only; hip sway at the top.
        let tour = reference_grades(
            ReferenceKind::Tour,
            vec![
                grade_of(
                    "hip_turn",
                    "impact",
                    Fit::Fade,
                    18.0,
                    Some("more open: fade side"),
                ),
                grade_of(
                    "hip_sway",
                    "top",
                    Fit::Near,
                    -1.8,
                    Some("further away from the target than tour: sway"),
                ),
            ],
            Vec::new(),
        );
        let tiles_at = |frame: u64| {
            frame_tiles(
                &[Swing {
                    report: &fo,
                    t_ns: Some(frame * FRAME_NS),
                    grades: Some(&tour),
                }],
                false,
            )
        };
        // On the impact frame and one frame either side: the turn is
        // colored with its word; the hip sway (no impact grade) is plain.
        for frame in [179, 180, 181] {
            let tiles = tiles_at(frame);
            assert_eq!(tiles[0].color, theme::GRADE_FADE, "{frame}");
            assert_eq!(tiles[0].word, Some("Open"), "{frame}");
            assert!(tiles[0].grade.is_some());
            assert_eq!(tiles[2].color, theme::TEXT, "{frame}");
            assert!(tiles[2].grade.is_none());
        }
        // Two frames off: plain, no word.
        for frame in [178, 182, 250] {
            let tiles = tiles_at(frame);
            assert!(
                tiles
                    .iter()
                    .all(|t| t.color == theme::TEXT && t.grade.is_none()),
                "{frame}"
            );
            assert_eq!(tiles[0].word, None, "{frame}");
        }
        // On the top: the one-camera turn is not graded there, the hip sway
        // is.
        let tiles = tiles_at(150);
        assert_eq!((tiles[0].color, tiles[0].word), (theme::TEXT, None));
        assert_eq!(tiles[2].color, theme::GRADE_NEAR);
        assert_eq!(key_position(&fo, 150 * FRAME_NS), Some(SwingPosition::Top));
        assert_eq!(
            key_position(&fo, 60 * FRAME_NS),
            Some(SwingPosition::Address)
        );
        assert_eq!(key_position(&fo, 100 * FRAME_NS), None);
        // Without grades nothing is colored.
        let plain_tiles = frame_tiles(&plain(&[(&fo, Some(180 * FRAME_NS))]), false);
        assert!(plain_tiles.iter().all(|t| t.color == theme::TEXT));
    }

    #[test]
    fn averaged_tiles_take_the_grade_the_cameras_share() {
        let (a, b) = (face_on_series(true), face_on_series(true));
        let fade = |fit| {
            reference_grades(
                ReferenceKind::Tour,
                vec![grade_of("hip_turn", "impact", fit, 18.0, Some("words"))],
                vec![grade_of("tempo_ratio", "swing", fit, 3.0, Some("words"))],
            )
        };
        let (g1, g2, g3) = (fade(Fit::Fade), fade(Fit::Fade), fade(Fit::Near));
        let at = Some(180 * FRAME_NS);
        let swing = |report, grades| Swing {
            report,
            t_ns: at,
            grades: Some(grades),
        };
        // Two face-on cameras that agree: the color; that disagree: none.
        let agree = frame_tiles(&[swing(&a, &g1), swing(&b, &g2)], false);
        assert_eq!(agree[0].color, theme::GRADE_FADE);
        let differ = frame_tiles(&[swing(&a, &g1), swing(&b, &g3)], false);
        assert_eq!(differ[0].color, theme::TEXT);
        assert!(differ[0].grade.is_none());
        // One of them without grades: none either.
        let half = frame_tiles(
            &[
                swing(&a, &g1),
                Swing {
                    report: &b,
                    t_ns: at,
                    grades: None,
                },
            ],
            false,
        );
        assert_eq!(half[0].color, theme::TEXT);
        // The tempo: the face-on camera's grade over another view's; the
        // first camera's when no face-on camera has one.
        let dtl = down_the_line_series();
        let tempo = tempo_values(&[swing(&dtl, &g3), swing(&a, &g1)]);
        assert_eq!(
            tempo.iter().map(|t| t.color).collect::<Vec<_>>(),
            [theme::GRADE_FADE, theme::TEXT, theme::TEXT],
            "ratio graded, backswing never, downswing not in these grades"
        );
        let tempo = tempo_values(&[
            swing(&dtl, &g3),
            Swing {
                report: &a,
                t_ns: at,
                grades: None,
            },
        ]);
        assert_eq!(tempo[0].color, theme::GRADE_NEAR);
        let tempo = tempo_values(&[swing(&a, &g1), swing(&b, &g3)]);
        assert_eq!(tempo[0].color, theme::TEXT, "two face-on cameras disagree");
    }

    #[test]
    fn the_miss_line_lists_the_tiles_off_their_band() {
        let tile = |label, grade: Option<MetricGrade>| FrameValue {
            id: "x",
            label,
            text: "1".into(),
            word: None,
            color: theme::TEXT,
            grade,
        };
        let tiles = [
            tile(
                "Hip turn",
                Some(grade_of(
                    "hip_turn",
                    "impact",
                    Fit::Fade,
                    44.0,
                    Some("more open: fade side"),
                )),
            ),
            tile(
                "Shoulder turn",
                Some(grade_of("shoulder_turn", "impact", Fit::Both, 31.0, None)),
            ),
            tile("Head sway", None),
            tile(
                "Forward bend",
                Some(grade_of(
                    "spine_forward_bend",
                    "impact",
                    Fit::Far,
                    20.0,
                    Some("less than tour: stood up, early extension pattern"),
                )),
            ),
        ];
        let entries = miss_entries(&tiles);
        assert_eq!(
            entries,
            [
                ("Hip turn", "more open: fade side".to_owned(), Fit::Fade),
                (
                    "Forward bend",
                    "less than tour: stood up, early extension pattern".to_owned(),
                    Fit::Far
                ),
            ]
        );
        let job = miss_job(&entries, 400.0);
        assert_eq!(
            job.text,
            "Hip turn more open: fade side \u{b7} Forward bend less than tour: stood up, early extension pattern"
        );
        // Names muted, words in their fit's color.
        let colors: Vec<egui::Color32> = job.sections.iter().map(|s| s.format.color).collect();
        assert_eq!(
            colors,
            [
                theme::TEXT_MUTED,
                theme::GRADE_FADE,
                theme::TEXT_MUTED,
                theme::TEXT_MUTED,
                theme::GRADE_FAR
            ]
        );
        assert_eq!(job.wrap.max_rows, 2, "two lines at most");
        assert!(
            miss_entries(&tiles[1..3]).is_empty(),
            "all inside or ungraded"
        );
    }

    #[test]
    fn a_cells_hover_names_the_band_in_the_units() {
        assert_eq!(band_text(33.0, 43.0, "degrees", true), "33\u{2013}43\u{b0}");
        assert_eq!(band_text(1.5, 5.5, "inches", true), "1.5\u{2013}5.5 in");
        // Rounded after the conversion.
        assert_eq!(band_text(1.5, 5.5, "inches", false), "3.8\u{2013}14.0 cm");
        assert_eq!(band_text(0.2, 0.3, "seconds", true), "0.20\u{2013}0.30 s");
        assert_eq!(band_text(2.5, 3.5, "ratio", true), "2.5\u{2013}3.5");
        let mut hips = grade_of(
            "hip_turn",
            "impact",
            Fit::Near,
            48.0,
            Some("more open than tour: fade or slice pattern"),
        );
        hips.fade = Some([36.0, 46.0]);
        hips.draw = Some([30.0, 40.0]);
        let shown = graded_shown("hip_turn", "degrees", 48.0, None, true, Some(&hips));
        assert_eq!(
            band_tip(ReferenceKind::Tour, &hips, &shown, true),
            "Tour \u{b7} fade 36\u{2013}46\u{b0} \u{b7} draw 30\u{2013}40\u{b0} \u{b7} 48\u{b0} Open \u{b7} more open than tour: fade or slice pattern"
        );
        let bend = grade_of("spine_forward_bend", "impact", Fit::Both, 35.0, None);
        let shown = graded_shown(
            "spine_forward_bend",
            "degrees",
            35.0,
            None,
            true,
            Some(&bend),
        );
        assert_eq!(
            band_tip(ReferenceKind::Amateur, &bend, &shown, true),
            "Amateur 33\u{2013}43\u{b0} \u{b7} 35\u{b0}"
        );
    }

    #[test]
    fn the_table_colors_its_graded_cells_and_widens_to_the_lead_arm_positions() {
        assert_eq!(table_positions(WIDE_TABLE - 1.0).len(), 4);
        assert_eq!(table_positions(WIDE_TABLE), SwingPosition::ALL.as_slice());
        assert_eq!(
            table_positions(400.0)
                .iter()
                .map(|p| p.label())
                .collect::<Vec<_>>(),
            ["Address", "Top", "Impact", "Finish"]
        );
        let r = report(false);
        let tour = reference_grades(
            ReferenceKind::Tour,
            vec![grade_of(
                "lateral_spine_tilt",
                "impact",
                Fit::Draw,
                21.2,
                Some("more: draw side"),
            )],
            Vec::new(),
        );
        let rows = metric_rows(&r, true, table_positions(400.0), Some(&tour));
        let side_bend = &rows.iter().find(|(n, _)| n == "Side bend").unwrap().1;
        let colors: Vec<_> = side_bend.iter().map(|c| c.shown.color).collect();
        assert_eq!(
            colors,
            [theme::TEXT, theme::TEXT, theme::GRADE_DRAW, theme::TEXT],
            "only the graded cell"
        );
        // The cell keeps the table's own text; its hover has the words.
        assert_eq!(side_bend[2].shown.text, "21\u{b0}");
        assert_eq!(side_bend[2].shown.word, None);
        assert_eq!(side_bend[2].worded.word, Some("Less"));
        // The finish column: the shoulder tilt's finish-only value.
        let tilt = &rows.iter().find(|(n, _)| n == "Shoulder tilt").unwrap().1;
        assert_eq!(tilt[3].shown.text, "30\u{b0}");
    }

    fn give_grades(app: &mut FlighthookApp, camera: &str, grades: flighthook::BodyGrades) {
        let player = app.shot_player.as_mut().expect("a player");
        let seq = player.seq();
        player.reply(crate::net::ReplayReply::Grades {
            replay: seq,
            camera: camera.to_owned(),
            result: Ok(grades),
        });
    }

    /// Grades of camera `camera`: the hip turn at impact against tour (a
    /// fade) and against amateurs (near).
    fn hip_grades(camera: &str) -> flighthook::BodyGrades {
        flighthook::BodyGrades {
            shot_id: "id1".into(),
            camera: camera.into(),
            club_class: Some(flighthook::ClubClass::Driver),
            sex: flighthook::Sex::Male,
            hand: Handedness::Right,
            references: vec![
                reference_grades(
                    ReferenceKind::Tour,
                    vec![grade_of(
                        "hip_turn",
                        "impact",
                        Fit::Fade,
                        18.0,
                        Some("more open: fade side"),
                    )],
                    Vec::new(),
                ),
                reference_grades(
                    ReferenceKind::Amateur,
                    vec![grade_of(
                        "hip_turn",
                        "impact",
                        Fit::Near,
                        18.0,
                        Some("more open than amateurs: fade or slice pattern"),
                    )],
                    Vec::new(),
                ),
            ],
        }
    }

    /// A shot of a `hand` golfer in the Cameras pane with the face-on
    /// camera's report, clip and grades, paused on the impact frame.
    fn graded_shot(hand: Handedness) -> (Screen, FlighthookApp) {
        let screen = Screen::new();
        let mut app = app(&[0], true);
        app.set_body_reference(ReferenceKind::Tour);
        let mut row = shot_row(1, &["camera.0"]);
        row.handed = Some(hand);
        app.shots = vec![row];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_report(&mut app, "camera.0", face_on_series(true));
        give_sidecar(&mut app, "camera.0");
        give_grades(&mut app, "camera.0", hip_grades("camera.0"));
        screen.frame(&mut app, Vec::new());
        seek_to_frame(&mut app, 180);
        screen.frame(&mut app, Vec::new());
        (screen, app)
    }

    #[test]
    fn the_reference_picker_switches_the_colors_and_is_kept() {
        let (screen, mut app) = graded_shot(Handedness::Right);
        let marks = screen.frame(&mut app, Vec::new());
        // Beside the Live switch, in the toolbar.
        let (live, picker) = (marks["live_toggle"].0, marks["body_reference"].0);
        assert!(picker.left() > live.right() && (picker.center().y - live.center().y).abs() < 2.0);
        assert!(
            screen.texts().iter().any(|t| t == "Tour")
                && screen.texts().iter().any(|t| t == "Amateur")
        );
        assert!(
            !screen.texts().iter().any(|t| t == "Mine"),
            "not before personal bands"
        );
        // Tour: the hips in the fade band, blue, with the turn's word and the
        // miss line.
        assert_eq!(screen.color_of("18°"), Some(theme::GRADE_FADE));
        assert_eq!(screen.color_of("Open"), Some(theme::GRADE_FADE));
        assert!(marks.contains_key("frame_misses"));
        assert!(
            screen
                .texts()
                .iter()
                .any(|t| t.contains("Hip turn more open: fade side")),
            "{:?}",
            screen.texts()
        );
        // Amateur: yellow, its own words.
        let at = marks["body_reference:amateur"].0.center();
        screen.click(&mut app, at);
        assert_eq!(app.body_reference, ReferenceKind::Amateur);
        assert_eq!(
            stored_reference(),
            ReferenceKind::Amateur,
            "kept on this device"
        );
        screen.frame(&mut app, Vec::new());
        assert_eq!(screen.color_of("18°"), Some(theme::GRADE_NEAR));
        assert!(screen.texts().iter().any(|t| t.contains("than amateurs")));
        // Off the key position: plain, and the miss line's words go.
        seek_to_frame(&mut app, 120);
        screen.frame(&mut app, Vec::new());
        assert_eq!(screen.color_of("12°"), Some(theme::TEXT));
        assert!(!screen.texts().iter().any(|t| t.contains("than amateurs")));
        app.set_body_reference(ReferenceKind::Tour);
        // Not in the Data pane.
        app.set_shots_pane(ShotsPane::Data);
        let marks = screen.frame(&mut app, Vec::new());
        assert!(!marks.contains_key("body_reference"));
    }

    #[test]
    fn a_left_handed_golfers_tiles_read_the_same() {
        let (right, mut rh) = graded_shot(Handedness::Right);
        let (left, mut lh) = graded_shot(Handedness::Left);
        right.frame(&mut rh, Vec::new());
        left.frame(&mut lh, Vec::new());
        assert_eq!(row_values(&right), row_values(&left));
        for text in ["18°", "Open"] {
            assert_eq!(right.color_of(text), left.color_of(text), "{text}");
        }
    }

    /// Answer the Last shot strip being fetched: camera `camera`'s swing
    /// report and its grades.
    fn answer_strip(
        app: &mut FlighthookApp,
        screen: &Screen,
        camera: &str,
        report: SwingReportFile,
    ) {
        screen.frame(app, Vec::new());
        let seq = app.live_strip.next_seq().expect("a strip being fetched");
        let strip = app.live_strip.find_mut(seq).unwrap();
        assert!(strip.reply(crate::net::ReplayReply::Swing {
            replay: seq,
            camera: camera.to_owned(),
            result: Ok(report),
        }));
        screen.frame(app, Vec::new());
        let strip = app.live_strip.find_mut(seq).unwrap();
        assert!(strip.reply(crate::net::ReplayReply::Grades {
            replay: seq,
            camera: camera.to_owned(),
            result: Ok(hip_grades(camera)),
        }));
    }

    #[test]
    fn live_shows_the_last_shots_swing_under_the_pictures_until_the_next_one_is_in() {
        for (w, h) in [(1600.0, 900.0), (1024.0, 768.0)] {
            let screen = Screen::sized(w, h);
            let mut app = app(&[0], true);
            set_mode(&mut app, BodyTrackingMode::Live);
            app.set_body_reference(ReferenceKind::Tour);
            app.shots = vec![shot_row(1, &["camera.0"])];
            app.shots_rev += 1;
            app.set_shots_pane(ShotsPane::Cameras);
            app.select_live();
            let marks = screen.frame(&mut app, Vec::new());
            assert!(marks.contains_key("camera_live:camera.0"));
            assert!(
                !marks.contains_key("live_last_shot"),
                "nothing before the swing"
            );
            answer_strip(&mut app, &screen, "camera.0", face_on_series(true));
            let marks = screen.frame(&mut app, Vec::new());
            let strip = marks["live_last_shot"].0;
            let picture = marks["camera_live:camera.0"].0;
            assert!(strip.top() >= picture.bottom(), "under the pictures");
            assert!(
                (strip.left() - picture.left()).abs() < 0.5,
                "{strip:?} {picture:?}"
            );
            assert!(strip.bottom() <= h, "on screen at {w}x{h}");
            let readout = marks["pose_readout"].0;
            assert!(readout.top() >= strip.bottom() && readout.bottom() <= h);
            assert!(screen.texts().iter().any(|t| t == "Last shot \u{b7} #1"));
            // The body at impact (frame 180), colored, with the misses.
            assert_eq!(screen.color_of("18°"), Some(theme::GRADE_FADE));
            assert!(
                screen
                    .texts()
                    .iter()
                    .any(|t| t.contains("Hip turn more open: fade side"))
            );
            // The next shot: the strip stays until its swing is in.
            app.shots.push(shot_row(2, &["camera.0"]));
            app.shots_rev += 1;
            screen.frame(&mut app, Vec::new());
            assert!(app.live_selected, "a new shot keeps Live");
            assert!(screen.texts().iter().any(|t| t == "Last shot \u{b7} #1"));
            answer_strip(&mut app, &screen, "camera.0", face_on_series(false));
            screen.frame(&mut app, Vec::new());
            assert!(screen.texts().iter().any(|t| t == "Last shot \u{b7} #2"));
            assert!(!screen.texts().iter().any(|t| t == "Last shot \u{b7} #1"));
            // Body tracking off: no strip.
            set_mode(&mut app, BodyTrackingMode::Off);
            let marks = screen.frame(&mut app, Vec::new());
            assert!(!marks.contains_key("live_last_shot"));
        }
    }

    /// A shot's clips each have a download icon; Live, which has no
    /// clip to download, has none.
    #[test]
    fn only_a_shots_clips_have_a_download_icon() {
        let screen = Screen::new();
        let mut app = app(&[0, 1], true);
        app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("player:download:camera.0"));
        assert!(marks.contains_key("player:download:camera.1"));
        app.select_live();
        screen.frame(&mut app, Vec::new());
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("camera_live:camera.0"));
        assert!(!marks.keys().any(|k| k.contains("download")), "{marks:?}");
    }

    /// The Cameras pane's scrubber marks the swing events once a report is
    /// in, in swing order and inside the scrubber, with impact left to the
    /// gold tick.
    #[test]
    fn the_panes_scrubber_marks_the_swing_events_once_reports_are_in() {
        let screen = Screen::new();
        let mut app = app(&[0, 1], true);
        app.shots = vec![shot_row(1, &["camera.0", "camera.1"])];
        app.shots_rev += 1;
        app.set_shots_pane(ShotsPane::Cameras);
        screen.frame(&mut app, Vec::new());
        give_sidecar(&mut app, "camera.0");
        give_sidecar(&mut app, "camera.1");
        let marks = screen.frame(&mut app, Vec::new());
        assert!(marks.contains_key("player:scrubber"));
        let events = |marks: &Marks| -> Vec<String> {
            let mut events: Vec<(f32, String)> = marks
                .iter()
                .filter_map(|(k, (r, _))| {
                    k.strip_prefix("player:event:")
                        .map(|n| (r.center().x, n.to_owned()))
                })
                .collect();
            events.sort_by(|a, b| a.0.total_cmp(&b.0));
            events.into_iter().map(|(_, n)| n).collect()
        };
        assert!(events(&marks).is_empty(), "no report yet");
        give_report(&mut app, "camera.0", report(false));
        give_report(&mut app, "camera.1", report(false));
        let marks = screen.frame(&mut app, Vec::new());
        assert_eq!(events(&marks), ["address", "takeaway", "top", "finish"]);
        let scrubber = marks["player:scrubber"].0;
        for name in ["address", "takeaway", "top", "finish"] {
            let rect = marks[&format!("player:event:{name}")].0;
            assert!(rect.intersects(scrubber), "{name}");
        }
    }
}
