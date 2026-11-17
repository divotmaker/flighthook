use super::*;

fn ball() -> Launch {
    Launch {
        launch_speed: Some(Velocity::MetersPerSecond(53.6)),
        launch_elevation: Some(17.0),
        launch_azimuth: Some(-1.2),
        backspin_rpm: Some(6500),
        sidespin_rpm: Some(-300),
        ..Launch::default()
    }
}

fn device_flight() -> FlightResult {
    FlightResult {
        carry_distance: Some(Distance::Meters(220.4)),
        max_height: Some(Distance::Meters(31.2)),
        flight_time: Some(6.4),
        ..FlightResult::new(Basis::Modeled)
    }
}

fn shot() -> ShotData {
    ShotData {
        shot_number: 7,
        launch: Some(ball()),
        from: "mevo.0".into(),
        card: Some("mevo.0".into()),
        flights: vec![Flight {
            from: "mevo.0".into(),
            flight: device_flight(),
        }],
        club: Some(ClubData {
            club_speed: Some(Velocity::MetersPerSecond(44.7)),
            club_speed_post: None,
            path: Some(1.5),
            attack_angle: Some(-2.0),
            face_angle: Some(0.5),
            dynamic_loft: Some(18.0),
            smash_factor: Some(1.2),
            swing_plane_horizontal: None,
            swing_plane_vertical: None,
            club_offset: None,
            club_height: None,
            ..ClubData::default()
        }),
        impact: Some(FaceImpact {
            lateral: Some(Distance::Millimeters(-4.0)),
            vertical: Some(Distance::Millimeters(2.5)),
        }),
        ..ShotData::default()
    }
}

fn derived() -> DerivedFlight {
    DerivedFlight {
        carry_offline: Some(Distance::Meters(-5.5)),
        descent_angle: Some(41.3),
        roll_distance: Some(Distance::Meters(25.1)),
        roll_offline: Some(Distance::Meters(-0.4)),
        total_distance: Some(Distance::Meters(245.5)),
        total_offline: Some(Distance::Meters(-5.9)),
        ..DerivedFlight::default()
    }
}

// The stored shot on the wire.
const GOLDEN_SHOT: &str = r#"{"shot_number":7,"launch":{"launch_speed":"53.6mps","launch_azimuth":-1.2,"launch_elevation":17.0,"backspin_rpm":6500,"sidespin_rpm":-300},"club":{"club_speed":"44.7mps","path":1.5,"attack_angle":-2.0,"face_angle":0.5,"dynamic_loft":18.0,"smash_factor":1.2},"impact":{"lateral":"-4mm","vertical":"2.5mm"},"from":"mevo.0","card":"mevo.0","flights":[{"from":"mevo.0","flight":{"basis":"modeled","carry_distance":"220.4m","max_height":"31.2m","flight_time":6.4}}]}"#;
const GOLDEN_SHOT_IMPERIAL: &str = r#"{"shot_number":7,"launch":{"launch_speed":"119.89998400000002mph","launch_azimuth":-1.2,"launch_elevation":17.0,"backspin_rpm":6500,"sidespin_rpm":-300},"club":{"club_speed":"99.99121800000002mph","path":1.5,"attack_angle":-2.0,"face_angle":0.5,"dynamic_loft":18.0,"smash_factor":1.2},"impact":{"lateral":"-4mm","vertical":"2.5mm"},"from":"mevo.0","card":"mevo.0","flights":[{"from":"mevo.0","flight":{"basis":"modeled","carry_distance":"241.03237095363082yd","max_height":"102.36220472440944ft","flight_time":6.4}}]}"#;
/// [`GOLDEN_SHOT`] in the shape stored before `launch` and `flight`:
/// one `ball` block with the device's carry, apex and flight time.
const STORED_BALL_SHOT: &str = r#"{"actor":"mevo.0","shot_number":7,"ball":{"launch_speed":"53.6mps","launch_azimuth":-1.2,"launch_elevation":17.0,"carry_distance":"220.4m","max_height":"31.2m","flight_time":6.4,"backspin_rpm":6500,"sidespin_rpm":-300},"club":{"club_speed":"44.7mps","path":1.5,"attack_angle":-2.0,"face_angle":0.5,"dynamic_loft":18.0,"smash_factor":1.2},"impact":{"lateral":"-4mm","vertical":"2.5mm"}}"#;

