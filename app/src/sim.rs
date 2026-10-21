//! Simulated flight and roll (beta).
//!
//! Fills in the values a launch monitor does not report (carry, apex, flight
//! time, descent angle, offline, roll, total) by simulating the measured
//! launch with the `dimple` ball-flight model. Measured values always win:
//! [`derive`] only emits a field when the device left it empty, and the
//! device's `BallFlight` is never modified. GSPro never sees these values.
//!
//! Device actors call [`ball_flight_event`] to build every `BallFlight` bus
//! event, so the event leaves the actor already carrying `derived`.

use std::sync::Mutex;

use dimple::aero::BallModel;
use dimple::environment::Environment;
use dimple::ground::{Surface, Termination};
use dimple::stats::ShotStats;
use dimple::trajectory::{ShotInput, simulate_shot};
use flighthook::{
    BallFlight, DerivedFlight, Distance, FlighthookEvent, ShotDetectionMode, ShotKey, SimSurface,
    SimulationSection,
};
use tracing::{debug, info, warn};

use crate::state::SystemState;

/// Slowest ball speed simulated (mph).
const MIN_SPEED_MPH: f64 = 5.0;
/// A shot slower than this (mph) with a launch below [`PUTT_MAX_LAUNCH_DEG`]
/// is treated as a putt, which catches putts when the detection mode is
/// unknown.
const PUTT_MAX_SPEED_MPH: f64 = 15.0;
/// See [`PUTT_MAX_SPEED_MPH`].
const PUTT_MAX_LAUNCH_DEG: f64 = 5.0;

/// The last invalid environment warned about, so the warning is logged once
/// per distinct invalid setting rather than on every shot.
static WARNED_ENVIRONMENT: Mutex<Option<String>> = Mutex::new(None);

/// Whether shots should be simulated at all: enabled, and not putting.
pub(crate) fn should_simulate(cfg: &SimulationSection, mode: Option<ShotDetectionMode>) -> bool {
    cfg.enabled && mode != Some(ShotDetectionMode::Putting)
}

/// The dimple surface preset for a configured surface.
fn surface(s: SimSurface) -> Surface {
    match s {
        SimSurface::Fairway => Surface::FAIRWAY,
        SimSurface::Green => Surface::GREEN,
        SimSurface::FirmGreen => Surface::FIRM_GREEN,
    }
}

/// The dimple input for a measured launch, or why it cannot be simulated.
///
/// All five launch values must be present and finite. No direction is
/// assumed for a device that does not report it, but zero on both spin axes
/// is treated as a missing reading rather than a knuckleball: a struck ball
/// always spins, so a source with no spin measurement (e.g. an Open Connect
/// monitor that sends no `BackSpin`/`SideSpin`) reports exactly this.
fn shot_input(ball: &BallFlight) -> Result<ShotInput, &'static str> {
    let (Some(speed), Some(vla), Some(hla), Some(back), Some(side)) = (
        ball.launch_speed,
        ball.launch_elevation,
        ball.launch_azimuth,
        ball.backspin_rpm,
        ball.sidespin_rpm,
    ) else {
        return Err("launch data incomplete");
    };
    let speed_mph = speed.as_mph();
    if !(speed_mph.is_finite() && vla.is_finite() && hla.is_finite()) {
        return Err("launch data not finite");
    }
    if back == 0 && side == 0 {
        return Err("no spin reading");
    }
    if speed_mph < MIN_SPEED_MPH {
        return Err("ball speed too low");
    }
    if !(vla > 0.0 && vla < 90.0) {
        return Err("launch angle outside 0 to 90 deg");
    }
    if hla.abs() >= 90.0 {
        return Err("launch direction outside -90 to 90 deg");
    }
    if speed_mph < PUTT_MAX_SPEED_MPH && vla < PUTT_MAX_LAUNCH_DEG {
        return Err("looks like a putt");
    }
    Ok(ShotInput {
        ball_speed_mph: speed_mph,
        launch_angle_deg: vla,
        // FRP and dimple both use positive = right: no sign change.
        launch_azimuth_deg: hla,
        backspin_rpm: f64::from(back),
        sidespin_rpm: f64::from(side),
    })
}

