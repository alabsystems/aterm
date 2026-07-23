// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::super::reflow::ReflowMode;
use super::*;
// Scroll region and damage tracking
// ========================================================================

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
        "precondition: resize should clear a live deferred wrap"
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

    assert!(
        !grid.pending_wrap(),
        "resize should clear deferred wrap state"
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

    // scroll_up(1) uses targeted row damage (#5227): only bottom N rows
    // are marked dirty, not full screen. Full damage requires n >= rows.
    grid.scroll_up(1);
    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "scroll_up(1) on 3-row grid should mark targeted damage, not full"
    );

    // scroll_up(n) with n >= visible_rows marks full damage
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
fn clamp_display_offset_small_delta_marks_bottom_rows_dirty() {
    let mut grid = Grid::with_scrollback(5, 3, 10);
    for marker in ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'] {
        write_marker_line(&mut grid, marker);
    }

    let max_offset = grid.scrollback_lines();
    assert!(max_offset >= 2, "precondition: expected clamp headroom");

    grid.clear_damage();
    grid.storage.display_offset = max_offset + 2;
    grid.clamp_display_offset();

    assert_eq!(grid.display_offset(), max_offset);
    assert!(grid.damage().has_damage());
    assert!(
        !grid.damage().is_full(),
        "clamp_display_offset with a 2-line delta should use targeted damage"
    );

    for row in 0..3u16 {
        assert!(
            !grid.damage().is_row_damaged(row),
            "row {row} should stay clean after a 2-line clamp"
        );
    }
    for row in 3..5u16 {
        assert!(
            grid.damage().is_row_damaged(row),
            "row {row} should be dirty after a 2-line clamp"
        );
    }
}

// ========================================================================
// Invariant checks after all operations
// ========================================================================

#[test]
fn invariants_after_erase_display_modes() {
    let mut grid = Grid::new(5, 10);
    fill_grid_rows(&mut grid, 5);

    grid.set_cursor(2, 5);
    grid.erase_to_end_of_screen();
    grid.assert_invariants();

    fill_grid_rows(&mut grid, 5);
    grid.set_cursor(2, 5);
    grid.erase_from_start_of_screen();
    grid.assert_invariants();

    fill_grid_rows(&mut grid, 5);
    grid.erase_screen();
    grid.assert_invariants();
}

#[test]
fn invariants_after_line_feed_with_scroll_region() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(3, 7);

    for row in 3..=7 {
        grid.set_cursor(row, 0);
        grid.line_feed();
        grid.assert_invariants();
    }
}

#[test]
fn invariants_after_reverse_line_feed_with_scroll_region() {
    let mut grid = Grid::new(10, 10);
    grid.set_scroll_region(3, 7);

    for row in (3..=7).rev() {
        grid.set_cursor(row, 0);
        grid.reverse_line_feed();
        grid.assert_invariants();
    }
}

// Algorithm audit: ring buffer + resize boundary tests

#[test]
fn row_shrink_after_scrolling_preserves_newest_content() {
    // Verify that shrinking rows after scrolling (ring_head != 0) preserves
    // the most recent visible rows, not the oldest.
    let mut grid = Grid::with_scrollback(5, 10, 0);
    // Fill all 5 rows: A, B, C, D, E
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    // Scroll twice from bottom to advance ring_head.
    // With scrollback=0, each scroll immediately recycles the oldest row.
    grid.set_cursor(4, 0);
    grid.line_feed();
    grid.line_feed();
    // After 2 scrolls: visible rows are C, D, E, <blank>, <blank>
    // Write distinct content to the last two rows
    grid.set_cursor(3, 0);
    grid.write_char('X');
    grid.set_cursor(4, 0);
    grid.write_char('Y');
    // Now visible: C, D, E, X, Y  (ring_head != 0)
    // Shrink from 5 to 3 rows. With scrollback=0, no scrollback to trim,
    // so bottom rows are removed. Should keep top 3: C, D, E.
    grid.resize_with_reflow_mode(3, 10, ReflowMode::Enabled);
    assert_eq!(grid.rows(), 3);
    for row in 0..3 {
        grid.row(row)
            .expect("row should be accessible after shrink");
    }
    // With no scrollback, shrink removes from bottom (X, Y discarded).
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'E');
    assert!(grid.cursor_row() < 3);
    grid.assert_invariants();
}

