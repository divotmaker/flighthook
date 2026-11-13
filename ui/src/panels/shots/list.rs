//! The shot list on the left: its items and section headers in every grouping view,
//! the list entries and the foot (new set / new session strip).

use super::*;

/// Shot list width limits, and the range its starting width is picked from
/// (a share of the tab's width).
pub(super) const LIST_MIN_WIDTH: f32 = 200.0;
pub(super) const LIST_MAX_WIDTH: f32 = 320.0;
pub(crate) const LIST_DEFAULT_WIDTH: std::ops::RangeInclusive<f32> = 212.0..=248.0;
/// Share of the body the shot list takes on top in the portrait layout, at
/// first.
pub(super) const LIST_PORTRAIT_SHARE: f32 = 0.46;
/// Least height of the shot list, and of the data under it, in the
/// portrait layout.
pub(super) const LIST_PORTRAIT_MIN: f32 = 120.0;
/// Height of the list's header line.
pub(crate) const LIST_HEADER_HEIGHT: f32 = 30.0;
/// Height of the Shots tab's toolbar row and of the shots navigation's
/// header row beside it, so both center on one line.
pub(crate) const TOOLBAR_HEIGHT: f32 = 38.0;
/// Height of the foot's New set / New session line.
pub(super) const STRIP_BUTTONS_HEIGHT: f32 = 28.0;
/// Height of the foot's hint line.
pub(super) const STRIP_HINT_HEIGHT: f32 = 16.0;
/// Height of the hairline row at the top of the list's foot.
pub(super) const FOOT_RULE: f32 = 1.0;
/// Vertical spacing between the foot's lines.
pub(super) const FOOT_SPACING: f32 = 6.0;
/// Height of a group header in the list.
pub(super) const GROUP_HEADER_HEIGHT: f32 = 18.0;
/// Extra room above a group header that follows other lines, so it sits
/// with the run below it.
pub(super) const HEADER_LEAD: f32 = 8.0;
pub(super) const ENTRY_HEIGHT: f32 = 68.0;
pub(super) const ENTRY_GAP: f32 = 8.0;
pub(super) const ENTRY_RADIUS: u8 = 8;
pub(super) const ENTRY_PAD: f32 = 12.0;

/// The foot's buttons and hint with `club` in play: the hint speaks of
/// the golfer up, so a view on another golfer leaves it out.
pub(super) fn shown_strip(groups: &ShotGroups, club: Option<Club>, view_only: bool) -> Strip {
    let mut strip = groups.strip(club);
    if !groups.viewing_up() {
        strip.hint = None;
    }
    if view_only {
        strip.new_group = false;
    }
    strip
}

/// Height of the list's foot for `strip`: the hairline, the hint line when
/// there is one and the buttons, with the spacing between them. Zero when
/// the buttons are hidden (the view `none`), which leaves no foot.
pub(super) fn list_foot_height(strip: &Strip) -> f32 {
    if !strip.actions {
        return 0.0;
    }
    let hint = if strip.hint.is_some() {
        STRIP_HINT_HEIGHT + FOOT_SPACING
    } else {
        0.0
    };
    FOOT_RULE + FOOT_SPACING + hint + STRIP_BUTTONS_HEIGHT
}

/// One line of the shot list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum ListItem {
    /// `shots[i]`.
    Shot(usize),
    /// By set: header of the set with id `id`, over a run of its rows, or,
    /// with `next`, over nothing at the top of the list for the empty set
    /// the next shot joins.
    GroupHeader { id: String, next: bool },
    /// By club: header of the section of `club` in `session`, over its
    /// rows, or, with `next`, over nothing at the top of the list for the
    /// club whose first shot of the session is next.
    ClubHeader {
        /// The section's golfer key (the lookup rule).
        golfer: Option<String>,
        session: u32,
        club: Option<Club>,
        next: bool,
    },
    /// Header over a run of rows with data that belong to no known set,
    /// when a header is above them.
    Ungrouped,
}

