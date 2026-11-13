//! Shot export: one JSONL line or CSV row per stored shot.
//!
//! The JSONL line is a [`ShotExport`]: the `/api/shots` element plus the
//! settings in effect and the raw device frames. CSV carries the decoded
//! values in fixed-unit columns and no raw frames. Neither ever contains the
//! FRP device ID, actor names, telemetry or configured addresses; identity
//! strings that could appear in text payloads or device labels are scrubbed.

use std::collections::HashMap;

use chrono::SecondsFormat;

use super::StoredShot;
use crate::raw_capture::scrub_text;
use flighthook::{
    Basis, Distance, EXPORT_SCHEMA, ExportDevice, ExportSettings, FlightResult,
    FlightScopeEnvironment, FlighthookConfig, ShotExport, UnitSystem, Velocity,
};

/// Build the JSONL record for one shot. `units` converts `data` (native
/// units when `None`); `include_raw = false` leaves the raw frames out;
/// `golfers` names the shot's golfer by the lookup rule.
pub fn build_export_line(
    shot: &StoredShot,
    exported_at: &str,
    units: Option<UnitSystem>,
    include_raw: bool,
    golfers: &FlighthookConfig,
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
        actor: shot.data.card_key().to_owned(),
        shot_id: shot.key.shot_id.clone(),
        shot_number: shot.key.shot_number,
        device,
        golfer_id: golfer_id(&shot.data, golfers),
        club_selected: shot.data.club_selected(),
        block: shot.data.block.clone(),
        settings: ExportSettings {
            device: ctx.device_section.clone(),
            // The conditions the shot was simulated under; whether the
            // dashboard offers Auto-chip, tournament mode and the flight
            // model's actor id are no part of them.
            simulation: flighthook::SimulationSection {
                auto_chip: false,
                tournament: false,
                actor_id: None,
                ..ctx.simulation.clone()
            },
            detection_mode: ctx.detection_mode,
            handed: shot.data.handed(),
        },
        data: export_data(&shot.data, units, &ctx.card_keys),
        raw,
    }
}

/// The golfer of the turn the shot was hit in, as the export names it:
/// the `flighthook.*` id of the golfer of `golfers` whose identifiers
/// contain it, else the turn's golfer id as it is.
fn golfer_id(data: &flighthook::ShotData, golfers: &FlighthookConfig) -> Option<String> {
    golfers.golfer_key(
        data.turn
            .as_ref()
            .and_then(|t| t.golfer.as_ref())
            .map(flightrelay::GolferId::as_str),
    )
}

/// `data` as the export carries it: in `units` (native when `None`),
/// without the shot's camera clips, which never leave this computer through
/// the export, and with every local actor (a key of `card_keys`) named by
/// its card key, since an actor id is stable per install.
fn export_data(
    data: &flighthook::ShotData,
    units: Option<UnitSystem>,
    card_keys: &HashMap<String, String>,
) -> flighthook::ShotData {
    let mut data = match units {
        Some(u) => data.to_unit_system(u),
        None => data.clone(),
    };
    data.video = Vec::new();
    data.from_name = None;
    data.golfer_name = None;
    let local = |from: &mut String| {
        if let Some(card) = card_keys.get(from.as_str()) {
            from.clone_from(card);
        }
    };
    local(&mut data.from);
    for flight in &mut data.flights {
        local(&mut flight.from);
    }
    data
}

