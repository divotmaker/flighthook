//! The card's game state on the bus, built from the [`Model`]:
//!
//! - `golfer.profile` per player, keyed `pureplay.<name>` (or
//!   `pureplay.player-<playerId>` for a player without a name): name,
//!   handedness (RH/LH; Unknown omitted) and bag (flighthook's codes, each
//!   once); `removed` when the player leaves.
//! - `game.round`: id, status, course, scoring, players (with `local`), the
//!   player up and the hole (`length` from Pureplay's yards). A round that
//!   ended goes out once with `status: ended` and its `end_reason`, then is
//!   removed; a round Pureplay no longer has (`context.invalidated`) is
//!   removed.
//! - `game.ball`: the ball about to be played, with only the fields Pureplay
//!   gives. `allowed_commands` is never set: the card takes no `game.aim`.
//! - `game.scores`: the whole object each time.
//!
//! Each goes out when it changed, and all of it again on `bus.sync`.

use std::collections::BTreeMap;

use flightrelay::{
    Course, Distance, EndReason, FrpBody, GameBall, GameRound, GameScores, GolferProfile, Hole,
    HoleScore, Lie, PlayerScore, Position, Removal, RoundPlayer, RoundStatus, StateKind,
};

use super::model::Model;

/// What the card last published.
#[derive(Debug, Default)]
pub struct Published {
    profiles: BTreeMap<String, GolferProfile>,
    round: Option<GameRound>,
    ball: Option<GameBall>,
    scores: Option<GameScores>,
}

impl Published {
    /// What changed since the last call, as messages to send, in order.
    /// A round that ended is taken off the model once its last state has
    /// gone out.
    pub fn changes(&mut self, model: &mut Model) -> Vec<FrpBody> {
        let mut out = Vec::new();
        let profiles = profiles(model);
        for (id, profile) in &profiles {
            if self.profiles.get(id) != Some(profile) {
                out.push(profile.clone().into());
            }
        }
        for id in self.profiles.keys() {
            if !profiles.contains_key(id) {
                out.push(Removal::of_id(StateKind::GolferProfile, id.clone()).into());
            }
        }
        self.profiles = profiles;

        let round = round(model);
        let ended = round
            .as_ref()
            .is_some_and(|r| r.status == RoundStatus::Ended);
        if round != self.round {
            match &round {
                Some(r) => out.push(r.clone().into()),
                None if self.round.is_some() => out.push(Removal::of(StateKind::GameRound).into()),
                None => {}
            }
        }
        if ended {
            // The removal of the round removes its ball and scores too.
            out.push(Removal::of(StateKind::GameRound).into());
            // The round's players go with it.
            model.end_round();
            out.extend(
                std::mem::take(&mut self.profiles)
                    .into_keys()
                    .map(|id| Removal::of_id(StateKind::GolferProfile, id).into()),
            );
            self.round = None;
            self.ball = None;
            self.scores = None;
            return out;
        }
        let gone = self.round.is_some() && round.is_none();
        self.round = round;
        if gone {
            self.ball = None;
            self.scores = None;
            return out;
        }

        let ball = ball(model);
        if ball != self.ball {
            match &ball {
                Some(b) => out.push(b.clone().into()),
                None if self.ball.is_some() => out.push(Removal::of(StateKind::GameBall).into()),
                None => {}
            }
            self.ball = ball;
        }
        let scores = scores(model, self.round.as_ref());
        if scores != self.scores {
            match &scores {
                Some(s) => out.push(s.clone().into()),
                None if self.scores.is_some() => {
                    out.push(Removal::of(StateKind::GameScores).into());
                }
                None => {}
            }
            self.scores = scores;
        }
        out
    }

    /// Everything the card holds, for `bus.sync`.
    #[must_use]
    pub fn all(&self) -> Vec<FrpBody> {
        let mut out: Vec<FrpBody> = self.profiles.values().cloned().map(Into::into).collect();
        out.extend(self.round.clone().map(Into::into));
        out.extend(self.ball.clone().map(Into::into));
        out.extend(self.scores.clone().map(Into::into));
        out
    }

    /// The connection ended: the removals of everything published.
    pub fn clear(&mut self) -> Vec<FrpBody> {
        let mut out: Vec<FrpBody> = self
            .profiles
            .keys()
            .map(|id| Removal::of_id(StateKind::GolferProfile, id.clone()).into())
            .collect();
        if self.round.is_some() {
            out.push(Removal::of(StateKind::GameRound).into());
        } else {
            if self.ball.is_some() {
                out.push(Removal::of(StateKind::GameBall).into());
            }
            if self.scores.is_some() {
                out.push(Removal::of(StateKind::GameScores).into());
            }
        }
        *self = Self::default();
        out
    }
}

/// A position from Pureplay's `[x, y, z]` in meters.
fn position(p: [f64; 3]) -> Option<Position> {
    p.iter().all(|v| v.is_finite()).then(|| {
        Position::new(
            Distance::Meters(p[0]),
            Distance::Meters(p[1]),
            Distance::Meters(p[2]),
        )
    })
}

