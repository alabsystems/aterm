// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Regression tests — display-offset (#2184), extras remapping (#2414, #3977).

use std::sync::Arc;

use super::super::super::reflow::ReflowMode;
use super::super::super::*;

/// Regression: resize while display_offset > 0 must reflow live content, not
/// scrolled-back history. Before the fix (#2184), collect_visible_rows used
/// row_index which applied display_offset, causing reflow to process the wrong
/// rows and silently discard live cursor content.
#[test]
fn resize_reflow_with_nonzero_display_offset_preserves_live_content() {
    // Create a grid with room for ring-buffer scrollback.
    let mut grid = Grid::with_scrollback(4, 10, 4);

    // Write distinct content per row.
    for row in 0..4u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Scroll to create ring-buffer scrollback and advance ring_head.
    grid.set_cursor(3, 0);
    grid.line_feed();
    grid.set_cursor(3, 0);
    grid.write_char('E');

    // Verify live content: rows should show B, C, D, E.
    let live_before: Vec<char> = (0..4)
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect();
    assert_eq!(live_before, vec!['B', 'C', 'D', 'E']);

    // Simulate user scrolling back into history.
    grid.scroll_display(1);
    assert!(grid.storage.display_offset > 0);

    // Resize with reflow (column change triggers collect_visible_rows).
    grid.resize_with_reflow_mode(4, 8, ReflowMode::Enabled);

    // A resize/zoom must PRESERVE the user's scrollback reading position (clamped to
    // the new, possibly-rewrapped history length), not yank them to the live bottom.
    // The reflow computation still runs at offset 0 internally — so live content is
    // processed correctly (#2184) — then the saved offset is restored afterward.
    assert!(
        grid.storage.display_offset > 0,
        "display_offset should be PRESERVED across resize, not reset to 0"
    );
    assert!(
        grid.storage.display_offset <= grid.storage.scrollback_lines(),
        "preserved display_offset must stay clamped to the new history length"
    );

    // The #2184 guarantee — reflow processed the LIVE rows, not the scrolled-back
    // history — is verified by snapping to the live view and confirming the live
    // content survived. (`cell()` applies display_offset, which is now non-zero, so we
    // must drop to the live view before reading the live rows.)
    grid.storage.display_offset = 0;
    let live_after: Vec<char> = (0..4)
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect();
    assert_eq!(
        live_after, live_before,
        "live row content must survive resize-with-reflow when display_offset was non-zero"
    );
    grid.assert_invariants();
}

/// #2414 / #3977: Reflow remaps extras coordinates instead of clearing them.
/// A hyperlink within the first chunk stays at the same coordinate.
#[test]
fn reflow_shrink_remaps_extras_within_first_chunk() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    // Set a hyperlink on cell (0, 3) — the 'D' cell.
    let coord = CellCoord::new(0, 3);
    grid.extras_mut()
        .get_or_create(coord)
        .set_hyperlink(Some(Arc::from("https://example.com")));
    assert!(grid.extras().get(coord).is_some());

    // Shrink from 10 to 5 columns. "ABCDE" on row 0, "FGHIJ" on row 1.
    // 'D' at old (0,3) stays in the first chunk → new (0,3).
    grid.resize(5, 5);

    assert!(
        grid.extras().get(CellCoord::new(0, 3)).is_some(),
        "hyperlink on 'D' should survive reflow at (0,3)"
    );
    assert!(
        grid.extras()
            .get(CellCoord::new(0, 3))
            .unwrap()
            .hyperlink()
            .is_some(),
        "hyperlink URL must be preserved"
    );
    // No stale extras at the old position on the second chunk row.
    assert!(
        grid.extras().get(CellCoord::new(1, 3)).is_none(),
        "no extras should appear on row 1 col 3"
    );
}

