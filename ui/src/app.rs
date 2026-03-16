//! egui application — FlighthookApp.

use std::collections::HashMap;

use crate::net::{self, PendingHandle, WsPollEvent};
use crate::panels::Tab;
use crate::panels::settings::{PendingRemoval, SettingsForm};
use crate::types::{
    ActorStatus, ActorStatusResponse, FlighthookEvent, FlighthookMessage, GsProSection, LogEntry,
    MevoSection, R10Section, ShotRow, UnitSystem,
};
use chrono::{SecondsFormat, Utc};

const API_DOCS_MD: &str = include_str!("../../docs/API.md");

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

fn new_actor(name: String) -> ActorStatusResponse {
    ActorStatusResponse {
        name,
        status: ActorStatus::Disconnected,
        telemetry: HashMap::new(),
    }
}

pub struct FlighthookApp {
    // Unified actor state
    pub(crate) actors: HashMap<String, ActorStatusResponse>,

    // Data
    pub(crate) shots: Vec<ShotRow>,

    // Global state
    pub(crate) current_mode: String,

    // Settings
    pub(crate) settings: SettingsForm,

    // Networking
    pub(crate) pending: PendingHandle,
    pub(crate) ws_sender: Option<ewebsock::WsSender>,
    pub(crate) ws_receiver: Option<ewebsock::WsReceiver>,

    // UI state
    pub(crate) ws_connected: bool,
    pub(crate) ws_ever_connected: bool,
    pub(crate) active_tab: Tab,
    pub(crate) confirm_remove: Option<PendingRemoval>,
    pub(crate) show_api_docs: bool,
    pub(crate) api_docs_cache: egui_commonmark::CommonMarkCache,

    // Unit display
    pub(crate) units_toggle: UnitSystem,

    // Deferred refresh
    pub(crate) needs_status_refresh: bool,

    // Log state
    pub(crate) log_entries: Vec<LogEntry>,
    pub(crate) log_auto_scroll: bool,
    pub(crate) log_type_filters: HashMap<String, bool>,
    pub(crate) log_filter_open: bool,

    // Setup wizard
    pub(crate) wizard_checked: bool,
    pub(crate) wizard_dismissed: bool,
    pub(crate) wizard_mevo: bool,
    pub(crate) wizard_r10: bool,
    pub(crate) wizard_gspro: bool,
    pub(crate) wizard_saving: bool,
}

