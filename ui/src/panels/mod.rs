pub(crate) mod cards;
pub(crate) mod devices;
pub(crate) mod dispersion;
pub(crate) mod flight;
pub(crate) mod log;
pub(crate) mod monitoring;
pub(crate) mod settings;
pub(crate) mod shots;

use std::collections::HashMap;

use crate::app::FlighthookApp;
use crate::theme;
use crate::types::ActorStatusResponse;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Devices,
    Shots,
    Log,
}

/// The tab bar, in order. The app opens on the first.
pub(crate) const TABS: [(Tab, &str); 3] = [
    (Tab::Devices, "Devices"),
    (Tab::Shots, "Shots"),
    (Tab::Log, "Log"),
];

/// The tab that carries the red notification dot, as an index into
/// [`TABS`]: Devices, while any card is red at `now_ms`, i.e. an actor lost
/// its connection less than
/// [`RECONNECT_ALERT_MS`](crate::types::RECONNECT_ALERT_MS) ago. Every actor counts
/// alike, outputs and servers included; nothing else lights it.
pub(crate) fn tab_alert(
    actors: &HashMap<String, ActorStatusResponse>,
    now_ms: u64,
) -> Option<usize> {
    if !actors.values().any(|a| a.red(now_ms)) {
        return None;
    }
    TABS.iter().position(|(tab, _)| *tab == Tab::Devices)
}

/// Milliseconds until the first red card turns yellow, if any card is red.
pub(crate) fn next_alert_change(
    actors: &HashMap<String, ActorStatusResponse>,
    now_ms: u64,
) -> Option<u64> {
    actors
        .values()
        .map(|a| a.red_for_ms_at(now_ms))
        .filter(|ms| *ms > 0)
        .min()
}

impl FlighthookApp {
    /// The tab bar. Disabled while the Devices tab has unsaved edits.
    pub(crate) fn render_tabs(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let selected = TABS
            .iter()
            .position(|(tab, _)| *tab == self.active_tab)
            .unwrap_or(0);
        let labels = TABS.map(|(_, label)| label);
        let alert = tab_alert(&self.actors, self.now_ms);
        // A red card turns yellow on time, with no event to wake the UI.
        if let Some(ms) = next_alert_change(&self.actors, self.now_ms) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(ms));
        }
        let tabs = ui
            .add_enabled_ui(!self.settings.locked(), |ui| {
                theme::segmented(ui, &labels, selected, alert)
            })
            .inner;
        devices::mark(ui, || "tabs".into(), &tabs.response);
        if let Some(i) = tabs.inner {
            self.active_tab = TABS[i].0;
        }
        tabs.response
    }
}

/// Extract the time portion from an ISO 8601 timestamp string.
/// "2026-02-27T12:34:56.789Z" -> "12:34:56.789"
pub(crate) fn extract_time(iso: &str) -> String {
    // Find the 'T' separator and take everything after it, trimming trailing 'Z'
    if let Some(t_pos) = iso.find('T') {
        let time_part = &iso[t_pos + 1..];
        time_part.trim_end_matches('Z').to_string()
    } else {
        iso.to_string()
    }
}
