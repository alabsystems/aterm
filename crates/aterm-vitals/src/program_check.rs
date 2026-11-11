// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tracks whether macOS `syspolicyd` is stuck at its workqueue thread limit.

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

/// Duration a `syspolicyd` pool must remain full before [`State::Stuck`] is reported.
pub const STUCK_AFTER_MS: u64 = 5 * 60_000;

/// Health state of the macOS program check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Not read yet (or not readable here).
    Unread,
    /// The last reading's pool was below its limit.
    Answering,
    /// The pool has read full since `since_ms` for less than [`STUCK_AFTER_MS`].
    Full {
        /// The first full reading of this run.
        since_ms: u64,
    },
    /// The pool has read full since `since_ms` for [`STUCK_AFTER_MS`] or more.
    Stuck {
        /// The first full reading of this run.
        since_ms: u64,
    },
}

impl State {
    /// The `status` word: `-`, `ok`, `full` or `stuck`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unread => "-",
            Self::Answering => "ok",
            Self::Full { .. } => "full",
            Self::Stuck { .. } => "stuck",
        }
    }

    /// When the pool first read full, for [`Self::Full`] and [`Self::Stuck`].
    #[must_use]
    pub const fn since_ms(self) -> Option<u64> {
        match self {
            Self::Full { since_ms } | Self::Stuck { since_ms } => Some(since_ms),
            Self::Unread | Self::Answering => None,
        }
    }
}

/// The readings so far, folded into one [`State`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Judge {
    state: State,
    daemon: Option<Daemon>,
}

impl Default for Judge {
    fn default() -> Self {
        Self::new()
    }
}

impl Judge {
    /// Nothing read.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Unread,
            daemon: None,
        }
    }

    /// The state the readings so far give.
    #[must_use]
    pub const fn state(&self) -> State {
        self.state
    }

    /// Folds in the reading taken at `at_ms` (`None` leaves the state unchanged).
    pub fn absorb(&mut self, at_ms: u64, reading: Option<&Reading>) -> State {
        let Some(reading) = reading else {
            return self.state;
        };
        let same = self.daemon == Some(reading.daemon);
        self.daemon = Some(reading.daemon);
        self.state = if !reading.full() {
            State::Answering
        } else {
            match self.state {
                State::Full { since_ms } | State::Stuck { since_ms } if same => {
                    if at_ms.saturating_sub(since_ms) >= STUCK_AFTER_MS {
                        State::Stuck { since_ms }
                    } else {
                        State::Full { since_ms }
                    }
                }
                _ => State::Full { since_ms: at_ms },
            }
        };
        self.state
    }

    /// Records that no `syspolicyd` process is running and returns [`State::Answering`].
    pub fn absorb_none_running(&mut self) -> State {
        self.daemon = None;
        self.state = State::Answering;
        self.state
    }
}

#[cfg(test)]
#[path = "program_check_tests.rs"]
mod tests;
