//! The Table view: its columns, header, cells and row painting.

use super::*;

/// Columns before the Replay column (`#` and Club), not counting Device.
pub(super) const REPLAY_AFTER: usize = 2;

/// The Table view's Replay column: a button on each shot with clips.
pub(super) fn replay_column() -> TableColumn {
    TableColumn {
        name: "Replay",
        unit: "",
        inset: 0.0,
    }
}

/// A Table view column: its header name and unit, and how far its cells
/// sit in from its left edge.
pub(super) struct TableColumn {
    pub(super) name: &'static str,
    pub(super) unit: &'static str,
    pub(super) inset: f32,
}

/// The Table view's columns: Device (when `show_device`), `#`, Club, the
/// ball columns, the club columns, then face impact (millimeters in both
/// unit systems).
pub(super) fn table_columns(show_device: bool, imperial: bool) -> Vec<TableColumn> {
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
        column("Impact H", "mm"),
        column("Impact V", "mm"),
    ]);
    columns
}

/// The Table view's header, laid out: two lines per column (name over
/// unit), and the width each column needs for them.
pub(super) struct TableHeader {
    pub(super) height: f32,
    pub(super) widths: Vec<f32>,
}

impl TableHeader {
    pub(super) const NAME: f32 = 12.5;
    pub(super) const UNIT: f32 = 11.0;

