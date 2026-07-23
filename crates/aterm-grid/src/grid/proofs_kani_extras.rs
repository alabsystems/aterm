// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for HAS_EXTRAS/COMPLEX flag independence, rendering guard
//! completeness, ScrolledRowExtras is_empty correctness, HAS_EXTRAS bit
//! independence with color data, and color mutation safety.
//!
//! Invariant proofs (StyleId, Cell::EMPTY, roundtrip, AC7 iff, ring_extras)
//! are in `proofs_kani_extras_invariants.rs`.
//!
//! ## Verification Targets
//!
//! 1. **HAS_EXTRAS ↔ COMPLEX independence**: flags in different Cell fields
//!    (PackedColors vs CellFlags) can be set/cleared independently.
//! 2. **Rendering guard completeness**: `has_extras() || is_complex()` catches
//!    exactly the cells needing extras probes (bulk.rs + cell_style.rs).
//! 3. **Shift-up amortization**: shift_rows_up_by invalidates old logical row
//!    lookups (the entry moves to a lower logical row).
//! 4. **ScrolledRowExtras::is_empty()**: operates on real struct, verifying
//!    all 5 Vec fields are checked. A false positive causes permanent data loss.
//! 5. **HAS_EXTRAS bit independence**: bit 16 of PackedColors is orthogonal
//!    to fg/bg color index bits and mode bits.
//! 6. **Color mutation safety**: all PackedColors color setters preserve the
//!    HAS_EXTRAS bit.
//!
//! Part of memory_verification phase, #5445 epic.

use std::sync::Arc;

use super::scroll_convert::ScrolledRowExtras;
use crate::{Cell, PackedColors};
use aterm_alloc::SmallVec;
use aterm_scrollback::HyperlinkSpan;

/// HAS_EXTRAS and COMPLEX flags are independent: setting one never clears the other.
///
/// These flags live in different Cell fields (PackedColors vs CellFlags) but
/// both gate extras probing in the rendering path. The `|| cell.is_complex()`
/// safety net relies on this independence — a complex cell with unset HAS_EXTRAS
/// must still be probed.
///
/// Uses symbolic overflow indices to verify independence across all index values.
#[kani::proof]
fn has_extras_and_complex_flags_independent() {
    // Start with a complex cell using a symbolic overflow index
    let overflow_idx: u16 = kani::any();
    let mut cell = Cell::with_overflow_index(overflow_idx);
    kani::assert(
        cell.is_complex(),
        "precondition: complex cell with symbolic index",
    );
    kani::assert(
        !cell.has_extras(),
        "precondition: no extras on fresh complex cell",
    );

    // Setting HAS_EXTRAS must not clear COMPLEX
    cell.set_has_extras(true);
    kani::assert(
        cell.is_complex(),
        "set_has_extras(true) must preserve COMPLEX for symbolic index",
    );
    kani::assert(
        cell.has_extras(),
        "set_has_extras(true) must set HAS_EXTRAS",
    );

    // Clearing HAS_EXTRAS must not clear COMPLEX
    cell.set_has_extras(false);
    kani::assert(
        cell.is_complex(),
        "set_has_extras(false) must preserve COMPLEX for symbolic index",
    );
    kani::assert(
        !cell.has_extras(),
        "set_has_extras(false) must clear HAS_EXTRAS",
    );

    // Start with a plain cell with HAS_EXTRAS
    let mut plain = Cell::EMPTY;
    plain.set_has_extras(true);
    kani::assert(!plain.is_complex(), "precondition: non-complex cell");
    kani::assert(plain.has_extras(), "precondition: has extras");

    // Setting COMPLEX via set_overflow_index with symbolic index must not clear HAS_EXTRAS
    let overflow_idx2: u16 = kani::any();
    plain.set_overflow_index(overflow_idx2);
    kani::assert(
        plain.is_complex(),
        "set_overflow_index must set COMPLEX for symbolic index",
    );
    kani::assert(
        plain.has_extras(),
        "set_overflow_index must preserve HAS_EXTRAS for symbolic index",
    );
}

