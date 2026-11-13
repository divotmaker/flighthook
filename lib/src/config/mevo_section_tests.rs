//! Tests of the config: mevo section.

use super::*;

/// Mevo sections as flighthook 0.1.24 wrote them (from the card: every
/// setting explicit; and by hand: a few) read into the same values, the
/// device settings now in the driver's, and write back the same JSON.
#[test]
fn a_saved_mevo_section_reads_and_writes_as_before() {
    let card = r#"{"name":"Mevo WiFi","address":"192.168.2.1:5100","ball_type":1,"tee_height":"1.5in","range":"8ft","surface_height":"0in","track_pct":80.0,"use_estimated":false,"camera_mode":"standard","environment":"outdoor","spin_source":"auto","launch_only_spin":120.0,"impact_lead_s":0.25}"#;
    let section: MevoSection = serde_json::from_str(card).unwrap();
    assert_eq!(section.name, "Mevo WiFi");
    assert_eq!(section.launch_only_spin, 120.0);
    assert_eq!(section.impact_lead_s, Some(0.25));
    let d = &section.driver;
    assert_eq!(d.address.as_deref(), Some("192.168.2.1:5100"));
    assert_eq!(d.ball_type, Some(1));
    assert_eq!(d.tee_height, Some(Distance::Inches(1.5)));
    assert_eq!(d.range, Some(Distance::Feet(8.0)));
    assert_eq!(d.surface_height, Some(Distance::Inches(0.0)));
    assert_eq!(d.track_pct, Some(80.0));
    assert_eq!(d.use_estimated, Some(false));
    assert_eq!(d.camera_mode, Some(CameraMode::Standard));
    assert_eq!(d.environment, Some(FlightScopeEnvironment::Outdoor));
    assert_eq!(d.spin_source, SpinSource::Auto);
    for json in [
        card,
        r#"{"name":"Xi","enabled":false,"address":"10.0.0.7:5100","spin_source":"e8"}"#,
        r#"{"name":"Mevo"}"#,
    ] {
        let section: MevoSection = serde_json::from_str(json).unwrap();
        assert_eq!(
            serde_json::to_value(&section).unwrap(),
            serde_json::from_str::<serde_json::Value>(json).unwrap(),
            "{json}"
        );
    }
}

/// Unset settings take the driver's defaults and are not written; a new
/// card's section states the defaults it shows, as before.
#[test]
fn unset_settings_are_left_out() {
    let s: MevoSection = serde_json::from_str(r#"{"name":"Mevo"}"#).unwrap();
    assert_eq!(s.driver, MevoConfig::default());
    assert_eq!(s.driver.camera_mode(), CameraMode::Standard);
    assert_eq!(
        s.driver.environment(),
        FlightScopeEnvironment::LimitedFlight
    );
    assert_eq!(s.driver.spin_source, SpinSource::D4);
    assert!(s.enabled);
    let json = serde_json::to_value(MevoSection::default()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "name": "Mevo WiFi",
            "address": "192.168.2.1:5100",
            "ball_type": 1,
            "tee_height": "1.5in",
            "range": "8ft",
            "surface_height": "0in",
            "track_pct": 80.0
        })
    );
    // An explicit `d4` in the file is dropped on the next save.
    let s: MevoSection = serde_json::from_str(r#"{"name":"Mevo","spin_source":"d4"}"#).unwrap();
    assert_eq!(s.driver.spin_source, SpinSource::D4);
    assert!(!serde_json::to_string(&s).unwrap().contains("spin_source"));
}

/// The driver refuses a track percentage outside 0 to 100, a negative
/// distance and an address that isn't an IP address and port, naming
/// the card and the field; a save that keeps such a stored value is not
/// refused.
#[test]
fn values_the_driver_refuses_name_the_card_and_field() {
    let mut config = FlighthookConfig::default();
    config.mevo.insert("0".into(), MevoSection::default());
    assert_eq!(config.device_error(), None);
    type Case = (fn(&mut MevoConfig), &'static str);
    let cases: [Case; 4] = [
        (
            |d| d.track_pct = Some(101.0),
            "mevo.0: track_pct: must be 0 to 100",
        ),
        (
            |d| d.range = Some(Distance::Feet(-1.0)),
            "mevo.0: range: must be a distance of 0 or more",
        ),
        (
            |d| d.ball_type = Some(2),
            "mevo.0: ball_type: must be 0 or 1",
        ),
        (
            |d| d.address = Some("mevo.local".into()),
            "mevo.0: address: ",
        ),
    ];
    for (set, want) in cases {
        let mut bad = config.clone();
        set(&mut bad.mevo.get_mut("0").unwrap().driver);
        let err = bad.device_error().expect("refused");
        assert!(err.starts_with(want), "{err}");
        let saved = bad.clone();
        bad.mevo.get_mut("0").unwrap().name = "Renamed".into();
        assert_eq!(bad.changed_device_error(&saved), None, "{want}");
    }
}
