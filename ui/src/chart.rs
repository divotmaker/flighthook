//! Chart maths for the Flight (side) and Dispersion (overhead) views and the
//! range bars: axis domains and ticks, chart-to-screen projections, the
//! dispersion ellipse, smooth path curves and their draw-on prefix,
//! hit-testing, the device
//! palette, the members of the view (which shots are bright) and the
//! caches keyed on them.
//!
//! Pure functions, no painting. Chart coordinates are in display units
//! (yd/ft or m) with the vertical axis pointing up; screen coordinates are
//! egui points with y pointing down.

use egui::{Color32, Pos2, Rect};

use crate::groups::ShotGroups;
use crate::theme;
use crate::types::{Club, FlightPath, ShotGroupInfo, ShotGrouping, ShotRow, UnitSystem};

// ---------------------------------------------------------------------------
// Domains and ticks
// ---------------------------------------------------------------------------

/// Smallest multiple of `step` that is at least `v`. Zero, negative and
/// non-finite values give 0.
#[must_use]
pub(crate) fn nice_ceil(v: f64, step: f64) -> f64 {
    if !v.is_finite() || v <= 0.0 || step <= 0.0 {
        return 0.0;
    }
    (v / step).ceil() * step
}

/// How one chart axis sizes its domain from the data it shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AxisSpec {
    /// Headroom factor applied to the data maximum.
    pub(crate) margin: f64,
    /// Tick spacing; the domain end is rounded up to a multiple of it.
    pub(crate) step: f64,
    /// Smallest domain end.
    pub(crate) floor: f64,
}

impl AxisSpec {
    /// Domain end for data reaching `data_max`: `nice_ceil(margin ×
    /// data_max, step)`, at least `floor`. `data_max` is capped at
    /// [`MAX_EXTENT`].
    #[must_use]
    pub(crate) fn domain_max(&self, data_max: f64) -> f64 {
        let data_max = if data_max.is_finite() {
            data_max.min(MAX_EXTENT)
        } else {
            0.0
        };
        nice_ceil(self.margin * data_max, self.step).max(self.floor)
    }
}

/// Largest distance or height, in display units, that sizes a chart axis.
/// A shot reaching further is malformed; it is left out of the axis sizing
/// and drawn off the chart.
pub(crate) const MAX_EXTENT: f64 = 2000.0;

/// Whether `v` is a finite value within [`MAX_EXTENT`] of the origin, and
/// so may size a chart axis.
#[must_use]
pub(crate) fn plausible(v: f64) -> bool {
    v.is_finite() && v.abs() <= MAX_EXTENT
}

/// Most ticks [`ticks`] returns; a wider range gives none.
pub(crate) const MAX_TICKS: f64 = 1000.0;

/// Largest magnitude of a scale's domain end; ends beyond it are clamped.
const MAX_DOMAIN: f64 = 1e9;

/// Furthest a mapped value lands from the screen span, in multiples of the
/// span: a value beyond the domain is drawn off the chart at a finite
/// screen coordinate.
const MAX_OVERSHOOT: f64 = 1000.0;

/// Side view, horizontal: path distance from the tee (yd or m).
pub(crate) const SIDE_DISTANCE: AxisSpec = AxisSpec {
    margin: 1.06,
    step: 25.0,
    floor: 50.0,
};

/// Side view, horizontal, when every shot sizing it is a ground ball: a
/// roll of a few yards or metres fills the plot.
pub(crate) const SIDE_DISTANCE_SHORT: AxisSpec = AxisSpec {
    margin: 1.06,
    step: 5.0,
    floor: 10.0,
};

/// Side view, vertical: height above the tee, in the apex unit (ft
/// imperial, m metric).
#[must_use]
pub(crate) fn side_height(units: UnitSystem) -> AxisSpec {
    match units {
        UnitSystem::Imperial => AxisSpec {
            margin: 1.15,
            step: 20.0,
            floor: 30.0,
        },
        UnitSystem::Metric => AxisSpec {
            margin: 1.15,
            step: 10.0,
            floor: 10.0,
        },
    }
}

/// Overhead view, vertical: downrange distance from the tee.
pub(crate) const OVERHEAD_DOWNRANGE: AxisSpec = AxisSpec {
    margin: 1.08,
    step: 25.0,
    floor: 50.0,
};

/// Overhead view, horizontal: half-width of the lateral domain, from the
/// largest absolute lateral.
pub(crate) const OVERHEAD_LATERAL: AxisSpec = AxisSpec {
    margin: 1.3,
    step: 5.0,
    floor: 15.0,
};

/// Tick spacing for an axis drawn at `px_per_unit`: `step`, doubled until
/// neighbouring ticks are at least `min_px` apart. A zero, negative or
/// non-finite `step` or `px_per_unit` gives `f64::INFINITY`: no ticks.
#[must_use]
pub(crate) fn tick_step(step: f64, px_per_unit: f32, min_px: f32) -> f64 {
    if !step.is_finite() || step <= 0.0 || !px_per_unit.is_finite() || px_per_unit <= 0.0 {
        return f64::INFINITY;
    }
    let mut s = step;
    // Bounded: `s` reaches infinity after at most about 2000 doublings.
    while s.is_finite() && s * f64::from(px_per_unit) < f64::from(min_px) {
        s *= 2.0;
    }
    s
}

/// Multiples of `step` in `lo..=hi`, ascending. None when `step` is zero,
/// negative or non-finite, when either end is non-finite, when the range
/// holds more than [`MAX_TICKS`] of them, or when an end is too many steps
/// from 0 to count them exactly.
#[must_use]
pub(crate) fn ticks(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    if !step.is_finite() || step <= 0.0 || !lo.is_finite() || !hi.is_finite() || hi < lo {
        return Vec::new();
    }
    // 2^53: the largest step count an `f64` holds exactly.
    const MAX_INDEX: f64 = 9_007_199_254_740_992.0;
    if (hi - lo) / step > MAX_TICKS || lo.abs().max(hi.abs()) / step > MAX_INDEX {
        return Vec::new();
    }
    let first = (lo / step).ceil() as i64;
    let last = (hi / step).floor() as i64;
    (first..=last).map(|i| i as f64 * step).collect()
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// Linear map from a chart domain `lo..=hi` to a screen span `from..=to`
/// (`from` may be greater than `to`, which flips the axis).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scale {
    lo: f64,
    hi: f64,
    from: f32,
    to: f32,
}

impl Scale {
    /// A scale mapping `lo` to `from` and `hi` to `to`. Domain ends are
    /// clamped to ±1e9 (a non-finite one is taken as 0), and an empty domain
    /// is widened to one unit; a non-finite screen end is taken as 0.
    #[must_use]
    pub(crate) fn new(lo: f64, hi: f64, from: f32, to: f32) -> Self {
        let end = |v: f64| {
            if v.is_finite() {
                v.clamp(-MAX_DOMAIN, MAX_DOMAIN)
            } else {
                0.0
            }
        };
        let screen = |v: f32| if v.is_finite() { v } else { 0.0 };
        let (lo, hi) = (end(lo), end(hi));
        let hi = if hi > lo { hi } else { lo + 1.0 };
        Self {
            lo,
            hi,
            from: screen(from),
            to: screen(to),
        }
    }

    /// Screen coordinate of the chart value `v`, always finite: a value far
    /// outside the domain lands off the chart, at most 1000 spans away, and
    /// NaN maps to `lo`.
    #[must_use]
    pub(crate) fn map(&self, v: f64) -> f32 {
        let t = (v - self.lo) / (self.hi - self.lo);
        let t = if t.is_nan() {
            0.0
        } else {
            t.clamp(-MAX_OVERSHOOT, MAX_OVERSHOOT)
        };
        self.from + (f64::from(self.to - self.from) * t) as f32
    }

    /// Screen points per chart unit.
    #[must_use]
    pub(crate) fn px_per_unit(&self) -> f32 {
        ((f64::from(self.to - self.from)).abs() / (self.hi - self.lo)) as f32
    }

    /// The chart domain, `(lo, hi)`.
    #[must_use]
    pub(crate) fn domain(&self) -> (f64, f64) {
        (self.lo, self.hi)
    }
}

/// Maps chart points (horizontal, vertical-up) into a screen rect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Projection {
    pub(crate) x: Scale,
    pub(crate) y: Scale,
}

impl Projection {
    /// Independent scales: `x_lo..=x_hi` across the rect, `y_lo..=y_hi` from
    /// its bottom edge to its top edge.
    #[must_use]
    pub(crate) fn fit(rect: Rect, (x_lo, x_hi): (f64, f64), (y_lo, y_hi): (f64, f64)) -> Self {
        Self {
            x: Scale::new(x_lo, x_hi, rect.left(), rect.right()),
            y: Scale::new(y_lo, y_hi, rect.bottom(), rect.top()),
        }
    }

    /// Equal scales on both axes with the origin at the bottom centre of the
    /// rect: `-x_half..=x_half` and `0..=y_max` both fit, and the axis with
    /// room to spare shows a wider domain.
    #[must_use]
    pub(crate) fn equal(rect: Rect, x_half: f64, y_max: f64) -> Self {
        let size = |v: f32| {
            if v.is_finite() {
                f64::from(v.max(1.0))
            } else {
                1.0
            }
        };
        let (w, h) = (size(rect.width()), size(rect.height()));
        // Non-finite or out-of-range extents are clamped, so `px` and the
        // domains it gives stay finite and positive.
        let extent = |v: f64| {
            if v.is_finite() {
                v.clamp(1e-6, MAX_DOMAIN)
            } else {
                1.0
            }
        };
        let (x_half, y_max) = (extent(x_half), extent(y_max));
        let px = (h / y_max).min(w / (2.0 * x_half));
        let half = w / 2.0 / px;
        Self {
            x: Scale::new(-half, half, rect.left(), rect.right()),
            y: Scale::new(0.0, h / px, rect.bottom(), rect.top()),
        }
    }

