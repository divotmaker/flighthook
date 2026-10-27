//! Shot metrics computed from the measured values.
//!
//! The formulas are the Flight Relay Protocol's "Derived Values" and its spin
//! relations: exact arithmetic on FRP fields, computed once here so every
//! consumer gets the same numbers with the same signs.

use serde::{Deserialize, Serialize};

use flightrelay::types::{BallFlight, ClubData};
use flightrelay::units::Distance;

/// Values flighthook computes from the measured [`BallFlight`] and
/// [`ClubData`] of a shot.
///
/// Each field is present only when every input it needs was measured, and
/// the spin values and spin loft only when the device did not report them
/// itself. A value under `ball` or `club` was sent by the device actor: the
/// device's reading, or for Open Connect the spin pair completed from the
/// one the monitor sent. A value here is computed from those. Nothing here
/// comes from the simulation
/// ([`DerivedFlight`](crate::DerivedFlight)). Lateral values are absolute:
/// positive = right of the target line, as in `launch_azimuth`.
///
/// Non-exhaustive so fields can be added; build one with
/// [`ShotMetrics::compute`], or `ShotMetrics::default()` and field
/// assignment.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ShotMetrics {
    /// Total spin, `√(backspin_rpm² + sidespin_rpm²)`, rounded. Only when the
    /// device reported both components and neither `total_spin_rpm` nor
    /// `spin_axis`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_spin_rpm: Option<i32>,
    /// Spin axis, `atan2(sidespin_rpm, backspin_rpm)`, degrees. Positive =
    /// tilted right (curves the ball right). Same conditions as
    /// `total_spin_rpm`, and absent when the total is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spin_axis: Option<f64>,
    /// `face_angle − path`, degrees. Positive = face pointing right of the
    /// club path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_to_path: Option<f64>,
    /// Approximate spin loft, `dynamic_loft − attack_angle`, degrees. Only
    /// when the device did not report `spin_loft`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spin_loft: Option<f64>,
    /// `carry_offline − carry_distance × tan(launch_azimuth)`: how far the
    /// ball curved off its start line by landing. Positive = curved right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<Distance>,
}

impl ShotMetrics {
    /// Compute the metrics whose inputs `ball` and `club` carry. `None` when
    /// there are none.
    #[must_use]
    pub fn compute(ball: Option<&BallFlight>, club: Option<&ClubData>) -> Option<Self> {
        let mut m = Self::default();
        if let Some(ball) = ball {
            (m.total_spin_rpm, m.spin_axis) = spin(ball);
            m.curve = curve(ball);
        }
        if let Some(club) = club {
            m.face_to_path = face_to_path(club);
            m.spin_loft = spin_loft(club);
        }
        (!m.is_empty()).then_some(m)
    }

    /// True when no field is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Convert `curve` to yards.
    #[must_use]
    pub fn to_imperial(&self) -> ShotMetrics {
        ShotMetrics {
            curve: self.curve.map(|d| Distance::Yards(d.as_yards())),
            ..self.clone()
        }
    }

    /// Convert `curve` to metres.
    #[must_use]
    pub fn to_metric(&self) -> ShotMetrics {
        ShotMetrics {
            curve: self.curve.map(|d| Distance::Meters(d.as_meters())),
            ..self.clone()
        }
    }
}

/// Finite value, else `None`.
fn finite(v: Option<f64>) -> Option<f64> {
    v.filter(|v| v.is_finite())
}

/// Total spin and axis from the spin components, when the device reported
/// both components and neither total nor axis (FRP's spin relations, via
/// [`BallFlight::complete_spin`]).
fn spin(ball: &BallFlight) -> (Option<i32>, Option<f64>) {
    if ball.total_spin_rpm.is_some() || ball.spin_axis.is_some() {
        return (None, None);
    }
    let mut spin = BallFlight {
        backspin_rpm: ball.backspin_rpm,
        sidespin_rpm: ball.sidespin_rpm,
        ..BallFlight::default()
    };
    spin.complete_spin();
    (spin.total_spin_rpm, finite(spin.spin_axis))
}

/// `face_angle − path`.
fn face_to_path(club: &ClubData) -> Option<f64> {
    Some(finite(club.face_angle)? - finite(club.path)?)
}

/// `dynamic_loft − attack_angle`, unless the device measured spin loft.
fn spin_loft(club: &ClubData) -> Option<f64> {
    if club.spin_loft.is_some() {
        return None;
    }
    Some(finite(club.dynamic_loft)? - finite(club.attack_angle)?)
}

