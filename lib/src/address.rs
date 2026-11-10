//! One card per address may be enabled.
//!
//! Two cards of the same type conflict when their addresses collide: the same
//! host and port, or, for a card that listens, the same port on overlapping
//! bind addresses (`0.0.0.0` overlaps every address). A card that is off
//! holds nothing. Creating a duplicate is allowed (to set it up for another
//! address), but a conflicting card cannot be on.

use crate::config::FlighthookConfig;

/// Address `address` in a form that compares equal for the same host and
/// port: trimmed, `localhost` as `127.0.0.1`, an IPv4-mapped IPv6 address as
/// IPv4, anything that is no socket address lowercased.
pub(crate) fn address_key(address: &str) -> String {
    let address = address.trim();
    let address = match address.rsplit_once(':') {
        Some((host, port)) if host.eq_ignore_ascii_case("localhost") => {
            format!("127.0.0.1:{port}")
        }
        _ => address.to_owned(),
    };
    match address.parse::<std::net::SocketAddr>() {
        Ok(std::net::SocketAddr::V6(v6)) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => std::net::SocketAddr::from((v4, v6.port())).to_string(),
            None => std::net::SocketAddr::V6(v6).to_string(),
        },
        Ok(addr) => addr.to_string(),
        Err(_) => address.to_ascii_lowercase(),
    }
}

/// The address a card of `type_key` uses when its address is blank. Blank for
/// a type that has no address to compare.
pub(crate) fn default_address(type_key: &str) -> &'static str {
    match type_key {
        "mevo" => "192.168.2.1:5100",
        "openconnect_client" => "127.0.0.1:921",
        "openconnect_server" => "0.0.0.0:921",
        "pureplay" => "127.0.0.1:9760",
        _ => "",
    }
}

/// The name of card type `type_key` in a message to the user.
pub(crate) fn type_name(type_key: &str) -> &str {
    match type_key {
        "mevo" => "Mevo",
        "r10" => "R10",
        "square" => "Square Golf Omni",
        "exputt" => "ExPutt",
        "trackball" => "Trackball",
        "openconnect_server" => "Open Connect Server",
        "openconnect_client" => "Open Connect",
        "pureplay" => "Pureplay Connect",
        "mock_monitor" => "Mock",
        "camera" => "Camera",
        "random_club" => "Random Club",
        other => other,
    }
}

/// Whether addresses `a` and `b` of a card of `type_key` collide. A card that
/// listens (an Open Connect server) collides on the same port with
/// overlapping bind addresses; any other card on the same host and port.
pub(crate) fn addresses_collide(type_key: &str, a: &str, b: &str) -> bool {
    let (a, b) = (address_key(a), address_key(b));
    if type_key == "openconnect_server"
        && let (Ok(x), Ok(y)) = (
            a.parse::<std::net::SocketAddr>(),
            b.parse::<std::net::SocketAddr>(),
        )
    {
        return x.port() == y.port()
            && (x.ip() == y.ip() || x.ip().is_unspecified() || y.ip().is_unspecified());
    }
    a == b
}

/// A card that cannot be on because an enabled card of its type has its
/// address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddressClash {
    /// The card that conflicts, e.g. `openconnect_client.1`.
    pub id: String,
    /// The enabled card that holds the address.
    pub holder: String,
    /// The card type, e.g. `openconnect_client`.
    pub type_key: String,
    /// The address the holder uses.
    pub address: String,
}

impl AddressClash {
    /// The message shown to the user, e.g. "An Open Connect card already
    /// uses 127.0.0.1:921".
    #[must_use]
    pub fn message(&self) -> String {
        let name = type_name(&self.type_key);
        let article = if name.starts_with(['A', 'E', 'I', 'O', 'U']) {
            "An"
        } else {
            "A"
        };
        format!("{article} {name} card already uses {}", self.address)
    }
}

/// One card that has an address to compare.
struct Card {
    id: String,
    type_key: &'static str,
    enabled: bool,
    /// The address in use: the card's own, else its type's default.
    address: String,
}

/// Index `a` before `b`: numeric indices in number order, others by text
/// (the order cards are listed and checked in).
#[must_use]
pub fn index_order(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.cmp(b),
    }
}

