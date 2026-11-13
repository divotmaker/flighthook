//! Shared settings card widgets: field labels, hints and errors, type tags, card headers with
//! the on/off switch, Save/Cancel, lock hint, and action buttons.

use super::*;

/// Width of the label column in a settings card.
pub(super) const LABEL_WIDTH: f32 = 140.0;
/// Width of the label column in the portrait layout.
pub(super) const NARROW_LABEL_WIDTH: f32 = 96.0;

/// Where the context keeps whether Settings lays out its portrait variant.
fn narrow_id() -> egui::Id {
    egui::Id::new("settings_narrow")
}

/// Settings follows the portrait layout (`portrait`) this frame: the label
/// column narrows and a device page stacks its telemetry over its settings.
/// Set before any page.
pub(crate) fn set_narrow(ctx: &egui::Context, portrait: bool) {
    ctx.data_mut(|d| d.insert_temp(narrow_id(), portrait));
}

/// Whether Settings' cards follow the portrait layout ([`set_narrow`]).
pub(crate) fn narrow(ui: &egui::Ui) -> bool {
    ui.data(|d| d.get_temp(narrow_id()).unwrap_or(false))
}

/// The label column's width: [`NARROW_LABEL_WIDTH`] in the portrait
/// layout, else [`LABEL_WIDTH`], or in [`form_rows`] a share of the row.
pub(super) fn label_width(ui: &egui::Ui) -> f32 {
    if narrow(ui) {
        NARROW_LABEL_WIDTH
    } else if rows(ui) {
        (ui.max_rect().width() * ROWS_LABEL_SHARE).max(LABEL_WIDTH)
    } else {
        LABEL_WIDTH
    }
}

/// Widest a form of [`form_rows`] gets.
const ROWS_MAX_WIDTH: f32 = 900.0;
/// The label column's share of a row of [`form_rows`].
const ROWS_LABEL_SHARE: f32 = 0.45;
/// Space between two rows of [`form_rows`], where their rule runs.
const ROWS_GAP: f32 = 12.0;

/// Space added between Save and Cancel in the portrait layout, on top of
/// the item spacing.
const TOUCH_GAP: f32 = 8.0;

/// Where the context keeps whether fields are drawn as [`form_rows`].
fn rows_id() -> egui::Id {
    egui::Id::new("settings_rows")
}

/// Whether fields are drawn as [`form_rows`].
fn rows(ui: &egui::Ui) -> bool {
    ui.data(|d| d.get_temp(rows_id()).unwrap_or(false))
}

/// A plain form drawn as grouped rows, like a system settings page: one
/// row per setting across the form (at most [`ROWS_MAX_WIDTH`]), its label
/// on the left, its control on the right, a rule between rows.
pub(crate) fn form_rows<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.data_mut(|d| d.insert_temp(rows_id(), true));
    let width = ui.available_width().min(ROWS_MAX_WIDTH);
    let inner = ui
        .vertical(|ui| {
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing.y = ROWS_GAP;
            add(ui)
        })
        .inner;
    ui.data_mut(|d| d.remove::<bool>(rows_id()));
    inner
}

/// In [`form_rows`], the rule above the row `ui` lays out.
fn row_rule(ui: &egui::Ui) {
    if !rows(ui) {
        return;
    }
    let rect = ui.max_rect();
    let y = rect.top() - ROWS_GAP / 2.0;
    ui.painter().hline(
        rect.left()..=rect.right(),
        y,
        egui::Stroke::new(1.0, theme::BORDER),
    );
}
/// Width of a unit dropdown beside a field.
pub(super) const UNIT_WIDTH: f32 = 50.0;
/// A text field's inner margin, both sides: its width beyond the
/// `desired_width` of its text.
pub(super) const TEXT_EDIT_MARGIN: f32 = 8.0;

/// A field label in the card's label column. Hover text goes on the result.
pub(super) fn field_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    row_rule(ui);
    let height = ui.spacing().interact_size.y;
    let width = label_width(ui);
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.label(text)
        },
    )
    .inner
}

/// Skip the label column, so the row's control lines up with the fields.
pub(super) fn field_indent(ui: &mut egui::Ui) {
    row_rule(ui);
    ui.add_space(label_width(ui) + ui.spacing().item_spacing.x);
}