/// #3977: Hyperlink that moves to a new row during shrink reflow is preserved.
#[test]
fn reflow_shrink_hyperlink_moves_to_new_row() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    // Set hyperlink on 'H' at (0, 7).
    let extra = grid.extras_mut().get_or_create(CellCoord::new(0, 7));
    extra.set_hyperlink(Some(Arc::from("https://h.example")));
    extra.set_hyperlink_id(Some(Arc::from("link-h")));

    // Shrink to 5 cols: "ABCDE" row 0, "FGHIJ" row 1.
    // 'H' was at offset 7 → chunk 1 (5..10), position 7-5=2 → (1, 2).
    grid.resize(5, 5);

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");

    let remapped = grid.extras().get(CellCoord::new(1, 2));
    assert!(
        remapped.is_some(),
        "hyperlink on 'H' should remap from (0,7) to (1,2)"
    );
    assert_eq!(
        remapped.unwrap().hyperlink().map(|u| u.as_ref()),
        Some("https://h.example")
    );
    assert_eq!(
        remapped.unwrap().hyperlink_id().map(|id| id.as_ref()),
        Some("link-h")
    );
    assert!(
        grid.cell(1, 2)
            .expect("remapped cell should exist")
            .has_extras(),
        "remapped destination cell should keep the HAS_EXTRAS flag"
    );
    // Old position should be gone.
    assert!(grid.extras().get(CellCoord::new(0, 7)).is_none());
}

/// #3977: Underline color survives grow reflow (unwrap).
#[test]
fn reflow_grow_preserves_underline_color() {
    // Start with 5-col grid: "ABCDE" + wrapped "FGHIJ".
    let mut grid = Grid::new(5, 5);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    grid.carriage_return();
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        for (i, c) in "FGHIJ".chars().enumerate() {
            row.write_char(i as u16, c);
        }
    }

    // Set underline color on 'G' at (1, 1). Logical offset = 5 + 1 = 6.
    grid.extras_mut()
        .get_or_create(CellCoord::new(1, 1))
        .set_underline_color(Some([255, 0, 128]));

    // Grow to 12 cols → "ABCDEFGHIJ" on row 0.
    // 'G' logical offset 6 → (0, 6).
    grid.resize(5, 12);
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");

    let remapped = grid.extras().get(CellCoord::new(0, 6));
    assert!(
        remapped.is_some(),
        "underline color on 'G' should remap from (1,1) to (0,6)"
    );
    assert_eq!(remapped.unwrap().underline_color(), Some([255, 0, 128]));
    assert!(grid.extras().get(CellCoord::new(1, 1)).is_none());
    assert!(
        grid.cell(0, 6)
            .expect("merged destination cell should exist")
            .has_extras(),
        "merged destination cell should keep the HAS_EXTRAS flag"
    );
}

/// #3977: RGB foreground color survives shrink + grow round-trip.
#[test]
fn reflow_round_trip_preserves_fg_rgb() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    // Set fg RGB on 'F' at (0, 5).
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 5))
        .set_fg_rgb(Some([10, 20, 30]));

    // Shrink to 5 → 'F' moves to (1, 0).
    grid.resize(5, 5);
    let after_shrink = grid.extras().get(CellCoord::new(1, 0));
    assert!(
        after_shrink.is_some(),
        "fg_rgb on 'F' should remap to (1,0) after shrink"
    );
    assert_eq!(after_shrink.unwrap().fg_rgb(), Some([10, 20, 30]));

    // Grow back to 10 → 'F' returns to (0, 5).
    grid.resize(5, 10);
    let after_grow = grid.extras().get(CellCoord::new(0, 5));
    assert!(
        after_grow.is_some(),
        "fg_rgb on 'F' should return to (0,5) after grow"
    );
    assert_eq!(after_grow.unwrap().fg_rgb(), Some([10, 20, 30]));
}

/// Background RGB must survive shrink reflow, not just foreground RGB.
#[test]
fn reflow_shrink_preserves_bg_rgb() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 5))
        .set_bg_rgb(Some([40, 50, 60]));

    grid.resize(5, 5);

    assert_eq!(grid.row(1).unwrap().get(0).unwrap().char(), 'F');
    assert_eq!(
        grid.bg_rgb_at(1, 0),
        Some([40, 50, 60]),
        "bg_rgb on 'F' should remap from (0,5) to (1,0) after shrink"
    );
    assert!(
        grid.cell(1, 0)
            .expect("remapped cell should exist")
            .has_extras(),
        "remapped destination cell should keep the HAS_EXTRAS flag"
    );
}

