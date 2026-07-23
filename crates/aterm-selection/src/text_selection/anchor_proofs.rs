// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for `SelectionAnchor` ordering and constructors (INV-SEL-19 through INV-SEL-28).
//!
//! These are the non-tautological anchor proofs retained from the original
//! text-selection verification batch after removing derive-only checks (#2740).

use super::*;

// ============================================================================
// SelectionAnchor Proofs (INV-SEL-19 through INV-SEL-28)
// ============================================================================

/// INV-SEL-19: SelectionAnchor::new creates anchor with given values
#[kani::proof]
fn anchor_new_creates_with_values() {
    let row: i32 = kani::any();
    let col: u16 = kani::any();

    kani::assume(row > -1000 && row < 1000);
    kani::assume(col < 1000);

    let anchor = SelectionAnchor::new(row, col, SelectionSide::Left);

    kani::assert(anchor.row == row, "assertion");
    kani::assert(anchor.col == col, "assertion");
    kani::assert(anchor.side == SelectionSide::Left, "assertion");
}

/// INV-SEL-20: SelectionAnchor::left creates anchor with Left side
#[kani::proof]
fn anchor_left_creates_left_side() {
    let row: i32 = kani::any();
    let col: u16 = kani::any();

    kani::assume(row > -1000 && row < 1000);
    kani::assume(col < 1000);

    let anchor = SelectionAnchor::left(row, col);

    kani::assert(anchor.row == row, "assertion");
    kani::assert(anchor.col == col, "assertion");
    kani::assert(anchor.side == SelectionSide::Left, "assertion");
}

/// INV-SEL-21: SelectionAnchor::right creates anchor with Right side
#[kani::proof]
fn anchor_right_creates_right_side() {
    let row: i32 = kani::any();
    let col: u16 = kani::any();

    kani::assume(row > -1000 && row < 1000);
    kani::assume(col < 1000);

    let anchor = SelectionAnchor::right(row, col);

    kani::assert(anchor.row == row, "assertion");
    kani::assert(anchor.col == col, "assertion");
    kani::assert(anchor.side == SelectionSide::Right, "assertion");
}

/// INV-SEL-23: SelectionAnchor Ord is reflexive
#[kani::proof]
fn anchor_ord_reflexive() {
    let row: i32 = kani::any();
    let col: u16 = kani::any();

    kani::assume(row > -100 && row < 100);
    kani::assume(col < 100);

    let anchor = SelectionAnchor::new(row, col, SelectionSide::Left);

    kani::assert(anchor == anchor, "assertion");
    kani::assert(anchor <= anchor, "assertion");
    kani::assert(anchor >= anchor, "assertion");
}

/// INV-SEL-24: SelectionAnchor Ord: row takes priority
#[kani::proof]
fn anchor_ord_row_priority() {
    let a1 = SelectionAnchor::new(0, 100, SelectionSide::Right);
    let a2 = SelectionAnchor::new(1, 0, SelectionSide::Left);

    // Even though col(a1) > col(a2), row(a1) < row(a2) so a1 < a2
    kani::assert(a1 < a2, "assertion");
}

/// INV-SEL-25: SelectionAnchor Ord: col secondary to row
#[kani::proof]
fn anchor_ord_col_secondary() {
    let a1 = SelectionAnchor::new(5, 10, SelectionSide::Left);
    let a2 = SelectionAnchor::new(5, 20, SelectionSide::Left);

    // Same row, different col
    kani::assert(a1 < a2, "assertion");
    kani::assert(a2 > a1, "assertion");
}

/// INV-SEL-26: SelectionAnchor Ord: side tertiary to col
#[kani::proof]
fn anchor_ord_side_tertiary() {
    let a1 = SelectionAnchor::new(5, 10, SelectionSide::Left);
    let a2 = SelectionAnchor::new(5, 10, SelectionSide::Right);

    // Same row and col, different side
    kani::assert(a1 < a2, "assertion");
    kani::assert(a2 > a1, "assertion");
}

/// INV-SEL-27: SelectionAnchor Ord is antisymmetric
#[kani::proof]
fn anchor_ord_antisymmetric() {
    let row1: i32 = kani::any();
    let col1: u16 = kani::any();
    let row2: i32 = kani::any();
    let col2: u16 = kani::any();

    kani::assume(row1 >= -10 && row1 <= 10);
    kani::assume(row2 >= -10 && row2 <= 10);
    kani::assume(col1 <= 20);
    kani::assume(col2 <= 20);

    let a1 = SelectionAnchor::new(row1, col1, SelectionSide::Left);
    let a2 = SelectionAnchor::new(row2, col2, SelectionSide::Left);

    // Antisymmetric: if a <= b and b <= a, then a == b
    if a1 <= a2 && a2 <= a1 {
        kani::assert(a1 == a2, "assertion");
    }
}

/// INV-SEL-28: SelectionAnchor Ord is transitive
#[kani::proof]
fn anchor_ord_transitive() {
    let row1: i32 = kani::any();
    let col1: u16 = kani::any();
    let row2: i32 = kani::any();
    let col2: u16 = kani::any();
    let row3: i32 = kani::any();
    let col3: u16 = kani::any();

    kani::assume(row1 >= -5 && row1 <= 5);
    kani::assume(row2 >= -5 && row2 <= 5);
    kani::assume(row3 >= -5 && row3 <= 5);
    kani::assume(col1 <= 10);
    kani::assume(col2 <= 10);
    kani::assume(col3 <= 10);

    let a1 = SelectionAnchor::new(row1, col1, SelectionSide::Left);
    let a2 = SelectionAnchor::new(row2, col2, SelectionSide::Left);
    let a3 = SelectionAnchor::new(row3, col3, SelectionSide::Left);

    // Transitive: if a <= b and b <= c, then a <= c
    if a1 <= a2 && a2 <= a3 {
        kani::assert(a1 <= a3, "assertion");
    }
}
