//! Body tracking's model download (alpha): nothing is downloaded until the
//! user says so here.
//!
//! - Turning body tracking on (Off → Historical or Live on the Body
//!   tracking card) asks the server where the model files are first
//!   (`GET /api/body_tracking/models`). Files already there: the mode is
//!   saved at once. Files missing: a dialog names what would be downloaded,
//!   how big, from which hosts and where it is kept; Cancel saves nothing,
//!   Download saves the mode and starts the download.
//! - Body tracking already on with files missing (a hand-edited config,
//!   models deleted): the same dialog, the first time something that needs
//!   them opens (the Cameras pane on a shot, Live in Live mode, Replay).
//! - While the download runs, an overlay replaces the dialog and polls the
//!   state about 4 times a second: per-file and overall progress, the
//!   current host, and Cancel (stops the download and turns body tracking
//!   off). A failure shows why, with Retry and Turn off. Once the files are
//!   there the overlay closes by itself and the clips on screen are asked
//!   to be tracked again.
//!
//! The mode is saved through the body tracking settings alone
//! (`scope=body_tracking`), so other unsaved edits on the card stay
//! unsaved.

use flighthook::{BodyTrackingMode, BodyTrackingModels, ModelFilesState, PoseModel};

use crate::app::FlighthookApp;
use crate::net::ModelsCall;
use crate::panels::devices;
use crate::theme;

/// Seconds between two polls while the overlay shows a download.
const POLL_S: f64 = 0.25;

/// Width of the dialog and the overlay, points.
const WIDTH: f32 = 470.0;

/// What is on screen.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Shown {
    /// Waiting for the state, before deciding; nothing drawn. `turn_on` is
    /// the mode picked on the card, `None` for a first need.
    Checking { turn_on: Option<BodyTrackingMode> },
    /// The dialog: what would be downloaded.
    Ask {
        turn_on: Option<BodyTrackingMode>,
        models: BodyTrackingModels,
    },
    /// The overlay: the download, or its failure. `started` once the
    /// server has answered the POST; polls run only then.
    Progress {
        models: BodyTrackingModels,
        started: bool,
    },
}

/// The download prompt's state.
#[derive(Debug, Default)]
pub(crate) struct ModelsPrompt {
    pub(crate) shown: Option<Shown>,
    /// A state request is out.
    in_flight: bool,
    /// No poll before this time (egui input time, s).
    next_poll: f64,
    /// The model the first-need check ran for while body tracking is on.
    checked: Option<PoseModel>,
    /// Every request sent, in tests.
    #[cfg(test)]
    pub(crate) sent: Vec<ModelsCall>,
}

impl ModelsPrompt {
    /// Whether the dialog or the overlay is up (or about to be).
    pub(crate) fn is_open(&self) -> bool {
        self.shown.is_some()
    }
}

/// Bytes in megabytes, one decimal.
fn mb(bytes: u64) -> String {
    // Model sizes are far below 2^52.
    #[allow(clippy::cast_precision_loss)]
    let mb = bytes as f64 / 1e6;
    format!("{mb:.1} MB")
}

/// The dialog's lines: the opening sentence, one line per missing file
/// (what, size, host), where they are kept, and that body tracking runs
/// on this computer afterwards.
pub(crate) fn ask_lines(models: &BodyTrackingModels) -> (String, Vec<String>, String) {
    let missing = models.missing();
    let total: u64 = missing.iter().map(|f| f.size).sum();
    // Whole megabytes for the total.
    #[allow(clippy::cast_precision_loss)]
    let about = (total as f64 / 1e6).round();
    let opening = if missing.len() == 1 {
        format!("Body tracking needs this model, downloaded once (about {about:.0} MB):")
    } else {
        format!("Body tracking needs these models, downloaded once (about {about:.0} MB):")
    };
    let files = missing
        .iter()
        .map(|f| format!("{}, {}, from {}", f.label, mb(f.size), f.host))
        .collect();
    let kept = format!("They are kept in {}.", models.dir);
    (opening, files, kept)
}

/// One progress bar: what, bytes done, bytes in all.
type Row = (String, u64, u64);

