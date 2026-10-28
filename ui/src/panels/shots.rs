use std::borrow::Cow;
use std::collections::HashMap;

use super::dispersion;
use crate::app::FlighthookApp;
use crate::chart::{self, MembersKey};
use crate::groups::{
    Activity, GROUPING_HINT, GROUPING_MODES, ShotGroups, Strip, club_name, multiple_devices,
};
use crate::net;
use crate::theme;
use crate::types::{
    ActorStatusResponse, BallFlight, Club, ClubData, DerivedFlight, Distance, ShotGroupInfo,
    ShotGrouping, ShotRow, Stats, UnitSystem, Velocity,
};

/// Format an optional f64 with the given precision, or "-" if None.
fn opt_f(v: Option<f64>, prec: usize) -> String {
    match v {
        Some(v) => format!("{v:.prec$}"),
        None => "-".into(),
    }
}

/// Hover text on every simulated cell.
const SIMULATED_HINT: &str = "Simulated, not measured by the device";

/// Hover text on the JSONL export link.
const EXPORT_HINT: &str = "Every shot this session (the last 1000), including shots the device \
     sent no data for, with the raw device frames behind each one. \
     Contains no serial numbers, addresses or network names.";

/// Hover text on the Clear history icon.
const CLEAR_HINT: &str = "Clear history";

/// Width of the Clear history confirmation's text.
const CLEAR_CONFIRM_WIDTH: f32 = 440.0;

/// The trash icon after the export links, as tall as they are: muted, red
/// on hover. Disabled (dimmed, no clicks) when there is no shot to clear.
fn clear_history_button(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    let height = ui.text_style_height(&egui::TextStyle::Body);
    let response = theme::icon(ui, theme::Icon::Trash, height, theme::DANGER, enabled);
    if enabled {
        response.on_hover_text(CLEAR_HINT)
    } else {
        response.on_hover_text("Clear history: no shots yet")
    }
}

/// The Clear history confirmation's question for `n` shots.
fn clear_confirm_title(n: usize) -> String {
    if n == 1 {
        "Delete the 1 shot and its set?".to_owned()
    } else {
        format!("Delete all {n} shots and sets?")
    }
}

/// Number of ball columns: launch, results and spin.
const BALL_COLUMNS: usize = 12;
/// Number of club columns: speed, path, attack, face, loft and smash.
const CLUB_COLUMNS: usize = 6;
/// Table columns besides Device: `#`, Club, then the ball and club columns.
const TABLE_COLUMNS: usize = 2 + BALL_COLUMNS + CLUB_COLUMNS;
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
/// show (`DerivedFlight::replaces_measured`, a measured 0 carry or flight
/// time on a shot that flies, a ground ball's 0 carry, apex and flight
/// time). Every measured-or-simulated value on the tab (cards, shot list,
/// table, range bars and their stats, Flight and Dispersion) is picked here.
pub(super) fn pick<T>(measured: Option<T>, derived: Option<T>) -> Option<(T, bool)> {
    derived
        .map(|v| (v, true))
        .or_else(|| measured.map(|v| (v, false)))
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

/// Smash factor for display: the device's own, else ball speed over club
/// speed when both are known and the club speed is positive.
fn smash(reported: Option<f64>, ball: Option<Velocity>, club: Option<Velocity>) -> Option<f64> {
    reported.or_else(|| {
        let (ball, club) = (ball?.as_mph(), club?.as_mph());
        (club > 0.0).then(|| ball / club)
    })
}

/// Format an optional i32, or "-" if None.
fn opt_i(v: Option<i32>) -> String {
    match v {
        Some(v) => format!("{v}"),
        None => "-".into(),
    }
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// A command on the shot history and its sets, from the Shots tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupCommand {
    NewGroup,
    NewSession,
    ClearHistory,
}

impl GroupCommand {
    /// The WebSocket command's `cmd`.
    fn ws_cmd(self) -> &'static str {
        match self {
            GroupCommand::NewGroup => "new_group",
            GroupCommand::NewSession => "new_session",
            GroupCommand::ClearHistory => "clear_history",
        }
    }
}

/// What the Shots tab shows below its toolbar.
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
    const ALL: &[ShotsView] = &[ShotsView::Cards, ShotsView::Table];

    fn label(self) -> &'static str {
        match self {
            ShotsView::Table => "Table",
            ShotsView::Cards => "Cards",
        }
    }

    /// Whether the view has the shot list down its left-hand side.
    fn shot_list(self) -> bool {
        self == ShotsView::Cards
    }

    /// Whether the toolbar shows the "italic = simulated" legend: only
    /// Table uses italics for simulated values, so only Table needs it.
    fn shows_simulated_legend(self) -> bool {
        self == ShotsView::Table
    }
}

/// A chart of the Shots tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chart {
    /// Side view of the ball flights (`panels::flight`).
    Flight,
    /// Top-down landing and rest positions (`panels::dispersion`).
    Dispersion,
}

/// Windows at least this wide give Dispersion a full-height column on the
/// right; narrower ones put it in the bottom row.
const WIDE_MIN_WIDTH: f32 = 1200.0;
/// Width of the Dispersion column: this share of the window's width,
/// within [`DISPERSION_WIDTH`].
const DISPERSION_SHARE: f32 = 0.24;
const DISPERSION_WIDTH: std::ops::RangeInclusive<f32> = 260.0..=420.0;
/// Height of the bottom row in Cards: [`ROW_SHARE`] of the window's height
/// within [`FLIGHT_HEIGHT`] when it holds Flight alone, and
/// [`SHARED_ROW_SHARE`] within [`SHARED_ROW_HEIGHT`] when Dispersion sits
/// beside it.
const ROW_SHARE: f32 = 0.30;
const FLIGHT_HEIGHT: std::ops::RangeInclusive<f32> = 180.0..=320.0;
const SHARED_ROW_SHARE: f32 = 0.27;
const SHARED_ROW_HEIGHT: std::ops::RangeInclusive<f32> = dispersion_floor(SHARED_PLOT_MIN)..=320.0;
/// Flight's share of a bottom row it shares with Dispersion. Dispersion
/// takes the rest, widened up to half the row so its header keeps to one
/// row.
const FLIGHT_ROW_SHARE: f32 = 0.62;
/// Height of Dispersion under a narrow Table: this share of the window's
/// height, within [`TABLE_DISPERSION_HEIGHT`].
const TABLE_DISPERSION_SHARE: f32 = 0.35;
const TABLE_DISPERSION_HEIGHT: std::ops::RangeInclusive<f32> =
    dispersion_floor(TABLE_PLOT_MIN)..=400.0;
/// Least height of the Dispersion plot (inside its gutters) in the Cards
/// bottom row, where the cards above must still fit at 1024×768, and under
/// a narrow Table.
const SHARED_PLOT_MIN: f32 = 144.0;
const TABLE_PLOT_MIN: f32 = 200.0;
/// Gap between a chart and its neighbour.
const CHART_GAP: f32 = 14.0;

/// Height of a bottom row whose Dispersion plot is `plot` tall under a
/// one-row header.
const fn dispersion_floor(plot: f32) -> f32 {
    CHART_GAP + dispersion::ROW_H + dispersion::PLOT_PAD + plot
}

/// What the Shots tab's bottom row holds, left to right.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BottomRow {
    Flight,
    FlightAndDispersion,
    Dispersion,
}

/// Where the charts sit: both are always shown, sized from the window
/// so nothing moves until the window does.
#[derive(Clone, Copy, PartialEq, Debug)]
struct ChartSlots {
    /// Width of the full-height Dispersion column on the right.
    right: Option<f32>,
    /// Height of the bottom row (under the main content, between the shot
    /// list and the right-hand column) and what it holds.
    bottom: Option<(f32, BottomRow)>,
}

/// The charts' places for `view` in a window of `window` size.
///
/// - Cards, wide: Dispersion in the right-hand column, Flight along the
///   bottom of the centre.
/// - Cards, narrow: Flight and Dispersion side by side in the bottom row,
///   so the cards keep the centre's width.
/// - Table, wide: Dispersion in the right-hand column.
/// - Table, narrow: Dispersion along the bottom.
fn chart_slots(view: ShotsView, window: egui::Vec2) -> ChartSlots {
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
fn row_split(width: f32) -> (f32, f32) {
    let inner = width - CHART_GAP;
    let slot = (inner * (1.0 - FLIGHT_ROW_SHARE))
        .max(dispersion::ONE_ROW_MIN_WIDTH)
        .min(inner / 2.0)
        .round();
    (inner - slot, slot)
}

/// Height of a bottom row `width` wide holding `row`, from its `height` in
/// [`chart_slots`]: raised by the Dispersion header's extra rows, so the
/// plot keeps [`SHARED_PLOT_MIN`] or [`TABLE_PLOT_MIN`]. `filter` when the
/// header carries the device filter.
fn bottom_height(height: f32, row: BottomRow, width: f32, filter: bool) -> f32 {
    let (slot, plot) = match row {
        BottomRow::Flight => return height,
        BottomRow::FlightAndDispersion => (row_split(width).1, SHARED_PLOT_MIN),
        BottomRow::Dispersion => (width, TABLE_PLOT_MIN),
    };
    let header = dispersion::header_height(slot, filter);
    height.max(CHART_GAP + header + dispersion::PLOT_PAD + plot)
}

/// Centred placeholder for a view with no shot to show.
fn empty_state(ui: &mut egui::Ui, detail: &str) {
    let top = ui.available_height() * 0.36;
    ui.vertical_centered(|ui| {
        ui.add_space(top);
        ui.label(
            egui::RichText::new("Waiting for the first shot")
                .size(22.0)
                .color(theme::TEXT_MUTED),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(detail)
                .size(15.0)
                .color(theme::TEXT_DIM),
        );
    });
}

// ---------------------------------------------------------------------------
// Selected shot and new-shot pulse
// ---------------------------------------------------------------------------

/// Which shot the Cards view shows, and when the latest shot arrived so the
/// cards (and the table's latest row) can glow briefly for it.
///
/// The view follows the latest shot unless one is pinned from the shot
/// list. A new latest shot clears the pin; so does the pinned shot leaving
/// the list.
#[derive(Default)]
pub(crate) struct ShotFocus {
    /// The latest shot (the newest one with data).
    latest: Option<ShotRef>,
    /// The shot picked in the shot list.
    pinned: Option<ShotRef>,
    started: Option<f64>,
    /// The list should scroll the shot on show into view.
    reveal: bool,
}

impl ShotFocus {
    /// Note the latest shot; a new one starts a pulse at `now` and becomes
    /// the shot on show.
    pub(crate) fn observe(&mut self, shots: &[ShotRow], now: f64) {
        let latest = latest_ref(shots);
        if latest != self.latest {
            self.started = latest.is_some().then_some(now);
            self.latest = latest;
            self.follow();
        }
        self.drop_missing_pin(shots);
    }

    /// Note the latest shot without pulsing or moving off a pinned shot
    /// that is still listed (bulk loads).
    pub(crate) fn sync(&mut self, shots: &[ShotRow]) {
        self.latest = latest_ref(shots);
        self.started = None;
        self.drop_missing_pin(shots);
    }

    fn drop_missing_pin(&mut self, shots: &[ShotRow]) {
        if self
            .pinned
            .as_ref()
            .is_some_and(|pin| !shots.iter().any(|s| pin.matches(s)))
        {
            self.pinned = None;
        }
    }

    /// Show `shots[i]`; picking the latest shot follows new shots again.
    pub(crate) fn select(&mut self, shots: &[ShotRow], i: usize) {
        if Some(i) == latest_index(shots) {
            self.follow();
        } else {
            self.pinned = shots.get(i).map(ShotRef::of);
        }
    }

    /// Show the latest shot, and every new one as it arrives.
    fn follow(&mut self) {
        self.pinned = None;
        self.reveal = true;
    }

    /// Index of the shot on show.
    pub(super) fn index(&self, shots: &[ShotRow]) -> Option<usize> {
        focus_index(shots, self.pinned.as_ref())
    }

    /// Whether the view is following the latest shot.
    pub(super) fn following(&self, shots: &[ShotRow]) -> bool {
        self.index(shots) == latest_index(shots)
    }

    /// Scroll the shot on show into view in the list, once.
    pub(super) fn reveal(&mut self) {
        self.reveal = true;
    }

    /// Whether the list should scroll the shot on show into view; true once
    /// per jump.
    fn take_reveal(&mut self) -> bool {
        std::mem::take(&mut self.reveal)
    }

    /// How far the latest shot's pulse has run at `now`: 0 when the shot
    /// arrives, rising linearly to 1 over `PULSE_SECS`. `None` when no pulse
    /// is running: none started (no shot yet, or a bulk load) or it is over.
    pub(crate) fn progress(&self, now: f64) -> Option<f32> {
        let start = self.started?;
        let t = ((now - start) / PULSE_SECS).max(0.0);
        (t < 1.0).then_some(t as f32)
    }

    /// Pulse strength at `now`: 1 when the shot arrives, easing to 0.
    fn strength(&self, now: f64) -> f32 {
        self.progress(now).map_or(0.0, |t| (1.0 - t).powi(3))
    }
}

/// Index of the latest shot: the newest one with any data, so a
/// just-triggered shot does not blank the cards while its data is in flight.
pub(super) fn latest_index(shots: &[ShotRow]) -> Option<usize> {
    shots
        .iter()
        .rposition(|s| s.ball.is_some() || s.club.is_some())
}

/// The chip on the shot header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderChip {
    /// The latest shot is shown.
    Latest,
    /// An older shot is pinned.
    BackToLatest,
    None,
}

/// The chip for shot `i` in the header: Back to latest while not
/// `following`, else LATEST when `i` is the latest shot with data (a
/// trigger whose data has not landed does not take it).
fn header_chip(shots: &[ShotRow], i: usize, following: bool) -> HeaderChip {
    if !following {
        HeaderChip::BackToLatest
    } else if latest_index(shots) == Some(i) {
        HeaderChip::Latest
    } else {
        HeaderChip::None
    }
}

fn latest_ref(shots: &[ShotRow]) -> Option<ShotRef> {
    latest_index(shots).map(|i| ShotRef::of(&shots[i]))
}

/// A shot that is found again after the list is reloaded: by `shot_id` (and
/// the actor, when both sides name one) when both sides have an id, else by
/// actor and shot number.
#[derive(Clone, PartialEq, Eq, Debug)]
struct ShotRef {
    actor: String,
    number: u32,
    id: String,
}

impl ShotRef {
    fn of(shot: &ShotRow) -> Self {
        Self {
            actor: shot.actor.clone(),
            number: shot.shot_number,
            id: shot.shot_id.clone(),
        }
    }

    fn matches(&self, shot: &ShotRow) -> bool {
        crate::groups::same_shot(
            (&self.actor, &self.id, self.number),
            (&shot.actor, &shot.shot_id, shot.shot_number),
        )
    }
}

/// Index of the shot to show: the `pinned` one while it is listed (the
/// newest match), else the latest.
fn focus_index(shots: &[ShotRow], pinned: Option<&ShotRef>) -> Option<usize> {
    pinned
        .and_then(|pin| shots.iter().rposition(|s| pin.matches(s)))
        .or_else(|| latest_index(shots))
}

// ---------------------------------------------------------------------------
// Cards
// ---------------------------------------------------------------------------

/// Where a card's value came from.
#[derive(Clone, Copy, PartialEq)]
enum Source {
    Measured,
    Simulated,
    Missing,
}

/// One metric card.
struct Metric {
    label: &'static str,
    unit: &'static str,
    text: String,
    /// A typically wide value of this metric, used to size the numerals.
    sample: &'static str,
    source: Source,
}

impl Metric {
    /// A metric showing a value picked by [`pick`] (measured or
    /// simulated), else missing.
    fn new(
        label: &'static str,
        unit: &'static str,
        value: Option<(f64, bool)>,
        prec: usize,
        signed: bool,
    ) -> Self {
        let (text, source) = match value {
            Some((v, false)) => (fmt_num(v, prec, signed), Source::Measured),
            Some((v, true)) => (fmt_num(v, prec, signed), Source::Simulated),
            None => ("\u{2013}".into(), Source::Missing),
        };
        let sample = match (prec, signed) {
            (0, false) => "8888",
            (0, true) => "+888",
            (1, false) => "888.8",
            (1, true) => "+88.8",
            (_, false) => "8.88",
            (_, true) => "+8.88",
        };
        Self {
            label,
            unit,
            text,
            sample,
            source,
        }
    }

    fn measured(label: &'static str, unit: &'static str, v: Option<f64>, prec: usize) -> Self {
        Self::new(label, unit, v.map(|v| (v, false)), prec, false)
    }

    fn signed(label: &'static str, unit: &'static str, v: Option<f64>, prec: usize) -> Self {
        Self::new(label, unit, v.map(|v| (v, false)), prec, true)
    }
}

/// A titled group of detail cards.
type MetricGroup = (&'static str, Vec<Metric>);

/// The Table view's column unit labels: speed, distance, height. `max_height`
/// converts to feet in imperial (`ShotData::to_imperial`), unlike every other
/// distance field, which converts to yards.
fn table_header_units(imperial: bool) -> (&'static str, &'static str, &'static str) {
    if imperial {
        ("mph", "yd", "ft")
    } else {
        ("m/s", "m", "m")
    }
}

/// The Cards view's metrics for a shot already converted to the display unit
/// system: the headline row, then the Ball, Flight and Club groups.
fn card_metrics(shot: &ShotRow, imperial: bool) -> (Vec<Metric>, [MetricGroup; 3]) {
    let (speed, dist) = if imperial {
        ("mph", "yd")
    } else {
        ("m/s", "m")
    };
    let height = if imperial { "ft" } else { "m" };
    // Borrowed: this runs per card per frame.
    let (no_ball, no_club, no_derived) = Default::default();
    let ball: &BallFlight = shot.ball.as_ref().unwrap_or(&no_ball);
    let club: &ClubData = shot.club.as_ref().unwrap_or(&no_club);
    let d: &DerivedFlight = shot.derived.as_ref().unwrap_or(&no_derived);
    let v = |x: Option<Distance>| x.map(|x| x.value());
    let rpm = |x: Option<i32>| x.map(f64::from);

    let headline = vec![
        Metric::measured("Ball speed", speed, ball.launch_speed.map(|v| v.value()), 1),
        Metric::new(
            "Carry",
            dist,
            pick(v(ball.carry_distance), v(d.carry_distance)),
            1,
            false,
        ),
        Metric::new(
            "Total",
            dist,
            pick(v(ball.total_distance), v(d.total_distance)),
            1,
            false,
        ),
        Metric::measured("Launch", "\u{b0}", ball.launch_elevation, 1),
        Metric::measured("Backspin", "rpm", rpm(ball.backspin_rpm), 0),
    ];
    let groups = [
        (
            "Ball",
            vec![
                Metric::signed("Direction", "\u{b0}", ball.launch_azimuth, 1),
                Metric::signed("Sidespin", "rpm", rpm(ball.sidespin_rpm), 0),
            ],
        ),
        (
            "Flight",
            vec![
                Metric::new(
                    "Apex",
                    height,
                    pick(v(ball.max_height), v(d.max_height)),
                    1,
                    false,
                ),
                Metric::new(
                    "Offline",
                    dist,
                    pick(v(ball.carry_offline), v(d.carry_offline)),
                    1,
                    true,
                ),
                Metric::new(
                    "Descent",
                    "\u{b0}",
                    pick(ball.descent_angle, d.descent_angle),
                    1,
                    false,
                ),
                Metric::new(
                    "Roll",
                    dist,
                    pick(v(ball.roll_distance), v(d.roll_distance)),
                    1,
                    true,
                ),
            ],
        ),
        (
            "Club",
            vec![
                Metric::measured("Club speed", speed, club.club_speed.map(|v| v.value()), 1),
                Metric::measured(
                    "Smash",
                    "",
                    smash(club.smash_factor, ball.launch_speed, club.club_speed),
                    2,
                ),
                Metric::signed("Path", "\u{b0}", club.path, 1),
                Metric::signed("AoA", "\u{b0}", club.attack_angle, 1),
                Metric::signed("Face", "\u{b0}", club.face_angle, 1),
                Metric::measured("Loft", "\u{b0}", club.dynamic_loft, 1),
            ],
        ),
    ];
    (headline, groups)
}

/// Gap between neighbouring cards in a row.
const CARD_GAP: f32 = 12.0;
/// Gap between detail groups side by side.
const GROUP_GAP: f32 = 28.0;
const HEADLINE_MIN_WIDTH: f32 = 120.0;
/// Narrowest detail card: room for an 18 px numeral with its unit, and for
/// the label beside a `SIM` tag.
const DETAIL_MIN_WIDTH: f32 = 96.0;
/// Detail card height limit; headline cards grow `HEADLINE_RATIO` as fast.
const DETAIL_MAX_HEIGHT: f32 = 136.0;
const HEADLINE_RATIO: f32 = 1.6;
/// Tallest a detail card gets, as a fraction of the headline cards' height.
const DETAIL_HEADLINE_RATIO: f32 = 0.8;
/// Tallest a card gets, as a fraction of its width.
const MAX_ASPECT: f32 = 0.75;
/// Card rows of each detail group when the groups sit side by side.
const DETAIL_ROWS: usize = 2;
/// Cards per row of each detail group when the groups share two columns.
const PAIRED_COLS: usize = 2;
/// Share of the spare height that goes above the cards; the rest is below.
const TOP_SLACK: f32 = 0.35;
/// Space kept free under the last card row, on top of the glow inset, so
/// the cards clear the panel below by a chart gap, scrolled or not.
const CARDS_FOOT: f32 = CHART_GAP - GLOW_WIDTH as f32;
/// Smallest numerals: headline and detail cards.
const HEADLINE_VALUE_MIN: f32 = 28.0;
const DETAIL_VALUE_MIN: f32 = 18.0;

/// The Cards view's vertical sizes at one density, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rhythm {
    /// The shot header line and the space under it.
    header: f32,
    header_gap: f32,
    /// Headline card height.
    headline: f32,
    /// Gap between the headline cards and the range bars' control row,
    /// then that row.
    controls_gap: f32,
    controls: f32,
    /// A label line: the range bars' title, a detail group's title.
    label: f32,
    /// How far a legend row of its own tucks up into the label line above.
    legend_overlap: f32,
    /// One range bar row and the gap between rows.
    bar_row: f32,
    bar_gap: f32,
    /// Gap above the detail groups, and between groups stacked in a column.
    group_gap: f32,
    /// Gap between card rows.
    row_gap: f32,
    /// Detail card height.
    detail: f32,
}

/// The sizes with room to spare. Cards grow from here with the height.
const ROOMY: Rhythm = Rhythm {
    header: 34.0,
    header_gap: 14.0,
    headline: 84.0 * HEADLINE_RATIO,
    controls_gap: 8.0,
    controls: 28.0,
    label: 28.0,
    legend_overlap: 8.0,
    bar_row: 30.0,
    bar_gap: 8.0,
    group_gap: 28.0,
    row_gap: 12.0,
    detail: 84.0,
};

/// The smallest sizes. Short of room the view shrinks every size of
/// [`ROOMY`] towards these by the same share; below them it scrolls.
const COMPACT: Rhythm = Rhythm {
    header: 28.0,
    header_gap: 6.0,
    headline: 68.0,
    controls_gap: 4.0,
    controls: 24.0,
    label: 20.0,
    legend_overlap: 4.0,
    bar_row: 26.0,
    bar_gap: 4.0,
    group_gap: 14.0,
    row_gap: 8.0,
    detail: 52.0,
};

impl Rhythm {
    /// `t` of the way from [`COMPACT`] (0) to [`ROOMY`] (1).
    fn between(t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let l = |a: f32, b: f32| a + (b - a) * t;
        let (a, b) = (COMPACT, ROOMY);
        Self {
            header: l(a.header, b.header),
            header_gap: l(a.header_gap, b.header_gap),
            headline: l(a.headline, b.headline),
            controls_gap: l(a.controls_gap, b.controls_gap),
            controls: l(a.controls, b.controls),
            label: l(a.label, b.label),
            legend_overlap: l(a.legend_overlap, b.legend_overlap),
            bar_row: l(a.bar_row, b.bar_row),
            bar_gap: l(a.bar_gap, b.bar_gap),
            group_gap: l(a.group_gap, b.group_gap),
            row_gap: l(a.row_gap, b.row_gap),
            detail: l(a.detail, b.detail),
        }
    }

    /// Height of the range bar section with its legend placed `legend`:
    /// the control row, the label line (and the legend's own row), and
    /// three bar rows.
    fn bars(&self, legend: Legend) -> f32 {
        let legend_row = match legend {
            Legend::Beside => 0.0,
            Legend::Below => self.label - self.legend_overlap,
        };
        self.controls + self.label + legend_row + 3.0 * self.bar_row + 2.0 * self.bar_gap
    }
}

/// Positions in the Cards view, relative to its top-left corner.
#[derive(Debug)]
struct CardsLayout {
    header: egui::Rect,
    headline: Vec<egui::Rect>,
    /// The range bar section under the headline row.
    bars: Option<BarsLayout>,
    /// Per detail group: its label line, then its cards.
    groups: Vec<(egui::Rect, Vec<egui::Rect>)>,
    /// Bottom of the last card.
    height: f32,
    /// The cards don't fit the height even at [`COMPACT`] sizes.
    scrolls: bool,
}

/// Where the range bars' legend goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Legend {
    /// On the label line, right of the title.
    Beside,
    /// On its own row under the title, when the line can't hold both.
    Below,
}

/// Positions in the range bar section, top to bottom.
#[derive(Debug, Clone, Copy)]
struct BarsLayout {
    /// The device filter's row.
    controls: egui::Rect,
    /// The label line: the title, and the legend when it sits beside it.
    label: egui::Rect,
    /// The legend's line: `label` itself, or a row of its own under it.
    legend: egui::Rect,
    /// One row per bar.
    rows: [egui::Rect; 3],
}

impl BarsLayout {
    fn translate(self, by: egui::Vec2) -> Self {
        Self {
            controls: self.controls.translate(by),
            label: self.label.translate(by),
            legend: self.legend.translate(by),
            rows: self.rows.map(|r| r.translate(by)),
        }
    }
}

/// A column of the detail section: detail groups stacked top to bottom,
/// each `cols` cards wide.
#[derive(Debug, PartialEq)]
struct DetailColumn {
    groups: std::ops::Range<usize>,
    cols: usize,
}

/// What a detail column stacks: card rows, label lines, gaps between card
/// rows of a group and gaps between groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Extent {
    rows: usize,
    labels: usize,
    row_gaps: usize,
    group_gaps: usize,
}

impl Extent {
    /// Height at sizes `r` with `detail`-tall cards.
    fn height(&self, r: &Rhythm, detail: f32) -> f32 {
        self.rows as f32 * detail
            + self.labels as f32 * r.label
            + self.row_gaps as f32 * r.row_gap
            + self.group_gaps as f32 * r.group_gap
    }
}

impl DetailColumn {
    /// What the column stacks for `counts` cards per group.
    fn extent(&self, counts: &[usize]) -> Extent {
        let rows: Vec<usize> = counts[self.groups.clone()]
            .iter()
            .map(|n| n.div_ceil(self.cols))
            .collect();
        let card_rows: usize = rows.iter().sum();
        let labels = rows.len();
        Extent {
            rows: card_rows,
            labels,
            row_gaps: card_rows - labels.min(card_rows),
            group_gaps: labels.saturating_sub(1),
        }
    }
}

/// How the detail groups are arranged in width `w`, with the card width:
/// every group side by side in `DETAIL_ROWS` rows; else in two columns of
/// `PAIRED_COLS`-wide groups, split to balance their heights; else
/// stacked in one column, as wide as fits. The first arrangement that
/// leaves each card `DETAIL_MIN_WIDTH` wins.
fn detail_columns(w: f32, counts: &[usize]) -> (Vec<DetailColumn>, f32) {
    let card_w = |columns: &[DetailColumn]| {
        let cards: usize = columns.iter().map(|c| c.cols).sum();
        let n = columns.len() as f32;
        (w - CARD_GAP * (cards as f32 - n) - GROUP_GAP * (n - 1.0).max(0.0)) / cards.max(1) as f32
    };

    let side: Vec<DetailColumn> = counts
        .iter()
        .enumerate()
        .map(|(i, n)| DetailColumn {
            groups: i..i + 1,
            cols: n.div_ceil(DETAIL_ROWS).max(1),
        })
        .collect();
    let side_w = card_w(&side);
    if side_w >= DETAIL_MIN_WIDTH {
        return (side, side_w);
    }

    // The contiguous split into two columns with the fewest card rows in
    // the taller one, then the fewest label lines.
    let paired = (1..counts.len())
        .map(|at| {
            [
                DetailColumn {
                    groups: 0..at,
                    cols: PAIRED_COLS,
                },
                DetailColumn {
                    groups: at..counts.len(),
                    cols: PAIRED_COLS,
                },
            ]
        })
        .min_by_key(|pair| {
            pair.iter()
                .map(|c| {
                    let e = c.extent(counts);
                    (e.rows, e.labels)
                })
                .max()
        });
    if let Some(pair) = paired {
        let pair = Vec::from(pair);
        let pair_w = card_w(&pair);
        if pair_w >= DETAIL_MIN_WIDTH {
            return (pair, pair_w);
        }
    }

    let widest = counts.iter().copied().max().unwrap_or(1).max(1);
    let fit = ((w + CARD_GAP) / (DETAIL_MIN_WIDTH + CARD_GAP))
        .floor()
        .max(1.0) as usize;
    let stacked = vec![DetailColumn {
        groups: 0..counts.len(),
        cols: fit.min(widest),
    }];
    let stacked_w = card_w(&stacked);
    (stacked, stacked_w)
}

