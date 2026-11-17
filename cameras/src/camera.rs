//! A swing camera (beta): [`Camera::run`] records a clip of every swing from
//! one `[camera.N]`.
//!
//! It runs a stance [`Capture`] (the camera on its own thread, frames into a
//! bounded ring) and feeds every frame to a [`ClipRecorder`], which keeps the
//! pre-roll. Each launch monitor's [`Trigger`] becomes a recorder trigger,
//! dated by the trigger's own `timestamp` when it carries one (the strike),
//! else when the camera reads it (the host is asked for triggers before the
//! frames on every pass, and a pass waits at most [`FRAME_WAIT`] for frames,
//! so a stamp is at most that late), with the impact estimated
//! [`impact_lead`] earlier: the monitor's own `impact_lead_s` setting, else
//! the default for that kind of monitor, both read live at every trigger.
//! The history reaches back over the longest lead plus the trigger delay of
//! the monitors' types ([`longest_reach`]), so a trigger dated at a strike
//! seconds before it arrived (the ExPutt's) still finds its frames.
//! Pre- and post-roll come from the live `[replay]` section the same way.
//! A camera that delivers late (a buffered network stream) has a `latency_s`:
//! every frame is dated that much earlier than it arrived ([`Restamp`]), so
//! frames, triggers and impact estimates share real time, and the recorder's
//! wall-clock `poll` runs on the same shifted clock ([`frame_clock`]). A trigger from a
//! second monitor for the same swing joins the open clip; the same monitor
//! triggering again cuts it and starts another. Auto-chip shots are never
//! recorded.
//!
//! A finished clip is written on a thread of its own into the clip store
//! (`Cameras::clips`), then announced with one [`Event::Video`]; no frame
//! leaves the crate. The newest frame is put in the camera's slot
//! (`Cameras::frames`) at most twice a second, for the device card's
//! still, and, while a viewer of the Live view has asked within the last
//! few seconds, in its live slot at most [`LIVE_FPS`] times a second.
//!
//! With body tracking in Live mode (alpha, `flighthook_cameras::pose`), a
//! watched live view's frames go through the camera's body tracking worker,
//! which puts each in the live slot with the golfer's keypoints on it, and
//! every saved clip is queued for body tracking of the whole swing. In
//! Historical mode a clip is tracked when a viewer asks for it
//! (`Cameras::request`).
//!
//! Status follows the host's other devices: `starting` while opening,
//! `connected` while frames arrive, `reconnecting` with a growing backoff
//! after the camera is lost (a clip that holds its impact frame is saved,
//! marked incomplete), `disconnected` when stopped or when the platform has
//! no camera support.
//!
//! The recording rules live in [`Recording`] (the recorder and the triggers
//! seen, across capture sessions) and [`Pass`] (one capture session's
//! still, dating and telemetry). Neither reads a clock: the loop around
//! them reads the time, the host and the camera, and passes them in.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use stance::{
    Capture, CaptureConfig, CaptureStats, CaptureStatus, ClipRecorder, FormatRequest, Frame,
    RecorderConfig, RecorderEvent, Source, TestPattern, clock,
};
use tracing::{debug, info, warn};

use crate::backoff::Backoff;
use crate::clips::{self, ClipShot, clip_impact};
use crate::pose::clips::{ClipJob, ClipTiming};
use crate::pose::live::{self, LIVE_FPS, LiveWorker};
use crate::pose::models;
use crate::settings;
use crate::{Cameras, Event, Out};
use flighthook::{
    ActorStatus, BodyTrackingMode, CameraSection, FlighthookConfig, GolferTurn, MAX_IMPACT_LEAD_S,
    MAX_TRIGGER_DELAY_S, ReplaySection, ShotKey, ShotVideo,
};

/// Recorder history beyond the pre-roll and the longest reach.
const BUFFER_MARGIN: Duration = Duration::from_millis(500);

/// Longest a capture pass waits for frames before it asks the host for
/// triggers again: the most a trigger's stamp can lag its arrival.
const FRAME_WAIT: Duration = Duration::from_millis(10);

/// Least time between two telemetry updates.
const TELEMETRY_EVERY: Duration = Duration::from_secs(2);

/// How often the newest frame is put in the camera's still slot.
const STILL_EVERY: Duration = Duration::from_millis(500);

/// Least time between two live frames.
const LIVE_EVERY: Duration = Duration::from_nanos(1_000_000_000 / LIVE_FPS as u64);

/// How long the frame rate must stay under half the requested one before
/// the camera warns.
const SLOW_FOR: Duration = Duration::from_secs(10);

/// Triggers remembered to drop repeats: a device that reconnects, or a
/// message replayed, sends a shot it already sent within a few shots, so
/// the last 64 per camera are plenty.
const SEEN_TRIGGERS: usize = 64;

/// Least change in the measured frame rate that is reported, as a share of
/// the mode's rate (at least 1 fps): a camera hovering around its rate
/// doesn't send a status every few seconds.
const FPS_REPORT_STEP: f64 = 0.02;

/// The status error on platforms without camera support.
const UNSUPPORTED: &str = "cameras are not supported on macOS yet";

/// How long before its trigger the launch monitor `actor` (e.g.
/// `mevo.0`) struck the ball: the `impact_lead_s` of its live config section,
/// else the default for its kind, or `None` for a source that is not a swing
/// (Auto-chip). These are beta estimates: FlightScope triggers at impact; the
/// others report once the shot is measured, which the clip's pre-roll has to
/// reach back over.
fn impact_lead(config: &FlighthookConfig, actor: &str) -> Option<Duration> {
    settings::impact_lead_s(config, actor).map(lead)
}

/// A lead in seconds as a duration, clamped to the valid range.
fn lead(seconds: f64) -> Duration {
    clamped(seconds, MAX_IMPACT_LEAD_S)
}

