//! The user guide of every Settings card: the page under `docs/` that explains its settings, and
//! the Docs link in each card's header that opens it on GitHub, at the version this build is.

use crate::panels::cards;
use crate::theme;

/// The guide of each card type, as a path under the repository's `docs/`.
/// A card type is an actor type (the part of a card's ID before the dot,
/// `mevo` of `mevo.0`), [`REMOTE`] for a program on another computer, or one
/// of the settings-only cards (`global`, `simulation`, `replay`,
/// `body_tracking`, `golfers`). Several kinds of card can share a page.
const PAGES: &[(&str, &str)] = &[
    ("global", "general.md"),
    ("simulation", "general.md"),
    ("webserver", "general.md"),
    ("mevo", "devices/flightscope.md"),
    ("r10", "devices/garmin-r10.md"),
    ("square", "devices/square-golf.md"),
    ("exputt", "devices/exputt.md"),
    ("trackball", "devices/trackball.md"),
    ("mock_monitor", "devices/mock.md"),
    ("openconnect_server", "devices/open-connect.md"),
    ("camera", "devices/cameras.md"),
    ("replay", "devices/cameras.md"),
    ("body_tracking", "devices/cameras.md"),
    ("openconnect_client", "outputs/open-connect.md"),
    ("pureplay", "outputs/pureplay.md"),
    ("pureplay_accessory", "devices/pureplay.md"),
    ("random_club", "outputs/random-club.md"),
    (REMOTE, "devices/remote.md"),
    ("golfers", "golfers.md"),
];

/// The card type of a program on another computer that connects to
/// flighthook's FRP socket (a remote card, keyed `remote:<actor id>`).
const REMOTE: &str = "remote";

/// The card type of the card a header names: `key` is a card's ID
/// (`mevo.0`), a remote card's key (`remote:<actor id>`) or a settings-only
/// card's key (`global`).
fn card_type(key: &str) -> &str {
    if key.starts_with("remote:") {
        REMOTE
    } else {
        cards::split_actor_id(key).0
    }
}

/// The guide of the card a header names (see [`card_type`]), as a path
/// under `docs/`; `None` for a card type without one.
pub(crate) fn page(key: &str) -> Option<&'static str> {
    let card_type = card_type(key);
    PAGES
        .iter()
        .find(|(t, _)| *t == card_type)
        .map(|(_, page)| *page)
}

/// Where `page` (a path under `docs/`) is on GitHub, at this build's
/// release tag.
fn url(page: &str) -> String {
    format!(
        "https://github.com/divotmaker/flighthook/blob/v{}/docs/{page}",
        env!("CARGO_PKG_VERSION")
    )
}

/// The Docs link of the card header `key` names: a book and the word
/// "Docs", opening the card's guide in a new tab. Nothing for a card type
/// without a guide. Marked `docs:<key>`.
pub(crate) fn link(ui: &mut egui::Ui, key: &str, height: f32) {
    let Some(page) = page(key) else {
        return;
    };
    let response = theme::icon_text(ui, theme::Icon::Book, "Docs", height, theme::ACCENT)
        .on_hover_text("Open this card's guide");
    crate::panels::devices::mark(ui, || format!("docs:{key}"), &response);
    if response.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url(page)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::settings_nav::SettingsItem;

    /// The header key of each page of `item`, matched without a wildcard:
    /// a new kind of Settings page fails to compile here until it is given
    /// one.
    fn header_keys(item: &SettingsItem) -> Vec<String> {
        match item {
            SettingsItem::Global
            | SettingsItem::Simulation
            | SettingsItem::Replay
            | SettingsItem::BodyTracking => vec![item.key()],
            SettingsItem::Card(_) => cards::ACTOR_TYPES
                .iter()
                .map(|(t, _)| format!("{t}.0"))
                .chain(["remote:abc.0".to_owned()])
                .collect(),
            SettingsItem::Golfer(_) => vec!["golfers".to_owned()],
        }
    }

    #[test]
    fn every_card_type_has_a_guide_in_docs() {
        let items = [
            SettingsItem::Global,
            SettingsItem::Simulation,
            SettingsItem::Replay,
            SettingsItem::BodyTracking,
            SettingsItem::Card(String::new()),
            SettingsItem::Golfer(0),
        ];
        let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs");
        for key in items.iter().flat_map(header_keys) {
            let page = page(&key).unwrap_or_else(|| panic!("{key}: no guide in docs::PAGES"));
            assert!(docs.join(page).is_file(), "{key}: docs/{page} is missing");
        }
    }
}
