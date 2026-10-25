//! Shot export: raw device frames captured per shot, and the export line.
//!
//! `GET /api/shots/export?format=jsonl` returns one [`ShotExport`] per line.
//! Each line holds the decoded shot exactly as `/api/shots` returns it, the
//! settings in effect when it was hit, and the raw device frames behind it
//! ([`ShotRaw`]). Device identity is limited to kind, maker, model and
//! firmware: no serial numbers, network names or addresses.

use serde::{Deserialize, Serialize};

use crate::{Club, Handedness, ShotData, ShotDetectionMode, SimulationSection};

/// Export format version. Bumped when a field changes meaning.
pub const EXPORT_SCHEMA: u32 = 1;

/// Direction of a raw frame relative to flighthook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawDir {
    /// Device → flighthook.
    Rx,
    /// flighthook → device.
    Tx,
}

/// One wire frame captured during a shot. Hex-first: `hex` (or `text`, for
/// JSON protocols) is the canonical content; every other field is metadata
/// that helps place it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRecord {
    /// Milliseconds relative to the shot trigger (negative = before it).
    pub t_ms: i64,
    pub dir: RawDir,
    /// Protocol-specific message type, e.g. `"0xD4"` (FlightScope), or the
    /// first byte of a BLE notification, e.g. `"0x11"`. Absent for JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// FlightScope bus address of the sender: `"AVR"`, `"DSP"`, `"PI"`, `"APP"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    /// FlightScope bus address of the receiver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dst: Option<String>,
    /// Channel for non-bus transports: BLE `"notify"` / `"cmd"` / `"write"` /
    /// `"register"`, TCP `"tcp"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Payload bytes as lowercase hex, no separators (binary protocols).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
    /// Payload text (JSON protocols).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl RawRecord {
    /// A binary frame with no metadata beyond its direction. `t_ms` is filled
    /// in by whoever places the frame in time.
    #[must_use]
    pub fn binary(dir: RawDir, bytes: &[u8]) -> Self {
        Self {
            t_ms: 0,
            dir,
            r#type: None,
            src: None,
            dst: None,
            channel: None,
            hex: Some(hex_lower(bytes)),
            text: None,
        }
    }

    /// A text frame (JSON protocols) with no metadata beyond its direction.
    #[must_use]
    pub fn text(dir: RawDir, text: impl Into<String>) -> Self {
        Self {
            t_ms: 0,
            dir,
            r#type: None,
            src: None,
            dst: None,
            channel: None,
            hex: None,
            text: Some(text.into()),
        }
    }

    /// Payload size in bytes: hex bytes plus text bytes.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        self.hex.as_ref().map_or(0, |h| h.len() / 2) + self.text.as_ref().map_or(0, String::len)
    }
}

/// Lowercase hex, no separators.
#[must_use]
pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Raw frames behind one shot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ShotRaw {
    /// `"flightscope-tcp"`, `"square-ble"`, `"r10-ble"`, `"openconnect-json"`,
    /// `"none"`.
    pub transport: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<RawRecord>,
    /// Frames that fell outside the per-shot cap and were not kept.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u32,
    /// Why `frames` is empty or incomplete when it should not be, e.g.
    /// `"mock device has no wire protocol"` or `"evicted: session raw budget"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Device identity safe to publish: kind, maker, model, firmware. Never a
/// serial, network name, BLE address or IP.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportDevice {
    /// Stable device kind from the actor type: `"flightscope"`,
    /// `"square_golf"`, `"garmin_r10"`, `"openconnect"`, `"mock"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
}

/// Settings in effect when the shot was hit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportSettings {
    /// The actor's config section with `name`, `address` and `bind` removed.
    pub device: serde_json::Value,
    pub simulation: SimulationSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_mode: Option<ShotDetectionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed: Option<Handedness>,
}

