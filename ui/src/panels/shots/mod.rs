//! The Shots tab: shared value helpers (picking measured or simulated values, cells,
//! smash), the table's column constants, and the submodules that own each part of the tab.

use std::borrow::Cow;
use std::collections::HashMap;

use super::card_row::{self, CardSource};
use super::{cards, devices, dispersion, impact, signs};
use crate::app::FlighthookApp;
use crate::chart::{self, MembersKey, ShotFilter};
use crate::groups::SetInfo;
use crate::groups::{
    Activity, GROUPING_HINT, GROUPING_MODES, ShotGroups, Strip, club_name, multiple_devices,
};
use crate::net;
use crate::theme;
use crate::types::{
    ActorStatus, ActorStatusResponse, Basis, Club, ClubData, DerivedFlight, Distance, FlightResult,
    Handedness, Launch, ShotGrouping, ShotRow, Stats, UnitSystem, Velocity,
};

/// Format an optional f64 with the given precision, or "-" if None.
fn opt_f(v: Option<f64>, prec: usize) -> String {
    match v {
        Some(v) => format!("{v:.prec$}"),
        None => "-".into(),
    }
}

/// Hover text on every simulated cell.
const SIMULATED_HINT: &str = "Simulated or estimated by flighthook, not measured by the device";

/// Hover text on the JSONL export menu item.
const EXPORT_HINT: &str = "Every shot this session (the last 1000), including shots the device \
     sent no data for, with the raw device frames behind each one. \
     Contains no serial numbers, addresses or network names.";

/// Hover text on the CSV export menu item.
const EXPORT_CSV_HINT: &str = "Decoded shots, one row each. No raw frames.";

/// Hover text on the export icon.
const EXPORT_ICON_HINT: &str = "Export session";

/// Hover text on the Clear history icon.
const CLEAR_HINT: &str = "Clear history";

/// Width of the Clear history confirmation's text.
const CLEAR_CONFIRM_WIDTH: f32 = 440.0;

/// The trash icon at the right end of the navigation's header, as tall as the body text
/// ([`toolbar_icon_height`]): muted, red on hover. Disabled (dimmed, no clicks) when there
/// is no shot to clear.
fn clear_history_button(ui: &mut egui::Ui, enabled: bool, portrait: bool) -> egui::Response {
    let height = toolbar_icon_height(ui, portrait);
    let response = theme::icon(ui, theme::Icon::Trash, height, theme::DANGER, enabled);
    if enabled {
        response.on_hover_text(CLEAR_HINT)
    } else {
        response.on_hover_text("Clear history: no shots yet")
    }
}

/// The export formats the export icon's menu lists, in order: menu label,
/// the `format` of [`net::export_url`], hover text.
const EXPORT_FORMATS: [(&str, &str, &str); 2] = [
    ("JSONL", "jsonl", EXPORT_HINT),
    ("CSV", "csv", EXPORT_CSV_HINT),
];

/// The shot list's toolbar icons' height: the body text's, a touch target
/// in the portrait layout (the glyph keeps its size).
fn toolbar_icon_height(ui: &egui::Ui, portrait: bool) -> f32 {
    if portrait {
        crate::topbar::TOUCH_TARGET
    } else {
        ui.text_style_height(&egui::TextStyle::Body)
    }
}

/// The export icon left of the trash icon, as tall as it: muted, accent on
/// hover. It opens a menu of the export formats ([`EXPORT_FORMATS`]);
/// picking one downloads the whole session in `units`.
fn export_menu(ui: &mut egui::Ui, units: UnitSystem, portrait: bool) {
    let height = toolbar_icon_height(ui, portrait);
    let icon = theme::icon(ui, theme::Icon::Export, height, theme::ACCENT, true);
    devices::mark(ui, || "export".to_owned(), &icon);
    let icon = icon.on_hover_text(EXPORT_ICON_HINT);
    egui::Popup::menu(&icon).show(|ui| {
        if portrait {
            crate::topbar::touch_menu(ui);
        }
        for (label, format, hint) in EXPORT_FORMATS {
            let item = ui.button(label).on_hover_text(hint);
            devices::mark(ui, || format!("export:{format}"), &item);
            if item.clicked() {
                ui.ctx()
                    .open_url(egui::OpenUrl::same_tab(net::export_url(format, units)));
            }
        }
    });
}

/// The Clear history confirmation's question for `n` shots.
fn clear_confirm_title(n: usize, golfers: bool) -> String {
    if golfers {
        "Clear every golfer's shots?".to_owned()
    } else if n == 1 {
        "Delete the 1 shot and its set?".to_owned()
    } else {
        format!("Delete all {n} shots and sets?")
    }
}

