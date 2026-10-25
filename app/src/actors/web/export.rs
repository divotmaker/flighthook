//! Shot export: one JSONL line or CSV row per stored shot.
//!
//! The JSONL line is a [`ShotExport`]: the `/api/shots` element plus the
//! settings in effect and the raw device frames. CSV carries the decoded
//! values in fixed-unit columns and no raw frames. Neither ever contains the
//! FRP device ID, actor names, telemetry or configured addresses; identity
//! strings that could appear in text payloads or device labels are scrubbed.

use chrono::SecondsFormat;

use super::StoredShot;
use crate::raw_capture::scrub_text;
use flighthook::{
    Distance, EXPORT_SCHEMA, ExportDevice, ExportSettings, FlightScopeEnvironment, ShotExport,
    UnitSystem, Velocity,
};

/// Build the JSONL record for one shot. `units` converts `data` (native
/// units when `None`); `include_raw = false` leaves the raw frames out.
pub fn build_export_line(
    shot: &StoredShot,
    exported_at: &str,
    units: Option<UnitSystem>,
    include_raw: bool,
) -> ShotExport {
    let ctx = &shot.context;
    let scrub = |v: &Option<String>| v.as_deref().map(|s| scrub_text(s, &ctx.scrub));
    let device = ExportDevice {
        kind: ctx.device.kind.clone(),
        manufacturer: scrub(&ctx.device.manufacturer),
        model: scrub(&ctx.device.model),
        firmware: scrub(&ctx.device.firmware),
    };
    let raw = if include_raw {
        shot.raw.clone().map(|mut raw| {
            for frame in &mut raw.frames {
                if let Some(text) = frame.text.as_deref() {
                    frame.text = Some(scrub_text(text, &ctx.scrub));
                }
            }
            raw
        })
    } else {
        None
    };
    ShotExport {
        schema: EXPORT_SCHEMA,
        flighthook_version: env!("CARGO_PKG_VERSION").to_string(),
        exported_at: exported_at.to_string(),
        time: shot
            .received_at
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        actor: shot.data.actor.clone(),
        shot_id: shot.key.shot_id.clone(),
        shot_number: shot.key.shot_number,
        device,
        club_selected: ctx.club_selected,
        settings: ExportSettings {
            device: ctx.device_section.clone(),
            simulation: ctx.simulation.clone(),
            detection_mode: ctx.detection_mode,
            handed: ctx.handed,
        },
        data: match units {
            Some(u) => shot.data.to_unit_system(u),
            None => shot.data.clone(),
        },
        raw,
    }
}

/// One JSONL line, newline-terminated.
pub fn jsonl_line(
    shot: &StoredShot,
    exported_at: &str,
    units: Option<UnitSystem>,
    include_raw: bool,
) -> String {
    let line = build_export_line(shot, exported_at, units, include_raw);
    match serde_json::to_string(&line) {
        Ok(mut s) => {
            s.push('\n');
            s
        }
        Err(e) => {
            tracing::warn!(
                "shot export: could not serialise shot {}: {e}",
                line.shot_id
            );
            String::new()
        }
    }
}

/// Unit suffixes for the CSV columns.
struct Units {
    speed: &'static str,
    dist: &'static str,
    height: &'static str,
    small: &'static str,
}

fn units(system: UnitSystem) -> Units {
    match system {
        UnitSystem::Imperial => Units {
            speed: "mph",
            dist: "yd",
            height: "ft",
            small: "in",
        },
        UnitSystem::Metric => Units {
            speed: "mps",
            dist: "m",
            height: "m",
            small: "m",
        },
    }
}

/// CSV header row, newline-terminated.
pub fn csv_header(system: UnitSystem) -> String {
    let u = units(system);
    let cols = [
        "time".to_string(),
        "actor".into(),
        "device_kind".into(),
        "manufacturer".into(),
        "model".into(),
        "firmware".into(),
        "shot_number".into(),
        "shot_id".into(),
        "club_selected".into(),
        "handed".into(),
        "detection_mode".into(),
        "environment".into(),
        format!("ball_speed_{}", u.speed),
        "launch_elevation_deg".into(),
        "launch_azimuth_deg".into(),
        "backspin_rpm".into(),
        "sidespin_rpm".into(),
        format!("carry_{}", u.dist),
        format!("total_{}", u.dist),
        format!("roll_{}", u.dist),
        format!("max_height_{}", u.height),
        "flight_time_s".into(),
        format!("sim_carry_{}", u.dist),
        format!("sim_carry_offline_{}", u.dist),
        format!("sim_max_height_{}", u.height),
        "sim_flight_time_s".into(),
        "sim_descent_angle_deg".into(),
        format!("sim_roll_{}", u.dist),
        format!("sim_roll_offline_{}", u.dist),
        format!("sim_total_{}", u.dist),
        format!("sim_total_offline_{}", u.dist),
        format!("club_speed_{}", u.speed),
        format!("club_speed_post_{}", u.speed),
        "path_deg".into(),
        "attack_angle_deg".into(),
        "face_angle_deg".into(),
        "dynamic_loft_deg".into(),
        "smash_factor".into(),
        "swing_plane_horizontal_deg".into(),
        "swing_plane_vertical_deg".into(),
        format!("club_offset_{}", u.small),
        format!("club_height_{}", u.small),
        "impact_lateral_mm".into(),
        "impact_vertical_mm".into(),
    ];
    let cells: Vec<Option<String>> = cols.into_iter().map(Some).collect();
    csv_row(&cells)
}

