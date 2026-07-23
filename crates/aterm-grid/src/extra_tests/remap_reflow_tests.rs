// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for `CellExtras::remap_reflow()` — coordinate remapping during
//! column resize. Ensures hyperlinks, RGB colors, combining marks, and
//! complex chars survive reflow operations (#5803, #3977).

use std::sync::Arc;

use super::*;

// =============================================================================
// Identity remap (no column change)
// =============================================================================

#[test]
fn remap_reflow_identity_preserves_all_extras() {
    let mut extras = CellExtras::new();

    // Hyperlink at (0, 2)
    extras
        .get_or_create(CellCoord::new(0, 2))
        .set_hyperlink(Some(Arc::from("https://example.com")));

    // RGB foreground at (1, 5)
    extras
        .get_or_create(CellCoord::new(1, 5))
        .set_fg_rgb(Some([255, 128, 0]));

    // Combining mark at (2, 0)
    extras
        .get_or_create(CellCoord::new(2, 0))
        .add_combining('\u{0301}'); // acute accent

    let identity = |row: u16, col: u16| Some(CellCoord::new(row, col));
    extras.remap_reflow(identity, 100);

    assert_eq!(extras.len(), 3);
    assert!(
        extras
            .get(CellCoord::new(0, 2))
            .unwrap()
            .hyperlink()
            .is_some()
    );
    assert_eq!(
        extras.get(CellCoord::new(1, 5)).unwrap().fg_rgb(),
        Some([255, 128, 0])
    );
    assert_eq!(
        extras.get(CellCoord::new(2, 0)).unwrap().combining(),
        &['\u{0301}']
    );
}

// =============================================================================
// Column grow: e.g. 40 cols -> 80 cols, row 1 content moves to row 0
// =============================================================================

#[test]
fn remap_reflow_column_grow_remaps_coordinates() {
    let mut extras = CellExtras::new();

    // Hyperlink on row 0 col 10
    extras
        .get_or_create(CellCoord::new(0, 10))
        .set_hyperlink(Some(Arc::from("https://grow.test")));

    // Extra on row 1 col 5 — after grow, mapped to row 0 col 45
    extras
        .get_or_create(CellCoord::new(1, 5))
        .set_fg_rgb(Some([10, 20, 30]));

    // Simulate 40->80 column grow: old row 1 col C maps to row 0 col 40+C
    let grow_remap = |row: u16, col: u16| -> Option<CellCoord> {
        if row == 0 {
            Some(CellCoord::new(0, col))
        } else if row == 1 {
            Some(CellCoord::new(0, 40 + col))
        } else {
            None
        }
    };
    extras.remap_reflow(grow_remap, 100);

    assert_eq!(extras.len(), 2);
    // Row 0 col 10 stays
    assert!(
        extras
            .get(CellCoord::new(0, 10))
            .unwrap()
            .hyperlink()
            .is_some()
    );
    // Row 1 col 5 -> Row 0 col 45
    assert_eq!(
        extras.get(CellCoord::new(0, 45)).unwrap().fg_rgb(),
        Some([10, 20, 30])
    );
    // Old coordinate should be gone
    assert!(extras.get(CellCoord::new(1, 5)).is_none());
}

// =============================================================================
// Column shrink: e.g. 80 cols -> 40 cols, content wraps to next row
// =============================================================================

#[test]
fn remap_reflow_column_shrink_wraps_to_new_rows() {
    let mut extras = CellExtras::new();

    // RGB color at row 0 col 60 — after shrink to 40 cols, wraps to row 1 col 20
    extras
        .get_or_create(CellCoord::new(0, 60))
        .set_bg_rgb(Some([100, 200, 50]));

    // Underline color at row 0 col 10 — stays on row 0
    extras
        .get_or_create(CellCoord::new(0, 10))
        .set_underline_color(Some([255, 0, 0]));

    let shrink_remap = |row: u16, col: u16| -> Option<CellCoord> {
        if row == 0 {
            let new_row = col / 40;
            let new_col = col % 40;
            Some(CellCoord::new(new_row, new_col))
        } else {
            // Rows beyond 0 shift down by how many rows row 0 produced
            Some(CellCoord::new(row + 1, col))
        }
    };
    extras.remap_reflow(shrink_remap, 100);

    assert_eq!(extras.len(), 2);
    // Row 0 col 10 -> row 0 col 10
    assert_eq!(
        extras.get(CellCoord::new(0, 10)).unwrap().underline_color(),
        Some([255, 0, 0])
    );
    // Row 0 col 60 -> row 1 col 20
    assert_eq!(
        extras.get(CellCoord::new(1, 20)).unwrap().bg_rgb(),
        Some([100, 200, 50])
    );
}

// =============================================================================
// Remap that drops some extras (returns None)
// =============================================================================