#[test]
fn row_shrink_with_scrollback_retains_history_within_the_ring_cap() {
    // A rows-only shrink on a RING-ONLY grid reclassifies instead of
    // trimming (the ring IS retention — see adjust_row_count_ring_only):
    // the demoted bottom row joins history and existing scrollback stays,
    // bounded by the ring cap. (This test previously pinned the old
    // trim-to-viewport behavior, which wiped ring history on every
    // height-only resize — the orc-reported retention bug.)
    let mut grid = Grid::with_scrollback(4, 10, 4);
    // Fill 4 rows: A, B, C, D
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    // Scroll 3 times to push A, B, C into scrollback.
    grid.set_cursor(3, 0);
    grid.line_feed();
    grid.line_feed();
    grid.line_feed();
    // Write to new visible rows
    grid.set_cursor(1, 0);
    grid.write_char('X');
    grid.set_cursor(2, 0);
    grid.write_char('Y');
    grid.set_cursor(3, 0);
    grid.write_char('Z');
    // Ring buffer: 4 visible + 3 scrollback = 7 rows total
    // Logical order: A, B, C (scrollback), D, X, Y, Z (visible)
    // Shrink visible to 3 rows: the bottom visible row Z is demoted to the
    // newest history line (#7662's bottom-push), everything else stays.
    // Result: history [A, B, C, Z] (exactly at the cap of 4), visible [D, X, Y].
    grid.resize_with_reflow_mode(3, 10, ReflowMode::Enabled);
    assert_eq!(grid.rows(), 3);
    for row in 0..3 {
        grid.row(row).expect("row should be accessible");
    }
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'X');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'Y');
    // History retained (nothing evicted: 4 lines fit the cap of 4).
    assert_eq!(grid.scrollback_lines(), 4);
    let history: Vec<String> = (0..4)
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(history, ["A", "B", "C", "Z"]);
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
        // Rows beyond the pre-grow count (2) must be blank.
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
fn reflow_shrink_cursor_at_chunk_boundary_exact() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 5);
    grid.resize(3, 5);
    // Cursor was at (0, 5) in the 10-wide grid. Column 5 is the first cell
    // of the second chunk after reflow to 5-wide, so cursor maps to (1, 0).
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 0);
    grid.assert_invariants();
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");
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

// Algorithm audit: reflow cursor boundary conditions
// ========================================================================

/// Reflow shrink: cursor past the end of cell content should be clamped.
#[test]
fn reflow_shrink_cursor_past_content_end() {
    let mut grid = Grid::new(3, 10);
    grid.set_cursor(0, 0);
    for c in "ABC".chars() {
        grid.write_char(c);
    }
    // Place cursor at col 8 — far past the 3 chars of content
    grid.set_cursor(0, 8);

    grid.resize(3, 5);

    // Cursor should be clamped to valid position within the new grid
    assert!(
        grid.cursor_col() < 5,
        "cursor col {} should be < new_cols 5",
        grid.cursor_col()
    );
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 1).unwrap().char(), 'B');
    assert_eq!(grid.cell(0, 2).unwrap().char(), 'C');
    grid.assert_invariants();
}

/// Reflow grow: cursor on last content cell stays with the content.
#[test]
fn reflow_grow_cursor_on_last_content_cell() {
    let mut grid = Grid::new(3, 5);
    grid.set_cursor(0, 0);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 4); // Last cell with content

    grid.resize(3, 10);

    // Cursor should stay at col 4 (content didn't move)
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 4);
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 4).unwrap().char(), 'E');
    grid.assert_invariants();
}

