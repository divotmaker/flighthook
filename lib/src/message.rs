//! Unified `FlighthookMessage` bus types.
//!
//! All events flow through a single `broadcast<FlighthookMessage>` channel.
//! Each message has an actor ID, optional device (FRP physical unit ID),
//! optional raw payload (hex-first policy), and a typed event.
//!
//! ## Wire format (FRP envelope)
//!
//! All messages use the FRP envelope shape with `kind` inside `event`:
//! ```json
//! { "actor": "mevo.0", "device": "FS-M2-XXXXXX", "event": { "kind": "ball_flight", ... } }
//! ```
//!
//! `actor` is a flighthook extension (actor framework ID). `device` is the
//! FRP device identifier (present on device events, absent on extensions).
//! FRP consumers silently ignore unknown `kind` values and extra fields per spec.
//!
//! FRP-compliant events: `ShotTrigger`, `BallFlight`, `ClubPath`, `FaceImpact`,
//! `ShotFinished`, `DeviceTelemetry`, `Alert`, `SetDetectionMode`.
//! Flighthook extensions: everything else.

use std::fmt;

use serde::{Deserialize, Serialize, Serializer};

use crate::{Handedness, ShotDetectionMode};
use std::collections::HashMap;

use crate::{ActorStatus, BallFlight, ClubData, DerivedFlight, FaceImpact, ShotRaw};
use crate::{
    BodyTrackingSection, CameraPosition, CameraSection, ExPuttSection, FlighthookConfig,
    MevoSection, MockMonitorSection, OpenConnectClientSection, OpenConnectServerSection,
    PlayerSection, PureplaySection, R10Section, RandomClubSection, SquareSection, WebserverSection,
};
use crate::{Club, ClubInfo, GroupedShot, HandInfo, PlayerInfo, ShotGroupInfo, ShotGrouping};

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

// ---------------------------------------------------------------------------
// Top-level message
// ---------------------------------------------------------------------------

/// A single event on the unified bus.
///
/// Uses the FRP envelope shape: `{ actor, device?, event: { kind, ... } }`.
/// `actor` is a flighthook extension field. FRP consumers ignore unknown fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlighthookMessage {
    /// Actor ID of the originator (e.g. "mevo.0", "openconnect_client.0", "system").
    #[serde(default)]
    pub actor: String,
    /// FRP device identifier for the physical unit (e.g. the Mevo SSID).
    /// Present on shot lifecycle and device telemetry events; absent on
    /// system/config/actor-status events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_payload: Option<RawPayload>,
    pub event: FlighthookEvent,
}

#[cfg(not(target_arch = "wasm32"))]
impl FlighthookMessage {
    /// Create a new message. Use `.actor()`, `.device()`, and
    /// `.raw()` / `.raw_binary()` to attach metadata.
    pub fn new(event: impl Into<FlighthookEvent>) -> Self {
        Self {
            actor: String::new(),
            device: None,
            raw_payload: None,
            event: event.into(),
        }
    }

    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = actor.into();
        self
    }

    pub fn device(mut self, device: impl Into<String>) -> Self {
        self.device = Some(device.into());
        self
    }

    pub fn raw(mut self, raw: RawPayload) -> Self {
        self.raw_payload = Some(raw);
        self
    }

    pub fn raw_binary(mut self, raw: Vec<u8>) -> Self {
        self.raw_payload = Some(RawPayload::Binary(raw));
        self
    }
}

// ---------------------------------------------------------------------------
// Raw payload — hex-first policy
// ---------------------------------------------------------------------------

/// Raw wire data attached to a bus message.
///
/// Binary payloads serialize as hex strings (no spaces, lowercase).
/// Text payloads (e.g. GSPro JSON) serialize as-is.
#[derive(Debug, Clone)]
pub enum RawPayload {
    Binary(Vec<u8>),
    Text(String),
}

impl Serialize for RawPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            RawPayload::Binary(bytes) => {
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                serializer.serialize_str(&hex)
            }
            RawPayload::Text(s) => serializer.serialize_str(s),
        }
    }
}

impl<'de> Deserialize<'de> for RawPayload {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(RawPayload::Text(s))
    }
}

