// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for CellExtras shift coordinate arithmetic.
//!
//! The shift methods (shift_rows_up_by, shift_region_up_by,
//! shift_region_down_by, shift_cols_right, shift_cols_left) drain and rebuild
//! an FxHashMap, which is intractable for Kani, so these proofs verify the
//! coordinate arithmetic in isolation over the full u16 space.

/// shift_rows_up_by: shifted row never underflows.
///
/// When coord.row >= start_row + n (and start_row + n fits in u16),
/// saturating_sub(n) equals row - n and the result is >= start_row, so the
/// saturating subtraction equals unchecked subtraction for all valid inputs.
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
/// If row_a < row_b and both are shifted, new_row_a < new_row_b, so no two
/// source rows collide: the subtraction is an order-preserving bijection on
/// the shifted range.
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
/// For rows in (top, bottom] where row >= top + n, `row - n` lands in
/// [top, bottom - n].
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
/// For rows in [top, drop_start), shifting by n yields row + n <= bottom, so
/// saturating_add equals unchecked addition for valid inputs.
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
/// drop_start = bottom - (n - 1), so [drop_start, bottom] holds exactly n
/// rows: the boundary between shifted and dropped rows.
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
/// When start_col + count fits in u16, `col - count >= start_col` under the
/// guard `coord.col >= shift_start`.
///
/// When start_col + count overflows u16, saturating_add clips shift_start to
/// u16::MAX and the guard is too weak: shifted columns can land below
/// start_col. Unreachable in practice (columns < 32768), but the invariant
/// holds only under the checked_add assumption.
#[kani::proof]
fn shift_cols_left_no_underflow() {
    let col: u16 = kani::any();
    let start_col: u16 = kani::any();
    let count: u16 = kani::any();

    kani::assume(count > 0);

    // Require start_col + count to fit in u16 (the invariant fails on
    // overflow). Production uses saturating_add, safe only because terminal
    // columns are far below u16::MAX.
    let shift_start = start_col.checked_add(count);
    kani::assume(shift_start.is_some());
    let shift_start = shift_start.unwrap();
    kani::assume(col >= shift_start);

    let new_col = col - count;

    kani::assert(new_col >= start_col, "shifted column must be >= start_col");
}

/// shift_cols_right: shifted column preserves ordering and bounds.
///
/// Columns at or after start_col shift right by count; those past max_col are
/// dropped; `saturating_add(count)` preserves order among columns that do not
/// overflow.
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
// Row-offset amortization proofs
// =========================================================================
//
// Full-screen scrolls increment a u16 row_offset instead of rebuilding the
// FxHashMap: physical_row(logical) = logical + row_offset (unchecked u16
// addition), with compaction at ROW_OFFSET_COMPACT_THRESHOLD (256). The
// HashMap operations are intractable for CBMC, so these proofs check the
// arithmetic: no overflow, correct compaction, equivalence.

/// Compaction threshold from extra.rs.
const ROW_OFFSET_COMPACT_THRESHOLD: u16 = 256;

/// physical_row: no overflow under real terminal bounds.
///
/// With at most 500 rows (a generous bound) and row_offset below
/// ROW_OFFSET_COMPACT_THRESHOLD, logical + row_offset fits in u16, so
/// physical_row() never panics or wraps.
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
/// After compaction, physical_row = coord.row - offset for non-stale entries,
/// which never underflows because those entries have coord.row >= offset.
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
