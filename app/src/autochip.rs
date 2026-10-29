//! Auto-chip (beta): a short, straight chip of a chosen total distance, sent
//! for chips the launch monitor can't read.
//!
//! The chip is launched straight (HLA 0) at [`LAUNCH_ELEVATION_DEG`], with
//! backspin [`MevoSection::DEFAULT_LAUNCH_ONLY_SPIN`] rpm per mph of ball
//! speed and no sidespin. Its ball speed is solved ([`solve_speed`]) so that
//! flighthook's own simulation, in the configured environment and on the
//! configured surface, rests at the chosen total distance (carry + roll).
//!
//! [`send`] puts the shot on the bus as the virtual device
//! [`AUTO_CHIP_ACTOR`], with the same lifecycle a device emits (trigger,
//! ball flight with the simulated values, finished; no club data), so the
//! outputs forward it and the web layer stores it like any shot. The chip is
//! wholly flighthook's, so its ball flight always carries the simulated
//! values, whatever the `[simulation]` mode.

use dimple::aero::BallModel;
use dimple::ground::Termination;
use dimple::trajectory::{ShotInput, simulate_shot};
use flighthook::{
    AUTO_CHIP_ACTOR, BallFlight, FlighthookEvent, FlighthookMessage, MevoSection,
    ShotDetectionMode, ShotKey, ShotRaw, SimulationSection, Velocity,
};
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::broadcast;
use tracing::info;

use crate::sim;
use crate::state::SystemState;

/// Launch angle of every Auto-chip, degrees.
pub(crate) const LAUNCH_ELEVATION_DEG: f64 = 28.0;

/// Metres in a yard.
const M_PER_YD: f64 = 0.9144;

/// Lowest ball speed the solver tries, mph.
const MIN_SPEED_MPH: f64 = 0.1;
/// Highest ball speed the solver tries, mph: far beyond any chip.
const MAX_SPEED_MPH: f64 = 60.0;
/// The solver stops once the speed bracket is this narrow, mph.
const SPEED_TOLERANCE_MPH: f64 = 1e-4;

/// Why no speed could be solved for a distance.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SolveError {
    /// The `[simulation]` environment is invalid (the reason).
    Environment(String),
    /// The target is not positive and finite.
    Distance,
    /// The simulation does not rest within the target at any speed tried.
    Unreachable,
}

impl std::fmt::Display for SolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Environment(e) => write!(f, "simulation environment invalid: {e}"),
            Self::Distance => write!(f, "distance must be positive"),
            Self::Unreachable => write!(f, "no ball speed gives that distance"),
        }
    }
}

/// The chip's backspin at `speed_mph`: the default launch-only spin times
/// the speed, rounded to whole rpm.
#[allow(clippy::cast_possible_truncation)] // speeds are at most MAX_SPEED_MPH
pub(crate) fn backspin_rpm(speed_mph: f64) -> i32 {
    (MevoSection::DEFAULT_LAUNCH_ONLY_SPIN * speed_mph).round() as i32
}

/// The launch flighthook sends for a chip at `speed_mph`: straight, at
/// [`LAUNCH_ELEVATION_DEG`], with [`backspin_rpm`] and no sidespin.
pub(crate) fn ball(speed_mph: f64) -> BallFlight {
    BallFlight {
        launch_speed: Some(Velocity::MilesPerHour(speed_mph)),
        launch_elevation: Some(LAUNCH_ELEVATION_DEG),
        launch_azimuth: Some(0.0),
        backspin_rpm: Some(backspin_rpm(speed_mph)),
        sidespin_rpm: Some(0),
        ..BallFlight::default()
    }
}

/// The simulated total (carry + roll), metres, of a chip at `speed_mph`
/// under `cfg`, or `None` when the ball does not come to rest.
fn simulated_total_m(speed_mph: f64, cfg: &SimulationSection) -> Option<f64> {
    let input = ShotInput {
        ball_speed_mph: speed_mph,
        launch_angle_deg: LAUNCH_ELEVATION_DEG,
        launch_azimuth_deg: 0.0,
        backspin_rpm: f64::from(backspin_rpm(speed_mph)),
        sidespin_rpm: 0.0,
    };
    let shot = simulate_shot(
        &input,
        &BallModel::TOUR,
        &sim::environment(cfg),
        &sim::surface(cfg.surface),
    );
    (shot.ground.termination == Termination::Rest && shot.total_m.is_finite())
        .then_some(shot.total_m)
}