#[test]
fn remap_reflow_drops_extras_when_remap_returns_none() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .set_fg_rgb(Some([1, 2, 3]));
    extras
        .get_or_create(CellCoord::new(0, 5))
        .set_fg_rgb(Some([4, 5, 6]));
    extras
        .get_or_create(CellCoord::new(1, 0))
        .set_fg_rgb(Some([7, 8, 9]));

    // Drop row 0 col 5, keep everything else
    let selective_drop = |row: u16, col: u16| -> Option<CellCoord> {
        if row == 0 && col == 5 {
            None
        } else {
            Some(CellCoord::new(row, col))
        }
    };
    extras.remap_reflow(selective_drop, 100);

    assert_eq!(extras.len(), 2);
    assert!(extras.get(CellCoord::new(0, 0)).is_some());
    assert!(extras.get(CellCoord::new(0, 5)).is_none()); // dropped
    assert!(extras.get(CellCoord::new(1, 0)).is_some());
}

// =============================================================================
// max_row truncation
// =============================================================================

#[test]
fn remap_reflow_truncates_extras_beyond_max_row() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .set_fg_rgb(Some([1, 1, 1]));
    extras
        .get_or_create(CellCoord::new(5, 0))
        .set_fg_rgb(Some([2, 2, 2]));
    extras
        .get_or_create(CellCoord::new(10, 0))
        .set_fg_rgb(Some([3, 3, 3]));

    let identity = |row: u16, col: u16| Some(CellCoord::new(row, col));
    extras.remap_reflow(identity, 6); // max_row=6, so row 10 is truncated

    assert_eq!(extras.len(), 2);
    assert!(extras.get(CellCoord::new(0, 0)).is_some());
    assert!(extras.get(CellCoord::new(5, 0)).is_some());
    assert!(extras.get(CellCoord::new(10, 0)).is_none()); // truncated
}

#[test]
fn remap_reflow_truncates_extras_at_exact_max_row() {
    let mut extras = CellExtras::new();

    // Extra at row 5 — should be dropped when max_row=5 (row < max_row required)
    extras
        .get_or_create(CellCoord::new(5, 0))
        .set_fg_rgb(Some([1, 1, 1]));
    // Extra at row 4 — should survive
    extras
        .get_or_create(CellCoord::new(4, 0))
        .set_fg_rgb(Some([2, 2, 2]));

    let identity = |row: u16, col: u16| Some(CellCoord::new(row, col));
    extras.remap_reflow(identity, 5);

    assert_eq!(extras.len(), 1);
    assert!(extras.get(CellCoord::new(4, 0)).is_some());
    assert!(extras.get(CellCoord::new(5, 0)).is_none()); // at max_row, dropped
}

// =============================================================================
// Empty extras is no-op
// =============================================================================

#[test]
fn remap_reflow_empty_is_noop() {
    let mut extras = CellExtras::new();

    // Use AtomicBool since remap_reflow takes Fn, not FnMut
    let called = std::sync::atomic::AtomicBool::new(false);
    extras.remap_reflow(
        |_row, _col| {
            called.store(true, std::sync::atomic::Ordering::Relaxed);
            Some(CellCoord::new(0, 0))
        },
        100,
    );

    assert!(
        !called.load(std::sync::atomic::Ordering::Relaxed),
        "remap_fn should not be called on empty extras"
    );
    assert!(extras.is_empty());
}

// =============================================================================
// All extra types survive remap
// =============================================================================

#[test]
fn remap_reflow_preserves_hyperlink_with_id() {
    let mut extras = CellExtras::new();

    let extra = extras.get_or_create(CellCoord::new(0, 0));
    extra.set_hyperlink(Some(Arc::from("https://test.com")));
    extra.set_hyperlink_id(Some(Arc::from("link-42")));

    // Move to new coordinate
    let remap = |_row: u16, _col: u16| Some(CellCoord::new(3, 7));
    extras.remap_reflow(remap, 100);

    assert_eq!(extras.len(), 1);
    let remapped = extras.get(CellCoord::new(3, 7)).unwrap();
    assert_eq!(remapped.hyperlink().unwrap().as_ref(), "https://test.com");
    assert_eq!(remapped.hyperlink_id().unwrap().as_ref(), "link-42");
}

#[test]
fn remap_reflow_preserves_all_rgb_channels() {
    let mut extras = CellExtras::new();

    let extra = extras.get_or_create(CellCoord::new(0, 0));
    extra.set_fg_rgb(Some([10, 20, 30]));
    extra.set_bg_rgb(Some([40, 50, 60]));
    extra.set_underline_color(Some([70, 80, 90]));

    let remap = |_row: u16, _col: u16| Some(CellCoord::new(1, 1));
    extras.remap_reflow(remap, 100);

    let remapped = extras.get(CellCoord::new(1, 1)).unwrap();
    assert_eq!(remapped.fg_rgb(), Some([10, 20, 30]));
    assert_eq!(remapped.bg_rgb(), Some([40, 50, 60]));
    assert_eq!(remapped.underline_color(), Some([70, 80, 90]));
}

