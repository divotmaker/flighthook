//! The range bars and the stats card in the row under the headline cards: bar values,
//! domains, device sub-bands, legend and the device filter's names.

use super::*;

/// Gap between the range bars' title and a legend beside it.
pub(super) const LEGEND_GAP: f32 = 16.0;

/// Where the range bars' legend goes in a section `width` wide: beside the
/// `title` when both fit on the label line, else on its own row. Without a
/// title the line holds a short note, and the legend is not drawn.
pub(super) fn legend_place(
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
pub(super) const BAR_LABEL_W: f32 = 92.0;
/// Gap between a bar row's columns.
pub(super) const BAR_COL_GAP: f32 = 14.0;
/// Height of a bar's track.
pub(super) const TRACK_H: f32 = 14.0;
/// Least height of a device's sub-band in the track.
pub(super) const SUB_BAND_MIN_H: f32 = 2.0;
/// Room at both ends of a track, outside the domain, for the labels beside
/// a band that reaches its ends.
pub(super) const GUTTER: f32 = 36.0;
/// Narrowest range band; a single value draws a pip this wide. Wide enough
/// that the pip still peeks out past the median ring when every marker lands
/// on the same value.
pub(super) const PIP_W: f32 = 16.0;
/// How far the "this shot" line reaches past the track, top and bottom.
pub(super) const MARKER_OVERHANG: f32 = 3.0;
/// Radius of the median ring, drawn just inside the track's half height.
pub(super) const MEDIAN_RING_RADIUS: f32 = TRACK_H / 2.0 - 1.0;
/// Stroke width of the median ring.
pub(super) const MEDIAN_RING_STROKE: f32 = 2.0;
/// Radius of the average dot: smaller than the median ring's inner radius,
/// so it fits inside the ring and fills it when the two coincide.
pub(super) const AVG_DOT_RADIUS: f32 = 4.0;
/// Width of the dark halo stroke around the average dot.
pub(super) const AVG_DOT_HALO: f32 = 1.0;
/// Width of the dark halo drawn behind the "this shot" line on each side,
/// so the gold line still reads where it crosses the median ring and the
/// average dot.
pub(super) const SHOT_LINE_HALO: f32 = 1.0;
/// Padding around the distance domain, as a share of its range.
pub(super) const DISTANCE_PAD: f64 = 0.15;
/// Least padding around the distance domain, in yards and in meters.
pub(super) const DISTANCE_PAD_YD: f64 = 10.0;
pub(super) const DISTANCE_PAD_M: f64 = 9.0;
/// Least half-width of the lateral domain before its margin, in display units.
pub(super) const LATERAL_FLOOR: f64 = 5.0;
/// Margin of the lateral domain past its widest value.
pub(super) const LATERAL_MARGIN: f64 = 1.2;

/// One of the range bars.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum BarKind {
    Total,
    Carry,
    Lateral,
}

impl BarKind {
    /// Every bar, top to bottom.
    pub(super) const ALL: [BarKind; 3] = [BarKind::Total, BarKind::Carry, BarKind::Lateral];

