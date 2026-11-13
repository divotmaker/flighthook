//! The tournament round policy ([`SystemState::round`](super::SystemState)).
//!
//! An output that records a tournament round asks for one with
//! [`RoundPolicy::begin_round`] and holds the [`RoundGuard`] it gets for as
//! long as the round lasts. Dropping the guard ends the round, so a dropped
//! link, an error, a reconnect or a stopped actor all end it. The policy, the
//! same for every output, decides what a round means:
//!
//! - **Acceptance.** Every launch monitor that feeds the output must be
//!   [`Source::Measured`]; otherwise the [`Refusal`] names the first one
//!   that is not, and its class.
//! - **Which shots and launch monitors count.** [`RoundGuard::counts`]
//!   takes only a [`Source::Measured`] source, for the shots the output
//!   sends and for its readiness.
//! - **Virtual producers.** Auto-chip and the mock launch monitor send a
//!   shot's trigger through [`RoundPolicy::send_if_allowed`], which says no
//!   while any round is active. The check and the send are one step: a
//!   round cannot begin between them, so no virtual shot starts during a
//!   round. (Auto-chip also asks [`RoundPolicy::allows`] before its solve,
//!   to answer at once.)
//! - **Tournament mode.** [`RoundPolicy::set_manual`] switches a round on
//!   by hand, held by "Manual" (the `[simulation] tournament` setting). It
//!   feeds no output, so nothing is checked and it is never refused: it only
//!   turns the virtual producers off, and holds the shots of every remote
//!   launch monitor that declares itself virtual
//!   ([`RoundPolicy::admits_remote`]). A simulator's round on top of it is
//!   named by the simulator, and when that round ends the state falls back
//!   to the setting.
//! - **Simulator sources.** The Pureplay Accessory card is a virtual launch monitor
//!   that never joins a round and never blocks one: no simulator output
//!   takes its shots, so it feeds none. It is not switched off during a
//!   round either: a simulator's own shots are not flighthook's to switch
//!   off.
//!
//! The policy also holds each shot source's type ([`ShotSource`]), registered
//! when the actor starts, and puts a `round_state` event on the bus whenever
//! a round starts or ends, which the UI shows.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::broadcast;

use crate::actors::provenance::ShotSource;
use crate::bus::{BusMsg, LocalEvent};
use flighthook::{RoundState, Source};

/// The round policy. Clones share it.
#[derive(Debug, Clone, Default)]
pub struct RoundPolicy(Arc<Inner>);

#[derive(Debug, Default)]
struct Inner {
    policy: Mutex<Policy>,
    /// Where a change of the round is announced, once attached.
    bus: Mutex<Option<broadcast::Sender<BusMsg>>>,
}

#[derive(Debug, Default)]
struct Policy {
    /// Every shot source's type, by actor ID.
    sources: HashMap<String, ShotSource>,
    /// The actor ID each card registered its source under, by card key.
    cards: HashMap<String, String>,
    /// Each remote launch monitor's declared source (its `bus.actor`
    /// `device.source`; `None` when it declares none), by actor ID.
    remote: HashMap<String, Option<Source>>,
    /// The rounds simulators hold, by guard, with their holder's display
    /// name. The first one taken names the round.
    holders: BTreeMap<u64, String>,
    next_guard: u64,
    /// Tournament mode is switched on.
    manual: bool,
    /// The remote shots held by tournament mode, as (actor ID, shot id):
    /// their later messages are held too, until their `shot.finished`.
    held_remote: HashSet<(String, String)>,
    /// The remote launch monitors whose shots tournament mode held since
    /// it was last switched on (logged once each).
    holding_remote: HashSet<String>,
}

/// The holder named while only tournament mode holds the round.
pub const MANUAL_HOLDER: &str = "Manual";

impl Policy {
    fn active(&self) -> bool {
        self.manual || !self.holders.is_empty()
    }

    /// Whether a shot of class `kind` is held now: any but a measured one
    /// while a round is active.
    fn holds(&self, kind: Source) -> bool {
        kind != Source::Measured && self.active()
    }

