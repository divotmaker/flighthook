//! Test support: a shot's measured launch and the device's flight values
//! written as one block, as tests describe a shot.

use flighthook::{Basis, Distance, FlightResult, Launch, ShotData, Velocity};

/// A shot's launch values and the device's flight values side by side.
/// [`launch`](Self::launch) and [`flight`](Self::flight) split it the way a
/// device actor sends it: the launch, and a [`Basis::Modeled`] flight result
/// when any flight value is set.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Ball {
    pub launch_speed: Option<Velocity>,
    pub launch_azimuth: Option<f64>,
    pub launch_elevation: Option<f64>,
    pub backspin_rpm: Option<i32>,
    pub sidespin_rpm: Option<i32>,
    pub total_spin_rpm: Option<i32>,
    pub spin_axis: Option<f64>,
    pub carry_distance: Option<Distance>,
    pub carry_offline: Option<Distance>,
    pub total_distance: Option<Distance>,
    pub total_offline: Option<Distance>,
    pub roll_distance: Option<Distance>,
    pub max_height: Option<Distance>,
    pub flight_time: Option<f64>,
    pub descent_angle: Option<f64>,
}

impl Ball {
    /// The launch values.
    pub(crate) fn launch(&self) -> Launch {
        Launch {
            launch_speed: self.launch_speed,
            launch_azimuth: self.launch_azimuth,
            launch_elevation: self.launch_elevation,
            backspin_rpm: self.backspin_rpm,
            sidespin_rpm: self.sidespin_rpm,
            total_spin_rpm: self.total_spin_rpm,
            spin_axis: self.spin_axis,
        }
    }

    /// The flight values as the device's flight result, `None` when there
    /// are none.
    pub(crate) fn flight(&self) -> Option<FlightResult> {
        let flight = FlightResult {
            carry_distance: self.carry_distance,
            carry_offline: self.carry_offline,
            total_distance: self.total_distance,
            total_offline: self.total_offline,
            roll_distance: self.roll_distance,
            max_height: self.max_height,
            flight_time: self.flight_time,
            descent_angle: self.descent_angle,
            ..FlightResult::new(Basis::Modeled)
        };
        (flight != FlightResult::new(Basis::Modeled)).then_some(flight)
    }

    /// The block for a shot's `launch` and `flight`.
    pub(crate) fn of(launch: Option<&Launch>, flight: Option<&FlightResult>) -> Self {
        let l = launch.cloned().unwrap_or_default();
        let f = flight.cloned();
        let f = f.as_ref();
        Self {
            launch_speed: l.launch_speed,
            launch_azimuth: l.launch_azimuth,
            launch_elevation: l.launch_elevation,
            backspin_rpm: l.backspin_rpm,
            sidespin_rpm: l.sidespin_rpm,
            total_spin_rpm: l.total_spin_rpm,
            spin_axis: l.spin_axis,
            carry_distance: f.and_then(|f| f.carry_distance),
            carry_offline: f.and_then(|f| f.carry_offline),
            total_distance: f.and_then(|f| f.total_distance),
            total_offline: f.and_then(|f| f.total_offline),
            roll_distance: f.and_then(|f| f.roll_distance),
            max_height: f.and_then(|f| f.max_height),
            flight_time: f.and_then(|f| f.flight_time),
            descent_angle: f.and_then(|f| f.descent_angle),
        }
    }

    /// Put this block on `shot`: its launch and flight result.
    pub(crate) fn put_on(&self, shot: &mut ShotData) {
        shot.launch = Some(self.launch());
        shot.flight = self.flight();
    }
}

/// The outputs of the shots the bus `messages` complete, as text: for each
/// shot, its GSPro / Open Connect message, its Pureplay frames and its CSV
/// row in both unit systems, one line each.
pub(crate) fn outputs(messages: &[flighthook::FlighthookMessage]) -> String {
    use crate::actors::web::{ShotContext, StoredShot};
    use flighthook::UnitSystem;
    let mut agg = flighthook::ShotAggregator::new();
    let mut text = String::new();
    for msg in messages {
        let Some(data) = agg.feed(msg) else {
            continue;
        };
        let gspro = serde_json::to_string(&crate::actors::gspro::mapper::map_shot(&data))
            .expect("gspro json");
        let pureplay = format!("{:?}", crate::actors::pureplay::mapper::map_shot(&data, 1));
        let stored = StoredShot {
            received_at: chrono::DateTime::UNIX_EPOCH,
            key: flighthook::ShotKey {
                shot_id: "pin".into(),
                shot_number: data.shot_number,
            },
            context: ShotContext::default(),
            data,
            raw: None,
        };
        let metric = crate::actors::web::export::to_csv_row(&stored, UnitSystem::Metric);
        let imperial = crate::actors::web::export::to_csv_row(&stored, UnitSystem::Imperial);
        text.push_str(&format!(
            "gspro {gspro}\npureplay {pureplay}\ncsv_metric {}\ncsv_imperial {}\n",
            metric.trim_end(),
            imperial.trim_end()
        ));
    }
    text
}

/// Every output's numbers for the case `case` are what they were before the
/// launch and the device's flight values were sent apart: `outputs(messages)`
/// equals the case's section of `testdata/output_pins.txt`, recorded from
/// the bus shape that carried them in one block.
pub(crate) fn assert_outputs_pinned(case: &str, messages: &[flighthook::FlighthookMessage]) {
    const PINS: &str = include_str!("testdata/output_pins.txt");
    let header = format!("== {case}\n");
    let start = PINS
        .find(&header)
        .unwrap_or_else(|| panic!("no pin for {case}"))
        + header.len();
    let end = PINS[start..]
        .find("\n== ")
        .map_or(PINS.len(), |n| start + n + 1);
    let got = outputs(messages);
    assert!(!got.is_empty(), "{case}: no shot completed");
    assert_eq!(got, &PINS[start..end], "{case}");
}

/// A shot on the bus as a device actor `actor` sends it: trigger, launch
/// (simulated under the default settings, in detection mode `mode`), the
/// device's flight result, club data, finished.
pub(crate) fn device_shot(
    actor: &str,
    launch: Launch,
    flight: Option<FlightResult>,
    club: Option<flighthook::ClubData>,
    mode: Option<flighthook::ShotDetectionMode>,
) -> Vec<flighthook::FlighthookMessage> {
    use flighthook::{FlighthookEvent, FlighthookMessage, ShotKey};
    let dir = std::env::temp_dir().join(format!(
        "flighthook-pin-test-{}",
        crate::state::config::generate_id()
    ));
    let (state, _writer) = crate::state::SystemState::new(dir.join("config.toml"));
    let key = ShotKey {
        shot_id: "pin".into(),
        shot_number: 1,
    };
    let mut events = vec![FlighthookEvent::shot_trigger(key.clone())];
    events.extend(crate::sim::launch_events_in_mode(
        &state,
        key.clone(),
        launch,
        flight,
        mode,
    ));
    if let Some(club) = club {
        events.push(FlighthookEvent::ClubPath {
            key: key.clone(),
            club: Box::new(club),
        });
    }
    events.push(FlighthookEvent::ShotFinished { key, raw: None });
    let _ = std::fs::remove_dir_all(&dir);
    events
        .into_iter()
        .map(|e| FlighthookMessage::new(e).actor(actor))
        .collect()
}
