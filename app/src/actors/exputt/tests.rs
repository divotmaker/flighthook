use flighthook::{ExPuttConfig, Mode, Severity};
use flightrelay::driver::{DeviceEvent, FrpDriver, FrpSession, TurnHints};
use threeputt::testing::{FakeDevice, PUTT_RESULT_FULL, PUTT_RESULT_PULLED};

use super::*;
use crate::actors::driver::tests::card_bus;
use crate::actors::scripted::{Stepped, is_finished};

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

/// Whether `event` says the device is ready (armed, nothing in the way).
fn is_ready(event: &DeviceEvent) -> bool {
    matches!(event, DeviceEvent::Status(s) if s.blockers.is_empty())
}

/// Every output's numbers for threeputt's recorded putts, a straight
/// one and a pulled one, through the card on threeputt's driver (its putt
/// mapping is the driver's own) stepped over its in-memory device, in the
/// putting mode the card registers for its shots whatever the game's mode.
#[test]
fn outputs_keep_their_numbers() {
    for (case, address, line) in [
        ("exputt", "10.77.0.23", PUTT_RESULT_FULL),
        ("exputt_pulled", "10.77.0.24", PUTT_RESULT_PULLED),
    ] {
        let section = ExPuttSection {
            driver: {
                let mut config = ExPuttConfig::default();
                config.address = Some(address.into());
                config
            },
            ..ExPuttSection::default()
        };
        let device = FakeDevice::new();
        let mut host = Stepped::new(TurnHints::default());
        let mut driver = ExPutt::new(section.driver.clone()).expect("driver");
        let mut session = host
            .connect(|ctx| driver.connect_with(ctx, device.clone()))
            .expect("connect");
        host.until(|now, out| session.step(now, out), is_ready);
        device.push_event(line);
        host.until(|now, out| session.step(now, out), is_finished);
        let identity = session.identity().clone();
        let messages = card_bus::<ExPutt, ExPuttHooks>(
            "exputt.0",
            section,
            Some(Mode::Putting),
            &identity,
            host.events,
        );
        crate::test_ball::assert_outputs_pinned(case, "exputt.0", Some(Mode::Putting), &messages);
    }
}
