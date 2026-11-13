//! Pureplay Accessory API v1 messages: what the card reads and sends.
//!
//! Every message is a JSON object with a `type`. Unknown fields and unknown
//! message or event types are ignored; event and command names compare
//! without regard to case. Absent values are `null`, read as `None`; a
//! list or flag that is `null` reads as empty or `false`.

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use flighthook::{Club, Handedness};

/// `hello.protocol` of a Pureplay Accessory server.
pub const PROTOCOL: &str = "ppaccessory";

/// The Accessory API major version this card speaks.
pub const VERSION: u64 = 1;

/// The name the player sees in Pureplay's approval prompt.
pub const CLIENT_NAME: &str = "flighthook";

/// The events the card subscribes to, those `hello.capabilities.events`
/// lists.
pub const EVENTS: [&str; 18] = [
    "context.invalidated",
    "match.started",
    "match.ended",
    "hole.started",
    "turn.changed",
    "player.joined",
    "player.left",
    "player.changed",
    "player.club_changed",
    "player.bag_changed",
    "shot.initialized",
    "shot.launched",
    "shot.completed",
    "shot.cancelled",
    "shot.replayed",
    "shot.ready_changed",
    "lie.changed",
    "score.updated",
];

/// The `id` of the card's `pair` message.
pub const PAIR_ID: &str = "pair-1";

/// The `id` of the card's `subscribe` message.
pub const SUBSCRIBE_ID: &str = "sub-1";

/// A message from Pureplay.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Hello(Hello),
    PairingPending {
        expires_in_sec: Option<u64>,
    },
    Paired(Paired),
    Response(Response),
    Event(Event),
    /// A message type the card does not know: ignored.
    Other(String),
}

/// `hello`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hello {
    pub protocol: Option<String>,
    pub version: Option<u64>,
    pub game: Option<String>,
    pub game_version: Option<String>,
    pub connection_id: Option<String>,
    pub server_instance_id: Option<String>,
    pub capabilities: Option<Capabilities>,
}

/// `hello.capabilities`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    #[serde(deserialize_with = "null_as_default")]
    pub events: Vec<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub commands: Vec<String>,
}

/// A field that may be `null`: read as its default (an empty list, `false`).
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// `paired`. The token is taken out of the text before anything logs it.
#[derive(Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Paired {
    pub token: Option<String>,
    pub client_id: Option<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for Paired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Paired")
            .field(
                "token",
                &self.token.as_ref().map(|_| super::token::REDACTED),
            )
            .field("client_id", &self.client_id)
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// `response`: the answer to a message the card sent, or (with `id`
/// `None`) one Pureplay sends unprompted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Response {
    pub id: Option<String>,
    pub ok: bool,
    pub data: Value,
    pub error: Option<ResponseError>,
}

/// A refused `response`'s `error`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResponseError {
    #[serde(deserialize_with = "null_as_default")]
    pub code: String,
    pub message: Option<String>,
    pub retry_after_ms: Option<u64>,
    #[serde(deserialize_with = "null_as_default")]
    pub blockers: Vec<String>,
}

/// An `event` message: the name in lower case, its `seq`, the snapshot
/// request it answers, and its payload.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Event {
    pub name: String,
    pub seq: Option<u64>,
    pub request_id: Option<String>,
    pub data: Value,
}

/// Why a frame could not be read.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The envelope's fields, whatever the type.
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default, rename = "requestId")]
    request_id: Option<Value>,
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

/// An id as text: a string as is, a number written out, `null` as `None`.
fn id_text(v: Option<Value>) -> Option<String> {
    match v? {
        Value::Null => None,
        Value::String(s) => Some(s),
        other => Some(other.to_string()),
    }
}

fn data_as<T: for<'de> Deserialize<'de> + Default>(data: Option<Value>) -> Result<T, ParseError> {
    match data {
        None | Some(Value::Null) => Ok(T::default()),
        Some(v) => serde_json::from_value(v).map_err(|e| ParseError(e.to_string())),
    }
}

