//! Pureplay's shots in flighthook: which ones the card takes, how a shot's
//! values become flighthook's, and how Pureplay's later result finds the
//! shot it belongs to.
//!
//! A shot of a player at this machine (`isLocal`) is a candidate. The
//! card's Shots setting ([`PureplayShots`]) decides what becomes of it
//! ([`decide`]):
//!
//! - **Auto**: a candidate that echoes a shot a simulator output sent
//!   (the send registry, [`SentShots::take_echo`](crate::state::sent_shots::SentShots))
//!   is not taken; Pureplay's result goes to the shot flighthook sent. Any
//!   other candidate is taken.
//! - **From Pureplay**: every candidate is taken, with no matching
//!   ([`matches_echoes`]).
//! - **From flighthook**: none is taken; echoes are still matched, so
//!   their results reach flighthook's shots.
//!
//! A shot the card takes gets flighthook's own key (a fresh shot id, the
//! card's own count). [`ShotMap`] keeps Pureplay's `shotId` against the key
//! of the flighthook shot it stands for, a taken shot or a matched echo, so
//! the `shot.completed` that follows seconds later finds it.

use std::collections::VecDeque;

use flighthook::{Distance, Launch, PureplayShots, ShotKey, Velocity};
use flightrelay::{Basis, FlightResult, GameStroke, GolferId, Lie, Position, StrokeOutcome};

use super::protocol::{ShotCompleted, ShotLaunched};

/// The sign that turns Pureplay's `sideSpinRpm` into flighthook's sidespin
/// (positive curves right). Set by a round trip on hardware.
pub const SIDE_SPIN_SIGN: f64 = 1.0;

/// Most Pureplay shots the card keeps the key of.
pub const MAP_CAPACITY: usize = 256;

/// The flight model Pureplay's flight is named by.
pub const SOURCE: &str = "Pureplay";

/// Whether the card matches a candidate against the shots the simulator
/// outputs sent, in mode `shots`. "From Pureplay" takes every local shot
/// as its own, so it does not.
#[must_use]
pub fn matches_echoes(shots: PureplayShots) -> bool {
    shots != PureplayShots::FromPureplay
}

/// What becomes of a candidate shot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The card takes it: a flighthook shot of its own.
    Take,
    /// It echoes flighthook's shot `ShotKey`: not taken; its result goes
    /// to that shot.
    Echo(ShotKey),
    /// Not taken, and no shot of flighthook's: "From flighthook" with no
    /// matching send.
    Ignore,
}

/// What becomes of a candidate shot in mode `shots`. `echo` finds (and
/// consumes) the send the shot echoes; it is asked only when the mode
/// matches echoes.
pub fn decide(shots: PureplayShots, echo: impl FnOnce() -> Option<ShotKey>) -> Decision {
    if !matches_echoes(shots) {
        return Decision::Take;
    }
    match echo() {
        Some(key) => Decision::Echo(key),
        None if shots == PureplayShots::FromFlighthook => Decision::Ignore,
        None => Decision::Take,
    }
}

/// flighthook's launch for Pureplay's `shot`: ball speed, launch angle,
/// direction (positive right, as FRP) and spin. `None` without a ball
/// speed.
#[must_use]
pub fn launch(shot: &ShotLaunched) -> Option<Launch> {
    let speed = shot.ball_speed_ms.filter(|v| v.is_finite())?;
    #[allow(clippy::cast_possible_truncation)]
    let rpm = |v: f64| v.is_finite().then(|| v.round() as i32);
    Some(Launch {
        launch_speed: Some(Velocity::MetersPerSecond(speed)),
        launch_elevation: shot.launch_angle_deg.filter(|v| v.is_finite()),
        launch_azimuth: shot.direction_deg.filter(|v| v.is_finite()),
        backspin_rpm: shot.back_spin_rpm.and_then(rpm),
        sidespin_rpm: shot.side_spin_rpm.and_then(|v| rpm(SIDE_SPIN_SIGN * v)),
        ..Launch::default()
    })
}

/// The outcomes no launch stands behind: their stroke has no shot.
const NO_SHOT: [&str; 3] = ["Gimme", "AutoPutt", "PenaltyOnly"];

