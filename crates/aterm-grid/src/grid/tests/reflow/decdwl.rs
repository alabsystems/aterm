// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! DECDWL/DECDHL reflow tests: double-width and double-height lines resize in place without splitting or merging.

use crate::{CellFlags, Grid, LineSize};

#[test]
fn decdhl_top_bottom_preserved_on_shrink() {
    let mut grid = Grid::new(5, 80);
    grid.write_char('T');
    grid.set_row_line_size(0, LineSize::DoubleHeightTop);

    grid.set_cursor(1, 0);
    grid.write_char('B');
    grid.set_row_line_size(1, LineSize::DoubleHeightBottom);

    grid.resize(5, 40);
    grid.assert_invariants();

    assert_eq!(
        grid.row(0).unwrap().line_size(),
        LineSize::DoubleHeightTop,
        "DECDHL top half must survive shrink"
    );
    assert_eq!(
        grid.row(1).unwrap().line_size(),
        LineSize::DoubleHeightBottom,
        "DECDHL bottom half must survive shrink"
    );
}

#[test]
fn decdwl_round_trip_shrink_then_grow() {
    let mut grid = Grid::new(5, 80);
    for i in 0..20u16 {
        grid.write_char((b'A' + (i % 26) as u8) as char);
    }
    grid.set_row_line_size(0, LineSize::DoubleWidth);

    grid.resize(5, 40);
    assert_eq!(grid.row(0).unwrap().line_size(), LineSize::DoubleWidth);
    assert_eq!(grid.row(0).unwrap().get(0).unwrap().char_data(), 'A' as u16);

    grid.resize(5, 80);
    grid.assert_invariants();
    assert_eq!(grid.row(0).unwrap().line_size(), LineSize::DoubleWidth);
    assert_eq!(grid.row(0).unwrap().get(0).unwrap().char_data(), 'A' as u16);
}

#[test]
fn decdwl_mixed_with_normal_rows_on_shrink() {
    let mut grid = Grid::new(5, 20);

    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.set_row_line_size(0, LineSize::DoubleWidth);

    grid.set_cursor(1, 0);
    for c in "1234567890".chars() {
        grid.write_char(c);
    }

    grid.resize(5, 5);
    grid.assert_invariants();

    assert_eq!(grid.row(0).unwrap().line_size(), LineSize::DoubleWidth);
    assert_eq!(grid.row(0).unwrap().get(0).unwrap().char_data(), 'A' as u16);
    assert_eq!(grid.row(0).unwrap().get(4).unwrap().char_data(), 'E' as u16);

    assert_eq!(grid.row(1).unwrap().line_size(), LineSize::SingleWidth);
    assert_eq!(grid.row(1).unwrap().get(0).unwrap().char_data(), '1' as u16);

    assert!(grid.row(2).unwrap().is_wrapped());
    assert_eq!(grid.row(2).unwrap().get(0).unwrap().char_data(), '6' as u16);
}

#[test]
fn decdwl_continuation_row_survives_shrink_merge() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJKL".chars() {
        grid.write_char_wrap(c);
    }
    assert!(
        grid.row(1).unwrap().is_wrapped(),
        "sanity: row 1 is a wrap continuation"
    );
    grid.set_row_line_size(1, LineSize::DoubleWidth);

    grid.resize(5, 6);
    grid.assert_invariants();

    let dwl = (0..grid.rows())
        .find(|&r| grid.row(r).unwrap().line_size() == LineSize::DoubleWidth)
        .expect("the DECDWL continuation row must survive the shrink merge");
    let row = grid.row(dwl).unwrap();
    assert!(
        row.is_wrapped(),
        "the DECDWL row keeps its continuation flag"
    );
    assert_eq!(row.get(0).unwrap().char_data(), 'K' as u16);
    assert_eq!(row.get(1).unwrap().char_data(), 'L' as u16);
}

#[test]
fn decdwl_continuation_row_not_merged_on_grow() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJKL".chars() {
        grid.write_char_wrap(c);
    }
    assert!(grid.row(1).unwrap().is_wrapped());
    grid.set_row_line_size(1, LineSize::DoubleWidth);

    grid.resize(5, 20);
    grid.assert_invariants();

    let row0 = grid.row(0).unwrap();
    assert_eq!(
        row0.len(),
        10,
        "the head must NOT absorb the DECDWL continuation"
    );
    let row1 = grid.row(1).unwrap();
    assert_eq!(
        row1.line_size(),
        LineSize::DoubleWidth,
        "DECDWL attribute must survive the grow reflow"
    );
    assert!(row1.is_wrapped(), "the continuation flag must survive too");
    assert_eq!(row1.get(0).unwrap().char_data(), 'K' as u16);
    assert_eq!(row1.get(1).unwrap().char_data(), 'L' as u16);
}

#[test]
fn decdwl_shrink_blanks_a_wide_char_cut_at_the_new_edge() {
    let mut grid = Grid::new(3, 100);
    grid.set_row_line_size(0, LineSize::DoubleWidth);
    for _ in 0..39 {
        grid.write_char('a');
    }
    grid.write_wide_char_wrap_styled(
        '界',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    {
        let row0 = grid.row(0).unwrap();
        assert!(row0.get(39).unwrap().is_wide(), "head at col 39");
        assert!(
            row0.get(40).unwrap().is_wide_continuation(),
            "continuation at col 40"
        );
    }
    grid.assert_wide_char_consistent();

    grid.resize(3, 40);
    grid.assert_wide_char_consistent();

    let row0 = grid.row(0).unwrap();
    assert_eq!(row0.line_size(), LineSize::DoubleWidth);
    assert_eq!(row0.get(38).unwrap().char_data(), 'a' as u16);
    let edge = row0.get(39).unwrap();
    assert!(!edge.is_wide(), "the cut wide head must be blanked");
}
