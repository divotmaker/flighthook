//! Auto-Chip (beta): a menu chip in the header, right of the club picker,
//! that sends a short straight chip to the sim (`POST /api/auto-chip`). It
//! shows while the `[simulation] auto_chip` setting enables it and is
//! disabled, with the reason as its hover text, while no simulator is
//! connected or a chip is on its way. A refused chip shows in the error banner
//! ([`super::monitoring`]).

use std::collections::HashMap;

use crate::app::FlighthookApp;
use crate::types::ActorStatusResponse;

/// Hover text on the Auto-chip button.
pub(crate) const AUTO_CHIP_HINT: &str =
    "Send a short straight chip to the sim, for chips your launch monitor can't read.";

/// The Auto-chip menu's items, in order: label and total distance (yards).
pub(crate) fn auto_chip_items() -> [(String, u32); 3] {
    flighthook::AUTO_CHIP_YARDS.map(|yards| (format!("{yards} Yard"), yards))
}

/// Why the Auto-Chip button is disabled, or `None` when it is enabled: a
/// chip is on its way, or no output that forwards shots (an Open Connect client
/// or Pureplay) is connected.
pub(crate) fn auto_chip_blocked(
    actors: &HashMap<String, ActorStatusResponse>,
    in_flight: Option<u32>,
) -> Option<String> {
    if in_flight.is_some() {
        Some("Sending the chip\u{2026}".to_owned())
    } else if !crate::app::simulator_connected(actors) {
        Some("Auto-Chip: no simulator connected. Connect an Open Connect Client first.".to_owned())
    } else {
        None
    }
}

impl FlighthookApp {
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
    use crate::theme;

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
            let _ = ctx.run(input, |ctx| app.render_header_in_test(ctx));
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
        // A frame for the header to take the new status.
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

    /// In the header the chip stays where it is, disabled: a click opens no
    /// menu while a chip is on its way, and the menu opens again after it.
    #[test]
    fn the_header_chip_is_read_only_while_a_chip_is_on_its_way() {
        use egui::{Event, Modifiers, PointerButton};

        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = enabled_app();
        app.actors.insert(
            "openconnect_client.0".into(),
            ActorStatusResponse::new("GSPro", crate::types::ActorStatus::Connected),
        );
        app.auto_chip_in_flight = Some(3);
        let click = |app: &mut FlighthookApp, at: egui::Pos2| -> Marks {
            let button = |pressed| Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            for events in [
                vec![Event::PointerMoved(at)],
                vec![button(true)],
                vec![button(false)],
            ] {
                ctx.data_mut(|d| d.remove::<Marks>(egui::Id::NULL));
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1600.0, 900.0),
                    )),
                    events,
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| app.render_header_in_test(ctx));
            }
            header_marks(&ctx, app, 1600.0)
        };
        let menu = |marks: &Marks| marks.keys().any(|k| k.starts_with("auto-chip:"));

        header_marks(&ctx, &mut app, 1600.0);
        let chip = header_marks(&ctx, &mut app, 1600.0)["auto-chip"].0;
        assert!(!menu(&click(&mut app, chip.center())), "read-only");
        assert_eq!(app.auto_chip_in_flight, Some(3));

        app.auto_chip_in_flight = None;
        let marks = click(&mut app, chip.center());
        assert_eq!(marks["auto-chip"].0, chip, "same place");
        assert!(menu(&marks), "the menu opens once the chip is answered");
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
        let _ = ctx.run(input, |ctx| app.render_header_in_test(ctx));
        ctx.data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default()
    }
}
