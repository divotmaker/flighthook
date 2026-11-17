use std::collections::VecDeque;
use std::path::PathBuf;

use flighthook::{DeviceSection, Mode, Source, TrackballConfig};
use flightrelay::FrpBody;
use flightrelay::driver::{FrpDriver, FrpSession, TurnHints};
use thumbswing::testing::StreamBuilder;
use thumbswing::{DeviceInfo, Error, Input, Sample};

use super::*;
use crate::actors::driver::tests::card_bus;
use crate::actors::scripted::{Stepped, body, is_finished};
use crate::bus::BusMsg;

/// A trackball that plays one swing from its own clock, which runs 1 ms
/// per read, as a fast replay does; it reports itself as [`trackball_info`].
struct Played {
    samples: VecDeque<Sample>,
    now: u64,
    info: DeviceInfo,
}

impl Played {
    fn new(swing: &[Sample]) -> Self {
        let now = 1_000_000;
        let t0 = swing.first().map_or(0, |s| s.t_us);
        Self {
            samples: swing
                .iter()
                .map(|s| Sample::new(now + 1000 + s.t_us - t0, s.dx, s.dy))
                .collect(),
            now,
            info: trackball_info(),
        }
    }
}

impl Input for Played {
    fn try_recv(&mut self) -> Result<Option<Sample>, Error> {
        match self.samples.front() {
            Some(s) if s.t_us <= self.now => Ok(self.samples.pop_front()),
            _ => Ok(None),
        }
    }

    fn now_us(&mut self) -> u64 {
        self.now += 1000;
        self.now
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn paced(&self) -> bool {
        false
    }
}

/// A trackball as the evdev backend describes one, with a `by-id` link
/// that carries a serial number.
fn trackball_info() -> DeviceInfo {
    DeviceInfo {
        path: PathBuf::from("/dev/input/event7"),
        by_id: vec![PathBuf::from(
            "/dev/input/by-id/usb-Example_Trackball_SN0042-event-mouse",
        )],
        name: "Example Trackball".into(),
        relative_xy: true,
        readable: true,
        kernel_timestamps: true,
        ..DeviceInfo::default()
    }
}

/// A straight pull, a pause, and a push at `counts` per 100 ms (1200 is
/// full power at 400 CPI), `pull_deg` / `push_deg` from straight.
fn swing(pull_deg: f64, push_deg: f64, counts: f64) -> Vec<Sample> {
    StreamBuilder::new(125.0)
        .pull(pull_deg, 600.0, 150.0)
        .pause_ms(300.0)
        .push(push_deg, counts, 100.0)
        .build()
}

/// What the card with `section` puts on the bus for one swing `samples`
/// of thumbswing's session, the session in `mode`, up to the shot's end.
fn swung(section: &TrackballSection, mode: Mode, samples: &[Sample]) -> Vec<BusMsg> {
    let hints = TurnHints::default().with_mode(mode);
    let mut host = Stepped::new(hints);
    let mut driver = Thumbswing::new(section.driver_settings()).expect("driver");
    let mut session = driver.connect_with(Played::new(samples), &hints);
    host.until(|now, out| session.step(now, out), is_finished);
    let identity = session.identity().clone();
    card_bus::<Thumbswing, TrackballHooks>(
        "trackball.0",
        section.clone(),
        Some(mode),
        &identity,
        host.events,
    )
}

/// The card's shots are a virtual source's.
#[test]
fn the_card_is_a_virtual_source() {
    let seen = swung(
        &TrackballSection::default(),
        Mode::Full,
        &swing(0.0, 0.0, 1200.0),
    );
    let source = seen
        .iter()
        .find_map(|m| match body(m) {
            Some(FrpBody::BusActor(a)) => a.device.as_ref().and_then(|d| d.source),
            _ => None,
        })
        .expect("the card's bus.actor");
    assert_eq!(source, Source::Virtual);
}

/// The shot export names the device by kind, maker, model and version
/// only: neither the device id (a `by-id` name, which can carry a
/// serial number) nor the `device` setting reaches it.
#[tokio::test(flavor = "multi_thread")]
async fn the_export_names_no_device() {
    let section = TrackballSection {
        name: "Trackball".into(),
        driver: TrackballConfig {
            device: Some("usb-Example_Trackball_SN0042-event-mouse".into()),
            ..TrackballConfig::default()
        },
        ..TrackballSection::default()
    };
    let seen = swung(&section, Mode::Full, &swing(0.0, 0.0, 1200.0));
    let firmware = seen
        .iter()
        .find_map(|m| match body(m) {
            Some(FrpBody::BusActor(a)) => a.device.as_ref().and_then(|d| d.firmware.clone()),
            _ => None,
        })
        .expect("the driver's version");
    let mut config = flighthook::FlighthookConfig::default();
    config.trackball.insert("0".into(), section);
    let export = crate::actors::web::tests::export_lines(&config, &seen).await;
    assert_eq!(export.len(), 1);
    assert!(!export[0].contains("SN0042"), "{}", export[0]);
    assert!(!export[0].contains("event-mouse"), "{}", export[0]);
    let line: serde_json::Value = serde_json::from_str(&export[0]).expect("json");
    assert_eq!(
        line["device"],
        serde_json::json!({
            "kind": "trackball",
            "manufacturer": "divotmaker",
            "model": "thumbswing trackball",
            "firmware": firmware,
        })
    );
    // The section without `name`, `device` and the card's actor id.
    assert_eq!(line["settings"]["device"], serde_json::json!({}));
}

/// Every output's numbers for a trackball drive and putt
/// (`output_pins.txt`).
#[test]
fn outputs_keep_their_numbers() {
    for (case, mode, club, pull, push, counts) in [
        (
            "trackball_full_swing",
            Mode::Full,
            thumbswing::Club::Driver,
            20.0,
            5.0,
            900.0,
        ),
        (
            "trackball_putt",
            Mode::Putting,
            thumbswing::Club::Iron7,
            0.0,
            -3.0,
            600.0,
        ),
    ] {
        let mut section = TrackballSection::default();
        section.driver.club = club;
        // The pinned numbers are for a full power speed of 30 in/s.
        section.driver.tuning.full_power_speed = 30.0;
        let messages = swung(&section, mode, &swing(pull, push, counts));
        crate::test_ball::assert_outputs_pinned(case, "trackball.0", Some(mode), &messages);
    }
}