/// The label column left empty on a row that continues the one above it:
/// no rule between them.
pub(super) fn field_continue(ui: &mut egui::Ui) {
    ui.add_space(label_width(ui) + ui.spacing().item_spacing.x);
}

/// A validation message on its own row under a field, lined up with it.
pub(super) fn field_error(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(label_width(ui) + ui.spacing().item_spacing.x);
        ui.label(egui::RichText::new(text).color(theme::DANGER).size(11.0));
    });
}

/// A muted hint on its own row under a field, lined up with it, wrapped to
/// the card.
pub(super) fn field_hint(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(label_width(ui) + ui.spacing().item_spacing.x);
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            )
            .wrap(),
        );
    });
}

/// Widths of a card's fields: a text field or dropdown with room for a unit
/// dropdown after it, and a dropdown spanning the unit column too.
pub(super) fn field_widths(ui: &egui::Ui) -> (f32, f32) {
    let gap = ui.spacing().item_spacing.x;
    let field = (ui.available_width() - label_width(ui) - UNIT_WIDTH - 2.0 * gap).max(80.0);
    (field, field + gap + UNIT_WIDTH)
}

/// Horizontal padding of a type tag, each side.
pub(super) const TAG_PAD: f32 = 6.0;
/// Narrowest a card's title gets before its type tag shrinks.
pub(super) const TITLE_MIN_WIDTH: f32 = 40.0;
/// Narrowest text a type tag keeps; with less room it is left out.
pub(super) const TAG_MIN_TEXT: f32 = 24.0;

/// Width of a type tag showing all of `text`.
pub(super) fn tag_width(ui: &egui::Ui, text: &str) -> f32 {
    tag_size(ui, text).x
}

/// Size of a type tag showing all of `text`.
fn tag_size(ui: &egui::Ui, text: &str) -> egui::Vec2 {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(11.0),
        theme::HEADING,
    );
    galley.size() + egui::vec2(2.0 * TAG_PAD, 4.0)
}

/// A card header's tag, at most `max_width` wide: its text is
/// cut short with "…" when it doesn't fit.
pub(super) fn type_tag(
    ui: &mut egui::Ui,
    text: &str,
    hover: Option<&str>,
    max_width: f32,
) -> egui::Response {
    let tag = egui::Frame::new()
        .fill(theme::SURFACE_HOVER)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(TAG_PAD as i8, 2))
        .show(ui, |ui| {
            ui.set_max_width(max_width - 2.0 * TAG_PAD);
            ui.add(
                egui::Label::new(egui::RichText::new(text).size(11.0).color(theme::HEADING))
                    .truncate(),
            )
        });
    let response = tag.response;
    match hover {
        Some(hover) => response.on_hover_text(hover),
        None => response,
    }
}

/// Where a card header's tag sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TagPlace {
    /// Right after the title.
    Title,
    /// Right-aligned, just left of the buttons.
    Buttons,
}

/// A card's header row: the title on the left, `buttons` on the right, and
/// an optional tag at `place`. `buttons` runs in a right-to-left layout: the
/// first button added sits furthest right. The buttons keep their size;
/// the title is cut short first, down to [`TITLE_MIN_WIDTH`], then the
/// tag, which is left out once it would be cut under [`TAG_MIN_TEXT`].
/// `key` names the card for the tests.
pub(crate) fn card_header<R>(
    ui: &mut egui::Ui,
    key: &str,
    title: &str,
    tag: Option<(&str, TagPlace)>,
    hover: Option<&str>,
    buttons: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    card_header_with(ui, key, title, tag, hover, None, buttons).0
}

/// The on/off switch of a card header: whether the card is on, and whether
/// the switch takes input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeaderSwitch {
    pub(crate) on: bool,
    pub(crate) enabled: bool,
    /// Why the card cannot be turned on: an enabled card of its type has its
    /// address. Shown on hover while the switch is locked.
    pub(crate) clash: Option<String>,
}

