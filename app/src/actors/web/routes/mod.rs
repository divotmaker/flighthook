//! REST endpoint handlers, one module per resource. Shared here: sending a config
//! command and waiting for its outcome.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::broadcast;

use super::types::{
    AutoChipRequest, AutoChipResponse, FindMatRequest, GroupingRequest, PostSettingsResponse,
    StatusResponse,
};
use super::{MAX_SHOTS, StoredShot, WebState, export};
use crate::actors::actor_names;
use crate::bus::{BusMsg, LocalEvent};
use crate::state::config::FlighthookConfig;
use flighthook::{CameraListResponse, ConfigAction, ShotData, ShotVideo, UnitSystem};
use flightrelay::{FrpMessage, GolferSelect};

mod blocks;
mod body_tracking;
#[cfg(test)]
mod body_tracking_models_tests;
mod cameras;
mod commands;
#[cfg(test)]
mod convert_tests;
#[cfg(test)]
mod group_tests;
#[cfg(test)]
mod live_tests;
mod reconnect;
#[cfg(test)]
mod reconnect_tests;
mod settings;
#[cfg(test)]
mod shot_fields_tests;
mod shots;
mod status;
#[cfg(test)]
mod tests;
mod tournament;
mod ui_assets;
mod video;

pub use blocks::*;
pub use body_tracking::*;
pub use cameras::*;
pub use commands::*;
pub use reconnect::*;
pub use settings::*;
pub use shots::*;
pub use status::*;
pub use tournament::*;
pub use ui_assets::*;
pub use video::*;

/// A `ConfigOutcome`'s actor lists.
#[derive(Debug)]
struct Outcome {
    restarted: Vec<String>,
    stopped: Vec<String>,
}

/// Emit a `ConfigCommand` for `action` and wait up to `timeout` for the
/// system actor's matching `ConfigOutcome`. Returns it, or `None` on
/// timeout or a closed bus.
async fn send_config_command_within(
    state: &WebState,
    action: ConfigAction,
    timeout: Duration,
) -> Option<Outcome> {
    tokio::time::timeout(timeout, send_config_command(state, action))
        .await
        .ok()
        .flatten()
}

/// Emit a `ConfigCommand` for `action` now. The future resolves to the
/// system actor's matching `ConfigOutcome`, or `None` once the bus closes.
fn send_config_command(
    state: &WebState,
    action: ConfigAction,
) -> impl Future<Output = Option<Outcome>> + Send + 'static {
    let request_id = crate::state::config::generate_id();
    // Subscribed before the command goes out, so its outcome cannot be
    // missed.
    let mut bus_rx = state.bus_tx.subscribe();
    let _ = state.bus_tx.send(BusMsg::local(
        "web",
        LocalEvent::ConfigCommand {
            request_id: Some(request_id.clone()),
            action: Box::new(action),
        },
    ));
    async move {
        loop {
            match bus_rx.recv().await {
                Ok(msg) => {
                    if let Some((
                        _,
                        LocalEvent::ConfigOutcome {
                            request_id: Some(rid),
                            restarted,
                            stopped,
                            ..
                        },
                    )) = msg.as_local()
                        && *rid == request_id
                    {
                        return Some(Outcome {
                            restarted: restarted.clone(),
                            stopped: stopped.clone(),
                        });
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => {}
            }
        }
    }
}
