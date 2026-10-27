//! Open Connect V1 -> flighthook bus types.
//!
//! The inverse of `gspro::mapper`. Open Connect always reports speeds in mph
//! and angles in degrees; distances follow the message's `Units` field, and
//! face impact is millimetres. Open Connect values are golf-semantic
//! (golfer-relative, like GSPro); they are converted to FRP's absolute
//! physical signs for left-handed players, the inverse of
//! `gspro::mapper::map_shot`.

use super::super::gspro::api;
use flighthook::{BallFlight, ClubData, Distance, FaceImpact, Handedness, Velocity};

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

/// The spin pair the monitor populated, golf-semantic: `(backspin,
/// sidespin)` components or `(total, axis)`.
enum Spin {
    Components(i32, i32),
    TotalAndAxis(f64, f64),
}

/// Pick the spin pair the message carries.
///
/// Monitors populate spin one of two ways: explicit `BackSpin`/`SideSpin`, or
/// `TotalSpin` plus a `SpinAxis` tilt in degrees. Open Connect has no
/// optional fields, so an unpopulated pair reads 0: the components are used
/// when either is non-zero or the total is 0, else the total and axis.
/// Positive `SpinAxis` tilts to the right, matching GSPro's convention for a
/// fade from a right-handed player.
fn spin(ball: &api::BallData) -> Spin {
    if ball.back_spin != 0.0 || ball.side_spin != 0.0 || ball.total_spin == 0.0 {
        Spin::Components(ball.back_spin.round() as i32, ball.side_spin.round() as i32)
    } else {
        Spin::TotalAndAxis(ball.total_spin, ball.spin_axis)
    }
}

/// Map the ball half of an Open Connect message.
///
/// Open Connect reports HLA, SideSpin and SpinAxis golf-semantically
/// (golfer-relative), like GSPro. For a left-handed player, flip them to
/// FRP's absolute physical signs (positive = right of target) — the inverse
/// of the flip `gspro::mapper::map_shot` applies on the way out.
///
/// Both FRP spin pairs are sent: the pair the monitor populated, and the
/// other one computed from it. A total and axis are decomposed from the
/// unrounded total; components are completed with
/// [`BallFlight::complete_spin`].
pub fn map_ball(msg: &api::GsProMessage, handed: Handedness) -> BallFlight {
    let flip = if handed == Handedness::Left {
        -1.0
    } else {
        1.0
    };
    let b = &msg.ball_data;
    let mut ball = BallFlight {
        launch_speed: Some(Velocity::MilesPerHour(b.speed)),
        launch_elevation: Some(b.vla),
        launch_azimuth: Some(b.hla * flip),
        carry_distance: b.carry_distance.map(|c| distance(&msg.units, c)),
        // Open Connect carries no total/apex/flight-time/roll fields.
        ..BallFlight::default()
    };
    match spin(b) {
        Spin::Components(back, side) => {
            ball.backspin_rpm = Some(back);
            ball.sidespin_rpm = Some((side as f64 * flip).round() as i32);
            ball.complete_spin();
        }
        Spin::TotalAndAxis(total, axis) => {
            let rad = axis.to_radians();
            let side = (total * rad.sin()).round() as i32;
            ball.backspin_rpm = Some((total * rad.cos()).round() as i32);
            ball.sidespin_rpm = Some((side as f64 * flip).round() as i32);
            ball.total_spin_rpm = Some(total.round() as i32);
            ball.spin_axis = Some(axis * flip);
        }
    }
    ball
}