fn meters(v: Option<f64>) -> Option<Distance> {
    v.filter(|v| v.is_finite()).map(Distance::Meters)
}

/// Every player's profile, by golfer id.
fn profiles(model: &Model) -> BTreeMap<String, GolferProfile> {
    let mut out = BTreeMap::new();
    for (id, player) in model.players() {
        let Some(golfer) = model.golfer(*id) else {
            continue;
        };
        let name = player
            .name
            .clone()
            .unwrap_or_else(|| format!("Player {id}"));
        let mut profile = GolferProfile::new(golfer.clone(), name);
        profile.handedness = player.hand();
        profile.bag = player.clubs();
        out.insert(golfer.as_str().to_owned(), profile);
    }
    out
}

fn round(model: &Model) -> Option<GameRound> {
    let r = model.round()?;
    let status = if r.ended.is_some() {
        RoundStatus::Ended
    } else if r.paused {
        RoundStatus::Paused
    } else {
        RoundStatus::Playing
    };
    let course = (r.course.is_some() || r.holes.is_some() || r.par.is_some()).then(|| Course {
        name: r.course.clone(),
        holes: r.holes,
        par: r.par,
    });
    let players = model
        .order()
        .iter()
        .filter_map(|id| Some((id, model.player(*id)?)))
        .filter_map(|(id, p)| {
            Some(RoundPlayer {
                golfer: model.golfer(*id)?,
                name: p.name.clone().unwrap_or_else(|| format!("Player {id}")),
                team: p.team,
                handicap: p.handicap.filter(|h| h.is_finite()),
                local: p.local,
            })
        })
        .collect();
    let hole = model.hole().and_then(|h| {
        Some(Hole {
            number: h.number?,
            par: h.par,
            stroke_index: h.stroke_index,
            length: h.yards.map(|y| Distance::Yards(f64::from(y))),
            pin: h.pin.and_then(position),
        })
    });
    Some(GameRound {
        id: r.id.clone(),
        status,
        end_reason: r.ended.as_deref().and_then(|reason| match reason {
            "completed" => Some(EndReason::Completed),
            "abandoned" => Some(EndReason::Abandoned),
            _ => None,
        }),
        course,
        scoring: r.scoring.as_deref().map(scoring),
        players,
        up: model.active().and_then(|id| model.golfer(id)),
        hole,
    })
}

/// FRP's scoring name for Pureplay's `scoringModel`.
fn scoring(model: &str) -> String {
    match model {
        "Stroke" => "stroke".into(),
        "Stableford" => "stableford".into(),
        "MatchPlay" => "match_play".into(),
        other => other.to_ascii_lowercase(),
    }
}

/// FRP's surface name for Pureplay's lie: lowercase, words joined by `_`.
#[must_use]
pub fn surface(lie: &str) -> String {
    let mut out = String::new();
    for (i, c) in lie.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn ball(model: &Model) -> Option<GameBall> {
    model.round()?;
    let id = model.active()?;
    let shot = model
        .shot_up()
        .filter(|s| s.player.is_none_or(|p| p == id))?;
    let hole = shot.hole.or_else(|| model.hole().and_then(|h| h.number))?;
    let player = model.player(id)?;
    // Only the lie of the shot that is up: no surface or slope carries
    // over from another shot.
    let lie = model.lie().and_then(|l| {
        let slope = |v: Option<f64>| v.filter(|v| v.is_finite());
        let lie = Lie {
            surface: l.surface.as_deref().map(surface),
            slope_up_down: slope(l.slope_up_down_deg),
            slope_left_right: slope(l.slope_left_right_deg),
        };
        (lie != Lie::default()).then_some(lie)
    });
    Some(GameBall {
        golfer: model.golfer(id)?,
        hole,
        stroke_number: shot.stroke?,
        allowed_commands: None,
        lie,
        position: player.ball.and_then(position),
        aim: player.aim.and_then(position).map(Some),
        distance_to_hole: meters(player.distance),
        decision: None,
    })
}

fn scores(model: &Model, round: Option<&GameRound>) -> Option<GameScores> {
    let round = round?;
    if model.scores().is_empty() {
        return None;
    }
    let players = model
        .scores()
        .iter()
        .filter_map(|(id, s)| {
            Some(PlayerScore {
                golfer: model.golfer(*id)?,
                strokes: s.total,
                to_par: s.to_par,
                net_to_par: None,
                points: None,
                rank: None,
                holes: s
                    .holes
                    .iter()
                    .map(|(number, h)| HoleScore {
                        number: *number,
                        strokes: h.strokes,
                        putts: None,
                        to_par: h.to_par,
                        net_to_par: h.net_to_par,
                        points: None,
                        result: None,
                        holed_out: h.holed_out,
                        r#final: h.r#final,
                    })
                    .collect(),
            })
        })
        .collect();
    Some(GameScores {
        round: round.id.clone(),
        players,
    })
}