/// The overlay's rows while downloading: one per file, then the whole
/// download (done, total), and the current host. Before the server has
/// answered, the files to download at 0.
pub(crate) fn progress_rows(models: &BodyTrackingModels) -> (Vec<Row>, (u64, u64), Option<String>) {
    if let Some(d) = &models.download {
        let rows = d
            .files
            .iter()
            .map(|f| (f.label.clone(), f.done, f.total))
            .collect();
        let host = d.current_file().map(|f| f.host.clone());
        return (rows, (d.done, d.total), host);
    }
    let missing = models.missing();
    let rows: Vec<Row> = missing
        .iter()
        .map(|f| (f.label.clone(), 0, f.size))
        .collect();
    let total = rows.iter().map(|r| r.2).sum();
    let host = missing.first().map(|f| f.host.clone());
    (rows, (0, total), host)
}

/// `done` of `total` as a fraction 0..1.
fn fraction(done: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    // Ratios of model sizes; precision is plenty.
    #[allow(clippy::cast_precision_loss)]
    let f = (done as f64 / total as f64).clamp(0.0, 1.0) as f32;
    f
}

/// What a button in the dialog or the overlay asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Download,
    CancelAsk,
    CancelDownload,
    Retry,
    TurnOff,
}

impl FlighthookApp {
    /// Send `call` (recorded instead in tests).
    fn send_models(&mut self, ctx: &egui::Context, call: ModelsCall) {
        if call == ModelsCall::State {
            self.body_models.in_flight = true;
        }
        #[cfg(not(test))]
        crate::net::body_models(ctx, &self.pending, call);
        #[cfg(test)]
        {
            let _ = ctx;
            self.body_models.sent.push(call);
        }
    }

    /// Save body tracking's mode now (`scope=body_tracking`, through the
    /// tuning saves), and show it on the card.
    pub(crate) fn save_body_tracking_mode(&mut self, mode: BodyTrackingMode) {
        self.tune(|s| s.mode = mode);
        self.settings.body_tracking.mode = mode;
    }

    /// The card's Off → `mode`: ask where the model files are before
    /// anything is saved.
    pub(crate) fn turn_body_tracking_on(&mut self, ctx: &egui::Context, mode: BodyTrackingMode) {
        if self.body_models.is_open() {
            return;
        }
        self.body_models.shown = Some(Shown::Checking {
            turn_on: Some(mode),
        });
        self.send_models(ctx, ModelsCall::State);
    }

    /// Whether something on screen needs the models now: a shot's cameras
    /// on the Cameras pane, Live in Live mode, or Replay.
    fn models_needed(&self) -> bool {
        let mode = self.body_tracking().mode;
        if !mode.is_on() {
            return false;
        }
        let pane = if self.live_selected {
            mode == BodyTrackingMode::Live
        } else {
            self.shown_index().is_some()
        };
        self.replay.is_some() || (self.cameras_pane_shown() && pane)
    }

