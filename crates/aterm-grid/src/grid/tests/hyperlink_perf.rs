// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `row_has_hyperlinks` correctness proof across all mutation types.
//!
//! Proves that `row_has_hyperlinks` returns correct results after every
//! mutation type. Filed as part of performance_proofs phase.

use super::*;
use std::sync::Arc;

fn test_url() -> Arc<str> {
    Arc::from("https://test.com")
}

/// After `get_or_create` (most common mutation), result is correct.
#[test]
fn correct_after_get_or_create() {
    let url = test_url();
    let mut extras = CellExtras::new();

    // Empty: no hyperlinks on any row
    assert!(!extras.row_has_hyperlinks(0));
    assert!(!extras.row_has_hyperlinks(5));

    // Add hyperlink to row 5
    extras
        .get_or_create(CellCoord::new(5, 0))
        .set_hyperlink(Some(url.clone()));
    assert!(extras.row_has_hyperlinks(5));
    assert!(!extras.row_has_hyperlinks(0));
    assert!(!extras.row_has_hyperlinks(4));
    assert!(!extras.row_has_hyperlinks(6));

    // Add non-hyperlink extra to row 3 (combining mark)
    extras
        .get_or_create(CellCoord::new(3, 0))
        .add_combining('\u{0301}');
    // Row 3 has extras but NOT hyperlinks
    assert!(!extras.row_has_hyperlinks(3));
    assert!(extras.row_has_hyperlinks(5));
}

/// After `clear_row`, reflects removal.
#[test]
fn correct_after_clear_row() {
    let url = test_url();
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(5, 0))
        .set_hyperlink(Some(url.clone()));
    extras
        .get_or_create(CellCoord::new(5, 10))
        .set_hyperlink(Some(url.clone()));
    extras
        .get_or_create(CellCoord::new(8, 0))
        .set_hyperlink(Some(url));

    assert!(extras.row_has_hyperlinks(5));
    assert!(extras.row_has_hyperlinks(8));

    extras.clear_row(5);
    assert!(!extras.row_has_hyperlinks(5));
    assert!(extras.row_has_hyperlinks(8));
}

/// After `clear_rows` (batch), reflects range removal.
#[test]
fn correct_after_clear_rows() {
    let url = test_url();
    let mut extras = CellExtras::new();

    for row in 0..10u16 {
        extras
            .get_or_create(CellCoord::new(row, 0))
            .set_hyperlink(Some(url.clone()));
    }

    // Clear rows 3..7
    extras.clear_rows(3..7);

    for row in 0..3 {
        assert!(extras.row_has_hyperlinks(row), "row {row} should survive");
    }
    for row in 3..7 {
        assert!(
            !extras.row_has_hyperlinks(row),
            "row {row} should be cleared"
        );
    }
    for row in 7..10 {
        assert!(extras.row_has_hyperlinks(row), "row {row} should survive");
    }
}

/// After `shift_rows_up_by`, reflects shifted positions.
#[test]
fn correct_after_shift_rows_up() {
    let url = test_url();
    let mut extras = CellExtras::new();

    // Hyperlink on row 10
    extras
        .get_or_create(CellCoord::new(10, 0))
        .set_hyperlink(Some(url));

    assert!(extras.row_has_hyperlinks(10));
    assert!(!extras.row_has_hyperlinks(5));

    // Shift all rows up by 5 (rows 0..5 discarded, row 10 → row 5)
    extras.shift_rows_up_by(0, 5);

    assert!(
        !extras.row_has_hyperlinks(10),
        "old position should be empty"
    );
    assert!(extras.row_has_hyperlinks(5), "shifted to row 5");
}

/// After `set` with empty extra, reflects removal.
#[test]
fn correct_after_set_empty() {
    let url = test_url();
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(5, 0))
        .set_hyperlink(Some(url));
    assert!(extras.row_has_hyperlinks(5));

    // Setting an empty CellExtra removes the entry
    extras.set(CellCoord::new(5, 0), CellExtra::default());
    assert!(
        !extras.row_has_hyperlinks(5),
        "empty extra should remove hyperlink"
    );
}

/// Correct across mixed mutations (add, combine, clear).
#[test]
fn correct_across_mixed_mutations() {
    let url = test_url();
    let mut extras = CellExtras::new();

    // Populate hyperlinks on rows 2, 5, 8, 12
    for row in [2u16, 5, 8, 12] {
        extras
            .get_or_create(CellCoord::new(row, 0))
            .set_hyperlink(Some(url.clone()));
    }
    let initial: Vec<bool> = (0..15).map(|r| extras.row_has_hyperlinks(r)).collect();

    // Mutation that doesn't change hyperlinks (combining mark on row 2)
    extras
        .get_or_create(CellCoord::new(2, 5))
        .add_combining('\u{0301}');
    let after_combine: Vec<bool> = (0..15).map(|r| extras.row_has_hyperlinks(r)).collect();
    assert_eq!(
        initial, after_combine,
        "non-hyperlink mutation should not change results"
    );

    // Clear row 5
    extras.clear_row(5);
    let after_clear: Vec<bool> = (0..15).map(|r| extras.row_has_hyperlinks(r)).collect();
    assert!(!after_clear[5], "row 5 should be cleared");
    assert!(after_clear[8], "row 8 should survive");
}
