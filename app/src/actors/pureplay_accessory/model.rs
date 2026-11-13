//! What the card knows of the round, and the turn it holds for flighthook.
//!
//! The model keeps, per player, whether the player sits at this machine
//! (`isLocal`), name, handedness, held club, bag, ball and
//! distance to the hole; the **active local player** is
//! `turn.activeShooterId` when that player is local. It also keeps the
//! round (match, course, hole, the shot about to be played, scores) that
//! the card publishes as `game.*` ([`game`](super::game)).
//!
//! **The turn the card holds** ([`Want`]) is the active local player's
//! golfer id, club, hand and mode, sent whole in one `golfer.select`
//! ([`Model::sync`]):
//!
//! - when Pureplay reports a change of it (snapshot, `turn.changed`,
//!   `player.changed`, the active local player's `player.club_changed`, a
//!   `shot.initialized` distance that changes the mode) and flighthook's turn
//!   differs from it;
//! - again whenever flighthook's turn differs from it, unless the turn was
//!   caused by a game or simulator card: between those the last sender wins.
//!
//! The golfer differs when the turn's golfer and the player's id do not
//! resolve to the same golfer (the lookup rule). A remote player up gives
//! nothing: the turn keeps the last local golfer. The mode is the shared
//! club + distance rule ([`mode_for_lie`]): the putter putts; a distance to
//! the hole under 30 yd chips; anything else, an unknown distance included,
//! is a full swing.
//!
//! While a club picked in flighthook is pending or in flight to Pureplay
//! ([`Model::hold_club`], see [`club_out`](super::club_out)), the card
//! neither sends nor compares the club. While a `golfer.select` (the card's
//! own or a pick from outside) awaits its `bus.reply`, the turn on the bus
//! is not final and nothing is compared with it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use flighthook::{Club, Handedness};
use flightrelay::{GolferId, GolferSelect, GolferTurn, Mode};

use super::protocol::{self, BagEntry, Event, Player, Readiness};
use super::shots::MAP_CAPACITY;
use crate::actors::simout::mode_for_lie;

/// One player as the card knows them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerState {
    pub local: bool,
    /// Pureplay's `name` for the player; it keys the golfer id.
    pub name: Option<String>,
    /// Pureplay's `handedness`, as sent.
    pub handedness: Option<String>,
    pub team: Option<u32>,
    pub handicap: Option<f64>,
    pub total_strokes: Option<u32>,
    pub to_par: Option<i32>,
    /// The held club, as Pureplay names its type.
    pub club: Option<String>,
    pub club_id: Option<String>,
    pub bag: Option<Vec<BagEntry>>,
    pub ball: Option<[f64; 3]>,
    pub aim: Option<[f64; 3]>,
    /// The distance to the hole before the next shot, in meters.
    pub distance: Option<f64>,
}

impl PlayerState {
    fn from_player(p: &Player) -> Self {
        Self {
            local: p.is_local.unwrap_or(false),
            name: p.name.clone().filter(|n| !n.trim().is_empty()),
            handedness: p.handedness.clone(),
            team: p.team_index,
            handicap: p.handicap,
            total_strokes: p.total_strokes,
            to_par: p.to_par,
            club: p.club.clone(),
            club_id: p.club_id.clone(),
            bag: p.bag.clone(),
            ball: p.ball,
            aim: p.aim,
            distance: p.distance_to_hole_m,
        }
    }

    /// Take what `p` gives; a field it leaves out stays.
    fn merge(&mut self, p: &Player) {
        fn keep<T>(mine: &mut Option<T>, theirs: Option<T>) {
            if theirs.is_some() {
                *mine = theirs;
            }
        }
        let given = Self::from_player(p);
        if let Some(local) = p.is_local {
            self.local = local;
        }
        keep(&mut self.name, given.name);
        keep(&mut self.handedness, given.handedness);
        keep(&mut self.team, given.team);
        keep(&mut self.handicap, given.handicap);
        keep(&mut self.total_strokes, given.total_strokes);
        keep(&mut self.to_par, given.to_par);
        keep(&mut self.club, given.club);
        keep(&mut self.club_id, given.club_id);
        keep(&mut self.bag, given.bag);
        keep(&mut self.ball, given.ball);
        keep(&mut self.aim, given.aim);
        keep(&mut self.distance, given.distance);
    }

