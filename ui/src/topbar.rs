//! The top bar: nav icons on the left (Shots, Cameras, Settings, Log), a
//! title in the center while it fits, and on the right, by role: status and
//! mode (energy, Follow turns), actions (Auto-Chip, Shot Type)
//! and selections (Club, then Golfer at the far right edge). Every button is
//! 40 px with a 6 px gap and no dividers; the groups are only an order.
//! The dropdowns open under their button. In the portrait layout the nav
//! is a tab bar at the bottom of the window instead.

use egui::{Color32, Pos2, Rect, Sense, pos2, vec2};
use flighthook::{Club, Mode};

use crate::app::{FlighthookApp, golfer_name, offered_clubs};
use crate::battery::{self, Battery};
use crate::groups::club_name;
use crate::icons::{self, Asset};
use crate::net;
use crate::panels::auto_chip::{AUTO_CHIP_HINT, auto_chip_blocked, auto_chip_items};
use crate::panels::cameras::ShotsPane;
use crate::panels::devices::mark;
use crate::panels::{Tab, settings_alert};
use crate::theme;

/// Height of the bar.
pub(crate) const BAR_HEIGHT: f32 = 52.0;
/// Space between the window edge and the first button of each group.
pub(crate) const SIDE_MARGIN: f32 = 12.0;
/// Width and height of a button's hit area.
pub(crate) const BUTTON: f32 = 40.0;
/// Space between two buttons.
pub(crate) const GAP: f32 = 6.0;
/// Least height (and width, for an icon button) of a control a finger taps
/// in the portrait layout.
pub(crate) const TOUCH_TARGET: f32 = 44.0;
/// Space the title keeps clear of both groups.
const TITLE_CLEAR: f32 = 24.0;
/// Corner radius of a button's background.
const RADIUS: u8 = 8;
/// A hovered button's background.
const HOVER_FILL: Color32 = Color32::from_rgb(33, 44, 62);
/// Why the nav is disabled while Settings has unsaved edits.
pub(crate) const LOCKED_REASON: &str = "Save or discard the Settings edits first.";
/// Hover text of the Cameras button while no camera is configured.
const NO_CAMERAS: &str = "No cameras. Add one in Settings, Cameras";
/// Hover text of the (i) beside the Shot Type title.
/// A dropdown's padding around its content.
const MENU_PAD: f32 = 11.0;
/// Least room between the Shot Type tip and the window's edge or the
/// dropdown it opens under.
const TIP_MARGIN: f32 = 4.0;
/// How long a toast shows at full strength, then how long it fades out
/// (seconds).
const TOAST_SHOW: f64 = 2.5;
const TOAST_FADE: f64 = 0.3;
/// A toast's widest text, and its room above the bottom of the content.
const TOAST_WIDTH: f32 = 360.0;
const TOAST_BOTTOM: f32 = 24.0;

const SHOT_TYPE_TIP: &str =
    "Used as a hint by some launch monitors for the type of shot to expect.";

/// A dropdown of the bar; at most one is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderMenu {
    Energy,
    AutoChip,
    ShotType,
    Club,
    Golfer,
}

/// A button of the right group, in order from the left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Energy,
    Follow,
    AutoChip,
    ShotType,
    Club,
    Golfer,
}

/// A golfer the Golfer card lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KnownGolfer {
    pub(crate) id: String,
    pub(crate) name: String,
    /// Named by a game, not saved.
    pub(crate) game: bool,
}

/// The name of the view on the shots with no golfer.
pub(crate) const NO_GOLFER: &str = "No golfer";

/// A Golfer card row that only changes the view: a golfer with shots this
/// run who is no longer a known golfer (`key` is their golfer key), or
/// "No golfer" (`key` is `None`) for the shots with no golfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PastGolfer {
    pub(crate) key: Option<String>,
    pub(crate) name: String,
}

/// The rows of the Golfer card: the known golfers, then the view-only
/// rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GolferList {
    pub(crate) known: Vec<KnownGolfer>,
    pub(crate) past: Vec<PastGolfer>,
}

impl GolferList {
    /// Whether the view can be on golfer key `key` (`None` = no golfer):
    /// a known golfer or a view-only row.
    pub(crate) fn viewable(&self, key: Option<&str>) -> bool {
        key.is_some_and(|k| self.known.iter().any(|g| g.id == k))
            || self.past.iter().any(|p| p.key.as_deref() == key)
    }
}

/// The widths of a group of `n` buttons `button` wide, `gap` apart.
pub(crate) fn group_width(n: usize, button: f32, gap: f32) -> f32 {
    if n == 0 {
        0.0
    } else {
        n as f32 * button + (n - 1) as f32 * gap
    }
}

/// A header button's size: [`TOUCH_TARGET`] in the portrait layout, else
/// [`BUTTON`].
#[must_use]
pub(crate) fn button_size(portrait: bool) -> f32 {
    if portrait { TOUCH_TARGET } else { BUTTON }
}

/// Whether a title `width` wide, centered in a bar of `bar` between a left
/// group ending at `left_end` and a right group starting at `right_start`,
/// keeps [`TITLE_CLEAR`] clear of both.
pub(crate) fn title_fits(bar: Rect, left_end: f32, right_start: f32, width: f32) -> bool {
    let half = width / 2.0;
    bar.center().x - left_end >= half + TITLE_CLEAR
        && right_start - bar.center().x >= half + TITLE_CLEAR
}

/// The button background for the state of `response`.
fn background(ui: &egui::Ui, rect: Rect, response: &egui::Response, selected: bool) {
    let fill = if selected {
        Some(theme::SURFACE_RAISED)
    } else if response.hovered() && response.enabled() {
        Some(HOVER_FILL)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().rect_filled(rect, RADIUS, fill);
    }
}

/// Draw a button's icon centered in `rect`: the hover state as drawn while
/// the button is hovered or selected, or `on` (a toggle that is on); else
/// the normal state tinted `normal`, or `TEXT_DIM` while disabled. Returns
/// the icon's rect and the ink of a label drawn on it.
fn button_icon(
    ui: &egui::Ui,
    rect: Rect,
    response: &egui::Response,
    selected: bool,
    on: bool,
    asset: &Asset,
) -> (Rect, Color32) {
    let enabled = response.enabled();
    let lit = enabled && (on || selected || response.hovered());
    let (tint, ink) = if lit {
        (Color32::WHITE, theme::BG)
    } else if enabled {
        (theme::TEXT_MUTED, theme::TEXT_MUTED)
    } else {
        (theme::TEXT_DIM, theme::TEXT_DIM)
    };
    let drawn = icons::paint(ui, rect.center(), asset, asset.size, lit, tint);
    (drawn, ink)
}

