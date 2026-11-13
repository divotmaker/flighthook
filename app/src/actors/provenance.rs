//! Where a shot comes from: a launch monitor that measured it, or no
//! device at all.
//!
//! Every actor type that puts shots on the bus is a [`ShotSource`], and every
//! source has a [`Source`]. [`ShotSource::provenance`] is the single
//! exhaustive match, so a new source type does not compile until it is given
//! a class. An actor declares its source ([`Actor::source`](super::Actor)):
//! when it starts, the round policy records the source by actor ID
//! ([`RoundPolicy`](crate::state::round::RoundPolicy)), and a launch
//! monitor's `bus.actor` carries the class in its `device` block.

pub use flighthook::Source;

/// A type of actor that puts shots on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotSource {
    /// FlightScope Mevo, Mevo+ and Xi (ironsight).
    Mevo,
    /// Garmin R10 (tenover).
    R10,
    /// Square Golf Omni and Home (allsquare).
    Square,
    /// ExPutt RG (threeputt).
    ExPutt,
    /// A trackball turned into a launch monitor (thumbswing): a swing on the
    /// ball becomes a synthesized shot.
    Trackball,
    /// The mock launch monitor's random shots.
    MockMonitor,
    /// The Open Connect input: shots a monitor's own app sends to
    /// flighthook as if it were GSPro.
    OpenConnectServer,
    /// Auto-chip's simulated chips ([`flighthook::AUTO_CHIP_ACTOR`]).
    AutoChip,
    /// The Pureplay Accessory card: a local player's shot in Pureplay, from whatever
    /// launch monitor Pureplay reads, with the values the game flew it with.
    /// A simulator.
    PureplayAccessory,
}

impl ShotSource {
    /// Every source type.
    #[cfg(test)]
    pub const ALL: [ShotSource; 9] = [
        Self::Mevo,
        Self::R10,
        Self::Square,
        Self::ExPutt,
        Self::Trackball,
        Self::MockMonitor,
        Self::OpenConnectServer,
        Self::AutoChip,
        Self::PureplayAccessory,
    ];

    /// Where this source's shots come from. The Open Connect input is
    /// measured: a launch monitor's own app sends it the shots that monitor
    /// measured. The trackball (a stroke turned into a shot) is virtual,
    /// like the mock and Auto-chip. Pureplay's shots are virtual: the game
    /// flies them with its own adjustments, from a device flighthook never
    /// saw.
    #[must_use]
    pub fn provenance(self) -> Source {
        match self {
            Self::Mevo | Self::R10 | Self::Square | Self::ExPutt | Self::OpenConnectServer => {
                Source::Measured
            }
            Self::Trackball | Self::MockMonitor | Self::AutoChip | Self::PureplayAccessory => {
                Source::Virtual
            }
        }
    }

    /// Whether the source is a launch monitor card (configured, with a
    /// status and readiness of its own); Auto-chip is not.
    #[must_use]
    pub fn is_launch_monitor(self) -> bool {
        match self {
            Self::Mevo
            | Self::R10
            | Self::Square
            | Self::ExPutt
            | Self::Trackball
            | Self::MockMonitor
            | Self::OpenConnectServer
            | Self::PureplayAccessory => true,
            Self::AutoChip => false,
        }
    }

    /// Whether the source is a simulator: its shots are the simulator's
    /// own, so no simulator output takes them, nor its readiness
    /// ([`Feed::feeds_simulators`](super::simout::Feed::feeds_simulators)).
    #[must_use]
    pub fn is_simulator(self) -> bool {
        match self {
            Self::PureplayAccessory => true,
            Self::Mevo
            | Self::R10
            | Self::Square
            | Self::ExPutt
            | Self::Trackball
            | Self::MockMonitor
            | Self::OpenConnectServer
            | Self::AutoChip => false,
        }
    }
}

