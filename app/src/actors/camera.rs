//! Camera actor (beta): records a clip of every swing from one camera.
//!
//! One actor per `[camera.N]`. It runs a stance [`Capture`] (the
//! camera on its own thread, frames into a bounded ring) and feeds every
//! frame to a [`ClipRecorder`], which keeps the pre-roll. Each launch
//! monitor's `shot_trigger` on the bus becomes a recorder trigger, stamped
//! when the actor reads it (the bus is read before the frames on every
//! pass, and a pass waits at most [`FRAME_WAIT`] for frames, so a stamp is
//! at most that late), with the impact estimated
//! [`impact_lead`] earlier: the monitor's own `impact_lead_s` setting, else
//! the default for that kind of monitor, both read live at every trigger.
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
//! (`SystemState::clips`), then announced with one `shot_video` event; no
//! frame goes on the bus. The newest frame is put in the actor's slot
//! (`SystemState::cameras`) at most twice a second, for the device card's
//! still, and, while a viewer of the Live view has asked within the last
//! few seconds, in its live slot at most [`LIVE_FPS`] times a second.
//!
//! With body tracking in Live mode (alpha, `crate::pose`), a watched live
//! view's frames go through the camera's body tracking worker, which puts
//! each in the live slot with the golfer's keypoints on it, and every saved
//! clip is queued for body tracking of the whole swing. In Historical mode
//! a clip is tracked when a viewer asks for it (`pose::clips::request`).
//!
//! Status follows the other device actors: `starting` while opening,
//! `connected` while frames arrive, `reconnecting` with the shared backoff
//! after the camera is lost (a clip that holds its impact frame is saved,
//! marked incomplete), `disconnected` when stopped or when the platform has
//! no camera support.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use stance::{
    Capture, CaptureConfig, CaptureStatus, ClipRecorder, FormatRequest, Frame, RecorderConfig,
    RecorderEvent, Source, TestPattern, Trigger, clock,
};
use tracing::{debug, info, warn};

use crate::actors::backoff::Backoff;
use crate::actors::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::pose::live::{self, LiveWorker};
use crate::pose::{clips::ClipJob, models};
use crate::state::SystemState;
use crate::state::clips::{self, ClipShot};
use flighthook::{
    ActorStatus, BodyTrackingMode, CameraSection, FlighthookEvent, FlighthookMessage,
    MAX_IMPACT_LEAD_S, MAX_LATENCY_S, ReplaySection, Severity, ShotKey, ShotVideo,
};

/// Recorder history beyond the pre-roll and the longest impact lead.
const BUFFER_MARGIN: Duration = Duration::from_millis(500);

/// Longest a capture pass waits for frames before it reads the bus again:
/// the most a trigger's stamp can lag its arrival.
const FRAME_WAIT: Duration = Duration::from_millis(10);

/// Least time between two telemetry updates.
const TELEMETRY_EVERY: Duration = Duration::from_secs(2);

/// How often the newest frame is put in the actor's still slot.
const STILL_EVERY: Duration = Duration::from_millis(500);

/// Most frames a second put in the actor's live slot while it is watched;
/// the frames in between are left out.
pub(crate) const LIVE_FPS: u32 = 15;

/// Least time between two live frames.
const LIVE_EVERY: Duration = Duration::from_nanos(1_000_000_000 / LIVE_FPS as u64);

/// How long the frame rate must stay under half the requested one before
/// the actor warns.
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
pub(crate) const UNSUPPORTED: &str = "cameras are not supported on macOS yet";

/// How long before its `shot_trigger` the launch monitor `actor` (e.g.
/// `mevo.0`) struck the ball: the `impact_lead_s` of its live config section,
/// else the default for its kind, or `None` for a source that is not a swing
/// (Auto-chip). These are beta estimates: FlightScope triggers at impact; the
/// others report once the shot is measured, which the clip's pre-roll has to
/// reach back over.
pub(crate) fn impact_lead(state: &SystemState, actor: &str) -> Option<Duration> {
    state.system.impact_lead_s(actor).map(lead)
}

/// A lead in seconds as a duration, clamped to the valid range.
fn lead(seconds: f64) -> Duration {
    let s = if seconds.is_finite() {
        seconds.clamp(0.0, MAX_IMPACT_LEAD_S)
    } else {
        0.0
    };
    Duration::from_secs_f64(s)
}

/// The latency in effect for camera `actor` (e.g. `camera.0`): its section's
/// `latency_s` when valid, else 0. Read live, so a change applies to the next
/// frame without reopening the camera.
pub(crate) fn latency(state: &SystemState, actor: &str) -> Duration {
    let s = state.system.camera_latency_s(actor);
    Duration::from_secs_f64(if s.is_finite() {
        s.clamp(0.0, MAX_LATENCY_S)
    } else {
        0.0
    })
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
    start: fn(&Arc<SystemState>, &str) -> Option<LiveWorker>,
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
        state: &Arc<SystemState>,
        actor: &str,
        wanted: bool,
        now: Instant,
        time_ns: u64,
        jpeg: Arc<[u8]>,
    ) {
        if wanted && state.system.body_tracking().mode == BodyTrackingMode::Live {
            if self.worker.as_ref().is_some_and(|w| !w.alive()) {
                self.worker = None;
                self.deaths += 1;
                if self.deaths > LIVE_RESTARTS {
                    warn!("{actor}: live body tracking stopped after repeated errors");
                    state.pose.set_live_failed(
                        actor,
                        Some("stopped on this camera after repeated errors".to_owned()),
                    );
                } else {
                    warn!("{actor}: live body tracking stopped on an error; restarting it");
                }
            }
            if self.worker.is_none() && self.deaths <= LIVE_RESTARTS {
                self.worker = (self.start)(state, actor);
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
                state.pose.set_live_failed(actor, None);
            }
        }
        if self.gate.admit(wanted, now) {
            state.cameras.set_live(actor, time_ns, jpeg, None);
        }
    }
}