/// A button's hover text: shown on hover while enabled, on hover over the
/// disabled button otherwise; on a touch screen also as a [`touch_tip`].
fn tip(response: egui::Response, text: impl Into<egui::WidgetText>) -> egui::Response {
    let text = text.into();
    let response = if response.enabled() {
        response.on_hover_text(text.clone())
    } else {
        response.on_disabled_hover_text(text.clone())
    };
    touch_tip(response, text)
}

/// Where the context keeps that this session saw a touch.
fn touch_seen_id() -> egui::Id {
    egui::Id::new("touch_seen")
}

/// Where the context keeps the widget whose touch tip shows.
fn touch_tip_id() -> egui::Id {
    egui::Id::new("touch_tip")
}

/// Once a frame, before any widget: notes a touch, and a press anywhere
/// hides the touch tip shown.
pub(crate) fn touch_tips_frame(ctx: &egui::Context) {
    let (touch, pressed) = ctx.input(|i| {
        let touch = i
            .events
            .iter()
            .any(|e| matches!(e, egui::Event::Touch { .. }));
        (touch, i.pointer.any_pressed())
    });
    ctx.data_mut(|d| {
        if touch {
            d.insert_temp(touch_seen_id(), true);
        }
        if pressed {
            d.insert_temp::<Option<egui::Id>>(touch_tip_id(), None);
        }
    });
}

/// On a touch screen, a tap on the disabled `response`, or a long press on
/// it enabled, shows `text` as a tooltip under it until the next press
/// anywhere: a finger can't hover.
pub(crate) fn touch_tip(response: egui::Response, text: egui::WidgetText) -> egui::Response {
    let tapped = if response.enabled() {
        response.long_touched()
    } else {
        response.contains_pointer() && response.ctx.input(|i| i.pointer.any_pressed())
    };
    if touch_tip_open(&response, tapped) {
        response.show_tooltip_text(text);
    }
    response
}

/// On a touch screen, whether the touch tip of `response` shows: from the
/// frame it is `tapped` until the next press anywhere.
pub(crate) fn touch_tip_open(response: &egui::Response, tapped: bool) -> bool {
    let ctx = &response.ctx;
    if !ctx.data(|d| d.get_temp::<bool>(touch_seen_id()).unwrap_or(false)) {
        return false;
    }
    if tapped {
        ctx.data_mut(|d| d.insert_temp(touch_tip_id(), Some(response.id)));
    }
    ctx.data(|d| d.get_temp::<Option<egui::Id>>(touch_tip_id()))
        .flatten()
        == Some(response.id)
}

/// The 40 px hit area at `rect`, enabled or not, named for the tests.
fn hit(ui: &mut egui::Ui, rect: Rect, name: &str, enabled: bool) -> egui::Response {
    // A child that takes no room in the bar's own layout.
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    if !enabled {
        child.disable();
    }
    let response = child.interact(rect, ui.id().with(("header", name)), Sense::click());
    mark(ui, || name.to_owned(), &response);
    response
}

/// The golfer ring with `initial`, or the person outline without one, in
/// its hover state when `lit`, else tinted `color`.
fn avatar(
    ui: &egui::Ui,
    center: Pos2,
    diameter: f32,
    initial: Option<char>,
    lit: bool,
    color: Color32,
) {
    let tint = if lit { Color32::WHITE } else { color };
    match initial {
        Some(c) => {
            let rect = icons::paint(ui, center, &icons::GOLFER, diameter, lit, tint);
            let ink = if lit { theme::BG } else { color };
            icons::paint_label(ui, rect, &icons::GOLFER, &c.to_string(), ink);
        }
        None => {
            icons::paint(ui, center, &icons::GOLFER_NONE, diameter, lit, tint);
        }
    }
}

/// The first letter of `name`, upper-cased; `None` for an empty name.
fn initial_of(name: &str) -> Option<char> {
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().next().unwrap_or(c))
}

/// The icon of shot type `mode`.
fn mode_icon(mode: Mode) -> &'static Asset {
    match mode {
        Mode::Full => &icons::SHOT_FULL,
        Mode::Chipping => &icons::SHOT_CHIP,
        Mode::Putting => &icons::SHOT_PUTT,
        _ => &icons::SHOT_FULL,
    }
}

/// The head icon of `club`, by the club picker's rows: driver, woods,
/// hybrids, irons, wedges, putter; `club-none` without a club.
fn club_icon(club: Option<Club>) -> &'static Asset {
    const HEADS: [&Asset; 6] = [
        &icons::CLUB_DRIVER,
        &icons::CLUB_WOOD,
        &icons::CLUB_HYBRID,
        &icons::CLUB_IRON,
        &icons::CLUB_WEDGE,
        &icons::CLUB_PUTTER,
    ];
    club.and_then(|c| Club::ALL.iter().position(|a| *a == c))
        .and_then(|i| {
            crate::app::CLUB_PICKER_ROWS
                .iter()
                .position(|r| r.contains(&i))
        })
        .map_or(&icons::CLUB_NONE, |row| HEADS[row])
}

/// The label of shot type `mode`.
fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Full => "Full",
        Mode::Chipping => "Chip",
        Mode::Putting => "Putt",
        _ => "Other",
    }
}

/// The shot type named by the UI's `current_mode` text.
fn mode_of(text: &str) -> Mode {
    match text {
        "chipping" => Mode::Chipping,
        "putting" => Mode::Putting,
        _ => Mode::Full,
    }
}

/// The popup frame every dropdown uses.
fn menu_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::SURFACE_RAISED)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::CARD_RADIUS)
        .inner_margin(egui::Margin::same(10))
}

/// What a dropdown's content may take in the portrait layout: the
/// screen's width less [`SIDE_MARGIN`] each side and the frame.
#[must_use]
pub(crate) fn menu_max_width(ctx: &egui::Context) -> f32 {
    ctx.content_rect().width() - 2.0 * SIDE_MARGIN - 22.0
}

/// A dropdown's content in the portrait layout: rows and buttons at least
/// [`TOUCH_TARGET`] tall, at most [`menu_max_width`] wide.
pub(crate) fn touch_menu(ui: &mut egui::Ui) {
    ui.spacing_mut().interact_size.y = TOUCH_TARGET;
    ui.set_max_width(menu_max_width(ui.ctx()));
}

