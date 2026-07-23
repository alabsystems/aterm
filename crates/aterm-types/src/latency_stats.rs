// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Latency stats contract types shared across FFI crates (#5465).
//!
//! The mutable process-global callback registry lives in
//! `aterm-runtime-callbacks::latency_stats`. This module provides only the
//! data struct, error enum, and callback type alias so that FFI signatures
//! can reference them without pulling in runtime state.

use std::ffi::c_void;

/// Latency statistics returned from the native UI layer.
///
/// All latency values are in milliseconds. Count is the number of samples
/// in the collector's circular buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LatencyStats {
    /// Input-to-display latency 50th percentile (ms).
    pub input_latency_p50_ms: f64,
    /// Input-to-display latency 95th percentile (ms).
    pub input_latency_p95_ms: f64,
    /// Input-to-display latency 99th percentile (ms).
    pub input_latency_p99_ms: f64,
    /// Number of input latency samples.
    pub input_latency_count: u32,

    /// GPU frame time 50th percentile (ms).
    pub gpu_frame_time_p50_ms: f64,
    /// GPU frame time 95th percentile (ms).
    pub gpu_frame_time_p95_ms: f64,
    /// GPU frame time 99th percentile (ms).
    pub gpu_frame_time_p99_ms: f64,
    /// Number of GPU frame time samples.
    pub gpu_frame_time_count: u32,

    // --- Rust pipeline stage durations (#5560 Phase 4) ---
    /// Rust `process()` total duration 50th percentile (ms).
    pub rust_process_p50_ms: f64,
    /// Rust `process()` total duration 95th percentile (ms).
    pub rust_process_p95_ms: f64,
    /// Rust `process()` total duration 99th percentile (ms).
    pub rust_process_p99_ms: f64,
    /// Number of Rust process duration samples.
    pub rust_process_count: u32,

    /// Rust render snapshot total duration 50th percentile (ms).
    pub rust_snapshot_p50_ms: f64,
    /// Rust render snapshot total duration 95th percentile (ms).
    pub rust_snapshot_p95_ms: f64,
    /// Rust render snapshot total duration 99th percentile (ms).
    pub rust_snapshot_p99_ms: f64,
    /// Number of Rust snapshot duration samples.
    pub rust_snapshot_count: u32,

    /// Latest SIMD parser duration from the most recent `process()` call (ms).
    pub latest_parse_ms: f64,
    /// Latest grid mutation / `post_process()` duration (ms).
    pub latest_grid_ms: f64,
    /// Latest total `process()` duration including routing overhead (ms).
    pub latest_process_total_ms: f64,
    /// Latest cell copy duration in render snapshot (ms).
    pub latest_snapshot_copy_ms: f64,
    /// Latest damage calculation duration in render snapshot (ms).
    pub latest_snapshot_damage_ms: f64,
    /// Latest total render snapshot duration under read lock (ms).
    pub latest_snapshot_total_ms: f64,

    // --- CPU-side vertex build durations (#5605 Phase 3) ---
    /// CPU-side vertex build 50th percentile (ms).
    pub vertex_build_p50_ms: f64,
    /// CPU-side vertex build 95th percentile (ms).
    pub vertex_build_p95_ms: f64,
    /// CPU-side vertex build 99th percentile (ms).
    pub vertex_build_p99_ms: f64,
    /// Number of vertex build samples.
    pub vertex_build_count: u32,

    // --- Metal command encoding durations (#5605) ---
    /// Metal command encoding 50th percentile (ms).
    pub encoding_p50_ms: f64,
    /// Metal command encoding 95th percentile (ms).
    pub encoding_p95_ms: f64,
    /// Metal command encoding 99th percentile (ms).
    pub encoding_p99_ms: f64,
    /// Number of encoding duration samples.
    pub encoding_count: u32,

    // --- Total draw duration (#5605) ---
    /// Total draw(in:) to commit() 50th percentile (ms).
    pub draw_duration_p50_ms: f64,
    /// Total draw(in:) to commit() 95th percentile (ms).
    pub draw_duration_p95_ms: f64,
    /// Total draw(in:) to commit() 99th percentile (ms).
    pub draw_duration_p99_ms: f64,
    /// Number of draw duration samples.
    pub draw_duration_count: u32,
}

/// Error returned when latency stats cannot be retrieved.
#[non_exhaustive]
#[derive(Debug, aterm_error::Error)]
pub enum LatencyStatsError {
    /// No callback has been registered (renderer not yet initialized).
    #[error("no latency stats callback registered")]
    NoCallback,
    /// The native callback returned a non-zero error code.
    #[error("latency stats callback returned error code {0}")]
    CallbackFailed(i32),
}

/// Callback type for latency stats (C ABI).
///
/// Called from Rust, implemented in the native UI (Swift Metal renderer).
/// Writes stats to the provided output pointer. Returns 0 on success.
pub type LatencyStatsCallback =
    unsafe extern "C" fn(context: *mut c_void, out_stats: *mut LatencyStats) -> i32;
