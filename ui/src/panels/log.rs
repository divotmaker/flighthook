use crate::app::FlighthookApp;
use crate::types::{AlertLevel, FlighthookEvent};

use super::extract_time;

/// All message type keys in display order.
pub(crate) const MESSAGE_TYPES: &[&str] = &[
    "alert_error",
    "alert_warn",
    "shot_trigger",
    "ball_flight",
    "club_path",
    "shot_finished",
    "ready_state",
    "actor_status",
    "player_info",
    "club_info",
    "mode",
    "config_command",
    "config_outcome",
];

/// Filter groups for the dropdown.
const FILTER_GROUPS: &[(&str, &[&str])] = &[
    ("Launch Monitor", &["shot_trigger", "ball_flight", "club_path", "shot_finished", "ready_state"]),
    ("Game", &["player_info", "club_info", "mode"]),
    ("System", &["actor_status", "config_command", "config_outcome"]),
    ("Alert", &["alert_error", "alert_warn"]),
];

/// Return the root FlighthookEvent kind (the serde `kind` tag).
pub(crate) fn message_type(event: &FlighthookEvent) -> &'static str {
    match event {
        FlighthookEvent::ShotTrigger { .. } => "shot_trigger",
        FlighthookEvent::BallFlight { .. } => "ball_flight",
        FlighthookEvent::ClubPath { .. } => "club_path",
        FlighthookEvent::ShotFinished { .. } => "shot_finished",
        FlighthookEvent::LaunchMonitorState { .. } => "ready_state",
        FlighthookEvent::PlayerInfo { .. } => "player_info",
        FlighthookEvent::ClubInfo { .. } => "club_info",
        FlighthookEvent::ShotDetectionMode { .. } => "mode",
        FlighthookEvent::ActorStatus { .. } => "actor_status",
        FlighthookEvent::ConfigCommand { .. } => "config_command",
        FlighthookEvent::ConfigOutcome { .. } => "config_outcome",
        FlighthookEvent::Alert { level, .. } => match level {
            AlertLevel::Error => "alert_error",
            AlertLevel::Warn => "alert_warn",
        },
    }
}

/// Extract the alert level if this is an Alert event, otherwise None.
pub(crate) fn alert_level(event: &FlighthookEvent) -> Option<AlertLevel> {
    match event {
        FlighthookEvent::Alert { level, .. } => Some(*level),
        _ => None,
    }
}

/// Debug representation of the inner event (unwraps envelope layers).
pub(crate) fn event_debug(event: &FlighthookEvent) -> String {
    match event {
        FlighthookEvent::ShotTrigger { key } => format!("trigger #{}", key.shot_number),
        FlighthookEvent::BallFlight { key, estimated, .. } => {
            format!(
                "ball #{}{}",
                key.shot_number,
                if *estimated { " (estimated)" } else { "" }
            )
        }
        FlighthookEvent::ClubPath { key, .. } => format!("club #{}", key.shot_number),
        FlighthookEvent::ShotFinished { key } => format!("finished #{}", key.shot_number),
        FlighthookEvent::LaunchMonitorState {
            armed,
            ball_detected,
        } => format!("armed={armed} ball={ball_detected}"),
        FlighthookEvent::PlayerInfo { player_info } => {
            format!("handed={}", player_info.handed)
        }
        FlighthookEvent::ClubInfo { club_info } => format!("club={}", club_info.club),
        FlighthookEvent::ShotDetectionMode { mode } => format!("{mode:?}"),
        FlighthookEvent::ActorStatus { status, .. } => format!("{status:?}"),
        FlighthookEvent::ConfigCommand { action, .. } => format!("{action:?}"),
        FlighthookEvent::ConfigOutcome { request_id, .. } => match request_id {
            Some(rid) => format!("outcome({rid})"),
            None => "outcome".into(),
        },
        FlighthookEvent::Alert { level, message } => format!("[{level}] {message}"),
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

            // Filter dropdown button
            let enabled_count = self.log_type_filters.values().filter(|v| **v).count();
            let total_count = self.log_type_filters.len();
            let filter_label = if enabled_count == total_count {
                "Filters: all".to_string()
            } else {
                format!("Filters: {enabled_count}/{total_count}")
            };
            let popup_id = ui.make_persistent_id("log_filter_popup");
            let filter_btn = ui.button(&filter_label);
            if filter_btn.clicked() {
                self.log_filter_open = !self.log_filter_open;
            }
            if self.log_filter_open {
                let area = egui::Area::new(popup_id)
                    .order(egui::Order::Foreground)
                    .fixed_pos(filter_btn.rect.left_bottom())
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            // Toggle all
                            let all_checked =
                                self.log_type_filters.iter().all(|(_, v)| *v);
                            let mut toggle_all = all_checked;
                            if ui.checkbox(&mut toggle_all, "All").clicked() {
                                for v in self.log_type_filters.values_mut() {
                                    *v = toggle_all;
                                }
                            }
                            ui.separator();

                            for &(group_name, keys) in FILTER_GROUPS {
                                // Group heading with toggle-all checkbox
                                let group_all = keys.iter().all(|k| {
                                    self.log_type_filters
                                        .get(*k)
                                        .copied()
                                        .unwrap_or(true)
                                });
                                let mut group_toggle = group_all;
                                let heading = egui::RichText::new(group_name).strong();
                                if ui.checkbox(&mut group_toggle, heading).clicked() {
                                    for &k in keys {
                                        if let Some(v) =
                                            self.log_type_filters.get_mut(k)
                                        {
                                            *v = group_toggle;
                                        }
                                    }
                                }

                                for &key in keys {
                                    if let Some(checked) =
                                        self.log_type_filters.get_mut(key)
                                    {
                                        ui.horizontal(|ui| {
                                            ui.add_space(16.0);
                                            ui.checkbox(checked, key);
                                        });
                                    }
                                }
                                ui.add_space(4.0);
                            }
                        });
                    });

                // Close popup when clicking outside it
                if ui.input(|i| i.pointer.any_click())
                    && !area.response.contains_pointer()
                    && !filter_btn.contains_pointer()
                {
                    self.log_filter_open = false;
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