    fn state(&self) -> RoundState {
        let simulator = self.holders.values().next().cloned();
        RoundState {
            active: self.active(),
            held: simulator.is_some(),
            manual: self.manual,
            holder: simulator.or_else(|| self.manual.then(|| MANUAL_HOLDER.to_owned())),
        }
    }
}

/// An output's request for a round.
#[derive(Debug, Clone, Copy)]
pub struct RoundRequest<'a> {
    /// The output's display name (its card name).
    pub holder: &'a str,
    /// The launch monitors whose shots reach the output, by actor ID, with
    /// their display names (blank when they have none).
    pub feeds: &'a [(&'a str, &'a str)],
}

/// Why a round was refused: the first launch monitor feeding the output
/// that is not measured, named, with its class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(String);

impl Refusal {
    /// The reason, e.g. "Mock is a virtual launch monitor".
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a source of class `provenance` (`None`: not a known shot source)
/// cannot count in a round; `None` when it can. `name` is its display name.
#[must_use]
pub fn not_measured_reason(provenance: Option<Source>, name: &str) -> Option<String> {
    match provenance {
        Some(Source::Measured) => None,
        Some(Source::Virtual) => Some(format!("{name} is a virtual launch monitor")),
        _ => Some(format!("{name} is not a launch monitor flighthook reads")),
    }
}

/// Why a remote launch monitor that declared `source` cannot count in a
/// round; `None` when it can. An undeclared source is not measured.
fn remote_reason(source: Option<Source>, name: &str) -> Option<String> {
    match source {
        Some(Source::Measured) => None,
        Some(Source::Virtual) => Some(format!("{name} is a virtual launch monitor")),
        _ => Some(format!("{name} does not say its shots are measured")),
    }
}

impl RoundPolicy {
    fn lock(&self) -> MutexGuard<'_, Policy> {
        self.0.policy.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Announce every change of the round on `bus` from now on.
    pub fn attach(&self, bus: broadcast::Sender<BusMsg>) {
        *self.0.bus.lock().unwrap_or_else(PoisonError::into_inner) = Some(bus);
    }

    /// Record that actor `actor` is a shot source of type `source`.
    pub fn register_source(&self, actor: &str, source: ShotSource) {
        self.lock().sources.insert(actor.to_owned(), source);
    }

    /// [`register_source`](Self::register_source) for card `card`, whose
    /// actor ID is `actor`: [`forget_card`](Self::forget_card) forgets it.
    pub fn register_card_source(&self, card: &str, actor: &str, source: ShotSource) {
        let mut policy = self.lock();
        // A card whose actor id changed (a restore) leaves its old id.
        if let Some(old) = policy.cards.insert(card.to_owned(), actor.to_owned())
            && old != actor
        {
            policy.sources.remove(&old);
        }
        policy.sources.insert(actor.to_owned(), source);
    }

    /// Record the remote launch monitor `actor`'s declared source, from its
    /// `bus.actor`.
    pub fn set_remote(&self, actor: &str, source: Option<Source>) {
        self.lock().remote.insert(actor.to_owned(), source);
    }

    /// Forget the remote launch monitor `actor` (it left the bus).
    pub fn forget_remote(&self, actor: &str) {
        let mut policy = self.lock();
        policy.remote.remove(actor);
        policy.held_remote.retain(|(from, _)| from != actor);
    }

    /// Forget actor `actor`: it stopped for good. Outputs stop counting it
    /// as a launch monitor that feeds them.
    #[cfg(test)]
    pub fn forget_source(&self, actor: &str) {
        self.lock().sources.remove(actor);
    }

    /// Forget the source card `card` registered, by its actor ID: the card
    /// was turned off or removed. Returns that actor ID.
    pub fn forget_card(&self, card: &str) -> Option<String> {
        let mut policy = self.lock();
        let actor = policy.cards.remove(card)?;
        policy.sources.remove(&actor);
        Some(actor)
    }

    /// The shot source type of actor `actor`, `None` when it is not one.
    #[must_use]
    pub fn source(&self, actor: &str) -> Option<ShotSource> {
        self.lock().sources.get(actor).copied()
    }