/// [`card_header`] with an optional on/off `switch` right after the title,
/// which keeps its size like the buttons (hover "Turn off" / "Turn on",
/// mark `enabled:<key>`). Returns what `buttons` returned and whether the
/// switch was flipped. On the phone's open page ([`set_page_back`]) the
/// header leads with the back icon, and the tag and `buttons` take a second
/// row unless they fit on the first ([`phone_page_header`]).
pub(crate) fn card_header_with<R>(
    ui: &mut egui::Ui,
    key: &str,
    title: &str,
    tag: Option<(&str, TagPlace)>,
    hover: Option<&str>,
    switch: Option<HeaderSwitch>,
    buttons: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, bool) {
    if narrow(ui)
        && let Some(locked) = page_back(ui)
    {
        return phone_page_header(ui, key, title, tag, hover, switch.as_ref(), locked, buttons);
    }
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let result = buttons(ui);
            let gap = ui.spacing().item_spacing.x;
            let switch_room = switch
                .as_ref()
                .map_or(0.0, |_| theme::ON_OFF_SWITCH.x + gap);
            let room = ui.available_width() - switch_room;
            let tag = tag.and_then(|(t, place)| {
                let full = tag_width(ui, t);
                let width = full.min(room - TITLE_MIN_WIDTH - gap);
                (width >= full.min(TAG_MIN_TEXT + 2.0 * TAG_PAD)).then_some((t, place, width))
            });
            let mut tag_after_title = None;
            match tag {
                Some((text, TagPlace::Buttons, width)) => {
                    let tag = type_tag(ui, text, hover, width);
                    devices::mark(ui, || format!("tag:{key}"), &tag);
                }
                Some((text, TagPlace::Title, width)) => tag_after_title = Some((text, width)),
                None => {}
            }
            let flipped = ui
                .with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let title_width = ui.available_width()
                        - switch_room
                        - tag_after_title.map_or(0.0, |(_, width)| width + gap);
                    header_title(ui, key, title, hover, title_width);
                    let flipped = header_switch(ui, key, switch.as_ref());
                    if let Some((text, width)) = tag_after_title {
                        let tag = type_tag(ui, text, hover, width);
                        devices::mark(ui, || format!("tag:{key}"), &tag);
                    }
                    flipped
                })
                .inner;
            (result, flipped)
        })
        .inner
    })
    .inner
}

/// [`card_header_with`] on the phone's open page: the back icon (locked
/// while `locked`), the title, the switch and a [`TagPlace::Title`] tag
/// lead; a [`TagPlace::Buttons`] tag and `buttons` trail, right-aligned.
/// The trail joins the first row when it fits there beside the full title
/// (always when it is empty), else it takes a second row, wrapping if it
/// doesn't fit. The fit uses the trail's width from the frame before.
#[allow(clippy::too_many_arguments)]
fn phone_page_header<R>(
    ui: &mut egui::Ui,
    key: &str,
    title: &str,
    tag: Option<(&str, TagPlace)>,
    hover: Option<&str>,
    switch: Option<&HeaderSwitch>,
    locked: bool,
    buttons: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, bool) {
    let gap = ui.spacing().item_spacing.x;
    let switch_room = switch.map_or(0.0, |_| theme::ON_OFF_SWITCH.x + gap);
    let after_title = tag
        .filter(|(_, place)| *place == TagPlace::Title)
        .map(|(t, _)| (t, tag_width(ui, t)));
    let trail_id = egui::Id::new(("settings_page_trail", key));
    let trail_width = ui.data(|d| d.get_temp::<f32>(trail_id));
    let title_full = ui
        .painter()
        .layout_no_wrap(
            title.to_owned(),
            egui::FontId::proportional(16.0),
            theme::HEADING,
        )
        .size()
        .x;
    let lead_width = crate::topbar::TOUCH_TARGET
        + gap
        + title_full
        + gap
        + switch_room
        + after_title.map_or(0.0, |(_, width)| width + gap);
    let one_row =
        trail_width.is_some_and(|w| w <= 0.0 || lead_width + gap + w <= ui.available_width());
    let lead = |ui: &mut egui::Ui| {
        page_back_icon(ui, locked);
        let title_width =
            ui.available_width() - switch_room - after_title.map_or(0.0, |(_, width)| width + gap);
        header_title(ui, key, title, hover, title_width);
        let flipped = header_switch(ui, key, switch);
        if let Some((text, width)) = after_title {
            let tag = type_tag(ui, text, hover, width);
            devices::mark(ui, || format!("tag:{key}"), &tag);
        }
        flipped
    };
    let trail = |ui: &mut egui::Ui| {
        let result = buttons(ui);
        if let Some((text, TagPlace::Buttons)) = tag {
            let size = tag_size(ui, text);
            let width = size.x.min(ui.available_width());
            if width >= size.x.min(TAG_MIN_TEXT + 2.0 * TAG_PAD) {
                // As tall as the tag, so it doesn't stretch to the
                // touch-sized icons beside it.
                let tag = ui
                    .allocate_ui_with_layout(
                        egui::vec2(width, size.y),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| type_tag(ui, text, hover, width),
                    )
                    .inner;
                devices::mark(ui, || format!("tag:{key}"), &tag);
            }
        }
        let width = ui.min_rect().width();
        if trail_width.is_none_or(|w| (w - width).abs() > 0.5) {
            ui.data_mut(|d| d.insert_temp(trail_id, width));
            ui.ctx().request_discard("phone page header");
        }
        result
    };
    if one_row {
        return ui
            .horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let result = trail(ui);
                    let flipped = ui
                        .with_layout(egui::Layout::left_to_right(egui::Align::Center), lead)
                        .inner;
                    (result, flipped)
                })
                .inner
            })
            .inner;
    }
    let flipped = ui.horizontal(lead).inner;
    let result = ui
        .horizontal(|ui| {
            let layout = egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(true);
            ui.with_layout(layout, trail).inner
        })
        .inner;
    (result, flipped)
}

