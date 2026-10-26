//! REST API request/response types shared between the app and UI crates.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{ActorStatus, Club, ShotDetectionMode, ShotGroupInfo, ShotGrouping};

/// GET /api/status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    #[serde(default)]
    pub actors: HashMap<String, ActorStatusResponse>,
    #[serde(default)]
    pub mode: Option<ShotDetectionMode>,
    /// Club selected in game state; absent when none has been selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub club: Option<Club>,
}

/// Per-actor status within the status response. Also used as the cached
/// per-actor state in the web layer and UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorStatusResponse {
    #[serde(default)]
    pub name: String,
    pub status: ActorStatus,
    #[serde(default)]
    pub telemetry: HashMap<String, String>,
}

/// POST /api/mode request body.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModeRequest {
    pub mode: ShotDetectionMode,
}

/// POST /api/settings response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostSettingsResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarted: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<String>,
}

/// POST /api/club request body. `club` is a club code, parsed leniently with
/// [`Club::from_code`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClubRequest {
    pub club: String,
}

/// POST /api/shots/grouping request body and response: the default view.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GroupingRequest {
    pub mode: ShotGrouping,
}

/// GET /api/shots/groups response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShotGroupsResponse {
    /// Actor ID of the webserver that produced this list (`"webserver.0"`).
    pub server: String,
    /// Default view (`shot_grouping`). It does not change how sets are
    /// assigned.
    pub mode: ShotGrouping,
    /// Current session: starts at 0 and counts up with every "New session".
    /// Omitted when 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub session: u32,
    /// Id of the open set: the one the next shot joins, unless the club in
    /// play differs from its club.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// Open groups and groups referenced by a stored shot, ascending `number`.
    #[serde(default)]
    pub groups: Vec<ShotGroupInfo>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes a reference
fn is_zero(v: &u32) -> bool {
    *v == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_club_is_optional() {
        let s: StatusResponse = serde_json::from_str(r#"{"actors":{},"mode":"full"}"#).unwrap();
        assert_eq!(s.club, None);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"actors":{},"mode":"full"}"#);

        let s = StatusResponse {
            club: Some(Club::Iron7),
            ..s
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["club"], "7I");
        let back: StatusResponse = serde_json::from_value(v).unwrap();
        assert_eq!(back.club, Some(Club::Iron7));
    }

    #[test]
    fn club_and_grouping_requests() {
        let c: ClubRequest = serde_json::from_str(r#"{"club":"I7"}"#).unwrap();
        assert_eq!(Club::from_code(&c.club), Some(Club::Iron7));
        let g: GroupingRequest = serde_json::from_str(r#"{"mode":"by_club"}"#).unwrap();
        assert_eq!(g.mode, ShotGrouping::ByClub);
        assert_eq!(
            serde_json::to_string(&GroupingRequest {
                mode: ShotGrouping::None
            })
            .unwrap(),
            r#"{"mode":"none"}"#
        );
    }

    #[test]
    fn shot_groups_response_round_trips() {
        let empty: ShotGroupsResponse =
            serde_json::from_str(r#"{"server":"webserver.0","mode":"none"}"#).unwrap();
        assert_eq!(empty.mode, ShotGrouping::None);
        assert_eq!(empty.current, None);
        assert_eq!(empty.session, 0);
        assert!(empty.groups.is_empty());
        assert_eq!(
            serde_json::to_string(&empty).unwrap(),
            r#"{"server":"webserver.0","mode":"none","groups":[]}"#
        );

        let group = ShotGroupInfo {
            id: "6f1c2a3b-4d5e-4f60-8a71-92b3c4d5e6f7".into(),
            number: 1,
            session: 0,
            golfer: None,
            club: Some(Club::Driver),
            started_at: "2026-10-26T13:58:02.117Z".into(),
            manual: true,
            open: true,
            shots: 0,
        };
        let full = ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::ByClub,
            session: 2,
            current: Some(group.id.clone()),
            groups: vec![group.clone()],
        };
        let v = serde_json::to_value(&full).unwrap();
        assert_eq!(v["mode"], "by_club");
        assert_eq!(v["current"], group.id.as_str());
        assert_eq!(v["session"], 2);
        let back: ShotGroupsResponse = serde_json::from_value(v).unwrap();
        assert_eq!(back.current, full.current);
        assert_eq!(back.session, 2);
        assert_eq!(back.groups, vec![group]);
    }
}
