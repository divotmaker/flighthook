//! Test support: scripted devices handed to a device driver under test,
//! and [`Software`], a scripted software driver.
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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use flightrelay::FrpMessage;
use flightrelay::config::{ConfigError, DriverConfig};
use flightrelay::driver::{
    Activity, ConnectCtx, DeviceEvent, DeviceIdentity, DriverInfo, Events, FrpDriver, FrpSession,
    Stopped, TurnHints,
};

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

/// A software driver's script: the events each step emits (a step past
/// the last emits nothing and idles), and where its session records what it
/// was handed.
#[derive(Debug, Default)]
pub(crate) struct Script {
    pub(crate) steps: VecDeque<Vec<DeviceEvent>>,
    pub(crate) seen: Arc<Mutex<Vec<Seen>>>,
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
        self.script
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(seen);
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