    /// Clear each identity field `current` gives as an explicit `null`
    /// (a field it leaves out is unchanged). Only the fields
    /// `player.changed` reports are cleared: name, hand, team and
    /// handicap. Club, club id, bag, ball, aim, distance, strokes and to-par
    /// have their own events and the snapshot, so a `null` here leaves them.
    fn clear_nulls(&mut self, current: &serde_json::Map<String, serde_json::Value>) {
        let nulled = |key: &str| current.get(key).is_some_and(serde_json::Value::is_null);
        if nulled("name") {
            self.name = None;
        }
        if nulled("handedness") {
            self.handedness = None;
        }
        if nulled("teamIndex") {
            self.team = None;
        }
        if nulled("handicap") {
            self.handicap = None;
        }
    }

    /// flighthook's club for the held one.
    #[must_use]
    pub fn held_club(&self) -> Option<Club> {
        self.club.as_deref().and_then(protocol::club)
    }

    /// flighthook's hand: RH or LH, `None` for Unknown.
    #[must_use]
    pub fn hand(&self) -> Option<Handedness> {
        self.handedness.as_deref().and_then(protocol::hand)
    }

    /// The player's clubs, flighthook's codes in bag order, each once.
    #[must_use]
    pub fn clubs(&self) -> Vec<Club> {
        let mut clubs = Vec::new();
        for entry in self.bag.iter().flatten() {
            if let Some(club) = entry.kind.as_deref().and_then(protocol::club)
                && !clubs.contains(&club)
            {
                clubs.push(club);
            }
        }
        clubs
    }
}

/// Player `id`'s golfer id: `pureplay.<name>` (the name as Pureplay sends
/// it), or `pureplay.player-<playerId>` for a player without a name.
#[must_use]
pub fn golfer_id(id: i64, name: Option<&str>) -> Option<GolferId> {
    name.and_then(|n| GolferId::new(NAMESPACE, n).ok())
        .or_else(|| GolferId::new(NAMESPACE, &format!("player-{id}")).ok())
}

/// The golfer namespace the card issues.
pub const NAMESPACE: &str = "pureplay";

/// The turn the card holds for the active local player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub golfer: GolferId,
    /// `None` while a pick holds the club, or for a club type flighthook
    /// has no club for ([`Want::unmapped`]).
    pub club: Option<Club>,
    /// Pureplay holds a club flighthook has no code for (and no pick holds
    /// the club): the select carries `club: null`.
    pub unmapped: bool,
    /// `None` when Pureplay gives no hand (Unknown).
    pub hand: Option<Handedness>,
    pub mode: Mode,
}

impl Want {
    /// The card's complete `golfer.select`: every value it holds.
    #[must_use]
    pub fn select(&self, r#ref: String) -> GolferSelect {
        GolferSelect {
            golfer: Some(Some(self.golfer.clone())),
            club: self.club.map(Some).or(self.unmapped.then_some(None)),
            handedness: self.hand,
            mode: Some(self.mode),
            ..GolferSelect::new(r#ref)
        }
    }

    /// Whether `turn` differs from what the card holds. `key` is the
    /// lookup rule: the key a golfer id is counted under.
    pub fn differs(&self, turn: &GolferTurn, key: impl Fn(Option<&str>) -> Option<String>) -> bool {
        key(turn.golfer.as_ref().map(GolferId::as_str)) != key(Some(self.golfer.as_str()))
            || self.club.is_some_and(|c| turn.club != Some(c))
            || (self.unmapped && turn.club.is_some())
            || self.hand.is_some_and(|h| turn.handedness != Some(h))
            || turn.mode != Some(self.mode)
    }
}

/// The round as Pureplay describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Round {
    pub id: String,
    pub course: Option<String>,
    pub holes: Option<u32>,
    pub par: Option<u32>,
    pub scoring: Option<String>,
    pub paused: bool,
    /// `match.ended`: its `reason`.
    pub ended: Option<String>,
}

