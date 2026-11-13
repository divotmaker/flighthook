//! The practice actor: flighthook's `practice.block`s.
//!
//! [`Practice`] is pure bookkeeping with no I/O. The system actor drives it
//! from the bus in bus order (the session's turn, every `shot.trigger`,
//! `practice.new_block`) and publishes every [`BlockChange`] it returns.
//!
//! The rules:
//!
//! - One open block per resolved golfer, `number` counted from 1 per golfer within a
//!   session. `session` starts at 1 each run.
//! - A block that has had no shot yet is **empty**. A shot counts for the
//!   open block of the golfer up at its `shot.trigger` and holds it while in
//!   flight; one that finishes with no data message (`shot.launch`,
//!   `shot.flight`, `shot.club`, `shot.impact`), a no-read, counts for
//!   nothing at its `shot.finished`, so it leaves no gap in the numbers.
//!   Nor does a shot that never finishes: its count goes back when its
//!   launch monitor is gone ([`Practice::gone`]) or after the shot timeout
//!   ([`Practice::expire`]), as the store then keeps nothing of it. Only
//!   the launch monitor that sent the trigger finishes its shot or gives it
//!   launch, club or impact; a flight may come from any actor.
//! - Start: a block opens for the golfer up with the turn's club.
//! - A turn change, for the golfer up: no open block opens one; an empty
//!   open block is republished in place (same `id` and `number`) with the
//!   turn's club and golfer; an open block with shots and another club is
//!   closed (removed) and a new one opens. The same club keeps the block.
//!   Other golfers' open blocks stay open.
//! - `practice.new_block` for a golfer (default: the golfer up): an empty
//!   open block is left as it is; otherwise it is closed and a new one
//!   opens with that golfer's own club (their open block's club; the turn's
//!   club when they have no block).
//! - `practice.new_block { new_session: true }` closes every open block,
//!   counts `session` up and opens a block for the golfer up.
//!
//! - A golfer coming up after no golfer takes over the no-golfer block when
//!   it has had no shot and they have no open block (same `id` and
//!   `number`, republished with their id); when they have one, the empty
//!   no-golfer block closes. So the start with no golfer leaves no empty
//!   block and no number gap.
//!
//! Golfers are keyed by the resolved golfer (the lookup rule,
//! [`flighthook::FlighthookConfig::golfer_key`], read against the live
//! config each time): a game id a golfer lists shares that golfer's open
//! block and numbers. A block's wire `golfer` is the raw id of the turn it
//! was opened or last republished under.

use std::collections::HashMap;

use flightrelay::{GolferId, GolferTurn, PracticeBlock};

use super::config::ConfigView;

/// The key a golfer's blocks are counted under: the resolved golfer's own
/// id, else the raw id; `None` for no golfer.
type Key = Option<String>;

/// What the practice actor publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockChange {
    /// A block opened or republished in place.
    Open(PracticeBlock),
    /// A block closed: the removal of the `practice.block` with this id.
    Closed(String),
}

/// An open block and whether a trigger has counted for it.
#[derive(Debug, Clone)]
struct Open {
    block: PracticeBlock,
    shots: u32,
}

/// The practice actor's state.
#[derive(Debug)]
pub struct Practice {
    session: u32,
    /// The open block of each resolved golfer.
    open: HashMap<Key, Open>,
    /// The last block number given out per resolved golfer in this session.
    numbers: HashMap<Key, u32>,
    /// The golfers, for the lookup rule; `None` keys by the raw id.
    config: Option<ConfigView>,
    /// The turn in effect.
    turn: GolferTurn,
    /// Shots triggered and not finished yet, oldest first.
    pending: std::collections::VecDeque<Pending>,
    /// Blocks closed while shots of theirs were still in flight, by id.
    closed: HashMap<String, Closed>,
}

/// A block closed with shots still in flight: if they all give their counts
/// back, it ended empty.
#[derive(Debug, Clone)]
struct Closed {
    key: Key,
    /// The block's wire `golfer`, to key it again when the golfers change.
    golfer: Option<GolferId>,
    number: u32,
    session: u32,
    shots: u32,
}

/// A triggered shot that has not finished.
#[derive(Debug, Clone)]
struct Pending {
    shot_id: String,
    /// The block it counts for.
    block: String,
    /// Its launch monitor (the trigger's sender).
    from: String,
    /// A data message arrived: a finish keeps the count.
    data: bool,
    at_ms: u64,
}

/// Triggered shots remembered until their finish.
const MAX_PENDING: usize = 64;

impl Practice {
    /// A practice actor following `turn`, with its first block opened,
    /// keying golfers by their raw id. Returns the actor and the block to
    /// publish.
    #[cfg(test)]
    #[must_use]
    pub fn start(turn: GolferTurn, now_ms: u64) -> (Self, Vec<BlockChange>) {
        Self::open_with(None, turn, now_ms)
    }

    /// [`Self::start`], keying golfers by the lookup rule on `config`.
    #[must_use]
    pub fn with_config(
        config: ConfigView,
        turn: GolferTurn,
        now_ms: u64,
    ) -> (Self, Vec<BlockChange>) {
        Self::open_with(Some(config), turn, now_ms)
    }

    fn open_with(
        config: Option<ConfigView>,
        turn: GolferTurn,
        now_ms: u64,
    ) -> (Self, Vec<BlockChange>) {
        let mut practice = Self {
            session: 1,
            open: HashMap::new(),
            numbers: HashMap::new(),
            config,
            turn,
            pending: std::collections::VecDeque::new(),
            closed: HashMap::new(),
        };
        let golfer = practice.turn.golfer.clone();
        let club = practice.turn.club;
        let changes = vec![practice.open_new(golfer, club, now_ms)];
        (practice, changes)
    }

    /// The key `golfer`'s blocks are counted under.
    fn key(&self, golfer: Option<&GolferId>) -> Key {
        self.key_of(golfer.map(GolferId::as_str))
    }

    /// The current session number.
    #[cfg(test)]
    #[must_use]
    pub fn session(&self) -> u32 {
        self.session
    }

