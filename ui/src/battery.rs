//! Battery indicators for the header: one small battery per connected device
//! that reports a charge, filled to the charge and named on hover.

use std::collections::HashMap;

use crate::panels::devices::device_card_order;
use crate::panels::settings::ActorFormEntry;
use crate::theme;
use crate::types::{ActorStatusResponse, ConnectionState};

/// Telemetry key a device reports its charge under, as a whole percent.
const KEY: &str = "battery_pct";

/// Charges below this many percent are drawn yellow.
const LOW_PCT: u8 = 20;
/// Charges below this many percent are drawn red.
const CRITICAL_PCT: u8 = 10;

/// Width and height of the battery body.
const BODY: egui::Vec2 = egui::vec2(22.0, 11.0);
/// Width and height of the terminal nub, and the gap between it and the body.
const NUB: egui::Vec2 = egui::vec2(2.0, 5.0);
const NUB_GAP: f32 = 0.5;
/// Space between the outline and the fill.
const INSET: f32 = 2.0;

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

/// One battery per remote launch monitor whose `bus.actor` reports a valid
/// `device.battery_pct`, after the local cards' (as on the Devices tab).
pub(crate) fn remote_batteries(remote: &[crate::panels::remote::RemoteCard]) -> Vec<Battery> {
    remote
        .iter()
        .filter(|c| c.group == crate::panels::cards::CardGroup::LaunchMonitors)
        .filter_map(|c| {
            Some(Battery {
                actor: c.key(),
                name: c.status.name.clone(),
                pct: parse_pct(c.status.telemetry.get(KEY)?)?,
            })
        })
        .collect()
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
            // The title of the card's page.
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

/// Red below [`CRITICAL_PCT`] percent, yellow below [`LOW_PCT`], else green.
pub(crate) fn fill_color(pct: u8) -> egui::Color32 {
    if pct < CRITICAL_PCT {
        theme::STATUS_DOWN
    } else if pct < LOW_PCT {
        theme::STATUS_PENDING
    } else {
        theme::STATUS_OK
    }
}

/// The color of the header's energy icon: the lowest charge decides, red
/// below 10 %, yellow below 20 %, else the normal icon color.
pub(crate) fn energy_color(batteries: &[Battery]) -> egui::Color32 {
    match batteries.iter().map(|b| b.pct).min() {
        Some(pct) if pct < CRITICAL_PCT => theme::DANGER,
        Some(pct) if pct < LOW_PCT => theme::GOLD,
        _ => theme::TEXT_MUTED,
    }
}

/// The hover text of the energy icon: the device with the lowest charge
/// (the first in card order on a tie), `Square Omni 18%`.
pub(crate) fn lowest_tooltip(batteries: &[Battery]) -> Option<String> {
    batteries.iter().min_by_key(|b| b.pct).map(Battery::tooltip)
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

/// Draw `battery`.
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
    response
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

    #[test]
    fn the_tooltip_names_the_device_and_the_charge() {
        let b = Battery {
            actor: "square.0".into(),
            name: "Square Omni".into(),
            pct: 74,
        };
        assert_eq!(b.tooltip(), "Square Omni 74%");
    }

    /// Green from 20 %, yellow from 10 % to 19 %, red below 10 %.
    #[test]
    fn the_fill_is_green_yellow_or_red_by_the_thresholds() {
        for (pct, color) in [
            (100, theme::STATUS_OK),
            (25, theme::STATUS_OK),
            (20, theme::STATUS_OK),
            (19, theme::STATUS_PENDING),
            (15, theme::STATUS_PENDING),
            (10, theme::STATUS_PENDING),
            (9, theme::STATUS_DOWN),
            (5, theme::STATUS_DOWN),
            (0, theme::STATUS_DOWN),
        ] {
            assert_eq!(fill_color(pct), color, "{pct}%");
        }
    }

    #[test]
    fn the_lowest_charge_decides_the_energy_icon() {
        let at = |pcts: &[u8]| -> Vec<Battery> {
            pcts.iter()
                .enumerate()
                .map(|(i, pct)| Battery {
                    actor: format!("d.{i}"),
                    name: format!("Device {i}"),
                    pct: *pct,
                })
                .collect()
        };
        assert_eq!(energy_color(&at(&[74, 25])), theme::TEXT_MUTED);
        assert_eq!(energy_color(&at(&[74, 20])), theme::TEXT_MUTED);
        assert_eq!(energy_color(&at(&[74, 19])), theme::GOLD);
        assert_eq!(energy_color(&at(&[74, 15])), theme::GOLD);
        assert_eq!(energy_color(&at(&[74, 10])), theme::GOLD);
        assert_eq!(energy_color(&at(&[74, 9])), theme::DANGER);
        assert_eq!(energy_color(&at(&[74, 15, 5])), theme::DANGER);
        assert_eq!(
            lowest_tooltip(&at(&[74, 18, 40])).as_deref(),
            Some("Device 1 18%")
        );
        assert_eq!(lowest_tooltip(&[]), None);
    }
}
