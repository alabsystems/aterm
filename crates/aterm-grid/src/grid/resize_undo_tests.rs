// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::grid::Grid;

fn labelled(rows: u16, cols: u16, cursor_row: u16, max_scrollback: usize) -> Grid {
    let mut grid = Grid::with_scrollback(rows, cols, max_scrollback);
    for r in 0..rows {
        grid.set_cursor(r, 0);
        for c in format!("R{r}").chars() {
            grid.write_char(c);
        }
    }
    grid.set_cursor(cursor_row, 2);
    grid
}

fn screen(grid: &Grid) -> Vec<String> {
    (0..grid.rows())
        .map(|r| grid.row(r).map(ToString::to_string).unwrap_or_default())
        .collect()
}

fn picture(grid: &Grid) -> (Vec<String>, u16, u16, bool, crate::ScrollRegion) {
    (
        screen(grid),
        grid.cursor_row(),
        grid.cursor_col(),
        grid.pending_wrap(),
        grid.scroll_region(),
    )
}

#[test]
fn a_quiet_52_51_52_flap_is_an_identity() {
    let mut grid = labelled(52, 121, 51, 0);
    grid.set_scroll_region(3, 40);
    grid.set_cursor(51, 7);
    let before = picture(&grid);

    grid.resize_no_reflow(51, 121);
    let shrink = grid.take_last_resize_shape();
    assert_eq!((shrink.demoted, shrink.stashed), (1, 1));
    assert_eq!(grid.row(0).unwrap().to_string(), "R1", "the shrink demotes");
    assert_eq!(grid.resize_undo_rows(), 1);

    grid.resize_no_reflow(52, 121);
    let grow = grid.take_last_resize_shape();
    assert_eq!(
        (grow.revealed, grow.restored_top, grow.appended),
        (1, 1, 0),
        "the grow hands the row back instead of appending a blank"
    );
    assert_eq!(picture(&grid), before, "an exact identity");
    assert_eq!(grid.resize_undo_rows(), 0, "the undo is spent");
    assert_eq!(grid.scrollback_lines(), 0);
    grid.assert_invariants();
}

#[test]
fn stacked_shrinks_and_partial_grows_unwind_in_order() {
    let mut grid = labelled(6, 10, 4, 0);
    let before = picture(&grid);
    grid.resize_no_reflow(5, 10);
    grid.resize_no_reflow(4, 10);
    assert_eq!(grid.row(0).unwrap().to_string(), "R2");
    assert_eq!(grid.resize_undo_rows(), 2);
    grid.resize_no_reflow(6, 10);
    assert_eq!(picture(&grid), before, "6→5→4→6 is an identity");

    let mut grid = labelled(6, 10, 4, 0);
    grid.resize_no_reflow(4, 10);
    grid.resize_no_reflow(5, 10);
    let shape = grid.take_last_resize_shape();
    assert_eq!((shape.restored_top, shape.appended), (1, 0));
    assert_eq!(grid.row(0).unwrap().to_string(), "R1", "the newest back");
    assert_eq!(grid.cursor_row(), 3);
    assert_eq!(grid.resize_undo_rows(), 1);
    grid.resize_no_reflow(6, 10);
    assert_eq!(picture(&grid), before, "and then the rest");

    let mut grid = labelled(6, 10, 4, 0);
    grid.resize_no_reflow(5, 10);
    grid.resize_no_reflow(8, 10);
    let shape = grid.take_last_resize_shape();
    assert_eq!((shape.restored_top, shape.appended), (1, 2));
    assert_eq!(screen(&grid)[..6], before.0[..]);
    assert_eq!(grid.resize_undo_rows(), 0);
    grid.assert_invariants();
}