    /// Every open block, for `bus.sync`, in a stable order (by number,
    /// then id).
    #[must_use]
    pub fn open_blocks(&self) -> Vec<PracticeBlock> {
        let mut blocks: Vec<PracticeBlock> = self.open.values().map(|o| o.block.clone()).collect();
        blocks.sort_by(|a, b| a.number.cmp(&b.number).then_with(|| a.id.cmp(&b.id)));
        blocks
    }

    /// The open block of the golfer up, if any.
    #[cfg(test)]
    #[must_use]
    pub fn current(&self) -> Option<&PracticeBlock> {
        self.open
            .get(&self.key(self.turn.golfer.as_ref()))
            .map(|o| &o.block)
    }

    /// The session published a new turn.
    pub fn turn(&mut self, turn: GolferTurn, now_ms: u64) -> Vec<BlockChange> {
        let from = std::mem::replace(&mut self.turn, turn).golfer;
        let golfer = self.turn.golfer.clone();
        let key = self.key(golfer.as_ref());
        let club = self.turn.club;
        let mut changes = Vec::new();
        // A golfer comes up after no golfer: the no-golfer block that has
        // had no shot becomes theirs when they have none (same id and
        // number), else it closes.
        if from.is_none()
            && key.is_some()
            && self.open.get(&None).is_some_and(|o| o.shots == 0)
            && let Some(empty) = self.open.remove(&None)
        {
            if self.open.contains_key(&key) {
                changes.push(BlockChange::Closed(empty.block.id));
            } else {
                // Its number goes back to the no-golfer count, and it takes
                // the golfer's next number.
                if let Some(n) = self.numbers.get_mut(&None) {
                    *n = n.saturating_sub(1);
                }
                let number = self.numbers.entry(key.clone()).or_insert(0);
                *number += 1;
                let mut empty = empty;
                empty.block.number = *number;
                // Its golfer is still none: republished below with theirs.
                self.open.insert(key.clone(), empty);
            }
        }
        let Some(open) = self.open.get_mut(&key) else {
            changes.push(self.open_new(golfer, club, now_ms));
            return changes;
        };
        if open.shots == 0 {
            if open.block.club == club && open.block.golfer == golfer {
                return changes;
            }
            open.block.club = club;
            open.block.golfer.clone_from(&golfer);
            changes.push(BlockChange::Open(open.block.clone()));
            return changes;
        }
        if open.block.club == club {
            if open.block.golfer == golfer {
                return changes;
            }
            open.block.golfer.clone_from(&golfer);
            changes.push(BlockChange::Open(open.block.clone()));
            return changes;
        }
        let closed = open.block.id.clone();
        self.remember_closed(&key);
        changes.push(BlockChange::Closed(closed));
        changes.push(self.open_new(golfer, club, now_ms));
        changes
    }

    /// The golfers changed (a save): every open block is keyed again by the
    /// lookup rule from its wire `golfer`, so each golfer has at most one
    /// open block.
    ///
    /// Where two or more open blocks now belong to one golfer, one stays
    /// open: for the golfer up one with the turn's club, else one with
    /// shots, else the highest number. The others close; an empty one gives
    /// its number back. A block that moved to another golfer keeps its
    /// number when it has shots; one that is empty takes its golfer's next
    /// number. A golfer that took over another's ids carries the higher
    /// count forward. The golfer up's block then follows the turn as on a
    /// turn change (republished under the turn's raw id, or closed for a
    /// new one on the turn's club). Returns the closed blocks, then the
    /// opened and renumbered ones.
    pub fn rekey(&mut self, now_ms: u64) -> Vec<BlockChange> {
        let up = self.key(self.turn.golfer.as_ref());
        let mut groups: HashMap<Key, Vec<(Key, Open)>> = HashMap::new();
        for (old, open) in std::mem::take(&mut self.open) {
            let new = self.key(open.block.golfer.as_ref());
            groups.entry(new).or_default().push((old, open));
        }
        let golfers: Vec<(String, Key)> = self
            .closed
            .iter()
            .map(|(id, c)| (id.clone(), self.key(c.golfer.as_ref())))
            .collect();
        for (id, key) in golfers {
            if let Some(c) = self.closed.get_mut(&id) {
                c.key = key;
            }
        }
        let mut keys: Vec<Key> = groups.keys().cloned().collect();
        keys.sort();
        let mut closing: Vec<(u32, String)> = Vec::new();
        let mut renumbered = Vec::new();
        for key in keys {
            let Some(mut blocks) = groups.remove(&key) else {
                continue;
            };
            if blocks.len() == 1 && blocks[0].0 == key {
                let (_, open) = blocks.remove(0);
                self.open.insert(key, open);
                continue;
            }
            // Empty blocks give their numbers back first.
            for (old, open) in &blocks {
                if open.shots == 0
                    && let Some(n) = self.numbers.get_mut(old)
                    && *n == open.block.number
                {
                    *n -= 1;
                }
            }
            // The count goes on from the golfer's own, the ids it took over
            // and the blocks with shots it holds now.
            let mut count = self.numbers.get(&key).copied().unwrap_or(0);
            for (old, open) in &blocks {
                if *old != key
                    && self.key_of(old.as_deref()) == key
                    && let Some(n) = self.numbers.get(old)
                {
                    count = count.max(*n);
                }
                if open.shots > 0 {
                    count = count.max(open.block.number);
                }
            }
            let is_up = key == up;
            let turn = &self.turn;
            // Kept: the block of the club in play for the golfer up, then a
            // block with shots, then the golfer's own block, then the
            // highest number.
            blocks.sort_by_key(|(old, o)| {
                (
                    is_up && o.block.club == turn.club,
                    o.shots > 0,
                    *old == key,
                    o.block.number,
                    o.block.id.clone(),
                )
            });
            let Some((_, mut kept)) = blocks.pop() else {
                continue;
            };
            for (_, open) in blocks {
                if self.pending.iter().any(|p| p.block == open.block.id) {
                    self.closed.insert(
                        open.block.id.clone(),
                        Closed {
                            key: key.clone(),
                            golfer: open.block.golfer.clone(),
                            number: open.block.number,
                            session: open.block.session,
                            shots: open.shots,
                        },
                    );
                }
                closing.push((open.block.number, open.block.id));
            }
            if kept.shots == 0 {
                count += 1;
                if kept.block.number != count {
                    kept.block.number = count;
                    renumbered.push(BlockChange::Open(kept.block.clone()));
                }
            }
            self.numbers.insert(key.clone(), count);
            self.open.insert(key, kept);
        }
        closing.sort();
        let mut changes: Vec<BlockChange> = closing
            .into_iter()
            .map(|(_, id)| BlockChange::Closed(id))
            .collect();
        changes.extend(renumbered);
        let turn = self.turn.clone();
        changes.extend(self.turn(turn, now_ms));
        // A block republished twice goes out once, as it is last.
        let mut seen = std::collections::HashSet::new();
        let mut kept = Vec::new();
        for change in changes.into_iter().rev() {
            match &change {
                BlockChange::Open(b) if !seen.insert(b.id.clone()) => {}
                _ => kept.push(change),
            }
        }
        kept.reverse();
        kept
    }