    pub(super) fn layout(painter: &egui::Painter, columns: &[TableColumn]) -> Self {
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
    pub(super) fn paint(
        &self,
        ui: &egui::Ui,
        origin: egui::Pos2,
        columns: &[TableColumn],
        widths: &[f32],
    ) {
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
pub(super) enum TableCell {
    /// Plain text.
    Text(String),
    /// A muted label, with hover text when there is some.
    Muted(String, Option<String>),
    /// The shot number: gold and bold on the latest row.
    Number(u32, bool),
    /// A value picked by [`pick`] (italics when simulated), with an explicit
    /// sign when `signed`.
    Picked(Option<(f64, bool)>, bool),
    /// A spin in rpm from [`spin`] (italics when estimated).
    Spin(Option<(f64, bool)>),
    /// The replay button, on a shot with clips.
    Replay(bool),
}

impl TableCell {
    /// Draw the cell. Returns whether its button was clicked.
    pub(super) fn show(&self, ui: &mut egui::Ui) -> bool {
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
            Self::Spin(value) => cell(ui, *value, 0, false),
            Self::Replay(true) => {
                let height = ui.text_style_height(&egui::TextStyle::Body);
                return theme::icon(ui, theme::Icon::Video, height, theme::ACCENT, true).clicked();
            }
            Self::Replay(false) => {}
        }
        false
    }
}

/// The Table view's cells for `shot`, already in display units, in
/// [`table_columns`] order: `device` (its display name, when the Device
/// column shows), its set number for the Club cell's hover, and whether it
/// is the latest row.
pub(super) fn table_cells(
    shot: &ShotRow,
    device: Option<&str>,
    set: Option<u32>,
    latest: bool,
) -> Vec<TableCell> {
    use TableCell::{Muted, Number, Picked, Spin, Text};

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
    let no_launch = Launch::default();
    let no_flight = FlightResult::new(Basis::Modeled);
    match (shot.launch.as_ref(), shot.flight.as_ref()) {
        (None, None) => cells.extend((0..BALL_COLUMNS).map(|_| Text("-".to_owned()))),
        (l, f) => cells.extend({
            let l = l.unwrap_or(&no_launch);
            let f = f.unwrap_or(&no_flight);
            let (backspin, sidespin) = flighthook::spin_components(l);
            [
                Text(opt_f(l.launch_speed.map(|v| v.value()), 1)),
                Text(opt_f(l.launch_elevation, 1)),
                Text(opt_f(l.launch_azimuth, 1)),
                Picked(pick(v(f.carry_distance), v(d.carry_distance)), false),
                Picked(pick(v(f.max_height), v(d.max_height)), false),
                Picked(pick(v(f.carry_offline), v(d.carry_offline)), true),
                Picked(pick(f.descent_angle, d.descent_angle), false),
                Picked(pick(v(f.roll_distance), v(d.roll_distance)), true),
                Picked(pick(v(f.total_distance), v(d.total_distance)), false),
                Picked(pick(v(f.total_offline), v(d.total_offline)), true),
                Spin(spin(backspin, d)),
                Spin(spin(sidespin, d)),
            ]
        }),
    }
    match shot.club.as_ref() {
        Some(c) => {
            let ball_speed = shot.launch.as_ref().and_then(|b| b.launch_speed);
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
    let (lat, vert) = impact::impact_mm(shot.impact.as_ref());
    let (h, v) = impact::impact_words(lat, vert);
    cells.extend([Text(h), Text(v)]);
    cells
}

/// How the Table view paints its rows' backgrounds.
#[derive(Clone, Copy)]
pub(super) struct RowLook {
    /// The latest row's tint.
    pub(super) latest: egui::Color32,
    /// Every other row's background.
    pub(super) stripe: egui::Color32,
    /// Strength of the new-shot glow, 0 to 1.
    pub(super) pulse: f32,
}

/// One row's background: its fill, for the latest row the gold stripe
/// and the glow's strength, and for the shot selected in the shot list
/// (when it is not the latest) an accent stripe.
#[derive(Clone, Copy)]
pub(super) struct RowPaint {
    pub(super) fill: Option<egui::Color32>,
    pub(super) latest: bool,
    pub(super) selected: bool,
    pub(super) pulse: f32,
}

impl RowLook {
    pub(super) fn for_row(self, idx: usize, last: usize, selected: bool) -> RowPaint {
        RowPaint {
            fill: table_row_fill(idx, last, self.latest, self.stripe),
            latest: idx == last,
            selected: selected && idx != last,
            pulse: self.pulse,
        }
    }
}

/// Add one Table view row of `cells` to `row`. The latest row gets a gold
/// stripe on its left edge and an outline that fades with the new-shot
/// glow; the selected row an accent stripe. Returns whether its replay
/// button was clicked.
pub(super) fn table_row(
    row: &mut egui_extras::TableRow<'_, '_>,
    columns: &[TableColumn],
    cells: Vec<TableCell>,
    paint: RowPaint,
) -> bool {
    let count = cells.len();
    let mut left = None;
    let mut clicked = false;
    for (i, (cell, column)) in cells.into_iter().zip(columns).enumerate() {
        row.col(|ui| {
            if let Some(fill) = paint.fill {
                fill_cell(ui, fill);
            }
            let rect = ui.max_rect();
            let first = *left.get_or_insert(rect.left());
            let band = rect.y_range().expand(TABLE_ROW_GAP / 2.0);
            if (paint.latest || paint.selected) && i == 0 {
                let bar = egui::Rect::from_x_y_ranges(first..=first + STRIPE_WIDTH, band);
                let color = if paint.latest {
                    theme::GOLD
                } else {
                    theme::ACCENT
                };
                ui.painter().rect_filled(bar, STRIPE_WIDTH / 2.0, color);
            }
            if paint.latest && i + 1 == count && paint.pulse > 0.0 {
                ui.painter().rect_stroke(
                    egui::Rect::from_x_y_ranges(first..=rect.right(), band),
                    2.0,
                    egui::Stroke::new(1.0, theme::ACCENT.gamma_multiply(paint.pulse)),
                    egui::StrokeKind::Inside,
                );
            }
            inset_cell(ui, column.inset, |ui| clicked |= cell.show(ui));
        });
    }
    clicked
}

/// Background of table row `idx` (of `0..=last`): `latest` for the latest
/// row, `stripe` on every other row counting from the first, else none.
pub(super) fn table_row_fill(
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
pub(super) fn fill_cell(ui: &egui::Ui, color: egui::Color32) {
    use egui::emath::GuiRounding as _;

    let rect = ui
        .max_rect()
        .expand2(0.5 * ui.spacing().item_spacing)
        .round_ui();
    ui.painter().rect_filled(rect, 0.0, color);
}