/// The `game.stroke` for Pureplay's `done`: `shot_id` is the launch
/// monitor's id of the shot it stands for (none for an outcome no launch
/// stands behind), `golfer` the player's id, `flown` the `shot.launched`
/// values, `replaces` the `shotId` of the shot it takes the place of. `None`
/// without a `shotId`, a hole and stroke number, or an outcome FRP has a
/// name for.
#[must_use]
pub fn stroke(
    done: &ShotCompleted,
    shot_id: Option<&ShotKey>,
    golfer: GolferId,
    flown: Option<&Launch>,
    replaces: Option<&str>,
) -> Option<GameStroke> {
    let stroke_id = done.shot_id.clone().filter(|id| !id.is_empty())?;
    let hole = done.hole_number?;
    let stroke_number = done.stroke_number?;
    let name = done.outcome.as_deref()?;
    let outcome = outcome_kind(name)?;
    let launched = !NO_SHOT.iter().any(|n| n.eq_ignore_ascii_case(name));
    let m = |v: Option<f64>| v.filter(|v| v.is_finite()).map(Distance::Meters);
    let flight =
        (done.carry_m.is_some() || done.total_m.is_some() || done.apex_m.is_some()).then(|| {
            FlightResult {
                model: Some(SOURCE.into()),
                carry_distance: m(done.carry_m),
                total_distance: m(done.total_m),
                max_height: m(done.apex_m),
                ..FlightResult::new(Basis::Modeled)
            }
        });
    let position = |p: Option<[f64; 3]>| {
        p.filter(|p| p.iter().all(|v| v.is_finite())).map(|p| {
            Position::new(
                Distance::Meters(p[0]),
                Distance::Meters(p[1]),
                Distance::Meters(p[2]),
            )
        })
    };
    Some(GameStroke {
        stroke_id,
        shot_id: shot_id.filter(|_| launched).map(|k| k.shot_id.clone()),
        replaces: replaces.map(str::to_owned),
        golfer,
        hole,
        stroke_number,
        flown: flown.filter(|_| launched).cloned(),
        flight: flight.filter(|_| launched),
        outcome,
        putt: done.is_putt,
        penalty_strokes: done.penalty_strokes.unwrap_or_default(),
        lie: done.lie.as_deref().map(|l| Lie {
            surface: Some(super::game::surface(l)),
            ..Lie::default()
        }),
        start_position: position(done.start),
        end_position: position(done.end),
        start_distance_to_hole: m(done.start_dist_to_hole_m),
        end_distance_to_hole: m(done.end_dist_to_hole_m),
    })
}

/// FRP's outcome for Pureplay's `outcome` name.
fn outcome_kind(name: &str) -> Option<StrokeOutcome> {
    const KINDS: [(&str, StrokeOutcome); 10] = [
        ("InPlay", StrokeOutcome::InPlay),
        ("Green", StrokeOutcome::Green),
        ("Holed", StrokeOutcome::Holed),
        ("OB", StrokeOutcome::OutOfBounds),
        ("Water", StrokeOutcome::Water),
        ("Lost", StrokeOutcome::Lost),
        ("Unplayable", StrokeOutcome::Unplayable),
        ("Gimme", StrokeOutcome::Gimme),
        ("AutoPutt", StrokeOutcome::AutoPutt),
        ("PenaltyOnly", StrokeOutcome::PenaltyOnly),
    ];
    KINDS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, k)| *k)
}

/// The `shot.launched` values of Pureplay's recent shots, by `shotId`, the
/// newest [`MAP_CAPACITY`] kept: what a `game.stroke` says was flown.
#[derive(Debug, Default)]
pub struct Flown {
    entries: VecDeque<(String, Launch)>,
}

impl Flown {
    /// Keep `launch` as what shot `shot_id` flew.
    pub fn insert(&mut self, shot_id: &str, launch: Launch) {
        self.entries.retain(|(id, _)| id != shot_id);
        if self.entries.len() >= MAP_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((shot_id.to_owned(), launch));
    }

    /// What shot `shot_id` flew.
    #[must_use]
    pub fn get(&self, shot_id: &str) -> Option<&Launch> {
        self.entries
            .iter()
            .find(|(id, _)| id == shot_id)
            .map(|(_, l)| l)
    }

    /// Forget every shot: Pureplay restarted.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// The `shotId`s Pureplay discarded, the newest [`MAP_CAPACITY`] kept.
#[derive(Debug, Default)]
pub struct Discarded {
    ids: VecDeque<String>,
}

impl Discarded {
    /// Keep `shot_id` as discarded.
    pub fn insert(&mut self, shot_id: &str) {
        if self.contains(shot_id) {
            return;
        }
        if self.ids.len() >= MAP_CAPACITY {
            self.ids.pop_front();
        }
        self.ids.push_back(shot_id.to_owned());
    }

