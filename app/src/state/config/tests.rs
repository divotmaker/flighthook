use super::*;
use flighthook::{DeviceSection, SimMode};
use flighthook_cameras::settings;

/// `FlighthookConfig::default()` as written to disk, with no
/// `[simulation]` table. An untouched config gains one only to hold the
/// flight model's actor id.
const GOLDEN_DEFAULT_TOML: &str = r#"default_units = "imperial"

[webserver.0]
name = "Web Server"
bind = "0.0.0.0:5880"
"#;

/// The default config is written as it is, and the first start writes
/// it with nothing else but the actor ids.
#[test]
fn default_config_file_is_unchanged() {
    let toml = toml::to_string_pretty(&FlighthookConfig::default()).expect("ser");
    assert_eq!(toml, GOLDEN_DEFAULT_TOML);

    let (dir, _) = config_dir("");
    let path = dir.0.join("new.toml");
    let (created, error) = load(&path);
    assert_eq!(error, None);
    let written = std::fs::read_to_string(&path).expect("read");
    assert_eq!(without_ids(&written), GOLDEN_DEFAULT_TOML, "{written}");
    assert!(created.frp.actor_id.is_some() && created.frp.auto_chip_actor_id.is_some());
    let (again, _) = load(&path);
    assert_same_config(&again, &created);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), written);
}

/// Two cards with the same actor id at load: the later one gets a new
/// id, the earlier keeps its own.
#[test]
fn a_duplicate_actor_id_at_load_is_renewed_on_the_later_card() {
    let src = "[mock_monitor.0]\nname = \"A\"\nactor_id = \"aaaa000000000001\"\n\n\
                   [mock_monitor.1]\nname = \"B\"\nactor_id = \"aaaa000000000001\"\n\n\
                   [mevo.0]\nname = \"C\"\nactor_id = \"aaaa000000000001\"\n";
    let (_dir, path) = config_dir(src);
    let (c, error) = load(&path);
    assert_eq!(error, None);
    assert_eq!(c.mevo["0"].actor_id.as_deref(), Some("aaaa000000000001"));
    let b0 = c.mock_monitor["0"].actor_id.clone().expect("id");
    let b1 = c.mock_monitor["1"].actor_id.clone().expect("id");
    assert_ne!(b0, "aaaa000000000001");
    assert_ne!(b1, "aaaa000000000001");
    assert_ne!(b0, b1);
    let (again, _) = load(&path);
    assert_same_config(&again, &c);
}

/// A replace with other ids for flighthook, Auto-chip and the flight
/// model keeps the running process's own; card ids come from `new`.
#[test]
fn replace_keeps_the_process_own_actor_ids() {
    let (_dir, path) = config_dir("[mock_monitor.0]\nname = \"A\"\n");
    let store = SystemConfig::new(path);
    let before = store.snapshot();
    let mut restored = before.clone();
    restored.frp.actor_id = Some("1111111111111111".into());
    restored.frp.auto_chip_actor_id = Some("2222222222222222".into());
    restored.simulation.actor_id = Some("3333333333333333".into());
    restored.mock_monitor.get_mut("0").expect("card").actor_id = Some("4444444444444444".into());
    store.replace(restored);
    let after = store.snapshot();
    assert_eq!(after.frp, before.frp);
    assert_eq!(after.simulation.actor_id, before.simulation.actor_id);
    assert_eq!(
        after.mock_monitor["0"].actor_id.as_deref(),
        Some("4444444444444444")
    );
}

/// An old config whose trackball card holds `hand = "lh"` loads; the
/// key is ignored and dropped on the next save.
#[test]
fn an_old_trackball_hand_setting_loads_and_is_ignored() {
    let (_dir, path) =
        config_dir("[trackball.0]\nname = \"Arcade\"\nclub = \"DR\"\nhand = \"lh\"\n");
    let (c, error) = load(&path);
    assert_eq!(error, None);
    let card = &c.trackball["0"];
    assert_eq!(card.name, "Arcade");
    assert_eq!(card.driver.hand, thumbswing::Hand::default());
    let written = std::fs::read_to_string(&path).expect("read");
    assert!(!written.contains("hand"), "{written}");
    assert!(written.contains("club = \"DR\""), "{written}");
}

/// A routing key no card holds at load stays as it is, and a card
/// added later at that index does not take the route.
#[test]
fn a_stale_routing_key_does_not_route_to_a_later_card() {
    let src = "[openconnect_client.0]\nname = \"GSPro\"\nfull_monitor = \"mevo.0\"\n";
    let (_dir, path) = config_dir(src);
    let store = SystemConfig::new(path);
    let c = store.snapshot();
    assert_eq!(
        c.openconnect_client["0"].full_monitor.as_deref(),
        Some("mevo.0")
    );
    store.update(|c| {
        c.mevo.insert(
            "0".into(),
            flighthook::MevoSection {
                name: "Mevo".into(),
                ..flighthook::MevoSection::default()
            },
        );
    });
    let c = store.snapshot();
    assert!(c.mevo["0"].actor_id.is_some());
    assert_eq!(
        c.openconnect_client["0"].full_monitor.as_deref(),
        Some("mevo.0"),
        "the stale key stays a missing monitor"
    );
}

/// The card map follows every change: a card added is found by key and
/// by id, a card removed is not.
#[test]
fn the_card_map_follows_changes() {
    let (_dir, path) = config_dir("");
    let store = SystemConfig::new(path);
    assert_eq!(store.actor_of_card("mock_monitor.0"), None);
    store.update(|c| {
        drop(
            c.mock_monitor
                .insert("0".into(), flighthook::MockMonitorSection::default()),
        )
    });
    let id = store.actor_of_card("mock_monitor.0").expect("id");
    assert_eq!(store.card_of_actor(&id).as_deref(), Some("mock_monitor.0"));
    assert_eq!(
        store.own_actor_ids().flighthook,
        store.snapshot().frp.actor_id
    );
    store.update(|c| drop(c.mock_monitor.remove("0")));
    assert_eq!(store.actor_of_card("mock_monitor.0"), None);
    assert_eq!(store.card_of_actor(&id), None);
}

/// A fresh config's `[simulation]` table holds the flight model's actor
/// id and nothing else.
#[test]
fn fresh_config_simulation_holds_only_actor_id() {
    let (dir, _) = config_dir("");
    let path = dir.0.join("new.toml");
    let (created, _) = load(&path);
    let written = std::fs::read_to_string(&path).expect("read");
    let id = created.simulation.actor_id.expect("flight model id");
    let table = format!("[simulation]\nactor_id = \"{id}\"\n");
    let at = written.find("[simulation]").expect("simulation table");
    let end = written[at + 1..]
        .find("\n[")
        .map_or(written.len(), |i| at + 1 + i);
    assert_eq!(&written[at..end], table, "{written}");
}

/// A `[simulation]` table that spells out the default values loads as
/// the defaults.
#[test]
fn simulation_with_explicit_defaults_loads() {
    let file = format!(
        "[simulation]\nmode = \"on\"\naltitude = \"0ft\"\ntemperature_c = 25.0\nsurface = \"fairway\"\nauto_chip = false\n{GOLDEN_DEFAULT_TOML}"
    );
    let c: FlighthookConfig = toml::from_str(&file).expect("parses");
    assert_eq!(c.simulation, flighthook::SimulationSection::default());
    let out = toml::to_string_pretty(&c).expect("ser");
    assert_eq!(out, GOLDEN_DEFAULT_TOML);
}

/// Ids survive load, save, load, and the second save writes the same
/// text as the first.
#[test]
fn ids_are_stable_across_load_and_save() {
    // A default `[simulation]`: the flight model's id is all its table
    // holds once written.
    let (dir, path) = config_dir("[mock_monitor.0]\nname = \"Mock\"\n");
    let store = SystemConfig::new(path.clone());
    assert_eq!(store.load_error(), None);
    let first = store.snapshot();
    let first_text = std::fs::read_to_string(&path).expect("read");
    let model_id = first
        .simulation
        .actor_id
        .clone()
        .expect("a flight model id");
    let table: toml::Table = toml::from_str(&first_text).expect("toml");
    let written = table["simulation"]
        .as_table()
        .expect("the [simulation] table");
    assert_eq!(written.len(), 1, "{first_text}");
    assert_eq!(written["actor_id"].as_str(), Some(model_id.as_str()));
    // Reloaded as written, the default table keeps its id.
    let reloaded = SystemConfig::new(path.clone());
    assert_eq!(
        reloaded.snapshot().simulation.actor_id.as_deref(),
        Some(model_id.as_str())
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), first_text);
    store.update(|_| {});
    let saved = std::fs::read_to_string(&path).expect("read");
    assert_eq!(saved, first_text);
    let (second, _) = load(&path);
    assert_same_config(&second, &first);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), first_text);
    drop(dir);
}

#[test]
fn file_without_simulation_parses_to_defaults() {
    let c: FlighthookConfig = toml::from_str(GOLDEN_DEFAULT_TOML).expect("parses");
    assert_eq!(c.simulation, flighthook::SimulationSection::default());
}

#[test]
fn shot_grouping_parses_from_toml_and_writes_back() {
    let c: FlighthookConfig = toml::from_str(GOLDEN_DEFAULT_TOML).expect("parses");
    assert_eq!(c.shot_grouping, flighthook::ShotGrouping::BySet);

    let file = format!("shot_grouping = \"by_club\"\n{GOLDEN_DEFAULT_TOML}");
    let c: FlighthookConfig = toml::from_str(&file).expect("parses");
    assert_eq!(c.shot_grouping, flighthook::ShotGrouping::ByClub);
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(out.contains("shot_grouping = \"by_club\"\n"), "{out}");
    let back: FlighthookConfig = toml::from_str(&out).expect("parses");
    assert_eq!(back, c);
}

#[test]
fn simulation_table_round_trips() {
    let src = "[simulation]\nmode = \"off\"\naltitude = \"5280ft\"\ntemperature_c = 30.0\nsurface = \"green\"\n";
    let file = format!("{src}\n[webserver.0]\nname = \"Web Server\"\nbind = \"0.0.0.0:5880\"\n");
    let c: FlighthookConfig = toml::from_str(&file).expect("parses");
    assert_eq!(c.simulation.mode, SimMode::Off);
    assert_eq!(c.simulation.surface, flighthook::SimSurface::Green);
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(out.contains(src), "{out}");
    // The table sits before the per-actor tables.
    let sim = out.find("[simulation]").expect("simulation table");
    let web = out.find("[webserver.0]").expect("webserver table");
    assert!(sim < web, "{out}");
}

/// `auto_chip` is written only when enabled, and a file without it
/// leaves Auto-chip disabled.
#[test]
fn auto_chip_setting_round_trips_in_toml() {
    let c: FlighthookConfig = toml::from_str(GOLDEN_DEFAULT_TOML).expect("parses");
    assert!(!c.simulation.auto_chip, "disabled when absent");
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(!out.contains("auto_chip"), "{out}");

    let file = format!("[simulation]\nauto_chip = true\n{GOLDEN_DEFAULT_TOML}");
    let c: FlighthookConfig = toml::from_str(&file).expect("parses");
    assert!(c.simulation.auto_chip);
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(out.contains("[simulation]"), "{out}");
    assert!(out.contains("auto_chip = true\n"), "{out}");
    let back: FlighthookConfig = toml::from_str(&out).expect("parses");
    assert_eq!(back, c);
}

