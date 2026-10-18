// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reflow regressions: display offset and extras remapping.

use std::sync::Arc;

use super::super::super::reflow::ReflowMode;
use super::super::super::*;

/// Resize while display_offset > 0 reflows the live content, not scrolled-back
/// history.
#[test]
fn resize_reflow_with_nonzero_display_offset_preserves_live_content() {
    let mut grid = Grid::with_scrollback(4, 10, 4);

    for row in 0..4u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Scroll once, advancing ring_head.
    grid.set_cursor(3, 0);
    grid.line_feed();
    grid.set_cursor(3, 0);
    grid.write_char('E');

    let live_before: Vec<char> = (0..4)
        .map(|row| grid.cell(row, 0).unwrap().char())
        .collect();
    assert_eq!(live_before, vec!['B', 'C', 'D', 'E']);

    grid.scroll_display(1);
    assert!(grid.storage.display_offset > 0);

    grid.resize_with_reflow_mode(4, 8, ReflowMode::Enabled);

    // The resize keeps the reading position (clamped to the new history length); the reflow
    // runs at offset 0 and restores it after.
    assert!(
        grid.storage.display_offset > 0,
        "display_offset should be PRESERVED across resize, not reset to 0"
    );
    assert!(
        grid.storage.display_offset <= grid.storage.scrollback_lines(),
        "preserved display_offset must stay clamped to the new history length"
    );

    // Back to the live view (`cell()` applies display_offset): the live content survived.
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

/// Hyperlink that moves to a new row during shrink reflow is preserved.
#[test]
fn reflow_shrink_hyperlink_moves_to_new_row() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    let extra = grid.extras_mut().get_or_create(CellCoord::new(0, 7));
    extra.set_hyperlink(Some(Arc::from("https://h.example")));
    extra.set_hyperlink_id(Some(Arc::from("link-h")));

    // 'H' at offset 7 lands on (1, 2).
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
    assert!(grid.extras().get(CellCoord::new(0, 7)).is_none());
}

/// Underline color survives grow reflow (unwrap).
#[test]
fn reflow_grow_preserves_underline_color() {
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

    grid.extras_mut()
        .get_or_create(CellCoord::new(1, 1))
        .set_underline_color(Some([255, 0, 128]));

    // 'G' (logical offset 6) lands on (0,6).
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

/// RGB foreground color survives shrink + grow round-trip.
#[test]
fn reflow_round_trip_preserves_fg_rgb() {
    let mut grid = Grid::new(5, 10);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }

    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 5))
        .set_fg_rgb(Some([10, 20, 30]));

    grid.resize(5, 5);
    let after_shrink = grid.extras().get(CellCoord::new(1, 0));
    assert!(
        after_shrink.is_some(),
        "fg_rgb on 'F' should remap to (1,0) after shrink"
    );
    assert_eq!(after_shrink.unwrap().fg_rgb(), Some([10, 20, 30]));

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

/// Complex-char overflow survives shrink reflow and still renders via row_text().
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

/// Extras on a wide cell must drop when 1-column reflow replaces the
/// cell with a blank and skips its spacer.
#[test]
fn reflow_shrink_drops_wide_char_extras_when_wide_cell_is_replaced() {
    let mut grid = Grid::new(3, 4);
    grid.write_wide_char_wrap_styled(
        '中',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
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

/// After reflow, HAS_EXTRAS flags match the extras map exactly.
#[test]
fn reflow_extras_flags_consistent_after_shrink() {
    let mut grid = Grid::new(4, 12);
    for c in "ABCDEFGHIJKL".chars() {
        grid.write_char(c);
    }

    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 2))
        .set_hyperlink(Some(Arc::from("https://c.test")));
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 6))
        .set_underline_color(Some([255, 0, 0]));
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 10))
        .set_fg_rgb(Some([0, 255, 0]));

    // At 4 cols 'C', 'G', 'K' land at (0,2), (1,2), (2,2).
    grid.resize(4, 4);

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

    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 1))
        .set_hyperlink(Some(Arc::from("https://b.test")));
    grid.extras_mut()
        .get_or_create(CellCoord::new(1, 1))
        .set_underline_color(Some([0, 0, 255]));

    // 'F' at (1,1) lands on (0,5).
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