/// Read one frame's text.
pub fn parse(text: &str) -> Result<Incoming, ParseError> {
    let env: Envelope = serde_json::from_str(text).map_err(|e| ParseError(e.to_string()))?;
    let kind = env.kind.to_ascii_lowercase();
    Ok(match kind.as_str() {
        "hello" => Incoming::Hello(data_as(env.data)?),
        "pairing.pending" => {
            #[derive(Default, Deserialize)]
            #[serde(rename_all = "camelCase", default)]
            struct Pending {
                expires_in_sec: Option<u64>,
            }
            let p: Pending = data_as(env.data)?;
            Incoming::PairingPending {
                expires_in_sec: p.expires_in_sec,
            }
        }
        "paired" => Incoming::Paired(data_as(env.data)?),
        "response" => Incoming::Response(Response {
            id: id_text(env.id),
            ok: env.ok.unwrap_or(false),
            data: env.data.unwrap_or(Value::Null),
            error: match env.error {
                None | Some(Value::Null) => None,
                Some(v) => Some(serde_json::from_value(v).map_err(|e| ParseError(e.to_string()))?),
            },
        }),
        "event" => Incoming::Event(Event {
            name: env.event.unwrap_or_default().to_ascii_lowercase(),
            seq: env.seq,
            request_id: id_text(env.request_id),
            data: env.data.unwrap_or(Value::Null),
        }),
        _ => Incoming::Other(env.kind),
    })
}

// -- Payloads ---------------------------------------------------------------

/// A player: the snapshot's `players[]` entry, `player.joined`, and
/// `player.changed.current`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Player {
    pub player_id: Option<i64>,
    /// A profile slot on the game PC, for players seated at it.
    pub profile_id: Option<String>,
    pub name: Option<String>,
    pub handedness: Option<String>,
    pub is_local: Option<bool>,
    pub team_index: Option<u32>,
    pub handicap: Option<f64>,
    pub total_strokes: Option<u32>,
    /// Gross strokes over par across the holes completed.
    pub to_par: Option<i32>,
    pub club: Option<String>,
    pub club_id: Option<String>,
    pub bag: Option<Vec<BagEntry>>,
    /// Where the player's ball lies, `[x, y, z]` in meters.
    pub ball: Option<[f64; 3]>,
    /// Where the player aims, `[x, y, z]` in meters.
    pub aim: Option<[f64; 3]>,
    /// The surface the ball lies on.
    pub lie: Option<String>,
    pub distance_to_hole_m: Option<f64>,
}

/// One club of a player's bag.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BagEntry {
    pub club_id: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub name: Option<String>,
}

/// The snapshot's `hole`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Hole {
    pub number: Option<u32>,
    pub par: Option<u32>,
    #[serde(rename = "strokeIndex")]
    pub stroke_index: Option<u32>,
    /// The hole's length in yards.
    pub yards: Option<u32>,
    /// The cup, `[x, y, z]` in meters.
    pub pin: Option<[f64; 3]>,
}

/// The snapshot's `session`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Session {
    pub match_id: Option<String>,
    pub scoring_model: Option<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub match_active: bool,
    #[serde(deserialize_with = "null_as_default")]
    pub paused: bool,
}

/// The snapshot's `course`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Course {
    pub display_name: Option<String>,
    pub hole_count: Option<u32>,
    pub total_par: Option<u32>,
}

/// The snapshot's `shot`: the shot waiting to be played or in flight.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SnapshotShot {
    pub shot_id: Option<String>,
    pub player_id: Option<i64>,
    pub hole_number: Option<u32>,
    pub stroke_number: Option<u32>,
}

/// The snapshot's `lie`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SnapshotLie {
    pub surface: Option<String>,
    pub slope_up_down_deg: Option<f64>,
    pub slope_left_right_deg: Option<f64>,
}

/// The snapshot's `turn`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Turn {
    pub active_shooter_id: Option<i64>,
}

/// `readiness`, in the snapshot and `shot.ready_changed`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Readiness {
    pub shot_id: Option<String>,
    pub player_id: Option<i64>,
    #[serde(deserialize_with = "null_as_default")]
    pub ready: bool,
    #[serde(deserialize_with = "null_as_default")]
    pub controllable: bool,
    #[serde(deserialize_with = "null_as_default")]
    pub allowed_commands: Vec<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub blockers: Vec<String>,
}

/// `state.snapshot`: the parts the card uses.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub session: Option<Session>,
    pub course: Option<Course>,
    pub hole: Option<Hole>,
    pub shot: Option<SnapshotShot>,
    pub lie: Option<SnapshotLie>,
    pub turn: Option<Turn>,
    #[serde(deserialize_with = "null_as_default")]
    pub players: Vec<Player>,
    pub readiness: Option<Readiness>,
}

/// `hole.started`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HoleStarted {
    pub match_id: Option<String>,
    pub hole_number: Option<u32>,
    pub par: Option<u32>,
    pub stroke_index: Option<u32>,
    pub yards: Option<u32>,
    pub pin: Option<[f64; 3]>,
}

/// `match.started`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MatchStarted {
    pub match_id: Option<String>,
    pub course_name: Option<String>,
    pub hole_count: Option<u32>,
    pub total_par: Option<u32>,
    pub scoring_model: Option<String>,
}

/// `match.ended`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MatchEnded {
    pub match_id: Option<String>,
    pub reason: Option<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub standings: Vec<Standing>,
}