    /// Screen point of the chart point `(x, y)`.
    #[must_use]
    pub(crate) fn to_screen(self, x: f64, y: f64) -> Pos2 {
        Pos2::new(self.x.map(x), self.y.map(y))
    }
}

// ---------------------------------------------------------------------------
// Dispersion ellipse
// ---------------------------------------------------------------------------

/// Size factor of the 2σ dispersion ellipse: a normal pattern fitted to the
/// points has about 86 % of its mass inside.
pub(crate) const ELLIPSE_K_2SIGMA: f64 = 2.0;

/// Size factor of the 95 % dispersion ellipse, `√χ²₂(0.95)`.
pub(crate) const ELLIPSE_K_95: f64 = 2.448;

/// Share of a 2D normal pattern inside the covariance ellipse of size
/// factor `k`, in percent: `1 − e^(−k²/2)`.
#[must_use]
pub(crate) fn ellipse_coverage_pct(k: f64) -> f64 {
    100.0 * (1.0 - (-k * k / 2.0).exp())
}

/// Smallest semi-axis, in display units (1 yd imperial, 1 m metric), so a
/// collinear or coincident set still draws.
pub(crate) const ELLIPSE_MIN_SEMI_AXIS: f64 = 1.0;

/// Fewest points that get an ellipse.
pub(crate) const ELLIPSE_MIN_POINTS: usize = 3;

/// Vertices of a drawn ellipse polygon.
pub(crate) const ELLIPSE_SEGMENTS: usize = 48;

/// Mean and sample covariance (`n − 1`) of a set of 2D points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Covariance {
    pub(crate) n: usize,
    pub(crate) mean: [f64; 2],
    pub(crate) xx: f64,
    pub(crate) yy: f64,
    pub(crate) xy: f64,
}

/// Mean and sample covariance of the finite points in `points`; `None` with
/// fewer than two.
#[must_use]
pub(crate) fn covariance(points: &[[f64; 2]]) -> Option<Covariance> {
    let finite = || {
        points
            .iter()
            .filter(|p| p[0].is_finite() && p[1].is_finite())
    };
    let n = finite().count();
    if n < 2 {
        return None;
    }
    let nf = n as f64;
    let (sx, sy) = finite().fold((0.0, 0.0), |(sx, sy), p| (sx + p[0], sy + p[1]));
    let mean = [sx / nf, sy / nf];
    let (mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0);
    for p in finite() {
        let (dx, dy) = (p[0] - mean[0], p[1] - mean[1]);
        xx += dx * dx;
        yy += dy * dy;
        xy += dx * dy;
    }
    let d = nf - 1.0;
    Some(Covariance {
        n,
        mean,
        xx: xx / d,
        yy: yy / d,
        xy: xy / d,
    })
}

/// A dispersion ellipse in chart coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Ellipse {
    /// Mean of the points.
    pub(crate) center: [f64; 2],
    /// Semi-axis along `angle`, the longer one.
    pub(crate) semi_major: f64,
    /// Semi-axis across `angle`.
    pub(crate) semi_minor: f64,
    /// Direction of the major axis, radians counter-clockwise from the
    /// chart's +x axis (chart y up), in `-π/2..=π/2`.
    pub(crate) angle: f64,
    /// Points the ellipse was fitted to.
    pub(crate) n: usize,
}

/// The covariance ellipse of size factor `k` ([`ELLIPSE_K_2SIGMA`] or
/// [`ELLIPSE_K_95`]) of the finite points in `points`: centred on their
/// mean, axes along the eigenvectors of their sample covariance, semi-axes
/// `k · √λ`, each at least
/// [`ELLIPSE_MIN_SEMI_AXIS`]. `None` with fewer than
/// [`ELLIPSE_MIN_POINTS`] finite points, or when the points are too large
/// for a finite ellipse.
#[must_use]
pub(crate) fn ellipse_from_points(points: &[[f64; 2]], k: f64) -> Option<Ellipse> {
    let c = covariance(points)?;
    if c.n < ELLIPSE_MIN_POINTS {
        return None;
    }
    let half_trace = (c.xx + c.yy) / 2.0;
    let root = (((c.xx - c.yy) / 2.0).powi(2) + c.xy * c.xy).sqrt();
    let major = (half_trace + root).max(0.0);
    let minor = (half_trace - root).max(0.0);
    let angle = if root > 0.0 {
        0.5 * (2.0 * c.xy).atan2(c.xx - c.yy)
    } else {
        0.0
    };
    let e = Ellipse {
        center: c.mean,
        semi_major: (k * major.sqrt()).max(ELLIPSE_MIN_SEMI_AXIS),
        semi_minor: (k * minor.sqrt()).max(ELLIPSE_MIN_SEMI_AXIS),
        angle,
        n: c.n,
    };
    // Points too large to square overflow the covariance.
    [
        e.center[0],
        e.center[1],
        e.semi_major,
        e.semi_minor,
        e.angle,
    ]
    .iter()
    .all(|v| v.is_finite())
    .then_some(e)
}

