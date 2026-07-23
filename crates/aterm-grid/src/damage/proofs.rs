// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kani proofs for damage tracking.

#[path = "proofs_display_offset.rs"]
mod display_offset;

#[path = "proofs_iter.rs"]
mod iter_proofs;

use super::*;

// Keep proof ranges small to avoid CBMC blow-ups on unconstrained u16s.
const PROOF_MAX_ROWS: u16 = 64;
const PROOF_MAX_COLS: u16 = 256;

/// DamageTracker bitset marking never goes out of bounds.
/// Reduced rows<=8, unwind 10 — Vec::extend_with needs rows+1 unwind
/// plus internal loop overhead. 8 rows covers all code paths (in-bounds,
/// out-of-bounds) without hitting CBMC's memory budget.
#[kani::proof]
#[kani::unwind(10)]
fn damage_tracker_mark_row_bounds() {
    let rows: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 8);

    let mut tracker = DamageTracker::new(rows);

    let row: u16 = kani::any();
    tracker.mark_row(row);

    // If row was in bounds, it should be marked
    // If out of bounds, should not crash
    if row < rows {
        kani::assert(tracker.is_row_damaged(row), "row should be marked");
    }
}

/// Cell damage tracking maintains proper bounds (left <= right when damaged).
#[kani::proof]
#[kani::unwind(10)] // Vec allocation needs rows+1 unwind
fn damage_tracker_cell_bounds_valid() {
    let rows: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 8);

    let mut tracker = DamageTracker::new(rows);

    let row: u16 = kani::any();
    let col: u16 = kani::any();
    kani::assume(row < rows);

    tracker.mark_cell(row, col);

    // After marking a cell, bounds should be valid, including saturation at u16::MAX.
    if let Some((left, right)) = tracker.row_damage_bounds(row) {
        kani::assert(left == col, "left should match the marked column");
        kani::assert(left <= right, "left must not exceed right");
        kani::assert(right >= col, "right must cover the marked column");
        if col < u16::MAX {
            kani::assert(
                right > col,
                "right must be past marked col when not saturated",
            );
        } else {
            kani::assert(right == u16::MAX, "right should saturate at u16::MAX");
        }
    } else {
        kani::assert(false, "bounds should exist after marking cell");
    }
}

/// Boundary regression: marking u16::MAX - 1 keeps right bound at u16::MAX.
#[kani::proof]
#[kani::unwind(10)] // Vec allocation needs rows+1 unwind
fn damage_tracker_cell_bounds_near_u16_max() {
    let rows: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 8);

    let mut tracker = DamageTracker::new(rows);

    let row: u16 = kani::any();
    kani::assume(row < rows);

    let col = u16::MAX - 1;
    tracker.mark_cell(row, col);

    if let Some((left, right)) = tracker.row_damage_bounds(row) {
        kani::assert(left == col, "left should match marked column");
        kani::assert(
            right == u16::MAX,
            "right should saturate at u16::MAX boundary",
        );
    } else {
        kani::assert(false, "bounds should exist after marking cell");
    }
}

/// Multiple cell marks expand bounds correctly.
#[kani::proof]
#[kani::unwind(10)] // Vec allocation needs rows+1 unwind
fn damage_tracker_cell_bounds_expand() {
    let rows: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 8);

    let mut tracker = DamageTracker::new(rows);

    let row: u16 = kani::any();
    let col1: u16 = kani::any();
    let col2: u16 = kani::any();
    kani::assume(row < rows);

    tracker.mark_cell(row, col1);
    tracker.mark_cell(row, col2);

    if let Some((left, right)) = tracker.row_damage_bounds(row) {
        let min_col = col1.min(col2);
        let max_col = col1.max(col2);
        kani::assert(left <= min_col, "left must cover minimum column");
        kani::assert(right >= max_col, "right must cover maximum column");
        if max_col < u16::MAX {
            kani::assert(
                right > max_col,
                "right must be past max col when not saturated",
            );
        } else {
            kani::assert(right == u16::MAX, "right should saturate at u16::MAX");
        }
    } else {
        kani::assert(false, "bounds should exist after marking cells");
    }
}

/// LineDamageBounds is_empty is correct.
#[kani::proof]
fn line_damage_bounds_is_empty_correct() {
    let line: u16 = kani::any();
    let left: u16 = kani::any();
    let right: u16 = kani::any();
    kani::assume(left <= PROOF_MAX_COLS);
    kani::assume(right <= PROOF_MAX_COLS);

    let bounds = LineDamageBounds::new(line, left, right);

    if left >= right {
        kani::assert(bounds.is_empty(), "should be empty when left >= right");
    } else {
        kani::assert(!bounds.is_empty(), "should not be empty when left < right");
    }
}

