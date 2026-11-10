//! What the `/frp` endpoint sends: the bus, with FRP 0.2.0's forwarding
//! rules applied to its FRP events.
//!
//! flighthook forwards its devices' shots to every `/frp` client, so each
//! connection passes the bus through one [`FrpOutput`]:
//!
//! - **Duplicates are dropped** with flightrelay's `ForwardFilter`: a shot
//!   event passes once per (device, shot id, kind) within the window
//!   ([`SHOT_TIMEOUT`](flighthook::SHOT_TIMEOUT), flighthook's accumulator
//!   timeout, so a repeat of a shot flighthook still holds never passes),
//!   and `device_telemetry` only when it changes something. When an actor's
//!   status leaves Connected (or it starts again), what was forwarded for its
//!   devices is forgotten, as a client forgets what it held: the next
//!   session's telemetry goes out in full.
//! - **`launch` holds measured values only.** A launch value flighthook
//!   estimated (the launch-only chip spin, `DerivedFlight::spin_estimated`)
//!   is left out of it; `derived`, flighthook's extension field, is kept.
//! - **flighthook adds its own flight result** for a shot whose device sent
//!   none, when it simulated one: `basis: modeled`, `model`
//!   [`FLIGHT_MODEL`](flighthook::FLIGHT_MODEL), the simulated values, and in
//!   `assumed` the launch values it estimated. It goes out when the shot's
//!   `shot_finished` arrives, just before it, and only when no
//!   `flight_result` was forwarded for the shot.
//!
//! Every other bus event, flighthook's extensions included, passes unchanged.
//! Alerts pass every time, a device's too: each one is raised anew by one of
//! flighthook's actors, and flighthook takes no FRP events in, so none can
//! come back around a loop. flighthook sends no `raw_frame`.

use std::collections::HashMap;
use std::time::Instant;

use flighthook::{
    ActorStatus, FlightResult, FlighthookEvent, FlighthookMessage, Provenance, SHOT_TIMEOUT,
    measured_launch,
};
use flightrelay::{ForwardFilter, FrpEnvelope, FrpEvent, Source};

/// One `/frp` connection's forwarding state.
pub(crate) struct FrpOutput {
    filter: ForwardFilter,
    /// The flight result flighthook may add for each shot whose launch went
    /// out, by (device, shot id), with when the launch went out.
    own: HashMap<(String, String), (Instant, Option<FlightResult>)>,
    /// The FRP devices each actor has sent for.
    devices: HashMap<String, Vec<String>>,
}

impl FrpOutput {
    pub(crate) fn new() -> Self {
        Self {
            filter: ForwardFilter::new().with_window(SHOT_TIMEOUT),
            own: HashMap::new(),
            devices: HashMap::new(),
        }
    }

    /// The messages to send for the bus message `msg`, in order: none (a
    /// duplicate), `msg` itself, `msg` with its launch reduced to measured
    /// values, or flighthook's own flight result followed by the shot's
    /// `shot_finished`.
    pub(crate) fn forward(&mut self, msg: &FlighthookMessage) -> Vec<FlighthookMessage> {
        self.forward_at(msg, Instant::now())
    }

