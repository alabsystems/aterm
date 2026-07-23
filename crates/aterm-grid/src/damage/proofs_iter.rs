// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kani proofs for damage iteration correctness.
//!
//! The `BitsetRowIterator` is the core of the render hot path — it converts
//! a bitset of damaged rows into a sequence of row indices using
//! `trailing_zeros()`. These proofs verify the four fundamental properties:
//!
//! 1. **Soundness**: yielded rows have their bit set
//! 2. **Completeness**: set bits below max_row are yielded
//! 3. **Monotonicity**: consecutive yields are strictly increasing
//! 4. **Bound**: no yielded row >= max_row
//!
//! The `DamageBoundsIterator` wraps the row iterator to produce column
//! bounds for each damaged row. These proofs verify the bounds are valid.

use super::*;

// Constrain bitset to 8 bits (rows 0..7) and max 8 rows to keep
// CBMC tractable. The algorithm is word-level — behaviour for bits 0..7
// generalises to bits 0..63 within a word.

/// BitsetRowIterator: every yielded row has its bit set in the bitset.
///
/// Proves soundness — the iterator never reports a row as damaged
/// when its bit is clear.
#[kani::proof]
#[kani::unwind(10)]
fn bitset_iter_yields_only_set_bits() {
    let word: u64 = kani::any();
    kani::assume(word <= 0xFF); // 8-bit range
    let bits = [word];
    let max_row: u16 = kani::any();
    kani::assume(max_row > 0 && max_row <= 8);

    let mut iter = BitsetRowIterator::new(&bits, max_row);
    if let Some(row) = iter.next() {
        kani::assert(
            (word >> (row as u64)) & 1 == 1,
            "yielded row must have its bit set in the bitset",
        );
        kani::assert(row < max_row, "yielded row must be below max_row");
    }
}

/// BitsetRowIterator: consecutive yields are strictly increasing.
///
/// Proves monotonicity — the iterator visits rows in ascending order.
/// Combined with soundness, this proves no row is yielded twice.
#[kani::proof]
#[kani::unwind(10)]
fn bitset_iter_monotonic() {
    let word: u64 = kani::any();
    kani::assume(word <= 0xFF);
    let bits = [word];
    let max_row: u16 = kani::any();
    kani::assume(max_row > 0 && max_row <= 8);

    let mut iter = BitsetRowIterator::new(&bits, max_row);
    if let Some(first) = iter.next() {
        if let Some(second) = iter.next() {
            kani::assert(
                second > first,
                "consecutive yields must be strictly increasing",
            );
        }
    }
}

/// BitsetRowIterator: a set bit below max_row is always yielded.
///
/// Marks exactly one bit and proves the iterator produces exactly
/// one element matching that bit position.
#[kani::proof]
#[kani::unwind(10)]
fn bitset_iter_completeness_single_bit() {
    let target_bit: u16 = kani::any();
    kani::assume(target_bit < 8);

    let word: u64 = 1u64 << (target_bit as u64);
    let bits = [word];
    let max_row: u16 = kani::any();
    kani::assume(max_row > target_bit); // bit is below max_row
    kani::assume(max_row <= 8);

    let mut iter = BitsetRowIterator::new(&bits, max_row);
    let yielded = iter.next();
    kani::assert(yielded.is_some(), "set bit below max_row must be yielded");
    kani::assert(
        yielded.unwrap() == target_bit,
        "yielded value must match the set bit position",
    );
    // Only one bit set — iterator should be exhausted.
    kani::assert(iter.next().is_none(), "no more values after single set bit");
}

/// BitsetRowIterator: set bits at or above max_row are NOT yielded.
///
/// Proves the upper bound is respected even when bits are set beyond it.
#[kani::proof]
#[kani::unwind(10)]
fn bitset_iter_respects_max_row_bound() {
    let target_bit: u16 = kani::any();
    kani::assume(target_bit < 8);

    let max_row: u16 = kani::any();
    kani::assume(max_row <= target_bit); // bit is at or above max_row

    let word: u64 = 1u64 << (target_bit as u64);
    let bits = [word];

    let mut iter = BitsetRowIterator::new(&bits, max_row);
    kani::assert(
        iter.next().is_none(),
        "set bit at or above max_row must not be yielded",
    );
}

/// BitsetRowIterator: empty bitset yields nothing.
#[kani::proof]
fn bitset_iter_empty_yields_nothing() {
    let max_row: u16 = kani::any();
    kani::assume(max_row <= 64);

    let bits = [0u64];
    let mut iter = BitsetRowIterator::new(&bits, max_row);
    kani::assert(iter.next().is_none(), "empty bitset must yield nothing");
}

