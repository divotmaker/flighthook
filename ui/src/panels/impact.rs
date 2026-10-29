//! Face impact: where the ball met the club face.
//!
//! Club-face outlines per club family, drawn face-on as the ball sees them
//! (a right-hander's toe on the left, a left-hander's mirrored), the words
//! the impact reads as ("4 toe", "2 low"), and the Cards view's face panel.
//!
//! Face impact is golfer-relative (FRP: `lateral` + toward the toe,
//! `vertical` + above the face centre) and is never mirrored for a
//! left-hander; only the drawing is.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;

use egui::{Color32, Pos2, Rect, Stroke, pos2, vec2};

use crate::chart;
use crate::theme;
use crate::types::{Club, FaceImpact, Handedness, ShotRow};

// ---------------------------------------------------------------------------
// Outlines
// ---------------------------------------------------------------------------

// Every outline below is a generic club-face silhouette, not any brand's
// head: a closed chain of cubic Béziers in face millimetres, origin at the
// face centre (the midpoint of both the horizontal and the vertical chord
// through it), +x toward the toe, +y up, walked counter-clockwise. Stored
// flat: the start point, then `c1, c2, end` per segment; the last end is
// the start. Straight runs have their handles at the thirds.

/// Driver and mini driver (DR, MD).
const DRIVER: [[f32; 2]; 28] = [
    [-48.266, -15.986],
    [-44.194, -22.503],
    [-37.446, -26.885],
    [-29.836, -27.955],
    [-20.812, -29.223],
    [-8.731, -30.275],
    [3.341, -30.275],
    [25.332, -30.275],
    [58.341, -26.802],
    [58.341, -2.155],
    [58.341, 13.264],
    [45.317, 30.345],
    [5.341, 30.345],
    [-6.868, 30.345],
    [-31.524, 28.642],
    [-49.553, 20.615],
    [-51.830, 19.601],
    [-54.506, 20.419],
    [-55.827, 22.533],
    [-58.062, 26.110],
    [-60.298, 29.687],
    [-62.533, 33.264],
    [-66.490, 30.791],
    [-70.448, 28.318],
    [-74.405, 25.845],
    [-65.692, 11.901],
    [-56.979, -2.042],
    [-48.266, -15.986],
];

/// Fairway woods (2W-9W).
const WOOD: [[f32; 2]; 28] = [
    [-42.074, -10.677],
    [-38.854, -15.635],
    [-33.605, -18.914],
    [-27.738, -19.635],
    [-11.844, -21.586],
    [2.390, -21.638],
    [5.015, -21.638],
    [14.922, -21.638],
    [49.015, -20.933],
    [49.015, -0.535],
    [49.015, 9.840],
    [40.188, 22.165],
    [12.015, 22.165],
    [-3.459, 22.165],
    [-25.867, 18.531],
    [-39.381, 13.613],
    [-41.349, 12.896],
    [-43.552, 13.634],
    [-44.694, 15.391],
    [-47.857, 20.261],
    [-51.019, 25.131],
    [-54.182, 30.001],
    [-57.537, 27.822],
    [-60.891, 25.644],
    [-64.246, 23.465],
    [-56.855, 12.084],
    [-49.465, 0.704],
    [-42.074, -10.677],
];

/// Hybrids (1H-9H).
const HYBRID: [[f32; 2]; 28] = [
    [-37.774, -12.951],
    [-35.758, -16.307],
    [-32.231, -18.468],
    [-28.326, -18.741],
    [-9.304, -20.072],
    [7.891, -20.119],
    [11.129, -20.119],
    [16.530, -20.119],
    [45.629, -19.997],
    [45.629, -1.641],
    [45.629, 5.887],
    [40.786, 21.359],
    [18.129, 21.359],
    [5.932, 21.359],
    [-24.264, 17.156],
    [-35.771, 13.638],
    [-37.747, 13.034],
    [-39.880, 13.853],
    [-40.944, 15.624],
    [-43.431, 19.762],
    [-45.917, 23.901],
    [-48.404, 28.039],
    [-51.833, 25.979],
    [-55.261, 23.919],
    [-58.690, 21.859],
    [-51.718, 10.256],
    [-44.746, -1.348],
    [-37.774, -12.951],
];

/// Irons (1I-9I) and the pitching wedge; also the face for an unknown club.
const IRON: [[f32; 2]; 28] = [
    [-30.915, -16.851],
    [-29.710, -19.167],
    [-27.316, -20.619],
    [-24.706, -20.619],
    [21.931, -20.619],
    [30.580, -20.512],
    [35.946, -16.319],
    [39.498, -13.544],
    [40.546, -9.669],
    [40.546, -6.819],
    [40.546, -0.024],
    [34.582, 27.697],
    [20.046, 26.681],
    [12.202, 26.132],
    [-10.624, 16.355],
    [-23.441, 10.915],
    [-26.803, 9.487],
    [-30.698, 10.886],
    [-32.385, 14.126],
    [-36.160, 21.379],
    [-39.936, 28.631],
    [-43.711, 35.884],
    [-47.555, 33.883],
    [-51.398, 31.882],
    [-55.242, 29.881],
    [-47.133, 14.304],
    [-39.024, -1.274],
    [-30.915, -16.851],
];

/// Gap, sand and lob wedges (GW, SW, LW).
const WEDGE: [[f32; 2]; 28] = [
    [-30.603, -17.371],
    [-29.430, -19.777],
    [-26.988, -21.303],
    [-24.312, -21.303],
    [20.548, -21.303],
    [27.382, -21.932],
    [33.614, -16.703],
    [39.035, -12.154],
    [39.314, -7.101],
    [39.314, -4.403],
    [39.314, 10.736],
    [30.523, 28.265],
    [18.314, 27.197],
    [9.255, 26.405],
    [-16.270, 14.161],
    [-21.230, 11.634],
    [-25.216, 9.603],
    [-30.092, 11.235],
    [-32.052, 15.255],
    [-35.977, 23.302],
    [-39.902, 31.349],
    [-43.827, 39.396],
    [-47.722, 37.496],
    [-51.616, 35.597],
    [-55.511, 33.697],
    [-47.208, 16.674],
    [-38.906, -0.348],
    [-30.603, -17.371],
];

/// Putter (PT): a blade with a neck.
const PUTTER: [[f32; 2]; 46] = [
    [-52.000, -8.500],
    [-52.000, -10.433],
    [-50.433, -12.000],
    [-48.500, -12.000],
    [-16.167, -12.000],
    [16.167, -12.000],
    [48.500, -12.000],
    [50.433, -12.000],
    [52.000, -10.433],
    [52.000, -8.500],
    [52.000, -2.833],
    [52.000, 2.833],
    [52.000, 8.500],
    [52.000, 10.433],
    [50.433, 12.000],
    [48.500, 12.000],
    [23.953, 12.000],
    [-0.595, 12.000],
    [-25.142, 12.000],
    [-26.193, 12.000],
    [-27.131, 12.657],
    [-27.491, 13.645],
    [-29.721, 19.772],
    [-31.951, 25.899],
    [-34.181, 32.026],
    [-36.060, 31.342],
    [-37.940, 30.658],
    [-39.819, 29.974],
    [-38.045, 25.101],
    [-36.272, 20.228],
    [-34.498, 15.355],
    [-34.219, 14.589],
    [-34.332, 13.734],
    [-34.800, 13.066],
    [-35.267, 12.398],
    [-36.032, 12.000],
    [-36.847, 12.000],
    [-40.731, 12.000],
    [-44.616, 12.000],
    [-48.500, 12.000],
    [-50.433, 12.000],
    [-52.000, 10.433],
    [-52.000, 8.500],
    [-52.000, 2.833],
    [-52.000, -2.833],
    [-52.000, -8.500],
];

