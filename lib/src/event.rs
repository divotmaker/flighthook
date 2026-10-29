//! Shared domain types used across the unified bus, the actors, and the UI.
//!
//! Shot data types (`BallFlight`, `ClubData`, `FaceImpact`) are re-exported
//! from the `flightrelay` crate. This module defines the composed `ShotData`
//! and accumulator types built on top of them.

use serde::{Deserialize, Serialize};

use flightrelay::types::{BallFlight, ClubData, FaceImpact};
use flightrelay::units::{Distance, Velocity};

use crate::{Club, Handedness, ShotKey, ShotMetrics, ShotRaw, UnitSystem};

// ---------------------------------------------------------------------------
// Derived (simulated) flight values
// ---------------------------------------------------------------------------

/// Values flighthook simulated from the measured launch (beta).
///
/// By default present only for fields the device did not report: a value
/// under [`BallFlight`] is measured and a value here is simulated. A field
/// set in both is shown with the value here: on a shot that flies, a
/// measured carry or flight time of exactly 0 is not a reading, so it is
/// simulated next to the untouched 0. A ground ball (a measured apex of 0,
/// or no measured apex and a launch angle of at most 0.5 deg) is rolled,
/// not flown: carry, apex and flight time are 0 (here when the device
/// measured something else) and there is no descent angle. Lateral
/// values (`*_offline`) are absolute: positive = right of the target line,
/// as in FRP's `launch_azimuth`.
///
/// Where the device reports carry, `total_distance` is that measured carry
/// plus the simulated roll; where it reports carry and total but not roll,
/// `roll_distance` is the total minus the carry. A ground ball's roll is its
/// total (its carry is 0), replacing a device roll that differs. So the
/// carry + roll = total shown holds.
///
/// When [`replaces_measured`](Self::replaces_measured) is set (the
/// `[simulation] mode = "force"` setting), every result field is the
/// simulation's, whether or not the device measured it, and these values
/// take precedence over the measured ones in [`BallFlight`], which are kept
/// unchanged.
///
/// [`spin_estimated`](Self::spin_estimated) marks a spin in [`BallFlight`]
/// that flighthook estimated rather than the device measured.
///
/// Non-exhaustive so fields can be added; build one with
/// `DerivedFlight::default()` and assign the fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DerivedFlight {
    /// Tee to landing, horizontal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carry_distance: Option<Distance>,
    /// Lateral offset of the landing point. Positive = right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carry_offline: Option<Distance>,
    /// Apex above the tee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<Distance>,
    /// Launch to landing, seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flight_time: Option<f64>,
    /// Angle below horizontal at landing, degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descent_angle: Option<f64>,
    /// Landing to rest along the line from the tee. Negative when the ball
    /// spins back toward the tee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roll_distance: Option<Distance>,
    /// Lateral movement from landing to rest. Positive = right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roll_offline: Option<Distance>,
    /// Tee to rest, horizontal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_distance: Option<Distance>,
    /// Lateral offset of the rest position. Positive = right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_offline: Option<Distance>,
    /// Simulated ball path from the tee to rest, fitted to the measured
    /// values (the raw simulation when `replaces_measured` is set) and
    /// decimated to a few dozen points. Present only when the shot was
    /// simulated, same conditions as the rest of `DerivedFlight`. Always
    /// metres and seconds, regardless of the unit system of the other
    /// fields: `to_imperial`/`to_metric` leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<FlightPath>,
    /// True when these values replace the device's results: every result
    /// field above is simulated and takes precedence over the measured
    /// value in [`BallFlight`]. Set per shot when the `[simulation] mode`
    /// in effect when the shot was simulated is `force`.
    /// Omitted on the wire when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replaces_measured: bool,
    /// True when the spin in [`BallFlight`] (`backspin_rpm`, `sidespin_rpm`)
    /// is flighthook's estimate, not the device's reading: a FlightScope
    /// result with launch data but no spin and no flight. The spin stays in
    /// `BallFlight`, where outputs read it; this flag only marks its source.
    /// Set on its own (every other field empty) when the shot was not
    /// simulated. Omitted on the wire when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spin_estimated: bool,
}

/// A simulated ball path from the tee to rest.
///
/// Points are `[t, x, y, z]`: seconds since launch, metres downrange, metres
/// up, metres right of the target line, origin at the tee. Always metres and
/// seconds, independent of the unit system of the rest of the shot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FlightPath {
    /// Launch (the tee, `[0, 0, 0, 0]`) to landing (last point, on the ground).
    pub flight: Vec<[f64; 4]>,
    /// Landing (first point equals `flight`'s last) to rest. Empty when the
    /// ground phase did not reach rest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ground: Vec<[f64; 4]>,
}

