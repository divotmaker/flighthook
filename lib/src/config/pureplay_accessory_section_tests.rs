//! Tests of the config: pureplay accessory section.

use super::*;

/// A new Pureplay Accessory card writes its name and address: the shots setting
/// is left out at Auto, and the impact lead at the default. Each
/// setting reads back, and the card's lead counts as a launch
/// monitor's.
#[test]
fn a_new_pureplay_card_leaves_the_defaults_out() {
    let section = PureplayAccessorySection::default();
    assert_eq!(
        serde_json::to_string(&section).unwrap(),
        r#"{"name":"Pureplay Accessory","address":"127.0.0.1:9770"}"#
    );
    for (shots, key) in [
        (PureplayShots::FromPureplay, "from_pureplay"),
        (PureplayShots::FromFlighthook, "from_flighthook"),
    ] {
        let s = PureplayAccessorySection {
            shots,
            impact_lead_s: Some(3.0),
            ..PureplayAccessorySection::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["shots"], key);
        assert_eq!(shots.key(), key);
        let back: PureplayAccessorySection = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }
    let mut config = FlighthookConfig::default();
    config.pureplay_accessory.insert(
        "0".into(),
        PureplayAccessorySection {
            impact_lead_s: Some(9.0),
            ..PureplayAccessorySection::default()
        },
    );
    assert_eq!(
        config.configured_impact_lead_s("pureplay_accessory.0"),
        Some(9.0)
    );
    assert!(config.impact_lead_error().is_some());
    assert_eq!(
        impact_lead_in_effect_s("pureplay_accessory", Some(9.0)),
        Some(0.9)
    );
    // A card that never set it takes the default; a value set stays.
    assert_eq!(
        impact_lead_in_effect_s("pureplay_accessory", None),
        Some(0.9)
    );
    assert_eq!(
        impact_lead_in_effect_s("pureplay_accessory", Some(2.5)),
        Some(2.5)
    );
    // A card named "Pureplay" (as an earlier default had it) loads.
    let old: PureplayAccessorySection =
        serde_json::from_str(r#"{"name":"Pureplay","address":"127.0.0.1:9770"}"#).unwrap();
    assert_eq!(old.name, "Pureplay");
    assert_eq!(old.impact_lead_s, None);
}