#[test]
fn unit_conversion_covers_the_new_shot_fields() {
    let mut s = shot();
    s.launch = Some(Launch {
        total_spin_rpm: Some(6507),
        spin_axis: Some(-2.6),
        ..ball()
    });
    s.flights[0].flight = FlightResult {
        carry_offline: Some(Distance::Meters(-9.144)),
        total_offline: Some(Distance::Meters(9.144)),
        descent_angle: Some(44.0),
        ..device_flight()
    };
    s.club = Some(ClubData {
        spin_loft: Some(20.0),
        low_point_distance: Some(Distance::Meters(0.0254)),
        ..ClubData::default()
    });
    s.metrics = Some(ShotMetrics {
        curve: Some(Distance::Meters(9.144)),
        ..ShotMetrics::default()
    });
    let close = |d: Option<Distance>, v: f64| (d.expect("present").value() - v).abs() < 1e-9;

    let imp = s.to_unit_system(UnitSystem::Imperial);
    let f = &imp.flights[0].flight;
    let l = imp.launch.as_ref().expect("launch");
    assert!(matches!(f.carry_offline, Some(Distance::Yards(_))));
    assert!(matches!(l.launch_speed, Some(Velocity::MilesPerHour(_))));
    assert!(close(f.carry_offline, -10.0) && close(f.total_offline, 10.0));
    assert_eq!(
        (f.descent_angle, l.total_spin_rpm, l.spin_axis, f.basis),
        (Some(44.0), Some(6507), Some(-2.6), Basis::Modeled)
    );
    let c = imp.club.as_ref().expect("club");
    assert!(matches!(c.low_point_distance, Some(Distance::Inches(_))));
    assert!(close(c.low_point_distance, 1.0));
    assert_eq!(c.spin_loft, Some(20.0));
    let curve = imp.metrics.as_ref().and_then(|m| m.curve);
    assert!(matches!(curve, Some(Distance::Yards(_))) && close(curve, 10.0));

    let met = imp.to_unit_system(UnitSystem::Metric);
    let f = &met.flights[0].flight;
    assert!(matches!(f.total_offline, Some(Distance::Meters(_))));
    assert!(matches!(f.max_height, Some(Distance::Meters(_))));
    assert!(close(f.carry_offline, -9.144));
    assert!(close(
        met.club.as_ref().expect("club").low_point_distance,
        0.0254
    ));
    assert!(close(met.metrics.as_ref().and_then(|m| m.curve), 9.144));
}

#[test]
fn has_data_needs_launch_flight_club_or_impact() {
    let mut s = shot();
    assert!(s.has_data());
    s.launch = None;
    s.club = None;
    s.impact = None;
    assert!(s.has_data(), "a flight result alone");
    s.flights.clear();
    assert!(!s.has_data());
    s.impact = shot().impact;
    assert!(s.has_data());
}

#[test]
fn the_stored_shot_on_the_wire() {
    assert_eq!(serde_json::to_string(&shot()).expect("ser"), GOLDEN_SHOT);
    let imperial = shot().to_unit_system(UnitSystem::Imperial);
    assert_eq!(
        serde_json::to_string(&imperial).expect("ser"),
        GOLDEN_SHOT_IMPERIAL
    );
}

#[test]
fn the_stored_shot_reads_back() {
    let s: ShotData = serde_json::from_str(GOLDEN_SHOT).expect("parses");
    assert_eq!(s, shot());
}

/// A shot in the shape with `actor`, `flight`, `derived`,
/// `club_selected` and `handed` reads into `card`/`from`, `flights`,
/// `path` and `turn`.
#[test]
fn the_earlier_shape_reads_into_flights_and_turn() {
    let old: ShotData = serde_json::from_str(&format!(
        r#"{{"actor":"mevo.0","shot_number":7,"launch":{L},"flight":{F},"derived":{{"total_distance":"245.5m","path":{{"flight":[[0.0,0.0,0.0,0.0]]}}}},"club_selected":"7I","handed":"lh"}}"#,
        L = r#"{"launch_speed":"53.6mps","launch_azimuth":-1.2,"launch_elevation":17.0,"backspin_rpm":6500,"sidespin_rpm":-300}"#,
        F = r#"{"basis":"modeled","carry_distance":"220.4m","max_height":"31.2m","flight_time":6.4}"#,
    ))
    .expect("old shape");
    assert_eq!(
        (old.from.as_str(), old.card.as_deref()),
        ("mevo.0", Some("mevo.0"))
    );
    assert_eq!(old.flights[0].flight, device_flight());
    assert_eq!(
        old.flights[1].flight.total_distance,
        Some(Distance::Meters(245.5))
    );
    assert!(old.path.is_some());
    assert_eq!(old.club_selected(), Some(Club::Iron7));
    assert_eq!(old.handed(), Some(Handedness::Left));
    let shown = old.shown();
    assert_eq!(shown.flight, Some(device_flight()));
    assert_eq!(
        shown.derived.and_then(|d| d.total_distance),
        Some(Distance::Meters(245.5))
    );
}

