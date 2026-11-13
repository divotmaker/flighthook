//! The output and simulator-side sections: Open Connect server and client, Pureplay Connect,
//! Pureplay Accessory, random club; the simulator route rule.

use super::*;

/// An Open Connect server instance — a *launch monitor*, not an integration.
///
/// Accepts inbound shot data from launch monitor software that sends GSPro
/// Open Connect V1 as a client. This is the inverse of
/// [`OpenConnectClientSection`], which dials a simulator as a client.
///
/// GSPro listens on 921 as well, but its port is movable: set
/// `<OpenAPIUseAltPort>true</OpenAPIUseAltPort>` in
/// `C:\GSPro\GSPC\GSPconnect.exe.config` to move GSPConnect to 922 and free
/// 921 for this actor, so both can share one host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenConnectServerSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Bind address. Defaults to `0.0.0.0:921`.
    pub bind: Option<String>,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
}

/// An Open Connect client instance — an *output*.
///
/// Dials a simulator that accepts GSPro Open Connect V1 (GSPro and Pureplay
/// on port 921, Muni Golf on 9210) and sends it every shot. This is the inverse of
/// [`OpenConnectServerSection`], which accepts shots from a launch monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenConnectClientSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Simulator address. Defaults to `127.0.0.1:921`.
    pub address: Option<String>,
    /// The FRP actor id of the launch monitor for full-swing shots. A card
    /// key (`mevo.0`) in the file names that card's actor id from the
    /// first load on. None = accept from any monitor.
    #[serde(default)]
    pub full_monitor: Option<String>,
    /// The FRP actor id of the launch monitor for chipping shots. None =
    /// accept from any monitor.
    #[serde(default)]
    pub chipping_monitor: Option<String>,
    /// The FRP actor id of the launch monitor for putting shots. None =
    /// accept from any monitor.
    #[serde(default)]
    pub putting_monitor: Option<String>,
    /// Clubs the simulator holds for a chip: with either list not empty,
    /// the card sets the mode from the simulator's club (a putting club:
    /// putting, a chipping club: chipping, any other: full). Both empty:
    /// the card sends no mode. Defaults to GW, SW and LW.
    #[serde(default = "default_chipping_clubs")]
    pub chipping_clubs: Vec<Club>,
    /// Clubs the simulator holds for a putt (see
    /// [`chipping_clubs`](Self::chipping_clubs)). Defaults to the putter.
    #[serde(default = "default_putting_clubs")]
    pub putting_clubs: Vec<Club>,
}

/// Why a save is refused that routes an output to a Pureplay Accessory card: a
/// simulator's own shots never go to a simulator output.
pub const SIMULATOR_ROUTE_ERROR: &str = "An output can't take shots from Pureplay";

/// Whether `routed`, an output's routed launch monitor (an actor id, or a
/// card key not converted yet), is a Pureplay Accessory card in `config`: a
/// simulator whose shots no output takes.
#[must_use]
pub fn routed_to_simulator(config: &FlighthookConfig, routed: Option<&str>) -> bool {
    routed.is_some_and(|id| {
        config
            .card_of_actor(id)
            .as_deref()
            .unwrap_or(id)
            .starts_with("pureplay_accessory.")
    })
}

impl OpenConnectClientSection {
    /// The mode the card holds for the simulator's `club`: `None` when both
    /// club lists are empty (the card sends no mode); else putting for a
    /// club in [`putting_clubs`](Self::putting_clubs), chipping for one in
    /// [`chipping_clubs`](Self::chipping_clubs), full for any other.
    #[must_use]
    pub fn club_mode(&self, club: Option<Club>) -> Option<Mode> {
        if self.chipping_clubs.is_empty() && self.putting_clubs.is_empty() {
            return None;
        }
        Some(match club {
            Some(c) if self.putting_clubs.contains(&c) => Mode::Putting,
            Some(c) if self.chipping_clubs.contains(&c) => Mode::Chipping,
            _ => Mode::Full,
        })
    }

