//! Test support: a shot's measured launch and the device's flight values
//! written as one block, as tests describe a shot.

use flighthook::{Basis, Distance, FlightResult, Launch, Mode, ShotData, Velocity};

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

    /// Put this block on `shot`: its launch and, as the launch monitor's
    /// own, its flight result.
    pub(crate) fn put_on(&self, shot: &mut ShotData) {
        shot.launch = Some(self.launch());
        shot.flights.retain(|f| f.from != shot.from);
        if let Some(flight) = self.flight() {
            shot.flights.insert(
                0,
                flighthook::Flight {
                    from: shot.from.clone(),
                    flight,
                },
            );
        }
    }
}

/// Put the flight model's result `derived` on `shot`: its values as the
/// flight model's flight and its path.
pub(crate) fn put_derived(shot: &mut ShotData, derived: Option<flighthook::DerivedFlight>) {
    let Some(derived) = derived else {
        return;
    };
    shot.path.clone_from(&derived.path);
    shot.flights.push(flighthook::Flight {
        from: "flight-model".into(),
        flight: derived.modeled(),
    });
}

/// The outputs of the shots the bus `messages` complete, as text: for each
/// shot, its GSPro / Open Connect message, its Pureplay frames and its CSV
/// row in both unit systems, one line each.
///
/// Each shot is built as production builds it. The outputs take it through
/// [`output_shot`](crate::actors::simout::output_shot) with Auto-chip's
/// actor id (the shot's own when `card` is Auto-chip's card), so Auto-chip's
/// flight never reaches them. The CSV row is the stored shot's: the flight
/// model's addendum is added as the flight model actor computes it
/// (default `[simulation]`, the shot's turn mode, else `mode`, the mode the
/// card arms in), then the web store's composition names the shot by its
/// card key `card` and stamps the putter on an ExPutt card's shot, as its
/// fixed mode makes the store do.
pub(crate) fn outputs(card: &str, mode: Option<Mode>, messages: &[crate::bus::BusMsg]) -> String {
    use crate::actors::web::{ExportContext, StoredShot};
    use flighthook::{SimulationSection, UnitSystem};
    const FLIGHT_MODEL: &str = "simulation";
    let mut agg = flightrelay::ShotAggregator::new();
    let mut text = String::new();
    for msg in messages.iter().filter_map(crate::bus::BusMsg::as_frp) {
        let Some(flightrelay::ShotEvent::Finished(mut shot)) = agg.feed(msg) else {
            continue;
        };
        let auto_chip = if card == flighthook::AUTO_CHIP_ACTOR {
            shot.from.clone()
        } else {
            flighthook::AUTO_CHIP_ACTOR.to_owned()
        };
        let data = crate::actors::simout::output_shot(&shot, card, &auto_chip);
        let gspro = serde_json::to_string(&crate::actors::gspro::mapper::map_shot(&data))
            .expect("gspro json");
        let pureplay = format!("{:?}", crate::actors::pureplay::mapper::map_shot(&data, 1));
        let mut path = None;
        if let Some(launch) = shot.launch_for_flight() {
            let device = shot
                .flights
                .iter()
                .find(|f| f.from == shot.from)
                .map(|f| &f.flight);
            let mode = shot.turn.as_ref().and_then(|t| t.mode).or(mode);
            if let Some(modeled) = crate::sim::model(
                &SimulationSection::default(),
                &launch,
                device,
                mode,
                &crate::sim::model_name(),
            ) {
                path = modeled.path;
                if let Some(flight) = modeled.flight {
                    shot.flights.push(flightrelay::Flight {
                        from: FLIGHT_MODEL.into(),
                        flight,
                    });
                }
            }
        }
        let mut data = crate::actors::web::stored::stored_data(&shot, Some(card), path.as_ref());
        // The ExPutt card registers its driver's fixed putting mode, so the
        // store stamps the putter on its shots.
        if card.starts_with("exputt.") {
            data.club_selected = Some(flighthook::Club::Putter);
        }
        let stored = StoredShot {
            received_at: chrono::DateTime::UNIX_EPOCH,
            key: flighthook::ShotKey {
                shot_id: "pin".into(),
                shot_number: data.shot_number,
            },
            context: ExportContext::default(),
            data,
            raw: None,
            frp: None,
            path: None,
        };
        let metric = crate::actors::web::export::to_csv_row(
            &stored,
            UnitSystem::Metric,
            false,
            &flighthook::FlighthookConfig::default(),
        );
        let imperial = crate::actors::web::export::to_csv_row(
            &stored,
            UnitSystem::Imperial,
            false,
            &flighthook::FlighthookConfig::default(),
        );
        text.push_str(&format!(
            "gspro {gspro}\npureplay {pureplay}\ncsv_metric {}\ncsv_imperial {}\n",
            metric.trim_end(),
            imperial.trim_end()
        ));
    }
    text
}

/// Every output's numbers for the case `case` are the pinned ones:
/// [`outputs`]`(card, mode, messages)` equals the case's section of
/// `testdata/output_pins.txt`.
pub(crate) fn assert_outputs_pinned(
    case: &str,
    card: &str,
    mode: Option<Mode>,
    messages: &[crate::bus::BusMsg],
) {
    const PINS: &str = include_str!("testdata/output_pins.txt");
    let header = format!("== {case}\n");
    let start = PINS
        .find(&header)
        .unwrap_or_else(|| panic!("no pin for {case}"))
        + header.len();
    let end = PINS[start..]
        .find("\n== ")
        .map_or(PINS.len(), |n| start + n + 1);
    let got = outputs(card, mode, messages);
    assert!(!got.is_empty(), "{case}: no shot completed");
    assert_eq!(got, &PINS[start..end], "{case}");
}

/// The frames Pureplay Connect sends in a tournament round (strict mode)
/// for the first shot that finishes in `messages`: the launch monitor's
/// values as the round takes them ([`measured_shot`]), from a source the
/// round counts (the launch monitor's `bus.actor` in `messages`), through
/// the Pureplay mapper. `None` when the round withholds the shot.
///
/// [`measured_shot`]: crate::actors::simout::measured_shot
pub(crate) fn strict_frames(
    card: &str,
    messages: &[crate::bus::BusMsg],
) -> Option<crate::actors::pureplay::mapper::ShotFrames> {
    use crate::state::round::{RoundPolicy, RoundRequest};
    let round = RoundPolicy::default();
    let guard = round
        .begin_round(&RoundRequest {
            holder: "Pureplay",
            feeds: &[],
        })
        .expect("a round");
    let mut agg = flightrelay::ShotAggregator::new();
    for msg in messages.iter().filter_map(crate::bus::BusMsg::as_frp) {
        if let Some(flightrelay::ShotEvent::Finished(shot)) = agg.feed(msg) {
            let shot = crate::actors::simout::measured_shot(&shot, card);
            if !guard.counts(shot.source) {
                return None;
            }
            return crate::actors::pureplay::mapper::map_shot(&shot, 1);
        }
    }
    panic!("no shot finished in {} messages", messages.len());
}

/// `messages` without any `bus.actor`: a launch monitor that never said
/// what it is.
pub(crate) fn without_bus_actor(messages: &[crate::bus::BusMsg]) -> Vec<crate::bus::BusMsg> {
    messages
        .iter()
        .filter(|m| {
            !matches!(
                m.as_frp().map(|f| &f.body),
                Some(flightrelay::FrpBody::BusActor(_))
            )
        })
        .cloned()
        .collect()
}
