//! Every local shot source's type ([`SystemState::sources`](super::SystemState)),
//! by actor ID, registered when the actor starts and forgotten when its card
//! is turned off or removed.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::actors::provenance::ShotSource;

/// The shot source registry.
#[derive(Debug, Default)]
pub struct Sources(Mutex<Registry>);

#[derive(Debug, Default)]
struct Registry {
    /// Every shot source's type, by actor ID.
    sources: HashMap<String, ShotSource>,
    /// The actor ID each card registered its source under, by card key.
    cards: HashMap<String, String>,
}

impl Sources {
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record that actor `actor` is a shot source of type `source`.
    pub fn register_source(&self, actor: &str, source: ShotSource) {
        self.lock().sources.insert(actor.to_owned(), source);
    }

    /// [`register_source`](Self::register_source) for card `card`, whose
    /// actor ID is `actor`: [`forget_card`](Self::forget_card) forgets it.
    pub fn register_card_source(&self, card: &str, actor: &str, source: ShotSource) {
        let mut registry = self.lock();
        // A card whose actor id changed (a restore) leaves its old id.
        if let Some(old) = registry.cards.insert(card.to_owned(), actor.to_owned())
            && old != actor
        {
            registry.sources.remove(&old);
        }
        registry.sources.insert(actor.to_owned(), source);
    }

    /// Forget the source card `card` registered, by its actor ID: the card
    /// was turned off or removed. Returns that actor ID.
    pub fn forget_card(&self, card: &str) -> Option<String> {
        let mut registry = self.lock();
        let actor = registry.cards.remove(card)?;
        registry.sources.remove(&actor);
        Some(actor)
    }

    /// The shot source type of actor `actor`, `None` when it is not one.
    #[must_use]
    pub fn source(&self, actor: &str) -> Option<ShotSource> {
        self.lock().sources.get(actor).copied()
    }
}