    /// Whether `shot_id` was discarded.
    #[must_use]
    pub fn contains(&self, shot_id: &str) -> bool {
        self.ids.iter().any(|id| id == shot_id)
    }

    /// Forget every shot: Pureplay restarted.
    pub fn clear(&mut self) {
        self.ids.clear();
    }
}

/// Pureplay's `shotId` against the key of the flighthook shot it stands
/// for, the newest [`MAP_CAPACITY`] kept.
#[derive(Debug, Default)]
pub struct ShotMap {
    entries: VecDeque<(String, ShotKey)>,
}

impl ShotMap {
    /// Keep `shot_id` as standing for `key`.
    pub fn insert(&mut self, shot_id: &str, key: ShotKey) {
        self.entries.retain(|(id, _)| id != shot_id);
        if self.entries.len() >= MAP_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((shot_id.to_owned(), key));
    }

    /// The key `shot_id` stands for.
    #[must_use]
    pub fn get(&self, shot_id: &str) -> Option<&ShotKey> {
        self.entries
            .iter()
            .find(|(id, _)| id == shot_id)
            .map(|(_, k)| k)
    }

    /// Forget `shot_id`. Returns whether it was kept.
    pub fn remove(&mut self, shot_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(id, _)| id != shot_id);
        self.entries.len() != before
    }

