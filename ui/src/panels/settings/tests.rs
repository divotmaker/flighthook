//! Tests of the Settings form: loading, dirty state, saves, validation and requests.

use std::collections::HashMap;

use super::is_ble_address;
use super::{ActorFormEntry, GeneralCard, SettingsForm, SimulationForm};
use crate::panels::cards::{self, CardGroup};
use crate::panels::devices::device_card_order;
use crate::types::{
    ActorStatus, ActorStatusResponse, Club, Distance, FlighthookConfig, MevoSection,
    MockMonitorSection, RandomClubSection, SimSurface, SimulationSection, UnitSystem,
};
use flighthook::{MockConfig, R10Section, SimMode};

#[test]
fn settings_are_requested_at_most_every_two_seconds_until_loaded() {
    let mut form = SettingsForm::default();
    assert!(form.fetch_due(10.0));
    // Not every frame while the request is out.
    assert!(!form.fetch_due(10.1));
    assert!(!form.fetch_due(11.9));
    // Unanswered: asked again.
    assert!(form.fetch_due(12.0));
    form.loaded = true;
    assert!(!form.fetch_due(100.0));
    // A save reloads at once.
    form.loaded = false;
    form.fetched_at = None;
    assert!(form.fetch_due(100.1));
}

/// A shot's grades follow the golfer who hit it, so the
/// key is that golfer's (by the lookup rule) and their facts: another
/// golfer up, or another golfer's height, leaves it alone; their own
/// height changes it.
#[test]
fn the_grades_key_is_the_shots_golfers() {
    let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
    eric.identifiers.push("pureplay.local-3".into());
    eric.height = Some(180.0);
    let mut sam = flighthook::GolferRecord::new("flighthook.s", "Sam");
    sam.height = Some(165.0);
    let mut config = FlighthookConfig {
        golfers: vec![eric, sam],
        ..FlighthookConfig::default()
    };
    let mut form = SettingsForm::default();
    form.load_from(&config);
    let eric_key = form.grades_key(Some("flighthook.e"));
    let sam_key = form.grades_key(Some("flighthook.s"));
    assert_ne!(eric_key, sam_key);
    assert_eq!(form.grades_key(Some("pureplay.local-3")), eric_key);
    assert_ne!(form.grades_key(None), eric_key);
    assert_ne!(form.grades_key(Some("backnine.p7")), eric_key);
    config.golfers[1].height = Some(170.0);
    form.load_from(&config);
    assert_eq!(form.grades_key(Some("flighthook.e")), eric_key);
    config.golfers[0].height = Some(181.0);
    form.load_from(&config);
    assert_ne!(form.grades_key(Some("flighthook.e")), eric_key);
}

/// IDs of the device cards of `form` by group, none of them running.
fn card_ids(form: &SettingsForm) -> Vec<(CardGroup, Vec<String>)> {
    device_card_order(&HashMap::new(), &form.actors)
        .into_iter()
        .map(|(g, cards)| (g, cards.into_iter().map(|c| c.id).collect()))
        .collect()
}

/// A form with two of every type the (+) menus offer, plus a mock
/// monitor and a random club, loaded from the config they save as.
fn form_with_every_type() -> SettingsForm {
    let mut form = SettingsForm::default();
    form.load_from(&FlighthookConfig::default());
    for group in CardGroup::ALL {
        for preset in cards::add_options(group) {
            form.add_preset(preset);
            form.add_preset(preset);
        }
    }
    let mut config = form.to_request();
    config.mock_monitor.insert(
        "0".into(),
        MockMonitorSection {
            name: "Mock Monitor".into(),
            enabled: true,
            actor_id: None,
            impact_lead_s: None,
            driver: Default::default(),
        },
    );
    config.random_club.insert(
        "0".into(),
        RandomClubSection {
            name: "Random Club".into(),
            enabled: true,
            actor_id: None,
        },
    );
    // No menu entry adds a Pureplay Connect output; the file can.
    for i in ["0", "1"] {
        config.pureplay.insert(
            i.into(),
            crate::types::PureplaySection {
                name: "Pureplay Connect".into(),
                ..Default::default()
            },
        );
    }
    let mut loaded = SettingsForm::default();
    loaded.load_from(&config);
    loaded
}

#[test]
fn settings_cards_group_by_type_then_index() {
    let form = form_with_every_type();
    let order = card_ids(&form);
    let ids = |group: CardGroup| -> Vec<String> {
        order.iter().find(|(g, _)| *g == group).unwrap().1.clone()
    };
    assert_eq!(ids(CardGroup::General), ["webserver.0"]);
    assert_eq!(
        ids(CardGroup::LaunchMonitors),
        [
            "mevo.0",
            "mevo.1",
            "r10.0",
            "r10.1",
            "square.0",
            "square.1",
            "exputt.0",
            "exputt.1",
            "trackball.0",
            "trackball.1",
            "mock_monitor.0",
            "openconnect_server.0",
            "openconnect_server.1",
        ]
    );
    assert_eq!(
        ids(CardGroup::Software),
        [
            "openconnect_client.0",
            "openconnect_client.1",
            "openconnect_client.2",
            "openconnect_client.3",
            "openconnect_client.4",
            "openconnect_client.5",
            "pureplay.0",
            "pureplay.1",
            "pureplay_accessory.0",
            "pureplay_accessory.1",
            "random_club.0",
        ]
    );
}

#[test]
fn a_card_keeps_its_place_whether_running_configured_or_both() {
    let form = form_with_every_type();
    let actors: HashMap<String, ActorStatusResponse> = form
        .actors
        .iter()
        .map(|a| {
            let status = ActorStatusResponse::new(a.name(), ActorStatus::Connected);
            (a.global_id(), status)
        })
        .collect();
    let ids = |cards: Vec<(CardGroup, Vec<crate::panels::devices::DeviceCard>)>| {
        cards
            .into_iter()
            .map(|(g, cards)| (g, cards.into_iter().map(|c| c.id).collect()))
            .collect::<Vec<(CardGroup, Vec<String>)>>()
    };
    let configured = card_ids(&form);
    // Running and configured
    assert_eq!(ids(device_card_order(&actors, &form.actors)), configured);
    // Running, before the settings load
    assert_eq!(ids(device_card_order(&actors, &[])), configured);
}

