//! Card layout of the Devices tab: which group each actor type belongs to,
//! the order of cards within a group ([`grouped`]), the types each group's
//! (+) menu offers, and the wrapping grid the cards flow into.

use crate::panels::devices;
use crate::theme;

/// A heading on the Devices tab, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardGroup {
    /// The settings-only Global and Simulation cards.
    General,
    /// Shot sources: launch monitors and the Open Connect server.
    LaunchMonitors,
    /// Everything that consumes shots or serves the dashboard.
    Outputs,
}

impl CardGroup {
    pub(crate) const ALL: [CardGroup; 3] = [
        CardGroup::General,
        CardGroup::LaunchMonitors,
        CardGroup::Outputs,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            CardGroup::General => "General",
            CardGroup::LaunchMonitors => "Launch monitors",
            CardGroup::Outputs => "Outputs",
        }
    }

    /// Hover text of the group's (+) button.
    pub(crate) fn add_hover(self) -> &'static str {
        match self {
            CardGroup::General => "",
            CardGroup::LaunchMonitors => "Add a launch monitor",
            CardGroup::Outputs => "Add an output",
        }
    }
}

/// Every configurable actor type (its config section and actor ID prefix),
/// in display order, with its group.
const ACTOR_TYPES: &[(&str, CardGroup)] = &[
    ("mevo", CardGroup::LaunchMonitors),
    ("r10", CardGroup::LaunchMonitors),
    ("square", CardGroup::LaunchMonitors),
    ("mock_monitor", CardGroup::LaunchMonitors),
    ("openconnect_server", CardGroup::LaunchMonitors),
    ("gspro", CardGroup::Outputs),
    ("webserver", CardGroup::Outputs),
    ("random_club", CardGroup::Outputs),
];

/// The group an actor type belongs to. A type this list does not know goes
/// under Outputs.
pub(crate) fn group_of(type_key: &str) -> CardGroup {
    ACTOR_TYPES
        .iter()
        .find(|(t, _)| *t == type_key)
        .map_or(CardGroup::Outputs, |(_, g)| *g)
}

/// Position of a type in [`ACTOR_TYPES`]; unknown types sort after them.
fn type_rank(type_key: &str) -> usize {
    ACTOR_TYPES
        .iter()
        .position(|(t, _)| *t == type_key)
        .unwrap_or(ACTOR_TYPES.len())
}

/// Split an actor ID (`"mevo.0"`) into its type and instance index.
pub(crate) fn split_actor_id(id: &str) -> (&str, &str) {
    id.split_once('.').unwrap_or((id, ""))
}

/// Order of a card within its group: type order, then instance index
/// (numeric indices numerically, ahead of any other).
fn order_key<'a>(type_key: &'a str, index: &'a str) -> (usize, &'a str, (bool, u64), &'a str) {
    let numeric = index.parse::<u64>().map_or((true, 0), |n| (false, n));
    (type_rank(type_key), type_key, numeric, index)
}

/// Sort `items` (type, instance index, payload) into every [`CardGroup`], in
/// group order, each group's items in card order. Groups without items are
/// present and empty.
pub(crate) fn grouped<'a, T>(
    items: impl IntoIterator<Item = (&'a str, &'a str, T)>,
) -> Vec<(CardGroup, Vec<T>)> {
    let mut items: Vec<(&str, &str, T)> = items.into_iter().collect();
    items.sort_by(|a, b| order_key(a.0, a.1).cmp(&order_key(b.0, b.1)));
    let mut groups: Vec<(CardGroup, Vec<T>)> =
        CardGroup::ALL.iter().map(|g| (*g, Vec::new())).collect();
    for (type_key, _, item) in items {
        let group = group_of(type_key);
        if let Some((_, members)) = groups.iter_mut().find(|(g, _)| *g == group) {
            members.push(item);
        }
    }
    groups
}