/// A run of consecutive shot list rows, newest first, headed by its first
/// row with data.
pub(super) struct Run<'a> {
    /// The set of the run's rows: `None` for the rows without data that
    /// come before any row with data, `Some(None)` for rows in no known set.
    pub(super) group: Option<Option<&'a str>>,
    pub(super) rows: Vec<usize>,
}

/// The rows of `shots` the device filter `filter` passes, split into runs,
/// newest first. A run starts at each row with data whose set differs from
/// the run before; rows without data never start one.
pub(super) fn runs<'a>(
    shots: &'a [ShotRow],
    groups: &ShotGroups,
    filter: ShotFilter<'_>,
) -> Vec<Run<'a>> {
    let mut runs: Vec<Run<'a>> = Vec::new();
    for i in (0..shots.len()).rev() {
        let row = &shots[i];
        if !filter.passes(row) {
            continue;
        }
        if row.has_data() {
            let group = row.group.as_deref().filter(|id| groups.get(id).is_some());
            if runs.last().is_none_or(|r| r.group != Some(group)) {
                runs.push(Run {
                    group: Some(group),
                    rows: Vec::new(),
                });
            }
        } else if runs.is_empty() {
            runs.push(Run {
                group: None,
                rows: Vec::new(),
            });
        }
        if let Some(run) = runs.last_mut() {
            run.rows.push(i);
        }
    }
    runs
}

/// A By club section's key: the golfer key (the lookup rule), the
/// session and the club.
pub(super) type SectionKey = (Option<String>, u32, Option<Club>);

