// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The run queue the over rule reads (stats deck A4 §2, §3): P10, the 10 s mean of the machine's
//! runnable threads (every user's, running or waiting for a core), where the host reads it, and
//! the 1-minute load average only where it does not.
//!
//! M-QUEUE (`docs/measured/stats-deck-a4-n1-2026-11-06/queue.md`) chose P10: the own-user wait
//! (`cpu_mc + wait_mc`) sat more than 20 % under the machine's count in 9.5–16.4 % of readings,
//! the per-cluster count needs root, and load1 holds 65 % of a burst's step 30 s after it ends.
//! The dial ([`crate::strain::over`]), the CPU row's over caps and the queue tile
//! ([`crate::units::cpu_over`]) and the CPU cause pointer ([`crate::strain::cpu_score`]) read the
//! one [`RunQueue`] this module makes, through the one rule [`crate::strain::cpu_over_depth`].

use crate::model::Unread;
use crate::ring::{BucketRing, Taken};
use crate::units::Level;

/// What the host read of the run queue at one reading (`aterm_sysprobe::runq::RunQueue`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunQueueRead {
    /// Runnable threads, machine-wide, in thousandths.
    Milli(u64),
    /// The read failed.
    Failed,
    /// The kernel's answer failed its self-check.
    UnreadFormat,
    /// No reader on this OS.
    #[default]
    Unsupported,
}

impl RunQueueRead {
    /// The runnable threads, where read.
    #[must_use]
    pub fn threads(self) -> Option<f64> {
        match self {
            Self::Milli(m) => Some(m as f64 / 1000.0),
            Self::Failed | Self::UnreadFormat | Self::Unsupported => None,
        }
    }

    /// Why it holds no count, where it holds none.
    #[must_use]
    pub const fn unread(self) -> Option<Unread> {
        match self {
            Self::Milli(_) => None,
            Self::Failed => Some(Unread::NotSampled),
            Self::UnreadFormat => Some(Unread::UnreadFormat),
            Self::Unsupported => Some(Unread::Unsupported),
        }
    }
}

/// P10: the machine's runnable threads over the [`crate::ring::NOW_WINDOW_MS`] window, or why
/// there is none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Runnable {
    /// The 10 s mean and max of the readings in the window.
    Read(Level),
    /// No reading in the window, and why: the newest reading's failure, or none yet.
    Unread(Unread),
}

impl Default for Runnable {
    fn default() -> Self {
        Self::Unread(Unread::NotSampled)
    }
}

impl Runnable {
    /// The 10 s mean, where read.
    #[must_use]
    pub const fn mean(self) -> Option<f64> {
        match self {
            Self::Read(l) => Some(l.mean),
            Self::Unread(_) => None,
        }
    }
}

/// Which reading a [`RunQueue`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueBasis {
    /// P10: the 10 s mean of the machine's runnable threads.
    Runnable,
    /// The 1-minute load average, read only where P10 is unread: it lags the work by about a
    /// minute.
    Load1,
}

impl QueueBasis {
    /// The word `sys` prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Runnable => "runnable_10s",
            Self::Load1 => "load1",
        }
    }
}

/// The queue the over rule reads: threads that want a core, against the cores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunQueue {
    /// Runnable threads (P10), or the 1-minute load average where P10 is unread.
    pub threads: f64,
    /// Which of the two it is.
    pub basis: QueueBasis,
}

impl RunQueue {
    /// P10 where read, else `load1`; `None` where neither is a number.
    #[must_use]
    pub fn of(runnable: Runnable, load1: Option<f64>) -> Option<Self> {
        let read = |v: Option<f64>| v.filter(|x| x.is_finite() && *x >= 0.0);
        read(runnable.mean())
            .map(|threads| Self {
                threads,
                basis: QueueBasis::Runnable,
            })
            .or_else(|| {
                read(load1).map(|threads| Self {
                    threads,
                    basis: QueueBasis::Load1,
                })
            })
    }
}

/// The run queue's 10 s window: the readings of the last minute, as a unit's ring keeps them.
#[derive(Debug)]
pub(crate) struct RunQueueRing {
    ring: BucketRing,
    /// Why the newest reading held no count, while it held none.
    unread: Unread,
}

impl Default for RunQueueRing {
    fn default() -> Self {
        Self {
            ring: BucketRing::without_long(4),
            unread: Unread::NotSampled,
        }
    }
}

impl RunQueueRing {
    /// Takes one reading, taken as `t` says, and answers P10 at it.
    pub(crate) fn absorb(&mut self, t: Taken, read: RunQueueRead) -> Runnable {
        let _ = self.ring.push(t, read.threads(), false);
        if let Some(why) = read.unread() {
            self.unread = why;
        }
        Level::of(&self.ring, t.at_ms).map_or(Runnable::Unread(self.unread), Runnable::Read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_792_000_800_000;

    #[test]
    fn p10_is_the_ten_second_mean_of_the_readings() {
        let mut r = RunQueueRing::default();
        let mut last = Runnable::default();
        for (i, milli) in [4_000, 8_000, 12_000, 30_000].into_iter().enumerate() {
            let at = T0 + i64::try_from(i).expect("few") * 1000;
            last = r.absorb(Taken::at(at, 1000), RunQueueRead::Milli(milli));
        }
        let Runnable::Read(l) = last else {
            panic!("{last:?}")
        };
        assert!((l.mean - 13.5).abs() < 1e-9, "{l:?}");
        assert_eq!(l.max, 30.0);
        // Ten seconds on with nothing read, the window is empty: the failure, not a zero.
        let gone = r.absorb(Taken::at(T0 + 15_000, 1000), RunQueueRead::Failed);
        assert_eq!(gone, Runnable::Unread(Unread::NotSampled));
        assert_eq!(gone.mean(), None);
    }

    #[test]
    fn the_queue_is_p10_and_load1_only_where_p10_is_unread() {
        let read = Runnable::Read(Level {
            mean: 26.8,
            max: 31.0,
        });
        assert_eq!(
            RunQueue::of(read, Some(16.59)),
            Some(RunQueue {
                threads: 26.8,
                basis: QueueBasis::Runnable
            })
        );
        for why in [
            Unread::Unsupported,
            Unread::UnreadFormat,
            Unread::NotSampled,
        ] {
            assert_eq!(
                RunQueue::of(Runnable::Unread(why), Some(16.59)),
                Some(RunQueue {
                    threads: 16.59,
                    basis: QueueBasis::Load1
                })
            );
        }
        assert_eq!(RunQueue::of(Runnable::default(), None), None);
        assert_eq!(RunQueue::of(Runnable::default(), Some(f64::NAN)), None);
    }

    #[test]
    fn an_unsupported_or_refused_reader_says_why_from_its_first_reading() {
        let mut r = RunQueueRing::default();
        assert_eq!(
            r.absorb(Taken::at(T0, 1000), RunQueueRead::Unsupported),
            Runnable::Unread(Unread::Unsupported)
        );
        let mut r = RunQueueRing::default();
        assert_eq!(
            r.absorb(Taken::at(T0, 1000), RunQueueRead::UnreadFormat),
            Runnable::Unread(Unread::UnreadFormat)
        );
    }
}
