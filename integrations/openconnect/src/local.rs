//! This process's live Open Connect client connections, by local socket
//! address.
//!
//! An Open Connect client pointed at this process's own Open Connect server
//! would loop every shot client -> server -> bus -> client, forever. Each
//! client registers the local address of its connection here once connected,
//! and each server looks the address of a peer it accepts up here
//! ([`client_at`]): a match is one of our own clients, which the server
//! refuses ([`refuse`]). "This process's own" is process-wide by nature, so
//! the registry is one static.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Mutex, MutexGuard};

/// The process's registry.
static CLIENTS: LocalClients = LocalClients::new();

/// The process's registry.
pub(crate) fn clients() -> &'static LocalClients {
    &CLIENTS
}

/// The actor ID of this process's own Open Connect client whose connection
/// comes from `peer`, if it is one.
#[must_use]
pub(crate) fn client_at(peer: SocketAddr) -> Option<String> {
    CLIENTS.client_at(peer)
}

/// Tell this process's own client connected from `peer` that its
/// connection is refused, and why.
pub(crate) fn refuse(peer: SocketAddr, reason: &str) {
    CLIENTS.refuse(peer, reason);
}

/// An address as both ends of one connection see it: IPv4-mapped IPv6
/// addresses as IPv4, every loopback address as `127.0.0.1`.
fn key(addr: SocketAddr) -> (IpAddr, u16) {
    let ip = addr.ip().to_canonical();
    let ip = if ip.is_loopback() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        ip
    };
    (ip, addr.port())
}

struct Entry {
    /// The client's actor ID.
    actor: String,
    /// Why a server refused the connection, once one has.
    refusal: Option<String>,
}

/// The registry: one entry per connected Open Connect client.
pub(crate) struct LocalClients {
    entries: Mutex<BTreeMap<(IpAddr, u16), Entry>>,
}

impl LocalClients {
    pub(crate) const fn new() -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<(IpAddr, u16), Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record client `actor`'s connection from local address `local`. The
    /// entry lasts as long as the returned registration.
    #[must_use]
    pub(crate) fn register(&self, local: SocketAddr, actor: &str) -> Registration<'_> {
        let key = key(local);
        self.lock().insert(
            key,
            Entry {
                actor: actor.to_string(),
                refusal: None,
            },
        );
        Registration { clients: self, key }
    }

    /// The actor ID of our own client whose connection comes from `peer`,
    /// if it is one.
    #[must_use]
    pub(crate) fn client_at(&self, peer: SocketAddr) -> Option<String> {
        self.lock().get(&key(peer)).map(|e| e.actor.clone())
    }

    /// Tell the client connected from `peer` that its connection is refused,
    /// and why.
    pub(crate) fn refuse(&self, peer: SocketAddr, reason: &str) {
        if let Some(entry) = self.lock().get_mut(&key(peer)) {
            entry.refusal = Some(reason.to_string());
        }
    }
}

/// A client's entry in [`LocalClients`], removed on drop.
pub(crate) struct Registration<'a> {
    clients: &'a LocalClients,
    key: (IpAddr, u16),
}

impl Registration<'_> {
    /// Why a server refused this connection, if one has.
    #[must_use]
    pub(crate) fn refusal(&self) -> Option<String> {
        self.clients
            .lock()
            .get(&self.key)
            .and_then(|e| e.refusal.clone())
    }
}

impl std::fmt::Debug for Registration<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.clients.lock().remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().expect("socket address")
    }

    #[test]
    fn a_registered_client_is_found_by_its_local_address_until_dropped() {
        let clients = LocalClients::new();
        let reg = clients.register(addr("127.0.0.1:50123"), "openconnect_client.0");
        assert_eq!(
            clients.client_at(addr("127.0.0.1:50123")).as_deref(),
            Some("openconnect_client.0")
        );
        assert_eq!(clients.client_at(addr("127.0.0.1:50124")), None);
        assert_eq!(clients.client_at(addr("192.168.1.5:50123")), None);
        drop(reg);
        assert_eq!(clients.client_at(addr("127.0.0.1:50123")), None);
    }

    #[test]
    fn loopback_and_ipv4_mapped_forms_match() {
        let clients = LocalClients::new();
        let _v4 = clients.register(addr("127.0.0.1:50123"), "openconnect_client.0");
        let _lan = clients.register(addr("192.168.1.5:50200"), "openconnect_client.1");
        for peer in ["[::ffff:127.0.0.1]:50123", "[::1]:50123", "127.0.0.1:50123"] {
            assert_eq!(
                clients.client_at(addr(peer)).as_deref(),
                Some("openconnect_client.0"),
                "{peer}"
            );
        }
        assert_eq!(
            clients
                .client_at(addr("[::ffff:192.168.1.5]:50200"))
                .as_deref(),
            Some("openconnect_client.1")
        );
    }

    #[test]
    fn a_refusal_reaches_the_client_it_names() {
        let clients = LocalClients::new();
        let a = clients.register(addr("127.0.0.1:50123"), "openconnect_client.0");
        let b = clients.register(addr("127.0.0.1:50124"), "openconnect_client.1");
        clients.refuse(addr("[::ffff:127.0.0.1]:50123"), "loop");
        assert_eq!(a.refusal().as_deref(), Some("loop"));
        assert_eq!(b.refusal(), None);
        // A peer that is not ours: nothing to tell.
        clients.refuse(addr("127.0.0.1:9"), "loop");
        assert_eq!(b.refusal(), None);
    }
}