/// A `[simulation]` table carrying unknown keys (e.g. `enabled`) loads,
/// ignoring them and keeping the default mode.
#[test]
fn unknown_simulation_keys_are_ignored_in_toml() {
    let src = "[simulation]\nenabled = false\ntemperature_c = 30.0\n";
    let c: FlighthookConfig = toml::from_str(src).expect("parses");
    assert_eq!(c.simulation.mode, SimMode::On);
    assert!((c.simulation.temperature_c - 30.0).abs() < 1e-9);
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(!out.contains("enabled"), "{out}");
    let back: FlighthookConfig = toml::from_str(&out).expect("parses");
    assert_eq!(back, c);
}

#[test]
fn simulation_mode_round_trips_and_is_omitted_at_default() {
    for mode in SimMode::all() {
        let c = FlighthookConfig {
            simulation: flighthook::SimulationSection {
                mode,
                ..flighthook::SimulationSection::default()
            },
            ..FlighthookConfig::default()
        };
        let out = toml::to_string_pretty(&c).expect("ser");
        if mode == SimMode::On {
            assert!(!out.contains("[simulation]"), "{out}");
        } else {
            let want = format!("[simulation]\nmode = \"{}\"\n", mode.key());
            assert!(out.contains(&want), "{out}");
        }
        let back: FlighthookConfig = toml::from_str(&out).expect("parses");
        assert_eq!(back, c);
    }
}

#[test]
fn legacy_migration_carries_simulation_through() {
    let src = r#"
[simulation]
mode = "off"
altitude = "5280ft"
temperature_c = 30.0
surface = "green"

[webserver]
name = "Web Server"
bind = "0.0.0.0:5880"
"#;
    let legacy: LegacyFlighthookConfig = toml::from_str(src).expect("parses");
    let config = legacy.into_current();
    assert_eq!(config.simulation.mode, SimMode::Off);
    assert_eq!(config.simulation.surface, flighthook::SimSurface::Green);
    assert!((config.simulation.temperature_c - 30.0).abs() < 1e-9);
}

#[test]
fn legacy_config_without_simulation_migrates_to_defaults() {
    let src = r#"
[webserver]
name = "Web Server"
bind = "0.0.0.0:5880"
"#;
    let legacy: LegacyFlighthookConfig = toml::from_str(src).expect("parses");
    let config = legacy.into_current();
    assert_eq!(config.simulation, flighthook::SimulationSection::default());
}

#[test]
fn mevo_environment_is_optional_in_toml() {
    let without = "[mevo.0]\nname = \"Mevo\"\naddress = \"192.168.2.1:5100\"\n";
    let c: FlighthookConfig = toml::from_str(without).expect("parses");
    assert_eq!(c.mevo["0"].driver.environment, None);
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(!out.contains("environment"), "{out}");

    let with = format!("{without}environment = \"outdoor\"\n");
    let c: FlighthookConfig = toml::from_str(&with).expect("parses");
    assert_eq!(
        c.mevo["0"].driver.environment,
        Some(flighthook::FlightScopeEnvironment::Outdoor)
    );
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(out.contains("environment = \"outdoor\""), "{out}");
}

#[test]
fn mevo_spin_source_is_omitted_at_default_in_toml() {
    let without = "[mevo.0]\nname = \"Xi\"\naddress = \"192.168.2.1:5100\"\n";
    let c: FlighthookConfig = toml::from_str(without).expect("parses");
    assert_eq!(c.mevo["0"].driver.spin_source, flighthook::SpinSource::D4);
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(!out.contains("spin_source"), "{out}");

    let with = format!("{without}spin_source = \"e8\"\n");
    let c: FlighthookConfig = toml::from_str(&with).expect("parses");
    assert_eq!(c.mevo["0"].driver.spin_source, flighthook::SpinSource::E8);
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(out.contains("spin_source = \"e8\"\n"), "{out}");
    let back: FlighthookConfig = toml::from_str(&out).expect("parses");
    assert_eq!(back, c);
}

/// Removes the directory on drop, so a failing assertion doesn't leak it.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn simulation_reads_the_live_section() {
    let dir =
        TempDir(std::env::temp_dir().join(format!("flighthook-config-test-{}", generate_id())));
    let cfg = SystemConfig::new(dir.0.join("config.toml"));
    assert_eq!(cfg.simulation().mode, SimMode::On);
    cfg.update(|c| c.simulation.mode = SimMode::Off);
    assert_eq!(cfg.simulation().mode, SimMode::Off);
}

