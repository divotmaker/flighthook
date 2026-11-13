//! Tests of the config: camera.

use super::*;

#[test]
fn replay_defaults_are_omitted_and_read_back() {
    let config = FlighthookConfig::default();
    let json = serde_json::to_string(&config).expect("ser");
    assert!(!json.contains("replay"), "{json}");
    assert!(!json.contains("camera"), "{json}");
    let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
    assert_eq!(back.replay, ReplaySection::default());
    let r = ReplaySection::default();
    assert_eq!((r.keep_shots, r.pre_roll_s, r.post_roll_s), (20, 4.0, 1.5));
    assert_eq!(r.start_before_impact_s, 2.0);
    assert_eq!(r.clip_dir, None);
    assert!(r.error().is_none());
    // A section with only some keys takes the defaults for the rest.
    let r: ReplaySection = serde_json::from_str(r#"{"keep_shots":5}"#).expect("de");
    assert_eq!((r.keep_shots, r.pre_roll_s, r.post_roll_s), (5, 4.0, 1.5));
    assert_eq!(r.start_before_impact_s, 2.0);
}

#[test]
fn a_changed_replay_section_round_trips() {
    let config = FlighthookConfig {
        replay: ReplaySection {
            keep_shots: 50,
            pre_roll_s: 3.0,
            post_roll_s: 2.0,
            start_before_impact_s: 3.5,
            clip_dir: Some("D:/swings".into()),
            shot_cards: false,
        },
        ..FlighthookConfig::default()
    };
    let v = serde_json::to_value(&config).expect("ser");
    assert_eq!(v["replay"]["keep_shots"], 50);
    assert_eq!(v["replay"]["start_before_impact_s"], 3.5);
    assert_eq!(v["replay"]["clip_dir"], "D:/swings");
    assert_eq!(v["replay"]["shot_cards"], false);
    let back: FlighthookConfig = serde_json::from_value(v).expect("de");
    assert_eq!(back.replay, config.replay);
}

#[test]
fn shot_cards_default_on_and_are_written_only_when_off() {
    // A section without the key: on, and written back without it.
    for text in ["{}", r#"{"keep_shots":5}"#, r#"{"clip_dir":"D:/swings"}"#] {
        let section: ReplaySection = serde_json::from_str(text).expect("de");
        assert!(section.shot_cards, "{text}");
        let json = serde_json::to_string(&section).expect("ser");
        assert!(!json.contains("shot_cards"), "{json}");
    }
    assert!(ReplaySection::default().shot_cards);
    let off = ReplaySection {
        shot_cards: false,
        ..ReplaySection::default()
    };
    assert!(!off.is_default());
    assert_eq!(off.error(), None);
    let config = FlighthookConfig {
        replay: off.clone(),
        ..FlighthookConfig::default()
    };
    let json = serde_json::to_string(&config).expect("ser");
    assert!(json.contains(r#""shot_cards":false"#), "{json}");
    let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
    assert_eq!(back.replay, off);
    let on: ReplaySection = serde_json::from_str(r#"{"shot_cards":true}"#).expect("de");
    assert!(on.is_default());
    let config = FlighthookConfig {
        replay: on,
        ..FlighthookConfig::default()
    };
    let json = serde_json::to_string(&config).expect("ser");
    assert!(!json.contains("replay"), "{json}");
}

#[test]
fn replay_values_out_of_range_are_errors() {
    let bad = [
        ReplaySection {
            keep_shots: 0,
            ..ReplaySection::default()
        },
        ReplaySection {
            keep_shots: ReplaySection::MAX_KEEP_SHOTS + 1,
            ..ReplaySection::default()
        },
        ReplaySection {
            pre_roll_s: -0.1,
            ..ReplaySection::default()
        },
        ReplaySection {
            post_roll_s: 10.5,
            ..ReplaySection::default()
        },
        ReplaySection {
            pre_roll_s: f64::NAN,
            ..ReplaySection::default()
        },
        ReplaySection {
            start_before_impact_s: -0.1,
            ..ReplaySection::default()
        },
        ReplaySection {
            start_before_impact_s: 10.5,
            ..ReplaySection::default()
        },
        ReplaySection {
            start_before_impact_s: f64::INFINITY,
            ..ReplaySection::default()
        },
    ];
    for r in bad {
        assert!(r.error().is_some(), "{r:?}");
    }
    let edge = ReplaySection {
        keep_shots: 1,
        pre_roll_s: 0.0,
        post_roll_s: ReplaySection::MAX_ROLL_S,
        start_before_impact_s: ReplaySection::MAX_START_BEFORE_IMPACT_S,
        clip_dir: None,
        shot_cards: true,
    };
    assert!(edge.error().is_none());
    let zero = ReplaySection {
        start_before_impact_s: 0.0,
        ..edge
    };
    assert!(zero.error().is_none());
}

#[test]
fn camera_sections_round_trip_with_snake_case_positions() {
    let keys = [
        (CameraPosition::TargetLine, "target_line"),
        (CameraPosition::GolferFront, "golfer_front"),
        (CameraPosition::GolferBack, "golfer_back"),
        (CameraPosition::Overhead, "overhead"),
    ];
    // The settings picker lists `all()` in this order.
    assert_eq!(CameraPosition::all(), keys.map(|(position, _)| position));
    assert_eq!(
        CameraPosition::all().map(CameraPosition::label),
        ["Target Line", "Golfer Front", "Golfer Back", "Overhead"]
    );
    for (position, key) in keys {
        assert_eq!(position.key(), key);
        let section = CameraSection {
            name: "DTL".into(),
            enabled: true,
            camera: "/dev/v4l/by-id/usb-cam-video-index0".into(),
            camera_name: Some("HD Pro Webcam C920".into()),
            position,
            mode: "1280x720@120".into(),
            latency_s: Some(2.5),
            rotation_deg: 90,
            yaw_deg: Some(4.0),
            pitch_deg: None,
        };
        let v = serde_json::to_value(&section).expect("ser");
        assert_eq!(v["position"], key);
        let back: CameraSection = serde_json::from_value(v).expect("de");
        assert_eq!(back, section);
    }
}

#[test]
fn a_camera_section_needs_only_its_camera() {
    let s: CameraSection =
        serde_json::from_str(r#"{"name":"Cam","camera":"test-pattern"}"#).expect("de");
    assert_eq!(s.position, CameraPosition::GolferFront);
    assert_eq!(s.mode, CameraSection::AUTO_MODE);
    assert_eq!(s.camera_name, None);
    assert_eq!(s.camera, CameraSection::TEST_PATTERN);
}

#[test]
fn cameras_count_as_user_actors_and_round_trip_in_toml_shape() {
    let mut config = FlighthookConfig::default();
    assert!(!config.has_user_actors());
    config.camera.insert(
        "0".into(),
        CameraSection {
            camera: CameraSection::TEST_PATTERN.into(),
            ..CameraSection::default()
        },
    );
    assert!(config.has_user_actors());
    let v = serde_json::to_value(&config).expect("ser");
    assert_eq!(v["camera"]["0"]["camera"], "test-pattern");
    assert_eq!(v["camera"]["0"]["mode"], "auto");
    let back: FlighthookConfig = serde_json::from_value(v).expect("de");
    assert_eq!(back, config);
}
