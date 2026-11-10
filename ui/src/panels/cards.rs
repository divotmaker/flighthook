//! Card layout of the Devices tab: which group each actor type belongs to,
//! the order of cards within a group ([`grouped`]), the presets each group's
//! (+) menu offers, and the wrapping grid the cards flow into.

use crate::panels::devices;
use crate::theme;

/// A heading on the Devices tab, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardGroup {
    /// The Global and Simulation cards, then the web server.
    General,
    /// Shot sources: launch monitors and the Open Connect server.
    LaunchMonitors,
    /// Swing cameras, which record a clip of every shot.
    Cameras,
    /// Everything that consumes shots.
    Outputs,
}

impl CardGroup {
    pub(crate) const ALL: [CardGroup; 4] = [
        CardGroup::General,
        CardGroup::LaunchMonitors,
        CardGroup::Cameras,
        CardGroup::Outputs,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            CardGroup::General => "General",
            CardGroup::LaunchMonitors => "Launch monitors",
            CardGroup::Cameras => "Cameras",
            CardGroup::Outputs => "Outputs",
        }
    }

    /// Hover text of the group's (+) button.
    pub(crate) fn add_hover(self) -> &'static str {
        match self {
            CardGroup::General => "",
            CardGroup::LaunchMonitors => "Add a launch monitor",
            CardGroup::Cameras => "Add a camera",
            CardGroup::Outputs => "Add an output",
        }
    }
}

/// Every configurable actor type (its config section and actor ID prefix),
/// in display order, with its group.
const ACTOR_TYPES: &[(&str, CardGroup)] = &[
    ("webserver", CardGroup::General),
    ("mevo", CardGroup::LaunchMonitors),
    ("r10", CardGroup::LaunchMonitors),
    ("square", CardGroup::LaunchMonitors),
    ("exputt", CardGroup::LaunchMonitors),
    ("trackball", CardGroup::LaunchMonitors),
    ("mock_monitor", CardGroup::LaunchMonitors),
    ("openconnect_server", CardGroup::LaunchMonitors),
    ("camera", CardGroup::Cameras),
    ("openconnect_client", CardGroup::Outputs),
    ("pureplay", CardGroup::Outputs),
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

/// Whether a card of this type offers Reconnect: every launch monitor and
/// output, not a camera or a General card.
pub(crate) fn reconnectable(type_key: &str) -> bool {
    matches!(
        group_of(type_key),
        CardGroup::LaunchMonitors | CardGroup::Outputs
    )
}

/// Whether a card of this type offers Remove. flighthook runs one web
/// server, which serves this UI: it stays.
pub(crate) fn removable(type_key: &str) -> bool {
    type_key != "webserver"
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

/// An entry of a (+) menu: the actor type it adds and the new card's name
/// and address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AddPreset {
    /// The menu label.
    pub(crate) label: &'static str,
    /// The config section and actor ID prefix.
    pub(crate) type_key: &'static str,
    /// The new card's name.
    pub(crate) name: &'static str,
    /// The new card's address: the bind address of a server, blank for a
    /// device that is auto-discovered.
    pub(crate) address: &'static str,
    /// Offered on the first-run welcome screen ([`welcome_options`]).
    pub(crate) welcome: bool,
    /// The welcome screen's label, when it differs from [`Self::label`].
    pub(crate) welcome_label: Option<&'static str>,
    /// The welcome screen's hover text.
    pub(crate) welcome_hover: Option<&'static str>,
}

const fn preset(
    label: &'static str,
    type_key: &'static str,
    name: &'static str,
    address: &'static str,
) -> AddPreset {
    AddPreset {
        label,
        type_key,
        name,
        address,
        welcome: true,
        welcome_label: None,
        welcome_hover: None,
    }
}

impl AddPreset {
    /// This preset labeled `label` on the welcome screen.
    const fn welcome_as(self, label: &'static str) -> Self {
        Self {
            welcome_label: Some(label),
            ..self
        }
    }

    /// This preset with hover text `hover` on the welcome screen.
    const fn welcome_hover(self, hover: &'static str) -> Self {
        Self {
            welcome_hover: Some(hover),
            ..self
        }
    }

    /// This preset left off the welcome screen.
    const fn not_on_welcome(self) -> Self {
        Self {
            welcome: false,
            ..self
        }
    }

    /// The label on the welcome screen.
    pub(crate) fn welcome_text(&self) -> &'static str {
        self.welcome_label.unwrap_or(self.label)
    }

    /// Whether this preset and `other` add the same connection: the same
    /// section at the same address, so a card from the second would start
    /// off. The welcome screen lets only one of them be checked.
    pub(crate) fn same_connection(&self, other: &AddPreset) -> bool {
        self.label != other.label
            && self.type_key == other.type_key
            && !self.address.is_empty()
            && self.address == other.address
    }
}

