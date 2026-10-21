// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The width-reflow bump of `history_renumber_epoch`.
//!
//! A width rewrap changes how many rows the same history occupies, so absolute
//! row numbers shift wholesale. `history_renumber_epoch` is the signal
//! absolute-row-keyed caches (the search index, the viewport row cache) use to
//! learn their keys went stale.
//!
//! The epoch is the oracle here because no consumer witnesses the bump through
//! a width change: the search slot's key (`SearchKey`) also compares `cols`,
//! and currently misses on `content_seq` too, which a width reflow happens to
//! bump. Should reflow stop touching `content_seq`, only
//! these tests would catch stale keys.

use aterm_grid::Grid;

/// A grid with `history` lines of retained scrollback, each long enough that a
/// narrower width must re-wrap it into more rows than it started with.
fn grid_with_wrapping_history(rows: u16, cols: u16, history: usize) -> Grid {
    let mut grid = Grid::with_scrollback(rows, cols, 200);
    for i in 0..history {
        grid.set_cursor(rows - 1, 0);
        for (col, ch) in format!("history line {i} with enough text to rewrap")
            .chars()
            .take(usize::from(cols))
            .enumerate()
        {
            grid.set_cursor(rows - 1, u16::try_from(col).unwrap_or(0));
            grid.write_char(ch);
        }
        grid.scroll_up(1);
    }
    assert!(
        grid.scrollback_lines() > 0,
        "fixture must actually retain history"
    );
    grid
}

/// A width change advances the renumber epoch; a rows-only change does not (a
/// spurious bump would discard every absolute-keyed cache on a height drag).
#[test]
fn a_width_reflow_advances_the_history_renumber_epoch_and_a_rows_only_resize_does_not() {
    let mut grid = grid_with_wrapping_history(4, 40, 30);
    let history_rows_before = grid.scrollback_lines();
    let epoch_before = grid.history_renumber_epoch();

    // Narrow: every retained logical line re-wraps into MORE rows, so the absolute
    // number of every row above the splice shifts.
    grid.resize(4, 17);
    assert!(
        grid.scrollback_lines() > history_rows_before,
        "precondition: the narrowing really did re-wrap history into more rows \
         (before={history_rows_before}, after={})",
        grid.scrollback_lines()
    );
    assert!(
        grid.history_renumber_epoch() > epoch_before,
        "a width rewrap renumbers history; the epoch is the only signal an \
         absolute-row-keyed cache gets"
    );

    // CONTROL: rows-only. Nothing is re-wrapped, so nothing is renumbered.
    let epoch_after_width = grid.history_renumber_epoch();
    grid.resize(9, 17);
    assert_eq!(
        grid.history_renumber_epoch(),
        epoch_after_width,
        "a rows-only resize re-wraps nothing; bumping here would discard every \
         absolute-keyed cache on an ordinary height drag"
    );
}

/// A width reflow with empty scrollback still advances the epoch: the same
/// logical lines move from four rows to eight, so absolute row N names
/// different text even though nothing was spliced into history.
#[test]
fn a_width_reflow_with_no_scrollback_still_advances_the_renumber_epoch() {
    // Three full-width lines in a 12-row viewport: 3 rows at width 20, 6 at
    // width 10, so nothing is pushed off the top.
    let mut grid = Grid::with_scrollback(12, 20, 200);
    for row in 0..3u16 {
        for (col, ch) in format!("row {row} text that wraps")
            .chars()
            .take(20)
            .enumerate()
        {
            grid.set_cursor(row, u16::try_from(col).unwrap_or(0));
            grid.write_char(ch);
        }
    }
    assert_eq!(
        grid.scrollback_lines(),
        0,
        "fixture: the whole point is a rewrap with nothing in history"
    );
    let epoch_before = grid.history_renumber_epoch();

    grid.resize(12, 10);
    assert_eq!(
        grid.scrollback_lines(),
        0,
        "fixture: the rewrap still spliced nothing into history — this is the \
         case the boundary-crossing bump sites cannot reach"
    );
    assert!(
        grid.history_renumber_epoch() > epoch_before,
        "a width rewrap renumbers rows whether or not a history row moved; \
         consumers holding a cached absolute row get no other signal"
    );

    // CONTROL, restated for this fixture: rows-only still renumbers nothing
    // that a reveal/demote relabel does not already account for.
    let epoch_after_width = grid.history_renumber_epoch();
    grid.resize(12, 10);
    assert_eq!(
        grid.history_renumber_epoch(),
        epoch_after_width,
        "a resize to the SAME dimensions rewraps nothing and must not bump"
    );
}
