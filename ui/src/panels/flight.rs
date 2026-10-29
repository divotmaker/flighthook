//! The Flight panel: a side view of the ball flights under the Cards view.
//!
//! Horizontal path distance from the tee (`hypot(x, z)` of the simulated
//! path, so the landing sits at the carry and the rest at the total)
//! against height, on independent auto scales over the bright shots.
//!
//! - **Gold**: the shown shot, with its apex, landing and rest marked. The
//!   latest shot draws on during the new-shot pulse.
//! - **Bright**: the view's other members (`chart::members_for`), coloured
//!   per device when every device is shown and there are two or more.
//! - **Faint**: the rest of the current session, newest [`FAINT_CAP`],
//!   for context. The view `none` draws the shown shot alone.
//!
//! A ground ball (a path whose flight is the tee alone) is drawn as its
//! roll along the ground line from the tee to rest. A shot without a path
//! gets ticks on the ground line at its carry and total. The projected
//! geometry is cached on `chart::ChartKey` and rebuilt only when the
//! members, units, shown shot, device filter or panel size change.

use egui::{Color32, Pos2, Rect, Stroke, Vec2, pos2, vec2};

use super::shots::{bar_group, pick};
use crate::app::FlighthookApp;
use crate::chart::{self, Candidate, ChartKey, Curve, Projection};
use crate::groups::ShotGroups;
use crate::theme;
use crate::types::{Distance, FlightPath, ShotGrouping, ShotRow, UnitSystem};

/// Most faint shots drawn: the newest ones.
pub(super) const FAINT_CAP: usize = 200;

/// Header line height.
const HEADER_H: f32 = 22.0;
/// Room left of the plot for the height labels.
const LEFT_GUTTER: f32 = 34.0;
/// Room under the plot for the distance labels.
const BOTTOM_GUTTER: f32 = 16.0;
/// Room right of the plot for the last distance label.
const RIGHT_PAD: f32 = 14.0;
/// Gap between the header and the plot.
const TOP_PAD: f32 = 6.0;
/// Narrowest gap between distance ticks before they thin out.
const X_TICK_MIN_PX: f32 = 70.0;
/// Narrowest gap between height ticks before they thin out.
const Y_TICK_MIN_PX: f32 = 24.0;
/// Hover radius around a landing or rest dot (or a ground tick).
const DOT_HIT: f32 = 8.0;
/// Hover radius around a path.
const LINE_HIT: f32 = 5.0;
/// Alpha of the faint shots.
const FAINT_ALPHA: f32 = 0.14;
/// Alpha of a bright shot's ground segment.
const GROUND_ALPHA: f32 = 0.7;
/// Height of a ground tick.
const TICK_H: f32 = 6.0;

const M_PER_YD: f64 = 0.9144;
const M_PER_FT: f64 = 0.3048;

// ---------------------------------------------------------------------------
// Units and readings
// ---------------------------------------------------------------------------

/// Display units per metre on the distance and height axes: yd and ft
/// imperial, m and m metric.
pub(super) fn per_metre(units: UnitSystem) -> (f64, f64) {
    match units {
        UnitSystem::Imperial => (1.0 / M_PER_YD, 1.0 / M_PER_FT),
        UnitSystem::Metric => (1.0, 1.0),
    }
}

/// Labels of the distance and height units.
pub(super) fn unit_labels(units: UnitSystem) -> (&'static str, &'static str) {
    match units {
        UnitSystem::Imperial => ("yd", "ft"),
        UnitSystem::Metric => ("m", "m"),
    }
}

/// Chart point of a path point `[t, x, y, z]` (metres): path distance from
/// the tee and height, in display units.
fn side_point(p: &[f64; 4], k: (f64, f64)) -> [f64; 2] {
    [p[1].hypot(p[3]) * k.0, p[2] * k.1]
}

/// What a shot displays for carry, total and apex, in display units
/// (picked like the cards: the simulated value when there is one, else the
/// measured one).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Readings {
    pub(super) carry: Option<f64>,
    pub(super) total: Option<f64>,
    pub(super) apex: Option<f64>,
}

pub(super) fn readings(shot: &ShotRow, units: UnitSystem) -> Readings {
    let imperial = units == UnitSystem::Imperial;
    let ball = shot.ball.as_ref();
    let derived = shot.derived.as_ref();
    let dist = |d: Distance| {
        if imperial {
            d.as_yards()
        } else {
            d.as_meters()
        }
    };
    let height = |d: Distance| if imperial { d.as_feet() } else { d.as_meters() };
    let read =
        |measured: Option<Distance>, simulated: Option<Distance>, to: &dyn Fn(Distance) -> f64| {
            pick(measured, simulated).map(|(d, _)| to(d))
        };
    Readings {
        carry: read(
            ball.and_then(|b| b.carry_distance),
            derived.and_then(|d| d.carry_distance),
            &dist,
        ),
        total: read(
            ball.and_then(|b| b.total_distance),
            derived.and_then(|d| d.total_distance),
            &dist,
        ),
        apex: read(
            ball.and_then(|b| b.max_height),
            derived.and_then(|d| d.max_height),
            &height,
        ),
    }
}

/// The shot's simulated path, when it has something to draw: a flight, or
/// a ground ball's roll from the tee ([`rolls`]).
pub(super) fn path(shot: &ShotRow) -> Option<&FlightPath> {
    shot.derived
        .as_ref()
        .and_then(|d| d.path.as_ref())
        .filter(|p| p.flight.len() >= 2 || rolls(p))
}

/// Whether `p` is a ground ball's: a one-point flight (the tee, where the
/// ball "lands") and a roll along the ground to rest.
pub(super) fn rolls(p: &FlightPath) -> bool {
    p.flight.len() == 1 && p.ground.len() >= 2
}

/// Whether `shot` has anything to draw: a path, or a carry or total for
/// ground ticks.
fn drawable(shot: &ShotRow) -> bool {
    let r = readings(shot, UnitSystem::Metric);
    path(shot).is_some() || r.carry.is_some() || r.total.is_some()
}

// ---------------------------------------------------------------------------
// Which shots are drawn
// ---------------------------------------------------------------------------

/// The shots a chart panel draws, as indices into the shot list.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Drawn {
    /// The shown shot, when it is a member of the view.
    pub(super) gold: Option<usize>,
    /// The view's other members with something to draw, oldest first.
    pub(super) bright: Vec<usize>,
    /// Other shots of the current session with something to draw, newest
    /// [`FAINT_CAP`], oldest first. Empty in the view `none`.
    pub(super) faint: Vec<usize>,
}

/// The gold, bright and faint shots for the view's `members` (ascending
/// indices, after the device filter) around the shown shot `shown`. Faint
/// shots are those from `device` (every device when `None`) whose set is in
/// the current session. Bright and faint shots are those `drawable` accepts;
/// the Flight panel's rule is [`drawable`].
pub(super) fn drawn_sets(
    shots: &[ShotRow],
    groups: &ShotGroups,
    view: ShotGrouping,
    members: &[usize],
    shown: usize,
    device: Option<&str>,
    drawable: impl Fn(&ShotRow) -> bool,
) -> Drawn {
    let member = |i: usize| members.binary_search(&i).is_ok();
    let gold = member(shown).then_some(shown);
    let bright = members
        .iter()
        .copied()
        .filter(|&i| i != shown && shots.get(i).is_some_and(&drawable))
        .collect();
    let mut faint = Vec::new();
    if view != ShotGrouping::None {
        let session = groups.session();
        let in_session = |r: &ShotRow| {
            r.group
                .as_deref()
                .and_then(|id| groups.get(id))
                .is_some_and(|g| g.session == session)
        };
        faint = shots
            .iter()
            .enumerate()
            .filter(|&(i, r)| {
                i != shown
                    && !member(i)
                    && device.is_none_or(|d| r.actor == d)
                    && in_session(r)
                    && drawable(r)
            })
            .map(|(i, _)| i)
            .collect();
        let excess = faint.len().saturating_sub(FAINT_CAP);
        faint.drain(..excess);
    }
    Drawn {
        gold,
        bright,
        faint,
    }
}

// ---------------------------------------------------------------------------
// Scene: the projected geometry, cached
// ---------------------------------------------------------------------------

/// How prominently a shot is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Faint,
    Bright,
    Gold,
}

/// One shot's path in panel-local screen points, as smooth curves
/// (`chart::smooth_path`).
#[derive(Debug)]
struct Trace {
    index: usize,
    level: Level,
    colour: Color32,
    flight: Curve,
    /// Empty when the ground phase did not reach rest.
    ground: Curve,
    /// A ground ball ([`rolls`]): `flight` is the tee alone and `ground` is
    /// the whole shot, drawn with a flight's stroke.
    rolled: bool,
    /// The landing dot (none for a ground ball), then the rest dot when
    /// there is a ground segment.
    dots: Vec<Pos2>,
    /// Bounds of `flight` and `ground`, so a hit-test skips a path far from
    /// the pointer.
    bounds: Rect,
}

/// A tick on the ground line at a carry or total, for a shot without a
/// path (or a total past a path with no ground segment).
#[derive(Debug)]
struct Tick {
    index: usize,
    level: Level,
    /// Foot of the tick, on the ground line.
    at: Pos2,
    total: bool,
}

