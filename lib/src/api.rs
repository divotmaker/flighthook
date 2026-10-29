//! REST API request/response types shared between the app and UI crates.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    ActorStatus, Club, ConnectionState, ShotDetectionMode, ShotGroupInfo, ShotGrouping,
    StatusHistory,
};

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
/// per-actor state in the web layer and UI, which record each reported
/// status with [`observe`](Self::observe) so the connection history behind
/// [`state`](Self::state) stays current.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorStatusResponse {
    #[serde(default)]
    pub name: String,
    pub status: ActorStatus,
    #[serde(default)]
    pub telemetry: HashMap<String, String>,
    /// The state to show, as of the reply: `status` plus the actor's history
    /// since it last started.
    #[serde(default)]
    pub state: ConnectionState,
    /// Milliseconds the state stays red after the reply was built: what is
    /// left of the alert after a lost connection, else 0.
    #[serde(default)]
    pub red_for_ms: u64,
    /// This process's record of the actor's statuses. Not serialised: a
    /// reply carries it as `state` and `red_for_ms`.
    #[serde(skip)]
    history: StatusHistory,
}

impl ActorStatusResponse {
    /// An actor named `name` whose first status is `status`.
    #[must_use]
    pub fn new(name: impl Into<String>, status: ActorStatus) -> Self {
        let mut actor = Self {
            name: name.into(),
            status,
            telemetry: HashMap::new(),
            state: ConnectionState::default(),
            red_for_ms: 0,
            history: StatusHistory::default(),
        };
        actor.observe(status, 0);
        actor
    }

    /// Record a status the actor reported at `now_ms` (see
    /// [`StatusHistory`] for the clock).
    pub fn observe(&mut self, status: ActorStatus, now_ms: u64) {
        self.status = status;
        self.history.observe(status, now_ms);
        self.stamp(now_ms);
    }

    /// The actor was stopped and started again: a new history, keeping only
    /// whether it is Connected now.
    pub fn restart(&mut self, now_ms: u64) {
        self.history.restart();
        self.stamp(now_ms);
    }

    /// Take the history from a received reply's `state` and `red_for_ms`,
    /// counting the red time from `now_ms`, the moment of receipt.
    pub fn received(&mut self, now_ms: u64) {
        self.history = StatusHistory::received(self.status, self.state, self.red_for_ms, now_ms);
    }

    /// Set `state` and `red_for_ms` from the history as of `now_ms`, for a
    /// reply.
    pub fn stamp(&mut self, now_ms: u64) {
        self.state = self.history.state();
        self.red_for_ms = self.history.red_for_ms(now_ms);
    }

    /// The state to show.
    #[must_use]
    pub fn state(&self) -> ConnectionState {
        self.history.state()
    }

    /// Whether the state is red (an alert) at `now_ms`.
    #[must_use]
    pub fn red(&self, now_ms: u64) -> bool {
        self.history.red(now_ms)
    }

    /// Milliseconds the state stays red after `now_ms`.
    #[must_use]
    pub fn red_for_ms_at(&self, now_ms: u64) -> u64 {
        self.history.red_for_ms(now_ms)
    }
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
    fn actor_status_carries_state_and_red_time() {
        let mut a = ActorStatusResponse::new("Uneekor", ActorStatus::Connected);
        a.observe(ActorStatus::Disconnected, 1_000);
        a.stamp(61_000);
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["status"], "disconnected");
        assert_eq!(v["state"], "reconnecting");
        assert_eq!(v["red_for_ms"], crate::RECONNECT_ALERT_MS - 60_000);
        assert!(v.get("history").is_none());

        // Received, the red time counts from receipt on the receiver's clock.
        let mut back: ActorStatusResponse = serde_json::from_value(v).unwrap();
        back.received(5);
        assert_eq!(back.state(), ConnectionState::Reconnecting);
        assert_eq!(back.red_for_ms_at(5), crate::RECONNECT_ALERT_MS - 60_000);

        // A reply without the fields.
        let mut old: ActorStatusResponse =
            serde_json::from_str(r#"{"name":"Mevo","status":"connected"}"#).unwrap();
        old.received(0);
        assert_eq!(old.state(), ConnectionState::Connected);
        assert_eq!(old.red_for_ms, 0);
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
