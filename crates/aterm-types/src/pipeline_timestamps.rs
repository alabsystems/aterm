// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Per-frame pipeline timing for keystroke-to-pixel decomposition (#5560).
//!
//! Stores durations measured at stage boundaries in the Rust processing
//! pipeline. Swift correlates these with its own absolute timestamps
//! (NSEvent → presentedTime) to produce full end-to-end latency breakdown.

/// Per-frame Rust-side pipeline timing in nanoseconds.
///
/// Durations are measured per `process()` call via `Instant::now()` deltas.
/// The render snapshot captures its own timing. Swift correlates both with
/// its endpoint timestamps to produce full keystroke-to-pixel decomposition.
///
/// All durations are nanoseconds. `process_sequence` increments on each
/// `process()` call so Swift can detect stale timestamps (render without
/// new input).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineTimestamps {
    /// Duration of `advance_fast()` — SIMD parser (ns).
    pub parse_duration_ns: u64,
    /// Duration of `post_process()` — grid mutation, damage marking (ns).
    pub grid_duration_ns: u64,
    /// Total `process()` duration including routing overhead (ns).
    pub process_total_ns: u64,
    /// Duration of `copy_visible_cells_inner()` in render snapshot (ns).
    pub snapshot_copy_ns: u64,
    /// Duration of `get_damage_inner()` in render snapshot (ns).
    pub snapshot_damage_ns: u64,
    /// Total render snapshot duration under read lock (ns).
    pub snapshot_total_ns: u64,
    /// Bytes processed in the last `process()` call.
    pub last_process_bytes: u32,
    /// Sequence counter — incremented on each `process()` call so Swift
    /// can detect whether timestamps are stale (render without new input).
    pub process_sequence: u32,
    /// When false, `process()` skips per-stage `Instant::now()` calls
    /// (6 syscalls per invocation) to maximize throughput. Enabled by
    /// `ATERM_PROFILING=1` or the profiling API. Part of Wave 1
    /// throughput optimization.
    pub profiling_enabled: bool,
}

impl Default for PipelineTimestamps {
    fn default() -> Self {
        Self {
            parse_duration_ns: 0,
            grid_duration_ns: 0,
            process_total_ns: 0,
            snapshot_copy_ns: 0,
            snapshot_damage_ns: 0,
            snapshot_total_ns: 0,
            last_process_bytes: 0,
            process_sequence: 0,
            profiling_enabled: cfg!(debug_assertions) || std::env::var("ATERM_PROFILING").is_ok(),
        }
    }
}
