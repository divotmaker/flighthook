//! Test support for a host (feature `testing`).

use std::net::SocketAddr;

/// The actor id of this process's own Open Connect client whose connection
/// comes from local address `peer`, if it is one: what a server looks up to
/// refuse it.
#[must_use]
pub fn client_at(peer: SocketAddr) -> Option<String> {
    crate::local::client_at(peer)
}