/// A reach in seconds (a lead plus a trigger delay) as a duration, clamped
/// to the valid range.
fn reach(seconds: f64) -> Duration {
    clamped(seconds, MAX_IMPACT_LEAD_S + MAX_TRIGGER_DELAY_S)
}

/// `seconds` as a duration, clamped to 0..=`max` (0 when not finite).
fn clamped(seconds: f64, max: f64) -> Duration {
    let s = if seconds.is_finite() {
        seconds.clamp(0.0, max)
    } else {
        0.0
    };
    Duration::from_secs_f64(s)
}

/// The latency in effect for camera `actor` (e.g. `camera.0`): its section's
/// `latency_s` when valid, else 0. Read live, so a change applies to the next
/// frame without reopening the camera.
fn latency(cameras: &Cameras, actor: &str) -> Duration {
    settings::latency(&cameras.settings().read(), actor)
}

/// Dates a camera's frames when they were captured instead of when they
/// arrived: `arrival - latency`, so that frames, triggers and impact
/// estimates are on one clock. The recorder takes frames only in strictly
/// increasing time, so a frame that would not be newer than the newest one
/// passed on is dropped; that happens right after the latency is raised, for
/// as long as the camera's already-delivered frames overlap the new dating.
/// A date before the clock's zero is 0.
#[derive(Debug, Default)]
struct Restamp {
    newest: Option<u64>,
}

impl Restamp {
    /// `frame` dated `latency` earlier (its device time moves with it), or
    /// `None` when that date is not after the newest frame passed on.
    fn apply(&mut self, mut frame: Frame, latency: Duration) -> Option<Frame> {
        let back = u64::try_from(latency.as_nanos()).unwrap_or(u64::MAX);
        frame.host_ns = frame.host_ns.saturating_sub(back);
        frame.device_ns = frame.device_ns.map(|d| d.saturating_sub(back));
        if self.newest.is_some_and(|n| frame.host_ns <= n) {
            return None;
        }
        self.newest = Some(frame.host_ns);
        Some(frame)
    }
}

/// Paces the live slot to [`LIVE_FPS`]: frames are due one [`LIVE_EVERY`]
/// apart on a schedule, so a camera a little faster than an even multiple
/// still delivers the full rate; a schedule fallen a whole interval behind
/// restarts from the frame at hand.
#[derive(Debug, Default)]
struct LiveGate {
    /// When the next live frame is due.
    next: Option<Instant>,
}

impl LiveGate {
    /// Whether a frame at `now` goes to the live slot, `wanted` saying
    /// whether a viewer watches it. Not watched, nothing goes, and the
    /// schedule starts afresh with the next viewer.
    fn admit(&mut self, wanted: bool, now: Instant) -> bool {
        if !wanted {
            self.next = None;
            return false;
        }
        if self.next.is_some_and(|due| now < due) {
            return false;
        }
        let base = self
            .next
            .filter(|due| now.duration_since(*due) < LIVE_EVERY)
            .unwrap_or(now);
        self.next = Some(base + LIVE_EVERY);
        true
    }
}

/// Where a capture pass's newest frame goes while a viewer watches the
/// live view: with body tracking Live, to the camera's body tracking worker
/// (`pose:camera.N`), which puts it in the live slot with its pose at the
/// pose rate (at most [`LIVE_FPS`]); otherwise straight into the slot,
/// paced by [`LiveGate`]. The worker runs only while the camera is watched
/// in Live mode: a lapsed watch or another mode stops it, and the next
/// viewer gets a fresh tracker.
///
/// A worker whose thread ended by itself (a panic) is replaced, up to
/// [`LIVE_RESTARTS`] times; after that live tracking stays off for the
/// camera, which its live frames say (`X-Pose-State` `failed`), until the
/// watch lapses or body tracking leaves Live.
struct LiveRoute {
    gate: LiveGate,
    worker: Option<LiveWorker>,
    /// Workers that ended by themselves since the watch began.
    deaths: u32,
    /// Starts a worker: [`start_live_worker`], or a stand-in in tests.
    start: fn(&Arc<Cameras>, &str) -> Option<LiveWorker>,
}

/// How many times a camera's live worker is replaced after its thread
/// ended by itself, while it is watched.
const LIVE_RESTARTS: u32 = 3;

impl Default for LiveRoute {
    fn default() -> Self {
        Self {
            gate: LiveGate::default(),
            worker: None,
            deaths: 0,
            start: start_live_worker,
        }
    }
}

impl LiveRoute {
    /// Route the frame at `time_ns`, `wanted` saying whether a viewer
    /// watches.
    fn route(
        &mut self,
        cameras: &Arc<Cameras>,
        actor: &str,
        wanted: bool,
        now: Instant,
        time_ns: u64,
        jpeg: Arc<[u8]>,
    ) {
        if wanted && cameras.settings().body_tracking().mode == BodyTrackingMode::Live {
            if self.worker.as_ref().is_some_and(|w| !w.alive()) {
                self.worker = None;
                self.deaths += 1;
                if self.deaths > LIVE_RESTARTS {
                    warn!("{actor}: live body tracking stopped after repeated errors");
                    cameras.pose.set_live_failed(
                        actor,
                        Some("stopped on this camera after repeated errors".to_owned()),
                    );
                } else {
                    warn!("{actor}: live body tracking stopped on an error; restarting it");
                }
            }
            if self.worker.is_none() && self.deaths <= LIVE_RESTARTS {
                self.worker = (self.start)(cameras, actor);
            }
            if let Some(worker) = &self.worker {
                self.gate.admit(false, now);
                worker.offer(time_ns, jpeg);
                return;
            }
        } else {
            self.worker = None;
            if self.deaths > 0 {
                self.deaths = 0;
                cameras.pose.set_live_failed(actor, None);
            }
        }
        if self.gate.admit(wanted, now) {
            cameras.frames.set_live(actor, time_ns, jpeg, None);
        }
    }
}

