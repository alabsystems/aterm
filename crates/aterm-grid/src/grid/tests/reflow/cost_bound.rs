// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded-cost tests for synchronous width-change scrollback reflow.

use super::super::super::*;
use crate::test_counters::{take_handback_history_reads, take_scrollback_reflow_sync_lines};
use aterm_scrollback::{Scrollback, ScrollbackStorage};

/// A `rows`×`cols` grid over a tiered store with a `ring`-line ring.
fn tiered_grid(rows: u16, cols: u16, ring: usize) -> Grid {
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    Grid::with_tiered_scrollback(rows, cols, ring, sb)
}

/// Write `s` on the bottom row and scroll it off.
fn short_line(g: &mut Grid, rows: u16, s: &str) {
    g.set_cursor(rows - 1, 0);
    for c in s.chars() {
        g.write_char(c);
    }
    g.line_feed();
    g.carriage_return();
}

/// [`tiered_grid`] after `n` short `H{i}` history lines (no wrap at 40 columns).
fn tiered_grid_with_short_history(rows: u16, cols: u16, ring: usize, n: usize) -> Grid {
    let mut grid = tiered_grid(rows, cols, ring);
    for i in 0..n {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
    grid
}

/// [`tiered_grid_with_short_history`] with an 8-line ring.
fn tiered_grid_with_history_away(rows: u16, cols: u16, n: usize) -> Grid {
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, n);
    grid.set_cursor(rows - 1, 0);
    for _ in 0..usize::from(cols) * (5 * usize::from(rows) + 8 + 6) {
        grid.write_char_wrap('y');
    }
    grid.line_feed();
    grid.carriage_return();
    grid
}

/// A grid whose bulk history is in the tiered store.
fn tiered_grid_with_deep_history(rows: u16, cols: u16, n: u16) -> Grid {
    let mut grid = tiered_grid(rows, cols, 8);
    for i in 0..n {
        grid.set_cursor(rows - 1, 0);
        let mut text = format!("L{i}-");
        while text.len() + 1 < cols as usize {
            text.push('x');
        }
        for c in text.chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }
    grid
}

#[test]
fn offloaded_resize_rewraps_only_the_lines_it_holds_back() {
    for (rows, cols, requested_rows, requested_cols) in [
        (1, crate::MAX_GRID_COLS, 0, u16::MAX),
        (crate::MAX_GRID_ROWS, 1, u16::MAX, 0),
    ] {
        let mut bounded = tiered_grid(rows, cols, 1);
        bounded.write_char('x');
        assert!(
            bounded
                .resize_offloading_scrollback(requested_rows, requested_cols)
                .is_none(),
            "the normalized size is unchanged, so no stale-width job is made"
        );
        assert_eq!((bounded.rows(), bounded.cols()), (rows, cols));
        assert_eq!(
            bounded.row(0).and_then(|r| r.get(0)).map(|c| c.char()),
            Some('x')
        );
        bounded.assert_invariants();
    }

    let (rows, cols) = (24u16, 80u16);
    let mut grid = tiered_grid(rows, cols, 1000);
    for i in 0..3000 {
        short_line(&mut grid, rows, &format!("L{i}-{}", "x".repeat(70)));
    }
    let ring_before = grid.ring_buffer_scrollback();
    assert_eq!(
        ring_before, 1000,
        "precondition: the ring is full, far past what the detach holds back"
    );
    let history_before = grid.scrollback_lines();

    let _ = take_scrollback_reflow_sync_lines(); // reset
    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("a width change with a tiered store yields an offload job");
    let sync = take_scrollback_reflow_sync_lines();
    let held = crate::grid::scrollback_offload::HOLD_BACK_SCREENS * usize::from(rows);
    assert_eq!(
        sync, held,
        "the offloaded resize rewraps the lines it holds back and no other history \
         line synchronously: the rest of the ring rides the job (RFL-1), the tiered \
         store is detached"
    );
    assert_eq!(
        pending.line_count(),
        history_before - held,
        "the job carries the store's history and the ring's lines past the held ones"
    );

    grid.reattach_reflowed_scrollback(pending.reflow());
    assert!(
        grid.scrollback_lines() > 1000,
        "history preserved after the round trip"
    );
    grid.assert_invariants();
}

