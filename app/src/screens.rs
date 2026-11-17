//! Test-only seams for the README screenshot scenes (`make screens`): this
//! module exists in debug builds alone, and each seam does nothing unless
//! its environment variable is set.
//!
//! - `FLIGHTHOOK_SCREENS_CLOCK`: the time of day (`HH:MM:SS`) telemetry shows
//!   instead of the local clock.

use std::sync::OnceLock;

/// The time of day telemetry shows, when `FLIGHTHOOK_SCREENS_CLOCK` is set.
pub fn clock() -> Option<&'static str> {
    static CLOCK: OnceLock<Option<String>> = OnceLock::new();
    CLOCK
        .get_or_init(|| {
            std::env::var("FLIGHTHOOK_SCREENS_CLOCK")
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        })
        .as_deref()
}