/// A card header's title, at most `max_width` wide (never under
/// [`TITLE_MIN_WIDTH`]), cut short with "…" when it doesn't fit; `hover`
/// on hover. Marked `title:<key>`.
fn header_title(ui: &mut egui::Ui, key: &str, title: &str, hover: Option<&str>, max_width: f32) {
    ui.scope(|ui| {
        ui.set_max_width(max_width.max(TITLE_MIN_WIDTH));
        let title = ui.add(
            egui::Label::new(
                egui::RichText::new(title)
                    .strong()
                    .size(16.0)
                    .color(theme::HEADING),
            )
            .truncate(),
        );
        devices::mark(ui, || format!("title:{key}"), &title);
        if let Some(hover) = hover {
            title.on_hover_text(hover);
        }
    });
}

/// A card header's on/off switch, if it has one. Returns whether it was
/// flipped.
fn header_switch(ui: &mut egui::Ui, key: &str, switch: Option<&HeaderSwitch>) -> bool {
    switch.is_some_and(|switch| {
        let response = ui
            .add_enabled_ui(switch.enabled, |ui| theme::on_off_switch(ui, switch.on))
            .inner
            .on_hover_text(if switch.on { "Turn off" } else { "Turn on" });
        let response = match &switch.clash {
            Some(clash) => response.on_disabled_hover_text(clash),
            None => response,
        };
        devices::mark(ui, || format!("enabled:{key}"), &response);
        response.clicked()
    })
}

/// Where the context keeps the phone's back icon: whether it shows this
/// frame and is locked.
fn page_back_id() -> egui::Id {
    egui::Id::new("settings_page_back")
}

/// Where the context keeps whether the back icon was clicked this frame.
fn back_clicked_id() -> egui::Id {
    egui::Id::new("settings_page_back_clicked")
}

/// The phone shows a row's page this frame (`back` is `Some`): its header
/// leads with the back icon, locked while `back` is `Some(true)`. Set
/// before any page; [`take_back`] tells whether it was clicked.
pub(crate) fn set_page_back(ctx: &egui::Context, back: Option<bool>) {
    ctx.data_mut(|d| {
        d.insert_temp(page_back_id(), back);
        d.remove::<bool>(back_clicked_id());
    });
}

/// Whether the page header shows the back icon ([`set_page_back`]), and
/// whether it is locked.
fn page_back(ui: &egui::Ui) -> Option<bool> {
    ui.data(|d| d.get_temp::<Option<bool>>(page_back_id()).flatten())
}

/// Whether the page header's back icon was clicked this frame.
pub(crate) fn take_back(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(back_clicked_id()).unwrap_or(false))
}