/// One entry of `match.ended.standings`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Standing {
    pub player_id: Option<i64>,
    pub rank: Option<u32>,
    pub total_strokes: Option<u32>,
    pub to_par: Option<i32>,
}

/// `shot.initialized`: a shot set up for a player.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShotInitialized {
    pub shot_id: Option<String>,
    pub match_id: Option<String>,
    pub player_id: Option<i64>,
    pub hole_number: Option<u32>,
    pub stroke_number: Option<u32>,
    pub ball_location: Option<[f64; 3]>,
    pub club: Option<String>,
    pub club_id: Option<String>,
    pub distance_to_hole_m: Option<f64>,
    /// The shot this one replaces (a mulligan).
    pub replaces_shot_id: Option<String>,
}

/// `shot.replayed`: a shot that takes the place of another.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShotReplayed {
    pub shot_id: Option<String>,
    pub replaces_shot_id: Option<String>,
}

/// `lie.changed`: where the ball of a shot lies.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LieChanged {
    pub shot_id: Option<String>,
    pub player_id: Option<i64>,
    pub surface: Option<String>,
    pub slope_up_down_deg: Option<f64>,
    pub slope_left_right_deg: Option<f64>,
}

/// `score.updated`: one player's score.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScoreUpdated {
    pub match_id: Option<String>,
    pub hole_number: Option<u32>,
    pub player_id: Option<i64>,
    pub hole_strokes: Option<u32>,
    pub total_strokes: Option<u32>,
    pub gross_to_par: Option<i32>,
    pub net_to_par: Option<i32>,
    pub holed_out: Option<bool>,
    #[serde(rename = "final")]
    pub is_final: Option<bool>,
}

/// `turn.changed`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TurnChanged {
    pub previous_player_id: Option<i64>,
    pub current_player_id: Option<i64>,
    pub hole_number: Option<u32>,
    pub reason: Option<String>,
}

/// `player.left`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerLeft {
    pub player_id: Option<i64>,
}

/// `player.changed`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerChanged {
    pub player_id: Option<i64>,
    pub current: Option<Player>,
    pub reason: Option<String>,
}

/// `player.club_changed`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClubChanged {
    pub player_id: Option<i64>,
    pub shot_id: Option<String>,
    pub previous: Option<String>,
    pub current: Option<String>,
    pub previous_club_id: Option<String>,
    pub current_club_id: Option<String>,
    pub reason: Option<String>,
}

/// `player.bag_changed`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BagChanged {
    pub player_id: Option<i64>,
    pub bag: Option<Vec<BagEntry>>,
}

/// `context.invalidated`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ContextInvalidated {
    pub reason: Option<String>,
    pub match_id: Option<String>,
}

/// `shot.launched`: the values the ball flew with, after Pureplay's lie
/// and power adjustments.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShotLaunched {
    pub shot_id: Option<String>,
    pub player_id: Option<i64>,
    pub ball_speed_ms: Option<f64>,
    pub launch_angle_deg: Option<f64>,
    /// Positive to the right of the target line.
    pub direction_deg: Option<f64>,
    pub back_spin_rpm: Option<f64>,
    pub side_spin_rpm: Option<f64>,
}

/// `shot.completed`: where the shot ended.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShotCompleted {
    pub shot_id: Option<String>,
    pub player_id: Option<i64>,
    pub hole_number: Option<u32>,
    pub stroke_number: Option<u32>,
    pub outcome: Option<String>,
    pub penalty_strokes: Option<u32>,
    pub is_putt: Option<bool>,
    pub carry_m: Option<f64>,
    pub total_m: Option<f64>,
    pub apex_m: Option<f64>,
    pub start_dist_to_hole_m: Option<f64>,
    pub end_dist_to_hole_m: Option<f64>,
    pub start: Option<[f64; 3]>,
    pub end: Option<[f64; 3]>,
    pub lie: Option<String>,
    pub hole_strokes: Option<u32>,
}

/// `shot.cancelled`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShotCancelled {
    pub shot_id: Option<String>,
    pub player_id: Option<i64>,
    pub reason: Option<String>,
}

/// An event's payload as `T`.
pub fn payload<T: for<'de> Deserialize<'de> + Default>(event: &Event) -> Result<T, ParseError> {
    data_as(Some(event.data.clone()))
}

// -- Messages the card sends -----------------------------------------------

/// `pair`, with the saved token or `null`.
#[must_use]
pub fn pair(token: Option<&str>) -> String {
    json!({
        "type": "pair",
        "id": PAIR_ID,
        "data": {
            "client": CLIENT_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "token": token,
        }
    })
    .to_string()
}