#[test]
fn offload_window_captures_concurrent_scrolloff_no_gap() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 1000);
    let before = grid.scrollback_lines();
    assert!(before > 500, "precondition: deep history ({before})");

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("W{i}"));
    }
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    let after = grid.scrollback_lines();
    assert!(
        after >= before + 480,
        "the ~500 lines produced during the reflow window must survive \
         (before={before}, after={after}) — a gap means concurrent output was dropped"
    );
    grid.assert_invariants();
}

#[test]
fn offload_window_erase_is_not_resurrected() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_deep_history(rows, cols, 1000);
    assert!(grid.scrollback_lines() > 500);

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    grid.erase_scrollback(); // Cmd-K / `clear` (ED3) lands during the window
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    assert_eq!(
        grid.scrollback_lines(),
        0,
        "erased scrollback must stay erased, not resurrect on re-attach"
    );
    grid.assert_invariants();
}

#[test]
fn offload_preserves_deep_scroll_position() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_deep_history(rows, cols, 2000);
    grid.scroll_to_top(); // reader is deep in history
    let deep = grid.display_offset();
    assert!(deep > 1000, "precondition: scrolled deep ({deep})");

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    assert!(
        grid.display_offset() > 1000,
        "deep scroll position must survive the offload, not collapse to the ring \
         (got {})",
        grid.display_offset()
    );
    grid.assert_invariants();
}

#[test]
fn offload_window_height_shrink_keeps_ring_scrollback() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 128, 1000);
    let before = grid.scrollback_lines();
    assert!(before > 500, "precondition: deep history ({before})");

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    for i in 0..600 {
        short_line(&mut grid, rows, &format!("W{i}")); // short: no wrap at 40
    }
    assert!(
        grid.resize_offloading_scrollback(rows / 2, cols / 2)
            .is_none(),
        "mid-window resize must not re-detach (nothing to offload)"
    );
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    let after = grid.scrollback_lines();
    assert!(
        after >= before + 560,
        "the ~600 lines produced during the reflow window must survive a mid-window \
         height shrink (before={before}, after={after}) — a deficit means \
         adjust_row_count dropped ring scrollback while the store was detached"
    );
    grid.assert_invariants();
}

#[test]
fn offload_abort_recovers_grid_to_bounded_state() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 64, 500);

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    drop(pending); // worker "panicked" — reattach never runs
    grid.abort_reflow_offload();

    for i in 0..5000 {
        short_line(&mut grid, rows, &format!("R{i}"));
    }
    assert!(
        grid.scrollback_lines() < 1000,
        "after abort the grid is ring-only bounded, not leaking staged lines into an \
         un-drainable lazy buffer (scrollback_lines={})",
        grid.scrollback_lines()
    );
    grid.assert_invariants();
}

#[test]
fn offload_window_scroll_to_bottom_not_clobbered() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_deep_history(rows, cols, 2000);
    grid.scroll_to_top(); // reader deep in history
    assert!(grid.display_offset() > 1000, "precondition: scrolled deep");

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    grid.scroll_to_bottom(); // reader presses End to watch streaming output
    assert_eq!(grid.display_offset(), 0);
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    assert_eq!(
        grid.display_offset(),
        0,
        "a reader who scrolled to the live bottom during the window must stay there, \
         not be yanked back up to the stale deep position"
    );
    grid.assert_invariants();
}

