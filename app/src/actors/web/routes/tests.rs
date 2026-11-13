//! Tests of the REST handlers: status, settings saves and validation.

use super::*;
use flighthook::MevoSection;

fn config() -> FlighthookConfig {
    serde_json::from_str("{}").expect("empty config")
}

#[tokio::test]
async fn the_page_icons_are_served_with_their_types() {
    for (response, content_type, magic) in [
        (get_favicon_ico().await, "image/x-icon", &[0, 0, 1, 0][..]),
        (get_favicon_png().await, "image/png", &b"\x89PNG"[..]),
        (get_apple_touch_icon().await, "image/png", &b"\x89PNG"[..]),
    ] {
        assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.starts_with(magic), "{content_type}");
    }
}

fn mevo_section(name: &str) -> MevoSection {
    let mut s: MevoSection = serde_json::from_str("{}").expect("empty section");
    s.name = name.to_string();
    s
}

#[test]
fn scope_upserts_only_the_named_section() {
    let mut c = config();
    c.mevo.insert("0".into(), mevo_section("Mevo WiFi"));
    c.openconnect_client.insert(
        "0".into(),
        serde_json::from_str("{}").expect("empty section"),
    );

    let action = scoped_action("mevo.0", &c).expect("known scope");
    match action {
        ConfigAction::UpsertMevo { index, section } => {
            assert_eq!(index, "0");
            assert_eq!(section.name, "Mevo WiFi");
        }
        other => panic!("expected UpsertMevo, got {other:?}"),
    }
}

#[test]
fn a_camera_scope_upserts_or_removes_its_section() {
    let mut c = config();
    c.camera.insert(
        "2".into(),
        flighthook::CameraSection {
            camera: "test-pattern".into(),
            ..flighthook::CameraSection::default()
        },
    );
    match scoped_action("camera.2", &c).expect("known scope") {
        ConfigAction::UpsertCamera { index, section } => {
            assert_eq!(index, "2");
            assert_eq!(section.camera, "test-pattern");
        }
        other => panic!("expected UpsertCamera, got {other:?}"),
    }
    match scoped_action("camera.3", &c).expect("known scope") {
        ConfigAction::Remove { id } => assert_eq!(id, "camera.3"),
        other => panic!("expected Remove, got {other:?}"),
    }
}

#[test]
fn a_camera_without_a_camera_or_with_a_bad_mode_or_bad_replay_values_is_invalid() {
    let camera = |camera: &str, mode: &str| ConfigAction::UpsertCamera {
        index: "0".into(),
        section: flighthook::CameraSection {
            camera: camera.into(),
            mode: mode.into(),
            ..flighthook::CameraSection::default()
        },
    };
    assert_eq!(invalid_setting(&camera("test-pattern", "auto")), None);
    assert_eq!(invalid_setting(&camera("usb-1", "1280x720@120")), None);
    assert_eq!(
        invalid_setting(&camera(" ", "auto")).as_deref(),
        Some("camera.0: no camera picked")
    );
    let bad_mode = invalid_setting(&camera("usb-1", "fast")).expect("rejected");
    assert!(
        bad_mode.starts_with("camera.0: invalid argument: video mode `fast`"),
        "{bad_mode}"
    );

    let mut c = config();
    c.replay.keep_shots = 0;
    for action in [
        ConfigAction::ReplaceGlobal {
            config: Box::new(c.clone()),
        },
        ConfigAction::ReplaceAll {
            config: Box::new(c.clone()),
        },
    ] {
        let error = invalid_setting(&action).expect("rejected");
        assert!(error.starts_with("replay: keep_shots 0"), "{error}");
    }
    c.replay.keep_shots = 5;
    c.replay.pre_roll_s = 2.0;
    assert_eq!(
        invalid_setting(&ConfigAction::ReplaceGlobal {
            config: Box::new(c.clone())
        }),
        None
    );
    // Body tracking values out of range are refused the same way.
    for (field, set) in [
        (
            "min_score",
            (|b: &mut flighthook::BodyTrackingSection| b.min_score = 2.0)
                as fn(&mut flighthook::BodyTrackingSection),
        ),
        ("smoothing", |b| b.smoothing = 50.0),
        ("detect_every", |b| b.detect_every = 0),
    ] {
        let mut bad = c.clone();
        set(&mut bad.body_tracking);
        for action in [
            ConfigAction::ReplaceGlobal {
                config: Box::new(bad.clone()),
            },
            ConfigAction::ReplaceAll {
                config: Box::new(bad.clone()),
            },
        ] {
            let error = invalid_setting(&action).expect("rejected");
            assert!(
                error.starts_with(&format!("body_tracking: {field}")),
                "{error}"
            );
        }
    }
    c.body_tracking.model = flighthook::PoseModel::RtmposeS;
    c.body_tracking.detect_every = 60;
    assert_eq!(
        invalid_setting(&ConfigAction::ReplaceGlobal {
            config: Box::new(c)
        }),
        None
    );
}

