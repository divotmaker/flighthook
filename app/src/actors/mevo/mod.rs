//! The FlightScope card (Mevo+, Mevo Gen2, Xi): ironsight's device driver
//! ([`Mevo`]) on [`DriverActor`].
//!
//! The driver owns the device: it connects over TCP, runs the handshake,
//! configures and arms the device for the detected model in the detection
//! mode in effect, follows mode changes (a reconfigure and re-arm when the
//! change reaches the device), applies the Fusion camera config after the
//! camera's warmup, and reports readiness, tilt, roll, battery, shots and
//! the frames of each shot. The ball goes out as soon as the device's full
//! result arrives. The Xi's spin source is applied live, without a new
//! session; any other setting change takes a new session (a restart).
//!
//! The card adds its section ([`MevoSection`]), its shot source and two
//! things of its own on each shot's launch:
//!
//! - **The mode at the trigger.** A shot is simulated in the detection mode
//!   the session ran in at its trigger ([`HookCtx::session_mode`]), not
//!   when its result arrives (seconds later), so a mode change during a
//!   shot doesn't change how it rolls. It is the mode the driver itself
//!   took for the shot (the Xi's `auto` spin source reads it), so the two
//!   agree even for a mode change landing in the trigger's `poll` pass.
//! - **The launch-only spin estimate** (beta). When the device reports a
//!   launch with speed and launch angle but no spin and no flight values
//!   (very short chips on the Xi), the card fills in a backspin of
//!   `launch_only_spin` rpm per mph of ball speed and no sidespin, so the
//!   simulation can fly it, and marks the shot's spin as estimated. Not for
//!   a shot whose mode at the trigger was Putting, a launch under
//!   [`LAUNCH_ONLY_MIN_LAUNCH_DEG`] (it rolls as a ground ball), or a
//!   `launch_only_spin` of 0.

use std::collections::VecDeque;

use flighthook::{Launch, MevoSection, ShotDetectionMode};
use flightrelay::FrpEvent;
use ironsight::driver::Mevo;
use ironsight::driver::convert::LAUNCH_ONLY_MIN_LAUNCH_DEG;
use tracing::info;

use crate::actors::driver::{DriverActor, HookCtx, Hooks};
use crate::actors::provenance::ShotSource;

/// How many shots' trigger modes the card remembers: more than can be open
/// at once (a shot's launch comes before the next shot's trigger).
const TRIGGER_MODES: usize = 8;

/// The FlightScope card: the mode at each shot's trigger and the launch-only
/// spin estimate.
#[derive(Debug, Default)]
pub struct MevoHooks {
    /// The detection mode at each recent shot's trigger, by shot id, newest
    /// last.
    trigger_modes: VecDeque<(String, Option<ShotDetectionMode>)>,
}

impl Hooks<Mevo> for MevoHooks {
    type Section = MevoSection;
    const SOURCE: ShotSource = ShotSource::Mevo;

    fn on_frp(&mut self, event: &mut FrpEvent, ctx: &mut HookCtx<'_, MevoSection>) {
        match event {
            FrpEvent::ShotTrigger { key, .. } => {
                if self.trigger_modes.len() == TRIGGER_MODES {
                    self.trigger_modes.pop_front();
                }
                self.trigger_modes
                    .push_back((key.shot_id.clone(), ctx.session_mode.or(ctx.mode)));
            }
            FrpEvent::Launch { key, launch } => {
                if let Some(at) = self
                    .trigger_modes
                    .iter()
                    .position(|(id, _)| *id == key.shot_id)
                    && let Some((_, mode)) = self.trigger_modes.remove(at)
                {
                    ctx.mode = mode;
                }
                if ctx.flight.is_none()
                    && let Some(back) = launch_only_spin(
                        launch,
                        ctx.section.launch_only_spin_rpm_per_mph(),
                        ctx.mode,
                    )
                {
                    launch.backspin_rpm = Some(back);
                    launch.sidespin_rpm = Some(0);
                    ctx.spin_estimated = true;
                    info!(
                        "shot #{}: launch only, estimated backspin {back} rpm ({} rpm per mph)",
                        key.shot_number,
                        ctx.section.launch_only_spin_rpm_per_mph()
                    );
                }
            }
            _ => {}
        }
    }
}

/// The estimated backspin (rpm) for `launch`, a launch the device sent no
/// flight result for: `rpm_per_mph` times its ball speed in mph, rounded.
/// `None` (no estimate) when `rpm_per_mph` is not above 0, `mode` (the mode
/// at the trigger) is Putting, the launch has any spin, its launch angle is
/// under [`LAUNCH_ONLY_MIN_LAUNCH_DEG`] or unknown, or its speed is not
/// positive or the product is out of range for an rpm.
fn launch_only_spin(
    launch: &Launch,
    rpm_per_mph: f64,
    mode: Option<ShotDetectionMode>,
) -> Option<i32> {
    if rpm_per_mph <= 0.0 || mode == Some(ShotDetectionMode::Putting) {
        return None;
    }
    if launch.backspin_rpm.unwrap_or(0) != 0
        || launch.sidespin_rpm.unwrap_or(0) != 0
        || launch.total_spin_rpm.unwrap_or(0) != 0
    {
        return None;
    }
    if !launch
        .launch_elevation
        .is_some_and(|vla| vla >= LAUNCH_ONLY_MIN_LAUNCH_DEG)
    {
        return None;
    }
    let mph = launch.launch_speed?.as_mph();
    if !(mph.is_finite() && mph > 0.0) {
        return None;
    }
    let rpm = (rpm_per_mph * mph).round();
    if rpm > f64::from(i32::MAX) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)] // 0 to i32::MAX, checked above
    Some(rpm as i32)
}

/// The FlightScope actor (`mevo.N`).
pub type MevoActor = DriverActor<Mevo, MevoHooks>;

