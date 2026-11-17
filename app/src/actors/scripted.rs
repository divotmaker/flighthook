//! Test support: scripted devices handed to a device driver under test,
//! [`Stepped`], a host that runs a real driver's session on the test's
//! thread, and [`Software`], a scripted software driver.
//!
//! [`FrpDriver::new`](flightrelay::driver::FrpDriver::new) gets only
//! the card's config, so a test parks its scripted device here under a key
//! taken from that config (an address, a range), and the test driver takes
//! it out in `new`. Every test picks its own key; two tests that share one
//! would steal each other's device, so parking under a key that is already
//! taken fails the test that does it.

use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use flighthook::ShotKey;
use flightrelay::config::{ConfigError, DriverConfig};
use flightrelay::driver::{
    Activity, ConnectCtx, ConnectHost, DeviceEvent, DeviceIdentity, DriverInfo, Events, FrpDriver,
    FrpSession, HostFlags, IDLE, Stop, Stopped, TurnHints,
};
use flightrelay::{FrpBody, FrpMessage, ShotFinished, ShotTrigger};
use tokio::sync::broadcast;

use crate::bus::BusMsg;

/// The next message on `rx`, blocking until one arrives; `None` when the
/// bus closes or nothing arrives within [`GIVE_UP`](crate::actors::GIVE_UP)
/// (the only bound: a test waits on the message it expects).
pub(crate) fn recv_within<T: Clone>(rx: &mut broadcast::Receiver<T>) -> Option<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        loop {
            match tokio::time::timeout(crate::actors::GIVE_UP, rx.recv()).await {
                Ok(Ok(msg)) => return Some(msg),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return None,
            }
        }
    })
}

/// Scripted devices of type `T` waiting for a driver, by key `K`.
pub(crate) struct Handoff<K, T>(Mutex<Option<HashMap<K, T>>>);

impl<K: Eq + Hash + Debug, T> Handoff<K, T> {
    /// An empty handoff, for a `static`.
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// Park `device` under `key`.
    ///
    /// # Panics
    ///
    /// When `key` already holds a device: another test uses the same key.
    pub(crate) fn put(&self, key: K, device: T) {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let map = map.get_or_insert_with(HashMap::new);
        assert!(
            !map.contains_key(&key),
            "scripted device key {key:?} is taken: give each test its own"
        );
        map.insert(key, device);
    }

    /// Take the device parked under `key`, if any.
    pub(crate) fn take(&self, key: &K) -> Option<T> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .and_then(|map| map.remove(key))
    }
}

// ---------------------------------------------------------------------------
// A real driver's session on this thread
// ---------------------------------------------------------------------------

/// The body of `m`, when it is an FRP message.
pub(crate) fn body(m: &BusMsg) -> Option<&FrpBody> {
    m.as_frp().map(|f| &f.body)
}

/// A host for one session of a real driver on the test's thread: a clock
/// that moves only when the driver waits or the session idles, and every
/// event the driver emitted, for [`card_bus`](crate::actors::driver::tests::card_bus).
pub(crate) struct Stepped {
    clock: Clock,
    flags: HostFlags,
    stop: Stop,
    /// What the driver emitted so far.
    pub(crate) events: Vec<DeviceEvent>,
}

/// Active steps in a row after which [`Stepped::until`] gives up: a fast
/// replay (whose clock is its input's) takes one step per millisecond of
/// its input.
const MAX_BUSY_STEPS: u32 = 1_000_000;

struct Clock {
    now: Instant,
    hints: TurnHints,
}

impl ConnectHost for Clock {
    fn tick(&mut self) {}

    fn hints(&self) -> &TurnHints {
        &self.hints
    }

    fn now(&self) -> Instant {
        self.now
    }

    fn wait(&mut self, d: Duration) {
        self.now += d;
    }
}

impl Stepped {
    /// A host whose connects see `hints`, raw frames wanted.
    pub(crate) fn new(hints: TurnHints) -> Self {
        let flags = HostFlags::new();
        flags.set_wants_raw(true);
        Self {
            clock: Clock {
                now: Instant::now(),
                hints,
            },
            flags,
            stop: Stop::new(),
            events: Vec::new(),
        }
    }

