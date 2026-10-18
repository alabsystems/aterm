// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! RGB-overflow cells keep their colours through scrollback line conversion.

use crate::{Cell, CellFlags, Grid, PackedColor};

/// RGB-marked cells with missing extras fall back to terminal defaults, not the
/// black placeholder (0x01_000000).
#[test]
fn row_to_line_rgb_missing_extras_returns_default_colors() {
    use crate::grid::scroll_convert::ScrolledRowExtras;

    let mut grid = Grid::new(3, 10);

    // `PackedColor::rgb` sets the 0x01 tag that `fg_needs_overflow`/`bg_needs_overflow` test.
    let rgb_fg = PackedColor::rgb(128, 64, 32);
    let rgb_bg = PackedColor::rgb(32, 64, 128);
    let cell = Cell::with_style('X', rgb_fg, rgb_bg, CellFlags::empty());
    assert!(cell.fg_needs_overflow(), "cell should be RGB-marked for fg");
    assert!(cell.bg_needs_overflow(), "cell should be RGB-marked for bg");

    grid.row_mut(0).unwrap().set(0, cell);

    let row = grid.row(0).unwrap();

    // Empty extras: the overflow data is lost.
    let line = Grid::row_to_line_with_stored_extras(row, &ScrolledRowExtras::default());

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
