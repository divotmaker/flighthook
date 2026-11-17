use flighthook::{DeviceSection, Launch, ShotKey, SpinSource};
use flightrelay::ShotLaunch;
use flightrelay::driver::{DeviceEvent, DeviceIdentity, FrpDriver, FrpSession, TurnHints};
use ironsight::protocol::shot::{FlightResult as D4, FlightResultV1};
use ironsight::protocol::{TYPE_CLUB_RESULT, TYPE_FLIGHT_RESULT, TYPE_FLIGHT_RESULT_V1};
use ironsight::testing::{FakeDevice, Shot, fixtures};

use super::*;
use crate::actors::driver::tests::card_bus;
use crate::actors::scripted::{Stepped, body, is_finished, shot_events};
use crate::bus::BusMsg;

/// The card sections the pins run with.
fn section(xi: bool) -> MevoSection {
    let json = if xi {
        serde_json::json!({
            "name": "Xi", "address": "192.168.2.1:5100", "use_estimated": true,
            "spin_source": "auto", "range": "8ft"
        })
    } else {
        // As the card writes a new FlightScope card.
        serde_json::json!({
            "name": "Mevo WiFi", "address": "192.168.2.1:5100", "ball_type": 1,
            "tee_height": "1.5in", "range": "8ft", "surface_height": "0in", "track_pct": 80.0,
            "use_estimated": false, "camera_mode": "standard",
            "environment": "limited_flight"
        })
    };
    serde_json::from_value(json).expect("section")
}

/// What the card puts on the bus for the first shot `shot` of ironsight's
/// session on `device`, the card with `section`, in Full mode.
fn first_wire_shot(device: &FakeDevice, section: MevoSection, shot: &Shot) -> Vec<BusMsg> {
    let mut host = Stepped::new(TurnHints::default().with_mode(Mode::Full));
    let mut mevo = Mevo::new(section.driver_settings()).expect("driver");
    let mut session = host
        .connect(|ctx| mevo.connect_with(ctx, device.clone()))
        .expect("connect");
    host.until(|now, out| session.step(now, out), is_ready);
    device.push_shot(shot);
    host.until(|now, out| session.step(now, out), is_finished);
    let identity = session.identity().clone();
    card_bus::<Mevo, MevoHooks>("mevo.0", section, Some(Mode::Full), &identity, host.events)
}

/// Whether `event` says the device is ready (armed, nothing in the way).
fn is_ready(event: &DeviceEvent) -> bool {
    matches!(event, DeviceEvent::Status(s) if s.blockers.is_empty())
}

/// The Xi full swing with an E8, a D4 and its club result.
fn xi_full_swing() -> Shot {
    Shot::new(
        63,
        ironsight::BusAddr::Avr,
        [
            (TYPE_FLIGHT_RESULT_V1, fixtures::XI_SHOT_E8.to_vec()),
            (TYPE_FLIGHT_RESULT, fixtures::XI_SHOT_D4.to_vec()),
            (TYPE_CLUB_RESULT, fixtures::XI_SHOT_ED.to_vec()),
        ],
        fixtures::XI_SHOT_ED.to_vec(),
    )
}

/// The Xi card with the E8 spin source.
fn xi_e8_section() -> MevoSection {
    let mut section = section(true);
    section.driver.spin_source = SpinSource::E8;
    section
}

/// What the card puts on the bus for one shot of `parts` (ironsight's
/// session's messages between the trigger and `shot.finished`), the card
/// with `section` and the session in `mode`.
fn card_shot(section: MevoSection, mode: Mode, parts: Vec<FrpBody>) -> Vec<BusMsg> {
    let key = ShotKey::new("replay-1", 1);
    let parts = parts.into_iter().map(|p| with_key(p, &key)).collect();
    card_bus::<Mevo, MevoHooks>(
        "mevo.0",
        section,
        Some(mode),
        &DeviceIdentity::new("FS M2-REPLAY"),
        shot_events(&key, parts),
    )
}

/// `part` as a message of the shot `key`.
fn with_key(mut part: FrpBody, key: &ShotKey) -> FrpBody {
    match &mut part {
        FrpBody::ShotLaunch(m) => m.key = key.clone(),
        FrpBody::ShotFlight(m) => m.key = key.clone(),
        _ => unreachable!("a launch or a flight"),
    }
    part
}

fn launch_body(launch: Launch) -> FrpBody {
    ShotLaunch {
        key: ShotKey::new("", 0),
        launch,
    }
    .into()
}