    /// Whether actor `actor` is a launch monitor: a shot source with a card,
    /// a status and readiness of its own.
    #[must_use]
    #[cfg(test)]
    pub fn is_launch_monitor(&self, actor: &str) -> bool {
        self.source(actor)
            .is_some_and(ShotSource::is_launch_monitor)
    }

    /// The round now: whether one is active and who holds it.
    #[must_use]
    pub fn state(&self) -> RoundState {
        self.lock().state()
    }

    /// Whether a round is active: a simulator holds one, or tournament
    /// mode is on.
    #[must_use]
    #[cfg(test)]
    pub fn active(&self) -> bool {
        self.lock().active()
    }

    /// Switch tournament mode on or off. Never refused: a launch monitor
    /// that is not measured just holds its shots while it is on. A
    /// simulator's round is untouched either way.
    pub fn set_manual(&self, on: bool) {
        let changed = {
            let mut policy = self.lock();
            let before = policy.state();
            policy.manual = on;
            if !policy.active() {
                policy.holding_remote.clear();
            }
            let after = policy.state();
            (before != after).then_some(after)
        };
        self.announce(changed);
    }

    /// Whether the message `body` from the remote actor `actor` may go on
    /// the bus. An active round (tournament mode or a simulator's) holds a
    /// shot from a remote launch monitor that declared itself
    /// [`Source::Virtual`], as it holds the local virtual cards' shots: a
    /// `shot.trigger` during the round is held, and so is every later
    /// message of that shot (through its `shot.finished`), so the shot
    /// never exists here. A shot that began before the round goes through.
    /// Logs once per monitor each time a round begins.
    #[must_use]
    pub fn admits_remote(&self, actor: &str, body: &flightrelay::FrpBody) -> bool {
        let Some(key) = body.shot_key() else {
            return true;
        };
        let mut policy = self.lock();
        let shot = (actor.to_owned(), key.shot_id.clone());
        if matches!(body, flightrelay::FrpBody::ShotTrigger(_)) {
            let virtual_monitor = policy.remote.get(actor) == Some(&Some(Source::Virtual));
            if !(virtual_monitor && policy.holds(Source::Virtual)) {
                return true;
            }
            if policy.holding_remote.insert(actor.to_owned()) {
                tracing::info!("{actor}: no shots during the tournament round");
            }
            policy.held_remote.insert(shot);
            return false;
        }
        if !policy.held_remote.contains(&shot) {
            return true;
        }
        if matches!(body, flightrelay::FrpBody::ShotFinished(_)) {
            policy.held_remote.remove(&shot);
        }
        false
    }

    /// Whether a producer of shots of class `kind` may make one now: a
    /// measured one always, any other only while no round is active.
    #[must_use]
    pub fn allows(&self, kind: Source) -> bool {
        !self.lock().holds(kind)
    }

    /// Run `send`, which puts a shot's trigger on the bus, when a producer
    /// of class `kind` may make a shot now ([`Self::allows`]); returns what
    /// it returns, `None` when it was not run. No round can begin while
    /// `send` runs, so a shot it triggers starts before any round does.
    /// `send` must not call back into the policy.
    pub fn send_if_allowed<T>(&self, kind: Source, send: impl FnOnce() -> T) -> Option<T> {
        let policy = self.lock();
        if policy.holds(kind) {
            return None;
        }
        let sent = send();
        drop(policy);
        Some(sent)
    }

    /// Start a round for `request`: accepted when every launch monitor that
    /// feeds it is measured (with none at all, nothing unmeasured can reach
    /// it). Otherwise refused, naming the first that is not, in actor ID
    /// order; a monitor with a blank name is named by its actor ID.
    pub fn begin_round(&self, request: &RoundRequest<'_>) -> Result<RoundGuard, Refusal> {
        let (guard, changed) = {
            let mut policy = self.lock();
            let mut feeds = request.feeds.to_vec();
            feeds.sort_unstable();
            feeds.dedup_by_key(|(id, _)| *id);
            let refusal = feeds.iter().find_map(|(id, name)| {
                let name = Some(name.trim()).filter(|n| !n.is_empty()).unwrap_or(id);
                match (policy.sources.get(*id), policy.remote.get(*id)) {
                    (None, Some(source)) => remote_reason(*source, name),
                    (local, _) => not_measured_reason(local.map(|s| s.provenance()), name),
                }
            });
            if let Some(reason) = refusal {
                return Err(Refusal(reason));
            }
            let before = policy.state();
            let token = policy.next_guard;
            policy.next_guard += 1;
            policy.holders.insert(token, request.holder.to_owned());
            let after = policy.state();
            (
                RoundGuard {
                    policy: self.clone(),
                    token,
                },
                (before != after).then_some(after),
            )
        };
        self.announce(changed);
        Ok(guard)
    }

