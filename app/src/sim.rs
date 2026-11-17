//! Simulated flight and roll (beta).
//!
//! Fills in the values a launch monitor does not report (carry, apex, flight
//! time, descent angle, offline, roll, total) by simulating the measured
//! launch with the `dimple` ball-flight model. `[simulation] mode` picks
//! what is simulated: `off` simulates nothing; `on` (the default) lets
//! measured values win, so [`derive`] only emits a field when the device left
//! it empty; `force` makes [`derive`] emit every result field and flag the
//! block `replaces_measured`, so the simulated results take precedence over
//! the device's. Either way the device's launch and flight result are never
//! modified, and GSPro never sees these values.
//!
//! `derived.path` carries the simulated ball path, fitted to the values the
//! device measured ([`anchor_path`]; nothing counts as measured under
//! `force`, so the path is the raw simulation) and decimated to a
//! few dozen points; the offline values are read from that path, so the
//! path, the offlines and the displayed carry and total describe one
//! geometry.
//!
//! A ground ball ([`is_ground_ball`]: a measured apex of 0, or no measured
//! apex and a launch angle of at most [`GROUND_MAX_LAUNCH_DEG`]) never
//! leaves the ground, whatever the device, club or detection mode: carry,
//! apex and flight time are 0, and the path is a roll from the tee,
//! simulated by dimple's ground model from the launch speed and direction
//! ([`derive_ground_ball`]), on a green when the shot was taken in Putting
//! mode and on the configured surface otherwise ([`ground_surface`]). On
//! any other shot a measured carry or flight time of exactly 0 is not a
//! reading, so it counts as not measured: the simulation fills it in and the
//! path is fitted only to the values really measured.
//!
//! Frame (path and offlines alike): `x` downrange, `y` up, `z` right of the
//! target line, origin at the tee. Positive `z` is right, the sign of FRP's
//! launch direction and of every `*_offline` value.
//!
//! The values a device measured are its launch; its flight values (carry,
//! apex and so on) come in its flight result. The flight model
//! ([`crate::actors::flight_model`]) and Auto-chip fly a shot with
//! [`model`], which turns the simulation into a `shot.flight` result.

use std::sync::Mutex;

use dimple::Vec3;
use dimple::aero::BallModel;
use dimple::environment::Environment;
use dimple::ground::{BallState, GroundModel, GroundResult, Surface, Termination};
use dimple::stats::ShotStats;
use dimple::trajectory::{ShotInput, ShotResult, simulate_shot};
use flighthook::{
    Basis, DerivedFlight, Distance, FlightPath, FlightResult, Launch, Mode, SimMode, SimSurface,
    SimulationSection,
};
#[cfg(test)]
use tracing::info;
use tracing::{debug, warn};

#[cfg(test)]
use crate::state::SystemState;

/// A measured apex within this of 0 (m) makes a ground ball.
const GROUND_MAX_APEX_M: f64 = 0.01;
/// Without a measured apex, a launch angle at or below this (deg) makes a
/// ground ball: too little lift to fly.
const GROUND_MAX_LAUNCH_DEG: f64 = 0.5;

/// The last invalid environment warned about, so the warning is logged once
/// per distinct invalid setting rather than on every shot.
static WARNED_ENVIRONMENT: Mutex<Option<String>> = Mutex::new(None);

/// Whether a shot is a ground ball, rolled rather than flown: the device's
/// apex is 0 (finite, within [`GROUND_MAX_APEX_M`]), or it reported no apex
/// and a launch angle above -90 and at most [`GROUND_MAX_LAUNCH_DEG`], or
/// neither a launch angle nor any spin (a putt measured along the ground, as
/// a putting mat reports it). The device, club and detection mode play no
/// part.
fn is_ground_ball(launch: &Launch, flight: Option<&FlightResult>) -> bool {
    match flight.and_then(|f| f.max_height).map(Distance::as_meters) {
        Some(apex) => apex.is_finite() && apex.abs() <= GROUND_MAX_APEX_M,
        None => match launch.launch_elevation {
            Some(vla) => vla > -90.0 && vla <= GROUND_MAX_LAUNCH_DEG,
            None => [
                launch.backspin_rpm,
                launch.sidespin_rpm,
                launch.total_spin_rpm,
            ]
            .iter()
            .all(Option::is_none),
        },
    }
}

/// The dimple surface preset for a configured surface.
pub(crate) fn surface(s: SimSurface) -> Surface {
    match s {
        SimSurface::Fairway => Surface::FAIRWAY,
        SimSurface::Green => Surface::GREEN,
        SimSurface::FirmGreen => Surface::FIRM_GREEN,
    }
}

/// The surface a ground ball rolls on: in Putting mode a green (the
/// configured one when it is a green, else [`Surface::GREEN`]); in any other
/// mode, or with no mode, the configured surface.
fn ground_surface(configured: SimSurface, mode: Option<Mode>) -> Surface {
    match (mode, configured) {
        (Some(Mode::Putting), SimSurface::Fairway) => Surface::GREEN,
        _ => surface(configured),
    }
}

