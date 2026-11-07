// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One process's "network moved" signal (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.5.2). The
//! kernel's route socket ([`hear_routes`]) and the standing `_alab-vault._tcp` browse
//! ([`crate::discover::Watch`]) each ring a [`Bell`], and the server's advert's also rings at
//! this Mac's wake from sleep ([`aterm_uds::power`]). A consumer (the server's advert, the
//! subscriber's error nap) naps on that bell, and the nap says which source woke it. Nothing
//! carries the signal between processes: each process hears its own sources.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// What ended a [`Bell::nap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The nap ran its whole length.
    Timer,
    /// This Mac's addresses or links changed ([`crate::network::RouteChanges`]).
    Route,
    /// The standing browse heard a vault server it had not heard ([`crate::discover::Watch`]).
    Browse,
    /// The local LAN broker began answering its verified hello.
    Broker,
    /// The subscriber's local readiness listener ended and needs upkeep.
    Listener,
    /// The server stored a new version, which its advert names.
    Put,
    /// This Mac woke from a sleep ([`aterm_uds::power::Power::Woke`]), before which the server
    /// ended its registrations.
    Resumed,
    /// The consumer is ending.
    Stop,
}

impl Wake {
    /// The words a log line gives this wake, after "woken by".
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Wake::Timer => "its timer",
            Wake::Route => "a route change",
            Wake::Browse => "a vault server the browse heard",
            Wake::Broker => "the LAN broker becoming ready",
            Wake::Listener => "the readiness listener ending",
            Wake::Put => "a new version stored",
            Wake::Resumed => "this Mac waking from sleep",
            Wake::Stop => "its end",
        }
    }
}

/// A wake-up that any source rings and one consumer naps on. A ring that comes while nobody
/// naps is kept for the next nap. Distinct causes are retained in arrival order;
/// repeated rings from the same source coalesce until the consumer takes it.
#[derive(Debug, Default)]
pub struct Bell {
    rung: Mutex<VecDeque<Wake>>,
    cond: Condvar,
}

impl Bell {
    /// A bell nobody rang.
    #[must_use]
    pub const fn new() -> Bell {
        Bell {
            rung: Mutex::new(VecDeque::new()),
            cond: Condvar::new(),
        }
    }

    /// Ring: the nap under way, or else the next one, ends with `why`.
    pub fn ring(&self, why: Wake) {
        let mut rung = self.rung.lock().unwrap_or_else(PoisonError::into_inner);
        if !rung.contains(&why) {
            rung.push_back(why);
        }
        self.cond.notify_all();
    }

    /// Wait at most `most` for a ring: the one that came, or [`Wake::Timer`].
    pub fn nap(&self, most: Duration) -> Wake {
        const MAX_NAP: Duration = Duration::from_secs(365 * 24 * 60 * 60);
        let deadline = Instant::now() + most.min(MAX_NAP);
        let mut rung = self.rung.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(why) = rung.pop_front() {
                return why;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Wake::Timer;
            }
            rung = self
                .cond
                .wait_timeout(rung, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Forget rings that no nap took: facts the next pass will read itself.
    /// Returns the first discarded cause, when any was pending.
    pub fn clear(&self) -> Option<Wake> {
        let mut rung = self.rung.lock().unwrap_or_else(PoisonError::into_inner);
        let first = rung.pop_front();
        rung.clear();
        first
    }
}

/// How often a [`hear`] thread looks at whether its bell is gone.
const HEAR_STEP: Duration = Duration::from_secs(5);

/// The kernel's route socket as a waiter for [`hear`]: it waits at most the time it is given
/// for a change of this Mac's addresses or links, and says whether one came. `None` where the
/// kernel gives this process no route socket.
#[must_use]
pub fn route_changes() -> Option<impl FnMut(Duration) -> bool + Send + 'static> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let changes = crate::network::RouteChanges::open().ok()?;
        Some(move |most: Duration| changes.wait(most))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None::<fn(Duration) -> bool>
    }
}

/// Ring `bell` with [`Wake::Route`] at each change of this Mac's addresses or links
/// ([`route_changes`]), from a thread of its own that ends once the bell is dropped. Whether it
/// hears them: where the kernel gives no route socket, a nap runs to its timer.
pub fn hear_routes(bell: &Arc<Bell>) -> bool {
    route_changes().is_some_and(|changes| hear(bell, changes, Wake::Route))
}

/// Ring `bell` with `why` each time `changed` says a change came. `changed` waits at most the
/// time it is given. It runs on a thread of its own, which ends once the bell is dropped.
/// Whether the thread started.
pub fn hear(
    bell: &Arc<Bell>,
    mut changed: impl FnMut(Duration) -> bool + Send + 'static,
    why: Wake,
) -> bool {
    let bell = Arc::downgrade(bell);
    std::thread::Builder::new()
        .name("vault-hear".to_owned())
        .spawn(move || {
            loop {
                let came = changed(HEAR_STEP);
                let Some(bell) = bell.upgrade() else {
                    return;
                };
                if came {
                    bell.ring(why);
                }
            }
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    /// Rings wake a nap or remain queued. Distinct causes survive an earlier
    /// route hint; repeats coalesce, and an empty queue waits for its timer.
    #[test]
    fn a_nap_ends_with_the_first_ring_or_its_timer() {
        let bell = Arc::new(Bell::default());
        assert_eq!(bell.nap(Duration::from_millis(10)), Wake::Timer);
        bell.ring(Wake::Route);
        bell.ring(Wake::Browse);
        bell.ring(Wake::Broker);
        bell.ring(Wake::Route);
        assert_eq!(
            bell.nap(HANG_DETECTOR),
            Wake::Route,
            "kept for the next nap"
        );
        assert_eq!(bell.nap(HANG_DETECTOR), Wake::Browse);
        assert_eq!(bell.nap(HANG_DETECTOR), Wake::Broker);
        assert_eq!(bell.nap(Duration::from_millis(10)), Wake::Timer, "taken");
        let ringer = {
            let bell = Arc::clone(&bell);
            std::thread::spawn(move || bell.ring(Wake::Browse))
        };
        assert_eq!(bell.nap(HANG_DETECTOR), Wake::Browse);
        ringer.join().expect("rang");
        bell.ring(Wake::Put);
        assert_eq!(bell.clear(), Some(Wake::Put));
        assert_eq!(bell.nap(Duration::from_millis(10)), Wake::Timer);
        let max_ringer = {
            let bell = Arc::clone(&bell);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(10));
                bell.ring(Wake::Put);
            })
        };
        assert_eq!(bell.nap(Duration::MAX), Wake::Put);
        max_ringer.join().expect("rang max");
    }

    /// A source heard rings its bell each time it says a change came, and its thread ends once
    /// the bell is gone.
    /// NEGATIVE CONTROL: a hearer that never rings leaves the nap to its timer.
    #[test]
    fn a_heard_source_rings_its_bell_and_ends_with_it() {
        let bell = Arc::new(Bell::default());
        let (tx, rx) = mpsc::channel::<()>();
        let (looked, looks) = mpsc::channel::<()>();
        assert!(hear(
            &bell,
            move |most| {
                let _ = looked.send(());
                rx.recv_timeout(most).is_ok()
            },
            Wake::Route,
        ));
        tx.send(()).expect("a change");
        assert_eq!(bell.nap(HANG_DETECTOR), Wake::Route);
        drop(bell);
        tx.send(()).expect("one more change");
        // The thread sees its bell gone after this change and stops looking: its waiter (and
        // with it this channel's sender) is dropped.
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            match looks.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(()) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("the hearer outlived its bell"),
            }
        }
    }
}