/// The hole in play.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HoleState {
    pub number: Option<u32>,
    pub par: Option<u32>,
    pub stroke_index: Option<u32>,
    pub yards: Option<u32>,
    pub pin: Option<[f64; 3]>,
}

/// The shot set up to be played next.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShotUp {
    pub id: Option<String>,
    pub player: Option<i64>,
    pub hole: Option<u32>,
    pub stroke: Option<u32>,
}

/// One hole of a player's score.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HoleScoreState {
    pub strokes: Option<u32>,
    pub to_par: Option<i32>,
    pub net_to_par: Option<i32>,
    pub holed_out: Option<bool>,
    pub r#final: Option<bool>,
}

/// One player's score.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScoreState {
    pub total: Option<u32>,
    /// The total strokes over par, as Pureplay last gave it together with
    /// the total (a snapshot, the final standings); left out once a score
    /// update moved the total without it.
    pub to_par: Option<i32>,
    pub holes: BTreeMap<u32, HoleScoreState>,
}

/// Values by Pureplay `shotId`, the newest [`MAP_CAPACITY`] kept.
#[derive(Debug)]
struct Recent<T> {
    entries: VecDeque<(String, T)>,
}

impl<T> Default for Recent<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
}

impl<T> Recent<T> {
    fn insert(&mut self, id: &str, value: T) {
        self.entries.retain(|(i, _)| i != id);
        if self.entries.len() >= MAP_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((id.to_owned(), value));
    }

    fn get(&self, id: &str) -> Option<&T> {
        self.entries.iter().find(|(i, _)| i == id).map(|(_, v)| v)
    }
}

/// The round as the card knows it.
#[derive(Debug, Default)]
pub struct Model {
    players: BTreeMap<i64, PlayerState>,
    /// The players in playing order: the snapshot's, then those who joined.
    order: Vec<i64>,
    /// `turn.activeShooterId`.
    active: Option<i64>,
    hole: Option<HoleState>,
    round: Option<Round>,
    shot: Option<ShotUp>,
    /// Where each shot's ball lies, by `shotId`.
    lies: Recent<protocol::SnapshotLie>,
    /// The `shotId` each shot replaces (a mulligan), by `shotId`.
    replaces: Recent<String>,
    scores: BTreeMap<i64, ScoreState>,
    readiness: Option<Readiness>,
    /// flighthook's turn as the bus last carried it, and what caused it.
    turn: Option<(GolferTurn, Option<String>)>,
    /// A club pick is outstanding: the club is neither sent nor compared.
    club_held: bool,
    /// Pureplay reported a change of the turn the card holds since the
    /// last comparison.
    reported: bool,
    /// The `golfer.select`s awaiting their `bus.reply`, by sender and
    /// `ref`: the card's own and the picks from outside. While any is
    /// unanswered, the turn on the bus is not final and nothing is
    /// compared with it.
    unanswered: BTreeSet<(String, String)>,
}

impl Model {
    /// The active player when they sit at this machine.
    #[must_use]
    pub fn active_local(&self) -> Option<&PlayerState> {
        self.active
            .and_then(|id| self.players.get(&id))
            .filter(|p| p.local)
    }

    /// The id of the active player when they sit at this machine.
    #[must_use]
    pub fn active_local_id(&self) -> Option<i64> {
        self.active.filter(|id| self.is_local(*id))
    }

    /// The active player, local or not.
    #[must_use]
    pub fn active(&self) -> Option<i64> {
        self.active
    }

    /// The player `event` names when this model does not hold them: the
    /// snapshot has to bring that player.
    #[must_use]
    pub fn unknown_player(&self, event: &Event) -> Option<i64> {
        let id = match event.name.as_str() {
            "turn.changed" => protocol::payload::<protocol::TurnChanged>(event)
                .ok()
                .and_then(|t| t.current_player_id),
            "shot.initialized" => protocol::payload::<protocol::ShotInitialized>(event)
                .ok()
                .and_then(|s| s.player_id),
            "shot.launched" => protocol::payload::<protocol::ShotLaunched>(event)
                .ok()
                .and_then(|s| s.player_id),
            "player.club_changed" => protocol::payload::<protocol::ClubChanged>(event)
                .ok()
                .and_then(|c| c.player_id),
            _ => None,
        };
        id.filter(|id| !self.players.contains_key(id))
    }