/// One family of club faces, by shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FaceFamily {
    Driver,
    Wood,
    Hybrid,
    Iron,
    Wedge,
    Putter,
}

impl FaceFamily {
    pub(crate) const ALL: [FaceFamily; 6] = [
        FaceFamily::Driver,
        FaceFamily::Wood,
        FaceFamily::Hybrid,
        FaceFamily::Iron,
        FaceFamily::Wedge,
        FaceFamily::Putter,
    ];

    /// The face `club` has; an iron when the club is unknown.
    pub(crate) fn of(club: Option<Club>) -> Self {
        use Club::{
            Driver, GapWedge, Hybrid1, Hybrid2, Hybrid3, Hybrid4, Hybrid5, Hybrid6, Hybrid7,
            Hybrid8, Hybrid9, Iron1, Iron2, Iron3, Iron4, Iron5, Iron6, Iron7, Iron8, Iron9,
            LobWedge, MiniDriver, PitchingWedge, Putter, SandWedge, Wood2, Wood3, Wood4, Wood5,
            Wood6, Wood7, Wood8, Wood9,
        };
        match club {
            Some(Driver | MiniDriver) => Self::Driver,
            Some(Wood2 | Wood3 | Wood4 | Wood5 | Wood6 | Wood7 | Wood8 | Wood9) => Self::Wood,
            Some(
                Hybrid1 | Hybrid2 | Hybrid3 | Hybrid4 | Hybrid5 | Hybrid6 | Hybrid7 | Hybrid8
                | Hybrid9,
            ) => Self::Hybrid,
            Some(
                Iron1 | Iron2 | Iron3 | Iron4 | Iron5 | Iron6 | Iron7 | Iron8 | Iron9
                | PitchingWedge,
            )
            | None => Self::Iron,
            Some(GapWedge | SandWedge | LobWedge) => Self::Wedge,
            Some(Putter) => Self::Putter,
        }
    }

    /// The outline's control points (see the consts above).
    fn control_points(self) -> &'static [[f32; 2]] {
        match self {
            Self::Driver => &DRIVER,
            Self::Wood => &WOOD,
            Self::Hybrid => &HYBRID,
            Self::Iron => &IRON,
            Self::Wedge => &WEDGE,
            Self::Putter => &PUTTER,
        }
    }

    /// Sizes measured once from the outline.
    fn geometry(self) -> &'static Geometry {
        static ALL: LazyLock<Vec<Geometry>> =
            LazyLock::new(|| FaceFamily::ALL.map(Geometry::measure).into());
        &ALL[self as usize]
    }
}

/// An outline's sizes in face millimetres.
#[derive(Debug, Clone)]
struct Geometry {
    /// Bounding box of the whole silhouette, hosel included:
    /// `[min x, min y, max x, max y]`.
    bbox: [f32; 4],
    /// Where the horizontal line through the face centre enters and leaves
    /// the outline (heel, toe).
    across: (f32, f32),
    /// Where the vertical line through the face centre enters and leaves
    /// it (bottom, top).
    up: (f32, f32),
    /// The outline flattened finely, for inside tests.
    polygon: Vec<Pos2>,
}

impl Geometry {
    fn measure(family: FaceFamily) -> Self {
        // 4 points per mm at 0.1 point: within 0.025 mm of the curves.
        let fine = flatten(family.control_points(), 4.0);
        let (x, y): (Vec<f32>, Vec<f32>) = fine.iter().map(|p| (p[0], p[1])).unzip();
        let lo = |v: &[f32]| v.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = |v: &[f32]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        Self {
            bbox: [lo(&x), lo(&y), hi(&x), hi(&y)],
            across: chord(&fine, Axis::Horizontal, 0.0),
            up: chord(&fine, Axis::Vertical, 0.0),
            polygon: fine.iter().map(|p| pos2(p[0], p[1])).collect(),
        }
    }
}

/// The direction of a chord through the outline.
#[derive(Clone, Copy)]
enum Axis {
    /// The line `y = at`.
    Horizontal,
    /// The line `x = at`.
    Vertical,
}

/// Where the line `axis` at `at` first enters and last leaves the closed
/// `polygon`, along the line. `(0, 0)` when it misses.
fn chord(polygon: &[[f32; 2]], axis: Axis, at: f32) -> (f32, f32) {
    let n = polygon.len();
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for i in 0..n {
        let (p, q) = (polygon[i], polygon[(i + 1) % n]);
        let (a0, a1, b0, b1) = match axis {
            Axis::Vertical => (p[0], q[0], p[1], q[1]),
            Axis::Horizontal => (p[1], q[1], p[0], q[0]),
        };
        if (a0 - at) * (a1 - at) <= 0.0 && (a1 - a0).abs() > 0.0 {
            let b = b0 + (b1 - b0) * (at - a0) / (a1 - a0);
            lo = lo.min(b);
            hi = hi.max(b);
        }
    }
    if lo <= hi { (lo, hi) } else { (0.0, 0.0) }
}

/// Chord error of the flattened outline, in screen points.
const FLATNESS: f32 = 0.1;

/// Line segments one cubic flattens to at `k` points per mm so the
/// polyline stays within [`FLATNESS`] of the curve (Wang's formula), 1 to
/// 64.
fn steps(seg: &[[f32; 2]], k: f32) -> usize {
    let second = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
        (a[0] - 2.0 * b[0] + c[0]).hypot(a[1] - 2.0 * b[1] + c[1])
    };
    let m = second(seg[0], seg[1], seg[2]).max(second(seg[1], seg[2], seg[3])) * k;
    let n = (0.75 * m / FLATNESS).sqrt().ceil();
    // In 1..=64, so the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = n.clamp(1.0, 64.0) as usize;
    n
}

/// The point at `t` on the cubic `seg`.
fn bezier(seg: &[[f32; 2]], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    let at = |i: usize| w[0] * seg[0][i] + w[1] * seg[1][i] + w[2] * seg[2][i] + w[3] * seg[3][i];
    [at(0), at(1)]
}

/// The closed chain of cubics `points` flattened for drawing at `k`
/// points per mm: uniform steps in `t` per segment (see [`steps`]), in face
/// millimetres, without repeating the start at the end.
fn flatten(points: &[[f32; 2]], k: f32) -> Vec<[f32; 2]> {
    let mut out = Vec::new();
    for seg in points.windows(4).step_by(3) {
        let n = steps(seg, k);
        #[allow(clippy::cast_precision_loss)]
        out.extend((0..n).map(|i| bezier(seg, i as f32 / n as f32)));
    }
    out
}

/// Twice the signed area of the polygon `p`: positive counter-clockwise.
fn area2(p: &[[f32; 2]]) -> f64 {
    let n = p.len();
    (0..n)
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % n]);
            f64::from(a[0]) * f64::from(b[1]) - f64::from(b[0]) * f64::from(a[1])
        })
        .sum()
}