/// Start camera `actor`'s live body tracking worker on the shared models,
/// following the live settings. `None` (logged) when its thread can't
/// start: frames then go out without a pose.
fn start_live_worker(cameras: &Arc<Cameras>, actor: &str) -> Option<LiveWorker> {
    let settings_cameras = Arc::clone(cameras);
    let settings_actor = actor.to_owned();
    let settings: live::Settings =
        Box::new(move || live_settings(&settings_cameras, &settings_actor));
    let build_cameras = Arc::clone(cameras);
    let build_actor = actor.to_owned();
    let build: live::Build = Box::new(move |s, rotation| {
        let shared = build_cameras.pose.models.want(s.model).ready()?;
        let made = shared.fork().and_then(|m| {
            m.tracker(stance::pose::PoseConfig {
                rotation,
                ..models::pose_config(s.model, s.detect_every)
            })
        });
        match made {
            Ok(tracker) => Some(Box::new(tracker) as Box<dyn live::Estimate>),
            Err(e) => {
                warn!("{build_actor}: body tracking could not start: {e}");
                None
            }
        }
    });
    let publish_cameras = Arc::clone(cameras);
    let publish_actor = actor.to_owned();
    let publish: live::Publish = Box::new(move |time_ns, jpeg, pose| {
        publish_cameras
            .frames
            .set_live(&publish_actor, time_ns, jpeg, pose);
    });
    match LiveWorker::start(actor, settings, build, publish) {
        Ok(worker) => Some(worker),
        Err(e) => {
            warn!("{actor}: no thread for body tracking: {e}");
            None
        }
    }
}

/// What camera `actor`'s live view follows now: body tracking's settings,
/// the posture setup for the camera's position and the player's hand, and
/// the camera's rotation.
fn live_settings(cameras: &Cameras, actor: &str) -> live::LiveSettings {
    let index = actor.strip_prefix("camera.").unwrap_or("");
    let handedness = cameras.turn().handedness;
    let config = cameras.settings().read();
    let position = config
        .camera
        .get(index)
        .map(|c| c.position)
        .unwrap_or_default();
    live::LiveSettings {
        body: config.body_tracking.in_effect(),
        posture: crate::pose::swing::live_setup(
            position,
            handedness,
            crate::pose::swing::mirrored(actor),
        ),
        rotation: stance::pose::Rotation::from_degrees(settings::camera_rotation(&config, actor))
            .unwrap_or_default(),
    }
}

/// Ask for body tracking's models in Live mode, so they are loaded before a
/// viewer or a clip needs them (never downloaded: missing files wait for
/// the user's download). In Historical mode the first clip viewed asks for
/// them.
fn want_models(cameras: &Cameras) {
    let settings = cameras.settings().body_tracking();
    if settings.mode == BodyTrackingMode::Live {
        cameras.pose.models.want(settings.model);
    }
}

/// The recorder's wall clock for `now_ns`: frames are dated `latency`
/// earlier than they arrive, so a frame dated up to this instant is only
/// complete `latency` after it. The recorder finishes a clip once this
/// passes its window by its stall time, which lets late frames in first.
fn frame_clock(now_ns: u64, latency: Duration) -> u64 {
    now_ns.saturating_sub(u64::try_from(latency.as_nanos()).unwrap_or(u64::MAX))
}

/// The recorder settings for `replay`: its rolls, and history for the
/// pre-roll plus `longest_reach`, how far before a trigger's arrival its
/// impact can lie ([`longest_reach`]). Frames are dated by capture time, so
/// a camera's latency adds nothing to it.
fn recorder_config(
    replay: &ReplaySection,
    longest_reach: Duration,
    fps: Option<f64>,
) -> RecorderConfig {
    let pre_roll = roll(replay.pre_roll_s);
    RecorderConfig {
        pre_roll,
        post_roll: roll(replay.post_roll_s),
        buffer: pre_roll + longest_reach + BUFFER_MARGIN,
        fps,
        ..RecorderConfig::default()
    }
}

/// How far before a trigger's arrival its impact can lie, over the
/// configured launch monitors: the longest impact lead in effect plus the
/// monitor type's trigger delay (`flighthook::trigger_delay_s`).
fn longest_reach(config: &FlighthookConfig) -> Duration {
    reach(settings::longest_reach_s(config))
}

/// When a trigger read at `now_ns` (host monotonic clock) happened: its
/// `timestamp` (UTC ms) on the same clock, else `now_ns`. A timestamp
/// ahead of the host's UTC clock counts as now. The UTC clock is read
/// afresh for each trigger: a step of the host's wall clock (an NTP
/// correction) between the driver's stamp and this read moves the clip by
/// the step. Steps are rare and a stamp is at most seconds old (the
/// ExPutt's), so no anchor is kept.
fn trigger_time(now_ns: u64, timestamp: Option<u64>) -> u64 {
    let Some(timestamp) = timestamp else {
        return now_ns;
    };
    let utc_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(timestamp);
    now_ns.saturating_sub(utc_ms.saturating_sub(timestamp).saturating_mul(1_000_000))
}

/// A roll in seconds as a duration, clamped to the valid range.
fn roll(seconds: f64) -> Duration {
    let s = if seconds.is_finite() {
        seconds.clamp(0.0, ReplaySection::MAX_ROLL_S)
    } else {
        0.0
    };
    Duration::from_secs_f64(s)
}

/// The shots the camera has seen triggered, to drop a repeated trigger for
/// the same shot (a reconnecting device, a replayed message).
#[derive(Default)]
struct SeenTriggers(VecDeque<(String, String)>);

impl SeenTriggers {
    /// Whether `actor`'s shot `key` is new; remembers it.
    fn first(&mut self, actor: &str, key: &ShotKey) -> bool {
        let entry = (actor.to_owned(), key.shot_id.clone());
        if self.0.contains(&entry) {
            return false;
        }
        if self.0.len() == SEEN_TRIGGERS {
            self.0.pop_front();
        }
        self.0.push_back(entry);
        true
    }
}

