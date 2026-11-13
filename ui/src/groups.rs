//! Shot sets as the UI knows them: the practice blocks of `/api/blocks`,
//! fetched again whenever flighthook's practice actor publishes one, plus
//! the session they are counted in.

use crate::types::{BlockInfo, BlocksResponse, Club, ShotGrouping, ShotKey, ShotRow};

/// A set: one practice block as the Shots tab shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetInfo {
    /// The block's id (UUID v4); a stored shot's `group`.
    pub(crate) id: String,
    /// Display ordinal ("SET 3"): counts from 1 within the session.
    pub(crate) number: u32,
    /// The practice session the set belongs to (from 1 each run).
    pub(crate) session: u32,
    /// Club the set is hit with; `None` for no club.
    pub(crate) club: Option<Club>,
    /// When the set started, in milliseconds since the Unix epoch: its
    /// first shot, else when it opened.
    pub(crate) started_at: u64,
    /// Not closed by the practice actor yet.
    pub(crate) open: bool,
    /// Stored shots with data that belong to it.
    pub(crate) shots: u32,
    /// The block's golfer id, as the wire carries it (resolve it with the
    /// lookup rule).
    pub(crate) golfer: Option<String>,
}

impl From<BlockInfo> for SetInfo {
    fn from(b: BlockInfo) -> Self {
        Self {
            id: b.block.id,
            number: b.block.number,
            session: b.block.session,
            club: b.block.club,
            started_at: b.first_shot_at.unwrap_or(b.block.started_at),
            open: b.open,
            shots: b.shots,
            golfer: b.block.golfer.map(|g| g.as_str().to_owned()),
        }
    }
}

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
        _ => "Club",
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
     a club change or New set starts one. \
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
    /// The view shown: the saved default view (`shot_grouping`), or the
    /// one picked since.
    pub(crate) mode: ShotGrouping,
    /// Ascending session, then `number`.
    list: Vec<SetInfo>,
    /// Current session: the server's, and never below a listed set's.
    session: u32,
    /// The golfer up (as the wire names it) and the club in play when last
    /// observed; `None` before the first observation.
    observed: Option<(Option<String>, Option<Club>)>,
    /// Where in the shot history each golfer last selected each club.
    selections: Vec<Selection>,
    /// Count of observed selections, ordering selections at one place.
    seq: u64,
    /// Bumped by every `replace` and every clear, so caches of what the
    /// sets cover know when to rebuild.
    rev: u64,
    /// The empty open set this UI asked for with New set, per golfer key:
    /// shown before its first shot. Any other empty open set is hidden.
    manual: std::collections::BTreeMap<Option<String>, String>,
    /// New set was sent for the golfer with this key (the viewed golfer
    /// then); the next reply's empty open set of that golfer goes into
    /// [`Self::manual`].
    asked_new: Option<Option<String>>,
    /// Resolves a shot's or set's golfer id to its golfer (the lookup rule).
    golfers: GolferKeys,
    /// The turn's golfer id, as the wire names it.
    up: Option<String>,
    /// The viewed golfer's key, when the view is on a golfer other than
    /// the golfer up; `None` while the view is the golfer up.
    view: Option<Option<String>>,
}

/// The lookup rule over the saved golfers, cached: each identifier a golfer
/// lists mapped to its key by [`flighthook::FlighthookConfig::golfer_key`].
/// An id no golfer lists is its own key.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub(crate) struct GolferKeys(std::collections::BTreeMap<String, String>);

impl GolferKeys {
    /// The keys of `config`'s golfers.
    pub(crate) fn of(config: Option<&flighthook::FlighthookConfig>) -> Self {
        let mut map = std::collections::BTreeMap::new();
        if let Some(config) = config {
            for id in config.golfers.iter().flat_map(|g| &g.identifiers) {
                if let Some(key) = config.golfer_key(Some(id)) {
                    map.entry(id.clone()).or_insert(key);
                }
            }
        }
        Self(map)
    }

