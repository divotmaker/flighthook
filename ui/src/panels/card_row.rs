//! The row of cards right of the range bars on the Cards view: what each
//! card shows, and how a click moves it on. No egui here: `shots` lays the
//! cards out and paints them.
//!
//! The sources, in cycle order, are the stats card, each connected camera in
//! Devices card order, and face impact. By default the leftmost card shows
//! the stats, face impact takes the rightmost, and cameras fill the cards
//! between. The first time face impact is available it takes the rightmost
//! card, as a new card when one fits, else in place of what was there; after
//! that the user's clicks stand. Nothing shows on two cards at once.

/// What a card in the row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CardSource {
    /// The range bars' averages and spread.
    Stats,
    /// A camera actor's clip of the shown shot.
    Camera(String),
    /// The face impact panel.
    Face,
}

/// The sources in cycle order: the stats, `cameras` (connected, in Devices
/// card order), then face impact when `face`.
pub(crate) fn sources(cameras: impl IntoIterator<Item = String>, face: bool) -> Vec<CardSource> {
    std::iter::once(CardSource::Stats)
        .chain(cameras.into_iter().map(CardSource::Camera))
        .chain(face.then_some(CardSource::Face))
        .collect()
}

/// What `slots` cards show, given what they showed (`prev`, left to right)
/// and the `sources` available now, in cycle order. `face_new`: face impact
/// is available for the first time.
///
/// A card whose source went takes the first source not on screen other
/// than face impact; when face impact is all that is left, it takes the
/// rightmost card instead, else the card goes. With fewer slots, cards go from the right, but face impact keeps
/// the rightmost card when it was on screen. Face impact's first appearance
/// takes the rightmost card. New slots take the sources not on screen in
/// cycle order, left of face impact. With nothing shown before, the cards
/// take their defaults.
pub(crate) fn assign(
    prev: &[CardSource],
    sources: &[CardSource],
    slots: usize,
    face_new: bool,
) -> Vec<CardSource> {
    use CardSource::Face;

    if slots == 0 || sources.is_empty() {
        return Vec::new();
    }
    let face_new = (face_new || prev.is_empty()) && sources.contains(&Face);
    let mut shown: Vec<CardSource> = Vec::with_capacity(slots);
    // A freed card that only face impact could take: face impact goes on
    // the rightmost card instead.
    let mut face_freed = false;
    for (i, s) in prev.iter().enumerate() {
        if shown.contains(s) {
            continue;
        }
        if sources.contains(s) {
            shown.push(s.clone());
            continue;
        }
        let free = |c: &&CardSource| !shown.contains(c) && !prev[i..].contains(c);
        if let Some(next) = sources.iter().filter(|c| **c != Face).find(free) {
            shown.push(next.clone());
        } else if sources.iter().any(|c| *c == Face && free(&c)) {
            face_freed = true;
        }
    }
    if face_freed && !shown.contains(&Face) {
        shown.push(Face);
    }
    if shown.len() > slots {
        match shown.iter().position(|s| *s == Face) {
            Some(at) => {
                shown.remove(at);
                shown.truncate(slots - 1);
                shown.push(Face);
            }
            None => shown.truncate(slots),
        }
    }
    if face_new {
        shown.retain(|s| *s != Face);
        shown.truncate(slots - 1);
        shown.push(Face);
    }
    while shown.len() < slots {
        let Some(next) = sources.iter().find(|s| !shown.contains(s)) else {
            break;
        };
        if *next != Face && shown.last() == Some(&Face) {
            shown.insert(shown.len() - 1, next.clone());
        } else {
            shown.push(next.clone());
        }
    }
    shown
}

/// Move card `slot` of `shown` on to the next source in cycle order that no
/// other card shows, wrapping round. Nothing changes when every source is
/// on screen.
pub(crate) fn cycle(shown: &mut [CardSource], sources: &[CardSource], slot: usize) {
    let Some(current) = shown.get(slot) else {
        return;
    };
    let at = sources.iter().position(|s| s == current).unwrap_or(0);
    let next = (1..sources.len())
        .map(|step| &sources[(at + step) % sources.len()])
        .find(|s| !shown.contains(s))
        .cloned();
    if let Some(next) = next {
        shown[slot] = next;
    }
}

/// The card row's state over a session.
#[derive(Debug, Default)]
pub(crate) struct CardRow {
    /// What each card shows, left to right.
    pub(crate) shown: Vec<CardSource>,
    /// Face impact has been available since the session (or the last Clear
    /// history) began.
    face_seen: bool,
}