    /// Any connect-shaped call against this host.
    pub(crate) fn connect<T, E>(
        &mut self,
        f: impl FnOnce(&mut ConnectCtx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let events = &mut self.events;
        let mut sink = |e: DeviceEvent| events.push(e);
        let mut ctx = ConnectCtx::new(
            &self.stop,
            Events::new(&mut sink, &self.flags),
            &mut self.clock,
        );
        f(&mut ctx)
    }

    /// Step with `step` until an event matching `want` arrives: again at
    /// the same instant after an active step, [`IDLE`] later after an idle
    /// one.
    ///
    /// # Panics
    ///
    /// When a step fails, or nothing matches within
    /// [`GIVE_UP`](crate::actors::GIVE_UP) of the host's clock or
    /// [`MAX_BUSY_STEPS`] active steps in a row.
    pub(crate) fn until<E: Debug>(
        &mut self,
        mut step: impl FnMut(Instant, &mut Events<'_>) -> Result<Activity, E>,
        want: impl Fn(&DeviceEvent) -> bool,
    ) {
        let from = self.events.len();
        let give_up = self.clock.now + crate::actors::GIVE_UP;
        let mut busy = 0u32;
        while !self.events[from..].iter().any(&want) {
            assert!(
                self.clock.now < give_up && busy < MAX_BUSY_STEPS,
                "never seen: {:?}",
                &self.events[from..]
            );
            let events = &mut self.events;
            let mut sink = |e: DeviceEvent| events.push(e);
            let activity =
                step(self.clock.now, &mut Events::new(&mut sink, &self.flags)).expect("a step");
            if activity == Activity::Idle {
                busy = 0;
                self.clock.now += IDLE;
            } else {
                busy += 1;
            }
        }
    }
}

/// Whether `event` is a `shot.finished`.
pub(crate) fn is_finished(event: &DeviceEvent) -> bool {
    matches!(event, DeviceEvent::Frp(FrpBody::ShotFinished(_)))
}

/// The events of one shot `key` as a driver emits them: its trigger, then
/// `parts` (the launch, flight, club, impact), then `shot.finished`.
pub(crate) fn shot_events(key: &ShotKey, parts: Vec<FrpBody>) -> Vec<DeviceEvent> {
    std::iter::once(ShotTrigger::new(key.clone()).into())
        .chain(parts)
        .chain(std::iter::once(ShotFinished { key: key.clone() }.into()))
        .map(DeviceEvent::Frp)
        .collect()
}

// ---------------------------------------------------------------------------
// A scripted software driver
// ---------------------------------------------------------------------------

/// The config a [`Software`] driver runs under: the facts of the driver it
/// stands in for, and the key its script is parked under in [`SOFTWARE`].
pub(crate) trait SoftwareConfig: DriverConfig {
    fn info(&self) -> DriverInfo;
    fn key(&self) -> String;
}

/// What a [`SoftwareSession`] was handed, in order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Seen {
    Bus(Box<FrpMessage>),
    Hints(TurnHints),
    Action(String),
    /// A step, at its `now`.
    Step(Instant),
}

/// What a session was handed, with a signal for each addition.
#[derive(Debug, Default)]
pub(crate) struct SeenLog {
    seen: Mutex<Vec<Seen>>,
    added: Condvar,
}

impl SeenLog {
    /// What was handed so far.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Vec<Seen>> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, seen: Seen) {
        self.lock().push(seen);
        self.added.notify_all();
    }

    /// What was handed, once `done` holds of it: blocks on the signal for
    /// each addition, bounded only by [`GIVE_UP`](crate::actors::GIVE_UP).
    ///
    /// # Panics
    ///
    /// When `done` does not hold within the bound.
    pub(crate) fn wait_until(&self, done: impl Fn(&[Seen]) -> bool) -> Vec<Seen> {
        let (seen, timeout) = self
            .added
            .wait_timeout_while(self.lock(), crate::actors::GIVE_UP, |seen| !done(seen))
            .unwrap_or_else(PoisonError::into_inner);
        assert!(!timeout.timed_out(), "never handed: {:?}", *seen);
        seen.clone()
    }
}

