// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{Cell, CellFlags, Grid, PackedColors};

#[test]
fn wide_wrap_tail_paints_the_bisected_orphan_with_the_bce_blank_and_its_ring() {
    let (rows, cols) = (3u16, 10u16);
    let mut grid = Grid::new(rows, cols);

    let old_bg = [10u8, 20, 30];
    let old_colors = PackedColors::new().with_rgb_bg();
    grid.set_cursor_template(Cell::bce_blank(old_colors), Some(old_bg));
    grid.move_cursor_to(0, 8);
    assert!(grid.write_wide_autowrap_fast('\u{4E2D}', old_colors, CellFlags::empty()));
    grid.extras_mut()
        .set_rgb_ring_range(0, 8, 10, None, Some(old_bg), rows, cols);
    assert!(
        grid.cell(0, 8).unwrap().flags().contains(CellFlags::WIDE),
        "precondition: col 8 is the wide lead"
    );
    assert_eq!(
        grid.bg_rgb_at(0, 8),
        Some(old_bg),
        "precondition: the ring holds the first glyph's bytes"
    );

    let new_bg = [200u8, 100, 50];
    let new_colors = PackedColors::new().with_rgb_bg();
    grid.set_cursor_template(Cell::bce_blank(new_colors), Some(new_bg));
    grid.move_cursor_to(0, 9);
    assert!(grid.write_wide_autowrap_fast('\u{4E2D}', new_colors, CellFlags::empty()));

    assert!(
        grid.cell(1, 0).unwrap().flags().contains(CellFlags::WIDE),
        "precondition: the second glyph wrapped to row 1"
    );

    let fill = Cell::bce_blank(new_colors);
    let orphan = grid.cell(0, 8).unwrap();
    assert!(
        !orphan.flags().contains(CellFlags::WIDE),
        "the orphaned wide head must be blanked"
    );
    assert_eq!(orphan.char(), ' ', "the orphan becomes a space");
    assert_eq!(
        orphan.colors(),
        fill.colors(),
        "the orphan carries the BCE blank, as xterm's ClearCells writes it"
    );
    assert_eq!(
        grid.bg_rgb_at(0, 8),
        Some(new_bg),
        "the orphan resolves the new truecolor background from the ring"
    );
    assert_eq!(
        grid.cell(0, 9).unwrap().colors(),
        fill.colors(),
        "the blanked tail itself carries the BCE blank"
    );
    assert_eq!(
        grid.bg_rgb_at(0, 9),
        Some(new_bg),
        "the blanked tail resolves the new background from the ring"
    );
}
