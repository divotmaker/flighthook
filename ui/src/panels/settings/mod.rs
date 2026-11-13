//! Settings: the form behind every Settings card and its sections, one module per concern;
//! the Golfers card and the trackball fields live in their own modules.

mod actor_fields;
mod camera_fields;
mod devices_form;
mod form;
mod general_cards;
mod general_forms;
pub(crate) mod golfers;
mod software_form;
#[cfg(test)]
mod tests;
mod trackball;
mod widgets;

pub(crate) use actor_fields::*;
use camera_fields::*;
pub(crate) use devices_form::*;
pub(crate) use form::*;
pub(crate) use general_forms::*;
pub(crate) use golfers::GolfersForm;
pub(crate) use software_form::*;
pub(crate) use widgets::*;

use std::collections::HashMap;

use crate::app::{CameraList, FlighthookApp};
use crate::net;
use crate::panels::cards::AddPreset;
use crate::panels::devices;
use crate::theme;
use crate::types::{
    CameraListEntry, CameraMode, CameraPosition, CameraSection, Club, Distance, DistanceExt,
    FlightScopeEnvironment, FlighthookConfig, MevoSection, MockMonitorSection,
    OpenConnectClientSection, PureplaySection, R10Section, RandomClubSection, ReplaySection,
    ShotGrouping, SimSurface, SimulationSection, SpinSource, UnitSystem, WebserverSection,
};
use flighthook::ClubExt;
use flighthook::{
    BodyTrackingMode, BodyTrackingSection, DriverConfig, MevoConfig, MockConfig, PoseModel,
    SimMode, SquareConfig,
};