    pub(super) fn label(self) -> &'static str {
        match self {
            BarKind::Total => "Total",
            BarKind::Carry => "Carry",
            BarKind::Lateral => "Lateral",
        }
    }

    /// This bar's value for `shot` in display units: total and carry are
    /// picked by [`pick`]; lateral is the carry offline, picked the same way
    /// (+ = right of the target line).
    pub(super) fn value(self, shot: &ShotRow, imperial: bool) -> Option<f64> {
        let flight = shot.flight.as_ref();
        let derived = shot.derived.as_ref();
        let (measured, simulated) = match self {
            BarKind::Total => (
                flight.and_then(|f| f.total_distance),
                derived.and_then(|d| d.total_distance),
            ),
            BarKind::Carry => (
                flight.and_then(|f| f.carry_distance),
                derived.and_then(|d| d.carry_distance),
            ),
            BarKind::Lateral => (
                flight.and_then(|f| f.carry_offline),
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
    /// with one and its side, `3.2 L` / `1.4 R` (`0.0` on the line).
    pub(super) fn format(self, v: f64, prec: usize) -> String {
        match self {
            BarKind::Lateral => {
                let v = round_to(v, 1);
                match signs::side(v) {
                    Some(side) => format!("{:.1} {side}", v.abs()),
                    None => "0.0".to_owned(),
                }
            }
            _ => format!("{v:.prec$}"),
        }
    }
}

/// One bar's values over a group's rows.
#[derive(Debug, PartialEq)]
pub(super) struct BarValues {
    pub(super) values: Vec<f64>,
    /// Rows with data; the ones without this bar's value are left out of
    /// `values`.
    pub(super) rows: usize,
}

/// `kind`'s values over `rows`, skipping rows without data.
pub(super) fn bar_values<'a>(
    rows: impl IntoIterator<Item = &'a ShotRow>,
    kind: BarKind,
    imperial: bool,
) -> BarValues {
    let mut out = BarValues {
        values: Vec::new(),
        rows: 0,
    };
    for row in rows {
        if !row.has_data() {
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
pub(super) fn distance_domain(stats: &[Option<Stats>], imperial: bool) -> Option<(f64, f64)> {
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
pub(super) fn lateral_domain(stats: Option<&Stats>) -> (f64, f64) {
    let widest = stats.map_or(0.0, |s| s.min.abs().max(s.max.abs()));
    let half = widest.max(LATERAL_FLOOR) * LATERAL_MARGIN;
    (-half, half)
}

/// Where `v` of `domain` falls on the plot span `x`, clamped to it; the
/// middle when the position is undefined (an empty or non-finite domain).
pub(super) fn bar_x(domain: (f64, f64), x: egui::Rangef, v: f64) -> f32 {
    let (lo, hi) = domain;
    let t = (v - lo) / (hi - lo);
    let t = if hi > lo && !t.is_nan() { t } else { 0.5 };
    x.min + x.span() * t.clamp(0.0, 1.0) as f32
}

/// The horizontal extent of the band from `min` to `max` on the plot span
/// `x`: at least `PIP_W` wide, centered on the values when narrower.
pub(super) fn band_x(domain: (f64, f64), x: egui::Rangef, min: f64, max: f64) -> egui::Rangef {
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
pub(super) fn shows_bars(view: ShotGrouping) -> bool {
    view != ShotGrouping::None
}

/// The set the range bars are built around: the shown shot's set, else the
/// set the next shot joins with `club` in play. `None` when there is no
/// such set.
pub(in crate::panels) fn bar_group<'a>(
    groups: &'a ShotGroups,
    shot: Option<&ShotRow>,
    club: Option<Club>,
) -> Option<&'a SetInfo> {
    shot.and_then(|s| s.group.as_deref())
        .and_then(|id| groups.get(id))
        .or_else(|| groups.shown_current(club))
}

/// One device's share of a range bar, drawn as a sub-band in its color.
#[derive(Debug)]
pub(super) struct DeviceBand {
    pub(super) color: egui::Color32,
    pub(super) stats: Option<Stats>,
}

/// One range bar, ready to paint.
pub(super) struct Bar {
    pub(super) kind: BarKind,
    /// Display unit.
    pub(super) unit: &'static str,
    pub(super) values: BarValues,
    pub(super) stats: Option<Stats>,
    /// One sub-band per device, in device order, when the bar is split by
    /// device; empty otherwise.
    pub(super) devices: Vec<DeviceBand>,
    /// The shown shot's value, when it is a member and has one.
    pub(super) this: Option<f64>,
    pub(super) domain: Option<(f64, f64)>,
}

/// The range bars over the rows `members` (indices into `shots`), with the
/// shown shot `shots[shown]` marked when it is a member. With two or more
/// `devices` (exact actor ids, sorted), each bar also splits into one
/// sub-band per device, colored by `chart::device_color`.
pub(super) fn bars_for(
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
                    color: chart::device_color(devices, id),
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
pub(super) fn sub_bands(track: egui::Rangef, n: usize) -> Vec<egui::Rangef> {
    let h = (track.span() / n.max(1) as f32).max(SUB_BAND_MIN_H);
    (0..n)
        .map(|i| {
            let top = track.min + i as f32 * h;
            egui::Rangef::new(top, top + h)
        })
        .collect()
}

/// "1 shot", "3 shots".
pub(super) fn shot_count(n: usize) -> String {
    if n == 1 {
        "1 shot".to_owned()
    } else {
        format!("{n} shots")
    }
}

/// Title and color of the range bar section in `view`, around the set
/// `set`, over `members` of `shots`; `session` is the current session.
///
/// - By set: the set's header. Its count is the set's own without its
///   Auto-chip shots ([`chart::set_shot_count`]) unless a device filter
///   (`filtered`) narrows it to the members.
/// - By club: the club's section header, counted over the members.
///
/// `None` when there is no set to show, which shows a note instead, and in
/// the view `none`, which has no bars.
pub(super) fn bars_title(
    view: ShotGrouping,
    set: Option<&SetInfo>,
    session: u32,
    shots: &[ShotRow],
    members: &[usize],
    filtered: bool,
) -> Option<(String, egui::Color32)> {
    let g = set?;
    let with_data: Vec<&ShotRow> = members
        .iter()
        .filter_map(|&i| shots.get(i))
        .filter(|r| r.has_data())
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
            let shots = if filtered {
                u32::try_from(with_data.len()).unwrap_or(u32::MAX)
            } else {
                chart::set_shot_count(g, shots, ShotFilter::ALL)
            };
            let counted = SetInfo { shots, ..g.clone() };
            let title = set_title(&counted, next);
            Some((title, color))
        }
    }
}

/// The name of the device `id`: its card's name in `actors`, else a remote
/// actor's `bus.actor` name in `remote`.
pub(crate) fn actor_name<'a>(
    actors: &'a HashMap<String, ActorStatusResponse>,
    remote: &'a HashMap<String, String>,
    id: &str,
) -> Option<&'a str> {
    actors
        .get(id)
        .map(|a| a.name.as_str())
        .filter(|n| !n.is_empty())
        .or_else(|| remote.get(id).map(String::as_str))
}

/// Labels of the device filter's options: `All`, then each of `ids` by its
/// name (its actor id when unnamed). `None` with fewer than two devices,
/// when the filter is not shown.
#[cfg(test)]
pub(in crate::panels) fn device_options(
    ids: &[String],
    actors: &HashMap<String, ActorStatusResponse>,
) -> Option<Vec<String>> {
    device_options_with(ids, actors, &HashMap::new())
}

/// [`device_options`] naming remote actors by their `bus.actor` names in
/// `remote`.
pub(in crate::panels) fn device_options_with(
    ids: &[String],
    actors: &HashMap<String, ActorStatusResponse>,
    remote: &HashMap<String, String>,
) -> Option<Vec<String>> {
    (ids.len() >= 2).then(|| {
        std::iter::once("All".to_owned())
            .chain(
                ids.iter()
                    .map(|id| chart::device_label(id, actor_name(actors, remote, id)).to_owned()),
            )
            .collect()
    })
}

/// What the stats card shows for a value it has no number for.
pub(super) const NO_VALUE: &str = "\u{2014}";

/// One row of the stats card: a bar's average and median as the bar prints
/// them (lateral with its side), its sample standard deviation to one
/// decimal, and how many shots have the value when some lack it. Without a
/// value, a dash in each column (and with one value, in the SD column).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StatsRow {
    pub(super) avg: String,
    pub(super) med: String,
    pub(super) sd: String,
    /// `k/n`: only k of the n shots with data have the bar's value.
    pub(super) count: Option<String>,
}