/// A launch monitor's shot trigger, as the camera takes it.
#[derive(Debug, Clone)]
pub struct Trigger {
    /// The monitor's card (e.g. `mevo.0`), or a remote monitor's actor id.
    pub card: String,
    /// The kind of device, for the clip's shot (e.g. `flightscope`).
    pub device: String,
    pub key: ShotKey,
    /// The strike's time (UTC ms) when the trigger carries one.
    pub timestamp: Option<u64>,
}

/// What the host tells a camera when it is asked: [`Camera::run`]'s `poll`.
#[derive(Debug, Clone)]
pub enum Input {
    /// The camera is to stop. It stays so.
    Stop,
    /// Nothing is waiting.
    Idle,
    /// A launch monitor triggered.
    Trigger(Trigger),
}

/// What outlives a capture session: the recorder, with its history and its
/// open clips, and the triggers seen. It never reads a clock: every time is
/// passed in.
struct Recording {
    recorder: ClipRecorder<Tag>,
    seen: SeenTriggers,
}

impl Recording {
    /// A recorder for `config`'s `[replay]` and launch monitors, before the
    /// camera's frame rate is known.
    fn new(config: &FlighthookConfig) -> Self {
        Self {
            recorder: ClipRecorder::new(recorder_config(
                &config.replay,
                longest_reach(config),
                None,
            )),
            seen: SeenTriggers::default(),
        }
    }

    /// The camera delivers `nominal` frames a second (0 when unknown): the
    /// recorder counts dropped frames by it.
    fn set_fps(&mut self, nominal: f64) {
        let mut config = self.recorder.config().clone();
        config.fps = (nominal > 0.0).then_some(nominal);
        self.recorder.set_config(config);
    }

    /// Apply trigger `t`, dated `at_ns`: its `timestamp` when it has one,
    /// else its arrival. Auto-chip, and repeats are ignored. The lead and
    /// the rolls come from `config` (the source's `impact_lead_s`, the
    /// `[replay]` section), the clip's hand and golfer from `turn`, and
    /// `generation` is the clip store's. A trigger without a timestamp from
    /// a type that dates its triggers at the strike and sends them late (an
    /// ExPutt putt reported without its strike message) takes the generic
    /// lead instead ([`flighthook::undated_impact_lead_s`]): it is dated at
    /// the result's arrival.
    fn trigger(
        &mut self,
        config: &FlighthookConfig,
        t: &Trigger,
        at_ns: u64,
        turn: &GolferTurn,
        generation: u64,
    ) {
        let source = t.card.as_str();
        let key = &t.key;
        let Some(mut lead) = impact_lead(config, source) else {
            return;
        };
        if !self.seen.first(source, key) {
            debug!("camera: repeated trigger for {source} shot {}", key.shot_id);
            return;
        }
        let prefix = source.split_once('.').map_or(source, |(p, _)| p);
        let mut delay = reach(flighthook::trigger_delay_s(prefix));
        if t.timestamp.is_none()
            && let Some(undated) = flighthook::undated_impact_lead_s(prefix)
        {
            debug!(
                "camera: {source} shot {} has no strike time; dated at its arrival, {undated} s lead",
                key.shot_number
            );
            lead = self::lead(undated);
            // It arrived when it is dated: nothing late to reach back over.
            delay = Duration::ZERO;
        }
        let recorder = &mut self.recorder;
        // The history covers this monitor's reach even if its section is gone.
        let mut cut = recorder_config(
            &config.replay,
            longest_reach(config).max(lead + delay),
            recorder.config().fps,
        );
        cut.merge_window = recorder.config().merge_window;
        if cut.buffer != recorder.config().buffer {
            recorder.set_config(cut.clone());
        }
        let shot = ClipShot {
            key: key.clone(),
            device: t.device.clone(),
            impact_lead_ms: u64::try_from(lead.as_millis()).unwrap_or(u64::MAX),
            hand: turn.handedness,
            golfer: turn.golfer.as_ref().map(|g| g.as_str().to_owned()),
        };
        let tag = Tag { shot, generation };
        let outcome = recorder.trigger(
            stance::Trigger::new(at_ns, source, tag)
                .with_impact_lead(lead)
                .with_pre_roll(cut.pre_roll)
                .with_post_roll(cut.post_roll),
        );
        debug!("camera: {source} shot {} -> {outcome:?}", key.shot_number);
    }

    /// Take a frame, dated as it is to be recorded.
    fn push(&mut self, frame: Frame) -> Vec<RecorderEvent<Tag>> {
        self.recorder.push(frame)
    }

    /// The clips finished at host time `now_ns` for a camera `latency`
    /// behind, on [`frame_clock`].
    fn poll(&mut self, now_ns: u64, latency: Duration) -> Vec<RecorderEvent<Tag>> {
        self.recorder.poll(frame_clock(now_ns, latency))
    }

    /// Frames stopped: finish the open clips with what arrived.
    fn flush(&mut self) -> Vec<RecorderEvent<Tag>> {
        self.recorder.flush()
    }
}

/// Whether a camera running on `running` has to be reopened for `now`: its
/// camera or its video mode changed, or its section is gone. The card name,
/// the camera's own name, the position, the latency and the rotation are
/// read live (the camera's name only matters when its id is not connected,
/// at the next open).
#[must_use]
pub fn restart_needed(running: &CameraSection, now: Option<&CameraSection>) -> bool {
    !now.is_some_and(|s| s.camera == running.camera && s.mode == running.mode)
}

/// What a recorder trigger carries into its clip: the shot, and the clip
/// store's generation when it triggered (a Clear history since removes it).
#[derive(Debug, Clone)]
pub(crate) struct Tag {
    pub(crate) shot: ClipShot,
    pub(crate) generation: u64,
}