    /// Each update: the first-need check, the overlay's polls and the mode
    /// saves.
    pub(crate) fn drive_body_models(&mut self, ctx: &egui::Context) {
        self.send_tuning(ctx);
        let body = self.body_tracking();
        if !body.mode.is_on() {
            self.body_models.checked = None;
        } else if self.settings.loaded
            && !self.body_models.is_open()
            && self.body_models.checked != Some(body.model)
            && self.models_needed()
        {
            self.body_models.checked = Some(body.model);
            self.body_models.shown = Some(Shown::Checking { turn_on: None });
            self.send_models(ctx, ModelsCall::State);
        }
        let now = ctx.input(|i| i.time);
        let polling = matches!(
            &self.body_models.shown,
            Some(Shown::Progress { models, started: true })
                if models.state == ModelFilesState::Downloading
        );
        if polling {
            if !self.body_models.in_flight && now >= self.body_models.next_poll {
                self.body_models.next_poll = now + POLL_S;
                self.send_models(ctx, ModelsCall::State);
            }
            // Keep polling while nothing else repaints.
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(POLL_S));
        }
    }

    /// The models are there: the overlay closes, and the clips on screen
    /// are asked to be tracked again (their first ask found no models).
    fn models_ready(&mut self) {
        self.body_models.shown = None;
        self.body_models.checked = Some(self.body_tracking().model);
        if let Some(view) = self.replay.as_mut() {
            view.ask_again();
        }
        if let Some(view) = self.shot_player.as_mut() {
            view.ask_again();
        }
    }

    /// A reply about the model files.
    pub(crate) fn body_models_reply(
        &mut self,
        call: ModelsCall,
        reply: Result<BodyTrackingModels, String>,
    ) {
        if call == ModelsCall::State {
            self.body_models.in_flight = false;
        }
        let Some(shown) = self.body_models.shown.clone() else {
            return;
        };
        match (shown, call) {
            (Shown::Checking { turn_on }, ModelsCall::State) => match reply {
                Ok(models) => match models.state {
                    ModelFilesState::Ready => {
                        if let Some(mode) = turn_on {
                            self.save_body_tracking_mode(mode);
                        }
                        self.body_models.checked = Some(models.model);
                        self.body_models.shown = None;
                    }
                    ModelFilesState::Downloading => {
                        if let Some(mode) = turn_on {
                            self.save_body_tracking_mode(mode);
                        }
                        self.body_models.shown = Some(Shown::Progress {
                            models,
                            started: true,
                        });
                    }
                    ModelFilesState::Missing | ModelFilesState::Failed => {
                        self.body_models.shown = Some(Shown::Ask { turn_on, models });
                    }
                },
                // No answer: nothing is saved or downloaded.
                Err(_) => self.body_models.shown = None,
            },
            (Shown::Progress { models, .. }, ModelsCall::Download) => {
                let models = match reply {
                    Ok(models) => models,
                    Err(e) => BodyTrackingModels {
                        state: ModelFilesState::Failed,
                        error: Some(e),
                        download: None,
                        ..models
                    },
                };
                if models.state == ModelFilesState::Ready {
                    self.models_ready();
                } else {
                    self.body_models.shown = Some(Shown::Progress {
                        models,
                        started: true,
                    });
                }
            }
            (
                Shown::Progress {
                    started: true,
                    models: before,
                },
                ModelsCall::State,
            ) if before.state == ModelFilesState::Downloading => {
                // A missed poll: the next one tries again.
                if let Ok(models) = reply {
                    match models.state {
                        ModelFilesState::Ready => self.models_ready(),
                        // Cancelled from elsewhere.
                        ModelFilesState::Missing => self.body_models.shown = None,
                        ModelFilesState::Downloading | ModelFilesState::Failed => {
                            self.body_models.shown = Some(Shown::Progress {
                                models,
                                started: true,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Apply a button of the dialog or the overlay.
    fn models_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::CancelAsk => self.body_models.shown = None,
            Action::Download | Action::Retry => {
                let models = match self.body_models.shown.take() {
                    Some(Shown::Ask { turn_on, models }) => {
                        if let Some(mode) = turn_on {
                            self.save_body_tracking_mode(mode);
                        }
                        models
                    }
                    Some(Shown::Progress { models, .. }) => models,
                    other => {
                        self.body_models.shown = other;
                        return;
                    }
                };
                self.body_models.shown = Some(Shown::Progress {
                    models: BodyTrackingModels {
                        state: ModelFilesState::Downloading,
                        download: None,
                        error: None,
                        ..models
                    },
                    started: false,
                });
                self.send_models(ctx, ModelsCall::Download);
            }
            Action::CancelDownload => {
                self.send_models(ctx, ModelsCall::Cancel);
                self.save_body_tracking_mode(BodyTrackingMode::Off);
                self.body_models.shown = None;
            }
            Action::TurnOff => {
                self.save_body_tracking_mode(BodyTrackingMode::Off);
                self.body_models.shown = None;
            }
        }
    }

    /// The dialog or the overlay, when one is up.
    pub(crate) fn render_body_models(&mut self, ctx: &egui::Context) {
        let Some(shown) = self.body_models.shown.clone() else {
            return;
        };
        // Each view is a modal of its own, sized to its contents.
        let view = match &shown {
            Shown::Checking { .. } => return,
            Shown::Ask { .. } => "ask",
            Shown::Progress { models, .. } if models.state == ModelFilesState::Failed => "failed",
            Shown::Progress { .. } => "progress",
        };
        let modal = egui::Modal::new(egui::Id::new(("body_tracking_models", view)))
            .frame(theme::card().inner_margin(egui::Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                match &shown {
                    Shown::Checking { .. } => None,
                    Shown::Ask { models, .. } => ask(ui, models),
                    Shown::Progress { models, .. } if models.state == ModelFilesState::Failed => {
                        failed(ui, models)
                    }
                    Shown::Progress { models, .. } => progress(ui, models),
                }
            });
        let mut action = modal.inner;
        if action.is_none() && modal.should_close() && matches!(shown, Shown::Ask { .. }) {
            action = Some(Action::CancelAsk);
        }
        if let Some(action) = action {
            self.models_action(ctx, action);
        }
    }
}

/// The modal's title line.
fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(17.0)
            .strong()
            .color(theme::TEXT),
    );
    ui.add_space(10.0);
}

/// Plain text in the modal, wrapped.
fn text(ui: &mut egui::Ui, text: &str, colour: egui::Color32) {
    ui.add(egui::Label::new(egui::RichText::new(text).size(14.0).color(colour)).wrap());
}

/// The dialog: what would be downloaded, from where, where it is kept.
fn ask(ui: &mut egui::Ui, models: &BodyTrackingModels) -> Option<Action> {
    let (opening, files, kept) = ask_lines(models);
    title(ui, "Download body tracking models?");
    text(ui, &opening, theme::TEXT);
    ui.add_space(6.0);
    for file in &files {
        text(ui, &format!("•  {file}"), theme::TEXT);
    }
    ui.add_space(6.0);
    text(ui, &kept, theme::TEXT_MUTED);
    text(
        ui,
        "After that, body tracking runs entirely on this computer.",
        theme::TEXT_MUTED,
    );
    ui.add_space(18.0);
    let shown = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let download = ui.add(theme::primary_button("Download", 15.0));
        let cancel = ui.button("Cancel");
        (download, cancel)
    });
    let (download, cancel) = shown.inner;
    devices::mark(ui, || "models:dialog".into(), &shown.response);
    devices::mark(ui, || "models:download".into(), &download);
    devices::mark(ui, || "models:cancel".into(), &cancel);
    if download.clicked() {
        Some(Action::Download)
    } else if cancel.clicked() {
        Some(Action::CancelAsk)
    } else {
        None
    }
}

/// One progress row: label and MB done/total over a bar.
fn bar(ui: &mut egui::Ui, label: &str, done: u64, total: u64, key: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(14.0).color(theme::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{} / {}", mb(done), mb(total)))
                    .size(13.0)
                    .color(theme::TEXT_MUTED),
            );
        });
    });
    let bar = ui.add(
        egui::ProgressBar::new(fraction(done, total))
            .desired_width(WIDTH)
            .desired_height(8.0)
            .fill(theme::ACCENT),
    );
    devices::mark(ui, || format!("models:bar:{key}"), &bar);
    ui.add_space(8.0);
}

