//! Shot data -> GSPro message conversion.

use super::api::{BallData, ClubData, GsProMessage, ShotDataOptions};
use flighthook::ShotData;

/// Convert a decoded shot into a GSPro Open Connect V1 message.
pub fn map_shot(shot: &ShotData) -> GsProMessage {
    let contains_ball = shot.ball.is_some();

    // Ball data: extract from Option<BallFlight>, zero-fill missing fields.
    let (speed, vla, hla, carry_distance, bs, ss) = if let Some(ref b) = shot.ball {
        (
            b.launch_speed.map(|v| v.as_mph()).unwrap_or(0.0),
            b.launch_elevation.unwrap_or(0.0),
            b.launch_azimuth.unwrap_or(0.0),
            b.carry_distance.map(|d| d.as_yards()),
            b.backspin_rpm.unwrap_or(0) as f64,
            b.sidespin_rpm.unwrap_or(0) as f64,
        )
    } else {
        (0.0, 0.0, 0.0, None, 0.0, 0.0)
    };

    // Derive total_spin and spin_axis from backspin + sidespin.
    let total_spin = (bs * bs + ss * ss).sqrt();
    let spin_axis = ss.atan2(bs).to_degrees();

    // Club data: use ClubData when present, otherwise zero-fill.
    let (club_data, contains_club) = if let Some(ref club) = shot.club {
        (
            ClubData {
                speed: club.club_speed.map(|v| v.as_mph()).unwrap_or(0.0),
                angle_of_attack: club.attack_angle.unwrap_or(0.0),
                face_to_target: club.face_angle.unwrap_or(0.0),
                loft: club.dynamic_loft.unwrap_or(0.0),
                path: club.path.unwrap_or(0.0),
            },
            true,
        )
    } else {
        (
            ClubData {
                speed: 0.0,
                angle_of_attack: 0.0,
                face_to_target: 0.0,
                loft: 0.0,
                path: 0.0,
            },
            false,
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
