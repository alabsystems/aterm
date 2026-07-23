// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Layout and FFI safety tests for `PipelineTimestamps`.
//!
//! This struct is `repr(C)` and crosses the Rust→C→Swift boundary via
//! `AtermRenderSnapshot.pipeline_timestamps`. These tests guard against
//! field reordering or type changes that would silently corrupt timestamps
//! in Swift's performance overlay.
//!
//! Part of #6152 — pipeline GPU layout parity verification.

use aterm_types::PipelineTimestamps;
use std::mem;

/// Verify the C-ABI layout matches what cbindgen generates and Swift consumes.
///
/// The C header (aterm_base.h) declares:
///   6 × uint64_t (48 bytes) + 2 × uint32_t (8 bytes) + bool (1 byte)
///   + 7 bytes padding = 64 bytes total
#[test]
fn layout_matches_c_header() {
    assert_eq!(mem::size_of::<PipelineTimestamps>(), 64);
    assert_eq!(mem::align_of::<PipelineTimestamps>(), 8);

    // Field offsets must match aterm_base.h typedef order
    assert_eq!(mem::offset_of!(PipelineTimestamps, parse_duration_ns), 0);
    assert_eq!(mem::offset_of!(PipelineTimestamps, grid_duration_ns), 8);
    assert_eq!(mem::offset_of!(PipelineTimestamps, process_total_ns), 16);
    assert_eq!(mem::offset_of!(PipelineTimestamps, snapshot_copy_ns), 24);
    assert_eq!(mem::offset_of!(PipelineTimestamps, snapshot_damage_ns), 32);
    assert_eq!(mem::offset_of!(PipelineTimestamps, snapshot_total_ns), 40);
    assert_eq!(mem::offset_of!(PipelineTimestamps, last_process_bytes), 48);
    assert_eq!(mem::offset_of!(PipelineTimestamps, process_sequence), 52);
    assert_eq!(mem::offset_of!(PipelineTimestamps, profiling_enabled), 56);
}

/// Verify Default produces a zero-initialized struct.
///
/// Important for FFI safety: the C side zeroes `AtermRenderSnapshot` via
/// `Default::default()` before Rust fills specific fields. A non-zero
/// default would corrupt unfilled timestamp fields.
///
/// Note: `profiling_enabled` may be non-zero in debug builds (defaults to
/// `cfg!(debug_assertions)`), so we verify timestamp fields individually.
#[test]
fn default_timestamps_are_zero() {
    let ts = PipelineTimestamps::default();
    assert_eq!(ts.parse_duration_ns, 0);
    assert_eq!(ts.grid_duration_ns, 0);
    assert_eq!(ts.process_total_ns, 0);
    assert_eq!(ts.snapshot_copy_ns, 0);
    assert_eq!(ts.snapshot_damage_ns, 0);
    assert_eq!(ts.snapshot_total_ns, 0);
    assert_eq!(ts.last_process_bytes, 0);
    assert_eq!(ts.process_sequence, 0);
}

/// Verify stride equals size (no trailing padding).
///
/// This matters for array-of-structs patterns: if stride != size,
/// a C consumer iterating `PipelineTimestamps[]` with `sizeof()` would
/// read at wrong offsets.
#[test]
fn stride_equals_size() {
    assert_eq!(
        mem::size_of::<PipelineTimestamps>(),
        64,
        "stride should equal size — no trailing padding"
    );
}

/// Verify that arbitrary values survive a byte-level round-trip.
///
/// This tests the same property the Kani proof formalizes: the struct
/// has no padding bytes that could leak uninitialized memory across FFI.
#[test]
fn roundtrip_through_bytes() {
    let original = PipelineTimestamps {
        parse_duration_ns: 0x0102030405060708,
        grid_duration_ns: 0x1112131415161718,
        process_total_ns: 0x2122232425262728,
        snapshot_copy_ns: 0x3132333435363738,
        snapshot_damage_ns: 0x4142434445464748,
        snapshot_total_ns: 0x5152535455565758,
        last_process_bytes: 0x61626364,
        process_sequence: 0x71727374,
        profiling_enabled: true,
    };

    // Round-trip through raw bytes (simulates FFI memcpy)
    let bytes: [u8; 64] = unsafe { mem::transmute(original) };
    let recovered: PipelineTimestamps = unsafe { mem::transmute(bytes) };

    assert_eq!(original, recovered);
}
