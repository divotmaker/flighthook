//! The shots this process's simulator outputs sent
//! ([`SystemState::sent_shots`](super::SystemState)).
//!
//! A simulator that flighthook sends a shot to may tell flighthook about
//! the shot it then plays: Pureplay's Accessory API reports every shot,
//! the ones flighthook sent included. Every simulator output (the Open
//! Connect client and the Pureplay Connect output) records each shot here
//! just before it writes it, with the simulator's address, so the Pureplay
//! card can tell a shot flighthook sent (an echo) from one Pureplay had
//! from elsewhere: [`SentShots::take_echo`].
//!
//! A record is consumed by the echo it matches, together with any other
//! record of the same shot (the same shot sent by a second output), and is
//! forgotten once it is older than [`ECHO_WINDOW`]. A write that fails
//! takes its record back ([`SentShots::forget`]). At most [`CAPACITY`] are
//! kept.

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use flighthook::ShotKey;

/// Longest a simulator takes from a shot sent to it to reporting that shot:
/// a report later than this is not the echo of the send.
pub const ECHO_WINDOW: Duration = Duration::from_secs(5);

/// Most sends kept.
pub const CAPACITY: usize = 64;

/// Where a simulator is, as seen from one connection to it: its address,
/// and whether it runs on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimAddr {
    ip: IpAddr,
    this_computer: bool,
}

impl SimAddr {
    /// The simulator at `peer` of a connection whose own end is `local`:
    /// on this computer when `peer` is loopback or unspecified, or the
    /// connection's two ends have the same address.
    #[must_use]
    pub fn of(local: Option<SocketAddr>, peer: SocketAddr) -> Self {
        let ip = peer.ip();
        Self {
            ip,
            this_computer: ip.is_loopback()
                || ip.is_unspecified()
                || local.is_some_and(|l| l.ip() == ip),
        }
    }

    /// Whether `self` and `other` reach the same computer as far as this
    /// one can tell: the same address, or both this computer.
    fn same_host(self, other: Self) -> bool {
        self.ip == other.ip || (self.this_computer && other.this_computer)
    }
}

/// One shot an output sent.
#[derive(Debug, Clone)]
struct Sent {
    key: ShotKey,
    peer: SimAddr,
    at: Instant,
}

/// The shots the simulator outputs sent, oldest first.
#[derive(Debug, Default)]
pub struct SentShots {
    sent: Mutex<VecDeque<Sent>>,
}

impl SentShots {
    /// Record that an output is about to write shot `key` to the simulator
    /// at `peer`, at `at`.
    pub fn record(&self, key: ShotKey, peer: SimAddr, at: Instant) {
        let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
        expire(&mut sent, at);
        if sent.len() >= CAPACITY {
            sent.pop_front();
        }
        sent.push_back(Sent { key, peer, at });
    }

    /// Take back the newest record of shot `key` to the simulator at
    /// `peer`: the write it was made for failed, so the shot was not sent.
    pub fn forget(&self, key: &ShotKey, peer: SimAddr) {
        let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = sent.iter().rposition(|s| s.key == *key && s.peer == peer) {
            sent.remove(index);
        }
    }

    /// The send a shot the simulator at `from` reported at `now` echoes,
    /// if any; it is consumed, with every other record of the same shot (a
    /// second output's send of it). Of the sends younger than
    /// [`ECHO_WINDOW`], the oldest is the echo. When sends to more than one
    /// simulator qualify, the oldest one to a simulator on `from`'s
    /// computer is preferred; the address never rules a send out.
    pub fn take_echo(&self, from: SimAddr, now: Instant) -> Option<ShotKey> {
        let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
        expire(&mut sent, now);
        let first = sent.front()?.peer;
        let several = sent.iter().any(|s| !s.peer.same_host(first));
        let index = several
            .then(|| sent.iter().position(|s| s.peer.same_host(from)))
            .flatten()
            .unwrap_or(0);
        let key = sent.remove(index)?.key;
        sent.retain(|s| s.key.shot_id != key.shot_id);
        Some(key)
    }