#[cfg(test)]
use ironsight as iron;
#[cfg(test)]
mod scenario;

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use flighthook::{
        ActorStatus, Distance, FlighthookEvent, FlighthookMessage, Launch, MevoConfig, ShotKey,
        SpinSource, Velocity,
    };
    use flightrelay::config::{ConfigError, DriverConfig};
    use flightrelay::driver::{
        ConnectCtx, DeviceCommand, DeviceDriver, DeviceIdentity, DeviceSession, DriverInfo, Events,
        NoAction,
    };
    use ironsight::protocol::shot::{FlightResult as D4, FlightResultV1};
    use ironsight::testing::{Shot, fixtures};
    use serde_json::{Value, json};
    use tokio::sync::broadcast;

    use super::scenario::{Bench, Model, play, section};
    use super::*;
    use crate::actors::scripted::Handoff;
    use crate::actors::{Actor, ReconfigureOutcome};
    use crate::bus::BusSender;
    use crate::state::{GameStateWriter, SystemState};

    // -- The equivalence harness ---------------------------------------------

    /// The bus messages and export lines of flighthook ec53ba6's Mevo actor
    /// (its own session code on ironsight 0.6.0) for each model's script
    /// ([`play`]), recorded with this module's scripted device.
    fn recorded(model: Model) -> (&'static str, &'static str) {
        match model {
            Model::MevoPlus => (
                include_str!("../../testdata/mevo_old_mevo_plus_session.jsonl"),
                include_str!("../../testdata/mevo_old_mevo_plus_export.jsonl"),
            ),
            Model::Gen2 => (
                include_str!("../../testdata/mevo_old_gen2_session.jsonl"),
                include_str!("../../testdata/mevo_old_gen2_export.jsonl"),
            ),
            Model::Xi => (
                include_str!("../../testdata/mevo_old_xi_session.jsonl"),
                include_str!("../../testdata/mevo_old_xi_export.jsonl"),
            ),
        }
    }

    fn lines(text: &str) -> Vec<Value> {
        text.lines()
            .map(|l| serde_json::from_str(l).expect("a recorded line"))
            .collect()
    }

    /// `messages` with their shot and group ids numbered in order of
    /// appearance (the ids are random), the scripted device's local port
    /// taken out of the Xi's device id, and each shot's raw frames taken
    /// out (compared on their own).
    fn normalized(mut messages: Vec<Value>) -> (Vec<Value>, Vec<Vec<Value>>) {
        let mut ids: HashMap<String, String> = HashMap::new();
        let mut raws = Vec::new();
        for m in &mut messages {
            scrub(m, &mut ids);
            let at = if m.pointer("/event/raw").is_some() {
                "/event/raw"
            } else {
                "/raw"
            };
            if let Some(raw) = m.pointer_mut(at)
                && raw.is_object()
            {
                raws.push(raw["frames"].as_array().cloned().unwrap_or_default());
                *raw = json!({ "transport": raw["transport"] });
            }
        }
        (messages, raws)
    }

    /// Number every UUID string under `v`, and write the local address of
    /// a device id as `<addr>`.
    fn scrub(v: &mut Value, ids: &mut HashMap<String, String>) {
        match v {
            Value::String(s) if s.len() == 36 && s.matches('-').count() == 4 => {
                let n = ids.len();
                *s = ids
                    .entry(s.clone())
                    .or_insert_with(|| format!("id-{n}"))
                    .clone();
            }
            Value::String(s) if s.contains("(127.0.0.1:") => {
                let start = s.find("(127.0.0.1:").expect("address");
                let end = start + s[start..].find(')').expect("closing");
                s.replace_range(start..=end, "(<addr>)");
            }
            Value::Array(a) => a.iter_mut().for_each(|v| scrub(v, ids)),
            Value::Object(o) => o.values_mut().for_each(|v| scrub(v, ids)),
            _ => {}
        }
    }

    fn kind(m: &Value) -> &str {
        m["event"]["kind"].as_str().unwrap_or("")
    }

    fn status_message(status: &str, telemetry: Value) -> Value {
        json!({ "actor": "mevo.0", "event": { "kind": "actor_status", "status": status, "telemetry": telemetry } })
    }

    /// `messages` without a `device_telemetry` that repeats the telemetry
    /// already published (the old actor sent its whole map at every
    /// keepalive; the driver sends what changed, and the card publishes the
    /// merged map).
    fn without_repeats(messages: Vec<Value>) -> Vec<Value> {
        let mut last: Option<Value> = None;
        messages
            .into_iter()
            .filter(|m| {
                if kind(m) != "device_telemetry" || m["event"].get("model").is_some() {
                    return true;
                }
                let t = m["event"]["telemetry"].clone();
                let repeat = last.as_ref() == Some(&t);
                last = Some(t);
                !repeat
            })
            .collect()
    }

    /// The card text of `model` on ironsight's driver: the identity's
    /// label, manufacturer and model.
    fn device_info(model: Model) -> &'static str {
        match model {
            Model::MevoPlus => "FlightScope Mevo+",
            Model::Gen2 => "FlightScope Mevo Gen2",
            Model::Xi => "FlightScope Xi",
        }
    }

    /// The telemetry map `t` as the driver reports it for `model`: the
    /// Mevo+'s temperature as its status says (31.0 °C on the scripted
    /// device; the old actor sent `0.0` until a keepalive), and no
    /// temperature or power on a Gen2 or Xi, whose status has none.
    fn telemetry_on_driver(model: Model, t: &mut Value) {
        let Some(map) = t.as_object_mut() else {
            return;
        };
        match model {
            Model::MevoPlus => {
                if map.contains_key("temp_c") {
                    map.insert("temp_c".into(), json!("31.0"));
                }
            }
            Model::Gen2 | Model::Xi => {
                map.remove("temp_c");
                map.remove("external_power");
            }
        }
    }

    /// What the recorded actor's `old` messages for `model` become on
    /// `DriverActor` with ironsight's driver. Each change is one of design
    /// §7.3's rows for the Mevo, a rule of `DriverActor` accepted in steps 1
    /// to 3E, or one of ironsight's flighthook-visible differences accepted
    /// at its review; the report lists each one.
    fn expected_from_old(model: Model, old: &[Value]) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        let mut device: Option<Value> = None;
        let mut session_telemetry = json!({});
        let mut i = 0;
        while i < old.len() {
            let mut m = old[i].clone();
            if kind(&m) == "device_telemetry" {
                telemetry_on_driver(model, &mut m["event"]["telemetry"]);
            }
            let status = m["event"]["status"].as_str().unwrap_or("").to_owned();
            let next = old.get(i + 1);
            match kind(&m) {
                // §7.3 #8: the driver's connect phases show on the card
                // while it starts.
                "actor_status" if status == "starting" => {
                    out.push(m);
                    for phase in ["connecting", "handshaking", "configuring"] {
                        out.push(status_message("starting", json!({ "phase": phase })));
                    }
                }
                // DriverActor: the identity telemetry follows the Connected
                // status, which carries the identity's card text (ironsight
                // report #10, see the report), the shot count (steps 1/2)
                // and removes the connect's phase (§7.3 #8).
                "device_telemetry" if m["event"].get("model").is_some() => {
                    device = m.get("device").cloned();
                    let connected = next.expect("Connected after the identity");
                    assert_eq!(connected["event"]["status"], "connected");
                    let mut status = status_message(
                        "connected",
                        json!({ "device_info": device_info(model), "shot_count": "0", "phase": "" }),
                    );
                    status["device"] = device.clone().expect("device id");
                    out.push(status);
                    session_telemetry = m["event"]["telemetry"].clone();
                    out.push(m);
                    i += 2;
                    continue;
                }
                // The old actor's empty Connected statuses changed nothing
                // on the card; the driver's phases take their place: a
                // reconfigure shows `configuring`, the arm `measuring`, a
                // trigger nothing (§7.3 #8).
                "actor_status" if status == "connected" => {
                    let ready = next.and_then(|n| n["event"]["telemetry"]["ready"].as_str());
                    let after_trigger = out.last().is_some_and(|l| kind(l) == "shot_trigger");
                    match ready {
                        Some(_) if after_trigger => {}
                        Some("false") => out.push(status_message(
                            "connected",
                            json!({ "phase": "configuring" }),
                        )),
                        Some("true") => {
                            out.push(status_message("connected", json!({ "phase": "measuring" })));
                        }
                        _ => panic!("unexpected empty Connected at {i}"),
                    }
                }
                // Steps 1/2: the card's shot count follows each finished
                // shot.
                "shot_finished" => {
                    out.push(m);
                    let shots = out.iter().filter(|m| kind(m) == "shot_finished").count();
                    out.push(status_message(
                        "connected",
                        json!({ "shot_count": shots.to_string() }),
                    ));
                }
                // ironsight report #4 (review: correct, FRP's "send the
                // other pair when known") and §7.3 #14 for the Xi's E8
                // split: a launch with backspin and sidespin also carries
                // the total and the axis.
                "launch" => {
                    let launch = &mut m["event"]["launch"];
                    if launch.get("backspin_rpm").is_some() && launch.get("sidespin_rpm").is_some()
                    {
                        let mut l: Launch =
                            serde_json::from_value(launch.clone()).expect("a launch");
                        l.complete_spin();
                        *launch = serde_json::to_value(l).expect("json");
                    }
                    out.push(m);
                }
                // A message the session cannot decode: ironsight's alert
                // names the error without the message's bytes (ironsight
                // review M1).
                "alert"
                    if m["event"]["message"]
                        .as_str()
                        .is_some_and(|t| t.starts_with("Could not process message: Wire(")) =>
                {
                    m["event"]["message"] = json!(
                        "Could not process message: wire error: payload too short for AvrStatus: need 25 bytes, got 20"
                    );
                    out.push(m);
                }
                // Step 2 (accepted): a lost session sends `ready=false` with
                // the rest of the session's telemetry, then Reconnecting,
                // then the driver's alert, whose text is the error itself
                // (ironsight report #9, the SDK's default retry delay).
                "alert"
                    if m["event"]["message"]
                        .as_str()
                        .is_some_and(|t| t.starts_with("Device connection error: ")) =>
                {
                    let text = m["event"]["message"].as_str().expect("text");
                    let error = text
                        .trim_start_matches("Device connection error: ")
                        .to_owned();
                    let ready = next.expect("ready=false");
                    assert_eq!(ready["event"]["telemetry"], json!({ "ready": "false" }));
                    let lost = old.get(i + 2).expect("Reconnecting");
                    assert_eq!(lost["event"]["status"], "reconnecting");
                    let mut telemetry = session_telemetry.clone();
                    telemetry["ready"] = json!("false");
                    let mut ready = ready.clone();
                    ready["event"]["telemetry"] = telemetry;
                    out.push(ready);
                    out.push(lost.clone());
                    m["event"]["message"] = json!(error);
                    out.push(m);
                    i += 3;
                    continue;
                }
                "device_telemetry" => {
                    if let Some(t) = m["event"]["telemetry"].as_object() {
                        for (k, v) in t {
                            session_telemetry[k] = v.clone();
                        }
                    }
                    out.push(m);
                }
                _ => out.push(m),
            }
            i += 1;
        }
        // DriverActor: every message from the session's Connected on
        // carries its device id, statuses and alerts included.
        if let Some(from) = out.iter().position(|m| m.get("device").is_some()) {
            for m in &mut out[from..] {
                m["device"] = device.clone().expect("device id");
            }
        }
        without_repeats(out)
    }

    /// FRP 0.2.0 (design §3.1): a trigger carries the host time the driver
    /// saw it at. Taken out of `messages` after checking it lies between
    /// the moment the script handed the shot to the device and the moment
    /// the trigger reached the bus (`shot_times`): the time the old actor's
    /// trigger reached the cameras.
    fn without_trigger_times(messages: &mut [Value], shot_times: &[(u64, u64)]) {
        let mut times = shot_times.iter();
        for m in messages.iter_mut().filter(|m| kind(m) == "shot_trigger") {
            let at = m["event"]
                .as_object_mut()
                .expect("event")
                .remove("timestamp")
                .and_then(|t| t.as_u64())
                .expect("a trigger time");
            if let Some(&(pushed, arrived)) = times.next() {
                assert!(
                    pushed <= at && at <= arrived,
                    "trigger at {at}, pushed {pushed}, on the bus {arrived}"
                );
            }
        }
    }

    /// §7.3 #1 and #19: each shot's raw records are the recorded actor's
    /// minus the device's free text and DSP debug messages, in the same
    /// order, each labeled `"<type> <src>><dst>"` instead of carrying
    /// `type`, `src` and `dst`. The pre-roll ring (8 frames) reaches further
    /// back by as many frames as it no longer holds of those, so the
    /// recorded frames are the end of the new ones.
    fn assert_raw_matches(old: &[Vec<Value>], new: &[Vec<Value>]) {
        assert_eq!(old.len(), new.len(), "shots with raw frames");
        for (n, (old, new)) in old.iter().zip(new).enumerate() {
            let audit_only = |f: &&Value| matches!(f["type"].as_str(), Some("0xE3" | "0xF0"));
            let want: Vec<(String, String, String)> = old
                .iter()
                .filter(|f| !audit_only(f))
                .map(|f| {
                    assert!(f.get("label").is_none());
                    (
                        f["dir"].as_str().expect("dir").to_owned(),
                        format!(
                            "{} {}>{}",
                            f["type"].as_str().expect("type").trim_start_matches("0x"),
                            f["src"].as_str().expect("src"),
                            f["dst"].as_str().expect("dst")
                        ),
                        f["hex"].as_str().unwrap_or("").to_owned(),
                    )
                })
                .collect();
            let got: Vec<(String, String, String)> = new
                .iter()
                .map(|f| {
                    assert!(
                        f.get("type").is_none()
                            && f.get("src").is_none()
                            && f.get("channel").is_none(),
                        "shot {n}: {f}"
                    );
                    (
                        f["dir"].as_str().expect("dir").to_owned(),
                        f["label"].as_str().expect("label").to_owned(),
                        f["hex"].as_str().unwrap_or("").to_owned(),
                    )
                })
                .collect();
            let left_out = old
                .iter()
                .filter(|f| audit_only(f) && f["t_ms"].as_i64().is_some_and(|t| t < 0))
                .count();
            assert!(
                got.len() >= want.len() && got.len() - want.len() <= left_out,
                "shot {n}: {} frames for {} recorded",
                got.len(),
                want.len()
            );
            assert_eq!(got[got.len() - want.len()..], want[..], "shot {n}");
        }
    }

    /// Design §7.2's equivalence harness, per model: the recorded Mevo
    /// actor and the card on ironsight's driver, fed the same scripted
    /// device and the same game events, put the same messages on the bus
    /// and export the same shots, except for the differences
    /// `expected_from_old` and `without_trigger_times` list and the raw
    /// records' labels and left-out messages (§7.3 #1, #19).
    async fn matches_the_recorded_actor(model: Model) {
        let mut bench = Bench::start(model, section(model), |s| {
            Box::new(MevoActor::new(s.clone()))
        });
        play(&mut bench, model);
        let (old_bus, old_export) = recorded(model);

        let (old, old_raws) = normalized(lines(old_bus));
        let (mut new, new_raws) = normalized(
            bench
                .seen
                .iter()
                .map(|m| serde_json::to_value(m).expect("json"))
                .collect(),
        );
        without_trigger_times(&mut new, &bench.shot_times);
        let new = without_repeats(new);
        let expected = expected_from_old(model, &old);
        for (n, (want, got)) in expected.iter().zip(&new).enumerate() {
            assert_eq!(got, want, "{model:?} message {n}");
        }
        assert_eq!(
            new.len(),
            expected.len(),
            "{model:?}: {:?}",
            new.iter().map(kind).collect::<Vec<_>>()
        );
        assert_raw_matches(&old_raws, &new_raws);

        // The export: the same lines but for the time, the launch's total
        // and axis (as on the bus; the metrics derived them before) and the
        // raw records (compared above).
        let config = bench.state.system.snapshot();
        let export = crate::actors::web::tests::export_lines(&config, &bench.seen).await;
        let (old_export, old_export_raws) = normalized(lines(old_export));
        let (new_export, new_export_raws) = normalized(
            export
                .iter()
                .map(|l| serde_json::from_str(l).expect("json"))
                .collect(),
        );
        assert_eq!(new_export.len(), old_export.len());
        for (mut want, got) in old_export.into_iter().zip(new_export) {
            want["time"] = got["time"].clone();
            if let Some(metrics) = want
                .pointer_mut("/data/metrics")
                .and_then(Value::as_object_mut)
                && let (Some(total), Some(axis)) = (
                    metrics.remove("total_spin_rpm"),
                    metrics.remove("spin_axis"),
                )
            {
                want["data"]["launch"]["total_spin_rpm"] = total;
                want["data"]["launch"]["spin_axis"] = axis;
            }
            assert_eq!(got, want, "{model:?}");
        }
        assert_raw_matches(&old_export_raws, &new_export_raws);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mevo_plus_card_matches_the_recorded_actor() {
        matches_the_recorded_actor(Model::MevoPlus).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_gen2_card_matches_the_recorded_actor() {
        matches_the_recorded_actor(Model::Gen2).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_xi_card_matches_the_recorded_actor() {
        matches_the_recorded_actor(Model::Xi).await;
    }

    // -- A replayed device ----------------------------------------------------

    /// What a replayed device does next.
    enum Step {
        /// These events, in one poll.
        Emit(Vec<FrpEvent>),
        /// Nothing, until the flag is set.
        Hold(Arc<AtomicBool>),
    }

    /// Replays waiting for a driver, by the card's `address`: each test
    /// runs with its own (see [`Handoff`]).
    static REPLAYS: Handoff<String, Vec<Step>> = Handoff::new();

    /// A FlightScope driver that replays a list of events as ironsight's
    /// session emits them: for the card's own processing of events the
    /// scripted device cannot produce (a launch-only chip, a device total).
    struct ReplayMevo {
        config: MevoConfig,
        steps: VecDeque<Step>,
    }

    struct ReplaySession {
        identity: DeviceIdentity,
        steps: VecDeque<Step>,
    }

    impl DeviceDriver for ReplayMevo {
        type Config = MevoConfig;
        type Error = ironsight::driver::Error;
        type Action = NoAction;
        type Session = ReplaySession;
        const STOP_WAIT: Duration = Mevo::STOP_WAIT;

        fn info(config: &MevoConfig) -> DriverInfo {
            Mevo::info(config)
        }

        fn new(config: MevoConfig) -> Result<Self, ConfigError> {
            config.validate()?;
            let steps = REPLAYS
                .take(&config.address.clone().unwrap_or_default())
                .unwrap_or_default();
            Ok(Self {
                config,
                steps: steps.into(),
            })
        }

        fn config(&self) -> &MevoConfig {
            &self.config
        }

        fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<ReplaySession, Self::Error> {
            ctx.tick()?;
            Ok(ReplaySession {
                identity: DeviceIdentity::new("FS M2-REPLAY")
                    .with_manufacturer("FlightScope")
                    .with_model("Mevo+"),
                steps: std::mem::take(&mut self.steps),
            })
        }

        fn apply_config(&mut self, new: &MevoConfig) {
            self.config = new.clone();
        }
    }

    impl DeviceSession for ReplaySession {
        type Driver = ReplayMevo;

        fn poll(
            &mut self,
            timeout: Duration,
            out: &mut Events<'_>,
        ) -> Result<(), ironsight::driver::Error> {
            match self.steps.front() {
                Some(Step::Emit(_)) => {
                    if let Some(Step::Emit(events)) = self.steps.pop_front() {
                        for event in events {
                            out.frp(event);
                        }
                    }
                }
                Some(Step::Hold(flag)) if flag.load(Ordering::SeqCst) => {
                    self.steps.pop_front();
                }
                _ => std::thread::sleep(timeout.min(Duration::from_millis(5))),
            }
            Ok(())
        }

        fn command(&mut self, _: &DeviceCommand) -> Result<(), ironsight::driver::Error> {
            Ok(())
        }

        fn identity(&self) -> &DeviceIdentity {
            &self.identity
        }

        fn close(self, out: &mut Events<'_>) -> Result<(), ironsight::driver::Error> {
            out.ready(false);
            Ok(())
        }
    }

    impl Hooks<ReplayMevo> for MevoHooks {
        type Section = MevoSection;
        const SOURCE: ShotSource = ShotSource::Mevo;

        fn on_frp(&mut self, event: &mut FrpEvent, ctx: &mut HookCtx<'_, MevoSection>) {
            <Self as Hooks<Mevo>>::on_frp(self, event, ctx);
        }
    }

    /// A running replay card (`mevo.0`), the game state it reads and the
    /// bus.
    struct Replay {
        state: Arc<SystemState>,
        writer: GameStateWriter,
        actor: Arc<DriverActor<ReplayMevo, MevoHooks>>,
        sender: BusSender,
        rx: broadcast::Receiver<FlighthookMessage>,
        _dir: crate::actors::web::tests::TempDir,
    }

    /// A card with `section` (its address the replay's key) replaying
    /// `steps`, the game in `mode`.
    fn replay(section: MevoSection, mode: ShotDetectionMode, steps: Vec<Step>) -> Replay {
        REPLAYS.put(section.driver.address.clone().unwrap_or_default(), steps);
        let dir = std::env::temp_dir().join(format!(
            "flighthook-mevo-replay-{}",
            crate::state::config::generate_id()
        ));
        let (state, writer) = SystemState::new(dir.join("config.toml"));
        writer.set_mode(mode);
        state.system.update(|c| {
            c.mevo.insert("0".into(), section.clone());
        });
        let state = Arc::new(state);
        let (tx, rx) = broadcast::channel(1024);
        let sender = BusSender::new("mevo.0".into(), tx, Arc::default())
            .with_provenance(ShotSource::Mevo.provenance());
        let actor = Arc::new(DriverActor::<ReplayMevo, MevoHooks>::new(section));
        actor.start(Arc::clone(&state), sender.clone(), sender.subscribe());
        Replay {
            state,
            writer,
            actor,
            sender,
            rx,
            _dir: crate::actors::web::tests::TempDir(dir),
        }
    }

    impl Replay {
        /// Messages until one matches `want`, within 5 s.
        fn until(
            &mut self,
            mut want: impl FnMut(&FlighthookMessage) -> bool,
        ) -> Vec<FlighthookMessage> {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut seen = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "timed out: {} seen", seen.len());
                match self.rx.try_recv() {
                    Ok(m) => {
                        let done = want(&m);
                        seen.push(m);
                        if done {
                            return seen;
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }

        /// The messages of the next finished shot, up to its end.
        fn shot(&mut self) -> Vec<FlighthookMessage> {
            self.until(|m| matches!(m.event, FlighthookEvent::ShotFinished { .. }))
        }

        /// The messages of the next [`launch_only_shot`]: up to its launch,
        /// which is on the bus before the device finishes the shot, then,
        /// once `finish` is set, up to its end.
        fn launch_only(&mut self, finish: &AtomicBool) -> Vec<FlighthookMessage> {
            let mut seen = self.until(|m| matches!(m.event, FlighthookEvent::Launch { .. }));
            assert!(
                !seen
                    .iter()
                    .any(|m| matches!(m.event, FlighthookEvent::ShotFinished { .. })),
                "the ball goes out before the device finishes the shot"
            );
            finish.store(true, Ordering::SeqCst);
            seen.extend(self.shot());
            seen
        }
    }

    impl Drop for Replay {
        fn drop(&mut self) {
            self.actor.stop();
        }
    }

    /// A card section replaying under `key` (an address no other test uses).
    fn replay_section(key: &str) -> MevoSection {
        MevoSection {
            driver: MevoConfig {
                address: Some(key.into()),
                ..MevoConfig::default()
            },
            ..MevoSection::default()
        }
    }

    fn shot_key(n: u32) -> ShotKey {
        ShotKey::new(format!("replay-{n}"), n)
    }

    /// Shot `n` as ironsight's session emits it: the trigger, a hold until
    /// `go` when given, then `rest`.
    fn replayed_shot(n: u32, go: Option<Arc<AtomicBool>>, rest: Vec<FrpEvent>) -> Vec<Step> {
        let mut steps = vec![Step::Emit(vec![FrpEvent::ShotTrigger {
            key: shot_key(n),
            timestamp: Some(super::scenario::now_ms()),
        }])];
        if let Some(go) = go {
            steps.push(Step::Hold(go));
        }
        steps.push(Step::Emit(rest));
        steps
    }

    /// Shot `n` as ironsight's session emits a launch-only full result: the
    /// trigger, then `launch` with no flight result, then `shot_finished`
    /// only once `finish` is set (the device finishes the shot after its
    /// re-arm, up to seconds later).
    fn launch_only_shot(n: u32, launch: Launch, finish: &Arc<AtomicBool>) -> Vec<Step> {
        let mut steps = replayed_shot(n, None, vec![FrpEvent::launch(shot_key(n), launch)]);
        steps.push(Step::Hold(Arc::clone(finish)));
        steps.push(Step::Emit(vec![FrpEvent::shot_finished(shot_key(n))]));
        steps
    }

    /// The launch-only Xi chip the old card's pins used: 7.019 m/s (15.7
    /// mph) at 22.8°, 0.856° left, no spin, no carry, apex or flight time.
    fn launch_only_d4() -> D4 {
        D4 {
            total: 1,
            track_time: 0.0,
            start_position: [0.0; 3],
            launch_speed: 7.019,
            // Wire sign: positive = left.
            launch_azimuth: 0.856,
            launch_elevation: 22.8,
            carry_distance: 0.0,
            flight_time: 0.0,
            max_height: 0.0,
            landing_position: [0.0; 3],
            backspin_rpm: 0,
            sidespin_rpm: 0,
            riflespin_rpm: 0,
            landing_spin_rpm: [0; 3],
            landing_velocity: [0.0; 3],
            total_distance: 0.0,
            roll_distance: 0.0,
            final_position: [0.0; 3],
            clubhead_speed: 0.0,
            club_strike_direction: 0.0,
            club_attack_angle: 0.0,
            clubhead_speed_post: 0.0,
            club_swing_plane_tilt: 0.0,
            club_swing_plane_rotation: 0.0,
            club_effective_loft: 0.0,
            club_face_angle: 0.0,
            poly_scale: 0,
            poly_x: [0.0; 5],
            poly_y: [0.0; 5],
            poly_z: [0.0; 5],
        }
    }

    /// The launch ironsight's session sends for a launch-only full result
    /// `d4`: its launch with no spin fields, and no flight result.
    fn launch_only_launch(d4: &D4) -> Launch {
        assert!(ironsight::driver::convert::launch_only(d4));
        Launch {
            backspin_rpm: None,
            sidespin_rpm: None,
            total_spin_rpm: None,
            spin_axis: None,
            ..ironsight::driver::convert::launch(d4)
        }
    }

    fn launch_of(seen: &[FlighthookMessage]) -> (Launch, Option<flighthook::DerivedFlight>) {
        seen.iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::Launch {
                    launch, derived, ..
                } => Some((launch.as_ref().clone(), derived.as_deref().cloned())),
                _ => None,
            })
            .expect("a launch")
    }

    // -- The card's own processing ------------------------------------------

    /// A shot is simulated in the detection mode at its trigger: a mode
    /// change between the trigger and the launch (the full result comes
    /// seconds later) doesn't change where a ground ball rolls (the green in
    /// Putting, the fairway otherwise).
    #[test]
    fn a_launch_is_simulated_in_the_mode_at_its_trigger() {
        let ground_ball = || Launch {
            launch_speed: Some(Velocity::MetersPerSecond(3.0)),
            launch_elevation: Some(0.0),
            launch_azimuth: Some(0.0),
            ..Launch::default()
        };
        let total = |key: &str, at_trigger, at_launch| {
            let go = Arc::new(AtomicBool::new(false));
            let steps = replayed_shot(
                1,
                Some(Arc::clone(&go)),
                vec![
                    FrpEvent::launch(shot_key(1), ground_ball()),
                    FrpEvent::shot_finished(shot_key(1)),
                ],
            );
            let mut card = replay(replay_section(key), at_trigger, steps);
            card.until(|m| matches!(m.event, FlighthookEvent::ShotTrigger { .. }));
            card.writer.set_mode(at_launch);
            go.store(true, Ordering::SeqCst);
            let (_, derived) = launch_of(&card.shot());
            derived
                .and_then(|d| d.total_distance)
                .expect("a rolled total")
                .as_meters()
        };
        use ShotDetectionMode::{Full, Putting};
        let green = total("10.1.0.1:5100", Putting, Putting);
        let fairway = total("10.1.0.2:5100", Full, Full);
        assert!(green > fairway, "{green} vs {fairway}");
        assert_eq!(total("10.1.0.3:5100", Putting, Full), green);
        assert_eq!(total("10.1.0.4:5100", Full, Putting), fairway);
    }

    /// The mode at the trigger is the mode the session (the driver) was
    /// handed, not the game state's: the system actor can change the game
    /// state before the card drains the bus's `set_detection_mode` (or the
    /// card can take one the game state shows later), so in the trigger's
    /// `poll` pass the two can differ. The simulation goes with the driver,
    /// whose own mode decides the Xi's `auto` spin source.
    #[test]
    fn a_mode_change_in_the_triggers_pass_is_the_drivers() {
        let ground_ball = || Launch {
            launch_speed: Some(Velocity::MetersPerSecond(3.0)),
            launch_elevation: Some(0.0),
            launch_azimuth: Some(0.0),
            ..Launch::default()
        };
        // The game state's mode and the bus's at the trigger.
        let total = |key: &str, game: ShotDetectionMode, bus: Option<ShotDetectionMode>| {
            let go = Arc::new(AtomicBool::new(false));
            let mut steps = vec![Step::Hold(Arc::clone(&go))];
            steps.extend(replayed_shot(
                1,
                None,
                vec![
                    FrpEvent::launch(shot_key(1), ground_ball()),
                    FrpEvent::shot_finished(shot_key(1)),
                ],
            ));
            let mut card = replay(replay_section(key), ShotDetectionMode::Full, steps);
            card.until(|m| {
                matches!(
                    m.event,
                    FlighthookEvent::ActorStatus {
                        status: ActorStatus::Connected,
                        ..
                    }
                )
            });
            card.writer.set_mode(game);
            if let Some(mode) = bus {
                card.sender
                    .send(FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
                        mode: Some(mode),
                        handed: None,
                        club: None,
                    }));
            }
            // The next pass drains what was sent before the hold ends, then
            // the trigger comes.
            go.store(true, Ordering::SeqCst);
            let (_, derived) = launch_of(&card.shot());
            derived
                .and_then(|d| d.total_distance)
                .expect("a rolled total")
                .as_meters()
        };
        use ShotDetectionMode::{Full, Putting};
        let fairway = total("10.1.7.1:5100", Full, None);
        let green = total("10.1.7.2:5100", Putting, Some(Putting));
        assert!(green > fairway, "{green} vs {fairway}");
        // The game state shows Putting, the card has not taken it yet.
        assert_eq!(total("10.1.7.3:5100", Putting, None), fairway);
        // The card took Putting, the game state does not show it yet.
        assert_eq!(total("10.1.7.4:5100", Full, Some(Putting)), green);
    }

    /// Design §5.3: a launch-only chip (speed and launch angle, no spin, no
    /// flight result) gets a backspin of `launch_only_spin` rpm per mph and
    /// no sidespin, marked as estimated, and the simulation flies it. Design
    /// §6.1: it goes out at its full result, not when the device finishes
    /// the shot.
    #[test]
    fn a_launch_only_chip_gets_the_estimated_spin() {
        let finish = Arc::new(AtomicBool::new(false));
        let steps = launch_only_shot(1, launch_only_launch(&launch_only_d4()), &finish);
        let mut card = replay(
            replay_section("10.1.1.1:5100"),
            ShotDetectionMode::Chipping,
            steps,
        );
        let (launch, derived) = launch_of(&card.launch_only(&finish));
        // 100 rpm per mph × 15.7 mph.
        assert_eq!(launch.backspin_rpm, Some(1570));
        assert_eq!(launch.sidespin_rpm, Some(0));
        let derived = derived.expect("simulated");
        assert!(derived.spin_estimated);
        assert!(derived.carry_distance.is_some_and(|c| c.as_meters() > 0.0));
    }

    /// No estimate when the mode at the trigger was Putting (whatever it is
    /// when the launch arrives), when the launch is under 5° (it rolls as a
    /// ground ball), when `launch_only_spin` is 0, or when the launch has a
    /// spin of the device's (§7.3 #11: an Xi E8 total).
    #[test]
    fn a_launch_only_estimate_needs_a_chip_without_spin() {
        let base = launch_only_launch(&launch_only_d4());
        // The card, the launch, `launch_only_spin`, the mode at the trigger
        // and at the launch, and the backspin the launch goes out with.
        type Case = (
            &'static str,
            Launch,
            f64,
            ShotDetectionMode,
            ShotDetectionMode,
            Option<i32>,
        );
        let cases: [Case; 6] = [
            (
                "10.1.2.1:5100",
                base.clone(),
                100.0,
                ShotDetectionMode::Putting,
                ShotDetectionMode::Full,
                None,
            ),
            (
                "10.1.2.2:5100",
                base.clone(),
                100.0,
                ShotDetectionMode::Full,
                ShotDetectionMode::Putting,
                Some(1570),
            ),
            (
                "10.1.2.3:5100",
                Launch {
                    launch_elevation: Some(4.9),
                    ..base.clone()
                },
                100.0,
                ShotDetectionMode::Full,
                ShotDetectionMode::Full,
                None,
            ),
            (
                "10.1.2.4:5100",
                base.clone(),
                0.0,
                ShotDetectionMode::Full,
                ShotDetectionMode::Full,
                None,
            ),
            (
                "10.1.2.5:5100",
                Launch {
                    total_spin_rpm: Some(1800),
                    ..base.clone()
                },
                100.0,
                ShotDetectionMode::Full,
                ShotDetectionMode::Full,
                None,
            ),
            (
                "10.1.2.6:5100",
                base.clone(),
                80.0,
                ShotDetectionMode::Chipping,
                ShotDetectionMode::Chipping,
                // 80 rpm per mph × 15.7 mph.
                Some(1256),
            ),
        ];
        for (key, launch, k, at_trigger, at_launch, want) in cases {
            let go = Arc::new(AtomicBool::new(false));
            let steps = replayed_shot(
                1,
                Some(Arc::clone(&go)),
                vec![
                    FrpEvent::launch(shot_key(1), launch),
                    FrpEvent::shot_finished(shot_key(1)),
                ],
            );
            let mut section = replay_section(key);
            section.launch_only_spin = k;
            let mut card = replay(section, at_trigger, steps);
            card.until(|m| matches!(m.event, FlighthookEvent::ShotTrigger { .. }));
            card.writer.set_mode(at_launch);
            go.store(true, Ordering::SeqCst);
            let (got, derived) = launch_of(&card.shot());
            assert_eq!(got.backspin_rpm, want, "{key}");
            assert_eq!(
                derived.is_some_and(|d| d.spin_estimated),
                want.is_some(),
                "{key}"
            );
        }
    }

    /// `launch_only_spin` is a card setting applied live: a change reaches
    /// the next shot without a restart.
    #[test]
    fn a_launch_only_spin_change_applies_to_the_next_shot() {
        let go = Arc::new(AtomicBool::new(false));
        let chip = |n| {
            vec![
                FrpEvent::launch(shot_key(n), launch_only_launch(&launch_only_d4())),
                FrpEvent::shot_finished(shot_key(n)),
            ]
        };
        let mut steps = replayed_shot(1, None, chip(1));
        steps.extend(replayed_shot(2, Some(Arc::clone(&go)), chip(2)));
        let section = replay_section("10.1.3.1:5100");
        let mut card = replay(section.clone(), ShotDetectionMode::Chipping, steps);
        assert_eq!(launch_of(&card.shot()).0.backspin_rpm, Some(1570));
        let changed = MevoSection {
            launch_only_spin: 50.0,
            ..section
        };
        card.state.system.update(|c| {
            c.mevo.insert("0".into(), changed.clone());
        });
        assert!(matches!(
            card.actor.reconfigure(&card.state, &card.sender),
            ReconfigureOutcome::Applied
        ));
        std::thread::sleep(Duration::from_millis(200));
        go.store(true, Ordering::SeqCst);
        assert_eq!(launch_of(&card.shot()).0.backspin_rpm, Some(785));
    }

    /// §7.3 #10: a full result's total distance reaches flighthook in the
    /// flight result, and the shot's total is the device's; the simulated
    /// roll is total − carry.
    #[test]
    fn the_device_total_is_the_shots_total() {
        let d4 = D4::decode(fixtures::XI_SHOT_D4).expect("d4");
        let mut flight = ironsight::driver::convert::flight_result(&d4);
        flight.total_distance = Some(Distance::Meters(95.0));
        let steps = replayed_shot(
            1,
            None,
            vec![
                FrpEvent::launch(shot_key(1), ironsight::driver::convert::launch(&d4)),
                FrpEvent::FlightResult {
                    key: shot_key(1),
                    flight,
                },
                FrpEvent::shot_finished(shot_key(1)),
            ],
        );
        let mut card = replay(
            replay_section("10.1.4.1:5100"),
            ShotDetectionMode::Full,
            steps,
        );
        let seen = card.shot();
        let (_, derived) = launch_of(&seen);
        let derived = derived.expect("simulated");
        let roll = derived.roll_distance.expect("a roll").as_meters();
        assert!((roll - (95.0 - 85.154)).abs() < 1e-9, "{roll}");
        let mut agg = flighthook::ShotAggregator::new();
        let shot = seen.iter().find_map(|m| agg.feed(m)).expect("a shot");
        assert_eq!(
            shot.flight.and_then(|f| f.total_distance),
            Some(Distance::Meters(95.0))
        );
        assert_eq!(derived.total_distance, None, "measured, not simulated");
    }

    // -- Output pins -----------------------------------------------------------

    /// The messages of the first shot `shot` makes on a scripted `model`
    /// card with `section`, up to its end.
    fn first_wire_shot(model: Model, section: MevoSection, shot: Shot) -> Vec<FlighthookMessage> {
        let mut bench = Bench::start(model, section, |s| Box::new(MevoActor::new(s.clone())));
        bench.ready("true");
        let from = bench.seen.len();
        bench.device.send(super::scenario::Do::Shot(shot));
        bench.until(|m| matches!(m.event, FlighthookEvent::ShotFinished { .. }));
        let seen = bench.seen[from..].to_vec();
        bench.state.stop_and_wait("mevo.0");
        seen
    }

    /// Every output's numbers for a Mevo+ and an Xi full swing (through the
    /// card on ironsight's driver, over the scripted device), a launch-only
    /// Xi chip with the card's spin estimate and a Mevo shot sent from its
    /// early result (both replayed as ironsight's session emits them).
    #[tokio::test(flavor = "multi_thread")]
    async fn outputs_keep_their_numbers() {
        use ironsight::protocol::{TYPE_CLUB_RESULT, TYPE_FLIGHT_RESULT, TYPE_FLIGHT_RESULT_V1};
        let d4 = || fixtures::XI_SHOT_D4.to_vec();
        let e8 = || fixtures::XI_SHOT_E8.to_vec();
        let ed = || fixtures::XI_SHOT_ED.to_vec();
        let avr = ironsight::BusAddr::Avr;

        let mevo = first_wire_shot(
            Model::MevoPlus,
            section(Model::MevoPlus),
            Shot::new(
                63,
                avr,
                [(TYPE_FLIGHT_RESULT, d4()), (TYPE_CLUB_RESULT, ed())],
                ed(),
            ),
        );
        crate::test_ball::assert_outputs_pinned("mevo_full_swing", &mevo);

        let mut xi_section = section(Model::Xi);
        xi_section.driver.spin_source = SpinSource::E8;
        let xi = first_wire_shot(
            Model::Xi,
            xi_section,
            Shot::new(
                63,
                avr,
                [
                    (TYPE_FLIGHT_RESULT_V1, e8()),
                    (TYPE_FLIGHT_RESULT, d4()),
                    (TYPE_CLUB_RESULT, ed()),
                ],
                ed(),
            ),
        );
        crate::test_ball::assert_outputs_pinned("xi_full_swing", &xi);

        let finish = Arc::new(AtomicBool::new(false));
        let mut chip = replay(
            replay_section("10.1.5.1:5100"),
            ShotDetectionMode::Full,
            launch_only_shot(1, launch_only_launch(&launch_only_d4()), &finish),
        );
        crate::test_ball::assert_outputs_pinned("xi_launch_only_chip", &chip.launch_only(&finish));

        let early = FlightResultV1::decode(fixtures::XI_SHOT_E8).expect("e8");
        let mut fallback = replay(
            replay_section("10.1.5.2:5100"),
            ShotDetectionMode::Full,
            replayed_shot(
                1,
                None,
                vec![
                    FrpEvent::launch(
                        shot_key(1),
                        ironsight::driver::convert::early_launch(&early),
                    ),
                    FrpEvent::FlightResult {
                        key: shot_key(1),
                        flight: ironsight::driver::convert::early_flight_result(&early),
                    },
                    FrpEvent::shot_finished(shot_key(1)),
                ],
            ),
        );
        crate::test_ball::assert_outputs_pinned("mevo_e8_fallback", &fallback.shot());
    }

    // -- The card ----------------------------------------------------------------

    /// The device takes a new connection while the old one closes (the
    /// driver is not exclusive), the card waits the driver's 3 s for a
    /// stop, and its shots are measured.
    #[test]
    fn the_card_is_replaceable_while_stopping() {
        let actor = MevoActor::new(MevoSection::default());
        assert!(actor.replaceable_while_stopping());
        assert_eq!(actor.stop_wait(), Duration::from_secs(3));
        assert_eq!(actor.source(), Some(ShotSource::Mevo));
    }

    /// The Xi spin source, the launch-only spin, the camera timing and the
    /// name apply live; every other FlightScope setting starts a new session
    /// (a restart).
    #[test]
    fn only_the_spin_source_and_card_settings_apply_live() {
        let dir = std::env::temp_dir().join(format!(
            "flighthook-mevo-reconfigure-{}",
            crate::state::config::generate_id()
        ));
        let _dir = crate::actors::web::tests::TempDir(dir.clone());
        let (state, _writer) = SystemState::new(dir.join("config.toml"));
        let state = Arc::new(state);
        let (tx, _rx) = broadcast::channel(16);
        let sender = BusSender::new("mevo.0".into(), tx, Arc::default());
        let saved = MevoSection::default();
        let actor = MevoActor::new(saved.clone());
        let outcome = |section: &MevoSection| {
            state.system.update(|c| {
                c.mevo.insert("0".into(), section.clone());
            });
            actor.reconfigure(&state, &sender)
        };
        let restarts: [fn(&mut MevoConfig); 9] = [
            |d| d.address = Some("10.0.0.9:5100".into()),
            |d| d.ball_type = Some(0),
            |d| d.tee_height = Some(Distance::Inches(2.0)),
            |d| d.range = Some(Distance::Feet(9.0)),
            |d| d.surface_height = Some(Distance::Inches(1.0)),
            |d| d.track_pct = Some(70.0),
            |d| d.use_estimated = Some(true),
            |d| d.camera_mode = Some(flighthook::CameraMode::RawFusion),
            |d| d.environment = Some(flighthook::FlightScopeEnvironment::Outdoor),
        ];
        for change in restarts {
            let mut section = saved.clone();
            change(&mut section.driver);
            assert!(matches!(
                outcome(&section),
                ReconfigureOutcome::RestartRequired
            ));
        }
        let mut live = MevoSection {
            name: "Bay 2".into(),
            launch_only_spin: 60.0,
            impact_lead_s: Some(0.2),
            ..saved.clone()
        };
        live.driver.spin_source = SpinSource::Auto;
        assert!(matches!(outcome(&live), ReconfigureOutcome::Applied));
    }

    /// A setting the driver refuses (from a hand-edited file) keeps the card
    /// from starting and shows on it (§7.3 #6 and #15).
    #[test]
    fn a_refused_setting_shows_on_the_card() {
        let mut section = replay_section("10.1.6.1:5100");
        section.driver.track_pct = Some(150.0);
        let mut card = replay(section, ShotDetectionMode::Full, Vec::new());
        let seen = card.until(|m| {
            matches!(
                &m.event,
                FlighthookEvent::ActorStatus {
                    status: ActorStatus::Disconnected,
                    ..
                }
            )
        });
        assert_eq!(seen.len(), 1, "no Starting, no connect");
        assert!(
            matches!(&seen[0].event, FlighthookEvent::ActorStatus { telemetry, .. }
            if telemetry["error"] == "track_pct: must be 0 to 100")
        );
    }
}
