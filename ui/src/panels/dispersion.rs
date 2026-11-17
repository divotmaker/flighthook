//! The Dispersion panel: a top-down view of where the shots came to rest
//! (Total) or landed (Carry), right of or under the Cards or Table view
//! (`shots::chart_slots`).
//!
//! Equal scales on both axes, the tee at the bottom center, downrange up
//! and right to the right, with distance rings and a target line.
//!
//! - **Gold**: the shown shot, with its roll drawn from the landing to the
//!   rest. The latest shot flies out from the tee during the new-shot pulse.
//! - **Bright**: the view's other members (`chart::members_for`), colored
//!   per device when every device is shown and there are two or more.
//! - **Faint**: the rest of the current session, newest
//!   [`flight::FAINT_CAP`], for context. The view `none` draws the shown
//!   shot alone.
//!
//! One covariance ellipse (2σ or 95 %) over the view's members (bright and
//! gold), or one per device in its color, with a gold cross at each mean.
//! A shown Auto-chip is drawn gold but is never a member. A point is the
//! path's landing or rest when the shot has a simulated path, else its
//! carry or total with the simulated offline; shots with neither are left
//! out. The projected geometry is cached and rebuilt only when the members,
//! units, shown shot, device filter, panel size or the panel's two switches
//! change.

use egui::{Color32, Pos2, Rect, Stroke, Vec2, pos2, vec2};

use super::flight;
use super::shots::pick;
use crate::app::FlighthookApp;
use crate::chart::{self, Candidate, ChartKey, Curve, Ellipse, Projection, ShotFilter};
use crate::groups::ShotGroups;
use crate::theme;
use crate::types::{Distance, ShotGrouping, ShotRow, UnitSystem};

/// Hover text of the `Total | Carry` switch.
const POINTS_HINT: &str = "Plot where each shot came to rest (Total) or landed (Carry)";
/// Hover text of the `2σ | 95 %` switch.
const SIZE_HINT: &str =
    "Ellipse size: 2\u{3c3} holds about 86 % of a normal pattern, 95 % holds 95 %";

/// Height of a header row: one switch.
pub(super) const ROW_H: f32 = 30.0;
/// Narrowest panel whose title and switches share one row.
pub(super) const ONE_ROW_MIN_WIDTH: f32 = 312.0;
/// Label size of the header switches.
const CONTROL_FONT: f32 = 12.0;
/// Room left of the plot for the ring labels.
const LEFT_GUTTER: f32 = 30.0;
/// Room right of the plot.
const RIGHT_PAD: f32 = 10.0;
/// Gap between the header and the plot.
const TOP_PAD: f32 = 6.0;
/// Room under the plot for the lateral labels.
const BOTTOM_GUTTER: f32 = 16.0;
/// Height a panel adds around its plot: the gaps above and below it.
pub(super) const PLOT_PAD: f32 = TOP_PAD + BOTTOM_GUTTER;
/// Widest plot, as a multiple of its height.
const MAX_ASPECT: f32 = 1.25;
/// Narrowest gap between distance rings before they thin out.
const RING_MIN_PX: f32 = 60.0;
/// Narrowest gap between lateral ticks before they thin out.
const LATERAL_MIN_PX: f32 = 36.0;
/// Vertices of a distance ring.
const RING_POINTS: usize = 48;
/// Hover radius around a dot.
const DOT_HIT: f32 = 8.0;
/// Alpha of the faint dots.
const FAINT_ALPHA: f32 = 0.18;
/// Alpha of an ellipse's fill.
const FILL_ALPHA: f32 = 0.10;
/// Alpha of an ellipse's outline.
const OUTLINE_ALPHA: f32 = 0.6;
/// Radius of a faint dot.
const FAINT_R: f32 = 2.5;
/// Radius of a bright dot.
const BRIGHT_R: f32 = 3.0;
/// Radius of the gold dot.
const GOLD_R: f32 = 5.0;
/// Half the span of a mean cross.
const CROSS_HALF: f32 = 5.0;

// ---------------------------------------------------------------------------
// Switches
// ---------------------------------------------------------------------------

/// Which end of each shot the panel plots.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Points {
    /// Where the ball came to rest.
    #[default]
    Total,
    /// Where the ball landed.
    Carry,
}

impl Points {
    /// Every option, in switch order.
    const ALL: [Points; 2] = [Points::Total, Points::Carry];

    fn label(self) -> &'static str {
        match self {
            Points::Total => "Total",
            Points::Carry => "Carry",
        }
    }
}

