// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Per-frame pipeline timing for keystroke-to-pixel decomposition.
//!
//! Stores durations measured at stage boundaries in the Rust processing
//! pipeline.

/// Per-frame Rust-side pipeline timing in nanoseconds.
///
/// Durations are measured per `process()` call via `Instant::now()` deltas.
///
/// All durations are nanoseconds. `process_sequence` increments on each
/// `process()` call; the renderers (aterm-effects, aterm-gui) compare it to
/// detect a frame with no new input.
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
    /// Sequence counter — incremented on each `process()` call so a renderer
    /// can detect a frame with no new input.
    pub process_sequence: u32,
    /// When false, `process()` skips its six per-stage `Instant::now()` calls.
    ///
    /// TRUE ONLY IN A DEBUG BUILD, never from the environment: the profiled
    /// arm is a second copy of the hot `process()` loop
    /// (`aterm-core/src/terminal/processing.rs`) that no release test covers.
    pub profiling_enabled: bool,
}

// NOT derivable, despite how it reads in a release build: `profiling_enabled`
// defaults to `cfg!(debug_assertions)`, so the derive's `false` would silently
// turn profiling off in debug builds. Clippy evaluates the macro under the
// profile it lints in, which is why the release lane calls this derivable.
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