#[test]
fn offload_window_lazy_buffer_is_bounded() {
    let (rows, cols) = (10u16, 80u16);
    let ring_cap = 64usize;
    let window_writes = 55_000usize;
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 64_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, ring_cap, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
    let trunc_before = grid.truncated_lines();

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    let held_at_detach = grid.ring_buffer_scrollback();
    for i in 0..window_writes {
        short_line(&mut grid, rows, &format!("W{i}"));
    }
    let reflowed_len = pending.lines_total();
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    let after = grid.scrollback_lines();
    let dropped = (grid.truncated_lines() - trunc_before) as usize;
    // `DETACHED_LAZY_CAP` bounds `lazy_buffer.len()`, which reserves 1 slot for the
    // flood-cut marker row alongside `DETACHED_LAZY_CAP - 1` content rows.
    let kept_window_rows = Grid::DETACHED_LAZY_CAP + ring_cap;
    let kept_content = (Grid::DETACHED_LAZY_CAP - 1) + ring_cap;
    assert_eq!(
        dropped,
        held_at_detach + window_writes - kept_content,
        "every content row evicted past (DETACHED_LAZY_CAP - 1) + ring_cap must be counted in truncated_lines"
    );
    assert_eq!(
        after,
        reflowed_len + kept_window_rows,
        "reattached history must equal reflowed prefix + flood marker + surviving window content"
    );
    let newest_scrolled_w = window_writes - usize::from(rows);
    let first_kept_w = newest_scrolled_w + 1 - kept_content;
    assert_eq!(
        grid.get_history_line(reflowed_len + 1)
            .and_then(|l| l.as_str().map(str::to_string))
            .as_deref(),
        Some(format!("W{first_kept_w}").as_str()),
        "oldest surviving window content row (after the flood marker) must be W{first_kept_w}"
    );
    assert_eq!(
        grid.get_history_line(after - 1)
            .and_then(|l| l.as_str().map(str::to_string))
            .as_deref(),
        Some(format!("W{newest_scrolled_w}").as_str()),
        "newest history line must be W{newest_scrolled_w}"
    );
    grid.assert_invariants();
}

/// A tiered grid whose history is soft-wrapped at `cols`.
fn tiered_grid_with_wrapped_history(rows: u16, cols: u16, n: u16) -> Grid {
    let mut grid = tiered_grid(rows, cols, 8);
    let width = cols as usize + 30;
    for i in 0..n {
        grid.set_cursor(rows - 1, 0);
        let mut text = format!("L{i}-");
        while text.len() < width {
            text.push('x');
        }
        for c in text.chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }
    grid
}

#[test]
fn offload_width_change_keeps_an_unwrapped_reader_on_the_same_line() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 500);
    grid.scroll_display(150);
    assert_eq!(grid.display_offset(), 150, "precondition: 150 rows up");
    let top_before = grid
        .row_text(0)
        .expect("top visible row")
        .trim_end()
        .to_string();
    assert!(
        top_before.starts_with('H'),
        "precondition: the reader is on a history line, not a blank ({top_before:?})"
    );

    let pending = grid
        .resize_offloading_scrollback(rows, 60)
        .expect("offload job");
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    assert_eq!(
        grid.display_offset(),
        150,
        "a width change that rewraps nothing must leave the reader exactly where \
         they were, not snap them to the live bottom"
    );
    assert_eq!(
        grid.row_text(0).expect("top visible row").trim_end(),
        top_before,
        "the reader must still be reading the same line after the resize"
    );
    grid.assert_invariants();
}

#[test]
fn offload_widen_keeps_the_scrolled_back_reader_in_history() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_wrapped_history(rows, cols, 300);
    grid.scroll_display(150);
    assert_eq!(grid.display_offset(), 150, "precondition: 150 rows up");

    let pending = grid
        .resize_offloading_scrollback(rows, cols + 40)
        .expect("offload job");
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    let sb = grid.scrollback_lines();
    assert_eq!(
        grid.display_offset(),
        150usize.min(sb),
        "the widen must leave the reader at their clamped pre-resize offset \
         (scrollback={sb})"
    );
    let seen: String = (0..rows)
        .filter_map(|r| grid.row_text(r))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !seen.contains("L299-"),
        "the newest history line must not be on screen — the reader was 150 rows \
         above it:\n{seen}"
    );
    grid.assert_invariants();
}