impl FlighthookApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let pending = net::new_pending();

        // Fire initial REST fetches (including settings for wizard detection)
        net::fetch_status(&cc.egui_ctx, &pending);
        net::fetch_shots(&cc.egui_ctx, &pending);
        net::fetch_settings(&cc.egui_ctx, &pending);

        // Defer WebSocket connection to first update() — gives the page
        // time to stabilise on iOS Safari before opening a second connection.

        Self {
            actors: HashMap::new(),
            shots: Vec::new(),
            current_mode: "full".into(),
            settings: SettingsForm::default(),
            pending,
            ws_sender: None,
            ws_receiver: None,
            ws_connected: false,
            ws_ever_connected: false,
            active_tab: Tab::Telemetry,
            confirm_remove: None,
            units_toggle: UnitSystem::default(),
            needs_status_refresh: false,
            show_api_docs: false,
            api_docs_cache: egui_commonmark::CommonMarkCache::default(),
            log_entries: Vec::new(),
            log_auto_scroll: true,
            log_type_filters: crate::panels::log::MESSAGE_TYPES
                .iter()
                .map(|&k| (k.to_string(), true))
                .collect(),
            log_filter_open: false,
            wizard_checked: false,
            wizard_dismissed: false,
            wizard_mevo: false,
            wizard_r10: false,
            wizard_gspro: false,
            wizard_saving: false,
        }
    }

    /// Drain pending REST results.
    fn apply_pending(&mut self) {
        let Ok(mut p) = self.pending.try_lock() else {
            return;
        };

        if let Some(status) = p.status.take() {
            if let Some(mode) = status.mode {
                self.current_mode = mode.to_string();
            }
            for (id, a) in status.actors {
                self.actors.insert(id, a);
            }
        }

        if let Some(shots) = p.shots.take() {
            self.shots = shots.into_iter().map(ShotRow::from).collect();
        }

        if let Some(s) = p.settings.take()
            && !self.settings.dirty
        {
            let first_load = !self.settings.loaded;
            // Check if we need to show the setup wizard (no user actors)
            if !self.wizard_checked {
                self.wizard_checked = true;
                self.wizard_dismissed = s.has_user_actors();
            }
            self.settings.load_from(&s);
            if first_load {
                self.units_toggle = s.default_units;
            }
        }

        if let Some(_resp) = p.settings_save.take() {
            // Dismiss wizard on successful save and reload settings
            if self.wizard_saving {
                self.wizard_saving = false;
                self.wizard_dismissed = true;
                self.needs_status_refresh = true;
                self.settings.loaded = false;
            }
            self.settings.saving = false;

            // Clear dirty flag only for the saved section
            let target = self.settings.save_target.take();
            match &target {
                Some(crate::panels::settings::SaveTarget::Global) => {
                    self.settings.global_dirty = false;
                }
                Some(crate::panels::settings::SaveTarget::Actor(idx)) => {
                    if let Some(actor) = self.settings.actors.get_mut(*idx) {
                        actor.set_dirty(false);
                    }
                }
                Some(crate::panels::settings::SaveTarget::Full) | None => {
                    self.settings.global_dirty = false;
                    for actor in &mut self.settings.actors {
                        actor.set_dirty(false);
                    }
                }
            }

            // Update original config to reflect saved values
            if let Some(ref t) = target {
                self.settings.update_original_after_save(t);
            }

            // Recalculate global dirty
            self.settings.dirty =
                self.settings.global_dirty || self.settings.actors.iter().any(|a| a.dirty());
        }
    }

    /// Poll WebSocket for unified bus events.
    fn poll_ws(&mut self) {
        let Some(rx) = &mut self.ws_receiver else {
            return;
        };
        let (events, _) = net::poll_ws(rx);
        let mut ws_disconnected = false;
        for event in events {
            match event {
                WsPollEvent::Opened => {
                    // Browser WebSocket actually opened — send the init handshake now.
                    if let Some(tx) = &mut self.ws_sender {
                        net::send_ws_start(tx);
                    }
                }
                WsPollEvent::Init { .. } => {}
                WsPollEvent::Message(msg) => self.handle_bus_event(msg),
                WsPollEvent::Error(_) => {
                    ws_disconnected = true;
                }
                WsPollEvent::Disconnected => {
                    self.actors.clear();
                    ws_disconnected = true;
                }
            }
        }
        if ws_disconnected {
            self.ws_sender = None;
            self.ws_receiver = None;
            self.ws_connected = false;
        }
    }

    fn handle_bus_event(&mut self, msg: FlighthookMessage) {
        // Buffer all events for the log panel
        {
            use crate::panels::log::{alert_severity, event_debug, message_type};

            let actor_name = self
                .actors
                .get(&msg.actor)
                .map(|a| a.name.as_str())
                .unwrap_or("")
                .to_string();
            let raw = msg
                .raw_payload
                .as_ref()
                .map(|r| r.to_string())
                .unwrap_or_else(|| {
                    serde_json::to_string(&msg.event).unwrap_or_default()
                });
            self.log_entries.push(LogEntry {
                timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                actor_name,
                actor_id: msg.actor.clone(),
                message_type: message_type(&msg.event).to_string(),
                event_debug: event_debug(&msg.event),
                raw,
                alert_severity: alert_severity(&msg.event),
            });
            const MAX_LOG_ENTRIES: usize = 500;
            if self.log_entries.len() > MAX_LOG_ENTRIES {
                self.log_entries
                    .drain(..self.log_entries.len() - MAX_LOG_ENTRIES);
            }
        }

        // Route event to UI state
        let actor = msg.actor.clone();
        match msg.event {
            FlighthookEvent::ActorStatus {
                status,
                telemetry,
            } => {
                let actor = self
                    .actors
                    .entry(actor)
                    .or_insert_with(|| new_actor(String::new()));
                actor.status = status;
                for (k, v) in telemetry {
                    actor.telemetry.insert(k, v);
                }
            }
            FlighthookEvent::DeviceTelemetry {
                telemetry: Some(tel),
                ..
            } => {
                if let Some(actor) = self.actors.get_mut(&actor) {
                    for (k, v) in tel {
                        actor.telemetry.insert(k, v);
                    }
                }
            }
            FlighthookEvent::ShotTrigger { key } => {
                self.shots.push(ShotRow {
                    actor: actor.clone(),
                    shot_id: key.shot_id.clone(),
                    shot_number: key.shot_number,
                    ball: None,
                    club: None,
                });
            }
            FlighthookEvent::BallFlight {
                key,
                ball,
            } => {
                if let Some(row) = self.shots.iter_mut().rev().find(|r| r.shot_id == key.shot_id) {
                    row.ball = Some(*ball);
                }
            }
            FlighthookEvent::ClubPath { key, club } => {
                if let Some(row) = self.shots.iter_mut().rev().find(|r| r.shot_id == key.shot_id) {
                    row.club = Some(*club);
                }
            }
            FlighthookEvent::ShotFinished { .. } => {}
            FlighthookEvent::PlayerInfo { player_info } => {
                if let Some(ref name) = player_info.name
                    && let Some(actor) = self.actors.get_mut(&actor)
                {
                    actor.telemetry.insert("name".into(), name.clone());
                }
            }
            FlighthookEvent::ClubInfo { club_info } => {
                if let Some(actor) = self.actors.get_mut(&actor) {
                    actor
                        .telemetry
                        .insert("club".into(), club_info.club.to_string());
                }
            }
            FlighthookEvent::SetDetectionMode { mode: Some(m), .. } => {
                self.current_mode = m.to_string();
            }
            FlighthookEvent::ConfigOutcome {
                ref started,
                ref restarted,
                ..
            } => {
                // New or restarted actors won't have names in the UI-side
                // actor map (WS events don't carry names). Re-fetch from
                // REST so the telemetry panel picks up display names.
                if !started.is_empty() || !restarted.is_empty() {
                    self.needs_status_refresh = true;
                }
            }
            _ => {}
        }
    }
}