impl DerivedFlight {
    /// True when no field is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Convert distances to yards (heights to feet, as for [`BallFlight`]).
    /// `path` is always metres/seconds and is carried over unchanged.
    #[must_use]
    pub fn to_imperial(&self) -> DerivedFlight {
        let yd = |d: Option<Distance>| d.map(|d| Distance::Yards(d.as_yards()));
        DerivedFlight {
            carry_distance: yd(self.carry_distance),
            carry_offline: yd(self.carry_offline),
            max_height: self.max_height.map(|d| Distance::Feet(d.as_feet())),
            flight_time: self.flight_time,
            descent_angle: self.descent_angle,
            roll_distance: yd(self.roll_distance),
            roll_offline: yd(self.roll_offline),
            total_distance: yd(self.total_distance),
            total_offline: yd(self.total_offline),
            path: self.path.clone(),
            replaces_measured: self.replaces_measured,
            spin_estimated: self.spin_estimated,
        }
    }

    /// Convert all distances to meters. `path` is always metres/seconds and
    /// is carried over unchanged.
    #[must_use]
    pub fn to_metric(&self) -> DerivedFlight {
        let m = |d: Option<Distance>| d.map(|d| Distance::Meters(d.as_meters()));
        DerivedFlight {
            carry_distance: m(self.carry_distance),
            carry_offline: m(self.carry_offline),
            max_height: m(self.max_height),
            flight_time: self.flight_time,
            descent_angle: self.descent_angle,
            roll_distance: m(self.roll_distance),
            roll_offline: m(self.roll_offline),
            total_distance: m(self.total_distance),
            total_offline: m(self.total_offline),
            path: self.path.clone(),
            replaces_measured: self.replaces_measured,
            spin_estimated: self.spin_estimated,
        }
    }
}

// ---------------------------------------------------------------------------
// Composed shot data
// ---------------------------------------------------------------------------

/// One shot as stored and served by the web layer (`/api/shots`).
///
/// `shot_id`, `club_selected`, `group` and `handed` are set by the web layer
/// on stored shots and are omitted on the wire when absent. Build one with
/// `ShotData::default()` and struct-update syntax.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ShotData {
    #[serde(default)]
    pub actor: String,
    pub shot_number: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ball: Option<BallFlight>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club: Option<ClubData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<FaceImpact>,
    /// Simulated values for fields the device did not report (beta).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<DerivedFlight>,
    /// Values computed from the measured `ball` and `club` (spin total and
    /// axis, face to path, spin loft, curve). Set when the shot is
    /// assembled ([`ShotAccumulator`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<ShotMetrics>,
    /// Shot id from the device actor's `ShotKey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shot_id: Option<String>,
    /// Club in play when the shot was triggered: the last `club_info` on the
    /// bus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club_selected: Option<Club>,
    /// Id of the shot group the shot belongs to (see
    /// [`ShotGroupInfo`](crate::ShotGroupInfo)); absent for shots that were
    /// not grouped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Player handedness in game state when the shot was triggered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed: Option<Handedness>,
}

impl ShotData {
    /// True when the device reported ball, club or impact data.
    #[must_use]
    pub fn has_data(&self) -> bool {
        self.ball.is_some() || self.club.is_some() || self.impact.is_some()
    }

    /// Convert all distance and velocity fields to the given unit system.
    pub fn to_unit_system(&self, system: UnitSystem) -> ShotData {
        match system {
            UnitSystem::Imperial => self.to_imperial(),
            UnitSystem::Metric => self.to_metric(),
        }
    }

    fn to_imperial(&self) -> ShotData {
        let ball = self.ball.as_ref().map(|b| BallFlight {
            launch_speed: b.launch_speed.map(|v| Velocity::MilesPerHour(v.as_mph())),
            launch_elevation: b.launch_elevation,
            launch_azimuth: b.launch_azimuth,
            carry_distance: b.carry_distance.map(|d| Distance::Yards(d.as_yards())),
            carry_offline: b.carry_offline.map(|d| Distance::Yards(d.as_yards())),
            total_distance: b.total_distance.map(|d| Distance::Yards(d.as_yards())),
            total_offline: b.total_offline.map(|d| Distance::Yards(d.as_yards())),
            max_height: b.max_height.map(|d| Distance::Feet(d.as_feet())),
            flight_time: b.flight_time,
            descent_angle: b.descent_angle,
            roll_distance: b.roll_distance.map(|d| Distance::Yards(d.as_yards())),
            backspin_rpm: b.backspin_rpm,
            sidespin_rpm: b.sidespin_rpm,
            total_spin_rpm: b.total_spin_rpm,
            spin_axis: b.spin_axis,
        });
        let club = self.club.as_ref().map(|c| ClubData {
            club_speed: c.club_speed.map(|v| Velocity::MilesPerHour(v.as_mph())),
            path: c.path,
            attack_angle: c.attack_angle,
            face_angle: c.face_angle,
            dynamic_loft: c.dynamic_loft,
            smash_factor: c.smash_factor,
            club_speed_post: c
                .club_speed_post
                .map(|v| Velocity::MilesPerHour(v.as_mph())),
            swing_plane_horizontal: c.swing_plane_horizontal,
            swing_plane_vertical: c.swing_plane_vertical,
            club_offset: c.club_offset.map(|d| Distance::Inches(d.as_inches())),
            club_height: c.club_height.map(|d| Distance::Inches(d.as_inches())),
            spin_loft: c.spin_loft,
            low_point_distance: c
                .low_point_distance
                .map(|d| Distance::Inches(d.as_inches())),
        });
        ShotData {
            actor: self.actor.clone(),
            shot_number: self.shot_number,
            ball,
            club,
            impact: self.impact.clone(),
            derived: self.derived.as_ref().map(DerivedFlight::to_imperial),
            metrics: self.metrics.as_ref().map(ShotMetrics::to_imperial),
            shot_id: self.shot_id.clone(),
            club_selected: self.club_selected,
            group: self.group.clone(),
            handed: self.handed,
        }
    }