/// Start camera `actor`'s live body tracking worker on the shared models,
/// following the live settings. `None` (logged) when its thread can't
/// start: frames then go out without a pose.
fn start_live_worker(state: &Arc<SystemState>, actor: &str) -> Option<LiveWorker> {
    let settings_state = Arc::clone(state);
    let settings_actor = actor.to_owned();
    let settings: live::Settings =
        Box::new(move || live_settings(&settings_state, &settings_actor));
    let build_state = Arc::clone(state);
    let build_actor = actor.to_owned();
    let build: live::Build = Box::new(move |s, rotation| {
        let shared = build_state.pose.models.want(s.model).ready()?;
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
    let publish_state = Arc::clone(state);
    let publish_actor = actor.to_owned();
    let publish: live::Publish = Box::new(move |time_ns, jpeg, pose| {
        publish_state
            .cameras
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
fn live_settings(state: &SystemState, actor: &str) -> live::LiveSettings {
    let index = actor.strip_prefix("camera.").unwrap_or("");
    let position = state
        .system
        .camera(index)
        .map(|c| c.position)
        .unwrap_or_default();
    live::LiveSettings {
        body: state.system.body_tracking(),
        posture: crate::pose::swing::live_setup(
            position,
            state.game.snapshot().handed,
            crate::pose::swing::mirrored(actor),
        ),
        rotation: stance::pose::Rotation::from_degrees(state.system.camera_rotation(actor))
            .unwrap_or_default(),
    }
}

/// Ask for body tracking's models in Live mode, so they are loaded before a
/// viewer or a clip needs them (never downloaded: missing files wait for
/// the user's download). In Historical mode the first clip viewed asks for
/// them.
fn want_models(state: &SystemState) {
    let settings = state.system.body_tracking();
    if settings.mode == BodyTrackingMode::Live {
        state.pose.models.want(settings.model);
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
/// pre-roll plus `longest_lead`, the longest impact lead in effect. Frames
/// are dated by capture time, so a camera's latency adds nothing to it.
fn recorder_config(
    replay: &ReplaySection,
    longest_lead: Duration,
    fps: Option<f64>,
) -> RecorderConfig {
    let pre_roll = roll(replay.pre_roll_s);
    RecorderConfig {
        pre_roll,
        post_roll: roll(replay.post_roll_s),
        buffer: pre_roll + longest_lead + BUFFER_MARGIN,
        fps,
        ..RecorderConfig::default()
    }
}

/// The longest impact lead in effect over the configured launch monitors.
fn longest_lead(state: &SystemState) -> Duration {
    lead(state.system.longest_impact_lead_s())
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

/// The shots the actor has seen triggered, to drop a repeated trigger for
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

/// Camera actor: the camera it records from and how.
pub struct CameraActor {
    pub section: CameraSection,
}

impl Actor for CameraActor {
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let section = self.section.clone();
        let thread_name = format!("device:{}", sender.actor_id());
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run(&state, &section, &sender, receiver))
            .expect("failed to spawn camera thread")
    }

    /// The camera and the video mode need it reopened; the card name, the
    /// camera's own name and the position are read live (the camera's name
    /// only matters when its id is not connected, at the next open).
    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let index = sender.actor_id().strip_prefix("camera.").unwrap_or("");
        match state.system.camera(index) {
            Some(s) if s.camera == self.section.camera && s.mode == self.section.mode => {
                ReconfigureOutcome::Applied
            }
            _ => ReconfigureOutcome::RestartRequired,
        }
    }
}

/// What a recorder trigger carries into its clip: the shot, and the clip
/// store's generation when it triggered (a Clear history since removes it).
#[derive(Debug, Clone)]
struct Tag {
    shot: ClipShot,
    generation: u64,
}

/// A failed attempt to open the camera.
enum OpenError {
    /// No camera support on this platform: the actor waits to be stopped.
    Unsupported,
    Failed(String),
}

/// Open the source `section` names: the test picture, or a camera by id,
/// falling back to the camera with its name when the id is not connected.
/// The camera's name is read from the live config. Runs on the capture
/// thread.
fn open_source(
    state: &SystemState,
    actor: &str,
    section: &CameraSection,
) -> stance::Result<Box<dyn Source>> {
    let request: FormatRequest = section.mode.parse()?;
    #[cfg(debug_assertions)]
    if let Some(path) = section.camera.strip_prefix(super::clip_source::PREFIX) {
        return Ok(Box::new(super::clip_source::LoopingClip::open(
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
            let name = state
                .system
                .camera(index)
                .map_or_else(|| section.camera_name.clone(), |c| c.camera_name);
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

fn status(sender: &BusSender, status: ActorStatus, telemetry: HashMap<String, String>) {
    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status,
        telemetry,
    }));
}

fn alert(sender: &BusSender, message: String) {
    sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
        severity: Severity::Warn,
        message,
    }));
}

/// The camera's display name, for alerts and logs: the actor's configured
/// name, else its ID.
fn display_name(state: &SystemState, actor: &str) -> String {
    let index = actor.strip_prefix("camera.").unwrap_or("");
    state
        .system
        .camera(index)
        .map(|c| c.name)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| actor.to_owned())
}

fn run(state: &Arc<SystemState>, section: &CameraSection, sender: &BusSender, mut rx: BusReceiver) {
    let actor = sender.actor_id().to_owned();
    let mut recorder: ClipRecorder<Tag> = ClipRecorder::new(recorder_config(
        &state.system.replay(),
        longest_lead(state),
        None,
    ));
    let mut seen = SeenTriggers::default();
    let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(15));
    let mut ever_connected = false;
    status(sender, ActorStatus::Starting, HashMap::new());

    loop {
        if rx.is_shutdown() {
            break;
        }
        let opened = open_capture(state, &actor, section);
        let session = match opened {
            Ok(capture) => {
                ever_connected = true;
                let started = Instant::now();
                let ended =
                    capture_session(state, sender, &mut rx, &capture, &mut recorder, &mut seen);
                state.cameras.set_open(&actor, None);
                capture.request_stop();
                // Frames stopped: finish the open clips with what arrived.
                for event in recorder.flush() {
                    finished(state, sender, event);
                }
                drop(capture);
                match ended {
                    SessionEnd::Shutdown => break,
                    SessionEnd::Lost(error) => {
                        warn!("{actor}: camera lost: {error}");
                        let telemetry = HashMap::from([("error".to_owned(), error)]);
                        status(sender, ActorStatus::Reconnecting, telemetry);
                    }
                }
                Some(started.elapsed())
            }
            Err(OpenError::Unsupported) => {
                let telemetry = HashMap::from([("error".to_owned(), UNSUPPORTED.to_owned())]);
                status(sender, ActorStatus::Disconnected, telemetry);
                // Nothing to retry: wait to be stopped, keeping up with the bus.
                while rx.poll().is_ok() {
                    while let Ok(Some(_)) = rx.poll() {}
                    std::thread::sleep(Duration::from_millis(250));
                }
                state.cameras.remove(&actor);
                return;
            }
            Err(OpenError::Failed(error)) => {
                warn!("{actor}: opening the camera failed: {error}");
                let s = if ever_connected {
                    ActorStatus::Reconnecting
                } else {
                    ActorStatus::Starting
                };
                status(sender, s, HashMap::from([("error".to_owned(), error)]));
                None
            }
        };
        let delay = backoff.delay(session);
        info!("{actor}: retrying in {}s", delay.as_secs());
        let deadline = Instant::now() + delay;
        while Instant::now() < deadline {
            if !drain_bus(state, &mut rx, &mut recorder, &mut seen) {
                break;
            }
            let now = frame_clock(clock::now_ns(), latency(state, &actor));
            for event in recorder.poll(now) {
                finished(state, sender, event);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    for event in recorder.flush() {
        finished(state, sender, event);
    }
    state.cameras.remove(&actor);
    status(sender, ActorStatus::Disconnected, HashMap::new());
}

/// Start capturing from the camera `section` names. Blocks while it opens.
fn open_capture(
    state: &Arc<SystemState>,
    actor: &str,
    section: &CameraSection,
) -> Result<Capture, OpenError> {
    let config = CaptureConfig {
        thread_name: format!("capture:{actor}"),
        ..CaptureConfig::default()
    };
    let source = section.clone();
    let state = Arc::clone(state);
    let actor_id = actor.to_owned();
    match Capture::start(config, move || open_source(&state, &actor_id, &source)) {
        Ok(capture) => Ok(capture),
        Err(stance::Error::Unsupported) => Err(OpenError::Unsupported),
        Err(e) => Err(OpenError::Failed(e.to_string())),
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

/// Run one capture: frames into the recorder, triggers from the bus,
/// finished clips out, telemetry and the still. Returns why it ended.
fn capture_session(
    state: &Arc<SystemState>,
    sender: &BusSender,
    rx: &mut BusReceiver,
    capture: &Capture,
    recorder: &mut ClipRecorder<Tag>,
    seen: &mut SeenTriggers,
) -> SessionEnd {
    let actor = sender.actor_id().to_owned();
    let index = actor.strip_prefix("camera.").unwrap_or("").to_owned();
    let info = capture.info().clone();
    let format = capture.format().clone();
    let nominal = format.fps();
    state.cameras.set_open(&actor, Some(&info.id));
    let mut config = recorder.config().clone();
    config.fps = (nominal > 0.0).then_some(nominal);
    recorder.set_config(config);

    let position = |state: &SystemState| {
        state
            .system
            .camera(&index)
            .map(|c| c.position.label().to_owned())
            .unwrap_or_default()
    };
    want_models(state);
    info!("{actor}: capturing from '{}' in {format}", info.name);
    status(
        sender,
        ActorStatus::Connected,
        HashMap::from([
            ("camera".to_owned(), info.name.clone()),
            ("position".to_owned(), position(state)),
            (
                "resolution".to_owned(),
                format!("{}x{}", format.width, format.height),
            ),
            ("mode_fps".to_owned(), format!("{nominal:.0}")),
            ("error".to_owned(), String::new()),
        ]),
    );

    let started = Instant::now();
    let mut cursor = None;
    let mut last_still: Option<Instant> = None;
    let mut live = LiveRoute::default();
    let mut last_telemetry = Instant::now();
    let mut reported: Option<Live> = None;
    let mut fps = FpsReport::default();
    let mut restamp = Restamp::default();
    let mut slow_since: Option<Instant> = None;
    let mut warned_slow = false;

    loop {
        // Triggers first, so a stamp lags its arrival by one short wait at
        // most.
        if !drain_bus(state, rx, recorder, seen) {
            return SessionEnd::Shutdown;
        }
        let lag = latency(state, &actor);
        // The newest frame of the pass, for the live slot.
        let mut newest = None;
        for frame in capture.wait_frames_after(cursor, FRAME_WAIT) {
            cursor = Some(frame.seq);
            if last_still.is_none_or(|t| t.elapsed() >= STILL_EVERY) {
                state.cameras.set_still(&actor, Arc::clone(&frame.jpeg));
                last_still = Some(Instant::now());
            }
            newest = Some((frame_clock(frame.host_ns, lag), Arc::clone(&frame.jpeg)));
            let Some(frame) = restamp.apply(frame, lag) else {
                continue;
            };
            for event in recorder.push(frame) {
                finished(state, sender, event);
            }
        }
        if let Some((time_ns, jpeg)) = newest {
            let now = Instant::now();
            let wanted = state.cameras.live_wanted(&actor, now);
            live.route(state, &actor, wanted, now, time_ns, jpeg);
        }
        for event in recorder.poll(frame_clock(clock::now_ns(), lag)) {
            finished(state, sender, event);
        }
        if let CaptureStatus::Failed(error) = capture.status() {
            return SessionEnd::Lost(error.to_string());
        }

        if last_telemetry.elapsed() >= TELEMETRY_EVERY {
            last_telemetry = Instant::now();
            let stats = capture.stats();
            let now = Live {
                fps: format!("{:.0}", fps.update(stats.fps, nominal)),
                dropped: stats.dropped.to_string(),
                position: position(state),
            };
            if reported.as_ref() != Some(&now) {
                status(
                    sender,
                    ActorStatus::Connected,
                    HashMap::from([
                        ("fps".to_owned(), now.fps.clone()),
                        ("dropped".to_owned(), now.dropped.clone()),
                        ("position".to_owned(), now.position.clone()),
                    ]),
                );
                reported = Some(now);
            }
            // Body tracking's models load once it is on and a camera runs.
            want_models(state);
            // The rate is measured over the last second of frames.
            let slow =
                nominal > 0.0 && started.elapsed() >= TELEMETRY_EVERY && stats.fps < nominal / 2.0;
            match (slow, slow_since) {
                (false, _) => slow_since = None,
                (true, None) => slow_since = Some(Instant::now()),
                (true, Some(since)) if since.elapsed() >= SLOW_FOR && !warned_slow => {
                    warned_slow = true;
                    alert(
                        sender,
                        format!(
                            "Camera {}: {:.0} fps of the {nominal:.0} requested. Another \
                             camera on the same USB controller, or a long exposure in low \
                             light, can slow it down.",
                            display_name(state, &actor),
                            stats.fps
                        ),
                    );
                }
                (true, Some(_)) => {}
            }
        }
    }
}

/// Apply the bus's waiting messages: each launch monitor trigger becomes a
/// recorder trigger. Returns false once the actor is to stop.
fn drain_bus(
    state: &SystemState,
    rx: &mut BusReceiver,
    recorder: &mut ClipRecorder<Tag>,
    seen: &mut SeenTriggers,
) -> bool {
    loop {
        match rx.poll() {
            Err(PollError::Shutdown) => return false,
            Ok(None) => return true,
            Ok(Some(msg)) => {
                if let FlighthookEvent::ShotTrigger { key } = &msg.event {
                    // Stamped on arrival: FRP triggers carry no time.
                    let at = clock::now_ns();
                    #[cfg(debug_assertions)]
                    let at = crate::screens::trigger_ns(at);
                    trigger(state, recorder, seen, &msg.actor, key, at);
                }
            }
        }
    }
}

/// Apply `source`'s trigger for shot `key`, read at `at_ns`. Auto-chip,
/// and repeats are ignored. The lead and the rolls come from the live config
/// (the source's `impact_lead_s`, the `[replay]` section).
fn trigger(
    state: &SystemState,
    recorder: &mut ClipRecorder<Tag>,
    seen: &mut SeenTriggers,
    source: &str,
    key: &ShotKey,
    at_ns: u64,
) {
    let Some(lead) = impact_lead(state, source) else {
        return;
    };
    if !seen.first(source, key) {
        debug!("camera: repeated trigger for {source} shot {}", key.shot_id);
        return;
    }
    let replay = state.system.replay();
    // The history covers this monitor's lead even if its section is gone.
    let mut config = recorder_config(
        &replay,
        longest_lead(state).max(lead),
        recorder.config().fps,
    );
    config.merge_window = recorder.config().merge_window;
    if config.buffer != recorder.config().buffer {
        recorder.set_config(config.clone());
    }
    let prefix = source.split_once('.').map_or(source, |(p, _)| p);
    let shot = ClipShot {
        key: key.clone(),
        device: crate::actors::web::device_kind(prefix).to_owned(),
        impact_lead_ms: u64::try_from(lead.as_millis()).unwrap_or(u64::MAX),
        hand: state.game.snapshot().handed,
    };
    let tag = Tag {
        shot,
        generation: state.clips.generation(),
    };
    let outcome = recorder.trigger(
        Trigger::new(at_ns, source, tag)
            .with_impact_lead(lead)
            .with_pre_roll(config.pre_roll)
            .with_post_roll(config.post_roll),
    );
    debug!("camera: {source} shot {} -> {outcome:?}", key.shot_number);
}

/// Handle a clip leaving the recorder: a finished one is written and
/// announced on a thread of its own; a discarded one (no impact frame) is
/// only logged.
fn finished(state: &Arc<SystemState>, sender: &BusSender, event: RecorderEvent<Tag>) {
    match event {
        RecorderEvent::Discarded(d) => {
            info!(
                "{}: no clip for {} shot(s): {:?}",
                sender.actor_id(),
                d.tags.len(),
                d.reason
            );
        }
        RecorderEvent::Finished(clip) => {
            let state = Arc::clone(state);
            let sender = sender.clone();
            let name = format!("clip:{}", sender.actor_id());
            let spawned = std::thread::Builder::new()
                .name(name)
                .spawn(move || save(&state, &sender, &clip));
            if let Err(e) = spawned {
                warn!("camera: no thread to save a clip: {e}");
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

/// A clip's estimated impact for its swing analysis, ns after its first
/// frame: `None` when it falls outside the clip's frames (before the first,
/// or after the last of a clip cut short).
pub(crate) fn clip_impact(sidecar: &stance::sidecar::Sidecar) -> Option<u64> {
    let last = sidecar.frames.last()?.t_ns;
    u64::try_from(sidecar.impact_ns).ok().filter(|t| *t <= last)
}

/// Write a finished clip into the clip store and announce it: one
/// `shot_video`, and `shot_video_removed` for the clips pruned to make
/// room. A failed write (disk full) raises a warning alert.
fn save(state: &Arc<SystemState>, sender: &BusSender, clip: &stance::Clip<Tag>) {
    let actor = sender.actor_id().to_owned();
    let replay = state.system.replay();
    // Shots a Clear history removed since they triggered are left out; a
    // clip with none left is not written.
    let generation = state.clips.generation();
    let shots: Vec<ClipShot> = clip
        .tags
        .iter()
        .filter(|t| t.generation == generation)
        .map(|t| t.shot.clone())
        .collect();
    let hand = shots.first().and_then(|s| s.hand);
    if shots.is_empty() {
        info!("{actor}: history cleared since the swing, clip not saved");
        return;
    }
    let slot = match state
        .clips
        .begin(&clips::clip_dir(&replay), &shots, generation)
    {
        Ok(slot) => slot,
        Err(e) => {
            warn!("{actor}: creating the clip folder: {e}");
            alert(
                sender,
                format!(
                    "Camera {}: couldn't save a clip: {e}",
                    display_name(state, &actor)
                ),
            );
            return;
        }
    };
    // History cleared since the swing: nothing to write.
    if !state.clips.is_current(&slot) {
        state.clips.abandon(&slot);
        return;
    }
    let files = match stance::write_clip(&slot.dir, &actor, clip) {
        Ok(files) => files,
        Err(_) if !state.clips.is_current(&slot) => {
            // Cleared while writing: the folder went with it.
            state.clips.abandon(&slot);
            return;
        }
        Err(e) => {
            warn!("{actor}: writing the clip: {e}");
            state.clips.abandon(&slot);
            alert(
                sender,
                format!(
                    "Camera {}: couldn't save a clip: {e}",
                    display_name(state, &actor)
                ),
            );
            return;
        }
    };
    let sidecar = &files.sidecar;
    let index = actor.strip_prefix("camera.").unwrap_or("");
    let position = state
        .system
        .camera(index)
        .map(|c| c.position)
        .unwrap_or_default();
    let duration = sidecar.frames.last().map_or(0, |f| f.t_ns) / 1_000_000;
    let lag = latency(state, &actor);
    let video = ShotVideo {
        camera: actor.clone(),
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
    let done = state
        .clips
        .finish(&slot, video.clone(), clips::keep(&replay));
    if done.kept {
        let timing = shots
            .first()
            .map(|s| crate::pose::clips::ClipTiming {
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
        // Body tracking of the clip in Live mode, announced on the bus
        // directly: its keypoints are files beside the clip, worth
        // announcing even when this camera has been restarted since.
        if state.system.body_tracking().mode == BodyTrackingMode::Live {
            let job = ClipJob {
                avi: files.avi.clone(),
                slot: slot.clone(),
                camera: actor.clone(),
                keys: keys.clone(),
                position,
                hand,
                impact_ns: clip_impact(sidecar),
                written: Instant::now(),
                on_demand: false,
                timing,
                bus: sender.raw_sender().clone(),
            };
            state
                .pose
                .clips
                .push(state, job, crate::pose::clips::bound(state));
        }
        sender.send(FlighthookMessage::new(FlighthookEvent::ShotVideo {
            keys,
            position: video.position,
            duration_ms: video.duration_ms,
            fps: video.fps,
            trigger_offset_ms: video.trigger_offset_ms,
            impact_offset_ms: video.impact_offset_ms,
            complete: video.complete,
            dropped: video.dropped,
        }));
    }
    if !done.removed.is_empty() {
        sender.send(FlighthookMessage::new(FlighthookEvent::ShotVideoRemoved {
            keys: done.removed,
        }));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::SocketAddr;
    use std::sync::atomic::AtomicBool;

    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::web::WebActor;
    use crate::actors::web::tests::TempDir;
    use crate::actors::{Actor, start_actor};
    use flighthook::{BallFlight, Velocity};

    /// A system state over a scratch config file holding `config`.
    fn state_with(config: &str) -> (TempDir, SystemState) {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-camera-lead-{}",
            uuid::Uuid::new_v4().simple()
        )));
        std::fs::create_dir_all(&dir.0).expect("dir");
        std::fs::write(dir.0.join("config.toml"), config).expect("config");
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        (dir, state)
    }

    /// A config in Live mode.
    const LIVE: &str = "[body_tracking]\nmode = \"live\"\n";

    fn ms(state: &SystemState, actor: &str) -> Option<u128> {
        impact_lead(state, actor).map(|d| d.as_millis())
    }

    #[test]
    fn impact_leads_default_by_device_kind() {
        let (_dir, state) = state_with("");
        assert_eq!(ms(&state, "mevo.0"), Some(0));
        assert_eq!(ms(&state, "mock_monitor.3"), Some(0));
        assert_eq!(ms(&state, "square.0"), Some(1000));
        assert_eq!(ms(&state, "exputt.0"), Some(1500));
        assert_eq!(ms(&state, "r10.1"), Some(2000));
        assert_eq!(ms(&state, "openconnect_server.0"), Some(2500));
        // A launch monitor kind without a better-known value.
        assert_eq!(ms(&state, "newmonitor.0"), Some(1500));
        assert_eq!(ms(&state, flighthook::AUTO_CHIP_ACTOR), None);
        for actor in ["mevo.0", "square.0", "r10.0", "openconnect_server.0"] {
            assert!(
                impact_lead(&state, actor).unwrap() <= lead(MAX_IMPACT_LEAD_S),
                "{actor}"
            );
        }
    }

    #[test]
    fn a_configured_impact_lead_overrides_the_default() {
        let (_dir, state) = state_with(
            "[mevo.0]\nname = \"Mevo\"\nimpact_lead_s = 0.25\n\n\
             [square.0]\nname = \"Omni\"\nimpact_lead_s = 0\n\n\
             [square.1]\nname = \"Omni 2\"\n\n\
             [exputt.0]\nname = \"Putt\"\nimpact_lead_s = 5\n",
        );
        assert_eq!(ms(&state, "mevo.0"), Some(250));
        // 0 is a value, not "unset".
        assert_eq!(ms(&state, "square.0"), Some(0));
        // No value: the device default.
        assert_eq!(ms(&state, "square.1"), Some(1000));
        assert_eq!(ms(&state, "exputt.0"), Some(5000));
        // No section: the default of its kind.
        assert_eq!(ms(&state, "mevo.7"), Some(0));
        assert_eq!(ms(&state, "r10.0"), Some(2000));
        // Auto-chip is not a swing, whatever a section says.
        assert_eq!(ms(&state, flighthook::AUTO_CHIP_ACTOR), None);
    }

    #[test]
    fn an_invalid_impact_lead_in_the_config_falls_back_to_the_default() {
        let (_dir, state) = state_with("[square.0]\nname = \"Omni\"\nimpact_lead_s = 9\n");
        assert_eq!(ms(&state, "square.0"), Some(1000));
        state.system.update(|c| {
            c.square.get_mut("0").unwrap().impact_lead_s = Some(f64::NAN);
        });
        assert_eq!(ms(&state, "square.0"), Some(1000));
        state.system.update(|c| {
            c.square.get_mut("0").unwrap().impact_lead_s = Some(-0.1);
        });
        assert_eq!(ms(&state, "square.0"), Some(1000));
    }

    #[test]
    fn the_impact_lead_is_read_live_at_every_trigger() {
        let (_dir, state) = state_with("[mevo.0]\nname = \"Mevo\"\n");
        let mut recorder: ClipRecorder<Tag> = ClipRecorder::new(recorder_config(
            &state.system.replay(),
            longest_lead(&state),
            None,
        ));
        let mut seen = SeenTriggers::default();
        let key = |n: u32| ShotKey {
            shot_id: format!("s{n}"),
            shot_number: n,
        };
        assert_eq!(ms(&state, "mevo.0"), Some(0));
        trigger(&state, &mut recorder, &mut seen, "mevo.0", &key(1), 1);
        state.system.update(|c| {
            c.mevo.get_mut("0").unwrap().impact_lead_s = Some(0.4);
        });
        assert_eq!(ms(&state, "mevo.0"), Some(400));
        trigger(&state, &mut recorder, &mut seen, "mevo.0", &key(2), 2);
        state.system.update(|c| {
            c.mevo.get_mut("0").unwrap().impact_lead_s = None;
        });
        assert_eq!(ms(&state, "mevo.0"), Some(0));
    }

    #[test]
    fn the_recorder_keeps_the_pre_roll_and_the_longest_lead() {
        let replay = ReplaySection {
            pre_roll_s: 4.0,
            post_roll_s: 1.5,
            ..ReplaySection::default()
        };
        let longest = Duration::from_millis(2500);
        let config = recorder_config(&replay, longest, Some(120.0));
        assert_eq!(config.pre_roll, Duration::from_secs(4));
        assert_eq!(config.post_roll, Duration::from_millis(1500));
        assert_eq!(config.buffer, Duration::from_millis(4000 + 2500 + 500));
        assert!(config.buffer_covers(longest));
        assert_eq!(config.fps, Some(120.0));
        // Out-of-range rolls from a hand-edited file are clamped.
        let wild = ReplaySection {
            pre_roll_s: f64::NAN,
            post_roll_s: 99.0,
            ..ReplaySection::default()
        };
        let config = recorder_config(&wild, longest, None);
        assert_eq!(config.pre_roll, Duration::ZERO);
        assert_eq!(config.post_roll, Duration::from_secs(10));
    }

    #[test]
    fn the_recorder_history_covers_the_largest_lead_in_effect() {
        let (_dir, state) =
            state_with("[mevo.0]\nname = \"Mevo\"\n\n[square.0]\nname = \"Omni\"\n");
        // Defaults only: Square's 1 s is the longest.
        assert_eq!(longest_lead(&state), Duration::from_secs(1));
        state.system.update(|c| {
            c.mevo.get_mut("0").unwrap().impact_lead_s = Some(3.5);
        });
        assert_eq!(longest_lead(&state), Duration::from_millis(3500));
        // A setting above the maximum is not in effect.
        state.system.update(|c| {
            c.mevo.get_mut("0").unwrap().impact_lead_s = Some(8.0);
        });
        assert_eq!(longest_lead(&state), Duration::from_secs(1));
        // With no monitor configured there is no lead to cover.
        let (_dir2, empty) = state_with("");
        assert_eq!(longest_lead(&empty), Duration::ZERO);

        // The recorder grows its history when a setting grows, at the next
        // trigger, and never lets a lead outrun it.
        let replay = state.system.replay();
        let mut recorder: ClipRecorder<Tag> =
            ClipRecorder::new(recorder_config(&replay, longest_lead(&state), None));
        let before = recorder.config().buffer;
        state.system.update(|c| {
            c.square.get_mut("0").unwrap().impact_lead_s = Some(5.0);
        });
        let mut seen = SeenTriggers::default();
        let key = ShotKey {
            shot_id: "a".into(),
            shot_number: 1,
        };
        trigger(&state, &mut recorder, &mut seen, "square.0", &key, 1);
        assert!(recorder.config().buffer > before);
        assert!(recorder.config().buffer_covers(Duration::from_secs(5)));
        assert_eq!(
            recorder.config().buffer,
            roll(replay.pre_roll_s) + Duration::from_secs(5) + BUFFER_MARGIN
        );
    }

    const MS: u64 = 1_000_000;

    fn millis(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn the_latency_in_effect_is_read_live_and_clamped() {
        let (_dir, state) = state_with(
            "[camera.0]\nname = \"Net\"\ncamera = \"test-pattern\"\nlatency_s = 1.5\n\n\
             [camera.1]\nname = \"USB\"\ncamera = \"test-pattern\"\n\n\
             [camera.2]\nname = \"Bad\"\ncamera = \"test-pattern\"\nlatency_s = 11\n",
        );
        assert_eq!(latency(&state, "camera.0"), millis(1500));
        // Absent is 0, and 0 is a value.
        assert_eq!(latency(&state, "camera.1"), Duration::ZERO);
        // Out of range from a hand-edited file: not in effect.
        assert_eq!(latency(&state, "camera.2"), Duration::ZERO);
        assert_eq!(latency(&state, "camera.9"), Duration::ZERO);
        state.system.update(|c| {
            c.camera.get_mut("1").unwrap().latency_s = Some(10.0);
            c.camera.get_mut("0").unwrap().latency_s = Some(f64::NAN);
        });
        assert_eq!(latency(&state, "camera.1"), millis(10_000));
        assert_eq!(latency(&state, "camera.0"), Duration::ZERO);
    }

    fn at(host_ns: u64, device_ns: Option<u64>) -> Frame {
        Frame {
            jpeg: Arc::from(vec![0u8; 8]),
            host_ns,
            device_ns,
            seq: host_ns / (10 * MS),
            width: 8,
            height: 8,
        }
    }

    #[test]
    fn a_frame_is_dated_its_latency_before_it_arrived() {
        let mut restamp = Restamp::default();
        let f = restamp
            .apply(at(5000 * MS, Some(4990 * MS)), millis(1500))
            .expect("frame");
        assert_eq!(f.host_ns, 3500 * MS);
        // The device time moves with it, so the two stay apart as before.
        assert_eq!(f.device_ns, Some(3490 * MS));
        // The rest of the frame is untouched.
        assert_eq!((f.seq, f.width, f.height), (500, 8, 8));
        assert_eq!(f.jpeg.len(), 8);
    }

    #[test]
    fn no_latency_leaves_every_frame_as_it_arrived() {
        let mut restamp = Restamp::default();
        for i in 1..=5u64 {
            let f = at(i * 10 * MS, None);
            assert_eq!(restamp.apply(f.clone(), Duration::ZERO), Some(f));
        }
    }

    #[test]
    fn a_date_before_the_clocks_zero_is_zero() {
        let mut restamp = Restamp::default();
        let f = restamp
            .apply(at(300 * MS, Some(200 * MS)), millis(2000))
            .expect("first");
        assert_eq!((f.host_ns, f.device_ns), (0, Some(0)));
        // The next one is not after it: dropped until the dating moves on.
        assert!(restamp.apply(at(310 * MS, None), millis(2000)).is_none());
        let f = restamp
            .apply(at(2010 * MS, None), millis(2000))
            .expect("next");
        assert_eq!(f.host_ns, 10 * MS);
    }

    #[test]
    fn dated_frames_stay_strictly_increasing_when_the_latency_changes() {
        let mut restamp = Restamp::default();
        let mut passed = Vec::new();
        let mut feed = |arrival_ms: u64, latency_ms: u64| {
            if let Some(f) = restamp.apply(at(arrival_ms * MS, None), millis(latency_ms)) {
                passed.push(f.host_ns / MS);
            }
        };
        // 100 fps at latency 0.
        for i in 1..=10 {
            feed(i * 10, 0);
        }
        // Raised to 50 ms: the next four frames (dated 60..90 ms) are not
        // newer than the 100 ms already passed on.
        for i in 11..=14 {
            feed(i * 10, 50);
        }
        for i in 15..=17 {
            feed(i * 10, 50);
        }
        // Lowered to 0: the dating jumps forward, nothing is dropped.
        for i in 18..=20 {
            feed(i * 10, 0);
        }
        assert_eq!(
            passed,
            [
                10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 180, 190, 200
            ]
        );
        assert!(passed.windows(2).all(|w| w[0] < w[1]));
    }

    /// Recorder settings of the simulations below: a 200 ms pre-roll and a
    /// 100 ms post-roll at 100 fps, finished 300 ms after the window.
    fn sim_config() -> RecorderConfig {
        RecorderConfig {
            pre_roll: millis(200),
            post_roll: millis(100),
            merge_window: millis(500),
            buffer: millis(5000),
            max_bytes: usize::MAX,
            stall: millis(300),
            fps: Some(100.0),
        }
    }

    /// A camera `latency_ms` behind real time, shooting 100 fps, and a launch
    /// monitor that triggers at impact at real time 2500 ms. Frame `seq`
    /// shows real time `seq * 10` ms and arrives `latency_ms` later, stamped
    /// with its arrival. The wall clock runs in 10 ms steps: the trigger,
    /// then the frame that arrives, then the recorder's poll. Returns what
    /// the recorder produced; `shifted` polls on [`frame_clock`] as the
    /// actor does, else on the plain wall clock.
    fn simulate(latency_ms: u64, shifted: bool) -> Vec<RecorderEvent<&'static str>> {
        let lag = millis(latency_ms);
        let mut recorder = ClipRecorder::new(sim_config());
        let mut restamp = Restamp::default();
        let mut events = Vec::new();
        for wall_ms in (0..=6000).step_by(10) {
            let wall = wall_ms * MS;
            if wall_ms == 2500 {
                recorder.trigger(Trigger::new(wall, "mock_monitor.0", "shot"));
            }
            if wall_ms >= latency_ms {
                let arrival = Frame {
                    seq: (wall_ms - latency_ms) / 10,
                    ..at(wall, None)
                };
                if let Some(frame) = restamp.apply(arrival, lag) {
                    events.extend(recorder.push(frame));
                }
            }
            let now = if shifted {
                frame_clock(wall, lag)
            } else {
                wall
            };
            events.extend(recorder.poll(now));
        }
        events
    }

    fn clip_of(events: Vec<RecorderEvent<&'static str>>) -> stance::Clip<&'static str> {
        let mut clips = events.into_iter().map(|e| match e {
            RecorderEvent::Finished(c) => c,
            RecorderEvent::Discarded(d) => panic!("discarded: {d:?}"),
        });
        let clip = clips.next().expect("a clip");
        assert!(clips.next().is_none(), "one clip");
        clip
    }

    #[test]
    fn a_clip_with_latency_covers_the_real_window() {
        let unlatencied = clip_of(simulate(0, true));
        // The window is [impact - 200 ms, trigger + 100 ms] = 2300..=2600 ms.
        assert_eq!(unlatencied.frames.len(), 31);
        assert_eq!(unlatencied.frames[0].host_ns, 2300 * MS);
        assert_eq!(unlatencied.frames[30].host_ns, 2600 * MS);
        assert!(unlatencied.complete);
        for latency_ms in [100, 500, 1500, 2000] {
            let clip = clip_of(simulate(latency_ms, true));
            assert!(clip.complete, "{latency_ms} ms");
            assert_eq!(clip.dropped, 0, "{latency_ms} ms");
            assert!(!clip.short_buffer, "{latency_ms} ms");
            // The same frames of the swing, at the same times.
            let times = |c: &stance::Clip<&str>| -> Vec<(u64, u64)> {
                c.frames.iter().map(|f| (f.seq, f.host_ns)).collect()
            };
            assert_eq!(times(&clip), times(&unlatencied), "{latency_ms} ms");
            // The impact mark sits on the frame that shows the impact, and
            // the trigger and impact times are the same as without latency.
            assert_eq!(clip.impact_frame().expect("impact").seq, 250);
            assert_eq!(clip.impact_index, unlatencied.impact_index);
            assert_eq!(clip.trigger_index, unlatencied.trigger_index);
            assert_eq!(clip.trigger_ns, unlatencied.trigger_ns);
            assert_eq!(clip.impact_ns, unlatencied.impact_ns);
        }
    }

    #[test]
    fn the_shifted_clock_keeps_a_clip_open_for_late_frames() {
        // On the plain wall clock the recorder gives up on the window 300 ms
        // after it ended (3 s of real time here, before any frame of it has
        // arrived) and the clip has no impact frame.
        let events = simulate(2000, false);
        assert!(
            matches!(events.as_slice(), [RecorderEvent::Discarded(_)]),
            "{events:?}"
        );
        // A latency under the stall time survives either way, only
        // incomplete.
        let clip = clip_of(simulate(200, false));
        assert_eq!(clip.impact_frame().expect("impact").seq, 250);
        // On the shifted clock a camera 2 s behind still gets its clip.
        assert!(clip_of(simulate(2000, true)).complete);
    }

    #[test]
    fn two_cameras_with_different_latency_cut_aligned_clips() {
        let direct = clip_of(simulate(0, true));
        let buffered = clip_of(simulate(1800, true));
        // What the sidecars would hold: times relative to the first frame.
        let relative = |c: &stance::Clip<&str>| {
            let first = c.frames[0].host_ns;
            (
                c.frames
                    .iter()
                    .map(|f| f.host_ns - first)
                    .collect::<Vec<_>>(),
                c.trigger_ns - first,
                c.impact_ns - first,
                c.trigger_index,
                c.impact_index,
            )
        };
        assert_eq!(relative(&direct), relative(&buffered));
        assert_eq!(direct.frames[0].host_ns, buffered.frames[0].host_ns);
        // Frame for frame, the same moment of the swing.
        for (a, b) in direct.frames.iter().zip(&buffered.frames) {
            assert_eq!(a.seq, b.seq);
        }
    }

    #[test]
    fn the_history_covers_the_pre_roll_whatever_the_latency() {
        // The ring spans the dated frames, which stop `latency` short of the
        // trigger, so it needs no more than pre-roll plus lead; the config
        // does not take the latency.
        let replay = ReplaySection::default();
        let config = recorder_config(&replay, millis(1500), None);
        assert_eq!(
            config.buffer,
            roll(replay.pre_roll_s) + millis(1500) + BUFFER_MARGIN
        );
        // A 10 s latency camera, impact lead 1.5 s, default pre-roll: the
        // window start is in the ring when the trigger arrives.
        let mut recorder: ClipRecorder<&'static str> = ClipRecorder::new(RecorderConfig {
            pre_roll: millis(500),
            post_roll: millis(100),
            buffer: millis(500) + millis(1500) + BUFFER_MARGIN,
            fps: Some(100.0),
            ..RecorderConfig::default()
        });
        let mut restamp = Restamp::default();
        let lag = millis(10_000);
        let mut clips = Vec::new();
        // Real time 0..30 s; frames arrive 10 s late.
        for seq in 0..3000u64 {
            let wall = seq * 10 * MS + 10_000 * MS;
            if wall == 22_000 * MS {
                // Trigger at real 22 s, impact 1.5 s earlier.
                recorder
                    .trigger(Trigger::new(wall, "square.0", "shot").with_impact_lead(millis(1500)));
            }
            if let Some(f) = restamp.apply(
                Frame {
                    seq,
                    ..at(wall, None)
                },
                lag,
            ) {
                clips.extend(recorder.push(f));
            }
        }
        let clip = clip_of(clips);
        assert!(clip.complete);
        assert!(!clip.short_buffer);
        // Window: impact 20.5 s - 0.5 s ..= 22.1 s.
        assert_eq!(clip.frames[0].host_ns, 20_000 * MS);
        assert_eq!(clip.impact_frame().expect("impact").seq, 2050);
    }

    /// Frames admitted by a watched [`LiveGate`] from a camera at `fps`
    /// over `secs` seconds.
    fn admitted(fps: f64, secs: f64) -> usize {
        let t0 = Instant::now();
        let mut gate = LiveGate::default();
        let frames = (fps * secs) as u32;
        (0..frames)
            .filter(|n| gate.admit(true, t0 + Duration::from_secs_f64(f64::from(*n) / fps)))
            .count()
    }

    #[test]
    fn the_live_slot_takes_at_most_live_fps_frames_a_second() {
        let fps = LIVE_FPS as usize;
        // Faster cameras are thinned to the live rate, a 30 fps one too
        // (its frames sit right on the schedule).
        for camera in [30.0, 31.0, 60.0, 120.0, 240.0] {
            let n = admitted(camera, 10.0);
            assert!(
                (fps * 10 - 1..=fps * 10 + 1).contains(&n),
                "{camera} fps: {n}"
            );
        }
        // A slower one keeps every frame.
        assert_eq!(admitted(10.0, 10.0), 100);
        assert_eq!(admitted(15.0, 10.0), 150);
    }

    #[test]
    fn the_live_slot_takes_nothing_unwatched_and_starts_afresh() {
        let t0 = Instant::now();
        let mut gate = LiveGate::default();
        assert!(!gate.admit(false, t0));
        assert!(gate.admit(true, t0 + millis(10)));
        assert!(!gate.admit(true, t0 + millis(20)), "within one interval");
        assert!(!gate.admit(false, t0 + millis(200)));
        // A new viewer gets the next frame at once.
        assert!(gate.admit(true, t0 + millis(210)));
        // A stall doesn't bank frames: one, then the rate again.
        assert!(gate.admit(true, t0 + millis(2000)));
        assert!(!gate.admit(true, t0 + millis(2010)));
        assert!(gate.admit(true, t0 + millis(2000) + LIVE_EVERY));
    }

    #[test]
    fn the_reported_frame_rate_moves_only_on_a_real_change() {
        let mut fps = FpsReport::default();
        assert!((fps.update(119.6, 120.0) - 119.6).abs() < 1e-9);
        // Hovering within 2 % of 120 fps keeps the value shown.
        for measured in [120.4, 118.0, 121.5, 119.1] {
            assert!(
                (fps.update(measured, 120.0) - 119.6).abs() < 1e-9,
                "{measured}"
            );
        }
        assert!((fps.update(100.0, 120.0) - 100.0).abs() < 1e-9);
        // At least 1 fps for slow modes.
        let mut slow = FpsReport::default();
        slow.update(30.0, 30.0);
        assert!((slow.update(29.2, 30.0) - 30.0).abs() < 1e-9);
        assert!((slow.update(28.9, 30.0) - 28.9).abs() < 1e-9);
    }

    #[test]
    fn only_the_camera_and_the_mode_restart_the_actor() {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-camera-reconf-{}",
            uuid::Uuid::new_v4().simple()
        )));
        std::fs::create_dir_all(&dir.0).expect("dir");
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        let state = Arc::new(state);
        let section = CameraSection {
            name: "Face".into(),
            camera: "usb-1".into(),
            camera_name: Some("HD Pro Webcam C920".into()),
            ..CameraSection::default()
        };
        let actor = CameraActor {
            section: section.clone(),
        };
        let (bus, _rx) = broadcast::channel(16);
        let sender = BusSender::new("camera.0".into(), bus, Arc::new(AtomicBool::new(false)));
        let outcome = |change: &dyn Fn(&mut CameraSection)| {
            let mut s = section.clone();
            change(&mut s);
            state.system.update(|c| {
                c.camera.insert("0".into(), s);
            });
            actor.reconfigure(&state, &sender)
        };
        assert!(matches!(outcome(&|_| {}), ReconfigureOutcome::Applied));
        assert!(matches!(
            outcome(&|s| {
                s.name = "DTL".into();
                s.camera_name = Some("Other name".into());
                s.position = flighthook::CameraPosition::TargetLine;
            }),
            ReconfigureOutcome::Applied
        ));
        // The latency is read for every frame: no reopen.
        assert!(matches!(
            outcome(&|s| s.latency_s = Some(2.5)),
            ReconfigureOutcome::Applied
        ));
        assert!(matches!(
            outcome(&|s| s.latency_s = None),
            ReconfigureOutcome::Applied
        ));
        // So are the rotation, yaw and pitch.
        assert!(matches!(
            outcome(&|s| {
                s.rotation_deg = 90;
                s.yaw_deg = Some(7.0);
                s.pitch_deg = Some(-4.0);
            }),
            ReconfigureOutcome::Applied
        ));
        assert!(matches!(
            outcome(&|s| s.camera = "usb-2".into()),
            ReconfigureOutcome::RestartRequired
        ));
        assert!(matches!(
            outcome(&|s| s.mode = "640x480@30".into()),
            ReconfigureOutcome::RestartRequired
        ));
    }

    #[test]
    fn a_clip_of_shots_cleared_since_their_trigger_is_not_written() {
        let dir = TempDir(std::env::temp_dir().join(format!(
            "flighthook-camera-save-{}",
            uuid::Uuid::new_v4().simple()
        )));
        std::fs::create_dir_all(&dir.0).expect("dir");
        let (state, _writer) = SystemState::new(dir.0.join("config.toml"));
        let state = Arc::new(state);
        let clips_dir = dir.0.join("clips");
        state
            .system
            .update(|c| c.replay.clip_dir = Some(clips_dir.display().to_string()));
        let (bus, mut rx) = broadcast::channel(16);
        let sender = BusSender::new("camera.0".into(), bus, Arc::new(AtomicBool::new(false)));

        // A 100 fps clip of one mock shot, triggered at generation `g`.
        let clip = |g: u64| {
            let mut pattern = TestPattern::new(stance::TestPatternConfig {
                width: 64,
                height: 48,
                fps: 100,
                paced: false,
                origin_ns: Some(0),
                ..stance::TestPatternConfig::default()
            })
            .expect("pattern");
            let mut recorder = ClipRecorder::new(RecorderConfig {
                pre_roll: Duration::from_millis(100),
                post_roll: Duration::from_millis(100),
                ..RecorderConfig::default()
            });
            let tag = Tag {
                shot: ClipShot {
                    key: ShotKey {
                        shot_id: format!("shot-{g}"),
                        shot_number: 1,
                    },
                    device: "mock".into(),
                    impact_lead_ms: 0,
                    hand: None,
                },
                generation: g,
            };
            for seq in 0..100 {
                if seq == 50 {
                    recorder.trigger(Trigger::new(500_000_000, "mock_monitor.0", tag.clone()));
                }
                let frame = pattern
                    .next_frame(Duration::ZERO)
                    .expect("frame")
                    .expect("frame");
                for event in recorder.push(frame) {
                    if let RecorderEvent::Finished(clip) = event {
                        return clip;
                    }
                }
            }
            panic!("no clip");
        };

        let before = clip(state.clips.generation());
        state.clips.clear();
        save(&state, &sender, &before);
        assert!(rx.try_recv().is_err(), "no shot_video");
        assert_eq!(std::fs::read_dir(&clips_dir).map_or(0, Iterator::count), 0);

        let after = clip(state.clips.generation());
        save(&state, &sender, &after);
        let msg = rx.try_recv().expect("shot_video");
        assert!(matches!(msg.event, FlighthookEvent::ShotVideo { .. }));
        assert_eq!(state.clips.videos("shot-1").len(), 1);
    }

    #[test]
    fn a_repeated_trigger_is_seen_once() {
        let mut seen = SeenTriggers::default();
        let key = |id: &str| ShotKey {
            shot_id: id.into(),
            shot_number: 1,
        };
        assert!(seen.first("mevo.0", &key("a")));
        assert!(!seen.first("mevo.0", &key("a")));
        assert!(seen.first("r10.0", &key("a")));
        for i in 0..SEEN_TRIGGERS {
            assert!(seen.first("mevo.0", &key(&i.to_string())));
        }
        // Forgotten after 64 others.
        assert!(seen.first("mevo.0", &key("a")));
    }

    /// A running system, web server and test-pattern camera.
    struct Rig {
        dir: TempDir,
        state: Arc<SystemState>,
        bus: broadcast::Sender<FlighthookMessage>,
        rx: broadcast::Receiver<FlighthookMessage>,
        web: WebActor,
        addr: SocketAddr,
    }

    impl Rig {
        /// Start with `[replay] keep_shots = keep`, short rolls, and one
        /// 30 fps test-pattern camera, waiting until it captures and its
        /// pre-roll has filled.
        ///
        /// The waits are lower bounds or polls, so a slow machine only makes
        /// them longer: the pre-roll wait (the paced pattern keeps real
        /// time, skipping frames when it falls behind), and the 0.6 s
        /// post-roll, which a second trigger has to land in for the
        /// back-to-back case (sent 120 ms after the first).
        fn start(keep: u32) -> Self {
            Self::start_in(keep, BodyTrackingMode::default())
        }

        /// [`Rig::start`] with body tracking in `mode`.
        fn start_in(keep: u32, mode: BodyTrackingMode) -> Self {
            let dir = TempDir(std::env::temp_dir().join(format!(
                "flighthook-camera-test-{}",
                uuid::Uuid::new_v4().simple()
            )));
            std::fs::create_dir_all(&dir.0).expect("dir");
            let clips = dir.0.join("clips");
            let config = format!(
                "[replay]\nkeep_shots = {keep}\npre_roll_s = 0.4\npost_roll_s = 0.6\nclip_dir = {:?}\n\n\
                 [camera.0]\nname = \"Face\"\ncamera = \"test-pattern\"\nposition = \"golfer_front\"\nmode = \"160x120@30\"\n\n\
                 [body_tracking]\nmode = {:?}\n",
                clips.display().to_string(),
                mode.key()
            );
            std::fs::write(dir.0.join("config.toml"), config).expect("config");
            let (state, writer) = SystemState::new(dir.0.join("config.toml"));
            let state = Arc::new(state);
            let (bus, rx) = broadcast::channel(1024);
            crate::actors::system::start_for_test(
                writer,
                &state,
                &bus,
                Arc::new(AtomicBool::new(false)),
            );
            let addr = std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|l| l.local_addr())
                .expect("free port");
            let web = WebActor::new(addr);
            let web_sender = BusSender::new(
                "webserver.0".into(),
                bus.clone(),
                Arc::new(AtomicBool::new(false)),
            );
            web.start(
                Arc::clone(&state),
                web_sender.clone(),
                web_sender.subscribe(),
            );
            let section = state.system.camera("0").expect("camera.0");
            start_actor(
                "camera.0".into(),
                Box::new(CameraActor { section }),
                &state,
                &bus,
            );
            let mut rig = Self {
                dir,
                state,
                bus,
                rx,
                web,
                addr,
            };
            rig.wait_for(Duration::from_secs(10), |m| {
                m.actor == "camera.0"
                    && matches!(
                        m.event,
                        FlighthookEvent::ActorStatus {
                            status: ActorStatus::Connected,
                            ..
                        }
                    )
            })
            .expect("camera connected");
            std::thread::sleep(Duration::from_millis(600));
            rig
        }

        fn clips_dir(&self) -> std::path::PathBuf {
            self.dir.0.join("clips")
        }

        /// Clip folders on disk.
        fn folders(&self) -> usize {
            std::fs::read_dir(self.clips_dir())
                .map(|d| d.flatten().filter(|e| e.path().is_dir()).count())
                .unwrap_or(0)
        }

        fn sender(&self, actor: &str) -> BusSender {
            BusSender::new(
                actor.into(),
                self.bus.clone(),
                Arc::new(AtomicBool::new(false)),
            )
        }

        /// A whole shot from `actor`: trigger, ball, finished.
        fn shot(&self, actor: &str, n: u32) -> ShotKey {
            let key = ShotKey {
                shot_id: uuid::Uuid::new_v4().to_string(),
                shot_number: n,
            };
            let monitor = self.sender(actor);
            monitor.send(FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                key: key.clone(),
            }));
            monitor.send(FlighthookMessage::new(FlighthookEvent::BallFlight {
                key: key.clone(),
                ball: Box::new(BallFlight {
                    launch_speed: Some(Velocity::MilesPerHour(100.0 + f64::from(n))),
                    ..BallFlight::default()
                }),
                derived: None,
            }));
            monitor.send(FlighthookMessage::new(FlighthookEvent::ShotFinished {
                key: key.clone(),
                raw: None,
            }));
            key
        }

        /// The next bus message matching `want` within `timeout`.
        fn wait_for(
            &mut self,
            timeout: Duration,
            mut want: impl FnMut(&FlighthookMessage) -> bool,
        ) -> Option<FlighthookMessage> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                match self.rx.try_recv() {
                    Ok(m) if want(&m) => return Some(m),
                    Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            None
        }

        /// The next `shot_video`'s keys.
        fn next_video(&mut self) -> Option<(Vec<ShotKey>, bool)> {
            self.wait_for(Duration::from_secs(10), |m| {
                matches!(m.event, FlighthookEvent::ShotVideo { .. })
            })
            .map(|m| match m.event {
                FlighthookEvent::ShotVideo { keys, complete, .. } => (keys, complete),
                _ => unreachable!(),
            })
        }

        /// GET `path` with `headers`: status code, headers (lower-case
        /// names), body.
        fn get(
            &self,
            path: &str,
            headers: &[(&str, &str)],
        ) -> (u16, HashMap<String, String>, Vec<u8>) {
            self.call("GET", path, headers)
        }

        /// POST `path` with no body: status code and body.
        fn post(&self, path: &str) -> (u16, serde_json::Value) {
            let (code, _, body) = self.call("POST", path, &[("Content-Length", "0")]);
            (code, serde_json::from_slice(&body).unwrap_or_default())
        }

        /// `method` `path` with `headers`, as [`Rig::get`].
        fn call(
            &self,
            method: &str,
            path: &str,
            headers: &[(&str, &str)],
        ) -> (u16, HashMap<String, String>, Vec<u8>) {
            let mut stream = std::net::TcpStream::connect(self.addr).expect("connect");
            let mut req = format!(
                "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
                self.addr
            );
            for (k, v) in headers {
                req.push_str(&format!("{k}: {v}\r\n"));
            }
            req.push_str("\r\n");
            stream.write_all(req.as_bytes()).expect("request");
            let mut response = Vec::new();
            stream.read_to_end(&mut response).expect("response");
            let split = response
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .expect("head");
            let head = String::from_utf8_lossy(&response[..split]).to_string();
            let body = response[split + 4..].to_vec();
            let mut lines = head.lines();
            let code = lines
                .next()
                .and_then(|l| l.split(' ').nth(1))
                .and_then(|c| c.parse().ok())
                .expect("status");
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                .collect();
            (code, headers, body)
        }

        fn json(&self, path: &str) -> serde_json::Value {
            let (code, _, body) = self.get(path, &[]);
            assert_eq!(code, 200, "{path}");
            serde_json::from_slice(&body).expect("json")
        }

        /// `/api/shots`, waiting until it lists `n` shots.
        fn shots(&self, n: usize) -> Vec<serde_json::Value> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let rows = self
                    .json("/api/shots")
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if rows.len() >= n || Instant::now() > deadline {
                    return rows;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// The `/api/shots` row of `key`, once its video count is `videos`.
        fn row(&self, key: &ShotKey, videos: usize) -> serde_json::Value {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let row = self
                    .json("/api/shots")
                    .as_array()
                    .and_then(|rows| {
                        rows.iter()
                            .find(|r| r["shot_id"] == key.shot_id.as_str())
                            .cloned()
                    })
                    .unwrap_or_default();
                let n = row
                    .get("video")
                    .and_then(serde_json::Value::as_array)
                    .map_or(0, Vec::len);
                if n == videos || Instant::now() > deadline {
                    return row;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            for id in self.state.actor_ids() {
                self.state.stop_actor(&id);
            }
            self.web.stop();
            // The camera releases its capture before the folder goes.
            self.wait_for(Duration::from_secs(5), |m| {
                m.actor == "camera.0"
                    && matches!(
                        m.event,
                        FlighthookEvent::ActorStatus {
                            status: ActorStatus::Disconnected,
                            ..
                        }
                    )
            });
        }
    }

    #[test]
    fn a_mock_shot_records_a_clip_that_is_served_with_the_shot() {
        let mut rig = Rig::start(20);
        let key = rig.shot("mock_monitor.0", 1);
        let (keys, complete) = rig.next_video().expect("shot_video");
        assert_eq!(keys, std::slice::from_ref(&key));
        assert!(complete);
        assert_eq!(rig.folders(), 1);

        // On disk: the camera's AVI and sidecar, and flighthook's shot.json.
        let folder = std::fs::read_dir(rig.clips_dir())
            .expect("clips")
            .flatten()
            .next()
            .expect("folder")
            .path();
        for file in ["camera.0.avi", "camera.0.json", "shot.json"] {
            assert!(folder.join(file).is_file(), "{file}");
        }
        let shot_file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(folder.join("shot.json")).expect("read"))
                .expect("shot.json");
        assert_eq!(
            shot_file["shots"][0]["key"]["shot_id"],
            key.shot_id.as_str()
        );
        assert_eq!(shot_file["shots"][0]["device"], "mock");
        assert_eq!(shot_file["cameras"][0]["position"], "golfer_front");

        // The /api/shots row carries it.
        let row = rig.row(&key, 1);
        let video = &row["video"][0];
        assert_eq!(video["camera"], "camera.0");
        assert_eq!(video["position"], "golfer_front");
        assert_eq!(video["complete"], true);
        assert!(video["fps"].as_f64().unwrap() > 20.0, "{video}");
        // 0.4 s pre-roll + 0.6 s post-roll, the trigger 0.4 s in.
        let duration = video["duration_ms"].as_u64().unwrap();
        assert!((850..=1100).contains(&duration), "{video}");
        let trigger = video["trigger_offset_ms"].as_i64().unwrap();
        assert!((300..=500).contains(&trigger), "{video}");
        assert_eq!(video["impact_offset_ms"], video["trigger_offset_ms"]);

        // The clip list and its files.
        let id = &key.shot_id;
        let list = rig.json(&format!("/api/shots/{id}/video"));
        assert_eq!(list.as_array().map(Vec::len), Some(1));
        assert_eq!(list[0]["camera"], "camera.0");
        let (code, headers, body) = rig.get(&format!("/api/shots/{id}/video/camera.0.json"), &[]);
        assert_eq!(code, 200);
        assert!(headers["content-type"].contains("json"), "{headers:?}");
        assert_eq!(headers["cache-control"], "private, no-cache");
        let sidecar =
            stance::sidecar::Sidecar::from_json(std::str::from_utf8(&body).expect("utf-8"))
                .expect("sidecar");
        let frame = sidecar.impact_frame().expect("impact frame");

        // The AVI in byte ranges: its header, then one frame.
        let avi = format!("/api/shots/{id}/video/camera.0.avi");
        let (code, headers, body) = rig.get(&avi, &[("Range", "bytes=0-11")]);
        assert_eq!(code, 206);
        assert_eq!(body.len(), 12);
        assert_eq!(&body[..4], b"RIFF");
        assert_eq!(&body[8..12], b"AVI ");
        assert!(
            headers["content-range"].starts_with("bytes 0-11/"),
            "{headers:?}"
        );
        let range = format!("bytes={}-{}", frame.offset, frame.end() - 1);
        let (code, _, body) = rig.get(&avi, &[("Range", &range)]);
        assert_eq!(code, 206);
        assert_eq!(body.len(), frame.len as usize);
        assert_eq!(&body[..2], &[0xFF, 0xD8]);
        assert_eq!(headers["cache-control"], "private, no-cache");
        let (code, headers, body) = rig.get(&avi, &[]);
        assert_eq!(code, 200);
        assert_eq!(&body[..4], b"RIFF");
        assert_eq!(headers["cache-control"], "private, no-cache");
        for missing in [
            format!("/api/shots/{id}/video/camera.1.avi"),
            format!("/api/shots/{id}/video/camera.0.txt"),
            format!("/api/shots/{id}/video/..%2Fshot.json"),
            "/api/shots/nope/video/camera.0.avi".to_owned(),
        ] {
            assert_eq!(rig.get(&missing, &[]).0, 404, "{missing}");
        }

        // The still: the newest frame, never cached.
        let (code, headers, body) = rig.get("/api/cameras/camera.0/still", &[]);
        assert_eq!(code, 200);
        assert_eq!(headers["content-type"], "image/jpeg");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(&body[..2], &[0xFF, 0xD8]);
        assert_eq!(rig.get("/api/cameras/camera.9/still", &[]).0, 404);

        // The status says where clips go. (`/api/cameras` is left out: it
        // enumerates the machine's real cameras.)
        let status = rig.json("/api/status");
        assert_eq!(
            status["clip_dir"],
            rig.clips_dir().display().to_string().as_str()
        );

        // Never in the export.
        let (code, _, body) = rig.get("/api/shots/export?format=jsonl", &[]);
        assert_eq!(code, 200);
        let text = String::from_utf8(body).expect("utf-8");
        assert!(text.contains(&key.shot_id), "{text}");
        assert!(!text.contains("\"video\""), "{text}");
        assert!(!text.contains("camera.0"), "{text}");
    }

    #[test]
    fn live_frames_flow_to_a_viewer_at_the_live_rate() {
        let rig = Rig::start(20);
        assert!(
            !rig.state.cameras.live_wanted("camera.0", Instant::now()),
            "nobody watches yet"
        );
        let mut after = 0;
        let mut frames = 0;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(1) {
            let (code, headers, body) =
                rig.get(&format!("/api/cameras/camera.0/live?after={after}"), &[]);
            assert_eq!(code, 200, "a 30 fps camera has a frame within the wait");
            assert_eq!(headers["content-type"], "image/jpeg");
            assert_eq!(headers["cache-control"], "no-store");
            assert_eq!(&body[..2], &[0xFF, 0xD8]);
            let seq: u64 = headers["x-frame-seq"].parse().expect("seq");
            assert!(seq > after);
            let time_ns: u64 = headers["x-frame-time-ns"].parse().expect("time");
            assert!(time_ns > 0 && time_ns <= clock::now_ns());
            // Historical mode: the picture alone.
            assert!(!headers.contains_key("x-pose"), "{headers:?}");
            assert_eq!(headers["x-pose-state"], "off");
            after = seq;
            frames += 1;
        }
        assert!(rig.state.cameras.live_wanted("camera.0", Instant::now()));
        let most = LIVE_FPS as usize + 2;
        assert!((3..=most).contains(&frames), "{frames} frames in a second");
        assert_eq!(rig.get("/api/cameras/camera.9/live", &[]).0, 404);
    }

    #[test]
    fn a_latency_saved_while_recording_applies_without_reopening_the_camera() {
        let mut rig = Rig::start(20);
        let section = CameraSection {
            latency_s: Some(0.3),
            ..rig.state.system.camera("0").expect("camera.0")
        };
        rig.sender("web")
            .send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
                request_id: Some("latency".into()),
                action: Box::new(flighthook::ConfigAction::UpsertCamera {
                    index: "0".into(),
                    section,
                }),
            }));
        let mut statuses = Vec::new();
        let outcome = rig
            .wait_for(Duration::from_secs(5), |m| {
                if m.actor == "camera.0"
                    && let FlighthookEvent::ActorStatus { status, .. } = &m.event
                {
                    statuses.push(*status);
                }
                matches!(
                    &m.event,
                    FlighthookEvent::ConfigOutcome { request_id: Some(r), .. } if r == "latency"
                )
            })
            .expect("outcome");
        let FlighthookEvent::ConfigOutcome {
            restarted, stopped, ..
        } = outcome.event
        else {
            unreachable!()
        };
        assert!(restarted.is_empty() && stopped.is_empty(), "{restarted:?}");
        assert!(
            !statuses.contains(&ActorStatus::Starting),
            "the camera was not reopened: {statuses:?}"
        );
        assert_eq!(latency(&rig.state, "camera.0"), millis(300));
        // The next shot's clip is made from frames dated 0.3 s earlier, and
        // still waits for the frames of its window.
        let key = rig.shot("mock_monitor.0", 1);
        let (keys, complete) = rig.next_video().expect("shot_video");
        assert_eq!(keys, [key]);
        assert!(complete);
    }

    #[test]
    fn two_monitors_seeing_one_swing_share_one_clip() {
        let mut rig = Rig::start(20);
        // Back to back on the bus: well inside the 500 ms merge window.
        let a = rig.shot("mock_monitor.0", 1);
        let b = rig.shot("mock_monitor.1", 1);
        let (keys, _) = rig.next_video().expect("shot_video");
        assert_eq!(keys, [a.clone(), b.clone()]);
        assert!(
            rig.wait_for(Duration::from_millis(800), |m| matches!(
                m.event,
                FlighthookEvent::ShotVideo { .. }
            ))
            .is_none(),
            "one clip"
        );
        assert_eq!(rig.folders(), 1);
        assert_eq!(rig.shots(2).len(), 2);
        for key in [&a, &b] {
            assert_eq!(rig.row(key, 1)["video"][0]["camera"], "camera.0");
        }
    }

    #[test]
    fn back_to_back_shots_from_one_monitor_make_two_clips() {
        let mut rig = Rig::start(20);
        let a = rig.shot("mock_monitor.0", 1);
        // Frames come between the two triggers (30 fps), and the second
        // lands inside the first's 0.6 s post-roll.
        std::thread::sleep(Duration::from_millis(120));
        let b = rig.shot("mock_monitor.0", 2);
        let first = rig.next_video().expect("first clip");
        let second = rig.next_video().expect("second clip");
        let mut got = [first.0, second.0];
        got.sort_by_key(|k| k[0].shot_number);
        assert_eq!(got, [vec![a.clone()], vec![b.clone()]]);
        assert_eq!(rig.folders(), 2);
        // The first was cut short at the second trigger.
        let sidecar = rig.json(&format!("/api/shots/{}/video/camera.0.json", a.shot_id));
        assert_eq!(sidecar["truncated"], true);
        let sidecar = rig.json(&format!("/api/shots/{}/video/camera.0.json", b.shot_id));
        assert_eq!(sidecar["truncated"], false);
    }

    #[test]
    fn an_auto_chip_records_nothing() {
        let mut rig = Rig::start(20);
        rig.shot(flighthook::AUTO_CHIP_ACTOR, 1);
        assert!(
            rig.wait_for(Duration::from_millis(1500), |m| matches!(
                m.event,
                FlighthookEvent::ShotVideo { .. }
            ))
            .is_none()
        );
        assert_eq!(rig.folders(), 0);
    }

    #[test]
    fn pruning_to_keep_shots_reports_the_removed_shots() {
        let mut rig = Rig::start(1);
        let a = rig.shot("mock_monitor.0", 1);
        rig.next_video().expect("first clip");
        let b = rig.shot("mock_monitor.0", 2);
        let removed = rig
            .wait_for(Duration::from_secs(10), |m| {
                matches!(m.event, FlighthookEvent::ShotVideoRemoved { .. })
            })
            .expect("shot_video_removed");
        let FlighthookEvent::ShotVideoRemoved { keys } = removed.event else {
            unreachable!()
        };
        assert_eq!(keys, std::slice::from_ref(&a));
        assert_eq!(rig.folders(), 1);
        assert!(rig.row(&a, 0).get("video").is_none());
        assert_eq!(rig.row(&b, 1)["video"][0]["camera"], "camera.0");
        assert!(rig.state.clips.videos(&a.shot_id).is_empty());
        assert_eq!(
            rig.get(&format!("/api/shots/{}/video/camera.0.avi", a.shot_id), &[])
                .0,
            404
        );
    }

    #[test]
    fn clear_history_deletes_the_clips() {
        let mut rig = Rig::start(20);
        let key = rig.shot("mock_monitor.0", 1);
        rig.next_video().expect("clip");
        assert_eq!(rig.folders(), 1);
        rig.sender("web")
            .send(FlighthookMessage::new(FlighthookEvent::ClearShotHistory));
        let deadline = Instant::now() + Duration::from_secs(5);
        while rig.folders() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(rig.folders(), 0);
        assert!(rig.state.clips.videos(&key.shot_id).is_empty());
        assert!(rig.shots(0).is_empty());
    }

    static BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// A live worker on a fake tracker, counting the trackers it makes.
    fn fake_worker(state: &Arc<SystemState>, actor: &str) -> Option<LiveWorker> {
        let settings_state = Arc::clone(state);
        let settings_actor = actor.to_owned();
        let settings: live::Settings =
            Box::new(move || live_settings(&settings_state, &settings_actor));
        let build: live::Build = Box::new(|s, _| {
            BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            Some(live::tests::fake_tracker(
                Duration::ZERO,
                &calls,
                true,
                s.detect_every,
            ))
        });
        let publish_state = Arc::clone(state);
        let publish_actor = actor.to_owned();
        let publish: live::Publish = Box::new(move |t, jpeg, pose| {
            publish_state
                .cameras
                .set_live(&publish_actor, t, jpeg, pose);
        });
        LiveWorker::start(actor, settings, build, publish).ok()
    }

    /// The live slot's frame once it is newer than `after`.
    fn live_after(state: &SystemState, after: u64) -> Arc<crate::state::cameras::LiveFrame> {
        for _ in 0..400 {
            if let Some(f) = state.cameras.live("camera.0").filter(|f| f.seq > after) {
                return f;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("no live frame after {after}");
    }

    #[test]
    fn a_watched_camera_goes_live_with_poses_and_a_lapsed_watch_resets_the_tracker() {
        use crate::pose::live::tests::numbered_jpeg;
        let (_dir, state) = state_with(LIVE);
        let state = Arc::new(state);
        let mut route = LiveRoute {
            start: fake_worker,
            ..LiveRoute::default()
        };
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // Watched, in Live mode: the frame goes out with its pose.
        route.route(&state, "camera.0", true, at(0), 11, numbered_jpeg(60));
        let frame = live_after(&state, 0);
        assert_eq!(frame.time_ns, 11);
        let pose: flighthook::LivePose =
            serde_json::from_str(frame.pose.as_deref().expect("a pose")).expect("json");
        assert_eq!(pose.keypoints.len(), 26);
        assert!((pose.keypoints[0][0] - 60.0).abs() <= 3.0, "{pose:?}");
        assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The watch lapses: the worker goes, and nothing is put.
        route.route(&state, "camera.0", false, at(100), 12, numbered_jpeg(1));
        assert!(route.worker.is_none());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            state.cameras.live("camera.0").map(|f| f.seq),
            Some(frame.seq)
        );
        // Watched again: a new tracker, nothing remembered.
        route.route(&state, "camera.0", true, at(200), 13, numbered_jpeg(120));
        let again = live_after(&state, frame.seq);
        assert_eq!(again.time_ns, 13);
        assert!(again.pose.is_some());
        assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 2);
        // Out of Live mode: frames go straight out, without a pose.
        let mut last = again.seq;
        for (n, mode) in [BodyTrackingMode::Historical, BodyTrackingMode::Off]
            .into_iter()
            .enumerate()
        {
            state.system.update(|c| c.body_tracking.mode = mode);
            let ms = 300 + 100 * n as u64;
            route.route(&state, "camera.0", true, at(ms), ms, numbered_jpeg(1));
            assert!(route.worker.is_none(), "{mode:?}");
            let plain = live_after(&state, last);
            assert_eq!(plain.time_ns, ms);
            assert!(plain.pose.is_none());
            last = plain.seq;
        }
        assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    static DYING_STARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// A live worker whose thread panics on its first frame.
    fn dying_worker(state: &Arc<SystemState>, actor: &str) -> Option<LiveWorker> {
        DYING_STARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let settings_state = Arc::clone(state);
        let settings_actor = actor.to_owned();
        let settings: live::Settings =
            Box::new(move || live_settings(&settings_state, &settings_actor));
        let build: live::Build = Box::new(|_, _| panic!("a broken model"));
        let publish: live::Publish = Box::new(|_, _, _| {});
        LiveWorker::start(actor, settings, build, publish).ok()
    }

    #[test]
    fn a_dead_live_worker_is_restarted_then_reported_as_failed() {
        use crate::pose::live::tests::numbered_jpeg;
        use std::sync::atomic::Ordering;
        let (_dir, state) = state_with(LIVE);
        let state = Arc::new(state);
        let mut route = LiveRoute {
            start: dying_worker,
            ..LiveRoute::default()
        };
        let settings = state.system.body_tracking();
        let t0 = Instant::now();
        let mut t = 0;
        let mut frame = |route: &mut LiveRoute, wanted: bool| {
            t += 1;
            route.route(
                &state,
                "camera.0",
                wanted,
                t0 + Duration::from_millis(t * 70),
                t,
                numbered_jpeg(1),
            );
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        // The first worker and three replacements die on their frame.
        for starts in 1..=4 {
            frame(&mut route, true);
            assert_eq!(DYING_STARTS.load(Ordering::SeqCst), starts);
            while route.worker.as_ref().is_some_and(LiveWorker::alive) {
                assert!(Instant::now() < deadline, "the worker did not end");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(state.pose.live_state(&settings, "camera.0").0, "loading");
        }
        // The fourth death: no more workers, frames go out without a pose,
        // and the live state says why.
        frame(&mut route, true);
        assert!(route.worker.is_none());
        assert_eq!(DYING_STARTS.load(Ordering::SeqCst), 4);
        let (code, error) = state.pose.live_state(&settings, "camera.0");
        assert_eq!(code, "failed");
        assert!(error.expect("why").contains("stopped"));
        let plain = state.cameras.live("camera.0").expect("a frame");
        assert!(plain.pose.is_none());
        frame(&mut route, true);
        assert_eq!(DYING_STARTS.load(Ordering::SeqCst), 4);
        // The watch lapses: forgotten, and the next viewer gets a worker.
        frame(&mut route, false);
        assert_eq!(state.pose.live_state(&settings, "camera.0").0, "loading");
        frame(&mut route, true);
        assert_eq!(DYING_STARTS.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn an_impact_outside_the_clip_is_no_prior() {
        use stance::sidecar::{Sidecar, SidecarFps, SidecarFrame};
        let frames = (0..10u64)
            .map(|i| SidecarFrame {
                t_ns: i * 100_000_000,
                offset: 0,
                len: 0,
            })
            .collect();
        let mut sidecar = Sidecar {
            version: stance::sidecar::SIDECAR_VERSION,
            fourcc: "MJPG".into(),
            width: 64,
            height: 48,
            fps: SidecarFps {
                nominal: Some(10.0),
                measured: 10.0,
            },
            start_ns: 0,
            frames,
            trigger_index: 9,
            impact_index: 5,
            trigger_ns: 900_000_000,
            impact_ns: 500_000_000,
            complete: true,
            dropped: 0,
            truncated: false,
        };
        assert_eq!(clip_impact(&sidecar), Some(500_000_000));
        sidecar.impact_ns = 900_000_000;
        assert_eq!(clip_impact(&sidecar), Some(900_000_000), "the last frame");
        // A clip cut short before the impact.
        sidecar.impact_ns = 950_000_000;
        assert_eq!(clip_impact(&sidecar), None);
        sidecar.impact_ns = -1;
        assert_eq!(clip_impact(&sidecar), None);
    }

    /// A clip pass without models: the golfer standing at address on every
    /// sidecar frame, so the swing analysis has a track to measure.
    fn fake_pass(
        avi: &std::path::Path,
        _config: &stance::pose::PoseConfig,
    ) -> stance::Result<stance::pose::PoseTrack> {
        use stance::pose::{PoseFrame, PoseTrack, Skeleton};
        let name = avi.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let (_, sidecar) = stance::open_clip(avi.parent().expect("folder"), name)?;
        let pose = crate::pose::swing::tests::golfer((300.0, 260.0));
        Ok(PoseTrack {
            version: stance::pose::POSE_TRACK_VERSION,
            skeleton: Skeleton::Halpe26,
            model: "fake".into(),
            detector: "fake".into(),
            width: sidecar.width,
            height: sidecar.height,
            rotation: stance::pose::Rotation::None,
            frames: sidecar
                .frames
                .iter()
                .map(|f| PoseFrame::with_pose(f.t_ns, &pose))
                .collect(),
        })
    }

    #[test]
    fn a_recorded_clips_keypoints_and_swing_are_served_beside_it_and_pruned_with_it() {
        let mut rig = Rig::start_in(1, BodyTrackingMode::Live);
        rig.state.pose.clips.fake(fake_pass);
        let a = rig.shot("mock_monitor.0", 1);
        rig.next_video().expect("shot_video");
        let done = rig
            .wait_for(Duration::from_secs(10), |m| {
                matches!(m.event, FlighthookEvent::ShotPose { .. })
            })
            .expect("shot_pose");
        assert_eq!(done.actor, "camera.0");
        let FlighthookEvent::ShotPose { keys, .. } = done.event else {
            unreachable!()
        };
        assert_eq!(keys, std::slice::from_ref(&a));
        // The row says so.
        let deadline = Instant::now() + Duration::from_secs(5);
        while rig.row(&a, 1)["video"][0]["pose"] != true {
            assert!(Instant::now() < deadline, "{}", rig.row(&a, 1));
            std::thread::sleep(Duration::from_millis(20));
        }
        // Served like the clip: whole, in ranges, never cached.
        let id = &a.shot_id;
        let path = format!("/api/shots/{id}/video/camera.0.pose.json");
        let (code, headers, body) = rig.get(&path, &[]);
        assert_eq!(code, 200);
        assert!(headers["content-type"].contains("json"), "{headers:?}");
        assert_eq!(headers["cache-control"], "private, no-cache");
        let track: flighthook::PoseTrackFile = serde_json::from_slice(&body).expect("pose json");
        let (_, _, sidecar) = rig.get(&format!("/api/shots/{id}/video/camera.0.json"), &[]);
        let sidecar =
            stance::sidecar::Sidecar::from_json(std::str::from_utf8(&sidecar).expect("utf-8"))
                .expect("sidecar");
        assert_eq!(track.frames.len(), sidecar.frames.len());
        let impact_t = sidecar.frames[sidecar.impact_index].t_ns;
        assert_eq!(track.keypoints_at(impact_t).map(<[_]>::len), Some(26));
        let (code, _, part) = rig.get(&path, &[("Range", "bytes=0-9")]);
        assert_eq!((code, part.len()), (206, 10));
        assert_eq!(
            rig.get(&format!("/api/shots/{id}/video/camera.1.pose.json"), &[])
                .0,
            404
        );
        // Then the swing analysis, served the same way.
        let swung = rig
            .wait_for(Duration::from_secs(10), |m| {
                matches!(m.event, FlighthookEvent::ShotSwing { .. })
            })
            .expect("shot_swing");
        assert_eq!(swung.actor, "camera.0");
        let deadline = Instant::now() + Duration::from_secs(5);
        while rig.row(&a, 1)["video"][0]["swing"] != true {
            assert!(Instant::now() < deadline, "{}", rig.row(&a, 1));
            std::thread::sleep(Duration::from_millis(20));
        }
        let swing_path = format!("/api/shots/{id}/video/camera.0.swing.json");
        let (code, headers, body) = rig.get(&swing_path, &[]);
        assert_eq!(code, 200);
        assert_eq!(headers["cache-control"], "private, no-cache");
        let report: flighthook::SwingReportFile =
            serde_json::from_slice(&body).expect("swing json");
        assert_eq!(report.view, Some(flighthook::SwingView::FaceOn));
        let (code, _, part) = rig.get(&swing_path, &[("Range", "bytes=0-9")]);
        assert_eq!((code, part.len()), (206, 10));
        assert_eq!(
            rig.get(&format!("/api/shots/{id}/video/camera.1.swing.json"), &[])
                .0,
            404
        );
        let swing_file = rig
            .state
            .clips
            .file(id, "camera.0", "swing.json")
            .expect("swing file");
        // The next clip prunes this one, keypoints and all.
        let file = rig
            .state
            .clips
            .file(id, "camera.0", "pose.json")
            .expect("file");
        assert!(file.is_file());
        rig.shot("mock_monitor.0", 2);
        rig.wait_for(Duration::from_secs(10), |m| {
            matches!(m.event, FlighthookEvent::ShotVideoRemoved { .. })
        })
        .expect("shot_video_removed");
        assert!(!file.exists());
        assert!(!swing_file.exists());
        assert_eq!(rig.get(&path, &[]).0, 404);
        assert_eq!(rig.get(&swing_path, &[]).0, 404);
    }

    #[test]
    fn historical_mode_tracks_a_clip_when_it_is_viewed_and_only_then() {
        let mut rig = Rig::start_in(20, BodyTrackingMode::Historical);
        rig.state.pose.clips.fake(fake_pass);
        let a = rig.shot("mock_monitor.0", 1);
        rig.next_video().expect("shot_video");
        // Nothing queued, no model asked for.
        assert!(rig.state.pose.clips.waiting().is_empty());
        assert!(!rig.state.pose.clips.working());
        let model = rig.state.system.body_tracking().model;
        assert!(rig.state.pose.models.state(model).is_none());
        let id = &a.shot_id;
        let analyze = format!("/api/shots/{id}/video/camera.0/analyze");
        assert_eq!(rig.row(&a, 1)["video"][0]["pose"], serde_json::Value::Null);
        let grades = format!("/api/shots/{id}/grades/camera.0");
        let (code, _, body) = rig.get(&grades, &[]);
        assert_eq!(code, 404, "no grades before the swing analysis");
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("camera.0"))
        );
        // Viewed: queued, then tracked at once (no 3 s wait) with its events.
        let asked = Instant::now();
        let (code, body) = rig.post(&analyze);
        assert_eq!(code, 202, "{body}");
        assert_eq!(body["state"], "queued");
        let done = rig
            .wait_for(Duration::from_secs(10), |m| {
                matches!(m.event, FlighthookEvent::ShotPose { .. })
            })
            .expect("shot_pose");
        assert!(
            asked.elapsed() < crate::pose::clips::CLIP_DELAY,
            "{:?}",
            asked.elapsed()
        );
        assert_eq!(done.actor, "camera.0");
        rig.wait_for(Duration::from_secs(10), |m| {
            matches!(m.event, FlighthookEvent::ShotSwing { .. })
        })
        .expect("shot_swing");
        let pose = format!("/api/shots/{id}/video/camera.0.pose.json");
        assert_eq!(rig.get(&pose, &[]).0, 200);
        assert_eq!(
            rig.get(&format!("/api/shots/{id}/video/camera.0.swing.json"), &[])
                .0,
            200
        );
        // Graded once analyzed: the mock monitor names no club, so nothing
        // is graded, and nothing is stored.
        let (code, _, body) = rig.get(&grades, &[]);
        assert_eq!(code, 200);
        let body: flighthook::BodyGrades = serde_json::from_slice(&body).expect("grades");
        assert_eq!(
            (body.shot_id.as_str(), body.camera.as_str()),
            (id.as_str(), "camera.0")
        );
        assert!(body.club_class.is_none() && body.references.is_empty());
        assert_eq!(
            rig.get(&format!("/api/shots/{id}/grades/camera.7"), &[]).0,
            404
        );
        // Viewed again: done, nothing queued.
        let (code, body) = rig.post(&analyze);
        assert_eq!((code, body["state"].as_str()), (200, Some("done")));
        assert!(rig.state.pose.clips.waiting().is_empty());
        // No such clip.
        let (code, body) = rig.post(&format!("/api/shots/{id}/video/camera.7/analyze"));
        assert_eq!(code, 404);
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("camera.7"))
        );
        assert_eq!(rig.post("/api/shots/nope/video/camera.0/analyze").0, 404);
        // A second shot asked for twice while it waits: queued once.
        let b = rig.shot("mock_monitor.0", 2);
        rig.next_video().expect("shot_video");
        let analyze_b = format!("/api/shots/{}/video/camera.0/analyze", b.shot_id);
        assert_eq!(rig.post(&analyze_b).0, 202);
        assert_eq!(rig.post(&analyze_b).0, 202);
        rig.wait_for(Duration::from_secs(10), |m| {
            matches!(m.event, FlighthookEvent::ShotPose { .. })
        })
        .expect("shot_pose");
        assert!(
            rig.wait_for(Duration::from_millis(500), |m| {
                matches!(m.event, FlighthookEvent::ShotPose { .. })
            })
            .is_none(),
            "tracked once"
        );
    }

    #[test]
    fn off_mode_records_and_replays_clips_without_tracking_them() {
        let mut rig = Rig::start_in(20, BodyTrackingMode::Off);
        rig.state.pose.clips.fake(fake_pass);
        let a = rig.shot("mock_monitor.0", 1);
        rig.next_video().expect("shot_video");
        let id = &a.shot_id;
        assert_eq!(
            rig.get(&format!("/api/shots/{id}/video/camera.0.avi"), &[])
                .0,
            200
        );
        let (code, body) = rig.post(&format!("/api/shots/{id}/video/camera.0/analyze"));
        assert_eq!(code, 409);
        assert_eq!(body["error"], "body tracking is off");
        assert!(rig.state.pose.clips.waiting().is_empty());
        assert!(!rig.state.pose.clips.working());
        let model = rig.state.system.body_tracking().model;
        assert!(
            rig.state.pose.models.state(model).is_none(),
            "no model asked for"
        );
        // The live view: the picture alone.
        let (code, headers, _) = rig.get("/api/cameras/camera.0/live?after=0", &[]);
        assert_eq!(code, 200);
        assert!(!headers.contains_key("x-pose"));
        assert_eq!(headers["x-pose-state"], "off");
        assert!(
            rig.wait_for(Duration::from_millis(3500), |m| {
                matches!(m.event, FlighthookEvent::ShotPose { .. })
            })
            .is_none()
        );
        assert!(rig.state.pose.models.state(model).is_none());
        assert!(
            !rig.dir
                .0
                .join("clips")
                .read_dir()
                .unwrap()
                .flatten()
                .any(|e| { e.path().join("camera.0.pose.json").exists() })
        );
    }

    #[test]
    fn no_mode_downloads_the_models_on_its_own() {
        for mode in [BodyTrackingMode::Historical, BodyTrackingMode::Live] {
            let mut rig = Rig::start_in(20, mode);
            let model = rig.state.system.body_tracking().model;
            let a = rig.shot("mock_monitor.0", 1);
            rig.next_video().expect("shot_video");
            // Viewed (and in Live, recorded): asked for, but the models are
            // missing, so the clip stays untracked.
            let analyze = format!("/api/shots/{}/video/camera.0/analyze", a.shot_id);
            assert_eq!(rig.post(&analyze).0, 202, "{mode:?}");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !matches!(
                rig.state.pose.models.state(model),
                Some(crate::pose::models::ModelState::Missing)
            ) {
                assert!(
                    Instant::now() < deadline,
                    "{mode:?}: the models are missing"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            let (code, headers, _) = rig.get("/api/cameras/camera.0/live?after=0", &[]);
            assert_eq!(code, 200);
            let live_state = if mode == BodyTrackingMode::Live {
                "missing"
            } else {
                "off"
            };
            assert_eq!(headers["x-pose-state"], live_state, "{mode:?}");
            assert!(
                rig.wait_for(Duration::from_millis(3500), |m| {
                    matches!(m.event, FlighthookEvent::ShotPose { .. })
                })
                .is_none(),
                "{mode:?}"
            );
            assert!(!rig.state.pose.models.downloading(), "{mode:?}");
            assert!(
                !rig.state.pose.models.dir().exists(),
                "{mode:?}: nothing written to the models folder"
            );
        }
    }

    #[test]
    fn switching_modes_at_runtime_follows_at_once_without_a_restart() {
        use flighthook::ConfigAction;
        let mut rig = Rig::start_in(20, BodyTrackingMode::Live);
        // Live asks for the models when the camera starts.
        let model = rig.state.system.body_tracking().model;
        let deadline = Instant::now() + Duration::from_secs(5);
        while rig.state.pose.models.state(model).is_none() {
            assert!(Instant::now() < deadline, "Live loads the models");
            std::thread::sleep(Duration::from_millis(20));
        }
        let save = |rig: &mut Rig, mode: BodyTrackingMode| {
            let mut section = rig.state.system.body_tracking();
            section.mode = mode;
            let request_id = uuid::Uuid::new_v4().to_string();
            rig.sender("web")
                .send(FlighthookMessage::new(FlighthookEvent::ConfigCommand {
                    request_id: Some(request_id.clone()),
                    action: Box::new(ConfigAction::SetBodyTracking { section }),
                }));
            let outcome = rig
                .wait_for(Duration::from_secs(5), |m| {
                    matches!(&m.event, FlighthookEvent::ConfigOutcome { request_id: Some(r), .. } if *r == request_id)
                })
                .expect("outcome");
            let FlighthookEvent::ConfigOutcome { restarted, .. } = outcome.event else {
                unreachable!()
            };
            assert!(restarted.is_empty(), "no camera restart");
        };
        // A recorded clip waits its 3 s in Live; leaving Live before its
        // turn leaves it untracked.
        rig.state.pose.clips.fake(fake_pass);
        rig.shot("mock_monitor.0", 1);
        rig.next_video().expect("shot_video");
        assert!(rig.state.pose.clips.working(), "queued for its pass");
        save(&mut rig, BodyTrackingMode::Historical);
        assert!(
            rig.wait_for(
                crate::pose::clips::CLIP_DELAY + Duration::from_secs(1),
                |m| { matches!(m.event, FlighthookEvent::ShotPose { .. }) }
            )
            .is_none(),
            "no automatic pass out of Live"
        );
        // Historical keeps the models for the clips viewed; Off lets them go
        // and drops what waits.
        assert!(rig.state.pose.models.state(model).is_some());
        save(&mut rig, BodyTrackingMode::Off);
        assert!(rig.state.pose.models.state(model).is_none());
        assert!(rig.state.pose.clips.waiting().is_empty());
        rig.shot("mock_monitor.0", 2);
        rig.next_video().expect("shot_video");
        assert!(rig.state.pose.clips.waiting().is_empty());
        // Back to Live: recorded clips are tracked again.
        save(&mut rig, BodyTrackingMode::Live);
        rig.shot("mock_monitor.0", 3);
        rig.next_video().expect("shot_video");
        rig.wait_for(Duration::from_secs(10), |m| {
            matches!(m.event, FlighthookEvent::ShotPose { .. })
        })
        .expect("Live tracks the clip");
    }
}
