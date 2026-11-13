//! Config commands ([`ConfigAction`]) and the shot correlation key.

use serde::{Deserialize, Serialize};

use crate::ShotGrouping;
use crate::{
    BodyTrackingSection, CameraSection, ExPuttSection, FlighthookConfig, GolferRecord, MevoSection,
    MockMonitorSection, OpenConnectClientSection, OpenConnectServerSection,
    PureplayAccessorySection, PureplaySection, R10Section, RandomClubSection, SquareSection,
    TrackballSection, WebserverSection,
};

/// Re-export flightrelay's ShotKey as the canonical shot correlation type.
pub use flightrelay::ShotKey;

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
    UpsertTrackball {
        index: String,
        section: TrackballSection,
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
    UpsertPureplayAccessory {
        index: String,
        section: PureplayAccessorySection,
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
    /// Switch tournament mode on or off (`[simulation] tournament`,
    /// persisted). Used by POST /api/tournament; never refused.
    SetTournament {
        on: bool,
    },
    /// Replace the golfers alone (`[[golfers]]`, persisted). Used by
    /// POST /api/settings?scope=golfers.
    SetGolfers {
        golfers: Vec<GolferRecord>,
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
    use crate::{CameraPosition, ExPuttConfig};

    #[test]
    fn exputt_upsert_round_trips_as_upsert_exputt() {
        let action = ConfigAction::UpsertExPutt {
            index: "0".into(),
            section: ExPuttSection {
                actor_id: None,
                name: "ExPutt".into(),
                enabled: true,
                impact_lead_s: None,
                driver: ExPuttConfig {
                    address: Some("192.168.1.50".into()),
                },
            },
        };
        let json = serde_json::to_string(&action).expect("ser");
        assert_eq!(
            json,
            r#"{"type":"upsert_exputt","index":"0","section":{"name":"ExPutt","address":"192.168.1.50"}}"#
        );
        let back: ConfigAction = serde_json::from_str(&json).expect("de");
        assert!(
            matches!(back, ConfigAction::UpsertExPutt { section, .. } if section.driver.address.is_some())
        );
    }

    #[test]
    fn trackball_upsert_round_trips_as_upsert_trackball() {
        let mut section = TrackballSection::default();
        section.driver.device = Some("usb-Example_Trackball-event-mouse".into());
        section.driver.tuning.full_power_speed = 40.0;
        let action = ConfigAction::UpsertTrackball {
            index: "0".into(),
            section: section.clone(),
        };
        let json = serde_json::to_string(&action).expect("ser");
        assert_eq!(
            json,
            r#"{"type":"upsert_trackball","index":"0","section":{"name":"Trackball","device":"usb-Example_Trackball-event-mouse","tuning":{"full_power_speed":40.0}}}"#
        );
        let back: ConfigAction = serde_json::from_str(&json).expect("de");
        assert!(
            matches!(back, ConfigAction::UpsertTrackball { section: back, .. } if back == section)
        );
    }

    #[test]
    fn set_golfers_round_trips() {
        let action = ConfigAction::SetGolfers {
            golfers: vec![GolferRecord {
                sex: Some(crate::Sex::Female),
                height: Some(170.0),
                ..GolferRecord::new("flighthook.a", "Eric")
            }],
        };
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(
            json,
            r#"{"type":"set_golfers","golfers":[{"identifiers":["flighthook.a"],"name":"Eric","sex":"female","height":170.0}]}"#
        );
        let back: ConfigAction = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ConfigAction::SetGolfers { golfers } if golfers[0].height == Some(170.0))
        );
    }

    #[test]
    fn upsert_camera_round_trips() {
        let action = ConfigAction::UpsertCamera {
            index: "1".into(),
            section: CameraSection {
                name: "Face".into(),
                enabled: true,
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
        let action = ConfigAction::SetShotGrouping {
            mode: ShotGrouping::ByClub,
        };
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, r#"{"type":"set_shot_grouping","mode":"by_club"}"#);
        let back: ConfigAction = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ConfigAction::SetShotGrouping {
                mode: ShotGrouping::ByClub
            }
        ));
    }
}
