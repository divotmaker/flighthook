use crate::app::FlighthookApp;
use crate::types::ActorStatus;

impl FlighthookApp {
    pub(crate) fn render_telemetry_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            // Collect and sort actors by display name, then by ID
            let mut sorted: Vec<(&String, &crate::types::ActorStatusResponse)> =
                self.actors.iter().collect();
            sorted.sort_by(|a, b| a.1.name.cmp(&b.1.name).then_with(|| a.0.cmp(b.0)));

            if sorted.is_empty() {
                ui.label("No connections configured.");
            } else {
                for (_id, actor) in &sorted {
                    let status_color = match actor.status {
                        ActorStatus::Connected => egui::Color32::from_rgb(40, 167, 69),
                        ActorStatus::Starting => egui::Color32::from_rgb(255, 193, 7),
                        ActorStatus::Disconnected | ActorStatus::Reconnecting => {
                            egui::Color32::from_rgb(220, 53, 69)
                        }
                    };

                    // Header with status badge
                    let status_label = actor.status.to_string().to_uppercase();
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&actor.name)
                                .strong()
                                .color(egui::Color32::from_rgb(180, 200, 255)),
                        );
                        Self::render_badge(ui, &status_label, status_color);
                    });

                    // All telemetry fields, sorted by key, one per row
                    if !actor.telemetry.is_empty() {
                        let mut keys: Vec<&String> = actor.telemetry.keys().collect();
                        keys.sort();
                        for key in &keys {
                            let value = &actor.telemetry[*key];
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(format!("{key}: {value}"));
                            });
                        }
                    }

                    ui.add_space(6.0);
                }
            }
        });
    }

    pub(crate) fn render_badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
        let galley = ui.painter().layout_no_wrap(
            text.to_string(),
            egui::FontId::proportional(13.0),
            egui::Color32::WHITE,
        );
        let desired = galley.size() + egui::vec2(10.0, 4.0);
        let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
        ui.painter().rect_filled(rect, 3.0, color);
        ui.painter().galley(
            rect.min + egui::vec2(5.0, 2.0),
            galley,
            egui::Color32::WHITE,
        );
    }

    pub(crate) fn render_error_banner(&self, ui: &mut egui::Ui) {
        for (_id, actor) in &self.actors {
            if let Some(err) = actor.telemetry.get("error") {
                let frame = egui::Frame::new()
                    .fill(egui::Color32::from_rgb(220, 53, 69))
                    .inner_margin(egui::Margin::same(6));
                frame.show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!("{}: {err}", actor.name))
                            .color(egui::Color32::WHITE)
                            .strong(),
                    );
                });
            }
        }
    }
}