/// #3977: Hyperlinks on multiple cells all survive reflow.
#[test]
fn reflow_shrink_multiple_extras_remapped() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    // Hyperlinks on 'C' (0,2) and 'H' (0,7).
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 2))
        .set_hyperlink(Some(Arc::from("https://c.example")));
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 7))
        .set_hyperlink(Some(Arc::from("https://h.example")));

    // Shrink to 5: 'C' stays (0,2), 'H' moves to (1,2).
    grid.resize(5, 5);

    let c_extra = grid.extras().get(CellCoord::new(0, 2));
    assert!(c_extra.is_some(), "'C' hyperlink at (0,2) preserved");
    assert_eq!(
        c_extra.unwrap().hyperlink().map(|u| u.as_ref()),
        Some("https://c.example")
    );

    let h_extra = grid.extras().get(CellCoord::new(1, 2));
    assert!(h_extra.is_some(), "'H' hyperlink remapped to (1,2)");
    assert_eq!(
        h_extra.unwrap().hyperlink().map(|u| u.as_ref()),
        Some("https://h.example")
    );
}

/// #3977: Complex-char overflow survives shrink reflow and still renders via row_text().
#[test]
fn reflow_shrink_preserves_complex_char_and_row_text() {
    let mut grid = Grid::new(4, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.set_cell(0, 7, Cell::new('X'));
    grid.set_cell_complex_char(0, 7, "👩‍🚀");

    grid.resize(4, 5);

    let extra = grid
        .cell_extra(1, 2)
        .expect("complex-char extra should move to the split row");
    assert_eq!(extra.complex_char().map(AsRef::as_ref), Some("👩‍🚀"));
    assert!(grid.cell_extra(0, 7).is_none());
    assert_eq!(
        grid.row_text(1).expect("row should exist"),
        "FG👩‍🚀IJ",
        "row_text should still resolve the overflow string after reflow"
    );
}

/// #3977: Extras on a wide cell must drop when 1-column reflow replaces the
/// cell with a blank and skips its spacer.
#[test]
fn reflow_shrink_drops_wide_char_extras_when_wide_cell_is_replaced() {
    let mut grid = Grid::new(3, 4);
    grid.write_wide_char_wrap_with_style_id('中', StyleId::default(), CellFlags::empty());
    grid.write_char('A');

    let extra = grid.extras_mut().get_or_create(CellCoord::new(0, 0));
    extra.set_hyperlink(Some(Arc::from("https://wide.example")));
    extra.set_underline_color(Some([0x11, 0x22, 0x33]));

    grid.resize(3, 1);

    assert!(grid.cell_extra(0, 0).is_none());
    assert!(
        !grid
            .cell(0, 0)
            .expect("replacement cell should exist")
            .has_extras(),
        "blank replacement cell must not retain a stale HAS_EXTRAS flag"
    );
    assert_eq!(
        grid.row_text(0).expect("row should exist"),
        " ",
        "the replaced wide char should render as a blank"
    );
}

/// #5859: After reflow, HAS_EXTRAS flags must be consistent with the extras
/// map. Cells with extras entries have the flag; cells without do not.
/// Validates the O(E) `sync_all_extras_flags` replacement for the previous
/// O(rows*cols) hash-probe loop.
#[test]
fn reflow_extras_flags_consistent_after_shrink() {
    let mut grid = Grid::new(4, 12);
    for c in "ABCDEFGHIJKL".chars() {
        grid.write_char(c);
    }

    // Place extras on 'C' (0,2), 'G' (0,6), 'K' (0,10).
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 2))
        .set_hyperlink(Some(Arc::from("https://c.test")));
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 6))
        .set_underline_color(Some([255, 0, 0]));
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 10))
        .set_fg_rgb(Some([0, 255, 0]));

    // Shrink to 4 cols:
    // Row 0: "ABCD", Row 1: "EFGH", Row 2: "IJKL"
    // 'C' (0,2) → (0,2), 'G' (0,6) → (1,2), 'K' (0,10) → (2,2)
    grid.resize(4, 4);

    // Positive: cells with extras must have HAS_EXTRAS.
    assert!(
        grid.cell(0, 2).unwrap().has_extras(),
        "C at (0,2) must have HAS_EXTRAS"
    );
    assert!(
        grid.cell(1, 2).unwrap().has_extras(),
        "G remapped to (1,2) must have HAS_EXTRAS"
    );
    assert!(
        grid.cell(2, 2).unwrap().has_extras(),
        "K remapped to (2,2) must have HAS_EXTRAS"
    );

    // Negative: cells without extras must NOT have HAS_EXTRAS.
    for row in 0..3u16 {
        for col in 0..4u16 {
            if col == 2 {
                continue; // skip the extras cells
            }
            assert!(
                !grid.cell(row, col).unwrap().has_extras(),
                "cell ({row},{col}) has no extras but HAS_EXTRAS is set"
            );
        }
    }
    grid.assert_invariants();
}

