// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine's run queue: how many threads are runnable (running, or waiting for a core),
//! every user's. The deck's over rule reads its 10 s mean, P10 (stats deck A4 §2, §3;
//! M-QUEUE, `docs/measured/stats-deck-a4-n1-2026-11-06/queue.md`), in place of the 1-minute load
//! average, which lags the work by about a minute.
//!
//! It comes with the task tables ([`crate::task_room`]), from the read that already counts the
//! machine's threads, so it costs no read of its own:
//!
//! - **macOS**: the default processor set's `load_average` from the same
//!   `processor_set_statistics(PROCESSOR_SET_LOAD_INFO)` that gives its `thread_count`: xnu's
//!   `sched_load_average`, which each scheduler tick (113 ms p50 on the M5, N1) steps to
//!   `(4L + 1000·n) / 5` with `n` the runnable threads, so it is a τ ≈ 0.5 s average of the
//!   count in thousandths. It needs no privilege, unlike the per-cluster count. Each reading holds
//!   the set's mach factor beside it ([`pset_holds`]): a pair no kernel could have stepped reads
//!   as unread, never as a count.
//! - **Linux**: `/proc/loadavg`'s running count, from the same read that gives its task count;
//!   the reading thread counts itself there, and is taken off.
//! - **Elsewhere**: no reader ([`RunQueue::Unsupported`]).

/// One reading of the run queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunQueue {
    /// Runnable threads, machine-wide, in thousandths: macOS's τ ≈ 0.5 s kernel average, Linux's
    /// count now.
    Milli(u64),
    /// The read failed (the call, or a line the read did not hold whole).
    Failed,
    /// The kernel answered with values that fail their self-check ([`pset_holds`]).
    UnreadFormat,
    /// No reader on this OS.
    #[default]
    Unsupported,
}

impl RunQueue {
    /// The runnable threads, where read.
    #[must_use]
    pub fn threads(self) -> Option<f64> {
        match self {
            Self::Milli(m) => Some(m as f64 / 1000.0),
            Self::Failed | Self::UnreadFormat | Self::Unsupported => None,
        }
    }
}

/// How far `L + M` may fall under `1000·k` and still be one kernel's pair ([`pset_holds`]):
/// each tick's two truncating divisions lose under 2, so `D' > 4D/5 − 2`, whose fixed point is
/// −10.
pub const PSET_SLACK: i64 = 10;

/// Whether a processor set's `load_average` `load` and `mach_factor` `factor`, both in
/// thousandths, can be one kernel's averages over at most `cores` processors.
///
/// Each scheduler tick xnu's `compute_averages` steps both with one count `n` of runnable threads
/// and its available processors `k`: `L' = (4L + 1000n) / 5` and `M' = (4M + f(n)) / 5`, with
/// `f(n) = (k − n)·1000` up to the processors and `k·1000 / (n + 1)` past them. Since
/// `(k − n)·1000 ≤ f(n) ≤ k·1000` for every `n`, `L + M` never falls more than [`PSET_SLACK`]
/// under `1000·k` and `M` never passes `1000·k`. So a pair passes when some `k` in `1..=cores`
/// explains it: `M ≤ 1000·k ≤ L + M + PSET_SLACK`. A read of other fields (the set's task and
/// thread counts), a kernel that stopped stepping one of them, a factor past every core and two
/// zeros are refused. A pair at a moment the processors changed may be refused for the second
/// it takes the averages to settle; that reads unread, as it should.
#[must_use]
pub fn pset_holds(load: i64, factor: i64, cores: u64) -> bool {
    let Ok(cores) = i64::try_from(cores) else {
        return false;
    };
    if load < 0 || factor < 0 || cores == 0 {
        return false;
    }
    let k = (factor + 999).div_euclid(1000).max(1);
    k <= cores && 1000 * k <= load + factor + PSET_SLACK
}

/// [`RunQueue`] of a processor set's `load_average` and `mach_factor` on a machine of `cores`
/// logical cores.
#[must_use]
pub fn of_pset(load: i64, factor: i64, cores: u64) -> RunQueue {
    match u64::try_from(load) {
        Ok(milli) if pset_holds(load, factor, cores) => RunQueue::Milli(milli),
        _ => RunQueue::UnreadFormat,
    }
}

