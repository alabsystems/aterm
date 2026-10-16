// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Style-id writes, ring/tiered row access, cursor damage, boundary regressions and the
//! HAS_EXTRAS cell flag.

use crate::grid::Scrollback;
use crate::{CellCoord, CellFlags, ExtendedStyle, Grid, PackedColor, StyleId};

// Style API tests

#[test]
fn grid_write_char_with_style_id_default() {
    let mut grid = Grid::new(24, 80);
    grid.write_char_with_style_id('A', StyleId::DEFAULT, CellFlags::empty());

    let cell = grid.row(0).unwrap().get(0).unwrap();
    assert_eq!(cell.char(), 'A');
    assert!(cell.colors().is_default());
}

#[test]
fn grid_write_char_with_style_id_indexed_color() {
    let mut grid = Grid::new(24, 80);

    let ext = ExtendedStyle::from_packed_colors_separate(
        PackedColor::indexed(196), // Red
        PackedColor::indexed(21),  // Blue
        CellFlags::BOLD,
    );
    let style_id = grid.intern_extended_style(ext);

    grid.write_char_with_style_id('B', style_id, CellFlags::empty());

    let cell = grid.row(0).unwrap().get(0).unwrap();
    assert_eq!(cell.char(), 'B');
    assert!(cell.colors().fg_is_indexed());
    assert_eq!(cell.colors().fg_index(), 196);
    assert!(cell.colors().bg_is_indexed());
    assert_eq!(cell.colors().bg_index(), 21);
    assert!(cell.flags().contains(CellFlags::BOLD));
}

#[test]
fn grid_write_char_with_style_id_rgb_color() {
    let mut grid = Grid::new(24, 80);

    let ext = ExtendedStyle::from_packed_colors_separate(
        PackedColor::rgb(255, 128, 64),
        PackedColor::rgb(32, 64, 128),
        CellFlags::ITALIC,
    );
    let style_id = grid.intern_extended_style(ext);

    grid.write_char_with_style_id('C', style_id, CellFlags::empty());

    let cell = grid.row(0).unwrap().get(0).unwrap();
    assert_eq!(cell.char(), 'C');
    assert!(cell.flags().contains(CellFlags::ITALIC));
    // RGB colors are marked as needing overflow
    assert!(cell.fg_needs_overflow());
    assert!(cell.bg_needs_overflow());
}

#[test]
fn grid_write_char_with_style_id_extra_flags() {
    let mut grid = Grid::new(24, 80);

    let ext = ExtendedStyle::from_packed_colors_separate(
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::BOLD,
    );
    let style_id = grid.intern_extended_style(ext);

    grid.write_char_with_style_id('D', style_id, CellFlags::PROTECTED);

    let cell = grid.row(0).unwrap().get(0).unwrap();
    assert_eq!(cell.char(), 'D');
    assert!(cell.flags().contains(CellFlags::BOLD));
    assert!(cell.flags().contains(CellFlags::PROTECTED));
}

#[test]
fn grid_style_id_deduplication_in_writes() {
    let mut grid = Grid::new(24, 80);

    let ext1 = ExtendedStyle::from_packed_colors_separate(
        PackedColor::indexed(100),
        PackedColor::DEFAULT_BG,
        CellFlags::BOLD,
    );
    let ext2 = ExtendedStyle::from_packed_colors_separate(
        PackedColor::indexed(100),
        PackedColor::DEFAULT_BG,
        CellFlags::BOLD,
    );

    let id1 = grid.intern_extended_style(ext1);
    let id2 = grid.intern_extended_style(ext2);

    assert_eq!(id1, id2, "Same style should produce same ID");

    grid.write_char_with_style_id('E', id1, CellFlags::empty());
    grid.write_char_with_style_id('F', id2, CellFlags::empty());

    let cell0 = grid.row(0).unwrap().get(0).unwrap();
    let cell1 = grid.row(0).unwrap().get(1).unwrap();

    assert_eq!(cell0.colors(), cell1.colors());
    assert_eq!(cell0.flags(), cell1.flags());
}

