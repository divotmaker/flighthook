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

    use flighthook::{
        ActorStatus, FlighthookEvent, FlighthookMessage, MockConfig, MockMonitorSection,
        Provenance, ShotDetectionMode, ShotKey,
    };
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::web::tests::TempDir;
    use crate::state::round::RoundRequest;
    use crate::state::{GameStateWriter, SystemState};

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
        writer: GameStateWriter,
        tx: broadcast::Sender<FlighthookMessage>,
        rx: broadcast::Receiver<FlighthookMessage>,
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
            crate::actors::start_actor(
                "mock_monitor.0".into(),
                Box::new(MockActor::new(section)),
                &self.state,
                &self.tx,
            );
        }

        /// The mock's messages until `want` matches one, within `within`.
        fn until(
            &mut self,
            within: Duration,
            mut want: impl FnMut(&FlighthookMessage) -> bool,
        ) -> Vec<FlighthookMessage> {
            let deadline = Instant::now() + within;
            let mut seen = Vec::new();
            loop {
                assert!(
                    Instant::now() < deadline,
                    "timed out after {} messages",
                    seen.len()
                );
                match self.rx.try_recv() {
                    Ok(m) if m.actor == "mock_monitor.0" => {
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

    fn finished(m: &FlighthookMessage, n: u32) -> bool {
        matches!(&m.event, FlighthookEvent::ShotFinished { key, .. } if key.shot_number == n)
    }

    fn shot_key(event: &FlighthookEvent) -> Option<&ShotKey> {
        match event {
            FlighthookEvent::ShotTrigger { key, .. }
            | FlighthookEvent::Launch { key, .. }
            | FlighthookEvent::FlightResult { key, .. }
            | FlighthookEvent::ClubPath { key, .. }
            | FlighthookEvent::FaceImpact { key, .. }
            | FlighthookEvent::ShotFinished { key, .. } => Some(key),
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
            ("mock_full", ShotDetectionMode::Full),
            ("mock_chipping", ShotDetectionMode::Chipping),
            ("mock_putting", ShotDetectionMode::Putting),
        ] {
            let mut rig = rig();
            rig.writer.set_mode(mode);
            rig.start(fast());
            let seen = rig.until(Duration::from_secs(10), |m| finished(m, 3));
            rig.state.stop_and_wait("mock_monitor.0");
            let shot: Vec<FlighthookMessage> = seen
                .into_iter()
                .filter(|m| shot_key(&m.event).is_some_and(|k| k.shot_number == 3))
                .map(|mut m| {
                    match &mut m.event {
                        FlighthookEvent::ShotTrigger { key, .. }
                        | FlighthookEvent::Launch { key, .. }
                        | FlighthookEvent::FlightResult { key, .. }
                        | FlighthookEvent::ClubPath { key, .. }
                        | FlighthookEvent::FaceImpact { key, .. }
                        | FlighthookEvent::ShotFinished { key, .. } => key.shot_number = 1,
                        _ => {}
                    }
                    m
                })
                .collect();
            crate::test_ball::assert_outputs_pinned(case, &shot);
        }
    }

    /// The mock card as users see it: Connected as "Mock Launch Monitor
    /// (simulated)" with its shot count, its tracking mode, radar mode and
    /// readiness as device telemetry (source virtual), each shot's events
    /// in order with no wire protocol behind them, and the shot count after
    /// each shot.
    #[test]
    fn the_mock_card_reads_as_before() {
        let mut rig = rig();
        rig.writer.set_mode(ShotDetectionMode::Chipping);
        rig.start(fast());
        let seen = rig.until(Duration::from_secs(10), |m| {
            matches!(&m.event, FlighthookEvent::ActorStatus { telemetry, .. }
                if telemetry.get("shot_count").map(String::as_str) == Some("1"))
        });
        rig.state.stop_and_wait("mock_monitor.0");
        let connected = seen
            .iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::ActorStatus {
                    status: ActorStatus::Connected,
                    telemetry,
                } if telemetry.contains_key("device_info") => Some(telemetry),
                _ => None,
            })
            .expect("connected");
        assert_eq!(connected["device_info"], "Mock Launch Monitor (simulated)");
        assert_eq!(connected["shot_count"], "0");
        let telemetry = seen
            .iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::DeviceTelemetry {
                    telemetry: Some(t),
                    source,
                    ..
                } => Some((t, *source)),
                _ => None,
            })
            .next_back()
            .expect("telemetry");
        assert_eq!(telemetry.0["tracking_mode"], "chipping");
        assert_eq!(telemetry.0["ready"], "false", "after the shot");
        assert!(telemetry.0.contains_key("radar_mode"));
        assert_eq!(telemetry.1, Some(Provenance::Virtual));
        let kinds: Vec<&str> = seen
            .iter()
            .filter_map(|m| match &m.event {
                FlighthookEvent::ShotTrigger { .. } => Some("trigger"),
                FlighthookEvent::Launch { .. } => Some("launch"),
                FlighthookEvent::FlightResult { .. } => Some("flight"),
                FlighthookEvent::ClubPath { .. } => Some("club"),
                FlighthookEvent::ShotFinished { .. } => Some("finished"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["trigger", "launch", "flight", "club", "finished"]);
        let raw = seen
            .iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::ShotFinished { raw: Some(raw), .. } => Some(raw),
                _ => None,
            })
            .expect("raw");
        assert_eq!(raw.transport, "none");
        assert_eq!(
            raw.unavailable.as_deref(),
            Some("mock device has no wire protocol")
        );
        assert!(
            seen.iter()
                .all(|m| m.device.is_none() || m.device.as_deref() == Some("MockLM-0001"))
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
            matches!(&m.event, FlighthookEvent::DeviceTelemetry { telemetry: Some(t), .. }
                if t.get("ready").map(String::as_str) == Some("true"))
        });
        std::thread::sleep(Duration::from_millis(500));
        let held: Vec<FlighthookMessage> = std::iter::from_fn(|| rig.rx.try_recv().ok()).collect();
        assert!(
            !held
                .iter()
                .any(|m| matches!(m.event, FlighthookEvent::ShotTrigger { .. })),
            "no shot during the round"
        );
        drop(guard);
        let seen = rig.until(Duration::from_secs(5), |m| {
            matches!(m.event, FlighthookEvent::ShotTrigger { .. })
        });
        let Some(FlighthookEvent::ShotTrigger { key, .. }) = seen.last().map(|m| &m.event) else {
            unreachable!()
        };
        assert_eq!(key.shot_number, 1, "no number taken while held");
        assert_eq!(
            seen.last().and_then(|m| m.provenance),
            Some(Provenance::Virtual)
        );
        rig.state.stop_and_wait("mock_monitor.0");
    }
}
