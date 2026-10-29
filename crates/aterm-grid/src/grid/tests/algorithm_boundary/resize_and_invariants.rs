// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::super::reflow::ReflowMode;
use super::*;

#[test]
fn reset_scroll_region_restores_full_screen_region() {
    let mut grid = Grid::new(6, 8);
    grid.set_scroll_region(1, 4);
    assert_eq!(
        grid.scroll_region(),
        ScrollRegion { top: 1, bottom: 4 },
        "precondition: custom scroll region should be active",
    );

    grid.reset_scroll_region();

    let region = grid.scroll_region();
    assert!(region.is_full(grid.rows()));
    assert_eq!(region.top, 0);
    assert_eq!(region.bottom, grid.rows() - 1);
}

#[test]
fn resize_resets_cursor_state_after_geometry_change() {
    let mut grid = Grid::new(5, 8);
    grid.set_scroll_region(1, 3);
    grid.set_horizontal_margins(2, 5);
    grid.set_cursor(4, 7);
    grid.save_cursor();
    grid.set_cursor(0, 0);
    for col in 0..grid.cols() {
        grid.write_char_wrap((b'A' + (col % 26) as u8) as char);
    }

    assert!(
        grid.pending_wrap(),
        "precondition: the H in the last column leaves a wrap pending"
    );
    assert_eq!(
        grid.scroll_region(),
        ScrollRegion { top: 1, bottom: 3 },
        "precondition: custom scroll region should be active",
    );
    assert!(
        !grid.horizontal_margins().is_full(grid.cols()),
        "precondition: custom horizontal margins should be active",
    );

    grid.resize_with_reflow_mode(3, 4, ReflowMode::Enabled);

    assert_eq!(
        (grid.cursor_row(), grid.cursor_col(), grid.pending_wrap()),
        (1, 3, true),
        "the rewrap ends the line on the H in the new last column, and its wrap stays pending"
    );
    assert!(
        grid.scroll_region().is_full(grid.rows()),
        "resize should reset the scroll region to the full viewport",
    );
    assert!(
        grid.horizontal_margins().is_full(grid.cols()),
        "resize should reset horizontal margins to the full width",
    );

    grid.restore_cursor();
    assert_eq!(
        grid.cursor_row(),
        2,
        "saved cursor row should clamp to the new height"
    );
    assert_eq!(
        grid.cursor_col(),
        3,
        "saved cursor col should clamp to the new width"
    );
    grid.assert_invariants();
}

#[test]
fn needs_full_redraw_distinguishes_partial_and_full_damage() {
    let mut grid = Grid::new(3, 3);
    assert!(grid.needs_full_redraw());

    grid.clear_damage();
    assert!(!grid.needs_full_redraw());

    grid.mark_cursor_damage();
    assert!(!grid.needs_full_redraw());
    assert!(grid.damage().has_damage());

    grid.scroll_up(1);
    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "scroll_up(1) on 3-row grid should mark targeted damage, not full"
    );

    grid.clear_damage();
    grid.scroll_up(3);
    assert!(grid.needs_full_redraw());
}

#[test]
fn clamp_display_offset_limits_and_marks_damage_on_clamp_only() {
    let mut grid = Grid::with_scrollback(3, 3, 10);
    for marker in ['A', 'B', 'C', 'D', 'E', 'F'] {
        write_marker_line(&mut grid, marker);
    }

    let max_offset = grid.scrollback_lines();
    assert!(max_offset > 0);

    grid.clear_damage();
    grid.storage.display_offset = max_offset + 5;
    assert!(!grid.needs_full_redraw());

    grid.clamp_display_offset();
    assert_eq!(grid.display_offset(), max_offset);
    assert!(grid.needs_full_redraw());

    grid.clear_damage();
    grid.clamp_display_offset();
    assert_eq!(grid.display_offset(), max_offset);
    assert!(!grid.needs_full_redraw());
}

#[test]
fn row_shrink_after_scrolling_preserves_newest_content() {
    let mut grid = Grid::with_scrollback(5, 10, 0);
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.set_cursor(4, 0);
    grid.line_feed();
    grid.line_feed();
    grid.set_cursor(3, 0);
    grid.write_char('X');
    grid.set_cursor(4, 0);
    grid.write_char('Y');
    grid.resize_with_reflow_mode(3, 10, ReflowMode::Enabled);
    assert_eq!(grid.rows(), 3);
    for row in 0..3 {
        grid.row(row)
            .expect("row should be accessible after shrink");
    }
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'E');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'X');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'Y');
    assert_eq!(grid.cursor_row(), 2);
    grid.assert_invariants();
}