/// A number rounded to 4 decimal places.
fn num(v: f64) -> String {
    let r = (v * 1e4).round() / 1e4;
    if r == 0.0 { "0".into() } else { r.to_string() }
}

/// Serialize a unit enum to its wire label (`"rh"`, `"full"`).
fn label<T: serde::Serialize>(v: Option<&T>) -> Option<String> {
    v.and_then(|v| match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => Some(s),
        _ => None,
    })
}

/// The shot's FlightScope environment: as configured, else the default.
fn environment(shot: &StoredShot) -> Option<String> {
    match shot.context.device_section.get("environment") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        _ if shot.context.device.kind == "flightscope" => {
            Some(FlightScopeEnvironment::default().key().to_string())
        }
        _ => None,
    }
}

/// One CSV row for a shot, in `system` units, newline-terminated.
pub fn to_csv_row(shot: &StoredShot, system: UnitSystem) -> String {
    let line = build_export_line(shot, "", Some(system), false);
    let imperial = system == UnitSystem::Imperial;
    let dist = |d: Option<Distance>| {
        d.map(|d| {
            num(if imperial {
                d.as_yards()
            } else {
                d.as_meters()
            })
        })
    };
    let height =
        |d: Option<Distance>| d.map(|d| num(if imperial { d.as_feet() } else { d.as_meters() }));
    let small = |d: Option<Distance>| {
        d.map(|d| {
            num(if imperial {
                d.as_inches()
            } else {
                d.as_meters()
            })
        })
    };
    let speed =
        |v: Option<Velocity>| v.map(|v| num(if imperial { v.as_mph() } else { v.as_mps() }));
    let f = |v: Option<f64>| v.map(num);
    let i = |v: Option<i32>| v.map(|v| v.to_string());

    let data = &line.data;
    let b = data.ball.clone().unwrap_or_default();
    let d = data.derived.clone().unwrap_or_default();
    let c = data.club.clone().unwrap_or_default();
    let m = data.impact.clone().unwrap_or_default();
    let cells = [
        Some(line.time.clone()),
        Some(line.actor.clone()),
        Some(line.device.kind.clone()),
        line.device.manufacturer.clone(),
        line.device.model.clone(),
        line.device.firmware.clone(),
        Some(line.shot_number.to_string()),
        Some(line.shot_id.clone()),
        line.club_selected.map(|c| c.to_string()),
        label(line.settings.handed.as_ref()),
        line.settings.detection_mode.map(|m| m.to_string()),
        environment(shot),
        speed(b.launch_speed),
        f(b.launch_elevation),
        f(b.launch_azimuth),
        i(b.backspin_rpm),
        i(b.sidespin_rpm),
        dist(b.carry_distance),
        dist(b.total_distance),
        dist(b.roll_distance),
        height(b.max_height),
        f(b.flight_time),
        dist(d.carry_distance),
        dist(d.carry_offline),
        height(d.max_height),
        f(d.flight_time),
        f(d.descent_angle),
        dist(d.roll_distance),
        dist(d.roll_offline),
        dist(d.total_distance),
        dist(d.total_offline),
        speed(c.club_speed),
        speed(c.club_speed_post),
        f(c.path),
        f(c.attack_angle),
        f(c.face_angle),
        f(c.dynamic_loft),
        f(c.smash_factor),
        f(c.swing_plane_horizontal),
        f(c.swing_plane_vertical),
        small(c.club_offset),
        small(c.club_height),
        m.lateral.map(|v| num(v.as_millimeters())),
        m.vertical.map(|v| num(v.as_millimeters())),
    ];
    csv_row(&cells)
}

