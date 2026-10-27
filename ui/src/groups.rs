//! Shot sets as the UI knows them: the `/api/shots/groups` list of the
//! webserver the UI talks to, kept current by its `shot_group` events, plus
//! the session they are counted in.

use crate::types::{Club, ShotGroupInfo, ShotGrouping, ShotGroupsResponse, ShotKey, ShotRow};

/// Full name of `club`, as group headers show it ("7 Iron").
pub(crate) fn club_name(club: Club) -> &'static str {
    match club {
        Club::Driver => "Driver",
        Club::MiniDriver => "Mini Driver",
        Club::Wood2 => "2 Wood",
        Club::Wood3 => "3 Wood",
        Club::Wood4 => "4 Wood",
        Club::Wood5 => "5 Wood",
        Club::Wood6 => "6 Wood",
        Club::Wood7 => "7 Wood",
        Club::Wood8 => "8 Wood",
        Club::Wood9 => "9 Wood",
        Club::Hybrid1 => "1 Hybrid",
        Club::Hybrid2 => "2 Hybrid",
        Club::Hybrid3 => "3 Hybrid",
        Club::Hybrid4 => "4 Hybrid",
        Club::Hybrid5 => "5 Hybrid",
        Club::Hybrid6 => "6 Hybrid",
        Club::Hybrid7 => "7 Hybrid",
        Club::Hybrid8 => "8 Hybrid",
        Club::Hybrid9 => "9 Hybrid",
        Club::Iron1 => "1 Iron",
        Club::Iron2 => "2 Iron",
        Club::Iron3 => "3 Iron",
        Club::Iron4 => "4 Iron",
        Club::Iron5 => "5 Iron",
        Club::Iron6 => "6 Iron",
        Club::Iron7 => "7 Iron",
        Club::Iron8 => "8 Iron",
        Club::Iron9 => "9 Iron",
        Club::PitchingWedge => "Pitching Wedge",
        Club::GapWedge => "Gap Wedge",
        Club::SandWedge => "Sand Wedge",
        Club::LobWedge => "Lob Wedge",
        Club::Putter => "Putter",
    }
}

/// Every view with its label, in selector order.
pub(crate) const GROUPING_MODES: [(ShotGrouping, &str); 3] = [
    (ShotGrouping::BySet, "By set"),
    (ShotGrouping::ByClub, "By club"),
    (ShotGrouping::None, "None"),
];

/// Hover text for the view selector.
pub(crate) const GROUPING_HINT: &str = "Sets are recorded the same way in every view: \
     a club change or + New set starts one. \
     By set: a section per set. \
     By club: a section per club, combining its sets since the last New session. \
     None: the last shot only.";

/// What the Cards shot list offers at its foot, below the list.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Strip {
    /// New set and New session are shown (every view but `none`).
    pub(crate) actions: bool,
    /// New set is enabled: the open set has shots, or there is none.
    pub(crate) new_group: bool,
    /// New session is enabled: a set is open, so the session has shots.
    pub(crate) new_session: bool,
    /// One line on where the next shot goes, when that is not the obvious
    /// place.
    pub(crate) hint: Option<String>,
}

/// The shot sets of one webserver.
#[derive(Default, Debug)]
pub(crate) struct ShotGroups {
    /// Actor id of the webserver the sets come from; `None` until
    /// `/api/shots/groups` answers. `shot_group` events from any other
    /// actor are ignored.
    pub(crate) server: Option<String>,
    /// The view shown: the server's default view (`shot_grouping`), or the
    /// one picked since.
    pub(crate) mode: ShotGrouping,
    /// Ascending `number`.
    list: Vec<ShotGroupInfo>,
    /// Current session: the server's, counted up by every `new_shot_session`
    /// and `clear_shot_history` seen since, and never below a listed set's.
    session: u32,
    /// The club in play when last observed; `None` before the first
    /// observation.
    observed: Option<Option<Club>>,
    /// Where in the shot history each club was last selected.
    selections: Vec<Selection>,
    /// Count of observed selections, ordering selections at one place.
    seq: u64,
    /// Bumped by every `replace`, every applied `shot_group` event, every
    /// new session and every clear, so caches of what the sets cover know
    /// when to rebuild.
    rev: u64,
}