impl fmt::Display for RawPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RawPayload::Binary(bytes) => {
                for b in bytes {
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
            RawPayload::Text(s) => write!(f, "{s}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Event variants
// ---------------------------------------------------------------------------

/// Re-export flightrelay's ShotKey as the canonical shot correlation type.
pub use flightrelay::ShotKey;

/// The typed event payload carried by a `FlighthookMessage`.
///
/// FRP-compliant events: `ShotTrigger`, `BallFlight`, `ClubPath`, `FaceImpact`,
/// `ShotFinished`, `DeviceTelemetry`, `Alert`.
///
/// Flighthook extensions: everything else. FRP-only consumers silently ignore
/// unknown `kind` values per spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlighthookEvent {
    // -- FRP: Shot lifecycle (correlated by ShotKey + device) --
    /// Ball strike detected. Emitted immediately — no data yet.
    ShotTrigger { key: ShotKey },
    /// Ball flight data available.
    ///
    /// `ball` is exactly what the device measured. `derived` carries values
    /// flighthook simulated for fields the device did not report (beta); it
    /// is a flighthook extension field, which FRP consumers ignore.
    BallFlight {
        key: ShotKey,
        ball: Box<BallFlight>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        derived: Option<Box<DerivedFlight>>,
    },
    /// Club path data available.
    ClubPath { key: ShotKey, club: Box<ClubData> },
    /// Face impact location available.
    FaceImpact {
        key: ShotKey,
        impact: Box<FaceImpact>,
    },
    /// Shot sequence complete. Accumulators should finalize.
    ///
    /// `raw` is a flighthook extension: the device frames captured for this
    /// shot, when the actor captures them. Omitted on the wire when absent, so
    /// the FRP shape is unchanged.
    ShotFinished {
        key: ShotKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw: Option<Box<ShotRaw>>,
    },

    // -- FRP: Device status --
    /// Device telemetry — emitted any time a device-reported value changes.
    ///
    /// Carries device-reported state: identity (manufacturer, model, firmware)
    /// and telemetry (ready, battery_pct, tilt, roll, temp_c, external_power).
    /// `ready` is the single readiness signal (all conditions met for a shot).
    ///
    /// Actor/connection lifecycle (status enum, mode, shooting) is conveyed
    /// separately via `ActorStatus`. The two variants have no overlapping keys.
    DeviceTelemetry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manufacturer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        firmware: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        telemetry: Option<HashMap<String, String>>,
    },
    /// Alert for warn/error/critical conditions.
    Alert {
        severity: flightrelay::Severity,
        message: String,
    },

    // -- FRP: Controller commands --
    /// Set the shot detection mode, handedness and/or club on the device.
    ///
    /// All fields are optional and latched independently — the most recent
    /// value for each field is the active value. Omitting a field does not
    /// reset it. `club` is a hint for the next shot and never implies `mode`.
    SetDetectionMode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<ShotDetectionMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handed: Option<Handedness>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        club: Option<Club>,
    },

