use super::*;
use crate::test_ball::Ball;
use flighthook::{Club, Velocity};

// What a device's shot made, with the simulation in `derived`: the
// harness of the tests below.

/// A device's launch with the simulated values, or its own flight
/// result.
#[derive(Debug)]
enum Ev {
    Launch {
        launch: Box<Launch>,
        derived: Option<Box<DerivedFlight>>,
    },
    FlightResult {
        flight: Box<FlightResult>,
    },
}

/// Whether shots should be simulated at all: the mode is not `off`.
fn should_simulate(cfg: &SimulationSection) -> bool {
    cfg.mode != SimMode::Off
}

/// Build the bus events for a device's measured `launch` and its `flight`
/// result, taken in the current detection mode (the live game mode); see
/// [`launch_events_in_mode`].
fn launch_events(state: &SystemState, launch: Launch, flight: Option<FlightResult>) -> Vec<Ev> {
    let mode = Some(state.session.mode());
    launch_events_in_mode(state, launch, flight, mode)
}

/// Build the bus events for a device's measured `launch` and its `flight`
/// result, taken in detection mode `mode`: the `Launch` event, then the
/// `FlightResult` event when the device sent one. Reads the live
/// `[simulation]` settings, simulates the missing values into the launch's
/// `derived` (every result value under `force`; a roll for a ground ball, on
/// the green in Putting mode), and logs the `sim` line under the device's
/// own ball line. For a device that captures the mode at its trigger, `mode`
/// is that capture.
fn launch_events_in_mode(
    state: &SystemState,
    launch: Launch,
    flight: Option<FlightResult>,
    mode: Option<Mode>,
) -> Vec<Ev> {
    let cfg = state.system.simulation();
    if should_simulate(&cfg) {
        simulated_launch_events(&cfg, launch, flight, mode)
    } else {
        events(launch, None, flight)
    }
}

/// Like [`launch_events_in_mode`] under `cfg`, but simulated whatever
/// `cfg.mode` (`off` included): for a shot that is wholly flighthook's own,
/// such as an Auto-chip, whose results exist only as simulated values.
fn simulated_launch_events(
    cfg: &SimulationSection,
    launch: Launch,
    flight: Option<FlightResult>,
    mode: Option<Mode>,
) -> Vec<Ev> {
    let env_err = cfg.environment_error();
    warn_invalid_environment(cfg, env_err.as_deref());
    let derived = derive_with_error(&launch, flight.as_ref(), cfg, mode, env_err.as_deref());
    if let Some(ref d) = derived {
        info!("  {}", log_line(d));
    }
    events(launch, derived, flight)
}

/// The `Launch` event, then the `FlightResult` event when there is one.
fn events(launch: Launch, derived: Option<DerivedFlight>, flight: Option<FlightResult>) -> Vec<Ev> {
    let mut events = vec![Ev::Launch {
        launch: Box::new(launch),
        derived: derived.map(Box::new),
    }];
    if let Some(flight) = flight {
        events.push(Ev::FlightResult {
            flight: Box::new(flight),
        });
    }
    events
}

// The functions under test, taking a test shot block.

fn derive(b: &Ball, cfg: &SimulationSection) -> Option<DerivedFlight> {
    super::derive(&b.launch(), b.flight().as_ref(), cfg)
}

fn is_ground_ball(b: &Ball) -> bool {
    super::is_ground_ball(&b.launch(), b.flight().as_ref())
}

fn shot_input(b: &Ball) -> Result<ShotInput, &'static str> {
    super::shot_input(&b.launch())
}

fn measured(b: &Ball) -> Measured {
    Measured::from_shot(&b.launch(), b.flight().as_ref())
}

/// The `Launch` event of `b` (its first event).
fn ball_flight_event(state: &SystemState, b: Ball) -> Ev {
    launch_events(state, b.launch(), b.flight()).remove(0)
}

fn ball_flight_event_in_mode(state: &SystemState, b: Ball, mode: Option<Mode>) -> Ev {
    launch_events_in_mode(state, b.launch(), b.flight(), mode).remove(0)
}

fn ball(speed_mph: f64, vla: f64, hla: f64, back: i32, side: i32) -> Ball {
    Ball {
        launch_speed: Some(Velocity::MilesPerHour(speed_mph)),
        launch_elevation: Some(vla),
        launch_azimuth: Some(hla),
        carry_distance: None,
        total_distance: None,
        roll_distance: None,
        max_height: None,
        flight_time: None,
        backspin_rpm: Some(back),
        sidespin_rpm: Some(side),
        ..Ball::default()
    }
}

/// A Square/R10-like launch: launch data only.
fn square_like() -> Ball {
    ball(120.0, 17.0, -1.2, 6500, -300)
}

fn from_input(i: &ShotInput) -> Ball {
    #[allow(clippy::cast_possible_truncation)]
    ball(
        i.ball_speed_mph,
        i.launch_angle_deg,
        i.launch_azimuth_deg,
        i.backspin_rpm as i32,
        i.sidespin_rpm as i32,
    )
}

fn cfg() -> SimulationSection {
    SimulationSection::default()
}

fn m(d: Option<Distance>) -> f64 {
    d.expect("present").as_meters()
}

struct TestState {
    state: SystemState,
    writer: crate::state::SessionWriter,
    dir: std::path::PathBuf,
}

/// Test shorthands on the session writer: a `golfer.select` of one field.
trait SelectExt {
    fn set_mode(&self, mode: Mode);
    fn set_club(&self, club: Club);
}

impl SelectExt for crate::state::SessionWriter {
    fn set_mode(&self, mode: Mode) {
        let _ = self.apply(&flightrelay::GolferSelect {
            mode: Some(mode),
            ..flightrelay::GolferSelect::new("test")
        });
    }

    fn set_club(&self, club: Club) {
        let _ = self.apply(&flightrelay::GolferSelect {
            club: Some(Some(club)),
            ..flightrelay::GolferSelect::new("test")
        });
    }
}

