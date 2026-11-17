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
    let e = ellipse_from_points(&[[2.0, 100.0], [2.0, 110.0], [2.0, 120.0]], ELLIPSE_K_95).unwrap();
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
    let center = proj.to_screen(e.center[0], e.center[1]);
    // The polygon vertex farthest from the center is a major-axis end;
    // take the one to the right of the center.
    let far = ellipse_points(&e, ELLIPSE_SEGMENTS)
        .iter()
        .map(|p| proj.to_screen(p[0], p[1]))
        .filter(|p| p.x > center.x)
        .max_by(|a, b| a.distance(center).total_cmp(&b.distance(center)))
        .unwrap();
    // Screen y grows downward: top-right means right of and above the center.
    assert!(
        far.x > center.x && far.y < center.y,
        "{far:?} vs {center:?}"
    );
    // And the long axis is closer to vertical than horizontal here.
    assert!(center.y - far.y > far.x - center.x);
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
/// x 100, contacts at 104 and 106, rest at 110 (meters).
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
    // 10 px per meter across, 20 px per meter up, ground at y 200.
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
        golfer: None,
        actor: actor.into(),
        shot_id: String::new(),
        shot_number: 1,
        launch: None,
        flight: None,
        club: None,
        impact: None,
        derived: None,
        club_selected: None,
        handed: None,
        group: None,
        finished: true,
        video: Vec::new(),
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
fn device_colors_follow_the_sorted_ids() {
    let ids: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
    assert_eq!(device_color(&ids, "a"), theme::ACCENT);
    assert_eq!(device_color(&ids, "b"), theme::HEADING);
    assert_eq!(device_color(&ids, "c"), theme::LOG_ACTOR);
    assert_eq!(device_color(&ids, "d"), theme::TEAL);
    assert_eq!(device_color(&ids, "e"), theme::TEXT_MUTED);
    assert_eq!(device_color(&ids, "zz"), theme::TEXT_MUTED);
    for c in theme::DEVICE_COLORS {
        assert_ne!(c, theme::GOLD);
        assert_ne!(c, theme::DANGER);
    }
}

#[test]
fn shots_take_device_colors_only_with_every_device_of_several_shown() {
    let two: Vec<String> = ["a", "b"].map(String::from).to_vec();
    assert!(by_device(&two, None));
    assert_eq!(shot_color(&two, None, "a"), theme::DEVICE_COLORS[0]);
    assert_eq!(shot_color(&two, None, "b"), theme::DEVICE_COLORS[1]);
    // A device filter, or one device: the accent, whatever the device's
    // own color.
    assert!(!by_device(&two, Some("b")));
    assert_eq!(shot_color(&two, Some("b"), "b"), theme::ACCENT);
    let one = vec!["b".to_owned()];
    assert!(!by_device(&one, None));
    assert_eq!(shot_color(&one, None, "b"), theme::ACCENT);
}

#[test]
fn device_label_falls_back_to_the_id() {
    assert_eq!(device_label("mevo.0", Some("Mevo+")), "Mevo+");
    assert_eq!(
        device_label("openconnect_server", Some("")),
        "openconnect_server"
    );
    assert_eq!(device_label("square.0", None), "square.0");
    assert_eq!(device_label("autochip", None), "Auto-Chip");
    assert_eq!(device_label("autochip", Some("")), "Auto-Chip");
}