/// A config with one section of every actor type, for tests that walk
/// every actor a config can create.
#[cfg(test)]
pub(crate) fn every_actor_config() -> flighthook::FlighthookConfig {
    use flighthook::{
        CameraSection, ExPuttSection, FlighthookConfig, MevoSection, MockMonitorSection,
        OpenConnectClientSection, OpenConnectServerSection, PureplayAccessorySection,
        PureplaySection, R10Section, RandomClubSection, SquareSection, TrackballSection,
        WebserverSection,
    };
    let mut config = FlighthookConfig::default();
    config.webserver.insert(
        "0".into(),
        WebserverSection {
            name: String::new(),
            bind: "127.0.0.1:0".into(),
        },
    );
    config.mevo.insert("0".into(), MevoSection::default());
    config.r10.insert("0".into(), R10Section::default());
    config.square.insert("0".into(), SquareSection::default());
    config.exputt.insert("0".into(), ExPuttSection::default());
    config
        .trackball
        .insert("0".into(), TrackballSection::default());
    config.mock_monitor.insert(
        "0".into(),
        MockMonitorSection {
            actor_id: None,
            name: String::new(),
            enabled: true,
            impact_lead_s: None,
            driver: Default::default(),
        },
    );
    config
        .openconnect_server
        .insert("0".into(), OpenConnectServerSection::default());
    config
        .openconnect_client
        .insert("0".into(), OpenConnectClientSection::default());
    config
        .pureplay
        .insert("0".into(), PureplaySection::default());
    config
        .pureplay_accessory
        .insert("0".into(), PureplayAccessorySection::default());
    config.random_club.insert(
        "0".into(),
        RandomClubSection {
            actor_id: None,
            name: String::new(),
            enabled: true,
        },
    );
    config.camera.insert("0".into(), CameraSection::default());
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_has_a_class() {
        use Source::{Measured, Virtual};
        let classes: Vec<(ShotSource, Source, bool, bool)> = ShotSource::ALL
            .into_iter()
            .map(|s| (s, s.provenance(), s.is_launch_monitor(), s.is_simulator()))
            .collect();
        assert_eq!(
            classes,
            [
                (ShotSource::Mevo, Measured, true, false),
                (ShotSource::R10, Measured, true, false),
                (ShotSource::Square, Measured, true, false),
                (ShotSource::ExPutt, Measured, true, false),
                (ShotSource::Trackball, Virtual, true, false),
                (ShotSource::MockMonitor, Virtual, true, false),
                (ShotSource::OpenConnectServer, Measured, true, false),
                (ShotSource::AutoChip, Virtual, false, false),
                (ShotSource::PureplayAccessory, Virtual, true, true),
            ]
        );
    }

    /// Every actor a config can create either declares the shot source it
    /// starts as or is one of the types that put no shots on the bus: a new
    /// type has to be sorted into one or the other. Every launch monitor
    /// source has a config section. A simulator source is refused by its
    /// actor ID alone (no source registered) by the outputs and by the
    /// save check exactly when its type says it is a simulator, so a second
    /// simulator source cannot be added to one list only.
    #[test]
    fn every_configurable_actor_is_classified() {
        const NOT_SOURCES: [&str; 5] = [
            "webserver",
            "camera",
            "openconnect_client",
            "pureplay",
            "random_club",
        ];
        let config = super::every_actor_config();
        let resolved = super::super::resolve_actors(&config);
        assert_eq!(resolved.len(), 13, "one actor per section");
        let mut sources = Vec::new();
        for ra in &resolved {
            let (section, _) = ra.id.split_once('.').expect("type.index");
            let source = ra.actor.source();
            assert!(
                source.is_some() != NOT_SOURCES.contains(&section),
                "{}: declare its shot source or list it as none",
                ra.id
            );
            sources.extend(source.map(|s| (section, s)));
        }
        assert_eq!(
            sources,
            [
                ("mevo", ShotSource::Mevo),
                ("r10", ShotSource::R10),
                ("square", ShotSource::Square),
                ("exputt", ShotSource::ExPutt),
                ("trackball", ShotSource::Trackball),
                ("mock_monitor", ShotSource::MockMonitor),
                ("openconnect_server", ShotSource::OpenConnectServer),
                ("pureplay_accessory", ShotSource::PureplayAccessory),
            ]
        );
        for source in ShotSource::ALL {
            assert!(
                !source.is_launch_monitor() || sources.iter().any(|(_, s)| *s == source),
                "{source:?} has a config section"
            );
        }
        let mut config = super::every_actor_config();
        let mut n = 0;
        config.fill_actor_ids(|| {
            n += 1;
            format!("actor-{n}")
        });
        let dir = std::env::temp_dir().join(format!(
            "flighthook-classified-test-{}",
            crate::state::config::generate_id()
        ));
        let path = dir.join("config.toml");
        crate::state::config::save_to(&path, &config);
        let (state, _writer) = crate::state::SystemState::new(path);
        let feed =
            crate::actors::simout::Feed::new(Default::default(), Some(std::sync::Arc::new(state)));
        for (section, source) in &sources {
            let card = format!("{section}.0");
            let id = config.actor_of_card(&card).expect("an actor id");
            assert_eq!(
                feed.feeds_simulators(&id),
                !source.is_simulator(),
                "{card} by its ID"
            );
            assert_eq!(
                flighthook::routed_to_simulator(&config, Some(&id)),
                source.is_simulator(),
                "{card} at save"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
