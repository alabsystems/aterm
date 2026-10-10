// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded-cost obligation for the width-change scrollback reflow.
//!
//! A width change rewraps the entire off-screen scrollback
//! (`take_scrollback_lines` + `reflow_scrollback_lines`) synchronously under
//! the caller's lock, costing O(session history) rather than O(viewport); on
//! the GUI main thread under the per-session `term` mutex that can freeze the
//! UI for seconds after a long session.
//!
//! Instead of timing a resize, these tests count how many history lines it
//! rewraps synchronously: `count_scrollback_reflow_sync_lines` fires at the
//! synchronous site inside `Grid::resize` (reflow.rs), where `old.len()` is the
//! history line count.
//!
//! [`naive_width_resize_rewraps_whole_history_synchronously`] pins that the
//! direct path scales with history, and
//! `offloaded_resize_keeps_synchronous_reflow_within_the_ring` pins that
//! `Grid::resize_offloading_scrollback` does not.

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

/// A direct `Grid::resize` width change rewraps the entire off-screen
/// scrollback on the caller's thread, scaling with history. Non-GUI callers
/// still use this synchronously; the GUI path uses the offload, whose
/// synchronous count stays O(ring).
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

    // The direct path rewraps nearly all of history synchronously; this pins
    // the magnitude the offloaded path is measured against.
    assert!(
        sync >= sb,
        "naive Grid::resize rewraps the whole history synchronously \
         (sync={sync}, scrollback={sb})"
    );
    grid.assert_invariants();
}

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

/// Build a grid whose bulk history lives in the TIERED store (small ring so most
/// scroll-off spills to tiered, exactly like a real session).
fn tiered_grid_with_deep_history(rows: u16, cols: u16, n: u16) -> Grid {
    // Small ring (8) → most of the `n` lines spill into the tiered store.
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

/// The offloaded resize path rewraps only a bounded (ring) number of history
/// lines synchronously; the tiered history is detached in O(1) and rewrapped
/// off-thread. History is preserved.
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

/// The offloaded resize path rewraps zero history lines synchronously: the
/// tiered store detaches in O(1), the ring history is lifted into the job as
/// materialized `Line`s, and the synchronous reflow sink sees no off-screen
/// history. Tightens `offloaded_resize_keeps_synchronous_reflow_within_the_ring`:
/// any history routed through the synchronous sink fails this.
#[test]
fn offloaded_resize_synchronous_rewrap_is_zero() {
    let (rows, cols) = (24u16, 80u16);
    let mut grid = tiered_grid_with_deep_history(rows, cols, 2000);
    let ring_before = grid.ring_buffer_scrollback();
    assert!(
        ring_before > 0,
        "precondition: ring scrollback exists (got {ring_before}) — otherwise \
         the zero below would be vacuous for the ring half of the claim"
    );

    let _ = take_scrollback_reflow_sync_lines(); // reset
    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("a width change with a tiered store yields an offload job");
    let sync = take_scrollback_reflow_sync_lines();
    assert_eq!(
        sync, 0,
        "the offloaded resize must rewrap NO history lines synchronously — the \
         ring history rides the job (RFL-1), the tiered store is detached"
    );
    assert!(
        pending.line_count() > 1000 + ring_before,
        "the job carries BOTH tiers: deep tiered history plus the {ring_before} \
         ring lines (line_count = {})",
        pending.line_count()
    );

    grid.reattach_reflowed_scrollback(pending.reflow());
    assert!(
        grid.scrollback_lines() > 1000,
        "history preserved after the zero-synchronous round trip"
    );
    grid.assert_invariants();
}

/// Output produced during the reflow window is not dropped. Short lines so the
/// 80→40 reflow does not change the line count; with a ring cap of 8, uncaptured
/// window lines would be evicted with nowhere to go.
#[test]
fn offload_window_captures_concurrent_scrolloff_no_gap() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 1000);
    let before = grid.scrollback_lines();
    assert!(before > 500, "precondition: deep history ({before})");

    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    // Foreground program keeps streaming while the worker rewraps.
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

