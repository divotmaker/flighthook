use crate::app::FlighthookApp;
use crate::theme;
use crate::types::{FlighthookEvent, Severity};

use super::extract_time;

/// All message type keys in display order.
pub(crate) const MESSAGE_TYPES: &[&str] = &[
    "alert_error",
    "alert_warn",
    "alert_critical",
    "shot_trigger",
    "ball_flight",
    "club_path",
    "face_impact",
    "shot_finished",
    "device_telemetry",
    "actor_status",
    "player_info",
    "club_info",
    "set_detection_mode",
    "new_shot_group",
    "new_shot_session",
    "clear_shot_history",
    "shot_group",
    "config_command",
    "config_outcome",
];

/// Filter groups for the dropdown.
const FILTER_GROUPS: &[(&str, &[&str])] = &[
    (
        "Launch Monitor",
        &[
            "shot_trigger",
            "ball_flight",
            "club_path",
            "face_impact",
            "shot_finished",
            "device_telemetry",
        ],
    ),
    (
        "Game",
        &[
            "player_info",
            "club_info",
            "set_detection_mode",
            "new_shot_group",
            "new_shot_session",
            "clear_shot_history",
            "shot_group",
        ],
    ),
    (
        "System",
        &["actor_status", "config_command", "config_outcome"],
    ),
    ("Alert", &["alert_error", "alert_warn", "alert_critical"]),
];

/// Return the root FlighthookEvent kind (the serde `kind` tag).
pub(crate) fn message_type(event: &FlighthookEvent) -> &'static str {
    match event {
        FlighthookEvent::ShotTrigger { .. } => "shot_trigger",
        FlighthookEvent::BallFlight { .. } => "ball_flight",
        FlighthookEvent::ClubPath { .. } => "club_path",
        FlighthookEvent::FaceImpact { .. } => "face_impact",
        FlighthookEvent::ShotFinished { .. } => "shot_finished",
        FlighthookEvent::DeviceTelemetry { .. } => "device_telemetry",
        FlighthookEvent::PlayerInfo { .. } => "player_info",
        FlighthookEvent::ClubInfo { .. } => "club_info",
        FlighthookEvent::SetDetectionMode { .. } => "set_detection_mode",
        FlighthookEvent::ActorStatus { .. } => "actor_status",
        FlighthookEvent::ConfigCommand { .. } => "config_command",
        FlighthookEvent::ConfigOutcome { .. } => "config_outcome",
        FlighthookEvent::NewShotGroup => "new_shot_group",
        FlighthookEvent::NewShotSession => "new_shot_session",
        FlighthookEvent::ClearShotHistory => "clear_shot_history",
        FlighthookEvent::ShotGroup { .. } => "shot_group",
        FlighthookEvent::Alert { severity, .. } => match severity {
            Severity::Critical => "alert_critical",
            Severity::Error => "alert_error",
            Severity::Warn => "alert_warn",
        },
    }
}

/// Extract the alert severity if this is an Alert event, otherwise None.
pub(crate) fn alert_severity(event: &FlighthookEvent) -> Option<Severity> {
    match event {
        FlighthookEvent::Alert { severity, .. } => Some(*severity),
        _ => None,
    }
}

