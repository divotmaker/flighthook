//! Open Connect V1 -> flighthook bus types.
//!
//! The inverse of `gspro::mapper`. Open Connect always reports speeds in mph
//! and angles in degrees; distances follow the message's `Units` field, and
//! face impact is millimetres.

use super::super::gspro::api;
use flighthook::{BallFlight, ClubData, Distance, FaceImpact, Velocity};

/// Distance unit selected by the message's `Units` field.
///
/// GSPro Open Connect defines `"Yards"` and `"Meters"`. Anything unrecognised
/// (including the empty string, which some monitors send) falls back to yards,
/// matching what flighthook itself emits as a client.
fn distance(units: &str, value: f64) -> Distance {
    if units.eq_ignore_ascii_case("meters") || units.eq_ignore_ascii_case("metres") {
        Distance::Meters(value)
    } else {
        Distance::Yards(value)
    }
}

/// Resolve back/side spin components.
///
/// Monitors populate spin one of two ways: explicit `BackSpin`/`SideSpin`, or
/// `TotalSpin` plus a `SpinAxis` tilt in degrees. When the explicit components
/// are both absent (zero) but a total is present, decompose the total about the
/// axis. Positive `SpinAxis` tilts to the right, matching GSPro's convention
/// for a fade from a right-handed player.
fn spin(ball: &api::BallData) -> (i32, i32) {
    if ball.back_spin != 0.0 || ball.side_spin != 0.0 || ball.total_spin == 0.0 {
        return (ball.back_spin.round() as i32, ball.side_spin.round() as i32);
    }
    let axis = ball.spin_axis.to_radians();
    (
        (ball.total_spin * axis.cos()).round() as i32,
        (ball.total_spin * axis.sin()).round() as i32,
    )
}

/// Map the ball half of an Open Connect message.
pub fn map_ball(msg: &api::GsProMessage) -> BallFlight {
    let b = &msg.ball_data;
    let (backspin, sidespin) = spin(b);
    BallFlight {
        launch_speed: Some(Velocity::MilesPerHour(b.speed)),
        launch_elevation: Some(b.vla),
        launch_azimuth: Some(b.hla),
        carry_distance: b.carry_distance.map(|c| distance(&msg.units, c)),
        // Open Connect carries no total/apex/flight-time/roll fields.
        total_distance: None,
        max_height: None,
        flight_time: None,
        roll_distance: None,
        backspin_rpm: Some(backspin),
        sidespin_rpm: Some(sidespin),
    }
}

/// Map the club half of an Open Connect message.
///
/// `Lie` and `ClosureRate` have no flighthook equivalent and are dropped.
/// `smash_factor` is deliberately left `None` rather than derived — the bus
/// carries measured values, and Open Connect does not report it.
pub fn map_club(msg: &api::GsProMessage) -> ClubData {
    let c = &msg.club_data;
    ClubData {
        club_speed: Some(Velocity::MilesPerHour(c.speed)),
        path: Some(c.path),
        attack_angle: Some(c.angle_of_attack),
        face_angle: Some(c.face_to_target),
        dynamic_loft: Some(c.loft),
        smash_factor: None,
        club_speed_post: Some(Velocity::MilesPerHour(c.speed_at_impact)),
        swing_plane_horizontal: None,
        swing_plane_vertical: None,
        club_offset: None,
        club_height: None,
    }
}

/// Map face impact, if the message actually carries it.
///
/// Open Connect's `ClubData` is a fixed schema with no optionality, so an
/// unmeasured field arrives as `0.0` and is indistinguishable from a dead-centre
/// strike. Emit `FaceImpact` only when at least one axis is non-zero; a shot
/// that really was struck dead centre simply loses a data point rather than
/// every non-reporting monitor claiming perfect centre contact.
///
/// Face impact is *not* handedness-flipped: toe/heel and high/low are already
/// golfer-relative, unlike the target-relative HLA/SideSpin/FaceToTarget/Path.
pub fn map_face_impact(msg: &api::GsProMessage) -> Option<FaceImpact> {
    let c = &msg.club_data;
    if c.horizontal_face_impact == 0.0 && c.vertical_face_impact == 0.0 {
        return None;
    }
    Some(FaceImpact {
        lateral: Some(Distance::Millimeters(c.horizontal_face_impact)),
        vertical: Some(Distance::Millimeters(c.vertical_face_impact)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> api::GsProMessage {
        api::GsProMessage {
            device_id: "TestLM".into(),
            units: "Yards".into(),
            shot_number: 1,
            api_version: "1".into(),
            ball_data: api::BallData::default(),
            club_data: api::ClubData::default(),
            shot_data_options: api::ShotDataOptions::default(),
        }
    }

    #[test]
    fn explicit_spin_components_pass_through() {
        let mut m = msg();
        m.ball_data.back_spin = 3000.0;
        m.ball_data.side_spin = -450.0;
        m.ball_data.total_spin = 3033.0;
        m.ball_data.spin_axis = -8.5;
        let ball = map_ball(&m);
        assert_eq!(ball.backspin_rpm, Some(3000));
        assert_eq!(ball.sidespin_rpm, Some(-450));
    }

    #[test]
    fn total_and_axis_decompose_when_components_absent() {
        let mut m = msg();
        m.ball_data.total_spin = 5000.0;
        m.ball_data.spin_axis = 30.0;
        let ball = map_ball(&m);
        assert_eq!(ball.backspin_rpm, Some(4330)); // 5000 * cos(30°)
        assert_eq!(ball.sidespin_rpm, Some(2500)); // 5000 * sin(30°)
    }

    #[test]
    fn zero_spin_stays_zero() {
        let ball = map_ball(&msg());
        assert_eq!(ball.backspin_rpm, Some(0));
        assert_eq!(ball.sidespin_rpm, Some(0));
    }

    #[test]
    fn units_field_selects_carry_unit() {
        let mut m = msg();
        m.ball_data.carry_distance = Some(200.0);
        assert_eq!(map_ball(&m).carry_distance, Some(Distance::Yards(200.0)));
        m.units = "Meters".into();
        assert_eq!(map_ball(&m).carry_distance, Some(Distance::Meters(200.0)));
        m.units = String::new();
        assert_eq!(map_ball(&m).carry_distance, Some(Distance::Yards(200.0)));
    }

    #[test]
    fn face_impact_absent_when_both_axes_zero() {
        assert!(map_face_impact(&msg()).is_none());
    }

    #[test]
    fn face_impact_keeps_negative_millimetres() {
        let mut m = msg();
        m.club_data.horizontal_face_impact = -6.5;
        m.club_data.vertical_face_impact = 0.0;
        let fi = map_face_impact(&m).expect("impact present");
        assert_eq!(fi.lateral, Some(Distance::Millimeters(-6.5)));
        assert_eq!(fi.vertical, Some(Distance::Millimeters(0.0)));
    }

    #[test]
    fn speeds_are_mph() {
        let mut m = msg();
        m.ball_data.speed = 150.0;
        m.club_data.speed = 100.0;
        assert_eq!(
            map_ball(&m).launch_speed,
            Some(Velocity::MilesPerHour(150.0))
        );
        assert_eq!(map_club(&m).club_speed, Some(Velocity::MilesPerHour(100.0)));
    }
}