    fn forward_at(&mut self, msg: &FlighthookMessage, now: Instant) -> Vec<FlighthookMessage> {
        let window = self.filter.window();
        self.own
            .retain(|_, (at, _)| now.saturating_duration_since(*at) <= window);
        let device = frp_device(msg);
        if msg.device.is_some() {
            let devices = self.devices.entry(msg.actor.clone()).or_default();
            if !devices.contains(&device) {
                devices.push(device.clone());
            }
        }
        match &msg.event {
            FlighthookEvent::ActorStatus { status, .. } if *status != ActorStatus::Connected => {
                for device in self.devices.remove(&msg.actor).into_iter().flatten() {
                    self.filter.forget_device(&device);
                    self.own.retain(|(d, _), _| *d != device);
                }
                vec![msg.clone()]
            }
            FlighthookEvent::Launch {
                key,
                launch,
                derived,
            } => {
                let (measured, assumed) = measured_launch(launch, derived.as_deref());
                let out = FlighthookMessage {
                    event: FlighthookEvent::Launch {
                        key: key.clone(),
                        launch: Box::new(measured),
                        derived: derived.clone(),
                    },
                    ..msg.clone()
                };
                if !self.passes(&out) {
                    return Vec::new();
                }
                let own = derived.as_ref().and_then(|d| d.flight_result(assumed));
                self.own.insert((device, key.shot_id.clone()), (now, own));
                vec![out]
            }
            FlighthookEvent::ShotFinished { key, .. } => {
                if !self.passes(msg) {
                    return Vec::new();
                }
                let mut out = Vec::with_capacity(2);
                if let Some((_, Some(flight))) = self.own.remove(&(device, key.shot_id.clone())) {
                    let added = FlighthookMessage {
                        actor: msg.actor.clone(),
                        device: msg.device.clone(),
                        raw_payload: None,
                        provenance: None,
                        event: FlighthookEvent::FlightResult {
                            key: key.clone(),
                            flight: Box::new(flight),
                        },
                    };
                    if self.passes(&added) {
                        out.push(added);
                    }
                }
                out.push(msg.clone());
                out
            }
            FlighthookEvent::ShotTrigger { .. }
            | FlighthookEvent::FlightResult { .. }
            | FlighthookEvent::ClubPath { .. }
            | FlighthookEvent::FaceImpact { .. }
            | FlighthookEvent::DeviceTelemetry { .. } => {
                if self.passes(msg) {
                    vec![msg.clone()]
                } else {
                    Vec::new()
                }
            }
            _ => vec![msg.clone()],
        }
    }

    /// Whether `msg`, an FRP event, passes the forward filter (and is then
    /// recorded as forwarded). A message that is no FRP event does not pass.
    fn passes(&mut self, msg: &FlighthookMessage) -> bool {
        frp_envelope(msg).is_some_and(|env| self.filter.should_forward(&env))
    }
}

/// The FRP `device` of a bus message: its device id, else its actor.
fn frp_device(msg: &FlighthookMessage) -> String {
    msg.device.clone().unwrap_or_else(|| msg.actor.clone())
}

