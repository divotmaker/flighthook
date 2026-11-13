//! The top bar: nav icons on the left (Shots, Cameras, Settings, Log), a
//! title in the center while it fits, and on the right, by role: status and
//! mode (energy, Tournament, Follow turns), actions (Auto-Chip, Shot Type)
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
use crate::types::RoundState;

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
    Tournament,
    AutoChip,
    ShotType,
    Club,
    Golfer,
}

/// A button of the right group, in order from the left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Energy,
    Tournament,
    Follow,
    AutoChip,
    ShotType,
    Club,
    Golfer,
}

/// The tournament control's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Tournament {
    /// No round.
    Off,
    /// The toggle alone holds a round.
    Manual,
    /// A simulator's round, held by the output named.
    Held(Option<String>),
}

impl Tournament {
    /// The state the server's round gives: held while a simulator holds
    /// it, else on while tournament mode is.
    pub(crate) fn of(round: &RoundState) -> Self {
        if round.held {
            Tournament::Held(round.holder.clone())
        } else if round.manual {
            Tournament::Manual
        } else {
            Tournament::Off
        }
    }

    /// Text beside `TOURNAMENT` in the center title, if any.
    fn holder(&self) -> Option<&str> {
        match self {
            Tournament::Off => None,
            Tournament::Manual => Some("Manual"),
            Tournament::Held(holder) => holder.as_deref(),
        }
    }

    /// The icon's hover text.
    fn button_tip(&self) -> String {
        match self {
            Tournament::Off => {
                "Tournament mode: off. Click to turn on.\nWhile it is on, virtual devices are not \
                 allowed."
                    .to_owned()
            }
            Tournament::Manual => {
                "Tournament mode: on. Click to turn off.\nVirtual devices are not allowed."
                    .to_owned()
            }
            Tournament::Held(holder) => format!(
                "Tournament round held by {}.\nIt can't be turned off here. Click for details.",
                holder.as_deref().unwrap_or("a simulator")
            ),
        }
    }

    /// The message a switch to this state shows (no "click" wording: a
    /// finger taps).
    fn switch_text(&self) -> String {
        match self {
            Tournament::Off => "Tournament mode: off.".to_owned(),
            _ => "Tournament mode: on.".to_owned(),
        }
    }

    /// The center title's hover text: why things are off.
    fn title_tip(&self) -> Option<String> {
        match self {
            Tournament::Off => None,
            Tournament::Manual => {
                Some("Tournament mode is on. Virtual devices are not allowed.".to_owned())
            }
            Tournament::Held(holder) => Some(format!(
                "{} tournament round: only shots a launch monitor measured are sent. \
                 Auto-Chip is off until the round ends.",
                holder.as_deref().unwrap_or("A")
            )),
        }
    }
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
        slots.push(Slot::Tournament);
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
                Slot::Tournament => self.tournament_button(ui, rect),
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