const LAUNCH_MONITOR_PRESETS: &[AddPreset] = &[
    preset("Mevo", "mevo", "Mevo WiFi", "192.168.2.1:5100")
        .welcome_as("FlightScope Mevo / Mevo+ / Xi"),
    preset("R10", "r10", "Garmin R10", ""),
    preset("Square Golf Omni", "square", "Square Golf Omni", ""),
    preset("ExPutt", "exputt", "ExPutt", ""),
    preset("Trackball", "trackball", "Trackball", ""),
    preset(
        "Uneekor (Open Connect)",
        "openconnect_server",
        "Uneekor",
        "0.0.0.0:921",
    )
    .welcome_hover(
        "Uneekor's own software pushes shots to flighthook over GSPro \
         Open Connect. Needs a Uneekor third-party license, and \
         flighthook binds port 921 — to run GSPro on the same host, \
         move it to 922 with OpenAPIUseAltPort.",
    ),
    preset(
        "Open Connect Server",
        "openconnect_server",
        "Open Connect Server",
        "0.0.0.0:921",
    )
    .not_on_welcome(),
];

const CAMERA_PRESETS: &[AddPreset] = &[preset("Camera", "camera", "Camera", "")];

const OUTPUT_PRESETS: &[AddPreset] = &[
    preset(
        "GSPro (Open Connect)",
        "openconnect_client",
        "Open Connect",
        "127.0.0.1:921",
    ),
    preset(
        "Pureplay (Open Connect)",
        "openconnect_client",
        "Open Connect",
        "127.0.0.1:921",
    )
    .welcome_hover(
        "Uses the same Open Connect connection as GSPro, so only one of \
         the two can be checked.",
    ),
    preset(
        "Muni Golf (Open Connect)",
        "openconnect_client",
        "Muni Golf",
        "127.0.0.1:9210",
    ),
];

/// What a group's (+) menu can offer, in menu order.
pub(crate) fn add_options(group: CardGroup) -> &'static [AddPreset] {
    match group {
        CardGroup::General => &[],
        CardGroup::LaunchMonitors => LAUNCH_MONITOR_PRESETS,
        CardGroup::Cameras => CAMERA_PRESETS,
        CardGroup::Outputs => OUTPUT_PRESETS,
    }
}

/// What a group's (+) menu offers on a server that can read a trackball
/// (`trackball`) or not: [`add_options`], the Trackball only where it works.
pub(crate) fn offered(group: CardGroup, trackball: bool) -> Vec<&'static AddPreset> {
    add_options(group)
        .iter()
        .filter(|p| trackball || p.type_key != "trackball")
        .collect()
}