    /// The key an id is counted under, from a key's text.
    fn key_of(&self, id: Option<&str>) -> Key {
        match &self.config {
            Some(config) => config.read().golfer_key(id),
            None => id.map(str::to_owned),
        }
    }

    /// The open block of `key` is about to close: while shots of it are in
    /// flight, remember it, so their counts can still come back.
    fn remember_closed(&mut self, key: &Key) {
        let Some(open) = self.open.get(key) else {
            return;
        };
        if self.pending.iter().any(|p| p.block == open.block.id) {
            self.closed.insert(
                open.block.id.clone(),
                Closed {
                    key: key.clone(),
                    golfer: open.block.golfer.clone(),
                    number: open.block.number,
                    session: open.block.session,
                    shots: open.shots,
                },
            );
        }
    }

    /// Forget closed blocks no shot in flight belongs to.
    fn prune_closed(&mut self) {
        let pending = &self.pending;
        self.closed
            .retain(|id, _| pending.iter().any(|p| &p.block == id));
    }

    /// A `shot.trigger` of the launch monitor `from` arrived at `now_ms`:
    /// the shot counts for the golfer up's open block until it finishes
    /// with no data ([`Self::finished`]) or never finishes. Returns that
    /// block.
    pub fn trigger(&mut self, shot_id: &str, from: &str, now_ms: u64) -> Option<String> {
        let key = self.key(self.turn.golfer.as_ref());
        let open = self.open.get_mut(&key)?;
        let block = open.block.id.clone();
        if !self.pending.iter().any(|p| p.shot_id == shot_id) {
            open.shots += 1;
            if self.pending.len() == MAX_PENDING {
                self.pending.pop_front();
            }
            self.pending.push_back(Pending {
                shot_id: shot_id.to_owned(),
                block: block.clone(),
                from: from.to_owned(),
                data: false,
                at_ms: now_ms,
            });
        }
        Some(block)
    }

    /// A data message of a triggered shot from `from`: it keeps its count.
    /// `any_sender` for a flight, which any actor may add; launch, club and
    /// impact count only from the shot's launch monitor.
    pub fn data(&mut self, shot_id: &str, from: &str, any_sender: bool) {
        if let Some(p) = self
            .pending
            .iter_mut()
            .find(|p| p.shot_id == shot_id && (any_sender || p.from == from))
        {
            p.data = true;
        }
    }

    /// The shot's launch monitor `from` finished it: one that had no data
    /// message gives its count back to its block. Returns what that
    /// changed ([`Self::give_back`]).
    pub fn finished(&mut self, shot_id: &str, from: &str) -> Vec<BlockChange> {
        let Some(at) = self
            .pending
            .iter()
            .position(|p| p.shot_id == shot_id && p.from == from)
        else {
            return Vec::new();
        };
        let mut changes = Vec::new();
        if let Some(p) = self.pending.remove(at)
            && !p.data
        {
            changes.extend(self.give_back(&p.block));
        }
        self.prune_closed();
        changes
    }

    /// The launch monitor `actor` is gone: its unfinished shots give their
    /// counts back. Returns what that changed.
    pub fn gone(&mut self, actor: &str) -> Vec<BlockChange> {
        let (gone, kept): (Vec<Pending>, Vec<Pending>) =
            self.pending.drain(..).partition(|p| p.from == actor);
        self.pending = kept.into();
        let changes = gone
            .iter()
            .filter_map(|p| self.give_back(&p.block))
            .collect();
        self.prune_closed();
        changes
    }

    /// The shots triggered more than the shot timeout before `now_ms` give
    /// their counts back: they never finished, and the store dropped them.
    /// Returns what that changed.
    pub fn expire(&mut self, now_ms: u64) -> Vec<BlockChange> {
        let limit = u64::try_from(flighthook::SHOT_TIMEOUT.as_millis()).unwrap_or(u64::MAX);
        let mut changes = Vec::new();
        while self
            .pending
            .front()
            .is_some_and(|p| now_ms.saturating_sub(p.at_ms) > limit)
        {
            if let Some(p) = self.pending.pop_front() {
                changes.extend(self.give_back(&p.block));
            }
        }
        self.prune_closed();
        changes
    }

    /// A shot of `block` counts for nothing after all. A block that was
    /// closed with shots in flight and so ends empty gives its number back:
    /// its golfer's open block, when it is the next one and still empty,
    /// takes that number (republished in place, returned).
    fn give_back(&mut self, block: &str) -> Option<BlockChange> {
        if let Some(open) = self.open.values_mut().find(|o| o.block.id == block) {
            open.shots = open.shots.saturating_sub(1);
            return None;
        }
        let closed = self.closed.get_mut(block)?;
        closed.shots = closed.shots.saturating_sub(1);
        if closed.shots > 0 {
            return None;
        }
        let closed = self.closed.remove(block)?;
        let open = self.open.get_mut(&closed.key)?;
        let next = self.numbers.get_mut(&closed.key)?;
        if open.shots > 0
            || open.block.session != closed.session
            || open.block.number != closed.number + 1
            || *next != open.block.number
        {
            return None;
        }
        open.block.number = closed.number;
        *next = closed.number;
        Some(BlockChange::Open(open.block.clone()))
    }

