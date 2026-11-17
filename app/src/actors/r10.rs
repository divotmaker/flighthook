//! The Garmin R10 card: tenover's device driver ([`R10`]) on
//! [`DriverActor`].
//!
//! The driver owns the device: it finds a paired R10 over BLE, runs the
//! handshake (10 s at most), sends the card's tee distance (`range`) once
//! the device wakes, and reports readiness, device state, tilt, device
//! errors and shots. The card adds its section ([`R10Section`]) and its
//! shot source; it has no processing of its own. A changed range takes a
//! new session (a restart).

use flighthook::R10Section;
use tenover::driver::R10;

use crate::actors::driver::{DriverActor, Hooks};
use crate::actors::provenance::ShotSource;

/// The R10 card: no processing of its own.
#[derive(Debug, Default)]
pub struct R10Hooks;

impl Hooks<R10> for R10Hooks {
    type Section = R10Section;
    const SOURCE: Option<ShotSource> = Some(ShotSource::R10);
}

/// The R10 actor (`r10.N`).
pub type R10Actor = DriverActor<R10, R10Hooks>;

#[cfg(test)]
mod tests {
    use flighthook::ShotKey;
    use flightrelay::driver::DeviceIdentity;
    use flightrelay::{FrpBody, ShotClub, ShotLaunch};
    use tenover::driver::convert;
    use tenover::proto::{EnvelopeEvent, decode_envelope};

    use super::*;
    use crate::actors::driver::tests::card_bus;
    use crate::actors::scripted::shot_events;
    use crate::bus::BusMsg;

    /// tenover's golden shot messages (its `wire_golden` fixtures), as the
    /// R10 sends them: a regular shot with every ball, club and swing value,
    /// and a warmup shot with measured spin and no club data.
    const EQ_SHOT_FULL: &[u8] = &[
        0xf2, 0x01, 0x53, 0x1a, 0x51, 0x08, 0x08, 0xca, 0x3e, 0x4c, 0x12, 0x4a, 0x08, 0x2a, 0x10,
        0x01, 0x1a, 0x1d, 0x0d, 0x00, 0x00, 0x48, 0x41, 0x15, 0x9a, 0x99, 0x99, 0xbf, 0x1d, 0x00,
        0x00, 0x86, 0x42, 0x25, 0x00, 0x00, 0xa0, 0x40, 0x2d, 0x00, 0x00, 0x2f, 0x45, 0x30, 0x00,
        0x38, 0x01, 0x22, 0x14, 0x0d, 0x00, 0x00, 0x30, 0x42, 0x15, 0x00, 0x00, 0xc0, 0x3f, 0x1d,
        0x00, 0x00, 0x00, 0xc0, 0x25, 0x00, 0x00, 0x60, 0xc0, 0x2a, 0x0f, 0x08, 0xe8, 0x07, 0x10,
        0xdc, 0x0b, 0x18, 0x88, 0x0e, 0x20, 0x98, 0x11, 0x28, 0xc4, 0x13,
    ];
    const EQ_SHOT_WARMUP: &[u8] = &[
        0xf2, 0x01, 0x2c, 0x1a, 0x2a, 0x08, 0x08, 0xca, 0x3e, 0x25, 0x12, 0x23, 0x08, 0x07, 0x10,
        0x00, 0x1a, 0x1d, 0x0d, 0x00, 0x00, 0xf2, 0x41, 0x15, 0x00, 0x00, 0x20, 0x40, 0x1d, 0x00,
        0x00, 0xcc, 0x41, 0x25, 0x00, 0x00, 0x20, 0xc1, 0x2d, 0x00, 0x80, 0xbb, 0x45, 0x30, 0x03,
        0x38, 0x02,
    ];

    /// What the card puts on the bus for the shot in `envelope` as
    /// tenover's session sends it (no aim offset): the launch, and the club
    /// data when the device tracked the club.
    fn first_shot(envelope: &[u8]) -> Vec<BusMsg> {
        let Ok(EnvelopeEvent::Shot(shot)) = decode_envelope(envelope) else {
            panic!("a shot envelope");
        };
        let section = R10Section::default();
        let aim = section.driver.aim_offset();
        let key = ShotKey::new("r10-1", 1);
        let mut parts: Vec<FrpBody> = Vec::new();
        if let Some(ball) = &shot.ball {
            let launch = convert::launch(ball, aim);
            parts.push(
                ShotLaunch {
                    key: key.clone(),
                    launch,
                }
                .into(),
            );
        }
        if let Some(club) = &shot.club {
            let club = convert::club_data(club, aim);
            parts.push(
                ShotClub {
                    key: key.clone(),
                    club,
                }
                .into(),
            );
        }
        let identity = DeviceIdentity::new("Approach R10");
        card_bus::<R10, R10Hooks>("r10.0", section, None, &identity, shot_events(&key, parts))
    }

    /// Every output's numbers for the R10's shot messages (tenover's golden
    /// shots).
    #[test]
    fn outputs_keep_their_numbers() {
        for (case, envelope) in [("r10", EQ_SHOT_FULL), ("r10_warmup", EQ_SHOT_WARMUP)] {
            crate::test_ball::assert_outputs_pinned(case, "r10.0", None, &first_shot(envelope));
        }
    }

    /// Strict mode sends the card's shots (a measured source): the R10's
    /// back and side spin go out with its total and axis; a shot whose
    /// launch monitor never sent its `bus.actor` (unknown source) is
    /// withheld.
    #[test]
    fn strict_mode_sends_the_r10_spin_pairs() {
        use flighthook_pureplay::connect::codec::BallData;
        let messages = first_shot(EQ_SHOT_FULL);
        let frames = crate::test_ball::strict_frames("r10.0", &messages).expect("sent");
        let spin =
            BallData::TOTAL_SPIN | BallData::BACK_SPIN | BallData::SIDE_SPIN | BallData::SPIN_AXIS;
        assert_eq!(frames.ball.valid & spin, spin);
        assert!(frames.ball.back_spin_rpm > 0.0);
        let unknown = crate::test_ball::without_bus_actor(&messages);
        assert!(crate::test_ball::strict_frames("r10.0", &unknown).is_none());
    }
}
