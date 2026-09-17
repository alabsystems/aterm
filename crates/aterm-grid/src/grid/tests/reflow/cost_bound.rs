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

/// RFL-1, the categorical claim: the offloaded resize path rewraps ZERO history
/// lines synchronously — not "a bounded ring's worth", none. The tiered store
/// detaches O(1), the ring history is lifted into the job as materialized
/// `Line`s (no rewrap), and the resize's synchronous reflow sink sees an empty
/// off-screen history. Strictly tightens
/// `offloaded_resize_keeps_synchronous_reflow_within_the_ring` (kept above as
/// the coarse budget guard): a regression that re-routes ANY history through
/// the synchronous sink — ring or tiered — turns this red.
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

/// Build a tiered grid whose history is SOFT-WRAPPED at `cols`: `n` logical lines
/// of `cols + 30` chars, so each occupies two rows at `cols` and exactly one once
/// the width passes their length — a widen that genuinely unwraps.
fn tiered_grid_with_wrapped_history(rows: u16, cols: u16, n: u16) -> Grid {
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
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

/// The reported gesture, at its commonest: a reader scrolled back into a shell log
/// of SHORT lines changes the window WIDTH. Nothing wraps at either width, so the
/// row numbering is identical on both sides of the resize and the reader must still
/// be looking at exactly the same line, character for character.
///
/// Before the fix the detach emptied all three history layers into the job, the
/// synchronous resize clamped `display_offset` against the resulting ZERO, and
/// re-attach read that self-inflicted 0 as "the reader pressed End" and declined to
/// restore — the viewport teleported 150 rows down to the live prompt.
#[test]
fn offload_width_change_keeps_an_unwrapped_reader_on_the_same_line() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
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

/// The same gesture on WRAPPED content — scroll back through a build log, then
/// widen to see the long lines. The widen unwraps history (the retained row count
/// halves), so no exact anchor exists; the contract is the CLAMPED pre-resize
/// offset, which still leaves the reader in history rather than at the live prompt.
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

/// RFL-3's follow-up detach removes the store (and drains the lazy buffer) with no
/// resize to re-clamp behind it, so the same law applies: the viewport must never be
/// left pointing past the end of the history that is still HOME, and the reader's
/// position must come back when the converging pass re-attaches.
#[test]
fn redetach_clamps_the_viewport_and_still_restores_the_reader() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_wrapped_history(rows, cols, 300);
    grid.scroll_display(150);
    assert_eq!(grid.display_offset(), 150, "precondition: 150 rows up");

    // Width change A detaches; a superseding width change B lands mid-flight, so
    // re-attach converges with one more detach at the settled width (RFL-3).
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

/// The audit-#7 exception is a SIGNAL, and a signal needs a baseline. Pressing End
/// at a viewport the detach has already pinned to the live bottom moves nothing and
/// therefore says nothing — so the restore still runs. Pinned deliberately: reading
/// that indistinguishable 0 as the reader's own choice is exactly the misattribution
/// that threw every scrolled-back reader to the live prompt on every width change.
#[test]
fn offload_window_end_at_an_already_pinned_bottom_is_not_a_signal() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
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

/// And the third: `abort_reflow_offload` discards the lazy buffer when the worker
/// dies, so the window output a reader had scrolled up over stops existing. The
/// viewport must come back down with it, not keep addressing rows no layer holds.
#[test]
fn abort_reclamps_the_viewport_when_the_staged_window_output_is_discarded() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }

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

/// A reader who MOVED the viewport during the window and ended at the live bottom
/// chose the live bottom — on the WIDEN axis, where a detach-time offset baseline
/// is blind.
///
/// Wheel up over the output that streamed in during the reflow, then wheel back
/// down to live. Both moves are real (offset 0 -> 20 -> 0), both are the reader's,
/// and the second is the descent audit #7 exists to honor. Nothing about the two
/// ENDPOINTS says so — the reader starts and finishes at 0, and the offset the
/// detach left is 0 too on every widen — so only a record of the window itself can
/// tell this apart from a reader who never touched anything.
#[test]
fn offload_window_reader_who_moved_then_returned_to_live_chose_it() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
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
         bottom chose it — do not yank them back into history (audit #7)"
    );
    grid.assert_invariants();
}

/// The same law reached by the other gesture: scroll up over the streaming output,
/// then press End. The position the reader descends FROM only exists because window
/// output staged into the lazy buffer, so it is invisible to any baseline sampled at
/// the detach — and the content under it is real, which this test checks before
/// pressing End.
#[test]
fn offload_window_end_after_scrolling_over_window_output_is_honored() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
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
         bottom, whatever the detach had left behind (audit #7)"
    );
    grid.assert_invariants();
}

/// And inside RFL-3's CONVERGING window, which has its own detach and so its own
/// baseline: the reader's End there must be honored exactly the same way.
#[test]
fn redetach_window_end_after_scrolling_over_window_output_is_honored() {
    let (rows, cols) = (10u16, 80u16);
    let mut grid = tiered_grid_with_wrapped_history(rows, cols, 300);
    grid.scroll_display(150);

    // Width change A detaches; a superseding width change B lands mid-flight, so the
    // re-attach converges with one more detach at the settled width (RFL-3).
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
         End is honored there too (audit #7)"
    );
    grid.assert_invariants();
}

/// A reader who scrolled UP during the window did not choose the live bottom — so
/// when the MACHINE afterwards puts them there, the restore must still run.
///
/// This is why the descent record is `non-zero -> zero`, not "the offset changed".
/// Under the looser reading this reader's wheel-up counts as "the reader touched the
/// viewport", a height drag lands mid-window and re-anchors them onto the live
/// bottom, and the two together are read as an End press: the restore is skipped and
/// they finish at the prompt. Same misattribution as the bug this all started with,
/// just sourced from the resize instead of from the detach.
#[test]
fn offload_window_reader_who_only_scrolled_up_did_not_choose_the_bottom() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }
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

/// SCR-1's output pin dance — force the viewport to live for the duration of a
/// batch, then re-pin the reader onto the same content — is the MACHINE moving the
/// viewport twice and the reader moving it zero times.
///
/// It routes a genuine >0 -> 0 descent through the grid on every batch of output
/// that arrives while someone is reading history, so a gesture record taken at
/// `reset_display_offset_with_damage` (or anywhere the dance passes through) would
/// call `tail -f` an End press. Pinned here because the offload guard's whole claim
/// is that its baseline separates the reader from the machine.
#[test]
fn output_batch_pin_dance_is_not_a_reader_gesture() {
    let (rows, cols) = (10u16, 80u16);
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, cols, 8, sb);
    for i in 0..500 {
        short_line(&mut grid, rows, &format!("H{i}"));
    }

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
