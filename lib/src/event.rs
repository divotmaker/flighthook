//! Shared domain types used across the unified bus, the actors, and the UI.
//!
//! Shot data types (`Launch`, `FlightResult`, `ClubData`, `FaceImpact`) are
//! re-exported from the `flightrelay` crate. This module defines the composed
//! `ShotData` and accumulator types built on top of them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use flightrelay::types::{AssumedLaunch, Basis, ClubData, FaceImpact, FlightResult, Launch};
use flightrelay::units::{Distance, Velocity};

use crate::{Club, Handedness, ShotKey, ShotMetrics, ShotRaw, ShotVideo, UnitSystem};

// ---------------------------------------------------------------------------
// Derived (simulated) flight values
// ---------------------------------------------------------------------------

/// Values flighthook simulated from the measured launch (beta).
///
/// By default present only for fields the device did not report: a value in
/// the device's flight result ([`ShotData::flight`]) is the device's and a
/// value here is simulated. A field set in both is shown with the value here: on a shot that flies, a
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
/// take precedence over the device's in [`ShotData::flight`], which are kept
/// unchanged.
///
/// [`spin_estimated`](Self::spin_estimated) marks a spin in the shot's
/// [`Launch`] that flighthook estimated rather than the device measured, and
/// [`computed`](Self::computed) the values in the shot's measured blocks
/// that come from a model rather than the device's readings.
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
    /// meters and seconds, regardless of the unit system of the other
    /// fields: `to_imperial`/`to_metric` leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<FlightPath>,
    /// True when these values replace the device's results: every result
    /// field above is simulated and takes precedence over the device's
    /// value in [`ShotData::flight`]. Set per shot when the `[simulation] mode`
    /// in effect when the shot was simulated is `force`.
    /// Omitted on the wire when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replaces_measured: bool,
    /// True when the spin in the shot's [`Launch`] (`backspin_rpm`,
    /// `sidespin_rpm`) is flighthook's estimate, not the device's reading: a
    /// FlightScope result with launch data but no spin and no flight. The
    /// spin stays in the launch, where outputs read it; this flag only marks
    /// its source. flighthook's FRP output sends it as the `assumed` spin of
    /// its own modeled flight result instead.
    /// Set on its own (every other field empty) when the shot was not
    /// simulated. Omitted on the wire when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spin_estimated: bool,
    /// Values in the shot's measured blocks ([`Launch`], `ClubData`,
    /// `FaceImpact`) that come from a model (aerodynamic, flight or
    /// otherwise) rather than from the device's readings. A plain
    /// translation of a reading is not computed: converting between the two
    /// spin pairs, a unit or sign conversion, a calibration offset. The
    /// values stay where outputs read them; this list only marks their
    /// source. Set on its own (every other field empty) when the shot was
    /// not simulated. Omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub computed: Vec<ShotField>,
}

/// A value in a shot's measured blocks, as named in
/// [`DerivedFlight::computed`]. Serialized with the FRP field name
/// (`impact_vertical` for `FaceImpact.vertical`).
///
/// Non-exhaustive so fields can be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ShotField {
    /// `Launch.launch_elevation`.
    LaunchElevation,
    /// `Launch.backspin_rpm`.
    BackspinRpm,
    /// `Launch.sidespin_rpm`.
    SidespinRpm,
    /// `Launch.total_spin_rpm`.
    TotalSpinRpm,
    /// `Launch.spin_axis`.
    SpinAxis,
    /// `FaceImpact.vertical`.
    ImpactVertical,
}

/// Where a shot comes from: a physical launch monitor that measured it, or
/// no device at all.
///
/// Set once, when the shot enters flighthook, from the type of the actor
/// that emits it ([`ShotData::provenance`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// A physical launch monitor measured the shot: one a flighthook driver
    /// reads, or one that sends its shots to the Open Connect input.
    Measured,
    /// No physical launch monitor measured the shot: the mock launch
    /// monitor, Auto-chip, and any simulated or replayed source.
    Virtual,
}

/// A tournament round: while one is active, the output holding it takes
/// only measured shots and values, and virtual shot sources (Auto-chip, the
/// mock launch monitor) are off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundState {
    /// A round is active.
    #[serde(default)]
    pub active: bool,
    /// The display name of the output holding the round (its card name).
    /// Absent when no round is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
}

impl RoundState {
    /// Whether no round is active.
    #[must_use]
    pub fn is_inactive(&self) -> bool {
        !self.active
    }
}

/// A simulated ball path from the tee to rest.
///
/// Points are `[t, x, y, z]`: seconds since launch, meters downrange, meters
/// up, meters right of the target line, origin at the tee. Always meters and
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

/// The `model` of the flight result flighthook adds on its FRP output for a
/// shot whose device sent none: flighthook's simulation
/// ([`DerivedFlight::flight_result`]).
pub const FLIGHT_MODEL: &str = "flighthook simulation (dimple)";

/// Whether `flight` is the flight result flighthook adds on its FRP output:
/// [`Basis::Modeled`] by its own simulation ([`FLIGHT_MODEL`]).
#[must_use]
pub fn is_flighthook_flight(flight: &FlightResult) -> bool {
    flight.basis == Basis::Modeled && flight.model.as_deref() == Some(FLIGHT_MODEL)
}

/// `launch` with the values `derived` marks as not measured
/// ([`DerivedFlight::computed`], and the spin when
/// [`DerivedFlight::spin_estimated`] is set) moved out: the measured launch
/// and the moved values, `None` when nothing was moved. This is the launch
/// flighthook's FRP output sends, and the moved values go out as the
/// `assumed` launch of its own flight result.
#[must_use]
pub fn measured_launch(
    launch: &Launch,
    derived: Option<&DerivedFlight>,
) -> (Launch, Option<AssumedLaunch>) {
    let Some(d) = derived else {
        return (launch.clone(), None);
    };
    let unmeasured =
        |f: ShotField| d.computed.contains(&f) || (d.spin_estimated && SPIN_FIELDS.contains(&f));
    let mut measured = launch.clone();
    let mut assumed = AssumedLaunch::default();
    if unmeasured(ShotField::LaunchElevation) {
        assumed.launch_elevation = measured.launch_elevation.take();
    }
    if unmeasured(ShotField::BackspinRpm) {
        assumed.backspin_rpm = measured.backspin_rpm.take();
    }
    if unmeasured(ShotField::SidespinRpm) {
        assumed.sidespin_rpm = measured.sidespin_rpm.take();
    }
    if unmeasured(ShotField::TotalSpinRpm) {
        assumed.total_spin_rpm = measured.total_spin_rpm.take();
    }
    if unmeasured(ShotField::SpinAxis) {
        assumed.spin_axis = measured.spin_axis.take();
    }
    (measured, (!assumed.is_empty()).then_some(assumed))
}