    /// The key golfer id `id` counts under: its golfer's own id, else
    /// `id`. `None` (no golfer) is its own key.
    pub(crate) fn key<'a>(&'a self, id: Option<&'a str>) -> Option<&'a str> {
        let id = id?;
        Some(self.0.get(id).map_or(id, String::as_str))
    }
}

/// A club selection by the golfer up, placed in the shot history: after
/// the row `after`, the newest row with data when the club was selected
/// (`None` when there was none).
#[derive(Debug)]
struct Selection {
    /// The golfer up, as the wire names them.
    golfer: Option<String>,
    club: Option<Club>,
    after: Option<(String, ShotKey)>,
    seq: u64,
}

/// A place in the shot history, for ordering activity newest last: a row
/// with data at index `i` is `(2i + 2, 0)`; a selection after it is
/// `(2i + 3, seq)`, and one made before any row is `(1, seq)`.
pub(crate) type Activity = (usize, u64);

impl ShotGroups {
    /// Replace every set with a `/api/blocks` reply.
    pub(crate) fn replace(&mut self, resp: BlocksResponse) {
        self.list = resp.blocks.into_iter().map(SetInfo::from).collect();
        self.list.sort_by_key(|g| (g.session, g.number));
        self.session = self
            .list
            .iter()
            .map(|g| g.session)
            .fold(resp.session, u32::max);
        if let Some(key) = self.asked_new.clone()
            && let Some(open) = self.open_set_of(key.as_deref()).filter(|g| g.shots == 0)
        {
            let id = open.id.clone();
            self.manual.insert(key, id);
            self.asked_new = None;
        }
        let list = &self.list;
        self.manual
            .retain(|_, id| list.iter().any(|g| g.open && g.id == *id));
        self.rev += 1;
    }

    /// This UI sent New set for the viewed golfer: the empty open set the
    /// reply brings is shown.
    pub(crate) fn asked_new_set(&mut self) {
        self.asked_new = Some(self.view_key().map(str::to_owned));
    }

    /// Revision of the set list: changes with every `replace` and every
    /// clear.
    pub(crate) fn rev(&self) -> u64 {
        self.rev
    }

    /// The current session.
    pub(crate) fn session(&self) -> u32 {
        self.session
    }

    /// The server cleared its shot history: every set goes (the next
    /// session's block comes with the next fetch). The club in play is
    /// observed afresh.
    pub(crate) fn clear(&mut self) {
        self.list.clear();
        self.manual.clear();
        self.asked_new = None;
        self.observed = None;
        self.selections.clear();
        self.rev += 1;
    }

    /// Whether no group is listed.
    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The group with id `id`.
    pub(crate) fn get(&self, id: &str) -> Option<&SetInfo> {
        self.list.iter().find(|g| g.id == id)
    }

    /// Take the saved golfers (`keys`) and the turn's golfer id (`up`).
    /// A change of either changes [`Self::rev`].
    pub(crate) fn set_golfers(&mut self, keys: GolferKeys, up: Option<&str>) {
        if self.golfers != keys || self.up.as_deref() != up {
            self.golfers = keys;
            self.up = up.map(str::to_owned);
            self.rev += 1;
        }
    }

    /// The keys that resolve a golfer id (the lookup rule).
    pub(crate) fn golfers(&self) -> &GolferKeys {
        &self.golfers
    }

    /// The key of the golfer up.
    pub(crate) fn up_key(&self) -> Option<&str> {
        self.golfers.key(self.up.as_deref())
    }

    /// Take the viewed golfer's key: `None` while the view is the golfer
    /// up, else `Some(key)`. A change changes [`Self::rev`].
    pub(crate) fn set_view(&mut self, view: Option<Option<String>>) {
        if self.view != view {
            self.view = view;
            self.rev += 1;
        }
    }

    /// The key of the viewed golfer: the golfer up unless the view is on
    /// someone else.
    pub(crate) fn view_key(&self) -> Option<&str> {
        match &self.view {
            Some(key) => key.as_deref(),
            None => self.up_key(),
        }
    }

