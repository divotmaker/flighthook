//! Shot data -> GSPro message conversion.

use super::api::{BallData, ClubData, GsProMessage, ShotDataOptions};
use flighthook::ShotData;

/// Convert a decoded shot into a GSPro Open Connect V1 message.
pub fn map_shot(shot: &ShotData) -> GsProMessage {
    let b = &shot.ball;

    // Spin: prefer SpinData when available, fall back to BallFlight backspin/sidespin.
    let (total_spin, spin_axis) = if let Some(ref spin) = shot.spin {
        (f64::from(spin.total_spin), spin.spin_axis)
    } else {
        let bs = b.backspin_rpm.unwrap_or(0) as f64;
        let ss = b.sidespin_rpm.unwrap_or(0) as f64;
        let total = (bs * bs + ss * ss).sqrt();
        let axis = ss.atan2(bs).to_degrees();
        (total, axis)
    };

    // Club data: use ClubData when present, otherwise zero-fill.
    let (club_data, contains_club) = if let Some(ref club) = shot.club {
        (
            ClubData {
                speed: club.club_speed.as_mph(),
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
            speed: b.launch_speed.as_mph(),
            spin_axis,
            total_spin,
            back_spin: b.backspin_rpm.unwrap_or(0) as f64,
            side_spin: b.sidespin_rpm.unwrap_or(0) as f64,
            hla: b.launch_azimuth,
            vla: b.launch_elevation,
            carry_distance: b.carry_distance.map(|d| d.as_yards()),
        },
        club_data,
        shot_data_options: ShotDataOptions {
            contains_ball_data: true,
            contains_club_data: contains_club,
            // A shot proves the device was ready and detected a ball.
            launch_monitor_is_ready: true,
            launch_monitor_ball_detected: true,
            is_heart_beat: false,
        },
    }
}
