use crate::app::FlighthookApp;
use crate::types::{AlertLevel, FlighthookEvent};

use super::extract_time;

/// Alert level filter options for the dropdown.
const ALERT_LEVELS: &[&str] = &["error", "warn"];

/// All message type keys in display order.
pub(crate) const MESSAGE_TYPES: &[&str] = &[
    "alert",
    "launch_monitor",
    "actor_status",
    "game_state_command",
    "config_changed",
    "config_command",
    "config_outcome",
    "game_state_snapshot",
    "user_data",
];

/// Return the root FlighthookEvent kind (the serde `kind` tag).
pub(crate) fn message_type(event: &FlighthookEvent) -> &'static str {
    match event {
        FlighthookEvent::LaunchMonitor(_) => "launch_monitor",
        FlighthookEvent::ConfigChanged(_) => "config_changed",
        FlighthookEvent::GameStateCommand(_) => "game_state_command",
        FlighthookEvent::GameStateSnapshot(_) => "game_state_snapshot",
        FlighthookEvent::UserData(_) => "user_data",
        FlighthookEvent::ActorStatus(_) => "actor_status",
        FlighthookEvent::ConfigCommand(_) => "config_command",
        FlighthookEvent::ConfigOutcome(_) => "config_outcome",
        FlighthookEvent::Alert(_) => "alert",
    }
}

/// Extract the alert level if this is an Alert event, otherwise None.
pub(crate) fn alert_level(event: &FlighthookEvent) -> Option<AlertLevel> {
    match event {
        FlighthookEvent::Alert(alert) => Some(alert.level),
        _ => None,
    }
}

/// Debug representation of the inner event (unwraps envelope layers).
pub(crate) fn event_debug(event: &FlighthookEvent) -> String {
    match event {
        FlighthookEvent::LaunchMonitor(recv) => format!("{:?}", recv.event),
        FlighthookEvent::ConfigChanged(changed) => format!("{:?}", changed.config),
        FlighthookEvent::GameStateCommand(cmd) => format!("{:?}", cmd.event),
        FlighthookEvent::GameStateSnapshot(snap) => format!("{snap:?}"),
        FlighthookEvent::UserData(data) => format!("{data:?}"),
        FlighthookEvent::ActorStatus(update) => format!("{:?}", update.status),
        FlighthookEvent::ConfigCommand(cmd) => format!("{:?}", cmd.action),
        FlighthookEvent::ConfigOutcome(result) => format!("{result:?}"),
        FlighthookEvent::Alert(alert) => format!("[{}] {}", alert.level, alert.message),
    }
}

impl FlighthookApp {
    pub(crate) fn render_log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Clear").clicked() {
                self.log_entries.clear();
            }
            ui.checkbox(&mut self.log_auto_scroll, "Auto-scroll");
            ui.separator();

            // Toggle-all checkbox
            let all_checked = self.log_type_filters.iter().all(|(_, v)| *v);
            let mut toggle_all = all_checked;
            if ui.checkbox(&mut toggle_all, "All").clicked() {
                for v in self.log_type_filters.values_mut() {
                    *v = toggle_all;
                }
            }

            // Per-type filters
            for &key in MESSAGE_TYPES {
                if key == "alert" {
                    // Alert gets a dropdown instead of a checkbox
                    if let Some(checked) = self.log_type_filters.get_mut(key) {
                        ui.checkbox(checked, "alert:");
                    }
                    let selected = ALERT_LEVELS[self.log_alert_filter];
                    egui::ComboBox::from_id_salt("log_alert_level")
                        .width(60.0)
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (i, &level) in ALERT_LEVELS.iter().enumerate() {
                                ui.selectable_value(&mut self.log_alert_filter, i, level);
                            }
                        });
                } else if let Some(checked) = self.log_type_filters.get_mut(key) {
                    ui.checkbox(checked, key);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!("{} events", self.log_entries.len()));
            });
        });

        let scroll = egui::ScrollArea::both()
            .stick_to_bottom(self.log_auto_scroll)
            .auto_shrink(false);
        scroll.show(ui, |ui| {
            let mono = |size: f32, text: &str| -> egui::RichText {
                egui::RichText::new(text).monospace().size(size)
            };

            let dim = egui::Color32::from_rgb(140, 140, 140);
            let warn_color = egui::Color32::from_rgb(255, 200, 60);
            let error_color = egui::Color32::from_rgb(255, 80, 80);

            // Resolve the minimum alert level from the dropdown
            let min_alert_level = match self.log_alert_filter {
                0 => AlertLevel::Error,
                _ => AlertLevel::Warn,
            };

            egui::Grid::new("log_grid")
                .num_columns(3)
                .min_col_width(0.0)
                .spacing(egui::vec2(12.0, 1.0))
                .show(ui, |ui| {
                    for entry in &self.log_entries {
                        // Skip entries whose type is filtered out
                        if !self
                            .log_type_filters
                            .get(entry.message_type.as_str())
                            .copied()
                            .unwrap_or(true)
                        {
                            continue;
                        }

                        // For alert entries, apply the level filter
                        if entry.message_type == "alert" {
                            if let Some(level) = entry.alert_level {
                                if level < min_alert_level {
                                    continue;
                                }
                            }
                        }

                        let time_str = extract_time(&entry.timestamp);

                        // Pick text color for alerts
                        let text_color = match entry.alert_level {
                            Some(AlertLevel::Error) => Some(error_color),
                            Some(AlertLevel::Warn) => Some(warn_color),
                            None => None,
                        };

                        // Row 1: timestamp | source name | event debug
                        ui.label(mono(11.0, &time_str));
                        ui.label(
                            mono(11.0, &entry.source_name)
                                .color(egui::Color32::from_rgb(180, 160, 220)),
                        );
                        let debug_label = mono(11.0, &entry.event_debug);
                        ui.label(match text_color {
                            Some(c) => debug_label.color(c),
                            None => debug_label,
                        });
                        ui.end_row();

                        // Row 2: message type | source id | raw payload
                        ui.label(mono(11.0, &entry.message_type).color(dim));
                        ui.label(mono(11.0, &entry.source_id).color(dim));
                        if entry.raw.is_empty() {
                            ui.label(mono(11.0, "(no payload)").color(dim));
                        } else {
                            ui.label(mono(11.0, &entry.raw).color(dim));
                        }
                        ui.end_row();

                        // Spacer row between entries
                        ui.allocate_space(egui::vec2(0.0, 4.0));
                        ui.label("");
                        ui.label("");
                        ui.end_row();
                    }
                });
        });
    }
}