/// Size of the dispersion ellipse.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum EllipseSize {
    /// Two standard deviations: about 86 % of a normal pattern.
    #[default]
    TwoSigma,
    /// 95 % of a normal pattern.
    P95,
}

impl EllipseSize {
    /// Every option, in switch order.
    const ALL: [EllipseSize; 2] = [EllipseSize::TwoSigma, EllipseSize::P95];

    fn label(self) -> &'static str {
        match self {
            EllipseSize::TwoSigma => "2\u{3c3}",
            EllipseSize::P95 => "95 %",
        }
    }

    /// The ellipse's size factor.
    #[must_use]
    pub(crate) fn k(self) -> f64 {
        match self {
            EllipseSize::TwoSigma => chart::ELLIPSE_K_2SIGMA,
            EllipseSize::P95 => chart::ELLIPSE_K_95,
        }
    }
}

// ---------------------------------------------------------------------------
// Where a shot landed and stopped
// ---------------------------------------------------------------------------

/// A position seen from above, in display units: lateral (+ right of the
/// target line) and downrange from the tee.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spot {
    lateral: f64,
    down: f64,
}

/// A shot's landing and rest, seen from above.
#[derive(Debug, Clone, Default, PartialEq)]
struct Ends {
    landing: Option<Spot>,
    rest: Option<Spot>,
    /// Landing to rest along the ground, when both are known.
    roll: Vec<Spot>,
}

impl Ends {
    /// The plotted end.
    fn at(&self, points: Points) -> Option<Spot> {
        match points {
            Points::Total => self.rest,
            Points::Carry => self.landing,
        }
    }
}

/// Where `shot` landed and stopped, in display units. A simulated path
/// gives its last flight point (landing) and last ground point (rest).
/// Without one, the displayed carry or total (picked like the cards: the
/// simulated value when there is one, else the measured one) is the
/// distance from the tee and the offline, picked the same way, the lateral;
/// an end without both is unknown.
fn ends(shot: &ShotRow, units: UnitSystem) -> Ends {
    let k = flight::per_meter(units).0;
    if let Some(p) = flight::path(shot) {
        let top = |q: &[f64; 4]| Spot {
            lateral: q[3] * k,
            down: q[1] * k,
        };
        return Ends {
            landing: p.flight.last().map(top),
            rest: p.ground.last().map(top),
            roll: if p.ground.len() >= 2 {
                p.ground.iter().map(top).collect()
            } else {
                Vec::new()
            },
        };
    }
    let imperial = units == UnitSystem::Imperial;
    let to = |d: Distance| {
        if imperial {
            d.as_yards()
        } else {
            d.as_meters()
        }
    };
    let flight = shot.flight.as_ref();
    let derived = shot.derived.as_ref();
    let spot = |measured: (Option<Distance>, Option<Distance>),
                sim: (Option<Distance>, Option<Distance>)| {
        let (radius, _) = pick(measured.0, sim.0)?;
        let (lateral, _) = pick(measured.1, sim.1)?;
        let (radius, lateral) = (to(radius), to(lateral));
        if !radius.is_finite() || !lateral.is_finite() {
            return None;
        }
        // Carry and total are distances from the tee.
        Some(Spot {
            lateral,
            down: (radius * radius - lateral * lateral).max(0.0).sqrt(),
        })
    };
    let landing = spot(
        (
            flight.and_then(|f| f.carry_distance),
            flight.and_then(|f| f.carry_offline),
        ),
        (
            derived.and_then(|d| d.carry_distance),
            derived.and_then(|d| d.carry_offline),
        ),
    );
    let rest = spot(
        (
            flight.and_then(|f| f.total_distance),
            flight.and_then(|f| f.total_offline),
        ),
        (
            derived.and_then(|d| d.total_distance),
            derived.and_then(|d| d.total_offline),
        ),
    );
    Ends {
        landing,
        rest,
        roll: landing
            .zip(rest)
            .map(|(a, b)| vec![a, b])
            .unwrap_or_default(),
    }
}

/// Whether `shot` has a point to plot for `points`.
fn plottable(shot: &ShotRow, points: Points) -> bool {
    ends(shot, UnitSystem::Metric).at(points).is_some()
}

// ---------------------------------------------------------------------------
// Scene: the projected geometry, cached
// ---------------------------------------------------------------------------

/// Everything a Dispersion scene depends on: the chart key and the panel's
/// two switches.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DispersionKey {
    pub(crate) chart: ChartKey,
    pub(crate) points: Points,
    pub(crate) size: EllipseSize,
}

