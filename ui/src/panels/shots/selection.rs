//! The shown shot: `ShotFocus` (follow the latest or a pinned shot, the new-shot pulse),
//! the header chip and the shot reference that survives a reload.

use super::*;

/// Which shot the Cards view shows, and when the latest shot arrived so the
/// cards (and the table's latest row) can glow briefly for it.
///
/// The view follows the latest shot unless one is pinned from the shot
/// list. A new latest shot clears the pin; so does the pinned shot leaving
/// the list.
///
/// Everything here is over the shots the Shots tab's filter passes
/// (`filter`, [`chart::ShotFilter`]: the viewed golfer and the device
/// filter): the latest shot is the newest one it passes, and a shot it does
/// not pass never pulses or moves the shot on show. A new filter shows its
/// newest shot, without a pulse, unless the shot on show passes it.
#[derive(Default)]
pub(crate) struct ShotFocus {
    /// The latest shot (the newest one with data).
    pub(super) latest: Option<ShotRef>,
    /// The shot picked in the shot list.
    pub(super) pinned: Option<ShotRef>,
    pub(super) started: Option<f64>,
    /// The list should scroll the shot on show into view.
    pub(super) reveal: bool,
    /// The filter `latest` was found under.
    pub(super) filter: chart::FilterKey,
}

impl ShotFocus {
    /// Note the latest shot; a new one starts a pulse at `now` and becomes
    /// the shot on show. A changed `filter` moves to its latest shot without
    /// a pulse.
    pub(crate) fn observe(&mut self, shots: &[ShotRow], filter: ShotFilter<'_>, now: f64) {
        if self.filter != filter.key() {
            self.sync(shots, filter);
            self.reveal = true;
            return;
        }
        let latest = latest_ref(shots, filter);
        if latest != self.latest {
            self.started = latest.is_some().then_some(now);
            self.latest = latest;
            self.follow();
        }
        self.drop_missing_pin(shots, filter);
    }

    /// Note the latest shot without pulsing or moving off a pinned shot
    /// that is still listed and passes `filter` (bulk loads, a new filter).
    pub(crate) fn sync(&mut self, shots: &[ShotRow], filter: ShotFilter<'_>) {
        self.filter = filter.key();
        self.latest = latest_ref(shots, filter);
        self.started = None;
        self.drop_missing_pin(shots, filter);
    }

    pub(super) fn drop_missing_pin(&mut self, shots: &[ShotRow], filter: ShotFilter<'_>) {
        if self
            .pinned
            .as_ref()
            .is_some_and(|pin| !shots.iter().any(|s| pin.matches(s) && filter.passes(s)))
        {
            self.pinned = None;
        }
    }

    /// Show `shots[i]`; picking the latest shot follows new shots again.
    pub(crate) fn select(&mut self, shots: &[ShotRow], filter: ShotFilter<'_>, i: usize) {
        if Some(i) == latest_index(shots, filter) {
            self.follow();
        } else {
            self.pinned = shots.get(i).map(ShotRef::of);
        }
    }

    /// Show the latest shot, and every new one as it arrives.
    pub(in crate::panels) fn follow(&mut self) {
        self.pinned = None;
        self.reveal = true;
    }

    /// Index of the shot on show.
    pub(in crate::panels) fn index(
        &self,
        shots: &[ShotRow],
        filter: ShotFilter<'_>,
    ) -> Option<usize> {
        focus_index(shots, self.pinned.as_ref(), filter)
    }

    /// Whether the view is following the latest shot.
    pub(in crate::panels) fn following(&self, shots: &[ShotRow], filter: ShotFilter<'_>) -> bool {
        self.index(shots, filter) == latest_index(shots, filter)
    }

    /// Scroll the shot on show into view in the list, once.
    pub(in crate::panels) fn reveal(&mut self) {
        self.reveal = true;
    }