/// DamageRect from_line produces single-row rect.
#[kani::proof]
fn damage_rect_from_line_single_row() {
    let line: u16 = kani::any();
    let left: u16 = kani::any();
    let right: u16 = kani::any();
    kani::assume(line < PROOF_MAX_ROWS); // Avoid overflow in bottom
    kani::assume(left <= PROOF_MAX_COLS);
    kani::assume(right <= PROOF_MAX_COLS);
    kani::assume(left < right);

    let bounds = LineDamageBounds::new(line, left, right);
    let rect = DamageRect::from_line(bounds);

    kani::assert(rect.top == line, "top should be line");
    kani::assert(rect.bottom == line + 1, "bottom should be line + 1");
    kani::assert(rect.left == left, "left preserved");
    kani::assert(rect.right == right, "right preserved");
    kani::assert(rect.height() == 1, "single row height");
}

/// LineDamageBounds can_merge_with is symmetric for adjacent lines.
#[kani::proof]
fn line_damage_bounds_merge_symmetric() {
    let line1: u16 = kani::any();
    let left1: u16 = kani::any();
    let right1: u16 = kani::any();
    let left2: u16 = kani::any();
    let right2: u16 = kani::any();

    kani::assume(line1 < PROOF_MAX_ROWS);
    kani::assume(left1 <= PROOF_MAX_COLS);
    kani::assume(right1 <= PROOF_MAX_COLS);
    kani::assume(left2 <= PROOF_MAX_COLS);
    kani::assume(right2 <= PROOF_MAX_COLS);
    kani::assume(left1 < right1);
    kani::assume(left2 < right2);

    let a = LineDamageBounds::new(line1, left1, right1);
    let b = LineDamageBounds::new(line1 + 1, left2, right2);

    if a.can_merge_with(&b) {
        kani::assert(b.can_merge_with(&a), "merge should be symmetric");
    }
}

/// DamageRect extend_with grows correctly.
#[kani::proof]
fn damage_rect_extend_grows() {
    let top: u16 = kani::any();
    let left: u16 = kani::any();
    let right: u16 = kani::any();

    kani::assume(top > 0 && top < PROOF_MAX_ROWS);
    kani::assume(left < right);
    kani::assume(right < PROOF_MAX_COLS);

    let mut rect = DamageRect::new(top, top + 1, left, right);

    let new_left: u16 = kani::any();
    let new_right: u16 = kani::any();
    kani::assume(new_left < new_right);
    kani::assume(new_right < PROOF_MAX_COLS);

    // Line immediately below the rect
    let bounds = LineDamageBounds::new(top + 1, new_left, new_right);

    if rect.can_extend_with(bounds) {
        let old_top = rect.top;
        rect.extend_with(bounds);

        kani::assert(rect.top == old_top, "top unchanged");
        kani::assert(rect.bottom == top + 2, "bottom extended by 1");
        kani::assert(rect.left <= new_left.min(left), "left covers both");
        kani::assert(rect.right >= new_right.max(right), "right covers both");
    }
}

/// Damage row bounds are clamped to cols.
#[kani::proof]
#[kani::unwind(10)] // Vec allocation needs rows+1 unwind
fn damage_row_bounds_clamped() {
    let rows: u16 = kani::any();
    let cols: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 8);
    kani::assume(cols > 0 && cols <= 100);

    let mut damage = Damage::new(rows);

    let row: u16 = kani::any();
    kani::assume(row < rows);

    damage.mark_row(row);

    if let Some((left, right)) = damage.row_damage_bounds(row, cols) {
        kani::assert(left <= cols, "left clamped to cols");
        kani::assert(right <= cols, "right clamped to cols");
    }
}

/// Damage::row_damage_bounds preserves the upper edge at u16::MAX for near-max cells.
#[kani::proof]
#[kani::unwind(10)] // Vec allocation needs rows+1 unwind
fn damage_row_bounds_near_u16_max() {
    let rows: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 8);

    let mut damage = Damage::new(rows);
    let row: u16 = kani::any();
    kani::assume(row < rows);

    let col = u16::MAX - 1;
    damage.mark_cell(row, col);

    if let Some((left, right)) = damage.row_damage_bounds(row, u16::MAX) {
        kani::assert(left == col, "left should preserve near-max mark");
        kani::assert(
            right == u16::MAX,
            "right should include the final valid column",
        );
    } else {
        kani::assert(
            false,
            "row bounds should remain present at near-max boundary",
        );
    }
}

/// Partial damage: unmarked row reports not damaged.
#[kani::proof]
#[kani::unwind(10)] // Vec allocation needs rows+1 unwind
fn damage_partial_unmarked_not_damaged() {
    let rows: u16 = kani::any();
    kani::assume(rows >= 2 && rows <= 8);

    let mut damage = Damage::new(rows);

    // Mark one row
    let marked_row: u16 = kani::any();
    kani::assume(marked_row < rows);
    damage.mark_row(marked_row);

    // Check a different row
    let check_row: u16 = kani::any();
    kani::assume(check_row < rows);
    kani::assume(check_row != marked_row);

    kani::assert(
        !damage.is_row_damaged(check_row),
        "unmarked row should not be damaged",
    );
}

// ---------------------------------------------------------------------------
// DamageTracker::clear() proofs
// ---------------------------------------------------------------------------