impl Drop for TestState {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn state_with(sim: SimulationSection) -> TestState {
    let dir = std::env::temp_dir().join(format!(
        "flighthook-sim-test-{}",
        crate::state::config::generate_id()
    ));
    let (state, writer) = SystemState::new(dir.join("config.toml"));
    state.system.update(|c| c.simulation = sim);
    TestState { state, writer, dir }
}

// -- 1. Measured value wins ------------------------------------------------

#[test]
fn measured_carry_apex_and_time_are_not_derived() {
    let mut b = ball(150.0, 12.0, 0.8, 2900, -350);
    b.carry_distance = Some(Distance::Yards(241.0));
    b.max_height = Some(Distance::Feet(95.0));
    b.flight_time = Some(6.2);
    let d = derive(&b, &cfg()).expect("simulated");
    assert_eq!(d.carry_distance, None);
    assert_eq!(d.max_height, None);
    assert_eq!(d.flight_time, None);
    assert!(d.carry_offline.is_some());
    assert!(d.descent_angle.is_some());
    // Total is anchored on the measured carry.
    let total = m(d.total_distance);
    let expected = Distance::Yards(241.0).as_meters() + m(d.roll_distance);
    assert!((total - expected).abs() < 1e-9, "{total} vs {expected}");
}

#[test]
fn fully_measured_ball_derives_only_offlines_and_descent() {
    let mut b = ball(150.0, 12.0, 0.8, 2900, -350);
    b.carry_distance = Some(Distance::Yards(241.0));
    b.total_distance = Some(Distance::Yards(265.0));
    b.roll_distance = Some(Distance::Yards(24.0));
    b.max_height = Some(Distance::Feet(95.0));
    b.flight_time = Some(6.2);
    let d = derive(&b, &cfg()).expect("simulated");
    let mut expected = DerivedFlight::default();
    expected.carry_offline = d.carry_offline;
    expected.roll_offline = d.roll_offline;
    expected.total_offline = d.total_offline;
    expected.descent_angle = d.descent_angle;
    expected.path.clone_from(&d.path);
    assert_eq!(d, expected);
    assert!(d.carry_offline.is_some() && d.roll_offline.is_some());
    assert!(d.total_offline.is_some() && d.descent_angle.is_some());
    let p = d.path.as_ref().expect("path");
    assert_pinned(
        &p.flight,
        &p.ground,
        Distance::Yards(241.0).as_meters(),
        Some(Distance::Yards(265.0).as_meters()),
        0.01,
    );
}

fn forced() -> SimulationSection {
    SimulationSection {
        mode: SimMode::Force,
        ..SimulationSection::default()
    }
}

fn fully_measured() -> Ball {
    let mut b = ball(150.0, 12.0, 0.8, 2900, -350);
    b.carry_distance = Some(Distance::Yards(241.0));
    b.total_distance = Some(Distance::Yards(265.0));
    b.roll_distance = Some(Distance::Yards(24.0));
    b.max_height = Some(Distance::Feet(95.0));
    b.flight_time = Some(6.2);
    b
}

#[test]
fn force_derives_every_field_for_a_fully_measured_ball() {
    let b = fully_measured();
    let d = derive(&b, &forced()).expect("simulated");
    assert!(d.replaces_measured);
    for (name, set) in [
        ("carry", d.carry_distance.is_some()),
        ("carry_offline", d.carry_offline.is_some()),
        ("apex", d.max_height.is_some()),
        ("flight_time", d.flight_time.is_some()),
        ("descent", d.descent_angle.is_some()),
        ("roll", d.roll_distance.is_some()),
        ("roll_offline", d.roll_offline.is_some()),
        ("total", d.total_distance.is_some()),
        ("total_offline", d.total_offline.is_some()),
        ("path", d.path.is_some()),
    ] {
        assert!(set, "{name} missing");
    }
    // Exactly what the same launch gives with nothing measured: every
    // value and the (unanchored) path are the simulation's.
    let mut launch_only = b.clone();
    launch_only.carry_distance = None;
    launch_only.total_distance = None;
    launch_only.roll_distance = None;
    launch_only.max_height = None;
    launch_only.flight_time = None;
    let mut expected = derive(&launch_only, &cfg()).expect("simulated");
    expected.replaces_measured = true;
    assert_eq!(d, expected);
    let raw = raw_path(&launch_only);
    let p = d.path.as_ref().expect("path");
    assert_eq!(p.flight, round_points(&decimate_flight(&raw.flight)));
    assert_eq!(
        p.ground,
        round_points(&decimate_ground(&raw.ground, &raw.contacts))
    );
    // The simulated carry is not the measured one, so nothing was kept.
    assert!((m(d.carry_distance) - Distance::Yards(241.0).as_meters()).abs() > 0.5);
}

#[test]
fn on_keeps_measured_values_and_no_flag() {
    let b = fully_measured();
    let off = derive(&b, &cfg()).expect("simulated");
    assert!(!off.replaces_measured);
    assert_eq!(off.carry_distance, None);
    assert_eq!(off.total_distance, None);
    let json = serde_json::to_value(&off).expect("ser");
    assert!(json.get("replaces_measured").is_none(), "{json}");
}

#[test]
fn off_and_unsimulatable_shots_have_no_derived() {
    let t = state_with(SimulationSection {
        mode: SimMode::Off,
        ..cfg()
    });
    let ev = ball_flight_event(&t.state, fully_measured());
    assert!(matches!(ev, Ev::Launch { derived: None, .. }));
    // No spin reading: no `derived`, so consumers show the device values.
    let mut no_spin = fully_measured();
    no_spin.backspin_rpm = Some(0);
    no_spin.sidespin_rpm = Some(0);
    assert_eq!(derive(&no_spin, &forced()), None);
}

// -- 2. Derived is flagged, ball untouched ---------------------------------

#[test]
fn event_keeps_ball_and_flags_derived() {
    let t = state_with(cfg());
    let b = square_like();
    let ev = ball_flight_event(&t.state, b.clone());
    let Ev::Launch {
        launch: out,
        derived,
        ..
    } = &ev
    else {
        panic!("not a launch: {ev:?}");
    };
    assert_eq!(**out, b.launch());
    assert!(derived.is_some());
    let json = serde_json::json!({ "launch": out, "derived": derived });
    assert!(json["derived"]["carry_distance"].is_string(), "{json}");
    assert!(json["launch"].get("carry_distance").is_none(), "{json}");
    // A launch-only device sends no flight result.
    let events = launch_events(&t.state, b.launch(), b.flight());
    assert_eq!(events.len(), 1);
}

/// The device's flight result goes on the bus as it came, after the
/// launch, whatever was simulated.
#[test]
fn the_device_flight_result_follows_the_launch_unchanged() {
    for sim in [cfg(), forced()] {
        let t = state_with(sim);
        let b = fully_measured();
        let events = launch_events(&t.state, b.launch(), b.flight());
        let [Ev::Launch { launch, .. }, Ev::FlightResult { flight, .. }] = &events[..] else {
            panic!("not a launch and a flight result: {events:?}");
        };
        assert_eq!(**launch, b.launch());
        assert_eq!(Some(&**flight), b.flight().as_ref());
    }
}

#[test]
fn replacing_event_keeps_the_measured_ball() {
    let t = state_with(forced());
    let b = fully_measured();
    let ev = ball_flight_event(&t.state, b.clone());
    let Ev::Launch {
        launch: out,
        derived,
        ..
    } = &ev
    else {
        panic!("not a launch: {ev:?}");
    };
    assert_eq!(**out, b.launch());
    assert!(derived.as_ref().is_some_and(|d| d.replaces_measured));
    let json = serde_json::json!({ "launch": out, "derived": derived });
    assert_eq!(json["derived"]["replaces_measured"], true, "{json}");
    assert_eq!(
        json["launch"],
        serde_json::to_value(b.launch()).expect("serializes"),
        "{json}"
    );
}

#[test]
fn disabling_simulation_removes_derived_without_restart() {
    let t = state_with(cfg());
    let on = ball_flight_event(&t.state, square_like());
    assert!(matches!(
        on,
        Ev::Launch {
            derived: Some(_),
            ..
        }
    ));
    t.state.system.update(|c| c.simulation.mode = SimMode::Off);
    let off = ball_flight_event(&t.state, square_like());
    assert!(matches!(off, Ev::Launch { derived: None, .. }));
}

// -- 3. Sign mapping, pinned by observable direction -----------------------

#[test]
fn positive_sidespin_curves_right_and_keeps_running_right() {
    let d = derive(&ball(150.0, 12.5, 0.0, 3200, 600), &cfg()).expect("simulated");
    let carry_off = m(d.carry_offline);
    assert!(carry_off > 0.0, "{carry_off}");
    assert!(m(d.total_offline) > carry_off);
}

#[test]
fn negative_sidespin_mirrors_positive() {
    let r = derive(&ball(150.0, 12.5, 0.0, 3200, 600), &cfg()).expect("simulated");
    let l = derive(&ball(150.0, 12.5, 0.0, 3200, -600), &cfg()).expect("simulated");
    assert!((m(r.carry_offline) + m(l.carry_offline)).abs() < 1e-9);
    assert!((m(r.carry_distance) - m(l.carry_distance)).abs() < 1e-9);
}

#[test]
fn positive_launch_direction_lands_right_along_the_start_line() {
    let d = derive(&ball(150.0, 12.5, 3.0, 3200, 0), &cfg()).expect("simulated");
    let carry = m(d.carry_distance);
    let off = m(d.carry_offline);
    let expected = carry * 3.0_f64.to_radians().sin();
    assert!(off > 0.0, "{off}");
    assert!(
        (off - expected).abs() < 0.01 * expected,
        "{off} vs {expected}"
    );
}

// -- 4. Gates -------------------------------------------------------------

#[test]
fn zero_spin_on_both_axes_blocks_simulation() {
    // An Open Connect source with no spin measurement reports exactly
    // this; it must not be simulated as a knuckleball.
    assert_eq!(derive(&ball(150.0, 12.0, 0.0, 0, 0), &cfg()), None);
    // Either axis alone reading zero is a real measurement, not a
    // missing one.
    assert!(derive(&ball(150.0, 12.0, 0.0, 3000, 0), &cfg()).is_some());
    assert!(derive(&ball(150.0, 12.0, 0.0, 0, 300), &cfg()).is_some());
}

#[test]
fn each_missing_input_blocks_simulation() {
    let full = square_like();
    assert!(derive(&full, &cfg()).is_some());
    let mut cases = Vec::new();
    let mut b = full.clone();
    b.launch_speed = None;
    cases.push(b);
    let mut b = full.clone();
    b.launch_elevation = None;
    cases.push(b);
    let mut b = full.clone();
    b.launch_azimuth = None;
    cases.push(b);
    let mut b = full.clone();
    b.backspin_rpm = None;
    cases.push(b);
    // A Mevo E8 estimate: no sidespin.
    let mut b = full;
    b.sidespin_rpm = None;
    cases.push(b);
    for b in cases {
        assert_eq!(derive(&b, &cfg()), None, "{b:?}");
    }
}

#[test]
fn non_finite_and_degenerate_launches_are_not_simulated() {
    for (speed, vla, hla) in [
        (0.0, 17.0, 0.0),
        (-3.0, 17.0, 0.0),
        (f64::NAN, 17.0, 0.0),
        (f64::INFINITY, 17.0, 0.0),
        (f64::NEG_INFINITY, 17.0, 0.0),
        (120.0, f64::NAN, 0.0),
        (120.0, 17.0, f64::INFINITY),
        (150.0, 90.0, 0.0),
        (150.0, 180.0, 0.0),
        (150.0, 12.0, 90.0),
        (150.0, 12.0, -90.0),
        (150.0, 12.0, 180.0),
    ] {
        let b = ball(speed, vla, hla, 3000, 0);
        // Rejected by the input gate, which runs before dimple: a NaN
        // launch would otherwise integrate a full 30 s of NaN flight.
        assert!(shot_input(&b).is_err(), "{speed} {vla} {hla}");
        assert_eq!(derive(&b, &cfg()), None, "{speed} {vla} {hla}");
    }
    // Not flown, but rolled: no measured apex and no lift.
    for vla in [-5.0, 0.0] {
        let b = ball(150.0, vla, 0.0, 3000, 0);
        assert!(shot_input(&b).is_err(), "{vla}");
        let d = derive(&b, &cfg()).expect("rolled");
        assert_eq!(d.path.expect("path").flight, vec![[0.0; 4]], "{vla}");
    }
}

/// A total spin with no axis (no components) flies as that backspin
/// with no sidespin; with an axis, it is split by it.
#[test]
fn a_total_spin_alone_flies_as_backspin() {
    let mut b = ball(150.0, 12.0, 1.0, 0, 0);
    b.backspin_rpm = None;
    b.sidespin_rpm = None;
    b.total_spin_rpm = Some(3000);
    let input = shot_input(&b).expect("simulatable");
    assert_eq!((input.backspin_rpm, input.sidespin_rpm), (3000.0, 0.0));
    assert!(derive(&b, &cfg()).is_some());
    b.spin_axis = Some(10.0);
    let input = shot_input(&b).expect("simulatable");
    assert!((input.backspin_rpm - 2954.0).abs() < 1.0);
    assert!((input.sidespin_rpm - 521.0).abs() < 1.0);
}

#[test]
fn slow_low_launches_with_lift_fly() {
    for (speed, vla) in [
        (10.0, 3.0),
        (7.4, 2.0),
        (14.9, 4.9),
        (5.0, 0.6),
        (3.0, 2.0),
        (0.5, 30.0),
    ] {
        let b = ball(speed, vla, 0.0, 300, 0);
        let d = derive(&b, &cfg()).expect("simulated");
        assert!(m(d.max_height) > 0.0, "{speed} {vla}");
        assert!(m(d.carry_distance) > 0.0, "{speed} {vla}");
        assert!(d.descent_angle.is_some(), "{speed} {vla}");
        assert!(d.path.as_ref().expect("path").flight.len() >= 2);
        assert_scalars_on_the_path(&b, &d);
    }
}

#[test]
fn should_simulate_gates_on_the_sim_mode_only() {
    let off = SimulationSection {
        mode: SimMode::Off,
        ..cfg()
    };
    assert!(should_simulate(&cfg()));
    assert!(should_simulate(&forced()));
    assert!(!should_simulate(&off));
}

#[test]
fn a_ground_ball_is_a_zero_apex_or_no_lift() {
    let with_apex = |apex: f64, vla: f64| Ball {
        max_height: Some(Distance::Meters(apex)),
        ..ball(60.0, vla, 0.0, 3000, 0)
    };
    for apex in [0.0, 0.01, -0.01, 0.005] {
        // A measured apex decides, whatever the launch angle.
        assert!(is_ground_ball(&with_apex(apex, 12.0)), "{apex}");
    }
    for apex in [0.011, -0.011, 1.0, f64::NAN, f64::INFINITY] {
        assert!(!is_ground_ball(&with_apex(apex, 0.0)), "{apex}");
    }
    // No measured apex: the launch angle decides.
    for vla in [0.5, 0.3, 0.0, -5.0, -89.9] {
        assert!(is_ground_ball(&ball(60.0, vla, 0.0, 3000, 0)), "{vla}");
    }
    for vla in [0.51, 3.0, 45.0, -90.0, f64::NAN] {
        assert!(!is_ground_ball(&ball(60.0, vla, 0.0, 3000, 0)), "{vla}");
    }
    let mut no_angle = ball(60.0, 0.0, 0.0, 3000, 0);
    no_angle.launch_elevation = None;
    assert!(!is_ground_ball(&no_angle));
    // No launch angle and no spin at all: a putt measured on the ground.
    no_angle.backspin_rpm = None;
    no_angle.sidespin_rpm = None;
    assert!(is_ground_ball(&no_angle));
    no_angle.total_spin_rpm = Some(0);
    assert!(!is_ground_ball(&no_angle));
}

#[test]
fn putting_mode_flies_a_shot_with_an_apex() {
    let t = state_with(cfg());
    let mut b = straight_shot();
    b.max_height = Some(Distance::Meters(10.0));
    t.writer.set_mode(Mode::Full);
    let full = event_derived(&ball_flight_event(&t.state, b.clone()));
    t.writer.set_mode(Mode::Putting);
    t.writer.set_club(Club::Putter);
    let putting = event_derived(&ball_flight_event(&t.state, b.clone()));
    assert_eq!(putting, full);
    assert!(m(putting.carry_distance) > 50.0);
    assert!(putting.descent_angle.is_some());
    assert!(putting.path.as_ref().expect("path").flight.len() > 2);
    assert_scalars_on_the_path(&b, &putting);
}

/// The `derived` block of a `Ball` event.
fn event_derived(ev: &Ev) -> DerivedFlight {
    let Ev::Launch { derived, .. } = ev else {
        panic!("not a ball flight: {ev:?}");
    };
    *derived.clone().expect("derived")
}

#[test]
fn out_of_range_environment_is_not_simulated() {
    let b = square_like();
    for sim in [
        SimulationSection {
            altitude: Distance::Meters(5000.1),
            ..cfg()
        },
        SimulationSection {
            altitude: Distance::Meters(-1.0),
            ..cfg()
        },
        SimulationSection {
            altitude: Distance::Meters(f64::NAN),
            ..cfg()
        },
        SimulationSection {
            temperature_c: -30.1,
            ..cfg()
        },
        SimulationSection {
            temperature_c: 50.1,
            ..cfg()
        },
        SimulationSection {
            temperature_c: f64::INFINITY,
            ..cfg()
        },
    ] {
        assert_eq!(derive(&b, &sim), None, "{sim:?}");
    }
    for sim in [
        SimulationSection {
            altitude: Distance::Meters(5000.0),
            temperature_c: -30.0,
            ..cfg()
        },
        SimulationSection {
            altitude: Distance::Feet(0.0),
            temperature_c: 50.0,
            ..cfg()
        },
    ] {
        assert!(derive(&b, &sim).is_some(), "{sim:?}");
    }
}

// -- 7. Known shots vs dimple directly -------------------------------------

fn assert_matches_dimple(input: &ShotInput) -> DerivedFlight {
    let d = derive(&from_input(input), &cfg()).expect("simulated");
    let shot = simulate_shot(input, &aero(&cfg()), &Surface::FAIRWAY);
    assert_eq!(shot.ground.termination, Termination::Rest);
    let f = &shot.flight;
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(close(m(d.carry_distance), f.carry_m));
    assert!(close(m(d.carry_offline), f.lateral_m));
    assert!(close(m(d.max_height), f.apex_m));
    assert!(close(d.flight_time.expect("time"), f.flight_time));
    assert!(close(
        d.descent_angle.expect("descent"),
        f.descent_angle_deg
    ));
    assert!(close(m(d.roll_distance), shot.total_m - f.carry_m));
    assert!(close(
        m(d.roll_offline),
        shot.ground.rest.pos.z - f.lateral_m
    ));
    assert!(close(m(d.total_distance), shot.total_m));
    assert!(close(m(d.total_offline), shot.ground.rest.pos.z));
    d
}

#[test]
fn seven_iron_matches_dimple_and_is_plausible() {
    let d = assert_matches_dimple(&ShotInput::seven_iron());
    let carry = d.carry_distance.expect("carry").as_yards();
    let roll = d.roll_distance.expect("roll").as_yards();
    assert!((175.0..=200.0).contains(&carry), "{carry}");
    assert!((0.0..=15.0).contains(&roll), "{roll}");
}

#[test]
fn driver_matches_dimple_and_is_plausible() {
    let d = assert_matches_dimple(&ShotInput::driver());
    let carry = d.carry_distance.expect("carry").as_yards();
    let total = d.total_distance.expect("total").as_yards();
    assert!((255.0..=290.0).contains(&carry), "{carry}");
    assert!(total > carry, "{total} <= {carry}");
}

#[test]
fn altitude_carries_further() {
    let b = from_input(&ShotInput::seven_iron());
    let sea = derive(&b, &cfg()).expect("simulated");
    let denver = derive(
        &b,
        &SimulationSection {
            altitude: Distance::Meters(1609.0),
            ..cfg()
        },
    )
    .expect("simulated");
    assert!(m(denver.carry_distance) > m(sea.carry_distance));
}

#[test]
fn surface_setting_reaches_the_ground_model() {
    let b = from_input(&ShotInput::seven_iron());
    let fairway = derive(&b, &cfg()).expect("simulated");
    let green = derive(
        &b,
        &SimulationSection {
            surface: SimSurface::FirmGreen,
            ..cfg()
        },
    )
    .expect("simulated");
    assert!((m(fairway.carry_distance) - m(green.carry_distance)).abs() < 1e-9);
    assert!((m(fairway.roll_distance) - m(green.roll_distance)).abs() > 1e-6);
}

// -- 10. Log line -----------------------------------------------------------

#[test]
fn log_line_prints_present_fields_in_order_with_signs() {
    let mut d = DerivedFlight::default();
    d.carry_offline = Some(Distance::Yards(-6.04));
    d.descent_angle = Some(41.26);
    d.roll_distance = Some(Distance::Yards(-1.1));
    d.total_distance = Some(Distance::Yards(239.9));
    d.total_offline = Some(Distance::Yards(6.5));
    assert_eq!(
        log_line(&d),
        "sim (estimated, beta): offline=-6.0yd descent=41.3deg roll=-1.1yd \
             total=239.9yd total_offline=+6.5yd"
    );

    let mut all = DerivedFlight::default();
    all.carry_distance = Some(Distance::Meters(150.0));
    all.carry_offline = Some(Distance::Meters(0.0));
    all.max_height = Some(Distance::Meters(26.0));
    all.flight_time = Some(6.12);
    all.descent_angle = Some(50.2);
    all.roll_distance = Some(Distance::Meters(3.7));
    all.roll_offline = Some(Distance::Meters(-0.4));
    all.total_distance = Some(Distance::Meters(153.7));
    all.total_offline = Some(Distance::Meters(-0.4));
    let line = log_line(&all);
    assert_eq!(
        line,
        "sim (estimated, beta): carry=164.0yd offline=+0.0yd apex=85.3ft time=6.1s \
             descent=50.2deg roll=+4.0yd roll_offline=-0.4yd total=168.1yd total_offline=-0.4yd"
    );
    assert!(line.is_ascii());

    all.replaces_measured = true;
    assert!(log_line(&all).ends_with(" total_offline=-0.4yd replaces_measured"));
}

// -- 11. Path anchoring -----------------------------------------------------

/// The raw simulated path for `b` under the default settings.
fn raw_path(b: &Ball) -> FullPath {
    let c = cfg();
    let input = shot_input(b).expect("simulatable");
    let shot = simulate_shot(&input, &aero(&c), &surface(c.surface));
    let rest = shot.ground.termination == Termination::Rest;
    FullPath::from_shot(&shot, rest)
}

fn radius(p: [f64; 4]) -> f64 {
    p[1].hypot(p[3])
}

fn bearing(p: [f64; 4]) -> f64 {
    p[3].atan2(p[1])
}

/// The path's ends: the tee first, the landing at radius `carry` on the
/// ground, the ground segment starting exactly at the landing and the
/// rest at radius `total` on the ground.
fn assert_pinned(
    flight: &[[f64; 4]],
    ground: &[[f64; 4]],
    carry: f64,
    total: Option<f64>,
    tol: f64,
) {
    assert_eq!(flight[0], [0.0; 4]);
    let landing = *flight.last().expect("landing");
    assert!(
        (radius(landing) - carry).abs() < tol,
        "{landing:?} vs {carry}"
    );
    assert!(landing[2].abs() < tol, "{landing:?}");
    match total {
        Some(total) => {
            assert_eq!(ground[0], landing, "seam");
            let rest = *ground.last().expect("rest");
            assert!((radius(rest) - total).abs() < tol, "{rest:?} vs {total}");
            assert!(rest[2].abs() < tol, "{rest:?}");
        }
        None => assert!(ground.is_empty()),
    }
}

/// Hops in a ground segment: strict local maxima above the ground.
fn hops(ground: &[[f64; 4]]) -> usize {
    ground
        .windows(3)
        .filter(|w| w[1][2] > w[0][2] && w[1][2] >= w[2][2] && w[1][2] > 1e-6)
        .count()
}

fn sim_carry(raw: &FullPath) -> f64 {
    radius(*raw.flight.last().expect("landing"))
}

fn sim_total(raw: &FullPath) -> f64 {
    radius(*raw.ground.last().expect("rest"))
}

/// A synthetic path: a 4 s parabola peaking at 20 m and landing at
/// `carry` on a 2 deg bearing, then ground points `[dt, radius, height]`
/// on the same bearing (the first must be `[0, carry, 0]`).
fn synthetic(carry: f64, ground: &[[f64; 3]], contacts: &[usize]) -> FullPath {
    let b = 2.0_f64.to_radians();
    let at = |t: f64, rho: f64, y: f64| [t, rho * b.cos(), y, rho * b.sin()];
    let flight = (0..=100)
        .map(|i| {
            let u = f64::from(i) / 100.0;
            at(4.0 * u, carry * u, 80.0 * u * (1.0 - u))
        })
        .collect();
    let ground = ground.iter().map(|g| at(4.0 + g[0], g[1], g[2])).collect();
    FullPath {
        flight,
        ground,
        contacts: contacts.to_vec(),
    }
}

fn straight_shot() -> Ball {
    ball(150.0, 12.5, 3.0, 3200, 0)
}

#[test]
fn nothing_measured_path_is_the_simulation() {
    for input in [
        ShotInput::driver(),
        ShotInput::seven_iron(),
        ShotInput::wedge(),
    ] {
        let b = from_input(&input);
        let raw = raw_path(&b);
        assert_eq!(anchor_path(&raw, &Measured::default()), Ok(raw.clone()));
        let d = derive(&b, &cfg()).expect("simulated");
        let landing = *raw.flight.last().expect("landing");
        let rest = *raw.ground.last().expect("rest");
        assert_eq!(m(d.carry_offline), landing[3]);
        assert_eq!(m(d.total_offline), rest[3]);
        let p = d.path.as_ref().expect("path");
        assert_pinned(
            &p.flight,
            &p.ground,
            m(d.carry_distance),
            Some(m(d.total_distance)),
            0.01,
        );
    }
}

#[test]
fn measured_carry_fits_the_landing_and_carries_the_simulated_roll() {
    for scale in [1.05, 0.9] {
        let mut b = straight_shot();
        let raw = raw_path(&b);
        let carry = sim_carry(&raw) * scale;
        b.carry_distance = Some(Distance::Meters(carry));
        let fit = anchor_path(&raw, &measured(&b)).expect("path");
        let d = derive(&b, &cfg()).expect("simulated");
        let total = m(d.total_distance);
        assert!((total - (carry + sim_total(&raw) - sim_carry(&raw))).abs() < 1e-9);
        assert_pinned(&fit.flight, &fit.ground, carry, Some(total), 1e-9);
        assert_eq!(hops(&fit.ground), hops(&raw.ground));
        assert!(hops(&raw.ground) > 0);

        // Offlines are the anchored path's.
        let landing = *fit.flight.last().expect("landing");
        let rest = *fit.ground.last().expect("rest");
        assert_eq!(m(d.carry_offline), landing[3]);
        assert_eq!(m(d.total_offline), rest[3]);
        assert_eq!(m(d.roll_offline), rest[3] - landing[3]);
        let expected = carry * 3.0_f64.to_radians().sin();
        let off = m(d.carry_offline);
        assert!(
            (off - expected).abs() < 0.01 * expected,
            "{off} vs {expected}"
        );

        let p = d.path.as_ref().expect("path");
        assert_pinned(&p.flight, &p.ground, carry, Some(total), 0.01);
        assert!((p.ground.last().expect("rest")[3] - rest[3]).abs() < 0.01);
    }
}

#[test]
fn measured_apex_and_flight_time_fit_the_flight() {
    let mut b = straight_shot();
    let raw = raw_path(&b);
    let apex_sim = raw.flight[peak_index(&raw.flight)][2];
    let time_sim = raw.flight.last().expect("landing")[0];
    let (apex, time) = (apex_sim * 1.1, time_sim + 0.4);
    b.max_height = Some(Distance::Meters(apex));
    b.flight_time = Some(time);
    let fit = anchor_path(&raw, &measured(&b)).expect("path");
    let peak = fit.flight.iter().map(|p| p[2]).fold(f64::MIN, f64::max);
    assert_eq!(peak, apex);
    assert_eq!(fit.flight.last().expect("landing")[0], time);
    assert_eq!(fit.ground.len(), raw.ground.len());
    for (a, r) in fit.ground.iter().zip(&raw.ground) {
        assert!((a[0] - r[0] - 0.4).abs() < 1e-9, "{a:?} {r:?}");
    }
    assert_pinned(
        &fit.flight,
        &fit.ground,
        sim_carry(&raw),
        Some(sim_total(&raw)),
        1e-9,
    );

    let p = derive(&b, &cfg()).expect("simulated").path.expect("path");
    let peak = p.flight.iter().map(|p| p[2]).fold(f64::MIN, f64::max);
    assert!((peak - apex).abs() < 0.01, "{peak} vs {apex}");
    assert!((p.flight.last().expect("landing")[0] - time).abs() < 0.001);
}

#[test]
fn measured_total_and_roll_stretch_the_roll() {
    let mut b = straight_shot();
    let raw = raw_path(&b);
    let carry = sim_carry(&raw) * 1.02;
    let roll = (sim_total(&raw) - sim_carry(&raw)) * 1.5;
    b.carry_distance = Some(Distance::Meters(carry));
    b.total_distance = Some(Distance::Meters(carry + roll));
    b.roll_distance = Some(Distance::Meters(roll));
    let fit = anchor_path(&raw, &measured(&b)).expect("path");
    assert_pinned(&fit.flight, &fit.ground, carry, Some(carry + roll), 1e-9);
    assert_eq!(fit.ground.len(), raw.ground.len(), "fitted, not straight");
    assert_eq!(hops(&fit.ground), hops(&raw.ground));
    // The roll runs along each point's own bearing.
    for (a, r) in fit.ground.iter().zip(&raw.ground).skip(1) {
        assert!((bearing(*a) - bearing(*r)).abs() < 1e-12);
        assert_eq!(a[2], r[2]);
    }
}

#[test]
fn measured_total_below_carry_runs_straight() {
    let mut b = straight_shot();
    let raw = raw_path(&b);
    let carry = sim_carry(&raw);
    b.carry_distance = Some(Distance::Meters(carry));
    b.total_distance = Some(Distance::Meters(carry - 2.0));
    let fit = anchor_path(&raw, &measured(&b)).expect("path");
    assert_eq!(fit.ground.len(), 2);
    assert!(fit.contacts.is_empty());
    assert_pinned(&fit.flight, &fit.ground, carry, Some(carry - 2.0), 1e-9);
    let (landing, rest) = (fit.ground[0], fit.ground[1]);
    assert!((bearing(rest) - bearing(landing)).abs() < 1e-12);
    let duration = raw.ground.last().expect("rest")[0] - raw.ground[0][0];
    assert!((rest[0] - landing[0] - duration.max(MIN_STRAIGHT_RUN_S)).abs() < 1e-9);

    let d = derive(&b, &cfg()).expect("simulated");
    assert_eq!(m(d.total_offline), rest[3]);
    assert_eq!(m(d.roll_offline), rest[3] - landing[3]);
    let p = d.path.expect("path");
    assert_eq!(p.ground.len(), 2);
    assert_pinned(&p.flight, &p.ground, carry, Some(carry - 2.0), 0.01);
}

#[test]
fn spin_back_against_a_measured_run_out_runs_straight() {
    // A high-spin wedge that checks up while the device says it ran out.
    let mut b = ball(100.0, 36.0, 0.0, 12_000, 0);
    let raw = raw_path(&b);
    let carry = sim_carry(&raw);
    assert!(
        sim_total(&raw) < carry + MIN_FIT_ROLL_M,
        "spins back or stops"
    );
    b.carry_distance = Some(Distance::Meters(carry));
    b.roll_distance = Some(Distance::Meters(3.0));
    let p = derive(&b, &cfg()).expect("simulated").path.expect("path");
    assert_eq!(p.ground.len(), 2);
    assert_pinned(&p.flight, &p.ground, carry, Some(carry + 3.0), 0.01);
}

#[test]
fn near_zero_simulated_roll_runs_straight() {
    let raw = synthetic(
        100.0,
        &[[0.0, 100.0, 0.0], [0.2, 100.05, 0.1], [0.5, 100.1, 0.0]],
        &[0],
    );
    let fit = anchor_path(
        &raw,
        &Measured {
            roll: Some(2.0),
            ..Measured::default()
        },
    )
    .expect("path");
    assert_eq!(fit.ground.len(), 2);
    assert_pinned(&fit.flight, &fit.ground, 100.0, Some(102.0), 1e-9);
    // Without a measured roll the simulated hop is kept as is.
    let fit = anchor_path(&raw, &Measured::default()).expect("path");
    assert_eq!(fit, raw);
}

#[test]
fn roll_ratio_outside_the_range_runs_straight() {
    let raw = synthetic(
        100.0,
        &[
            [0.0, 100.0, 0.0],
            [0.5, 102.0, 0.4],
            [1.0, 104.0, 0.0],
            [2.0, 105.0, 0.0],
        ],
        &[0, 2],
    );
    for (roll, fitted) in [(25.0, false), (1.0, false), (19.9, true), (1.3, true)] {
        let fit = anchor_path(
            &raw,
            &Measured {
                total: Some(100.0 + roll),
                ..Measured::default()
            },
        )
        .expect("path");
        assert_eq!(fit.ground.len() == raw.ground.len(), fitted, "{roll}");
        assert_pinned(&fit.flight, &fit.ground, 100.0, Some(100.0 + roll), 1e-9);
    }
}

#[test]
fn negative_fitted_radius_runs_straight() {
    // Spins back 8 m, then forward 6 m; a measured carry of 5 m would
    // put the spin-back behind the tee.
    let raw = synthetic(
        100.0,
        &[[0.0, 100.0, 0.0], [0.5, 92.0, 0.0], [1.0, 98.0, 0.0]],
        &[0],
    );
    let fit = anchor_path(
        &raw,
        &Measured {
            carry: Some(5.0),
            ..Measured::default()
        },
    )
    .expect("path");
    assert_eq!(fit.ground.len(), 2);
    assert_pinned(&fit.flight, &fit.ground, 5.0, Some(3.0), 1e-9);
}

#[test]
fn no_rest_fits_the_flight_and_has_no_ground() {
    let raw = synthetic(100.0, &[], &[]);
    let fit = anchor_path(
        &raw,
        &Measured {
            carry: Some(110.0),
            total: Some(120.0),
            ..Measured::default()
        },
    )
    .expect("path");
    assert_pinned(&fit.flight, &fit.ground, 110.0, None, 1e-9);
    assert!(fit.contacts.is_empty());
    // The curve keeps its start line.
    assert!((bearing(fit.flight[50]) - 2.0_f64.to_radians()).abs() < 1e-12);
}

#[test]
fn non_positive_or_non_finite_measurements_give_no_path() {
    let raw = synthetic(100.0, &[[0.0, 100.0, 0.0], [1.0, 104.0, 0.0]], &[0]);
    let bad = [0.0, -1.0, f64::NAN, f64::INFINITY];
    for v in bad {
        for m in [
            Measured {
                carry: Some(v),
                ..Measured::default()
            },
            Measured {
                apex: Some(v),
                ..Measured::default()
            },
            Measured {
                flight_time: Some(v),
                ..Measured::default()
            },
            Measured {
                total: Some(v),
                ..Measured::default()
            },
        ] {
            assert!(anchor_path(&raw, &m).is_err(), "{m:?}");
        }
    }
    let nan_roll = Measured {
        roll: Some(f64::NAN),
        ..Measured::default()
    };
    assert!(anchor_path(&raw, &nan_roll).is_err());
    // A negative roll is a real measurement (spin-back).
    let back = Measured {
        roll: Some(-1.0),
        ..Measured::default()
    };
    assert!(anchor_path(&raw, &back).is_ok());

    // Through `derive`: no path, the scalars as without the measurement.
    // (A measured 0 is not a reading there: see the zero-reading and
    // ground-ball tests.)
    let plain = derive(&straight_shot(), &cfg()).expect("simulated");
    for v in [-5.0, f64::NAN, f64::INFINITY] {
        let mut b = straight_shot();
        b.carry_distance = Some(Distance::Meters(v));
        let d = derive(&b, &cfg()).expect("simulated");
        assert_eq!(d.path, None, "{v}");
        assert_eq!(d.carry_offline, plain.carry_offline);
        assert_eq!(d.total_offline, plain.total_offline);
        assert_eq!(d.roll_offline, plain.roll_offline);
    }
}

#[test]
fn a_roll_back_past_the_tee_gives_no_total() {
    // Measured carry, and a measured roll that would put the rest behind
    // the tee: no path, so no total either.
    let mut b = straight_shot();
    b.carry_distance = Some(Distance::Meters(100.0));
    b.roll_distance = Some(Distance::Meters(-101.0));
    let d = derive(&b, &cfg()).expect("simulated");
    assert_eq!(d.path, None);
    assert_eq!(d.total_distance, None);
    // A short roll back still has both.
    b.roll_distance = Some(Distance::Meters(-1.0));
    let d = derive(&b, &cfg()).expect("simulated");
    assert!(d.path.is_some());
    assert!((m(d.total_distance) - 99.0).abs() < 1e-9);
}

#[test]
fn a_path_with_a_non_finite_point_is_not_finite() {
    let raw = synthetic(100.0, &[[0.0, 100.0, 0.0], [1.0, 104.0, 0.0]], &[0]);
    assert!(raw.is_finite());
    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut bad = raw.clone();
        bad.ground[1][3] = v;
        assert!(!bad.is_finite(), "{v}");
        let mut bad = raw.clone();
        bad.flight[0][0] = v;
        assert!(!bad.is_finite(), "{v}");
    }
}

