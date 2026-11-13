//! REST API request/response types shared between the app and UI crates.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{ActorStatus, ConnectionState, RoundState, ShotGrouping, StatusHistory};

/// GET /api/status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    /// Each local card's status, by card key.
    #[serde(default)]
    pub cards: HashMap<String, ActorStatusResponse>,
    /// Every FRP actor on the bus, local and remote, by actor id.
    #[serde(default)]
    pub actors: HashMap<String, StatusActor>,
    /// The session's current `golfer.turn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<flightrelay::GolferTurn>,
    /// The server can record from cameras on its platform (beta). Omitted
    /// when it can't.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cameras_supported: bool,
    /// The server can read a trackball on its platform (alpha), so it offers
    /// the Trackball card. Omitted when it can't.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trackball_supported: bool,
    /// The folder replay clips are written to (`[replay] clip_dir`, or the
    /// default). Omitted when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_dir: Option<String>,
    /// Why the config file was not used at startup. Omitted when it was
    /// read, or after a save has replaced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<ConfigLoadError>,
    /// The tournament round: whether one is active, and which output holds
    /// it. Omitted when no round is active.
    #[serde(default, skip_serializing_if = "RoundState::is_inactive")]
    pub round: RoundState,
}

/// One FRP actor in [`StatusResponse::actors`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusActor {
    pub name: String,
    #[serde(default)]
    pub roles: Vec<String>,
    /// What keeps it from working; empty when it is ready.
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<flightrelay::DeviceInfo>,
    /// The local card key, when the actor is a flighthook card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
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

    /// An actor named `name` that is turned off in the settings: status
    /// `Disconnected`, state [`ConnectionState::Disabled`], no telemetry,
    /// never red. A `Starting` status (a new instance) ends it.
    #[must_use]
    pub fn disabled(name: impl Into<String>) -> Self {
        let status = ActorStatus::Disconnected;
        let mut actor = Self {
            name: name.into(),
            status,
            telemetry: HashMap::new(),
            state: ConnectionState::Disabled,
            red_for_ms: 0,
            history: StatusHistory::off(status),
        };
        actor.stamp(0);
        actor
    }

    /// Whether the actor is turned off in the settings.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.history.state() == ConnectionState::Disabled
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

/// POST /api/settings response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PostSettingsResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarted: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<String>,
    /// For `?scope=golfers`: the golfers as saved, in the order posted,
    /// each with its own `flighthook.*` id (a new golfer's is given by the
    /// save).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub golfers: Vec<crate::GolferRecord>,
}

/// POST /api/find-mat request body: the ExPutt's actor ID, e.g. `exputt.0`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FindMatRequest {
    pub actor: String,
}

/// Actor id of the Auto-chip source: shots sent with `POST /api/auto-chip`
/// come from this id, a virtual device with no config section.
pub const AUTO_CHIP_ACTOR: &str = "autochip";

/// Display name of the Auto-chip source.
pub const AUTO_CHIP_NAME: &str = "Auto-Chip";

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

/// POST /api/tournament request body: switch tournament mode on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct TournamentRequest {
    pub on: bool,
}

/// One practice block in `GET /api/blocks`: the `practice.block` as last
/// published, whether it is still open, and the stored shots in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockInfo {
    #[serde(flatten)]
    pub block: flightrelay::PracticeBlock,
    /// Not closed by the practice actor yet.
    pub open: bool,
    /// Stored shots with data that belong to it.
    pub shots: u32,
    /// When its first stored shot was triggered, in milliseconds since the
    /// Unix epoch; absent before its first shot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_shot_at: Option<u64>,
}

/// `GET /api/blocks` response: every practice block this run, open and
/// closed, in the order first seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlocksResponse {
    /// The current practice session (from 1 each run).
    pub session: u32,
    pub blocks: Vec<BlockInfo>,
}

/// `POST /api/blocks` body: a `practice.new_block` from flighthook.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewBlockRequest {
    /// The golfer whose block to roll; absent for the golfer up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub golfer: Option<flightrelay::GolferId>,
    /// Close every open block and count the session up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new_session: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_turn_is_optional_and_actors_name_their_card() {
        let s: StatusResponse = serde_json::from_str(r#"{"cards":{}}"#).unwrap();
        assert_eq!(s.turn, None);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"cards":{},"actors":{}}"#);

        let s = StatusResponse {
            turn: Some(flightrelay::GolferTurn::new(
                None,
                Some(crate::Club::Iron7),
                crate::Handedness::Left,
                crate::Mode::Full,
            )),
            actors: HashMap::from([(
                "aaaa".to_string(),
                StatusActor {
                    name: "Range".into(),
                    roles: vec!["launch_monitor".into()],
                    blockers: vec![],
                    device: None,
                    card: Some("mevo.0".into()),
                },
            )]),
            ..s
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["turn"]["club"], "7I");
        assert_eq!(v["turn"]["handedness"], "lh");
        assert_eq!(v["actors"]["aaaa"]["card"], "mevo.0");
        let back: StatusResponse = serde_json::from_value(v).unwrap();
        assert_eq!(back.turn, s.turn);
        assert_eq!(back.actors, s.actors);
    }

    #[test]
    fn actor_status_carries_state_and_red_time() {
        let mut a = ActorStatusResponse::new("Range Bridge", ActorStatus::Connected);
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
    fn a_disabled_actor_reads_disabled_until_a_new_instance_starts() {
        let a = ActorStatusResponse::disabled("Mock");
        assert!(a.is_disabled());
        assert_eq!(a.state, ConnectionState::Disabled);
        assert_eq!(a.red_for_ms, 0);
        assert!(!a.red(0));
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["state"], "disabled");
        assert_eq!(json["status"], "disconnected");
        // A reply read back keeps it, as the UI receives it.
        let mut got: ActorStatusResponse = serde_json::from_value(json).unwrap();
        got.received(10);
        assert!(got.is_disabled());
        // Turned on: the new instance's Starting ends it.
        got.apply_status(ActorStatus::Starting, &HashMap::new(), 20);
        assert!(!got.is_disabled());
        assert_eq!(got.state, ConnectionState::Connecting);
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
    fn grouping_requests() {
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
}