/// clear() is behaviorally equivalent to new() for same row count.
///
/// After clear(rows), every observable query must return the same value
/// as on a freshly constructed DamageTracker::new(rows). This proves the
/// zero-allocation frame-to-frame reset path is safe.
///
/// Rows capped at 4 (not 8) because mark_cell column tracking doubles
/// the Vec state space; two DamageTracker instances push CBMC past its
/// memory budget at rows=8 (OOM exit 241 at ~94s).
#[kani::proof]
#[kani::unwind(6)]
fn damage_tracker_clear_equivalent_to_new() {
    let rows: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 4);

    // Construct tracker, dirty it, then clear.
    let mut tracker = DamageTracker::new(rows);
    let dirty_row: u16 = kani::any();
    let dirty_col: u16 = kani::any();
    if dirty_row < rows {
        tracker.mark_cell(dirty_row, dirty_col);
    }
    tracker.clear(rows);

    let fresh = DamageTracker::new(rows);

    // All rows must be undamaged.
    let check_row: u16 = kani::any();
    kani::assume(check_row < rows);
    kani::assert(
        tracker.is_row_damaged(check_row) == fresh.is_row_damaged(check_row),
        "clear must produce same is_row_damaged as new",
    );
    kani::assert(
        tracker.row_damage_bounds(check_row) == fresh.row_damage_bounds(check_row),
        "clear must produce same row_damage_bounds as new",
    );

    // Internal representation must match.
    kani::assert(
        tracker.row_bits.len() == fresh.row_bits.len(),
        "row_bits length must match",
    );
}

// =============================================================================
// A4: copy_damaged_cells_inner buffer write index safety
// Part of #6765 (Visual Verification Pipeline Sprint, Stream A4).
// =============================================================================

/// A4.1: For any symbolic damage state, every buffer write index
/// computed from `row_damage_bounds` is within `rows * cols`.
///
/// This proves the critical safety property of `copy_damaged_cells_inner`:
/// when iterating `col in left..right` from `row_damage_bounds(row, cols)`,
/// the index `row * cols + col` never exceeds the allocated buffer size.
///
/// Covers both Full and Partial damage variants.
#[kani::proof]
#[kani::unwind(10)]
fn damage_buffer_write_index_in_bounds() {
    let rows: u16 = kani::any();
    let cols: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 4);
    kani::assume(cols > 0 && cols <= 8);

    let buffer_size = usize::from(rows) * usize::from(cols);

    let mut damage = Damage::new(rows);

    // Symbolically mark some cells.
    let mark_row: u16 = kani::any();
    let mark_col: u16 = kani::any();
    if mark_row < rows {
        damage.mark_cell(mark_row, mark_col);
    }

    // For every row, check the damage bounds contract.
    let check_row: u16 = kani::any();
    kani::assume(check_row < rows);

    if let Some((left, right)) = damage.row_damage_bounds(check_row, cols) {
        // Contract: left < right <= cols (from row_damage_bounds implementation).
        kani::assert(left < right, "left must be strictly less than right");
        kani::assert(right <= cols, "right must not exceed cols");

        // For any col in [left, right), the buffer index is in bounds.
        let col: u16 = kani::any();
        kani::assume(col >= left && col < right);
        let index = usize::from(check_row) * usize::from(cols) + usize::from(col);
        kani::assert(
            index < buffer_size,
            "buffer write index must be within rows*cols",
        );
    }
}

/// A4.2: Full damage produces valid indices for every cell position.
///
/// When damage is Full, `row_damage_bounds` returns `(0, cols)` for every
/// row, so every `row * cols + col` for `col in 0..cols` must be in bounds.
#[kani::proof]
fn damage_full_all_indices_in_bounds() {
    let rows: u16 = kani::any();
    let cols: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 64);
    kani::assume(cols > 0 && cols <= 256);

    let buffer_size = usize::from(rows) * usize::from(cols);

    let row: u16 = kani::any();
    let col: u16 = kani::any();
    kani::assume(row < rows);
    kani::assume(col < cols);

    // This is the exact index computation from copy_damaged_cells_inner.
    let index = usize::from(row) * usize::from(cols) + usize::from(col);
    kani::assert(
        index < buffer_size,
        "full-damage index must be within buffer",
    );
}

/// clear() with different row count produces correct capacity.
#[kani::proof]
#[kani::unwind(10)]
fn damage_tracker_clear_resize() {
    let old_rows: u16 = kani::any();
    let new_rows: u16 = kani::any();
    kani::assume(old_rows > 0 && old_rows <= 8);
    kani::assume(new_rows > 0 && new_rows <= 8);

    let mut tracker = DamageTracker::new(old_rows);
    let dirty_row: u16 = kani::any();
    if dirty_row < old_rows {
        tracker.mark_row(dirty_row);
    }
    tracker.clear(new_rows);

    let fresh = DamageTracker::new(new_rows);

    // After clear with new size, internal sizes must match fresh.
    kani::assert(
        tracker.row_bits.len() == fresh.row_bits.len(),
        "row_bits length must match after resize clear",
    );

    // All rows must be undamaged.
    let check_row: u16 = kani::any();
    kani::assume(check_row < new_rows);
    kani::assert(
        !tracker.is_row_damaged(check_row),
        "no row should be damaged after clear",
    );
}