    /// `practice.new_block`: `golfer` is the command's (`None` for the
    /// golfer up, `Some(None)` for the block of shots with no golfer),
    /// resolved by the lookup rule. The new block carries the turn's raw
    /// id when that golfer is up, else the raw id of their open block, else
    /// the command's.
    pub fn new_block(
        &mut self,
        golfer: Option<Option<GolferId>>,
        new_session: bool,
        now_ms: u64,
    ) -> Vec<BlockChange> {
        if new_session {
            return self.new_session(now_ms);
        }
        let golfer = golfer.unwrap_or_else(|| self.turn.golfer.clone());
        let key = self.key(golfer.as_ref());
        let up = self.key(self.turn.golfer.as_ref()) == key;
        let (club, raw) = match self.open.get(&key) {
            Some(open) if open.shots == 0 => return Vec::new(),
            Some(open) => (open.block.club, open.block.golfer.clone()),
            None => (self.turn.club, golfer),
        };
        let raw = if up { self.turn.golfer.clone() } else { raw };
        let mut changes = Vec::new();
        self.remember_closed(&key);
        if let Some(open) = self.open.remove(&key) {
            changes.push(BlockChange::Closed(open.block.id));
        }
        changes.push(self.open_new(raw, club, now_ms));
        changes
    }

    /// Close every open block, count the session up and open a block for
    /// the golfer up.
    pub fn new_session(&mut self, now_ms: u64) -> Vec<BlockChange> {
        let mut closed: Vec<Open> = self.open.drain().map(|(_, o)| o).collect();
        closed.sort_by(|a, b| a.block.number.cmp(&b.block.number));
        let mut changes: Vec<BlockChange> = closed
            .into_iter()
            .map(|o| BlockChange::Closed(o.block.id))
            .collect();
        self.session += 1;
        self.numbers.clear();
        let golfer = self.turn.golfer.clone();
        let club = self.turn.club;
        changes.push(self.open_new(golfer, club, now_ms));
        changes
    }

    fn open_new(
        &mut self,
        golfer: Option<GolferId>,
        club: Option<flightrelay::Club>,
        now_ms: u64,
    ) -> BlockChange {
        let key = self.key(golfer.as_ref());
        let number = self.numbers.entry(key.clone()).or_insert(0);
        *number += 1;
        let block = PracticeBlock {
            id: uuid::Uuid::new_v4().to_string(),
            golfer: golfer.clone(),
            club,
            number: *number,
            session: self.session,
            started_at: now_ms,
        };
        self.open.insert(
            key,
            Open {
                block: block.clone(),
                shots: 0,
            },
        );
        BlockChange::Open(block)
    }
}