impl FlighthookApp {
    fn show_wizard(&self) -> bool {
        self.settings.loaded && !self.wizard_dismissed && self.wizard_checked
    }

    fn render_wizard(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::from_rgb(25, 25, 30)))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.2);

                    ui.label(
                        egui::RichText::new("FLIGHTHOOK")
                            .strong()
                            .size(28.0)
                            .color(egui::Color32::from_rgb(180, 200, 255)),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("What do you have?")
                            .size(16.0)
                            .color(egui::Color32::from_rgb(160, 160, 160)),
                    );
                    ui.add_space(24.0);

                    // Left-aligned checkbox group
                    let checkbox_width = 260.0;
                    ui.allocate_ui_with_layout(
                        egui::vec2(checkbox_width, 0.0),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            ui.label(
                                egui::RichText::new("Launch Monitors")
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(120, 120, 120)),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut self.wizard_mevo,
                                egui::RichText::new("FlightScope Mevo / Mevo+").size(15.0),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut self.wizard_r10,
                                egui::RichText::new("Garmin R10").size(15.0),
                            );
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new("Simulators")
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(120, 120, 120)),
                            );
                            ui.add_space(4.0);
                            ui.checkbox(
                                &mut self.wizard_gspro,
                                egui::RichText::new("GSPro").size(15.0),
                            );
                        },
                    );

                    ui.add_space(24.0);

                    let any_selected =
                        self.wizard_mevo || self.wizard_r10 || self.wizard_gspro;
                    let btn = egui::Button::new(
                        egui::RichText::new("Get Started").size(15.0).strong(),
                    )
                    .min_size(egui::vec2(140.0, 36.0));

                    if ui.add_enabled(any_selected && !self.wizard_saving, btn).clicked() {
                        self.apply_wizard(ctx);
                    }

                    if !any_selected {
                        ui.add_space(8.0);
                        if ui
                            .link(egui::RichText::new("Skip — configure manually").size(12.0))
                            .clicked()
                        {
                            self.wizard_dismissed = true;
                            self.active_tab = Tab::Settings;
                        }
                    }
                });
            });
    }

    fn apply_wizard(&mut self, ctx: &egui::Context) {
        let mut config = self.settings.to_request();

        if self.wizard_mevo {
            config.mevo.insert("0".into(), MevoSection::default());
        }
        if self.wizard_r10 {
            config.r10.insert("0".into(), R10Section::default());
        }
        if self.wizard_gspro {
            config.gspro.insert("0".into(), GsProSection::default());
        }

        self.wizard_saving = true;
        self.settings.saving = true;
        self.settings.save_target = Some(crate::panels::settings::SaveTarget::Full);
        net::post_settings(ctx, &self.pending, &config, None);
    }
}