/// The launch-only Xi chip: 7.019 m/s (15.7 mph) at 22.8°, 0.856° left,
/// no spin, no carry, apex or flight time.
fn launch_only_d4() -> D4 {
    D4 {
        total: 1,
        track_time: 0.0,
        start_position: [0.0; 3],
        launch_speed: 7.019,
        // Wire sign: positive = left.
        launch_azimuth: 0.856,
        launch_elevation: 22.8,
        carry_distance: 0.0,
        flight_time: 0.0,
        max_height: 0.0,
        landing_position: [0.0; 3],
        backspin_rpm: 0,
        sidespin_rpm: 0,
        riflespin_rpm: 0,
        landing_spin_rpm: [0; 3],
        landing_velocity: [0.0; 3],
        total_distance: 0.0,
        roll_distance: 0.0,
        final_position: [0.0; 3],
        clubhead_speed: 0.0,
        club_strike_direction: 0.0,
        club_attack_angle: 0.0,
        clubhead_speed_post: 0.0,
        club_swing_plane_tilt: 0.0,
        club_swing_plane_rotation: 0.0,
        club_effective_loft: 0.0,
        club_face_angle: 0.0,
        poly_scale: 0,
        poly_x: [0.0; 5],
        poly_y: [0.0; 5],
        poly_z: [0.0; 5],
    }
}

/// The launch ironsight's session sends for the launch-only chip: its
/// launch with no spin fields, and no flight result.
fn launch_only_launch() -> Launch {
    let d4 = launch_only_d4();
    assert!(ironsight::driver::convert::launch_only(&d4));
    Launch {
        backspin_rpm: None,
        sidespin_rpm: None,
        total_spin_rpm: None,
        spin_axis: None,
        ..ironsight::driver::convert::launch(&d4)
    }
}

/// The card's launch-only estimate in `seen`: its assumed backspin and
/// sidespin. Checks it is on the bus before the shot's `shot.finished`.
fn estimate_of(seen: &[BusMsg]) -> Option<(Option<i32>, Option<i32>)> {
    let at = seen.iter().position(|m| {
        matches!(body(m), Some(FrpBody::ShotFlight(f))
            if f.flight.model.as_deref() == Some(LAUNCH_ONLY_MODEL))
    })?;
    let finished = seen
        .iter()
        .position(|m| matches!(body(m), Some(FrpBody::ShotFinished(_))))
        .expect("finished");
    assert!(at < finished, "the estimate comes before shot.finished");
    let Some(FrpBody::ShotFlight(f)) = body(&seen[at]) else {
        unreachable!()
    };
    assert_eq!(f.flight.basis, Basis::Modeled);
    let assumed = f.flight.assumed.as_ref().expect("assumed");
    Some((assumed.backspin_rpm, assumed.sidespin_rpm))
}

// -- The card's own processing ------------------------------------------

/// A launch-only chip (speed and launch angle, no spin, no flight result)
/// gets the card's own flight result before `shot.finished`, modeled,
/// assuming a backspin of `launch_only_spin` rpm per mph and no sidespin;
/// the launch carries only what the device measured.
#[test]
fn a_launch_only_chip_gets_the_estimated_spin() {
    let seen = card_shot(
        section(true),
        Mode::Chipping,
        vec![launch_body(launch_only_launch())],
    );
    // 100 rpm per mph × 15.7 mph.
    assert_eq!(estimate_of(&seen), Some((Some(1570), Some(0))));
    let launch = seen
        .iter()
        .find_map(|m| match body(m) {
            Some(FrpBody::ShotLaunch(l)) => Some(&l.launch),
            _ => None,
        })
        .expect("a launch");
    assert_eq!((launch.backspin_rpm, launch.sidespin_rpm), (None, None));
}

/// The estimate goes with the mode the session ran in at the trigger:
/// none in Putting.
#[test]
fn the_estimate_goes_with_the_sessions_mode() {
    let chip = || vec![launch_body(launch_only_launch())];
    assert!(estimate_of(&card_shot(section(true), Mode::Full, chip())).is_some());
    assert!(estimate_of(&card_shot(section(true), Mode::Putting, chip())).is_none());
}