/// The gold shot's labels: the point each one labels, its text and the
/// time its draw-on reveals it at.
#[derive(Debug, Default)]
struct Marks {
    /// The apex, and the time the path reaches it.
    apex: Option<(Pos2, String, f64)>,
    /// The landing on the ground line, and the landing time.
    carry: Option<(Pos2, String, f64)>,
    /// The rest on the ground line, and the time the ball stops.
    total: Option<(Pos2, String, f64)>,
}

/// Font size of the gold shot's apex, carry and total labels.
const MARK_FONT: f32 = 14.0;
/// Padding of a label's background box.
const MARK_PAD: Vec2 = vec2(3.0, 1.0);
/// A carry or total box's bottom sits this far above its point.
const MARK_RISE: f32 = 7.0;
/// The apex box's bottom sits this far above the apex, over its tick.
const APEX_RISE: f32 = 13.0;
/// Least gap between the carry and total boxes on one row.
const MARK_GAP: f32 = 6.0;
/// Gap under a label lifted over another.
const LIFT_GAP: f32 = 5.0;
/// Height of a label's box: a [`MARK_FONT`] line (16 pt) and its padding.
const MARK_BOX_H: f32 = 16.0 + 2.0 * MARK_PAD.y;
/// Gap between the apex label's box and the plot's top.
const MARK_MARGIN: f32 = 4.0;
/// Room the gold apex keeps under the plot's top: its label's rise, its box
/// and a margin.
const MARK_ROOM: f32 = APEX_RISE + MARK_BOX_H + MARK_MARGIN;

/// Where the gold shot's apex, carry and total labels go, as their boxes
/// (padding included), given the points they label and their text sizes.
///
/// Each box is centred on its point's x, its bottom [`MARK_RISE`] above a
/// carry or total point and [`APEX_RISE`] above the apex. When the carry
/// and total boxes would come closer than [`MARK_GAP`], total stays and
/// carry lifts one row. An apex box that would meet either lifts above the
/// higher of them. Every box is then kept inside the plot's width.
fn place_marks(points: [Option<Pos2>; 3], sizes: [Vec2; 3], plot: Rect) -> [Option<Rect>; 3] {
    let [apex, carry, total] = points;
    let boxed = |at: Option<Pos2>, size: Vec2, rise: f32| {
        at.map(|p| {
            let size = size + 2.0 * MARK_PAD;
            Rect::from_center_size(pos2(p.x, p.y - rise - size.y / 2.0), size)
        })
    };
    let x_in = |r: Rect| {
        let shift = (plot.left() - r.left()).max(0.0) + (plot.right() - r.right()).min(0.0);
        r.translate(vec2(shift, 0.0))
    };
    let mut carry = boxed(carry, sizes[1], MARK_RISE).map(x_in);
    let total = boxed(total, sizes[2], MARK_RISE).map(x_in);
    if let (Some(c), Some(t)) = (carry.as_mut(), total)
        && c.expand2(vec2(MARK_GAP / 2.0, 0.0))
            .intersects(t.expand2(vec2(MARK_GAP / 2.0, 0.0)))
    {
        *c = c.translate(vec2(0.0, -(c.height() + LIFT_GAP)));
    }
    let mut apex = boxed(apex, sizes[0], APEX_RISE).map(x_in);
    if let Some(a) = apex.as_mut() {
        let below = [carry, total].into_iter().flatten();
        if below.clone().any(|b| b.intersects(*a)) {
            let top = below.map(|b| b.top()).fold(f32::INFINITY, f32::min);
            *a = a.translate(vec2(0.0, top - LIFT_GAP - a.bottom()));
        }
    }
    [apex, carry, total]
}

/// Everything the panel paints, in panel-local points (origin at the
/// panel's top-left), built once per [`ChartKey`].
#[derive(Debug)]
pub(crate) struct Scene {
    plot: Rect,
    proj: Projection,
    x_ticks: Vec<(f32, String)>,
    y_ticks: Vec<(f32, String)>,
    /// Faint, then bright, then gold.
    traces: Vec<Trace>,
    ticks: Vec<Tick>,
    marks: Marks,
    /// The shown shot's displayed values, for the header.
    shown: Readings,
    /// How many of the view's members the device filter leaves.
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
    /// Panel size.
    size: Vec2,
}

/// The plot area inside a panel of `size`.
fn plot_rect(size: Vec2) -> Rect {
    let min = pos2(LEFT_GUTTER, HEADER_H + TOP_PAD);
    let max = pos2(
        (size.x - RIGHT_PAD).max(min.x + 1.0),
        (size.y - BOTTOM_GUTTER).max(min.y + 1.0),
    );
    Rect::from_min_max(min, max)
}

/// Largest distance and height `shot` reaches, in display units. Values
/// that are not [`chart::plausible`] are left out.
fn extent(shot: &ShotRow, units: UnitSystem) -> (f64, f64) {
    let k = per_metre(units);
    let r = readings(shot, units);
    let fit = |v: f64| if chart::plausible(v) { v } else { 0.0 };
    let mut x = fit(r.carry.unwrap_or(0.0)).max(fit(r.total.unwrap_or(0.0)));
    let mut y = fit(r.apex.unwrap_or(0.0));
    if let Some(p) = path(shot) {
        for q in p.flight.iter().chain(&p.ground) {
            let [s, h] = side_point(q, k);
            x = x.max(fit(s));
            y = y.max(fit(h));
        }
    }
    (x, y)
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
        size,
    } = *input;
    let drawn = drawn_sets(shots, groups, view, members, shown, device, drawable);
    let k = per_metre(units);

    // Scales over the bright shots, else the faint ones.
    let bright: Vec<usize> = drawn
        .gold
        .into_iter()
        .chain(drawn.bright.iter().copied())
        .collect();
    let scaled = if bright.is_empty() {
        &drawn.faint
    } else {
        &bright
    };
    let (x_data, y_data) = scaled
        .iter()
        .filter_map(|&i| shots.get(i))
        .map(|s| extent(s, units))
        .fold((0.0_f64, 0.0_f64), |a, b| (a.0.max(b.0), a.1.max(b.1)));
    // Over the drawable ones: a shown shot with nothing to draw yet (club
    // data before its ball flight) doesn't pick the axis.
    let scaled_paths: Vec<Option<&FlightPath>> = scaled
        .iter()
        .filter_map(|&i| shots.get(i))
        .filter(|s| drawable(s))
        .map(path)
        .collect();
    let all_rolled = !scaled_paths.is_empty() && scaled_paths.iter().all(|p| p.is_some_and(rolls));
    let distance_axis = if all_rolled {
        chart::SIDE_DISTANCE_SHORT
    } else {
        chart::SIDE_DISTANCE
    };
    let height_axis = chart::side_height(units);
    let plot = plot_rect(size);
    let domain = (
        distance_axis.domain_max(x_data),
        headroom(
            height_axis.domain_max(y_data),
            gold_top(drawn.gold.and_then(|i| shots.get(i)), k),
            plot.height(),
        ),
    );
    let proj = Projection::fit(plot, (0.0, domain.0), (0.0, domain.1));

    let tick_labels = |scale: &chart::Scale, step: f64, min_px: f32, hi: f64| {
        let step = chart::tick_step(step, scale.px_per_unit(), min_px);
        chart::ticks(0.0, hi, step)
            .into_iter()
            .map(|v| (scale.map(v), format!("{v:.0}")))
            .collect::<Vec<_>>()
    };
    let x_ticks = tick_labels(&proj.x, distance_axis.step, X_TICK_MIN_PX, domain.0);
    let y_ticks = tick_labels(&proj.y, height_axis.step, Y_TICK_MIN_PX, domain.1);

    let colour_of = |s: &ShotRow| chart::shot_colour(devices, device, &s.actor);
    let ground_y = proj.y.map(0.0);
    let mut traces = Vec::new();
    let mut ticks = Vec::new();
    let mut marks = Marks::default();
    let levels = drawn
        .faint
        .iter()
        .map(|&i| (i, Level::Faint))
        .chain(drawn.bright.iter().map(|&i| (i, Level::Bright)))
        .chain(drawn.gold.map(|i| (i, Level::Gold)));
    for (index, level) in levels {
        let Some(shot) = shots.get(index) else {
            continue;
        };
        let r = readings(shot, units);
        let mut tick = |v: Option<f64>, total: bool| {
            if let Some(v) = v {
                ticks.push(Tick {
                    index,
                    level,
                    at: pos2(proj.x.map(v), ground_y),
                    total,
                });
            }
        };
        match path(shot) {
            Some(p) => {
                let (flight, ground) = chart::smooth_path(p, |q| {
                    let [s, h] = side_point(q, k);
                    proj.to_screen(s, h)
                });
                let rolled = rolls(p);
                let landing = if rolled { None } else { flight.points.last() };
                let dots = landing
                    .into_iter()
                    .chain(ground.points.last())
                    .copied()
                    .collect();
                if ground.points.is_empty() {
                    tick(r.total, true);
                }
                if level == Level::Gold {
                    marks = gold_marks(p, &r, &proj, k, units);
                }
                let bounds =
                    Rect::from_points(&flight.points).union(Rect::from_points(&ground.points));
                traces.push(Trace {
                    index,
                    level,
                    colour: colour_of(shot),
                    flight,
                    ground,
                    rolled,
                    dots,
                    bounds,
                });
            }
            None => {
                tick(r.carry, false);
                tick(r.total, true);
                if level == Level::Gold {
                    let label = |v: Option<f64>| {
                        v.map(|v| (pos2(proj.x.map(v), ground_y), format!("{v:.0}"), 0.0))
                    };
                    marks.carry = label(r.carry);
                    marks.total = label(r.total);
                }
            }
        }
    }

    Scene {
        plot,
        proj,
        x_ticks,
        y_ticks,
        traces,
        ticks,
        marks,
        // Only for a shot drawn in gold: one the device filter hides has no
        // values in the header either.
        shown: drawn
            .gold
            .and_then(|i| shots.get(i))
            .map(|s| readings(s, units))
            .unwrap_or_default(),
        members: members.len(),
    }
}