impl eframe::App for FlighthookApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll for WebSocket events ~10x/sec instead of only on user input.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));

        // Connect WebSocket on first frame only — no auto-reconnect.
        // User should refresh the page to pick up new WASM code after a deploy.
        if self.ws_sender.is_none()
            && !self.ws_ever_connected
            && let Some((tx, rx)) = net::connect_ws()
        {
            self.ws_sender = Some(tx);
            self.ws_receiver = Some(rx);
            self.ws_connected = true;
            self.ws_ever_connected = true;
            net::fetch_status(ctx, &self.pending);
            net::fetch_shots(ctx, &self.pending);
        }

        self.apply_pending();
        self.poll_ws();

        if self.needs_status_refresh {
            self.needs_status_refresh = false;
            net::fetch_status(ctx, &self.pending);
        }

        // Confirmation dialog for item removal
        if self.confirm_remove.is_some() {
            let name = self.confirm_remove.as_ref().unwrap().1.clone();
            let mut keep_open = true;
            egui::Window::new("Confirm removal")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Remove \"{name}\"?"));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() {
                            let PendingRemoval(idx, _) = self.confirm_remove.take().unwrap();
                            self.settings.actors.remove(idx);
                            // Auto-save after removal (full config since an actor was removed)
                            self.settings.saving = true;
                            self.settings.save_target =
                                Some(crate::panels::settings::SaveTarget::Full);
                            let req = self.settings.to_request();
                            net::post_settings(ctx, &self.pending, &req, None);
                            keep_open = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                        }
                    });
                });
            if !keep_open {
                self.confirm_remove = None;
            }
        }

        // API documentation popup
        if self.show_api_docs {
            let screen = ctx.content_rect();
            let win_size = [screen.width() * 0.9, screen.height() * 0.9];
            egui::Window::new("API Documentation")
                .open(&mut self.show_api_docs)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .fixed_size(win_size)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            egui_commonmark::CommonMarkViewer::new().show(
                                ui,
                                &mut self.api_docs_cache,
                                API_DOCS_MD,
                            );
                        });
                });
        }

        // Setup wizard — shown when config has no user actors
        if self.show_wizard() {
            self.render_wizard(ctx);
            return;
        }

        // Dark overlay when backend WS is gone (no actors = not connected to backend)
        if !self.ws_connected && self.actors.is_empty() {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::from_rgb(20, 20, 20)))
                .show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("DISCONNECTED")
                                .size(32.0)
                                .color(egui::Color32::from_rgb(140, 140, 140))
                                .strong(),
                        );
                    });
                });
            return;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_error_banner(ui);

            ui.horizontal(|ui| {
                // Telemetry tab — red if any connection issue
                let has_issue = self.actors.values().any(|a| {
                    a.status == ActorStatus::Disconnected
                        || a.status == ActorStatus::Reconnecting
                        || a.status == ActorStatus::Starting
                });
                if has_issue && self.active_tab != Tab::Telemetry {
                    let text = egui::RichText::new("Telemetry")
                        .color(egui::Color32::from_rgb(255, 80, 80));
                    if ui.selectable_label(false, text).clicked() {
                        self.active_tab = Tab::Telemetry;
                    }
                } else {
                    ui.selectable_value(&mut self.active_tab, Tab::Telemetry, "Telemetry");
                }

                ui.selectable_value(&mut self.active_tab, Tab::Shots, "Shots");
                ui.selectable_value(&mut self.active_tab, Tab::Log, "Log");
                ui.selectable_value(&mut self.active_tab, Tab::Settings, "Settings");

                ui.separator();

                // Global mode selector
                let mut new_mode: Option<&str> = None;
                let modes = [
                    ("full", "Full"),
                    ("chipping", "Chipping"),
                    ("putting", "Putting"),
                ];
                for (val, label) in &modes {
                    let selected = self.current_mode == *val;
                    let text = egui::RichText::new(*label).strong().size(12.0);
                    if ui.selectable_label(selected, text).clicked() && !selected {
                        new_mode = Some(val);
                    }
                }
                if let Some(mode) = new_mode {
                    self.current_mode = mode.to_string();
                    if let Some(tx) = &mut self.ws_sender {
                        let msg = format!(r#"{{"cmd":"mode","mode":"{mode}"}}"#);
                        tx.send(ewebsock::WsMessage::Text(msg));
                    } else {
                        net::post_mode(ui.ctx(), mode);
                    }
                }

                // Right-aligned title
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("FLIGHTHOOK")
                            .strong()
                            .size(14.0)
                            .color(egui::Color32::from_rgb(140, 140, 140)),
                    );
                });
            });
            ui.separator();

            match self.active_tab {
                Tab::Shots => self.render_shots_panel(ui),
                Tab::Telemetry => self.render_telemetry_panel(ui),
                Tab::Log => self.render_log_panel(ui),
                Tab::Settings => {
                    let ctx = ui.ctx().clone();
                    self.render_settings_panel(&ctx, ui);
                }
            }
        });
    }
}
