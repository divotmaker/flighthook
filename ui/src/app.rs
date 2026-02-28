//! egui application — FlighthookApp.

use std::collections::HashMap;

use crate::net::{self, PendingHandle, WsPollEvent};
use crate::panels::Tab;
use crate::panels::settings::{PendingRemoval, SettingsForm};
use crate::types::{
    ActorState, ActorStatus, ActorStatusResponse, FlighthookEvent, FlighthookMessage,
    GameStateCommand, GameStateCommandEvent, LaunchMonitorEvent, LaunchMonitorRecv, LogEntry,
    ShotData, UnitSystem,
};
use chrono::SecondsFormat;

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
    pub(crate) shots: Vec<ShotData>,

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

    // Log state
    pub(crate) log_entries: Vec<LogEntry>,
    pub(crate) log_auto_scroll: bool,
    pub(crate) log_type_filters: HashMap<String, bool>,
    /// Alert level filter: 0 = error only, 1 = warn (includes error). Default: warn.
    pub(crate) log_alert_filter: usize,
}

impl FlighthookApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let pending = net::new_pending();

        // Fire initial REST fetches
        net::fetch_status(&cc.egui_ctx, &pending);
        net::fetch_shots(&cc.egui_ctx, &pending);

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
            show_api_docs: false,
            api_docs_cache: egui_commonmark::CommonMarkCache::default(),
            log_entries: Vec::new(),
            log_auto_scroll: true,
            log_type_filters: crate::panels::log::MESSAGE_TYPES
                .iter()
                .map(|&k| (k.to_string(), true))
                .collect(),
            log_alert_filter: 1, // default: warn (show warn + error)
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
            self.shots = shots;
        }

        if let Some(s) = p.settings.take()
            && !self.settings.dirty
        {
            let first_load = !self.settings.loaded;
            self.settings.load_from(&s);
            if first_load {
                self.units_toggle = s.default_units;
            }
        }

        if let Some(_resp) = p.settings_save.take() {
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
            use crate::panels::log::{alert_level, event_debug, message_type};

            let source_name = self
                .actors
                .get(&msg.source)
                .map(|a| a.name.as_str())
                .unwrap_or("")
                .to_string();
            let raw = msg
                .raw_payload
                .as_ref()
                .map(|r| r.to_string())
                .unwrap_or_default();
            self.log_entries.push(LogEntry {
                timestamp: msg.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
                source_name,
                source_id: msg.source.clone(),
                message_type: message_type(&msg.event).to_string(),
                event_debug: event_debug(&msg.event),
                raw,
                alert_level: alert_level(&msg.event),
            });
            const MAX_LOG_ENTRIES: usize = 500;
            if self.log_entries.len() > MAX_LOG_ENTRIES {
                self.log_entries
                    .drain(..self.log_entries.len() - MAX_LOG_ENTRIES);
            }
        }

        // Route event to UI state
        let source = msg.source.clone();
        match msg.event {
            FlighthookEvent::ActorStatus(update) => {
                self.handle_actor_status(&source, update);
            }
            FlighthookEvent::LaunchMonitor(recv) => {
                self.handle_monitor_recv(&source, recv);
            }
            FlighthookEvent::GameStateCommand(cmd) => {
                self.handle_game_state_command(&source, cmd);
            }
            _ => {}
        }
    }

    fn handle_actor_status(&mut self, source: &str, update: ActorState) {
        let actor = self
            .actors
            .entry(source.to_string())
            .or_insert_with(|| new_actor(String::new()));
        actor.status = update.status;
        actor.telemetry = update.telemetry;
    }

    fn handle_monitor_recv(&mut self, _source: &str, recv: LaunchMonitorRecv) {
        match recv.event {
            LaunchMonitorEvent::ShotResult { shot } => {
                self.shots.push(*shot);
            }
            LaunchMonitorEvent::ReadyState { .. } => {
                // Handled by integration actors (GSPro); UI doesn't need this.
            }
        }
    }

    fn handle_game_state_command(&mut self, source: &str, cmd: GameStateCommand) {
        match cmd.event {
            GameStateCommandEvent::SetPlayerInfo { player_info } => {
                if let Some(actor) = self.actors.get_mut(source) {
                    actor.telemetry.insert("handed".into(), player_info.handed);
                }
            }
            GameStateCommandEvent::SetClubInfo { club_info } => {
                if let Some(actor) = self.actors.get_mut(source) {
                    actor
                        .telemetry
                        .insert("club".into(), club_info.club.to_string());
                }
            }
            GameStateCommandEvent::SetMode { mode } => {
                self.current_mode = mode.to_string();
            }
        }
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
                    a.status == ActorStatus::Disconnected || a.status == ActorStatus::Reconnecting
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
