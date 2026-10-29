use crate::app::FlighthookApp;
use crate::panels::devices;
use crate::theme;
use crate::types::{ActorStatusResponse, ConnectionState};

/// Stripe and badge colour for an actor at `now_ms`: green when Connected,
/// yellow while Connecting, red while Reconnecting within
/// [`RECONNECT_ALERT_MS`](crate::types::RECONNECT_ALERT_MS) of the lost
/// connection and yellow after that.
pub(crate) fn status_color(actor: &ActorStatusResponse, now_ms: u64) -> egui::Color32 {
    match actor.state() {
        ConnectionState::Connected => theme::STATUS_OK,
        ConnectionState::Reconnecting if actor.red(now_ms) => theme::STATUS_DOWN,
        ConnectionState::Connecting | ConnectionState::Reconnecting => theme::STATUS_PENDING,
    }
}

/// Widest the key column of [`telemetry_body`] gets, as a share of the card.
const TELEMETRY_KEY_SHARE: f32 = 0.45;
/// Space between a telemetry key and its value.
const TELEMETRY_GAP: f32 = 18.0;

/// A device card's telemetry: every key/value the actor reports, sorted by
/// key, one per row. The rows keep to the card's width: the key column is as
/// wide as the longest key (at most [`TELEMETRY_KEY_SHARE`] of the card), and
/// a key or value too long for its column is cut short with "…", the full
/// text on hover. An empty value is not shown.
pub(crate) fn telemetry_body(ui: &mut egui::Ui, id: &str, actor: &ActorStatusResponse) {
    let mut keys: Vec<&String> = actor
        .telemetry
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, _)| k)
        .collect();
    if keys.is_empty() {
        return;
    }
    keys.sort();
    ui.add_space(6.0);
    let width = ui.available_width();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let key_width = keys
        .iter()
        .map(|k| {
            ui.painter()
                .layout_no_wrap((*k).clone(), font.clone(), theme::TEXT_MUTED)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
        .min(width * TELEMETRY_KEY_SHARE)
        .ceil();
    let value_width = (width - key_width - TELEMETRY_GAP).max(0.0);
    let height = ui.spacing().interact_size.y.min(18.0);
    let cell = |ui: &mut egui::Ui, w: f32, text: egui::RichText| {
        ui.allocate_ui_with_layout(
            egui::vec2(w, height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(w);
                ui.set_max_width(w);
                ui.add(egui::Label::new(text).truncate())
            },
        )
        .inner
    };
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(TELEMETRY_GAP, 4.0);
        for key in keys {
            ui.horizontal(|ui| {
                cell(
                    ui,
                    key_width,
                    egui::RichText::new(key.as_str()).color(theme::TEXT_MUTED),
                );
                let value = cell(
                    ui,
                    value_width,
                    egui::RichText::new(&actor.telemetry[key]).color(theme::TEXT),
                );
                devices::mark(ui, || format!("telemetry:{id}:{key}"), &value);
            });
        }
    });
}

impl FlighthookApp {
    /// One banner per actor reporting an `error`, then one for a failed
    /// Auto-chip.
    pub(crate) fn render_error_banner(&self, ui: &mut egui::Ui) {
        for actor in self.actors.values() {
            if let Some(err) = actor.telemetry.get("error").filter(|e| !e.is_empty()) {
                error_banner(ui, &format!("{}: {err}", actor.name));
            }
        }
        if let Some(err) = &self.auto_chip_error {
            error_banner(ui, &format!("{}: {err}", flighthook::AUTO_CHIP_NAME));
        }
    }
}

/// A full-width red banner with `text`.
fn error_banner(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(theme::DANGER.gamma_multiply(0.22))
        .stroke(egui::Stroke::new(1.0, theme::DANGER))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(text).color(theme::TEXT).strong());
        });
    ui.add_space(8.0);
}
