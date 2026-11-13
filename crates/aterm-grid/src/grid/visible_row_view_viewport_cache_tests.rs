// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use super::VisibleRowView;
use crate::Grid;

fn grid_with_history(lines: usize) -> Grid {
    let mut grid = Grid::new(3, 8);
    for i in 0..lines {
        grid.carriage_return();
        for ch in format!("L{i:03}").chars() {
            grid.write_char(ch);
        }
        grid.scroll_up(1);
    }
    grid
}

fn history_row(grid: &Grid, visible_row: u16) -> Arc<super::MaterializedRow> {
    match grid.visible_row_view(visible_row) {
        VisibleRowView::History { mat } => mat,
        _ => panic!("visible row {visible_row} is not a history row"),
    }
}

fn row_text(grid: &Grid, visible_row: u16) -> String {
    let view = grid.visible_row_view(visible_row);
    let mut out = String::new();
    for col in 0..view.len() {
        if let Some(cell) = view.cell(col) {
            view.push_cell_text(col, cell, &mut out);
        }
    }
    out
}

#[test]
fn a_scrolled_row_keeps_its_materialization_at_its_new_viewport_index() {
    let mut grid = grid_with_history(40);
    grid.scroll_display(10);
    let before = history_row(&grid, 1);
    let text_before = row_text(&grid, 1);
    grid.scroll_display(1);
    let after = history_row(&grid, 2);
    assert_eq!(
        row_text(&grid, 2),
        text_before,
        "the row that moved down one viewport slot is not the same line"
    );
    assert!(
        Arc::ptr_eq(&before, &after),
        "an overlapping scroll re-materialized a row it had already built"
    );
}

#[test]
fn a_content_change_invalidates_every_memoized_row() {
    let mut grid = grid_with_history(40);
    grid.scroll_display(10);
    let before = history_row(&grid, 0);
    let text_before = row_text(&grid, 0);
    grid.mark_content_full();
    let after = history_row(&grid, 0);
    assert!(
        !Arc::ptr_eq(&before, &after),
        "a content change left stale rows in the memo"
    );
    assert_eq!(
        row_text(&grid, 0),
        text_before,
        "the rebuilt row does not match the row it replaced"
    );
}

#[test]
fn a_line_arriving_while_scrolled_back_reshuffles_rows_correctly() {
    let mut grid = grid_with_history(40);
    grid.scroll_display(10);
    let cached_row1 = history_row(&grid, 1);
    let text_row0 = row_text(&grid, 0);
    let text_row1 = row_text(&grid, 1);
    assert_ne!(text_row0, text_row1, "the fixture rows are not distinct");
    grid.carriage_return();
    for ch in "NEW".chars() {
        grid.write_char(ch);
    }
    grid.scroll_up(1);
    let after_row0 = history_row(&grid, 0);
    assert_eq!(
        row_text(&grid, 0),
        text_row1,
        "the line that was at viewport row 1 is not at row 0 after one new line"
    );
    assert_ne!(
        row_text(&grid, 0),
        text_row0,
        "viewport row 0 still shows the pre-arrival line — a row was served by \
         INDEX across a history growth"
    );
    assert!(
        Arc::ptr_eq(&cached_row1, &after_row0),
        "live-screen writes and scroll_up invalidated unchanged absolute history rows"
    );
}

#[test]
fn memoized_rows_match_a_fresh_materialize_across_a_scrub() {
    let mut grid = grid_with_history(60);
    grid.scroll_display(30);
    for _ in 0..20 {
        let offset = grid.display_offset();
        for r in 0..3u16 {
            if usize::from(r) >= offset {
                continue;
            }
            let rev_idx = offset - 1 - usize::from(r);
            let fresh = grid
                .materialize_scrollback_row_full(rev_idx, grid.cols())
                .expect("history row exists");
            let cached = history_row(&grid, r);
            assert_eq!(
                *cached, fresh,
                "memoized viewport row {r} (rev_idx {rev_idx}) diverged from a \
                 fresh materialization"
            );
        }
        grid.scroll_display(-1);
    }
}

#[test]
fn early_wrap_hole_row_reflects_live_row_zero_edits_without_stale_memo() {
    let mut grid = Grid::new(1, 5);
    for ch in "AAAA".chars() {
        grid.write_char(ch);
    }
    // Wide char at col 4 of a 1x5 grid wraps early, scrolling "AAAA" into
    // `rev_idx == 0` with an early-wrap hole and placing '漢' at live row 0.
    let _ = grid.pre_wrap_wide_ecols(crate::Cell::EMPTY);
    assert!(grid.write_wide_char_at_cursor_packed(
        '漢',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::empty(),
    ));
    grid.scroll_display(1);

    let first = history_row(&grid, 0);
    grid.set_cursor(0, 0);
    assert!(grid.write_wide_char_at_cursor_packed(
        '国',
        crate::PackedColors::with_indexed_bg(1),
        crate::CellFlags::empty(),
    ));
    let second = history_row(&grid, 0);
    let fresh = grid
        .materialize_scrollback_row_full(0, grid.cols())
        .expect("history row exists");
    assert_eq!(
        *second, fresh,
        "early-wrap hole row at rev_idx == 0 must track live row 0 wide opening"
    );
    assert_ne!(
        *first, *second,
        "mutating live row 0's wide opening background must update the early-wrap history row"
    );
}
