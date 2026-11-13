//! The internal bus: FRP messages beside flighthook's own local events, on
//! one `tokio::sync::broadcast` channel. [`BusSender`] and [`BusReceiver`]
//! wrap the channel so callers never touch the broadcast types directly.
//!
//! - [`BusMsg::Frp`] carries an FRP message: from one of flighthook's own
//!   actors (`peer: None`), whose `from` the sender stamps with the card's
//!   actor id. `cause` is set only on the session's own `golfer.turn`: the
//!   actor whose `golfer.select` produced it. It is internal and never goes
//!   on the wire.
//! - [`BusMsg::Local`] carries flighthook's own events ([`LocalEvent`]):
//!   configuration, card status, cameras, the tournament round, a shot's raw
//!   frames and the flight model's path. They never leave flighthook.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use flightrelay::{FrpBody, FrpMessage, GolferTurn, Severity, StateKind};
use tokio::sync::broadcast;

use flighthook::{
    ActorStatus, CameraPosition, ConfigAction, FlightPath, RoundState, ShotKey, ShotRaw,
};

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// A connection to another FRP program on the hub (`/frp` or a Flight
/// Relay card): a message from it is never sent back to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerId(pub u64);

impl PeerId {
    /// A new id, never given out before in this process.
    #[must_use]
    pub fn next() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// One message on the internal bus.
// Most messages are `Frp`: boxing it would add an allocation to nearly
// every message for the few small `Local` ones.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum BusMsg {
    /// An FRP message: from a local actor (`peer: None`) or from a peer.
    /// `cause` is set only on the session's own `golfer.turn`: the actor id
    /// whose `golfer.select` produced it.
    Frp {
        msg: FrpMessage,
        /// The hub link the message came in on; `None` for a local actor.
        peer: Option<PeerId>,
        cause: Option<String>,
    },
    /// flighthook's own event. `card` is the card key (`mevo.0`), or
    /// `system` / `web`.
    Local { card: String, event: LocalEvent },
}

impl BusMsg {
    /// An FRP message from the hub link `peer`.
    #[must_use]
    pub fn from_peer(msg: FrpMessage, peer: PeerId) -> Self {
        Self::Frp {
            msg,
            peer: Some(peer),
            cause: None,
        }
    }

    /// An FRP message from a local actor.
    #[must_use]
    pub fn frp(msg: FrpMessage) -> Self {
        Self::Frp {
            msg,
            peer: None,
            cause: None,
        }
    }

    /// A local event of `card`.
    #[must_use]
    pub fn local(card: impl Into<String>, event: LocalEvent) -> Self {
        Self::Local {
            card: card.into(),
            event,
        }
    }

    /// The FRP message, if this is one.
    #[must_use]
    pub fn as_frp(&self) -> Option<&FrpMessage> {
        match self {
            Self::Frp { msg, .. } => Some(msg),
            Self::Local { .. } => None,
        }
    }

    /// The local event and its card, if this is one.
    #[must_use]
    pub fn as_local(&self) -> Option<(&str, &LocalEvent)> {
        match self {
            Self::Local { card, event } => Some((card, event)),
            Self::Frp { .. } => None,
        }
    }

    /// The session's `golfer.turn` and its cause, when this message is the
    /// turn of the session `session` (flighthook's own actor id).
    #[must_use]
    pub fn turn_of(&self, session: &str) -> Option<(&GolferTurn, Option<&str>)> {
        match self {
            Self::Frp {
                msg:
                    FrpMessage {
                        from,
                        body: FrpBody::GolferTurn(turn),
                    },
                cause,
                ..
            } if from == session => Some((turn, cause.as_deref())),
            _ => None,
        }
    }
}

