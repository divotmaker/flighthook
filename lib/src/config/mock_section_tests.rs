//! Tests of the config: mock section.

use super::*;

/// A default mock section writes only its name; a section without
/// driver settings reads with the driver's defaults; a section with
/// them round-trips, writing only what differs from the defaults. TOML
/// files are tested in the app's config tests.
#[test]
fn the_mock_section_writes_only_what_differs() {
    let section = MockMonitorSection {
        name: "Mock".into(),
        ..MockMonitorSection::default()
    };
    assert_eq!(
        serde_json::to_string(&section).unwrap(),
        r#"{"name":"Mock"}"#
    );
    let old: MockMonitorSection = serde_json::from_str(r#"{"name":"Mock"}"#).unwrap();
    assert_eq!(old, section);
    let empty: MockMonitorSection = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, MockMonitorSection::default());

    let set: MockMonitorSection =
        serde_json::from_str(r#"{"name":"Mock","enabled":false,"shot_interval_s":5,"serial":"7"}"#)
            .unwrap();
    assert!(!set.enabled);
    assert_eq!(set.driver.shot_interval_s, 5.0);
    assert_eq!(set.driver.serial.as_deref(), Some("7"));
    let json = serde_json::to_string(&set).unwrap();
    assert_eq!(
        json,
        r#"{"name":"Mock","enabled":false,"shot_interval_s":5.0,"serial":"7"}"#
    );
    assert_eq!(
        serde_json::from_str::<MockMonitorSection>(&json).unwrap(),
        set
    );
}

/// `device_error` names the first card, by index, whose driver refuses
/// its settings, and the field.
#[test]
fn device_error_names_the_card_and_the_field() {
    let mut config = FlighthookConfig::default();
    assert_eq!(config.device_error(), None);
    config
        .mock_monitor
        .insert("0".into(), MockMonitorSection::default());
    assert_eq!(config.device_error(), None);
    for (index, interval) in [("2", -1.0), ("1", f64::NAN)] {
        let mut bad = MockMonitorSection::default();
        bad.driver.shot_interval_s = interval;
        config.mock_monitor.insert(index.into(), bad);
    }
    assert_eq!(
        config.device_error().as_deref(),
        Some("mock_monitor.1: shot_interval_s: must be 0 to 86400")
    );
    assert_eq!(MockMonitorSection::sections(&config).len(), 3);

    // In number order: card 2 before card 10.
    let mut config = FlighthookConfig::default();
    for index in ["10", "2"] {
        let mut bad = MockMonitorSection::default();
        bad.driver.shot_interval_s = -1.0;
        config.mock_monitor.insert(index.into(), bad);
    }
    assert_eq!(
        config.device_error().as_deref(),
        Some("mock_monitor.2: shot_interval_s: must be 0 to 86400")
    );
}
