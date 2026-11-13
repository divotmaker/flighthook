//! Tests of the config: exputt section.

use super::*;

/// An ExPutt section as flighthook 0.1.24 wrote it reads into the same
/// values, the address now in the driver's settings, and writes back the
/// same JSON; a default section writes only its name, and the address is
/// left out while unset.
#[test]
fn a_saved_exputt_section_reads_and_writes_as_before() {
    for (json, address) in [
        (
            r#"{"name":"ExPutt","address":"192.168.1.50","impact_lead_s":0.5}"#,
            Some("192.168.1.50"),
        ),
        (r#"{"name":"Practice","enabled":false}"#, None),
        (r#"{"name":"ExPutt"}"#, None),
    ] {
        let section: ExPuttSection = serde_json::from_str(json).unwrap();
        assert_eq!(section.driver.address.as_deref(), address, "{json}");
        let back = serde_json::to_value(&section).unwrap();
        assert_eq!(
            back,
            serde_json::from_str::<serde_json::Value>(json).unwrap()
        );
    }
    let section: ExPuttSection =
        serde_json::from_str(r#"{"name":"ExPutt","address":"192.168.1.50","impact_lead_s":0.5}"#)
            .unwrap();
    assert_eq!(section.impact_lead_s, Some(0.5));
    assert!(section.enabled);
    assert_eq!(
        serde_json::to_string(&ExPuttSection::default()).unwrap(),
        r#"{"name":"ExPutt"}"#
    );
}

/// The ExPutt driver refuses an address that is not an IP (blank finds
/// the device), and the card and field are named; a save that keeps
/// such a stored address is not refused.
#[test]
fn an_address_that_is_not_an_ip_is_refused() {
    let mut config = FlighthookConfig::default();
    config.exputt.insert("0".into(), ExPuttSection::default());
    for fine in ["", "  ", "10.0.0.7", " 10.0.0.7 ", "fe80::1"] {
        config.exputt.get_mut("0").unwrap().driver.address = Some(fine.into());
        assert_eq!(config.device_error(), None, "{fine:?}");
    }
    for bad in ["exputt.local", "10.0.0.7:8889"] {
        config.exputt.get_mut("0").unwrap().driver.address = Some(bad.into());
        let error = config.device_error().expect("refused");
        assert!(error.starts_with("exputt.0: address: "), "{error}");
        assert!(error.contains(bad), "{error}");
        let saved = config.clone();
        config.exputt.get_mut("0").unwrap().name = "Renamed".into();
        assert_eq!(config.changed_device_error(&saved), None);
        config.exputt.get_mut("0").unwrap().driver.address = Some("putter".into());
        assert!(config.changed_device_error(&saved).is_some());
    }
}