impl FlighthookConfig {
    /// Every card with an address to compare, by type and then index.
    fn address_cards(&self) -> Vec<Card> {
        fn push<T>(
            out: &mut Vec<Card>,
            type_key: &'static str,
            map: &std::collections::HashMap<String, T>,
            card: impl Fn(&T) -> (bool, Option<&str>),
        ) {
            let mut indices: Vec<_> = map.keys().collect();
            indices.sort_by(|a, b| index_order(a, b));
            for index in indices {
                let (enabled, address) = card(&map[index]);
                let address = address
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| default_address(type_key));
                if !address.is_empty() {
                    out.push(Card {
                        id: format!("{type_key}.{index}"),
                        type_key,
                        enabled,
                        address: address.to_owned(),
                    });
                }
            }
        }
        let mut out = Vec::new();
        push(&mut out, "mevo", &self.mevo, |s| {
            (s.enabled, s.driver.address.as_deref())
        });
        push(&mut out, "square", &self.square, |s| {
            (s.enabled, s.driver.address.as_deref())
        });
        push(&mut out, "exputt", &self.exputt, |s| {
            (s.enabled, s.driver.address.as_deref())
        });
        push(&mut out, "trackball", &self.trackball, |s| {
            (s.enabled, s.driver.device())
        });
        push(
            &mut out,
            "openconnect_server",
            &self.openconnect_server,
            |s| (s.enabled, s.bind.as_deref()),
        );
        push(
            &mut out,
            "openconnect_client",
            &self.openconnect_client,
            |s| (s.enabled, s.address.as_deref()),
        );
        push(&mut out, "pureplay", &self.pureplay, |s| {
            (s.enabled, s.address.as_deref())
        });
        out
    }

    /// The first enabled card of the same type, other than card `id`, that
    /// has card `id`'s address, or `None` (also for a card without an
    /// address to compare). For a card that is off it is why the card cannot
    /// be turned on.
    #[must_use]
    pub fn address_clash(&self, id: &str) -> Option<AddressClash> {
        let cards = self.address_cards();
        let me = cards.iter().find(|c| c.id == id)?;
        cards
            .iter()
            .find(|c| {
                c.id != id
                    && c.enabled
                    && c.type_key == me.type_key
                    && addresses_collide(me.type_key, &me.address, &c.address)
            })
            .map(|holder| AddressClash {
                id: me.id.clone(),
                holder: holder.id.clone(),
                type_key: me.type_key.to_owned(),
                address: holder.address.clone(),
            })
    }

    /// The cards that `self`, as a save over `live`, turns on although an
    /// enabled card of their type has their address: a card that is off (or
    /// absent) in `live` and on here. A save that does this is refused.
    #[must_use]
    pub fn enable_clashes(&self, live: &FlighthookConfig) -> Vec<AddressClash> {
        let before = live.address_cards();
        self.address_cards()
            .iter()
            .filter(|c| c.enabled)
            .filter(|c| !before.iter().any(|b| b.id == c.id && b.enabled))
            .filter_map(|c| self.address_clash(&c.id))
            .collect()
    }

    /// Turn off every enabled card whose address another enabled card of its
    /// type holds, and return those cards. Of two cards on one address the
    /// one first in index order keeps running; with `live`, the config this
    /// one replaces, a card whose address or switch changed comes after
    /// the ones that did not, so editing a card onto a held address turns
    /// that card off, not the holder.
    pub fn demote_clashing(&mut self, live: Option<&FlighthookConfig>) -> Vec<AddressClash> {
        let cards = self.address_cards();
        let before = live.map(FlighthookConfig::address_cards);
        let unchanged = |c: &Card| {
            before.as_ref().is_none_or(|before| {
                before
                    .iter()
                    .any(|b| b.id == c.id && b.enabled == c.enabled && b.address == c.address)
            })
        };
        let (first, last): (Vec<&Card>, Vec<&Card>) = cards.iter().partition(|c| unchanged(c));
        let mut held: Vec<&Card> = Vec::new();
        let mut clashes = Vec::new();
        for card in first.into_iter().chain(last).filter(|c| c.enabled) {
            match held.iter().find(|h| {
                h.type_key == card.type_key
                    && addresses_collide(card.type_key, &card.address, &h.address)
            }) {
                Some(holder) => clashes.push(AddressClash {
                    id: card.id.clone(),
                    holder: holder.id.clone(),
                    type_key: card.type_key.to_owned(),
                    address: holder.address.clone(),
                }),
                None => held.push(card),
            }
        }
        for clash in &clashes {
            self.set_actor_enabled(&clash.id, false);
        }
        clashes
    }

    /// Turn the section of actor `actor_id` (e.g. `mevo.0`) on or off.
    /// Returns false when there is no such section or its type has no
    /// switch (the web server).
    pub fn set_actor_enabled(&mut self, actor_id: &str, on: bool) -> bool {
        fn set<T>(
            map: &mut std::collections::HashMap<String, T>,
            index: &str,
            on: bool,
            field: fn(&mut T) -> &mut bool,
        ) -> bool {
            map.get_mut(index).map(|s| *field(s) = on).is_some()
        }
        let Some((type_key, i)) = actor_id.split_once('.') else {
            return false;
        };
        match type_key {
            "mevo" => set(&mut self.mevo, i, on, |s| &mut s.enabled),
            "r10" => set(&mut self.r10, i, on, |s| &mut s.enabled),
            "square" => set(&mut self.square, i, on, |s| &mut s.enabled),
            "exputt" => set(&mut self.exputt, i, on, |s| &mut s.enabled),
            "trackball" => set(&mut self.trackball, i, on, |s| &mut s.enabled),
            "mock_monitor" => set(&mut self.mock_monitor, i, on, |s| &mut s.enabled),
            "openconnect_server" => set(&mut self.openconnect_server, i, on, |s| &mut s.enabled),
            "openconnect_client" => set(&mut self.openconnect_client, i, on, |s| &mut s.enabled),
            "pureplay" => set(&mut self.pureplay, i, on, |s| &mut s.enabled),
            "random_club" => set(&mut self.random_club, i, on, |s| &mut s.enabled),
            "camera" => set(&mut self.camera, i, on, |s| &mut s.enabled),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MevoConfig;
    use crate::config::{
        FlighthookConfig, MevoSection, OpenConnectClientSection, OpenConnectServerSection,
    };

    fn client(address: Option<&str>, enabled: bool) -> OpenConnectClientSection {
        OpenConnectClientSection {
            name: "Open Connect".into(),
            enabled,
            address: address.map(str::to_owned),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
        }
    }

    fn server(bind: &str, enabled: bool) -> OpenConnectServerSection {
        OpenConnectServerSection {
            name: "Server".into(),
            enabled,
            bind: Some(bind.into()),
            impact_lead_s: None,
        }
    }

    fn config(clients: &[(&str, Option<&str>, bool)]) -> FlighthookConfig {
        let mut c = FlighthookConfig::default();
        for (i, a, on) in clients {
            c.openconnect_client.insert((*i).into(), client(*a, *on));
        }
        c
    }

    #[test]
    fn addresses_compare_by_host_and_port() {
        assert_eq!(address_key(" 127.0.0.1:921 "), "127.0.0.1:921");
        assert_eq!(address_key("localhost:921"), "127.0.0.1:921");
        assert_eq!(address_key("LocalHost:921"), "127.0.0.1:921");
        assert_eq!(address_key("[::ffff:127.0.0.1]:921"), "127.0.0.1:921");
        assert_eq!(address_key("Sim-PC:921"), "sim-pc:921");
        assert_ne!(address_key("127.0.0.1:921"), address_key("127.0.0.1:9210"));
    }

    #[test]
    fn a_blank_address_is_the_types_default() {
        let c = config(&[("0", None, true), ("1", Some("localhost:921"), false)]);
        let clash = c.address_clash("openconnect_client.1").expect("clash");
        assert_eq!(clash.holder, "openconnect_client.0");
        assert_eq!(clash.address, "127.0.0.1:921");
        assert_eq!(
            clash.message(),
            "An Open Connect card already uses 127.0.0.1:921"
        );
        assert_eq!(
            config(&[("0", Some(" "), true), ("1", Some(""), false)])
                .address_clash("openconnect_client.1")
                .map(|c| c.holder),
            Some("openconnect_client.0".into())
        );
    }

    #[test]
    fn a_card_that_is_off_holds_nothing() {
        let c = config(&[("0", None, false), ("1", None, false)]);
        assert_eq!(c.address_clash("openconnect_client.1"), None);
        assert_eq!(c.address_clash("openconnect_client.0"), None);
    }

    #[test]
    fn other_ports_hosts_and_types_do_not_clash() {
        let mut c = config(&[
            ("0", Some("127.0.0.1:921"), true),
            ("1", Some("127.0.0.1:9210"), false),
            ("2", Some("192.168.1.20:921"), false),
        ]);
        assert_eq!(c.address_clash("openconnect_client.1"), None);
        assert_eq!(c.address_clash("openconnect_client.2"), None);
        c.mevo.insert(
            "0".into(),
            MevoSection {
                driver: MevoConfig {
                    address: Some("127.0.0.1:921".into()),
                    ..MevoConfig::default()
                },
                ..MevoSection::default()
            },
        );
        assert_eq!(c.address_clash("mevo.0"), None, "another type");
        assert_eq!(c.address_clash("r10.0"), None, "no such card");
    }

    #[test]
    fn a_listening_card_clashes_on_an_overlapping_bind_address() {
        let mut c = FlighthookConfig::default();
        for (i, bind, on) in [
            ("0", "0.0.0.0:921", true),
            ("1", "127.0.0.1:921", false),
            ("2", "0.0.0.0:9921", false),
        ] {
            c.openconnect_server.insert(i.into(), server(bind, on));
        }
        let clash = c.address_clash("openconnect_server.1").expect("clash");
        assert_eq!(
            clash.message(),
            "An Open Connect Server card already uses 0.0.0.0:921"
        );
        assert_eq!(
            c.address_clash("openconnect_server.2"),
            None,
            "another port"
        );
        c.openconnect_server.get_mut("0").unwrap().bind = Some("127.0.0.2:921".into());
        assert_eq!(
            c.address_clash("openconnect_server.1"),
            None,
            "disjoint hosts"
        );
        c.openconnect_server.get_mut("0").unwrap().bind = Some("127.0.0.1:921".into());
        assert!(
            c.address_clash("openconnect_server.1").is_some(),
            "same host"
        );
    }

    #[test]
    fn a_mevo_clashes_on_its_default_address() {
        let mut c = FlighthookConfig::default();
        c.mevo.insert("0".into(), MevoSection::default());
        c.mevo.insert(
            "1".into(),
            MevoSection {
                enabled: false,
                ..MevoSection::default()
            },
        );
        let clash = c.address_clash("mevo.1").expect("clash");
        assert_eq!(clash.message(), "A Mevo card already uses 192.168.2.1:5100");
    }

    #[test]
    fn a_save_that_turns_a_clashing_card_on_is_found() {
        let live = config(&[("0", None, true), ("1", None, false)]);
        let mut save = live.clone();
        assert!(save.enable_clashes(&live).is_empty(), "nothing turned on");
        save.set_actor_enabled("openconnect_client.1", true);
        let found = save.enable_clashes(&live);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "openconnect_client.1");
        // A new card on a held address is turned on too.
        let mut added = live.clone();
        added
            .openconnect_client
            .insert("2".into(), client(None, true));
        assert_eq!(added.enable_clashes(&live).len(), 1);
        // The same card on a free address is no clash.
        added.openconnect_client.get_mut("2").unwrap().address = Some("127.0.0.1:9210".into());
        assert!(added.enable_clashes(&live).is_empty());
    }

    #[test]
    fn editing_a_card_onto_a_held_address_turns_that_card_off() {
        let live = config(&[
            ("0", Some("127.0.0.1:921"), true),
            ("1", Some("127.0.0.1:9210"), true),
        ]);
        // The first card moves onto the second's address: it loses, though
        // it is first in order.
        let mut save = live.clone();
        save.openconnect_client.get_mut("0").unwrap().address = Some("127.0.0.1:9210".into());
        let clashes = save.demote_clashing(Some(&live));
        assert_eq!(clashes.len(), 1);
        assert_eq!(clashes[0].id, "openconnect_client.0");
        assert_eq!(clashes[0].holder, "openconnect_client.1");
        assert!(!save.openconnect_client["0"].enabled);
        assert!(save.openconnect_client["1"].enabled);
        // No clash left: a second pass changes nothing.
        assert!(save.demote_clashing(Some(&live)).is_empty());
    }

    #[test]
    fn a_config_with_two_enabled_cards_on_one_address_keeps_the_first() {
        let mut c = config(&[
            ("10", None, true),
            ("2", None, true),
            ("3", Some("localhost:921"), true),
            ("4", Some("127.0.0.1:9210"), true),
        ]);
        let clashes = c.demote_clashing(None);
        let ids: Vec<_> = clashes.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["openconnect_client.3", "openconnect_client.10"]);
        assert!(c.openconnect_client["2"].enabled, "index 2 sorts before 10");
        assert!(!c.openconnect_client["3"].enabled);
        assert!(!c.openconnect_client["10"].enabled);
        assert!(c.openconnect_client["4"].enabled, "another port");
    }

    #[test]
    fn set_actor_enabled_reaches_every_switchable_section() {
        let mut c = config(&[("0", None, true)]);
        assert!(c.set_actor_enabled("openconnect_client.0", false));
        assert_eq!(c.disabled_actors(), ["openconnect_client.0"]);
        assert!(
            !c.set_actor_enabled("openconnect_client.9", false),
            "absent"
        );
        assert!(!c.set_actor_enabled("webserver.0", false), "no switch");
        assert!(!c.set_actor_enabled("nonsense", false));
    }
}
