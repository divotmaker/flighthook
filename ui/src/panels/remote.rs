//! Other FRP programs on the bus: a
//! read-only card for each remote actor, and the ownership conflicts every
//! card involved shows.
//!
//! A remote actor is one whose `bus.actor` the UI socket carried, that is
//! neither flighthook's own actor nor one of its cards (`card_keys`). It
//! appears with its first `bus.actor` and goes when the actor expires or
//! sends `removed`, since the UI's state map follows both.

use std::collections::{BTreeMap, HashMap};

use flighthook::{ActorStatus, ActorStatusResponse, BusActor, FrpBody, StateKey, roles};

use super::cards::CardGroup;

type States = BTreeMap<StateKey, flighthook::BusMessage>;

/// A remote actor's card: its group and the telemetry its page shows.
#[derive(Debug, Clone)]
pub(crate) struct RemoteCard {
    /// The actor id.
    pub id: String,
    pub group: CardGroup,
    /// Name, connected state and rows: device, `ready` (or the blockers),
    /// battery, attitude, the actor's telemetry and a conflict `error`.
    pub status: ActorStatusResponse,
}

impl RemoteCard {
    /// The card's key on the Devices tab (never a local card's key).
    pub(crate) fn key(&self) -> String {
        format!("remote:{}", self.id)
    }
}

/// Every `bus.actor` on the bus, by actor id.
fn actors(states: &States) -> impl Iterator<Item = (&str, &BusActor)> {
    states.values().filter_map(|m| match &m.body {
        FrpBody::BusActor(a) => Some((m.from.as_str(), a)),
        _ => None,
    })
}

/// The read-only cards of the remote actors, in actor-id order.
/// `conflicts` gives each actor's conflict row ([`conflicts`]).
pub(crate) fn remote_cards(
    states: &States,
    card_keys: &HashMap<String, String>,
    own_id: &str,
    conflicts: &HashMap<String, String>,
) -> Vec<RemoteCard> {
    actors(states)
        .filter(|(id, _)| *id != own_id && !card_keys.contains_key(*id))
        .map(|(id, actor)| {
            let group = if actor.has_role(roles::LAUNCH_MONITOR) {
                CardGroup::LaunchMonitors
            } else {
                CardGroup::Software
            };
            let mut status = ActorStatusResponse::new(actor.name.clone(), ActorStatus::Connected);
            status.telemetry = rows(actor);
            if let Some(error) = conflicts.get(id) {
                status.telemetry.insert("error".into(), error.clone());
            }
            RemoteCard {
                id: id.to_owned(),
                group,
                status,
            }
        })
        .collect()
}

/// A remote actor's rows: the device (maker, model, firmware), `ready` as
/// "true" with no blockers or the blockers as the value, battery, attitude
/// and its telemetry.
fn rows(actor: &BusActor) -> HashMap<String, String> {
    let mut rows: HashMap<String, String> = flighthook::bus_actor_rows(actor)
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect();
    if !actor.blockers.is_empty() {
        rows.insert("ready".into(), actor.blockers.join(", "));
    }
    if let Some(device) = &actor.device {
        let info: Vec<&str> = [&device.manufacturer, &device.model, &device.firmware]
            .into_iter()
            .filter_map(|v| v.as_deref())
            .filter(|v| !v.is_empty())
            .collect();
        if !info.is_empty() {
            rows.insert("device_info".into(), info.join(" "));
        }
    }
    rows
}

/// What a claim of `role` reads as in a conflict row, after "runs".
fn claim(role: &str) -> Option<&'static str> {
    match role {
        roles::SESSION => Some("golfer turns"),
        roles::PRACTICE => Some("practice blocks"),
        roles::GAME => Some("a game"),
        _ => None,
    }
}

/// The ownership conflicts on the bus, by actor id: for every actor that
/// claims `session`, `practice` or `game`, or issues a golfer id namespace,
/// that another actor also claims, one line naming each other actor and
/// what they share ("Another program also runs golfer turns and practice
/// blocks: X"). Two flight models are no conflict.
pub(crate) fn conflicts(states: &States) -> HashMap<String, String> {
    let all: Vec<(&str, &BusActor)> = actors(states).collect();
    let mut out = HashMap::new();
    for (id, actor) in &all {
        let mut lines = Vec::new();
        for (other_id, other) in &all {
            if other_id == id {
                continue;
            }
            let runs: Vec<&str> = actor
                .roles
                .iter()
                .filter(|r| other.has_role(r))
                .filter_map(|r| claim(r))
                .collect();
            let mut shared = Vec::new();
            if !runs.is_empty() {
                shared.push(format!("runs {}", runs.join(" and ")));
            }
            shared.extend(
                actor
                    .namespaces
                    .iter()
                    .filter(|n| other.namespaces.contains(n))
                    .map(|n| format!("issues \"{n}\" golfer ids")),
            );
            if !shared.is_empty() {
                lines.push(format!(
                    "Another program also {}: {}",
                    shared.join(" and "),
                    other.name
                ));
            }
        }
        if !lines.is_empty() {
            out.insert((*id).to_owned(), lines.join("; "));
        }
    }
    out
}

