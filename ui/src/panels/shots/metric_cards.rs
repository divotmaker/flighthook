//! The Cards view's metric cards: which metrics show (`card_metrics`), their layout
//! (`cards_layout`), and how a card and its value are painted.

use super::*;

/// Where a card's value came from.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Source {
    Measured,
    Simulated,
    Missing,
}

/// One metric card.
pub(super) struct Metric {
    pub(super) label: &'static str,
    pub(super) unit: &'static str,
    pub(super) text: String,
    /// A typically wide value of this metric, used to size the numerals.
    pub(super) sample: &'static str,
    pub(super) source: Source,
    /// The word for the value's sign (`Open`, `L`), shown after the unit
    /// when the value is its magnitude.
    pub(super) word: Option<&'static str>,
}

impl Metric {
    /// A metric showing a value picked by [`pick`] (measured or
    /// simulated), else missing.
    pub(super) fn new(
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
            word: None,
        }
    }

    /// A signed value shown as its magnitude with the word `words` gives
    /// its sign (none at 0), at the precision it is shown with.
    pub(super) fn worded(
        label: &'static str,
        unit: &'static str,
        value: Option<(f64, bool)>,
        prec: usize,
        words: impl Fn(f64) -> Option<&'static str>,
    ) -> Self {
        let value = value.map(|(v, simulated)| (round_to(v, prec), simulated));
        let mut m = Self::new(
            label,
            unit,
            value.map(|(v, simulated)| (v.abs(), simulated)),
            prec,
            false,
        );
        m.word = value.and_then(|(v, _)| words(v));
        m
    }

    pub(super) fn measured(
        label: &'static str,
        unit: &'static str,
        v: Option<f64>,
        prec: usize,
    ) -> Self {
        Self::new(label, unit, v.map(|v| (v, false)), prec, false)
    }
}

