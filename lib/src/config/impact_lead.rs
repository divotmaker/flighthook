//! Camera timing per launch monitor: the impact lead defaults, trigger delays and their validation.

/// Seconds before the report of a shot that a clip assumes the swing's impact
/// happened, for a launch monitor without a better-known value. Beta.
pub const DEFAULT_IMPACT_LEAD_S: f64 = 1.5;

/// Longest `impact_lead_s`, seconds.
pub const MAX_IMPACT_LEAD_S: f64 = 5.0;

/// The default impact lead of the launch monitor type `prefix` (the part of
/// an actor ID before the dot, e.g. `mevo`), seconds: how long before the
/// time of the monitor's `shot_trigger` the ball was struck. A trigger's
/// time is its `timestamp` when it carries one, else its arrival. `None`
/// for Auto-chip and the trackball, whose shots are no swing a camera
/// films. Any other source is a launch monitor and gets
/// [`DEFAULT_IMPACT_LEAD_S`] unless a value is better known.
#[must_use]
pub fn impact_lead_default_s(prefix: &str) -> Option<f64> {
    match prefix {
        // Not a swing a camera films: Auto-chip, and the trackball.
        crate::api::AUTO_CHIP_ACTOR | "trackball" => None,
        // Reports at impact.
        "mevo" | "mock_monitor" => Some(0.0),
        "exputt" => Some(0.5),
        "square" => Some(1.0),
        "r10" => Some(2.0),
        "openconnect_server" => Some(2.5),
        // The Pureplay Accessory card reports a shot once Pureplay has it
        // from its launch monitor: that monitor's delay and Pureplay's.
        "pureplay_accessory" => Some(0.9),
        _ => Some(DEFAULT_IMPACT_LEAD_S),
    }
}

/// Whether `v`, a section's `impact_lead_s`, is acceptable: absent, or finite
/// and from 0 to [`MAX_IMPACT_LEAD_S`].
#[must_use]
pub fn impact_lead_valid(v: Option<f64>) -> bool {
    v.is_none_or(|v| v.is_finite() && (0.0..=MAX_IMPACT_LEAD_S).contains(&v))
}

/// The impact lead in effect for launch monitor type `prefix`, seconds: the
/// section's `configured` value when it is valid, else the type's default
/// ([`impact_lead_default_s`]). `None` for a source that is not a swing.
#[must_use]
pub fn impact_lead_in_effect_s(prefix: &str, configured: Option<f64>) -> Option<f64> {
    let default = impact_lead_default_s(prefix)?;
    Some(
        configured
            .filter(|v| impact_lead_valid(Some(*v)))
            .unwrap_or(default),
    )
}

/// Why `impact_lead_s` of section `id` (e.g. `mevo.0`) is not acceptable, or
/// `None` when it is ([`impact_lead_valid`]).
#[must_use]
pub fn impact_lead_error(id: &str, v: Option<f64>) -> Option<String> {
    if impact_lead_valid(v) {
        return None;
    }
    v.map(|v| format!("{id}: impact_lead_s {v} is outside 0 to {MAX_IMPACT_LEAD_S} s"))
}
