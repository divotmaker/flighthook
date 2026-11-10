//! The tournament round policy ([`SystemState::round`](super::SystemState)).
//!
//! An output that records a tournament round asks for one with
//! [`RoundPolicy::begin_round`] and holds the [`RoundGuard`] it gets for as
//! long as the round lasts. Dropping the guard ends the round, so a dropped
//! link, an error, a reconnect or a stopped actor all end it. The policy, the
//! same for every output, decides what a round means:
//!
//! - **Acceptance.** Every launch monitor that feeds the output must be
//!   [`Provenance::Measured`]; otherwise the [`Refusal`] names the first one
//!   that is not, and its class.
//! - **Which shots count, and which values.** [`RoundGuard::certify`] takes
//!   only measured shots, with every value that is not the device's reading
//!   removed ([`CertifiedShot`]).
//! - **Readiness.** [`RoundGuard::counts`] takes only measured launch
//!   monitors.
//! - **Virtual producers.** Auto-chip and the mock launch monitor send a
//!   shot's trigger through [`RoundPolicy::send_if_allowed`], which says no
//!   while any round is active. The check and the send are one step: a
//!   round cannot begin between them, so no virtual shot starts during a
//!   round. (Auto-chip also asks [`RoundPolicy::allows`] before its solve,
//!   to answer at once.)
//!
//! The policy also holds each shot source's type ([`ShotSource`]), registered
//! when the actor starts, and puts a `round_state` event on the bus whenever
//! a round starts or ends, which the UI shows.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::broadcast;

use crate::actors::provenance::ShotSource;
use flighthook::{FlighthookEvent, FlighthookMessage, Provenance, RoundState, ShotData};

/// The round policy. Clones share it.
#[derive(Debug, Clone, Default)]
pub struct RoundPolicy(Arc<Inner>);

#[derive(Debug, Default)]
struct Inner {
    policy: Mutex<Policy>,
    /// Where a change of the round is announced, once attached.
    bus: Mutex<Option<broadcast::Sender<FlighthookMessage>>>,
}

#[derive(Debug, Default)]
struct Policy {
    /// Every shot source's type, by actor ID.
    sources: HashMap<String, ShotSource>,
    /// The rounds held, by guard, with their holder's display name. The
    /// first one taken names the round.
    holders: BTreeMap<u64, String>,
    next_guard: u64,
}