/// A failed attempt to open the camera.
enum OpenError {
    /// No camera support on this platform: the camera waits to be stopped.
    Unsupported,
    Failed(String),
}

/// Open the source `section` names: the test picture, or a camera by id,
/// falling back to the camera with its name when the id is not connected.
/// The camera's name is read from the live config. Runs on the capture
/// thread.
fn open_source(
    cameras: &Cameras,
    actor: &str,
    section: &CameraSection,
) -> stance::Result<Box<dyn Source>> {
    let request: FormatRequest = section.mode.parse()?;
    #[cfg(debug_assertions)]
    if let Some(path) = section.camera.strip_prefix(crate::clip_source::PREFIX) {
        return Ok(Box::new(crate::clip_source::LoopingClip::open(
            std::path::Path::new(path),
        )?));
    }
    if section.camera == CameraSection::TEST_PATTERN {
        return Ok(Box::new(TestPattern::new(TestPattern::config_for(
            &request,
        ))?));
    }
    match stance::open(&section.camera, &request) {
        Err(stance::Error::NotFound(id)) => {
            let index = actor.strip_prefix("camera.").unwrap_or("");
            let name = cameras
                .settings()
                .read()
                .camera
                .get(index)
                .map_or_else(|| section.camera_name.clone(), |c| c.camera_name.clone());
            let Some(name) = name.as_deref().filter(|n| !n.is_empty()) else {
                return Err(stance::Error::NotFound(id));
            };
            let found = stance::list()?
                .into_iter()
                .find(|c| c.name == name)
                .ok_or(stance::Error::NotFound(id))?;
            warn!(
                "camera '{}' is not connected; using the camera named '{name}' ({})",
                section.camera, found.id
            );
            Ok(Box::new(stance::open(&found.id, &request)?))
        }
        result => Ok(Box::new(result?)),
    }
}

/// The camera's display name, for alerts and logs: its configured
/// name, else its ID.
fn display_name(config: &FlighthookConfig, actor: &str) -> String {
    let index = actor.strip_prefix("camera.").unwrap_or("");
    config
        .camera
        .get(index)
        .map(|c| c.name.as_str())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(actor)
        .to_owned()
}

/// One `[camera.N]`: the thread body of its camera.
pub struct Camera;

impl Camera {
    /// Record `actor`'s camera (`section`) until `poll` says stop: open it,
    /// feed its frames to the recorder, cut a clip at every trigger `poll`
    /// hands over, and reopen the camera with a growing backoff when it is
    /// lost. `out` is the camera's own events (status, alerts, clips saved
    /// and removed), `raw` those of its clip jobs (body tracking of a clip is
    /// announced even when this camera has been restarted since). `poll` is
    /// asked at least every 10 ms while the camera captures, every 100 ms
    /// while the loop waits to reopen it, every 250 ms on a platform without
    /// cameras and not at all while the camera opens; it returns what is
    /// waiting, one input per call.
    pub fn run(
        cameras: &Arc<Cameras>,
        actor: &str,
        section: &CameraSection,
        out: Out,
        raw: Out,
        mut poll: impl FnMut() -> Input,
    ) {
        let recording = Recording::new(&cameras.settings().read());
        Run {
            recording,
            cameras,
            actor,
            out,
            raw,
            poll: &mut poll,
        }
        .run(section);
    }
}

/// What one [`Camera::run`] holds: the host's seams and the recording that
/// outlives its capture sessions.
struct Run<'a> {
    cameras: &'a Arc<Cameras>,
    actor: &'a str,
    out: Out,
    raw: Out,
    poll: &'a mut dyn FnMut() -> Input,
    recording: Recording,
}

