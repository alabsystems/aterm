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
mod tests {
    use super::*;

    const D: Daemon = Daemon {
        pid: 690,
        start_s: 1_791_000_000,
        start_us: 1,
    };
    const MIN: u64 = 60_000;

    fn reading(daemon: Daemon, workers: u64, at_limit: bool) -> Reading {
        Reading {
            daemon,
            workers,
            at_limit,
            limit: Some(90),
        }
    }

    fn run(readings: &[(u64, Option<Reading>)]) -> Vec<State> {
        let mut judge = Judge::new();
        readings
            .iter()
            .map(|(at, r)| judge.absorb(*at, r.as_ref()))
            .collect()
    }

    #[test]
    fn a_pool_full_for_five_minutes_is_stuck_and_one_below_it_is_not() {
        let full = Some(reading(D, 95, true));
        let states = run(&[
            (0, Some(reading(D, 6, false))),
            (MIN, full),
            (2 * MIN, full),
            (6 * MIN, full),
            (7 * MIN, full),
        ]);
        assert_eq!(
            states,
            [
                State::Answering,
                State::Full { since_ms: MIN },
                State::Full { since_ms: MIN },
                State::Stuck { since_ms: MIN },
                State::Stuck { since_ms: MIN },
            ]
        );
        let busy = Some(reading(D, 40, false));
        let states = run(&[(MIN, busy), (2 * MIN, busy), (6 * MIN, busy)]);
        assert!(states.iter().all(|s| *s == State::Answering), "{states:?}");
    }

    #[test]
    fn a_full_pool_that_drains_starts_over() {
        let full = Some(reading(D, 90, true));
        let states = run(&[
            (0, full),
            (4 * MIN, full),
            (5 * MIN - 1, Some(reading(D, 3, false))),
            (6 * MIN, full),
            (10 * MIN, full),
        ]);
        assert_eq!(
            states,
            [
                State::Full { since_ms: 0 },
                State::Full { since_ms: 0 },
                State::Answering,
                State::Full { since_ms: 6 * MIN },
                State::Full { since_ms: 6 * MIN },
            ]
        );
    }

    #[test]
    fn a_new_syspolicyd_answers_and_a_missing_reading_changes_nothing() {
        let full = Some(reading(D, 95, true));
        let again = Daemon {
            pid: 489,
            start_s: 1_792_597_850,
            start_us: 7,
        };
        let states = run(&[
            (0, full),
            (5 * MIN, full),
            (6 * MIN, None),
            (7 * MIN, Some(reading(again, 95, true))),
            (8 * MIN, Some(reading(again, 4, false))),
        ]);
        assert_eq!(
            states,
            [
                State::Full { since_ms: 0 },
                State::Stuck { since_ms: 0 },
                State::Stuck { since_ms: 0 },
                State::Full { since_ms: 7 * MIN },
                State::Answering,
            ]
        );
        assert_eq!(Judge::new().absorb(0, None), State::Unread);
        let mut judge = Judge::new();
        judge.absorb(0, full.as_ref());
        judge.absorb(5 * MIN, full.as_ref());
        assert_eq!(judge.absorb_none_running(), State::Answering);
        assert_eq!(
            judge.absorb(6 * MIN, full.as_ref()),
            State::Full { since_ms: 6 * MIN }
        );
    }

    #[test]
    fn the_kernel_flag_alone_reads_full() {
        let flagged = Reading {
            daemon: D,
            workers: 12,
            at_limit: true,
            limit: None,
        };
        assert!(flagged.full());
        assert!(
            !Reading {
                at_limit: false,
                ..flagged
            }
            .full()
        );
        assert!(!reading(D, 90, false).full());
        assert!(!reading(D, 100, false).full());
        assert!(reading(D, 95, true).full());
    }

    #[test]
    fn the_status_words() {
        assert_eq!(State::Unread.as_str(), "-");
        assert_eq!(State::Answering.as_str(), "ok");
        assert_eq!(State::Full { since_ms: 1 }.as_str(), "full");
        assert_eq!(State::Stuck { since_ms: 1 }.as_str(), "stuck");
        assert_eq!(State::Stuck { since_ms: 9 }.since_ms(), Some(9));
        assert_eq!(State::Answering.since_ms(), None);
    }
}
