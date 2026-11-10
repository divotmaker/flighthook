use crate::app::FlighthookApp;
use crate::panels::devices;
use crate::theme;
use crate::types::{ActorStatusResponse, ConnectionState};

/// Stripe and badge color for an actor at `now_ms`: green when Connected,
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

/// Whether [`telemetry_body`] has a row to show for `actor`.
pub(crate) fn has_telemetry(actor: &ActorStatusResponse) -> bool {
    actor.telemetry.values().any(|v| !v.is_empty())
}

/// The body of a card with no telemetry to show, centered in it: a check in
/// a ring once the actor is Connected, otherwise
/// a loading spinner in `color` (the card's status color) going round,
/// redrawn about 60 times a second while it is on screen.
pub(crate) fn no_telemetry(
    ui: &mut egui::Ui,
    id: &str,
    actor: &ActorStatusResponse,
    color: egui::Color32,
) {
    // The rest of the body (the scroll area's viewport), a little short of
    // it, so it never scrolls.
    let height = (ui.available_height() - 1.0).max(theme::STATUS_RING);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height.floor()),
        egui::Sense::hover(),
    );
    if !ui.is_rect_visible(rect) {
        return;
    }
    let center = rect.center();
    if actor.state() == ConnectionState::Connected {
        theme::connected_ring(ui.painter(), center);
        devices::mark(ui, || format!("connected_icon:{id}"), &response);
    } else {
        theme::loading_ring(ui.painter(), center, color, ui.input(|i| i.time));
        ui.ctx().request_repaint_after(theme::LOADING_FRAME);
        devices::mark(ui, || format!("spinner:{id}"), &response);
    }
}

impl FlighthookApp {
    /// One banner per actor reporting an `error` (or whose reconnect
    /// request failed), then one for a failed Auto-chip.
    pub(crate) fn render_error_banner(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.config_error {
            config_error_frame(ui, error);
        }
        for (id, actor) in &self.actors {
            if let Some(err) = self
                .reconnect_errors
                .get(id)
                .or_else(|| actor.telemetry.get("error"))
                .filter(|e| !e.is_empty())
            {
                error_banner(ui, &format!("{}: {err}", actor.name));
            }
        }
        for (id, err) in &self.reconnect_errors {
            if !self.actors.contains_key(id) {
                error_banner(ui, &format!("{id}: {err}"));
            }
        }
        if let Some(err) = &self.auto_chip_error {
            error_banner(ui, &format!("{}: {err}", flighthook::AUTO_CHIP_NAME));
        }
    }
}

/// The error frame for a config file the server could not use: what
/// happened, which file, and the fault.
fn config_error_frame(ui: &mut egui::Ui, error: &crate::types::ConfigLoadError) {
    let shown = egui::Frame::new()
        .fill(theme::DANGER.gamma_multiply(0.22))
        .stroke(egui::Stroke::new(1.0, theme::DANGER))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new("The config file couldn't be read. The defaults are in use.")
                    .color(theme::TEXT)
                    .strong(),
            );
            ui.label(egui::RichText::new(&error.path).color(theme::TEXT));
            ui.label(egui::RichText::new(&error.error).color(theme::TEXT));
        });
    devices::mark(ui, || "config_error".into(), &shown.response);
    ui.add_space(8.0);
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::types::ActorStatus;

    /// The repaint asked for after a few passes drawing `add`, on a
    /// 400×300 screen.
    fn repaint_after(mut add: impl FnMut(&mut egui::Ui)) -> Duration {
        let ctx = egui::Context::default();
        let mut delay = Duration::MAX;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| add(ui));
            });
            delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        }
        delay
    }

    fn actor(statuses: &[ActorStatus]) -> ActorStatusResponse {
        let mut actor = ActorStatusResponse::new("Mevo+", statuses[0]);
        for status in &statuses[1..] {
            actor.observe(*status, 0);
        }
        actor
    }

    /// A spinner on screen asks for the next frame about 30 times a
    /// second; the connected mark, telemetry rows and a spinner off screen
    /// ask for none.
    #[test]
    fn only_a_spinner_on_screen_asks_for_repaints() {
        let connecting = actor(&[ActorStatus::Starting]);
        let reconnecting = actor(&[ActorStatus::Connected, ActorStatus::Reconnecting]);
        for a in [&connecting, &reconnecting] {
            let delay = repaint_after(|ui| no_telemetry(ui, "mevo.0", a, theme::GOLD));
            assert!(delay <= theme::LOADING_FRAME, "{delay:?}");
        }

        let connected = actor(&[ActorStatus::Connected]);
        let delay = repaint_after(|ui| no_telemetry(ui, "mevo.0", &connected, theme::ACCENT));
        assert_eq!(delay, Duration::MAX);

        let mut rows = actor(&[ActorStatus::Starting]);
        rows.telemetry.insert("ready".into(), "false".into());
        assert!(has_telemetry(&rows));
        assert!(!has_telemetry(&connecting));
        let delay = repaint_after(|ui| telemetry_body(ui, "mevo.0", &rows));
        assert_eq!(delay, Duration::MAX);

        let delay = repaint_after(|ui| {
            ui.add_space(1000.0);
            no_telemetry(ui, "mevo.0", &connecting, theme::GOLD);
        });
        assert_eq!(delay, Duration::MAX, "off screen");
    }
}
