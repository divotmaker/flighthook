//! Shared domain types used across the unified bus, the actors, and the UI.
//!
//! Shot data types (`Launch`, `FlightResult`, `ClubData`, `FaceImpact`) are
//! re-exported from the `flightrelay` crate. This module defines the composed
//! `ShotData` and accumulator types built on top of them.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use flightrelay::types::{Basis, ClubData, FaceImpact, FlightResult, Launch};
use flightrelay::units::{Distance, Velocity};

use crate::{Club, Handedness, ShotMetrics, ShotVideo, UnitSystem};

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
/// [`Launch`] that flighthook estimated rather than the device measured.
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
}

/// Where a shot comes from (FRP's device `source`): a physical launch
/// monitor that measured it, or no device at all.
pub use flightrelay::Source;

/// A tournament round: while one is active, virtual shot sources
/// (Auto-chip, the mock launch monitor, the trackball) are off, and an
/// output holding the round takes only measured shots and values.
///
/// A round is active while a simulator holds one (`held`), or while
/// tournament mode is switched on by hand (`manual`), or both.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundState {
    /// A round is active.
    #[serde(default)]
    pub active: bool,
    /// The display name of the round's holder: the first simulator holding
    /// it (its card name), else `"Manual"` while only tournament mode holds
    /// it. Absent when no round is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
    /// Tournament mode is switched on (`[simulation] tournament`). It stays
    /// on underneath a simulator's round. Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
    /// A simulator holds the round, so it cannot be switched off here.
    /// Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
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

impl DerivedFlight {
    /// The simulated values as a [`Basis::Modeled`] flight result.
    #[must_use]
    pub fn modeled(&self) -> FlightResult {
        FlightResult {
            carry_distance: self.carry_distance,
            carry_offline: self.carry_offline,
            total_distance: self.total_distance,
            total_offline: self.total_offline,
            roll_distance: self.roll_distance,
            max_height: self.max_height,
            flight_time: self.flight_time,
            descent_angle: self.descent_angle,
            ..FlightResult::new(Basis::Modeled)
        }
    }

    /// True when no field is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
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
        }
    }
}

// ---------------------------------------------------------------------------
// Composed shot data
// ---------------------------------------------------------------------------

/// One `shot.flight` of a stored shot: the actor that sent it and its
/// result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flight {
    pub from: String,
    pub flight: FlightResult,
}

/// `f` in yards and feet.
fn flight_imperial(f: &FlightResult) -> FlightResult {
    let yd = |d: Option<Distance>| d.map(|d| Distance::Yards(d.as_yards()));
    FlightResult {
        carry_distance: yd(f.carry_distance),
        carry_offline: yd(f.carry_offline),
        total_distance: yd(f.total_distance),
        total_offline: yd(f.total_offline),
        roll_distance: yd(f.roll_distance),
        max_height: f.max_height.map(|d| Distance::Feet(d.as_feet())),
        ..f.clone()
    }
}

/// `f` in meters.
fn flight_metric(f: &FlightResult) -> FlightResult {
    let m = |d: Option<Distance>| d.map(|d| Distance::Meters(d.as_meters()));
    FlightResult {
        carry_distance: m(f.carry_distance),
        carry_offline: m(f.carry_offline),
        total_distance: m(f.total_distance),
        total_offline: m(f.total_offline),
        roll_distance: m(f.roll_distance),
        max_height: m(f.max_height),
        ..f.clone()
    }
}

/// `stroke` with every distance in yards and feet, speeds in mph.
fn stroke_imperial(stroke: &flightrelay::GameStroke) -> flightrelay::GameStroke {
    let yd = |d: Distance| Distance::Yards(d.as_yards());
    let position = |p: &flightrelay::Position| flightrelay::Position(p.0.map(yd));
    flightrelay::GameStroke {
        flown: stroke.flown.as_ref().map(|l| Launch {
            launch_speed: l.launch_speed.map(|v| Velocity::MilesPerHour(v.as_mph())),
            ..l.clone()
        }),
        flight: stroke.flight.as_ref().map(flight_imperial),
        start_position: stroke.start_position.as_ref().map(position),
        end_position: stroke.end_position.as_ref().map(position),
        start_distance_to_hole: stroke.start_distance_to_hole.map(yd),
        end_distance_to_hole: stroke.end_distance_to_hole.map(yd),
        ..stroke.clone()
    }
}