/// Turn at `b` going `a` → `b` → `c`: positive for a left turn.
fn cross(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f64 {
    let (ax, ay) = (f64::from(b[0] - a[0]), f64::from(b[1] - a[1]));
    let (bx, by) = (f64::from(c[0] - b[0]), f64::from(c[1] - b[1]));
    ax * by - ay * bx
}

/// Triangle indices filling the simple polygon `p` (ear clipping). The fill
/// needs them because egui fills only convex paths, and every head is
/// concave at the neck.
#[allow(clippy::many_single_char_names)] // a triangle's corners and points
fn triangulate(p: &[[f32; 2]]) -> Vec<u32> {
    let mut ring: Vec<usize> = (0..p.len()).collect();
    if area2(p) < 0.0 {
        ring.reverse();
    }
    let same = |q: [f32; 2], r: [f32; 2]| (q[0] - r[0]).abs() + (q[1] - r[1]).abs() == 0.0;
    let inside = |q: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
        cross(a, b, q) >= 0.0 && cross(b, c, q) >= 0.0 && cross(c, a, q) >= 0.0
    };
    let mut out = Vec::with_capacity(p.len().saturating_sub(2) * 3);
    while ring.len() > 3 {
        let m = ring.len();
        let ear = (0..m).find_map(|i| {
            let (a, b, c) = (ring[(i + m - 1) % m], ring[i], ring[(i + 1) % m]);
            let turn = cross(p[a], p[b], p[c]);
            if turn.abs() < 1e-9 {
                // A point on a straight run: drop it, no triangle.
                return Some((i, None));
            }
            // A point on one of the corners (a repeated vertex) does not
            // block the ear.
            let corner = |j: usize| [a, b, c].iter().any(|&k| j == k || same(p[j], p[k]));
            let blocked = turn < 0.0
                || ring
                    .iter()
                    .any(|&j| !corner(j) && inside(p[j], p[a], p[b], p[c]));
            (!blocked).then_some((i, Some([a, b, c])))
        });
        let Some((i, triangle)) = ear else {
            // Not a simple polygon: leave the rest unfilled.
            break;
        };
        if let Some(t) = triangle {
            // Outline vertex counts are far below u32::MAX.
            #[allow(clippy::cast_possible_truncation)]
            out.extend(t.map(|v| v as u32));
        }
        ring.remove(i);
    }
    if ring.len() == 3 {
        #[allow(clippy::cast_possible_truncation)]
        out.extend(ring.iter().map(|&v| v as u32));
    }
    out
}

/// A family's outline flattened for one drawing scale, with its fill
/// triangles.
#[derive(Debug)]
struct Outline {
    /// Face millimetres.
    points: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

/// The outline of `family` at `k` points per mm, built once per family
/// and scale.
fn outline(family: FaceFamily, k: f32) -> Rc<Outline> {
    thread_local! {
        static CACHE: RefCell<HashMap<(FaceFamily, u32), Rc<Outline>>> =
            RefCell::new(HashMap::new());
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 64 {
            cache.clear();
        }
        cache
            .entry((family, k.to_bits()))
            .or_insert_with(|| {
                let points = flatten(family.control_points(), k);
                let indices = triangulate(&points);
                Rc::new(Outline { points, indices })
            })
            .clone()
    })
}

// ---------------------------------------------------------------------------
// Drawing frame
// ---------------------------------------------------------------------------

/// Share of the plot's width and height a face may fill.
const FIT_WIDTH: f32 = 0.94;
const FIT_HEIGHT: f32 = 0.86;
/// How far (points) the silhouette's centre sits below the plot's middle:
/// the face hangs a touch nearer its TOE / HEEL line than the readout.
const FIT_DROP: f32 = 2.0;

/// Where a face is drawn: face millimetres to screen points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FaceView {
    pub(crate) family: FaceFamily,
    /// Points per millimetre.
    pub(crate) k: f32,
    /// The face centre on screen.
    pub(crate) centre: Pos2,
    /// -1 when the toe is on the left (a right-hander), +1 mirrored.
    side: f32,
}

impl FaceView {
    /// `family`'s face fitted into `plot` for a `handed` golfer. This is
    /// the one place a face's scale is set.
    ///
    /// The rule: each family is scaled on its own, as large as its whole
    /// silhouette (hosel included) fits within [`FIT_WIDTH`] of the plot's
    /// width and [`FIT_HEIGHT`] of its height, `k = min(FIT_WIDTH · w /
    /// bbox w, FIT_HEIGHT · h / bbox h)` points per mm. So every face fills
    /// the panel, and two families are not drawn to one scale: a wood and a
    /// hybrid look about the same size. The silhouette is centred, its
    /// centre [`FIT_DROP`] below the plot's middle.
    pub(crate) fn fit(plot: Rect, family: FaceFamily, handed: Handedness) -> Self {
        let [x0, y0, x1, y1] = family.geometry().bbox;
        let k = (FIT_WIDTH * plot.width() / (x1 - x0)).min(FIT_HEIGHT * plot.height() / (y1 - y0));
        let k = k.max(0.0);
        let side = match handed {
            Handedness::Right => -1.0,
            Handedness::Left => 1.0,
        };
        let middle = plot.center();
        let centre = pos2(
            middle.x - side * f32::midpoint(x0, x1) * k,
            middle.y + FIT_DROP + f32::midpoint(y0, y1) * k,
        );
        Self {
            family,
            k,
            centre,
            side,
        }
    }

    /// Screen point of `lat` mm toward the toe and `vert` mm up.
    pub(crate) fn to_screen(self, lat: f32, vert: f32) -> Pos2 {
        pos2(
            self.centre.x + self.side * lat * self.k,
            self.centre.y - vert * self.k,
        )
    }

    fn map(&self, p: [f32; 2]) -> Pos2 {
        self.to_screen(p[0], p[1])
    }

    /// The outline on screen, closed, without repeating its start.
    fn outline_points(&self) -> Vec<Pos2> {
        outline(self.family, self.k)
            .points
            .iter()
            .map(|&p| self.map(p))
            .collect()
    }

    /// The face filled: a mesh over the outline's triangles.
    fn fill(&self, colour: Color32) -> egui::Mesh {
        let outline = outline(self.family, self.k);
        let mut mesh = egui::Mesh::default();
        for &p in &outline.points {
            mesh.colored_vertex(self.map(p), colour);
        }
        mesh.indices.clone_from(&outline.indices);
        mesh
    }
}

