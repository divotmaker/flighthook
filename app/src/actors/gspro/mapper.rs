//! Shot data -> GSPro message conversion.

use super::api::{BallData, ClubData, GsProMessage, ShotDataOptions};
use flighthook::{Handedness, ShotData};

/// Convert a decoded shot into a GSPro Open Connect V1 message.
///
/// FRP uses absolute physical signs (positive = right of target). GSPro uses
/// golf-semantic signs (positive = in-to-out / open *for that player*). For
/// left-handed players, lateral fields are negated so the sim sees the shot
/// from the golfer's perspective.
pub fn map_shot(shot: &ShotData, handed: Handedness) -> GsProMessage {
    let contains_ball = shot.ball.is_some();
    // LH flip: GSPro expects golf-semantic signs, FRP uses absolute physical.
    let flip = if handed == Handedness::Left {
        -1.0
    } else {
        1.0
    };

    // Ball data: extract from Option<BallFlight>, zero-fill missing fields.
    let (speed, vla, hla, carry_distance, bs, ss) = if let Some(ref b) = shot.ball {
        (
            b.launch_speed.map(|v| v.as_mph()).unwrap_or(0.0),
            b.launch_elevation.unwrap_or(0.0),
            b.launch_azimuth.unwrap_or(0.0) * flip,
            b.carry_distance.map(|d| d.as_yards()),
            b.backspin_rpm.unwrap_or(0) as f64,
            b.sidespin_rpm.unwrap_or(0) as f64 * flip,
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
    // No handedness flip: toe/heel and high/low are already golfer-relative,
    // unlike the target-relative fields above.
    //
    // Polarity is device-verified (negative = toe / low) but not GSPro-verified:
    // GSPro shows these as numeric stats with no face diagram. If a toe strike
    // ever reads heel-side, negate impact_h here.
    let (impact_v, impact_h) = shot.impact.as_ref().map_or((0.0, 0.0), |i| {
        (
            i.vertical.map_or(0.0, |d| d.as_millimeters()),
            i.lateral.map_or(0.0, |d| d.as_millimeters()),
        )
    });

    // Club data: use ClubData when present, otherwise zero-fill.
    let (club_data, contains_club) = if let Some(ref club) = shot.club {
        (
            ClubData {
                speed: club.club_speed.map(|v| v.as_mph()).unwrap_or(0.0),
                angle_of_attack: club.attack_angle.unwrap_or(0.0),
                face_to_target: club.face_angle.unwrap_or(0.0) * flip,
                loft: club.dynamic_loft.unwrap_or(0.0),
                path: club.path.unwrap_or(0.0) * flip,
                speed_at_impact: club.club_speed_post.map_or(0.0, |v| v.as_mph()),
                vertical_face_impact: impact_v,
                horizontal_face_impact: impact_h,
                ..ClubData::default()
            },
            true,
        )
    } else {
        (
            ClubData {
                speed: 0.0,
                angle_of_attack: 0.0,
                face_to_target: 0.0,
                vertical_face_impact: impact_v,
                horizontal_face_impact: impact_h,
                ..ClubData::default()
            },
            // Impact without club data still counts as club data for GSPro.
            shot.impact.is_some(),
        )
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
            contains_club_data: contains_club,
            // A shot proves the device was ready and detected a ball.
            launch_monitor_is_ready: true,
            launch_monitor_ball_detected: true,
            is_heart_beat: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flighthook::{
        BallFlight, ClubData as FhClubData, Distance, FaceImpact, ShotData, Velocity,
    };

    fn shot_with_impact(lateral_mm: f64, vertical_mm: f64) -> ShotData {
        ShotData {
            ball: Some(BallFlight {
                launch_speed: Some(Velocity::MetersPerSecond(3.30)),
                launch_elevation: Some(32.51),
                launch_azimuth: Some(7.39),
                carry_distance: None,
                total_distance: None,
                roll_distance: None,
                max_height: None,
                flight_time: None,
                backspin_rpm: Some(621),
                sidespin_rpm: Some(-87),
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
            }),
            impact: Some(FaceImpact {
                lateral: Some(Distance::Millimeters(lateral_mm)),
                vertical: Some(Distance::Millimeters(vertical_mm)),
            }),
            actor: "square.0".into(),
            shot_number: 1,
        }
    }

    #[test]
    fn face_impact_reaches_gspro_with_sign_intact() {
        // A toe-side, low strike. Both values are negative in FRP terms; a
        // saturating u16 conversion would silently report 0.0 for both.
        let msg = map_shot(&shot_with_impact(-32.19, -17.43), Handedness::Right);
        assert!((msg.club_data.horizontal_face_impact - -32.19).abs() < 1e-9);
        assert!((msg.club_data.vertical_face_impact - -17.43).abs() < 1e-9);
        assert!(msg.shot_data_options.contains_club_data);
    }

    #[test]
    fn face_impact_is_not_flipped_for_left_handers() {
        // Toe/heel and high/low are already golfer-relative, unlike the
        // target-relative fields, which do flip.
        let shot = shot_with_impact(-32.19, -17.43);
        let rh = map_shot(&shot, Handedness::Right);
        let lh = map_shot(&shot, Handedness::Left);
        assert_eq!(
            rh.club_data.horizontal_face_impact,
            lh.club_data.horizontal_face_impact
        );
        assert_eq!(
            rh.club_data.vertical_face_impact,
            lh.club_data.vertical_face_impact
        );
        // ...whereas path does flip.
        assert_eq!(rh.club_data.path, -lh.club_data.path);
    }

    #[test]
    fn serialized_json_carries_every_gspro_club_field() {
        let msg = map_shot(&shot_with_impact(-32.19, -17.43), Handedness::Right);
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
}