/// How prominently a shot is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Faint,
    Bright,
    Gold,
}

/// One plotted shot, in panel-local screen points.
#[derive(Debug)]
struct Dot {
    index: usize,
    level: Level,
    color: Color32,
    at: Pos2,
    /// The plotted lateral, in display units.
    lateral: f64,
}

/// A dispersion ellipse and where it is drawn.
#[derive(Debug)]
struct Oval {
    ellipse: Ellipse,
    color: Color32,
    /// The device it covers, when there is one per device.
    device: Option<String>,
    polygon: Vec<Pos2>,
    /// The mean, on screen.
    center: Pos2,
}

/// A distance ring and the label where it meets the plot's left edge.
#[derive(Debug)]
struct Ring {
    arc: Vec<Pos2>,
    label: Option<(f32, String)>,
}

/// Everything the panel paints, in panel-local points (origin at the plot
/// area's allocation), built once per [`DispersionKey`].
#[derive(Debug)]
pub(crate) struct Scene {
    points: Points,
    size: EllipseSize,
    plot: Rect,
    proj: Projection,
    rings: Vec<Ring>,
    lateral_ticks: Vec<(f32, String)>,
    /// Faint, then bright, then gold.
    dots: Vec<Dot>,
    /// The gold shot's roll, landing to rest.
    roll: Vec<Pos2>,
    /// The gold shot's path seen from above, flight and ground, as smooth
    /// curves for its draw-on (`chart::smooth_path`).
    track: Option<(Curve, Curve)>,
    ovals: Vec<Oval>,
    /// Members of the view.
    members: usize,
}

/// What a scene is built from.
struct SceneInput<'a> {
    shots: &'a [ShotRow],
    groups: &'a ShotGroups,
    view: ShotGrouping,
    members: &'a [usize],
    shown: usize,
    /// Every device in the history, exact and sorted.
    devices: &'a [String],
    /// The Shots tab's filter.
    filter: ShotFilter<'a>,
    units: UnitSystem,
    points: Points,
    size: EllipseSize,
    /// Size of the plot area's allocation.
    area: Vec2,
}

/// Height of the header of a panel `width` wide: the title row, and the
/// switches' own row when they don't fit beside the title.
#[must_use]
pub(super) fn header_height(width: f32) -> f32 {
    let rows = 1 + usize::from(width < ONE_ROW_MIN_WIDTH);
    rows as f32 * ROW_H
}

/// The plot inside an allocation of `area`: the whole of it, or in a wide
/// area a plot [`MAX_ASPECT`] times as wide as it is tall, centered, so the
/// equal axes keep a useful scale.
fn plot_rect(area: Vec2) -> Rect {
    let min = pos2(LEFT_GUTTER, TOP_PAD);
    let max = pos2(
        (area.x - RIGHT_PAD).max(min.x + 1.0),
        (area.y - BOTTOM_GUTTER).max(min.y + 1.0),
    );
    let full = Rect::from_min_max(min, max);
    let width = full.width().min(full.height() * MAX_ASPECT);
    let left = (full.center().x - width / 2.0).round();
    Rect::from_x_y_ranges(left..=left + width, full.y_range())
}

/// Distance rings every `step` out to the far corner of the plot, as arcs
/// about the tee clipped to the lateral domain `x_hi`, labeled where they
/// meet the plot's left edge.
fn rings(proj: &Projection, step: f64) -> Vec<Ring> {
    let (_, x_hi) = proj.x.domain();
    let (_, y_hi) = proj.y.domain();
    chart::ticks(step, x_hi.hypot(y_hi), step)
        .into_iter()
        .map(|r| {
            // Bearing from the target line where the ring leaves the plot
            // sideways (a half circle when it ends on the ground).
            let edge = (x_hi / r).min(1.0).asin();
            let arc = (0..=RING_POINTS)
                .map(|i| {
                    let b = -edge + 2.0 * edge * i as f64 / RING_POINTS as f64;
                    proj.to_screen(r * b.sin(), r * b.cos())
                })
                .collect();
            let down = (r * r - x_hi * x_hi).max(0.0).sqrt();
            let label = (x_hi < r && down <= y_hi).then(|| (proj.y.map(down), format!("{r:.0}")));
            Ring { arc, label }
        })
        .collect()
}

