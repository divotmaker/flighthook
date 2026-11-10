//! Battery indicators for the header: one small battery per connected device
//! that reports a charge, filled to the charge and named on hover.

use std::collections::HashMap;

use crate::panels::devices::device_card_order;
use crate::panels::settings::ActorFormEntry;
use crate::theme;
use crate::types::{ActorStatusResponse, ConnectionState};

/// Telemetry key a device reports its charge under, as a whole percent.
const KEY: &str = "battery_pct";

/// Charges below this many percent are drawn red.
const LOW_PCT: u8 = 20;

/// Width and height of the battery body.
const BODY: egui::Vec2 = egui::vec2(22.0, 11.0);
/// Width and height of the terminal nub, and the gap between it and the body.
const NUB: egui::Vec2 = egui::vec2(2.0, 5.0);
const NUB_GAP: f32 = 0.5;
/// Space between the outline and the fill.
const INSET: f32 = 2.0;
/// Space between two indicators.
pub(crate) const GAP: f32 = 6.0;

/// A connected device's charge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Battery {
    /// The device's actor ID.
    pub(crate) actor: String,
    /// The name on the device's card.
    pub(crate) name: String,
    /// Percent charged, 0 to 100.
    pub(crate) pct: u8,
}

impl Battery {
    /// The hover text: `Square Omni 74%`.
    pub(crate) fn tooltip(&self) -> String {
        format!("{} {}%", self.name, self.pct)
    }
}

/// The charge `value` reports: a whole number from 0 to 100.
fn parse_pct(value: &str) -> Option<u8> {
    value.trim().parse::<u8>().ok().filter(|p| *p <= 100)
}

/// One battery per actor that is shown Connected and reports a valid
/// `battery_pct`, in Devices card order.
pub(crate) fn batteries(
    actors: &HashMap<String, ActorStatusResponse>,
    form: &[ActorFormEntry],
) -> Vec<Battery> {
    device_card_order(actors, form)
        .into_iter()
        .flat_map(|(_, cards)| cards)
        .filter_map(|card| {
            let actor = actors.get(&card.id)?;
            if actor.state() != ConnectionState::Connected {
                return None;
            }
            let pct = parse_pct(actor.telemetry.get(KEY)?)?;
            // The title of the card's Telemetry side.
            let name = Some(actor.name.clone())
                .filter(|n| !n.is_empty())
                .or_else(|| {
                    card.form
                        .and_then(|i| form.get(i))
                        .map(|f| f.name().to_string())
                })
                .unwrap_or_else(|| card.id.clone());
            Some(Battery {
                actor: card.id,
                name,
                pct,
            })
        })
        .collect()
}

/// Red below [`LOW_PCT`] percent, else green.
pub(crate) fn fill_color(pct: u8) -> egui::Color32 {
    if pct < LOW_PCT {
        theme::STATUS_DOWN
    } else {
        theme::STATUS_OK
    }
}

/// The room a battery takes: body and nub.
pub(crate) fn size() -> egui::Vec2 {
    egui::vec2(BODY.x + NUB_GAP + NUB.x, BODY.y)
}

/// Where a battery in `rect` has its body, nub and fill (`pct` percent of
/// the room inside the outline, from the left).
fn parts(rect: egui::Rect, pct: u8) -> (egui::Rect, egui::Rect, egui::Rect) {
    let body = egui::Rect::from_min_size(rect.min, BODY);
    let nub = egui::Rect::from_center_size(
        egui::pos2(body.right() + NUB_GAP + NUB.x / 2.0, body.center().y),
        NUB,
    );
    let inner = body.shrink(INSET);
    let fill = egui::Rect::from_min_size(
        inner.min,
        egui::vec2(inner.width() * f32::from(pct) / 100.0, inner.height()),
    );
    (body, nub, fill)
}

