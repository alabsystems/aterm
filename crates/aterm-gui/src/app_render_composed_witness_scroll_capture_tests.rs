// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ComposedWitnessRows, focused_composed_cursor_fx_sample};
use aterm_core::terminal::Terminal;

fn witness_rows(
    term: &Terminal,
    previous: aterm_core::terminal::ContentScrollState,
    requested: &[u16],
) -> ComposedWitnessRows {
    focused_composed_cursor_fx_sample(
        1,
        term,
        None,
        true,
        true,
        true,
        (0, 0),
        requested,
        Some(previous),
        ComposedWitnessRows::default(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .witness_rows
}

fn row_text(term: &Terminal, row: u16) -> Vec<char> {
    let mut cols = Vec::new();
    term.row_cols_into(usize::from(row), &mut cols);
    cols
}

#[test]
fn a_whole_grid_scroll_reads_each_witness_row_where_asked_and_where_carried() {
    let mut term = Terminal::new(10, 20);
    for i in 0..10 {
        term.process(format!("line {i}\r\n").as_bytes());
    }
    let still = term.content_scroll_state();
    let rows = witness_rows(&term, still, &[5]);
    assert_eq!(rows.get(5), Some(&row_text(&term, 5)[..]));
    assert_eq!(rows.get(2), None, "no scroll: the asked row alone");

    term.process(b"step 1\r\nstep 2\r\nstep 3\r\n");
    let rows = witness_rows(&term, still, &[5]);
    assert_eq!(rows.get(5), Some(&row_text(&term, 5)[..]), "where asked");
    assert_eq!(
        rows.get(2),
        Some(&row_text(&term, 2)[..]),
        "and the scroll's three rows higher"
    );

    let banded = term.content_scroll_state();
    term.process(b"\x1b[2;6r\x1b[6;1H\r\n\x1b[r");
    let rows = witness_rows(&term, banded, &[5]);
    assert_eq!(
        (rows.get(5), rows.get(2)),
        (None, None),
        "a band move: none"
    );
}