    /// End the round `token` holds.
    fn end(&self, token: u64) {
        let changed = {
            let mut policy = self.lock();
            let before = policy.state();
            policy.holders.remove(&token);
            if !policy.active() {
                policy.holding_remote.clear();
            }
            let after = policy.state();
            (before != after).then_some(after)
        };
        self.announce(changed);
    }

    /// Put a changed round on the bus.
    fn announce(&self, changed: Option<RoundState>) {
        let Some(round) = changed else {
            return;
        };
        tracing::info!(
            "round: {}",
            match (&round.holder, round.held, round.manual) {
                (Some(holder), true, true) => {
                    format!("{holder} holds a tournament round (tournament mode on)")
                }
                (Some(holder), true, false) => format!("{holder} holds a tournament round"),
                (_, false, true) => "tournament mode is on".to_owned(),
                _ => "no tournament round".to_owned(),
            }
        );
        if let Some(bus) = &*self.0.bus.lock().unwrap_or_else(PoisonError::into_inner) {
            let _ = bus.send(BusMsg::local("system", LocalEvent::RoundState { round }));
        }
    }
}

/// A round held by one output. Dropping it ends the round.
#[derive(Debug)]
pub struct RoundGuard {
    policy: RoundPolicy,
    token: u64,
}

impl RoundGuard {
    /// Whether a shot or launch monitor of class `provenance` counts during
    /// the round: only a measured one.
    #[must_use]
    pub fn counts(&self, provenance: Option<Source>) -> bool {
        provenance == Some(Source::Measured)
    }
}

impl Drop for RoundGuard {
    fn drop(&mut self) {
        self.policy.end(self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A policy that knows the usual sources.
    fn policy() -> RoundPolicy {
        let policy = RoundPolicy::default();
        for (id, source) in [
            ("mevo.0", ShotSource::Mevo),
            ("r10.0", ShotSource::R10),
            ("square.0", ShotSource::Square),
            ("exputt.0", ShotSource::ExPutt),
            ("trackball.0", ShotSource::Trackball),
            ("mock_monitor.0", ShotSource::MockMonitor),
            ("openconnect_server.0", ShotSource::OpenConnectServer),
            (flighthook::AUTO_CHIP_ACTOR, ShotSource::AutoChip),
        ] {
            policy.register_source(id, source);
        }
        policy
    }

    fn ask<'a>(
        policy: &RoundPolicy,
        feeds: &'a [(&'a str, &'a str)],
    ) -> Result<RoundGuard, Refusal> {
        policy.begin_round(&RoundRequest {
            holder: "Pureplay",
            feeds,
        })
    }

    fn reason(result: Result<RoundGuard, Refusal>) -> String {
        result.expect_err("refused").reason().to_owned()
    }

    /// Tournament mode holds a virtual remote monitor's shot from trigger
    /// to finish; an undeclared or measured one, and every shot while it is
    /// off, goes through; forgetting the monitor drops what it held.
    #[test]
    fn tournament_mode_holds_a_virtual_remote_monitors_shots() {
        use flightrelay::{FrpBody, ShotKey, ShotTrigger};
        let policy = policy();
        policy.set_remote("remote-v", Some(Source::Virtual));
        policy.set_remote("remote-u", None);
        let trigger = |id: &str| FrpBody::from(ShotTrigger::new(ShotKey::new(id, 1)));
        let finished = |id: &str| {
            FrpBody::from(flightrelay::ShotFinished {
                key: ShotKey::new(id, 1),
            })
        };
        assert!(policy.admits_remote("remote-v", &trigger("a")));
        policy.set_manual(true);
        assert!(!policy.admits_remote("remote-v", &trigger("b")));
        assert!(!policy.admits_remote("remote-v", &finished("b")));
        assert!(
            policy.admits_remote("remote-v", &finished("a")),
            "began before"
        );
        assert!(policy.admits_remote("remote-u", &trigger("c")));
        assert!(!policy.admits_remote("remote-v", &trigger("d")));
        policy.forget_remote("remote-v");
        assert!(policy.lock().held_remote.is_empty());
        policy.set_manual(false);
        policy.set_remote("remote-v", Some(Source::Virtual));
        assert!(policy.admits_remote("remote-v", &trigger("e")));
    }

    /// A remote launch monitor counts by the source its `bus.actor`
    /// declares; one that declares none is not measured.
    #[test]
    fn a_remote_monitor_counts_by_its_declared_source() {
        let policy = policy();
        policy.set_remote("remote-m", Some(Source::Measured));
        policy.set_remote("remote-v", Some(Source::Virtual));
        policy.set_remote("remote-u", None);
        assert!(ask(&policy, &[("remote-m", "Far radar")]).is_ok());
        assert_eq!(
            reason(ask(&policy, &[("remote-v", "Trackball")])),
            "Trackball is a virtual launch monitor"
        );
        assert_eq!(
            reason(ask(&policy, &[("remote-u", "Tool")])),
            "Tool does not say its shots are measured"
        );
        policy.forget_remote("remote-u");
        assert_eq!(
            reason(ask(&policy, &[("remote-u", "Tool")])),
            "Tool is not a launch monitor flighthook reads"
        );
    }

    #[test]
    fn a_round_needs_every_feeding_monitor_measured() {
        let policy = policy();
        let all = [
            ("mevo.0", "Mevo+"),
            ("r10.0", "R10"),
            ("square.0", "Omni"),
            ("exputt.0", "ExPutt"),
        ];
        assert!(ask(&policy, &all).is_ok());
        assert!(ask(&policy, &[]).is_ok(), "nothing feeds it");
        assert_eq!(
            reason(ask(
                &policy,
                &[("square.0", "Omni"), ("mock_monitor.0", "Mock")]
            )),
            "Mock is a virtual launch monitor"
        );
        assert_eq!(
            reason(ask(
                &policy,
                &[("thing.0", "Thing"), ("mock_monitor.0", "Mock")]
            )),
            "Mock is a virtual launch monitor",
            "the first in actor ID order"
        );
        assert!(
            ask(
                &policy,
                &[
                    ("openconnect_server.0", "Open Connect input"),
                    ("square.0", "Omni")
                ]
            )
            .is_ok(),
            "the Open Connect input is measured"
        );
        assert_eq!(
            reason(ask(&policy, &[("thing.0", "Thing")])),
            "Thing is not a launch monitor flighthook reads"
        );
        assert_eq!(
            reason(ask(&policy, &[("mock_monitor.0", " ")])),
            "mock_monitor.0 is a virtual launch monitor",
            "a blank name gives the actor ID"
        );
        assert_eq!(
            reason(ask(&policy, &[(flighthook::AUTO_CHIP_ACTOR, "Auto-chip")])),
            "Auto-chip is a virtual launch monitor"
        );
        assert_eq!(
            reason(ask(
                &policy,
                &[("square.0", "Omni"), ("trackball.0", "Trackball")]
            )),
            "Trackball is a virtual launch monitor"
        );
        assert!(!policy.active(), "no refusal holds a round");
    }

    #[test]
    fn a_round_lasts_until_its_guard_drops() {
        let policy = policy();
        assert_eq!(policy.state(), RoundState::default());
        let guard = ask(&policy, &[("square.0", "Omni")]).expect("accepted");
        assert_eq!(
            policy.state(),
            RoundState {
                active: true,
                held: true,
                manual: false,
                holder: Some("Pureplay".into())
            }
        );
        drop(guard);
        assert_eq!(policy.state(), RoundState::default());
    }

    #[test]
    fn two_holders_keep_the_round_until_both_end() {
        let policy = policy();
        let first = ask(&policy, &[]).expect("accepted");
        let second = policy
            .begin_round(&RoundRequest {
                holder: "Pureplay 2",
                feeds: &[("r10.0", "R10")],
            })
            .expect("accepted");
        assert_eq!(policy.state().holder.as_deref(), Some("Pureplay"));
        drop(first);
        assert_eq!(
            policy.state(),
            RoundState {
                active: true,
                held: true,
                manual: false,
                holder: Some("Pureplay 2".into())
            },
            "the other still holds it"
        );
        drop(second);
        assert!(!policy.active());
    }

    #[test]
    fn virtual_producers_are_off_during_a_round_and_back_after() {
        let policy = policy();
        assert!(policy.allows(Source::Virtual));
        let guard = ask(&policy, &[]).expect("accepted");
        assert!(!policy.allows(Source::Virtual));
        assert!(policy.allows(Source::Measured));
        let shared = policy.clone();
        drop(guard);
        assert!(shared.allows(Source::Virtual));
    }

    /// A virtual producer's send runs only while no round is active, and a
    /// round that is asked for while it runs begins only after it.
    #[test]
    fn a_round_cannot_begin_while_a_virtual_shot_is_sent() {
        let policy = policy();
        assert_eq!(policy.send_if_allowed(Source::Virtual, || 7), Some(7));
        let begun = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let asker = policy.send_if_allowed(Source::Virtual, || {
            let (shared, flag) = (policy.clone(), Arc::clone(&begun));
            let asker = std::thread::spawn(move || {
                let guard = ask(&shared, &[]).expect("accepted");
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                guard
            });
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert!(
                !begun.load(std::sync::atomic::Ordering::SeqCst),
                "the round waits for the send"
            );
            asker
        });
        let guard = asker.expect("sent").join().expect("asker");
        assert!(policy.active());
        let mut sent = false;
        assert_eq!(
            policy.send_if_allowed(Source::Virtual, || sent = true),
            None
        );
        assert!(!sent, "nothing is sent during the round");
        assert_eq!(policy.send_if_allowed(Source::Measured, || 1), Some(1));
        drop(guard);
        assert_eq!(policy.send_if_allowed(Source::Virtual, || 2), Some(2));
    }

    /// A removed actor is no longer a known source, so it no longer counts
    /// as a launch monitor.
    #[test]
    fn a_forgotten_source_is_unknown() {
        let policy = policy();
        assert!(policy.is_launch_monitor("mock_monitor.0"));
        policy.forget_source("mock_monitor.0");
        assert_eq!(policy.source("mock_monitor.0"), None);
        assert!(!policy.is_launch_monitor("mock_monitor.0"));
        assert_eq!(policy.source("square.0"), Some(ShotSource::Square));
    }

    #[test]
    fn a_change_of_the_round_is_announced_once() {
        let policy = policy();
        let (tx, mut rx) = broadcast::channel(16);
        policy.attach(tx);
        let rounds = |rx: &mut broadcast::Receiver<BusMsg>| -> Vec<RoundState> {
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|m| match m {
                    BusMsg::Local {
                        event: crate::bus::LocalEvent::RoundState { round },
                        ..
                    } => Some(round),
                    _ => None,
                })
                .collect()
        };
        let first = ask(&policy, &[]).expect("accepted");
        let second = ask(&policy, &[]).expect("accepted");
        let _ = ask(&policy, &[("mock_monitor.0", "Mock")]);
        let active = RoundState {
            active: true,
            held: true,
            manual: false,
            holder: Some("Pureplay".into()),
        };
        assert_eq!(rounds(&mut rx), [active], "a second holder changes nothing");
        drop(first);
        assert!(rounds(&mut rx).is_empty());
        drop(second);
        assert_eq!(rounds(&mut rx), [RoundState::default()]);
    }