/// The shot list's lines, newest shot first, under the headers of the view
/// `groups.mode` with `club` in play, over the shots the device filter
/// `filter` passes: a set or club with none of them has no header.
///
/// - `none`: the last shot only, so no headers.
/// - By set: a header goes before each run of one set, and the empty set
///   the next shot joins gets a header at the very top.
/// - By club: one section per golfer, club and session, combining every
///   set with that golfer (the lookup rule), club and session. The section
///   of the golfer up and the club in play in the current session comes
///   first, then every other section by last
///   activity, newest first (see [`section_activity`]). A section's rows
///   are newest first, and rows in no known set gather in one last
///   section. While the open set is empty and the club in play has no
///   section in the current session, that club gets a header at the very
///   top.
///
/// Rows without data never break a run, and the rows before the first row
/// of a known set (a shot in flight, rows whose set is not known yet) come
/// first.
pub(super) fn list_items(
    shots: &[ShotRow],
    groups: &ShotGroups,
    club: Option<Club>,
    filter: ShotFilter<'_>,
) -> Vec<ListItem> {
    let view = groups.mode;
    if view == ShotGrouping::None {
        return (0..shots.len())
            .rev()
            .filter(|&i| filter.passes(&shots[i]))
            .map(ListItem::Shot)
            .collect();
    }
    let runs = runs(shots, groups, filter);
    let mut items = Vec::with_capacity(shots.len() + 4);
    let mut headed = false;
    let mut push_run = |items: &mut Vec<ListItem>, header: Option<ListItem>, rows: &[usize]| {
        match header {
            Some(h @ ListItem::Ungrouped) if headed => items.push(h),
            Some(ListItem::Ungrouped) => {}
            Some(h) => {
                items.push(h);
                headed = true;
            }
            None => {}
        }
        items.extend(rows.iter().copied().map(ListItem::Shot));
    };
    let set_header = |group: Option<Option<&str>>| match group {
        Some(Some(id)) => Some(ListItem::GroupHeader {
            id: id.to_owned(),
            next: false,
        }),
        Some(None) => Some(ListItem::Ungrouped),
        None => None,
    };
    // The viewed golfer's empty next set.
    let empty_open = groups.shown_current(club).filter(|g| g.shots == 0);

    if view == ShotGrouping::BySet {
        if let Some(next) = empty_open {
            let header = ListItem::GroupHeader {
                id: next.id.clone(),
                next: true,
            };
            push_run(&mut items, Some(header), &[]);
        }
        for run in &runs {
            push_run(&mut items, set_header(run.group), &run.rows);
        }
        return items;
    }

    let session = groups.session();
    let section_of = |id: &str| {
        groups.get(id).map(|g| {
            let golfer = groups.golfers().key(g.golfer.as_deref()).map(str::to_owned);
            (golfer, g.session, g.club)
        })
    };
    let head = runs
        .iter()
        .position(|r| matches!(r.group, Some(Some(_))))
        .unwrap_or(runs.len());
    let mut sections: Vec<(SectionKey, Vec<usize>)> = Vec::new();
    let mut loose = Vec::new();
    for run in &runs[head..] {
        match run.group.flatten().and_then(section_of) {
            // A putting device's putt counts under the putter.
            Some((golfer, session, club)) => {
                for &i in &run.rows {
                    let key = (golfer.clone(), session, chart::row_club(&shots[i], club));
                    match sections.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, rows)) => rows.push(i),
                        None => sections.push((key, vec![i])),
                    }
                }
            }
            None => loose.extend(&run.rows),
        }
    }
    // The section of the club in play leads only while the view is the
    // golfer up: another golfer's pick never reorders the viewed golfer's
    // list. For anyone else the empty next set keeps its own club.
    let up = groups.viewing_up();
    let next_club = if up {
        club
    } else {
        empty_open.and_then(|g| g.club)
    };
    let pinned: SectionKey = (groups.view_key().map(str::to_owned), session, next_club);
    sections.sort_by_cached_key(|(key, rows)| {
        let number = groups
            .iter_sets()
            .filter(|g| {
                (g.session, g.club) == (key.1, key.2)
                    && groups.golfers().key(g.golfer.as_deref()) == key.0.as_deref()
            })
            .map(|g| g.number)
            .max()
            .unwrap_or(0);
        let activity = section_activity(groups, key, rows, shots);
        std::cmp::Reverse((up && *key == pinned, activity, number))
    });
    if empty_open.is_some() && !sections.iter().any(|(k, _)| *k == pinned) {
        let header = ListItem::ClubHeader {
            golfer: pinned.0.clone(),
            session,
            club: next_club,
            next: true,
        };
        push_run(&mut items, Some(header), &[]);
    }
    for run in &runs[..head] {
        push_run(&mut items, set_header(run.group), &run.rows);
    }
    for ((golfer, session, club), rows) in &sections {
        let header = ListItem::ClubHeader {
            golfer: golfer.clone(),
            session: *session,
            club: *club,
            next: false,
        };
        push_run(&mut items, Some(header), rows);
    }
    if !loose.is_empty() {
        push_run(&mut items, Some(ListItem::Ungrouped), &loose);
    }
    items
}

/// When the By club section of a golfer's `club` in `session` was last
/// active: its newest row (`rows` is newest first and starts with a row
/// with data), or, in the current session, that golfer's last selection
/// of its club, whichever is later. Another golfer's pick of the club does
/// not count.
pub(super) fn section_activity(
    groups: &ShotGroups,
    (golfer, session, club): &SectionKey,
    rows: &[usize],
    shots: &[ShotRow],
) -> Activity {
    let shot = rows.first().map_or((0, 0), |&i| (2 * i + 2, 0));
    let selected = (*session == groups.session())
        .then(|| groups.selected_at(golfer.as_deref(), *club, shots))
        .flatten();
    selected.map_or(shot, |sel| shot.max(sel))
}

/// A By set header's title, e.g. "Set 3 · 7I · 10 shots". `next` marks
/// the empty set the next shot joins: "Set 4 · DR · Next shot".
pub(super) fn set_title(group: &SetInfo, next: bool) -> String {
    let tail = match (next, group.shots) {
        (true, _) => "Next shot".to_owned(),
        (false, n) => shot_count(n as usize),
    };
    let club = group
        .club
        .map_or_else(|| "No club".to_owned(), |c| c.to_string());
    format!("Set {} \u{b7} {club} \u{b7} {tail}", group.number)
}

