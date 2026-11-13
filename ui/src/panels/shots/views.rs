//! Shots tab views and chart glue: the Cards | Table view (`ShotsView`) and layout
//! (`ShotsLayout`) enums, where the Flight and Dispersion charts go (`chart_slots`), and the
//! empty-state placeholder.

use super::*;

/// A command on the shot history and its sets, from the Shots tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupCommand {
    NewGroup,
    NewSession,
    ClearHistory,
}

/// How the Shots tab shows the shots: cards for one shot, or a table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) enum ShotsView {
    /// One row per shot.
    Table,
    /// Large metric cards for one shot, picked from a shot list.
    #[default]
    Cards,
}

impl ShotsView {
    /// Every view, in toggle order.
    pub(super) const ALL: &[ShotsView] = &[ShotsView::Cards, ShotsView::Table];

    pub(super) fn label(self) -> &'static str {
        match self {
            ShotsView::Table => "Table",
            ShotsView::Cards => "Cards",
        }
    }

    /// Whether the toolbar shows the "italic = simulated" legend: only
    /// Table uses italics for simulated values, so only Table needs it.
    pub(super) fn shows_simulated_legend(self) -> bool {
        self == ShotsView::Table
    }
}

/// What the Shots tab lays out under its toolbar
/// ([`FlighthookApp::shots_layout`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShotsLayout {
    /// The Shots tab's Cards view.
    Cards,
    /// The Shots tab's Table view.
    Table,
    /// The Cameras pane right of the shot list, whichever view is chosen.
    Cameras,
}

impl ShotsLayout {
    /// The view whose shot list and charts it lays out: the Cameras pane
    /// keeps the Cards view's shot list and draws no charts.
    pub(super) fn view(self) -> ShotsView {
        match self {
            Self::Cards | Self::Cameras => ShotsView::Cards,
            Self::Table => ShotsView::Table,
        }
    }
}

/// A chart of the Shots tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Chart {
    /// Side view of the ball flights (`panels::flight`).
    Flight,
    /// Top-down landing and rest positions (`panels::dispersion`).
    Dispersion,
}

/// Windows at least this wide give Dispersion a full-height column on the
/// right; narrower ones put it in the bottom row.
pub(super) const WIDE_MIN_WIDTH: f32 = 1200.0;
/// Width of the Dispersion column: this share of the window's width,
/// within [`DISPERSION_WIDTH`].
pub(super) const DISPERSION_SHARE: f32 = 0.24;
pub(super) const DISPERSION_WIDTH: std::ops::RangeInclusive<f32> = 260.0..=420.0;
/// Height of the bottom row in Cards: [`ROW_SHARE`] of the window's height
/// within [`FLIGHT_HEIGHT`] when it holds Flight alone, and
/// [`SHARED_ROW_SHARE`] within [`SHARED_ROW_HEIGHT`] when Dispersion sits
/// beside it.
pub(super) const ROW_SHARE: f32 = 0.30;
pub(super) const FLIGHT_HEIGHT: std::ops::RangeInclusive<f32> = 180.0..=320.0;
pub(super) const SHARED_ROW_SHARE: f32 = 0.27;
pub(super) const SHARED_ROW_HEIGHT: std::ops::RangeInclusive<f32> =
    dispersion_floor(SHARED_PLOT_MIN)..=320.0;
/// Flight's share of a bottom row it shares with Dispersion. Dispersion
/// takes the rest, widened up to half the row so its header keeps to one
/// row.
pub(super) const FLIGHT_ROW_SHARE: f32 = 0.62;
/// Height of Dispersion under a narrow Table: this share of the window's
/// height, within [`TABLE_DISPERSION_HEIGHT`].
pub(super) const TABLE_DISPERSION_SHARE: f32 = 0.35;
pub(super) const TABLE_DISPERSION_HEIGHT: std::ops::RangeInclusive<f32> =
    dispersion_floor(TABLE_PLOT_MIN)..=400.0;
/// Least height of the Dispersion plot (inside its gutters) in the Cards
/// bottom row, where the cards above must still fit at 1024×768, and under
/// a narrow Table.
pub(super) const SHARED_PLOT_MIN: f32 = 144.0;
pub(super) const TABLE_PLOT_MIN: f32 = 200.0;
/// Gap between a chart and its neighbor.
pub(super) const CHART_GAP: f32 = 14.0;
/// Height of Flight and of Dispersion under the cards in the portrait
/// layout.
pub(super) const PORTRAIT_CHART_HEIGHT: f32 = 240.0;

/// Height of a bottom row whose Dispersion plot is `plot` tall under a
/// one-row header.
pub(super) const fn dispersion_floor(plot: f32) -> f32 {
    CHART_GAP + dispersion::ROW_H + dispersion::PLOT_PAD + plot
}