    // -- Flighthook extensions --
    /// Player info update (name).
    PlayerInfo { player_info: PlayerInfo },
    /// Club selection update.
    ClubInfo { club_info: ClubInfo },
    /// Player handedness update.
    HandInfo { hand_info: HandInfo },
    /// Config mutation request (emitted by POST handler, processed by SystemActor).
    ConfigCommand {
        /// Opaque correlation ID for request-reply pattern.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        action: Box<ConfigAction>,
    },
    /// Config mutation outcome (emitted by SystemActor after processing).
    ConfigOutcome {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restarted: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stopped: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        started: Vec<String>,
    },
    /// Command: start a new set (shot group); the next shot joins it. The
    /// web layer acts on it.
    NewShotGroup,
    /// Command: close every open set and start a new session; the next shot
    /// starts a new set. The web layer acts on it.
    NewShotSession,
    /// Command: delete the shot history and every set, and start a new
    /// session. Every web layer clears its history, and every UI that sees
    /// it clears its shot list.
    ClearShotHistory,
    /// Command: the ExPutt actor `target` (e.g. `exputt.0`) has its device
    /// locate the mat again. Emitted by `POST /api/find-mat`.
    FindMat { target: String },
    /// A shot group record changed: created, a shot joined, closed, or a shot
    /// left the history. Emitted by the web layer.
    ShotGroup {
        group: ShotGroupInfo,
        /// The shot that joined, when the change is a join.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shot: Option<GroupedShot>,
    },
    /// A camera finished a clip of a shot (beta). Emitted by the camera
    /// actor (the envelope's `actor`), once per clip; the clip itself is
    /// served over HTTP, never on the bus. `keys` are every shot the clip
    /// belongs to: one per launch monitor that saw the swing.
    ShotVideo {
        keys: Vec<ShotKey>,
        position: CameraPosition,
        duration_ms: u64,
        fps: f64,
        trigger_offset_ms: i64,
        impact_offset_ms: i64,
        complete: bool,
        dropped: u32,
    },
    /// The clips of these shots were deleted to keep the newest
    /// `[replay] keep_shots` (beta).
    ShotVideoRemoved { keys: Vec<ShotKey> },
    /// Body tracking of a camera's clip is done (alpha): its keypoints are
    /// served beside the clip. The envelope's `actor` is the camera, as for
    /// `shot_video`; `keys` are the clip's shots; `rotation` is the camera's
    /// rotation, clockwise degrees, the clip was tracked with (omitted at
    /// 0). Never per-frame data.
    ShotPose {
        keys: Vec<ShotKey>,
        #[serde(default, skip_serializing_if = "is_zero_u16")]
        rotation: u16,
    },
    /// The swing analysis of a camera's clip is done (alpha): events, tempo
    /// and metrics are served beside the clip. The envelope's `actor` is
    /// the camera; `keys` are the clip's shots. Never per-frame data.
    ShotSwing { keys: Vec<ShotKey> },
    /// Actor/connection lifecycle update.
    ///
    /// Carries actor-framework state: connection status enum and actor-specific
    /// telemetry (detection_mode, radar_mode, shot_count, tracking_mode, device_info label).
    ///
    /// Device-reported state (ready, battery, tilt, temp) is conveyed
    /// separately via `DeviceTelemetry`. The two variants have no overlapping keys.
    ActorStatus {
        status: ActorStatus,
        #[serde(default)]
        telemetry: HashMap<String, String>,
    },
}

impl FlighthookEvent {
    /// Returns true if this is an ActorStatus event containing telemetry
    /// (battery_pct key in state map). Used for heartbeat filtering in audit.
    pub fn is_actor_status_with_telemetry(&self) -> bool {
        matches!(self, FlighthookEvent::ActorStatus { telemetry, .. } if telemetry.contains_key("battery_pct"))
    }
}

// ---------------------------------------------------------------------------
// ConfigAction — specific config mutations
// ---------------------------------------------------------------------------

