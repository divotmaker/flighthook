//! The Pureplay links in strict mode (a recorded tournament round).
//!
//! A Pureplay output holds its actor ID here from the StrictModeAck(1) it
//! sends until strict mode ends, its link drops or the actor stops. While
//! any is held, Auto-chip does not fire.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// The actor IDs of the Pureplay outputs whose link is in strict mode.
/// Clones share the set.
#[derive(Debug, Clone, Default)]
pub struct StrictRounds(Arc<Mutex<HashSet<String>>>);

impl StrictRounds {
    fn set(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `actor`'s link is in strict mode.
    pub fn hold(&self, actor: &str) {
        self.set().insert(actor.to_owned());
    }

    /// `actor`'s link is no longer in strict mode.
    pub fn release(&self, actor: &str) {
        self.set().remove(actor);
    }

    /// Whether any link is in strict mode.
    #[must_use]
    pub fn any(&self) -> bool {
        !self.set().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_holds_until_every_link_releases_it() {
        let rounds = StrictRounds::default();
        let shared = rounds.clone();
        assert!(!rounds.any());
        rounds.hold("pureplay.0");
        shared.hold("pureplay.1");
        rounds.hold("pureplay.0");
        assert!(shared.any());
        rounds.release("pureplay.0");
        assert!(rounds.any(), "pureplay.1 still holds it");
        shared.release("pureplay.1");
        assert!(!rounds.any());
    }
}
