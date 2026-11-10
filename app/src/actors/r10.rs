//! The Garmin R10 card: tenover's device driver ([`R10`]) on
//! [`DriverActor`].
//!
//! The driver owns the device: it finds a paired R10 over BLE, runs the
//! handshake (10 s at most), sends the card's tee distance (`range`) once
//! the device wakes, and reports readiness, device state, tilt, device
//! errors and shots. The card adds its section ([`R10Section`]) and its
//! shot source; it has no processing of its own. A changed range takes a
//! new session (a restart).

use flighthook::R10Section;
use tenover::driver::R10;

use crate::actors::driver::{DriverActor, Hooks};
use crate::actors::provenance::ShotSource;

/// The R10 card: no processing of its own.
#[derive(Debug, Default)]
pub struct R10Hooks;

impl Hooks<R10> for R10Hooks {
    type Section = R10Section;
    const SOURCE: ShotSource = ShotSource::R10;
}

/// The R10 actor (`r10.N`).
pub type R10Actor = DriverActor<R10, R10Hooks>;

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use flighthook::{ActorStatus, FlighthookEvent, FlighthookMessage, R10Config};
    use flightrelay::config::ConfigError;
    use flightrelay::driver::{
        Backoff, ConnectCtx, DeviceDriver, DeviceIdentity, DeviceSession, DriverInfo, Events,
        NoAction, SessionEnd,
    };
    use serde_json::{Value, json};
    use tenover::driver::R10Session;
    use tokio::sync::broadcast;

    use super::*;
    use crate::actors::scripted::Handoff;
    use crate::actors::web::tests::TempDir;
    use crate::state::SystemState;

    // -- The scripted R10 session --------------------------------------------

    /// MultiLink handle the scripted device grants.
    const EQ_HANDLE: u8 = 0x05;
    /// The scripted device's BLE address (synthetic).
    const EQ_ADDRESS: &str = "F5:D1:00:00:00:01";
    /// Reads that find nothing between two batches of the script.
    const EQ_GAP_READS: u32 = 4;

    /// What the scripted device has left to send, batch by batch.
    #[derive(Default)]
    struct EqWire {
        batches: VecDeque<VecDeque<Vec<u8>>>,
        gap: u32,
    }

    /// A transport playing [`eq_script`]: each batch's chunks back to back,
    /// a few empty reads between batches, then the device is gone (a
    /// transport error). Writes are taken and dropped.
    #[derive(Clone, Default)]
    struct EqFake(Arc<Mutex<EqWire>>);

    impl EqFake {
        fn new() -> Self {
            let fake = Self::default();
            fake.0.lock().unwrap().batches = eq_script().into_iter().map(Into::into).collect();
            fake
        }
    }

    impl tenover::Transport for EqFake {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
            let mut w = self.0.lock().unwrap();
            loop {
                let Some(batch) = w.batches.front_mut() else {
                    return Err(std::io::ErrorKind::NotConnected.into());
                };
                if let Some(chunk) = batch.pop_front() {
                    buf[..chunk.len()].copy_from_slice(&chunk);
                    return Ok(chunk.len());
                }
                if w.gap < EQ_GAP_READS {
                    w.gap += 1;
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
                w.gap = 0;
                w.batches.pop_front();
            }
        }

        fn write(&mut self, _data: &[u8]) -> Result<(), std::io::Error> {
            Ok(())
        }

        fn write_register(&mut self, _data: &[u8]) -> Result<(), std::io::Error> {
            Ok(())
        }
    }

    /// One message from the device, split at MTU 20.
    fn eq_msg(frame: &[u8]) -> Vec<Vec<u8>> {
        tenover::testing::chunks(EQ_HANDLE, frame, 20)
    }

    fn eq_envelope(envelope: &[u8]) -> Vec<Vec<u8>> {
        eq_msg(&tenover::testing::protobuf_frame(envelope))
    }

    /// The session the harness plays: handshake, subscribe, wake (the tee
    /// distance goes out), armed, recording, processing, a full shot, armed
    /// again, a shot with no spin, a device error with its tilt, a warmup
    /// shot without club data; then the device is gone.
    fn eq_script() -> Vec<Vec<Vec<u8>>> {
        use tenover::testing as t;
        let handshake = [
            vec![t::register_response(EQ_HANDLE)],
            eq_msg(&t::device_info_frame()),
            eq_msg(&t::configuration_frame()),
            eq_msg(&t::ack_frame(tenover::gfdi::MSG_CONFIGURATION, 0, &[])),
        ]
        .concat();
        vec![
            handshake,
            eq_envelope(&t::subscribe_result_envelope(true)),
            eq_envelope(&t::wake_result_envelope(0)),
            eq_envelope(&t::environment_result_envelope(true)),
            eq_envelope(&t::state_envelope(2)),
            eq_envelope(&t::state_envelope(3)),
            eq_envelope(&t::state_envelope(4)),
            eq_envelope(EQ_SHOT_FULL),
            eq_envelope(&t::state_envelope(2)),
            eq_envelope(&t::shot_envelope(3, 0.0, 12.0)),
            eq_envelope(&t::error_envelope(3, 0, Some((4.5, -6.25)))),
            eq_envelope(EQ_SHOT_WARMUP),
        ]
    }

    /// tenover's golden shot messages (its `wire_golden` fixtures), as the
    /// R10 sends them: a regular shot with every ball, club and swing value,
    /// and a warmup shot with measured spin and no club data.
    const EQ_SHOT_FULL: &[u8] = &[
        0xf2, 0x01, 0x53, 0x1a, 0x51, 0x08, 0x08, 0xca, 0x3e, 0x4c, 0x12, 0x4a, 0x08, 0x2a, 0x10,
        0x01, 0x1a, 0x1d, 0x0d, 0x00, 0x00, 0x48, 0x41, 0x15, 0x9a, 0x99, 0x99, 0xbf, 0x1d, 0x00,
        0x00, 0x86, 0x42, 0x25, 0x00, 0x00, 0xa0, 0x40, 0x2d, 0x00, 0x00, 0x2f, 0x45, 0x30, 0x00,
        0x38, 0x01, 0x22, 0x14, 0x0d, 0x00, 0x00, 0x30, 0x42, 0x15, 0x00, 0x00, 0xc0, 0x3f, 0x1d,
        0x00, 0x00, 0x00, 0xc0, 0x25, 0x00, 0x00, 0x60, 0xc0, 0x2a, 0x0f, 0x08, 0xe8, 0x07, 0x10,
        0xdc, 0x0b, 0x18, 0x88, 0x0e, 0x20, 0x98, 0x11, 0x28, 0xc4, 0x13,
    ];
    const EQ_SHOT_WARMUP: &[u8] = &[
        0xf2, 0x01, 0x2c, 0x1a, 0x2a, 0x08, 0x08, 0xca, 0x3e, 0x25, 0x12, 0x23, 0x08, 0x07, 0x10,
        0x00, 0x1a, 0x1d, 0x0d, 0x00, 0x00, 0xf2, 0x41, 0x15, 0x00, 0x00, 0x20, 0x40, 0x1d, 0x00,
        0x00, 0xcc, 0x41, 0x25, 0x00, 0x00, 0x20, 0xc1, 0x2d, 0x00, 0x80, 0xbb, 0x45, 0x30, 0x03,
        0x38, 0x02,
    ];

    /// The R10 section the harness runs with (7 ft range: the tee distance
    /// goes out at the wake).
    fn eq_section_json() -> &'static str {
        r#"{"name":"Garmin R10","range":"7ft"}"#
    }

    // -- tenover's driver over the scripted device --------------------------

    /// Scripted devices waiting for a driver, by the card's range (with
    /// its unit; empty for none): each test runs with its own range (see
    /// [`Handoff`]).
    static DEVICES: Handoff<String, VecDeque<EqFake>> = Handoff::new();

    fn range_key(config: &R10Config) -> String {
        config.range.map(|d| format!("{d:?}")).unwrap_or_default()
    }

    /// tenover's [`R10`] with its BLE scan replaced by the scripted devices
    /// queued for its range: a connect runs the real handshake and session
    /// over the next one, and waits for its stop when none is left.
    struct ScriptedR10 {
        inner: R10,
        devices: VecDeque<EqFake>,
    }

    struct ScriptedSession(R10Session<EqFake>);

    impl DeviceDriver for ScriptedR10 {
        type Config = R10Config;
        type Error = tenover::driver::Error;
        type Action = NoAction;
        type Session = ScriptedSession;
        const STOP_WAIT: Duration = R10::STOP_WAIT;

        fn info(config: &R10Config) -> DriverInfo {
            R10::info(config)
        }

        fn new(config: R10Config) -> Result<Self, ConfigError> {
            let devices = DEVICES.take(&range_key(&config)).unwrap_or_default();
            Ok(Self {
                inner: R10::new(config)?,
                devices,
            })
        }

        fn config(&self) -> &R10Config {
            self.inner.config()
        }

        fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<ScriptedSession, Self::Error> {
            ctx.tick()?;
            ctx.out.phase("discovering");
            match self.devices.pop_front() {
                Some(device) => Ok(ScriptedSession(
                    self.inner.connect_with(ctx, device, EQ_ADDRESS)?,
                )),
                None => loop {
                    ctx.tick()?;
                    std::thread::sleep(Duration::from_millis(20));
                },
            }
        }

        fn retry_delay(
            &mut self,
            ended: &SessionEnd<'_, Self::Error>,
            backoff: &mut Backoff,
            out: &mut Events<'_>,
        ) -> Option<Duration> {
            self.inner.retry_delay(ended, backoff, out)
        }

        fn apply_config(&mut self, new: &R10Config) {
            self.inner.apply_config(new);
        }
    }

    impl DeviceSession for ScriptedSession {
        type Driver = ScriptedR10;

        fn poll(
            &mut self,
            timeout: Duration,
            out: &mut Events<'_>,
        ) -> Result<(), tenover::driver::Error> {
            self.0.poll(timeout, out)
        }

        fn identity(&self) -> &DeviceIdentity {
            self.0.identity()
        }

        fn close(self, out: &mut Events<'_>) -> Result<(), tenover::driver::Error> {
            self.0.close(out)
        }
    }

    impl Hooks<ScriptedR10> for R10Hooks {
        type Section = R10Section;
        const SOURCE: ShotSource = ShotSource::R10;
    }

    /// A running R10 card (`r10.0`) on a scripted device, and the bus.
    struct Card {
        _dir: TempDir,
        state: Arc<SystemState>,
        rx: broadcast::Receiver<FlighthookMessage>,
    }

    impl Card {
        fn start(section: &R10Section, device: EqFake) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flighthook-r10-test-{}",
                crate::state::config::generate_id()
            ));
            let (state, _writer) = SystemState::new(dir.join("config.toml"));
            state.system.update(|c| {
                c.r10.insert("0".into(), section.clone());
            });
            let state = Arc::new(state);
            DEVICES.put(range_key(&section.driver), VecDeque::from([device]));
            let (tx, rx) = broadcast::channel(1024);
            crate::actors::start_actor(
                "r10.0".into(),
                Box::new(DriverActor::<ScriptedR10, R10Hooks>::new(section.clone())),
                &state,
                &tx,
            );
            Self {
                _dir: TempDir(dir),
                state,
                rx,
            }
        }

        /// Messages of `r10.0` until one matches `want`, within 10 s.
        fn until(
            &mut self,
            mut want: impl FnMut(&FlighthookMessage) -> bool,
        ) -> Vec<FlighthookMessage> {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut seen = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "timed out: {} seen", seen.len());
                match self.rx.try_recv() {
                    Ok(m) if m.actor == "r10.0" => {
                        let done = want(&m);
                        seen.push(m);
                        if done {
                            return seen;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }

        /// The whole scripted session: every message up to the
        /// Disconnected after the stop that follows the lost connection.
        fn session(&mut self) -> Vec<FlighthookMessage> {
            let mut seen = self.until(|m| is_status(m, ActorStatus::Reconnecting));
            assert_eq!(
                self.state.stop_and_wait("r10.0"),
                crate::state::Stopped::Exited
            );
            seen.extend(self.until(|m| is_status(m, ActorStatus::Disconnected)));
            seen
        }
    }

    fn is_status(m: &FlighthookMessage, want: ActorStatus) -> bool {
        matches!(&m.event, FlighthookEvent::ActorStatus { status, .. } if *status == want)
    }

    fn section(range: Option<flighthook::Distance>) -> R10Section {
        R10Section {
            driver: R10Config {
                range,
                ..R10Config::default()
            },
            ..R10Section::default()
        }
    }

    /// A device that arms and makes the one shot `envelope`.
    fn one_shot(envelope: &[u8]) -> EqFake {
        let mut script = eq_script();
        script.truncate(5);
        script.push(eq_envelope(envelope));
        let fake = EqFake::default();
        fake.0.lock().unwrap().batches = script.into_iter().map(Into::into).collect();
        fake
    }

    /// The bus messages of a card on `device` up to its first shot's end.
    fn first_shot(feet: f64, device: EqFake) -> Vec<FlighthookMessage> {
        let mut card = Card::start(&section(Some(flighthook::Distance::Feet(feet))), device);
        let messages = card.until(|m| matches!(m.event, FlighthookEvent::ShotFinished { .. }));
        card.state.stop_and_wait("r10.0");
        messages
    }

    // -- The equivalence harness ---------------------------------------------

    /// The bus messages and export lines of flighthook a6db5ec's R10 actor
    /// (its own BLE session code on tenover's client) for [`eq_script`],
    /// recorded with this file's scripted device.
    const OLD_BUS: &str = include_str!("../testdata/r10_old_session.jsonl");
    const OLD_EXPORT: &str = include_str!("../testdata/r10_old_export.jsonl");

    fn lines(text: &str) -> Vec<Value> {
        text.lines()
            .map(|l| serde_json::from_str(l).expect("a recorded line"))
            .collect()
    }

    /// `messages` with their shot and group ids numbered in order of
    /// appearance (the ids are random), and each shot's raw frames taken
    /// out (compared on their own): the shot's frames in order.
    fn normalized(mut messages: Vec<Value>) -> (Vec<Value>, Vec<Vec<Value>>) {
        let mut ids: HashMap<String, String> = HashMap::new();
        let mut raws = Vec::new();
        for m in &mut messages {
            renumber(m, &mut ids);
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

    /// Replace every UUID string under `v` by its number of appearance.
    fn renumber(v: &mut Value, ids: &mut HashMap<String, String>) {
        match v {
            Value::String(s) if s.len() == 36 && s.matches('-').count() == 4 => {
                let n = ids.len();
                *s = ids
                    .entry(s.clone())
                    .or_insert_with(|| format!("id-{n}"))
                    .clone();
            }
            Value::Array(a) => a.iter_mut().for_each(|v| renumber(v, ids)),
            Value::Object(o) => o.values_mut().for_each(|v| renumber(v, ids)),
            _ => {}
        }
    }

    fn kind(m: &Value) -> &str {
        m["event"]["kind"].as_str().unwrap_or("")
    }

    fn status_message(status: &str, telemetry: Value) -> Value {
        json!({ "actor": "r10.0", "event": { "kind": "actor_status", "status": status, "telemetry": telemetry } })
    }

    /// What the recorded actor's `messages` become on `DriverActor` with
    /// tenover's driver: each change is one of design §7.3's rows for the
    /// R10, or a rule of `DriverActor` accepted in steps 1 and 2.
    fn expected_from_old(old: &[Value]) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        let mut i = 0;
        let mut device: Option<Value> = None;
        while i < old.len() {
            let mut m = old[i].clone();
            let status = m["event"]["status"].as_str().unwrap_or("").to_owned();
            match kind(&m) {
                // §7.3 #8: the driver's connect phases show on the card
                // while it connects.
                "actor_status" if status == "starting" => {
                    out.push(m);
                    out.push(status_message(
                        "starting",
                        json!({ "phase": "discovering" }),
                    ));
                    out.push(status_message(
                        "starting",
                        json!({ "phase": "handshaking" }),
                    ));
                    i += 1;
                    continue;
                }
                // §7.3 #8: Connected removes the connect's phase; from here
                // on every message carries the session's device id (the
                // sink's rule, step 2).
                "actor_status" if status == "connected" && device.is_none() => {
                    m["event"]["telemetry"]["phase"] = json!("");
                    device = Some(old[i + 1]["device"].clone());
                }
                // Steps 1/2: a shot's `shot_count` goes out after its
                // `shot_finished`, and its launch is held until the shot
                // ends (no flight result from an R10), so it follows the
                // club data.
                "actor_status" if m["event"]["telemetry"].get("shot_count").is_some() => {
                    let end = (i..old.len())
                        .find(|&j| kind(&old[j]) == "shot_finished")
                        .expect("the shot's end");
                    let launch = old[i + 1..end]
                        .iter()
                        .find(|x| kind(x) == "launch")
                        .cloned();
                    for x in &old[i + 1..end] {
                        if kind(x) != "launch" {
                            out.push(x.clone());
                        }
                    }
                    out.extend(launch);
                    out.push(old[end].clone());
                    out.push(m);
                    i = end + 1;
                    continue;
                }
                // tenover sends a device error's tilt telemetry before its
                // alert (the recorded actor: alert first).
                "alert"
                    if old
                        .get(i + 1)
                        .is_some_and(|x| kind(x) == "device_telemetry")
                        && m["event"]["message"]
                            .as_str()
                            .is_some_and(|s| s.starts_with("R10: ")) =>
                {
                    out.push(old[i + 1].clone());
                    out.push(m);
                    i += 2;
                    continue;
                }
                // Step 2: a lost session sends `ready=false` with the rest
                // of the session's telemetry, then Reconnecting, then the
                // driver's per-attempt alert.
                "alert"
                    if m["event"]["message"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("R10 connection error")) =>
                {
                    let mut lost = old[i + 1].clone();
                    let mut merged = out
                        .iter()
                        .rev()
                        .find(|x| kind(x) == "device_telemetry")
                        .map_or_else(|| json!({}), |x| x["event"]["telemetry"].clone());
                    merged["ready"] = json!("false");
                    lost["event"]["telemetry"] = merged;
                    out.push(lost);
                    out.push(old[i + 2].clone());
                    out.push(m);
                    i += 3;
                    continue;
                }
                _ => {}
            }
            out.push(m);
            i += 1;
        }
        if let Some(device) = device {
            let from = out
                .iter()
                .position(|m| kind(m) == "actor_status" && m["event"]["status"] == "connected")
                .expect("connected");
            for m in &mut out[from..] {
                m["device"] = device.clone();
            }
        }
        out
    }

    /// The recorded actor passed the device's spin tilt through with its
    /// sign (+ = left); tenover's driver decodes it as FRP's + = right. Flip
    /// the recorded sidespin and axis of a launch `l` that has spin, and
    /// return whether it had any.
    fn flip_recorded_spin(l: &mut Value) -> bool {
        let spun = spun(l);
        if spun {
            for k in ["sidespin_rpm", "spin_axis"] {
                if let Some(v) = l[k].as_f64() {
                    l[k] = if l[k].is_i64() {
                        json!(-l[k].as_i64().expect("int"))
                    } else {
                        json!(-v)
                    };
                }
            }
        }
        spun
    }

    fn spun(launch: &Value) -> bool {
        launch["total_spin_rpm"].as_i64().is_some_and(|t| t != 0)
    }

    /// A spun launch's simulated flight follows the corrected spin, so it is
    /// compared by the output pins (`outputs_keep_their_numbers`), not here.
    fn without_spun_flight(mut m: Value) -> Value {
        if kind(&m) == "launch"
            && spun(&m["event"]["launch"])
            && let Some(event) = m["event"].as_object_mut()
        {
            event.remove("derived");
        }
        m
    }

    /// The protocol messages in the recorded actor's BLE chunks of direction
    /// `dir` (each led by the MultiLink handle, messages COBS framed between
    /// zero bytes), as hex.
    fn messages_in_chunks(frames: &[Value], dir: &str) -> Vec<String> {
        let mut stream = Vec::new();
        for f in frames.iter().filter(|f| f["dir"] == dir) {
            let bytes = hex(f["hex"].as_str().expect("hex"));
            stream.extend_from_slice(&bytes[1..]);
        }
        stream
            .split(|b| *b == 0)
            .filter(|piece| !piece.is_empty())
            .map(|piece| flighthook::hex_lower(&tenover::cobs::decode(piece).expect("cobs")))
            .collect()
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// §7.3 #1: each shot's raw records hold the same protocol messages as
    /// the recorded actor's BLE chunks, one record per message, labeled
    /// `notify <type>` / `write <type>` instead of a `channel`.
    fn assert_raw_matches(old: &[Vec<Value>], new: &[Vec<Value>]) {
        assert_eq!(old.len(), new.len(), "shots with raw frames");
        for (n, (old, new)) in old.iter().zip(new).enumerate() {
            for (dir, prefix) in [("rx", "notify "), ("tx", "write ")] {
                let want = messages_in_chunks(old, dir);
                let got: Vec<String> = new
                    .iter()
                    .filter(|f| f["dir"] == dir)
                    .map(|f| f["hex"].as_str().expect("hex").to_owned())
                    .collect();
                assert_eq!(got, want, "shot {n} {dir}");
                for f in new.iter().filter(|f| f["dir"] == dir) {
                    let label = f["label"].as_str().expect("a label");
                    assert!(label.starts_with(prefix), "shot {n}: {label}");
                    assert!(f.get("channel").is_none() && f.get("type").is_none());
                }
            }
            assert!(old.iter().all(|f| f.get("label").is_none()));
        }
    }

    /// Design §7.2's equivalence harness: the recorded R10 actor and
    /// the R10 card on tenover's driver, fed the same scripted device, put
    /// the same messages on the bus and export the same shots, except for
    /// the differences `expected_from_old` lists (design §7.3 rows 1 and 8
    /// and `DriverActor`'s accepted rules); the raw records hold the same
    /// protocol messages per shot (§7.3 #1), the tee distance among them.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_r10_card_matches_the_recorded_actor() {
        let section: R10Section = serde_json::from_str(eq_section_json()).expect("section");
        let mut card = Card::start(&section, EqFake::new());
        let seen = card.session();

        let (old, old_raws) = normalized(lines(OLD_BUS));
        let (new, new_raws) = normalized(
            seen.iter()
                .map(|m| serde_json::to_value(m).expect("json"))
                .collect(),
        );
        let mut expected = expected_from_old(&old);
        for m in expected.iter_mut().filter(|m| kind(m) == "launch") {
            flip_recorded_spin(&mut m["event"]["launch"]);
        }
        for (n, (want, got)) in expected.iter().zip(&new).enumerate() {
            assert_eq!(
                without_spun_flight(got.clone()),
                without_spun_flight(want.clone()),
                "message {n}"
            );
        }
        assert_eq!(
            new.len(),
            expected.len(),
            "{:?}",
            new.iter().map(kind).collect::<Vec<_>>()
        );
        assert_raw_matches(&old_raws, &new_raws);
        let launches: Vec<&Value> = new.iter().filter(|m| kind(m) == "launch").collect();
        assert_eq!(launches[1]["event"]["launch"]["total_spin_rpm"], 0);
        assert!(
            launches[1]["event"]["launch"].get("spin_axis").is_none(),
            "a total of 0 has no axis"
        );
        let environment = tenover::proto::build_environment_request(&tenover::proto::Environment {
            tee_distance_yd: section.driver.tee_distance_yd(),
            ..tenover::proto::Environment::default()
        });
        assert!(
            new_raws[0].iter().any(|f| f["dir"] == "tx"
                && f["hex"]
                    .as_str()
                    .is_some_and(|h| h.contains(&flighthook::hex_lower(&environment)))),
            "the tee distance went out at the wake"
        );

        // The export: the same lines but for the schema (§7.3 #1) and the
        // raw records (compared above).
        let config = card.state.system.snapshot();
        let export = crate::actors::web::tests::export_lines(&config, &seen).await;
        let (old_export, old_export_raws) = normalized(lines(OLD_EXPORT));
        let (new_export, new_export_raws) = normalized(
            export
                .iter()
                .map(|l| serde_json::from_str(l).expect("json"))
                .collect(),
        );
        assert_eq!(new_export.len(), old_export.len());
        for (mut want, mut got) in old_export.into_iter().zip(new_export) {
            assert_eq!(want["schema"], 2);
            want["schema"] = json!(flighthook::EXPORT_SCHEMA);
            want["time"] = got["time"].clone();
            want["flighthook_version"] = got["flighthook_version"].clone();
            if flip_recorded_spin(&mut want["data"]["launch"]) {
                for line in [&mut want, &mut got] {
                    if let Some(data) = line["data"].as_object_mut() {
                        data.remove("derived");
                        data.remove("metrics");
                    }
                }
            }
            assert_eq!(got, want);
        }
        assert_raw_matches(&old_export_raws, &new_export_raws);
    }

    /// A range the driver refuses (from the file) keeps the card from
    /// starting and shows on it (design §7.3 #6, #15); the card still stops
    /// at once.
    #[test]
    fn a_refused_range_shows_on_the_card() {
        let mut card = Card::start(
            &section(Some(flighthook::Distance::Feet(0.0))),
            EqFake::new(),
        );
        let seen = card.until(|m| is_status(m, ActorStatus::Disconnected));
        assert_eq!(seen.len(), 1, "no Starting, no connect");
        assert!(
            matches!(&seen[0].event, FlighthookEvent::ActorStatus { telemetry, .. }
            if telemetry["error"] == "range: must be a distance greater than 0")
        );
        let at = Instant::now();
        assert_eq!(
            card.state.stop_and_wait("r10.0"),
            crate::state::Stopped::Exited
        );
        assert!(at.elapsed() < Duration::from_secs(2));
    }

    // -- Shot values and outputs ---------------------------------------------

    /// Every output's numbers for the R10's shot messages (tenover's golden
    /// shots), through the card on tenover's driver.
    #[test]
    fn outputs_keep_their_numbers() {
        for (case, envelope, feet) in [
            ("r10", EQ_SHOT_FULL, 6.0),
            ("r10_warmup", EQ_SHOT_WARMUP, 6.5),
        ] {
            let messages = first_shot(feet, one_shot(envelope));
            crate::test_ball::assert_outputs_pinned(case, &messages);
        }
    }

    /// The R10 measures total spin and axis; the back and side spin tenover
    /// hands back are the same spin in the other pair, not a model's: the
    /// bus launch carries all four, they agree, and a tournament round
    /// keeps them as measured.
    #[test]
    fn a_round_keeps_all_four_r10_spin_values() {
        let messages = first_shot(5.0, one_shot(EQ_SHOT_FULL));
        let launch = messages
            .iter()
            .find_map(|m| match &m.event {
                FlighthookEvent::Launch { launch, .. } => Some((**launch).clone()),
                _ => None,
            })
            .expect("a launch");
        assert_eq!(launch.total_spin_rpm, Some(2800));
        // The golden shot's wire tilt of 5 is 5° left.
        assert_eq!(launch.spin_axis, Some(-5.0));
        let mut components = flighthook::Launch {
            backspin_rpm: launch.backspin_rpm,
            sidespin_rpm: launch.sidespin_rpm,
            ..flighthook::Launch::default()
        };
        components.complete_spin();
        assert_eq!(components.total_spin_rpm, launch.total_spin_rpm);
        assert!((components.spin_axis.expect("axis") + 5.0).abs() < 0.05);

        let round = crate::state::round::RoundPolicy::default();
        let guard = round
            .begin_round(&crate::state::round::RoundRequest {
                holder: "Pureplay",
                feeds: &[],
            })
            .expect("accepted");
        let shot = flighthook::ShotData {
            launch: Some(launch.clone()),
            provenance: Some(ShotSource::R10.provenance()),
            ..flighthook::ShotData::default()
        };
        assert_eq!(shot.unmeasured_fields().count(), 0);
        let certified = guard.certify(&shot).expect("measured");
        assert_eq!(certified.shot().launch.as_ref(), Some(&launch));
    }
}
