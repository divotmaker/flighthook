use std::sync::Arc;
use std::sync::atomic::AtomicU16;
use std::time::Duration;

use flighthook::{ActorStatus, ExPuttConfig, Mode, Severity};
use flightrelay::config::{ConfigError, DriverConfig};
use flightrelay::driver::{Backoff, ConnectCtx, DriverInfo, Events, FrpDriver, SessionEnd};
use threeputt::driver::{ExPuttSession, Tuning};
use threeputt::testing::{MockConfig, MockDevice, PUTT_RESULT_FULL, PUTT_RESULT_PULLED};
use tokio::sync::broadcast;

use super::*;
use crate::actors::scripted::{Handoff, body, recv_within};
use crate::state::SystemState;

// -- threeputt's driver on its mock device ---------------------------------

/// The mock device's port by the card's `address`: each test runs with
/// its own (see [`Handoff`]).
static PORTS: Handoff<String, u16> = Handoff::new();

/// threeputt's [`ExPutt`] reaching the mock device parked for its
/// `address` on `127.0.0.1`.
struct ScriptedExPutt {
    inner: ExPutt,
    config: ExPuttConfig,
}

impl FrpDriver for ScriptedExPutt {
    type Config = ExPuttConfig;
    type Error = threeputt::driver::Error;
    type Action = Action;
    type Session = ExPuttSession;
    const STOP_WAIT: Duration = ExPutt::STOP_WAIT;

    fn info(config: &ExPuttConfig) -> DriverInfo {
        ExPutt::info(config)
    }

    fn new(config: ExPuttConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        let port = PORTS
            .take(&config.address.clone().unwrap_or_default())
            .expect("a mock device");
        let mut tuning = Tuning::default();
        tuning.port = Arc::new(AtomicU16::new(port));
        let mut local = ExPuttConfig::default();
        local.address = Some("127.0.0.1".into());
        Ok(Self {
            inner: ExPutt::with_tuning(local, tuning)?,
            config,
        })
    }

    fn config(&self) -> &ExPuttConfig {
        &self.config
    }

    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<ExPuttSession, Self::Error> {
        self.inner.connect(ctx)
    }

    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, Self::Error>,
        backoff: &mut Backoff,
        out: &mut Events<'_>,
    ) -> Option<Duration> {
        self.inner.retry_delay(ended, backoff, out)
    }

    fn apply_config(&mut self, new: &ExPuttConfig) {
        self.config = new.clone();
    }
}

impl Hooks<ScriptedExPutt> for ExPuttHooks {
    type Section = ExPuttSection;
    const SOURCE: Option<ShotSource> = Some(ShotSource::ExPutt);

    fn on_frp(&mut self, body: &mut FrpBody, _ctx: &mut HookCtx<'_, ExPuttSection>) -> Pass {
        card_words(body);
        Pass::Send
    }

    fn action(msg: &BusMsg, card: &str) -> Option<Action> {
        find_mat(msg, card)
    }
}

// -- The card's own --------------------------------------------------------

#[test]
fn find_mat_is_the_cards_own() {
    let mine = BusMsg::local(
        "web",
        LocalEvent::FindMat {
            target: "exputt.0".into(),
        },
    );
    assert_eq!(find_mat(&mine, "exputt.0"), Some(Action::FindMat));
    assert_eq!(find_mat(&mine, "exputt.1"), None);
    let other = BusMsg::local("web", LocalEvent::ClearShotHistory);
    assert_eq!(find_mat(&other, "exputt.0"), None);
}

#[test]
fn the_discovery_hint_is_in_the_cards_words() {
    let mut hint: FrpBody =
        flightrelay::BusAlert::new(Severity::Warn, threeputt::driver::DISCOVERY_HINT).into();
    card_words(&mut hint);
    let FrpBody::BusAlert(alert) = &hint else {
        unreachable!()
    };
    assert_eq!(alert.message, DISCOVERY_HINT);
}

// -- Output pins -------------------------------------------------------------

/// Every output's numbers for threeputt's recorded putts, a straight
/// one and a pulled one, through the card on threeputt's driver (its putt
/// mapping is the driver's own), in the putting mode the card registers
/// for its shots whatever the game's mode.
#[test]
fn outputs_keep_their_numbers() {
    for (case, address, line) in [
        ("exputt", "10.77.0.23", PUTT_RESULT_FULL),
        ("exputt_pulled", "10.77.0.24", PUTT_RESULT_PULLED),
    ] {
        let mock = MockDevice::spawn(MockConfig::default()).expect("mock");
        PORTS.put(address.into(), mock.port());
        let section = ExPuttSection {
            driver: {
                let mut config = ExPuttConfig::default();
                config.address = Some(address.into());
                config
            },
            ..ExPuttSection::default()
        };
        let dir = crate::actors::web::tests::TempDir(std::env::temp_dir().join(format!(
            "flighthook-exputt-{}",
            crate::state::config::generate_id()
        )));
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        state.system.update(|c| {
            c.exputt.insert("0".into(), section.clone());
        });
        let frp = state.actor_of_card("exputt.0").expect("frp id");
        let state = Arc::new(state);
        let (tx, mut rx) = broadcast::channel(4096);
        crate::actors::start_actor(
            "exputt.0".into(),
            Box::new(DriverActor::<ScriptedExPutt, ExPuttHooks>::new(section)),
            &state,
            &tx,
        );
        let mut messages = Vec::new();
        let mut pushed = false;
        while !matches!(
            messages.last().and_then(body),
            Some(FrpBody::ShotFinished(_))
        ) {
            let m = recv_within(&mut rx).expect("the putt");
            // A session is up: the putt goes to it.
            if !pushed
                && matches!(
                    m.as_local(),
                    Some((
                        _,
                        LocalEvent::CardStatus {
                            status: ActorStatus::Connected,
                            ..
                        }
                    ))
                )
            {
                assert_eq!(state.fixed_mode(&frp), Some(Mode::Putting));
                mock.push_event(line);
                pushed = true;
            }
            messages.push(m);
        }
        assert_eq!(
            state.stop_and_wait("exputt.0"),
            crate::state::Stopped::Exited
        );
        crate::test_ball::assert_outputs_pinned(case, "exputt.0", Some(Mode::Putting), &messages);
    }
}