/// A dropdown row's height: `base`, or the touch height inside a
/// [`touch_menu`].
fn row_height(ui: &egui::Ui, base: f32) -> f32 {
    base.max(ui.spacing().interact_size.y)
}

/// A dropdown's title: a small caps caption, as every card in the bar
/// uses.
fn menu_title(ui: &mut egui::Ui, text: &str) {
    ui.label(theme::caps(text, 11.0, theme::TEXT_MUTED));
    ui.add_space(4.0);
}

/// A full-width dropdown row with an optional icon and `label`; the
/// selected one is highlighted with an accent bar.
fn menu_row(
    ui: &mut egui::Ui,
    width: f32,
    selected: bool,
    icon: Option<&Asset>,
    label: &str,
) -> egui::Response {
    let height = row_height(ui, 32.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let painter = ui.painter();
    if selected || response.hovered() {
        let fill = if selected {
            theme::SURFACE_HOVER
        } else {
            theme::SURFACE
        };
        painter.rect_filled(rect, 6.0, fill);
    }
    if selected {
        painter.rect_filled(
            Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(3.0, rect.height() - 12.0)),
            1.5,
            theme::ACCENT,
        );
    }
    let mut x = rect.left() + 14.0;
    if let Some(icon) = icon {
        let lit = selected || response.hovered();
        let tint = if lit {
            Color32::WHITE
        } else {
            theme::TEXT_MUTED
        };
        icons::paint(ui, pos2(x + 8.0, rect.center().y), icon, 16.0, lit, tint);
        x += 28.0;
    }
    ui.painter().text(
        pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(14.0),
        theme::TEXT,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// What a pass of the bar picked.
#[derive(Default)]
struct Picks {
    mode: Option<Mode>,
    club: Option<Club>,
    golfer: Option<String>,
    /// A view-only row picked: the key it views.
    view: Option<Option<String>>,
    chip: Option<u32>,
    settings: bool,
}

impl FlighthookApp {
    /// The known golfers ([`Self::compute_known_golfers`]) as worked out
    /// this frame.
    pub(crate) fn known_golfers(&self) -> &[KnownGolfer] {
        &self.golfer_list.known
    }

    /// Work out the Golfer card's rows for this frame: the known golfers,
    /// then a row for every golfer key the shot history holds that no known
    /// golfer has (newest shot first, named as their shots are), then
    /// "No golfer" while the history holds shots with no golfer and the
    /// card lists anyone else.
    pub(crate) fn refresh_golfer_list(&mut self) {
        let known = self.compute_known_golfers();
        let config = self.settings.saved_config_ref();
        let keys = self.groups.golfers();
        let mut past: Vec<PastGolfer> = Vec::new();
        let mut no_golfer = false;
        for row in self.shots.iter().rev() {
            match keys.key(row.golfer.as_deref()) {
                None => no_golfer = true,
                Some(key) => {
                    if !known.iter().any(|g| g.id == key)
                        && !past.iter().any(|p| p.key.as_deref() == Some(key))
                    {
                        let mut name =
                            golfer_name(config, &self.bus_states, &self.golfer_names, key);
                        if name == crate::app::UNKNOWN_GOLFER {
                            let at = self.unknown_golfers.iter().position(|k| k == key);
                            let n = at.unwrap_or_else(|| {
                                self.unknown_golfers.push(key.to_owned());
                                self.unknown_golfers.len() - 1
                            });
                            name = format!("{name} {}", n + 1);
                        }
                        past.push(PastGolfer {
                            key: Some(key.to_owned()),
                            name,
                        });
                    }
                }
            }
        }
        if no_golfer && !(known.is_empty() && past.is_empty()) {
            past.push(PastGolfer {
                key: None,
                name: NO_GOLFER.to_owned(),
            });
        }
        self.golfer_list = GolferList { known, past };
    }

    /// The golfers the Golfer card lists first: the saved golfers, then the
    /// golfers a game names that no saved golfer stands for.
    pub(crate) fn compute_known_golfers(&self) -> Vec<KnownGolfer> {
        let config = self.settings.saved_config_ref();
        let mut out: Vec<KnownGolfer> = config
            .map(|c| {
                c.golfers
                    .iter()
                    .filter_map(|g| g.own_id())
                    .map(|id| KnownGolfer {
                        id: id.to_owned(),
                        name: golfer_name(Some(c), &self.bus_states, &self.golfer_names, id),
                        game: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut game_ids: Vec<String> = self
            .bus_states
            .values()
            .filter_map(|m| match &m.body {
                flighthook::FrpBody::GolferProfile(p) => Some(p.id.as_str().to_owned()),
                _ => None,
            })
            .filter(|id| !flighthook::is_own_golfer_id(id))
            .collect();
        if let Some(up) = self.turn_golfer.as_deref()
            && !flighthook::is_own_golfer_id(up)
        {
            game_ids.push(up.to_owned());
        }
        game_ids.sort();
        game_ids.dedup();
        for id in game_ids {
            if config.is_some_and(|c| c.golfer_of(&id).is_some()) {
                continue;
            }
            out.push(KnownGolfer {
                name: golfer_name(config, &self.bus_states, &self.golfer_names, &id),
                id,
                game: true,
            });
        }
        out
    }

    /// Whether the Shot Type button shows ([`flighthook::FlighthookConfig::shot_type_available`]).
    pub(crate) fn shot_type_shown(&self) -> bool {
        self.settings
            .saved_config_ref()
            .is_some_and(|c| c.shot_type_available())
    }

    /// The right group's buttons, left to right, for what is shown now.
    fn right_slots(&self, batteries: &[Battery]) -> Vec<Slot> {
        let mut slots = Vec::new();
        if !batteries.is_empty() {
            slots.push(Slot::Energy);
        }
        if self.known_golfers().len() >= 2 {
            slots.push(Slot::Follow);
        }
        if self.settings.auto_chip_enabled() {
            slots.push(Slot::AutoChip);
        }
        if self.shot_type_shown() {
            slots.push(Slot::ShotType);
        }
        slots.push(Slot::Club);
        slots.push(Slot::Golfer);
        slots
    }

    /// The device batteries, in Devices card order, then the remote
    /// launch monitors'.
    fn all_batteries(&self) -> Vec<Battery> {
        let mut batteries = battery::batteries(&self.actors, &self.settings.actors);
        batteries.extend(battery::remote_batteries(&self.remote_cards()));
        batteries
    }

    /// Open `menu`, or close it when it is the one open.
    fn toggle_menu(&mut self, menu: HeaderMenu) {
        self.header_menu = if self.header_menu == Some(menu) {
            None
        } else {
            Some(menu)
        };
    }

    /// Show the dropdown `menu` under `button` while it is the open one.
    fn dropdown(
        &mut self,
        menu: HeaderMenu,
        button: &egui::Response,
        content: impl FnOnce(&mut egui::Ui),
    ) {
        let mut open = self.header_menu == Some(menu);
        let portrait = self.portrait;
        egui::Popup::from_response(button)
            .open_bool(&mut open)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .align(egui::RectAlign::BOTTOM_END)
            .frame(menu_frame())
            .show(|ui| {
                if portrait {
                    touch_menu(ui);
                }
                content(ui);
            });
        if !open && self.header_menu == Some(menu) {
            self.header_menu = None;
        }
    }

    /// [`Self::render_header`] after working out the Golfer card's rows,
    /// as `update` does each frame before it.
    #[cfg(test)]
    pub(crate) fn render_header_in_test(&mut self, ctx: &egui::Context) {
        touch_tips_frame(ctx);
        self.refresh_golfer_list();
        self.render_header(ctx);
    }

    /// The top bar. In the portrait layout it holds the right group and
    /// the title while it fits, and the nav is the [`Self::render_tab_bar`]
    /// at the bottom.
    pub(crate) fn render_header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .frame(egui::Frame::new().fill(theme::SURFACE))
            .exact_height(BAR_HEIGHT)
            .show_separator_line(true)
            .show(ctx, |ui| {
                let full = ui.max_rect();
                let mut picks = Picks::default();
                if self.portrait {
                    let right_start = self.render_right(ui, full, full.left(), &mut picks);
                    let room = Rect::from_min_max(full.min, pos2(right_start, full.bottom()));
                    self.render_title(ui, room, full.left(), right_start);
                } else {
                    let left_end = self.render_nav(ui, full, false);
                    let right_start = self.render_right(ui, full, left_end, &mut picks);
                    self.render_title(ui, full, left_end, right_start);
                }
                self.apply_picks(ui.ctx(), picks);
            });
        if self.portrait {
            self.render_tab_bar(ctx);
        }
        self.render_toast(ctx);
    }

    /// Show `text` as a toast, in place of the one showing.
    fn toast(&mut self, ctx: &egui::Context, text: String) {
        self.toast = Some((text, ctx.input(|i| i.time)));
    }

    /// The toast, while it shows: a dark panel centered near the bottom of
    /// the content, above the tab bar, that fades out at the end. A finger
    /// can't hover, so a switch says what it now does.
    fn render_toast(&mut self, ctx: &egui::Context) {
        let Some((text, since)) = &self.toast else {
            return;
        };
        let age = ctx.input(|i| i.time) - since;
        if age >= TOAST_SHOW + TOAST_FADE {
            self.toast = None;
            return;
        }
        if age < TOAST_SHOW {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(TOAST_SHOW - age));
        } else {
            ctx.request_repaint();
        }
        #[allow(clippy::cast_possible_truncation)] // a 0..1 opacity
        let opacity = ((TOAST_SHOW + TOAST_FADE - age) / TOAST_FADE).min(1.0) as f32;
        let content = ctx.available_rect();
        let width = TOAST_WIDTH.min(content.width() - 2.0 * TOAST_BOTTOM);
        egui::Area::new(egui::Id::new("toast"))
            .order(egui::Order::Foreground)
            .interactable(false)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(content.center().x, content.bottom() - TOAST_BOTTOM))
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                egui::Frame::new()
                    .fill(theme::SURFACE_RAISED)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .corner_radius(theme::CARD_RADIUS)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(width);
                        ui.label(egui::RichText::new(text.as_str()).color(theme::TEXT));
                    });
            });
    }

    /// The portrait layout's nav: a bar at the bottom of the window with
    /// four equal tabs.
    fn render_tab_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("tab_bar")
            .frame(egui::Frame::new().fill(theme::SURFACE))
            .exact_height(BAR_HEIGHT)
            .show_separator_line(true)
            .show(ctx, |ui| {
                let full = ui.max_rect();
                self.render_nav(ui, full, true);
            });
    }

    /// The nav icons, as buttons at the left of `bar` or as `tabs` that
    /// share its width; returns where they end.
    fn render_nav(&mut self, ui: &mut egui::Ui, bar: Rect, tabs: bool) -> f32 {
        let locked = self.settings.locked();
        let cameras = self.cameras_shown();
        let alert = settings_alert(&self.actors, self.now_ms);
        // A red card turns yellow on time, with no event to wake the UI.
        if let Some(ms) = crate::panels::next_alert_change(&self.actors, self.now_ms) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(ms));
        }
        let on_shots = self.active_tab == Tab::Shots;
        let cameras_on = on_shots && cameras && self.shots_pane == ShotsPane::Cameras;
        let items = [
            ("shots", "Shots", &icons::NAV_SHOTS, on_shots && !cameras_on),
            ("cameras", "Cameras", &icons::NAV_CAMERAS, cameras_on),
            (
                "settings",
                "Settings",
                &icons::NAV_SETTINGS,
                self.active_tab == Tab::Devices,
            ),
            ("log", "Log", &icons::NAV_LOG, self.active_tab == Tab::Log),
        ];
        let (button, gap, mut x) = if tabs {
            (vec2(bar.width() / 4.0, TOUCH_TARGET), 0.0, bar.left())
        } else {
            (vec2(BUTTON, BUTTON), GAP, bar.left() + SIDE_MARGIN)
        };
        let top = bar.top() + (bar.height() - button.y) / 2.0;
        // The selected marker: under the icon in the top bar, at the top
        // edge of a tab.
        let marker_y = if tabs {
            bar.top() + 1.5
        } else {
            bar.bottom() - 1.5
        };
        for (key, label, icon, selected) in items {
            let rect = Rect::from_min_size(pos2(x, top), button);
            x += button.x + gap;
            let usable = !locked && (key != "cameras" || cameras);
            let response = hit(ui, rect, &format!("nav:{key}"), usable);
            background(ui, rect, &response, selected);
            button_icon(ui, rect, &response, selected, false, icon);
            if selected {
                ui.painter().rect_filled(
                    Rect::from_center_size(pos2(rect.center().x, marker_y), vec2(24.0, 3.0)),
                    1.5,
                    theme::ACCENT,
                );
            }
            if key == "settings" && alert {
                // At the icon's top right corner, also on a wide tab.
                let corner = rect.center() + vec2(BUTTON, -BUTTON) / 2.0;
                let dot = Rect::from_center_size(corner + vec2(-7.0, 7.0), vec2(7.0, 7.0));
                ui.painter().circle_filled(dot.center(), 3.5, theme::DANGER);
                let marker = ui.interact(dot, ui.id().with("alert_dot"), Sense::hover());
                mark(ui, || "alert_dot".to_owned(), &marker);
            }
            let text = if locked {
                LOCKED_REASON
            } else if key == "cameras" && !cameras {
                NO_CAMERAS
            } else {
                label
            };
            let response = tip(response, text);
            if response.clicked() {
                match key {
                    "shots" => {
                        self.active_tab = Tab::Shots;
                        self.set_shots_pane(ShotsPane::Data);
                    }
                    "cameras" => {
                        self.active_tab = Tab::Shots;
                        self.set_shots_pane(ShotsPane::Cameras);
                    }
                    "settings" => self.active_tab = Tab::Devices,
                    _ => self.active_tab = Tab::Log,
                }
            }
        }
        x - gap
    }

    /// The right group, anchored to the right edge; returns where it
    /// starts. In a window too narrow for both groups it starts a gap
    /// after the nav (`left_end`) and is cut off at the right edge, so the
    /// groups never overlap.
    fn render_right(
        &mut self,
        ui: &mut egui::Ui,
        bar: Rect,
        left_end: f32,
        picks: &mut Picks,
    ) -> f32 {
        let batteries = self.all_batteries();
        let slots = self.right_slots(&batteries);
        // With fewer than two golfers Follow is hidden and counts as on.
        if !slots.contains(&Slot::Follow) {
            self.follow_turns = true;
        }
        let button = button_size(self.portrait);
        let top = bar.top() + (bar.height() - button) / 2.0;
        let start =
            (bar.right() - SIDE_MARGIN - group_width(slots.len(), button, GAP)).max(left_end + GAP);
        let clip = ui.clip_rect();
        ui.set_clip_rect(clip.intersect(Rect::from_min_max(pos2(start, bar.top()), bar.max)));
        for (i, slot) in slots.iter().enumerate() {
            let x = start + i as f32 * (button + GAP);
            let rect = Rect::from_min_size(pos2(x, top), vec2(button, button));
            match slot {
                Slot::Energy => self.energy_button(ui, rect, &batteries),
                Slot::Follow => self.follow_button(ui, rect),
                Slot::AutoChip => self.auto_chip_button(ui, rect, picks),
                Slot::ShotType => self.shot_type_button(ui, rect, picks),
                Slot::Club => self.club_button(ui, rect, picks),
                Slot::Golfer => self.golfer_button(ui, rect, picks),
            }
        }
        ui.set_clip_rect(clip);
        start
    }

    /// Whether `menu` is open, for a button's selected look.
    fn open(&self, menu: HeaderMenu) -> bool {
        self.header_menu == Some(menu)
    }

    /// The energy icon and its Batteries dropdown.
    fn energy_button(&mut self, ui: &mut egui::Ui, rect: Rect, batteries: &[Battery]) {
        let selected = self.open(HeaderMenu::Energy);
        let response = hit(ui, rect, "energy", true);
        background(ui, rect, &response, selected);
        let color = battery::energy_color(batteries);
        if color == theme::TEXT_MUTED {
            button_icon(ui, rect, &response, selected, false, &icons::ENERGY);
        } else {
            // The filled bolt keeps its warning color in both states.
            let lit = selected || response.hovered();
            icons::paint(ui, rect.center(), &icons::ENERGY_FILLED, 24.0, lit, color);
        }
        let text = battery::lowest_tooltip(batteries).unwrap_or_default();
        let response = tip(response, text);
        if response.clicked() {
            self.toggle_menu(HeaderMenu::Energy);
        }
        self.dropdown(HeaderMenu::Energy, &response, |ui| {
            ui.set_min_width(240.0);
            menu_title(ui, "Batteries");
            for b in batteries {
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        ui.label(egui::RichText::new(&b.name).color(theme::TEXT));
                    },
                    |ui| {
                        ui.label(
                            egui::RichText::new(format!("{}%", b.pct))
                                .color(battery::fill_color(b.pct)),
                        );
                        let icon = battery::show(ui, b);
                        mark(ui, || format!("battery:{}", b.actor), &icon);
                    },
                );
            }
        });
    }

    /// The Follow turns button: the session's state, drawn on or slashed.
    fn follow_button(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let response = hit(ui, rect, "follow", true);
        background(ui, rect, &response, false);
        let icon = if self.follow_turns {
            &icons::FOLLOW_ON
        } else {
            &icons::FOLLOW_OFF
        };
        button_icon(ui, rect, &response, false, self.follow_turns, icon);
        if tip(response, self.follow_tip()).clicked() {
            self.follow_turns = !self.follow_turns;
            self.view_hold = None;
            self.follow_view();
            self.toast(ui.ctx(), self.follow_switch_text().to_owned());
        }
    }

    /// The message a Follow turns switch shows: the new state only.
    fn follow_switch_text(&self) -> &'static str {
        if self.follow_turns {
            "Following turns: on."
        } else {
            "Following turns: off."
        }
    }

    /// The Follow turns button's hover text.
    pub(crate) fn follow_tip(&self) -> String {
        let up_name = self.up_name();
        if self.follow_turns {
            "Following turns: on. The view moves to the golfer up.".to_owned()
        } else if self.viewing_other() {
            let viewed = self.viewed_name();
            match up_name {
                Some(up) => format!("Following turns: off. Viewing {viewed}; {up} is up."),
                None => format!("Following turns: off. Viewing {viewed}; no golfer is up."),
            }
        } else {
            let up = up_name.unwrap_or_else(|| "the golfer up".to_owned());
            format!("Following turns: off. The view stays on {up} when the turn changes.")
        }
    }

    /// The golfer up's name, when a golfer is up.
    fn up_name(&self) -> Option<String> {
        self.turn_golfer.as_deref().map(|id| {
            golfer_name(
                self.settings.saved_config_ref(),
                &self.bus_states,
                &self.golfer_names,
                id,
            )
        })
    }

    /// The viewed golfer's name ("No golfer" for none): as its Golfer card
    /// row names it.
    pub(crate) fn viewed_name(&self) -> String {
        if let Some(p) = self
            .golfer_list
            .past
            .iter()
            .find(|p| p.key == self.viewed_golfer)
        {
            return p.name.clone();
        }
        self.viewed_golfer.as_deref().map_or_else(
            || NO_GOLFER.to_owned(),
            |id| {
                golfer_name(
                    self.settings.saved_config_ref(),
                    &self.bus_states,
                    &self.golfer_names,
                    id,
                )
            },
        )
    }

    /// What the Club, Shot Type and Auto-Chip hover texts add while the
    /// view is on someone other than the golfer up: " ({name}, up)".
    pub(crate) fn up_suffix(&self) -> String {
        match self.up_name() {
            Some(up) if self.viewing_other() => format!(" ({up}, up)"),
            _ => String::new(),
        }
    }

    /// The Auto-Chip button and its dropdown.
    fn auto_chip_button(&mut self, ui: &mut egui::Ui, rect: Rect, picks: &mut Picks) {
        let blocked = auto_chip_blocked(&self.actors, self.auto_chip_in_flight);
        let selected = self.open(HeaderMenu::AutoChip);
        let response = hit(ui, rect, "auto-chip", blocked.is_none());
        background(ui, rect, &response, selected);
        let icon = if blocked.is_some() {
            &icons::AUTO_CHIP_OFF
        } else {
            &icons::AUTO_CHIP_ON
        };
        // An action, not a toggle: muted at rest, lit on hover or while open.
        button_icon(ui, rect, &response, selected, false, icon);
        let text = format!(
            "{}{}",
            blocked.as_deref().unwrap_or(AUTO_CHIP_HINT),
            self.up_suffix()
        );
        let response = tip(response, text);
        if response.clicked() {
            self.toggle_menu(HeaderMenu::AutoChip);
        }
        if blocked.is_some() && self.open(HeaderMenu::AutoChip) {
            self.header_menu = None;
        }
        let mut chosen = None;
        self.dropdown(HeaderMenu::AutoChip, &response, |ui| {
            ui.set_min_width(150.0);
            menu_title(ui, "Auto-Chip");
            for (label, yards) in auto_chip_items() {
                let item = menu_row(ui, 150.0, false, None, &label);
                mark(ui, || format!("auto-chip:{label}"), &item);
                if item.clicked() {
                    chosen = Some(yards);
                }
            }
        });
        if let Some(yards) = chosen {
            self.header_menu = None;
            picks.chip = Some(yards);
        }
    }

    /// The Shot Type button and its dropdown.
    fn shot_type_button(&mut self, ui: &mut egui::Ui, rect: Rect, picks: &mut Picks) {
        let mode = mode_of(&self.current_mode);
        let selected = self.open(HeaderMenu::ShotType);
        let response = hit(ui, rect, "shot-type", true);
        background(ui, rect, &response, selected);
        button_icon(ui, rect, &response, selected, false, mode_icon(mode));
        let text = format!("Shot Type: {}{}", mode_label(mode), self.up_suffix());
        let response = tip(response, text);
        if response.clicked() {
            self.toggle_menu(HeaderMenu::ShotType);
        }
        let mut chosen = None;
        let portrait = self.portrait;
        self.dropdown(HeaderMenu::ShotType, &response, |ui| {
            ui.set_min_width(170.0);
            let info = ui
                .horizontal(|ui| {
                    menu_title(ui, "Shot Type");
                    let info = theme::icon(ui, theme::Icon::Info, 14.0, theme::TEXT_MUTED, true);
                    mark(ui, || "shot-type:tip".to_owned(), &info);
                    info
                })
                .inner;
            for m in [Mode::Full, Mode::Chipping, Mode::Putting] {
                let row = menu_row(ui, 170.0, m == mode, Some(mode_icon(m)), mode_label(m));
                mark(ui, || format!("shot-type:{}", mode_label(m)), &row);
                if row.clicked() {
                    chosen = Some(m);
                }
            }
            // On a touch screen a tap on the (i) shows it too.
            let tapped = touch_tip_open(&info, info.clicked());
            if tapped || info.hovered() {
                // Left of the dropdown, its top on the dropdown's top, so it
                // never covers a row; where there is no room on the left,
                // under the dropdown; in the portrait layout under the (i).
                // Two lines.
                let content = ui.min_rect();
                let font = egui::TextStyle::Body.resolve(ui.style());
                let line = ui
                    .painter()
                    .layout_no_wrap(SHOT_TYPE_TIP.to_owned(), font.clone(), theme::TEXT)
                    .size()
                    .x;
                let wrap = (line * 0.6).ceil();
                let text = ui
                    .painter()
                    .layout(SHOT_TYPE_TIP.to_owned(), font, theme::TEXT, wrap)
                    .size();
                let size = text + ui.style().spacing.menu_margin.sum() + vec2(2.0, 2.0);
                let screen = ui.ctx().content_rect();
                let left = content.left() - 20.0;
                let (at, pivot) = if portrait {
                    let x = info
                        .rect
                        .left()
                        .min(screen.right() - TIP_MARGIN - size.x)
                        .max(screen.left() + TIP_MARGIN);
                    (
                        pos2(x, info.rect.bottom() + TIP_MARGIN),
                        egui::Align2::LEFT_TOP,
                    )
                } else if left - size.x >= screen.left() + TIP_MARGIN {
                    (
                        pos2(left, content.top() - MENU_PAD),
                        egui::Align2::RIGHT_TOP,
                    )
                } else {
                    let x = (content.left() - MENU_PAD)
                        .min(screen.right() - TIP_MARGIN - size.x)
                        .max(screen.left() + TIP_MARGIN);
                    (
                        pos2(x, content.bottom() + MENU_PAD + TIP_MARGIN),
                        egui::Align2::LEFT_TOP,
                    )
                };
                let tip = egui::Area::new(ui.id().with("shot-type-tip"))
                    .order(egui::Order::Tooltip)
                    .fixed_pos(at)
                    .pivot(pivot)
                    .interactable(false)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_max_width(wrap);
                            ui.label(SHOT_TYPE_TIP);
                        });
                    });
                mark(ui, || "shot-type:tip-text".to_owned(), &tip.response);
            }
        });
        if let Some(m) = chosen {
            self.header_menu = None;
            picks.mode = Some(m);
        }
    }

    /// The Club button, its code drawn in a 32 px slot, and the popup.
    fn club_button(&mut self, ui: &mut egui::Ui, rect: Rect, picks: &mut Picks) {
        let selected = self.open(HeaderMenu::Club);
        let response = hit(ui, rect, "club", true);
        background(ui, rect, &response, selected);
        let icon = club_icon(self.selected_club);
        let (drawn, ink) = button_icon(ui, rect, &response, selected, false, icon);
        if let Some(club) = self.selected_club {
            icons::paint_label(ui, drawn, icon, &club.to_string(), ink);
        }
        let name = self.selected_club.map_or("none", club_name);
        let response = tip(response, format!("Club: {name}{}", self.up_suffix()));
        if response.clicked() {
            self.toggle_menu(HeaderMenu::Club);
        }
        let selected_club = self.selected_club;
        let offered = offered_clubs(
            self.settings.saved_config_ref(),
            &self.bus_states,
            self.turn_golfer.as_deref(),
            selected_club,
        );
        let mut chosen = None;
        self.dropdown(HeaderMenu::Club, &response, |ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            menu_title(ui, "Club");
            for row in crate::app::CLUB_PICKER_ROWS {
                let clubs: Vec<Club> = Club::ALL[row]
                    .iter()
                    .copied()
                    .filter(|c| offered.contains(c))
                    .collect();
                if clubs.is_empty() {
                    continue;
                }
                ui.horizontal(|ui| {
                    for club in clubs {
                        let on = selected_club == Some(club);
                        let size = vec2(
                            crate::app::CLUB_PILL.x,
                            row_height(ui, crate::app::CLUB_PILL.y),
                        );
                        let pill = theme::pill_sized(ui, on, &club.to_string(), size);
                        mark(ui, || format!("club_{club}"), &pill);
                        if pill.clicked() {
                            chosen = Some(club);
                        }
                    }
                });
            }
        });
        if let Some(club) = chosen {
            self.header_menu = None;
            picks.club = Some(club);
        }
    }

    /// The Golfer button and its card: the golfer up's initial while the
    /// view follows the turn, else the viewed golfer's.
    fn golfer_button(&mut self, ui: &mut egui::Ui, rect: Rect, picks: &mut Picks) {
        let config = self.settings.saved_config_ref();
        // A held view-only row shows like a view off the turn.
        let follows = self.follows_turns() && self.view_hold.is_none();
        let shown = if follows {
            self.turn_golfer.as_deref()
        } else {
            self.viewed_golfer.as_deref()
        };
        let name = shown.map(|id| golfer_name(config, &self.bus_states, &self.golfer_names, id));
        let selected = self.open(HeaderMenu::Golfer);
        let response = hit(ui, rect, "golfer", true);
        background(ui, rect, &response, selected);
        avatar(
            ui,
            rect.center(),
            icons::GOLFER.size,
            name.as_deref().and_then(initial_of),
            selected || response.hovered(),
            theme::TEXT_MUTED,
        );
        let text = name
            .as_ref()
            .map_or_else(|| "Golfer".to_owned(), |n| format!("Golfer: {n}"));
        let response = tip(response, text);
        if response.clicked() {
            self.toggle_menu(HeaderMenu::Golfer);
        }
        let list = self.golfer_list.clone();
        // A linked game id stands for its saved golfer's row.
        let up = self.settings.saved_config_ref().map_or_else(
            || self.turn_golfer.clone(),
            |c| c.golfer_key(self.turn_golfer.as_deref()),
        );
        let rows = GolferRows {
            viewed: if follows {
                up.clone()
            } else {
                self.viewed_golfer.clone()
            },
            up_dot: (!follows).then_some(up).flatten(),
        };
        let mut chosen = None;
        let portrait = self.portrait;
        self.dropdown(HeaderMenu::Golfer, &response, |ui| {
            ui.set_width(if portrait {
                menu_max_width(ui.ctx())
            } else {
                300.0
            });
            golfer_card(ui, name.as_deref(), &list, &rows, picks, &mut chosen);
        });
        match chosen {
            Some(Chosen::Golfer(id)) => {
                self.header_menu = None;
                picks.golfer = Some(id);
            }
            Some(Chosen::View(key)) => {
                self.header_menu = None;
                picks.view = Some(key);
            }
            None => {}
        }
        if picks.settings {
            self.header_menu = None;
        }
    }

    /// The center title, drawn only while it fits.
    fn render_title(&mut self, ui: &mut egui::Ui, bar: Rect, left_end: f32, right_start: f32) {
        let title = brand_title();
        let galley = ui.painter().layout_job(title.job());
        let dot = 10.0;
        let gap = 6.0;
        let width = dot + gap + galley.size().x;
        if !title_fits(bar, left_end, right_start, width) {
            return;
        }
        let rect = Rect::from_center_size(bar.center(), vec2(width, galley.size().y.max(dot)));
        ui.painter().circle_filled(
            pos2(rect.left() + dot / 2.0, rect.center().y),
            4.0,
            title.dot,
        );
        ui.painter().galley(
            pos2(
                rect.left() + dot + gap,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            Color32::PLACEHOLDER,
        );
        let response = ui.interact(rect, ui.id().with("title"), Sense::hover());
        mark(ui, || "title".to_owned(), &response);
    }

    /// Carry out what the bar picked.
    fn apply_picks(&mut self, ctx: &egui::Context, picks: Picks) {
        if let Some(mode) = picks.mode {
            self.current_mode = mode.to_string();
            if let Some(tx) = &mut self.ws_sender {
                net::send_command(tx, &net::select_command(|s| s.mode = Some(mode)));
            }
        }
        if let Some(club) = picks.club {
            self.pick_club(club);
        }
        if let Some(id) = picks.golfer {
            self.pick_golfer(&id);
        }
        if let Some(key) = picks.view {
            self.pick_view_only(key);
        }
        if let Some(yards) = picks.chip {
            self.send_auto_chip(ctx, yards);
        }
        if picks.settings {
            self.active_tab = Tab::Devices;
            // Unsaved edits keep their card in sight.
            if !self.settings.locked() {
                self.settings_section = crate::panels::settings_nav::SettingsSection::Golfers;
                self.settings_open = true;
            }
        }
    }
}