#[test]
fn redetach_clamps_the_viewport_and_still_restores_the_reader() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_wrapped_history(rows, cols, 300);
    grid.scroll_display(150);
    assert_eq!(grid.display_offset(), 150, "precondition: 150 rows up");

    let pending = grid
        .resize_offloading_scrollback(rows, cols + 40)
        .expect("offload job A");
    grid.resize(rows, cols + 20); // supersedes: detaches nothing, rewraps the ring
    let reflowed = pending.reflow();
    let follow = grid
        .reattach_reflowed_scrollback_or_redetach(reflowed)
        .expect("stale width must converge with a follow-up job");

    assert!(
        grid.display_offset() <= grid.scrollback_lines(),
        "the follow-up detach must re-clamp the viewport to the history still home \
         (offset={}, scrollback={})",
        grid.display_offset(),
        grid.scrollback_lines()
    );

    let done = follow.reflow();
    assert!(
        grid.reattach_reflowed_scrollback_or_redetach(done)
            .is_none(),
        "the converging pass ran at the settled width"
    );
    let sb = grid.scrollback_lines();
    assert_eq!(
        grid.display_offset(),
        150usize.min(sb),
        "the reader's position must survive the converging pass too (scrollback={sb})"
    );
    grid.assert_invariants();
}

#[test]
fn abort_reclamps_the_viewport_when_the_staged_window_output_is_discarded() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 500);

    let pending = grid
        .resize_offloading_scrollback(rows, 60)
        .expect("offload job");
    for i in 0..200 {
        short_line(&mut grid, rows, &format!("W{i}"));
    }
    grid.scroll_to_top();
    let deep = grid.display_offset();
    assert!(
        deep > 100,
        "precondition: the reader is up over the staged window output ({deep})"
    );

    drop(pending); // the worker panicked: this reflow will never re-attach
    grid.abort_reflow_offload();

    assert!(
        grid.display_offset() <= grid.scrollback_lines(),
        "the abort discarded the staged window output, so the viewport must be \
         re-clamped to the history that is left (offset={}, scrollback={})",
        grid.display_offset(),
        grid.scrollback_lines()
    );
    grid.assert_invariants();
}

#[test]
fn offload_window_reader_who_moved_then_returned_to_live_chose_it() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_history_away(rows, cols, 500);
    grid.scroll_display(150);

    let pending = grid
        .resize_offloading_scrollback(rows, cols + 40)
        .expect("offload job");
    assert_eq!(
        grid.display_offset(),
        0,
        "precondition: the widen's detach clamped the viewport to the live bottom, \
         so the detach-time offset carries no signal at all"
    );
    for i in 0..50 {
        short_line(&mut grid, rows, &format!("W{i}"));
    }
    grid.scroll_display(20); // wheel up over the window's output
    assert_eq!(grid.display_offset(), 20, "the reader really moved");
    grid.scroll_display(-20); // and wheel back down to the live bottom
    assert_eq!(grid.display_offset(), 0);

    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    assert_eq!(
        grid.display_offset(),
        0,
        "a reader who moved the viewport during the window and ended at the live \
         bottom chose it — do not yank them back into history"
    );
    grid.assert_invariants();
}

/// `n` short history lines and a long one ([`tiered_grid_with_history_away`]).
fn detached_with_reader_at(offset: i32, n: usize) -> (Grid, PendingScrollbackReflow) {
    let rows = 10u16;
    let mut grid = tiered_grid_with_history_away(rows, 80, n);
    grid.scroll_display(offset);
    let pending = grid
        .resize_offloading_scrollback(rows, 60)
        .expect("offload job");
    assert_eq!(
        grid.scrollback_lines(),
        0,
        "precondition: nothing is attached above the viewport while the job runs"
    );
    assert_eq!(grid.display_offset(), 0);
    (grid, pending)
}