impl CardRow {
    /// Fit the row to `slots` cards over `sources` ([`assign`]).
    pub(crate) fn update(&mut self, sources: &[CardSource], slots: usize) {
        let face = sources.contains(&CardSource::Face);
        self.shown = assign(&self.shown, sources, slots, face && !self.face_seen);
        if face && !self.shown.is_empty() {
            self.face_seen = true;
        }
    }

    /// A click on card `slot` ([`cycle`]).
    pub(crate) fn click(&mut self, sources: &[CardSource], slot: usize) {
        cycle(&mut self.shown, sources, slot);
    }

    /// Clear history: face impact's next appearance takes the rightmost card
    /// again.
    pub(crate) fn clear(&mut self) {
        self.face_seen = false;
    }
}

#[cfg(test)]
mod tests {
    use super::CardSource::{Camera, Face, Stats};
    use super::*;

    fn cam(n: u8) -> CardSource {
        Camera(format!("camera.{n}"))
    }

    fn cams(n: u8) -> Vec<String> {
        (0..n).map(|i| format!("camera.{i}")).collect()
    }

    #[test]
    fn sources_run_stats_cameras_then_face() {
        assert_eq!(sources(cams(0), false), [Stats]);
        assert_eq!(sources(cams(2), true), [Stats, cam(0), cam(1), Face]);
    }

    #[test]
    fn the_defaults_put_stats_left_face_right_and_cameras_between() {
        let all = sources(cams(2), true);
        assert_eq!(assign(&[], &all, 3, false), [Stats, cam(0), Face]);
        assert_eq!(assign(&[], &all, 2, false), [Stats, Face]);
        // One slot: face impact takes it once it is available.
        assert_eq!(assign(&[], &all, 1, false), [Face]);
        assert_eq!(assign(&[], &sources(cams(2), false), 1, false), [Stats]);
        // Two slots, no face impact: Stats, camera 1.
        assert_eq!(
            assign(&[], &sources(cams(2), false), 2, false),
            [Stats, cam(0)]
        );
        assert_eq!(
            assign(&[], &sources(cams(2), false), 3, false),
            [Stats, cam(0), cam(1)]
        );
        // Never more cards than sources.
        assert_eq!(
            assign(&[], &sources(cams(0), true), 3, false),
            [Stats, Face]
        );
        assert_eq!(assign(&[], &[Stats], 3, false), [Stats]);
        assert!(assign(&[], &all, 0, false).is_empty());
    }

    #[test]
    fn face_impact_first_appears_on_the_rightmost_card() {
        // A new slot when one fits.
        let before = [Stats, cam(0)];
        assert_eq!(
            assign(&before, &sources(cams(1), true), 3, true),
            [Stats, cam(0), Face]
        );
        // Else it takes the rightmost card over.
        assert_eq!(
            assign(&before, &sources(cams(1), true), 2, true),
            [Stats, Face]
        );
        assert_eq!(assign(&[Stats], &sources(cams(0), true), 1, true), [Face]);
        // After that, the user's choices stand.
        let mut row = CardRow::default();
        let all = sources(cams(1), false);
        row.update(&all, 2);
        assert_eq!(row.shown, [Stats, cam(0)]);
        let all = sources(cams(1), true);
        row.update(&all, 2);
        assert_eq!(row.shown, [Stats, Face]);
        row.click(&all, 1);
        assert_eq!(row.shown, [Stats, cam(0)]);
        row.update(&all, 2);
        assert_eq!(row.shown, [Stats, cam(0)], "not taken over again");
        // Clear history: the next appearance takes it again.
        row.update(&sources(cams(1), false), 2);
        row.clear();
        row.update(&all, 2);
        assert_eq!(row.shown, [Stats, Face]);
    }

    #[test]
    fn a_click_moves_on_to_the_next_source_not_on_screen() {
        let all = sources(cams(2), true);
        let mut shown = vec![Stats, cam(0), Face];
        // The middle card moves on to the next camera.
        cycle(&mut shown, &all, 1);
        assert_eq!(shown, [Stats, cam(1), Face]);
        // It wraps round past the end of the cycle, skipping what is shown.
        cycle(&mut shown, &all, 1);
        assert_eq!(shown, [Stats, cam(0), Face]);
        cycle(&mut shown, &all, 2);
        assert_eq!(shown, [Stats, cam(0), cam(1)]);
        cycle(&mut shown, &all, 2);
        assert_eq!(shown, [Stats, cam(0), Face], "wrapped to Face");
        cycle(&mut shown, &all, 0);
        assert_eq!(shown, [cam(1), cam(0), Face]);
        // Every source on screen: nothing changes.
        let all = sources(cams(1), true);
        let mut shown = vec![Stats, cam(0), Face];
        for slot in 0..3 {
            cycle(&mut shown, &all, slot);
            assert_eq!(shown, [Stats, cam(0), Face]);
        }
        // One card cycles through every source.
        let all = sources(cams(1), true);
        let mut shown = vec![Stats];
        let mut seen = Vec::new();
        for _ in 0..3 {
            cycle(&mut shown, &all, 0);
            seen.push(shown[0].clone());
        }
        assert_eq!(seen, [cam(0), Face, Stats]);
        // Out of range: no-op.
        cycle(&mut shown, &all, 5);
        assert_eq!(shown, [Stats]);
    }