#[test]
fn a_hand_edited_zero_keep_shots_loads_and_keeps_one_clip() {
    let dir = std::env::temp_dir().join(format!("flighthook-replay-load-{}", generate_id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[replay]\nkeep_shots = 0\n").expect("write");
    let c = load(&path).0;
    assert_eq!(c.replay.keep_shots, 0, "the file is not rewritten");
    assert!(c.replay.error().is_some());
    assert_eq!(flighthook_cameras::clips::keep(&c.replay), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn impact_lead_s_is_omitted_when_absent_and_kept_when_set_in_toml() {
    for kind in [
        "mevo",
        "r10",
        "square",
        "exputt",
        "mock_monitor",
        "openconnect_server",
    ] {
        let plain = format!("[{kind}.0]\nname = \"X\"\n");
        let c: FlighthookConfig = toml::from_str(&plain).expect("parse");
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(!out.contains("impact_lead_s"), "{kind}: {out}");
        // What was saved without the field parses and saves the same.
        let again: FlighthookConfig = toml::from_str(&out).expect("parse");
        assert_eq!(toml::to_string_pretty(&again).expect("ser"), out);

        let set = format!("[{kind}.0]\nname = \"X\"\nimpact_lead_s = 0.75\n");
        let c: FlighthookConfig = toml::from_str(&set).expect("parse");
        assert_eq!(c.configured_impact_lead_s(&format!("{kind}.0")), Some(0.75));
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(out.contains("impact_lead_s = 0.75"), "{kind}: {out}");
    }
}

/// A file that still holds `[simulation] tournament` loads with the
/// rest of its section, and the next save leaves the key out.
#[test]
fn a_file_with_the_tournament_key_loads() {
    for value in ["true", "false"] {
        let dir = std::env::temp_dir().join(format!("flighthook-tournament-{}", generate_id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("config.toml");
        let contents = format!("[simulation]\nauto_chip = true\ntournament = {value}\n");
        std::fs::write(&path, contents).expect("write");
        let cfg = SystemConfig::new(path);
        assert_eq!(cfg.load_error(), None, "{value}");
        assert!(cfg.simulation().auto_chip, "{value}");
        let out = toml::to_string_pretty(&cfg.snapshot()).expect("ser");
        assert!(!out.contains("tournament"), "{value}: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_file_with_an_invalid_impact_lead_loads_and_uses_the_default() {
    let dir = std::env::temp_dir().join(format!("flighthook-lead-{}", generate_id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[square.0]\nname = \"Omni\"\nimpact_lead_s = 7.5\n").expect("write");
    let cfg = SystemConfig::new(path);
    assert!(cfg.snapshot().impact_lead_error().is_some());
    assert_eq!(
        settings::impact_lead_s(&cfg.view().read(), "square.0"),
        Some(1.0)
    );
    assert_eq!(settings::longest_reach_s(&cfg.view().read()), 1.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn camera_latency_s_is_omitted_when_absent_and_kept_when_set_in_toml() {
    let plain = "[camera.0]\nname = \"Cam\"\ncamera = \"test-pattern\"\n";
    let c: FlighthookConfig = toml::from_str(plain).expect("parse");
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(!out.contains("latency_s"), "{out}");
    // What was saved without the field parses and saves the same.
    let again: FlighthookConfig = toml::from_str(&out).expect("parse");
    assert_eq!(toml::to_string_pretty(&again).expect("ser"), out);

    let set = format!("{plain}latency_s = 2.5\n");
    let c: FlighthookConfig = toml::from_str(&set).expect("parse");
    assert_eq!(c.camera["0"].latency_s, Some(2.5));
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(out.contains("latency_s = 2.5"), "{out}");
}

#[test]
fn a_file_with_an_invalid_camera_latency_loads_and_uses_zero() {
    let dir = std::env::temp_dir().join(format!("flighthook-latency-{}", generate_id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        "[camera.0]\nname = \"A\"\ncamera = \"test-pattern\"\nlatency_s = 12\n\n\
             [camera.1]\nname = \"B\"\ncamera = \"test-pattern\"\nlatency_s = 1.5\n",
    )
    .expect("write");
    let cfg = SystemConfig::new(path);
    assert!(cfg.snapshot().latency_error().is_some());
    assert_eq!(
        settings::camera_latency_s(&cfg.view().read(), "camera.0"),
        0.0
    );
    assert_eq!(
        settings::camera_latency_s(&cfg.view().read(), "camera.1"),
        1.5
    );
    assert_eq!(
        settings::camera_latency_s(&cfg.view().read(), "camera.9"),
        0.0
    );
    assert_eq!(
        settings::camera_latency_s(&cfg.view().read(), "mevo.0"),
        0.0
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mevo_launch_only_spin_is_omitted_at_default_in_toml() {
    let without = "[mevo.0]\nname = \"Xi\"\naddress = \"192.168.2.1:5100\"\n";
    let c: FlighthookConfig = toml::from_str(without).expect("parses");
    assert_eq!(c.mevo["0"].launch_only_spin, 100.0);
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(!out.contains("launch_only_spin"), "{out}");

    for (line, v) in [
        ("launch_only_spin = 0\n", 0.0),
        ("launch_only_spin = 75.5\n", 75.5),
    ] {
        let c: FlighthookConfig = toml::from_str(&format!("{without}{line}")).expect("parses");
        assert_eq!(c.mevo["0"].launch_only_spin, v);
        let out = toml::to_string_pretty(&c).expect("ser");
        assert!(out.contains("launch_only_spin = "), "{out}");
        let back: FlighthookConfig = toml::from_str(&out).expect("parses");
        assert_eq!(back, c);
    }
}

/// A config file from before `openconnect_client`: one GSPro output
/// routed to a Mevo.
const GSPRO_TOML: &str = r#"default_units = "imperial"
chipping_clubs = ["GW", "SW", "LW"]
putting_clubs = ["PT"]

[webserver.0]
name = "Web Server"
bind = "0.0.0.0:5880"

[mevo.0]
name = "Mevo+"
address = "192.168.2.1:5100"

[gspro.0]
name = "Bay GSPro"
address = "192.168.1.20:921"
full_monitor = "mevo.0"
"#;

/// The error `load` logs for config text `contents`.
fn load_error(contents: &str) -> String {
    let error = toml::from_str::<FlighthookConfig>(contents).expect_err("a bad config");
    located_error(contents, error).to_string()
}

/// A type error in a card's driver settings, which serde reads through
/// the section's flattened buffer, is located by its own key.
#[test]
fn a_driver_settings_type_error_names_its_key_and_line() {
    let text = "[webserver.0]\n\
                    name = \"Web Server\"\n\
                    bind = \"127.0.0.1:5880\"\n\
                    \n\
                    [mock_monitor.0]\n\
                    name = \"Mock\"\n\
                    \n\
                    [mock_monitor.1]\n\
                    name = \"Mock 2\"\n\
                    shot_interval_s = \"x\"\n";
    let error = kept_error(text);
    assert_eq!(
        error.error,
        "line 10, column 1: mock_monitor.1: shot_interval_s: invalid type: string \"x\", expected f64"
    );
}

/// A mock card's section in a file reads and writes as before its
/// driver settings were flattened in: a current file with only its name
/// loads with the driver's defaults and is not rewritten; driver settings
/// read from the file (an integer into a seconds field) and are written
/// back only when they differ from the defaults.
#[test]
fn a_mock_section_file_round_trips_unchanged() {
    let mut written = FlighthookConfig::default();
    written.mock_monitor.insert(
        "0".into(),
        flighthook::MockMonitorSection {
            name: "Mock".into(),
            ..flighthook::MockMonitorSection::default()
        },
    );
    let text = toml::to_string_pretty(&written).expect("toml");
    assert!(
        text.ends_with("[mock_monitor.0]\nname = \"Mock\"\n"),
        "{text}"
    );
    let text = with_ids(&text);
    let (dir, path) = config_dir(&text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    assert_eq!(plain(&config), written);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        text,
        "not rewritten"
    );
    assert_eq!(files(&dir.0), ["config.toml"], "no backup");

    let text = "[mock_monitor.0]\nname = \"Mock\"\nshot_interval_s = 5\n";
    let config: FlighthookConfig = toml::from_str(text).expect("parses");
    assert_eq!(config.mock_monitor["0"].driver.shot_interval_s, 5.0);
    let saved = toml::to_string_pretty(&config).expect("toml");
    assert!(
        saved.contains("[mock_monitor.0]\nname = \"Mock\"\nshot_interval_s = 5.0\n"),
        "{saved}"
    );
    assert_eq!(
        toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
        config
    );
}

/// An R10 section as flighthook 0.1.24 saved it (range, camera timing,
/// a second card that is off) loads into the same values, the range now
/// in the driver's settings, and the file is rewritten once, only to give
/// its actors ids (the global club lists go: no Open Connect card takes
/// them), with a backup.
#[test]
fn an_r10_section_file_from_0_1_24_loads_unchanged() {
    let body = "\n\
                    [r10.0]\n\
                    name = \"Garmin R10\"\n\
                    range = \"7ft\"\n\
                    impact_lead_s = 2.5\n\
                    \n\
                    [r10.1]\n\
                    name = \"Bay 2\"\n\
                    enabled = false\n\
                    range = \"2.1m\"\n";
    let expected = format!("default_units = \"imperial\"\n{body}");
    let text = format!("default_units = \"imperial\"\n{GLOBAL_CLUB_LISTS}{body}");
    let (dir, path) = config_dir(&text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    let first = &config.r10["0"];
    assert_eq!(first.name, "Garmin R10");
    assert_eq!(first.driver.range, Some(flighthook::Distance::Feet(7.0)));
    assert_eq!(first.impact_lead_s, Some(2.5));
    assert!(first.enabled);
    let second = &config.r10["1"];
    assert!(!second.enabled);
    assert_eq!(second.driver.range, Some(flighthook::Distance::Meters(2.1)));
    assert_eq!(config.device_error(), None);
    assert_only_ids_added(&dir, &path, &config, &text, &expected);
    let saved = toml::to_string_pretty(&config).expect("toml");
    assert!(
        without_ids(&saved)
            .contains("[r10.0]\nname = \"Garmin R10\"\nimpact_lead_s = 2.5\nrange = \"7ft\"\n"),
        "{saved}"
    );
    assert_same_config(
        &toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
        &config,
    );
}

/// FlightScope sections as flighthook 0.1.24 saved them (from the card:
/// every setting stated; a Xi with its spin source, launch-only spin and
/// camera timing; one that is off with its settings by hand) load into
/// the same values, the device settings now in the driver's, with no
/// device error; the file is rewritten once, only to give its actors ids
/// (the global club lists go: no Open Connect card takes them), with a
/// backup, and the saved form round-trips.
#[test]
fn a_mevo_section_file_from_0_1_24_loads_unchanged() {
    let body = "\n\
                    [mevo.0]\n\
                    name = \"Mevo WiFi\"\n\
                    address = \"192.168.2.1:5100\"\n\
                    ball_type = 1\n\
                    tee_height = \"1.5in\"\n\
                    range = \"8ft\"\n\
                    surface_height = \"0in\"\n\
                    track_pct = 80.0\n\
                    use_estimated = false\n\
                    camera_mode = \"standard\"\n\
                    environment = \"limited_flight\"\n\
                    \n\
                    [mevo.1]\n\
                    name = \"Xi\"\n\
                    address = \"10.0.0.7:5100\"\n\
                    ball_type = 1\n\
                    tee_height = \"1.5in\"\n\
                    range = \"9ft\"\n\
                    surface_height = \"1in\"\n\
                    track_pct = 80.0\n\
                    use_estimated = true\n\
                    camera_mode = \"standard\"\n\
                    environment = \"outdoor\"\n\
                    spin_source = \"auto\"\n\
                    launch_only_spin = 120.0\n\
                    impact_lead_s = 0.25\n\
                    \n\
                    [mevo.2]\n\
                    name = \"Garage\"\n\
                    enabled = false\n\
                    address = \"192.168.2.1:5100\"\n\
                    camera_mode = \"raw_fusion\"\n";
    let expected = format!("default_units = \"imperial\"\n{body}");
    let text = format!("default_units = \"imperial\"\n{GLOBAL_CLUB_LISTS}{body}");
    let (dir, path) = config_dir(&text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    let card = &config.mevo["0"];
    assert_eq!(card.name, "Mevo WiFi");
    assert_eq!(card.driver.address.as_deref(), Some("192.168.2.1:5100"));
    assert_eq!(card.driver.ball_type, Some(1));
    assert_eq!(
        card.driver.tee_height,
        Some(flighthook::Distance::Inches(1.5))
    );
    assert_eq!(card.driver.range, Some(flighthook::Distance::Feet(8.0)));
    assert_eq!(
        card.driver.surface_height,
        Some(flighthook::Distance::Inches(0.0))
    );
    assert_eq!(card.driver.track_pct, Some(80.0));
    assert_eq!(card.driver.use_estimated, Some(false));
    assert_eq!(
        card.driver.camera_mode,
        Some(flighthook::CameraMode::Standard)
    );
    assert_eq!(
        card.driver.environment,
        Some(flighthook::FlightScopeEnvironment::LimitedFlight)
    );
    assert_eq!(card.driver.spin_source, flighthook::SpinSource::D4);
    assert_eq!(
        card.launch_only_spin,
        flighthook::MevoSection::DEFAULT_LAUNCH_ONLY_SPIN
    );
    let xi = &config.mevo["1"];
    assert_eq!(xi.driver.spin_source, flighthook::SpinSource::Auto);
    assert_eq!(
        xi.driver.environment,
        Some(flighthook::FlightScopeEnvironment::Outdoor)
    );
    assert_eq!(xi.launch_only_spin, 120.0);
    assert_eq!(xi.impact_lead_s, Some(0.25));
    let off = &config.mevo["2"];
    assert!(!off.enabled);
    assert_eq!(off.driver.ball_type, None);
    assert_eq!(
        off.driver.camera_mode,
        Some(flighthook::CameraMode::RawFusion)
    );
    assert_eq!(config.device_error(), None);
    assert_only_ids_added(&dir, &path, &config, &text, &expected);
    let saved = toml::to_string_pretty(&config).expect("toml");
    assert_same_config(
        &toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
        &config,
    );
}

/// A FlightScope setting the driver refuses (a hand-edited file) loads
/// as written and is reported by card and field; a setting of the wrong
/// type names its card, key and line.
#[test]
fn a_refused_mevo_setting_loads_and_is_reported() {
    let text = "[mevo.0]\nname = \"Mevo\"\ntrack_pct = 150.0\n";
    let (_dir, path) = config_dir(text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    assert_eq!(config.mevo["0"].driver.track_pct, Some(150.0));
    assert_eq!(
        config.device_error().as_deref(),
        Some("mevo.0: track_pct: must be 0 to 100")
    );

    let text = "[mevo.0]\n\
                    name = \"Mevo\"\n\
                    \n\
                    [mevo.3]\n\
                    name = \"Xi\"\n\
                    spin_source = \"e9\"\n";
    let error = kept_error(text);
    assert!(
        error
            .error
            .starts_with("line 6, column 1: mevo.3: spin_source: "),
        "{}",
        error.error
    );
}

/// Square sections as flighthook 0.1.24 saved them load into the same
/// values, now in the driver's settings, with no device error; a file
/// whose values are not at their defaults is rewritten once, only to give
/// its actors ids (the global club lists go: no Open Connect card takes
/// them), with a backup, and the saved form round-trips.
#[test]
fn a_square_section_file_from_0_1_24_loads_unchanged() {
    let body = "\n\
                    [square.0]\n\
                    name = \"Square Golf Omni\"\n\
                    address = \"SquareGolf(CD01)\"\n\
                    club = \"PW\"\n\
                    advanced_spin = false\n\
                    discard_non_putting_zero_spin = false\n\
                    impact_lead_s = 1.5\n\
                    \n\
                    [square.0.dot_bottom_to_face_center_mm]\n\
                    7I = 21.5\n\
                    DR = 19.0\n\
                    \n\
                    [square.1]\n\
                    name = \"Bay 2\"\n\
                    enabled = false\n\
                    address = \"02:00:00:AB:CD:01\"\n";
    let expected = format!("default_units = \"imperial\"\n{body}");
    let text = format!("default_units = \"imperial\"\n{GLOBAL_CLUB_LISTS}{body}");
    let (dir, path) = config_dir(&text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    let first = &config.square["0"];
    assert_eq!(first.name, "Square Golf Omni");
    assert!(first.enabled);
    assert_eq!(first.impact_lead_s, Some(1.5));
    assert_eq!(first.driver.address.as_deref(), Some("SquareGolf(CD01)"));
    assert_eq!(first.driver.club, "PW");
    assert!(!first.driver.advanced_spin);
    assert!(!first.driver.discard_non_putting_zero_spin);
    assert_eq!(
        first.driver.dot_bottom_to_face_center_mm,
        std::collections::BTreeMap::from([("7I".into(), 21.5), ("DR".into(), 19.0)])
    );
    let second = &config.square["1"];
    assert!(!second.enabled);
    assert_eq!(second.driver.address.as_deref(), Some("02:00:00:AB:CD:01"));
    assert_eq!(second.driver, {
        let mut config = flighthook::SquareConfig::default();
        config.address = Some("02:00:00:AB:CD:01".into());
        config
    });
    assert_eq!(config.device_error(), None);
    assert_only_ids_added(&dir, &path, &config, &text, &expected);
    let saved = toml::to_string_pretty(&config).expect("toml");
    assert_same_config(
        &toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
        &config,
    );
}

/// What the 0.1.24 card saved for a new Omni writes the zero-spin switch
/// out at its default: it loads into the same values, and the file is
/// rewritten once without the default, with a backup of the original.
#[test]
fn a_square_section_with_explicit_defaults_is_rewritten_once_with_a_backup() {
    let text = "[square.0]\n\
                    name = \"Square Golf Omni\"\n\
                    discard_non_putting_zero_spin = true\n";
    let (dir, path) = config_dir(text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    assert_eq!(
        plain(&config).square["0"],
        flighthook::SquareSection::default()
    );
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    assert_eq!(
        std::fs::read_to_string(dir.0.join(backup_name())).expect("backup"),
        text
    );
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("[square.0]"), "{saved}");
    assert!(!saved.contains("discard_non_putting_zero_spin"), "{saved}");
    // Loading the rewritten file changes nothing more.
    let (again, _) = load(&path);
    assert_same_config(&again, &config);
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
}

/// A Square setting of the wrong type names its key's line.
#[test]
fn a_square_setting_type_error_names_its_key_and_line() {
    let text = "[square.0]\n\
                    name = \"Omni\"\n\
                    \n\
                    [square.3]\n\
                    name = \"Bay 2\"\n\
                    advanced_spin = \"maybe\"\n";
    let error = kept_error(text);
    assert!(
        error
            .error
            .starts_with("line 6, column 1: square.3: advanced_spin: "),
        "{}",
        error.error
    );
}

/// A range the R10 driver refuses (0 or less) loads as written and names
/// the card, which does not start; a range of the wrong type names its
/// key's line.
#[test]
fn a_refused_r10_range_loads_and_is_reported() {
    let text = "[r10.0]\nname = \"Garmin R10\"\nrange = \"0ft\"\n";
    let (_dir, path) = config_dir(text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    assert_eq!(
        config.r10["0"].driver.range,
        Some(flighthook::Distance::Feet(0.0))
    );
    assert_eq!(
        config.device_error().as_deref(),
        Some("r10.0: range: must be a distance greater than 0")
    );

    let text = "[r10.0]\n\
                    name = \"Garmin R10\"\n\
                    \n\
                    [r10.2]\n\
                    name = \"Bay 2\"\n\
                    range = 7\n";
    let error = kept_error(text);
    assert!(
        error.error.starts_with("line 6, column 1: r10.2: range: "),
        "{}",
        error.error
    );
}

/// ExPutt sections as flighthook 0.1.24 saved them (an address and a
/// camera timing, a second card that is off and finds its device) load
/// into the same values, the address now in the driver's settings, and
/// the file is rewritten once, only to give its actors ids (the global
/// club lists go: no Open Connect card takes them), with a backup.
#[test]
fn an_exputt_section_file_from_0_1_24_loads_unchanged() {
    let body = "\n\
                    [exputt.0]\n\
                    name = \"ExPutt\"\n\
                    address = \"192.168.1.50\"\n\
                    impact_lead_s = 0.75\n\
                    \n\
                    [exputt.1]\n\
                    name = \"Practice\"\n\
                    enabled = false\n";
    let expected = format!("default_units = \"imperial\"\n{body}");
    let text = format!("default_units = \"imperial\"\n{GLOBAL_CLUB_LISTS}{body}");
    let (dir, path) = config_dir(&text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    let first = &config.exputt["0"];
    assert_eq!(first.name, "ExPutt");
    assert_eq!(first.driver.address.as_deref(), Some("192.168.1.50"));
    assert_eq!(first.impact_lead_s, Some(0.75));
    assert!(first.enabled);
    let second = &config.exputt["1"];
    assert!(!second.enabled);
    assert_eq!(second.driver.address, None);
    assert_eq!(config.device_error(), None);
    assert_only_ids_added(&dir, &path, &config, &text, &expected);
    let saved = toml::to_string_pretty(&config).expect("toml");
    assert!(
        without_ids(&saved).contains(
            "[exputt.0]\nname = \"ExPutt\"\nimpact_lead_s = 0.75\naddress = \"192.168.1.50\"\n"
        ),
        "{saved}"
    );
    assert_same_config(
        &toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
        &config,
    );
}

/// A Trackball section as the card writes it (device, club, a tuning
/// and a thresholds table) and one that is off load into the
/// driver's settings, and the current file is not rewritten.
#[test]
fn a_trackball_section_file_loads_unchanged() {
    let text = "default_units = \"imperial\"\n\
                    \n\
                    [trackball.0]\n\
                    name = \"Arcade\"\n\
                    device = \"usb-Example_Trackball-event-mouse\"\n\
                    club = \"DR\"\n\
                    cpi = 800\n\
                    \n\
                    [trackball.0.thresholds]\n\
                    coil_timeout_s = 20.0\n\
                    \n\
                    [trackball.0.tuning]\n\
                    full_power_speed = 40.0\n\
                    curve_sense = \"physical\"\n\
                    \n\
                    [trackball.1]\n\
                    name = \"Thumb\"\n\
                    enabled = false\n";
    let text = with_ids(text);
    let (dir, path) = config_dir(&text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    let first = &config.trackball["0"];
    assert_eq!(first.name, "Arcade");
    let d = &first.driver;
    assert_eq!(d.device(), Some("usb-Example_Trackball-event-mouse"));
    assert_eq!(d.club, flighthook::TrackballClub::Driver);
    assert!((d.detector.cpi - 800.0).abs() < f64::EPSILON);
    assert!(!d.detector.grab);
    assert!((d.detector.thresholds.coil_timeout_s - 20.0).abs() < f64::EPSILON);
    assert!((d.tuning.full_power_speed - 40.0).abs() < f64::EPSILON);
    assert_eq!(
        d.tuning.curve_sense,
        flighthook::TrackballCurveSense::Physical
    );
    let second = &config.trackball["1"];
    assert!(!second.enabled);
    assert!(!second.driver_settings().detector.grab);
    assert_eq!(config.device_error(), None);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        text,
        "not rewritten"
    );
    assert_eq!(files(&dir.0), ["config.toml"], "no backup");
    let saved = toml::to_string_pretty(&config).expect("toml");
    assert_same_config(
        &toml::from_str::<FlighthookConfig>(&saved).expect("parses"),
        &config,
    );
}

/// A `grab` key in a Trackball section loads without an error or a
/// refused setting, never turns the grab on, and leaves a current file as
/// it is; the next save leaves the key out.
#[test]
fn a_trackball_grab_key_loads_and_does_not_grab() {
    let text = "default_units = \"imperial\"\n\
                    \n\
                    [trackball.0]\n\
                    name = \"Arcade\"\n\
                    device = \"usb-Example_Trackball-event-mouse\"\n\
                    grab = true\n\
                    \n\
                    [trackball.1]\n\
                    name = \"Thumb\"\n\
                    enabled = false\n\
                    grab = false\n";
    let text = with_ids(text);
    let (dir, path) = config_dir(&text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    assert_eq!(config.device_error(), None);
    for section in config.trackball.values() {
        assert!(!section.driver.detector.grab);
        assert!(!section.driver_settings().detector.grab);
    }
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        text,
        "not rewritten"
    );
    assert_eq!(files(&dir.0), ["config.toml"], "no backup");
    save_to(&path, &config);
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(!saved.contains("grab"), "{saved}");
    assert!(saved.contains("[trackball.0]"), "{saved}");
    assert_same_config(&load(&path).0, &config);
}

/// A Trackball setting the driver refuses loads as written and names
/// the card, which does not start; a club it does not know names its
/// key's line.
#[test]
fn a_refused_trackball_setting_loads_and_is_reported() {
    let text = "[trackball.0]\nname = \"Trackball\"\ncpi = 0\n";
    let (_dir, path) = config_dir(text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    assert_eq!(
        config.device_error().as_deref(),
        Some("trackball.0: cpi: must be greater than 0")
    );

    let text = "[trackball.0]\n\
                    name = \"Trackball\"\n\
                    \n\
                    [trackball.3]\n\
                    name = \"Thumb\"\n\
                    club = \"ZZ\"\n";
    let error = kept_error(text);
    assert!(
        error
            .error
            .starts_with("line 6, column 1: trackball.3: club: "),
        "{}",
        error.error
    );
}

/// An ExPutt address the driver refuses (not an IP) loads as written and
/// names the card, which does not start; an address of the wrong type
/// names its key's line.
#[test]
fn a_refused_exputt_address_loads_and_is_reported() {
    let text = "[exputt.0]\nname = \"ExPutt\"\naddress = \"exputt.local\"\n";
    let (_dir, path) = config_dir(text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    assert_eq!(
        config.exputt["0"].driver.address.as_deref(),
        Some("exputt.local")
    );
    let refused = config.device_error().expect("refused");
    assert!(
        refused.starts_with("exputt.0: address: ") && refused.contains("exputt.local"),
        "{refused}"
    );

    let text = "[exputt.0]\n\
                    name = \"ExPutt\"\n\
                    \n\
                    [exputt.3]\n\
                    name = \"Practice\"\n\
                    address = 50\n";
    let error = kept_error(text);
    assert!(
        error
            .error
            .starts_with("line 6, column 1: exputt.3: address: "),
        "{}",
        error.error
    );
}

/// A value the mock driver refuses does not fail the load: the config
/// loads as written and names the card, which does not start.
#[test]
fn refused_driver_settings_load_and_are_reported() {
    let text = "[mock_monitor.0]\nname = \"Mock\"\nshot_interval_s = -1\n";
    let (_dir, path) = config_dir(text);
    let (config, error) = load(&path);
    assert_eq!(error, None);
    assert_eq!(config.mock_monitor["0"].driver.shot_interval_s, -1.0);
    assert_eq!(
        config.device_error().as_deref(),
        Some("mock_monitor.0: shot_interval_s: must be 0 to 86400")
    );
}

#[test]
fn a_parse_error_names_the_line_of_the_bad_value() {
    let simulation = "default_units = \"metric\"\n\
                          \n\
                          [webserver.0]\n\
                          name = \"Web Server\"\n\
                          bind = \"127.0.0.1:5880\"\n\
                          \n\
                          [simulation]\n\
                          temperature_c = \"hot\"\n";
    let error = load_error(simulation);
    assert!(error.contains("line 8"), "{error}");

    let client = "[mevo.0]\n\
                      name = \"Mevo+\"\n\
                      \n\
                      [openconnect_client.0]\n\
                      name = \"GSPro\"\n\
                      full_monitor = 7\n";
    let error = load_error(client);
    assert!(error.contains("line 6"), "{error}");

    let gspro = "[mevo.0]\n\
                     name = \"Mevo+\"\n\
                     \n\
                     [gspro.0]\n\
                     name = 7\n";
    let error = load_error(gspro);
    assert!(error.contains("line 5"), "{error}");
}

#[test]
fn gspro_config_loads_as_openconnect_client_and_saves_the_new_name() {
    let dir =
        TempDir(std::env::temp_dir().join(format!("flighthook-config-test-{}", generate_id())));
    std::fs::create_dir_all(&dir.0).expect("mkdir");
    let path = dir.0.join("config.toml");
    std::fs::write(&path, GSPRO_TOML).expect("write");

    let cfg = SystemConfig::new(path.clone());
    let c = cfg.snapshot();
    let client = &c.openconnect_client["0"];
    assert_eq!(client.name, "Bay GSPro");
    assert_eq!(client.driver.address.as_deref(), Some("192.168.1.20:921"));
    let routed = client.full_monitor.as_deref().expect("routed");
    assert_eq!(c.card_of_actor(routed).as_deref(), Some("mevo.0"));
    assert_eq!(c.mevo["0"].name, "Mevo+");

    // The same actor, under its new ID, as the old file started.
    let ids: Vec<_> = crate::actors::resolve_actors(&c)
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert!(ids.contains(&"openconnect_client.0".to_string()), "{ids:?}");
    assert!(!ids.iter().any(|id| id.starts_with("gspro.")), "{ids:?}");

    // Loading wrote the new name; so does every later save.
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("[openconnect_client.0]"), "{saved}");
    assert!(!saved.contains("gspro"), "{saved}");
    cfg.update(|c| c.simulation.mode = SimMode::Off);
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("[openconnect_client.0]"), "{saved}");
    assert!(!saved.contains("gspro"), "{saved}");
    let back: FlighthookConfig = toml::from_str(&saved).expect("parses");
    assert_eq!(back.openconnect_client, c.openconnect_client);
}

/// A fresh directory holding `config.toml` with `contents`.
fn config_dir(contents: &str) -> (TempDir, PathBuf) {
    let dir =
        TempDir(std::env::temp_dir().join(format!("flighthook-config-test-{}", generate_id())));
    std::fs::create_dir_all(&dir.0).expect("mkdir");
    let path = dir.0.join("config.toml");
    std::fs::write(&path, contents).expect("write");
    (dir, path)
}

/// The names of the files in `dir`, sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The name of the first backup of `config.toml`.
fn backup_name() -> String {
    format!("config.pre-{}.toml", env!("CARGO_PKG_VERSION"))
}

/// The keys that hold a generated actor id in a config file.
const ID_KEYS: [&str; 2] = ["actor_id", "auto_chip_actor_id"];

/// The keys that route an output to a card.
const ROUTE_KEYS: [&str; 3] = ["full_monitor", "chipping_monitor", "putting_monitor"];

/// The value of config line `line` when it sets one of `keys` to a
/// string: `(key, value)`.
fn string_setting<'a>(line: &'a str, keys: &[&'a str]) -> Option<(&'a str, &'a str)> {
    let line = line.trim();
    keys.iter().find_map(|&key| {
        let value = line.strip_prefix(key)?.trim_start().strip_prefix('=')?;
        let value = value.trim().strip_prefix('"')?;
        Some((key, &value[..value.find('"')?]))
    })
}

/// The table a config line `line` opens (`mevo.0` for `[mevo.0]`), if it
/// is a table header.
fn table_header(line: &str) -> Option<&str> {
    let line = line.trim();
    line.strip_prefix('[')
        .filter(|rest| !rest.starts_with('['))?
        .strip_suffix(']')
}

/// What the app writes for top-level table `name` at its defaults with
/// no id: the table an id alone makes the app write. `None` for a table
/// that is never written for an id alone.
fn table_at_defaults(name: &str) -> Option<toml::Value> {
    match name {
        "frp" => toml::Value::try_from(flighthook::FrpSection::default()).ok(),
        "simulation" => toml::Value::try_from(flighthook::SimulationSection::default()).ok(),
        _ => None,
    }
}

/// Config text `text` with its generated actor ids taken out: every
/// line setting one ([`ID_KEYS`]) is removed, a table left empty by that
/// or holding only its defaults ([`table_at_defaults`]: written only for
/// the id) is removed with its blank lines, and every other value that
/// is one of those ids (output routing) names the table the id was set
/// in (the card's key) instead. Everything else is kept byte for byte.
fn without_ids(text: &str) -> String {
    let mut owners: Vec<(String, String)> = Vec::new();
    let mut emptied: Vec<(usize, &str)> = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    let mut table = None;
    for line in text.split_inclusive('\n') {
        if let Some(name) = table_header(line) {
            table = Some((name, lines.len()));
        } else if let Some((_, id)) = string_setting(line, &ID_KEYS) {
            let (name, at) = table.expect("an id outside a table");
            owners.push((id.to_owned(), name.to_owned()));
            emptied.push((at, name));
            continue;
        }
        lines.push(line);
    }
    let blank = |line: &str| line.trim().is_empty();
    let mut keep = vec![true; lines.len()];
    for (at, name) in emptied {
        let end = (at + 1..lines.len())
            .find(|&i| table_header(lines[i]).is_some())
            .unwrap_or(lines.len());
        let rest: String = lines[at + 1..end].concat();
        let only_defaults = || {
            let rest = rest.parse::<toml::Value>().ok();
            rest.zip(table_at_defaults(name))
                .is_some_and(|(rest, defaults)| same_value(&rest, &defaults))
        };
        if !blank(&rest) && !only_defaults() {
            continue;
        }
        keep[at..end].fill(false);
        if end == lines.len() {
            for i in (0..at).rev().take_while(|&i| blank(lines[i])) {
                keep[i] = false;
            }
        }
    }
    lines
        .into_iter()
        .zip(keep)
        .filter(|&(_, kept)| kept)
        .map(|(line, _)| {
            owners.iter().fold(line.to_owned(), |line, (id, owner)| {
                line.replace(&format!("\"{id}\""), &format!("\"{owner}\""))
            })
        })
        .collect()
}

/// Config text `text`, from before actor ids, as a current file: the
/// ids its first load gives, those the app saves, are written in under
/// their tables' headers (a table the text lacks, such as `[frp]` or a
/// `[simulation]` at its defaults, is added at the end as the app writes
/// it), and output routing that names a card by its key names the
/// card's id. Comments, order and whitespace are kept, so
/// [`without_ids`] gives `text` back.
fn with_ids(text: &str) -> String {
    let mut config: FlighthookConfig = toml::from_str(text).expect("parses");
    give_actor_ids(&mut config);
    let saved: toml::Table = toml::to_string_pretty(&config)
        .expect("ser")
        .parse()
        .expect("toml");
    let table_of = |name: &str| {
        name.split('.')
            .try_fold(&saved, |t, part| t.get(part)?.as_table())
    };
    let id_lines = |name: &str| -> String {
        let Some(table) = table_of(name) else {
            return String::new();
        };
        ID_KEYS
            .iter()
            .filter_map(|key| Some(format!("{key} = {}\n", table.get(*key)?)))
            .collect()
    };
    let cards = config.card_actors();
    let mut placed = Vec::new();
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        if let Some((_, value)) = string_setting(line, &ROUTE_KEYS)
            && let Some((_, Some(id))) = cards.iter().find(|(card, _)| card == value)
        {
            out.push_str(&line.replacen(&format!("\"{value}\""), &format!("\"{id}\""), 1));
            continue;
        }
        out.push_str(line);
        if let Some(name) = table_header(line) {
            out.push_str(&id_lines(name));
            placed.push(name.to_owned());
        }
    }
    for name in ["frp", "simulation"] {
        if placed.iter().any(|p| p == name) || id_lines(name).is_empty() {
            continue;
        }
        out.push_str(&format!("\n[{name}]\n"));
        for (key, value) in table_of(name).expect("the table") {
            assert!(!value.is_table(), "[{name}] has a sub-table");
            out.push_str(&format!("{key} = {value}\n"));
        }
    }
    for (card, _) in &cards {
        assert!(placed.contains(card), "no [{card}] header in {text}");
    }
    out
}

/// `config` as it was before its first load gave it actor ids: no ids,
/// and output routing naming cards by their keys.
fn plain(config: &FlighthookConfig) -> FlighthookConfig {
    let keys: HashMap<String, String> = config
        .card_actors()
        .into_iter()
        .filter_map(|(key, id)| Some((id?.to_owned(), key)))
        .collect();
    let mut c = config.clone();
    c.frp = flighthook::FrpSection::default();
    c.simulation.actor_id = None;
    macro_rules! clear_ids {
        ($($field:ident),*) => {$(
            for section in c.$field.values_mut() {
                section.actor_id = None;
            }
        )*};
    }
    clear_ids!(
        mevo,
        r10,
        square,
        exputt,
        trackball,
        mock_monitor,
        openconnect_server,
        openconnect_client,
        pureplay,
        pureplay_accessory,
        random_club
    );
    let by_key = |slot: &mut Option<String>| {
        if let Some(key) = slot.as_deref().and_then(|id| keys.get(id)) {
            *slot = Some(key.clone());
        }
    };
    for s in c.openconnect_client.values_mut() {
        by_key(&mut s.full_monitor);
        by_key(&mut s.chipping_monitor);
        by_key(&mut s.putting_monitor);
    }
    for s in c.pureplay.values_mut() {
        by_key(&mut s.full_monitor);
        by_key(&mut s.chipping_monitor);
        by_key(&mut s.putting_monitor);
    }
    c
}

/// Asserts that `a` and `b` are the same config with the same actors:
/// equal besides their ids, and flighthook's own actors and every card
/// with the same id in both.
fn assert_same_config(a: &FlighthookConfig, b: &FlighthookConfig) {
    assert_eq!(plain(a), plain(b));
    assert_eq!(a.frp, b.frp);
    assert_eq!(a.simulation.actor_id, b.simulation.actor_id);
    assert_eq!(a.card_actors(), b.card_actors());
}

/// Asserts that the first load of `original`, which gave `first` from
/// `path` in `dir`, rewrote the file once only to give its actors ids:
/// the original is backed up, the file holds the values of `expected`
/// (`original` less what moved elsewhere) besides the ids, and a second
/// load reads the same and leaves the file as it is.
fn assert_only_ids_added(
    dir: &TempDir,
    path: &Path,
    first: &FlighthookConfig,
    original: &str,
    expected: &str,
) {
    let after_first = [backup_name(), "config.toml".to_string()];
    assert_eq!(files(&dir.0), after_first);
    assert_eq!(
        std::fs::read_to_string(dir.0.join(backup_name())).expect("backup"),
        original
    );
    let rewritten = std::fs::read_to_string(path).expect("read");
    let value = |s: &str| s.parse::<toml::Value>().expect("toml");
    assert!(
        same_value(&value(&without_ids(&rewritten)), &value(expected)),
        "{rewritten}"
    );
    let (second, error) = load(path);
    assert_eq!(error, None);
    assert_same_config(&second, first);
    assert_eq!(std::fs::read_to_string(path).expect("read"), rewritten);
    assert_eq!(files(&dir.0), after_first);
}

/// The global club lists of a file from before they moved onto the Open
/// Connect client cards, as flighthook saved them.
const GLOBAL_CLUB_LISTS: &str = "chipping_clubs = [\n    \"GW\",\n    \"SW\",\n    \"LW\",\n]\n\
                                     putting_clubs = [\"PT\"]\n";

#[test]
fn a_gspro_file_is_rewritten_with_a_backup_of_the_original() {
    let (dir, path) = config_dir(GSPRO_TOML);
    let loaded = load(&path).0;

    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    let backup = std::fs::read(dir.0.join(backup_name())).expect("backup");
    assert_eq!(backup, GSPRO_TOML.as_bytes());

    let saved = std::fs::read_to_string(&path).expect("read");
    assert_eq!(saved, toml::to_string_pretty(&loaded).expect("ser"));
    assert!(saved.contains("[openconnect_client.0]"), "{saved}");
    assert!(!saved.contains("gspro"), "{saved}");
    let back: FlighthookConfig = toml::from_str(&saved).expect("parses");
    assert_same_config(&back, &loaded);
    assert_eq!(loaded.openconnect_client["0"].name, "Bay GSPro");
    // The global club lists moved onto the client card.
    let client = &loaded.openconnect_client["0"];
    assert_eq!(client.chipping_clubs, flighthook::default_chipping_clubs());
    assert_eq!(client.putting_clubs, flighthook::default_putting_clubs());
    let top: toml::Table = saved.parse().expect("toml");
    assert!(!top.contains_key("chipping_clubs"), "{saved}");
    assert!(!top.contains_key("putting_clubs"), "{saved}");
}

#[test]
fn a_singleton_webserver_file_is_rewritten_with_a_backup() {
    let src = "[webserver]\nname = \"\"\nbind = \"0.0.0.0:5881\"\n";
    let (dir, path) = config_dir(src);
    let loaded = load(&path).0;
    assert_eq!(loaded.webserver["0"].name, "Web Server");
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    assert_eq!(
        std::fs::read_to_string(dir.0.join(backup_name())).expect("backup"),
        src
    );
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("[webserver.0]"), "{saved}");
}

#[test]
fn a_file_with_the_british_face_impact_key_loads_and_is_rewritten_with_a_backup() {
    let src = format!(
        "{GOLDEN_DEFAULT_TOML}\n[square.0]\nname = \"Omni\"\n\n\
             [square.0.dot_bottom_to_face_centre_mm]\nDR = 15.0\n7i = 18.5\n"
    );
    let (dir, path) = config_dir(&src);
    let loaded = load(&path).0;
    let overrides = &loaded.square["0"].driver.dot_bottom_to_face_center_mm;
    assert_eq!(overrides.get("DR"), Some(&15.0));
    assert_eq!(overrides.get("7i"), Some(&18.5));

    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    assert_eq!(
        std::fs::read_to_string(dir.0.join(backup_name())).expect("backup"),
        src
    );
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(
        saved.contains("[square.0.dot_bottom_to_face_center_mm]"),
        "{saved}"
    );
    assert!(!saved.contains("centre"), "{saved}");
    let back: FlighthookConfig = toml::from_str(&saved).expect("parses");
    assert_same_config(&back, &loaded);
}

#[test]
fn a_body_tracking_section_loads_as_written_and_bad_values_default_in_effect() {
    // Only the values off their defaults: read and left byte-identical.
    let src = with_ids(&format!(
        "{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmodel = \"rtmpose-s\"\ndetect_every = 5\n"
    ));
    let src = src.as_str();
    let (dir, path) = config_dir(src);
    let loaded = load(&path).0;
    assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
    assert_eq!(files(&dir.0), ["config.toml"]);
    assert_eq!(loaded.body_tracking.model, flighthook::PoseModel::RtmposeS);
    assert_eq!(loaded.body_tracking.detect_every, 5);
    assert_eq!(loaded.body_tracking.mode, flighthook::BodyTrackingMode::Off);
    // Out of range: loaded as written (the file is not rewritten), the
    // default in effect.
    let src = with_ids(&format!(
        "{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmin_score = 3.0\nsmoothing = 0.5\n"
    ));
    let src = src.as_str();
    let (dir, path) = config_dir(src);
    let cfg = SystemConfig::new(path.clone());
    assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
    assert_eq!(files(&dir.0), ["config.toml"]);
    let in_effect = cfg.body_tracking();
    assert_eq!(
        in_effect.min_score,
        flighthook::BodyTrackingSection::DEFAULT_MIN_SCORE
    );
    assert_eq!(in_effect.smoothing, 0.5, "the valid value is kept");
    // A change is read live.
    cfg.update(|c| c.body_tracking.mode = flighthook::BodyTrackingMode::Live);
    assert_eq!(cfg.body_tracking().mode, flighthook::BodyTrackingMode::Live);
}

#[test]
fn show_skeleton_reads_on_without_the_field_and_off_as_written() {
    // A config from before the field: the skeleton shows and the file
    // is left byte-identical, with no backup.
    for src in [
        GOLDEN_DEFAULT_TOML.to_owned(),
        format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmode = \"live\"\nmin_score = 0.45\n"),
    ] {
        let src = with_ids(&src);
        let (dir, path) = config_dir(&src);
        let loaded = load(&path).0;
        assert!(loaded.body_tracking.show_skeleton, "{src}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
        assert_eq!(files(&dir.0), ["config.toml"]);
    }
    // Off: loaded and left as written; a change is read live.
    let src = with_ids(&format!(
        "{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmode = \"live\"\nshow_skeleton = false\n"
    ));
    let (dir, path) = config_dir(&src);
    let cfg = SystemConfig::new(path.clone());
    assert!(!cfg.body_tracking().show_skeleton);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
    assert_eq!(files(&dir.0), ["config.toml"]);
    cfg.update(|c| c.body_tracking.show_skeleton = true);
    assert!(cfg.body_tracking().show_skeleton);
}

#[test]
fn shot_cards_read_on_without_the_key_and_off_as_written() {
    // A config without the key: the camera cards show and the file is
    // left byte-identical, with no backup.
    let replay = "[replay]\nkeep_shots = 5\npre_roll_s = 4.0\npost_roll_s = 1.5\nstart_before_impact_s = 2.0\n";
    for src in [
        GOLDEN_DEFAULT_TOML.to_owned(),
        format!("{GOLDEN_DEFAULT_TOML}\n{replay}"),
    ] {
        let src = with_ids(&src);
        let (dir, path) = config_dir(&src);
        let loaded = load(&path).0;
        assert!(loaded.replay.shot_cards, "{src}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
        assert_eq!(files(&dir.0), ["config.toml"]);
    }
    // Off: loaded and left as written; a change is read live.
    let src = with_ids(&format!(
        "{GOLDEN_DEFAULT_TOML}\n{replay}shot_cards = false\n"
    ));
    let (dir, path) = config_dir(&src);
    let cfg = SystemConfig::new(path.clone());
    assert!(!cfg.replay().shot_cards);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
    assert_eq!(files(&dir.0), ["config.toml"]);
    cfg.update(|c| c.replay.shot_cards = true);
    assert!(cfg.replay().shot_cards);
}

#[test]
fn a_body_tracking_mode_loads_byte_identical_and_the_older_switch_becomes_one() {
    use flighthook::BodyTrackingMode;
    // Without the section (Off, the default), and with a mode other
    // than the default written: left as it is.
    for (src, mode) in [
        (GOLDEN_DEFAULT_TOML.to_owned(), BodyTrackingMode::Off),
        (
            format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmode = \"live\"\n"),
            BodyTrackingMode::Live,
        ),
        (
            format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nmode = \"historical\"\n"),
            BodyTrackingMode::Historical,
        ),
    ] {
        let src = with_ids(&src);
        let (dir, path) = config_dir(&src);
        let loaded = load(&path).0;
        assert_eq!(loaded.body_tracking.mode, mode);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
        assert_eq!(files(&dir.0), ["config.toml"]);
    }
    // `enabled = true` is Historical, written as the mode, the original
    // backed up.
    let src = format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nenabled = true\n");
    let (dir, path) = config_dir(&src);
    assert_eq!(
        load(&path).0.body_tracking.mode,
        BodyTrackingMode::Historical
    );
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("mode = \"historical\""), "{saved}");
    assert!(!saved.contains("enabled"), "{saved}");
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    // `enabled = false` is the default, Off: the section goes.
    let src = format!("{GOLDEN_DEFAULT_TOML}\n[body_tracking]\nenabled = false\n");
    let (_dir, path) = config_dir(&src);
    assert_eq!(load(&path).0.body_tracking.mode, BodyTrackingMode::Off);
    assert_eq!(
        without_ids(&std::fs::read_to_string(&path).expect("read")),
        GOLDEN_DEFAULT_TOML
    );
}

/// A card that is on writes no `enabled`; one that is off writes
/// `enabled = false` in its own table, and a file holding it loads
/// untouched.
#[test]
fn enabled_is_written_only_when_off_and_such_a_file_is_left_alone() {
    let mut c: FlighthookConfig = toml::from_str(EVERY_SECTION_TOML).expect("parses");
    // Its `[player]` became a golfer; a current file holds its id.
    c.fill_golfer_ids(|| "0f1e".into());
    assert!(c.disabled_actors().is_empty(), "absent is on");
    assert!(!toml::to_string_pretty(&c).expect("ser").contains("enabled"));

    let mut off = c.clone();
    let (index, mock) = off.mock_monitor.iter_mut().next().expect("a mock");
    mock.enabled = false;
    let table = format!("[mock_monitor.{index}]");
    let out = toml::to_string_pretty(&off).expect("ser");
    assert_eq!(out.matches("enabled").count(), 1, "{out}");
    let at = out.find(&table).expect("mock table");
    let rest = &out[at..];
    let end = rest[table.len()..]
        .find("\n[")
        .map_or(rest.len(), |e| e + table.len());
    assert!(rest[..end].contains("enabled = false\n"), "{out}");

    let out = with_ids(&out);
    let (dir, path) = config_dir(&out);
    let loaded = load(&path).0;
    assert_eq!(std::fs::read_to_string(&path).expect("read"), out);
    assert_eq!(files(&dir.0), ["config.toml"]);
    assert_eq!(plain(&loaded), off);
}

/// A file with two enabled cards on one address: the first in index
/// order keeps running, the later ones load off. The file itself is left
/// as it is; the next save writes `enabled = false` for them.
#[test]
fn a_file_with_two_enabled_cards_on_one_address_loads_the_later_one_off() {
    let contents = format!(
        r#"{GOLDEN_DEFAULT_TOML}
[openconnect_client.2]
name = "Open Connect"
address = "localhost:921"
chipping_clubs = ["GW", "SW", "LW"]
putting_clubs = ["PT"]

[openconnect_client.1]
name = "GSPro"
chipping_clubs = ["GW", "SW", "LW"]
putting_clubs = ["PT"]

[openconnect_client.3]
name = "Muni Golf"
address = "127.0.0.1:9210"
chipping_clubs = ["GW", "SW", "LW"]
putting_clubs = ["PT"]
"#
    );
    let contents = with_ids(&contents);
    let (dir, path) = config_dir(&contents);
    let system = SystemConfig::new(path.clone());
    let config = system.snapshot();
    assert!(config.openconnect_client["1"].enabled, "first in order");
    assert!(!config.openconnect_client["2"].enabled);
    assert!(config.openconnect_client["3"].enabled, "another port");
    assert_eq!(config.disabled_actors(), ["openconnect_client.2"]);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), contents);
    assert_eq!(files(&dir.0), ["config.toml"], "no rewrite, no backup");

    // Any save writes the card off with everything else.
    system.update(|_| {});
    let written = std::fs::read_to_string(&path).expect("read");
    assert_eq!(written.matches("enabled = false").count(), 1, "{written}");
    let again = SystemConfig::new(path).snapshot();
    assert_same_config(&again, &config);
}

#[test]
fn a_current_file_is_left_byte_identical_with_no_backup() {
    let mut c: FlighthookConfig = toml::from_str(EVERY_SECTION_TOML).expect("parses");
    // Its `[player]` became a golfer; a current file holds its id.
    c.fill_golfer_ids(|| "0f1e".into());
    let current = toml::to_string_pretty(&c).expect("ser");
    for contents in [GOLDEN_DEFAULT_TOML, current.as_str()] {
        let contents = with_ids(contents);
        let (dir, path) = config_dir(&contents);
        let loaded = load(&path).0;
        assert_eq!(std::fs::read_to_string(&path).expect("read"), contents);
        assert_eq!(files(&dir.0), ["config.toml"]);
        let want: FlighthookConfig = toml::from_str(&contents).expect("parses");
        assert_same_config(&loaded, &want);
    }
}

#[test]
fn comments_order_and_whitespace_alone_do_not_rewrite() {
    let src = r#"# My bay.
default_units = "imperial"

[mevo.0]
address = "192.168.2.1:5100"   # the Mevo's own WiFi
name = "Mevo+"

[openconnect_client.0]
putting_clubs = [ "PT" ]
full_monitor = "mevo.0"
chipping_clubs = ["GW",
  "SW", "LW"]
address = "192.168.1.20:921"
name = "Bay GSPro"

# web
[webserver.0]
bind = "0.0.0.0:5880"
name = "Web Server"

[simulation]
temperature_c = 12
surface = "firm_green"
"#;
    let src = with_ids(src);
    let (dir, path) = config_dir(&src);
    let loaded = load(&path).0;
    assert!((loaded.simulation.temperature_c - 12.0).abs() < f64::EPSILON);
    assert_eq!(loaded.openconnect_client["0"].name, "Bay GSPro");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), src);
    assert_eq!(files(&dir.0), ["config.toml"]);
}

/// A value written out at its default, where the app leaves it out, is
/// normalized away once; the file is stable from then on.
#[test]
fn explicit_defaults_are_normalized_once() {
    let src = format!("{GOLDEN_DEFAULT_TOML}\n[mevo.0]\nname = \"Xi\"\nspin_source = \"d4\"\n");
    let (dir, path) = config_dir(&src);
    let loaded = load(&path).0;
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(!saved.contains("spin_source"), "{saved}");
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    assert_same_config(&load(&path).0, &loaded);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), saved);
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
}

#[test]
fn a_second_start_after_a_migration_changes_nothing() {
    let legacy = "[webserver]\nname = \"Web\"\nbind = \"0.0.0.0:5881\"\n\n\
                      [gspro.0]\nname = \"GSPro\"\n";
    let unknown = "[simulation]\nenabled = false\ntemperature_c = 30.0\n";
    for contents in [GSPRO_TOML, EVERY_SECTION_TOML, legacy, unknown] {
        let (dir, path) = config_dir(contents);
        let first = load(&path).0;
        let migrated = std::fs::read(&path).expect("read");
        assert_ne!(migrated, contents.as_bytes(), "{contents}");
        let after_first = files(&dir.0);
        assert_eq!(after_first, [backup_name(), "config.toml".to_string()]);

        let second = load(&path).0;
        assert_same_config(&second, &first);
        assert_eq!(std::fs::read(&path).expect("read"), migrated);
        assert_eq!(files(&dir.0), after_first);
    }
}

#[test]
fn a_golfer_height_moves_to_a_golfer_once_with_a_backup() {
    let src = "[body_tracking]\nmode = \"live\"\ngolfer_height = 182.5\n";
    let (dir, path) = config_dir(src);
    let first = load(&path).0;
    assert_eq!(first.golfers[0].height, Some(182.5));
    let id = first.golfers[0].own_id().expect("generated id").to_owned();
    assert!(id.starts_with("flighthook."), "{id}");
    assert_eq!(first.body_tracking.mode, flighthook::BodyTrackingMode::Live);
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    let backup = std::fs::read(dir.0.join(backup_name())).expect("backup");
    assert_eq!(backup, src.as_bytes());
    let migrated = std::fs::read_to_string(&path).expect("read");
    assert!(!migrated.contains("golfer_height"), "{migrated}");
    assert!(!migrated.contains("[player]"), "{migrated}");
    assert!(
        migrated.contains(&format!(
            "[[golfers]]\nidentifiers = [\"{id}\"]\nname = \"Golfer\"\nheight = 182.5"
        )),
        "{migrated}"
    );
    assert!(
        migrated.contains("[body_tracking]\nmode = \"live\""),
        "{migrated}"
    );

    // The second load reads the same and leaves the file alone.
    let second = load(&path).0;
    assert_same_config(&second, &first);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), migrated);
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    let cfg = SystemConfig::new(path);
    assert_eq!(
        settings::golfer_facts(&cfg.view().read(), Some(&id)).height,
        Some(182.5)
    );
    assert_eq!(
        settings::golfer_facts(&cfg.view().read(), None).height,
        None
    );
}

/// `[player]` moves into the golfers in one rewrite with a backup: into
/// a new golfer "Golfer" with a generated id when there is none, into
/// the first golfer's missing facts when there are golfers. The id and
/// the height reappear on the next load.
#[test]
fn the_player_section_moves_into_the_golfers_once() {
    let src = "[player]\nsex = \"female\"\nheight = 170\n";
    let (dir, path) = config_dir(src);
    let first = load(&path).0;
    assert_eq!(first.golfers.len(), 1);
    let golfer = &first.golfers[0];
    assert_eq!(golfer.name, "Golfer");
    assert_eq!(golfer.sex, Some(flighthook::Sex::Female));
    let id = golfer.own_id().expect("id").to_owned();
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    let migrated = std::fs::read_to_string(&path).expect("read");
    assert!(!migrated.contains("[player]"), "{migrated}");
    let second = load(&path).0;
    assert_eq!(second.golfers, first.golfers, "same id and height again");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), migrated);
    assert_eq!(
        settings::golfer_facts(&SystemConfig::new(path).view().read(), Some(&id)).height,
        Some(170.0)
    );

    let src = "[player]\nheight = 170\n\n[[golfers]]\nidentifiers = [\"flighthook.a\"]\n\
                   name = \"A\"\n\n[[golfers]]\nname = \"B\"\n";
    let (_dir, path) = config_dir(src);
    let c = load(&path).0;
    assert_eq!(c.golfers[0].identifiers, ["flighthook.a"]);
    assert_eq!(c.golfers[0].height, Some(170.0));
    assert_eq!(c.golfers[1].height, None);
    assert!(
        c.golfers[1].own_id().is_some(),
        "a golfer without one gets an id"
    );
}

/// A file that lists one id in two golfers loads with the first keeping
/// it.
#[test]
fn a_duplicate_golfer_id_in_a_file_stays_with_the_first_golfer() {
    let src = "[[golfers]]\nidentifiers = [\"flighthook.a\", \"pureplay.local-3\"]\nname = \"A\"\n\n\
                   [[golfers]]\nidentifiers = [\"flighthook.b\", \"pureplay.local-3\"]\nname = \"B\"\n";
    let (_dir, path) = config_dir(src);
    let c = load(&path).0;
    assert_eq!(
        c.golfers[0].identifiers,
        ["flighthook.a", "pureplay.local-3"]
    );
    assert_eq!(c.golfers[1].identifiers, ["flighthook.b"]);
}

#[test]
fn camera_rotation_and_golfer_facts_are_read_in_effect() {
    let (_dir, path) = config_dir(
        "[camera.0]\nname = \"Face\"\ncamera = \"test-pattern\"\nrotation_deg = 90\n\n\
             [camera.1]\nname = \"Side\"\ncamera = \"test-pattern\"\nrotation_deg = 45\n\n\
             [player]\nsex = \"female\"\nheight = 99\n",
    );
    let cfg = SystemConfig::new(path);
    assert!(cfg.snapshot().camera_error().is_some());
    assert_eq!(
        settings::camera_rotation(&cfg.view().read(), "camera.0"),
        90
    );
    assert_eq!(
        settings::camera_rotation(&cfg.view().read(), "camera.1"),
        0,
        "not one of the four"
    );
    assert_eq!(
        settings::camera_rotation(&cfg.view().read(), "camera.7"),
        0,
        "no such camera"
    );
    assert_eq!(settings::camera_rotation(&cfg.view().read(), "mevo.0"), 0);
    let id = cfg.snapshot().golfers[0].own_id().expect("id").to_owned();
    assert_eq!(
        settings::golfer_facts(&cfg.view().read(), Some(&id)),
        flighthook::PlayerSection {
            sex: flighthook::Sex::Female,
            height: None,
        }
    );
    // The out-of-range values stay in the file for the user to fix.
    assert_eq!(cfg.snapshot().camera["1"].rotation_deg, 45);
    assert_eq!(cfg.snapshot().golfers[0].height, Some(99.0));
}

#[test]
fn an_existing_backup_is_never_overwritten() {
    let (dir, path) = config_dir(GSPRO_TOML);
    let older = dir.0.join(backup_name());
    std::fs::write(&older, "older backup").expect("write");
    let _ = load(&path);
    let next = format!("config.pre-{}.1.toml", env!("CARGO_PKG_VERSION"));
    assert_eq!(
        files(&dir.0),
        [next.clone(), backup_name(), "config.toml".to_string()]
    );
    assert_eq!(
        std::fs::read_to_string(&older).expect("read"),
        "older backup"
    );
    assert_eq!(
        std::fs::read_to_string(dir.0.join(next)).expect("read"),
        GSPRO_TOML
    );
}

#[test]
fn a_file_that_fails_to_parse_is_left_alone() {
    for contents in [
        "[gspro.0]\nname = 7\n",
        "default_units = \"metric\"\n[simulation\n",
        "[mevo.0]\nname = \"Mevo+\"\ntrack_pct = \"lots\"\n",
    ] {
        let (dir, path) = config_dir(contents);
        let cfg = SystemConfig::new(path.clone());
        assert_eq!(plain(&cfg.snapshot()), FlighthookConfig::default());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), contents);
        assert_eq!(files(&dir.0), ["config.toml"]);
    }
}

/// The error `load` keeps for config text `contents`.
fn kept_error(contents: &str) -> flighthook::ConfigLoadError {
    let (dir, path) = config_dir(contents);
    let (config, error) = load(&path);
    assert_eq!(plain(&config), FlighthookConfig::default());
    assert_eq!(std::fs::read_to_string(&path).expect("read"), contents);
    drop(dir);
    error.expect("an error")
}

#[test]
fn a_file_that_fails_to_parse_keeps_its_located_error() {
    let bad_mode = "default_units = \"metric\"\n\n[body_tracking]\nmode = \"liv\"\n";
    let error = kept_error(bad_mode);
    assert!(error.path.ends_with("config.toml"), "{}", error.path);
    assert!(error.error.starts_with("line 4, column 8: "), "{error:?}");
    assert!(error.error.contains("liv"), "{error:?}");

    let syntax = kept_error("default_units = \"metric\"\n[simulation\n");
    assert!(syntax.error.starts_with("line 2, "), "{syntax:?}");

    let gspro = kept_error("[mevo.0]\nname = \"Mevo+\"\n\n[gspro.0]\nname = 7\n");
    assert!(gspro.error.starts_with("line 5, "), "{gspro:?}");
}

#[test]
fn a_clean_file_and_a_missing_file_keep_no_error() {
    let (_dir, path) = config_dir(GSPRO_TOML);
    assert_eq!(SystemConfig::new(path).load_error(), None);
    let (dir, _) = config_dir("");
    let missing = dir.0.join("elsewhere.toml");
    assert_eq!(SystemConfig::new(missing).load_error(), None);
}

#[test]
fn a_file_that_cannot_be_read_keeps_the_reason() {
    // A directory where the file should be: not NotFound.
    let (dir, _) = config_dir("");
    let path = dir.0.join("folder.toml");
    std::fs::create_dir(&path).expect("mkdir");
    let cfg = SystemConfig::new(path.clone());
    let error = cfg.load_error().expect("an error");
    assert!(error.path.ends_with("folder.toml"), "{}", error.path);
    assert!(!error.error.is_empty());

    // It cannot be backed up either: the save is refused and the file
    // stays.
    let refused = cfg.back_up_unusable_file().expect_err("refused");
    assert!(refused.contains("cannot be backed up"), "{refused}");
    cfg.update(|c| c.simulation.mode = SimMode::Off);
    assert!(path.is_dir());
    assert!(cfg.load_error().is_some(), "still reported");
    assert_eq!(files(&dir.0), ["config.toml", "folder.toml"]);
}

#[test]
fn the_first_save_backs_up_a_file_that_failed_to_parse_and_clears_the_error() {
    let broken = "default_units = \"metric\"\n[body_tracking]\nmode = \"liv\"\n";
    let (dir, path) = config_dir(broken);
    let cfg = SystemConfig::new(path.clone());
    assert!(cfg.load_error().is_some());
    assert_eq!(files(&dir.0), ["config.toml"], "start leaves it alone");

    cfg.update(|c| c.simulation.mode = SimMode::Off);
    assert_eq!(cfg.load_error(), None);
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
    assert_eq!(
        std::fs::read_to_string(dir.0.join(backup_name())).expect("read"),
        broken
    );
    let saved = std::fs::read_to_string(&path).expect("read");
    let back: FlighthookConfig = toml::from_str(&saved).expect("parses");
    assert_eq!(back.simulation.mode, SimMode::Off);

    // Later saves back up nothing more.
    cfg.update(|c| c.simulation.mode = SimMode::On);
    assert_eq!(files(&dir.0), [backup_name(), "config.toml".to_string()]);
}

#[test]
fn backing_up_ahead_of_a_save_clears_the_error_once() {
    let broken = "[body_tracking]\nmode = \"liv\"\n";
    let (dir, path) = config_dir(broken);
    let cfg = SystemConfig::new(path.clone());
    let backup = cfg.back_up_unusable_file().expect("backed up");
    assert_eq!(backup, Some(dir.0.join(backup_name())));
    assert_eq!(cfg.load_error(), None);
    assert_eq!(cfg.back_up_unusable_file(), Ok(None));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        broken,
        "the file itself is the save's to replace"
    );
}

/// A directory the app may not write to: the file is left as it was and
/// the migrated config is used.
#[cfg(unix)]
#[test]
fn a_failed_rewrite_keeps_the_file_and_runs_on_the_loaded_config() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, path) = config_dir(GSPRO_TOML);
    let set_mode = |mode| {
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    set_mode(0o555);
    // Root writes anyway; nothing to check then.
    let probe = dir.0.join("probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(&probe);
        set_mode(0o755);
        return;
    }

    let cfg = SystemConfig::new(path.clone());
    let c = cfg.snapshot();
    set_mode(0o755);
    assert_eq!(c.openconnect_client["0"].name, "Bay GSPro");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), GSPRO_TOML);
    assert_eq!(files(&dir.0), ["config.toml"]);
}