/// Map the club half of an Open Connect message.
///
/// `Lie` and `ClosureRate` have no flighthook equivalent and are dropped.
/// `smash_factor` is deliberately left `None` rather than derived — the bus
/// carries measured values, and Open Connect does not report it.
///
/// FaceToTarget and Path are golf-semantic (golfer-relative), like GSPro. For
/// a left-handed player, flip them to FRP's absolute physical signs — the
/// inverse of the flip `gspro::mapper::map_shot` applies on the way out.
pub fn map_club(msg: &api::GsProMessage, handed: Handedness) -> ClubData {
    let flip = if handed == Handedness::Left {
        -1.0
    } else {
        1.0
    };
    let c = &msg.club_data;
    ClubData {
        club_speed: Some(Velocity::MilesPerHour(c.speed)),
        path: Some(c.path * flip),
        attack_angle: Some(c.angle_of_attack),
        face_angle: Some(c.face_to_target * flip),
        dynamic_loft: Some(c.loft),
        smash_factor: None,
        club_speed_post: Some(Velocity::MilesPerHour(c.speed_at_impact)),
        swing_plane_horizontal: None,
        swing_plane_vertical: None,
        club_offset: None,
        club_height: None,
        ..ClubData::default()
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
/// golfer-relative, unlike the golf-semantic, handedness-dependent
/// HLA/SideSpin/FaceToTarget/Path.
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
    use flighthook::ShotData;

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
        let ball = map_ball(&m, Handedness::Right);
        assert_eq!(ball.backspin_rpm, Some(3000));
        assert_eq!(ball.sidespin_rpm, Some(-450));
        // Total and axis are completed from the components.
        assert_eq!(ball.total_spin_rpm, Some(3034)); // √(3000² + 450²)
        let axis = ball.spin_axis.expect("axis");
        assert!((axis - -8.53).abs() < 0.01, "{axis}"); // atan2(-450, 3000)
    }

    #[test]
    fn total_and_axis_decompose_when_components_absent() {
        let mut m = msg();
        m.ball_data.total_spin = 5000.0;
        m.ball_data.spin_axis = 30.0;
        let ball = map_ball(&m, Handedness::Right);
        assert_eq!(ball.backspin_rpm, Some(4330)); // 5000 * cos(30°)
        assert_eq!(ball.sidespin_rpm, Some(2500)); // 5000 * sin(30°)
        assert_eq!(ball.total_spin_rpm, Some(5000));
        assert_eq!(ball.spin_axis, Some(30.0));
    }

    #[test]
    fn zero_spin_stays_zero() {
        let ball = map_ball(&msg(), Handedness::Right);
        assert_eq!(ball.backspin_rpm, Some(0));
        assert_eq!(ball.sidespin_rpm, Some(0));
        assert_eq!(ball.total_spin_rpm, Some(0));
        assert_eq!(ball.spin_axis, None);
    }

    #[test]
    fn units_field_selects_carry_unit() {
        let mut m = msg();
        m.ball_data.carry_distance = Some(200.0);
        assert_eq!(
            map_ball(&m, Handedness::Right).carry_distance,
            Some(Distance::Yards(200.0))
        );
        m.units = "Meters".into();
        assert_eq!(
            map_ball(&m, Handedness::Right).carry_distance,
            Some(Distance::Meters(200.0))
        );
        m.units = String::new();
        assert_eq!(
            map_ball(&m, Handedness::Right).carry_distance,
            Some(Distance::Yards(200.0))
        );
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
            map_ball(&m, Handedness::Right).launch_speed,
            Some(Velocity::MilesPerHour(150.0))
        );
        assert_eq!(
            map_club(&m, Handedness::Right).club_speed,
            Some(Velocity::MilesPerHour(100.0))
        );
    }

    /// Build a message with non-zero lateral fields in every ingested slot:
    /// HLA, explicit back/side spin, FaceToTarget, and Path.
    fn lateral_msg() -> api::GsProMessage {
        let mut m = msg();
        m.ball_data.hla = 4.2;
        m.ball_data.back_spin = 2800.0;
        m.ball_data.side_spin = -650.0;
        m.ball_data.total_spin = 2878.5;
        m.ball_data.spin_axis = -13.05;
        m.club_data.face_to_target = 2.1;
        m.club_data.path = -3.4;
        m
    }

    #[test]
    fn left_handed_ingest_flips_lateral_fields_only() {
        let m = lateral_msg();
        let rh_ball = map_ball(&m, Handedness::Right);
        let lh_ball = map_ball(&m, Handedness::Left);
        assert_eq!(lh_ball.launch_azimuth, rh_ball.launch_azimuth.map(|v| -v));
        assert_eq!(lh_ball.sidespin_rpm, rh_ball.sidespin_rpm.map(|v| -v));
        assert_eq!(lh_ball.backspin_rpm, rh_ball.backspin_rpm);
        assert_eq!(lh_ball.launch_elevation, rh_ball.launch_elevation);
        assert_eq!(lh_ball.launch_speed, rh_ball.launch_speed);
        assert_eq!(lh_ball.carry_distance, rh_ball.carry_distance);

        let rh_club = map_club(&m, Handedness::Right);
        let lh_club = map_club(&m, Handedness::Left);
        assert_eq!(lh_club.face_angle, rh_club.face_angle.map(|v| -v));
        assert_eq!(lh_club.path, rh_club.path.map(|v| -v));
    }

    #[test]
    fn left_handed_ingest_flips_total_spin_decomposition() {
        let mut m = msg();
        m.ball_data.total_spin = 5000.0;
        m.ball_data.spin_axis = 30.0;
        let rh = map_ball(&m, Handedness::Right);
        let lh = map_ball(&m, Handedness::Left);
        assert_eq!(lh.backspin_rpm, rh.backspin_rpm);
        assert_eq!(lh.sidespin_rpm, rh.sidespin_rpm.map(|v| -v));
        assert_eq!(lh.total_spin_rpm, rh.total_spin_rpm);
        assert_eq!(lh.spin_axis, rh.spin_axis.map(|v| -v));
        assert_eq!(rh.spin_axis, Some(30.0));
    }

    #[test]
    fn left_handed_ingest_flips_the_completed_axis() {
        let m = lateral_msg();
        let rh = map_ball(&m, Handedness::Right);
        let lh = map_ball(&m, Handedness::Left);
        assert_eq!(lh.total_spin_rpm, rh.total_spin_rpm);
        assert!(rh.spin_axis.expect("axis") < 0.0);
        assert_eq!(lh.spin_axis, rh.spin_axis.map(|v| -v));
    }

    /// A fractional total is decomposed unrounded, so the components and
    /// the GSPro bridge's output match a decomposition of the f64 total.
    #[test]
    fn fractional_total_decomposes_unrounded() {
        let mut m = msg();
        m.ball_data.total_spin = 3250.5;
        m.ball_data.spin_axis = 10.0;
        for (handed, flip) in [(Handedness::Right, 1), (Handedness::Left, -1)] {
            let ball = map_ball(&m, handed);
            // 3250.5·cos(10°) = 3201.12, 3250.5·sin(10°) = 564.45.
            assert_eq!(ball.backspin_rpm, Some(3201), "{handed:?}");
            assert_eq!(ball.sidespin_rpm, Some(564 * flip), "{handed:?}");
            assert_eq!(ball.total_spin_rpm, Some(3251), "{handed:?}");
            assert_eq!(ball.spin_axis, Some(10.0 * f64::from(flip)), "{handed:?}");

            let shot = ShotData {
                actor: "openconnect_server.0".into(),
                ball: Some(ball),
                ..ShotData::default()
            };
            let out = super::super::super::gspro::mapper::map_shot(&shot, handed);
            let (bs, ss) = (3201.0_f64, 564.0_f64);
            assert_eq!(out.ball_data.back_spin, bs, "{handed:?}");
            assert_eq!(out.ball_data.side_spin, ss, "{handed:?}");
            assert_eq!(
                out.ball_data.total_spin,
                (bs * bs + ss * ss).sqrt(),
                "{handed:?}"
            );
            assert_eq!(
                out.ball_data.spin_axis,
                ss.atan2(bs).to_degrees(),
                "{handed:?}"
            );
            assert!((out.ball_data.total_spin - 3250.31).abs() < 0.01);
            assert!((out.ball_data.spin_axis - 9.9927).abs() < 1e-4);
        }
    }

    /// Ingest (`map_ball`/`map_club`) is the exact inverse of
    /// `gspro::mapper::map_shot`: for either handedness, running a message
    /// through both must reproduce the original golf-semantic values.
    #[test]
    fn ingest_then_gspro_map_shot_round_trips_for_both_handedness() {
        let m = lateral_msg();
        for handed in [Handedness::Right, Handedness::Left] {
            let ball = map_ball(&m, handed);
            let club = map_club(&m, handed);
            let shot = ShotData {
                actor: "openconnect_server.0".into(),
                shot_number: m.shot_number,
                ball: Some(ball),
                club: Some(club),
                impact: None,
                derived: None,
                ..ShotData::default()
            };
            let out = super::super::super::gspro::mapper::map_shot(&shot, handed);
            let eps = 1e-9;
            assert!(
                (out.ball_data.hla - m.ball_data.hla).abs() < eps,
                "{handed:?} hla"
            );
            assert!(
                (out.ball_data.side_spin - m.ball_data.side_spin).abs() < eps,
                "{handed:?} side_spin"
            );
            assert!(
                (out.club_data.face_to_target - m.club_data.face_to_target).abs() < eps,
                "{handed:?} face_to_target"
            );
            assert!(
                (out.club_data.path - m.club_data.path).abs() < eps,
                "{handed:?} path"
            );
        }
    }
}
