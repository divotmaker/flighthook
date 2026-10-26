//! JSON request/response types for the web API.
//!
//! All types live in the `flighthook` lib so the UI crate can share them.

pub use flighthook::{
    ClubRequest, GroupingRequest, ModeRequest, PostSettingsResponse, ShotGroupsResponse,
    StatusResponse,
};
