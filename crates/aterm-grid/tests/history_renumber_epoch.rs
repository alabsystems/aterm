// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SELECTION CUSTODY §3 row 15 — the width-reflow bump of `history_renumber_epoch`.
//!
//! A width rewrap changes how many rows the SAME history occupies, so every absolute
//! row number above the splice shifts. That is a wholesale RENUMBERING, and
//! `history_renumber_epoch` is the one signal absolute-row-keyed caches (the
//! terminal's incremental search-index refresh, the viewport row cache) have that
//! their keys went stale. The bump landed at `scrollback_reflow.rs` with no test in
//! either `aterm-grid/tests` or `aterm-core/tests`: deleting it left every suite
//! green while the search index carried pre-rewrap keys forward across a
//! window-width drag.
//!
//! The epoch itself is the honest oracle here, because no CONSUMER can witness the
//! bump through a width change — but not for the reason a first reading suggests, and
//! the distinction is worth writing down since it decides what a future test may
//! assume.
//!
//! The two consumers are fenced differently. `refresh_search_index`'s `reusable` guard
//! (`search_index.rs:265`) does compare `cached.cols == cols`, so a width change
//! invalidates it whatever the epoch does. `indexed_search`'s hit key
//! (`search_index.rs:154`) does NOT mention `cols` at all — it is
//! `(alternate_screen, content_seq, history_renumber_epoch)`. What covers it is that a
//! width reflow bumps `content_seq` as a side effect (measured: 26 -> 27 across a
//! 40 -> 20 resize), so the entry misses on the seq term before the epoch term is ever
//! reached.
//!
//! So both are covered on this path, and a behavioural test through either says
//! nothing about the bump. But `indexed_search` is covered by a COINCIDENCE of the
//! reflow implementation rather than by a guard that names width, and the epoch's
//! documented reason for joining that key is the case where the coincidence does not
//! hold: a Kitty CSI +T unscroll renumbers history WITHOUT the `content_seq`
//! arithmetic the cache can observe. Should a future reflow stop touching
//! `content_seq`, `indexed_search` would silently serve pre-rewrap keys and this file
//! is what still fails.

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

/// A width change re-wraps retained history and must ADVANCE the renumber epoch.
/// A rows-only change must not: it splices rows without re-numbering the history
/// that was already archived, and a spurious bump would throw away every
/// absolute-keyed cache on an ordinary window-height drag.
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
