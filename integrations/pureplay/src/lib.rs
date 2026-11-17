//! flighthook's Pureplay integrations, as flightrelay drivers.
//!
//! - [`connect`]: Pureplay Connect v1, the output that sends Pureplay the
//!   shots of the launch monitors it stands for.
//! - [`accessory`]: Pureplay's Accessory API (beta): the protocol, the round
//!   model, the game messages, the club going out, Pureplay's shots, the
//!   pairing tokens and the link.
//!
//! Feature `config` builds the settings types only (serde), feature `driver`
//! the drivers.

#[cfg(feature = "config")]
pub mod accessory;
#[cfg(feature = "config")]
pub mod connect;