/// What a group's (+) menu offers: (menu label, actor type).
pub(crate) fn add_options(group: CardGroup) -> &'static [(&'static str, &'static str)] {
    match group {
        CardGroup::General => &[],
        CardGroup::LaunchMonitors => &[
            ("Mevo", "mevo"),
            ("R10", "r10"),
            ("Square Golf Omni", "square"),
            ("Uneekor (OpenConnect)", "openconnect_server"),
        ],
        CardGroup::Outputs => &[("GSPro", "gspro"), ("Web Server", "webserver")],
    }
}

/// Narrowest a card gets before the grid drops a column.
pub(crate) const CARD_MIN_WIDTH: f32 = 420.0;
/// Widest a card gets; a window too narrow for another column leaves the
/// rest of the row empty.
pub(crate) const CARD_MAX_WIDTH: f32 = 620.0;
/// Space between cards, both ways.
pub(crate) const CARD_GAP: f32 = 10.0;

/// Columns and card width for a grid `available` wide: as many columns as
/// fit at [`CARD_MIN_WIDTH`], each card at most [`CARD_MAX_WIDTH`]. One
/// column on a window narrower than a card, the card as wide as the window.
pub(crate) fn grid_layout(available: f32) -> (usize, f32) {
    let available = available.max(1.0);
    let cols = ((available + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP))
        .floor()
        .max(1.0);
    let width = (available - CARD_GAP * (cols - 1.0)) / cols;
    (cols as usize, width.min(CARD_MAX_WIDTH))
}

/// A group heading: small caps, muted.
pub(crate) fn group_heading(ui: &mut egui::Ui, group: CardGroup) {
    ui.add_space(4.0);
    ui.label(theme::caps(group.title(), 13.0, theme::TEXT_MUTED));
    ui.add_space(2.0);
}

/// A group heading with a (+) after it, muted at rest and accent on hover,
/// that opens a menu of the types the group holds ([`add_options`]).
/// Disabled unless `enabled`. Returns the type picked.
pub(crate) fn add_heading(
    ui: &mut egui::Ui,
    group: CardGroup,
    enabled: bool,
) -> Option<&'static str> {
    ui.add_space(4.0);
    let picked = ui
        .horizontal(|ui| {
            ui.label(theme::caps(group.title(), 13.0, theme::TEXT_MUTED));
            let add = ui
                .add_enabled_ui(enabled, |ui| {
                    theme::icon(ui, theme::Icon::Add, 20.0, theme::ACCENT, enabled)
                })
                .inner
                .on_hover_text(group.add_hover());
            devices::mark(ui, || format!("add:{}", group.title()), &add);
            let mut picked = None;
            egui::Popup::menu(&add).show(|ui| {
                for &(label, type_key) in add_options(group) {
                    let item = ui.button(label);
                    devices::mark(ui, || format!("add:{}:{type_key}", group.title()), &item);
                    if item.clicked() {
                        picked = Some(type_key);
                    }
                }
            });
            picked
        })
        .inner;
    ui.add_space(2.0);
    picked
}