/// Lay out the Cards view in `size`: the shot header, `headline` cards in
/// balanced rows, the range bar section when `bars` (with its legend placed
/// as given), then detail groups of `groups[i]` cards arranged by
/// `detail_columns`.
///
/// Everything fits the height whenever it can. With room to spare, the
/// sizes are [`ROOMY`], the cards grow with the height up to a limit, and
/// what is still spare is split above and below. Short of room, every
/// height (cards, bar rows, label lines and gaps) shrinks towards
/// [`COMPACT`] by the same share, just enough to fit. Shorter still, the
/// sizes stay [`COMPACT`] and `scrolls` is set.
fn cards_layout(
    size: egui::Vec2,
    headline: usize,
    groups: &[usize],
    bars: Option<Legend>,
) -> CardsLayout {
    use egui::{Rect, pos2, vec2};

    let w = size.x;
    let fit = |min: f32| ((w + CARD_GAP) / (min + CARD_GAP)).floor().max(1.0) as usize;
    let span = |n: usize, card: f32, gap: f32| card * n as f32 + gap * (n as f32 - 1.0);

    let headline = headline.max(1);
    let head_rows = headline.div_ceil(fit(HEADLINE_MIN_WIDTH).min(headline));
    let head_cols = headline.div_ceil(head_rows);
    let head_w = (w - CARD_GAP * (head_cols as f32 - 1.0)) / head_cols as f32;

    let (columns, detail_w) = detail_columns(w, groups);
    let extents: Vec<Extent> = columns.iter().map(|c| c.extent(groups)).collect();

    // Card heights at sizes `r`: no taller than their width allows, detail
    // cards shorter than the headline cards, never under COMPACT.
    let cards = |r: &Rhythm| {
        let head = r.headline.min(head_w * MAX_ASPECT).max(COMPACT.headline);
        let detail = r
            .detail
            .min(detail_w * MAX_ASPECT)
            .min(head * DETAIL_HEADLINE_RATIO)
            .max(COMPACT.detail);
        (head, detail)
    };
    // Everything above the detail groups, at sizes `r` with `head`-tall
    // headline cards.
    let above = |r: &Rhythm, head: f32| {
        r.header
            + r.header_gap
            + span(head_rows, head, r.row_gap)
            + bars.map_or(0.0, |legend| r.controls_gap + r.bars(legend))
            + r.group_gap
    };
    let total = |r: &Rhythm| {
        let (head, detail) = cards(r);
        let tallest = extents
            .iter()
            .map(|e| e.height(r, detail))
            .fold(0.0, f32::max);
        above(r, head) + tallest
    };

    let (r, head_h, detail_h) = if total(&ROOMY) <= size.y {
        // Grow the cards into the spare height. The tallest column (most
        // card rows, then the most label and gap height) sets the
        // detail section's height.
        let tallest = extents.iter().copied().max_by(|a, b| {
            (a.rows, a.height(&ROOMY, 0.0))
                .partial_cmp(&(b.rows, b.height(&ROOMY, 0.0)))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let tallest = tallest.unwrap_or(Extent {
            rows: 0,
            labels: 0,
            row_gaps: 0,
            group_gaps: 0,
        });
        let fixed = above(&ROOMY, 0.0) + tallest.height(&ROOMY, 0.0);
        let grow = ((size.y - fixed) / (HEADLINE_RATIO * head_rows as f32 + tallest.rows as f32))
            .clamp(ROOMY.detail, DETAIL_MAX_HEIGHT);
        let grown = Rhythm {
            headline: grow * HEADLINE_RATIO,
            detail: grow,
            ..ROOMY
        };
        let (head, detail) = cards(&grown);
        (ROOMY, head, detail)
    } else {
        // The largest share that fits: the total only grows with it.
        let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if total(&Rhythm::between(mid)) <= size.y {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let r = Rhythm::between(lo);
        let (head, detail) = cards(&r);
        (r, head, detail)
    };
    let content = {
        let tallest = extents
            .iter()
            .map(|e| e.height(&r, detail_h))
            .fold(0.0, f32::max);
        above(&r, head_h) + tallest
    };
    // A hair of float error is not worth a scroll bar.
    let scrolls = content > size.y + 0.5;
    let top = ((size.y - content).max(0.0) * TOP_SLACK).floor();

    let header = Rect::from_min_size(pos2(0.0, top), vec2(w, r.header));
    let y0 = header.bottom() + r.header_gap;
    let headline = (0..headline)
        .map(|i| {
            let (row, col) = (i / head_cols, i % head_cols);
            Rect::from_min_size(
                pos2(
                    col as f32 * (head_w + CARD_GAP),
                    y0 + row as f32 * (head_h + r.row_gap),
                ),
                vec2(head_w, head_h),
            )
        })
        .collect();
    let head_bottom = y0 + span(head_rows, head_h, r.row_gap);
    let bars_top = head_bottom + r.controls_gap;
    let bars = bars.map(|place| {
        let controls = Rect::from_min_size(pos2(0.0, bars_top), vec2(w, r.controls));
        let label = Rect::from_min_size(pos2(0.0, controls.bottom()), vec2(w, r.label));
        let legend = match place {
            Legend::Beside => label,
            Legend::Below => Rect::from_min_size(
                pos2(0.0, label.bottom() - r.legend_overlap),
                vec2(w, r.label),
            ),
        };
        let top = legend.bottom().max(label.bottom());
        let rows = std::array::from_fn(|i| {
            Rect::from_min_size(
                pos2(0.0, top + i as f32 * (r.bar_row + r.bar_gap)),
                vec2(w, r.bar_row),
            )
        });
        BarsLayout {
            controls,
            label,
            legend,
            rows,
        }
    });
    let detail_top = bars.map_or(head_bottom, |b| b.rows[2].bottom()) + r.group_gap;

    let mut out = Vec::with_capacity(groups.len());
    let mut x = 0.0;
    for column in &columns {
        let c = column.cols;
        let mut y = detail_top;
        for &n in &groups[column.groups.clone()] {
            let label = Rect::from_min_size(pos2(x, y), vec2(span(c, detail_w, CARD_GAP), r.label));
            let top = label.bottom();
            let cards: Vec<egui::Rect> = (0..n)
                .map(|i| {
                    let (row, col) = (i / c, i % c);
                    Rect::from_min_size(
                        pos2(
                            x + col as f32 * (detail_w + CARD_GAP),
                            top + row as f32 * (detail_h + r.row_gap),
                        ),
                        vec2(detail_w, detail_h),
                    )
                })
                .collect();
            out.push((label, cards));
            y = top + span(n.div_ceil(c), detail_h, r.row_gap) + r.group_gap;
        }
        x += span(c, detail_w, CARD_GAP) + GROUP_GAP;
    }
    let height = out
        .iter()
        .flat_map(|(_, cards)| cards.iter().map(|r| r.bottom()))
        .fold(detail_top, f32::max);

    CardsLayout {
        header,
        headline,
        bars,
        groups: out,
        height,
        scrolls,
    }
}

/// Size-dependent styling of a card.
#[derive(Clone, Copy)]
struct CardLook {
    radius: u8,
    pad: f32,
    label: f32,
    /// Numeral font size.
    value: f32,
}

/// How far `height` is from `compact` (0) to `roomy` (1).
fn density(height: f32, compact: f32, roomy: f32) -> f32 {
    ((height - compact) / (roomy - compact)).clamp(0.0, 1.0)
}

/// `t` of the way from `a` to `b`.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

impl CardLook {
    /// Narrow cards get less padding, leaving more room for the numeral;
    /// short ones get less padding and a smaller label.
    fn headline(size: egui::Vec2) -> Self {
        let d = density(size.y, COMPACT.headline, ROOMY.headline);
        let pad = if size.x < 180.0 { 14.0 } else { 18.0 };
        Self {
            radius: theme::CARD_RADIUS + 4,
            pad: lerp(12.0, pad, d),
            label: lerp(12.0, 13.5, d),
            value: (size.y * 0.42).clamp(HEADLINE_VALUE_MIN, 96.0),
        }
    }

    /// As [`CardLook::headline`], for detail cards.
    fn detail(size: egui::Vec2) -> Self {
        let d = density(size.y, COMPACT.detail, ROOMY.detail);
        let pad = if size.x < 130.0 { 10.0 } else { 14.0 };
        Self {
            radius: theme::CARD_RADIUS,
            pad: lerp(8.0, pad, d),
            label: lerp(10.0, 11.5, d),
            value: (size.y * 0.32).clamp(DETAIL_VALUE_MIN, 46.0),
        }
    }

    /// Shrink the numeral so every metric's value and sample fit inside
    /// cards `width` wide.
    fn fit<'a>(
        mut self,
        painter: &egui::Painter,
        metrics: impl IntoIterator<Item = &'a Metric>,
        width: f32,
    ) -> Self {
        // Readings scale with their font size, so measure once at a
        // reference size.
        const REFERENCE: f32 = 100.0;
        let widest = metrics
            .into_iter()
            .map(|m| {
                let sample = Reading::new(painter, m, m.sample, REFERENCE, true).width();
                Reading::new(painter, m, &m.text, REFERENCE, true)
                    .width()
                    .max(sample)
            })
            .fold(0.0, f32::max);
        if widest > 0.0 {
            let room = width - 2.0 * self.pad;
            self.value = self.value.min((REFERENCE * room / widest).floor());
        }
        self
    }
}

/// Approximate descent of the proportional font, as a fraction of its size.
const DESCENT: f32 = 0.2;
/// Approximate height of a numeral, as a fraction of the font size.
const DIGIT_HEIGHT: f32 = 0.7;

/// A card's numeral and unit, laid out at one font size. The unit offset is
/// from the numeral's top-left corner.
struct Reading {
    value: std::sync::Arc<egui::Galley>,
    unit: Option<(std::sync::Arc<egui::Galley>, egui::Vec2)>,
    color: egui::Color32,
}

impl Reading {
    /// `distinguish_source` applies the simulated-value styling (a muted
    /// colour); when false, every value but a missing one reads as plain
    /// text regardless of its source. Values are always upright: italics
    /// for simulated values is Table's own thing (`cell`), not drawn here.
    fn new(
        painter: &egui::Painter,
        m: &Metric,
        text: &str,
        size: f32,
        distinguish_source: bool,
    ) -> Self {
        use egui::text::{LayoutJob, TextFormat};
        use egui::{FontId, vec2};

        let color = match m.source {
            Source::Missing => theme::TEXT_DIM,
            Source::Simulated if distinguish_source => {
                theme::mix(theme::TEXT, theme::HEADING, 0.35)
            }
            Source::Measured | Source::Simulated => theme::TEXT,
        };
        let mut job = LayoutJob::default();
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: FontId::proportional(size),
                color,
                // The light face is loosely spaced at display sizes.
                extra_letter_spacing: -0.02 * size,
                ..Default::default()
            },
        );
        let value = painter.layout_job(job);

        let unit = (m.source != Source::Missing && !m.unit.is_empty()).then(|| {
            // The degree sign sits high like a superscript; word units share
            // the numeral's baseline.
            let degree = m.unit == "\u{b0}";
            let unit_size = if degree {
                size * 0.7
            } else {
                (size * 0.36).max(12.0)
            };
            let galley = painter.layout_no_wrap(
                m.unit.to_string(),
                FontId::proportional(unit_size),
                theme::TEXT_MUTED,
            );
            let offset = if degree {
                vec2(value.size().x + size * 0.03, size * 0.03)
            } else {
                let baseline_gap = DESCENT * (size - unit_size);
                vec2(
                    value.size().x + size * 0.16,
                    value.size().y - baseline_gap - galley.size().y,
                )
            };
            (galley, offset)
        });
        Self { value, unit, color }
    }

    fn width(&self) -> f32 {
        self.unit
            .as_ref()
            .map_or(self.value.size().x, |(g, off)| off.x + g.size().x)
    }
}

/// Bands in a card's top sheen.
const SHEEN_STEPS: u8 = 7;

/// Width of the new-shot glow around a card, in pixels.
const GLOW_WIDTH: u8 = 6;

/// Paint the new-shot glow around `rect`: concentric 1 px rings, each with
/// the card radius grown by its distance, so the halo hugs the card and
/// follows its rounded corners. Opacity falls off quadratically outwards.
fn glow(painter: &egui::Painter, rect: egui::Rect, radius: u8, pulse: f32) {
    for i in 0..GLOW_WIDTH {
        let falloff = 1.0 - f32::from(i) / f32::from(GLOW_WIDTH);
        painter.rect_stroke(
            rect.expand(f32::from(i)),
            egui::CornerRadius::same(radius + i),
            egui::Stroke::new(
                1.0,
                theme::ACCENT.gamma_multiply(0.8 * pulse * falloff * falloff),
            ),
            egui::StrokeKind::Outside,
        );
    }
}

/// Paint a faint sheen over the top of a card at `rect`, fading downwards,
/// to lift it off the background: overlapping bands from the top edge, each
/// one step of white.
fn sheen(painter: &egui::Painter, rect: egui::Rect, radius: egui::CornerRadius) {
    let top_corners = egui::CornerRadius {
        sw: 0,
        se: 0,
        ..radius
    };
    for i in 1..=SHEEN_STEPS {
        let depth = rect.height() * 0.75 * f32::from(i) / f32::from(SHEEN_STEPS);
        let band = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), depth));
        painter.rect_filled(band, top_corners, egui::Color32::from_white_alpha(1));
    }
}

/// Paint one metric card at `rect`. `pulse` (0..=1) adds the new-shot glow.
fn card(ui: &egui::Ui, rect: egui::Rect, m: &Metric, look: CardLook, pulse: f32) {
    use egui::text::{LayoutJob, TextFormat};
    use egui::{FontId, Stroke, StrokeKind, pos2, vec2};

    if !ui.is_rect_visible(rect.expand(f32::from(GLOW_WIDTH))) {
        return;
    }
    let painter = ui.painter();
    let radius = egui::CornerRadius::same(look.radius);
    if pulse > 0.0 {
        glow(painter, rect, look.radius, pulse);
    }
    let fill = theme::mix(theme::SURFACE, theme::SURFACE_RAISED, 0.35 + 0.65 * pulse);
    let stroke = Stroke::new(1.0 + pulse, theme::mix(theme::BORDER, theme::ACCENT, pulse));
    painter.rect_filled(rect, radius, fill);
    sheen(painter, rect, radius);
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);

    let pad = look.pad;
    let label = painter.layout_job(LayoutJob::single_section(
        m.label.to_uppercase(),
        TextFormat {
            font_id: FontId::proportional(look.label),
            color: theme::TEXT_MUTED,
            extra_letter_spacing: 0.6,
            ..Default::default()
        },
    ));
    let label_pos = rect.min + vec2(pad, pad * 0.8);
    let label_bottom = label_pos.y + label.size().y;
    painter.galley(label_pos, label, theme::TEXT_MUTED);

    if m.source == Source::Simulated {
        sim_tag(painter, look.label * 0.85, |size| {
            pos2(
                rect.right() - pad - size.x,
                label_bottom - (look.label + size.y) / 2.0 - 1.0,
            )
        });
    }

    // Centre the numeral's digits in the space under the label.
    let reading = Reading::new(painter, m, &m.text, look.value, true);
    let middle = (label_bottom + rect.bottom()) / 2.0;
    let digits_middle = reading.value.size().y - (DESCENT + DIGIT_HEIGHT / 2.0) * look.value;
    let pos = pos2(rect.left() + pad, (middle - digits_middle).round());
    // Drawn three times, a fraction of a pixel apart, for a heavier numeral
    // than the default light face.
    let weight = (look.value / 56.0).clamp(0.5, 1.4);
    for offset in [
        vec2(0.0, 0.0),
        vec2(weight, 0.0),
        vec2(weight / 2.0, weight / 2.0),
    ] {
        painter.galley(pos + offset, reading.value.clone(), reading.color);
    }
    if let Some((unit, offset)) = reading.unit {
        painter.galley(pos + offset, unit, theme::TEXT_MUTED);
    }

    if m.source == Source::Simulated {
        ui.interact(rect, ui.id().with(m.label), egui::Sense::hover())
            .on_hover_text(SIMULATED_HINT);
    }
}

/// Paint the `SIM` tag with `font`-sized text where `place` puts it, given
/// its size. Returns its rect.
fn sim_tag(
    painter: &egui::Painter,
    font: f32,
    place: impl FnOnce(egui::Vec2) -> egui::Pos2,
) -> egui::Rect {
    let tag = painter.layout_no_wrap(
        "SIM".into(),
        egui::FontId::proportional(font),
        theme::HEADING,
    );
    let size = tag.size() + egui::vec2(10.0, 4.0);
    let rect = egui::Rect::from_min_size(place(size), size);
    painter.rect(
        rect,
        egui::CornerRadius::same(4),
        theme::HEADING.gamma_multiply(0.14),
        egui::Stroke::new(1.0, theme::HEADING.gamma_multiply(0.45)),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.min + egui::vec2(5.0, 2.0), tag, theme::HEADING);
    rect
}

/// A label line's caps title in `color`.
fn group_title(
    painter: &egui::Painter,
    title: &str,
    color: egui::Color32,
) -> std::sync::Arc<egui::Galley> {
    use egui::text::{LayoutJob, TextFormat};

    painter.layout_job(LayoutJob::single_section(
        title.to_uppercase(),
        TextFormat {
            font_id: egui::FontId::proportional(12.5),
            color,
            extra_letter_spacing: 1.5,
            ..Default::default()
        },
    ))
}

/// Width of a label line's bar and caps `title`, as [`group_label`] paints
/// them.
fn group_label_width(painter: &egui::Painter, title: &str) -> f32 {
    GROUP_TITLE_LEFT + group_title(painter, title, theme::TEXT).size().x
}

/// Left of a label line's title: its bar and the gap after it.
const GROUP_TITLE_LEFT: f32 = 3.0 + 8.0;

/// Paint a section's label line in `color`: a bar, the caps title and a
/// hairline out to the section's right edge.
fn group_label(painter: &egui::Painter, rect: egui::Rect, title: &str, color: egui::Color32) {
    use egui::{pos2, vec2};

    let mid = rect.top() + 9.0;
    let bar = egui::Rect::from_center_size(pos2(rect.left() + 1.5, mid), vec2(3.0, 16.0));
    painter.rect_filled(bar, egui::CornerRadius::same(2), color);
    let text = group_title(painter, title, color);
    let text_pos = pos2(rect.left() + GROUP_TITLE_LEFT, mid - text.size().y / 2.0);
    let line_x = text_pos.x + text.size().x + 12.0;
    painter.galley(text_pos, text, color);
    if line_x < rect.right() {
        painter.hline(
            line_x..=rect.right(),
            mid,
            egui::Stroke::new(1.0, theme::BORDER),
        );
    }
}

// ---------------------------------------------------------------------------
// Range bars
// ---------------------------------------------------------------------------

/// Font size of the device filter.
const CONTROL_FONT: f32 = 12.0;
/// Gap between the range bars' title and a legend beside it.
const LEGEND_GAP: f32 = 16.0;

/// Where the range bars' legend goes in a section `width` wide: beside the
/// `title` when both fit on the label line, else on its own row. Without a
/// title the line holds a short note, and the legend is not drawn.
fn legend_place(
    painter: &egui::Painter,
    width: f32,
    title: Option<&str>,
    chips: &[(String, egui::Color32)],
) -> Legend {
    let Some(title) = title else {
        return Legend::Beside;
    };
    let mut probe = painter.clone();
    probe.set_invisible();
    let legend = -bars_legend(&probe, 0.0, 0.0, chips);
    let title = group_label_width(painter, title);
    if title + LEGEND_GAP + legend <= width {
        Legend::Beside
    } else {
        Legend::Below
    }
}
/// Width of a bar row's label column.
const BAR_LABEL_W: f32 = 92.0;
/// Width of a bar row's stats column.
const BAR_STATS_W: f32 = 150.0;
/// Gap between a bar row's columns.
const BAR_COL_GAP: f32 = 14.0;
/// Height of a bar's track.
const TRACK_H: f32 = 14.0;
/// Least height of a device's sub-band in the track.
const SUB_BAND_MIN_H: f32 = 2.0;
/// Room at both ends of a track, outside the domain.
const GUTTER: f32 = 36.0;
/// Narrowest range band; a single value draws a pip this wide. Wide enough
/// that the pip still peeks out past the median ring when every marker lands
/// on the same value.
const PIP_W: f32 = 16.0;
/// How far the "this shot" line reaches past the track, top and bottom.
const MARKER_OVERHANG: f32 = 3.0;
/// Radius of the median ring, drawn just inside the track's half height.
const MEDIAN_RING_RADIUS: f32 = TRACK_H / 2.0 - 1.0;
/// Stroke width of the median ring.
const MEDIAN_RING_STROKE: f32 = 2.0;
/// Radius of the average dot: smaller than the median ring's inner radius,
/// so it fits inside the ring and fills it when the two coincide.
const AVG_DOT_RADIUS: f32 = 4.0;
/// Width of the dark halo stroke around the average dot.
const AVG_DOT_HALO: f32 = 1.0;
/// Width of the dark halo drawn behind the "this shot" line on each side,
/// so the gold line still reads where it crosses the median ring and the
/// average dot.
const SHOT_LINE_HALO: f32 = 1.0;
/// Padding around the distance domain, as a share of its range.
const DISTANCE_PAD: f64 = 0.15;
/// Least padding around the distance domain, in yards and in metres.
const DISTANCE_PAD_YD: f64 = 10.0;
const DISTANCE_PAD_M: f64 = 9.0;
/// Least half-width of the lateral domain before its margin, in display units.
const LATERAL_FLOOR: f64 = 5.0;
/// Margin of the lateral domain past its widest value.
const LATERAL_MARGIN: f64 = 1.2;

/// One of the range bars.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum BarKind {
    Total,
    Carry,
    Lateral,
}

impl BarKind {
    /// Every bar, top to bottom.
    const ALL: [BarKind; 3] = [BarKind::Total, BarKind::Carry, BarKind::Lateral];

    fn label(self) -> &'static str {
        match self {
            BarKind::Total => "Total",
            BarKind::Carry => "Carry",
            BarKind::Lateral => "Lateral",
        }
    }

    /// This bar's value for `shot` in display units: total and carry are
    /// picked by [`pick`]; lateral is the carry offline, picked the same way
    /// (+ = right of the target line).
    fn value(self, shot: &ShotRow, imperial: bool) -> Option<f64> {
        let ball = shot.ball.as_ref();
        let derived = shot.derived.as_ref();
        let (measured, simulated) = match self {
            BarKind::Total => (
                ball.and_then(|b| b.total_distance),
                derived.and_then(|d| d.total_distance),
            ),
            BarKind::Carry => (
                ball.and_then(|b| b.carry_distance),
                derived.and_then(|d| d.carry_distance),
            ),
            BarKind::Lateral => (
                ball.and_then(|b| b.carry_offline),
                derived.and_then(|d| d.carry_offline),
            ),
        };
        let shown = |d: Distance| {
            if imperial {
                d.as_yards()
            } else {
                d.as_meters()
            }
        };
        pick(measured, simulated).map(|(d, _)| shown(d))
    }

    /// `v` as the bar prints it: distances with `prec` decimals, lateral
    /// signed with one.
    fn format(self, v: f64, prec: usize) -> String {
        match self {
            BarKind::Lateral => format!("{v:+.1}"),
            _ => format!("{v:.prec$}"),
        }
    }
}

/// One bar's values over a group's rows.
#[derive(Debug, PartialEq)]
struct BarValues {
    values: Vec<f64>,
    /// Rows with data; the ones without this bar's value are left out of
    /// `values`.
    rows: usize,
}

/// `kind`'s values over `rows`, skipping rows without data.
fn bar_values<'a>(
    rows: impl IntoIterator<Item = &'a ShotRow>,
    kind: BarKind,
    imperial: bool,
) -> BarValues {
    let mut out = BarValues {
        values: Vec::new(),
        rows: 0,
    };
    for row in rows {
        if row.ball.is_none() && row.club.is_none() {
            continue;
        }
        out.rows += 1;
        if let Some(v) = kind.value(row, imperial) {
            out.values.push(v);
        }
    }
    out
}

/// The domain Total and Carry share, so roll reads as the offset between
/// their bands: both bars' extent, padded by `DISTANCE_PAD` of its range
/// and at least 10 yd (9 m) on each side. `None` when neither has data.
fn distance_domain(stats: &[Option<Stats>], imperial: bool) -> Option<(f64, f64)> {
    let (lo, hi) = stats
        .iter()
        .flatten()
        .fold(None, |acc: Option<(f64, f64)>, s| {
            Some(acc.map_or((s.min, s.max), |(lo, hi)| (lo.min(s.min), hi.max(s.max))))
        })?;
    let floor = if imperial {
        DISTANCE_PAD_YD
    } else {
        DISTANCE_PAD_M
    };
    let pad = (DISTANCE_PAD * (hi - lo)).max(floor);
    Some((lo - pad, hi + pad))
}

/// The lateral bar's domain: symmetric about the target line, reaching
/// `LATERAL_MARGIN` past the widest value and never narrower than
/// `LATERAL_FLOOR` before the margin.
fn lateral_domain(stats: Option<&Stats>) -> (f64, f64) {
    let widest = stats.map_or(0.0, |s| s.min.abs().max(s.max.abs()));
    let half = widest.max(LATERAL_FLOOR) * LATERAL_MARGIN;
    (-half, half)
}

/// Where `v` of `domain` falls on the plot span `x`, clamped to it; the
/// middle when the position is undefined (an empty or non-finite domain).
fn bar_x(domain: (f64, f64), x: egui::Rangef, v: f64) -> f32 {
    let (lo, hi) = domain;
    let t = (v - lo) / (hi - lo);
    let t = if hi > lo && !t.is_nan() { t } else { 0.5 };
    x.min + x.span() * t.clamp(0.0, 1.0) as f32
}

/// The horizontal extent of the band from `min` to `max` on the plot span
/// `x`: at least `PIP_W` wide, centred on the values when narrower.
fn band_x(domain: (f64, f64), x: egui::Rangef, min: f64, max: f64) -> egui::Rangef {
    let (a, b) = (bar_x(domain, x, min), bar_x(domain, x, max));
    if b - a >= PIP_W {
        egui::Rangef::new(a, b)
    } else {
        let mid = (a + b) / 2.0;
        egui::Rangef::new(mid - PIP_W / 2.0, mid + PIP_W / 2.0)
    }
}

/// Whether the Cards view draws the range bars and their device filter in
/// `view`: not in `none`, which tracks only the last shot.
fn shows_bars(view: ShotGrouping) -> bool {
    view != ShotGrouping::None
}

/// The set the range bars are built around: the shown shot's set, else the
/// set the next shot joins with `club` in play. `None` when there is no
/// such set.
pub(super) fn bar_group<'a>(
    groups: &'a ShotGroups,
    shot: Option<&ShotRow>,
    club: Option<Club>,
) -> Option<&'a ShotGroupInfo> {
    shot.and_then(|s| s.group.as_deref())
        .and_then(|id| groups.get(id))
        .or_else(|| groups.current_group(club))
}

/// One device's share of a range bar, drawn as a sub-band in its colour.
#[derive(Debug)]
struct DeviceBand {
    colour: egui::Color32,
    stats: Option<Stats>,
}

/// One range bar, ready to paint.
struct Bar {
    kind: BarKind,
    /// Display unit.
    unit: &'static str,
    values: BarValues,
    stats: Option<Stats>,
    /// One sub-band per device, in device order, when the bar is split by
    /// device; empty otherwise.
    devices: Vec<DeviceBand>,
    /// The shown shot's value, when it is a member and has one.
    this: Option<f64>,
    domain: Option<(f64, f64)>,
}

/// The range bars over the rows `members` (indices into `shots`), with the
/// shown shot `shots[shown]` marked when it is a member. With two or more
/// `devices` (exact actor ids, sorted), each bar also splits into one
/// sub-band per device, coloured by `chart::device_colour`.
fn bars_for(
    shots: &[ShotRow],
    members: &[usize],
    shown: Option<usize>,
    devices: &[String],
    imperial: bool,
) -> [Bar; 3] {
    let rows = || members.iter().filter_map(|&i| shots.get(i));
    let shown = shown
        .filter(|i| members.contains(i))
        .and_then(|i| shots.get(i));
    let mut bars = BarKind::ALL.map(|kind| {
        let values = bar_values(rows(), kind, imperial);
        let stats = Stats::of(values.values.iter().copied());
        let devices = if devices.len() < 2 {
            Vec::new()
        } else {
            devices
                .iter()
                .map(|id| DeviceBand {
                    colour: chart::device_colour(devices, id),
                    stats: Stats::of(
                        bar_values(rows().filter(|r| r.actor == *id), kind, imperial).values,
                    ),
                })
                .collect()
        };
        Bar {
            kind,
            unit: if imperial { "yd" } else { "m" },
            this: shown.and_then(|s| kind.value(s, imperial)),
            values,
            stats,
            devices,
            domain: None,
        }
    });
    let distance = distance_domain(&[bars[0].stats, bars[1].stats], imperial);
    bars[0].domain = distance;
    bars[1].domain = distance;
    bars[2].domain = bars[2]
        .stats
        .is_some()
        .then(|| lateral_domain(bars[2].stats.as_ref()));
    bars
}

