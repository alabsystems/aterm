// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Per-frame pipeline timing for keystroke-to-pixel decomposition.

use std::time::Duration;

/// Per-frame pipeline timing, in nanoseconds, from `Instant::now()` deltas per `process()` call.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineTimestamps {
    /// Duration of `advance_fast()` — SIMD parser (ns).
    pub parse_duration_ns: u64,
    /// Duration of `post_process()` — grid mutation, damage marking (ns).
    pub grid_duration_ns: u64,
    /// Total `process()` duration including routing overhead (ns).
    pub process_total_ns: u64,
    /// Bytes processed in the last `process()` call.
    pub last_process_bytes: u32,
    /// Incremented on each `process()` call so renderers can detect frames with no new input.
    pub process_sequence: u32,
    /// When false, `process()` skips its per-stage `Instant::now()` calls.
    pub profiling_enabled: bool,
}

/// One profiled batch's stage durations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchStages {
    /// The parser's.
    pub parse: Duration,
    /// The grid's: `post_process()` through the batch's `observe_at`.
    pub grid: Duration,
    /// The timed span's, parse and grid together (the batch's prologue and its tail after the
    /// grid are not timed).
    pub total: Duration,
}

impl PipelineTimestamps {
    /// One `process()` batch of `bytes` ended: it is counted and its size kept whether or not it
    /// was profiled, and only a profiled batch's `stages` replace the last durations. Every batch
    /// ends here, and a terminal reset keeps the count, so a renderer that waits on
    /// [`Self::process_sequence`] sees each one in every build.
    pub fn note_batch(&mut self, bytes: usize, stages: Option<BatchStages>) {
        if let Some(stages) = stages {
            self.parse_duration_ns = crate::duration_to_nanos(stages.parse);
            self.grid_duration_ns = crate::duration_to_nanos(stages.grid);
            self.process_total_ns = crate::duration_to_nanos(stages.total);
        }
        self.last_process_bytes = u32::try_from(bytes).unwrap_or(u32::MAX);
        self.process_sequence = self.process_sequence.wrapping_add(1);
    }
}

#[allow(clippy::derivable_impls)]
impl Default for PipelineTimestamps {
    fn default() -> Self {
        Self {
            parse_duration_ns: 0,
            grid_duration_ns: 0,
            process_total_ns: 0,
            last_process_bytes: 0,
            process_sequence: 0,
            profiling_enabled: cfg!(debug_assertions),
        }
    }
}

#[cfg(test)]
mod tests {
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
}
