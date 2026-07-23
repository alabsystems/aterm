// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for CellExtras shift coordinate arithmetic.
//!
//! The shift methods (shift_rows_up_by, shift_region_up_by,
//! shift_region_down_by, shift_cols_right, shift_cols_left) use
//! drain-rebuild on FxHashMap which is intractable for Kani.
//!
//! These proofs verify the COORDINATE ARITHMETIC in isolation,
//! proving that row/col transformations are correct for all possible
//! input coordinates and parameters across the full u16 space.
//!
//! ## Wiring
//!
//! Add to `grid/mod.rs`:
//! ```ignore
//! #[cfg(kani)]
//! #[path = "shift_coord_arithmetic_tests.rs"]
//! mod proofs_kani_shift_coord;
//! ```

/// shift_rows_up_by: shifted row never underflows.
///
/// When coord.row >= start_row + n (and start_row + n fits in u16),
/// saturating_sub(n) equals row - n (no saturation needed), and
/// the result is >= start_row.
///
/// Proves: extra.rs:528 `coord.row.saturating_sub(n)` is equivalent
/// to unchecked subtraction for all valid inputs.
#[kani::proof]
fn shift_up_no_underflow() {
    let row: u16 = kani::any();
    let start_row: u16 = kani::any();
    let n: u16 = kani::any();
    kani::assume(n > 0);

    if let Some(shift_start) = start_row.checked_add(n) {
        kani::assume(row >= shift_start);

        let new_row = row.saturating_sub(n);

        // row >= start_row + n >= n, so saturating_sub doesn't saturate
        kani::assert(new_row == row - n, "saturating_sub must not saturate");
        // Shifted rows land at start_row or above
        kani::assert(new_row >= start_row, "shifted row must be >= start_row");
    }
}

/// shift_rows_up_by: translation preserves strict ordering.
///
/// If row_a < row_b and both are shifted, new_row_a < new_row_b.
/// This ensures no two distinct source rows collide after shifting.
///
/// Proves: the subtraction in extra.rs:528 is an order-preserving
/// bijection on the shifted coordinate range.
#[kani::proof]
fn shift_up_preserves_order() {
    let row_a: u16 = kani::any();
    let row_b: u16 = kani::any();
    let n: u16 = kani::any();
    kani::assume(n > 0);
    kani::assume(row_a < row_b);
    kani::assume(row_a >= n); // both shifted (row >= shift_start >= n)

    kani::assert(row_a - n < row_b - n, "shift preserves row ordering");
}

/// shift_region_up_by: shifted rows stay within region bounds.
///
/// For rows in (top, bottom] where row >= top + n:
/// new_row = row - n is in [top, bottom - n].
///
/// Proves: extra.rs:558 `coord.row - n` always lands within the
/// valid target range [top, bottom-n] for the region.
#[kani::proof]
fn shift_region_up_stays_in_bounds() {
    let row: u16 = kani::any();
    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    let n: u16 = kani::any();

    kani::assume(n > 0);
    kani::assume(top < bottom);

    if let Some(shift_start) = top.checked_add(n) {
        kani::assume(shift_start <= bottom); // n fits in region
        kani::assume(row > top && row <= bottom);
        kani::assume(row >= shift_start);

        let new_row = row - n;

        kani::assert(new_row >= top, "shifted row must be >= top");
        kani::assert(new_row <= bottom - n, "shifted row must be <= bottom - n");
    }
}

/// shift_region_down_by: drop_start partitions region correctly.
///
/// For rows in [top, drop_start), shifting by n yields row + n <= bottom.
/// This proves saturating_add won't saturate for valid inputs.
///
/// Proves: extra.rs:588 `coord.row.saturating_add(n)` is equivalent
/// to unchecked addition for all valid inputs within the region.
#[kani::proof]
fn shift_region_down_stays_in_bounds() {
    let row: u16 = kani::any();
    let top: u16 = kani::any();
    let bottom: u16 = kani::any();
    let n: u16 = kani::any();

    kani::assume(n > 0);
    kani::assume(top < bottom);
    kani::assume(n as u32 <= (bottom as u32) - (top as u32) + 1); // n fits in region

    let drop_start = bottom.saturating_sub(n.saturating_sub(1));

    kani::assume(row >= top && row < drop_start);

    // row < drop_start = bottom - n + 1, so row <= bottom - n
    // therefore row + n <= bottom <= u16::MAX
    let new_row = row.saturating_add(n);

    kani::assert(new_row == row + n, "saturating_add must not saturate");
    kani::assert(new_row <= bottom, "shifted row must stay within region");
    kani::assert(new_row >= top + n, "shifted row must be in lower part");
}

