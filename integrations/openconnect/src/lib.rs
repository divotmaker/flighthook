//! flighthook's Open Connect integrations, as flightrelay drivers.
//!
//! - [`client`]: the Open Connect client, the output that sends a simulator
//!   (GSPro, Pureplay, Muni Golf, ...) the shots of the launch monitors it
//!   stands for.
//! - [`server`]: the Open Connect server, the launch monitor side: the
//!   software of a launch monitor that sends Open Connect connects to it, and
//!   its shots become a launch monitor's shot messages.
//!
//! The clients of this process register their connections with the
//! server of this process, which refuses them: a client pointed at its own
//! process's server would loop every shot.
//!
//! Feature `config` builds the settings and the Open Connect JSON types
//! only (serde), feature `driver` the drivers.

#[cfg(feature = "config")]
pub mod client;
#[cfg(feature = "driver")]
mod local;
#[cfg(feature = "config")]
pub mod server;
#[cfg(feature = "testing")]
pub mod testing;
