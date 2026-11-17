//! Pureplay Connect v1 (alpha): flighthook as a launch monitor device for
//! Pureplay.
//!
//! Pureplay is the host and listens on TCP 9760; the driver ([`Connect`],
//! feature `driver`) connects to it, and each [`Session`] runs one link. The
//! session sends Hello and waits for HelloAck, then pushes the ready state,
//! sends each finished shot as BallData plus ClubData when the shot has club
//! values, answers every Ping with a Heartbeat, and takes the host's
//! commands:
//!
//! - SetClubInfo and SetShotContext give the club and hand, and the mode
//!   from the club and the distance to the target ([`mode_for_lie`]: the
//!   putter putts, under 30 yd chips, else a full swing; an unknown distance
//!   is full). The session sends all three in one `golfer.select` on every
//!   SetClubInfo and whenever a new distance changes the mode.
//! - SetDetect and SetBallType are logged and otherwise left alone.
//! - SetStrictMode(1) (a recorded tournament round in Pureplay) is always
//!   accepted and turns strict mode on for the link; SetStrictMode(0) or the
//!   end of the link turns it off. In strict mode the session is a
//!   measured-only receiver: each shot goes out as its launch monitor sent it
//!   (launch, club and impact; no flight result, no value filled from an
//!   `assumed`), a shot whose launch monitor does not declare its source
//!   measured ([`sends`]) is not sent (logged once per source), and the ready
//!   state and heartbeat count only measured launch monitors.
//! - An Error from the host raises a Warn alert, one per burst of errors less
//!   than [`HOST_ERROR_BURST_GAP`] apart, on any link.
//! - Any other message type is answered with Error (code 0).
//!
//! **What the session is handed** ([`FrpSession::bus`]) is its view of the
//! bus, chosen by the host: the `bus.actor` of each launch monitor it stands
//! for (and that actor's removal when it leaves the view), and the messages of
//! the shots it may send. It composes each shot from those messages itself.
//! A shot that finishes before the handshake is dropped. The launch monitors in
//! view give the device's state: the ready state and the connection state
//! are the best of theirs (a monitor whose `bus.actor` is blocked by
//! `not_armed` is connecting, any other is connected), and the name in Hello
//! and the battery are those of the only monitor in view, when there is one.
//!
//! **Telemetry** (the actor's): `ready_state` (the ready state last sent),
//! `strict_mode` (`on` | `off`), and `club` and `handed`, what Pureplay set,
//! for the link. After a failed handshake (no HelloAck, an unsupported
//! version) `error` holds the reason until a handshake succeeds.
//!
//! **Raw frames:** every frame, both ways, goes to the host as a `shot.raw`
//! labeled with its message name. The BallData of a shot carries the key the
//! shot had on the bus, after it was written, so a host can tell which of its
//! shots Pureplay was sent.

#[cfg(feature = "driver")]
pub mod codec;
#[cfg(feature = "driver")]
pub mod mapper;
#[cfg(feature = "driver")]
mod session;
#[cfg(feature = "driver")]
mod shot;

#[cfg(feature = "driver")]
pub use session::{
    Connect, Error, HELLO_TIMEOUT, HOST_ERROR_BURST_GAP, PROCESSING_LIMIT, Session, mode_for_lie,
};
#[cfg(feature = "driver")]
pub use shot::{OutputShot, measured_shot, output_shot, sends};

use std::net::SocketAddr;

use flightrelay::config::{ConfigError, DriverConfig};
use serde::{Deserialize, Serialize};

/// Pureplay's default address.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:9760";

/// The driver's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Pureplay's address, `ip:port`. Defaults to [`DEFAULT_ADDRESS`], and
    /// is left out while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

impl Config {
    /// Pureplay's address.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] for an `address` that is not `ip:port`.
    pub fn addr(&self) -> Result<SocketAddr, ConfigError> {
        self.address
            .as_deref()
            .unwrap_or(DEFAULT_ADDRESS)
            .parse()
            .map_err(|_| ConfigError::field("address", "must be ip:port, e.g. 127.0.0.1:9760"))
    }
}

impl DriverConfig for Config {
    fn validate(&self) -> Result<(), ConfigError> {
        self.addr().map(|_| ())
    }

    fn example() -> Self {
        Self {
            address: Some(String::new()),
        }
    }
}