/// The ball speed, mph, at which the chip's simulated total under `cfg` is
/// `yards`: bisection between [`MIN_SPEED_MPH`] and [`MAX_SPEED_MPH`] to
/// [`SPEED_TOLERANCE_MPH`]. A target shorter than the slowest chip's total
/// gets [`MIN_SPEED_MPH`], the closest the simulation comes.
pub(crate) fn solve_speed(yards: f64, cfg: &SimulationSection) -> Result<f64, SolveError> {
    if let Some(e) = cfg.environment_error() {
        return Err(SolveError::Environment(e));
    }
    if !(yards.is_finite() && yards > 0.0) {
        return Err(SolveError::Distance);
    }
    let target = yards * M_PER_YD;
    // Short of the target (or not at rest) counts as too slow.
    let short = |speed: f64| simulated_total_m(speed, cfg).is_none_or(|t| t < target);
    if !short(MIN_SPEED_MPH) {
        return Ok(MIN_SPEED_MPH);
    }
    if short(MAX_SPEED_MPH) {
        return Err(SolveError::Unreachable);
    }
    let (mut lo, mut hi) = (MIN_SPEED_MPH, MAX_SPEED_MPH);
    while hi - lo > SPEED_TOLERANCE_MPH {
        let mid = 0.5 * (lo + hi);
        if short(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(0.5 * (lo + hi))
}

/// Solve and send an Auto-chip of `yards` under the live `[simulation]`
/// settings, as the next shot of [`AUTO_CHIP_ACTOR`]: `shots` counts the
/// Auto-chip shots sent, and is advanced only once the solve has succeeded,
/// so a chip that is not sent takes no number. Blocks for the solve (some
/// tens of simulations). Returns the shot's key and ball.
pub(crate) fn send(
    state: &SystemState,
    bus: &broadcast::Sender<FlighthookMessage>,
    yards: u32,
    shots: &AtomicU32,
) -> Result<(ShotKey, BallFlight), SolveError> {
    let cfg = state.system.simulation();
    let speed = solve_speed(f64::from(yards), &cfg)?;
    let shot_number = shots.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    let key = ShotKey {
        shot_id: uuid::Uuid::new_v4().to_string(),
        shot_number,
    };
    let ball = ball(speed);
    info!(
        "auto-chip shot #{shot_number}: {yards}yd ball={speed:.2}mph VLA={LAUNCH_ELEVATION_DEG:.1} backspin={}rpm",
        backspin_rpm(speed),
    );
    let send = |event| {
        let _ = bus.send(FlighthookMessage::new(event).actor(AUTO_CHIP_ACTOR));
    };
    send(FlighthookEvent::ShotTrigger { key: key.clone() });
    send(sim::simulated_ball_flight_event(
        &cfg,
        key.clone(),
        ball.clone(),
        Some(ShotDetectionMode::Chipping),
    ));
    send(FlighthookEvent::ShotFinished {
        key: key.clone(),
        raw: Some(Box::new(ShotRaw {
            transport: "none".into(),
            unavailable: Some("auto-chip has no device".into()),
            ..ShotRaw::default()
        })),
    });
    Ok((key, ball))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flighthook::{AUTO_CHIP_YARDS, Distance, SimSurface};

    /// The total the shot pipeline shows for a chip at `speed_mph`, yards.
    fn derived_total_yd(speed_mph: f64, cfg: &SimulationSection) -> f64 {
        sim::derive(&ball(speed_mph), cfg)
            .and_then(|d| d.total_distance)
            .map(Distance::as_yards)
            .expect("a simulated total")
    }

    fn configs() -> Vec<SimulationSection> {
        let green = SimulationSection {
            surface: SimSurface::Green,
            ..SimulationSection::default()
        };
        let high_and_cold = SimulationSection {
            altitude: Distance::Meters(1600.0),
            temperature_c: 5.0,
            surface: SimSurface::FirmGreen,
            ..SimulationSection::default()
        };
        vec![SimulationSection::default(), green, high_and_cold]
    }

    #[test]
    fn solved_speed_gives_the_target_total_within_a_tenth_of_a_yard() {
        for cfg in configs() {
            for yards in AUTO_CHIP_YARDS {
                let speed = solve_speed(f64::from(yards), &cfg).expect("solved");
                let total = derived_total_yd(speed, &cfg);
                assert!(
                    (total - f64::from(yards)).abs() <= 0.1,
                    "{yards} yd on {:?}: {speed} mph -> {total} yd",
                    cfg.surface
                );
            }
        }
    }

    #[test]
    fn speed_rises_with_distance() {
        for cfg in configs() {
            let speeds: Vec<f64> = [0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 7.0, 10.0]
                .iter()
                .map(|&y| solve_speed(y, &cfg).expect("solved"))
                .collect();
            assert!(speeds.windows(2).all(|w| w[0] < w[1]), "{speeds:?}");
        }
    }

    #[test]
    fn the_launch_is_straight_at_28_deg_with_speed_scaled_backspin() {
        let b = ball(7.25);
        assert_eq!(b.launch_speed, Some(Velocity::MilesPerHour(7.25)));
        assert_eq!(b.launch_elevation, Some(28.0));
        assert_eq!(b.launch_azimuth, Some(0.0));
        assert_eq!(b.backspin_rpm, Some(725));
        assert_eq!(b.sidespin_rpm, Some(0));
        assert_eq!(b.carry_distance, None);
        assert_eq!(b.total_distance, None);
        assert_eq!(backspin_rpm(7.254), 725);
        assert_eq!(backspin_rpm(7.256), 726);
    }

    #[test]
    fn no_speed_without_a_valid_environment_or_distance() {
        let mut cfg = SimulationSection::default();
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(solve_speed(bad, &cfg), Err(SolveError::Distance), "{bad}");
        }
        assert_eq!(solve_speed(500.0, &cfg), Err(SolveError::Unreachable));
        cfg.temperature_c = 80.0;
        assert!(matches!(
            solve_speed(3.0, &cfg),
            Err(SolveError::Environment(_))
        ));
    }

    /// A target shorter than the slowest chip gets the slowest speed.
    #[test]
    fn a_target_below_the_slowest_chip_gets_the_slowest_speed() {
        let cfg = SimulationSection::default();
        assert_eq!(solve_speed(1e-6, &cfg), Ok(MIN_SPEED_MPH));
    }

    /// `send` puts trigger, ball flight (with the simulated total, in
    /// Chipping mode, no club) and finished on the bus as the Auto-chip
    /// actor, all under one key.
    #[test]
    fn send_emits_a_full_shot_lifecycle_as_auto_chip() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-autochip-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let (tx, mut rx) = broadcast::channel(16);
        let (key, ball) = send(&state, &tx, 5, &AtomicU32::new(6)).expect("sent");
        assert_eq!(key.shot_number, 7);

        let mut events = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            assert_eq!(msg.actor, AUTO_CHIP_ACTOR);
            events.push(msg.event);
        }
        assert_eq!(events.len(), 3, "{events:?}");
        assert!(matches!(&events[0], FlighthookEvent::ShotTrigger { key: k } if *k == key));
        let FlighthookEvent::BallFlight {
            key: k,
            ball: b,
            derived,
        } = &events[1]
        else {
            panic!("not a ball flight: {:?}", events[1]);
        };
        assert_eq!(*k, key);
        assert_eq!(**b, ball);
        let speed = ball.launch_speed.expect("speed").as_mph();
        assert_eq!(ball.backspin_rpm, Some(backspin_rpm(speed)));
        let d = derived.as_deref().expect("simulated");
        let total = d.total_distance.expect("total").as_yards();
        assert!((total - 5.0).abs() <= 0.1, "{total}");
        assert!(d.carry_distance.is_some_and(|c| c.as_yards() < total));
        assert!(!d.spin_estimated);
        assert!(matches!(
            &events[2],
            FlighthookEvent::ShotFinished { key: k, raw: Some(_) } if *k == key
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// With `[simulation] mode = "off"` the chip still carries its simulated
    /// carry and total: they are its only results.
    #[test]
    fn send_simulates_the_chip_with_the_simulation_off() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-autochip-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        state
            .system
            .update(|c| c.simulation.mode = flighthook::SimMode::Off);
        let (tx, mut rx) = broadcast::channel(16);
        send(&state, &tx, 3, &AtomicU32::new(0)).expect("sent");
        let derived = std::iter::from_fn(|| rx.try_recv().ok())
            .find_map(|msg| match msg.event {
                FlighthookEvent::BallFlight { derived, .. } => Some(derived),
                _ => None,
            })
            .expect("a ball flight")
            .expect("simulated");
        let total = derived.total_distance.expect("total").as_yards();
        assert!((total - 3.0).abs() <= 0.1, "{total}");
        assert!(derived.carry_distance.is_some_and(|c| c.as_yards() < total));
        assert!(!derived.replaces_measured);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A chip that can't be solved takes no shot number; the next one that
    /// is sent takes the next number.
    #[test]
    fn a_failed_chip_takes_no_shot_number() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-autochip-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let (tx, _rx) = broadcast::channel(16);
        let shots = AtomicU32::new(0);
        assert_eq!(send(&state, &tx, 1, &shots).expect("sent").0.shot_number, 1);
        assert_eq!(
            send(&state, &tx, 500, &shots).map(|_| ()),
            Err(SolveError::Unreachable)
        );
        state.system.update(|c| c.simulation.temperature_c = 99.0);
        assert!(matches!(
            send(&state, &tx, 3, &shots),
            Err(SolveError::Environment(_))
        ));
        assert_eq!(shots.load(Ordering::Relaxed), 1);
        state.system.update(|c| c.simulation.temperature_c = 25.0);
        assert_eq!(send(&state, &tx, 3, &shots).expect("sent").0.shot_number, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The solved speeds and simulated carry/total on the default settings.
    #[test]
    #[ignore = "report"]
    fn print_default_solutions() {
        let cfg = SimulationSection::default();
        for yards in AUTO_CHIP_YARDS {
            let speed = solve_speed(f64::from(yards), &cfg).expect("solved");
            let d = sim::derive(&ball(speed), &cfg).expect("derived");
            println!(
                "{yards} yd: {speed:.3} mph, backspin {} rpm, carry {:.2} yd, roll {:.2} yd, total {:.3} yd",
                backspin_rpm(speed),
                d.carry_distance.expect("carry").as_yards(),
                d.roll_distance.expect("roll").as_yards(),
                d.total_distance.expect("total").as_yards(),
            );
        }
    }
}