    /// Whether the view is on the golfer up.
    pub(crate) fn viewing_up(&self) -> bool {
        self.view_key() == self.up_key()
    }

    /// The viewed golfer's open set (the practice actor keeps one open per
    /// golfer).
    pub(crate) fn open_set(&self) -> Option<&SetInfo> {
        self.open_set_of(self.view_key())
    }

    /// The open set of the golfer with key `key`.
    fn open_set_of(&self, key: Option<&str>) -> Option<&SetInfo> {
        self.list
            .iter()
            .filter(|g| g.open && self.golfers.key(g.golfer.as_deref()) == key)
            .max_by_key(|g| g.number)
    }

    /// Whether set `id` is an empty set this UI asked for with New set.
    fn asked_for(&self, id: &str) -> bool {
        self.manual.values().any(|m| m == id)
    }

    /// The set the viewed golfer's next shot joins when `club` is in play:
    /// the open set, when it has no shots yet (it takes the first shot's
    /// club) or, while the view is the golfer up, its club is `club`. The
    /// club in play is the golfer up's, so for anyone else only an empty
    /// open set counts. `None` when the next shot starts a new set.
    pub(crate) fn current_group(&self, club: Option<Club>) -> Option<&SetInfo> {
        let up = self.viewing_up();
        self.open_set()
            .filter(|g| g.shots == 0 || (up && g.club == club))
    }

    /// The open set as the list shows it: one with shots, or the empty set
    /// this UI asked for with New set. An empty open set is hidden
    /// otherwise.
    pub(crate) fn shown_open(&self) -> Option<&SetInfo> {
        self.open_set()
            .filter(|g| g.shots > 0 || self.asked_for(&g.id))
    }

