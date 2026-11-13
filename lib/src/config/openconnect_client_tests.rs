//! Tests of the config: openconnect client.

use super::*;

#[test]
fn gspro_sections_read_as_openconnect_client() {
    let c: FlighthookConfig = serde_json::from_str(
        r#"{"gspro":{"1":{"name":"Bay","address":"10.0.0.5:921","full_monitor":"mevo.0"}}}"#,
    )
    .expect("parse");
    let s = &c.openconnect_client["1"];
    assert_eq!(s.name, "Bay");
    assert_eq!(s.address.as_deref(), Some("10.0.0.5:921"));
    assert_eq!(s.full_monitor.as_deref(), Some("mevo.0"));
    assert!(c.has_user_actors());
}

#[test]
fn mixed_sections_merge_and_openconnect_client_wins_a_collision() {
    let c: FlighthookConfig = serde_json::from_str(
        r#"{
                "gspro": {"0": {"name": "old 0"}, "2": {"name": "old 2"}},
                "openconnect_client": {"0": {"name": "new 0"}, "1": {"name": "new 1"}}
            }"#,
    )
    .expect("parse");
    let mut names: Vec<_> = c
        .openconnect_client
        .iter()
        .map(|(i, s)| (i.as_str(), s.name.as_str()))
        .collect();
    names.sort_unstable();
    assert_eq!(names, [("0", "new 0"), ("1", "new 1"), ("2", "old 2")]);
}