/// Which rows of the Golfer card are marked.
struct GolferRows {
    /// The viewed golfer's row key: drawn selected.
    viewed: Option<String>,
    /// The golfer up's row key, drawn with the gold "up" dot (only while
    /// the view does not follow the turn).
    up_dot: Option<String>,
}

/// What a click in the Golfer card picked.
enum Chosen {
    /// A known golfer's row: their id.
    Golfer(String),
    /// A view-only row: the golfer key it views (`None` = no golfer).
    View(Option<String>),
}

/// The Golfer card's content.
/// Where the Golfer card's name starts: right of the avatar.
const NAME_LEFT: f32 = 56.0;
/// What the Golfer card's name leaves free on the right: the gear and a gap.
const GEAR_ROOM: f32 = 26.0;

fn golfer_card(
    ui: &mut egui::Ui,
    name: Option<&str>,
    list: &GolferList,
    rows: &GolferRows,
    picks: &mut Picks,
    chosen: &mut Option<Chosen>,
) {
    let golfers = &list.known;
    // The avatar with the name centered on it; the gear in the top right
    // corner. The full name wraps between the avatar and the gear, and the
    // header grows to it.
    let wrap = (ui.available_width() - NAME_LEFT - GEAR_ROOM).max(1.0);
    let galley = ui.painter().layout(
        name.unwrap_or(NO_GOLFER).to_owned(),
        egui::FontId::proportional(16.0),
        theme::TEXT,
        wrap,
    );
    let height = galley.size().y.max(44.0);
    let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    avatar(
        ui,
        pos2(head.left() + 22.0, head.center().y),
        44.0,
        name.and_then(initial_of),
        false,
        theme::TEXT_MUTED,
    );
    let name_rect = Rect::from_min_size(
        pos2(
            head.left() + NAME_LEFT,
            head.center().y - galley.size().y / 2.0,
        ),
        galley.size(),
    );
    ui.painter().galley(name_rect.min, galley, theme::TEXT);
    let named = ui.interact(name_rect, ui.id().with("golfer-name"), Sense::hover());
    mark(ui, || "golfer:name".to_owned(), &named);
    let face = Rect::from_center_size(pos2(head.left() + 22.0, head.center().y), vec2(44.0, 44.0));
    let face = ui.interact(face, ui.id().with("golfer-avatar"), Sense::hover());
    mark(ui, || "golfer:avatar".to_owned(), &face);
    let gear = Rect::from_min_size(pos2(head.right() - 20.0, head.top()), vec2(20.0, 20.0));
    let link = ui.interact(gear, ui.id().with("golfer-settings"), Sense::click());
    let lit = link.hovered();
    let tint = if lit {
        Color32::WHITE
    } else {
        theme::TEXT_MUTED
    };
    icons::paint(ui, gear.center(), &icons::NAV_SETTINGS, 20.0, lit, tint);
    let link = link.on_hover_cursor(egui::CursorIcon::PointingHand);
    mark(ui, || "golfer:settings".to_owned(), &link);
    if link.on_hover_text("Golfers in Settings").clicked() {
        picks.settings = true;
    }
    ui.add_space(8.0);
    // A hairline before GOLFERS.
    let line = ui
        .allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover())
        .0;
    ui.painter().hline(
        line.x_range(),
        line.center().y,
        egui::Stroke::new(1.0, theme::BORDER),
    );
    ui.add_space(6.0);
    ui.label(theme::caps("Golfers", 11.0, theme::TEXT_MUTED));
    if golfers.is_empty() {
        ui.label(egui::RichText::new("No golfers").color(theme::TEXT_DIM));
    }
    for g in golfers {
        let on = rows.viewed.as_deref() == Some(g.id.as_str());
        let (rect, response) = golfer_row(ui, &g.name, initial_of(&g.name), on);
        // The game's name at the right end; the gold "up" dot left of it.
        let mut dot_x = rect.right() - 20.0;
        if g.game
            && let Some(game) = crate::panels::settings::golfers::game_name(&g.id)
        {
            let label = ui.painter().text(
                pos2(rect.right() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                game,
                egui::FontId::proportional(11.0),
                theme::TEXT_DIM,
            );
            dot_x = label.left() - 12.0;
        }
        if rows.up_dot.as_deref() == Some(g.id.as_str()) {
            let at = pos2(dot_x, rect.center().y);
            ui.painter().circle_filled(at, 4.0, theme::GOLD);
            let dot = Rect::from_center_size(at, vec2(8.0, 8.0));
            let dot = ui.interact(dot, ui.id().with(("golfer-up", &g.id)), Sense::hover());
            mark(ui, || format!("golfer:up_{}", g.id), &dot);
        }
        mark(ui, || format!("golfer_{}", g.id), &response);
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            *chosen = Some(Chosen::Golfer(g.id.clone()));
        }
    }
    // The view-only rows, after the known golfers.
    for p in &list.past {
        let on = rows.viewed == p.key;
        let (_, response) = golfer_row(
            ui,
            &p.name,
            p.key.as_ref().and_then(|_| initial_of(&p.name)),
            on,
        );
        match &p.key {
            Some(key) => mark(ui, || format!("golfer_{key}"), &response),
            None => mark(ui, || "golfer:none".to_owned(), &response),
        }
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            *chosen = Some(Chosen::View(p.key.clone()));
        }
    }
}