    /// Whether the list should scroll the shot on show into view; true once
    /// per jump.
    pub(super) fn take_reveal(&mut self) -> bool {
        std::mem::take(&mut self.reveal)
    }

    /// How far the latest shot's pulse has run at `now`: 0 when the shot
    /// arrives, rising linearly to 1 over `PULSE_SECS`. `None` when no pulse
    /// is running: none started (no shot yet, or a bulk load) or it is over.
    pub(crate) fn progress(&self, now: f64) -> Option<f32> {
        let start = self.started?;
        let t = ((now - start) / PULSE_SECS).max(0.0);
        (t < 1.0).then_some(t as f32)
    }

    /// Pulse strength at `now`: 1 when the shot arrives, easing to 0.
    pub(super) fn strength(&self, now: f64) -> f32 {
        self.progress(now).map_or(0.0, |t| (1.0 - t).powi(3))
    }
}

/// The count line's total: the rows of `shots` the filter `filter` passes
/// (the viewed golfer's, and with a device filter that device's;
/// [`chart::ShotFilter`]).
pub(super) fn shot_total(shots: &[ShotRow], filter: ShotFilter<'_>) -> usize {
    shots.iter().filter(|r| filter.passes(r)).count()
}

/// Index of the latest shot the filter `filter` passes: the newest
/// one with any data, so a just-triggered shot does not blank the cards
/// while its data is in flight.
pub(in crate::panels) fn latest_index(shots: &[ShotRow], filter: ShotFilter<'_>) -> Option<usize> {
    shots.iter().rposition(|s| s.has_data() && filter.passes(s))
}

/// The chip on the shot header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HeaderChip {
    /// The latest shot is shown.
    Latest,
    /// An older shot is pinned.
    BackToLatest,
    None,
}

/// The chip for shot `i` in the header: Back to latest while not
/// `following`, else LATEST when `i` is the latest shot with data (a
/// trigger whose data has not landed does not take it).
/// What a click on the shot header asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::panels) struct HeaderClick {
    /// Back to latest.
    pub(in crate::panels) back: bool,
    /// Replay the shot's clips.
    pub(in crate::panels) replay: bool,
}

pub(super) fn header_chip(
    shots: &[ShotRow],
    i: usize,
    following: bool,
    filter: ShotFilter<'_>,
) -> HeaderChip {
    if !following {
        HeaderChip::BackToLatest
    } else if latest_index(shots, filter) == Some(i) {
        HeaderChip::Latest
    } else {
        HeaderChip::None
    }
}

pub(super) fn latest_ref(shots: &[ShotRow], filter: ShotFilter<'_>) -> Option<ShotRef> {
    latest_index(shots, filter).map(|i| ShotRef::of(&shots[i]))
}

/// A shot that is found again after the list is reloaded: by `shot_id` (and
/// the actor, when both sides name one) when both sides have an id, else by
/// actor and shot number.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct ShotRef {
    pub(super) actor: String,
    pub(super) number: u32,
    pub(super) id: String,
}

impl ShotRef {
    pub(super) fn of(shot: &ShotRow) -> Self {
        Self {
            actor: shot.actor.clone(),
            number: shot.shot_number,
            id: shot.shot_id.clone(),
        }
    }

    pub(super) fn matches(&self, shot: &ShotRow) -> bool {
        crate::groups::same_shot(
            (&self.actor, &self.id, self.number),
            (&shot.actor, &shot.shot_id, shot.shot_number),
        )
    }
}

/// Index of the shot to show: the `pinned` one while it is listed and the
/// filter `filter` passes it (the newest match), else the latest.
pub(super) fn focus_index(
    shots: &[ShotRow],
    pinned: Option<&ShotRef>,
    filter: ShotFilter<'_>,
) -> Option<usize> {
    pinned
        .and_then(|pin| shots.iter().rposition(|s| pin.matches(s)))
        .filter(|&i| filter.passes(&shots[i]))
        .or_else(|| latest_index(shots, filter))
}