/// A club selection, placed in the shot history: after the row `after`,
/// the newest row with data when the club was selected (`None` when there
/// was none).
#[derive(Debug)]
struct Selection {
    club: Option<Club>,
    after: Option<(String, ShotKey)>,
    seq: u64,
}

/// A place in the shot history, for ordering activity newest last: a row
/// with data at index `i` is `(2i + 2, 0)`; a selection after it is
/// `(2i + 3, seq)`, and one made before any row is `(1, seq)`.
pub(crate) type Activity = (usize, u64);

/// What a `shot_group` event did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GroupEvent {
    /// From another webserver, or before the REST list arrived: ignored.
    Foreign,
    /// Applied.
    Applied,
    /// Applied, but the UI's rows look out of step with the server (the
    /// joined shot's row is missing, or a newer row with data never got its
    /// group): refetch groups and shots.
    OutOfStep,
}

impl ShotGroups {
    /// Replace everything with a `/api/shots/groups` reply.
    pub(crate) fn replace(&mut self, resp: ShotGroupsResponse) {
        self.server = Some(resp.server);
        self.mode = resp.mode;
        self.list = resp.groups;
        self.list.sort_by_key(|g| g.number);
        self.session = self
            .list
            .iter()
            .map(|g| g.session)
            .fold(resp.session, u32::max);
        self.rev += 1;
    }

    /// Revision of the set list: changes with every `replace`, every
    /// applied `shot_group` event, every new session and every clear.
    pub(crate) fn rev(&self) -> u64 {
        self.rev
    }

    /// The current session.
    pub(crate) fn session(&self) -> u32 {
        self.session
    }

    /// A `new_shot_session` command went over the bus: the server counts
    /// its session up, and so does the UI.
    pub(crate) fn new_session(&mut self) {
        self.session += 1;
        self.rev += 1;
    }

    /// A `clear_shot_history` command went over the bus: the server drops
    /// every set and counts its session up, and so does the UI. The club in
    /// play is observed afresh.
    pub(crate) fn clear(&mut self) {
        self.list.clear();
        self.session += 1;
        self.observed = None;
        self.selections.clear();
        self.rev += 1;
    }

    /// Whether no group is listed.
    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The group with id `id`.
    pub(crate) fn get(&self, id: &str) -> Option<&ShotGroupInfo> {
        self.list.iter().find(|g| g.id == id)
    }

    /// The open set (the server keeps at most one open).
    pub(crate) fn open_set(&self) -> Option<&ShotGroupInfo> {
        self.list.iter().filter(|g| g.open).max_by_key(|g| g.number)
    }

    /// The set the next shot joins when `club` is in play: the open set,
    /// when it has no shots yet (it takes the first shot's club) or its club
    /// is `club`. `None` when the next shot starts a new set.
    pub(crate) fn current_group(&self, club: Option<Club>) -> Option<&ShotGroupInfo> {
        self.open_set().filter(|g| g.shots == 0 || g.club == club)
    }

    /// The foot's buttons and hint when `club` is in play. The view `none`
    /// shows neither.
    pub(crate) fn strip(&self, club: Option<Club>) -> Strip {
        if self.mode == ShotGrouping::None {
            return Strip {
                actions: false,
                new_group: false,
                new_session: false,
                hint: None,
            };
        }
        let hint = match club {
            None => Some("No club selected".to_owned()),
            Some(c) if self.mode == ShotGrouping::BySet && self.current_group(club).is_none() => {
                Some(format!("Next shot starts a new set ({c})"))
            }
            Some(_) => None,
        };
        let open = self.open_set();
        Strip {
            actions: true,
            new_group: open.is_none_or(|g| g.shots > 0),
            new_session: open.is_some(),
            hint,
        }
    }

    /// Note the club in play; a change is a selection of `club`, placed
    /// after the newest row with data in `shots`.
    pub(crate) fn observe_club(&mut self, club: Option<Club>, shots: &[ShotRow]) {
        if self.observed == Some(club) {
            return;
        }
        self.observed = Some(club);
        self.seq += 1;
        let after = shots
            .iter()
            .rfind(|r| r.ball.is_some() || r.club.is_some())
            .map(|r| {
                let key = ShotKey {
                    shot_id: r.shot_id.clone(),
                    shot_number: r.shot_number,
                };
                (r.actor.clone(), key)
            });
        let selection = Selection {
            club,
            after,
            seq: self.seq,
        };
        match self.selections.iter_mut().find(|s| s.club == club) {
            Some(s) => *s = selection,
            None => self.selections.push(selection),
        }
    }

