//! Which pane the shots navigation serves (`ShotsPane`), the toolbar's Live switch, and the
//! grades reference picker (`stored_reference`).

use super::*;

/// Which of the two tabs that share the shots navigation shows: the Shots
/// tab or the Cameras tab (the nav icons).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ShotsPane {
    /// The shots' numbers and charts, in the Cards | Table choice.
    #[default]
    Data,
    /// The shot's clips and swing analysis.
    Cameras,
}

impl ShotsPane {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Cameras => "cameras",
        }
    }

    /// The choice kept on this device, Data when there is none.
    pub(crate) fn stored() -> Self {
        match crate::local::load(PANE_KEY).as_deref() {
            Some("cameras") => Self::Cameras,
            _ => Self::Data,
        }
    }
}

impl FlighthookApp {
    /// The toolbar's Live switch, while the Cameras pane shows: on, the
    /// live views; off, the shot the list shows.
    pub(crate) fn render_live_toggle(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.cameras_pane_shown() {
            return false;
        }
        let toggle = theme::switch_pill(ui, self.live_selected, "Live", theme::DANGER);
        devices::mark(ui, || "live_toggle".to_owned(), &toggle.response);
        if toggle.inner.clicked() {
            if self.live_selected {
                self.live_selected = false;
            } else {
                self.select_live();
            }
        }
        true
    }
}

impl FlighthookApp {
    /// The toolbar's reference picker, while the Cameras pane shows: which
    /// population's bands the swing numbers are colored against (marks
    /// `body_reference`, `body_reference:<key>`). Kept on this device.
    pub(crate) fn render_reference_picker(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.cameras_pane_shown() {
            return false;
        }
        let selected = ReferenceKind::ALL
            .iter()
            .position(|r| *r == self.body_reference)
            .unwrap_or(0);
        let labels = ReferenceKind::ALL.map(ReferenceKind::label);
        let picker = theme::segmented_pills(ui, &labels, selected, None, theme::PILL_FONT);
        devices::mark(ui, || "body_reference".to_owned(), &picker.response);
        let (clicked, pills) = picker.inner;
        for (reference, pill) in ReferenceKind::ALL.iter().zip(&pills) {
            devices::mark(ui, || format!("body_reference:{}", reference.key()), pill);
        }
        if let Some(i) = clicked {
            self.set_body_reference(ReferenceKind::ALL[i]);
        }
        true
    }

    /// Color the swing numbers against `reference`'s bands, and keep the
    /// choice on this device.
    pub(crate) fn set_body_reference(&mut self, reference: ReferenceKind) {
        self.body_reference = reference;
        crate::local::store(REFERENCE_KEY, reference.key());
    }
}

/// Column `c`'s grades against `reference`, once fetched.
pub(super) fn column_grades(
    c: &replay::PlayerColumn,
    reference: ReferenceKind,
) -> Option<&ReferenceGrades> {
    c.grades.as_deref()?.reference(reference)
}

/// Where the reference picker's choice is kept.
pub(super) const REFERENCE_KEY: &str = "body_reference";

/// The reference kept on this device, Tour when there is none.
pub(crate) fn stored_reference() -> ReferenceKind {
    let kept = crate::local::load(REFERENCE_KEY);
    ReferenceKind::ALL
        .into_iter()
        .find(|r| kept.as_deref() == Some(r.key()))
        .unwrap_or_default()
}
