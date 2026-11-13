//! Tests of the config: simulation.

use super::*;

/// `FlighthookConfig::default()` as JSON (`GET /api/settings`), with no
/// `[simulation]` key. A default config must not emit one.
const GOLDEN_DEFAULT_JSON: &str =
    r#"{"default_units":"imperial","webserver":{"0":{"name":"Web Server","bind":"0.0.0.0:5880"}}}"#;

/// The club lists that were global settings go into every Open Connect
/// client, and are not written at the top level again.
#[test]
fn the_global_club_lists_move_into_every_open_connect_client() {
    let c: FlighthookConfig = serde_json::from_str(
        r#"{"chipping_clubs":["SW"],"putting_clubs":[],
                "openconnect_client":{"0":{"name":"GSPro"}},
                "gspro":{"1":{"name":"Muni Golf"}}}"#,
    )
    .expect("parse");
    for section in c.openconnect_client.values() {
        assert_eq!(section.chipping_clubs, vec![Club::SandWedge]);
        assert!(section.putting_clubs.is_empty());
    }
    assert_eq!(c.openconnect_client.len(), 2);
    let json = serde_json::to_value(&c).expect("ser");
    assert!(json.get("chipping_clubs").is_none(), "{json}");
    assert_eq!(
        json["openconnect_client"]["0"]["chipping_clubs"],
        serde_json::json!(["SW"])
    );
}

/// A new Open Connect client holds GSPro's lists; both lists empty
/// means the card sends no mode.
#[test]
fn an_open_connect_client_sets_the_mode_from_its_club_lists() {
    let card = OpenConnectClientSection::default();
    assert_eq!(card.club_mode(Some(Club::SandWedge)), Some(Mode::Chipping));
    assert_eq!(card.club_mode(Some(Club::Putter)), Some(Mode::Putting));
    assert_eq!(card.club_mode(Some(Club::Iron7)), Some(Mode::Full));
    assert_eq!(card.club_mode(None), Some(Mode::Full));
    let none = OpenConnectClientSection {
        chipping_clubs: Vec::new(),
        putting_clubs: Vec::new(),
        ..OpenConnectClientSection::default()
    };
    assert_eq!(none.club_mode(Some(Club::Putter)), None);
}

/// Every actor gets an id once, and routing by card key moves to the
/// card's id; a key naming no card stays as it is.
#[test]
fn actor_ids_are_generated_once_and_routing_follows_them() {
    let mut c: FlighthookConfig = serde_json::from_str(
        r#"{"mevo":{"1":{"name":"Xi"}},
                "openconnect_client":{"0":{"name":"GSPro","chipping_monitor":"mevo.1",
                                           "putting_monitor":"exputt.4"}}}"#,
    )
    .expect("parse");
    let mut n = 0;
    let mut next = || {
        n += 1;
        format!("{n:016x}")
    };
    assert!(c.fill_actor_ids(&mut next));
    assert!(!c.fill_actor_ids(&mut next), "ids are kept");
    let xi = c.actor_of_card("mevo.1").expect("id");
    assert_eq!(c.card_of_actor(&xi).as_deref(), Some("mevo.1"));
    assert!(c.frp.actor_id.is_some() && c.frp.auto_chip_actor_id.is_some());
    assert!(c.simulation.actor_id.is_some());
    assert!(c.route_by_actor_id());
    let gspro = &c.openconnect_client["0"];
    assert_eq!(gspro.chipping_monitor.as_deref(), Some(xi.as_str()));
    assert_eq!(gspro.putting_monitor.as_deref(), Some("exputt.4"));
    assert!(!c.route_by_actor_id());
}

#[test]
fn default_config_json_is_unchanged() {
    let json = serde_json::to_string(&FlighthookConfig::default()).expect("ser");
    assert_eq!(json, GOLDEN_DEFAULT_JSON);
    assert!(!json.contains("simulation"));
}

#[test]
fn shot_grouping_is_omitted_at_default() {
    let c: FlighthookConfig = serde_json::from_str(GOLDEN_DEFAULT_JSON).expect("parses");
    assert_eq!(c.shot_grouping, ShotGrouping::BySet);
    let json = serde_json::to_string(&c).expect("ser");
    assert!(!json.contains("shot_grouping"), "{json}");
}