#[test]
fn pushed_and_trimmed_rows_come_back_where_they_were() {
    for cursor in [0, 1] {
        let mut grid = labelled(6, 10, cursor, 0);
        let before = picture(&grid);
        grid.resize_no_reflow(4, 10);
        let shrink = grid.take_last_resize_shape();
        assert_eq!(
            (shrink.demoted + shrink.pushed, shrink.stashed),
            (2, 2),
            "cursor {cursor}"
        );
        grid.resize_no_reflow(6, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!(
            (grow.restored_bottom, grow.appended),
            (2 - shrink.demoted, 0)
        );
        assert_eq!(picture(&grid), before, "cursor {cursor}");
        grid.assert_invariants();
    }

    let mut grid = labelled(6, 10, 5, 0);
    grid.erase_line();
    grid.set_cursor(4, 1);
    let before = picture(&grid);
    grid.resize_no_reflow(3, 10);
    assert_eq!(
        grid.resize_undo_rows(),
        2,
        "two demoted; the trim is a blank"
    );
    grid.resize_no_reflow(6, 10);
    assert_eq!(picture(&grid), before);
}

#[test]
fn a_trim_stacked_on_a_push_comes_back_above_it() {
    fn painted() -> Grid {
        let mut grid = labelled(8, 20, 0, 0);
        grid.set_cursor(6, 0);
        grid.erase_line();
        grid.set_cursor(7, 0);
        grid.erase_line();
        grid.write_char('X');
        grid.set_cursor(0, 0);
        grid
    }
    let steps: [&[u16]; 4] = [&[7, 6, 8], &[7, 6, 7, 8], &[6, 8], &[7, 6, 5, 8]];
    for heights in steps {
        let mut grid = painted();
        let before = picture(&grid);
        assert_eq!(before.0[5..], ["R5", "", "X"]);
        for &h in heights {
            grid.resize_no_reflow(h, 20);
        }
        assert_eq!(picture(&grid), before, "{heights:?}");
        assert_eq!(grid.resize_undo_rows(), 0, "{heights:?}");
        grid.assert_invariants();
    }

    let mut grid = painted();
    grid.resize_no_reflow(7, 20);
    grid.resize_no_reflow(6, 20);
    grid.resize_no_reflow(8, 20);
    let grow = grid.take_last_resize_shape();
    assert_eq!((grow.restored_bottom, grow.appended), (1, 1));
}

#[test]
fn a_blank_band_the_first_shrink_leaves_trailing_unwinds_through_its_trim() {
    let mut grid = labelled(15, 10, 1, 0);
    for r in [11, 12] {
        grid.set_cursor(r, 0);
        grid.erase_line();
    }
    grid.set_cursor(1, 2);
    let before = picture(&grid);
    grid.resize_no_reflow(12, 10);
    grid.resize_no_reflow(9, 10);
    assert_eq!(grid.take_last_resize_shape().trimmed, 2);
    grid.resize_no_reflow(15, 10);
    assert_eq!(picture(&grid), before);
    assert_eq!(grid.resize_undo_rows(), 0);
    grid.assert_invariants();
}

#[test]
fn a_write_a_cursor_move_or_a_width_change_drops_the_undo() {
    type Between = fn(&mut Grid);
    let cases: [(&str, Between); 4] = [
        ("write", |g| g.write_char('x')),
        ("cursor move", |g| g.set_cursor(0, 0)),
        ("scroll region", |g| g.set_scroll_region(0, 2)),
        ("drop", Grid::drop_resize_undo),
    ];
    for (what, between) in cases {
        let mut grid = labelled(6, 10, 4, 0);
        grid.resize_no_reflow(5, 10);
        between(&mut grid);
        grid.resize_no_reflow(6, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!((grow.restored_top, grow.appended), (0, 1), "{what}");
        assert_eq!(grid.row(0).unwrap().to_string(), "R1", "{what}: shifted");
        assert!(grid.row(5).unwrap().is_empty(), "{what}");
    }

    let mut grid = labelled(6, 10, 4, 0);
    grid.resize_no_reflow(5, 10);
    grid.resize_no_reflow(5, 12);
    assert_eq!(grid.resize_undo_rows(), 0);
    grid.resize_no_reflow(6, 12);
    assert_eq!(grid.row(0).unwrap().to_string(), "R1");

    let mut grid = labelled(6, 10, 4, 0);
    grid.resize_with_policy(5, 10, crate::grid::reflow::ResizePolicy::ConPty);
    assert_eq!(grid.resize_undo_rows(), 0);
}

#[test]
fn a_retaining_grid_keeps_no_undo() {
    let mut grid = labelled(6, 10, 4, 100);
    let before = picture(&grid);
    grid.resize(5, 10);
    assert_eq!(grid.resize_undo_rows(), 0);
    assert_eq!(grid.take_last_resize_shape().stashed, 0);
    grid.resize(6, 10);
    let grow = grid.take_last_resize_shape();
    assert_eq!((grow.revealed, grow.restored_top), (1, 0));
    assert_eq!(picture(&grid).0, before.0);
}