/// flighthook's own events: never on the wire.
#[derive(Debug, Clone)]
pub enum LocalEvent {
    /// Config mutation request (emitted by the REST handlers, processed by
    /// the system actor).
    ConfigCommand {
        /// Opaque correlation ID for request-reply.
        request_id: Option<String>,
        action: Box<ConfigAction>,
    },
    /// Config mutation outcome (emitted by the system actor).
    ConfigOutcome {
        request_id: Option<String>,
        restarted: Vec<String>,
        stopped: Vec<String>,
        started: Vec<String>,
    },
    /// The card's lifecycle and display rows: its connection status and
    /// card telemetry (error, address, device label, ...).
    CardStatus {
        status: ActorStatus,
        telemetry: HashMap<String, String>,
    },
    /// An alert of a card that is no FRP actor (a camera, the web server,
    /// the system).
    Alert { severity: Severity, message: String },
    /// The tournament round started or ended.
    RoundState { round: RoundState },
    /// Delete the shot history (a store action); every set and the shot
    /// list are cleared.
    ClearShotHistory,
    /// The ExPutt card `target` locates its mat again.
    FindMat { target: String },
    /// A camera finished a clip of these shots (beta).
    ShotVideo {
        keys: Vec<ShotKey>,
        position: CameraPosition,
        duration_ms: u64,
        fps: f64,
        trigger_offset_ms: i64,
        impact_offset_ms: i64,
        complete: bool,
        dropped: u32,
    },
    /// The clips of these shots were deleted (beta).
    ShotVideoRemoved { keys: Vec<ShotKey> },
    /// Body tracking of a camera's clip is done (alpha).
    ShotPose { keys: Vec<ShotKey>, rotation: u16 },
    /// The swing analysis of a camera's clip is done (alpha).
    ShotSwing { keys: Vec<ShotKey> },
    /// The device frames a launch monitor card captured for its shot, for
    /// the export, with the strings the export scrubs from them (the
    /// device's own id: SSID, BLE name or address). Sent just before the
    /// shot's `shot.finished`. Local only: the device id never goes on FRP.
    ShotCapture {
        key: ShotKey,
        raw: Box<ShotRaw>,
        scrub: Vec<String>,
    },
    /// The flight model's path of a shot: FRP has no path field.
    ShotPath { key: ShotKey, path: FlightPath },
    /// The remote actor `actor` went silent past its heartbeat and is gone
    /// (sent by the system actor, [`crate::state::remotes`]). Every reader
    /// that keeps state of other actors forgets it, as on its `bus.actor`
    /// removal. Never sent for an actor this process hosts.
    ActorGone { actor: String },
}

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
// Beats
// ---------------------------------------------------------------------------

/// The newest `bus.actor` of every local actor, which the heartbeat timer
/// sends again each heartbeat. The senders keep it themselves and send
/// under its lock, so a heartbeat is never older than the actor's newest
/// `bus.actor` on the bus. It also remembers each card's actor id, so the
/// removal of a card that is gone from the config can still be sent.
#[derive(Clone, Default)]
pub struct Beats(Arc<Mutex<Held>>);

#[derive(Default)]
struct Held {
    /// Actor id to its newest `bus.actor`.
    actors: HashMap<String, FrpMessage>,
    /// Card key to the actor id its `bus.actor` went out under.
    cards: HashMap<String, String>,
}

impl Beats {
    fn lock(&self) -> std::sync::MutexGuard<'_, Held> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Send `msg` of card `card` on `tx`, keeping it when it is a local
    /// `bus.actor` and forgetting the actor when it is that `bus.actor`'s
    /// removal.
    fn send(&self, tx: &broadcast::Sender<BusMsg>, card: &str, msg: BusMsg) {
        let mut held = self.lock();
        if let BusMsg::Frp {
            msg: frp,
            peer: None,
            ..
        } = &msg
        {
            match &frp.body {
                FrpBody::BusActor(_) => {
                    held.actors.insert(frp.from.clone(), frp.clone());
                    held.cards.insert(card.to_owned(), frp.from.clone());
                }
                FrpBody::Removed(r) if r.kind == StateKind::BusActor => {
                    held.actors.remove(&frp.from);
                    held.cards.remove(card);
                }
                _ => {}
            }
        }
        let _ = tx.send(msg);
    }

    /// The actor id card `card`'s last `bus.actor` went out under, unless
    /// its removal went out since.
    pub fn actor_of_card(&self, card: &str) -> Option<String> {
        self.lock().cards.get(card).cloned()
    }

    /// Send the kept `bus.actor` of every actor `which` accepts again on
    /// `tx` (for a `bus.sync`).
    pub fn send_again(&self, tx: &broadcast::Sender<BusMsg>, which: impl Fn(&str) -> bool) {
        let held = self.lock();
        for (from, msg) in &held.actors {
            if which(from) {
                let _ = tx.send(BusMsg::frp(msg.clone()));
            }
        }
    }

    /// Forget card `card`'s kept `bus.actor`. Returns the actor id it went
    /// out under.
    pub fn forget_card(&self, card: &str) -> Option<String> {
        let mut held = self.lock();
        let actor = held.cards.remove(card)?;
        held.actors.remove(&actor);
        Some(actor)
    }