/// One row of the Golfer card's list: the avatar (the ring with `initial`,
/// or the person outline without one) and `name`, drawn selected when `on`.
fn golfer_row(
    ui: &mut egui::Ui,
    name: &str,
    initial: Option<char>,
    on: bool,
) -> (Rect, egui::Response) {
    let height = row_height(ui, 36.0);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if on || response.hovered() {
        let fill = if on {
            theme::SURFACE_HOVER
        } else {
            theme::SURFACE
        };
        ui.painter().rect_filled(rect, 6.0, fill);
    }
    if on {
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(3.0, rect.height() - 12.0)),
            1.5,
            theme::ACCENT,
        );
    }
    avatar(
        ui,
        pos2(rect.left() + 24.0, rect.center().y),
        24.0,
        initial,
        false,
        theme::TEXT_MUTED,
    );
    ui.painter().text(
        pos2(rect.left() + 44.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(14.0),
        theme::TEXT,
    );
    (rect, response)
}

/// A title the bar can show, in preference order.
struct TitleText {
    dot: Color32,
    /// Text, size, color and letter spacing of each part.
    parts: Vec<(String, f32, Color32, f32)>,
}

impl TitleText {
    fn job(&self) -> egui::text::LayoutJob {
        let mut job = egui::text::LayoutJob::default();
        for (text, size, color, spacing) in &self.parts {
            job.append(
                text,
                0.0,
                egui::TextFormat {
                    font_id: egui::FontId::proportional(*size),
                    color: *color,
                    extra_letter_spacing: *spacing,
                    ..Default::default()
                },
            );
        }
        job
    }
}

/// The center title: the brand mark and the version.
fn brand_title() -> TitleText {
    TitleText {
        dot: theme::ACCENT,
        parts: vec![
            ("FLIGHTHOOK".to_owned(), 15.0, theme::ACCENT, 2.5),
            (
                concat!("  v", env!("CARGO_PKG_VERSION")).to_owned(),
                11.0,
                theme::TEXT_DIM,
                0.0,
            ),
        ],
    }
}

#[cfg(test)]
mod tests;