/// Host clock, milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use flightrelay::{Club, Handedness, Mode};

    use super::*;

    /// A shot with data for the golfer up's open block; the block it
    /// counted for.
    fn hit(p: &mut Practice) -> Option<String> {
        let id = crate::state::config::generate_id();
        let block = p.trigger(&id, "lm", 0);
        p.data(&id, "lm", false);
        p.finished(&id, "lm");
        block
    }

    /// A trigger that finishes with no data counts for nothing: the empty
    /// block is republished in place for another club.
    #[test]
    fn a_no_read_leaves_the_block_empty() {
        let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        let first = opened(&start)[0].clone();
        assert_eq!(p.trigger("miss", "lm", 0), Some(first.id.clone()));
        p.finished("miss", "lm");
        let changes = p.turn(turn(None, Some(Club::Driver)), 1);
        let again = &opened(&changes)[0];
        assert_eq!(changes.len(), 1, "no block closed");
        assert_eq!((again.id.as_str(), again.number), (first.id.as_str(), 1));
        // A shot with data keeps its count past its finish.
        assert_eq!(p.trigger("hit", "lm", 0), Some(first.id.clone()));
        p.data("hit", "lm", false);
        p.data("hit", "lm", false);
        p.finished("hit", "lm");
        let changes = p.turn(turn(None, Some(Club::Iron7)), 2);
        assert_eq!(closed(&changes), vec![first.id.clone()]);
        assert_eq!(opened(&changes)[0].number, 2);
    }

    /// A shot that never finishes gives its count back when its launch
    /// monitor is gone, or after the shot timeout, even with data; another
    /// actor's launch or finish does not count as the launch monitor's.
    #[test]
    fn a_shot_that_never_finishes_leaves_no_gap() {
        let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        let first = opened(&start)[0].clone();
        p.trigger("dies", "remote-lm", 0);
        p.data("dies", "remote-lm", false);
        p.gone("remote-lm");
        let changes = p.turn(turn(None, Some(Club::Driver)), 1);
        assert_eq!(changes.len(), 1, "no block closed");
        assert_eq!(opened(&changes)[0].number, 1);

        p.trigger("stalls", "lm", 1000);
        p.data("stalls", "other", false);
        p.finished("stalls", "other");
        p.expire(1000 + 44_000);
        p.data("stalls", "other", true);
        assert!(
            p.pending.iter().any(|q| q.shot_id == "stalls" && q.data),
            "a flight counts from anyone, and only the launch monitor finishes"
        );
        p.expire(1000 + 46_000);
        let changes = p.turn(turn(None, Some(Club::Iron7)), 2);
        assert_eq!(changes.len(), 1, "still empty: no block closed");
        assert_eq!(opened(&changes)[0].id, first.id);
    }

    fn turn(golfer: Option<&str>, club: Option<Club>) -> GolferTurn {
        GolferTurn::new(
            golfer.map(|g| g.parse().unwrap()),
            club,
            Handedness::Right,
            Mode::Full,
        )
    }

    fn opened(changes: &[BlockChange]) -> Vec<PracticeBlock> {
        changes
            .iter()
            .filter_map(|c| match c {
                BlockChange::Open(b) => Some(b.clone()),
                BlockChange::Closed(_) => None,
            })
            .collect()
    }

    fn closed(changes: &[BlockChange]) -> Vec<String> {
        changes
            .iter()
            .filter_map(|c| match c {
                BlockChange::Closed(id) => Some(id.clone()),
                BlockChange::Open(_) => None,
            })
            .collect()
    }

    /// The start with no golfer, then the first golfer up: the empty
    /// no-golfer block becomes theirs (same id, number 1), so no empty block
    /// stays behind; with shots it stays the no-golfer block.
    #[test]
    fn the_first_golfer_takes_over_the_empty_no_golfer_block() {
        let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        let first = opened(&start)[0].clone();
        let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Iron7)), 1);
        assert_eq!(changes.len(), 1, "{changes:?}");
        let b = &opened(&changes)[0];
        assert_eq!((b.id.as_str(), b.number), (first.id.as_str(), 1));
        assert_eq!(
            b.golfer.as_ref().map(GolferId::as_str),
            Some("flighthook.e")
        );
        assert_eq!(p.open_blocks().len(), 1);
        // Their next block is number 2.
        hit(&mut p);
        let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Driver)), 2);
        assert_eq!(opened(&changes)[0].number, 2);

        // With a shot, the no-golfer block stays as it is.
        let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        hit(&mut p);
        let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Iron7)), 1);
        let b = &opened(&changes)[0];
        assert_eq!(b.number, 1);
        assert_eq!(p.open_blocks().len(), 2);
    }

    /// The empty no-golfer block the first golfer takes over
    /// becomes their Set 1 even after no-golfer Sets with shots, and the
    /// no-golfer numbers go on where they were.
    #[test]
    fn the_first_golfers_sets_start_at_set_1() {
        let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        hit(&mut p);
        let changes = p.turn(turn(None, Some(Club::Driver)), 1);
        let dr = opened(&changes)[0].clone();
        assert_eq!(dr.number, 2);
        let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Driver)), 2);
        assert_eq!(changes.len(), 1, "{changes:?}");
        let b = &opened(&changes)[0];
        assert_eq!((b.id.as_str(), b.number), (dr.id.as_str(), 1));
        hit(&mut p);
        let changes = p.turn(turn(Some("flighthook.e"), Some(Club::Iron7)), 3);
        assert_eq!(opened(&changes)[0].number, 2, "Eric's Set 2");
        // No golfer again: the no-golfer Set 1 exists, so this is Set 2.
        let changes = p.turn(turn(None, Some(Club::Iron7)), 4);
        let b = &opened(&changes)[0];
        assert_eq!((b.golfer.clone(), b.number), (None, 2));
    }

    /// A trigger, a club change (the block
    /// closes with the shot in flight), then the launch monitor dies or the
    /// shot times out. The closed block ends empty, so the new block takes
    /// its number back: no gap.
    #[test]
    fn a_club_change_before_the_monitor_dies_leaves_no_gap() {
        let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        p.trigger("dies", "remote-lm", 0);
        let changes = p.turn(turn(None, Some(Club::Driver)), 1);
        let dr = opened(&changes)[0].clone();
        assert_eq!(dr.number, 2);
        let changes = p.gone("remote-lm");
        assert_eq!(changes.len(), 1, "{changes:?}");
        let b = &opened(&changes)[0];
        assert_eq!((b.id.as_str(), b.number), (dr.id.as_str(), 1));
        hit(&mut p);
        let changes = p.turn(turn(None, Some(Club::Iron7)), 2);
        assert_eq!(opened(&changes)[0].number, 2);

        // The shot timeout gives it back the same way.
        let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        p.trigger("stalls", "lm", 0);
        let dr = opened(&p.turn(turn(None, Some(Club::Driver)), 1))[0].clone();
        let changes = p.expire(46_000);
        let b = &opened(&changes)[0];
        assert_eq!((b.id.as_str(), b.number), (dr.id.as_str(), 1));

        // A no-read finish too; one with data keeps the closed block.
        let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        p.trigger("miss", "lm", 0);
        let dr = opened(&p.turn(turn(None, Some(Club::Driver)), 1))[0].clone();
        let b = &opened(&p.finished("miss", "lm"))[0];
        assert_eq!((b.id.as_str(), b.number), (dr.id.as_str(), 1));
        let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        p.trigger("hit", "lm", 0);
        p.turn(turn(None, Some(Club::Driver)), 1);
        p.data("hit", "lm", false);
        assert!(p.finished("hit", "lm").is_empty());
        // The new block has a shot already: the gap stays, nothing moves.
        let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        p.trigger("dies", "remote-lm", 0);
        p.turn(turn(None, Some(Club::Driver)), 1);
        hit(&mut p);
        assert!(p.gone("remote-lm").is_empty());
    }

    #[test]
    fn start_opens_block_one_of_session_one_for_the_golfer_up() {
        let (p, changes) = Practice::start(turn(None, Some(Club::Iron7)), 5);
        let b = &opened(&changes)[0];
        assert_eq!(changes.len(), 1);
        assert_eq!(
            (b.session, b.number, b.club, b.golfer.clone()),
            (1, 1, Some(Club::Iron7), None)
        );
        assert_eq!(b.started_at, 5);
        assert_eq!(p.session(), 1);
        assert_eq!(p.open_blocks(), vec![b.clone()]);
    }

    #[test]
    fn club_scrolling_before_a_shot_republishes_the_empty_block_in_place() {
        let (mut p, start) = Practice::start(turn(None, None), 0);
        let first = opened(&start)[0].clone();
        for club in [Club::Driver, Club::Iron7, Club::PitchingWedge] {
            let changes = p.turn(turn(None, Some(club)), 1);
            let b = &opened(&changes)[0];
            assert_eq!(changes.len(), 1);
            assert_eq!(
                (b.id.as_str(), b.number, b.club),
                (first.id.as_str(), 1, Some(club))
            );
        }
    }

    #[test]
    fn a_turn_that_changes_neither_club_nor_golfer_publishes_nothing() {
        let (mut p, _) = Practice::start(turn(None, Some(Club::Driver)), 0);
        assert!(p.turn(turn(None, Some(Club::Driver)), 1).is_empty());
        hit(&mut p);
        assert!(p.turn(turn(None, Some(Club::Driver)), 1).is_empty());
    }

    #[test]
    fn another_club_after_a_shot_closes_the_block_and_opens_the_next_number() {
        let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        let first = opened(&start)[0].clone();
        assert_eq!(hit(&mut p), Some(first.id.clone()));
        let changes = p.turn(turn(None, Some(Club::Driver)), 1);
        assert_eq!(closed(&changes), vec![first.id.clone()]);
        let next = &opened(&changes)[0];
        assert_eq!((next.number, next.club), (2, Some(Club::Driver)));
        assert_ne!(next.id, first.id);
        assert!(matches!(changes[0], BlockChange::Closed(_)), "closed first");
    }

    #[test]
    fn returning_to_a_club_without_a_shot_gives_a_new_block() {
        let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        hit(&mut p);
        let dr = opened(&p.turn(turn(None, Some(Club::Driver)), 1))[0].clone();
        let back = p.turn(turn(None, Some(Club::Iron7)), 2);
        // The DR block is empty: republished in place as 7I, number 2.
        let b = &opened(&back)[0];
        assert_eq!(
            (b.id.as_str(), b.number, b.club),
            (dr.id.as_str(), 2, Some(Club::Iron7))
        );
        assert!(closed(&back).is_empty());
    }

    #[test]
    fn new_block_on_an_empty_block_is_a_no_op() {
        let (mut p, _) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        assert!(p.new_block(None, false, 1).is_empty());
        assert!(p.new_block(Some(None), false, 1).is_empty());
    }

    #[test]
    fn new_block_after_a_shot_rolls_to_the_next_number_on_the_same_club() {
        let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        hit(&mut p);
        let changes = p.new_block(None, false, 9);
        assert_eq!(closed(&changes), vec![opened(&start)[0].id.clone()]);
        let b = &opened(&changes)[0];
        assert_eq!(
            (b.number, b.club, b.session, b.started_at),
            (2, Some(Club::Iron7), 1, 9)
        );
    }

    #[test]
    fn new_block_for_a_golfer_not_up_uses_their_own_club() {
        let (mut p, _) = Practice::start(turn(Some("pureplay.local-1"), Some(Club::Iron7)), 0);
        hit(&mut p);
        p.turn(turn(Some("pureplay.local-2"), Some(Club::Driver)), 1);
        let one: GolferId = "pureplay.local-1".parse().unwrap();
        let changes = p.new_block(Some(Some(one.clone())), false, 2);
        let b = &opened(&changes)[0];
        assert_eq!(
            (b.golfer.clone(), b.club, b.number),
            (Some(one), Some(Club::Iron7), 2)
        );
        // A golfer with no block gets the turn's club.
        let three: GolferId = "pureplay.local-3".parse().unwrap();
        let b = &opened(&p.new_block(Some(Some(three.clone())), false, 3))[0];
        assert_eq!(
            (b.golfer.clone(), b.club, b.number),
            (Some(three), Some(Club::Driver), 1)
        );
    }

    #[test]
    fn other_golfers_blocks_stay_open_and_a_golfer_coming_back_continues() {
        let (mut p, start) = Practice::start(turn(Some("pureplay.local-1"), Some(Club::Iron7)), 0);
        let one = opened(&start)[0].clone();
        hit(&mut p);
        let two = p.turn(turn(Some("pureplay.local-2"), Some(Club::Driver)), 1);
        assert!(closed(&two).is_empty());
        assert_eq!(opened(&two)[0].number, 1, "numbers count per golfer");
        hit(&mut p);
        let back = p.turn(turn(Some("pureplay.local-1"), Some(Club::Iron7)), 2);
        assert!(back.is_empty(), "same club, same block: {back:?}");
        assert_eq!(hit(&mut p), Some(one.id.clone()));
        assert_eq!(p.open_blocks().len(), 2);
    }

    #[test]
    fn new_session_closes_every_block_and_counts_the_session_up() {
        let (mut p, _) = Practice::start(turn(Some("pureplay.local-1"), Some(Club::Iron7)), 0);
        hit(&mut p);
        p.turn(turn(Some("pureplay.local-2"), Some(Club::Driver)), 1);
        let changes = p.new_block(Some(None), true, 2);
        assert_eq!(closed(&changes).len(), 2);
        let b = &opened(&changes)[0];
        assert_eq!((b.session, b.number, b.club), (2, 1, Some(Club::Driver)));
        assert_eq!(p.session(), 2);
        assert_eq!(p.open_blocks(), vec![b.clone()]);
        // An empty block's new_session still rolls (it is not new_block).
        assert_eq!(closed(&p.new_session(3)).len(), 1);
        assert_eq!(p.session(), 3);
    }

    #[test]
    fn the_null_golfer_to_a_golfer_leaves_no_gaps() {
        let (mut p, _) = Practice::start(turn(None, None), 0);
        let changes = p.turn(turn(Some("flighthook.a"), Some(Club::Driver)), 1);
        let b = &opened(&changes)[0];
        assert_eq!(b.number, 1);
    }

    /// Eric (`flighthook.e`, linked to `pureplay.local-3`) and Sam.
    fn linked() -> ConfigView {
        let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
        eric.identifiers.push("pureplay.local-3".into());
        ConfigView::detached(flighthook::FlighthookConfig {
            golfers: vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")],
            ..flighthook::FlighthookConfig::default()
        })
    }

    /// A linked profile shares its golfer's block run: Eric's game id
    /// continues the block Eric opened (republished under the game id),
    /// and numbers count once for both ids.
    #[test]
    fn a_linked_profile_shares_its_golfers_block_run() {
        let (mut p, start) =
            Practice::with_config(linked(), turn(Some("flighthook.e"), Some(Club::Iron7)), 0);
        let one = opened(&start)[0].clone();
        hit(&mut p);
        let game = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 1);
        let b = &opened(&game)[0];
        assert_eq!(game.len(), 1, "republished in place: {game:?}");
        assert_eq!((b.id.as_str(), b.number), (one.id.as_str(), 1));
        assert_eq!(
            b.golfer.as_ref().map(GolferId::as_str),
            Some("pureplay.local-3"),
            "the wire carries the raw id it was last republished under"
        );
        assert_eq!(hit(&mut p), Some(one.id.clone()));
        // A club change under the game id rolls Eric's run to number 2.
        let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Driver)), 2);
        assert_eq!(closed(&next), vec![one.id.clone()]);
        assert_eq!(opened(&next)[0].number, 2);
        hit(&mut p);
        // New set for Eric by his own id while the game id is up: the new
        // block carries the turn's raw id, on his club.
        let roll = p.new_block(Some(Some("flighthook.e".parse().unwrap())), false, 3);
        let b = &opened(&roll)[0];
        assert_eq!(
            (b.number, b.club, b.golfer.as_ref().map(GolferId::as_str)),
            (3, Some(Club::Driver), Some("pureplay.local-3"))
        );
        assert_eq!(p.open_blocks().len(), 1, "one open block for Eric");
        // Unlinked, the same ids are two golfers.
        let (mut p, _) = Practice::start(turn(Some("flighthook.e"), Some(Club::Iron7)), 0);
        hit(&mut p);
        let game = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 1);
        assert_ne!(opened(&game)[0].id, one.id);
        assert_eq!(p.open_blocks().len(), 2);
    }

    /// Two golfers alternating keep their own blocks, with one club and
    /// with different clubs.
    #[test]
    fn two_golfers_alternating_each_keep_their_block() {
        for (eric_club, sam_club) in [(Club::Iron7, Club::Iron7), (Club::Iron7, Club::Driver)] {
            let (mut p, start) =
                Practice::with_config(linked(), turn(Some("flighthook.e"), Some(eric_club)), 0);
            let eric = opened(&start)[0].clone();
            hit(&mut p);
            let sam = opened(&p.turn(turn(Some("flighthook.s"), Some(sam_club)), 1))[0].clone();
            assert_eq!(sam.number, 1);
            for round in 0..3 {
                hit(&mut p);
                let back = p.turn(turn(Some("pureplay.local-3"), Some(eric_club)), 2 + round);
                assert!(closed(&back).is_empty(), "{back:?}");
                assert_eq!(hit(&mut p), Some(eric.id.clone()));
                let back = p.turn(turn(Some("flighthook.s"), Some(sam_club)), 2 + round);
                assert!(back.is_empty(), "{back:?}");
                assert_eq!(p.current().map(|b| b.id.clone()), Some(sam.id.clone()));
            }
            assert_eq!(p.open_blocks().len(), 2);
        }
    }

    /// The session and the practice actor together. Two
    /// golfers alternating by golfer-only picks (the header's), with one
    /// club and with different clubs: each gets their own club back and
    /// keeps their block; Eric's linked game id shares his set run.
    #[test]
    fn alternating_golfers_get_their_club_and_their_block_back() {
        use crate::state::session::Session;
        let pick = |golfer: &str| flightrelay::GolferSelect {
            golfer: Some(Some(golfer.parse().expect("id"))),
            ..flightrelay::GolferSelect::new("r")
        };
        let club = |c: Club| flightrelay::GolferSelect {
            club: Some(Some(c)),
            ..flightrelay::GolferSelect::new("r")
        };
        for (eric_club, sam_club) in [(Club::Iron7, Club::Iron7), (Club::Iron7, Club::Driver)] {
            let config = linked();
            let (_session, writer) = Session::with_config(config.clone());
            let (mut p, start) = Practice::with_config(config, writer.turn(), 0);
            let follow = |p: &mut Practice, turn: Option<GolferTurn>| {
                turn.map(|t| p.turn(t, 1)).unwrap_or_default()
            };
            follow(&mut p, writer.apply(&club(eric_club)));
            let eric = p.current().expect("eric's block").clone();
            assert_eq!(eric.id, opened(&start)[0].id, "empty block republished");
            hit(&mut p);
            follow(&mut p, writer.apply(&pick("flighthook.s")));
            follow(&mut p, writer.apply(&club(sam_club)));
            let sam = p.current().expect("sam's block").clone();
            assert_ne!(sam.id, eric.id);
            hit(&mut p);
            for _ in 0..3 {
                let changes = follow(&mut p, writer.apply(&pick("pureplay.local-3")));
                assert_eq!(writer.turn().club, Some(eric_club), "Eric's club back");
                assert!(closed(&changes).is_empty(), "{changes:?}");
                assert_eq!(hit(&mut p), Some(eric.id.clone()));
                let changes = follow(&mut p, writer.apply(&pick("flighthook.s")));
                assert_eq!(writer.turn().club, Some(sam_club), "Sam's club back");
                assert!(closed(&changes).is_empty(), "{changes:?}");
                assert_eq!(hit(&mut p), Some(sam.id.clone()));
                follow(&mut p, writer.apply(&pick("flighthook.e")));
                assert_eq!(p.current().map(|b| b.id.clone()), Some(eric.id.clone()));
            }
            assert_eq!(p.open_blocks().len(), 2);
            assert!(p.open_blocks().iter().all(|b| b.number == 1));
        }
    }

    /// Eric and Sam, nobody linked.
    fn unlinked() -> ConfigView {
        ConfigView::detached(flighthook::FlighthookConfig {
            golfers: vec![
                flighthook::GolferRecord::new("flighthook.e", "Eric"),
                flighthook::GolferRecord::new("flighthook.s", "Sam"),
            ],
            ..flighthook::FlighthookConfig::default()
        })
    }

    /// The open blocks by wire golfer: at most one each.
    fn one_per_golfer(p: &Practice) {
        let mut golfers: Vec<_> = p.open_blocks().into_iter().map(|b| b.golfer).collect();
        let all = golfers.len();
        golfers.sort();
        golfers.dedup();
        assert_eq!(golfers.len(), all, "{:?}", p.open_blocks());
    }

    /// A game id linked to Eric while it is up with an open Set: Eric's
    /// own open Set closes, the game id's Set goes on as his, and a club
    /// change then rolls it, so later shots land in the new Set.
    #[test]
    fn a_link_while_the_game_id_is_up_leaves_one_open_set() {
        let config = unlinked();
        let (mut p, start) = Practice::with_config(
            config.clone(),
            turn(Some("flighthook.e"), Some(Club::Driver)),
            0,
        );
        let eric = opened(&start)[0].clone();
        hit(&mut p);
        let game = opened(&p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 1))[0].clone();
        assert_eq!(game.number, 1);
        hit(&mut p);
        hit(&mut p);
        config.write().golfers[0]
            .identifiers
            .push("pureplay.local-3".into());
        let saved = p.rekey(2);
        assert_eq!(saved, vec![BlockChange::Closed(eric.id.clone())]);
        assert_eq!(p.open_blocks().len(), 1);
        assert_eq!(p.current().map(|b| b.id.clone()), Some(game.id.clone()));
        // The club change rolls the game id's Set, Eric's run counting on.
        let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron8)), 3);
        assert_eq!(closed(&next), vec![game.id.clone()]);
        let eight = opened(&next)[0].clone();
        assert_eq!((eight.number, eight.club), (2, Some(Club::Iron8)));
        for _ in 0..6 {
            assert_eq!(hit(&mut p), Some(eight.id.clone()));
        }
        let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron9)), 4);
        assert_eq!(closed(&next), vec![eight.id.clone()]);
        let nine = opened(&next)[0].clone();
        assert_eq!(nine.number, 3);
        assert_eq!(hit(&mut p), Some(nine.id));
        assert_eq!(p.open_blocks().len(), 1);
        one_per_golfer(&p);
    }

    /// The golfer a game id is linked to is removed while the game id is
    /// up: its Set goes on as the game id's own, and a club change opens
    /// Set 2, never a second Set 1.
    #[test]
    fn removing_a_linked_golfer_keeps_one_open_set_for_the_game_id() {
        let config = linked();
        let (mut p, _) = Practice::with_config(
            config.clone(),
            turn(Some("pureplay.local-3"), Some(Club::Iron7)),
            0,
        );
        let one = hit(&mut p).expect("block");
        config.write().golfers.retain(|g| g.name != "Eric");
        assert!(p.rekey(1).is_empty(), "nothing on the wire changes");
        let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron8)), 2);
        assert_eq!(closed(&next), vec![one.clone()]);
        let two = opened(&next)[0].clone();
        assert_eq!((two.number, two.club), (2, Some(Club::Iron8)));
        assert_eq!(hit(&mut p), Some(two.id.clone()));
        assert_eq!(p.open_blocks().len(), 1);
        one_per_golfer(&p);
    }

    /// A game id moved from Eric to Sam while Sam is up: the game id's Set
    /// closes, Sam's stays open, and the game id coming up continues Sam's
    /// run.
    #[test]
    fn moving_a_game_id_to_another_golfer_merges_into_their_run() {
        let config = linked();
        let (mut p, _) = Practice::with_config(
            config.clone(),
            turn(Some("pureplay.local-3"), Some(Club::Iron7)),
            0,
        );
        let game = hit(&mut p).expect("block");
        let sam = opened(&p.turn(turn(Some("flighthook.s"), Some(Club::Iron7)), 1))[0].clone();
        hit(&mut p);
        {
            let mut c = config.write();
            c.golfers[0].identifiers.clear();
            c.golfers[1].identifiers.push("pureplay.local-3".into());
        }
        assert_eq!(p.rekey(2), vec![BlockChange::Closed(game)]);
        let up = p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 3);
        let b = &opened(&up)[0];
        assert_eq!((b.id.as_str(), b.number), (sam.id.as_str(), 1));
        assert_eq!(hit(&mut p), Some(sam.id.clone()));
        let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Driver)), 4);
        assert_eq!(opened(&next)[0].number, 2);
        // Eric gets a block of his own again when he comes up.
        let eric = p.turn(turn(Some("flighthook.e"), Some(Club::Iron7)), 5);
        assert_eq!(opened(&eric)[0].number, 2, "Eric's own count goes on");
        one_per_golfer(&p);
    }

    /// An empty duplicate is dropped and gives its number back; the Set
    /// with shots is kept and republished under the turn's raw id.
    #[test]
    fn an_empty_duplicate_is_dropped_without_a_gap() {
        let config = unlinked();
        let (mut p, _) = Practice::with_config(
            config.clone(),
            turn(Some("flighthook.e"), Some(Club::Iron7)),
            0,
        );
        let eric = hit(&mut p).expect("block");
        let game = opened(&p.turn(turn(Some("pureplay.local-3"), Some(Club::Iron7)), 1))[0].clone();
        config.write().golfers[0]
            .identifiers
            .push("pureplay.local-3".into());
        let saved = p.rekey(2);
        assert_eq!(saved.first(), Some(&BlockChange::Closed(game.id.clone())));
        let b = &opened(&saved)[0];
        assert_eq!(
            (
                b.id.as_str(),
                b.number,
                b.golfer.as_ref().map(GolferId::as_str)
            ),
            (eric.as_str(), 1, Some("pureplay.local-3"))
        );
        assert_eq!(hit(&mut p), Some(eric.clone()));
        let next = p.turn(turn(Some("pureplay.local-3"), Some(Club::Driver)), 3);
        assert_eq!(opened(&next)[0].number, 2, "no gap");
        // A save that changes nothing changes nothing.
        assert!(p.rekey(4).is_empty());
    }

    #[test]
    fn a_trigger_counts_for_the_golfer_up() {
        let (mut p, start) = Practice::start(turn(None, Some(Club::Iron7)), 0);
        assert_eq!(
            p.current().map(|b| b.id.clone()),
            Some(opened(&start)[0].id.clone())
        );
        assert_eq!(hit(&mut p), Some(opened(&start)[0].id.clone()));
    }
}
