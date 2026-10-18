// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cursor damage, scroll-down and HAS_EXTRAS flag tests.

use crate::Grid;

#[test]
fn mark_cursor_damage_marks_cursor_cell() {
    let mut grid = Grid::new(24, 80);
    grid.clear_damage(); // Start with no damage

    grid.set_cursor(10, 40);

    assert!(!grid.damage().has_damage());

    grid.mark_cursor_damage();

    assert!(grid.damage().has_damage());
    assert!(!grid.damage().is_full()); // Should be partial, not full

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    assert_eq!(bounds.len(), 1);
    let b = &bounds[0];
    assert_eq!(b.line, 10);
    assert!(b.left <= 40 && b.right > 40);
}

// Boundary regressions

/// scroll_down() shifts content (delegates to scroll_region_down()).
#[test]
fn grid_scroll_down_shifts_content_and_clears_top() {
    let mut grid = Grid::new(5, 10);
    for row in 0..5u16 {
        grid.move_cursor_to(row, 0);
        let ch = (b'A' + row as u8) as char;
        for _ in 0..5 {
            grid.write_char(ch);
        }
    }

    let total_before = grid.total_lines();
    grid.scroll_down(1);

    assert_eq!(
        grid.total_lines(),
        total_before,
        "scroll_down must not alter total_lines"
    );

    let top_char = grid.cell(0, 0).unwrap().char();
    assert!(
        top_char == ' ' || top_char == '\0',
        "After scroll_down, top row should be blank, got '{top_char}'"
    );

    assert_eq!(
        grid.cell(1, 0).unwrap().char(),
        'A',
        "After scroll_down(1), row 1 should have old row 0 content"
    );

    assert_eq!(
        grid.cell(4, 0).unwrap().char(),
        'D',
        "After scroll_down(1), row 4 should have old row 3 content"
    );
}

/// scroll_down(n) with n >= visible_rows clears the entire screen.
#[test]
fn grid_scroll_down_full_screen_clears_all() {
    let mut grid = Grid::new(4, 10);
    for row in 0..4u16 {
        grid.move_cursor_to(row, 0);
        grid.write_char('X');
    }

    grid.scroll_down(4);

    for row in 0..4u16 {
        let ch = grid.cell(row, 0).unwrap().char();
        assert!(
            ch == ' ' || ch == '\0',
            "After scroll_down(visible_rows), row {row} should be blank, got '{ch}'"
        );
    }
}

// HAS_EXTRAS per-cell flag

/// remove_cell_extra atomically clears HAS_EXTRAS and removes the extras entry.
#[test]
fn remove_cell_extra_clears_flag_and_entry() {
    let mut grid = Grid::new(1, 10);
    grid.set_cursor(0, 0);
    grid.write_char('X');

    let extra = grid.cell_extra_mut(0, 0);
    extra.set_fg_rgb(Some([255, 0, 0]));

    assert!(grid.cell(0, 0).unwrap().has_extras());
    assert!(grid.cell_extra(0, 0).is_some());

    let removed = grid.remove_cell_extra(0, 0);
    assert!(removed, "remove should return true when entry existed");
    assert!(
        !grid.cell(0, 0).unwrap().has_extras(),
        "HAS_EXTRAS must be cleared after remove_cell_extra"
    );
    assert!(
        grid.cell_extra(0, 0).is_none(),
        "extras entry must be gone after remove"
    );

    let removed_again = grid.remove_cell_extra(0, 0);
    assert!(!removed_again, "second remove should return false");
}