impl StatsRow {
    pub(super) fn of(bar: &Bar) -> Self {
        let Some(s) = &bar.stats else {
            return Self {
                avg: NO_VALUE.to_owned(),
                med: NO_VALUE.to_owned(),
                sd: NO_VALUE.to_owned(),
                count: None,
            };
        };
        let k = bar.kind;
        Self {
            avg: k.format(s.mean, 0),
            med: k.format(s.median, 0),
            sd: s
                .std_dev
                .map_or_else(|| NO_VALUE.to_owned(), |sd| format!("{sd:.1}")),
            count: (s.count < bar.values.rows).then(|| format!("{}/{}", s.count, bar.values.rows)),
        }
    }

    pub(super) fn values(&self) -> [&str; 3] {
        [&self.avg, &self.med, &self.sd]
    }
}

/// Hover text on a bar's track.
pub(super) fn bar_tooltip(bar: &Bar) -> String {
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
    text
}

/// Paint the legend of the range bars, right-aligned at `right` on the line
/// through `mid`, with a color chip per device in `chips` on its left.
/// Returns its left edge.
pub(super) fn bars_legend(
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
    for (name, color) in chips.iter().rev() {
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
        painter.rect_filled(swatch, egui::CornerRadius::same(2), *color);
        x -= 8.0 + 12.0;
    }
    x + if chips.is_empty() { 16.0 } else { 12.0 }
}