/// The dimple input for a measured launch, or why it cannot be simulated.
///
/// All five launch values must be present and finite. No direction is
/// assumed for a device that does not report it, but zero on both spin axes
/// is treated as a missing reading rather than a knuckleball: a struck ball
/// always spins, so a source with no spin measurement (e.g. an Open Connect
/// monitor that sends no `BackSpin`/`SideSpin`) reports exactly this. A
/// total spin with no axis is flown as backspin with no sidespin.
fn shot_input(ball: &Launch) -> Result<ShotInput, &'static str> {
    let (back, side) = match ball.spin_components() {
        (Some(back), None) if ball.total_spin_rpm.is_some() => (Some(back), Some(0)),
        pair => pair,
    };
    let (Some(speed), Some(vla), Some(hla), Some(back), Some(side)) = (
        ball.launch_speed,
        ball.launch_elevation,
        ball.launch_azimuth,
        back,
        side,
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
    if speed_mph <= 0.0 {
        return Err("ball speed not positive");
    }
    if !(vla > 0.0 && vla < 90.0) {
        return Err("launch angle outside 0 to 90 deg");
    }
    if hla.abs() >= 90.0 {
        return Err("launch direction outside -90 to 90 deg");
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

/// Simulate the device's `launch` under `cfg` and return the values the
/// device did not report in its `flight` result, or `None` when the launch
/// or environment can't be simulated.
///
/// Where the device reports carry (or roll), the simulated total is that
/// measured carry plus the simulated roll, so carry + roll = total holds for
/// the values a consumer displays. The path is simulated, fitted to the
/// measured values ([`anchor_path`]), decimated and rounded to centimeters
/// and milliseconds; the offlines are its landing and rest `z`.
///
/// With [`SimMode::Force`], nothing the device reported counts as
/// measured: every result field is the simulation's, the path is the raw
/// simulation, and `replaces_measured` is set.
///
/// A ground ball ([`is_ground_ball`]) is rolled instead
/// ([`derive_ground_ball`]).
///
/// Test-only convenience with no detection mode: production code computes
/// the environment error once per shot in [`launch_events_in_mode`] and
/// calls [`derive_with_error`] directly, since it already needs that error
/// for the log-once warning.
#[cfg(test)]
pub(crate) fn derive(
    launch: &Launch,
    flight: Option<&FlightResult>,
    cfg: &SimulationSection,
) -> Option<DerivedFlight> {
    derive_with_error(
        launch,
        flight,
        cfg,
        None,
        cfg.environment_error().as_deref(),
    )
}

/// Like [`derive`], for a shot taken in detection mode `mode`, taking the
/// environment error already computed by the caller instead of recomputing
/// it. The mode only picks a ground ball's surface ([`ground_surface`]).
fn derive_with_error(
    launch: &Launch,
    flight: Option<&FlightResult>,
    cfg: &SimulationSection,
    mode: Option<Mode>,
    env_err: Option<&str>,
) -> Option<DerivedFlight> {
    if env_err.is_some() {
        return None;
    }
    if is_ground_ball(launch, flight) {
        return derive_ground_ball(launch, flight, cfg, ground_surface(cfg.surface, mode));
    }
    let input = match shot_input(launch) {
        Ok(input) => input,
        Err(reason) => {
            debug!("not simulated: {reason}");
            return None;
        }
    };
    let env = environment(cfg);
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

    let rest = shot.ground.termination == Termination::Rest
        && shot.total_m.is_finite()
        && shot.final_pos.z.is_finite();
    let raw = FullPath::from_shot(&shot, rest);
    let force = cfg.mode == SimMode::Force;
    let measured = if force {
        Measured::default()
    } else {
        Measured::from_shot(launch, flight).without_zero_flight()
    };
    let anchored = finite_path(anchor_path(&raw, &measured));

    let mut d = DerivedFlight::default();
    d.replaces_measured = force;
    if measured.carry.is_none() {
        d.carry_distance = Some(Distance::Meters(f.carry_m));
    }
    // Offlines are read from the anchored path, so they sit at the displayed
    // landing and rest; without a path they are the raw simulation's. A
    // measured carry offline is the displayed one (the path lands on it).
    let carry_offline = measured.offline.unwrap_or_else(|| {
        anchored
            .as_ref()
            .and_then(|p| p.flight.last())
            .map_or(f.lateral_m, |l| l[3])
    });
    if measured.offline.is_none() {
        d.carry_offline = Some(Distance::Meters(carry_offline));
    }
    if measured.apex.is_none() {
        d.max_height = Some(Distance::Meters(f.apex_m));
    }
    if measured.flight_time.is_none() {
        d.flight_time = Some(f.flight_time);
    }
    if measured.descent.is_none() {
        d.descent_angle = Some(descent);
    }

    if rest {
        let carry = measured.carry.unwrap_or(f.carry_m);
        // With a measured total the roll is what the carry leaves of it, so
        // the carry, roll and total shown add up.
        let sim_roll = measured
            .total
            .map_or(shot.total_m - f.carry_m, |t| t - carry);
        if measured.roll.is_none() {
            d.roll_distance = Some(Distance::Meters(sim_roll));
        }
        if measured.total.is_none() {
            let roll = measured.roll.unwrap_or(sim_roll);
            let total = carry + roll;
            // As for the path: a total that is not positive is not shown.
            if total.is_finite() && total > 0.0 {
                d.total_distance = Some(Distance::Meters(total));
            }
        }
        // Without a path the raw simulation's rest is in another frame from
        // a measured landing, so neither offline is shown.
        let total_offline = match anchored.as_ref().and_then(|p| p.ground.last()) {
            Some(r) => Some(r[3]),
            None if measured.offline.is_none() => Some(shot.final_pos.z),
            None => None,
        };
        if let Some(total_offline) = total_offline {
            d.roll_offline = Some(Distance::Meters(total_offline - carry_offline));
            // A measured rest offline is the one shown.
            if measured.total_offline.is_none() {
                d.total_offline = Some(Distance::Meters(total_offline));
            }
        }
    }
    d.path = anchored.map(|p| p.decimated());

    // Guards a future optional-only field; every field set above is
    // unconditional or follows from `flight_ok`, so `d` is never empty today.
    (!d.is_empty()).then_some(d)
}

/// The `dimple` environment for `cfg`: its altitude and temperature over
/// `Environment::SEA_LEVEL`. The temperature default is the ball model's
/// 25 C calibration point (not sea level's 15 C), carried in the config
/// default.
pub(crate) fn environment(cfg: &SimulationSection) -> Environment {
    let mut env = Environment::SEA_LEVEL;
    env.altitude_m = cfg.altitude.as_meters();
    env.temperature_c = cfg.temperature_c;
    env
}

/// The anchored path when there is one and every point is finite; logs why
/// there is none otherwise.
fn finite_path(anchored: Result<FullPath, &'static str>) -> Option<FullPath> {
    match anchored {
        Ok(path) if path.is_finite() => Some(path),
        Ok(_) => {
            debug!("no simulated path: a point is not finite");
            None
        }
        Err(reason) => {
            debug!("no simulated path: {reason}");
            None
        }
    }
}

/// A ground ball's launch: ball speed (m/s) and direction (deg, positive =
/// right), or why it cannot be rolled. Speed must be finite and positive and
/// the direction finite and within 90 deg; launch angle and spin are not
/// used.
fn ground_input(ball: &Launch) -> Result<(f64, f64), &'static str> {
    let (Some(speed), Some(hla)) = (ball.launch_speed, ball.launch_azimuth) else {
        return Err("ground ball speed or direction missing");
    };
    let speed = speed.as_mps();
    if !(speed.is_finite() && speed > 0.0) {
        return Err("ground ball speed not finite and positive");
    }
    if !(hla.is_finite() && hla.abs() < 90.0) {
        return Err("ground ball direction outside -90 to 90 deg");
    }
    Ok((speed, hla))
}

/// Roll a ground ball and return its values, or `None` when the launch
/// can't be rolled.
///
/// The ball starts on the ground at the tee with the launch speed along the
/// launch direction and no spin, so it skids until it rolls and then rolls
/// to rest: dimple's ground model ([`GroundModel::settle`]) from that
/// state, on `surface`. Carry, apex and flight time are 0: each
/// is in `derived` unless the device measured exactly 0 for it (under
/// `force`, always). There is no descent angle. The path has a one-point
/// flight (the tee, where the ball "lands") and the roll as its ground
/// segment, fitted to a measured total or roll ([`anchor_ground_ball`]).
/// The displayed total is the measured total, else the measured roll, else
/// the simulated roll, and roll and total offlines are the rest's `z`.
fn derive_ground_ball(
    launch: &Launch,
    flight: Option<&FlightResult>,
    cfg: &SimulationSection,
    surface: Surface,
) -> Option<DerivedFlight> {
    let (speed, hla) = match ground_input(launch) {
        Ok(launch) => launch,
        Err(reason) => {
            debug!("not simulated: {reason}");
            return None;
        }
    };
    let az = hla.to_radians();
    let start = BallState {
        time: 0.0,
        pos: Vec3::ZERO,
        vel: Vec3::new(speed * az.cos(), 0.0, speed * az.sin()),
        spin: Vec3::ZERO,
    };
    let env = environment(cfg);
    let ground = surface.settle(&start, &BallModel::TOUR, env.air_density());
    let p = ground.rest.pos;
    let rest = ground.termination == Termination::Rest
        && p.x.is_finite()
        && p.y.is_finite()
        && p.z.is_finite();
    let raw = FullPath::new(vec![[0.0; 4]], &ground, rest);
    let force = cfg.mode == SimMode::Force;
    let measured = if force {
        Measured::default()
    } else {
        Measured::from_shot(launch, flight)
    };
    let anchored = finite_path(anchor_ground_ball(&raw, &measured));

    let mut d = DerivedFlight::default();
    d.replaces_measured = force;
    let measured_zero = |v: Option<f64>| v == Some(0.0);
    if !measured_zero(measured.carry) {
        d.carry_distance = Some(Distance::Meters(0.0));
    }
    if !measured_zero(measured.offline) {
        d.carry_offline = Some(Distance::Meters(0.0));
    }
    if !measured_zero(measured.apex) {
        d.max_height = Some(Distance::Meters(0.0));
    }
    if !measured_zero(measured.flight_time) {
        d.flight_time = Some(0.0);
    }
    if rest {
        let sim_roll = ground.rest.pos.x.hypot(ground.rest.pos.z);
        // The carry shown is 0, so the roll is the total: the device's
        // total, else its roll, else the simulated one. A device roll that
        // is not that total is replaced.
        let total = measured
            .total
            .unwrap_or_else(|| measured.roll.unwrap_or(sim_roll));
        if measured.roll != Some(total) {
            d.roll_distance = Some(Distance::Meters(total));
        }
        if measured.total.is_none() && total.is_finite() && total > 0.0 {
            d.total_distance = Some(Distance::Meters(total));
        }
        let total_offline = anchored
            .as_ref()
            .and_then(|p| p.ground.last())
            .map_or(ground.rest.pos.z, |r| r[3]);
        d.roll_offline = Some(Distance::Meters(total_offline));
        d.total_offline = Some(Distance::Meters(total_offline));
    }
    d.path = anchored.map(|p| p.decimated());
    Some(d)
}

/// A full-resolution path in the frame `sim` reads every scalar from, the
/// same as [`FlightPath`]: points are `[t, x, y, z]`, seconds since launch,
/// meters downrange, meters up, meters right of the target line (FRP's
/// positive = right, so a point's `z` has the sign of `carry_offline`),
/// origin at the tee.
#[derive(Debug, Clone, Default, PartialEq)]
struct FullPath {
    /// Tee to landing.
    flight: Vec<[f64; 4]>,
    /// Landing (first point equals `flight`'s last) to rest; empty when the
    /// ground phase did not reach rest.
    ground: Vec<[f64; 4]>,
    /// Indices into `ground` of the points where the ball meets the ground
    /// and a hop starts, in order.
    contacts: Vec<usize>,
}

impl FullPath {
    /// Whether every coordinate of every point is finite.
    fn is_finite(&self) -> bool {
        self.flight
            .iter()
            .chain(&self.ground)
            .flatten()
            .all(|v| v.is_finite())
    }

    /// The simulated path; the ground segment only when the ball reached
    /// rest.
    fn from_shot(shot: &ShotResult, rest: bool) -> Self {
        let flight = shot
            .flight
            .points
            .iter()
            .map(|p| [p.time, p.pos.x, p.pos.y, p.pos.z])
            .collect();
        Self::new(flight, &shot.ground, rest)
    }

    /// `flight` followed by `ground`'s points; the ground segment only when
    /// the ball reached rest.
    fn new(flight: Vec<[f64; 4]>, ground: &GroundResult, rest: bool) -> Self {
        if !rest {
            return Self {
                flight,
                ..Self::default()
            };
        }
        let points: Vec<[f64; 4]> = ground
            .points
            .iter()
            .map(|p| [p.time, p.pos.x, p.pos.y, p.pos.z])
            .collect();
        let mut contacts: Vec<usize> = ground
            .contacts
            .iter()
            .filter_map(|c| nearest_time(&points, c.time))
            .collect();
        contacts.dedup();
        Self {
            flight,
            ground: points,
            contacts,
        }
    }

    /// The path as sent: decimated and rounded to milliseconds and
    /// centimeters.
    fn decimated(&self) -> FlightPath {
        FlightPath {
            flight: round_points(&decimate_flight(&self.flight)),
            ground: round_points(&decimate_ground(&self.ground, &self.contacts)),
        }
    }
}

/// Index of the point whose time is closest to `t`.
fn nearest_time(points: &[[f64; 4]], t: f64) -> Option<usize> {
    let after = points.partition_point(|p| p[0] < t);
    let before = after.checked_sub(1);
    match (before, (after < points.len()).then_some(after)) {
        (Some(b), Some(a)) => Some(if t - points[b][0] <= points[a][0] - t {
            b
        } else {
            a
        }),
        (b, a) => b.or(a),
    }
}

/// The values the device reported that the path is fitted to, meters and
/// seconds: its flight result, and its launch direction.
#[derive(Debug, Clone, Copy, Default)]
struct Measured {
    carry: Option<f64>,
    /// Carry offline: landing point right (+) of the target line.
    offline: Option<f64>,
    /// Launch direction (deg, + = right): the start line a fit to a
    /// measured offline keeps.
    azimuth: Option<f64>,
    apex: Option<f64>,
    flight_time: Option<f64>,
    descent: Option<f64>,
    total: Option<f64>,
    roll: Option<f64>,
    /// Rest point right (+) of the target line.
    total_offline: Option<f64>,
}

impl Measured {
    fn from_shot(launch: &Launch, flight: Option<&FlightResult>) -> Self {
        let azimuth = launch.launch_azimuth;
        let Some(f) = flight else {
            return Self {
                azimuth,
                ..Self::default()
            };
        };
        Self {
            carry: f.carry_distance.map(Distance::as_meters),
            offline: f
                .carry_offline
                .map(Distance::as_meters)
                .filter(|v| v.is_finite()),
            azimuth,
            apex: f.max_height.map(Distance::as_meters),
            flight_time: f.flight_time,
            descent: f.descent_angle.filter(|v| v.is_finite()),
            total: f.total_distance.map(Distance::as_meters),
            roll: f.roll_distance.map(Distance::as_meters),
            total_offline: f
                .total_offline
                .map(Distance::as_meters)
                .filter(|v| v.is_finite()),
        }
    }

    /// For a shot that flies (not a ground ball): a carry or flight time of
    /// exactly 0 is not a reading, so it counts as not measured.
    fn without_zero_flight(self) -> Self {
        let reading = |v: Option<f64>| v.filter(|&v| v != 0.0);
        Self {
            carry: reading(self.carry),
            flight_time: reading(self.flight_time),
            ..self
        }
    }
}

/// Simulated roll (m) below which the ground segment is not stretched to a
/// measured roll: the ratio would be dominated by noise.
const MIN_FIT_ROLL_M: f64 = 0.3;
/// Range of measured-to-simulated roll ratios the ground segment is
/// stretched over; outside it the run-out is drawn straight.
const ROLL_RATIO_RANGE: std::ops::RangeInclusive<f64> = 0.25..=4.0;
/// Shortest time drawn for a straight run-out (s).
const MIN_STRAIGHT_RUN_S: f64 = 0.5;
/// A fit ratio further than this from 1 is logged.
const LARGE_FIT: f64 = 0.3;

/// Fit the simulated path to the values the device measured, so that it
/// lands at the displayed carry, peaks at the displayed apex, lands at the
/// displayed flight time and rests at the displayed total.
///
/// The displayed carry `C`, apex and flight time are the measured ones,
/// else the simulation's. The displayed total `T` is the measured total,
/// else `C` plus the measured roll, else `C` plus the simulated roll.
///
/// - Flight: `(t, x, y, z) -> (k_t*t, k*x, k_y*y, k*z)` with
///   `k = C / C_sim`, `k_y` and `k_t` the measured-to-simulated apex and
///   flight time (1 when not measured). The landing keeps its bearing from
///   the tee and lands at radius `C`; the curve keeps its shape.
/// - Offline: with a measured carry offline `O`, the path is then fitted to
///   land on it while leaving the tee on the measured launch direction
///   ([`fit_offline`]): the start line is kept and the curve, the lateral
///   deviation from it, is scaled.
/// - Ground: a polar fit about the tee. A point at radius `rho` keeps its
///   bearing and height and moves to `C + r*(rho - C_sim)`, with
///   `r = (T - C) / roll_sim` when the device measured total or roll and 1
///   otherwise; its time moves by the flight-time shift. The first point
///   lands on the fitted landing and the rest point at radius `T`.
/// - Straight run-out: when a measured roll is fitted and the simulated roll
///   is under [`MIN_FIT_ROLL_M`], runs the other way, or the ratio is
///   outside [`ROLL_RATIO_RANGE`], or any fitted radius is negative, the
///   ground segment is two points, the landing and the rest at radius `T` on
///   the landing's bearing.
///
/// With nothing measured the path is the simulation, unchanged. A measured
/// carry, apex, flight time or total that is not finite and positive, a
/// non-finite measured roll, a measured offline that is not inside the
/// carry (`|O| < C`) or comes without a launch direction, or a displayed
/// total that is not positive gives no path. A measured 0 apex never gets here (a ground ball,
/// [`derive_ground_ball`]), and [`derive`] treats a measured 0 carry or
/// flight time as not measured ([`Measured::without_zero_flight`]).
fn anchor_path(raw: &FullPath, m: &Measured) -> Result<FullPath, &'static str> {
    for (value, err) in [
        (m.carry, "measured carry not finite and positive"),
        (m.apex, "measured apex not finite and positive"),
        (
            m.flight_time,
            "measured flight time not finite and positive",
        ),
        (m.total, "measured total not finite and positive"),
    ] {
        if value.is_some_and(|v| !(v.is_finite() && v > 0.0)) {
            return Err(err);
        }
    }
    if m.roll.is_some_and(|v| !v.is_finite()) {
        return Err("measured roll not finite");
    }
    let Some(&landing) = raw.flight.last() else {
        return Err("empty flight");
    };
    let apex_idx = peak_index(&raw.flight);
    let carry_sim = landing[1].hypot(landing[3]);
    let time_sim = landing[0];
    let apex_sim = raw.flight[apex_idx][2];
    // A flight too short to sample its peak (tee and landing only) is fine
    // unless an apex is measured to fit it to.
    if !(carry_sim > 0.0 && time_sim > 0.0 && (apex_sim > 0.0 || m.apex.is_none())) {
        return Err("degenerate simulated flight");
    }

    let carry = m.carry.unwrap_or(carry_sim);
    let k = carry / carry_sim;
    let k_y = m.apex.map_or(1.0, |h| h / apex_sim);
    let k_t = m.flight_time.map_or(1.0, |t| t / time_sim);
    for (name, ratio) in [("carry", k), ("apex", k_y), ("flight time", k_t)] {
        if (ratio - 1.0).abs() > LARGE_FIT {
            debug!("sim path: measured {name} is {ratio:.2}x the simulation's");
        }
    }
    let mut flight: Vec<[f64; 4]> = raw
        .flight
        .iter()
        .map(|p| [k_t * p[0], k * p[1], k_y * p[2], k * p[3]])
        .collect();
    if let Some(h) = m.apex {
        flight[apex_idx][2] = h;
    }
    let last = flight.len() - 1;
    if let Some(t) = m.flight_time {
        flight[last][0] = t;
    }
    let fitted = flight[last];
    let (mut ground, contacts) = fit_ground(
        raw,
        m,
        &Landing {
            fitted,
            carry,
            carry_sim,
            moved: m.carry.is_some(),
            shift: fitted[0] - time_sim,
            bearing: landing[3].atan2(landing[1]),
        },
    )?;
    if let Some(offline) = m.offline {
        let Some(azimuth) = m.azimuth else {
            return Err("measured offline without a launch direction");
        };
        fit_offline(&mut flight, &mut ground, offline, azimuth)?;
    }
    Ok(FullPath {
        flight,
        ground,
        contacts,
    })
}

