// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! MaterializedRow::len() unit tests (#5613).
//!
//! Migrated from aterm-core as part of #6556 Batch 2.

use super::super::*;

#[test]
fn materialized_row_len_ascii_content() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, 5).collect();
    let line = Line::with_attrs("Hello", attrs);
    let row = materialize_from_line(&line, 10);

    assert_eq!(row.len(), 5, "len should be 5 for 'Hello' in 10 cols");
    assert!(!row.is_empty());
}

#[test]
fn materialized_row_len_empty_row() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let line = Line::new();
    let row = materialize_from_line(&line, 10);

    assert_eq!(row.len(), 0, "empty row should have len 0");
    assert!(row.is_empty());
}

#[test]
fn materialized_row_len_wide_cjk() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let text = "日本";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);
    let row = materialize_from_line(&line, 10);

    assert_eq!(row.len(), 4, "two wide CJK chars should give len 4");
}

#[test]
fn materialized_row_len_complex_emoji() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let text = "\u{1F389}";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);
    let row = materialize_from_line(&line, 10);

    assert_eq!(row.len(), 2, "wide emoji should give len 2");
}

#[test]
fn materialized_row_len_combining_mark() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let text = "e\u{0301}";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);
    let row = materialize_from_line(&line, 10);

    assert_eq!(row.len(), 1, "base + combining mark should give len 1");
}

#[test]
fn materialized_row_len_trailing_spaces_after_content() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let text = "AB   ";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);
    let row = materialize_from_line(&line, 10);

    assert_eq!(
        row.len(),
        2,
        "trailing default spaces should not contribute to len"
    );
}

#[test]
fn materialized_row_len_styled_trailing_spaces() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let default_a = CellAttrs::DEFAULT;
    let red_space = CellAttrs::new(0x01_FF_00_00, 0xFF_00_00_00, 0);
    let attrs: Rle<CellAttrs> = [default_a, red_space].into_iter().collect();
    let line = Line::with_attrs("A ", attrs);
    let row = materialize_from_line(&line, 10);

    // A trailing coloured space is a WRITTEN cell, so it counts toward len exactly
    // as the live `Row::len` (write high-water mark) does — the previous `len() == 1`
    // was the gap that dropped coloured status-bar blanks on scrollback.
    assert_eq!(
        row.len(),
        2,
        "MaterializedRow::len() now counts coloured trailing spaces, matching Row::len()"
    );
}

#[test]
fn materialized_row_len_matches_row_len_for_content() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let mut grid = Grid::new(3, 10);
    let red_fg = PackedColor::indexed(1);
    let default_bg = PackedColor::DEFAULT_BG;

    for (col, c) in "Hello".chars().enumerate() {
        let cell = Cell::with_style(c, red_fg, default_bg, CellFlags::BOLD);
        grid.row_mut(0).unwrap().set(col as u16, cell);
    }

    let row = grid.row(0).unwrap();
    let row_len = row.len();

    let line = Grid::row_to_line_static(row);
    let mat = materialize_from_line(&line, 10);
    let mat_len = mat.len();

    assert_eq!(
        row_len, mat_len,
        "Row::len() ({row_len}) and MaterializedRow::len() ({mat_len}) should match for normal content"
    );
}
