//! Shot data -> Pureplay Connect telemetry, and Pureplay's club numbers.
//!
//! Only values on the bus's measured blocks (`ball`, `club`, `impact`) are
//! sent, each with its ValidMask bit; nothing is computed here, and
//! flighthook's simulated values (`derived`) are never sent. During a
//! tournament round the shot comes certified by the round policy
//! ([`map_certified`]), with the values that are not the device's reading
//! already removed. A field the device did not report, or whose value is
//! not finite, is sent as 0.0 with its bit clear.
//!
//! Signs need no change: flighthook's lateral values are absolute (positive
//! = right of the target line for both hands), as Pureplay's HLA, SpinAxis,
//! FaceAngle and ClubPath are; face impact is golfer-relative on both sides
//! (lateral positive toward the toe, vertical positive above center), so
//! ImpactPointX/Y take it as it is for either hand. Pureplay Connect v1 does
//! not define SideSpin's sign: it goes out with flighthook's (positive =
//! curves right) through [`side_spin`], the one place that sets it.

use super::codec::{BallData, ClubData};
use crate::state::round::CertifiedShot;
use flighthook::{Club, ShotData};

/// The two telemetry messages for one shot.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotFrames {
    pub ball: BallData,
    /// `None` when the device measured no club field.
    pub club: Option<ClubData>,
}

/// The sign SideSpin is sent with, relative to flighthook's (positive =
/// curves right): 1.0 sends it as it is, -1.0 would send it negated.
const SIDE_SPIN_SIGN: f64 = 1.0;

/// Pureplay's SideSpin for flighthook's `sidespin_rpm`.
fn side_spin(rpm: i32) -> f64 {
    SIDE_SPIN_SIGN * f64::from(rpm)
}

/// Set `bit` in `mask` and return the value as `f32` when `value` is a
/// finite measurement; 0.0 with the bit clear otherwise.
#[allow(clippy::cast_possible_truncation)]
fn field(mask: &mut u16, bit: u16, value: Option<f64>) -> f32 {
    match value.filter(|v| v.is_finite()) {
        Some(v) => {
            *mask |= bit;
            v as f32
        }
        None => 0.0,
    }
}

/// Map a shot certified for a tournament round: only the device's readings
/// are left in it, so a value that is not one (an estimated spin, any value
/// marked computed) goes out as 0.0 with its bit clear. As [`map_shot`]
/// otherwise.
#[must_use]
pub fn map_certified(shot: &CertifiedShot, shot_id: u32) -> Option<ShotFrames> {
    map_shot(shot.shot(), shot_id)
}