#[test]
fn ellipse_size_is_a_parameter() {
    // √χ²₂(p) = √(−2 ln(1 − p)): 2.448 holds 95 %, 2σ about 86 %.
    let k95 = (-2.0 * (1.0_f64 - 0.95).ln()).sqrt();
    assert!(approx(ELLIPSE_K_95, k95, 1e-3));
    assert!(approx(ellipse_coverage_pct(ELLIPSE_K_95), 95.0, 0.01));
    assert!(approx(ellipse_coverage_pct(ELLIPSE_K_2SIGMA), 86.47, 0.01));
    // The semi-axes scale with k; center and angle do not move.
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

fn set(id: &str, number: u32, club: Option<Club>, open: bool) -> SetInfo {
    SetInfo {
        golfer: None,
        id: id.into(),
        number,
        session: 0,
        club,
        started_at: 0,
        open,
        shots: 2,
    }
}

fn in_session(session: u32, s: SetInfo) -> SetInfo {
    SetInfo { session, ..s }
}

fn sets(list: Vec<SetInfo>) -> ShotGroups {
    ShotGroups::with_sets(ShotGrouping::BySet, list)
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
fn history() -> (Vec<ShotRow>, Vec<SetInfo>) {
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
    assert_eq!(m("c", ShotFilter::ALL), vec![6, 7]);
    assert_eq!(m("a", ShotFilter::ALL), vec![2, 3]);
    assert_eq!(m("old", ShotFilter::ALL), vec![0]);
    // The device filter applies first.
    assert_eq!(m("c", ShotFilter::device(Some("square.0"))), vec![7]);
    assert!(m("c", ShotFilter::device(Some("r10.0"))).is_empty());
    // No set: nothing.
    assert!(
        members_for(
            &shots,
            &g,
            ShotGrouping::BySet,
            None,
            Some(1),
            ShotFilter::ALL
        )
        .is_empty()
    );
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
    assert_eq!(m("c", ShotFilter::ALL), vec![2, 3, 6, 7]);
    assert_eq!(
        m("a", ShotFilter::ALL),
        vec![2, 3, 6, 7],
        "either set of the club"
    );
    assert_eq!(m("b", ShotFilter::ALL), vec![4, 5]);
    // A shot from the earlier session shows that session's club.
    assert_eq!(m("old", ShotFilter::ALL), vec![0]);
    // The device filter applies first.
    assert_eq!(m("c", ShotFilter::device(Some("mevo.0"))), vec![2, 6]);
    assert_eq!(m("b", ShotFilter::device(Some("square.0"))), vec![5]);
    assert!(
        members_for(
            &shots,
            &g,
            ShotGrouping::ByClub,
            None,
            Some(1),
            ShotFilter::ALL
        )
        .is_empty()
    );
}

/// A putting device's putt filed in a 7-iron set stays out of that
/// set's members and count; shown, its view is the putts' (By club:
/// every putt of the session).
#[test]
fn putts_stay_out_of_another_clubs_set() {
    const PT: Option<Club> = Some(Club::Putter);
    let shots = rows(&[
        ("mevo.0", Some("a"), I7),
        ("exputt.0", Some("a"), PT),
        ("mevo.0", Some("b"), DR),
        ("exputt.0", Some("b"), PT),
    ]);
    let g = sets(vec![set("a", 1, I7, false), set("b", 2, DR, true)]);
    let m = |view, set: &str, shown| {
        members_for(&shots, &g, view, g.get(set), Some(shown), ShotFilter::ALL)
    };
    assert_eq!(m(ShotGrouping::BySet, "a", 0), vec![0]);
    assert_eq!(
        m(ShotGrouping::BySet, "a", 1),
        vec![1],
        "the putt's own view"
    );
    assert_eq!(m(ShotGrouping::ByClub, "b", 3), vec![1, 3], "every putt");
    assert_eq!(m(ShotGrouping::ByClub, "b", 2), vec![2]);
    let a = g.get("a").expect("a");
    assert_eq!(set_shot_count(a, &shots, ShotFilter::ALL), 1);
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
        members_for(
            &shots,
            &g,
            ShotGrouping::ByClub,
            y,
            Some(2),
            ShotFilter::ALL
        ),
        vec![0, 2]
    );
    assert_eq!(
        members_for(&shots, &g, ShotGrouping::BySet, y, Some(2), ShotFilter::ALL),
        vec![2]
    );
}

/// Auto-chip rows are no set or club view's members and no device of the
/// device filter; in the view `none` a shown chip is the shown shot.
#[test]
fn auto_chip_rows_are_never_members_or_devices() {
    assert!(is_auto_chip(flighthook::AUTO_CHIP_ACTOR));
    assert!(!is_auto_chip("mevo.0") && !is_auto_chip(""));
    let chip = flighthook::AUTO_CHIP_ACTOR;
    let shots = rows(&[
        ("mevo.0", Some("a"), I7),
        (chip, Some("a"), I7),
        ("mevo.0", Some("a"), I7),
        (chip, Some("b"), I7),
    ]);
    let a = SetInfo {
        shots: 3,
        ..set("a", 1, Some(Club::Iron7), true)
    };
    let g = sets(vec![a.clone(), set("b", 2, Some(Club::Iron7), false)]);
    for view in [ShotGrouping::BySet, ShotGrouping::ByClub] {
        for shown in [1, 2] {
            assert_eq!(
                members_for(&shots, &g, view, g.get("a"), Some(shown), ShotFilter::ALL),
                vec![0, 2],
                "{view:?} {shown}"
            );
        }
    }
    assert_eq!(
        members_for(
            &shots,
            &g,
            ShotGrouping::None,
            g.get("a"),
            Some(1),
            ShotFilter::ALL
        ),
        vec![1]
    );
    assert_eq!(device_ids(&shots), vec!["mevo.0"]);
    assert!(device_ids(&shots[1..2]).is_empty());
    // The set counts 3 shots, one an Auto-chip: its headers say 2.
    assert_eq!(set_shot_count(&a, &shots, ShotFilter::ALL), 2);
    assert_eq!(set_shot_count(&a, &shots[..1], ShotFilter::ALL), 3);
    // Under a device filter, the set's rows from that device.
    assert_eq!(
        set_shot_count(&a, &shots, ShotFilter::device(Some("mevo.0"))),
        2
    );
    assert_eq!(
        set_shot_count(&a, &shots, ShotFilter::device(Some("square.0"))),
        0
    );
}

#[test]
fn members_in_view_none_are_the_shown_shot_alone() {
    let (shots, list) = history();
    let g = sets(list);
    let m = |set, shown, device| members_for(&shots, &g, ShotGrouping::None, set, shown, device);
    for set in [None, g.get("c")] {
        assert_eq!(m(set, Some(6), ShotFilter::ALL), vec![6]);
        assert_eq!(m(set, Some(1), ShotFilter::ALL), vec![1], "a row in no set");
        // The device filter does not apply: the shown shot stays.
        assert_eq!(
            m(set, Some(4), ShotFilter::device(Some("square.0"))),
            vec![4]
        );
        assert!(m(set, None, ShotFilter::ALL).is_empty());
        assert!(m(set, Some(shots.len()), ShotFilter::ALL).is_empty());
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
        golfer: None,
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