/// Height of the gold `shot`'s apex on its drawn path, in display units
/// (`k` per metre); none for a shot without a flight drawn.
fn gold_top(shot: Option<&ShotRow>, k: (f64, f64)) -> Option<f64> {
    let p = path(shot?)?;
    if rolls(p) {
        return None;
    }
    p.flight
        .iter()
        .map(|q| side_point(q, k)[1])
        .filter(|h| chart::plausible(*h))
        .reduce(f64::max)
}

/// The height domain's top: `nice`, raised just enough that an apex at
/// `top` sits [`MARK_ROOM`] under the top of a plot `plot_h` tall, so its
/// label fits above it.
fn headroom(nice: f64, top: Option<f64>, plot_h: f32) -> f64 {
    let room = f64::from(plot_h - MARK_ROOM);
    match top {
        Some(top) if top > 0.0 && room > 0.0 => nice.max(top * f64::from(plot_h) / room),
        _ => nice,
    }
}

/// The gold shot's apex, carry and total labels, placed on its path `p`.
/// The numbers are the displayed values; a total with no ground segment is
/// left to its tick's label. A ground ball ([`rolls`]) has the total label
/// only.
fn gold_marks(
    p: &FlightPath,
    r: &Readings,
    proj: &Projection,
    k: (f64, f64),
    units: UnitSystem,
) -> Marks {
    let to_screen = |q: &[f64; 4]| {
        let [s, h] = side_point(q, k);
        proj.to_screen(s, h)
    };
    let rolled = rolls(p);
    let top = p
        .flight
        .iter()
        .max_by(|a, b| a[2].total_cmp(&b[2]))
        .filter(|_| !rolled);
    let (_, height) = unit_labels(units);
    let apex = top.map(|q| {
        let v = r.apex.unwrap_or(q[2] * k.1);
        (to_screen(q), format!("{v:.0} {height}"), q[0])
    });
    let ground_y = proj.y.map(0.0);
    let on_ground = |x: f32| pos2(x, ground_y);
    let landing = p.flight.last();
    let carry = landing.filter(|_| !rolled).map(|q| {
        let v = r.carry.unwrap_or(side_point(q, k)[0]);
        (on_ground(to_screen(q).x), format!("{v:.0}"), q[0])
    });
    let total = match p.ground.last() {
        Some(q) => {
            let v = r.total.unwrap_or(side_point(q, k)[0]);
            Some((on_ground(to_screen(q).x), format!("{v:.0}"), q[0]))
        }
        None => r.total.map(|v| {
            let t = landing.map_or(0.0, |q| q[0]);
            (on_ground(proj.x.map(v)), format!("{v:.0}"), t)
        }),
    };
    Marks { apex, carry, total }
}

/// The shot under `pointer` (panel-local): the nearest landing or rest dot
/// or ground tick within [`DOT_HIT`], else the nearest path within
/// [`LINE_HIT`]. Faint shots are not hit.
fn hit(scene: &Scene, pointer: Pos2) -> Option<usize> {
    // Topmost first, so the gold shot wins a tie with a path it covers.
    let dots: Vec<Candidate<'_, usize>> = scene
        .traces
        .iter()
        .rev()
        .map(|t| Candidate {
            key: t.index,
            points: &t.dots,
            hittable: t.level != Level::Faint,
        })
        .chain(scene.ticks.iter().map(|t| Candidate {
            key: t.index,
            points: std::slice::from_ref(&t.at),
            hittable: t.level != Level::Faint,
        }))
        .collect();
    chart::nearest_point(pointer, DOT_HIT, &dots).or_else(|| {
        let lines: Vec<Candidate<'_, usize>> = scene
            .traces
            .iter()
            .rev()
            .filter(|t| t.bounds.expand(LINE_HIT).contains(pointer))
            .flat_map(|t| {
                [&t.flight, &t.ground].map(|curve| Candidate {
                    key: t.index,
                    points: &curve.points,
                    hittable: t.level != Level::Faint,
                })
            })
            .collect();
        chart::nearest_segment(pointer, LINE_HIT, &lines)
    })
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// The gold path drawn so far during the draw-on, panel-local.
struct DrawOn {
    t: f64,
    flight: Vec<Pos2>,
    ground: Vec<Pos2>,
}

impl DrawOn {
    /// The scene's gold path flown up to `progress` (0..1) of its whole
    /// time: its cached curves cut by time. `None` without a gold path.
    fn new(scene: &Scene, progress: f32) -> Option<Self> {
        let gold = scene.traces.iter().find(|t| t.level == Level::Gold)?;
        let landing = gold.flight.end_time()?;
        let end = gold.ground.end_time().unwrap_or(landing);
        let t = f64::from(progress) * end;
        Some(Self {
            t,
            flight: gold.flight.prefix(t),
            ground: if t >= landing {
                gold.ground.prefix(t)
            } else {
                Vec::new()
            },
        })
    }
}

fn moved(points: &[Pos2], by: Vec2) -> Vec<Pos2> {
    points.iter().map(|p| *p + by).collect()
}

/// A trace's `flight` and `ground` points as the lines drawn with the
/// flight's stroke and the ground's. A ground ball's roll is the whole
/// shot, so it takes the flight's stroke and there is no ground line.
fn strokes<'a>(rolled: bool, flight: &'a [Pos2], ground: &'a [Pos2]) -> (&'a [Pos2], &'a [Pos2]) {
    if rolled {
        (ground, &[])
    } else {
        (flight, ground)
    }
}

