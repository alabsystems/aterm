// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded-cost obligation for the width-change scrollback reflow (the L0
//! whole-Mac-freeze class).
//!
//! The hazard: a width change rewraps the ENTIRE off-screen scrollback
//! (`take_scrollback_lines` + `reflow_scrollback_lines`) SYNCHRONOUSLY on the
//! caller's thread, under the caller's lock — cost O(session history), not
//! O(viewport). On the GUI main thread, under the per-session `term` mutex,
//! that is the 42s whole-Mac freeze observed after a long session.
//!
//! This turns "how long does a resize take" (unprovable wall-clock) into "how
//! many history lines does a resize rewrap SYNCHRONOUSLY" — a bounded VALUE
//! predicate the counter makes checkable. `count_scrollback_reflow_sync_lines`
//! fires at the exact synchronous site inside `Grid::resize` (reflow.rs), where
//! `old.len()` is precisely the session-history line count.
//!
//! [`naive_width_resize_rewraps_whole_history_synchronously`] pins the hazard:
//! today the synchronous reflow scales with history. It is the *teeth* — a
//! regression guard that fails the instant the fix's off-thread path is
//! bypassed and full history is rewrapped synchronously again. The fix's GREEN
//! acceptance test (`offloaded_resize_keeps_synchronous_reflow_within_the_ring`)
//! lands together with `Grid::resize_offloading_scrollback`.

use super::super::super::*;
use crate::test_counters::take_scrollback_reflow_sync_lines;
use aterm_scrollback::{Scrollback, ScrollbackStorage};

/// Fill a scrolling grid with `n` distinct near-full-width logical lines so most
/// of them land in off-screen scrollback, then return (grid, scrollback_len).
fn grid_with_deep_scrollback(rows: u16, cols: u16, n: u16) -> (Grid, usize) {
    // Ring cap well above `n` so nothing is evicted before the resize.
    let mut grid = Grid::with_scrollback(rows, cols, (n as usize) + 100);
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
    let sb = grid.scrollback_lines();
    (grid, sb)
}

/// TODAY'S HAZARD, pinned: a width change rewraps the entire off-screen
/// scrollback on the caller's thread. The synchronous reflow count scales with
/// history — the exact O(session-history)-under-lock shape that froze the Mac.
///
/// This test PASSES today (documenting the bug) and is the regression guard the
/// offload must keep honest: the fix moves this work off-thread, so the GUI
/// path's synchronous count drops to O(ring), while this direct-`Grid::resize`
/// call — which non-GUI callers still use synchronously — stays O(history).
#[test]
fn naive_width_resize_rewraps_whole_history_synchronously() {
    let (rows, cols) = (24u16, 80u16);
    let (mut grid, sb) = grid_with_deep_scrollback(rows, cols, 2000);
    assert!(
        sb > 1000,
        "precondition: deep off-screen scrollback ({sb} lines)"
    );

    let _ = take_scrollback_reflow_sync_lines(); // reset
    grid.resize(rows, cols / 2); // width change → the reflow sink
    let sync = take_scrollback_reflow_sync_lines();

    // The naive path rewraps ~all of history synchronously. A resize whose
    // synchronous cost grows with session history is the L0 freeze; this pins
    // the magnitude so the fix's off-thread path can be measured against it.
    assert!(
        sync >= sb,
        "naive Grid::resize rewraps the whole history synchronously \
         (sync={sync}, scrollback={sb}) — this is the bounded-cost violation the \
         offload fixes"
    );
    grid.assert_invariants();
}