#[test]
fn shot_grouping_round_trips() {
    for (mode, name) in [
        (ShotGrouping::None, "none"),
        (ShotGrouping::ByClub, "by_club"),
    ] {
        let c = FlighthookConfig {
            shot_grouping: mode,
            ..FlighthookConfig::default()
        };
        let v = serde_json::to_value(&c).expect("ser");
        assert_eq!(v["shot_grouping"], name);
        let back: FlighthookConfig = serde_json::from_value(v).expect("de");
        assert_eq!(back.shot_grouping, mode);
    }
    // The default mode is omitted even when set explicitly.
    let c = FlighthookConfig {
        shot_grouping: ShotGrouping::BySet,
        ..FlighthookConfig::default()
    };
    let json = serde_json::to_string(&c).expect("ser");
    assert!(!json.contains("shot_grouping"), "{json}");
    let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
    assert_eq!(back.shot_grouping, ShotGrouping::BySet);
    // An explicit `none` round-trips.
    let c: FlighthookConfig = serde_json::from_str(r#"{"shot_grouping":"none"}"#).expect("parses");
    assert_eq!(c.shot_grouping, ShotGrouping::None);
}

#[test]
fn absent_section_parses_to_defaults() {
    let c: FlighthookConfig = serde_json::from_str(GOLDEN_DEFAULT_JSON).expect("parses");
    assert_eq!(c.simulation, SimulationSection::default());
    assert_eq!(c.simulation.mode, SimMode::On);
    assert_eq!(c.simulation.altitude.as_meters(), 0.0);
    assert_eq!(c.simulation.temperature_c, 25.0);
    assert_eq!(c.simulation.surface, SimSurface::Fairway);
    assert_eq!(c, FlighthookConfig::default());
}

#[test]
fn partial_section_fills_defaults() {
    let s: SimulationSection = serde_json::from_str(r#"{"mode":"off"}"#).expect("parses");
    assert_eq!(s.mode, SimMode::Off);
    assert_eq!(s.temperature_c, 25.0);
    let s: SimulationSection =
        serde_json::from_str(r#"{"altitude":"5280ft","surface":"firm_green"}"#).expect("parses");
    assert_eq!(s.mode, SimMode::On);
    assert!((s.altitude.as_meters() - 1609.344).abs() < 1e-9);
    assert_eq!(s.surface, SimSurface::FirmGreen);
}

#[test]
fn zero_altitude_in_any_unit_is_default() {
    for altitude in [
        Distance::Feet(0.0),
        Distance::Meters(0.0),
        Distance::Yards(0.0),
    ] {
        let s = SimulationSection {
            altitude,
            ..SimulationSection::default()
        };
        assert!(s.is_default(), "{altitude:?}");
    }
    let changed = SimulationSection {
        temperature_c: 30.0,
        ..SimulationSection::default()
    };
    assert!(!changed.is_default());
    let json = serde_json::to_string(&FlighthookConfig {
        simulation: changed,
        ..FlighthookConfig::default()
    })
    .expect("ser");
    assert!(
        json.contains(r#""simulation":{"temperature_c":30.0}"#),
        "{json}"
    );
}

/// Auto-chip is disabled by default and written only when enabled; an
/// enabled one survives a JSON round trip and makes the section
/// non-default.
#[test]
fn auto_chip_defaults_off_and_round_trips_when_on() {
    assert!(!SimulationSection::default().auto_chip);
    assert!(SimulationSection::default().is_default());
    let s: SimulationSection = serde_json::from_str("{}").expect("parses");
    assert!(!s.auto_chip, "an absent key is disabled");
    let json = serde_json::to_string(&SimulationSection::default()).expect("ser");
    assert!(!json.contains("auto_chip"), "{json}");

    let on = SimulationSection {
        auto_chip: true,
        ..SimulationSection::default()
    };
    assert!(!on.is_default());
    let json = serde_json::to_string(&on).expect("ser");
    assert!(json.contains(r#""auto_chip":true"#), "{json}");
    let back: SimulationSection = serde_json::from_str(&json).expect("parses");
    assert_eq!(back, on);
}

/// `tournament` is off by default and absent while off; on, it
/// round-trips.
#[test]
fn tournament_defaults_off_and_round_trips_when_on() {
    let off = FlighthookConfig::default();
    assert!(!off.simulation.tournament);
    let text = serde_json::to_string(&off).expect("ser");
    assert!(!text.contains("tournament"), "{text}");
    let s: SimulationSection = serde_json::from_str("{}").expect("parses");
    assert!(!s.tournament, "an absent key is off");

    let mut on = FlighthookConfig::default();
    on.simulation.tournament = true;
    assert!(!on.simulation.is_default());
    let text = serde_json::to_string(&on).expect("ser");
    assert!(text.contains(r#""tournament":true"#), "{text}");
    let back: FlighthookConfig = serde_json::from_str(&text).expect("parses");
    assert!(back.simulation.tournament);
}

#[test]
fn environment_bounds() {
    let ok = |altitude, temperature_c| {
        SimulationSection {
            altitude,
            temperature_c,
            ..SimulationSection::default()
        }
        .environment_error()
        .is_none()
    };
    assert!(ok(Distance::Meters(0.0), 25.0));
    assert!(ok(Distance::Meters(5000.0), -30.0));
    assert!(ok(Distance::Feet(16_000.0), 50.0));
    assert!(!ok(Distance::Feet(16_500.0), 25.0));
    assert!(!ok(Distance::Meters(-0.1), 25.0));
    assert!(!ok(Distance::Meters(f64::NAN), 25.0));
    assert!(!ok(Distance::Meters(f64::INFINITY), 25.0));
    assert!(!ok(Distance::Meters(0.0), -30.1));
    assert!(!ok(Distance::Meters(0.0), 50.1));
    assert!(!ok(Distance::Meters(0.0), f64::NAN));
}

/// A `simulation` object carrying unknown keys (e.g. `enabled`) loads,
/// ignoring them and keeping the default mode.
#[test]
fn unknown_simulation_keys_are_ignored_in_json() {
    let s: SimulationSection =
        serde_json::from_str(r#"{"enabled":false,"unknown":true}"#).expect("parses");
    assert_eq!(s.mode, SimMode::On);
    assert_eq!(s.temperature_c, SimulationSection::DEFAULT_TEMPERATURE_C);
    let wrapped = r#"{"simulation":{"enabled":false,"unknown":true}}"#;
    let c: FlighthookConfig = serde_json::from_str(wrapped).expect("parses");
    assert_eq!(c.simulation.mode, SimMode::On);
}

#[test]
fn mode_round_trips_and_is_omitted_at_default() {
    for mode in SimMode::all() {
        let c = FlighthookConfig {
            simulation: SimulationSection {
                mode,
                ..SimulationSection::default()
            },
            ..FlighthookConfig::default()
        };
        assert_eq!(c.simulation.is_default(), mode == SimMode::On);

        let json = serde_json::to_string(&c).expect("ser");
        assert!(!json.contains("enabled"), "{json}");
        if mode == SimMode::On {
            assert!(!json.contains("simulation"), "{json}");
        } else {
            let want = format!(r#""simulation":{{"mode":"{}"}}"#, mode.key());
            assert!(json.contains(&want), "{json}");
        }
        let back: FlighthookConfig = serde_json::from_str(&json).expect("de");
        assert_eq!(back, c);
    }
    // Any other non-default setting writes the section without `mode`.
    let warm = SimulationSection {
        temperature_c: 30.0,
        ..SimulationSection::default()
    };
    let json = serde_json::to_string(&warm).expect("ser");
    assert!(!json.contains("mode"), "{json}");
}

#[test]
fn mode_keys_match_serde() {
    for m in SimMode::all() {
        let json = serde_json::to_string(&m).expect("ser");
        assert_eq!(json, format!("\"{}\"", m.key()));
    }
    assert_eq!(SimMode::default(), SimMode::On);
}

#[test]
fn surface_keys_match_serde() {
    for s in SimSurface::all() {
        let json = serde_json::to_string(&s).expect("ser");
        assert_eq!(json, format!("\"{}\"", s.key()));
    }
}