/// Debug representation of the inner event (unwraps envelope layers).
pub(crate) fn event_debug(event: &FlighthookEvent) -> String {
    match event {
        FlighthookEvent::ShotTrigger { key } => format!("trigger #{}", key.shot_number),
        FlighthookEvent::BallFlight { key, .. } => {
            format!("ball #{}", key.shot_number)
        }
        FlighthookEvent::ClubPath { key, .. } => format!("club #{}", key.shot_number),
        FlighthookEvent::FaceImpact { key, .. } => format!("impact #{}", key.shot_number),
        FlighthookEvent::ShotFinished { key, raw } => match raw {
            Some(r) => format!(
                "finished #{} ({} raw frames)",
                key.shot_number,
                r.frames.len()
            ),
            None => format!("finished #{}", key.shot_number),
        },
        FlighthookEvent::DeviceTelemetry {
            manufacturer,
            model,
            telemetry,
            ..
        } => {
            // Show readiness if telemetry has ready key, otherwise show device identity
            if let Some(tel) = telemetry
                && tel.contains_key("ready")
            {
                let ready = tel.get("ready").map(|v| v.as_str()).unwrap_or("?");
                return format!("ready={ready}");
            }
            format!(
                "device: {} {}",
                manufacturer.as_deref().unwrap_or("?"),
                model.as_deref().unwrap_or("?")
            )
        }
        FlighthookEvent::PlayerInfo { player_info } => {
            format!("name={}", player_info.name.as_deref().unwrap_or("?"))
        }
        FlighthookEvent::ClubInfo { club_info } => format!("club={}", club_info.club),
        FlighthookEvent::SetDetectionMode { mode, handed } => match (mode, handed) {
            (Some(m), Some(h)) => format!("{m:?} handed={h}"),
            (Some(m), None) => format!("{m:?}"),
            (None, Some(h)) => format!("handed={h}"),
            (None, None) => "no-op".into(),
        },
        FlighthookEvent::ActorStatus { status, .. } => format!("{status:?}"),
        FlighthookEvent::ConfigCommand { action, .. } => format!("{action:?}"),
        FlighthookEvent::ConfigOutcome { request_id, .. } => match request_id {
            Some(rid) => format!("outcome({rid})"),
            None => "outcome".into(),
        },
        FlighthookEvent::NewShotGroup => "new set".into(),
        FlighthookEvent::NewShotSession => "new session".into(),
        FlighthookEvent::ClearShotHistory => "clear history".into(),
        FlighthookEvent::ShotGroup { group, shot } => {
            let club = group
                .club
                .map_or_else(|| "no club".to_owned(), |c| c.to_string());
            let state = if group.open { "open" } else { "closed" };
            match shot {
                Some(s) => format!(
                    "set {} {club}: {} #{} joined ({} shots, {state})",
                    group.number, s.actor, s.key.shot_number, group.shots
                ),
                None => format!(
                    "set {} {club} ({} shots, {state})",
                    group.number, group.shots
                ),
            }
        }
        FlighthookEvent::Alert { severity, message } => format!("[{severity}] {message}"),
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
                            let all_checked = self.log_type_filters.iter().all(|(_, v)| *v);
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
                                    self.log_type_filters.get(*k).copied().unwrap_or(true)
                                });
                                let mut group_toggle = group_all;
                                let heading = egui::RichText::new(group_name).strong();
                                if ui.checkbox(&mut group_toggle, heading).clicked() {
                                    for &k in keys {
                                        if let Some(v) = self.log_type_filters.get_mut(k) {
                                            *v = group_toggle;
                                        }
                                    }
                                }

                                for &key in keys {
                                    if let Some(checked) = self.log_type_filters.get_mut(key) {
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
            // Dense rows: the log is a wall of monospace, not a form.
            ui.spacing_mut().interact_size.y = 14.0;
            let mono = |size: f32, text: &str| -> egui::RichText {
                egui::RichText::new(text).monospace().size(size)
            };

            let dim = theme::TEXT_MUTED;
            let warn_color = theme::GOLD;
            let error_color = theme::DANGER;

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
                        let text_color = match entry.alert_severity {
                            Some(Severity::Error | Severity::Critical) => Some(error_color),
                            Some(Severity::Warn) => Some(warn_color),
                            None => None,
                        };

                        // Row 1: timestamp | actor name | event debug
                        ui.label(mono(11.0, &time_str));
                        ui.label(mono(11.0, &entry.actor_name).color(theme::LOG_ACTOR));
                        let debug_label = mono(11.0, &entry.event_debug);
                        ui.label(match text_color {
                            Some(c) => debug_label.color(c),
                            None => debug_label,
                        });
                        ui.end_row();

                        // Row 2: message type | actor id | raw payload
                        ui.label(mono(11.0, &entry.message_type).color(dim));
                        ui.label(mono(11.0, &entry.actor_id).color(dim));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_is_in_exactly_one_filter_group() {
        for kind in MESSAGE_TYPES {
            let groups = FILTER_GROUPS
                .iter()
                .filter(|(_, keys)| keys.contains(kind))
                .count();
            assert_eq!(groups, 1, "{kind}");
        }
        let grouped: usize = FILTER_GROUPS.iter().map(|(_, keys)| keys.len()).sum();
        assert_eq!(grouped, MESSAGE_TYPES.len());
        let game = FILTER_GROUPS
            .iter()
            .find(|(name, _)| *name == "Game")
            .unwrap();
        for kind in [
            "new_shot_group",
            "new_shot_session",
            "clear_shot_history",
            "shot_group",
        ] {
            assert!(game.1.contains(&kind), "{kind}");
        }
    }

    #[test]
    fn group_summaries_say_set() {
        use crate::types::{Club, ShotGroupInfo, ShotKey};
        use flighthook::GroupedShot;
        let group = ShotGroupInfo {
            id: "a".into(),
            number: 3,
            session: 0,
            golfer: None,
            club: Some(Club::Iron7),
            started_at: "2026-10-26T15:41:28.300Z".into(),
            manual: false,
            open: true,
            shots: 4,
        };
        assert_eq!(event_debug(&FlighthookEvent::NewShotGroup), "new set");
        assert_eq!(
            event_debug(&FlighthookEvent::ClearShotHistory),
            "clear history"
        );
        assert_eq!(
            message_type(&FlighthookEvent::ClearShotHistory),
            "clear_shot_history"
        );
        let joined = FlighthookEvent::ShotGroup {
            group: group.clone(),
            shot: Some(GroupedShot {
                actor: "mevo.0".into(),
                key: ShotKey {
                    shot_id: "s9".into(),
                    shot_number: 9,
                },
            }),
        };
        assert_eq!(
            event_debug(&joined),
            "set 3 7I: mevo.0 #9 joined (4 shots, open)"
        );
        let changed = FlighthookEvent::ShotGroup { group, shot: None };
        assert_eq!(event_debug(&changed), "set 3 7I (4 shots, open)");
    }
}
