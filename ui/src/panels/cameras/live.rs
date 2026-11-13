//! The Cameras pane and its Live entry: each camera's live view (`LiveView`, its fetch loop and
//! texture), the body tracking readout and tuning saves, the status bands, and the pane state the
//! Shots tab reads (`impl FlighthookApp`).

use super::*;

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
    pub(super) wanted: Option<BodyTrackingSection>,
    /// The values of the save that is out.
    pub(super) in_flight: Option<BodyTrackingSection>,
    /// The values of the last save the server took.
    pub(super) confirmed: Option<BodyTrackingSection>,
    /// Every save sent, in tests.
    #[cfg(test)]
    pub(crate) sent: Vec<BodyTrackingSection>,
}

impl Tuning {
    /// The values in effect: the wanted ones, else the saved ones.
    pub(crate) fn current(&self, saved: &BodyTrackingSection) -> BodyTrackingSection {
        self.wanted.clone().unwrap_or_else(|| saved.clone())
    }

    pub(super) fn set(&mut self, values: BodyTrackingSection) {
        self.wanted = Some(values);
    }

    /// The values to save now, marked as out: the wanted ones, unless a
    /// save is out or the server already holds them.
    pub(super) fn due(&mut self, saved: &BodyTrackingSection) -> Option<BodyTrackingSection> {
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
    pub(super) fn settle(&mut self, saved: &BodyTrackingSection) {
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
    pub(super) id: u64,
    /// A request is out.
    pub(super) in_flight: bool,
    /// The sequence number of the frame received last.
    pub(super) after: u64,
    /// No request before this time (egui input time, s).
    pub(super) retry_at: f64,
    /// The newest frame received, not decoded yet, and its pose.
    pub(super) jpeg: Option<(Vec<u8>, Option<LivePose>)>,
    /// The picture on screen.
    pub(super) texture: Option<egui::TextureHandle>,
    /// The pose found on the picture on screen.
    pub(super) shown_pose: Option<LivePose>,
    /// Body tracking's state, as the last reply had it.
    pub(super) pose_state: PoseState,
    /// When the frames of the last [`FPS_WINDOW_S`] arrived (s).
    pub(super) arrivals: VecDeque<f64>,
    /// Pose time per frame, averaged (ms).
    pub(super) pose_ms: Option<f32>,
}

impl LiveView {
    pub(super) fn new(id: u64) -> Self {
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
    pub(super) fn due(&mut self, now: f64) -> Option<u64> {
        if self.in_flight || now < self.retry_at {
            return None;
        }
        self.in_flight = true;
        Some(self.after)
    }

    /// Take the reply to the request out, answered at `now`. A frame
    /// numbered below the last one is taken too: the server restarted and
    /// numbers its frames afresh.
    pub(super) fn reply(&mut self, reply: LiveReply, now: f64) {
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
    pub(super) fn fps(&self, now: f64) -> Option<f64> {
        let first = self.arrivals.iter().find(|t| now - **t <= FPS_WINDOW_S)?;
        let last = self.arrivals.back()?;
        let n = self.arrivals.iter().filter(|t| *t >= first).count();
        (n >= 2 && last > first).then(|| (n - 1) as f64 / (last - first))
    }

    /// The picture, decoding a newly received frame into it (its pose goes
    /// with it). A frame that does not decode leaves the last picture up,
    /// with its own pose.
    pub(super) fn texture(
        &mut self,
        ctx: &egui::Context,
        actor: &str,
    ) -> Option<&egui::TextureHandle> {
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
    pub(super) fn aspect(&self) -> Option<f32> {
        let [w, h] = self.texture.as_ref()?.size();
        (w > 0).then(|| h as f32 / w as f32)
    }
}

/// Whether the app's window is on screen: not minimized, and in a browser
/// not a hidden tab.
pub(super) fn page_visible(ctx: &egui::Context) -> bool {
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
pub(super) fn resolution_aspect(resolution: &str) -> Option<f32> {
    let (w, h) = resolution.split_once('x')?;
    let (w, h) = (w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?);
    (w > 0.0 && h > 0.0).then_some(h / w)
}

/// The width of each of `n` side-by-side pictures of height-over-width
/// `aspect` in `available`: as wide as the pane allows, unless a picture
/// and its heading would be taller than the pane.
pub(super) fn column_width(n: usize, available: egui::Vec2, aspect: f32) -> f32 {
    let n = n.max(1) as f32;
    let widest = (available.x - GAP * (n - 1.0)) / n;
    let tallest = (available.y - HEADING_H) / aspect;
    widest.min(tallest).max(1.0)
}

impl FlighthookApp {
    /// Whether the Cameras tab is available (its nav icon enabled): the
    /// server can record from cameras
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

    /// What the body right of the shots navigation lays out: the Cameras
    /// tab while it is chosen ([`FlighthookApp::cameras_pane_shown`]), else
    /// the Shots tab in the Cards | Table choice.
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

    /// Show the Shots tab (`Data`) or the Cameras tab, remembered on this
    /// device. The Shots tab leaves Live: Live is part of the Cameras tab.
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

    /// Each time the Cameras pane comes on screen: Live when there is no
    /// shot to show under the filter (the viewed golfer's, narrowed by the
    /// device filter). Opened from outside the shots navigation (another
    /// tab, or the shot history cleared while it shows) it shows the latest
    /// shot, any pin dropped; a switch from the Shots tab keeps the selected
    /// shot. A view change applies the opening rule too (`set_view`).
    /// Called before anything reads the pane's state in a frame; a call
    /// with no change does nothing.
    pub(crate) fn sync_cameras_pane(&mut self) {
        let shown = self.cameras_pane_shown();
        if shown && !self.cameras_pane_was_shown {
            if !self.shots_nav_was_shown {
                self.shot_focus.follow();
            }
            self.live_selected = self.shown_index().is_none();
        }
        self.cameras_pane_was_shown = shown;
        self.shots_nav_was_shown = self.active_tab == Tab::Shots;
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
    pub(super) fn fetch_live_frames(&mut self, ctx: &egui::Context) {
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
            self.render_empty_state(
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
    pub(super) fn render_live(&mut self, ui: &mut egui::Ui) {
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
    pub(super) fn pose_readout(&self, cameras: &[(String, CameraSection)], now: f64) -> String {
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
    pub(super) fn camera_aspect(&self, actor: &str) -> f32 {
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
    pub(super) fn camera_column(
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
    pub(super) fn pose_over(&self, ui: &egui::Ui, actor: &str, rect: egui::Rect, frame: [u32; 2]) {
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
pub(super) fn status_band(ui: &egui::Ui, rect: egui::Rect, text: &str) -> egui::Response {
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
pub(super) fn status_over(
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
