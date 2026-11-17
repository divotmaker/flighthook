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
    use flighthook::DeviceSection;
    use flightrelay::driver::{FrpDriver, FrpSession, TurnHints};

    use super::*;
    use crate::actors::driver::tests::card_bus;
    use crate::actors::scripted::{Stepped, is_finished};
    use crate::bus::BusMsg;
    use crate::golden::{R10_ADDRESS, R10_SHOT_FULL, R10Link, r10_envelope, r10_session_start};

    /// tenover's golden warmup shot message (its `wire_golden` fixture):
    /// measured spin and no club data. Its full shot is [`R10_SHOT_FULL`].
    const EQ_SHOT_WARMUP: &[u8] = &[
        0xf2, 0x01, 0x2c, 0x1a, 0x2a, 0x08, 0x08, 0xca, 0x3e, 0x25, 0x12, 0x23, 0x08, 0x07, 0x10,
        0x00, 0x1a, 0x1d, 0x0d, 0x00, 0x00, 0xf2, 0x41, 0x15, 0x00, 0x00, 0x20, 0x40, 0x1d, 0x00,
        0x00, 0xcc, 0x41, 0x25, 0x00, 0x00, 0x20, 0xc1, 0x2d, 0x00, 0x80, 0xbb, 0x45, 0x30, 0x03,
        0x38, 0x02,
    ];

    /// What the card puts on the bus for the first shot of tenover's
    /// session (no aim offset) when the device sends the shot in
    /// `envelope`.
    fn first_shot(envelope: &[u8]) -> Vec<BusMsg> {
        use tenover::testing::state_envelope;
        let section = R10Section::default();
        let link = R10Link::default();
        link.queue(r10_session_start());
        link.queue(vec![
            r10_envelope(&state_envelope(3)),
            r10_envelope(&state_envelope(4)),
            r10_envelope(envelope),
            r10_envelope(&state_envelope(2)),
        ]);
        let mut host = Stepped::new(TurnHints::default());
        let mut r10 = R10::new(section.driver_settings()).expect("driver");
        let mut session = host
            .connect(|ctx| r10.connect_with(ctx, link, R10_ADDRESS))
            .expect("connect");
        host.until(|now, out| session.step(now, out), is_finished);
        let identity = session.identity().clone();
        card_bus::<R10, R10Hooks>("r10.0", section, None, &identity, host.events)
    }

    /// Every output's numbers for the R10's shot messages (tenover's golden
    /// shots).
    #[test]
    fn outputs_keep_their_numbers() {
        for (case, envelope) in [("r10", R10_SHOT_FULL), ("r10_warmup", EQ_SHOT_WARMUP)] {
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
        let messages = first_shot(R10_SHOT_FULL);
        let frames = crate::test_ball::strict_frames("r10.0", &messages).expect("sent");
        let spin =
            BallData::TOTAL_SPIN | BallData::BACK_SPIN | BallData::SIDE_SPIN | BallData::SPIN_AXIS;
        assert_eq!(frames.ball.valid & spin, spin);
        assert!(frames.ball.back_spin_rpm > 0.0);
        let unknown = crate::test_ball::without_bus_actor(&messages);
        assert!(crate::test_ball::strict_frames("r10.0", &unknown).is_none());
    }
}
