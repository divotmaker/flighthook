//! Shot groups ("sets"): which set each stored shot belongs to.
//!
//! [`GroupState`] is pure bookkeeping with no I/O. The web updater drives it
//! from bus events in bus order and emits every record it returns as a
//! `shot_group` event.
//!
//! - A shot is **assigned** to a set when it triggers (`assign`) and
//!   **joins** it when it finishes (`finish`); only shots with data count.
//! - At most one set is **open**. A shot joins it unless the club in play
//!   differs from its club, in which case the open set closes and a new one
//!   starts. "New set" (`new_group`) does the same for the next shot.
//!   Returning to an earlier club starts a new set. A closed set never
//!   reopens.
//! - "New session" (`new_session`) closes the open set and counts the
//!   session up; the next shot starts a new set in the new session.
//! - "Clear history" (`clear`) drops every set and counts the session up;
//!   set numbers start again at 1.
//! - The default view (`shot_grouping`) has no effect here.
//! - A record is kept while it is open, has counted shots, or has shots
//!   assigned and not yet finished. When it drops out, the last record
//!   returned for it has `open: false, shots: 0`.

use flighthook::{Club, GroupKey, ShotGroupInfo};

/// A set record plus the shots assigned to it that have not finished yet.
#[derive(Debug, Clone)]
struct Group {
    info: ShotGroupInfo,
    pending: u32,
}

impl Group {
    fn is_empty(&self) -> bool {
        self.info.shots == 0 && self.pending == 0
    }

    fn retained(&self) -> bool {
        self.info.open || !self.is_empty()
    }
}

/// Shot sets for one webserver's history.
#[derive(Debug, Default)]
pub struct GroupState {
    /// Ascending `number` (creation order).
    groups: Vec<Group>,
    /// Id of the open set, if any.
    open: Option<String>,
    /// Club last seen on the bus (`golfer` is always `None`).
    selected: GroupKey,
    /// Ordinal of the last set created (0 before the first).
    last_number: u32,
    /// Current session: 0 at start, counted up by every "New session".
    session: u32,
}

