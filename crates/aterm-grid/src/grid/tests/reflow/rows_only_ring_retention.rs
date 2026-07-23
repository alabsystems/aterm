// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rows-only resize retention on RING-ONLY grids (the wasm engines' shape).
//!
//! A rows-only resize must never shed ring history: the ring IS retention
//! when no tiered store is attached. Identity law: the resulting logical
//! buffer — history sequence, viewport content, absolute-row numbering —
//! matches what the SAME writes + resize produce on a tiered grid (whose
//! migration path has always preserved retention). Pre-fix,
//! `adjust_row_count` trimmed the ring to the viewport, destroying ALL
//! history and renumbering the survivors on every height-only change (orc:
//! search "staleness" after a height-only resize — history matches vanished
//! and surviving lines reported shifted absolute rows).

use super::super::super::*;
use aterm_scrollback::Scrollback;

fn write_numbered_lines(grid: &mut Grid, n: usize) {
    for i in 0..n {
        grid.carriage_return();
        for c in format!("N{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }
}

/// The full logical buffer: (history texts oldest-first, viewport texts,
/// oldest_absolute_row) — the identity-law comparison key.
fn logical_buffer(grid: &Grid) -> (Vec<String>, Vec<String>, u64) {
    let history: Vec<String> = (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect();
    let viewport: Vec<String> = (0..grid.rows())
        .map(|r| {
            grid.row_text(r)
                .map(|t| t.trim_end().to_string())
                .unwrap_or_default()
        })
        .collect();
    (history, viewport, grid.oldest_absolute_row())
}

fn ring_only_and_tiered_after(
    rows: u16,
    cols: u16,
    lines: usize,
    resize_to: (u16, u16),
) -> (Grid, Grid) {
    let mut ring = Grid::with_scrollback(rows, cols, 10_000);
    write_numbered_lines(&mut ring, lines);
    ring.resize(resize_to.0, resize_to.1);
    ring.assert_invariants();

    let sb = Scrollback::new(1000, 10_000, 100_000_000);
    let mut tiered = Grid::with_tiered_scrollback(rows, cols, 10_000, sb);
    write_numbered_lines(&mut tiered, lines);
    tiered.resize(resize_to.0, resize_to.1);
    tiered.assert_invariants();

    (ring, tiered)
}

#[test]
fn rows_only_shrink_preserves_ring_history_and_matches_tiered() {
    let (ring, tiered) = ring_only_and_tiered_after(10, 40, 30, (5, 40));

    // Nothing may be dropped: retention grows by the demoted bottom rows.
    assert_eq!(
        ring.scrollback_lines(),
        tiered.scrollback_lines(),
        "ring-only shrink must retain what the tiered twin retains"
    );
    assert_eq!(
        logical_buffer(&ring),
        logical_buffer(&tiered),
        "identity law: same history sequence, viewport, and absolute numbering"
    );

    // Spot-pin the shape so the identity cannot drift silently: 21 old
    // history lines + the 5 demoted bottom rows; viewport = old top rows;
    // numbering intact (nothing evicted).
    let (history, viewport, oldest_abs) = logical_buffer(&ring);
    assert_eq!(history.len(), 26);
    assert_eq!(history[0], "N0");
    assert_eq!(history[20], "N20");
    assert_eq!(&history[21..], ["N26", "N27", "N28", "N29", ""]);
    assert_eq!(viewport, ["N21", "N22", "N23", "N24", "N25"]);
    assert_eq!(
        oldest_abs, 0,
        "no eviction: absolute rows keep their identity"
    );
}

#[test]
fn rows_only_grow_reveals_history_and_matches_tiered() {
    let (ring, tiered) = ring_only_and_tiered_after(5, 40, 30, (10, 40));

    assert_eq!(
        ring.scrollback_lines(),
        tiered.scrollback_lines(),
        "ring-only grow must retain what the tiered twin retains"
    );
    assert_eq!(
        logical_buffer(&ring),
        logical_buffer(&tiered),
        "identity law: same history sequence, viewport, and absolute numbering"
    );

    // Spot-pin: 5 history lines re-enter the viewport, the rest stay history.
    let (history, viewport, oldest_abs) = logical_buffer(&ring);
    assert_eq!(history.len(), 21);
    assert_eq!(history[0], "N0");
    assert_eq!(history[20], "N20");
    assert_eq!(
        viewport,
        [
            "N21", "N22", "N23", "N24", "N25", "N26", "N27", "N28", "N29", ""
        ]
    );
    assert_eq!(oldest_abs, 0);
}

/// The cursor FOLLOWS its content through a rows-grow reveal: revealed
/// history re-labels the newest ring lines as the top of the viewport, so
/// every pre-resize viewport row — the cursor's included — sits `revealed`
/// rows further down afterwards. Pre-fix the cursor kept its old viewport
/// row, pointing `revealed` rows ABOVE its line; an inline TUI's
/// post-SIGWINCH repaint (and CPR answers) then anchored wrong, painting
/// into the revealed band. Pinned on BOTH grid shapes via the identity law.
#[test]
fn rows_only_grow_cursor_follows_its_content_line() {
    for tiered in [false, true] {
        let mut grid = if tiered {
            let sb = Scrollback::new(1000, 10_000, 100_000_000);
            Grid::with_tiered_scrollback(5, 40, 10_000, sb)
        } else {
            Grid::with_scrollback(5, 40, 10_000)
        };
        write_numbered_lines(&mut grid, 30);
        // The prompt line: cursor parked on the row holding "N29".
        let before_row = grid.cursor().row;
        let before_text = grid
            .row_text(before_row)
            .map(|t| t.trim_end().to_string())
            .unwrap_or_default();

        grid.resize(10, 40);
        grid.assert_invariants();

        let after_row = grid.cursor().row;
        let after_text = grid
            .row_text(after_row)
            .map(|t| t.trim_end().to_string())
            .unwrap_or_default();
        assert_eq!(
            after_text, before_text,
            "tiered={tiered}: the cursor must stay on its logical line \
             through the reveal (was row {before_row}, now {after_row})"
        );
        // 5 lines revealed (grow 5 -> 10 with ample history): the cursor row
        // shifted down by exactly the revealed count.
        assert_eq!(
            after_row,
            before_row + 5,
            "tiered={tiered}: cursor shifts by the revealed count"
        );
    }
}

/// A shrink whose demoted rows overflow the retention cap evicts ONLY past
/// the cap (oldest-first), exactly like scroll_up's at-capacity reuse.
#[test]
fn rows_only_shrink_at_cap_evicts_only_beyond_the_limit() {
    let mut grid = Grid::with_scrollback(5, 20, 30);
    write_numbered_lines(&mut grid, 100);
    assert_eq!(grid.scrollback_lines(), 30, "precondition: at the cap");
    let oldest_abs_before = grid.oldest_absolute_row();

    grid.resize(3, 20);
    grid.assert_invariants();

    // 2 demoted rows push history to 32; the cap evicts the 2 oldest.
    assert_eq!(grid.scrollback_lines(), 30, "capped, not wiped");
    assert_eq!(
        grid.oldest_absolute_row(),
        oldest_abs_before + 2,
        "eviction advances the oldest row by exactly the overflow"
    );
}

/// The alt-screen configuration (ring cap 0) keeps its no-scrollback
/// invariant through a rows-only shrink: demoted rows are evicted by the
/// zero cap, matching the pre-fix discard behavior.
#[test]
fn rows_only_shrink_with_zero_cap_still_retains_nothing() {
    let mut grid = Grid::with_scrollback(5, 20, 0);
    write_numbered_lines(&mut grid, 10);
    assert_eq!(grid.scrollback_lines(), 0);

    grid.resize(3, 20);
    grid.assert_invariants();
    assert_eq!(
        grid.scrollback_lines(),
        0,
        "zero cap: nothing may accumulate"
    );
    assert_eq!(grid.rows(), 3);
}

/// Ring history extras survive a rows-only resize in place: the ring is not
/// rebuilt, so a hyperlink that already lived in history must still resolve
/// afterwards (the tiered path clears `ring_extras` because it rebuilds; the
/// ring-only path keeps the rows AND their side table).
#[test]
fn rows_only_resize_keeps_ring_history_extras() {
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(3, 20, 10_000);
    // "Hello" with a hyperlink on row 0 (the #7783 extras pattern), then
    // enough lines to push it into ring history.
    let url: Arc<str> = Arc::from("https://example.com/rows-only");
    for c in "Hello".chars() {
        grid.write_char(c);
    }
    for col in 0..5u16 {
        grid.extras_mut()
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url.clone()));
    }
    grid.carriage_return();
    grid.line_feed();
    write_numbered_lines(&mut grid, 10);
    let sb = grid.scrollback_lines();
    assert!(sb > 0);
    let linked = grid
        .get_history_line(0)
        .expect("oldest history line present");
    assert!(
        linked.hyperlinks().is_some_and(|spans| !spans.is_empty()),
        "precondition: the scrolled-off line carries its hyperlink"
    );
    drop(linked);

    grid.resize(2, 20);
    grid.assert_invariants();
    let linked = grid
        .get_history_line(0)
        .expect("history survives the rows-only shrink");
    assert_eq!(linked.to_string().trim_end(), "Hello");
    assert!(
        linked
            .hyperlinks()
            .is_some_and(|spans| { spans[0].url.as_ref() == "https://example.com/rows-only" }),
        "history extras survive in place (no ring rebuild on a rows-only resize)"
    );
}
