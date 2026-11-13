//! Tests of the config: body tracking.

use super::*;

#[test]
fn a_config_without_the_section_reads_and_writes_without_it() {
    let c: FlighthookConfig = serde_json::from_str(r#"{"default_units":"metric"}"#).expect("parse");
    assert!(c.body_tracking.is_default());
    let json = serde_json::to_string(&c).expect("serialize");
    assert!(!json.contains("body_tracking"), "{json}");
    assert_eq!(
        c.body_tracking.mode,
        BodyTrackingMode::Off,
        "off by default"
    );
    // Written out at the defaults, the section is left out again.
    let c: FlighthookConfig = serde_json::from_str(
            r#"{"body_tracking":{"enabled":false,"model":"rtmpose-m","min_score":0.3,"smoothing":1.0,"detect_every":10}}"#,
        )
        .expect("parse");
    assert!(!serde_json::to_string(&c).unwrap().contains("body_tracking"));
}

#[test]
fn only_the_fields_off_their_defaults_are_written() {
    let section = BodyTrackingSection {
        model: PoseModel::RtmposeS,
        detect_every: 30,
        ..BodyTrackingSection::default()
    };
    assert_eq!(
        serde_json::to_string(&section).unwrap(),
        r#"{"model":"rtmpose-s","detect_every":30}"#
    );
    let back: BodyTrackingSection =
        serde_json::from_str(r#"{"model":"rtmpose-s","detect_every":30}"#).unwrap();
    assert_eq!(back, section);
    let live: BodyTrackingSection = serde_json::from_str(r#"{"mode":"live"}"#).unwrap();
    assert!(live.mode == BodyTrackingMode::Live && live.model == PoseModel::RtmposeM);
    assert_eq!(serde_json::to_string(&live).unwrap(), r#"{"mode":"live"}"#);
    let off: BodyTrackingSection = serde_json::from_str(r#"{"mode":"off"}"#).unwrap();
    assert_eq!(off, BodyTrackingSection::default());
    assert!(serde_json::from_str::<BodyTrackingSection>(r#"{"model":"rtmpose-x"}"#).is_err());
    assert!(serde_json::from_str::<BodyTrackingSection>(r#"{"mode":"always"}"#).is_err());
}

#[test]
fn show_skeleton_defaults_on_and_is_written_only_when_off() {
    // A section from before the field: on, and written back unchanged.
    for text in [
        "{}",
        r#"{"mode":"live","min_score":0.45}"#,
        r#"{"mode":"historical","model":"rtmpose-s","detect_every":30}"#,
    ] {
        let section: BodyTrackingSection = serde_json::from_str(text).unwrap();
        assert!(section.show_skeleton, "{text}");
        assert_eq!(serde_json::to_string(&section).unwrap(), text);
    }
    assert!(BodyTrackingSection::default().show_skeleton);
    let hidden = BodyTrackingSection {
        show_skeleton: false,
        ..BodyTrackingSection::default()
    };
    assert!(!hidden.is_default());
    assert_eq!(hidden.error(), None);
    assert!(!hidden.in_effect().show_skeleton);
    let json = serde_json::to_string(&hidden).unwrap();
    assert_eq!(json, r#"{"show_skeleton":false}"#);
    assert_eq!(
        serde_json::from_str::<BodyTrackingSection>(&json).unwrap(),
        hidden
    );
    let shown: BodyTrackingSection = serde_json::from_str(r#"{"show_skeleton":true}"#).unwrap();
    assert!(shown.is_default());
    assert_eq!(serde_json::to_string(&shown).unwrap(), "{}");
}

#[test]
fn each_mode_round_trips_and_only_off_the_default_is_written() {
    for mode in BodyTrackingMode::all() {
        let section = BodyTrackingSection {
            mode,
            ..BodyTrackingSection::default()
        };
        let json = serde_json::to_string(&section).unwrap();
        if mode == BodyTrackingMode::Off {
            assert_eq!(json, "{}");
            assert!(section.is_default());
        } else {
            assert_eq!(json, format!(r#"{{"mode":"{}"}}"#, mode.key()));
        }
        assert_eq!(
            serde_json::from_str::<BodyTrackingSection>(&json).unwrap(),
            section
        );
    }
    assert_eq!(
        BodyTrackingMode::all().map(BodyTrackingMode::label),
        ["Off", "Historical", "Live"]
    );
    assert!(!BodyTrackingMode::Off.is_on() && BodyTrackingMode::Live.is_on());
}

#[test]
fn the_switch_of_an_older_section_reads_as_a_mode_and_only_the_mode_is_written() {
    let read = |text: &str| -> BodyTrackingSection { serde_json::from_str(text).unwrap() };
    let on = read(r#"{"enabled":true,"model":"rtmpose-s"}"#);
    assert_eq!(on.mode, BodyTrackingMode::Historical);
    assert_eq!(on.model, PoseModel::RtmposeS);
    assert_eq!(
        serde_json::to_string(&on).unwrap(),
        r#"{"mode":"historical","model":"rtmpose-s"}"#
    );
    let off = read(r#"{"enabled":false}"#);
    assert_eq!(off.mode, BodyTrackingMode::Off);
    assert!(off.is_default());
    assert_eq!(serde_json::to_string(&off).unwrap(), "{}");
    // Neither: off.
    assert_eq!(read(r#"{"model":"rtmpose-s"}"#).mode, BodyTrackingMode::Off);
    // A mode wins over the switch.
    assert_eq!(
        read(r#"{"enabled":false,"mode":"live"}"#).mode,
        BodyTrackingMode::Live
    );
    // In a whole config.
    let c: FlighthookConfig =
        serde_json::from_str(r#"{"body_tracking":{"enabled":false}}"#).unwrap();
    assert!(c.body_tracking.is_default());
    assert!(!serde_json::to_string(&c).unwrap().contains("body_tracking"));
    let c: FlighthookConfig =
        serde_json::from_str(r#"{"body_tracking":{"enabled":true}}"#).unwrap();
    assert_eq!(c.body_tracking.mode, BodyTrackingMode::Historical);
    assert!(
        serde_json::to_string(&c)
            .unwrap()
            .contains(r#""body_tracking":{"mode":"historical"}"#)
    );
}

#[test]
fn out_of_range_values_are_errors_and_default_in_effect() {
    let ok = BodyTrackingSection::default();
    assert_eq!(ok.error(), None);
    for (bad, field) in [
        (
            BodyTrackingSection {
                min_score: 1.5,
                ..ok.clone()
            },
            "min_score",
        ),
        (
            BodyTrackingSection {
                min_score: f64::NAN,
                ..ok.clone()
            },
            "min_score",
        ),
        (
            BodyTrackingSection {
                smoothing: 0.05,
                ..ok.clone()
            },
            "smoothing",
        ),
        (
            BodyTrackingSection {
                smoothing: 11.0,
                ..ok.clone()
            },
            "smoothing",
        ),
        (
            BodyTrackingSection {
                detect_every: 0,
                ..ok.clone()
            },
            "detect_every",
        ),
        (
            BodyTrackingSection {
                detect_every: 61,
                ..ok.clone()
            },
            "detect_every",
        ),
    ] {
        let error = bad.error().expect(field);
        assert!(error.starts_with(field), "{error}");
        assert_eq!(bad.in_effect(), ok, "{field}");
    }
    // Off, and the ends of each range, are fine.
    for good in [
        BodyTrackingSection {
            smoothing: 0.0,
            min_score: 0.0,
            detect_every: 1,
            ..ok.clone()
        },
        BodyTrackingSection {
            smoothing: BodyTrackingSection::MAX_SMOOTHING_HZ,
            min_score: 1.0,
            detect_every: BodyTrackingSection::MAX_DETECT_EVERY,
            mode: BodyTrackingMode::Off,
            model: PoseModel::RtmposeS,
            show_skeleton: false,
            legacy_golfer_height: None,
        },
    ] {
        assert_eq!(good.error(), None);
        assert_eq!(good.in_effect(), good);
    }
}

#[test]
fn the_player_section_round_trips_and_is_omitted_at_its_defaults() {
    let c = FlighthookConfig::default();
    assert!(c.golfers.is_empty());
    assert!(!serde_json::to_string(&c).unwrap().contains("player"));
    assert!(!serde_json::to_string(&c).unwrap().contains("golfers"));
    assert!(
        !serde_json::to_string(&c)
            .unwrap()
            .contains("selected_golfer")
    );
    let player = PlayerSection {
        sex: Sex::Female,
        height: Some(182.5),
    };
    assert!(!player.is_default());
    let json = serde_json::to_string(&player).unwrap();
    assert_eq!(json, r#"{"sex":"female","height":182.5}"#);
    assert_eq!(
        serde_json::from_str::<PlayerSection>(&json).unwrap(),
        player
    );
    let height_only = PlayerSection {
        height: Some(175.0),
        ..PlayerSection::default()
    };
    assert_eq!(
        serde_json::to_string(&height_only).unwrap(),
        r#"{"height":175.0}"#
    );
    assert_eq!(Sex::all().map(Sex::key), ["male", "female"]);
    assert_eq!(Sex::all().map(Sex::label), ["Male", "Female"]);
    for sex in Sex::all() {
        assert_eq!(
            serde_json::to_value(sex).unwrap(),
            serde_json::Value::from(sex.key())
        );
    }
}

#[test]
fn a_player_section_moves_into_the_golfers() {
    // Without golfers: one golfer "Golfer" with the player's facts, no
    // id yet (the app gives one).
    let mut c: FlighthookConfig =
        serde_json::from_str(r#"{"player":{"sex":"female","height":170}}"#).unwrap();
    assert_eq!(c.golfers.len(), 1);
    assert_eq!(c.golfers[0].name, "Golfer");
    assert_eq!(c.golfers[0].sex, Some(Sex::Female));
    assert_eq!(c.golfers[0].height, Some(170.0));
    assert!(c.golfers[0].own_id().is_none());
    assert!(c.fill_golfer_ids(|| "u1".into()));
    assert_eq!(c.golfers[0].identifiers, ["flighthook.u1"]);
    assert!(!c.fill_golfer_ids(|| "u2".into()), "generated once");
    let written = serde_json::to_string(&c).unwrap();
    assert!(!written.contains("player"), "{written}");
    assert!(
        written.contains(r#""golfers":[{"identifiers":["flighthook.u1"],"name":"Golfer""#),
        "{written}"
    );
    // With golfers: the first takes what it lacks; its own values win.
    let c: FlighthookConfig = serde_json::from_str(
        r#"{"player":{"sex":"female","height":170},"golfers":[
                {"identifiers":["flighthook.a"],"name":"A","height":160},
                {"identifiers":["flighthook.b"],"name":"B"}]}"#,
    )
    .unwrap();
    assert_eq!(c.golfers.len(), 2);
    assert_eq!(c.golfers[0].height, Some(160.0));
    assert_eq!(c.golfers[0].sex, Some(Sex::Female));
    assert_eq!(c.golfers[1].sex, None);
    assert_eq!(c.golfers[1].height, None);
    // An empty or default [player] makes no golfer.
    let c: FlighthookConfig = serde_json::from_str(r#"{"player":{}}"#).unwrap();
    assert!(c.golfers.is_empty());
    let c: FlighthookConfig = serde_json::from_str(r#"{"player":{"sex":"male"}}"#).unwrap();
    assert!(c.golfers.is_empty());
}

#[test]
fn the_lookup_rule_finds_the_golfer_whose_identifiers_hold_the_id() {
    let mut c = FlighthookConfig::default();
    let mut eric = GolferRecord::new("flighthook.e", "Eric");
    eric.identifiers.push("pureplay.local-3".into());
    c.golfers = vec![eric, GolferRecord::new("flighthook.s", "Sam")];
    assert_eq!(
        c.golfer_of("pureplay.local-3").map(|g| g.name.as_str()),
        Some("Eric")
    );
    assert_eq!(
        c.golfer_of("flighthook.s").map(|g| g.name.as_str()),
        Some("Sam")
    );
    assert!(c.golfer_of("pureplay.local-9").is_none());
    assert_eq!(
        c.golfer_key(Some("pureplay.local-3")).as_deref(),
        Some("flighthook.e")
    );
    assert_eq!(
        c.golfer_key(Some("flighthook.e")).as_deref(),
        Some("flighthook.e")
    );
    assert_eq!(
        c.golfer_key(Some("pureplay.local-9")).as_deref(),
        Some("pureplay.local-9"),
        "an unknown id stands on its own"
    );
    assert_eq!(c.golfer_key(None), None);
    assert_eq!(
        c.golfers[0].game_ids().collect::<Vec<_>>(),
        ["pureplay.local-3"]
    );
    // The golfer up at start: selected, else first, else none.
    assert_eq!(c.start_golfer().map(|g| g.name.as_str()), Some("Eric"));
    c.selected_golfer = Some("flighthook.s".into());
    assert_eq!(c.start_golfer().map(|g| g.name.as_str()), Some("Sam"));
    c.selected_golfer = Some("flighthook.gone".into());
    assert_eq!(c.start_golfer().map(|g| g.name.as_str()), Some("Eric"));
    assert!(FlighthookConfig::default().start_golfer().is_none());
}

#[test]
fn one_id_belongs_to_one_golfer() {
    let mut c = FlighthookConfig::default();
    let mut a = GolferRecord::new("flighthook.a", "A");
    a.identifiers.push("pureplay.local-3".into());
    let mut b = GolferRecord::new("flighthook.b", "B");
    b.identifiers.push("pureplay.local-3".into());
    c.golfers = vec![a, b];
    let dup = c.duplicate_golfer_id().expect("duplicate");
    assert_eq!(dup.id, "pureplay.local-3");
    assert_eq!(
        dup.to_string(),
        "'pureplay.local-3' is in two golfers: A and B"
    );
    let removed = c.dedupe_golfer_ids();
    assert_eq!(removed, [dup]);
    assert_eq!(
        c.golfers[0].identifiers,
        ["flighthook.a", "pureplay.local-3"]
    );
    assert_eq!(c.golfers[1].identifiers, ["flighthook.b"]);
    assert!(c.duplicate_golfer_id().is_none());
}

#[test]
fn a_golfer_record_round_trips() {
    let src = r#"{"golfers":[{"identifiers":["flighthook.5b0c","pureplay.local-3"],
            "name":"Eric","handed":"lh","bag":["DR","7I"],"sex":"female","height":182.5}]}"#;
    let c: FlighthookConfig = serde_json::from_str(src).unwrap();
    let g = &c.golfers[0];
    assert_eq!(g.own_id(), Some("flighthook.5b0c"));
    assert_eq!(g.handed, Some(Handedness::Left));
    assert_eq!(g.bag, [Club::Driver, Club::Iron7]);
    assert_eq!(g.facts().height, Some(182.5));
    let back: FlighthookConfig = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back.golfers, c.golfers);
    let mut tall = g.clone();
    tall.height = Some(300.0);
    assert!(tall.error().is_some());
    assert_eq!(tall.facts().height, None);
}

#[test]
fn a_player_height_is_validated() {
    for good in [
        None,
        Some(PlayerSection::MIN_HEIGHT_CM),
        Some(180.0),
        Some(PlayerSection::MAX_HEIGHT_CM),
    ] {
        let s = PlayerSection {
            height: good,
            sex: Sex::Female,
        };
        assert_eq!(s.error(), None, "{good:?}");
        assert_eq!(s.in_effect(), s);
    }
    for bad in [99.0, 99.9, 250.5, f64::NAN, f64::INFINITY, -180.0] {
        let s = PlayerSection {
            height: Some(bad),
            sex: Sex::Female,
        };
        let error = s.error().expect("out of range");
        assert!(error.starts_with("height"), "{error}");
        assert_eq!(
            s.in_effect(),
            PlayerSection {
                height: None,
                sex: Sex::Female
            },
            "{bad}"
        );
    }
    assert_eq!(
        PlayerSection {
            height: Some(99.0),
            ..PlayerSection::default()
        }
        .error()
        .as_deref(),
        Some("height 99 is outside 100 to 250 cm")
    );
}

#[test]
fn a_golfer_height_in_body_tracking_moves_to_the_player() {
    let c: FlighthookConfig =
        serde_json::from_str(r#"{"body_tracking":{"golfer_height":182.5}}"#).unwrap();
    assert_eq!(c.golfers[0].height, Some(182.5));
    assert_eq!(c.body_tracking.legacy_golfer_height, None);
    assert!(c.body_tracking.is_default());
    // Never written back where it came from.
    let json = serde_json::to_string(&c).unwrap();
    assert!(!json.contains("golfer_height"), "{json}");
    assert!(!json.contains("body_tracking"), "{json}");
    assert!(!json.contains("player"), "{json}");
    assert!(json.contains(r#""height":182.5"#), "{json}");
    // The player's own height wins.
    let c: FlighthookConfig = serde_json::from_str(
        r#"{"body_tracking":{"golfer_height":182.5},"player":{"height":170.0}}"#,
    )
    .unwrap();
    assert_eq!(c.golfers[0].height, Some(170.0));
    // Beside the other body tracking settings.
    let c: FlighthookConfig =
        serde_json::from_str(r#"{"body_tracking":{"mode":"live","golfer_height":182.5}}"#).unwrap();
    assert_eq!(c.golfers[0].height, Some(182.5));
    assert_eq!(c.body_tracking.mode, BodyTrackingMode::Live);
    // A section read alone carries it, and it never counts as a setting.
    let section: BodyTrackingSection = serde_json::from_str(r#"{"golfer_height":182.5}"#).unwrap();
    assert_eq!(section.legacy_golfer_height, Some(182.5));
    assert!(section.is_default());
    assert_eq!(section, BodyTrackingSection::default());
    assert_eq!(serde_json::to_string(&section).unwrap(), "{}");
    assert_eq!(section.in_effect().legacy_golfer_height, None);
}

#[test]
fn camera_angles_and_rotation_are_omitted_at_their_defaults() {
    let s = CameraSection {
        camera: CameraSection::TEST_PATTERN.into(),
        ..CameraSection::default()
    };
    let v = serde_json::to_value(&s).unwrap();
    for key in ["rotation_deg", "yaw_deg", "pitch_deg"] {
        assert!(v.get(key).is_none(), "{key}: {v}");
    }
    let turned = CameraSection {
        rotation_deg: 90,
        yaw_deg: Some(0.0),
        pitch_deg: Some(-3.5),
        ..s.clone()
    };
    let v = serde_json::to_value(&turned).unwrap();
    assert_eq!(v["rotation_deg"], 90);
    assert_eq!(v["yaw_deg"], 0.0);
    assert_eq!(v["pitch_deg"], -3.5);
    let back: CameraSection = serde_json::from_value(v).unwrap();
    assert_eq!(back, turned);
    // An old section loads with no rotation and no angles.
    let old: CameraSection = serde_json::from_str(
        r#"{"name":"Face","camera":"test-pattern","position":"golfer_front","mode":"auto"}"#,
    )
    .unwrap();
    assert_eq!(old.rotation_deg, 0);
    assert_eq!(old.yaw_deg, None);
    assert_eq!(old.pitch_deg, None);
}

#[test]
fn camera_angles_and_rotation_are_validated() {
    let ok = CameraSection {
        camera: CameraSection::TEST_PATTERN.into(),
        ..CameraSection::default()
    };
    for rotation in CAMERA_ROTATIONS {
        let s = CameraSection {
            rotation_deg: rotation,
            ..ok.clone()
        };
        assert_eq!(s.error("camera.0"), None);
        assert_eq!(rotation_in_effect_deg(rotation), rotation);
    }
    for angle in [None, Some(-30.0), Some(0.0), Some(12.5), Some(30.0)] {
        let s = CameraSection {
            yaw_deg: angle,
            pitch_deg: angle,
            ..ok.clone()
        };
        assert_eq!(s.error("camera.0"), None, "{angle:?}");
        assert_eq!(camera_angle_in_effect_deg(angle), angle.unwrap_or(0.0));
    }
    let cases = [
        (
            CameraSection {
                rotation_deg: 45,
                ..ok.clone()
            },
            "camera.2: rotation_deg 45 is not 0, 90, 180 or 270",
        ),
        (
            CameraSection {
                yaw_deg: Some(40.0),
                ..ok.clone()
            },
            "camera.2: yaw_deg 40 is outside -30 to 30",
        ),
        (
            CameraSection {
                yaw_deg: Some(-30.5),
                ..ok.clone()
            },
            "camera.2: yaw_deg -30.5 is outside -30 to 30",
        ),
        (
            CameraSection {
                pitch_deg: Some(f64::NAN),
                ..ok.clone()
            },
            "camera.2: pitch_deg NaN is outside -30 to 30",
        ),
        (
            CameraSection {
                latency_s: Some(11.0),
                rotation_deg: 45,
                ..ok.clone()
            },
            "camera.2: latency_s 11 is outside 0 to 10 s",
        ),
    ];
    for (section, message) in cases {
        assert_eq!(section.error("camera.2").as_deref(), Some(message));
    }
    assert_eq!(rotation_in_effect_deg(45), 0);
    assert_eq!(camera_angle_in_effect_deg(Some(40.0)), 0.0);
    assert_eq!(camera_angle_in_effect_deg(Some(f64::INFINITY)), 0.0);
    // The config reports the first camera by index.
    let mut c = FlighthookConfig::default();
    c.camera.insert(
        "1".into(),
        CameraSection {
            pitch_deg: Some(31.0),
            ..ok.clone()
        },
    );
    c.camera.insert(
        "0".into(),
        CameraSection {
            rotation_deg: 1,
            ..ok.clone()
        },
    );
    assert_eq!(
        c.camera_error().as_deref(),
        Some("camera.0: rotation_deg 1 is not 0, 90, 180 or 270")
    );
    c.camera.remove("0");
    assert_eq!(
        c.camera_error().as_deref(),
        Some("camera.1: pitch_deg 31 is outside -30 to 30")
    );
}
