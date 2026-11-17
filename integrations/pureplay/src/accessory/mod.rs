//! Pureplay's Accessory API (beta): the parts of the Accessory card that are
//! pure.
//!
//! Pureplay listens on a WebSocket (`ws://<host>:9770/`, subprotocol
//! `binary`), sends JSON in binary frames, and takes JSON in text frames.
//!
//! - [`protocol`]: the messages, both ways, and their parser.
//! - [`model`]: the round as the card follows it (players, bags, the turn,
//!   readiness) and the complete `golfer.select` it derives.
//! - [`game`]: the model as the bus's `game.*` and `golfer.profile` messages.
//! - [`club_out`]: a club picked in flighthook on its way to Pureplay.
//! - [`shots`]: Pureplay's shots as flighthook's, and the map from a
//!   Pureplay shot to the flighthook shot it stands for.
//! - [`token`]: the pairing tokens, kept in a file of their own, and their
//!   redaction.
//! - [`link`]: [`Link`](link::Link), one connection, and its WebSocket form.
//!
//! Feature `config` builds [`Config`] only (serde), feature `driver` the rest.

#[cfg(feature = "driver")]
pub mod club_out;
#[cfg(feature = "driver")]
pub mod game;
#[cfg(feature = "driver")]
pub mod link;
#[cfg(feature = "driver")]
pub mod model;
#[cfg(feature = "driver")]
pub mod protocol;
#[cfg(feature = "driver")]
pub mod shots;
#[cfg(feature = "driver")]
pub mod token;

use std::net::SocketAddr;
#[cfg(feature = "driver")]
use std::time::Duration;

use flightrelay::config::{ConfigError, DriverConfig};
use serde::{Deserialize, Serialize};

/// Pureplay's default Accessory address.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:9770";

/// The card's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Pureplay's Accessory address, `ip:port`. Defaults to
    /// [`DEFAULT_ADDRESS`], and is left out while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// The token file, for tests; unset uses the user's data folder.
    #[serde(skip)]
    pub token_file: Option<std::path::PathBuf>,
}

impl Config {
    /// Pureplay's Accessory address.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] for an `address` that is not `ip:port`.
    pub fn addr(&self) -> Result<SocketAddr, ConfigError> {
        self.address
            .as_deref()
            .unwrap_or(DEFAULT_ADDRESS)
            .parse()
            .map_err(|_| ConfigError::field("address", "must be ip:port, e.g. 127.0.0.1:9770"))
    }
}

impl DriverConfig for Config {
    fn validate(&self) -> Result<(), ConfigError> {
        self.addr().map(|_| ())
    }

    fn example() -> Self {
        Self {
            address: Some(String::new()),
            token_file: None,
        }
    }
}

/// `ms` in a build, `test_ms` in tests (this crate's, and a dependent's with
/// feature `testing`).
#[cfg(feature = "driver")]
pub(crate) const fn timing(ms: u64, test_ms: u64) -> Duration {
    if cfg!(any(test, feature = "testing")) {
        Duration::from_millis(test_ms)
    } else {
        Duration::from_millis(ms)
    }
}
