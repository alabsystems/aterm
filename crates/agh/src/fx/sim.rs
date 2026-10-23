// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The simulated clock behind `Fx::simulated`: a test's time, moved only by sleeps.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// A virtual monotonic clock from 0 and a wall clock from a start, both moved only by `sleep`,
/// by exactly its length; every sleep recorded.
pub(super) struct SimClock {
    start_unix_secs: u64,
    /// The time slept so far, and each sleep in order.
    state: Mutex<(Duration, Vec<Duration>)>,
}

impl SimClock {
    /// Both clocks at their start.
    pub(super) const fn new(start_unix_secs: u64) -> SimClock {
        SimClock {
            start_unix_secs,
            state: Mutex::new((Duration::ZERO, Vec::new())),
        }
    }

    /// The time slept so far.
    pub(super) fn mono(&self) -> Duration {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0
    }

    /// `start_unix_secs` plus the whole seconds slept.
    pub(super) fn wall_secs(&self) -> u64 {
        self.start_unix_secs.saturating_add(self.mono().as_secs())
    }

    /// Move both clocks by `d`, taking no real time.
    pub(super) fn sleep(&self, d: Duration) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = state.0.saturating_add(d);
        state.1.push(d);
    }

    /// Every sleep so far, in order.
    pub(super) fn slept(&self) -> Vec<Duration> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .clone()
    }
}
