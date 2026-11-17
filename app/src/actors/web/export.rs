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
            // dashboard offers Auto-chip and the flight model's actor id
            // are no part of them.
            simulation: flighthook::SimulationSection {
                auto_chip: false,
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
        i(b.output_backspin()),
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
mod tests;
