//! `Default` for the config and each section that has non-trivial defaults.

use super::*;

impl Default for FlighthookConfig {
    /// Minimal empty config — webserver only, no devices or integrations.
    /// The setup wizard will add devices on first startup.
    fn default() -> Self {
        let mut webserver = std::collections::HashMap::new();
        webserver.insert(
            "0".into(),
            WebserverSection {
                name: "Web Server".into(),
                bind: "0.0.0.0:5880".into(),
            },
        );
        Self {
            default_units: UnitSystem::default(),
            frp: FrpSection::default(),
            selected_golfer: None,
            shot_grouping: ShotGrouping::default(),
            simulation: SimulationSection::default(),
            replay: ReplaySection::default(),
            body_tracking: BodyTrackingSection::default(),
            golfers: Vec::new(),
            webserver,
            mevo: std::collections::HashMap::new(),
            r10: std::collections::HashMap::new(),
            square: std::collections::HashMap::new(),
            exputt: std::collections::HashMap::new(),
            trackball: std::collections::HashMap::new(),
            mock_monitor: std::collections::HashMap::new(),
            openconnect_server: std::collections::HashMap::new(),
            openconnect_client: std::collections::HashMap::new(),
            pureplay: std::collections::HashMap::new(),
            pureplay_accessory: std::collections::HashMap::new(),
            random_club: std::collections::HashMap::new(),
            camera: std::collections::HashMap::new(),
        }
    }
}

impl Default for MevoSection {
    fn default() -> Self {
        Self {
            actor_id: None,
            name: "Mevo WiFi".into(),
            enabled: true,
            launch_only_spin: Self::DEFAULT_LAUNCH_ONLY_SPIN,
            impact_lead_s: None,
            driver: MevoConfig {
                address: Some("192.168.2.1:5100".into()),
                ball_type: Some(1),
                tee_height: Some(Distance::Inches(1.5)),
                range: Some(Distance::Feet(8.0)),
                surface_height: Some(Distance::Inches(0.0)),
                track_pct: Some(80.0),
                ..MevoConfig::default()
            },
        }
    }
}

impl Default for R10Section {
    fn default() -> Self {
        Self {
            actor_id: None,
            name: "Garmin R10".into(),
            enabled: true,
            impact_lead_s: None,
            // No range: leave the device's own tee distance untouched.
            driver: R10Config::default(),
        }
    }
}

impl Default for SquareSection {
    fn default() -> Self {
        Self {
            actor_id: None,
            name: "Square Golf Omni".into(),
            enabled: true,
            impact_lead_s: None,
            // Auto-discover, arm the 7-iron, advanced spin, discard
            // zero-spin misreads, built-in calibration.
            driver: SquareConfig::default(),
        }
    }
}

impl Default for TrackballSection {
    /// What a new Trackball card writes: its name.
    fn default() -> Self {
        Self {
            name: "Trackball".into(),
            enabled: true,
            actor_id: None,
            driver: TrackballConfig {
                config: thumbswing::Config {
                    grab: false,
                    ..thumbswing::Config::default()
                },
                ..TrackballConfig::default()
            },
        }
    }
}

impl Default for ExPuttSection {
    fn default() -> Self {
        Self {
            actor_id: None,
            name: "ExPutt".into(),
            enabled: true,
            impact_lead_s: None,
            // Found on the network automatically.
            driver: ExPuttConfig::default(),
        }
    }
}

impl Default for OpenConnectServerSection {
    fn default() -> Self {
        Self {
            actor_id: None,
            name: "Open Connect Server".into(),
            enabled: true,
            bind: Some("0.0.0.0:921".into()),
            impact_lead_s: None,
        }
    }
}

impl Default for OpenConnectClientSection {
    fn default() -> Self {
        Self {
            actor_id: None,
            chipping_clubs: crate::default_chipping_clubs(),
            putting_clubs: crate::default_putting_clubs(),
            name: "GSPro".into(),
            enabled: true,
            address: Some("127.0.0.1:921".into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
        }
    }
}

impl Default for PureplaySection {
    fn default() -> Self {
        Self {
            actor_id: None,
            name: "Pureplay".into(),
            enabled: true,
            address: Some("127.0.0.1:9760".into()),
            full_monitor: None,
            chipping_monitor: None,
            putting_monitor: None,
        }
    }
}

impl Default for PureplayAccessorySection {
    fn default() -> Self {
        Self {
            actor_id: None,
            name: "Pureplay Accessory".into(),
            enabled: true,
            address: Some("127.0.0.1:9770".into()),
            shots: PureplayShots::Auto,
            impact_lead_s: None,
        }
    }
}