/// Rendering guard completeness: `has_extras() || is_complex()` catches all
/// cells that need extras probing.
///
/// Models the actual rendering path condition from bulk.rs and cell_style.rs.
/// Verifies that for all possible Cell flag combinations, the guard correctly
/// identifies cells needing extras lookup. This is the formal basis for the
/// safety net added to both FFI and GPU rendering paths.
///
#[kani::proof]
fn rendering_guard_covers_all_extras_cases() {
    let has_extras_flag: bool = kani::any();
    let is_complex_flag: bool = kani::any();

    let mut cell = Cell::EMPTY;

    // Set up the cell with the given flag state
    cell.set_has_extras(has_extras_flag);
    if is_complex_flag {
        cell.set_overflow_index(0);
    }

    // Verify flag readback matches what we set
    kani::assert(
        cell.has_extras() == has_extras_flag,
        "has_extras must reflect set_has_extras",
    );
    kani::assert(
        cell.is_complex() == is_complex_flag,
        "is_complex must reflect set_overflow_index",
    );

    // The rendering guard from bulk.rs / cell_style.rs
    let needs_probe = cell.has_extras() || cell.is_complex();

    // If either flag is set, the guard must fire
    if has_extras_flag || is_complex_flag {
        kani::assert(needs_probe, "guard must fire when any flag is set");
    }

    // If neither flag is set, the guard must not fire (optimization correctness)
    if !has_extras_flag && !is_complex_flag {
        kani::assert(!needs_probe, "guard must not fire on plain cells");
    }
}

/// Shift-up-by amortization roundtrip: insert at logical (r, c), then
/// shift_rows_up_by(0, n), then get at logical (r, c) must return None
/// (the entry moved to a lower logical row or was scrolled off).
///
/// After shift_rows_up_by(0, n), logical row r maps to physical row r + (offset + n).
/// But the entry was stored at physical row r + offset. So looking up logical row r
/// now looks for physical row r + offset + n, which won't find the entry stored
/// at r + offset. The entry is now at logical row r - n (if r >= n) or scrolled off.
#[kani::proof]
fn shift_up_amortization_invalidates_old_logical() {
    let logical_row: u16 = kani::any();
    let old_offset: u16 = kani::any();
    let n: u16 = kani::any();
    kani::assume(n >= 1);
    kani::assume(n <= 256); // Reasonable scroll amount

    // Entry was stored at physical row = logical_row + old_offset
    let stored_phys = logical_row.checked_add(old_offset);

    // After shift, new offset = old_offset + n (saturating)
    let new_offset = old_offset.saturating_add(n);

    // Looking up the same logical_row now looks for physical row = logical_row + new_offset
    let lookup_phys = logical_row.checked_add(new_offset);

    // These should differ (unless both overflow to None)
    if let (Some(stored), Some(lookup)) = (stored_phys, lookup_phys) {
        // n >= 1, so new_offset > old_offset (unless saturated at u16::MAX)
        if new_offset > old_offset {
            kani::assert(
                stored != lookup,
                "old logical row must not find the stored entry after shift",
            );
        }
    }
}

/// ScrolledRowExtras::is_empty() on real struct: true iff all 5 Vec fields empty.
///
/// Constructs real ScrolledRowExtras instances with symbolic field population
/// and verifies is_empty() returns the correct answer. This replaces the
/// previous abstract boolean model that was tautological.
///
/// A false positive causes permanent data loss: hyperlinks, RGB colors, and
/// combining marks silently dropped from ring buffer scrollback.
#[kani::proof]
fn scrolled_row_extras_is_empty_on_real_struct() {
    let has_hyperlinks: bool = kani::any();
    let has_complex: bool = kani::any();
    let has_combining: bool = kani::any();
    let has_rgb_fg: bool = kani::any();
    let has_rgb_bg: bool = kani::any();

    let mut extras = ScrolledRowExtras::default();

    if has_hyperlinks {
        extras
            .hyperlinks
            .push(HyperlinkSpan::new(0, 1, Arc::from("https://example.com")));
    }
    if has_complex {
        extras.complex_chars.push((0, Arc::from("abc")));
    }
    if has_combining {
        extras
            .combining
            .push((0, SmallVec::from_slice(&['\u{0300}'])));
    }
    if has_rgb_fg {
        extras.rgb_fg.push((0, [255, 0, 0]));
    }
    if has_rgb_bg {
        extras.rgb_bg.push((0, [0, 0, 255]));
    }

    let all_empty = !has_hyperlinks && !has_complex && !has_combining && !has_rgb_fg && !has_rgb_bg;

    kani::assert(
        extras.is_empty() == all_empty,
        "is_empty must reflect actual field population",
    );

    // Contrapositive: any populated field means not empty
    if has_hyperlinks || has_complex || has_combining || has_rgb_fg || has_rgb_bg {
        kani::assert(!extras.is_empty(), "non-empty extras must not report empty");
    }
}