    fn to_metric(&self) -> ShotData {
        let ball = self.ball.as_ref().map(|b| BallFlight {
            launch_speed: b
                .launch_speed
                .map(|v| Velocity::MetersPerSecond(v.as_mps())),
            launch_elevation: b.launch_elevation,
            launch_azimuth: b.launch_azimuth,
            carry_distance: b.carry_distance.map(|d| Distance::Meters(d.as_meters())),
            carry_offline: b.carry_offline.map(|d| Distance::Meters(d.as_meters())),
            total_distance: b.total_distance.map(|d| Distance::Meters(d.as_meters())),
            total_offline: b.total_offline.map(|d| Distance::Meters(d.as_meters())),
            max_height: b.max_height.map(|d| Distance::Meters(d.as_meters())),
            flight_time: b.flight_time,
            descent_angle: b.descent_angle,
            roll_distance: b.roll_distance.map(|d| Distance::Meters(d.as_meters())),
            backspin_rpm: b.backspin_rpm,
            sidespin_rpm: b.sidespin_rpm,
            total_spin_rpm: b.total_spin_rpm,
            spin_axis: b.spin_axis,
        });
        let club = self.club.as_ref().map(|c| ClubData {
            club_speed: c.club_speed.map(|v| Velocity::MetersPerSecond(v.as_mps())),
            path: c.path,
            attack_angle: c.attack_angle,
            face_angle: c.face_angle,
            dynamic_loft: c.dynamic_loft,
            smash_factor: c.smash_factor,
            club_speed_post: c
                .club_speed_post
                .map(|v| Velocity::MetersPerSecond(v.as_mps())),
            swing_plane_horizontal: c.swing_plane_horizontal,
            swing_plane_vertical: c.swing_plane_vertical,
            club_offset: c.club_offset.map(|d| Distance::Meters(d.as_meters())),
            club_height: c.club_height.map(|d| Distance::Meters(d.as_meters())),
            spin_loft: c.spin_loft,
            low_point_distance: c
                .low_point_distance
                .map(|d| Distance::Meters(d.as_meters())),
        });
        ShotData {
            actor: self.actor.clone(),
            shot_number: self.shot_number,
            ball,
            club,
            impact: self.impact.clone(),
            derived: self.derived.as_ref().map(DerivedFlight::to_metric),
            metrics: self.metrics.as_ref().map(ShotMetrics::to_metric),
            shot_id: self.shot_id.clone(),
            club_selected: self.club_selected,
            group: self.group.clone(),
            handed: self.handed,
        }
    }
}

// ---------------------------------------------------------------------------
// ActorStatus — generic actor lifecycle
// ---------------------------------------------------------------------------

/// Generic actor lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorStatus {
    Starting,
    Disconnected,
    Connected,
    Reconnecting,
}

impl std::fmt::Display for ActorStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Disconnected => write!(f, "disconnected"),
            Self::Connected => write!(f, "connected"),
            Self::Reconnecting => write!(f, "reconnecting"),
        }
    }
}

// ---------------------------------------------------------------------------
// ShotAccumulator — consumer-side shot assembly
// ---------------------------------------------------------------------------

/// Collects shot lifecycle events (`BallFlight`, `ClubPath`, `FaceImpact`)
/// and produces a complete [`ShotData`] on `ShotFinished`, plus the raw
/// device frames that came with it.
///
/// Used by consumers that need all shot fields together (GSPro bridge,
/// web shot cache, UI shot grid). Keyed by `(actor, ShotKey)`.
#[derive(Debug)]
pub struct ShotAccumulator {
    pub actor: String,
    pub key: ShotKey,
    ball: Option<BallFlight>,
    club: Option<ClubData>,
    impact: Option<FaceImpact>,
    derived: Option<DerivedFlight>,
    raw: Option<ShotRaw>,
}

impl ShotAccumulator {
    /// Create a new accumulator for a shot trigger.
    pub fn new(actor: String, key: ShotKey) -> Self {
        Self {
            actor,
            key,
            ball: None,
            club: None,
            impact: None,
            derived: None,
            raw: None,
        }
    }

    /// Record ball flight data.
    pub fn set_ball(&mut self, ball: BallFlight) {
        self.ball = Some(ball);
    }

    /// Record the simulated values that came with the ball flight.
    pub fn set_derived(&mut self, derived: Option<DerivedFlight>) {
        self.derived = derived;
    }