/// Scrolling into tiered scrollback makes `row()` return None rather than
/// underflowing `row_index()`.
#[test]
fn row_returns_none_when_scrolled_into_tiered_scrollback() {
    // A small ring and hot tier, so 15 lines reach the tiered store.
    let scrollback = Scrollback::new(5, 100, 1_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 2, scrollback);

    for i in 0..15 {
        for c in format!("Line{i:02}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let total_scrollback = grid.scrollback_lines();
    let ring_scrollback = grid
        .storage
        .total_lines
        .saturating_sub(grid.storage.visible_rows as usize);
    let tiered_scrollback = total_scrollback.saturating_sub(ring_scrollback);

    assert!(
        tiered_scrollback > 0,
        "Test requires tiered scrollback content"
    );

    grid.scroll_to_top();
    assert_eq!(grid.display_offset(), total_scrollback);

    // With display_offset > ring_scrollback + visible_row, the row is in tiered
    // scrollback and row() returns None.
    let row_result = grid.row(0);

    assert!(
        row_result.is_none(),
        "row(0) should be None when scrolled into tiered scrollback (display_offset={}, ring={}, tiered={})",
        grid.display_offset(),
        ring_scrollback,
        tiered_scrollback
    );

    grid.scroll_to_bottom();
    assert_eq!(grid.display_offset(), 0);

    for visible_row in 0..grid.rows() {
        assert!(
            grid.row(visible_row).is_some(),
            "row({visible_row}) should be Some at live position"
        );
    }
}

/// Partial scroll at the exact boundary between ring and tiered scrollback.
#[test]
fn row_access_at_ring_tiered_boundary() {
    let scrollback = Scrollback::new(5, 100, 1_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 2, scrollback);

    for i in 0..12 {
        for c in format!("Row{i:02}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    assert!(
        grid.storage.total_lines > grid.storage.visible_rows as usize,
        "12 line feeds in a 3-row grid should produce scrollback: total_lines={}, visible_rows={}",
        grid.storage.total_lines,
        grid.storage.visible_rows
    );

    let ring_scrollback = grid
        .storage
        .total_lines
        .saturating_sub(grid.storage.visible_rows as usize);

    grid.scroll_display(ring_scrollback as i32);

    let mut accessible_count = 0u16;
    for visible_row in 0..grid.rows() {
        if grid.row(visible_row).is_some() {
            accessible_count += 1;
        }
    }
    assert!(
        accessible_count > 0,
        "at ring boundary, at least one visible row should be accessible (got 0 of {})",
        grid.rows()
    );

    grid.scroll_display(1);

    // Row 0 at this position comes from tiered scrollback: either accessible
    // (Some) or evicted (None), but must not panic
    let row = grid.row(0);
    assert!(
        grid.display_offset() > 0,
        "after scrolling past ring boundary, display_offset should be positive"
    );
    if let Some(r) = row {
        assert_eq!(
            r.len(),
            grid.cols(),
            "accessible row width should match grid columns"
        );
    }
}

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
fn mark_cursor_damage_after_cursor_move() {
    let mut grid = Grid::new(24, 80);

    grid.set_cursor(5, 10);
    grid.clear_damage();
    grid.mark_cursor_damage();

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    assert_eq!(bounds.len(), 1);
    assert_eq!(bounds[0].line, 5);

    grid.set_cursor(15, 30);
    grid.clear_damage();
    grid.mark_cursor_damage();

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    assert_eq!(bounds.len(), 1);
    assert_eq!(bounds[0].line, 15);
}
// Boundary regressions

/// A 1-row terminal's scroll region has top == bottom == 0, which
/// ScrollRegionValid allows.
#[test]
fn grid_one_row_invariants_valid() {
    let grid = Grid::new(1, 80);
    assert_eq!(grid.scroll_region().top, 0);
    assert_eq!(grid.scroll_region().bottom, 0);
    grid.assert_invariants();
}

/// scroll_region.bottom is < visible_rows (0-indexed).
#[test]
fn grid_scroll_region_bottom_strictly_less_than_visible_rows() {
    let grid = Grid::new(24, 80);
    assert!(grid.scroll_region().bottom < grid.rows());
    grid.assert_invariants();
}

/// scroll_region_down() shifts content and clears the top rows.
#[test]
fn grid_scroll_region_down_clears_top_rows() {
    let mut grid = Grid::new(5, 10);
    for row in 0..5u16 {
        grid.move_cursor_to(row, 0);
        let ch = (b'A' + row as u8) as char;
        for _ in 0..5 {
            grid.write_char(ch);
        }
    }
    grid.scroll_region_down(1);
    let first_cell_char = grid.cell(0, 0).unwrap().char();
    assert!(
        first_cell_char == ' ' || first_cell_char == '\0',
        "After scroll_region_down, top row should be blank, got '{first_cell_char}'"
    );
    assert_eq!(
        grid.cell(1, 0).unwrap().char(),
        'A',
        "After scroll_region_down, row 1 should have old row 0's content"
    );
}

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

/// A coloured prompt, then a plain body: has_extras() is true only on the
/// prompt cells, which have extras entries.
#[test]
fn colored_prompt_has_extras_only_on_prompt_cells() {
    let mut grid = Grid::new(1, 80);

    let prompt = "user@host:~$ ";
    grid.set_cursor(0, 0);
    for c in prompt.chars() {
        grid.write_char(c);
    }
    let prompt_len = prompt.len() as u16;

    // `cell_extra_mut` sets HAS_EXTRAS.
    for col in 0..prompt_len {
        let extra = grid.cell_extra_mut(0, col);
        extra.set_fg_rgb(Some([0, 255, 0])); // green prompt
    }

    grid.set_cursor(0, prompt_len);
    for c in "ls -la /tmp".chars() {
        grid.write_char(c);
    }

    assert!(!grid.extras().is_empty());

    for col in 0..prompt_len {
        assert!(
            grid.cell(0, col).unwrap().has_extras(),
            "prompt cell at col {col} should have has_extras flag"
        );
        assert!(
            grid.cell_extra(0, col).is_some(),
            "prompt cell at col {col} should have extras entry"
        );
    }

    let body_end = prompt_len + 11; // "ls -la /tmp" is 11 chars
    for col in prompt_len..body_end {
        assert!(
            !grid.cell(0, col).unwrap().has_extras(),
            "body cell at col {col} should NOT have has_extras flag"
        );
    }

    for col in body_end..80 {
        assert!(
            !grid.cell(0, col).unwrap().has_extras(),
            "empty cell at col {col} should NOT have has_extras flag"
        );
    }
}

/// Clearing a hyperlink on a cell clears HAS_EXTRAS when no other extras remain.
#[test]
fn clear_hyperlink_clears_has_extras_when_empty() {
    use std::sync::Arc;

    let mut grid = Grid::new(1, 10);
    grid.set_cursor(0, 0);
    grid.write_char('A');

    let extra = grid.cell_extra_mut(0, 0);
    extra.set_hyperlink(Some(Arc::from("https://example.com")));

    assert!(grid.cell(0, 0).unwrap().has_extras());
    assert!(grid.cell_extra(0, 0).is_some());

    // Clear the hyperlink by setting an empty CellExtra.
    let coord = CellCoord::new(0, 0);
    if let Some(existing) = grid.extras_mut().get(coord).cloned() {
        let mut cleared = existing;
        cleared.set_hyperlink(None);
        grid.extras_mut().set(coord, cleared);
    }

    // After clearing, the extras entry should be auto-removed (has_data() == false).
    assert!(
        grid.extras().get(coord).is_none(),
        "empty extras entry should be auto-removed by set()"
    );

    // Manually sync the HAS_EXTRAS flag (caller's responsibility).
    let has_entry = grid.extras().get(coord).is_some();
    if let Some(row) = grid.row_mut(0)
        && let Some(cell) = row.get_mut(0)
    {
        cell.set_has_extras(has_entry);
    }

    assert!(
        !grid.cell(0, 0).unwrap().has_extras(),
        "has_extras should be false after extras entry removed"
    );
}

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

/// remove_cell_extra preserves cell content and colors.
#[test]
fn remove_cell_extra_preserves_cell_content() {
    let mut grid = Grid::new(1, 10);
    grid.set_cursor(0, 0);
    grid.write_char('Q');

    let orig_fg = grid.cell(0, 0).unwrap().colors().fg_index();

    let extra = grid.cell_extra_mut(0, 0);
    extra.set_fg_rgb(Some([0, 128, 255]));
    grid.remove_cell_extra(0, 0);

    let cell = grid.cell(0, 0).unwrap();
    assert_eq!(cell.char(), 'Q', "char must survive remove");
    assert_eq!(
        cell.colors().fg_index(),
        orig_fg,
        "fg_index must survive remove"
    );
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
