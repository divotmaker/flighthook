//! Auto-chip (beta): a menu chip in the header, right of the club picker,
//! that sends a short straight chip to the sim (`POST /api/auto-chip`). It
//! shows while the `[simulation] auto_chip` setting enables it and is
//! disabled, with the reason as its hover text, while no simulator is
//! connected or a chip is on its way. A refused chip shows in the error
//! banner ([`super::monitoring`]).

use std::collections::HashMap;

use super::devices;
use crate::app::FlighthookApp;
use crate::theme;
use crate::types::ActorStatusResponse;

/// Hover text on the Auto-chip button.
const AUTO_CHIP_HINT: &str =
    "Send a short straight chip to the sim, for chips your launch monitor can't read.";

/// The Auto-chip menu's items, in order: label and total distance (yards).
fn auto_chip_items() -> [(String, u32); 3] {
    flighthook::AUTO_CHIP_YARDS.map(|yards| (format!("{yards} Yard"), yards))
}

/// Why the Auto-chip button is disabled, or `None` when it is enabled: a
/// chip is on its way, or no output that forwards shots (an Open Connect
/// client) is connected.
fn auto_chip_blocked(
    actors: &HashMap<String, ActorStatusResponse>,
    in_flight: Option<u32>,
) -> Option<&'static str> {
    if in_flight.is_some() {
        Some("Sending the chip\u{2026}")
    } else if !crate::app::set_by_simulator(actors) {
        Some("Auto-chip: no simulator connected. Connect an OpenConnect Client output first.")
    } else {
        None
    }
}

impl FlighthookApp {
    /// The header's Auto-chip chip, a gap right of the club pill, while the
    /// settings enable it ([`SimulationSection::auto_chip`]). Returns the
    /// distance picked this frame.
    pub(crate) fn header_auto_chip(&self, ui: &mut egui::Ui) -> Option<u32> {
        if !self.settings.auto_chip_enabled() {
            return None;
        }
        ui.add_space(10.0);
        self.render_auto_chip(ui)
    }

    /// The Auto-chip button and its menu ([`auto_chip_items`]). Returns the
    /// distance picked this frame.
    fn render_auto_chip(&self, ui: &mut egui::Ui) -> Option<u32> {
        let blocked = auto_chip_blocked(&self.actors, self.auto_chip_in_flight);
        let button = theme::menu_chip(ui, "Auto-chip", blocked.is_none(), egui::vec2(0.0, 26.0));
        devices::mark(ui, || "auto-chip".to_owned(), &button);
        let button = button.on_hover_text(blocked.unwrap_or(AUTO_CHIP_HINT));
        let mut picked = None;
        egui::Popup::menu(&button).show(|ui| {
            for (label, yards) in auto_chip_items() {
                let item = ui.button(label.as_str());
                devices::mark(ui, || format!("auto-chip:{label}"), &item);
                if item.clicked() {
                    picked = Some(yards);
                }
            }
        });
        picked
    }

    /// Send an Auto-chip of `yards`: the button waits for the reply, and a
    /// failure shows in the error banner.
    pub(crate) fn send_auto_chip(&mut self, ctx: &egui::Context, yards: u32) {
        self.auto_chip_error = None;
        self.auto_chip_in_flight = Some(yards);
        // The UI tests pick from the menu with no server behind them.
        #[cfg(not(test))]
        crate::net::post_auto_chip(ctx, &self.pending, yards);
        #[cfg(test)]
        let _ = ctx;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::devices::Marks;

    /// An app whose settings have loaded with Auto-chip as configured by
    /// `auto_chip`.
    fn app_with_setting(auto_chip: bool) -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(crate::net::new_pending());
        let mut config = crate::types::FlighthookConfig::default();
        config.simulation.auto_chip = auto_chip;
        app.settings.load_from(&config);
        app
    }

    fn enabled_app() -> FlighthookApp {
        app_with_setting(true)
    }

    /// The Auto-chip button opens a menu of 1, 3 and 5 Yard, in that order;
    /// picking one sends that chip and holds the button until the reply. It
    /// is disabled while no simulator is connected.
    #[test]
    fn auto_chip_menu_lists_three_distances_and_sends_the_pick() {
        use egui::{Event, Modifiers, PointerButton, Pos2, Rect, vec2};

        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = enabled_app();
        let frame = |app: &mut FlighthookApp, events: Vec<Event>| -> Marks {
            ctx.data_mut(|d| d.remove::<Marks>(egui::Id::NULL));
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| app.render_header(ctx));
            ctx.data(|d| d.get_temp::<Marks>(egui::Id::NULL))
                .unwrap_or_default()
        };
        let click = |app: &mut FlighthookApp, at: Pos2| -> Marks {
            let button = |pressed| Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            frame(app, vec![Event::PointerMoved(at)]);
            frame(app, vec![button(true)]);
            frame(app, vec![button(false)]);
            frame(app, Vec::new())
        };
        let items = |marks: &Marks| -> Vec<(String, f32)> {
            let mut items: Vec<(String, f32)> = marks
                .iter()
                .filter_map(|(k, (rect, _))| {
                    k.strip_prefix("auto-chip:")
                        .map(|l| (l.to_owned(), rect.top()))
                })
                .collect();
            items.sort_by(|a, b| a.1.total_cmp(&b.1));
            items
        };

        // No simulator connected: disabled, and a click opens nothing.
        let button = frame(&mut app, Vec::new())["auto-chip"].0;
        assert!(items(&click(&mut app, button.center())).is_empty());
        assert_eq!(app.auto_chip_in_flight, None);