/// A height shrink pushes the top rows into ring history; a regrow reveals them again and the
/// bottom row's hyperlink follows its content.
#[test]
fn reflow_row_shrink_retains_rows_and_extras_follow_regrow() {
    let mut grid = Grid::new(5, 10);
    for row in 0..5u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    let extra = grid.extras_mut().get_or_create(CellCoord::new(4, 0));
    extra.set_hyperlink(Some(Arc::from("https://bottom.example")));

    grid.resize(3, 5);
    assert_eq!(grid.row(0).unwrap().to_string(), "C");
    assert_eq!(grid.row(2).unwrap().to_string(), "E");
    assert_eq!(grid.scrollback_lines(), 2, "A and B retained as history");

    grid.resize(5, 5);
    for (row, expect) in ["A", "B", "C", "D", "E"].iter().enumerate() {
        assert_eq!(
            grid.row(row_u16(row)).unwrap().to_string(),
            *expect,
            "row {row} restored after the round-trip"
        );
    }
    // The hyperlink stays with 'E'.
    let remapped = grid.cell_extra(4, 0).expect("E keeps its hyperlink");
    assert_eq!(
        remapped.hyperlink().map(|u| u.as_ref()),
        Some("https://bottom.example")
    );
    for row in 0..4u16 {
        assert!(
            grid.cell_extra(row, 0).is_none(),
            "no stale extras on row {row}"
        );
    }
    grid.assert_invariants();
}

/// A height decrease (no column change) stages the excess front rows in the lazy buffer instead
/// of dropping them.
#[test]
fn adjust_row_count_height_decrease_preserves_scrollback_content() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(5, 80, 10, scrollback);

    for row in 0..5u16 {
        grid.set_cursor(row, 0);
        for c in format!("Row{row}").chars() {
            grid.write_char(c);
        }
    }

    for i in 5..10u16 {
        grid.set_cursor(4, 0);
        grid.line_feed();
        grid.set_cursor(4, 0);
        for c in format!("Row{i}").chars() {
            grid.write_char(c);
        }
    }

    let scrollback_before = grid.scrollback_lines();
    assert!(
        scrollback_before > 0,
        "expected scrollback lines before resize, got 0"
    );

    grid.resize(3, 80);

    let scrollback_after = grid.scrollback_lines();
    assert!(
        scrollback_after >= scrollback_before,
        "scrollback lines should not decrease after height shrink: \
         before={scrollback_before}, after={scrollback_after}"
    );

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

/// A height decrease on a grid with no tiered store does not panic.
#[test]
fn adjust_row_count_height_decrease_no_scrollback_no_panic() {
    let mut grid = Grid::new(5, 80);

    for row in 0..5u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Ring entries for the shrink path to drain.
    grid.set_cursor(4, 0);
    grid.line_feed();
    grid.set_cursor(4, 0);
    grid.write_char('F');

    grid.resize(3, 80);

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

/// A column change rewraps all ring scrollback; the logical lines survive both directions.
#[test]
fn column_reflow_preserves_ring_scrollback_both_directions() {
    let mut grid = Grid::with_scrollback(4, 40, 1000);
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

    // Widen back: lines unwrap; content still complete and the rewrap is
    // reversible (same physical row count).
    grid.resize(4, 40);
    grid.assert_invariants();
    assert_eq!(
        grid.scrollback_lines(),
        count_before,
        "round-trip restores original scrollback row count"
    );
    let wide = logical_scrollback(&grid);
    for line in &before {
        assert!(
            wide.contains(line),
            "logical line dropped on widen: {line:?}"
        );
    }
}

/// With a tiered store, `restore_reflowed_scrollback` pushes rewrapped history into the store,
/// then drains the lazy buffer holding the reflow overflow: [old scrollback | overflow].
#[test]
fn width_reflow_tiered_history_precedes_visible_overflow() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 8, 2, scrollback);

    // H0..H3 scroll into history; H4/H5 are overwritten below.
    for i in 0..6u16 {
        grid.set_cursor(2, 0);
        for c in format!("H{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }

    // Full visible rows wrap into more rows than fit; with the cursor on the bottom row,
    // `finalize_reflow` pushes the excess top rows to the lazy buffer.
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

    let overflow = ["V0aa".to_string(), "aaaa".to_string(), "V1bb".to_string()];
    assert!(
        texts.ends_with(&overflow),
        "visible-reflow overflow must sit at the newest end: {texts:?}"
    );
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

/// Restoring rewrapped history into the tiered store honours its line limit: oldest evicted.
#[test]
fn width_reflow_tiered_restore_honors_line_limit() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 8, 2, scrollback);
    grid.storage.set_store_line_limit(Some(4));

    for i in 0..12u16 {
        grid.set_cursor(2, 0);
        for c in format!("H{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }

    // No visible-grid overflow: history alone round-trips through the restore.
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