/// A simulated curve (m) below which [`fit_offline`] adds the missing
/// lateral instead of scaling the curve: the ratio would be dominated by
/// noise.
const MIN_FIT_CURVE_M: f64 = 0.3;
/// Largest curve scale [`fit_offline`] applies; beyond it the missing
/// lateral is added instead.
const MAX_CURVE_SCALE: f64 = 4.0;

/// Fit a flight landing at radius `C` (and its ground segment) to land at
/// the measured carry offline `O` while leaving the tee on the measured
/// launch direction `az` (deg).
///
/// The landing moves to `(X', O)` with `X' = sqrt(C² - O²)`, so it stays at
/// radius `C`. A point's downrange `x` scales by `X' / X` and its lateral
/// becomes `z' = x'·tan(az) + s·(z - x·tan(az))`: the start line is kept and
/// the deviation from it (the curve) is scaled by
/// `s = (O - X'·tan(az)) / (Z - X·tan(az))`, `(X, Z)` the landing before
/// the fit. When the simulated curve at the landing is under
/// [`MIN_FIT_CURVE_M`], or `s` is not finite or beyond
/// [`MAX_CURVE_SCALE`], the curve is kept and the missing lateral is added
/// as `(O - Z')·(x / X)²`, `Z'` the landing's lateral with the curve kept;
/// that leaves the tee on the same line. Either way the tee does not move.
///
/// The ground segment is turned about the landing by the change in the
/// flight's arrival heading, so the roll continues in the direction the
/// fitted flight arrives, and its run from the landing is then scaled so
/// the rest keeps its radius from the tee (the displayed total).
///
/// An offline that is not inside the carry (`|O| < C`), a landing that is
/// not downrange or a launch direction outside ±90° fails.
fn fit_offline(
    flight: &mut [[f64; 4]],
    ground: &mut [[f64; 4]],
    offline: f64,
    azimuth: f64,
) -> Result<(), &'static str> {
    let n = flight.len();
    let Some(&[_, x_l, _, z_l]) = flight.last() else {
        return Err("empty flight");
    };
    let carry = x_l.hypot(z_l);
    if !(offline.is_finite() && offline.abs() < carry) {
        return Err("measured offline not finite and inside the carry");
    }
    if !(azimuth.is_finite() && azimuth.abs() < 90.0) {
        return Err("launch direction outside -90 to 90 deg");
    }
    if x_l <= 0.0 {
        return Err("landing not downrange");
    }
    let x_fit = (carry * carry - offline * offline).sqrt();
    let q = x_fit / x_l;
    let tan = azimuth.to_radians().tan();
    let curve_sim = z_l - x_l * tan;
    let curve = offline - x_fit * tan;
    let scale = curve / curve_sim;
    let scaled =
        curve_sim.abs() >= MIN_FIT_CURVE_M && scale.is_finite() && scale.abs() <= MAX_CURVE_SCALE;
    if !scaled {
        debug!("sim path: offline fitted by adding the lateral (simulated curve {curve_sim:.2} m)");
    }
    let lateral = |x: f64, z: f64| {
        let deviation = z - x * tan;
        let fitted = if scaled {
            scale * deviation
        } else {
            deviation + (curve - curve_sim) * (x / x_l).powi(2)
        };
        q * x * tan + fitted
    };
    let heading = |a: [f64; 4], b: [f64; 4]| (b[3] - a[3]).atan2(b[1] - a[1]);
    let arrival_sim = (n >= 2).then(|| heading(flight[n - 2], flight[n - 1]));
    for p in flight.iter_mut() {
        p[3] = lateral(p[1], p[3]);
        p[1] *= q;
    }
    flight[n - 1][3] = offline;
    let turn = arrival_sim.map_or(0.0, |a| heading(flight[n - 2], flight[n - 1]) - a);

    let (Some(&first), Some(&rest)) = (ground.first(), ground.last()) else {
        return Ok(());
    };
    let (sin, cos) = turn.sin_cos();
    let run = |p: [f64; 4]| {
        let (dx, dz) = (p[1] - first[1], p[3] - first[3]);
        (dx * cos - dz * sin, dx * sin + dz * cos)
    };
    // The run scale `u` that keeps the rest at its radius `T`:
    // |L + u·w| = T, the root nearest 1.
    let total = rest[1].hypot(rest[3]);
    let (wx, wz) = run(rest);
    let (a, b, c) = (
        wx * wx + wz * wz,
        x_fit * wx + offline * wz,
        carry * carry - total * total,
    );
    let disc = b * b - a * c;
    let u = if a > 0.0 && disc >= 0.0 {
        let root = disc.sqrt();
        let (u1, u2) = ((-b + root) / a, (-b - root) / a);
        if (u1 - 1.0).abs() <= (u2 - 1.0).abs() {
            u1
        } else {
            u2
        }
    } else {
        1.0
    };
    for p in ground.iter_mut() {
        let (dx, dz) = run(*p);
        *p = [p[0], x_fit + u * dx, p[2], offline + u * dz];
    }
    ground[0] = flight[n - 1];
    Ok(())
}