impl Run<'_> {
    fn status(&self, status: ActorStatus, telemetry: HashMap<String, String>) {
        (self.out)(Event::Status { status, telemetry });
    }

    fn alert(&self, message: String) {
        (self.out)(Event::Alert(message));
    }

    fn run(&mut self, section: &CameraSection) {
        let actor = self.actor;
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(15));
        let mut ever_connected = false;
        self.status(ActorStatus::Starting, HashMap::new());

        loop {
            if !self.drain() {
                break;
            }
            let opened = self.open_capture(section);
            let session = match opened {
                Ok(capture) => {
                    ever_connected = true;
                    let started = Instant::now();
                    let ended = self.capture_session(&capture);
                    self.cameras.frames.set_open(actor, None);
                    capture.request_stop();
                    // Frames stopped: finish the open clips with what arrived.
                    for event in self.recording.flush() {
                        self.finished(event);
                    }
                    drop(capture);
                    match ended {
                        SessionEnd::Shutdown => break,
                        SessionEnd::Lost(error) => {
                            warn!("{actor}: camera lost: {error}");
                            let telemetry = HashMap::from([("error".to_owned(), error)]);
                            self.status(ActorStatus::Reconnecting, telemetry);
                        }
                    }
                    Some(started.elapsed())
                }
                Err(OpenError::Unsupported) => {
                    let telemetry = HashMap::from([("error".to_owned(), UNSUPPORTED.to_owned())]);
                    self.status(ActorStatus::Disconnected, telemetry);
                    // Nothing to retry: wait to be stopped, keeping up with the host.
                    loop {
                        match (self.poll)() {
                            Input::Stop => break,
                            Input::Idle => std::thread::sleep(Duration::from_millis(250)),
                            Input::Trigger(_) => {}
                        }
                    }
                    self.cameras.frames.remove(actor);
                    return;
                }
                Err(OpenError::Failed(error)) => {
                    warn!("{actor}: opening the camera failed: {error}");
                    let s = if ever_connected {
                        ActorStatus::Reconnecting
                    } else {
                        ActorStatus::Starting
                    };
                    self.status(s, HashMap::from([("error".to_owned(), error)]));
                    None
                }
            };
            let delay = backoff.delay(session);
            info!("{actor}: retrying in {}s", delay.as_secs());
            let deadline = Instant::now() + delay;
            while Instant::now() < deadline {
                if !self.drain() {
                    break;
                }
                for event in self
                    .recording
                    .poll(clock::now_ns(), latency(self.cameras, actor))
                {
                    self.finished(event);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        for event in self.recording.flush() {
            self.finished(event);
        }
        self.cameras.frames.remove(actor);
        self.status(ActorStatus::Disconnected, HashMap::new());
    }

    /// Start capturing from the camera `section` names. Blocks while it opens.
    fn open_capture(&self, section: &CameraSection) -> Result<Capture, OpenError> {
        let config = CaptureConfig {
            thread_name: format!("capture:{}", self.actor),
            ..CaptureConfig::default()
        };
        let source = section.clone();
        let cameras = Arc::clone(self.cameras);
        let actor = self.actor.to_owned();
        match Capture::start(config, move || open_source(&cameras, &actor, &source)) {
            Ok(capture) => Ok(capture),
            Err(stance::Error::Unsupported) => Err(OpenError::Unsupported),
            Err(e) => Err(OpenError::Failed(e.to_string())),
        }
    }
}

enum SessionEnd {
    Shutdown,
    Lost(String),
}

/// Telemetry that changes while capturing, as last sent.
#[derive(PartialEq)]
struct Live {
    fps: String,
    dropped: String,
    position: String,
}

/// The frame rate as reported: it moves only when the measured rate moved
/// [`FPS_REPORT_STEP`] of the mode's rate (at least 1 fps) away from the
/// value last reported.
#[derive(Debug, Default)]
struct FpsReport(Option<f64>);

impl FpsReport {
    /// The rate to report for a `measured` rate in a `nominal` fps mode.
    fn update(&mut self, measured: f64, nominal: f64) -> f64 {
        let step = (nominal * FPS_REPORT_STEP).max(1.0);
        match self.0 {
            Some(shown) if (measured - shown).abs() < step => shown,
            _ => {
                self.0 = Some(measured);
                measured
            }
        }
    }
}

/// One capture session, fed the time: when the still is due, the frames'
/// dating ([`Restamp`]), the telemetry last sent and the slow-camera
/// warning. It never reads a clock.
struct Pass {
    restamp: Restamp,
    /// When the still was last put.
    last_still: Option<Instant>,
    /// When the telemetry was last due.
    last_telemetry: Instant,
    /// The telemetry last sent.
    reported: Option<Live>,
    fps: FpsReport,
    /// Since when the camera has been slow.
    slow_since: Option<Instant>,
    /// The slow-camera alert went out.
    warned_slow: bool,
    /// When the session began.
    started: Instant,
}

impl Pass {
    /// A session begun at `now`.
    fn new(now: Instant) -> Self {
        Self {
            restamp: Restamp::default(),
            last_still: None,
            last_telemetry: now,
            reported: None,
            fps: FpsReport::default(),
            slow_since: None,
            warned_slow: false,
            started: now,
        }
    }

    /// A frame read at `now`, dated `latency` before it arrived: its
    /// picture when the still is due ([`STILL_EVERY`]), and the frame for
    /// the recorder, `None` when its date is not after the last one's.
    fn frame(
        &mut self,
        frame: Frame,
        latency: Duration,
        now: Instant,
    ) -> (Option<Arc<[u8]>>, Option<Frame>) {
        let still = if self
            .last_still
            .is_none_or(|t| now.duration_since(t) >= STILL_EVERY)
        {
            self.last_still = Some(now);
            Some(Arc::clone(&frame.jpeg))
        } else {
            None
        };
        (still, self.restamp.apply(frame, latency))
    }

    /// Whether the telemetry is due at `now`: [`TELEMETRY_EVERY`] since it
    /// last was.
    fn telemetry_due(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_telemetry) < TELEMETRY_EVERY {
            return false;
        }
        self.last_telemetry = now;
        true
    }

    /// At `now`, with the capture's `stats` in a `nominal` fps mode and the
    /// camera at `position`: the card rows to send, when they changed since
    /// last sent, and the measured rate to warn about, once a session, when
    /// the camera has delivered under half its rate for [`SLOW_FOR`].
    fn tick(
        &mut self,
        now: Instant,
        stats: &CaptureStats,
        nominal: f64,
        position: String,
    ) -> (Option<HashMap<String, String>>, Option<f64>) {
        let live = Live {
            fps: format!("{:.0}", self.fps.update(stats.fps, nominal)),
            dropped: stats.dropped.to_string(),
            position,
        };
        let mut rows = None;
        if self.reported.as_ref() != Some(&live) {
            rows = Some(HashMap::from([
                ("fps".to_owned(), live.fps.clone()),
                ("dropped".to_owned(), live.dropped.clone()),
                ("position".to_owned(), live.position.clone()),
            ]));
            self.reported = Some(live);
        }
        // The rate is measured over the last second of frames.
        let slow = nominal > 0.0
            && now.duration_since(self.started) >= TELEMETRY_EVERY
            && stats.fps < nominal / 2.0;
        let mut warn = None;
        match (slow, self.slow_since) {
            (false, _) => self.slow_since = None,
            (true, None) => self.slow_since = Some(now),
            (true, Some(since)) if now.duration_since(since) >= SLOW_FOR && !self.warned_slow => {
                self.warned_slow = true;
                warn = Some(stats.fps);
            }
            (true, Some(_)) => {}
        }
        (rows, warn)
    }
}

