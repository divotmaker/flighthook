//! Bus abstraction layer — wraps `tokio::sync::broadcast` so callers never
//! touch the broadcast types directly.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::broadcast;

use flighthook::{FlighthookEvent, FlighthookMessage, Provenance};

// ---------------------------------------------------------------------------
// PollError
// ---------------------------------------------------------------------------

/// Error from `BusReceiver::poll()` — the bus is closed or the actor's
/// shutdown flag is set.
#[derive(Debug)]
pub enum PollError {
    Shutdown,
}

// ---------------------------------------------------------------------------
// BusSender
// ---------------------------------------------------------------------------

/// Cloneable sender that auto-stamps `actor` on every outbound message, and
/// the actor's provenance on every shot trigger when it is a shot source.
pub struct BusSender {
    actor_id: String,
    inner: broadcast::Sender<FlighthookMessage>,
    shutdown: Arc<AtomicBool>,
    /// Set once a new instance of the actor has replaced this one: what this
    /// one still sends is dropped, so it never speaks for its successor.
    muted: Arc<AtomicBool>,
    /// The class of the shots the actor triggers, `None` for an actor that
    /// is not a shot source.
    provenance: Option<Provenance>,
}

impl BusSender {
    pub fn new(
        actor_id: String,
        inner: broadcast::Sender<FlighthookMessage>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            actor_id,
            inner,
            shutdown,
            muted: Arc::default(),
            provenance: None,
        }
    }

    /// This sender stamps `provenance` on every shot trigger it sends: the
    /// class of the shot source it speaks for, from the actor's type.
    #[must_use]
    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = Some(provenance);
        self
    }

    /// The flag that silences this sender and its clones.
    pub(crate) fn muted(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.muted)
    }

    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }

    /// Access the underlying broadcast sender (e.g. for WebState). What is
    /// sent on it is not muted ([`Self::muted`]), so a device actor never
    /// sends through it.
    pub fn raw_sender(&self) -> &broadcast::Sender<FlighthookMessage> {
        &self.inner
    }

    /// Send a message, auto-stamping actor from the actor ID, and on a shot
    /// trigger the sender's provenance. Nothing is sent once the sender is
    /// muted.
    pub fn send(&self, mut msg: FlighthookMessage) {
        if self.muted.load(Ordering::Relaxed) {
            return;
        }
        msg.actor = self.actor_id.clone();
        if let (Some(provenance), FlighthookEvent::ShotTrigger { .. }) =
            (self.provenance, &msg.event)
        {
            msg.provenance = Some(provenance);
        }
        let _ = self.inner.send(msg);
    }

    /// Create a new receiver subscribed to this bus, sharing this sender's
    /// shutdown flag.
    pub fn subscribe(&self) -> BusReceiver {
        BusReceiver {
            inner: self.inner.subscribe(),
            shutdown: Arc::clone(&self.shutdown),
        }
    }
}

impl Clone for BusSender {
    fn clone(&self) -> Self {
        Self {
            actor_id: self.actor_id.clone(),
            inner: self.inner.clone(),
            shutdown: Arc::clone(&self.shutdown),
            muted: Arc::clone(&self.muted),
            provenance: self.provenance,
        }
    }
}

// ---------------------------------------------------------------------------
// BusReceiver
// ---------------------------------------------------------------------------

/// Receiver wrapper. Holds the broadcast Receiver and a shutdown flag.
pub struct BusReceiver {
    inner: broadcast::Receiver<FlighthookMessage>,
    shutdown: Arc<AtomicBool>,
}

impl BusReceiver {
    /// Check whether this actor's shutdown flag has been set.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    /// Non-blocking drain: returns the next message, `Ok(None)` if empty,
    /// or `Err(PollError::Shutdown)` if the bus is closed or shutdown flag set.
    pub fn poll(&mut self) -> Result<Option<FlighthookMessage>, PollError> {
        if self.is_shutdown() {
            return Err(PollError::Shutdown);
        }
        loop {
            match self.inner.try_recv() {
                Ok(msg) => return Ok(Some(msg)),
                Err(broadcast::error::TryRecvError::Empty) => return Ok(None),
                Err(broadcast::error::TryRecvError::Closed) => return Err(PollError::Shutdown),
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!("bus: lagged, dropped {n} events");
                    continue;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience: create BusReceiver from raw broadcast receiver
// ---------------------------------------------------------------------------

impl From<broadcast::Receiver<FlighthookMessage>> for BusReceiver {
    fn from(inner: broadcast::Receiver<FlighthookMessage>) -> Self {
        Self {
            inner,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }
}
