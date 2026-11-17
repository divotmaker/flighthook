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
mod tests;