/// Simulate `ball` under `cfg` and return the values the device did not
/// report, or `None` when the launch or environment can't be simulated.
///
/// Where the device reports carry (or roll), the simulated total is that
/// measured carry plus the simulated roll, so carry + roll = total holds for
/// the values a consumer displays.
///
/// Test-only convenience: production code computes the environment error
/// once per shot in [`ball_flight_event`] and calls [`derive_with_error`]
/// directly, since it already needs that error for the log-once warning.
#[cfg(test)]
pub(crate) fn derive(ball: &BallFlight, cfg: &SimulationSection) -> Option<DerivedFlight> {
    derive_with_error(ball, cfg, cfg.environment_error().as_deref())
}

/// Like [`derive`], but takes the environment error already computed by the
/// caller instead of recomputing it.
fn derive_with_error(
    ball: &BallFlight,
    cfg: &SimulationSection,
    env_err: Option<&str>,
) -> Option<DerivedFlight> {
    if env_err.is_some() {
        return None;
    }
    let input = match shot_input(ball) {
        Ok(input) => input,
        Err(reason) => {
            debug!("not simulated: {reason}");
            return None;
        }
    };
    // Not `Environment::SEA_LEVEL` (15 C): the default is the ball model's
    // 25 C calibration point, carried in the config default.
    let env = Environment {
        altitude_m: cfg.altitude.as_meters(),
        temperature_c: cfg.temperature_c,
    };
    let shot = simulate_shot(&input, &BallModel::TOUR, &env, &surface(cfg.surface));
    let f = &shot.flight;
    let descent = ShotStats::from_shot(&shot).descent_angle_deg;
    let flight_ok = f.flight_time > 0.0
        && f.points.len() >= 2
        && [f.flight_time, f.carry_m, f.apex_m, f.lateral_m, descent]
            .iter()
            .all(|v| v.is_finite());
    if !flight_ok {
        debug!("not simulated: degenerate flight");
        return None;
    }

    let mut d = DerivedFlight::default();
    if ball.carry_distance.is_none() {
        d.carry_distance = Some(Distance::Meters(f.carry_m));
    }
    d.carry_offline = Some(Distance::Meters(f.lateral_m));
    if ball.max_height.is_none() {
        d.max_height = Some(Distance::Meters(f.apex_m));
    }
    if ball.flight_time.is_none() {
        d.flight_time = Some(f.flight_time);
    }
    d.descent_angle = Some(descent);

    let rest = shot.ground.termination == Termination::Rest
        && shot.total_m.is_finite()
        && shot.final_pos.z.is_finite();
    if rest {
        let sim_roll = shot.total_m - f.carry_m;
        if ball.roll_distance.is_none() {
            d.roll_distance = Some(Distance::Meters(sim_roll));
        }
        d.roll_offline = Some(Distance::Meters(shot.final_pos.z - f.lateral_m));
        if ball.total_distance.is_none() {
            let carry = ball.carry_distance.map_or(f.carry_m, Distance::as_meters);
            let roll = ball.roll_distance.map_or(sim_roll, Distance::as_meters);
            let total = carry + roll;
            if total.is_finite() {
                d.total_distance = Some(Distance::Meters(total));
            }
        }
        d.total_offline = Some(Distance::Meters(shot.final_pos.z));
    }

    // Guards a future optional-only field; every field set above is
    // unconditional or follows from `flight_ok`, so `d` is never empty today.
    (!d.is_empty()).then_some(d)
}