/// One JSONL line of `GET /api/shots/export?format=jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShotExport {
    /// Export format version ([`EXPORT_SCHEMA`]).
    pub schema: u32,
    pub flighthook_version: String,
    /// RFC 3339 UTC, host clock, when the export was produced.
    pub exported_at: String,
    /// RFC 3339 UTC, host clock, when `shot_trigger` reached the web layer.
    pub time: String,
    /// Actor ID (`"mevo.0"`). Not a device identity.
    pub actor: String,
    pub shot_id: String,
    pub shot_number: u32,
    pub device: ExportDevice,
    /// Club in play from game state (the simulator's selection), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club_selected: Option<Club>,
    pub settings: ExportSettings,
    /// Exactly the `/api/shots` element for this shot (same serde shape).
    /// Shots the device produced no data for have only `actor` and
    /// `shot_number`.
    pub data: ShotData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<ShotRaw>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BallFlight, Velocity};

    fn record() -> RawRecord {
        RawRecord {
            t_ms: -3,
            dir: RawDir::Rx,
            r#type: Some("0xD4".into()),
            src: Some("DSP".into()),
            dst: Some("APP".into()),
            channel: None,
            hex: Some(hex_lower(&[0x9C, 0x00, 0xAB])),
            text: None,
        }
    }

    fn export(raw: Option<ShotRaw>) -> ShotExport {
        ShotExport {
            schema: EXPORT_SCHEMA,
            flighthook_version: "0.0.0".into(),
            exported_at: "2026-01-01T00:00:01.000Z".into(),
            time: "2026-01-01T00:00:00.000Z".into(),
            actor: "mevo.0".into(),
            shot_id: "abc".into(),
            shot_number: 3,
            device: ExportDevice {
                kind: "flightscope".into(),
                manufacturer: Some("FlightScope".into()),
                model: None,
                firmware: None,
            },
            club_selected: Some(Club::Iron7),
            settings: ExportSettings {
                device: serde_json::json!({"ball_type": 1}),
                simulation: SimulationSection::default(),
                detection_mode: Some(ShotDetectionMode::Full),
                handed: None,
            },
            data: ShotData {
                actor: "mevo.0".into(),
                shot_number: 3,
                ball: Some(BallFlight {
                    launch_speed: Some(Velocity::MetersPerSecond(40.0)),
                    ..BallFlight::default()
                }),
                club: None,
                impact: None,
                derived: None,
            },
            raw,
        }
    }

    #[test]
    fn schema_is_one() {
        assert_eq!(EXPORT_SCHEMA, 1);
        assert_eq!(export(None).schema, 1);
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex_lower(&[0xAB, 0x0F, 0x00]), "ab0f00");
        let json = serde_json::to_value(record()).expect("ser");
        assert_eq!(json["hex"], "9c00ab");
    }

    #[test]
    fn raw_record_round_trips_and_skips_none() {
        let rec = record();
        let json = serde_json::to_string(&rec).expect("ser");
        assert_eq!(
            json,
            r#"{"t_ms":-3,"dir":"rx","type":"0xD4","src":"DSP","dst":"APP","hex":"9c00ab"}"#
        );
        let back: RawRecord = serde_json::from_str(&json).expect("de");
        assert_eq!(back, rec);

        let text = RawRecord::text(RawDir::Tx, "{}");
        let json = serde_json::to_value(&text).expect("ser");
        assert_eq!(
            json,
            serde_json::json!({"t_ms": 0, "dir": "tx", "text": "{}"})
        );
    }

    #[test]
    fn shot_raw_skips_defaults() {
        let raw = ShotRaw {
            transport: "none".into(),
            ..ShotRaw::default()
        };
        assert_eq!(
            serde_json::to_string(&raw).expect("ser"),
            r#"{"transport":"none"}"#
        );
        let full = ShotRaw {
            transport: "flightscope-tcp".into(),
            frames: vec![record()],
            dropped: 2,
            unavailable: Some("why".into()),
        };
        let back: ShotRaw =
            serde_json::from_str(&serde_json::to_string(&full).expect("ser")).expect("de");
        assert_eq!(back, full);
        assert_eq!(full.frames[0].payload_len(), 3);
    }

    #[test]
    fn shot_export_round_trips() {
        for raw in [
            None,
            Some(ShotRaw {
                transport: "flightscope-tcp".into(),
                frames: vec![record()],
                ..ShotRaw::default()
            }),
        ] {
            let line = export(raw);
            let json = serde_json::to_string(&line).expect("ser");
            let back: ShotExport = serde_json::from_str(&json).expect("de");
            assert_eq!(back, line);
            let v: serde_json::Value = serde_json::from_str(&json).expect("value");
            assert_eq!(v["raw"].is_null(), line.raw.is_none());
            assert_eq!(v["club_selected"], "7I");
            assert!(v["device"].get("model").is_none());
            assert!(v["settings"].get("handed").is_none());
        }
    }
}