    /// The round's end has been published: it is over, and so are its
    /// players (a snapshot or `player.joined` brings them back).
    pub fn end_round(&mut self) {
        self.round = None;
        self.shot = None;
        self.scores.clear();
        self.players.clear();
        self.order.clear();
        self.active = None;
    }

    /// Player `id`, as the card knows them.
    #[must_use]
    pub fn player(&self, id: i64) -> Option<&PlayerState> {
        self.players.get(&id)
    }

    /// Every player, by `playerId` (see [`Model::order`] for playing
    /// order).
    #[must_use]
    pub fn players(&self) -> &BTreeMap<i64, PlayerState> {
        &self.players
    }

    /// Player `id`'s golfer id; `None` for a player the card does not hold.
    #[must_use]
    pub fn golfer(&self, id: i64) -> Option<GolferId> {
        let player = self.players.get(&id)?;
        golfer_id(id, player.name.as_deref())
    }

    #[must_use]
    pub fn round(&self) -> Option<&Round> {
        self.round.as_ref()
    }

    #[must_use]
    pub fn hole(&self) -> Option<&HoleState> {
        self.hole.as_ref()
    }

    #[must_use]
    pub fn shot_up(&self) -> Option<&ShotUp> {
        self.shot.as_ref()
    }

    /// Where the ball of the shot up lies: only a lie that belongs to that
    /// shot, never one carried over from another.
    #[must_use]
    pub fn lie(&self) -> Option<&protocol::SnapshotLie> {
        self.lies.get(self.shot.as_ref()?.id.as_deref()?)
    }

    /// The `shotId` shot `id` replaces.
    #[must_use]
    pub fn replaces(&self, id: &str) -> Option<&str> {
        self.replaces.get(id).map(String::as_str)
    }

    /// Every player's id in playing order.
    #[must_use]
    pub fn order(&self) -> &[i64] {
        &self.order
    }

    #[must_use]
    pub fn scores(&self) -> &BTreeMap<i64, ScoreState> {
        &self.scores
    }

    /// Player `id` holds `club` (`club_id`), as a command's reply says. A
    /// player the card does not hold is not made up.
    pub fn set_club(&mut self, id: i64, club: Option<String>, club_id: Option<String>) {
        let Some(player) = self.players.get_mut(&id) else {
            return;
        };
        player.club = club;
        player.club_id = club_id;
        if self.active_local_id() == Some(id) {
            self.reported = true;
        }
    }

    /// Neither send nor compare the club while `hold` (a club pick is
    /// outstanding). Letting go compares again.
    pub fn hold_club(&mut self, hold: bool) {
        if self.club_held && !hold {
            self.reported = true;
        }
        self.club_held = hold;
    }

    /// Whether player `id` sits at this machine.
    #[must_use]
    pub fn is_local(&self, id: i64) -> bool {
        self.players.get(&id).is_some_and(|p| p.local)
    }

    #[must_use]
    pub fn readiness(&self) -> Option<&Readiness> {
        self.readiness.as_ref()
    }

    /// Forget the round: Pureplay restarted. The turn flighthook holds is
    /// flighthook's, not Pureplay's, and stays.
    pub fn reset(&mut self) {
        *self = Self {
            turn: self.turn.take(),
            ..Self::default()
        };
    }

    /// Note flighthook's turn and what caused it (also how the card seeds
    /// itself with the session's turn when it starts).
    pub fn flighthook_holds(&mut self, turn: &GolferTurn, cause: Option<&str>) {
        self.turn = Some((turn.clone(), cause.map(str::to_owned)));
    }

