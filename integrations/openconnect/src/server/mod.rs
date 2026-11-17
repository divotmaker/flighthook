//! The Open Connect V1 server: the launch monitor side of GSPro Open
//! Connect. Launch monitor software that sends Open Connect (a TCP stream of
//! JSON objects) connects to it, and every shot it sends becomes a launch
//! monitor's shot messages. It is the inverse of the [`client`](crate::client).
//!
//! ```text
//!   launch monitor software        --TCP 921-->  Server
//!                                                  |
//!                                                  v  shot messages
//!                                             a host's bus
//! ```
//!
//! The driver ([`Server`], feature `driver`) is a launch monitor whose
//! device is the software that connects. It keeps its listener across
//! sessions. A connect binds (once), accepts, and waits for the peer's first
//! message, whose `DeviceID` names the session's device (the identity's
//! model); the peer's address is the identity's `device` and shows in its
//! label. Open Connect is a single-peer protocol: one monitor at a time, and
//! a second monitor is a second server on its own port. While it waits the
//! driver's phase is `listening`, and `handshaking` from the accept of a peer
//! to its first message. When the monitor disconnects the session ends at
//! once, with no alert, and the next connect listens again on the same
//! socket.
//!
//! Each message the session reads:
//!
//! - sets the readiness (`ready`, no blockers) and `ball_detected`
//!   (telemetry) the monitor says, shot or heartbeat;
//! - is answered with a 200;
//! - is a shot when it is no heartbeat and says it carries ball data. A shot
//!   is `shot.trigger`, `shot.launch` (the spin pair the monitor left out
//!   completed from the other), the monitor's carry as `shot.flight`
//!   (`modeled`: whether it was tracked is not known), and, when the message
//!   says it carries club data, `shot.club` and `shot.impact`, each value as
//!   the monitor sent it; then the request and the reply as text raw
//!   frames, and `shot.finished`. A message without ball data holds zeros,
//!   not readings, and is no shot.
//!
//! **Player information.** Once the first message has been answered, and
//! again whenever the turn's hand or club ([`FrpSession::hints`]) differs
//! from what the monitor was last told, the monitor is sent a 201 with the
//! hand and club (as GSPro does). Nothing is sent while the club is not
//! known. The 201 waits until the last 200 is 50 ms old, so
//! the two objects, which carry no terminator, never arrive in one read.
//!
//! **This process's own clients.** A connection from an Open Connect client
//! of this process would loop every shot client ->
//! server -> bus -> client. The server closes such a peer at once, unread,
//! tells the client why, and raises one Error alert per burst of that
//! client's reconnect attempts, naming the client by its actor id. A client
//! registers its connection a moment after connecting, so the peer is looked
//! up again after each read, before the read is handled. A connection from
//! anything else, on the same host or not, is served as usual.
//!
//! **Retry.** A bind that fails (the port is taken) raises an Error alert
//! and retries after 1 s, growing to 15 s. A session that ends is followed
//! by the next connect at once.
//!
//! **Raw frames:** every object read (`request`, `heartbeat`) and every
//! reply written (`reply`) goes to the host as a text `shot.raw`.
//!
//! [`FrpSession::hints`]: flightrelay::driver::FrpSession::hints

#[cfg(feature = "driver")]
pub mod mapper;
#[cfg(feature = "driver")]
mod session;

#[cfg(feature = "driver")]
pub use session::{Error, Server, Session};

use std::net::SocketAddr;

use flightrelay::config::{ConfigError, DriverConfig};
use serde::{Deserialize, Serialize};

/// The address the server binds when none is set.
pub const DEFAULT_BIND: &str = "0.0.0.0:921";

/// The driver's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The address to listen on, `ip:port`. Defaults to [`DEFAULT_BIND`],
    /// and is left out while unset. A change restarts the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
}

impl Config {
    /// The address to listen on.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] for a `bind` that is not `ip:port`.
    pub fn addr(&self) -> Result<SocketAddr, ConfigError> {
        self.bind
            .as_deref()
            .unwrap_or(DEFAULT_BIND)
            .parse()
            .map_err(|_| ConfigError::field("bind", "must be ip:port, e.g. 0.0.0.0:921"))
    }
}

impl DriverConfig for Config {
    fn validate(&self) -> Result<(), ConfigError> {
        self.addr().map(|_| ())
    }

    fn example() -> Self {
        Self {
            bind: Some(String::new()),
        }
    }
}