/// sync_extras_flags_for_row sets HAS_EXTRAS on cells that have extras entries.
#[test]
fn sync_extras_flags_for_row_sets_flags_correctly() {
    let mut grid = Grid::new(1, 10);

    for col in 0..5u16 {
        grid.set_cursor(0, col);
        grid.write_char((b'A' + col as u8) as char);
    }

    // Entries on cols 1 and 3, then every flag cleared, as after a checkpoint load.
    {
        let extra = grid.cell_extra_mut(0, 1);
        extra.set_fg_rgb(Some([255, 0, 0]));
        let extra = grid.cell_extra_mut(0, 3);
        extra.set_fg_rgb(Some([0, 255, 0]));
    }

    for col in 0..10u16 {
        if let Some(row) = grid.row_mut(0)
            && let Some(cell) = row.get_mut(col)
        {
            cell.set_has_extras(false);
        }
    }

    assert!(!grid.cell(0, 1).unwrap().has_extras());
    assert!(!grid.cell(0, 3).unwrap().has_extras());

    grid.sync_extras_flags_for_row(0, 10);

    assert!(
        grid.cell(0, 1).unwrap().has_extras(),
        "col 1 has extras entry, flag should be set"
    );
    assert!(
        grid.cell(0, 3).unwrap().has_extras(),
        "col 3 has extras entry, flag should be set"
    );

    assert!(
        !grid.cell(0, 0).unwrap().has_extras(),
        "col 0 has no extras, flag should remain false"
    );
    assert!(
        !grid.cell(0, 2).unwrap().has_extras(),
        "col 2 has no extras, flag should remain false"
    );
    assert!(
        !grid.cell(0, 4).unwrap().has_extras(),
        "col 4 has no extras, flag should remain false"
    );
}

/// sync_extras_flags_for_row clears a HAS_EXTRAS flag that has no extras entry.
#[test]
fn sync_extras_flags_for_row_clears_stale_flags() {
    let mut grid = Grid::new(1, 10);

    for col in 0..5u16 {
        grid.set_cursor(0, col);
        grid.write_char((b'A' + col as u8) as char);
    }
    let extra = grid.cell_extra_mut(0, 2);
    extra.set_fg_rgb(Some([255, 0, 0]));

    // HAS_EXTRAS without an entry on cols 0, 1 and 4.
    for col in [0u16, 1, 4] {
        if let Some(row) = grid.row_mut(0)
            && let Some(cell) = row.get_mut(col)
        {
            cell.set_has_extras(true);
        }
    }

    assert!(
        grid.cell(0, 0).unwrap().has_extras(),
        "precondition: col 0 stale flag"
    );
    assert!(
        grid.cell(0, 1).unwrap().has_extras(),
        "precondition: col 1 stale flag"
    );
    assert!(
        grid.cell(0, 2).unwrap().has_extras(),
        "precondition: col 2 real extras"
    );
    assert!(
        grid.cell(0, 4).unwrap().has_extras(),
        "precondition: col 4 stale flag"
    );

    grid.sync_extras_flags_for_row(0, 10);

    assert!(
        !grid.cell(0, 0).unwrap().has_extras(),
        "col 0 has no extras entry, flag should be cleared"
    );
    assert!(
        !grid.cell(0, 1).unwrap().has_extras(),
        "col 1 has no extras entry, flag should be cleared"
    );
    assert!(
        grid.cell(0, 2).unwrap().has_extras(),
        "col 2 has real extras entry, flag should remain set"
    );
    assert!(
        !grid.cell(0, 4).unwrap().has_extras(),
        "col 4 has no extras entry, flag should be cleared"
    );
}

/// Erase operations clear HAS_EXTRAS by resetting cells to empty.
#[test]
fn erase_to_end_of_screen_clears_has_extras() {
    let mut grid = Grid::new(3, 10);

    for row in 0..3u16 {
        grid.set_cursor(row, 0);
        grid.write_char('X');
        let extra = grid.cell_extra_mut(row, 0);
        extra.set_fg_rgb(Some([128, 128, 128]));
    }

    for row in 0..3u16 {
        assert!(grid.cell(row, 0).unwrap().has_extras());
    }

    grid.set_cursor(1, 0);
    grid.erase_to_end_of_screen();

    assert!(
        grid.cell(0, 0).unwrap().has_extras(),
        "row 0 above cursor should keep has_extras"
    );

    for row in 1..3u16 {
        assert!(
            !grid.cell(row, 0).unwrap().has_extras(),
            "row {row} below cursor should have has_extras cleared"
        );
    }
}