/// Paint the range bar section `at`: the label line (`title`, or `note`
/// when there is none), the legend with the device `chips` beside the title
/// or on its own row, then one row per bar.
pub(super) fn range_bars(
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

/// Paint one range bar row in `row`: label and unit, then the track.
pub(super) fn bar_row(ui: &egui::Ui, row: egui::Rect, bar: &Bar) {
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
        (row.left() + BAR_LABEL_W + BAR_COL_GAP)..=row.right(),
        (row.center().y - TRACK_H / 2.0)..=(row.center().y + TRACK_H / 2.0),
    );
    let radius = egui::CornerRadius::same((TRACK_H / 2.0) as u8);
    painter.rect_filled(track, radius, theme::VEIL);
    let plot = egui::Rangef::new(track.left() + GUTTER, track.right() - GUTTER);
    if let (Some(s), Some(domain)) = (&bar.stats, bar.domain) {
        let band = band_x(domain, plot, s.min, s.max);
        let paint_band = |x: egui::Rangef, y: egui::Rangef, color: egui::Color32| {
            painter.rect(
                egui::Rect::from_x_y_ranges(x, y),
                egui::CornerRadius::same((x.span().min(y.span()) / 2.0) as u8),
                color.gamma_multiply(0.28),
                Stroke::new(1.0, color.gamma_multiply(0.6)),
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
                    paint_band(band_x(domain, plot, ds.min, ds.max), y, d.color);
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
        painter.galley(pos2(x, y(&after)), after, theme::TEXT_MUTED);
        if s.count > 1 {
            let before = numeral(bar.kind.format(s.min, 0));
            let x = (band.min - 6.0 - before.size().x).max(track.left() + 4.0);
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
    ui.interact(
        track,
        ui.id().with(("bar_track", bar.kind)),
        egui::Sense::hover(),
    )
    .on_hover_text(bar_tooltip(bar));
}

/// The stats card's column headers, over the [`StatsRow`] values.
pub(super) const STATS_COLUMNS: [&str; 3] = ["Avg", "Med", "SD"];
/// The stats card's value size, the least it shrinks to with the row names
/// beside the values, and the least in a narrow card.
pub(super) const STATS_VALUE: f32 = 13.0;
pub(super) const STATS_BESIDE_MIN: f32 = 11.0;
pub(super) const STATS_VALUE_MIN: f32 = 8.0;
/// Sizes of the stats card's column headers, row names (as the bars'
/// labels), units and `k/n`.
pub(super) const STATS_HEAD: f32 = 9.5;
pub(super) const STATS_NAME: f32 = 11.0;
pub(super) const STATS_UNIT: f32 = 10.0;
pub(super) const STATS_COUNT: f32 = 10.0;
/// Gap before each of the stats card's value columns, and between a row
/// name's parts on one line.
pub(super) const STATS_COL_GAP: f32 = 8.0;
pub(super) const STATS_NAME_GAP: f32 = 4.0;
/// Where the stats card's title sits under the card's top.
pub(super) const STATS_TITLE_TOP: f32 = 8.0;

/// Caps text in the label style of the Cards view.
pub(super) fn caps_galley(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: egui::Color32,
) -> std::sync::Arc<egui::Galley> {
    use egui::text::{LayoutJob, TextFormat};

    painter.layout_job(LayoutJob::single_section(
        text.to_uppercase(),
        TextFormat {
            font_id: egui::FontId::proportional(size),
            color,
            extra_letter_spacing: 0.6,
            ..Default::default()
        },
    ))
}

/// The stats card's table across a width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StatsTable {
    /// The row names sit in a column of their own left of the values (the
    /// unit and any `k/n` on a second line under the name and the values);
    /// else each row's name, unit and `k/n` share a line over its values.
    pub(super) beside: bool,
    /// Value size.
    pub(super) value: f32,
    /// Right edge of each value column, from the table's left.
    pub(super) columns: [f32; 3],
    /// Right edge of the widest line.
    pub(super) width: f32,
}

/// Lay the stats table of `bars`' `rows` out across `width`. The row names
/// go beside the values while the values fit at [`STATS_BESIDE_MIN`] or
/// more, else over them; either way the values start at [`STATS_VALUE`]
/// and shrink in half-point steps (to [`STATS_VALUE_MIN`] with the names
/// over them) until the widest line fits. Below that the gaps close up.
pub(super) fn stats_table(
    painter: &egui::Painter,
    bars: &[Bar; 3],
    rows: &[StatsRow; 3],
    width: f32,
) -> StatsTable {
    let text_w = |text: &str, size: f32| {
        painter
            .layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(size),
                theme::TEXT,
            )
            .size()
            .x
    };
    let names = bars.each_ref().map(|bar| {
        caps_galley(painter, bar.kind.label(), STATS_NAME, theme::TEXT_MUTED)
            .size()
            .x
    });
    let units = bars.each_ref().map(|bar| text_w(bar.unit, STATS_UNIT));
    let counts = rows
        .each_ref()
        .map(|r| r.count.as_deref().map_or(0.0, |c| text_w(c, STATS_COUNT)));
    let head_w = STATS_COLUMNS.map(|h| caps_galley(painter, h, STATS_HEAD, theme::TEXT).size().x);
    let at = |beside: bool, value: f32, gap: f32| {
        let mut x = if beside {
            names.iter().chain(&units).copied().fold(0.0, f32::max)
        } else {
            -gap
        };
        let columns = std::array::from_fn(|j| {
            let widest = rows
                .iter()
                .map(|r| text_w(r.values()[j], value))
                .fold(head_w[j], f32::max);
            x += gap + widest;
            x
        });
        // The second line's unit, or the name line's parts, then the k/n.
        let lines = (0..3)
            .map(|k| {
                let first = if beside {
                    units[k]
                } else {
                    names[k] + STATS_NAME_GAP + units[k]
                };
                if counts[k] > 0.0 {
                    first + gap + counts[k]
                } else {
                    first
                }
            })
            .fold(0.0, f32::max);
        StatsTable {
            beside,
            value,
            columns,
            width: x.max(lines),
        }
    };
    for (beside, least) in [(true, STATS_BESIDE_MIN), (false, STATS_VALUE_MIN)] {
        let mut value = STATS_VALUE;
        loop {
            let table = at(beside, value, STATS_COL_GAP);
            if table.width <= width {
                return table;
            }
            if value <= least {
                break;
            }
            value = (value - 0.5).max(least);
        }
    }
    // Still too wide at the least size: close the gaps up.
    let tight = at(false, STATS_VALUE_MIN, 0.0);
    let gap = ((width - tight.width) / 2.0).clamp(2.0, STATS_COL_GAP);
    at(false, STATS_VALUE_MIN, gap)
}

/// Paint the stats card at `rect`: the title `STATS` in its top-left in the
/// headline cards' label style (`title` points), then a table of one row
/// per bar, in bar order, named like the bars with the unit after the
/// name: the average, median and standard deviation under `AVG`, `MED` and
/// `SD` ([`StatsRow`]), and `k/n` at the row's end, under or beside the
/// SD, when some shots lack the value ([`stats_table`] picks the
/// arrangement). The rows sit on the bars' rows (`rows`) when the table
/// fits there under the title, else they spread evenly down the card.
pub(super) fn stats_card(
    painter: &egui::Painter,
    rect: egui::Rect,
    bars: &[Bar; 3],
    rows: &[egui::Rect; 3],
    title: f32,
) {
    use egui::{Align2, pos2};

    impact::paint_card(painter, rect);
    if rect.width() < 1.0 {
        return;
    }
    let painter = painter.with_clip_rect(painter.clip_rect().intersect(rect));
    let left = rect.left() + impact::PAD;
    let width = (rect.width() - 2.0 * impact::PAD).max(0.0);
    let title = caps_galley(&painter, "Stats", title, theme::TEXT_MUTED);
    let title_bottom = rect.top() + STATS_TITLE_TOP + title.size().y;
    painter.galley(
        pos2(left, rect.top() + STATS_TITLE_TOP),
        title,
        theme::TEXT_MUTED,
    );

    let texts = bars.each_ref().map(StatsRow::of);
    let table = stats_table(&painter, bars, &texts, width);
    let heads = STATS_COLUMNS.map(|h| caps_galley(&painter, h, STATS_HEAD, theme::TEXT_MUTED));
    let head_h = heads[0].size().y;
    let line_h = |size: f32| {
        painter
            .layout_no_wrap(
                "8".to_owned(),
                egui::FontId::proportional(size),
                theme::TEXT,
            )
            .size()
            .y
    };
    let name_h = caps_galley(&painter, "Total", STATS_NAME, theme::TEXT_MUTED)
        .size()
        .y;
    let value_h = line_h(table.value);
    // Beside: the name line (as tall as a value) over the unit line; else
    // the name line over the value line.
    let (first_h, row_h) = if table.beside {
        let first = name_h.max(value_h);
        (first, first + line_h(STATS_UNIT))
    } else {
        (name_h, name_h + value_h)
    };
    // On the bars' rows when the headers still clear the title and the
    // last row the card's foot; else spread down the card under the title.
    let aligned = rows.map(|r| r.center().y);
    let foot = rect.bottom() - impact::PAD / 2.0;
    let centers = if aligned[0] - row_h / 2.0 - head_h - 2.0 >= title_bottom + 2.0
        && aligned[2] + row_h / 2.0 <= foot
    {
        aligned
    } else {
        let top = title_bottom + 4.0 + head_h + 2.0;
        let slot = ((foot - top) / 3.0).max(row_h);
        std::array::from_fn(|k| top + slot * (k as f32 + 0.5))
    };

    let head_bottom = centers[0] - row_h / 2.0 - 2.0;
    for (head, right) in heads.into_iter().zip(table.columns) {
        let pos = pos2(left + right - head.size().x, head_bottom - head_h);
        painter.galley(pos, head, theme::TEXT_MUTED);
    }
    let right = left + table.columns[2];
    for ((bar, text), c) in bars.iter().zip(&texts).zip(centers) {
        let top = c - row_h / 2.0;
        let first_mid = top + first_h / 2.0;
        let second_mid = top + first_h + (row_h - first_h) / 2.0;
        let name = caps_galley(&painter, bar.kind.label(), STATS_NAME, theme::TEXT_MUTED);
        let name_w = name.size().x;
        painter.galley(
            pos2(left, first_mid - name_h / 2.0),
            name,
            theme::TEXT_MUTED,
        );
        let (unit_at, values_mid, count_mid) = if table.beside {
            (pos2(left, second_mid), first_mid, second_mid)
        } else {
            let unit = pos2(left + name_w + STATS_NAME_GAP, first_mid);
            (unit, second_mid, first_mid)
        };
        painter.text(
            unit_at,
            Align2::LEFT_CENTER,
            bar.unit,
            egui::FontId::proportional(STATS_UNIT),
            theme::TEXT_DIM,
        );
        let color = if bar.stats.is_some() {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        for (value, x) in text.values().into_iter().zip(table.columns) {
            painter.text(
                pos2(left + x, values_mid),
                Align2::RIGHT_CENTER,
                value,
                egui::FontId::proportional(table.value),
                color,
            );
        }
        if let Some(count) = &text.count {
            painter.text(
                pos2(right, count_mid),
                Align2::RIGHT_CENTER,
                count,
                egui::FontId::proportional(STATS_COUNT),
                theme::TEXT_MUTED,
            );
        }
    }
}

/// Whether the card in the row beside the range bars at `rect` (slot
/// `slot`) was clicked, with the pointer cursor over it.
pub(super) fn row_card_clicked(ui: &egui::Ui, rect: egui::Rect, slot: usize) -> bool {
    rect.width() >= 1.0
        && ui
            .interact(rect, ui.id().with(("row_card", slot)), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
}