    #[test]
    fn only_a_measured_source_counts() {
        let guard = ask(&policy(), &[]).expect("accepted");
        assert!(guard.counts(Some(Source::Measured)));
        assert!(!guard.counts(Some(Source::Virtual)));
        assert!(!guard.counts(None));
    }

    /// Tournament mode turns Auto-chip, the mock and the trackball off
    /// (`allows` and `send_if_allowed` say no), names the round "Manual",
    /// and is never refused, whatever feeds an output.
    #[test]
    fn tournament_mode_turns_virtual_producers_off_and_is_never_refused() {
        let policy = policy();
        let mut sent = false;
        policy.set_manual(true);
        assert_eq!(
            policy.state(),
            RoundState {
                active: true,
                holder: Some(MANUAL_HOLDER.into()),
                manual: true,
                held: false,
            }
        );
        for id in [flighthook::AUTO_CHIP_ACTOR, "mock_monitor.0", "trackball.0"] {
            let kind = policy.source(id).expect("registered").provenance();
            assert!(!policy.allows(kind), "{id}");
            assert_eq!(policy.send_if_allowed(kind, || sent = true), None, "{id}");
        }
        assert!(!sent, "nothing virtual is sent in tournament mode");
        assert!(policy.allows(Source::Measured));
        assert_eq!(policy.send_if_allowed(Source::Measured, || 1), Some(1));
        policy.set_manual(false);
        assert_eq!(policy.state(), RoundState::default());
        assert!(policy.allows(Source::Virtual));
    }