    /// Forget every shot: Pureplay restarted.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> ShotKey {
        ShotKey::new(format!("k-{n}"), n)
    }

    /// The protocol's `shot.launched` example becomes flighthook's launch,
    /// direction kept (positive right) and spin rounded.
    #[test]
    fn the_launch_maps_to_flighthooks() {
        let shot: ShotLaunched = serde_json::from_str(
            r#"{ "shotId": "shot-42", "playerId": 1,
                 "ballSpeedMs": 61.2, "launchAngleDeg": 18.4, "directionDeg": -1.5,
                 "backSpinRpm": 5200.4, "sideSpinRpm": -240.6 }"#,
        )
        .expect("parse");
        let l = launch(&shot).expect("a launch");
        assert_eq!(l.launch_speed, Some(Velocity::MetersPerSecond(61.2)));
        assert_eq!(l.launch_elevation, Some(18.4));
        assert_eq!(l.launch_azimuth, Some(-1.5));
        assert_eq!(l.backspin_rpm, Some(5200));
        assert_eq!(l.sidespin_rpm, Some(-241));
        assert_eq!(l.total_spin_rpm, None);
        let no_speed = ShotLaunched {
            ball_speed_ms: None,
            ..shot
        };
        assert_eq!(launch(&no_speed), None);
    }

    /// The protocol's `shot.completed` example becomes a `game.stroke`, in
    /// meters, with the launch monitor's shot id; an outcome no launch
    /// stands behind has no shot id, flown values or flight.
    #[test]
    fn the_completed_shot_maps_to_its_stroke() {
        let done: ShotCompleted = serde_json::from_str(
            r#"{ "shotId": "shot-42", "matchId": "match-123", "playerId": 1,
                 "holeNumber": 7, "strokeNumber": 2,
                 "outcome": "Green", "penaltyStrokes": 0, "isPutt": false,
                 "carryM": 138.4, "totalM": 145.1, "apexM": 24.6,
                 "startDistToHoleM": 152.0, "endDistToHoleM": 7.4,
                 "start": [12.4, 88.1, 3.2], "end": [141.0, 92.7, 3.9],
                 "lie": "Green", "holeStrokes": 2 }"#,
        )
        .expect("parse");
        let golfer: GolferId = "pureplay.local-3".parse().expect("id");
        let flown = Launch {
            launch_speed: Some(Velocity::MetersPerSecond(61.2)),
            ..Launch::default()
        };
        let s = stroke(
            &done,
            Some(&key(5)),
            golfer.clone(),
            Some(&flown),
            Some("shot-41"),
        )
        .expect("a stroke");
        assert_eq!(s.stroke_id, "shot-42");
        assert_eq!(s.shot_id.as_deref(), Some("k-5"));
        assert_eq!(s.golfer, golfer);
        assert_eq!(s.replaces.as_deref(), Some("shot-41"));
        assert_eq!((s.hole, s.stroke_number), (7, 2));
        assert_eq!(s.outcome, StrokeOutcome::Green);
        assert_eq!(s.flown, Some(flown.clone()));
        let flight = s.flight.expect("flight");
        assert_eq!(flight.basis, Basis::Modeled);
        assert_eq!(flight.model.as_deref(), Some("Pureplay"));
        assert_eq!(flight.carry_distance, Some(Distance::Meters(138.4)));
        assert_eq!(flight.total_distance, Some(Distance::Meters(145.1)));
        assert_eq!(flight.max_height, Some(Distance::Meters(24.6)));
        assert_eq!(s.start_distance_to_hole, Some(Distance::Meters(152.0)));
        assert_eq!(s.end_distance_to_hole, Some(Distance::Meters(7.4)));
        assert_eq!(s.lie.and_then(|l| l.surface).as_deref(), Some("green"));
        assert_eq!((s.penalty_strokes, s.putt), (0, Some(false)));
        for (name, kind) in [
            ("InPlay", StrokeOutcome::InPlay),
            ("Holed", StrokeOutcome::Holed),
            ("OB", StrokeOutcome::OutOfBounds),
            ("Water", StrokeOutcome::Water),
            ("Lost", StrokeOutcome::Lost),
            ("Unplayable", StrokeOutcome::Unplayable),
        ] {
            let d = ShotCompleted {
                outcome: Some(name.into()),
                ..done.clone()
            };
            let s = stroke(&d, Some(&key(5)), golfer.clone(), None, None).expect(name);
            assert_eq!(s.outcome, kind, "{name}");
            assert!(s.shot_id.is_some(), "{name}");
        }
        for (name, kind) in [
            ("Gimme", StrokeOutcome::Gimme),
            ("AutoPutt", StrokeOutcome::AutoPutt),
            ("PenaltyOnly", StrokeOutcome::PenaltyOnly),
        ] {
            let d = ShotCompleted {
                outcome: Some(name.into()),
                ..done.clone()
            };
            let s = stroke(&d, Some(&key(5)), golfer.clone(), Some(&flown), None).expect(name);
            assert_eq!(s.outcome, kind, "{name}");
            assert_eq!((s.shot_id, s.flown, s.flight), (None, None, None), "{name}");
        }
        let unknown = ShotCompleted {
            outcome: Some("Bunker".into()),
            ..done.clone()
        };
        assert_eq!(stroke(&unknown, None, golfer.clone(), None, None), None);
        // No stroke without its hole and stroke number: no zeros.
        for d in [
            ShotCompleted {
                hole_number: None,
                ..done.clone()
            },
            ShotCompleted {
                stroke_number: None,
                ..done.clone()
            },
        ] {
            assert_eq!(stroke(&d, None, golfer.clone(), None, None), None);
        }
    }

    /// Auto takes what is no echo; From Pureplay takes everything without
    /// asking; From flighthook takes nothing but still matches echoes.
    #[test]
    fn the_shots_setting_decides() {
        use PureplayShots::{Auto, FromFlighthook, FromPureplay};
        assert_eq!(decide(Auto, || None), Decision::Take);
        assert_eq!(decide(Auto, || Some(key(1))), Decision::Echo(key(1)));
        assert_eq!(decide(FromFlighthook, || None), Decision::Ignore);
        assert_eq!(
            decide(FromFlighthook, || Some(key(2))),
            Decision::Echo(key(2))
        );
        let asked = std::cell::Cell::new(false);
        let d = decide(FromPureplay, || {
            asked.set(true);
            Some(key(3))
        });
        assert_eq!(d, Decision::Take);
        assert!(!asked.get(), "From Pureplay does not match echoes");
        assert!(matches_echoes(Auto) && matches_echoes(FromFlighthook));
        assert!(!matches_echoes(FromPureplay));
    }

    /// The map keeps the newest shots, a repeat replacing the earlier.
    #[test]
    fn the_shot_map_is_bounded() {
        let mut map = ShotMap::default();
        for n in 0..=u32::try_from(MAP_CAPACITY).unwrap() {
            map.insert(&format!("shot-{n}"), key(n));
        }
        assert_eq!(map.len(), MAP_CAPACITY);
        assert_eq!(map.get("shot-0"), None);
        assert_eq!(map.get("shot-1"), Some(&key(1)));
        map.insert("shot-1", key(99));
        assert_eq!(map.get("shot-1"), Some(&key(99)));
        assert_eq!(map.len(), MAP_CAPACITY);
        assert!(map.remove("shot-1"));
        assert!(!map.remove("shot-1"));
        map.clear();
        assert_eq!(map.len(), 0);
    }
}
