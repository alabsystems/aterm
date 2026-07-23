// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for the grid cell module.
//!
//! Extracted from cell.rs (#1977).

use super::*;

/// Codepoint pack/unpack is lossless for BMP codepoints.
#[kani::proof]
fn cell_bmp_codepoint_roundtrip() {
    let codepoint: u16 = kani::any();
    // Only test valid non-surrogate BMP codepoints
    kani::assume(codepoint < 0xD800 || codepoint > 0xDFFF);

    if let Some(c) = char::from_u32(codepoint as u32) {
        let cell = Cell::new(c);
        kani::assert(!cell.is_complex(), "BMP char should not be complex");
        kani::assert(
            cell.codepoint() == codepoint as u32,
            "BMP codepoint roundtrip failed",
        );
    }
}

/// Flags pack/unpack is lossless.
#[kani::proof]
fn cell_flags_roundtrip() {
    let flags_bits: u16 = kani::any();
    // Mask to valid range (exclude COMPLEX for this test)
    let flags_bits = flags_bits & CellFlags::VISUAL_FLAGS_MASK;

    let flags = CellFlags::from_bits(flags_bits);
    let cell = Cell::with_style(' ', PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG, flags);

    kani::assert(
        (cell.flags().bits() & CellFlags::VISUAL_FLAGS_MASK) == flags_bits,
        "flags roundtrip failed",
    );
}

/// Indexed color roundtrip.
#[kani::proof]
fn packed_colors_indexed_roundtrip() {
    let fg_index: u8 = kani::any();
    let bg_index: u8 = kani::any();

    let colors = PackedColors::with_indexed(fg_index, bg_index);

    kani::assert(colors.fg_is_indexed(), "fg should be indexed");
    kani::assert(colors.bg_is_indexed(), "bg should be indexed");
    kani::assert(colors.fg_index() == fg_index, "fg index mismatch");
    kani::assert(colors.bg_index() == bg_index, "bg index mismatch");
}

/// Complex cell flag handling.
#[kani::proof]
fn cell_complex_flag() {
    let index: u16 = kani::any();
    let cell = Cell::with_overflow_index(index);

    kani::assert(cell.is_complex(), "should be complex");
    kani::assert(cell.char_data() == index, "index should match");
    kani::assert(
        cell.codepoint() == 0xFFFD,
        "complex codepoint should be replacement",
    );
}

/// set_char() with any BMP char on a complex cell produces a valid state:
/// stores the codepoint directly and clears the COMPLEX flag.
///
/// Note: set_char() has a debug_assert that rejects non-BMP characters
/// (callers should use set_overflow_index() instead). This proof covers
/// the BMP contract only. Re: #5788.
#[kani::proof]
fn set_char_clears_complex_and_stores_bmp() {
    let codepoint: u16 = kani::any();
    // Only valid non-surrogate BMP codepoints
    kani::assume(codepoint < 0xD800 || codepoint > 0xDFFF);

    if let Some(c) = char::from_u32(codepoint as u32) {
        // Start with an arbitrary complex cell state
        let initial_index: u16 = kani::any();
        let mut cell = Cell::with_overflow_index(initial_index);
        kani::assert(cell.is_complex(), "precondition: cell starts complex");

        cell.set_char(c);

        // Post-conditions:
        // 1. COMPLEX flag is always cleared
        kani::assert(!cell.is_complex(), "set_char must clear COMPLEX flag");

        // 2. char_data stores the BMP codepoint directly
        kani::assert(
            cell.char_data() == codepoint,
            "BMP char must be stored directly",
        );

        // 3. char() accessor returns the original character
        kani::assert(cell.char() == c, "char() must return original BMP char");
    }
}

/// Cell::new() with any valid Unicode codepoint produces a valid state:
/// BMP characters stored directly, non-BMP stored as U+FFFD.
/// (Cell::new does NOT debug_assert on non-BMP, unlike set_char.)
#[kani::proof]
fn cell_new_any_codepoint_valid_state() {
    let codepoint: u32 = kani::any();
    kani::assume(codepoint <= 0x10_FFFF);
    kani::assume(codepoint < 0xD800 || codepoint > 0xDFFF);

    if let Some(c) = char::from_u32(codepoint) {
        let cell = Cell::new(c);

        // Never complex from Cell::new
        kani::assert(!cell.is_complex(), "Cell::new must not set COMPLEX flag");

        if codepoint <= Cell::MAX_DIRECT_CODEPOINT {
            kani::assert(
                cell.char_data() == codepoint as u16,
                "BMP char must be stored directly",
            );
            kani::assert(cell.char() == c, "char() must return original BMP char");
        } else {
            kani::assert(
                cell.char_data() == '\u{FFFD}' as u16,
                "non-BMP char must store U+FFFD",
            );
            kani::assert(
                cell.char() == '\u{FFFD}',
                "char() must return U+FFFD for non-BMP",
            );
        }
    }
}

/// set_char() followed by Cell::new() produce equivalent results for
/// the same BMP character. This is the constructor-parity invariant.
#[kani::proof]
fn set_char_new_parity_for_bmp() {
    let codepoint: u16 = kani::any();
    kani::assume(codepoint < 0xD800 || codepoint > 0xDFFF);

    if let Some(c) = char::from_u32(codepoint as u32) {
        let via_new = Cell::new(c);
        let mut via_set = Cell::EMPTY;
        via_set.set_char(c);

        kani::assert(
            via_new.char_data() == via_set.char_data(),
            "new vs set_char must produce same char_data",
        );
        kani::assert(
            via_new.is_complex() == via_set.is_complex(),
            "new vs set_char must agree on complex flag",
        );
    }
}
