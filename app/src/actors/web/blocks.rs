//! The collector's practice blocks: every `practice.block` flighthook's
//! practice actor published this run, open or closed, with the stored shots
//! that belong to it.
//!
//! [`BlockStore`] is pure bookkeeping with no I/O. The web layer feeds it the
//! practice actor's `practice.block` messages and their removals in bus
//! order, counts a stored shot for the block the SDK aggregator stamped on it
//! at its trigger, and releases it again when the shot leaves the history.
//! Clear history forgets everything.

use flightrelay::{FrpBody, PracticeBlock, StateKind};

/// A block and what the collector knows about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRecord {
    pub block: PracticeBlock,
    /// Still open: not removed by the practice actor.
    pub open: bool,
    /// Stored shots with data that belong to it.
    pub shots: u32,
    /// When its first stored shot was triggered (ms since the Unix epoch).
    pub first_shot_at: Option<u64>,
}

/// Every block seen this run, in the order first seen.
#[derive(Debug, Default)]
pub struct BlockStore {
    blocks: Vec<BlockRecord>,
}

impl BlockStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a message from the practice actor: a `practice.block` (new or
    /// republished in place) or its removal. Returns the changed record.
    pub fn apply(&mut self, body: &FrpBody) -> Option<BlockRecord> {
        match body {
            FrpBody::PracticeBlock(block) => {
                if let Some(r) = self.get_mut(&block.id) {
                    r.block = block.clone();
                    r.open = true;
                    return Some(r.clone());
                }
                let record = BlockRecord {
                    block: block.clone(),
                    open: true,
                    shots: 0,
                    first_shot_at: None,
                };
                self.blocks.push(record.clone());
                Some(record)
            }
            FrpBody::Removed(removal) if removal.kind == StateKind::PracticeBlock => {
                let r = self.get_mut(removal.id.as_deref()?)?;
                r.open = false;
                Some(r.clone())
            }
            _ => None,
        }
    }

    /// A stored shot with data, triggered at `at_ms`, joined block `id`.
    pub fn count(&mut self, id: &str, at_ms: u64) -> Option<BlockRecord> {
        let r = self.get_mut(id)?;
        r.shots += 1;
        r.first_shot_at = Some(r.first_shot_at.map_or(at_ms, |t| t.min(at_ms)));
        Some(r.clone())
    }

    /// A shot of block `id` left the history.
    pub fn release(&mut self, id: &str) -> Option<BlockRecord> {
        let r = self.get_mut(id)?;
        r.shots = r.shots.saturating_sub(1);
        Some(r.clone())
    }

    /// Forget every block (Clear history).
    pub fn clear(&mut self) {
        self.blocks.clear();
    }

    /// Every block seen, in the order first seen.
    #[must_use]
    pub fn all(&self) -> &[BlockRecord] {
        &self.blocks
    }

    /// The session of the newest block (1 before any).
    #[must_use]
    pub fn session(&self) -> u32 {
        self.blocks.last().map_or(1, |r| r.block.session)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut BlockRecord> {
        self.blocks.iter_mut().find(|r| r.block.id == id)
    }
}

#[cfg(test)]
mod tests {
    use flightrelay::{Club, Removal};

    use super::*;

    fn block(id: &str, number: u32, club: Club) -> FrpBody {
        FrpBody::PracticeBlock(PracticeBlock {
            id: id.into(),
            golfer: None,
            club: Some(club),
            number,
            session: 1,
            started_at: 0,
        })
    }

    #[test]
    fn keeps_every_block_seen_with_its_shot_count_after_removal() {
        let mut store = BlockStore::new();
        store.apply(&block("a", 1, Club::Iron7));
        assert_eq!(store.count("a", 0).map(|r| r.shots), Some(1));
        let closed = store
            .apply(&Removal::of_id(StateKind::PracticeBlock, "a").into())
            .unwrap();
        assert!(!closed.open);
        assert_eq!(closed.shots, 1);
        store.apply(&block("b", 2, Club::Driver));
        assert_eq!(store.all().len(), 2);
        assert_eq!(store.release("a").map(|r| r.shots), Some(0));
        assert_eq!(store.all().len(), 2, "a closed block stays as history");
    }

    #[test]
    fn a_republished_block_updates_in_place() {
        let mut store = BlockStore::new();
        store.apply(&block("a", 1, Club::Iron7));
        let r = store.apply(&block("a", 1, Club::Driver)).unwrap();
        assert_eq!(r.block.club, Some(Club::Driver));
        assert_eq!(store.all().len(), 1);
    }

    #[test]
    fn clear_forgets_everything_and_unknown_ids_are_ignored() {
        let mut store = BlockStore::new();
        store.apply(&block("a", 1, Club::Iron7));
        store.clear();
        assert!(store.all().is_empty());
        assert!(store.count("a", 0).is_none());
        assert!(
            store
                .apply(&Removal::of_id(StateKind::PracticeBlock, "zz").into())
                .is_none()
        );
        assert_eq!(store.session(), 1);
    }
}
