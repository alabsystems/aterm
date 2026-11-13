// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Startup of the vault subscriber's mend and join-notice workers, and of the converge pass's
//! window lane beside them.

use std::sync::atomic::{AtomicBool, Ordering};

/// The process's independent once-only starts of its two vault workers and its converge lane.
pub(crate) struct Lanes {
    mending: AtomicBool,
    asking: AtomicBool,
    converging: AtomicBool,
}

impl Lanes {
    pub(crate) const fn new() -> Self {
        Self {
            mending: AtomicBool::new(false),
            asking: AtomicBool::new(false),
            converging: AtomicBool::new(false),
        }
    }

    /// An active request starts each worker once its spawner accepts it. A headless or
    /// unsupported-platform request (`inactive`) leaves both starts available.
    pub(crate) fn start(
        &self,
        inactive: bool,
        mend: impl FnOnce() -> bool,
        asking: impl FnOnce() -> bool,
    ) {
        if !inactive {
            Self::start_one(&self.mending, mend);
            Self::start_one(&self.asking, asking);
        }
    }

    /// The converge pass's window lane, started once its spawner accepts it, on its own start: a
    /// launch that runs no first-run setup starts it beside the mend, and a setup's run starts it
    /// once it ends, not before (the mend starts before a run too). An `inactive` request leaves
    /// it available.
    pub(crate) fn start_converge(&self, inactive: bool, converge: impl FnOnce() -> bool) {
        if !inactive {
            Self::start_one(&self.converging, converge);
        }
    }

    /// Claim before calling the spawner so overlapping requests coalesce. A refused start
    /// remains available; a worker that started and later ends keeps its once-per-process bound.
    fn start_one(started: &AtomicBool, spawn: impl FnOnce() -> bool) {
        if started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && !spawn()
        {
            started.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
#[path = "secrets_lanes_tests.rs"]
mod tests;
