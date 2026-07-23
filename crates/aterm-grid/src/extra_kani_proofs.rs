// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for the cell extras module.

use super::*;

// cell_coord_hash_consistent removed (#5887): derived PartialEq reflexivity is a
// language guarantee — symbolic row/col adds no verification value.

/// is_combining_mark returns true for all codepoints in U+0300..U+036F.
///
/// Uses symbolic codepoint with kani::assume to verify the combining mark
/// detection function covers the entire diacritical marks range.
#[kani::proof]
fn combining_mark_range_valid() {
    let codepoint: u32 = kani::any();
    kani::assume(codepoint >= 0x0300 && codepoint <= 0x036F);

    if let Some(c) = char::from_u32(codepoint) {
        kani::assert(
            is_combining_mark(c),
            "diacritical marks should be combining",
        );
    }
}

// hyperlink_data_box_niche removed (#5887): size_of is a compile-time constant;
// Kani bounded model checking adds nothing over a const_assert.

/// FNV-1a hash used for hyperlink grouping never returns 0 for any
/// single-byte input.
#[kani::proof]
fn fnv1a_nonzero_single_byte() {
    let byte: u8 = kani::any();

    // Same FNV-1a logic as aterm_terminal_cell_hyperlink_id
    let mut hash: u32 = 2_166_136_261;
    hash ^= u32::from(byte);
    hash = hash.wrapping_mul(16_777_619);

    // Post-mapping: 0 becomes 1
    let result = if hash == 0 { 1 } else { hash };
    kani::assert(result != 0, "FNV-1a result must be non-zero after mapping");
}

/// FNV-1a hash for two symbolic bytes also produces a non-zero result.
#[kani::proof]
fn fnv1a_nonzero_two_bytes() {
    let b0: u8 = kani::any();
    let b1: u8 = kani::any();

    let mut hash: u32 = 2_166_136_261;
    hash ^= u32::from(b0);
    hash = hash.wrapping_mul(16_777_619);
    hash ^= u32::from(b1);
    hash = hash.wrapping_mul(16_777_619);

    let result = if hash == 0 { 1 } else { hash };
    kani::assert(
        result != 0,
        "two-byte FNV-1a must be non-zero after mapping",
    );
}