/// The vertical extents of `n` device sub-bands stacked down a track
/// spanning `track`, top first: each a `n`th of the track (`TRACK_H / n`),
/// at least `SUB_BAND_MIN_H`.
fn sub_bands(track: egui::Rangef, n: usize) -> Vec<egui::Rangef> {
    let h = (track.span() / n.max(1) as f32).max(SUB_BAND_MIN_H);
    (0..n)
        .map(|i| {
            let top = track.min + i as f32 * h;
            egui::Rangef::new(top, top + h)
        })
        .collect()
}

/// "1 shot", "3 shots".
fn shot_count(n: usize) -> String {
    if n == 1 {
        "1 shot".to_owned()
    } else {
        format!("{n} shots")
    }
}

/// Title and colour of the range bar section in `view`, around the set
/// `set`, over `members` of `shots`; `session` is the current session.
///
/// - By set: the set's header. Its count is the set's own unless a device
///   filter (`filtered`) narrows it to the members.
/// - By club: the club's section header, counted over the members.
///
/// `None` when there is no set to show, which shows a note instead, and in
/// the view `none`, which has no bars.
fn bars_title(
    view: ShotGrouping,
    set: Option<&ShotGroupInfo>,
    session: u32,
    shots: &[ShotRow],
    members: &[usize],
    filtered: bool,
) -> Option<(String, egui::Color32)> {
    let g = set?;
    let with_data: Vec<&ShotRow> = members
        .iter()
        .filter_map(|&i| shots.get(i))
        .filter(|r| r.ball.is_some() || r.club.is_some())
        .collect();
    match view {
        ShotGrouping::None => None,
        ShotGrouping::ByClub => {
            let mut ids: Vec<&str> = with_data
                .iter()
                .filter_map(|r| r.group.as_deref())
                .collect();
            ids.sort_unstable();
            ids.dedup();
            let earlier = g.session < session;
            let tally = Some((ids.len(), with_data.len()));
            let color = if earlier {
                theme::TEXT_MUTED
            } else {
                theme::ACCENT
            };
            Some((club_title(g.club, earlier, tally), color))
        }
        ShotGrouping::BySet => {
            let next = g.open && g.shots == 0;
            let color = match (next, g.open) {
                (true, _) => theme::GOLD,
                (false, true) => theme::ACCENT,
                (false, false) => theme::TEXT_MUTED,
            };
            let title = if filtered && !next {
                let narrowed = ShotGroupInfo {
                    shots: u32::try_from(with_data.len()).unwrap_or(u32::MAX),
                    ..g.clone()
                };
                set_title(&narrowed, next)
            } else {
                set_title(g, next)
            };
            Some((title, color))
        }
    }
}

/// Labels of the device filter's options: `All`, then each of `ids` by its
/// name (its actor id when unnamed). `None` with fewer than two devices,
/// when the filter is not shown.
pub(super) fn device_options(
    ids: &[String],
    actors: &HashMap<String, ActorStatusResponse>,
) -> Option<Vec<String>> {
    (ids.len() >= 2).then(|| {
        std::iter::once("All".to_owned())
            .chain(ids.iter().map(|id| {
                let name = actors.get(id).map(|a| a.name.as_str());
                chart::device_label(id, name).to_owned()
            }))
            .collect()
    })
}

/// Paint the control row in `ui` (right to left): the device filter over
/// `devices` (`All`, then each device), with `device` picked. Returns the
/// option picked this frame.
pub(super) fn bar_controls(ui: &mut egui::Ui, devices: &[String], device: usize) -> Option<usize> {
    let labels: Vec<&str> = devices.iter().map(String::as_str).collect();
    let picked = theme::segmented_sized(ui, &labels, device, None, CONTROL_FONT).inner;
    ui.label(theme::caps("Device", 11.0, theme::TEXT_MUTED))
        .on_hover_text("Shots from one device, or every device coloured by device");
    picked
}

/// The stats column's two lines for `bar`: averages, then spread and how
/// many rows have the value.
fn bar_stats_text(bar: &Bar) -> (String, String) {
    let Some(s) = &bar.stats else {
        return ("no data".to_owned(), String::new());
    };
    let k = bar.kind;
    let first = format!(
        "avg {} \u{b7} med {}",
        k.format(s.mean, 0),
        k.format(s.median, 0)
    );
    let mut second = s
        .std_dev
        .map(|sd| format!("sd {sd:.1}"))
        .unwrap_or_default();
    if s.count < bar.values.rows {
        if !second.is_empty() {
            second.push_str(" \u{b7} ");
        }
        second.push_str(&format!("{}/{}", s.count, bar.values.rows));
    }
    (first, second)
}

/// Hover text on a bar's track.
fn bar_tooltip(bar: &Bar) -> String {
    let Some(s) = &bar.stats else {
        return format!(
            "No {} values in these shots",
            bar.kind.label().to_lowercase()
        );
    };
    let f = |v: f64| bar.kind.format(v, 1);
    let mut text = format!(
        "n = {} \u{b7} min {} \u{b7} max {} \u{b7} avg {} \u{b7} median {}",
        s.count,
        f(s.min),
        f(s.max),
        f(s.mean),
        f(s.median)
    );
    if let Some(sd) = s.std_dev {
        text.push_str(&format!(" \u{b7} sd {sd:.1}"));
    }
    if bar.kind == BarKind::Lateral {
        text.push_str("\n+ is right of the target line.");
    }
    text
}

/// Paint the legend of the range bars, right-aligned at `right` on the line
/// through `mid`, with a colour chip per device in `chips` on its left.
/// Returns its left edge.
fn bars_legend(
    painter: &egui::Painter,
    right: f32,
    mid: f32,
    chips: &[(String, egui::Color32)],
) -> f32 {
    use egui::{FontId, Stroke, pos2};

    let mut x = right;
    for (i, text) in ["this shot", "median", "avg", "range"]
        .into_iter()
        .enumerate()
    {
        let galley =
            painter.layout_no_wrap(text.to_owned(), FontId::proportional(11.0), theme::TEXT_DIM);
        x -= galley.size().x;
        painter.galley(
            pos2(x, mid - galley.size().y / 2.0),
            galley,
            theme::TEXT_DIM,
        );
        x -= 6.0;
        let (top, bottom) = (mid - 6.0, mid + 6.0);
        match i {
            0 => {
                let lx = x - 1.0;
                painter.line_segment(
                    [pos2(lx, top), pos2(lx, bottom)],
                    Stroke::new(1.5 + 2.0 * SHOT_LINE_HALO, theme::BG_DEEP),
                );
                painter.line_segment(
                    [pos2(lx, top), pos2(lx, bottom)],
                    Stroke::new(1.5, theme::GOLD),
                );
                x -= 2.0;
            }
            1 => {
                painter.circle_stroke(
                    pos2(x - MEDIAN_RING_RADIUS, mid),
                    MEDIAN_RING_RADIUS,
                    Stroke::new(MEDIAN_RING_STROKE, theme::TEXT),
                );
                x -= 2.0 * MEDIAN_RING_RADIUS;
            }
            2 => {
                painter.circle(
                    pos2(x - AVG_DOT_RADIUS, mid),
                    AVG_DOT_RADIUS,
                    theme::GOLD,
                    Stroke::new(AVG_DOT_HALO, theme::BG_DEEP),
                );
                x -= 2.0 * AVG_DOT_RADIUS;
            }
            _ => {
                let swatch =
                    egui::Rect::from_min_max(pos2(x - 18.0, mid - 4.0), pos2(x, mid + 4.0));
                painter.rect(
                    swatch,
                    egui::CornerRadius::same(4),
                    theme::ACCENT.gamma_multiply(0.28),
                    Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.6)),
                    egui::StrokeKind::Inside,
                );
                x -= 18.0;
            }
        }
        x -= 16.0;
    }
    for (name, colour) in chips.iter().rev() {
        let galley =
            painter.layout_no_wrap(name.clone(), FontId::proportional(11.0), theme::TEXT_MUTED);
        x -= galley.size().x;
        painter.galley(
            pos2(x, mid - galley.size().y / 2.0),
            galley,
            theme::TEXT_MUTED,
        );
        x -= 5.0;
        let swatch = egui::Rect::from_min_max(pos2(x - 8.0, mid - 4.0), pos2(x, mid + 4.0));
        painter.rect_filled(swatch, egui::CornerRadius::same(2), *colour);
        x -= 8.0 + 12.0;
    }
    x + if chips.is_empty() { 16.0 } else { 12.0 }
}

/// Paint the range bar section `at`: the label line (`title`, or `note`
/// when there is none), the legend with the device `chips` beside the title
/// or on its own row, then one row per bar.
fn range_bars(
    ui: &egui::Ui,
    at: &BarsLayout,
    title: Option<&(String, egui::Color32)>,
    bars: &[Bar; 3],
    chips: &[(String, egui::Color32)],
    note: &str,
) {
    use egui::{FontId, pos2};

    let painter = ui.painter();
    let label = at.label;
    let mid = label.top() + 9.0;
    match title {
        Some((text, color)) => {
            let legend = bars_legend(painter, at.legend.right(), at.legend.top() + 9.0, chips);
            let right = if at.legend == label {
                legend - LEGEND_GAP
            } else {
                label.right()
            };
            group_label(painter, label.with_max_x(right), text, *color);
        }
        None => {
            let galley = painter.layout_no_wrap(
                note.to_owned(),
                FontId::proportional(12.0),
                theme::TEXT_DIM,
            );
            let x = label.right() - galley.size().x;
            painter.galley(
                pos2(x, mid - galley.size().y / 2.0),
                galley,
                theme::TEXT_DIM,
            );
            group_label(painter, label.with_max_x(x - 16.0), "Set", theme::TEXT_DIM);
        }
    }
    for (bar, row) in bars.iter().zip(&at.rows) {
        bar_row(ui, *row, bar);
    }
}

/// Paint one range bar row in `row`: label and unit, the track, then the
/// stats column.
fn bar_row(ui: &egui::Ui, row: egui::Rect, bar: &Bar) {
    use egui::text::{LayoutJob, TextFormat};
    use egui::{FontId, Stroke, StrokeKind, pos2};

    if !ui.is_rect_visible(row) {
        return;
    }
    let painter = ui.painter();

    // Label column: caps name over the unit.
    let name = painter.layout_job(LayoutJob::single_section(
        bar.kind.label().to_uppercase(),
        TextFormat {
            font_id: FontId::proportional(11.0),
            color: theme::TEXT_MUTED,
            extra_letter_spacing: 0.6,
            ..Default::default()
        },
    ));
    let name_h = name.size().y;
    let unit = painter.layout_no_wrap(
        bar.unit.to_owned(),
        FontId::proportional(10.0),
        theme::TEXT_DIM,
    );
    let top = row.center().y - (name_h + unit.size().y) / 2.0;
    painter.galley(pos2(row.left(), top), name, theme::TEXT_MUTED);
    painter.galley(pos2(row.left(), top + name_h), unit, theme::TEXT_DIM);

    // Track.
    let track = egui::Rect::from_x_y_ranges(
        (row.left() + BAR_LABEL_W + BAR_COL_GAP)..=(row.right() - BAR_STATS_W - BAR_COL_GAP),
        (row.center().y - TRACK_H / 2.0)..=(row.center().y + TRACK_H / 2.0),
    );
    let radius = egui::CornerRadius::same((TRACK_H / 2.0) as u8);
    painter.rect_filled(track, radius, theme::VEIL);
    let plot = egui::Rangef::new(track.left() + GUTTER, track.right() - GUTTER);
    let small =
        |text: String, color| painter.layout_no_wrap(text, FontId::proportional(10.0), color);
    // Gutter labels: the domain's ends, or L and R for lateral. Each is
    // dropped when a value numeral needs its room.
    let mut gutter = Vec::new();
    if let Some(domain) = bar.domain {
        let (lo, hi) = match bar.kind {
            BarKind::Lateral => ("L".to_owned(), "R".to_owned()),
            k => (k.format(domain.0, 0), k.format(domain.1, 0)),
        };
        gutter.push((small(lo, theme::TEXT_DIM), track.left() + GUTTER / 2.0));
        gutter.push((small(hi, theme::TEXT_DIM), track.right() - GUTTER / 2.0));
    }

    let mut numerals: Vec<egui::Rect> = Vec::new();
    if let (Some(s), Some(domain)) = (&bar.stats, bar.domain) {
        let band = band_x(domain, plot, s.min, s.max);
        let paint_band = |x: egui::Rangef, y: egui::Rangef, colour: egui::Color32| {
            painter.rect(
                egui::Rect::from_x_y_ranges(x, y),
                egui::CornerRadius::same((x.span().min(y.span()) / 2.0) as u8),
                colour.gamma_multiply(0.28),
                Stroke::new(1.0, colour.gamma_multiply(0.6)),
                StrokeKind::Inside,
            );
        };
        if bar.devices.is_empty() {
            paint_band(band, track.y_range(), theme::ACCENT);
        } else {
            // One sub-band per device, stacked; a device without this
            // bar's value leaves its slot empty.
            for (d, y) in bar
                .devices
                .iter()
                .zip(sub_bands(track.y_range(), bar.devices.len()))
            {
                if let Some(ds) = &d.stats {
                    paint_band(band_x(domain, plot, ds.min, ds.max), y, d.colour);
                }
            }
        }
        if bar.kind == BarKind::Lateral {
            // The target line.
            let x = bar_x(domain, plot, 0.0).round() + 0.5;
            painter.vline(x, track.y_range(), Stroke::new(1.0, theme::TEXT_DIM));
        }

        // Min and max beside the band; a single value once, after it.
        let numeral = |text: String| {
            painter.layout_no_wrap(text, FontId::proportional(11.0), theme::TEXT_MUTED)
        };
        let y = |g: &std::sync::Arc<egui::Galley>| track.center().y - g.size().y / 2.0;
        let after = numeral(bar.kind.format(s.max, 0));
        let x = (band.max + 6.0).min(track.right() - after.size().x - 4.0);
        numerals.push(egui::Rect::from_min_size(pos2(x, y(&after)), after.size()));
        painter.galley(pos2(x, y(&after)), after, theme::TEXT_MUTED);
        if s.count > 1 {
            let before = numeral(bar.kind.format(s.min, 0));
            let x = (band.min - 6.0 - before.size().x).max(track.left() + 4.0);
            numerals.push(egui::Rect::from_min_size(
                pos2(x, y(&before)),
                before.size(),
            ));
            painter.galley(pos2(x, y(&before)), before, theme::TEXT_MUTED);
        }

        // Median ring, average dot, then the this-shot line on top, in
        // that order: a value shared by all three still reads as a white
        // ring filled with a gold dot, with a gold line (haloed so it still
        // reads against the dot) through it.
        let median = bar_x(domain, plot, s.median);
        painter.circle_stroke(
            pos2(median, track.center().y),
            MEDIAN_RING_RADIUS,
            Stroke::new(MEDIAN_RING_STROKE, theme::TEXT),
        );

        painter.circle(
            pos2(bar_x(domain, plot, s.mean), track.center().y),
            AVG_DOT_RADIUS,
            theme::GOLD,
            Stroke::new(AVG_DOT_HALO, theme::BG_DEEP),
        );

        if let Some(v) = bar.this {
            let (top, bottom) = (
                track.top() - MARKER_OVERHANG,
                track.bottom() + MARKER_OVERHANG,
            );
            let shot = bar_x(domain, plot, v);
            painter.line_segment(
                [pos2(shot, top), pos2(shot, bottom)],
                Stroke::new(1.5 + 2.0 * SHOT_LINE_HALO, theme::BG_DEEP),
            );
            painter.line_segment(
                [pos2(shot, top), pos2(shot, bottom)],
                Stroke::new(1.5, theme::GOLD),
            );
        }
    }
    for (galley, centre) in gutter {
        let rect = egui::Rect::from_center_size(pos2(centre, track.center().y), galley.size());
        if numerals.iter().all(|n| !n.expand(2.0).intersects(rect)) {
            painter.galley(rect.min, galley, theme::TEXT_DIM);
        }
    }
    ui.interact(
        track,
        ui.id().with(("bar_track", bar.kind)),
        egui::Sense::hover(),
    )
    .on_hover_text(bar_tooltip(bar));

    // Stats column.
    let (first, second) = bar_stats_text(bar);
    let left = row.right() - BAR_STATS_W;
    let first_color = if bar.stats.is_some() {
        theme::TEXT
    } else {
        theme::TEXT_DIM
    };
    let first = painter.layout_no_wrap(first, FontId::proportional(13.0), first_color);
    let first_h = first.size().y;
    let top = if second.is_empty() {
        row.center().y - first_h / 2.0
    } else {
        row.top()
    };
    painter.galley(pos2(left, top), first, first_color);
    if !second.is_empty() {
        let second = painter.layout_no_wrap(second, FontId::proportional(11.0), theme::TEXT_MUTED);
        painter.galley(pos2(left, top + first_h), second, theme::TEXT_MUTED);
    }
}

// ---------------------------------------------------------------------------
// Shot list
// ---------------------------------------------------------------------------

/// Shot list width limits, and the range its starting width is picked from
/// (a share of the tab's width).
const LIST_MIN_WIDTH: f32 = 200.0;
const LIST_MAX_WIDTH: f32 = 320.0;
const LIST_DEFAULT_WIDTH: std::ops::RangeInclusive<f32> = 212.0..=248.0;
/// Height of the list's header line.
const LIST_HEADER_HEIGHT: f32 = 30.0;
/// Height of the foot's New set / New session line.
const STRIP_BUTTONS_HEIGHT: f32 = 28.0;
/// Height of the foot's hint line.
const STRIP_HINT_HEIGHT: f32 = 16.0;
/// Height of the hairline row at the top of the list's foot.
const FOOT_RULE: f32 = 1.0;
/// Vertical spacing between the foot's lines.
const FOOT_SPACING: f32 = 6.0;
/// Height of a group header in the list.
const GROUP_HEADER_HEIGHT: f32 = 18.0;
/// Extra room above a group header that follows other lines, so it sits
/// with the run below it.
const HEADER_LEAD: f32 = 8.0;
const ENTRY_HEIGHT: f32 = 68.0;
const ENTRY_GAP: f32 = 8.0;
const ENTRY_RADIUS: u8 = 8;
const ENTRY_PAD: f32 = 12.0;

/// Height of the list's foot for `strip`: the hairline, the hint line when
/// there is one and the buttons, with the spacing between them. Zero when
/// the buttons are hidden (the view `none`), which leaves no foot.
fn list_foot_height(strip: &Strip) -> f32 {
    if !strip.actions {
        return 0.0;
    }
    let hint = if strip.hint.is_some() {
        STRIP_HINT_HEIGHT + FOOT_SPACING
    } else {
        0.0
    };
    FOOT_RULE + FOOT_SPACING + hint + STRIP_BUTTONS_HEIGHT
}

/// One line of the shot list.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ListItem {
    /// `shots[i]`.
    Shot(usize),
    /// By set: header of the set with id `id`, over a run of its rows, or,
    /// with `next`, over nothing at the top of the list for the empty set
    /// the next shot joins.
    GroupHeader { id: String, next: bool },
    /// By club: header of the section of `club` in `session`, over its
    /// rows, or, with `next`, over nothing at the top of the list for the
    /// club whose first shot of the session is next.
    ClubHeader {
        session: u32,
        club: Option<Club>,
        next: bool,
    },
    /// Header over a run of rows with data that belong to no known set,
    /// when a header is above them.
    Ungrouped,
}

/// A run of consecutive shot list rows, newest first, headed by its first
/// row with data.
struct Run<'a> {
    /// The set of the run's rows: `None` for the rows without data that
    /// come before any row with data, `Some(None)` for rows in no known set.
    group: Option<Option<&'a str>>,
    rows: Vec<usize>,
}

/// `shots` split into runs, newest first. A run starts at each row with
/// data whose set differs from the run before; rows without data never
/// start one.
fn runs<'a>(shots: &'a [ShotRow], groups: &ShotGroups) -> Vec<Run<'a>> {
    let mut runs: Vec<Run<'a>> = Vec::new();
    for i in (0..shots.len()).rev() {
        let row = &shots[i];
        if row.ball.is_some() || row.club.is_some() {
            let group = row.group.as_deref().filter(|id| groups.get(id).is_some());
            if runs.last().is_none_or(|r| r.group != Some(group)) {
                runs.push(Run {
                    group: Some(group),
                    rows: Vec::new(),
                });
            }
        } else if runs.is_empty() {
            runs.push(Run {
                group: None,
                rows: Vec::new(),
            });
        }
        if let Some(run) = runs.last_mut() {
            run.rows.push(i);
        }
    }
    runs
}

/// A By club section's key: the session and the club.
type SectionKey = (u32, Option<Club>);

/// The shot list's lines, newest shot first, under the headers of the view
/// `groups.mode` with `club` in play.
///
/// - `none`: the last shot only, so no headers.
/// - By set: a header goes before each run of one set, and the empty set
///   the next shot joins gets a header at the very top.
/// - By club: one section per club and session, combining every set with
///   that club in that session. The section of the club in play in the
///   current session comes first, then every other section by last
///   activity, newest first (see [`section_activity`]). A section's rows
///   are newest first, and rows in no known set gather in one last
///   section. While the open set is empty and the club in play has no
///   section in the current session, that club gets a header at the very
///   top.
///
/// Rows without data never break a run, and the rows before the first row
/// of a known set (a shot in flight, rows whose set is not known yet) come
/// first.
fn list_items(shots: &[ShotRow], groups: &ShotGroups, club: Option<Club>) -> Vec<ListItem> {
    let view = groups.mode;
    if view == ShotGrouping::None {
        return (0..shots.len()).rev().map(ListItem::Shot).collect();
    }
    let runs = runs(shots, groups);
    let mut items = Vec::with_capacity(shots.len() + 4);
    let mut headed = false;
    let mut push_run = |items: &mut Vec<ListItem>, header: Option<ListItem>, rows: &[usize]| {
        match header {
            Some(h @ ListItem::Ungrouped) if headed => items.push(h),
            Some(ListItem::Ungrouped) => {}
            Some(h) => {
                items.push(h);
                headed = true;
            }
            None => {}
        }
        items.extend(rows.iter().copied().map(ListItem::Shot));
    };
    let set_header = |group: Option<Option<&str>>| match group {
        Some(Some(id)) => Some(ListItem::GroupHeader {
            id: id.to_owned(),
            next: false,
        }),
        Some(None) => Some(ListItem::Ungrouped),
        None => None,
    };
    let empty_open = groups.current_group(club).filter(|g| g.shots == 0);

    if view == ShotGrouping::BySet {
        if let Some(next) = empty_open {
            let header = ListItem::GroupHeader {
                id: next.id.clone(),
                next: true,
            };
            push_run(&mut items, Some(header), &[]);
        }
        for run in &runs {
            push_run(&mut items, set_header(run.group), &run.rows);
        }
        return items;
    }

    let session = groups.session();
    let section_of = |id: &str| groups.get(id).map(|g| (g.session, g.club));
    let head = runs
        .iter()
        .position(|r| matches!(r.group, Some(Some(_))))
        .unwrap_or(runs.len());
    let mut sections: Vec<(SectionKey, Vec<usize>)> = Vec::new();
    let mut loose = Vec::new();
    for run in &runs[head..] {
        match run.group.flatten().and_then(section_of) {
            Some(key) => match sections.iter_mut().find(|(k, _)| *k == key) {
                Some((_, rows)) => rows.extend(&run.rows),
                None => sections.push((key, run.rows.clone())),
            },
            None => loose.extend(&run.rows),
        }
    }
    let pinned = (session, club);
    sections.sort_by_cached_key(|(key, rows)| {
        let number = groups
            .iter_sets()
            .filter(|g| (g.session, g.club) == *key)
            .map(|g| g.number)
            .max()
            .unwrap_or(0);
        let activity = section_activity(groups, *key, rows, shots);
        std::cmp::Reverse((*key == pinned, activity, number))
    });
    if empty_open.is_some() && !sections.iter().any(|(k, _)| *k == pinned) {
        let header = ListItem::ClubHeader {
            session,
            club,
            next: true,
        };
        push_run(&mut items, Some(header), &[]);
    }
    for run in &runs[..head] {
        push_run(&mut items, set_header(run.group), &run.rows);
    }
    for ((session, club), rows) in &sections {
        let header = ListItem::ClubHeader {
            session: *session,
            club: *club,
            next: false,
        };
        push_run(&mut items, Some(header), rows);
    }
    if !loose.is_empty() {
        push_run(&mut items, Some(ListItem::Ungrouped), &loose);
    }
    items
}

/// When the By club section `key` was last active: its newest row (`rows`
/// is newest first and starts with a row with data), or, in the current
/// session, the last selection of its club, whichever is later.
fn section_activity(
    groups: &ShotGroups,
    (session, club): SectionKey,
    rows: &[usize],
    shots: &[ShotRow],
) -> Activity {
    let shot = rows.first().map_or((0, 0), |&i| (2 * i + 2, 0));
    let selected = (session == groups.session())
        .then(|| groups.selected_at(club, shots))
        .flatten();
    selected.map_or(shot, |sel| shot.max(sel))
}

/// A By set header's title, e.g. "Set 3 · 7I · 10 shots". `next` marks
/// the empty set the next shot joins: "Set 4 · DR · Next shot".
fn set_title(group: &ShotGroupInfo, next: bool) -> String {
    let tail = match (next, group.shots) {
        (true, _) => "Next shot".to_owned(),
        (false, n) => shot_count(n as usize),
    };
    let club = group
        .club
        .map_or_else(|| "No club".to_owned(), |c| c.to_string());
    format!("Set {} \u{b7} {club} \u{b7} {tail}", group.number)
}

/// A By club header's title, e.g. "7 Iron · 2 sets · 14 shots", with
/// "Earlier" after the club for a section from an earlier session.
/// `tally` is `(sets, shots)`; the set count shows only when it is more
/// than one. `None` marks the club whose first shot of the session is
/// next: "7 Iron · Next shot".
fn club_title(club: Option<Club>, earlier: bool, tally: Option<(usize, usize)>) -> String {
    let mut parts = vec![club.map_or("No club", club_name).to_owned()];
    if earlier {
        parts.push("Earlier".to_owned());
    }
    match tally {
        None => parts.push("Next shot".to_owned()),
        Some((sets, shots)) => {
            if sets > 1 {
                parts.push(format!("{sets} sets"));
            }
            parts.push(shot_count(shots));
        }
    }
    parts.join(" \u{b7} ")
}

/// A By club list header's title: [`club_title`], with an earlier
/// session's set count left to the hover text so the title stays short
/// enough for the list.
fn club_header_title(club: Option<Club>, earlier: bool, tally: Option<(usize, usize)>) -> String {
    let tally = tally.map(|(sets, shots)| (if earlier { 1 } else { sets }, shots));
    club_title(club, earlier, tally)
}

/// Local `HH:MM` of an RFC 3339 timestamp.
fn clock_time(rfc3339: &str) -> Option<String> {
    let t = chrono::DateTime::parse_from_rfc3339(rfc3339).ok()?;
    Some(t.with_timezone(&chrono::Local).format("%H:%M").to_string())
}

/// Hover text for + New set in `view`.
fn new_group_hint(view: ShotGrouping) -> &'static str {
    if view == ShotGrouping::ByClub {
        "Start a fresh set with the next shot. By club keeps combining a club's \
         sets until New session."
    } else {
        "Close this set; the next shot starts a fresh one."
    }
}

/// Hover text for New session.
const NEW_SESSION_HINT: &str = "Close the open set and start a new session: the next shot starts a new set, \
     and By club starts every club afresh.";

/// A list header's title in caps, laid out to fit `width`: whole when it
/// fits, else the part before the last ` · ` cut with an ellipsis and the
/// last part (the shot count) kept whole after it, returned as the second
/// galley.
fn header_text(
    painter: &egui::Painter,
    title: &str,
    width: f32,
    color: egui::Color32,
) -> (
    std::sync::Arc<egui::Galley>,
    Option<std::sync::Arc<egui::Galley>>,
) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};

    let layout = |text: String, max: Option<f32>| {
        let mut job = LayoutJob::single_section(
            text,
            TextFormat {
                font_id: egui::FontId::proportional(12.0),
                color,
                extra_letter_spacing: 1.0,
                ..Default::default()
            },
        );
        if let Some(max) = max {
            job.wrap = TextWrapping::truncate_at_width(max.max(0.0));
        }
        painter.layout_job(job)
    };
    let caps = title.to_uppercase();
    let whole = layout(caps.clone(), None);
    if whole.size().x <= width {
        return (whole, None);
    }
    match caps.rsplit_once(" \u{b7} ") {
        Some((head, last)) => {
            let tail = layout(format!(" \u{b7} {last}"), None);
            let head = layout(head.to_owned(), Some(width - tail.size().x));
            (head, Some(tail))
        }
        None => (layout(caps, Some(width)), None),
    }
}