/// Scrollback erased during the reflow window is not resurrected by the worker
/// re-attaching the stale pre-erase store.
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

/// A reader scrolled deep into history keeps their position across an offloaded
/// resize (not collapsed to the ring-only count during the detach).
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

/// A height-shrink resize landing during the reflow window (find bar, bottom
/// drag, vertical split) keeps the ring scrollback: with the store detached,
/// front ring rows (window output) are staged to the lazy buffer, not dropped.
/// Short lines so the 80→40 reflow does not change the count.
#[test]
fn offload_window_height_shrink_keeps_ring_scrollback() {
    let (rows, cols) = (10u16, 80u16);
    // Large ring cap so a big block of window output sits in the ring (not yet spilled
    // to lazy) at the moment the height shrink arrives — that block is what the bug drops.
    let mut grid = tiered_grid_with_short_history(rows, cols, 128, 1000);
    let before = grid.scrollback_lines();
    assert!(before > 500, "precondition: deep history ({before})");

    // Detach for the off-thread reflow (width 80 -> 40).
    let pending = grid
        .resize_offloading_scrollback(rows, cols / 2)
        .expect("offload job");
    // Foreground program streams while the worker rewraps: the ring fills to its cap
    // (128) and the overflow stages to the lazy buffer.
    for i in 0..600 {
        short_line(&mut grid, rows, &format!("W{i}")); // short: no wrap at 40
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

/// If the reflow worker panics (or its thread dies) mid-rewrap it never
/// re-attaches, so `abort_reflow_offload` must close the detach window — otherwise
/// `scrollback_detached_for_reflow` stays true forever, every scroll-off stages into
/// an un-drainable lazy buffer (unbounded leak) and all tiered history is invisible.
#[test]
fn offload_abort_recovers_grid_to_bounded_state() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 64, 500);

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

/// A reader who follows output to the live bottom during the window (e.g.
/// presses End) stays there on re-attach instead of returning to the old deep
/// position; a reader who did not scroll keeps their deep position
/// (`offload_preserves_deep_scroll_position`).
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

/// Heavy streaming through a long reflow window does not grow the lazy buffer
/// without bound (drain is suppressed while the store is detached): the buffer
/// is capped, dropping the oldest staged lines.
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

    // The cap engaged: ~50k window lines survived and the ~5k oldest were
    // dropped (a non-detached grid would have tiered all 55k).
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

/// Build a tiered grid whose history is SOFT-WRAPPED at `cols`: `n` logical lines
/// of `cols + 30` chars, so each occupies two rows at `cols` and exactly one once
/// the width passes their length — a widen that genuinely unwraps.
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

/// A reader scrolled back into a log of short lines changes the window width.
/// Nothing wraps at either width, so the row numbering is identical and the
/// reader must see exactly the same line afterwards. (The detach's own clamp to
/// an empty history must not be read as the reader pressing End.)
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

/// The same gesture on wrapped content: the widen unwraps history, so no exact
/// anchor exists; the reader gets the clamped pre-resize offset and stays in
/// history rather than at the live prompt.
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

/// The converging follow-up detach removes the store (and drains the lazy
/// buffer) with no resize to re-clamp: the viewport must never point past the
/// history still attached, and the reader's position comes back when the
/// converging pass re-attaches.
#[test]
fn redetach_clamps_the_viewport_and_still_restores_the_reader() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_wrapped_history(rows, cols, 300);
    grid.scroll_display(150);
    assert_eq!(grid.display_offset(), 150, "precondition: 150 rows up");

    // Width change A detaches; a superseding width change B lands mid-flight, so
    // re-attach converges with one more detach at the settled width.
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

/// Pressing End at a viewport the detach already pinned to the live bottom
/// moves nothing and says nothing, so the restore still runs.
#[test]
fn offload_window_end_at_an_already_pinned_bottom_is_not_a_signal() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 500);
    grid.scroll_display(150);

    let pending = grid
        .resize_offloading_scrollback(rows, 60)
        .expect("offload job");
    // Short lines rewrap into nothing, so the detach left the viewport at 0 already.
    assert_eq!(
        grid.display_offset(),
        0,
        "precondition: the detach itself pinned the viewport to the live bottom"
    );
    grid.scroll_to_bottom(); // a no-op End press: it moves nothing
    let reflowed = pending.reflow();
    grid.reattach_reflowed_scrollback(reflowed);

    assert_eq!(
        grid.display_offset(),
        150,
        "an End that moved nothing is not evidence the reader chose the bottom"
    );
    grid.assert_invariants();
}