/// What the first-run welcome screen offers in `group`: the (+) menu's
/// presets ([`offered`]) marked for it, none for General and Cameras.
pub(crate) fn welcome_options(group: CardGroup, trackball: bool) -> Vec<&'static AddPreset> {
    match group {
        CardGroup::LaunchMonitors | CardGroup::Outputs => offered(group, trackball)
            .into_iter()
            .filter(|p| p.welcome)
            .collect(),
        CardGroup::General | CardGroup::Cameras => Vec::new(),
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
/// that opens a menu of the group's presets ([`offered`], with the
/// Trackball where the server can read one: `trackball`). Disabled unless
/// `enabled`. Returns the preset picked.
pub(crate) fn add_heading(
    ui: &mut egui::Ui,
    group: CardGroup,
    enabled: bool,
    trackball: bool,
) -> Option<&'static AddPreset> {
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
                for preset in offered(group, trackball) {
                    let item = ui.add(egui::Button::new(preset.label));
                    devices::mark(
                        ui,
                        || format!("add:{}:{}", group.title(), preset.label),
                        &item,
                    );
                    if item.clicked() {
                        picked = Some(preset);
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
/// card `i` and fills the width it is given. Each column, and each card in
/// it, is a fixed-width child, so a card that overflows never widens the
/// grid or the cards under it. The grid leaves
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
            // Each card in a slot of its own, so one that overflows never
            // widens the cards under it.
            let top = column.cursor().min;
            let mut slot = column.new_child(
                egui::UiBuilder::new()
                    .id_salt(("card_slot", i))
                    .max_rect(egui::Rect::from_min_size(
                        top,
                        egui::vec2(width, column.available_height()),
                    ))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            slot.set_width(width);
            card(&mut slot, i);
            let used = egui::vec2(width, slot.min_rect().height());
            column.allocate_rect(egui::Rect::from_min_size(top, used), egui::Sense::hover());
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
            "exputt",
            "trackball",
            "mock_monitor",
            "openconnect_server",
        ] {
            assert_eq!(group_of(t), CardGroup::LaunchMonitors, "{t}");
        }
        assert_eq!(group_of("webserver"), CardGroup::General);
        assert_eq!(group_of("camera"), CardGroup::Cameras);
        for t in ["openconnect_client", "pureplay", "random_club"] {
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
            exputt: _,
            trackball: _,
            mock_monitor: _,
            openconnect_server: _,
            openconnect_client: _,
            pureplay: _,
            random_club: _,
            replay: _,
            body_tracking: _,
            player: _,
            camera: _,
        } = flighthook::FlighthookConfig::default();
        for key in [
            "webserver",
            "mevo",
            "r10",
            "square",
            "exputt",
            "trackball",
            "mock_monitor",
            "openconnect_server",
            "openconnect_client",
            "pureplay",
            "random_club",
            "camera",
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
            "openconnect_client.10",
            "openconnect_server.0",
            "openconnect_client.2",
            "mevo.1",
            "random_club.0",
            "r10.0",
            "mevo.0",
            "mock_monitor.0",
            "exputt.0",
            "square.0",
            "camera.1",
            "custom.0",
            "camera.0",
        ];
        let groups = grouped(ids.iter().map(|id| {
            let (t, i) = split_actor_id(id);
            (t, i, *id)
        }));
        assert_eq!(
            groups,
            vec![
                (CardGroup::General, vec!["webserver.0"]),
                (
                    CardGroup::LaunchMonitors,
                    vec![
                        "mevo.0",
                        "mevo.1",
                        "r10.0",
                        "square.0",
                        "exputt.0",
                        "mock_monitor.0",
                        "openconnect_server.0",
                    ]
                ),
                (CardGroup::Cameras, vec!["camera.0", "camera.1"]),
                (
                    CardGroup::Outputs,
                    vec![
                        "openconnect_client.2",
                        "openconnect_client.10",
                        "random_club.0",
                        "custom.0",
                    ]
                ),
            ]
        );
    }

    #[test]
    fn the_welcome_screen_takes_its_entries_from_the_add_menus() {
        let labels = |g, trackball| {
            welcome_options(g, trackball)
                .iter()
                .map(|p| p.welcome_text())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(CardGroup::LaunchMonitors, true),
            [
                "FlightScope Mevo / Mevo+ / Xi",
                "R10",
                "Square Golf Omni",
                "ExPutt",
                "Trackball",
                "Uneekor (Open Connect)",
            ]
        );
        assert!(!labels(CardGroup::LaunchMonitors, false).contains(&"Trackball"));
        assert_eq!(
            labels(CardGroup::Outputs, false),
            [
                "GSPro (Open Connect)",
                "Pureplay (Open Connect)",
                "Muni Golf (Open Connect)",
            ]
        );
        assert!(welcome_options(CardGroup::Cameras, true).is_empty());
        assert!(welcome_options(CardGroup::General, true).is_empty());

        // Only GSPro and Pureplay over Open Connect share a connection.
        let all: Vec<_> = [CardGroup::LaunchMonitors, CardGroup::Outputs]
            .into_iter()
            .flat_map(|g| welcome_options(g, true))
            .collect();
        let mut pairs = Vec::new();
        for a in &all {
            for b in &all {
                if a.same_connection(b) {
                    pairs.push((a.label, b.label));
                }
            }
        }
        assert_eq!(
            pairs,
            [
                ("GSPro (Open Connect)", "Pureplay (Open Connect)"),
                ("Pureplay (Open Connect)", "GSPro (Open Connect)"),
            ]
        );
    }

    #[test]
    fn add_menus_offer_only_their_groups_types() {
        assert!(add_options(CardGroup::General).is_empty());
        let entries = |g| {
            add_options(g)
                .iter()
                .map(|p| (p.label, p.type_key, p.name, p.address))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            entries(CardGroup::LaunchMonitors),
            [
                ("Mevo", "mevo", "Mevo WiFi", "192.168.2.1:5100"),
                ("R10", "r10", "Garmin R10", ""),
                ("Square Golf Omni", "square", "Square Golf Omni", ""),
                ("ExPutt", "exputt", "ExPutt", ""),
                ("Trackball", "trackball", "Trackball", ""),
                (
                    "Uneekor (Open Connect)",
                    "openconnect_server",
                    "Uneekor",
                    "0.0.0.0:921"
                ),
                (
                    "Open Connect Server",
                    "openconnect_server",
                    "Open Connect Server",
                    "0.0.0.0:921"
                ),
            ]
        );
        assert_eq!(
            entries(CardGroup::Cameras),
            [("Camera", "camera", "Camera", "")]
        );
        assert_eq!(
            entries(CardGroup::Outputs),
            [
                (
                    "GSPro (Open Connect)",
                    "openconnect_client",
                    "Open Connect",
                    "127.0.0.1:921"
                ),
                (
                    "Pureplay (Open Connect)",
                    "openconnect_client",
                    "Open Connect",
                    "127.0.0.1:921"
                ),
                (
                    "Muni Golf (Open Connect)",
                    "openconnect_client",
                    "Muni Golf",
                    "127.0.0.1:9210"
                ),
            ]
        );
        assert!(
            CardGroup::ALL
                .iter()
                .flat_map(|g| add_options(*g))
                .all(|p| p.type_key != "webserver"),
            "no menu adds a web server"
        );
        for group in CardGroup::ALL {
            for p in add_options(group) {
                assert_eq!(group_of(p.type_key), group, "{}", p.label);
            }
        }
        // Labels name the controls the frame tests click, so they are unique.
        let mut labels: Vec<_> = CardGroup::ALL
            .iter()
            .flat_map(|g| add_options(*g).iter().map(|p| p.label))
            .collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 11);
    }

    /// The Trackball is offered only where the server can read one; the
    /// other presets always.
    #[test]
    fn the_trackball_is_offered_only_where_it_works() {
        let labels = |group, trackball| -> Vec<&str> {
            offered(group, trackball).iter().map(|p| p.label).collect()
        };
        let with = labels(CardGroup::LaunchMonitors, true);
        let without = labels(CardGroup::LaunchMonitors, false);
        assert_eq!(with.iter().filter(|l| **l == "Trackball").count(), 1);
        assert!(!without.contains(&"Trackball"), "{without:?}");
        assert_eq!(without.len(), with.len() - 1, "only the Trackball goes");
        assert!(without.iter().all(|l| with.contains(l)), "{without:?}");
        for group in CardGroup::ALL
            .into_iter()
            .filter(|g| *g != CardGroup::LaunchMonitors)
        {
            assert_eq!(labels(group, false), labels(group, true), "{group:?}");
            assert_eq!(
                labels(group, false).len(),
                add_options(group).len(),
                "{group:?}"
            );
        }
    }

    #[test]
    fn every_card_but_the_web_server_can_be_removed() {
        assert!(!removable("webserver"));
        for (t, _) in ACTOR_TYPES.iter().filter(|(t, _)| *t != "webserver") {
            assert!(removable(t), "{t}");
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