fn build_scene(input: &SceneInput<'_>) -> Scene {
    let SceneInput {
        shots,
        groups,
        view,
        members,
        shown,
        devices,
        filter,
        units,
        points,
        size,
        area,
    } = *input;
    let drawn = flight::drawn_sets(shots, groups, view, members, shown, filter, |s| {
        plottable(s, points)
    });
    let ends_of = |i: usize| shots.get(i).map(|s| ends(s, units)).unwrap_or_default();
    let spots = |list: &[usize]| -> Vec<(usize, Spot)> {
        list.iter()
            .filter_map(|&i| ends_of(i).at(points).map(|s| (i, s)))
            .collect()
    };
    let faint = spots(&drawn.faint);
    let bright = spots(&drawn.bright);
    let gold_ends = drawn.gold.map(ends_of);
    let gold = drawn
        .gold
        .zip(gold_ends.as_ref().and_then(|e| e.at(points)));

    // Scales over the bright and gold points (and the gold roll), else the
    // faint ones.
    let mut scaled: Vec<Spot> = bright.iter().map(|b| b.1).collect();
    if let Some(e) = gold_ends.as_ref().filter(|_| gold.is_some()) {
        scaled.extend(e.landing.iter().chain(&e.rest).copied());
    }
    if scaled.is_empty() {
        scaled = faint.iter().map(|f| f.1).collect();
    }
    let (down, lateral) = scaled
        .iter()
        .filter(|s| chart::plausible(s.down) && chart::plausible(s.lateral))
        .fold((0.0_f64, 0.0_f64), |a, s| {
            (a.0.max(s.down), a.1.max(s.lateral.abs()))
        });
    let plot = plot_rect(area);
    let proj = Projection::equal(
        plot,
        chart::OVERHEAD_LATERAL.domain_max(lateral),
        chart::OVERHEAD_DOWNRANGE.domain_max(down),
    );
    let ring_step = chart::tick_step(
        chart::OVERHEAD_DOWNRANGE.step,
        proj.y.px_per_unit(),
        RING_MIN_PX,
    );
    let (_, x_hi) = proj.x.domain();
    let lateral_step = chart::tick_step(
        chart::OVERHEAD_LATERAL.step,
        proj.x.px_per_unit(),
        LATERAL_MIN_PX,
    );
    // Clear of the L and R at the plot's corners.
    let edge = f64::from(14.0 / proj.x.px_per_unit().max(1e-3));
    let lateral_ticks = chart::ticks(-x_hi + edge, x_hi - edge, lateral_step)
        .into_iter()
        .filter(|v| *v != 0.0)
        .map(|v| (proj.x.map(v), format!("{:.0}", v.abs())))
        .collect();

    let by_device = chart::by_device(devices, filter.device);
    let color_of = |i: usize| {
        shots.get(i).map_or(theme::ACCENT, |s| {
            chart::shot_color(devices, filter.device, &s.actor)
        })
    };
    let to_screen = |s: Spot| proj.to_screen(s.lateral, s.down);
    let dot = |level: Level| {
        move |&(index, s): &(usize, Spot)| Dot {
            index,
            level,
            color: color_of(index),
            at: to_screen(s),
            lateral: s.lateral,
        }
    };
    let dots: Vec<Dot> = faint
        .iter()
        .map(dot(Level::Faint))
        .chain(bright.iter().map(dot(Level::Bright)))
        .chain(gold.iter().map(dot(Level::Gold)))
        .collect();
    let roll = match (&gold, &gold_ends) {
        (Some(_), Some(e)) => e.roll.iter().map(|s| to_screen(*s)).collect(),
        _ => Vec::new(),
    };
    let k = flight::per_meter(units).0;
    let track = gold
        .and_then(|(i, _)| shots.get(i))
        .and_then(flight::path)
        .map(|p| chart::smooth_path(p, |q| proj.to_screen(q[3] * k, q[1] * k)));

    // One ellipse over the bright points and the gold one, or one per
    // device. A shown Auto-chip is drawn but is not one of the view's shots.
    let lit: Vec<(usize, Spot)> = bright
        .iter()
        .copied()
        .chain(gold.filter(|(i, _)| members.binary_search(i).is_ok()))
        .collect();
    let clusters: Vec<(Option<String>, Color32, Vec<[f64; 2]>)> = if by_device {
        devices
            .iter()
            .map(|d| {
                let pts = lit
                    .iter()
                    .filter(|(i, _)| shots.get(*i).is_some_and(|s| &s.actor == d))
                    .map(|(_, s)| [s.lateral, s.down])
                    .collect();
                (Some(d.clone()), chart::device_color(devices, d), pts)
            })
            .collect()
    } else {
        vec![(
            None,
            theme::ACCENT,
            lit.iter().map(|(_, s)| [s.lateral, s.down]).collect(),
        )]
    };
    let ovals = clusters
        .into_iter()
        .filter_map(|(device, color, pts)| {
            let ellipse = chart::ellipse_from_points(&pts, size.k())?;
            let polygon = chart::ellipse_points(&ellipse, chart::ELLIPSE_SEGMENTS)
                .into_iter()
                .map(|p| proj.to_screen(p[0], p[1]))
                .collect();
            Some(Oval {
                center: proj.to_screen(ellipse.center[0], ellipse.center[1]),
                ellipse,
                color,
                device,
                polygon,
            })
        })
        .collect();

    Scene {
        points,
        size,
        plot,
        proj,
        rings: rings(&proj, ring_step),
        lateral_ticks,
        dots,
        roll,
        track,
        ovals,
        members: members.len(),
    }
}

