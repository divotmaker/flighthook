//! Shot data -> GSPro message conversion.

use super::api::{BallData, ClubData, GsProMessage, ShotDataOptions};
use crate::actors::simout::OutputShot;

/// Convert a decoded shot into a GSPro Open Connect V1 message.
///
/// FRP lateral signs are absolute: positive = right of the target line, for
/// left- and right-handed players alike. GSPro takes HLA, SideSpin,
/// FaceToTarget and Path with the same signs, so they are sent as they are,
/// whatever the player's handedness.
pub fn map_shot(shot: &OutputShot) -> GsProMessage {
    let contains_ball = shot.launch.is_some();

    // Ball data: the launch, zero-filling missing fields, and the device's
    // carry. A total spin with no axis goes out as backspin with no
    // sidespin.
    let (speed, vla, hla, carry_distance, bs, ss) = if let Some(ref b) = shot.launch {
        let (back, side) = flighthook::spin_components(b);
        (
            b.launch_speed.map(|v| v.as_mph()).unwrap_or(0.0),
            b.launch_elevation.unwrap_or(0.0),
            b.launch_azimuth.unwrap_or(0.0),
            shot.flight
                .as_ref()
                .and_then(|f| f.carry_distance)
                .map(|d| d.as_yards()),
            back.unwrap_or(0) as f64,
            side.unwrap_or(0) as f64,
        )
    } else {
        (0.0, 0.0, 0.0, None, 0.0, 0.0)
    };

    // Derive total_spin and spin_axis from backspin + sidespin.
    let total_spin = (bs * bs + ss * ss).sqrt();
    let spin_axis = ss.atan2(bs).to_degrees();

    // Face impact, mm. Not every monitor measures it; absent means zero.
    //
    // Use as_millimeters() (f64) and NOT DistanceExt::to_mm(), which returns
    // u16 — a saturating cast that would silently turn every toe or low strike
    // into 0.
    //
    // Toe/heel and high/low are golfer-relative, the same for either hand,
    // and are sent as they are.
    //
    // GSPro renders a face-impact diagram, so polarity is checkable end to end.
    // Any device-specific correction (e.g. the Square Omni's per-club vertical
    // offset) belongs in the device driver, not here.
    let (impact_v, impact_h) = shot.impact.as_ref().map_or((0.0, 0.0), |i| {
        (
            i.vertical.map_or(0.0, |d| d.as_millimeters()),
            i.lateral.map_or(0.0, |d| d.as_millimeters()),
        )
    });

    // Club data: use ClubData when present, otherwise zero-fill.
    let club = shot.club.as_ref();
    let club_data = ClubData {
        speed: club.and_then(|c| c.club_speed).map_or(0.0, |v| v.as_mph()),
        angle_of_attack: club.and_then(|c| c.attack_angle).unwrap_or(0.0),
        face_to_target: club.and_then(|c| c.face_angle).unwrap_or(0.0),
        loft: club.and_then(|c| c.dynamic_loft).unwrap_or(0.0),
        path: club.and_then(|c| c.path).unwrap_or(0.0),
        speed_at_impact: club
            .and_then(|c| c.club_speed_post)
            .map_or(0.0, |v| v.as_mph()),
        vertical_face_impact: impact_v,
        horizontal_face_impact: impact_h,
        ..ClubData::default()
    };

    GsProMessage {
        device_id: "Flighthook".into(),
        units: "Yards".into(),
        shot_number: shot.shot_number,
        api_version: "1".into(),
        ball_data: BallData {
            speed,
            spin_axis,
            total_spin,
            back_spin: bs,
            side_spin: ss,
            hla,
            vla,
            carry_distance,
        },
        club_data,
        shot_data_options: ShotDataOptions {
            contains_ball_data: contains_ball,
            // Always set, even when the shot carries no club data. GSPro keeps
            // showing the previous shot's club data and face impact when this
            // is false, so a shot without club tracking (e.g. a club with no
            // sticker on a Square Golf) would display the last tracked club's
            // numbers. Sending the zero-filled block clears them.
            contains_club_data: true,
            // A shot proves the device was ready and detected a ball.
            launch_monitor_is_ready: true,
            launch_monitor_ball_detected: true,
            is_heart_beat: false,
        },
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use flighthook::{ClubData as FhClubData, Distance, FaceImpact, Launch, Velocity};

    fn shot_with_impact(lateral_mm: f64, vertical_mm: f64) -> OutputShot {
        OutputShot {
            launch: Some(Launch {
                launch_speed: Some(Velocity::MetersPerSecond(3.30)),
                launch_elevation: Some(32.51),
                launch_azimuth: Some(7.39),
                backspin_rpm: Some(621),
                sidespin_rpm: Some(-87),
                ..Launch::default()
            }),
            club: Some(FhClubData {
                club_speed: Some(Velocity::MetersPerSecond(3.26)),
                club_speed_post: None,
                path: Some(4.45),
                attack_angle: Some(2.14),
                face_angle: Some(8.13),
                dynamic_loft: Some(40.73),
                smash_factor: Some(1.01),
                swing_plane_horizontal: None,
                swing_plane_vertical: None,
                club_offset: None,
                club_height: None,
                ..FhClubData::default()
            }),
            impact: Some(FaceImpact {
                lateral: Some(Distance::Millimeters(lateral_mm)),
                vertical: Some(Distance::Millimeters(vertical_mm)),
            }),
            actor: "square.0".into(),
            shot_number: 1,
            ..OutputShot::default()
        }
    }

    /// A Square-like full swing: launch data only, no carry, no club data.
    pub(in crate::actors::gspro) fn square_like_shot() -> OutputShot {
        OutputShot {
            actor: "square.0".into(),
            shot_number: 12,
            launch: Some(Launch {
                launch_speed: Some(Velocity::MetersPerSecond(53.6)),
                launch_elevation: Some(17.0),
                launch_azimuth: Some(-1.2),
                backspin_rpm: Some(6500),
                sidespin_rpm: Some(-300),
                ..Launch::default()
            }),
            club: None,
            impact: None,
            ..OutputShot::default()
        }
    }

    /// GSPro output for a launch-only shot: no `CarryDistance` appears.
    pub(in crate::actors::gspro) const GOLDEN_RH: &str = r#"{"DeviceID":"Flighthook","Units":"Yards","ShotNumber":12,"APIversion":"1","BallData":{"Speed":119.89998400000002,"SpinAxis":-2.642545294064724,"TotalSpin":6506.9193939989755,"BackSpin":6500.0,"SideSpin":-300.0,"HLA":-1.2,"VLA":17.0},"ClubData":{"Speed":0.0,"AngleOfAttack":0.0,"FaceToTarget":0.0,"Lie":0.0,"Loft":0.0,"Path":0.0,"SpeedAtImpact":0.0,"VerticalFaceImpact":0.0,"HorizontalFaceImpact":0.0,"ClosureRate":0.0},"ShotDataOptions":{"ContainsBallData":true,"ContainsClubData":true,"LaunchMonitorIsReady":true,"LaunchMonitorBallDetected":true,"IsHeartBeat":false}}"#;

    #[test]
    fn a_launch_only_shot_sends_no_carry() {
        let json = serde_json::to_string(&map_shot(&square_like_shot())).expect("serializes");
        assert_eq!(json, GOLDEN_RH);
    }

    #[test]
    fn the_devices_carry_is_sent() {
        let mut measured = square_like_shot();
        measured.flight = Some(flighthook::FlightResult {
            carry_distance: Some(Distance::Yards(171.0)),
            ..flighthook::FlightResult::new(flighthook::Basis::Modeled)
        });
        assert_eq!(map_shot(&measured).ball_data.carry_distance, Some(171.0));
    }

    #[test]
    fn face_impact_reaches_gspro_with_sign_intact() {
        // A toe-side, low strike. Both values are negative in FRP terms; a
        // saturating u16 conversion would silently report 0.0 for both.
        let msg = map_shot(&shot_with_impact(-32.19, -17.43));
        assert!((msg.club_data.horizontal_face_impact - -32.19).abs() < 1e-9);
        assert!((msg.club_data.vertical_face_impact - -17.43).abs() < 1e-9);
        assert!(msg.shot_data_options.contains_club_data);
    }

    #[test]
    fn shot_without_club_data_clears_gspro_club_display() {
        // A ball-only shot (no club tracking). GSPro holds the previous shot's
        // club data and face impact unless ContainsClubData is set, so the
        // zero-filled block must still be flagged as present.
        let mut shot = shot_with_impact(-32.19, -17.43);
        shot.club = None;
        shot.impact = None;
        let msg = map_shot(&shot);
        assert!(msg.shot_data_options.contains_ball_data);
        assert!(msg.shot_data_options.contains_club_data);
        assert_eq!(msg.club_data.speed, 0.0);
        assert_eq!(msg.club_data.path, 0.0);
        assert_eq!(msg.club_data.face_to_target, 0.0);
        assert_eq!(msg.club_data.vertical_face_impact, 0.0);
        assert_eq!(msg.club_data.horizontal_face_impact, 0.0);
    }

    /// Every lateral field goes out with the shot's own absolute sign, and
    /// face impact as it is.
    #[test]
    fn lateral_fields_and_face_impact_go_out_unchanged() {
        let msg = map_shot(&shot_with_impact(-32.19, -17.43));
        assert_eq!(msg.ball_data.hla, 7.39);
        assert_eq!(msg.ball_data.side_spin, -87.0);
        assert!(msg.ball_data.spin_axis < 0.0);
        assert_eq!(msg.club_data.face_to_target, 8.13);
        assert_eq!(msg.club_data.path, 4.45);
        assert_eq!(msg.club_data.horizontal_face_impact, -32.19);
        assert_eq!(msg.club_data.vertical_face_impact, -17.43);
    }

    #[test]
    fn serialized_json_carries_every_gspro_club_field() {
        let msg = map_shot(&shot_with_impact(-32.19, -17.43));
        let json = serde_json::to_string(&msg).expect("serializes");
        for field in [
            "Speed",
            "AngleOfAttack",
            "FaceToTarget",
            "Lie",
            "Loft",
            "Path",
            "SpeedAtImpact",
            "VerticalFaceImpact",
            "HorizontalFaceImpact",
            "ClosureRate",
        ] {
            assert!(json.contains(field), "ClubData missing {field} in {json}");
        }
    }

    /// A total spin with no axis and no components (the Xi's E8 total on a
    /// shot without club data) goes out as backspin with axis 0.
    #[test]
    fn a_total_spin_alone_goes_out_as_backspin() {
        let shot = OutputShot {
            actor: "mevo.0".into(),
            shot_number: 3,
            launch: Some(Launch {
                launch_speed: Some(Velocity::MetersPerSecond(60.0)),
                launch_elevation: Some(14.0),
                launch_azimuth: Some(1.0),
                total_spin_rpm: Some(6897),
                ..Launch::default()
            }),
            ..OutputShot::default()
        };
        let b = map_shot(&shot).ball_data;
        assert_eq!(
            (b.total_spin, b.back_spin, b.side_spin, b.spin_axis),
            (6897.0, 6897.0, 0.0, 0.0)
        );
    }

    /// A launch-only chip with the estimated backspin goes out as that total
    /// spin with axis 0 and no `CarryDistance`; a chip whose spin is 0 or
    /// absent goes out with 0.
    #[test]
    fn launch_only_chip_sends_its_backspin_with_axis_zero() {
        let chip = |back: Option<i32>, side: Option<i32>| OutputShot {
            actor: "mevo.0".into(),
            shot_number: 3,
            launch: Some(Launch {
                launch_speed: Some(Velocity::MetersPerSecond(7.019)),
                launch_elevation: Some(22.8),
                launch_azimuth: Some(-0.856),
                backspin_rpm: back,
                sidespin_rpm: side,
                ..Launch::default()
            }),
            ..OutputShot::default()
        };
        let b = map_shot(&chip(Some(1570), Some(0))).ball_data;
        assert_eq!(
            (b.total_spin, b.back_spin, b.side_spin),
            (1570.0, 1570.0, 0.0)
        );
        assert_eq!(b.spin_axis, 0.0);
        assert_eq!(b.carry_distance, None);
        for (back, side) in [(Some(0), Some(0)), (None, None)] {
            let b = map_shot(&chip(back, side)).ball_data;
            assert_eq!((b.total_spin, b.back_spin, b.side_spin), (0.0, 0.0, 0.0));
            assert_eq!(b.spin_axis, 0.0);
        }
        let json = serde_json::to_string(&map_shot(&chip(Some(1570), Some(0)))).expect("serialize");
        assert!(
            json.contains(r#""SpinAxis":0.0,"TotalSpin":1570.0,"BackSpin":1570.0,"SideSpin":0.0"#),
            "{json}"
        );
        assert!(!json.contains("CarryDistance"), "{json}");
    }
}