/// The backup is written but the file can't be replaced: the file is
/// left as it was.
#[cfg(unix)]
#[test]
fn a_failed_replace_leaves_the_file_as_it_was() {
    let (dir, path) = config_dir(GSPRO_TOML);
    // A directory in the way of the temporary file's rename target.
    let blocked = dir.0.join("blocked");
    std::fs::create_dir_all(blocked.join("config.toml")).expect("mkdir");
    let blocked_path = blocked.join("config.toml");
    let c: FlighthookConfig = toml::from_str(GSPRO_TOML).expect("parses");
    bring_up_to_date(&blocked_path, GSPRO_TOML, &c);
    assert!(blocked_path.is_dir());
    assert_eq!(
        files(&blocked),
        [backup_name(), "config.toml".to_string()],
        "the backup stays, no temporary file is left"
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), GSPRO_TOML);
}

#[test]
fn same_value_ignores_order_and_number_spelling_only() {
    let v = |s: &str| s.parse::<toml::Value>().expect("toml");
    assert!(same_value(&v("a = 1\nb = 2\n"), &v("b = 2\na = 1\n")));
    assert!(same_value(&v("t = 12\n"), &v("t = 12.0\n")));
    assert!(same_value(&v("t = nan\n"), &v("t = nan\n")));
    assert!(!same_value(&v("t = 12\n"), &v("t = 12.5\n")));
    assert!(!same_value(&v("a = [1, 2]\n"), &v("a = [2, 1]\n")));
    assert!(!same_value(&v("a = 1\n"), &v("a = 1\nb = 2\n")));
    assert!(!same_value(&v("a = \"1\"\n"), &v("a = 1\n")));
}