/// `subscribe` to `events`, with the snapshot.
#[must_use]
pub fn subscribe(events: &[&str]) -> String {
    json!({
        "type": "subscribe",
        "id": SUBSCRIBE_ID,
        "data": { "events": events, "snapshot": true }
    })
    .to_string()
}

/// `state.get`, tagged `id`.
#[must_use]
pub fn state_get(id: &str) -> String {
    json!({ "type": "state.get", "id": id }).to_string()
}

/// A club `command` tagged `id` for `player`: `club.set` with `club_id`,
/// or a step (`club.next`, `club.prev`) without one. `playerId` is always
/// sent: bag ids belong to one player.
#[must_use]
pub fn command(id: &str, command: &str, player: i64, club_id: Option<&str>) -> String {
    let mut data = json!({ "playerId": player });
    if let Some(club_id) = club_id {
        data["clubId"] = json!(club_id);
    }
    json!({ "type": "command", "id": id, "command": command, "data": data }).to_string()
}

/// The events of [`EVENTS`] that `capabilities` lists: every one when
/// Pureplay lists none, none when its list holds none of them.
#[must_use]
pub fn events_to_subscribe(capabilities: Option<&Capabilities>) -> Vec<&'static str> {
    match capabilities.filter(|c| !c.events.is_empty()) {
        None => EVENTS.to_vec(),
        Some(c) => EVENTS
            .into_iter()
            .filter(|e| c.events.iter().any(|l| l.eq_ignore_ascii_case(e)))
            .collect(),
    }
}

// -- Values ------------------------------------------------------------------

/// flighthook's club for a Pureplay club type (a bag entry's `type`, a
/// player's `club`), `None` for a type Pureplay v1 does not define.
#[must_use]
pub fn club(kind: &str) -> Option<Club> {
    const CLUBS: [(&str, Club); 31] = [
        ("Driver", Club::Driver),
        ("Wood2", Club::Wood2),
        ("Wood3", Club::Wood3),
        ("Wood4", Club::Wood4),
        ("Wood5", Club::Wood5),
        ("Wood6", Club::Wood6),
        ("Wood7", Club::Wood7),
        ("Wood8", Club::Wood8),
        ("Wood9", Club::Wood9),
        ("Hybrid2", Club::Hybrid2),
        ("Hybrid3", Club::Hybrid3),
        ("Hybrid4", Club::Hybrid4),
        ("Hybrid5", Club::Hybrid5),
        ("Hybrid6", Club::Hybrid6),
        ("Hybrid7", Club::Hybrid7),
        ("Hybrid8", Club::Hybrid8),
        ("Hybrid9", Club::Hybrid9),
        ("Iron1", Club::Iron1),
        ("Iron2", Club::Iron2),
        ("Iron3", Club::Iron3),
        ("Iron4", Club::Iron4),
        ("Iron5", Club::Iron5),
        ("Iron6", Club::Iron6),
        ("Iron7", Club::Iron7),
        ("Iron8", Club::Iron8),
        ("Iron9", Club::Iron9),
        ("PitchingWedge", Club::PitchingWedge),
        ("GapWedge", Club::GapWedge),
        ("SandWedge", Club::SandWedge),
        ("LobWedge", Club::LobWedge),
        ("Putter", Club::Putter),
    ];
    CLUBS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(kind))
        .map(|(_, club)| *club)
}

/// flighthook's hand for a Pureplay `handedness`: `RH`, `LH`, or `None` for
/// `Unknown` and anything else.
#[must_use]
pub fn hand(handedness: &str) -> Option<Handedness> {
    if handedness.eq_ignore_ascii_case("RH") {
        Some(Handedness::Right)
    } else if handedness.eq_ignore_ascii_case("LH") {
        Some(Handedness::Left)
    } else {
        None
    }
}

/// The card's text for a readiness blocker.
#[must_use]
pub fn blocker_text(blocker: &str) -> String {
    match blocker {
        "not_in_play" => "no round in play".into(),
        "interrupted" => "interrupted".into(),
        "not_your_turn" => "not your turn".into(),
        "shot_in_progress" => "shot in progress".into(),
        "awaiting_decision" => "waiting on a ruling".into(),
        "controls_disabled" => "apps may not change aim and club".into(),
        other => other.replace('_', " "),
    }
}

/// The card's `blockers` row for `readiness`: what keeps the shot from
/// being played, empty while it can be (or Pureplay names nothing).
#[must_use]
pub fn blockers_text(readiness: &Readiness) -> String {
    if readiness.ready {
        return String::new();
    }
    readiness
        .blockers
        .iter()
        .filter(|b| b.as_str() != "controls_disabled")
        .map(|b| blocker_text(b))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[path = "fixtures.rs"]
mod fixtures;
