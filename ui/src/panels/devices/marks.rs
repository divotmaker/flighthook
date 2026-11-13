//! Test marks: the rects of the controls and card bodies the frame-driven tests click and measure
//! (no-ops outside tests).

#[cfg(test)]
use super::*;

/// The controls the tests click on, by name: their rect and whether they
/// were enabled, from the last pass.
#[cfg(test)]
pub(crate) type Marks = HashMap<String, (egui::Rect, bool)>;

/// Record a control under `name` for the tests.
#[cfg(test)]
pub(crate) fn mark(ui: &egui::Ui, name: impl FnOnce() -> String, response: &egui::Response) {
    let entry = (response.rect, response.enabled());
    ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Marks>(egui::Id::NULL)
            .insert(name(), entry);
    });
}

/// Record a control for the tests: nothing outside them.
#[cfg(not(test))]
#[inline]
pub(crate) fn mark(_ui: &egui::Ui, _name: impl FnOnce() -> String, _response: &egui::Response) {}

/// A card body's scroll state, for the tests: its rect, content height and
/// scroll offset, by card ID.
#[cfg(test)]
pub(crate) type Bodies = HashMap<String, (egui::Rect, f32, f32)>;

/// Where [`Bodies`] are kept for the tests.
#[cfg(test)]
pub(super) fn bodies_id() -> egui::Id {
    egui::Id::new("devices_bodies")
}

/// Record card `key`'s body for the tests.
#[cfg(test)]
pub(super) fn mark_body<R>(
    ui: &egui::Ui,
    key: &str,
    shown: &egui::scroll_area::ScrollAreaOutput<R>,
) {
    let entry = (shown.inner_rect, shown.content_size.y, shown.state.offset.y);
    ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Bodies>(bodies_id())
            .insert(key.to_owned(), entry);
    });
}

/// Record a card body for the tests: nothing outside them.
#[cfg(not(test))]
#[inline]
pub(super) fn mark_body<R>(
    _ui: &egui::Ui,
    _key: &str,
    _shown: &egui::scroll_area::ScrollAreaOutput<R>,
) {
}
