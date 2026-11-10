// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared updater wake signaling and connection-recovery classification.

use std::time::{Duration, Instant, SystemTime};

/// Wall-clock excess over a requested sleep slice that indicates the machine was asleep.
pub(crate) const SLEEP_GAP: Duration = Duration::from_secs(90);

/// Whether an update-check error `message` indicates an unreachable network.
pub(crate) fn is_network_unreachable(message: &str) -> bool {
    // Resolving the proxy is as necessary to reaching the channel as resolving its host.
    const UNREACHABLE: [u32; 4] = [5, 6, 7, 28];
    if !message.contains("curl") {
        return false;
    }
    let code_after = |marker: &str| {
        message.match_indices(marker).any(|(at, _)| {
            let digits: String = message[at + marker.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits
                .parse::<u32>()
                .is_ok_and(|code| UNREACHABLE.contains(&code))
        })
    };
    code_after("curl: (") || code_after("exit status: ") || code_after("(exit ")
}

/// Outcome of a cadence sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waited {
    /// Full delay elapsed normally.
    Elapsed,
    /// The caller no longer owns this wait.
    Cancelled,
    /// A wake signal, network change, or system sleep gap was detected; the
    /// duration is any observed wall-clock excess, zero when there was none.
    Woke(Duration),
}

/// Whether `observed` wall-clock elapsed time exceeds `slice` by at least [`SLEEP_GAP`].
pub(crate) fn is_wake_gap(slice: Duration, observed: Duration) -> bool {
    observed.saturating_sub(slice) >= SLEEP_GAP
}

/// Cross-thread wake signal, also polling the host's settled network changes.
pub(crate) struct WakeSignal {
    posts: std::sync::Mutex<u64>,
    posted: std::sync::Condvar,
    network_changed: std::sync::OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
}

impl WakeSignal {
    pub(crate) const fn new() -> Self {
        Self {
            posts: std::sync::Mutex::new(0),
            posted: std::sync::Condvar::new(),
            network_changed: std::sync::OnceLock::new(),
        }
    }

    /// The host supplies a cheap, non-networking, consuming change probe.
    #[cfg(any(target_os = "macos", target_os = "linux", test))]
    pub(crate) fn set_network_probe(&self, probe: Box<dyn Fn() -> bool + Send + Sync>) {
        let _ = self.network_changed.set(probe);
    }

    fn poll_network(&self) {
        if self.network_changed.get().is_some_and(|probe| probe()) {
            // Publish to every generation's waiter, even when a retiring
            // checker was the one that consumed the host's change signal.
            self.post();
        }
    }

    /// Notify all waiters that the system woke.
    pub(crate) fn post(&self) {
        let mut posts = self
            .posts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *posts = posts.wrapping_add(1);
        self.posted.notify_all();
    }

    /// Total wake notifications posted so far.
    pub(crate) fn posts(&self) -> u64 {
        *self
            .posts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait up to `timeout` for a post past the `seen`-th; whether one came.
    pub(crate) fn wait_past(&self, seen: u64, timeout: Duration) -> bool {
        self.poll_network();
        let posts = self
            .posts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (posts, _) = self
            .posted
            .wait_timeout_while(posts, timeout, |posts| *posts == seen)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *posts != seen {
            return true;
        }
        drop(posts);
        self.poll_network();
        self.posts() != seen
    }
}

/// Process-wide system wake and network-change signal.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) static SYSTEM_WOKE: WakeSignal = WakeSignal::new();

/// Maximum sleep slice used by [`sleep_watching_for_wake`].
pub(crate) const WAKE_SLICE: Duration = Duration::from_secs(5);

/// Sleep for `total` in [`WAKE_SLICE`] increments, returning early on `signal`,
/// a wall-clock jump, or lost ownership. Cancellation takes precedence over a wake.
pub(crate) fn sleep_watching_for_wake(
    signal: &WakeSignal,
    seen: u64,
    total: Duration,
    keep_waiting: impl FnMut() -> bool,
) -> Waited {
    let started = Instant::now();
    wait_slices(
        total,
        || started.elapsed(),
        keep_waiting,
        |slice| {
            let before = SystemTime::now();
            let ran = Instant::now();
            let posted = signal.wait_past(seen, slice);
            let observed = before.elapsed().unwrap_or(slice);
            if posted {
                // The wall clock's lead over this thread's own running time, for the log.
                return Waited::Woke(observed.saturating_sub(ran.elapsed()));
            }
            if is_wake_gap(slice, observed) {
                return Waited::Woke(observed.saturating_sub(slice));
            }
            Waited::Elapsed
        },
    )
}

/// Scheduling delays consume the same budget as the requested sleeps.
fn wait_slices(
    total: Duration,
    mut elapsed: impl FnMut() -> Duration,
    mut keep_waiting: impl FnMut() -> bool,
    mut wait: impl FnMut(Duration) -> Waited,
) -> Waited {
    loop {
        if !keep_waiting() {
            return Waited::Cancelled;
        }
        let remaining = total.saturating_sub(elapsed());
        if remaining.is_zero() {
            return Waited::Elapsed;
        }
        let outcome = wait(remaining.min(WAKE_SLICE));
        if !keep_waiting() {
            return Waited::Cancelled;
        }
        if matches!(outcome, Waited::Woke(_)) {
            return outcome;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn scheduler_delays_consume_the_wait_budget_instead_of_extending_it() {
        let elapsed = Cell::new(Duration::ZERO);
        let mut slices = Vec::new();
        let result = wait_slices(
            Duration::from_secs(20),
            || elapsed.get(),
            || true,
            |slice| {
                slices.push(slice);
                // The first five-second request resumes twelve seconds late, below
                // the system-sleep gap detector's threshold.
                elapsed.set(
                    elapsed.get()
                        + if slices.len() == 1 {
                            Duration::from_secs(17)
                        } else {
                            slice
                        },
                );
                Waited::Elapsed
            },
        );
        assert_eq!(result, Waited::Elapsed);
        assert_eq!(slices, [Duration::from_secs(5), Duration::from_secs(3)]);
        assert_eq!(elapsed.get(), Duration::from_secs(20));
    }

    #[test]
    fn an_expired_budget_does_not_wait_and_a_wake_still_ends_the_wait() {
        assert_eq!(
            wait_slices(
                Duration::from_secs(20),
                || Duration::from_secs(21),
                || true,
                |_| panic!("budget already expired")
            ),
            Waited::Elapsed
        );
        let mut calls = 0;
        let woke = Waited::Woke(Duration::from_secs(150));
        assert_eq!(
            wait_slices(
                Duration::from_secs(20),
                || Duration::ZERO,
                || true,
                |slice| {
                    calls += 1;
                    assert_eq!(slice, WAKE_SLICE);
                    woke
                }
            ),
            woke
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn lost_ownership_cancels_before_sleep_and_before_reporting_a_wake() {
        assert_eq!(
            wait_slices(
                Duration::from_secs(86_400),
                || Duration::ZERO,
                || false,
                |_| panic!("retired wait slept")
            ),
            Waited::Cancelled
        );
        let current = Cell::new(true);
        let mut calls = 0;
        let result = wait_slices(
            Duration::from_secs(86_400),
            || Duration::ZERO,
            || current.get(),
            |_| {
                calls += 1;
                current.set(false);
                Waited::Woke(Duration::ZERO)
            },
        );
        assert_eq!(result, Waited::Cancelled);
        assert_eq!(calls, 1);
    }
}