    /// Record club path data.
    pub fn set_club(&mut self, club: ClubData) {
        self.club = Some(club);
    }

    /// Record face impact data.
    pub fn set_impact(&mut self, impact: FaceImpact) {
        self.impact = Some(impact);
    }

    /// Record the raw device frames that came with `ShotFinished`.
    pub fn set_raw(&mut self, raw: Option<ShotRaw>) {
        self.raw = raw;
    }

    /// Finalize into a `ShotData`. Returns `None` if no data arrived at all.
    pub fn finish(self) -> Option<ShotData> {
        self.into_parts().0
    }

    /// Finalize into the decoded shot and its raw frames. The shot is `None`
    /// if no data arrived at all; the raw frames are kept either way. The
    /// shot's [`metrics`](ShotData::metrics) are computed from its ball and
    /// club data.
    pub fn into_parts(self) -> (Option<ShotData>, Option<ShotRaw>) {
        if self.ball.is_none() && self.club.is_none() && self.impact.is_none() {
            return (None, self.raw);
        }
        let metrics = ShotMetrics::compute(self.ball.as_ref(), self.club.as_ref());
        let shot = ShotData {
            actor: self.actor,
            shot_number: self.key.shot_number,
            ball: self.ball,
            club: self.club,
            impact: self.impact,
            derived: self.derived,
            metrics,
            ..ShotData::default()
        };
        (Some(shot), self.raw)
    }
}

// ---------------------------------------------------------------------------
// ShotAggregator — feed FlighthookMessages, get complete ShotData out
// ---------------------------------------------------------------------------

/// High-level shot collector that manages multiple in-flight shots.
///
/// Feed [`FlighthookMessage`](crate::FlighthookMessage) events via
/// [`feed`](Self::feed) and receive complete [`ShotData`] when a shot
/// lifecycle finishes.
///
/// ```ignore
/// # use flighthook::{ShotAggregator, FlighthookClient, FlighthookEvent};
/// let mut client = FlighthookClient::connect("ws://localhost:5880/frp", "my-app").unwrap();
/// let mut shots = ShotAggregator::new();
///
/// loop {
///     let msg = client.recv().unwrap();
///     // Match event variants with `..` so new fields don't break the pattern.
///     if let FlighthookEvent::BallFlight { key, .. } = &msg.event {
///         println!("ball flight for shot #{}", key.shot_number);
///     }
///     if let Some(shot) = shots.feed(&msg) {
///         println!("shot #{}: {:?} {:?}", shot.shot_number, shot.ball, shot.derived);
///     }
/// }
/// ```
#[derive(Debug, Default)]
pub struct ShotAggregator {
    pending: std::collections::HashMap<(String, ShotKey), ShotAccumulator>,
}

