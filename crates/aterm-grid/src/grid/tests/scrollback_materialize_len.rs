// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! MaterializedRow::len() unit tests.

use super::super::*;

/// The occupied length counts columns, not chars, and ignores default-styled
/// trailing spaces.
#[test]
fn materialized_row_len_counts_occupied_columns() {
    use crate::grid::scroll_materialize::materialize_from_line;

    for (text, want) in [
        ("", 0),
        ("Hello", 5),
        ("日本", 4),
        ("\u{1F389}", 2),
        ("e\u{0301}", 1),
        ("AB   ", 2),
    ] {
        let line = if text.is_empty() {
            Line::new()
        } else {
            let attrs: Rle<CellAttrs> =
                std::iter::repeat_n(CellAttrs::DEFAULT, text.chars().count()).collect();
            Line::with_attrs(text, attrs)
        };
        let row = materialize_from_line(&line, 10);
        assert_eq!(row.len(), want, "{text:?}");
        assert_eq!(row.is_empty(), want == 0, "{text:?}");
    }
}

#[test]
fn materialized_row_len_styled_trailing_spaces() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let default_a = CellAttrs::DEFAULT;
    let red_space = CellAttrs::new(0x01_FF_00_00, 0xFF_00_00_00, 0);
    let attrs: Rle<CellAttrs> = [default_a, red_space].into_iter().collect();
    let line = Line::with_attrs("A ", attrs);
    let row = materialize_from_line(&line, 10);

    // A trailing coloured space is a written cell and counts toward len, as the
    // live `Row::len` (write high-water mark) does.
    assert_eq!(
        row.len(),
        2,
        "a coloured trailing space counts, as in Row::len()"
    );
}