/// A titled group of detail cards.
pub(super) type MetricGroup = (&'static str, Vec<Metric>);

/// The Table view's column unit labels: speed, distance, height. `max_height`
/// converts to feet in imperial (`ShotData::to_imperial`), unlike every other
/// distance field, which converts to yards.
pub(super) fn table_header_units(imperial: bool) -> (&'static str, &'static str, &'static str) {
    if imperial {
        ("mph", "yd", "ft")
    } else {
        ("m/s", "m", "m")
    }
}

/// The Cards view's metrics for a shot already converted to the display unit
/// system: the headline row, then the Ball, Flight and Club groups.
///
/// Directional values show their magnitude with a word for the sign
/// ([`signs`]), read for a `handed` golfer.
#[allow(clippy::too_many_lines)] // one table of every card
pub(super) fn card_metrics(
    shot: &ShotRow,
    imperial: bool,
    handed: Handedness,
) -> (Vec<Metric>, [MetricGroup; 3]) {
    let (speed, dist) = if imperial {
        ("mph", "yd")
    } else {
        ("m/s", "m")
    };
    let height = if imperial { "ft" } else { "m" };
    // Borrowed: this runs per card per frame.
    let (no_ball, no_club, no_derived) = Default::default();
    let no_flight = FlightResult::new(Basis::Modeled);
    let ball: &Launch = shot.launch.as_ref().unwrap_or(&no_ball);
    let f: &FlightResult = shot.flight.as_ref().unwrap_or(&no_flight);
    let club: &ClubData = shot.club.as_ref().unwrap_or(&no_club);
    let d: &DerivedFlight = shot.derived.as_ref().unwrap_or(&no_derived);
    let (backspin, sidespin) = flighthook::spin_components(ball);
    let v = |x: Option<Distance>| x.map(|x| x.value());
    let measured = |x: Option<f64>| x.map(|x| (x, false));

    let headline = vec![
        Metric::measured("Ball speed", speed, ball.launch_speed.map(|v| v.value()), 1),
        Metric::new(
            "Carry",
            dist,
            pick(v(f.carry_distance), v(d.carry_distance)),
            1,
            false,
        ),
        Metric::new(
            "Total",
            dist,
            pick(v(f.total_distance), v(d.total_distance)),
            1,
            false,
        ),
        Metric::measured("Launch", "\u{b0}", ball.launch_elevation, 1),
        Metric::new("Backspin", "rpm", spin(backspin, d), 0, false),
    ];
    let groups = [
        (
            "Ball",
            vec![
                Metric::worded(
                    "Direction",
                    "\u{b0}",
                    measured(ball.launch_azimuth),
                    1,
                    signs::side,
                ),
                Metric::worded("Sidespin", "rpm", spin(sidespin, d), 0, |v| {
                    signs::sidespin(v, handed)
                }),
            ],
        ),
        (
            "Flight",
            vec![
                Metric::new(
                    "Apex",
                    height,
                    pick(v(f.max_height), v(d.max_height)),
                    1,
                    false,
                ),
                Metric::worded(
                    "Offline",
                    dist,
                    pick(v(f.carry_offline), v(d.carry_offline)),
                    1,
                    signs::side,
                ),
                Metric::new(
                    "Descent",
                    "\u{b0}",
                    pick(f.descent_angle, d.descent_angle),
                    1,
                    false,
                ),
                Metric::new(
                    "Roll",
                    dist,
                    pick(v(f.roll_distance), v(d.roll_distance)),
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
                Metric::worded("Path", "\u{b0}", measured(club.path), 1, |v| {
                    signs::path(v, handed)
                }),
                Metric::worded(
                    "AoA",
                    "\u{b0}",
                    measured(club.attack_angle),
                    1,
                    signs::attack,
                ),
                Metric::worded("Face", "\u{b0}", measured(club.face_angle), 1, |v| {
                    signs::face(v, handed)
                }),
                Metric::measured("Loft", "\u{b0}", club.dynamic_loft, 1),
            ],
        ),
    ];
    (headline, groups)
}

/// Gap between neighboring cards in a row.
pub(super) const CARD_GAP: f32 = 12.0;
/// Gap between detail groups side by side.
pub(super) const GROUP_GAP: f32 = 28.0;
pub(super) const HEADLINE_MIN_WIDTH: f32 = 120.0;
/// Narrowest detail card: room for an 18 px numeral with its unit, and for
/// the label beside a `SIM` tag.
pub(super) const DETAIL_MIN_WIDTH: f32 = 96.0;
/// Detail card height limit; headline cards grow `HEADLINE_RATIO` as fast.
pub(super) const DETAIL_MAX_HEIGHT: f32 = 136.0;
pub(super) const HEADLINE_RATIO: f32 = 1.6;
/// Tallest a detail card gets, as a fraction of the headline cards' height.
pub(super) const DETAIL_HEADLINE_RATIO: f32 = 0.8;
/// Tallest a card gets, as a fraction of its width.
pub(super) const MAX_ASPECT: f32 = 0.75;
/// Card rows of each detail group when the groups sit side by side.
pub(super) const DETAIL_ROWS: usize = 2;
/// Cards per row of each detail group when the groups share two columns.
pub(super) const PAIRED_COLS: usize = 2;
/// Share of the spare height that goes above the cards; the rest is below.
pub(super) const TOP_SLACK: f32 = 0.35;
/// Space kept free under the last card row, on top of the glow inset, so
/// the cards clear the panel below by a chart gap, scrolled or not.
pub(super) const CARDS_FOOT: f32 = CHART_GAP - GLOW_WIDTH as f32;
/// Smallest numerals: headline and detail cards.
pub(super) const HEADLINE_VALUE_MIN: f32 = 28.0;
pub(super) const DETAIL_VALUE_MIN: f32 = 18.0;

/// The Cards view's vertical sizes at one density, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rhythm {
    /// The shot header line and the space under it.
    pub(super) header: f32,
    pub(super) header_gap: f32,
    /// Headline card height.
    pub(super) headline: f32,
    /// Gap between the headline cards and the range bars' control row,
    /// then that row.
    pub(super) controls_gap: f32,
    pub(super) controls: f32,
    /// A label line: the range bars' title, a detail group's title.
    pub(super) label: f32,
    /// How far a legend row of its own tucks up into the label line above.
    pub(super) legend_overlap: f32,
    /// One range bar row and the gap between rows.
    pub(super) bar_row: f32,
    pub(super) bar_gap: f32,
    /// Gap above the detail groups, and between groups stacked in a column.
    pub(super) group_gap: f32,
    /// Gap between card rows.
    pub(super) row_gap: f32,
    /// Detail card height.
    pub(super) detail: f32,
}

/// The sizes with room to spare. Cards grow from here with the height.
pub(super) const ROOMY: Rhythm = Rhythm {
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
/// [`ROOMY`] toward these by the same share; below them it scrolls.
pub(super) const COMPACT: Rhythm = Rhythm {
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
    pub(super) fn between(t: f32) -> Self {
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
    pub(super) fn bars(&self, legend: Legend) -> f32 {
        let legend_row = match legend {
            Legend::Beside => 0.0,
            Legend::Below => self.label - self.legend_overlap,
        };
        self.controls + self.label + legend_row + 3.0 * self.bar_row + 2.0 * self.bar_gap
    }
}

/// Positions in the Cards view, relative to its top-left corner.
#[derive(Debug)]
pub(super) struct CardsLayout {
    pub(super) header: egui::Rect,
    pub(super) headline: Vec<egui::Rect>,
    /// The range bar section under the headline row.
    pub(super) bars: Option<BarsLayout>,
    /// The cards right of the range bars, left to right: under the
    /// rightmost headline columns, CARD_GAP from the headline cards and
    /// from the detail section's hairline.
    pub(super) cards: Vec<egui::Rect>,
    /// Per detail group: its label line, then its cards.
    pub(super) groups: Vec<(egui::Rect, Vec<egui::Rect>)>,
    /// Bottom of the last card.
    pub(super) height: f32,
    /// The cards don't fit the height even at [`COMPACT`] sizes.
    pub(super) scrolls: bool,
}

/// Where the range bars' legend goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Legend {
    /// On the label line, right of the title.
    Beside,
    /// On its own row under the title, when the line can't hold both.
    Below,
}

/// Positions in the range bar section, top to bottom.
#[derive(Debug, Clone, Copy)]
pub(super) struct BarsLayout {
    /// The device filter's row.
    pub(super) controls: egui::Rect,
    /// The label line: the title, and the legend when it sits beside it.
    pub(super) label: egui::Rect,
    /// The legend's line: `label` itself, or a row of its own under it.
    pub(super) legend: egui::Rect,
    /// One row per bar.
    pub(super) rows: [egui::Rect; 3],
}

impl BarsLayout {
    pub(super) fn translate(self, by: egui::Vec2) -> Self {
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
pub(super) struct DetailColumn {
    pub(super) groups: std::ops::Range<usize>,
    pub(super) cols: usize,
}

/// What a detail column stacks: card rows, label lines, gaps between card
/// rows of a group and gaps between groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Extent {
    pub(super) rows: usize,
    pub(super) labels: usize,
    pub(super) row_gaps: usize,
    pub(super) group_gaps: usize,
}

impl Extent {
    /// Height at sizes `r` with `detail`-tall cards.
    pub(super) fn height(&self, r: &Rhythm, detail: f32) -> f32 {
        self.rows as f32 * detail
            + self.labels as f32 * r.label
            + self.row_gaps as f32 * r.row_gap
            + self.group_gaps as f32 * r.group_gap
    }
}

impl DetailColumn {
    /// What the column stacks for `counts` cards per group.
    pub(super) fn extent(&self, counts: &[usize]) -> Extent {
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
pub(super) fn detail_columns(w: f32, counts: &[usize]) -> (Vec<DetailColumn>, f32) {
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

/// Most cards in the row right of the range bars.
pub(in crate::panels) const MAX_CARDS: usize = 3;
/// Gap between the range bars and the cards right of them.
pub(super) const FACE_GAP: f32 = 16.0;
/// Least width of a range bar's plot (its track inside the gutters) beside
/// the cards.
pub(super) const FACE_MIN_PLOT: f32 = 170.0;

/// The Cards layout with the range bars' legend placed where it fits the
/// bars as laid out. `place(w)` is where the legend goes in a bar section
/// `w` wide, and `at(legend)` lays the view out with it.
///
/// The cards beside the bars take their width whatever the legend does, so
/// the legend goes beside the title when the bars' label line holds both,
/// else on its own row. At most two layouts are tried, and the result never
/// shows a legend beside the title that does not fit there.
pub(super) fn settle_legend(
    place: impl Fn(f32) -> Legend,
    at: impl Fn(Legend) -> CardsLayout,
) -> CardsLayout {
    let beside = at(Legend::Beside);
    match beside.bars {
        Some(bars) if place(bars.label.width()) == Legend::Below => at(Legend::Below),
        _ => beside,
    }
}

/// Width of a range bar's plot in a bar section `width` wide.
pub(super) fn bar_plot_width(width: f32) -> f32 {
    width - BAR_LABEL_W - BAR_COL_GAP - 2.0 * GUTTER
}

/// The x ranges of up to `want` cards right of the range bars in a view `w`
/// wide with headline columns `head_w` wide, left to right.
///
/// Each card is a headline column wide, flush right under the rightmost
/// columns, as many as leave the bars the two leftmost columns and a plot
/// of at least [`FACE_MIN_PLOT`]. When not even one does, one card is
/// `square` wide; when that leaves the bars' plot under the least too, the
/// bars keep the least and the card takes what is left (zero wide when that
/// is under a point).
pub(super) fn card_columns(w: f32, head_w: f32, square: f32, want: usize) -> Vec<egui::Rangef> {
    let reserved = 2.0 * (head_w + CARD_GAP);
    for n in (1..=want.min(MAX_CARDS)).rev() {
        let left = w - n as f32 * head_w - (n as f32 - 1.0) * CARD_GAP;
        if left + 0.01 >= reserved && bar_plot_width(left - FACE_GAP) >= FACE_MIN_PLOT {
            return (0..n)
                .map(|i| {
                    let l = left + i as f32 * (head_w + CARD_GAP);
                    egui::Rangef::new(l, l + head_w)
                })
                .collect();
        }
    }
    if want == 0 {
        return Vec::new();
    }
    let side = if bar_plot_width(w - square - FACE_GAP) >= FACE_MIN_PLOT {
        square
    } else {
        // The narrowest bar section with the least plot; under a point
        // wide, no card at all.
        let least = FACE_MIN_PLOT - bar_plot_width(0.0);
        Some(w - FACE_GAP - least)
            .filter(|&side| side >= 1.0)
            .unwrap_or(0.0)
    };
    vec![egui::Rangef::new(w - side, w)]
}

/// Lay out the Cards view in `size`: the shot header, `headline` cards in
/// balanced rows, the range bar section when `bars` (with its legend placed
/// as given) and up to `want` cards right of it, then detail groups of
/// `groups[i]` cards arranged by `detail_columns`.
///
/// The cards sit flush right ([`card_columns`]), each from [`CARD_GAP`]
/// under the headline cards to [`CARD_GAP`] above the hairline of the first
/// detail section's label, [`CARD_GAP`] apart. The bars end [`FACE_GAP`]
/// short of the leftmost card, and no height changes with the cards.
///
/// Everything fits the height whenever it can. With room to spare, the
/// sizes are [`ROOMY`], the cards grow with the height up to a limit, and
/// what is still spare is split above and below. Short of room, every
/// height (cards, bar rows, label lines and gaps) shrinks toward
/// [`COMPACT`] by the same share, just enough to fit. Shorter still, the
/// sizes stay [`COMPACT`] and `scrolls` is set.
pub(super) fn cards_layout(
    size: egui::Vec2,
    headline: usize,
    groups: &[usize],
    bars: Option<Legend>,
    want: usize,
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
    // The cards run from CARD_GAP under the headline cards to CARD_GAP
    // above the detail section's hairline; the fallback square is the bar
    // section's label-to-last-bar height.
    let cards: Vec<Rect> = bars.map_or_else(Vec::new, |legend| {
        let top = head_bottom + CARD_GAP;
        let bottom =
            head_bottom + r.controls_gap + r.bars(legend) + r.group_gap + GROUP_RULE_Y - CARD_GAP;
        let square = r.bars(legend) - r.controls;
        card_columns(w, head_w, square, want)
            .into_iter()
            .map(|x| Rect::from_x_y_ranges(x, top..=bottom))
            .collect()
    });
    // A zero-wide card takes no gap from the bars.
    let bars_w = cards
        .first()
        .filter(|card| card.width() > 0.0)
        .map_or(w, |card| card.left() - FACE_GAP);
    let bars = bars.map(|place| {
        let controls = Rect::from_min_size(pos2(0.0, bars_top), vec2(bars_w, r.controls));
        let label = Rect::from_min_size(pos2(0.0, controls.bottom()), vec2(bars_w, r.label));
        let legend = match place {
            Legend::Beside => label,
            Legend::Below => Rect::from_min_size(
                pos2(0.0, label.bottom() - r.legend_overlap),
                vec2(bars_w, r.label),
            ),
        };
        let top = legend.bottom().max(label.bottom());
        let rows = std::array::from_fn(|i| {
            Rect::from_min_size(
                pos2(0.0, top + i as f32 * (r.bar_row + r.bar_gap)),
                vec2(bars_w, r.bar_row),
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
        cards,
        groups: out,
        height,
        scrolls,
    }
}

/// Size-dependent styling of a card.
#[derive(Clone, Copy)]
pub(super) struct CardLook {
    pub(super) radius: u8,
    pub(super) pad: f32,
    pub(super) label: f32,
    /// Numeral font size.
    pub(super) value: f32,
}

/// How far `height` is from `compact` (0) to `roomy` (1).
pub(super) fn density(height: f32, compact: f32, roomy: f32) -> f32 {
    ((height - compact) / (roomy - compact)).clamp(0.0, 1.0)
}

/// `t` of the way from `a` to `b`.
pub(super) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

impl CardLook {
    /// Narrow cards get less padding, leaving more room for the numeral;
    /// short ones get less padding and a smaller label.
    pub(super) fn headline(size: egui::Vec2) -> Self {
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
    pub(super) fn detail(size: egui::Vec2) -> Self {
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
    pub(super) fn fit<'a>(
        mut self,
        painter: &egui::Painter,
        metrics: impl IntoIterator<Item = &'a Metric>,
        width: f32,
    ) -> Self {
        let metrics: Vec<&Metric> = metrics.into_iter().collect();
        let widest = |size: f32| {
            metrics
                .iter()
                .map(|m| {
                    let sample = Reading::new(painter, m, m.sample, None, size, true).width();
                    Reading::new(painter, m, &m.text, None, size, true)
                        .width()
                        .max(sample)
                })
                .fold(0.0, f32::max)
        };
        let room = width - 2.0 * self.pad;
        // Numerals scale with their font size, so measure once at a
        // reference size; word units have a floor size, so check the result
        // and step down while anything is still too wide. Sign words are
        // fitted per card (`fitted_reading`).
        const REFERENCE: f32 = 100.0;
        let at_reference = widest(REFERENCE);
        if at_reference > 0.0 {
            self.value = self.value.min((REFERENCE * room / at_reference).floor());
            while self.value > NUMERAL_FLOOR && widest(self.value) > room {
                self.value -= 1.0;
            }
        }
        self
    }
}

/// Approximate descent of the proportional font, as a fraction of its size.
pub(crate) const DESCENT: f32 = 0.2;
/// Approximate height of a numeral, as a fraction of the font size.
pub(super) const DIGIT_HEIGHT: f32 = 0.7;

/// A card's numeral, unit and sign word, laid out at one font size. The
/// unit and word offsets are from the numeral's top-left corner.
pub(super) struct Reading {
    pub(super) value: std::sync::Arc<egui::Galley>,
    pub(super) unit: Option<(std::sync::Arc<egui::Galley>, egui::Vec2)>,
    pub(super) word: Option<(std::sync::Arc<egui::Galley>, egui::Vec2)>,
    pub(super) color: egui::Color32,
}

/// Font size of a card's word unit or sign word next to a `size` numeral.
pub(crate) fn small_size(size: f32) -> f32 {
    (size * 0.36).max(12.0)
}

/// The smallest a card's numeral shrinks to when fitting its width.
pub(crate) const NUMERAL_FLOOR: f32 = 8.0;
/// The smallest a sign word shrinks to before the numeral gives way.
pub(crate) const WORD_MIN: f32 = 9.0;

/// `m`'s reading at numeral size `size` within `room`: the sign word starts
/// at the word unit's size and shrinks toward [`WORD_MIN`] to fit; only
/// when it still doesn't fit does this card's numeral shrink. Returns the
/// reading and its numeral size.
pub(super) fn fitted_reading(
    painter: &egui::Painter,
    m: &Metric,
    size: f32,
    room: f32,
) -> (Reading, f32) {
    let mut size = size;
    loop {
        let mut word_size = small_size(size);
        loop {
            let reading = Reading::new(
                painter,
                m,
                &m.text,
                m.word.map(|w| (w, word_size)),
                size,
                true,
            );
            let fits = reading.width() <= room;
            if fits || m.word.is_none() || (word_size <= WORD_MIN && size <= NUMERAL_FLOOR) {
                return (reading, size);
            }
            if word_size <= WORD_MIN {
                break;
            }
            word_size = (word_size - 0.5).max(WORD_MIN);
        }
        size = (size - 1.0).max(NUMERAL_FLOOR);
    }
}

impl Reading {
    /// `distinguish_source` applies the simulated-value styling (a muted
    /// color); when false, every value but a missing one reads as plain
    /// text regardless of its source. Values are always upright: italics
    /// for simulated values is Table's own thing (`cell`), not drawn here.
    pub(super) fn new(
        painter: &egui::Painter,
        m: &Metric,
        text: &str,
        word: Option<(&str, f32)>,
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
            let unit_size = if degree { size * 0.7 } else { small_size(size) };
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

        // The sign word follows the unit on the numeral's baseline.
        let word = word
            .filter(|_| m.source != Source::Missing)
            .map(|(w, word_size)| {
                let galley = painter.layout_no_wrap(
                    w.to_owned(),
                    FontId::proportional(word_size),
                    theme::TEXT,
                );
                let end = unit
                    .as_ref()
                    .map_or(value.size().x, |(g, off)| off.x + g.size().x);
                let baseline_gap = DESCENT * (size - word_size);
                let offset = vec2(
                    end + size * 0.16,
                    value.size().y - baseline_gap - galley.size().y,
                );
                (galley, offset)
            });
        Self {
            value,
            unit,
            word,
            color,
        }
    }

    pub(super) fn width(&self) -> f32 {
        let end = |part: &Option<(std::sync::Arc<egui::Galley>, egui::Vec2)>| {
            part.as_ref().map_or(0.0, |(g, off)| off.x + g.size().x)
        };
        self.value
            .size()
            .x
            .max(end(&self.unit))
            .max(end(&self.word))
    }
}

/// Bands in a card's top sheen.
pub(super) const SHEEN_STEPS: u8 = 7;

/// Width of the new-shot glow around a card, in pixels.
pub(crate) const GLOW_WIDTH: u8 = 6;

/// Paint the new-shot glow around `rect`: concentric 1 px rings, each with
/// the card radius grown by its distance, so the halo hugs the card and
/// follows its rounded corners. Opacity falls off quadratically outwards.
pub(super) fn glow(painter: &egui::Painter, rect: egui::Rect, radius: u8, pulse: f32) {
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
pub(super) fn sheen(painter: &egui::Painter, rect: egui::Rect, radius: egui::CornerRadius) {
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
pub(super) fn card(ui: &egui::Ui, rect: egui::Rect, m: &Metric, look: CardLook, pulse: f32) {
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

    // Center the numeral's digits in the space under the label.
    let (reading, size) = fitted_reading(painter, m, look.value, rect.width() - 2.0 * pad);
    let middle = (label_bottom + rect.bottom()) / 2.0;
    let digits_middle = reading.value.size().y - (DESCENT + DIGIT_HEIGHT / 2.0) * size;
    let pos = pos2(rect.left() + pad, (middle - digits_middle).round());
    // Drawn three times, a fraction of a pixel apart, for a heavier numeral
    // than the default light face.
    let weight = (size / 56.0).clamp(0.5, 1.4);
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
    if let Some((word, offset)) = reading.word {
        painter.galley(pos + offset, word, theme::TEXT);
    }

    if m.source == Source::Simulated {
        ui.interact(rect, ui.id().with(m.label), egui::Sense::hover())
            .on_hover_text(SIMULATED_HINT);
    }
}

/// Paint the `SIM` tag with `font`-sized text where `place` puts it, given
/// its size. Returns its rect.
pub(super) fn sim_tag(
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
pub(super) fn group_title(
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
pub(super) fn group_label_width(painter: &egui::Painter, title: &str) -> f32 {
    GROUP_TITLE_LEFT + group_title(painter, title, theme::TEXT).size().x
}

/// Left of a label line's title: its bar and the gap after it.
pub(super) const GROUP_TITLE_LEFT: f32 = 3.0 + 8.0;
/// Height of a section label's title and hairline under the top of its
/// label line.
pub(super) const GROUP_RULE_Y: f32 = 9.0;

/// Paint a section's label line in `color`: a bar, the caps title and a
/// hairline out to the section's right edge.
pub(super) fn group_label(
    painter: &egui::Painter,
    rect: egui::Rect,
    title: &str,
    color: egui::Color32,
) {
    use egui::{pos2, vec2};

    let mid = rect.top() + GROUP_RULE_Y;
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