/// The console summary of a simulated shot: only the fields present, in a
/// fixed order, ASCII only, imperial. Offlines and roll carry an explicit
/// sign (roll is negative when the ball spins back).
pub(crate) fn log_line(d: &DerivedFlight) -> String {
    let mut line = String::from("sim (estimated, beta):");
    let mut push = |s: String| {
        line.push(' ');
        line.push_str(&s);
    };
    if let Some(v) = d.carry_distance {
        push(format!("carry={:.1}yd", v.as_yards()));
    }
    if let Some(v) = d.carry_offline {
        push(format!("offline={:+.1}yd", v.as_yards()));
    }
    if let Some(v) = d.max_height {
        push(format!("apex={:.1}ft", v.as_feet()));
    }
    if let Some(v) = d.flight_time {
        push(format!("time={v:.1}s"));
    }
    if let Some(v) = d.descent_angle {
        push(format!("descent={v:.1}deg"));
    }
    if let Some(v) = d.roll_distance {
        push(format!("roll={:+.1}yd", v.as_yards()));
    }
    if let Some(v) = d.roll_offline {
        push(format!("roll_offline={:+.1}yd", v.as_yards()));
    }
    if let Some(v) = d.total_distance {
        push(format!("total={:.1}yd", v.as_yards()));
    }
    if let Some(v) = d.total_offline {
        push(format!("total_offline={:+.1}yd", v.as_yards()));
    }
    line
}

/// Log an invalid environment once per distinct setting. A valid setting
/// clears the latch, so the warning comes back if it goes invalid again.
fn warn_invalid_environment(cfg: &SimulationSection, env_err: Option<&str>) {
    let mut last = WARNED_ENVIRONMENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match env_err {
        None => *last = None,
        Some(err) => {
            let key = format!("{:?}/{:?}", cfg.altitude, cfg.temperature_c);
            if last.as_deref() != Some(key.as_str()) {
                warn!("[simulation] {err}; shots are not simulated until it is fixed");
                *last = Some(key);
            }
        }
    }
}

