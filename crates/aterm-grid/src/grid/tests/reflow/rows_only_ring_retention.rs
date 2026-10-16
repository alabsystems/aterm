// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rows-only resize retention on ring-only grids (the wasm engines' shape): the ring is the
//! retention, so a rows-only resize never sheds ring history. Identity law: history, viewport
//! and absolute-row numbering match what the same writes and resize produce on a tiered grid.

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

/// The identity-law key: (history oldest-first, viewport, `oldest_absolute_row`).
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

    // The shrink anchors at the cursor: the top 5 viewport rows become the newest history, so
    // reading order stays the write order and the cursor's line stays visible.
    let (history, viewport, oldest_abs) = logical_buffer(&ring);
    assert_eq!(history.len(), 26);
    assert_eq!(history[0], "N0");
    assert_eq!(history[20], "N20");
    assert_eq!(&history[21..], ["N21", "N22", "N23", "N24", "N25"]);
    assert_eq!(viewport, ["N26", "N27", "N28", "N29", ""]);
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

    // 5 history lines re-enter the viewport; the rest stay history.
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

/// The cursor follows its content through a rows-grow reveal: every pre-resize viewport row sits
/// `revealed` rows further down, so a TUI's post-SIGWINCH repaint and CPR answers anchor. Both
/// grid shapes.
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
        // The cursor is parked on the row below "N29".
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
        // Grow 5 -> 10 with ample history reveals 5 lines.
        assert_eq!(
            after_row,
            before_row + 5,
            "tiered={tiered}: cursor shifts by the revealed count"
        );
    }
}

/// A shrink whose demoted rows overflow the retention cap evicts only past the cap, oldest first,
/// like `scroll_up`'s at-capacity reuse.
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

/// The alt-screen configuration (ring cap 0) keeps no scrollback through a
/// rows-only shrink: demoted rows are evicted by the zero cap.
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

/// Ring history extras survive a rows-only resize, side table and all.
#[test]
fn rows_only_resize_keeps_ring_history_extras() {
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(3, 20, 10_000);
    // A linked "Hello" on row 0, then enough lines to push it into ring history.
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

/// Every retained line oldest-first, then the viewport (trailing blanks
/// trimmed): the transcript a reader of history plus screen sees.
fn transcript(grid: &Grid) -> Vec<String> {
    let mut all = crate::grid::test_support::history_trimmed(grid);
    all.extend((0..grid.rows()).map(|r| {
        grid.row_text(r)
            .map(|t| t.trim_end().to_string())
            .unwrap_or_default()
    }));
    all
}

/// A rows-only grow that reveals history raises a wrapped ring's cap without adding a row, so the
/// next line feed appends (see `linearize_ring`): every line feed after the grow scrolls every
/// row, on both grid shapes, and the transcript stays the write order.
#[test]
fn line_feeds_after_a_wrapped_ring_grows_in_rows_scroll_every_row() {
    for tiered in [false, true] {
        let mut grid = if tiered {
            let sb = Scrollback::new(1000, 10_000, 100_000_000);
            Grid::with_tiered_scrollback(5, 20, 10, sb)
        } else {
            Grid::with_scrollback(5, 20, 10)
        };
        // 40 lines through a 15-row ring: full and wrapped.
        crate::grid::test_support::feed_numbered_lines(&mut grid, 0, 40);
        grid.resize(8, 20);
        grid.assert_invariants();
        assert_eq!(
            grid.row_text(0).map(|t| t.trim_end().to_string()),
            Some("L33".to_string()),
            "tiered={tiered}: precondition: the grow revealed three history lines"
        );
        for i in 40..46 {
            crate::grid::test_support::feed_numbered_lines(&mut grid, i, 1);
            grid.assert_invariants();
            let viewport: Vec<String> = transcript(&grid).split_off(grid.scrollback_lines());
            let want: Vec<String> = (i - 6..=i)
                .map(|k| format!("L{k}"))
                .chain([String::new()])
                .collect();
            assert_eq!(
                viewport, want,
                "tiered={tiered}: the line feed after L{i} scrolled every row"
            );
        }
        let all = transcript(&grid);
        let want: Vec<String> = (0..46)
            .map(|k| format!("L{k}"))
            .chain([String::new()])
            .collect();
        assert_eq!(
            all,
            want[want.len() - all.len()..],
            "tiered={tiered}: history then screen is the write order"
        );
    }
}

/// The same for a top-anchored partial region with fixed rows below it (Codex's inline history
/// above its input rows), which archives through the same ring append
/// (`scroll_top_anchored_region_up_with_history`): after a rows-only grow of a wrapped ring, a
/// region scroll (a line feed at its bottom margin, or SU 2) moves every region row, blanks the
/// vacated rows, leaves the footer and archives the displaced rows as the newest history.
#[test]
fn top_anchored_region_scrolls_after_a_wrapped_ring_grows_in_rows() {
    let screen = |grid: &Grid| -> Vec<String> {
        (0..grid.rows())
            .map(|r| {
                grid.row_text(r)
                    .map(|t| t.trim_end().to_string())
                    .unwrap_or_default()
            })
            .collect()
    };
    for tiered in [false, true] {
        for count in [1usize, 2] {
            let ctx = format!("tiered={tiered} count={count}");
            let mut grid = if tiered {
                let sb = Scrollback::new(1000, 10_000, 100_000_000);
                Grid::with_tiered_scrollback(5, 20, 10, sb)
            } else {
                Grid::with_scrollback(5, 20, 10)
            };
            // A full, wrapped 15-row ring; the grow reveals three lines (rows 0..=6: L33..L39).
            crate::grid::test_support::feed_numbered_lines(&mut grid, 0, 40);
            grid.resize(8, 20);
            grid.assert_invariants();
            assert_eq!(screen(&grid)[0], "L33", "{ctx}: precondition");
            grid.set_cursor(7, 0);
            for c in "FOOTER".chars() {
                grid.write_char(c);
            }
            grid.set_scroll_region(0, 6);
            let mut next = 40;
            for round in 0..3 {
                let before = screen(&grid);
                if count == 1 {
                    grid.set_cursor(6, 0);
                    grid.line_feed();
                } else {
                    grid.scroll_region_up(count);
                }
                grid.assert_invariants();
                let mut want = before[count..7].to_vec();
                want.extend(vec![String::new(); count]);
                want.push("FOOTER".to_string());
                assert_eq!(
                    screen(&grid),
                    want,
                    "{ctx} round {round}: the region scrolled by {count}, the footer stayed"
                );
                let history = crate::grid::test_support::history_trimmed(&grid);
                assert_eq!(
                    &history[history.len() - count..],
                    &before[..count],
                    "{ctx} round {round}: the displaced rows are the newest history"
                );
                for row in 7 - count..7 {
                    grid.set_cursor(crate::row_u16(row), 0);
                    for c in format!("L{next}").chars() {
                        grid.write_char(c);
                    }
                    next += 1;
                }
            }
            let mut all = crate::grid::test_support::history_trimmed(&grid);
            all.extend(screen(&grid).into_iter().take(7));
            let want: Vec<String> = (0..next).map(|k| format!("L{k}")).collect();
            assert_eq!(
                all,
                want[want.len() - all.len()..],
                "{ctx}: history then the region is the write order"
            );
        }
    }
}