#[test]
fn scope_naming_an_absent_section_is_a_removal() {
    let action = scoped_action("mevo.0", &config()).expect("known scope");
    match action {
        ConfigAction::Remove { id } => assert_eq!(id, "mevo.0"),
        other => panic!("expected Remove, got {other:?}"),
    }
}

#[test]
fn gspro_scope_names_the_openconnect_client_section() {
    let mut c = config();
    c.openconnect_client.insert(
        "1".into(),
        serde_json::from_str(r#"{"name":"Muni Golf"}"#).expect("section"),
    );
    match scoped_action("gspro.1", &c).expect("known scope") {
        ConfigAction::UpsertOpenConnectClient { index, section } => {
            assert_eq!(index, "1");
            assert_eq!(section.name, "Muni Golf");
        }
        other => panic!("expected UpsertOpenConnectClient, got {other:?}"),
    }
    match scoped_action("gspro.2", &c).expect("known scope") {
        ConfigAction::Remove { id } => assert_eq!(id, "openconnect_client.2"),
        other => panic!("expected Remove, got {other:?}"),
    }
}

#[test]
fn a_pureplay_scope_upserts_or_removes_its_section() {
    let mut c = config();
    c.pureplay.insert(
        "0".into(),
        serde_json::from_str(r#"{"name":"Pureplay","address":"127.0.0.1:9760"}"#).expect("section"),
    );
    match scoped_action("pureplay.0", &c).expect("known scope") {
        ConfigAction::UpsertPureplay { index, section } => {
            assert_eq!(index, "0");
            assert_eq!(section.address.as_deref(), Some("127.0.0.1:9760"));
        }
        other => panic!("expected UpsertPureplay, got {other:?}"),
    }
    match scoped_action("pureplay.1", &c).expect("known scope") {
        ConfigAction::Remove { id } => assert_eq!(id, "pureplay.1"),
        other => panic!("expected Remove, got {other:?}"),
    }
}

fn with_webservers(indices: &[&str]) -> FlighthookConfig {
    let mut c = config();
    for i in indices {
        c.webserver.insert(
            (*i).to_string(),
            flighthook::WebserverSection {
                name: "Web Server".into(),
                bind: format!("0.0.0.0:588{i}"),
            },
        );
    }
    c
}

#[test]
fn no_save_removes_a_web_server_or_adds_a_second() {
    let one = with_webservers(&["0"]);
    let remove = |id: &str| ConfigAction::Remove { id: id.into() };
    let upsert = |index: &str| ConfigAction::UpsertWebserver {
        index: index.into(),
        section: with_webservers(&[index]).webserver[index].clone(),
    };
    let replace = |c: FlighthookConfig| ConfigAction::ReplaceAll {
        config: Box::new(c),
    };
    let no_remove = Some("the web server can't be removed");
    let only_one = Some("flighthook runs one web server; edit the existing one");

    assert_eq!(webserver_violation(&one, &remove("webserver.0")), no_remove);
    assert_eq!(webserver_violation(&one, &remove("webserver.7")), no_remove);
    assert_eq!(webserver_violation(&one, &remove("mevo.0")), None);
    assert_eq!(webserver_violation(&one, &upsert("0")), None);
    assert_eq!(webserver_violation(&one, &upsert("1")), only_one);
    assert_eq!(webserver_violation(&config(), &upsert("0")), None);
    assert_eq!(
        webserver_violation(&one, &replace(with_webservers(&["0"]))),
        None
    );
    assert_eq!(webserver_violation(&one, &replace(config())), no_remove);
    assert_eq!(
        webserver_violation(&one, &replace(with_webservers(&["1"]))),
        no_remove
    );
    assert_eq!(
        webserver_violation(&one, &replace(with_webservers(&["0", "1"]))),
        only_one
    );
    assert_eq!(
        webserver_violation(&config(), &replace(with_webservers(&["0"]))),
        None
    );

    // A config that already holds two keeps both, and each stays
    // editable, but gets no third.
    let two = with_webservers(&["0", "1"]);
    assert_eq!(webserver_violation(&two, &upsert("1")), None);
    assert_eq!(webserver_violation(&two, &upsert("2")), only_one);
    assert_eq!(
        webserver_violation(&two, &replace(with_webservers(&["0", "1"]))),
        None
    );
    assert_eq!(
        webserver_violation(&two, &replace(with_webservers(&["0"]))),
        no_remove
    );
    assert_eq!(
        webserver_violation(&two, &replace(with_webservers(&["0", "1", "2"]))),
        only_one
    );
}

#[test]
fn malformed_and_unknown_scopes_are_rejected() {
    assert!(scoped_action("mevo", &config()).is_none());
    assert!(scoped_action("nosuchtype.0", &config()).is_none());
}