/// `stroke` with every distance in meters, speeds in m/s.
fn stroke_metric(stroke: &flightrelay::GameStroke) -> flightrelay::GameStroke {
    let m = |d: Distance| Distance::Meters(d.as_meters());
    let position = |p: &flightrelay::Position| flightrelay::Position(p.0.map(m));
    flightrelay::GameStroke {
        flown: stroke.flown.as_ref().map(|l| Launch {
            launch_speed: l
                .launch_speed
                .map(|v| Velocity::MetersPerSecond(v.as_mps())),
            ..l.clone()
        }),
        flight: stroke.flight.as_ref().map(flight_metric),
        start_position: stroke.start_position.as_ref().map(position),
        end_position: stroke.end_position.as_ref().map(position),
        start_distance_to_hole: stroke.start_distance_to_hole.map(m),
        end_distance_to_hole: stroke.end_distance_to_hole.map(m),
        ..stroke.clone()
    }
}

/// What is shown for a stored shot, composed from its launch and flights
/// ([`ShotData::shown`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shown {
    /// The launch for a flight: each measured value, a missing one filled
    /// from the shown flight's `assumed` launch when that flight is modeled.
    pub launch: Option<Launch>,
    /// The launch monitor's own flight result (not an Auto-chip's, not one
    /// holding only assumed values).
    pub flight: Option<FlightResult>,
    /// The simulated values: the flight model's flight (an Auto-chip's own),
    /// the flight model's path, and `spin_estimated` when the spin came from
    /// an `assumed`.
    pub derived: Option<DerivedFlight>,
    /// Metrics from the shown launch, the launch monitor's own flight and
    /// the club.
    pub metrics: Option<ShotMetrics>,
}

/// Whether `flight` carries only assumed launch values and no result.
fn only_assumed(flight: &FlightResult) -> bool {
    let bare = |f: &FlightResult| FlightResult {
        assumed: None,
        model: None,
        ..f.clone()
    };
    bare(flight) == FlightResult::new(flight.basis)
}

/// `derived` from a flight model's result `modeled`, next to the launch
/// monitor's own flight `device`: every result value and the roll offline
/// (total offline less carry offline).
fn derived_of(modeled: &FlightResult, device: Option<&FlightResult>) -> DerivedFlight {
    let carry_offline = modeled
        .carry_offline
        .or(device.and_then(|f| f.carry_offline));
    DerivedFlight {
        carry_distance: modeled.carry_distance,
        carry_offline: modeled.carry_offline,
        max_height: modeled.max_height,
        flight_time: modeled.flight_time,
        descent_angle: modeled.descent_angle,
        roll_distance: modeled.roll_distance,
        total_distance: modeled.total_distance,
        total_offline: modeled.total_offline,
        roll_offline: modeled.total_offline.map(|total| {
            Distance::Meters(total.as_meters() - carry_offline.map_or(0.0, Distance::as_meters))
        }),
        ..DerivedFlight::default()
    }
}

