//! Settings: the list of items ([`settings_nav`]) and the selected item's
//! page: the settings-only Global, Simulation, Replay, Body tracking and
//! golfer pages, and one page per web server, launch monitor, camera and
//! output.
//!
//! While any page has unsaved edits Settings is locked to it: the nav icons,
//! the list, the other pages' toggles and edit controls, and the (+)
//! buttons are disabled until the edits are saved or canceled. While a save is out,
//! every page's controls wait for its reply.

mod card;
mod marks;
mod panel;
#[cfg(test)]
mod tests;

pub(crate) use card::*;
pub(crate) use marks::*;

use std::collections::HashMap;

use crate::app::FlighthookApp;
use crate::net;
use crate::panels::cards::{self, CardGroup};
use crate::panels::monitoring;
use crate::panels::remote;
use crate::panels::settings::{self, ActorFormEntry, Pending, PendingRemoval, SaveTarget};
use crate::panels::settings_nav::{self, SettingsItem, SettingsSection};
use crate::panels::shots;
use crate::theme;
use crate::types::{ActorStatus, ActorStatusResponse};