/// A By club header's title, e.g. "7 Iron · 2 sets · 14 shots", with
/// "Earlier" after the club for a section from an earlier session.
/// `tally` is `(sets, shots)`; the set count shows only when it is more
/// than one. `None` marks the club whose first shot of the session is
/// next: "7 Iron · Next shot".
pub(super) fn club_title(
    club: Option<Club>,
    earlier: bool,
    tally: Option<(usize, usize)>,
) -> String {
    let mut parts = vec![club.map_or("No club", club_name).to_owned()];
    if earlier {
        parts.push("Earlier".to_owned());
    }
    match tally {
        None => parts.push("Next shot".to_owned()),
        Some((sets, shots)) => {
            if sets > 1 {
                parts.push(format!("{sets} sets"));
            }
            parts.push(shot_count(shots));
        }
    }
    parts.join(" \u{b7} ")
}

/// A By club list header's title: [`club_title`], with an earlier
/// session's set count left to the hover text so the title stays short
/// enough for the list.
pub(super) fn club_header_title(
    club: Option<Club>,
    earlier: bool,
    tally: Option<(usize, usize)>,
) -> String {
    let tally = tally.map(|(sets, shots)| (if earlier { 1 } else { sets }, shots));
    club_title(club, earlier, tally)
}

/// Local `HH:MM` of an RFC 3339 timestamp.
pub(super) fn clock_time(ms: u64) -> Option<String> {
    let t = chrono::DateTime::from_timestamp_millis(i64::try_from(ms).ok()?)?;
    Some(t.with_timezone(&chrono::Local).format("%H:%M").to_string())
}

/// Hover text for New set while it is off: `view_only` while a view-only
/// row of the Golfer card is viewed.
pub(super) fn new_group_off_hint(view_only: bool) -> &'static str {
    if view_only {
        "New set applies to the golfer up."
    } else {
        "The open set has no shots yet."
    }
}

/// Hover text for New set in `view`.
pub(super) fn new_group_hint(view: ShotGrouping) -> &'static str {
    if view == ShotGrouping::ByClub {
        "Start a fresh set with the next shot. By club keeps combining a club's \
         sets until New session."
    } else {
        "Close this set; the next shot starts a fresh one."
    }
}

/// Hover text for New session.
pub(super) const NEW_SESSION_HINT: &str = "Close the open set and start a new session: the next shot starts a new set, \
     and By club starts every club afresh.";

/// A list header's title in caps, laid out to fit `width`: whole when it
/// fits, else the part before the last ` · ` cut and the last part (the
/// shot count) kept whole after it.
pub(super) fn header_text(
    painter: &egui::Painter,
    title: &str,
    width: f32,
    color: egui::Color32,
) -> (
    std::sync::Arc<egui::Galley>,
    Option<std::sync::Arc<egui::Galley>>,
) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};

    let layout = |text: String, max: Option<f32>| {
        let mut job = LayoutJob::single_section(
            text,
            TextFormat {
                font_id: egui::FontId::proportional(12.0),
                color,
                extra_letter_spacing: 1.0,
                ..Default::default()
            },
        );
        if let Some(max) = max {
            job.wrap = TextWrapping::truncate_at_width(max.max(0.0));
        }
        painter.layout_job(job)
    };
    let caps = title.to_uppercase();
    let whole = layout(caps.clone(), None);
    if whole.size().x <= width {
        return (whole, None);
    }
    match caps.rsplit_once(" \u{b7} ") {
        Some((head, last)) => {
            let tail = layout(format!(" \u{b7} {last}"), None);
            let head = layout(head.to_owned(), Some(width - tail.size().x));
            (head, Some(tail))
        }
        None => (layout(caps, Some(width)), None),
    }
}