/// One shot as stored and served by the web layer (`/api/shots`).
///
/// `launch` is the launch as the launch monitor `from` sent it and
/// `flights` every `shot.flight` of the shot with its sender; what is shown
/// is composed from them by [`ShotData::shown`]. `turn` is the
/// `golfer.turn` at the trigger, `block` the practice block the shot
/// belongs to. Optional fields are omitted on the wire when absent. Build
/// one with `ShotData::default()` and struct-update syntax.
///
/// Reading also accepts earlier shapes: a `ball` block in place of
/// `launch` (its flight values become the launch monitor's flight with
/// [`Basis::Modeled`]), `actor` (the card key, read as `card` and, without
/// `from`, as `from`), `flight` (the launch monitor's flight), `derived`
/// (the flight model's flight and path), `group`, `provenance`, and
/// `club_selected` / `handed` (read into `turn`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "ShotDataWire")]
pub struct ShotData {
    pub shot_number: u32,
    /// The measured launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<Launch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club: Option<ClubData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<FaceImpact>,
    /// Values computed from the shown result ([`ShotData::shown`]): spin
    /// total and axis, face to path, spin loft, curve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<ShotMetrics>,
    /// Shot id from the device actor's `ShotKey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shot_id: Option<String>,
    /// Id of the practice block (set) the shot belongs to; absent for shots
    /// in no block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    /// The `golfer.turn` when the shot was triggered: golfer, club, hand
    /// and mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<flightrelay::GolferTurn>,
    /// The club in play at the trigger, as the store stamped it: the putter
    /// for a launch monitor that arms only for putts, else the turn's club.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club_selected: Option<Club>,
    /// The cameras' clips of the shot (beta), one per camera. Never in the
    /// export.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub video: Vec<ShotVideo>,
    /// The launch monitor actor's id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    /// The local card key, when the launch monitor is a flighthook card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    /// A remote launch monitor's name from its `bus.actor` when the shot
    /// was stored, so the shot stays named after the actor leaves. Never
    /// in the export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    /// The name of a game's golfer (its `golfer.profile`) when the shot was
    /// stored, so the shot stays named after the round is over. Never in
    /// the export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub golfer_name: Option<String>,
    /// What stands behind the shot, from the launch monitor's `bus.actor`.
    /// Absent on shots stored without it; a tournament round counts those
    /// as not measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Every `shot.flight` of the shot, with its sender, in arrival order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flights: Vec<Flight>,
    /// The flight model's path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<FlightPath>,
    /// The game's view of the stroke, joined by `shot_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<flightrelay::GameStroke>,
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
    #[serde(default, alias = "group")]
    block: Option<String>,
    #[serde(default)]
    handed: Option<Handedness>,
    #[serde(default)]
    turn: Option<flightrelay::GolferTurn>,
    #[serde(default)]
    video: Vec<ShotVideo>,
    #[serde(default)]
    from: String,
    #[serde(default)]
    card: Option<String>,
    #[serde(default, alias = "provenance")]
    source: Option<Source>,
    #[serde(default)]
    flights: Vec<Flight>,
    #[serde(default)]
    path: Option<FlightPath>,
    #[serde(default)]
    stroke: Option<flightrelay::GameStroke>,
    #[serde(default)]
    from_name: Option<String>,
    #[serde(default)]
    golfer_name: Option<String>,
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
        let own = w
            .flight
            .or_else(|| w.launch.as_ref().and_then(LaunchWire::flight));
        let from = if w.from.is_empty() {
            w.actor.clone()
        } else {
            w.from
        };
        let card = w.card.or_else(|| (!w.actor.is_empty()).then_some(w.actor));
        let mut flights = w.flights;
        let mut path = w.path;
        if flights.is_empty() {
            if let Some(flight) = own {
                flights.push(Flight {
                    from: from.clone(),
                    flight,
                });
            }
            if let Some(derived) = w.derived {
                path = path.or_else(|| derived.path.clone());
                let flight = derived.modeled();
                if flight != FlightResult::new(Basis::Modeled) {
                    flights.push(Flight {
                        from: DERIVED_FROM.to_owned(),
                        flight,
                    });
                }
            }
        }
        let turn = w.turn.or_else(|| {
            (w.club_selected.is_some() || w.handed.is_some()).then_some(flightrelay::GolferTurn {
                golfer: None,
                club: w.club_selected,
                handedness: w.handed,
                mode: None,
            })
        });
        ShotData {
            shot_number: w.shot_number,
            launch: w.launch.map(|l| l.launch),
            club: w.club,
            impact: w.impact,
            metrics: w.metrics,
            shot_id: w.shot_id,
            block: w.block,
            turn,
            club_selected: w.club_selected,
            video: w.video,
            from,
            card,
            source: w.source,
            flights,
            path,
            stroke: w.stroke,
            from_name: w.from_name,
            golfer_name: w.golfer_name,
        }
    }
}