/// Paint a list header in `rect`: a 3 px bar and the caps `title` in
/// `color`, then a hairline out to `time`, right-aligned in dim text. The
/// time, then the hairline, are left out when the title leaves no room.
fn list_header(
    painter: &egui::Painter,
    rect: egui::Rect,
    title: &str,
    time: Option<&str>,
    color: egui::Color32,
) {
    use egui::{FontId, pos2, vec2};

    let mid = rect.center().y;
    let bar = egui::Rect::from_center_size(pos2(rect.left() + 1.5, mid), vec2(3.0, 14.0));
    painter.rect_filled(bar, egui::CornerRadius::same(2), color);
    let left = bar.right() + 7.0;
    let (head, tail) = header_text(painter, title, (rect.right() - left).max(0.0), color);
    let mut text_right = left + head.size().x;
    painter.galley(pos2(left, mid - head.size().y / 2.0), head, color);
    if let Some(tail) = tail {
        painter.galley(
            pos2(text_right, mid - tail.size().y / 2.0),
            tail.clone(),
            color,
        );
        text_right += tail.size().x;
    }

    let mut line_end = rect.right();
    if let Some(time) = time {
        let time =
            painter.layout_no_wrap(time.to_owned(), FontId::proportional(11.0), theme::TEXT_DIM);
        let x = rect.right() - time.size().x;
        // Shown only with room for a hairline before it too.
        if x - text_right >= 32.0 {
            line_end = x - 8.0;
            painter.galley(pos2(x, mid - time.size().y / 2.0), time, theme::TEXT_DIM);
        }
    }
    let line_start = text_right + 8.0;
    if line_end - line_start >= 8.0 {
        painter.hline(
            line_start..=line_end,
            mid,
            egui::Stroke::new(1.0, theme::BORDER),
        );
    }
}

/// What a shot list entry shows.
struct ListEntry {
    number: u32,
    device: Option<String>,
    carry: Metric,
    total: Metric,
    /// The shot has ball or club data.
    has_data: bool,
    /// The shot finished without ball or club data: the device did not read
    /// it.
    no_read: bool,
}

impl ListEntry {
    /// `shot` already converted to the display unit system.
    fn new(shot: &ShotRow, device: Option<&str>, imperial: bool) -> Self {
        let dist = if imperial { "yd" } else { "m" };
        let (no_ball, no_derived) = Default::default();
        let ball: &BallFlight = shot.ball.as_ref().unwrap_or(&no_ball);
        let d: &DerivedFlight = shot.derived.as_ref().unwrap_or(&no_derived);
        let v = |x: Option<Distance>| x.map(|x| x.value());
        Self {
            number: shot.shot_number,
            device: device.map(str::to_string),
            carry: Metric::new(
                "Carry",
                dist,
                pick(v(ball.carry_distance), v(d.carry_distance)),
                1,
                false,
            ),
            total: Metric::new(
                "Total",
                dist,
                pick(v(ball.total_distance), v(d.total_distance)),
                1,
                false,
            ),
            has_data: shot.ball.is_some() || shot.club.is_some(),
            no_read: shot.finished && shot.ball.is_none() && shot.club.is_none(),
        }
    }
}

/// How a list entry is drawn.
#[derive(Clone, Copy, Default)]
struct EntryState {
    selected: bool,
    hovered: bool,
    latest: bool,
    /// New-shot glow, 0..=1.
    pulse: f32,
}

/// Paint one shot list entry at `rect`: a mini card with the shot number
/// and device on top, then carry and total.
fn list_entry(ui: &egui::Ui, rect: egui::Rect, e: &ListEntry, state: EntryState) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    use egui::{FontId, Stroke, StrokeKind, pos2, vec2};

    let painter = ui.painter();
    let radius = egui::CornerRadius::same(ENTRY_RADIUS);
    let pulse = state.pulse;
    if pulse > 0.0 {
        glow(painter, rect, ENTRY_RADIUS, pulse);
    }
    let base = if state.selected {
        0.8
    } else if state.hovered {
        0.55
    } else {
        0.2
    };
    let fill = theme::mix(
        theme::SURFACE,
        theme::SURFACE_RAISED,
        base + (1.0 - base) * pulse,
    );
    painter.rect_filled(rect, radius, fill);
    sheen(painter, rect, radius);
    let stroke = if state.selected {
        Stroke::new(1.0 + pulse, theme::ACCENT)
    } else if state.hovered {
        Stroke::new(1.0, theme::mix(theme::BORDER, theme::TEXT_MUTED, 0.35))
    } else {
        Stroke::new(1.0, theme::mix(theme::BORDER, theme::ACCENT, pulse))
    };
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    if state.selected {
        let stripe = egui::Rect::from_min_size(rect.min, vec2(4.0, rect.height()));
        let corners = egui::CornerRadius {
            ne: 0,
            se: 0,
            ..radius
        };
        painter.rect_filled(stripe, corners, theme::ACCENT);
    }

    let left = rect.left() + ENTRY_PAD + 2.0;
    let right = rect.right() - ENTRY_PAD;
    let number_color = match (e.has_data, state.latest) {
        (false, _) => theme::TEXT_DIM,
        (true, true) => theme::GOLD,
        (true, false) => theme::TEXT,
    };
    let number = painter.layout_no_wrap(
        format!("#{}", e.number),
        FontId::proportional(17.0),
        number_color,
    );
    let top = rect.top() + 8.0;
    let number_width = number.size().x;
    let number_height = number.size().y;
    // Drawn twice, half a pixel apart, for a heavier numeral.
    painter.galley(pos2(left, top), number.clone(), number_color);
    painter.galley(pos2(left + 0.5, top), number, number_color);

    if let Some(device) = &e.device {
        let room = right - left - number_width - 10.0;
        let mut job = LayoutJob::single_section(
            device.clone(),
            TextFormat {
                font_id: FontId::proportional(11.5),
                color: theme::TEXT_MUTED,
                ..Default::default()
            },
        );
        job.wrap = TextWrapping::truncate_at_width(room.max(0.0));
        let galley = painter.layout_job(job);
        let pos = pos2(
            right - galley.size().x,
            top + (number_height - galley.size().y) / 2.0 + 1.0,
        );
        painter.galley(pos, galley, theme::TEXT_MUTED);
    }

    let label_y = top + number_height + 3.0;
    if e.no_read {
        let text = painter.layout_no_wrap(
            "No read".to_owned(),
            FontId::proportional(11.5),
            theme::TEXT_DIM,
        );
        painter.galley(pos2(left, label_y), text, theme::TEXT_DIM);
        return;
    }
    let half = (right - left) / 2.0;
    for (k, m) in [&e.carry, &e.total].into_iter().enumerate() {
        let x = left + half * k as f32;
        let label = painter.layout_job(LayoutJob::single_section(
            m.label.to_uppercase(),
            TextFormat {
                font_id: FontId::proportional(9.5),
                color: theme::TEXT_DIM,
                extra_letter_spacing: 0.8,
                ..Default::default()
            },
        ));
        let label_height = label.size().y;
        painter.galley(pos2(x, label_y), label, theme::TEXT_DIM);
        let reading = Reading::new(painter, m, &m.text, 16.0, false);
        let pos = pos2(x, label_y + label_height);
        painter.galley(pos, reading.value.clone(), reading.color);
        if let Some((unit, offset)) = reading.unit {
            painter.galley(pos + offset, unit, theme::TEXT_MUTED);
        }
    }
}

// ---------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------

impl FlighthookApp {
    /// Bring the caches derived from the shot list up to date: the rows in
    /// display units and the device ids. A device filter whose device has
    /// left the history falls back to every device.
    pub(crate) fn refresh_shot_caches(&mut self) {
        let (rev, units) = (self.shots_rev, self.units_toggle);
        let shots = &self.shots;
        self.display_rows.get((rev, units), || {
            shots.iter().map(|s| s.to_unit_system(units)).collect()
        });
        let ids = self.device_ids.get(rev, || chart::device_ids(shots));
        if self
            .device_filter
            .as_ref()
            .is_some_and(|d| !ids.contains(d))
        {
            self.device_filter = None;
        }
    }

