// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn grid_insert_lines_with_scroll_region() {
    let mut grid = lettered(Grid::new(8, 10));

    grid.set_scroll_region(2, 5);

    grid.set_cursor(3, 0);
    grid.insert_lines(2);

    // Two blanks inserted at row 3; E, F pushed off the region bottom.
    assert_eq!(col0(&grid), "ABC  DGH");
}

#[test]
fn grid_insert_lines_cursor_outside_scroll_region() {
    let mut grid = lettered(Grid::new(8, 10));

    grid.set_scroll_region(2, 5);

    // Cursor above the region: IL is a no-op.
    grid.set_cursor(1, 0);
    grid.insert_lines(2);

    assert_eq!(col0(&grid), "ABCDEFGH");
}

#[test]
fn grid_delete_lines_with_scroll_region() {
    let mut grid = lettered(Grid::new(8, 10));

    grid.set_scroll_region(2, 5);

    grid.set_cursor(3, 0);
    grid.delete_lines(2);

    // D, E deleted; blanks fill the region bottom.
    assert_eq!(col0(&grid), "ABCF  GH");
}

#[test]
fn grid_delete_lines_cursor_outside_scroll_region() {
    let mut grid = lettered(Grid::new(8, 10));

    grid.set_scroll_region(2, 5);

    // Cursor below the region: DL is a no-op.
    grid.set_cursor(7, 0);
    grid.delete_lines(2);

    assert_eq!(col0(&grid), "ABCDEFGH");
}

#[test]
fn grid_scroll_region_up_within_region() {
    let mut grid = lettered(Grid::new(8, 10));

    grid.set_scroll_region(2, 5);

    grid.scroll_region_up(2);

    // C, D scrolled out; blanks fill the region bottom.
    assert_eq!(col0(&grid), "ABEF  GH");
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
    let mut grid = lettered(Grid::with_scrollback(5, 10, 10));

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
    assert_eq!(col0(&grid), "BC DE");

    grid.scroll_display(1);
    assert_eq!(grid.display_offset(), 1, "new history is user-scrollable");
    grid.assert_invariants();
}

#[test]
fn grid_repeated_top_anchored_scrolls_coalesce_footer_row_splices() {
    let mut grid = Grid::with_scrollback(5, 10, 10);
    // Written rows: never-written ones archive nothing and splice nothing (pinned below).
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

/// While the grid holds no history, a top-anchored region scroll over blank
/// rows archives nothing: no blank history line, no scrollbar growth, no footer
/// splice. Once any history exists, blank displaced rows archive like written
/// ones (`grid_top_anchored_scroll_archives_interior_blank_between_written_lines`).
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

    // Once a displaced row has been written, archival resumes.
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

/// A blank row displaced between written lines is transcript (a paragraph
/// break streamed as "A", blank, "B" one scroll at a time) and archives once
/// any history exists.
#[test]
fn grid_top_anchored_scroll_archives_interior_blank_between_written_lines() {
    let mut grid = Grid::with_scrollback(5, 10, 10);
    grid.set_scroll_region(0, 2);
    // Region content: A, blank separator (never written), B.
    grid.set_cursor(0, 0);
    grid.write_char('A');
    grid.set_cursor(2, 0);
    grid.write_char('B');

    grid.scroll_region_up(1); // displaces A — archives, history begins
    grid.scroll_region_up(1); // displaces the blank separator alone
    grid.scroll_region_up(1); // displaces B

    assert_eq!(
        grid.scrollback_lines(),
        3,
        "the paragraph break must be retained in history"
    );
    for (index, expected) in ["A", "", "B"].into_iter().enumerate() {
        assert_eq!(
            grid.get_history_line(index)
                .expect("every displaced row enters history")
                .to_string()
                .trim_end(),
            expected,
            "history line {index}"
        );
    }
    grid.assert_invariants();
}

/// A written row erased back to blank (`Row::len` 0 after EL/ED) is still
/// transcript once history exists and archives as an empty line.
#[test]
fn grid_top_anchored_scroll_archives_written_then_erased_row() {
    let mut grid = Grid::with_scrollback(5, 10, 10);
    grid.set_scroll_region(0, 2);
    // Leave the fresh-session band: archive one written row first.
    grid.set_cursor(0, 0);
    grid.write_char('A');
    grid.scroll_region_up(1);
    assert_eq!(grid.scrollback_lines(), 1);

    // Write a row, then EL-erase it with the default background template.
    grid.set_cursor(0, 0);
    for c in "gone".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 0);
    grid.erase_line();
    grid.scroll_region_up(1);

    assert_eq!(
        grid.scrollback_lines(),
        2,
        "an erased row is transcript, not a fresh-session blank"
    );
    assert_eq!(
        grid.get_history_line(1)
            .expect("displaced erased row enters history")
            .to_string()
            .trim_end(),
        ""
    );
    grid.assert_invariants();
}

