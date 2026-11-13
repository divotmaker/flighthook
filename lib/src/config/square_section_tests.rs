//! Tests of the config: square section.

use super::*;

/// Square sections as flighthook 0.1.24 wrote them read into the same
/// values, now in the driver's settings. Only values at their default
/// (`club = "7I"`, `advanced_spin = true`,
/// `discard_non_putting_zero_spin = true`, an empty calibration) are
/// written back without them; everything else writes back the same.
#[test]
fn a_saved_square_section_reads_as_before() {
    let section: SquareSection = serde_json::from_str(
        r#"{"name":"Omni","address":"SquareGolf(CD01)","club":"pw","advanced_spin":false,
                "discard_non_putting_zero_spin":false,"impact_lead_s":1.5,
                "dot_bottom_to_face_center_mm":{"7I":21.5,"DR":19.0}}"#,
    )
    .unwrap();
    assert_eq!(section.name, "Omni");
    assert_eq!(section.impact_lead_s, Some(1.5));
    assert_eq!(section.driver.address.as_deref(), Some("SquareGolf(CD01)"));
    assert_eq!(section.driver.club, "pw");
    assert!(!section.driver.advanced_spin);
    assert!(!section.driver.discard_non_putting_zero_spin);
    assert_eq!(
        section.driver.dot_bottom_to_face_center_mm,
        std::collections::BTreeMap::from([("7I".into(), 21.5), ("DR".into(), 19.0)])
    );
    let back = serde_json::to_value(&section).unwrap();
    assert_eq!(
        back,
        serde_json::json!({"name":"Omni","address":"SquareGolf(CD01)","club":"pw",
                "advanced_spin":false,"discard_non_putting_zero_spin":false,"impact_lead_s":1.5,
                "dot_bottom_to_face_center_mm":{"7I":21.5,"DR":19.0}})
    );

    // What the 0.1.24 card wrote for a new Omni: the default values, the
    // zero-spin switch explicit.
    let section: SquareSection =
        serde_json::from_str(r#"{"name":"Square Golf Omni","discard_non_putting_zero_spin":true}"#)
            .unwrap();
    assert_eq!(section, SquareSection::default());
    let section: SquareSection = serde_json::from_str(
        r#"{"name":"Square Golf Omni","club":"7I","advanced_spin":true,
                "dot_bottom_to_face_center_mm":{}}"#,
    )
    .unwrap();
    assert_eq!(section, SquareSection::default());
    assert_eq!(
        serde_json::to_string(&SquareSection::default()).unwrap(),
        r#"{"name":"Square Golf Omni"}"#
    );
    let off: SquareSection =
        serde_json::from_str(r#"{"name":"Square Golf Omni","enabled":false}"#).unwrap();
    assert!(!off.enabled);
    assert_eq!(
        serde_json::to_string(&off).unwrap(),
        r#"{"name":"Square Golf Omni","enabled":false}"#
    );
}

#[test]
fn the_british_key_is_read_and_the_american_one_written() {
    let s: SquareSection =
        serde_json::from_str(r#"{"name":"Square","dot_bottom_to_face_centre_mm":{"DR":15.0}}"#)
            .expect("parse");
    assert_eq!(s.driver.dot_bottom_to_face_center_mm.get("DR"), Some(&15.0));
    let json = serde_json::to_string(&s).expect("serialize");
    assert!(
        json.contains(r#""dot_bottom_to_face_center_mm":{"DR":15.0}"#),
        "{json}"
    );
    assert!(!json.contains("centre"), "{json}");
}

/// The Square driver refuses nothing it can read; a value of the wrong
/// type fails the section, naming the key.
#[test]
fn the_square_driver_refuses_nothing_it_reads() {
    let mut config = FlighthookConfig::default();
    config.square.insert(
        "0".into(),
        serde_json::from_str(
            r#"{"name":"Omni","club":"no such club","dot_bottom_to_face_center_mm":{"7I":-3}}"#,
        )
        .unwrap(),
    );
    assert_eq!(config.device_error(), None);
    let e = serde_json::from_str::<SquareSection>(r#"{"name":"Omni","advanced_spin":"x"}"#)
        .unwrap_err();
    assert!(e.to_string().contains("invalid type"), "{e}");
}
