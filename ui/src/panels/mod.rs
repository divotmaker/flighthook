pub(crate) mod log;
pub(crate) mod monitoring;
pub(crate) mod settings;
pub(crate) mod shots;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Tab {
    Shots,
    Telemetry,
    Log,
    Settings,
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