/// Fit a ground ball's roll to the measured values: the path's flight is
/// the tee alone (`[0, 0, 0, 0]`, where the ball "lands"), and the ground
/// segment is the simulated roll from there, fitted to the measured total,
/// else the measured roll, as [`anchor_path`] fits the ground after a
/// landing at radius 0. A straight run-out keeps the simulated rest's bearing. A
/// measured total that is not finite and positive, a non-finite measured
/// roll or a displayed total that is not positive gives no path.
fn anchor_ground_ball(raw: &FullPath, m: &Measured) -> Result<FullPath, &'static str> {
    if m.total.is_some_and(|v| !(v.is_finite() && v > 0.0)) {
        return Err("measured total not finite and positive");
    }
    if m.roll.is_some_and(|v| !v.is_finite()) {
        return Err("measured roll not finite");
    }
    let tee = [0.0; 4];
    let bearing = raw.ground.last().map_or(0.0, |r| r[3].atan2(r[1]));
    let (ground, contacts) = fit_ground(
        raw,
        m,
        &Landing {
            fitted: tee,
            carry: 0.0,
            carry_sim: 0.0,
            moved: false,
            shift: 0.0,
            bearing,
        },
    )?;
    Ok(FullPath {
        flight: vec![tee],
        ground,
        contacts,
    })
}