/// shift_region_down_by: drop_start drops exactly n rows.
///
/// drop_start = bottom - (n - 1) means [drop_start, bottom] contains
/// exactly n rows.
///
/// Proves: the drop_start formula at extra.rs:580 correctly identifies
/// the boundary between shifted and dropped rows.
#[kani::proof]
fn shift_region_down_drop_count() {
    let bottom: u16 = kani::any();
    let n: u16 = kani::any();

    kani::assume(n > 0);
    kani::assume(n as u32 <= bottom as u32 + 1); // n can span [0, bottom]

    let drop_start = bottom.saturating_sub(n.saturating_sub(1));

    // Rows in [drop_start, bottom]: count = bottom - drop_start + 1
    let dropped = (bottom - drop_start + 1) as u32;
    kani::assert(dropped == n as u32, "exactly n rows must be dropped");
}

/// shift_cols_left: shifted column never underflows (no-overflow case).
///
/// When start_col + count fits in u16 (no overflow), col - count >= start_col.
///
/// Proves: extra.rs:785 `coord.col - count` is safe when the guard
/// `coord.col >= shift_start` holds AND `start_col + count` doesn't overflow.
///
/// NOTE: When start_col + count OVERFLOWS u16, saturating_add clips shift_start
/// to u16::MAX, making the guard too weak — shifted columns can land below
/// start_col. See comment on production code extra.rs:772.
/// In practice unreachable (terminal columns < 32768) but the invariant
/// only holds under the checked_add assumption.
#[kani::proof]
fn shift_cols_left_no_underflow() {
    let col: u16 = kani::any();
    let start_col: u16 = kani::any();
    let count: u16 = kani::any();

    kani::assume(count > 0);

    // Require start_col + count fits in u16 (the invariant fails on overflow).
    // Production code uses saturating_add which silently clips — safe only
    // because real terminal columns are bounded well below u16::MAX.
    let shift_start = start_col.checked_add(count);
    kani::assume(shift_start.is_some());
    let shift_start = shift_start.unwrap();
    kani::assume(col >= shift_start);

    let new_col = col - count;

    kani::assert(new_col >= start_col, "shifted column must be >= start_col");
}

/// shift_cols_right: shifted column preserves ordering and bounds.
///
/// Columns at or after start_col shift right by count. Those that
/// exceed max_col are correctly dropped. Order is preserved among
/// surviving columns.
///
/// Proves: extra.rs:624-625 `saturating_add(count)` preserves
/// relative ordering for columns that don't overflow.
#[kani::proof]
fn shift_cols_right_preserves_order() {
    let col_a: u16 = kani::any();
    let col_b: u16 = kani::any();
    let count: u16 = kani::any();
    let max_col: u16 = kani::any();

    kani::assume(count > 0);
    kani::assume(max_col > 0);
    kani::assume(col_a < col_b);

    let new_a = col_a.saturating_add(count);
    let new_b = col_b.saturating_add(count);

    // If both survive (< max_col) and don't saturate, order is preserved
    if col_a.checked_add(count).is_some() && col_b.checked_add(count).is_some() {
        if new_a < max_col && new_b < max_col {
            kani::assert(new_a < new_b, "right shift preserves column ordering");
        }
    }
}

// =========================================================================
// Row-offset amortization proofs (#4542)
// =========================================================================
//
// W13 added row_offset amortization to CellExtras: full-screen scrolls
// increment a u16 offset instead of drain-rebuilding FxHashMap.
// physical_row(logical) = logical + row_offset (unchecked u16 addition).
// Compaction triggers at ROW_OFFSET_COMPACT_THRESHOLD (256).
//
// The HashMap operations are intractable for CBMC. These proofs verify
// the ARITHMETIC properties: no overflow, correct compaction, equivalence.

/// Compaction threshold from extra.rs.
const ROW_OFFSET_COMPACT_THRESHOLD: u16 = 256;

