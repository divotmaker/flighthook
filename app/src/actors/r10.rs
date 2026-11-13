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
    const SOURCE: ShotSource = ShotSource::R10;
}

/// The R10 actor (`r10.N`).
pub type R10Actor = DriverActor<R10, R10Hooks>;

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use flighthook::{ActorStatus, R10Config};
    use flightrelay::FrpBody;
    use flightrelay::config::ConfigError;
    use flightrelay::driver::{
        Backoff, ConnectCtx, DeviceDriver, DeviceIdentity, DeviceSession, DriverInfo, Events,
        NoAction, SessionEnd,
    };
    use tenover::driver::R10Session;
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::scripted::Handoff;
    use crate::actors::web::tests::TempDir;
    use crate::bus::{BusMsg, LocalEvent};
    use crate::state::SystemState;

    // -- The scripted R10 session --------------------------------------------

    /// MultiLink handle the scripted device grants.
    const EQ_HANDLE: u8 = 0x05;
    /// The scripted device's BLE address (synthetic).
    const EQ_ADDRESS: &str = "F5:D1:00:00:00:01";
    /// Reads that find nothing between two batches of the script.
    const EQ_GAP_READS: u32 = 4;

    /// What the scripted device has left to send, batch by batch.
    #[derive(Default)]
    struct EqWire {
        batches: VecDeque<VecDeque<Vec<u8>>>,
        gap: u32,
    }

    /// A transport playing [`eq_script`]: each batch's chunks back to back,
    /// a few empty reads between batches, then the device is gone (a
    /// transport error). Writes are taken and dropped.
    #[derive(Clone, Default)]
    struct EqFake(Arc<Mutex<EqWire>>);

    impl EqFake {
        fn new() -> Self {
            let fake = Self::default();
            fake.0.lock().unwrap().batches = eq_script().into_iter().map(Into::into).collect();
            fake
        }
    }

    impl tenover::Transport for EqFake {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
            let mut w = self.0.lock().unwrap();
            loop {
                let Some(batch) = w.batches.front_mut() else {
                    return Err(std::io::ErrorKind::NotConnected.into());
                };
                if let Some(chunk) = batch.pop_front() {
                    buf[..chunk.len()].copy_from_slice(&chunk);
                    return Ok(chunk.len());
                }
                if w.gap < EQ_GAP_READS {
                    w.gap += 1;
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
                w.gap = 0;
                w.batches.pop_front();
            }
        }

        fn write(&mut self, _data: &[u8]) -> Result<(), std::io::Error> {
            Ok(())
        }

        fn write_register(&mut self, _data: &[u8]) -> Result<(), std::io::Error> {
            Ok(())
        }
    }

    /// One message from the device, split at MTU 20.
    fn eq_msg(frame: &[u8]) -> Vec<Vec<u8>> {
        tenover::testing::chunks(EQ_HANDLE, frame, 20)
    }

    fn eq_envelope(envelope: &[u8]) -> Vec<Vec<u8>> {
        eq_msg(&tenover::testing::protobuf_frame(envelope))
    }

    /// The session the harness plays: handshake, subscribe, wake (the tee
    /// distance goes out), armed, recording, processing, a full shot, armed
    /// again, a shot with no spin, a device error with its tilt, a warmup
    /// shot without club data; then the device is gone.
    fn eq_script() -> Vec<Vec<Vec<u8>>> {
        use tenover::testing as t;
        let handshake = [
            vec![t::register_response(EQ_HANDLE)],
            eq_msg(&t::device_info_frame()),
            eq_msg(&t::configuration_frame()),
            eq_msg(&t::ack_frame(tenover::gfdi::MSG_CONFIGURATION, 0, &[])),
        ]
        .concat();
        vec![
            handshake,
            eq_envelope(&t::subscribe_result_envelope(true)),
            eq_envelope(&t::wake_result_envelope(0)),
            eq_envelope(&t::environment_result_envelope(true)),
            eq_envelope(&t::state_envelope(2)),
            eq_envelope(&t::state_envelope(3)),
            eq_envelope(&t::state_envelope(4)),
            eq_envelope(EQ_SHOT_FULL),
            eq_envelope(&t::state_envelope(2)),
            eq_envelope(&t::shot_envelope(3, 0.0, 12.0)),
            eq_envelope(&t::error_envelope(3, 0, Some((4.5, -6.25)))),
            eq_envelope(EQ_SHOT_WARMUP),
        ]
    }

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

    // -- tenover's driver over the scripted device --------------------------

    /// Scripted devices waiting for a driver, by the card's range (with
    /// its unit; empty for none): each test runs with its own range (see
    /// [`Handoff`]).
    static DEVICES: Handoff<String, VecDeque<EqFake>> = Handoff::new();

    fn range_key(config: &R10Config) -> String {
        config.range.map(|d| format!("{d:?}")).unwrap_or_default()
    }

    /// tenover's [`R10`] with its BLE scan replaced by the scripted devices
    /// queued for its range: a connect runs the real handshake and session
    /// over the next one, and waits for its stop when none is left.
    struct ScriptedR10 {
        inner: R10,
        devices: VecDeque<EqFake>,
    }

    struct ScriptedSession(R10Session<EqFake>);

    impl DeviceDriver for ScriptedR10 {
        type Config = R10Config;
        type Error = tenover::driver::Error;
        type Action = NoAction;
        type Session = ScriptedSession;
        const STOP_WAIT: Duration = R10::STOP_WAIT;

        fn info(config: &R10Config) -> DriverInfo {
            R10::info(config)
        }

        fn new(config: R10Config) -> Result<Self, ConfigError> {
            let devices = DEVICES.take(&range_key(&config)).unwrap_or_default();
            Ok(Self {
                inner: R10::new(config)?,
                devices,
            })
        }

        fn config(&self) -> &R10Config {
            self.inner.config()
        }

        fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<ScriptedSession, Self::Error> {
            ctx.tick()?;
            ctx.out.phase("discovering");
            match self.devices.pop_front() {
                Some(device) => Ok(ScriptedSession(
                    self.inner.connect_with(ctx, device, EQ_ADDRESS)?,
                )),
                None => loop {
                    ctx.tick()?;
                    std::thread::sleep(Duration::from_millis(20));
                },
            }
        }

        fn retry_delay(
            &mut self,
            ended: &SessionEnd<'_, Self::Error>,
            backoff: &mut Backoff,
            out: &mut Events<'_>,
        ) -> Option<Duration> {
            self.inner.retry_delay(ended, backoff, out)
        }

        fn apply_config(&mut self, new: &R10Config) {
            self.inner.apply_config(new);
        }
    }

    impl DeviceSession for ScriptedSession {
        type Driver = ScriptedR10;

        fn poll(
            &mut self,
            timeout: Duration,
            out: &mut Events<'_>,
        ) -> Result<(), tenover::driver::Error> {
            self.0.poll(timeout, out)
        }

        fn identity(&self) -> &DeviceIdentity {
            self.0.identity()
        }

        fn close(self, out: &mut Events<'_>) -> Result<(), tenover::driver::Error> {
            self.0.close(out)
        }
    }

    impl Hooks<ScriptedR10> for R10Hooks {
        type Section = R10Section;
        const SOURCE: ShotSource = ShotSource::R10;
    }

    /// A running R10 card (`r10.0`) on a scripted device, and the bus.
    struct Card {
        _dir: TempDir,
        state: Arc<SystemState>,
        rx: broadcast::Receiver<BusMsg>,
    }

    impl Card {
        fn start(section: &R10Section, device: EqFake) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flighthook-r10-test-{}",
                crate::state::config::generate_id()
            ));
            let (state, _writer) = SystemState::new(dir.join("config.toml"));
            state.system.update(|c| {
                c.r10.insert("0".into(), section.clone());
            });
            let state = Arc::new(state);
            DEVICES.put(range_key(&section.driver), VecDeque::from([device]));
            let (tx, rx) = broadcast::channel(1024);
            crate::actors::start_actor(
                "r10.0".into(),
                Box::new(DriverActor::<ScriptedR10, R10Hooks>::new(section.clone())),
                &state,
                &tx,
            );
            Self {
                _dir: TempDir(dir),
                state,
                rx,
            }
        }

        /// Whether `m` is the card's: its local events and its FRP
        /// messages (anything not from flighthook's session).
        fn is_cards(&self, m: &BusMsg) -> bool {
            match m {
                BusMsg::Local { card, .. } => card == "r10.0",
                BusMsg::Frp { msg, .. } => msg.from != self.state.flighthook_id(),
            }
        }

        /// Messages of `r10.0` until one matches `want`, within 10 s.
        fn until(&mut self, mut want: impl FnMut(&BusMsg) -> bool) -> Vec<BusMsg> {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut seen = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "timed out: {} seen", seen.len());
                match self.rx.try_recv() {
                    Ok(m) if self.is_cards(&m) => {
                        let done = want(&m);
                        seen.push(m);
                        if done {
                            return seen;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }
    }

    fn is_status(m: &BusMsg, want: ActorStatus) -> bool {
        matches!(m.as_local(), Some((_, LocalEvent::CardStatus { status, .. })) if *status == want)
    }

    fn section(range: Option<flighthook::Distance>) -> R10Section {
        R10Section {
            driver: R10Config {
                range,
                ..R10Config::default()
            },
            ..R10Section::default()
        }
    }

    /// A device that arms and makes the one shot `envelope`.
    fn one_shot(envelope: &[u8]) -> EqFake {
        let mut script = eq_script();
        script.truncate(5);
        script.push(eq_envelope(envelope));
        let fake = EqFake::default();
        fake.0.lock().unwrap().batches = script.into_iter().map(Into::into).collect();
        fake
    }

    /// The bus messages of a card on `device` up to its first shot's end.
    fn first_shot(feet: f64, device: EqFake) -> Vec<BusMsg> {
        let mut card = Card::start(&section(Some(flighthook::Distance::Feet(feet))), device);
        let messages =
            card.until(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::ShotFinished(_))));
        card.state.stop_and_wait("r10.0");
        messages
    }

    /// A range the driver refuses (from the file) keeps the card from
    /// starting and shows on it; the card still stops
    /// at once.
    #[test]
    fn a_refused_range_shows_on_the_card() {
        let mut card = Card::start(
            &section(Some(flighthook::Distance::Feet(0.0))),
            EqFake::new(),
        );
        let seen = card.until(|m| is_status(m, ActorStatus::Disconnected));
        let statuses: Vec<&BusMsg> = seen
            .iter()
            .filter(|m| matches!(m.as_local(), Some((_, LocalEvent::CardStatus { .. }))))
            .collect();
        assert_eq!(statuses.len(), 1, "no Starting, no connect");
        assert!(
            !seen
                .iter()
                .any(|m| matches!(m.as_frp().map(|f| &f.body), Some(FrpBody::ShotTrigger(_)))),
            "no shot"
        );
        assert!(
            matches!(statuses[0].as_local(), Some((_, LocalEvent::CardStatus { telemetry, .. }))
            if telemetry["error"] == "range: must be a distance greater than 0")
        );
        let at = Instant::now();
        assert_eq!(
            card.state.stop_and_wait("r10.0"),
            crate::state::Stopped::Exited
        );
        assert!(at.elapsed() < Duration::from_secs(2));
    }

    // -- Shot values and outputs ---------------------------------------------

    /// Every output's numbers for the R10's shot messages (tenover's golden
    /// shots), through the card on tenover's driver.
    #[test]
    fn outputs_keep_their_numbers() {
        for (case, envelope, feet) in [
            ("r10", EQ_SHOT_FULL, 6.0),
            ("r10_warmup", EQ_SHOT_WARMUP, 6.5),
        ] {
            let messages = first_shot(feet, one_shot(envelope));
            crate::test_ball::assert_outputs_pinned(case, "r10.0", None, &messages);
        }
    }

    /// Strict mode, through the real path: the R10's back and side spin
    /// go out with its total and axis; a shot whose launch monitor never
    /// sent its `bus.actor` (unknown source) is withheld.
    #[test]
    fn strict_mode_sends_the_r10_spin_pairs() {
        // Its own range: the range keys the scripted device handoff.
        let messages = first_shot(7.0, one_shot(EQ_SHOT_FULL));
        let frames = crate::test_ball::strict_frames("r10.0", &messages).expect("sent");
        let spin = crate::actors::pureplay::codec::BallData::TOTAL_SPIN
            | crate::actors::pureplay::codec::BallData::BACK_SPIN
            | crate::actors::pureplay::codec::BallData::SIDE_SPIN
            | crate::actors::pureplay::codec::BallData::SPIN_AXIS;
        assert_eq!(frames.ball.valid & spin, spin);
        assert!(frames.ball.back_spin_rpm > 0.0);
        let unknown = crate::test_ball::without_bus_actor(&messages);
        assert!(crate::test_ball::strict_frames("r10.0", &unknown).is_none());
    }

    /// The R10 measures total spin and axis; the back and side spin tenover
    /// hands back are the same spin in the other pair, not a model's: the
    /// bus launch carries all four, they agree, and a tournament round
    /// keeps them as measured.
    #[test]
    fn a_round_keeps_all_four_r10_spin_values() {
        let messages = first_shot(5.0, one_shot(EQ_SHOT_FULL));
        let launch = messages
            .iter()
            .find_map(|m| match m.as_frp().map(|f| &f.body) {
                Some(FrpBody::ShotLaunch(l)) => Some(l.launch.clone()),
                _ => None,
            })
            .expect("a launch");
        assert_eq!(launch.total_spin_rpm, Some(2800));
        // The golden shot's wire tilt of 5 is 5° left.
        assert_eq!(launch.spin_axis, Some(-5.0));
        let mut components = flightrelay::Launch {
            backspin_rpm: launch.backspin_rpm,
            sidespin_rpm: launch.sidespin_rpm,
            ..flightrelay::Launch::default()
        };
        components.complete_spin();
        assert_eq!(components.total_spin_rpm, launch.total_spin_rpm);
        assert!((components.spin_axis.expect("axis") + 5.0).abs() < 0.05);

        let round = crate::state::round::RoundPolicy::default();
        let guard = round
            .begin_round(&crate::state::round::RoundRequest {
                holder: "Pureplay",
                feeds: &[],
            })
            .expect("accepted");
        let shot = crate::actors::simout::OutputShot {
            launch: Some(launch.clone()),
            source: Some(ShotSource::R10.provenance()),
            ..crate::actors::simout::OutputShot::default()
        };
        assert!(
            guard.counts(shot.source),
            "a measured source counts in a round"
        );
        assert_eq!(shot.launch.as_ref(), Some(&launch));
    }
}
