// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The clock of a bounded run that carries a [`Queue`]: the run's own time, which leaves out
//! the time its builds waited to be admitted.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::tools::Queue;

/// How often a clock reads the queue, and so how late past its bound a run can be ended.
pub const LOOK: Duration = Duration::from_secs(1);

/// A run's own time against its bound.
#[derive(Clone, Copy, Debug)]
pub struct QueueClock {
    within: Duration,
    started: Instant,
    looked: Instant,
    waited: Duration,
}

impl QueueClock {
    /// A run bounded by `within` that started at `now`.
    #[must_use]
    pub const fn start(within: Duration, now: Instant) -> Self {
        Self {
            within,
            started: now,
            looked: now,
            waited: Duration::ZERO,
        }
    }

    /// Whether the run's own time has reached its bound at `now`. It is judged once a [`LOOK`]:
    /// `waiting` then reads whether a build of the run waits to be admitted, and where one does,
    /// the time since the look before is the queue's and not the run's. Between looks the
    /// answer is `false`.
    pub fn over(&mut self, now: Instant, waiting: &mut dyn FnMut() -> bool) -> bool {
        let since = now.saturating_duration_since(self.looked);
        if since < LOOK {
            return false;
        }
        if waiting() {
            self.waited = self.waited.saturating_add(since);
        }
        self.looked = now;
        self.own(now) >= self.within
    }

    /// The run's own time at `now`: the time since it started less the time it waited.
    #[must_use]
    pub fn own(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
            .saturating_sub(self.waited)
    }

    /// How long the run's builds have waited so far.
    #[must_use]
    pub const fn waited(&self) -> Duration {
        self.waited
    }
}

/// Whether one of `claims` waits for a build in one of `checkouts`. A claim is whether it is
/// admitted and the checkout it was made for; an admitted claim is the run's own time.
pub fn waits_in<'a>(
    claims: impl IntoIterator<Item = (bool, &'a str)>,
    checkouts: &[PathBuf],
) -> bool {
    claims.into_iter().any(|(admitted, checkout)| {
        !admitted && checkouts.iter().any(|c| c.as_path() == Path::new(checkout))
    })
}

/// Whether a claim of this user's ledger waits for a build in one of `checkouts`. A ledger that
/// cannot be read shows no wait: the run's bound then counts every second.
fn claim_waits(checkouts: &[PathBuf]) -> bool {
    aterm_capacity::Ledger::user()
        .and_then(|ledger| ledger.list().ok())
        .is_some_and(|records| {
            waits_in(
                records
                    .iter()
                    .map(|r| (r.held.is_some(), r.checkout.as_str())),
                checkouts,
            )
        })
}

/// A [`QueueClock`] on this machine's clock and ledger, which keeps its [`Queue`] told of the
/// wait it has seen.
#[derive(Debug)]
pub struct HostClock<'a> {
    clock: QueueClock,
    queue: &'a Queue,
    checkouts: Vec<PathBuf>,
}

impl<'a> HostClock<'a> {
    /// A run bounded by `within` that starts now and builds in `queue`'s checkout, which a
    /// claim names as it is written or resolved.
    #[must_use]
    pub fn start(within: Duration, queue: &'a Queue) -> Self {
        let mut checkouts = vec![queue.checkout.clone()];
        if let Ok(resolved) = std::fs::canonicalize(&queue.checkout)
            && resolved != queue.checkout
        {
            checkouts.push(resolved);
        }
        Self {
            clock: QueueClock::start(within, Instant::now()),
            queue,
            checkouts,
        }
    }

    /// [`QueueClock::over`] now.
    pub fn over(&mut self) -> bool {
        let checkouts = &self.checkouts;
        let over = self
            .clock
            .over(Instant::now(), &mut || claim_waits(checkouts));
        self.queue.set_waited(self.clock.waited());
        over
    }
}

#[cfg(test)]
#[path = "queue_clock_tests.rs"]
mod tests;
