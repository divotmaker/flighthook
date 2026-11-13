pub(crate) mod auto_chip;
pub(crate) mod body_models;
pub(crate) mod cameras;
pub(crate) mod card_row;
pub(crate) mod cards;
pub(crate) mod devices;
pub(crate) mod dispersion;
pub(crate) mod flight;
pub(crate) mod impact;
pub(crate) mod last_shot;
pub(crate) mod log;
pub(crate) mod monitoring;
pub(crate) mod remote;
pub(crate) mod replay;
pub(crate) mod settings;
pub(crate) mod settings_nav;
pub(crate) mod shots;
pub(crate) mod signs;

use std::collections::HashMap;

use crate::types::ActorStatusResponse;

/// What the nav shows: Settings (the device, output and global cards), the Shots and
/// Cameras tabs (`Shots`, with [`ShotsPane`](cameras::ShotsPane) picking which) or the Log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    /// Settings: the cards of the devices, outputs and global settings.
    Devices,
    Shots,
    Log,
}

/// Whether the Settings nav icon carries the red notification dot: while
/// any card is red at `now_ms`, i.e. an actor lost its connection less than
/// [`RECONNECT_ALERT_MS`](crate::types::RECONNECT_ALERT_MS) ago. Every
/// actor counts alike, outputs and servers included; nothing else lights it.
pub(crate) fn settings_alert(actors: &HashMap<String, ActorStatusResponse>, now_ms: u64) -> bool {
    actors.values().any(|a| a.red(now_ms))
}

/// Milliseconds until the first red card turns yellow, if any card is red.
pub(crate) fn next_alert_change(
    actors: &HashMap<String, ActorStatusResponse>,
    now_ms: u64,
) -> Option<u64> {
    actors
        .values()
        .map(|a| a.red_for_ms_at(now_ms))
        .filter(|ms| *ms > 0)
        .min()
}

/// Extract the time portion from an ISO 8601 timestamp string.
/// "2026-02-27T12:34:56.789Z" -> "12:34:56.789"
pub(crate) fn extract_time(iso: &str) -> String {
    // Find the 'T' separator and take everything after it, trimming trailing 'Z'
    if let Some(t_pos) = iso.find('T') {
        let time_part = &iso[t_pos + 1..];
        time_part.trim_end_matches('Z').to_string()
    } else {
        iso.to_string()
    }
}