    #[test]
    fn cameras_come_and_go() {
        let mut row = CardRow::default();
        row.update(&sources(cams(0), true), 2);
        assert_eq!(row.shown, [Stats, Face]);
        // A camera connects: a new slot, left of face impact.
        row.update(&sources(cams(1), true), 3);
        assert_eq!(row.shown, [Stats, cam(0), Face]);
        // Another connects with no slot left: reachable by cycling.
        let four = sources(cams(2), true);
        row.update(&four, 3);
        assert_eq!(row.shown, [Stats, cam(0), Face]);
        row.click(&four, 1);
        assert_eq!(row.shown, [Stats, cam(1), Face]);
        // camera.1 disconnects: its card takes the next source not shown.
        row.update(&[Stats, cam(0), Face], 3);
        assert_eq!(row.shown, [Stats, cam(0), Face]);
        // camera.0 disconnects too: one slot fewer.
        row.update(&sources(cams(0), true), 2);
        assert_eq!(row.shown, [Stats, Face]);
        // A freed card in place: the first source not on screen.
        let shown = assign(&[cam(0), Stats, Face], &[Stats, cam(1), Face], 3, false);
        assert_eq!(shown, [cam(1), Stats, Face]);
        // Face impact never moves into a freed middle card: it takes the
        // rightmost.
        let shown = assign(&[Stats, cam(0), cam(1)], &[Stats, cam(1), Face], 3, false);
        assert_eq!(shown, [Stats, cam(1), Face]);
        let shown = assign(&[cam(0), Stats], &[Stats, Face], 2, false);
        assert_eq!(shown, [Stats, Face]);
        // A camera not on screen still takes the freed card in place.
        let shown = assign(
            &[Stats, cam(0), cam(1)],
            &[Stats, cam(1), cam(2), Face],
            3,
            false,
        );
        assert_eq!(shown, [Stats, cam(2), cam(1)]);
    }

    #[test]
    fn fewer_slots_drop_cards_from_the_right_and_face_keeps_the_rightmost() {
        let all = sources(cams(2), true);
        assert_eq!(
            assign(&[Stats, cam(0), Face], &all, 2, false),
            [Stats, Face]
        );
        assert_eq!(assign(&[Stats, cam(0), Face], &all, 1, false), [Face]);
        assert_eq!(
            assign(&[Stats, Face, cam(0)], &all, 2, false),
            [Stats, Face]
        );
        // Without face impact on screen, from the right.
        assert_eq!(
            assign(&[Stats, cam(0), cam(1)], &all, 2, false),
            [Stats, cam(0)]
        );
        assert_eq!(assign(&[cam(1), Stats, cam(0)], &all, 1, false), [cam(1)]);
        // More slots again: the sources not on screen, left of face impact.
        assert_eq!(assign(&[Face], &all, 3, false), [Stats, cam(0), Face]);
        assert_eq!(assign(&[cam(1)], &all, 3, false), [cam(1), Stats, cam(0)]);
    }

    #[test]
    fn nothing_is_shown_twice() {
        let all = sources(cams(2), true);
        let prevs: [&[CardSource]; 5] = [
            &[],
            &[Stats, Stats],
            &[Face, Face, Face],
            &[cam(5), cam(6), cam(7)],
            &[cam(0), cam(9), cam(0)],
        ];
        for prev in prevs {
            for slots in 0..=4 {
                for face_new in [false, true] {
                    let shown = assign(prev, &all, slots, face_new);
                    assert_eq!(shown.len(), slots.min(all.len()), "{prev:?} {slots}");
                    for (i, s) in shown.iter().enumerate() {
                        assert!(all.contains(s), "{prev:?} {slots}: {s:?}");
                        assert!(!shown[i + 1..].contains(s), "{prev:?} {slots}: {shown:?}");
                    }
                }
            }
        }
    }
}
