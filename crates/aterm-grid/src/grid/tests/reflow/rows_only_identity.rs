// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rows-only shrink→grow ROUND-TRIP identity (audit-2 item 1).
//!
//! Before the anchoring rework, shrink demoted the BOTTOM viewport rows as
//! newest history while grow revealed newest history at the TOP — mutually
//! inconsistent, so every height toggle ROTATED the screen (measured on the
//! macOS GUI: LINE0..LINE4 shrink+grow → LINE3,LINE4,LINE0,LINE1,LINE2 with
//! the cursor detached from its line, the prompt walking down two rows per
//! toggle, and scroll-back reading order corrupted), and the grow reveal
//! DISCARDED the transiting rows' `ring_extras` (emoji → U+FFFD, hyperlinks
//! and RGB gone). These pins hold the anchoring to identity: same viewport,
//! same cursor line, same history order, same extras, after any shrink+grow.

use super::super::super::*;
use crate::CellCoord;
use std::sync::Arc;

fn write_lines(grid: &mut Grid, names: &[&str]) {
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            grid.line_feed();
        }
        grid.carriage_return();
        for c in name.chars() {
            grid.write_char(c);
        }
    }
}

fn viewport_texts(grid: &Grid) -> Vec<String> {
    (0..grid.rows())
        .map(|r| {
            grid.row_text(r)
                .map(|t| t.trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

fn history_texts(grid: &Grid) -> Vec<String> {
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// The audit's screen-rotation repro, now pinned to identity: a full screen,
/// cursor at the bottom, shrink then grow restores content, cursor, and order.
#[test]
fn full_screen_shrink_grow_round_trip_is_identity() {
    let mut grid = Grid::with_scrollback(5, 20, 100);
    write_lines(&mut grid, &["LINE0", "LINE1", "LINE2", "LINE3", "LINE4"]);
    let before_view = viewport_texts(&grid);
    let before_cursor = (grid.cursor_row(), grid.cursor_col());
    assert_eq!(before_view, ["LINE0", "LINE1", "LINE2", "LINE3", "LINE4"]);

    grid.resize(3, 20);
    // Shrink anchors at the cursor: its line (LINE4) stays visible, the TOP
    // rows transit to history in reading order.
    assert_eq!(viewport_texts(&grid), ["LINE2", "LINE3", "LINE4"]);
    assert_eq!(history_texts(&grid), ["LINE0", "LINE1"]);

    grid.resize(5, 20);
    // Grow pulls the same rows back to the top — the exact inverse.
    assert_eq!(viewport_texts(&grid), before_view, "round trip is identity");
    assert_eq!(
        (grid.cursor_row(), grid.cursor_col()),
        before_cursor,
        "the cursor returns to its line"
    );
    assert_eq!(history_texts(&grid), Vec::<String>::new());
    grid.assert_invariants();
}

/// The audit's prompt-walk repro: a prompt at the top with blanks below must
/// not move AT ALL across height toggles — trailing blanks trim on shrink
/// (they are not content, so no fake blank history is manufactured) and
/// nothing exists to pull on grow.
#[test]
fn prompt_with_trailing_blanks_does_not_walk_across_toggles() {
    let mut grid = Grid::with_scrollback(5, 20, 100);
    write_lines(&mut grid, &["PROMPT$"]);
    let cursor = (grid.cursor_row(), grid.cursor_col());
    assert_eq!(cursor.0, 0);

    for _ in 0..3 {
        grid.resize(3, 20);
        grid.resize(5, 20);
    }
    assert_eq!(
        viewport_texts(&grid),
        ["PROMPT$", "", "", "", ""],
        "the prompt stays exactly where it was"
    );
    assert_eq!(
        (grid.cursor_row(), grid.cursor_col()),
        cursor,
        "the cursor never detaches from the prompt"
    );
    assert_eq!(
        history_texts(&grid),
        Vec::<String>::new(),
        "blank rows are trimmed, never archived as fake history"
    );
    grid.assert_invariants();
}

/// The audit's history-order repro: with deep history, a shrink+grow cycle
/// must leave scroll-back reading order exactly the write order — the old
/// shapes produced [.., L6, L7, L3, L4, L5].
#[test]
fn history_reading_order_survives_a_round_trip() {
    let mut grid = Grid::with_scrollback(5, 20, 100);
    write_lines(&mut grid, &["L0", "L1", "L2", "L3", "L4", "L5", "L6", "L7"]);
    assert_eq!(viewport_texts(&grid), ["L3", "L4", "L5", "L6", "L7"]);
    assert_eq!(history_texts(&grid), ["L0", "L1", "L2"]);

    grid.resize(3, 20);
    grid.resize(5, 20);
    assert_eq!(viewport_texts(&grid), ["L3", "L4", "L5", "L6", "L7"]);
    assert_eq!(
        history_texts(&grid),
        ["L0", "L1", "L2"],
        "reading order is the write order, everywhere"
    );
    grid.assert_invariants();
}

/// Extras survive the transit (audit-2 item 6): a hyperlink and a non-BMP
/// scalar on rows that transit through ring history across a shrink+grow
/// round trip come back intact — the old grow reveal popped and DISCARDED
/// the `ring_extras` entries (emoji read back as U+FFFD, links vanished).
#[test]
fn extras_survive_the_round_trip_through_ring_history() {
    let mut grid = Grid::with_scrollback(5, 20, 100);
    write_lines(&mut grid, &["f0", "f1", "tail 🚀", "link", "bottom"]);
    let url: Arc<str> = Arc::from("https://example.com/round-trip");
    for col in 0..4u16 {
        grid.extras_mut()
            .get_or_create(CellCoord::new(3, col))
            .set_hyperlink(Some(url.clone()));
    }
    // Cursor sits on the bottom row, so the shrink demotes the TOP rows —
    // f0/f1 — and a deeper shrink would take the emoji and link rows too.
    grid.resize(2, 20);
    assert_eq!(viewport_texts(&grid), ["link", "bottom"]);
    grid.resize(5, 20);
    assert_eq!(
        viewport_texts(&grid),
        ["f0", "f1", "tail 🚀", "link", "bottom"],
        "the emoji survives the transit (no U+FFFD)"
    );
    let link_cell = grid
        .extras()
        .get(CellCoord::new(3, 0))
        .expect("link row extras restored");
    assert_eq!(
        link_cell.hyperlink().map(|u| u.to_string()).as_deref(),
        Some("https://example.com/round-trip"),
        "the hyperlink survives the transit"
    );
    grid.assert_invariants();
}

/// The cursor-near-top corner (a full non-blank screen, cursor on the top
/// row): content is preserved — nothing is lost — and the cursor's line
/// stays visible; this is the one shape that still bottom-pushes.
#[test]
fn cursor_near_top_shrink_preserves_all_content() {
    let mut grid = Grid::with_scrollback(5, 20, 100);
    write_lines(&mut grid, &["top", "m1", "m2", "m3", "m4"]);
    grid.set_cursor(0, 3);

    grid.resize(3, 20);
    let view = viewport_texts(&grid);
    assert_eq!(view[0], "top", "the cursor's line stays visible");
    assert_eq!(grid.cursor_row(), 0);
    let mut all: Vec<String> = history_texts(&grid);
    all.extend(view);
    for name in ["top", "m1", "m2", "m3", "m4"] {
        assert!(
            all.iter().any(|l| l == name),
            "{name} must survive the shrink somewhere (nothing is lost): {all:?}"
        );
    }
    grid.assert_invariants();
}
