//! REST API request/response types shared between the app and UI crates.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    ActorStatus, Club, ConnectionState, Handedness, ShotDetectionMode, ShotGroupInfo, ShotGrouping,
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
    /// Player handedness in game state; absent when none has been set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed: Option<Handedness>,
    /// The server can record from cameras on its platform (beta). Omitted
    /// when it can't.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cameras_supported: bool,
    /// The folder replay clips are written to (`[replay] clip_dir`, or the
    /// default). Omitted when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_dir: Option<String>,
    /// Why the config file was not used at startup. Omitted when it was
    /// read, or after a save has replaced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<ConfigLoadError>,
}

/// A config file the app could not use at startup, so it runs on the
/// defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigLoadError {
    /// The config file.
    pub path: String,
    /// What is wrong with it: for a parse error the line and column, then
    /// the message; for a read error the reason.
    pub error: String,
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
    /// This process's record of the actor's statuses. Not serialized: a
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

    /// Apply an `actor_status` event: record `status` at `now_ms`, then fold
    /// in the keys it carries. What the actor held (device-reported values,
    /// `club`, `handed`, `name`, status keys) is dropped first when a
    /// connection is lost (Connected, then Disconnected or Reconnecting) and
    /// when a new instance starts (Starting); a card never shows a lost
    /// connection's values. Any other status merges: each key replaces the one
    /// held, and an empty value removes it. Returns whether the held telemetry
    /// was dropped.
    pub fn apply_status(
        &mut self,
        status: ActorStatus,
        telemetry: &HashMap<String, String>,
        now_ms: u64,
    ) -> bool {
        let cleared = clears_telemetry(self.status, status);
        if cleared {
            self.telemetry.clear();
        }
        self.observe(status, now_ms);
        merge_telemetry(&mut self.telemetry, telemetry);
        cleared
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

/// POST /api/find-mat request body: the ExPutt's actor ID, e.g. `exputt.0`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FindMatRequest {
    pub actor: String,
}

/// POST /api/club request body. `club` is a club code, parsed leniently with
/// [`Club::from_code`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClubRequest {
    pub club: String,
}

/// POST /api/hand request body. `handed` is `rh` or `lh`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HandRequest {
    pub handed: String,
}

/// Actor id of the Auto-chip source: shots sent with `POST /api/auto-chip`
/// come from this id, a virtual device with no config section.
pub const AUTO_CHIP_ACTOR: &str = "autochip";

/// Display name of the Auto-chip source.
pub const AUTO_CHIP_NAME: &str = "Auto-chip";

/// The total distances, in yards, `POST /api/auto-chip` takes, in menu order.
pub const AUTO_CHIP_YARDS: [u32; 3] = [1, 3, 5];

/// POST /api/auto-chip request body: the chip's total distance in yards,
/// one of [`AUTO_CHIP_YARDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct AutoChipRequest {
    pub yards: u32,
}

/// POST /api/auto-chip response: the shot that was sent.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AutoChipResponse {
    /// Always [`AUTO_CHIP_ACTOR`].
    pub actor: String,
    pub shot_id: String,
    pub shot_number: u32,
    pub ball_speed_mph: f64,
    pub launch_elevation: f64,
    pub backspin_rpm: i32,
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

/// Whether a status change from `from` to `to` ends the telemetry the actor
/// held: a lost connection, or a new instance starting.
fn clears_telemetry(from: ActorStatus, to: ActorStatus) -> bool {
    to == ActorStatus::Starting
        || (from == ActorStatus::Connected
            && matches!(to, ActorStatus::Disconnected | ActorStatus::Reconnecting))
}