/// The phone page's back icon, a touch target, marked `settings_back`;
/// locked while `locked`, with the reason as its hover and touch tip.
fn page_back_icon(ui: &mut egui::Ui, locked: bool) {
    let back = ui
        .add_enabled_ui(!locked, |ui| {
            theme::icon(
                ui,
                theme::Icon::ChevronLeft,
                crate::topbar::TOUCH_TARGET,
                theme::ACCENT,
                !locked,
            )
        })
        .inner;
    devices::mark(ui, || "settings_back".to_owned(), &back);
    let reason = crate::topbar::LOCKED_REASON;
    let back = if locked {
        crate::topbar::touch_tip(back.on_disabled_hover_text(reason), reason.into())
    } else {
        back.on_hover_text("Settings")
    };
    if back.clicked() {
        ui.data_mut(|d| d.insert_temp(back_clicked_id(), true));
    }
}

/// A small header button: accent-filled while `primary`.
pub(crate) fn header_button(text: &str, primary: bool) -> egui::Button<'static> {
    if primary {
        theme::primary_button(text, 11.0)
    } else {
        egui::Button::new(egui::RichText::new(text.to_string()).size(11.0))
    }
}

/// What a card's Save or Cancel asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    Save,
    Cancel,
}

/// A dirty card's Save and Cancel, in a right-to-left header: Save (the
/// primary) furthest right. A spinner sits beside them while a save is out.
/// In the portrait layout both are touch targets with a wider gap between.
/// `key` names the card for the tests.
pub(crate) fn save_cancel(
    ui: &mut egui::Ui,
    key: &str,
    can_save: bool,
    saving: bool,
) -> Option<Pending> {
    let touch = narrow(ui);
    let button = |text, primary| {
        let button = header_button(text, primary);
        if touch {
            button.min_size(egui::vec2(
                crate::topbar::TOUCH_TARGET,
                crate::topbar::TOUCH_TARGET,
            ))
        } else {
            button
        }
    };
    let mut pending = None;
    let save = ui.add_enabled(can_save && !saving, button("Save", true));
    devices::mark(ui, || format!("save:{key}"), &save);
    if save.clicked() {
        pending = Some(Pending::Save);
    }
    if touch {
        ui.add_space(TOUCH_GAP);
    }
    let cancel = ui.add_enabled(!saving, button("Cancel", false));
    devices::mark(ui, || format!("cancel:{key}"), &cancel);
    if cancel.clicked() {
        pending = Some(Pending::Cancel);
    }
    if saving {
        ui.spinner();
    }
    pending
}

/// The hint under a dirty card's header while the rest of the tab waits for
/// it.
pub(crate) fn lock_hint(ui: &mut egui::Ui) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.label(
            egui::RichText::new("Save or cancel your changes")
                .size(11.0)
                .color(theme::TEXT_MUTED),
        );
    });
}

/// A failed save's reason, under the card's header. `key` names the card
/// for the tests.
pub(crate) fn save_error_line(ui: &mut egui::Ui, key: &str, message: &str) {
    let line = ui.label(
        egui::RichText::new(format!("Save failed: {message}"))
            .size(11.0)
            .color(theme::DANGER),
    );
    devices::mark(ui, || format!("error:{key}"), &line);
}

/// Hover text of the Simulation card.
pub(super) const SIMULATION_HOVER: &str = "When a launch monitor doesn't report carry, apex, flight time, descent, offline, roll or total, flighthook simulates them from the measured launch. Simulated values are tagged SIM on the Cards view and shown in italics in the Table. Never sent to GSPro, which simulates the flight itself.";

/// Hover text of the Auto-Chip Helper setting on the Global card.
pub(super) const AUTO_CHIP_SETTING_HOVER: &str = "Show the Auto-Chip control in the header: it sends a short straight chip to the sim, for chips your launch monitor can't read (radar monitors often miss short chips). The chip is simulated under the altitude, temperature and surface below, whatever the Simulation mode.";

/// Hover text of the Replay card.
pub(super) const REPLAY_HOVER: &str = "The cameras record a clip around every shot: from the pre-roll before the estimated impact to the post-roll after the launch monitor reports the shot. The newest clips are kept on disk; older ones are deleted, and Clear history deletes them all.";