/// Build a grid whose bulk history lives in the TIERED store (small ring so most
/// scroll-off spills to tiered, exactly like a real session).
fn tiered_grid_with_deep_history(rows: u16, cols: u16, n: u16) -> Grid {
    // Small ring (8) → most of the `n` lines spill into the tiered store.
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
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

/// THE FIX'S CONTRACT: the offloaded resize path rewraps only a bounded (ring)
/// number of history lines SYNCHRONOUSLY; the unbounded tiered history is
/// detached in O(1) and rewrapped off-thread. History is preserved.
#[test]
fn offloaded_resize_keeps_synchronous_reflow_within_the_ring() {
    let (rows, cols) = (24u16, 80u16);
    let mut grid = tiered_grid_with_deep_history(rows, cols, 2000);

    let tiered_before = grid.scrollback().map_or(0, |s| s.line_count());
    let total_before = grid.scrollback_lines();
    assert!(
        tiered_before > 1000,
        "precondition: the bulk of history is in the tiered store \
         (tiered={tiered_before}, total={total_before})"
    );

    // Bound: the synchronous reflow must not scale with session history. The ring
    // is fixed-size (8), so a few viewports is a generous, lifetime-independent cap.
    let budget = (rows as usize) * 8;

    let _ = take_scrollback_reflow_sync_lines(); // reset
    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("a width change with a tiered store yields an offload job");
    let sync = take_scrollback_reflow_sync_lines();

    assert!(
        sync <= budget,
        "offloaded resize must rewrap <= {budget} history lines synchronously, \
         got {sync} (tiered history {tiered_before} was detached, not rewrapped \
         on-thread)"
    );
    assert!(
        pending.line_count() > 1000,
        "the detached job carries the deep history for off-thread rewrap"
    );

    // The expensive step, off-thread in production; inline here.
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    // History survived the round trip (rewrap can change the exact line count,
    // so assert it is back in the same order of magnitude, not lost).
    let total_after = grid.scrollback_lines();
    assert!(
        total_after > 1000,
        "history preserved after offload round trip (before={total_before}, \
         after={total_after})"
    );
    grid.assert_invariants();
}

/// Audit bug B: output produced DURING the reflow window must not be dropped.
/// Short lines so the 80→40 reflow doesn't change the line count, isolating the
/// window contribution. Ring cap is 8, so without the capture fix ~all 500
/// window lines would be evicted with nowhere to go.
#[test]
fn offload_window_captures_concurrent_scrolloff_no_gap() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
    fn line(g: &mut Grid, rows: u16, s: &str) {
        g.set_cursor(rows - 1, 0);
        for c in s.chars() {
            g.write_char(c);
        }
        g.line_feed();
        g.carriage_return();
    }
    for i in 0..1000 {
        line(&mut grid, rows, &format!("H{i}")); // short: no wrap at 40
    }
    let before = grid.scrollback_lines();
    assert!(before > 500, "precondition: deep history ({before})");

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    // Foreground program keeps streaming while the worker rewraps.
    for i in 0..500 {
        line(&mut grid, rows, &format!("W{i}"));
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

/// Audit bug C: scrollback ERASED during the reflow window must not be resurrected
/// by the worker re-attaching the stale pre-erase store.
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

/// Audit bug D: a reader scrolled deep into history keeps their position across an
/// offloaded resize (not collapsed to the ring-only count during the detach).
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

/// Audit bug B, residual hole: a HEIGHT-shrink resize that lands DURING the reflow
/// window (find-bar open / bottom-edge drag / vertical split) must not drop the ring
/// scrollback. While the store is detached, `adjust_row_count` gated eviction on the
/// raw `scrollback.is_some()` (false during the window) and dropped the front ring
/// rows — window output — instead of staging them to the lazy buffer. Short lines so
/// the 80→40 reflow does not change the count, isolating the window contribution.
#[test]
fn offload_window_height_shrink_keeps_ring_scrollback() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    // Large ring cap so a big block of window output sits in the ring (not yet spilled
    // to lazy) at the moment the height shrink arrives — that block is what the bug drops.
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 128, sb);
    fn line(g: &mut Grid, rows: u16, s: &str) {
        g.set_cursor(rows - 1, 0);
        for c in s.chars() {
            g.write_char(c);
        }
        g.line_feed();
        g.carriage_return();
    }
    for i in 0..1000 {
        line(&mut grid, rows, &format!("H{i}")); // short: no wrap at 40
    }
    let before = grid.scrollback_lines();
    assert!(before > 500, "precondition: deep history ({before})");

    // Detach for the off-thread reflow (width 80 -> 40).
    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    // Foreground program streams while the worker rewraps: the ring fills to its cap
    // (128) and the overflow stages to the lazy buffer.
    for i in 0..600 {
        line(&mut grid, rows, &format!("W{i}")); // short: no wrap at 40
    }
    // A HEIGHT-shrink resize lands mid-window. Width unchanged (40) + store detached
    // => resize_offloading_scrollback early-returns to a plain resize, driving
    // adjust_row_count with the store still None.
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

fn short_line(g: &mut Grid, rows: u16, s: &str) {
    g.set_cursor(rows - 1, 0);
    for c in s.chars() {
        g.write_char(c);
    }
    g.line_feed();
    g.carriage_return();
}

/// Audit #5: if the reflow worker panics (or its thread dies) mid-rewrap it never
/// re-attaches, so `abort_reflow_offload` must close the detach window — otherwise
/// `scrollback_detached_for_reflow` stays true forever, every scroll-off stages into
/// an un-drainable lazy buffer (unbounded leak) and all tiered history is invisible.
#[test]
fn offload_abort_recovers_grid_to_bounded_state() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 64, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }

    // Detach for a reflow, then simulate the worker dying: drop the pending job
    // WITHOUT re-attaching (its owned tiered store is gone), then abort.
    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    drop(pending); // worker "panicked" — reattach never runs
    grid.abort_reflow_offload();

    // The window is closed: streaming must stay BOUNDED (ring-only; the lazy buffer
    // discards rather than accumulating), not leak every scrolled-off line.
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

/// Audit #7: a reader who follows output to the live bottom DURING the window (e.g.
/// presses End) must stay there on re-attach, not be yanked back to their stale
/// pre-detach deep position. Must NOT regress bug D (a reader who did NOT scroll
/// keeps their deep position — covered by offload_preserves_deep_scroll_position).
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
         not be yanked back up to the stale deep position (audit #7)"
    );
    grid.assert_invariants();
}

/// Audit #4: heavy streaming through a long reflow window must not grow the lazy
/// buffer without bound (the tiered store is detached, so drain is suppressed). The
/// buffer is capped, dropping the OLDEST staged lines beyond the cap.
#[test]
fn offload_window_lazy_buffer_is_bounded() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 64_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 64, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
    let before = grid.scrollback_lines();

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    // Stream more than the DETACHED_LAZY_CAP (50_000) short lines during the window.
    for i in 0..55_000 {
        short_line(&mut grid, rows, &format!("W{i}"));
    }
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    // The cap engaged: ~50k window lines survived, the ~5k oldest were dropped (a
    // non-detached grid would have tiered all 55k, but that is the bounded-vs-OOM
    // trade the cap makes). Without the cap, all 55k would be kept (unbounded).
    let after = grid.scrollback_lines();
    assert!(
        after < before + 52_000,
        "the lazy buffer must be bounded during the window (before={before}, \
         after={after}) — keeping all 55k window lines means the cap did not engage"
    );
    assert!(
        after > before + 45_000,
        "the cap must keep ~50k window lines, not drop the whole buffer \
         (before={before}, after={after})"
    );
    grid.assert_invariants();
}