    /// Whether a mode routes this output to a Pureplay Accessory card
    /// ([`routed_to_simulator`]).
    #[must_use]
    pub fn routes_to_simulator(&self, config: &FlighthookConfig) -> bool {
        [
            &self.full_monitor,
            &self.chipping_monitor,
            &self.putting_monitor,
        ]
        .into_iter()
        .any(|m| routed_to_simulator(config, m.as_deref()))
    }
}

impl PureplaySection {
    /// Whether a mode routes this output to a Pureplay Accessory card
    /// ([`routed_to_simulator`]).
    #[must_use]
    pub fn routes_to_simulator(&self, config: &FlighthookConfig) -> bool {
        [
            &self.full_monitor,
            &self.chipping_monitor,
            &self.putting_monitor,
        ]
        .into_iter()
        .any(|m| routed_to_simulator(config, m.as_deref()))
    }
}

/// Deprecated alias of [`OpenConnectClientSection`].
#[deprecated(note = "use OpenConnectClientSection")]
pub type GsProSection = OpenConnectClientSection;

/// A Pureplay output instance (alpha).
///
/// Dials Pureplay (port 9760) and sends it every routed shot over the
/// Pureplay Connect protocol, taking its club, hand and strict-mode
/// commands. Routing works as for [`OpenConnectClientSection`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PureplaySection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Pureplay's address. Defaults to `127.0.0.1:9760`.
    pub address: Option<String>,
    /// The FRP actor id of the launch monitor for full-swing shots. A card
    /// key (`mevo.0`) in the file names that card's actor id from the
    /// first load on. None = accept from any monitor.
    #[serde(default)]
    pub full_monitor: Option<String>,
    /// The FRP actor id of the launch monitor for chipping shots. None =
    /// accept from any monitor.
    #[serde(default)]
    pub chipping_monitor: Option<String>,
    /// The FRP actor id of the launch monitor for putting shots. None =
    /// accept from any monitor.
    #[serde(default)]
    pub putting_monitor: Option<String>,
}

/// A Pureplay Accessory card (beta), under Software.
///
/// Dials Pureplay's Accessory API (a WebSocket on port 9770), pairs with
/// it, and follows the round: Pureplay's club and hand become flighthook's,
/// a local player's shot becomes a flighthook shot (as [`shots`](Self::shots)
/// says), and Pureplay's result for a shot is kept on it. The pairing token
/// is kept in a file of its own in the user's data folder, never in this
/// section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PureplayAccessorySection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// Pureplay's address, `ip:port`. Defaults to `127.0.0.1:9770`.
    pub address: Option<String>,
    /// Which shots the card takes from Pureplay. Defaults to
    /// [`PureplayShots::Auto`], and is left out of the config file then.
    #[serde(default, skip_serializing_if = "PureplayShots::is_default")]
    pub shots: PureplayShots,
    /// Seconds before the shot's report that the swing's impact happened, for
    /// the cameras' clips (beta), 0 to [`MAX_IMPACT_LEAD_S`]. Absent uses the
    /// device's default ([`impact_lead_default_s`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_lead_s: Option<f64>,
}

/// Which of Pureplay's shots the Pureplay Accessory card takes (beta). A shot
/// flighthook sent to Pureplay comes back from it; the card tells such a
/// shot by the time flighthook sent it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PureplayShots {
    /// Each shot of a player at this computer that is not one flighthook
    /// sent to Pureplay. The default.
    #[default]
    Auto,
    /// Every shot of a player at this computer: the launch monitor is
    /// connected to Pureplay.
    FromPureplay,
    /// None: the launch monitor is connected to flighthook, which sends its
    /// shots to Pureplay. Pureplay's results still reach those shots.
    FromFlighthook,
}

impl PureplayShots {
    /// True for the default, [`PureplayShots::Auto`].
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Stable key used by the settings UI and config file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::FromPureplay => "from_pureplay",
            Self::FromFlighthook => "from_flighthook",
        }
    }
}

/// A random club cycling integration instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RandomClubSection {
    #[serde(default)]
    pub name: String,
    /// Whether the card is on. A section that is off keeps its settings
    /// and runs no actor. Defaults to true, and is left out of the config
    /// file then.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The card's FRP actor id, generated once and kept, so the card stays
    /// the same actor across restarts. Absent until flighthook first runs
    /// with the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
}