/// Build the `BallFlight` bus event for a measured ball: reads the live
/// `[simulation]` settings and detection mode, simulates the missing values,
/// and logs the `sim` line under the device's own ball line.
pub(crate) fn ball_flight_event(
    state: &SystemState,
    key: ShotKey,
    ball: BallFlight,
) -> FlighthookEvent {
    let cfg = state.system.simulation();
    let mode = state.game.snapshot().mode;
    let derived = if should_simulate(&cfg, mode) {
        let env_err = cfg.environment_error();
        warn_invalid_environment(&cfg, env_err.as_deref());
        derive_with_error(&ball, &cfg, env_err.as_deref())
    } else {
        None
    };
    if let Some(ref d) = derived {
        info!("  {}", log_line(d));
    }
    FlighthookEvent::BallFlight {
        key,
        ball: Box::new(ball),
        derived: derived.map(Box::new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flighthook::Velocity;

    fn ball(speed_mph: f64, vla: f64, hla: f64, back: i32, side: i32) -> BallFlight {
        BallFlight {
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
        }
    }

    /// A Square/R10-like launch: launch data only.
    fn square_like() -> BallFlight {
        ball(120.0, 17.0, -1.2, 6500, -300)
    }

    fn from_input(i: &ShotInput) -> BallFlight {
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
        writer: crate::state::GameStateWriter,
        dir: std::path::PathBuf,
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

    fn key() -> ShotKey {
        ShotKey {
            shot_id: "abc".into(),
            shot_number: 1,
        }
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
        assert_eq!(d, expected);
        assert!(d.carry_offline.is_some() && d.roll_offline.is_some());
        assert!(d.total_offline.is_some() && d.descent_angle.is_some());
    }

    // -- 2. Derived is flagged, ball untouched ---------------------------------

    #[test]
    fn event_keeps_ball_and_flags_derived() {
        let t = state_with(cfg());
        let b = square_like();
        let ev = ball_flight_event(&t.state, key(), b.clone());
        let FlighthookEvent::BallFlight {
            ball: out, derived, ..
        } = &ev
        else {
            panic!("not a ball flight: {ev:?}");
        };
        assert_eq!(**out, b);
        assert!(derived.is_some());
        let json = serde_json::to_value(&ev).expect("serializes");
        assert!(json["derived"]["carry_distance"].is_string(), "{json}");
        assert!(json["ball"].get("carry_distance").is_none(), "{json}");
    }

    #[test]
    fn disabling_simulation_removes_derived_without_restart() {
        let t = state_with(cfg());
        let on = ball_flight_event(&t.state, key(), square_like());
        assert!(matches!(
            on,
            FlighthookEvent::BallFlight {
                derived: Some(_),
                ..
            }
        ));
        t.state.system.update(|c| c.simulation.enabled = false);
        let off = ball_flight_event(&t.state, key(), square_like());
        assert!(matches!(
            off,
            FlighthookEvent::BallFlight { derived: None, .. }
        ));
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
            (f64::NAN, 17.0, 0.0),
            (f64::INFINITY, 17.0, 0.0),
            (f64::NEG_INFINITY, 17.0, 0.0),
            (120.0, f64::NAN, 0.0),
            (120.0, 17.0, f64::INFINITY),
            (4.9, 30.0, 0.0),   // below 5 mph
            (150.0, -5.0, 0.0), // topped
            (150.0, 0.0, 0.0),
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
    }

    #[test]
    fn slow_low_launch_is_treated_as_a_putt() {
        assert_eq!(derive(&ball(7.4, 2.0, 0.0, 300, 0), &cfg()), None);
        assert_eq!(derive(&ball(14.9, 4.9, 0.0, 300, 0), &cfg()), None);
        // A chip at the same speed but launched higher is simulated.
        assert!(derive(&ball(14.9, 25.0, 0.0, 3000, 0), &cfg()).is_some());
        // So is a fast low shot.
        assert!(derive(&ball(60.0, 4.0, 0.0, 3000, 0), &cfg()).is_some());
    }

    #[test]
    fn should_simulate_gates_on_enabled_and_putting() {
        let on = cfg();
        let off = SimulationSection {
            enabled: false,
            ..cfg()
        };
        assert!(should_simulate(&on, None));
        assert!(should_simulate(&on, Some(ShotDetectionMode::Full)));
        assert!(should_simulate(&on, Some(ShotDetectionMode::Chipping)));
        assert!(!should_simulate(&on, Some(ShotDetectionMode::Putting)));
        assert!(!should_simulate(&off, None));
        assert!(!should_simulate(&off, Some(ShotDetectionMode::Full)));
    }

    #[test]
    fn putting_mode_skips_the_simulation() {
        let t = state_with(cfg());
        t.writer.set_mode(ShotDetectionMode::Chipping);
        let chip = ball_flight_event(&t.state, key(), square_like());
        assert!(matches!(
            chip,
            FlighthookEvent::BallFlight {
                derived: Some(_),
                ..
            }
        ));
        t.writer.set_mode(ShotDetectionMode::Putting);
        let putt = ball_flight_event(&t.state, key(), square_like());
        assert!(matches!(
            putt,
            FlighthookEvent::BallFlight { derived: None, .. }
        ));
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
        let env = Environment {
            altitude_m: 0.0,
            temperature_c: 25.0,
        };
        let shot = simulate_shot(input, &BallModel::TOUR, &env, &Surface::FAIRWAY);
        assert_eq!(shot.ground.termination, Termination::Rest);
        let f = &shot.flight;
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(m(d.carry_distance), f.carry_m));
        assert!(close(m(d.carry_offline), f.lateral_m));
        assert!(close(m(d.max_height), f.apex_m));
        assert!(close(d.flight_time.expect("time"), f.flight_time));
        assert!(close(
            d.descent_angle.expect("descent"),
            ShotStats::from_shot(&shot).descent_angle_deg
        ));
        assert!(close(m(d.roll_distance), shot.total_m - f.carry_m));
        assert!(close(m(d.roll_offline), shot.final_pos.z - f.lateral_m));
        assert!(close(m(d.total_distance), shot.total_m));
        assert!(close(m(d.total_offline), shot.final_pos.z));
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
    }
}
