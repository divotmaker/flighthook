use ironsight::protocol::shot::{ClubResult, FlightResultV1};
use ironsight::seq::ShotData as IronShotData;

use flighthook::{BallFlight, ClubData, Distance, ShotData, SpinData, Velocity};

/// Convert ironsight `ShotData` (from `BinaryEvent::Shot`) into flighthook
/// `ShotData`. Uses the stashed E8 (`FlightResultV1`) as fallback when the
/// primary D4 (`FlightResult`) is absent.
///
/// Returns `None` if neither D4 nor E8 is available.
pub(crate) fn convert_shot(
    shot: &IronShotData,
    stashed_e8: Option<&FlightResultV1>,
    source: &str,
) -> Option<ShotData> {
    // Build ball flight + fallback club data from D4 or E8
    let (shot_number, had_d4, ball, d4_club) = if let Some(d4) = &shot.flight {
        let ball = BallFlight {
            launch_speed: Velocity::MetersPerSecond(d4.launch_speed),
            launch_elevation: d4.launch_elevation,
            launch_azimuth: d4.launch_azimuth,
            carry_distance: Some(Distance::Meters(d4.carry_distance)),
            total_distance: None,
            max_height: Some(Distance::Meters(d4.max_height)),
            flight_time: Some(d4.flight_time),
            roll_distance: None,
            backspin_rpm: Some(d4.backspin_rpm),
            sidespin_rpm: Some(d4.sidespin_rpm),
        };
        let club = ClubData {
            club_speed: Velocity::MetersPerSecond(d4.clubhead_speed),
            path: Some(d4.club_strike_direction),
            attack_angle: Some(d4.club_attack_angle),
            face_angle: Some(d4.club_face_angle),
            dynamic_loft: Some(d4.club_effective_loft),
            smash_factor: None,
            club_speed_post: Some(Velocity::MetersPerSecond(d4.clubhead_speed_post)),
            swing_plane_horizontal: Some(d4.club_swing_plane_rotation),
            swing_plane_vertical: Some(d4.club_swing_plane_tilt),
            club_offset: None,
            club_height: None,
        };
        (d4.total, true, ball, Some(club))
    } else if let Some(e8) = stashed_e8 {
        let ball = BallFlight {
            launch_speed: Velocity::MetersPerSecond(e8.ball_velocity),
            launch_elevation: e8.elevation,
            launch_azimuth: e8.azimuth,
            carry_distance: Some(Distance::Meters(e8.distance)),
            total_distance: None,
            max_height: Some(Distance::Meters(e8.height)),
            flight_time: Some(e8.flight_time),
            roll_distance: None,
            backspin_rpm: Some(e8.backspin_rpm),
            sidespin_rpm: None, // always zero in E8 — None, not Some(0)
        };
        let club = ClubData {
            club_speed: Velocity::MetersPerSecond(e8.club_velocity),
            path: Some(e8.club_strike_direction),
            attack_angle: None,
            face_angle: None,
            dynamic_loft: None,
            smash_factor: None,
            club_speed_post: None,
            swing_plane_horizontal: None,
            swing_plane_vertical: None,
            club_offset: None,
            club_height: None,
        };
        (e8.total, false, ball, Some(club))
    } else {
        return None;
    };

    // Club: prefer ED (authoritative), fall back to D4/E8 embedded fields
    let club = if let Some(ed) = &shot.club {
        Some(club_from_ed(ed))
    } else {
        d4_club
    };

    let spin = shot.spin.as_ref().map(|s| SpinData {
        total_spin: s.pm_spin_final,
        spin_axis: s.spin_axis,
    });

    Some(ShotData {
        source: source.to_string(),
        shot_number,
        ball,
        club,
        spin,
        estimated: !had_d4,
    })
}

fn club_from_ed(ed: &ClubResult) -> ClubData {
    ClubData {
        club_speed: Velocity::MetersPerSecond(ed.pre_club_speed),
        path: Some(ed.strike_direction),
        attack_angle: Some(ed.attack_angle),
        face_angle: Some(ed.face_angle),
        dynamic_loft: Some(ed.dynamic_loft),
        smash_factor: Some(ed.smash_factor),
        club_speed_post: Some(Velocity::MetersPerSecond(ed.post_club_speed)),
        swing_plane_horizontal: Some(ed.swing_plane_horizontal),
        swing_plane_vertical: Some(ed.swing_plane_vertical),
        club_offset: Some(Distance::Meters(ed.club_offset)),
        club_height: Some(Distance::Meters(ed.club_height)),
    }
}