    /// Switching tournament mode on while a mock and a trackball are
    /// registered (and feeding outputs) refuses nothing; the measured
    /// check stays a simulator's alone.
    #[test]
    fn tournament_mode_with_virtual_monitors_feeding_an_output_is_not_refused() {
        let policy = policy();
        policy.set_manual(true);
        assert!(policy.active(), "on, with the mock and trackball known");
        assert_eq!(
            reason(ask(&policy, &[("mock_monitor.0", "Mock")])),
            "Mock is a virtual launch monitor",
            "a simulator's own acceptance is unchanged"
        );
        assert!(
            policy.state().manual,
            "the refusal leaves tournament mode on"
        );
        assert!(!policy.state().held);
    }

    /// A simulator's round on top of tournament mode is held and named by
    /// the simulator; when it ends, the state falls back to the setting.
    #[test]
    fn a_simulators_round_names_itself_over_manual_and_falls_back() {
        let policy = policy();
        // Manual first, then the simulator: the simulator names it.
        policy.set_manual(true);
        let guard = ask(&policy, &[("square.0", "Omni")]).expect("accepted");
        assert_eq!(
            policy.state(),
            RoundState {
                active: true,
                holder: Some("Pureplay".into()),
                manual: true,
                held: true,
            }
        );
        drop(guard);
        assert_eq!(
            policy.state(),
            RoundState {
                active: true,
                holder: Some(MANUAL_HOLDER.into()),
                manual: true,
                held: false,
            },
            "on stays on"
        );
        // Off underneath: the end of the round is the end of tournament mode.
        policy.set_manual(false);
        let guard = ask(&policy, &[]).expect("accepted");
        assert!(policy.state().held && !policy.state().manual);
        drop(guard);
        assert_eq!(policy.state(), RoundState::default(), "off stays off");
    }

    /// Switching tournament mode off while a simulator holds the round
    /// leaves the round active and the producers off.
    #[test]
    fn manual_off_while_held_leaves_the_round_active() {
        let policy = policy();
        policy.set_manual(true);
        let guard = ask(&policy, &[]).expect("accepted");
        policy.set_manual(false);
        assert_eq!(
            policy.state(),
            RoundState {
                active: true,
                holder: Some("Pureplay".into()),
                manual: false,
                held: true,
            }
        );
        assert!(!policy.allows(Source::Virtual));
        drop(guard);
        assert!(!policy.active());
    }

    /// Each change of tournament mode is announced once; switching it while
    /// a simulator holds the round announces the flag.
    #[test]
    fn a_change_of_tournament_mode_is_announced() {
        let policy = policy();
        let (tx, mut rx) = broadcast::channel(16);
        policy.attach(tx);
        let count = |rx: &mut broadcast::Receiver<BusMsg>| {
            std::iter::from_fn(|| rx.try_recv().ok()).count()
        };
        policy.set_manual(true);
        policy.set_manual(true);
        assert_eq!(count(&mut rx), 1, "a repeat changes nothing");
        policy.set_manual(false);
        assert_eq!(count(&mut rx), 1);
    }
}
