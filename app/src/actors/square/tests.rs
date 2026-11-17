use allsquare::driver::{club_data, face_impact, launch};
use allsquare::testing;
use allsquare::{BallMetrics, ClubMetrics};
use flighthook::ShotKey;
use flightrelay::driver::DeviceIdentity;
use flightrelay::{FrpBody, ShotClub, ShotImpact, ShotLaunch};

use super::*;
use crate::actors::driver::tests::card_bus;
use crate::actors::scripted::shot_events;
use crate::bus::BusMsg;

fn full_swing() -> (BallMetrics, ClubMetrics) {
    let mut ball = testing::ball_metrics();
    ball.speed = 60.0;
    ball.launch_angle = 14.0;
    ball.direction = -1.2;
    ball.total_spin = 3059;
    ball.spin_axis = -11.31;
    ball.back_spin = 3000;
    ball.side_spin = -600;
    let mut club = ClubMetrics::default();
    club.mask = 0xff;
    club.path = Some(-2.4);
    club.face_angle = Some(-0.8);
    club.attack_angle = Some(-1.6);
    club.dynamic_loft = Some(17.9);
    club.impact_horizontal = Some(3.5);
    club.impact_vertical = Some(-4.0);
    club.club_speed = Some(41.4);
    club.smash_factor = Some(1.45);
    (ball, club)
}

/// A tracked fade chip (the chip pin's).
fn chip() -> (BallMetrics, ClubMetrics) {
    let mut ball = testing::ball_metrics();
    ball.speed = 3.30;
    ball.launch_angle = 32.51;
    ball.direction = 7.39;
    ball.total_spin = 627;
    ball.spin_axis = -8.00;
    ball.back_spin = 621;
    ball.side_spin = -87;
    let mut club = ClubMetrics::default();
    club.mask = 0xff;
    club.path = Some(2.15);
    club.face_angle = Some(4.62);
    club.attack_angle = Some(-3.18);
    club.dynamic_loft = Some(38.4);
    club.impact_horizontal = Some(-6.5);
    club.impact_vertical = Some(2.25);
    club.club_speed = Some(3.02);
    club.smash_factor = Some(1.09);
    (ball, club)
}

/// What the card puts on the bus for the shot `(ball, club)` as
/// allsquare's session sends it with the 7-iron (calibration 0, so the
/// vertical impact is the device's value): the launch, club data and face
/// impact.
fn first_shot((ball, club): (BallMetrics, ClubMetrics)) -> Vec<BusMsg> {
    let key = ShotKey::new("omni-1", 1);
    let parts: Vec<FrpBody> = vec![
        ShotLaunch {
            key: key.clone(),
            launch: launch(&ball),
        }
        .into(),
        ShotClub {
            key: key.clone(),
            club: club_data(&club),
        }
        .into(),
        ShotImpact {
            key: key.clone(),
            impact: face_impact(&club).expect("an impact"),
        }
        .into(),
    ];
    card_bus::<Omni, SquareHooks>(
        "square.0",
        SquareSection::default(),
        None,
        &DeviceIdentity::new("SquareGolf(EQ01)"),
        shot_events(&key, parts),
    )
}

/// Every output's numbers for an Omni shot with club data and face
/// impact: the fade chip and a full swing.
#[test]
fn outputs_keep_their_numbers() {
    for (case, shot) in [("square", chip()), ("square_full_swing", full_swing())] {
        crate::test_ball::assert_outputs_pinned(case, "square.0", None, &first_shot(shot));
    }
}

/// Strict mode sends the card's shots (a measured source): the Omni's
/// vertical impact goes out with the horizontal one.
#[test]
fn strict_mode_sends_the_omni_vertical_impact() {
    use flighthook_pureplay::connect::codec::ClubData;
    let frames =
        crate::test_ball::strict_frames("square.0", &first_shot(full_swing())).expect("sent");
    let club = frames.club.expect("club data");
    let impact = ClubData::IMPACT_POINT_X | ClubData::IMPACT_POINT_Y;
    assert_eq!(club.valid & impact, impact);
}
