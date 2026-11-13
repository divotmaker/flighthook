//! Tests of the config: simulator route.

use super::*;

/// An output routed to a Pureplay Accessory card in any mode is found; one routed
/// to a launch monitor, to "Any", or to a Pureplay Connect output's own
/// type is not.
#[test]
fn outputs_routed_to_a_pureplay_card_are_found() {
    let mut config = FlighthookConfig::default();
    config.openconnect_client.insert(
        "1".into(),
        OpenConnectClientSection {
            chipping_monitor: Some("pureplay_accessory.0".into()),
            ..OpenConnectClientSection::default()
        },
    );
    config.openconnect_client.insert(
        "0".into(),
        OpenConnectClientSection {
            full_monitor: Some("mevo.0".into()),
            ..OpenConnectClientSection::default()
        },
    );
    config.pureplay.insert(
        "0".into(),
        PureplaySection {
            putting_monitor: Some("pureplay_accessory.2".into()),
            ..PureplaySection::default()
        },
    );
    config.pureplay.insert(
        "1".into(),
        PureplaySection {
            full_monitor: Some("pureplay.0".into()),
            ..PureplaySection::default()
        },
    );
    assert_eq!(
        config.simulator_routes(),
        ["openconnect_client.1", "pureplay.0"]
    );
    assert!(!routed_to_simulator(&config, None));
    assert!(!routed_to_simulator(&config, Some("pureplay_accessory")));
    assert!(routed_to_simulator(&config, Some("pureplay_accessory.0")));
}
