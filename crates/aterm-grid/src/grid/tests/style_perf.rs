// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cursor damage and HAS_EXTRAS flag tests.

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