#[test]
fn add_puts_a_dirty_entry_in_the_group_that_offered_it() {
    for group in CardGroup::ALL {
        for preset in cards::add_options(group) {
            let type_key = preset.type_key;
            let mut form = SettingsForm::default();
            form.load_from(&FlighthookConfig::default());
            form.add_preset(preset);
            form.add_preset(preset);
            let added: Vec<&ActorFormEntry> = form
                .actors
                .iter()
                .filter(|a| a.type_key() == type_key && a.dirty())
                .collect();
            assert_eq!(added.len(), 2, "{type_key}");
            for a in &added {
                assert_eq!(a.name(), preset.name, "{}", preset.label);
            }
            assert!(form.dirty());
            let order = card_ids(&form);
            let members = &order.iter().find(|(g, _)| *g == group).unwrap().1;
            for a in &added {
                assert!(
                    members.contains(&a.global_id()),
                    "{} not under {}",
                    a.global_id(),
                    group.title()
                );
            }
            // The lowest unused index for the type, from 0
            let mut indices: Vec<&str> = added.iter().map(|a| a.index()).collect();
            indices.sort_unstable();
            let first = form
                .actors
                .iter()
                .filter(|a| a.type_key() == type_key)
                .count()
                - 2;
            assert_eq!(indices, [first.to_string(), (first + 1).to_string()]);
        }
    }
}

#[test]
fn add_offers_only_known_types() {
    let mut form = SettingsForm::default();
    form.load_from(&FlighthookConfig::default());
    let before = form.actors.len();
    assert_eq!(form.add_actor("mock_monitor"), None);
    assert_eq!(form.add_actor("random_club"), None);
    assert_eq!(form.actors.len(), before);
    assert_eq!(
        form.add_actor("openconnect_client").as_deref(),
        Some("openconnect_client.0")
    );
}

#[test]
fn cancel_restores_a_saved_entry_and_drops_a_new_one() {
    let mut form = form_with_every_type();
    let idx = |form: &SettingsForm, id: &str| {
        form.actors
            .iter()
            .position(|a| a.global_id() == id)
            .unwrap()
    };
    let saved_count = form.actors.len();

    // A saved entry: back to the saved values, clean.
    let i = idx(&form, "mevo.1");
    if let ActorFormEntry::Device(d) = &mut form.actors[i] {
        d.address = "10.0.0.9:5100".into();
        d.track_pct = "55".into();
        d.dirty = true;
    }
    assert!(form.locked());
    assert!(!form.cancel_actor(i));
    let ActorFormEntry::Device(d) = &form.actors[idx(&form, "mevo.1")] else {
        panic!("mevo.1 is a device");
    };
    assert_eq!(d.address, "192.168.2.1:5100");
    assert_eq!(d.track_pct, "80");
    assert!(!d.dirty);
    assert!(!form.dirty());
    assert!(!form.locked());
    assert_eq!(form.actors.len(), saved_count);
    assert_eq!(form.to_request(), form.original_config.clone().unwrap());

    // A new entry: gone.
    let id = form.add_actor("square").unwrap();
    assert!(form.locked());
    assert!(form.cancel_actor(idx(&form, &id)));
    assert_eq!(form.actors.len(), saved_count);
    assert!(!form.locked());

    // After a save, the saved values are what a cancel goes back to.
    let id = form.add_actor("openconnect_client").unwrap();
    let _ = form.begin_save(super::SaveTarget::Actor(id.clone()));
    assert_eq!(form.finish_save(Ok(())), None);
    let i = idx(&form, &id);
    assert!(!form.actors[i].dirty());
    if let ActorFormEntry::Integration(e) = &mut form.actors[i] {
        e.name = "Bay 3".into();
        e.dirty = true;
    }
    assert!(!form.cancel_actor(i));
    assert_eq!(form.actors[i].name(), "Open Connect");
    assert_eq!(form.actors.len(), saved_count + 1);
}

/// Tournament mode switched by another client while the General cards
/// hold an unsaved edit: the save (scoped and whole) carries the live
/// value from the reload, not the form's copy, in both directions.
#[test]
fn a_general_save_carries_the_live_tournament_mode() {
    for live in [true, false] {
        let mut before = FlighthookConfig::default();
        before.simulation.tournament = !live;
        let mut form = SettingsForm::default();
        form.load_from(&before);
        form.replay.keep_shots = "7".into();
        form.mark_general(GeneralCard::Replay);
        // Switched elsewhere: the ConfigOutcome reloads the settings.
        let mut after = before.clone();
        after.simulation.tournament = live;
        form.load_from(&after);
        assert!(form.global_dirty, "the edit stays");
        assert_eq!(form.build_global_request().simulation.tournament, live);
        assert_eq!(form.to_request().simulation.tournament, live);
        assert_eq!(form.build_global_request().replay.keep_shots, 7);

        // The save lands; the saved config keeps the live value.
        let _ = form.begin_save(super::SaveTarget::Global);
        assert_eq!(form.finish_save(Ok(())), None);
        assert!(!form.global_dirty);
        assert_eq!(form.saved_config_ref().unwrap().simulation.tournament, live);
    }
}

#[test]
fn general_edits_mark_the_card_they_were_made_on() {
    let mut form = SettingsForm::default();
    form.load_from(&FlighthookConfig::default());
    for card in [GeneralCard::Global, GeneralCard::Simulation] {
        assert!(!form.general_card_dirty(card));
    }
    assert!(!form.locked());

    form.mark_general(GeneralCard::Simulation);
    assert!(form.locked());
    assert!(!form.general_card_dirty(GeneralCard::Global));
    assert!(form.general_card_dirty(GeneralCard::Simulation));
    form.mark_general(GeneralCard::Global);
    assert!(form.general_card_dirty(GeneralCard::Global));
    assert!(form.general_card_dirty(GeneralCard::Simulation));

    // Saved: both clean, and the next edit marks only its own card.
    let _ = form.begin_save(super::SaveTarget::Global);
    assert_eq!(form.finish_save(Ok(())), None);
    assert!(!form.global_dirty);
    assert!(!form.locked());
    form.mark_general(GeneralCard::Global);
    assert!(form.general_card_dirty(GeneralCard::Global));
    assert!(!form.general_card_dirty(GeneralCard::Simulation));

    // Unsaved Global edits from neither card show on the Global card.
    let mut form = SettingsForm::default();
    form.load_from(&FlighthookConfig::default());
    form.global_dirty = true;
    assert!(form.general_card_dirty(GeneralCard::Global));
    assert!(!form.general_card_dirty(GeneralCard::Simulation));
}