/// What the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hover {
    /// A bright or gold shot, by index into the shot list.
    Shot(usize),
    /// An ellipse, by index into the scene's ovals.
    Oval(usize),
}

/// What is under `pointer` (panel-local): the nearest bright or gold dot
/// within [`DOT_HIT`], else the topmost ellipse around it. Faint shots are
/// not hit.
fn hit(scene: &Scene, pointer: Pos2) -> Option<Hover> {
    // Topmost first, so the gold dot wins a tie.
    let dots: Vec<Candidate<'_, usize>> = scene
        .dots
        .iter()
        .rev()
        .map(|d| Candidate {
            key: d.index,
            points: std::slice::from_ref(&d.at),
            hittable: d.level != Level::Faint,
        })
        .collect();
    if let Some(i) = chart::nearest_point(pointer, DOT_HIT, &dots) {
        return Some(Hover::Shot(i));
    }
    scene
        .ovals
        .iter()
        .rposition(|o| chart::inside_polygon(&o.polygon, pointer))
        .map(Hover::Oval)
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// The gold shot's track flown so far during the draw-on, panel-local.
struct DrawOn {
    track: Vec<Pos2>,
    /// Whether the ball has reached the plotted end (landing or rest).
    reached: bool,
}

impl DrawOn {
    /// The scene's gold track flown up to `progress` (0..1) of its whole
    /// time: its cached curves cut by time. `None` without a gold path.
    fn new(scene: &Scene, progress: f32) -> Option<Self> {
        let (flight, ground) = scene.track.as_ref()?;
        let landing = flight.end_time()?;
        let t = f64::from(progress) * ground.end_time().unwrap_or(landing);
        let mut track = flight.prefix(t);
        if t >= landing {
            track.extend(ground.prefix(t).into_iter().skip(1));
        }
        let goal = match scene.points {
            Points::Carry => Some(landing),
            Points::Total => ground.end_time(),
        };
        Some(Self {
            track,
            reached: goal.is_some_and(|g| t >= g),
        })
    }
}

fn moved(points: &[Pos2], by: Vec2) -> Vec<Pos2> {
    points.iter().map(|p| *p + by).collect()
}

/// Paint `scene` with its origin at `origin`: rings, target line and
/// lateral ticks, faint dots, ellipses, bright dots, mean crosses, then
/// the gold shot and its roll. `hover` brightens what
/// the pointer is over; `draw_on` replaces the gold shot while it flies.
fn paint(
    painter: &egui::Painter,
    scene: &Scene,
    origin: Vec2,
    hover: Option<Hover>,
    draw_on: Option<&DrawOn>,
) {
    let plot = scene.plot.translate(origin);
    let label_font = egui::FontId::proportional(10.0);
    let clip = painter.with_clip_rect(plot);

    for ring in &scene.rings {
        clip.line(
            moved(&ring.arc, origin),
            Stroke::new(1.0, theme::BORDER.gamma_multiply(0.8)),
        );
        if let Some((y, text)) = &ring.label {
            painter.text(
                pos2(plot.left() - 5.0, origin.y + y),
                egui::Align2::RIGHT_CENTER,
                text,
                label_font.clone(),
                theme::TEXT_DIM,
            );
        }
    }
    let center_x = origin.x + scene.proj.x.map(0.0);
    clip.extend(egui::Shape::dashed_line(
        &[pos2(center_x, plot.bottom()), pos2(center_x, plot.top())],
        Stroke::new(1.0, theme::BORDER),
        3.0,
        3.0,
    ));
    painter.hline(
        plot.x_range(),
        plot.bottom(),
        Stroke::new(1.0, theme::BORDER),
    );
    let tee = pos2(center_x, plot.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tee + vec2(-4.0, 0.0),
            tee + vec2(4.0, 0.0),
            tee + vec2(0.0, -6.0),
        ],
        theme::TEXT_MUTED,
        Stroke::NONE,
    ));
    let label_y = plot.bottom() + 3.0;
    for (x, text) in &scene.lateral_ticks {
        let x = origin.x + x;
        painter.vline(
            x,
            plot.bottom()..=plot.bottom() + 3.0,
            Stroke::new(1.0, theme::BORDER),
        );
        painter.text(
            pos2(x, label_y),
            egui::Align2::CENTER_TOP,
            text,
            label_font.clone(),
            theme::TEXT_DIM,
        );
    }
    for (x, align, text) in [
        (plot.left(), egui::Align2::LEFT_TOP, "L"),
        (plot.right(), egui::Align2::RIGHT_TOP, "R"),
    ] {
        painter.text(
            pos2(x, label_y),
            align,
            text,
            label_font.clone(),
            theme::TEXT_MUTED,
        );
    }

    for d in scene.dots.iter().filter(|d| d.level == Level::Faint) {
        clip.circle_filled(d.at + origin, FAINT_R, d.color.gamma_multiply(FAINT_ALPHA));
    }
    for (n, o) in scene.ovals.iter().enumerate() {
        let polygon = moved(&o.polygon, origin);
        clip.add(egui::Shape::convex_polygon(
            polygon.clone(),
            o.color.gamma_multiply(FILL_ALPHA),
            Stroke::NONE,
        ));
        let outline = if hover == Some(Hover::Oval(n)) {
            Stroke::new(1.5, o.color)
        } else {
            Stroke::new(1.0, o.color.gamma_multiply(OUTLINE_ALPHA))
        };
        clip.add(egui::Shape::closed_line(polygon, outline));
    }
    for d in scene.dots.iter().filter(|d| d.level == Level::Bright) {
        let (c, r) = if hover == Some(Hover::Shot(d.index)) {
            (theme::mix(d.color, theme::TEXT, 0.35), BRIGHT_R + 1.0)
        } else {
            (d.color, BRIGHT_R)
        };
        clip.circle_filled(d.at + origin, r, c);
    }
    // Mean crosses: both halos first, so neither arm cuts the other.
    let arms = [
        (vec2(-CROSS_HALF, 0.0), vec2(CROSS_HALF, 0.0)),
        (vec2(0.0, -CROSS_HALF), vec2(0.0, CROSS_HALF)),
    ];
    for (stroke, reach) in [
        (Stroke::new(4.0, theme::BG), 1.2),
        (Stroke::new(2.0, theme::GOLD), 1.0),
    ] {
        for o in &scene.ovals {
            let c = o.center + origin;
            for (a, b) in arms {
                clip.line_segment([c + a * reach, c + b * reach], stroke);
            }
        }
    }

    // The gold shot on top, over a halo: its flight while it draws on, then
    // its roll and the plotted end.
    if let Some(d) = draw_on {
        let track = moved(&d.track, origin);
        if track.len() >= 2 {
            clip.line(track.clone(), Stroke::new(3.5, theme::BG));
            clip.line(track.clone(), Stroke::new(1.5, theme::GOLD));
        }
        if let Some(head) = track.last() {
            clip.circle_filled(*head, 3.0, theme::GOLD);
        }
    } else if scene.roll.len() >= 2 {
        let roll = moved(&scene.roll, origin);
        clip.line(roll.clone(), Stroke::new(4.0, theme::BG));
        clip.line(roll, Stroke::new(2.0, theme::GOLD));
    }
    if let Some(g) = scene.dots.iter().find(|d| d.level == Level::Gold)
        && draw_on.is_none_or(|d| d.reached)
    {
        clip.circle_filled(g.at + origin, GOLD_R + 1.5, theme::BG);
        clip.circle_filled(g.at + origin, GOLD_R, theme::GOLD);
    }
}