    /// Where `club` was last selected in `shots`. A selection whose row has
    /// left `shots` is older than every row.
    pub(crate) fn selected_at(&self, club: Option<Club>, shots: &[ShotRow]) -> Option<Activity> {
        let s = self.selections.iter().find(|s| s.club == club)?;
        let at = s
            .after
            .as_ref()
            .and_then(|(actor, key)| find_row(shots, actor, key))
            .map_or(1, |i| 2 * i + 3);
        Some((at, s.seq))
    }

    /// Every set, ascending `number`.
    pub(crate) fn iter_sets(&self) -> impl Iterator<Item = &ShotGroupInfo> {
        self.list.iter()
    }

    /// Store `group`, replacing the record with the same id. A closed group
    /// with no shots left is removed.
    fn upsert(&mut self, group: ShotGroupInfo) {
        self.rev += 1;
        self.session = self.session.max(group.session);
        let existing = self.list.iter().position(|g| g.id == group.id);
        if !group.open && group.shots == 0 {
            if let Some(i) = existing {
                self.list.remove(i);
            }
            return;
        }
        match existing {
            Some(i) => self.list[i] = group,
            None => {
                let at = self.list.partition_point(|g| g.number <= group.number);
                self.list.insert(at, group);
            }
        }
    }

    /// Apply a `shot_group` event sent by `actor`. When it names a shot
    /// that joined the group, that shot's row in `shots` is stamped with the
    /// group and its club.
    pub(crate) fn apply(
        &mut self,
        actor: &str,
        group: ShotGroupInfo,
        joined: Option<(&str, &ShotKey)>,
        shots: &mut [ShotRow],
    ) -> GroupEvent {
        if self.server.as_deref() != Some(actor) {
            return GroupEvent::Foreign;
        }
        let id = group.id.clone();
        let club = group.club;
        self.upsert(group);
        let Some((shot_actor, key)) = joined else {
            return GroupEvent::Applied;
        };
        match stamp(shots, shot_actor, key, &id, club) {
            Some(i) if !ungrouped_after_grouped(&shots[..i]) => GroupEvent::Applied,
            _ => GroupEvent::OutOfStep,
        }
    }
}

/// Index of the newest row for the shot `key` from `actor`, matched by
/// [`same_shot`].
pub(crate) fn find_row(shots: &[ShotRow], actor: &str, key: &ShotKey) -> Option<usize> {
    shots.iter().rposition(|r| {
        same_shot(
            (&r.actor, &r.shot_id, r.shot_number),
            (actor, &key.shot_id, key.shot_number),
        )
    })
}

/// Whether two `(actor, shot_id, shot_number)` triples name the same shot:
/// by `shot_id` (and the actor, when both sides name one) when both sides
/// have an id, else by actor and shot number.
pub(crate) fn same_shot(a: (&str, &str, u32), b: (&str, &str, u32)) -> bool {
    if a.1.is_empty() || b.1.is_empty() {
        a.0 == b.0 && a.2 == b.2
    } else {
        a.1 == b.1 && same_actor(a.0, b.0)
    }
}

/// Whether two actor ids can name the same device: equal, or one is unset.
pub(crate) fn same_actor(a: &str, b: &str) -> bool {
    a.is_empty() || b.is_empty() || a == b
}

/// Whether `shots` holds data from more than one device, by actor identity
/// (`same_actor`). A device's actor id does not change across a reconnect,
/// so it still counts once.
pub(crate) fn multiple_devices(shots: &[ShotRow]) -> bool {
    let mut seen: Vec<&str> = Vec::new();
    for s in shots {
        if seen.iter().any(|a| same_actor(a, &s.actor)) {
            continue;
        }
        seen.push(&s.actor);
        if seen.len() > 1 {
            return true;
        }
    }
    false
}

/// Stamp the row for the shot `key` from `actor` with `group` and `club`.
/// Returns the row's index.
fn stamp(
    shots: &mut [ShotRow],
    actor: &str,
    key: &ShotKey,
    group: &str,
    club: Option<Club>,
) -> Option<usize> {
    let i = find_row(shots, actor, key)?;
    let row = &mut shots[i];
    row.group = Some(group.to_owned());
    row.club_selected = club;
    Some(i)
}