/// Draw `battery` and give it its hover text.
pub(crate) fn show(ui: &mut egui::Ui, battery: &Battery) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size(), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let (body, nub, fill) = parts(rect, battery.pct);
        let painter = ui.painter();
        painter.rect_filled(nub, 1.0, theme::TEXT_MUTED);
        painter.rect_filled(fill, 1.0, fill_color(battery.pct));
        painter.rect_stroke(
            body,
            3.0,
            egui::Stroke::new(1.0, theme::TEXT_MUTED),
            egui::StrokeKind::Inside,
        );
    }
    response.on_hover_text(battery.tooltip())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ActorStatus;

    fn actor(name: &str, status: ActorStatus, battery: Option<&str>) -> ActorStatusResponse {
        let mut a = ActorStatusResponse::new(name, status);
        if let Some(b) = battery {
            a.telemetry.insert(KEY.into(), b.into());
        }
        a
    }

    fn actors(list: &[(&str, ActorStatusResponse)]) -> HashMap<String, ActorStatusResponse> {
        list.iter()
            .map(|(id, a)| ((*id).to_owned(), a.clone()))
            .collect()
    }

    fn pcts(list: &[(&str, ActorStatusResponse)]) -> Vec<(String, u8)> {
        batteries(&actors(list), &[])
            .into_iter()
            .map(|b| (b.actor, b.pct))
            .collect()
    }

    #[test]
    fn a_connected_device_with_a_charge_shows() {
        let all = actors(&[(
            "square.0",
            actor("Square Omni", ActorStatus::Connected, Some("74")),
        )]);
        assert_eq!(
            batteries(&all, &[]),
            [Battery {
                actor: "square.0".into(),
                name: "Square Omni".into(),
                pct: 74
            }]
        );
    }

    #[test]
    fn a_device_that_is_not_connected_is_skipped() {
        for status in [
            ActorStatus::Starting,
            ActorStatus::Disconnected,
            ActorStatus::Reconnecting,
        ] {
            let list = [("square.0", actor("Omni", status, Some("74")))];
            assert!(pcts(&list).is_empty(), "{status:?}");
        }
        // Lost after being connected: shown Reconnecting.
        let mut lost = actor("Omni", ActorStatus::Connected, Some("74"));
        lost.observe(ActorStatus::Reconnecting, 1_000);
        assert!(pcts(&[("square.0", lost)]).is_empty());
        // Connecting: never connected since it started.
        let list = [("mevo.0", actor("Mevo", ActorStatus::Starting, Some("50")))];
        assert_eq!(list[0].1.state(), ConnectionState::Connecting);
        assert!(pcts(&list).is_empty());
    }

    #[test]
    fn a_missing_or_unreadable_charge_is_skipped() {
        for value in [
            None,
            Some(""),
            Some("abc"),
            Some("74%"),
            Some("7.5"),
            Some("-1"),
            Some("101"),
            Some("256"),
            Some("1000"),
        ] {
            let list = [("square.0", actor("Omni", ActorStatus::Connected, value))];
            assert!(pcts(&list).is_empty(), "{value:?}");
        }
    }

    #[test]
    fn the_ends_of_the_range_show() {
        for (value, pct) in [("0", 0), ("100", 100), (" 5 ", 5)] {
            let list = [(
                "square.0",
                actor("Omni", ActorStatus::Connected, Some(value)),
            )];
            assert_eq!(pcts(&list), [("square.0".to_owned(), pct)]);
        }
    }

    #[test]
    fn several_devices_come_in_card_order() {
        let on = |b| actor("x", ActorStatus::Connected, Some(b));
        let list = [
            ("square.0", on("10")),
            ("mevo.1", on("20")),
            ("mevo.0", on("30")),
            ("r10.0", actor("R10", ActorStatus::Connected, None)),
            ("square.1", on("40")),
        ];
        let got = pcts(&list);
        let ids: Vec<&str> = got.iter().map(|(id, _)| id.as_str()).collect();
        let order: Vec<String> = device_card_order(&actors(&list), &[])
            .into_iter()
            .flat_map(|(_, cards)| cards)
            .map(|c| c.id)
            .filter(|id| ids.contains(&id.as_str()))
            .collect();
        assert_eq!(ids, order);
        assert_eq!(ids.len(), 4);
        let pos = |id: &str| ids.iter().position(|i| *i == id).unwrap();
        assert!(pos("mevo.0") < pos("mevo.1"));
        assert!(pos("square.0") < pos("square.1"));
    }

    #[test]
    fn the_name_is_the_cards_title() {
        // The actor's own name, else the form's, else the ID.
        let list = [("square.0", actor("", ActorStatus::Connected, Some("74")))];
        assert_eq!(batteries(&actors(&list), &[])[0].name, "square.0");
    }

    #[test]
    fn under_twenty_percent_is_red_otherwise_green() {
        assert_eq!(fill_color(0), theme::STATUS_DOWN);
        assert_eq!(fill_color(19), theme::STATUS_DOWN);
        assert_eq!(fill_color(20), theme::STATUS_OK);
        assert_eq!(fill_color(100), theme::STATUS_OK);
        assert_ne!(theme::STATUS_DOWN, theme::STATUS_OK);
    }

    #[test]
    fn the_fill_is_the_charge_of_the_inner_width() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 5.0), size());
        let inner = BODY.x - 2.0 * INSET;
        for (pct, width) in [(0, 0.0), (50, inner / 2.0), (100, inner)] {
            let (body, nub, fill) = parts(rect, pct);
            assert!((fill.width() - width).abs() < 1e-4, "{pct}%");
            // From the left of the inside; never past it.
            assert!((fill.left() - (body.left() + INSET)).abs() < 1e-4);
            assert!(fill.right() <= body.right() - INSET + 1e-4);
            assert!(nub.left() > body.right() && nub.right() <= rect.right() + 1e-4);
        }
    }

    use crate::app::FlighthookApp;
    use crate::net;
    use crate::panels::devices::Marks;
    use egui::{Event, Pos2, Rect, vec2};

    fn flatten(shape: &egui::Shape, out: &mut Vec<egui::Shape>) {
        match shape {
            egui::Shape::Vec(inner) => inner.iter().for_each(|s| flatten(s, out)),
            other => out.push(other.clone()),
        }
    }

    /// A header with a Mevo+ at 15 %, an Xi without a charge and a Square
    /// Omni at 74 %, drawn in a window of `size`.
    fn header(
        ctx: &egui::Context,
        app: &mut FlighthookApp,
        size: egui::Vec2,
        events: Vec<Event>,
    ) -> (Marks, Vec<egui::Shape>) {
        ctx.data_mut(|d| d.remove_temp::<Marks>(egui::Id::NULL));
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            events,
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.render_header(ctx));
        let marks = ctx
            .data(|d| d.get_temp::<Marks>(egui::Id::NULL))
            .unwrap_or_default();
        let mut shapes = Vec::new();
        for clipped in &output.shapes {
            flatten(&clipped.shape, &mut shapes);
        }
        (marks, shapes)
    }

    fn app_with_batteries(charges: &[(&str, &str, &str)]) -> FlighthookApp {
        let mut app = FlighthookApp::with_pending(net::new_pending());
        for (id, name, pct) in charges {
            let mut a = ActorStatusResponse::new(*name, ActorStatus::Connected);
            if !pct.is_empty() {
                a.telemetry.insert(KEY.into(), (*pct).into());
            }
            app.actors.insert((*id).into(), a);
        }
        app
    }

    #[test]
    fn the_icons_sit_left_of_the_title_inside_the_header() {
        let charges = [
            ("mevo.0", "Mevo+", "15"),
            ("mevo.1", "Xi", ""),
            ("square.0", "Square Omni", "74"),
        ];
        for (w, h) in [(1024.0, 768.0), (1600.0, 900.0)] {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let mut app = app_with_batteries(&charges);
            let _ = header(&ctx, &mut app, vec2(w, h), Vec::new());
            let (marks, _) = header(&ctx, &mut app, vec2(w, h), Vec::new());
            let at = format!("{w}x{h}");
            assert!(!marks.contains_key("battery:mevo.1"), "{at}");
            let (mevo, square) = (marks["battery:mevo.0"].0, marks["battery:square.0"].0);
            let title = marks["title"].0;
            let club = marks["club"].0;
            // Card order: the Mevo furthest left.
            assert!(mevo.right() + GAP - 0.5 <= square.left(), "{at}");
            assert!(square.right() < title.left(), "{at}");
            assert!(
                club.right() < mevo.left(),
                "{at}: batteries over the club pill"
            );
            for r in [mevo, square] {
                assert!((r.size() - size()).abs().max_elem() < 0.01, "{at}");
                assert!(r.top() >= 0.0 && r.bottom() <= 58.0, "{at}: {r:?}");
            }
            assert!(title.right() <= w, "{at}");
        }
    }

    #[test]
    fn no_batteries_leave_the_header_as_it_was() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = app_with_batteries(&[("r10.0", "R10", "")]);
        let (marks, _) = header(&ctx, &mut app, vec2(1600.0, 900.0), Vec::new());
        assert!(!marks.keys().any(|k| k.starts_with("battery:")));
    }

    #[test]
    fn the_icons_are_filled_red_or_green_and_hover_names_the_device() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app =
            app_with_batteries(&[("mevo.0", "Mevo+", "15"), ("square.0", "Square Omni", "74")]);
        let size = vec2(1600.0, 900.0);
        let _ = header(&ctx, &mut app, size, Vec::new());
        let (marks, shapes) = header(&ctx, &mut app, size, Vec::new());
        let fills: Vec<(Rect, egui::Color32)> = shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Rect(r)
                    if r.fill == theme::STATUS_DOWN || r.fill == theme::STATUS_OK =>
                {
                    Some((r.rect, r.fill))
                }
                _ => None,
            })
            .filter(|(r, _)| {
                marks
                    .iter()
                    .any(|(k, v)| k.starts_with("battery:") && v.0.contains_rect(*r))
            })
            .collect();
        assert_eq!(fills.len(), 2, "{fills:?}");
        let inner = BODY.x - 2.0 * INSET;
        let red = fills.iter().find(|f| f.1 == theme::STATUS_DOWN).unwrap();
        let green = fills.iter().find(|f| f.1 == theme::STATUS_OK).unwrap();
        assert!(marks["battery:mevo.0"].0.contains_rect(red.0));
        assert!((red.0.width() - inner * 0.15).abs() < 0.01);
        assert!(marks["battery:square.0"].0.contains_rect(green.0));
        assert!((green.0.width() - inner * 0.74).abs() < 0.01);

        // Hover the Omni's icon: the tooltip is the only new text.
        let over = marks["battery:square.0"].0.center();
        let texts = |shapes: &[egui::Shape]| -> Vec<String> {
            shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect()
        };
        let before = texts(&shapes);
        assert!(!before.iter().any(|t| t.contains('%')), "{before:?}");
        let mut hovered = Vec::new();
        for (i, t) in [0.0, 0.2, 1.0, 1.2].into_iter().enumerate() {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                time: Some(t),
                events: if i == 0 {
                    vec![Event::PointerMoved(over)]
                } else {
                    Vec::new()
                },
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| app.render_header(ctx));
            let mut shapes = Vec::new();
            for clipped in &output.shapes {
                flatten(&clipped.shape, &mut shapes);
            }
            hovered = texts(&shapes);
        }
        let new: Vec<&String> = hovered.iter().filter(|t| !before.contains(t)).collect();
        assert_eq!(new, ["Square Omni 74%"]);
    }

    #[test]
    fn the_tooltip_names_the_device_and_the_charge() {
        let b = Battery {
            actor: "square.0".into(),
            name: "Square Omni".into(),
            pct: 74,
        };
        assert_eq!(b.tooltip(), "Square Omni 74%");
    }
}