/// BitsetRowIterator: count of yielded rows equals popcount of
/// bits below max_row.
///
/// Exhaustively iterates the full output and verifies the total count
/// matches the number of set bits in the valid range.
#[kani::proof]
#[kani::unwind(10)]
fn bitset_iter_count_matches_popcount() {
    let word: u64 = kani::any();
    kani::assume(word <= 0xFF);
    let bits = [word];
    let max_row: u16 = kani::any();
    kani::assume(max_row > 0 && max_row <= 8);

    // Compute expected count: bits set in positions 0..max_row
    let mask = (1u64 << (max_row as u64)) - 1;
    let expected = (word & mask).count_ones();

    let mut iter = BitsetRowIterator::new(&bits, max_row);
    let mut count: u32 = 0;
    while let Some(_) = iter.next() {
        count += 1;
    }
    kani::assert(
        count == expected,
        "yielded count must equal popcount of bits below max_row",
    );
}

/// DamageBoundsIterator: yielded bounds have left < right (non-empty).
///
/// After marking a cell, the iterator must only produce non-empty bounds.
///
/// Rows capped at 4, cols at 16: Damage + column tracking + iterator
/// state exceeds CBMC memory budget at rows=8/cols=100 (OOM exit 241).
/// The algorithm is row/col-independent so smaller bounds cover all paths.
#[kani::proof]
#[kani::unwind(6)]
fn damage_bounds_iter_yields_nonempty_bounds() {
    let rows: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 4);

    let cols: u16 = kani::any();
    kani::assume(cols > 0 && cols <= 16);

    let mut damage = Damage::new(rows);

    let mark_row: u16 = kani::any();
    let mark_col: u16 = kani::any();
    kani::assume(mark_row < rows);

    damage.mark_cell(mark_row, mark_col);

    let mut iter = damage.iter_bounds(rows, cols);
    if let Some(bounds) = iter.next() {
        kani::assert(
            bounds.left < bounds.right,
            "yielded bounds must be non-empty",
        );
        kani::assert(bounds.line < rows, "yielded row must be in range");
        kani::assert(bounds.right <= cols, "right bound must be clamped to cols");
    }
}

/// DamageBoundsIterator: a cell marked within visible range is yielded.
///
/// This is the completeness property for the bounds iterator —
/// if a cell is marked and is within the visible column range,
/// the iterator must yield bounds covering that cell.
///
/// Rows capped at 4, cols at 16: same CBMC budget constraint as
/// damage_bounds_iter_yields_nonempty_bounds (OOM exit 241 at higher).
#[kani::proof]
#[kani::unwind(6)]
fn damage_bounds_iter_yields_marked_cell() {
    let rows: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 4);

    let cols: u16 = kani::any();
    kani::assume(cols > 0 && cols <= 16);

    let mut damage = Damage::new(rows);

    let mark_row: u16 = kani::any();
    let mark_col: u16 = kani::any();
    kani::assume(mark_row < rows);
    kani::assume(mark_col < cols); // within visible range

    damage.mark_cell(mark_row, mark_col);

    let mut iter = damage.iter_bounds(rows, cols);
    let first = iter.next();
    kani::assert(
        first.is_some(),
        "marked cell in visible range must produce bounds",
    );

    let bounds = first.unwrap();
    kani::assert(bounds.line == mark_row, "bounds must be for the marked row");
    kani::assert(
        bounds.left <= mark_col,
        "left bound must cover marked column",
    );
    kani::assert(
        bounds.right > mark_col,
        "right bound must be past marked column",
    );
}

/// DamageBoundsIterator: marking a cell outside visible columns
/// yields nothing for that row.
///
/// When a cell is marked at col >= cols, the bounds iterator should
/// filter it out because after clamping, left >= right.
///
/// Rows capped at 4, cols at 16: same CBMC budget constraint as
/// damage_bounds_iter_yields_nonempty_bounds (OOM exit 241 at higher).
#[kani::proof]
#[kani::unwind(6)]
fn damage_bounds_iter_filters_out_of_range_columns() {
    let rows: u16 = kani::any();
    kani::assume(rows > 0 && rows <= 4);

    let cols: u16 = kani::any();
    kani::assume(cols > 0 && cols <= 16);

    let mut damage = Damage::new(rows);

    let mark_row: u16 = kani::any();
    kani::assume(mark_row < rows);

    // Mark a cell beyond the visible column range.
    let mark_col: u16 = kani::any();
    kani::assume(mark_col >= cols);

    damage.mark_cell(mark_row, mark_col);

    let mut iter = damage.iter_bounds(rows, cols);
    // The bounds for this row should be filtered: left = min(mark_col, cols) = cols,
    // right = min(mark_col+1, cols) = cols, so left >= right → None.
    kani::assert(
        iter.next().is_none(),
        "cell beyond visible range must not produce bounds",
    );
}