/// The specific config mutation to apply. New actions may be added.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConfigAction {
    /// Replace the entire config. Used by POST /api/settings.
    ///
    /// Boxed so this variant does not set the size of every `ConfigAction`:
    /// the per-section upserts are an order of magnitude smaller and far more
    /// frequent. `Box` is transparent to serde, so the wire format is a plain
    /// config object either way.
    ReplaceAll {
        config: Box<FlighthookConfig>,
    },
    /// Replace the global settings (everything but the actor sections:
    /// units, chipping and putting clubs, default shot view, simulation,
    /// replay)
    /// with `config`'s, keeping every actor section of the live config.
    /// Used by POST /api/settings?scope=global.
    ReplaceGlobal {
        config: Box<FlighthookConfig>,
    },
    /// Per-section upserts.
    UpsertWebserver {
        index: String,
        section: WebserverSection,
    },
    UpsertMevo {
        index: String,
        section: MevoSection,
    },
    UpsertR10 {
        index: String,
        section: R10Section,
    },
    UpsertSquare {
        index: String,
        section: SquareSection,
    },
    #[serde(rename = "upsert_exputt")]
    UpsertExPutt {
        index: String,
        section: ExPuttSection,
    },
    UpsertOpenConnectServer {
        index: String,
        section: OpenConnectServerSection,
    },
    /// Also accepted as `upsert_gs_pro`.
    #[serde(alias = "upsert_gs_pro")]
    UpsertOpenConnectClient {
        index: String,
        section: OpenConnectClientSection,
    },
    UpsertPureplay {
        index: String,
        section: PureplaySection,
    },
    UpsertMockMonitor {
        index: String,
        section: MockMonitorSection,
    },
    UpsertRandomClub {
        index: String,
        section: RandomClubSection,
    },
    UpsertCamera {
        index: String,
        section: CameraSection,
    },
    /// Remove a section by global ID ("mevo.0", "openconnect_client.1",
    /// "webserver.0"). A `gspro.N` ID names `openconnect_client.N`.
    Remove {
        id: String,
    },
    /// Set the default shot view (`shot_grouping`, persisted). Sets are
    /// assigned the same way in every view.
    SetShotGrouping {
        mode: ShotGrouping,
    },
    /// Replace the `[body_tracking]` settings alone (persisted). Used by
    /// POST /api/settings?scope=body_tracking.
    SetBodyTracking {
        section: BodyTrackingSection,
    },
    /// Replace the `[player]` settings alone (persisted). Used by
    /// POST /api/settings?scope=player.
    SetPlayer {
        section: PlayerSection,
    },
    /// Stop the launch monitor or output `id` ("mevo.0",
    /// "openconnect_client.1"), wait for its connection to close, and start
    /// it again from the saved settings. The outcome lists it as restarted,
    /// or nothing when it was not.
    Reconnect {
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exputt_upsert_round_trips_as_upsert_exputt() {
        let action = ConfigAction::UpsertExPutt {
            index: "0".into(),
            section: ExPuttSection {
                name: "ExPutt".into(),
                address: Some("192.168.1.50".into()),
                impact_lead_s: None,
            },
        };
        let json = serde_json::to_string(&action).expect("ser");
        assert_eq!(
            json,
            r#"{"type":"upsert_exputt","index":"0","section":{"name":"ExPutt","address":"192.168.1.50"}}"#
        );
        let back: ConfigAction = serde_json::from_str(&json).expect("de");
        assert!(
            matches!(back, ConfigAction::UpsertExPutt { section, .. } if section.address.is_some())
        );
    }

    #[test]
    fn device_event_envelope() {
        let msg = FlighthookMessage {
            actor: "mevo.0".into(),
            device: Some("FS-M2-XXXXXX".into()),
            raw_payload: None,
            event: FlighthookEvent::DeviceTelemetry {
                manufacturer: Some("FlightScope".into()),
                model: None,
                firmware: None,
                telemetry: None,
            },
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["event"]["kind"], "device_telemetry");
        assert_eq!(json["actor"], "mevo.0");
        assert_eq!(json["device"], "FS-M2-XXXXXX");
    }

    #[test]
    fn non_device_event_envelope() {
        let msg = FlighthookMessage {
            actor: "webserver.0".into(),
            device: None,
            raw_payload: None,
            event: FlighthookEvent::ActorStatus {
                status: crate::ActorStatus::Connected,
                telemetry: HashMap::from([("bind".into(), "0.0.0.0:5880".into())]),
            },
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        // kind inside event even without device
        assert_eq!(json["event"]["kind"], "actor_status");
        assert_eq!(json["actor"], "webserver.0");
        assert!(json.get("device").is_none());
    }

    #[test]
    fn roundtrip_with_device() {
        let msg = FlighthookMessage {
            actor: "mevo.0".into(),
            device: Some("FS-M2-XXXXXX".into()),
            raw_payload: None,
            event: FlighthookEvent::ShotFinished {
                key: ShotKey {
                    shot_id: "abc".into(),
                    shot_number: 1,
                },
                raw: None,
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.actor, "mevo.0");
        assert_eq!(back.device.as_deref(), Some("FS-M2-XXXXXX"));
        assert!(matches!(back.event, FlighthookEvent::ShotFinished { .. }));
    }

    #[test]
    fn roundtrip_without_device() {
        let msg = FlighthookMessage {
            actor: "system".into(),
            device: None,
            raw_payload: None,
            event: FlighthookEvent::ActorStatus {
                status: crate::ActorStatus::Connected,
                telemetry: HashMap::new(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.actor, "system");
        assert!(back.device.is_none());
        assert!(matches!(
            back.event,
            FlighthookEvent::ActorStatus {
                status: crate::ActorStatus::Connected,
                ..
            }
        ));
    }

    fn envelope(event: FlighthookEvent) -> FlighthookMessage {
        FlighthookMessage {
            actor: "web".into(),
            device: None,
            raw_payload: None,
            event,
        }
    }

    #[test]
    fn group_commands_are_bare_tags() {
        for (event, kind) in [
            (FlighthookEvent::NewShotGroup, "new_shot_group"),
            (FlighthookEvent::NewShotSession, "new_shot_session"),
            (FlighthookEvent::ClearShotHistory, "clear_shot_history"),
        ] {
            let json = serde_json::to_string(&envelope(event)).unwrap();
            assert_eq!(
                json,
                format!(r#"{{"actor":"web","event":{{"kind":"{kind}"}}}}"#)
            );
            let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
            let again = serde_json::to_string(&back).unwrap();
            assert_eq!(again, json);
        }
        let back: FlighthookMessage =
            serde_json::from_str(r#"{"actor":"web","event":{"kind":"new_shot_group"}}"#).unwrap();
        assert!(matches!(back.event, FlighthookEvent::NewShotGroup));
        let back: FlighthookMessage =
            serde_json::from_str(r#"{"actor":"web","event":{"kind":"new_shot_session"}}"#).unwrap();
        assert!(matches!(back.event, FlighthookEvent::NewShotSession));
        let back: FlighthookMessage =
            serde_json::from_str(r#"{"actor":"web","event":{"kind":"clear_shot_history"}}"#)
                .unwrap();
        assert!(matches!(back.event, FlighthookEvent::ClearShotHistory));
    }

    #[test]
    fn hand_info_round_trips() {
        let event = FlighthookEvent::HandInfo {
            hand_info: HandInfo {
                handed: Handedness::Left,
            },
        };
        let json = serde_json::to_string(&envelope(event)).unwrap();
        assert_eq!(
            json,
            r#"{"actor":"web","event":{"kind":"hand_info","hand_info":{"handed":"lh"}}}"#
        );
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back.event,
            FlighthookEvent::HandInfo {
                hand_info: HandInfo {
                    handed: Handedness::Left
                }
            }
        ));
    }

    /// The JSON of the `event` member of the bus message carrying `event`,
    /// in wire order.
    fn event_json(event: FlighthookEvent) -> String {
        let json = serde_json::to_string(&envelope(event)).unwrap();
        json.strip_prefix(r#"{"actor":"web","event":"#)
            .and_then(|rest| rest.strip_suffix('}'))
            .expect("actor, then event")
            .to_owned()
    }

    #[test]
    fn set_detection_mode_without_club_is_unchanged() {
        assert_eq!(
            event_json(FlighthookEvent::SetDetectionMode {
                mode: Some(ShotDetectionMode::Putting),
                handed: None,
                club: None,
            }),
            r#"{"kind":"set_detection_mode","mode":"putting"}"#
        );
        assert_eq!(
            event_json(FlighthookEvent::SetDetectionMode {
                mode: Some(ShotDetectionMode::Full),
                handed: Some(Handedness::Left),
                club: None,
            }),
            r#"{"kind":"set_detection_mode","mode":"full","handed":"lh"}"#
        );
        assert_eq!(
            event_json(FlighthookEvent::SetDetectionMode {
                mode: None,
                handed: Some(Handedness::Right),
                club: None,
            }),
            r#"{"kind":"set_detection_mode","handed":"rh"}"#
        );
    }

    /// The spec's `set_detection_mode` example, member for member.
    #[test]
    fn set_detection_mode_with_club_matches_the_frp_example() {
        let json = event_json(FlighthookEvent::SetDetectionMode {
            mode: Some(ShotDetectionMode::Chipping),
            handed: Some(Handedness::Right),
            club: Some(Club::SandWedge),
        });
        assert_eq!(
            json,
            r#"{"kind":"set_detection_mode","mode":"chipping","handed":"rh","club":"SW"}"#
        );

        // An FRP client parses what flighthook puts on the wire, club included.
        let parsed = flightrelay::FrpMessage::parse(&json).unwrap();
        assert_eq!(
            parsed,
            flightrelay::FrpMessage::Protocol(flightrelay::FrpProtocolMessage::SetDetectionMode {
                mode: Some(flightrelay::DetectionMode::Chipping),
                handed: Some(Handedness::Right),
                club: Some(flightrelay::Club::SandWedge),
            })
        );

        // A club alone leaves mode and hand to their latched values.
        let json = event_json(FlighthookEvent::SetDetectionMode {
            mode: None,
            handed: None,
            club: Some(Club::Iron7),
        });
        assert_eq!(json, r#"{"kind":"set_detection_mode","club":"7I"}"#);
        let back: FlighthookEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            FlighthookEvent::SetDetectionMode {
                mode: None,
                handed: None,
                club: Some(Club::Iron7)
            }
        ));
    }

    /// Every flighthook club code is an FRP club code and the other way
    /// round: 33 clubs, same code, same serde form.
    #[test]
    fn club_codes_match_flightrelay() {
        assert_eq!(Club::ALL.len(), 33);
        assert_eq!(flightrelay::Club::ALL.len(), Club::ALL.len());
        for &club in Club::ALL {
            let code = club.to_string();
            let frp: flightrelay::Club = code
                .parse()
                .unwrap_or_else(|e| panic!("{code} is not an FRP club: {e}"));
            assert_eq!(frp.to_string(), code);
            assert_eq!(
                serde_json::to_string(&frp).unwrap(),
                serde_json::to_string(&club).unwrap()
            );
            let back: Club = serde_json::from_str(&serde_json::to_string(&frp).unwrap()).unwrap();
            assert_eq!(back, club);
        }
        for &frp in flightrelay::Club::ALL {
            let club = Club::from_code(&frp.to_string())
                .unwrap_or_else(|| panic!("{frp} is not a flighthook club"));
            assert_eq!(club.to_string(), frp.to_string());
        }
    }

    #[test]
    fn find_mat_names_its_target() {
        let event = FlighthookEvent::FindMat {
            target: "exputt.0".into(),
        };
        let json = serde_json::to_string(&envelope(event)).unwrap();
        assert_eq!(
            json,
            r#"{"actor":"web","event":{"kind":"find_mat","target":"exputt.0"}}"#
        );
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.event, FlighthookEvent::FindMat { target } if target == "exputt.0"));
    }

    fn group_info() -> ShotGroupInfo {
        ShotGroupInfo {
            id: "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7".into(),
            number: 3,
            session: 0,
            golfer: None,
            club: Some(crate::Club::Iron7),
            started_at: "2026-10-26T13:58:02.117Z".into(),
            manual: false,
            open: true,
            shots: 10,
        }
    }

    #[test]
    fn shot_group_without_shot_round_trips() {
        let msg = FlighthookMessage {
            actor: "webserver.0".into(),
            ..envelope(FlighthookEvent::ShotGroup {
                group: group_info(),
                shot: None,
            })
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"actor":"webserver.0","event":{"kind":"shot_group","group":{"id":"6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7","number":3,"club":"7I","started_at":"2026-10-26T13:58:02.117Z","open":true,"shots":10}}}"#
        );
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        let FlighthookEvent::ShotGroup { group, shot } = back.event else {
            panic!("not shot_group");
        };
        assert_eq!(group, group_info());
        assert_eq!(shot, None);
    }

    #[test]
    fn shot_group_with_shot_round_trips() {
        let joined = GroupedShot {
            actor: "mevo.0".into(),
            key: ShotKey {
                shot_id: "550e8400".into(),
                shot_number: 12,
            },
        };
        let msg = envelope(FlighthookEvent::ShotGroup {
            group: group_info(),
            shot: Some(joined.clone()),
        });
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["event"]["kind"], "shot_group");
        assert_eq!(json["event"]["shot"]["actor"], "mevo.0");
        assert_eq!(json["event"]["shot"]["key"]["shot_number"], 12);
        let back: FlighthookMessage = serde_json::from_value(json).unwrap();
        let FlighthookEvent::ShotGroup { group, shot } = back.event else {
            panic!("not shot_group");
        };
        assert_eq!(group, group_info());
        assert_eq!(shot, Some(joined));
    }

    #[test]
    fn shot_video_round_trips_with_its_keys() {
        let keys = vec![
            ShotKey {
                shot_id: "a".into(),
                shot_number: 1,
            },
            ShotKey {
                shot_id: "b".into(),
                shot_number: 7,
            },
        ];
        let msg = FlighthookMessage {
            actor: "camera.0".into(),
            ..envelope(FlighthookEvent::ShotVideo {
                keys: keys.clone(),
                position: CameraPosition::TargetLine,
                duration_ms: 5500,
                fps: 119.5,
                trigger_offset_ms: 4000,
                impact_offset_ms: 2000,
                complete: false,
                dropped: 3,
            })
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"actor":"camera.0","event":{"kind":"shot_video","keys":[{"shot_id":"a","shot_number":1},{"shot_id":"b","shot_number":7}],"position":"target_line","duration_ms":5500,"fps":119.5,"trigger_offset_ms":4000,"impact_offset_ms":2000,"complete":false,"dropped":3}}"#
        );
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        let FlighthookEvent::ShotVideo {
            keys: got,
            position,
            impact_offset_ms,
            ..
        } = back.event
        else {
            panic!("not shot_video");
        };
        assert_eq!(got, keys);
        assert_eq!(position, CameraPosition::TargetLine);
        assert_eq!(impact_offset_ms, 2000);

        let removed = envelope(FlighthookEvent::ShotVideoRemoved { keys: keys.clone() });
        let json = serde_json::to_string(&removed).unwrap();
        assert!(json.contains(r#""kind":"shot_video_removed""#), "{json}");
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.event, FlighthookEvent::ShotVideoRemoved { keys: k } if k == keys));
    }

    #[test]
    fn shot_pose_carries_its_rotation_only_when_turned() {
        let keys = vec![ShotKey {
            shot_id: "a".into(),
            shot_number: 1,
        }];
        let plain = serde_json::to_string(&FlighthookEvent::ShotPose {
            keys: keys.clone(),
            rotation: 0,
        })
        .unwrap();
        assert!(!plain.contains("rotation"), "{plain}");
        let turned = serde_json::to_string(&FlighthookEvent::ShotPose {
            keys: keys.clone(),
            rotation: 270,
        })
        .unwrap();
        assert!(turned.contains(r#""rotation":270"#), "{turned}");
        let back: FlighthookEvent = serde_json::from_str(&plain).unwrap();
        assert!(matches!(
            back,
            FlighthookEvent::ShotPose { rotation: 0, .. }
        ));
        let back: FlighthookEvent = serde_json::from_str(&turned).unwrap();
        assert!(matches!(
            back,
            FlighthookEvent::ShotPose { rotation: 270, .. }
        ));
    }

    #[test]
    fn set_player_round_trips() {
        let action = ConfigAction::SetPlayer {
            section: PlayerSection {
                sex: crate::Sex::Female,
                height: Some(170.0),
            },
        };
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(
            json,
            r#"{"type":"set_player","section":{"sex":"female","height":170.0}}"#
        );
        let back: ConfigAction = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ConfigAction::SetPlayer { section } if section.height == Some(170.0))
        );
    }

    #[test]
    fn upsert_camera_round_trips() {
        let action = ConfigAction::UpsertCamera {
            index: "1".into(),
            section: CameraSection {
                name: "Face".into(),
                camera: "test-pattern".into(),
                camera_name: None,
                position: CameraPosition::GolferFront,
                mode: "auto".into(),
                latency_s: None,
                rotation_deg: 0,
                yaw_deg: None,
                pitch_deg: None,
            },
        };
        let json = serde_json::to_string(&action).expect("ser");
        assert_eq!(
            json,
            r#"{"type":"upsert_camera","index":"1","section":{"name":"Face","camera":"test-pattern","position":"golfer_front","mode":"auto"}}"#
        );
        let back: ConfigAction = serde_json::from_str(&json).expect("de");
        assert!(matches!(back, ConfigAction::UpsertCamera { index, .. } if index == "1"));
    }

    #[test]
    fn set_body_tracking_round_trips() {
        let action = ConfigAction::SetBodyTracking {
            section: BodyTrackingSection {
                min_score: 0.5,
                ..BodyTrackingSection::default()
            },
        };
        let json = serde_json::to_string(&action).expect("ser");
        assert_eq!(
            json,
            r#"{"type":"set_body_tracking","section":{"min_score":0.5}}"#
        );
        let back: ConfigAction = serde_json::from_str(&json).expect("de");
        assert!(matches!(
            back,
            ConfigAction::SetBodyTracking { section } if section.min_score.to_bits() == 0.5f64.to_bits()
        ));
    }

    #[test]
    fn set_shot_grouping_command_round_trips() {
        let msg = envelope(FlighthookEvent::ConfigCommand {
            request_id: None,
            action: Box::new(ConfigAction::SetShotGrouping {
                mode: ShotGrouping::ByClub,
            }),
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"actor":"web","event":{"kind":"config_command","action":{"type":"set_shot_grouping","mode":"by_club"}}}"#
        );
        let back: FlighthookMessage = serde_json::from_str(&json).unwrap();
        let FlighthookEvent::ConfigCommand { action, .. } = back.event else {
            panic!("not config_command");
        };
        assert!(matches!(
            *action,
            ConfigAction::SetShotGrouping {
                mode: ShotGrouping::ByClub
            }
        ));
    }
}
