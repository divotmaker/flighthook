//! flighthook's Pureplay integrations, as flightrelay drivers.
//!
//! - [`connect`]: Pureplay Connect v1, the output that sends Pureplay the
//!   shots of the launch monitors it stands for.
//!
//! Feature `config` builds the settings types only (serde), feature `driver`
//! the drivers.

#[cfg(feature = "config")]
pub mod connect;