/// Map a finished shot to BallData (and ClubData when the device measured
/// any club field), both numbered `shot_id`. Returns `None` for a shot
/// without a ball field to send: Pureplay drops club data that has no ball
/// data.
#[must_use]
pub fn map_shot(shot: &ShotData, shot_id: u32) -> Option<ShotFrames> {
    let b = shot.launch.as_ref()?;
    let mut valid = 0;
    let mut ball = BallData {
        shot_id,
        ball_speed_ms: field(
            &mut valid,
            BallData::BALL_SPEED,
            b.launch_speed.map(|v| v.as_mps()),
        ),
        vla: field(&mut valid, BallData::VLA, b.launch_elevation),
        hla: field(&mut valid, BallData::HLA, b.launch_azimuth),
        total_spin_rpm: field(
            &mut valid,
            BallData::TOTAL_SPIN,
            b.total_spin_rpm.map(f64::from),
        ),
        // A total spin with no axis goes out as backspin too, with no
        // sidespin or axis made up.
        back_spin_rpm: field(
            &mut valid,
            BallData::BACK_SPIN,
            flighthook::output_backspin(b).map(f64::from),
        ),
        side_spin_rpm: field(
            &mut valid,
            BallData::SIDE_SPIN,
            b.sidespin_rpm.map(side_spin),
        ),
        spin_axis_deg: field(&mut valid, BallData::SPIN_AXIS, b.spin_axis),
        valid: 0,
    };
    if valid == 0 {
        return None;
    }
    ball.valid = valid;

    let c = shot.club.as_ref();
    let i = shot.impact.as_ref();
    let mut valid = 0;
    let mut club = ClubData {
        shot_id,
        club_head_speed_ms: field(
            &mut valid,
            ClubData::CLUB_HEAD_SPEED,
            c.and_then(|c| c.club_speed).map(|v| v.as_mps()),
        ),
        smash_factor: field(
            &mut valid,
            ClubData::SMASH_FACTOR,
            c.and_then(|c| c.smash_factor),
        ),
        dynamic_loft_deg: field(
            &mut valid,
            ClubData::DYNAMIC_LOFT,
            c.and_then(|c| c.dynamic_loft),
        ),
        face_angle_deg: field(
            &mut valid,
            ClubData::FACE_ANGLE,
            c.and_then(|c| c.face_angle),
        ),
        club_path_deg: field(&mut valid, ClubData::CLUB_PATH, c.and_then(|c| c.path)),
        attack_angle_deg: field(
            &mut valid,
            ClubData::ATTACK_ANGLE,
            c.and_then(|c| c.attack_angle),
        ),
        // `as_millimeters` is signed; toe and low strikes stay negative
        // where they are.
        impact_point_x_mm: field(
            &mut valid,
            ClubData::IMPACT_POINT_X,
            i.and_then(|i| i.lateral).map(|d| d.as_millimeters()),
        ),
        impact_point_y_mm: field(
            &mut valid,
            ClubData::IMPACT_POINT_Y,
            i.and_then(|i| i.vertical).map(|d| d.as_millimeters()),
        ),
        valid: 0,
    };
    club.valid = valid;
    Some(ShotFrames {
        ball,
        club: (valid != 0).then_some(club),
    })
}