/// One RFC 4180 row: empty cell for `None`, quoted when a cell holds a
/// comma, quote or line break. Newline-terminated (`\r\n`).
fn csv_row(cells: &[Option<String>]) -> String {
    let mut out = String::new();
    for (n, cell) in cells.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        let Some(cell) = cell else { continue };
        if cell.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&cell.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(cell);
        }
    }
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::super::tests::{feed, test_state};
    use super::super::{ShotContext, StoredShot, device_section};
    use super::*;
    use flighthook::{
        BallFlight, ClubData, FaceImpact, FlighthookConfig, FlighthookEvent, MevoSection, RawDir,
        RawRecord, ShotData, ShotKey, ShotRaw,
    };

    const SSID: &str = "FS M2-123456";
    const ADDR: &str = "192.168.2.1:5100";
    const BLE: &str = "02:00:00:AB:CD:01";

    fn ball() -> BallFlight {
        BallFlight {
            launch_speed: Some(Velocity::MetersPerSecond(40.0)),
            launch_elevation: Some(20.0),
            launch_azimuth: Some(-1.5),
            carry_distance: Some(Distance::Meters(100.0)),
            backspin_rpm: Some(6000),
            sidespin_rpm: None,
            ..BallFlight::default()
        }
    }

    fn mevo_config() -> FlighthookConfig {
        let mut config: FlighthookConfig = serde_json::from_str("{}").expect("defaults");
        config.mevo.insert(
            "0".into(),
            MevoSection {
                name: "My Mevo".into(),
                address: Some(ADDR.into()),
                ball_type: Some(1),
                tee_height: None,
                range: None,
                surface_height: None,
                track_pct: None,
                use_estimated: None,
                camera_mode: None,
                environment: None,
            },
        );
        config
    }

    fn private_shot() -> StoredShot {
        let (section, mut scrub) = device_section(&mevo_config(), "mevo", "0");
        scrub.extend([SSID.to_string(), BLE.to_string()]);
        let text = format!("{{\"id\":\"{SSID}\",\"peer\":\"{ADDR}\",\"ble\":\"{BLE}\"}}");
        StoredShot {
            received_at: Utc::now(),
            key: ShotKey {
                shot_id: "id-1".into(),
                shot_number: 1,
            },
            context: ShotContext {
                device: ExportDevice {
                    kind: "flightscope".into(),
                    manufacturer: Some("FlightScope".into()),
                    model: Some(format!("{SSID} v1")),
                    firmware: None,
                },
                device_section: section,
                scrub,
                ..ShotContext::default()
            },
            data: ShotData {
                actor: "mevo.0".into(),
                shot_number: 1,
                ball: Some(ball()),
                club: None,
                impact: None,
                derived: None,
            },
            raw: Some(ShotRaw {
                transport: "flightscope-tcp".into(),
                frames: vec![
                    RawRecord::text(RawDir::Rx, text),
                    RawRecord::binary(RawDir::Rx, &[0xd4, 0x01]),
                ],
                ..ShotRaw::default()
            }),
        }
    }

    #[test]
    fn export_line_contains_no_identity() {
        let shot = private_shot();
        let line = jsonl_line(&shot, "2026-01-01T00:00:00.000Z", None, true);
        assert!(line.ends_with('\n'));
        for secret in [SSID, ADDR, BLE, "192.168.2.1", "My Mevo"] {
            assert!(!line.contains(secret), "line leaks {secret}: {line}");
        }
        for key in [
            "\"name\"",
            "\"device_info\"",
            "\"ble_address\"",
            "\"address\"",
        ] {
            assert!(!line.contains(key), "line has {key}");
        }
        let parsed: ShotExport = serde_json::from_str(&line).expect("parses");
        assert_eq!(parsed.device.kind, "flightscope");
        assert_eq!(parsed.device.model.as_deref(), Some("<redacted> v1"));
        let raw = parsed.raw.expect("raw");
        assert_eq!(
            raw.frames[0].text.as_deref(),
            Some(r#"{"id":"<redacted>","peer":"<redacted>","ble":"<redacted>"}"#)
        );
        assert_eq!(raw.frames[1].hex.as_deref(), Some("d401"));
        assert_eq!(parsed.settings.device["ball_type"], 1);
    }

    #[test]
    fn jsonl_line_parses_back_and_converts_units() {
        let shot = private_shot();
        let native: ShotExport =
            serde_json::from_str(&jsonl_line(&shot, "t", None, true)).expect("parses");
        assert_eq!(native.schema, 1);
        assert_eq!(native.data, shot.data);
        assert_eq!(native.exported_at, "t");

        let imperial: ShotExport =
            serde_json::from_str(&jsonl_line(&shot, "t", Some(UnitSystem::Imperial), false))
                .expect("parses");
        assert_eq!(
            imperial.data,
            shot.data.to_unit_system(UnitSystem::Imperial)
        );
        let ball = imperial.data.ball.expect("ball");
        assert!(matches!(ball.launch_speed, Some(Velocity::MilesPerHour(_))));
        assert!(matches!(ball.carry_distance, Some(Distance::Yards(_))));
        assert!(imperial.raw.is_none());
    }

    #[test]
    fn csv_header_names_units() {
        let imp = csv_header(UnitSystem::Imperial);
        let met = csv_header(UnitSystem::Metric);
        assert!(imp.starts_with("time,actor,device_kind,manufacturer,model,firmware,"));
        assert!(imp.ends_with("impact_lateral_mm,impact_vertical_mm\r\n"));
        for col in [
            "ball_speed_mph",
            "carry_yd",
            "max_height_ft",
            "club_offset_in",
        ] {
            assert!(imp.contains(col), "{col}");
        }
        for col in ["ball_speed_mps", "carry_m", "max_height_m", "club_offset_m"] {
            assert!(met.contains(col), "{col}");
        }
        assert_eq!(imp.split(',').count(), 44);
        assert_eq!(met.split(',').count(), 44);
    }

    #[test]
    fn csv_row_has_empty_cells_for_none_and_matches_header() {
        let shot = private_shot();
        let row = to_csv_row(&shot, UnitSystem::Metric);
        let cells: Vec<&str> = row.trim_end().split(',').collect();
        assert_eq!(cells.len(), 44);
        assert_eq!(cells[1], "mevo.0");
        assert_eq!(cells[2], "flightscope");
        assert_eq!(cells[4], "<redacted> v1");
        assert_eq!(cells[5], "", "firmware");
        assert_eq!(cells[11], "limited_flight");
        assert_eq!(cells[12], "40");
        assert_eq!(cells[16], "", "sidespin");
        assert_eq!(cells[17], "100");
        assert!(!row.contains(SSID) && !row.contains(ADDR));

        let imp = to_csv_row(&shot, UnitSystem::Imperial);
        let cells: Vec<&str> = imp.trim_end().split(',').collect();
        assert_eq!(cells[12], num(Velocity::MetersPerSecond(40.0).as_mph()));
        assert_eq!(cells[17], num(Distance::Meters(100.0).as_yards()));
    }

    #[test]
    fn csv_quotes_commas_and_quotes() {
        let row = csv_row(&[
            Some("Model, \"X\"".into()),
            None,
            Some("plain".into()),
            Some("two\nlines".into()),
        ]);
        assert_eq!(row, "\"Model, \"\"X\"\"\",,plain,\"two\nlines\"\r\n");
    }

    #[test]
    fn csv_has_club_and_impact_columns() {
        let mut shot = private_shot();
        shot.data.club = Some(ClubData {
            club_speed: Some(Velocity::MetersPerSecond(30.0)),
            club_offset: Some(Distance::Meters(0.01)),
            ..ClubData::default()
        });
        shot.data.impact = Some(FaceImpact {
            lateral: Some(Distance::Millimeters(-4.0)),
            vertical: Some(Distance::Millimeters(2.5)),
        });
        let header: Vec<String> = csv_header(UnitSystem::Metric)
            .trim_end()
            .split(',')
            .map(str::to_string)
            .collect();
        let row = to_csv_row(&shot, UnitSystem::Metric);
        let cells: Vec<&str> = row.trim_end().split(',').collect();
        let col = |name: &str| cells[header.iter().position(|h| h == name).expect(name)];
        assert_eq!(col("club_speed_mps"), "30");
        assert_eq!(col("club_offset_m"), "0.01");
        assert_eq!(col("impact_lateral_mm"), "-4");
        assert_eq!(col("impact_vertical_mm"), "2.5");
    }

    #[tokio::test]
    async fn lifecycle_round_trips_through_the_export() {
        let (state, _writer, _dir) = test_state();
        let key = ShotKey {
            shot_id: "rt".into(),
            shot_number: 9,
        };
        let raw = ShotRaw {
            transport: "flightscope-tcp".into(),
            frames: vec![
                RawRecord::binary(RawDir::Rx, &[0xe5, 0x00]),
                RawRecord::binary(RawDir::Tx, &[0x01, 0x01]),
            ],
            ..ShotRaw::default()
        };
        feed(
            &state,
            "mevo.0",
            vec![
                FlighthookEvent::ShotTrigger { key: key.clone() },
                FlighthookEvent::BallFlight {
                    key: key.clone(),
                    ball: Box::new(ball()),
                    derived: None,
                },
                FlighthookEvent::ClubPath {
                    key: key.clone(),
                    club: Box::new(ClubData::default()),
                },
                FlighthookEvent::ShotFinished {
                    key: key.clone(),
                    raw: Some(Box::new(raw)),
                },
            ],
        )
        .await;
        let stored = state.shots.read().await[0].clone();
        let line: ShotExport =
            serde_json::from_str(&jsonl_line(&stored, "t", None, true)).expect("parses");
        assert_eq!(line.data, stored.data);
        assert_eq!(line.shot_id, "rt");
        assert_eq!(line.shot_number, 9);
        assert_eq!(line.raw.expect("raw").frames.len(), 2);
    }
}