/// `abort_reflow_offload` discards the lazy buffer when the worker dies, so
/// window output a reader had scrolled up over is gone; the viewport comes down
/// with it rather than addressing rows no layer holds.
#[test]
fn abort_reclamps_the_viewport_when_the_staged_window_output_is_discarded() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 500);

    let pending = grid
        .resize_offloading_scrollback(rows, 60)
        .expect("offload job");
    // Window output stages into the lazy buffer (the store is out with the job);
    // the reader scrolls up over it.
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

/// A reader who moved the viewport during the window and ended at the live
/// bottom chose it, on the widen axis where a detach-time offset baseline is
/// blind: wheel up over output that streamed in during the reflow, then back
/// down to live (offset 0 -> 20 -> 0). Both endpoints and the detach's offset
/// are 0, so only a record of the window distinguishes this from a reader who
/// touched nothing.
#[test]
fn offload_window_reader_who_moved_then_returned_to_live_chose_it() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 500);
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

/// A grid holding `n` short history lines, detached for a width change with the
/// reader at `offset` beforehand: the job, and the grid with nothing attached
/// above the viewport (short lines rewrap into nothing, and the detach lifts
/// the ring).
fn detached_with_reader_at(offset: i32, n: usize) -> (Grid, PendingScrollbackReflow) {
    let rows = 10u16;
    let mut grid = tiered_grid_with_short_history(rows, 80, 8, n);
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

/// A scroll up while the history is away is not lost: the wheel moves nothing
/// (nothing is attached above the viewport), but the re-attach lands the reader
/// where they asked. The view never moves in the window, and nothing is
/// recorded outside one.
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

    // Outside a window the same gesture is only a scroll.
    grid.scroll_display(5);
    assert_eq!(grid.display_offset(), 35);
    assert!(!grid.reader_aim_held());
}

/// Down spends the aim first; all the way back down is the reader choosing the
/// live bottom — even over a restore that would have put them 150 rows up.
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

    // End (and a keystroke's snap, which is the same primitive) after an aim.
    let (mut grid, pending) = detached_with_reader_at(150, 500);
    grid.scroll_display(30);
    grid.scroll_to_bottom();
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(grid.display_offset(), 0, "the End after the aim is honored");
    grid.assert_invariants();
}

/// Top aims at the top of the whole history; an aim below the restore never
/// pulls the reader down from where the restore puts them; with no aim the
/// restore stands exactly (including an End that moved nothing).
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

/// Output that scrolls into history after the ask pushes the aim up with it (the
/// per-batch repin rule), so the reader lands on the content they asked for.
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

/// An abort drops the aim with the history it pointed into.
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

/// The same rule via scroll up over the streaming output, then End. The
/// position the reader descends from exists only because window output staged
/// into the lazy buffer, so a detach-time baseline cannot see it; the test
/// checks the content under it is real before pressing End.
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

/// And inside the converging window, which has its own detach and so its own
/// baseline: the reader's End there must be honored exactly the same way.
#[test]
fn redetach_window_end_after_scrolling_over_window_output_is_honored() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_wrapped_history(rows, cols, 300);
    grid.scroll_display(150);

    // Width change A detaches; a superseding width change B lands mid-flight, so the
    // re-attach converges with one more detach at the settled width.
    let pending = grid
        .resize_offloading_scrollback(rows, cols + 40)
        .expect("offload job A");
    grid.resize(rows, cols + 20); // supersedes: detaches nothing, rewraps the ring
    let reflowed = pending.reflow();
    let follow = grid
        .reattach_reflowed_scrollback_or_redetach(reflowed)
        .expect("stale width must converge with a follow-up job");

    // The SECOND window: output streams, the reader scrolls up over it, then End.
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

