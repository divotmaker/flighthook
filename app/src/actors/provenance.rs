//! Where a shot comes from, and whether a recorded round can count it.
//!
//! Every actor that puts shots on the bus is a [`ShotSource`], and every
//! source has a [`Provenance`]. Both are single exhaustive matches, so a new
//! source type does not compile until it is given a class. Pureplay's strict
//! mode (a tournament round) takes shots from [`Provenance::Measured`]
//! sources only.

/// What stands behind a source's shots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// A flighthook driver reads a physical launch monitor directly.
    Measured,
    /// No physical launch monitor measured the shot: the mock launch
    /// monitor, Auto-chip, and any simulated or replayed source. The
    /// trackball launch monitor (a stroke turned into a shot) is virtual
    /// too, and goes here when it is added.
    Virtual,
    /// The shot came from another application, so flighthook cannot vouch
    /// for the device that measured it: the Open Connect input.
    Relayed,
}

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
    pub const ALL: [ShotSource; 7] = [
        Self::Mevo,
        Self::R10,
        Self::Square,
        Self::ExPutt,
        Self::MockMonitor,
        Self::OpenConnectServer,
        Self::AutoChip,
    ];

    /// The source of actor `actor` (an actor ID such as `mevo.0`, or
    /// [`flighthook::AUTO_CHIP_ACTOR`]); `None` for an actor that is not a
    /// shot source.
    #[must_use]
    pub fn of(actor: &str) -> Option<Self> {
        if actor == flighthook::AUTO_CHIP_ACTOR {
            return Some(Self::AutoChip);
        }
        let (type_key, _) = actor.split_once('.')?;
        Self::ALL
            .into_iter()
            .find(|s| s.type_key() == Some(type_key))
    }

    /// The config section and actor ID prefix of a configured source;
    /// `None` for Auto-chip, which has no section and a fixed actor ID.
    #[must_use]
    pub fn type_key(self) -> Option<&'static str> {
        match self {
            Self::Mevo => Some("mevo"),
            Self::R10 => Some("r10"),
            Self::Square => Some("square"),
            Self::ExPutt => Some("exputt"),
            Self::MockMonitor => Some("mock_monitor"),
            Self::OpenConnectServer => Some("openconnect_server"),
            Self::AutoChip => None,
        }
    }

    /// What stands behind this source's shots.
    #[must_use]
    pub fn provenance(self) -> Provenance {
        match self {
            Self::Mevo | Self::R10 | Self::Square | Self::ExPutt => Provenance::Measured,
            Self::MockMonitor | Self::AutoChip => Provenance::Virtual,
            Self::OpenConnectServer => Provenance::Relayed,
        }
    }

    /// Whether the source is a launch monitor card (configured, with a
    /// status and readiness of its own); Auto-chip is not.
    #[must_use]
    pub fn is_launch_monitor(self) -> bool {
        self.type_key().is_some()
    }
}

/// Why a shot from `actor`, a source that is not measured (or not a known
/// source), cannot count in a recorded round; `None` when it can. `name` is
/// the actor's display name.
#[must_use]
pub fn not_measured_reason(actor: &str, name: &str) -> Option<String> {
    match ShotSource::of(actor).map(ShotSource::provenance) {
        Some(Provenance::Measured) => None,
        Some(Provenance::Virtual) => Some(format!("{name} is a virtual launch monitor")),
        Some(Provenance::Relayed) => Some(format!("{name} relays shots from another app")),
        None => Some(format!("{name} is not a launch monitor flighthook reads")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each source's class, and its actor IDs found back.
    #[test]
    fn every_source_has_a_class() {
        use Provenance::{Measured, Relayed, Virtual};
        let classes: Vec<(ShotSource, Provenance)> = ShotSource::ALL
            .into_iter()
            .map(|s| (s, s.provenance()))
            .collect();
        assert_eq!(
            classes,
            [
                (ShotSource::Mevo, Measured),
                (ShotSource::R10, Measured),
                (ShotSource::Square, Measured),
                (ShotSource::ExPutt, Measured),
                (ShotSource::MockMonitor, Virtual),
                (ShotSource::OpenConnectServer, Relayed),
                (ShotSource::AutoChip, Virtual),
            ]
        );
        for source in ShotSource::ALL {
            let actor = match source.type_key() {
                Some(t) => format!("{t}.3"),
                None => flighthook::AUTO_CHIP_ACTOR.to_owned(),
            };
            assert_eq!(ShotSource::of(&actor), Some(source), "{actor}");
        }
        assert!(!ShotSource::AutoChip.is_launch_monitor());
        assert!(ShotSource::MockMonitor.is_launch_monitor());
    }

    /// Every actor a config can create is either a shot source or one of
    /// the types that put no shots on the bus: a new type has to be sorted
    /// into one or the other.
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
            },
        );
        config.camera.insert("0".into(), CameraSection::default());

        let ids: Vec<String> = super::super::resolve_actors(&config, None)
            .into_iter()
            .map(|a| a.id)
            .collect();
        let types: Vec<&str> = ids
            .iter()
            .filter_map(|id| id.split_once('.').map(|(t, _)| t))
            .collect();
        assert_eq!(types.len(), 11, "one actor per section: {ids:?}");
        for id in &ids {
            let (t, _) = id.split_once('.').expect("type.index");
            assert!(
                ShotSource::of(id).is_some() != NOT_SOURCES.contains(&t),
                "{id}: classify it as a shot source or list it as none"
            );
        }
        for source in ShotSource::ALL {
            if let Some(t) = source.type_key() {
                assert!(types.contains(&t), "{t} has a config section");
            }
        }
    }

    #[test]
    fn the_reason_names_the_device_and_its_class() {
        assert_eq!(not_measured_reason("mevo.0", "Mevo+"), None);
        assert_eq!(not_measured_reason("r10.0", "R10"), None);
        assert_eq!(not_measured_reason("square.0", "Omni"), None);
        assert_eq!(not_measured_reason("exputt.0", "ExPutt"), None);
        assert_eq!(
            not_measured_reason("mock_monitor.0", "Mock").as_deref(),
            Some("Mock is a virtual launch monitor")
        );
        assert_eq!(
            not_measured_reason(flighthook::AUTO_CHIP_ACTOR, "Auto-chip").as_deref(),
            Some("Auto-chip is a virtual launch monitor")
        );
        assert_eq!(
            not_measured_reason("openconnect_server.0", "OpenConnect input").as_deref(),
            Some("OpenConnect input relays shots from another app")
        );
        assert_eq!(
            not_measured_reason("thing.0", "Thing").as_deref(),
            Some("Thing is not a launch monitor flighthook reads")
        );
    }
}