/// Whether a row with data but no set follows a row in a set. Rows older
/// than every row in a set (from before the server's history) do not count.
fn ungrouped_after_grouped(shots: &[ShotRow]) -> bool {
    let Some(first) = shots.iter().position(|r| r.group.is_some()) else {
        return false;
    };
    shots[first..]
        .iter()
        .any(|r| r.group.is_none() && (r.ball.is_some() || r.club.is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BallFlight;

    fn info(id: &str, number: u32, open: bool, shots: u32) -> ShotGroupInfo {
        ShotGroupInfo {
            id: id.into(),
            number,
            session: 0,
            golfer: None,
            club: Some(Club::Iron7),
            started_at: "2026-10-26T15:41:28.300Z".into(),
            manual: false,
            open,
            shots,
        }
    }

    fn row(n: u32, id: &str) -> ShotRow {
        ShotRow {
            actor: "mevo.0".into(),
            shot_id: id.into(),
            shot_number: n,
            ball: Some(BallFlight::default()),
            club: None,
            derived: None,
            club_selected: None,
            group: None,
            finished: true,
        }
    }

    fn key(n: u32, id: &str) -> ShotKey {
        ShotKey {
            shot_id: id.into(),
            shot_number: n,
        }
    }

    fn served(groups: Vec<ShotGroupInfo>) -> ShotGroups {
        let mut g = ShotGroups::default();
        g.replace(ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::BySet,
            session: 0,
            current: None,
            groups,
        });
        g
    }

    fn numbers(g: &ShotGroups) -> Vec<u32> {
        g.iter_sets().map(|g| g.number).collect()
    }

    #[test]
    fn foreign_server_events_are_ignored() {
        let mut shots = vec![row(1, "s1")];
        // Before the REST list arrives, every event is foreign.
        let mut g = ShotGroups::default();
        let ev = g.apply("webserver.0", info("a", 1, true, 1), None, &mut shots);
        assert_eq!(ev, GroupEvent::Foreign);
        assert_eq!(g.iter_sets().count(), 0);

        let mut g = served(vec![]);
        let rev = g.rev();
        let joined = Some(("mevo.0", &key(1, "s1")));
        let ev = g.apply("webserver.1", info("b", 1, true, 1), joined, &mut shots);
        assert_eq!(ev, GroupEvent::Foreign);
        assert_eq!(g.iter_sets().count(), 0);
        assert_eq!(shots[0].group, None, "a foreign event stamps nothing");
        assert_eq!(g.rev(), rev, "a foreign event changes nothing");

        let ev = g.apply("webserver.0", info("a", 1, true, 1), joined, &mut shots);
        assert_eq!(ev, GroupEvent::Applied);
        assert_eq!(shots[0].group.as_deref(), Some("a"));
        assert!(g.rev() > rev);
        let rev = g.rev();
        g.replace(ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::None,
            session: 0,
            current: None,
            groups: vec![],
        });
        assert!(g.rev() > rev);
    }

    #[test]
    fn shot_group_upserts_by_id_in_number_order() {
        let mut g = served(vec![info("c", 3, true, 0), info("a", 1, false, 4)]);
        assert_eq!(numbers(&g), [1, 3]);
        let mut shots = vec![];
        // A new group lands in number order, whatever its id sorts as.
        g.apply("webserver.0", info("0-b", 2, true, 0), None, &mut shots);
        assert_eq!(numbers(&g), [1, 2, 3]);
        // Same id: replaced, not duplicated.
        g.apply("webserver.0", info("0-b", 2, false, 5), None, &mut shots);
        assert_eq!(numbers(&g), [1, 2, 3]);
        let b = g.get("0-b").unwrap();
        assert!(!b.open);
        assert_eq!(b.shots, 5);
    }

    #[test]
    fn closed_empty_group_is_deleted() {
        let mut g = served(vec![info("a", 1, false, 1), info("b", 2, true, 0)]);
        let mut shots = vec![];
        // An open empty group stays.
        g.apply("webserver.0", info("b", 2, true, 0), None, &mut shots);
        assert!(g.get("b").is_some());
        // Its last shot left the history and it is closed: gone.
        g.apply("webserver.0", info("a", 1, false, 0), None, &mut shots);
        assert!(g.get("a").is_none());
        // A drop for a group the UI never saw is a no-op.
        g.apply("webserver.0", info("z", 9, false, 0), None, &mut shots);
        assert_eq!(numbers(&g), [2]);
    }

    #[test]
    fn stamping_finds_rows_by_id_else_by_actor_and_number() {
        let mut g = served(vec![]);
        let mut shots = vec![row(7, "s7"), row(8, ""), row(8, "s8")];
        let mut grp = info("a", 1, true, 1);
        grp.club = Some(Club::Driver);
        // By id: the row with that id, though another row has the number.
        let ev = g.apply(
            "webserver.0",
            grp.clone(),
            Some(("mevo.0", &key(8, "s8"))),
            &mut shots,
        );
        assert_eq!(ev, GroupEvent::Applied);
        assert_eq!(shots[2].group.as_deref(), Some("a"));
        assert_eq!(shots[2].club_selected, Some(Club::Driver));
        assert_eq!(shots[1].group, None);
        // A row with no id: by actor and number.
        let mut shots = vec![row(9, "")];
        let ev = g.apply(
            "webserver.0",
            grp,
            Some(("mevo.0", &key(9, "s9"))),
            &mut shots,
        );
        assert_eq!(ev, GroupEvent::Applied);
        assert_eq!(shots[0].group.as_deref(), Some("a"));
    }

    #[test]
    fn find_row_matches_the_id_only_from_the_same_actor() {
        let mut omni = row(3, "3");
        omni.actor = "square.0".into();
        let shots = vec![row(3, "3"), omni];
        // Two devices counting ids alike: each finds its own row.
        assert_eq!(find_row(&shots, "mevo.0", &key(3, "3")), Some(0));
        assert_eq!(find_row(&shots, "square.0", &key(3, "3")), Some(1));
        assert_eq!(find_row(&shots, "r10.0", &key(3, "3")), None);
        // A row with no actor matches on the id alone.
        let mut anon = row(4, "s4");
        anon.actor.clear();
        assert_eq!(find_row(&[anon], "mevo.0", &key(4, "s4")), Some(0));
        // A key with no id matches by actor and number, as a pinned shot
        // does, whichever side lacks the id.
        assert_eq!(find_row(&shots, "square.0", &key(3, "")), Some(1));
        assert_eq!(find_row(&shots, "r10.0", &key(3, "")), None);
        let mut no_id = row(5, "");
        no_id.actor = "mevo.0".into();
        assert_eq!(find_row(&[no_id], "mevo.0", &key(5, "s5")), Some(0));
    }

    #[test]
    fn multiple_devices_counts_distinct_actors() {
        assert!(!multiple_devices(&[]), "zero shots");
        assert!(
            !multiple_devices(&[row(1, "s1"), row(2, "s2"), row(3, "s3")]),
            "one device"
        );
        // A reconnect keeps the same actor id, so it still counts once.
        assert!(
            !multiple_devices(&[row(1, "a1"), row(1, "b1")]),
            "one device across a reconnect"
        );
        let mut other = row(1, "o1");
        other.actor = "square.0".into();
        assert!(multiple_devices(&[row(1, "s1"), other]), "two devices");
    }

    #[test]
    fn missing_row_or_ungrouped_row_asks_for_a_refetch() {
        let mut g = served(vec![]);
        let mut shots = vec![row(1, "s1")];
        let ev = g.apply(
            "webserver.0",
            info("a", 1, true, 1),
            Some(("mevo.0", &key(5, "s5"))),
            &mut shots,
        );
        assert_eq!(ev, GroupEvent::OutOfStep, "the joined shot has no row");

        // s1 grouped, s2 has data but missed its event, s3 joins now.
        let mut shots = vec![row(1, "s1"), row(2, "s2"), row(3, "s3")];
        shots[0].group = Some("a".into());
        let ev = g.apply(
            "webserver.0",
            info("a", 1, true, 3),
            Some(("mevo.0", &key(3, "s3"))),
            &mut shots,
        );
        assert_eq!(ev, GroupEvent::OutOfStep);

        // Ungrouped rows older than every grouped row are history, and a
        // pending row (no data yet) is not missing anything.
        let mut shots = vec![row(1, "s1"), row(2, "s2"), row(3, "s3"), row(4, "s4")];
        shots[1].group = Some("a".into());
        shots[2].ball = None;
        let ev = g.apply(
            "webserver.0",
            info("a", 1, true, 2),
            Some(("mevo.0", &key(4, "s4"))),
            &mut shots,
        );
        assert_eq!(ev, GroupEvent::Applied);
    }

    fn in_mode(mode: ShotGrouping, groups: Vec<ShotGroupInfo>) -> ShotGroups {
        let mut g = served(groups);
        g.mode = mode;
        g
    }

    fn club_group(id: &str, number: u32, club: Club, open: bool, shots: u32) -> ShotGroupInfo {
        ShotGroupInfo {
            club: Some(club),
            ..info(id, number, open, shots)
        }
    }

    fn current(g: &ShotGroups, club: Option<Club>) -> Option<&str> {
        g.current_group(club).map(|g| g.id.as_str())
    }

    #[test]
    fn current_group_is_the_open_set_unless_the_club_differs() {
        let groups = vec![
            club_group("a", 1, Club::Iron7, false, 3),
            club_group("b", 2, Club::Driver, false, 2),
            club_group("c", 3, Club::Iron7, true, 1),
        ];
        // The same in every view: sets are assigned one way.
        for mode in [
            ShotGrouping::BySet,
            ShotGrouping::ByClub,
            ShotGrouping::None,
        ] {
            let g = in_mode(mode, groups.clone());
            assert_eq!(g.open_set().map(|s| s.id.as_str()), Some("c"));
            assert_eq!(current(&g, Some(Club::Iron7)), Some("c"), "{mode:?}");
            // Another club starts a new set; a closed set never reopens.
            assert_eq!(current(&g, Some(Club::Driver)), None, "{mode:?}");
            assert_eq!(current(&g, None), None, "{mode:?}");
        }
        // An empty open set takes whatever club the next shot has.
        let empty = club_group("e", 4, Club::Iron7, true, 0);
        let g = in_mode(ShotGrouping::BySet, vec![empty]);
        assert_eq!(current(&g, Some(Club::Driver)), Some("e"));
        assert_eq!(current(&g, None), Some("e"));
        // Nothing open: the next shot starts a set.
        let g = in_mode(ShotGrouping::BySet, groups[..2].to_vec());
        assert_eq!(current(&g, Some(Club::Iron7)), None);
        // No club selected shares the "no club" set.
        let mut no_club = info("n", 4, true, 2);
        no_club.club = None;
        let g = in_mode(ShotGrouping::ByClub, vec![no_club]);
        assert_eq!(current(&g, None), Some("n"));
        assert_eq!(current(&g, Some(Club::Iron7)), None);
    }

    #[test]
    fn clear_drops_every_set_and_counts_the_session_up() {
        let mut g = ShotGroups::default();
        g.replace(ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::ByClub,
            session: 2,
            current: Some("b".into()),
            groups: vec![info("a", 1, false, 4), info("b", 2, true, 1)],
        });
        let shots = [row(1, "a"), row(2, "b")];
        g.observe_club(Some(Club::Iron7), &shots);
        let rev = g.rev();
        g.clear();
        assert!(g.is_empty());
        assert!(g.open_set().is_none());
        assert_eq!(g.session(), 3);
        assert!(g.rev() > rev, "the caches rebuild");
        assert_eq!(g.selected_at(Some(Club::Iron7), &[]), None);
        // The server and the view stay; its next set is applied.
        assert_eq!(g.server.as_deref(), Some("webserver.0"));
        assert_eq!(g.mode, ShotGrouping::ByClub);
        let mut next = info("c", 1, true, 0);
        next.session = 3;
        assert_eq!(
            g.apply("webserver.0", next, None, &mut []),
            GroupEvent::Applied
        );
        assert_eq!(g.iter_sets().count(), 1);
        // The club in play is observed again, before any row.
        g.observe_club(Some(Club::Iron7), &[]);
        assert_eq!(g.selected_at(Some(Club::Iron7), &[]).map(|a| a.0), Some(1));
    }

    #[test]
    fn session_follows_the_server_and_new_session_commands() {
        let mut later = info("b", 2, true, 1);
        later.session = 3;
        let mut g = ShotGroups::default();
        g.replace(ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::ByClub,
            session: 2,
            current: None,
            groups: vec![info("a", 1, false, 4), later.clone()],
        });
        assert_eq!(g.session(), 3, "never below a listed set's");
        let rev = g.rev();
        g.new_session();
        assert_eq!(g.session(), 4);
        assert!(g.rev() > rev, "a new session rebuilds the caches");
        // A set from a later session lifts it.
        let mut newer = info("c", 3, true, 0);
        newer.session = 6;
        g.apply("webserver.0", newer, None, &mut []);
        assert_eq!(g.session(), 6);
        // A closed record from an earlier session leaves it.
        later.open = false;
        g.apply("webserver.0", later, None, &mut []);
        assert_eq!(g.session(), 6);
        // The reply replaces it.
        g.replace(ShotGroupsResponse {
            server: "webserver.0".into(),
            mode: ShotGrouping::ByClub,
            session: 1,
            current: None,
            groups: vec![],
        });
        assert_eq!(g.session(), 1);
    }

    #[test]
    fn selections_sit_after_the_newest_row_with_data() {
        let mut g = served(vec![]);
        let mut shots = vec![row(1, "s1"), row(2, "s2"), row(3, "s3")];
        shots[2].ball = None;
        // Before any row; then after s2, the newest row with data, though
        // a pending row is newer.
        g.observe_club(Some(Club::Iron7), &[]);
        g.observe_club(Some(Club::Driver), &shots);
        assert_eq!(g.selected_at(Some(Club::Iron7), &shots), Some((1, 1)));
        assert_eq!(g.selected_at(Some(Club::Driver), &shots), Some((5, 2)));
        assert_eq!(g.selected_at(Some(Club::PitchingWedge), &shots), None);
        // A later selection replaces the club's earlier one.
        g.observe_club(Some(Club::Iron7), &shots);
        assert_eq!(g.selected_at(Some(Club::Iron7), &shots), Some((5, 3)));
        // Once its row leaves the history, a selection is older than every
        // row still held.
        let trimmed = vec![row(4, "s4")];
        assert_eq!(g.selected_at(Some(Club::Driver), &trimmed), Some((1, 2)));
    }

    #[test]
    fn strip_disabled_states() {
        let hidden = Strip {
            actions: false,
            new_group: false,
            new_session: false,
            hint: None,
        };
        let open7 = club_group("a", 1, Club::Iron7, true, 3);
        assert_eq!(
            in_mode(ShotGrouping::None, vec![open7.clone()]).strip(Some(Club::Iron7)),
            hidden,
            "the view none shows neither button nor hint"
        );

        // Nothing open yet: New set starts one, New session has nothing to close.
        let s = in_mode(ShotGrouping::BySet, vec![]).strip(Some(Club::Iron7));
        assert!(s.actions && s.new_group && !s.new_session);
        assert_eq!(s.hint.as_deref(), Some("Next shot starts a new set (7I)"));

        // An open set with shots: both enabled, no hint for the same club.
        let s = in_mode(ShotGrouping::BySet, vec![open7.clone()]).strip(Some(Club::Iron7));
        assert!(s.new_group && s.new_session);
        assert_eq!(s.hint, None);
        // By set, a different club: the next shot starts a new set.
        let s = in_mode(ShotGrouping::BySet, vec![open7.clone()]).strip(Some(Club::Driver));
        assert_eq!(s.hint.as_deref(), Some("Next shot starts a new set (DR)"));
        // By club combines the club's sets, so the hint says nothing there.
        let s = in_mode(ShotGrouping::ByClub, vec![open7.clone()]).strip(Some(Club::Driver));
        assert!(s.actions && s.new_group && s.new_session);
        assert_eq!(s.hint, None);

        // An empty open set: New set is disabled; it takes any club.
        let empty = club_group("b", 2, Club::Iron7, true, 0);
        for mode in [ShotGrouping::BySet, ShotGrouping::ByClub] {
            let s = in_mode(mode, vec![empty.clone()]).strip(Some(Club::Driver));
            assert!(!s.new_group && s.new_session, "{mode:?}");
            assert_eq!(s.hint, None, "{mode:?}");
        }

        // Only closed sets (after New session): New session is disabled.
        let closed = club_group("c", 1, Club::Iron7, false, 3);
        let s = in_mode(ShotGrouping::ByClub, vec![closed]).strip(Some(Club::Iron7));
        assert!(s.new_group && !s.new_session);

        // No club selected, in either view.
        for mode in [ShotGrouping::ByClub, ShotGrouping::BySet] {
            let s = in_mode(mode, vec![open7.clone()]).strip(None);
            assert_eq!(s.hint.as_deref(), Some("No club selected"));
        }
    }
}