    /// The Tournament button: a click toggles tournament mode (one
    /// request); a held round's click opens its card and sends nothing.
    /// The toggle waits while Settings has unsaved edits, so the two never
    /// write the file together.
    fn tournament_button(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let state = Tournament::of(&self.round);
        let held = matches!(state, Tournament::Held(_));
        let locked = !held && self.settings.locked();
        let selected = self.open(HeaderMenu::Tournament);
        let response = hit(ui, rect, "tournament", !locked);
        background(ui, rect, &response, selected);
        let (icon, on) = match &state {
            Tournament::Off => (&icons::TOURNAMENT_OFF, false),
            Tournament::Manual => (&icons::TOURNAMENT_ON, true),
            Tournament::Held(_) => (&icons::TOURNAMENT_HELD, true),
        };
        button_icon(ui, rect, &response, selected, on, icon);
        let text = if locked {
            LOCKED_REASON.to_owned()
        } else {
            state.button_tip()
        };
        let response = tip(response, text);
        if response.clicked() {
            if held {
                self.toggle_menu(HeaderMenu::Tournament);
            } else {
                let on = state == Tournament::Off;
                self.set_tournament(ui.ctx(), on);
                let asked = if on {
                    Tournament::Manual
                } else {
                    Tournament::Off
                };
                self.toast(ui.ctx(), asked.switch_text());
            }
        }
        if !held && self.open(HeaderMenu::Tournament) {
            self.header_menu = None;
        }
        if held {
            let manual = self.round.manual;
            let holder = state.holder().unwrap_or("a simulator").to_owned();
            self.dropdown(HeaderMenu::Tournament, &response, |ui| {
                ui.set_width(380.0);
                ui.label(theme::caps("Tournament round", 11.0, theme::TEXT_MUTED));
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, theme::GOLD);
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(egui::RichText::new("Held by").color(theme::TEXT));
                    ui.label(egui::RichText::new(&holder).strong().color(theme::GOLD));
                });
                ui.separator();
                ui.label(theme::caps("While it lasts", 11.0, theme::TEXT_MUTED));
                for line in [
                    "Auto-Chip is off".to_owned(),
                    "Trackball and Mock hold their shots".to_owned(),
                    format!("{holder} gets only measured shots"),
                ] {
                    ui.label(format!("\u{2022} {line}"));
                }
                ui.label(
                    egui::RichText::new(format!("It ends when {holder} ends it."))
                        .color(theme::TEXT_DIM),
                );
                if manual {
                    ui.label(
                        egui::RichText::new("Tournament mode is also on and stays on afterward.")
                            .color(theme::TEXT_DIM),
                    );
                }
            });
        }
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
        let blocked = auto_chip_blocked(&self.round, &self.actors, self.auto_chip_in_flight);
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
        let state = Tournament::of(&self.round);
        let candidates = title_candidates(&state);
        for candidate in candidates {
            let galley = ui.painter().layout_job(candidate.job());
            let dot = 10.0;
            let gap = 6.0;
            let width = dot + gap + galley.size().x;
            if !title_fits(bar, left_end, right_start, width) {
                continue;
            }
            let rect = Rect::from_center_size(bar.center(), vec2(width, galley.size().y.max(dot)));
            ui.painter().circle_filled(
                pos2(rect.left() + dot / 2.0, rect.center().y),
                4.0,
                candidate.dot,
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
            if let Some(text) = state.title_tip() {
                response.on_hover_text(text);
            }
            return;
        }
    }

    /// Carry out what the bar picked.
    fn apply_picks(&mut self, ctx: &egui::Context, picks: Picks) {
        if let Some(mode) = picks.mode {
            self.current_mode = mode.to_string();
            if let Some(tx) = &mut self.ws_sender {
                net::send_command(tx, &net::select_command(|s| s.mode = Some(mode)));
            } else {
                net::post_mode(ctx, &mode.to_string());
            }
        }
        if let Some(club) = picks.club {
            self.pick_club(ctx, club);
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

/// The titles for `state`, the fullest first: the brand mark, or
/// `TOURNAMENT · holder` and then `TOURNAMENT` during a round.
fn title_candidates(state: &Tournament) -> Vec<TitleText> {
    let tournament = |holder: Option<&str>| {
        let mut parts = vec![("TOURNAMENT".to_owned(), 15.0, theme::GOLD, 2.5)];
        if let Some(holder) = holder {
            parts.push((format!("  \u{b7} {holder}"), 15.0, theme::TEXT_MUTED, 0.0));
        }
        TitleText {
            dot: theme::GOLD,
            parts,
        }
    };
    match state {
        Tournament::Off => vec![TitleText {
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
        }],
        other => {
            let mut out = Vec::new();
            if let Some(holder) = other.holder() {
                out.push(tournament(Some(holder)));
            }
            out.push(tournament(None));
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::devices::Marks;
    use crate::types::{ActorStatus, ActorStatusResponse, FlighthookConfig};
    use egui::Event;
    use std::collections::HashMap;

    /// What one pass of the bar drew.
    struct Pass {
        marks: Marks,
        /// Every text and its color.
        texts: Vec<(String, Color32)>,
        /// Vertical hairlines drawn (dividers).
        dividers: usize,
        /// Where the area under the bar starts.
        below: f32,
        /// Where the area above a bottom bar ends.
        above: f32,
        /// Every rect filled with the accent color (selection bars, the
        /// nav marker).
        accents: Vec<Rect>,
        /// The y of every horizontal hairline longer than 100 px.
        rules: Vec<f32>,
        /// Every icon drawn: its file name, tint and rect.
        images: Vec<(String, Color32, Rect)>,
        /// Every text and where it was drawn.
        text_rects: Vec<(String, Rect)>,
    }

    impl Pass {
        fn has_text(&self, text: &str) -> bool {
            self.texts.iter().any(|(t, _)| t == text)
        }

        fn color_of(&self, text: &str) -> Color32 {
            self.texts
                .iter()
                .find(|(t, _)| t == text)
                .unwrap_or_else(|| panic!("{text:?} not drawn: {:?}", self.texts))
                .1
        }

        fn rect(&self, name: &str) -> Rect {
            self.marks
                .get(name)
                .unwrap_or_else(|| panic!("{name} not drawn: {:?}", self.marks.keys()))
                .0
        }

        fn enabled(&self, name: &str) -> bool {
            self.marks[name].1
        }

        /// The tint and rect of the icon drawn from file `file` (no `.png`).
        fn image(&self, file: &str) -> (Color32, Rect) {
            self.images
                .iter()
                .find(|(f, _, _)| f == file)
                .map(|(_, c, r)| (*c, *r))
                .unwrap_or_else(|| panic!("{file} not drawn: {:?}", self.images))
        }

        /// The file name of the icon drawn inside the mark `name`.
        fn icon_in(&self, name: &str) -> (String, Color32) {
            let rect = self.rect(name);
            self.images
                .iter()
                .find(|(_, _, r)| rect.contains_rect(*r))
                .map(|(f, c, _)| (f.clone(), *c))
                .unwrap_or_else(|| panic!("no icon in {name}: {:?}", self.images))
        }

        /// Where text `text` was drawn.
        fn text_rect(&self, text: &str) -> Rect {
            self.text_rects
                .iter()
                .find(|(t, _)| t == text)
                .unwrap_or_else(|| panic!("{text:?} not drawn: {:?}", self.texts))
                .1
        }

        /// Whether an accent bar is drawn inside the mark `name`.
        fn has_accent_in(&self, name: &str) -> bool {
            let rect = self.rect(name);
            self.accents.iter().any(|a| rect.contains_rect(*a))
        }
    }

    fn flatten(shape: &egui::Shape, pass: &mut Pass, names: &HashMap<egui::TextureId, String>) {
        match shape {
            egui::Shape::Vec(inner) => inner.iter().for_each(|s| flatten(s, pass, names)),
            egui::Shape::Rect(r) if is_accent(r.fill) => pass.accents.push(r.rect),
            egui::Shape::Mesh(m) => {
                if let (Some(name), Some(v)) = (names.get(&m.texture_id), m.vertices.first()) {
                    pass.images.push((name.clone(), v.color, m.calc_bounds()));
                }
            }
            egui::Shape::Text(t) => {
                let color = t
                    .galley
                    .job
                    .sections
                    .first()
                    .map_or(t.fallback_color, |s| s.format.color);
                pass.texts.push((t.galley.text().to_owned(), color));
                pass.text_rects
                    .push((t.galley.text().to_owned(), t.visual_bounding_rect()));
            }
            egui::Shape::LineSegment { points, .. } => {
                let (a, b) = (points[0], points[1]);
                if (a.x - b.x).abs() < 0.5 && (a.y - b.y).abs() > 8.0 {
                    pass.dividers += 1;
                }
                if (a.y - b.y).abs() < 0.5 && (a.x - b.x).abs() > 100.0 {
                    pass.rules.push(a.y);
                }
            }
            _ => {}
        }
    }

    /// Whether `fill` is the accent color, also while a popup fades it in.
    fn is_accent(fill: Color32) -> bool {
        if fill.a() == 0 {
            return false;
        }
        let o = fill.to_opaque();
        let near = |x: u8, y: u8| x.abs_diff(y) <= 4;
        near(o.r(), theme::ACCENT.r())
            && near(o.g(), theme::ACCENT.g())
            && near(o.b(), theme::ACCENT.b())
    }

    fn pass_at(
        ctx: &egui::Context,
        app: &mut FlighthookApp,
        width: f32,
        time: f64,
        events: Vec<Event>,
    ) -> Pass {
        ctx.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
            time: Some(time),
            events,
            ..Default::default()
        };
        let (mut below, mut above) = (0.0, 0.0);
        let out = ctx.run(input, |ctx| {
            app.render_header_in_test(ctx);
            below = ctx.available_rect().top();
            above = ctx.available_rect().bottom();
        });
        let mut pass = Pass {
            marks: ctx
                .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
                .unwrap_or_default(),
            texts: Vec::new(),
            dividers: 0,
            below,
            above,
            accents: Vec::new(),
            rules: Vec::new(),
            images: Vec::new(),
            text_rects: Vec::new(),
        };
        let names = icons::loaded(ctx);
        for clipped in &out.shapes {
            flatten(&clipped.shape, &mut pass, &names);
        }
        pass
    }

    /// The bar settled (the icon font loads on the second pass).
    fn settled(app: &mut FlighthookApp, width: f32) -> (egui::Context, Pass) {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, app, width, 0.0, Vec::new());
        let pass = pass_at(&ctx, app, width, 0.0, Vec::new());
        (ctx, pass)
    }

    fn pass(app: &mut FlighthookApp, width: f32) -> Pass {
        settled(app, width).1
    }

    /// Open `menu` and draw it.
    fn open(app: &mut FlighthookApp, menu: HeaderMenu, width: f32) -> Pass {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, app, width, 0.0, Vec::new());
        app.header_menu = Some(menu);
        pass_at(&ctx, app, width, 0.0, Vec::new());
        pass_at(&ctx, app, width, 0.0, Vec::new())
    }

    const ICON_FLAGS: [&str; 4] = ["energy", "follow", "auto-chip", "shot-type"];

    /// An app whose bar shows the icons in `flags` (`ICON_FLAGS` bits).
    fn bar_app(flags: u8) -> FlighthookApp {
        let on = |i: usize| flags & (1 << i) != 0;
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        let mut config = FlighthookConfig::default();
        config.simulation.auto_chip = on(2);
        if on(3) {
            config
                .mevo
                .insert("0".into(), flighthook::MevoSection::default());
        }
        if on(1) {
            config.golfers = vec![
                flighthook::GolferRecord::new("flighthook.e", "Eric"),
                flighthook::GolferRecord::new("flighthook.s", "Sam"),
            ];
            app.turn_golfer = Some("flighthook.e".into());
        }
        app.settings.load_from(&config);
        if on(0) {
            charge(&mut app, "square.0", "Square Omni", 74);
        }
        app
    }

    fn charge(app: &mut FlighthookApp, id: &str, name: &str, pct: u8) {
        let mut actor = ActorStatusResponse::new(name, ActorStatus::Connected);
        actor
            .telemetry
            .insert("battery_pct".into(), pct.to_string());
        app.actors.insert(id.into(), actor);
    }

    const RIGHT_ORDER: [&str; 7] = [
        "energy",
        "tournament",
        "follow",
        "auto-chip",
        "shot-type",
        "club",
        "golfer",
    ];

    /// Every button's rect, at three widths and for every combination of
    /// the icons that come and go: the right group in role order,
    /// anchored to the right edge, 40 px buttons 6 px apart, no divider,
    /// widths 178 / 224 / 270 / 316; the nav the same on the left.
    #[test]
    fn every_button_has_its_place_at_every_width_and_icon_combination() {
        for flags in 0..16u8 {
            for width in [1600.0, 1024.0, 720.0] {
                let mut app = bar_app(flags);
                let p = pass(&mut app, width);
                let at = format!("flags {flags:04b} at {width}");
                let expected: Vec<&str> = RIGHT_ORDER
                    .iter()
                    .copied()
                    .filter(|name| {
                        ICON_FLAGS
                            .iter()
                            .position(|f| f == name)
                            .is_none_or(|i| flags & (1 << i) != 0)
                    })
                    .collect();
                let present: Vec<&str> = RIGHT_ORDER
                    .iter()
                    .copied()
                    .filter(|n| p.marks.contains_key(*n))
                    .collect();
                assert_eq!(present, expected, "{at}");
                let rects: Vec<Rect> = present.iter().map(|n| p.rect(n)).collect();
                for r in &rects {
                    assert_eq!(r.size(), vec2(BUTTON, BUTTON), "{at}");
                    assert_eq!(r.center().y, BAR_HEIGHT / 2.0, "{at}");
                }
                for pair in rects.windows(2) {
                    assert_eq!(pair[1].left() - pair[0].right(), GAP, "{at}");
                }
                assert_eq!(rects.last().unwrap().right(), width - SIDE_MARGIN, "{at}");
                let group = rects.last().unwrap().right() - rects[0].left();
                assert_eq!(group, group_width(expected.len(), BUTTON, GAP), "{at}");
                let widths = [132.0, 178.0, 224.0, 270.0, 316.0];
                assert!(widths.contains(&group), "{at}: {group}");
                let nav: Vec<Rect> = ["shots", "cameras", "settings", "log"]
                    .iter()
                    .map(|n| p.rect(&format!("nav:{n}")))
                    .collect();
                assert_eq!(nav[0].left(), SIDE_MARGIN, "{at}");
                for pair in nav.windows(2) {
                    assert_eq!(pair[1].left() - pair[0].right(), GAP, "{at}");
                }
                assert_eq!(nav[3].right() - nav[0].left(), 178.0, "{at}");
                assert!(nav[3].right() < rects[0].left(), "{at}: the groups touch");
                assert_eq!(p.dividers, 0, "{at}: no divider");
                assert_eq!(p.below, BAR_HEIGHT, "{at}: the bar is one row");
            }
        }
    }

    /// In the portrait layout the top bar is one row of 52 with all seven
    /// right-group buttons (44 px, 6 apart), anchored right while they fit
    /// and always on screen; the nav is a bottom bar of 52 with four equal
    /// tabs, 44 px tall, across the window.
    #[test]
    fn the_portrait_header_is_one_row_over_a_bottom_tab_bar() {
        for width in [360.0, 390.0, 430.0] {
            let mut app = bar_app(0b1111);
            app.portrait = true;
            let p = pass(&mut app, width);
            let at = format!("at {width}");
            assert_eq!(p.below, BAR_HEIGHT, "{at}");
            assert_eq!(p.above, 900.0 - BAR_HEIGHT, "{at}");
            let right: Vec<Rect> = RIGHT_ORDER.iter().map(|n| p.rect(n)).collect();
            for r in &right {
                assert_eq!(r.size(), vec2(TOUCH_TARGET, TOUCH_TARGET), "{at}");
                assert_eq!(r.center().y, BAR_HEIGHT / 2.0, "{at}");
                assert!(r.left() >= 0.0 && r.right() <= width, "{at}: {r:?}");
            }
            for pair in right.windows(2) {
                assert_eq!(pair[1].left() - pair[0].right(), GAP, "{at}");
            }
            let last = right.last().unwrap().right();
            assert_eq!(last, (width - SIDE_MARGIN - 344.0).max(GAP) + 344.0, "{at}");
            let nav: Vec<Rect> = ["shots", "cameras", "settings", "log"]
                .iter()
                .map(|n| p.rect(&format!("nav:{n}")))
                .collect();
            assert_eq!(nav[0].left(), 0.0, "{at}");
            assert_eq!(nav[3].right(), width, "{at}");
            for r in &nav {
                assert_eq!(r.size(), vec2(width / 4.0, TOUCH_TARGET), "{at}");
                assert_eq!(r.center().y, 900.0 - BAR_HEIGHT / 2.0, "{at}");
            }
            for pair in nav.windows(2) {
                assert_eq!(pair[1].left(), pair[0].right(), "{at}");
            }
            // Shots is selected: its marker sits at the tab bar's top edge.
            let marker = pos2(nav[0].center().x, 900.0 - BAR_HEIGHT + 1.5);
            assert!(
                p.accents.iter().any(|a| a.center() == marker),
                "{at}: {:?}",
                p.accents
            );
            assert!(!p.marks.contains_key("title"), "{at}: no room for it");
        }
    }

    /// With few right-group buttons the portrait top bar shows the title,
    /// left of the group and clear of it.
    #[test]
    fn the_portrait_header_shows_the_title_where_it_fits() {
        let mut app = bar_app(0);
        app.portrait = true;
        let p = pass(&mut app, 430.0);
        let title = p.rect("title");
        assert!(title.left() >= TITLE_CLEAR, "{title:?}");
        assert!(
            title.right() + TITLE_CLEAR <= p.rect("club").left(),
            "{title:?}"
        );
        assert_eq!(title.center().y, BAR_HEIGHT / 2.0);
    }

    /// On a touch screen a tap on a disabled tab shows its reason above
    /// the tab, on screen.
    #[test]
    fn a_tap_on_a_disabled_tab_shows_its_reason_above_it() {
        let width = 390.0;
        let mut app = bar_app(0b1000);
        app.portrait = true;
        app.settings.actors[0].set_dirty(true);
        let (ctx, p) = settled(&mut app, width);
        let tab = p.rect("nav:cameras");
        assert!(!p.enabled("nav:cameras"));
        let at = tab.center();
        let press = |pressed: bool| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let finger = Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(0),
            phase: egui::TouchPhase::Start,
            pos: at,
            force: None,
        };
        pass_at(
            &ctx,
            &mut app,
            width,
            0.1,
            vec![finger, Event::PointerMoved(at), press(true)],
        );
        pass_at(
            &ctx,
            &mut app,
            width,
            0.2,
            vec![press(false), Event::PointerGone],
        );
        pass_at(&ctx, &mut app, width, 0.3, Vec::new());
        let p = pass_at(&ctx, &mut app, width, 0.4, Vec::new());
        let tip = p.text_rect(LOCKED_REASON);
        assert!(tip.bottom() < tab.top(), "above the tab: {tip:?} {tab:?}");
        assert!(
            tip.left() >= 0.0 && tip.right() <= width,
            "on screen: {tip:?}"
        );
    }

    /// Below the narrowest supported width (400 logical px here) the right
    /// group starts a gap after the nav and is cut off at the right edge:
    /// the groups never overlap.
    #[test]
    fn a_window_too_narrow_for_both_groups_clips_the_right_group() {
        let mut app = bar_app(0b1111);
        let p = pass(&mut app, 400.0);
        let nav_end = p.rect("nav:log").right();
        for name in RIGHT_ORDER {
            assert!(
                p.rect(name).left() >= nav_end + GAP - 0.01,
                "{name} overlaps the nav: {:?} vs {nav_end}",
                p.rect(name)
            );
        }
        assert!(!p.marks.contains_key("title"));
    }

    /// The narrowest window, in px, at which a title is drawn: found by
    /// widening the window one step at a time.
    fn threshold(flags: u8, want: &str, prepare: impl Fn(&mut FlighthookApp)) -> f32 {
        let mut app = bar_app(flags);
        prepare(&mut app);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, &mut app, 800.0, 0.0, Vec::new());
        let mut width = 520.0;
        loop {
            if pass_at(&ctx, &mut app, width, 0.0, Vec::new())
                .texts
                .iter()
                .any(|(t, _)| t == want || (want != "TOURNAMENT" && t.contains(want)))
            {
                return width;
            }
            width += 1.0;
            assert!(width < 1400.0, "no title ever fits");
        }
    }

    fn held(holder: &str) -> impl Fn(&mut FlighthookApp) {
        let holder = holder.to_owned();
        move |app: &mut FlighthookApp| {
            app.round = RoundState {
                active: true,
                holder: Some(holder.clone()),
                held: true,
                manual: false,
            };
        }
    }

    /// Tournament mode on, as the server reports it.
    fn manual(app: &mut FlighthookApp) {
        app.round = RoundState {
            active: true,
            holder: Some("Manual".into()),
            held: false,
            manual: true,
        };
    }

    /// Widths here are logical px (the window at zoom 1; the app zooms out
    /// below a 1024 px window, so a narrower window has more logical px).
    /// The title is there at 1024 and gone at 720, and the narrowest
    /// width each title first fits in, per right group, is the measured
    /// one within 3 px.
    #[test]
    fn the_title_shows_at_1024_not_at_720_logical_px_and_fits_by_the_measured_widths() {
        for flags in [0b1100u8, 0b1110, 0b1101, 0b1111] {
            let mut app = bar_app(flags);
            assert!(
                pass(&mut app, 1024.0).marks.contains_key("title"),
                "{flags}"
            );
            // At 720 only the narrower groups leave room for it.
            let at_720 = pass(&mut app, 720.0).marks.contains_key("title");
            assert_eq!(at_720, flags == 0b1100, "{flags:04b} at 720");
        }
        // (flags: auto-chip + shot type, + follow, + energy, all)
        let rows = [
            (0b1100u8, [690.0, 721.0, 790.0, 659.0]),
            (0b1110, [782.0, 813.0, 882.0, 751.0]),
            (0b1101, [782.0, 813.0, 882.0, 751.0]),
            (0b1111, [874.0, 905.0, 974.0, 843.0]),
        ];
        // The brand rows were measured with a four-digit version ("v0.1.26");
        // each digit more or less moves them by a digit's width.
        let digits = env!("CARGO_PKG_VERSION")
            .chars()
            .filter(char::is_ascii_digit)
            .count();
        #[allow(clippy::cast_precision_loss)]
        let version_shift = (digits as f32 - 4.0) * 7.0;
        for (flags, [brand, manual, pureplay, short]) in rows {
            let measured = [
                (
                    "brand",
                    threshold(flags, "FLIGHTHOOK", |_| {}),
                    brand + version_shift,
                ),
                ("manual", threshold(flags, "Manual", self::manual), manual),
                (
                    "held",
                    threshold(flags, "Pureplay Connect", held("Pureplay Connect")),
                    pureplay,
                ),
                (
                    "short",
                    threshold(flags, "TOURNAMENT", held("Pureplay Connect")),
                    short,
                ),
            ];
            for (name, got, want) in measured {
                assert!(
                    (got - want).abs() <= 3.0,
                    "flags {flags:04b} {name}: first fits at {got} logical px, want {want}"
                );
            }
        }
    }

    /// The window a title of width `title` first fits in: 24 px clear of
    /// the right group on the half of the bar it sits on.
    #[test]
    fn the_narrowest_window_follows_the_clearance_rule() {
        for flags in [0b1100u8, 0b1111] {
            let n = 3 + flags.count_ones() as usize;
            let mut app = bar_app(flags);
            let wide = pass(&mut app, 1600.0);
            let title = wide.rect("title").width();
            let found = threshold(flags, "FLIGHTHOOK", |_| {});
            let rule =
                2.0 * (SIDE_MARGIN + group_width(n, BUTTON, GAP) + TITLE_CLEAR + title / 2.0);
            assert!(
                (found - rule).abs() <= 1.5,
                "{flags:04b}: {found} vs {rule}"
            );
        }
    }

    /// The title keeps 24 px clear of both groups and sits at the true
    /// center of the bar.
    #[test]
    fn the_title_is_centered_and_keeps_clear_of_the_groups() {
        let mut app = bar_app(0b1111);
        for width in [900.0, 1024.0, 1600.0] {
            let p = pass(&mut app, width);
            let title = p.rect("title");
            assert!((title.center().x - width / 2.0).abs() < 0.01, "{width}");
            assert!(
                title.left() - p.rect("nav:log").right() >= TITLE_CLEAR,
                "{width}"
            );
            assert!(
                p.rect("energy").left() - title.right() >= TITLE_CLEAR,
                "{width}"
            );
        }
    }

    /// A round shows in the title: `TOURNAMENT · holder`, `· Manual` for
    /// the toggle alone, and no banner strip under the bar.
    #[test]
    fn a_round_shows_in_the_title_with_no_banner() {
        let mut app = bar_app(0);
        let p = pass(&mut app, 1600.0);
        assert!(p.texts.iter().any(|(t, _)| t.starts_with("FLIGHTHOOK")));
        let idle_below = p.below;

        held("Pureplay Connect")(&mut app);
        let p = pass(&mut app, 1600.0);
        assert!(
            p.has_text("TOURNAMENT  \u{b7} Pureplay Connect"),
            "{:?}",
            p.texts
        );
        assert_eq!(
            p.color_of("TOURNAMENT  \u{b7} Pureplay Connect"),
            theme::GOLD
        );
        assert!(!p.texts.iter().any(|(t, _)| t.starts_with("FLIGHTHOOK")));
        assert_eq!(p.below, idle_below, "no banner strip");

        manual(&mut app);
        let p = pass(&mut app, 1600.0);
        assert!(p.has_text("TOURNAMENT  \u{b7} Manual"), "{:?}", p.texts);

        // Out of room, the holder goes first, then the title.
        let mut app = bar_app(0b1111);
        held("Pureplay Connect")(&mut app);
        let narrow = threshold(0b1111, "TOURNAMENT", held("Pureplay Connect"));
        let p = pass(&mut app, narrow + 1.0);
        assert!(p.has_text("TOURNAMENT"), "{:?}", p.texts);
        assert!(!p.has_text("TOURNAMENT  \u{b7} Pureplay Connect"));
        let p = pass(&mut app, 720.0);
        assert!(!p.marks.contains_key("title"));
        assert!(p.marks.contains_key("tournament"), "the icon carries it");
    }

    /// The Tournament button's three states, read from the round: its
    /// glyph and color, and the hover text.
    #[test]
    fn the_tournament_button_shows_off_manual_and_held() {
        let mut app = bar_app(0);
        let p = pass(&mut app, 1600.0);
        assert_eq!(
            p.icon_in("tournament"),
            ("tournament-off-normal@1x".to_owned(), theme::TEXT_MUTED)
        );

        // On is drawn in the hover style, untinted.
        manual(&mut app);
        let p = pass(&mut app, 1600.0);
        assert_eq!(
            p.icon_in("tournament"),
            ("tournament-on-hover@1x".to_owned(), Color32::WHITE)
        );

        // Held: the trophy with the lock badge.
        held("Pureplay Connect")(&mut app);
        app.round.manual = true;
        let p = pass(&mut app, 1600.0);
        assert_eq!(
            p.icon_in("tournament"),
            ("tournament-held-hover@1x".to_owned(), Color32::WHITE)
        );
        // A click opens the card naming the holder and the effects.
        let p = open(&mut app, HeaderMenu::Tournament, 1600.0);
        // Only the holder's name is gold.
        assert_eq!(p.color_of("Held by"), theme::TEXT);
        assert_eq!(p.color_of("Pureplay Connect"), theme::GOLD);
        for text in [
            "\u{2022} Auto-Chip is off",
            "\u{2022} Trackball and Mock hold their shots",
            "\u{2022} Pureplay Connect gets only measured shots",
            "It ends when Pureplay Connect ends it.",
        ] {
            assert!(p.has_text(text), "{text}: {:?}", p.texts);
        }
        assert!(p.has_text("Tournament mode is also on and stays on afterward."));
    }

    #[test]
    fn the_tournament_states_have_their_hover_texts() {
        let off = Tournament::of(&RoundState::default());
        assert_eq!(
            off.button_tip(),
            "Tournament mode: off. Click to turn on.\nWhile it is on, virtual devices are not \
             allowed."
        );
        assert_eq!(off.title_tip(), None);
        let mut app = bar_app(0);
        manual(&mut app);
        let manual = Tournament::of(&app.round);
        assert_eq!(
            manual.button_tip(),
            "Tournament mode: on. Click to turn off.\nVirtual devices are not allowed."
        );
        assert_eq!(
            manual.title_tip().as_deref(),
            Some("Tournament mode is on. Virtual devices are not allowed.")
        );
        let round = RoundState {
            active: true,
            holder: Some("Pureplay Connect".into()),
            held: true,
            manual: true,
        };
        // A simulator's round wins over the toggle.
        let held = Tournament::of(&round);
        assert_eq!(held, Tournament::Held(Some("Pureplay Connect".into())));
        assert_eq!(
            held.button_tip(),
            "Tournament round held by Pureplay Connect.\nIt can't be turned off here. Click \
             for details."
        );
        assert_eq!(
            held.title_tip().as_deref(),
            Some(
                "Pureplay Connect tournament round: only shots a launch monitor measured are \
                 sent. Auto-Chip is off until the round ends."
            )
        );
    }

    /// Off and on (manual), one click switches tournament mode with one
    /// request; the icon follows the server's round, not the click.
    #[test]
    fn one_click_toggles_tournament_mode_with_one_request() {
        let mut app = bar_app(0);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
            .rect("tournament")
            .center();
        click(&ctx, &mut app, at);
        assert_eq!(app.tournament_sent, [true], "one request to turn it on");
        assert_eq!(app.header_menu, None, "no popup");
        assert!(!app.round.active, "the server's round decides the state");
        manual(&mut app);
        click(&ctx, &mut app, at);
        assert_eq!(
            app.tournament_sent,
            [true, false],
            "one request to turn it off"
        );
        assert_eq!(app.header_menu, None, "no popup");
    }

    /// While a simulator holds the round, a click opens the locked card
    /// and sends nothing; a second click closes it.
    #[test]
    fn a_click_on_a_held_round_opens_the_card_and_sends_nothing() {
        let mut app = bar_app(0);
        held("Pureplay Connect")(&mut app);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
            .rect("tournament")
            .center();
        click(&ctx, &mut app, at);
        assert_eq!(app.header_menu, Some(HeaderMenu::Tournament));
        let p = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        assert!(p.has_text("Held by"), "{:?}", p.texts);
        assert!(
            !p.has_text("Tournament mode is also on and stays on afterward."),
            "the extra line only with tournament mode on too"
        );
        assert!(app.tournament_sent.is_empty(), "nothing sent");
        click(&ctx, &mut app, at);
        assert_eq!(app.header_menu, None);
        assert!(app.tournament_sent.is_empty(), "still nothing sent");
    }

    /// The toast's text, if one shows.
    fn toast_text(app: &FlighthookApp) -> Option<&str> {
        app.toast.as_ref().map(|(text, _)| text.as_str())
    }

    /// A tournament switch shows a toast saying the state it asked for,
    /// drawn near the bottom of the content.
    #[test]
    fn a_tournament_switch_shows_a_toast_of_the_state_asked_for() {
        let mut app = bar_app(0);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
            .rect("tournament")
            .center();
        click(&ctx, &mut app, at);
        let on = Tournament::Manual.switch_text();
        assert_eq!(toast_text(&app), Some(on.as_str()));
        manual(&mut app);
        click(&ctx, &mut app, at);
        let off = Tournament::Off.switch_text();
        assert_eq!(
            toast_text(&app),
            Some(off.as_str()),
            "the new toast replaces the old"
        );
        let p = pass_at(&ctx, &mut app, 1600.0, 1.0, Vec::new());
        assert!(p.has_text(&off) && !p.has_text(&on), "{:?}", p.texts);
        let rect = p.text_rect(&off);
        assert!(
            rect.bottom() > 700.0 && rect.width() <= TOAST_WIDTH,
            "{rect:?}"
        );
        assert!((rect.center().x - 800.0).abs() < 8.0, "centered: {rect:?}");
    }

    /// A held round's click (it opens the card) and a locked click show no
    /// toast, and neither does the round changing on its own.
    #[test]
    fn a_held_or_locked_tournament_click_shows_no_toast() {
        let mut app = bar_app(0);
        held("Pureplay Connect")(&mut app);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
            .rect("tournament")
            .center();
        click(&ctx, &mut app, at);
        assert_eq!(app.header_menu, Some(HeaderMenu::Tournament));
        assert_eq!(toast_text(&app), None, "held");
        manual(&mut app);
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        assert_eq!(toast_text(&app), None, "the round changed on its own");

        let mut app = bar_app(0b1000);
        app.settings.actors[0].set_dirty(true);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
            .rect("tournament")
            .center();
        click(&ctx, &mut app, at);
        assert_eq!(toast_text(&app), None, "locked");
    }

    /// A Follow turns switch shows a toast naming the new state.
    #[test]
    fn a_follow_switch_shows_a_toast_of_the_new_state() {
        let mut app = golfers_app();
        let (ctx, p) = settled(&mut app, 1600.0);
        let at = p.rect("follow").center();
        click(&ctx, &mut app, at);
        assert!(!app.follow_turns);
        assert_eq!(toast_text(&app), Some("Following turns: off."));
        click(&ctx, &mut app, at);
        assert_eq!(toast_text(&app), Some("Following turns: on."));
    }

    /// The toast shows for its time and the fade, then is gone. On a phone
    /// it is centered above the tab bar, no wider than the screen.
    #[test]
    fn the_toast_is_gone_after_its_time() {
        let mut app = golfers_app();
        app.portrait = true;
        let (ctx, p) = settled(&mut app, 390.0);
        click_at(&ctx, &mut app, 390.0, p.rect("follow").center());
        let text = app.follow_switch_text().to_owned();
        for time in [1.0, 2.4, 2.7] {
            let p = pass_at(&ctx, &mut app, 390.0, time, Vec::new());
            assert!(p.has_text(&text), "{time}: {:?}", p.texts);
            let rect = p.text_rect(&text);
            let tab = p.rect("nav:shots");
            assert!(rect.bottom() < tab.top(), "{rect:?} {tab:?}");
            assert!(rect.left() > 0.0 && rect.right() < 390.0, "{rect:?}");
        }
        let p = pass_at(&ctx, &mut app, 390.0, TOAST_SHOW + TOAST_FADE, Vec::new());
        assert!(!p.has_text(&text), "{:?}", p.texts);
        assert_eq!(toast_text(&app), None);
    }

    /// The Auto-Chip icon is disabled in both on states, with the reason
    /// as its hover text.
    #[test]
    fn auto_chip_is_disabled_in_both_on_states_with_its_reason() {
        let mut app = bar_app(0b0100);
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", ActorStatus::Connected),
        );
        assert!(pass(&mut app, 1600.0).enabled("auto-chip"));
        manual(&mut app);
        assert!(!pass(&mut app, 1600.0).enabled("auto-chip"));
        let texts = hover_texts(&mut app, "auto-chip");
        assert!(
            texts.iter().any(|t| t.starts_with("Tournament mode is on")),
            "{texts:?}"
        );
        held("Pureplay Connect")(&mut app);
        assert!(!pass(&mut app, 1600.0).enabled("auto-chip"));
        let texts = hover_texts(&mut app, "auto-chip");
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("Pureplay Connect tournament round")),
            "{texts:?}"
        );
    }

    /// On a touch screen a tap on the disabled Auto-Chip shows its reason
    /// under it until the next tap anywhere; without a touch it does not.
    #[test]
    fn a_tap_on_disabled_auto_chip_shows_its_reason_until_the_next_tap() {
        let reason = |p: &Pass| {
            p.texts
                .iter()
                .any(|(t, _)| t.starts_with("Tournament mode is on"))
        };
        for touch in [true, false] {
            let mut app = bar_app(0b0100);
            app.actors.insert(
                "openconnect_client.0".into(),
                ActorStatusResponse::new("GSPro", ActorStatus::Connected),
            );
            manual(&mut app);
            let (ctx, p) = settled(&mut app, 390.0);
            let at = p.rect("auto-chip").center();
            let press = |pos: Pos2, pressed: bool| Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let finger = |phase| Event::Touch {
                device_id: egui::TouchDeviceId(0),
                id: egui::TouchId(0),
                phase,
                pos: at,
                force: None,
            };
            let mut down = vec![Event::PointerMoved(at), press(at, true)];
            if touch {
                down.insert(0, finger(egui::TouchPhase::Start));
            }
            pass_at(&ctx, &mut app, 390.0, 0.1, down);
            pass_at(
                &ctx,
                &mut app,
                390.0,
                0.2,
                vec![press(at, false), Event::PointerGone],
            );
            let p = pass_at(&ctx, &mut app, 390.0, 0.3, Vec::new());
            assert_eq!(reason(&p), touch, "touch {touch}: shown after the tap");
            let p = pass_at(&ctx, &mut app, 390.0, 5.0, Vec::new());
            assert_eq!(reason(&p), touch, "touch {touch}: stays");
            let away = pos2(200.0, 600.0);
            pass_at(
                &ctx,
                &mut app,
                390.0,
                5.1,
                vec![Event::PointerMoved(away), press(away, true)],
            );
            pass_at(
                &ctx,
                &mut app,
                390.0,
                5.2,
                vec![press(away, false), Event::PointerGone],
            );
            let p = pass_at(&ctx, &mut app, 390.0, 5.3, Vec::new());
            assert!(!reason(&p), "touch {touch}: hidden by the next tap");
        }
    }

    /// Unsaved Settings edits disable the toggle with the nav lock's
    /// reason, in both off and on; a click sends nothing.
    #[test]
    fn unsaved_settings_edits_disable_the_tournament_toggle() {
        let mut app = bar_app(0b1000);
        app.settings.actors[0].set_dirty(true);
        assert!(app.settings.locked());
        for on in [false, true] {
            if on {
                manual(&mut app);
            }
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let p = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
            assert!(!p.enabled("tournament"), "on: {on}");
            click(&ctx, &mut app, p.rect("tournament").center());
            assert!(app.tournament_sent.is_empty(), "on: {on}");
            let texts = hover_texts(&mut app, "tournament");
            assert!(texts.iter().any(|t| t == LOCKED_REASON), "{texts:?}");
        }
    }

    /// Auto-Chip: a titled dropdown of 1, 3 and 5 Yard; a pick sends the
    /// chip as before.
    #[test]
    fn the_auto_chip_dropdown_is_titled_and_lists_the_distances() {
        let mut app = bar_app(0b0100);
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", ActorStatus::Connected),
        );
        let p = open(&mut app, HeaderMenu::AutoChip, 1600.0);
        assert!(p.has_text("AUTO-CHIP"));
        let ys: Vec<f32> = ["1 Yard", "3 Yard", "5 Yard"]
            .iter()
            .map(|l| p.rect(&format!("auto-chip:{l}")).top())
            .collect();
        assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
        assert!(p.rect("auto-chip:1 Yard").top() > p.rect("auto-chip").bottom());
        // It is blocked, with its reason, during a round: no dropdown.
        held("Pureplay Connect")(&mut app);
        let p = open(&mut app, HeaderMenu::AutoChip, 1600.0);
        assert!(!p.enabled("auto-chip"));
        assert!(!p.marks.contains_key("auto-chip:1 Yard"));
    }

    /// Shot Type's dropdown: a title, the (i) tip and Full, Chip and Putt
    /// with the current one first-class; a pick sets the shot type.
    #[test]
    fn the_shot_type_tip_opens_left_of_the_dropdown() {
        for width in [1600.0, 1024.0, 400.0] {
            let mut app = bar_app(0b1000);
            let ctx = egui::Context::default();
            theme::install(&ctx);
            pass_at(&ctx, &mut app, width, 0.0, Vec::new());
            app.header_menu = Some(HeaderMenu::ShotType);
            pass_at(&ctx, &mut app, width, 0.0, Vec::new());
            let p = pass_at(&ctx, &mut app, width, 0.0, Vec::new());
            assert!(!p.marks.contains_key("shot-type:tip-text"), "only on hover");
            let at = p.rect("shot-type:tip").center();
            pass_at(&ctx, &mut app, width, 0.1, vec![Event::PointerMoved(at)]);
            let p = pass_at(&ctx, &mut app, width, 0.2, Vec::new());
            let tip = p.rect("shot-type:tip-text");
            let title = p.rect("shot-type:tip");
            assert!(!tip.intersects(title), "{width}: covers the title");
            for label in ["Full", "Chip", "Putt"] {
                let row = p.rect(&format!("shot-type:{label}"));
                if width > 1000.0 {
                    assert!(tip.right() < row.left(), "{width}: left of {label}");
                } else {
                    assert!(tip.top() > row.bottom(), "{width}: under {label}");
                }
                assert!(!tip.intersects(row), "{width}: covers {label}");
            }
            assert!(
                tip.left() >= 0.0 && tip.right() <= width,
                "{width}: on screen: {tip:?}"
            );
            assert!(tip.height() < 70.0, "{width}: two lines: {tip:?}");
        }
    }

    /// In the portrait layout the dropdowns' rows and club chips are 44 px
    /// tall, the Golfer card takes the screen's width less the margins, and
    /// the Shot Type tip opens under the (i).
    #[test]
    fn portrait_dropdowns_are_touch_sized_and_on_screen() {
        let width = 390.0;
        let mut app = golfers_app();
        app.portrait = true;
        let p = open(&mut app, HeaderMenu::Club, width);
        for club in ["club_DR", "club_7I", "club_PW"] {
            assert_eq!(p.rect(club).height(), TOUCH_TARGET, "{club}");
        }
        app.header_menu = None;
        let p = open(&mut app, HeaderMenu::Golfer, width);
        for id in ["flighthook.e", "flighthook.s"] {
            let row = p.rect(&format!("golfer_{id}"));
            assert_eq!(row.height(), TOUCH_TARGET, "{id}");
            assert!(row.left() >= SIDE_MARGIN && row.right() <= width - SIDE_MARGIN);
            assert!(row.width() > 300.0, "{id}: the card's full width: {row:?}");
        }

        let mut app = bar_app(0b1000);
        app.portrait = true;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, &mut app, width, 0.0, Vec::new());
        app.header_menu = Some(HeaderMenu::ShotType);
        pass_at(&ctx, &mut app, width, 0.0, Vec::new());
        let p = pass_at(&ctx, &mut app, width, 0.0, Vec::new());
        for label in ["Full", "Chip", "Putt"] {
            let row = p.rect(&format!("shot-type:{label}"));
            assert_eq!(row.height(), TOUCH_TARGET, "{label}");
        }
        let info = p.rect("shot-type:tip");
        pass_at(
            &ctx,
            &mut app,
            width,
            0.1,
            vec![Event::PointerMoved(info.center())],
        );
        let p = pass_at(&ctx, &mut app, width, 0.2, Vec::new());
        let tip = p.rect("shot-type:tip-text");
        assert!(tip.top() > info.bottom(), "under the (i): {tip:?} {info:?}");
        assert!(
            tip.left() >= 0.0 && tip.right() <= width,
            "on screen: {tip:?}"
        );
    }

    /// On a touch screen a tap on the Shot Type (i) shows its tip until the
    /// next press; the dropdown stays open.
    #[test]
    fn a_tap_on_the_shot_type_info_shows_its_tip() {
        let width = 390.0;
        let mut app = bar_app(0b1000);
        app.portrait = true;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, &mut app, width, 0.0, Vec::new());
        app.header_menu = Some(HeaderMenu::ShotType);
        pass_at(&ctx, &mut app, width, 0.0, Vec::new());
        let p = pass_at(&ctx, &mut app, width, 0.0, Vec::new());
        let at = p.rect("shot-type:tip").center();
        let press = |pos: Pos2, pressed: bool| Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let finger = Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(0),
            phase: egui::TouchPhase::Start,
            pos: at,
            force: None,
        };
        pass_at(
            &ctx,
            &mut app,
            width,
            0.1,
            vec![finger, Event::PointerMoved(at), press(at, true)],
        );
        pass_at(
            &ctx,
            &mut app,
            width,
            0.2,
            vec![press(at, false), Event::PointerGone],
        );
        let p = pass_at(&ctx, &mut app, width, 0.3, Vec::new());
        assert!(
            p.marks.contains_key("shot-type:tip-text"),
            "shown after the tap"
        );
        assert_eq!(app.header_menu, Some(HeaderMenu::ShotType));
        let p = pass_at(&ctx, &mut app, width, 5.0, Vec::new());
        assert!(p.marks.contains_key("shot-type:tip-text"), "stays");
        let row = p.rect("shot-type:Full").center();
        pass_at(
            &ctx,
            &mut app,
            width,
            5.1,
            vec![Event::PointerMoved(row), press(row, true)],
        );
        let p = pass_at(&ctx, &mut app, width, 5.2, Vec::new());
        assert!(
            !p.marks.contains_key("shot-type:tip-text"),
            "hidden by the next press"
        );
    }

    #[test]
    fn the_shot_type_dropdown_lists_full_chip_and_putt() {
        let mut app = bar_app(0b1000);
        app.current_mode = "chipping".into();
        let p = open(&mut app, HeaderMenu::ShotType, 1600.0);
        assert!(p.has_text("SHOT TYPE"));
        assert!(p.marks.contains_key("shot-type:tip"));
        let ys: Vec<f32> = ["Full", "Chip", "Putt"]
            .iter()
            .map(|l| p.rect(&format!("shot-type:{l}")).top())
            .collect();
        assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
        // The button shows the current type's icon, lit while open.
        assert_eq!(p.icon_in("shot-type").0, "shot-type-chip-hover@1x");
        // A pick closes the dropdown and changes the type.
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        app.header_menu = Some(HeaderMenu::ShotType);
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
            .rect("shot-type:Putt")
            .center();
        click(&ctx, &mut app, at);
        assert_eq!(app.current_mode, "putting");
        assert_eq!(app.header_menu, None);
    }

    /// A click at `at`: move, press, release.
    fn click(ctx: &egui::Context, app: &mut FlighthookApp, at: Pos2) {
        click_at(ctx, app, 1600.0, at);
    }

    /// A click at `at` on a window `width` wide.
    fn click_at(ctx: &egui::Context, app: &mut FlighthookApp, width: f32, at: Pos2) {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        pass_at(ctx, app, width, 0.0, vec![Event::PointerMoved(at)]);
        pass_at(ctx, app, width, 0.0, vec![button(true)]);
        pass_at(ctx, app, width, 0.0, vec![button(false)]);
        pass_at(ctx, app, width, 0.0, Vec::new());
    }

    fn golfers_app() -> FlighthookApp {
        let mut app = bar_app(0b0010);
        let mut config = app.settings.saved_config_ref().cloned().unwrap();
        let mut eric = flighthook::GolferRecord::new("flighthook.e", "Eric");
        eric.bag = vec![Club::Driver, Club::PitchingWedge];
        config.golfers = vec![eric, flighthook::GolferRecord::new("flighthook.s", "Sam")];
        app.settings.load_from(&config);
        app.selected_club = Some(Club::Iron7);
        app
    }

    /// The Club dropdown: a title and the club pills, the golfer up's bag
    /// with the club in play always shown; the button shows the code.
    #[test]
    fn the_club_dropdown_is_titled_and_follows_the_bag() {
        let mut app = golfers_app();
        let p = pass(&mut app, 1600.0);
        assert!(p.has_text("7I"), "the code in the slot");
        let p = open(&mut app, HeaderMenu::Club, 1600.0);
        assert!(p.has_text("CLUB"));
        for club in ["club_DR", "club_7I", "club_PW"] {
            assert!(p.marks.contains_key(club), "{club}");
        }
        assert!(!p.marks.contains_key("club_SW"), "not in the bag");
        // No club: the iron with a dash, no code.
        app.selected_club = None;
        app.header_menu = None;
        let p = pass(&mut app, 1600.0);
        assert_eq!(p.icon_in("club").0, "club-none-normal@1x");
        assert!(!p.has_text("7I"));
    }

    /// The Golfer card: the golfer's name and initial, every golfer, a
    /// link to Settings; no hand control.
    #[test]
    fn the_golfer_card_holds_the_name_and_the_list() {
        let mut app = golfers_app();
        let p = pass(&mut app, 1600.0);
        assert!(p.has_text("E"), "the initial: {:?}", p.texts);
        let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
        assert!(p.has_text("Eric") && p.has_text("Sam"), "{:?}", p.texts);
        for text in ["Right-handed", "Left-handed"] {
            assert!(!p.has_text(text), "{text}: {:?}", p.texts);
        }
        assert!(!p.marks.contains_key("hand"));
        assert!(!p.marks.contains_key("golfer:hand-caption"));
        assert!(p.marks.contains_key("golfer_flighthook.e"));
        assert!(p.marks.contains_key("golfer_flighthook.s"));
        assert!(p.marks.contains_key("golfer:settings"));
        // The link opens Settings and closes the card.
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        app.header_menu = Some(HeaderMenu::Golfer);
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
            .rect("golfer:settings")
            .center();
        app.active_tab = Tab::Shots;
        click(&ctx, &mut app, at);
        assert_eq!(app.active_tab, Tab::Devices);
        assert_eq!(
            app.settings_section,
            crate::panels::settings_nav::SettingsSection::Golfers,
            "Settings opens on Golfers"
        );
        assert_eq!(app.header_menu, None);
        // With no golfer: "No golfer" and "No golfers".
        let mut none = bar_app(0);
        let p = open(&mut none, HeaderMenu::Golfer, 1600.0);
        assert!(
            p.has_text("No golfer") && p.has_text("No golfers"),
            "{:?}",
            p.texts
        );
    }

    /// A long golfer name wraps inside the Golfer card, between the avatar
    /// and the gear, still centered on the avatar; never out of the card
    /// or the window.
    #[test]
    fn a_long_golfer_name_wraps_inside_the_card() {
        const LONG: &str = "Christopher Montgomery-Wellington III";
        for width in [1600.0, 400.0] {
            let mut app = golfers_app();
            let mut config = app.settings.saved_config_ref().cloned().unwrap();
            config.golfers[0].name = LONG.to_owned();
            app.settings.load_from(&config);
            let p = open(&mut app, HeaderMenu::Golfer, width);
            assert!(
                p.texts.iter().any(|t| t.0.contains("Christopher")),
                "{width}: the name"
            );
            let avatar = p.rect("golfer:avatar");
            let name = p.rect("golfer:name");
            let gear = p.rect("golfer:settings");
            let row = p.rect("golfer_flighthook.e");
            assert!(name.left() > avatar.right(), "{width}: right of the avatar");
            assert!(
                name.right() <= gear.left() && name.right() <= row.right(),
                "{width}: inside the card, clear of the gear: {name:?} {gear:?} {row:?}"
            );
            assert!(name.right() <= width, "{width}: inside the window");
            assert!(
                name.height() > 30.0,
                "{width}: wraps to a second line: {name:?}"
            );
            assert!(
                (name.center().y - avatar.center().y).abs() <= 1.0,
                "{width}: centered on the avatar"
            );
            assert!(
                row.top() > name.bottom(),
                "{width}: the golfers below the name"
            );
        }
    }

    /// The Golfer card's layout: the name centered on the avatar, the gear
    /// in the top right corner and a hairline between the avatar and the
    /// golfers.
    #[test]
    fn the_golfer_card_is_laid_out_as_drawn() {
        for width in [1600.0, 1024.0] {
            let mut app = golfers_app();
            let p = open(&mut app, HeaderMenu::Golfer, width);
            let avatar = p.rect("golfer:avatar");
            let name = p.rect("golfer:name");
            assert!(
                (name.center().y - avatar.center().y).abs() <= 1.0,
                "{width}: name centered on the avatar"
            );
            let gear = p.rect("golfer:settings");
            let row = p.rect("golfer_flighthook.e");
            assert!(gear.top() <= avatar.top() + 0.5, "{width}: gear at the top");
            assert!(
                (gear.right() - row.right()).abs() <= 1.0,
                "{width}: gear at the right"
            );
            assert!(
                p.rules
                    .iter()
                    .any(|y| *y > avatar.bottom() && *y < row.top()),
                "{width}: a hairline before GOLFERS: {:?}",
                p.rules
            );
        }
    }

    /// The row of the golfer up carries the accent bar, also when the
    /// turn names a game id linked to a saved golfer.
    #[test]
    fn the_golfer_up_is_highlighted_also_by_a_linked_game_id() {
        let mut app = golfers_app();
        let mut config = app.settings.saved_config_ref().cloned().unwrap();
        config.golfers[0]
            .identifiers
            .push("pureplay.local-3".into());
        app.settings.load_from(&config);
        for up in ["flighthook.e", "pureplay.local-3"] {
            app.turn_golfer = Some(up.into());
            let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
            assert!(p.has_accent_in("golfer_flighthook.e"), "{up} up");
            assert!(!p.has_accent_in("golfer_flighthook.s"), "{up} up");
            assert!(
                !p.marks.contains_key("golfer_pureplay.local-3"),
                "the linked id has no row of its own"
            );
        }
    }

    /// A game golfer's row names its game by the id's namespace.
    #[test]
    fn a_game_golfers_row_names_its_game() {
        let mut app = golfers_app();
        app.turn_golfer = Some("othergame.player-9".into());
        let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
        assert!(p.marks.contains_key("golfer_othergame.player-9"));
        assert!(p.has_text("othergame"), "{:?}", p.texts);
        assert!(!p.has_text("Pureplay"), "{:?}", p.texts);
        assert!(p.has_accent_in("golfer_othergame.player-9"));
        app.turn_golfer = Some("pureplay.player-7".into());
        let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
        assert!(p.has_text("Pureplay"), "{:?}", p.texts);
    }

    /// Follow turns is on whenever fewer than two golfers are known, so it
    /// is on when it shows again.
    #[test]
    fn follow_turns_is_on_when_it_shows_again() {
        let mut app = golfers_app();
        let (ctx, p) = settled(&mut app, 1600.0);
        click(&ctx, &mut app, p.rect("follow").center());
        assert!(!app.follow_turns);
        let mut config = app.settings.saved_config_ref().cloned().unwrap();
        let sam = config.golfers.pop().unwrap();
        app.settings.load_from(&config);
        assert!(!pass(&mut app, 1600.0).marks.contains_key("follow"));
        assert!(app.follow_turns, "on while hidden");
        config.golfers.push(sam);
        app.settings.load_from(&config);
        assert!(pass(&mut app, 1600.0).marks.contains_key("follow"));
        assert!(app.follow_turns, "on when it shows again");
    }

    /// Follow turns: shown with two or more known golfers (a game's count),
    /// on by default; a click flips the session state.
    #[test]
    fn follow_turns_shows_with_two_golfers_and_flips_on_a_click() {
        let mut one = bar_app(0);
        assert!(!pass(&mut one, 1600.0).marks.contains_key("follow"));
        // One saved golfer and one a game names: two.
        let config = FlighthookConfig {
            golfers: vec![flighthook::GolferRecord::new("flighthook.e", "Eric")],
            ..Default::default()
        };
        one.settings.load_from(&config);
        assert!(!pass(&mut one, 1600.0).marks.contains_key("follow"));
        let game = flighthook::GolferProfile::new("pureplay.player-7".parse().unwrap(), "Guest");
        let msg = flighthook::BusMessage::new("pp", game);
        one.bus_states.insert(msg.state_key().unwrap(), msg);
        assert!(pass(&mut one, 1600.0).marks.contains_key("follow"));
        assert!(one.follow_turns, "on by default");

        let mut app = golfers_app();
        let (ctx, p) = settled(&mut app, 1600.0);
        let at = p.rect("follow").center();
        click(&ctx, &mut app, at);
        assert!(!app.follow_turns);
        click(&ctx, &mut app, at);
        assert!(app.follow_turns);
    }

    /// Whether Shot Type shows, per card type: one test per card type.
    macro_rules! shot_type_row {
        ($name:ident, $shown:expr, |$c:ident| $add:expr) => {
            #[test]
            fn $name() {
                let mut app = FlighthookApp::with_pending(crate::net::new_pending());
                let mut $c = FlighthookConfig::default();
                $add;
                app.settings.load_from(&$c);
                assert_eq!(
                    pass(&mut app, 1600.0).marks.contains_key("shot-type"),
                    $shown
                );
                // Turned off, the card steers nothing.
                let mut off = $c.clone();
                for s in off.mevo.values_mut() {
                    s.enabled = false;
                }
                for s in off.trackball.values_mut() {
                    s.enabled = false;
                }
                for s in off.mock_monitor.values_mut() {
                    s.enabled = false;
                }
                app.settings.load_from(&off);
                assert!(!pass(&mut app, 1600.0).marks.contains_key("shot-type"));
            }
        };
    }

    shot_type_row!(a_mevo_card_shows_shot_type, true, |c| c
        .mevo
        .insert("0".into(), flighthook::MevoSection::default())
        .map(drop)
        .unwrap_or_default());
    shot_type_row!(an_r10_card_hides_shot_type, false, |c| c
        .r10
        .insert("0".into(), flighthook::R10Section::default())
        .map(drop)
        .unwrap_or_default());
    shot_type_row!(an_omni_card_hides_shot_type, false, |c| c
        .square
        .insert("0".into(), flighthook::SquareSection::default())
        .map(drop)
        .unwrap_or_default());
    shot_type_row!(an_exputt_card_hides_shot_type, false, |c| c
        .exputt
        .insert("0".into(), flighthook::ExPuttSection::default())
        .map(drop)
        .unwrap_or_default());
    shot_type_row!(a_trackball_card_shows_shot_type, true, |c| c
        .trackball
        .insert("0".into(), flighthook::TrackballSection::default())
        .map(drop)
        .unwrap_or_default());
    shot_type_row!(a_mock_monitor_shows_shot_type, true, |c| c
        .mock_monitor
        .insert("0".into(), flighthook::MockMonitorSection::default())
        .map(drop)
        .unwrap_or_default());
    shot_type_row!(an_open_connect_server_hides_shot_type, false, |c| c
        .openconnect_server
        .insert("0".into(), flighthook::OpenConnectServerSection::default())
        .map(drop)
        .unwrap_or_default());
    shot_type_row!(a_pureplay_accessory_hides_shot_type, false, |c| c
        .pureplay_accessory
        .insert("0".into(), flighthook::PureplayAccessorySection::default())
        .map(drop)
        .unwrap_or_default());

    /// A remote launch monitor is not counted: the bar cannot know.
    #[test]
    fn a_remote_launch_monitor_does_not_show_shot_type() {
        let mut app = bar_app(0);
        let msg = flighthook::BusMessage::new(
            "remote.lm",
            flighthook::BusActor::new("Remote", 5.0).with_role(flighthook::roles::LAUNCH_MONITOR),
        );
        app.bus_states.insert(msg.state_key().unwrap(), msg);
        assert!(!pass(&mut app, 1600.0).marks.contains_key("shot-type"));
    }

    /// Per-mode routing counts: Chip, Putt, Full alone and "Any".
    fn routed(edit: fn(&mut flighthook::OpenConnectClientSection)) -> bool {
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        let mut config = FlighthookConfig::default();
        let mut client = flighthook::OpenConnectClientSection::default();
        edit(&mut client);
        config.openconnect_client.insert("0".into(), client);
        app.settings.load_from(&config);
        pass(&mut app, 1600.0).marks.contains_key("shot-type")
    }

    #[test]
    fn routing_chips_to_a_monitor_shows_shot_type() {
        assert!(routed(|c| c.chipping_monitor = Some("r10.0".into())));
    }

    #[test]
    fn routing_putts_to_a_monitor_shows_shot_type() {
        assert!(routed(|c| c.putting_monitor = Some("exputt.0".into())));
    }

    #[test]
    fn routing_only_full_swings_to_a_monitor_hides_shot_type() {
        assert!(!routed(|c| c.full_monitor = Some("r10.0".into())));
    }

    #[test]
    fn routing_to_any_monitor_hides_shot_type() {
        assert!(!routed(|_| {}));
    }

    /// The energy icon: not drawn without a charge; the lowest charge sets
    /// its color (normal from 20 %, yellow from 10 to 19, red below 10).
    #[test]
    fn the_energy_icon_needs_a_charge_and_the_lowest_one_colors_it() {
        let mut none = bar_app(0);
        none.actors.insert(
            "r10.0".into(),
            ActorStatusResponse::new("R10", ActorStatus::Connected),
        );
        assert!(!pass(&mut none, 1600.0).marks.contains_key("energy"));
        let outline = "energy-normal@1x".to_owned();
        let filled = "energy-filled-normal@1x".to_owned();
        for (pct, icon) in [
            (74u8, (outline.clone(), theme::TEXT_MUTED)),
            (25, (outline.clone(), theme::TEXT_MUTED)),
            (20, (outline.clone(), theme::TEXT_MUTED)),
            (19, (filled.clone(), theme::GOLD)),
            (15, (filled.clone(), theme::GOLD)),
            (10, (filled.clone(), theme::GOLD)),
            (9, (filled.clone(), theme::DANGER)),
            (5, (filled.clone(), theme::DANGER)),
        ] {
            let mut app = bar_app(0);
            charge(&mut app, "square.0", "Square Omni", pct);
            let p = pass(&mut app, 1600.0);
            assert_eq!(p.icon_in("energy"), icon, "{pct}%");
        }
        // The lowest of two decides.
        let mut app = bar_app(0);
        charge(&mut app, "square.0", "Square Omni", 74);
        charge(&mut app, "mevo.0", "Mevo+", 8);
        assert_eq!(
            pass(&mut app, 1600.0).icon_in("energy"),
            (filled, theme::DANGER)
        );
    }

    /// The bar after the pointer rests on the mark `name`.
    fn hovering(app: &mut FlighthookApp, name: &str, ppp: f32) -> Pass {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx.set_pixels_per_point(ppp);
        pass_at(&ctx, app, 1600.0, 0.0, Vec::new());
        let at = pass_at(&ctx, app, 1600.0, 0.0, Vec::new())
            .rect(name)
            .center();
        pass_at(&ctx, app, 1600.0, 0.1, vec![Event::PointerMoved(at)]);
        pass_at(&ctx, app, 1600.0, 0.2, Vec::new())
    }

    /// Every button draws its own icon: the normal file tinted (muted at
    /// rest, dim while disabled), the hover file untinted while hovered,
    /// selected or a toggle that is on.
    #[test]
    fn each_button_draws_its_icon_for_its_state() {
        let normal = |f: &str| (format!("{f}-normal@1x"), theme::TEXT_MUTED);
        let lit = |f: &str| (format!("{f}-hover@1x"), Color32::WHITE);
        let mut app = bar_app(0b1110);
        let p = pass(&mut app, 1600.0);
        // The nav: Shots selected, Cameras disabled (none configured).
        assert_eq!(p.icon_in("nav:shots"), lit("nav-shots"));
        assert_eq!(
            p.icon_in("nav:cameras"),
            ("nav-cameras-normal@1x".to_owned(), theme::TEXT_DIM)
        );
        assert_eq!(p.icon_in("nav:settings"), normal("nav-settings"));
        assert_eq!(p.icon_in("nav:log"), normal("nav-log"));
        // Follow is on; Auto-Chip is off (no simulator) and disabled.
        assert_eq!(p.icon_in("follow"), lit("follow-on"));
        assert_eq!(
            p.icon_in("auto-chip"),
            ("auto-chip-off-normal@1x".to_owned(), theme::TEXT_DIM)
        );
        assert_eq!(p.icon_in("shot-type"), normal("shot-type-full"));
        assert_eq!(p.icon_in("club"), normal("club-none"));
        assert_eq!(p.icon_in("golfer"), normal("golfer"));
        // Auto-Chip on, Follow off, nobody up.
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", ActorStatus::Connected),
        );
        app.follow_turns = false;
        app.turn_golfer = None;
        app.viewed_golfer = None;
        let p = pass(&mut app, 1600.0);
        // Auto-Chip is an action: muted at rest even when usable.
        assert_eq!(p.icon_in("auto-chip"), normal("auto-chip-on"));
        assert_eq!(p.icon_in("follow"), normal("follow-off"));
        assert_eq!(p.icon_in("golfer"), normal("golfer-none"));
        // Hovered, each takes its hover file.
        for (name, file) in [
            ("nav:log", "nav-log"),
            ("follow", "follow-off"),
            ("shot-type", "shot-type-full"),
            ("club", "club-none"),
            ("golfer", "golfer-none"),
            ("tournament", "tournament-off"),
        ] {
            assert_eq!(
                hovering(&mut app, name, 1.0).icon_in(name),
                lit(file),
                "{name}"
            );
        }
        // Each shot type has its icon.
        for (mode, file) in [
            ("chipping", "shot-type-chip"),
            ("putting", "shot-type-putt"),
        ] {
            app.current_mode = mode.into();
            assert_eq!(pass(&mut app, 1600.0).icon_in("shot-type"), normal(file));
        }
    }

    /// The outline bolt lights up on hover; the filled bolt keeps its
    /// warning color in both states.
    #[test]
    fn the_filled_energy_icon_keeps_its_tint_on_hover() {
        let mut app = bar_app(0);
        charge(&mut app, "square.0", "Square Omni", 74);
        assert_eq!(
            hovering(&mut app, "energy", 1.0).icon_in("energy"),
            ("energy-hover@1x".to_owned(), Color32::WHITE)
        );
        charge(&mut app, "square.0", "Square Omni", 5);
        assert_eq!(
            hovering(&mut app, "energy", 1.0).icon_in("energy"),
            ("energy-filled-hover@1x".to_owned(), theme::DANGER)
        );
    }

    /// Each club picker row has its head; no club is the dashed iron.
    #[test]
    fn each_club_takes_the_head_of_its_picker_row() {
        for (club, head) in [
            (Some(Club::Driver), "club-driver"),
            (Some(Club::MiniDriver), "club-driver"),
            (Some(Club::Wood3), "club-wood"),
            (Some(Club::Hybrid4), "club-hybrid"),
            (Some(Club::Iron7), "club-iron"),
            (Some(Club::PitchingWedge), "club-wedge"),
            (Some(Club::LobWedge), "club-wedge"),
            (Some(Club::Putter), "club-putter"),
            (None, "club-none"),
        ] {
            assert_eq!(club_icon(club).name, head, "{club:?}");
        }
    }

    /// The club code and the golfer's initial are drawn inside their
    /// icons, in the tint at rest and in the background color on hover.
    #[test]
    fn the_club_code_and_the_initial_are_drawn_inside_their_icons() {
        let mut app = bar_app(0b0010);
        app.selected_club = Some(Club::Iron7);
        let p = pass(&mut app, 1600.0);
        let (_, head) = p.image("club-iron-normal@1x");
        assert_eq!(head.size(), vec2(32.0, 32.0));
        assert!(head.contains_rect(p.text_rect("7I")), "{head:?}");
        assert_eq!(p.color_of("7I"), theme::TEXT_MUTED);
        let (_, ring) = p.image("golfer-normal@1x");
        assert_eq!(ring.size(), vec2(28.0, 28.0));
        assert!(ring.contains_rect(p.text_rect("E")), "{ring:?}");
        assert_eq!(p.color_of("E"), theme::TEXT_MUTED);
        let p = hovering(&mut app, "club", 1.0);
        assert!(
            p.image("club-iron-hover@1x")
                .1
                .contains_rect(p.text_rect("7I"))
        );
        assert_eq!(p.color_of("7I"), theme::BG);
        let p = hovering(&mut app, "golfer", 1.0);
        assert_eq!(p.color_of("E"), theme::BG);
        // A wide code shrinks to the head's label width.
        app.selected_club = Some(Club::Wood3);
        let p = pass(&mut app, 1600.0);
        assert!(
            p.text_rect("3W").width() <= 15.2 + 0.5,
            "{:?}",
            p.text_rect("3W")
        );
    }

    /// At UI zoom 2 the bar draws the 2x files, at 4 the 3x files.
    #[test]
    fn the_icons_follow_the_zoom() {
        let mut app = bar_app(0);
        for (ppp, file) in [(2.0, "nav-log-hover@2x"), (4.0, "nav-log-hover@3x")] {
            assert_eq!(
                hovering(&mut app, "nav:log", ppp).icon_in("nav:log").0,
                file
            );
        }
    }

    /// The Batteries dropdown: a title, one row per device in card order
    /// with its name and percent, colored by the same thresholds.
    #[test]
    fn the_batteries_dropdown_lists_the_devices_in_card_order() {
        let mut app = bar_app(0);
        charge(&mut app, "square.0", "Square Omni", 74);
        charge(&mut app, "mevo.0", "Mevo+", 18);
        charge(&mut app, "r10.0", "R10", 7);
        let p = open(&mut app, HeaderMenu::Energy, 1600.0);
        assert!(p.has_text("BATTERIES"));
        let order: Vec<&str> = ["mevo.0", "r10.0", "square.0"].into();
        let ys: Vec<f32> = order
            .iter()
            .map(|id| p.rect(&format!("battery:{id}")).top())
            .collect();
        assert!(ys.windows(2).all(|w| w[0] < w[1]), "card order: {ys:?}");
        for (text, color) in [
            ("18%", theme::STATUS_PENDING),
            ("7%", theme::STATUS_DOWN),
            ("74%", theme::STATUS_OK),
        ] {
            assert_eq!(p.color_of(text), color, "{text}");
        }
        for name in ["Mevo+", "R10", "Square Omni"] {
            assert!(p.has_text(name), "{name}");
        }
    }

    /// Settings with unsaved edits lock the nav, with the reason as the
    /// hover text; the toggle's click still works unlocked.
    #[test]
    fn unsaved_settings_edits_lock_the_nav() {
        let mut app = bar_app(0b1000);
        let p = pass(&mut app, 1600.0);
        for n in ["shots", "cameras", "settings", "log"] {
            let enabled = p.enabled(&format!("nav:{n}"));
            // Cameras is off anyway while no camera is configured.
            assert_eq!(enabled, n != "cameras", "{n}");
        }
        app.settings.actors[0].set_dirty(true);
        assert!(app.settings.locked());
        let p = pass(&mut app, 1600.0);
        for n in ["shots", "cameras", "settings", "log"] {
            assert!(!p.enabled(&format!("nav:{n}")), "{n} locked");
        }
        // The right group still answers.
        assert!(p.enabled("club"));
    }

    /// The red dot sits on the Settings icon while a card is red.
    #[test]
    fn the_alert_dot_rides_the_settings_icon() {
        let mut app = bar_app(0);
        assert!(!pass(&mut app, 1600.0).marks.contains_key("alert_dot"));
        let mut actor = ActorStatusResponse::new("Range Bridge", ActorStatus::Connected);
        actor.observe(ActorStatus::Connected, 1_000);
        actor.observe(ActorStatus::Disconnected, 1_000);
        app.actors.insert("openconnect_server.0".into(), actor);
        app.now_ms = 1_001;
        let p = pass(&mut app, 1600.0);
        let settings = p.rect("nav:settings");
        assert!(settings.contains_rect(p.rect("alert_dot")));
        assert!(
            p.rect("alert_dot").center().x > settings.center().x,
            "top right"
        );
    }

    /// The nav icons switch what the content shows: Shots and Cameras the
    /// Shots tab's pane, Settings the device, output and global cards.
    #[test]
    fn the_nav_icons_switch_the_content() {
        let mut app = bar_app(0);
        assert_eq!(app.active_tab, Tab::Shots, "the app opens on Shots");
        let (ctx, p) = settled(&mut app, 1600.0);
        click(&ctx, &mut app, p.rect("nav:settings").center());
        assert_eq!(app.active_tab, Tab::Devices);
        click(&ctx, &mut app, p.rect("nav:log").center());
        assert_eq!(app.active_tab, Tab::Log);
        click(&ctx, &mut app, p.rect("nav:shots").center());
        assert_eq!(app.active_tab, Tab::Shots);
        assert_eq!(app.shots_pane, ShotsPane::Data);
        // Cameras is disabled while none is configured, with the reason.
        assert!(!p.enabled("nav:cameras"));
    }

    /// The texts drawn while the pointer rests on the mark `name`.
    fn hover_texts(app: &mut FlighthookApp, name: &str) -> Vec<String> {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, app, 1600.0, 0.0, Vec::new());
        let at = pass_at(&ctx, app, 1600.0, 0.0, Vec::new())
            .rect(name)
            .center();
        pass_at(&ctx, app, 1600.0, 0.1, vec![Event::PointerMoved(at)]);
        pass_at(&ctx, app, 1600.0, 2.0, Vec::new());
        pass_at(&ctx, app, 1600.0, 2.1, Vec::new())
            .texts
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }

    /// Every button of the right group, and Cameras with none configured,
    /// shows its hover text.
    #[test]
    fn every_right_group_button_has_its_hover_text() {
        let mut app = golfers_app();
        charge(&mut app, "square.0", "Square Omni", 74);
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", ActorStatus::Connected),
        );
        let mut config = app.settings.saved_config_ref().cloned().unwrap();
        config.simulation.auto_chip = true;
        config
            .mevo
            .insert("0".into(), flighthook::MevoSection::default());
        app.settings.load_from(&config);
        app.turn_golfer = Some("flighthook.e".into());
        app.current_mode = "full".into();
        let off = Tournament::Off.button_tip();
        let cases: [(&str, &str); 9] = [
            ("energy", "Square Omni 74%"),
            ("tournament", off.as_str()),
            (
                "follow",
                "Following turns: on. The view moves to the golfer up.",
            ),
            ("auto-chip", AUTO_CHIP_HINT),
            ("shot-type", "Shot Type: Full"),
            ("club", "Club: 7 Iron"),
            ("golfer", "Golfer: Eric"),
            ("nav:cameras", NO_CAMERAS),
            ("nav:log", "Log"),
        ];
        for (name, want) in cases {
            let texts = hover_texts(&mut app, name);
            assert!(
                texts.iter().any(|t| t == want),
                "{name}: {want:?} not in {texts:?}"
            );
        }
    }

    /// A row of the Auto-Chip dropdown sends that chip and closes it.
    #[test]
    fn an_auto_chip_row_sends_the_chip() {
        let mut app = bar_app(0b0100);
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", ActorStatus::Connected),
        );
        let ctx = egui::Context::default();
        theme::install(&ctx);
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        app.header_menu = Some(HeaderMenu::AutoChip);
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
            .rect("auto-chip:3 Yard")
            .center();
        click(&ctx, &mut app, at);
        assert_eq!(app.auto_chip_in_flight, Some(3));
        assert_eq!(app.header_menu, None);
    }

    /// With a camera configured, the Cameras icon opens the Shots tab on
    /// its Cameras pane and takes the marker.
    #[test]
    fn the_cameras_icon_opens_the_cameras_pane() {
        let mut app = bar_app(0);
        let mut config = app.settings.saved_config_ref().cloned().unwrap();
        config.camera.insert(
            "0".into(),
            flighthook::CameraSection {
                camera: flighthook::CameraSection::TEST_PATTERN.into(),
                ..Default::default()
            },
        );
        app.settings.load_from(&config);
        app.cameras_supported = true;
        app.active_tab = Tab::Log;
        let (ctx, p) = settled(&mut app, 1600.0);
        assert!(p.enabled("nav:cameras"));
        click(&ctx, &mut app, p.rect("nav:cameras").center());
        assert_eq!(app.active_tab, Tab::Shots);
        assert_eq!(app.shots_pane, ShotsPane::Cameras);
        let p = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        let cameras = p.rect("nav:cameras");
        assert!(
            p.accents
                .iter()
                .any(|a| a.center().x == cameras.center().x && a.top() >= cameras.bottom()),
            "the marker under Cameras"
        );
    }

    /// Escape closes the open dropdown.
    #[test]
    fn escape_closes_an_open_dropdown() {
        let mut app = golfers_app();
        let (ctx, p) = settled(&mut app, 1600.0);
        click(&ctx, &mut app, p.rect("club").center());
        assert_eq!(app.header_menu, Some(HeaderMenu::Club));
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        let key = |pressed| Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        pass_at(&ctx, &mut app, 1600.0, 0.0, vec![key(true)]);
        pass_at(&ctx, &mut app, 1600.0, 0.0, vec![key(false)]);
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        assert_eq!(app.header_menu, None);
    }

    /// One dropdown at a time: opening another closes the first.
    #[test]
    fn one_dropdown_is_open_at_a_time() {
        let mut app = golfers_app();
        let (ctx, p) = settled(&mut app, 1600.0);
        click(&ctx, &mut app, p.rect("club").center());
        assert_eq!(app.header_menu, Some(HeaderMenu::Club));
        let p = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        click(&ctx, &mut app, p.rect("golfer").center());
        assert_eq!(app.header_menu, Some(HeaderMenu::Golfer));
    }

    /// `golfers_app` with Eric up and Follow turns off, viewing `viewed`.
    fn viewing(viewed: &str) -> FlighthookApp {
        let mut app = golfers_app();
        app.turn_golfer = Some("flighthook.e".into());
        app.follow_turns = false;
        app.refresh_shot_caches();
        app.set_view(Some(viewed.into()));
        app
    }

    /// Follow turns' three hover texts, and the Club, Shot Type and
    /// Auto-Chip hover texts naming the golfer up while the view is on
    /// someone else.
    #[test]
    fn hover_texts_follow_the_view() {
        let setup = |app: &mut FlighthookApp| {
            app.actors.insert(
                "openconnect_client.0".into(),
                ActorStatusResponse::new("GSPro", ActorStatus::Connected),
            );
            let mut config = app.settings.saved_config_ref().cloned().unwrap();
            config.simulation.auto_chip = true;
            config
                .mevo
                .insert("0".into(), flighthook::MevoSection::default());
            app.settings.load_from(&config);
            app.current_mode = "full".into();
        };
        let mut other = viewing("flighthook.s");
        setup(&mut other);
        let chip = format!("{AUTO_CHIP_HINT} (Eric, up)");
        let cases: [(&str, &str); 5] = [
            ("follow", "Following turns: off. Viewing Sam; Eric is up."),
            ("auto-chip", chip.as_str()),
            ("shot-type", "Shot Type: Full (Eric, up)"),
            ("club", "Club: 7 Iron (Eric, up)"),
            ("golfer", "Golfer: Sam"),
        ];
        for (name, want) in cases {
            let texts = hover_texts(&mut other, name);
            assert!(
                texts.iter().any(|t| t == want),
                "{name}: {want:?} not in {texts:?}"
            );
        }
        let mut up = viewing("flighthook.e");
        setup(&mut up);
        let cases: [(&str, &str); 5] = [
            (
                "follow",
                "Following turns: off. The view stays on Eric when the turn changes.",
            ),
            ("auto-chip", AUTO_CHIP_HINT),
            ("shot-type", "Shot Type: Full"),
            ("club", "Club: 7 Iron"),
            ("golfer", "Golfer: Eric"),
        ];
        for (name, want) in cases {
            let texts = hover_texts(&mut up, name);
            assert!(
                texts.iter().any(|t| t == want),
                "{name}: {want:?} not in {texts:?}"
            );
        }
    }

    /// The Golfer icon shows the golfer up's initial while Follow turns is
    /// on and the viewed golfer's while it is off; the card marks the
    /// viewed golfer's row and, while off, the golfer up's with the gold
    /// dot.
    #[test]
    fn the_golfer_icon_and_card_follow_the_view() {
        let mut on = golfers_app();
        on.turn_golfer = Some("flighthook.e".into());
        on.refresh_shot_caches();
        let p = open(&mut on, HeaderMenu::Golfer, 1600.0);
        assert!(p.has_text("E"), "{:?}", p.texts);
        assert!(p.has_accent_in("golfer_flighthook.e"));
        assert!(
            !p.marks.keys().any(|k| k.starts_with("golfer:up_")),
            "no dot while on"
        );
        let mut off = viewing("flighthook.s");
        let p = open(&mut off, HeaderMenu::Golfer, 1600.0);
        assert!(p.has_text("S") && p.has_text("Sam"), "{:?}", p.texts);
        assert!(p.has_accent_in("golfer_flighthook.s"), "the viewed row");
        assert!(!p.has_accent_in("golfer_flighthook.e"));
        let dot = p.rect("golfer:up_flighthook.e");
        assert!(
            p.rect("golfer_flighthook.e").contains_rect(dot),
            "on Eric's row"
        );
        assert!(!p.marks.contains_key("golfer:up_flighthook.s"));
        // Off and viewing the golfer up: the dot stays on the golfer up.
        let mut same = viewing("flighthook.e");
        let p = open(&mut same, HeaderMenu::Golfer, 1600.0);
        assert!(p.marks.contains_key("golfer:up_flighthook.e"));
        assert!(p.has_text("E"));
    }

    /// The gear opens Settings on Golfers, but unsaved edits keep their
    /// section on show; the card closes either way.
    #[test]
    fn the_golfer_gear_keeps_the_section_of_unsaved_edits() {
        use crate::panels::settings_nav::SettingsSection;
        let mut app = golfers_app();
        app.settings_section = SettingsSection::General;
        app.settings.global_dirty = true;
        assert!(app.settings.locked());
        let (ctx, _) = settled(&mut app, 1600.0);
        app.header_menu = Some(HeaderMenu::Golfer);
        pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
        let at = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new())
            .rect("golfer:settings")
            .center();
        click(&ctx, &mut app, at);
        assert_eq!(app.settings_section, SettingsSection::General);
        assert_eq!(app.header_menu, None);
    }

    /// Follow turns off then on with a click: the view snaps to the golfer
    /// up at once.
    #[test]
    fn turning_follow_on_snaps_the_view_to_the_golfer_up() {
        let mut app = viewing("flighthook.s");
        assert!(app.viewing_other());
        let (ctx, p) = settled(&mut app, 1600.0);
        click(&ctx, &mut app, p.rect("follow").center());
        assert!(app.follow_turns);
        assert_eq!(app.viewed_golfer.as_deref(), Some("flighthook.e"));
        assert!(!app.viewing_other());
    }
    /// The Golfer card lists a departed guest and "No golfer" after the
    /// known golfers; a click on either moves only the view and sends
    /// nothing.
    #[test]
    fn view_only_rows_follow_the_known_golfers_and_send_nothing() {
        let mut app = golfers_app();
        app.turn_golfer = Some("flighthook.e".into());
        let row = |n: u32, golfer: Option<&str>| {
            let mut row = crate::types::ShotRow::from(crate::types::ShotData {
                shot_number: n,
                shot_id: Some(format!("s{n}")),
                ..crate::types::ShotData::default()
            });
            row.golfer = golfer.map(str::to_owned);
            row
        };
        app.shots = vec![
            row(1, None),
            row(2, Some("pureplay.player-7")),
            row(3, Some("flighthook.e")),
        ];
        app.golfer_names
            .insert("pureplay.player-7".into(), "Pat Guest".into());
        app.shots_rev += 1;
        app.refresh_shot_caches();
        for follow in [true, false] {
            app.follow_turns = follow;
            app.view_hold = None;
            app.refresh_shot_caches();
            let p = open(&mut app, HeaderMenu::Golfer, 1600.0);
            assert!(p.has_text("Pat Guest") && p.has_text("No golfer"));
            let sam = p.rect("golfer_flighthook.s");
            let pat = p.rect("golfer_pureplay.player-7");
            let none = p.rect("golfer:none");
            assert!(sam.bottom() <= pat.top() && pat.bottom() <= none.top());
            assert!(p.icon_in("golfer:none").0.starts_with("golfer-none"));
            assert!(
                p.icon_in("golfer_pureplay.player-7")
                    .0
                    .starts_with("golfer-normal")
            );
            for (name, want) in [
                ("golfer_pureplay.player-7", Some("pureplay.player-7")),
                ("golfer:none", None),
            ] {
                let ctx = egui::Context::default();
                theme::install(&ctx);
                app.header_menu = Some(HeaderMenu::Golfer);
                pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
                let p = pass_at(&ctx, &mut app, 1600.0, 0.0, Vec::new());
                click(&ctx, &mut app, p.rect(name).center());
                app.refresh_shot_caches();
                assert_eq!(app.viewed_golfer.as_deref(), want, "{name} follow {follow}");
                assert!(app.sent_selects.is_empty(), "{name} follow {follow}");
                assert_eq!(app.turn_golfer.as_deref(), Some("flighthook.e"));
            }
        }
    }
}