#[test]
fn output_only_carries_openconnect_client() {
    let c: FlighthookConfig =
        serde_json::from_str(r#"{"gspro":{"0":{"name":"GSPro"}}}"#).expect("parse");
    let json = serde_json::to_value(&c).expect("serialize");
    assert!(json.get("gspro").is_none(), "{json}");
    assert_eq!(json["openconnect_client"]["0"]["name"], "GSPro");
}

/// A config with every field away from its default and every section
/// map holding an entry. Built field by field without `..`, so a field
/// added to [`FlighthookConfig`] fails to compile here until it is set.
fn every_field() -> FlighthookConfig {
    fn one<T>(section: T) -> std::collections::HashMap<String, T> {
        std::collections::HashMap::from([("3".to_string(), section)])
    }
    FlighthookConfig {
        default_units: UnitSystem::Metric,
        frp: FrpSection {
            actor_id: Some("1111111111111111".into()),
            auto_chip_actor_id: Some("2222222222222222".into()),
        },
        shot_grouping: ShotGrouping::ByClub,
        simulation: SimulationSection {
            actor_id: None,
            mode: SimMode::Force,
            altitude: Distance::Meters(250.0),
            temperature_c: 12.5,
            surface: SimSurface::FirmGreen,
            auto_chip: true,
            tournament: true,
        },
        replay: ReplaySection {
            keep_shots: 7,
            pre_roll_s: 2.5,
            post_roll_s: 0.5,
            start_before_impact_s: 1.5,
            clip_dir: Some("/srv/clips".into()),
            shot_cards: false,
        },
        body_tracking: BodyTrackingSection {
            mode: BodyTrackingMode::Live,
            model: PoseModel::RtmposeS,
            min_score: 0.45,
            smoothing: 2.0,
            detect_every: 5,
            show_skeleton: false,
            legacy_golfer_height: None,
        },
        selected_golfer: Some("flighthook.a1".into()),
        golfers: vec![GolferRecord {
            identifiers: vec!["flighthook.a1".into(), "pureplay.local-3".into()],
            name: "Eric".into(),
            handed: Some(Handedness::Left),
            bag: vec![Club::Driver, Club::Iron7],
            sex: Some(Sex::Female),
            height: Some(180.0),
        }],
        webserver: one(WebserverSection {
            name: "Web".into(),
            bind: "0.0.0.0:5881".into(),
        }),
        mevo: one(MevoSection::default()),
        r10: one(R10Section::default()),
        square: one(SquareSection::default()),
        exputt: one(ExPuttSection {
            actor_id: None,
            name: "Putting".into(),
            enabled: true,
            impact_lead_s: None,
            driver: ExPuttConfig {
                address: Some("192.168.1.50".into()),
            },
        }),
        trackball: one(TrackballSection {
            actor_id: None,
            name: "Arcade".into(),
            enabled: false,
            driver: TrackballConfig {
                device: Some("usb-Example_Trackball-event-mouse".into()),
                club: thumbswing::Club::Driver,
                hand: thumbswing::Hand::default(),
                config: thumbswing::Config {
                    cpi: 800.0,
                    report_limit: 2047,
                    grab: false,
                    thresholds: thumbswing::Thresholds {
                        v_start: 0.08,
                        ..thumbswing::Thresholds::default()
                    },
                },
                tuning: thumbswing::Tuning {
                    curve_sense: thumbswing::CurveSense::Physical,
                    full_power_speed: 40.0,
                    ..thumbswing::Tuning::default()
                },
            },
        }),
        mock_monitor: one(MockMonitorSection {
            actor_id: None,
            name: "Mock".into(),
            enabled: true,
            impact_lead_s: None,
            driver: Default::default(),
        }),
        openconnect_server: one(OpenConnectServerSection {
            actor_id: None,
            name: "Range Bridge".into(),
            enabled: true,
            bind: Some("0.0.0.0:921".into()),
            impact_lead_s: None,
        }),
        openconnect_client: one(OpenConnectClientSection {
            name: "Muni Golf".into(),
            address: Some("127.0.0.1:9210".into()),
            full_monitor: Some("openconnect_server.3".into()),
            ..OpenConnectClientSection::default()
        }),
        pureplay: one(PureplaySection {
            name: "Pureplay".into(),
            address: Some("192.168.1.20:9760".into()),
            putting_monitor: Some("exputt.3".into()),
            ..PureplaySection::default()
        }),
        pureplay_accessory: one(PureplayAccessorySection {
            actor_id: None,
            name: "Pureplay bay".into(),
            enabled: false,
            address: Some("192.168.1.20:9770".into()),
            shots: PureplayShots::FromPureplay,
            impact_lead_s: Some(3.0),
        }),
        random_club: one(RandomClubSection {
            actor_id: None,
            name: "Clubs".into(),
            enabled: true,
        }),
        camera: one(CameraSection {
            name: "Face".into(),
            enabled: true,
            camera: "usb-0000:00:14.0-1/HD Pro Webcam C920".into(),
            camera_name: Some("HD Pro Webcam C920".into()),
            position: CameraPosition::Overhead,
            mode: "1920x1080@30".into(),
            latency_s: Some(0.4),
            rotation_deg: 270,
            yaw_deg: Some(-7.5),
            pitch_deg: Some(12.0),
        }),
    }
}

#[test]
fn every_field_survives_a_round_trip() {
    let c = every_field();
    assert_ne!(c, FlighthookConfig::default());
    let json = serde_json::to_string(&c).expect("serialize");
    let back: FlighthookConfig = serde_json::from_str(&json).expect("parse");
    assert_eq!(back, c);
}

#[test]
fn gspro_sections_join_every_other_field() {
    let mut json = serde_json::to_value(every_field()).expect("serialize");
    json["gspro"] = serde_json::json!({"4": {"name": "Bay", "address": "10.0.0.5:921"}});
    let back: FlighthookConfig = serde_json::from_value(json).expect("parse");
    let mut want = every_field();
    want.openconnect_client.insert(
        "4".into(),
        OpenConnectClientSection {
            actor_id: None,
            chipping_clubs: crate::default_chipping_clubs(),
            putting_clubs: crate::default_putting_clubs(),
            name: "Bay".into(),
            enabled: true,
            address: Some("10.0.0.5:921".into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
        },
    );
    assert_eq!(back, want);
}

#[test]
fn upsert_action_accepts_the_gspro_spelling() {
    let a: crate::ConfigAction =
        serde_json::from_str(r#"{"type":"upsert_gs_pro","index":"0","section":{"name":"GSPro"}}"#)
            .expect("parse");
    assert!(matches!(
        a,
        crate::ConfigAction::UpsertOpenConnectClient { ref index, .. } if index == "0"
    ));
    let json = serde_json::to_value(&a).expect("serialize");
    assert_eq!(json["type"], "upsert_open_connect_client");
}