    /// How many sends are held now (expired ones included until the next
    /// call that expires them).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// Drop the sends older than [`ECHO_WINDOW`] at `now`.
fn expire(sent: &mut VecDeque<Sent>, now: Instant) {
    while sent
        .front()
        .is_some_and(|s| now.saturating_duration_since(s.at) >= ECHO_WINDOW)
    {
        sent.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> ShotKey {
        ShotKey::new(format!("shot-{n}"), n)
    }

    /// The simulator at `ip`:`port`, seen from a connection whose own end
    /// is not known.
    fn at(ip: [u8; 4], port: u16) -> SimAddr {
        SimAddr::of(None, SocketAddr::from((ip, port)))
    }

    /// The card on loopback.
    fn loopback() -> SimAddr {
        at([127, 0, 0, 1], 8765)
    }

    /// No send: no echo. One send: the next report is its echo, once; a
    /// second report inside the window is not an echo.
    #[test]
    fn a_send_is_echoed_once() {
        let sent = SentShots::default();
        let t0 = Instant::now();
        assert_eq!(sent.take_echo(loopback(), t0), None);
        sent.record(key(1), at([127, 0, 0, 1], 921), t0);
        let echo = sent.take_echo(loopback(), t0 + Duration::from_millis(800));
        assert_eq!(echo, Some(key(1)));
        assert_eq!(
            sent.take_echo(loopback(), t0 + Duration::from_secs(1)),
            None
        );
    }

    /// A send with no echo expires after the window and is no echo for a
    /// later report.
    #[test]
    fn a_send_expires_after_the_window() {
        let sent = SentShots::default();
        let t0 = Instant::now();
        sent.record(key(1), at([127, 0, 0, 1], 921), t0);
        assert_eq!(sent.take_echo(loopback(), t0 + ECHO_WINDOW), None);
        assert_eq!(sent.len(), 0);
        sent.record(key(2), at([127, 0, 0, 1], 921), t0 + ECHO_WINDOW);
        let just_inside = t0 + ECHO_WINDOW + ECHO_WINDOW - Duration::from_millis(1);
        assert_eq!(sent.take_echo(loopback(), just_inside), Some(key(2)));
    }

    /// Two sends in the window: the reports take them oldest first.
    #[test]
    fn the_oldest_send_is_echoed_first() {
        let sent = SentShots::default();
        let t0 = Instant::now();
        sent.record(key(1), at([127, 0, 0, 1], 921), t0);
        sent.record(key(2), at([127, 0, 0, 1], 921), t0 + Duration::from_secs(1));
        let t = t0 + Duration::from_secs(2);
        assert_eq!(sent.take_echo(loopback(), t), Some(key(1)));
        assert_eq!(sent.take_echo(loopback(), t), Some(key(2)));
    }

    /// One shot sent by two outputs (Open Connect and Pureplay Connect):
    /// its echo takes both records, so the next shot's echo finds its own
    /// record, not the first shot's leftover.
    #[test]
    fn an_echo_takes_every_record_of_its_shot() {
        let sent = SentShots::default();
        let t0 = Instant::now();
        let oc = at([127, 0, 0, 1], 921);
        let ppc = at([127, 0, 0, 1], 9760);
        sent.record(key(1), oc, t0);
        sent.record(key(1), ppc, t0);
        assert_eq!(sent.take_echo(loopback(), t0), Some(key(1)));
        assert_eq!(sent.len(), 0, "both records of shot 1");
        sent.record(key(2), oc, t0);
        sent.record(key(2), ppc, t0);
        assert_eq!(sent.take_echo(loopback(), t0), Some(key(2)));
        assert_eq!(sent.take_echo(loopback(), t0), None);
    }

    /// A write that failed takes its own record back, and only that one.
    #[test]
    fn a_failed_write_takes_its_record_back() {
        let sent = SentShots::default();
        let t0 = Instant::now();
        let oc = at([127, 0, 0, 1], 921);
        let ppc = at([127, 0, 0, 1], 9760);
        sent.record(key(1), oc, t0);
        sent.record(key(2), oc, t0);
        sent.record(key(2), ppc, t0);
        sent.forget(&key(2), oc);
        assert_eq!(sent.len(), 2);
        sent.forget(&key(3), oc);
        assert_eq!(sent.len(), 2, "no such record");
        assert_eq!(sent.take_echo(loopback(), t0), Some(key(1)));
        sent.forget(&key(2), ppc);
        assert_eq!(sent.take_echo(loopback(), t0), None);
    }

    /// Sends to two simulators: the one on the card's computer is
    /// preferred, loopback and an address the connection's own end has
    /// counting as one. A lone send to another address is still the echo:
    /// the card on the LAN address and the output on loopback reach the
    /// same Pureplay.
    #[test]
    fn the_address_only_breaks_ties() {
        let sent = SentShots::default();
        let t0 = Instant::now();
        sent.record(key(1), at([10, 99, 0, 7], 921), t0);
        sent.record(key(2), at([127, 0, 0, 1], 9760), t0);
        let t = t0 + Duration::from_secs(1);
        assert_eq!(sent.take_echo(loopback(), t), Some(key(2)));
        assert_eq!(sent.take_echo(loopback(), t), Some(key(1)), "a lone send");

        sent.record(key(3), at([127, 0, 0, 1], 921), t0);
        sent.record(key(4), at([10, 99, 0, 7], 921), t0);
        let lan = at([10, 99, 0, 7], 8765);
        assert_eq!(sent.take_echo(lan, t), Some(key(4)));
        assert_eq!(sent.take_echo(lan, t), Some(key(3)));

        // The card on this computer's LAN address (its connection's two
        // ends share it) and an output on loopback: the same computer.
        let own_lan = SimAddr::of(
            Some(SocketAddr::from(([10, 99, 0, 9], 50123))),
            SocketAddr::from(([10, 99, 0, 9], 8765)),
        );
        sent.record(key(5), at([10, 99, 0, 7], 921), t0);
        sent.record(key(6), at([127, 0, 0, 1], 921), t0);
        assert_eq!(sent.take_echo(own_lan, t), Some(key(6)));
    }

    /// At most [`CAPACITY`] sends are kept: the oldest goes.
    #[test]
    fn the_registry_is_bounded() {
        let sent = SentShots::default();
        let t0 = Instant::now();
        for n in 0..=u32::try_from(CAPACITY).unwrap() {
            sent.record(key(n), at([127, 0, 0, 1], 921), t0);
        }
        assert_eq!(sent.len(), CAPACITY);
        assert_eq!(sent.take_echo(loopback(), t0), Some(key(1)));
    }

    #[test]
    fn loopback_unspecified_and_a_shared_address_are_this_computer() {
        assert!(loopback().same_host(at([127, 0, 0, 2], 921)));
        assert!(loopback().same_host(at([0, 0, 0, 0], 921)));
        assert!(!loopback().same_host(at([192, 0, 2, 1], 921)));
        let shared = SimAddr::of(
            Some(SocketAddr::from(([192, 0, 2, 1], 50000))),
            SocketAddr::from(([192, 0, 2, 1], 921)),
        );
        assert!(loopback().same_host(shared));
    }
}
