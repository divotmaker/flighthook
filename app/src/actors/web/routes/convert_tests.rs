//! Tests of shot conversion (`POST /api/shots/convert`).

use super::*;
use flighthook::{DerivedFlight, Distance, Launch, ShotData, Velocity};

fn shot() -> ShotData {
    let mut derived = DerivedFlight::default();
    derived.carry_distance = Some(Distance::Meters(150.9));
    derived.carry_offline = Some(Distance::Meters(-5.5));
    derived.max_height = Some(Distance::Meters(26.0));
    derived.flight_time = Some(6.1);
    derived.descent_angle = Some(50.2);
    derived.roll_distance = Some(Distance::Meters(-1.2));
    derived.total_distance = Some(Distance::Meters(149.7));
    ShotData {
        card: Some("square.0".into()),
        shot_number: 3,
        launch: Some(Launch {
            launch_speed: Some(Velocity::MetersPerSecond(53.6)),
            launch_elevation: Some(17.0),
            launch_azimuth: Some(-1.2),
            backspin_rpm: Some(6500),
            sidespin_rpm: Some(-300),
            ..Launch::default()
        }),
        club: None,
        impact: None,
        flights: vec![flighthook::Flight {
            from: "flight-model".into(),
            flight: derived.modeled(),
        }],
        ..ShotData::default()
    }
}

async fn convert(shot: ShotData, units: &str) -> ShotData {
    // Through JSON both ways, as a WS consumer would send it.
    let body: ShotData =
        serde_json::from_str(&serde_json::to_string(&shot).expect("ser")).expect("de");
    let Json(out) = post_convert_shot(
        Query(ConvertQuery {
            units: units.into(),
        }),
        Json(body),
    )
    .await
    .expect("converts");
    serde_json::from_str(&serde_json::to_string(&out).expect("ser")).expect("de")
}

#[tokio::test]
async fn convert_drops_camera_clips() {
    let mut shot = shot();
    shot.video = vec![ShotVideo {
        camera: "camera.0".into(),
        position: flighthook::CameraPosition::GolferFront,
        duration_ms: 1,
        fps: 30.0,
        trigger_offset_ms: 0,
        impact_offset_ms: 0,
        complete: true,
        dropped: 0,
        pose: false,
        swing: false,
        rotation: 0,
        analyzed_with: None,
    }];
    for units in ["imperial", "metric"] {
        assert!(
            convert(shot.clone(), units).await.video.is_empty(),
            "{units}"
        );
    }
}

#[tokio::test]
async fn convert_round_trips_the_flight_models_values() {
    let original = shot();
    let imperial = convert(original.clone(), "imperial").await;
    let d = imperial.flights[0].flight.clone();
    assert!(matches!(d.carry_distance, Some(Distance::Yards(_))));
    assert!(matches!(d.carry_offline, Some(Distance::Yards(_))));
    assert!(matches!(d.max_height, Some(Distance::Feet(_))));
    assert!(matches!(d.roll_distance, Some(Distance::Yards(v)) if v < 0.0));
    assert_eq!(d.flight_time, Some(6.1));
    assert_eq!(d.descent_angle, Some(50.2));

    let back = convert(imperial, "metric").await.flights[0].flight.clone();
    let want = original.flights[0].flight.clone();
    let close = |a: Option<Distance>, b: Option<Distance>| {
        (a.expect("a").as_meters() - b.expect("b").as_meters()).abs() < 1e-9
    };
    assert!(close(back.carry_distance, want.carry_distance));
    assert!(close(back.carry_offline, want.carry_offline));
    assert!(close(back.max_height, want.max_height));
    assert!(close(back.roll_distance, want.roll_distance));
    assert!(close(back.total_distance, want.total_distance));
    assert_eq!(back.total_offline, None);
}