impl crate::app::FlighthookApp {
    /// The remote actors' cards, with their conflict rows.
    pub(crate) fn remote_cards(&self) -> Vec<RemoteCard> {
        remote_cards(
            &self.bus_states,
            &self.card_keys,
            &self.flighthook_id,
            &conflicts(&self.bus_states),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flighthook::{BusMessage, DeviceInfo};

    fn put(states: &mut States, from: &str, actor: BusActor) {
        let msg = BusMessage::new(from, actor);
        states.insert(msg.state_key().expect("state"), msg);
    }

    fn actor(name: &str, roles: &[&str], namespaces: &[&str]) -> BusActor {
        let mut a = BusActor::new(name, 2.0);
        a.roles = roles.iter().map(|r| (*r).to_owned()).collect();
        a.namespaces = namespaces.iter().map(|n| (*n).to_owned()).collect();
        a
    }

    #[test]
    fn a_remote_actor_gets_a_card_in_its_roles_group_and_local_ones_none() {
        let mut states = States::new();
        let mut lm = actor("Trackball", &[roles::LAUNCH_MONITOR], &[]);
        lm.blockers = vec!["not_armed".into()];
        lm.device = Some(DeviceInfo {
            manufacturer: Some("divotmaker".into()),
            model: Some("thumbswing".into()),
            battery_pct: Some(80),
            ..DeviceInfo::default()
        });
        put(&mut states, "remote-lm", lm);
        put(
            &mut states,
            "remote-tool",
            actor("Lights", &["lighting"], &[]),
        );
        put(
            &mut states,
            "me",
            actor("flighthook", &[roles::SESSION], &[]),
        );
        put(
            &mut states,
            "lm1",
            actor("Mevo+", &[roles::LAUNCH_MONITOR], &[]),
        );
        let keys = HashMap::from([("lm1".to_string(), "mevo.0".to_string())]);
        let cards = remote_cards(&states, &keys, "me", &HashMap::new());
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].id, "remote-lm");
        assert_eq!(cards[0].group, CardGroup::LaunchMonitors);
        assert_eq!(cards[0].key(), "remote:remote-lm");
        let rows = &cards[0].status.telemetry;
        assert_eq!(rows["ready"], "not_armed");
        assert_eq!(rows["device_info"], "divotmaker thumbswing");
        assert_eq!(rows["battery_pct"], "80");
        assert_eq!(cards[0].status.name, "Trackball");
        assert_eq!(cards[1].group, CardGroup::Software);
        assert_eq!(cards[1].status.telemetry["ready"], "true");
    }

    #[test]
    fn two_sessions_or_namespace_issuers_conflict_and_two_flight_models_do_not() {
        let mut states = States::new();
        put(
            &mut states,
            "me",
            actor(
                "flighthook",
                &[roles::SESSION, roles::PRACTICE],
                &["flighthook"],
            ),
        );
        put(
            &mut states,
            "other",
            actor(
                "Other app",
                &[roles::SESSION, roles::PRACTICE],
                &["flighthook"],
            ),
        );
        put(
            &mut states,
            "m1",
            actor("Model A", &[roles::FLIGHT_MODEL], &[]),
        );
        put(
            &mut states,
            "m2",
            actor("Model B", &[roles::FLIGHT_MODEL], &[]),
        );
        let found = conflicts(&states);
        assert_eq!(
            found["me"],
            "Another program also runs golfer turns and practice blocks and issues \"flighthook\" \
             golfer ids: Other app"
        );
        assert_eq!(
            found["other"],
            "Another program also runs golfer turns and practice blocks and issues \"flighthook\" \
             golfer ids: flighthook"
        );
        assert!(!found.contains_key("m1") && !found.contains_key("m2"));
        let cards = remote_cards(&states, &HashMap::new(), "me", &found);
        let other = cards.iter().find(|c| c.id == "other").expect("card");
        assert!(other.status.telemetry["error"].contains("flighthook"));
    }

    #[test]
    fn a_remote_launch_monitors_battery_shows_in_the_header() {
        let mut app = crate::app::FlighthookApp::with_pending(crate::net::new_pending());
        let mut lm = actor("Far radar", &[roles::LAUNCH_MONITOR], &[]);
        lm.device = Some(DeviceInfo {
            battery_pct: Some(41),
            ..DeviceInfo::default()
        });
        put(&mut app.bus_states, "far", lm);
        let shown = crate::battery::remote_batteries(&app.remote_cards());
        assert_eq!(shown.len(), 1);
        assert_eq!((shown[0].name.as_str(), shown[0].pct), ("Far radar", 41));
    }
}
