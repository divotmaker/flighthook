//! What the cameras read from the live config: each setting in effect (a
//! value out of range is its default). Everything is read at the point of
//! use, so a change applies without restarting a camera.

use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::Duration;

use flighthook::{BodyTrackingSection, FlighthookConfig, MAX_LATENCY_S, ReplaySection};

/// The live config, shared with the host, which changes it.
#[derive(Clone)]
pub(crate) struct Settings(Arc<RwLock<FlighthookConfig>>);

impl Settings {
    /// The cameras' view of the host's live `config`.
    pub(crate) fn new(config: Arc<RwLock<FlighthookConfig>>) -> Self {
        Self(config)
    }

    /// The config as it is now.
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, FlighthookConfig> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The `[body_tracking]` settings in effect.
    pub(crate) fn body_tracking(&self) -> BodyTrackingSection {
        self.read().body_tracking.in_effect()
    }

    /// The `[replay]` section.
    pub(crate) fn replay(&self) -> ReplaySection {
        self.read().replay.clone()
    }
}

/// The impact lead in effect for launch monitor `actor` (e.g. `mevo.0`),
/// seconds: its section's `impact_lead_s` when valid, else the monitor
/// type's default. `None` for a source that is not a swing (Auto-chip).
/// Read at every trigger, so a change applies to the next shot.
#[must_use]
pub fn impact_lead_s(config: &FlighthookConfig, actor: &str) -> Option<f64> {
    let prefix = actor.split_once('.').map_or(actor, |(prefix, _)| prefix);
    flighthook::impact_lead_in_effect_s(prefix, config.configured_impact_lead_s(actor))
}

/// How far before a trigger's arrival the cameras' history has to reach
/// for the configured launch monitors, seconds (0 with none): the largest
/// impact lead in effect.
#[must_use]
pub fn longest_reach_s(config: &FlighthookConfig) -> f64 {
    config
        .launch_monitor_leads()
        .into_iter()
        .filter_map(|(kind, _, lead)| flighthook::impact_lead_in_effect_s(kind, lead))
        .fold(0.0, f64::max)
}

/// The latency in effect for camera `actor` (e.g. `camera.0`), seconds:
/// its section's `latency_s` when valid, else 0. Read for every frame, so
/// a change applies at once without reopening the camera.
#[must_use]
pub fn camera_latency_s(config: &FlighthookConfig, actor: &str) -> f64 {
    let configured = actor
        .strip_prefix("camera.")
        .and_then(|index| config.camera.get(index))
        .and_then(|c| c.latency_s);
    flighthook::latency_in_effect_s(configured)
}

/// [`camera_latency_s`] as a duration, clamped to the valid range.
#[must_use]
pub fn latency(config: &FlighthookConfig, actor: &str) -> Duration {
    let s = camera_latency_s(config, actor);
    Duration::from_secs_f64(if s.is_finite() {
        s.clamp(0.0, MAX_LATENCY_S)
    } else {
        0.0
    })
}

/// The rotation in effect for camera `actor` (e.g. `camera.0`), clockwise
/// degrees: its section's `rotation_deg` when valid, else 0. Read for
/// every live frame and every clip pass, so a change applies at once
/// without reopening the camera.
#[must_use]
pub fn camera_rotation(config: &FlighthookConfig, actor: &str) -> u16 {
    actor
        .strip_prefix("camera.")
        .and_then(|index| config.camera.get(index))
        .map_or(0, |c| flighthook::rotation_in_effect_deg(c.rotation_deg))
}

/// The facts (sex, height) of the golfer golfer id `golfer` means, by the
/// lookup rule; a height out of range is no height. The defaults when no
/// golfer has the id. Read by the swing analysis and the grades of every
/// clip.
#[must_use]
pub fn golfer_facts(config: &FlighthookConfig, golfer: Option<&str>) -> flighthook::PlayerSection {
    golfer
        .and_then(|id| config.golfer_of(id))
        .map(flighthook::GolferRecord::facts)
        .unwrap_or_default()
}
