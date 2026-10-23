// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Per-frame pipeline timing for keystroke-to-pixel decomposition.

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