impl DerivedFlight {
    /// flighthook's own flight result from these simulated values:
    /// [`Basis::Modeled`], [`FLIGHT_MODEL`], every flight value here, and
    /// `assumed` holding the launch values the shot's measured launch lacks
    /// that the values rest on ([`measured_launch`]). `None` when there is
    /// nothing to send: no flight value and nothing assumed.
    #[must_use]
    pub fn flight_result(&self, assumed: Option<AssumedLaunch>) -> Option<FlightResult> {
        let flight = FlightResult {
            model: Some(FLIGHT_MODEL.to_owned()),
            carry_distance: self.carry_distance,
            carry_offline: self.carry_offline,
            total_distance: self.total_distance,
            total_offline: self.total_offline,
            roll_distance: self.roll_distance,
            max_height: self.max_height,
            flight_time: self.flight_time,
            descent_angle: self.descent_angle,
            assumed: assumed.filter(|a| !a.is_empty()),
            ..FlightResult::new(Basis::Modeled)
        };
        let empty = FlightResult {
            model: Some(FLIGHT_MODEL.to_owned()),
            ..FlightResult::new(Basis::Modeled)
        };
        (flight != empty).then_some(flight)
    }

    /// True when no field is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether `field` is marked as [`computed`](Self::computed).
    #[must_use]
    pub fn is_computed(&self, field: ShotField) -> bool {
        self.computed.contains(&field)
    }

    /// Mark `fields` as [`computed`](Self::computed) in `derived`, adding a
    /// block that holds only the marks when there is none. Marks already
    /// there are kept once.
    pub fn mark_computed(derived: &mut Option<Box<DerivedFlight>>, fields: &[ShotField]) {
        if fields.is_empty() {
            return;
        }
        let d = derived.get_or_insert_with(Box::default);
        for f in fields {
            if !d.computed.contains(f) {
                d.computed.push(*f);
            }
        }
    }

    /// Convert distances to yards (heights to feet, as for [`ShotData`]).
    /// `path` is always meters/seconds and is carried over unchanged.
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
            computed: self.computed.clone(),
        }
    }

    /// Convert all distances to meters. `path` is always meters/seconds and
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
            computed: self.computed.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Composed shot data
// ---------------------------------------------------------------------------

/// One shot as stored and served by the web layer (`/api/shots`).
///
/// `launch` is the measured launch and `flight` the device's flight result,
/// as the device sent them (FRP's `launch` and `flight_result`); `derived`
/// holds what flighthook simulated. `shot_id`, `club_selected`, `group`,
/// `handed` and `video` are set by the web layer on stored shots and are
/// omitted on the wire when absent. Build one with `ShotData::default()`
/// and struct-update syntax.
///
/// Reading also accepts the shape with a `ball` block in place of `launch`,
/// whose flight values (carry, total, apex and so on) become a `flight` with
/// [`Basis::Modeled`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "ShotDataWire")]
pub struct ShotData {
    #[serde(default)]
    pub actor: String,
    pub shot_number: u32,
    /// The measured launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<Launch>,
    /// The device's flight result: carry, total, apex and the rest as the
    /// device reported them, with the basis they were obtained on. Absent
    /// when the device reported none; flighthook's own simulated values are
    /// in `derived`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flight: Option<FlightResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club: Option<ClubData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<FaceImpact>,
    /// Simulated values for fields the device did not report (beta).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<DerivedFlight>,
    /// Values computed from the measured `launch`, the device's `flight` and
    /// `club` (spin total and axis, face to path, spin loft, curve). Set
    /// when the shot is assembled ([`ShotAccumulator`]).
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
    /// The cameras' clips of the shot (beta), one per camera. Never in the
    /// export.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub video: Vec<ShotVideo>,
    /// What stands behind the shot, from the type of the actor that emitted
    /// it. Absent on shots stored without it; a tournament round counts
    /// those as not measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// [`ShotData`] as read: `launch` may be a `ball` block that also holds the
/// device's flight values.
#[derive(Deserialize)]
struct ShotDataWire {
    #[serde(default)]
    actor: String,
    shot_number: u32,
    #[serde(default, alias = "ball")]
    launch: Option<LaunchWire>,
    #[serde(default)]
    flight: Option<FlightResult>,
    #[serde(default)]
    club: Option<ClubData>,
    #[serde(default)]
    impact: Option<FaceImpact>,
    #[serde(default)]
    derived: Option<DerivedFlight>,
    #[serde(default)]
    metrics: Option<ShotMetrics>,
    #[serde(default)]
    shot_id: Option<String>,
    #[serde(default)]
    club_selected: Option<Club>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    handed: Option<Handedness>,
    #[serde(default)]
    video: Vec<ShotVideo>,
    #[serde(default)]
    provenance: Option<Provenance>,
}

/// A `launch` block as read: the launch values, plus the flight values a
/// `ball` block carries next to them.
#[derive(Deserialize)]
struct LaunchWire {
    #[serde(flatten)]
    launch: Launch,
    #[serde(default)]
    carry_distance: Option<Distance>,
    #[serde(default)]
    carry_offline: Option<Distance>,
    #[serde(default)]
    total_distance: Option<Distance>,
    #[serde(default)]
    total_offline: Option<Distance>,
    #[serde(default)]
    roll_distance: Option<Distance>,
    #[serde(default)]
    max_height: Option<Distance>,
    #[serde(default)]
    flight_time: Option<f64>,
    #[serde(default)]
    descent_angle: Option<f64>,
}

impl LaunchWire {
    /// The flight values next to the launch, as a [`Basis::Modeled`] flight
    /// result; `None` when there are none.
    fn flight(&self) -> Option<FlightResult> {
        let flight = FlightResult {
            carry_distance: self.carry_distance,
            carry_offline: self.carry_offline,
            total_distance: self.total_distance,
            total_offline: self.total_offline,
            roll_distance: self.roll_distance,
            max_height: self.max_height,
            flight_time: self.flight_time,
            descent_angle: self.descent_angle,
            ..FlightResult::new(Basis::Modeled)
        };
        (flight != FlightResult::new(Basis::Modeled)).then_some(flight)
    }
}

impl From<ShotDataWire> for ShotData {
    fn from(w: ShotDataWire) -> Self {
        let flight = w
            .flight
            .or_else(|| w.launch.as_ref().and_then(LaunchWire::flight));
        ShotData {
            actor: w.actor,
            shot_number: w.shot_number,
            launch: w.launch.map(|l| l.launch),
            flight,
            club: w.club,
            impact: w.impact,
            derived: w.derived,
            metrics: w.metrics,
            shot_id: w.shot_id,
            club_selected: w.club_selected,
            group: w.group,
            handed: w.handed,
            video: w.video,
            provenance: w.provenance,
        }
    }
}

/// The four spin values, all unmeasured when flighthook estimated the spin.
const SPIN_FIELDS: [ShotField; 4] = [
    ShotField::TotalSpinRpm,
    ShotField::BackspinRpm,
    ShotField::SidespinRpm,
    ShotField::SpinAxis,
];

impl ShotData {
    /// True when the device reported launch, flight, club or impact data.
    #[must_use]
    pub fn has_data(&self) -> bool {
        self.launch.is_some()
            || self.flight.is_some()
            || self.club.is_some()
            || self.impact.is_some()
    }