/// The overlay while downloading: per file, overall, the host, Cancel.
fn progress(ui: &mut egui::Ui, models: &BodyTrackingModels) -> Option<Action> {
    let (rows, (done, total), host) = progress_rows(models);
    title(ui, "Downloading body tracking models");
    for (i, (label, done, total)) in rows.iter().enumerate() {
        bar(ui, label, *done, *total, &i.to_string());
    }
    if rows.len() > 1 {
        bar(ui, "Overall", done, total, "overall");
    }
    if let Some(host) = host {
        text(ui, &format!("From {host}"), theme::TEXT_MUTED);
    }
    ui.add_space(14.0);
    let shown = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.button("Cancel")
    });
    devices::mark(ui, || "models:progress".into(), &shown.response);
    devices::mark(ui, || "models:stop".into(), &shown.inner);
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs_f64(POLL_S));
    shown.inner.clicked().then_some(Action::CancelDownload)
}

/// The overlay after a failure: why, a likely cause, Retry and Turn off.
fn failed(ui: &mut egui::Ui, models: &BodyTrackingModels) -> Option<Action> {
    title(ui, "The download failed");
    text(
        ui,
        models.error.as_deref().unwrap_or("No reason was given."),
        theme::TEXT,
    );
    ui.add_space(6.0);
    text(
        ui,
        "A firewall or proxy may be blocking it.",
        theme::TEXT_MUTED,
    );
    ui.add_space(18.0);
    let shown = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let retry = ui.add(theme::primary_button("Retry", 15.0));
        let off = ui.button("Turn off");
        (retry, off)
    });
    let (retry, off) = shown.inner;
    devices::mark(ui, || "models:failed".into(), &shown.response);
    devices::mark(ui, || "models:retry".into(), &retry);
    devices::mark(ui, || "models:turn_off".into(), &off);
    if retry.clicked() {
        Some(Action::Retry)
    } else if off.clicked() {
        Some(Action::TurnOff)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use egui::{Event, Modifiers, PointerButton, Pos2, Rect, vec2};
    use flighthook::{FlighthookConfig, ModelDownload, ModelFile, ModelFileProgress};

    use super::*;
    use crate::net;
    use crate::panels::devices::Marks;

    fn file(name: &str, label: &str, host: &str, size: u64, present: bool) -> ModelFile {
        ModelFile {
            name: name.into(),
            label: label.into(),
            host: host.into(),
            size,
            present,
        }
    }

    /// The server's answer for RTMPose-m with `present` files.
    fn models(state: ModelFilesState, present: [bool; 2]) -> BodyTrackingModels {
        BodyTrackingModels {
            state,
            model: PoseModel::RtmposeM,
            dir: "/home/golfer/.local/share/flighthook/models".into(),
            files: vec![
                file(
                    "rtmpose-m-halpe26.onnx",
                    "RTMPose-m pose model",
                    "download.openmmlab.com",
                    55_685_444,
                    present[0],
                ),
                file(
                    "yolox-tiny.onnx",
                    "YOLOX-Tiny person detector",
                    "github.com",
                    20_219_662,
                    present[1],
                ),
            ],
            download: None,
            error: None,
        }
    }

    /// Mid-download: the pose model half done.
    fn downloading() -> BodyTrackingModels {
        let mut m = models(ModelFilesState::Downloading, [false, false]);
        m.download = Some(ModelDownload {
            files: vec![
                ModelFileProgress {
                    name: "rtmpose-m-halpe26.onnx".into(),
                    label: "RTMPose-m pose model".into(),
                    host: "download.openmmlab.com".into(),
                    done: 27_000_000,
                    total: 55_000_000,
                },
                ModelFileProgress {
                    name: "yolox-tiny.onnx".into(),
                    label: "YOLOX-Tiny person detector".into(),
                    host: "github.com".into(),
                    done: 0,
                    total: 20_219_662,
                },
            ],
            current: 0,
            done: 27_000_000,
            total: 75_219_662,
        });
        m
    }

    /// An app with settings loaded, body tracking in `mode`.
    fn app(mode: BodyTrackingMode) -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        let mut config = FlighthookConfig::default();
        config.body_tracking.mode = mode;
        app.settings.load_from(&config);
        app
    }

    /// Draws the Devices tab and the prompt.
    struct Screen(egui::Context);

    impl Screen {
        fn new() -> Self {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            Self(ctx)
        }

        fn frame(&self, app: &mut FlighthookApp, events: Vec<Event>) -> Marks {
            self.0.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1600.0))),
                events,
                ..Default::default()
            };
            let _ = self.0.run(input, |ctx| {
                app.drive_body_models(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    let ctx = ui.ctx().clone();
                    app.render_devices_panel(&ctx, ui);
                });
                app.render_body_models(ctx);
            });
            self.0
                .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
                .unwrap_or_default()
        }

        /// Two passes: a modal shown for the first time is laid out on the
        /// first one (its sizing pass) and in place on the second.
        fn settled(&self, app: &mut FlighthookApp) -> Marks {
            self.frame(app, Vec::new());
            self.frame(app, Vec::new())
        }

        fn click(&self, app: &mut FlighthookApp, marks: &Marks, name: &str) -> Marks {
            let at = marks
                .get(name)
                .unwrap_or_else(|| panic!("{name} not drawn"))
                .0
                .center();
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

    /// The tuning saves sent so far (each carries the whole section).
    fn saved_modes(app: &FlighthookApp) -> Vec<BodyTrackingMode> {
        app.tuning.sent.iter().map(|s| s.mode).collect()
    }

    #[test]
    fn the_dialog_names_what_is_downloaded_from_where_and_where_it_is_kept() {
        let (opening, files, kept) = ask_lines(&models(ModelFilesState::Missing, [false, false]));
        assert_eq!(
            opening,
            "Body tracking needs these models, downloaded once (about 76 MB):"
        );
        assert_eq!(
            files,
            [
                "RTMPose-m pose model, 55.7 MB, from download.openmmlab.com",
                "YOLOX-Tiny person detector, 20.2 MB, from github.com",
            ]
        );
        assert_eq!(
            kept,
            "They are kept in /home/golfer/.local/share/flighthook/models."
        );
        // Only what is missing.
        let (opening, files, _) = ask_lines(&models(ModelFilesState::Missing, [true, false]));
        assert_eq!(
            opening,
            "Body tracking needs this model, downloaded once (about 20 MB):"
        );
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn the_overlay_shows_each_file_the_whole_download_and_the_host() {
        let (rows, overall, host) = progress_rows(&downloading());
        assert_eq!(
            rows,
            [
                ("RTMPose-m pose model".to_owned(), 27_000_000, 55_000_000),
                ("YOLOX-Tiny person detector".to_owned(), 0, 20_219_662),
            ]
        );
        assert_eq!(overall, (27_000_000, 75_219_662));
        assert_eq!(host.as_deref(), Some("download.openmmlab.com"));
        // Before the server answers: the missing files at 0.
        let (rows, overall, host) = progress_rows(&models(ModelFilesState::Missing, [false, true]));
        assert_eq!(rows, [("RTMPose-m pose model".to_owned(), 0, 55_685_444)]);
        assert_eq!(overall, (0, 55_685_444));
        assert_eq!(host.as_deref(), Some("download.openmmlab.com"));
        assert!((fraction(27, 54) - 0.5).abs() < 1e-6);
        assert!(fraction(5, 0).abs() < 1e-6);
        assert!((fraction(9, 4) - 1.0).abs() < 1e-6);
        assert_eq!(mb(55_685_444), "55.7 MB");
    }

    #[test]
    fn ready_models_skip_the_dialog_and_the_mode_is_saved_at_once() {
        let screen = Screen::new();
        let mut app = app(BodyTrackingMode::Off);
        let marks = screen.settled(&mut app);
        screen.click(&mut app, &marks, "body_tracking_mode:historical");
        // Nothing saved yet: the state is asked for first.
        assert_eq!(app.body_models.sent, [ModelsCall::State]);
        assert!(saved_modes(&app).is_empty());
        assert_eq!(app.body_tracking().mode, BodyTrackingMode::Off);
        app.body_models_reply(
            ModelsCall::State,
            Ok(models(ModelFilesState::Ready, [true, true])),
        );
        let marks = screen.settled(&mut app);
        assert!(!marks.contains_key("models:dialog"), "no dialog");
        assert!(!app.body_models.is_open());
        assert_eq!(saved_modes(&app), [BodyTrackingMode::Historical]);
        assert_eq!(app.body_tracking().mode, BodyTrackingMode::Historical);
        assert_eq!(
            app.settings.body_tracking.mode,
            BodyTrackingMode::Historical
        );
        assert!(
            !marks.contains_key("save:body_tracking"),
            "saved, not an edit"
        );
        assert_eq!(app.body_models.sent, [ModelsCall::State], "no download");
    }

    #[test]
    fn missing_models_ask_first_and_cancel_saves_nothing() {
        let screen = Screen::new();
        let mut app = app(BodyTrackingMode::Off);
        let marks = screen.settled(&mut app);
        screen.click(&mut app, &marks, "body_tracking_mode:live");
        app.body_models_reply(
            ModelsCall::State,
            Ok(models(ModelFilesState::Missing, [false, false])),
        );
        let marks = screen.settled(&mut app);
        assert!(marks.contains_key("models:dialog"));
        assert!(marks.contains_key("models:download"));
        let marks = screen.click(&mut app, &marks, "models:cancel");
        assert!(!marks.contains_key("models:dialog"));
        assert!(!app.body_models.is_open());
        assert!(saved_modes(&app).is_empty(), "nothing saved");
        assert_eq!(app.body_tracking().mode, BodyTrackingMode::Off);
        assert_eq!(app.settings.body_tracking.mode, BodyTrackingMode::Off);
        assert!(!marks.contains_key("save:body_tracking"));
        assert_eq!(app.body_models.sent, [ModelsCall::State], "no download");
        // A failed last download asks again the same way.
        screen.click(&mut app, &marks, "body_tracking_mode:live");
        let mut failed = models(ModelFilesState::Failed, [false, false]);
        failed.error = Some("timed out".into());
        app.body_models_reply(ModelsCall::State, Ok(failed));
        assert!(
            screen
                .frame(&mut app, Vec::new())
                .contains_key("models:dialog")
        );
    }

    #[test]
    fn download_saves_the_mode_and_the_overlay_follows_it_to_the_end() {
        let screen = Screen::new();
        let mut app = app(BodyTrackingMode::Off);
        let marks = screen.settled(&mut app);
        screen.click(&mut app, &marks, "body_tracking_mode:historical");
        app.body_models_reply(
            ModelsCall::State,
            Ok(models(ModelFilesState::Missing, [false, false])),
        );
        let marks = screen.settled(&mut app);
        let marks = screen.click(&mut app, &marks, "models:download");
        assert_eq!(saved_modes(&app), [BodyTrackingMode::Historical]);
        assert_eq!(
            app.body_models.sent,
            [ModelsCall::State, ModelsCall::Download]
        );
        // The overlay replaces the dialog at once.
        assert!(!marks.contains_key("models:dialog"));
        assert!(marks.contains_key("models:progress"));
        assert!(marks.contains_key("models:bar:0") && marks.contains_key("models:bar:overall"));
        // No poll before the server has answered.
        app.body_models.sent.clear();
        screen.frame(&mut app, Vec::new());
        assert!(app.body_models.sent.is_empty());
        app.body_models_reply(ModelsCall::Download, Ok(downloading()));
        screen.frame(&mut app, Vec::new());
        assert_eq!(app.body_models.sent, [ModelsCall::State], "polling");
        // One poll at a time.
        screen.frame(&mut app, Vec::new());
        assert_eq!(app.body_models.sent.len(), 1);
        app.body_models_reply(ModelsCall::State, Ok(downloading()));
        assert!(
            screen
                .frame(&mut app, Vec::new())
                .contains_key("models:bar:1")
        );
        // Done: the overlay closes by itself.
        app.body_models_reply(
            ModelsCall::State,
            Ok(models(ModelFilesState::Ready, [true, true])),
        );
        let marks = screen.settled(&mut app);
        assert!(!marks.contains_key("models:progress"));
        assert!(!app.body_models.is_open());
        assert_eq!(app.body_tracking().mode, BodyTrackingMode::Historical);
    }

    #[test]
    fn cancel_stops_the_download_and_turns_body_tracking_off() {
        let screen = Screen::new();
        let mut app = app(BodyTrackingMode::Off);
        let marks = screen.settled(&mut app);
        screen.click(&mut app, &marks, "body_tracking_mode:live");
        app.body_models_reply(
            ModelsCall::State,
            Ok(models(ModelFilesState::Missing, [false, false])),
        );
        let marks = screen.settled(&mut app);
        screen.click(&mut app, &marks, "models:download");
        app.body_models_reply(ModelsCall::Download, Ok(downloading()));
        let marks = screen.settled(&mut app);
        let marks = screen.click(&mut app, &marks, "models:stop");
        assert!(!marks.contains_key("models:progress"));
        assert!(app.body_models.sent.contains(&ModelsCall::Cancel));
        assert_eq!(app.body_tracking().mode, BodyTrackingMode::Off);
        assert_eq!(app.settings.body_tracking.mode, BodyTrackingMode::Off);
        // Live was saved for the download, then Off: the newest goes out
        // once the first save is answered and the settings read again.
        assert_eq!(saved_modes(&app), [BodyTrackingMode::Live]);
        app.tuning.reply(Ok(()));
        // Before the settings are read again they still say Off: Off is
        // kept, not taken as saved.
        screen.frame(&mut app, Vec::new());
        assert_eq!(app.body_tracking().mode, BodyTrackingMode::Off);
        let mut config = FlighthookConfig::default();
        config.body_tracking.mode = BodyTrackingMode::Live;
        app.settings.load_from(&config);
        screen.frame(&mut app, Vec::new());
        assert_eq!(
            saved_modes(&app),
            [BodyTrackingMode::Live, BodyTrackingMode::Off]
        );
        app.tuning.reply(Ok(()));
        app.settings.load_from(&FlighthookConfig::default());
        screen.frame(&mut app, Vec::new());
        assert_eq!(app.body_tracking().mode, BodyTrackingMode::Off);
        assert_eq!(saved_modes(&app).len(), 2, "settled");
    }

    #[test]
    fn a_failed_download_offers_retry_and_turn_off() {
        let screen = Screen::new();
        let mut app = app(BodyTrackingMode::Off);
        let marks = screen.settled(&mut app);
        screen.click(&mut app, &marks, "body_tracking_mode:historical");
        app.body_models_reply(
            ModelsCall::State,
            Ok(models(ModelFilesState::Missing, [false, false])),
        );
        let marks = screen.settled(&mut app);
        screen.click(&mut app, &marks, "models:download");
        app.body_models_reply(ModelsCall::Download, Ok(downloading()));
        let mut failed = models(ModelFilesState::Failed, [false, false]);
        failed.error = Some("downloading `https://download.openmmlab.com/...`: timed out".into());
        app.body_models_reply(ModelsCall::State, Ok(failed));
        let marks = screen.settled(&mut app);
        assert!(marks.contains_key("models:failed"));
        assert!(!marks.contains_key("models:progress"));
        // No more polls while it shows the failure.
        app.body_models.sent.clear();
        screen.frame(&mut app, Vec::new());
        assert!(app.body_models.sent.is_empty());
        // Retry: the download again, the overlay back.
        let marks = screen.click(&mut app, &marks, "models:retry");
        assert_eq!(app.body_models.sent, [ModelsCall::Download]);
        assert!(marks.contains_key("models:progress"));
        // A server that can't be reached fails the same way.
        app.body_models_reply(ModelsCall::Download, Err("connection refused".into()));
        let marks = screen.settled(&mut app);
        assert!(marks.contains_key("models:failed"));
        // Turn off: closed, saved Off.
        let marks = screen.click(&mut app, &marks, "models:turn_off");
        assert!(!marks.contains_key("models:failed"));
        assert!(!app.body_models.is_open());
        assert_eq!(app.body_tracking().mode, BodyTrackingMode::Off);
    }

    #[test]
    fn already_on_without_models_asks_the_first_time_something_needs_them() {
        let screen = Screen::new();
        let mut app = app(BodyTrackingMode::Historical);
        // The Devices tab needs none.
        screen.frame(&mut app, Vec::new());
        assert!(app.body_models.sent.is_empty());
        // Replay open: asked once.
        let row = crate::panels::replay::tests::shot(&["camera.0"]);
        app.replay = Some(crate::panels::replay::ReplayView::new(
            1,
            &row,
            str::to_owned,
            false,
            0.2,
        ));
        screen.frame(&mut app, Vec::new());
        assert_eq!(app.body_models.sent, [ModelsCall::State]);
        app.body_models_reply(
            ModelsCall::State,
            Ok(models(ModelFilesState::Missing, [false, false])),
        );
        let marks = screen.settled(&mut app);
        assert!(marks.contains_key("models:dialog"));
        // Cancel: the mode stays as it is, and it is not asked again.
        screen.click(&mut app, &marks, "models:cancel");
        assert_eq!(app.body_tracking().mode, BodyTrackingMode::Historical);
        assert!(saved_modes(&app).is_empty());
        for _ in 0..3 {
            screen.frame(&mut app, Vec::new());
        }
        assert_eq!(app.body_models.sent, [ModelsCall::State]);
        // Download from there: no mode save, the overlay, and once done the
        // clips on screen are asked to be tracked again.
        app.body_models.checked = None;
        screen.frame(&mut app, Vec::new());
        app.body_models_reply(
            ModelsCall::State,
            Ok(models(ModelFilesState::Missing, [false, false])),
        );
        let marks = screen.settled(&mut app);
        screen.click(&mut app, &marks, "models:download");
        assert!(saved_modes(&app).is_empty());
        let view = app.replay.as_mut().expect("replay");
        view.step(BodyTrackingMode::Historical);
        assert!(view.asked_to_track("camera.0"));
        app.body_models_reply(
            ModelsCall::Download,
            Ok(models(ModelFilesState::Ready, [true, true])),
        );
        assert!(!app.body_models.is_open());
        assert!(!app.replay.as_ref().unwrap().asked_to_track("camera.0"));
    }

    #[test]
    fn live_needs_the_models_only_in_live_mode() {
        let mut app = app(BodyTrackingMode::Historical);
        assert!(!app.models_needed());
        app.replay = Some(crate::panels::replay::ReplayView::new(
            1,
            &crate::panels::replay::tests::shot(&[]),
            str::to_owned,
            false,
            0.2,
        ));
        assert!(app.models_needed());
        app.save_body_tracking_mode(BodyTrackingMode::Off);
        assert!(!app.models_needed(), "off needs none");
    }
}