/// Number of ball columns: launch, results and spin.
const BALL_COLUMNS: usize = 12;
/// Number of club columns: speed, path, attack, face, loft and smash.
const CLUB_COLUMNS: usize = 6;
/// Number of face impact columns: lateral and vertical.
const IMPACT_COLUMNS: usize = 2;
/// Table columns besides Device: `#`, Club, then the ball, club and face
/// impact columns.
const TABLE_COLUMNS: usize = 2 + BALL_COLUMNS + CLUB_COLUMNS + IMPACT_COLUMNS;
/// Narrowest a table column gets.
const TABLE_MIN_COLUMN: f32 = 48.0;
/// Table row height beyond the body text's.
const TABLE_ROW_PAD: f32 = 2.0;
/// Vertical gap between table rows.
const TABLE_ROW_GAP: f32 = 6.0;
/// Horizontal gap between table columns.
const TABLE_COLUMN_GAP: f32 = 14.0;

/// How long the shot cards glow after a new shot arrives, in seconds.
const PULSE_SECS: f64 = 1.5;
/// Left inset of the table's first column, leaving room for the latest-row
/// stripe.
const ROW_INSET: f32 = 8.0;
/// Width of the latest row's gold stripe on the table's left edge.
const STRIPE_WIDTH: f32 = 3.0;
// The first column's inset clears the stripe with room to spare.
const _: () = assert!(ROW_INSET > STRIPE_WIDTH + 2.0);

/// The table columns that can come first in a row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LeadColumn {
    Device,
    Number,
}

/// Left inset of `column` in the table: `ROW_INSET` for the first column
/// shown (Device when `show_device`, else `#`), so the latest row's stripe
/// never covers its text; 0 otherwise.
fn column_inset(column: LeadColumn, show_device: bool) -> f32 {
    let first = if show_device {
        LeadColumn::Device
    } else {
        LeadColumn::Number
    };
    if column == first { ROW_INSET } else { 0.0 }
}

/// Add a table cell with `add`, `inset` points from the column's left edge.
fn inset_cell(ui: &mut egui::Ui, inset: f32, add: impl FnOnce(&mut egui::Ui)) {
    if inset > 0.0 {
        ui.horizontal(|ui| {
            ui.add_space(inset);
            add(ui);
        });
    } else {
        add(ui);
    }
}

/// The value a shot displays for one field, and whether it is simulated:
/// the simulated `derived` value, else the device's `measured` one. The
/// server sets a field in both only when the simulated value is the one to
/// show (`[simulation] mode = "force"`, a measured 0 carry or flight
/// time on a shot that flies, a ground ball's 0 carry, apex and flight
/// time). Every measured-or-simulated value on the tab (cards, shot list,
/// table, range bars and their stats, Flight and Dispersion) is picked here.
pub(super) fn pick<T>(measured: Option<T>, derived: Option<T>) -> Option<(T, bool)> {
    derived
        .map(|v| (v, true))
        .or_else(|| measured.map(|v| (v, false)))
}

/// A ball's spin `rpm` (backspin or sidespin) like a value picked by
/// [`pick`]: shown as simulated when flighthook estimated it
/// ([`DerivedFlight::spin_estimated`]). The value itself is always the
/// ball's.
fn spin(rpm: Option<i32>, d: &DerivedFlight) -> Option<(f64, bool)> {
    rpm.map(|v| (f64::from(v), d.spin_estimated))
}

/// Render a value picked by [`pick`]: a measured value plainly, a simulated
/// one in italics, else "-". `signed` prints an explicit `+` (offlines,
/// roll).
fn cell(ui: &mut egui::Ui, value: Option<(f64, bool)>, prec: usize, signed: bool) {
    match value {
        Some((v, false)) => {
            ui.label(fmt_num(v, prec, signed));
        }
        Some((v, true)) => {
            ui.label(egui::RichText::new(fmt_num(v, prec, signed)).italics())
                .on_hover_text(SIMULATED_HINT);
        }
        None => {
            ui.label("-");
        }
    }
}

fn fmt_num(v: f64, prec: usize, signed: bool) -> String {
    if signed {
        format!("{v:+.prec$}")
    } else {
        format!("{v:.prec$}")
    }
}

/// `v` rounded to `prec` decimals, as [`fmt_num`] shows it.
fn round_to(v: f64, prec: usize) -> f64 {
    let scale = 10_f64.powi(i32::try_from(prec).unwrap_or(0));
    (v * scale).round() / scale
}

/// Smash factor for display: the device's own, else ball speed over club
/// speed when both are known and the club speed is positive.
fn smash(reported: Option<f64>, ball: Option<Velocity>, club: Option<Velocity>) -> Option<f64> {
    reported.or_else(|| {
        let (ball, club) = (ball?.as_mph(), club?.as_mph());
        (club > 0.0).then(|| ball / club)
    })
}

mod list;
mod metric_cards;
mod panel;
mod range;
mod selection;
mod table;
#[cfg(test)]
mod tests;
mod views;

pub(crate) use list::*;
pub(crate) use metric_cards::*;
pub(crate) use panel::*;
pub(crate) use range::*;
pub(crate) use selection::*;
use table::*;
pub(crate) use views::*;