#[test]
fn cancel_general_restores_both_cards() {
    let config = FlighthookConfig::default();
    let mut form = SettingsForm::default();
    form.load_from(&config);
    form.default_units = UnitSystem::Metric;
    form.simulation.set_fahrenheit(false);
    form.shot_grouping = crate::types::ShotGrouping::None;
    form.mark_general(GeneralCard::Global);
    form.simulation.mode = SimMode::Off;
    form.simulation.altitude_val = "300".into();
    form.mark_general(GeneralCard::Simulation);

    form.cancel_general();
    assert!(!form.global_dirty);
    assert!(!form.locked());
    assert_eq!(form.build_global_request(), config);
    assert!(form.simulation.fahrenheit);
    // Nothing to give back when the units were not changed.
    form.simulation.altitude_val = "200".into();
    form.mark_general(GeneralCard::Simulation);
    form.cancel_general();
    assert_eq!(form.build_global_request(), config);
}

#[test]
fn simulation_form_round_trips_defaults_exactly() {
    for units in [UnitSystem::Imperial, UnitSystem::Metric] {
        let form = SimulationForm::from_section(&SimulationSection::default(), units);
        assert_eq!(form.to_section(), Some(SimulationSection::default()));
    }
    let imperial =
        SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Imperial);
    assert_eq!(imperial.temperature_val, "77");
    assert_eq!(imperial.temperature_unit(), "°F");
}

