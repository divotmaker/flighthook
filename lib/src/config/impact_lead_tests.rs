//! Tests of the config: impact lead.

use super::*;
use crate::AUTO_CHIP_ACTOR;

#[test]
fn defaults_by_launch_monitor_kind() {
    for (prefix, s) in [
        ("mevo", 0.0),
        ("mock_monitor", 0.0),
        ("square", 1.0),
        ("exputt", 0.0),
        ("r10", 2.0),
        ("openconnect_server", 2.5),
        // A kind added later, or one without a better-known value.
        ("someday", 1.5),
    ] {
        assert_eq!(impact_lead_default_s(prefix), Some(s), "{prefix}");
    }
    assert_eq!(DEFAULT_IMPACT_LEAD_S, 1.5);
    assert_eq!(impact_lead_default_s(AUTO_CHIP_ACTOR), None);
    assert_eq!(impact_lead_default_s("trackball"), None);
    assert_eq!(impact_lead_default_s("pureplay_accessory"), Some(0.9));
    assert_eq!(impact_lead_in_effect_s("trackball", Some(1.0)), None);
}

#[test]
fn only_the_exputt_delivers_its_trigger_late() {
    assert_eq!(trigger_delay_s("exputt"), MAX_TRIGGER_DELAY_S);
    assert_eq!(undated_impact_lead_s("exputt"), Some(DEFAULT_IMPACT_LEAD_S));
    for prefix in [
        "mevo",
        "mock_monitor",
        "square",
        "r10",
        "openconnect_server",
        "trackball",
        AUTO_CHIP_ACTOR,
    ] {
        assert_eq!(trigger_delay_s(prefix), 0.0, "{prefix}");
        assert_eq!(undated_impact_lead_s(prefix), None, "{prefix}");
    }
}

#[test]
fn the_value_in_effect_is_the_valid_setting_else_the_default() {
    assert_eq!(impact_lead_in_effect_s("square", Some(0.25)), Some(0.25));
    assert_eq!(impact_lead_in_effect_s("square", Some(0.0)), Some(0.0));
    assert_eq!(impact_lead_in_effect_s("square", Some(5.0)), Some(5.0));
    assert_eq!(impact_lead_in_effect_s("square", None), Some(1.0));
    for bad in [-0.1, 5.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            impact_lead_in_effect_s("r10", Some(bad)),
            Some(2.0),
            "{bad}"
        );
    }
    assert_eq!(impact_lead_in_effect_s(AUTO_CHIP_ACTOR, Some(1.0)), None);
}

#[test]
fn the_setting_is_validated() {
    for ok in [None, Some(0.0), Some(1.5), Some(5.0)] {
        assert!(impact_lead_valid(ok), "{ok:?}");
        assert_eq!(impact_lead_error("mevo.0", ok), None);
    }
    for bad in [-0.1, 5.1, f64::NAN, f64::INFINITY] {
        assert!(!impact_lead_valid(Some(bad)), "{bad}");
        let error = impact_lead_error("mevo.0", Some(bad)).expect("error");
        assert!(error.starts_with("mevo.0: impact_lead_s "), "{error}");
        assert!(error.ends_with("is outside 0 to 5 s"), "{error}");
    }
    let mut config = FlighthookConfig::default();
    config.square.insert("1".into(), SquareSection::default());
    assert_eq!(config.impact_lead_error(), None);
    config.square.get_mut("1").unwrap().impact_lead_s = Some(6.0);
    assert!(
        config
            .impact_lead_error()
            .is_some_and(|e| e.starts_with("square.1:"))
    );
}

#[test]
fn a_section_without_the_setting_serializes_as_before() {
    let json = serde_json::to_string(&MevoSection::default()).expect("ser");
    assert!(!json.contains("impact_lead_s"), "{json}");
    for json in [
        r#"{"mevo":{"0":{"name":"Mevo"}}}"#,
        r#"{"r10":{"0":{"name":"R10"}}}"#,
        r#"{"square":{"0":{"name":"Omni"}}}"#,
        r#"{"exputt":{"0":{"name":"Putt"}}}"#,
        r#"{"mock_monitor":{"0":{"name":"Mock"}}}"#,
        r#"{"openconnect_server":{"0":{"name":"Server","bind":null}}}"#,
    ] {
        let config: FlighthookConfig = serde_json::from_str(json).expect("parse");
        let out = serde_json::to_string(&config).expect("ser");
        assert!(!out.contains("impact_lead_s"), "{out}");
        let back: FlighthookConfig = serde_json::from_str(&out).expect("parse");
        assert_eq!(back, config);
    }
}

#[test]
fn the_setting_round_trips_on_every_launch_monitor_section() {
    for kind in [
        "mevo",
        "r10",
        "square",
        "exputt",
        "mock_monitor",
        "openconnect_server",
    ] {
        let json = format!(r#"{{"{kind}":{{"0":{{"name":"X","impact_lead_s":0.75}}}}}}"#);
        let config: FlighthookConfig = serde_json::from_str(&json).expect("parse");
        assert_eq!(
            config.configured_impact_lead_s(&format!("{kind}.0")),
            Some(0.75)
        );
        assert_eq!(config.configured_impact_lead_s(&format!("{kind}.1")), None);
        let out = serde_json::to_string(&config).expect("ser");
        assert!(out.contains(r#""impact_lead_s":0.75"#), "{out}");
        let back: FlighthookConfig = serde_json::from_str(&out).expect("parse");
        assert_eq!(back, config);
    }
    // An integer is a number too.
    let config: FlighthookConfig =
        serde_json::from_str(r#"{"square":{"0":{"name":"X","impact_lead_s":2}}}"#).expect("parse");
    assert_eq!(config.configured_impact_lead_s("square.0"), Some(2.0));
}