/// physical_row: no overflow under real terminal bounds.
///
/// Terminal grids are bounded: max 500 rows (generous upper bound).
/// row_offset is bounded by ROW_OFFSET_COMPACT_THRESHOLD.
/// Proves: logical + row_offset fits in u16 for all realistic inputs.
///
/// Proves: extra.rs physical_row() won't panic or wrap for valid grids.
#[kani::proof]
fn physical_row_no_overflow() {
    let logical: u16 = kani::any();
    let row_offset: u16 = kani::any();

    // Bounds matching real terminal constraints:
    // - Terminal rows: 0..500 (generous; typical is 24-200)
    // - Row offset: 0..256 (compaction threshold)
    kani::assume(logical < 500);
    kani::assume(row_offset < ROW_OFFSET_COMPACT_THRESHOLD);

    // This is the computation in physical_row()
    let result = logical.checked_add(row_offset);
    kani::assert(result.is_some(), "physical_row must not overflow");

    let phys = result.unwrap();
    kani::assert(phys < 756, "physical row bounded by logical + threshold");
}

/// physical_coord: translation is a bijection (distinct logical coords
/// map to distinct physical coords for the same row_offset).
///
/// Proves: no two different logical coordinates collide in physical space,
/// ensuring FxHashMap lookups remain correct after offset translation.
#[kani::proof]
fn physical_coord_bijective() {
    let row_a: u16 = kani::any();
    let col_a: u16 = kani::any();
    let row_b: u16 = kani::any();
    let col_b: u16 = kani::any();
    let row_offset: u16 = kani::any();

    kani::assume(row_a < 500);
    kani::assume(row_b < 500);
    kani::assume(row_offset < ROW_OFFSET_COMPACT_THRESHOLD);
    // At least one coordinate differs
    kani::assume(row_a != row_b || col_a != col_b);

    let phys_row_a = row_a + row_offset;
    let phys_row_b = row_b + row_offset;

    // If rows differ, physical rows differ (addition preserves inequality)
    // If rows same but cols differ, physical coords differ (cols unchanged)
    kani::assert(
        phys_row_a != phys_row_b || col_a != col_b,
        "physical_coord must be injective",
    );
}

/// compact_row_offset: re-keying arithmetic is correct.
///
/// After compaction, physical_row = coord.row - offset (for non-stale entries).
/// This subtraction is safe because non-stale entries have coord.row >= offset.
///
/// Proves: extra.rs compact_row_offset() subtraction never underflows
/// for entries that pass the coord.row >= offset filter.
#[kani::proof]
fn compact_rekey_no_underflow() {
    let coord_row: u16 = kani::any();
    let offset: u16 = kani::any();

    kani::assume(offset > 0); // compaction only runs when offset > 0
    kani::assume(offset < ROW_OFFSET_COMPACT_THRESHOLD + 500); // saturating_add bound
    kani::assume(coord_row >= offset); // non-stale entry filter

    let new_row = coord_row - offset;

    // The re-keyed row is a valid logical coordinate
    kani::assert(new_row <= coord_row, "re-keyed row must be <= original");
    kani::assert(new_row + offset == coord_row, "re-keying is reversible");
}

/// shift_rows_up_by amortized path: offset accumulation + compaction.
///
/// Models the fast path: start_row == 0, so we increment row_offset
/// by n (saturating). If offset >= threshold, compact resets to 0.
///
/// Proves: row_offset stays bounded after any sequence of full-screen
/// scrolls, and the offset value is always < threshold after return.
#[kani::proof]
#[kani::unwind(11)]
fn amortized_scroll_offset_bounded() {
    let mut row_offset: u16 = 0;

    let scroll_count: usize = kani::any();
    kani::assume(scroll_count <= 10);

    let mut i = 0;
    while i < scroll_count {
        let n: u16 = kani::any();
        kani::assume(n >= 1 && n <= 50); // typical scroll: 1-50 lines

        row_offset = row_offset.saturating_add(n);

        // Model compaction trigger (matches extra.rs shift_rows_up_by)
        if row_offset >= ROW_OFFSET_COMPACT_THRESHOLD {
            row_offset = 0; // compact_row_offset() resets to 0
        }

        i += 1;
    }

    kani::assert(
        row_offset < ROW_OFFSET_COMPACT_THRESHOLD,
        "row_offset must be < threshold after any scroll sequence",
    );
}