#[test]
fn mixed_client_sections_merge_with_openconnect_client_winning() {
    let src = r#"
[gspro.0]
name = "old 0"

[gspro.2]
name = "old 2"
address = "127.0.0.1:9210"

[openconnect_client.0]
name = "new 0"

[openconnect_client.1]
name = "new 1"
"#;
    let c: FlighthookConfig = toml::from_str(src).expect("parses");
    let mut names: Vec<_> = c
        .openconnect_client
        .iter()
        .map(|(i, s)| (i.as_str(), s.name.as_str()))
        .collect();
    names.sort_unstable();
    assert_eq!(names, [("0", "new 0"), ("1", "new 1"), ("2", "old 2")]);
    assert_eq!(
        c.openconnect_client["2"].driver.address.as_deref(),
        Some("127.0.0.1:9210")
    );
    // The collision is what `load` warns about.
    let indices: ClientSectionIndices = toml::from_str(src).expect("parses");
    assert_eq!(indices.collisions(), ["0"]);
    let out = toml::to_string_pretty(&c).expect("ser");
    assert!(!out.contains("gspro"), "{out}");
}

/// A hand-written file with every section, integers where floats go and
/// a nested table: the shapes the file format allows.
const EVERY_SECTION_TOML: &str = r#"default_units = "metric"
chipping_clubs = ["SW"]
putting_clubs = ["PT"]
shot_grouping = "by_club"