/// No estimate when the mode at the trigger was Putting, the launch is
/// under 5° (it rolls as a ground ball), `launch_only_spin` is 0, the
/// launch has a spin of the device's (an Xi E8 total), or the device sent
/// its own flight result.
#[test]
fn a_launch_only_estimate_needs_a_chip_without_spin() {
    let base = launch_only_launch();
    let backspin = |launch: Launch, k: f64, mode: Mode, flight: Option<FlightResult>| {
        let section = MevoSection {
            launch_only_spin: k,
            ..section(true)
        };
        let shot = ShotSoFar {
            key: ShotKey::new("chip", 1),
            launch: Some(launch),
            flight,
            mode: Some(mode),
        };
        match estimate(&shot, &section) {
            Some(FrpBody::ShotFlight(f)) => f.flight.assumed.and_then(|a| a.backspin_rpm),
            _ => None,
        }
    };
    let low = Launch {
        launch_elevation: Some(4.9),
        ..base.clone()
    };
    let spun = Launch {
        total_spin_rpm: Some(1800),
        ..base.clone()
    };
    let tracked = Some(FlightResult::new(Basis::Tracked));
    assert_eq!(backspin(base.clone(), 100.0, Mode::Full, None), Some(1570));
    assert_eq!(backspin(base.clone(), 100.0, Mode::Putting, None), None);
    assert_eq!(backspin(low, 100.0, Mode::Full, None), None);
    assert_eq!(backspin(base.clone(), 0.0, Mode::Full, None), None);
    assert_eq!(backspin(spun, 100.0, Mode::Full, None), None);
    assert_eq!(backspin(base.clone(), 100.0, Mode::Full, tracked), None);
    // 80 rpm per mph × 15.7 mph.
    assert_eq!(backspin(base, 80.0, Mode::Chipping, None), Some(1256));
}

// -- Output pins -----------------------------------------------------------

/// Every output's numbers for a Mevo+ and an Xi full swing (ironsight's
/// session on its scripted device), a launch-only Xi chip with the card's
/// spin estimate and a Mevo shot sent from its early result (as
/// ironsight's session sends them).
#[test]
fn outputs_keep_their_numbers() {
    let full = Some(Mode::Full);
    let mevo = first_wire_shot(
        &FakeDevice::mevo_plus(),
        section(false),
        &Shot::new(
            63,
            ironsight::BusAddr::Avr,
            [
                (TYPE_FLIGHT_RESULT, fixtures::XI_SHOT_D4.to_vec()),
                (TYPE_CLUB_RESULT, fixtures::XI_SHOT_ED.to_vec()),
            ],
            fixtures::XI_SHOT_ED.to_vec(),
        ),
    );
    crate::test_ball::assert_outputs_pinned("mevo_full_swing", "mevo.0", full, &mevo);

    let xi = first_wire_shot(&FakeDevice::xi(), xi_e8_section(), &xi_full_swing());
    crate::test_ball::assert_outputs_pinned("xi_full_swing", "mevo.0", full, &xi);

    let chip = card_shot(
        section(true),
        Mode::Full,
        vec![launch_body(launch_only_launch())],
    );
    crate::test_ball::assert_outputs_pinned("xi_launch_only_chip", "mevo.0", full, &chip);

    let early = FlightResultV1::decode(fixtures::XI_SHOT_E8).expect("e8");
    let fallback = card_shot(
        section(false),
        Mode::Full,
        vec![
            launch_body(ironsight::driver::convert::early_launch(&early)),
            ShotFlight {
                key: ShotKey::new("", 0),
                flight: ironsight::driver::convert::early_flight_result(&early),
            }
            .into(),
        ],
    );
    crate::test_ball::assert_outputs_pinned("mevo_e8_fallback", "mevo.0", full, &fallback);
}

/// Strict mode: an Xi with the E8 spin source sends its spin pairs; a
/// launch-only chip's spin estimate (the card's `assumed` spin, no
/// measurement) is withheld.
#[test]
fn strict_mode_sends_the_xi_e8_spin_and_withholds_an_assumed_one() {
    use flighthook_pureplay::connect::codec::BallData;
    let xi = first_wire_shot(&FakeDevice::xi(), xi_e8_section(), &xi_full_swing());
    let frames = crate::test_ball::strict_frames("mevo.0", &xi).expect("sent");
    let pair = BallData::BACK_SPIN | BallData::SIDE_SPIN;
    assert_eq!(frames.ball.valid & pair, pair);
    assert!(frames.ball.back_spin_rpm > 0.0);

    let chip = card_shot(
        section(true),
        Mode::Full,
        vec![launch_body(launch_only_launch())],
    );
    let frames = crate::test_ball::strict_frames("mevo.0", &chip).expect("sent");
    let spin =
        BallData::TOTAL_SPIN | BallData::BACK_SPIN | BallData::SIDE_SPIN | BallData::SPIN_AXIS;
    assert_eq!(frames.ball.valid & spin, 0, "no measured spin");
    assert_ne!(frames.ball.valid & BallData::BALL_SPEED, 0);
}
