// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// aterm certified-gate corpus — functions whose Level-0 safety obligations the
// clean CIC kernel CAN reconstruct (linear-int / bounded fragment). These MUST
// KERNEL-CERTIFY under `-Z trust-verify-full` (certified ON BY DEFAULT); the gate
// fails if any regresses to a solver-trusted-only proof. Each is an aterm-shaped
// safety check (cursor/buffer/color), not a synthetic fixture.
#![crate_type = "lib"]

// Guarded add: pos<1000 ∧ n<1000 -> pos+n < 2000 < u32::MAX (linear bound).
pub fn cursor_advance(pos: u32, n: u32) -> u32 {
    if pos < 1000 && n < 1000 { pos + n } else { 0 }
}

// Modulo index into a fixed ring: n % 4 ∈ [0,4) is in bounds for a [_;4].
pub fn ring_cell(n: usize, ring: &[u8; 4]) -> u8 {
    ring[n % 4]
}

// Constant in-bounds index (3 < 8).
pub fn palette_entry(palette: &[u8; 8]) -> u8 {
    palette[3]
}

// Saturating arithmetic never overflows (the std intrinsic's obligation).
pub fn bump_intensity(x: u8) -> u8 {
    x.saturating_add(1)
}