    /// Send every kept `bus.actor` again on `tx`, for the actors `live`
    /// accepts; the others are forgotten. Returns how many went out.
    pub fn beat(&self, tx: &broadcast::Sender<BusMsg>, live: impl Fn(&str) -> bool) -> usize {
        let mut held = self.lock();
        held.actors.retain(|from, _| live(from));
        for msg in held.actors.values() {
            let _ = tx.send(BusMsg::frp(msg.clone()));
        }
        held.actors.len()
    }
}

// ---------------------------------------------------------------------------
// BusSender
// ---------------------------------------------------------------------------

/// Cloneable sender of one card: it stamps the card key on its local events
/// and the card's FRP actor id as the `from` of its FRP messages.
pub struct BusSender {
    card: String,
    /// The card's FRP actor id; `None` for a card that is no actor (a
    /// camera, the web server).
    frp_id: Option<String>,
    inner: broadcast::Sender<BusMsg>,
    shutdown: Arc<AtomicBool>,
    /// Set once a new instance of the actor has replaced this one: what this
    /// one still sends is dropped, so it never speaks for its successor.
    muted: Arc<AtomicBool>,
    /// Where the card's `bus.actor` is kept for the heartbeat timer.
    beats: Option<Beats>,
}

impl BusSender {
    pub fn new(card: String, inner: broadcast::Sender<BusMsg>, shutdown: Arc<AtomicBool>) -> Self {
        Self {
            card,
            frp_id: None,
            inner,
            shutdown,
            muted: Arc::default(),
            beats: None,
        }
    }

    /// This sender keeps the card's `bus.actor` in `beats`, which the
    /// heartbeat timer sends again every heartbeat.
    #[must_use]
    pub fn with_beats(mut self, beats: Beats) -> Self {
        self.beats = Some(beats);
        self
    }

    /// This sender stamps `frp_id` as the `from` of the card's FRP messages.
    #[must_use]
    pub fn with_frp_id(mut self, frp_id: Option<String>) -> Self {
        self.frp_id = frp_id;
        self
    }

    /// A sender for another card run by the same actor thread(s): it shares
    /// this sender's shutdown flag and mute.
    #[must_use]
    pub fn for_card(&self, card: impl Into<String>, frp_id: Option<String>) -> Self {
        Self {
            card: card.into(),
            frp_id,
            inner: self.inner.clone(),
            shutdown: Arc::clone(&self.shutdown),
            muted: Arc::clone(&self.muted),
            beats: self.beats.clone(),
        }
    }

    /// The flag that silences this sender and its clones.
    pub(crate) fn muted(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.muted)
    }

    /// The card key, e.g. `mevo.0`.
    pub fn actor_id(&self) -> &str {
        &self.card
    }

    /// The card's FRP actor id, if it is an actor.
    pub fn frp_id(&self) -> Option<&str> {
        self.frp_id.as_deref()
    }

    /// Access the underlying broadcast sender (e.g. for WebState). What is
    /// sent on it is not muted ([`Self::muted`]), so a device actor never
    /// sends through it.
    pub fn raw_sender(&self) -> &broadcast::Sender<BusMsg> {
        &self.inner
    }

    fn live(&self) -> bool {
        !self.muted.load(Ordering::Relaxed)
    }

    /// Send a prepared message as it is. Nothing is sent once the sender is
    /// muted.
    pub fn send(&self, msg: BusMsg) {
        if !self.live() {
            return;
        }
        match &self.beats {
            Some(beats) => beats.send(&self.inner, &self.card, msg),
            None => {
                let _ = self.inner.send(msg);
            }
        }
    }

    /// Send a local event of this card.
    pub fn local(&self, event: LocalEvent) {
        self.send(BusMsg::local(self.card.clone(), event));
    }

    /// Send an FRP message body as this card's actor. A card that is no
    /// actor sends nothing.
    pub fn frp(&self, body: impl Into<FrpBody>) {
        match &self.frp_id {
            Some(id) => self.send(BusMsg::frp(FrpMessage::new(id.clone(), body))),
            None => tracing::debug!("{}: no actor id, FRP message dropped", self.card),
        }
    }

    /// Raise an alert: a `bus.alert` from a card that is an FRP actor, else
    /// a local alert of the card.
    pub fn alert(&self, severity: Severity, message: impl Into<String>) {
        let message = message.into();
        if self.frp_id.is_some() {
            self.frp(flightrelay::BusAlert::new(severity, message));
        } else {
            self.local(LocalEvent::Alert { severity, message });
        }
    }

    /// Send an FRP message body as this card's actor with an internal
    /// `cause` (the session's turn).
    pub fn frp_caused(&self, body: impl Into<FrpBody>, cause: Option<String>) {
        if let Some(id) = &self.frp_id {
            self.send(BusMsg::Frp {
                msg: FrpMessage::new(id.clone(), body),
                peer: None,
                cause,
            });
        }
    }

    /// Create a new receiver subscribed to this bus, sharing this sender's
    /// shutdown flag.
    pub fn subscribe(&self) -> BusReceiver {
        BusReceiver {
            inner: self.inner.subscribe(),
            shutdown: Arc::clone(&self.shutdown),
            lagged: false,
        }
    }
}