/// Where a fitted ground segment starts.
struct Landing {
    /// The fitted landing: the ground segment's first point.
    fitted: [f64; 4],
    /// Displayed carry `C`: the fitted landing's radius from the tee.
    carry: f64,
    /// Simulated carry `C_sim`: the simulated landing's radius.
    carry_sim: f64,
    /// Whether the landing moved (a measured carry).
    moved: bool,
    /// Time shift of the ground segment (s): fitted minus simulated
    /// landing time.
    shift: f64,
    /// Bearing (rad) of a straight run-out.
    bearing: f64,
}

/// The ground segment of `raw` fitted after `landing` (see [`anchor_path`])
/// and its contact indices; both empty when `raw` has no ground segment.
fn fit_ground(
    raw: &FullPath,
    m: &Measured,
    landing: &Landing,
) -> Result<(Vec<[f64; 4]>, Vec<usize>), &'static str> {
    let (Some(&first), Some(&rest)) = (raw.ground.first(), raw.ground.last()) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let Landing {
        fitted: landing_fit,
        carry,
        carry_sim,
        shift: dt,
        ..
    } = *landing;
    let flight_time = landing_fit[0];
    let roll_sim = rest[1].hypot(rest[3]) - carry_sim;
    let fit_roll = m.total.is_some() || m.roll.is_some();
    let total = m.total.unwrap_or(carry + m.roll.unwrap_or(roll_sim));
    if !(total.is_finite() && total > 0.0) {
        return Err("displayed total not positive");
    }
    let roll = total - carry;
    // Read only when the roll is fitted to a simulated roll big enough.
    let r = if fit_roll && roll_sim.abs() >= MIN_FIT_ROLL_M {
        roll / roll_sim
    } else {
        1.0
    };

    let mut straight = if !fit_roll {
        None
    } else if roll_sim.abs() < MIN_FIT_ROLL_M {
        Some("simulated roll near zero")
    } else if roll.signum() != roll_sim.signum() {
        Some("measured roll runs the other way")
    } else if !ROLL_RATIO_RANGE.contains(&r) {
        Some("roll ratio out of range")
    } else {
        None
    };
    let mut ground = Vec::new();
    let mut contacts = Vec::new();
    if straight.is_none() {
        if !landing.moved && !fit_roll {
            ground = raw
                .ground
                .iter()
                .map(|p| [p[0] + dt, p[1], p[2], p[3]])
                .collect();
        } else {
            for p in &raw.ground {
                let rho = carry + r * (p[1].hypot(p[3]) - carry_sim);
                if rho < 0.0 {
                    straight = Some("fitted radius negative");
                    break;
                }
                let bearing = p[3].atan2(p[1]);
                ground.push([p[0] + dt, rho * bearing.cos(), p[2], rho * bearing.sin()]);
            }
        }
        contacts.clone_from(&raw.contacts);
    }
    if let Some(reason) = straight {
        debug!("sim path: straight run-out ({reason})");
        let bearing = landing.bearing;
        let duration = (rest[0] - first[0]).max(MIN_STRAIGHT_RUN_S);
        ground = vec![
            landing_fit,
            [
                flight_time + duration,
                total * bearing.cos(),
                0.0,
                total * bearing.sin(),
            ],
        ];
        contacts.clear();
    }
    ground[0] = landing_fit;
    Ok((ground, contacts))
}

