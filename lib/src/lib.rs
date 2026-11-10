mod address;
mod api;
#[cfg(feature = "client")]
mod client;
mod config;
mod connection;
mod event;
mod export;
mod game_state;
mod grades;
mod group;
mod message;
mod metrics;
mod pose;
mod stats;
mod video;

pub use address::*;
pub use api::*;
#[cfg(feature = "client")]
pub use client::*;
pub use config::*;
pub use connection::*;
pub use event::*;
pub use export::*;
pub use game_state::*;
pub use grades::*;
pub use group::*;
pub use message::*;
pub use metrics::*;
pub use pose::*;
pub use stats::*;
pub use video::*;

// Re-export flightrelay types used throughout flighthook.
pub use allsquare::config::Config as SquareConfig;
pub use flightrelay::config::{ConfigError, DriverConfig};
pub use flightrelay::mock::MockConfig;
pub use flightrelay::types::{AssumedLaunch, Basis, ClubData, FaceImpact, FlightResult, Launch};
pub use flightrelay::units::{Distance, Velocity};
pub use flightrelay::{DEFAULT_PATH as FRP_PATH, DEFAULT_PORT as FRP_PORT, DEFAULT_URL as FRP_URL};
pub use flightrelay::{Handedness, SPEC_VERSION as FRP_VERSION, Severity};
pub use ironsight::config::Config as MevoConfig;
/// What the FlightScope driver uses for a setting left unset.
pub use ironsight::config::{
    DEFAULT_ADDRESS as MEVO_DEFAULT_ADDRESS, DEFAULT_BALL_TYPE as MEVO_DEFAULT_BALL_TYPE,
    DEFAULT_RANGE as MEVO_DEFAULT_RANGE, DEFAULT_SURFACE_HEIGHT as MEVO_DEFAULT_SURFACE_HEIGHT,
    DEFAULT_TEE_HEIGHT as MEVO_DEFAULT_TEE_HEIGHT, DEFAULT_TRACK_PCT as MEVO_DEFAULT_TRACK_PCT,
};
pub use tenover::config::Config as R10Config;
pub use threeputt::config::Config as ExPuttConfig;