impl ShotAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a bus message. Returns a completed [`ShotData`] when a
    /// `ShotFinished` event finalizes an accumulated shot.
    pub fn feed(&mut self, msg: &crate::FlighthookMessage) -> Option<ShotData> {
        match &msg.event {
            crate::FlighthookEvent::ShotTrigger { key } => {
                let acc = ShotAccumulator::new(msg.actor.clone(), key.clone());
                self.pending.insert((msg.actor.clone(), key.clone()), acc);
                None
            }
            crate::FlighthookEvent::BallFlight { key, ball, derived } => {
                if let Some(acc) = self.pending.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_ball(*ball.clone());
                    acc.set_derived(derived.as_deref().cloned());
                }
                None
            }
            crate::FlighthookEvent::ClubPath { key, club } => {
                if let Some(acc) = self.pending.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_club(*club.clone());
                }
                None
            }
            crate::FlighthookEvent::FaceImpact { key, impact } => {
                if let Some(acc) = self.pending.get_mut(&(msg.actor.clone(), key.clone())) {
                    acc.set_impact(*impact.clone());
                }
                None
            }
            crate::FlighthookEvent::ShotFinished { key, .. } => self
                .pending
                .remove(&(msg.actor.clone(), key.clone()))?
                .finish(),
            _ => None,
        }
    }

    /// Number of shots currently being accumulated.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FlighthookEvent, FlighthookMessage, RawPayload};

    fn ball() -> BallFlight {
        BallFlight {
            launch_speed: Some(Velocity::MetersPerSecond(53.6)),
            launch_elevation: Some(17.0),
            launch_azimuth: Some(-1.2),
            carry_distance: None,
            total_distance: None,
            roll_distance: None,
            max_height: None,
            flight_time: None,
            backspin_rpm: Some(6500),
            sidespin_rpm: Some(-300),
            ..BallFlight::default()
        }
    }

    fn key() -> ShotKey {
        ShotKey {
            shot_id: "7f8e2d1c".into(),
            shot_number: 12,
        }
    }

    fn ball_flight_message(derived: Option<DerivedFlight>) -> FlighthookMessage {
        FlighthookMessage {
            actor: "square.0".into(),
            device: Some("SquareGolf-A1B2".into()),
            raw_payload: Some(RawPayload::Binary(vec![0x11, 0x02, 0x00, 0x2a, 0xff])),
            event: FlighthookEvent::BallFlight {
                key: key(),
                ball: Box::new(ball()),
                derived: derived.map(Box::new),
            },
        }
    }

    fn shot(derived: Option<DerivedFlight>) -> ShotData {
        ShotData {
            actor: "mevo.0".into(),
            shot_number: 7,
            ball: Some(BallFlight {
                carry_distance: Some(Distance::Meters(220.4)),
                max_height: Some(Distance::Meters(31.2)),
                flight_time: Some(6.4),
                ..ball()
            }),
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
            derived,
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

    // Wire format with no derived values; must stay byte-identical.
    const GOLDEN_BALL_FLIGHT: &str = r#"{"actor":"square.0","device":"SquareGolf-A1B2","raw_payload":"1102002aff","event":{"kind":"ball_flight","key":{"shot_id":"7f8e2d1c","shot_number":12},"ball":{"launch_speed":"53.6mps","launch_azimuth":-1.2,"launch_elevation":17.0,"backspin_rpm":6500,"sidespin_rpm":-300}}}"#;
    const GOLDEN_SHOT: &str = r#"{"actor":"mevo.0","shot_number":7,"ball":{"launch_speed":"53.6mps","launch_azimuth":-1.2,"launch_elevation":17.0,"carry_distance":"220.4m","max_height":"31.2m","flight_time":6.4,"backspin_rpm":6500,"sidespin_rpm":-300},"club":{"club_speed":"44.7mps","path":1.5,"attack_angle":-2.0,"face_angle":0.5,"dynamic_loft":18.0,"smash_factor":1.2},"impact":{"lateral":"-4mm","vertical":"2.5mm"}}"#;
    const GOLDEN_SHOT_IMPERIAL: &str = r#"{"actor":"mevo.0","shot_number":7,"ball":{"launch_speed":"119.89998400000002mph","launch_azimuth":-1.2,"launch_elevation":17.0,"carry_distance":"241.03237095363082yd","max_height":"102.36220472440944ft","flight_time":6.4,"backspin_rpm":6500,"sidespin_rpm":-300},"club":{"club_speed":"99.99121800000002mph","path":1.5,"attack_angle":-2.0,"face_angle":0.5,"dynamic_loft":18.0,"smash_factor":1.2},"impact":{"lateral":"-4mm","vertical":"2.5mm"}}"#;

    const GOLDEN_SHOT_FINISHED: &str = r#"{"actor":"square.0","device":"SquareGolf-A1B2","event":{"kind":"shot_finished","key":{"shot_id":"7f8e2d1c","shot_number":12}}}"#;

    fn raw() -> ShotRaw {
        ShotRaw {
            transport: "square-ble".into(),
            frames: vec![crate::RawRecord::binary(
                crate::RawDir::Rx,
                &[0x11, 0x02, 0x00],
            )],
            ..ShotRaw::default()
        }
    }

    #[test]
    fn shot_finished_without_raw_is_unchanged_on_the_wire() {
        let msg = FlighthookMessage {
            actor: "square.0".into(),
            device: Some("SquareGolf-A1B2".into()),
            raw_payload: None,
            event: FlighthookEvent::ShotFinished {
                key: key(),
                raw: None,
            },
        };
        assert_eq!(
            serde_json::to_string(&msg).expect("ser"),
            GOLDEN_SHOT_FINISHED
        );
        let back: FlighthookMessage = serde_json::from_str(GOLDEN_SHOT_FINISHED).expect("de");
        assert!(matches!(
            back.event,
            FlighthookEvent::ShotFinished { raw: None, .. }
        ));
    }

    #[test]
    fn shot_finished_raw_round_trips() {
        let msg = FlighthookMessage {
            actor: "square.0".into(),
            device: None,
            raw_payload: None,
            event: FlighthookEvent::ShotFinished {
                key: key(),
                raw: Some(Box::new(raw())),
            },
        };
        let json = serde_json::to_value(&msg).expect("ser");
        assert_eq!(json["event"]["raw"]["frames"][0]["hex"], "110200");
        let back: FlighthookMessage = serde_json::from_value(json).expect("de");
        let FlighthookEvent::ShotFinished { raw: r, .. } = back.event else {
            panic!("not shot_finished");
        };
        assert_eq!(r.as_deref(), Some(&raw()));
    }

    #[test]
    fn into_parts_keeps_raw_with_and_without_data() {
        let mut acc = ShotAccumulator::new("square.0".into(), key());
        acc.set_ball(ball());
        acc.set_raw(Some(raw()));
        let (shot, r) = acc.into_parts();
        assert_eq!(shot.expect("data").ball, Some(ball()));
        assert_eq!(r, Some(raw()));

        let mut acc = ShotAccumulator::new("square.0".into(), key());
        acc.set_raw(Some(raw()));
        let (shot, r) = acc.into_parts();
        assert!(shot.is_none());
        assert_eq!(r, Some(raw()));
    }

    #[test]
    fn aggregator_returns_shot_data_when_raw_is_present() {
        let mut agg = ShotAggregator::new();
        let msg = |event| FlighthookMessage {
            actor: "square.0".into(),
            device: None,
            raw_payload: None,
            event,
        };
        agg.feed(&msg(FlighthookEvent::ShotTrigger { key: key() }));
        agg.feed(&msg(FlighthookEvent::BallFlight {
            key: key(),
            ball: Box::new(ball()),
            derived: None,
        }));
        let shot = agg
            .feed(&msg(FlighthookEvent::ShotFinished {
                key: key(),
                raw: Some(Box::new(raw())),
            }))
            .expect("finished");
        assert_eq!(shot.ball, Some(ball()));
        assert_eq!(agg.pending_count(), 0);
    }

    #[test]
    fn finished_shot_carries_its_metrics() {
        let mut acc = ShotAccumulator::new("mevo.0".into(), key());
        acc.set_ball(ball());
        acc.set_club(ClubData {
            face_angle: Some(0.5),
            path: Some(1.5),
            ..ClubData::default()
        });
        let shot = acc.finish().expect("data");
        let m = shot.metrics.expect("metrics");
        // √(6500² + 300²); sidespin -300 tilts the axis left.
        assert_eq!(m.total_spin_rpm, Some(6507));
        assert!(m.spin_axis.expect("axis") < 0.0);
        assert_eq!(m.face_to_path, Some(-1.0));
        assert_eq!(m.curve, None);

        // Only impact data: nothing to compute from.
        let mut acc = ShotAccumulator::new("mevo.0".into(), key());
        acc.set_impact(FaceImpact {
            lateral: Some(Distance::Millimeters(1.0)),
            vertical: None,
        });
        assert_eq!(acc.finish().expect("data").metrics, None);
    }

    #[test]
    fn unit_conversion_covers_the_new_shot_fields() {
        let mut s = shot(None);
        s.ball = Some(BallFlight {
            carry_offline: Some(Distance::Meters(-9.144)),
            total_offline: Some(Distance::Meters(9.144)),
            descent_angle: Some(44.0),
            total_spin_rpm: Some(6507),
            spin_axis: Some(-2.6),
            ..ball()
        });
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
        let b = imp.ball.as_ref().expect("ball");
        assert!(matches!(b.carry_offline, Some(Distance::Yards(_))));
        assert!(close(b.carry_offline, -10.0) && close(b.total_offline, 10.0));
        assert_eq!(
            (b.descent_angle, b.total_spin_rpm, b.spin_axis),
            (Some(44.0), Some(6507), Some(-2.6))
        );
        let c = imp.club.as_ref().expect("club");
        assert!(matches!(c.low_point_distance, Some(Distance::Inches(_))));
        assert!(close(c.low_point_distance, 1.0));
        assert_eq!(c.spin_loft, Some(20.0));
        let curve = imp.metrics.as_ref().and_then(|m| m.curve);
        assert!(matches!(curve, Some(Distance::Yards(_))) && close(curve, 10.0));

        let met = imp.to_unit_system(UnitSystem::Metric);
        let b = met.ball.as_ref().expect("ball");
        assert!(matches!(b.total_offline, Some(Distance::Meters(_))));
        assert!(close(b.carry_offline, -9.144));
        assert!(close(
            met.club.as_ref().expect("club").low_point_distance,
            0.0254
        ));
        assert!(close(met.metrics.as_ref().and_then(|m| m.curve), 9.144));
    }

    #[test]
    fn has_data_needs_ball_club_or_impact() {
        let mut s = shot(None);
        assert!(s.has_data());
        s.ball = None;
        s.club = None;
        assert!(s.has_data());
        s.impact = None;
        assert!(!s.has_data());
    }

    #[test]
    fn ball_flight_without_derived_is_unchanged_on_the_wire() {
        let json = serde_json::to_string(&ball_flight_message(None)).expect("serializes");
        assert_eq!(json, GOLDEN_BALL_FLIGHT);
    }

    #[test]
    fn shot_data_without_derived_is_unchanged_on_the_wire() {
        assert_eq!(
            serde_json::to_string(&shot(None)).expect("ser"),
            GOLDEN_SHOT
        );
        let imperial = shot(None).to_unit_system(UnitSystem::Imperial);
        assert_eq!(
            serde_json::to_string(&imperial).expect("ser"),
            GOLDEN_SHOT_IMPERIAL
        );
    }

    #[test]
    fn derived_is_a_sibling_of_ball() {
        let json = serde_json::to_value(ball_flight_message(Some(derived()))).expect("ser");
        assert_eq!(json["event"]["kind"], "ball_flight");
        assert_eq!(json["event"]["derived"]["carry_offline"], "-5.5m");
        assert_eq!(json["event"]["derived"]["descent_angle"], 41.3);
        assert!(json["event"]["derived"].get("carry_distance").is_none());
        // The measured block is exactly what it was.
        let golden: serde_json::Value = serde_json::from_str(GOLDEN_BALL_FLIGHT).expect("golden");
        assert_eq!(json["event"]["ball"], golden["event"]["ball"]);
    }

    #[test]
    fn old_shape_json_parses_with_no_derived() {
        let msg: FlighthookMessage = serde_json::from_str(GOLDEN_BALL_FLIGHT).expect("parses");
        let FlighthookEvent::BallFlight {
            ball: b, derived, ..
        } = msg.event
        else {
            panic!("not a ball flight");
        };
        assert_eq!(*b, ball());
        assert_eq!(derived, None);

        let s: ShotData = serde_json::from_str(GOLDEN_SHOT).expect("parses");
        assert_eq!(s.derived, None);
        assert_eq!(s.shot_number, 7);
    }

    #[test]
    fn derived_round_trips() {
        let msg = ball_flight_message(Some(derived()));
        let back: FlighthookMessage =
            serde_json::from_str(&serde_json::to_string(&msg).expect("ser")).expect("de");
        let FlighthookEvent::BallFlight { derived: d, .. } = back.event else {
            panic!("not a ball flight");
        };
        assert_eq!(d.as_deref(), Some(&derived()));

        let s = shot(Some(derived()));
        let back: ShotData =
            serde_json::from_str(&serde_json::to_string(&s).expect("ser")).expect("de");
        assert_eq!(back.derived, Some(derived()));
    }

    #[test]
    fn spin_estimated_is_omitted_when_false_and_round_trips_when_true() {
        let json = serde_json::to_value(shot(Some(derived()))).expect("ser");
        assert!(json["derived"].get("spin_estimated").is_none(), "{json}");
        // Older JSON without the key parses as false.
        let old: DerivedFlight = serde_json::from_str(r#"{"descent_angle":41.3}"#).expect("de");
        assert!(!old.spin_estimated);

        let mut d = derived();
        d.spin_estimated = true;
        let text = serde_json::to_string(&d).expect("ser");
        assert!(text.ends_with(r#","spin_estimated":true}"#), "{text}");
        let back: DerivedFlight = serde_json::from_str(&text).expect("de");
        assert_eq!(back, d);
        assert!(d.to_imperial().spin_estimated);
        assert!(d.to_metric().spin_estimated);

        // On its own, for a shot that was not simulated.
        let alone = DerivedFlight {
            spin_estimated: true,
            ..DerivedFlight::default()
        };
        assert!(!alone.is_empty());
        let msg = ball_flight_message(Some(alone.clone()));
        let json = serde_json::to_value(&msg).expect("ser");
        assert_eq!(
            json["event"]["derived"],
            serde_json::json!({"spin_estimated": true})
        );
        let golden: serde_json::Value = serde_json::from_str(GOLDEN_BALL_FLIGHT).expect("golden");
        assert_eq!(json["event"]["ball"], golden["event"]["ball"]);
        let back: FlighthookMessage = serde_json::from_value(json).expect("de");
        let FlighthookEvent::BallFlight { derived: back, .. } = back.event else {
            panic!("not a ball flight");
        };
        assert_eq!(back.as_deref(), Some(&alone));

        // The assembled shot keeps it, in either unit system.
        let mut acc = ShotAccumulator::new(
            "mevo.0".into(),
            ShotKey {
                shot_id: "chip".into(),
                shot_number: 1,
            },
        );
        acc.set_ball(BallFlight {
            backspin_rpm: Some(1570),
            sidespin_rpm: Some(0),
            ..BallFlight::default()
        });
        acc.set_derived(Some(alone));
        let shot = acc.finish().expect("shot");
        assert!(shot.derived.as_ref().is_some_and(|d| d.spin_estimated));
        for system in [UnitSystem::Imperial, UnitSystem::Metric] {
            let converted = shot.to_unit_system(system);
            assert!(converted.derived.is_some_and(|d| d.spin_estimated));
        }
    }

    #[test]
    fn replaces_measured_is_omitted_when_false_and_round_trips_when_true() {
        let json = serde_json::to_value(shot(Some(derived()))).expect("ser");
        assert!(json["derived"].get("replaces_measured").is_none(), "{json}");

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

        let msg = ball_flight_message(Some(d));
        let json = serde_json::to_value(&msg).expect("ser");
        assert_eq!(json["event"]["derived"]["replaces_measured"], true);
        // The measured block is unchanged.
        let golden: serde_json::Value = serde_json::from_str(GOLDEN_BALL_FLIGHT).expect("golden");
        assert_eq!(json["event"]["ball"], golden["event"]["ball"]);
    }

    #[test]
    fn unit_conversion_covers_derived() {
        let s = shot(Some(derived()));
        let imp = s
            .to_unit_system(UnitSystem::Imperial)
            .derived
            .expect("kept");
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

    #[test]
    fn aggregator_carries_derived_to_the_finished_shot() {
        let mut agg = ShotAggregator::new();
        let msg = |event| FlighthookMessage {
            actor: "square.0".into(),
            device: None,
            raw_payload: None,
            event,
        };
        assert!(
            agg.feed(&msg(FlighthookEvent::ShotTrigger { key: key() }))
                .is_none()
        );
        assert!(
            agg.feed(&msg(FlighthookEvent::BallFlight {
                key: key(),
                ball: Box::new(ball()),
                derived: Some(Box::new(derived())),
            }))
            .is_none()
        );
        let shot = agg
            .feed(&msg(FlighthookEvent::ShotFinished {
                key: key(),
                raw: None,
            }))
            .expect("finished");
        assert_eq!(shot.ball, Some(ball()));
        assert_eq!(shot.derived, Some(derived()));
    }

    #[test]
    fn accumulator_without_derived_finishes_with_none() {
        let mut acc = ShotAccumulator::new("r10.0".into(), key());
        acc.set_ball(ball());
        let shot = acc.finish().expect("finished");
        assert_eq!(shot.derived, None);
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
        // A shot whose `derived` has no path omits the key entirely, and a
        // shot with no `derived` at all still matches the pre-path golden
        // shape byte-for-byte.
        let json = serde_json::to_value(shot(Some(derived()))).expect("ser");
        assert!(json["derived"].get("path").is_none());
        assert_eq!(
            serde_json::to_string(&shot(None)).expect("ser"),
            GOLDEN_SHOT
        );
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

        // Nested under a ball_flight bus message (the FRP output path):
        // `path` rides along as a sibling of the other derived fields.
        let msg = ball_flight_message(Some(d.clone()));
        let back: FlighthookMessage =
            serde_json::from_str(&serde_json::to_string(&msg).expect("ser")).expect("de");
        let FlighthookEvent::BallFlight { derived: got, .. } = back.event else {
            panic!("not a ball flight");
        };
        assert_eq!(got.as_deref(), Some(&d));

        // Nested under a full ShotData (the `/api/shots` shape):
        let s = shot(Some(d.clone()));
        let back: ShotData =
            serde_json::from_str(&serde_json::to_string(&s).expect("ser")).expect("de");
        assert_eq!(
            back.derived.as_ref().and_then(|d| d.path.as_ref()),
            Some(&flight_path())
        );
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
            club_selected: Some(Club::Iron7),
            group: Some(GROUP_ID.into()),
            ..shot(None)
        }
    }

    #[test]
    fn stamped_fields_round_trip() {
        let json = serde_json::to_string(&stamped()).expect("ser");
        let want_tail = format!(r#","shot_id":"abc","club_selected":"7I","group":"{GROUP_ID}"}}"#);
        assert!(json.ends_with(&want_tail), "{json}");
        // Everything before the new fields is the unstamped wire shape.
        assert_eq!(
            &json[..json.len() - want_tail.len()],
            &GOLDEN_SHOT[..GOLDEN_SHOT.len() - 1]
        );
        let back: ShotData = serde_json::from_str(&json).expect("de");
        assert_eq!(back, stamped());

        let minimal: ShotData = serde_json::from_str(&format!(
            r#"{{"shot_number":1,"shot_id":"abc","club_selected":"7I","group":"{GROUP_ID}"}}"#
        ))
        .expect("de");
        assert_eq!(minimal.shot_id.as_deref(), Some("abc"));
        assert_eq!(minimal.club_selected, Some(Club::Iron7));
        assert_eq!(minimal.group.as_deref(), Some(GROUP_ID));
    }

    #[test]
    fn json_without_stamped_fields_parses_as_none() {
        let s: ShotData = serde_json::from_str(GOLDEN_SHOT).expect("parses");
        assert_eq!(s.shot_id, None);
        assert_eq!(s.club_selected, None);
        assert_eq!(s.group, None);
        assert_eq!(s, shot(None));
    }

    #[test]
    fn unit_conversion_keeps_stamped_fields() {
        for system in [UnitSystem::Imperial, UnitSystem::Metric] {
            let c = stamped().to_unit_system(system);
            assert_eq!(c.shot_id.as_deref(), Some("abc"));
            assert_eq!(c.club_selected, Some(Club::Iron7));
            assert_eq!(c.group.as_deref(), Some(GROUP_ID));
        }
    }

    #[test]
    fn default_shot_data_is_empty() {
        let d = ShotData::default();
        assert_eq!(d.actor, "");
        assert_eq!(d.shot_number, 0);
        assert!(!d.has_data());
        assert_eq!(d.derived, None);
        assert_eq!(d.shot_id, None);
        assert_eq!(d.club_selected, None);
        assert_eq!(d.group, None);
        assert_eq!(
            serde_json::to_string(&d).expect("ser"),
            r#"{"actor":"","shot_number":0}"#
        );
    }

    #[test]
    fn accumulator_leaves_stamped_fields_unset() {
        let mut acc = ShotAccumulator::new("mevo.0".into(), key());
        acc.set_ball(ball());
        let shot = acc.finish().expect("finished");
        assert_eq!(shot.shot_id, None);
        assert_eq!(shot.club_selected, None);
        assert_eq!(shot.group, None);
    }
}