#[test]
fn a_near_zero_simulated_roll_never_divides() {
    // The roll ratio is not read below the fitting threshold, so a
    // simulated roll of exactly zero runs straight without a NaN.
    let raw = synthetic(100.0, &[[0.0, 100.0, 0.0], [1.0, 100.0, 0.0]], &[0]);
    let m = Measured {
        roll: Some(3.0),
        ..Measured::default()
    };
    let p = anchor_path(&raw, &m).expect("path");
    assert!(p.is_finite());
    assert_eq!(p.ground.len(), 2);
    assert!((p.ground[1][1].hypot(p.ground[1][3]) - 103.0).abs() < 1e-9);
}

// -- 12. Zero readings and ground balls -------------------------------------

/// The value shown for a field: the simulated one when there is one,
/// else the measured one.
fn shown(measured: Option<Distance>, derived: Option<Distance>) -> f64 {
    m(derived.or(measured))
}

/// Every scalar shown equals the path's: landing radius = carry, peak =
/// apex, landing time = flight time, rest radius = total, and the
/// offlines are the landing and rest `z` (to the path's cm/ms rounding).
fn assert_scalars_on_the_path(b: &Ball, d: &DerivedFlight) {
    let p = d.path.as_ref().expect("path");
    let landing = *p.flight.last().expect("landing");
    let rest = *p.ground.last().expect("rest");
    let carry = shown(b.carry_distance, d.carry_distance);
    let apex = shown(b.max_height, d.max_height);
    let time = d.flight_time.or(b.flight_time).expect("flight time");
    let total = shown(b.total_distance, d.total_distance);
    let peak = p.flight.iter().map(|q| q[2]).fold(0.0, f64::max);
    assert!(
        (radius(landing) - carry).abs() < 0.01,
        "{landing:?} {carry}"
    );
    assert!((peak - apex).abs() < 0.01, "{peak} {apex}");
    assert!((landing[0] - time).abs() < 0.001, "{landing:?} {time}");
    assert!((radius(rest) - total).abs() < 0.01, "{rest:?} {total}");
    assert!((m(d.carry_offline) - landing[3]).abs() < 0.01);
    assert!((m(d.total_offline) - rest[3]).abs() < 0.01);
    assert!((m(d.roll_offline) - (rest[3] - landing[3])).abs() < 0.01);
    assert_eq!(p.ground[0], landing, "seam");
}