/// Reflow grow: cursor in trailing blank area of a continuation row should
/// remain on the same logical line (the merged row), not jump to a different line.
///
/// Bug: reflow_grow_columns uses row.len() (content length) rather than
/// row width when computing cursor_logical_offset for merged continuation rows.
/// When cursor_col exceeds the continuation row's content length, the computed
/// offset exceeds the merged cells length. No chunk matches, and the cursor
/// retains its initial value (original row index), which is wrong after reflow
/// because row indices have changed.
///
/// Part of #2741: algorithm_audit boundary condition finding.
#[test]
fn reflow_grow_cursor_in_trailing_blank_of_continuation_row() {
    // Setup: 5-col terminal, 4 visible rows
    let mut grid = Grid::new(4, 5);

    // Row 0: "ABCDE" — fills the row, autowrap makes row 1 a continuation
    grid.set_cursor(0, 0);
    for c in "ABCDE".chars() {
        grid.write_char_wrap(c);
    }
    // Row 1 is now a continuation (wrapped). Write "FG" on it.
    for c in "FG".chars() {
        grid.write_char_wrap(c);
    }
    // Row 2: separate line "XY"
    grid.set_cursor(2, 0);
    for c in "XY".chars() {
        grid.write_char(c);
    }

    // Verify preconditions
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert!(grid.row(1).unwrap().is_wrapped(), "row 1 should be wrapped");

    // Place cursor at col 4 on the continuation row (trailing blank area past "FG")
    grid.set_cursor(1, 4);
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 4);

    // Resize wider: rows 0+1 should merge into a single row "ABCDEFG"
    grid.resize(4, 10);

    // After merge, cursor should be on row 0 (the merged line), NOT row 1 ("XY")
    // The cursor was on the continuation of the "ABCDE"→"FG" logical line, so it
    // must remain on that logical line after reflow.
    assert_eq!(
        grid.cursor_row(),
        0,
        "cursor should be on merged row 0, not row {} (was on continuation of 'ABCDE'→'FG')",
        grid.cursor_row()
    );
    // Column should be clamped to content end or preserved within the merged row
    assert!(
        grid.cursor_col() <= 9,
        "cursor col {} should be within new_cols 10",
        grid.cursor_col()
    );
    // Content should be correctly merged
    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(0, 6).unwrap().char(), 'G');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'X');
    grid.assert_invariants();
}

/// Reflow shrink to 1-column terminal: all content wraps to individual rows.
#[test]
fn reflow_shrink_to_single_column() {
    let mut grid = Grid::new(5, 5);
    grid.set_cursor(0, 0);
    for c in "ABC".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 0);

    grid.resize(5, 1);

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'C');
    assert!(grid.cursor_col() < 1, "cursor should be clamped to col 0");
    grid.assert_invariants();
}

// visible_to_absolute coordinate consistency
// ========================================================================

/// Resize shrink while scrolled back: TotalLinesMinimum and TotalLinesValid
/// must hold when `display_offset > 0` and visible rows are reduced.
///
/// This is the critical scenario that #4304 invariants were designed to catch:
/// `total_lines` and `visible_rows` are updated independently during resize,
/// and display_offset complicates the bookkeeping. A bug here causes
/// `row_index()` to compute incorrect ring_scrollback values.
#[test]
fn invariants_resize_shrink_while_scrolled_back() {
    let mut grid = Grid::with_scrollback(10, 20, 50);

    // Fill 30 lines of content to build scrollback.
    for i in 0..30u16 {
        grid.set_cursor(grid.rows() - 1, 0);
        grid.write_char((b'A' + (i % 26) as u8) as char);
        grid.line_feed();
    }
    assert!(grid.scrollback_lines() > 0, "should have scrollback");
    grid.assert_invariants();

    // Scroll display back to middle of scrollback.
    let half_back = (grid.scrollback_lines() / 2) as i32;
    grid.scroll_display(half_back);
    assert!(grid.display_offset() > 0, "should be scrolled back");
    grid.assert_invariants();

    // Shrink rows from 10 to 3 while scrolled back.
    grid.resize(3, 20);
    grid.assert_invariants();

    // Grow back to 10 while still potentially scrolled back.
    grid.resize(10, 20);
    grid.assert_invariants();
}

/// Resize shrink to minimum (2 rows) after filling scrollback to capacity,
/// then grow back. Exercises TotalLinesMinimum under extreme pressure.
#[test]
fn invariants_resize_extreme_shrink_full_scrollback() {
    let mut grid = Grid::with_scrollback(8, 10, 20);

    // Fill scrollback to capacity: write 40 lines of content.
    for i in 0..40u16 {
        grid.set_cursor(grid.rows() - 1, 0);
        grid.write_char((b'0' + (i % 10) as u8) as char);
        grid.line_feed();
    }
    grid.assert_invariants();

    // Scroll to top of scrollback.
    grid.scroll_to_top();
    assert!(grid.display_offset() > 0);
    grid.assert_invariants();

    // Extreme shrink: 8 rows → 2 rows while at top of scrollback.
    grid.resize(2, 10);
    grid.assert_invariants();

    // Verify grid is functional after extreme shrink.
    grid.write_char('Z');
    grid.assert_invariants();

    // Grow back.
    grid.resize(8, 10);
    grid.assert_invariants();
}