impl Run<'_> {
    /// Run one capture: frames into the recorder, triggers from the host,
    /// finished clips out, telemetry and the still. Returns why it ended.
    fn capture_session(&mut self, capture: &Capture) -> SessionEnd {
        let cameras = self.cameras;
        let actor = self.actor;
        let index = actor.strip_prefix("camera.").unwrap_or("").to_owned();
        let info = capture.info().clone();
        let format = capture.format().clone();
        let nominal = format.fps();
        cameras.frames.set_open(actor, Some(&info.id));
        self.recording.set_fps(nominal);

        let position = || {
            cameras
                .settings()
                .read()
                .camera
                .get(&index)
                .map(|c| c.position.label().to_owned())
                .unwrap_or_default()
        };
        want_models(cameras);
        info!("{actor}: capturing from '{}' in {format}", info.name);
        self.status(
            ActorStatus::Connected,
            HashMap::from([
                ("camera".to_owned(), info.name.clone()),
                ("position".to_owned(), position()),
                (
                    "resolution".to_owned(),
                    format!("{}x{}", format.width, format.height),
                ),
                ("mode_fps".to_owned(), format!("{nominal:.0}")),
                ("error".to_owned(), String::new()),
            ]),
        );

        let mut pass = Pass::new(Instant::now());
        let mut cursor = None;
        let mut live = LiveRoute::default();

        loop {
            // Triggers first, so a stamp lags its arrival by one short wait at
            // most.
            if !self.drain() {
                return SessionEnd::Shutdown;
            }
            let lag = latency(cameras, actor);
            // The newest frame of the pass, for the live slot.
            let mut newest = None;
            for frame in capture.wait_frames_after(cursor, FRAME_WAIT) {
                cursor = Some(frame.seq);
                newest = Some((frame_clock(frame.host_ns, lag), Arc::clone(&frame.jpeg)));
                let (still, frame) = pass.frame(frame, lag, Instant::now());
                if let Some(jpeg) = still {
                    cameras.frames.set_still(actor, jpeg);
                }
                let Some(frame) = frame else {
                    continue;
                };
                for event in self.recording.push(frame) {
                    self.finished(event);
                }
            }
            if let Some((time_ns, jpeg)) = newest {
                let now = Instant::now();
                let wanted = cameras.frames.live_wanted(actor, now);
                live.route(cameras, actor, wanted, now, time_ns, jpeg);
            }
            for event in self.recording.poll(clock::now_ns(), lag) {
                self.finished(event);
            }
            if let CaptureStatus::Failed(error) = capture.status() {
                return SessionEnd::Lost(error.to_string());
            }

            let now = Instant::now();
            if pass.telemetry_due(now) {
                let stats = capture.stats();
                let (rows, slow) = pass.tick(now, &stats, nominal, position());
                if let Some(rows) = rows {
                    self.status(ActorStatus::Connected, rows);
                }
                // Body tracking's models load once it is on and a camera runs.
                want_models(cameras);
                if let Some(fps) = slow {
                    let name = display_name(&cameras.settings().read(), actor);
                    self.alert(format!(
                        "Camera {name}: {fps:.0} fps of the {nominal:.0} requested. Another \
                         camera on the same USB controller, or a long exposure in low \
                         light, can slow it down."
                    ));
                }
            }
        }
    }

    /// Apply the host's waiting inputs: each trigger becomes a recorder
    /// trigger. Returns false once the camera is to stop.
    fn drain(&mut self) -> bool {
        loop {
            match (self.poll)() {
                Input::Stop => return false,
                Input::Idle => return true,
                Input::Trigger(shot) => {
                    // The strike's time when the trigger carries one, else
                    // its arrival.
                    let at = match shot.timestamp {
                        Some(_) => trigger_time(clock::now_ns(), shot.timestamp),
                        None => {
                            let at = clock::now_ns();
                            #[cfg(debug_assertions)]
                            let at = crate::screens::trigger_ns(at);
                            at
                        }
                    };
                    self.trigger(&shot, at);
                }
            }
        }
    }

    /// Apply trigger `shot`, dated `at_ns`, with the turn, the clip store's
    /// generation and the config as they are now.
    fn trigger(&mut self, shot: &Trigger, at_ns: u64) {
        let turn = self.cameras.turn();
        let generation = self.cameras.clips.generation();
        self.recording.trigger(
            &self.cameras.settings().read(),
            shot,
            at_ns,
            &turn,
            generation,
        );
    }

    /// Handle a clip leaving the recorder: a finished one is written and
    /// announced on a thread of its own; a discarded one (no impact frame) is
    /// only logged.
    fn finished(&self, event: RecorderEvent<Tag>) {
        match event {
            RecorderEvent::Discarded(d) => {
                info!(
                    "{}: no clip for {} shot(s): {:?}",
                    self.actor,
                    d.tags.len(),
                    d.reason
                );
            }
            RecorderEvent::Finished(clip) => {
                let cameras = Arc::clone(self.cameras);
                let actor = self.actor.to_owned();
                let (out, raw) = (Arc::clone(&self.out), Arc::clone(&self.raw));
                let spawned = std::thread::Builder::new()
                    .name(format!("clip:{actor}"))
                    .spawn(move || save(&cameras, &actor, &out, &raw, &clip));
                if let Err(e) = spawned {
                    warn!("camera: no thread to save a clip: {e}");
                }
            }
        }
    }
}

/// Milliseconds of a relative nanosecond time, rounded.
fn ms(ns: i64) -> i64 {
    // Clip times are far below 2^52 ns.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let ms = (ns as f64 / 1e6).round() as i64;
    ms
}

/// How much of the pre-roll `pre_roll` a clip whose first frame is at
/// `first_ns` and whose impact is at `impact_ns` misses, when more than
/// [`PRE_ROLL_SLACK`]: its trigger came later than the history reaches.
fn pre_roll_shortfall(first_ns: u64, impact_ns: u64, pre_roll: Duration) -> Option<Duration> {
    let wanted = impact_ns.saturating_sub(u64::try_from(pre_roll.as_nanos()).unwrap_or(u64::MAX));
    Some(Duration::from_nanos(first_ns.saturating_sub(wanted)))
        .filter(|short| *short > PRE_ROLL_SLACK)
}

