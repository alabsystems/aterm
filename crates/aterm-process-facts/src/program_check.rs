// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observed daemon identity, thread-pool facts and the count-only program-check rule.

/// Identity of a `syspolicyd` process by PID and start timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Daemon {
    /// Its pid.
    pub pid: u32,
    /// When it started: unix seconds.
    pub start_s: u64,
    /// The microseconds within [`Self::start_s`].
    pub start_us: u32,
}

impl Daemon {
    /// When it started: microseconds since the epoch, the clock a process's fork time is read on
    /// (`aterm_sysprobe::jobs::StartFacts::start_us`); saturating.
    #[must_use]
    pub const fn started_us(&self) -> u64 {
        self.start_s
            .saturating_mul(1_000_000)
            .saturating_add(self.start_us as u64)
    }
}

/// Snapshot of `syspolicyd` workqueue thread-pool state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The daemon read.
    pub daemon: Daemon,
    /// Total workqueue threads of all kinds (`wq`).
    pub workers: u64,
    /// Whether the kernel flagged a workqueue limit (`wql` `C` or `T`).
    pub at_limit: bool,
    /// Constrained thread limit (`kern.wq_max_constrained_threads`), if read.
    pub limit: Option<u64>,
}

impl Reading {
    /// Returns whether the kernel flagged the workqueue pool as at its limit.
    #[must_use]
    pub const fn full(&self) -> bool {
        self.at_limit
    }
}

/// How many programs held at one reading make a backed-up program-check queue. A normal
/// exec reads held for under 0.3 ms, so three at once is a line. A reader that keeps process
/// identities can also recognize the same held process across readings.
pub const HELD_AT_ONCE: usize = 3;

/// The count-only backlog rule: enough programs held at once, or positive counts at both
/// readings. Counts cannot establish that the same program appeared in both readings.
#[must_use]
pub const fn held_count_backed_up(count: usize, before: Option<usize>) -> bool {
    count >= HELD_AT_ONCE || (count > 0 && matches!(before, Some(previous) if previous > 0))
}

#[cfg(test)]
#[path = "program_check_tests.rs"]
mod tests;