impl Policy {
    fn state(&self) -> RoundState {
        RoundState {
            active: !self.holders.is_empty(),
            holder: self.holders.values().next().cloned(),
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
pub fn not_measured_reason(provenance: Option<Provenance>, name: &str) -> Option<String> {
    match provenance {
        Some(Provenance::Measured) => None,
        Some(Provenance::Virtual) => Some(format!("{name} is a virtual launch monitor")),
        None => Some(format!("{name} is not a launch monitor flighthook reads")),
    }
}

impl RoundPolicy {
    fn lock(&self) -> MutexGuard<'_, Policy> {
        self.0.policy.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Announce every change of the round on `bus` from now on.
    pub fn attach(&self, bus: broadcast::Sender<FlighthookMessage>) {
        *self.0.bus.lock().unwrap_or_else(PoisonError::into_inner) = Some(bus);
    }

    /// Record that actor `actor` is a shot source of type `source`.
    pub fn register_source(&self, actor: &str, source: ShotSource) {
        self.lock().sources.insert(actor.to_owned(), source);
    }

    /// Forget actor `actor`: it was removed from the config. Outputs stop
    /// counting it as a launch monitor that feeds them.
    pub fn forget_source(&self, actor: &str) {
        self.lock().sources.remove(actor);
    }

    /// The shot source type of actor `actor`, `None` when it is not one.
    #[must_use]
    pub fn source(&self, actor: &str) -> Option<ShotSource> {
        self.lock().sources.get(actor).copied()
    }

    /// Whether actor `actor` is a launch monitor: a shot source with a card,
    /// a status and readiness of its own.
    #[must_use]
    pub fn is_launch_monitor(&self, actor: &str) -> bool {
        self.source(actor)
            .is_some_and(ShotSource::is_launch_monitor)
    }

    /// The round now: whether one is active and who holds it.
    #[must_use]
    pub fn state(&self) -> RoundState {
        self.lock().state()
    }

    /// Whether a round is active.
    #[must_use]
    pub fn active(&self) -> bool {
        !self.lock().holders.is_empty()
    }

    /// Whether a producer of shots of class `kind` may make one now: a
    /// measured one always, any other only while no round is active.
    #[must_use]
    pub fn allows(&self, kind: Provenance) -> bool {
        kind == Provenance::Measured || !self.active()
    }

    /// Run `send`, which puts a shot's trigger on the bus, when a producer
    /// of class `kind` may make a shot now ([`Self::allows`]); returns what
    /// it returns, `None` when it was not run. No round can begin while
    /// `send` runs, so a shot it triggers starts before any round does.
    /// `send` must not call back into the policy.
    pub fn send_if_allowed<T>(&self, kind: Provenance, send: impl FnOnce() -> T) -> Option<T> {
        let policy = self.lock();
        if kind != Provenance::Measured && !policy.holders.is_empty() {
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
                let provenance = policy.sources.get(*id).map(|s| s.provenance());
                not_measured_reason(provenance, name)
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
            match &round.holder {
                Some(holder) => format!("{holder} holds a tournament round"),
                None => "no tournament round".to_owned(),
            }
        );
        if let Some(bus) = &*self.0.bus.lock().unwrap_or_else(PoisonError::into_inner) {
            let _ = bus.send(
                FlighthookMessage::new(FlighthookEvent::RoundState { round }).actor("system"),
            );
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
    /// The shot as it counts in the round: `None` when it is not measured,
    /// else with every value that is not the device's reading removed.
    #[must_use]
    pub fn certify(&self, shot: &ShotData) -> Option<CertifiedShot> {
        certify(shot)
    }

    /// Whether a launch monitor of class `provenance` counts for the
    /// output's readiness during the round: only a measured one.
    #[must_use]
    pub fn counts(&self, provenance: Option<Provenance>) -> bool {
        provenance == Some(Provenance::Measured)
    }
}

impl Drop for RoundGuard {
    fn drop(&mut self) {
        self.policy.end(self.token);
    }
}

/// A shot as it counts in a round: measured, with only the device's
/// readings. Every value [`ShotData::unmeasured_fields`] names is removed,
/// and so are the device's flight result (FRP's measured-only rule: a
/// round takes no `flight_result`) and flighthook's own values (`derived`,
/// `metrics`). Only a held round makes one ([`RoundGuard::certify`]).
#[derive(Debug, Clone, PartialEq)]
pub struct CertifiedShot(ShotData);

impl CertifiedShot {
    /// The certified shot.
    #[must_use]
    pub fn shot(&self) -> &ShotData {
        &self.0
    }
}

/// `shot` as it counts in a round ([`RoundGuard::certify`]).
fn certify(shot: &ShotData) -> Option<CertifiedShot> {
    if shot.provenance != Some(Provenance::Measured) {
        return None;
    }
    let mut certified = shot.clone();
    for field in shot.unmeasured_fields() {
        certified.clear_field(field);
    }
    certified.flight = None;
    certified.derived = None;
    certified.metrics = None;
    Some(CertifiedShot(certified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flighthook::{
        Basis, ClubData, DerivedFlight, Distance, FaceImpact, FlightResult, Launch, ShotField,
        ShotMetrics, Velocity,
    };

    /// A policy that knows the usual sources.
    fn policy() -> RoundPolicy {
        let policy = RoundPolicy::default();
        for (id, source) in [
            ("mevo.0", ShotSource::Mevo),
            ("r10.0", ShotSource::R10),
            ("square.0", ShotSource::Square),
            ("exputt.0", ShotSource::ExPutt),
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
        assert!(policy.allows(Provenance::Virtual));
        let guard = ask(&policy, &[]).expect("accepted");
        assert!(!policy.allows(Provenance::Virtual));
        assert!(policy.allows(Provenance::Measured));
        let shared = policy.clone();
        drop(guard);
        assert!(shared.allows(Provenance::Virtual));
    }

    /// A virtual producer's send runs only while no round is active, and a
    /// round that is asked for while it runs begins only after it.
    #[test]
    fn a_round_cannot_begin_while_a_virtual_shot_is_sent() {
        let policy = policy();
        assert_eq!(policy.send_if_allowed(Provenance::Virtual, || 7), Some(7));
        let begun = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let asker = policy.send_if_allowed(Provenance::Virtual, || {
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
            policy.send_if_allowed(Provenance::Virtual, || sent = true),
            None
        );
        assert!(!sent, "nothing is sent during the round");
        assert_eq!(policy.send_if_allowed(Provenance::Measured, || 1), Some(1));
        drop(guard);
        assert_eq!(policy.send_if_allowed(Provenance::Virtual, || 2), Some(2));
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
        let rounds = |rx: &mut broadcast::Receiver<FlighthookMessage>| -> Vec<RoundState> {
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|m| match m.event {
                    FlighthookEvent::RoundState { round } => Some(round),
                    _ => None,
                })
                .collect()
        };
        let first = ask(&policy, &[]).expect("accepted");
        let second = ask(&policy, &[]).expect("accepted");
        let _ = ask(&policy, &[("mock_monitor.0", "Mock")]);
        let active = RoundState {
            active: true,
            holder: Some("Pureplay".into()),
        };
        assert_eq!(rounds(&mut rx), [active], "a second holder changes nothing");
        drop(first);
        assert!(rounds(&mut rx).is_empty());
        drop(second);
        assert_eq!(rounds(&mut rx), [RoundState::default()]);
    }

    /// A measured shot with both spin pairs and a face impact, with back
    /// and side spin and the impact height marked computed (a model's
    /// values).
    fn marked_shot() -> ShotData {
        let mut derived = None;
        DerivedFlight::mark_computed(
            &mut derived,
            &[
                ShotField::BackspinRpm,
                ShotField::SidespinRpm,
                ShotField::ImpactVertical,
            ],
        );
        let mut derived = derived.map(|d| *d);
        if let Some(d) = derived.as_mut() {
            d.carry_distance = Some(Distance::Meters(150.0));
        }
        ShotData {
            actor: "r10.0".into(),
            launch: Some(Launch {
                launch_speed: Some(Velocity::MetersPerSecond(60.0)),
                launch_elevation: Some(14.0),
                launch_azimuth: Some(1.0),
                backspin_rpm: Some(2400),
                sidespin_rpm: Some(-300),
                total_spin_rpm: Some(2419),
                spin_axis: Some(-7.1),
            }),
            flight: Some(FlightResult {
                carry_distance: Some(Distance::Meters(148.0)),
                ..FlightResult::new(Basis::Modeled)
            }),
            club: Some(ClubData {
                club_speed: Some(Velocity::MetersPerSecond(40.0)),
                ..ClubData::default()
            }),
            impact: Some(FaceImpact {
                lateral: Some(Distance::Millimeters(4.0)),
                vertical: Some(Distance::Millimeters(-2.0)),
            }),
            derived,
            metrics: Some(ShotMetrics::default()),
            provenance: Some(Provenance::Measured),
            ..ShotData::default()
        }
    }

    #[test]
    fn certify_strips_exactly_the_unmeasured_fields() {
        let shot = marked_shot();
        let certified = certify(&shot).expect("measured");
        let mut want = shot.clone();
        if let Some(b) = want.launch.as_mut() {
            b.backspin_rpm = None;
            b.sidespin_rpm = None;
        }
        if let Some(i) = want.impact.as_mut() {
            i.vertical = None;
        }
        want.flight = None;
        want.derived = None;
        want.metrics = None;
        assert_eq!(certified.shot(), &want);

        // An estimated spin takes all four spin values with it.
        let mut estimated = shot.clone();
        let mut derived = DerivedFlight::default();
        derived.spin_estimated = true;
        estimated.derived = Some(derived);
        let ball = certify(&estimated).expect("measured").shot().launch.clone();
        let ball = ball.expect("ball");
        assert_eq!(
            (
                ball.backspin_rpm,
                ball.sidespin_rpm,
                ball.total_spin_rpm,
                ball.spin_axis
            ),
            (None, None, None, None)
        );
        assert_eq!(ball.launch_elevation, Some(14.0), "the rest stays");

        // Nothing marked: only flighthook's own values go.
        let mut clean = shot.clone();
        clean.derived = None;
        let certified = certify(&clean).expect("measured");
        assert_eq!(certified.shot().launch, clean.launch);
        assert_eq!(
            certified.shot().flight,
            None,
            "a round takes no flight result"
        );
        assert_eq!(certified.shot().impact, clean.impact);
    }

    #[test]
    fn only_a_measured_shot_is_certified() {
        let guard = ask(&policy(), &[]).expect("accepted");
        for provenance in [None, Some(Provenance::Virtual)] {
            let shot = ShotData {
                provenance,
                ..marked_shot()
            };
            assert_eq!(guard.certify(&shot), None, "{provenance:?}");
        }
        assert!(guard.certify(&marked_shot()).is_some());
        assert!(guard.counts(Some(Provenance::Measured)));
        assert!(!guard.counts(Some(Provenance::Virtual)));
        assert!(!guard.counts(None));
    }
}