/// The sender named for a flight read from an earlier shape's `derived`
/// block: the flight model's.
const DERIVED_FROM: &str = "flight-model";

impl ShotData {
    /// The local card key of the launch monitor, else its actor id.
    #[must_use]
    pub fn card_key(&self) -> &str {
        self.card.as_deref().unwrap_or(&self.from)
    }

    /// The club in play at the trigger: the stamped
    /// [`club_selected`](Self::club_selected), else the turn's club.
    #[must_use]
    pub fn club_selected(&self) -> Option<Club> {
        self.club_selected
            .or_else(|| self.turn.as_ref().and_then(|t| t.club))
    }

    /// The player's hand in the turn at the trigger.
    #[must_use]
    pub fn handed(&self) -> Option<Handedness> {
        self.turn.as_ref().and_then(|t| t.handedness)
    }

    /// True when the device reported launch, flight, club or impact data.
    #[must_use]
    pub fn has_data(&self) -> bool {
        self.launch.is_some()
            || !self.flights.is_empty()
            || self.club.is_some()
            || self.impact.is_some()
    }

    /// What is shown for the shot, composed from its measured `launch`
    /// and its `flights`: the launch monitor's own flight is the device's,
    /// a flight from any other actor (the flight model) is simulated, and
    /// an Auto-chip's own flight ([`AUTO_CHIP_ACTOR`](crate::AUTO_CHIP_ACTOR)
    /// card) is simulated too.
    ///
    /// Both are shown side by side (the device's in `flight`, the
    /// simulated in `derived`), and where both carry a field the simulated
    /// value is the one displayed. The flight model sets such a field only
    /// for `[simulation] mode = "force"`, for a flown shot whose measured
    /// carry or flight time is exactly 0, and for a ground ball's 0 carry,
    /// apex and time; everywhere else it fills only what the launch monitor
    /// left out.
    #[must_use]
    pub fn shown(&self) -> Shown {
        let auto_chip = self.card_key() == crate::AUTO_CHIP_ACTOR;
        let own = self
            .flights
            .iter()
            .find(|f| f.from == self.from)
            .map(|f| &f.flight);
        let spin_estimated = own
            .and_then(|f| f.assumed.as_ref())
            .is_some_and(|a| a.backspin_rpm.is_some() || a.sidespin_rpm.is_some())
            && self
                .launch
                .as_ref()
                .is_none_or(|l| l.backspin_rpm.is_none() && l.sidespin_rpm.is_none());
        let device_flight = own.filter(|f| !auto_chip && !only_assumed(f));
        let modeled = self
            .flights
            .iter()
            .find(|f| f.from != self.from)
            .map(|f| &f.flight)
            .or_else(|| own.filter(|_| auto_chip));
        let mut derived = modeled.map(|m| derived_of(m, device_flight));
        if let Some(path) = &self.path {
            derived.get_or_insert_with(DerivedFlight::default).path = Some(path.clone());
        }
        if spin_estimated {
            derived
                .get_or_insert_with(DerivedFlight::default)
                .spin_estimated = true;
        }
        // The launch for a flight, as the outputs send it.
        let shown_flight = self
            .flights
            .iter()
            .find(|f| f.flight.basis == Basis::Tracked)
            .or_else(|| self.flights.iter().find(|f| f.from == self.from))
            .or_else(|| self.flights.first())
            .map(|f| &f.flight);
        let mut launch = self.launch.clone().unwrap_or_default();
        if let Some(flight) = shown_flight {
            launch.fill_assumed(flight);
        }
        let launch = (self.launch.is_some() || launch != Launch::default()).then_some(launch);
        let metrics = ShotMetrics::compute(launch.as_ref(), device_flight, self.club.as_ref());
        Shown {
            launch,
            flight: device_flight.cloned(),
            derived,
            metrics,
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
            shot_number: self.shot_number,
            launch,
            club,
            impact: self.impact.clone(),
            metrics: self.metrics.as_ref().map(ShotMetrics::to_imperial),
            shot_id: self.shot_id.clone(),
            block: self.block.clone(),
            turn: self.turn.clone(),
            club_selected: self.club_selected,
            video: self.video.clone(),
            from: self.from.clone(),
            card: self.card.clone(),
            source: self.source,
            flights: self
                .flights
                .iter()
                .map(|f| Flight {
                    from: f.from.clone(),
                    flight: flight_imperial(&f.flight),
                })
                .collect(),
            path: self.path.clone(),
            stroke: self.stroke.as_ref().map(stroke_imperial),
            from_name: self.from_name.clone(),
            golfer_name: self.golfer_name.clone(),
        }
    }

