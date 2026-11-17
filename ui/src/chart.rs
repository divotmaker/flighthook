//! Chart math for the Flight (side) and Dispersion (overhead) views and the
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
use crate::groups::{GolferKeys, SetInfo};
use crate::theme;
use crate::types::{Club, FlightPath, ShotGrouping, ShotRow, UnitSystem};

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
/// roll of a few yards or meters fills the plot.
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
/// neighboring ticks are at least `min_px` apart. A zero, negative or
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

    /// Equal scales on both axes with the origin at the bottom center of the
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
/// [`ELLIPSE_K_95`]) of the finite points in `points`: centered on their
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
/// `after` (equally spaced), so an end segment bends like its neighbors: a
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
/// neighbors share their end point. Empty for fewer than two points.
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

/// The distinct, non-empty actor ids in `shots` that statistics are kept
/// for (every one but the Auto-chip source, [`is_auto_chip`]), sorted: the
/// device filter's options, the range bars' per-device bands and the
/// charts' device colors. Ids are compared exactly, so a row without an
/// actor never merges two devices.
#[must_use]
pub(crate) fn device_ids(shots: &[ShotRow]) -> Vec<String> {
    let mut ids: Vec<String> = shots
        .iter()
        .filter(|s| !s.actor.is_empty() && !is_auto_chip(&s.actor))
        .map(|s| s.actor.clone())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Color of the device `id` among `ids` (as from [`device_ids`]):
/// [`theme::DEVICE_COLORS`] in order, then `TEXT_MUTED` for any further or
/// unknown device.
#[must_use]
pub(crate) fn device_color(ids: &[String], id: &str) -> Color32 {
    ids.iter()
        .position(|d| d == id)
        .and_then(|i| theme::DEVICE_COLORS.get(i).copied())
        .unwrap_or(theme::TEXT_MUTED)
}

/// Whether a chart over `devices` with the device filter `filter` colors
/// its shots by device: every device shown, and two or more of them.
#[must_use]
pub(crate) fn by_device(devices: &[String], filter: Option<&str>) -> bool {
    filter.is_none() && devices.len() >= 2
}

/// Color of a shot from `actor` in a chart over `devices` with the device
/// filter `filter`: its device's color when the chart colors by device
/// ([`by_device`]), else the accent. The Flight and Dispersion charts and
/// the face impact panel all color their shots by this rule.
#[must_use]
pub(crate) fn shot_color(devices: &[String], filter: Option<&str>, actor: &str) -> Color32 {
    if by_device(devices, filter) {
        device_color(devices, actor)
    } else {
        theme::ACCENT
    }
}

/// Whether `actor` is the Auto-chip source ([`flighthook::AUTO_CHIP_ACTOR`]).
/// Its shots are listed, shown and exported like any other, but no set or
/// club statistic counts them: they are never a view's members
/// ([`members_for`]), a set's or club's shot count ([`set_shot_count`]) or a
/// device of the device filter ([`device_ids`]).
#[must_use]
pub(crate) fn is_auto_chip(actor: &str) -> bool {
    actor == flighthook::AUTO_CHIP_ACTOR
}

/// What a [`ShotFilter`] picks: the device and the golfer key.
pub(crate) type FilterKey = (Option<String>, Option<Option<String>>);

/// The Shots tab's one filter: a device (an exact actor id) and the viewed
/// golfer (a golfer key by the lookup rule, `None` = shots with no golfer),
/// each unset for every shot. The shot list, the shot on show, the Table rows,
/// the view's members and the charts' faint shots all go through
/// [`ShotFilter::passes`]. An Auto-chip is no device, so a device filter
/// hides it like any other device's shot.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ShotFilter<'a> {
    /// The device filter: an exact actor id, or every device.
    pub(crate) device: Option<&'a str>,
    /// The viewed golfer: its key and the keys that resolve a shot's golfer
    /// id; every golfer when `None`.
    golfer: Option<(Option<&'a str>, &'a GolferKeys)>,
}

impl<'a> ShotFilter<'a> {
    /// Every shot.
    pub(crate) const ALL: Self = Self {
        device: None,
        golfer: None,
    };

    /// The device filter `device` alone.
    #[must_use]
    pub(crate) fn device(device: Option<&'a str>) -> Self {
        Self {
            device,
            golfer: None,
        }
    }

    /// This filter, narrowed to the golfer `key` (by `keys`) when `key` is
    /// `Some`.
    #[must_use]
    pub(crate) fn golfer(self, key: Option<Option<&'a str>>, keys: &'a GolferKeys) -> Self {
        Self {
            golfer: key.map(|k| (k, keys)),
            ..self
        }
    }

    /// What the filter picks, owned: the device and the golfer key.
    #[must_use]
    pub(crate) fn key(&self) -> FilterKey {
        (
            self.device.map(str::to_owned),
            self.golfer.map(|(k, _)| k.map(str::to_owned)),
        )
    }