#[test]
fn offload_window_scroll_up_over_the_away_history_lands_at_the_reattach() {
    let (mut grid, pending) = detached_with_reader_at(0, 500);
    grid.scroll_display(30);
    assert_eq!(
        grid.display_offset(),
        0,
        "nothing to show yet: the view stays"
    );
    assert!(grid.reader_aim_held(), "but the ask is kept");
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(
        grid.display_offset(),
        30,
        "the view lands where the reader asked"
    );
    assert!(!grid.reader_aim_held(), "spent at the re-attach");
    grid.assert_invariants();

    grid.scroll_display(5);
    assert_eq!(grid.display_offset(), 35);
    assert!(!grid.reader_aim_held());
}

#[test]
fn offload_window_aim_moves_both_ways_and_home_is_a_choice() {
    let (mut grid, pending) = detached_with_reader_at(0, 500);
    grid.scroll_display(30);
    grid.scroll_display(-10);
    assert_eq!(grid.display_offset(), 0);
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(grid.display_offset(), 20);

    let (mut grid, pending) = detached_with_reader_at(150, 500);
    grid.scroll_display(30);
    grid.scroll_display(-30);
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(
        grid.display_offset(),
        0,
        "up and back down to the live bottom is the reader's choice, not a no-op"
    );

    let (mut grid, pending) = detached_with_reader_at(150, 500);
    grid.scroll_display(30);
    grid.scroll_to_bottom();
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(grid.display_offset(), 0, "the End after the aim is honored");
    grid.assert_invariants();
}

#[test]
fn offload_window_aim_top_and_the_restore() {
    let (mut grid, pending) = detached_with_reader_at(0, 500);
    grid.scroll_to_top();
    grid.scroll_display(-5);
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(grid.display_offset(), grid.scrollback_lines() - 5);

    let (mut grid, pending) = detached_with_reader_at(150, 500);
    grid.scroll_display(30);
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(
        grid.display_offset(),
        150,
        "they reached UP; the restore already puts them further up than that"
    );

    let (mut grid, pending) = detached_with_reader_at(150, 500);
    grid.scroll_to_bottom();
    assert!(
        !grid.reader_aim_held(),
        "an End that moved nothing records nothing"
    );
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(grid.display_offset(), 150);
    grid.assert_invariants();
}

#[test]
fn offload_window_aim_rides_up_with_window_output() {
    let (mut grid, pending) = detached_with_reader_at(0, 500);
    grid.scroll_display(30);
    for i in 0..20 {
        short_line(&mut grid, 10, &format!("W{i}"));
    }
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(grid.display_offset(), 50);
    grid.assert_invariants();
}

#[test]
fn offload_abort_drops_the_reader_aim() {
    let (mut grid, pending) = detached_with_reader_at(0, 500);
    grid.scroll_display(30);
    drop(pending);
    grid.abort_reflow_offload();
    assert!(!grid.reader_aim_held());
    assert!(grid.display_offset() <= grid.scrollback_lines());
    grid.assert_invariants();
}

#[test]
fn offload_window_end_after_scrolling_over_window_output_is_honored() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 500);
    grid.scroll_display(150);

    let pending = grid
        .resize_offloading_scrollback(rows, 60)
        .expect("offload job");
    for i in 0..200 {
        short_line(&mut grid, rows, &format!("W{i}"));
    }
    grid.scroll_display(50);
    assert_eq!(grid.display_offset(), 50, "the reader really moved");
    assert_eq!(
        grid.row_text(0).expect("top visible row").trim_end(),
        "W141",
        "precondition: the scrolled-up viewport is showing REAL window output, so \
         the position the reader descends from is one they could see"
    );
    grid.scroll_to_bottom(); // End, and it moves the viewport 50 rows

    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    assert_eq!(
        grid.display_offset(),
        0,
        "an End that moved the viewport 50 rows is the reader choosing the live \
         bottom, whatever the detach had left behind"
    );
    grid.assert_invariants();
}