/// Paint a list header in `rect`: a 3 px bar and the caps `title` in
/// `color`, then a hairline out to `time`, right-aligned in dim text. The
/// time, then the hairline, are left out when the title leaves no room.
pub(super) fn list_header(
    painter: &egui::Painter,
    rect: egui::Rect,
    title: &str,
    time: Option<&str>,
    color: egui::Color32,
) {
    use egui::{FontId, pos2, vec2};

    let mid = rect.center().y;
    let bar = egui::Rect::from_center_size(pos2(rect.left() + 1.5, mid), vec2(3.0, 14.0));
    painter.rect_filled(bar, egui::CornerRadius::same(2), color);
    let left = bar.right() + 7.0;
    let (head, tail) = header_text(painter, title, (rect.right() - left).max(0.0), color);
    let mut text_right = left + head.size().x;
    painter.galley(pos2(left, mid - head.size().y / 2.0), head, color);
    if let Some(tail) = tail {
        painter.galley(
            pos2(text_right, mid - tail.size().y / 2.0),
            tail.clone(),
            color,
        );
        text_right += tail.size().x;
    }

    let mut line_end = rect.right();
    if let Some(time) = time {
        let time =
            painter.layout_no_wrap(time.to_owned(), FontId::proportional(11.0), theme::TEXT_DIM);
        let x = rect.right() - time.size().x;
        // Shown only with room for a hairline before it too.
        if x - text_right >= 32.0 {
            line_end = x - 8.0;
            painter.galley(pos2(x, mid - time.size().y / 2.0), time, theme::TEXT_DIM);
        }
    }
    let line_start = text_right + 8.0;
    if line_end - line_start >= 8.0 {
        painter.hline(
            line_start..=line_end,
            mid,
            egui::Stroke::new(1.0, theme::BORDER),
        );
    }
}

/// What a shot list entry shows.
pub(super) struct ListEntry {
    pub(super) number: u32,
    pub(super) device: Option<String>,
    pub(super) carry: Metric,
    pub(super) total: Metric,
    /// The shot has ball or club data.
    pub(super) has_data: bool,
    /// The shot finished without ball or club data: the device did not read
    /// it.
    pub(super) no_read: bool,
}

impl ListEntry {
    /// `shot` already converted to the display unit system.
    pub(super) fn new(shot: &ShotRow, device: Option<&str>, imperial: bool) -> Self {
        let dist = if imperial { "yd" } else { "m" };
        let no_derived = DerivedFlight::default();
        let no_flight = FlightResult::new(Basis::Modeled);
        let f: &FlightResult = shot.flight.as_ref().unwrap_or(&no_flight);
        let d: &DerivedFlight = shot.derived.as_ref().unwrap_or(&no_derived);
        let v = |x: Option<Distance>| x.map(|x| x.value());
        Self {
            number: shot.shot_number,
            device: device.map(str::to_string),
            carry: Metric::new(
                "Carry",
                dist,
                pick(v(f.carry_distance), v(d.carry_distance)),
                1,
                false,
            ),
            total: Metric::new(
                "Total",
                dist,
                pick(v(f.total_distance), v(d.total_distance)),
                1,
                false,
            ),
            has_data: shot.has_data(),
            no_read: shot.finished && !shot.has_data(),
        }
    }
}

/// How a list entry is drawn.
#[derive(Clone, Copy, Default)]
pub(crate) struct EntryState {
    pub(crate) selected: bool,
    pub(crate) hovered: bool,
    pub(crate) latest: bool,
    /// New-shot glow, 0..=1.
    pub(crate) pulse: f32,
}