/// A reader who scrolled up during the window did not choose the live bottom,
/// so when the machine later puts them there the restore still runs.
///
/// Hence the descent record is `non-zero -> zero`, not "the offset changed":
/// otherwise the wheel-up plus a height drag re-anchoring them to the live
/// bottom would read as an End press and leave them at the prompt.
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

    // A height drag lands mid-window: growing the viewport by 4 rows pulls the
    // anchored line down to the live bottom all on its own.
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

/// The per-batch output pin (force the viewport live for a batch, then re-pin
/// the reader onto the same content) is the machine moving the viewport, not
/// the reader: it passes through a >0 -> 0 descent on every batch while
/// someone reads history, so a gesture record there would call `tail -f` an
/// End press.
#[test]
fn output_batch_pin_dance_is_not_a_reader_gesture() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_short_history(rows, cols, 8, 500);

    grid.scroll_display(20); // the reader goes UP: real, but not a descent
    let after_reader = grid.storage.reader_live_bottom_gen;

    // Exactly what `Terminal::process` does around a batch (processing.rs) and what
    // `flatten_restored_display_offset` does on an rmcup (handler_dec.rs).
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

    // …while the reader's own End at the same offset is not.
    grid.scroll_to_bottom();
    assert_eq!(
        grid.storage.reader_live_bottom_gen,
        after_reader + 1,
        "an End that moved the viewport IS a gesture"
    );
    // And one that moves nothing still is not.
    grid.scroll_to_bottom();
    assert_eq!(
        grid.storage.reader_live_bottom_gen,
        after_reader + 1,
        "an End at a viewport already pinned to the live bottom moves nothing and \
         says nothing"
    );
    grid.assert_invariants();
}

/// The `aterm_log` records (level and message) written on THIS thread while
/// `body` runs. The one logger this test binary installs keeps only the
/// records of a thread that is capturing, so tests running beside it add none.
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

/// THE LOG FLOOD: the detached-reflow window's staging cap is a fact about the
/// WINDOW. Once a program streams past it, every further line it prints drops
/// one staged line, on the PTY reader under the terminal lock; a WARN per drop
/// let one width resize during a flood roll `aterm.log`'s 8 MiB pair over twice
/// in under a second (measured on a release binary: `scrollback_lines =
/// 1 000 000`, `yes | head` and eight `ctl resize`), audit lines and all. The
/// window writes its first drop and, when it closes, its total: two lines per
/// window, whether it re-attaches or aborts, and every drop is still counted.
#[test]
fn a_reflow_windows_cap_is_logged_at_its_first_drop_and_its_close() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 64_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 64, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
    // Past the window's 50 000-line staging cap (`Grid::DETACHED_LAZY_CAP`).
    let streamed = 55_000;
    let before = grid.truncated_lines();

    // Window 1: a flood through it, then the re-attach.
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
        capped[0].starts_with("WARN reflow window: lazy buffer exceeded 50000 lines"),
        "{}",
        capped[0]
    );
    assert_eq!(
        *capped[1],
        format!(
            "WARN reflow window: closed after dropping {dropped} staged line(s) past its \
             50000-line cap"
        ),
        "the close carries the window's whole count"
    );
    grid.assert_invariants();

    // Window 2 opens at zero and ends by abort: its own two lines.
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
    assert!(capped[0].contains("lazy buffer exceeded"), "{}", capped[0]);
    assert!(
        capped[1].contains(&format!("closed after dropping {dropped} staged line(s)")),
        "a fresh window counts from zero: {}",
        capped[1]
    );
    grid.assert_invariants();
}