/// A Mevo-style full shot: carry, apex and flight time measured.
fn mevo_like() -> Ball {
    let mut b = straight_shot();
    let plain = derive(&b, &cfg()).expect("simulated");
    b.carry_distance = Some(Distance::Meters(m(plain.carry_distance) * 1.03));
    b.max_height = Some(Distance::Meters(m(plain.max_height) * 0.95));
    b.flight_time = plain.flight_time.map(|t| t + 0.2);
    b
}

#[test]
fn a_zero_flight_time_with_an_apex_is_simulated() {
    let mut b = mevo_like();
    b.flight_time = Some(0.0);
    let d = derive(&b, &cfg()).expect("simulated");
    let plain = derive(&straight_shot(), &cfg()).expect("simulated");
    assert_eq!(d.flight_time, plain.flight_time);
    assert_eq!(d.max_height, None);
    assert_eq!(d.carry_distance, None);
    assert_scalars_on_the_path(&b, &d);
    // Fitted to the measured carry and apex, not to the 0.
    let p = d.path.as_ref().expect("path");
    let landing = *p.flight.last().expect("landing");
    let peak = p.flight.iter().map(|q| q[2]).fold(0.0, f64::max);
    assert!((radius(landing) - m(b.carry_distance)).abs() < 0.01);
    assert!((peak - m(b.max_height)).abs() < 0.01);
}