/// Lay `n` cards out in a wrapping grid ([`grid_layout`]): card `i` goes in
/// column `i % cols`, each column stacking its cards top-down. `card` draws
/// card `i` and fills the width it is given. Each column is a fixed-width
/// child, so a card that overflows never widens the grid. The grid leaves
/// the width of a scroll bar free on its right, so the bar of the scroll
/// area around it never covers a card.
pub(crate) fn card_grid(ui: &mut egui::Ui, n: usize, mut card: impl FnMut(&mut egui::Ui, usize)) {
    if n == 0 {
        return;
    }
    let scroll = ui.spacing().scroll;
    let bar = scroll.bar_width + scroll.bar_inner_margin + scroll.bar_outer_margin;
    let (cols, width) = grid_layout(ui.available_width() - bar);
    let used = cols.min(n);
    let top_left = ui.cursor().min;
    let mut height: f32 = 0.0;
    for col in 0..used {
        let x = top_left.x + col as f32 * (width + CARD_GAP);
        let rect = egui::Rect::from_min_size(
            egui::pos2(x, top_left.y),
            egui::vec2(width, ui.available_height()),
        );
        let mut column = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("card_column", col))
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        column.set_width(width);
        for i in (col..n).step_by(cols) {
            card(&mut column, i);
            column.add_space(CARD_GAP);
        }
        height = height.max(column.min_rect().height());
    }
    let total = used as f32 * width + (used - 1) as f32 * CARD_GAP;
    ui.allocate_rect(
        egui::Rect::from_min_size(top_left, egui::vec2(total, height)),
        egui::Sense::hover(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_has_its_group() {
        for t in [
            "mevo",
            "r10",
            "square",
            "mock_monitor",
            "openconnect_server",
        ] {
            assert_eq!(group_of(t), CardGroup::LaunchMonitors, "{t}");
        }
        for t in ["gspro", "webserver", "random_club"] {
            assert_eq!(group_of(t), CardGroup::Outputs, "{t}");
        }
        assert_eq!(group_of("something_new"), CardGroup::Outputs);
    }

    #[test]
    fn every_config_section_is_a_known_type() {
        // Destructured without `..`: a new instance map in the config fails
        // to compile here until it is listed below and given a group.
        let flighthook::FlighthookConfig {
            default_units: _,
            chipping_clubs: _,
            putting_clubs: _,
            shot_grouping: _,
            simulation: _,
            webserver: _,
            mevo: _,
            r10: _,
            square: _,
            mock_monitor: _,
            openconnect_server: _,
            gspro: _,
            random_club: _,
        } = flighthook::FlighthookConfig::default();
        for key in [
            "webserver",
            "mevo",
            "r10",
            "square",
            "mock_monitor",
            "openconnect_server",
            "gspro",
            "random_club",
        ] {
            assert!(
                ACTOR_TYPES.iter().any(|(t, _)| *t == key),
                "config section {key} has no card group"
            );
        }
    }

    #[test]
    fn cards_sort_by_group_then_type_then_index() {
        let ids = [
            "webserver.0",
            "gspro.10",
            "openconnect_server.0",
            "gspro.2",
            "mevo.1",
            "random_club.0",
            "r10.0",
            "mevo.0",
            "mock_monitor.0",
            "square.0",
            "custom.0",
        ];
        let groups = grouped(ids.iter().map(|id| {
            let (t, i) = split_actor_id(id);
            (t, i, *id)
        }));
        assert_eq!(
            groups,
            vec![
                (CardGroup::General, vec![]),
                (
                    CardGroup::LaunchMonitors,
                    vec![
                        "mevo.0",
                        "mevo.1",
                        "r10.0",
                        "square.0",
                        "mock_monitor.0",
                        "openconnect_server.0",
                    ]
                ),
                (
                    CardGroup::Outputs,
                    vec![
                        "gspro.2",
                        "gspro.10",
                        "webserver.0",
                        "random_club.0",
                        "custom.0",
                    ]
                ),
            ]
        );
    }

    #[test]
    fn add_menus_offer_only_their_groups_types() {
        assert!(add_options(CardGroup::General).is_empty());
        let types = |g| add_options(g).iter().map(|(_, t)| *t).collect::<Vec<_>>();
        assert_eq!(
            types(CardGroup::LaunchMonitors),
            ["mevo", "r10", "square", "openconnect_server"]
        );
        assert_eq!(types(CardGroup::Outputs), ["gspro", "webserver"]);
        for group in CardGroup::ALL {
            for (_, t) in add_options(group) {
                assert_eq!(group_of(t), group, "{t}");
            }
        }
    }

    #[test]
    fn grid_fits_columns_between_min_and_max_width() {
        // A phone: one column, the card as wide as the window.
        assert_eq!(grid_layout(360.0), (1, 360.0));
        // Too narrow for two: one column, capped at the max width.
        assert_eq!(grid_layout(800.0), (1, CARD_MAX_WIDTH));
        let (cols, width) = grid_layout(996.0);
        assert_eq!(cols, 2);
        assert!((CARD_MIN_WIDTH..=CARD_MAX_WIDTH).contains(&width));
        let (cols, width) = grid_layout(1572.0);
        assert_eq!(cols, 3);
        assert!((CARD_MIN_WIDTH..=CARD_MAX_WIDTH).contains(&width));
        let (cols, _) = grid_layout(2560.0);
        assert_eq!(cols, 5);
    }
}
