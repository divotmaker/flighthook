use super::*;
use crate::groups::SetInfo;
use crate::types::{Basis, Club, DerivedFlight, FlightPath, FlightResult, Launch};

fn set(id: &str, number: u32, club: Club, session: u32) -> SetInfo {
    SetInfo {
        golfer: None,
        id: id.into(),
        number,
        session,
        club: Some(club),
        started_at: 0,
        open: false,
        shots: 2,
    }
}

fn groups(list: Vec<SetInfo>) -> ShotGroups {
    ShotGroups::with_sets(ShotGrouping::BySet, list)
}

/// A path landing `carry` meters downrange and `lateral` meters right,
/// rolling straight on to `total` meters downrange (no ground segment
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
        golfer: None,
        actor: actor.into(),
        shot_id: format!("id{n}"),
        shot_number: n,
        launch: Some(Launch::default()),
        flight: None,
        club: None,
        impact: None,
        derived: Some(d),
        club_selected: Some(Club::Iron7),
        handed: None,
        group: Some(group.into()),
        finished: true,
        video: Vec::new(),
    }
}

/// A shot with a measured carry and nothing else: no lateral.
fn no_lateral(n: u32, group: &str) -> ShotRow {
    ShotRow {
        launch: Some(Launch::default()),
        flight: Some(FlightResult {
            carry_distance: Some(Distance::Meters(140.0)),
            ..FlightResult::new(Basis::Modeled)
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
    chart::members_for(shots, g, view, set, Some(shown), ShotFilter::device(device))
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
        filter: ShotFilter::device(None),
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
                        .flat_map(|o| o.polygon.iter().chain([&o.center])),
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
            filter: ShotFilter::device(device),
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
    scalars.flight = Some(FlightResult {
        carry_distance: Some(Distance::Meters(140.0)),
        ..FlightResult::new(Basis::Modeled)
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
fn a_wide_area_centers_a_plot_of_bounded_aspect() {
    // A tall column: the whole area.
    let tall = plot_rect(vec2(306.0, 700.0));
    assert_eq!(tall.left(), LEFT_GUTTER);
    assert_eq!(tall.right(), 306.0 - RIGHT_PAD);
    // A wide strip under a narrow Table: centered, 1.25 times as wide as
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
fn overhead_axes_are_equal_with_the_tee_at_the_bottom_center() {
    let (shots, g) = history();
    let m = members(&shots, &g, ShotGrouping::BySet, 7, None);
    let s = build_scene(&input(&shots, &g, &m, 7, &[]));
    // One unit across is one unit up.
    assert!((s.proj.x.px_per_unit() - s.proj.y.px_per_unit()).abs() < 1e-4);
    // Tee at the bottom center; downrange up, right to the right.
    assert_eq!(s.proj.to_screen(0.0, 0.0), s.plot.center_bottom());
    let right = dot(&s, 6).at; // 5 m right, 145 m carry, 6 m right at rest
    let left = dot(&s, 5).at; // 4 m left
    assert!(right.x > s.plot.center().x && left.x < s.plot.center().x);
    let gold = dot(&s, 7).at; // 160 m total, 2.4 m right
    assert!(gold.y < s.plot.bottom() && gold.y > s.plot.top());
    // The bright rests fit: 160 m × 1.08 → 175 m at least.
    assert!(s.proj.y.domain().1 >= 175.0);
    assert!(s.proj.x.domain().1 >= 15.0);
    // Rings every 25 m (thinned by pitch), labeled at the left edge.
    assert!(s.rings.len() >= 4);
    assert!(s.rings.iter().any(|r| r.label.is_some()));
    // Lateral ticks are symmetric about the target line.
    let xs: Vec<f32> = s.lateral_ticks.iter().map(|t| t.0).collect();
    let center = s.plot.center().x;
    for x in &xs {
        assert!(xs.iter().any(|y| (center - x - (y - center)).abs() < 1e-3));
    }
}

/// A set of 8 iron shots plus an Auto-chip has the same ellipse as the
/// 8 shots alone, whichever of them is shown; a shown chip is gold.
#[test]
fn an_auto_chip_leaves_the_ellipse_alone() {
    let g = groups(vec![set("b", 1, Club::Iron7, 0)]);
    let irons: Vec<ShotRow> = (0..8)
        .map(|n| {
            shot(
                n,
                "mevo.0",
                "b",
                140.0 + 3.0 * f64::from(n),
                f64::from(n % 3) * 2.0 - 2.0,
            )
        })
        .collect();
    let mut with_chip = irons.clone();
    with_chip.insert(4, shot(99, flighthook::AUTO_CHIP_ACTOR, "b", 2.0, 0.0));
    let ellipses = |shots: &[ShotRow], shown: usize| {
        let devices = chart::device_ids(shots);
        assert_eq!(devices, vec!["mevo.0"]);
        let m = members(shots, &g, ShotGrouping::BySet, shown, None);
        let s = build_scene(&input(shots, &g, &m, shown, &devices));
        let ovals: Vec<chart::Ellipse> = s.ovals.iter().map(|o| o.ellipse).collect();
        (ovals, s.members, gold(&s))
    };
    let (alone, n, _) = ellipses(&irons, 7);
    assert_eq!((alone.len(), n), (1, 8));
    assert_eq!(alone[0].n, 8);
    assert_eq!(ellipses(&with_chip, 8), (alone.clone(), 8, Some(8)));
    assert_eq!(ellipses(&with_chip, 4), (alone, 8, Some(4)));
    let s = {
        let m = members(&with_chip, &g, ShotGrouping::BySet, 8, None);
        build_scene(&input(&with_chip, &g, &m, 8, &[]))
    };
    assert!(
        !s.dots.iter().any(|d| d.index == 4),
        "the chip is not drawn"
    );
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
    // Every device shown, two devices: one ellipse each, in its color.
    let s = build_scene(&input(&shots, &g, &m, 6, &devices));
    let ovals: Vec<(Option<&str>, Color32, usize)> = s
        .ovals
        .iter()
        .map(|o| (o.device.as_deref(), o.color, o.ellipse.n))
        .collect();
    assert_eq!(
        ovals,
        vec![
            (Some("mevo.0"), theme::DEVICE_COLORS[0], 4),
            (Some("square.0"), theme::DEVICE_COLORS[1], 3),
        ]
    );
    assert_eq!(dot(&s, 1).color, theme::DEVICE_COLORS[1]);
    // One device's points only: one ellipse in the accent color.
    let mevo = members(&shots, &g, ShotGrouping::BySet, 6, Some("mevo.0"));
    let s = build_scene(&SceneInput {
        filter: ShotFilter::device(Some("mevo.0")),
        ..input(&shots, &g, &mevo, 6, &devices)
    });
    assert_eq!(s.ovals.len(), 1);
    assert_eq!(
        (s.ovals[0].device.as_deref(), s.ovals[0].color),
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
        .map(|p| o.center + (*p - o.center) * 0.9)
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
    let k = flight::per_meter(UnitSystem::Metric).0;
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
fn the_panel_builds_around_the_shown_shot() {
    let (shots, g) = history();
    let mut app = FlighthookApp::with_pending(crate::net::new_pending());
    app.shots = shots;
    app.shots_rev = 1;
    app.groups = g;
    app.refresh_shot_caches();
    app.shot_focus.observe(&app.shots, ShotFilter::ALL, 0.0);
    app.select_shot(3);
    assert_eq!(
        app.dispersion_shown(),
        Some(3),
        "the pick, in Cards and Table"
    );
    // The latest shot's scene: its set is bright.
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
    app.settings.default_units = UnitSystem::Metric;
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
    let list: Vec<SetInfo> = (0..20)
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
        app.settings.default_units = units;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 200.0))),
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new())
                .show(ctx, |ui| {
                    let rect = Rect::from_min_size(Pos2::ZERO, vec2(ONE_ROW_MIN_WIDTH, ROW_H));
                    assert_eq!(header_height(rect.width()), ROW_H);
                    app.dispersion_header(ui, rect);
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