/// A fresh-session batch scroll mixing leading never-written rows with written
/// content drops only the leading blank prefix: history starts at the first
/// written row, the footer splice covers only the archived count, and the
/// screen equals a single scroll of the full amount.
#[test]
fn grid_fresh_session_mixed_batch_scroll_drops_only_leading_blank_prefix() {
    let mut grid = Grid::with_scrollback(5, 10, 10);
    grid.set_scroll_region(0, 2);
    // Rows 0-1 never written; row 2 written; footer rows written.
    grid.set_cursor(2, 0);
    grid.write_char('X');
    grid.set_cursor(3, 0);
    grid.write_char('D');
    grid.set_cursor(4, 0);
    grid.write_char('E');

    grid.scroll_region_up(3);

    assert_eq!(
        grid.scrollback_lines(),
        1,
        "history starts at the first written row — no leading blanks minted"
    );
    assert_eq!(
        grid.get_history_line(0)
            .expect("first written displaced row enters history")
            .to_string()
            .trim_end(),
        "X"
    );
    assert_eq!(
        grid.take_absolute_row_update(),
        Some(crate::AbsoluteRowUpdate::Splice { at: 3, inserted: 1 }),
        "the footer splice covers only the archived suffix"
    );
    assert_eq!(
        col0(&grid),
        "   DE",
        "the screen must match a single scroll of the full amount"
    );
    grid.assert_invariants();
}

#[test]
fn grid_top_anchored_partial_scroll_keeps_history_order_at_ring_capacity() {
    let mut grid = lettered(Grid::with_scrollback(5, 10, 2));
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
    assert_eq!(col0(&grid), "LM NO");
    grid.assert_invariants();
}

#[test]
fn grid_top_anchored_partial_scroll_clamps_to_region_size() {
    let mut grid = lettered(Grid::with_scrollback(5, 10, 10));

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
    assert_eq!(col0(&grid), "   DE");
    grid.assert_invariants();
}

#[test]
fn grid_history_free_top_region_remains_ephemeral() {
    let mut grid = lettered(Grid::with_scrollback(5, 10, 0));

    grid.set_scroll_region(0, 2);
    grid.scroll_region_up(1);

    assert_eq!(grid.scrollback_lines(), 0);
    assert_eq!(grid.take_absolute_row_update(), None);
    assert_eq!(col0(&grid), "BC DE");
    grid.assert_invariants();
}

#[test]
fn grid_zero_ring_tiered_history_archives_top_anchored_partial_scroll() {
    let scrollback = aterm_scrollback::Scrollback::new(4, 32, 100_000);
    let mut grid = lettered(Grid::with_tiered_scrollback(5, 10, 0, scrollback));

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
    assert_eq!(col0(&grid), "BC DE");
    grid.assert_invariants();
}

#[test]
fn grid_top_anchored_partial_scroll_preserves_dense_extras_and_bce() {
    let mut grid = lettered(Grid::with_scrollback(5, 4, 10));

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
    let mut grid = lettered(Grid::new(8, 10));

    grid.set_scroll_region(2, 5);

    grid.scroll_region_down(2);

    // Blanks inserted at the region top; E, F pushed off.
    assert_eq!(col0(&grid), "AB  CDGH");
}

/// Every region line op (SU, SD, IL, DL) run with a nonzero display_offset
/// snaps the viewport to the live view.
#[test]
fn grid_region_line_ops_reset_nonzero_display_offset() {
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let ops: [(&str, fn(&mut Grid)); 4] = [
        ("scroll_region_up", |g| g.scroll_region_up(1)),
        ("scroll_region_down", |g| g.scroll_region_down(1)),
        ("insert_lines", |g| g.insert_lines(1)),
        ("delete_lines", |g| g.delete_lines(1)),
    ];
    for (name, op) in ops {
        let mut grid = lettered(Grid::with_scrollback(8, 10, 20));
        grid.scroll_up(4);
        grid.set_scroll_region(2, 5);
        grid.set_cursor(3, 0);
        grid.scroll_display(2);
        assert_eq!(grid.display_offset(), 2, "{name}: precondition");

        op(&mut grid);
        assert_eq!(grid.display_offset(), 0, "{name}");
        grid.assert_invariants();
    }
}

// Margined IL/DL tests (DECLRMM)

