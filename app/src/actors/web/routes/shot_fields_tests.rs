//! Tests of the shot fields the REST API serves.

use super::super::tests::{Ev, feed, test_state};
use super::*;
use flighthook::{Basis, ClubData, Distance, FlightResult, Launch, ShotKey, Velocity};

async fn shots_json(state: &Arc<WebState>, units: Option<&str>) -> serde_json::Value {
    let Json(shots) = get_shots(
        State(Arc::clone(state)),
        Query(ShotsQuery {
            limit: 50,
            units: units.map(str::to_string),
        }),
    )
    .await;
    serde_json::to_value(shots).expect("ser")
}

/// A finished shot's measured carry offline and descent angle and its
/// computed metrics reach `/api/shots`, converted by `?units=`.
#[tokio::test]
async fn api_shots_carry_offline_and_metrics() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let key = ShotKey {
        shot_id: "lat".into(),
        shot_number: 4,
    };
    let ball = Launch {
        launch_speed: Some(Velocity::MetersPerSecond(40.0)),
        launch_azimuth: Some(2.0),
        launch_elevation: Some(20.0),
        backspin_rpm: Some(6000),
        sidespin_rpm: Some(-800),
        ..Launch::default()
    };
    let flight = FlightResult {
        carry_distance: Some(Distance::Meters(100.0)),
        carry_offline: Some(Distance::Meters(-1.5)),
        descent_angle: Some(44.2),
        ..FlightResult::new(Basis::Modeled)
    };
    let club = ClubData {
        path: Some(3.0),
        face_angle: Some(1.0),
        dynamic_loft: Some(25.0),
        attack_angle: Some(-4.0),
        ..ClubData::default()
    };
    feed(
        &state,
        "mevo.0",
        vec![
            Ev::trigger(key.clone()),
            Ev::frp(flightrelay::ShotLaunch {
                key: key.clone(),
                launch: ball,
            }),
            Ev::frp(flightrelay::ShotFlight {
                key: key.clone(),
                flight,
            }),
            Ev::frp(flightrelay::ShotClub {
                key: key.clone(),
                club,
            }),
            Ev::Finished(key, None),
        ],
    )
    .await;

    let json = shots_json(&state, None).await;
    let shot = &json[0];
    let own = &shot["flights"][0];
    assert_eq!(own["from"], shot["from"], "{shot}");
    assert_eq!(own["flight"]["carry_offline"], "-1.5m", "{shot}");
    assert_eq!(own["flight"]["descent_angle"], 44.2, "{shot}");
    assert_eq!(own["flight"]["basis"], "modeled", "{shot}");
    assert!(shot.get("ball").is_none(), "{shot}");
    let m = &shot["metrics"];
    assert_eq!(m["total_spin_rpm"], 6053, "{m}");
    let axis = m["spin_axis"].as_f64().expect("axis");
    assert!((axis + 7.595).abs() < 1e-3, "{axis}");
    assert_eq!(m["face_to_path"], -2.0, "{m}");
    assert_eq!(m["spin_loft"], 29.0, "{m}");
    // Started right (2 deg = +3.49 m at 100 m), landed 1.5 m left: curved
    // left by about 4.99 m.
    let curve: Distance = serde_json::from_value(m["curve"].clone()).expect("curve");
    assert!(matches!(curve, Distance::Meters(_)), "{curve:?}");
    let want = -1.5 - 100.0 * 2.0_f64.to_radians().tan();
    assert!((curve.as_meters() - want).abs() < 1e-9, "{curve:?}");

    let json = shots_json(&state, Some("imperial")).await;
    let shot = &json[0];
    let offline: Distance =
        serde_json::from_value(shot["flights"][0]["flight"]["carry_offline"].clone())
            .expect("offline");
    assert!(matches!(offline, Distance::Yards(v) if (v + 1.640_42).abs() < 1e-4));
    let curve: Distance = serde_json::from_value(shot["metrics"]["curve"].clone()).expect("curve");
    assert!(matches!(curve, Distance::Yards(_)), "{curve:?}");
    assert!((curve.as_meters() - want).abs() < 1e-9, "{curve:?}");
    assert_eq!(shot["metrics"]["face_to_path"], -2.0);
}

/// No inputs, no metrics: a shot with only launch data has no `metrics`.
#[tokio::test]
async fn api_shots_omit_metrics_without_inputs() {
    let (state, _writer, _dir) = test_state();
    let state = Arc::new(state);
    let key = ShotKey {
        shot_id: "bare".into(),
        shot_number: 1,
    };
    feed(
        &state,
        "mevo.0",
        vec![
            Ev::trigger(key.clone()),
            Ev::frp(flightrelay::ShotLaunch {
                key: key.clone(),
                launch: Launch {
                    launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                    backspin_rpm: Some(6000),
                    ..Launch::default()
                },
            }),
            Ev::Finished(key, None),
        ],
    )
    .await;
    let json = shots_json(&state, None).await;
    assert!(json[0].get("metrics").is_none(), "{json}");
    assert!(json[0]["ball"].get("carry_offline").is_none(), "{json}");
}
