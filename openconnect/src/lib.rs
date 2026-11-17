//! flighthook's Open Connect integrations, as flightrelay drivers.
//!
//! - [`client`]: the Open Connect client, the output that sends a simulator
//!   (GSPro, Pureplay, Muni Golf, ...) the shots of the launch monitors it
//!   stands for.
//! - [`local`]: this process's own Open Connect client connections, which
//!   an Open Connect server in the same process refuses.
//!
//! Feature `config` builds the settings and the Open Connect JSON types
//! only (serde), feature `driver` the drivers.

#[cfg(feature = "config")]
pub mod client;
#[cfg(feature = "driver")]
pub mod local;
