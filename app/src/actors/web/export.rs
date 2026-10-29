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
        group: shot.data.group.clone(),
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
        "group".into(),
        "handed".into(),
        "detection_mode".into(),
        "environment".into(),
        format!("ball_speed_{}", u.speed),
        "launch_elevation_deg".into(),
        "launch_azimuth_deg".into(),
        "backspin_rpm".into(),
        "sidespin_rpm".into(),
        "total_spin_rpm".into(),
        "spin_axis_deg".into(),
        format!("carry_{}", u.dist),
        format!("carry_offline_{}", u.dist),
        format!("total_{}", u.dist),
        format!("total_offline_{}", u.dist),
        format!("roll_{}", u.dist),
        format!("max_height_{}", u.height),
        "flight_time_s".into(),
        "descent_angle_deg".into(),
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
        "spin_loft_deg".into(),
        format!("low_point_distance_{}", u.small),
        "impact_lateral_mm".into(),
        "impact_vertical_mm".into(),
        "metrics_total_spin_rpm".into(),
        "metrics_spin_axis_deg".into(),
        "metrics_face_to_path_deg".into(),
        "metrics_spin_loft_deg".into(),
        format!("metrics_curve_{}", u.dist),
        "spin_estimated".into(),
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
    let x = data.metrics.clone().unwrap_or_default();
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
        line.group.clone(),
        label(line.settings.handed.as_ref()),
        line.settings.detection_mode.map(|m| m.to_string()),
        environment(shot),
        speed(b.launch_speed),
        f(b.launch_elevation),
        f(b.launch_azimuth),
        i(b.backspin_rpm),
        i(b.sidespin_rpm),
        i(b.total_spin_rpm),
        f(b.spin_axis),
        dist(b.carry_distance),
        dist(b.carry_offline),
        dist(b.total_distance),
        dist(b.total_offline),
        dist(b.roll_distance),
        height(b.max_height),
        f(b.flight_time),
        f(b.descent_angle),
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
        f(c.spin_loft),
        small(c.low_point_distance),
        m.lateral.map(|v| num(v.as_millimeters())),
        m.vertical.map(|v| num(v.as_millimeters())),
        i(x.total_spin_rpm),
        f(x.spin_axis),
        f(x.face_to_path),
        f(x.spin_loft),
        dist(x.curve),
        d.spin_estimated.then(|| "true".to_owned()),
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
        BallFlight, ClubData, DerivedFlight, FaceImpact, FlighthookConfig, FlighthookEvent,
        MevoSection, RawDir, RawRecord, ShotData, ShotKey, ShotMetrics, ShotRaw,
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
                spin_source: flighthook::SpinSource::D4,
                launch_only_spin: MevoSection::DEFAULT_LAUNCH_ONLY_SPIN,
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
                ..ShotData::default()
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
        assert!(imp.contains(",impact_lateral_mm,impact_vertical_mm,"));
        assert!(imp.ends_with(",metrics_curve_yd,spin_estimated\r\n"));
        assert!(met.ends_with(",metrics_curve_m,spin_estimated\r\n"));
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
        assert_eq!(imp.split(',').count(), 58);
        assert_eq!(met.split(',').count(), 58);
    }

    #[test]
    fn csv_row_has_empty_cells_for_none_and_matches_header() {
        let shot = private_shot();
        let row = to_csv_row(&shot, UnitSystem::Metric);
        let cells: Vec<&str> = row.trim_end().split(',').collect();
        assert_eq!(cells.len(), 58);
        assert_eq!(cells[1], "mevo.0");
        assert_eq!(cells[2], "flightscope");
        assert_eq!(cells[4], "<redacted> v1");
        assert_eq!(cells[5], "", "firmware");
        assert_eq!(cells[9], "", "group");
        assert_eq!(cells[12], "limited_flight");
        assert_eq!(cells[13], "40");
        assert_eq!(cells[17], "", "sidespin");
        assert_eq!(cells[20], "100");
        assert!(!row.contains(SSID) && !row.contains(ADDR));

        let imp = to_csv_row(&shot, UnitSystem::Imperial);
        let cells: Vec<&str> = imp.trim_end().split(',').collect();
        assert_eq!(cells[13], num(Velocity::MetersPerSecond(40.0).as_mph()));
        assert_eq!(cells[20], num(Distance::Meters(100.0).as_yards()));
    }

    #[test]
    fn simulated_shot_exports_its_path_in_jsonl_only() {
        let mut shot = private_shot();
        let mut b = ball();
        b.sidespin_rpm = Some(-200);
        shot.data.ball = Some(b.clone());
        shot.data.derived = crate::sim::derive(&b, &flighthook::SimulationSection::default());
        assert!(shot.data.derived.as_ref().is_some_and(|d| d.path.is_some()));

        let line: serde_json::Value =
            serde_json::from_str(&jsonl_line(&shot, "t", Some(UnitSystem::Imperial), false))
                .expect("parses");
        let flight = &line["data"]["derived"]["path"]["flight"];
        assert_eq!(flight[0], serde_json::json!([0.0, 0.0, 0.0, 0.0]));
        assert!(flight.as_array().is_some_and(|f| f.len() > 2), "{line}");

        let row = to_csv_row(&shot, UnitSystem::Metric);
        assert_eq!(row.trim_end().split(',').count(), 58);
    }

    #[test]
    fn forced_shot_exports_the_flag_and_both_values() {
        let replacing = flighthook::SimulationSection {
            mode: flighthook::SimMode::Force,
            ..flighthook::SimulationSection::default()
        };
        let mut shot = private_shot();
        let mut b = ball();
        b.sidespin_rpm = Some(-200);
        shot.data.ball = Some(b.clone());
        shot.data.derived = crate::sim::derive(&b, &replacing);
        shot.context.simulation = replacing;
        let sim_carry = shot
            .data
            .derived
            .as_ref()
            .and_then(|d| d.carry_distance)
            .expect("simulated carry");

        let line: serde_json::Value =
            serde_json::from_str(&jsonl_line(&shot, "t", None, false)).expect("parses");
        assert_eq!(line["data"]["derived"]["replaces_measured"], true, "{line}");
        assert_eq!(line["settings"]["simulation"]["mode"], "force", "{line}");
        assert!(line["settings"]["simulation"].get("enabled").is_none());
        assert_eq!(line["data"]["ball"]["carry_distance"], "100m", "{line}");

        // CSV keeps its columns: the device's carry and the simulated carry.
        let row = to_csv_row(&shot, UnitSystem::Metric);
        let cells: Vec<&str> = row.trim_end().split(',').collect();
        assert_eq!(cells.len(), 58);
        assert_eq!(cells[20], "100");
        assert_eq!(cells[28], num(sim_carry.as_meters()));
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

    #[test]
    fn csv_has_new_shot_field_and_metrics_columns() {
        let mut shot = private_shot();
        shot.data.ball = Some(BallFlight {
            carry_offline: Some(Distance::Meters(-2.5)),
            total_offline: Some(Distance::Meters(-3.0)),
            descent_angle: Some(44.5),
            total_spin_rpm: Some(6100),
            spin_axis: Some(-4.0),
            ..ball()
        });
        shot.data.club = Some(ClubData {
            spin_loft: Some(23.5),
            low_point_distance: Some(Distance::Meters(0.0254)),
            ..ClubData::default()
        });
        let mut metrics = ShotMetrics::default();
        metrics.face_to_path = Some(-1.25);
        metrics.spin_loft = Some(21.0);
        metrics.curve = Some(Distance::Meters(9.144));
        metrics.total_spin_rpm = Some(3231);
        metrics.spin_axis = Some(8.0);
        shot.data.metrics = Some(metrics);
        for (system, offline, total_offline, curve, low_point) in [
            (UnitSystem::Metric, "-2.5", "-3", "9.144", "0.0254"),
            (UnitSystem::Imperial, "-2.734", "-3.2808", "10", "1"),
        ] {
            let header: Vec<String> = csv_header(system)
                .trim_end()
                .split(',')
                .map(str::to_string)
                .collect();
            let row = to_csv_row(&shot, system);
            let cells: Vec<&str> = row.trim_end().split(',').collect();
            assert_eq!(cells.len(), header.len());
            let (d, s) = if system == UnitSystem::Imperial {
                ("yd", "in")
            } else {
                ("m", "m")
            };
            let col = |name: &str| cells[header.iter().position(|h| h == name).expect(name)];
            assert_eq!(col(&format!("carry_offline_{d}")), offline);
            assert_eq!(col(&format!("total_offline_{d}")), total_offline);
            assert_eq!(col("descent_angle_deg"), "44.5");
            assert_eq!(col("total_spin_rpm"), "6100");
            assert_eq!(col("spin_axis_deg"), "-4");
            assert_eq!(col("spin_loft_deg"), "23.5");
            assert_eq!(col(&format!("low_point_distance_{s}")), low_point);
            assert_eq!(col("metrics_total_spin_rpm"), "3231");
            assert_eq!(col("metrics_spin_axis_deg"), "8");
            assert_eq!(col("metrics_face_to_path_deg"), "-1.25");
            assert_eq!(col("metrics_spin_loft_deg"), "21");
            assert_eq!(col(&format!("metrics_curve_{d}")), curve);
        }
    }

    /// The export's `data` carries the metrics the web layer computed when
    /// the shot finished.
    #[tokio::test]
    async fn exported_shot_carries_its_metrics() {
        let (state, _writer, _dir) = test_state();
        let key = ShotKey {
            shot_id: "m".into(),
            shot_number: 2,
        };
        feed(
            &state,
            "mevo.0",
            vec![
                FlighthookEvent::ShotTrigger { key: key.clone() },
                FlighthookEvent::ClubPath {
                    key: key.clone(),
                    club: Box::new(ClubData {
                        face_angle: Some(1.0),
                        path: Some(-2.0),
                        ..ClubData::default()
                    }),
                },
                FlighthookEvent::ShotFinished { key, raw: None },
            ],
        )
        .await;
        let stored = state.shots.read().await[0].clone();
        let line = build_export_line(&stored, "t", None, false);
        assert_eq!(line.data.metrics.and_then(|m| m.face_to_path), Some(3.0));
        let row = to_csv_row(&stored, UnitSystem::Metric);
        let header = csv_header(UnitSystem::Metric);
        let at = header
            .trim_end()
            .split(',')
            .position(|h| h == "metrics_face_to_path_deg")
            .expect("column");
        assert_eq!(row.trim_end().split(',').nth(at), Some("3"));
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

    /// A shot whose spin flighthook estimated keeps the flag through the
    /// shot store into both exports; a measured shot has none.
    #[tokio::test]
    async fn estimated_spin_is_flagged_in_both_exports() {
        let (state, _writer, _dir) = test_state();
        let mut only_the_flag = DerivedFlight::default();
        only_the_flag.spin_estimated = true;
        for (n, derived) in [(1, Some(only_the_flag)), (2, None)] {
            let key = ShotKey {
                shot_id: format!("chip-{n}"),
                shot_number: n,
            };
            feed(
                &state,
                "mevo.0",
                vec![
                    FlighthookEvent::ShotTrigger { key: key.clone() },
                    FlighthookEvent::BallFlight {
                        key: key.clone(),
                        ball: Box::new(ball()),
                        derived: derived.map(Box::new),
                    },
                    FlighthookEvent::ShotFinished { key, raw: None },
                ],
            )
            .await;
        }
        let shots = state.shots.read().await.clone();
        assert_eq!(shots.len(), 2);
        for system in [UnitSystem::Imperial, UnitSystem::Metric] {
            let header = csv_header(system);
            let at = header
                .trim_end()
                .split(',')
                .position(|h| h == "spin_estimated")
                .expect("column");
            for (shot, flagged) in shots.iter().zip([true, false]) {
                let line: serde_json::Value =
                    serde_json::from_str(&jsonl_line(shot, "t", Some(system), false))
                        .expect("parses");
                let flag = line["data"]["derived"].get("spin_estimated");
                assert_eq!(
                    flag,
                    flagged.then_some(&serde_json::Value::Bool(true)),
                    "{line}"
                );
                let row = to_csv_row(shot, system);
                let cell = row.trim_end().split(',').nth(at);
                assert_eq!(cell, Some(if flagged { "true" } else { "" }), "{row}");
            }
        }
    }

    /// An Auto-chip is exported with the detection mode it was simulated in,
    /// Chipping, whatever the game mode; a device's shot keeps the game's.
    #[tokio::test]
    async fn auto_chip_exports_chipping_as_its_detection_mode() {
        let (state, writer, _dir) = test_state();
        writer.set_mode(flighthook::ShotDetectionMode::Full);
        for (n, actor) in [(1, "mevo.0"), (2, flighthook::AUTO_CHIP_ACTOR)] {
            let key = ShotKey {
                shot_id: format!("shot-{n}"),
                shot_number: n,
            };
            feed(
                &state,
                actor,
                vec![
                    FlighthookEvent::ShotTrigger { key: key.clone() },
                    FlighthookEvent::BallFlight {
                        key: key.clone(),
                        ball: Box::new(ball()),
                        derived: None,
                    },
                    FlighthookEvent::ShotFinished { key, raw: None },
                ],
            )
            .await;
        }
        let shots = state.shots.read().await.clone();
        let modes: Vec<serde_json::Value> = shots
            .iter()
            .map(|shot| {
                let line: serde_json::Value =
                    serde_json::from_str(&jsonl_line(shot, "t", None, false)).expect("parses");
                line["settings"]["detection_mode"].clone()
            })
            .collect();
        assert_eq!(modes, ["full", "chipping"]);
        let at = csv_header(UnitSystem::Imperial)
            .trim_end()
            .split(',')
            .position(|h| h == "detection_mode")
            .expect("column");
        let cells: Vec<String> = shots
            .iter()
            .map(|shot| {
                to_csv_row(shot, UnitSystem::Imperial)
                    .trim_end()
                    .split(',')
                    .nth(at)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        assert_eq!(cells, ["full", "chipping"]);
    }

    const GROUP: &str = "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7";

    fn grouped_shot() -> StoredShot {
        let mut shot = private_shot();
        shot.context.club_selected = Some(flighthook::Club::Iron7);
        shot.data.shot_id = Some(shot.key.shot_id.clone());
        shot.data.club_selected = Some(flighthook::Club::Iron7);
        shot.data.group = Some(GROUP.into());
        shot
    }

    #[test]
    fn csv_group_column_follows_club_selected() {
        let header: Vec<String> = csv_header(UnitSystem::Imperial)
            .trim_end()
            .split(',')
            .map(str::to_string)
            .collect();
        let at = |name: &str| header.iter().position(|h| h == name).expect(name);
        assert_eq!(at("group"), at("club_selected") + 1);
        assert_eq!(at("handed"), at("group") + 1);

        let row = to_csv_row(&grouped_shot(), UnitSystem::Imperial);
        let cells: Vec<&str> = row.trim_end().split(',').collect();
        assert_eq!(cells.len(), header.len());
        assert_eq!(cells[at("club_selected")], "7I");
        assert_eq!(cells[at("group")], GROUP);
    }

    #[test]
    fn jsonl_line_carries_the_group() {
        let shot = grouped_shot();
        let line = jsonl_line(&shot, "t", None, false);
        let v: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(v["group"], GROUP);
        assert_eq!(v["data"]["group"], GROUP);
        assert_eq!(v["data"]["shot_id"], v["shot_id"]);
        assert_eq!(v["data"]["club_selected"], v["club_selected"]);
        for absent in ["number", "mode", "session"] {
            assert!(v.get(absent).is_none(), "{absent}");
        }
        // Top level: `group` right after `club_selected`, before `settings`.
        let pair = format!(r#""club_selected":"7I","group":"{GROUP}","settings""#);
        assert!(line.contains(&pair), "{line}");

        let ungrouped = jsonl_line(&private_shot(), "t", None, false);
        let v: serde_json::Value = serde_json::from_str(&ungrouped).expect("json");
        assert!(v.get("group").is_none());
    }
}