/// #5859: HAS_EXTRAS flags are consistent after grow reflow too.
#[test]
fn reflow_extras_flags_consistent_after_grow() {
    let mut grid = Grid::new(4, 4);
    for c in "ABCD".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    grid.carriage_return();
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        for (i, c) in "EFGH".chars().enumerate() {
            row.write_char(i as u16, c);
        }
    }

    // Place extras on 'B' (0,1) and 'F' (1,1).
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 1))
        .set_hyperlink(Some(Arc::from("https://b.test")));
    grid.extras_mut()
        .get_or_create(CellCoord::new(1, 1))
        .set_underline_color(Some([0, 0, 255]));

    // Grow to 10 cols: "ABCDEFGH" on row 0.
    // 'B' at (0,1) → (0,1), 'F' at (1,1) → (0,5).
    grid.resize(4, 10);

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGH");
    assert!(
        grid.cell(0, 1).unwrap().has_extras(),
        "B at (0,1) must have HAS_EXTRAS after grow"
    );
    assert!(
        grid.cell(0, 5).unwrap().has_extras(),
        "F remapped to (0,5) must have HAS_EXTRAS after grow"
    );

    // Verify no stale flags on other cells in row 0.
    for col in 0..8u16 {
        if col == 1 || col == 5 {
            continue;
        }
        assert!(
            !grid.cell(0, col).unwrap().has_extras(),
            "cell (0,{col}) should not have HAS_EXTRAS"
        );
    }
    grid.assert_invariants();
}

/// #3977: Reflow must size the rebuilt row buffer to the requested height so
/// truncated extras do not reappear when rows are grown later.
#[test]
fn reflow_row_shrink_drops_truncated_extras_before_regrow() {
    let mut grid = Grid::new(5, 10);
    for row in 0..5u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    let extra = grid.extras_mut().get_or_create(CellCoord::new(4, 0));
    extra.set_hyperlink(Some(Arc::from("https://bottom.example")));

    grid.resize(3, 5);
    grid.resize(5, 5);

    assert!(grid.cell_extra(4, 0).is_none());
    assert!(
        !grid
            .cell(4, 0)
            .expect("regrown blank row should exist")
            .has_extras(),
        "regrown blank rows must not inherit truncated extras"
    );
    assert!(
        grid.row(4).expect("row should exist").is_empty(),
        "regrown rows should stay blank"
    );
}

