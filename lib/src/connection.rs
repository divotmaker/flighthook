//! Connection state shown for an actor: derived from its `ActorStatus`
//! reports and its history since it last started.

use serde::{Deserialize, Serialize};

use crate::ActorStatus;

/// How long, in milliseconds, a lost connection is an alert: an actor that
/// was Connected and lost the connection less than this long ago is
/// Reconnecting and red.
pub const RECONNECT_ALERT_MS: u64 = 10 * 60 * 1000;

/// The connection state shown for an actor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// Not Connected since it last started (or restarted after a config
    /// change).
    #[default]
    Connecting,
    Connected,
    /// Connected since it last started, and has since lost the connection.
    Reconnecting,
}

impl ConnectionState {
    /// The badge label: `CONNECTING`, `CONNECTED` or `RECONNECTING`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "CONNECTING",
            Self::Connected => "CONNECTED",
            Self::Reconnecting => "RECONNECTING",
        }
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connecting => write!(f, "connecting"),
            Self::Connected => write!(f, "connected"),
            Self::Reconnecting => write!(f, "reconnecting"),
        }
    }
}

/// An actor's statuses since it last started: whether it has been Connected
/// and until when its last lost connection is an alert. Times are
/// milliseconds on a monotonic clock the caller picks; only differences
/// between them matter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusHistory {
    /// The latest status seen.
    last: Option<ActorStatus>,
    /// Connected at least once since the last start.
    connected: bool,
    /// When the alert for the last Connected → not-connected change ends.
    alert_until: Option<u64>,
}

impl StatusHistory {
    /// The history of an actor that reported `state`, red for `red_for_ms`
    /// more, as received at `now_ms` (a `/api/status` reply).
    #[must_use]
    pub fn received(
        status: ActorStatus,
        state: ConnectionState,
        red_for_ms: u64,
        now_ms: u64,
    ) -> Self {
        Self {
            last: Some(status),
            connected: status == ActorStatus::Connected || state != ConnectionState::Connecting,
            alert_until: (red_for_ms > 0).then(|| now_ms.saturating_add(red_for_ms)),
        }
    }

    /// Record a status reported at `now_ms`. `Starting` begins a new
    /// history; leaving `Connected` starts a new alert window.
    pub fn observe(&mut self, status: ActorStatus, now_ms: u64) {
        match status {
            ActorStatus::Starting => *self = Self::default(),
            ActorStatus::Connected => self.connected = true,
            ActorStatus::Disconnected | ActorStatus::Reconnecting => {
                if self.last == Some(ActorStatus::Connected) {
                    self.alert_until = Some(now_ms.saturating_add(RECONNECT_ALERT_MS));
                }
            }
        }
        self.last = Some(status);
    }

    /// Begin a new history for an actor that was stopped and started again,
    /// keeping only whether it is Connected now.
    pub fn restart(&mut self) {
        *self = Self {
            last: self.last,
            connected: self.last == Some(ActorStatus::Connected),
            alert_until: None,
        };
    }

    /// The state to show.
    #[must_use]
    pub fn state(&self) -> ConnectionState {
        if self.last == Some(ActorStatus::Connected) {
            ConnectionState::Connected
        } else if self.connected {
            ConnectionState::Reconnecting
        } else {
            ConnectionState::Connecting
        }
    }

    /// Milliseconds the state stays red after `now_ms`: what is left of the
    /// alert window while Reconnecting, else 0.
    #[must_use]
    pub fn red_for_ms(&self, now_ms: u64) -> u64 {
        match (self.state(), self.alert_until) {
            (ConnectionState::Reconnecting, Some(until)) => until.saturating_sub(now_ms),
            _ => 0,
        }
    }

