//! Test support: scripted devices handed to a device driver under test.
//!
//! [`DeviceDriver::new`](flightrelay::driver::DeviceDriver::new) gets only
//! the card's config, so a test parks its scripted device here under a key
//! taken from that config (an address, a range), and the test driver takes
//! it out in `new`. Every test picks its own key; two tests that share one
//! would steal each other's device, so parking under a key that is already
//! taken fails the test that does it.

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::{Mutex, PoisonError};

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