    /// The values in the measured blocks that are not the device's reading:
    /// those marked [`DerivedFlight::computed`], and all four spin values
    /// when [`DerivedFlight::spin_estimated`] is set. Each field once.
    pub fn unmeasured_fields(&self) -> impl Iterator<Item = ShotField> + '_ {
        let derived = self.derived.as_ref();
        let estimated = derived.is_some_and(|d| d.spin_estimated);
        let computed = derived.map_or(&[][..], |d| d.computed.as_slice());
        let mut fields: Vec<ShotField> = Vec::new();
        for f in computed
            .iter()
            .copied()
            .chain(SPIN_FIELDS.into_iter().filter(move |_| estimated))
        {
            if !fields.contains(&f) {
                fields.push(f);
            }
        }
        fields.into_iter()
    }

    /// Remove `field`'s value from the measured blocks.
    pub fn clear_field(&mut self, field: ShotField) {
        let launch = self.launch.as_mut();
        match field {
            ShotField::LaunchElevation => {
                if let Some(l) = launch {
                    l.launch_elevation = None;
                }
            }
            ShotField::BackspinRpm => {
                if let Some(l) = launch {
                    l.backspin_rpm = None;
                }
            }
            ShotField::SidespinRpm => {
                if let Some(l) = launch {
                    l.sidespin_rpm = None;
                }
            }
            ShotField::TotalSpinRpm => {
                if let Some(l) = launch {
                    l.total_spin_rpm = None;
                }
            }
            ShotField::SpinAxis => {
                if let Some(l) = launch {
                    l.spin_axis = None;
                }
            }
            ShotField::ImpactVertical => {
                if let Some(i) = self.impact.as_mut() {
                    i.vertical = None;
                }
            }
        }
    }

    /// Convert all distance and velocity fields to the given unit system.
    pub fn to_unit_system(&self, system: UnitSystem) -> ShotData {
        match system {
            UnitSystem::Imperial => self.to_imperial(),
            UnitSystem::Metric => self.to_metric(),
        }
    }