/// flighthook's club for Pureplay's club number; `None` for a number v1
/// does not define.
///
/// | Pureplay | Club |
/// |---|---|
/// | 0 | Driver |
/// | 1 to 8 | 2 to 9 wood |
/// | 9 to 16 | 2 to 9 hybrid |
/// | 17 to 25 | 1 to 9 iron |
/// | 26, 27, 28, 29 | pitching, gap, sand, lob wedge |
/// | 30 | Putter |
#[must_use]
pub fn club_from_pureplay(n: u8) -> Option<Club> {
    const WOODS: [Club; 8] = [
        Club::Wood2,
        Club::Wood3,
        Club::Wood4,
        Club::Wood5,
        Club::Wood6,
        Club::Wood7,
        Club::Wood8,
        Club::Wood9,
    ];
    const HYBRIDS: [Club; 8] = [
        Club::Hybrid2,
        Club::Hybrid3,
        Club::Hybrid4,
        Club::Hybrid5,
        Club::Hybrid6,
        Club::Hybrid7,
        Club::Hybrid8,
        Club::Hybrid9,
    ];
    const IRONS: [Club; 9] = [
        Club::Iron1,
        Club::Iron2,
        Club::Iron3,
        Club::Iron4,
        Club::Iron5,
        Club::Iron6,
        Club::Iron7,
        Club::Iron8,
        Club::Iron9,
    ];
    let n = usize::from(n);
    Some(match n {
        0 => Club::Driver,
        1..=8 => WOODS[n - 1],
        9..=16 => HYBRIDS[n - 9],
        17..=25 => IRONS[n - 17],
        26 => Club::PitchingWedge,
        27 => Club::GapWedge,
        28 => Club::SandWedge,
        29 => Club::LobWedge,
        30 => Club::Putter,
        _ => return None,
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use flighthook::{
        Basis, ClubData as FhClubData, DerivedFlight, Distance, FaceImpact, FlightResult, Launch,
        Provenance, ShotField, Velocity,
    };

    /// Map `shot` as a tournament round sends it: certified by a held
    /// round, from a measured source.
    fn map_strict(shot: &ShotData, shot_id: u32) -> Option<ShotFrames> {
        let measured = ShotData {
            provenance: Some(Provenance::Measured),
            ..shot.clone()
        };
        let round = crate::state::round::RoundPolicy::default();
        let guard = round
            .begin_round(&crate::state::round::RoundRequest {
                holder: "Pureplay",
                feeds: &[],
            })
            .expect("accepted");
        let certified = guard.certify(&measured).expect("measured");
        map_certified(&certified, shot_id)
    }

    /// A full shot: ball with both spin pairs, club, face impact.
    pub(crate) fn full_shot() -> ShotData {
        ShotData {
            actor: "square.0".into(),
            shot_number: 3,
            launch: Some(Launch {
                launch_speed: Some(Velocity::MetersPerSecond(64.0)),
                launch_elevation: Some(12.5),
                launch_azimuth: Some(-1.5),
                backspin_rpm: Some(2450),
                sidespin_rpm: Some(-300),
                total_spin_rpm: Some(2468),
                spin_axis: Some(-6.98),
            }),
            // Device-reported carry: Pureplay has no field for it.
            flight: Some(FlightResult {
                carry_distance: Some(Distance::Meters(150.0)),
                ..FlightResult::new(Basis::Modeled)
            }),
            club: Some(FhClubData {
                club_speed: Some(Velocity::MetersPerSecond(47.5)),
                smash_factor: Some(1.347),
                dynamic_loft: Some(14.2),
                face_angle: Some(-0.8),
                path: Some(2.1),
                attack_angle: Some(-3.4),
                ..FhClubData::default()
            }),
            impact: Some(FaceImpact {
                lateral: Some(Distance::Millimeters(5.0)),
                vertical: Some(Distance::Millimeters(-2.0)),
            }),
            ..ShotData::default()
        }
    }

    /// A launch-only ball: speed and angles, back and side spin only.
    pub(crate) fn ball_only_shot() -> ShotData {
        ShotData {
            actor: "mevo.0".into(),
            shot_number: 4,
            launch: Some(Launch {
                launch_speed: Some(Velocity::MilesPerHour(100.0)),
                launch_elevation: Some(14.0),
                launch_azimuth: Some(2.0),
                backspin_rpm: Some(5000),
                sidespin_rpm: Some(400),
                ..Launch::default()
            }),
            ..ShotData::default()
        }
    }

    #[test]
    fn every_measured_field_is_sent_with_its_bit() {
        let f = map_shot(&full_shot(), 1).expect("frames");
        assert_eq!(
            f.ball,
            BallData {
                shot_id: 1,
                valid: 0x7F,
                ball_speed_ms: 64.0,
                vla: 12.5,
                hla: -1.5,
                total_spin_rpm: 2468.0,
                back_spin_rpm: 2450.0,
                side_spin_rpm: -300.0,
                spin_axis_deg: -6.98,
            }
        );
        assert_eq!(
            f.club,
            Some(ClubData {
                shot_id: 1,
                valid: 0xFF,
                club_head_speed_ms: 47.5,
                smash_factor: 1.347,
                dynamic_loft_deg: 14.2,
                face_angle_deg: -0.8,
                club_path_deg: 2.1,
                attack_angle_deg: -3.4,
                impact_point_x_mm: 5.0,
                impact_point_y_mm: -2.0,
            })
        );
    }

    /// Only what the device reported is marked: no total/axis computed from
    /// the components, no club message without club fields, speed in m/s.
    #[test]
    fn a_ball_only_shot_sends_only_its_own_fields() {
        let f = map_shot(&ball_only_shot(), 9).expect("frames");
        assert_eq!(
            f.ball.valid,
            BallData::BALL_SPEED
                | BallData::VLA
                | BallData::HLA
                | BallData::BACK_SPIN
                | BallData::SIDE_SPIN
        );
        assert!((f.ball.ball_speed_ms - 44.704).abs() < 1e-3);
        assert_eq!(f.ball.total_spin_rpm, 0.0);
        assert_eq!(f.ball.spin_axis_deg, 0.0);
        assert_eq!(f.club, None);
    }

    /// SmashFactor is left for Pureplay to derive unless the device
    /// measured it; a club with only a speed still gets its message.
    #[test]
    fn smash_factor_is_sent_only_when_measured() {
        let mut shot = full_shot();
        shot.impact = None;
        shot.club = Some(FhClubData {
            club_speed: Some(Velocity::MilesPerHour(90.0)),
            ..FhClubData::default()
        });
        let club = map_shot(&shot, 2).and_then(|f| f.club).expect("club");
        assert_eq!(club.valid, ClubData::CLUB_HEAD_SPEED);
        assert_eq!(club.smash_factor, 0.0);
    }

    /// Face impact alone (no club block) still sends ClubData.
    #[test]
    fn face_impact_alone_sends_club_data() {
        let mut shot = full_shot();
        shot.club = None;
        let club = map_shot(&shot, 2).and_then(|f| f.club).expect("club");
        assert_eq!(
            club.valid,
            ClubData::IMPACT_POINT_X | ClubData::IMPACT_POINT_Y
        );
    }

    /// The simulated block is never read: a shot whose ball has only speed
    /// sends only speed, however much flighthook simulated.
    #[test]
    fn simulated_values_are_never_sent() {
        let mut shot = ball_only_shot();
        let ball = shot.launch.as_mut().expect("launch");
        ball.backspin_rpm = None;
        ball.sidespin_rpm = None;
        ball.launch_elevation = None;
        ball.launch_azimuth = None;
        let mut derived = DerivedFlight::default();
        derived.carry_distance = Some(Distance::Meters(120.0));
        shot.derived = Some(derived);
        let f = map_shot(&shot, 1).expect("frames");
        assert_eq!(f.ball.valid, BallData::BALL_SPEED);
    }

    /// A total spin with no axis and no components (an Xi E8 total without
    /// club data) goes out as the total and as backspin; no sidespin or
    /// spin axis is made up (their bits stay clear).
    #[test]
    fn a_total_spin_alone_goes_out_as_backspin() {
        let mut shot = ball_only_shot();
        let ball = shot.launch.as_mut().expect("launch");
        (ball.backspin_rpm, ball.sidespin_rpm) = (None, None);
        (ball.total_spin_rpm, ball.spin_axis) = (Some(6897), None);
        let f = map_shot(&shot, 1).expect("frames");
        assert_eq!(f.ball.total_spin_rpm, 6897.0);
        assert_eq!(f.ball.back_spin_rpm, 6897.0);
        assert_eq!(f.ball.valid & BallData::TOTAL_SPIN, BallData::TOTAL_SPIN);
        assert_eq!(f.ball.valid & BallData::BACK_SPIN, BallData::BACK_SPIN);
        assert_eq!(
            f.ball.valid & (BallData::SIDE_SPIN | BallData::SPIN_AXIS),
            0
        );
        assert_eq!((f.ball.side_spin_rpm, f.ball.spin_axis_deg), (0.0, 0.0));
    }

    #[test]
    fn non_finite_values_are_not_marked() {
        let mut shot = ball_only_shot();
        shot.launch.as_mut().expect("launch").launch_azimuth = Some(f64::NAN);
        let f = map_shot(&shot, 1).expect("frames");
        assert_eq!(f.ball.valid & BallData::HLA, 0);
        assert_eq!(f.ball.hla, 0.0);
    }

    #[test]
    fn a_shot_without_ball_data_sends_nothing() {
        let mut shot = full_shot();
        shot.launch = None;
        assert_eq!(
            map_shot(&shot, 1),
            None,
            "the flight result alone is not sent"
        );
        shot.launch = Some(Launch::default());
        assert_eq!(map_shot(&shot, 1), None);
    }

    fn estimated_spin_shot() -> ShotData {
        let mut shot = ball_only_shot();
        let ball = shot.launch.as_mut().expect("launch");
        ball.backspin_rpm = Some(2400);
        ball.sidespin_rpm = Some(0);
        let mut derived = DerivedFlight::default();
        derived.spin_estimated = true;
        shot.derived = Some(derived);
        shot
    }

    /// flighthook's spin estimate goes out like a measurement normally, and
    /// with every spin bit clear in strict mode; the launch values stay.
    #[test]
    fn strict_mode_clears_an_estimated_spin() {
        let shot = estimated_spin_shot();
        let normal = map_shot(&shot, 1).expect("frames");
        assert_eq!(normal.ball.valid & BallData::BACK_SPIN, BallData::BACK_SPIN);
        assert_eq!(normal.ball.back_spin_rpm, 2400.0);

        let strict = map_strict(&shot, 1).expect("frames");
        assert_eq!(
            strict.ball.valid,
            BallData::BALL_SPEED | BallData::VLA | BallData::HLA
        );
        assert_eq!(strict.ball.back_spin_rpm, 0.0);
        assert_eq!(strict.ball.side_spin_rpm, 0.0);
    }

    /// A measured spin is untouched by strict mode.
    #[test]
    fn strict_mode_keeps_a_measured_spin() {
        let shot = full_shot();
        assert_eq!(map_strict(&shot, 1), map_shot(&shot, 1));
    }

    /// A shot with `fields` marked computed (a model's values).
    fn with_computed(mut shot: ShotData, fields: &[ShotField]) -> ShotData {
        let mut derived = shot.derived.take().map(Box::new);
        DerivedFlight::mark_computed(&mut derived, fields);
        shot.derived = derived.map(|d| *d);
        shot
    }

    /// Strict mode sends only the spin values that are not computed: with
    /// back and side marked computed, total and axis go out, and the
    /// computed pair goes out as 0.0 with its bits clear. Without strict
    /// mode all four go out.
    #[test]
    fn strict_mode_withholds_a_computed_spin_pair() {
        let shot = with_computed(
            full_shot(),
            &[ShotField::BackspinRpm, ShotField::SidespinRpm],
        );
        let normal = map_shot(&shot, 1).expect("frames");
        assert_eq!(normal, map_shot(&full_shot(), 1).expect("frames"));

        let strict = map_strict(&shot, 1).expect("frames");
        assert_eq!(
            strict.ball.valid,
            BallData::BALL_SPEED
                | BallData::VLA
                | BallData::HLA
                | BallData::TOTAL_SPIN
                | BallData::SPIN_AXIS
        );
        assert_eq!(strict.ball.back_spin_rpm, 0.0);
        assert_eq!(strict.ball.side_spin_rpm, 0.0);
        assert_eq!(strict.ball.total_spin_rpm, 2468.0);
        assert_eq!(strict.ball.spin_axis_deg, -6.98);
        assert_eq!(strict.club, normal.club, "club data is untouched");

        // The other way round (total and axis marked computed).
        let shot = with_computed(full_shot(), &[ShotField::TotalSpinRpm, ShotField::SpinAxis]);
        let strict = map_strict(&shot, 1).expect("frames");
        assert_eq!(
            strict.ball.valid & BallData::SPIN,
            BallData::BACK_SPIN | BallData::SIDE_SPIN
        );
        assert_eq!(
            (strict.ball.total_spin_rpm, strict.ball.spin_axis_deg),
            (0.0, 0.0)
        );
    }

    /// A launch angle or an impact height marked computed is withheld in
    /// strict mode; the measured values beside them go out.
    #[test]
    fn strict_mode_withholds_computed_launch_and_impact_values() {
        let putt = with_computed(ball_only_shot(), &[ShotField::LaunchElevation]);
        assert_eq!(
            map_shot(&putt, 1).expect("frames").ball.valid & BallData::VLA,
            BallData::VLA
        );
        let strict = map_strict(&putt, 1).expect("frames");
        assert_eq!(strict.ball.valid & BallData::VLA, 0);
        assert_eq!(strict.ball.vla, 0.0);
        assert_eq!(
            strict.ball.valid & BallData::BALL_SPEED,
            BallData::BALL_SPEED
        );

        let omni = with_computed(full_shot(), &[ShotField::ImpactVertical]);
        let club = map_strict(&omni, 1).expect("frames").club.expect("club");
        assert_eq!(club.valid & ClubData::IMPACT_POINT_Y, 0);
        assert_eq!(club.impact_point_y_mm, 0.0);
        assert_eq!(
            club.valid & ClubData::IMPACT_POINT_X,
            ClubData::IMPACT_POINT_X
        );
        assert_eq!(club.impact_point_x_mm, 5.0);

        // Club data left with nothing to send is not sent.
        let mut only_height = with_computed(ball_only_shot(), &[ShotField::ImpactVertical]);
        only_height.impact = Some(FaceImpact {
            lateral: None,
            vertical: Some(Distance::Millimeters(4.0)),
        });
        assert!(map_shot(&only_height, 1).expect("frames").club.is_some());
        assert!(map_strict(&only_height, 1).expect("frames").club.is_none());
    }

    /// Lateral values are absolute and face impact golfer-relative, so a
    /// left-hander's shot goes out with exactly the signs it has on the bus:
    /// a push right is + for either hand, a toe strike is + for either hand.
    #[test]
    fn a_left_handed_shot_keeps_its_signs() {
        let rh = full_shot();
        let mut lh = full_shot();
        lh.handed = Some(flighthook::Handedness::Left);
        let rh_frames = map_shot(&rh, 1).expect("rh");
        let lh_frames = map_shot(&lh, 1).expect("lh");
        assert_eq!(rh_frames, lh_frames);
        assert_eq!(lh_frames.ball.hla, -1.5);
        assert_eq!(lh_frames.ball.spin_axis_deg, -6.98);
        let club = lh_frames.club.expect("club");
        assert_eq!(club.face_angle_deg, -0.8);
        assert_eq!(club.club_path_deg, 2.1);
        assert_eq!(club.impact_point_x_mm, 5.0, "toe stays +");
        assert_eq!(club.impact_point_y_mm, -2.0, "low stays -");
    }

    /// SideSpin goes out through `side_spin`, with flighthook's sign: a
    /// fade (+ side spin, + axis) is + in both fields.
    #[test]
    fn side_spin_goes_out_with_flighthooks_sign() {
        assert_eq!(side_spin(600), 600.0 * SIDE_SPIN_SIGN);
        let mut shot = ball_only_shot();
        shot.launch.as_mut().expect("launch").sidespin_rpm = Some(600);
        let f = map_shot(&shot, 1).expect("frames");
        assert_eq!(f.ball.side_spin_rpm, 600.0);
        let full = map_shot(&full_shot(), 1).expect("frames");
        assert_eq!(full.ball.side_spin_rpm, -300.0);
        assert_eq!(full.ball.spin_axis_deg, -6.98);
    }

    #[test]
    fn every_pureplay_club_number_maps() {
        let want = [
            (0, Club::Driver),
            (1, Club::Wood2),
            (2, Club::Wood3),
            (4, Club::Wood5),
            (8, Club::Wood9),
            (9, Club::Hybrid2),
            (11, Club::Hybrid4),
            (16, Club::Hybrid9),
            (17, Club::Iron1),
            (20, Club::Iron4),
            (23, Club::Iron7),
            (25, Club::Iron9),
            (26, Club::PitchingWedge),
            (27, Club::GapWedge),
            (28, Club::SandWedge),
            (29, Club::LobWedge),
            (30, Club::Putter),
        ];
        for (n, club) in want {
            assert_eq!(club_from_pureplay(n), Some(club), "{n}");
        }
        // Every number 0..=30 maps, to 31 distinct clubs.
        let clubs: std::collections::HashSet<Club> =
            (0..=30).filter_map(club_from_pureplay).collect();
        assert_eq!(clubs.len(), 31);
        for n in [31, 99, 255] {
            assert_eq!(club_from_pureplay(n), None, "{n}");
        }
    }
}
