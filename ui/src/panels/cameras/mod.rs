//! The Cameras tab's pane (beta): with body tracking (alpha), the golfer's
//! skeleton over every camera's picture, and the golf numbers measured on
//! it. The Shots and Cameras tabs share the shots navigation; the nav icons
//! pick the tab ([`ShotsPane`]), and the Cameras tab's toolbar Live switch
//! shows the cameras live.
//!
//! - **A shot:** its clips play inline, side by side, with the replay's
//!   transport controls ([`replay`]); under each, that camera's swing
//!   analysis: tempo and the view's metrics at address, top and impact; the
//!   swing events are marked on the scrubber. A shot without clips shows each
//!   camera's status instead.
//! - **Live:** each configured camera's live picture with its skeleton and,
//!   under it, the golfer's posture for that camera's view, measured on the
//!   server with each frame. Each view runs its own fetch loop on
//!   `GET /api/cameras/{actor}/live`: a request goes out only once the last
//!   one is answered, asking for a frame newer than the one shown, and the
//!   server holds it until there is one (up to a second). A frame asks for
//!   a repaint; it is decoded on the next screen update into the view's
//!   texture, and the keypoints and posture that came with it (`X-Pose`) are
//!   drawn with that picture, so they always match it. The loops run only
//!   while Live is on screen: elsewhere, or with the window minimized or
//!   the browser tab hidden, the views are dropped, and the server stops
//!   updating the live frames a few seconds later.
//!
//! Body tracking's settings are on the Devices tab's Body tracking card; a
//! single line under the live views reads out the values and each camera's
//! measured rate and pose time.

mod live;
mod pane;
mod shot;
mod swing;
#[cfg(test)]
mod tests;

pub(crate) use live::*;
pub(crate) use pane::*;
pub(crate) use shot::*;
use std::collections::{HashMap, VecDeque};
pub(crate) use swing::*;

use crate::app::{FlighthookApp, new_actor};
use crate::net::{LiveReply, PoseState};
use crate::panels::shots::{ShotsLayout, ShotsView};
use crate::panels::{Tab, devices, monitoring, replay, shots, signs};
use crate::replay::decode;
use crate::theme;
use crate::types::{
    BodyTrackingMode, BodyTrackingSection, CameraPosition, CameraSection, ConnectionState,
    Handedness, LivePose, LivePosture, SwingReportFile, SwingTempo, SwingView, UnitSystem,
};
use flighthook::{Fit, MetricGrade, ReferenceGrades, ReferenceKind, SwingPosition};

/// Space over the Last shot strip.
const LAST_SHOT_GAP: f32 = 10.0;

/// How long a view waits after a failed request before the next (s).
const RETRY_S: f64 = 1.0;
/// Space between two cameras' columns.
const GAP: f32 = 12.0;
/// Height of a column's heading row.
const HEADING_H: f32 = 24.0;
/// A picture's height over its width until the camera's size is known.
const DEFAULT_ASPECT: f32 = 9.0 / 16.0;
/// Height of the readout line under the live views, with its gap.
const READOUT_H: f32 = 26.0;
/// Height of the pane's title row (the shot's header, or Live).
const TITLE_H: f32 = 40.0;
/// Height of a row of number tiles (a live posture, a tempo), with its gap
/// above.
const TILES_H: f32 = 52.0;
/// Tallest a shot's clips get, as a share of the pane's height.
const PLAYER_SHARE: f32 = 0.62;
/// Where the Shots tab's pane choice is kept on this device.
const PANE_KEY: &str = "shots_pane";
/// Shown for a number that was not measured.
const NONE: &str = "\u{2014}";
/// The span a view's frame rate is measured over (s).
const FPS_WINDOW_S: f64 = 2.0;
