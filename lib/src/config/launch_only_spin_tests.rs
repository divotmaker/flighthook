//! Tests of the config: launch only spin.

use super::*;

#[test]
fn absent_launch_only_spin_defaults_to_100() {
    let s: MevoSection = serde_json::from_str(r#"{"name":"Mevo"}"#).expect("parse");
    assert_eq!(s.launch_only_spin, 100.0);
    assert_eq!(s.launch_only_spin_rpm_per_mph(), 100.0);
    assert_eq!(MevoSection::default().launch_only_spin, 100.0);
}

#[test]
fn default_launch_only_spin_is_not_written_back() {
    let json = serde_json::to_string(&MevoSection::default()).expect("serialize");
    assert!(!json.contains("launch_only_spin"), "{json}");
    let s: MevoSection =
        serde_json::from_str(r#"{"name":"Mevo","launch_only_spin":100}"#).expect("parse");
    let json = serde_json::to_string(&s).expect("serialize");
    assert!(!json.contains("launch_only_spin"), "{json}");
}

#[test]
fn zero_and_other_values_round_trip() {
    for v in [0.0, 80.0, 125.5] {
        let json = format!(r#"{{"name":"Mevo","launch_only_spin":{v}}}"#);
        let s: MevoSection = serde_json::from_str(&json).expect("parse");
        assert_eq!(s.launch_only_spin, v);
        let out = serde_json::to_string(&s).expect("serialize");
        assert!(out.contains("launch_only_spin"), "{out}");
        let back: MevoSection = serde_json::from_str(&out).expect("parse");
        assert_eq!(back, s);
        assert_eq!(back.launch_only_spin_rpm_per_mph(), v);
    }
}

#[test]
fn negative_non_finite_or_above_1000_is_off() {
    for v in [
        -1.0,
        1000.001,
        1e12,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert!(!MevoSection::launch_only_spin_valid(v), "{v}");
        let s = MevoSection {
            launch_only_spin: v,
            ..MevoSection::default()
        };
        assert_eq!(s.launch_only_spin_rpm_per_mph(), 0.0, "{v}");
    }
    assert!(MevoSection::launch_only_spin_valid(0.0));
    assert!(MevoSection::launch_only_spin_valid(1000.0));
    assert_eq!(MevoSection::MAX_LAUNCH_ONLY_SPIN, 1000.0);
}