    /// `shots[i]` in display units: from the cache `refresh_shot_caches`
    /// keeps, converted on the spot when the cache is out of date.
    fn display_row(&self, i: usize) -> Option<Cow<'_, ShotRow>> {
        let cached = self
            .display_rows
            .peek(&(self.shots_rev, self.units_toggle))
            .and_then(|rows| rows.get(i));
        match cached {
            Some(row) => Some(Cow::Borrowed(row)),
            None => self
                .shots
                .get(i)
                .map(|s| Cow::Owned(s.to_unit_system(self.units_toggle))),
        }
    }

    /// The distinct actor ids in the shot list, exact and sorted.
    pub(super) fn devices(&self) -> Cow<'_, [String]> {
        match self.device_ids.peek(&self.shots_rev) {
            Some(ids) => Cow::Borrowed(ids.as_slice()),
            None => Cow::Owned(chart::device_ids(&self.shots)),
        }
    }

    /// Key of the view's members around the shown shot `shown`.
    pub(super) fn members_key(&self, shown: usize) -> MembersKey {
        MembersKey {
            shots_rev: self.shots_rev,
            groups_rev: self.groups.rev(),
            view: self.groups.mode,
            device: self.device_filter.clone(),
            shown: Some(shown),
            club: self.selected_club,
        }
    }

    pub(crate) fn render_shots_panel(&mut self, ui: &mut egui::Ui) {
        self.refresh_shot_caches();
        self.render_shots_toolbar(ui);
        ui.add_space(8.0);

        // The right-hand column and the shot list are laid out first so
        // they run the full height; the bottom row then spans the width
        // between them. Chart sizes are fixed for a window size.
        let view = self.shots_view;
        let slots = chart_slots(view, ui.ctx().content_rect().size());
        let gap = CHART_GAP as i8;
        if let Some(width) = slots.right {
            egui::SidePanel::right(egui::Id::new("shots_right"))
                .resizable(false)
                .exact_width(width)
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    left: gap,
                    ..Default::default()
                }))
                .show_inside(ui, |ui| self.render_chart(ui, Chart::Dispersion));
        }
        if view.shot_list() {
            let default = (ui.available_width() * 0.18)
                .clamp(*LIST_DEFAULT_WIDTH.start(), *LIST_DEFAULT_WIDTH.end());
            egui::SidePanel::left(egui::Id::new(("shots_list", view)))
                .resizable(true)
                .show_separator_line(false)
                .default_width(default)
                .width_range(LIST_MIN_WIDTH..=LIST_MAX_WIDTH)
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    right: 8,
                    ..Default::default()
                }))
                .show_inside(ui, |ui| self.render_shot_list(ui));
        }
        if let Some((height, row)) = slots.bottom {
            let filter = row == BottomRow::Dispersion
                && self.dispersion_filter(view == ShotsView::Table).is_some();
            let height = bottom_height(height, row, ui.available_width(), filter);
            egui::TopBottomPanel::bottom(egui::Id::new(("shots_bottom", view)))
                .resizable(false)
                .exact_height(height)
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    top: gap,
                    ..Default::default()
                }))
                .show_inside(ui, |ui| match row {
                    BottomRow::Flight => self.render_chart(ui, Chart::Flight),
                    BottomRow::Dispersion => self.render_chart(ui, Chart::Dispersion),
                    BottomRow::FlightAndDispersion => {
                        let rect = ui.available_rect_before_wrap();
                        let (flight, _) = row_split(rect.width());
                        let left = rect.with_max_x(rect.left() + flight);
                        let right = rect.with_min_x(left.right() + CHART_GAP);
                        for (rect, chart) in [(left, Chart::Flight), (right, Chart::Dispersion)] {
                            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                                self.render_chart(ui, chart);
                            });
                        }
                    }
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show_inside(ui, |ui| match view {
                ShotsView::Table => self.render_shots_table(ui),
                ShotsView::Cards => self.render_shot_cards(ui),
            });
    }

    /// Draw `chart` in its panel `ui`.
    fn render_chart(&mut self, ui: &mut egui::Ui, chart: Chart) {
        match chart {
            Chart::Flight => self.render_flight_panel(ui),
            Chart::Dispersion => {
                let table = self.shots_view == ShotsView::Table;
                self.render_dispersion_panel(ui, table);
            }
        }
    }

    fn render_shots_toolbar(&mut self, ui: &mut egui::Ui) {
        let imperial = self.units_toggle == UnitSystem::Imperial;
        // A fixed row height, taller than any item, so every item centres on
        // the same line.
        let row = egui::vec2(ui.available_width(), 38.0);
        let layout = egui::Layout::left_to_right(egui::Align::Center);
        ui.allocate_ui_with_layout(row, layout, |ui| {
            ui.label(theme::caps("View", 11.0, theme::TEXT_MUTED));
            let current = ShotsView::ALL
                .iter()
                .position(|v| *v == self.shots_view)
                .unwrap_or(0);
            let labels: Vec<&str> = ShotsView::ALL.iter().map(|v| v.label()).collect();
            if let Some(i) = theme::segmented(ui, &labels, current, None).inner {
                self.shots_view = ShotsView::ALL[i];
            }
            ui.add_space(12.0);
            ui.label(theme::caps("Units", 11.0, theme::TEXT_MUTED));
            match theme::segmented(ui, &["Imperial", "Metric"], usize::from(!imperial), None).inner
            {
                Some(0) => self.units_toggle = UnitSystem::Imperial,
                Some(_) => self.units_toggle = UnitSystem::Metric,
                None => {}
            }
            if self.shots_view.shows_simulated_legend() {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("italic = simulated")
                        .italics()
                        .color(theme::TEXT_MUTED)
                        .size(12.0),
                )
                .on_hover_text(
                    "Values simulated from the measured launch, not measured by the device. \
                     Offline is + right of the target line.",
                );
            }

            let units = self.units_toggle;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if clear_history_button(ui, !self.shots.is_empty()).clicked() {
                    self.confirm_clear = true;
                }
                ui.add_space(4.0);
                ui.hyperlink_to("CSV", net::export_url("csv", units))
                    .on_hover_text("Decoded shots, one row each. No raw frames.");
                ui.hyperlink_to("JSONL", net::export_url("jsonl", units))
                    .on_hover_text(EXPORT_HINT);
                ui.label(theme::caps("Export session", 11.0, theme::TEXT_MUTED));
            });
        });
        if self.shots.is_empty() {
            self.confirm_clear = false;
        }
        if self.confirm_clear {
            self.render_clear_confirm(ui.ctx());
        }
    }

    /// The Clear history confirmation, over everything. Delete sends the
    /// command; Cancel, Escape or a click outside closes it.
    fn render_clear_confirm(&mut self, ctx: &egui::Context) {
        let modal = egui::Modal::new(egui::Id::new("clear_history_confirm"))
            .frame(theme::card().inner_margin(egui::Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(CLEAR_CONFIRM_WIDTH);
                ui.label(
                    egui::RichText::new(clear_confirm_title(self.shots.len()))
                        .size(17.0)
                        .strong()
                        .color(theme::TEXT),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "This can't be undone. Export first if you want to keep them.",
                    )
                    .color(theme::TEXT_MUTED),
                );
                ui.add_space(18.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let delete = ui.add(theme::danger_button("Delete", 15.0)).clicked();
                    let cancel = ui.button("Cancel").clicked();
                    (delete, cancel)
                })
                .inner
            });
        let (delete, cancel) = modal.inner;
        if delete {
            self.send_group_command(ctx, GroupCommand::ClearHistory);
            self.confirm_clear = false;
        } else if cancel || modal.should_close() {
            self.confirm_clear = false;
        }
    }

    /// Label of the device behind `shot` ([`chart::device_label`]), when
    /// `show` (the session has shots from more than one device).
    pub(super) fn device_name<'a>(&'a self, shot: &'a ShotRow, show: bool) -> Option<&'a str> {
        show.then(|| {
            let name = self.actors.get(&shot.actor).map(|a| a.name.as_str());
            chart::device_label(&shot.actor, name)
        })
    }

    /// The shot header line: number, device name, and either the LATEST
    /// chip or, on an older shot, a Back to latest chip. Returns whether
    /// Back to latest was clicked.
    fn render_shot_header(&self, ui: &mut egui::Ui, i: usize, following: bool) -> bool {
        let shot = &self.shots[i];
        let show_device = multiple_devices(&self.shots);
        let mut back = false;
        ui.horizontal_centered(|ui| {
            ui.label(
                egui::RichText::new(format!("Shot #{}", shot.shot_number))
                    .size(22.0)
                    .strong()
                    .color(theme::TEXT),
            );
            if let Some(name) = self.device_name(shot, show_device) {
                ui.label(
                    egui::RichText::new(name)
                        .size(15.0)
                        .color(theme::TEXT_MUTED),
                );
            }
            ui.add_space(4.0);
            match header_chip(&self.shots, i, following) {
                HeaderChip::BackToLatest => {
                    back = theme::jump_chip(ui, "Back to latest").clicked();
                }
                HeaderChip::Latest => {
                    theme::badge(ui, "LATEST", theme::GOLD);
                }
                HeaderChip::None => {}
            }
        });
        back
    }

    /// The Cards view's shot list: the count line and the view selector on
    /// top; one entry per shot, newest first, under the view's headers,
    /// scrolling in the middle; and, outside the view `none`, the hint line
    /// and + New set / New session pinned at the foot. Clicking an entry
    /// shows that shot.
    fn render_shot_list(&mut self, ui: &mut egui::Ui) {
        let inset = f32::from(GLOW_WIDTH);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), LIST_HEADER_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_space(inset);
                ui.label(theme::caps("Shots", 11.0, theme::TEXT_MUTED));
                ui.label(
                    egui::RichText::new(self.shots.len().to_string())
                        .size(13.0)
                        .color(theme::TEXT),
                );
            },
        );
        self.render_view_selector(ui, inset);

        let strip = self.groups.strip(self.selected_club);
        let foot = list_foot_height(&strip);
        let list_height = if foot > 0.0 {
            (ui.available_height() - foot - ui.spacing().item_spacing.y).max(0.0)
        } else {
            ui.available_height()
        };
        let imperial = self.units_toggle == UnitSystem::Imperial;
        let show_device = multiple_devices(&self.shots);
        let focus = self.shot_focus.index(&self.shots);
        let latest = latest_index(&self.shots);
        let reveal = self.shot_focus.take_reveal();
        let pulse = self.shot_focus.strength(ui.ctx().input(|i| i.time));
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .id_salt("shot_list")
            .auto_shrink(false)
            .max_height(list_height)
            .show(ui, |ui| {
                // Room for the glow around the entries.
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(GLOW_WIDTH as i8))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = ENTRY_GAP;
                        if self.shots.is_empty() {
                            ui.label(
                                egui::RichText::new("No shots yet")
                                    .size(13.0)
                                    .color(theme::TEXT_DIM),
                            );
                        }
                        let mut first = true;
                        for item in list_items(&self.shots, &self.groups, self.selected_club) {
                            if !matches!(item, ListItem::Shot(_)) && !first {
                                ui.add_space(HEADER_LEAD);
                            }
                            first = false;
                            let i = match item {
                                ListItem::Shot(i) => i,
                                ListItem::GroupHeader { id, next } => {
                                    self.paint_group_header(ui, &id, next);
                                    continue;
                                }
                                ListItem::ClubHeader {
                                    session,
                                    club,
                                    next,
                                } => {
                                    self.paint_club_header(ui, session, club, next);
                                    continue;
                                }
                                ListItem::Ungrouped => {
                                    let size =
                                        egui::vec2(ui.available_width(), GROUP_HEADER_HEIGHT);
                                    let (rect, _) =
                                        ui.allocate_exact_size(size, egui::Sense::hover());
                                    list_header(
                                        ui.painter(),
                                        rect,
                                        "Not in a set",
                                        None,
                                        theme::TEXT_DIM,
                                    );
                                    continue;
                                }
                            };
                            let size = egui::vec2(ui.available_width(), ENTRY_HEIGHT);
                            let (rect, response) =
                                ui.allocate_exact_size(size, egui::Sense::click());
                            let selected = Some(i) == focus;
                            if reveal && selected {
                                ui.scroll_to_rect(rect.expand(inset), None);
                            }
                            if ui.is_rect_visible(rect.expand(inset))
                                && let Some(shown) = self.display_row(i)
                            {
                                let shot = &self.shots[i];
                                let entry = ListEntry::new(
                                    &shown,
                                    self.device_name(shot, show_device),
                                    imperial,
                                );
                                let is_latest = Some(i) == latest;
                                let state = EntryState {
                                    selected,
                                    hovered: response.hovered(),
                                    latest: is_latest,
                                    pulse: if is_latest { pulse } else { 0.0 },
                                };
                                list_entry(ui, rect, &entry, state);
                            }
                            if response
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                clicked = Some(i);
                            }
                        }
                    });
            });
        if let Some(i) = clicked {
            self.shot_focus.select(&self.shots, i);
        }
        if strip.actions {
            self.render_list_foot(ui, inset, &strip);
        }
    }

    /// The view selector under the list's count line.
    fn render_view_selector(&mut self, ui: &mut egui::Ui, inset: f32) {
        let width = ui.available_width() - 2.0 * inset;
        let mode = self.groups.mode;
        let mut picked = None;
        ui.horizontal(|ui| {
            ui.add_space(inset);
            ui.vertical(|ui| {
                ui.set_width(width);
                let current = GROUPING_MODES
                    .iter()
                    .position(|(m, _)| *m == mode)
                    .unwrap_or(0);
                let labels = GROUPING_MODES.map(|(_, label)| label);
                let response = ui
                    .scope(|ui| {
                        picked = theme::segmented_fill(ui, &labels, current, 12.0);
                    })
                    .response;
                response.on_hover_text(GROUPING_HINT);
            });
        });
        ui.add_space(6.0);
        if let Some(i) = picked {
            let ctx = ui.ctx().clone();
            self.set_grouping(&ctx, GROUPING_MODES[i].0);
        }
    }

    /// The list's foot, pinned under the scrolling entries: a hairline, the
    /// hint line when `strip` has one, then + New set and New session
    /// sharing the width.
    fn render_list_foot(&mut self, ui: &mut egui::Ui, inset: f32, strip: &Strip) {
        let width = ui.available_width() - 2.0 * inset;
        let view = self.groups.mode;
        let mut new_group = false;
        let mut new_session = false;
        ui.horizontal(|ui| {
            ui.add_space(inset);
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = FOOT_SPACING;
                let (line, _) =
                    ui.allocate_exact_size(egui::vec2(width, FOOT_RULE), egui::Sense::hover());
                ui.painter().hline(
                    line.x_range(),
                    line.top(),
                    egui::Stroke::new(1.0, theme::BORDER),
                );
                if let Some(hint) = &strip.hint {
                    let size = egui::vec2(width, STRIP_HINT_HEIGHT);
                    ui.allocate_ui_with_layout(
                        size,
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(hint)
                                    .size(11.0)
                                    .color(theme::TEXT_MUTED),
                            );
                        },
                    );
                }
                let gap = 8.0;
                let chip = egui::vec2((width - gap) / 2.0, STRIP_BUTTONS_HEIGHT);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    let group = theme::chip_button(ui, "+ New set", strip.new_group, chip);
                    new_group = if strip.new_group {
                        group.on_hover_text(new_group_hint(view)).clicked()
                    } else {
                        group.on_hover_text("The open set has no shots yet.");
                        false
                    };
                    let session = theme::chip_button(ui, "New session", strip.new_session, chip);
                    new_session = if strip.new_session {
                        session.on_hover_text(NEW_SESSION_HINT).clicked()
                    } else {
                        session.on_hover_text("No set is open.");
                        false
                    };
                });
            });
        });

        let ctx = ui.ctx().clone();
        if new_group {
            self.send_group_command(&ctx, GroupCommand::NewGroup);
        }
        if new_session {
            self.send_group_command(&ctx, GroupCommand::NewSession);
        }
    }

    /// Switch the view: applied here at once and saved as the default view,
    /// over the WebSocket when it is up, else over REST.
    fn set_grouping(&mut self, ctx: &egui::Context, mode: ShotGrouping) {
        self.groups.mode = mode;
        if let Some(tx) = &mut self.ws_sender {
            let msg = serde_json::json!({ "cmd": "grouping", "mode": mode }).to_string();
            tx.send(ewebsock::WsMessage::Text(msg));
        } else {
            net::post_grouping(ctx, mode);
        }
    }

    /// Send `command` over the WebSocket when it is up, else over REST.
    /// Without the socket this UI hears none of the bus events that follow,
    /// so it applies the command itself (see `apply_posted`).
    fn send_group_command(&mut self, ctx: &egui::Context, command: GroupCommand) {
        if let Some(tx) = &mut self.ws_sender {
            let cmd = command.ws_cmd();
            tx.send(ewebsock::WsMessage::Text(format!(r#"{{"cmd":"{cmd}"}}"#)));
            return;
        }
        match command {
            GroupCommand::NewGroup => net::post_new_group(ctx, &self.pending),
            GroupCommand::NewSession => net::post_new_session(ctx, &self.pending),
            GroupCommand::ClearHistory => net::post_clear_history(ctx),
        }
        self.apply_posted(command);
    }

    /// What a group command sent over REST does to this UI at once: Clear
    /// history empties the Shots tab and New session starts the UI's
    /// session, as their bus events do. The sets are fetched again once a
    /// New set or New session is answered.
    pub(crate) fn apply_posted(&mut self, command: GroupCommand) {
        match command {
            GroupCommand::NewGroup => {}
            GroupCommand::NewSession => self.start_new_session(),
            GroupCommand::ClearHistory => self.clear_shot_history(),
        }
    }

    /// Paint the By set header of set `id`: accent while it is open, muted
    /// once closed, gold over the empty set the next shot joins (`next`).
    fn paint_group_header(&self, ui: &mut egui::Ui, id: &str, next: bool) {
        let size = egui::vec2(ui.available_width(), GROUP_HEADER_HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
        let Some(group) = self.groups.get(id) else {
            return;
        };
        let color = match (next, group.open) {
            (true, _) => theme::GOLD,
            (false, true) => theme::ACCENT,
            (false, false) => theme::TEXT_MUTED,
        };
        let time = clock_time(&group.started_at);
        if ui.is_rect_visible(rect) {
            list_header(
                ui.painter(),
                rect,
                &set_title(group, next),
                time.as_deref(),
                color,
            );
        }
        let state = if group.open { "open" } else { "closed" };
        let mut hover = if next {
            format!("Set {}: the next shot joins it", group.number)
        } else {
            format!("Set {}, {state}", group.number)
        };
        if let Some(time) = &time {
            hover.push_str(&format!(". Started {time}."));
        }
        response.on_hover_text(hover);
    }

    /// Paint the By club header of `club`'s section in `session`: accent in
    /// the current session, muted for an earlier one, gold over the club
    /// whose first shot of the session is next (`next`). The time is when
    /// the section's first set started.
    fn paint_club_header(&self, ui: &mut egui::Ui, session: u32, club: Option<Club>, next: bool) {
        let size = egui::vec2(ui.available_width(), GROUP_HEADER_HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
        let sets: Vec<&ShotGroupInfo> = self
            .groups
            .iter_sets()
            .filter(|g| g.session == session && g.club == club)
            .collect();
        let counted = sets.iter().filter(|g| g.shots > 0).count();
        let shots: u32 = sets.iter().map(|g| g.shots).sum();
        let earlier = session < self.groups.session();
        let tally = (!next).then_some((counted, shots as usize));
        let color = match (next, earlier) {
            (true, _) => theme::GOLD,
            (false, true) => theme::TEXT_MUTED,
            (false, false) => theme::ACCENT,
        };
        let time = sets.first().and_then(|g| clock_time(&g.started_at));
        if ui.is_rect_visible(rect) {
            list_header(
                ui.painter(),
                rect,
                &club_header_title(club, earlier, tally),
                time.as_deref(),
                color,
            );
        }
        let name = club.map_or("No club", club_name);
        let sets = match counted {
            1 => "1 set".to_owned(),
            n => format!("{n} sets"),
        };
        let hover = match (next, earlier) {
            (true, _) => format!("{name}: the next shot starts its section"),
            (false, true) => format!("{name}, an earlier session: {sets}"),
            (false, false) => format!("{name} since the last New session: {sets}"),
        };
        response.on_hover_text(hover);
    }

    fn render_shot_cards(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.shot_focus.index(&self.shots) else {
            empty_state(
                ui,
                "The latest shot's numbers appear here as soon as it lands.",
            );
            return;
        };

        let imperial = self.units_toggle == UnitSystem::Imperial;
        let following = self.shot_focus.following(&self.shots);
        let Some(shot) = self.display_row(i) else {
            return;
        };
        let now = ui.ctx().input(|inp| inp.time);
        // The glow is for the latest shot, which is on show while it lasts
        // unless an older shot is picked in the meantime.
        let pulse = if following {
            self.shot_focus.strength(now)
        } else {
            0.0
        };
        if pulse > 0.0 {
            ui.ctx().request_repaint();
        }
        let (headline, groups) = card_metrics(&shot, imperial);
        let counts: Vec<usize> = groups.iter().map(|(_, m)| m.len()).collect();
        let height = ui.available_height();
        // The range bars over the view's members around the shown shot,
        // after the device filter. The view none tracks only the last shot:
        // no bars and no device filter.
        let view = self.groups.mode;
        let show_bars = shows_bars(view);
        let set = bar_group(&self.groups, Some(&self.shots[i]), self.selected_club);
        let key = self.members_key(i);
        // A handful of ids, copied so the borrow of `self` ends here.
        let devices = self.devices().into_owned();
        let (bars, title) = {
            let members = self.view_members.get(key, || {
                chart::members_for(
                    &self.shots,
                    &self.groups,
                    view,
                    set,
                    Some(i),
                    self.device_filter.as_deref(),
                )
            });
            // With every device shown, two or more split the bands by device.
            let split: &[String] = if self.device_filter.is_none() {
                &devices
            } else {
                &[]
            };
            (
                bars_for(&self.shots, members, Some(i), split, imperial),
                bars_title(
                    view,
                    set,
                    self.groups.session(),
                    &self.shots,
                    members,
                    self.device_filter.is_some(),
                ),
            )
        };
        let device_labels = device_options(&devices, &self.actors);
        // Legend chips for the per-device sub-bands.
        let chips: Vec<(String, egui::Color32)> = match (&device_labels, &self.device_filter) {
            (Some(labels), None) => labels
                .iter()
                .skip(1)
                .zip(&devices)
                .map(|(label, id)| (label.clone(), chart::device_colour(&devices, id)))
                .collect(),
            _ => Vec::new(),
        };
        let device_index = self
            .device_filter
            .as_ref()
            .and_then(|d| devices.iter().position(|id| id == d))
            .map_or(0, |p| p + 1);
        let bars_note = if self.groups.is_empty() {
            "Sets start with your first shot"
        } else {
            "The next shot starts a new set"
        };

        // Room for the glow on the outermost cards.
        let inset = f32::from(GLOW_WIDTH);
        let lay_out = |width: f32| {
            let legend = legend_place(
                ui.painter(),
                width,
                title.as_ref().map(|(t, _)| t.as_str()),
                &chips,
            );
            cards_layout(
                egui::vec2(width, height - 2.0 * inset - CARDS_FOOT),
                headline.len(),
                &counts,
                show_bars.then_some(legend),
            )
        };
        let mut layout = lay_out(ui.available_width() - 2.0 * inset);
        // Too short for everything: a solid scroll bar, always shown, with
        // the cards laid out beside it.
        let scroll = egui::style::ScrollStyle::solid();
        if layout.scrolls {
            let bar = scroll.bar_width + scroll.bar_inner_margin + scroll.bar_outer_margin;
            layout = lay_out(ui.available_width() - 2.0 * inset - bar);
        }
        let visibility = if layout.scrolls {
            egui::scroll_area::ScrollBarVisibility::AlwaysVisible
        } else {
            egui::scroll_area::ScrollBarVisibility::AlwaysHidden
        };

        let mut back = false;
        let mut picked_device = None;
        ui.scope(|ui| {
            ui.spacing_mut().scroll = scroll;
            egui::ScrollArea::vertical()
                .id_salt("shot_cards")
                .auto_shrink(false)
                .scroll_bar_visibility(visibility)
                .show(ui, |ui| {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(
                            ui.available_width(),
                            layout.height + 2.0 * inset + CARDS_FOOT,
                        ),
                        egui::Sense::hover(),
                    );
                    let origin = (rect.min + egui::vec2(inset, inset)).to_vec2();

                    back = ui
                        .scope_builder(
                            egui::UiBuilder::new().max_rect(layout.header.translate(origin)),
                            |ui| self.render_shot_header(ui, i, following),
                        )
                        .inner;

                    if let Some(first) = layout.headline.first() {
                        let look = CardLook::headline(first.size()).fit(
                            ui.painter(),
                            &headline,
                            first.width(),
                        );
                        for (m, r) in headline.iter().zip(&layout.headline) {
                            card(ui, r.translate(origin), m, look, pulse);
                        }
                    }
                    if let Some(at) = layout.bars.map(|b| b.translate(origin)) {
                        if let Some(labels) = &device_labels {
                            picked_device = ui
                                .scope_builder(
                                    egui::UiBuilder::new()
                                        .max_rect(at.controls)
                                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                                    |ui| bar_controls(ui, labels, device_index),
                                )
                                .inner;
                        }
                        range_bars(ui, &at, title.as_ref(), &bars, &chips, bars_note);
                    }
                    let detail_cards = layout.groups.iter().flat_map(|(_, cards)| cards);
                    if let Some(first) = detail_cards.clone().next() {
                        let all = groups.iter().flat_map(|(_, m)| m);
                        let look =
                            CardLook::detail(first.size()).fit(ui.painter(), all, first.width());
                        for ((title, metrics), (label, cards)) in groups.iter().zip(&layout.groups)
                        {
                            group_label(
                                ui.painter(),
                                label.translate(origin),
                                title,
                                theme::ACCENT,
                            );
                            for (m, r) in metrics.iter().zip(cards) {
                                card(ui, r.translate(origin), m, look, pulse);
                            }
                        }
                    }
                });
        });
        if back {
            self.shot_focus.follow();
        }
        if let Some(d) = picked_device {
            self.device_filter = d.checked_sub(1).and_then(|p| devices.get(p).cloned());
        }
    }

    fn render_shots_table(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        let imperial = self.units_toggle == UnitSystem::Imperial;
        let show_device = multiple_devices(&self.shots);

        if self.shots.is_empty() {
            empty_state(ui, "Each shot appears here as a row as soon as it lands.");
            return;
        }

        let last = self.shots.len() - 1;
        let pulse = self.shot_focus.strength(ui.ctx().input(|i| i.time));
        if pulse > 0.0 {
            ui.ctx().request_repaint();
        }
        let look = RowLook {
            latest: theme::HEADING.gamma_multiply(0.07 + 0.05 * pulse),
            stripe: ui.visuals().faint_bg_color,
            pulse,
        };
        let columns = table_columns(show_device, imperial);
        let header = TableHeader::layout(ui.painter(), &columns);
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + TABLE_ROW_PAD;

        // One scroll area for both directions, so both scroll bars sit on
        // the view's edges. The table lays out only the rows in view, and
        // the header is painted over the top of the view once they are in.
        egui::ScrollArea::both()
            .id_salt("shots_table")
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(TABLE_COLUMN_GAP, TABLE_ROW_GAP);
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                let origin = ui.cursor().min;
                ui.add_space(header.height + TABLE_ROW_GAP);
                let mut widths = Vec::new();
                let mut table = TableBuilder::new(ui)
                    // A new column set or unit system sizes the columns afresh.
                    .id_salt(("shots_table", show_device, imperial))
                    .vscroll(false)
                    .striped(false)
                    .resizable(false)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
                for width in &header.widths {
                    table = table.column(Column::auto().at_least(width.max(TABLE_MIN_COLUMN)));
                }
                table.body(|body| {
                    widths = body.widths().to_vec();
                    body.rows(row_height, self.shots.len(), |mut row| {
                        let idx = row.index();
                        let Some(converted) = self.display_row(idx) else {
                            return;
                        };
                        let shot = &self.shots[idx];
                        let device = self.device_name(shot, show_device);
                        let set = shot
                            .group
                            .as_deref()
                            .and_then(|g| self.groups.get(g))
                            .map(|g| g.number);
                        let cells = table_cells(&converted, device, set, idx == last);
                        table_row(&mut row, &columns, cells, look.for_row(idx, last));
                    });
                });
                header.paint(ui, origin, &columns, &widths);
            });
    }
}

/// A Table view column: its header name and unit, and how far its cells
/// sit in from its left edge.
struct TableColumn {
    name: &'static str,
    unit: &'static str,
    inset: f32,
}

/// The Table view's columns: Device (when `show_device`), `#`, Club, the
/// ball columns, then the club columns.
fn table_columns(show_device: bool, imperial: bool) -> Vec<TableColumn> {
    let (speed, dist, height) = table_header_units(imperial);
    let column = |name, unit| TableColumn {
        name,
        unit,
        inset: 0.0,
    };
    let mut columns = Vec::with_capacity(TABLE_COLUMNS + 1);
    if show_device {
        columns.push(TableColumn {
            inset: column_inset(LeadColumn::Device, show_device),
            ..column("Device", "")
        });
    }
    columns.push(TableColumn {
        inset: column_inset(LeadColumn::Number, show_device),
        ..column("#", "")
    });
    columns.extend([
        column("Club", ""),
        column("Ball", speed),
        column("VLA", "deg"),
        column("HLA", "deg"),
        column("Carry", dist),
        column("Height", height),
        column("Offline", dist),
        column("Desc", "deg"),
        column("Roll", dist),
        column("Total", dist),
        column("T.Offline", dist),
        column("Back", "rpm"),
        column("Side", "rpm"),
        column("Club Spd", speed),
        column("Path", "deg"),
        column("AoA", "deg"),
        column("Face", "deg"),
        column("Loft", "deg"),
        column("Smash", ""),
    ]);
    columns
}

/// The Table view's header, laid out: two lines per column (name over
/// unit), and the width each column needs for them.
struct TableHeader {
    height: f32,
    widths: Vec<f32>,
}

impl TableHeader {
    const NAME: f32 = 12.5;
    const UNIT: f32 = 11.0;

    fn layout(painter: &egui::Painter, columns: &[TableColumn]) -> Self {
        let name = egui::FontId::proportional(Self::NAME);
        let unit = egui::FontId::proportional(Self::UNIT);
        let height = painter.fonts_mut(|f| f.row_height(&name) + f.row_height(&unit));
        let widths = columns
            .iter()
            .map(|c| {
                let n = painter.layout_no_wrap(c.name.to_owned(), name.clone(), theme::TEXT);
                let u = painter.layout_no_wrap(c.unit.to_owned(), unit.clone(), theme::TEXT);
                c.inset + n.size().x.max(u.size().x)
            })
            .collect();
        Self { height, widths }
    }

    /// Paint the header over the top of the view: at `origin` (the table's
    /// top-left), or pinned to the view's top once that has scrolled out.
    /// The columns are `widths` wide. It takes the pointer, so the rows
    /// under it show no hover.
    fn paint(&self, ui: &egui::Ui, origin: egui::Pos2, columns: &[TableColumn], widths: &[f32]) {
        use egui::{FontId, pos2};

        let gap = ui.spacing().item_spacing;
        let top = ui.clip_rect().top().max(origin.y);
        let width = widths.iter().sum::<f32>() + gap.x * widths.len().saturating_sub(1) as f32;
        let band = egui::Rect::from_min_max(
            pos2(origin.x - gap.x / 2.0, top - gap.y / 2.0),
            pos2(
                origin.x + width + gap.x / 2.0,
                top + self.height + gap.y / 2.0,
            ),
        );
        let _ = ui.interact(band, ui.id().with("table_header"), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(band, 0.0, theme::SURFACE_RAISED);
        let mut x = origin.x;
        for (column, width) in columns.iter().zip(widths) {
            let name = painter.layout_no_wrap(
                column.name.to_owned(),
                FontId::proportional(Self::NAME),
                theme::TEXT,
            );
            let name_h = name.size().y;
            painter.galley(pos2(x + column.inset, top), name, theme::TEXT);
            let unit = painter.layout_no_wrap(
                column.unit.to_owned(),
                FontId::proportional(Self::UNIT),
                theme::TEXT_MUTED,
            );
            painter.galley(
                pos2(x + column.inset, top + name_h),
                unit,
                theme::TEXT_MUTED,
            );
            x += width + gap.x;
        }
    }
}

/// One Table view cell.
#[derive(Debug, PartialEq)]
enum TableCell {
    /// Plain text.
    Text(String),
    /// A muted label, with hover text when there is some.
    Muted(String, Option<String>),
    /// The shot number: gold and bold on the latest row.
    Number(u32, bool),
    /// A value picked by [`pick`] (italics when simulated), with an explicit
    /// sign when `signed`.
    Picked(Option<(f64, bool)>, bool),
}

impl TableCell {
    fn show(&self, ui: &mut egui::Ui) {
        match self {
            Self::Text(text) => {
                ui.label(text.as_str());
            }
            Self::Muted(text, hover) => {
                let label = ui.label(egui::RichText::new(text.as_str()).color(theme::TEXT_MUTED));
                if let Some(hover) = hover {
                    label.on_hover_text(hover.as_str());
                }
            }
            Self::Number(n, latest) => {
                let text = egui::RichText::new(n.to_string());
                ui.label(if *latest {
                    text.strong().color(theme::GOLD)
                } else {
                    text.color(theme::TEXT_MUTED)
                });
            }
            Self::Picked(value, signed) => cell(ui, *value, 1, *signed),
        }
    }
}

/// The Table view's cells for `shot`, already in display units, in
/// [`table_columns`] order: `device` (its display name, when the Device
/// column shows), its set number for the Club cell's hover, and whether it
/// is the latest row.
fn table_cells(
    shot: &ShotRow,
    device: Option<&str>,
    set: Option<u32>,
    latest: bool,
) -> Vec<TableCell> {
    use TableCell::{Muted, Number, Picked, Text};

    let mut cells = Vec::with_capacity(TABLE_COLUMNS + 1);
    if let Some(device) = device {
        cells.push(if device.is_empty() {
            Text("-".to_owned())
        } else {
            Muted(device.to_owned(), None)
        });
    }
    cells.push(Number(shot.shot_number, latest));
    let club = shot
        .club_selected
        .map_or_else(|| "-".to_owned(), |c| c.to_string());
    cells.push(Muted(club, set.map(|n| format!("Set {n}"))));

    let no_derived = DerivedFlight::default();
    let d = shot.derived.as_ref().unwrap_or(&no_derived);
    let v = |x: Option<Distance>| x.map(|x| x.value());
    match shot.ball.as_ref() {
        Some(f) => cells.extend([
            Text(opt_f(f.launch_speed.map(|v| v.value()), 1)),
            Text(opt_f(f.launch_elevation, 1)),
            Text(opt_f(f.launch_azimuth, 1)),
            Picked(pick(v(f.carry_distance), v(d.carry_distance)), false),
            Picked(pick(v(f.max_height), v(d.max_height)), false),
            Picked(pick(v(f.carry_offline), v(d.carry_offline)), true),
            Picked(pick(f.descent_angle, d.descent_angle), false),
            Picked(pick(v(f.roll_distance), v(d.roll_distance)), true),
            Picked(pick(v(f.total_distance), v(d.total_distance)), false),
            Picked(pick(v(f.total_offline), v(d.total_offline)), true),
            Text(opt_i(f.backspin_rpm)),
            Text(opt_i(f.sidespin_rpm)),
        ]),
        None => cells.extend((0..BALL_COLUMNS).map(|_| Text("-".to_owned()))),
    }
    match shot.club.as_ref() {
        Some(c) => {
            let ball_speed = shot.ball.as_ref().and_then(|b| b.launch_speed);
            cells.extend([
                Text(opt_f(c.club_speed.map(|v| v.value()), 1)),
                Text(opt_f(c.path, 1)),
                Text(opt_f(c.attack_angle, 1)),
                Text(opt_f(c.face_angle, 1)),
                Text(opt_f(c.dynamic_loft, 1)),
                Text(opt_f(smash(c.smash_factor, ball_speed, c.club_speed), 2)),
            ]);
        }
        None => cells.extend((0..CLUB_COLUMNS).map(|_| Text("-".to_owned()))),
    }
    cells
}

/// How the Table view paints its rows' backgrounds.
#[derive(Clone, Copy)]
struct RowLook {
    /// The latest row's tint.
    latest: egui::Color32,
    /// Every other row's background.
    stripe: egui::Color32,
    /// Strength of the new-shot glow, 0 to 1.
    pulse: f32,
}

/// One row's background: its fill, and for the latest row the gold stripe
/// and the glow's strength.
#[derive(Clone, Copy)]
struct RowPaint {
    fill: Option<egui::Color32>,
    latest: bool,
    pulse: f32,
}

impl RowLook {
    fn for_row(self, idx: usize, last: usize) -> RowPaint {
        RowPaint {
            fill: table_row_fill(idx, last, self.latest, self.stripe),
            latest: idx == last,
            pulse: self.pulse,
        }
    }
}

/// Add one Table view row of `cells` to `row`. The latest row gets a gold
/// stripe on its left edge and an outline that fades with the new-shot
/// glow.
fn table_row(
    row: &mut egui_extras::TableRow<'_, '_>,
    columns: &[TableColumn],
    cells: Vec<TableCell>,
    paint: RowPaint,
) {
    let count = cells.len();
    let mut left = None;
    for (i, (cell, column)) in cells.into_iter().zip(columns).enumerate() {
        row.col(|ui| {
            if let Some(fill) = paint.fill {
                fill_cell(ui, fill);
            }
            let rect = ui.max_rect();
            let first = *left.get_or_insert(rect.left());
            let band = rect.y_range().expand(TABLE_ROW_GAP / 2.0);
            if paint.latest && i == 0 {
                let bar = egui::Rect::from_x_y_ranges(first..=first + STRIPE_WIDTH, band);
                ui.painter()
                    .rect_filled(bar, STRIPE_WIDTH / 2.0, theme::GOLD);
            }
            if paint.latest && i + 1 == count && paint.pulse > 0.0 {
                ui.painter().rect_stroke(
                    egui::Rect::from_x_y_ranges(first..=rect.right(), band),
                    2.0,
                    egui::Stroke::new(1.0, theme::ACCENT.gamma_multiply(paint.pulse)),
                    egui::StrokeKind::Inside,
                );
            }
            inset_cell(ui, column.inset, |ui| cell.show(ui));
        });
    }
}

/// Background of table row `idx` (of `0..=last`): `latest` for the latest
/// row, `stripe` on every other row counting from the first, else none.
fn table_row_fill(
    idx: usize,
    last: usize,
    latest: egui::Color32,
    stripe: egui::Color32,
) -> Option<egui::Color32> {
    if idx == last {
        Some(latest)
    } else if idx % 2 == 1 {
        Some(stripe)
    } else {
        None
    }
}

/// Fill a table cell's background in `color`, out to the middle of the gaps
/// around it so a row's cells join into one band.
fn fill_cell(ui: &egui::Ui, color: egui::Color32) {
    use egui::emath::GuiRounding as _;

    let rect = ui
        .max_rect()
        .expand2(0.5 * ui.spacing().item_spacing)
        .round_ui();
    ui.painter().rect_filled(rect, 0.0, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BallFlight;

    fn row(n: u32, with_ball: bool) -> ShotRow {
        ShotRow {
            actor: "mock_monitor.0".into(),
            shot_id: format!("id{n}"),
            shot_number: n,
            ball: with_ball.then(BallFlight::default),
            club: None,
            derived: None,
            club_selected: None,
            group: None,
            finished: true,
        }
    }

    #[test]
    fn a_finished_shot_without_data_is_a_no_read() {
        let entry = |shot: &ShotRow| ListEntry::new(shot, None, true);
        assert!(entry(&row(1, false)).no_read);
        assert!(!entry(&row(1, true)).no_read);
        let club_only = ShotRow {
            club: Some(ClubData::default()),
            ..row(1, false)
        };
        assert!(!entry(&club_only).no_read);
        // Still in flight: no data yet, but not a no-read.
        let in_flight = ShotRow {
            finished: false,
            ..row(1, false)
        };
        assert!(!entry(&in_flight).no_read);
        assert!(!entry(&in_flight).has_data);
    }

    #[test]
    fn cards_show_newest_shot_with_data() {
        let shots = vec![row(1, true), row(2, true), row(3, false)];
        assert_eq!(latest_index(&shots), Some(1));
        assert_eq!(latest_index(&[row(1, false)]), None);
        assert_eq!(latest_index(&[]), None);
    }

    #[test]
    fn device_names_follow_one_rule_everywhere() {
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        let named = |name: &str| ActorStatusResponse {
            name: name.into(),
            status: crate::types::ActorStatus::Connected,
            telemetry: HashMap::new(),
        };
        app.actors.insert("mevo.0".into(), named("Mevo+"));
        app.actors.insert("square.0".into(), named(""));
        let shot = |actor: &str| ShotRow {
            actor: actor.into(),
            ..row(1, true)
        };
        // Name, else the actor id, as the device filter and tooltips label it.
        assert_eq!(app.device_name(&shot("mevo.0"), true), Some("Mevo+"));
        assert_eq!(app.device_name(&shot("square.0"), true), Some("square.0"));
        assert_eq!(app.device_name(&shot("r10.0"), true), Some("r10.0"));
        assert_eq!(app.device_name(&shot("mevo.0"), false), None);
        let ids = vec!["mevo.0".to_owned(), "square.0".to_owned()];
        assert_eq!(
            device_options(&ids, &app.actors),
            Some(vec!["All".into(), "Mevo+".into(), "square.0".into()])
        );
    }

    #[test]
    fn latest_chip_stays_while_a_trigger_has_no_data() {
        let mut shots = vec![row(1, true), row(2, true)];
        assert_eq!(header_chip(&shots, 1, true), HeaderChip::Latest);
        // A trigger lands; its data has not.
        shots.push(row(3, false));
        assert_eq!(header_chip(&shots, 1, true), HeaderChip::Latest);
        // An older shot pinned.
        assert_eq!(header_chip(&shots, 0, false), HeaderChip::BackToLatest);
        assert_eq!(header_chip(&shots, 0, true), HeaderChip::None);
        // The data lands: the new shot takes the chip.
        shots[2] = row(3, true);
        assert_eq!(header_chip(&shots, 2, true), HeaderChip::Latest);
        assert_eq!(header_chip(&shots, 1, true), HeaderChip::None);
    }

    #[test]
    fn pulse_starts_on_new_shot_and_fades() {
        let mut focus = ShotFocus::default();
        let mut shots = vec![row(1, true)];
        focus.sync(&shots);
        assert_eq!(focus.strength(0.0), 0.0);

        // A trigger without data yet does not change the latest shot.
        shots.push(row(2, false));
        focus.observe(&shots, 10.0);
        assert_eq!(focus.strength(10.0), 0.0);

        shots[1].ball = Some(BallFlight::default());
        focus.observe(&shots, 11.0);
        assert_eq!(focus.strength(11.0), 1.0);
        let mid = focus.strength(11.0 + PULSE_SECS / 2.0);
        assert!(mid > 0.0 && mid < 1.0);
        assert_eq!(focus.strength(11.0 + PULSE_SECS), 0.0);

        // Observing the same shot again does not restart the pulse.
        focus.observe(&shots, 20.0);
        assert_eq!(focus.strength(20.0), 0.0);
    }

    #[test]
    fn focus_index_prefers_listed_pin() {
        let shots = vec![row(1, true), row(2, true), row(3, false)];
        let pin = |n| ShotRef::of(&row(n, true));
        assert_eq!(focus_index(&shots, None), Some(1));
        assert_eq!(focus_index(&shots, Some(&pin(1))), Some(0));
        // A shot without data can be picked.
        assert_eq!(focus_index(&shots, Some(&pin(3))), Some(2));
        // A pin that is not listed falls back to the latest.
        assert_eq!(focus_index(&shots, Some(&pin(9))), Some(1));
        assert_eq!(focus_index(&[], Some(&pin(1))), None);
    }

    #[test]
    fn shot_ref_matches_without_ids() {
        let live = row(5, true);
        let mut loaded = row(5, true);
        loaded.shot_id.clear();
        let pin = ShotRef::of(&live);
        assert!(pin.matches(&live) && pin.matches(&loaded));
        assert!(ShotRef::of(&loaded).matches(&live));
        // Without an id on one side, the same number from another device is
        // not it.
        let mut other = loaded.clone();
        other.actor = "r10.0".into();
        assert!(!pin.matches(&other));
    }

    #[test]
    fn shot_ref_prefers_the_id() {
        let pin = ShotRef::of(&row(5, true));
        // Same actor and number, another id: a different shot.
        let mut reused = row(5, true);
        reused.shot_id = "other".into();
        assert!(!pin.matches(&reused));
        // Same id and actor, different number: the same shot.
        let mut renumbered = row(6, true);
        renumbered.shot_id = "id5".into();
        assert!(pin.matches(&renumbered));
        // Same id from another device: a different shot.
        renumbered.actor = "r10.0".into();
        assert!(!pin.matches(&renumbered));
    }

    #[test]
    fn selection_follows_latest_until_pinned() {
        let mut focus = ShotFocus::default();
        let mut shots = vec![row(1, true), row(2, true)];
        focus.sync(&shots);
        assert_eq!(focus.index(&shots), Some(1));
        assert!(focus.following(&shots));

        // Picking an older shot pins it.
        focus.select(&shots, 0);
        assert_eq!(focus.index(&shots), Some(0));
        assert!(!focus.following(&shots));
        // It stays pinned while a trigger without data comes in.
        shots.push(row(3, false));
        focus.observe(&shots, 1.0);
        assert_eq!(focus.index(&shots), Some(0));

        // The new shot's data lands: the view jumps to it, glowing.
        shots[2].ball = Some(BallFlight::default());
        focus.observe(&shots, 2.0);
        assert_eq!(focus.index(&shots), Some(2));
        assert!(focus.following(&shots));
        assert_eq!(focus.strength(2.0), 1.0);
        assert!(focus.take_reveal());
        assert!(!focus.take_reveal());

        // Picking the latest shot, or Back to latest, follows again.
        focus.select(&shots, 1);
        focus.select(&shots, 2);
        assert!(focus.following(&shots));
        focus.select(&shots, 0);
        focus.follow();
        assert!(focus.following(&shots));
    }

    #[test]
    fn pin_survives_reload_until_shot_is_gone() {
        let mut focus = ShotFocus::default();
        let shots = vec![row(1, true), row(2, true), row(3, true)];
        focus.sync(&shots);
        focus.select(&shots, 1);

        // A reload that still lists the shot keeps it, even with new shots,
        // at a new index and without shot ids, and does not pulse.
        let mut reloaded = vec![row(2, true), row(3, true), row(4, true)];
        for s in &mut reloaded {
            s.shot_id.clear();
        }
        focus.sync(&reloaded);
        assert_eq!(focus.index(&reloaded), Some(0));
        assert_eq!(focus.strength(0.0), 0.0);

        // Once it is gone the view falls back to the latest, and stays there.
        let trimmed = vec![row(3, true), row(4, true)];
        focus.sync(&trimmed);
        assert_eq!(focus.index(&trimmed), Some(1));
        assert!(focus.following(&trimmed));
        focus.sync(&reloaded);
        assert_eq!(focus.index(&reloaded), Some(2));
    }

    #[test]
    fn repeated_shot_numbers_after_a_device_reconnect_are_told_apart_by_id() {
        let shot = |n, id: &str| ShotRow {
            shot_id: id.into(),
            ..row(n, true)
        };
        let mut focus = ShotFocus::default();
        let mut shots = vec![shot(1, "a1"), shot(2, "a2")];
        focus.sync(&shots);
        focus.select(&shots, 0);

        // The device reconnects and its shot #1 lands: a new latest shot,
        // though the actor and number match an earlier one.
        shots.push(shot(1, "b1"));
        focus.observe(&shots, 5.0);
        assert_eq!(focus.strength(5.0), 1.0);
        assert_eq!(focus.index(&shots), Some(2));
        assert!(focus.following(&shots));

        // A pin on the earlier #1 finds that shot, not the newer #1, and
        // survives a reload.
        focus.select(&shots, 0);
        let reloaded = shots.clone();
        focus.sync(&reloaded);
        assert_eq!(focus.index(&reloaded), Some(0));
        assert!(ShotRef::of(&shots[0]).matches(&shots[0]));
        assert!(!ShotRef::of(&shots[0]).matches(&shots[2]));
        assert!(!ShotRef::of(&shots[2]).matches(&shots[0]));
        // The newer #1 is found as itself too.
        focus.select(&shots, 1);
        assert_eq!(focus.index(&shots), Some(1));
        assert_eq!(focus_index(&shots, Some(&ShotRef::of(&shots[2]))), Some(2));
    }

    #[test]
    fn list_is_newest_first() {
        let shots = vec![row(1, true), row(2, false), row(3, true)];
        assert_eq!(
            list_items(&shots, &ShotGroups::default(), None),
            vec![ListItem::Shot(2), ListItem::Shot(1), ListItem::Shot(0)]
        );
    }

    fn group(id: &str, number: u32, club: Club, open: bool, shots: u32) -> ShotGroupInfo {
        ShotGroupInfo {
            id: id.into(),
            number,
            session: 0,
            golfer: None,
            club: Some(club),
            started_at: "2026-10-26T15:41:28.300Z".into(),
            manual: false,
            open,
            shots,
        }
    }

    /// `g` in session `session`.
    fn sess(session: u32, g: ShotGroupInfo) -> ShotGroupInfo {
        ShotGroupInfo { session, ..g }
    }

    fn groups(mode: ShotGrouping, list: Vec<ShotGroupInfo>) -> ShotGroups {
        let session = list.iter().map(|g| g.session).max().unwrap_or(0);
        let mut g = ShotGroups::default();
        g.replace(crate::types::ShotGroupsResponse {
            server: "webserver.0".into(),
            mode,
            session,
            current: None,
            groups: list,
        });
        g
    }

    /// Rows oldest first, each `(set id, has data)`.
    fn grouped(rows: &[(Option<&str>, bool)]) -> Vec<ShotRow> {
        rows.iter()
            .zip(1..)
            .map(|(&(g, data), n)| ShotRow {
                group: g.map(str::to_owned),
                ..row(n, data)
            })
            .collect()
    }

    fn header(id: &str) -> ListItem {
        ListItem::GroupHeader {
            id: id.into(),
            next: false,
        }
    }

    fn club_header(session: u32, club: Club) -> ListItem {
        ListItem::ClubHeader {
            session,
            club: Some(club),
            next: false,
        }
    }

    #[test]
    fn view_none_has_no_headers() {
        let shots = grouped(&[(Some("a"), true), (Some("a"), true), (None, true)]);
        // Sets are recorded in every view; `none` shows no headers.
        let g = groups(
            ShotGrouping::None,
            vec![group("a", 1, Club::Iron7, true, 2)],
        );
        assert_eq!(
            list_items(&shots, &g, Some(Club::Iron7)),
            vec![ListItem::Shot(2), ListItem::Shot(1), ListItem::Shot(0)]
        );
    }

    #[test]
    fn by_set_runs_get_one_header_each() {
        let shots = grouped(&[
            (Some("a"), true),
            (Some("a"), true),
            (Some("a"), true),
            (Some("b"), true),
            (Some("b"), true),
        ]);
        let g = groups(
            ShotGrouping::BySet,
            vec![
                group("a", 1, Club::Iron7, false, 3),
                group("b", 2, Club::Driver, true, 2),
            ],
        );
        assert_eq!(
            list_items(&shots, &g, Some(Club::Driver)),
            vec![
                header("b"),
                ListItem::Shot(4),
                ListItem::Shot(3),
                header("a"),
                ListItem::Shot(2),
                ListItem::Shot(1),
                ListItem::Shot(0),
            ]
        );
    }

    #[test]
    fn by_club_combines_a_clubs_sets_into_one_section() {
        // 7I (a), DR (b), 7I (c): returning to the 7-iron started set c, and
        // By club shows both 7-iron sets together, newest first.
        let shots = grouped(&[
            (Some("a"), true),
            (Some("a"), true),
            (Some("b"), true),
            (Some("c"), true),
            (Some("c"), true),
        ]);
        let g = groups(
            ShotGrouping::ByClub,
            vec![
                group("a", 1, Club::Iron7, false, 2),
                group("b", 2, Club::Driver, false, 1),
                group("c", 3, Club::Iron7, true, 2),
            ],
        );
        let sevens = [
            club_header(0, Club::Iron7),
            ListItem::Shot(4),
            ListItem::Shot(3),
            ListItem::Shot(1),
            ListItem::Shot(0),
        ];
        let driver = [club_header(0, Club::Driver), ListItem::Shot(2)];
        assert_eq!(
            list_items(&shots, &g, Some(Club::Iron7)),
            [sevens.as_slice(), &driver].concat()
        );
        // The selected club's section comes first, though its last shot
        // is older.
        assert_eq!(
            list_items(&shots, &g, Some(Club::Driver)),
            [driver.as_slice(), &sevens].concat()
        );
        // + New set does not split the section: an empty open 7-iron set
        // adds no header while the club has a section.
        let mut list = vec![
            group("a", 1, Club::Iron7, false, 2),
            group("b", 2, Club::Driver, false, 1),
            group("c", 3, Club::Iron7, false, 2),
            group("m", 4, Club::Iron7, true, 0),
        ];
        let g = groups(ShotGrouping::ByClub, list.clone());
        assert_eq!(
            list_items(&shots, &g, Some(Club::Iron7)),
            [sevens.as_slice(), &driver].concat()
        );
        // Its next shot joins the same section.
        list[3].shots = 1;
        let mut more = shots.clone();
        more.push(ShotRow {
            group: Some("m".into()),
            ..row(6, true)
        });
        let g = groups(ShotGrouping::ByClub, list);
        let items = list_items(&more, &g, Some(Club::Iron7));
        assert_eq!(items[..2], [club_header(0, Club::Iron7), ListItem::Shot(5)]);
        assert_eq!(items[6], club_header(0, Club::Driver));
        // A dead trigger stays with the run it is in.
        let mut shots = shots;
        shots.insert(2, row(9, false));
        let g = groups(
            ShotGrouping::ByClub,
            vec![
                group("a", 1, Club::Iron7, false, 2),
                group("b", 2, Club::Driver, false, 1),
                group("c", 3, Club::Iron7, true, 2),
            ],
        );
        let items = list_items(&shots, &g, Some(Club::Iron7));
        assert_eq!(
            items[5..],
            [
                club_header(0, Club::Driver),
                ListItem::Shot(3),
                ListItem::Shot(2)
            ]
        );
    }

    #[test]
    fn by_club_starts_afresh_after_new_session() {
        // Session 0: 7I (a), DR (b). New session. Session 1: 7I (c).
        let shots = grouped(&[
            (Some("a"), true),
            (Some("a"), true),
            (Some("b"), true),
            (Some("c"), true),
            (Some("c"), true),
        ]);
        let mut g = groups(
            ShotGrouping::ByClub,
            vec![
                group("a", 1, Club::Iron7, false, 2),
                group("b", 2, Club::Driver, false, 1),
                sess(1, group("c", 3, Club::Iron7, true, 2)),
            ],
        );
        assert_eq!(g.session(), 1);
        assert_eq!(
            list_items(&shots, &g, Some(Club::Iron7)),
            vec![
                club_header(1, Club::Iron7),
                ListItem::Shot(4),
                ListItem::Shot(3),
                club_header(0, Club::Driver),
                ListItem::Shot(2),
                club_header(0, Club::Iron7),
                ListItem::Shot(1),
                ListItem::Shot(0),
            ]
        );
        // Re-selecting a club from the earlier session does not lift its
        // earlier section.
        g.observe_club(Some(Club::Driver), &shots);
        let ids = section_ids(&list_items(&shots, &g, Some(Club::PitchingWedge)));
        assert_eq!(
            ids,
            [
                (1, Some(Club::Iron7)),
                (0, Some(Club::Driver)),
                (0, Some(Club::Iron7))
            ]
        );
        // Right after New session nothing is combined with the earlier
        // session: every section reads earlier until the next shot.
        g.new_session();
        let items = list_items(&shots, &g, Some(Club::Iron7));
        assert_eq!(items[0], club_header(1, Club::Iron7));
        assert_eq!(
            club_title(Some(Club::Iron7), 1 < g.session(), Some((1, 2))),
            "7 Iron \u{b7} Earlier \u{b7} 2 shots"
        );
    }

    /// The By club section keys of the headers in `items`, top to bottom.
    fn section_ids(items: &[ListItem]) -> Vec<SectionKey> {
        items
            .iter()
            .filter_map(|i| match i {
                ListItem::ClubHeader {
                    session,
                    club,
                    next: false,
                } => Some((*session, *club)),
                _ => None,
            })
            .collect()
    }

    /// The first `n` rows of `all`: the history as the UI held it then.
    fn upto(all: &[ShotRow], n: usize) -> &[ShotRow] {
        &all[..n]
    }

    #[test]
    fn by_club_sections_follow_last_activity() {
        // 7I ×3 (a), then DR ×2 (b).
        let shots = grouped(&[
            (Some("a"), true),
            (Some("a"), true),
            (Some("a"), true),
            (Some("b"), true),
            (Some("b"), true),
        ]);
        let mut g = groups(
            ShotGrouping::ByClub,
            vec![
                group("a", 1, Club::Iron7, false, 3),
                group("b", 2, Club::Driver, true, 2),
            ],
        );
        g.observe_club(Some(Club::Iron7), upto(&shots, 0));
        g.observe_club(Some(Club::Driver), upto(&shots, 3));
        let ids = |g: &ShotGroups, club| {
            section_ids(&list_items(&shots, g, club))
                .iter()
                .map(|(_, c)| c.map_or("-".to_owned(), |c| c.to_string()))
                .collect::<Vec<_>>()
                .join(",")
        };
        // Selected club first; with a club that has no section, the newest
        // activity wins.
        assert_eq!(ids(&g, Some(Club::Driver)), "DR,7I");
        assert_eq!(ids(&g, Some(Club::Iron7)), "7I,DR");
        assert_eq!(ids(&g, Some(Club::PitchingWedge)), "DR,7I");
        // Re-selecting 7I with no new shot bumps its section above DR's
        // newer shots, after a switch away too.
        g.observe_club(Some(Club::Iron7), upto(&shots, 5));
        g.observe_club(Some(Club::PitchingWedge), upto(&shots, 5));
        assert_eq!(ids(&g, Some(Club::PitchingWedge)), "7I,DR");
        // So does selecting DR again.
        g.observe_club(Some(Club::Driver), upto(&shots, 5));
        g.observe_club(Some(Club::PitchingWedge), upto(&shots, 5));
        assert_eq!(ids(&g, Some(Club::PitchingWedge)), "DR,7I");
        // Observing the same club again is not a new selection.
        let before = g.selected_at(Some(Club::PitchingWedge), &shots);
        g.observe_club(Some(Club::PitchingWedge), &shots);
        assert_eq!(g.selected_at(Some(Club::PitchingWedge), &shots), before);
    }

    #[test]
    fn by_club_loose_rows_gather_last() {
        // History in no set and a row in an unknown set sit in one section
        // after the club sections; pending rows stay on top.
        let shots = grouped(&[
            (None, true),
            (Some("a"), true),
            (Some("b"), true),
            (None, true),
            (Some("c"), true),
            (None, false),
        ]);
        let g = groups(
            ShotGrouping::ByClub,
            vec![
                group("a", 1, Club::Iron7, false, 1),
                group("b", 2, Club::Driver, false, 1),
                group("c", 3, Club::Iron7, true, 1),
            ],
        );
        assert_eq!(
            list_items(&shots, &g, Some(Club::Iron7)),
            vec![
                ListItem::Shot(5),
                club_header(0, Club::Iron7),
                ListItem::Shot(4),
                ListItem::Shot(1),
                club_header(0, Club::Driver),
                ListItem::Shot(2),
                ListItem::Ungrouped,
                ListItem::Shot(3),
                ListItem::Shot(0),
            ]
        );
    }

    #[test]
    fn headers_read_per_view() {
        let open = group("a", 3, Club::Iron7, true, 5);
        let closed = group("b", 2, Club::Iron7, false, 4);
        let one = group("c", 4, Club::Driver, true, 1);
        let mut none = group("d", 5, Club::Iron7, true, 2);
        none.club = None;
        assert_eq!(set_title(&open, false), "Set 3 \u{b7} 7I \u{b7} 5 shots");
        assert_eq!(set_title(&closed, false), "Set 2 \u{b7} 7I \u{b7} 4 shots");
        assert_eq!(set_title(&one, true), "Set 4 \u{b7} DR \u{b7} Next shot");
        assert_eq!(
            set_title(&none, false),
            "Set 5 \u{b7} No club \u{b7} 2 shots"
        );

        let i7 = Some(Club::Iron7);
        assert_eq!(club_title(i7, false, Some((1, 5))), "7 Iron \u{b7} 5 shots");
        assert_eq!(
            club_title(i7, false, Some((2, 9))),
            "7 Iron \u{b7} 2 sets \u{b7} 9 shots"
        );
        assert_eq!(
            club_title(i7, true, Some((1, 4))),
            "7 Iron \u{b7} Earlier \u{b7} 4 shots"
        );
        assert_eq!(
            club_title(Some(Club::Driver), false, Some((1, 1))),
            "Driver \u{b7} 1 shot"
        );
        assert_eq!(club_title(i7, false, None), "7 Iron \u{b7} Next shot");
        assert_eq!(
            club_title(None, false, Some((1, 2))),
            "No club \u{b7} 2 shots"
        );
    }

    #[test]
    fn long_list_headers_keep_their_shot_count() {
        let i7 = Some(Club::Iron7);
        assert_eq!(
            club_header_title(i7, true, Some((2, 14))),
            "7 Iron \u{b7} Earlier \u{b7} 14 shots"
        );
        assert_eq!(
            club_header_title(i7, false, Some((2, 14))),
            "7 Iron \u{b7} 2 sets \u{b7} 14 shots"
        );
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            // The narrowest default list (212 px): the earlier 7-iron
            // section fits whole.
            let width = *LIST_DEFAULT_WIDTH.start() - 8.0 - 2.0 * f32::from(GLOW_WIDTH) - 10.0;
            let title = club_header_title(i7, true, Some((2, 14)));
            let (head, tail) = header_text(&painter, &title, width, theme::TEXT_MUTED);
            assert!(tail.is_none(), "{title} fits in {width}");
            assert!(head.size().x <= width);
            // A longer one is split before the count, which stays whole.
            let long = club_header_title(Some(Club::PitchingWedge), false, Some((3, 25)));
            let (head, tail) = header_text(&painter, &long, width, theme::TEXT_MUTED);
            let tail = tail.expect("the count is kept apart");
            assert_eq!(tail.text(), " \u{b7} 25 SHOTS");
            assert_eq!(head.text(), "PITCHING WEDGE \u{b7} 3 SETS");
            assert!(head.size().x + tail.size().x <= width + 0.5);
            // Narrower still, the head is cut and the whole still fits.
            let (head, tail) = header_text(&painter, &long, 140.0, theme::TEXT_MUTED);
            let tail = tail.expect("the count is kept apart");
            assert_eq!(tail.text(), " \u{b7} 25 SHOTS");
            assert!(head.size().x + tail.size().x <= 140.5);
        });
    }

    #[test]
    fn club_names_are_spelled_out() {
        let names: Vec<_> = [
            Club::Driver,
            Club::MiniDriver,
            Club::Wood3,
            Club::Hybrid4,
            Club::Iron7,
            Club::PitchingWedge,
            Club::GapWedge,
            Club::SandWedge,
            Club::LobWedge,
            Club::Putter,
        ]
        .map(club_name)
        .into();
        assert_eq!(
            names,
            [
                "Driver",
                "Mini Driver",
                "3 Wood",
                "4 Hybrid",
                "7 Iron",
                "Pitching Wedge",
                "Gap Wedge",
                "Sand Wedge",
                "Lob Wedge",
                "Putter",
            ]
        );
        // Every club has its own name.
        let mut all: Vec<_> = Club::ALL.iter().map(|&c| club_name(c)).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), Club::ALL.len());
    }

    #[test]
    fn empty_open_set_heads_the_list() {
        let shots = grouped(&[(Some("a"), true), (Some("a"), true)]);
        let list = vec![
            group("a", 1, Club::Iron7, false, 2),
            group("c", 3, Club::Iron7, true, 0),
        ];
        let next = ListItem::GroupHeader {
            id: "c".into(),
            next: true,
        };
        let g = groups(ShotGrouping::BySet, list.clone());
        let items = list_items(&shots, &g, Some(Club::Iron7));
        assert_eq!(
            items,
            vec![
                next.clone(),
                header("a"),
                ListItem::Shot(1),
                ListItem::Shot(0)
            ]
        );
        assert_eq!(
            set_title(g.get("c").unwrap(), true),
            "Set 3 \u{b7} 7I \u{b7} Next shot"
        );
        // It takes any club: a different club in play is still its header.
        assert_eq!(list_items(&shots, &g, Some(Club::Driver))[0], next);

        // By club: the club in play already has a section, which is the
        // first line; no extra header.
        let g = groups(ShotGrouping::ByClub, list);
        let items = list_items(&shots, &g, Some(Club::Iron7));
        assert_eq!(items[0], club_header(0, Club::Iron7));
        assert_eq!(items.len(), 3);
        // A club with no section yet this session gets a next header.
        let items = list_items(&shots, &g, Some(Club::Driver));
        assert_eq!(
            items[0],
            ListItem::ClubHeader {
                session: 0,
                club: Some(Club::Driver),
                next: true
            }
        );
        assert_eq!(items[1], club_header(0, Club::Iron7));
    }

    #[test]
    fn pending_and_ungrouped_rows_come_first() {
        // Oldest first: history in no set, the 7I set with a dead trigger
        // inside it, a row whose set event has not arrived yet, and a shot
        // in flight.
        let shots = grouped(&[
            (None, true),
            (Some("a"), true),
            (None, false),
            (Some("a"), true),
            (None, true),
            (Some("unknown"), true),
            (None, false),
        ]);
        let g = groups(
            ShotGrouping::BySet,
            vec![group("a", 1, Club::Iron7, true, 2)],
        );
        assert_eq!(
            list_items(&shots, &g, Some(Club::Iron7)),
            vec![
                ListItem::Shot(6),
                ListItem::Shot(5),
                ListItem::Shot(4),
                header("a"),
                ListItem::Shot(3),
                ListItem::Shot(2),
                ListItem::Shot(1),
                ListItem::Ungrouped,
                ListItem::Shot(0),
            ]
        );

        // Under an empty open set's header, the pending row stays on top
        // and the run in no set gets its own header.
        let g = groups(
            ShotGrouping::BySet,
            vec![
                group("a", 1, Club::Iron7, false, 2),
                group("c", 2, Club::Iron7, true, 0),
            ],
        );
        let items = list_items(&shots, &g, Some(Club::Iron7));
        assert_eq!(
            items[..5],
            [
                ListItem::GroupHeader {
                    id: "c".into(),
                    next: true
                },
                ListItem::Shot(6),
                ListItem::Ungrouped,
                ListItem::Shot(5),
                ListItem::Shot(4),
            ]
        );
    }

    #[test]
    fn new_set_hint_follows_the_view() {
        assert!(new_group_hint(ShotGrouping::ByClub).contains("until New session"));
        assert_eq!(
            new_group_hint(ShotGrouping::BySet),
            "Close this set; the next shot starts a fresh one."
        );
    }

    #[test]
    fn foot_is_pinned_below_the_list_and_hidden_in_view_none() {
        let open7 = group("a", 1, Club::Iron7, true, 3);
        let strip = |mode, club| groups(mode, vec![open7.clone()]).strip(club);
        // None: no buttons, no hint, no foot.
        let none = strip(ShotGrouping::None, Some(Club::Iron7));
        assert!(!none.actions && none.hint.is_none());
        assert_eq!(list_foot_height(&none), 0.0);
        // By set and By club: the buttons, plus the hint line when there is one.
        let plain = strip(ShotGrouping::BySet, Some(Club::Iron7));
        let hinted = strip(ShotGrouping::BySet, Some(Club::Driver));
        assert!(plain.actions && plain.hint.is_none());
        assert!(hinted.hint.is_some());
        assert!(list_foot_height(&plain) >= STRIP_BUTTONS_HEIGHT);
        assert_eq!(
            list_foot_height(&hinted),
            list_foot_height(&plain) + STRIP_HINT_HEIGHT + FOOT_SPACING
        );
        let by_club = strip(ShotGrouping::ByClub, Some(Club::Iron7));
        assert_eq!(list_foot_height(&by_club), list_foot_height(&plain));
    }

    #[test]
    fn table_insets_its_first_column_past_the_stripe() {
        // One device: no Device column, so `#` is first and clears the stripe.
        assert_eq!(column_inset(LeadColumn::Number, false), ROW_INSET);
        // Two devices: Device is first; `#` sits in the grid as usual.
        assert_eq!(column_inset(LeadColumn::Device, true), ROW_INSET);
        assert_eq!(column_inset(LeadColumn::Number, true), 0.0);
    }

    #[test]
    fn smash_falls_back_to_speed_ratio() {
        let mph = Velocity::MilesPerHour;
        assert_eq!(
            smash(Some(1.45), Some(mph(150.0)), Some(mph(100.0))),
            Some(1.45)
        );
        assert_eq!(smash(None, Some(mph(150.0)), Some(mph(100.0))), Some(1.5));
        // Mixed units are compared like for like.
        let s = smash(
            None,
            Some(Velocity::MetersPerSecond(67.056)),
            Some(mph(100.0)),
        );
        assert!((s.unwrap() - 1.5).abs() < 1e-4);
        assert_eq!(smash(None, None, Some(mph(100.0))), None);
        assert_eq!(smash(None, Some(mph(150.0)), None), None);
        assert_eq!(smash(None, Some(mph(150.0)), Some(mph(0.0))), None);
    }

    /// Every card of `layout`, headline first.
    fn all_cards(layout: &CardsLayout) -> Vec<egui::Rect> {
        let details = layout.groups.iter().flat_map(|(_, cards)| cards.iter());
        layout.headline.iter().chain(details).copied().collect()
    }

    fn assert_fits(layout: &CardsLayout, width: f32) {
        let cards = all_cards(layout);
        for (i, a) in cards.iter().enumerate() {
            assert!(
                a.left() >= 0.0 && a.right() <= width + 0.01,
                "{a:?} overflows"
            );
            assert!(a.top() >= layout.header.bottom() && a.bottom() <= layout.height + 0.01);
            for b in &cards[i + 1..] {
                assert!(!a.shrink(0.5).intersects(*b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn cards_sit_side_by_side_when_wide() {
        let size = egui::vec2(1560.0, 760.0);
        let layout = cards_layout(size, 5, &[2, 4, 6], None);
        assert_fits(&layout, size.x);
        // One headline row, and all group labels on one line.
        assert!(
            layout
                .headline
                .iter()
                .all(|r| r.top() == layout.headline[0].top())
        );
        let label_y = layout.groups[0].0.top();
        assert!(layout.groups.iter().all(|(l, _)| l.top() == label_y));
        // Headline cards are taller, and the spare height is split.
        assert!(layout.headline[0].height() > layout.groups[0].1[0].height());
        assert!(layout.header.top() > 0.0 && layout.height < size.y);
    }

    #[test]
    fn cards_stack_and_wrap_when_narrow() {
        let size = egui::vec2(420.0, 700.0);
        let layout = cards_layout(size, 5, &[2, 4, 6], None);
        assert_fits(&layout, size.x);
        // Headline wraps into balanced rows (3 + 2).
        let first_row = layout.headline[0].top();
        let in_first = layout
            .headline
            .iter()
            .filter(|r| r.top() == first_row)
            .count();
        assert_eq!(in_first, 3);
        // Groups stack, each label below the previous group's cards.
        for pair in layout.groups.windows(2) {
            let prev_bottom = pair[0].1.iter().map(|r| r.bottom()).fold(0.0, f32::max);
            assert!(pair[1].0.top() > prev_bottom);
        }
        // Too short to fit: cards keep their compact height and scroll.
        let short = cards_layout(egui::vec2(420.0, 300.0), 5, &[2, 4, 6], None);
        assert_fits(&short, 420.0);
        assert!(short.height > 300.0 && short.scrolls);
        assert_eq!(short.headline[0].height(), COMPACT.headline);
        assert_eq!(short.groups[0].1[0].height(), COMPACT.detail);
    }

    #[test]
    fn cards_pair_groups_when_medium() {
        // Too narrow for six cards side by side, wide enough for five
        // headline cards in a row.
        let size = egui::vec2(660.0, 620.0);
        let layout = cards_layout(size, 5, &[2, 4, 6], None);
        assert_fits(&layout, size.x);
        assert!(layout.height <= size.y);
        // One headline row of five.
        assert!(
            layout
                .headline
                .iter()
                .all(|r| r.top() == layout.headline[0].top())
        );
        // Ball over Flight on the left, Club on the right, two cards wide.
        let [ball, flight, club] = &layout.groups[..] else {
            panic!("three groups");
        };
        assert_eq!(ball.0.left(), flight.0.left());
        assert!(flight.0.top() > ball.1[0].bottom());
        assert!(club.0.left() > flight.0.right());
        assert_eq!(club.0.top(), ball.0.top());
        assert_eq!(club.1[1].top(), club.1[0].top());
        assert!(club.1[2].top() > club.1[0].top());
        // Detail cards stay shorter than the headline cards.
        assert!(club.1[0].height() < layout.headline[0].height());
    }

    fn stats(values: &[f64]) -> Option<Stats> {
        Stats::of(values.iter().copied())
    }

    #[test]
    fn distance_domain_is_shared_and_padded() {
        // Total 230-250 and carry 210-225 share 210-250; 15 % of 40 is
        // under the 10 yd floor.
        let total = stats(&[230.0, 250.0]);
        let carry = stats(&[210.0, 225.0]);
        assert_eq!(distance_domain(&[total, carry], true), Some((200.0, 260.0)));
        // Metres have a 9 m floor.
        assert_eq!(
            distance_domain(&[total, carry], false),
            Some((201.0, 259.0))
        );
        // A wide spread pads by 15 % of its range.
        let wide = stats(&[100.0, 300.0]);
        assert_eq!(distance_domain(&[wide, None], true), Some((70.0, 330.0)));
        // One bar with data sets the domain alone; neither gives none.
        assert_eq!(distance_domain(&[None, carry], true), Some((200.0, 235.0)));
        assert_eq!(distance_domain(&[None, None], true), None);
        // A single value still gets the floor on both sides.
        let one = stats(&[150.0]);
        assert_eq!(distance_domain(&[one, None], true), Some((140.0, 160.0)));
    }

    #[test]
    fn lateral_domain_is_symmetric_with_a_floor() {
        let close = |(lo, hi): (f64, f64), want: f64| {
            assert!(
                (lo + want).abs() < 1e-9 && (hi - want).abs() < 1e-9,
                "{lo} {hi}"
            );
        };
        // The widest side sets both, with the margin.
        close(lateral_domain(stats(&[-3.0, 8.0]).as_ref()), 9.6);
        close(lateral_domain(stats(&[-12.0, 1.0]).as_ref()), 14.4);
        // A tight group, or no data, falls back to the floor.
        close(lateral_domain(stats(&[-1.0, 2.0]).as_ref()), 6.0);
        close(lateral_domain(None), 6.0);
        // The target line is the middle of the plot.
        let plot = egui::Rangef::new(100.0, 300.0);
        assert_eq!(bar_x(lateral_domain(None), plot, 0.0), 200.0);
    }

    #[test]
    fn one_value_draws_a_pip() {
        let plot = egui::Rangef::new(0.0, 200.0);
        let pip = band_x((0.0, 100.0), plot, 50.0, 50.0);
        assert_eq!((pip.min, pip.max), (92.0, 108.0));
        // A narrow range widens to the pip about its middle; a wide one is
        // drawn as is.
        let narrow = band_x((0.0, 100.0), plot, 50.0, 51.0);
        assert_eq!((narrow.min, narrow.max), (93.0, 109.0));
        let wide = band_x((0.0, 100.0), plot, 20.0, 60.0);
        assert!((wide.min - 40.0).abs() < 1e-3 && (wide.max - 120.0).abs() < 1e-3);
        // Values outside the domain are clamped to the plot.
        assert_eq!(bar_x((0.0, 100.0), plot, 150.0), 200.0);
    }

    #[test]
    fn coincident_markers_nest_with_expected_sizes() {
        // The average dot fits inside the median ring's hollow centre, so a
        // value shared by every marker reads as a ring filled by the dot,
        // with the this-shot line drawn through both.
        let ring_inner_radius = MEDIAN_RING_RADIUS - MEDIAN_RING_STROKE / 2.0;
        assert!(AVG_DOT_RADIUS < ring_inner_radius);
        // The ring sits just inside the track, not poking out of it; only
        // the this-shot line sticks out past the bar. Both hold by
        // construction, so the compiler checks them as a build-time
        // invariant rather than a runtime assertion on constants.
        const { assert!(MEDIAN_RING_RADIUS < TRACK_H / 2.0) };
        const { assert!(TRACK_H / 2.0 + MARKER_OVERHANG > TRACK_H / 2.0) };
    }

    /// A row with data whose total, carry and offline are measured (`m`)
    /// and simulated (`d`), in yards.
    fn flight(
        n: u32,
        m: (Option<f64>, Option<f64>),
        d: (Option<f64>, Option<f64>, Option<f64>),
    ) -> ShotRow {
        let yd = |v: Option<f64>| v.map(Distance::Yards);
        let ball = BallFlight {
            total_distance: yd(m.0),
            carry_distance: yd(m.1),
            ..Default::default()
        };
        let mut derived = crate::types::DerivedFlight::default();
        derived.total_distance = yd(d.0);
        derived.carry_distance = yd(d.1);
        derived.carry_offline = yd(d.2);
        ShotRow {
            ball: Some(ball),
            derived: Some(derived),
            group: Some("a".into()),
            ..row(n, true)
        }
    }

    #[test]
    fn bar_values_pick_each_shots_value_and_count_missing() {
        let rows = vec![
            // Measured only.
            flight(1, (Some(250.0), Some(230.0)), (None, None, Some(-4.0))),
            // Simulated only.
            flight(2, (None, None), (Some(245.0), Some(225.0), Some(6.0))),
            // No total or offline at all (e.g. the sim was gated).
            flight(3, (None, Some(228.0)), (None, None, None)),
            // A trigger with no data yet is not counted.
            row(4, false),
        ];
        let total = bar_values(&rows, BarKind::Total, true);
        assert_eq!(
            total,
            BarValues {
                values: vec![250.0, 245.0],
                rows: 3
            }
        );
        let carry = bar_values(&rows, BarKind::Carry, true);
        assert_eq!(carry.values, vec![230.0, 225.0, 228.0]);
        // Lateral is the simulated carry offline, always.
        let lateral = bar_values(&rows, BarKind::Lateral, true);
        assert_eq!(lateral.values, vec![-4.0, 6.0]);
        // Metric converts.
        let m = bar_values(&rows[..1], BarKind::Total, false);
        assert!((m.values[0] - 228.6).abs() < 1e-9);
    }

    #[test]
    fn bars_mark_the_shot_and_report_spread() {
        let mut rows: Vec<ShotRow> = [240.0, 250.0, 245.0]
            .iter()
            .zip(1..)
            .map(|(&t, n)| flight(n, (Some(t), None), (None, Some(t - 20.0), Some(1.5))))
            .collect();
        // A shot of another group, and one in this group without a total.
        rows.push(ShotRow {
            group: Some("b".into()),
            ..flight(4, (Some(300.0), None), (None, None, None))
        });
        rows.push(flight(5, (None, None), (None, Some(222.0), None)));
        // The members of set "a".
        let a: Vec<usize> = vec![0, 1, 2, 4];
        let [total, carry, lateral] = bars_for(&rows, &a, Some(1), &[], true);
        assert_eq!(total.values.values, vec![240.0, 250.0, 245.0]);
        assert_eq!(total.this, Some(250.0));
        assert_eq!(
            bar_stats_text(&total),
            (
                "avg 245 \u{b7} med 245".to_owned(),
                "sd 5.0 \u{b7} 3/4".to_owned()
            )
        );
        // Total and Carry share a domain; lateral has its own.
        assert_eq!(total.domain, carry.domain);
        assert_eq!(total.domain, Some((210.0, 260.0)));
        assert_eq!(
            bar_stats_text(&lateral),
            (
                "avg +1.5 \u{b7} med +1.5".to_owned(),
                "sd 0.0 \u{b7} 3/4".to_owned()
            )
        );
        // A shown shot from another group gets no dot.
        let [total, ..] = bars_for(&rows, &a, Some(3), &[], true);
        assert_eq!(total.this, None);

        // One shot: no sd; no group or no values: no data and no domain.
        let one = bars_for(&rows[..1], &[0], Some(0), &[], true);
        assert_eq!(
            bar_stats_text(&one[0]),
            ("avg 240 \u{b7} med 240".to_owned(), String::new())
        );
        assert_eq!(one[0].this, Some(240.0));
        let none = bars_for(&rows, &[], Some(0), &[], true);
        assert!(none.iter().all(|b| b.stats.is_none() && b.domain.is_none()));
        assert_eq!(
            bar_stats_text(&none[0]),
            ("no data".to_owned(), String::new())
        );
    }

    #[test]
    fn bars_show_the_shown_shots_set() {
        let list = vec![
            group("a", 1, Club::Iron7, false, 3),
            group("b", 2, Club::Driver, false, 2),
            group("c", 3, Club::Iron7, true, 1),
        ];
        let in_a = ShotRow {
            group: Some("a".into()),
            ..row(1, true)
        };
        let loose = row(2, true);
        let stale = ShotRow {
            group: Some("gone".into()),
            ..row(3, true)
        };
        let id = |g: Option<&ShotGroupInfo>| g.map(|g| g.id.clone());
        // The same in every view: the shot's own set, even a closed one;
        // else the set the next shot joins.
        for mode in [
            ShotGrouping::BySet,
            ShotGrouping::ByClub,
            ShotGrouping::None,
        ] {
            let g = groups(mode, list.clone());
            assert_eq!(
                id(bar_group(&g, Some(&in_a), Some(Club::Driver))),
                Some("a".into())
            );
            assert_eq!(
                id(bar_group(&g, Some(&loose), Some(Club::Iron7))),
                Some("c".into())
            );
            // A different club starts a new set: nothing to show yet.
            assert_eq!(id(bar_group(&g, Some(&loose), Some(Club::Driver))), None);
            assert_eq!(id(bar_group(&g, Some(&stale), None)), None);
            assert_eq!(id(bar_group(&g, None, Some(Club::Iron7))), Some("c".into()));
        }
    }

    #[test]
    fn bars_sit_between_headline_and_details() {
        let size = egui::vec2(1560.0, 760.0);
        let plain = cards_layout(size, 5, &[2, 4, 6], None);
        assert!(plain.bars.is_none());
        let layout = cards_layout(size, 5, &[2, 4, 6], Some(Legend::Beside));
        assert_fits(&layout, size.x);
        assert!(layout.height <= size.y + 1e-3, "{}", layout.height);
        let BarsLayout {
            controls,
            label,
            legend,
            rows,
        } = layout.bars.expect("bars");
        assert_eq!(legend, label, "the legend sits on the label line");
        let headline_bottom = layout.headline[0].bottom();
        assert_eq!(controls.top(), headline_bottom + ROOMY.controls_gap);
        assert_eq!(label.top(), controls.bottom());
        assert_eq!(rows[0].top(), label.bottom());
        assert_eq!(
            rows[2].bottom() - controls.top(),
            ROOMY.bars(Legend::Beside)
        );
        assert_eq!(
            layout.groups[0].0.top(),
            controls.top() + ROOMY.bars(Legend::Beside) + ROOMY.group_gap
        );
        assert!(rows.iter().all(|r| r.width() == size.x));
        assert_eq!(controls.width(), size.x);
        // The cards give up height to make room.
        assert!(layout.headline[0].height() < plain.headline[0].height());
    }

    #[test]
    fn bars_legend_wraps_below_a_title_it_would_overlap() {
        use egui::{Pos2, Rect, vec2};

        let chips = vec![
            ("Open Connect".to_owned(), theme::ACCENT),
            ("Omni Bridge".to_owned(), theme::HEADING),
        ];
        let title = ("Set 2 \u{b7} 7I \u{b7} 10 shots".to_owned(), theme::ACCENT);
        let bars = bars_for(&[], &[], None, &[], true);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        // Cards widths: 1600 with both panels, then about 455 px, then
        // narrower still; each with room to spare and at compact sizes.
        for (width, expect, height) in [
            (1000.0, Legend::Beside, 760.0),
            (455.0, Legend::Below, 760.0),
            (300.0, Legend::Below, 760.0),
            (1000.0, Legend::Beside, 300.0),
            (455.0, Legend::Below, 300.0),
        ] {
            let mut place = None;
            let mut layout = None;
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new())
                        .show(ctx, |ui| {
                            let legend = legend_place(ui.painter(), width, Some(&title.0), &chips);
                            let at = cards_layout(vec2(width, height), 5, &[2, 4, 6], Some(legend));
                            let bars_at = at.bars.expect("bars");
                            range_bars(ui, &bars_at, Some(&title), &bars, &chips, "");
                            place = Some(legend);
                            layout = Some(at);
                        });
                },
            );
            assert_eq!(place, Some(expect), "at {width}");
            let layout = layout.expect("laid out");
            let at = layout.bars.expect("bars");
            assert_fits(&layout, width);
            // Every painted text, by position.
            let mut texts: Vec<(String, Rect)> = Vec::new();
            for clipped in &out.shapes {
                if let egui::Shape::Text(t) = &clipped.shape {
                    texts.push((t.galley.text().to_owned(), t.visual_bounding_rect()));
                }
            }
            let title_rect = texts
                .iter()
                .find(|(t, _)| t.starts_with("SET 2"))
                .map(|(_, r)| *r)
                .expect("title painted");
            let legend: Vec<Rect> = texts
                .iter()
                .filter(|(t, _)| {
                    [
                        "this shot",
                        "median",
                        "avg",
                        "range",
                        "Open Connect",
                        "Omni Bridge",
                    ]
                    .contains(&t.as_str())
                })
                .map(|(_, r)| *r)
                .collect();
            assert_eq!(legend.len(), 6, "at {width}");
            for r in &legend {
                assert!(
                    !r.intersects(title_rect),
                    "at {width}: {r:?} over {title_rect:?}"
                );
                assert!(r.bottom() <= at.rows[0].top(), "above the bars at {width}");
            }
            match expect {
                Legend::Beside => {
                    assert_eq!(at.legend, at.label);
                    assert!(legend.iter().all(|r| r.left() > title_rect.right()));
                }
                Legend::Below => {
                    assert!(legend.iter().all(|r| r.top() >= title_rect.bottom()));
                    assert!(at.legend.top() > at.label.top());
                    assert_eq!(at.rows[0].top(), at.legend.bottom());
                }
            }
        }
        // Without a title (a note instead) the legend is not drawn.
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            assert_eq!(legend_place(&painter, 100.0, None, &chips), Legend::Beside);
        });
    }

    /// The windows the Cards view must fit without scrolling.
    const FIT_WINDOWS: [(f32, f32); 5] = [
        (1920.0, 1080.0),
        (1600.0, 900.0),
        (1440.0, 900.0),
        (1280.0, 800.0),
        (1024.0, 768.0),
    ];

    /// Space the app takes around the Shots tab: its header bar (53 px and a
    /// 1 px separator) and the 14 px content margin.
    const TAB_MARGIN: egui::Margin = egui::Margin {
        left: 14,
        right: 14,
        top: 54 + 14,
        bottom: 14,
    };

    /// Filled bar-thin rects within 12 px left of `right`: a solid scroll
    /// bar's track and handle.
    fn scroll_bars(out: &egui::FullOutput, right: f32) -> usize {
        out.shapes
            .iter()
            .filter(|c| match &c.shape {
                egui::Shape::Rect(r) => {
                    r.rect.width() <= 10.0
                        && r.rect.height() > 40.0
                        && r.rect.left() >= right - 12.0
                        && r.rect.right() <= right + 0.5
                        && r.fill.a() > 0
                }
                _ => false,
            })
            .count()
    }

    /// One frame of the Shots tab in a `window`-sized app.
    fn shots_frame(
        ctx: &egui::Context,
        app: &mut FlighthookApp,
        window: egui::Vec2,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, window)),
            ..Default::default()
        };
        ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(TAB_MARGIN))
                .show(ctx, |ui| app.render_shots_panel(ui));
        })
    }

    /// An app on the Cards view with a set of 12 shots from two devices.
    fn cards_app() -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.shots = thousand_rows()
            .into_iter()
            .take(12)
            .map(|r| ShotRow {
                group: Some("a".into()),
                ..r
            })
            .collect();
        app.groups = groups(
            ShotGrouping::BySet,
            vec![group("a", 1, Club::Iron7, true, 12)],
        );
        app.shots_rev = 1;
        app.shots_view = ShotsView::Cards;
        app
    }

    /// The Cards area (inside the glow inset and the foot) at each of
    /// [`FIT_WINDOWS`], then at 900×700, with the shot list at its default
    /// width; and the narrower width each takes with the list at its
    /// widest default (opened at 1920, then resized).
    const FIT_AREAS: [(f32, f32, f32); 6] = [
        (1212.0, 606.0, 1212.0),
        (962.0, 476.0, 928.0),
        (842.0, 476.0, 806.0),
        (721.0, 406.0, 685.0),
        (772.0, 404.0, 736.0),
        (640.0, 336.0, 604.0),
    ];

    #[test]
    fn cards_fit_every_listed_window_without_scrolling() {
        for (i, &(w, h, narrow)) in FIT_AREAS[..FIT_WINDOWS.len()].iter().enumerate() {
            for width in [w, narrow] {
                for legend in [None, Some(Legend::Beside), Some(Legend::Below)] {
                    let layout = cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], legend);
                    let at = format!("{:?} {width}x{h} {legend:?}", FIT_WINDOWS[i]);
                    assert!(!layout.scrolls, "{at}");
                    assert!(layout.height <= h + 0.01, "{at}: {}", layout.height);
                    assert_fits(&layout, width);
                    // One headline row, and the details in two rows.
                    let head = layout.headline[0].top();
                    assert!(layout.headline.iter().all(|r| r.top() == head), "{at}");
                    let rows: std::collections::BTreeSet<i32> = layout
                        .groups
                        .iter()
                        .flat_map(|(_, c)| c.iter().map(|r| r.top().round() as i32))
                        .collect();
                    assert_eq!(rows.len(), 2, "{at}");
                    // Never under the compact sizes.
                    assert!(layout.headline[0].height() >= COMPACT.headline, "{at}");
                    assert!(layout.groups[0].1[0].height() >= COMPACT.detail, "{at}");
                }
            }
        }
    }

    #[test]
    fn numerals_stay_readable_at_the_smallest_cards() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        // Wide numerals and units in every card.
        let mut shot = thousand_rows().remove(0);
        shot.club = Some(ClubData {
            club_speed: Some(Velocity::MilesPerHour(108.8)),
            path: Some(-10.8),
            attack_angle: Some(-10.8),
            face_angle: Some(-10.8),
            dynamic_loft: Some(48.8),
            ..Default::default()
        });
        let (headline, groups) = card_metrics(&shot, true);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            for &(w, h, narrow) in &FIT_AREAS {
                for width in [w, narrow] {
                    let layout =
                        cards_layout(egui::vec2(width, h), 5, &[2, 4, 6], Some(Legend::Below));
                    let head = layout.headline[0];
                    let look =
                        CardLook::headline(head.size()).fit(&painter, &headline, head.width());
                    assert!(
                        look.value >= HEADLINE_VALUE_MIN,
                        "{width}x{h}: {}",
                        look.value
                    );
                    let detail = layout.groups[0].1[0];
                    let all = groups.iter().flat_map(|(_, m)| m);
                    let look = CardLook::detail(detail.size()).fit(&painter, all, detail.width());
                    assert!(
                        look.value >= DETAIL_VALUE_MIN,
                        "{width}x{h}: {}",
                        look.value
                    );
                }
            }
        });
        // The floors hold before fitting, however short the card.
        assert_eq!(
            CardLook::headline(egui::vec2(200.0, 40.0)).value,
            HEADLINE_VALUE_MIN
        );
        assert_eq!(
            CardLook::detail(egui::vec2(140.0, 30.0)).value,
            DETAIL_VALUE_MIN
        );
    }

    #[test]
    fn short_of_room_every_size_shrinks_together() {
        assert_eq!(Rhythm::between(0.0), COMPACT);
        assert_eq!(Rhythm::between(1.0), ROOMY);
        assert_eq!(Rhythm::between(-1.0), COMPACT);
        assert_eq!(Rhythm::between(2.0), ROOMY);
        // Every compact size is the smaller one.
        let field = |r: Rhythm| {
            [
                r.header,
                r.header_gap,
                r.headline,
                r.controls_gap,
                r.controls,
                r.label,
                r.legend_overlap,
                r.bar_row,
                r.bar_gap,
                r.group_gap,
                r.row_gap,
                r.detail,
            ]
        };
        for (c, r) in field(COMPACT).into_iter().zip(field(ROOMY)) {
            assert!(c < r, "{c} < {r}");
        }
        // Halfway: the bar rows, the label lines and the cards all shrank.
        let (w, h, _) = FIT_AREAS[1];
        let roomy = cards_layout(egui::vec2(w, 900.0), 5, &[2, 4, 6], Some(Legend::Beside));
        let fit = cards_layout(egui::vec2(w, h), 5, &[2, 4, 6], Some(Legend::Beside));
        let (a, b) = (roomy.bars.expect("bars"), fit.bars.expect("bars"));
        assert!(b.rows[0].height() < a.rows[0].height());
        assert!(b.rows[0].height() > COMPACT.bar_row);
        assert!(b.label.height() < a.label.height());
        assert!(fit.headline[0].height() < roomy.headline[0].height());
        assert!(fit.groups[0].1[0].height() < roomy.groups[0].1[0].height());
        // It fills the height: no spare room above the header.
        assert_eq!(fit.header.top(), 0.0);
        assert!((fit.height - h).abs() < 0.5, "{}", fit.height);
    }

    #[test]
    fn cards_resize_smoothly_with_the_height() {
        // A pixel of height never moves anything by much more than a pixel,
        // and cards never get shorter as the view gets taller.
        for width in [1212.0, 772.0, 640.0, 440.0] {
            let mut last: Option<CardsLayout> = None;
            for h in 200..=1000 {
                let layout = cards_layout(
                    egui::vec2(width, h as f32),
                    5,
                    &[2, 4, 6],
                    Some(Legend::Beside),
                );
                if let Some(prev) = &last {
                    let moved = |a: egui::Rect, b: egui::Rect| {
                        (a.top() - b.top())
                            .abs()
                            .max((a.bottom() - b.bottom()).abs())
                    };
                    for (a, b) in all_cards(prev).into_iter().zip(all_cards(&layout)) {
                        assert!(moved(a, b) <= 2.0, "{width}x{h}: {a:?} -> {b:?}");
                        assert!(b.height() >= a.height() - 1e-3, "{width}x{h}");
                    }
                }
                last = Some(layout);
            }
        }
    }

    #[test]
    fn scroll_engages_only_below_the_compact_height() {
        for (w, _, _) in FIT_AREAS {
            for legend in [Some(Legend::Beside), Some(Legend::Below)] {
                // The compact height: what the cards need at their smallest.
                let least = cards_layout(egui::vec2(w, 1.0), 5, &[2, 4, 6], legend).height;
                let at = |h: f32| cards_layout(egui::vec2(w, h), 5, &[2, 4, 6], legend);
                assert!(!at(least).scrolls, "{w}: {least}");
                assert!(at(least).height <= least + 0.01);
                let short = at(least - 2.0);
                assert!(short.scrolls, "{w}: {least}");
                // Scrolling, every size is compact.
                assert_eq!(short.headline[0].height(), COMPACT.headline);
                assert_eq!(short.groups[0].1[0].height(), COMPACT.detail);
                assert_eq!(short.bars.expect("bars").rows[0].height(), COMPACT.bar_row);
            }
        }
        // 900×700 is below it, the listed windows are not.
        let (w, h, _) = FIT_AREAS[5];
        assert!(cards_layout(egui::vec2(w, h), 5, &[2, 4, 6], Some(Legend::Beside)).scrolls);
    }

    #[test]
    fn a_short_window_scrolls_with_a_bar_and_room_under_the_last_row() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = cards_app();
        let window = egui::vec2(900.0, 700.0);
        for _ in 0..3 {
            shots_frame(&ctx, &mut app, window);
        }
        let out = shots_frame(&ctx, &mut app, window);
        let right = window.x - f32::from(TAB_MARGIN.right);
        assert_eq!(scroll_bars(&out, right), 2, "the bar's track and handle");
        // Scrolled to the end, the last row stands clear of the panel.
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, window)),
            events: vec![
                egui::Event::PointerMoved(egui::pos2(500.0, 300.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -2000.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(TAB_MARGIN))
                .show(ctx, |ui| app.render_shots_panel(ui));
        });
        let mut out = shots_frame(&ctx, &mut app, window);
        for _ in 0..30 {
            out = shots_frame(&ctx, &mut app, window);
        }
        let (row, _) = chart_slots(ShotsView::Cards, window)
            .bottom
            .expect("a bottom row");
        let panel_top = window.y - f32::from(TAB_MARGIN.bottom) - row;
        let lowest = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r)
                    if r.stroke.color == theme::BORDER
                        && r.rect.height() >= 40.0
                        && r.rect.left() > LIST_MIN_WIDTH =>
                {
                    Some(r.rect.bottom())
                }
                _ => None,
            })
            .fold(0.0, f32::max);
        assert!(lowest > panel_top - 200.0, "not scrolled: {lowest}");
        assert!(
            lowest + CHART_GAP <= panel_top + 0.5,
            "{lowest} reaches the panel at {panel_top}"
        );
    }

    #[test]
    fn cards_fit_above_flight_at_common_window_sizes() {
        let labels = [
            "BALL SPEED",
            "CARRY",
            "TOTAL",
            "LAUNCH",
            "BACKSPIN",
            "DIRECTION",
            "SIDESPIN",
            "APEX",
            "OFFLINE",
            "DESCENT",
            "ROLL",
            "CLUB SPEED",
            "SMASH",
            "PATH",
            "AOA",
            "FACE",
            "LOFT",
        ];
        // Each size freshly opened, then the same sizes in turn in one
        // window, so the shot list keeps the width it took at 1920.
        let shared = egui::Context::default();
        theme::install(&shared);
        let runs = FIT_WINDOWS
            .iter()
            .map(|&s| (s, None))
            .chain(FIT_WINDOWS.iter().map(|&s| (s, Some(shared.clone()))));
        for ((w, h), ctx) in runs {
            let ctx = ctx.unwrap_or_else(|| {
                let ctx = egui::Context::default();
                theme::install(&ctx);
                ctx
            });
            let window = egui::vec2(w, h);
            let mut app = cards_app();
            for _ in 0..3 {
                shots_frame(&ctx, &mut app, window);
            }
            let out = shots_frame(&ctx, &mut app, window);
            let slots = chart_slots(ShotsView::Cards, window);
            let (row, _) = slots.bottom.expect("a bottom row");
            // The top of the panel holding Flight.
            let panel_top = h - f32::from(TAB_MARGIN.bottom) - row;
            let right = w - f32::from(TAB_MARGIN.right) - slots.right.unwrap_or(0.0);
            // Every card: its outline, unclipped, above the panel.
            let cards: Vec<egui::Rect> = out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Rect(r)
                        if r.stroke.color == theme::BORDER
                            && r.rect.height() >= 40.0
                            && r.rect.left() > LIST_MIN_WIDTH
                            && r.rect.right() <= right =>
                    {
                        assert!(
                            c.clip_rect.contains_rect(r.rect),
                            "{w}x{h}: {:?} clipped to {:?}",
                            r.rect,
                            c.clip_rect
                        );
                        Some(r.rect)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(cards.len(), labels.len(), "{w}x{h}: {cards:?}");
            for r in &cards {
                assert!(
                    r.bottom() + CHART_GAP <= panel_top + 0.5,
                    "{w}x{h}: {r:?} reaches the panel at {panel_top}"
                );
            }
            assert_eq!(scroll_bars(&out, right), 0, "{w}x{h}: a scroll bar");
            let texts = painted_texts(&out.shapes);
            for label in labels {
                assert!(
                    texts.iter().any(|(t, _)| t == label),
                    "{w}x{h}: {label} not painted"
                );
            }
        }
    }

    #[test]
    fn the_bottom_row_budgets_the_dispersion_header() {
        // The floors hold a plot of the least height under a one-row header.
        assert_eq!(*SHARED_ROW_HEIGHT.start(), 210.0);
        assert_eq!(*TABLE_DISPERSION_HEIGHT.start(), 266.0);
        // Cards: Dispersion is widened so its header keeps one row at
        // 1024 px, with the list at its default or widest width; Flight
        // keeps the rest.
        for width in [748.0, 785.0] {
            let (flight, slot) = row_split(width);
            assert_eq!(slot, dispersion::ONE_ROW_MIN_WIDTH, "{width}");
            assert_eq!(flight + CHART_GAP + slot, width);
            assert!(flight >= 400.0, "{width}: {flight}");
        }
        // Wide rows keep the 38 % share; narrow ones give at most half.
        assert_eq!(row_split(1014.0).1, 380.0);
        assert_eq!(row_split(414.0), (200.0, 200.0));
        // Every narrow window, any row width: the plot keeps its minimum
        // under however many header rows the slot draws.
        for w in (600..1200).step_by(25) {
            for h in (400..1200).step_by(20) {
                let window = egui::vec2(w as f32, h as f32);
                for view in [ShotsView::Cards, ShotsView::Table] {
                    let (height, row) = chart_slots(view, window).bottom.expect("a bottom row");
                    for width in (300..1200).step_by(7) {
                        let width = width as f32;
                        for filter in [false, true] {
                            let filter = filter && view == ShotsView::Table;
                            let (slot, least) = match row {
                                BottomRow::FlightAndDispersion => {
                                    (row_split(width).1, SHARED_PLOT_MIN)
                                }
                                _ => (width, TABLE_PLOT_MIN),
                            };
                            let row_height = bottom_height(height, row, width, filter);
                            assert!(row_height >= height);
                            let plot = row_height
                                - CHART_GAP
                                - dispersion::header_height(slot, filter)
                                - dispersion::PLOT_PAD;
                            let at = format!("{view:?} {w}x{h} row {width} filter {filter}");
                            assert!(plot >= least, "{at}: {plot}");
                            // Only the extra header rows are added.
                            if row_height > height {
                                assert_eq!(plot, least, "{at}");
                            }
                        }
                    }
                }
            }
        }
        // The fit windows keep their Cards bottom row.
        for &(w, h) in &FIT_WINDOWS {
            let window = egui::vec2(w, h);
            let (height, row) = chart_slots(ShotsView::Cards, window).bottom.unwrap();
            if row == BottomRow::FlightAndDispersion {
                for width in [748.0, 785.0] {
                    assert_eq!(bottom_height(height, row, width, false), height);
                }
            }
        }
    }

    #[test]
    fn the_dispersion_plot_keeps_its_least_height_in_narrow_windows() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let one_device = |app: &mut FlighthookApp| {
            for shot in &mut app.shots {
                shot.actor = "mevo.0".into();
            }
        };
        // (view, window, two devices)
        let cases = [
            (ShotsView::Cards, (1024.0, 768.0), true),
            (ShotsView::Cards, (900.0, 700.0), true),
            (ShotsView::Cards, (760.0, 600.0), true),
            (ShotsView::Table, (1024.0, 768.0), false),
            (ShotsView::Table, (1024.0, 768.0), true),
            (ShotsView::Table, (900.0, 600.0), false),
            (ShotsView::Table, (900.0, 600.0), true),
            (ShotsView::Table, (640.0, 480.0), true),
        ];
        for (view, (w, h), two) in cases {
            let at = format!("{view:?} {w}x{h} two devices {two}");
            let ctx = ctx.clone();
            let mut app = cards_app();
            app.shots_view = view;
            if !two {
                one_device(&mut app);
            }
            let window = egui::vec2(w, h);
            for _ in 0..3 {
                shots_frame(&ctx, &mut app, window);
            }
            let out = shots_frame(&ctx, &mut app, window);
            let (plot, area) = app.dispersion_plot().expect("a Dispersion plot");
            let least = match view {
                ShotsView::Cards => SHARED_PLOT_MIN,
                ShotsView::Table => TABLE_PLOT_MIN,
            };
            assert!(plot.height() >= least - 0.5, "{at}: {plot:?}");
            // The header sits above the plot's allocation, every switch in
            // it, and the device filter only in Table with two devices.
            let top = h - f32::from(TAB_MARGIN.bottom) - area;
            let texts = painted_text_rects(&out.shapes);
            let title = texts
                .iter()
                .find(|(t, _, _)| t == "DISPERSION")
                .map(|(_, _, r)| *r)
                .expect("the title");
            let header: Vec<_> = texts
                .iter()
                .filter(|(_, _, r)| r.left() >= title.left() - 0.5 && r.top() >= title.top() - 20.0)
                .filter(|(t, _, r)| {
                    [
                        "DISPERSION",
                        "2\u{3c3}",
                        "95 %",
                        "Total",
                        "Carry",
                        "DEVICE",
                        "All",
                    ]
                    .contains(&t.as_str())
                        && r.top() < top
                })
                .collect();
            for (t, _, r) in &header {
                assert!(r.bottom() <= top + 0.5, "{at}: {t} at {r:?} under {top}");
            }
            let shown = |label: &str| header.iter().any(|(t, _, _)| t == label);
            for label in ["DISPERSION", "2\u{3c3}", "95 %", "Total", "Carry"] {
                assert!(shown(label), "{at}: {label}");
            }
            let filter = view == ShotsView::Table && two;
            assert_eq!(shown("DEVICE"), filter, "{at}");
        }
    }

    #[test]
    fn pick_prefers_derived_when_both_are_present() {
        // Measured only, derived only: the one there is.
        assert_eq!(pick(Some(1.0), None), Some((1.0, false)));
        assert_eq!(pick(None, Some(2.0)), Some((2.0, true)));
        assert_eq!(pick::<f64>(None, None), None);
        // Both: derived wins, shown as simulated.
        assert_eq!(pick(Some(1.0), Some(2.0)), Some((2.0, true)));
        // A simulated 0 (a ground ball) over a device's carry.
        assert_eq!(pick(Some(12.0), Some(0.0)), Some((0.0, true)));
    }

    #[test]
    fn metric_shows_the_picked_value_and_its_source() {
        let m = Metric::new("Carry", "yd", pick(Some(200.04), None), 1, false);
        assert_eq!(
            (m.text.as_str(), m.source == Source::Measured),
            ("200.0", true)
        );
        let m = Metric::new("Carry", "yd", pick(Some(200.04), Some(190.0)), 1, false);
        assert_eq!(
            (m.text.as_str(), m.source == Source::Simulated),
            ("190.0", true)
        );
        let m = Metric::new("Offline", "yd", pick(None, Some(3.0)), 1, true);
        assert_eq!(
            (m.text.as_str(), m.source == Source::Simulated),
            ("+3.0", true)
        );
        let m = Metric::new("Descent", "\u{b0}", None, 1, false);
        assert!(m.source == Source::Missing);
    }

    /// A row whose device measured carry 171 and apex 30, simulated as
    /// carry 165, total 180 and no apex, with `replaces` as the shot's flag.
    fn forced(replaces: bool) -> ShotRow {
        let mut shot = flight(
            1,
            (None, Some(171.0)),
            (Some(180.0), Some(165.0), Some(2.0)),
        );
        shot.ball.as_mut().unwrap().max_height = Some(Distance::Feet(30.0));
        shot.derived.as_mut().unwrap().replaces_measured = replaces;
        shot
    }

    /// The text of the Cards metric `label` and whether it is simulated.
    fn card_text(shot: &ShotRow, label: &str) -> (String, bool) {
        let (headline, groups) = card_metrics(shot, true);
        let m = headline
            .into_iter()
            .chain(groups.into_iter().flat_map(|(_, g)| g))
            .find(|m| m.label == label)
            .unwrap();
        (m.text, m.source == Source::Simulated)
    }

    #[test]
    fn a_field_in_both_shows_the_simulated_value_everywhere() {
        // Force or not, a field the server set in both shows the simulated
        // value; a field measured only shows the measured one.
        let shots = [forced(false), forced(true)];
        for shot in &shots {
            assert_eq!(card_text(shot, "Carry"), ("165.0".into(), true));
            assert_eq!(card_text(shot, "Total"), ("180.0".into(), true));
            assert_eq!(card_text(shot, "Apex"), ("30.0".into(), false));
            let entry = ListEntry::new(shot, None, true);
            assert_eq!(entry.carry.text, "165.0");
            assert!(entry.carry.source == Source::Simulated);
            assert_eq!(BarKind::Carry.value(shot, true), Some(165.0));
            assert_eq!(BarKind::Total.value(shot, true), Some(180.0));
        }

        // The bars' stats run over the picked values.
        let carry = bar_values(&shots, BarKind::Carry, true);
        assert_eq!(carry.values, vec![165.0, 165.0]);
    }

    #[test]
    fn a_ground_ball_shows_zero_carry_over_the_devices_carry() {
        // The device sent carry 12 yd, apex 1 ft and 0.4 s on a bladed
        // shot; the server rolled it: carry, apex and flight time 0.
        let mut shot = flight(1, (None, Some(12.0)), (Some(9.0), Some(0.0), Some(0.0)));
        let ball = shot.ball.as_mut().unwrap();
        ball.max_height = Some(Distance::Feet(1.0));
        ball.flight_time = Some(0.4);
        let d = shot.derived.as_mut().unwrap();
        d.max_height = Some(Distance::Feet(0.0));
        d.flight_time = Some(0.0);
        assert_eq!(card_text(&shot, "Carry"), ("0.0".into(), true));
        assert_eq!(card_text(&shot, "Apex"), ("0.0".into(), true));
        assert_eq!(card_text(&shot, "Total"), ("9.0".into(), true));
        assert_eq!(ListEntry::new(&shot, None, true).carry.text, "0.0");
        assert_eq!(BarKind::Carry.value(&shot, true), Some(0.0));
        let r = crate::panels::flight::readings(&shot, UnitSystem::Imperial);
        assert_eq!(
            (r.carry, r.apex, r.total),
            (Some(0.0), Some(0.0), Some(9.0))
        );
    }

    #[test]
    fn simulated_legend_only_in_table() {
        assert!(ShotsView::Table.shows_simulated_legend());
        assert!(!ShotsView::Cards.shows_simulated_legend());
    }

    #[test]
    fn chart_slots_follow_the_window() {
        use egui::vec2;
        let slots = |view, w, h| chart_slots(view, vec2(w, h));
        let cards = ShotsView::Cards;
        let table = ShotsView::Table;
        // Wide Cards: Dispersion on the right (24 % of the width, 260-420),
        // Flight along the bottom (30 % of the height, 180-320).
        assert_eq!(
            slots(cards, 1920.0, 1080.0),
            ChartSlots {
                right: Some(420.0),
                bottom: Some((320.0, BottomRow::Flight)),
            }
        );
        assert_eq!(
            slots(cards, 1600.0, 900.0),
            ChartSlots {
                right: Some(384.0),
                bottom: Some((270.0, BottomRow::Flight)),
            }
        );
        assert_eq!(
            slots(cards, 1280.0, 800.0),
            ChartSlots {
                right: Some(307.0),
                bottom: Some((240.0, BottomRow::Flight)),
            }
        );
        assert_eq!(
            slots(cards, 1200.0, 500.0).bottom,
            Some((180.0, BottomRow::Flight))
        );
        assert_eq!(slots(cards, 1200.0, 500.0).right, Some(288.0));
        // Narrow Cards: both charts share the bottom row (27 % of the
        // height, 210-320), which stops shrinking below about 780 px tall;
        // Flight keeps at least 180 px under the row's top gap.
        assert_eq!(
            slots(cards, 1199.0, 900.0),
            ChartSlots {
                right: None,
                bottom: Some((243.0, BottomRow::FlightAndDispersion)),
            }
        );
        assert_eq!(
            slots(cards, 1024.0, 768.0).bottom,
            Some((210.0, BottomRow::FlightAndDispersion))
        );
        assert_eq!(
            slots(cards, 900.0, 700.0).bottom,
            Some((210.0, BottomRow::FlightAndDispersion))
        );
        assert!(*SHARED_ROW_HEIGHT.start() - CHART_GAP >= 180.0);
        // Table: Dispersion on the right when wide, below when narrow.
        assert_eq!(
            slots(table, 1600.0, 900.0),
            ChartSlots {
                right: Some(384.0),
                bottom: None,
            }
        );
        assert_eq!(
            slots(table, 1024.0, 768.0),
            ChartSlots {
                right: None,
                bottom: Some((269.0, BottomRow::Dispersion)),
            }
        );
        assert_eq!(
            slots(table, 900.0, 600.0).bottom,
            Some((266.0, BottomRow::Dispersion))
        );
        // Every chart is always somewhere.
        for &view in ShotsView::ALL {
            for (w, h) in [
                (1920.0, 1080.0),
                (1440.0, 900.0),
                (1024.0, 768.0),
                (900.0, 700.0),
            ] {
                let s = slots(view, w, h);
                let charts = usize::from(s.right.is_some())
                    + s.bottom.map_or(0, |(_, row)| match row {
                        BottomRow::FlightAndDispersion => 2,
                        BottomRow::Flight | BottomRow::Dispersion => 1,
                    });
                let expected = if view == cards { 2 } else { 1 };
                assert_eq!(charts, expected, "{view:?} {w}x{h}");
            }
        }
    }

    #[test]
    fn apex_card_is_labelled_feet_in_imperial_and_metres_in_metric() {
        let mut shot = row(1, true);
        shot.ball.as_mut().unwrap().max_height = Some(Distance::Meters(31.2));

        let imperial = shot.to_unit_system(UnitSystem::Imperial);
        let (_, groups) = card_metrics(&imperial, true);
        let apex = groups[1].1.iter().find(|m| m.label == "Apex").unwrap();
        assert_eq!(apex.unit, "ft");

        let metric = shot.to_unit_system(UnitSystem::Metric);
        let (_, groups) = card_metrics(&metric, false);
        let apex = groups[1].1.iter().find(|m| m.label == "Apex").unwrap();
        assert_eq!(apex.unit, "m");
    }

    /// A device's measured carry offline, descent angle and total offline
    /// fill the existing Offline and Descent cards, Table columns and the
    /// Lateral bar, as measured values; a simulated one still wins when both
    /// are there.
    #[test]
    fn measured_offline_and_descent_feed_cards_table_and_bars() {
        let mut shot = row(1, true);
        let ball = shot.ball.as_mut().unwrap();
        ball.carry_offline = Some(Distance::Yards(-4.2));
        ball.total_offline = Some(Distance::Yards(-5.0));
        ball.descent_angle = Some(43.1);

        let (_, groups) = card_metrics(&shot, true);
        let card = |label: &str| {
            groups[1]
                .1
                .iter()
                .find(|m| m.label == label)
                .map(|m| (m.text.clone(), m.source == Source::Measured))
                .unwrap()
        };
        assert_eq!(card("Offline"), (fmt_num(-4.2, 1, true), true));
        assert_eq!(card("Descent"), (fmt_num(43.1, 1, false), true));

        let columns = table_columns(false, true);
        let name = |n: &str| columns.iter().position(|c| c.name == n).expect(n);
        let cells = table_cells(&shot, None, None, false);
        assert_eq!(
            cells[name("Offline")],
            TableCell::Picked(Some((-4.2, false)), true)
        );
        assert_eq!(
            cells[name("Desc")],
            TableCell::Picked(Some((43.1, false)), false)
        );
        assert_eq!(
            cells[name("T.Offline")],
            TableCell::Picked(Some((-5.0, false)), true)
        );

        assert_eq!(BarKind::Lateral.value(&shot, true), Some(-4.2));

        let mut d = DerivedFlight::default();
        d.carry_offline = Some(Distance::Yards(-3.0));
        shot.derived = Some(d);
        assert_eq!(BarKind::Lateral.value(&shot, true), Some(-3.0));
    }

    #[test]
    fn table_header_apex_unit_is_feet_in_imperial_not_yards() {
        assert_eq!(table_header_units(true), ("mph", "yd", "ft"));
        assert_eq!(table_header_units(false), ("m/s", "m", "m"));
    }

    #[test]
    fn progress_is_linear_over_the_pulse() {
        let mut focus = ShotFocus::default();
        assert_eq!(focus.progress(0.0), None, "no shot, no pulse");
        let shots = vec![row(1, true)];
        focus.observe(&shots, 10.0);
        assert_eq!(focus.progress(10.0), Some(0.0));
        let quarter = focus.progress(10.0 + PULSE_SECS / 4.0).unwrap();
        assert!((quarter - 0.25).abs() < 1e-6);
        let half = focus.progress(10.0 + PULSE_SECS / 2.0).unwrap();
        assert!((half - 0.5).abs() < 1e-6);
        // The eased strength stays tied to it.
        assert!((focus.strength(10.0 + PULSE_SECS / 2.0) - 0.125).abs() < 1e-6);
        assert_eq!(focus.progress(10.0 + PULSE_SECS), None, "over");
        // A bulk load starts no pulse.
        focus.sync(&shots);
        assert_eq!(focus.progress(10.1), None);
    }

    /// A row from `actor` in set `group` with a measured total and carry and
    /// a simulated offline, in yards.
    fn device_shot(n: u32, actor: &str, group: &str, total: f64, offline: f64) -> ShotRow {
        ShotRow {
            actor: actor.into(),
            group: Some(group.into()),
            ..flight(
                n,
                (Some(total), Some(total - 10.0)),
                (None, None, Some(offline)),
            )
        }
    }

    #[test]
    fn bars_split_into_device_sub_bands() {
        let rows = vec![
            device_shot(1, "square.0", "a", 240.0, -3.0),
            device_shot(2, "mevo.0", "a", 250.0, 2.0),
            device_shot(3, "square.0", "a", 244.0, -1.0),
            device_shot(4, "mevo.0", "a", 252.0, 4.0),
        ];
        let all: Vec<usize> = (0..4).collect();
        let ids = chart::device_ids(&rows);
        assert_eq!(ids, vec!["mevo.0", "square.0"]);
        let [total, carry, lateral] = bars_for(&rows, &all, Some(3), &ids, true);
        // The whole bar is unchanged by the split.
        assert_eq!(total.stats.map(|s| (s.min, s.max)), Some((240.0, 252.0)));
        assert_eq!(total.this, Some(252.0));
        // One band per device, in sorted id order and its device colour.
        let bands = |b: &Bar| {
            b.devices
                .iter()
                .map(|d| (d.colour, d.stats.map(|s| (s.min, s.max))))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            bands(&total),
            vec![
                (theme::DEVICE_COLOURS[0], Some((250.0, 252.0))),
                (theme::DEVICE_COLOURS[1], Some((240.0, 244.0))),
            ]
        );
        assert_eq!(carry.devices.len(), 2);
        assert_eq!(
            bands(&lateral),
            vec![
                (theme::DEVICE_COLOURS[0], Some((2.0, 4.0))),
                (theme::DEVICE_COLOURS[1], Some((-3.0, -1.0))),
            ]
        );
        // A device with no value in a bar keeps its slot, empty.
        let mut gap = rows.clone();
        gap[1].derived = None;
        gap[3].derived = None;
        let [_, _, lateral] = bars_for(&gap, &all, None, &ids, true);
        assert_eq!(lateral.devices.len(), 2);
        assert!(lateral.devices[0].stats.is_none());
        // One device, or a device filter (no ids passed): a single band.
        let [total, ..] = bars_for(&rows, &all, None, &ids[..1], true);
        assert!(total.devices.is_empty());
        let [total, ..] = bars_for(&rows, &[1, 3], None, &[], true);
        assert!(total.devices.is_empty());
        assert_eq!(total.stats.map(|s| (s.min, s.max)), Some((250.0, 252.0)));
    }

    #[test]
    fn sub_bands_share_the_track() {
        let track = egui::Rangef::new(100.0, 100.0 + TRACK_H);
        for n in 1..=4 {
            let bands = sub_bands(track, n);
            assert_eq!(bands.len(), n);
            assert_eq!(bands[0].min, track.min);
            for b in &bands {
                assert!((b.span() - TRACK_H / n as f32).abs() < 1e-5, "{n}: {b:?}");
            }
            // Stacked, touching, filling the track.
            for w in bands.windows(2) {
                assert!((w[0].max - w[1].min).abs() < 1e-5);
            }
            assert!((bands[n - 1].max - track.max).abs() < 1e-4);
        }
        // Many devices: the 2 px floor.
        let bands = sub_bands(track, 9);
        assert!(bands.iter().all(|b| b.span() == SUB_BAND_MIN_H));
        assert!(sub_bands(track, 0).is_empty());
    }

    #[test]
    fn device_filter_shows_only_with_two_devices() {
        let named = |name: &str| ActorStatusResponse {
            name: name.into(),
            status: crate::types::ActorStatus::Connected,
            telemetry: HashMap::new(),
        };
        let actors: HashMap<String, ActorStatusResponse> = [
            ("mevo.0".to_owned(), named("Mevo+")),
            ("openconnect_server".to_owned(), named("")),
        ]
        .into_iter()
        .collect();
        assert_eq!(device_options(&[], &actors), None);
        assert_eq!(device_options(&["mevo.0".into()], &actors), None);
        // An unnamed or unknown device is labelled by its id.
        let ids: Vec<String> = ["mevo.0", "openconnect_server", "square.0"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            device_options(&ids, &actors),
            Some(
                ["All", "Mevo+", "openconnect_server", "square.0"]
                    .map(String::from)
                    .to_vec()
            )
        );
        // The ids are exact: an empty-actor row adds no device and merges
        // none (unlike the name-hiding rule's `same_actor`).
        let rows = vec![
            ShotRow {
                actor: String::new(),
                ..row(1, true)
            },
            row(2, true),
        ];
        assert_eq!(chart::device_ids(&rows), vec!["mock_monitor.0"]);
        assert_eq!(device_options(&chart::device_ids(&rows), &actors), None);
    }

    #[test]
    fn the_view_sets_the_bars_and_titles_them() {
        let i7 = |n, g: &str, total| ShotRow {
            club_selected: Some(Club::Iron7),
            ..device_shot(n, "mevo.0", g, total, 0.0)
        };
        // 7I (a), DR (b), 7I (c), then New session and 7I (d).
        let rows = vec![
            i7(1, "a", 160.0),
            i7(2, "a", 164.0),
            ShotRow {
                club_selected: Some(Club::Driver),
                ..device_shot(3, "mevo.0", "b", 250.0, 0.0)
            },
            i7(4, "c", 170.0),
            ShotRow {
                actor: "square.0".into(),
                ..i7(5, "c", 174.0)
            },
            i7(6, "d", 180.0),
        ];
        let first = vec![
            group("a", 1, Club::Iron7, false, 2),
            group("b", 2, Club::Driver, false, 1),
            group("c", 3, Club::Iron7, true, 2),
        ];
        let bars = |g: &ShotGroups, view, shown: usize, device| {
            let set = bar_group(g, Some(&rows[shown]), Some(Club::Iron7));
            let m = chart::members_for(&rows, g, view, set, Some(shown), device);
            let [total, ..] = bars_for(&rows, &m, Some(shown), &[], true);
            let title = bars_title(view, set, g.session(), &rows, &m, device.is_some());
            (
                total.stats.map(|s| (s.min, s.max)),
                total.this,
                title.map(|t| t.0),
            )
        };
        let g = groups(ShotGrouping::BySet, first.clone());
        assert_eq!(
            bars(&g, ShotGrouping::BySet, 4, None),
            (
                Some((170.0, 174.0)),
                Some(174.0),
                Some("Set 3 \u{b7} 7I \u{b7} 2 shots".to_owned())
            )
        );
        // By club: both 7-iron sets of the session; New set does not reset it.
        assert_eq!(
            bars(&g, ShotGrouping::ByClub, 4, None),
            (
                Some((160.0, 174.0)),
                Some(174.0),
                Some("7 Iron \u{b7} 2 sets \u{b7} 4 shots".to_owned())
            )
        );
        // The device filter in each view: the members narrow, the title
        // counts what is shown.
        assert_eq!(
            bars(&g, ShotGrouping::BySet, 4, Some("square.0")),
            (
                Some((174.0, 174.0)),
                Some(174.0),
                Some("Set 3 \u{b7} 7I \u{b7} 1 shot".to_owned())
            )
        );
        assert_eq!(
            bars(&g, ShotGrouping::ByClub, 4, Some("mevo.0")),
            (
                Some((160.0, 170.0)),
                None,
                Some("7 Iron \u{b7} 2 sets \u{b7} 3 shots".to_owned())
            )
        );

        // After New session: By club only has the new session's 7-iron.
        let mut later = first;
        later[2].open = false;
        later.push(sess(1, group("d", 4, Club::Iron7, true, 1)));
        let g = groups(ShotGrouping::ByClub, later);
        assert_eq!(
            bars(&g, ShotGrouping::ByClub, 5, None),
            (
                Some((180.0, 180.0)),
                Some(180.0),
                Some("7 Iron \u{b7} 1 shot".to_owned())
            )
        );
        // An earlier session's shot shows its own session's club, muted.
        let set = bar_group(&g, Some(&rows[0]), None);
        let m = chart::members_for(&rows, &g, ShotGrouping::ByClub, set, Some(0), None);
        assert_eq!(m, vec![0, 1, 3, 4]);
        assert_eq!(
            bars_title(ShotGrouping::ByClub, set, g.session(), &rows, &m, false),
            Some((
                "7 Iron \u{b7} Earlier \u{b7} 2 sets \u{b7} 4 shots".to_owned(),
                theme::TEXT_MUTED
            ))
        );

        // None: no bars and no device filter; the shown shot alone.
        assert!(!shows_bars(ShotGrouping::None));
        assert!(shows_bars(ShotGrouping::BySet) && shows_bars(ShotGrouping::ByClub));
        let m = chart::members_for(&rows, &g, ShotGrouping::None, set, Some(2), Some("x"));
        assert_eq!(m, vec![2]);
        assert_eq!(
            bars_title(ShotGrouping::None, set, g.session(), &rows, &m, false),
            None
        );
        // No set: the note instead of a title.
        assert_eq!(
            bars_title(ShotGrouping::BySet, None, 0, &rows, &[], false),
            None
        );
    }

    #[test]
    fn clear_history_is_disabled_without_shots_and_confirms_with_them() {
        use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};

        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        let frame = |app: &mut FlighthookApp, events: Vec<Event>| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| app.render_shots_toolbar(ui));
            });
        };
        // The icon is the toolbar's rightmost item, on its centre line.
        let at = pos2(1600.0 - 6.0, 19.0);
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let click = |app: &mut FlighthookApp| {
            frame(app, vec![Event::PointerMoved(at)]);
            frame(app, vec![button(true)]);
            frame(app, vec![button(false)]);
        };

        frame(&mut app, Vec::new());
        click(&mut app);
        assert!(!app.confirm_clear, "no shots: disabled, no confirmation");

        app.shots = vec![row(1, true), row(2, true)];
        app.shots_rev += 1;
        click(&mut app);
        assert!(app.confirm_clear, "the confirmation opens");
        frame(&mut app, Vec::new());
        assert!(app.confirm_clear, "and stays open");
        frame(
            &mut app,
            vec![Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert!(!app.confirm_clear, "Escape cancels");
        assert_eq!(app.shots.len(), 2, "nothing cleared");

        // Cleared elsewhere while open: it closes.
        click(&mut app);
        assert!(app.confirm_clear);
        app.shots.clear();
        frame(&mut app, Vec::new());
        assert!(!app.confirm_clear);
    }

    /// 1000 simulated shots from two devices, each with every table field.
    fn thousand_rows() -> Vec<ShotRow> {
        let yd = Distance::Yards;
        (0..1000_u32)
            .map(|n| {
                let carry = 130.0 + f64::from(n % 17);
                let ball = BallFlight {
                    launch_speed: Some(Velocity::MilesPerHour(118.0 + f64::from(n % 7))),
                    launch_elevation: Some(16.5),
                    launch_azimuth: Some(f64::from(n % 9) - 4.0),
                    carry_distance: Some(yd(carry)),
                    backspin_rpm: Some(6200),
                    sidespin_rpm: Some(-300),
                    ..Default::default()
                };
                let mut derived = DerivedFlight::default();
                derived.max_height = Some(yd(31.0));
                derived.carry_offline = Some(yd(2.5));
                derived.descent_angle = Some(48.0);
                derived.roll_distance = Some(yd(11.0));
                derived.total_distance = Some(yd(carry + 11.0));
                derived.total_offline = Some(yd(3.1));
                ShotRow {
                    actor: if n % 2 == 0 { "mevo.0" } else { "square.0" }.into(),
                    ball: Some(ball),
                    derived: Some(derived),
                    club_selected: Some(Club::Iron7),
                    ..row(n, true)
                }
            })
            .collect()
    }

    /// Every text the frame painted, with whether it is in italics.
    fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, bool)> {
        painted_text_rects(shapes)
            .into_iter()
            .map(|(text, italics, _)| (text, italics))
            .collect()
    }

    /// Every text the frame painted: the text, whether it is in italics, and
    /// where.
    fn painted_text_rects(
        shapes: &[egui::epaint::ClippedShape],
    ) -> Vec<(String, bool, egui::Rect)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, bool, egui::Rect)>) {
            match shape {
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(t) => {
                    let italics = t.galley.job.sections.iter().any(|s| s.format.italics);
                    out.push((
                        t.galley.text().to_owned(),
                        italics,
                        t.visual_bounding_rect(),
                    ));
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    /// Gold rectangles the frame painted, as `(width, height)`.
    fn gold_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<egui::Vec2> {
        shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r) if r.fill == theme::GOLD => Some(r.rect.size()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn table_lays_out_only_the_rows_in_view() {
        use egui::{Pos2, Rect, pos2, vec2};

        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.shots = thousand_rows();
        app.shots_rev = 1;
        app.shots_view = ShotsView::Table;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let frame = |app: &mut FlighthookApp, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
                events,
                ..Default::default()
            };
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.render_shots_panel(ui));
            })
        };
        // The first frames size the columns and settle the scroll.
        for _ in 0..5 {
            frame(&mut app, Vec::new());
        }
        let out = frame(&mut app, Vec::new());
        let header_at = |out: &egui::FullOutput| {
            painted_text_rects(&out.shapes)
                .into_iter()
                .find(|(t, _, _)| t == "T.Offline")
                .map(|(_, _, r)| r)
        };
        let header = header_at(&out).expect("header painted");
        let texts = painted_texts(&out.shapes);
        let has = |t: &str| texts.iter().any(|(s, _)| s == t);
        let numbers: Vec<u32> = texts.iter().filter_map(|(s, _)| s.parse().ok()).collect();

        // The sticky header, with the Device column for two devices.
        for name in [
            "Device",
            "#",
            "Club",
            "Carry",
            "Height",
            "T.Offline",
            "Smash",
        ] {
            assert!(has(name), "header {name}");
        }
        assert!(has("ft"), "apex in feet");
        // Stuck to the bottom: the latest shot is in view, the first is not,
        // and only a screenful of rows is laid out.
        assert!(numbers.contains(&999), "{numbers:?}");
        assert!(!numbers.contains(&500) && !numbers.contains(&3));
        assert!(texts.len() < 1500, "{} texts for 1000 rows", texts.len());
        // Simulated values in italics, measured ones not.
        assert!(
            texts.iter().any(|(s, i)| s == "+2.5" && *i),
            "simulated offline"
        );
        assert!(
            texts.iter().any(|(s, i)| s == "16.5" && !*i),
            "measured VLA"
        );
        // The latest row's gold stripe, a row tall.
        let stripe = gold_rects(&out.shapes);
        assert!(
            stripe.iter().any(|s| s.x == STRIPE_WIDTH
                && s.y >= ctx.style().text_styles[&egui::TextStyle::Body].size),
            "{stripe:?}"
        );

        // Scrolled up, the middle rows are laid out instead.
        for _ in 0..10 {
            frame(
                &mut app,
                vec![
                    egui::Event::PointerMoved(pos2(600.0, 450.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: vec2(0.0, 1300.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let out = frame(&mut app, Vec::new());
        let texts = painted_texts(&out.shapes);
        let numbers: Vec<u32> = texts.iter().filter_map(|(s, _)| s.parse().ok()).collect();
        assert!(!numbers.contains(&999), "the latest row scrolled away");
        assert!(
            numbers.iter().any(|n| (300..700).contains(n)),
            "{numbers:?}"
        );
        assert_eq!(header_at(&out), Some(header), "the header stays put");
        assert!(gold_rects(&out.shapes).iter().all(|s| s.x != STRIPE_WIDTH));
    }

    #[test]
    fn table_cells_follow_the_columns() {
        use TableCell::{Muted, Number, Picked, Text};

        let rows = thousand_rows();
        for show_device in [false, true] {
            let columns = table_columns(show_device, true);
            assert_eq!(columns.len(), TABLE_COLUMNS + usize::from(show_device));
            let device = show_device.then_some("Mevo+");
            assert_eq!(
                table_cells(&rows[0], device, None, false).len(),
                columns.len()
            );
            assert_eq!(
                table_cells(&row(1, false), device, None, false).len(),
                columns.len()
            );
            // The first column clears the latest row's stripe.
            assert_eq!(columns[0].inset, ROW_INSET);
            assert!(columns[1..].iter().all(|c| c.inset == 0.0));
        }
        let columns = table_columns(true, false);
        let name = |n: &str| columns.iter().position(|c| c.name == n).expect(n);
        assert_eq!(columns[name("Height")].unit, "m");
        assert_eq!(columns[name("Club Spd")].unit, "m/s");

        let cells = table_cells(&rows[4], Some("Mevo+"), Some(3), true);
        assert_eq!(cells[0], Muted("Mevo+".into(), None));
        assert_eq!(cells[1], Number(4, true));
        assert_eq!(cells[2], Muted("7I".into(), Some("Set 3".into())));
        assert_eq!(cells[name("VLA")], Text("16.5".into()));
        assert_eq!(cells[name("Carry")], Picked(Some((134.0, false)), false));
        assert_eq!(cells[name("Offline")], Picked(Some((2.5, true)), true));
        assert_eq!(cells[name("Roll")], Picked(Some((11.0, true)), true));
        assert_eq!(cells[name("Smash")], Text("-".into()), "no club data");
        // A device without a name shows a dash.
        assert_eq!(
            table_cells(&rows[4], Some(""), None, false)[0],
            Text("-".into())
        );
        // A carry in both, with or without force: the simulated one, in
        // italics.
        for replaces in [false, true] {
            let mut both = rows[4].clone();
            let d = both.derived.as_mut().expect("derived");
            d.carry_distance = Some(Distance::Yards(150.0));
            d.replaces_measured = replaces;
            let cells = table_cells(&both, None, None, false);
            assert_eq!(cells[name("Carry") - 1], Picked(Some((150.0, true)), false));
        }
    }

    #[test]
    fn table_rows_alternate_and_the_latest_is_tinted() {
        let (latest, stripe) = (egui::Color32::GOLD, egui::Color32::GRAY);
        assert_eq!(table_row_fill(0, 4, latest, stripe), None);
        assert_eq!(table_row_fill(1, 4, latest, stripe), Some(stripe));
        assert_eq!(table_row_fill(2, 4, latest, stripe), None);
        assert_eq!(table_row_fill(3, 4, latest, stripe), Some(stripe));
        assert_eq!(table_row_fill(4, 4, latest, stripe), Some(latest));
        assert_eq!(TABLE_COLUMNS, 20);
    }

    /// Mean frame time of the Table view with 1000 shots (and the Dispersion
    /// column), scrolled to the bottom and to the middle. Run with
    /// `cargo test --release -p flighthook-ui table_frame_time -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, not a check"]
    fn table_frame_time_with_1000_shots() {
        use egui::{Pos2, Rect, pos2, vec2};
        use std::time::Instant;

        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.shots = thousand_rows();
        app.shots_rev = 1;
        app.shots_view = ShotsView::Table;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let frames = 200;
        let run = |app: &mut FlighthookApp, scroll: f32| {
            let mut total = std::time::Duration::ZERO;
            for n in 0..frames + 20 {
                let mut events = vec![egui::Event::PointerMoved(pos2(
                    300.0 + (n % 50) as f32 * 16.0,
                    450.0,
                ))];
                if n < 10 && scroll != 0.0 {
                    events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: vec2(0.0, scroll),
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
                    events,
                    ..Default::default()
                };
                let start = Instant::now();
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.render_shots_panel(ui));
                });
                let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
                std::hint::black_box(prims);
                if n >= 20 {
                    total += start.elapsed();
                }
            }
            total / frames
        };
        let bottom = run(&mut app, 0.0);
        // 10 wheel steps of 1500 pt up from the bottom: about the middle.
        let middle = run(&mut app, 1500.0);
        println!(
            "Table, 1000 shots, 1600x900: frame {bottom:?} at the bottom, {middle:?} \
             scrolled up (run + tessellate, mean of {frames})"
        );
    }

    #[test]
    fn clear_confirm_counts_the_shots() {
        assert_eq!(clear_confirm_title(12), "Delete all 12 shots and sets?");
        assert_eq!(clear_confirm_title(1), "Delete the 1 shot and its set?");
    }
}
