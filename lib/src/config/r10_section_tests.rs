//! Tests of the config: r10 section.

use super::*;

/// An R10 section as flighthook 0.1.24 wrote it reads into the same
/// values, the range now in the driver's settings, and writes back the
/// same JSON; a default section writes only its name, and the range is
/// left out while unset.
#[test]
fn a_saved_r10_section_reads_and_writes_as_before() {
    for (json, range) in [
        (
            r#"{"name":"Garmin R10","range":"7ft","impact_lead_s":2.5}"#,
            Some(Distance::Feet(7.0)),
        ),
        (
            r#"{"name":"Bay 2","enabled":false,"range":"2.5m"}"#,
            Some(Distance::Meters(2.5)),
        ),
        (r#"{"name":"Garmin R10"}"#, None),
    ] {
        let section: R10Section = serde_json::from_str(json).unwrap();
        assert_eq!(section.driver.range, range, "{json}");
        let back = serde_json::to_value(&section).unwrap();
        assert_eq!(
            back,
            serde_json::from_str::<serde_json::Value>(json).unwrap()
        );
    }
    let section: R10Section =
        serde_json::from_str(r#"{"name":"Garmin R10","range":"7ft","impact_lead_s":2.5}"#).unwrap();
    assert_eq!(section.impact_lead_s, Some(2.5));
    assert!(section.enabled);
    assert_eq!(
        serde_json::to_string(&R10Section::default()).unwrap(),
        r#"{"name":"Garmin R10"}"#
    );
}

/// The R10 driver refuses a range that isn't greater than 0, and the
/// card and field are named; a save that keeps such a stored range is
/// not refused.
#[test]
fn a_range_of_zero_is_refused() {
    let mut config = FlighthookConfig::default();
    config.r10.insert("0".into(), R10Section::default());
    assert_eq!(config.device_error(), None);
    for bad in [
        Distance::Feet(0.0),
        Distance::Meters(-2.0),
        Distance::Feet(f64::NAN),
    ] {
        config.r10.get_mut("0").unwrap().driver.range = Some(bad);
        assert_eq!(
            config.device_error().as_deref(),
            Some("r10.0: range: must be a distance greater than 0")
        );
        let saved = config.clone();
        config.r10.get_mut("0").unwrap().name = "Renamed".into();
        assert_eq!(config.changed_device_error(&saved), None);
        config.r10.get_mut("0").unwrap().driver.range = Some(Distance::Feet(-1.0));
        assert!(config.changed_device_error(&saved).is_some());
    }
    config.r10.get_mut("0").unwrap().driver.range = Some(Distance::Feet(7.0));
    assert_eq!(config.device_error(), None);
}