/// One JSONL line, newline-terminated.
pub fn jsonl_line(
    shot: &StoredShot,
    exported_at: &str,
    units: Option<UnitSystem>,
    include_raw: bool,
    golfers: &FlighthookConfig,
) -> String {
    let line = build_export_line(shot, exported_at, units, include_raw, golfers);
    match serde_json::to_string(&line) {
        Ok(mut s) => {
            s.push('\n');
            s
        }
        Err(e) => {
            tracing::warn!(
                "shot export: could not serialize shot {}: {e}",
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

/// CSV header row, newline-terminated. `golfer` adds the trailing
/// `golfer_id` column, for an export where a shot has a golfer.
pub fn csv_header(system: UnitSystem, golfer: bool) -> String {
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
        "block".into(),
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
    let mut cells: Vec<Option<String>> = cols.into_iter().map(Some).collect();
    if golfer {
        cells.push(Some("golfer_id".into()));
    }
    csv_row(&cells)
}

/// Whether any of `shots` has a golfer: the CSV then has a `golfer_id`
/// column.
pub fn any_golfer<'a>(shots: impl IntoIterator<Item = &'a StoredShot>) -> bool {
    shots
        .into_iter()
        .any(|s| s.data.turn.as_ref().is_some_and(|t| t.golfer.is_some()))
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
            label(Some(&FlightScopeEnvironment::default()))
        }
        _ => None,
    }
}

/// One CSV row for a shot, in `system` units, newline-terminated; with
/// `golfer`, the trailing `golfer_id` cell (by the lookup rule on
/// `golfers`).
pub fn to_csv_row(
    shot: &StoredShot,
    system: UnitSystem,
    golfer: bool,
    golfers: &FlighthookConfig,
) -> String {
    let line = build_export_line(shot, "", Some(system), false, golfers);
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
    let shown = data.shown();
    let b = shown.launch.unwrap_or_default();
    let fl = shown
        .flight
        .unwrap_or_else(|| FlightResult::new(Basis::Modeled));
    let d = shown.derived.unwrap_or_default();
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
        line.block.clone(),
        label(line.settings.handed.as_ref()),
        line.settings.detection_mode.map(|m| m.to_string()),
        environment(shot),
        speed(b.launch_speed),
        f(b.launch_elevation),
        f(b.launch_azimuth),
        i(flighthook::output_backspin(&b)),
        i(b.sidespin_rpm),
        i(b.total_spin_rpm),
        f(b.spin_axis),
        dist(fl.carry_distance),
        dist(fl.carry_offline),
        dist(fl.total_distance),
        dist(fl.total_offline),
        dist(fl.roll_distance),
        height(fl.max_height),
        f(fl.flight_time),
        f(fl.descent_angle),
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
    let mut cells = cells.to_vec();
    if golfer {
        cells.push(line.golfer_id.clone());
    }
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

    use super::super::tests::{Ev, feed, test_state};
    use super::super::{ExportContext, StoredShot, device_section};
    use super::*;
    use crate::test_ball::Ball;
    use flighthook::{
        ClubData, FaceImpact, FlighthookConfig, MevoSection, RawDir, RawRecord, ShotData, ShotKey,
        ShotMetrics, ShotRaw,
    };

    const SSID: &str = "FS M2-123456";
    const ADDR: &str = "192.168.2.1:5100";
    const BLE: &str = "02:00:00:AB:CD:01";

    fn ball() -> Ball {
        Ball {
            launch_speed: Some(Velocity::MetersPerSecond(40.0)),
            launch_elevation: Some(20.0),
            launch_azimuth: Some(-1.5),
            carry_distance: Some(Distance::Meters(100.0)),
            backspin_rpm: Some(6000),
            sidespin_rpm: None,
            ..Ball::default()
        }
    }

    /// The `shot.launch` and `shot.flight` of [`ball`].
    fn ball_events(key: &ShotKey) -> [Ev; 2] {
        [
            Ev::frp(flightrelay::ShotLaunch {
                key: key.clone(),
                launch: ball().launch(),
            }),
            Ev::frp(flightrelay::ShotFlight {
                key: key.clone(),
                flight: ball().flight().expect("carry"),
            }),
        ]
    }

    fn mevo_config() -> FlighthookConfig {
        let mut config: FlighthookConfig = serde_json::from_str("{}").expect("defaults");
        config.mevo.insert(
            "0".into(),
            MevoSection {
                name: "My Mevo".into(),
                driver: flighthook::MevoConfig {
                    address: Some(ADDR.into()),
                    ball_type: Some(1),
                    ..flighthook::MevoConfig::default()
                },
                ..MevoSection::default()
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
            context: ExportContext {
                device: ExportDevice {
                    kind: "flightscope".into(),
                    manufacturer: Some("FlightScope".into()),
                    model: Some(format!("{SSID} v1")),
                    firmware: None,
                },
                device_section: section,
                scrub,
                ..ExportContext::default()
            },
            data: ShotData {
                card: Some("mevo.0".into()),
                shot_number: 1,
                launch: Some(ball().launch()),
                flights: ball()
                    .flight()
                    .into_iter()
                    .map(|flight| flighthook::Flight {
                        from: String::new(),
                        flight,
                    })
                    .collect(),
                club: None,
                impact: None,
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
            frp: None,
            path: None,
        }
    }

    #[test]
    fn export_line_contains_no_identity() {
        let shot = private_shot();
        let line = jsonl_line(
            &shot,
            "2026-01-01T00:00:00.000Z",
            None,
            true,
            &FlighthookConfig::default(),
        );
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

    /// Local actor ids never leave through the export: the shot's and each
    /// flight's sender are written as the card key; a remote actor keeps
    /// its actor id.
    #[test]
    fn the_export_names_local_actors_by_card_key() {
        let mut shot = private_shot();
        shot.data.from = "lm-actor-7f3a".into();
        shot.data.flights = vec![
            flighthook::Flight {
                from: "lm-actor-7f3a".into(),
                flight: flightrelay::FlightResult::new(flightrelay::Basis::Tracked),
            },
            flighthook::Flight {
                from: "fm-actor-91bc".into(),
                flight: flightrelay::FlightResult::new(flightrelay::Basis::Modeled),
            },
            flighthook::Flight {
                from: "remote-model".into(),
                flight: flightrelay::FlightResult::new(flightrelay::Basis::Modeled),
            },
        ];
        shot.context.card_keys = HashMap::from([
            ("lm-actor-7f3a".into(), "mevo.0".into()),
            ("fm-actor-91bc".into(), "simulation".into()),
        ]);
        let line = jsonl_line(
            &shot,
            "2026-01-01T00:00:00.000Z",
            None,
            false,
            &FlighthookConfig::default(),
        );
        for id in ["lm-actor-7f3a", "fm-actor-91bc"] {
            assert!(!line.contains(id), "line has {id}: {line}");
        }
        let parsed: ShotExport = serde_json::from_str(&line).expect("parses");
        assert_eq!(parsed.data.from, "mevo.0");
        let senders: Vec<&str> = parsed
            .data
            .flights
            .iter()
            .map(|f| f.from.as_str())
            .collect();
        assert_eq!(senders, ["mevo.0", "simulation", "remote-model"]);
    }

    /// A game golfer's name and a remote sender's name stay on this
    /// computer: neither reaches the JSONL line nor the CSV row.
    #[test]
    fn the_export_never_carries_golfer_or_sender_names() {
        let mut shot = private_shot();
        shot.data.golfer_name = Some("Joe Private".into());
        shot.data.from_name = Some("Bay Two Sender".into());
        let line = jsonl_line(
            &shot,
            "2026-01-01T00:00:00.000Z",
            None,
            false,
            &FlighthookConfig::default(),
        );
        for leak in ["Joe Private", "Bay Two Sender", "golfer_name", "from_name"] {
            assert!(!line.contains(leak), "line has {leak}: {line}");
        }
        let parsed: ShotExport = serde_json::from_str(&line).expect("parses");
        assert_eq!(parsed.data.golfer_name, None);
        assert_eq!(parsed.data.from_name, None);
        for golfer in [false, true] {
            let row = to_csv_row(
                &shot,
                UnitSystem::Metric,
                golfer,
                &FlighthookConfig::default(),
            );
            for leak in ["Joe Private", "Bay Two Sender"] {
                assert!(!row.contains(leak), "row has {leak}: {row}");
            }
        }
    }

    #[test]
    fn the_export_never_carries_camera_clips() {
        let mut shot = private_shot();
        shot.data.video = vec![flighthook::ShotVideo {
            camera: "camera.0".into(),
            position: flighthook::CameraPosition::GolferFront,
            duration_ms: 5500,
            fps: 120.0,
            trigger_offset_ms: 4000,
            impact_offset_ms: 4000,
            complete: true,
            dropped: 0,
            pose: false,
            swing: false,
            rotation: 0,
            analyzed_with: None,
        }];
        for units in [None, Some(UnitSystem::Imperial), Some(UnitSystem::Metric)] {
            let line = jsonl_line(&shot, "t", units, true, &FlighthookConfig::default());
            assert!(!line.contains("video"), "{line}");
            assert!(!line.contains("camera.0"), "{line}");
        }
    }

    #[test]
    fn jsonl_line_parses_back_and_converts_units() {
        let shot = private_shot();
        let native: ShotExport = serde_json::from_str(&jsonl_line(
            &shot,
            "t",
            None,
            true,
            &FlighthookConfig::default(),
        ))
        .expect("parses");
        assert_eq!(native.schema, flighthook::EXPORT_SCHEMA);
        assert_eq!(native.data, shot.data);
        assert_eq!(native.exported_at, "t");

        let imperial: ShotExport = serde_json::from_str(&jsonl_line(
            &shot,
            "t",
            Some(UnitSystem::Imperial),
            false,
            &FlighthookConfig::default(),
        ))
        .expect("parses");
        assert_eq!(
            imperial.data,
            shot.data.to_unit_system(UnitSystem::Imperial)
        );
        let flight = imperial.data.shown().flight.expect("flight");
        let launch = imperial.data.launch.expect("launch");
        assert!(matches!(
            launch.launch_speed,
            Some(Velocity::MilesPerHour(_))
        ));
        assert!(matches!(flight.carry_distance, Some(Distance::Yards(_))));
        assert!(imperial.raw.is_none());
    }

    #[test]
    fn csv_header_names_units() {
        let imp = csv_header(UnitSystem::Imperial, false);
        let met = csv_header(UnitSystem::Metric, false);
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
        let row = to_csv_row(
            &shot,
            UnitSystem::Metric,
            false,
            &FlighthookConfig::default(),
        );
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

        let imp = to_csv_row(
            &shot,
            UnitSystem::Imperial,
            false,
            &FlighthookConfig::default(),
        );
        let cells: Vec<&str> = imp.trim_end().split(',').collect();
        assert_eq!(cells[13], num(Velocity::MetersPerSecond(40.0).as_mph()));
        assert_eq!(cells[20], num(Distance::Meters(100.0).as_yards()));
    }

    #[test]
    fn simulated_shot_exports_its_path_in_jsonl_only() {
        let mut shot = private_shot();
        let mut b = ball();
        b.sidespin_rpm = Some(-200);
        b.put_on(&mut shot.data);
        crate::test_ball::put_derived(
            &mut shot.data,
            crate::sim::derive(
                &b.launch(),
                b.flight().as_ref(),
                &flighthook::SimulationSection::default(),
            ),
        );
        assert!(shot.data.path.is_some());

        let line: serde_json::Value = serde_json::from_str(&jsonl_line(
            &shot,
            "t",
            Some(UnitSystem::Imperial),
            false,
            &FlighthookConfig::default(),
        ))
        .expect("parses");
        let flight = &line["data"]["path"]["flight"];
        assert_eq!(flight[0], serde_json::json!([0.0, 0.0, 0.0, 0.0]));
        assert!(flight.as_array().is_some_and(|f| f.len() > 2), "{line}");

        let row = to_csv_row(
            &shot,
            UnitSystem::Metric,
            false,
            &FlighthookConfig::default(),
        );
        assert_eq!(row.trim_end().split(',').count(), 58);
    }

    #[test]
    fn the_auto_chip_setting_is_not_exported_with_a_shots_simulation() {
        let mut shot = private_shot();
        shot.context.simulation.auto_chip = true;
        shot.context.simulation.tournament = true;
        let line: serde_json::Value = serde_json::from_str(&jsonl_line(
            &shot,
            "t",
            None,
            false,
            &FlighthookConfig::default(),
        ))
        .expect("parses");
        assert!(
            line["settings"]["simulation"].get("auto_chip").is_none(),
            "{line}"
        );
        assert!(
            line["settings"]["simulation"].get("tournament").is_none(),
            "tournament mode is no shot condition: {line}"
        );
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
        b.put_on(&mut shot.data);
        crate::test_ball::put_derived(
            &mut shot.data,
            crate::sim::derive(&b.launch(), b.flight().as_ref(), &replacing),
        );
        shot.context.simulation = replacing;
        let sim_carry = shot
            .data
            .shown()
            .derived
            .and_then(|d| d.carry_distance)
            .expect("simulated carry");

        let line: serde_json::Value = serde_json::from_str(&jsonl_line(
            &shot,
            "t",
            None,
            false,
            &FlighthookConfig::default(),
        ))
        .expect("parses");
        assert_eq!(line["settings"]["simulation"]["mode"], "force", "{line}");
        assert!(line["settings"]["simulation"].get("enabled").is_none());
        let own = &line["data"]["flights"][0]["flight"];
        assert_eq!(own["carry_distance"], "100m", "{line}");
        assert_eq!(own["basis"], "modeled", "{line}");

        // CSV keeps its columns: the device's carry and the simulated carry.
        let row = to_csv_row(
            &shot,
            UnitSystem::Metric,
            false,
            &FlighthookConfig::default(),
        );
        let cells: Vec<&str> = row.trim_end().split(',').collect();
        assert_eq!(cells.len(), 58);
        assert_eq!(cells[20], "100");
        assert_eq!(cells[28], num(sim_carry.as_meters()));
    }

    /// A total spin with no axis and no components fills `backspin_rpm`
    /// with the total, as the dashboard shows it, and leaves sidespin and
    /// spin axis empty.
    #[test]
    fn csv_shows_a_total_spin_alone_as_backspin() {
        let mut shot = private_shot();
        Ball {
            backspin_rpm: None,
            sidespin_rpm: None,
            total_spin_rpm: Some(6897),
            spin_axis: None,
            ..ball()
        }
        .put_on(&mut shot.data);
        let header: Vec<String> = csv_header(UnitSystem::Metric, false)
            .trim_end()
            .split(',')
            .map(str::to_string)
            .collect();
        let row = to_csv_row(
            &shot,
            UnitSystem::Metric,
            false,
            &FlighthookConfig::default(),
        );
        let cells: Vec<&str> = row.trim_end().split(',').collect();
        let col = |name: &str| cells[header.iter().position(|h| h == name).expect(name)];
        assert_eq!(col("backspin_rpm"), "6897");
        assert_eq!(col("total_spin_rpm"), "6897");
        assert_eq!(col("sidespin_rpm"), "");
        assert_eq!(col("spin_axis_deg"), "");
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
        let header: Vec<String> = csv_header(UnitSystem::Metric, false)
            .trim_end()
            .split(',')
            .map(str::to_string)
            .collect();
        let row = to_csv_row(
            &shot,
            UnitSystem::Metric,
            false,
            &FlighthookConfig::default(),
        );
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
        Ball {
            carry_offline: Some(Distance::Meters(-2.5)),
            total_offline: Some(Distance::Meters(-3.0)),
            descent_angle: Some(44.5),
            total_spin_rpm: Some(6100),
            spin_axis: Some(-4.0),
            ..ball()
        }
        .put_on(&mut shot.data);
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
            let header: Vec<String> = csv_header(system, false)
                .trim_end()
                .split(',')
                .map(str::to_string)
                .collect();
            let row = to_csv_row(&shot, system, false, &FlighthookConfig::default());
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
                Ev::trigger(key.clone()),
                Ev::frp(flightrelay::ShotClub {
                    key: key.clone(),
                    club: ClubData {
                        face_angle: Some(1.0),
                        path: Some(-2.0),
                        ..ClubData::default()
                    },
                }),
                Ev::Finished(key, None),
            ],
        )
        .await;
        let stored = state.shots.read().await[0].clone();
        let line = build_export_line(&stored, "t", None, false, &FlighthookConfig::default());
        assert_eq!(line.data.metrics.and_then(|m| m.face_to_path), Some(3.0));
        let row = to_csv_row(
            &stored,
            UnitSystem::Metric,
            false,
            &FlighthookConfig::default(),
        );
        let header = csv_header(UnitSystem::Metric, false);
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
                Ev::trigger(key.clone()),
                ball_events(&key)[0].clone(),
                ball_events(&key)[1].clone(),
                Ev::frp(flightrelay::ShotClub {
                    key: key.clone(),
                    club: ClubData::default(),
                }),
                Ev::Finished(key.clone(), Some(Box::new(raw))),
            ],
        )
        .await;
        let stored = state.shots.read().await[0].clone();
        let line: ShotExport = serde_json::from_str(&jsonl_line(
            &stored,
            "t",
            None,
            true,
            &FlighthookConfig::default(),
        ))
        .expect("parses");
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
        let model = state.root.flight_model_id();
        for (n, estimated) in [(1, true), (2, false)] {
            let key = ShotKey {
                shot_id: format!("chip-{n}"),
                shot_number: n,
            };
            // Shot 1 is a launch-only chip: a launch without spin and the
            // card's own flight result holding only the assumed backspin.
            // Shot 2 is measured: spin on the launch, a measured carry.
            let device = if estimated {
                let launch = flighthook::Launch {
                    backspin_rpm: None,
                    ..ball().launch()
                };
                vec![
                    Ev::frp(flightrelay::ShotLaunch {
                        key: key.clone(),
                        launch,
                    }),
                    Ev::frp(flightrelay::ShotFlight {
                        key: key.clone(),
                        flight: FlightResult {
                            model: Some(crate::actors::mevo::LAUNCH_ONLY_MODEL.into()),
                            assumed: Some(flighthook::AssumedLaunch {
                                backspin_rpm: Some(6000),
                                sidespin_rpm: Some(0),
                                ..flighthook::AssumedLaunch::default()
                            }),
                            ..FlightResult::new(Basis::Modeled)
                        },
                    }),
                ]
            } else {
                ball_events(&key).to_vec()
            };
            let mut events = vec![Ev::trigger(key.clone())];
            events.extend(device);
            events.push(Ev::Finished(key.clone(), None));
            feed(&state, "mevo.0", events).await;
            // The flight model flies the finished shot from its own actor.
            feed(
                &state,
                &model,
                vec![Ev::frp(flightrelay::ShotFlight {
                    key,
                    flight: FlightResult {
                        carry_distance: Some(Distance::Meters(100.0)),
                        ..FlightResult::new(Basis::Modeled)
                    },
                })],
            )
            .await;
        }
        let shots = state.shots.read().await.clone();
        assert_eq!(shots.len(), 2);
        for system in [UnitSystem::Imperial, UnitSystem::Metric] {
            let header = csv_header(system, false);
            let at = header
                .trim_end()
                .split(',')
                .position(|h| h == "spin_estimated")
                .expect("column");
            for (shot, flagged) in shots.iter().zip([true, false]) {
                let line: serde_json::Value = serde_json::from_str(&jsonl_line(
                    shot,
                    "t",
                    Some(system),
                    false,
                    &FlighthookConfig::default(),
                ))
                .expect("parses");
                // The JSONL line carries the assumed spin it came from.
                let assumed = line["data"]["flights"][0]["flight"].get("assumed");
                assert_eq!(assumed.is_some(), flagged, "{line}");
                let row = to_csv_row(shot, system, false, &FlighthookConfig::default());
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
        writer.apply(&flightrelay::GolferSelect {
            mode: Some(flighthook::Mode::Full),
            ..flightrelay::GolferSelect::new("test")
        });
        for (n, actor) in [(1, "mevo.0"), (2, flighthook::AUTO_CHIP_ACTOR)] {
            let key = ShotKey {
                shot_id: format!("shot-{n}"),
                shot_number: n,
            };
            feed(
                &state,
                actor,
                vec![
                    Ev::trigger(key.clone()),
                    ball_events(&key)[0].clone(),
                    ball_events(&key)[1].clone(),
                    Ev::Finished(key, None),
                ],
            )
            .await;
        }
        let shots = state.shots.read().await.clone();
        let modes: Vec<serde_json::Value> = shots
            .iter()
            .map(|shot| {
                let line: serde_json::Value = serde_json::from_str(&jsonl_line(
                    shot,
                    "t",
                    None,
                    false,
                    &FlighthookConfig::default(),
                ))
                .expect("parses");
                line["settings"]["detection_mode"].clone()
            })
            .collect();
        assert_eq!(modes, ["full", "chipping"]);
        let at = csv_header(UnitSystem::Imperial, false)
            .trim_end()
            .split(',')
            .position(|h| h == "detection_mode")
            .expect("column");
        let cells: Vec<String> = shots
            .iter()
            .map(|shot| {
                to_csv_row(
                    shot,
                    UnitSystem::Imperial,
                    false,
                    &FlighthookConfig::default(),
                )
                .trim_end()
                .split(',')
                .nth(at)
                .unwrap_or_default()
                .to_owned()
            })
            .collect();
        assert_eq!(cells, ["full", "chipping"]);
    }

    const BLOCK: &str = "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7";

    fn blocked_shot() -> StoredShot {
        let mut shot = private_shot();
        shot.data.shot_id = Some(shot.key.shot_id.clone());
        shot.data.turn = Some(flighthook::GolferTurn {
            golfer: None,
            club: Some(flighthook::Club::Iron7),
            handedness: None,
            mode: None,
        });
        shot.data.block = Some(BLOCK.into());
        shot
    }

    fn header(golfer: bool) -> Vec<String> {
        csv_header(UnitSystem::Imperial, golfer)
            .trim_end()
            .split(',')
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn csv_block_column_follows_club_selected() {
        let header = header(false);
        let at = |name: &str| header.iter().position(|h| h == name).expect(name);
        assert_eq!(at("block"), at("club_selected") + 1);
        assert_eq!(at("handed"), at("block") + 1);
        assert!(!header.iter().any(|h| h == "golfer_id" || h == "group"));

        let row = to_csv_row(
            &blocked_shot(),
            UnitSystem::Imperial,
            false,
            &FlighthookConfig::default(),
        );
        let cells: Vec<&str> = row.trim_end().split(',').collect();
        assert_eq!(cells.len(), header.len());
        assert_eq!(cells[at("club_selected")], "7I");
        assert_eq!(cells[at("block")], BLOCK);
    }

    #[test]
    fn jsonl_line_carries_the_block() {
        let shot = blocked_shot();
        let line = jsonl_line(&shot, "t", None, false, &FlighthookConfig::default());
        let v: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(v["schema"], 4);
        assert_eq!(v["block"], BLOCK);
        assert_eq!(v["data"]["block"], BLOCK);
        assert_eq!(v["data"]["shot_id"], v["shot_id"]);
        assert_eq!(v["data"]["turn"]["club"], v["club_selected"]);
        for absent in ["number", "mode", "session", "group", "golfer_id"] {
            assert!(v.get(absent).is_none(), "{absent}");
        }
        // Top level: `block` right after `club_selected`, before `settings`.
        let pair = format!(r#""club_selected":"7I","block":"{BLOCK}","settings""#);
        assert!(line.contains(&pair), "{line}");

        let unblocked = jsonl_line(
            &private_shot(),
            "t",
            None,
            false,
            &FlighthookConfig::default(),
        );
        let v: serde_json::Value = serde_json::from_str(&unblocked).expect("json");
        assert!(v.get("block").is_none());
    }

    /// A shot hit in a golfer's turn names the golfer: `golfer_id` in the
    /// JSONL line, and a trailing CSV column when a shot has one. The hand
    /// comes from the turn.
    #[test]
    fn a_golfers_shot_carries_golfer_id_and_the_turns_hand() {
        let mut shot = blocked_shot();
        let golfer: flightrelay::GolferId = "pureplay.local-3".parse().expect("id");
        shot.data.turn = Some(flightrelay::GolferTurn::new(
            Some(golfer),
            Some(flighthook::Club::Iron7),
            flighthook::Handedness::Left,
            flighthook::Mode::Full,
        ));
        let v: serde_json::Value = serde_json::from_str(&jsonl_line(
            &shot,
            "t",
            None,
            false,
            &FlighthookConfig::default(),
        ))
        .expect("json");
        assert_eq!(v["golfer_id"], "pureplay.local-3");
        assert_eq!(v["data"]["turn"]["golfer"], "pureplay.local-3");
        assert_eq!(v["settings"]["handed"], "lh");

        assert!(any_golfer([&shot]));
        assert!(!any_golfer([&blocked_shot()]));
        let header = header(true);
        assert_eq!(header.last().map(String::as_str), Some("golfer_id"));
        let row = to_csv_row(
            &shot,
            UnitSystem::Imperial,
            true,
            &FlighthookConfig::default(),
        );
        let cells: Vec<&str> = row.trim_end().split(',').collect();
        assert_eq!(cells.len(), header.len());
        assert_eq!(cells.last(), Some(&"pureplay.local-3"));
        let none = to_csv_row(
            &blocked_shot(),
            UnitSystem::Imperial,
            true,
            &FlighthookConfig::default(),
        );
        assert_eq!(none.trim_end().split(',').next_back(), Some(""));
    }

    /// `golfer_id` is the `flighthook.*` id of the golfer whose
    /// identifiers contain the turn's golfer, in JSONL and CSV; the turn in
    /// `data` keeps the raw id.
    #[test]
    fn golfer_id_names_the_golfer_by_the_lookup_rule() {
        let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
        eric.identifiers.push("pureplay.local-3".into());
        let golfers = FlighthookConfig {
            golfers: vec![eric],
            ..FlighthookConfig::default()
        };
        let mut shot = blocked_shot();
        shot.data.turn = Some(flightrelay::GolferTurn::new(
            Some("pureplay.local-3".parse().expect("id")),
            Some(flighthook::Club::Iron7),
            flighthook::Handedness::Right,
            flighthook::Mode::Full,
        ));
        let v: serde_json::Value =
            serde_json::from_str(&jsonl_line(&shot, "t", None, false, &golfers)).expect("json");
        assert_eq!(v["golfer_id"], "flighthook.e");
        assert_eq!(v["data"]["turn"]["golfer"], "pureplay.local-3");
        let row = to_csv_row(&shot, UnitSystem::Imperial, true, &golfers);
        assert_eq!(row.trim_end().split(',').next_back(), Some("flighthook.e"));
        // Its own id is itself; an id no golfer lists stays as it is.
        for (raw, out) in [
            ("flighthook.e", "flighthook.e"),
            ("backnine.p7", "backnine.p7"),
        ] {
            if let Some(turn) = shot.data.turn.as_mut() {
                turn.golfer = Some(raw.parse().expect("id"));
            }
            let line = build_export_line(&shot, "t", None, false, &golfers);
            assert_eq!(line.golfer_id.as_deref(), Some(out));
        }
    }
}