    /// Note a `golfer.select` sent by `from` with `r#ref`: until its
    /// `bus.reply`, nothing is compared with flighthook's turn.
    pub fn select_sent(&mut self, from: &str, r#ref: &str) {
        self.unanswered.insert((from.to_owned(), r#ref.to_owned()));
    }

    /// Note the `bus.reply` to the select `r#ref` of `to`. Returns true when
    /// it was the last one unanswered: the turn on the bus is final (the
    /// session publishes a turn before its reply).
    pub fn select_answered(&mut self, to: &str, r#ref: &str) -> bool {
        self.unanswered.remove(&(to.to_owned(), r#ref.to_owned())) && self.unanswered.is_empty()
    }

    /// The card's bus receiver fell behind: replies may be lost, so the
    /// selects awaiting one are forgotten and `turn` (the session's current
    /// one, with its cause) is what flighthook holds.
    pub fn lagged(&mut self, turn: &GolferTurn, cause: Option<&str>) {
        self.unanswered.clear();
        self.flighthook_holds(turn, cause);
    }

    /// Forget the selects awaiting a reply. Called when the connection
    /// ends. The turn flighthook holds is kept: the card reads the bus the
    /// whole time, so its copy stays current.
    pub fn forget_held(&mut self) {
        self.unanswered.clear();
        self.reported = false;
    }

    /// The turn the card holds: the active local player's golfer, club,
    /// hand and mode. `None` while no local player is up.
    #[must_use]
    pub fn want(&self) -> Option<Want> {
        let id = self.active_local_id()?;
        let player = self.players.get(&id)?;
        let golfer = self.golfer(id)?;
        let held = player.held_club();
        let unmapped = held.is_none() && player.club.is_some();
        if let Some(kind) = player.club.as_ref().filter(|_| unmapped) {
            tracing::debug!("pureplay_accessory: unknown club type '{kind}'");
        }
        let club = if self.club_held { None } else { held };
        // While a pick holds the club, the mode follows the club picked.
        let mode_club = if self.club_held {
            self.turn.as_ref().and_then(|(t, _)| t.club)
        } else {
            held
        };
        Some(Want {
            golfer,
            club,
            unmapped: unmapped && !self.club_held,
            hand: player.hand(),
            mode: mode_for_lie(mode_club, player.distance),
        })
    }

    /// The `golfer.select` to send now, if any: once every select is
    /// answered, the card's turn when flighthook's differs from it and
    /// either Pureplay reported a change or the turn was not caused by a
    /// game or simulator card (`is_game_card`). `key` is the lookup rule.
    pub fn sync(
        &mut self,
        key: impl Fn(Option<&str>) -> Option<String>,
        is_game_card: impl Fn(&str) -> bool,
    ) -> Option<Want> {
        if !self.unanswered.is_empty() {
            return None;
        }
        let reported = std::mem::take(&mut self.reported);
        let want = self.want()?;
        // No turn known is no difference: the card is seeded with the
        // session's turn when it starts.
        let (turn, cause) = self.turn.as_ref()?;
        if !want.differs(turn, key) {
            return None;
        }
        (reported || !cause.as_deref().is_some_and(is_game_card)).then_some(want)
    }

    /// The hole row: "7, par 4".
    #[must_use]
    pub fn hole_text(&self) -> Option<String> {
        let hole = self.hole.as_ref()?;
        match (hole.number, hole.par) {
            (Some(n), Some(par)) => Some(format!("{n}, par {par}")),
            (Some(n), None) => Some(n.to_string()),
            (None, _) => None,
        }
    }

    /// Take a snapshot: it replaces what the model knew.
    pub fn apply_snapshot(&mut self, snap: &protocol::Snapshot) {
        let session = snap.session.clone().unwrap_or_default();
        // A snapshot with no match active (no `session`, or the results
        // screen, which keeps the ended match's players in it) brings no
        // players, no shooter and no shot: nobody is playing, whether or
        // not the card saw the match end, and whatever its `matchId`.
        let results = !session.match_active;
        let players: &[protocol::Player] = if results { &[] } else { &snap.players };
        self.order = players.iter().filter_map(|p| p.player_id).collect();
        self.players = players
            .iter()
            .filter_map(|p| Some((p.player_id?, PlayerState::from_player(p))))
            .collect();
        self.active = snap
            .turn
            .as_ref()
            .and_then(|t| t.active_shooter_id)
            .filter(|_| !results);
        self.hole = snap.hole.as_ref().map(|h| HoleState {
            number: h.number,
            par: h.par,
            stroke_index: h.stroke_index,
            yards: h.yards,
            pin: h.pin,
        });
        self.readiness = snap.readiness.clone();
        self.shot = snap.shot.as_ref().filter(|_| !results).map(|s| ShotUp {
            id: s.shot_id.clone().filter(|id| !id.is_empty()),
            player: s.player_id,
            hole: s.hole_number,
            stroke: s.stroke_number,
        });
        // The snapshot's lie is the one of the shot it names.
        if let (Some(lie), Some(id)) = (
            &snap.lie,
            snap.shot.as_ref().and_then(|s| s.shot_id.as_deref()),
        ) {
            self.lies.insert(id, lie.clone());
        }
        let round = session.match_id.filter(|_| session.match_active);
        // Scores belong to their match.
        if round.as_deref() != self.round.as_ref().map(|r| r.id.as_str()) {
            self.scores.clear();
        }
        self.round = round.map(|id| Round {
            id,
            course: snap.course.as_ref().and_then(|c| c.display_name.clone()),
            holes: snap.course.as_ref().and_then(|c| c.hole_count),
            par: snap.course.as_ref().and_then(|c| c.total_par),
            scoring: session.scoring_model.clone(),
            paused: session.paused,
            ended: None,
        });
        for (id, p) in &self.players {
            if p.total_strokes.is_some() {
                let score = self.scores.entry(*id).or_default();
                score.total = p.total_strokes;
                score.to_par = p.to_par;
            }
        }
        self.reported = true;
    }

    /// Take a live event. An event the model does not use changes nothing.
    pub fn apply_event(&mut self, event: &Event) -> Result<(), protocol::ParseError> {
        let before = self.want();
        match event.name.as_str() {
            "turn.changed" => {
                let t: protocol::TurnChanged = protocol::payload(event)?;
                self.active = t.current_player_id;
            }
            "hole.started" => {
                let h: protocol::HoleStarted = protocol::payload(event)?;
                self.hole = Some(HoleState {
                    number: h.hole_number,
                    par: h.par,
                    stroke_index: h.stroke_index,
                    yards: h.yards,
                    pin: h.pin,
                });
                self.shot = None;
                // The distance to the hole is a hole's own: until the next
                // shot is set up, nobody's is known.
                for player in self.players.values_mut() {
                    player.distance = None;
                }
            }
            "match.started" => {
                let m: protocol::MatchStarted = protocol::payload(event)?;
                self.round = m.match_id.map(|id| Round {
                    id,
                    course: m.course_name,
                    holes: m.hole_count,
                    par: m.total_par,
                    scoring: m.scoring_model,
                    paused: false,
                    ended: None,
                });
                self.scores.clear();
            }
            "match.paused" | "match.resumed" => {
                if let Some(round) = &mut self.round {
                    round.paused = event.name == "match.paused";
                }
            }
            "match.ended" => {
                let m: protocol::MatchEnded = protocol::payload(event)?;
                if let Some(round) = &mut self.round {
                    round.ended = Some(m.reason.unwrap_or_default());
                }
                for s in &m.standings {
                    if let (Some(id), Some(total)) = (s.player_id, s.total_strokes) {
                        let score = self.scores.entry(id).or_default();
                        score.total = Some(total);
                        score.to_par = s.to_par;
                    }
                }
            }
            "player.joined" => {
                let p: Player = protocol::payload(event)?;
                if let Some(id) = p.player_id
                    && self
                        .players
                        .insert(id, PlayerState::from_player(&p))
                        .is_none()
                {
                    self.order.push(id);
                }
            }
            "player.left" => {
                let p: protocol::PlayerLeft = protocol::payload(event)?;
                if let Some(id) = p.player_id {
                    self.players.remove(&id);
                    self.order.retain(|o| *o != id);
                }
            }
            "player.changed" => {
                let c: protocol::PlayerChanged = protocol::payload(event)?;
                // Field by field: what `current` leaves out is unchanged.
                if let (Some(id), Some(current)) = (c.player_id, c.current)
                    && let Some(player) = self.players.get_mut(&id)
                {
                    player.merge(&current);
                    if let Some(given) = event.data.get("current").and_then(|c| c.as_object()) {
                        player.clear_nulls(given);
                    }
                }
            }
            "player.club_changed" => {
                let c: protocol::ClubChanged = protocol::payload(event)?;
                if let Some(player) = c.player_id.and_then(|id| self.players.get_mut(&id)) {
                    player.club = c.current;
                    player.club_id = c.current_club_id;
                }
            }
            "player.bag_changed" => {
                let b: protocol::BagChanged = protocol::payload(event)?;
                if let Some(player) = b.player_id.and_then(|id| self.players.get_mut(&id)) {
                    player.bag = b.bag;
                }
            }
            "shot.initialized" => {
                let s: protocol::ShotInitialized = protocol::payload(event)?;
                if let Some(player) = s.player_id.and_then(|id| self.players.get_mut(&id)) {
                    if s.distance_to_hole_m.is_some() {
                        player.distance = s.distance_to_hole_m;
                    }
                    if s.ball_location.is_some() {
                        player.ball = s.ball_location;
                    }
                    if s.club.is_some() {
                        player.club = s.club;
                        player.club_id = s.club_id;
                    }
                }
                if let (Some(id), Some(replaces)) = (&s.shot_id, &s.replaces_shot_id)
                    && !id.is_empty()
                    && !replaces.is_empty()
                {
                    self.replaces.insert(id, replaces.clone());
                }
                self.shot = Some(ShotUp {
                    id: s.shot_id.filter(|id| !id.is_empty()),
                    player: s.player_id,
                    hole: s.hole_number,
                    stroke: s.stroke_number,
                });
            }
            "shot.replayed" => {
                let r: protocol::ShotReplayed = protocol::payload(event)?;
                if let (Some(id), Some(replaces)) = (&r.shot_id, &r.replaces_shot_id)
                    && !id.is_empty()
                    && !replaces.is_empty()
                {
                    self.replaces.insert(id, replaces.clone());
                }
            }
            "lie.changed" => {
                let l: protocol::LieChanged = protocol::payload(event)?;
                if let Some(id) = l.shot_id.as_deref().filter(|id| !id.is_empty()) {
                    self.lies.insert(
                        id,
                        protocol::SnapshotLie {
                            surface: l.surface,
                            slope_up_down_deg: l.slope_up_down_deg,
                            slope_left_right_deg: l.slope_left_right_deg,
                        },
                    );
                }
            }
            "shot.completed" => {
                let d: protocol::ShotCompleted = protocol::payload(event)?;
                // The distance to the hole is the next shot's to give
                // (`shot.initialized`, the snapshot): where this stroke
                // ended says nothing of the club about to be played.
                if let Some(player) = d.player_id.and_then(|id| self.players.get_mut(&id))
                    && d.end.is_some()
                {
                    player.ball = d.end;
                }
                self.shot = None;
            }
            "score.updated" => {
                let s: protocol::ScoreUpdated = protocol::payload(event)?;
                if let Some(id) = s.player_id.filter(|id| self.players.contains_key(id)) {
                    let score = self.scores.entry(id).or_default();
                    if s.total_strokes.is_some() {
                        score.total = s.total_strokes;
                        // The update carries no total to par.
                        score.to_par = None;
                    }
                    if let Some(hole) = s.hole_number {
                        score.holes.insert(
                            hole,
                            HoleScoreState {
                                strokes: s.hole_strokes,
                                to_par: s.gross_to_par,
                                net_to_par: s.net_to_par,
                                holed_out: s.holed_out,
                                r#final: s.is_final,
                            },
                        );
                    }
                }
            }
            "shot.ready_changed" => {
                self.readiness = Some(protocol::payload(event)?);
            }
            "context.invalidated" => {
                let c: protocol::ContextInvalidated = protocol::payload(event)?;
                tracing::debug!(
                    "pureplay_accessory: context invalidated ({})",
                    c.reason.as_deref().unwrap_or("no reason")
                );
                self.active = None;
                self.players.clear();
                self.order.clear();
                self.hole = None;
                self.readiness = None;
                self.round = None;
                self.shot = None;
                self.lies = Recent::default();
                self.scores.clear();
            }
            _ => {}
        }
        if self.want() != before {
            self.reported = true;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
pub(crate) mod tests;