/// A grid whose every cell in row `r` holds the letter `A + r`.
fn filled(rows: u16, cols: u16) -> Grid {
    let mut grid = Grid::new(rows, cols);
    for row in 0..rows {
        for col in 0..cols {
            grid.set_cursor(row, col);
            grid.write_char((b'A' + row as u8) as char);
        }
    }
    grid
}

/// Every visible row as its full-width cell characters.
fn screen(grid: &Grid) -> Vec<String> {
    (0..grid.rows())
        .map(|row| {
            (0..grid.cols())
                .map(|col| grid.cell(row, col).unwrap().char())
                .collect()
        })
        .collect()
}

/// IL with DECLRMM shifts a rectangle: region rows 1-4, margins cols 2-5,
/// cursor row 2. Cols 2-5 of rows 2-4 move down one; the rest is untouched.
#[test]
fn grid_insert_lines_margined_rectangular_shift() {
    let mut grid = filled(6, 10);
    grid.set_scroll_region(1, 4);
    grid.set_horizontal_margins(2, 5);

    grid.set_cursor(2, 3);
    grid.insert_lines_margined(1, true);

    assert_eq!(grid.cursor().col, 2, "cursor moves to the left margin");
    assert_eq!(
        screen(&grid),
        [
            "AAAAAAAAAA",
            "BBBBBBBBBB",
            "CC    CCCC",
            "DDCCCCDDDD",
            "EEDDDDEEEE",
            "FFFFFFFFFF",
        ]
    );
    grid.assert_invariants();
}

/// DL with DECLRMM shifts a rectangle: region rows 1-4, margins cols 2-5,
/// cursor row 2. Cols 2-5 of rows 2-4 move up one; the rest is untouched.
#[test]
fn grid_delete_lines_margined_rectangular_shift() {
    let mut grid = filled(6, 10);
    grid.set_scroll_region(1, 4);
    grid.set_horizontal_margins(2, 5);

    grid.set_cursor(2, 3);
    grid.delete_lines_margined(1, true);

    assert_eq!(grid.cursor().col, 2, "cursor moves to the left margin");
    assert_eq!(
        screen(&grid),
        [
            "AAAAAAAAAA",
            "BBBBBBBBBB",
            "CCDDDDCCCC",
            "DDEEEEDDDD",
            "EE    EEEE",
            "FFFFFFFFFF",
        ]
    );
    grid.assert_invariants();
}

/// IL margined with lrmm=false falls back to full-width insert_lines.
#[test]
fn grid_insert_lines_margined_lrmm_off_falls_back() {
    let mut grid = lettered(Grid::new(6, 10));

    grid.set_scroll_region(1, 4);
    grid.set_horizontal_margins(2, 5);
    grid.set_cursor(2, 0);

    grid.insert_lines_margined(1, false);

    assert_eq!(grid.cell(2, 0).unwrap().char(), ' ');
    assert_eq!(grid.cell(3, 0).unwrap().char(), 'C');
    grid.assert_invariants();
}

/// DL margined with full-width margins falls back to full-width delete_lines.
#[test]
fn grid_delete_lines_margined_full_width_falls_back() {
    let mut grid = lettered(Grid::new(6, 10));

    grid.set_scroll_region(1, 4);
    grid.set_horizontal_margins(0, 9);
    grid.set_cursor(2, 0);

    grid.delete_lines_margined(1, true);

    assert_eq!(grid.cell(2, 0).unwrap().char(), 'D');
    assert_eq!(grid.cell(4, 0).unwrap().char(), ' ');
    grid.assert_invariants();
}

/// IL margined with the cursor above the scroll region is a no-op.
#[test]
fn grid_insert_lines_margined_cursor_outside_noop() {
    let mut grid = filled(6, 10);
    let before = screen(&grid);
    grid.set_scroll_region(2, 4);
    grid.set_horizontal_margins(2, 5);

    grid.set_cursor(1, 3);
    grid.insert_lines_margined(1, true);

    assert_eq!(screen(&grid), before);
    grid.assert_invariants();
}

/// Margined IL with a count beyond the region clamps to the region.
#[test]
fn grid_insert_lines_margined_count_exceeds_region() {
    let mut grid = filled(6, 10);
    grid.set_scroll_region(1, 4);
    grid.set_horizontal_margins(2, 5);
    grid.set_cursor(2, 3);

    grid.insert_lines_margined(100, true);

    assert_eq!(
        screen(&grid),
        [
            "AAAAAAAAAA",
            "BBBBBBBBBB",
            "CC    CCCC",
            "DD    DDDD",
            "EE    EEEE",
            "FFFFFFFFFF",
        ]
    );
    grid.assert_invariants();
}
