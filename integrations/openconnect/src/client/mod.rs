//! The Open Connect V1 client: flighthook as a launch monitor for a
//! simulator that accepts GSPro Open Connect (GSPro and Pureplay on port
//! 921, Muni Golf on 9210, or any other; TCP, a stream of JSON objects).
//!
//! The driver ([`Client`], feature `driver`) connects to the simulator, and
//! each [`Session`] runs one connection. The connection is registered as
//! this process's own before anything is sent; the connect then waits
//! 1 s, and a refusal by this process's own Open Connect server (the client
//! points at it, which would loop shots back) fails the connect with the
//! server's reason. The session then:
//!
//! - sends each finished shot as one shot message. Its `ShotNumber` is the
//!   shot's own number, or one more than the last one sent when that is
//!   higher, so the numbers a simulator sees always increase, across
//!   sources that count separately and across reconnects (the driver keeps
//!   the last one). A shot with no launch, flight, club or impact (a missed
//!   read) is not sent and takes no number;
//! - sends a heartbeat with the readiness about a second after it starts,
//!   then every 10 s, and at once whenever the readiness changes. The
//!   readiness is whether any launch monitor in view has no blockers;
//! - reads the simulator's responses. A code outside 2xx, an unknown club
//!   code or text that is not a response raises a Warn alert. Player
//!   information (club and hand) becomes one `golfer.select` with the club
//!   and hand the session holds (a hand or club left out stays).
//!
//! **What the session is handed** ([`FrpSession::bus`]) is its view of the
//! bus, chosen by the host: the `bus.actor` of each launch monitor it stands
//! for (and that actor's removal when it leaves the view), and the messages
//! of the shots it may send. It composes each shot from those messages
//! itself.
//!
//! **Telemetry** (the actor's, for the connection): `monitor_ready` (the
//! readiness last sent), `response` (the simulator's last response code and
//! message), and `club` and `handed`, what the simulator set.
//!
//! **Raw frames:** every object written (`shot`, `heartbeat`) and every read
//! (`response`) goes to the host as a text `shot.raw`. The frame of a shot
//! carries the key the shot had on the bus, after it was written, so a host
//! can tell which of its shots the simulator was sent.
//!
//! [`FrpSession::bus`]: flightrelay::driver::FrpSession::bus

pub mod api;
#[cfg(feature = "driver")]
pub mod mapper;
#[cfg(feature = "driver")]
mod proto;
#[cfg(feature = "driver")]
mod session;
#[cfg(feature = "driver")]
mod shot;

#[cfg(feature = "driver")]
pub use session::{Client, Error, Session};
#[cfg(feature = "driver")]
pub use shot::{OutputShot, output_shot};

use std::net::SocketAddr;

use flightrelay::config::{ConfigError, DriverConfig};
use serde::{Deserialize, Serialize};

/// The simulator's default address.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:921";

/// The driver's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The simulator's address, `ip:port`. Defaults to [`DEFAULT_ADDRESS`],
    /// and is left out while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// The actor id the connection is registered under as this process's
    /// own; the host puts it in. Not a setting.
    #[serde(skip)]
    pub actor: String,
}

impl Config {
    /// The simulator's address.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] for an `address` that is not `ip:port`.
    pub fn addr(&self) -> Result<SocketAddr, ConfigError> {
        self.address
            .as_deref()
            .unwrap_or(DEFAULT_ADDRESS)
            .parse()
            .map_err(|_| ConfigError::field("address", "must be ip:port, e.g. 127.0.0.1:921"))
    }
}

impl DriverConfig for Config {
    fn validate(&self) -> Result<(), ConfigError> {
        self.addr().map(|_| ())
    }

    fn example() -> Self {
        Self {
            address: Some(String::new()),
            ..Self::default()
        }
    }
}