/// #7473: Height decrease (no column change) must push excess front rows to
/// the lazy scrollback buffer instead of dropping them. This ensures scrollback
/// content survives a terminal height decrease.
#[test]
fn adjust_row_count_height_decrease_preserves_scrollback_content() {
    // Grid with tiered scrollback: 5 visible rows, 80 cols, 10 ring buffer scrollback.
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(5, 80, 10, scrollback);

    // Write distinct content on each of the 5 visible rows.
    for row in 0..5u16 {
        grid.set_cursor(row, 0);
        for c in format!("Row{row}").chars() {
            grid.write_char(c);
        }
    }

    // Scroll up to push rows into ring buffer scrollback, then write more.
    // This creates scrollback content that adjust_row_count should preserve.
    for i in 5..10u16 {
        grid.set_cursor(4, 0);
        grid.line_feed();
        grid.set_cursor(4, 0);
        for c in format!("Row{i}").chars() {
            grid.write_char(c);
        }
    }

    // Verify we have some scrollback lines before resize.
    let scrollback_before = grid.scrollback_lines();
    assert!(
        scrollback_before > 0,
        "expected scrollback lines before resize, got 0"
    );

    // Shrink height from 5 to 3 rows (no column change).
    // This triggers adjust_row_count which should push excess front rows
    // to the lazy buffer instead of dropping them.
    grid.resize(3, 80);

    // Total scrollback should be >= what we had before, plus the rows
    // that were pushed from the front during the height decrease.
    let scrollback_after = grid.scrollback_lines();
    assert!(
        scrollback_after >= scrollback_before,
        "scrollback lines should not decrease after height shrink: \
         before={scrollback_before}, after={scrollback_after}"
    );

    // Verify that the oldest scrollback line has our expected content.
    let line = grid
        .try_get_history_line(0)
        .expect("no I/O error")
        .expect("oldest scrollback line must exist");
    let text = line.to_string();
    assert!(
        text.starts_with("Row"),
        "oldest scrollback line should contain our written content, got: '{text}'"
    );

    grid.assert_invariants();
}

/// #7473: Height decrease with plain grid (no column change, no scrollback
/// attached) must not panic. The excess rows are simply dropped.
#[test]
fn adjust_row_count_height_decrease_no_scrollback_no_panic() {
    let mut grid = Grid::new(5, 80);

    for row in 0..5u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Scroll to create ring buffer entries that the shrink path will drain.
    grid.set_cursor(4, 0);
    grid.line_feed();
    grid.set_cursor(4, 0);
    grid.write_char('F');

    // Shrink height: should not panic even without scrollback.
    grid.resize(3, 80);

    // Grid is functional and invariants hold.
    assert_eq!(grid.rows(), 3);
    grid.assert_invariants();
}

/// Join soft-wrapped continuation history rows back into logical lines.
#[cfg(test)]
fn logical_scrollback(grid: &Grid) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in 0..grid.scrollback_lines() {
        let line = grid.get_history_line(i).expect("history line present");
        let text = line.to_string().trim_end_matches(' ').to_string();
        if line.is_wrapped() && !out.is_empty() {
            out.last_mut().unwrap().push_str(&text);
        } else {
            out.push(text);
        }
    }
    out
}

/// #7906: a COLUMN change must rewrap and preserve ALL ring-buffer scrollback,
/// not drop it. Shrinking grows the physical scrollback row count; the logical
/// lines survive in both directions.
#[test]
fn column_reflow_preserves_ring_scrollback_both_directions() {
    let mut grid = Grid::with_scrollback(4, 40, 1000);
    // 30 distinct near-full-width lines; most scroll into ring scrollback.
    for i in 0..30u16 {
        grid.set_cursor(3, 0);
        let mut text = format!("L{i}-");
        while text.len() < 38 {
            text.push('x');
        }
        for c in text.chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }

    let before = logical_scrollback(&grid);
    let count_before = grid.scrollback_lines();
    assert!(
        before.iter().any(|l| l.starts_with("L3-")),
        "L3 in scrollback"
    );

    // Shrink width: wide lines rewrap into more rows -> scrollback grows.
    grid.resize(4, 20);
    grid.assert_invariants();
    assert!(
        grid.scrollback_lines() > count_before,
        "shrinking width must grow physical scrollback rows"
    );
    let narrow = logical_scrollback(&grid);
    for line in &before {
        assert!(
            narrow.contains(line),
            "logical line dropped on shrink: {line:?}"
        );
    }

    // Widen back: lines unwrap; content still complete.
    grid.resize(4, 40);
    grid.assert_invariants();
    let wide = logical_scrollback(&grid);
    for line in &before {
        assert!(
            wide.contains(line),
            "logical line dropped on widen: {line:?}"
        );
    }
}