#[test]
fn redetach_window_end_after_scrolling_over_window_output_is_honored() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_wrapped_history(rows, cols, 300);
    grid.scroll_display(150);

    let pending = grid
        .resize_offloading_scrollback(rows, cols + 40)
        .expect("offload job A");
    grid.resize(rows, cols + 20); // supersedes: detaches nothing, rewraps the ring
    let reflowed = pending.reflow();
    let follow = grid
        .reattach_reflowed_scrollback_or_redetach(reflowed)
        .expect("stale width must converge with a follow-up job");

    for i in 0..200 {
        short_line(&mut grid, rows, &format!("W{i}"));
    }
    grid.scroll_display(50);
    assert_eq!(grid.display_offset(), 50, "the reader really moved");
    grid.scroll_to_bottom();

    let done = follow.reflow();
    assert!(
        grid.reattach_reflowed_scrollback_or_redetach(done)
            .is_none(),
        "the converging pass ran at the settled width"
    );
    assert_eq!(
        grid.display_offset(),
        0,
        "the converging window carries its own gesture baseline, so the reader's \
         End is honored there too"
    );
    grid.assert_invariants();
}

#[test]
fn offload_window_reader_who_only_scrolled_up_did_not_choose_the_bottom() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 500);
    grid.scroll_display(150);

    let pending = grid
        .resize_offloading_scrollback(rows, 60)
        .expect("offload job");
    for i in 0..50 {
        short_line(&mut grid, rows, &format!("W{i}"));
    }
    grid.scroll_display(4); // the reader stops here and touches nothing else
    assert_eq!(grid.display_offset(), 4);
    let gen_after_reader = grid.storage.reader_live_bottom_gen;

    grid.resize(rows + 4, 60);
    assert_eq!(
        grid.display_offset(),
        0,
        "precondition: the re-anchor alone put the viewport at the live bottom"
    );
    assert_eq!(
        grid.storage.reader_live_bottom_gen, gen_after_reader,
        "a resize re-anchoring the viewport is machine motion, and the reader only \
         went UP — neither is a descent to the live bottom"
    );

    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    let sb_after = grid.scrollback_lines();
    assert_eq!(
        grid.display_offset(),
        150usize.min(sb_after),
        "the reader never chose the live bottom — the resize put them there, so the \
         restore must still run (scrollback={sb_after})"
    );
    grid.assert_invariants();
}

#[test]
fn output_batch_pin_dance_is_not_a_reader_gesture() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 500);

    grid.scroll_display(20); // the reader goes up: real, but not a descent
    let after_reader = grid.storage.reader_live_bottom_gen;

    let pinned = grid.display_offset();
    grid.pin_viewport_to_live_for_output_batch();
    assert_eq!(
        grid.display_offset(),
        0,
        "the prologue forces the precondition"
    );
    grid.repin_display_offset(pinned, 0);
    assert_eq!(
        grid.display_offset(),
        20,
        "the epilogue puts the reader back"
    );

    assert_eq!(
        grid.storage.reader_live_bottom_gen, after_reader,
        "the machine's force-to-live-and-back must be invisible to the reader's \
         viewport-gesture record"
    );

    grid.scroll_to_bottom();
    assert_eq!(
        grid.storage.reader_live_bottom_gen,
        after_reader + 1,
        "an End that moved the viewport IS a gesture"
    );
    grid.scroll_to_bottom();
    assert_eq!(
        grid.storage.reader_live_bottom_gen,
        after_reader + 1,
        "an End at a viewport already pinned to the live bottom moves nothing and \
         says nothing"
    );
    grid.assert_invariants();
}