/// Paint `scene` with its origin at `origin`: grid, ground, faint, bright
/// and gold paths, ticks and the gold shot's labels. `hovered` brightens
/// one shot; `draw_on` replaces the gold path while it draws on.
fn paint(
    painter: &egui::Painter,
    scene: &Scene,
    origin: Vec2,
    hovered: Option<usize>,
    draw_on: Option<&DrawOn>,
) {
    let plot = scene.plot.translate(origin);
    let grid = Stroke::new(1.0, theme::BORDER.gamma_multiply(0.5));
    let label_font = egui::FontId::proportional(10.0);
    let shown_t = draw_on.map_or(f64::INFINITY, |d| d.t);

    for (x, text) in &scene.x_ticks {
        let x = origin.x + x;
        painter.vline(x, plot.y_range(), grid);
        let galley = painter.layout_no_wrap(text.clone(), label_font.clone(), theme::TEXT_DIM);
        let rect = Rect::from_center_size(
            pos2(x, plot.bottom() + 3.0 + galley.size().y / 2.0),
            galley.size(),
        );
        painter.galley(rect.min, galley, theme::TEXT_DIM);
    }
    for (y, text) in &scene.y_ticks {
        let y = origin.y + y;
        painter.hline(plot.x_range(), y, grid);
        painter.text(
            pos2(plot.left() - 6.0, y),
            egui::Align2::RIGHT_CENTER,
            text,
            label_font.clone(),
            theme::TEXT_DIM,
        );
    }
    let tee = scene.proj.to_screen(0.0, 0.0) + origin;
    painter.hline(plot.x_range(), tee.y, Stroke::new(1.0, theme::BORDER));
    painter.add(egui::Shape::convex_polygon(
        vec![tee, tee + vec2(4.0, 0.0), tee + vec2(0.0, -6.0)],
        theme::TEXT_MUTED,
        Stroke::NONE,
    ));

    let clip = painter.with_clip_rect(plot.expand(6.0));
    for t in &scene.traces {
        let lit = hovered == Some(t.index);
        let (air, roll) = strokes(t.rolled, &t.flight.points, &t.ground.points);
        match t.level {
            Level::Faint => {
                let c = t.colour.gamma_multiply(FAINT_ALPHA);
                clip.line(moved(air, origin), Stroke::new(1.5, c));
                if roll.len() >= 2 {
                    clip.line(moved(roll, origin), Stroke::new(1.0, c));
                }
            }
            Level::Bright => {
                let c = if lit {
                    theme::mix(t.colour, theme::TEXT, 0.35)
                } else {
                    t.colour
                };
                let w = if lit { 1.0 } else { 0.0 };
                clip.line(moved(air, origin), Stroke::new(2.0 + w, c));
                if roll.len() >= 2 {
                    clip.line(
                        moved(roll, origin),
                        Stroke::new(1.5 + w, c.gamma_multiply(GROUND_ALPHA)),
                    );
                }
                for (i, d) in t.dots.iter().enumerate() {
                    let r = if i == 0 && !t.rolled { 3.0 } else { 2.5 };
                    clip.circle_filled(*d + origin, r, c);
                }
            }
            Level::Gold => {}
        }
    }
    for tick in &scene.ticks {
        let colour = match (tick.level, tick.total) {
            (Level::Faint, _) => theme::TEXT_MUTED.gamma_multiply(FAINT_ALPHA),
            (Level::Bright, false) => theme::TEXT_MUTED,
            (Level::Bright, true) => theme::TEXT_DIM,
            (Level::Gold, _) => theme::GOLD,
        };
        let colour = if hovered == Some(tick.index) && tick.level == Level::Bright {
            theme::TEXT
        } else {
            colour
        };
        let at = tick.at + origin;
        // A gold tick appears with its label during the draw-on.
        let mark = if tick.total {
            &scene.marks.total
        } else {
            &scene.marks.carry
        };
        if tick.level != Level::Gold || mark.as_ref().is_none_or(|m| m.2 <= shown_t) {
            clip.vline(at.x, (at.y - TICK_H)..=at.y, Stroke::new(1.0, colour));
        }
    }

    // The gold shot on top, over a halo.
    if let Some(t) = scene.traces.iter().find(|t| t.level == Level::Gold) {
        let (flight, ground) = match draw_on {
            Some(d) => (d.flight.as_slice(), d.ground.as_slice()),
            None => (t.flight.points.as_slice(), t.ground.points.as_slice()),
        };
        let (air, roll) = strokes(t.rolled, flight, ground);
        let (air, roll) = (moved(air, origin), moved(roll, origin));
        if roll.len() >= 2 {
            clip.line(roll.clone(), Stroke::new(4.0, theme::BG));
            clip.line(roll, Stroke::new(2.0, theme::GOLD));
        }
        if air.len() >= 2 {
            clip.line(air.clone(), Stroke::new(4.5, theme::BG));
            clip.line(air, Stroke::new(2.5, theme::GOLD));
        }
        let reached = |time: f64| time <= shown_t;
        let landing = scene.marks.carry.as_ref().is_none_or(|m| reached(m.2));
        let stopped = scene.marks.total.as_ref().is_none_or(|m| reached(m.2));
        for (i, d) in t.dots.iter().enumerate() {
            let (r, show) = if i == 0 && !t.rolled {
                (4.0, landing)
            } else {
                (3.5, stopped)
            };
            if show {
                clip.circle_filled(*d + origin, r + 1.5, theme::BG);
                clip.circle_filled(*d + origin, r, theme::GOLD);
            }
        }
    }

    // The gold shot's labels, placed together whether or not the draw-on
    // has reached them yet, so none moves when another appears.
    let marks = [&scene.marks.apex, &scene.marks.carry, &scene.marks.total];
    let colours = [theme::GOLD, theme::TEXT, theme::TEXT];
    let galleys = std::array::from_fn::<_, 3, _>(|i| {
        marks[i].as_ref().map(|(_, text, _)| {
            painter.layout_no_wrap(
                text.clone(),
                egui::FontId::proportional(MARK_FONT),
                colours[i],
            )
        })
    });
    let boxes = place_marks(
        marks.map(|m| m.as_ref().map(|(at, _, _)| *at + origin)),
        std::array::from_fn(|i| galleys[i].as_ref().map_or(Vec2::ZERO, |g| g.size())),
        plot,
    );
    if let Some((at, _, t)) = &scene.marks.apex
        && *t <= shown_t
    {
        let at = *at + origin;
        painter.vline(
            at.x,
            (at.y - 10.0)..=(at.y - 4.0),
            Stroke::new(1.5, theme::GOLD),
        );
    }
    for ((mark, galley), (rect, colour)) in marks.iter().zip(galleys).zip(boxes.iter().zip(colours))
    {
        if let (Some((_, _, t)), Some(galley), Some(rect)) = (mark, galley, rect)
            && *t <= shown_t
        {
            painter.rect_filled(*rect, 3.0, theme::BG);
            painter.galley(rect.min + MARK_PAD, galley, colour);
        }
    }
}

