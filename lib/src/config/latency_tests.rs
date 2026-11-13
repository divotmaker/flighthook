//! Tests of the config: latency.

use super::*;

#[test]
fn latency_is_valid_from_zero_to_the_maximum() {
    for ok in [None, Some(0.0), Some(0.25), Some(10.0)] {
        assert!(latency_valid(ok), "{ok:?}");
        assert_eq!(latency_error("camera.0", ok), None);
    }
    for bad in [-0.1, 10.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!latency_valid(Some(bad)), "{bad}");
        let error = latency_error("camera.0", Some(bad)).expect("error");
        assert!(error.starts_with("camera.0: latency_s "), "{error}");
        assert!(error.ends_with("is outside 0 to 10 s"), "{error}");
    }
}

#[test]
fn an_invalid_latency_is_not_in_effect() {
    assert!((latency_in_effect_s(Some(2.5)) - 2.5).abs() < f64::EPSILON);
    assert!(latency_in_effect_s(Some(0.0)).abs() < f64::EPSILON);
    assert!(latency_in_effect_s(None).abs() < f64::EPSILON);
    for bad in [-0.1, 10.1, f64::NAN] {
        assert!(latency_in_effect_s(Some(bad)).abs() < f64::EPSILON, "{bad}");
    }
}

#[test]
fn the_config_reports_its_first_invalid_camera_latency() {
    let mut config = FlighthookConfig::default();
    assert_eq!(config.latency_error(), None);
    for (index, latency_s) in [("0", Some(1.0)), ("1", Some(11.0)), ("2", Some(-1.0))] {
        config.camera.insert(
            index.into(),
            CameraSection {
                camera: CameraSection::TEST_PATTERN.into(),
                latency_s,
                ..CameraSection::default()
            },
        );
    }
    let error = config.latency_error().expect("error");
    assert!(error.starts_with("camera.1: latency_s 11 "), "{error}");
}

#[test]
fn enabled_defaults_on_and_is_written_only_when_off() {
    let text = r#"{
            "mevo": {"0": {"name": "Mevo", "address": "192.168.2.1:5100"}},
            "r10": {"0": {"name": "R10"}},
            "square": {"0": {"name": "Omni"}},
            "exputt": {"0": {"name": "ExPutt"}},
            "trackball": {"0": {"name": "Trackball"}},
            "mock_monitor": {"0": {"name": "Mock"}},
            "openconnect_server": {"0": {"name": "Range Bridge"}},
            "openconnect_client": {"0": {"name": "Open Connect"}},
            "pureplay": {"0": {"name": "Pureplay Connect"}},
            "pureplay_accessory": {"0": {"name": "Pureplay"}},
            "random_club": {"0": {"name": "Random"}},
            "camera": {"0": {"name": "Camera"}}
        }"#;
    let mut config: FlighthookConfig = serde_json::from_str(text).expect("de");
    assert!(config.disabled_actors().is_empty(), "absent is on");
    let json = serde_json::to_string(&config).expect("ser");
    assert!(!json.contains("enabled"), "{json}");

    config.mock_monitor.get_mut("0").unwrap().enabled = false;
    config.openconnect_client.get_mut("0").unwrap().enabled = false;
    config.camera.get_mut("0").unwrap().enabled = false;
    assert_eq!(
        config.disabled_actors(),
        ["camera.0", "mock_monitor.0", "openconnect_client.0"]
    );
    assert!(!config.actor_enabled("mock_monitor.0"));
    assert!(config.actor_enabled("mevo.0"));
    assert!(config.actor_enabled("webserver.0"), "not a toggled type");
    assert!(config.actor_enabled("mevo.7"), "not configured");
    let json = serde_json::to_string(&config).expect("ser");
    assert_eq!(json.matches(r#""enabled":false"#).count(), 3, "{json}");
    assert!(!json.contains(r#""enabled":true"#), "{json}");
    let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
    assert_eq!(back.disabled_actors(), config.disabled_actors());
    // Written on: read as on.
    let on: MockMonitorSection =
        serde_json::from_str(r#"{"name":"Mock","enabled":true}"#).expect("de");
    assert!(on.enabled);
}

#[test]
fn a_camera_without_a_latency_serializes_byte_identically() {
    let section = CameraSection {
        camera: CameraSection::TEST_PATTERN.into(),
        ..CameraSection::default()
    };
    assert_eq!(section.latency_s, None);
    let json = serde_json::to_string(&section).expect("ser");
    assert!(!json.contains("latency_s"), "{json}");
}

#[test]
fn a_latency_round_trips_and_zero_is_kept() {
    for (text, expected) in [
        (r#"{"camera":"test-pattern","latency_s":2.5}"#, Some(2.5)),
        (r#"{"camera":"test-pattern","latency_s":0}"#, Some(0.0)),
        (r#"{"camera":"test-pattern"}"#, None),
    ] {
        let s: CameraSection = serde_json::from_str(text).expect("de");
        assert_eq!(s.latency_s, expected, "{text}");
        let back = serde_json::to_string(&s).expect("ser");
        assert_eq!(back.contains("latency_s"), expected.is_some(), "{back}");
    }
}