    fn to_metric(&self) -> ShotData {
        let launch = self.launch.as_ref().map(|l| Launch {
            launch_speed: l
                .launch_speed
                .map(|v| Velocity::MetersPerSecond(v.as_mps())),
            ..l.clone()
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
            shot_number: self.shot_number,
            launch,
            club,
            impact: self.impact.clone(),
            metrics: self.metrics.as_ref().map(ShotMetrics::to_metric),
            shot_id: self.shot_id.clone(),
            block: self.block.clone(),
            turn: self.turn.clone(),
            club_selected: self.club_selected,
            video: self.video.clone(),
            from: self.from.clone(),
            card: self.card.clone(),
            source: self.source,
            flights: self
                .flights
                .iter()
                .map(|f| Flight {
                    from: f.from.clone(),
                    flight: flight_metric(&f.flight),
                })
                .collect(),
            path: self.path.clone(),
            stroke: self.stroke.as_ref().map(stroke_metric),
            from_name: self.from_name.clone(),
            golfer_name: self.golfer_name.clone(),
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

/// How long a shot may take from `shot_trigger` to `shot_finished` before the
/// shot store and the outputs discard it: 45 s, flighthook's own accumulator timeout
/// (FRP lets a controller choose it; its default is 30 s).
///
/// It is longer than any device driver's own deadline for a shot, so a slow
/// shot the driver still completes reaches every output: ironsight gives up
/// on a Mevo or Xi shot the device has not processed 30 s after its trigger.
pub const SHOT_TIMEOUT: Duration = Duration::from_secs(45);

#[cfg(test)]
mod tests {
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
            held: true,
            manual: false,
        };
        assert_eq!(
            serde_json::to_value(&round).expect("ser"),
            serde_json::json!({"active": true, "holder": "Pureplay", "held": true})
        );
        let status: crate::StatusResponse = serde_json::from_str(
            r#"{"actors":{},"round":{"active":true,"holder":"Pureplay","held":true}}"#,
        )
        .expect("de");
        assert_eq!(status.round, round);
    }

    /// Tournament mode alone and under a simulator's round: `manual` and
    /// `held` go out only when true.
    #[test]
    fn round_state_carries_the_manual_and_held_flags() {
        let manual = RoundState {
            active: true,
            holder: Some("Manual".into()),
            held: false,
            manual: true,
        };
        let json = serde_json::to_value(&manual).expect("ser");
        assert_eq!(
            json,
            serde_json::json!({"active": true, "holder": "Manual", "manual": true})
        );
        assert_eq!(
            serde_json::from_value::<RoundState>(json).expect("de"),
            manual
        );
        let both: RoundState = serde_json::from_str(
            r#"{"active":true,"holder":"Pureplay Connect","held":true,"manual":true}"#,
        )
        .expect("de");
        assert!(both.held && both.manual);
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
}