/// Index of the highest point (the first one on a tie).
fn peak_index(points: &[[f64; 4]]) -> usize {
    points.iter().enumerate().fold(
        0,
        |best, (i, p)| if p[2] > points[best][2] { i } else { best },
    )
}

/// RDP tolerance for the decimated path (m).
const PATH_TOLERANCE_M: f64 = 0.25;
/// Most points kept in the decimated flight segment.
const MAX_FLIGHT_POINTS: usize = 40;
/// Most points kept in the decimated ground segment.
const MAX_GROUND_POINTS: usize = 24;
/// A hop rising more than this (m) keeps its contact and its peak.
const MIN_KEPT_HOP_M: f64 = 0.05;

/// The flight segment reduced to a few dozen points: the tee, the apex
/// sample and the landing are always kept, the rest by Ramer-Douglas-Peucker
/// at [`PATH_TOLERANCE_M`]. Above [`MAX_FLIGHT_POINTS`] it is resampled
/// uniformly in time and the apex put back in place of its nearest sample.
fn decimate_flight(points: &[[f64; 4]]) -> Vec<[f64; 4]> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let apex = peak_index(points);
    let mut keep = vec![0, apex, points.len() - 1];
    keep.dedup();
    let kept = rdp(points, &keep, PATH_TOLERANCE_M);
    if kept.len() <= MAX_FLIGHT_POINTS {
        return kept.iter().map(|&i| points[i]).collect();
    }
    let mut out = resample(points, MAX_FLIGHT_POINTS);
    reinsert(&mut out, points[apex]);
    out
}

