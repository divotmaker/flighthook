//! Simulated flight and roll (beta).
//!
//! Fills in the values a launch monitor does not report (carry, apex, flight
//! time, descent angle, offline, roll, total) by simulating the measured
//! launch with the `dimple` ball-flight model. `[simulation] mode` picks
//! what is simulated: `off` simulates nothing; `on` (the default) lets
//! measured values win, so [`derive`] only emits a field when the device left
//! it empty; `force` makes [`derive`] emit every result field and flag the
//! block `replaces_measured`, so the simulated results take precedence over
//! the device's. Either way the device's `BallFlight` is never modified, and
//! GSPro never sees these values.
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
//! Device actors call [`ball_flight_event`] to build every `BallFlight` bus
//! event, so the event leaves the actor already carrying `derived`.

use std::sync::Mutex;

use dimple::aero::BallModel;
use dimple::environment::Environment;
use dimple::ground::{BallState, GroundModel, GroundResult, Surface, Termination};
use dimple::stats::ShotStats;
use dimple::trajectory::{ShotInput, ShotResult, simulate_shot};
use flighthook::{
    BallFlight, DerivedFlight, Distance, FlightPath, FlighthookEvent, ShotDetectionMode, ShotKey,
    SimMode, SimSurface, SimulationSection,
};
use glam::DVec3;
use tracing::{debug, info, warn};

use crate::state::SystemState;

/// A measured apex within this of 0 (m) makes a ground ball.
const GROUND_MAX_APEX_M: f64 = 0.01;
/// Without a measured apex, a launch angle at or below this (deg) makes a
/// ground ball: too little lift to fly.
const GROUND_MAX_LAUNCH_DEG: f64 = 0.5;

/// The last invalid environment warned about, so the warning is logged once
/// per distinct invalid setting rather than on every shot.
static WARNED_ENVIRONMENT: Mutex<Option<String>> = Mutex::new(None);

/// Whether shots should be simulated at all: the mode is not `off`.
pub(crate) fn should_simulate(cfg: &SimulationSection) -> bool {
    cfg.mode != SimMode::Off
}

/// Whether a shot is a ground ball, rolled rather than flown: its measured
/// apex is 0 (finite, within [`GROUND_MAX_APEX_M`]), or it has no measured
/// apex and a launch angle above -90 and at most [`GROUND_MAX_LAUNCH_DEG`].
/// The device, club and detection mode play no part.
fn is_ground_ball(ball: &BallFlight) -> bool {
    match ball.max_height.map(Distance::as_meters) {
        Some(apex) => apex.is_finite() && apex.abs() <= GROUND_MAX_APEX_M,
        None => ball
            .launch_elevation
            .is_some_and(|vla| vla > -90.0 && vla <= GROUND_MAX_LAUNCH_DEG),
    }
}

/// The dimple surface preset for a configured surface.
fn surface(s: SimSurface) -> Surface {
    match s {
        SimSurface::Fairway => Surface::FAIRWAY,
        SimSurface::Green => Surface::GREEN,
        SimSurface::FirmGreen => Surface::FIRM_GREEN,
    }
}

