use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tenover::ble::BleTransport;
use tenover::capture::IdentityFilter;
use tenover::{Client, Event};
use tracing::{debug, info, warn};

use super::{Actor, ReconfigureOutcome};
use crate::bus::{BusReceiver, BusSender, PollError};
use crate::raw_capture::{self, RawCapture, RawCaptureConfig};
use crate::sim;
use crate::state::SystemState;

use flighthook::{
    ActorStatus, BallFlight, ClubData, FlighthookEvent, FlighthookMessage, RawDir, RawRecord,
    Severity, ShotDetectionMode, ShotField, ShotKey, Velocity,
};

/// Reconnect backoff bounds (linear: +1s per attempt, capped at 15s).
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(15);

/// Garmin R10 BLE actor. Connects via BLE auto-discovery, runs the
/// MultiLink/GFDI/protobuf handshake, and processes shots.
pub struct R10Actor {
    pub initial_mode: ShotDetectionMode,
    /// Tee distance to push to the device after wake-up, in yards — the unit
    /// the R10 protocol expects. `None` leaves the device's own setting alone.
    pub tee_range_yards: Option<f32>,
}

impl Actor for R10Actor {
    fn start(
        &self,
        state: Arc<SystemState>,
        sender: BusSender,
        receiver: BusReceiver,
    ) -> std::thread::JoinHandle<()> {
        let initial_mode = self.initial_mode;
        let tee_range_yards = self.tee_range_yards;
        let thread_name = format!("device:{}", sender.actor_id());

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run(&state, initial_mode, tee_range_yards, sender, receiver);
            })
            .expect("failed to spawn r10 thread")
    }

    fn reconfigure(&self, state: &Arc<SystemState>, sender: &BusSender) -> ReconfigureOutcome {
        let actor_id = sender.actor_id();
        let Some((_, index)) = actor_id.split_once('.') else {
            return ReconfigureOutcome::Applied;
        };

        let snap = state.system.snapshot();
        match snap.r10.get(index) {
            // Tee distance is pushed once, right after the device wakes up, so
            // a change only takes effect on the next connection.
            Some(section) => {
                #[allow(clippy::cast_possible_truncation)]
                let yards = section.range.map(|d| d.as_yards() as f32);
                if yards == self.tee_range_yards {
                    ReconfigureOutcome::Applied
                } else {
                    ReconfigureOutcome::RestartRequired
                }
            }
            None => ReconfigureOutcome::RestartRequired, // section removed
        }
    }
}

// ---------------------------------------------------------------------------
// Raw capture
// ---------------------------------------------------------------------------

/// Raw capture for one R10 connection. Chunks are recorded as they cross the
/// transport, MultiLink handle byte included. The pre-roll holds the shot
/// message and the device's replays of it at any MTU.
fn raw_capture_config() -> RawCaptureConfig {
    RawCaptureConfig {
        transport: "r10-ble",
        pre_roll_frames: 256,
        max_frames: 256,
        max_bulk_bytes: 0,
        exclude: raw_capture::exclude_none,
        is_bulk: raw_capture::no_bulk,
    }
}

/// Transport wrapper that records chunks for the shot export. The capture
/// starts disabled and is enabled once the handshake (which carries the
/// device's unit ID) is complete; after that, chunks of identity-bearing
/// GFDI messages are filtered out by type.
struct Tap<T> {
    inner: T,
    capture: RawCapture,
    rx: IdentityFilter<(Instant, RawRecord)>,
    tx: IdentityFilter<(Instant, RawRecord)>,
}

impl<T> Tap<T> {
    fn new(inner: T) -> Self {
        let mut capture = RawCapture::new(raw_capture_config());
        capture.set_enabled(false);
        Self {
            inner,
            capture,
            rx: IdentityFilter::new(),
            tx: IdentityFilter::new(),
        }
    }