    /// Whether the state is red (an alert) at `now_ms`.
    #[must_use]
    pub fn red(&self, now_ms: u64) -> bool {
        self.red_for_ms(now_ms) > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60 * 1000;

    fn seen(statuses: &[(ActorStatus, u64)]) -> StatusHistory {
        let mut h = StatusHistory::default();
        for (status, at) in statuses {
            h.observe(*status, *at);
        }
        h
    }

    #[test]
    fn never_connected_is_connecting_and_never_red() {
        for status in [
            ActorStatus::Starting,
            ActorStatus::Disconnected,
            ActorStatus::Reconnecting,
        ] {
            let h = seen(&[(ActorStatus::Starting, 0), (status, 1000)]);
            assert_eq!(h.state(), ConnectionState::Connecting, "{status:?}");
            assert!(!h.red(1000), "{status:?}");
        }
        assert_eq!(
            StatusHistory::default().state(),
            ConnectionState::Connecting
        );
    }

    #[test]
    fn connected_then_a_drop_is_red_reconnecting_for_ten_minutes() {
        let mut h = seen(&[(ActorStatus::Starting, 0), (ActorStatus::Connected, 5)]);
        assert_eq!(h.state(), ConnectionState::Connected);
        assert!(!h.red(5));
        h.observe(ActorStatus::Disconnected, 100);
        assert_eq!(h.state(), ConnectionState::Reconnecting);
        assert!(h.red(100));
        assert_eq!(h.red_for_ms(100), RECONNECT_ALERT_MS);
        assert_eq!(h.red_for_ms(100 + 4 * MIN), 6 * MIN);
        assert!(h.red(100 + 10 * MIN - 1));
        // Yellow after the window, still Reconnecting.
        assert!(!h.red(100 + 10 * MIN));
        assert_eq!(h.state(), ConnectionState::Reconnecting);
    }

    #[test]
    fn another_drop_while_reconnecting_keeps_the_window() {
        let mut h = seen(&[(ActorStatus::Connected, 0), (ActorStatus::Reconnecting, 0)]);
        h.observe(ActorStatus::Disconnected, 3 * MIN);
        h.observe(ActorStatus::Reconnecting, 6 * MIN);
        assert_eq!(h.red_for_ms(6 * MIN), 4 * MIN);
        assert!(!h.red(10 * MIN));
    }

    #[test]
    fn a_new_connect_and_drop_restarts_the_window() {
        let mut h = seen(&[(ActorStatus::Connected, 0), (ActorStatus::Reconnecting, 0)]);
        h.observe(ActorStatus::Connected, 20 * MIN);
        assert!(!h.red(20 * MIN));
        h.observe(ActorStatus::Reconnecting, 25 * MIN);
        assert_eq!(h.red_for_ms(30 * MIN), 5 * MIN);
    }

    #[test]
    fn starting_resets_the_history() {
        let mut h = seen(&[(ActorStatus::Connected, 0), (ActorStatus::Disconnected, 1)]);
        assert!(h.red(1));
        h.observe(ActorStatus::Starting, 2);
        assert_eq!(h.state(), ConnectionState::Connecting);
        assert!(!h.red(2));
        h.observe(ActorStatus::Reconnecting, 3);
        assert_eq!(h.state(), ConnectionState::Connecting);
        assert!(!h.red(3));
    }

    #[test]
    fn a_restart_drops_the_alert_and_keeps_only_connected_now() {
        let mut h = seen(&[(ActorStatus::Connected, 0), (ActorStatus::Disconnected, 1)]);
        h.restart();
        assert_eq!(h.state(), ConnectionState::Connecting);
        assert!(!h.red(1));
        let mut h = seen(&[(ActorStatus::Connected, 0)]);
        h.restart();
        assert_eq!(h.state(), ConnectionState::Connected);
        h.observe(ActorStatus::Disconnected, 10);
        assert!(h.red(10));
    }

    #[test]
    fn a_received_history_counts_down_from_receipt() {
        let h = StatusHistory::received(
            ActorStatus::Disconnected,
            ConnectionState::Reconnecting,
            5 * MIN,
            1_000,
        );
        assert_eq!(h.state(), ConnectionState::Reconnecting);
        assert_eq!(h.red_for_ms(1_000 + MIN), 4 * MIN);
        assert!(!h.red(1_000 + 5 * MIN));
        // A further Reconnecting is no new drop.
        let mut later = h;
        later.observe(ActorStatus::Reconnecting, 1_000 + MIN);
        assert_eq!(later.red_for_ms(1_000 + MIN), 4 * MIN);

        let yellow = StatusHistory::received(
            ActorStatus::Reconnecting,
            ConnectionState::Reconnecting,
            0,
            0,
        );
        assert_eq!(yellow.state(), ConnectionState::Reconnecting);
        assert!(!yellow.red(0));
        let connecting =
            StatusHistory::received(ActorStatus::Disconnected, ConnectionState::Connecting, 0, 0);
        assert_eq!(connecting.state(), ConnectionState::Connecting);
        // A reply without `state` (default Connecting) for a Connected actor.
        let connected =
            StatusHistory::received(ActorStatus::Connected, ConnectionState::Connecting, 0, 0);
        assert_eq!(connected.state(), ConnectionState::Connected);
    }

    #[test]
    fn labels_and_wire_names() {
        assert_eq!(ConnectionState::Connecting.label(), "CONNECTING");
        assert_eq!(ConnectionState::Connected.label(), "CONNECTED");
        assert_eq!(ConnectionState::Reconnecting.label(), "RECONNECTING");
        assert_eq!(
            serde_json::to_string(&ConnectionState::Reconnecting).unwrap(),
            r#""reconnecting""#
        );
    }
}