/// [`RunQueue`] of a runnable count that includes the thread reading it (`/proc/loadavg`'s).
#[must_use]
pub fn of_running(running: Option<u64>) -> RunQueue {
    running.map_or(RunQueue::Failed, |n| {
        RunQueue::Milli(n.saturating_sub(1).saturating_mul(1000))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One scheduler tick of xnu's `compute_averages` over `k` available processors with `n`
    /// runnable threads.
    fn tick(l: i64, m: i64, n: i64, k: i64) -> (i64, i64) {
        let f = if n > k {
            k * 1000 / (n + 1)
        } else {
            (k - n) * 1000
        };
        ((4 * l + 1000 * n) / 5, (4 * m + f) / 5)
    }

    /// A kernel's pairs over `ticks` ticks of a swinging queue on `k` processors, from boot (both
    /// zero), the first 64 ticks (7 s) left out while `L + M` climbs from 0 to `1000·k`: it closes
    /// a fifth of the gap a tick, so 128 processors' 128 000 are within 1 by tick 53.
    fn trace(k: i64, ticks: usize) -> Vec<(i64, i64)> {
        let (mut l, mut m) = (0, 0);
        let mut out = Vec::new();
        let span = u64::try_from(4 * k + 1).expect("positive");
        for i in 0..ticks {
            // 0 to 4k runnable, moving every tick: the desk's swings and bursts.
            let i64_of = |v: u64| i64::try_from(v).expect("small");
            let at = u64::try_from(i).expect("small");
            let n = i64_of((at * 7919 + at * at * 31) % span);
            (l, m) = tick(l, m, n, k);
            if i >= 64 {
                out.push((l, m));
            }
        }
        out
    }

    #[test]
    fn every_pair_a_kernel_steps_holds() {
        for k in [1, 4, 10, 18, 128] {
            let cores = u64::try_from(k).expect("small");
            let t = trace(k, 4000);
            assert!(t.iter().all(|&(l, m)| pset_holds(l, m, cores)), "k {k}");
            // Fewer processors available than cores (some withdrawn) still holds.
            assert!(t.iter().all(|&(l, m)| pset_holds(l, m, cores + 2)), "k {k}");
            for &(l, m) in &t {
                let n = of_pset(l, m, cores).threads().expect("read");
                assert!((0.0..=4.0 * k as f64).contains(&n), "{n} on {k}");
            }
        }
    }

    /// An idle machine (no thread runnable) settles where truncation stops the factor short of
    /// `1000·k` (9996 on 10 processors): within [`PSET_SLACK`], so it reads 0 runnable, not
    /// unread. CONTROL: with no slack the settled pair is refused.
    #[test]
    fn an_idle_kernels_pairs_hold() {
        for k in [4, 10, 18] {
            let cores = u64::try_from(k).expect("small");
            let (mut l, mut m) = (0, 0);
            let mut settled = Vec::new();
            for i in 0..400 {
                (l, m) = tick(l, m, 0, k);
                if i >= 64 {
                    settled.push((l, m));
                }
            }
            assert!(
                settled.iter().all(|&(l, m)| pset_holds(l, m, cores)),
                "k {k}"
            );
            assert_eq!(of_pset(l, m, cores).threads(), Some(0.0), "k {k}");
            assert!(l + m < 1000 * k, "control: it settles short, at {}", l + m);
        }
    }

    #[test]
    fn a_pair_no_kernel_could_step_reads_unread() {
        // Negative controls, each a misread the self-check must refuse.
        let t = trace(10, 4000);
        // The set's task and thread counts read as the averages (a layout one slot off).
        assert_eq!(of_pset(812, 4103, 10), RunQueue::UnreadFormat);
        // A load average that stopped stepping (stuck at zero) beside a live factor: most refused.
        let stuck = t.iter().filter(|&&(_, m)| pset_holds(0, m, 10)).count();
        assert!(stuck * 10 < t.len(), "{stuck} of {} pass", t.len());
        // Both stuck at zero: the fake zero is no reading.
        assert_eq!(of_pset(0, 0, 10), RunQueue::UnreadFormat);
        assert_eq!(of_pset(0, 0, 10).threads(), None);
        // A factor past every core.
        assert!(!pset_holds(30_000, 10_500, 10));
        // The two read the wrong way round under a queue past the cores.
        assert!(!pset_holds(1_200, 26_800, 10));
        assert!(!pset_holds(-1, 5_000, 10));
        assert!(!pset_holds(5_000, 5_000, 0));
    }

    #[test]
    fn a_count_that_holds_its_reader_takes_it_off() {
        assert_eq!(of_running(Some(27)).threads(), Some(26.0));
        assert_eq!(of_running(Some(0)).threads(), Some(0.0));
        assert_eq!(of_running(None), RunQueue::Failed);
        assert_eq!(
            RunQueue::default(),
            RunQueue::Unsupported,
            "no reader says so"
        );
        assert_eq!(RunQueue::Unsupported.threads(), None);
    }
}