    /// Whether the device filter narrows the shots. The viewed golfer is
    /// the view itself, not a narrowing.
    #[must_use]
    pub(crate) fn narrows(&self) -> bool {
        self.device.is_some()
    }

    /// Whether golfer id `golfer` resolves to the viewed golfer.
    #[must_use]
    pub(crate) fn passes_golfer(&self, golfer: Option<&str>) -> bool {
        self.golfer.is_none_or(|(k, keys)| keys.key(golfer) == k)
    }

    /// Whether the filter passes the shot `row`.
    #[must_use]
    pub(crate) fn passes(&self, row: &ShotRow) -> bool {
        self.device.is_none_or(|d| row.actor == d) && self.passes_golfer(row.golfer.as_deref())
    }
}

/// Label of a device: its name, else [`flighthook::AUTO_CHIP_NAME`] for the
/// Auto-chip source, else its actor id.
#[must_use]
pub(crate) fn device_label<'a>(id: &'a str, name: Option<&'a str>) -> &'a str {
    name.filter(|n| !n.is_empty())
        .or_else(|| (id == flighthook::AUTO_CHIP_ACTOR).then_some(flighthook::AUTO_CHIP_NAME))
        .unwrap_or(id)
}

// ---------------------------------------------------------------------------
// Members of the view
// ---------------------------------------------------------------------------

/// Indices into `shots`, ascending, of the rows the range bars and charts
/// show bright in `view`, among the rows from `device` (exact actor id;
/// every device when `None`). Auto-chip rows ([`is_auto_chip`]) are never
/// members of a set or club view.
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
    set: Option<&SetInfo>,
    shown: Option<usize>,
    filter: ShotFilter<'_>,
) -> Vec<usize> {
    if view == ShotGrouping::None {
        return shown.filter(|&i| i < shots.len()).into_iter().collect();
    }
    let Some(set) = set else {
        return Vec::new();
    };
    // A putting device's putt in another club's set is a putter shot: the
    // view around one is the putts'.
    let club = match shown.and_then(|i| shots.get(i)) {
        Some(r) if r.group.as_deref() == Some(set.id.as_str()) => row_club(r, set.club),
        _ => set.club,
    };
    let in_view = |r: &ShotRow| {
        let Some(id) = r.group.as_deref() else {
            return false;
        };
        if view == ShotGrouping::BySet {
            return id == set.id && row_club(r, set.club) == club;
        }
        let golfer = groups.golfers().key(set.golfer.as_deref());
        groups.get(id).is_some_and(|g| {
            g.session == set.session
                && row_club(r, g.club) == club
                && groups.golfers().key(g.golfer.as_deref()) == golfer
        })
    };
    shots
        .iter()
        .enumerate()
        .filter(|&(_, r)| filter.passes(r) && !is_auto_chip(&r.actor) && in_view(r))
        .map(|(i, _)| i)
        .collect()
}

/// The club a row of a set with club `club` counts under: the putter for a
/// putting device's putt (the store stamps it, whatever club is in play),
/// else the set's club.
#[must_use]
pub(crate) fn row_club(row: &ShotRow, club: Option<Club>) -> Option<Club> {
    if row.club_selected == Some(Club::Putter) {
        Some(Club::Putter)
    } else {
        club
    }
}

/// The shots of `set` its headers count: its shot count less its Auto-chip
/// shots ([`is_auto_chip`]) and putting-device putts of another club's set
/// ([`row_club`]) in `shots`; with a device filter `filter`, its rows in
/// `shots` from that device. A shot joins a set's count when it has data,
/// which is when it carries the set's id.
#[must_use]
pub(crate) fn set_shot_count(set: &SetInfo, shots: &[ShotRow], filter: ShotFilter<'_>) -> u32 {
    if filter.device.is_some() {
        let n = shots
            .iter()
            .filter(|r| filter.passes(r) && r.group.as_deref() == Some(set.id.as_str()))
            .count();
        return u32::try_from(n).unwrap_or(u32::MAX);
    }
    if !filter.passes_golfer(set.golfer.as_deref()) {
        let n = shots
            .iter()
            .filter(|r| {
                filter.passes(r)
                    && r.group.as_deref() == Some(set.id.as_str())
                    && !is_auto_chip(&r.actor)
                    && row_club(r, set.club) == set.club
            })
            .count();
        return u32::try_from(n).unwrap_or(u32::MAX);
    }
    let chips = shots
        .iter()
        .filter(|r| {
            r.group.as_deref() == Some(set.id.as_str())
                && (is_auto_chip(&r.actor) || row_club(r, set.club) != set.club)
        })
        .count();
    set.shots
        .saturating_sub(u32::try_from(chips).unwrap_or(u32::MAX))
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
    /// The viewed golfer's key (`None` = no golfer).
    pub(crate) golfer: Option<String>,
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
mod tests;