#[test]
fn a_zero_carry_with_an_apex_is_simulated() {
    let mut b = mevo_like();
    b.carry_distance = Some(Distance::Meters(0.0));
    let d = derive(&b, &cfg()).expect("simulated");
    let plain = derive(&straight_shot(), &cfg()).expect("simulated");
    assert_eq!(d.carry_distance, plain.carry_distance);
    assert_eq!(d.total_distance, plain.total_distance);
    assert_scalars_on_the_path(&b, &d);
    // Both zeros at once, on a launch-only shot too.
    b.flight_time = Some(0.0);
    let d = derive(&b, &cfg()).expect("simulated");
    assert_eq!(d.carry_distance, plain.carry_distance);
    assert_eq!(d.flight_time, plain.flight_time);
    assert_scalars_on_the_path(&b, &d);
    let mut launch_only = straight_shot();
    launch_only.carry_distance = Some(Distance::Meters(0.0));
    let d = derive(&launch_only, &cfg()).expect("simulated");
    assert_eq!(d, plain);
}

/// A Mevo-style bladed shot: carry, apex and time read 0.
fn bladed(speed_mps: f64, hla: f64) -> Ball {
    Ball {
        launch_speed: Some(Velocity::MetersPerSecond(speed_mps)),
        launch_elevation: Some(2.0),
        launch_azimuth: Some(hla),
        carry_distance: Some(Distance::Meters(0.0)),
        total_distance: None,
        roll_distance: None,
        max_height: Some(Distance::Meters(0.0)),
        flight_time: Some(0.0),
        backspin_rpm: Some(0),
        sidespin_rpm: Some(0),
        ..Ball::default()
    }
}

/// A ground ball's path: the tee alone as the flight, and a roll from
/// the tee to the total on the launch bearing.
fn assert_rolls_from_the_tee(b: &Ball, d: &DerivedFlight) {
    let p = d.path.as_ref().expect("path");
    assert_eq!(p.flight, vec![[0.0; 4]]);
    assert_eq!(p.ground[0], [0.0; 4]);
    assert!(p.ground.iter().all(|q| q[2] == 0.0), "{:?}", p.ground);
    let rest = *p.ground.last().expect("rest");
    let hla = b.launch_azimuth.expect("hla").to_radians();
    assert!(
        (rest[3] - radius(rest) * hla.sin()).abs() < 0.01,
        "{rest:?}"
    );
    assert_eq!(d.descent_angle, None);
    assert_eq!(m(d.carry_offline), 0.0);
    assert_scalars_on_the_path(b, d);
}

#[test]
fn a_zero_apex_in_full_mode_with_a_seven_iron_rolls() {
    // A bladed 7-iron: fast, launched with some angle, apex 0.
    let mut b = mevo_like();
    b.max_height = Some(Distance::Meters(0.0));
    let t = state_with(cfg());
    t.writer.set_mode(Mode::Full);
    t.writer.set_club(Club::Iron7);
    let ev = ball_flight_event(&t.state, b.clone());
    let Ev::Launch { launch: out, .. } = &ev else {
        panic!("not a launch");
    };
    assert_eq!(**out, b.launch());
    let d = event_derived(&ev);
    // Not rule 1: nothing is flown, and the measured carry and time
    // give way to the ground ball's 0.
    assert_eq!(d.max_height, None, "the measured 0 is shown");
    assert_eq!(m(d.carry_distance), 0.0);
    assert_eq!(d.flight_time, Some(0.0));
    assert_eq!(m(d.roll_distance), m(d.total_distance));
    assert_rolls_from_the_tee(&b, &d);
    // It rolls as the same launch with every zero measured does.
    let rolled = derive(
        &Ball {
            launch_speed: b.launch_speed,
            launch_azimuth: b.launch_azimuth,
            ..bladed(1.0, 0.0)
        },
        &cfg(),
    )
    .expect("rolled");
    assert_eq!(d.total_distance, rolled.total_distance);
    assert_eq!(d.path, rolled.path);
}