impl Clone for BusSender {
    fn clone(&self) -> Self {
        Self {
            card: self.card.clone(),
            frp_id: self.frp_id.clone(),
            inner: self.inner.clone(),
            shutdown: Arc::clone(&self.shutdown),
            muted: Arc::clone(&self.muted),
            beats: self.beats.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// BusReceiver
// ---------------------------------------------------------------------------

/// Receiver wrapper. Holds the broadcast Receiver and a shutdown flag.
pub struct BusReceiver {
    inner: broadcast::Receiver<BusMsg>,
    shutdown: Arc<AtomicBool>,
    /// Messages were dropped because this receiver fell behind, since the
    /// last [`take_lagged`](Self::take_lagged).
    lagged: bool,
}

impl BusReceiver {
    /// Check whether this actor's shutdown flag has been set.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    /// Whether messages were dropped because this receiver fell behind,
    /// since the last call.
    pub fn take_lagged(&mut self) -> bool {
        std::mem::take(&mut self.lagged)
    }

    /// Non-blocking drain: returns the next message, `Ok(None)` if empty,
    /// or `Err(PollError::Shutdown)` if the bus is closed or shutdown flag set.
    pub fn poll(&mut self) -> Result<Option<BusMsg>, PollError> {
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
                    self.lagged = true;
                }
            }
        }
    }
}

impl From<broadcast::Receiver<BusMsg>> for BusReceiver {
    fn from(inner: broadcast::Receiver<BusMsg>) -> Self {
        Self {
            inner,
            shutdown: Arc::new(AtomicBool::new(false)),
            lagged: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A receiver that fell behind says so once.
    #[test]
    fn a_lagging_receiver_reports_it_once() {
        let (tx, _) = broadcast::channel(2);
        let sender = BusSender::new("web".into(), tx, Arc::default());
        let mut rx = sender.subscribe();
        for _ in 0..4 {
            sender.local(LocalEvent::ClearShotHistory);
        }
        assert!(!rx.take_lagged(), "nothing read yet");
        assert!(matches!(rx.poll(), Ok(Some(_))));
        assert!(rx.take_lagged());
        assert!(!rx.take_lagged());
    }

    /// The heartbeat sends each live actor's newest `bus.actor` again, and
    /// forgets a removed or no longer live one.
    #[test]
    fn the_heartbeat_sends_each_actors_newest_bus_actor() {
        use flightrelay::{BusActor, Removal};
        let (tx, mut rx) = broadcast::channel(16);
        let beats = Beats::default();
        let lm = BusSender::new("mevo.0".into(), tx.clone(), Arc::default())
            .with_frp_id(Some("lm".into()))
            .with_beats(beats.clone());
        let other = lm.for_card("mock_monitor.0", Some("other".into()));
        lm.frp(BusActor::new("Old", 2.0));
        lm.frp(BusActor::new("New", 2.0));
        other.frp(BusActor::new("Other", 2.0));
        while rx.try_recv().is_ok() {}
        assert_eq!(beats.beat(&tx, |_| true), 2);
        let mut names = Vec::new();
        while let Ok(BusMsg::Frp { msg, .. }) = rx.try_recv() {
            if let FrpBody::BusActor(a) = msg.body {
                names.push(a.name);
            }
        }
        names.sort();
        assert_eq!(names, ["New", "Other"]);
        other.frp(Removal::of(StateKind::BusActor));
        assert_eq!(beats.beat(&tx, |from| from != "lm"), 0);
    }
}