/// #7906: a known early line survives a width round-trip and the scrollback row
/// count returns to its original value (rewrap is reversible).
#[test]
fn column_reflow_round_trip_restores_scrollback_count() {
    let mut grid = Grid::with_scrollback(3, 30, 1000);
    for i in 0..25u16 {
        grid.set_cursor(2, 0);
        let mut text = format!("ROW{i}-");
        while text.len() < 28 {
            text.push('z');
        }
        for c in text.chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }

    let sb0 = grid.scrollback_lines();
    let present = |g: &Grid| logical_scrollback(g).iter().any(|l| l.starts_with("ROW5-"));
    assert!(present(&grid), "ROW5 present initially");

    grid.resize(3, 18);
    assert!(present(&grid), "ROW5 survives shrink");
    grid.resize(3, 30);
    assert!(present(&grid), "ROW5 survives round-trip");
    assert_eq!(
        grid.scrollback_lines(),
        sb0,
        "round-trip restores original scrollback row count"
    );
    grid.assert_invariants();
}

/// #7906 ordering with TIERED scrollback: restore_reflowed_scrollback pushes the
/// rewrapped history directly into tiered storage (no per-line DeferredLine
/// staging), then drains the lazy buffer holding the visible-grid reflow
/// overflow — the [old scrollback | reflow overflow] contract must hold.
#[test]
fn width_reflow_tiered_history_precedes_visible_overflow() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 8, 2, scrollback);

    // Scroll H0..H3 into history (H4/H5 stay visible and are overwritten below).
    for i in 0..6u16 {
        grid.set_cursor(2, 0);
        for c in format!("H{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }

    // Fill every visible row completely so the width shrink wraps them into
    // more rows than fit; with the cursor on the bottom row, finalize_reflow
    // pushes the excess top rows to the lazy buffer as overflow (#7410).
    let fills = ["V0aaaaaa", "V1bbbbbb", "V2cccccc"];
    for (row, text) in fills.iter().enumerate() {
        grid.set_cursor(row_u16(row), 0);
        for c in text.chars() {
            grid.write_char(c);
        }
    }

    grid.resize(3, 4);
    grid.assert_invariants();

    let texts: Vec<String> = (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .expect("history line present")
                .to_string()
                .trim_end_matches(' ')
                .to_string()
        })
        .collect();

    // Overflow from the visible grid is the newest scrollback content.
    let overflow = ["V0aa".to_string(), "aaaa".to_string(), "V1bb".to_string()];
    assert!(
        texts.ends_with(&overflow),
        "visible-reflow overflow must sit at the newest end: {texts:?}"
    );
    // Every history line precedes the overflow, still in H0..H3 order.
    let h_positions: Vec<usize> = (0..4)
        .map(|i| {
            texts
                .iter()
                .position(|t| t == &format!("H{i}"))
                .unwrap_or_else(|| panic!("H{i} missing from {texts:?}"))
        })
        .collect();
    assert!(
        h_positions.windows(2).all(|w| w[0] < w[1]),
        "history must stay in write order: {texts:?}"
    );
    let first_overflow = texts.len() - overflow.len();
    assert!(
        h_positions.iter().all(|&p| p < first_overflow),
        "all history must precede the overflow: {texts:?}"
    );
}

/// Line-limit eviction while restoring rewrapped history into tiered storage:
/// the direct push_line path must honor the configured limit exactly like the
/// old lazy-buffer round-trip did — oldest lines evicted, newest kept.
#[test]
fn width_reflow_tiered_restore_honors_line_limit() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 8, 2, scrollback);
    grid.storage.set_store_line_limit(Some(4));

    // Scroll H0..H9 into history (H10/H11 stay visible).
    for i in 0..12u16 {
        grid.set_cursor(2, 0);
        for c in format!("H{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }

    // Width change with no visible-grid overflow: history alone round-trips
    // through take_scrollback_lines / restore_reflowed_scrollback.
    grid.resize(3, 6);
    grid.assert_invariants();

    assert_eq!(
        grid.scrollback_lines(),
        4,
        "restore must enforce the line limit"
    );
    let texts: Vec<String> = (0..4)
        .map(|i| {
            grid.get_history_line(i)
                .expect("history line present")
                .to_string()
                .trim_end_matches(' ')
                .to_string()
        })
        .collect();
    assert_eq!(
        texts,
        vec!["H6", "H7", "H8", "H9"],
        "oldest history evicted, newest kept"
    );
}
