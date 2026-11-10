//! Where a shot comes from: a launch monitor that measured it, or no
//! device at all.
//!
//! Every actor type that puts shots on the bus is a [`ShotSource`], and every
//! source has a [`Provenance`]. [`ShotSource::provenance`] is the single
//! exhaustive match, so a new source type does not compile until it is given
//! a class. An actor declares its source ([`Actor::source`](super::Actor));
//! when it starts, its bus sender stamps that class on every shot it
//! triggers, and the round policy records the source by actor ID
//! ([`RoundPolicy`](crate::state::round::RoundPolicy)).

pub use flighthook::Provenance;

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
    /// The mock launch monitor's random shots.
    MockMonitor,
    /// The Open Connect input: shots a monitor's own app sends to
    /// flighthook as if it were GSPro.
    OpenConnectServer,
    /// Auto-chip's simulated chips ([`flighthook::AUTO_CHIP_ACTOR`]).
    AutoChip,
}

impl ShotSource {
    /// Every source type.
    #[cfg(test)]
    pub const ALL: [ShotSource; 7] = [
        Self::Mevo,
        Self::R10,
        Self::Square,
        Self::ExPutt,
        Self::MockMonitor,
        Self::OpenConnectServer,
        Self::AutoChip,
    ];

    /// Where this source's shots come from. The Open Connect input is
    /// measured: a launch monitor's own app sends it the shots that monitor
    /// measured. The trackball launch monitor (a stroke turned into a shot)
    /// is virtual, and goes with the mock and Auto-chip when it is added.
    #[must_use]
    pub fn provenance(self) -> Provenance {
        match self {
            Self::Mevo | Self::R10 | Self::Square | Self::ExPutt | Self::OpenConnectServer => {
                Provenance::Measured
            }
            Self::MockMonitor | Self::AutoChip => Provenance::Virtual,
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
            | Self::MockMonitor
            | Self::OpenConnectServer => true,
            Self::AutoChip => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_has_a_class() {
        use Provenance::{Measured, Virtual};
        let classes: Vec<(ShotSource, Provenance, bool)> = ShotSource::ALL
            .into_iter()
            .map(|s| (s, s.provenance(), s.is_launch_monitor()))
            .collect();
        assert_eq!(
            classes,
            [
                (ShotSource::Mevo, Measured, true),
                (ShotSource::R10, Measured, true),
                (ShotSource::Square, Measured, true),
                (ShotSource::ExPutt, Measured, true),
                (ShotSource::MockMonitor, Virtual, true),
                (ShotSource::OpenConnectServer, Measured, true),
                (ShotSource::AutoChip, Virtual, false),
            ]
        );
    }

    /// Every actor a config can create either declares the shot source it
    /// starts as or is one of the types that put no shots on the bus: a new
    /// type has to be sorted into one or the other. Every launch monitor
    /// source has a config section.
    #[test]
    fn every_configurable_actor_is_classified() {
        use flighthook::{
            CameraSection, ExPuttSection, FlighthookConfig, MevoSection, MockMonitorSection,
            OpenConnectClientSection, OpenConnectServerSection, PureplaySection, R10Section,
            RandomClubSection, SquareSection, WebserverSection,
        };
        const NOT_SOURCES: [&str; 5] = [
            "webserver",
            "camera",
            "openconnect_client",
            "pureplay",
            "random_club",
        ];
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
        config.mock_monitor.insert(
            "0".into(),
            MockMonitorSection {
                name: String::new(),
                enabled: true,
                impact_lead_s: None,
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
        config.random_club.insert(
            "0".into(),
            RandomClubSection {
                name: String::new(),
                enabled: true,
            },
        );
        config.camera.insert("0".into(), CameraSection::default());

        let resolved = super::super::resolve_actors(&config, None);
        assert_eq!(resolved.len(), 11, "one actor per section");
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
                ("mock_monitor", ShotSource::MockMonitor),
                ("openconnect_server", ShotSource::OpenConnectServer),
            ]
        );
        for source in ShotSource::ALL {
            assert!(
                !source.is_launch_monitor() || sources.iter().any(|(_, s)| *s == source),
                "{source:?} has a config section"
            );
        }
    }
}
