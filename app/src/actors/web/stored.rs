//! The stored shot: a composed shot.

use flightrelay::CompletedShot;

use crate::state::SystemState;
use flighthook::{FlightPath, ShotData};

/// The card key of actor `from` for the UI: a local card's key, else the
/// actor id.
pub(crate) fn card_of(state: &SystemState, from: &str) -> String {
    state.card_of_actor(from).unwrap_or_else(|| from.to_owned())
}

/// A composed shot as stored: the launch as the launch monitor sent it,
/// every flight with its sender, club, impact, the flight model's `path`,
/// the card key `card` (for a flighthook card), and the metrics of what is
/// shown ([`ShotData::shown`]).
pub(crate) fn stored_data(
    shot: &CompletedShot,
    card: Option<&str>,
    path: Option<&FlightPath>,
) -> ShotData {
    let mut data = ShotData {
        shot_number: shot.key.shot_number,
        shot_id: Some(shot.key.shot_id.clone()),
        launch: shot.launch.clone(),
        club: shot.club.clone(),
        impact: shot.impact.clone(),
        from: shot.from.clone(),
        card: card.map(str::to_owned),
        source: shot.source,
        flights: shot
            .flights
            .iter()
            .map(|f| flighthook::Flight {
                from: f.from.clone(),
                flight: f.flight.clone(),
            })
            .collect(),
        path: path.cloned(),
        ..ShotData::default()
    };
    data.metrics = data.shown().metrics;
    data
}

#[cfg(test)]
mod tests {
    use flightrelay::{
        AssumedLaunch, Basis, FrpMessage, Launch, ShotAggregator, ShotEvent, ShotFinished,
        ShotFlight, ShotKey, ShotLaunch, ShotTrigger, Velocity,
    };

    use super::*;
    use flighthook::{Distance, FlightResult, Shown};

    const MONITOR: &str = "monitor-id";
    const CHIP: &str = "auto-chip-id";
    const MODEL: &str = "flight-model-id";

    fn key() -> ShotKey {
        ShotKey {
            shot_id: "shot-1".into(),
            shot_number: 1,
        }
    }

    fn m(v: f64) -> Option<Distance> {
        Some(Distance::Meters(v))
    }

    fn launch(spin: bool) -> Launch {
        Launch {
            launch_speed: Some(Velocity::MetersPerSecond(60.0)),
            launch_elevation: Some(14.0),
            backspin_rpm: spin.then_some(3000),
            sidespin_rpm: spin.then_some(0),
            ..Launch::default()
        }
    }

    /// The shot `from` sent (launch, its own flight), with the flights of
    /// other actors attached after its finish.
    fn shot(
        from: &str,
        launch: Launch,
        own: Option<FlightResult>,
        addenda: &[(&str, FlightResult)],
    ) -> CompletedShot {
        let mut agg = ShotAggregator::new();
        agg.feed(&FrpMessage::new(from, ShotTrigger::new(key())));
        agg.feed(&FrpMessage::new(from, ShotLaunch { key: key(), launch }));
        if let Some(flight) = own {
            agg.feed(&FrpMessage::new(from, ShotFlight { key: key(), flight }));
        }
        let Some(ShotEvent::Finished(mut shot)) =
            agg.feed(&FrpMessage::new(from, ShotFinished { key: key() }))
        else {
            panic!("finished");
        };
        for (actor, flight) in addenda {
            shot.flights.push(flightrelay::Flight {
                from: (*actor).to_owned(),
                flight: flight.clone(),
            });
        }
        shot
    }

    fn tracked() -> FlightResult {
        FlightResult {
            carry_distance: m(150.0),
            carry_offline: m(-4.0),
            ..FlightResult::new(Basis::Tracked)
        }
    }

    fn modeled() -> FlightResult {
        FlightResult {
            total_distance: m(170.0),
            total_offline: m(-7.0),
            roll_distance: m(20.0),
            ..FlightResult::new(Basis::Modeled)
        }
    }

    /// The Auto-chip card for the Auto-chip's shot.
    fn card(shot: &CompletedShot) -> Option<&'static str> {
        (shot.from == CHIP).then_some(flighthook::AUTO_CHIP_ACTOR)
    }

    fn data(shot: &CompletedShot) -> Shown {
        stored_data(shot, card(shot), None).shown()
    }

    /// The launch monitor's own flight is the shot's `flight`; the flight
    /// model's is `derived`, with the roll offline from the device's carry
    /// offline.
    #[test]
    fn the_devices_flight_is_flight_and_the_models_is_derived() {
        let s = shot(
            MONITOR,
            launch(true),
            Some(tracked()),
            &[(MODEL, modeled())],
        );
        let stored = stored_data(&s, Some("mevo.0"), None);
        assert_eq!(stored.from, MONITOR);
        assert_eq!(stored.card.as_deref(), Some("mevo.0"));
        assert_eq!(stored.shot_id.as_deref(), Some("shot-1"));
        assert_eq!(stored.launch, Some(launch(true)));
        assert_eq!(stored.flights.len(), 2);
        let d = data(&s);
        assert_eq!(d.flight, Some(tracked()));
        let derived = d.derived.expect("derived");
        assert_eq!(derived.total_distance, m(170.0));
        assert_eq!(derived.carry_distance, None);
        assert_eq!(derived.roll_offline, m(-3.0));
        assert!(!derived.spin_estimated);
    }

    /// Without a carry offline anywhere the roll offline is the total
    /// offline.
    #[test]
    fn roll_offline_without_a_carry_offline_is_the_total_offline() {
        let s = shot(MONITOR, launch(true), None, &[(MODEL, modeled())]);
        let derived = data(&s).derived.expect("derived");
        assert_eq!(derived.roll_offline, m(-7.0));
        assert_eq!(data(&s).flight, None);
    }

    /// An Auto-chip's own flight is `derived`, never a device's `flight`.
    #[test]
    fn an_auto_chips_flight_is_derived() {
        let mut own = modeled();
        own.carry_distance = m(10.0);
        let s = shot(CHIP, launch(true), Some(own), &[]);
        let d = data(&s);
        assert_eq!(d.flight, None);
        assert_eq!(d.derived.expect("derived").carry_distance, m(10.0));
    }

    /// A launch monitor's flight holding only assumed spin is no flight;
    /// the spin is then estimated, unless the launch measured it.
    #[test]
    fn assumed_only_spin_is_estimated_and_no_flight() {
        let assumed = FlightResult {
            assumed: Some(AssumedLaunch {
                backspin_rpm: Some(4000),
                ..AssumedLaunch::default()
            }),
            ..FlightResult::new(Basis::Tracked)
        };
        let s = shot(MONITOR, launch(false), Some(assumed.clone()), &[]);
        let d = data(&s);
        assert_eq!(d.flight, None);
        assert!(d.derived.expect("derived").spin_estimated);

        let s = shot(MONITOR, launch(true), Some(assumed), &[]);
        assert!(data(&s).derived.is_none());
    }

    /// A path alone still makes a `derived` holding it.
    #[test]
    fn a_path_alone_makes_derived() {
        let s = shot(MONITOR, launch(true), Some(tracked()), &[]);
        let path = FlightPath::default();
        let d = stored_data(&s, card(&s), Some(&path));
        assert_eq!(d.path.as_ref(), Some(&path));
        assert_eq!(d.shown().derived.expect("derived").path, Some(path));
    }
}