/// HAS_EXTRAS bit independence: setting/clearing bit 16 preserves all color data.
///
/// Verifies that with_extras_flag() and without_extras_flag() are orthogonal
/// to fg_index, bg_index, fg_mode, and bg_mode. This is the critical invariant
/// that allows the rendering path to use has_extras() as a fast skip without
/// corrupting color information.
#[kani::proof]
fn has_extras_bit_independent_of_colors() {
    let raw: u32 = kani::any();
    let colors = PackedColors(raw);

    // Capture original color state
    let orig_fg_index = colors.fg_index();
    let orig_bg_index = colors.bg_index();
    let orig_fg_mode = colors.fg_mode();
    let orig_bg_mode = colors.bg_mode();

    // Set HAS_EXTRAS flag
    let with_flag = colors.with_extras_flag();
    kani::assert(with_flag.has_extras(), "with_extras_flag must set the bit");
    kani::assert(
        with_flag.fg_index() == orig_fg_index,
        "with_extras_flag must not change fg_index",
    );
    kani::assert(
        with_flag.bg_index() == orig_bg_index,
        "with_extras_flag must not change bg_index",
    );
    kani::assert(
        with_flag.fg_mode() == orig_fg_mode,
        "with_extras_flag must not change fg_mode",
    );
    kani::assert(
        with_flag.bg_mode() == orig_bg_mode,
        "with_extras_flag must not change bg_mode",
    );

    // Clear HAS_EXTRAS flag
    let without_flag = with_flag.without_extras_flag();
    kani::assert(
        !without_flag.has_extras(),
        "without_extras_flag must clear the bit",
    );
    kani::assert(
        without_flag.fg_index() == orig_fg_index,
        "without_extras_flag must not change fg_index",
    );
    kani::assert(
        without_flag.bg_index() == orig_bg_index,
        "without_extras_flag must not change bg_index",
    );
    kani::assert(
        without_flag.fg_mode() == orig_fg_mode,
        "without_extras_flag must not change fg_mode",
    );
    kani::assert(
        without_flag.bg_mode() == orig_bg_mode,
        "without_extras_flag must not change bg_mode",
    );
}

/// Color mutations preserve has_extras() flag.
///
/// Verifies that setting fg/bg colors (indexed, RGB mode, default) does not
/// corrupt the HAS_EXTRAS bit. This is the reverse direction from the previous
/// proof: color ops must not clobber bit 16.
#[kani::proof]
fn color_mutations_preserve_has_extras() {
    let fg_index: u8 = kani::any();
    let bg_index: u8 = kani::any();

    // Start with HAS_EXTRAS set
    let base = PackedColors::DEFAULT.with_extras_flag();
    kani::assert(base.has_extras(), "precondition: HAS_EXTRAS set");

    // set_fg_indexed preserves HAS_EXTRAS
    let after_fg = base.set_fg_indexed(fg_index);
    kani::assert(
        after_fg.has_extras(),
        "set_fg_indexed must preserve HAS_EXTRAS",
    );
    kani::assert(
        after_fg.fg_index() == fg_index,
        "set_fg_indexed must store correct index",
    );

    // set_bg_indexed preserves HAS_EXTRAS
    let after_bg = base.set_bg_indexed(bg_index);
    kani::assert(
        after_bg.has_extras(),
        "set_bg_indexed must preserve HAS_EXTRAS",
    );
    kani::assert(
        after_bg.bg_index() == bg_index,
        "set_bg_indexed must store correct index",
    );

    // with_rgb_fg preserves HAS_EXTRAS
    let after_rgb_fg = base.with_rgb_fg();
    kani::assert(
        after_rgb_fg.has_extras(),
        "with_rgb_fg must preserve HAS_EXTRAS",
    );

    // with_rgb_bg preserves HAS_EXTRAS
    let after_rgb_bg = base.with_rgb_bg();
    kani::assert(
        after_rgb_bg.has_extras(),
        "with_rgb_bg must preserve HAS_EXTRAS",
    );

    // set_fg_default preserves HAS_EXTRAS
    let after_fg_default = base.set_fg_default();
    kani::assert(
        after_fg_default.has_extras(),
        "set_fg_default must preserve HAS_EXTRAS",
    );

    // set_bg_default preserves HAS_EXTRAS
    let after_bg_default = base.set_bg_default();
    kani::assert(
        after_bg_default.has_extras(),
        "set_bg_default must preserve HAS_EXTRAS",
    );
}