#[test]
fn a_measured_zero_apex_rolls_and_keeps_its_zeros() {
    for mode in [Mode::Full, Mode::Chipping, Mode::Putting] {
        let t = state_with(cfg());
        t.writer.set_mode(mode);
        let b = bladed(3.0, 1.5);
        let ev = ball_flight_event(&t.state, b.clone());
        let Ev::Launch { launch: out, .. } = &ev else {
            panic!("not a launch");
        };
        assert_eq!(**out, b.launch());
        let d = event_derived(&ev);
        // The measured zeros are real for a ground ball: not simulated.
        assert_eq!(d.carry_distance, None);
        assert_eq!(d.max_height, None);
        assert_eq!(d.flight_time, None);
        assert_eq!(m(d.roll_distance), m(d.total_distance));
        assert!(m(d.total_offline) > 0.0, "started right, stays right");
        assert_rolls_from_the_tee(&b, &d);
    }
}

#[test]
fn a_launch_only_shot_without_lift_rolls() {
    // Launch data only, as from a Square or an R10.
    let b = ball(20.0, 0.3, -2.0, 1500, 100);
    let d = derive(&b, &cfg()).expect("rolled");
    assert_eq!(m(d.carry_distance), 0.0);
    assert_eq!(m(d.max_height), 0.0);
    assert_eq!(d.flight_time, Some(0.0));
    assert!(m(d.total_offline) < 0.0);
    assert_rolls_from_the_tee(&b, &d);
    // Spin is not read: a missing spin reading rolls the same.
    let mut no_spin = b.clone();
    no_spin.backspin_rpm = None;
    no_spin.sidespin_rpm = None;
    assert_eq!(derive(&no_spin, &cfg()), Some(d));
    // With lift, the same speed flies.
    let flies = derive(&ball(20.0, 3.0, -2.0, 1500, 100), &cfg()).expect("flown");
    assert!(m(flies.max_height) > 0.0);
}

#[test]
fn a_ground_ball_with_a_measured_total_or_roll_is_fitted_to_it() {
    let plain = derive(&bladed(3.0, 1.0), &cfg()).expect("rolled");
    let sim = m(plain.total_distance);
    for total in [sim * 1.2, sim * 0.8, sim * 6.0] {
        let mut b = bladed(3.0, 1.0);
        b.total_distance = Some(Distance::Meters(total));
        let d = derive(&b, &cfg()).expect("rolled");
        assert_eq!(d.total_distance, None);
        assert_eq!(m(d.roll_distance), total, "carry 0 + roll = total");
        assert_rolls_from_the_tee(&b, &d);
    }
    // A measured roll alone sets the total.
    let mut b = bladed(3.0, 1.0);
    b.roll_distance = Some(Distance::Meters(sim * 1.1));
    let d = derive(&b, &cfg()).expect("rolled");
    assert!((m(d.total_distance) - sim * 1.1).abs() < 1e-9);
    assert_rolls_from_the_tee(&b, &d);
    // A measured total of 0 is not positive: no path.
    b.total_distance = Some(Distance::Meters(0.0));
    assert_eq!(derive(&b, &cfg()).expect("rolled").path, None);
}

#[test]
fn a_ground_ball_with_a_measured_carry_and_roll_shows_the_total_as_roll() {
    // The device's carry 3 + roll 2 = total 5; the carry shown is 0.
    let mut b = bladed(3.0, 0.0);
    b.carry_distance = Some(Distance::Meters(3.0));
    b.roll_distance = Some(Distance::Meters(2.0));
    b.total_distance = Some(Distance::Meters(5.0));
    let d = derive(&b, &cfg()).expect("rolled");
    assert_eq!(m(d.carry_distance), 0.0);
    assert_eq!(m(d.roll_distance), 5.0);
    assert_eq!(d.total_distance, None, "the device's total stands");
    assert_rolls_from_the_tee(&b, &d);
    // A device roll that is the total already is kept.
    b.roll_distance = Some(Distance::Meters(5.0));
    let d = derive(&b, &cfg()).expect("rolled");
    assert_eq!(d.roll_distance, None);
    assert_eq!(d.total_distance, None);
}

#[test]
fn a_flown_shot_with_a_measured_carry_and_total_rolls_the_difference() {
    let mut b = straight_shot();
    let plain = derive(&b, &cfg()).expect("simulated");
    let carry = m(plain.carry_distance) * 1.02;
    let total = carry + 9.0;
    b.carry_distance = Some(Distance::Meters(carry));
    b.total_distance = Some(Distance::Meters(total));
    let d = derive(&b, &cfg()).expect("simulated");
    assert_eq!(d.carry_distance, None);
    assert_eq!(d.total_distance, None);
    assert!((m(d.roll_distance) - 9.0).abs() < 1e-9);
    assert_scalars_on_the_path(&b, &d);
}

#[test]
fn a_ground_ball_with_a_measured_carry_shows_zero_carry() {
    let mut b = bladed(8.0, 0.0);
    b.carry_distance = Some(Distance::Meters(4.0));
    b.flight_time = Some(0.3);
    b.max_height = Some(Distance::Meters(0.004));
    let d = derive(&b, &cfg()).expect("rolled");
    assert_eq!(m(d.carry_distance), 0.0);
    assert_eq!(d.flight_time, Some(0.0));
    assert_eq!(m(d.max_height), 0.0);
    assert_rolls_from_the_tee(&b, &d);
}

#[test]
fn force_rolls_a_ground_ball_the_same_way() {
    let mut b = bladed(3.0, 1.0);
    b.total_distance = Some(Distance::Meters(9.0));
    b.roll_distance = Some(Distance::Meters(9.0));
    let d = derive(&b, &forced()).expect("rolled");
    assert!(d.replaces_measured);
    assert_eq!(m(d.carry_distance), 0.0);
    assert_eq!(m(d.max_height), 0.0);
    assert_eq!(d.flight_time, Some(0.0));
    let mut expected = derive(&bladed(3.0, 1.0), &cfg()).expect("rolled");
    expected.carry_distance = Some(Distance::Meters(0.0));
    expected.max_height = Some(Distance::Meters(0.0));
    expected.flight_time = Some(0.0);
    expected.replaces_measured = true;
    assert_eq!(d, expected);
    assert_rolls_from_the_tee(&b, &d);
    // A full launch with a measured 0 apex rolls under force too.
    let mut full = mevo_like();
    full.max_height = Some(Distance::Meters(0.0));
    let d = derive(&full, &forced()).expect("rolled");
    assert!(d.replaces_measured);
    assert_rolls_from_the_tee(&full, &d);
}

/// The roll of a straight ground ball at `speed_mps` on `surface` (m).
fn roll_on(speed_mps: f64, surface: SimSurface) -> f64 {
    let c = SimulationSection { surface, ..cfg() };
    m(derive(&bladed(speed_mps, 0.0), &c)
        .expect("rolled")
        .total_distance)
}

#[test]
fn ground_balls_roll_on_the_configured_surface() {
    // The default surface is the fairway: a bladed shot is not on a green.
    assert_eq!(cfg().surface, SimSurface::Fairway);
    let fairway = (
        roll_on(3.0, SimSurface::Fairway),
        roll_on(8.0, SimSurface::Fairway),
    );
    let green = (
        roll_on(3.0, SimSurface::Green),
        roll_on(8.0, SimSurface::Green),
    );
    // GREEN reads 11.4 ft on the stimpmeter; a struck ball skids first.
    assert!((4.0..6.0).contains(&green.0), "{green:?}");
    assert!((15.0..23.0).contains(&green.1), "{green:?}");
    // The fairway is slower than the green.
    assert!((2.0..3.5).contains(&fairway.0), "{fairway:?}");
    assert!((9.0..13.0).contains(&fairway.1), "{fairway:?}");
    let firm = roll_on(3.0, SimSurface::FirmGreen);
    assert!(firm > green.0, "{firm} vs {green:?}");
}

/// The rolled total of a straight 3 m/s ground ball under `sim`, built
/// as a device that captured `mode` at its trigger while the live game
/// mode is `live` (m).
fn roll_in_mode(sim: SimulationSection, live: Option<Mode>, mode: Option<Mode>) -> f64 {
    let t = state_with(sim);
    if let Some(live) = live {
        t.writer.set_mode(live);
    }
    let b = bladed(3.0, 0.0);
    let ev = ball_flight_event_in_mode(&t.state, b.clone(), mode);
    let d = event_derived(&ev);
    assert_rolls_from_the_tee(&b, &d);
    m(d.total_distance)
}

/// The rolled total of a straight 3 m/s ground ball under `sim` with
/// the live game mode `live` (m).
fn roll_live(sim: SimulationSection, live: Option<Mode>) -> f64 {
    let t = state_with(sim);
    if let Some(live) = live {
        t.writer.set_mode(live);
    }
    let b = bladed(3.0, 0.0);
    let d = event_derived(&ball_flight_event(&t.state, b.clone()));
    assert_rolls_from_the_tee(&b, &d);
    m(d.total_distance)
}

#[test]
fn putting_mode_rolls_a_ground_ball_on_the_green() {
    let putting = roll_live(cfg(), Some(Mode::Putting));
    assert!((putting - 4.68).abs() < 0.01, "{putting}");
    assert_eq!(putting, roll_on(3.0, SimSurface::Green));
    // Any other mode, or none yet, rolls on the configured fairway.
    for live in [Some(Mode::Full), Some(Mode::Chipping), None] {
        let fairway = roll_live(cfg(), live);
        assert!((fairway - 2.84).abs() < 0.01, "{live:?} {fairway}");
        assert_eq!(fairway, roll_on(3.0, SimSurface::Fairway), "{live:?}");
    }
}

#[test]
fn the_mode_at_trigger_picks_the_surface_over_the_live_mode() {
    let green = roll_on(3.0, SimSurface::Green);
    let fairway = roll_on(3.0, SimSurface::Fairway);
    // Triggered in Putting, switched to Full before the result arrived.
    let putt = roll_in_mode(cfg(), Some(Mode::Full), Some(Mode::Putting));
    assert_eq!(putt, green);
    // Triggered in Full, switched to Putting before the result arrived.
    let full = roll_in_mode(cfg(), Some(Mode::Putting), Some(Mode::Full));
    assert_eq!(full, fairway);
}