/// How far a clip's first frame may lie after its wanted start (a frame
/// interval and the clock's jitter) without counting as short.
const PRE_ROLL_SLACK: Duration = Duration::from_millis(100);

/// Write a finished clip into the clip store and announce it: one
/// [`Event::Video`], and [`Event::VideoRemoved`] for the clips pruned to make
/// room, on `out`. A failed write (disk full) raises a warning alert. In
/// Live mode the clip is queued for body tracking, which announces on
/// `raw`.
pub(crate) fn save(
    cameras: &Arc<Cameras>,
    actor: &str,
    out: &Out,
    raw: &Out,
    clip: &stance::Clip<Tag>,
) {
    let replay = cameras.settings().replay();
    // Shots a Clear history removed since they triggered are left out; a
    // clip with none left is not written.
    let generation = cameras.clips.generation();
    let shots: Vec<ClipShot> = clip
        .tags
        .iter()
        .filter(|t| t.generation == generation)
        .map(|t| t.shot.clone())
        .collect();
    let hand = shots.first().and_then(|s| s.hand);
    let golfer = shots.first().and_then(|s| s.golfer.clone());
    if shots.is_empty() {
        info!("{actor}: history cleared since the swing, clip not saved");
        return;
    }
    if let Some(short) = clip
        .frames
        .first()
        .and_then(|f| pre_roll_shortfall(f.host_ns, clip.impact_ns, roll(replay.pre_roll_s)))
    {
        info!(
            "{actor}: shot #{}'s clip has {} ms less pre-roll than set: its trigger came \
             later than the camera's history reaches",
            shots[0].key.shot_number,
            short.as_millis()
        );
    }
    let slot = match cameras
        .clips
        .begin(&clips::clip_dir(&replay), &shots, generation)
    {
        Ok(slot) => slot,
        Err(e) => {
            warn!("{actor}: creating the clip folder: {e}");
            let name = display_name(&cameras.settings().read(), actor);
            out(Event::Alert(format!(
                "Camera {name}: couldn't save a clip: {e}"
            )));
            return;
        }
    };
    // History cleared since the swing: nothing to write.
    if !cameras.clips.is_current(&slot) {
        cameras.clips.abandon(&slot);
        return;
    }
    let files = match stance::write_clip(&slot.dir, actor, clip) {
        Ok(files) => files,
        Err(_) if !cameras.clips.is_current(&slot) => {
            // Cleared while writing: the folder went with it.
            cameras.clips.abandon(&slot);
            return;
        }
        Err(e) => {
            warn!("{actor}: writing the clip: {e}");
            cameras.clips.abandon(&slot);
            let name = display_name(&cameras.settings().read(), actor);
            out(Event::Alert(format!(
                "Camera {name}: couldn't save a clip: {e}"
            )));
            return;
        }
    };
    let sidecar = &files.sidecar;
    let index = actor.strip_prefix("camera.").unwrap_or("");
    let position = cameras
        .settings()
        .read()
        .camera
        .get(index)
        .map(|c| c.position)
        .unwrap_or_default();
    let duration = sidecar.frames.last().map_or(0, |f| f.t_ns) / 1_000_000;
    let lag = settings::latency(&cameras.settings().read(), actor);
    let video = ShotVideo {
        camera: actor.to_owned(),
        position,
        duration_ms: duration,
        fps: sidecar.fps.measured,
        trigger_offset_ms: ms(sidecar.trigger_ns),
        impact_offset_ms: ms(sidecar.impact_ns),
        complete: sidecar.complete,
        dropped: sidecar.dropped,
        pose: false,
        swing: false,
        rotation: 0,
        analyzed_with: None,
    };
    let done = cameras
        .clips
        .finish(&slot, video.clone(), clips::keep(&replay));
    if done.kept {
        let timing = shots
            .first()
            .map(|s| ClipTiming {
                device: s.device.clone(),
                impact_lead_ms: s.impact_lead_ms,
                latency_ms: u64::try_from(lag.as_millis()).unwrap_or(u64::MAX),
            })
            .unwrap_or_default();
        let keys: Vec<ShotKey> = shots.into_iter().map(|s| s.key).collect();
        info!(
            "{actor}: clip of {} shot(s), {} ms at {:.1} fps{}{} in {}",
            keys.len(),
            video.duration_ms,
            video.fps,
            if lag.is_zero() {
                String::new()
            } else {
                format!(", latency {} ms", lag.as_millis())
            },
            if video.complete { "" } else { ", incomplete" },
            slot.dir.display()
        );
        // Body tracking of the clip in Live mode, announced on `raw`: its
        // keypoints are files beside the clip, worth announcing even when
        // this camera has been restarted since.
        if cameras.settings().body_tracking().mode == BodyTrackingMode::Live {
            let job = ClipJob {
                avi: files.avi.clone(),
                slot: slot.clone(),
                camera: actor.to_owned(),
                keys: keys.clone(),
                position,
                hand,
                golfer,
                impact_ns: clip_impact(sidecar),
                written: Instant::now(),
                on_demand: false,
                timing,
                out: Arc::clone(raw),
            };
            cameras
                .pose
                .clips
                .push(cameras, job, crate::pose::clips::bound(cameras));
        }
        out(Event::Video {
            keys,
            position: video.position,
            duration_ms: video.duration_ms,
            fps: video.fps,
            trigger_offset_ms: video.trigger_offset_ms,
            impact_offset_ms: video.impact_offset_ms,
            complete: video.complete,
            dropped: video.dropped,
        });
    }
    if !done.removed.is_empty() {
        out(Event::VideoRemoved { keys: done.removed });
    }
}

#[cfg(test)]
mod bench;

#[cfg(test)]
mod tests;