        // GSPro connected: the menu opens with the three distances in order.
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", crate::types::ActorStatus::Connected),
        );
        // The club pill turns read-only and the chip moves with it.
        frame(&mut app, Vec::new());
        let button = frame(&mut app, Vec::new())["auto-chip"].0;
        let marks = click(&mut app, button.center());
        let labels: Vec<String> = items(&marks).into_iter().map(|(l, _)| l).collect();
        assert_eq!(labels, ["1 Yard", "3 Yard", "5 Yard"]);

        // Picking 3 Yard sends it and closes the menu.
        let three = marks["auto-chip:3 Yard"].0.center();
        let marks = click(&mut app, three);
        assert_eq!(app.auto_chip_in_flight, Some(3));
        assert!(items(&marks).is_empty(), "menu closed");
        let marks = click(&mut app, button.center());
        assert!(items(&marks).is_empty(), "held until the reply");

        // A failed reply frees the button and shows in the error banner.
        app.pending.lock().expect("pending").auto_chip = Some(Err("409 no".into()));
        app.apply_pending();
        assert_eq!(app.auto_chip_in_flight, None);
        assert_eq!(app.auto_chip_error.as_deref(), Some("409 no"));
        let marks = click(&mut app, button.center());
        assert_eq!(items(&marks).len(), 3, "free again");

        // The next pick clears it; a good reply leaves it clear.
        app.send_auto_chip(&ctx, 1);
        assert_eq!(app.auto_chip_error, None);
        assert_eq!(app.auto_chip_in_flight, Some(1));
        app.pending.lock().expect("pending").auto_chip = Some(Ok(()));
        app.apply_pending();
        assert_eq!((app.auto_chip_in_flight, app.auto_chip_error), (None, None));
    }

    #[test]
    fn auto_chip_hover_says_why_it_is_disabled() {
        let mut actors = HashMap::new();
        assert!(auto_chip_blocked(&actors, None).is_some_and(|w| w.contains("no simulator")));
        actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", crate::types::ActorStatus::Reconnecting),
        );
        assert!(auto_chip_blocked(&actors, None).is_some());
        actors.insert(
            "openconnect_client.1".into(),
            ActorStatusResponse::new("Muni Golf", crate::types::ActorStatus::Connected),
        );
        assert_eq!(auto_chip_blocked(&actors, None), None);
        assert!(auto_chip_blocked(&actors, Some(5)).is_some_and(|w| w.starts_with("Sending")));
    }

    /// One pass of the header on a 1600×900 screen; its marked controls.
    fn header_marks(ctx: &egui::Context, app: &mut FlighthookApp, width: f32) -> Marks {
        ctx.data_mut(|d| d.remove::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 900.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.render_header(ctx));
        ctx.data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default()
    }

    /// The chip sits in the header right of the Club picker, on its centre
    /// line, and is gone from the Shots toolbar; with the setting off, or
    /// before the settings have loaded, there is no chip at all.
    #[test]
    fn the_chip_is_in_the_header_right_of_club_and_only_while_enabled() {
        let ctx = egui::Context::default();
        theme::install(&ctx);

        let mut app = enabled_app();
        header_marks(&ctx, &mut app, 1600.0);
        let marks = header_marks(&ctx, &mut app, 1600.0);
        let (chip, club) = (marks["auto-chip"].0, marks["club"].0);
        assert!(club.right() < chip.left(), "right of the club picker");
        assert!(chip.left() - club.right() < 40.0, "next to it");
        assert!((chip.center().y - club.center().y).abs() <= 2.5, "one line");
        assert!(chip.right() < 1600.0 - 160.0, "clear of the title");

        // Read-only club (a simulator connected): the chip is still there.
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", crate::types::ActorStatus::Connected),
        );
        header_marks(&ctx, &mut app, 1600.0);
        let marks = header_marks(&ctx, &mut app, 1600.0);
        assert!(marks["club"].0.right() < marks["auto-chip"].0.left());

        // Disabled in the settings, which is the default: no chip.
        let mut loaded = FlighthookApp::with_pending(crate::net::new_pending());
        loaded
            .settings
            .load_from(&crate::types::FlighthookConfig::default());
        header_marks(&ctx, &mut loaded, 1600.0);
        assert!(!header_marks(&ctx, &mut loaded, 1600.0).contains_key("auto-chip"));
        // Explicitly off is the same.
        let mut off = app_with_setting(false);
        header_marks(&ctx, &mut off, 1600.0);
        assert!(!header_marks(&ctx, &mut off, 1600.0).contains_key("auto-chip"));
        // Settings not loaded yet: none either.
        let mut fresh = FlighthookApp::with_pending(crate::net::new_pending());
        header_marks(&ctx, &mut fresh, 1600.0);
        assert!(!header_marks(&ctx, &mut fresh, 1600.0).contains_key("auto-chip"));
    }

    /// The header still holds the chip at the smallest window the UI fits
    /// (1024 wide): the tabs, mode, hand, club, chip and title all fit on
    /// the one row without overlapping.
    #[test]
    fn the_header_fits_the_chip_at_1024_wide() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = enabled_app();
        header_marks(&ctx, &mut app, 1024.0);
        let marks = header_marks(&ctx, &mut app, 1024.0);
        let (chip, title) = (marks["auto-chip"].0, marks["title"].0);
        assert!(chip.right() <= title.left() - 8.0, "{chip:?} vs {title:?}");
        assert!(chip.right() <= 1024.0);
    }
}