/// `segments` points evenly spaced around `e`, counter-clockwise from the
/// positive end of the major axis. The polygon closes from the last point
/// back to the first.
#[must_use]
pub(crate) fn ellipse_points(e: &Ellipse, segments: usize) -> Vec<[f64; 2]> {
    let (sin, cos) = e.angle.sin_cos();
    (0..segments)
        .map(|i| {
            let t = std::f64::consts::TAU * i as f64 / segments as f64;
            let (u, v) = (e.semi_major * t.cos(), e.semi_minor * t.sin());
            [
                e.center[0] + u * cos - v * sin,
                e.center[1] + u * sin + v * cos,
            ]
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Smooth curves and the draw-on
// ---------------------------------------------------------------------------

/// Height (m) at or below which a path point is on the ground: where a hop
/// ends and the next one starts, or a point of the roll.
pub(crate) const ON_GROUND_M: f64 = 0.005;
/// Longest sub-segment (points) a spline segment is cut into, up to
/// [`SPLINE_MAX_STEPS`] of them.
const SPLINE_STEP_PX: f32 = 6.0;
/// About the furthest (points) a sub-segment strays from the true curve.
const SPLINE_TOLERANCE_PX: f32 = 0.25;
/// Most sub-segments per spline segment.
pub(crate) const SPLINE_MAX_STEPS: usize = 12;
/// Points closer than this (points) are one point to the spline.
const SPLINE_SAME_PX: f32 = 0.01;

/// A timed screen polyline: each point with the path time (s) it is
/// reached at, ascending.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Curve {
    pub(crate) points: Vec<Pos2>,
    pub(crate) times: Vec<f64>,
}

impl Curve {
    fn push(&mut self, p: Pos2, t: f64) {
        self.points.push(p);
        self.times.push(t);
    }

    /// Time of the last point.
    #[must_use]
    pub(crate) fn end_time(&self) -> Option<f64> {
        self.times.last().copied()
    }

    /// The part drawn by time `t`: every point up to `t`, plus a point
    /// interpolated at `t` when it falls between two. Before the first
    /// point (or for a NaN `t`), the first point alone.
    #[must_use]
    pub(crate) fn prefix(&self, t: f64) -> Vec<Pos2> {
        let (Some(&first), Some(&t0)) = (self.points.first(), self.times.first()) else {
            return Vec::new();
        };
        if t.is_nan() || t <= t0 {
            return vec![first];
        }
        let end = self.times.partition_point(|&ti| ti <= t);
        let mut out = self.points[..end].to_vec();
        if end > 0
            && let (Some(&b), Some(&tb)) = (self.points.get(end), self.times.get(end))
        {
            let (a, ta) = (self.points[end - 1], self.times[end - 1]);
            if t > ta && tb > ta {
                out.push(a.lerp(b, ((t - ta) / (tb - ta)) as f32));
            }
        }
        out
    }
}

/// A smooth curve through `points` (screen points, each with its time): a
/// centripetal Catmull-Rom spline, which has no cusps or loops, through
/// every point. Each segment is cut into as many sub-segments as keep the
/// polyline within [`SPLINE_TOLERANCE_PX`] of the curve, none shorter than
/// about [`SPLINE_STEP_PX`] and at most [`SPLINE_MAX_STEPS`], so a gentle
/// or short segment gets few and a straight one none; every
/// sub-point is kept inside the box of its segment's two ends, so the curve
/// never overshoots the points (an apex stays the highest point). Times are
/// interpolated along each segment. Points that are not finite are left
/// out, and so is a point on top of the one before; a non-finite time
/// takes the one before it (0 at the start). Fewer than three points are
/// returned as they are.
#[must_use]
pub(crate) fn spline(points: &[(Pos2, f64)]) -> Curve {
    let mut p: Vec<(Pos2, f64)> = Vec::with_capacity(points.len());
    for &(q, t) in points {
        if !(q.x.is_finite() && q.y.is_finite()) {
            continue;
        }
        let t = if t.is_finite() {
            t
        } else {
            p.last().map_or(0.0, |l| l.1)
        };
        if p.last().is_some_and(|l| l.0.distance(q) <= SPLINE_SAME_PX) {
            continue;
        }
        p.push((q, t));
    }
    let mut out = Curve::default();
    if p.len() < 3 {
        for (q, t) in p {
            out.push(q, t);
        }
        return out;
    }
    let reach = SPLINE_STEP_PX * SPLINE_MAX_STEPS as f32;
    out.points.reserve(p.len() * SPLINE_MAX_STEPS);
    out.times.reserve(p.len() * SPLINE_MAX_STEPS);
    out.push(p[0].0, p[0].1);
    for i in 0..p.len() - 1 {
        let ((p1, t1), (p2, t2)) = (p[i], p[i + 1]);
        let last = p.len() - 1;
        let p0 = i
            .checked_sub(1)
            .map_or_else(|| beyond(p[0].0, p[1].0, p[2].0), |j| p[j].0);
        let p3 = p
            .get(i + 2)
            .map_or_else(|| beyond(p[last].0, p[last - 1].0, p[last - 2].0), |q| q.0);
        let segment = Segment::new([p0, p1, p2, p3]);
        // A chord cut into n strays about 1/n² as far from the curve.
        let bow = [0.25, 0.5, 0.75]
            .map(|u| segment_distance(segment.at(u), p1, p2))
            .into_iter()
            .fold(0.0, f32::max);
        let by_bend = (bow / SPLINE_TOLERANCE_PX).sqrt().ceil();
        let by_length = (p1.distance(p2).min(reach) / SPLINE_STEP_PX).ceil();
        let steps = (by_bend.min(by_length) as usize).clamp(1, SPLINE_MAX_STEPS);
        let bounds = Rect::from_two_pos(p1, p2);
        for s in 1..steps {
            let u = s as f32 / steps as f32;
            let q = segment.at(u);
            let q = Pos2::new(
                q.x.clamp(bounds.min.x, bounds.max.x),
                q.y.clamp(bounds.min.y, bounds.max.y),
            );
            out.push(q, t1 + (t2 - t1) * f64::from(u));
        }
        out.push(p2, t2);
    }
    out
}

/// The point before `end` on the parabola through `end`, `next` and
/// `after` (equally spaced), so an end segment bends like its neighbours: a
/// hop's three points give a parabola, steep off the ground and flat at
/// the top.
fn beyond(end: Pos2, next: Pos2, after: Pos2) -> Pos2 {
    end + (end - next) * 3.0 + (after - end)
}

/// One centripetal Catmull-Rom segment, from `p[1]` to `p[2]`, with its
/// knots (`p[0]` at 0).
struct Segment {
    p: [Pos2; 4],
    k: [f32; 3],
}

impl Segment {
    fn new(p: [Pos2; 4]) -> Self {
        // Centripetal: knots spaced by the square root of the chord length.
        let k1 = p[0].distance(p[1]).sqrt();
        let k2 = k1 + p[1].distance(p[2]).sqrt();
        let k3 = k2 + p[2].distance(p[3]).sqrt();
        Self { p, k: [k1, k2, k3] }
    }

    /// The point `u` (0..1) of the way from `p[1]` to `p[2]` in knot
    /// terms (Barry and Goldman's pyramid).
    fn at(&self, u: f32) -> Pos2 {
        let [p0, p1, p2, p3] = self.p;
        let [k1, k2, k3] = self.k;
        let t = k1 + u * (k2 - k1);
        let mix = |a: Pos2, b: Pos2, ka: f32, kb: f32| {
            let span = kb - ka;
            if span > 0.0 {
                a.lerp(b, (t - ka) / span)
            } else {
                a
            }
        };
        let a1 = mix(p0, p1, 0.0, k1);
        let a2 = mix(p1, p2, k1, k2);
        let a3 = mix(p2, p3, k2, k3);
        let b1 = mix(a1, a2, 0.0, k2);
        let b2 = mix(a2, a3, k1, k3);
        mix(b1, b2, k1, k2)
    }
}

/// The stretches of a ground segment (`[t, x, y, z]`) between consecutive
/// points on the ground ([`ON_GROUND_M`]): one per hop, and one per step of
/// the roll. Each runs from one ground point to the next, inclusive, so
/// neighbours share their end point. Empty for fewer than two points.
#[must_use]
pub(crate) fn ground_pieces(points: &[[f64; 4]]) -> Vec<std::ops::RangeInclusive<usize>> {
    let Some(last) = points.len().checked_sub(1).filter(|&l| l > 0) else {
        return Vec::new();
    };
    let mut cuts = vec![0];
    cuts.extend((1..last).filter(|&i| points[i][2] <= ON_GROUND_M));
    cuts.push(last);
    cuts.windows(2).map(|w| w[0]..=w[1]).collect()
}

/// A path's flight and ground segments as smooth screen curves, each point
/// placed by `project`. The flight is one curve, tee to landing; each hop
/// of the ground segment is its own ([`ground_pieces`]), so a bounce keeps
/// its corner. Built once per scene; the draw-on cuts it by time
/// ([`Curve::prefix`]).
#[must_use]
pub(crate) fn smooth_path(p: &FlightPath, project: impl Fn(&[f64; 4]) -> Pos2) -> (Curve, Curve) {
    let timed = |points: &[[f64; 4]]| -> Vec<(Pos2, f64)> {
        points.iter().map(|q| (project(q), q[0])).collect()
    };
    let flight = spline(&timed(&p.flight));
    let mut ground = Curve::default();
    for piece in ground_pieces(&p.ground) {
        let curve = spline(&timed(&p.ground[piece]));
        let skip = usize::from(!ground.points.is_empty());
        ground.points.extend(curve.points.into_iter().skip(skip));
        ground.times.extend(curve.times.into_iter().skip(skip));
    }
    (flight, ground)
}

// ---------------------------------------------------------------------------
// Hit-testing
// ---------------------------------------------------------------------------

/// A shot's screen geometry offered to a hit-test.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Candidate<'a, K> {
    /// What a hit returns (e.g. the shot's index).
    pub(crate) key: K,
    /// Dots for [`nearest_point`], or a polyline for [`nearest_segment`].
    pub(crate) points: &'a [Pos2],
    /// Whether the shot takes hover and clicks; faint shots do not.
    pub(crate) hittable: bool,
}

/// Key of the hittable candidate with a point nearest `pointer`, within
/// `radius` screen points.
#[must_use]
pub(crate) fn nearest_point<'a, K: Copy + 'a>(
    pointer: Pos2,
    radius: f32,
    candidates: impl IntoIterator<Item = &'a Candidate<'a, K>>,
) -> Option<K> {
    nearest(radius, candidates, |c| {
        c.points
            .iter()
            .map(|p| p.distance(pointer))
            .reduce(f32::min)
    })
}

/// Key of the hittable candidate whose polyline passes nearest `pointer`,
/// within `radius` screen points.
#[must_use]
pub(crate) fn nearest_segment<'a, K: Copy + 'a>(
    pointer: Pos2,
    radius: f32,
    candidates: impl IntoIterator<Item = &'a Candidate<'a, K>>,
) -> Option<K> {
    nearest(radius, candidates, |c| match c.points {
        [] => None,
        [p] => Some(p.distance(pointer)),
        pts => pts
            .windows(2)
            .map(|w| segment_distance(pointer, w[0], w[1]))
            .reduce(f32::min),
    })
}

fn nearest<'a, K: Copy + 'a>(
    radius: f32,
    candidates: impl IntoIterator<Item = &'a Candidate<'a, K>>,
    distance: impl Fn(&Candidate<'a, K>) -> Option<f32>,
) -> Option<K> {
    candidates
        .into_iter()
        .filter(|c| c.hittable)
        .filter_map(|c| distance(c).map(|d| (c.key, d)))
        .filter(|&(_, d)| d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k)
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 <= 0.0 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// Whether `p` lies inside the closed polygon `polygon` (even-odd rule).
#[must_use]
pub(crate) fn inside_polygon(polygon: &[Pos2], p: Pos2) -> bool {
    let mut inside = false;
    let mut j = polygon.len().wrapping_sub(1);
    for (i, a) in polygon.iter().enumerate() {
        let b = polygon[j];
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/// The distinct, non-empty actor ids in `shots`, sorted. Ids are compared
/// exactly, so a row without an actor never merges two devices.
#[must_use]
pub(crate) fn device_ids(shots: &[ShotRow]) -> Vec<String> {
    let mut ids: Vec<String> = shots
        .iter()
        .filter(|s| !s.actor.is_empty())
        .map(|s| s.actor.clone())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Colour of the device `id` among `ids` (as from [`device_ids`]):
/// [`theme::DEVICE_COLOURS`] in order, then `TEXT_MUTED` for any further or
/// unknown device.
#[must_use]
pub(crate) fn device_colour(ids: &[String], id: &str) -> Color32 {
    ids.iter()
        .position(|d| d == id)
        .and_then(|i| theme::DEVICE_COLOURS.get(i).copied())
        .unwrap_or(theme::TEXT_MUTED)
}

/// Label of a device: its name, or its actor id when the name is missing
/// or empty.
#[must_use]
pub(crate) fn device_label<'a>(id: &'a str, name: Option<&'a str>) -> &'a str {
    name.filter(|n| !n.is_empty()).unwrap_or(id)
}

// ---------------------------------------------------------------------------
// Members of the view
// ---------------------------------------------------------------------------

/// Indices into `shots`, ascending, of the rows the range bars and charts
/// show bright in `view`, among the rows from `device` (exact actor id;
/// every device when `None`).
///
/// `set` is the set the view is built around (the shown shot's set, or the
/// one the next shot joins) and `shown` the index of the shown shot.
/// - By set: the rows of `set`.
/// - By club: the rows of every set with `set`'s club in `set`'s session,
///   which for a shot of the current session is that club since the last
///   New session.
/// - `none` tracks only the last shot: the members are the shown shot
///   alone, whatever the device filter.
#[must_use]
pub(crate) fn members_for(
    shots: &[ShotRow],
    groups: &ShotGroups,
    view: ShotGrouping,
    set: Option<&ShotGroupInfo>,
    shown: Option<usize>,
    device: Option<&str>,
) -> Vec<usize> {
    if view == ShotGrouping::None {
        return shown.filter(|&i| i < shots.len()).into_iter().collect();
    }
    let Some(set) = set else {
        return Vec::new();
    };
    let in_view = |r: &ShotRow| {
        let Some(id) = r.group.as_deref() else {
            return false;
        };
        if view == ShotGrouping::BySet {
            return id == set.id;
        }
        groups
            .get(id)
            .is_some_and(|g| g.club == set.club && g.session == set.session)
    };
    shots
        .iter()
        .enumerate()
        .filter(|&(_, r)| device.is_none_or(|d| r.actor == d) && in_view(r))
        .map(|(i, _)| i)
        .collect()
}

// ---------------------------------------------------------------------------
// Caches
// ---------------------------------------------------------------------------

/// A value kept until its key changes. The UI repaints at least every
/// 100 ms, so anything derived from the shot list is built once per change
/// rather than once per frame.
#[derive(Debug)]
pub(crate) struct Cached<K, V> {
    entry: Option<(K, V)>,
}

impl<K, V> Default for Cached<K, V> {
    fn default() -> Self {
        Self { entry: None }
    }
}

impl<K: PartialEq, V> Cached<K, V> {
    /// The value for `key`: the stored one when it was built for an equal
    /// key, else `build()`, stored for the next call.
    pub(crate) fn get(&mut self, key: K, build: impl FnOnce() -> V) -> &V {
        if self.entry.as_ref().is_some_and(|(k, _)| *k != key) {
            self.entry = None;
        }
        &self.entry.get_or_insert_with(|| (key, build())).1
    }

    /// The stored value when it was built for a key equal to `key`.
    #[must_use]
    pub(crate) fn peek(&self, key: &K) -> Option<&V> {
        self.entry
            .as_ref()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// The stored value, whatever its key.
    #[cfg(test)]
    pub(crate) fn value(&self) -> Option<&V> {
        self.entry.as_ref().map(|(_, v)| v)
    }
}

/// Everything the view's members depend on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MembersKey {
    /// Bumped at every change to the shot list.
    pub(crate) shots_rev: u64,
    /// Bumped at every change to the set list or the session.
    pub(crate) groups_rev: u64,
    pub(crate) view: ShotGrouping,
    /// Device filter: an exact actor id, or every device.
    pub(crate) device: Option<String>,
    /// Index of the shown shot.
    pub(crate) shown: Option<usize>,
    /// Club in play, which picks the set when the shown shot has none.
    pub(crate) club: Option<Club>,
}

/// Everything a chart's screen geometry depends on: the view's members, the
/// units and the plot size in whole points. The data domain follows from
/// these.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChartKey {
    pub(crate) members: MembersKey,
    pub(crate) units: UnitSystem,
    pub(crate) size: [u32; 2],
}

impl ChartKey {
    #[must_use]
    pub(crate) fn new(members: MembersKey, units: UnitSystem, size: egui::Vec2) -> Self {
        let px = |v: f32| v.round().max(0.0) as u32;
        Self {
            members,
            units,
            size: [px(size.x), px(size.y)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    fn rect(w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::new(10.0, 20.0), egui::vec2(w, h))
    }

    // -- domains and ticks --

    #[test]
    fn nice_ceil_rounds_up_to_the_step() {
        assert_eq!(nice_ceil(173.0 * 1.06, 25.0), 200.0);
        assert_eq!(nice_ceil(200.0, 25.0), 200.0);
        assert_eq!(nice_ceil(200.1, 25.0), 225.0);
        assert_eq!(nice_ceil(0.0, 25.0), 0.0);
        assert_eq!(nice_ceil(-3.0, 25.0), 0.0);
        assert_eq!(nice_ceil(f64::NAN, 25.0), 0.0);
    }

    #[test]
    fn axis_domains_use_margin_step_and_floor() {
        assert_eq!(SIDE_DISTANCE.domain_max(173.0), 200.0);
        assert_eq!(SIDE_DISTANCE.domain_max(20.0), 50.0);
        assert_eq!(SIDE_DISTANCE.domain_max(f64::NAN), 50.0);
        assert_eq!(SIDE_DISTANCE_SHORT.domain_max(5.5), 10.0);
        assert_eq!(SIDE_DISTANCE_SHORT.domain_max(20.6), 25.0);
        assert_eq!(SIDE_DISTANCE_SHORT.domain_max(0.0), 10.0);
        let ft = side_height(UnitSystem::Imperial);
        assert_eq!(ft.domain_max(94.0), 120.0);
        assert_eq!(ft.domain_max(10.0), 30.0);
        let m = side_height(UnitSystem::Metric);
        assert_eq!(m.domain_max(28.0), 40.0);
        assert_eq!(m.domain_max(2.0), 10.0);
        assert_eq!(OVERHEAD_DOWNRANGE.domain_max(185.0), 200.0);
        assert_eq!(OVERHEAD_DOWNRANGE.domain_max(30.0), 50.0);
        assert_eq!(OVERHEAD_LATERAL.domain_max(4.0), 15.0);
        assert_eq!(OVERHEAD_LATERAL.domain_max(17.0), 25.0);
    }

    #[test]
    fn ticks_are_step_multiples_inside_the_domain() {
        assert_eq!(ticks(0.0, 100.0, 25.0), vec![0.0, 25.0, 50.0, 75.0, 100.0]);
        assert_eq!(
            ticks(-22.0, 22.0, 10.0),
            vec![-20.0, -10.0, 0.0, 10.0, 20.0]
        );
        assert!(ticks(0.0, 100.0, 0.0).is_empty());
        assert!(ticks(10.0, 0.0, 5.0).is_empty());
    }

    #[test]
    fn tick_step_thins_below_the_pixel_pitch() {
        // 25 yd at 3 px/yd is 75 px: kept.
        assert_eq!(tick_step(25.0, 3.0, 70.0), 25.0);
        // 25 yd at 2 px/yd is 50 px: doubled to 50 (100 px).
        assert_eq!(tick_step(25.0, 2.0, 70.0), 50.0);
        // Tiny chart: doubled until the pitch fits.
        assert_eq!(tick_step(25.0, 0.5, 70.0), 200.0);
        // No pixel density, or no step: no ticks.
        for px in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(tick_step(25.0, px, 70.0), f64::INFINITY, "{px}");
        }
        for step in [0.0, -25.0, f64::NAN, f64::INFINITY] {
            assert_eq!(tick_step(step, 2.0, 70.0), f64::INFINITY, "{step}");
        }
        // The smallest positive density still ends.
        assert!(tick_step(25.0, f32::from_bits(1), 70.0) > 1e40);
    }

    #[test]
    fn ticks_are_bounded_for_extreme_domains() {
        for (lo, hi, step) in [
            (0.0, 1e300, 25.0),
            (0.0, 1e38, 25.0),
            (-1e308, 1e308, 1.0),
            (0.0, 100_000.0, 25.0),
            (1e300, 1e300, 1.0),
            (0.0, f64::INFINITY, 25.0),
            (f64::NEG_INFINITY, 0.0, 25.0),
            (f64::NAN, 100.0, 25.0),
            (0.0, 100.0, f64::NAN),
            (0.0, 100.0, f64::INFINITY),
            (0.0, 100.0, -25.0),
        ] {
            assert!(ticks(lo, hi, step).is_empty(), "{lo} {hi} {step}");
        }
        assert_eq!(ticks(0.0, 25_000.0, 25.0).len(), 1001);
    }

    #[test]
    fn extreme_data_keeps_the_scales_finite_and_the_ticks_few() {
        let r = rect(400.0, 200.0);
        for v in [
            1e38,
            1e300,
            f64::MAX,
            -1e300,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -5.0,
            0.0,
        ] {
            let x_hi = SIDE_DISTANCE.domain_max(v);
            assert!(
                x_hi.is_finite() && (50.0..=MAX_EXTENT * 1.1).contains(&x_hi),
                "{v}"
            );
            for proj in [
                Projection::fit(r, (0.0, v), (0.0, v)),
                Projection::fit(r, (v, 0.0), (-v, v)),
                Projection::equal(r, v, v),
                Projection::equal(r, OVERHEAD_LATERAL.domain_max(v), x_hi),
            ] {
                for scale in [proj.x, proj.y] {
                    let px = scale.px_per_unit();
                    assert!(px.is_finite() && px > 0.0, "{v}: {px}");
                    let (lo, hi) = scale.domain();
                    assert!(lo.is_finite() && hi.is_finite() && hi > lo, "{v}");
                    let step = tick_step(25.0, px, 70.0);
                    assert!(ticks(lo, hi, step).len() <= 1001, "{v}");
                }
                for p in [v, -v, 0.0, 1e300, f64::NAN] {
                    let s = proj.to_screen(p, p);
                    assert!(s.x.is_finite() && s.y.is_finite(), "{v} {p}");
                }
            }
        }
        // Non-finite screen rects too.
        let bad = Rect::from_min_max(Pos2::new(f32::NAN, 0.0), Pos2::new(f32::INFINITY, 10.0));
        for proj in [
            Projection::fit(bad, (0.0, 100.0), (0.0, 100.0)),
            Projection::equal(bad, 10.0, 100.0),
        ] {
            let s = proj.to_screen(50.0, 50.0);
            assert!(s.x.is_finite() && s.y.is_finite());
        }
    }

    #[test]
    fn plausible_values_bound_the_axis_sizing() {
        assert!(plausible(0.0) && plausible(-300.0) && plausible(MAX_EXTENT));
        for v in [MAX_EXTENT + 1.0, 1e38, -1e300, f64::NAN, f64::INFINITY] {
            assert!(!plausible(v), "{v}");
        }
    }

    // -- projection --

    #[test]
    fn scale_maps_domain_ends_to_rect_ends() {
        let r = rect(400.0, 200.0);
        let p = Projection::fit(r, (0.0, 200.0), (0.0, 120.0));
        assert_eq!(p.to_screen(0.0, 0.0), r.left_bottom());
        assert_eq!(p.to_screen(200.0, 120.0), r.right_top());
        assert_eq!(p.to_screen(100.0, 60.0), r.center());
        assert_eq!(p.x.px_per_unit(), 2.0);
        assert_eq!(p.x.domain(), (0.0, 200.0));
        // An empty domain does not divide by zero.
        let flat = Scale::new(5.0, 5.0, 0.0, 10.0);
        assert!(flat.map(5.0).is_finite());
    }

    #[test]
    fn equal_axes_bind_on_the_tighter_dimension() {
        let r = rect(300.0, 600.0);
        // Tall, thin pattern: height binds, the lateral domain widens.
        let tall = Projection::equal(r, 15.0, 200.0);
        assert_eq!(tall.y.domain(), (0.0, 200.0));
        assert!(tall.x.domain().1 > 15.0);
        assert!((tall.x.px_per_unit() - tall.y.px_per_unit()).abs() < 1e-4);
        assert_eq!(tall.to_screen(0.0, 0.0), r.center_bottom());
        // Wide pattern: width binds, the downrange domain grows.
        let wide = Projection::equal(r, 150.0, 100.0);
        assert!(approx(wide.x.domain().1, 150.0, 1e-9));
        assert!(wide.y.domain().1 > 100.0);
        assert!((wide.x.px_per_unit() - wide.y.px_per_unit()).abs() < 1e-4);
        // Both data extents fit inside the rect either way.
        for p in [tall, wide] {
            assert!(r.expand(1e-3).contains(p.to_screen(-15.0, 100.0)));
            assert!(r.expand(1e-3).contains(p.to_screen(15.0, 100.0)));
        }
    }

    // -- covariance and ellipse --

    /// Four points with sample sd `sx` across and `sy` along, uncorrelated.
    fn cross(sx: f64, sy: f64) -> Vec<[f64; 2]> {
        // Two points at ±a: sample variance 2a² / 3 over four points.
        let (a, b) = (sx * 1.5_f64.sqrt(), sy * 1.5_f64.sqrt());
        vec![[a, 0.0], [-a, 0.0], [0.0, b], [0.0, -b]]
    }

    fn rotate(points: &[[f64; 2]], deg: f64) -> Vec<[f64; 2]> {
        let (s, c) = deg.to_radians().sin_cos();
        points
            .iter()
            .map(|p| [p[0] * c - p[1] * s, p[0] * s + p[1] * c])
            .collect()
    }

    #[test]
    fn covariance_uses_n_minus_one() {
        // Mean (1, 1); deviations (-1,-1), (0,1), (1,0).
        let c = covariance(&[[0.0, 0.0], [1.0, 2.0], [2.0, 1.0]]).unwrap();
        assert_eq!(c.n, 3);
        assert_eq!(c.mean, [1.0, 1.0]);
        assert!(approx(c.xx, 1.0, 1e-12));
        assert!(approx(c.yy, 1.0, 1e-12));
        assert!(approx(c.xy, 0.5, 1e-12));
        assert!(covariance(&[[1.0, 1.0]]).is_none());
    }

    #[test]
    fn ellipse_needs_three_finite_points() {
        assert!(ellipse_from_points(&[], ELLIPSE_K_95).is_none());
        assert!(ellipse_from_points(&[[0.0, 0.0], [1.0, 1.0]], ELLIPSE_K_95).is_none());
        assert!(
            ellipse_from_points(&[[0.0, 0.0], [1.0, 1.0], [f64::NAN, 2.0]], ELLIPSE_K_95).is_none()
        );
        let e = ellipse_from_points(
            &[[0.0, 0.0], [1.0, 2.0], [2.0, 1.0], [f64::NAN, 0.0]],
            ELLIPSE_K_95,
        );
        assert_eq!(e.unwrap().n, 3);
    }

    #[test]
    fn ellipse_of_extreme_points_is_finite_or_none() {
        for big in [1e38, 1e200, 1e300, f64::MAX, -1e300] {
            let pts = [[0.0, 0.0], [1.0, 2.0], [2.0, 1.0], [big, big]];
            if let Some(e) = ellipse_from_points(&pts, ELLIPSE_K_95) {
                assert!(
                    ellipse_points(&e, ELLIPSE_SEGMENTS)
                        .iter()
                        .all(|p| p[0].is_finite() && p[1].is_finite()),
                    "{big}: {e:?}"
                );
            }
        }
        let inf = [[0.0, 0.0], [1.0, 2.0], [2.0, 1.0], [f64::INFINITY, 0.0]];
        assert_eq!(ellipse_from_points(&inf, ELLIPSE_K_95).unwrap().n, 3);
    }

    #[test]
    fn axis_aligned_spread_gives_k_sigma_semi_axes() {
        let e = ellipse_from_points(&cross(5.0, 10.0), ELLIPSE_K_95).unwrap();
        assert!(approx(e.semi_major, 24.48, 1e-9), "{e:?}");
        assert!(approx(e.semi_minor, 12.24, 1e-9), "{e:?}");
        assert!(approx(e.angle.to_degrees().abs(), 90.0, 1e-9), "{e:?}");
        let e = ellipse_from_points(&cross(10.0, 5.0), ELLIPSE_K_95).unwrap();
        assert!(approx(e.semi_major, 24.48, 1e-9));
        assert!(approx(e.angle, 0.0, 1e-9));
    }

    #[test]
    fn rotated_spread_recovers_its_angle() {
        let pts = rotate(&cross(10.0, 5.0), 45.0);
        let e = ellipse_from_points(&pts, ELLIPSE_K_95).unwrap();
        assert!(approx(e.angle.to_degrees(), 45.0, 1.0), "{e:?}");
        assert!(approx(e.semi_major, 24.48, 1e-6));
        assert!(approx(e.semi_minor, 12.24, 1e-6));
        let e = ellipse_from_points(&rotate(&cross(10.0, 5.0), -30.0), ELLIPSE_K_95).unwrap();
        assert!(approx(e.angle.to_degrees(), -30.0, 1.0), "{e:?}");
    }

    #[test]
    fn mean_is_the_centroid() {
        let pts = [[1.0, 10.0], [3.0, 14.0], [8.0, 9.0], [-4.0, 11.0]];
        let e = ellipse_from_points(&pts, ELLIPSE_K_95).unwrap();
        assert!(approx(e.center[0], 2.0, 1e-12));
        assert!(approx(e.center[1], 11.0, 1e-12));
    }

    #[test]
    fn degenerate_sets_get_the_floor() {
        // Collinear: the minor axis is floored.
        let e = ellipse_from_points(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]], ELLIPSE_K_95).unwrap();
        assert_eq!(e.semi_minor, ELLIPSE_MIN_SEMI_AXIS);
        assert!(e.semi_major > ELLIPSE_MIN_SEMI_AXIS);
        assert!(approx(e.angle.to_degrees(), 45.0, 1e-9));
        // Coincident: zero variance, a floor-sized circle.
        let e = ellipse_from_points(&[[5.0, 150.0]; 4], ELLIPSE_K_95).unwrap();
        assert_eq!(e.semi_major, ELLIPSE_MIN_SEMI_AXIS);
        assert_eq!(e.semi_minor, ELLIPSE_MIN_SEMI_AXIS);
        assert_eq!(e.angle, 0.0);
        assert_eq!(e.center, [5.0, 150.0]);
        // Zero variance across only: a vertical line.
        let e =
            ellipse_from_points(&[[2.0, 100.0], [2.0, 110.0], [2.0, 120.0]], ELLIPSE_K_95).unwrap();
        assert_eq!(e.semi_minor, ELLIPSE_MIN_SEMI_AXIS);
        assert!(approx(e.angle.to_degrees().abs(), 90.0, 1e-9));
    }

    #[test]
    fn ellipse_points_close_and_span_the_axes() {
        let e = Ellipse {
            center: [3.0, 150.0],
            semi_major: 20.0,
            semi_minor: 8.0,
            angle: 0.0,
            n: 5,
        };
        let pts = ellipse_points(&e, ELLIPSE_SEGMENTS);
        assert_eq!(pts.len(), ELLIPSE_SEGMENTS);
        let gap = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).hypot(a[1] - b[1]);
        // The closing edge, last to first, is one step like the others.
        let steps: Vec<f64> = pts.windows(2).map(|w| gap(w[0], w[1])).collect();
        let max_step = steps.iter().copied().fold(0.0, f64::max);
        let closing = gap(pts[pts.len() - 1], pts[0]);
        assert!(closing > 0.0 && closing <= max_step + 1e-9);
        let (min_x, max_x) = pts.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p[0]), hi.max(p[0]))
        });
        let (min_y, max_y) = pts.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p[1]), hi.max(p[1]))
        });
        assert!(approx(min_x, -17.0, 1e-9) && approx(max_x, 23.0, 1e-9));
        assert!(approx(min_y, 142.0, 1e-9) && approx(max_y, 158.0, 1e-9));
        // Rotated: the box follows the rotated axes.
        let r = Ellipse {
            angle: std::f64::consts::FRAC_PI_2,
            ..e
        };
        let pts = ellipse_points(&r, ELLIPSE_SEGMENTS);
        let max_x = pts.iter().map(|p| p[0]).fold(f64::MIN, f64::max);
        let max_y = pts.iter().map(|p| p[1]).fold(f64::MIN, f64::max);
        assert!(approx(max_x, 11.0, 1e-9) && approx(max_y, 170.0, 1e-9));
    }

    #[test]
    fn right_and_long_spread_points_to_the_screen_top_right() {
        // Positive lateral/downrange correlation: right-and-long, left-and-short.
        let pts = [
            [-6.0, 160.0],
            [-3.0, 168.0],
            [0.0, 172.0],
            [2.0, 175.0],
            [5.0, 182.0],
            [8.0, 190.0],
        ];
        let e = ellipse_from_points(&pts, ELLIPSE_K_95).unwrap();
        let r = rect(320.0, 600.0);
        let proj = Projection::equal(
            r,
            OVERHEAD_LATERAL.domain_max(8.0),
            OVERHEAD_DOWNRANGE.domain_max(190.0),
        );
        let centre = proj.to_screen(e.center[0], e.center[1]);
        // The polygon vertex farthest from the centre is a major-axis end;
        // take the one to the right of the centre.
        let far = ellipse_points(&e, ELLIPSE_SEGMENTS)
            .iter()
            .map(|p| proj.to_screen(p[0], p[1]))
            .filter(|p| p.x > centre.x)
            .max_by(|a, b| a.distance(centre).total_cmp(&b.distance(centre)))
            .unwrap();
        // Screen y grows downward: top-right means right of and above the centre.
        assert!(
            far.x > centre.x && far.y < centre.y,
            "{far:?} vs {centre:?}"
        );
        // And the long axis is closer to vertical than horizontal here.
        assert!(centre.y - far.y > far.x - centre.x);
    }

    // -- smooth curves and the draw-on --

    fn timed(points: &[(f32, f32)]) -> Vec<(Pos2, f64)> {
        points
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| (Pos2::new(x, y), i as f64))
            .collect()
    }

    /// Whether `curve` runs through `points` in order, each exactly.
    fn passes_through(curve: &Curve, points: &[Pos2]) -> bool {
        let mut at = 0;
        points
            .iter()
            .all(|p| match curve.points[at..].iter().position(|q| q == p) {
                Some(i) => {
                    at += i;
                    true
                }
                None => false,
            })
    }

    fn finite(curve: &Curve) -> bool {
        curve
            .points
            .iter()
            .all(|p| p.x.is_finite() && p.y.is_finite())
            && curve.times.iter().all(|t| t.is_finite())
            && curve.points.len() == curve.times.len()
    }

    /// A ball flight on screen (y down): tee at (0, 300), apex near x 330,
    /// steeper descent, sampled unevenly like a decimated path.
    fn apex_arc() -> Vec<(Pos2, f64)> {
        let xs = [
            0.0, 40.0, 120.0, 220.0, 300.0, 330.0, 360.0, 420.0, 470.0, 500.0,
        ];
        xs.iter()
            .map(|&x: &f32| {
                let h = if x <= 330.0 {
                    1.0 - ((330.0 - x) / 330.0).powi(2)
                } else {
                    1.0 - ((x - 330.0) / 170.0).powi(2)
                };
                (Pos2::new(x, 300.0 - 200.0 * h), f64::from(x) / 100.0)
            })
            .collect()
    }

    #[test]
    fn spline_passes_through_every_point() {
        let arc = apex_arc();
        let curve = spline(&arc);
        let points: Vec<Pos2> = arc.iter().map(|p| p.0).collect();
        assert!(passes_through(&curve, &points));
        assert_eq!(curve.points.first(), points.first());
        assert_eq!(curve.points.last(), points.last());
        assert!(finite(&curve));
        // Times climb with the curve and match the input's at each point.
        assert!(curve.times.windows(2).all(|w| w[0] <= w[1]));
        for (p, t) in &arc {
            let i = curve.points.iter().position(|q| q == p).unwrap();
            assert_eq!(curve.times[i], *t);
        }
        // Curved, not the polyline: some sub-point sits off its chord.
        let off = curve
            .points
            .iter()
            .map(|q| {
                points
                    .windows(2)
                    .map(|w| segment_distance(*q, w[0], w[1]))
                    .fold(f32::INFINITY, f32::min)
            })
            .fold(0.0, f32::max);
        assert!(off > 0.5, "largest offset from the polyline {off}");
    }

    #[test]
    fn spline_does_not_overshoot_a_monotone_apex_arc() {
        let arc = apex_arc();
        let curve = spline(&arc);
        let apex = arc.iter().map(|p| p.0.y).fold(f32::INFINITY, f32::min);
        let top = curve
            .points
            .iter()
            .map(|p| p.y)
            .fold(f32::INFINITY, f32::min);
        assert_eq!(top, apex, "the apex sample stays the highest point");
        assert!(
            curve.points.windows(2).all(|w| w[0].x <= w[1].x),
            "x only grows"
        );
        assert!(
            curve.points.iter().all(|p| p.y <= 300.0),
            "never below the tee"
        );
        // Up to the apex the curve only climbs, after it only falls.
        let at = curve.points.iter().position(|p| p.y == apex).unwrap();
        assert!(curve.points[..=at].windows(2).all(|w| w[1].y <= w[0].y));
        assert!(curve.points[at..].windows(2).all(|w| w[1].y >= w[0].y));
    }

    #[test]
    fn spline_steps_adapt_to_the_bend_and_length() {
        // Straight: no sub-points at all.
        let line = timed(&[(0.0, 0.0), (100.0, 50.0), (200.0, 100.0), (260.0, 130.0)]);
        assert_eq!(spline(&line).points.len(), 4);
        // A sharp bend over a long segment: the most steps.
        let sharp = spline(&timed(&[(0.0, 0.0), (200.0, -200.0), (400.0, 0.0)]));
        let apex = sharp.points.iter().position(|p| p.y == -200.0).unwrap();
        assert!((8..=SPLINE_MAX_STEPS).contains(&apex), "{apex}");
        // A gentle bend: a few.
        let gentle = spline(&timed(&[(0.0, 0.0), (100.0, -10.0), (200.0, 0.0)]));
        let mid = gentle.points.iter().position(|p| p.x == 100.0).unwrap();
        assert!((2..SPLINE_MAX_STEPS).contains(&mid), "{mid}");
        // Short segments: sub-segments no shorter than about 6 px.
        let short = spline(&timed(&[(0.0, 0.0), (10.0, -10.0), (20.0, 0.0)]));
        let mid = short.points.iter().position(|p| p.x == 10.0).unwrap();
        assert!(mid <= 3, "{mid}");
        // The polyline stays within the tolerance of a densely sampled
        // curve, or within a pixel and a half where the step cap binds.
        for (curve, input, tolerance) in [
            (&sharp, [(0.0, 0.0), (200.0, -200.0), (400.0, 0.0)], 1.5),
            (
                &gentle,
                [(0.0, 0.0), (100.0, -10.0), (200.0, 0.0)],
                SPLINE_TOLERANCE_PX * 2.0,
            ),
        ] {
            let p: Vec<Pos2> = input.iter().map(|&(x, y)| Pos2::new(x, y)).collect();
            let segments = [
                Segment::new([beyond(p[0], p[1], p[2]), p[0], p[1], p[2]]),
                Segment::new([p[0], p[1], p[2], beyond(p[2], p[1], p[0])]),
            ];
            for seg in &segments {
                for k in 0..=200 {
                    let q = seg.at(k as f32 / 200.0);
                    let off = curve
                        .points
                        .windows(2)
                        .map(|w| segment_distance(q, w[0], w[1]))
                        .fold(f32::INFINITY, f32::min);
                    assert!(off <= tolerance, "{q:?} {off}");
                }
            }
        }
    }

    #[test]
    fn a_three_point_hop_is_a_parabola() {
        // Contact, peak, contact of y = x (2 - x), scaled: the curve leaves
        // the ground near the parabola's slope, not the chord's, and stays
        // within a few pixels of the parabola (centripetal knots are not
        // uniform, so it is close, not exact).
        let hop: Vec<(Pos2, f64)> = [0.0_f32, 50.0, 100.0]
            .iter()
            .map(|&x| (Pos2::new(x, -x * (100.0 - x) / 50.0), f64::from(x)))
            .collect();
        let curve = spline(&hop);
        let first = curve.points[1] - curve.points[0];
        assert!(-first.y / first.x > 1.4, "{first:?}");
        for q in &curve.points {
            let y = -q.x * (100.0 - q.x) / 50.0;
            assert!((q.y - y).abs() < 4.0, "{q:?} vs {y}");
        }
    }

    #[test]
    fn spline_handles_degenerate_input() {
        assert_eq!(spline(&[]), Curve::default());
        let one = spline(&timed(&[(3.0, 4.0)]));
        assert_eq!(one.points, vec![Pos2::new(3.0, 4.0)]);
        let two = spline(&timed(&[(0.0, 0.0), (10.0, 5.0)]));
        assert_eq!(two.points, vec![Pos2::new(0.0, 0.0), Pos2::new(10.0, 5.0)]);
        assert_eq!(two.times, vec![0.0, 1.0]);

        // Duplicates collapse; the curve still runs through each point.
        let dup = spline(&timed(&[
            (0.0, 0.0),
            (0.0, 0.0),
            (50.0, -40.0),
            (50.0, -40.0),
            (100.0, 0.0),
            (100.0, 0.0),
        ]));
        assert!(finite(&dup));
        let distinct = [
            Pos2::new(0.0, 0.0),
            Pos2::new(50.0, -40.0),
            Pos2::new(100.0, 0.0),
        ];
        assert!(passes_through(&dup, &distinct));
        assert_eq!(dup.points.iter().filter(|p| **p == distinct[1]).count(), 1);
        // Two distinct points among duplicates: a straight line.
        let pair = spline(&timed(&[(0.0, 0.0), (0.0, 0.0), (9.0, 9.0)]));
        assert_eq!(pair.points, vec![Pos2::new(0.0, 0.0), Pos2::new(9.0, 9.0)]);

        // Non-finite points are left out, non-finite times carried over.
        let nan = spline(&[
            (Pos2::new(0.0, 0.0), 0.0),
            (Pos2::new(f32::NAN, 1.0), 1.0),
            (Pos2::new(40.0, -30.0), f64::NAN),
            (Pos2::new(f32::INFINITY, 0.0), 3.0),
            (Pos2::new(80.0, 0.0), 4.0),
        ]);
        assert!(finite(&nan));
        assert!(passes_through(
            &nan,
            &[
                Pos2::new(0.0, 0.0),
                Pos2::new(40.0, -30.0),
                Pos2::new(80.0, 0.0)
            ]
        ));
        assert!(nan.times.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(
            spline(&[(Pos2::new(f32::NAN, f32::NAN), 0.0)]),
            Curve::default()
        );
    }

    /// A path with a flight and two hops before a short roll: landing at
    /// x 100, contacts at 104 and 106, rest at 110 (metres).
    fn bouncing_path() -> FlightPath {
        FlightPath {
            flight: vec![
                [0.0, 0.0, 0.0, 0.0],
                [1.0, 40.0, 25.0, 0.0],
                [2.0, 70.0, 30.0, 0.0],
                [3.0, 90.0, 18.0, 0.0],
                [4.0, 100.0, 0.0, 0.0],
            ],
            ground: vec![
                [4.0, 100.0, 0.0, 0.0],
                [4.3, 102.0, 0.8, 0.0],
                [4.6, 104.0, 0.0, 0.0],
                [4.7, 105.0, 0.2, 0.0],
                [4.8, 106.0, 0.0, 0.0],
                [6.0, 110.0, 0.0, 0.0],
            ],
        }
    }

    #[test]
    fn ground_pieces_split_at_every_contact() {
        let p = bouncing_path();
        assert_eq!(ground_pieces(&p.ground), vec![0..=2, 2..=4, 4..=5]);
        assert!(ground_pieces(&[]).is_empty());
        assert!(ground_pieces(&p.ground[..1]).is_empty());
        assert_eq!(ground_pieces(&p.ground[..2]), vec![0..=1]);
    }

    #[test]
    fn smooth_path_keeps_the_contacts_and_their_corners() {
        let p = bouncing_path();
        // 10 px per metre across, 20 px per metre up, ground at y 200.
        let project = |q: &[f64; 4]| Pos2::new(q[1] as f32 * 10.0, 200.0 - q[2] as f32 * 20.0);
        let (flight, ground) = smooth_path(&p, project);
        assert!(finite(&flight) && finite(&ground));
        // Every input point is on its curve, contacts included.
        let projected = |pts: &[[f64; 4]]| pts.iter().map(project).collect::<Vec<_>>();
        assert!(passes_through(&flight, &projected(&p.flight)));
        assert!(passes_through(&ground, &projected(&p.ground)));
        assert_eq!(flight.points.last(), ground.points.first());
        // Never below the ground.
        assert!(ground.points.iter().all(|q| q.y <= 200.0));
        // Each hop is its own curve: the ground curve is the pieces' splines
        // joined, so nothing is smoothed across a contact.
        let mut joined: Vec<Pos2> = Vec::new();
        for piece in ground_pieces(&p.ground) {
            let curve = spline(
                &p.ground[piece]
                    .iter()
                    .map(|q| (project(q), q[0]))
                    .collect::<Vec<_>>(),
            );
            let skip = usize::from(!joined.is_empty());
            joined.extend(curve.points.into_iter().skip(skip));
        }
        assert_eq!(ground.points, joined);
        // The bounce at x 104 keeps its corner: the curve comes down into it
        // and leaves it climbing.
        let contact = Pos2::new(1040.0, 200.0);
        let at = ground.points.iter().position(|q| *q == contact).unwrap();
        let (before, after) = (ground.points[at - 1], ground.points[at + 1]);
        assert!(before.y < 200.0 && after.y < 200.0, "{before:?} {after:?}");
        let into = (contact - before).normalized();
        let out = (after - contact).normalized();
        assert!(into.y > 0.3 && out.y < -0.3, "into {into:?} out {out:?}");
    }

    #[test]
    fn curve_prefix_cuts_at_t() {
        let curve = spline(&timed(&[
            (0.0, 0.0),
            (10.0, -8.0),
            (20.0, -10.0),
            (30.0, 0.0),
        ]));
        let last = curve.points.len() - 1;
        assert_eq!(curve.end_time(), Some(3.0));
        assert_eq!(curve.prefix(-1.0), vec![curve.points[0]]);
        assert_eq!(curve.prefix(0.0), vec![curve.points[0]]);
        assert_eq!(curve.prefix(f64::NAN), vec![curve.points[0]]);
        assert_eq!(curve.prefix(3.0), curve.points);
        assert_eq!(curve.prefix(9.0), curve.points);
        // Up to the second input point exactly.
        let at = curve
            .points
            .iter()
            .position(|p| *p == Pos2::new(20.0, -10.0))
            .unwrap();
        assert_eq!(curve.prefix(2.0), curve.points[..=at]);
        // Between two sub-points: interpolated, ending between them.
        let mid = curve.prefix(1.53);
        let end = *mid.last().unwrap();
        let n = mid.len() - 1;
        assert_eq!(mid[..n], curve.points[..n]);
        assert!(curve.times[n - 1] < 1.53 && curve.times[n] > 1.53);
        assert!(segment_distance(end, curve.points[n - 1], curve.points[n]) < 1e-3);
        assert!(n < last);
        assert!(Curve::default().prefix(1.0).is_empty());
    }

    // -- hit-testing --

    #[test]
    fn nearest_point_respects_radius_and_skips_faint() {
        let a = [Pos2::new(100.0, 100.0), Pos2::new(140.0, 100.0)];
        let b = [Pos2::new(106.0, 100.0)];
        let faint = [Pos2::new(101.0, 100.0)];
        let cands = [
            Candidate {
                key: 1,
                points: &a,
                hittable: true,
            },
            Candidate {
                key: 2,
                points: &b,
                hittable: true,
            },
            Candidate {
                key: 3,
                points: &faint,
                hittable: false,
            },
        ];
        assert_eq!(nearest_point(Pos2::new(102.0, 100.0), 8.0, &cands), Some(1));
        assert_eq!(nearest_point(Pos2::new(105.0, 101.0), 8.0, &cands), Some(2));
        // Any of a candidate's points counts.
        assert_eq!(nearest_point(Pos2::new(139.0, 103.0), 8.0, &cands), Some(1));
        assert_eq!(nearest_point(Pos2::new(120.0, 100.0), 8.0, &cands), None);
    }

    #[test]
    fn nearest_segment_respects_radius_and_skips_faint() {
        let low = [Pos2::new(0.0, 100.0), Pos2::new(100.0, 100.0)];
        let high = [
            Pos2::new(0.0, 90.0),
            Pos2::new(50.0, 90.0),
            Pos2::new(100.0, 80.0),
        ];
        let faint = [Pos2::new(0.0, 96.0), Pos2::new(100.0, 96.0)];
        let cands = [
            Candidate {
                key: 'l',
                points: &low,
                hittable: true,
            },
            Candidate {
                key: 'h',
                points: &high,
                hittable: true,
            },
            Candidate {
                key: 'f',
                points: &faint,
                hittable: false,
            },
        ];
        assert_eq!(
            nearest_segment(Pos2::new(40.0, 97.0), 5.0, &cands),
            Some('l')
        );
        assert_eq!(
            nearest_segment(Pos2::new(40.0, 93.0), 5.0, &cands),
            Some('h')
        );
        assert_eq!(nearest_segment(Pos2::new(40.0, 50.0), 5.0, &cands), None);
        // Past a segment's end the distance is to the endpoint.
        assert_eq!(
            nearest_segment(Pos2::new(104.0, 100.0), 5.0, &cands),
            Some('l')
        );
        assert_eq!(nearest_segment(Pos2::new(110.0, 100.0), 5.0, &cands), None);
    }

    // -- devices --

    fn shot(actor: &str) -> ShotRow {
        ShotRow {
            actor: actor.into(),
            shot_id: String::new(),
            shot_number: 1,
            ball: None,
            club: None,
            derived: None,
            club_selected: None,
            group: None,
            finished: true,
        }
    }

    #[test]
    fn device_ids_are_exact_sorted_and_distinct() {
        let shots: Vec<ShotRow> = ["square.0", "mevo.0", "", "square.0", "mevo.0"]
            .into_iter()
            .map(shot)
            .collect();
        // The empty actor merges with nothing (unlike `same_actor`).
        assert_eq!(device_ids(&shots), vec!["mevo.0", "square.0"]);
        let mut reversed = shots.clone();
        reversed.reverse();
        assert_eq!(device_ids(&reversed), device_ids(&shots));
        assert!(device_ids(&[]).is_empty());
    }

    #[test]
    fn device_colours_follow_the_sorted_ids() {
        let ids: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        assert_eq!(device_colour(&ids, "a"), theme::ACCENT);
        assert_eq!(device_colour(&ids, "b"), theme::HEADING);
        assert_eq!(device_colour(&ids, "c"), theme::LOG_ACTOR);
        assert_eq!(device_colour(&ids, "d"), theme::TEAL);
        assert_eq!(device_colour(&ids, "e"), theme::TEXT_MUTED);
        assert_eq!(device_colour(&ids, "zz"), theme::TEXT_MUTED);
        for c in theme::DEVICE_COLOURS {
            assert_ne!(c, theme::GOLD);
            assert_ne!(c, theme::DANGER);
        }
    }

    #[test]
    fn device_label_falls_back_to_the_id() {
        assert_eq!(device_label("mevo.0", Some("Mevo+")), "Mevo+");
        assert_eq!(
            device_label("openconnect_server", Some("")),
            "openconnect_server"
        );
        assert_eq!(device_label("square.0", None), "square.0");
    }

    #[test]
    fn ellipse_size_is_a_parameter() {
        // √χ²₂(p) = √(−2 ln(1 − p)): 2.448 holds 95 %, 2σ about 86 %.
        let k95 = (-2.0 * (1.0_f64 - 0.95).ln()).sqrt();
        assert!(approx(ELLIPSE_K_95, k95, 1e-3));
        assert!(approx(ellipse_coverage_pct(ELLIPSE_K_95), 95.0, 0.01));
        assert!(approx(ellipse_coverage_pct(ELLIPSE_K_2SIGMA), 86.47, 0.01));
        // The semi-axes scale with k; centre and angle do not move.
        let pts = rotate(&cross(10.0, 5.0), 20.0);
        let two = ellipse_from_points(&pts, ELLIPSE_K_2SIGMA).unwrap();
        let ninety_five = ellipse_from_points(&pts, ELLIPSE_K_95).unwrap();
        assert!(approx(two.semi_major, 20.0, 1e-6), "{two:?}");
        assert!(approx(two.semi_minor, 10.0, 1e-6), "{two:?}");
        let ratio = ninety_five.semi_major / two.semi_major;
        assert!(approx(ratio, ELLIPSE_K_95 / ELLIPSE_K_2SIGMA, 1e-9));
        assert_eq!(two.center, ninety_five.center);
        assert!(approx(two.angle, ninety_five.angle, 1e-12));
    }

    #[test]
    fn inside_polygon_uses_the_even_odd_rule() {
        let square = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ];
        assert!(inside_polygon(&square, Pos2::new(5.0, 5.0)));
        assert!(!inside_polygon(&square, Pos2::new(15.0, 5.0)));
        assert!(!inside_polygon(&square, Pos2::new(5.0, -1.0)));
        assert!(!inside_polygon(&[], Pos2::new(5.0, 5.0)));
    }

    // -- members of the view --

    fn set(id: &str, number: u32, club: Option<Club>, open: bool) -> ShotGroupInfo {
        ShotGroupInfo {
            id: id.into(),
            number,
            session: 0,
            golfer: None,
            club,
            started_at: "2026-10-26T15:41:28.300Z".into(),
            manual: false,
            open,
            shots: 2,
        }
    }

    fn in_session(session: u32, s: ShotGroupInfo) -> ShotGroupInfo {
        ShotGroupInfo { session, ..s }
    }

    fn sets(list: Vec<ShotGroupInfo>) -> ShotGroups {
        let mut g = ShotGroups::default();
        g.replace(crate::types::ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::BySet,
            session: 0,
            current: None,
            groups: list,
        });
        g
    }

    /// Rows oldest first, each `(actor, set id, club)`.
    fn rows(spec: &[(&str, Option<&str>, Option<Club>)]) -> Vec<ShotRow> {
        spec.iter()
            .map(|&(actor, group, club)| ShotRow {
                group: group.map(str::to_owned),
                club_selected: club,
                ..shot(actor)
            })
            .collect()
    }

    const I7: Option<Club> = Some(Club::Iron7);
    const DR: Option<Club> = Some(Club::Driver);

    /// Session 0: a 7-iron set. Session 1: 7I, DR, 7I sets, from two
    /// devices. Plus a row in no set.
    fn history() -> (Vec<ShotRow>, Vec<ShotGroupInfo>) {
        let shots = rows(&[
            ("mevo.0", Some("old"), I7),
            ("mevo.0", None, I7),
            ("mevo.0", Some("a"), I7),
            ("square.0", Some("a"), I7),
            ("mevo.0", Some("b"), DR),
            ("square.0", Some("b"), DR),
            ("mevo.0", Some("c"), I7),
            ("square.0", Some("c"), I7),
        ]);
        let list = vec![
            set("old", 1, I7, false),
            in_session(1, set("a", 2, I7, false)),
            in_session(1, set("b", 3, DR, false)),
            in_session(1, set("c", 4, I7, true)),
        ];
        (shots, list)
    }

    #[test]
    fn by_set_members_are_the_sets_rows() {
        let (shots, list) = history();
        let g = sets(list);
        let m = |set: &str, device| {
            members_for(&shots, &g, ShotGrouping::BySet, g.get(set), Some(6), device)
        };
        assert_eq!(m("c", None), vec![6, 7]);
        assert_eq!(m("a", None), vec![2, 3]);
        assert_eq!(m("old", None), vec![0]);
        // The device filter applies first.
        assert_eq!(m("c", Some("square.0")), vec![7]);
        assert!(m("c", Some("r10.0")).is_empty());
        // No set: nothing.
        assert!(members_for(&shots, &g, ShotGrouping::BySet, None, Some(1), None).is_empty());
    }

    #[test]
    fn by_club_members_combine_the_clubs_sets_in_the_session() {
        let (shots, list) = history();
        let g = sets(list);
        let m = |set: &str, device| {
            members_for(
                &shots,
                &g,
                ShotGrouping::ByClub,
                g.get(set),
                Some(6),
                device,
            )
        };
        // Both 7-iron sets since the last New session, not the driver set,
        // the earlier session's 7-iron set or the row in no set.
        assert_eq!(m("c", None), vec![2, 3, 6, 7]);
        assert_eq!(m("a", None), vec![2, 3, 6, 7], "either set of the club");
        assert_eq!(m("b", None), vec![4, 5]);
        // A shot from the earlier session shows that session's club.
        assert_eq!(m("old", None), vec![0]);
        // The device filter applies first.
        assert_eq!(m("c", Some("mevo.0")), vec![2, 6]);
        assert_eq!(m("b", Some("square.0")), vec![5]);
        assert!(members_for(&shots, &g, ShotGrouping::ByClub, None, Some(1), None).is_empty());
    }

    #[test]
    fn by_club_without_a_club_combines_the_no_club_sets() {
        let shots = rows(&[
            ("mevo.0", Some("x"), None),
            ("mevo.0", Some("d"), DR),
            ("mevo.0", Some("y"), None),
        ]);
        let g = sets(vec![
            set("x", 1, None, false),
            set("d", 2, DR, false),
            set("y", 3, None, true),
        ]);
        let y = g.get("y");
        assert_eq!(
            members_for(&shots, &g, ShotGrouping::ByClub, y, Some(2), None),
            vec![0, 2]
        );
        assert_eq!(
            members_for(&shots, &g, ShotGrouping::BySet, y, Some(2), None),
            vec![2]
        );
    }

    #[test]
    fn members_in_view_none_are_the_shown_shot_alone() {
        let (shots, list) = history();
        let g = sets(list);
        let m =
            |set, shown, device| members_for(&shots, &g, ShotGrouping::None, set, shown, device);
        for set in [None, g.get("c")] {
            assert_eq!(m(set, Some(6), None), vec![6]);
            assert_eq!(m(set, Some(1), None), vec![1], "a row in no set");
            // The device filter does not apply: the shown shot stays.
            assert_eq!(m(set, Some(4), Some("square.0")), vec![4]);
            assert!(m(set, None, None).is_empty());
            assert!(m(set, Some(shots.len()), None).is_empty());
        }
    }

    // -- caches --

    #[test]
    fn cached_rebuilds_only_for_a_new_key() {
        let mut c: Cached<u32, String> = Cached::default();
        assert!(c.peek(&1).is_none());
        let mut builds = 0;
        let mut get = |c: &mut Cached<u32, String>, k: u32| {
            c.get(k, || {
                builds += 1;
                format!("v{k}")
            })
            .clone()
        };
        assert_eq!(get(&mut c, 1), "v1");
        assert_eq!(get(&mut c, 1), "v1");
        assert_eq!(get(&mut c, 2), "v2");
        assert_eq!(get(&mut c, 1), "v1");
        assert_eq!(builds, 3);
        assert_eq!(c.peek(&1).map(String::as_str), Some("v1"));
        assert!(c.peek(&2).is_none(), "built for another key");
    }

    #[test]
    fn chart_key_changes_invalidate_and_idle_frames_do_not() {
        let members = MembersKey {
            shots_rev: 7,
            groups_rev: 3,
            view: ShotGrouping::BySet,
            device: None,
            shown: Some(4),
            club: I7,
        };
        let size = egui::vec2(320.0, 600.0);
        let base = ChartKey::new(members.clone(), UnitSystem::Imperial, size);
        let mut cache: Cached<ChartKey, ()> = Cached::default();
        let builds = std::cell::Cell::new(0);
        let mut frame = |key: ChartKey| {
            cache.get(key, || builds.set(builds.get() + 1));
        };
        frame(base.clone());
        // Idle frames, including sub-point jitter in the plot size.
        frame(base.clone());
        frame(ChartKey::new(
            members.clone(),
            UnitSystem::Imperial,
            egui::vec2(320.3, 599.8),
        ));
        assert_eq!(builds.get(), 1);
        let changed = [
            // A resize.
            ChartKey::new(
                members.clone(),
                UnitSystem::Imperial,
                egui::vec2(300.0, 600.0),
            ),
            // A unit flip.
            ChartKey::new(members.clone(), UnitSystem::Metric, size),
            // A new shot.
            ChartKey::new(
                MembersKey {
                    shots_rev: 8,
                    ..members.clone()
                },
                UnitSystem::Imperial,
                size,
            ),
            // A view change.
            ChartKey::new(
                MembersKey {
                    view: ShotGrouping::ByClub,
                    ..members.clone()
                },
                UnitSystem::Imperial,
                size,
            ),
            // A device filter.
            ChartKey::new(
                MembersKey {
                    device: Some("mevo.0".into()),
                    ..members.clone()
                },
                UnitSystem::Imperial,
                size,
            ),
            // Another shot shown.
            ChartKey::new(
                MembersKey {
                    shown: Some(2),
                    ..members.clone()
                },
                UnitSystem::Imperial,
                size,
            ),
            // A set list or session change.
            ChartKey::new(
                MembersKey {
                    groups_rev: 4,
                    ..members.clone()
                },
                UnitSystem::Imperial,
                size,
            ),
        ];
        for key in changed {
            let before = builds.get();
            frame(key.clone());
            assert_eq!(builds.get(), before + 1, "{key:?}");
            frame(key);
            assert_eq!(builds.get(), before + 1);
            frame(base.clone());
            assert_eq!(builds.get(), before + 2);
        }
    }
}