    /// Record a GFDI data chunk once its frames are classified.
    fn record(&mut self, dir: RawDir, channel: &str, data: &[u8]) {
        let rec = RawRecord {
            channel: Some(channel.into()),
            ..RawRecord::binary(dir, data)
        };
        let filter = match dir {
            RawDir::Rx => &mut self.rx,
            RawDir::Tx => &mut self.tx,
        };
        for (at, rec) in filter.feed(data, (Instant::now(), rec)) {
            self.capture.push_at(rec, at);
        }
    }
}

impl<T: tenover::Transport> tenover::Transport for Tap<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.record(RawDir::Rx, "notify", &buf[..n]);
        }
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), io::Error> {
        self.inner.write(data)?;
        self.record(RawDir::Tx, "write", data);
        Ok(())
    }

    /// MultiLink control plane (service registration): no GFDI frames.
    fn write_register(&mut self, data: &[u8]) -> Result<(), io::Error> {
        self.inner.write_register(data)?;
        self.capture.push(RawRecord {
            channel: Some("register".into()),
            ..RawRecord::binary(RawDir::Tx, data)
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn emit_device_status(sender: &BusSender, status: ActorStatus, telemetry: HashMap<String, String>) {
    sender.send(FlighthookMessage::new(FlighthookEvent::ActorStatus {
        status,
        telemetry,
    }));
}

fn emit_alert(sender: &BusSender, severity: Severity, message: impl Into<String>) {
    sender.send(FlighthookMessage::new(FlighthookEvent::Alert {
        severity,
        message: message.into(),
    }));
}

// ---------------------------------------------------------------------------
// Protocol type -> bus type conversion helpers
// ---------------------------------------------------------------------------

/// The values of [`ball_from_r10`] the R10 does not report: tenover
/// computes back and side spin from the measured total and axis.
const R10_COMPUTED: [ShotField; 2] = [ShotField::BackspinRpm, ShotField::SidespinRpm];

/// The R10 measures total spin and spin axis; tenover also hands back the
/// back/side components computed from them, so both FRP spin pairs are sent
/// and the components are marked computed ([`R10_COMPUTED`]). A total of 0
/// has no axis.
fn ball_from_r10(b: &tenover::proto::BallData) -> BallFlight {
    #[allow(clippy::cast_possible_truncation)]
    let total_spin = b.total_spin.round() as i32;
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(f64::from(b.ball_speed))),
        launch_elevation: Some(f64::from(b.launch_angle)),
        launch_azimuth: Some(f64::from(b.launch_direction)),
        // R10 doesn't compute carry, apex, flight time or roll.
        #[allow(clippy::cast_possible_truncation)]
        backspin_rpm: Some(b.backspin.round() as i32),
        #[allow(clippy::cast_possible_truncation)]
        sidespin_rpm: Some(b.sidespin.round() as i32),
        total_spin_rpm: Some(total_spin),
        spin_axis: (total_spin != 0).then_some(f64::from(b.spin_axis)),
        ..BallFlight::default()
    }
}

fn club_from_r10(c: &tenover::proto::ClubData) -> ClubData {
    ClubData {
        club_speed: Some(Velocity::MetersPerSecond(f64::from(c.club_head_speed))),
        club_speed_post: None,
        path: Some(f64::from(c.path_angle)),
        attack_angle: Some(f64::from(c.attack_angle)),
        face_angle: Some(f64::from(c.face_angle)),
        dynamic_loft: None,
        smash_factor: None,
        swing_plane_horizontal: None,
        swing_plane_vertical: None,
        club_offset: None,
        club_height: None,
        ..ClubData::default()
    }
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

#[allow(clippy::needless_pass_by_value)]
fn run(
    state: &SystemState,
    _initial_mode: ShotDetectionMode,
    tee_range_yards: Option<f32>,
    sender: BusSender,
    mut receiver: BusReceiver,
) {
    let mut backoff = MIN_BACKOFF;
    let mut ever_connected = false;
    let mut device_id: Option<String> = None;

    loop {
        if receiver.poll().is_err() {
            break;
        }

        emit_device_status(&sender, ActorStatus::Starting, HashMap::new());

        match connect_and_run(
            state,
            &sender,
            &mut receiver,
            &mut ever_connected,
            &mut device_id,
            tee_range_yards,
        ) {
            Ok(()) => break,
            Err(e) => {
                warn!("session error: {e}");
                emit_alert(
                    &sender,
                    Severity::Warn,
                    format!("R10 connection error: {e}"),
                );
                let backoff_status = if ever_connected {
                    ActorStatus::Reconnecting
                } else {
                    ActorStatus::Starting
                };
                // Not ready before the status changes: the status drops what
                // the card held, so nothing of the lost device is left to
                // report after it.
                if let Some(dev) = device_id.as_deref() {
                    sender.send(
                        FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                            manufacturer: None,
                            model: None,
                            firmware: None,
                            telemetry: Some(HashMap::from([("ready".into(), "false".into())])),
                        })
                        .device(dev),
                    );
                }
                emit_device_status(&sender, backoff_status, HashMap::new());

                let verb = if ever_connected {
                    "Reconnecting"
                } else {
                    "Retrying"
                };
                info!("{} in {}s", verb.to_lowercase(), backoff.as_secs());

                let deadline = Instant::now() + backoff;
                while Instant::now() < deadline {
                    if receiver.poll().is_err() {
                        emit_device_status(&sender, ActorStatus::Disconnected, HashMap::new());
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                backoff = (backoff + Duration::from_secs(1)).min(MAX_BACKOFF);
            }
        }
    }

    emit_device_status(&sender, ActorStatus::Disconnected, HashMap::new());
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn connect_and_run(
    state: &SystemState,
    sender: &BusSender,
    receiver: &mut BusReceiver,
    ever_connected: &mut bool,
    device_id: &mut Option<String>,
    tee_range_yards: Option<f32>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. BLE connect
    info!("searching for Garmin R10...");
    let transport = BleTransport::auto_connect()?;
    *ever_connected = true;

    let addr = transport.device_address().to_string();
    let mtu = transport.mtu();
    info!("connected to R10 at {addr} (mtu={mtu})");

    // Capture BLE address as FRP device identity
    *device_id = Some(addr.clone());

    // 2. Create client and start registration
    let mut client = Client::new(Tap::new(transport), mtu.into());
    client.start()?;

    // 3. Event loop state
    let mut shot_counter: u32 = 0;
    let mut idle_count: u32 = 0;
    let mut device_telemetry: HashMap<String, String> = HashMap::new();

    loop {
        // ==============================================================
        // Phase 1: Drain bus commands
        // ==============================================================
        loop {
            match receiver.poll() {
                Err(PollError::Shutdown) => {
                    drop(client);
                    return Ok(());
                }
                Ok(None) => break,
                Ok(Some(msg)) => {
                    // R10 doesn't support mode switching — just log
                    if let FlighthookEvent::SetDetectionMode {
                        mode: Some(new_mode),
                        ..
                    } = msg.event
                    {
                        debug!("mode change to {new_mode:?} (ignored — R10 has no mode control)");
                    }
                }
            }
        }

        // ==============================================================
        // Phase 2: Drive R10 client
        // ==============================================================
        match client.poll() {
            Ok(Some(event)) => {
                idle_count = 0;

                match event {
                    Event::Registered { handle } => {
                        info!("MultiLink registered (handle={handle})");
                    }

                    Event::HandshakeComplete => {
                        info!("handshake complete");
                        // The unit ID travelled in the handshake; capture
                        // from here on.
                        client.transport_mut().capture.set_enabled(true);
                        emit_device_status(
                            sender,
                            ActorStatus::Connected,
                            HashMap::from([
                                ("device_info".into(), format!("Garmin R10 ({addr})")),
                                ("shot_count".into(), shot_counter.to_string()),
                            ]),
                        );

                        // Emit identity DeviceTelemetry
                        device_telemetry.insert("ready".into(), "false".into());
                        device_telemetry.insert("tilt".into(), "ok".into());
                        device_telemetry.insert("roll".into(), "ok".into());
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: Some("Garmin".into()),
                                model: Some("R10".into()),
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&addr),
                        );
                    }

                    Event::Ready => {
                        info!("armed — waiting for shots");
                        device_telemetry.insert("ready".into(), "true".into());
                        device_telemetry.insert("device_state".into(), "waiting".into());
                        device_telemetry.insert("tilt".into(), "ok".into());
                        device_telemetry.insert("roll".into(), "ok".into());
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: None,
                                model: None,
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&addr),
                        );
                    }

                    Event::Shot(shot) => {
                        shot_counter += 1;
                        let key = ShotKey {
                            shot_id: uuid::Uuid::new_v4().to_string(),
                            shot_number: shot_counter,
                        };
                        info!("shot #{} (device_id={})", key.shot_number, shot.shot_id);
                        client.transport_mut().capture.mark_trigger();

                        emit_device_status(
                            sender,
                            ActorStatus::Connected,
                            HashMap::from([("shot_count".into(), shot_counter.to_string())]),
                        );

                        // Emit ShotTrigger
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::ShotTrigger {
                                key: key.clone(),
                            })
                            .device(&addr),
                        );

                        // Emit BallFlight if ball data present
                        if let Some(ref ball) = shot.ball {
                            let bf = ball_from_r10(ball);
                            info!(
                                "  ball: {:.1}mph VLA={:.1} HLA={:.1} back={:.0}rpm side={:.0}rpm",
                                bf.launch_speed.map_or(0.0, Velocity::as_mph),
                                bf.launch_elevation.unwrap_or(0.0),
                                bf.launch_azimuth.unwrap_or(0.0),
                                bf.backspin_rpm.unwrap_or(0),
                                bf.sidespin_rpm.unwrap_or(0),
                            );
                            let mut event = sim::ball_flight_event(state, key.clone(), bf);
                            sim::mark_computed(&mut event, &R10_COMPUTED);
                            sender.send(FlighthookMessage::new(event).device(&addr));
                        }

                        // Emit ClubPath if club data present
                        if let Some(ref club) = shot.club {
                            let cd = club_from_r10(club);
                            info!(
                                "  club: {:.1}mph face={:.1} path={:.1} AoA={:.1}",
                                cd.club_speed.map_or(0.0, Velocity::as_mph),
                                cd.face_angle.unwrap_or(0.0),
                                cd.path.unwrap_or(0.0),
                                cd.attack_angle.unwrap_or(0.0),
                            );
                            sender.send(
                                FlighthookMessage::new(FlighthookEvent::ClubPath {
                                    key: key.clone(),
                                    club: Box::new(cd),
                                })
                                .device(&addr),
                            );
                        }

                        // Emit ShotFinished
                        let raw = client.transport_mut().capture.take();
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::ShotFinished {
                                key,
                                raw: Some(Box::new(raw)),
                            })
                            .device(&addr),
                        );

                        // R10 auto-returns to WAITING after processing
                        device_telemetry.insert("ready".into(), "false".into());
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: None,
                                model: None,
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&addr),
                        );
                    }

                    Event::StateChange(state) => {
                        // Only surface user-relevant states as telemetry.
                        // Standby and NoiseCheck are transient internal
                        // states that flicker during shot retransmits.
                        let label = match state {
                            tenover::proto::DeviceState::Waiting => Some("waiting"),
                            tenover::proto::DeviceState::Recording => Some("recording"),
                            tenover::proto::DeviceState::Processing => Some("processing"),
                            _ => None,
                        };
                        debug!("device state: {state:?}");
                        if let Some(label) = label {
                            device_telemetry.insert("device_state".into(), label.into());
                            sender.send(
                                FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                    manufacturer: None,
                                    model: None,
                                    firmware: None,
                                    telemetry: Some(device_telemetry.clone()),
                                })
                                .device(&addr),
                            );
                        }
                    }

                    Event::DeviceError(err) => {
                        let severity = match err.severity {
                            tenover::proto::ErrorSeverity::Warning => Severity::Warn,
                            tenover::proto::ErrorSeverity::Serious
                            | tenover::proto::ErrorSeverity::Fatal => Severity::Error,
                        };
                        let mut message = format!("R10: {:?}", err.code);
                        if let Some((roll, pitch)) = err.tilt {
                            use std::fmt::Write;
                            let _ = write!(message, " (roll={roll:.1}, pitch={pitch:.1})");
                            device_telemetry.insert("tilt".into(), format!("{pitch:.1}"));
                            device_telemetry.insert("roll".into(), format!("{roll:.1}"));
                        }
                        warn!("{message}");
                        emit_alert(sender, severity, &message);
                        sender.send(
                            FlighthookMessage::new(FlighthookEvent::DeviceTelemetry {
                                manufacturer: None,
                                model: None,
                                firmware: None,
                                telemetry: Some(device_telemetry.clone()),
                            })
                            .device(&addr),
                        );
                    }

                    // The device is awake, so it will accept its environment
                    // settings (tenover's SEQUENCE.md step 15). Skipped
                    // entirely when unset, which leaves whatever tee distance
                    // the device already had.
                    Event::WakeResult { .. } => {
                        if let Some(yards) = tee_range_yards {
                            let config = tenover::proto::Environment {
                                tee_distance_yd: Some(yards),
                                ..tenover::proto::Environment::default()
                            };
                            if let Err(e) = client.send_environment(&config) {
                                warn!("failed to send tee distance: {e}");
                            } else {
                                debug!("sent tee distance {yards:.1} yd");
                            }
                        }
                    }

                    Event::EnvironmentResult { success } => {
                        if success {
                            info!(
                                "device accepted tee distance {:.1} yd",
                                tee_range_yards.unwrap_or_default()
                            );
                        } else {
                            warn!("device rejected the tee distance");
                        }
                    }

                    // Protocol ack with nothing for the bus to carry.
                    Event::Subscribed { .. } => {}
                }
            }

            Ok(None) => {
                idle_count += 1;
                if idle_count >= 3 {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }

            Err(tenover::Error::Transport(e)) => {
                info!("BLE transport error: {e}");
                return Err(Box::new(tenover::Error::Transport(e)));
            }
            Err(e) => {
                warn!("protocol error: {e}");
                emit_alert(sender, Severity::Warn, format!("R10 protocol error: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tenover::Transport;
    use tenover::gfdi::{
        MSG_DEVICE_INFO, MSG_PROTOBUF_REQUEST as REQUEST, MSG_PROTOBUF_RESPONSE as RESPONSE,
    };
    use tenover::testing::{self, encode_frame as wire, with_handle};

    /// MultiLink handle the fake device uses.
    const HANDLE: u8 = 0x05;

    /// Transport that serves queued chunks and accepts every write.
    #[derive(Default)]
    struct FakeTransport {
        chunks: std::collections::VecDeque<Vec<u8>>,
    }

    impl Transport for FakeTransport {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
            let Some(c) = self.chunks.pop_front() else {
                return Err(io::ErrorKind::WouldBlock.into());
            };
            buf[..c.len()].copy_from_slice(&c);
            Ok(c.len())
        }

        fn write(&mut self, _data: &[u8]) -> Result<(), io::Error> {
            Ok(())
        }

        fn write_register(&mut self, _data: &[u8]) -> Result<(), io::Error> {
            Ok(())
        }
    }

    /// `bytes` split into MTU-sized chunks, each led by the handle byte.
    fn chunks(bytes: &[u8], mtu: usize) -> Vec<Vec<u8>> {
        testing::chunks(HANDLE, bytes, mtu)
    }

    fn tap_with(rx: Vec<Vec<u8>>) -> Tap<FakeTransport> {
        let mut tap = Tap::new(FakeTransport {
            chunks: rx.into_iter().collect(),
        });
        tap.capture.set_enabled(true);
        tap
    }

    fn read_all(tap: &mut Tap<FakeTransport>) {
        let mut buf = [0u8; 256];
        while tap.read(&mut buf).is_ok() {}
    }

    #[test]
    fn chunks_before_enable_are_dropped() {
        let mut tap = Tap::new(FakeTransport {
            chunks: chunks(&wire(RESPONSE, b"shot"), 64).into_iter().collect(),
        });
        read_all(&mut tap);
        tap.write(&chunks(&wire(REQUEST, b"req"), 64)[0])
            .expect("write");
        tap.write_register(&[0x00, 0x01]).expect("register");
        tap.capture.mark_trigger();
        assert!(tap.capture.take().frames.is_empty());
    }

    #[test]
    fn channels_are_labelled() {
        let rx = with_handle(HANDLE, &wire(RESPONSE, &[0x10, 0x20]));
        let tx = with_handle(HANDLE, &wire(REQUEST, &[0x30]));
        let mut tap = tap_with(vec![rx.clone()]);
        read_all(&mut tap);
        tap.write(&tx).expect("write");
        tap.write_register(&[0x00, 0x04]).expect("register");
        tap.capture.mark_trigger();
        let raw = tap.capture.take();
        let got: Vec<(RawDir, &str, &str)> = raw
            .frames
            .iter()
            .map(|r| {
                (
                    r.dir,
                    r.channel.as_deref().unwrap_or(""),
                    r.hex.as_deref().unwrap_or(""),
                )
            })
            .collect();
        let (rx, tx) = (flighthook::hex_lower(&rx), flighthook::hex_lower(&tx));
        assert_eq!(
            got,
            [
                (RawDir::Rx, "notify", rx.as_str()),
                (RawDir::Tx, "write", tx.as_str()),
                (RawDir::Tx, "register", "0004"),
            ]
        );
        assert!(raw.frames.iter().all(|r| r.r#type.is_none()));
        assert_eq!(raw.transport, "r10-ble");
    }

    #[test]
    fn pre_roll_is_bounded() {
        let rx = (0..300u16)
            .map(|i| with_handle(HANDLE, &wire(RESPONSE, &i.to_be_bytes())))
            .collect();
        let mut tap = tap_with(rx);
        read_all(&mut tap);
        tap.capture.mark_trigger();
        let raw = tap.capture.take();
        assert_eq!(raw.frames.len(), 256);
        let first = with_handle(HANDLE, &wire(RESPONSE, &44u16.to_be_bytes()));
        assert_eq!(raw.frames[0].hex, Some(flighthook::hex_lower(&first)));
    }

    #[test]
    fn device_info_after_the_handshake_is_never_captured() {
        // A shot message, a repeated device information message and another
        // shot message, packed back to back and split at MTU 20 so chunks
        // straddle frames.
        let (shot1, info, shot2) = (
            wire(RESPONSE, &[0x11; 30]),
            testing::device_info_frame(),
            wire(RESPONSE, &[0x22; 30]),
        );
        let stream = [shot1.clone(), info.clone(), shot2].concat();
        let rx = chunks(&stream, 20);
        let mut tap = tap_with(rx.clone());
        read_all(&mut tap);
        // Our reply to it, and the ACK of a shot message.
        for chunk in chunks(&testing::ack_frame(MSG_DEVICE_INFO, 0, &[0xAA, 0xBB]), 20) {
            tap.write(&chunk).expect("write");
        }
        let shot_ack = with_handle(HANDLE, &testing::ack_frame(RESPONSE, 0, &[]));
        tap.write(&shot_ack).expect("write");
        tap.capture.mark_trigger();
        let raw = tap.capture.take();

        // Exactly the chunks holding no byte of the device information frame
        // (its COBS bytes and closing delimiter) are kept, in order.
        let info_bytes = shot1.len() + 1..shot1.len() + info.len();
        let expected: Vec<String> = rx
            .iter()
            .enumerate()
            .filter(|(k, _)| {
                let (lo, hi) = (k * 19, (k + 1) * 19);
                hi <= info_bytes.start || lo >= info_bytes.end
            })
            .map(|(_, c)| flighthook::hex_lower(c))
            .collect();
        assert!(expected.len() >= 4 && expected.len() < rx.len());
        let got: Vec<String> = raw
            .frames
            .iter()
            .filter(|r| r.dir == RawDir::Rx)
            .filter_map(|r| r.hex.clone())
            .collect();
        assert_eq!(got, expected);
        let tx: Vec<String> = raw
            .frames
            .iter()
            .filter(|r| r.dir == RawDir::Tx)
            .filter_map(|r| r.hex.clone())
            .collect();
        assert_eq!(
            tx,
            [flighthook::hex_lower(&shot_ack)],
            "ACK of device information dropped"
        );

        let all = raw
            .frames
            .iter()
            .filter_map(|r| r.hex.as_deref())
            .collect::<String>();
        assert!(
            !all.contains(&flighthook::hex_lower(
                &testing::DEVICE_INFO_UNIT_ID.to_le_bytes()
            )),
            "unit ID leaked"
        );
        assert!(
            !all.contains(&flighthook::hex_lower(testing::DEVICE_INFO_NAME.as_bytes())),
            "name leaked"
        );
    }

    #[test]
    fn unclassifiable_frames_are_excluded() {
        let mut bad = wire(RESPONSE, b"shot");
        let n = bad.len();
        bad[n - 2] ^= 0x01; // corrupt the CRC
        let mut tap = tap_with(vec![with_handle(HANDLE, &bad)]);
        read_all(&mut tap);
        tap.capture.mark_trigger();
        assert!(tap.capture.take().frames.is_empty());
    }

    fn r10_ball(total_spin: f32, spin_axis: f32) -> tenover::proto::BallData {
        let axis = spin_axis.to_radians();
        tenover::proto::BallData {
            launch_angle: 14.0,
            launch_direction: 1.5,
            ball_speed: 60.0,
            spin_axis,
            total_spin,
            backspin: total_spin * axis.cos(),
            sidespin: total_spin * axis.sin(),
            spin_calc_type: tenover::proto::SpinCalcType::Measured,
        }
    }

    /// The R10's measured total spin and axis pass through with the
    /// components, and the two pairs agree. Positive axis = curves right,
    /// with positive sidespin.
    #[test]
    fn r10_sends_total_spin_and_axis() {
        for axis in [8.0_f32, -8.0] {
            let bf = ball_from_r10(&r10_ball(3231.0, axis));
            assert_eq!(bf.total_spin_rpm, Some(3231));
            assert_eq!(bf.spin_axis, Some(f64::from(axis)));
            let side = bf.sidespin_rpm.expect("side");
            assert_eq!(side.signum(), axis.signum() as i32, "{axis}: {side}");
            let mut components = BallFlight {
                backspin_rpm: bf.backspin_rpm,
                sidespin_rpm: bf.sidespin_rpm,
                ..BallFlight::default()
            };
            components.complete_spin();
            assert_eq!(components.total_spin_rpm, bf.total_spin_rpm);
            assert!((components.spin_axis.unwrap() - f64::from(axis)).abs() < 0.05);
        }
    }

    /// The R10 reports total spin and axis; the components tenover works
    /// out from them are the ones marked computed.
    #[test]
    fn r10_marks_its_spin_components_computed() {
        assert_eq!(
            R10_COMPUTED,
            [ShotField::BackspinRpm, ShotField::SidespinRpm]
        );
        let bf = ball_from_r10(&r10_ball(3231.0, 8.0));
        assert!(bf.backspin_rpm.is_some() && bf.sidespin_rpm.is_some());
    }

    #[test]
    fn r10_zero_spin_has_no_axis() {
        let bf = ball_from_r10(&r10_ball(0.0, 12.0));
        assert_eq!(bf.total_spin_rpm, Some(0));
        assert_eq!(bf.spin_axis, None);
    }
}
