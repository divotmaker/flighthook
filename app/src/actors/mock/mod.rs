//! The mock launch monitor card and the random club integration.
//!
//! The mock launch monitor is flightrelay's [`MockDriver`]: a virtual device
//! that arms, reports a ball ready and makes a shot at a fixed interval, with
//! values for the detection mode in play. It runs as a [`DriverActor`] like
//! any device driver; the round policy holds its shots during a tournament
//! round without using up shot numbers.

pub mod randomclub;

use flightrelay::mock::MockDriver;

use crate::actors::driver::{DriverActor, Hooks};
use crate::actors::provenance::ShotSource;

/// The mock launch monitor card: no processing of its own.
#[derive(Debug, Default)]
pub struct MockHooks;

impl Hooks<MockDriver> for MockHooks {
    type Section = flighthook::MockMonitorSection;
    const SOURCE: ShotSource = ShotSource::MockMonitor;
}

/// The mock launch monitor actor (`mock_monitor.N`).
pub type MockActor = DriverActor<MockDriver, MockHooks>;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use flighthook::{ActorStatus, MockConfig, MockMonitorSection};
    use flightrelay::{FrpBody, Mode, Source};
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::web::tests::TempDir;
    use crate::bus::{BusMsg, LocalEvent};
    use crate::state::SystemState;
    use crate::state::round::RoundRequest;
    use crate::state::session::SessionWriter;

    /// A mock that makes a shot as fast as it polls.
    fn fast() -> MockMonitorSection {
        MockMonitorSection {
            name: "Mock".into(),
            driver: MockConfig {
                shot_interval_s: 0.0,
                arm_delay_s: 0.0,
                ball_ready_delay_s: 0.0,
                post_shot_delay_s: 0.0,
                serial: None,
            },
            ..MockMonitorSection::default()
        }
    }

    struct Rig {
        _dir: TempDir,
        state: Arc<SystemState>,
        writer: SessionWriter,
        tx: broadcast::Sender<BusMsg>,
        rx: broadcast::Receiver<BusMsg>,
    }

    fn rig() -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-mock-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, writer) = SystemState::new(dir.join("config.toml"));
        let (tx, rx) = broadcast::channel(4096);
        Rig {
            _dir: TempDir(dir),
            state: Arc::new(state),
            writer,
            tx,
            rx,
        }
    }

    impl Rig {
        fn start(&self, section: MockMonitorSection) {
            self.state.system.update(|c| {
                c.mock_monitor.insert("0".into(), section.clone());
            });
            crate::actors::start_actor(
                "mock_monitor.0".into(),
                Box::new(MockActor::new(section)),
                &self.state,
                &self.tx,
            );
        }

        fn set_mode(&self, mode: Mode) {
            let _ = self.writer.apply(&flightrelay::GolferSelect {
                mode: Some(mode),
                ..flightrelay::GolferSelect::new("r")
            });
        }

        /// Whether `m` is the mock card's: its local events and its FRP
        /// messages (anything not from flighthook's session).
        fn is_mocks(&self, m: &BusMsg) -> bool {
            match m {
                BusMsg::Local { card, .. } => card == "mock_monitor.0",
                BusMsg::Frp { msg, .. } => msg.from != self.state.flighthook_id(),
            }
        }

        /// The mock's messages until `want` matches one, within `within`.
        fn until(
            &mut self,
            within: Duration,
            mut want: impl FnMut(&BusMsg) -> bool,
        ) -> Vec<BusMsg> {
            let deadline = Instant::now() + within;
            let mut seen = Vec::new();
            loop {
                assert!(
                    Instant::now() < deadline,
                    "timed out after {} messages",
                    seen.len()
                );
                match self.rx.try_recv() {
                    Ok(m) if self.is_mocks(&m) => {
                        let done = want(&m);
                        seen.push(m);
                        if done {
                            return seen;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        }
    }

    fn body(m: &BusMsg) -> Option<&FrpBody> {
        m.as_frp().map(|f| &f.body)
    }

    fn actor(m: &BusMsg) -> Option<&flightrelay::BusActor> {
        match body(m) {
            Some(FrpBody::BusActor(a)) => Some(a),
            _ => None,
        }
    }

    /// Every output's numbers for a mock shot in each detection mode, the
    /// third shot of a mock card running on its driver, are the pinned
    /// ones. The pins were recorded as shot number 1, so the shot is
    /// renumbered before it reaches the outputs.
    #[test]
    fn outputs_keep_their_numbers() {
        for (case, mode) in [
            ("mock_full", Mode::Full),
            ("mock_chipping", Mode::Chipping),
            ("mock_putting", Mode::Putting),
        ] {
            let mut rig = rig();
            rig.set_mode(mode);
            rig.start(fast());
            let seen = rig.until(
                Duration::from_secs(10),
                |m| matches!(body(m), Some(FrpBody::ShotFinished(f)) if f.key.shot_number == 3),
            );
            rig.state.stop_and_wait("mock_monitor.0");
            let shot: Vec<BusMsg> = seen
                .into_iter()
                .filter(|m| {
                    body(m)
                        .and_then(FrpBody::shot_key)
                        .is_none_or(|k| k.shot_number == 3)
                })
                .map(|mut m| {
                    if let BusMsg::Frp { msg, .. } = &mut m {
                        match &mut msg.body {
                            FrpBody::ShotTrigger(b) => b.key.shot_number = 1,
                            FrpBody::ShotLaunch(b) => b.key.shot_number = 1,
                            FrpBody::ShotClub(b) => b.key.shot_number = 1,
                            FrpBody::ShotImpact(b) => b.key.shot_number = 1,
                            FrpBody::ShotFlight(b) => b.key.shot_number = 1,
                            FrpBody::ShotFinished(b) => b.key.shot_number = 1,
                            _ => {}
                        }
                    }
                    m
                })
                .collect();
            crate::test_ball::assert_outputs_pinned(case, "mock_monitor.0", Some(mode), &shot);
        }
    }

    /// The mock card as users see it: Connected as "Mock Launch Monitor
    /// (simulated)" with its shot count, its tracking mode, radar mode and
    /// readiness as the card's `bus.actor` telemetry (source virtual), each
    /// shot's messages in order with no wire protocol behind them, and the
    /// shot count after each shot.
    #[test]
    fn the_mock_card_reads_as_before() {
        let mut rig = rig();
        rig.set_mode(Mode::Chipping);
        rig.start(fast());
        let seen = rig.until(Duration::from_secs(10), |m| {
            matches!(m.as_local(), Some((_, LocalEvent::CardStatus { telemetry, .. }))
                if telemetry.get("shot_count").map(String::as_str) == Some("1"))
        });
        rig.state.stop_and_wait("mock_monitor.0");
        let connected = seen
            .iter()
            .find_map(|m| match m.as_local() {
                Some((
                    _,
                    LocalEvent::CardStatus {
                        status: ActorStatus::Connected,
                        telemetry,
                    },
                )) if telemetry.contains_key("device_info") => Some(telemetry),
                _ => None,
            })
            .expect("connected");
        assert_eq!(connected["device_info"], "Mock Launch Monitor (simulated)");
        assert_eq!(connected["shot_count"], "0");
        let last = seen
            .iter()
            .filter_map(actor)
            .rfind(|a| a.telemetry.is_some())
            .expect("telemetry");
        let telemetry = last.telemetry.as_ref().expect("telemetry");
        assert_eq!(telemetry["tracking_mode"], "chipping");
        let after = seen.iter().filter_map(actor).next_back().expect("an actor");
        assert!(!after.blockers.is_empty(), "not ready after the shot");
        assert!(telemetry.contains_key("radar_mode"));
        assert_eq!(
            last.device.as_ref().and_then(|d| d.source),
            Some(Source::Virtual)
        );
        let kinds: Vec<&str> = seen
            .iter()
            .filter_map(|m| match body(m) {
                Some(FrpBody::ShotTrigger(_)) => Some("trigger"),
                Some(FrpBody::ShotLaunch(_)) => Some("launch"),
                Some(FrpBody::ShotFlight(_)) => Some("flight"),
                Some(FrpBody::ShotClub(_)) => Some("club"),
                Some(FrpBody::ShotFinished(_)) => Some("finished"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["trigger", "launch", "flight", "club", "finished"]);
        let raw = seen
            .iter()
            .find_map(|m| match m.as_local() {
                Some((_, LocalEvent::ShotCapture { raw, .. })) => Some(raw),
                _ => None,
            })
            .expect("raw");
        assert_eq!(raw.transport, "none");
        assert_eq!(
            raw.unavailable.as_deref(),
            Some("mock device has no wire protocol")
        );
    }

    /// During a tournament round the mock makes no shot and takes no shot
    /// number; its first shot after the round is number 1.
    #[test]
    fn a_round_holds_the_mocks_shots_without_taking_numbers() {
        let mut rig = rig();
        let guard = rig
            .state
            .round
            .begin_round(&RoundRequest {
                holder: "Pureplay",
                feeds: &[],
            })
            .expect("a round");
        rig.start(fast());
        rig.until(Duration::from_secs(5), |m| {
            actor(m).is_some_and(|a| a.device.is_some() && a.blockers.is_empty())
        });
        std::thread::sleep(Duration::from_millis(500));
        let held: Vec<BusMsg> = std::iter::from_fn(|| rig.rx.try_recv().ok()).collect();
        assert!(
            !held
                .iter()
                .any(|m| matches!(body(m), Some(FrpBody::ShotTrigger(_)))),
            "no shot during the round"
        );
        drop(guard);
        let seen = rig.until(Duration::from_secs(5), |m| {
            matches!(body(m), Some(FrpBody::ShotTrigger(_)))
        });
        let Some(FrpBody::ShotTrigger(trigger)) = seen.last().and_then(body) else {
            unreachable!()
        };
        assert_eq!(trigger.key.shot_number, 1, "no number taken while held");
        let source = seen
            .iter()
            .filter_map(actor)
            .filter_map(|a| a.device.as_ref().and_then(|d| d.source))
            .next_back();
        assert_eq!(source, Some(Source::Virtual));
        rig.state.stop_and_wait("mock_monitor.0");
    }
}