#[test]
fn row_shrink_with_scrollback_retains_history_within_the_ring_cap() {
    let mut grid = Grid::with_scrollback(4, 10, 4);
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.set_cursor(3, 0);
    grid.line_feed();
    grid.line_feed();
    grid.line_feed();
    grid.set_cursor(1, 0);
    grid.write_char('X');
    grid.set_cursor(2, 0);
    grid.write_char('Y');
    grid.set_cursor(3, 0);
    grid.write_char('Z');
    grid.resize_with_reflow_mode(3, 10, ReflowMode::Enabled);
    assert_eq!(grid.rows(), 3);
    for row in 0..3 {
        grid.row(row).expect("row should be accessible");
    }
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'X');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'Y');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'Z');
    assert_eq!(grid.scrollback_lines(), 4);
    let history: Vec<String> = (0..4)
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(history, ["A", "B", "C", "D"]);
    grid.assert_invariants();
}

#[test]
fn row_shrink_grow_cycle_nonzero_ring_head() {
    let mut grid = Grid::with_scrollback(4, 8, 0);
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'1' + row as u8) as char);
    }
    grid.set_cursor(3, 0);
    for _ in 0..3 {
        grid.line_feed();
    }
    grid.resize_with_reflow_mode(2, 8, ReflowMode::Enabled);
    assert_eq!(grid.rows(), 2);
    grid.assert_invariants();
    grid.resize_with_reflow_mode(6, 8, ReflowMode::Enabled);
    assert_eq!(grid.rows(), 6);
    for row in 0..6 {
        grid.row(row).expect("row should be accessible after grow");
        if row >= 2 {
            assert_eq!(
                grid.cell(row, 0).unwrap().char(),
                ' ',
                "newly grown row {row} col 0 should be blank"
            );
        }
    }
    grid.assert_invariants();
}

#[test]
fn multipage_grid_resize_zeroes_cells() {
    let mut grid = Grid::with_scrollback(3, 4096, 0);
    for row in 0..3 {
        grid.set_cursor(row, 0);
        let m = (b'A' + row as u8) as char;
        for _ in 0..100 {
            grid.write_char(m);
        }
    }
    grid.resize_with_reflow_mode(3, 2048, ReflowMode::Disabled);
    for row in 0..3 {
        let expected = (b'A' + row as u8) as char;
        assert_eq!(grid.cell(row, 0).unwrap().char(), expected);
    }
    grid.resize_with_reflow_mode(3, 4096, ReflowMode::Disabled);
    for row in 0..3 {
        assert!(
            grid.cell(row, 3000).unwrap().is_empty(),
            "row {row} col 3000 should be empty after grow"
        );
    }
}

/// Reflow grow keeps a cursor in the trailing blank area of a continuation row on the merged row.
#[test]
fn reflow_grow_cursor_in_trailing_blank_of_continuation_row() {
    let mut grid = Grid::new(4, 5);

    grid.set_cursor(0, 0);
    for c in "ABCDE".chars() {
        grid.write_char_wrap(c);
    }
    for c in "FG".chars() {
        grid.write_char_wrap(c);
    }
    grid.set_cursor(2, 0);
    for c in "XY".chars() {
        grid.write_char(c);
    }

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert!(grid.row(1).unwrap().is_wrapped(), "row 1 should be wrapped");

    grid.set_cursor(1, 4);
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 4);

    grid.resize(4, 10);

    assert_eq!(
        grid.cursor_row(),
        0,
        "cursor should be on merged row 0, not row {} (was on continuation of 'ABCDE'→'FG')",
        grid.cursor_row()
    );
    assert_eq!(
        grid.cursor_col(),
        9,
        "cursor at col 4 after 5-col row 0 ('ABCDE') maps to col 5 + 4 == 9 on merged 10-col row 0"
    );
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 6).unwrap().char(), 'G');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'X');
    grid.assert_invariants();
}

/// Resize shrink while scrolled back preserves `TotalLinesMinimum` and `TotalLinesValid`.
#[test]
fn invariants_resize_shrink_while_scrolled_back() {
    let mut grid = Grid::with_scrollback(10, 20, 50);

    for i in 0..30u16 {
        grid.set_cursor(grid.rows() - 1, 0);
        grid.write_char((b'A' + (i % 26) as u8) as char);
        grid.line_feed();
    }
    assert!(grid.scrollback_lines() > 0, "should have scrollback");
    grid.assert_invariants();

    let half_back = (grid.scrollback_lines() / 2) as i32;
    grid.scroll_display(half_back);
    assert!(grid.display_offset() > 0, "should be scrolled back");
    grid.assert_invariants();

    grid.resize(3, 20);
    grid.assert_invariants();

    grid.resize(10, 20);
    grid.assert_invariants();
}

/// Resize shrink to 2 rows after filling scrollback to capacity and grow back.
#[test]
fn invariants_resize_extreme_shrink_full_scrollback() {
    let mut grid = Grid::with_scrollback(8, 10, 20);

    for i in 0..40u16 {
        grid.set_cursor(grid.rows() - 1, 0);
        grid.write_char((b'0' + (i % 10) as u8) as char);
        grid.line_feed();
    }
    grid.assert_invariants();

    grid.scroll_to_top();
    assert!(grid.display_offset() > 0);
    grid.assert_invariants();

    grid.resize(2, 10);
    grid.assert_invariants();

    grid.write_char('Z');
    grid.assert_invariants();

    grid.resize(8, 10);
    grid.assert_invariants();
}

