//! The Dispersion panel: a top-down view of where the shots came to rest
//! (Total) or landed (Carry), right of or under the Cards or Table view
//! (`shots::chart_slots`).
//!
//! Equal scales on both axes, the tee at the bottom centre, downrange up
//! and right to the right, with distance rings and a target line.
//!
//! - **Gold**: the shown shot, with its roll drawn from the landing to the
//!   rest. The latest shot flies out from the tee during the new-shot pulse.
//! - **Bright**: the view's other members (`chart::members_for`), coloured
//!   per device when every device is shown and there are two or more.
//! - **Faint**: the rest of the current session, newest
//!   [`flight::FAINT_CAP`], for context. The view `none` draws the shown
//!   shot alone.
//!
//! One covariance ellipse (2σ or 95 %) over the bright points, or one per
//! device in its colour, with a gold cross at each mean. A point is the
//! path's landing or rest when the shot has a simulated path, else its
//! carry or total with the simulated offline; shots with neither are left
//! out. The projected geometry is cached and rebuilt only when the members,
//! units, shown shot, device filter, panel size or the panel's two switches
//! change.

use egui::{Color32, Pos2, Rect, Stroke, Vec2, pos2, vec2};

use super::flight;
use super::shots::{bar_controls, bar_group, device_options, latest_index, pick};
use crate::app::FlighthookApp;
use crate::chart::{self, Candidate, ChartKey, Curve, Ellipse, Projection};
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
    let k = flight::per_metre(units).0;
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
    let ball = shot.ball.as_ref();
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
            ball.and_then(|b| b.carry_distance),
            ball.and_then(|b| b.carry_offline),
        ),
        (
            derived.and_then(|d| d.carry_distance),
            derived.and_then(|d| d.carry_offline),
        ),
    );
    let rest = spot(
        (
            ball.and_then(|b| b.total_distance),
            ball.and_then(|b| b.total_offline),
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
    colour: Color32,
    at: Pos2,
    /// The plotted lateral, in display units.
    lateral: f64,
}

/// A dispersion ellipse and where it is drawn.
#[derive(Debug)]
struct Oval {
    ellipse: Ellipse,
    colour: Color32,
    /// The device it covers, when there is one per device.
    device: Option<String>,
    polygon: Vec<Pos2>,
    /// The mean, on screen.
    centre: Pos2,
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
    /// The device filter.
    device: Option<&'a str>,
    units: UnitSystem,
    points: Points,
    size: EllipseSize,
    /// Size of the plot area's allocation.
    area: Vec2,
}

/// Height of the header of a panel `width` wide: the title row, the
/// switches' own row when they don't fit beside the title, and the device
/// filter's row when `filter` (see `FlighthookApp::dispersion_filter`).
#[must_use]
pub(super) fn header_height(width: f32, filter: bool) -> f32 {
    let rows = 1 + usize::from(width < ONE_ROW_MIN_WIDTH) + usize::from(filter);
    rows as f32 * ROW_H
}

/// The plot inside an allocation of `area`: the whole of it, or in a wide
/// area a plot [`MAX_ASPECT`] times as wide as it is tall, centred, so the
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
/// about the tee clipped to the lateral domain `x_hi`, labelled where they
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
        device,
        units,
        points,
        size,
        area,
    } = *input;
    let drawn = flight::drawn_sets(shots, groups, view, members, shown, device, |s| {
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

    let by_device = device.is_none() && devices.len() >= 2;
    let colour_of = |i: usize| {
        if by_device {
            shots
                .get(i)
                .map_or(theme::ACCENT, |s| chart::device_colour(devices, &s.actor))
        } else {
            theme::ACCENT
        }
    };
    let to_screen = |s: Spot| proj.to_screen(s.lateral, s.down);
    let dot = |level: Level| {
        move |&(index, s): &(usize, Spot)| Dot {
            index,
            level,
            colour: colour_of(index),
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
    let k = flight::per_metre(units).0;
    let track = gold
        .and_then(|(i, _)| shots.get(i))
        .and_then(flight::path)
        .map(|p| chart::smooth_path(p, |q| proj.to_screen(q[3] * k, q[1] * k)));

    // One ellipse over the bright points, or one per device.
    let lit: Vec<(usize, Spot)> = bright.iter().copied().chain(gold).collect();
    let clusters: Vec<(Option<String>, Color32, Vec<[f64; 2]>)> = if by_device {
        devices
            .iter()
            .map(|d| {
                let pts = lit
                    .iter()
                    .filter(|(i, _)| shots.get(*i).is_some_and(|s| &s.actor == d))
                    .map(|(_, s)| [s.lateral, s.down])
                    .collect();
                (Some(d.clone()), chart::device_colour(devices, d), pts)
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
        .filter_map(|(device, colour, pts)| {
            let ellipse = chart::ellipse_from_points(&pts, size.k())?;
            let polygon = chart::ellipse_points(&ellipse, chart::ELLIPSE_SEGMENTS)
                .into_iter()
                .map(|p| proj.to_screen(p[0], p[1]))
                .collect();
            Some(Oval {
                centre: proj.to_screen(ellipse.center[0], ellipse.center[1]),
                ellipse,
                colour,
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
    let centre_x = origin.x + scene.proj.x.map(0.0);
    clip.extend(egui::Shape::dashed_line(
        &[pos2(centre_x, plot.bottom()), pos2(centre_x, plot.top())],
        Stroke::new(1.0, theme::BORDER),
        3.0,
        3.0,
    ));
    painter.hline(
        plot.x_range(),
        plot.bottom(),
        Stroke::new(1.0, theme::BORDER),
    );
    let tee = pos2(centre_x, plot.bottom());
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
        clip.circle_filled(d.at + origin, FAINT_R, d.colour.gamma_multiply(FAINT_ALPHA));
    }
    for (n, o) in scene.ovals.iter().enumerate() {
        let polygon = moved(&o.polygon, origin);
        clip.add(egui::Shape::convex_polygon(
            polygon.clone(),
            o.colour.gamma_multiply(FILL_ALPHA),
            Stroke::NONE,
        ));
        let outline = if hover == Some(Hover::Oval(n)) {
            Stroke::new(1.5, o.colour)
        } else {
            Stroke::new(1.0, o.colour.gamma_multiply(OUTLINE_ALPHA))
        };
        clip.add(egui::Shape::closed_line(polygon, outline));
    }
    for d in scene.dots.iter().filter(|d| d.level == Level::Bright) {
        let (c, r) = if hover == Some(Hover::Shot(d.index)) {
            (theme::mix(d.colour, theme::TEXT, 0.35), BRIGHT_R + 1.0)
        } else {
            (d.colour, BRIGHT_R)
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
            let c = o.centre + origin;
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
    /// The shot the panel is built around: the shown shot in Cards, the
    /// latest one in Table (`table`), whose rows cannot be picked.
    pub(super) fn dispersion_shown(&self, table: bool) -> Option<usize> {
        if table {
            latest_index(&self.shots)
        } else {
            self.shot_focus.index(&self.shots)
        }
    }

    /// Key of the Dispersion scene for a plot area of `size` around the
    /// shown shot `shown`.
    fn dispersion_key(&self, shown: usize, size: Vec2) -> DispersionKey {
        DispersionKey {
            chart: ChartKey::new(self.members_key(shown), self.units_toggle, size),
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
        let set = bar_group(&self.groups, self.shots.get(shown), self.selected_club);
        let members = self
            .view_members
            .get(key.chart.members.clone(), || {
                chart::members_for(
                    &self.shots,
                    &self.groups,
                    view,
                    set,
                    Some(shown),
                    self.device_filter.as_deref(),
                )
            })
            .clone();
        let devices = self.devices().into_owned();
        let scene = build_scene(&SceneInput {
            shots: &self.shots,
            groups: &self.groups,
            view,
            members: &members,
            shown,
            devices: &devices,
            device: self.device_filter.as_deref(),
            units: self.units_toggle,
            points: key.points,
            size: key.size,
            area: vec2(key.chart.size[0] as f32, key.chart.size[1] as f32),
        });
        self.dispersion_scene.get(key.clone(), || scene);
    }

    /// The device filter's options when the header carries it: in Table
    /// view, which has no other place for it, with a set or club view and
    /// two or more devices.
    pub(super) fn dispersion_filter(&self, table: bool) -> Option<Vec<String>> {
        (table && self.groups.mode != ShotGrouping::None)
            .then(|| device_options(&self.devices(), &self.actors))
            .flatten()
    }

    /// The header in `rect` ([`header_height`] tall): `DISPERSION` and the
    /// unit, the `Total | Carry` and `2σ | 95 %` switches (on a second row
    /// when the panel is narrow), and the device filter over `filter`.
    fn dispersion_header(&mut self, ui: &mut egui::Ui, rect: Rect, filter: Option<&[String]>) {
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
        let (dist, _) = flight::unit_labels(self.units_toggle);
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

        if let Some(labels) = filter {
            let devices = self.devices().into_owned();
            let at = self
                .device_filter
                .as_ref()
                .and_then(|d| devices.iter().position(|id| id == d))
                .map_or(0, |p| p + 1);
            let last = 1 + usize::from(!one_row);
            let picked = ui
                .scope_builder(in_row(last, right_to_left), |ui| {
                    bar_controls(ui, labels, at)
                })
                .inner;
            if let Some(d) = picked {
                self.device_filter = d.checked_sub(1).and_then(|p| devices.get(p).cloned());
            }
        }
    }

    /// The Dispersion panel, filling `ui`; `table` when it sits beside the
    /// Table view.
    pub(super) fn render_dispersion_panel(&mut self, ui: &mut egui::Ui, table: bool) {
        let full = ui.available_rect_before_wrap();
        let filter = self.dispersion_filter(table);
        let header = header_height(full.width(), filter.is_some());
        self.dispersion_header(ui, full.with_max_y(full.top() + header), filter.as_deref());
        let rect = full.with_min_y(full.top() + header);
        let response = ui.allocate_rect(rect, egui::Sense::click());
        let painter = ui.painter_at(rect);
        let units = self.units_toggle;
        let Some(shown) = self.dispersion_shown(table) else {
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
        let following = table || self.shot_focus.following(&self.shots);
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

        let device_name = |id: &str| {
            let name = self.actors.get(id).map(|a| a.name.as_str());
            chart::device_label(id, name).to_owned()
        };
        let mut clicked = None;
        match hover {
            Some(Hover::Shot(i)) => {
                let several = self.devices().len() >= 2;
                let device = several.then(|| device_name(&self.shots[i].actor));
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
            self.shot_focus.select(&self.shots, i);
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
mod tests {
    use super::*;
    use crate::types::{BallFlight, Club, DerivedFlight, FlightPath, ShotGroupInfo};

    fn set(id: &str, number: u32, club: Club, session: u32) -> ShotGroupInfo {
        ShotGroupInfo {
            id: id.into(),
            number,
            session,
            golfer: None,
            club: Some(club),
            started_at: "2026-10-26T15:41:28.300Z".into(),
            manual: false,
            open: false,
            shots: 2,
        }
    }

    fn groups(list: Vec<ShotGroupInfo>) -> ShotGroups {
        let session = list.iter().map(|g| g.session).max().unwrap_or(0);
        let mut g = ShotGroups::default();
        g.replace(crate::types::ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::BySet,
            session,
            current: None,
            groups: list,
        });
        g
    }

    /// A path landing `carry` metres downrange and `lateral` metres right,
    /// rolling straight on to `total` metres downrange (no ground segment
    /// when `total` is not past `carry`).
    fn flight_path(carry: f64, lateral: f64, total: f64) -> FlightPath {
        FlightPath {
            flight: vec![
                [0.0, 0.0, 0.0, 0.0],
                [2.0, carry * 0.5, 25.0, lateral * 0.3],
                [5.0, carry, 0.0, lateral],
            ],
            ground: if total > carry {
                vec![
                    [5.0, carry, 0.0, lateral],
                    [5.5, (carry + total) / 2.0, 0.4, lateral * 1.1],
                    [6.0, total, 0.0, lateral * 1.2],
                ]
            } else {
                Vec::new()
            },
        }
    }

    /// A shot from `actor` in set `group` with a simulated path.
    fn shot(n: u32, actor: &str, group: &str, carry: f64, lateral: f64) -> ShotRow {
        let mut d = DerivedFlight::default();
        d.carry_distance = Some(Distance::Meters(carry));
        d.total_distance = Some(Distance::Meters(carry + 10.0));
        d.carry_offline = Some(Distance::Meters(lateral));
        d.total_offline = Some(Distance::Meters(lateral * 1.2));
        d.path = Some(flight_path(carry, lateral, carry + 10.0));
        ShotRow {
            actor: actor.into(),
            shot_id: format!("id{n}"),
            shot_number: n,
            ball: Some(BallFlight::default()),
            club: None,
            derived: Some(d),
            club_selected: Some(Club::Iron7),
            group: Some(group.into()),
        }
    }

    /// A shot with a measured carry and nothing else: no lateral.
    fn no_lateral(n: u32, group: &str) -> ShotRow {
        ShotRow {
            ball: Some(BallFlight {
                carry_distance: Some(Distance::Meters(140.0)),
                ..Default::default()
            }),
            derived: None,
            ..shot(n, "mevo.0", group, 140.0, 0.0)
        }
    }

    /// Session 0: set `old` (2 shots). Session 1: set `a` (3, two
    /// devices), set `b` (3, two devices).
    fn history() -> (Vec<ShotRow>, ShotGroups) {
        let shots = vec![
            shot(1, "mevo.0", "old", 140.0, 0.0),
            shot(2, "mevo.0", "old", 141.0, 1.0),
            shot(3, "mevo.0", "a", 200.0, -8.0),
            shot(4, "square.0", "a", 205.0, 6.0),
            shot(5, "mevo.0", "a", 210.0, -2.0),
            shot(6, "mevo.0", "b", 140.0, -4.0),
            shot(7, "square.0", "b", 145.0, 5.0),
            shot(8, "mevo.0", "b", 150.0, 2.0),
        ];
        let g = groups(vec![
            set("old", 1, Club::Iron7, 0),
            set("a", 2, Club::Driver, 1),
            set("b", 3, Club::Iron7, 1),
        ]);
        (shots, g)
    }

    fn members(
        shots: &[ShotRow],
        g: &ShotGroups,
        view: ShotGrouping,
        shown: usize,
        device: Option<&str>,
    ) -> Vec<usize> {
        let set = shots[shown].group.as_deref().and_then(|id| g.get(id));
        chart::members_for(shots, g, view, set, Some(shown), device)
    }

    fn input<'a>(
        shots: &'a [ShotRow],
        g: &'a ShotGroups,
        members: &'a [usize],
        shown: usize,
        devices: &'a [String],
    ) -> SceneInput<'a> {
        SceneInput {
            shots,
            groups: g,
            view: ShotGrouping::BySet,
            members,
            shown,
            devices,
            device: None,
            units: UnitSystem::Metric,
            points: Points::Total,
            size: EllipseSize::TwoSigma,
            area: vec2(306.0, 700.0),
        }
    }

    #[test]
    fn an_absurd_shot_neither_hangs_nor_resizes_the_chart() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let sane = build_scene(&input(&shots, &g, &m, 7, &[])).proj;
        for v in [1e38, 1e300, f64::MAX, -1e300, -5.0] {
            for points in [Points::Total, Points::Carry] {
                let mut shots = shots.clone();
                // Simulated ends far off the chart, as a malformed
                // measured carry could produce.
                let d = shots[6].derived.as_mut().unwrap();
                d.carry_distance = Some(Distance::Meters(v));
                d.total_distance = Some(Distance::Meters(v));
                d.carry_offline = Some(Distance::Meters(v));
                d.total_offline = Some(Distance::Meters(v));
                d.path = Some(flight_path(v, v, v));
                let scene = build_scene(&SceneInput {
                    points,
                    ..input(&shots, &g, &m, 7, &[])
                });
                assert_eq!(scene.proj, sane, "{v}");
                assert!(scene.rings.len() < 100 && scene.lateral_ticks.len() < 100);
                let screen = scene
                    .dots
                    .iter()
                    .map(|d| &d.at)
                    .chain(&scene.roll)
                    .chain(
                        scene
                            .ovals
                            .iter()
                            .flat_map(|o| o.polygon.iter().chain([&o.centre])),
                    )
                    .chain(scene.rings.iter().flat_map(|r| &r.arc));
                for p in screen {
                    assert!(p.x.is_finite() && p.y.is_finite(), "{v}: {p:?}");
                }
            }
        }
    }

    fn levels(scene: &Scene, level: Level) -> Vec<usize> {
        scene
            .dots
            .iter()
            .filter(|d| d.level == level)
            .map(|d| d.index)
            .collect()
    }

    /// The shot drawn in gold.
    fn gold(scene: &Scene) -> Option<usize> {
        levels(scene, Level::Gold).first().copied()
    }

    fn dot(scene: &Scene, index: usize) -> &Dot {
        scene.dots.iter().find(|d| d.index == index).unwrap()
    }

    #[test]
    fn members_are_bright_and_the_session_faint() {
        let (mut shots, g) = history();
        // A faint shot without a lateral is not drawn.
        shots[3] = no_lateral(4, "a");
        let scene_for = |view: ShotGrouping, shown: usize, device: Option<&str>| {
            let m = members(&shots, &g, view, shown, device);
            build_scene(&SceneInput {
                view,
                device,
                ..input(&shots, &g, &m, shown, &[])
            })
        };
        // By set, shown #8 in set b: the set bright, set a faint, the
        // earlier session's set left out.
        let s = scene_for(ShotGrouping::BySet, 7, None);
        assert_eq!(gold(&s), Some(7));
        assert_eq!(levels(&s, Level::Gold), vec![7]);
        assert_eq!(levels(&s, Level::Bright), vec![5, 6]);
        assert_eq!(levels(&s, Level::Faint), vec![2, 4]);
        // The device filter applies first, to the faint shots too; a
        // filtered-out shown shot is not gold.
        let s = scene_for(ShotGrouping::BySet, 6, Some("mevo.0"));
        assert_eq!(gold(&s), None);
        assert_eq!(levels(&s, Level::Bright), vec![5, 7]);
        assert_eq!(levels(&s, Level::Faint), vec![2, 4]);
        // By club: the club's sets in the session (one 7-iron set here).
        let s = scene_for(ShotGrouping::ByClub, 4, None);
        assert_eq!(levels(&s, Level::Gold), vec![4]);
        assert_eq!(levels(&s, Level::Bright), vec![2]);
        assert_eq!(levels(&s, Level::Faint), vec![5, 6, 7]);
        // None: the shown shot alone.
        let s = scene_for(ShotGrouping::None, 7, None);
        assert_eq!(levels(&s, Level::Gold), vec![7]);
        assert_eq!(s.dots.len(), 1);
        assert!(s.ovals.is_empty());
    }

    #[test]
    fn faint_shots_are_capped_to_the_newest() {
        let g = groups(vec![
            set("a", 1, Club::Driver, 0),
            set("b", 2, Club::Iron7, 0),
        ]);
        let mut shots: Vec<ShotRow> = (0..250)
            .map(|n| shot(n, "mevo.0", "a", 200.0, 0.0))
            .collect();
        shots.push(shot(250, "mevo.0", "b", 150.0, 0.0));
        let m = members(&shots, &g, ShotGrouping::BySet, 250, None);
        let s = build_scene(&input(&shots, &g, &m, 250, &[]));
        let faint = levels(&s, Level::Faint);
        assert_eq!(faint.len(), flight::FAINT_CAP);
        assert_eq!(faint.first(), Some(&50));
        assert_eq!(faint.last(), Some(&249));
    }

    #[test]
    fn points_come_from_the_path_or_the_scalars() {
        // A path: its last flight and ground points, lateral = z.
        let s = shot(1, "mevo.0", "b", 150.0, 3.0);
        let e = ends(&s, UnitSystem::Metric);
        assert_eq!(
            e.at(Points::Carry),
            Some(Spot {
                lateral: 3.0,
                down: 150.0
            })
        );
        assert_eq!(
            e.at(Points::Total),
            Some(Spot {
                lateral: 3.0 * 1.2,
                down: 160.0
            })
        );
        assert_eq!(e.roll.len(), 3);
        let yd = ends(&s, UnitSystem::Imperial).at(Points::Carry).unwrap();
        assert!((yd.down - 150.0 / 0.9144).abs() < 1e-9);

        // No path: carry/total are distances from the tee, the simulated
        // offline is the lateral.
        let mut scalars = s.clone();
        scalars.derived.as_mut().unwrap().path = None;
        let e = ends(&scalars, UnitSystem::Metric);
        let landing = e.at(Points::Carry).unwrap();
        assert_eq!(landing.lateral, 3.0);
        assert!((landing.down.hypot(landing.lateral) - 150.0).abs() < 1e-9);
        let rest = e.at(Points::Total).unwrap();
        assert!((rest.down.hypot(rest.lateral) - 160.0).abs() < 1e-9);
        assert_eq!(e.roll, vec![landing, rest]);

        // A carry in both: the simulated one wins, with or without force.
        // Measured only: the measured one.
        scalars.ball = Some(BallFlight {
            carry_distance: Some(Distance::Meters(140.0)),
            ..Default::default()
        });
        let down = |s: &ShotRow| {
            let p = ends(s, UnitSystem::Metric).at(Points::Carry).unwrap();
            p.down.hypot(p.lateral)
        };
        assert!((down(&scalars) - 150.0).abs() < 1e-9);
        scalars.derived.as_mut().unwrap().replaces_measured = true;
        assert!((down(&scalars) - 150.0).abs() < 1e-9);
        scalars.derived.as_mut().unwrap().carry_distance = None;
        assert!((down(&scalars) - 140.0).abs() < 1e-9);

        // No lateral at all: nothing to plot in either mode.
        let none = no_lateral(2, "b");
        assert!(!plottable(&none, Points::Carry));
        assert!(!plottable(&none, Points::Total));
        // A path that did not reach rest has a landing only.
        let mut flying = shot(3, "mevo.0", "b", 150.0, 3.0);
        flying.derived.as_mut().unwrap().path = Some(flight_path(150.0, 3.0, 0.0));
        assert!(plottable(&flying, Points::Carry));
        assert!(!plottable(&flying, Points::Total));
    }

    #[test]
    fn a_wide_area_centres_a_plot_of_bounded_aspect() {
        // A tall column: the whole area.
        let tall = plot_rect(vec2(306.0, 700.0));
        assert_eq!(tall.left(), LEFT_GUTTER);
        assert_eq!(tall.right(), 306.0 - RIGHT_PAD);
        // A wide strip under a narrow Table: centred, 1.25 times as wide as
        // tall, so the lateral scale stays near the downrange one.
        let area = vec2(1000.0, 200.0);
        let wide = plot_rect(area);
        assert!((wide.width() - wide.height() * MAX_ASPECT).abs() < 1.0);
        assert!((wide.center().x - (LEFT_GUTTER + area.x - RIGHT_PAD) / 2.0).abs() <= 1.0);
        assert_eq!(wide.bottom(), area.y - BOTTOM_GUTTER);
    }

    #[test]
    fn unplotted_members_leave_no_note() {
        let (mut shots, g) = history();
        shots[5] = no_lateral(6, "b");
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let s = build_scene(&input(&shots, &g, &m, 7, &[]));
        assert_eq!(s.members, 3);
        // The plot runs down to the lateral labels.
        assert_eq!(s.plot.bottom(), 700.0 - BOTTOM_GUTTER);
        // Two dots: the member without a lateral is left out.
        assert_eq!(s.dots.iter().filter(|d| d.level != Level::Faint).count(), 2);
    }

    #[test]
    fn overhead_axes_are_equal_with_the_tee_at_the_bottom_centre() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let s = build_scene(&input(&shots, &g, &m, 7, &[]));
        // One unit across is one unit up.
        assert!((s.proj.x.px_per_unit() - s.proj.y.px_per_unit()).abs() < 1e-4);
        // Tee at the bottom centre; downrange up, right to the right.
        assert_eq!(s.proj.to_screen(0.0, 0.0), s.plot.center_bottom());
        let right = dot(&s, 6).at; // 5 m right, 145 m carry, 6 m right at rest
        let left = dot(&s, 5).at; // 4 m left
        assert!(right.x > s.plot.center().x && left.x < s.plot.center().x);
        let gold = dot(&s, 7).at; // 160 m total, 2.4 m right
        assert!(gold.y < s.plot.bottom() && gold.y > s.plot.top());
        // The bright rests fit: 160 m × 1.08 → 175 m at least.
        assert!(s.proj.y.domain().1 >= 175.0);
        assert!(s.proj.x.domain().1 >= 15.0);
        // Rings every 25 m (thinned by pitch), labelled at the left edge.
        assert!(s.rings.len() >= 4);
        assert!(s.rings.iter().any(|r| r.label.is_some()));
        // Lateral ticks are symmetric about the target line.
        let xs: Vec<f32> = s.lateral_ticks.iter().map(|t| t.0).collect();
        let centre = s.plot.center().x;
        for x in &xs {
            assert!(xs.iter().any(|y| (centre - x - (y - centre)).abs() < 1e-3));
        }
    }

    #[test]
    fn ellipses_follow_the_devices() {
        let g = groups(vec![set("b", 1, Club::Iron7, 0)]);
        let shots: Vec<ShotRow> = (0..7)
            .map(|n| {
                let actor = if n % 2 == 0 { "mevo.0" } else { "square.0" };
                shot(n, actor, "b", 140.0 + f64::from(n), f64::from(n) - 3.0)
            })
            .collect();
        let devices = chart::device_ids(&shots);
        let m = members(&shots, &g, ShotGrouping::BySet, 6, None);
        // Every device shown, two devices: one ellipse each, in its colour.
        let s = build_scene(&input(&shots, &g, &m, 6, &devices));
        let ovals: Vec<(Option<&str>, Color32, usize)> = s
            .ovals
            .iter()
            .map(|o| (o.device.as_deref(), o.colour, o.ellipse.n))
            .collect();
        assert_eq!(
            ovals,
            vec![
                (Some("mevo.0"), theme::DEVICE_COLOURS[0], 4),
                (Some("square.0"), theme::DEVICE_COLOURS[1], 3),
            ]
        );
        assert_eq!(dot(&s, 1).colour, theme::DEVICE_COLOURS[1]);
        // One device's points only: one ellipse in the accent colour.
        let mevo = members(&shots, &g, ShotGrouping::BySet, 6, Some("mevo.0"));
        let s = build_scene(&SceneInput {
            device: Some("mevo.0"),
            ..input(&shots, &g, &mevo, 6, &devices)
        });
        assert_eq!(s.ovals.len(), 1);
        assert_eq!(
            (s.ovals[0].device.as_deref(), s.ovals[0].colour),
            (None, theme::ACCENT)
        );
        // Fewer than three points of a device: no ellipse for it.
        let few = members(&shots[..5], &g, ShotGrouping::BySet, 4, None);
        let s = build_scene(&input(&shots[..5], &g, &few, 4, &devices));
        assert_eq!(s.ovals.len(), 1);
        assert_eq!(s.ovals[0].device.as_deref(), Some("mevo.0"));
    }

    #[test]
    fn ellipse_size_follows_the_switch() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let two = build_scene(&input(&shots, &g, &m, 7, &[]));
        let ninety_five = build_scene(&SceneInput {
            size: EllipseSize::P95,
            ..input(&shots, &g, &m, 7, &[])
        });
        let (a, b) = (&two.ovals[0].ellipse, &ninety_five.ovals[0].ellipse);
        assert!((b.semi_major / a.semi_major - 2.448 / 2.0).abs() < 1e-9);
        assert_eq!(a.center, b.center);
        assert_eq!(EllipseSize::default(), EllipseSize::TwoSigma);
        assert_eq!(Points::default(), Points::Total);
        let text = oval_tooltip(
            &two.ovals[0],
            None,
            UnitSystem::Metric,
            Points::Total,
            EllipseSize::TwoSigma,
        );
        assert!(text.starts_with("3 shots\nmean rest "), "{text}");
        assert!(
            text.contains("2\u{3c3} ellipse, about 86 % of a normal pattern"),
            "{text}"
        );
        let text = oval_tooltip(
            &ninety_five.ovals[0],
            Some("Mevo+"),
            UnitSystem::Metric,
            Points::Carry,
            EllipseSize::P95,
        );
        assert!(
            text.starts_with("Mevo+ \u{b7} 3 shots\nmean landing "),
            "{text}"
        );
        assert!(text.contains("95 % ellipse, about 95 %"), "{text}");
        assert!(text.ends_with(&format!(
            "{:.1} \u{d7} {:.1} m semi-axes",
            b.semi_major, b.semi_minor
        )));
    }

    #[test]
    fn gold_roll_runs_from_landing_to_rest() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        for points in Points::ALL {
            let s = build_scene(&SceneInput {
                points,
                ..input(&shots, &g, &m, 7, &[])
            });
            let landing = s.proj.to_screen(2.0, 150.0);
            let rest = s.proj.to_screen(2.4, 160.0);
            assert_eq!(s.roll.first(), Some(&landing));
            assert_eq!(s.roll.last(), Some(&rest));
            let expected = match points {
                Points::Total => rest,
                Points::Carry => landing,
            };
            assert_eq!(dot(&s, 7).at, expected);
        }
    }

    #[test]
    fn a_ground_ball_rolls_from_the_tee() {
        let (mut shots, g) = history();
        // Rolled 5 m to 0.4 m right: a one-point flight (the tee), then the
        // roll; carry 0.
        let d = shots[7].derived.as_mut().unwrap();
        d.carry_distance = Some(Distance::Meters(0.0));
        d.carry_offline = Some(Distance::Meters(0.0));
        d.total_distance = Some(Distance::Meters(5.0));
        d.total_offline = Some(Distance::Meters(0.4));
        d.path = Some(FlightPath {
            flight: vec![[0.0; 4]],
            ground: vec![[0.0; 4], [1.5, 3.0, 0.0, 0.24], [4.0, 5.0, 0.0, 0.4]],
        });
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let tee = |s: &Scene| s.proj.to_screen(0.0, 0.0);
        for points in Points::ALL {
            let s = build_scene(&SceneInput {
                points,
                ..input(&shots, &g, &m, 7, &[])
            });
            let rest = s.proj.to_screen(0.4, 5.0);
            // The roll runs along the path's ground points from the tee.
            assert_eq!(s.roll.len(), 3);
            assert_eq!(s.roll.first(), Some(&tee(&s)));
            assert_eq!(s.roll.last(), Some(&rest));
            let expected = match points {
                Points::Total => rest,
                Points::Carry => tee(&s),
            };
            assert_eq!(dot(&s, 7).at, expected);
        }
        // The draw-on starts at the tee and ends at the rest dot.
        let s = build_scene(&input(&shots, &g, &m, 7, &[]));
        let mid = DrawOn::new(&s, 0.5).unwrap();
        assert_eq!(mid.track.first(), Some(&tee(&s)));
        assert!(!mid.reached);
        let end = DrawOn::new(&s, 1.0).unwrap();
        assert!(end.reached);
        assert_eq!(end.track.last(), Some(&dot(&s, 7).at));
    }

    #[test]
    fn hit_test_picks_the_right_shot() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let s = build_scene(&input(&shots, &g, &m, 7, &[]));
        assert_eq!(
            hit(&s, dot(&s, 6).at + vec2(3.0, -2.0)),
            Some(Hover::Shot(6))
        );
        assert_eq!(hit(&s, dot(&s, 7).at), Some(Hover::Shot(7)));
        // Faint shots are never hit, even on their own dots.
        let faint = s.dots.iter().find(|d| d.level == Level::Faint).unwrap();
        assert_ne!(hit(&s, faint.at), Some(Hover::Shot(faint.index)));
        // Inside the ellipse, away from the dots.
        let o = &s.ovals[0];
        let inside = o
            .polygon
            .iter()
            .map(|p| o.centre + (*p - o.centre) * 0.9)
            .find(|p| s.dots.iter().all(|d| d.at.distance(*p) > DOT_HIT))
            .unwrap();
        assert_eq!(hit(&s, inside), Some(Hover::Oval(0)));
        // Under the plot, and an empty corner.
        let foot = pos2(s.plot.right() - 10.0, s.plot.bottom() + BOTTOM_GUTTER - 4.0);
        assert_eq!(hit(&s, foot), None);
        assert_eq!(hit(&s, s.plot.left_top() + vec2(2.0, 2.0)), None);
    }

    #[test]
    fn draw_on_flies_the_gold_shot_out() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let s = build_scene(&input(&shots, &g, &m, 7, &[]));
        let (flight, ground) = s.track.as_ref().unwrap();
        // The track is a smooth curve through the path's points.
        let p = flight::path(&shots[7]).unwrap();
        let k = flight::per_metre(UnitSystem::Metric).0;
        for q in p.flight.iter().chain(&p.ground) {
            let at = s.proj.to_screen(q[3] * k, q[1] * k);
            assert!(flight.points.contains(&at) || ground.points.contains(&at));
        }
        // 6 s in all: half way the ball is in the air.
        let mid = DrawOn::new(&s, 0.5).unwrap();
        assert!(mid.track.len() >= 3 && mid.track.len() <= flight.points.len());
        assert!(!mid.reached);
        assert_eq!(mid.track[0], s.proj.to_screen(0.0, 0.0));
        // At the end: the whole track, ending at the rest dot.
        let end = DrawOn::new(&s, 1.0).unwrap();
        assert!(end.reached);
        assert_eq!(
            end.track.len(),
            flight.points.len() + ground.points.len() - 1
        );
        assert_eq!(end.track.last(), Some(&dot(&s, 7).at));
        // In Carry mode the landing is reached first.
        let carry = build_scene(&SceneInput {
            points: Points::Carry,
            ..input(&shots, &g, &m, 7, &[])
        });
        assert!(DrawOn::new(&carry, 5.0 / 6.0 + 0.01).unwrap().reached);
        assert!(!DrawOn::new(&carry, 5.0 / 6.0 - 0.01).unwrap().reached);
    }

    #[test]
    fn tooltip_names_the_shot() {
        let (shots, _) = history();
        let text = tooltip(&shots[7], Some("Mevo+"), UnitSystem::Metric, Some(2.4));
        assert_eq!(
            text,
            "#8 \u{b7} Mevo+ \u{b7} 7I\ncarry 150 m \u{b7} total 160 m \u{b7} offline +2.4 m"
        );
    }

    #[test]
    fn table_view_builds_around_the_latest_shot() {
        let (shots, g) = history();
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.shots = shots;
        app.shots_rev = 1;
        app.groups = g;
        app.refresh_shot_caches();
        app.shot_focus.observe(&app.shots, 0.0);
        app.shot_focus.select(&app.shots, 3);
        assert_eq!(app.dispersion_shown(false), Some(3));
        assert_eq!(app.dispersion_shown(true), Some(7));
        // Table: the latest shot's set is bright, whatever the Cards pin.
        let key = app.dispersion_key(7, vec2(306.0, 700.0));
        app.refresh_dispersion_scene(&key, 7);
        let s = app.dispersion_scene.peek(&key).unwrap();
        assert_eq!(gold(s), Some(7));
        assert_eq!(levels(s, Level::Bright), vec![5, 6]);
    }

    #[test]
    fn clear_history_drops_the_scene() {
        let (shots, g) = history();
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.shots = shots;
        app.shots_rev = 1;
        app.groups = g;
        app.refresh_shot_caches();
        let key = app.dispersion_key(7, vec2(306.0, 700.0));
        app.refresh_dispersion_scene(&key, 7);
        assert!(app.dispersion_scene.peek(&key).is_some());
        app.clear_shot_history();
        assert!(app.dispersion_scene.peek(&key).is_none());
    }

    #[test]
    fn scene_rebuilds_only_when_its_key_changes() {
        let (shots, g) = history();
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.shots = shots;
        app.shots_rev = 1;
        app.groups = g;
        app.refresh_shot_caches();
        let size = vec2(306.0, 700.0);
        let mut key = app.dispersion_key(7, size);
        app.refresh_dispersion_scene(&key, 7);
        assert!(app.dispersion_scene.peek(&key).is_some());
        // An idle frame, sub-point jitter included, keeps the scene.
        assert_eq!(app.dispersion_key(7, size + vec2(0.3, -0.2)), key);

        // Each input invalidates it; the rebuilt scene follows the change.
        fn changed(
            app: &mut FlighthookApp,
            key: &mut DispersionKey,
            shown: usize,
            size: Vec2,
            what: &str,
        ) {
            let next = app.dispersion_key(shown, size);
            assert_ne!(next, *key, "{what}");
            assert!(app.dispersion_scene.peek(&next).is_none(), "{what}");
            app.refresh_dispersion_scene(&next, shown);
            assert!(app.dispersion_scene.peek(&next).is_some(), "{what}");
            *key = next;
        }
        app.dispersion_points = Points::Carry;
        changed(&mut app, &mut key, 7, size, "Total | Carry");
        assert_eq!(
            app.dispersion_scene.peek(&key).unwrap().points,
            Points::Carry
        );
        app.ellipse_size = EllipseSize::P95;
        changed(&mut app, &mut key, 7, size, "2σ | 95 %");
        assert_eq!(
            app.dispersion_scene.peek(&key).unwrap().size,
            EllipseSize::P95
        );
        app.units_toggle = UnitSystem::Metric;
        changed(&mut app, &mut key, 7, size, "units");
        changed(&mut app, &mut key, 6, size, "shown shot");
        assert_eq!(gold(app.dispersion_scene.peek(&key).unwrap()), Some(6));
        app.device_filter = Some("mevo.0".into());
        changed(&mut app, &mut key, 6, size, "device filter");
        assert_eq!(gold(app.dispersion_scene.peek(&key).unwrap()), None);
        app.device_filter = None;
        changed(&mut app, &mut key, 6, size, "device filter cleared");
        changed(&mut app, &mut key, 6, vec2(400.0, 700.0), "panel width");
        changed(&mut app, &mut key, 6, vec2(400.0, 600.0), "panel height");
        app.shots.push(shot(9, "mevo.0", "b", 150.0, 0.0));
        app.shots_rev += 1;
        changed(&mut app, &mut key, 6, vec2(400.0, 600.0), "new shot");
        app.groups.mode = ShotGrouping::ByClub;
        changed(&mut app, &mut key, 6, vec2(400.0, 600.0), "view");
        app.groups.new_session();
        changed(&mut app, &mut key, 6, vec2(400.0, 600.0), "new session");
        assert!(
            app.dispersion_scene
                .peek(&key)
                .unwrap()
                .dots
                .iter()
                .all(|d| d.level != Level::Faint),
            "nothing left in the new session to draw faint"
        );
    }

    /// Frame time of the Shots tab (Cards, 1600×900, both panels) with 1000
    /// shots in the session, the pointer moving over the Dispersion panel,
    /// and the time to rebuild its scene. Run with
    /// `cargo test --release -p flighthook-ui dispersion_frame_time -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, not a check"]
    fn dispersion_frame_time_with_1000_shots() {
        use std::time::Instant;
        let list: Vec<ShotGroupInfo> = (0..20)
            .map(|n| set(&format!("s{n}"), n + 1, Club::Iron7, 0))
            .collect();
        let g = groups(list);
        let shots: Vec<ShotRow> = (0..1000_u32)
            .map(|n| {
                let carry = 130.0 + f64::from(n % 17);
                let lateral = f64::from(n % 11) - 5.0;
                let mut s = shot(n, "mevo.0", &format!("s{}", n / 50), carry, lateral);
                s.derived.as_mut().unwrap().path =
                    Some(flight::tests::realistic_path(carry, 28.0, lateral));
                s
            })
            .collect();
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.shots = shots;
        app.shots_rev = 1;
        app.groups = g;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let frames = 200;
        let run = |app: &mut FlighthookApp| {
            let mut total = std::time::Duration::ZERO;
            for n in 0..frames + 20 {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
                    events: vec![egui::Event::PointerMoved(pos2(
                        1320.0 + (n % 50) as f32 * 5.0,
                        300.0 + (n % 30) as f32 * 10.0,
                    ))],
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
        let frame = run(&mut app);
        let key = app.dispersion_key(999, vec2(370.0, 760.0));
        let start = Instant::now();
        app.dispersion_scene = chart::Cached::default();
        app.refresh_dispersion_scene(&key, 999);
        let rebuild = start.elapsed();
        let scene = app.dispersion_scene.peek(&key).unwrap();
        let faint = levels(scene, Level::Faint).len();
        let bright = scene.dots.len() - faint;
        println!(
            "1000 shots: frame {frame:?} (run + tessellate, mean of {frames}); Dispersion \
             scene rebuild {rebuild:?}; {bright} bright + {faint} faint dots"
        );
    }

    #[test]
    fn the_title_and_switches_share_a_row_from_its_least_width() {
        fn walk(shape: &egui::Shape, texts: &mut Vec<Rect>, tracks: &mut Vec<Rect>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, texts, tracks)),
                egui::Shape::Text(t) => texts.push(t.visual_bounding_rect()),
                egui::Shape::Rect(r) if r.fill == theme::VEIL => tracks.push(r.rect),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        for units in [UnitSystem::Imperial, UnitSystem::Metric] {
            app.units_toggle = units;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 200.0))),
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show(ctx, |ui| {
                        let rect = Rect::from_min_size(Pos2::ZERO, vec2(ONE_ROW_MIN_WIDTH, ROW_H));
                        assert_eq!(header_height(rect.width(), false), ROW_H);
                        app.dispersion_header(ui, rect, None);
                    });
            });
            let (mut texts, mut tracks) = (Vec::new(), Vec::new());
            out.shapes
                .iter()
                .for_each(|c| walk(&c.shape, &mut texts, &mut tracks));
            // Both switches in the row, clear of the title and the unit.
            assert_eq!(tracks.len(), 2, "{units:?}");
            let switches = tracks.iter().map(|r| r.left()).fold(f32::MAX, f32::min);
            // The title and the unit, everything that starts left of the
            // switches, end at least 4 px before them.
            let title = texts
                .iter()
                .filter(|r| r.left() < switches)
                .map(|r| r.right())
                .fold(0.0, f32::max);
            assert!(
                title > 50.0 && title + 4.0 <= switches,
                "{units:?}: {title} {switches}"
            );
            for r in &tracks {
                assert!(r.right() <= ONE_ROW_MIN_WIDTH + 0.5, "{units:?}: {r:?}");
                assert!(
                    r.top() >= -0.5 && r.bottom() <= ROW_H + 0.5,
                    "{units:?}: {r:?}"
                );
            }
        }
    }
}