    fn to_imperial(&self) -> ShotData {
        let launch = self.launch.as_ref().map(|l| Launch {
            launch_speed: l.launch_speed.map(|v| Velocity::MilesPerHour(v.as_mph())),
            ..l.clone()
        });
        let yd = |d: Option<Distance>| d.map(|d| Distance::Yards(d.as_yards()));
        let flight = self.flight.as_ref().map(|f| FlightResult {
            carry_distance: yd(f.carry_distance),
            carry_offline: yd(f.carry_offline),
            total_distance: yd(f.total_distance),
            total_offline: yd(f.total_offline),
            roll_distance: yd(f.roll_distance),
            max_height: f.max_height.map(|d| Distance::Feet(d.as_feet())),
            ..f.clone()
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
            launch,
            flight,
            club,
            impact: self.impact.clone(),
            derived: self.derived.as_ref().map(DerivedFlight::to_imperial),
            metrics: self.metrics.as_ref().map(ShotMetrics::to_imperial),
            shot_id: self.shot_id.clone(),
            club_selected: self.club_selected,
            group: self.group.clone(),
            handed: self.handed,
            video: self.video.clone(),
            provenance: self.provenance,
        }
    }

    fn to_metric(&self) -> ShotData {
        let launch = self.launch.as_ref().map(|l| Launch {
            launch_speed: l
                .launch_speed
                .map(|v| Velocity::MetersPerSecond(v.as_mps())),
            ..l.clone()
        });
        let m = |d: Option<Distance>| d.map(|d| Distance::Meters(d.as_meters()));
        let flight = self.flight.as_ref().map(|f| FlightResult {
            carry_distance: m(f.carry_distance),
            carry_offline: m(f.carry_offline),
            total_distance: m(f.total_distance),
            total_offline: m(f.total_offline),
            roll_distance: m(f.roll_distance),
            max_height: m(f.max_height),
            ..f.clone()
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
            launch,
            flight,
            club,
            impact: self.impact.clone(),
            derived: self.derived.as_ref().map(DerivedFlight::to_metric),
            metrics: self.metrics.as_ref().map(ShotMetrics::to_metric),
            shot_id: self.shot_id.clone(),
            club_selected: self.club_selected,
            group: self.group.clone(),
            handed: self.handed,
            video: self.video.clone(),
            provenance: self.provenance,
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

/// Collects shot lifecycle events (`Launch`, `FlightResult`, `ClubPath`,
/// `FaceImpact`) and produces a complete [`ShotData`] on `ShotFinished`,
/// plus the raw device frames that came with it.
///
/// A shot keeps the first event of each kind and ignores a repeat. Used by
/// consumers that need all shot fields together, usually through a
/// [`ShotAggregator`].
#[derive(Debug)]
pub struct ShotAccumulator {
    pub actor: String,
    pub key: ShotKey,
    launch: Option<Launch>,
    flight: Option<FlightResult>,
    club: Option<ClubData>,
    impact: Option<FaceImpact>,
    derived: Option<DerivedFlight>,
    raw: Option<ShotRaw>,
    provenance: Option<Provenance>,
}

impl ShotAccumulator {
    /// Create a new accumulator for a shot trigger.
    pub fn new(actor: String, key: ShotKey) -> Self {
        Self {
            actor,
            key,
            launch: None,
            flight: None,
            club: None,
            impact: None,
            derived: None,
            raw: None,
            provenance: None,
        }
    }

    /// The shot's provenance, from its trigger's envelope
    /// ([`FlighthookMessage::provenance`](crate::FlighthookMessage)).
    #[must_use]
    pub fn with_provenance(mut self, provenance: Option<Provenance>) -> Self {
        self.provenance = provenance;
        self
    }

    /// Record the launch and the simulated values that came with it. A shot
    /// keeps the first launch it receives; returns whether this one was
    /// kept.
    pub fn set_launch(&mut self, launch: Launch, derived: Option<DerivedFlight>) -> bool {
        if self.launch.is_some() {
            return false;
        }
        self.launch = Some(launch);
        self.derived = derived;
        true
    }

    /// Record the device's flight result. A shot keeps the first one it
    /// receives.
    pub fn set_flight(&mut self, flight: FlightResult) {
        self.flight.get_or_insert(flight);
    }

    /// Record club path data. A shot keeps the first it receives.
    pub fn set_club(&mut self, club: ClubData) {
        self.club.get_or_insert(club);
    }

    /// Record face impact data. A shot keeps the first it receives.
    pub fn set_impact(&mut self, impact: FaceImpact) {
        self.impact.get_or_insert(impact);
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
    /// shot's [`metrics`](ShotData::metrics) are computed from its launch,
    /// flight result and club data.
    pub fn into_parts(self) -> (Option<ShotData>, Option<ShotRaw>) {
        if self.launch.is_none()
            && self.flight.is_none()
            && self.club.is_none()
            && self.impact.is_none()
        {
            return (None, self.raw);
        }
        let metrics = ShotMetrics::compute(
            self.launch.as_ref(),
            self.flight.as_ref(),
            self.club.as_ref(),
        );
        let shot = ShotData {
            actor: self.actor,
            shot_number: self.key.shot_number,
            launch: self.launch,
            flight: self.flight,
            club: self.club,
            impact: self.impact,
            derived: self.derived,
            metrics,
            provenance: self.provenance,
            ..ShotData::default()
        };
        (Some(shot), self.raw)
    }

    /// Feed one shot event of this shot (anything but its trigger and
    /// finish), keeping the first of each kind.
    fn feed(&mut self, event: &crate::FlighthookEvent) {
        use crate::FlighthookEvent;
        match event {
            FlighthookEvent::Launch {
                launch, derived, ..
            } => {
                self.set_launch(*launch.clone(), derived.as_deref().cloned());
            }
            FlighthookEvent::FlightResult { flight, .. } => self.set_flight(*flight.clone()),
            FlighthookEvent::ClubPath { club, .. } => self.set_club(*club.clone()),
            FlighthookEvent::FaceImpact { impact, .. } => self.set_impact(*impact.clone()),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// ShotAggregator — feed FlighthookMessages, get complete ShotData out
// ---------------------------------------------------------------------------

/// How long a shot may take from `shot_trigger` to `shot_finished` before a
/// [`ShotAggregator`] discards it: FRP's accumulator timeout.
pub const SHOT_TIMEOUT: Duration = flightrelay::DEFAULT_SHOT_TIMEOUT;

/// High-level shot collector that manages multiple in-flight shots, keyed
/// by actor and shot id, with FRP's shot lifecycle rules.
///
/// Feed [`FlighthookMessage`](crate::FlighthookMessage) events via
/// [`feed`](Self::feed) and receive complete [`ShotData`] when a shot
/// lifecycle finishes.
///
/// - A shot keeps the first `launch`, `flight_result`, `club_path` and
///   `face_impact` it receives and ignores a repeat; a second `shot_trigger`
///   for a shot in flight is ignored too.
/// - A shot that has not finished within the timeout of its `shot_trigger`
///   ([`SHOT_TIMEOUT`] unless set with [`with_timeout`](Self::with_timeout))
///   is discarded, never emitted.
/// - Events for a shot that finished or was discarded are ignored for one
///   more timeout, so a late `shot_finished` or a repeat never starts or
///   finishes a shot again.
///
/// ```ignore
/// # use flighthook::{ShotAggregator, FlighthookClient, FlighthookEvent};
/// let mut client = FlighthookClient::connect("ws://localhost:5880/frp", "my-app").unwrap();
/// let mut shots = ShotAggregator::new();
///
/// loop {
///     let msg = client.recv().unwrap();
///     // Match event variants with `..` so new fields don't break the pattern.
///     if let FlighthookEvent::Launch { key, .. } = &msg.event {
///         println!("launch for shot #{}", key.shot_number);
///     }
///     if let Some(shot) = shots.feed(&msg) {
///         println!("shot #{}: {:?} {:?}", shot.shot_number, shot.launch, shot.flight);
///     }
/// }
/// ```
#[derive(Debug)]
pub struct ShotAggregator {
    timeout: Duration,
    /// Shots in flight by (actor, shot id), with the time of their trigger.
    pending: HashMap<(String, String), (Instant, ShotAccumulator)>,
    /// Shots finished or discarded, with the time they closed.
    closed: HashMap<(String, String), Instant>,
}

impl Default for ShotAggregator {
    fn default() -> Self {
        Self {
            timeout: SHOT_TIMEOUT,
            pending: HashMap::new(),
            closed: HashMap::new(),
        }
    }
}

impl ShotAggregator {
    /// An aggregator with the [`SHOT_TIMEOUT`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets how long a shot may take from `shot_trigger` to `shot_finished`
    /// before it is discarded.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The accumulator timeout.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Process a bus message. Returns a completed [`ShotData`] when a
    /// `ShotFinished` event finalizes an accumulated shot that has data.
    ///
    /// Reads the clock, which `wasm32-unknown-unknown` lacks; there, use
    /// [`feed_at`](Self::feed_at).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn feed(&mut self, msg: &crate::FlighthookMessage) -> Option<ShotData> {
        self.feed_at(msg, Instant::now())?.finish()
    }

    /// Process a bus message received at `now`. Returns the shot's
    /// accumulator when a `ShotFinished` event finalizes a shot in flight,
    /// for the caller to add its raw frames to and finish.
    pub fn feed_at(
        &mut self,
        msg: &crate::FlighthookMessage,
        now: Instant,
    ) -> Option<ShotAccumulator> {
        use crate::FlighthookEvent;
        self.discard_expired_at(now);
        let key = match &msg.event {
            FlighthookEvent::ShotTrigger { key, .. }
            | FlighthookEvent::Launch { key, .. }
            | FlighthookEvent::FlightResult { key, .. }
            | FlighthookEvent::ClubPath { key, .. }
            | FlighthookEvent::FaceImpact { key, .. }
            | FlighthookEvent::ShotFinished { key, .. } => key,
            _ => return None,
        };
        let id = (msg.actor.clone(), key.shot_id.clone());
        match &msg.event {
            FlighthookEvent::ShotTrigger { .. } => {
                if !self.closed.contains_key(&id) && !self.pending.contains_key(&id) {
                    let acc = ShotAccumulator::new(msg.actor.clone(), key.clone())
                        .with_provenance(msg.provenance);
                    self.pending.insert(id, (now, acc));
                }
                None
            }
            FlighthookEvent::ShotFinished { .. } => {
                let (_, acc) = self.pending.remove(&id)?;
                self.closed.insert(id, now);
                Some(acc)
            }
            event => {
                if let Some((_, acc)) = self.pending.get_mut(&id) {
                    acc.feed(event);
                }
                None
            }
        }
    }

    /// Whether the shot `shot_id` of `actor` is in flight: triggered, not
    /// yet finished or discarded.
    #[must_use]
    pub fn is_pending(&self, actor: &str, shot_id: &str) -> bool {
        self.pending
            .contains_key(&(actor.to_owned(), shot_id.to_owned()))
    }

    /// Discard the shot `shot_id` of `actor` if it is in flight. Its later
    /// events are ignored. Returns whether it was in flight.
    pub fn discard(&mut self, actor: &str, shot_id: &str, now: Instant) -> bool {
        let id = (actor.to_owned(), shot_id.to_owned());
        if self.pending.remove(&id).is_none() {
            return false;
        }
        self.closed.insert(id, now);
        true
    }

    /// Discard every shot that has not finished within the timeout of its
    /// `shot_trigger` at `now`, and return their actors and keys. Their
    /// later events are ignored. Every [`feed_at`](Self::feed_at) does this
    /// first.
    pub fn discard_expired_at(&mut self, now: Instant) -> Vec<(String, ShotKey)> {
        let timeout = self.timeout;
        self.closed
            .retain(|_, closed| now.saturating_duration_since(*closed) <= timeout);
        let expired: Vec<(String, String)> = self
            .pending
            .iter()
            .filter(|(_, (at, _))| now.saturating_duration_since(*at) > timeout)
            .map(|(id, _)| id.clone())
            .collect();
        let mut discarded = Vec::with_capacity(expired.len());
        for id in expired {
            if let Some((_, acc)) = self.pending.remove(&id) {
                discarded.push((acc.actor, acc.key));
            }
            self.closed.insert(id, now);
        }
        discarded
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

    fn key() -> ShotKey {
        ShotKey {
            shot_id: "7f8e2d1c".into(),
            shot_number: 12,
        }
    }

    fn launch_message(derived: Option<DerivedFlight>) -> FlighthookMessage {
        FlighthookMessage {
            actor: "square.0".into(),
            device: Some("SquareGolf-A1B2".into()),
            raw_payload: Some(RawPayload::Binary(vec![0x11, 0x02, 0x00, 0x2a, 0xff])),
            provenance: None,
            event: FlighthookEvent::Launch {
                key: key(),
                launch: Box::new(ball()),
                derived: derived.map(Box::new),
            },
        }
    }

    fn shot(derived: Option<DerivedFlight>) -> ShotData {
        ShotData {
            actor: "mevo.0".into(),
            shot_number: 7,
            launch: Some(ball()),
            flight: Some(device_flight()),
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
    const GOLDEN_LAUNCH: &str = r#"{"actor":"square.0","device":"SquareGolf-A1B2","raw_payload":"1102002aff","event":{"kind":"launch","key":{"shot_id":"7f8e2d1c","shot_number":12},"launch":{"launch_speed":"53.6mps","launch_azimuth":-1.2,"launch_elevation":17.0,"backspin_rpm":6500,"sidespin_rpm":-300}}}"#;
    const GOLDEN_FLIGHT_RESULT: &str = r#"{"actor":"mevo.0","device":"FS-M2-XXXXXX","event":{"kind":"flight_result","key":{"shot_id":"7f8e2d1c","shot_number":12},"flight":{"basis":"modeled","carry_distance":"220.4m","max_height":"31.2m","flight_time":6.4}}}"#;
    const GOLDEN_SHOT: &str = r#"{"actor":"mevo.0","shot_number":7,"launch":{"launch_speed":"53.6mps","launch_azimuth":-1.2,"launch_elevation":17.0,"backspin_rpm":6500,"sidespin_rpm":-300},"flight":{"basis":"modeled","carry_distance":"220.4m","max_height":"31.2m","flight_time":6.4},"club":{"club_speed":"44.7mps","path":1.5,"attack_angle":-2.0,"face_angle":0.5,"dynamic_loft":18.0,"smash_factor":1.2},"impact":{"lateral":"-4mm","vertical":"2.5mm"}}"#;
    const GOLDEN_SHOT_IMPERIAL: &str = r#"{"actor":"mevo.0","shot_number":7,"launch":{"launch_speed":"119.89998400000002mph","launch_azimuth":-1.2,"launch_elevation":17.0,"backspin_rpm":6500,"sidespin_rpm":-300},"flight":{"basis":"modeled","carry_distance":"241.03237095363082yd","max_height":"102.36220472440944ft","flight_time":6.4},"club":{"club_speed":"99.99121800000002mph","path":1.5,"attack_angle":-2.0,"face_angle":0.5,"dynamic_loft":18.0,"smash_factor":1.2},"impact":{"lateral":"-4mm","vertical":"2.5mm"}}"#;
    /// [`GOLDEN_SHOT`] in the shape stored before `launch` and `flight`:
    /// one `ball` block with the device's carry, apex and flight time.
    const STORED_BALL_SHOT: &str = r#"{"actor":"mevo.0","shot_number":7,"ball":{"launch_speed":"53.6mps","launch_azimuth":-1.2,"launch_elevation":17.0,"carry_distance":"220.4m","max_height":"31.2m","flight_time":6.4,"backspin_rpm":6500,"sidespin_rpm":-300},"club":{"club_speed":"44.7mps","path":1.5,"attack_angle":-2.0,"face_angle":0.5,"dynamic_loft":18.0,"smash_factor":1.2},"impact":{"lateral":"-4mm","vertical":"2.5mm"}}"#;

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
            provenance: None,
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
            provenance: None,
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
        acc.set_launch(ball(), None);
        acc.set_raw(Some(raw()));
        let (shot, r) = acc.into_parts();
        assert_eq!(shot.expect("data").launch, Some(ball()));
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
            provenance: None,
            event,
        };
        agg.feed(&msg(FlighthookEvent::shot_trigger(key())));
        agg.feed(&msg(FlighthookEvent::Launch {
            key: key(),
            launch: Box::new(ball()),
            derived: None,
        }));
        let shot = agg
            .feed(&msg(FlighthookEvent::ShotFinished {
                key: key(),
                raw: Some(Box::new(raw())),
            }))
            .expect("finished");
        assert_eq!(shot.launch, Some(ball()));
        assert_eq!(agg.pending_count(), 0);
    }

    #[test]
    fn finished_shot_carries_its_metrics() {
        let mut acc = ShotAccumulator::new("mevo.0".into(), key());
        acc.set_launch(ball(), None);
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
        s.launch = Some(Launch {
            total_spin_rpm: Some(6507),
            spin_axis: Some(-2.6),
            ..ball()
        });
        s.flight = Some(FlightResult {
            carry_offline: Some(Distance::Meters(-9.144)),
            total_offline: Some(Distance::Meters(9.144)),
            descent_angle: Some(44.0),
            ..device_flight()
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
        let f = imp.flight.as_ref().expect("flight");
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
        let f = met.flight.as_ref().expect("flight");
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
        let mut s = shot(None);
        assert!(s.has_data());
        s.launch = None;
        s.club = None;
        s.impact = None;
        assert!(s.has_data(), "the flight result alone");
        s.flight = None;
        assert!(!s.has_data());
        s.impact = shot(None).impact;
        assert!(s.has_data());
    }

    #[test]
    fn launch_without_derived_is_unchanged_on_the_wire() {
        let json = serde_json::to_string(&launch_message(None)).expect("serializes");
        assert_eq!(json, GOLDEN_LAUNCH);
    }

    #[test]
    fn flight_result_round_trips_with_its_basis() {
        let msg = FlighthookMessage {
            actor: "mevo.0".into(),
            device: Some("FS-M2-XXXXXX".into()),
            raw_payload: None,
            provenance: None,
            event: FlighthookEvent::FlightResult {
                key: key(),
                flight: Box::new(device_flight()),
            },
        };
        assert_eq!(
            serde_json::to_string(&msg).expect("ser"),
            GOLDEN_FLIGHT_RESULT
        );
        let back: FlighthookMessage = serde_json::from_str(GOLDEN_FLIGHT_RESULT).expect("de");
        let FlighthookEvent::FlightResult { flight, .. } = back.event else {
            panic!("not a flight result");
        };
        assert_eq!(*flight, device_flight());
        // The same event parses as FRP's.
        let frp = flightrelay::FrpMessage::parse(GOLDEN_FLIGHT_RESULT).expect("frp");
        let flightrelay::FrpMessage::Envelope(env) = frp else {
            panic!("not an envelope");
        };
        assert!(matches!(
            env.event,
            flightrelay::FrpEvent::FlightResult { ref flight, .. } if *flight == device_flight()
        ));
    }

    /// flighthook's launch and trigger events are FRP's, with flighthook's
    /// extension fields on top.
    #[test]
    fn launch_and_trigger_parse_as_frp() {
        let frp = flightrelay::FrpMessage::parse(GOLDEN_LAUNCH).expect("frp");
        let flightrelay::FrpMessage::Envelope(env) = frp else {
            panic!("not an envelope");
        };
        assert_eq!(env.device, "SquareGolf-A1B2");
        assert!(matches!(
            env.event,
            flightrelay::FrpEvent::Launch { ref launch, .. } if *launch == ball()
        ));
        let trigger = FlighthookMessage {
            actor: "square.0".into(),
            device: Some("SquareGolf-A1B2".into()),
            raw_payload: None,
            provenance: Some(Provenance::Measured),
            event: FlighthookEvent::ShotTrigger {
                key: key(),
                timestamp: Some(1_762_538_400_000),
            },
        };
        let json = serde_json::to_string(&trigger).expect("ser");
        let flightrelay::FrpMessage::Envelope(env) =
            flightrelay::FrpMessage::parse(&json).expect("frp")
        else {
            panic!("not an envelope");
        };
        assert!(matches!(
            env.event,
            flightrelay::FrpEvent::ShotTrigger {
                timestamp: Some(1_762_538_400_000),
                ..
            }
        ));
        let plain = serde_json::to_value(FlighthookEvent::shot_trigger(key())).expect("ser");
        assert!(plain.get("timestamp").is_none(), "{plain}");
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
    fn derived_is_a_sibling_of_launch() {
        let json = serde_json::to_value(launch_message(Some(derived()))).expect("ser");
        assert_eq!(json["event"]["kind"], "launch");
        assert_eq!(json["event"]["derived"]["carry_offline"], "-5.5m");
        assert_eq!(json["event"]["derived"]["descent_angle"], 41.3);
        assert!(json["event"]["derived"].get("carry_distance").is_none());
        // The measured block is exactly what it was.
        let golden: serde_json::Value = serde_json::from_str(GOLDEN_LAUNCH).expect("golden");
        assert_eq!(json["event"]["launch"], golden["event"]["launch"]);
    }

    #[test]
    fn json_without_derived_parses_with_no_derived() {
        let msg: FlighthookMessage = serde_json::from_str(GOLDEN_LAUNCH).expect("parses");
        let FlighthookEvent::Launch {
            launch: l, derived, ..
        } = msg.event
        else {
            panic!("not a launch");
        };
        assert_eq!(*l, ball());
        assert_eq!(derived, None);

        let s: ShotData = serde_json::from_str(GOLDEN_SHOT).expect("parses");
        assert_eq!(s.derived, None);
        assert_eq!(s.shot_number, 7);
    }

    /// A shot stored with one `ball` block reads into `launch` and a modeled
    /// `flight`, and is written back in the new shape.
    #[test]
    fn a_stored_ball_block_reads_into_launch_and_flight() {
        let old: ShotData = serde_json::from_str(STORED_BALL_SHOT).expect("old shape");
        assert_eq!(old, shot(None));
        assert_eq!(serde_json::to_string(&old).expect("ser"), GOLDEN_SHOT);

        // Launch values only: no flight result is made up.
        let launch_only: ShotData = serde_json::from_str(
            r#"{"shot_number":2,"ball":{"launch_speed":"53.6mps","backspin_rpm":0}}"#,
        )
        .expect("de");
        assert_eq!(launch_only.flight, None);
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
            full.flight,
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
        assert_eq!((empty.launch, empty.flight), (None, None));
    }

    #[test]
    fn derived_round_trips() {
        let msg = launch_message(Some(derived()));
        let back: FlighthookMessage =
            serde_json::from_str(&serde_json::to_string(&msg).expect("ser")).expect("de");
        let FlighthookEvent::Launch { derived: d, .. } = back.event else {
            panic!("not a launch");
        };
        assert_eq!(d.as_deref(), Some(&derived()));

        let s = shot(Some(derived()));
        let back: ShotData =
            serde_json::from_str(&serde_json::to_string(&s).expect("ser")).expect("de");
        assert_eq!(back.derived, Some(derived()));
    }

    /// `unmeasured_fields` names the computed marks and, for an estimated
    /// spin, all four spin values, each once; `clear_field` removes each.
    #[test]
    fn unmeasured_fields_join_both_marks() {
        let mut shot = ShotData {
            launch: Some(Launch {
                total_spin_rpm: Some(2500),
                spin_axis: Some(3.0),
                backspin_rpm: Some(2490),
                sidespin_rpm: Some(130),
                ..ball()
            }),
            impact: Some(FaceImpact {
                lateral: Some(Distance::Millimeters(3.0)),
                vertical: Some(Distance::Millimeters(-4.0)),
            }),
            ..ShotData::default()
        };
        assert_eq!(shot.unmeasured_fields().count(), 0);
        let mut marked = None;
        DerivedFlight::mark_computed(
            &mut marked,
            &[ShotField::SidespinRpm, ShotField::ImpactVertical],
        );
        shot.derived = marked.map(|d| *d);
        assert_eq!(
            shot.unmeasured_fields().collect::<Vec<_>>(),
            [ShotField::SidespinRpm, ShotField::ImpactVertical]
        );
        if let Some(d) = shot.derived.as_mut() {
            d.spin_estimated = true;
        }
        assert_eq!(
            shot.unmeasured_fields().collect::<Vec<_>>(),
            [
                ShotField::SidespinRpm,
                ShotField::ImpactVertical,
                ShotField::TotalSpinRpm,
                ShotField::BackspinRpm,
                ShotField::SpinAxis,
            ]
        );

        let fields: Vec<ShotField> = shot.unmeasured_fields().collect();
        for f in fields {
            shot.clear_field(f);
        }
        let b = shot.launch.as_ref().expect("launch");
        assert_eq!(
            (
                b.total_spin_rpm,
                b.spin_axis,
                b.backspin_rpm,
                b.sidespin_rpm
            ),
            (None, None, None, None)
        );
        assert_eq!(b.launch_elevation, Some(17.0), "untouched");
        let i = shot.impact.as_ref().expect("impact");
        assert_eq!(
            (i.lateral, i.vertical),
            (Some(Distance::Millimeters(3.0)), None)
        );
        shot.clear_field(ShotField::LaunchElevation);
        assert_eq!(shot.launch.and_then(|b| b.launch_elevation), None);
    }

    /// A shot's provenance is omitted when absent, so a shot stored without
    /// it reads back as `None`; set, it round-trips in snake case.
    #[test]
    fn provenance_is_optional_on_a_shot() {
        let old: ShotData =
            serde_json::from_str(r#"{"actor":"mevo.0","shot_number":3}"#).expect("old shot");
        assert_eq!(old.provenance, None);
        assert!(
            serde_json::to_value(&old)
                .expect("ser")
                .get("provenance")
                .is_none()
        );
        let shot = ShotData {
            provenance: Some(Provenance::Virtual),
            ..old
        };
        let json = serde_json::to_value(&shot).expect("ser");
        assert_eq!(json["provenance"], "virtual");
        assert_eq!(serde_json::from_value::<ShotData>(json).expect("de"), shot);
        assert_eq!(
            shot.to_unit_system(UnitSystem::Metric).provenance,
            Some(Provenance::Virtual)
        );
        assert_eq!(
            serde_json::to_value(Provenance::Measured).expect("ser"),
            "measured"
        );
    }

    /// The aggregator takes a shot's provenance from its trigger's envelope.
    #[test]
    fn the_trigger_envelope_sets_the_shot_provenance() {
        let mut agg = ShotAggregator::new();
        let trigger = FlighthookMessage::new(FlighthookEvent::shot_trigger(key()))
            .actor("square.0")
            .provenance(Provenance::Measured);
        let json = serde_json::to_value(&trigger).expect("ser");
        assert_eq!(json["provenance"], "measured");
        let msg = |event| FlighthookMessage::new(event).actor("square.0");
        assert!(
            serde_json::to_value(msg(FlighthookEvent::NewShotGroup))
                .expect("ser")
                .get("provenance")
                .is_none()
        );
        agg.feed(&trigger);
        agg.feed(&msg(FlighthookEvent::Launch {
            key: key(),
            launch: Box::new(ball()),
            derived: None,
        }));
        let shot = agg
            .feed(&msg(FlighthookEvent::ShotFinished {
                key: key(),
                raw: None,
            }))
            .expect("shot");
        assert_eq!(shot.provenance, Some(Provenance::Measured));
    }

    #[test]
    fn round_state_is_omitted_from_the_status_while_inactive() {
        let inactive = RoundState::default();
        assert!(inactive.is_inactive());
        assert_eq!(
            serde_json::to_value(&inactive).expect("ser"),
            serde_json::json!({"active": false})
        );
        let round = RoundState {
            active: true,
            holder: Some("Pureplay".into()),
        };
        let event = FlighthookEvent::RoundState {
            round: round.clone(),
        };
        assert_eq!(
            serde_json::to_value(&event).expect("ser"),
            serde_json::json!({"kind": "round_state", "round": {"active": true, "holder": "Pureplay"}})
        );
        let status: crate::StatusResponse =
            serde_json::from_str(r#"{"actors":{},"round":{"active":true,"holder":"Pureplay"}}"#)
                .expect("de");
        assert_eq!(status.round, round);
    }

    #[test]
    fn computed_marks_are_omitted_when_empty_and_round_trip() {
        let json = serde_json::to_value(shot(Some(derived()))).expect("ser");
        assert!(json["derived"].get("computed").is_none(), "{json}");
        let old: DerivedFlight = serde_json::from_str(r#"{"descent_angle":41.3}"#).expect("de");
        assert!(old.computed.is_empty());

        let mut marked = None;
        DerivedFlight::mark_computed(&mut marked, &[]);
        assert!(marked.is_none(), "nothing to mark adds no block");
        DerivedFlight::mark_computed(
            &mut marked,
            &[ShotField::BackspinRpm, ShotField::SidespinRpm],
        );
        DerivedFlight::mark_computed(&mut marked, &[ShotField::SidespinRpm]);
        let d = *marked.expect("a block holding only the marks");
        assert_eq!(d.computed, [ShotField::BackspinRpm, ShotField::SidespinRpm]);
        assert!(d.is_computed(ShotField::SidespinRpm));
        assert!(!d.is_computed(ShotField::SpinAxis));
        assert!(!d.is_empty());
        assert_eq!(
            serde_json::to_value(&d).expect("ser"),
            serde_json::json!({"computed": ["backspin_rpm", "sidespin_rpm"]})
        );
        let back: DerivedFlight =
            serde_json::from_str(r#"{"computed":["launch_elevation","impact_vertical"]}"#)
                .expect("de");
        assert_eq!(
            back.computed,
            [ShotField::LaunchElevation, ShotField::ImpactVertical]
        );
        assert_eq!(d.to_imperial().computed, d.computed);
        assert_eq!(d.to_metric().computed, d.computed);
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
        let msg = launch_message(Some(alone.clone()));
        let json = serde_json::to_value(&msg).expect("ser");
        assert_eq!(
            json["event"]["derived"],
            serde_json::json!({"spin_estimated": true})
        );
        let golden: serde_json::Value = serde_json::from_str(GOLDEN_LAUNCH).expect("golden");
        assert_eq!(json["event"]["launch"], golden["event"]["launch"]);
        let back: FlighthookMessage = serde_json::from_value(json).expect("de");
        let FlighthookEvent::Launch { derived: back, .. } = back.event else {
            panic!("not a launch");
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
        acc.set_launch(
            Launch {
                backspin_rpm: Some(1570),
                sidespin_rpm: Some(0),
                ..Launch::default()
            },
            Some(alone),
        );
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

        let msg = launch_message(Some(d));
        let json = serde_json::to_value(&msg).expect("ser");
        assert_eq!(json["event"]["derived"]["replaces_measured"], true);
        // The measured block is unchanged.
        let golden: serde_json::Value = serde_json::from_str(GOLDEN_LAUNCH).expect("golden");
        assert_eq!(json["event"]["launch"], golden["event"]["launch"]);
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
            provenance: None,
            event,
        };
        assert!(
            agg.feed(&msg(FlighthookEvent::shot_trigger(key())))
                .is_none()
        );
        assert!(
            agg.feed(&msg(FlighthookEvent::Launch {
                key: key(),
                launch: Box::new(ball()),
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
        assert_eq!(shot.launch, Some(ball()));
        assert_eq!(shot.derived, Some(derived()));
    }

    #[test]
    fn accumulator_without_derived_finishes_with_none() {
        let mut acc = ShotAccumulator::new("r10.0".into(), key());
        acc.set_launch(ball(), None);
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

        // Nested under a launch bus message (the FRP output path): `path`
        // rides along as a sibling of the other derived fields.
        let msg = launch_message(Some(d.clone()));
        let back: FlighthookMessage =
            serde_json::from_str(&serde_json::to_string(&msg).expect("ser")).expect("de");
        let FlighthookEvent::Launch { derived: got, .. } = back.event else {
            panic!("not a launch");
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
            json.ends_with(r#""video":[{"camera":"camera.0","position":"golfer_front","duration_ms":5500,"fps":120.0,"trigger_offset_ms":4000,"impact_offset_ms":4000,"complete":true,"dropped":0}]}"#),
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
        acc.set_launch(ball(), None);
        let shot = acc.finish().expect("finished");
        assert_eq!(shot.shot_id, None);
        assert_eq!(shot.club_selected, None);
        assert_eq!(shot.group, None);
    }

    fn bus(event: FlighthookEvent) -> FlighthookMessage {
        FlighthookMessage::new(event).actor("mevo.0")
    }

    fn launch_event(launch: Launch) -> FlighthookEvent {
        FlighthookEvent::Launch {
            key: key(),
            launch: Box::new(launch),
            derived: None,
        }
    }

    /// A shot keeps the first event of each kind: a repeated trigger, launch,
    /// flight result, club path or face impact changes nothing.
    #[test]
    fn the_aggregator_keeps_the_first_event_of_each_kind() {
        let t0 = Instant::now();
        let mut agg = ShotAggregator::new();
        let other = Launch {
            launch_speed: Some(Velocity::MetersPerSecond(10.0)),
            ..Launch::default()
        };
        let club = |speed| ClubData {
            club_speed: Some(Velocity::MetersPerSecond(speed)),
            ..ClubData::default()
        };
        let impact = |mm| FaceImpact {
            lateral: Some(Distance::Millimeters(mm)),
            vertical: None,
        };
        let second_flight = FlightResult {
            carry_distance: Some(Distance::Meters(1.0)),
            ..FlightResult::new(Basis::Tracked)
        };
        for event in [
            FlighthookEvent::shot_trigger(key()),
            launch_event(ball()),
            FlighthookEvent::shot_trigger(key()),
            launch_event(other),
            FlighthookEvent::FlightResult {
                key: key(),
                flight: Box::new(device_flight()),
            },
            FlighthookEvent::FlightResult {
                key: key(),
                flight: Box::new(second_flight),
            },
            FlighthookEvent::ClubPath {
                key: key(),
                club: Box::new(club(40.0)),
            },
            FlighthookEvent::ClubPath {
                key: key(),
                club: Box::new(club(10.0)),
            },
            FlighthookEvent::FaceImpact {
                key: key(),
                impact: Box::new(impact(3.0)),
            },
            FlighthookEvent::FaceImpact {
                key: key(),
                impact: Box::new(impact(-9.0)),
            },
        ] {
            assert!(agg.feed_at(&bus(event), t0).is_none());
        }
        let shot = agg
            .feed_at(
                &bus(FlighthookEvent::ShotFinished {
                    key: key(),
                    raw: None,
                }),
                t0,
            )
            .expect("finished")
            .finish()
            .expect("data");
        assert_eq!(shot.launch, Some(ball()));
        assert_eq!(shot.flight, Some(device_flight()));
        assert_eq!(shot.club, Some(club(40.0)));
        assert_eq!(shot.impact, Some(impact(3.0)));
        // Metrics read the device's flight (curve needs its carry offline,
        // which this one lacks) and the launch's spin.
        assert_eq!(shot.metrics.and_then(|m| m.total_spin_rpm), Some(6507));
    }

    /// A shot that has not finished within the timeout is discarded: its
    /// late `shot_finished` is ignored, and so is any event of a finished or
    /// discarded shot for one more timeout.
    #[test]
    fn the_aggregator_discards_a_shot_after_the_timeout() {
        let t0 = Instant::now();
        let mut agg = ShotAggregator::new();
        assert_eq!(agg.timeout(), Duration::from_secs(30));
        agg.feed_at(&bus(FlighthookEvent::shot_trigger(key())), t0);
        agg.feed_at(&bus(launch_event(ball())), t0);
        assert!(agg.is_pending("mevo.0", &key().shot_id));
        let late = t0 + SHOT_TIMEOUT + Duration::from_millis(1);
        let discarded = agg.discard_expired_at(late);
        assert_eq!(discarded, [("mevo.0".to_owned(), key())]);
        let finished = bus(FlighthookEvent::ShotFinished {
            key: key(),
            raw: None,
        });
        assert!(agg.feed_at(&finished, late).is_none(), "discarded");
        // A repeat of its trigger within the next timeout starts nothing.
        agg.feed_at(&bus(FlighthookEvent::shot_trigger(key())), late);
        assert_eq!(agg.pending_count(), 0);
        // After it, the shot id may start a shot again.
        let much_later = late + SHOT_TIMEOUT + Duration::from_millis(1);
        agg.feed_at(&bus(FlighthookEvent::shot_trigger(key())), much_later);
        assert_eq!(agg.pending_count(), 1);

        // Within the timeout the shot finishes; a second finish is ignored.
        let mut agg = ShotAggregator::new().with_timeout(Duration::from_secs(5));
        agg.feed_at(&bus(FlighthookEvent::shot_trigger(key())), t0);
        let within = t0 + Duration::from_secs(5);
        assert!(agg.feed_at(&finished, within).is_some());
        assert!(agg.feed_at(&finished, within).is_none());
        agg.feed_at(&bus(FlighthookEvent::shot_trigger(key())), within);
        assert_eq!(agg.pending_count(), 0, "a finished shot does not restart");
    }

    /// Shots are kept apart by actor and shot id; `discard` closes one.
    #[test]
    fn the_aggregator_keys_shots_by_actor_and_shot_id() {
        let t0 = Instant::now();
        let mut agg = ShotAggregator::new();
        let from = |actor: &str, event| FlighthookMessage::new(event).actor(actor);
        agg.feed_at(&from("mevo.0", FlighthookEvent::shot_trigger(key())), t0);
        agg.feed_at(&from("r10.0", FlighthookEvent::shot_trigger(key())), t0);
        assert_eq!(agg.pending_count(), 2);
        assert!(agg.discard("r10.0", &key().shot_id, t0));
        assert!(!agg.discard("r10.0", &key().shot_id, t0));
        let finished = FlighthookEvent::ShotFinished {
            key: key(),
            raw: None,
        };
        assert!(agg.feed_at(&from("r10.0", finished.clone()), t0).is_none());
        let acc = agg.feed_at(&from("mevo.0", finished), t0).expect("mevo");
        assert_eq!(acc.actor, "mevo.0");
    }

    /// The launch flighthook's FRP output sends leaves out what derived marks
    /// as not measured, and its own flight result assumes those values.
    #[test]
    fn an_estimated_spin_moves_to_the_assumed_launch_of_flighthooks_flight() {
        let launch = Launch {
            backspin_rpm: Some(1570),
            sidespin_rpm: Some(0),
            ..ball()
        };
        assert_eq!(measured_launch(&launch, None), (launch.clone(), None));
        let plain = derived();
        assert_eq!(
            measured_launch(&launch, Some(&plain)),
            (launch.clone(), None)
        );

        let estimated = DerivedFlight {
            spin_estimated: true,
            carry_distance: Some(Distance::Meters(14.2)),
            ..derived()
        };
        let (measured, assumed) = measured_launch(&launch, Some(&estimated));
        assert_eq!((measured.backspin_rpm, measured.sidespin_rpm), (None, None));
        assert_eq!(measured.launch_speed, launch.launch_speed);
        let assumed = assumed.expect("assumed");
        assert_eq!(
            (
                assumed.backspin_rpm,
                assumed.sidespin_rpm,
                assumed.total_spin_rpm
            ),
            (Some(1570), Some(0), None)
        );

        let flight = estimated.flight_result(Some(assumed)).expect("flight");
        assert!(is_flighthook_flight(&flight));
        assert_eq!(flight.carry_distance, Some(Distance::Meters(14.2)));
        assert_eq!(flight.total_distance, estimated.total_distance);
        assert_eq!(flight.descent_angle, Some(41.3));
        // A controller that flies the ball gets the estimate back.
        let mut flown = measured.clone();
        flown.fill_assumed(&flight);
        assert_eq!(flown, launch);
        // Nothing simulated and nothing assumed: no flight result.
        assert_eq!(DerivedFlight::default().flight_result(None), None);
        // A computed launch angle moves too.
        let mut marked = None;
        DerivedFlight::mark_computed(&mut marked, &[ShotField::LaunchElevation]);
        let (m, a) = measured_launch(&launch, marked.as_deref());
        assert_eq!(m.launch_elevation, None);
        assert_eq!(a.and_then(|a| a.launch_elevation), Some(17.0));
        // A device's own flight result is not flighthook's.
        assert!(!is_flighthook_flight(&device_flight()));
    }
}