#[test]
fn a_configured_green_is_kept_in_putting_mode() {
    for surface in [SimSurface::Green, SimSurface::FirmGreen] {
        let sim = SimulationSection { surface, ..cfg() };
        let putting = roll_live(sim.clone(), Some(Mode::Putting));
        assert_eq!(putting, roll_on(3.0, surface), "{surface:?}");
        assert_eq!(putting, roll_live(sim, Some(Mode::Full)));
    }
    let firm = roll_on(3.0, SimSurface::FirmGreen);
    assert!((firm - 4.82).abs() < 0.01, "{firm}");
}

#[test]
fn ground_balls_need_only_speed_and_direction() {
    let mut b = bladed(3.0, 0.0);
    b.launch_elevation = None;
    b.backspin_rpm = None;
    assert!(derive(&b, &cfg()).is_some());
    for bad in [
        Ball {
            launch_speed: None,
            ..bladed(3.0, 0.0)
        },
        Ball {
            launch_azimuth: None,
            ..bladed(3.0, 0.0)
        },
        bladed(0.0, 0.0),
        bladed(f64::NAN, 0.0),
        bladed(3.0, 90.0),
    ] {
        assert_eq!(derive(&bad, &cfg()), None, "{bad:?}");
    }
}

// -- 13. Decimation ------------------------------------------------------------

fn strictly_increasing(points: &[[f64; 4]]) -> bool {
    points.windows(2).all(|w| w[1][0] > w[0][0])
}

fn on_grid(v: f64, scale: f64) -> bool {
    ((v * scale).round() - v * scale).abs() < 1e-6
}

#[test]
fn decimated_paths_keep_the_ends_and_the_apex_within_the_caps() {
    for b in [
        from_input(&ShotInput::driver()),
        from_input(&ShotInput::seven_iron()),
        from_input(&ShotInput::wedge()),
        ball(40.0, 30.0, 0.0, 9000, 0),
        ball(150.0, 12.5, -2.0, 3200, -700),
        ball(150.0, 12.5, 2.0, 3200, 700),
    ] {
        let d = derive(&b, &cfg()).expect("simulated");
        let p = d.path.as_ref().expect("path");
        assert!(p.flight.len() <= MAX_FLIGHT_POINTS, "{}", p.flight.len());
        assert!(p.ground.len() <= MAX_GROUND_POINTS, "{}", p.ground.len());
        assert!(p.flight.len() + p.ground.len() <= 64);
        assert!(strictly_increasing(&p.flight) && strictly_increasing(&p.ground));
        assert_pinned(
            &p.flight,
            &p.ground,
            m(d.carry_distance),
            Some(m(d.total_distance)),
            0.01,
        );
        let peak = p.flight.iter().map(|p| p[2]).fold(f64::MIN, f64::max);
        assert!((peak - m(d.max_height)).abs() < 0.01, "{peak}");
        assert!((p.flight.last().expect("landing")[3] - m(d.carry_offline)).abs() < 0.01);
        assert!((p.ground.last().expect("rest")[3] - m(d.total_offline)).abs() < 0.01);
        for q in p.flight.iter().chain(&p.ground) {
            assert!(on_grid(q[0], 1000.0), "{q:?}");
            assert!(q[1..].iter().all(|&v| on_grid(v, 100.0)), "{q:?}");
        }
    }
}

/// A path RDP cannot reduce: a 1 m lateral zigzag every sample.
fn zigzag(n: u32, peak_at: u32) -> Vec<[f64; 4]> {
    (0..n)
        .map(|i| {
            let t = f64::from(i) * 0.01;
            let y = if i == peak_at { 30.0 } else { 10.0 };
            let z = if i % 2 == 0 { 0.0 } else { 1.0 };
            let y = if i == 0 || i == n - 1 { 0.0 } else { y };
            [t, f64::from(i) * 0.5, y, if i == 0 { 0.0 } else { z }]
        })
        .collect()
}

#[test]
fn capped_flight_is_resampled_with_the_apex_put_back() {
    let pts = zigzag(400, 137);
    let out = decimate_flight(&pts);
    assert_eq!(out.len(), MAX_FLIGHT_POINTS);
    assert_eq!(out[0], pts[0]);
    assert_eq!(out.last(), pts.last());
    assert!(out.contains(&pts[137]), "apex kept");
    assert!(strictly_increasing(&out));
}

#[test]
fn capped_ground_is_resampled_between_landing_and_rest() {
    let pts = zigzag(300, 0);
    let out = decimate_ground(&pts, &[0]);
    assert_eq!(out.len(), MAX_GROUND_POINTS);
    assert_eq!(out[0], pts[0]);
    assert_eq!(out.last(), pts.last());
    assert!(strictly_increasing(&out));
}

#[test]
fn only_hops_above_five_cm_keep_their_contact() {
    // Hops of 50, 10 and 3 cm from contacts at 0, 10 and 20, then a roll.
    let mut pts = Vec::new();
    for (start, h) in [(0, 0.5), (10, 0.1), (20, 0.03)] {
        for i in 0..10 {
            let u = f64::from(i) / 10.0;
            let n = f64::from(start + i);
            pts.push([n * 0.01, 100.0 + n * 0.1, 4.0 * h * u * (1.0 - u), 0.0]);
        }
    }
    for i in 30..60 {
        let n = f64::from(i);
        pts.push([n * 0.01, 103.0 + (n - 30.0) * 0.05, 0.0, 0.0]);
    }
    let out = decimate_ground(&pts, &[0, 10, 20]);
    assert_eq!(out[0], pts[0]);
    assert_eq!(out.last(), pts.last());
    assert!(out.contains(&pts[10]), "10 cm hop's contact kept");
    assert!(out.contains(&pts[15]), "10 cm hop's peak kept");
    assert!(!out.contains(&pts[20]), "3 cm hop dropped");
    assert!(!out.contains(&pts[25]), "3 cm hop dropped");
}

#[test]
fn rounding_is_to_ms_and_cm_with_advancing_times() {
    let out = round_points(&[
        [0.0, 0.0, 0.0, 0.0],
        [1.0001, 1.234, -0.001, 2.346],
        [1.0004, 9.0, 9.0, 9.0],
        [2.0, 3.0, 0.0, 0.0],
        [2.0004, 4.0, 0.0, 0.0],
    ]);
    assert_eq!(
        out,
        vec![
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 1.23, 0.0, 2.35],
            [2.0, 4.0, 0.0, 0.0],
        ]
    );
    assert!(out[1][2].is_sign_positive());
}

/// A measured carry offline and descent angle are not simulated over:
/// they stay out of `derived`, the path lands on the measured offline
/// at the measured carry and the roll offline is measured from that
/// landing.
#[test]
fn measured_offline_and_descent_are_not_derived() {
    for offline in [-6.0, 4.5] {
        let mut b = fully_measured();
        b.carry_offline = Some(Distance::Meters(offline));
        b.descent_angle = Some(41.0);
        let d = derive(&b, &cfg()).expect("simulated");
        assert_eq!(d.carry_offline, None, "{offline}");
        assert_eq!(d.descent_angle, None, "{offline}");
        let p = d.path.as_ref().expect("path");
        let landing = p.flight.last().expect("landing");
        assert!(
            (landing[3] - offline).abs() < 0.01,
            "{offline}: {landing:?}"
        );
        let carry = Distance::Yards(241.0).as_meters();
        assert!(
            (landing[1].hypot(landing[3]) - carry).abs() < 0.02,
            "{landing:?}"
        );
        let rest = p.ground.last().expect("rest");
        assert!((m(d.total_offline) - rest[3]).abs() < 0.01);
        assert!((m(d.roll_offline) - (m(d.total_offline) - offline)).abs() < 1e-9);
        assert_pinned(
            &p.flight,
            &p.ground,
            carry,
            Some(Distance::Yards(265.0).as_meters()),
            0.01,
        );
    }
}

/// Without a measured carry the path lands on the measured offline at
/// the simulated carry.
#[test]
fn measured_offline_lands_at_the_simulated_carry() {
    let mut b = ball(150.0, 12.0, 0.8, 2900, -350);
    let plain = derive(&b, &cfg()).expect("simulated");
    b.carry_offline = Some(Distance::Meters(-3.0));
    let d = derive(&b, &cfg()).expect("simulated");
    assert_eq!(d.carry_offline, None);
    let landing = *d
        .path
        .as_ref()
        .expect("path")
        .flight
        .last()
        .expect("landing");
    assert!((landing[3] - -3.0).abs() < 0.01, "{landing:?}");
    let carry = m(d.carry_distance);
    assert!((carry - m(plain.carry_distance)).abs() < 1e-9);
    assert!(
        (landing[1].hypot(landing[3]) - carry).abs() < 0.02,
        "{landing:?}"
    );
}

/// An offline the carry can't reach gives no path; the measured value
/// still is not overridden, and with no path to read them from there
/// are no roll or total offlines (the raw simulation's rest is not in
/// the measured landing's frame).
#[test]
fn offline_beyond_the_carry_gives_no_path() {
    let mut b = fully_measured();
    b.carry_offline = Some(Distance::Yards(300.0));
    let d = derive(&b, &cfg()).expect("simulated");
    assert_eq!(d.carry_offline, None);
    assert!(d.path.is_none());
    assert_eq!(d.roll_offline, None);
    assert_eq!(d.total_offline, None);
    assert!(d.roll_distance.is_none() && d.total_distance.is_none());
    assert!(d.descent_angle.is_some());
}

/// A non-finite measured offline or descent is not a reading: both are
/// simulated, and every offline is finite.
#[test]
fn non_finite_offline_and_descent_are_not_measured() {
    let mut b = fully_measured();
    b.carry_offline = Some(Distance::Meters(f64::NAN));
    b.descent_angle = Some(f64::INFINITY);
    let m = measured(&b);
    assert!(m.offline.is_none() && m.descent.is_none());
    let d = derive(&b, &cfg()).expect("simulated");
    let plain = derive(&fully_measured(), &cfg()).expect("simulated");
    assert_eq!(d, plain);
    for v in [d.carry_offline, d.roll_offline, d.total_offline] {
        assert!(v.expect("present").as_meters().is_finite());
    }
}

