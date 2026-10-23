// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rows-only shrink-then-grow round-trip identity tests.

use super::super::super::*;
use crate::CellCoord;
use crate::grid::test_support::history_trimmed;
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

#[test]
fn full_screen_shrink_grow_round_trip_is_identity() {
    let mut grid = Grid::with_scrollback(5, 20, 100);
    write_lines(&mut grid, &["LINE0", "LINE1", "LINE2", "LINE3", "LINE4"]);
    let before_view = viewport_texts(&grid);
    let before_cursor = (grid.cursor_row(), grid.cursor_col());
    assert_eq!(before_view, ["LINE0", "LINE1", "LINE2", "LINE3", "LINE4"]);

    grid.resize(3, 20);
    assert_eq!(viewport_texts(&grid), ["LINE2", "LINE3", "LINE4"]);
    assert_eq!(history_trimmed(&grid), ["LINE0", "LINE1"]);

    grid.resize(5, 20);
    assert_eq!(viewport_texts(&grid), before_view, "round trip is identity");
    assert_eq!(
        (grid.cursor_row(), grid.cursor_col()),
        before_cursor,
        "the cursor returns to its line"
    );
    assert_eq!(history_trimmed(&grid), Vec::<String>::new());
    grid.assert_invariants();
}

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
        history_trimmed(&grid),
        Vec::<String>::new(),
        "blank rows are trimmed, never archived as fake history"
    );
    grid.assert_invariants();
}

#[test]
fn history_reading_order_survives_a_round_trip() {
    let mut grid = Grid::with_scrollback(5, 20, 100);
    write_lines(&mut grid, &["L0", "L1", "L2", "L3", "L4", "L5", "L6", "L7"]);
    assert_eq!(viewport_texts(&grid), ["L3", "L4", "L5", "L6", "L7"]);
    assert_eq!(history_trimmed(&grid), ["L0", "L1", "L2"]);

    grid.resize(3, 20);
    grid.resize(5, 20);
    assert_eq!(viewport_texts(&grid), ["L3", "L4", "L5", "L6", "L7"]);
    assert_eq!(
        history_trimmed(&grid),
        ["L0", "L1", "L2"],
        "reading order is the write order, everywhere"
    );
    grid.assert_invariants();
}

#[test]
fn extras_survive_the_round_trip_through_ring_history() {
    let mut grid = Grid::with_scrollback(5, 20, 100);
    write_lines(&mut grid, &["f0", "f1", "tail 🚀", "link", "bottom"]);
    let url: Arc<str> = Arc::from("https://example.com/round-trip");
    for col in 0..4u16 {
        grid.cell_extra_mut(3, col).set_hyperlink(Some(url.clone()));
    }
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

#[test]
fn cursor_near_top_shrink_preserves_all_content() {
    let mut grid = Grid::with_scrollback(5, 20, 100);
    write_lines(&mut grid, &["top", "m1", "m2", "m3", "m4"]);
    grid.set_cursor(0, 3);

    grid.resize(3, 20);
    let view = viewport_texts(&grid);
    assert_eq!(view[0], "top", "the cursor's line stays visible");
    assert_eq!(grid.cursor_row(), 0);
    let mut all: Vec<String> = history_trimmed(&grid);
    all.extend(view);
    for name in ["top", "m1", "m2", "m3", "m4"] {
        assert!(
            all.iter().any(|l| l == name),
            "{name} must survive the shrink somewhere (nothing is lost): {all:?}"
        );
    }
    grid.assert_invariants();
}

#[test]
fn a_rows_only_resize_keeps_every_absolute_key() {
    let cleared = |rows: u16| {
        let mut grid = Grid::with_scrollback(rows, 20, 100);
        let names: Vec<String> = (0..20).map(|i| format!("old {i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        write_lines(&mut grid, &refs);
        grid.erase_scrollback();
        grid.erase_screen();
        grid.set_cursor(0, 0);
        assert_eq!(grid.scrollback_lines(), 0, "the clear took the history");
        assert!(
            grid.oldest_absolute_row() > 0,
            "…but not the absolute numbering the rows already earned"
        );
        grid
    };
    let maps_agree = |grid: &Grid| {
        let base = grid.oldest_absolute_row() + grid.scrollback_lines() as u64;
        (0..grid.rows()).all(|r| grid.visible_to_absolute(r) == base + u64::from(r))
    };

    let mut grid = cleared(5);
    write_lines(&mut grid, &["a", "b", "c"]);
    let before = grid.oldest_absolute_row();
    let keys: Vec<u64> = (0..3).map(|r| grid.visible_to_absolute(r)).collect();
    let epoch = grid.history_renumber_epoch();
    grid.resize(9, 20);
    assert_eq!(grid.oldest_absolute_row(), before, "no key slid");
    assert_eq!(
        (0..3)
            .map(|r| grid.visible_to_absolute(r))
            .collect::<Vec<_>>(),
        keys,
        "a, b and c keep their keys; the appended rows take fresh ones"
    );
    assert_eq!(
        grid.history_renumber_epoch(),
        epoch,
        "nothing was renumbered, so no rebuild is asked for"
    );
    assert!(maps_agree(&grid));
    grid.assert_invariants();

    let mut grid = Grid::with_scrollback(3, 20, 4);
    let names: Vec<String> = (0..20).map(|i| format!("h{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    write_lines(&mut grid, &refs);
    assert!(grid.scrollback_lines() >= 2, "history to reveal");
    let before = grid.oldest_absolute_row();
    assert!(before > 0, "the fixture has really scrolled");
    let epoch = grid.history_renumber_epoch();
    grid.resize(5, 20);
    assert_eq!(
        grid.oldest_absolute_row(),
        before,
        "the reveal is a relabel: every absolute key survives it"
    );
    assert_eq!(
        grid.history_renumber_epoch(),
        epoch,
        "so there is nothing to report and no spurious rebuild is asked for"
    );
    assert!(maps_agree(&grid));
    grid.assert_invariants();

    let mut grid = cleared(8);
    write_lines(&mut grid, &["a", "b"]);
    let before = grid.oldest_absolute_row();
    let keys: Vec<u64> = (0..2).map(|r| grid.visible_to_absolute(r)).collect();
    let epoch = grid.history_renumber_epoch();
    grid.resize(5, 20);
    assert_eq!(grid.oldest_absolute_row(), before, "no key slid");
    assert_eq!(
        (0..2)
            .map(|r| grid.visible_to_absolute(r))
            .collect::<Vec<_>>(),
        keys
    );
    assert_eq!(grid.history_renumber_epoch(), epoch);
    assert!(maps_agree(&grid));
    grid.assert_invariants();

    let mut grid = Grid::with_scrollback(5, 20, 100);
    assert_eq!(grid.oldest_absolute_row(), 0);
    grid.resize(6, 20);
    assert!(maps_agree(&grid), "agree right after the grow");
    let names: Vec<String> = (0..9).map(|i| format!("out {i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    write_lines(&mut grid, &refs);
    assert!(grid.scrollback_lines() > 0, "rows scrolled into history");
    assert!(
        grid.absolute_row_counter() >= u64::from(grid.rows()) + grid.scrollback_lines() as u64,
        "every retained row has a key at or above 0"
    );
    assert!(maps_agree(&grid), "and still agree once history exists");
    grid.assert_invariants();
}