/// A shot stored with one `ball` block reads into `launch` and a modeled
/// `flight`, and is written back in the new shape.
#[test]
fn a_stored_ball_block_reads_into_launch_and_flight() {
    let old: ShotData = serde_json::from_str(STORED_BALL_SHOT).expect("old shape");
    assert_eq!(old, shot());
    assert_eq!(serde_json::to_string(&old).expect("ser"), GOLDEN_SHOT);

    // Launch values only: no flight result is made up.
    let launch_only: ShotData = serde_json::from_str(
        r#"{"shot_number":2,"ball":{"launch_speed":"53.6mps","backspin_rpm":0}}"#,
    )
    .expect("de");
    assert!(launch_only.flights.is_empty());
    assert_eq!(
        launch_only.launch.and_then(|l| l.backspin_rpm),
        Some(0),
        "a launch value of 0 stays"
    );

    // Every flight value a ball block carried.
    let full: ShotData = serde_json::from_str(
        r#"{"shot_number":3,"ball":{"carry_distance":"150m","carry_offline":"-2m","total_distance":"170m","total_offline":"-3m","roll_distance":"20m","max_height":"30m","flight_time":6.0,"descent_angle":45.0}}"#,
    )
    .expect("de");
    assert_eq!(full.launch, Some(Launch::default()));
    assert_eq!(
        full.flights.first().map(|f| f.flight.clone()),
        Some(FlightResult {
            carry_distance: Some(Distance::Meters(150.0)),
            carry_offline: Some(Distance::Meters(-2.0)),
            total_distance: Some(Distance::Meters(170.0)),
            total_offline: Some(Distance::Meters(-3.0)),
            roll_distance: Some(Distance::Meters(20.0)),
            max_height: Some(Distance::Meters(30.0)),
            flight_time: Some(6.0),
            descent_angle: Some(45.0),
            ..FlightResult::new(Basis::Modeled)
        })
    );

    // A shot with neither block is unchanged.
    let empty: ShotData = serde_json::from_str(r#"{"shot_number":4}"#).expect("de");
    assert_eq!(empty.launch, None);
    assert!(empty.flights.is_empty());
}

#[test]
fn source_is_optional_on_a_shot() {
    let old: ShotData =
        serde_json::from_str(r#"{"actor":"mevo.0","shot_number":3}"#).expect("old shot");
    assert_eq!(old.source, None);
    assert!(
        serde_json::to_value(&old)
            .expect("ser")
            .get("source")
            .is_none()
    );
    let shot = ShotData {
        source: Some(Source::Virtual),
        ..old
    };
    let json = serde_json::to_value(&shot).expect("ser");
    assert_eq!(json["source"], "virtual");
    let stored: ShotData =
        serde_json::from_str(r#"{"actor":"mevo.0","shot_number":3,"provenance":"virtual"}"#)
            .expect("stored");
    assert_eq!(stored.source, Some(Source::Virtual));
    assert_eq!(serde_json::from_value::<ShotData>(json).expect("de"), shot);
    assert_eq!(
        shot.to_unit_system(UnitSystem::Metric).source,
        Some(Source::Virtual)
    );
    assert_eq!(
        serde_json::to_value(Source::Measured).expect("ser"),
        "measured"
    );
}

#[test]
fn replaces_measured_is_omitted_when_false_and_round_trips_when_true() {
    let json = serde_json::to_value(derived()).expect("ser");
    assert!(json.get("replaces_measured").is_none(), "{json}");

    let mut d = derived();
    d.replaces_measured = true;
    let text = serde_json::to_string(&d).expect("ser");
    assert!(text.ends_with(r#","replaces_measured":true}"#), "{text}");
    let back: DerivedFlight = serde_json::from_str(&text).expect("de");
    assert_eq!(back, d);
    // Older JSON without the key parses as false.
    let old: DerivedFlight = serde_json::from_str(r#"{"descent_angle":41.3}"#).expect("de");
    assert!(!old.replaces_measured);
    // Unit conversion keeps the flag.
    assert!(d.to_imperial().replaces_measured);
    assert!(d.to_metric().replaces_measured);
}

#[test]
fn unit_conversion_covers_derived() {
    let imp = derived().to_imperial();
    assert!(matches!(imp.carry_offline, Some(Distance::Yards(_))));
    assert!(matches!(imp.roll_distance, Some(Distance::Yards(_))));
    assert!(matches!(imp.total_distance, Some(Distance::Yards(_))));
    assert!(matches!(imp.total_offline, Some(Distance::Yards(_))));
    assert_eq!(imp.descent_angle, Some(41.3));
    let want = Distance::Meters(-5.5).as_yards();
    assert!((imp.carry_offline.expect("set").value() - want).abs() < 1e-9);

    let mut with_height = derived();
    with_height.max_height = Some(Distance::Meters(26.0));
    with_height.carry_distance = Some(Distance::Yards(165.0));
    let imp = with_height.to_imperial();
    assert!(matches!(imp.max_height, Some(Distance::Feet(_))));
    let met = with_height.to_metric();
    assert!(matches!(met.max_height, Some(Distance::Meters(_))));
    assert!(matches!(met.carry_distance, Some(Distance::Meters(_))));
    let carry_m = Distance::Yards(165.0).as_meters();
    assert!((met.carry_distance.expect("set").value() - carry_m).abs() < 1e-9);
}

fn flight_path() -> FlightPath {
    FlightPath {
        flight: vec![
            [0.0, 0.0, 0.0, 0.0],
            [0.42, 30.1, 15.8, -8.3],
            [1.9, 155.4, 0.0, -6.5],
        ],
        ground: vec![[1.9, 155.4, 0.0, -6.5], [2.6, 164.1, 0.0, -7.3]],
    }
}

#[test]
fn flight_path_round_trips() {
    let json = serde_json::to_string(&flight_path()).expect("ser");
    let back: FlightPath = serde_json::from_str(&json).expect("de");
    assert_eq!(back, flight_path());
}

#[test]
fn flight_path_omits_empty_ground() {
    let p = FlightPath {
        flight: vec![[0.0, 0.0, 0.0, 0.0]],
        ground: vec![],
    };
    let json = serde_json::to_value(&p).expect("ser");
    assert!(json.get("ground").is_none());
    let back: FlightPath = serde_json::from_value(json).expect("de");
    assert_eq!(back, p);
}

#[test]
fn shot_without_path_is_unchanged_on_the_wire() {
    // A shot with no path omits the key entirely.
    let json = serde_json::to_value(derived()).expect("ser");
    assert!(json.get("path").is_none());
    assert_eq!(serde_json::to_string(&shot()).expect("ser"), GOLDEN_SHOT);
}

#[test]
fn derived_path_round_trips() {
    let mut d = derived();
    d.path = Some(flight_path());

    let json = serde_json::to_value(&d).expect("ser");
    assert_eq!(
        json["path"]["flight"][0],
        serde_json::json!([0.0, 0.0, 0.0, 0.0])
    );
    assert_eq!(json["path"]["ground"][1][3], -7.3);

    let back: DerivedFlight = serde_json::from_value(json).expect("de");
    assert_eq!(back, d);

    // On a full ShotData (the `/api/shots` shape):
    let s = ShotData {
        path: Some(flight_path()),
        ..shot()
    };
    let back: ShotData =
        serde_json::from_str(&serde_json::to_string(&s).expect("ser")).expect("de");
    assert_eq!(back.path.as_ref(), Some(&flight_path()));
}

#[test]
fn to_imperial_and_to_metric_leave_path_unchanged() {
    let mut d = derived();
    d.path = Some(flight_path());

    let imp = d.to_imperial();
    assert_eq!(imp.path, Some(flight_path()));
    let met = d.to_metric();
    assert_eq!(met.path, Some(flight_path()));

    // The rest of the struct still converts normally.
    assert!(matches!(imp.carry_offline, Some(Distance::Yards(_))));
    assert!(matches!(met.total_distance, Some(Distance::Meters(_))));
}

const GROUP_ID: &str = "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7";

fn stamped() -> ShotData {
    ShotData {
        shot_id: Some("abc".into()),
        block: Some(GROUP_ID.into()),
        turn: Some(flightrelay::GolferTurn {
            golfer: None,
            club: Some(Club::Iron7),
            handedness: Some(Handedness::Right),
            mode: Some(crate::Mode::Full),
        }),
        ..shot()
    }
}

#[test]
fn stamped_fields_round_trip() {
    let json = serde_json::to_string(&stamped()).expect("ser");
    assert!(
        json.contains(&format!(
            r#","shot_id":"abc","block":"{GROUP_ID}","turn":{{"golfer":null,"club":"7I","#
        )),
        "{json}"
    );
    let back: ShotData = serde_json::from_str(&json).expect("de");
    assert_eq!(back, stamped());
    assert_eq!(back.club_selected(), Some(Club::Iron7));

    let minimal: ShotData = serde_json::from_str(&format!(
        r#"{{"shot_number":1,"shot_id":"abc","club_selected":"7I","group":"{GROUP_ID}"}}"#
    ))
    .expect("de");
    assert_eq!(minimal.shot_id.as_deref(), Some("abc"));
    assert_eq!(minimal.club_selected(), Some(Club::Iron7));
    assert_eq!(
        minimal.block.as_deref(),
        Some(GROUP_ID),
        "read from `group`"
    );
}

#[test]
fn video_is_omitted_when_empty_and_survives_unit_conversion() {
    let plain = serde_json::to_string(&stamped()).expect("ser");
    assert!(!plain.contains("video"), "{plain}");
    let shot = ShotData {
        video: vec![ShotVideo {
            camera: "camera.0".into(),
            position: crate::CameraPosition::GolferFront,
            duration_ms: 5500,
            fps: 120.0,
            trigger_offset_ms: 4000,
            impact_offset_ms: 4000,
            complete: true,
            dropped: 0,
            pose: false,
            swing: false,
            rotation: 0,
            analyzed_with: None,
        }],
        ..stamped()
    };
    let json = serde_json::to_string(&shot).expect("ser");
    assert!(
        json.contains(r#","video":[{"camera":"camera.0","position":"golfer_front","duration_ms":5500,"fps":120.0,"trigger_offset_ms":4000,"impact_offset_ms":4000,"complete":true,"dropped":0}],"#),
        "{json}"
    );
    let back: ShotData = serde_json::from_str(&json).expect("de");
    assert_eq!(back, shot);
    for system in [UnitSystem::Imperial, UnitSystem::Metric] {
        assert_eq!(shot.to_unit_system(system).video, shot.video);
    }
    let mut tracked = shot.video[0].clone();
    tracked.pose = true;
    tracked.rotation = 90;
    tracked.analyzed_with = Some("0.0.4".into());
    let json = serde_json::to_string(&tracked).expect("ser");
    assert!(
        json.ends_with(r#""dropped":0,"pose":true,"rotation":90,"analyzed_with":"0.0.4"}"#),
        "{json}"
    );
    assert_eq!(
        serde_json::from_str::<ShotVideo>(&json).expect("de"),
        tracked
    );
}

#[test]
fn json_without_stamped_fields_parses_as_none() {
    let s: ShotData = serde_json::from_str(GOLDEN_SHOT).expect("parses");
    assert_eq!(s.shot_id, None);
    assert_eq!(s.turn, None);
    assert_eq!(s.block, None);
    assert_eq!(s, shot());
}

#[test]
fn unit_conversion_keeps_stamped_fields() {
    for system in [UnitSystem::Imperial, UnitSystem::Metric] {
        let c = stamped().to_unit_system(system);
        assert_eq!(c.shot_id.as_deref(), Some("abc"));
        assert_eq!(c.club_selected(), Some(Club::Iron7));
        assert_eq!(c.block.as_deref(), Some(GROUP_ID));
    }
}

#[test]
fn default_shot_data_is_empty() {
    let d = ShotData::default();
    assert_eq!(d.from, "");
    assert_eq!(d.shot_number, 0);
    assert!(!d.has_data());
    assert_eq!(d.shot_id, None);
    assert_eq!(d.club_selected(), None);
    assert_eq!(d.block, None);
    assert_eq!(
        serde_json::to_string(&d).expect("ser"),
        r#"{"shot_number":0}"#
    );
}

/// A stored shot with the game's stroke reads it back the same, and a
/// shot without one writes no `stroke` key.
#[test]
fn a_shot_with_a_stroke_round_trips() {
    let stroke: flightrelay::GameStroke = serde_json::from_str(
        r#"{"stroke_id":"shot-42","shot_id":"abc","golfer":"pureplay.local-3","hole":7,
                "stroke_number":2,"outcome":"out_of_bounds","penalty_strokes":1}"#,
    )
    .expect("stroke");
    let shot = ShotData {
        from: "pureplay-accessory-id".into(),
        shot_number: 3,
        launch: Some(ball()),
        shot_id: Some("abc".into()),
        stroke: Some(stroke),
        ..ShotData::default()
    };
    let json = serde_json::to_value(&shot).expect("serialize");
    assert_eq!(json["stroke"]["outcome"], "out_of_bounds");
    let back: ShotData = serde_json::from_value(json).expect("parse");
    assert_eq!(back, shot);
    let plain = ShotData {
        stroke: None,
        ..shot.clone()
    };
    let json = serde_json::to_value(&plain).expect("serialize");
    assert!(json.get("stroke").is_none(), "{json}");
    let yards = shot.to_unit_system(UnitSystem::Imperial);
    assert_eq!(yards.stroke, shot.stroke);
}

/// A stroke's distances follow the unit system like the shot's own.
#[test]
fn a_strokes_distances_follow_the_unit_system() {
    let m = Distance::Meters;
    let stroke = flightrelay::GameStroke {
        stroke_id: "s".into(),
        shot_id: None,
        replaces: None,
        golfer: "pureplay.local-3".parse().expect("golfer"),
        hole: 7,
        stroke_number: 2,
        flown: Some(Launch {
            launch_speed: Some(Velocity::MetersPerSecond(60.0)),
            ..Launch::default()
        }),
        flight: Some(FlightResult {
            carry_distance: Some(m(100.0)),
            max_height: Some(m(30.0)),
            ..FlightResult::new(flightrelay::Basis::Modeled)
        }),
        outcome: flightrelay::StrokeOutcome::Green,
        putt: None,
        penalty_strokes: 0,
        lie: None,
        start_position: Some(flightrelay::Position::new(m(0.0), m(0.0), m(0.0))),
        end_position: Some(flightrelay::Position::new(m(50.0), m(0.0), m(0.0))),
        start_distance_to_hole: Some(m(152.0)),
        end_distance_to_hole: Some(m(10.0)),
    };
    let shot = ShotData {
        stroke: Some(stroke),
        ..ShotData::default()
    };
    let yards = shot
        .to_unit_system(UnitSystem::Imperial)
        .stroke
        .expect("stroke");
    assert!(matches!(
        yards.end_distance_to_hole,
        Some(Distance::Yards(y)) if (y - 10.936).abs() < 1e-3
    ));
    assert!(matches!(
        yards.flight.as_ref().and_then(|f| f.carry_distance),
        Some(Distance::Yards(y)) if (y - 109.36).abs() < 1e-2
    ));
    assert!(matches!(
        yards.flight.as_ref().and_then(|f| f.max_height),
        Some(Distance::Feet(_))
    ));
    assert!(matches!(
        yards.end_position.as_ref().map(|p| p.0[0]),
        Some(Distance::Yards(_))
    ));
    assert!(matches!(
        yards.flown.as_ref().and_then(|l| l.launch_speed),
        Some(Velocity::MilesPerHour(_))
    ));
    let meters = shot
        .to_unit_system(UnitSystem::Metric)
        .stroke
        .expect("stroke");
    assert_eq!(meters.end_distance_to_hole, Some(Distance::Meters(10.0)));
    // A shot already in yards comes back in meters, exactly the same
    // distance.
    let back = ShotData {
        stroke: Some(yards),
        ..ShotData::default()
    }
    .to_unit_system(UnitSystem::Metric)
    .stroke
    .expect("stroke");
    assert!(matches!(
        back.end_distance_to_hole,
        Some(Distance::Meters(m)) if (m - 10.0).abs() < 1e-9
    ));
}