/// The raw path for `b` and its fit to `b`'s measured values.
fn fitted(b: &Ball) -> (FullPath, FullPath) {
    let raw = raw_path(b);
    let fit = anchor_path(&raw, &measured(b)).expect("path");
    (raw, fit)
}

/// The displayed total for `b` without its offline: the rest radius of
/// the path fitted to everything else.
fn total_without_offline(raw: &FullPath, b: &Ball) -> f64 {
    let m = Measured {
        offline: None,
        ..measured(b)
    };
    sim_total(&anchor_path(raw, &m).expect("path"))
}

/// Launch bearing (deg): the path's tangent at the tee, extrapolated
/// from the bearings of its first two samples (`z/x = a + b·x` for a
/// lateral `a·x + b·x²`), so a curve's own bend is not counted.
fn launch_bearing(p: &FullPath) -> f64 {
    let ([_, x1, _, z1], [_, x2, _, z2]) = (p.flight[1], p.flight[2]);
    let slope = (x2 * (z1 / x1) - x1 * (z2 / x2)) / (x2 - x1);
    slope.atan().to_degrees()
}

/// The simulated curve at the landing: the lateral off the start line.
fn sim_curve(raw: &FullPath, azimuth: f64) -> f64 {
    let l = raw.flight.last().expect("landing");
    l[3] - l[1] * azimuth.to_radians().tan()
}

/// Heading (rad) of the step from `a` to `b`.
fn step_heading(a: [f64; 4], b: [f64; 4]) -> f64 {
    (b[3] - a[3]).atan2(b[1] - a[1])
}

/// The fit's invariants: the tee on the measured start line, the
/// landing at the carry and the measured offline, the rest at the
/// displayed total, and the roll leaving the landing at the same angle
/// to the arrival heading as in the simulation.
fn assert_offline_fit(
    raw: &FullPath,
    fit: &FullPath,
    azimuth: f64,
    carry: f64,
    offline: f64,
    total: Option<f64>,
) {
    assert!(fit.is_finite());
    let start = launch_bearing(fit);
    assert!((start - azimuth).abs() < 0.01, "start {start} vs {azimuth}");
    let landing = *fit.flight.last().expect("landing");
    assert!((landing[3] - offline).abs() < 1e-9, "{landing:?}");
    assert_pinned(&fit.flight, &fit.ground, carry, total, 1e-6);
    if total.is_some() {
        let n = fit.flight.len();
        let roll_turn = |p: &FullPath| {
            step_heading(p.ground[0], p.ground[1]) - step_heading(p.flight[n - 2], p.flight[n - 1])
        };
        let (r, f) = (roll_turn(raw), roll_turn(fit));
        assert!((r - f).abs() < 0.02, "roll turn {r} vs {f}");
    }
}

/// The fit keeps the measured launch direction and lands on the
/// measured offline (including a 4° start with a hook back to 6 m
/// left).
#[test]
fn offline_fit_keeps_the_launch_direction() {
    for (azimuth, offline) in [(4.0, -6.0), (0.8, -3.0), (0.8, 12.0), (-2.5, 5.0)] {
        let mut b = fully_measured();
        b.launch_azimuth = Some(azimuth);
        b.carry_offline = Some(Distance::Meters(offline));
        let (raw, fit) = fitted(&b);
        let start = launch_bearing(&raw);
        assert!((start - azimuth).abs() < 0.01, "raw start {start}");
        assert!(sim_curve(&raw, azimuth).abs() >= MIN_FIT_CURVE_M);
        assert_offline_fit(
            &raw,
            &fit,
            azimuth,
            Distance::Yards(241.0).as_meters(),
            offline,
            Some(Distance::Yards(265.0).as_meters()),
        );
    }
}

/// Through `derive`: the decimated path's landing and rest are the
/// offlines it reports.
#[test]
fn offline_fit_offlines_are_the_paths() {
    let mut b = fully_measured();
    b.launch_azimuth = Some(4.0);
    b.carry_offline = Some(Distance::Meters(-6.0));
    let d = derive(&b, &cfg()).expect("simulated");
    let p = d.path.as_ref().expect("path");
    let landing = p.flight.last().expect("landing");
    let rest = p.ground.last().expect("rest");
    assert!((landing[3] - -6.0).abs() < 0.005, "{landing:?}");
    assert!((m(d.total_offline) - rest[3]).abs() < 0.005);
    assert!((m(d.roll_offline) - (m(d.total_offline) + 6.0)).abs() < 1e-9);
}

/// A shot with almost no simulated curve (no sidespin) gets the missing
/// lateral added, still leaving on the measured line.
#[test]
fn offline_fit_adds_the_lateral_without_a_curve() {
    let mut b = straight_shot();
    b.carry_distance = Some(Distance::Meters(200.0));
    b.carry_offline = Some(Distance::Meters(14.0));
    let (raw, fit) = fitted(&b);
    assert!(sim_curve(&raw, 3.0).abs() < MIN_FIT_CURVE_M);
    let total = total_without_offline(&raw, &b);
    assert_offline_fit(&raw, &fit, 3.0, 200.0, 14.0, Some(total));
    // The lateral added grows with the square of the distance: halfway
    // downrange a quarter of it.
    let l = *fit.flight.last().expect("landing");
    let mid = fit
        .flight
        .iter()
        .min_by(|a, b| {
            (a[1] - l[1] / 2.0)
                .abs()
                .total_cmp(&(b[1] - l[1] / 2.0).abs())
        })
        .expect("mid");
    let tan = 3.0_f64.to_radians().tan();
    let added = |p: [f64; 4]| p[3] - p[1] * tan;
    let ratio = added(*mid) / added(l);
    assert!((ratio - 0.25).abs() < 0.02, "{ratio}");
}

/// A straight shot measured straight stays on the target line.
#[test]
fn offline_fit_straight_shot_stays_straight() {
    let mut b = ball(150.0, 12.5, 0.0, 3200, 0);
    b.carry_distance = Some(Distance::Meters(200.0));
    b.carry_offline = Some(Distance::Meters(0.0));
    let (raw, fit) = fitted(&b);
    let total = total_without_offline(&raw, &b);
    assert_offline_fit(&raw, &fit, 0.0, 200.0, 0.0, Some(total));
    for p in fit.flight.iter().chain(&fit.ground) {
        assert!(p[3].abs() < 1e-9, "{p:?}");
    }
}

/// A measured curve far beyond the simulation's (scale over
/// [`MAX_CURVE_SCALE`]) is added rather than scaled; the fit holds.
#[test]
fn offline_fit_absurd_scale_adds_the_lateral() {
    let mut b = fully_measured();
    let azimuth = 0.8;
    let raw = raw_path(&b);
    let carry = Distance::Yards(241.0).as_meters();
    let k = carry / sim_carry(&raw);
    let curve = sim_curve(&raw, azimuth) * k;
    // Ten times the simulated curve, on the same side.
    let offline = carry * azimuth.to_radians().sin() + 10.0 * curve;
    let x_fit = (carry * carry - offline * offline).sqrt();
    let scale = (offline - x_fit * azimuth.to_radians().tan()) / curve;
    assert!(scale > MAX_CURVE_SCALE, "{scale}");
    b.carry_offline = Some(Distance::Meters(offline));
    let fit = anchor_path(&raw, &measured(&b)).expect("path");
    assert_offline_fit(
        &raw,
        &fit,
        azimuth,
        carry,
        offline,
        Some(Distance::Yards(265.0).as_meters()),
    );
}

/// Large launch directions (up to 75° either way) keep their start line
/// and land on the measured offline, with every point finite.
#[test]
fn offline_fit_huge_launch_directions() {
    for azimuth in [-75.0, -40.0, 40.0, 75.0_f64] {
        let mut b = ball(150.0, 12.0, azimuth, 2900, -350);
        b.carry_distance = Some(Distance::Meters(200.0));
        let offline = 200.0 * azimuth.to_radians().sin() - 4.0;
        b.carry_offline = Some(Distance::Meters(offline));
        let (raw, fit) = fitted(&b);
        let start = launch_bearing(&raw);
        assert!((start - azimuth).abs() < 0.01, "raw start {start}");
        let total = total_without_offline(&raw, &b);
        assert_offline_fit(&raw, &fit, azimuth, 200.0, offline, Some(total));
    }
}

/// Under `force` the measured offline and descent are simulated over,
/// like every other result field.
#[test]
fn force_derives_offline_and_descent_over_measured() {
    let mut b = fully_measured();
    b.carry_offline = Some(Distance::Meters(-6.0));
    b.descent_angle = Some(41.0);
    let d = derive(&b, &forced()).expect("simulated");
    assert!(d.carry_offline.is_some() && d.descent_angle.is_some());
    let plain = derive(&fully_measured(), &forced()).expect("simulated");
    assert_eq!(d, plain);
}

/// A ground ball shows a carry offline of 0, unless the device measured
/// exactly 0.
#[test]
fn ground_ball_offline_is_zero_unless_measured_zero() {
    let mut b = ball(8.0, 0.0, 1.0, 0, 0);
    b.max_height = Some(Distance::Meters(0.0));
    b.carry_offline = Some(Distance::Meters(0.4));
    let d = derive(&b, &cfg()).expect("rolled");
    assert_eq!(d.carry_offline, Some(Distance::Meters(0.0)));
    b.carry_offline = Some(Distance::Meters(0.0));
    let d = derive(&b, &cfg()).expect("rolled");
    assert_eq!(d.carry_offline, None);
}

/// A flight that already holds every value the model computes (an
/// Auto-chip's own) gets nothing from the model under `on`, and every
/// value under `force`.
#[test]
fn the_model_repeats_no_value_of_the_launch_monitors_flight() {
    let b = ball(10.4, 28.0, 0.0, 1041, 0);
    let own = model(&cfg(), &b.launch(), None, None, "own")
        .and_then(|m| m.flight)
        .expect("flown");
    let on = model(&cfg(), &b.launch(), Some(&own), None, "model").expect("flown");
    assert_eq!(on.flight, None);
    let force = SimulationSection {
        mode: SimMode::Force,
        ..cfg()
    };
    let forced = model(&force, &b.launch(), Some(&own), None, "model")
        .and_then(|m| m.flight)
        .expect("forced");
    assert_eq!(forced.total_distance, own.total_distance);
    assert_eq!(forced.total_offline, own.total_offline);
}
