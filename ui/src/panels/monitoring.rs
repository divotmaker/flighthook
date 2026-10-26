use crate::app::FlighthookApp;
use crate::theme;
use crate::types::{ActorStatus, ActorStatusResponse};

/// Badge colour for an actor status: green when connected, gold while
/// starting or reconnecting, red when disconnected.
pub(crate) fn status_color(status: ActorStatus) -> egui::Color32 {
    match status {
        ActorStatus::Connected => theme::STATUS_OK,
        ActorStatus::Starting | ActorStatus::Reconnecting => theme::STATUS_PENDING,
        ActorStatus::Disconnected => theme::STATUS_DOWN,
    }
}

/// A device card's telemetry: every key/value the actor reports, sorted by
/// key, one per row.
pub(crate) fn telemetry_body(ui: &mut egui::Ui, id: &str, actor: &ActorStatusResponse) {
    if actor.telemetry.is_empty() {
        return;
    }
    ui.add_space(6.0);
    let mut keys: Vec<&String> = actor.telemetry.keys().collect();
    keys.sort();
    egui::Grid::new(("telemetry", id))
        .num_columns(2)
        .spacing(egui::vec2(18.0, 4.0))
        .show(ui, |ui| {
            for key in &keys {
                ui.label(egui::RichText::new(key.as_str()).color(theme::TEXT_MUTED));
                ui.label(egui::RichText::new(&actor.telemetry[*key]).color(theme::TEXT));
                ui.end_row();
            }
        });
}

impl FlighthookApp {
    pub(crate) fn render_error_banner(&self, ui: &mut egui::Ui) {
        for actor in self.actors.values() {
            if let Some(err) = actor.telemetry.get("error") {
                egui::Frame::new()
                    .fill(theme::DANGER.gamma_multiply(0.22))
                    .stroke(egui::Stroke::new(1.0, theme::DANGER))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            egui::RichText::new(format!("{}: {err}", actor.name))
                                .color(theme::TEXT)
                                .strong(),
                        );
                    });
                ui.add_space(8.0);
            }
        }
    }
}