/// Whether `lat`, `vert` (mm) is on `family`'s face.
fn on_face(family: FaceFamily, lat: f32, vert: f32) -> bool {
    chart::inside_polygon(&family.geometry().polygon, pos2(lat, vert))
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

/// Signed millimetres of a reading, when finite.
fn mm(d: Option<flighthook::Distance>) -> Option<f64> {
    d.map(flighthook::Distance::as_millimeters)
        .filter(|v| v.is_finite())
}

/// `impact`'s lateral (+ toe) and vertical (+ high) millimetres.
pub(crate) fn impact_mm(impact: Option<&FaceImpact>) -> (Option<f64>, Option<f64>) {
    impact.map_or((None, None), |i| (mm(i.lateral), mm(i.vertical)))
}

/// Whether a shot has a face impact reading on either axis.
pub(crate) fn has_impact(shot: &ShotRow) -> bool {
    let (lat, vert) = impact_mm(shot.impact.as_ref());
    lat.is_some() || vert.is_some()
}

/// Whole millimetres and a side word: `4 toe`, `6 heel`; `0` under half
/// a millimetre. `unit` goes between the number and the word.
fn side_words(v: f64, unit: &str, positive: &str, negative: &str) -> String {
    let whole = v.abs().round();
    if whole == 0.0 {
        return "0".to_owned();
    }
    let word = if v > 0.0 { positive } else { negative };
    format!("{whole:.0}{unit} {word}")
}

/// The lateral and vertical readings as words, `-` when absent:
/// `("4 toe", "2 low")`.
pub(crate) fn impact_words(lat: Option<f64>, vert: Option<f64>) -> (String, String) {
    (
        lat.map_or_else(|| "-".to_owned(), |v| side_words(v, "", "toe", "heel")),
        vert.map_or_else(|| "-".to_owned(), |v| side_words(v, "", "high", "low")),
    )
}

/// The panel's readout line: `4 toe · 2 low`, one axis alone when only one
/// was read, `-` with none.
fn readout(lat: Option<f64>, vert: Option<f64>) -> String {
    let (h, v) = impact_words(lat, vert);
    match (lat, vert) {
        (Some(_), Some(_)) => format!("{h} \u{b7} {v}"),
        (Some(_), None) => h,
        (None, Some(_)) => v,
        (None, None) => "-".to_owned(),
    }
}

/// A shot's impact with units, for a tooltip: `4 mm toe · 2 mm low`.
fn impact_hover(lat: Option<f64>, vert: Option<f64>) -> String {
    let h = lat.map(|v| side_words(v, " mm", "toe", "heel"));
    let v = vert.map(|v| side_words(v, " mm", "high", "low"));
    let zero = |s: String| if s == "0" { "0 mm".to_owned() } else { s };
    match (h.map(zero), v.map(zero)) {
        (Some(h), Some(v)) => format!("{h} \u{b7} {v}"),
        (Some(one), None) | (None, Some(one)) => one,
        (None, None) => "-".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Face panel
// ---------------------------------------------------------------------------

/// Padding inside the panel.
pub(crate) const PAD: f32 = 10.0;
/// The readout line's height at the top.
const HEAD_H: f32 = 24.0;
/// The TOE / HEEL line's height at the bottom.
const FOOT_H: f32 = 16.0;
/// Readout font size, and the least it shrinks to in a narrow panel.
const READOUT_FONT: f32 = 12.5;
const READOUT_MIN: f32 = 10.0;
/// Outline stroke width.
const OUTLINE_W: f32 = 1.2;
/// Radius of a member's dot, and its halo.
const DOT_R: f32 = 2.8;
const DOT_HALO_R: f32 = 3.6;
/// Radius of the shown shot's dot, and its halo.
const SHOWN_R: f32 = 5.0;
const SHOWN_HALO_R: f32 = 6.5;
/// Half-length of the mean cross's arms.
const MEAN_ARM: f32 = 5.0;
/// How near the pointer must come to a dot to hit it.
const DOT_HIT: f32 = 8.0;
/// The crosshair stops this far inside the face's edges (mm).
const CROSS_INSET_X: f32 = 5.0;
const CROSS_INSET_Y: f32 = 4.0;
/// Least members with both axes for the mean cross.
const MEAN_MIN: usize = 3;

/// One member's mark.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Dot {
    /// Index into the shot list.
    pub(crate) index: usize,
    pub(crate) lat: f64,
    pub(crate) vert: f64,
    pub(crate) colour: Color32,
    /// The hover text.
    pub(crate) hover: String,
}

/// What the face panel shows, built from the view's members.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FacePanel {
    pub(crate) family: FaceFamily,
    pub(crate) handed: Handedness,
    /// The shown shot's index and readings (either axis may be missing),
    /// when it is a member with a reading.
    pub(crate) shown: Option<(usize, Option<f64>, Option<f64>)>,
    /// The shown shot's hover text.
    pub(crate) shown_hover: String,
    /// The other members with both axes.
    pub(crate) dots: Vec<Dot>,
    /// Mean of the members with both axes (the shown shot among them when
    /// it is a member), when there are at least [`MEAN_MIN`].
    pub(crate) mean: Option<(f64, f64)>,
    pub(crate) readout: String,
}

/// How the panel names a shot in its hover: `#5282 · 7I`, and the device
/// when `device` is given.
fn shot_title(shot: &ShotRow, device: Option<&str>) -> String {
    let club = shot.club_selected.map(|c| c.to_string());
    std::iter::once(format!("#{}", shot.shot_number))
        .chain(club)
        .chain(device.map(str::to_owned))
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

impl FacePanel {
    /// The panel for the shown shot `shots[shown]` among `members`
    /// (indices into `shots`, the range bars' members). The face is the
    /// shown shot's club's, else `club`'s; the hand is the shown shot's,
    /// else `handed`, else right. `colour` and `device` give a member's dot
    /// colour and the device name its hover carries.
    ///
    /// The shown shot is marked only when it is a member, as on the range
    /// bars and the charts: when the device filter leaves it out, the panel
    /// has no gold mark and the readout is `-`.
    pub(crate) fn new(
        shots: &[ShotRow],
        members: &[usize],
        shown: usize,
        club: Option<Club>,
        handed: Option<Handedness>,
        colour: impl Fn(&ShotRow) -> Color32,
        device: impl Fn(&ShotRow) -> Option<String>,
    ) -> Self {
        let row = shots.get(shown);
        let family = FaceFamily::of(row.and_then(|r| r.club_selected).or(club));
        let handed = row
            .and_then(|r| r.handed)
            .or(handed)
            .unwrap_or(Handedness::Right);
        let read = |r: &ShotRow| impact_mm(r.impact.as_ref());
        let (lat, vert) = row
            .filter(|_| members.contains(&shown))
            .map_or((None, None), read);
        let hover = |r: &ShotRow, lat, vert| {
            format!(
                "{}\n{}",
                shot_title(r, device(r).as_deref()),
                impact_hover(lat, vert)
            )
        };
        let mut both = Vec::new();
        let mut dots = Vec::new();
        for &i in members {
            let Some(r) = shots.get(i) else { continue };
            let (Some(a), Some(b)) = read(r) else {
                continue;
            };
            both.push((a, b));
            if i != shown {
                dots.push(Dot {
                    index: i,
                    lat: a,
                    vert: b,
                    colour: colour(r),
                    hover: hover(r, Some(a), Some(b)),
                });
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let mean = (both.len() >= MEAN_MIN).then(|| {
            let n = both.len() as f64;
            let (a, b) = both
                .iter()
                .fold((0.0, 0.0), |acc, p| (acc.0 + p.0, acc.1 + p.1));
            (a / n, b / n)
        });
        let has = lat.is_some() || vert.is_some();
        Self {
            family,
            handed,
            shown: (row.is_some() && has).then_some((shown, lat, vert)),
            shown_hover: row.map(|r| hover(r, lat, vert)).unwrap_or_default(),
            dots,
            mean,
            readout: readout(lat, vert),
        }
    }
}

/// The readout `text` laid out to fit `width`: at [`READOUT_FONT`], shrunk
/// in half-point steps to [`READOUT_MIN`] while it is wider, and cut short
/// with an ellipsis if it is still wider there.
pub(crate) fn readout_galley(
    painter: &egui::Painter,
    text: &str,
    width: f32,
    colour: Color32,
) -> std::sync::Arc<egui::Galley> {
    let mut size = READOUT_FONT;
    loop {
        let galley =
            painter.layout_no_wrap(text.to_owned(), egui::FontId::proportional(size), colour);
        if galley.size().x <= width {
            return galley;
        }
        if size <= READOUT_MIN {
            break;
        }
        size = (size - 0.5).max(READOUT_MIN);
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_owned(),
        egui::FontId::proportional(READOUT_MIN),
        colour,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    painter.layout_job(job)
}

/// The plot area of a face panel at `rect`: inside the padding, under the
/// readout line and over the TOE / HEEL line.
fn plot_rect(rect: Rect) -> Rect {
    Rect::from_min_max(
        pos2(rect.left() + PAD, rect.top() + HEAD_H + 4.0),
        pos2(rect.right() - PAD, rect.bottom() - FOOT_H - 2.0),
    )
}

/// The view `panel` draws in `rect`.
pub(crate) fn view(rect: Rect, panel: &FacePanel) -> FaceView {
    FaceView::fit(plot_rect(rect), panel.family, panel.handed)
}

/// Where a mark at `lat`, `vert` is drawn: on its spot, pulled into the
/// plot when it is off it; and whether it is on the face.
fn mark_at(view: FaceView, plot: Rect, lat: f64, vert: f64) -> (Pos2, bool) {
    #[allow(clippy::cast_possible_truncation)]
    let (lat, vert) = (lat as f32, vert as f32);
    let at = view.to_screen(lat, vert);
    (plot.clamp(at), on_face(view.family, lat, vert))
}

/// A dot, filled when it is on the face and a ring when it is off it.
fn paint_dot(painter: &egui::Painter, at: Pos2, r: f32, halo: f32, colour: Color32, on: bool) {
    painter.circle_filled(at, halo, theme::BG_DEEP);
    if on {
        painter.circle_filled(at, r, colour);
    } else {
        painter.circle_stroke(at, r - 0.75, Stroke::new(1.5, colour));
    }
}

/// Caps text, as the Cards view's labels.
fn caps(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    colour: Color32,
) -> std::sync::Arc<egui::Galley> {
    use egui::text::{LayoutJob, TextFormat};
    painter.layout_job(LayoutJob::single_section(
        text.to_uppercase(),
        TextFormat {
            font_id: egui::FontId::proportional(size),
            color: colour,
            extra_letter_spacing: 0.6,
            ..Default::default()
        },
    ))
}

/// The shot under `pointer`: the nearest dot within [`DOT_HIT`], the shown
/// shot's first.
fn hit(view: FaceView, plot: Rect, panel: &FacePanel, pointer: Pos2) -> Option<(usize, String)> {
    let shown = panel
        .shown
        .and_then(|(i, lat, vert)| Some((i, lat?, vert?, panel.shown_hover.clone())));
    shown
        .into_iter()
        .chain(
            panel
                .dots
                .iter()
                .map(|d| (d.index, d.lat, d.vert, d.hover.clone())),
        )
        .map(|(i, lat, vert, text)| {
            let (at, _) = mark_at(view, plot, lat, vert);
            (at.distance(pointer), i, text)
        })
        .filter(|(d, _, _)| *d <= DOT_HIT)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, i, text)| (i, text))
}

/// Paint the face of `view` in the panel at `rect`: the fill, the face
/// centre's crosshair, the outline, then TOE and HEEL under the face's
/// ends with mm between them.
fn paint_face(painter: &egui::Painter, rect: Rect, view: FaceView) {
    let geometry = view.family.geometry();
    let face = theme::mix(theme::SURFACE, theme::SURFACE_HOVER, 0.75);
    let outline_colour = theme::mix(theme::TEXT_DIM, theme::TEXT_MUTED, 0.4);
    painter.add(egui::Shape::mesh(view.fill(face)));

    // The face centre's crosshair, inside the face.
    let (heel, toe) = geometry.across;
    let (bottom, top) = geometry.up;
    let dash = Stroke::new(1.0, theme::mix(face, theme::TEXT_DIM, 0.9));
    for line in [
        [
            view.to_screen(heel + CROSS_INSET_X, 0.0),
            view.to_screen(toe - CROSS_INSET_X, 0.0),
        ],
        [
            view.to_screen(0.0, bottom + CROSS_INSET_Y),
            view.to_screen(0.0, top - CROSS_INSET_Y),
        ],
    ] {
        painter.extend(egui::Shape::dashed_line(&line, dash, 3.0, 3.0));
    }
    painter.add(egui::Shape::closed_line(
        view.outline_points(),
        Stroke::new(OUTLINE_W, outline_colour),
    ));

    // TOE and HEEL under the face's ends, mm between them.
    let foot = rect.bottom() - FOOT_H + 1.0;
    for (word, at) in [("toe", toe - 6.0), ("heel", heel + 6.0)] {
        let g = caps(painter, word, 9.0, theme::TEXT_DIM);
        let x = view.to_screen(at, 0.0).x;
        painter.galley(pos2(x - g.size().x / 2.0, foot), g, theme::TEXT_DIM);
    }
    let unit = painter.layout_no_wrap(
        "mm".to_owned(),
        egui::FontId::proportional(10.0),
        theme::TEXT_DIM,
    );
    painter.galley(
        pos2(rect.center().x - unit.size().x / 2.0, foot),
        unit,
        theme::TEXT_DIM,
    );
}

/// Paint `panel`'s marks in `view`: the members, the mean, then the shown
/// shot on top. A mark off the plot is pulled in.
fn paint_marks(painter: &egui::Painter, view: FaceView, plot: Rect, panel: &FacePanel) {
    let (heel, toe) = view.family.geometry().across;
    let (bottom, top) = view.family.geometry().up;
    for d in &panel.dots {
        let (at, on) = mark_at(view, plot, d.lat, d.vert);
        paint_dot(painter, at, DOT_R, DOT_HALO_R, d.colour, on);
    }
    if let Some((lat, vert)) = panel.mean {
        let (at, _) = mark_at(view, plot, lat, vert);
        for (width, colour) in [(3.4, theme::BG_DEEP), (1.6, theme::GOLD)] {
            let stroke = Stroke::new(width, colour);
            painter.line_segment([at - vec2(MEAN_ARM, 0.0), at + vec2(MEAN_ARM, 0.0)], stroke);
            painter.line_segment([at - vec2(0.0, MEAN_ARM), at + vec2(0.0, MEAN_ARM)], stroke);
        }
    }
    if let Some((_, lat, vert)) = panel.shown {
        let line = match (lat, vert) {
            (Some(lat), Some(vert)) => {
                let (at, on) = mark_at(view, plot, lat, vert);
                paint_dot(painter, at, SHOWN_R, SHOWN_HALO_R, theme::GOLD, on);
                None
            }
            // One axis read: a line across the face at it.
            #[allow(clippy::cast_possible_truncation)]
            (Some(lat), None) => {
                let x = plot.x_range().clamp(view.to_screen(lat as f32, 0.0).x);
                Some([
                    pos2(x, view.to_screen(0.0, bottom + CROSS_INSET_Y).y),
                    pos2(x, view.to_screen(0.0, top - CROSS_INSET_Y).y),
                ])
            }
            #[allow(clippy::cast_possible_truncation)]
            (None, Some(vert)) => {
                let y = plot.y_range().clamp(view.to_screen(0.0, vert as f32).y);
                Some([
                    pos2(view.to_screen(heel + CROSS_INSET_X, 0.0).x, y),
                    pos2(view.to_screen(toe - CROSS_INSET_X, 0.0).x, y),
                ])
            }
            (None, None) => None,
        };
        if let Some(line) = line {
            painter.line_segment(line, Stroke::new(4.5, theme::BG_DEEP));
            painter.line_segment(line, Stroke::new(2.0, theme::GOLD));
        }
    }
}

/// Paint the face panel in `rect` and handle the pointer: hovering a dot
/// shows its shot, clicking it returns the shot's index.
pub(crate) fn paint_face_panel(ui: &egui::Ui, rect: Rect, panel: &FacePanel) -> Option<usize> {
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let painter = ui.painter();
    painter.rect(
        rect,
        egui::CornerRadius::same(theme::CARD_RADIUS),
        theme::SURFACE,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );

    // Readout.
    let colour = if panel.shown.is_some() {
        theme::TEXT
    } else {
        theme::TEXT_DIM
    };
    let text = readout_galley(painter, &panel.readout, rect.width() - 2.0 * PAD, colour);
    painter.galley(
        pos2(rect.center().x - text.size().x / 2.0, rect.top() + 8.0),
        text,
        colour,
    );

    let plot = plot_rect(rect);
    let view = view(rect, panel);
    paint_face(painter, rect, view);
    paint_marks(painter, view, plot, panel);

    let response = ui.interact(rect, ui.id().with("face_panel"), egui::Sense::click());
    let hovered = response.hover_pos().and_then(|p| hit(view, plot, panel, p));
    let (index, text) = hovered?;
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text_at_pointer(text);
    response.clicked().then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Distance;

    /// Lie angle (degrees from the ground) of each head family's shaft line.
    fn lie(family: FaceFamily) -> Option<f32> {
        match family {
            FaceFamily::Driver => Some(58.0),
            FaceFamily::Wood => Some(57.0),
            FaceFamily::Hybrid => Some(59.0),
            FaceFamily::Iron => Some(62.5),
            FaceFamily::Wedge => Some(64.0),
            FaceFamily::Putter => None,
        }
    }

    fn segments(family: FaceFamily) -> Vec<[[f32; 2]; 4]> {
        family
            .control_points()
            .windows(4)
            .step_by(3)
            .map(|w| [w[0], w[1], w[2], w[3]])
            .collect()
    }

    /// A straight run: handles at the thirds of its chord.
    fn is_line(s: &[[f32; 2]; 4]) -> bool {
        let third = |t: f32, i: usize| s[0][i] + (s[3][i] - s[0][i]) * t;
        let near = |p: [f32; 2], t: f32| (p[0] - third(t, 0)).hypot(p[1] - third(t, 1)) < 0.01;
        near(s[1], 1.0 / 3.0) && near(s[2], 2.0 / 3.0)
    }

    /// Heading in degrees of the curve leaving `s` (end) or entering it.
    fn heading(s: &[[f32; 2]; 4], end: bool) -> f32 {
        let pairs = if end {
            [(s[3], s[2]), (s[3], s[1]), (s[3], s[0])]
        } else {
            [(s[1], s[0]), (s[2], s[0]), (s[3], s[0])]
        };
        let (q, p) = pairs
            .into_iter()
            .find(|(q, p)| (q[0] - p[0]).hypot(q[1] - p[1]) > 1e-6)
            .expect("a handle");
        (q[1] - p[1]).atan2(q[0] - p[0]).to_degrees()
    }

    /// Whether the segments `ab` and `cd` cross properly.
    fn intersect(ab: [[f32; 2]; 2], cd: [[f32; 2]; 2]) -> bool {
        let ([a, b], [c, d]) = (ab, cd);
        cross(a, b, c) * cross(a, b, d) < 0.0 && cross(c, d, a) * cross(c, d, b) < 0.0
    }

    #[test]
    fn every_club_has_a_face() {
        use Club::*;
        for &club in Club::ALL {
            let _ = FaceFamily::of(Some(club));
        }
        assert_eq!(FaceFamily::of(None), FaceFamily::Iron);
        for (club, family) in [
            (Driver, FaceFamily::Driver),
            (MiniDriver, FaceFamily::Driver),
            (Wood2, FaceFamily::Wood),
            (Wood9, FaceFamily::Wood),
            (Hybrid1, FaceFamily::Hybrid),
            (Hybrid9, FaceFamily::Hybrid),
            (Iron1, FaceFamily::Iron),
            (Iron9, FaceFamily::Iron),
            (PitchingWedge, FaceFamily::Iron),
            (GapWedge, FaceFamily::Wedge),
            (SandWedge, FaceFamily::Wedge),
            (LobWedge, FaceFamily::Wedge),
            (Putter, FaceFamily::Putter),
        ] {
            assert_eq!(FaceFamily::of(Some(club)), family, "{club}");
        }
    }

    #[test]
    fn outlines_are_closed_smooth_and_centred() {
        for family in FaceFamily::ALL {
            let p = family.control_points();
            assert_eq!(p.len() % 3, 1, "{family:?}");
            assert_eq!(p.first(), p.last(), "{family:?}: closed");
            // The face centre is the midpoint of both chords through it.
            let g = family.geometry();
            assert!((g.across.0 + g.across.1).abs() / 2.0 < 0.05, "{family:?}");
            assert!((g.up.0 + g.up.1).abs() / 2.0 < 0.05, "{family:?}");
            // Tangent at every join but the hosel cut's two corners (90°).
            let segs = segments(family);
            let corners: Vec<f32> = (0..segs.len())
                .map(|i| {
                    let turn =
                        heading(&segs[(i + 1) % segs.len()], false) - heading(&segs[i], true);
                    (turn + 540.0).rem_euclid(360.0) - 180.0
                })
                .filter(|t| t.abs() > 0.5)
                .collect();
            assert_eq!(corners.len(), 2, "{family:?}: {corners:?}");
            assert!(
                corners.iter().all(|t| (t.abs() - 90.0).abs() < 1.0),
                "{family:?}"
            );
        }
    }

    #[test]
    fn heads_have_a_straight_heel_run_on_the_shaft_line() {
        for family in FaceFamily::ALL {
            let Some(lie) = lie(family) else { continue };
            // Hosel toe edge, cut, heel run: the last three segments.
            let segs = segments(family);
            let run = segs[segs.len() - 1];
            assert!(is_line(&run), "{family:?}");
            let angle = (run[3][1] - run[0][1])
                .atan2(run[3][0] - run[0][0])
                .to_degrees();
            assert!((angle.abs() - lie).abs() < 0.05, "{family:?}: {angle}");
            assert!(angle < 0.0, "{family:?}: runs down from the cut");
            assert!((run[3][0] - run[0][0]).hypot(run[3][1] - run[0][1]) >= 40.0);
            // Nothing lies heel-ward of the run's line: the face is on its
            // left, walking counter-clockwise.
            let length = f64::from((run[3][0] - run[0][0]).hypot(run[3][1] - run[0][1]));
            let heelward = flatten(family.control_points(), 4.0)
                .into_iter()
                .map(|q| -cross(run[0], run[3], q) / length)
                .fold(f64::NEG_INFINITY, f64::max);
            assert!(heelward < 0.01, "{family:?}: {heelward} mm");
        }
    }

    #[test]
    fn flattened_outlines_are_simple_with_one_step_per_straight_run() {
        for family in FaceFamily::ALL {
            for k in [0.8, 1.2, 2.4, 4.0] {
                let pts = flatten(family.control_points(), k);
                let n = pts.len();
                let mut crossings = 0;
                for i in 0..n {
                    for j in i + 2..n {
                        if i == 0 && j == n - 1 {
                            continue;
                        }
                        if intersect([pts[i], pts[(i + 1) % n]], [pts[j], pts[(j + 1) % n]]) {
                            crossings += 1;
                        }
                    }
                }
                assert_eq!(crossings, 0, "{family:?} at {k}");
                assert!(area2(&pts) > 0.0, "{family:?}: counter-clockwise");
            }
            for s in segments(family).iter().filter(|s| is_line(s)) {
                assert_eq!(steps(s, 4.0), 1, "{family:?}");
            }
        }
        // Finer at a larger scale, as the flatness rule says.
        let coarse = flatten(FaceFamily::Iron.control_points(), 1.0).len();
        let fine = flatten(FaceFamily::Iron.control_points(), 4.0).len();
        assert!(fine > coarse);
    }

    #[test]
    fn the_fill_triangles_cover_the_outline_exactly() {
        for family in FaceFamily::ALL {
            for k in [1.0, 1.3] {
                let o = outline(family, k);
                assert_eq!(o.indices.len() % 3, 0);
                let p = &o.points;
                let triangles: f64 = o
                    .indices
                    .chunks(3)
                    .map(|t| area2(&[p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]]))
                    .inspect(|a| assert!(*a >= 0.0, "{family:?}: a flipped triangle"))
                    .sum();
                let polygon = area2(p);
                assert!(
                    (triangles - polygon).abs() < polygon * 1e-5,
                    "{family:?}: {triangles} vs {polygon}"
                );
                // Built once per family and scale.
                assert!(Rc::ptr_eq(&o, &outline(family, k)));
            }
        }
    }

    fn plot() -> Rect {
        Rect::from_min_size(pos2(100.0, 40.0), vec2(124.0, 98.0))
    }

    #[test]
    fn the_face_centre_lands_on_the_frame_centre() {
        for family in FaceFamily::ALL {
            for handed in [Handedness::Right, Handedness::Left] {
                let view = FaceView::fit(plot(), family, handed);
                let centre = view.to_screen(0.0, 0.0);
                assert_eq!(centre, view.centre);
                assert!(plot().contains(centre), "{family:?}");
                // The midpoints of the face's two chords on screen.
                let g = family.geometry();
                let across = f32::midpoint(
                    view.to_screen(g.across.0, 0.0).x,
                    view.to_screen(g.across.1, 0.0).x,
                );
                let up =
                    f32::midpoint(view.to_screen(0.0, g.up.0).y, view.to_screen(0.0, g.up.1).y);
                assert!((across - centre.x).abs() < 0.1, "{family:?} {handed:?}");
                assert!((up - centre.y).abs() < 0.1, "{family:?} {handed:?}");
                // The whole silhouette fits the plot.
                let [x0, y0, x1, y1] = g.bbox;
                for (x, y) in [(x0, y0), (x1, y1), (x0, y1), (x1, y0)] {
                    assert!(
                        plot().expand(0.01).contains(view.to_screen(x, y)),
                        "{family:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_face_fills_the_plot_on_its_own_scale() {
        let wide = Rect::from_min_size(pos2(0.0, 0.0), vec2(220.0, 90.0));
        let tall = Rect::from_min_size(pos2(0.0, 0.0), vec2(110.0, 160.0));
        for p in [plot(), wide, tall] {
            for family in FaceFamily::ALL {
                let view = FaceView::fit(p, family, Handedness::Right);
                let [x0, y0, x1, y1] = family.geometry().bbox;
                let (w, h) = ((x1 - x0) * view.k, (y1 - y0) * view.k);
                // Within its share of the plot, touching one of the limits.
                assert!(w <= FIT_WIDTH * p.width() + 1e-3, "{family:?} {p:?}");
                assert!(h <= FIT_HEIGHT * p.height() + 1e-3, "{family:?} {p:?}");
                let touches = (w - FIT_WIDTH * p.width()).abs() < 1e-3
                    || (h - FIT_HEIGHT * p.height()).abs() < 1e-3;
                assert!(touches, "{family:?} {p:?}: {w} x {h}");
                // The drawn outline stays on the plot.
                let drawn = Rect::from_points(&view.outline_points());
                assert!(p.contains_rect(drawn), "{family:?} {p:?}: {drawn:?}");
            }
        }
        // Each family on its own: the driver, the widest head, is drawn at a
        // smaller scale than the iron in the same plot.
        let k = |family| FaceView::fit(plot(), family, Handedness::Right).k;
        assert!(k(FaceFamily::Driver) < k(FaceFamily::Iron));
    }

    #[test]
    fn a_left_hander_sees_the_right_handers_face_mirrored() {
        let p = plot();
        for family in FaceFamily::ALL {
            let rh = FaceView::fit(p, family, Handedness::Right);
            let lh = FaceView::fit(p, family, Handedness::Left);
            // Toe to the left for a right-hander, to the right mirrored.
            assert!(rh.to_screen(10.0, 0.0).x < rh.centre.x);
            assert!(lh.to_screen(10.0, 0.0).x > lh.centre.x);
            // High is up for both.
            assert!(rh.to_screen(0.0, 5.0).y < rh.centre.y);
            assert!(lh.to_screen(0.0, 5.0).y < lh.centre.y);
            let (a, b) = (rh.outline_points(), lh.outline_points());
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(&b) {
                assert!((a.x + b.x - 2.0 * p.center().x).abs() < 1e-3, "{family:?}");
                assert!((a.y - b.y).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn impact_reads_as_whole_millimetres_and_side_words() {
        assert_eq!(
            impact_words(Some(4.4), Some(-2.2)),
            ("4 toe".to_owned(), "2 low".to_owned())
        );
        assert_eq!(impact_words(Some(-0.6), Some(0.4)).0, "1 heel");
        assert_eq!(impact_words(Some(0.4), Some(12.5)).0, "0");
        assert_eq!(impact_words(Some(0.4), Some(12.5)).1, "13 high");
        assert_eq!(impact_words(None, None), ("-".to_owned(), "-".to_owned()));
        assert_eq!(readout(Some(4.4), Some(-2.2)), "4 toe \u{b7} 2 low");
        assert_eq!(readout(Some(-3.0), None), "3 heel");
        assert_eq!(readout(None, Some(-5.0)), "5 low");
        assert_eq!(readout(None, None), "-");
        assert_eq!(
            impact_hover(Some(4.0), Some(-2.0)),
            "4 mm toe \u{b7} 2 mm low"
        );
        assert_eq!(impact_hover(Some(0.2), None), "0 mm");
        // Any unit reads in millimetres, negatives kept.
        let i = FaceImpact {
            lateral: Some(Distance::Inches(-0.31)),
            vertical: Some(Distance::Millimeters(2.0)),
        };
        let (lat, vert) = impact_mm(Some(&i));
        assert!((lat.unwrap() + 7.874).abs() < 1e-9);
        assert_eq!(vert, Some(2.0));
    }

    fn row(n: u32, impact: (Option<f64>, Option<f64>)) -> ShotRow {
        ShotRow {
            actor: "square.0".into(),
            shot_id: format!("s{n}"),
            shot_number: n,
            ball: None,
            club: None,
            impact: (impact.0.is_some() || impact.1.is_some()).then(|| FaceImpact {
                lateral: impact.0.map(Distance::Millimeters),
                vertical: impact.1.map(Distance::Millimeters),
            }),
            derived: None,
            club_selected: Some(Club::Iron7),
            handed: None,
            group: Some("a".into()),
            finished: true,
        }
    }

    fn panel(shots: &[ShotRow], shown: usize, handed: Option<Handedness>) -> FacePanel {
        let members: Vec<usize> = (0..shots.len()).collect();
        FacePanel::new(
            shots,
            &members,
            shown,
            None,
            handed,
            |_| theme::ACCENT,
            |_| None,
        )
    }

    #[test]
    fn the_panel_marks_the_shown_shot_and_the_members() {
        let shots = vec![
            row(1, (Some(-6.0), Some(1.0))),
            row(2, (Some(9.0), Some(-4.0))),
            row(3, (Some(2.0), None)),
            row(4, (None, None)),
            row(5, (Some(4.4), Some(-2.2))),
        ];
        let p = panel(&shots, 4, None);
        assert_eq!(p.family, FaceFamily::Iron);
        assert_eq!(p.handed, Handedness::Right);
        assert_eq!(p.readout, "4 toe \u{b7} 2 low");
        assert_eq!(p.shown, Some((4, Some(4.4), Some(-2.2))));
        assert_eq!(p.shown_hover, "#5 \u{b7} 7I\n4 mm toe \u{b7} 2 mm low");
        // Lateral-only and empty members are left out of the dots and mean.
        assert_eq!(p.dots.iter().map(|d| d.index).collect::<Vec<_>>(), [0, 1]);
        let (a, b) = p.mean.unwrap();
        assert!((a - (-6.0 + 9.0 + 4.4) / 3.0).abs() < 1e-9);
        assert!((b - (1.0 - 4.0 - 2.2) / 3.0).abs() < 1e-9);
        // Under three with both axes: no mean.
        assert!(panel(&shots[..2], 1, None).mean.is_none());
        // A lateral-only read is shown as one axis.
        let p = panel(&shots, 2, None);
        assert_eq!(p.shown, Some((2, Some(2.0), None)));
        assert_eq!(p.readout, "2 toe");
        // No read on the shown shot: the members still show.
        let p = panel(&shots, 3, None);
        assert_eq!(p.shown, None);
        assert_eq!(p.readout, "-");
        assert_eq!(p.dots.len(), 3);
    }

    #[test]
    fn the_panel_takes_the_shots_club_and_hand() {
        let mut shots = vec![row(1, (Some(1.0), Some(1.0)))];
        assert_eq!(
            panel(&shots, 0, Some(Handedness::Left)).handed,
            Handedness::Left
        );
        shots[0].handed = Some(Handedness::Right);
        assert_eq!(
            panel(&shots, 0, Some(Handedness::Left)).handed,
            Handedness::Right
        );
        shots[0].club_selected = Some(Club::Driver);
        assert_eq!(panel(&shots, 0, None).family, FaceFamily::Driver);
        shots[0].club_selected = None;
        let p = FacePanel::new(
            &shots,
            &[0],
            0,
            Some(Club::Putter),
            None,
            |_| theme::ACCENT,
            |_| None,
        );
        assert_eq!(p.family, FaceFamily::Putter);
        assert_eq!(panel(&shots, 0, None).family, FaceFamily::Iron);
    }

    #[test]
    fn an_off_face_mark_is_pulled_in_and_drawn_hollow() {
        let p = plot();
        let view = FaceView::fit(p, FaceFamily::Iron, Handedness::Right);
        let (at, on) = mark_at(view, p, 4.0, -2.0);
        assert!(on && p.contains(at));
        let (at, on) = mark_at(view, p, 80.0, 0.0);
        assert!(!on);
        assert!(p.contains(at), "{at:?}");
        assert!((at.x - p.left()).abs() < 1e-4);
        // Just past the toe is off the face but on the plot.
        let toe = FaceFamily::Iron.geometry().across.1;
        let (at, on) = mark_at(view, p, f64::from(toe) + 1.0, 0.0);
        assert!(!on && at.x > p.left());
    }

    #[test]
    fn a_dot_is_hit_near_its_mark() {
        let shots = vec![
            row(1, (Some(-6.0), Some(1.0))),
            row(2, (Some(4.0), Some(-2.0))),
        ];
        let panel = panel(&shots, 1, None);
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(150.0, 150.0));
        let plot = plot_rect(rect);
        let view = view(rect, &panel);
        let at = view.to_screen(-6.0, 1.0);
        assert_eq!(
            hit(view, plot, &panel, at + vec2(3.0, 0.0)).map(|h| h.0),
            Some(0)
        );
        let shown = view.to_screen(4.0, -2.0);
        assert_eq!(hit(view, plot, &panel, shown).map(|h| h.0), Some(1));
        assert_eq!(hit(view, plot, &panel, pos2(0.0, 0.0)), None);
    }

    #[test]
    fn face_dots_take_the_charts_device_colours_and_follow_the_device_filter() {
        let mut shots: Vec<ShotRow> = (1..=6)
            .map(|n| row(n, (Some(f64::from(n)), Some(-f64::from(n)))))
            .collect();
        for (n, shot) in shots.iter_mut().enumerate() {
            shot.actor = if n % 2 == 0 { "square.0" } else { "mevo.0" }.into();
        }
        let ids = chart::device_ids(&shots);
        assert_eq!(ids, ["mevo.0", "square.0"]);
        // The panel as the Cards view builds it, with a device filter.
        let build = |filter: Option<&str>, shown: usize| {
            let members: Vec<usize> = (0..shots.len())
                .filter(|&i| filter.is_none_or(|d| shots[i].actor == d))
                .collect();
            FacePanel::new(
                &shots,
                &members,
                shown,
                None,
                None,
                |s| chart::shot_colour(&ids, filter, &s.actor),
                |s| Some(s.actor.clone()),
            )
        };
        // Every device shown: each dot in its device's colour.
        let p = build(None, 4);
        assert_eq!(p.dots.len(), 5);
        for d in &p.dots {
            let colour = chart::device_colour(&ids, &shots[d.index].actor);
            assert_eq!(d.colour, colour, "#{}", d.index);
        }
        assert!(p.dots.iter().any(|d| d.colour == theme::DEVICE_COLOURS[1]));
        assert!(p.dots[0].hover.contains("square.0"));
        assert_eq!(p.shown, Some((4, Some(5.0), Some(-5.0))));
        // Filtered to the Mevo: its dots alone, in the accent.
        let p = build(Some("mevo.0"), 3);
        assert_eq!(p.dots.iter().map(|d| d.index).collect::<Vec<_>>(), [1, 5]);
        assert!(p.dots.iter().all(|d| d.colour == theme::ACCENT));
        assert_eq!(p.shown, Some((3, Some(4.0), Some(-4.0))));
        // The shown shot filtered out: no gold mark and no reading, as on
        // the bars; the mean is over the members alone.
        let p = build(Some("mevo.0"), 4);
        assert_eq!(p.shown, None);
        assert_eq!(p.readout, "-");
        assert_eq!(p.dots.len(), 3);
        let (a, b) = p.mean.expect("three members");
        assert!((a - 4.0).abs() < 1e-9 && (b + 4.0).abs() < 1e-9);
    }

    #[test]
    fn the_readout_shrinks_then_elides_to_fit() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let text = readout(Some(-30.0), Some(30.0));
            let full = readout_galley(&painter, &text, 1000.0, theme::TEXT);
            assert!(!full.elided);
            // Narrower than at full size: smaller, whole.
            let width = full.size().x - 8.0;
            let shrunk = readout_galley(&painter, &text, width, theme::TEXT);
            assert!(shrunk.size().x <= width && !shrunk.elided);
            assert_eq!(shrunk.text(), text);
            // Too narrow even at the floor: cut short with an ellipsis.
            let cut = readout_galley(&painter, &text, 40.0, theme::TEXT);
            assert!(cut.elided);
            assert!(cut.size().x <= 40.0);
        });
    }
}