/// What the Shots tab's bottom row holds, left to right.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BottomRow {
    Flight,
    FlightAndDispersion,
    Dispersion,
}

/// Where the charts sit: both are always shown, sized from the window
/// so nothing moves until the window does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct ChartSlots {
    /// Width of the full-height Dispersion column on the right.
    pub(super) right: Option<f32>,
    /// Height of the bottom row (under the main content, between the shot
    /// list and the right-hand column) and what it holds.
    pub(super) bottom: Option<(f32, BottomRow)>,
}

/// The charts' places for `view` in a window of `window` size.
///
/// - Cards, wide: Dispersion in the right-hand column, Flight along the
///   bottom of the center.
/// - Cards, narrow: Flight and Dispersion side by side in the bottom row,
///   so the cards keep the center's width.
/// - Table, wide: Dispersion in the right-hand column.
/// - Table, narrow: Dispersion along the bottom.
pub(super) fn chart_slots(view: ShotsView, window: egui::Vec2) -> ChartSlots {
    let share = |len: f32, share: f32, range: &std::ops::RangeInclusive<f32>| {
        (len * share).round().clamp(*range.start(), *range.end())
    };
    let wide = window.x >= WIDE_MIN_WIDTH;
    let right = wide.then(|| share(window.x, DISPERSION_SHARE, &DISPERSION_WIDTH));
    let bottom = match (view, wide) {
        (ShotsView::Cards, true) => Some((
            share(window.y, ROW_SHARE, &FLIGHT_HEIGHT),
            BottomRow::Flight,
        )),
        (ShotsView::Cards, false) => Some((
            share(window.y, SHARED_ROW_SHARE, &SHARED_ROW_HEIGHT),
            BottomRow::FlightAndDispersion,
        )),
        (ShotsView::Table, true) => None,
        (ShotsView::Table, false) => Some((
            share(window.y, TABLE_DISPERSION_SHARE, &TABLE_DISPERSION_HEIGHT),
            BottomRow::Dispersion,
        )),
    };
    ChartSlots { right, bottom }
}

/// Widths of Flight and Dispersion in a bottom row `width` wide that they
/// share, the gap between them left out.
pub(super) fn row_split(width: f32) -> (f32, f32) {
    let inner = width - CHART_GAP;
    let slot = (inner * (1.0 - FLIGHT_ROW_SHARE))
        .max(dispersion::ONE_ROW_MIN_WIDTH)
        .min(inner / 2.0)
        .round();
    (inner - slot, slot)
}

/// Height of a bottom row `width` wide holding `row`, from its `height` in
/// [`chart_slots`]: raised by the Dispersion header's extra row, so the
/// plot keeps [`SHARED_PLOT_MIN`] or [`TABLE_PLOT_MIN`].
pub(super) fn bottom_height(height: f32, row: BottomRow, width: f32) -> f32 {
    let (slot, plot) = match row {
        BottomRow::Flight => return height,
        BottomRow::FlightAndDispersion => (row_split(width).1, SHARED_PLOT_MIN),
        BottomRow::Dispersion => (width, TABLE_PLOT_MIN),
    };
    let header = dispersion::header_height(slot);
    height.max(CHART_GAP + header + dispersion::PLOT_PAD + plot)
}

/// Centered placeholder for a view with no shot to show: `headline`, and
/// `detail` under it when given.
pub(in crate::panels) fn empty_state(ui: &mut egui::Ui, headline: &str, detail: Option<&str>) {
    let top = ui.available_height() * 0.36;
    ui.vertical_centered(|ui| {
        ui.add_space(top);
        ui.label(
            egui::RichText::new(headline)
                .size(22.0)
                .color(theme::TEXT_MUTED),
        );
        if let Some(detail) = detail {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(detail)
                    .size(15.0)
                    .color(theme::TEXT_DIM),
            );
        }
    });
}

impl FlighthookApp {
    /// The placeholder of a view with no shot to show: "Waiting for the
    /// first shot" and `detail` while the view is the golfer up; for
    /// anyone else one line, "{name} has no shots yet.", since the next
    /// shot is not theirs.
    pub(crate) fn render_empty_state(&self, ui: &mut egui::Ui, detail: &str) {
        if self.viewing_other() {
            empty_state(ui, &no_shots_yet(&self.viewed_name()), None);
        } else {
            empty_state(ui, WAITING_FOR_FIRST_SHOT, Some(detail));
        }
    }
}

/// The empty state's headline while the view is the golfer up.
pub(crate) const WAITING_FOR_FIRST_SHOT: &str = "Waiting for the first shot";

/// The empty state's line for viewed golfer `name`, who is not up.
pub(crate) fn no_shots_yet(name: &str) -> String {
    format!("{name} has no shots yet.")
}
