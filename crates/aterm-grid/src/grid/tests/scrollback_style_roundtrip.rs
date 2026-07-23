// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! StyleId color preservation in scrollback round-trips (#5890).
//!
//! Verifies that cells using StyleId (indirect color via StyleTable) and
//! RGB overflow cells both preserve their colors through the scrollback
//! extraction → line conversion pipeline.

use crate::{Cell, CellFlags, Color, Grid, PackedColor, Style, StyleAttrs, StyleId};

/// Regression test for #5890: StyleId cells must preserve their actual colors
/// through the scrollback extraction + line conversion round-trip.
/// Before fix: StyleId cells returned placeholder black (0,0,0) from the
/// deprecated fg()/bg() accessors, corrupting scrollback colors.
#[test]
fn style_id_cells_preserve_colors_in_scrollback_roundtrip() {
    let mut grid = Grid::new(3, 10);

    // Intern a style with distinctive colors: bright red fg, blue bg
    let style = Style {
        fg: Color::new(255, 0, 0), // Bright red
        bg: Color::new(0, 0, 255), // Blue
        attrs: StyleAttrs::BOLD,
    };
    let style_id = grid.styles_mut().intern(style);

    // Create a cell using this StyleId
    let cell = Cell::with_style_id('S', style_id, CellFlags::BOLD);
    assert!(cell.uses_style_id(), "cell should use StyleId");

    grid.row_mut(0).unwrap().set(0, cell);
    // Also place a normal indexed cell for comparison
    let indexed_cell = Cell::with_style(
        'I',
        PackedColor::indexed(2), // Green
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    grid.row_mut(0).unwrap().set(1, indexed_cell);

    let row = grid.row(0).unwrap();

    // Extract extras with style table access — this resolves StyleId to RGB
    let extracted = Grid::extract_row_extras(row, grid.extras(), 0, grid.styles());

    // Verify extraction stored the resolved colors for the StyleId cell
    assert!(
        !extracted.rgb_fg.is_empty(),
        "StyleId cell should have resolved fg in extras"
    );
    assert_eq!(extracted.rgb_fg[0].0, 0, "fg should be at column 0");
    assert_eq!(
        extracted.rgb_fg[0].1,
        [255, 0, 0],
        "fg should be bright red"
    );
    assert_eq!(extracted.rgb_bg[0].0, 0, "bg should be at column 0");
    assert_eq!(extracted.rgb_bg[0].1, [0, 0, 255], "bg should be blue");

    // Convert to Line — should use the stored colors, not placeholder black
    let line = Grid::row_to_line_with_stored_extras(row, &extracted);

    // Verify the StyleId cell's colors survived
    let attr_s = line.get_attr(0);
    let expected_fg = PackedColor::rgb(255, 0, 0).0;
    let expected_bg = PackedColor::rgb(0, 0, 255).0;
    assert_eq!(
        attr_s.fg, expected_fg,
        "StyleId cell fg should be red (0x{expected_fg:08X}), got 0x{:08X}",
        attr_s.fg,
    );
    assert_eq!(
        attr_s.bg, expected_bg,
        "StyleId cell bg should be blue (0x{expected_bg:08X}), got 0x{:08X}",
        attr_s.bg,
    );

    // Verify the indexed cell still works correctly (no regression)
    let attr_i = line.get_attr(1);
    let expected_indexed_fg = PackedColor::indexed(2).0;
    assert_eq!(
        attr_i.fg, expected_indexed_fg,
        "indexed cell fg should be green (0x{expected_indexed_fg:08X}), got 0x{:08X}",
        attr_i.fg,
    );
}

/// Regression test for #5890: StyleId cells with missing style table entry
/// (e.g., after compaction) should fall back to terminal defaults, not black.
#[test]
fn style_id_cells_missing_style_fallback_to_defaults() {
    let mut grid = Grid::new(3, 10);

    // Create a StyleId cell pointing to a non-existent style (simulates
    // style table compaction where the style was garbage-collected)
    let cell = Cell::with_style_id('X', StyleId::new(999), CellFlags::empty());
    grid.row_mut(0).unwrap().set(0, cell);

    let row = grid.row(0).unwrap();
    let extracted = Grid::extract_row_extras(row, grid.extras(), 0, grid.styles());

    // No resolved colors should be stored (style lookup returned None)
    assert!(
        extracted.rgb_fg.is_empty(),
        "missing style should not produce fg colors"
    );
    assert!(
        extracted.rgb_bg.is_empty(),
        "missing style should not produce bg colors"
    );

    // Line conversion should use DEFAULT_FG/DEFAULT_BG as fallback
    let line = Grid::row_to_line_with_stored_extras(row, &extracted);
    let attr = line.get_attr(0);
    assert_eq!(
        attr.fg,
        PackedColor::DEFAULT_FG.0,
        "missing style should fall back to DEFAULT_FG"
    );
    assert_eq!(
        attr.bg,
        PackedColor::DEFAULT_BG.0,
        "missing style should fall back to DEFAULT_BG"
    );
}

/// Regression test for #5771: RGB-marked cells with missing extras in scrollback
/// conversion must use terminal defaults, not the black placeholder (0x01_000000).
#[test]
fn row_to_line_rgb_missing_extras_returns_default_colors() {
    use crate::grid::scroll_convert::ScrolledRowExtras;

    let mut grid = Grid::new(3, 10);

    // Create an RGB-marked cell: PackedColor::rgb() sets the 0x01 tag that
    // triggers fg_needs_overflow() / bg_needs_overflow().
    let rgb_fg = PackedColor::rgb(128, 64, 32);
    let rgb_bg = PackedColor::rgb(32, 64, 128);
    let cell = Cell::with_style('X', rgb_fg, rgb_bg, CellFlags::empty());
    assert!(cell.fg_needs_overflow(), "cell should be RGB-marked for fg");
    assert!(cell.bg_needs_overflow(), "cell should be RGB-marked for bg");

    grid.row_mut(0).unwrap().set(0, cell);

    let row = grid.row(0).unwrap();

    // Convert with empty extras — simulates overflow data loss during scrollback.
    let line = Grid::row_to_line_with_stored_extras(row, &ScrolledRowExtras::default());

    // The first character's attrs should have DEFAULT_FG/DEFAULT_BG, not the
    // inline black placeholder that the old deprecated Cell::fg()/bg() would return.
    let attr = line.get_attr(0);
    assert_eq!(
        attr.fg,
        PackedColor::DEFAULT_FG.0,
        "RGB cell with missing fg extras should get DEFAULT_FG (0x{:08X}), got 0x{:08X}",
        PackedColor::DEFAULT_FG.0,
        attr.fg,
    );
    assert_eq!(
        attr.bg,
        PackedColor::DEFAULT_BG.0,
        "RGB cell with missing bg extras should get DEFAULT_BG (0x{:08X}), got 0x{:08X}",
        PackedColor::DEFAULT_BG.0,
        attr.bg,
    );
}
