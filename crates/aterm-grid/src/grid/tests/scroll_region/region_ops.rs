// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn grid_insert_lines_with_scroll_region() {
    let mut grid = Grid::new(8, 10);
    // Write content to each row
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    // Row 0: A, Row 1: B, Row 2: C, Row 3: D, Row 4: E, Row 5: F, Row 6: G, Row 7: H

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Cursor at row 3 (within region)
    grid.set_cursor(3, 0);
    grid.insert_lines(2);

    // Expected result:
    // Row 0: A (unchanged, outside region)
    // Row 1: B (unchanged, outside region)
    // Row 2: C (unchanged, top of region but above cursor)
    // Row 3: (blank - inserted)
    // Row 4: (blank - inserted)
    // Row 5: D (shifted from row 3, E and F pushed off bottom of region)
    // Row 6: G (unchanged, outside region)
    // Row 7: H (unchanged, outside region)

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(3, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(4, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(5, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(6, 0).unwrap().char(), 'G');
    assert_eq!(grid.cell(7, 0).unwrap().char(), 'H');
}

#[test]
fn grid_insert_lines_cursor_outside_scroll_region() {
    let mut grid = Grid::new(8, 10);
    // Write content to each row
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Cursor at row 1 (above region) - IL should have no effect
    grid.set_cursor(1, 0);
    grid.insert_lines(2);

    // All rows unchanged
    for row in 0..8 {
        assert_eq!(
            grid.cell(row, 0).unwrap().char(),
            (b'A' + row as u8) as char,
            "Row {row} should be unchanged"
        );
    }
}

#[test]
fn grid_delete_lines_with_scroll_region() {
    let mut grid = Grid::new(8, 10);
    // Write content to each row
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    // Row 0: A, Row 1: B, Row 2: C, Row 3: D, Row 4: E, Row 5: F, Row 6: G, Row 7: H

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Cursor at row 3 (within region)
    grid.set_cursor(3, 0);
    grid.delete_lines(2);

    // Expected result:
    // Row 0: A (unchanged, outside region)
    // Row 1: B (unchanged, outside region)
    // Row 2: C (unchanged, top of region but above cursor)
    // Row 3: F (shifted from row 5)
    // Row 4: (blank - inserted at bottom of region)
    // Row 5: (blank - inserted at bottom of region)
    // Row 6: G (unchanged, outside region)
    // Row 7: H (unchanged, outside region)

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'F');
    assert_eq!(grid.cell(4, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(5, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(6, 0).unwrap().char(), 'G');
    assert_eq!(grid.cell(7, 0).unwrap().char(), 'H');
}

#[test]
fn grid_delete_lines_cursor_outside_scroll_region() {
    let mut grid = Grid::new(8, 10);
    // Write content to each row
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Cursor at row 7 (below region) - DL should have no effect
    grid.set_cursor(7, 0);
    grid.delete_lines(2);

    // All rows unchanged
    for row in 0..8 {
        assert_eq!(
            grid.cell(row, 0).unwrap().char(),
            (b'A' + row as u8) as char,
            "Row {row} should be unchanged"
        );
    }
}

#[test]
fn grid_scroll_region_up_within_region() {
    let mut grid = Grid::new(8, 10);
    // Write content to each row
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    // Row 0: A, Row 1: B, Row 2: C, Row 3: D, Row 4: E, Row 5: F, Row 6: G, Row 7: H

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Scroll region up by 2 lines
    grid.scroll_region_up(2);

    // Expected result:
    // Row 0: A (unchanged, outside region)
    // Row 1: B (unchanged, outside region)
    // Row 2: E (shifted from row 4)
    // Row 3: F (shifted from row 5)
    // Row 4: (blank - inserted at bottom of region)
    // Row 5: (blank - inserted at bottom of region)
    // Row 6: G (unchanged, outside region)
    // Row 7: H (unchanged, outside region)

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'E');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'F');
    assert_eq!(grid.cell(4, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(5, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(6, 0).unwrap().char(), 'G');
    assert_eq!(grid.cell(7, 0).unwrap().char(), 'H');
    assert_eq!(
        grid.scrollback_lines(),
        0,
        "an interior region must never enter scrollback"
    );
}

/// Codex renders committed inline history through a full-width DECSTBM region
/// rooted at row zero, leaving its input/status rows fixed below the margin.
/// Content displaced through row zero must enter primary-screen history.
#[test]
fn grid_top_anchored_partial_scroll_archives_history_and_preserves_footer() {
    let mut grid = Grid::with_scrollback(5, 10, 10);
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.set_scroll_region(0, 2);
    grid.clear_damage();
    let content_gen_before = grid.content_gen();
    grid.scroll_region_up(1);

    assert_eq!(
        grid.take_absolute_row_update(),
        Some(crate::AbsoluteRowUpdate::Splice { at: 3, inserted: 1 }),
        "the protected footer moves forward in absolute-row space"
    );
    assert_eq!(
        grid.take_selection_row_update(),
        Some(crate::AbsoluteRowUpdate::Splice { at: 3, inserted: 1 }),
        "selection remapping retains an independent copy until post-processing"
    );

    assert_eq!(
        grid.content_gen(),
        content_gen_before + 1,
        "one region scroll is exactly one content mutation"
    );
    assert!(!grid.damage().is_full());
    let dirty_rows: Vec<_> = grid
        .damage()
        .iter_bounds(grid.rows(), grid.cols())
        .map(|bound| bound.line)
        .collect();
    assert_eq!(
        dirty_rows,
        vec![0, 1, 2],
        "protected footer rows must not be damaged"
    );
    assert_eq!(grid.scrollback_lines(), 1);
    assert_eq!(
        grid.get_history_line(0)
            .expect("displaced top row enters history")
            .to_string()
            .trim_end(),
        "A"
    );
    for (row, expected) in ['B', 'C', ' ', 'D', 'E'].into_iter().enumerate() {
        assert_eq!(
            grid.cell(row as u16, 0).unwrap().char(),
            expected,
            "visible row {row}"
        );
    }

    grid.scroll_display(1);
    assert_eq!(grid.display_offset(), 1, "new history is user-scrollable");
    grid.assert_invariants();
}

#[test]
fn grid_top_anchored_partial_batch_scroll_archives_each_displaced_row() {
    let mut grid = Grid::with_scrollback(5, 10, 10);
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.set_scroll_region(0, 2);
    grid.scroll_region_up(2);

    assert_eq!(grid.scrollback_lines(), 2);
    for (index, expected) in ["A", "B"].into_iter().enumerate() {
        assert_eq!(
            grid.get_history_line(index)
                .expect("each displaced row enters history")
                .to_string()
                .trim_end(),
            expected
        );
    }
    for (row, expected) in ['C', ' ', ' ', 'D', 'E'].into_iter().enumerate() {
        assert_eq!(grid.cell(row as u16, 0).unwrap().char(), expected);
    }
    grid.assert_invariants();
}

#[test]
fn grid_repeated_top_anchored_scrolls_coalesce_footer_row_splices() {
    let mut grid = Grid::with_scrollback(5, 10, 10);
    // Written displaced rows — never-written rows take the history-free
    // path (no archival, no splice; pinned separately below).
    for row in 0..2 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.set_scroll_region(0, 2);

    grid.scroll_region_up(1);
    grid.scroll_region_up(1);

    assert_eq!(
        grid.take_absolute_row_update(),
        Some(crate::AbsoluteRowUpdate::Splice { at: 3, inserted: 2 })
    );
    assert_eq!(
        grid.take_selection_row_update(),
        Some(crate::AbsoluteRowUpdate::Splice { at: 3, inserted: 2 })
    );
    assert_eq!(grid.take_absolute_row_update(), None);
    assert_eq!(grid.take_selection_row_update(), None);
}

/// Never-written displaced rows carry no transcript: a top-anchored region
/// scroll over them archives NOTHING — no minted blank history line, no
/// scrollbar growth, no footer splice. (A fresh Codex session scrolls its
/// region before anything printed at the physical top; archiving those rows
/// used to fill the ring with blanks that a later rows-grow reveal painted
/// as a dead band above the content.)
#[test]
fn grid_top_anchored_scroll_of_never_written_rows_mints_no_history() {
    let mut grid = Grid::with_scrollback(5, 10, 10);
    grid.set_scroll_region(0, 2);

    grid.scroll_region_up(1);
    grid.scroll_region_up(1);

    assert_eq!(grid.scrollback_lines(), 0, "no blank history minted");
    assert_eq!(
        grid.take_absolute_row_update(),
        None,
        "no insertion happened"
    );
    assert_eq!(grid.take_selection_row_update(), None);
    grid.assert_invariants();

    // The moment a displaced row HAS been written, archival resumes.
    grid.set_cursor(0, 0);
    grid.write_char('A');
    grid.scroll_region_up(1);
    assert_eq!(grid.scrollback_lines(), 1, "written rows archive as before");
    assert_eq!(
        grid.get_history_line(0)
            .expect("displaced written row enters history")
            .to_string()
            .trim_end(),
        "A"
    );
    grid.assert_invariants();
}

#[test]
fn grid_top_anchored_partial_scroll_keeps_history_order_at_ring_capacity() {
    let mut grid = Grid::with_scrollback(5, 10, 2);
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.scroll_up(2);
    assert_eq!(grid.scrollback_lines(), 2);

    for (row, marker) in ['K', 'L', 'M', 'N', 'O'].into_iter().enumerate() {
        grid.set_cursor(row as u16, 0);
        grid.write_char(marker);
    }
    grid.set_scroll_region(0, 2);
    grid.scroll_region_up(1);

    assert_eq!(grid.scrollback_lines(), 2);
    for (index, expected) in ["B", "K"].into_iter().enumerate() {
        assert_eq!(
            grid.get_history_line(index)
                .expect("bounded ring history remains ordered")
                .to_string()
                .trim_end(),
            expected
        );
    }
    for (row, expected) in ['L', 'M', ' ', 'N', 'O'].into_iter().enumerate() {
        assert_eq!(grid.cell(row as u16, 0).unwrap().char(), expected);
    }
    grid.assert_invariants();
}

#[test]
fn grid_top_anchored_partial_scroll_clamps_to_region_size() {
    let mut grid = Grid::with_scrollback(5, 10, 10);
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.set_scroll_region(0, 2);
    grid.scroll_region_up(usize::MAX);

    assert_eq!(grid.scrollback_lines(), 3);
    for (index, expected) in ["A", "B", "C"].into_iter().enumerate() {
        assert_eq!(
            grid.get_history_line(index)
                .expect("all and only region rows enter history")
                .to_string()
                .trim_end(),
            expected
        );
    }
    for (row, expected) in [' ', ' ', ' ', 'D', 'E'].into_iter().enumerate() {
        assert_eq!(grid.cell(row as u16, 0).unwrap().char(), expected);
    }
    grid.assert_invariants();
}

#[test]
fn grid_history_free_top_region_remains_ephemeral() {
    let mut grid = Grid::with_scrollback(5, 10, 0);
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.set_scroll_region(0, 2);
    grid.scroll_region_up(1);

    assert_eq!(grid.scrollback_lines(), 0);
    assert_eq!(grid.take_absolute_row_update(), None);
    for (row, expected) in ['B', 'C', ' ', 'D', 'E'].into_iter().enumerate() {
        assert_eq!(grid.cell(row as u16, 0).unwrap().char(), expected);
    }
    grid.assert_invariants();
}

#[test]
fn grid_zero_ring_tiered_history_archives_top_anchored_partial_scroll() {
    let scrollback = aterm_scrollback::Scrollback::new(4, 32, 100_000);
    let mut grid = Grid::with_tiered_scrollback(5, 10, 0, scrollback);
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.set_scroll_region(0, 2);
    grid.scroll_region_up(1);

    assert_eq!(grid.scrollback_lines(), 1);
    assert_eq!(
        grid.get_history_line(0)
            .expect("zero-ring tiered store receives displaced row")
            .to_string()
            .trim_end(),
        "A"
    );
    for (row, expected) in ['B', 'C', ' ', 'D', 'E'].into_iter().enumerate() {
        assert_eq!(grid.cell(row as u16, 0).unwrap().char(), expected);
    }
    grid.assert_invariants();
}

#[test]
fn grid_top_anchored_partial_scroll_preserves_dense_extras_and_bce() {
    let mut grid = Grid::with_scrollback(5, 4, 10);
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    let history_fg = [201, 31, 41];
    grid.row_mut(0).unwrap().set(
        0,
        Cell::with_style(
            'A',
            PackedColor::rgb(history_fg[0], history_fg[1], history_fg[2]),
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        ),
    );
    grid.extras_mut()
        .set_rgb_ring_range(0, 0, 1, Some(history_fg), None, 5, 4);
    grid.row_mut(0)
        .unwrap()
        .set(1, Cell::with_overflow_index(1));
    grid.set_complex_char_ring(0, 1, '🚀');

    let footer_fg = [17, 121, 231];
    grid.row_mut(4).unwrap().set(
        0,
        Cell::with_style(
            'E',
            PackedColor::rgb(footer_fg[0], footer_fg[1], footer_fg[2]),
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        ),
    );
    grid.extras_mut()
        .set_rgb_ring_range(4, 0, 1, Some(footer_fg), None, 5, 4);
    grid.row_mut(4)
        .unwrap()
        .set(1, Cell::with_overflow_index(2));
    grid.set_complex_char_ring(4, 1, '🧭');

    let bce_bg = [9, 19, 29];
    grid.set_cursor_template(
        Cell::with_style(
            ' ',
            PackedColor::DEFAULT_FG,
            PackedColor::rgb(bce_bg[0], bce_bg[1], bce_bg[2]),
            CellFlags::empty(),
        ),
        Some(bce_bg),
    );

    grid.set_scroll_region(0, 2);
    grid.scroll_region_up(1);

    let history = grid
        .get_history_line(0)
        .expect("dense metadata follows the displaced row into history");
    assert_eq!(history.get_attr(0).fg, PackedColor::rgb(201, 31, 41).0);
    assert!(history.to_string().contains('🚀'));

    assert_eq!(grid.fg_rgb_at(4, 0), Some(footer_fg));
    assert_eq!(grid.complex_char_at(4, 1), Some('🧭'));
    assert_eq!(grid.fg_rgb_at(3, 0), None);
    assert_eq!(grid.complex_char_at(3, 1), None);

    for col in 0..4 {
        assert_eq!(
            grid.bg_rgb_at(2, col),
            Some(bce_bg),
            "vacated cell {col} receives the active BCE truecolor background"
        );
    }
    assert_eq!(grid.fg_rgb_at(2, 0), None);
    assert_eq!(grid.complex_char_at(2, 1), None);
    assert_eq!(
        grid.take_content_scroll_delta(),
        0,
        "piecewise selection remapping replaces the generic clear sentinel"
    );
    assert_eq!(grid.take_content_scroll_delta(), 0);
    grid.assert_invariants();
}

#[test]
fn grid_scroll_region_down_within_region() {
    let mut grid = Grid::new(8, 10);
    // Write content to each row
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    // Row 0: A, Row 1: B, Row 2: C, Row 3: D, Row 4: E, Row 5: F, Row 6: G, Row 7: H

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Scroll region down by 2 lines
    grid.scroll_region_down(2);

    // Expected result:
    // Row 0: A (unchanged, outside region)
    // Row 1: B (unchanged, outside region)
    // Row 2: (blank - inserted at top of region)
    // Row 3: (blank - inserted at top of region)
    // Row 4: C (shifted from row 2)
    // Row 5: D (shifted from row 3, E and F pushed off)
    // Row 6: G (unchanged, outside region)
    // Row 7: H (unchanged, outside region)

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'B');
    assert_eq!(grid.cell(2, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(3, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(4, 0).unwrap().char(), 'C');
    assert_eq!(grid.cell(5, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(6, 0).unwrap().char(), 'G');
    assert_eq!(grid.cell(7, 0).unwrap().char(), 'H');
}

/// Verify that scroll_region_up resets display_offset to 0 when called with
/// nonzero display_offset (#5019). Previously this was a debug_assert panic;
/// now the function self-heals by snapping to live view.
#[test]
fn grid_scroll_region_up_resets_nonzero_display_offset() {
    let mut grid = Grid::with_scrollback(8, 10, 20);
    for row in 0..8u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.scroll_up(4);
    grid.set_scroll_region(2, 5);
    grid.scroll_display(2);
    assert_eq!(grid.display_offset(), 2);

    // Should not panic — resets display_offset and operates on live rows
    grid.scroll_region_up(1);
    assert_eq!(grid.display_offset(), 0);
    grid.assert_invariants();
}

/// Verify that scroll_region_down resets display_offset to 0 (#5019).
#[test]
fn grid_scroll_region_down_resets_nonzero_display_offset() {
    let mut grid = Grid::with_scrollback(8, 10, 20);
    for row in 0..8u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.scroll_up(4);
    grid.set_scroll_region(2, 5);
    grid.scroll_display(2);
    assert_eq!(grid.display_offset(), 2);

    grid.scroll_region_down(1);
    assert_eq!(grid.display_offset(), 0);
    grid.assert_invariants();
}

/// Verify that insert_lines resets display_offset to 0 (#5019).
#[test]
fn grid_insert_lines_resets_nonzero_display_offset() {
    let mut grid = Grid::with_scrollback(8, 10, 20);
    for row in 0..8u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.scroll_up(4);
    grid.set_scroll_region(2, 5);
    grid.set_cursor(3, 0);
    grid.scroll_display(2);
    assert_eq!(grid.display_offset(), 2);

    grid.insert_lines(1);
    assert_eq!(grid.display_offset(), 0);
    grid.assert_invariants();
}

/// Verify that delete_lines resets display_offset to 0 (#5019).
#[test]
fn grid_delete_lines_resets_nonzero_display_offset() {
    let mut grid = Grid::with_scrollback(8, 10, 20);
    for row in 0..8u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }
    grid.scroll_up(4);
    grid.set_scroll_region(2, 5);
    grid.set_cursor(3, 0);
    grid.scroll_display(2);
    assert_eq!(grid.display_offset(), 2);

    grid.delete_lines(1);
    assert_eq!(grid.display_offset(), 0);
    grid.assert_invariants();
}

// -------------------------------------------------------------------------
// Margined IL/DL tests (DECLRMM, #7408)
// -------------------------------------------------------------------------

/// IL with DECLRMM: rectangular shift within horizontal margins.
///
/// Grid 6x10, scroll region rows 1-4, horizontal margins cols 2-5.
/// Cursor at row 2. insert_lines_margined(1, true) should shift cells
/// within cols 2-5 down by 1 row within rows 2-4, leaving cols 0-1 and
/// cols 6-9 untouched.
#[test]
fn grid_insert_lines_margined_rectangular_shift() {
    let mut grid = Grid::new(6, 10);
    // Fill each row with a distinguishable letter at every column.
    for row in 0..6u16 {
        for col in 0..10u16 {
            grid.set_cursor(row, col);
            grid.write_char((b'A' + row as u8) as char);
        }
    }
    // Row 0: AAAAAAAAAA, Row 1: BBBBBBBBBB, ..., Row 5: FFFFFFFF

    grid.set_scroll_region(1, 4);
    grid.set_horizontal_margins(2, 5);

    grid.set_cursor(2, 3); // within region and margins
    grid.insert_lines_margined(1, true);

    // Cursor should move to left margin (col 2)
    assert_eq!(grid.cursor().col, 2);

    // Row 0: unchanged (outside scroll region)
    for col in 0..10u16 {
        assert_eq!(grid.cell(0, col).unwrap().char(), 'A', "row 0, col {col}");
    }

    // Row 1: unchanged (above cursor within region)
    for col in 0..10u16 {
        assert_eq!(grid.cell(1, col).unwrap().char(), 'B', "row 1, col {col}");
    }

    // Row 2: cols 0-1 unchanged ('C'), cols 2-5 blank (inserted), cols 6-9 unchanged ('C')
    for col in 0..2u16 {
        assert_eq!(grid.cell(2, col).unwrap().char(), 'C', "row 2, col {col}");
    }
    for col in 2..6u16 {
        assert_eq!(grid.cell(2, col).unwrap().char(), ' ', "row 2, col {col}");
    }
    for col in 6..10u16 {
        assert_eq!(grid.cell(2, col).unwrap().char(), 'C', "row 2, col {col}");
    }

    // Row 3: cols 0-1 unchanged ('D'), cols 2-5 shifted from row 2 ('C'), cols 6-9 unchanged ('D')
    for col in 0..2u16 {
        assert_eq!(grid.cell(3, col).unwrap().char(), 'D', "row 3, col {col}");
    }
    for col in 2..6u16 {
        assert_eq!(grid.cell(3, col).unwrap().char(), 'C', "row 3, col {col}");
    }
    for col in 6..10u16 {
        assert_eq!(grid.cell(3, col).unwrap().char(), 'D', "row 3, col {col}");
    }

    // Row 4: cols 0-1 unchanged ('E'), cols 2-5 shifted from row 3 ('D'), cols 6-9 unchanged ('E')
    for col in 0..2u16 {
        assert_eq!(grid.cell(4, col).unwrap().char(), 'E', "row 4, col {col}");
    }
    for col in 2..6u16 {
        assert_eq!(grid.cell(4, col).unwrap().char(), 'D', "row 4, col {col}");
    }
    for col in 6..10u16 {
        assert_eq!(grid.cell(4, col).unwrap().char(), 'E', "row 4, col {col}");
    }

    // Row 5: unchanged (outside scroll region)
    for col in 0..10u16 {
        assert_eq!(grid.cell(5, col).unwrap().char(), 'F', "row 5, col {col}");
    }

    grid.assert_invariants();
}

/// DL with DECLRMM: rectangular shift within horizontal margins.
///
/// Grid 6x10, scroll region rows 1-4, horizontal margins cols 2-5.
/// Cursor at row 2. delete_lines_margined(1, true) should shift cells
/// within cols 2-5 up by 1 row within rows 2-4, leaving cols 0-1 and
/// cols 6-9 untouched.
#[test]
fn grid_delete_lines_margined_rectangular_shift() {
    let mut grid = Grid::new(6, 10);
    for row in 0..6u16 {
        for col in 0..10u16 {
            grid.set_cursor(row, col);
            grid.write_char((b'A' + row as u8) as char);
        }
    }

    grid.set_scroll_region(1, 4);
    grid.set_horizontal_margins(2, 5);

    grid.set_cursor(2, 3);
    grid.delete_lines_margined(1, true);

    // Cursor should move to left margin (col 2)
    assert_eq!(grid.cursor().col, 2);

    // Row 0: unchanged
    for col in 0..10u16 {
        assert_eq!(grid.cell(0, col).unwrap().char(), 'A', "row 0, col {col}");
    }

    // Row 1: unchanged (above cursor)
    for col in 0..10u16 {
        assert_eq!(grid.cell(1, col).unwrap().char(), 'B', "row 1, col {col}");
    }

    // Row 2: cols 0-1 unchanged ('C'), cols 2-5 shifted from row 3 ('D'), cols 6-9 unchanged ('C')
    for col in 0..2u16 {
        assert_eq!(grid.cell(2, col).unwrap().char(), 'C', "row 2, col {col}");
    }
    for col in 2..6u16 {
        assert_eq!(grid.cell(2, col).unwrap().char(), 'D', "row 2, col {col}");
    }
    for col in 6..10u16 {
        assert_eq!(grid.cell(2, col).unwrap().char(), 'C', "row 2, col {col}");
    }

    // Row 3: cols 0-1 unchanged ('D'), cols 2-5 shifted from row 4 ('E'), cols 6-9 unchanged ('D')
    for col in 0..2u16 {
        assert_eq!(grid.cell(3, col).unwrap().char(), 'D', "row 3, col {col}");
    }
    for col in 2..6u16 {
        assert_eq!(grid.cell(3, col).unwrap().char(), 'E', "row 3, col {col}");
    }
    for col in 6..10u16 {
        assert_eq!(grid.cell(3, col).unwrap().char(), 'D', "row 3, col {col}");
    }

    // Row 4: cols 0-1 unchanged ('E'), cols 2-5 blank (vacated), cols 6-9 unchanged ('E')
    for col in 0..2u16 {
        assert_eq!(grid.cell(4, col).unwrap().char(), 'E', "row 4, col {col}");
    }
    for col in 2..6u16 {
        assert_eq!(grid.cell(4, col).unwrap().char(), ' ', "row 4, col {col}");
    }
    for col in 6..10u16 {
        assert_eq!(grid.cell(4, col).unwrap().char(), 'E', "row 4, col {col}");
    }

    // Row 5: unchanged
    for col in 0..10u16 {
        assert_eq!(grid.cell(5, col).unwrap().char(), 'F', "row 5, col {col}");
    }

    grid.assert_invariants();
}

/// IL margined with lrmm=false falls back to full-width insert_lines.
#[test]
fn grid_insert_lines_margined_lrmm_off_falls_back() {
    let mut grid = Grid::new(6, 10);
    for row in 0..6u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.set_scroll_region(1, 4);
    grid.set_horizontal_margins(2, 5);
    grid.set_cursor(2, 0);

    // lrmm=false: should do full-width IL regardless of horizontal margins
    grid.insert_lines_margined(1, false);

    // Full-width shift: row 2 should be blank, row 3 should have old row 2 content
    assert_eq!(grid.cell(2, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'C');
    grid.assert_invariants();
}

/// DL margined with full-width margins falls back to full-width delete_lines.
#[test]
fn grid_delete_lines_margined_full_width_falls_back() {
    let mut grid = Grid::new(6, 10);
    for row in 0..6u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.set_scroll_region(1, 4);
    // Full-width margins (0 to cols-1)
    grid.set_horizontal_margins(0, 9);
    grid.set_cursor(2, 0);

    // Even with lrmm=true, full-width margins should fall back to full-width DL
    grid.delete_lines_margined(1, true);

    // Full-width shift: row 2 should have old row 3 content
    assert_eq!(grid.cell(2, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(4, 0).unwrap().char(), ' ');
    grid.assert_invariants();
}

/// IL margined cursor outside scroll region is noop.
#[test]
fn grid_insert_lines_margined_cursor_outside_noop() {
    let mut grid = Grid::new(6, 10);
    for row in 0..6u16 {
        for col in 0..10u16 {
            grid.set_cursor(row, col);
            grid.write_char((b'A' + row as u8) as char);
        }
    }
    grid.set_scroll_region(2, 4);
    grid.set_horizontal_margins(2, 5);

    // Cursor above scroll region
    grid.set_cursor(1, 3);
    grid.insert_lines_margined(1, true);

    // Nothing should change within the scroll region
    for col in 2..6u16 {
        assert_eq!(grid.cell(2, col).unwrap().char(), 'C', "row 2, col {col}");
        assert_eq!(grid.cell(3, col).unwrap().char(), 'D', "row 3, col {col}");
        assert_eq!(grid.cell(4, col).unwrap().char(), 'E', "row 4, col {col}");
    }
    grid.assert_invariants();
}

/// IL/DL margined with count exceeding region size clamps correctly.
#[test]
fn grid_insert_lines_margined_count_exceeds_region() {
    let mut grid = Grid::new(6, 10);
    for row in 0..6u16 {
        for col in 0..10u16 {
            grid.set_cursor(row, col);
            grid.write_char((b'A' + row as u8) as char);
        }
    }
    grid.set_scroll_region(1, 4);
    grid.set_horizontal_margins(2, 5);
    grid.set_cursor(2, 3);

    // Insert 100 lines (exceeds region size 3) — should clamp to 3
    grid.insert_lines_margined(100, true);

    // All cells within margins and region should be blank
    for row in 2..5u16 {
        for col in 2..6u16 {
            assert_eq!(
                grid.cell(row, col).unwrap().char(),
                ' ',
                "row {row}, col {col}"
            );
        }
    }
    // Cells outside margins should be unchanged
    for row in 2..5u16 {
        for col in 0..2u16 {
            let expected = (b'C' + (row - 2) as u8) as char;
            assert_eq!(
                grid.cell(row, col).unwrap().char(),
                expected,
                "row {row}, col {col}"
            );
        }
    }
    grid.assert_invariants();
}
