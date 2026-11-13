//! Tests of the config: trackball section.

use super::*;

/// A new Trackball card's section: its name, the driver's settings left
/// out at their defaults, and the grab off.
#[test]
fn a_new_trackball_card_writes_its_name() {
    let section = TrackballSection::default();
    assert_eq!(
        serde_json::to_string(&section).unwrap(),
        r#"{"name":"Trackball"}"#
    );
    assert!(!section.driver.config.grab);
    assert!(!section.driver_settings().config.grab);
    assert!(section.enabled);
    assert_eq!(
        section.driver,
        TrackballConfig {
            config: thumbswing::Config {
                grab: false,
                ..thumbswing::Config::default()
            },
            ..TrackballConfig::default()
        }
    );
}

/// Every key reads into the driver's settings and writes back the same
/// JSON, and the tables hold only what was set.
#[test]
fn a_trackball_section_reads_and_writes_its_keys() {
    for json in [
        r#"{"name":"Arcade","enabled":false,"device":"usb-Example_Trackball-event-mouse","club":"DR","cpi":800.0,"report_limit":2047,"thresholds":{"v_start":0.08,"coil_timeout_s":20.0},"tuning":{"full_power_speed":40.0,"curve_sense":"physical","mirror_for_left_hand":true}}"#,
        r#"{"name":"Trackball"}"#,
    ] {
        let section: TrackballSection = serde_json::from_str(json).unwrap();
        let back = serde_json::to_value(&section).unwrap();
        assert_eq!(
            back,
            serde_json::from_str::<serde_json::Value>(json).unwrap(),
            "{json}"
        );
    }
    let section: TrackballSection = serde_json::from_str(
            r#"{"name":"Arcade","enabled":false,"device":"usb-Example_Trackball-event-mouse","club":"DR","hand":"lh","cpi":800,"thresholds":{"v_start":0.08},"tuning":{"curve_sense":"physical"}}"#,
        )
        .unwrap();
    assert!(!section.enabled);
    let d = &section.driver;
    assert_eq!(d.device(), Some("usb-Example_Trackball-event-mouse"));
    assert_eq!(d.club, thumbswing::Club::Driver);
    assert_eq!(d.hand, thumbswing::Hand::default(), "a hand key is ignored");
    assert!(!serde_json::to_string(&section).unwrap().contains("hand"));
    assert!((d.config.cpi - 800.0).abs() < f64::EPSILON, "an integer");
    assert!(!d.config.grab);
    assert!((d.config.thresholds.v_start - 0.08).abs() < f64::EPSILON);
    assert_eq!(d.tuning.curve_sense, thumbswing::CurveSense::Physical);
}

/// A `grab` key, true or false, reads without an error, never turns the
/// grab on, and is not written back.
#[test]
fn a_grab_key_is_ignored_and_dropped() {
    for grab in ["true", "false"] {
        let json = format!(r#"{{"name":"Trackball","cpi":800.0,"grab":{grab}}}"#);
        let section: TrackballSection = serde_json::from_str(&json).unwrap();
        assert!(!section.driver.config.grab, "{json}");
        assert!(!section.driver_settings().config.grab, "{json}");
        assert_eq!(
            serde_json::to_string(&section).unwrap(),
            r#"{"name":"Trackball","cpi":800.0}"#
        );
    }
    // A section built in code with the grab on still hands the driver
    // the grab off, and writes no key.
    let mut section = TrackballSection::default();
    section.driver.config.grab = true;
    assert!(!section.driver_settings().config.grab);
    assert_eq!(
        serde_json::to_string(&section).unwrap(),
        r#"{"name":"Trackball"}"#
    );
}

/// The trackball driver refuses settings out of range, naming the card
/// and the key; a save that keeps a stored refused value is not
/// refused, one that changes it into another refused one is.
#[test]
fn values_the_driver_refuses_name_the_card_and_field() {
    let mut config = FlighthookConfig::default();
    config
        .trackball
        .insert("0".into(), TrackballSection::default());
    assert_eq!(config.device_error(), None);
    type Refuse = fn(&mut TrackballConfig);
    let cases: [(Refuse, &str); 5] = [
        (|d| d.config.cpi = 0.0, "trackball.0: cpi: "),
        (|d| d.config.report_limit = 0, "trackball.0: report_limit: "),
        (
            |d| d.config.thresholds.v_stop = 0.5,
            "trackball.0: thresholds.v_stop: ",
        ),
        (
            |d| d.tuning.power_gamma = 0.0,
            "trackball.0: tuning.power_gamma: ",
        ),
        (
            |d| d.tuning.backswing_power = 2.0,
            "trackball.0: tuning.backswing_power: ",
        ),
    ];
    for (refuse, prefix) in cases {
        config
            .trackball
            .insert("0".into(), TrackballSection::default());
        refuse(&mut config.trackball.get_mut("0").unwrap().driver);
        let error = config.device_error().expect("refused");
        assert!(error.starts_with(prefix), "{error}");
        let saved = config.clone();
        config.trackball.get_mut("0").unwrap().name = "Renamed".into();
        config.trackball.get_mut("0").unwrap().enabled = false;
        assert_eq!(config.changed_device_error(&saved), None, "{prefix}");
        config.trackball.get_mut("0").unwrap().driver.config.cpi = -1.0;
        assert!(config.changed_device_error(&saved).is_some(), "{prefix}");
    }
}

/// Two cards that name the same device cannot both be on.
#[test]
fn two_cards_naming_one_device_clash() {
    let mut config = FlighthookConfig::default();
    let named = |device: Option<&str>| {
        let mut section = TrackballSection::default();
        section.driver.device = device.map(Into::into);
        section
    };
    config.trackball.insert("0".into(), named(Some("event7")));
    config.trackball.insert("1".into(), named(Some(" event7 ")));
    config.trackball.insert("2".into(), named(None));
    config.trackball.insert("3".into(), named(Some("")));
    config.trackball.get_mut("1").unwrap().enabled = false;
    let clash = config.address_clash("trackball.1").expect("clash");
    assert_eq!(clash.message(), "A Trackball card already uses event7");
    let clash = config.address_clash("trackball.3").expect("blank clash");
    assert_eq!(
        clash.holder, "trackball.2",
        "unset and blank are one device"
    );
    assert_eq!(
        clash.message(),
        "A Trackball card already uses the only trackball or mouse"
    );
    config.trackball.get_mut("2").unwrap().enabled = false;
    assert_eq!(
        config.address_clash("trackball.3"),
        None,
        "a blank device and a named one are not compared"
    );
    let clashes = config.clone().demote_clashing(None);
    assert!(clashes.is_empty(), "{clashes:?}");
    config.trackball.get_mut("2").unwrap().enabled = true;
    let clashes = config.demote_clashing(None);
    assert_eq!(clashes.len(), 1, "{clashes:?}");
    assert_eq!(
        clashes[0].id, "trackball.3",
        "the first blank card keeps running"
    );
}
