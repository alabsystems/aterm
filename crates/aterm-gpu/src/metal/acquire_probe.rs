// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native acquire conformance only. The normal build contains neither this gate
//! nor a configuration switch for it. The driver arms ONE real acquisition after
//! an attached window has presented, observes entry, and releases it independently
//! of AppKit. Both the worker and historical synchronous control cross this same
//! seam immediately before `nextDrawable`.

use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

static PROBE: OnceLock<Arc<AcquireProbe>> = OnceLock::new();
const ABANDONED_DRIVER_BACKSTOP: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
struct State {
    armed: bool,
    entered: bool,
    released: bool,
    entries: u64,
    timed_out: bool,
}

/// A process-local, single-acquisition latch. Install before GUI initialization;
/// initially disarmed so baseline pixels use the actual native drawable path.
#[derive(Debug)]
pub struct AcquireProbe {
    synchronous_control: bool,
    state: Mutex<State>,
    changed: Condvar,
}

impl AcquireProbe {
    #[must_use]
    pub fn new(synchronous_control: bool) -> Arc<Self> {
        Arc::new(Self {
            synchronous_control,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        })
    }

    /// Refuse overlapping arms and reuse after entry: one process proves one
    /// acquisition, so no second round can race the first waiter's retirement.
    #[must_use]
    pub fn arm(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.armed || state.entries != 0 {
            return false;
        }
        state.armed = true;
        state.entered = false;
        state.released = false;
        true
    }

    #[must_use]
    pub fn entered(&self) -> bool {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).entered
    }

    #[must_use]
    pub fn blocked(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.entered && !state.released
    }

    /// Wait on the entry notification rather than spinning while AppKit starts
    /// the attempted frame. Entry alone is insufficient evidence; the harness
    /// also checks `blocked()` before AND after its main-loop acknowledgments.
    #[must_use]
    pub fn wait_entered(&self, budget: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let (state, _) = self
            .changed
            .wait_timeout_while(state, budget, |state| !state.entered)
            .unwrap_or_else(|p| p.into_inner());
        state.entered
    }

    /// Also cancels an arm not yet consumed, making teardown independent of the
    /// renderer ever requesting another frame.
    pub fn release(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.armed = false;
        state.released = true;
        self.changed.notify_all();
    }

    #[must_use]
    pub fn entries(&self) -> u64 {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).entries
    }

    /// Sticky: a later successful round cannot conceal an abandoned driver.
    #[must_use]
    pub fn timed_out(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .timed_out
    }

    fn before_acquire(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.armed {
            return;
        }
        state.armed = false;
        state.entered = true;
        state.entries += 1;
        self.changed.notify_all();
        let deadline = Instant::now() + ABANDONED_DRIVER_BACKSTOP;
        while !state.released {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.timed_out = true;
                state.released = true;
                self.changed.notify_all();
                break;
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|p| p.into_inner());
            state = next;
        }
    }
}

/// Install once per process. A duplicate installation fails rather than silently
/// observing a different latch from the native acquisition path.
#[must_use]
pub fn install_acquire_probe(probe: Arc<AcquireProbe>) -> bool {
    PROBE.set(probe).is_ok()
}

pub(crate) fn synchronous_control() -> bool {
    PROBE.get().is_some_and(|probe| probe.synchronous_control)
}

pub(crate) fn before_acquire() {
    if let Some(probe) = PROBE.get() {
        probe.before_acquire();
    }
}