/// Hover text for `shot`: number, device (with two or more devices) and
/// club, then carry, total and the plotted lateral.
fn tooltip(
    shot: &ShotRow,
    device: Option<&str>,
    units: UnitSystem,
    lateral: Option<f64>,
) -> String {
    let (dist, _) = flight::unit_labels(units);
    let r = flight::readings(shot, units);
    let mut parts = vec![format!("#{}", shot.shot_number)];
    parts.extend(device.map(str::to_owned));
    parts.extend(shot.club_selected.map(|c| c.to_string()));
    let value = |name: &str, v: Option<f64>| v.map(|v| format!("{name} {v:.0} {dist}"));
    let values: Vec<String> = [
        value("carry", r.carry),
        value("total", r.total),
        lateral.map(|v| format!("offline {v:+.1} {dist}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut text = parts.join(" \u{b7} ");
    if !values.is_empty() {
        text.push('\n');
        text.push_str(&values.join(" \u{b7} "));
    }
    text
}

/// Hover text for an ellipse: who and how many, the mean, and the size.
fn oval_tooltip(
    o: &Oval,
    device: Option<&str>,
    units: UnitSystem,
    points: Points,
    size: EllipseSize,
) -> String {
    let (dist, _) = flight::unit_labels(units);
    let e = &o.ellipse;
    let mut who = vec![format!("{} shots", e.n)];
    if let Some(d) = device {
        who.insert(0, d.to_owned());
    }
    let end = match points {
        Points::Total => "rest",
        Points::Carry => "landing",
    };
    format!(
        "{}\nmean {end} {:.0} {dist} \u{b7} offline {:+.1} {dist}\n\
         {} ellipse, about {:.0} % of a normal pattern: {:.1} \u{d7} {:.1} {dist} semi-axes",
        who.join(" \u{b7} "),
        e.center[1],
        e.center[0],
        size.label(),
        chart::ellipse_coverage_pct(size.k()),
        e.semi_major,
        e.semi_minor,
    )
}

impl FlighthookApp {
    /// The shot the panel is built around: the shown shot, from the
    /// filtered device.
    pub(super) fn dispersion_shown(&self) -> Option<usize> {
        self.shown_index()
    }

    /// Key of the Dispersion scene for a plot area of `size` around the
    /// shown shot `shown`.
    fn dispersion_key(&self, shown: usize, size: Vec2) -> DispersionKey {
        DispersionKey {
            chart: ChartKey::new(self.members_key(shown), self.units(), size),
            points: self.dispersion_points,
            size: self.ellipse_size,
        }
    }

    /// Bring the cached Dispersion scene up to date for `key`: rebuilt
    /// only when the key changed, with the view's members from their own
    /// cache.
    fn refresh_dispersion_scene(&mut self, key: &DispersionKey, shown: usize) {
        if self.dispersion_scene.peek(key).is_some() {
            return;
        }
        let view = self.groups.mode;
        let members_key = self.refresh_members(shown);
        let members = self
            .view_members
            .peek(&members_key)
            .cloned()
            .unwrap_or_default();
        let devices = self.devices().into_owned();
        let scene = build_scene(&SceneInput {
            shots: &self.shots,
            groups: &self.groups,
            view,
            members: &members,
            shown,
            devices: &devices,
            filter: self.filter(),
            units: self.units(),
            points: key.points,
            size: key.size,
            area: vec2(key.chart.size[0] as f32, key.chart.size[1] as f32),
        });
        self.dispersion_scene.get(key.clone(), || scene);
    }

    /// The header in `rect` ([`header_height`] tall): `DISPERSION` and the
    /// unit, then the `Total | Carry` and `2σ | 95 %` switches (on a second
    /// row when the panel is narrow).
    fn dispersion_header(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let one_row = rect.width() >= ONE_ROW_MIN_WIDTH;
        let row = |i: usize| {
            Rect::from_min_size(
                rect.min + vec2(0.0, i as f32 * ROW_H),
                vec2(rect.width(), ROW_H),
            )
        };
        let in_row =
            |i: usize, layout: egui::Layout| egui::UiBuilder::new().max_rect(row(i)).layout(layout);
        let right_to_left = egui::Layout::right_to_left(egui::Align::Center);
        let (dist, _) = flight::unit_labels(self.units());
        let mut points = self.dispersion_points;
        let mut size = self.ellipse_size;
        let mut switches = |ui: &mut egui::Ui| {
            let sizes: Vec<&str> = EllipseSize::ALL.iter().map(|s| s.label()).collect();
            let at = EllipseSize::ALL
                .iter()
                .position(|s| *s == size)
                .unwrap_or(0);
            let picked = theme::segmented_sized(ui, &sizes, at, None, CONTROL_FONT);
            picked.response.on_hover_text(SIZE_HINT);
            if let Some(i) = picked.inner {
                size = EllipseSize::ALL[i];
            }
            let labels: Vec<&str> = Points::ALL.iter().map(|p| p.label()).collect();
            let at = Points::ALL.iter().position(|p| *p == points).unwrap_or(0);
            let picked = theme::segmented_sized(ui, &labels, at, None, CONTROL_FONT);
            picked.response.on_hover_text(POINTS_HINT);
            if let Some(i) = picked.inner {
                points = Points::ALL[i];
            }
        };
        let layout = egui::Layout::left_to_right(egui::Align::Center);
        ui.scope_builder(in_row(0, layout), |ui| {
            ui.label(theme::caps("Dispersion", 11.0, theme::TEXT_MUTED));
            ui.label(egui::RichText::new(dist).size(11.0).color(theme::TEXT_DIM));
            if one_row {
                ui.with_layout(right_to_left, |ui| switches(ui));
            }
        });
        if !one_row {
            ui.scope_builder(in_row(1, right_to_left), |ui| switches(ui));
        }
        self.dispersion_points = points;
        self.ellipse_size = size;
    }

    /// The Dispersion panel, filling `ui`; `table` when it sits beside the
    /// Table view, where its dots cannot be picked.
    pub(super) fn render_dispersion_panel(&mut self, ui: &mut egui::Ui, table: bool) {
        let full = ui.available_rect_before_wrap();
        let header = header_height(full.width());
        self.dispersion_header(ui, full.with_max_y(full.top() + header));
        let rect = full.with_min_y(full.top() + header);
        let response = ui.allocate_rect(rect, egui::Sense::click());
        let painter = ui.painter_at(rect);
        let units = self.units();
        let Some(shown) = self.dispersion_shown() else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Landing spots appear here once a shot lands",
                egui::FontId::proportional(13.0),
                theme::TEXT_DIM,
            );
            return;
        };
        let key = self.dispersion_key(shown, rect.size());
        self.refresh_dispersion_scene(&key, shown);
        let Some(scene) = self.dispersion_scene.peek(&key) else {
            return;
        };
        let origin = rect.min.to_vec2();

        // The latest shot flies out from the tee while its pulse runs.
        let now = ui.ctx().input(|i| i.time);
        let following = self.following();
        let progress = following.then(|| self.shot_focus.progress(now)).flatten();
        let draw_on = progress.and_then(|p| DrawOn::new(scene, p));
        if draw_on.is_some() {
            ui.ctx().request_repaint();
        }

        let hover = response.hover_pos().and_then(|p| hit(scene, p - origin));
        paint(&painter, scene, origin, hover, draw_on.as_ref());
        if !scene.dots.iter().any(|d| d.level != Level::Faint) {
            let note = if scene.members == 0 {
                "Nothing to plot for this shot"
            } else {
                "No lateral for this shot"
            };
            painter.text(
                scene.plot.translate(origin).center(),
                egui::Align2::CENTER_CENTER,
                note,
                egui::FontId::proportional(12.0),
                theme::TEXT_DIM,
            );
        }

        let device_name = |id: &str| chart::device_label(id, self.actor_name(id)).to_owned();
        let mut clicked = None;
        match hover {
            Some(Hover::Shot(i)) => {
                // An Auto-chip is always named: it is never one of the devices.
                let actor = &self.shots[i].actor;
                let several = self.devices().len() >= 2 || chart::is_auto_chip(actor);
                let device = several.then(|| device_name(actor));
                let lateral = scene.dots.iter().find(|d| d.index == i).map(|d| d.lateral);
                let text = tooltip(&self.shots[i], device.as_deref(), units, lateral);
                let response = if table {
                    response.on_hover_text_at_pointer(text)
                } else {
                    response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text_at_pointer(text)
                };
                clicked = (!table && response.clicked()).then_some(i);
            }
            Some(Hover::Oval(n)) => {
                let o = &scene.ovals[n];
                let device = o.device.as_deref().map(device_name);
                let text = oval_tooltip(o, device.as_deref(), units, scene.points, scene.size);
                response.on_hover_text_at_pointer(text);
            }
            None => {}
        }
        if let Some(i) = clicked {
            self.select_shot(i);
            self.shot_focus.reveal();
        }
    }
}

#[cfg(test)]
impl FlighthookApp {
    /// The last Dispersion plot drawn (panel-local) and the height of the
    /// allocation it sits in.
    pub(super) fn dispersion_plot(&self) -> Option<(Rect, f32)> {
        let scene = self.dispersion_scene.value()?;
        Some((scene.plot, scene.plot.bottom() + BOTTOM_GUTTER))
    }
}

#[cfg(test)]
mod tests;
