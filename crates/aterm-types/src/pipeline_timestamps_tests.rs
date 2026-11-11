// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// Every batch is counted and its size kept; only a profiled one replaces the durations.
/// NEGATIVE CONTROL: with an unprofiled batch left uncounted, `` an unprofiled batch is
/// counted `` fails.
#[test]
fn every_batch_is_counted_and_only_a_profiled_one_times_its_stages() {
    let mut ts = PipelineTimestamps {
        profiling_enabled: false,
        ..PipelineTimestamps::default()
    };
    let stages = BatchStages {
        parse: Duration::from_nanos(5),
        grid: Duration::from_nanos(7),
        total: Duration::from_nanos(20),
    };
    ts.note_batch(7, Some(stages));
    ts.note_batch(1, None);
    assert_eq!(ts.process_sequence, 2, "an unprofiled batch is counted");
    assert_eq!(ts.last_process_bytes, 1, "the last batch's size");
    assert_eq!(
        (
            ts.parse_duration_ns,
            ts.grid_duration_ns,
            ts.process_total_ns
        ),
        (5, 7, 20),
        "an unprofiled batch keeps the last profiled durations"
    );
    ts.process_sequence = u32::MAX;
    // 2^32 + 1 on a 64-bit target, which a truncating cast would read as 1.
    let past = usize::try_from(u64::from(u32::MAX) + 2).unwrap_or(usize::MAX);
    ts.note_batch(past, None);
    assert_eq!(ts.process_sequence, 0, "the count wraps");
    assert_eq!(ts.last_process_bytes, u32::MAX, "a size past u32 saturates");
}
