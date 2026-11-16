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

/// The viewport's rows, top first, as `visible_row_view` reads them.
fn viewport_texts(grid: &Grid) -> Vec<String> {
    (0..grid.rows()).map(|r| row_text(grid, r)).collect()
}

#[test]
fn reserving_older_history_keys_rekeys_the_memoized_rows() {
    // The reserve raises every retained row's key without moving a row: the viewport shows the
    // same lines, now under keys the memo filled from the rows one line newer.
    let mut grid = grid_with_history(40);
    grid.scroll_display(10);
    let shown = viewport_texts(&grid);
    let _claim = grid.reserve_older_history_keys(grid.oldest_absolute_row() + 1);
    assert_eq!(
        viewport_texts(&grid),
        shown,
        "reserving older history keys moved no row, yet the viewport changed"
    );
}

#[test]
fn continuing_the_numbering_rekeys_the_memoized_rows() {
    // A checkpoint's counter raises every retained row's key without moving a row, as a
    // reserve does.
    let mut grid = grid_with_history(40);
    grid.scroll_display(10);
    let shown = viewport_texts(&grid);
    grid.continue_absolute_numbering(grid.absolute_row_counter() + 1);
    assert_eq!(
        viewport_texts(&grid),
        shown,
        "continuing the numbering from a checkpoint moved no row, yet the viewport changed"
    );
}

#[test]
fn a_reflow_window_cap_drop_puts_the_marker_where_the_dropped_row_was_memoized() {
    use crate::grid::lazy_buffer::FLOOD_MARKER_PREFIX;
    use aterm_scrollback::{Scrollback, ScrollbackStorage};

    let (rows, cols) = (4u16, 80u16);
    let store: ScrollbackStorage = Scrollback::new(64, 512, 64_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 16, store);
    let line = |grid: &mut Grid, text: &str| {
        grid.set_cursor(rows - 1, 0);
        for ch in text.chars() {
            grid.write_char(ch);
        }
        grid.line_feed();
        grid.carriage_return();
    };
    for i in 0..100 {
        line(&mut grid, &format!("H{i}"));
    }
    // A width change detaches the store; the window's lines stage until the job reattaches.
    let _job = grid
        .resize_offloading_scrollback(rows, cols - 20)
        .expect("a width change over a tiered store detaches it");
    for i in 0..=Grid::DETACHED_LAZY_CAP {
        line(&mut grid, &format!("W{i}"));
    }
    // Every viewport row is memoized: the cut's next drop moves the marker one or two keys up
    // (two when the last of those line feeds opened the cut), onto a row read here either way.
    grid.scroll_to_top();
    let shown = viewport_texts(&grid);
    assert!(
        shown[0].starts_with(FLOOD_MARKER_PREFIX),
        "precondition: the cap opened a cut, whose marker is the oldest row ({shown:?})"
    );
    assert!(
        shown[1..]
            .iter()
            .all(|text| !text.is_empty() && !text.starts_with(FLOOD_MARKER_PREFIX)),
        "precondition: staged lines sit under the marker ({shown:?})"
    );

    // One more line: the cap drops the oldest staged line(s), and the marker now stands on the
    // absolute row the newest of them held. That key names other text now, so the drop moves
    // the renumber epoch every absolute-row-keyed cache reads (the find bar's matches and the
    // cell frame's scratch, not only the memo).
    let epoch = grid.history_renumber_epoch();
    line(&mut grid, "LAST");
    assert!(
        grid.history_renumber_epoch() > epoch,
        "a cap drop renumbered a key without moving history_renumber_epoch"
    );
    grid.scroll_to_top();
    let top = row_text(&grid, 0);
    assert!(
        top.starts_with(FLOOD_MARKER_PREFIX),
        "viewport row 0 shows {top:?} where the marker belongs (before the drop: {shown:?})"
    );
    assert_ne!(top, shown[0], "the marker names more dropped lines");
    let d = grid.display_offset();
    for v in 0..rows {
        let fresh = grid
            .materialize_scrollback_row_full(d - 1 - usize::from(v), grid.cols())
            .expect("every viewport row is a history row");
        assert_eq!(
            *history_row(&grid, v),
            fresh,
            "viewport row {v} is not what history holds there"
        );
    }
}