/// Merge an `actor_status` event's telemetry into an actor's: each key
/// replaces the one held, and an empty value removes the key.
fn merge_telemetry(held: &mut HashMap<String, String>, update: &HashMap<String, String>) {
    for (k, v) in update {
        if v.is_empty() {
            held.remove(k);
        } else {
            held.insert(k.clone(), v.clone());
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes a reference
fn is_zero(v: &u32) -> bool {
    *v == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_club_and_hand_are_optional() {
        let s: StatusResponse = serde_json::from_str(r#"{"actors":{},"mode":"full"}"#).unwrap();
        assert_eq!(s.club, None);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"actors":{},"mode":"full"}"#);

        let s = StatusResponse {
            club: Some(Club::Iron7),
            handed: Some(Handedness::Left),
            ..s
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["club"], "7I");
        assert_eq!(v["handed"], "lh");
        let back: StatusResponse = serde_json::from_value(v).unwrap();
        assert_eq!(back.club, Some(Club::Iron7));
        assert_eq!(back.handed, Some(Handedness::Left));
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

    fn tel(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn connected_with(pairs: &[(&str, &str)]) -> ActorStatusResponse {
        let mut a = ActorStatusResponse::new("Mevo", ActorStatus::Connected);
        a.telemetry = tel(pairs);
        a
    }

    #[test]
    fn losing_a_connection_drops_the_held_telemetry_but_keeps_the_messages_own_keys() {
        for lost in [ActorStatus::Disconnected, ActorStatus::Reconnecting] {
            let mut a = connected_with(&[
                ("battery_pct", "80"),
                ("ready", "true"),
                ("club", "DR"),
                ("handed", "rh"),
                ("name", "Eric"),
                ("detection_mode", "full"),
            ]);
            a.apply_status(lost, &tel(&[("error", "refused")]), 10);
            assert_eq!(a.status, lost);
            assert_eq!(a.telemetry, tel(&[("error", "refused")]), "{lost:?}");
        }
    }

    #[test]
    fn a_new_instance_starts_without_the_old_telemetry() {
        for from in [
            ActorStatus::Connected,
            ActorStatus::Disconnected,
            ActorStatus::Reconnecting,
            ActorStatus::Starting,
        ] {
            let mut a = ActorStatusResponse::new("Mevo", from);
            a.telemetry = tel(&[("ready", "true"), ("error", "old")]);
            a.apply_status(ActorStatus::Starting, &tel(&[("address", "1.2.3.4:5")]), 10);
            assert_eq!(a.telemetry, tel(&[("address", "1.2.3.4:5")]), "{from:?}");
        }
    }

    #[test]
    fn statuses_that_do_not_end_a_connection_merge() {
        // Not Connected before: keys reported bit by bit while connecting stay.
        let mut a = ActorStatusResponse::new("Mevo", ActorStatus::Disconnected);
        a.apply_status(ActorStatus::Disconnected, &tel(&[("error", "refused")]), 1);
        a.apply_status(ActorStatus::Reconnecting, &tel(&[("attempt", "2")]), 2);
        a.apply_status(ActorStatus::Disconnected, &tel(&[("error", "")]), 3);
        assert_eq!(a.telemetry, tel(&[("attempt", "2")]));

        // Connected stays Connected, or comes from Starting: merge, an empty
        // value removes a key.
        let mut a = connected_with(&[("club", "DR"), ("error", "x")]);
        a.apply_status(
            ActorStatus::Connected,
            &tel(&[("club", "7I"), ("error", "")]),
            4,
        );
        assert_eq!(a.telemetry, tel(&[("club", "7I")]));
        let mut a = ActorStatusResponse::new("Mevo", ActorStatus::Starting);
        a.telemetry = tel(&[("address", "x")]);
        a.apply_status(ActorStatus::Connected, &tel(&[("ready", "true")]), 5);
        assert_eq!(a.telemetry, tel(&[("address", "x"), ("ready", "true")]));
    }

    #[test]
    fn applying_a_status_keeps_the_connection_history() {
        let mut a = connected_with(&[("ready", "true")]);
        a.apply_status(ActorStatus::Disconnected, &HashMap::new(), 1_000);
        assert_eq!(a.state(), ConnectionState::Reconnecting);
        assert!(a.red(1_001));
    }

    #[test]
    fn auto_chip_request_and_response() {
        let r: AutoChipRequest = serde_json::from_str(r#"{"yards":3}"#).unwrap();
        assert_eq!(r, AutoChipRequest { yards: 3 });
        assert!(serde_json::from_str::<AutoChipRequest>(r#"{"yards":"3"}"#).is_err());
        assert!(serde_json::from_str::<AutoChipRequest>(r#"{"yards":-1}"#).is_err());
        assert_eq!(AUTO_CHIP_YARDS, [1, 3, 5]);
        let resp = AutoChipResponse {
            actor: AUTO_CHIP_ACTOR.into(),
            shot_id: "id".into(),
            shot_number: 2,
            ball_speed_mph: 7.5,
            launch_elevation: 28.0,
            backspin_rpm: 750,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["actor"], "autochip");
        assert_eq!(v["backspin_rpm"], 750);
        assert_eq!(serde_json::from_value::<AutoChipResponse>(v).unwrap(), resp);
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