/// A shot entry's frame at `rect`: its fill, sheen and border, the glow of
/// a pulse, and the selected entry's stripe.
pub(crate) fn entry_frame(painter: &egui::Painter, rect: egui::Rect, state: EntryState) {
    use egui::{Stroke, StrokeKind, vec2};

    let radius = egui::CornerRadius::same(ENTRY_RADIUS);
    let pulse = state.pulse;
    if pulse > 0.0 {
        glow(painter, rect, ENTRY_RADIUS, pulse);
    }
    let base = if state.selected {
        0.8
    } else if state.hovered {
        0.55
    } else {
        0.2
    };
    let fill = theme::mix(
        theme::SURFACE,
        theme::SURFACE_RAISED,
        base + (1.0 - base) * pulse,
    );
    painter.rect_filled(rect, radius, fill);
    sheen(painter, rect, radius);
    let stroke = if state.selected {
        Stroke::new(1.0 + pulse, theme::ACCENT)
    } else if state.hovered {
        Stroke::new(1.0, theme::mix(theme::BORDER, theme::TEXT_MUTED, 0.35))
    } else {
        Stroke::new(1.0, theme::mix(theme::BORDER, theme::ACCENT, pulse))
    };
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    if state.selected {
        let stripe = egui::Rect::from_min_size(rect.min, vec2(4.0, rect.height()));
        let corners = egui::CornerRadius {
            ne: 0,
            se: 0,
            ..radius
        };
        painter.rect_filled(stripe, corners, theme::ACCENT);
    }
}

/// Paint one shot list entry at `rect`: a mini card with the shot number
/// and device on top, then carry and total.
pub(super) fn list_entry(ui: &egui::Ui, rect: egui::Rect, e: &ListEntry, state: EntryState) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    use egui::{FontId, pos2};

    let painter = ui.painter();
    entry_frame(painter, rect, state);

    let left = rect.left() + ENTRY_PAD + 2.0;
    let right = rect.right() - ENTRY_PAD;
    let number_color = match (e.has_data, state.latest) {
        (false, _) => theme::TEXT_DIM,
        (true, true) => theme::GOLD,
        (true, false) => theme::TEXT,
    };
    let number = painter.layout_no_wrap(
        format!("#{}", e.number),
        FontId::proportional(17.0),
        number_color,
    );
    let top = rect.top() + 8.0;
    let number_width = number.size().x;
    let number_height = number.size().y;
    // Drawn twice, half a pixel apart, for a heavier numeral.
    painter.galley(pos2(left, top), number.clone(), number_color);
    painter.galley(pos2(left + 0.5, top), number, number_color);

    if let Some(device) = &e.device {
        let room = right - left - number_width - 10.0;
        let mut job = LayoutJob::single_section(
            device.clone(),
            TextFormat {
                font_id: FontId::proportional(11.5),
                color: theme::TEXT_MUTED,
                ..Default::default()
            },
        );
        job.wrap = TextWrapping::truncate_at_width(room.max(0.0));
        let galley = painter.layout_job(job);
        let pos = pos2(
            right - galley.size().x,
            top + (number_height - galley.size().y) / 2.0 + 1.0,
        );
        painter.galley(pos, galley, theme::TEXT_MUTED);
    }

    let label_y = top + number_height + 3.0;
    if e.no_read {
        let text = painter.layout_no_wrap(
            "No read".to_owned(),
            FontId::proportional(11.5),
            theme::TEXT_DIM,
        );
        painter.galley(pos2(left, label_y), text, theme::TEXT_DIM);
        return;
    }
    let half = (right - left) / 2.0;
    for (k, m) in [&e.carry, &e.total].into_iter().enumerate() {
        let x = left + half * k as f32;
        let label = painter.layout_job(LayoutJob::single_section(
            m.label.to_uppercase(),
            TextFormat {
                font_id: FontId::proportional(9.5),
                color: theme::TEXT_DIM,
                extra_letter_spacing: 0.8,
                ..Default::default()
            },
        ));
        let label_height = label.size().y;
        painter.galley(pos2(x, label_y), label, theme::TEXT_DIM);
        let reading = Reading::new(painter, m, &m.text, None, 16.0, false);
        let pos = pos2(x, label_y + label_height);
        painter.galley(pos, reading.value.clone(), reading.color);
        if let Some((unit, offset)) = reading.unit {
            painter.galley(pos + offset, unit, theme::TEXT_MUTED);
        }
    }
}