/// The ground segment reduced the same way: the landing, the rest, and the
/// contact and peak of every hop rising more than [`MIN_KEPT_HOP_M`] are
/// kept, the rest by RDP. Above [`MAX_GROUND_POINTS`] it is resampled
/// uniformly in time between the landing and the rest.
fn decimate_ground(points: &[[f64; 4]], contacts: &[usize]) -> Vec<[f64; 4]> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let last = points.len() - 1;
    let mut keep = vec![0, last];
    for (j, &start) in contacts.iter().enumerate() {
        let end = contacts.get(j + 1).copied().unwrap_or(last).min(last);
        if start >= end {
            continue;
        }
        let peak = start + peak_index(&points[start..=end]);
        if points[peak][2] - points[start][2] > MIN_KEPT_HOP_M {
            keep.extend([start, peak]);
        }
    }
    keep.sort_unstable();
    keep.dedup();
    let kept = rdp(points, &keep, PATH_TOLERANCE_M);
    if kept.len() <= MAX_GROUND_POINTS {
        return kept.iter().map(|&i| points[i]).collect();
    }
    resample(points, MAX_GROUND_POINTS)
}

/// Ramer-Douglas-Peucker on `(x, y, z)` between each pair of consecutive
/// `keep` indices (sorted, first and last included). Returns the kept
/// indices in order.
fn rdp(points: &[[f64; 4]], keep: &[usize], tolerance: f64) -> Vec<usize> {
    let mut out = vec![keep[0]];
    for pair in keep.windows(2) {
        let mut stack = vec![(pair[0], pair[1])];
        let mut inner = Vec::new();
        while let Some((a, b)) = stack.pop() {
            let far = (a + 1..b)
                .map(|i| (i, segment_distance(points[i], points[a], points[b])))
                .fold(None, |best: Option<(usize, f64)>, (i, d)| match best {
                    Some((_, bd)) if bd >= d => best,
                    _ => Some((i, d)),
                });
            if let Some((i, d)) = far
                && d > tolerance
            {
                inner.push(i);
                stack.push((a, i));
                stack.push((i, b));
            }
        }
        inner.sort_unstable();
        out.extend(inner);
        out.push(pair[1]);
    }
    out
}

