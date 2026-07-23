// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kani proofs for display-offset damage computation.

use super::*;

/// No panics for any bounded input combination, and result is consistent
/// with the None/Full/partial classification.
///
/// The function contains two `expect()` calls on `u16::try_from()` that
/// could panic if the delta or `rows - delta` exceeded u16. This proof
/// verifies neither path panics for all bounded inputs.
#[kani::proof]
fn display_offset_damage_no_panic() {
    let old_offset: usize = kani::any();
    let new_offset: usize = kani::any();
    let visible_rows: u16 = kani::any();

    // Bound offsets to keep CBMC tractable — the function's arithmetic
    // only depends on delta (|old-new|) vs visible_rows, so bounding
    // offsets to u16::MAX range covers all code paths.
    kani::assume(old_offset <= usize::from(u16::MAX));
    kani::assume(new_offset <= usize::from(u16::MAX));

    let result = compute_display_offset_damage(old_offset, new_offset, visible_rows);
    // Equal offsets must produce None; different offsets must not.
    if old_offset == new_offset {
        kani::assert(
            result == DisplayOffsetDamage::None,
            "equal offsets must produce None",
        );
    } else {
        kani::assert(
            result != DisplayOffsetDamage::None,
            "different offsets must not produce None",
        );
    }
}

/// None iff offsets are equal.
#[kani::proof]
fn display_offset_damage_none_iff_equal() {
    let old_offset: usize = kani::any();
    let new_offset: usize = kani::any();
    let visible_rows: u16 = kani::any();

    kani::assume(old_offset <= usize::from(u16::MAX));
    kani::assume(new_offset <= usize::from(u16::MAX));

    let result = compute_display_offset_damage(old_offset, new_offset, visible_rows);

    if old_offset == new_offset {
        kani::assert(
            result == DisplayOffsetDamage::None,
            "equal offsets must produce None",
        );
    } else {
        kani::assert(
            result != DisplayOffsetDamage::None,
            "unequal offsets must not produce None",
        );
    }
}

/// Full damage when delta >= visible_rows.
#[kani::proof]
fn display_offset_damage_full_when_delta_ge_rows() {
    let old_offset: usize = kani::any();
    let new_offset: usize = kani::any();
    let visible_rows: u16 = kani::any();

    kani::assume(old_offset <= usize::from(u16::MAX));
    kani::assume(new_offset <= usize::from(u16::MAX));
    kani::assume(old_offset != new_offset);

    let delta = if new_offset > old_offset {
        new_offset - old_offset
    } else {
        old_offset - new_offset
    };

    let result = compute_display_offset_damage(old_offset, new_offset, visible_rows);

    if delta >= usize::from(visible_rows) {
        kani::assert(
            result == DisplayOffsetDamage::Full,
            "delta >= rows must produce Full",
        );
    }
}

/// TopRows variant has valid bounds: 0 < n < visible_rows.
#[kani::proof]
fn display_offset_damage_top_rows_bounds() {
    let old_offset: usize = kani::any();
    let new_offset: usize = kani::any();
    let visible_rows: u16 = kani::any();

    kani::assume(old_offset <= usize::from(u16::MAX));
    kani::assume(new_offset <= usize::from(u16::MAX));

    let result = compute_display_offset_damage(old_offset, new_offset, visible_rows);

    if let DisplayOffsetDamage::TopRows(n) = result {
        kani::assert(n > 0, "TopRows count must be positive");
        kani::assert(
            n < visible_rows,
            "TopRows count must be less than visible_rows",
        );
    }
}

/// BottomRows variant has valid bounds: 0 < start < end == visible_rows.
#[kani::proof]
fn display_offset_damage_bottom_rows_bounds() {
    let old_offset: usize = kani::any();
    let new_offset: usize = kani::any();
    let visible_rows: u16 = kani::any();

    kani::assume(old_offset <= usize::from(u16::MAX));
    kani::assume(new_offset <= usize::from(u16::MAX));

    let result = compute_display_offset_damage(old_offset, new_offset, visible_rows);

    if let DisplayOffsetDamage::BottomRows { start, end } = result {
        kani::assert(start > 0, "BottomRows start must be positive");
        kani::assert(start < end, "BottomRows start must be less than end");
        kani::assert(
            end == visible_rows,
            "BottomRows end must equal visible_rows",
        );
    }
}

/// Damaged row count equals min(delta, visible_rows) for all variants.
#[kani::proof]
fn display_offset_damage_row_count_correct() {
    let old_offset: usize = kani::any();
    let new_offset: usize = kani::any();
    let visible_rows: u16 = kani::any();

    kani::assume(old_offset <= usize::from(u16::MAX));
    kani::assume(new_offset <= usize::from(u16::MAX));

    let delta = if new_offset > old_offset {
        new_offset - old_offset
    } else {
        old_offset - new_offset
    };

    let result = compute_display_offset_damage(old_offset, new_offset, visible_rows);
    let rows = usize::from(visible_rows);

    let damaged_count: usize = match result {
        DisplayOffsetDamage::None => 0,
        DisplayOffsetDamage::Full => rows,
        DisplayOffsetDamage::TopRows(n) => usize::from(n),
        DisplayOffsetDamage::BottomRows { start, end } => usize::from(end - start),
    };

    let expected = delta.min(rows);
    kani::assert(
        damaged_count == expected,
        "damaged row count must equal min(delta, visible_rows)",
    );
}
