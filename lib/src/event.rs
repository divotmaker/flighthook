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
/// the device's flight result (its own entry in [`ShotData::flights`]) is the device's and a
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
/// take precedence over the device's in [`ShotData::flights`], which are kept
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
    /// value in [`ShotData::flights`]. Set per shot when the `[simulation] mode`
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
    /// Absent on shots stored without it.
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
mod tests;
