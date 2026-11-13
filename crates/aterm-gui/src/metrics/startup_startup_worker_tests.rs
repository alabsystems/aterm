// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{StartupGpuSample, StartupWorkerLegs, close_startup_gpu, derive_startup_worker};
use std::time::{Duration, Instant};

/// Synthetic 30 ms worker leg sample for tests.
pub(super) fn legs() -> StartupWorkerLegs {
    StartupWorkerLegs {
        prelude_ns: 1_000_000,
        gpu_build_ns: 12_000_000,
        font_admit_ns: 2_000_000,
        font_apply_ns: 1_000_000,
        font_seal_ns: 14_000_000,
    }
}

#[test]
fn a_worker_still_running_at_the_join_splits_both_intervals_exactly() {
    let spawn = Instant::now();
    let join_entry = spawn + Duration::from_millis(20);
    let done = join_entry + Duration::from_millis(10);
    let join_exit = done + Duration::from_millis(1);
    let sample = derive_startup_worker(
        Some(spawn),
        Some(done),
        Some(legs()),
        Some(join_entry),
        Some(join_exit),
    );
    assert!(sample.valid);
    assert_eq!(sample.total_ns, 30_000_000);
    assert_eq!(sample.overlap_ns, 20_000_000);
    assert_eq!(sample.after_join_ns, 10_000_000);
    assert_eq!(sample.post_join_ns, 1_000_000);
    assert_eq!(sample.overlap_ns + sample.after_join_ns, sample.total_ns);
    assert_eq!(sample.after_join_ns + sample.post_join_ns, 11_000_000);
    assert_eq!(sample.epilogue_ns, 0);
}

#[test]
fn a_worker_finished_before_the_join_costs_the_join_nothing() {
    let spawn = Instant::now();
    let done = spawn + Duration::from_millis(30);
    let join_entry = done + Duration::from_millis(5);
    let join_exit = join_entry + Duration::from_micros(10);
    let sample = derive_startup_worker(
        Some(spawn),
        Some(done),
        Some(legs()),
        Some(join_entry),
        Some(join_exit),
    );
    assert!(sample.valid);
    assert_eq!(sample.after_join_ns, 0);
    assert_eq!(sample.overlap_ns, sample.total_ns);
    assert_eq!(sample.post_join_ns, 10_000);
}

#[test]
fn a_missing_stamp_or_leg_transaction_publishes_nothing() {
    let spawn = Instant::now();
    let join_entry = spawn + Duration::from_millis(20);
    let done = join_entry + Duration::from_millis(10);
    let join_exit = done + Duration::from_millis(1);
    for (spawn, done, legs, entry, exit) in [
        (
            None,
            Some(done),
            Some(legs()),
            Some(join_entry),
            Some(join_exit),
        ),
        (
            Some(spawn),
            None,
            Some(legs()),
            Some(join_entry),
            Some(join_exit),
        ),
        (
            Some(spawn),
            Some(done),
            None,
            Some(join_entry),
            Some(join_exit),
        ),
        (Some(spawn), Some(done), Some(legs()), None, Some(join_exit)),
        (
            Some(spawn),
            Some(done),
            Some(legs()),
            Some(join_entry),
            None,
        ),
    ] {
        let sample = derive_startup_worker(spawn, done, legs, entry, exit);
        assert!(!sample.valid);
        assert_eq!(sample.total_ns, 0);
    }
}

#[test]
fn legs_that_overrun_the_worker_refuse_to_publish_a_partition() {
    let spawn = Instant::now();
    let join_entry = spawn + Duration::from_millis(1);
    let done = join_entry + Duration::from_millis(1);
    let join_exit = done + Duration::from_millis(1);
    let sample = derive_startup_worker(
        Some(spawn),
        Some(done),
        Some(legs()),
        Some(join_entry),
        Some(join_exit),
    );
    assert!(!sample.valid);
}

/// Synthetic 12 ms GPU probe sample for tests.
pub(super) fn probe() -> StartupGpuSample {
    StartupGpuSample {
        valid: true,
        context_ns: 9_400_000,
        font_thread_ns: 5_400_000,
        font_join_ns: 10_000,
        assemble_ns: 2_500_000,
        tail_ns: 0,
    }
}

#[test]
fn the_gpu_tail_absorbs_whatever_the_probe_did_not_measure() {
    let exclusive_ns = 9_400_000 + 10_000 + 2_500_000;
    let sample = close_startup_gpu(probe(), exclusive_ns + 80_000);
    assert!(sample.valid);
    assert_eq!(sample.tail_ns, 80_000);
    assert_eq!(sample.font_thread_ns, 5_400_000);
}

#[test]
fn an_unset_probe_is_not_a_zero_length_gpu_build() {
    assert!(!close_startup_gpu(StartupGpuSample::default(), 12_000_000).valid);
    let mut partial = probe();
    partial.context_ns = 0;
    assert!(!close_startup_gpu(partial, 12_000_000).valid);
}

#[test]
fn legs_that_do_not_fit_their_parent_refuse_to_publish() {
    assert!(!close_startup_gpu(probe(), 1_000_000).valid);
}