/// Resize with simultaneous row shrink and column change while scrolled back.
/// Column change triggers reflow, which interacts with ring buffer state.
#[test]
fn invariants_resize_reflow_while_scrolled_back() {
    let mut grid = Grid::with_scrollback(6, 20, 30);

    // Fill with wrappable content.
    for _i in 0..20u16 {
        grid.set_cursor(grid.rows() - 1, 0);
        for c in "ABCDEFGHIJKLMNOP".chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }
    grid.assert_invariants();

    // Scroll back.
    grid.scroll_display(5);
    assert!(grid.display_offset() > 0);

    // Shrink both rows AND columns (triggers reflow + ring buffer adjustment).
    grid.resize(3, 10);
    grid.assert_invariants();

    // Grow columns back (triggers reverse reflow).
    grid.resize(3, 40);
    grid.assert_invariants();
}

/// visible_to_absolute must be self-consistent across scroll operations:
/// if content is at visible row R and gets absolute value A, then after
/// scrolling K lines, that content (now at visible row R-K) must still
/// produce absolute value A.
///
/// Part of #3974: the old formula subtracted scrollback_lines, which grows
/// during the ring buffer growth phase and caused absolute values to drift.
/// Fixed by removing the spurious subtraction.
#[test]
fn visible_to_absolute_self_consistency_growth_phase() {
    // Use small scrollback to quickly test the growth phase.
    let mut grid = Grid::with_scrollback(4, 10, 5);

    // Fill all visible rows with content.
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Record absolute position of visible row 2 (content 'C') before scrolling.
    let abs_before = grid.visible_to_absolute(2);

    // Scroll up by 1 (growth phase: ring buffer grows from 4 to 5).
    grid.set_cursor(3, 0);
    grid.line_feed();

    // Content 'C' was at visible row 2, now at visible row 1 (shifted up by 1).
    let abs_after = grid.visible_to_absolute(1);

    assert_eq!(
        abs_before, abs_after,
        "visible_to_absolute should be self-consistent: content 'C' was at row 2 \
         (abs={abs_before}) before scroll, now at row 1 (abs={abs_after}) after scroll. \
         Same content must produce the same absolute value."
    );
}

/// visible_to_absolute self-consistency during reuse phase (ring at capacity).
/// After removing the scrollback subtraction, both phases produce correct
/// absolute values.
#[test]
fn visible_to_absolute_self_consistency_reuse_phase() {
    let mut grid = Grid::with_scrollback(4, 10, 3);

    // Fill and scroll enough to reach ring capacity (4 visible + 3 scrollback = 7 rows).
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    // 3 scrolls to fill scrollback.
    for _ in 0..3 {
        grid.set_cursor(3, 0);
        grid.line_feed();
    }
    // Write identifiable content at the current visible rows.
    for row in 0..4 {
        grid.set_cursor(row, 0);
        grid.write_char((b'W' + row as u8) as char);
    }

    // Now in reuse phase. Record absolute for visible row 2 ('Y').
    let abs_before = grid.visible_to_absolute(2);

    // Scroll 1 more (reuse).
    grid.set_cursor(3, 0);
    grid.line_feed();

    // 'Y' content moved from visible row 2 to visible row 1.
    let abs_after = grid.visible_to_absolute(1);

    assert_eq!(
        abs_before, abs_after,
        "reuse phase: content at row 2 (abs={abs_before}) should match row 1 after \
         scroll (abs={abs_after})"
    );
}

// Algorithm audit: resize-to-zero clamping boundary
// ========================================================================

/// Resize an existing populated grid to 0 rows — verifies the `max(1)` clamp
/// in `resize_with_reflow_mode` preserves at least 1 row and doesn't panic.
#[test]
fn resize_existing_grid_to_zero_rows() {
    let mut grid = Grid::new(5, 10);
    for c in "Hello".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(2, 3);

    grid.resize(0, 10);

    assert_eq!(grid.rows(), 1, "0 rows should be clamped to 1");
    assert_eq!(grid.cols(), 10);
    assert!(grid.cursor_row() < 1, "cursor row clamped to within 1 row");
    grid.assert_invariants();
}

/// Resize an existing populated grid to 0 cols — verifies the `max(1)` clamp
/// and that cursor column is clamped to the single available column.
#[test]
fn resize_existing_grid_to_zero_cols() {
    let mut grid = Grid::new(5, 10);
    for c in "Hello".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(2, 7);

    grid.resize(5, 0);

    assert_eq!(grid.rows(), 5);
    assert_eq!(grid.cols(), 1, "0 cols should be clamped to 1");
    assert_eq!(grid.cursor_col(), 0, "cursor col clamped to col 0");
    grid.assert_invariants();
}