/// The `aterm_log` records (level and message) written on this thread while `body` runs.
fn logged_on_this_thread<T>(body: impl FnOnce() -> T) -> (T, Vec<String>) {
    use std::cell::RefCell;
    thread_local! {
        static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }
    struct Capture;
    impl aterm_log::Log for Capture {
        fn enabled(&self, _: &aterm_log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &aterm_log::Record<'_>) {
            LINES.with(|lines| {
                if let Some(lines) = lines.borrow_mut().as_mut() {
                    lines.push(format!("{} {}", record.level(), record.args()));
                }
            });
        }
        fn flush(&self) {}
    }
    static CAPTURE: Capture = Capture;
    let _ = aterm_log::set_logger(&CAPTURE);
    aterm_log::set_max_level(aterm_log::LevelFilter::Trace);
    LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
    let out = body();
    (
        out,
        LINES.with(|lines| lines.borrow_mut().take().unwrap_or_default()),
    )
}

#[test]
fn a_reflow_windows_cap_is_logged_at_its_first_drop_and_its_close() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 64_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 64, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
    let streamed = 55_000;
    let before = grid.truncated_lines();

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    let ((), lines) = logged_on_this_thread(|| {
        for i in 0..streamed {
            short_line(&mut grid, rows, &format!("W{i}"));
        }
        grid.reattach_reflowed_scrollback(pending.reflow());
    });
    let dropped = grid.truncated_lines() - before;
    assert!(
        dropped > 4_000,
        "the flood went past the cap: {dropped} dropped"
    );
    let capped: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("reflow window:"))
        .collect();
    assert_eq!(
        capped.len(),
        2,
        "one window's cap: its first drop and its total, not a line per streamed \
         line ({} lines): {:?}",
        capped.len(),
        capped.iter().take(3).collect::<Vec<_>>()
    );
    assert!(
        capped[1].contains(&dropped.to_string()),
        "the close carries the window's whole count ({dropped}): {}",
        capped[1]
    );
    grid.assert_invariants();

    let before = grid.truncated_lines();
    let pending = grid
        .resize_offloading_scrollback(rows, cols)
        .expect("offload job");
    let ((), lines) = logged_on_this_thread(|| {
        for i in 0..streamed {
            short_line(&mut grid, rows, &format!("X{i}"));
        }
        drop(pending);
        grid.abort_reflow_offload();
    });
    let dropped = grid.truncated_lines() - before;
    let capped: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("reflow window:"))
        .collect();
    assert_eq!(capped.len(), 2, "{capped:?}");
    assert!(
        capped[1].contains(&dropped.to_string()),
        "a fresh window counts from zero ({dropped}): {}",
        capped[1]
    );
    grid.assert_invariants();
}

#[test]
fn offloaded_width_change_reads_history_bounded_by_the_screen() {
    let (rows, cols) = (24u16, 80u16);
    let reads = |line_rows: usize| {
        let mut grid = tiered_grid_with_short_history(rows, cols, 8, 200);
        grid.set_cursor(rows - 1, 0);
        let letters = b"abcdefghijklmnopqrstuvwxyz";
        for i in 0..line_rows * usize::from(cols) {
            grid.write_char_wrap(char::from(letters[i % letters.len()]));
        }
        grid.carriage_return();
        grid.line_feed();
        for c in "$ ".chars() {
            grid.write_char(c);
        }
        assert!(
            grid.row(0).is_some_and(|row| row.is_wrapped()),
            "precondition: the screen's top row continues the line"
        );
        let _ = take_handback_history_reads();
        let mut job = grid.resize_offloading_scrollback(rows, cols - 1);
        let read = take_handback_history_reads();
        while let Some(pending) = job {
            job = grid.reattach_reflowed_scrollback_or_redetach(pending.reflow());
        }
        grid.assert_invariants();
        read
    };
    let walk = 4 * usize::from(rows);
    let near = reads(40);
    assert!(
        near > 0 && near <= walk,
        "a head 17 rows up is read and brought onto the screen: {near}"
    );
    let far = reads(500);
    assert!(far <= walk + usize::from(rows), "{far}");
    assert_eq!(
        reads(5000),
        far,
        "what it reads does not grow with the line"
    );
}
