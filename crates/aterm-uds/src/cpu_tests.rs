// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicU64, Ordering};

use super::ThreadCpu;

/// A meter's pieces sum to the last reading: each add is what was spent since the one before,
/// the same reading adds nothing, and a reading behind the last (a refused clock) adds nothing
/// and does not move the meter back.
///
/// Negative control: add `now_ns` whole in `add_at` (the total reads 17 after the second add,
/// where 12 is wanted).
#[test]
fn a_meter_adds_each_nanosecond_once() {
    let total = AtomicU64::new(0);
    let mut meter = ThreadCpu::born();
    meter.add_at(5, &total);
    assert_eq!(total.load(Ordering::Relaxed), 5);
    meter.add_at(12, &total);
    assert_eq!(total.load(Ordering::Relaxed), 12);
    meter.add_at(12, &total);
    assert_eq!(total.load(Ordering::Relaxed), 12);
    meter.add_at(0, &total);
    assert_eq!(total.load(Ordering::Relaxed), 12);
    meter.add_at(20, &total);
    assert_eq!(total.load(Ordering::Relaxed), 20);
}

/// The clocks only rise: a later reading of either, on one thread, is never behind an earlier
/// one.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_clocks_do_not_run_back() {
    let first = super::thread_ns();
    let second = super::thread_ns();
    assert!(second >= first, "{first} then {second}");
    let process = super::process_ns();
    let again = super::process_ns();
    assert!(again >= process, "{process} then {again}");
}

/// Each thread that runs its work through [`super::ThreadsCpu::ending`] is counted once, when
/// the work returned, and the work's value comes back.
///
/// Negative control: delete the `ended` add in `ending` (reads 0, wants 3).
#[test]
fn each_thread_that_ended_is_counted_once() {
    let threads = super::ThreadsCpu::new();
    let sum: u64 = std::thread::scope(|scope| {
        let workers: Vec<_> = (1..=3_u64)
            .map(|n| {
                let threads = &threads;
                scope.spawn(move || threads.ending(|| n))
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).sum()
    });
    assert_eq!(sum, 6);
    assert_eq!(threads.ended.load(Ordering::Relaxed), 3);
}