/// Resize an existing populated grid to 0x0 — both dimensions clamped to 1.
#[test]
fn resize_existing_grid_to_zero_both() {
    let mut grid = Grid::with_scrollback(10, 20, 50);
    // Generate some scrollback
    for i in 0..15u16 {
        grid.set_cursor(9, 0);
        grid.write_char((b'A' + (i % 26) as u8) as char);
        grid.line_feed();
    }

    grid.resize(0, 0);

    assert_eq!(grid.rows(), 1, "0 rows → 1");
    assert_eq!(grid.cols(), 1, "0 cols → 1");
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 0);
    grid.assert_invariants();
}

// Algorithm audit: row_index on minimal grid
// ========================================================================

/// `row_index` on a 1-row grid with no scrollback — the modulo path
/// `(ring_head + absolute_row) % rows.len()` must not divide by zero and
/// must return the correct index for the sole visible row.
#[test]
fn row_index_one_row_grid_visible_row_zero() {
    let grid = Grid::new(1, 10);
    // row(0) should succeed — the only visible row
    assert!(grid.row(0).is_some(), "row 0 should exist in 1-row grid");
    // row(1) should be None — out of bounds
    assert!(
        grid.row(1).is_none(),
        "row 1 should not exist in 1-row grid"
    );
    grid.assert_invariants();
}

/// After resize from multi-row to 1 row, row_index still works correctly.
#[test]
fn row_index_after_resize_to_one_row() {
    let mut grid = Grid::new(5, 10);
    for row in 0..5u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.resize(1, 10);

    assert_eq!(grid.rows(), 1);
    assert!(grid.row(0).is_some(), "row 0 accessible after resize to 1");
    assert!(
        grid.row(1).is_none(),
        "row 1 inaccessible after resize to 1"
    );
    grid.assert_invariants();
}

/// After resize from 1 row to multi-row, all rows accessible via row_index.
#[test]
fn row_index_after_resize_from_one_row() {
    let mut grid = Grid::new(1, 10);
    grid.write_char('X');

    grid.resize(5, 10);

    assert_eq!(grid.rows(), 5);
    for r in 0..5u16 {
        assert!(grid.row(r).is_some(), "row {r} should exist after grow");
    }
    assert!(grid.row(5).is_none(), "row 5 out of bounds");
    grid.assert_invariants();
}

// ComplexCharRing invalidation on row-only resize
// ========================================================================

/// Row-only resize (same columns) must invalidate the ComplexCharRing so
/// that ring-stored emoji data from the old geometry does not alias into
/// new row indices.
///
/// Regression test for #7260: `invalidate_rings()` in reflow.rs.
#[test]
fn row_only_resize_invalidates_complex_char_ring() {
    let mut grid = Grid::new(24, 80);

    // Write a supplementary-plane emoji to row 0 — this goes through the
    // ComplexCharRing because U+1F389 (party popper) is outside the BMP.
    grid.set_cursor(0, 0);
    grid.write_char('\u{1F389}');

    let text_before = grid.row_text(0).unwrap();
    assert!(
        text_before.contains('\u{1F389}'),
        "precondition: row 0 should contain party popper emoji, got: {text_before:?}"
    );

    // Row-only resize: 24 -> 48 rows, columns unchanged.
    // This must invalidate the ComplexCharRing; otherwise the old 24-row
    // ring dimensions remain and row 30 aliases to row 30 % 24 = 6.
    grid.resize(48, 80);
    assert_eq!(grid.rows(), 48);

    // Write a different emoji to row 30 (beyond the old row count).
    grid.set_cursor(30, 0);
    grid.write_char('\u{1F680}');

    // Row 30 should contain the rocket emoji.
    let text_row30 = grid.row_text(30).unwrap();
    assert!(
        text_row30.contains('\u{1F680}'),
        "row 30 should contain rocket emoji after row-only resize, got: {text_row30:?}"
    );

    // Row 6 (= 30 % 24, the alias under stale ring dimensions) must be
    // empty — if the ring was not invalidated, the emoji data would leak here.
    let cell_row6 = grid.cell(6, 0).unwrap();
    assert!(
        cell_row6.is_empty(),
        "row 6 (alias of row 30 mod 24) should be empty, but has char {:?}",
        cell_row6.char()
    );

    grid.assert_invariants();
}