impl GroupState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current session.
    #[must_use]
    pub fn session(&self) -> u32 {
        self.session
    }

    /// A club was selected (a `club_info` on the bus).
    pub fn select(&mut self, club: Option<Club>) {
        self.selected = GroupKey { golfer: None, club };
    }

    /// The club last seen on the bus: the club in play for a shot that
    /// triggers now.
    #[must_use]
    pub fn selected_club(&self) -> Option<Club> {
        self.selected.club
    }

    /// Set for a shot triggered with `key`, creating one if needed; the shot
    /// is counted as pending in it. Returns the set id and the records that
    /// changed (closed, created, or re-keyed by adoption).
    pub fn assign(&mut self, key: GroupKey, now: &str) -> (String, Vec<ShotGroupInfo>) {
        let mut changes = Vec::new();
        let open = self
            .open
            .clone()
            .and_then(|id| self.group(&id).map(|g| (id, g.info.key(), g.is_empty())));
        let id = match open {
            Some((id, open_key, _)) if open_key == key => id,
            Some((id, _, true)) => {
                // An empty set takes the first shot's club, so "New set,
                // change club, hit" is one set.
                if let Some(g) = self.group_mut(&id) {
                    g.info.golfer.clone_from(&key.golfer);
                    g.info.club = key.club;
                    changes.push(g.info.clone());
                }
                id
            }
            Some((id, _, false)) => {
                changes.extend(self.close(&id));
                self.create_into(key, false, now, &mut changes)
            }
            None => self.create_into(key, false, now, &mut changes),
        };
        if let Some(g) = self.group_mut(&id) {
            g.pending += 1;
        }
        (id, changes)
    }

    /// A shot assigned to `id` finished; `counted` when it had data. Returns
    /// the updated record when it was counted, or the dropped record when
    /// the uncounted shot was all that kept it.
    pub fn finish(&mut self, id: &str, counted: bool) -> Option<ShotGroupInfo> {
        let g = self.group_mut(id)?;
        g.pending = g.pending.saturating_sub(1);
        if counted {
            g.info.shots += 1;
            return Some(g.info.clone());
        }
        self.drop_if_unretained(id)
    }

    /// "New set" for the selected club. Returns the records that changed
    /// (closed, created); empty when the open set is already empty.
    pub fn new_group(&mut self, now: &str) -> Vec<ShotGroupInfo> {
        let mut changes = Vec::new();
        if let Some(id) = self.open.clone() {
            if self.group(&id).is_some_and(Group::is_empty) {
                return changes;
            }
            changes.extend(self.close(&id));
        }
        self.create_into(self.selected.clone(), true, now, &mut changes);
        changes
    }

    /// "New session": closes the open set and counts the session up.
    /// Returns the closed record, if any.
    pub fn new_session(&mut self) -> Vec<ShotGroupInfo> {
        self.session += 1;
        match self.open.clone() {
            Some(id) => self.close(&id).into_iter().collect(),
            None => Vec::new(),
        }
    }

    /// "Clear history": drops every set, open or closed, and counts the
    /// session up. The next set created is number 1; the club in play is
    /// kept. Shots assigned before the clear are forgotten: finishing one
    /// changes nothing.
    pub fn clear(&mut self) {
        self.groups.clear();
        self.open = None;
        self.last_number = 0;
        self.session += 1;
    }

    /// A counted shot of set `id` left the history. Returns the updated
    /// (or dropped) record.
    pub fn release(&mut self, id: &str) -> Option<ShotGroupInfo> {
        let g = self.group_mut(id)?;
        g.info.shots = g.info.shots.saturating_sub(1);
        let info = g.info.clone();
        self.drop_if_unretained(id).or(Some(info))
    }

    /// The open set: the one the next shot joins unless the club in play
    /// differs from its club.
    #[must_use]
    pub fn current(&self) -> Option<&ShotGroupInfo> {
        let id = self.open.as_deref()?;
        self.group(id).map(|g| &g.info)
    }

    #[cfg(test)]
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ShotGroupInfo> {
        self.group(id).map(|g| &g.info)
    }

    /// Every kept record, ascending `number`.
    #[must_use]
    pub fn list(&self) -> Vec<ShotGroupInfo> {
        self.groups.iter().map(|g| g.info.clone()).collect()
    }

    fn group(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.info.id == id)
    }

    fn group_mut(&mut self, id: &str) -> Option<&mut Group> {
        self.groups.iter_mut().find(|g| g.info.id == id)
    }

    /// Create the open set for `key` in the current session, push its record
    /// onto `changes` and return its id.
    fn create_into(
        &mut self,
        key: GroupKey,
        manual: bool,
        now: &str,
        changes: &mut Vec<ShotGroupInfo>,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.last_number += 1;
        let info = ShotGroupInfo {
            id: id.clone(),
            number: self.last_number,
            session: self.session,
            golfer: key.golfer,
            club: key.club,
            started_at: now.to_string(),
            manual,
            open: true,
            shots: 0,
        };
        self.open = Some(id.clone());
        changes.push(info.clone());
        self.groups.push(Group { info, pending: 0 });
        id
    }

    /// Close set `id`; returns its final record (dropped when empty).
    fn close(&mut self, id: &str) -> Option<ShotGroupInfo> {
        if self.open.as_deref() == Some(id) {
            self.open = None;
        }
        let g = self.group_mut(id)?;
        g.info.open = false;
        let info = g.info.clone();
        self.drop_if_unretained(id).or(Some(info))
    }

    /// Remove `id` when nothing keeps it; returns the dropped record.
    fn drop_if_unretained(&mut self, id: &str) -> Option<ShotGroupInfo> {
        let pos = self.groups.iter().position(|g| g.info.id == id)?;
        if self.groups[pos].retained() {
            return None;
        }
        let mut info = self.groups.remove(pos).info;
        info.open = false;
        info.shots = 0;
        Some(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-10-26T13:58:02.117Z";

    fn key(club: Option<Club>) -> GroupKey {
        GroupKey { golfer: None, club }
    }

    const I7: Option<Club> = Some(Club::Iron7);
    const DR: Option<Club> = Some(Club::Driver);

    /// Trigger and finish one shot with data; returns its set id.
    fn shot(g: &mut GroupState, club: Option<Club>) -> String {
        let (id, _) = g.assign(key(club), NOW);
        g.finish(&id, true).expect("record");
        id
    }

    fn shots(g: &GroupState, id: &str) -> u32 {
        g.get(id).expect("kept").shots
    }

    fn is_open(g: &GroupState, id: &str) -> bool {
        g.get(id).expect("kept").open
    }

    fn open_count(g: &GroupState) -> usize {
        g.list().iter().filter(|s| s.open).count()
    }

    #[test]
    fn first_shot_creates_set_one_and_same_club_joins() {
        let mut g = GroupState::new();
        let (id, changes) = g.assign(key(I7), NOW);
        assert_eq!(changes.len(), 1);
        let created = &changes[0];
        assert_eq!(created.id, id);
        assert_eq!(created.number, 1);
        assert_eq!(created.session, 0);
        assert_eq!(created.club, I7);
        assert_eq!(created.started_at, NOW);
        assert!(created.open && !created.manual);
        assert_eq!(created.shots, 0);
        let joined = g.finish(&id, true).expect("record");
        assert_eq!(joined.shots, 1);

        let (again, changes) = g.assign(key(I7), NOW);
        assert_eq!(again, id);
        assert!(changes.is_empty());
        g.finish(&again, true);
        assert_eq!(shots(&g, &id), 2);
    }

    #[test]
    fn no_club_shots_share_a_set() {
        let mut g = GroupState::new();
        let a = shot(&mut g, None);
        let b = shot(&mut g, None);
        assert_eq!(a, b);
        assert_eq!(g.get(&a).expect("kept").club, None);
        assert_eq!(shots(&g, &a), 2);
    }

    #[test]
    fn club_change_closes_and_creates() {
        let mut g = GroupState::new();
        let a = shot(&mut g, I7);
        let (b, changes) = g.assign(key(DR), NOW);
        assert_ne!(a, b);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].id, a);
        assert!(!changes[0].open);
        assert_eq!(changes[0].shots, 1);
        assert_eq!(changes[1].id, b);
        assert_eq!(changes[1].number, 2);
        assert!(!is_open(&g, &a));
        assert!(is_open(&g, &b));
        assert_eq!(open_count(&g), 1);
    }

    #[test]
    fn returning_to_an_earlier_club_starts_a_new_set() {
        let mut g = GroupState::new();
        let a = shot(&mut g, I7);
        let d = shot(&mut g, DR);
        let (again, changes) = g.assign(key(I7), NOW);
        assert_ne!(again, a, "the earlier 7-iron set stays closed");
        assert_eq!(changes.len(), 2, "DR closes, a new 7I set starts");
        assert_eq!(changes[0].id, d);
        assert!(!changes[0].open);
        assert_eq!(changes[1].id, again);
        assert_eq!(changes[1].number, 3);
        g.finish(&again, true);
        assert_eq!(shots(&g, &a), 1);
        assert_eq!(shots(&g, &again), 1);
        assert!(!is_open(&g, &a) && !is_open(&g, &d));
        assert_eq!(g.list().len(), 3);
        assert_eq!(open_count(&g), 1);
    }

    #[test]
    fn new_set_then_same_club_joins_the_manual_set() {
        let mut g = GroupState::new();
        g.select(I7);
        let a = shot(&mut g, I7);
        let changes = g.new_group(NOW);
        assert_eq!(changes.len(), 2);
        assert!(!changes[0].open && changes[0].id == a);
        let m = changes[1].id.clone();
        assert!(changes[1].manual && changes[1].open);
        assert_eq!(changes[1].club, I7);
        assert_eq!(shot(&mut g, I7), m);
    }

    #[test]
    fn empty_manual_set_adopts_a_different_club() {
        let mut g = GroupState::new();
        g.select(I7);
        shot(&mut g, I7);
        let m = g.new_group(NOW)[1].id.clone();
        let (id, changes) = g.assign(key(DR), NOW);
        assert_eq!(id, m, "one set, not two");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].club, DR);
        g.finish(&id, true);
        assert_eq!(g.list().len(), 2);
        assert_eq!(g.current().expect("current").club, DR);
    }

    #[test]
    fn new_set_on_an_empty_current_set_is_a_no_op() {
        let mut g = GroupState::new();
        g.select(I7);
        shot(&mut g, I7);
        assert_eq!(g.new_group(NOW).len(), 2);
        assert!(g.new_group(NOW).is_empty());
        assert_eq!(g.list().len(), 2);
    }

    #[test]
    fn new_set_with_nothing_open_creates_one() {
        let mut g = GroupState::new();
        g.select(DR);
        let changes = g.new_group(NOW);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].manual && changes[0].open);
        assert_eq!(changes[0].club, DR);
        assert_eq!(g.current().expect("current").id, changes[0].id);
    }

    #[test]
    fn new_session_closes_the_open_set_and_counts_up() {
        let mut g = GroupState::new();
        assert_eq!(g.session(), 0);
        let a = shot(&mut g, I7);
        let closed = g.new_session();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].id, a);
        assert!(!closed[0].open);
        assert_eq!(closed[0].session, 0);
        assert_eq!(g.session(), 1);
        assert!(g.current().is_none());
        // The next shot starts a new set in the new session, same club or not.
        let b = shot(&mut g, I7);
        assert_ne!(a, b);
        assert_eq!(g.get(&b).expect("kept").session, 1);
        assert_eq!(g.get(&a).expect("kept").session, 0);
        // Nothing open: nothing closes, but the session still counts up.
        g.new_session();
        assert!(g.new_session().is_empty());
        assert_eq!(g.session(), 3);
        let c = shot(&mut g, DR);
        assert_eq!(g.get(&c).expect("kept").session, 3);
        // New set stays in the session.
        g.select(DR);
        let m = g.new_group(NOW)[1].clone();
        assert_eq!(m.session, 3);
    }

    #[test]
    fn clear_drops_every_set_and_counts_the_session_up() {
        let mut g = GroupState::new();
        g.select(I7);
        let a = shot(&mut g, I7);
        let b = shot(&mut g, DR);
        // A shot still in flight in the open set.
        let (in_flight, _) = g.assign(key(DR), NOW);
        assert_eq!(in_flight, b);
        g.clear();
        assert!(g.list().is_empty());
        assert!(g.current().is_none());
        assert_eq!(g.session(), 1);
        assert_eq!(g.selected_club(), I7, "the club in play is kept");
        // The shot in flight finishing changes nothing.
        assert_eq!(g.finish(&in_flight, true), None);
        assert_eq!(g.release(&a), None);
        assert!(g.list().is_empty());
        // The next set is number 1, in the new session.
        let c = shot(&mut g, I7);
        let info = g.get(&c).expect("kept");
        assert_ne!(c, a);
        assert_eq!((info.number, info.session, info.shots), (1, 1, 1));
    }

    #[test]
    fn uncounted_finish_keeps_the_set_empty() {
        let mut g = GroupState::new();
        let (id, _) = g.assign(key(I7), NOW);
        assert!(g.finish(&id, false).is_none(), "open set stays, no change");
        assert_eq!(shots(&g, &id), 0);
        assert!(is_open(&g, &id));
        // An empty set with nothing pending is "empty" for New set.
        g.select(I7);
        assert!(g.new_group(NOW).is_empty());
    }

    #[test]
    fn a_pending_shot_makes_the_set_non_empty() {
        let mut g = GroupState::new();
        g.select(I7);
        let (a, _) = g.assign(key(I7), NOW);
        let changes = g.new_group(NOW);
        assert_eq!(changes.len(), 2, "pending counts: close and create");
        assert_eq!(changes[0].id, a);
        assert!(!changes[0].open);
        assert_eq!(changes[0].shots, 0, "kept while pending");
        assert!(g.get(&a).is_some());
    }

    #[test]
    fn finish_counts_into_a_set_closed_meanwhile() {
        let mut g = GroupState::new();
        g.select(I7);
        let (a, _) = g.assign(key(I7), NOW);
        let m = g.new_group(NOW)[1].id.clone();
        let joined = g.finish(&a, true).expect("record");
        assert_eq!(joined.id, a);
        assert_eq!(joined.shots, 1);
        assert!(!joined.open);
        assert_eq!(shots(&g, &m), 0, "the new set is still empty");
        assert_eq!(g.current().expect("current").id, m);
    }

    #[test]
    fn uncounted_finish_of_a_closed_set_drops_it() {
        let mut g = GroupState::new();
        let (a, _) = g.assign(key(I7), NOW);
        g.new_session();
        assert!(g.get(&a).is_some(), "kept while pending");
        let dropped = g.finish(&a, false).expect("dropped record");
        assert_eq!(dropped.id, a);
        assert!(!dropped.open);
        assert_eq!(dropped.shots, 0);
        assert!(g.get(&a).is_none());
    }

    #[test]
    fn release_drops_a_closed_set_at_zero_and_keeps_an_open_one() {
        let mut g = GroupState::new();
        let a = shot(&mut g, I7);
        let b = shot(&mut g, DR);
        assert!(!is_open(&g, &a));

        let dropped = g.release(&a).expect("change");
        assert_eq!(dropped.id, a);
        assert!(!dropped.open);
        assert_eq!(dropped.shots, 0);
        assert!(g.get(&a).is_none());

        let kept = g.release(&b).expect("change");
        assert!(kept.open);
        assert_eq!(kept.shots, 0);
        assert!(g.get(&b).is_some());
        assert!(g.release("no-such-group").is_none());
    }

    #[test]
    fn release_counts_down() {
        let mut g = GroupState::new();
        let a = shot(&mut g, I7);
        shot(&mut g, I7);
        let change = g.release(&a).expect("change");
        assert_eq!(change.shots, 1);
        assert!(change.open);
    }

    #[test]
    fn group_ids_are_unique_v4_uuids_and_numbers_ascend() {
        let mut g = GroupState::new();
        let clubs = [I7, DR, I7, DR, None, I7];
        let ids: Vec<String> = clubs.iter().map(|c| shot(&mut g, *c)).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len());
        for id in &ids {
            let parsed = uuid::Uuid::parse_str(id).expect("uuid");
            assert_eq!(parsed.get_version_num(), 4);
        }
        let numbers: Vec<u32> = g.list().iter().map(|i| i.number).collect();
        assert_eq!(numbers, vec![1, 2, 3, 4, 5, 6]);

        // Two states (two webservers, or two runs) never share an id.
        let mut other = GroupState::new();
        let o = shot(&mut other, I7);
        assert!(!ids.contains(&o));
        assert_eq!(other.get(&o).expect("kept").number, 1);
    }
}