/// Simultaneous row shrink and column reflow while scrolled back preserves grid invariants.
#[test]
fn invariants_resize_reflow_while_scrolled_back() {
    let mut grid = Grid::with_scrollback(6, 20, 30);

    for _i in 0..20u16 {
        grid.set_cursor(grid.rows() - 1, 0);
        for c in "ABCDEFGHIJKLMNOP".chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }
    grid.assert_invariants();

    grid.scroll_display(5);
    assert!(grid.display_offset() > 0);

    grid.resize(3, 10);
    grid.assert_invariants();

    grid.resize(3, 40);
    grid.assert_invariants();
}

/// `visible_to_absolute` stays consistent across scrolls while the ring grows.
#[test]
fn visible_to_absolute_self_consistency_growth_phase() {
    let mut grid = Grid::with_scrollback(4, 10, 5);

    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    let abs_before = grid.visible_to_absolute(2);

    grid.set_cursor(3, 0);
    grid.line_feed();

    let abs_after = grid.visible_to_absolute(1);

    assert_eq!(
        abs_before, abs_after,
        "visible_to_absolute should be self-consistent: content 'C' was at row 2 \
         (abs={abs_before}) before scroll, now at row 1 (abs={abs_after}) after scroll. \
         Same content must produce the same absolute value."
    );
}

/// `visible_to_absolute` self-consistency once the ring is at capacity.
#[test]
fn visible_to_absolute_self_consistency_reuse_phase() {
    let mut grid = Grid::with_scrollback(4, 10, 3);

    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    for _ in 0..3 {
        grid.set_cursor(3, 0);
        grid.line_feed();
    }
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'W' + row as u8) as char);
    }

    let abs_before = grid.visible_to_absolute(2);

    grid.set_cursor(3, 0);
    grid.line_feed();

    let abs_after = grid.visible_to_absolute(1);

    assert_eq!(
        abs_before, abs_after,
        "reuse phase: content at row 2 (abs={abs_before}) should match row 1 after \
         scroll (abs={abs_after})"
    );
}

/// A resize to zero rows or columns clamps to one, the cursor with it, and
/// leaves exactly the rows the grid now has addressable.
#[test]
fn resize_to_zero_clamps_to_one_row_and_column() {
    for (rows, cols, want) in [
        (0, 10, (1, 10)),
        (5, 0, (5, 1)),
        (0, 0, (1, 1)),
        (1, 10, (1, 10)),
    ] {
        let mut grid = Grid::with_scrollback(10, 20, 50);
        for i in 0..15u16 {
            grid.set_cursor(9, 0);
            grid.write_char((b'A' + (i % 26) as u8) as char);
            grid.line_feed();
        }
        grid.set_cursor(7, 15);
        grid.resize(rows, cols);
        let at = format!("{rows}x{cols}");
        assert_eq!((grid.rows(), grid.cols()), want, "{at}");
        assert!(
            grid.cursor_row() < want.0 && grid.cursor_col() < want.1,
            "{at}"
        );
        assert!(
            grid.row(want.0 - 1).is_some() && grid.row(want.0).is_none(),
            "{at}"
        );
        grid.assert_invariants();

        grid.resize(5, 10);
        assert!(
            (0..5).all(|r| grid.row(r).is_some()) && grid.row(5).is_none(),
            "{at} regrown"
        );
        grid.assert_invariants();
    }
}

/// A row-only resize invalidates the `ComplexCharRing` so non-BMP glyphs do not alias into new row indices.
#[test]
fn row_only_resize_invalidates_complex_char_ring() {
    let mut grid = Grid::new(24, 80);

    grid.set_cursor(0, 0);
    grid.write_char('\u{1F389}');

    let text_before = grid.row_text(0).unwrap();
    assert!(
        text_before.contains('\u{1F389}'),
        "precondition: row 0 should contain party popper emoji, got: {text_before:?}"
    );

    grid.resize(48, 80);
    assert_eq!(grid.rows(), 48);

    grid.set_cursor(30, 0);
    grid.write_char('\u{1F680}');

    let text_row30 = grid.row_text(30).unwrap();
    assert!(
        text_row30.contains('\u{1F680}'),
        "row 30 should contain rocket emoji after row-only resize, got: {text_row30:?}"
    );

    let cell_row6 = grid.cell(6, 0).unwrap();
    assert!(
        cell_row6.is_empty(),
        "row 6 (alias of row 30 mod 24) should be empty, but has char {:?}",
        cell_row6.char()
    );

    grid.assert_invariants();
}
