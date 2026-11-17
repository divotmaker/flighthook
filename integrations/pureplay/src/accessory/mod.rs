//! Pureplay's Accessory API (beta): the Accessory driver, [`Accessory`].
//!
//! Pureplay listens on a WebSocket (`ws://<host>:9770/`, subprotocol
//! `binary`), sends JSON in binary frames, and takes JSON in text frames.
//! The driver pairs (the token kept in a file of its own, [`token`]),
//! follows the round, publishes it as the bus's `game` (the `pureplay`
//! golfer namespace: `golfer.profile`, `game.round`, `game.ball`,
//! `game.scores`), sends the turn of the local player up as one complete
//! `golfer.select`, sends a club picked in flighthook to Pureplay, reports
//! every shot of a local player as its own (`shot.trigger`, `shot.launch`,
//! `shot.finished`) and Pureplay's result as `game.stroke`, and answers
//! `game.aim` `unsupported`. Inside the crate: `protocol` (the messages and
//! their parser), `model` (the round and the select it derives), `game` (the
//! model as bus messages), `club_out`, `shots` (Pureplay's shots as
//! flighthook's) and `session` (the driver). [`link`] is one connection,
//! and its WebSocket form.
//!
//! Feature `config` builds [`Config`] only (serde), feature `driver` the rest.

#[cfg(feature = "driver")]
mod card;
#[cfg(feature = "driver")]
pub(crate) mod club_out;
#[cfg(feature = "driver")]
pub(crate) mod game;
#[cfg(feature = "driver")]
pub mod link;
#[cfg(feature = "driver")]
pub(crate) mod model;
#[cfg(feature = "driver")]
pub(crate) mod protocol;
#[cfg(feature = "driver")]
mod session;
#[cfg(feature = "driver")]
pub(crate) mod shots;
#[cfg(feature = "driver")]
pub mod token;

#[cfg(feature = "driver")]
pub use session::{Accessory, Error, Session};
/// How many of Pureplay's recent shots the driver maps to its own.
#[cfg(feature = "driver")]
pub use shots::MAP_CAPACITY as SHOTS_KEPT;

use std::net::SocketAddr;

use flightrelay::config::{ConfigError, DriverConfig};
use serde::{Deserialize, Serialize};

/// Pureplay's default Accessory address.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:9770";

/// The card's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Pureplay's Accessory address, `ip:port`. Defaults to
    /// [`DEFAULT_ADDRESS`].
    #[serde(default)]
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

    /// Only another address (or token file) restarts the card: an unset
    /// address is the default one.
    fn restart_needed(old: &Self, new: &Self) -> bool {
        old.addr().ok() != new.addr().ok() || old.token_file != new.token_file
    }

    fn example() -> Self {
        Self {
            address: Some(String::new()),
            token_file: None,
        }
    }
}
