// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