/// A software driver's script: the events each step emits (a step past
/// the last emits nothing and idles), and where its session records what it
/// was handed.
#[derive(Debug, Default)]
pub(crate) struct Script {
    pub(crate) steps: VecDeque<Vec<DeviceEvent>>,
    pub(crate) seen: Arc<SeenLog>,
}

/// Scripts waiting for a [`Software`] driver, by [`SoftwareConfig::key`].
pub(crate) static SOFTWARE: Handoff<String, Script> = Handoff::new();

/// A scripted software driver with config `C`: its connect opens a session
/// at once on the script parked under the config's key (an empty script
/// when there is none, or for a later connect).
pub(crate) struct Software<C> {
    config: C,
    script: Option<Script>,
}

/// A [`Software`] driver's session: records what it is handed into its
/// script's `seen` and emits the script's steps.
pub(crate) struct SoftwareSession<C> {
    identity: DeviceIdentity,
    script: Script,
    _config: PhantomData<fn() -> C>,
}

impl<C> SoftwareSession<C> {
    /// A session of device `device` on `script`.
    pub(crate) fn new(device: &str, script: Script) -> Self {
        Self {
            identity: DeviceIdentity::new(device),
            script,
            _config: PhantomData,
        }
    }

    fn see(&self, seen: Seen) {
        self.script.seen.push(seen);
    }
}

/// A [`Software`] driver's error.
#[derive(Debug)]
pub(crate) struct SoftwareError;

impl std::fmt::Display for SoftwareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped")
    }
}

impl std::error::Error for SoftwareError {}

impl From<Stopped> for SoftwareError {
    fn from(_: Stopped) -> Self {
        Self
    }
}

impl<C: SoftwareConfig> FrpDriver for Software<C> {
    type Config = C;
    type Error = SoftwareError;
    type Action = String;
    type Session = SoftwareSession<C>;

    fn info(config: &C) -> DriverInfo {
        config.info()
    }

    fn new(config: C) -> Result<Self, ConfigError> {
        config.validate()?;
        let script = SOFTWARE.take(&config.key());
        Ok(Self { config, script })
    }

    fn config(&self) -> &C {
        &self.config
    }

    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<SoftwareSession<C>, SoftwareError> {
        ctx.tick()?;
        let script = self.script.take().unwrap_or_default();
        Ok(SoftwareSession::new(&self.config.key(), script))
    }
    fn apply_config(&mut self, new: &C) {
        self.config = new.clone();
    }
}

impl<C: SoftwareConfig> FrpSession for SoftwareSession<C> {
    type Driver = Software<C>;

    fn step(&mut self, now: Instant, out: &mut Events<'_>) -> Result<Activity, SoftwareError> {
        self.see(Seen::Step(now));
        match self.script.steps.pop_front() {
            Some(events) => {
                for event in events {
                    out.emit(event);
                }
                Ok(Activity::Active)
            }
            None => Ok(Activity::Idle),
        }
    }

    fn hints(&mut self, hints: &TurnHints) -> Result<(), SoftwareError> {
        self.see(Seen::Hints(*hints));
        Ok(())
    }

    fn bus(&mut self, msg: &FrpMessage) -> Result<(), SoftwareError> {
        self.see(Seen::Bus(Box::new(msg.clone())));
        Ok(())
    }

    fn action(&mut self, action: &String) -> Result<(), SoftwareError> {
        self.see(Seen::Action(action.clone()));
        Ok(())
    }

    fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    fn close(self, _: &mut Events<'_>) -> Result<(), SoftwareError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_is_taken_once() {
        static DEVICES: Handoff<&str, u32> = Handoff::new();
        DEVICES.put("a", 1);
        assert_eq!(DEVICES.take(&"a"), Some(1));
        assert_eq!(DEVICES.take(&"a"), None);
        DEVICES.put("a", 2);
        assert_eq!(DEVICES.take(&"a"), Some(2));
    }

    #[test]
    #[should_panic(expected = "is taken")]
    fn a_shared_key_fails_the_test() {
        static DEVICES: Handoff<&str, u32> = Handoff::new();
        DEVICES.put("b", 1);
        DEVICES.put("b", 2);
    }
}