#[test]
fn sim_mode_round_trips_through_the_form() {
    for mode in SimMode::all() {
        let section = SimulationSection {
            mode,
            ..SimulationSection::default()
        };
        for units in [UnitSystem::Imperial, UnitSystem::Metric] {
            let form = SimulationForm::from_section(&section, units);
            assert_eq!(form.mode, mode);
            assert_eq!(form.to_section(), Some(section.clone()));
        }

        // Picking the mode in a default form saves exactly that mode.
        let mut form =
            SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Metric);
        assert_eq!(form.mode, SimMode::On);
        form.mode = mode;
        assert_eq!(form.to_section(), Some(section.clone()));

        let cfg = FlighthookConfig {
            simulation: section,
            ..FlighthookConfig::default()
        };
        let mut settings = SettingsForm::default();
        settings.load_from(&cfg);
        assert_eq!(settings.simulation.mode, mode);
        let json = serde_json::to_string(&settings.build_global_request()).expect("ser");
        assert!(!json.contains("enabled"), "{json}");
        if mode == SimMode::On {
            assert!(!json.contains(r#""mode""#), "{json}");
        } else {
            let want = format!(r#""mode":"{}""#, mode.key());
            assert!(json.contains(&want), "{json}");
        }
    }
}

/// The Auto-chip setting loads into the form, is edited there, and goes
/// out in the save request only while enabled (the default stays off
/// the wire).
#[test]
fn the_auto_chip_setting_round_trips_through_the_form() {
    let mut settings = SettingsForm::default();
    assert!(
        !settings.auto_chip_enabled(),
        "off before the settings load"
    );
    settings.load_from(&FlighthookConfig::default());
    assert!(!settings.simulation.auto_chip, "disabled by default");
    assert!(!settings.auto_chip_enabled());
    let json = serde_json::to_string(&settings.build_global_request()).expect("ser");
    assert!(!json.contains("auto_chip"), "{json}");

    settings.simulation.auto_chip = true;
    settings.mark_general(GeneralCard::Simulation);
    assert!(
        settings
            .simulation
            .to_section()
            .is_some_and(|s| s.auto_chip)
    );
    let json = serde_json::to_string(&settings.build_global_request()).expect("ser");
    assert!(json.contains(r#""auto_chip":true"#), "{json}");
    assert!(
        !settings.auto_chip_enabled(),
        "the header follows the saved setting, not the edit"
    );

    let on = FlighthookConfig {
        simulation: SimulationSection {
            auto_chip: true,
            ..SimulationSection::default()
        },
        ..FlighthookConfig::default()
    };
    let mut loaded = SettingsForm::default();
    loaded.load_from(&on);
    assert!(loaded.simulation.auto_chip);
    assert!(loaded.auto_chip_enabled());
}

#[test]
fn untouched_odd_values_save_unchanged() {
    let s = SimulationSection {
        altitude: Distance::Yards(12.345),
        temperature_c: 21.37,
        ..SimulationSection::default()
    };
    let form = SimulationForm::from_section(&s, UnitSystem::Imperial);
    assert_eq!(form.to_section(), Some(s));
}

#[test]
fn metric_units_show_a_zero_altitude_in_meters() {
    let form = SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Metric);
    assert_eq!(form.altitude_unit, "meters");
    // Untouched, it still saves as the exact loaded (imperial) default.
    assert_eq!(form.to_section(), Some(SimulationSection::default()));

    let imperial =
        SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Imperial);
    assert_eq!(imperial.altitude_unit, "feet");
}

#[test]
fn fahrenheit_is_stored_as_celsius() {
    let mut form =
        SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Imperial);
    form.temperature_val = "86".into();
    assert_eq!(form.to_section().map(|s| s.temperature_c), Some(30.0));
    form.temperature_val = "50.5".into();
    assert_eq!(form.to_section().map(|s| s.temperature_c), Some(10.3));
    form.set_fahrenheit(false);
    assert_eq!(form.temperature_val, "10.3");
    assert_eq!(form.to_section().map(|s| s.temperature_c), Some(10.3));
}

#[test]
fn out_of_range_or_unparsable_values_are_invalid() {
    let base = SimulationForm::from_section(&SimulationSection::default(), UnitSystem::Metric);
    for (alt, unit, temp) in [
        ("5001", "meters", "25"),
        ("16500", "feet", "25"),
        ("-1", "meters", "25"),
        ("abc", "meters", "25"),
        ("0", "meters", "50.2"),
        ("0", "meters", "-31"),
        ("0", "meters", "warm"),
        ("NaN", "meters", "25"),
    ] {
        let mut f = base.clone();
        f.altitude_val = alt.into();
        f.altitude_unit = unit.into();
        f.temperature_val = temp.into();
        assert_eq!(f.to_section(), None, "{alt} {unit} {temp}");
        let form = SettingsForm {
            simulation: f,
            ..SettingsForm::default()
        };
        assert!(!form.general_valid(), "{alt} {unit} {temp}");
    }
    let mut f = base;
    f.altitude_val = "5280".into();
    f.altitude_unit = "feet".into();
    f.surface = SimSurface::Green;
    let s = f.to_section().expect("valid");
    assert!((s.altitude.as_meters() - 1609.344).abs() < 1e-9);
    assert_eq!(s.surface, SimSurface::Green);
}

#[test]
fn loading_a_default_config_keeps_simulation_off_the_wire() {
    let mut form = SettingsForm::default();
    form.load_from(&FlighthookConfig::default());
    let json = serde_json::to_string(&form.build_global_request()).expect("ser");
    assert!(!json.contains("simulation"), "{json}");
    assert!(form.general_valid());
}

#[test]
fn launch_only_spin_survives_a_settings_save() {
    for k in [100.0, 0.0, 87.5] {
        let mut cfg = FlighthookConfig::default();
        cfg.mevo.insert(
            "0".into(),
            MevoSection {
                launch_only_spin: k,
                ..MevoSection::default()
            },
        );
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        assert_eq!(form.to_request().mevo["0"].launch_only_spin, k);
        assert!(super::actor_valid(&form.actors[0]), "{k}");

        let actor = form.actors[0].clone();
        let mut orig = FlighthookConfig::default();
        super::apply_actor_to_config(&mut orig, &actor);
        assert_eq!(orig.mevo["0"].launch_only_spin, k);
    }
}

/// The mock card's driver settings, which the form does not show,
/// survive a save of the card; settings the driver refuses keep the
/// card from saving, with the field named.
#[test]
fn mock_driver_settings_survive_a_save_and_are_validated() {
    let mut cfg = FlighthookConfig::default();
    let driver = MockConfig {
        shot_interval_s: 5.0,
        serial: Some("7".into()),
        ..MockConfig::default()
    };
    cfg.mock_monitor.insert(
        "0".into(),
        MockMonitorSection {
            name: "Mock".into(),
            driver: driver.clone(),
            ..MockMonitorSection::default()
        },
    );
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    assert!(super::actor_valid(&form.actors[0]));
    assert_eq!(
        form.build_actor_request("mock_monitor.0").mock_monitor["0"].driver,
        driver
    );
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(dev.driver_error(), None);
    dev.mock.shot_interval_s = -1.0;
    assert_eq!(
        dev.driver_error().as_deref(),
        Some("shot_interval_s: must be 0 to 86400")
    );
    assert!(!super::actor_valid(&form.actors[0]));
}

/// An Open Connect card saves only the clubs its pickers offer.
#[test]
fn an_open_connect_card_drops_clubs_its_pickers_do_not_offer() {
    let mut cfg = FlighthookConfig::default();
    cfg.openconnect_client.insert(
        "0".into(),
        flighthook::OpenConnectClientSection {
            name: "GSPro".into(),
            address: Some("127.0.0.1:921".into()),
            chipping_clubs: vec![Club::SandWedge, Club::Iron9],
            putting_clubs: vec![Club::Putter, Club::Driver],
            ..flighthook::OpenConnectClientSection::default()
        },
    );
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let saved = form.build_actor_request("openconnect_client.0");
    let card = &saved.openconnect_client["0"];
    assert_eq!(card.chipping_clubs, [Club::SandWedge]);
    assert_eq!(card.putting_clubs, [Club::Putter]);
}

/// Saving a card, or the whole form, keeps every saved card's actor id.
#[test]
fn saves_keep_the_saved_card_ids() {
    let mut cfg = FlighthookConfig::default();
    cfg.mock_monitor.insert(
        "0".into(),
        flighthook::MockMonitorSection {
            actor_id: Some("aaaa000000000001".into()),
            name: "Mock".into(),
            ..flighthook::MockMonitorSection::default()
        },
    );
    cfg.random_club.insert(
        "0".into(),
        flighthook::RandomClubSection {
            actor_id: Some("aaaa000000000002".into()),
            name: "Clubs".into(),
            enabled: true,
        },
    );
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let saved = form.build_actor_request("mock_monitor.0");
    assert_eq!(
        saved.mock_monitor["0"].actor_id.as_deref(),
        Some("aaaa000000000001")
    );
    let all = form.to_request();
    assert_eq!(
        all.mock_monitor["0"].actor_id.as_deref(),
        Some("aaaa000000000001")
    );
    assert_eq!(
        all.random_club["0"].actor_id.as_deref(),
        Some("aaaa000000000002")
    );
}

/// The Trackball card saves its section as loaded unless a field
/// changed: an unchanged card writes it byte for byte, an edited field
/// alone is written, a blank number is the driver's default, a loaded
/// `grab` is left out, and a new card writes its name.
#[test]
fn trackball_settings_are_saved_as_loaded_unless_changed() {
    let json = |section: &flighthook::TrackballSection| serde_json::to_string(section).unwrap();
    for loaded in [
        r#"{"name":"Arcade","device":"usb-Example_Trackball-event-mouse","club":"DR","cpi":800.0,"report_limit":2047,"thresholds":{"v_start":0.0712,"coil_timeout_s":20.0},"tuning":{"full_power_speed":41.3,"curve_sense":"physical","mirror_for_left_hand":true}}"#,
        r#"{"name":"Trackball"}"#,
    ] {
        let section: flighthook::TrackballSection = serde_json::from_str(loaded).unwrap();
        let mut cfg = FlighthookConfig::default();
        cfg.trackball.insert("0".into(), section);
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        assert!(super::actor_valid(&form.actors[0]), "{loaded}");
        let saved = form.build_actor_request("trackball.0");
        assert_eq!(json(&saved.trackball["0"]), loaded);
    }
    for loaded in [
        r#"{"name":"Trackball","grab":true}"#,
        r#"{"name":"Trackball","grab":false}"#,
    ] {
        let section: flighthook::TrackballSection = serde_json::from_str(loaded).unwrap();
        let mut cfg = FlighthookConfig::default();
        cfg.trackball.insert("0".into(), section);
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let saved = form.build_actor_request("trackball.0");
        assert_eq!(json(&saved.trackball["0"]), r#"{"name":"Trackball"}"#);
        assert!(!saved.trackball["0"].driver.config.grab, "{loaded}");
    }

    let mut cfg = FlighthookConfig::default();
    cfg.trackball.insert(
        "0".into(),
        serde_json::from_str(r#"{"name":"Trackball","cpi":800.0,"tuning":{"power_gamma":1.5}}"#)
            .unwrap(),
    );
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.trackball_form()
        .numbers
        .insert("tuning.max_club_speed", "110".into());
    dev.trackball_form().numbers.insert("cpi", " ".into());
    dev.trackball_form().device = " event7 ".into();
    dev.trackball_form().club = flighthook::TrackballClub::Putter;
    dev.dirty = true;
    assert!(super::actor_valid(&form.actors[0]));
    let saved = form.build_actor_request("trackball.0");
    assert_eq!(
        json(&saved.trackball["0"]),
        r#"{"name":"Trackball","device":"event7","club":"PT","tuning":{"max_club_speed":110.0,"power_gamma":1.5}}"#,
        "blank CPI is the default, left out"
    );

    let mut form = SettingsForm::default();
    form.load_from(&FlighthookConfig::default());
    let preset = cards::add_options(CardGroup::LaunchMonitors)
        .iter()
        .find(|p| p.type_key == "trackball")
        .expect("a Trackball preset");
    let id = form.add_preset(preset).expect("added");
    assert_eq!(id, "trackball.0");
    let saved = form.build_actor_request(&id);
    assert_eq!(json(&saved.trackball["0"]), r#"{"name":"Trackball"}"#);

    // Only a Trackball card carries the Trackball fields.
    let mut cfg = FlighthookConfig::default();
    cfg.trackball
        .insert("0".into(), flighthook::TrackballSection::default());
    cfg.mock_monitor
        .insert("0".into(), MockMonitorSection::default());
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    form.add_preset(preset).expect("added");
    let devices: Vec<&super::DeviceFormEntry> = form
        .actors
        .iter()
        .filter_map(|a| match a {
            ActorFormEntry::Device(dev) => Some(dev),
            _ => None,
        })
        .collect();
    assert_eq!(devices.iter().filter(|d| d.is_trackball()).count(), 2);
    assert!(devices.iter().any(|d| !d.is_trackball()));
    for dev in devices {
        assert_eq!(dev.trackball.is_some(), dev.is_trackball(), "{}", dev.id);
    }
}

/// The Trackball card checks its settings with the driver's own
/// validation and refuses text that is not a number; a stored refused
/// value does not lock the card.
#[test]
fn the_trackball_settings_are_validated_by_its_driver() {
    let mut cfg = FlighthookConfig::default();
    cfg.trackball
        .insert("0".into(), flighthook::TrackballSection::default());
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    for (key, typed, error) in [
        ("cpi", "0", "cpi: must be greater than 0"),
        ("cpi", "lots", "cpi: not a number"),
        ("report_limit", "0", "report_limit: must be at least 1"),
        (
            "thresholds.gap_ms",
            "2.5",
            "thresholds.gap_ms: not a whole number",
        ),
        (
            "thresholds.speed_window_ms",
            "-1",
            "thresholds.speed_window_ms: not a whole number",
        ),
        (
            "thresholds.v_stop",
            "0.5",
            "thresholds.v_stop: must be below thresholds.v_start (0.067)",
        ),
        (
            "tuning.backswing_power",
            "2",
            "tuning.backswing_power: must be 0 to 1",
        ),
        (
            "tuning.power_gamma",
            "0",
            "tuning.power_gamma: must be greater than 0",
        ),
    ] {
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        let before = dev.trackball_form().numbers[key].clone();
        dev.trackball_form().numbers.insert(key, typed.into());
        dev.dirty = true;
        assert_eq!(dev.driver_error().as_deref(), Some(error), "{key} {typed}");
        assert!(!super::actor_valid(&form.actors[0]), "{key} {typed}");
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.trackball_form().numbers.insert(key, before);
        assert_eq!(dev.driver_error(), None);
    }

    let mut cfg = FlighthookConfig::default();
    let mut refused = flighthook::TrackballSection::default();
    refused.driver.config.cpi = 0.0;
    cfg.trackball.insert("0".into(), refused);
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(
        dev.driver_error().as_deref(),
        Some("cpi: must be greater than 0"),
        "the card shows it"
    );
    dev.name = "Arcade".into();
    dev.dirty = true;
    assert!(super::actor_valid(&form.actors[0]), "a rename saves");
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.trackball_form()
        .numbers
        .insert("tuning.power_gamma", "0".into());
    assert!(!super::actor_valid(&form.actors[0]), "another refusal");
}

/// A card whose saved driver settings are refused shows the refusal and
/// still saves its name (its other fields stay as saved); changing the
/// driver settings into other refused ones blocks the save, into usable
/// ones does not.
#[test]
fn a_refused_saved_driver_setting_does_not_lock_the_card() {
    let mut cfg = FlighthookConfig::default();
    let mut section = MockMonitorSection {
        name: "Mock".into(),
        ..MockMonitorSection::default()
    };
    section.driver.shot_interval_s = -1.0;
    cfg.mock_monitor.insert("0".into(), section);
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(
        dev.driver_error().as_deref(),
        Some("shot_interval_s: must be 0 to 86400"),
        "the card shows it"
    );
    dev.name = "Renamed".into();
    dev.dirty = true;
    assert!(super::actor_valid(&form.actors[0]), "a rename saves");
    let saved = form.build_actor_request("mock_monitor.0");
    assert_eq!(saved.mock_monitor["0"].name, "Renamed");
    assert_eq!(saved.mock_monitor["0"].driver.shot_interval_s, -1.0);
    assert!(
        form.begin_enable("mock_monitor.0", false).is_some(),
        "the switch saves"
    );

    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.mock.arm_delay_s = -1.0;
    assert!(
        !super::actor_valid(&form.actors[0]),
        "another refused value"
    );
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.mock.arm_delay_s = MockConfig::default().arm_delay_s;
    dev.mock.shot_interval_s = 10.0;
    assert!(super::actor_valid(&form.actors[0]), "fixed");
}

/// The ExPutt card's IP Address is the ExPutt driver's `address`: one
/// the driver refuses (not an IP) shows under the card's fields and
/// keeps a changed card from saving, blank finds the device, and a
/// saved refused address still lets the card save its name.
#[test]
fn the_exputt_address_is_validated_by_its_driver() {
    let mut cfg = FlighthookConfig::default();
    cfg.exputt.insert(
        "0".into(),
        flighthook::ExPuttSection {
            driver: flighthook::ExPuttConfig {
                address: Some("exputt.local".into()),
            },
            ..flighthook::ExPuttSection::default()
        },
    );
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(dev.address, "exputt.local");
    let error = dev.driver_error().expect("shown");
    assert!(
        error.starts_with("address: ") && error.contains("exputt.local"),
        "{error}"
    );
    dev.name = "Renamed".into();
    dev.dirty = true;
    assert!(super::actor_valid(&form.actors[0]), "a rename saves");
    let saved = form.build_actor_request("exputt.0");
    assert_eq!(saved.exputt["0"].name, "Renamed");
    assert_eq!(
        saved.exputt["0"].driver.address.as_deref(),
        Some("exputt.local")
    );
    for bad in ["putter", "192.168.1.50:8889"] {
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.address = bad.into();
        assert!(dev.driver_error().is_some(), "{bad}");
        assert!(!super::actor_valid(&form.actors[0]), "{bad}");
    }
    for (typed, address) in [("", None), (" 10.0.0.7 ", Some("10.0.0.7"))] {
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.address = typed.into();
        assert_eq!(dev.driver_error(), None, "{typed:?}");
        assert!(super::actor_valid(&form.actors[0]), "{typed:?}");
        assert_eq!(
            form.build_actor_request("exputt.0").exputt["0"]
                .driver
                .address
                .as_deref(),
            address
        );
    }
}

/// The R10 card's Monitor-to-Ball is the R10 driver's `range`: a value
/// the driver refuses (0 or less) shows under the card's fields and
/// keeps a changed card from saving, blank means none, and a saved
/// refused range still lets the card save its name.
/// The Square card edits the address, the zero-spin switch and the
/// listed clubs' calibration; the driver's other settings (`club`,
/// `advanced_spin`, calibration keys the card does not list) survive a
/// save as they were. The driver refuses nothing, so the card never
/// shows a driver error.
#[test]
fn square_driver_settings_survive_a_save() {
    let mut cfg = FlighthookConfig::default();
    cfg.square.insert(
        "0".into(),
        flighthook::SquareSection {
            driver: flighthook::SquareConfig {
                address: Some("SquareGolf(CD01)".into()),
                club: "PW".into(),
                advanced_spin: false,
                discard_non_putting_zero_spin: true,
                dot_bottom_to_face_center_mm: std::collections::BTreeMap::from([
                    ("7i".into(), 21.5),
                    ("PT".into(), 8.0),
                ]),
            },
            ..flighthook::SquareSection::default()
        },
    );
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(dev.address, "SquareGolf(CD01)");
    assert!(dev.discard_zero_spin);
    assert_eq!(
        dev.square_impact_mm.get(&Club::Iron7).map(String::as_str),
        Some("21.5")
    );
    assert_eq!(dev.driver_error(), None);
    assert!(!dev.driver_blocks_save());

    // Saved as loaded: the same settings, the 7-iron's key in its code.
    let saved = form.build_actor_request("square.0").square["0"].clone();
    assert_eq!(saved.driver.club, "PW");
    assert!(!saved.driver.advanced_spin);
    assert_eq!(
        saved.driver.dot_bottom_to_face_center_mm,
        std::collections::BTreeMap::from([("7I".into(), 21.5), ("PT".into(), 8.0)])
    );

    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.address = "  ".into();
    dev.discard_zero_spin = false;
    dev.square_impact_mm.insert(Club::Iron7, String::new());
    dev.square_impact_mm.insert(Club::Driver, "19".into());
    assert_eq!(dev.driver_error(), None);
    assert!(super::actor_valid(&form.actors[0]));
    let saved = form.build_actor_request("square.0").square["0"].clone();
    assert_eq!(
        saved.driver,
        flighthook::SquareConfig {
            address: None,
            club: "PW".into(),
            advanced_spin: false,
            discard_non_putting_zero_spin: false,
            dot_bottom_to_face_center_mm: std::collections::BTreeMap::from([
                ("DR".into(), 19.0),
                ("PT".into(), 8.0),
            ]),
        }
    );
}

#[test]
fn the_r10_range_is_validated_by_its_driver() {
    let mut cfg = FlighthookConfig::default();
    cfg.r10.insert(
        "0".into(),
        R10Section {
            driver: flighthook::R10Config {
                range: Some(Distance::Feet(7.0)),
                ..flighthook::R10Config::default()
            },
            ..R10Section::default()
        },
    );
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(
        (dev.range_val.as_str(), dev.range_unit.as_str()),
        ("7", "feet")
    );
    assert_eq!(dev.driver_error(), None);
    for bad in ["0", "-2"] {
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.range_val = bad.into();
        assert_eq!(
            dev.driver_error().as_deref(),
            Some("range: must be a distance greater than 0"),
            "{bad}"
        );
        assert!(!super::actor_valid(&form.actors[0]), "{bad}");
    }
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.range_val = "7f".into();
    assert_eq!(dev.driver_error().as_deref(), Some("range: not a number"));
    assert!(
        !super::actor_valid(&form.actors[0]),
        "never saved as no range"
    );
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.range_val = String::new();
    assert_eq!(dev.driver_error(), None);
    assert!(super::actor_valid(&form.actors[0]));
    assert_eq!(
        form.build_actor_request("r10.0").r10["0"].driver.range,
        None
    );
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.range_val = "2.5".into();
    dev.range_unit = "meters".into();
    assert_eq!(
        form.build_actor_request("r10.0").r10["0"].driver.range,
        Some(Distance::Meters(2.5))
    );

    cfg.r10.get_mut("0").unwrap().driver.range = Some(Distance::Feet(0.0));
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert!(dev.driver_error().is_some(), "shown");
    dev.name = "Bay 1".into();
    assert!(super::actor_valid(&form.actors[0]), "a rename saves");

    // Text that is not a number never saves, also on a card with no
    // range saved.
    cfg.r10.get_mut("0").unwrap().driver.range = None;
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.range_val = "seven".into();
    assert!(!super::actor_valid(&form.actors[0]));
}

/// The R10 card's Aim offset is the R10 driver's `aim_offset_deg`:
/// blank is unset, a value outside -10 to 10 is refused by the driver
/// and keeps a changed card from saving, text that is not a number never
/// saves, and a saved value survives a round trip through the form.
#[test]
fn the_r10_aim_offset_is_validated_by_its_driver() {
    let mut cfg = FlighthookConfig::default();
    cfg.r10.insert(
        "0".into(),
        R10Section {
            driver: flighthook::R10Config {
                range: Some(Distance::Feet(7.0)),
                aim_offset_deg: Some(-1.5),
            },
            ..R10Section::default()
        },
    );
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(dev.aim_offset, "-1.5");
    assert_eq!(dev.driver_error(), None);
    // Saved as loaded, range and all.
    let saved = form.build_actor_request("r10.0").r10["0"].driver.clone();
    assert_eq!(saved.aim_offset_deg, Some(-1.5));
    assert_eq!(saved.range, Some(Distance::Feet(7.0)));

    for (typed, want) in [("2.25", 2.25), (" 10 ", 10.0), ("-10", -10.0), ("0", 0.0)] {
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.aim_offset = typed.into();
        assert_eq!(dev.driver_error(), None, "{typed}");
        assert!(super::actor_valid(&form.actors[0]), "{typed}");
        assert_eq!(
            form.build_actor_request("r10.0").r10["0"]
                .driver
                .aim_offset_deg,
            Some(want),
            "{typed}"
        );
    }
    for bad in ["10.5", "-11", "45", "inf", "NaN"] {
        let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
            panic!("not a device");
        };
        dev.aim_offset = bad.into();
        assert_eq!(
            dev.driver_error().as_deref(),
            Some("aim_offset_deg: must be -10 to 10 degrees"),
            "{bad}"
        );
        assert!(!super::actor_valid(&form.actors[0]), "{bad}");
    }
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.aim_offset = "left".into();
    assert_eq!(
        dev.driver_error().as_deref(),
        Some("aim_offset_deg: not a number")
    );
    assert!(!super::actor_valid(&form.actors[0]), "never saved as none");
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    dev.aim_offset = String::new();
    assert_eq!(dev.driver_error(), None);
    assert!(super::actor_valid(&form.actors[0]));
    assert_eq!(
        form.build_actor_request("r10.0").r10["0"]
            .driver
            .aim_offset_deg,
        None
    );

    // A refused saved value shows, and the card still saves its name.
    cfg.r10.get_mut("0").unwrap().driver.aim_offset_deg = Some(30.0);
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(
        dev.driver_error().as_deref(),
        Some("aim_offset_deg: must be -10 to 10 degrees")
    );
    dev.name = "Bay 1".into();
    assert!(super::actor_valid(&form.actors[0]), "a rename saves");

    // A card with none saved shows a blank field.
    cfg.r10.get_mut("0").unwrap().driver.aim_offset_deg = None;
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(dev.aim_offset, "");
    assert_eq!(dev.driver_error(), None);
}

#[test]
fn launch_only_spin_field_is_validated() {
    assert_eq!(super::launch_only_spin_value(""), Some(100.0));
    assert_eq!(super::launch_only_spin_value(" 0 "), Some(0.0));
    assert_eq!(super::launch_only_spin_value("120"), Some(120.0));
    assert_eq!(super::launch_only_spin_value("1000"), Some(1000.0));
    for bad in ["-1", "1000.5", "5000", "abc", "NaN", "inf"] {
        assert_eq!(super::launch_only_spin_value(bad), None, "{bad}");
    }
    let mut cfg = FlighthookConfig::default();
    cfg.mevo.insert("0".into(), MevoSection::default());
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let ActorFormEntry::Device(dev) = &mut form.actors[0] else {
        panic!("not a device");
    };
    assert_eq!(dev.launch_only_spin, "100");
    dev.launch_only_spin = "-5".into();
    assert!(!super::actor_valid(&form.actors[0]));
}

#[test]
fn spin_source_survives_a_settings_save() {
    for src in super::SPIN_SOURCES {
        let mut cfg = FlighthookConfig::default();
        let mut section = MevoSection::default();
        section.driver.spin_source = src;
        cfg.mevo.insert("0".into(), section);
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        assert_eq!(form.to_request().mevo["0"].driver.spin_source, src);

        let actor = form.actors[0].clone();
        let mut orig = FlighthookConfig::default();
        super::apply_actor_to_config(&mut orig, &actor);
        assert_eq!(orig.mevo["0"].driver.spin_source, src);
    }
}

/// Where the form holds its FlightScope card.
fn mevo_at(form: &SettingsForm) -> usize {
    form.actors
        .iter()
        .position(|a| matches!(a, ActorFormEntry::Device(d) if d.monitor_type == "mevo"))
        .expect("a FlightScope card")
}

/// The FlightScope card saves a section it didn't change exactly as it
/// was loaded (settings left unset stay unset), and a changed field as
/// typed; a new card states the values it shows, as before.
#[test]
fn mevo_settings_are_saved_as_loaded_unless_changed() {
    for json in [
        r#"{"name":"Mevo WiFi","address":"192.168.2.1:5100","ball_type":1,"tee_height":"1.5in","range":"8ft","surface_height":"0in","track_pct":80.0,"use_estimated":false,"camera_mode":"standard","environment":"limited_flight"}"#,
        r#"{"name":"Xi","address":"10.0.0.7:5100","spin_source":"e8","range":"9ft"}"#,
        r#"{"name":"Garage","enabled":false}"#,
    ] {
        let section: MevoSection = serde_json::from_str(json).unwrap();
        let mut cfg = FlighthookConfig::default();
        cfg.mevo.insert("0".into(), section.clone());
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        let ActorFormEntry::Device(dev) = &form.actors[mevo_at(&form)] else {
            panic!("not a device");
        };
        assert_eq!(dev.driver_error(), None, "{json}");
        assert!(!dev.driver_blocks_save(), "{json}");
        assert_eq!(
            form.build_actor_request("mevo.0").mevo["0"],
            section,
            "{json}"
        );
    }

    let mut cfg = FlighthookConfig::default();
    let section: MevoSection =
        serde_json::from_str(r#"{"name":"Xi","address":"10.0.0.7:5100"}"#).unwrap();
    cfg.mevo.insert("0".into(), section);
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let at = mevo_at(&form);
    let ActorFormEntry::Device(dev) = &mut form.actors[at] else {
        panic!("not a device");
    };
    dev.range_val = "2.5".into();
    dev.range_unit = "meters".into();
    dev.track_pct = "70".into();
    dev.environment = crate::types::FlightScopeEnvironment::Outdoor;
    dev.address = String::new();
    let saved = form.build_actor_request("mevo.0").mevo["0"].driver.clone();
    assert_eq!(saved.range, Some(Distance::Meters(2.5)));
    assert_eq!(saved.track_pct, Some(70.0));
    assert_eq!(
        saved.environment,
        Some(crate::types::FlightScopeEnvironment::Outdoor)
    );
    assert_eq!(saved.address, None, "blank: the default address");
    assert_eq!(saved.tee_height, None, "still unset");
    assert_eq!(saved.camera_mode, None, "still unset");

    let mut form = SettingsForm::default();
    let id = form
        .add_preset(
            &super::super::cards::add_options(super::super::cards::CardGroup::LaunchMonitors)[0],
        )
        .expect("added");
    assert!(id.starts_with("mevo."), "{id}");
    let saved = form.build_actor_request(&id).mevo[id.trim_start_matches("mevo.")].clone();
    assert_eq!(
        serde_json::to_value(&saved.driver).unwrap(),
        serde_json::json!({
            "address": "192.168.2.1:5100", "ball_type": 1, "tee_height": "1.5in",
            "range": "8ft", "surface_height": "0in", "track_pct": 80.0,
            "use_estimated": false, "camera_mode": "standard",
            "environment": "limited_flight"
        })
    );
}

/// Every FlightScope setting the driver can refuse is on the card and
/// checked by the driver's own validation: track % outside
/// 0 to 100, a negative distance, an address that is not an IP address
/// and port; a number field that is not a number never saves. A stored
/// refused value still lets the card save its other fields.
#[test]
fn the_mevo_settings_are_validated_by_its_driver() {
    let mut cfg = FlighthookConfig::default();
    cfg.mevo.insert("0".into(), MevoSection::default());
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    type Case = (fn(&mut super::DeviceFormEntry), &'static str);
    let cases: [Case; 6] = [
        (
            |d| d.track_pct = "150".into(),
            "track_pct: must be 0 to 100",
        ),
        (|d| d.track_pct = "lots".into(), "track_pct: not a number"),
        (
            |d| d.tee_height_val = "-1".into(),
            "tee_height: must be a distance of 0 or more",
        ),
        (|d| d.range_val = "8x".into(), "range: not a number"),
        (
            |d| d.surface_height_val = "-0.5".into(),
            "surface_height: must be a distance of 0 or more",
        ),
        (
            |d| d.address = "mevo.local".into(),
            "address: must be an IP address",
        ),
    ];
    for (set, want) in cases {
        let mut form = form.clone();
        let at = mevo_at(&form);
        let ActorFormEntry::Device(dev) = &mut form.actors[at] else {
            panic!("not a device");
        };
        set(dev);
        let error = dev.driver_error().expect("refused");
        assert!(error.starts_with(want), "{error}");
        assert!(!super::actor_valid(&form.actors[mevo_at(&form)]), "{want}");
    }

    let mut stored = cfg.clone();
    stored.mevo.get_mut("0").unwrap().driver.track_pct = Some(150.0);
    let mut form = SettingsForm::default();
    form.load_from(&stored);
    let at = mevo_at(&form);
    let ActorFormEntry::Device(dev) = &mut form.actors[at] else {
        panic!("not a device");
    };
    assert!(dev.driver_error().is_some(), "shown");
    dev.name = "Bay 1".into();
    assert!(
        super::actor_valid(&form.actors[mevo_at(&form)]),
        "a rename saves"
    );
}

#[test]
fn ble_address_accepts_mac_and_uuid() {
    assert!(is_ble_address("02:00:00:AB:CD:01"));
    assert!(is_ble_address(" 02:00:00:ab:cd:01 "));
    assert!(is_ble_address("5F2A9C1E-3B7D-4E8A-9C0F-1A2B3C4D5E6F"));
}

#[test]
fn ble_address_accepts_advertised_name() {
    assert!(is_ble_address("SquareGolf(CD01)"));
    assert!(is_ble_address(" squaregolf(cd01) "));
}

#[test]
fn ble_address_rejects_malformed() {
    assert!(!is_ble_address(""));
    assert!(!is_ble_address("02:00:00:AB:CD"));
    assert!(!is_ble_address("02:00:00:AB:CD:G1"));
    assert!(!is_ble_address("5F2A9C1E-3B7D-4E8A-9C0F"));
    assert!(!is_ble_address("192.168.2.1:5100"));
    assert!(!is_ble_address("SquareGolf"));
    assert!(!is_ble_address("SGO300A"));
}

#[test]
fn ble_address_rejects_zero_mac() {
    assert!(!is_ble_address("00:00:00:00:00:00"));
    assert!(!is_ble_address(" 00:00:00:00:00:00 "));
}

#[test]
fn shot_grouping_survives_a_settings_save() {
    use super::SaveTarget;
    use crate::types::ShotGrouping;

    for (mode, _) in crate::groups::GROUPING_MODES {
        let cfg = FlighthookConfig {
            shot_grouping: mode,
            ..FlighthookConfig::default()
        };
        let mut form = SettingsForm::default();
        form.load_from(&cfg);
        assert_eq!(form.shot_grouping, mode);
        assert_eq!(form.to_request().shot_grouping, mode);
        assert_eq!(form.build_global_request().shot_grouping, mode);
    }

    // A change in the form is saved, and the loaded config follows it.
    let mut form = SettingsForm::default();
    form.load_from(&FlighthookConfig::default());
    form.shot_grouping = ShotGrouping::ByClub;
    form.global_dirty = true;
    assert_eq!(
        form.build_global_request().shot_grouping,
        ShotGrouping::ByClub
    );
    let _ = form.begin_save(SaveTarget::Global);
    assert_eq!(form.finish_save(Ok(())), None);
    assert!(!form.global_dirty);
    assert_eq!(
        form.original_config.as_ref().unwrap().shot_grouping,
        ShotGrouping::ByClub
    );

    // A mode set elsewhere reaches a clean form, and never an edited one.
    form.sync_shot_grouping(ShotGrouping::BySet);
    assert_eq!(form.shot_grouping, ShotGrouping::BySet);
    assert_eq!(form.to_request().shot_grouping, ShotGrouping::BySet);
    form.shot_grouping = ShotGrouping::None;
    form.global_dirty = true;
    form.sync_shot_grouping(ShotGrouping::ByClub);
    assert_eq!(form.shot_grouping, ShotGrouping::None);
    assert_eq!(
        form.original_config.as_ref().unwrap().shot_grouping,
        ShotGrouping::ByClub
    );
}

#[test]
fn chipping_and_putting_pickers_offer_only_wedges_and_putter() {
    assert_eq!(
        flighthook::chipping_club_options(),
        &[
            Club::PitchingWedge,
            Club::GapWedge,
            Club::SandWedge,
            Club::LobWedge
        ]
    );
    assert_eq!(flighthook::putting_club_options(), &[Club::Putter]);
}

/// An Open Connect client card's club lists load into its form entry
/// and go back out in the card's own config section.
#[test]
fn an_open_connect_cards_club_lists_round_trip_into_its_config() {
    let mut cfg = FlighthookConfig::default();
    cfg.openconnect_client.insert(
        "0".into(),
        flighthook::OpenConnectClientSection {
            name: "Open Connect".into(),
            chipping_clubs: vec![Club::PitchingWedge, Club::GapWedge],
            putting_clubs: vec![Club::Putter],
            ..flighthook::OpenConnectClientSection::default()
        },
    );
    let mut form = SettingsForm::default();
    form.load_from(&cfg);
    let entry = form
        .actors
        .iter()
        .find_map(|a| match a {
            ActorFormEntry::Integration(i) if a.global_id() == "openconnect_client.0" => Some(i),
            _ => None,
        })
        .expect("the card's entry");
    assert_eq!(
        entry.chipping_clubs,
        vec![Club::PitchingWedge, Club::GapWedge]
    );
    assert_eq!(entry.putting_clubs, vec![Club::Putter]);

    let req = form.to_request();
    let section = &req.openconnect_client["0"];
    assert_eq!(
        section.chipping_clubs,
        vec![Club::PitchingWedge, Club::GapWedge]
    );
    assert_eq!(section.putting_clubs, vec![Club::Putter]);
}
