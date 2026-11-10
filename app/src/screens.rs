//! Test-only seams for the README screenshot scenes (`make screens`): this
//! module exists in debug builds alone, and each seam does nothing unless
//! its environment variable is set.
//!
//! - `FLIGHTHOOK_SCREENS_GRID_MS`: a time grid on the host clock. A `clip:`
//!   camera puts its clip's labeled impact on a whole multiple of it, and
//!   every camera stamps a shot trigger at the next multiple at or after its
//!   arrival, so a shot's clip holds the same frames, impact on the same
//!   frame, whenever the shot arrives within one grid step.
//! - `FLIGHTHOOK_SCREENS_CLOCK`: the time of day (`HH:MM:SS`) telemetry shows
//!   instead of the local clock.

use std::sync::OnceLock;

/// The grid step in ns, when `FLIGHTHOOK_SCREENS_GRID_MS` is a positive
/// whole number.
pub fn grid_ns() -> Option<u64> {
    static GRID: OnceLock<Option<u64>> = OnceLock::new();
    *GRID.get_or_init(|| {
        std::env::var("FLIGHTHOOK_SCREENS_GRID_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .map(|ms| ms.saturating_mul(1_000_000))
    })
}

/// The first multiple of `grid` at or after `at_ns`.
pub fn ceil_to(at_ns: u64, grid: u64) -> u64 {
    at_ns.div_ceil(grid).saturating_mul(grid)
}

/// A trigger arriving at `at_ns`, stamped on the grid when it is on.
pub fn trigger_ns(at_ns: u64) -> u64 {
    grid_ns().map_or(at_ns, |grid| ceil_to(at_ns, grid))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_round_up_to_the_grid() {
        let grid = 2_000_000_000;
        assert_eq!(ceil_to(0, grid), 0);
        assert_eq!(ceil_to(1, grid), grid);
        assert_eq!(ceil_to(grid, grid), grid);
        assert_eq!(ceil_to(grid + 1, grid), 2 * grid);
    }
}