#[test]
fn remap_reflow_preserves_combining_marks() {
    let mut extras = CellExtras::new();

    let extra = extras.get_or_create(CellCoord::new(2, 3));
    extra.add_combining('\u{0301}'); // acute accent
    extra.add_combining('\u{0308}'); // diaeresis

    let remap = |_row: u16, _col: u16| Some(CellCoord::new(0, 0));
    extras.remap_reflow(remap, 100);

    let remapped = extras.get(CellCoord::new(0, 0)).unwrap();
    assert_eq!(remapped.combining(), &['\u{0301}', '\u{0308}']);
}

#[test]
fn remap_reflow_preserves_complex_char() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .set_complex_char(Some(Arc::from("👨‍👩‍👧‍👦")));

    let remap = |_row: u16, _col: u16| Some(CellCoord::new(5, 10));
    extras.remap_reflow(remap, 100);

    let remapped = extras.get(CellCoord::new(5, 10)).unwrap();
    assert_eq!(remapped.complex_char().unwrap().as_ref(), "👨‍👩‍👧‍👦");
}

#[test]
fn remap_reflow_preserves_extended_flags() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .set_extended_flags(0x1ABC);

    let remap = |_row: u16, _col: u16| Some(CellCoord::new(2, 2));
    extras.remap_reflow(remap, 100);

    let remapped = extras.get(CellCoord::new(2, 2)).unwrap();
    assert_eq!(remapped.extended_flags(), 0x1ABC);
}

// =============================================================================
// Remap drops all extras
// =============================================================================

#[test]
fn remap_reflow_drop_all_yields_empty() {
    let mut extras = CellExtras::new();

    for col in 0..10 {
        extras
            .get_or_create(CellCoord::new(0, col))
            .set_fg_rgb(Some([col as u8, 0, 0]));
    }
    assert_eq!(extras.len(), 10);

    // Drop everything
    extras.remap_reflow(|_row, _col| None, 100);

    assert!(extras.is_empty());
}

// =============================================================================
// Multiple extras with mixed types across rows
// =============================================================================

#[test]
fn remap_reflow_mixed_extras_across_rows() {
    let mut extras = CellExtras::new();

    // Row 0: hyperlink + fg color
    let e0 = extras.get_or_create(CellCoord::new(0, 0));
    e0.set_hyperlink(Some(Arc::from("https://row0.test")));
    e0.set_fg_rgb(Some([255, 0, 0]));

    // Row 1: bg color + combining
    let e1 = extras.get_or_create(CellCoord::new(1, 3));
    e1.set_bg_rgb(Some([0, 255, 0]));
    e1.add_combining('\u{0300}'); // grave accent

    // Row 2: underline + complex char
    let e2 = extras.get_or_create(CellCoord::new(2, 7));
    e2.set_underline_color(Some([0, 0, 255]));
    e2.set_complex_char(Some(Arc::from("é")));

    // Remap: shift all up by 1, drop row 0
    let remap = |row: u16, col: u16| -> Option<CellCoord> {
        if row == 0 {
            None
        } else {
            Some(CellCoord::new(row - 1, col))
        }
    };
    extras.remap_reflow(remap, 100);

    assert_eq!(extras.len(), 2);

    // Row 0 dropped
    assert!(extras.get(CellCoord::new(0, 0)).is_none());

    // Row 1 -> Row 0
    let r0 = extras.get(CellCoord::new(0, 3)).unwrap();
    assert_eq!(r0.bg_rgb(), Some([0, 255, 0]));
    assert_eq!(r0.combining(), &['\u{0300}']);

    // Row 2 -> Row 1
    let r1 = extras.get(CellCoord::new(1, 7)).unwrap();
    assert_eq!(r1.underline_color(), Some([0, 0, 255]));
    assert_eq!(r1.complex_char().unwrap().as_ref(), "é");
}

// =============================================================================
// Hyperlinks follow remap
// =============================================================================

#[test]
fn remap_reflow_moves_hyperlinks() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .set_hyperlink(Some(Arc::from("https://cache.test")));

    assert!(extras.row_has_hyperlinks(0));

    // Move hyperlink from row 0 to row 5
    let remap = |_row: u16, col: u16| Some(CellCoord::new(5, col));
    extras.remap_reflow(remap, 100);

    // After remap, row 0 should no longer have hyperlinks
    assert!(!extras.row_has_hyperlinks(0));
    assert!(extras.row_has_hyperlinks(5));
}

// =============================================================================
// Coordinate collision: two extras map to the same new coordinate
// =============================================================================

#[test]
fn remap_reflow_last_wins_on_coordinate_collision() {
    let mut extras = CellExtras::new();

    extras
        .get_or_create(CellCoord::new(0, 0))
        .set_fg_rgb(Some([1, 1, 1]));
    extras
        .get_or_create(CellCoord::new(1, 0))
        .set_fg_rgb(Some([2, 2, 2]));

    // Both map to the same destination — HashMap insert order determines winner
    let collapse = |_row: u16, _col: u16| Some(CellCoord::new(0, 0));
    extras.remap_reflow(collapse, 100);

    // Should have exactly one entry (the second insert overwrites the first)
    assert_eq!(extras.len(), 1);
    assert!(extras.get(CellCoord::new(0, 0)).is_some());
}