/// `carry_offline − carry_distance × tan(launch_azimuth)`, in metres.
fn curve(ball: &BallFlight) -> Option<Distance> {
    let offline = finite(ball.carry_offline.map(Distance::as_meters))?;
    let carry = finite(ball.carry_distance.map(Distance::as_meters))?;
    let azimuth = finite(ball.launch_azimuth).filter(|a| a.abs() < 90.0)?;
    let curve = offline - carry * azimuth.to_radians().tan();
    curve.is_finite().then_some(Distance::Meters(curve))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn components(back: Option<i32>, side: Option<i32>) -> BallFlight {
        BallFlight {
            backspin_rpm: back,
            sidespin_rpm: side,
            ..BallFlight::default()
        }
    }

    #[test]
    fn spin_from_components_signs() {
        // Rightward sidespin tilts the axis right (+), leftward left (−).
        let m = ShotMetrics::compute(Some(&components(Some(3200), Some(450))), None).unwrap();
        assert_eq!(m.total_spin_rpm, Some(3231));
        let axis = m.spin_axis.unwrap();
        assert!((axis - 8.0047).abs() < 1e-3, "{axis}");

        let m = ShotMetrics::compute(Some(&components(Some(3200), Some(-450))), None).unwrap();
        assert_eq!(m.total_spin_rpm, Some(3231));
        let axis = m.spin_axis.unwrap();
        assert!((axis + 8.0047).abs() < 1e-3, "{axis}");
    }

    #[test]
    fn spin_omitted_unless_both_components() {
        assert!(ShotMetrics::compute(Some(&components(Some(3200), None)), None).is_none());
        assert!(ShotMetrics::compute(Some(&components(None, Some(-450))), None).is_none());
        assert!(ShotMetrics::compute(Some(&components(None, None)), None).is_none());
    }

    #[test]
    fn spin_omitted_when_device_reported_total_or_axis() {
        let mut ball = components(Some(3200), Some(-450));
        ball.total_spin_rpm = Some(3231);
        ball.spin_axis = Some(-8.0);
        assert!(ShotMetrics::compute(Some(&ball), None).is_none());
        // A total alone still counts as reported: no second total is made up.
        ball.spin_axis = None;
        assert!(ShotMetrics::compute(Some(&ball), None).is_none());
    }

    #[test]
    fn zero_spin_has_a_total_and_no_axis() {
        let m = ShotMetrics::compute(Some(&components(Some(0), Some(0))), None).unwrap();
        assert_eq!(m.total_spin_rpm, Some(0));
        assert_eq!(m.spin_axis, None);
    }

    fn club(face: Option<f64>, path: Option<f64>) -> ClubData {
        ClubData {
            face_angle: face,
            path,
            ..ClubData::default()
        }
    }

    #[test]
    fn face_to_path_signs() {
        // Face open to an in-to-out path: face right of path.
        let m = ShotMetrics::compute(None, Some(&club(Some(1.2), Some(-2.1)))).unwrap();
        assert!(close(m.face_to_path.unwrap(), 3.3));
        // Face closed to the path: face left of path.
        let m = ShotMetrics::compute(None, Some(&club(Some(-1.0), Some(2.5)))).unwrap();
        assert!(close(m.face_to_path.unwrap(), -3.5));
    }

    #[test]
    fn face_to_path_omitted_without_both_inputs() {
        assert!(ShotMetrics::compute(None, Some(&club(Some(1.2), None))).is_none());
        assert!(ShotMetrics::compute(None, Some(&club(None, Some(-2.1)))).is_none());
        assert!(ShotMetrics::compute(None, Some(&club(Some(f64::NAN), Some(1.0)))).is_none());
    }

    fn lofts(dynamic: Option<f64>, attack: Option<f64>) -> ClubData {
        ClubData {
            dynamic_loft: dynamic,
            attack_angle: attack,
            ..ClubData::default()
        }
    }

    #[test]
    fn spin_loft_approximation() {
        // Hitting down adds to the spin loft, hitting up takes from it.
        let m = ShotMetrics::compute(None, Some(&lofts(Some(18.4), Some(-3.5)))).unwrap();
        assert!(close(m.spin_loft.unwrap(), 21.9));
        let m = ShotMetrics::compute(None, Some(&lofts(Some(12.0), Some(4.0)))).unwrap();
        assert!(close(m.spin_loft.unwrap(), 8.0));
    }

    #[test]
    fn spin_loft_omitted_when_measured_or_inputs_missing() {
        let mut c = lofts(Some(18.4), Some(-3.5));
        c.spin_loft = Some(22.1);
        assert!(ShotMetrics::compute(None, Some(&c)).is_none());
        assert!(ShotMetrics::compute(None, Some(&lofts(Some(18.4), None))).is_none());
        assert!(ShotMetrics::compute(None, Some(&lofts(None, Some(-3.5)))).is_none());
    }

    fn flight(offline: Option<f64>, carry: Option<f64>, azimuth: Option<f64>) -> BallFlight {
        BallFlight {
            carry_offline: offline.map(Distance::Meters),
            carry_distance: carry.map(Distance::Meters),
            launch_azimuth: azimuth,
            ..BallFlight::default()
        }
    }

    fn curve_m(ball: &BallFlight) -> Option<f64> {
        ShotMetrics::compute(Some(ball), None)?
            .curve
            .map(Distance::as_meters)
    }

    #[test]
    fn curve_signs() {
        let straight = 100.0 * 5.0_f64.to_radians().tan();
        // Started right, landed further right: curved right.
        let c = curve_m(&flight(Some(12.0), Some(100.0), Some(5.0))).unwrap();
        assert!(close(c, 12.0 - straight) && c > 0.0, "{c}");
        // Started right, landed left of the start line: curved left.
        let c = curve_m(&flight(Some(2.0), Some(100.0), Some(5.0))).unwrap();
        assert!(close(c, 2.0 - straight) && c < 0.0, "{c}");
        // Started left, landed right of the target line: curved right.
        let c = curve_m(&flight(Some(3.0), Some(100.0), Some(-5.0))).unwrap();
        assert!(close(c, 3.0 + straight) && c > 0.0, "{c}");
        // Straight down the start line: no curve.
        let c = curve_m(&flight(Some(straight), Some(100.0), Some(5.0))).unwrap();
        assert!(c.abs() < 1e-9, "{c}");
    }

    #[test]
    fn curve_converts_units_first() {
        // 5 yd offline on a 100 m carry launched straight: curve 5 yd = 4.572 m.
        let ball = BallFlight {
            carry_offline: Some(Distance::Yards(5.0)),
            carry_distance: Some(Distance::Meters(100.0)),
            launch_azimuth: Some(0.0),
            ..BallFlight::default()
        };
        assert!(close(curve_m(&ball).unwrap(), 4.572));
    }

    #[test]
    fn curve_omitted_without_every_input() {
        assert_eq!(curve_m(&flight(None, Some(100.0), Some(5.0))), None);
        assert_eq!(curve_m(&flight(Some(12.0), None, Some(5.0))), None);
        assert_eq!(curve_m(&flight(Some(12.0), Some(100.0), None)), None);
        assert_eq!(curve_m(&flight(Some(12.0), Some(100.0), Some(90.0))), None);
    }

    #[test]
    fn nothing_to_compute_is_none() {
        assert!(ShotMetrics::compute(None, None).is_none());
        assert!(ShotMetrics::compute(Some(&BallFlight::default()), None).is_none());
        assert!(ShotMetrics::compute(None, Some(&ClubData::default())).is_none());
    }

    #[test]
    fn serialises_only_present_fields() {
        let m = ShotMetrics::compute(
            Some(&components(Some(3200), Some(-450))),
            Some(&club(Some(1.5), Some(-1.0))),
        )
        .unwrap();
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["total_spin_rpm"], 3231);
        assert_eq!(json["face_to_path"], 2.5);
        assert!(json.get("spin_loft").is_none(), "{json}");
        assert!(json.get("curve").is_none(), "{json}");
        let back: ShotMetrics = serde_json::from_value(json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn unit_conversion_touches_only_curve() {
        let m = ShotMetrics {
            curve: Some(Distance::Meters(9.144)),
            face_to_path: Some(2.0),
            ..ShotMetrics::default()
        };
        let imperial = m.to_imperial();
        assert!(close(imperial.curve.unwrap().value(), 10.0));
        assert!(matches!(imperial.curve, Some(Distance::Yards(_))));
        assert_eq!(imperial.face_to_path, Some(2.0));
        assert!(matches!(
            imperial.to_metric().curve,
            Some(Distance::Meters(_))
        ));
    }
}