[simulation]
mode = "force"
altitude = "250m"
temperature_c = 12
surface = "green"

[player]
sex = "female"
height = 170

[webserver.0]
name = "Web"
bind = "0.0.0.0:5881"

[mevo.0]
name = "Mevo+"
address = "192.168.2.1:5100"
ball_type = 1
tee_height = "1.5in"
range = "8ft"
track_pct = 80

[r10.0]
name = "R10"

[square.0]
name = "Omni"

[square.0.dot_bottom_to_face_center_mm]
DR = 15
7i = 18.5

[mock_monitor.0]
name = "Mock"

[openconnect_server.0]
name = "Range Bridge"
bind = "0.0.0.0:921"

[openconnect_client.0]
name = "GSPro"
address = "127.0.0.1:922"
full_monitor = "openconnect_server.0"

[gspro.1]
name = "Muni Golf"
address = "127.0.0.1:9210"

[pureplay.0]
name = "Pureplay"
address = "127.0.0.1:9760"
full_monitor = "mevo.0"

[random_club.0]
name = "Clubs"
"#;

#[test]
fn every_section_of_a_file_loads_and_round_trips() {
    let c: FlighthookConfig = toml::from_str(EVERY_SECTION_TOML).expect("parses");
    assert_eq!(c.default_units, flighthook::UnitSystem::Metric);
    assert_eq!(c.shot_grouping, flighthook::ShotGrouping::ByClub);
    assert_eq!(c.simulation.mode, SimMode::Force);
    assert!((c.simulation.temperature_c - 12.0).abs() < f64::EPSILON);
    assert_eq!(c.webserver["0"].bind, "0.0.0.0:5881");
    assert_eq!(c.mevo["0"].driver.ball_type, Some(1));
    assert_eq!(c.mevo["0"].driver.track_pct, Some(80.0));
    assert_eq!(c.r10["0"].name, "R10");
    assert_eq!(c.square["0"].driver.dot_bottom_to_face_center_mm.len(), 2);
    assert_eq!(c.mock_monitor["0"].name, "Mock");
    assert_eq!(
        c.openconnect_server["0"].driver.bind.as_deref(),
        Some("0.0.0.0:921")
    );
    assert_eq!(c.openconnect_client["0"].name, "GSPro");
    assert_eq!(c.openconnect_client["1"].name, "Muni Golf");
    assert_eq!(c.random_club["0"].name, "Clubs");
    assert_eq!(
        c.pureplay["0"].driver.address.as_deref(),
        Some("127.0.0.1:9760")
    );
    assert_eq!(c.pureplay["0"].full_monitor.as_deref(), Some("mevo.0"));
    assert_eq!(c.golfers[0].sex, Some(flighthook::Sex::Female));
    assert_eq!(c.golfers[0].height, Some(170.0));

    let out = toml::to_string_pretty(&c).expect("ser");
    let back: FlighthookConfig = toml::from_str(&out).expect("parses");
    assert_eq!(back, c);
}

#[test]
fn a_file_with_two_web_servers_keeps_both() {
    let src = "[webserver.0]\nname = \"Web Server\"\nbind = \"0.0.0.0:5880\"\n\n\
                   [webserver.1]\nname = \"LAN\"\nbind = \"0.0.0.0:5881\"\n";
    let c: FlighthookConfig = toml::from_str(src).expect("parses");
    assert_eq!(c.webserver.len(), 2);
    assert_eq!(c.webserver["1"].bind, "0.0.0.0:5881");
    let out = toml::to_string_pretty(&c).expect("ser");
    let back: FlighthookConfig = toml::from_str(&out).expect("parses");
    assert_eq!(back, c);
}

#[test]
fn canonical_id_spells_gspro_as_openconnect_client() {
    assert_eq!(canonical_id("gspro.3"), "openconnect_client.3");
    assert_eq!(canonical_id("openconnect_client.3"), "openconnect_client.3");
    assert_eq!(canonical_id("mevo.0"), "mevo.0");
    assert_eq!(canonical_id("gspro"), "gspro");
}