/// `msg` as its FRP envelope, with [`frp_device`] as its device; `None`
/// when it is no FRP event. flighthook's extension fields (`actor`, `derived`,
/// the raw frames) are not part of it.
fn frp_envelope(msg: &FlighthookMessage) -> Option<FrpEnvelope> {
    let event = match &msg.event {
        FlighthookEvent::ShotTrigger { key, timestamp } => FrpEvent::ShotTrigger {
            key: key.clone(),
            timestamp: *timestamp,
        },
        FlighthookEvent::Launch { key, launch, .. } => FrpEvent::Launch {
            key: key.clone(),
            launch: (**launch).clone(),
        },
        FlighthookEvent::FlightResult { key, flight } => FrpEvent::FlightResult {
            key: key.clone(),
            flight: (**flight).clone(),
        },
        FlighthookEvent::ClubPath { key, club } => FrpEvent::ClubPath {
            key: key.clone(),
            club: (**club).clone(),
        },
        FlighthookEvent::FaceImpact { key, impact } => FrpEvent::FaceImpact {
            key: key.clone(),
            impact: (**impact).clone(),
        },
        FlighthookEvent::ShotFinished { key, .. } => FrpEvent::ShotFinished { key: key.clone() },
        FlighthookEvent::DeviceTelemetry {
            manufacturer,
            model,
            firmware,
            source,
            telemetry,
        } => FrpEvent::DeviceTelemetry {
            manufacturer: manufacturer.clone(),
            model: model.clone(),
            firmware: firmware.clone(),
            source: source.map(|p| match p {
                Provenance::Measured => Source::Measured,
                Provenance::Virtual => Source::Virtual,
            }),
            telemetry: telemetry.clone(),
        },
        FlighthookEvent::Alert { severity, message } => FrpEvent::Alert {
            severity: *severity,
            message: message.clone(),
        },
        _ => return None,
    };
    Some(FrpEnvelope::new(frp_device(msg), event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flighthook::{
        ActorStatus, Basis, DerivedFlight, Distance, FLIGHT_MODEL, Launch, Provenance, Severity,
        ShotKey, Velocity,
    };
    use flightrelay::FrpMessage;
    use std::collections::HashMap;

    fn key(n: u32) -> ShotKey {
        ShotKey {
            shot_id: format!("shot-{n}"),
            shot_number: n,
        }
    }

    fn from_device(event: FlighthookEvent) -> FlighthookMessage {
        FlighthookMessage::new(event)
            .actor("mevo.0")
            .device("FS-XI-TEST")
    }

    fn chip() -> Launch {
        Launch {
            launch_speed: Some(Velocity::MilesPerHour(20.0)),
            launch_elevation: Some(32.0),
            launch_azimuth: Some(0.5),
            backspin_rpm: Some(1500),
            sidespin_rpm: Some(0),
            ..Launch::default()
        }
    }

    fn launch(n: u32, launch: Launch, derived: Option<DerivedFlight>) -> FlighthookMessage {
        from_device(FlighthookEvent::Launch {
            key: key(n),
            launch: Box::new(launch),
            derived: derived.map(Box::new),
        })
    }

    fn finished(n: u32) -> FlighthookMessage {
        from_device(FlighthookEvent::ShotFinished {
            key: key(n),
            raw: None,
        })
    }

    fn simulated(estimated: bool) -> DerivedFlight {
        let mut d = DerivedFlight::default();
        d.carry_distance = Some(Distance::Meters(14.0));
        d.total_distance = Some(Distance::Meters(19.5));
        d.spin_estimated = estimated;
        d
    }

    fn kinds(out: &[FlighthookMessage]) -> Vec<String> {
        out.iter()
            .map(|m| {
                serde_json::to_value(&m.event).expect("ser")["kind"]
                    .as_str()
                    .expect("kind")
                    .to_owned()
            })
            .collect()
    }

    /// A Xi launch-only chip: the measured launch goes out without the
    /// estimated spin, and flighthook's own flight result, before the shot's
    /// finish, carries the simulated carry and total with the spin in
    /// `assumed`. Both read as FRP 0.2.0.
    #[test]
    fn an_estimated_spin_goes_out_as_assumed_in_flighthooks_flight_result() {
        let mut out = FrpOutput::new();
        let t0 = Instant::now();
        assert_eq!(
            out.forward_at(&from_device(FlighthookEvent::shot_trigger(key(1))), t0)
                .len(),
            1
        );
        let sent = out.forward_at(&launch(1, chip(), Some(simulated(true))), t0);
        let FlighthookEvent::Launch {
            launch, derived, ..
        } = &sent[0].event
        else {
            panic!("not a launch");
        };
        assert_eq!((launch.backspin_rpm, launch.sidespin_rpm), (None, None));
        assert_eq!(launch.launch_speed, chip().launch_speed);
        assert!(derived.as_ref().is_some_and(|d| d.spin_estimated), "kept");

        let sent = out.forward_at(&finished(1), t0);
        assert_eq!(kinds(&sent), ["flight_result", "shot_finished"]);
        let json = serde_json::to_string(&sent[0]).expect("ser");
        let FrpMessage::Envelope(env) = FrpMessage::parse(&json).expect("frp") else {
            panic!("not an envelope");
        };
        assert_eq!(env.device, "FS-XI-TEST");
        let FrpEvent::FlightResult { flight, .. } = env.event else {
            panic!("not a flight result: {json}");
        };
        assert_eq!(flight.basis, Basis::Modeled);
        assert_eq!(flight.model.as_deref(), Some(FLIGHT_MODEL));
        assert_eq!(flight.carry_distance, Some(Distance::Meters(14.0)));
        assert_eq!(flight.total_distance, Some(Distance::Meters(19.5)));
        let assumed = flight.assumed.expect("assumed");
        assert_eq!(
            (assumed.backspin_rpm, assumed.sidespin_rpm),
            (Some(1500), Some(0))
        );
    }

    /// A device that sent its own flight result gets none from flighthook,
    /// estimated spin or not; a shot that was not simulated gets none
    /// either.
    #[test]
    fn flighthook_adds_no_flight_result_where_the_device_sent_one() {
        let mut out = FrpOutput::new();
        let t0 = Instant::now();
        let device_flight = flighthook::FlightResult {
            carry_distance: Some(Distance::Meters(150.0)),
            ..flighthook::FlightResult::new(Basis::Modeled)
        };
        out.forward_at(&from_device(FlighthookEvent::shot_trigger(key(2))), t0);
        out.forward_at(&launch(2, chip(), Some(simulated(false))), t0);
        let sent = out.forward_at(
            &from_device(FlighthookEvent::FlightResult {
                key: key(2),
                flight: Box::new(device_flight),
            }),
            t0,
        );
        assert_eq!(kinds(&sent), ["flight_result"]);
        assert_eq!(kinds(&out.forward_at(&finished(2), t0)), ["shot_finished"]);

        // Not simulated: the launch goes out whole, no flight result.
        out.forward_at(&launch(3, chip(), None), t0);
        assert_eq!(kinds(&out.forward_at(&finished(3), t0)), ["shot_finished"]);
    }

    /// A shot event passes once per (device, shot, kind): a repeat, back
    /// around a loop or sent twice, is dropped, and so is a second
    /// `shot_finished` (no second flight result either).
    #[test]
    fn repeats_of_a_shot_event_are_dropped() {
        let mut out = FrpOutput::new();
        let t0 = Instant::now();
        let trigger = from_device(FlighthookEvent::shot_trigger(key(4)));
        assert_eq!(out.forward_at(&trigger, t0).len(), 1);
        assert!(out.forward_at(&trigger, t0).is_empty());
        let l = launch(4, chip(), Some(simulated(false)));
        assert_eq!(out.forward_at(&l, t0).len(), 1);
        assert!(out.forward_at(&l, t0).is_empty());
        assert_eq!(out.forward_at(&finished(4), t0).len(), 2);
        assert!(out.forward_at(&finished(4), t0).is_empty());
        // The same shot id from another device is another shot.
        let other = FlighthookMessage::new(FlighthookEvent::shot_trigger(key(4))).actor("r10.0");
        assert_eq!(out.forward_at(&other, t0).len(), 1);
    }

    /// Device telemetry passes when it changes something; the bus stamps
    /// its `source`, which FRP reads.
    #[test]
    fn device_telemetry_passes_on_change_with_its_source() {
        let mut out = FrpOutput::new();
        let t0 = Instant::now();
        let telemetry = |ready: &str| {
            from_device(FlighthookEvent::DeviceTelemetry {
                manufacturer: None,
                model: None,
                firmware: None,
                source: Some(Provenance::Measured),
                telemetry: Some(HashMap::from([("ready".into(), ready.into())])),
            })
        };
        let sent = out.forward_at(&telemetry("true"), t0);
        assert_eq!(sent.len(), 1);
        let json = serde_json::to_string(&sent[0]).expect("ser");
        let FrpMessage::Envelope(env) = FrpMessage::parse(&json).expect("frp") else {
            panic!("not an envelope");
        };
        assert!(matches!(
            env.event,
            FrpEvent::DeviceTelemetry {
                source: Some(flightrelay::Source::Measured),
                ..
            }
        ));
        assert!(out.forward_at(&telemetry("true"), t0).is_empty());
        assert_eq!(out.forward_at(&telemetry("false"), t0).len(), 1);
    }

    /// A device's telemetry is forwarded in full again after its actor's
    /// session ends, as the dashboard drops a lost connection's values.
    #[test]
    fn a_new_session_sends_the_same_telemetry_again() {
        let mut out = FrpOutput::new();
        let t0 = Instant::now();
        let battery = from_device(FlighthookEvent::DeviceTelemetry {
            manufacturer: None,
            model: None,
            firmware: None,
            source: Some(Provenance::Measured),
            telemetry: Some(HashMap::from([("battery_pct".into(), "80".into())])),
        });
        let status = |status| {
            FlighthookMessage::new(FlighthookEvent::ActorStatus {
                status,
                telemetry: HashMap::new(),
            })
            .actor("mevo.0")
        };
        assert_eq!(out.forward_at(&battery, t0).len(), 1);
        assert!(out.forward_at(&battery, t0).is_empty());
        assert_eq!(out.forward_at(&status(ActorStatus::Connected), t0).len(), 1);
        assert!(out.forward_at(&battery, t0).is_empty(), "still connected");
        assert_eq!(
            out.forward_at(&status(ActorStatus::Reconnecting), t0).len(),
            1
        );
        assert_eq!(out.forward_at(&battery, t0).len(), 1, "a new session");
    }

    /// flighthook's extensions and every alert pass unchanged: a device
    /// that raises the same alert twice shows it twice, as each one is raised
    /// anew.
    #[test]
    fn extensions_and_alerts_pass_every_time() {
        let mut out = FrpOutput::new();
        let t0 = Instant::now();
        let ext = FlighthookMessage::new(FlighthookEvent::NewShotGroup).actor("ws.1");
        assert_eq!(out.forward_at(&ext, t0).len(), 1);
        assert_eq!(out.forward_at(&ext, t0).len(), 1);
        let mode = FlighthookMessage::new(FlighthookEvent::SetDetectionMode {
            mode: Some(flighthook::ShotDetectionMode::Putting),
            handed: Some(flighthook::Handedness::Left),
            club: Some(flighthook::Club::Putter),
        })
        .actor("system");
        assert_eq!(out.forward_at(&mode, t0).len(), 1);
        let own = FlighthookMessage::new(FlighthookEvent::Alert {
            severity: Severity::Warn,
            message: "GSPro not connected".into(),
        })
        .actor("gspro.0");
        assert_eq!(out.forward_at(&own, t0).len(), 1);
        assert_eq!(out.forward_at(&own, t0).len(), 1);
        let device = from_device(FlighthookEvent::Alert {
            severity: Severity::Warn,
            message: "Shot #3: no read".into(),
        });
        assert_eq!(out.forward_at(&device, t0).len(), 1);
        assert_eq!(out.forward_at(&device, t0).len(), 1, "raised again");
    }

    /// A repeat of a shot is dropped for as long as flighthook holds a shot,
    /// which is longer than FRP's 30 s default.
    #[test]
    fn the_window_is_flighthooks_shot_timeout() {
        let out = FrpOutput::new();
        assert_eq!(out.filter.window(), SHOT_TIMEOUT);
        assert!(SHOT_TIMEOUT > flightrelay::DEFAULT_SHOT_TIMEOUT);
    }

    /// The envelope the filter keys on reads, on the wire, as the one an FRP
    /// peer reads from the message as sent, for every FRP event, also with
    /// flighthook's extension fields and odd values on it.
    #[test]
    fn the_filters_envelope_is_the_one_on_the_wire() {
        let raw = flighthook::ShotRaw {
            transport: "tcp".into(),
            frames: vec![flighthook::RawRecord::binary(
                flighthook::RawDir::Rx,
                &[0xc0, 0xff, 0xee],
            )],
            ..flighthook::ShotRaw::default()
        };
        let odd = Launch {
            launch_speed: Some(Velocity::MetersPerSecond(f64::NAN)),
            launch_azimuth: Some(f64::NAN),
            ..chip()
        };
        let messages = [
            from_device(FlighthookEvent::ShotTrigger {
                key: key(1),
                timestamp: Some(1_762_500_000_000),
            })
            .provenance(Provenance::Measured),
            launch(1, chip(), Some(simulated(true))),
            launch(1, odd, None),
            launch(1, Launch::default(), None),
            from_device(FlighthookEvent::FlightResult {
                key: key(1),
                flight: Box::new(flighthook::FlightResult {
                    carry_distance: Some(Distance::Meters(f64::NAN)),
                    ..flighthook::FlightResult::new(Basis::Modeled)
                }),
            }),
            from_device(FlighthookEvent::ClubPath {
                key: key(1),
                club: Box::new(flighthook::ClubData::default()),
            }),
            from_device(FlighthookEvent::FaceImpact {
                key: key(1),
                impact: Box::new(flighthook::FaceImpact::default()),
            }),
            from_device(FlighthookEvent::ShotFinished {
                key: key(1),
                raw: Some(Box::new(raw)),
            }),
            from_device(FlighthookEvent::DeviceTelemetry {
                manufacturer: Some("FlightScope".into()),
                model: None,
                firmware: None,
                source: Some(Provenance::Virtual),
                telemetry: Some(HashMap::from([("ready".into(), "true".into())])),
            }),
            FlighthookMessage::new(FlighthookEvent::Alert {
                severity: Severity::Warn,
                message: "no device".into(),
            })
            .actor("gspro.0"),
        ];
        for msg in &messages {
            let mut json = serde_json::to_value(msg).expect("ser");
            json["device"] = serde_json::Value::String(frp_device(msg));
            let FrpMessage::Envelope(wire) = FrpMessage::parse(&json.to_string()).expect("frp")
            else {
                panic!("not an envelope: {json}");
            };
            let ours = frp_envelope(msg).expect("an FRP event");
            let FrpMessage::Envelope(ours) =
                FrpMessage::parse(&serde_json::to_string(&ours).expect("ser")).expect("frp")
            else {
                panic!("not an envelope");
            };
            // NaN never equals itself: compare the envelopes as JSON.
            assert_eq!(
                serde_json::to_string(&ours).expect("ser"),
                serde_json::to_string(&wire).expect("ser"),
                "{json}"
            );
        }
        let ext = FlighthookMessage::new(FlighthookEvent::NewShotGroup).actor("ws.1");
        assert!(frp_envelope(&ext).is_none());
    }

    /// Auto-chip goes out as an FRP device: every envelope carries its
    /// `device` id, and a `device_telemetry` declaring `source: virtual`
    /// comes before its first shot event. The next chip's declaration
    /// changes nothing and is dropped.
    #[test]
    fn auto_chip_has_a_device_and_declares_its_source_first() {
        use std::sync::atomic::AtomicU32;
        let dir = std::env::temp_dir().join(format!(
            "flighthook-frp-autochip-test-{}",
            crate::state::config::generate_id()
        ));
        let (state, _writer) = crate::state::SystemState::new(dir.join("config.toml"));
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let chips = AtomicU32::new(0);
        let mut out = FrpOutput::new();
        let mut chip = || {
            crate::autochip::send(&state, &tx, 3, &chips).expect("sent");
            std::iter::from_fn(|| rx.try_recv().ok())
                .flat_map(|m| out.forward(&m))
                .map(|m| {
                    let json = serde_json::to_string(&m).expect("ser");
                    match FrpMessage::parse(&json).expect("frp") {
                        FrpMessage::Envelope(env) => env,
                        FrpMessage::Protocol(_) => panic!("not an envelope: {json}"),
                    }
                })
                .collect::<Vec<_>>()
        };
        let first = chip();
        let second = chip();
        let _ = std::fs::remove_dir_all(dir);
        let kinds = |envs: &[FrpEnvelope]| {
            envs.iter()
                .map(|e| {
                    serde_json::to_value(&e.event).expect("ser")["kind"]
                        .as_str()
                        .expect("kind")
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&first),
            [
                "device_telemetry",
                "shot_trigger",
                "launch",
                "flight_result",
                "shot_finished"
            ]
        );
        assert!(matches!(
            first[0].event,
            FrpEvent::DeviceTelemetry {
                source: Some(Source::Virtual),
                ..
            }
        ));
        assert_eq!(
            kinds(&second),
            ["shot_trigger", "launch", "flight_result", "shot_finished"]
        );
        assert!(
            first
                .iter()
                .chain(&second)
                .all(|e| e.device == flighthook::AUTO_CHIP_ACTOR)
        );
    }
}
