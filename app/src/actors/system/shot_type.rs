//! `ShotTypeReset`: puts a mode left unheld back to full, once per change.

use super::*;

/// Puts a mode left behind back to full. The mode is "unheld" while the
/// Shot Type picker is hidden (nothing it steers is configured) and no
/// connected card sets the shot type itself
/// ([`FlighthookConfig::card_sets_shot_type`]). Each time that goes from
/// false to true after start-up, whatever caused it (a config command, or
/// the last connected card that set the mode disconnecting or going), the
/// mode goes back to full, once. A mode already full publishes nothing.
pub(crate) struct ShotTypeReset {
    /// The cards whose newest status is Connected.
    pub(super) connected: std::collections::HashSet<String>,
    /// Whether the mode was unheld at the last check.
    pub(super) unheld: bool,
}

impl ShotTypeReset {
    /// The state at start-up, judged from `config` with no card connected:
    /// start-up itself never resets.
    pub(crate) fn new(config: &FlighthookConfig) -> Self {
        let connected = std::collections::HashSet::new();
        let unheld = Self::judge(config, &connected);
        Self { connected, unheld }
    }

    pub(super) fn judge(
        config: &FlighthookConfig,
        connected: &std::collections::HashSet<String>,
    ) -> bool {
        !config.shot_type_available() && !connected.iter().any(|id| config.card_sets_shot_type(id))
    }

    /// Follow a card's status; whether the set of connected cards changed.
    /// A card that is turned off or removed sends no last status: [`check`]
    /// drops it.
    ///
    /// [`check`]: ShotTypeReset::check
    pub(crate) fn observe(&mut self, msg: &BusMsg) -> bool {
        match msg.as_local() {
            Some((card, LocalEvent::CardStatus { status, .. })) => {
                if *status == ActorStatus::Connected {
                    self.connected.insert(card.to_owned())
                } else {
                    self.connected.remove(card)
                }
            }
            _ => false,
        }
    }

    /// Judge the saved config and the connected cards; reset the mode to
    /// full when it has just become unheld. Cards no longer configured, or
    /// turned off, leave the connected set first.
    pub(crate) fn check(
        &mut self,
        state: &SystemState,
        writer: &SessionWriter,
        sender: &BusSender,
    ) {
        let config = state.system.snapshot();
        let cards = config.card_actors();
        self.connected
            .retain(|id| config.actor_enabled(id) && cards.iter().any(|(key, _)| key == id));
        let unheld = Self::judge(&config, &self.connected);
        let became = unheld && !self.unheld;
        self.unheld = unheld;
        if !became {
            return;
        }
        let own = state.flighthook_id();
        let select = flightrelay::GolferSelect {
            mode: Some(flightrelay::Mode::Full),
            ..flightrelay::GolferSelect::new(own.clone())
        };
        if let Some(turn) = writer.apply_from(&own, &select) {
            sender.frp_caused(turn, Some(own));
        }
    }
}