/// The surface a ground ball rolls on: in Putting mode a green (the
/// configured one when it is a green, else [`Surface::GREEN`]); in any other
/// mode, or with no mode, the configured surface.
fn ground_surface(configured: SimSurface, mode: Option<ShotDetectionMode>) -> Surface {
    match (mode, configured) {
        (Some(ShotDetectionMode::Putting), SimSurface::Fairway) => Surface::GREEN,
        _ => surface(configured),
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

/// Simulate `ball` under `cfg` and return the values the device did not
/// report, or `None` when the launch or environment can't be simulated.
///
/// Where the device reports carry (or roll), the simulated total is that
/// measured carry plus the simulated roll, so carry + roll = total holds for
/// the values a consumer displays. The path is simulated, fitted to the
/// measured values ([`anchor_path`]), decimated and rounded to centimetres
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
/// the environment error once per shot in [`ball_flight_event_in_mode`] and
/// calls [`derive_with_error`] directly, since it already needs that error
/// for the log-once warning.
#[cfg(test)]
pub(crate) fn derive(ball: &BallFlight, cfg: &SimulationSection) -> Option<DerivedFlight> {
    derive_with_error(ball, cfg, None, cfg.environment_error().as_deref())
}

/// Like [`derive`], for a shot taken in detection mode `mode`, taking the
/// environment error already computed by the caller instead of recomputing
/// it. The mode only picks a ground ball's surface ([`ground_surface`]).
fn derive_with_error(
    ball: &BallFlight,
    cfg: &SimulationSection,
    mode: Option<ShotDetectionMode>,
    env_err: Option<&str>,
) -> Option<DerivedFlight> {
    if env_err.is_some() {
        return None;
    }
    if is_ground_ball(ball) {
        return derive_ground_ball(ball, cfg, ground_surface(cfg.surface, mode));
    }
    let input = match shot_input(ball) {
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
        Measured::from_ball(ball).without_zero_flight()
    };
    let anchored = finite_path(anchor_path(&raw, &measured));

    let mut d = DerivedFlight::default();
    d.replaces_measured = force;
    if measured.carry.is_none() {
        d.carry_distance = Some(Distance::Meters(f.carry_m));
    }
    // Offlines are read from the anchored path, so they sit at the displayed
    // landing and rest; without a path they are the raw simulation's.
    let carry_offline = anchored
        .as_ref()
        .and_then(|p| p.flight.last())
        .map_or(f.lateral_m, |l| l[3]);
    d.carry_offline = Some(Distance::Meters(carry_offline));
    if measured.apex.is_none() {
        d.max_height = Some(Distance::Meters(f.apex_m));
    }
    if measured.flight_time.is_none() {
        d.flight_time = Some(f.flight_time);
    }
    d.descent_angle = Some(descent);

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
        let total_offline = anchored
            .as_ref()
            .and_then(|p| p.ground.last())
            .map_or(shot.final_pos.z, |r| r[3]);
        d.roll_offline = Some(Distance::Meters(total_offline - carry_offline));
        d.total_offline = Some(Distance::Meters(total_offline));
    }
    d.path = anchored.map(|p| p.decimated());

    // Guards a future optional-only field; every field set above is
    // unconditional or follows from `flight_ok`, so `d` is never empty today.
    (!d.is_empty()).then_some(d)
}

/// The `dimple` environment for `cfg`. Not `Environment::SEA_LEVEL` (15 C):
/// the default is the ball model's 25 C calibration point, carried in the
/// config default.
fn environment(cfg: &SimulationSection) -> Environment {
    Environment {
        altitude_m: cfg.altitude.as_meters(),
        temperature_c: cfg.temperature_c,
    }
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
fn ground_input(ball: &BallFlight) -> Result<(f64, f64), &'static str> {
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
    ball: &BallFlight,
    cfg: &SimulationSection,
    surface: Surface,
) -> Option<DerivedFlight> {
    let (speed, hla) = match ground_input(ball) {
        Ok(launch) => launch,
        Err(reason) => {
            debug!("not simulated: {reason}");
            return None;
        }
    };
    let az = hla.to_radians();
    let start = BallState {
        time: 0.0,
        pos: DVec3::ZERO,
        vel: DVec3::new(speed * az.cos(), 0.0, speed * az.sin()),
        spin: DVec3::ZERO,
    };
    let env = environment(cfg);
    let ground = surface.settle(&start, &BallModel::TOUR, env.air_density());
    let rest = ground.termination == Termination::Rest && ground.rest.pos.is_finite();
    let raw = FullPath::new(vec![[0.0; 4]], &ground, rest);
    let force = cfg.mode == SimMode::Force;
    let measured = if force {
        Measured::default()
    } else {
        Measured::from_ball(ball)
    };
    let anchored = finite_path(anchor_ground_ball(&raw, &measured));

    let mut d = DerivedFlight::default();
    d.replaces_measured = force;
    let measured_zero = |v: Option<f64>| v == Some(0.0);
    if !measured_zero(measured.carry) {
        d.carry_distance = Some(Distance::Meters(0.0));
    }
    d.carry_offline = Some(Distance::Meters(0.0));
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
/// metres downrange, metres up, metres right of the target line (FRP's
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
    /// centimetres.
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

/// The values the device measured that the path is fitted to, metres and
/// seconds.
#[derive(Debug, Clone, Copy, Default)]
struct Measured {
    carry: Option<f64>,
    apex: Option<f64>,
    flight_time: Option<f64>,
    total: Option<f64>,
    roll: Option<f64>,
}

impl Measured {
    fn from_ball(ball: &BallFlight) -> Self {
        Self {
            carry: ball.carry_distance.map(Distance::as_meters),
            apex: ball.max_height.map(Distance::as_meters),
            flight_time: ball.flight_time,
            total: ball.total_distance.map(Distance::as_meters),
            roll: ball.roll_distance.map(Distance::as_meters),
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
/// non-finite measured roll or a displayed total that is not positive
/// gives no path. A measured 0 apex never gets here (a ground ball,
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
    let (ground, contacts) = fit_ground(
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
    Ok(FullPath {
        flight,
        ground,
        contacts,
    })
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

/// Round to milliseconds and centimetres, dropping a point whose rounded
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

/// Build the `BallFlight` bus event for a measured ball taken in the
/// current detection mode (the live game mode); see
/// [`ball_flight_event_in_mode`].
pub(crate) fn ball_flight_event(
    state: &SystemState,
    key: ShotKey,
    ball: BallFlight,
) -> FlighthookEvent {
    let mode = state.game.snapshot().mode;
    ball_flight_event_in_mode(state, key, ball, mode)
}

/// Build the `BallFlight` bus event for a measured ball taken in detection
/// mode `mode`: reads the live `[simulation]` settings, simulates the
/// missing values (every result value under `force`; a roll for a ground
/// ball, on the green in Putting mode), and logs the `sim` line under the
/// device's own ball line. For a device that captures the mode at its
/// trigger, `mode` is that capture.
pub(crate) fn ball_flight_event_in_mode(
    state: &SystemState,
    key: ShotKey,
    ball: BallFlight,
    mode: Option<ShotDetectionMode>,
) -> FlighthookEvent {
    let cfg = state.system.simulation();
    let derived = if should_simulate(&cfg) {
        let env_err = cfg.environment_error();
        warn_invalid_environment(&cfg, env_err.as_deref());
        derive_with_error(&ball, &cfg, mode, env_err.as_deref())
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
    use flighthook::{Club, Velocity};

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

    fn fully_measured() -> BallFlight {
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
        let ev = ball_flight_event(&t.state, key(), fully_measured());
        assert!(matches!(
            ev,
            FlighthookEvent::BallFlight { derived: None, .. }
        ));
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
    fn replacing_event_keeps_the_measured_ball() {
        let t = state_with(forced());
        let b = fully_measured();
        let ev = ball_flight_event(&t.state, key(), b.clone());
        let FlighthookEvent::BallFlight {
            ball: out, derived, ..
        } = &ev
        else {
            panic!("not a ball flight: {ev:?}");
        };
        assert_eq!(**out, b);
        assert!(derived.as_ref().is_some_and(|d| d.replaces_measured));
        let json = serde_json::to_value(&ev).expect("serializes");
        assert_eq!(json["derived"]["replaces_measured"], true, "{json}");
        assert_eq!(
            json["ball"],
            serde_json::to_value(&b).expect("serializes"),
            "{json}"
        );
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
        t.state.system.update(|c| c.simulation.mode = SimMode::Off);
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
        let with_apex = |apex: f64, vla: f64| BallFlight {
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
    }

    #[test]
    fn putting_mode_flies_a_shot_with_an_apex() {
        let t = state_with(cfg());
        let mut b = straight_shot();
        b.max_height = Some(Distance::Meters(10.0));
        t.writer.set_mode(ShotDetectionMode::Full);
        let full = event_derived(&ball_flight_event(&t.state, key(), b.clone()));
        t.writer.set_mode(ShotDetectionMode::Putting);
        t.writer
            .set_club_info(flighthook::ClubInfo { club: Club::Putter });
        let putting = event_derived(&ball_flight_event(&t.state, key(), b.clone()));
        assert_eq!(putting, full);
        assert!(m(putting.carry_distance) > 50.0);
        assert!(putting.descent_angle.is_some());
        assert!(putting.path.as_ref().expect("path").flight.len() > 2);
        assert_scalars_on_the_path(&b, &putting);
    }

    /// The `derived` block of a `BallFlight` event.
    fn event_derived(ev: &FlighthookEvent) -> DerivedFlight {
        let FlighthookEvent::BallFlight { derived, .. } = ev else {
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

        all.replaces_measured = true;
        assert!(log_line(&all).ends_with(" total_offline=-0.4yd replaces_measured"));
    }

    // -- 11. Path anchoring -----------------------------------------------------

    /// The raw simulated path for `b` under the default settings.
    fn raw_path(b: &BallFlight) -> FullPath {
        let c = cfg();
        let env = Environment {
            altitude_m: c.altitude.as_meters(),
            temperature_c: c.temperature_c,
        };
        let input = shot_input(b).expect("simulatable");
        let shot = simulate_shot(&input, &BallModel::TOUR, &env, &surface(c.surface));
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

    fn straight_shot() -> BallFlight {
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
            let fit = anchor_path(&raw, &Measured::from_ball(&b)).expect("path");
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
        let fit = anchor_path(&raw, &Measured::from_ball(&b)).expect("path");
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
        let fit = anchor_path(&raw, &Measured::from_ball(&b)).expect("path");
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
        let fit = anchor_path(&raw, &Measured::from_ball(&b)).expect("path");
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
        let mut b = ball(100.0, 30.0, 0.0, 11_000, 0);
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
        for (roll, fitted) in [(25.0, false), (1.0, false), (20.0, true), (1.25, true)] {
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
    fn assert_scalars_on_the_path(b: &BallFlight, d: &DerivedFlight) {
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
    fn mevo_like() -> BallFlight {
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
    fn bladed(speed_mps: f64, hla: f64) -> BallFlight {
        BallFlight {
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
        }
    }

    /// A ground ball's path: the tee alone as the flight, and a roll from
    /// the tee to the total on the launch bearing.
    fn assert_rolls_from_the_tee(b: &BallFlight, d: &DerivedFlight) {
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
        t.writer.set_mode(ShotDetectionMode::Full);
        t.writer
            .set_club_info(flighthook::ClubInfo { club: Club::Iron7 });
        let ev = ball_flight_event(&t.state, key(), b.clone());
        let FlighthookEvent::BallFlight { ball: out, .. } = &ev else {
            panic!("not a ball flight");
        };
        assert_eq!(**out, b);
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
            &BallFlight {
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
        for mode in [
            ShotDetectionMode::Full,
            ShotDetectionMode::Chipping,
            ShotDetectionMode::Putting,
        ] {
            let t = state_with(cfg());
            t.writer.set_mode(mode);
            let b = bladed(3.0, 1.5);
            let ev = ball_flight_event(&t.state, key(), b.clone());
            let FlighthookEvent::BallFlight { ball: out, .. } = &ev else {
                panic!("not a ball flight");
            };
            assert_eq!(**out, b);
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
    fn roll_in_mode(
        sim: SimulationSection,
        live: Option<ShotDetectionMode>,
        mode: Option<ShotDetectionMode>,
    ) -> f64 {
        let t = state_with(sim);
        if let Some(live) = live {
            t.writer.set_mode(live);
        }
        let b = bladed(3.0, 0.0);
        let ev = ball_flight_event_in_mode(&t.state, key(), b.clone(), mode);
        let d = event_derived(&ev);
        assert_rolls_from_the_tee(&b, &d);
        m(d.total_distance)
    }

    /// The rolled total of a straight 3 m/s ground ball under `sim` with
    /// the live game mode `live` (m).
    fn roll_live(sim: SimulationSection, live: Option<ShotDetectionMode>) -> f64 {
        let t = state_with(sim);
        if let Some(live) = live {
            t.writer.set_mode(live);
        }
        let b = bladed(3.0, 0.0);
        let d = event_derived(&ball_flight_event(&t.state, key(), b.clone()));
        assert_rolls_from_the_tee(&b, &d);
        m(d.total_distance)
    }

    #[test]
    fn putting_mode_rolls_a_ground_ball_on_the_green() {
        let putting = roll_live(cfg(), Some(ShotDetectionMode::Putting));
        assert!((putting - 5.03).abs() < 0.01, "{putting}");
        assert_eq!(putting, roll_on(3.0, SimSurface::Green));
        // Any other mode, or none yet, rolls on the configured fairway.
        for live in [
            Some(ShotDetectionMode::Full),
            Some(ShotDetectionMode::Chipping),
            None,
        ] {
            let fairway = roll_live(cfg(), live);
            assert!((fairway - 2.61).abs() < 0.01, "{live:?} {fairway}");
            assert_eq!(fairway, roll_on(3.0, SimSurface::Fairway), "{live:?}");
        }
    }

    #[test]
    fn the_mode_at_trigger_picks_the_surface_over_the_live_mode() {
        let green = roll_on(3.0, SimSurface::Green);
        let fairway = roll_on(3.0, SimSurface::Fairway);
        // Triggered in Putting, switched to Full before the result arrived.
        let putt = roll_in_mode(
            cfg(),
            Some(ShotDetectionMode::Full),
            Some(ShotDetectionMode::Putting),
        );
        assert_eq!(putt, green);
        // Triggered in Full, switched to Putting before the result arrived.
        let full = roll_in_mode(
            cfg(),
            Some(ShotDetectionMode::Putting),
            Some(ShotDetectionMode::Full),
        );
        assert_eq!(full, fairway);
    }

    #[test]
    fn a_configured_green_is_kept_in_putting_mode() {
        for surface in [SimSurface::Green, SimSurface::FirmGreen] {
            let sim = SimulationSection { surface, ..cfg() };
            let putting = roll_live(sim.clone(), Some(ShotDetectionMode::Putting));
            assert_eq!(putting, roll_on(3.0, surface), "{surface:?}");
            assert_eq!(putting, roll_live(sim, Some(ShotDetectionMode::Full)));
        }
        let firm = roll_on(3.0, SimSurface::FirmGreen);
        assert!((firm - 5.37).abs() < 0.01, "{firm}");
    }

    #[test]
    fn ground_balls_need_only_speed_and_direction() {
        let mut b = bladed(3.0, 0.0);
        b.launch_elevation = None;
        b.backspin_rpm = None;
        assert!(derive(&b, &cfg()).is_some());
        for bad in [
            BallFlight {
                launch_speed: None,
                ..bladed(3.0, 0.0)
            },
            BallFlight {
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
}