/// Paint the header line along the top of `rect`: `FLIGHT` and the units
/// on the left, the shown shot's apex, carry and total on the right.
fn paint_header(painter: &egui::Painter, rect: Rect, units: UnitSystem, shown: Option<&Readings>) {
    let mid = rect.top() + HEADER_H / 2.0;
    let title = painter.text(
        pos2(rect.left(), mid),
        egui::Align2::LEFT_CENTER,
        "FLIGHT",
        egui::FontId::proportional(11.0),
        theme::TEXT_MUTED,
    );
    let (dist, height) = unit_labels(units);
    painter.text(
        pos2(title.right() + 8.0, mid),
        egui::Align2::LEFT_CENTER,
        format!("{dist} \u{b7} {height}"),
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
    let Some(r) = shown else {
        return;
    };
    // Right to left: total, carry, apex.
    let mut right = rect.right() - RIGHT_PAD;
    let items = [
        ("TOTAL", r.total, dist),
        ("CARRY", r.carry, dist),
        ("APEX", r.apex, height),
    ];
    for (n, (label, value, unit)) in items.into_iter().enumerate() {
        let Some(v) = value else {
            continue;
        };
        if n > 0 && right < rect.right() - RIGHT_PAD {
            right -= 14.0;
        }
        let value = painter.text(
            pos2(right, mid),
            egui::Align2::RIGHT_CENTER,
            format!("{v:.0} {unit}"),
            egui::FontId::proportional(12.0),
            theme::TEXT_MUTED,
        );
        let label = painter.text(
            pos2(value.left() - 5.0, mid),
            egui::Align2::RIGHT_CENTER,
            label,
            egui::FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
        right = label.left();
    }
}

/// Hover text for `shot`: number, device (with two or more devices) and
/// club, then carry, total and apex on a second line.
fn tooltip(shot: &ShotRow, device: Option<&str>, units: UnitSystem) -> String {
    let (dist, height) = unit_labels(units);
    let r = readings(shot, units);
    let mut parts = vec![format!("#{}", shot.shot_number)];
    parts.extend(device.map(str::to_owned));
    parts.extend(shot.club_selected.map(|c| c.to_string()));
    let value = |name: &str, v: Option<f64>, unit: &str| v.map(|v| format!("{name} {v:.0} {unit}"));
    let values: Vec<String> = [
        value("carry", r.carry, dist),
        value("total", r.total, dist),
        value("apex", r.apex, height),
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

impl FlighthookApp {
    /// Key of the Flight scene for a panel of `size` around the shown shot
    /// `shown`.
    fn flight_key(&self, shown: usize, size: Vec2) -> ChartKey {
        ChartKey::new(self.members_key(shown), self.units_toggle, size)
    }

    /// Bring the cached Flight scene up to date for `key`: rebuilt only when
    /// the key changed, with the view's members from their own cache.
    fn refresh_flight_scene(&mut self, key: &ChartKey, shown: usize) {
        if self.flight_scene.peek(key).is_some() {
            return;
        }
        let view = self.groups.mode;
        let set = bar_group(&self.groups, self.shots.get(shown), self.selected_club);
        let members = self
            .view_members
            .get(key.members.clone(), || {
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
        let devices = self
            .device_ids
            .peek(&self.shots_rev)
            .cloned()
            .unwrap_or_else(|| chart::device_ids(&self.shots));
        let scene = build_scene(&SceneInput {
            shots: &self.shots,
            groups: &self.groups,
            view,
            members: &members,
            shown,
            devices: &devices,
            device: self.device_filter.as_deref(),
            units: self.units_toggle,
            size: vec2(key.size[0] as f32, key.size[1] as f32),
        });
        self.flight_scene.get(key.clone(), || scene);
    }

    /// The Flight panel, filling `ui`.
    pub(super) fn render_flight_panel(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
        let painter = ui.painter_at(rect);
        let units = self.units_toggle;
        let Some(shown) = self.shot_focus.index(&self.shots) else {
            paint_header(&painter, rect, units, None);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Ball flights appear here once a shot lands",
                egui::FontId::proportional(13.0),
                theme::TEXT_DIM,
            );
            return;
        };
        let key = self.flight_key(shown, rect.size());
        self.refresh_flight_scene(&key, shown);
        let Some(scene) = self.flight_scene.peek(&key) else {
            return;
        };
        let origin = rect.min.to_vec2();

        // The latest shot draws on while its pulse runs.
        let now = ui.ctx().input(|i| i.time);
        let progress = self
            .shot_focus
            .following(&self.shots)
            .then(|| self.shot_focus.progress(now))
            .flatten();
        let draw_on = progress.and_then(|p| DrawOn::new(scene, p));
        if draw_on.is_some() {
            ui.ctx().request_repaint();
        }

        let hovered = response.hover_pos().and_then(|p| hit(scene, p - origin));
        paint_header(&painter, rect, units, Some(&scene.shown));
        paint(&painter, scene, origin, hovered, draw_on.as_ref());
        // Nothing is said when the device filter hides every member.
        let drawn = |level: Level| level != Level::Faint;
        if scene.members > 0
            && !scene.traces.iter().any(|t| drawn(t.level))
            && !scene.ticks.iter().any(|t| drawn(t.level))
        {
            painter.text(
                scene.plot.translate(origin).center(),
                egui::Align2::CENTER_CENTER,
                "No flight data for this shot",
                egui::FontId::proportional(12.0),
                theme::TEXT_DIM,
            );
        }

        let clicked = match hovered {
            Some(i) => {
                let devices = self.devices();
                let device = (devices.len() >= 2).then(|| {
                    let id = self.shots[i].actor.as_str();
                    let name = self.actors.get(id).map(|a| a.name.as_str());
                    chart::device_label(id, name)
                });
                let text = tooltip(&self.shots[i], device, units);
                let response = response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text_at_pointer(text);
                response.clicked().then_some(i)
            }
            None => None,
        };
        if let Some(i) = clicked {
            self.shot_focus.select(&self.shots, i);
            self.shot_focus.reveal();
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::types::{BallFlight, Club, DerivedFlight, ShotGroupInfo};

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

    /// A path landing `carry` metres out with an apex of `apex` metres,
    /// rolling on to `total`.
    fn flight_path(carry: f64, apex: f64, total: f64) -> FlightPath {
        FlightPath {
            flight: vec![
                [0.0, 0.0, 0.0, 0.0],
                [1.0, carry * 0.3, apex * 0.8, 0.0],
                [3.0, carry * 0.6, apex, 0.0],
                [6.0, carry, 0.0, 0.0],
            ],
            ground: if total > carry {
                vec![[6.0, carry, 0.0, 0.0], [7.0, total, 0.0, 0.0]]
            } else {
                Vec::new()
            },
        }
    }

    fn derived(path: Option<FlightPath>, carry: f64, total: f64, apex: f64) -> DerivedFlight {
        let mut d = DerivedFlight::default();
        d.carry_distance = Some(Distance::Meters(carry));
        d.total_distance = Some(Distance::Meters(total));
        d.max_height = Some(Distance::Meters(apex));
        d.path = path;
        d
    }

    /// A shot from `actor` in set `group` with a simulated path.
    fn shot(n: u32, actor: &str, group: &str) -> ShotRow {
        ShotRow {
            actor: actor.into(),
            shot_id: format!("id{n}"),
            shot_number: n,
            ball: Some(BallFlight::default()),
            club: None,
            impact: None,
            derived: Some(derived(
                Some(flight_path(140.0, 30.0, 150.0)),
                140.0,
                150.0,
                30.0,
            )),
            club_selected: Some(Club::Iron7),
            handed: None,
            group: Some(group.into()),
            finished: true,
        }
    }

    /// A shot with a measured carry and total but no path.
    fn no_path(n: u32, carry: f64, total: f64) -> ShotRow {
        let ball = BallFlight {
            carry_distance: Some(Distance::Meters(carry)),
            total_distance: Some(Distance::Meters(total)),
            ..Default::default()
        };
        ShotRow {
            ball: Some(ball),
            derived: None,
            ..shot(n, "mevo.0", "b")
        }
    }

    /// A ground ball's path: the tee alone as its flight, then a roll to
    /// `total` metres, `lateral` metres right.
    fn roll_path(total: f64, lateral: f64) -> FlightPath {
        let at = |f: f64| [4.0 * f, total * f, 0.0, lateral * f];
        FlightPath {
            flight: vec![[0.0; 4]],
            ground: vec![at(0.0), at(0.4), at(1.0)],
        }
    }

    /// `shot(n, actor, group)` as a ground ball rolling `total` metres:
    /// carry, apex and flight time 0.
    fn ground_ball(n: u32, actor: &str, group: &str, total: f64) -> ShotRow {
        let mut d = derived(Some(roll_path(total, 0.2)), 0.0, total, 0.0);
        d.flight_time = Some(0.0);
        ShotRow {
            derived: Some(d),
            club_selected: Some(Club::Putter),
            ..shot(n, actor, group)
        }
    }

    /// Session 0: set `old` (2 shots). Session 1: set `a` (3), set `b` (3).
    fn history() -> (Vec<ShotRow>, ShotGroups) {
        let shots = vec![
            shot(1, "mevo.0", "old"),
            shot(2, "mevo.0", "old"),
            shot(3, "mevo.0", "a"),
            shot(4, "square.0", "a"),
            shot(5, "mevo.0", "a"),
            shot(6, "mevo.0", "b"),
            shot(7, "square.0", "b"),
            shot(8, "mevo.0", "b"),
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
        units: UnitSystem,
    ) -> SceneInput<'a> {
        SceneInput {
            shots,
            groups: g,
            view: ShotGrouping::BySet,
            members,
            shown,
            devices,
            device: None,
            units,
            size: vec2(800.0, 240.0),
        }
    }

    /// Domain ends: distance and height.
    fn domain(scene: &Scene) -> (f64, f64) {
        (scene.proj.x.domain().1, scene.proj.y.domain().1)
    }

    /// The shot drawn in gold, by its path or its ticks.
    fn gold(scene: &Scene) -> Option<usize> {
        let traces = scene.traces.iter().map(|t| (t.index, t.level));
        let ticks = scene.ticks.iter().map(|t| (t.index, t.level));
        traces
            .chain(ticks)
            .find(|(_, level)| *level == Level::Gold)
            .map(|(i, _)| i)
    }

    #[test]
    fn members_are_bright_and_the_session_faint() {
        let (shots, g) = history();
        // By set, shown #8 in set b: the set bright, set a faint, the
        // earlier session's set left out.
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let d = drawn_sets(&shots, &g, ShotGrouping::BySet, &m, 7, None, drawable);
        assert_eq!(d.gold, Some(7));
        assert_eq!(d.bright, vec![5, 6]);
        assert_eq!(d.faint, vec![2, 3, 4]);
        // The device filter applies first, to the faint shots too.
        let m = members(&shots, &g, ShotGrouping::BySet, 7, Some("mevo.0"));
        let d = drawn_sets(
            &shots,
            &g,
            ShotGrouping::BySet,
            &m,
            7,
            Some("mevo.0"),
            drawable,
        );
        assert_eq!(d.bright, vec![5]);
        assert_eq!(d.faint, vec![2, 4]);
        // The shown shot filtered out is not gold, as on the bars.
        let d = drawn_sets(
            &shots,
            &g,
            ShotGrouping::BySet,
            &m,
            6,
            Some("mevo.0"),
            drawable,
        );
        assert_eq!(d.gold, None);
        // By club: the club's sets in the session.
        let m = members(&shots, &g, ShotGrouping::ByClub, 7, None);
        let d = drawn_sets(&shots, &g, ShotGrouping::ByClub, &m, 7, None, drawable);
        assert_eq!((d.gold, d.bright.len(), d.faint.len()), (Some(7), 2, 3));
        // None: the shown shot alone, nothing faint.
        let m = members(&shots, &g, ShotGrouping::None, 7, None);
        let d = drawn_sets(&shots, &g, ShotGrouping::None, &m, 7, None, drawable);
        assert_eq!(
            d,
            Drawn {
                gold: Some(7),
                bright: vec![],
                faint: vec![]
            }
        );
    }

    #[test]
    fn faint_shots_are_capped_to_the_newest() {
        let g = groups(vec![
            set("a", 1, Club::Driver, 0),
            set("b", 2, Club::Iron7, 0),
        ]);
        let mut shots: Vec<ShotRow> = (0..250).map(|n| shot(n, "mevo.0", "a")).collect();
        shots.push(shot(250, "mevo.0", "b"));
        let m = members(&shots, &g, ShotGrouping::BySet, 250, None);
        let d = drawn_sets(&shots, &g, ShotGrouping::BySet, &m, 250, None, drawable);
        assert_eq!(d.faint.len(), FAINT_CAP);
        assert_eq!(d.faint.first(), Some(&50));
        assert_eq!(d.faint.last(), Some(&249));
    }

    #[test]
    fn the_header_shows_only_a_drawn_shot() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        assert_eq!(scene.shown, readings(&shots[7], UnitSystem::Metric));
        assert!(scene.shown.carry.is_some());
        // The device filter hides the shown shot: no gold, no header values.
        let m = members(&shots, &g, ShotGrouping::BySet, 6, Some("mevo.0"));
        let scene = build_scene(&SceneInput {
            device: Some("mevo.0"),
            ..input(&shots, &g, &m, 6, &[], UnitSystem::Metric)
        });
        assert_eq!(gold(&scene), None);
        assert_eq!(scene.shown, Readings::default());
    }

    #[test]
    fn an_absurd_shot_neither_hangs_nor_resizes_the_chart() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let sane = domain(&build_scene(&input(
            &shots,
            &g,
            &m,
            7,
            &[],
            UnitSystem::Metric,
        )));
        for v in [1e38, 1e300, f64::MAX, -1e300, -5.0, 0.0] {
            for units in [UnitSystem::Metric, UnitSystem::Imperial] {
                let mut shots = shots.clone();
                // A measured value no device sends, with no path.
                shots[6] = no_path(7, v, v);
                shots[6].ball.as_mut().unwrap().max_height = Some(Distance::Meters(v));
                let scene = build_scene(&input(&shots, &g, &m, 7, &[], units));
                let (x, y) = domain(&scene);
                assert!(x.is_finite() && y.is_finite(), "{v}");
                if units == UnitSystem::Metric {
                    assert_eq!((x, y), sane, "{v}");
                }
                assert!(scene.x_ticks.len() < 100 && scene.y_ticks.len() < 100);
                let points = scene
                    .traces
                    .iter()
                    .flat_map(|t| {
                        t.flight
                            .points
                            .iter()
                            .chain(&t.ground.points)
                            .chain(&t.dots)
                    })
                    .chain(scene.ticks.iter().map(|t| &t.at));
                for p in points {
                    assert!(p.x.is_finite() && p.y.is_finite(), "{v}: {p:?}");
                }
            }
        }
    }

    #[test]
    fn shots_without_a_path_get_ground_ticks() {
        let (mut shots, g) = history();
        shots[7] = no_path(8, 120.0, 130.0);
        shots[6].derived.as_mut().unwrap().path = Some(flight_path(140.0, 30.0, 140.0));
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        let gold: Vec<&Tick> = scene
            .ticks
            .iter()
            .filter(|t| t.level == Level::Gold)
            .collect();
        assert_eq!(gold.len(), 2);
        assert_eq!(gold[0].at.x, scene.proj.x.map(120.0));
        assert!(!gold[0].total && gold[1].total);
        assert_eq!(gold[1].at.x, scene.proj.x.map(130.0));
        assert_eq!(gold[0].at.y, scene.plot.bottom());
        // The gold labels sit at the ticks.
        assert_eq!(
            scene.marks.carry.as_ref().map(|m| m.1.as_str()),
            Some("120")
        );
        assert_eq!(
            scene.marks.total.as_ref().map(|m| m.1.as_str()),
            Some("130")
        );
        // A path with no ground segment gets a tick at its total only.
        let bright: Vec<&Tick> = scene.ticks.iter().filter(|t| t.index == 6).collect();
        assert_eq!(bright.len(), 1);
        assert!(bright[0].total);
        // No gold trace: the shown shot has no path.
        assert!(scene.traces.iter().all(|t| t.level != Level::Gold));
    }

    #[test]
    fn axes_follow_the_units() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        // 150 m total, 30 m apex.
        let metric = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        assert_eq!(domain(&metric), (175.0, 40.0));
        let imperial = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Imperial));
        // 164 yd × 1.06 → 175; 98.4 ft × 1.15 → 120.
        assert_eq!(domain(&imperial), (175.0, 120.0));
        let gold = |s: &Scene| {
            s.traces
                .iter()
                .find(|t| t.level == Level::Gold)
                .map(|t| t.dots.clone())
                .unwrap()
        };
        let landing_m = gold(&metric)[0];
        let landing_yd = gold(&imperial)[0];
        assert!((landing_m.x - metric.proj.x.map(140.0)).abs() < 1e-3);
        assert!((landing_yd.x - imperial.proj.x.map(140.0 / M_PER_YD)).abs() < 1e-3);
        assert_eq!(metric.marks.carry.as_ref().unwrap().1, "140");
        assert_eq!(imperial.marks.carry.as_ref().unwrap().1, "153");
        assert_eq!(metric.marks.apex.as_ref().unwrap().1, "30 m");
        assert_eq!(imperial.marks.apex.as_ref().unwrap().1, "98 ft");
        // Height ticks every 20 ft or 10 m (240 px panel), distance every 25.
        let labels = |t: &[(f32, String)]| t.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>();
        assert_eq!(labels(&metric.y_ticks), ["0", "10", "20", "30", "40"]);
        assert_eq!(labels(&imperial.y_ticks)[..3], ["0", "20", "40"]);
        assert_eq!(labels(&metric.x_ticks)[..3], ["0", "25", "50"]);
    }

    #[test]
    fn scales_cover_the_bright_shots_only() {
        let (mut shots, g) = history();
        // A long faint drive does not stretch the scale.
        shots[3].derived = Some(derived(
            Some(flight_path(250.0, 40.0, 270.0)),
            250.0,
            270.0,
            40.0,
        ));
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        assert_eq!(domain(&scene).0, 175.0);
    }

    #[test]
    fn bright_paths_take_device_colours_with_every_device_shown() {
        let (shots, g) = history();
        let devices = chart::device_ids(&shots);
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &devices, UnitSystem::Metric));
        let colour = |i: usize| scene.traces.iter().find(|t| t.index == i).unwrap().colour;
        assert_eq!(colour(5), theme::DEVICE_COLOURS[0]);
        assert_eq!(colour(6), theme::DEVICE_COLOURS[1]);
        // One device, or a filter: accent.
        let scene = build_scene(&input(&shots, &g, &m, 7, &devices[..1], UnitSystem::Metric));
        let colour = |i: usize| scene.traces.iter().find(|t| t.index == i).unwrap().colour;
        assert_eq!(colour(6), theme::ACCENT);
    }

    #[test]
    fn hit_test_picks_the_right_shot() {
        let (mut shots, g) = history();
        shots[5].derived = Some(derived(
            Some(flight_path(100.0, 20.0, 110.0)),
            100.0,
            110.0,
            20.0,
        ));
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        let trace = |i: usize| scene.traces.iter().find(|t| t.index == i).unwrap();
        // On #6's landing dot, and just off it.
        let landing = trace(5).dots[0];
        assert_eq!(hit(&scene, landing + vec2(3.0, -2.0)), Some(5));
        // On the gold shot's curve at its apex.
        let apex = trace(7)
            .flight
            .points
            .iter()
            .copied()
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .unwrap();
        assert_eq!(hit(&scene, apex + vec2(0.0, 2.0)), Some(7));
        // Over a faint shot only (set a shares the bright paths, so pick an
        // empty spot): nothing.
        assert_eq!(
            hit(
                &scene,
                pos2(scene.plot.right() - 2.0, scene.plot.top() + 2.0)
            ),
            None
        );
        // Faint shots are never hit, even on their own dots.
        let faint = scene
            .traces
            .iter()
            .find(|t| t.level == Level::Faint)
            .unwrap();
        let only_faint = Scene {
            traces: vec![Trace {
                index: faint.index,
                level: Level::Faint,
                colour: faint.colour,
                flight: faint.flight.clone(),
                ground: faint.ground.clone(),
                rolled: faint.rolled,
                dots: faint.dots.clone(),
                bounds: faint.bounds,
            }],
            ticks: Vec::new(),
            ..build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric))
        };
        assert_eq!(hit(&only_faint, faint.dots[0]), None);
    }

    #[test]
    fn paths_are_smooth_curves_through_their_samples() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        let p = path(&shots[7]).unwrap();
        let gold = scene
            .traces
            .iter()
            .find(|t| t.level == Level::Gold)
            .unwrap();
        let k = per_metre(UnitSystem::Metric);
        for (curve, samples) in [(&gold.flight, &p.flight), (&gold.ground, &p.ground)] {
            for q in samples {
                let [d, h] = side_point(q, k);
                let at = scene.proj.to_screen(d, h);
                assert!(curve.points.contains(&at), "{q:?} on the curve");
            }
        }
        // Sub-points between the samples, none above the apex sample.
        assert!(gold.flight.points.len() > p.flight.len() * 4);
        let apex = scene.proj.y.map(30.0);
        assert!(gold.flight.points.iter().all(|q| q.y >= apex));
        // The landing and rest dots are the curves' ends.
        assert_eq!(gold.dots[0], *gold.flight.points.last().unwrap());
        assert_eq!(gold.dots[1], *gold.ground.points.last().unwrap());
    }

    #[test]
    fn draw_on_cuts_the_gold_path_by_time() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        let gold = scene
            .traces
            .iter()
            .find(|t| t.level == Level::Gold)
            .unwrap();
        // 7 s in all: at 0.5 the ball is in the air, between sub-points.
        let mid = DrawOn::new(&scene, 0.5).unwrap();
        assert_eq!(mid.t, 3.5);
        let n = mid.flight.len() - 1;
        assert!(n > 4 && n < gold.flight.points.len());
        assert_eq!(mid.flight[..n], gold.flight.points[..n]);
        assert!(mid.ground.is_empty());
        // Near the end it is rolling; the whole path at 1.
        let late = DrawOn::new(&scene, 0.95).unwrap();
        assert_eq!(late.flight, gold.flight.points);
        assert!(!late.ground.is_empty());
        let end = DrawOn::new(&scene, 1.0).unwrap();
        assert_eq!(end.flight, gold.flight.points);
        assert_eq!(end.ground, gold.ground.points);
        // No gold path, no draw-on.
        let (mut shots, g) = history();
        shots[7] = no_path(8, 120.0, 130.0);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        assert!(DrawOn::new(&scene, 0.5).is_none());
    }

    #[test]
    fn ground_balls_roll_along_the_ground_from_the_tee() {
        let (shots, g) = history();
        let mut shots: Vec<ShotRow> = shots
            .iter()
            .map(|s| ground_ball(s.shot_number, &s.actor, s.group.as_deref().unwrap(), 4.0))
            .collect();
        shots[5] = ground_ball(6, "mevo.0", "b", 2.5);
        shots[7] = ground_ball(8, "mevo.0", "b", 5.0);
        assert!(path(&shots[7]).is_some_and(rolls));
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        // A ground-ball-only set: a short distance axis (5 m x 1.06 -> 10),
        // and the height axis keeps its floor.
        assert_eq!(domain(&scene), (10.0, 10.0));
        assert_eq!(
            scene
                .x_ticks
                .iter()
                .map(|(_, s)| s.as_str())
                .collect::<Vec<_>>(),
            ["0", "5", "10"]
        );
        let imperial = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Imperial));
        assert_eq!(domain(&imperial), (10.0, 30.0));

        // Every shot is a line on the ground from the tee to rest, no ticks.
        assert!(scene.ticks.is_empty());
        let ground_y = scene.proj.y.map(0.0);
        let tee = scene.proj.to_screen(0.0, 0.0);
        let trace = |i: usize| scene.traces.iter().find(|t| t.index == i).unwrap();
        for t in &scene.traces {
            assert!(t.rolled);
            assert_eq!(t.flight.points, vec![tee]);
            assert_eq!(t.ground.points.first(), Some(&tee));
            assert!(
                t.ground
                    .points
                    .iter()
                    .all(|q| (q.y - ground_y).abs() < 1e-3)
            );
            // The rest dot only: no landing dot at the tee.
            assert_eq!(t.dots.len(), 1);
            assert_eq!(t.dots.last(), t.ground.points.last());
        }
        let gold = trace(7);
        assert_eq!(gold.level, Level::Gold);
        let rest = scene.proj.x.map(5.0_f64.hypot(0.2));
        assert!((gold.dots[0].x - rest).abs() < 1e-3);
        assert_eq!(trace(5).level, Level::Bright);
        assert_eq!(trace(2).level, Level::Faint);
        // The gold shot is labelled with its total only.
        assert!(scene.marks.apex.is_none() && scene.marks.carry.is_none());
        let (at, text, t) = scene.marks.total.as_ref().unwrap();
        assert_eq!((text.as_str(), *t), ("5", 4.0));
        assert!((at.x - rest).abs() < 1e-3);
        assert_eq!(at.y, ground_y);
        // The header: 0 apex and carry, the total.
        assert_eq!(
            scene.shown,
            Readings {
                carry: Some(0.0),
                total: Some(5.0),
                apex: Some(0.0)
            }
        );

        // Hover and click: a bright roll's rest dot and its line.
        let bright = trace(5);
        assert_eq!(hit(&scene, bright.dots[0] + vec2(2.0, -2.0)), Some(5));
        let along = scene.proj.to_screen(1.0, 0.0);
        assert_eq!(hit(&scene, along + vec2(0.0, -2.0)), Some(7), "gold on top");

        // The draw-on runs along the roll from the tee.
        let mid = DrawOn::new(&scene, 0.5).unwrap();
        assert_eq!(mid.t, 2.0);
        assert_eq!(mid.flight, vec![tee]);
        assert_eq!(mid.ground.first(), Some(&tee));
        let head = *mid.ground.last().unwrap();
        assert!(head.x > tee.x && head.x < rest);
        let end = DrawOn::new(&scene, 1.0).unwrap();
        assert_eq!(end.ground, gold.ground.points);
    }

    #[test]
    fn a_ground_ball_among_full_shots_keeps_the_full_scale() {
        let (mut shots, g) = history();
        shots[7] = ground_ball(8, "mevo.0", "b", 5.0);
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        assert_eq!(domain(&scene), (175.0, 40.0));
        let gold = scene
            .traces
            .iter()
            .find(|t| t.level == Level::Gold)
            .unwrap();
        assert!(gold.rolled);
        assert_eq!(gold.ground.points.len(), 3);
        assert!(scene.ticks.iter().all(|t| t.index != 7));
    }

    #[test]
    fn a_shown_shot_with_nothing_to_draw_yet_keeps_the_short_axis() {
        let (shots, g) = history();
        let mut shots: Vec<ShotRow> = shots
            .iter()
            .map(|s| ground_ball(s.shot_number, &s.actor, s.group.as_deref().unwrap(), 4.0))
            .collect();
        // #8 before its ball flight arrives: nothing to draw.
        shots[7].ball = None;
        shots[7].derived = None;
        assert!(!drawable(&shots[7]));
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        assert_eq!(domain(&scene).0, 10.0);
    }

    #[test]
    fn the_scene_counts_the_members_the_device_filter_leaves() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        assert_eq!(scene.members, 3);
        // A filter on a device with no shot in the set: no members, so no
        // note blaming the shot.
        let m = members(&shots, &g, ShotGrouping::BySet, 7, Some("r10.0"));
        let scene = build_scene(&SceneInput {
            device: Some("r10.0"),
            ..input(&shots, &g, &m, 7, &[], UnitSystem::Metric)
        });
        assert_eq!(scene.members, 0);
        assert!(scene.traces.iter().all(|t| t.level == Level::Faint));
    }

    #[test]
    fn a_path_is_hit_only_near_its_bounds() {
        let (shots, g) = history();
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        for t in &scene.traces {
            let all = t.flight.points.iter().chain(&t.ground.points);
            assert!(all.into_iter().all(|p| t.bounds.contains(*p)));
        }
        let gold = scene
            .traces
            .iter()
            .find(|t| t.level == Level::Gold)
            .unwrap();
        let mid = gold.flight.points[gold.flight.points.len() / 2];
        assert_eq!(hit(&scene, mid + vec2(0.0, -2.0)), Some(7));
        let outside = pos2(gold.bounds.right() + LINE_HIT + 1.0, gold.bounds.top());
        assert_eq!(hit(&scene, outside), None);
    }

    #[test]
    fn a_one_point_flight_without_a_roll_gets_ticks() {
        let (mut shots, g) = history();
        shots[7] = ground_ball(8, "mevo.0", "b", 5.0);
        let d = shots[7].derived.as_mut().unwrap();
        d.path.as_mut().unwrap().ground.clear();
        assert!(path(&shots[7]).is_none());
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        let gold: Vec<&Tick> = scene.ticks.iter().filter(|t| t.index == 7).collect();
        assert_eq!(gold.len(), 2, "carry 0 and total");
        assert!(scene.traces.iter().all(|t| t.index != 7));
    }

    #[test]
    fn tooltip_names_the_shot() {
        let (shots, _) = history();
        let text = tooltip(&shots[7], Some("Mevo+"), UnitSystem::Imperial);
        assert_eq!(
            text,
            "#8 \u{b7} Mevo+ \u{b7} 7I\ncarry 153 yd \u{b7} total 164 yd \u{b7} apex 98 ft"
        );
        let text = tooltip(&no_path(9, 120.0, 130.0), None, UnitSystem::Metric);
        assert_eq!(text, "#9 \u{b7} 7I\ncarry 120 m \u{b7} total 130 m");
    }

    #[test]
    fn scene_rebuilds_only_when_its_key_changes() {
        let (shots, g) = history();
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        app.shots = shots;
        app.shots_rev = 1;
        app.groups = g;
        app.refresh_shot_caches();
        let size = vec2(800.0, 240.0);
        let mut key = app.flight_key(7, size);
        app.refresh_flight_scene(&key, 7);
        assert!(app.flight_scene.peek(&key).is_some());
        // An idle frame, sub-point jitter included, keeps the scene.
        assert_eq!(app.flight_key(7, size + vec2(0.3, -0.2)), key);

        // Each input invalidates it; the rebuilt scene follows the change.
        fn changed(
            app: &mut FlighthookApp,
            key: &mut ChartKey,
            shown: usize,
            size: Vec2,
            what: &str,
        ) {
            let next = app.flight_key(shown, size);
            assert_ne!(next, *key, "{what}");
            assert!(app.flight_scene.peek(&next).is_none(), "{what}");
            app.refresh_flight_scene(&next, shown);
            assert!(app.flight_scene.peek(&next).is_some(), "{what}");
            *key = next;
        }
        app.units_toggle = UnitSystem::Metric;
        changed(&mut app, &mut key, 7, size, "units");
        let apex = &app.flight_scene.peek(&key).unwrap().marks.apex;
        assert!(apex.as_ref().unwrap().1.ends_with(" m"));
        changed(&mut app, &mut key, 6, size, "shown shot");
        assert_eq!(gold(app.flight_scene.peek(&key).unwrap()), Some(6));
        app.device_filter = Some("mevo.0".into());
        changed(&mut app, &mut key, 6, size, "device filter");
        assert_eq!(gold(app.flight_scene.peek(&key).unwrap()), None);
        app.device_filter = None;
        changed(&mut app, &mut key, 6, size, "device filter cleared");
        changed(&mut app, &mut key, 6, vec2(900.0, 240.0), "panel width");
        changed(&mut app, &mut key, 6, vec2(900.0, 300.0), "panel height");
        app.shots.push(shot(9, "mevo.0", "b"));
        app.shots_rev += 1;
        changed(&mut app, &mut key, 6, vec2(900.0, 300.0), "new shot");
        app.groups.mode = ShotGrouping::ByClub;
        changed(&mut app, &mut key, 6, vec2(900.0, 300.0), "view");
        app.groups.new_session();
        changed(&mut app, &mut key, 6, vec2(900.0, 300.0), "new session");
        assert!(
            app.flight_scene
                .peek(&key)
                .unwrap()
                .traces
                .iter()
                .all(|t| t.level != Level::Faint),
            "nothing left in the new session to draw faint"
        );
    }

    /// A decimated 7-iron path as the server sends it (15 flight points,
    /// three hops then the roll), scaled to land at `carry` metres with an
    /// apex of `apex` metres, `lateral` metres right at the rest.
    pub(in crate::panels) fn realistic_path(carry: f64, apex: f64, lateral: f64) -> FlightPath {
        const FLIGHT: [[f64; 4]; 15] = [
            [0.0, 0.0, 0.0, 0.0],
            [0.332, 19.03, 5.63, 0.08],
            [1.146, 56.32, 17.48, 0.25],
            [1.545, 71.16, 21.69, 0.33],
            [2.077, 88.63, 25.35, 0.43],
            [2.326, 96.12, 26.25, 0.47],
            [2.567, 102.96, 26.54, 0.51],
            [2.849, 110.53, 26.14, 0.56],
            [3.131, 117.68, 24.96, 0.6],
            [3.422, 124.66, 22.97, 0.65],
            [3.721, 131.51, 20.13, 0.69],
            [4.028, 138.22, 16.42, 0.74],
            [4.344, 144.81, 11.82, 0.78],
            [4.668, 151.3, 6.35, 0.82],
            [5.0, 157.71, 0.0, 0.87],
        ];
        const GROUND: [[f64; 4]; 7] = [
            [5.0, 157.71, 0.0, 0.87],
            [5.38, 159.41, 0.72, 0.88],
            [5.766, 161.12, 0.0, 0.89],
            [5.936, 161.88, 0.14, 0.9],
            [6.104, 162.62, 0.0, 0.9],
            [6.204, 163.05, 0.05, 0.91],
            [10.455, 173.48, 0.0, 0.99],
        ];
        let scale = |q: &[f64; 4]| {
            [
                q[0],
                q[1] * carry / 157.71,
                q[2] * apex / 26.54,
                q[3] * lateral / 0.99,
            ]
        };
        FlightPath {
            flight: FLIGHT.iter().map(scale).collect(),
            ground: GROUND.iter().map(scale).collect(),
        }
    }

    /// Frame time of the Shots tab (Cards, 1600×900, both panels) with 1000
    /// shots in the session, the pointer moving over the Flight panel, and
    /// the time to rebuild its scene (smoothing every path). Run with
    /// `cargo test --release -p flighthook-ui frame_time -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, not a check"]
    fn frame_time_with_1000_shots() {
        use std::time::Instant;
        let list: Vec<ShotGroupInfo> = (0..20)
            .map(|n| set(&format!("s{n}"), n + 1, Club::Iron7, 0))
            .collect();
        let g = groups(list);
        let shots: Vec<ShotRow> = (0..1000_u32)
            .map(|n| {
                let carry = 130.0 + f64::from(n % 17);
                let mut s = shot(n, "mevo.0", &format!("s{}", n / 50));
                s.derived = Some(derived(
                    Some(realistic_path(carry, 28.0, f64::from(n % 11) - 5.0)),
                    carry,
                    carry + 12.0,
                    28.0,
                ));
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
                        400.0 + (n % 50) as f32 * 16.0,
                        760.0,
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
        let key = app.flight_key(999, vec2(1000.0, 256.0));
        let start = Instant::now();
        app.flight_scene = chart::Cached::default();
        app.refresh_flight_scene(&key, 999);
        let rebuild = start.elapsed();
        let scene = app.flight_scene.peek(&key).unwrap();
        let faint = scene
            .traces
            .iter()
            .filter(|t| t.level == Level::Faint)
            .count();
        let bright = scene.traces.len() - faint;
        let points: usize = scene
            .traces
            .iter()
            .map(|t| t.flight.points.len() + t.ground.points.len())
            .sum();
        println!(
            "1000 shots: frame {frame:?} (run + tessellate, mean of {frames}); \
             Flight scene rebuild {rebuild:?}; {bright} bright + {faint} faint paths, \
             {points} curve points"
        );
    }

    /// Text sizes for the label tests: a 14 pt "88 ft", "112", "115".
    const SIZES: [Vec2; 3] = [vec2(38.0, 17.0), vec2(24.0, 17.0), vec2(24.0, 17.0)];

    fn label_plot() -> Rect {
        Rect::from_min_max(pos2(34.0, 28.0), pos2(600.0, 200.0))
    }

    #[test]
    fn labels_sit_centred_above_their_marks() {
        let plot = label_plot();
        let (apex, carry, total) = (pos2(300.0, 60.0), pos2(400.0, 200.0), pos2(470.0, 200.0));
        let [a, c, t] = place_marks([Some(apex), Some(carry), Some(total)], SIZES, plot);
        for (r, at, rise, size) in [
            (a.unwrap(), apex, APEX_RISE, SIZES[0]),
            (c.unwrap(), carry, MARK_RISE, SIZES[1]),
            (t.unwrap(), total, MARK_RISE, SIZES[2]),
        ] {
            assert!((r.center().x - at.x).abs() < 1e-3, "{r:?}");
            assert!((r.bottom() - (at.y - rise)).abs() < 1e-3, "{r:?}");
            assert_eq!(r.size(), size + 2.0 * MARK_PAD);
        }
    }

    #[test]
    fn carry_lifts_when_it_would_meet_total() {
        let plot = label_plot();
        // A PW: carry 112, total 115, a few points apart.
        for (carry_x, total_x) in [(500.0, 512.0), (512.0, 500.0), (500.0, 500.0)] {
            let points = [None, Some(pos2(carry_x, 200.0)), Some(pos2(total_x, 200.0))];
            let [_, c, t] = place_marks(points, SIZES, plot);
            let (c, t) = (c.unwrap(), t.unwrap());
            // Total stays on its dot; carry is a row up, still centred.
            assert!((t.bottom() - (200.0 - MARK_RISE)).abs() < 1e-3);
            assert!((c.bottom() - (t.top() - LIFT_GAP)).abs() < 1e-3);
            assert!((c.center().x - carry_x).abs() < 1e-3);
            assert!(!c.intersects(t), "{carry_x} {total_x}");
        }
        // Far enough apart: one row.
        let points = [None, Some(pos2(400.0, 200.0)), Some(pos2(470.0, 200.0))];
        let [_, c, t] = place_marks(points, SIZES, plot);
        assert_eq!(c.unwrap().bottom(), t.unwrap().bottom());
    }

    #[test]
    fn labels_stay_inside_the_plot_and_apex_clears_the_others() {
        let plot = label_plot();
        // Near the right edge.
        let points = [None, Some(pos2(560.0, 200.0)), Some(pos2(598.0, 200.0))];
        let [_, c, t] = place_marks(points, SIZES, plot);
        for r in [c.unwrap(), t.unwrap()] {
            assert!(
                r.left() >= plot.left() && r.right() <= plot.right() + 1e-3,
                "{r:?}"
            );
        }
        assert!(!c.unwrap().intersects(t.unwrap()));
        // A flat shot: the apex just above the ground among both labels.
        let points = [
            Some(pos2(450.0, 190.0)),
            Some(pos2(440.0, 200.0)),
            Some(pos2(462.0, 200.0)),
        ];
        let [a, c, t] = place_marks(points, SIZES, plot);
        let (a, c, t) = (a.unwrap(), c.unwrap(), t.unwrap());
        assert!(!a.intersects(c) && !a.intersects(t));
        assert!(a.bottom() <= c.top().min(t.top()));
        // A ground ball: the total alone.
        let [a, c, t] = place_marks([None, None, Some(pos2(60.0, 200.0))], SIZES, plot);
        assert!(a.is_none() && c.is_none());
        assert!(t.unwrap().left() >= plot.left());
    }

    #[test]
    fn the_plot_keeps_room_above_the_apex_for_its_label() {
        // A nice domain already roomy enough is kept.
        assert_eq!(headroom(120.0, Some(88.0), 172.0), 120.0);
        assert_eq!(headroom(120.0, None, 172.0), 120.0);
        // An apex near the top raises it just enough.
        for plot_h in [136.0_f32, 172.0, 260.0] {
            let top = 0.97 * 100.0;
            let hi = headroom(100.0, Some(top), plot_h);
            let room = f64::from(plot_h) * (1.0 - top / hi);
            assert!(room >= f64::from(MARK_ROOM) - 1e-6, "{plot_h}: {room}");
        }
        // In a scene: the gold apex sits MARK_ROOM under the top at least.
        let (mut shots, g) = history();
        shots[7].derived.as_mut().unwrap().path = Some(flight_path(140.0, 39.5, 150.0));
        let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
        let scene = build_scene(&input(&shots, &g, &m, 7, &[], UnitSystem::Metric));
        let (apex, _, _) = scene.marks.apex.as_ref().unwrap();
        assert!(
            apex.y - scene.plot.top() >= MARK_ROOM - 0.01,
            "{apex:?} {:?}",
            scene.plot
        );
        // The axis ticks keep their step.
        let steps: Vec<f32> = scene.y_ticks.windows(2).map(|w| w[0].0 - w[1].0).collect();
        assert!(steps.windows(2).all(|w| (w[0] - w[1]).abs() < 0.01));
        // A label's box is no taller than MARK_BOX_H, so the margin above
        // the apex label is real.
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            for text in ["31 yd", "163", "Apex 104 ft"] {
                let galley = painter.layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::proportional(MARK_FONT),
                    theme::GOLD,
                );
                let box_h = galley.size().y + 2.0 * MARK_PAD.y;
                assert!(box_h <= MARK_BOX_H + 1e-3, "{text}: {box_h}");
            }
        });
    }
}