/// Distance from `p` to the segment `a`-`b` in `(x, y, z)`.
fn segment_distance(p: [f64; 4], a: [f64; 4], b: [f64; 4]) -> f64 {
    let ab = [b[1] - a[1], b[2] - a[2], b[3] - a[3]];
    let ap = [p[1] - a[1], p[2] - a[2], p[3] - a[3]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    let s = if len2 > 0.0 {
        ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let d = [ap[0] - s * ab[0], ap[1] - s * ab[1], ap[2] - s * ab[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// `n` (at least 2) points uniformly spaced in time from the first point to
/// the last, linearly interpolated; the end points are kept exactly.
fn resample(points: &[[f64; 4]], n: usize) -> Vec<[f64; 4]> {
    let (first, last) = (points[0], points[points.len() - 1]);
    let mut out = Vec::with_capacity(n);
    out.push(first);
    #[allow(clippy::cast_precision_loss)]
    let step = (last[0] - first[0]) / (n - 1) as f64;
    for i in 1..n - 1 {
        #[allow(clippy::cast_precision_loss)]
        let t = first[0] + step * i as f64;
        let b = points
            .partition_point(|p| p[0] <= t)
            .clamp(1, points.len() - 1);
        let (p, q) = (points[b - 1], points[b]);
        let span = q[0] - p[0];
        let s = if span > 0.0 { (t - p[0]) / span } else { 1.0 };
        out.push([
            t,
            p[1] + s * (q[1] - p[1]),
            p[2] + s * (q[2] - p[2]),
            p[3] + s * (q[3] - p[3]),
        ]);
    }
    out.push(last);
    out
}

/// Put `point` back in place of the interior sample nearest it in time.
fn reinsert(out: &mut [[f64; 4]], point: [f64; 4]) {
    if out.len() < 3 {
        return;
    }
    let last = out.len() - 1;
    let i = nearest_time(out, point[0]).unwrap_or(1).clamp(1, last - 1);
    out[i] = point;
}

/// Round to milliseconds and centimeters, dropping a point whose rounded
/// time does not advance (the last point is kept in place of the one before
/// it).
fn round_points(points: &[[f64; 4]]) -> Vec<[f64; 4]> {
    // `+ 0.0` turns a rounded -0.0 into 0.0.
    let round = |v: f64, scale: f64| (v * scale).round() / scale + 0.0;
    let mut out: Vec<[f64; 4]> = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let q = [
            round(p[0], 1000.0),
            round(p[1], 100.0),
            round(p[2], 100.0),
            round(p[3], 100.0),
        ];
        if out.last().is_some_and(|prev| q[0] <= prev[0]) {
            if i + 1 < points.len() || out.len() < 2 {
                continue;
            }
            out.pop();
        }
        out.push(q);
    }
    out
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
    if d.replaces_measured {
        push("replaces_measured".into());
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

/// The flight model's name on its `shot.flight`: flighthook's and dimple's
/// versions.
pub(crate) fn model_name() -> String {
    format!(
        "flighthook {} (dimple {})",
        env!("CARGO_PKG_VERSION"),
        env!("DIMPLE_VERSION")
    )
}

/// What the flight model computed for one shot: its flight result (only
/// the fields it computed; `None` when there is none to add) and the path.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Modeled {
    pub flight: Option<FlightResult>,
    pub path: Option<FlightPath>,
    /// Every value as the simulation gave it, for the log line.
    pub derived: DerivedFlight,
}

/// Fly `launch` (the shot's launch for a flight model: measured values plus
/// the launch monitor's own assumed ones) under `cfg`, anchored on the
/// launch monitor's own flight `device`: the fields [`derive_with_error`]
/// chose (under `on` the ones `device` lacks and the ones the model wins
/// over a measured value; every result field under `force`),
/// with a ground ball rolled on the surface for `mode` (the mode at the
/// shot's trigger). `None` when the shot can't be flown or the environment
/// is invalid. `name` is the flight result's `model`.
pub(crate) fn model(
    cfg: &SimulationSection,
    launch: &Launch,
    device: Option<&FlightResult>,
    mode: Option<Mode>,
    name: &str,
) -> Option<Modeled> {
    let env_err = cfg.environment_error();
    warn_invalid_environment(cfg, env_err.as_deref());
    let derived = derive_with_error(launch, device, cfg, mode, env_err.as_deref())?;
    let flight = FlightResult {
        model: Some(name.to_owned()),
        carry_distance: derived.carry_distance,
        carry_offline: derived.carry_offline,
        total_distance: derived.total_distance,
        total_offline: derived.total_offline,
        roll_distance: derived.roll_distance,
        max_height: derived.max_height,
        flight_time: derived.flight_time,
        descent_angle: derived.descent_angle,
        ..FlightResult::new(Basis::Modeled)
    };
    let empty = FlightResult {
        model: Some(name.to_owned()),
        ..FlightResult::new(Basis::Modeled)
    };
    Some(Modeled {
        flight: (flight != empty).then_some(flight),
        path: derived.path.clone(),
        derived,
    })
}

#[cfg(test)]
mod tests;