    /// [`Self::current_group`] among the shown sets ([`Self::shown_open`]):
    /// `None` when the next shot starts a set the list does not show yet.
    pub(crate) fn shown_current(&self, club: Option<Club>) -> Option<&SetInfo> {
        self.current_group(club)
            .filter(|g| g.shots > 0 || self.asked_for(&g.id))
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
            Some(c) if self.mode == ShotGrouping::BySet && self.shown_current(club).is_none() => {
                Some(format!("Next shot starts a new set ({c})"))
            }
            Some(_) => None,
        };
        let open = self.shown_open();
        let has_shots = self
            .list
            .iter()
            .any(|g| g.session == self.session && g.shots > 0);
        Strip {
            actions: true,
            new_group: open.is_none_or(|g| g.shots > 0),
            new_session: has_shots || open.is_some(),
            hint,
        }
    }

    /// Note the club in play; a change of it or of the golfer up is a
    /// selection of `club` by the golfer up, placed after the newest row
    /// with data in `shots`.
    pub(crate) fn observe_club(&mut self, club: Option<Club>, shots: &[ShotRow]) {
        let now = (self.up.clone(), club);
        if self.observed.as_ref() == Some(&now) {
            return;
        }
        self.observed = Some(now);
        self.seq += 1;
        let after = shots.iter().rfind(|r| r.has_data()).map(|r| {
            let key = ShotKey {
                shot_id: r.shot_id.clone(),
                shot_number: r.shot_number,
            };
            (r.actor.clone(), key)
        });
        let selection = Selection {
            golfer: self.up.clone(),
            club,
            after,
            seq: self.seq,
        };
        match self
            .selections
            .iter_mut()
            .find(|s| s.club == club && s.golfer == selection.golfer)
        {
            Some(s) => *s = selection,
            None => self.selections.push(selection),
        }
    }

    /// Where the golfer keyed `golfer` (the lookup rule) last selected
    /// `club` in `shots`. A selection whose row has left `shots` is older
    /// than every row.
    pub(crate) fn selected_at(
        &self,
        golfer: Option<&str>,
        club: Option<Club>,
        shots: &[ShotRow],
    ) -> Option<Activity> {
        self.selections
            .iter()
            .filter(|s| s.club == club && self.golfers.key(s.golfer.as_deref()) == golfer)
            .map(|s| {
                let at = s
                    .after
                    .as_ref()
                    .and_then(|(actor, key)| find_row(shots, actor, key))
                    .map_or(1, |i| 2 * i + 3);
                (at, s.seq)
            })
            .max()
    }

    /// Sets `list` in view `mode`, as a `/api/blocks` reply listing them
    /// would leave it after this UI's New set: an empty open set in `list`
    /// is shown (tests).
    #[cfg(test)]
    pub(crate) fn with_sets(mode: ShotGrouping, list: Vec<SetInfo>) -> Self {
        let mut g = Self {
            mode,
            ..Self::default()
        };
        g.session = list.iter().map(|s| s.session).max().unwrap_or(1);
        g.manual = list
            .iter()
            .filter(|s| s.open && s.shots == 0)
            .map(|s| (s.golfer.clone(), s.id.clone()))
            .collect();
        g.list = list;
        g.list.sort_by_key(|s| (s.session, s.number));
        g.rev += 1;
        g
    }

    /// What the `/api/blocks` reply after a New session holds (tests): the
    /// open set closed, the session counted up.
    #[cfg(test)]
    pub(crate) fn new_session(&mut self) {
        for g in &mut self.list {
            g.open = false;
        }
        self.session += 1;
        self.rev += 1;
    }

    /// Every set, ascending session and `number`.
    pub(crate) fn iter_sets(&self) -> impl Iterator<Item = &SetInfo> {
        self.list.iter()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Launch;

    /// A set and a shot row carry their golfer id as the wire named it.
    #[test]
    fn sets_and_rows_carry_their_golfer() {
        let golfer: flighthook::GolferId = "pureplay.local-3".parse().expect("id");
        let mut block = flighthook::PracticeBlock {
            id: "b".into(),
            golfer: Some(golfer.clone()),
            club: Some(Club::Iron7),
            number: 1,
            session: 1,
            started_at: 0,
        };
        let info = |block: flighthook::PracticeBlock| BlockInfo {
            block,
            open: true,
            shots: 0,
            first_shot_at: None,
        };
        let set = SetInfo::from(info(block.clone()));
        assert_eq!(set.golfer.as_deref(), Some("pureplay.local-3"));
        block.golfer = None;
        assert_eq!(SetInfo::from(info(block)).golfer, None);
        let shot = crate::types::ShotData {
            turn: Some(flighthook::GolferTurn::new(
                Some(golfer),
                Some(Club::Iron7),
                flighthook::Handedness::Right,
                flighthook::Mode::Full,
            )),
            ..crate::types::ShotData::default()
        };
        assert_eq!(
            ShotRow::from(shot).golfer.as_deref(),
            Some("pureplay.local-3")
        );
    }

    fn in_mode(mode: ShotGrouping, groups: Vec<SetInfo>) -> ShotGroups {
        let mut g = served(groups);
        g.mode = mode;
        g
    }

    fn club_group(id: &str, number: u32, club: Club, open: bool, shots: u32) -> SetInfo {
        SetInfo {
            club: Some(club),
            ..info(id, number, open, shots)
        }
    }

    fn current(g: &ShotGroups, club: Option<Club>) -> Option<&str> {
        g.current_group(club).map(|g| g.id.as_str())
    }

    fn info(id: &str, number: u32, open: bool, shots: u32) -> SetInfo {
        SetInfo {
            golfer: None,
            id: id.into(),
            number,
            session: 1,
            club: Some(Club::Iron7),
            started_at: 0,
            open,
            shots,
        }
    }

    fn row(n: u32, id: &str) -> ShotRow {
        ShotRow {
            golfer: None,
            actor: "mevo.0".into(),
            shot_id: id.into(),
            shot_number: n,
            launch: Some(Launch::default()),
            flight: None,
            club: None,
            impact: None,
            derived: None,
            club_selected: None,
            handed: None,
            group: None,
            finished: true,
            video: Vec::new(),
        }
    }

    fn key(n: u32, id: &str) -> ShotKey {
        ShotKey {
            shot_id: id.into(),
            shot_number: n,
        }
    }

    fn served(groups: Vec<SetInfo>) -> ShotGroups {
        ShotGroups::with_sets(ShotGrouping::BySet, groups)
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
    fn selections_sit_after_the_newest_row_with_data() {
        let mut g = served(vec![]);
        let mut shots = vec![row(1, "s1"), row(2, "s2"), row(3, "s3")];
        shots[2].launch = None;
        // Before any row; then after s2, the newest row with data, though
        // a pending row is newer.
        g.observe_club(Some(Club::Iron7), &[]);
        g.observe_club(Some(Club::Driver), &shots);
        assert_eq!(g.selected_at(None, Some(Club::Iron7), &shots), Some((1, 1)));
        assert_eq!(
            g.selected_at(None, Some(Club::Driver), &shots),
            Some((5, 2))
        );
        assert_eq!(g.selected_at(None, Some(Club::PitchingWedge), &shots), None);
        // A later selection replaces the club's earlier one.
        g.observe_club(Some(Club::Iron7), &shots);
        assert_eq!(g.selected_at(None, Some(Club::Iron7), &shots), Some((5, 3)));
        // Once its row leaves the history, a selection is older than every
        // row still held.
        let trimmed = vec![row(4, "s4")];
        assert_eq!(
            g.selected_at(None, Some(Club::Driver), &trimmed),
            Some((1, 2))
        );
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
        let mut after = in_mode(ShotGrouping::ByClub, vec![closed]);
        after.new_session();
        let s = after.strip(Some(Club::Iron7));
        assert!(s.new_group && !s.new_session);

        // No club selected, in either view.
        for mode in [ShotGrouping::ByClub, ShotGrouping::BySet] {
            let s = in_mode(mode, vec![open7.clone()]).strip(None);
            assert_eq!(s.hint.as_deref(), Some("No club selected"));
        }
    }

    /// `/api/blocks` as the server sends it: `(id, session, club, open,
    /// shots, first shot at)`, numbered in order.
    type Block<'a> = (&'a str, u32, Club, bool, u32, Option<u64>);

    fn reply(session: u32, sets: &[Block<'_>]) -> BlocksResponse {
        BlocksResponse {
            session,
            blocks: sets
                .iter()
                .zip(1..)
                .map(
                    |(&(id, session, club, open, shots, first), number)| BlockInfo {
                        block: flighthook::PracticeBlock {
                            id: id.into(),
                            golfer: None,
                            club: Some(club),
                            number,
                            session,
                            started_at: 1_000,
                        },
                        open,
                        shots,
                        first_shot_at: first,
                    },
                )
                .collect(),
        }
    }

    /// With the server's shape (an empty open block at start and after a
    /// club change), the empty set is hidden: no next-shot
    /// set, New session only once the session has shots, the hint as
    /// before; New set from this UI shows the set it opened. A set's time
    /// is its first shot's.
    #[test]
    fn an_empty_open_block_is_hidden_unless_new_set_asked_for_it() {
        let mut g = ShotGroups {
            mode: ShotGrouping::BySet,
            ..ShotGroups::default()
        };
        // Start: the practice actor's first block, no shots.
        g.replace(reply(1, &[("a", 1, Club::Iron7, true, 0, None)]));
        assert_eq!(g.shown_open(), None);
        assert_eq!(g.shown_current(Some(Club::Iron7)), None);
        let s = g.strip(Some(Club::Iron7));
        assert!(s.new_group && !s.new_session);
        assert_eq!(s.hint.as_deref(), Some("Next shot starts a new set (7I)"));
        // Two 7I shots, then DR: block a closed, b open and empty.
        g.replace(reply(
            1,
            &[
                ("a", 1, Club::Iron7, false, 2, Some(5_000)),
                ("b", 1, Club::Driver, true, 0, None),
            ],
        ));
        assert_eq!(g.get("a").map(|s| s.started_at), Some(5_000), "first shot");
        assert_eq!(g.shown_current(Some(Club::Driver)), None);
        let s = g.strip(Some(Club::Driver));
        assert!(s.new_group && s.new_session);
        assert_eq!(s.hint.as_deref(), Some("Next shot starts a new set (DR)"));
        // New set from this UI: the block it leaves open is shown.
        g.asked_new_set();
        g.replace(reply(
            1,
            &[
                ("a", 1, Club::Iron7, false, 2, Some(5_000)),
                ("b", 1, Club::Driver, true, 0, None),
            ],
        ));
        assert_eq!(
            g.shown_current(Some(Club::Driver)).map(|s| s.id.as_str()),
            Some("b")
        );
        let s = g.strip(Some(Club::Driver));
        assert!(!s.new_group && s.new_session && s.hint.is_none());
        // New session: nothing of the new session has shots.
        g.replace(reply(
            2,
            &[
                ("a", 1, Club::Iron7, false, 2, Some(5_000)),
                ("b", 1, Club::Driver, false, 0, None),
                ("c", 2, Club::Driver, true, 0, None),
            ],
        ));
        let s = g.strip(Some(Club::Driver));
        assert!(s.new_group && !s.new_session);
        assert_eq!(g.shown_open(), None);
    }

    /// New set for two golfers: each golfer's asked-for empty set shows
    /// while that golfer is up, and a golfer change between the click and
    /// the reply marks the asker's set, not the new golfer's.
    #[test]
    fn each_golfer_keeps_the_empty_set_it_asked_for() {
        let set = |id: &str, golfer: &str, number: u32, open: bool, shots: u32| BlockInfo {
            block: flighthook::PracticeBlock {
                id: id.into(),
                golfer: Some(golfer.parse().expect("id")),
                club: Some(Club::Iron7),
                number,
                session: 1,
                started_at: 1_000,
            },
            open,
            shots,
            first_shot_at: None,
        };
        let reply = |blocks: Vec<BlockInfo>| BlocksResponse { session: 1, blocks };
        let mut g = ShotGroups {
            mode: ShotGrouping::BySet,
            ..ShotGroups::default()
        };
        let keys = GolferKeys::default();
        let shown = |g: &ShotGroups| g.shown_open().map(|s| s.id.clone());
        // Eric presses New set.
        g.set_golfers(keys.clone(), Some("flighthook.e"));
        g.asked_new_set();
        g.replace(reply(vec![
            set("e1", "flighthook.e", 1, false, 2),
            set("e2", "flighthook.e", 2, true, 0),
        ]));
        assert_eq!(shown(&g).as_deref(), Some("e2"));
        // Sam comes up and presses New set.
        g.set_golfers(keys.clone(), Some("flighthook.s"));
        g.asked_new_set();
        g.replace(reply(vec![
            set("e1", "flighthook.e", 1, false, 2),
            set("e2", "flighthook.e", 2, true, 0),
            set("s1", "flighthook.s", 1, false, 1),
            set("s2", "flighthook.s", 2, true, 0),
        ]));
        assert_eq!(shown(&g).as_deref(), Some("s2"));
        // Eric back: his empty set still shows.
        g.set_golfers(keys.clone(), Some("flighthook.e"));
        assert_eq!(shown(&g).as_deref(), Some("e2"));

        // Eric clicks; Sam is up before the reply comes.
        let mut g = ShotGroups {
            mode: ShotGrouping::BySet,
            ..ShotGroups::default()
        };
        g.set_golfers(keys.clone(), Some("flighthook.e"));
        g.asked_new_set();
        g.set_golfers(keys.clone(), Some("flighthook.s"));
        g.replace(reply(vec![
            set("e1", "flighthook.e", 1, false, 2),
            set("e2", "flighthook.e", 2, true, 0),
            set("s1", "flighthook.s", 1, true, 0),
        ]));
        assert_eq!(shown(&g), None, "Sam's hidden empty set stays hidden");
        g.set_golfers(keys, Some("flighthook.e"));
        assert_eq!(shown(&g).as_deref(), Some("e2"));
    }
}
