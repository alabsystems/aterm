// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Style API, scrollback boundary, damage tracking, and targeted regression tests.

use crate::grid::Scrollback;
use crate::{
    CellCoord, CellFlags, Color, ExtendedStyle, Grid, PackedColor, PackedColors, Style, StyleAttrs,
    StyleId,
};

// -------------------------------------------------------------------------
// Style API tests
// -------------------------------------------------------------------------

#[test]
fn grid_style_table_initialized() {
    let grid = Grid::new(24, 80);
    // Grid should have a style table with the default style
    // Note: StyleTable::is_empty() returns true when only default style exists,
    // so we check that get_style returns the default style
    let default_style = grid
        .get_style(StyleId::DEFAULT)
        .expect("grid should have default style at initialization");
    assert_eq!(
        *default_style,
        Style::DEFAULT,
        "default style should be canonical"
    );
}

#[test]
fn grid_intern_style_returns_id() {
    let mut grid = Grid::new(24, 80);
    let style = Style::new(Color::new(255, 0, 0), Color::DEFAULT_BG, StyleAttrs::BOLD);
    let id = grid.intern_style(style);
    // Should get a non-default ID for non-default style
    assert!(!id.is_default());
    // Should be able to retrieve it
    let retrieved = grid.get_style(id).unwrap();
    assert_eq!(*retrieved, style);
}

#[test]
fn grid_intern_same_style_returns_same_id() {
    let mut grid = Grid::new(24, 80);
    let style = Style::new(Color::new(0, 255, 0), Color::DEFAULT_BG, StyleAttrs::ITALIC);
    let id1 = grid.intern_style(style);
    let id2 = grid.intern_style(style);
    assert_eq!(id1, id2);
}

#[test]
fn grid_intern_default_style() {
    let mut grid = Grid::new(24, 80);
    let id = grid.intern_style(Style::DEFAULT);
    assert!(id.is_default());
}

#[test]
fn grid_style_stats() {
    let mut grid = Grid::new(24, 80);
    let initial_stats = grid.style_stats();
    assert_eq!(initial_stats.total_styles, 1); // Just default

    // Add some styles
    grid.intern_style(Style::new(
        Color::new(255, 0, 0),
        Color::DEFAULT_BG,
        StyleAttrs::BOLD,
    ));
    grid.intern_style(Style::new(
        Color::new(0, 255, 0),
        Color::DEFAULT_BG,
        StyleAttrs::ITALIC,
    ));

    let stats = grid.style_stats();
    assert_eq!(stats.total_styles, 3); // default + 2 new
    assert!(stats.memory_bytes > 0);
}

#[test]
fn grid_clear_styles() {
    let mut grid = Grid::new(24, 80);
    grid.intern_style(Style::new(
        Color::new(255, 0, 0),
        Color::DEFAULT_BG,
        StyleAttrs::empty(),
    ));
    grid.intern_style(Style::new(
        Color::new(0, 255, 0),
        Color::DEFAULT_BG,
        StyleAttrs::empty(),
    ));
    assert_eq!(grid.style_stats().total_styles, 3);

    grid.clear_styles();
    assert_eq!(grid.style_stats().total_styles, 1); // Only default remains
}

#[test]
fn grid_intern_extended_style() {
    let mut grid = Grid::new(24, 80);
    let colors = PackedColors::with_indexed(196, 21);
    let flags = CellFlags::BOLD.union(CellFlags::UNDERLINE);
    let ext = ExtendedStyle::from_cell_style(colors, flags, None, None);
    let id = grid.intern_extended_style(ext);

    assert!(!id.is_default());
    let style = grid.get_style(id).unwrap();
    assert!(style.attrs.contains(StyleAttrs::BOLD));
    assert!(style.attrs.contains(StyleAttrs::UNDERLINE));
}

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

    // Intern a style with indexed colors
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

    // Intern a style with RGB colors
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

    // Intern a bold style
    let ext = ExtendedStyle::from_packed_colors_separate(
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::BOLD,
    );
    let style_id = grid.intern_extended_style(ext);

    // Write with extra PROTECTED flag
    grid.write_char_with_style_id('D', style_id, CellFlags::PROTECTED);

    let cell = grid.row(0).unwrap().get(0).unwrap();
    assert_eq!(cell.char(), 'D');
    assert!(cell.flags().contains(CellFlags::BOLD));
    assert!(cell.flags().contains(CellFlags::PROTECTED));
}

#[test]
fn grid_write_char_wrap_with_style_id() {
    let mut grid = Grid::new(24, 10); // 10 columns

    let ext = ExtendedStyle::from_packed_colors_separate(
        PackedColor::indexed(31),
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    let style_id = grid.intern_extended_style(ext);

    // Fill first row and wrap to second
    for _ in 0..12 {
        grid.write_char_wrap_with_style_id('X', style_id, CellFlags::empty());
    }

    // First row should be full (10 chars)
    let row0 = grid.row(0).unwrap();
    assert_eq!(row0.len(), 10);

    // Second row should have 2 chars
    let row1 = grid.row(1).unwrap();
    assert_eq!(row1.len(), 2);
    assert!(row1.is_wrapped());
}

#[test]
fn grid_write_wide_char_with_style_id() {
    let mut grid = Grid::new(24, 80);

    let ext = ExtendedStyle::from_packed_colors_separate(
        PackedColor::indexed(196),
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    let style_id = grid.intern_extended_style(ext);

    // Write a wide CJK character
    let ok = grid.write_wide_char_with_style_id('あ', style_id, CellFlags::empty());
    assert!(ok);

    // Check the main cell
    let cell0 = grid.row(0).unwrap().get(0).unwrap();
    assert_eq!(cell0.char(), 'あ');
    assert!(cell0.is_wide());
    assert!(cell0.colors().fg_is_indexed());

    // Check the continuation cell
    let cell1 = grid.row(0).unwrap().get(1).unwrap();
    assert!(cell1.is_wide_continuation());
}

#[test]
fn grid_write_wide_char_wrap_with_style_id() {
    let mut grid = Grid::new(24, 10);

    let ext = ExtendedStyle::from_packed_colors_separate(
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::UNDERLINE,
    );
    let style_id = grid.intern_extended_style(ext);

    // Position cursor at column 9 (last column)
    grid.set_cursor(0, 9);

    // Write wide char - should wrap to next line
    let ok = grid.write_wide_char_wrap_with_style_id('中', style_id, CellFlags::empty());
    assert!(ok);

    // Character should be on row 1 (wrapped)
    let row1 = grid.row(1).unwrap();
    assert_eq!(row1.get(0).unwrap().char(), '中');
    assert!(row1.is_wrapped());
}

#[test]
fn grid_style_id_deduplication_in_writes() {
    let mut grid = Grid::new(24, 80);

    // Same style interned twice should give same ID
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

    // Write with both IDs
    grid.write_char_with_style_id('E', id1, CellFlags::empty());
    grid.write_char_with_style_id('F', id2, CellFlags::empty());

    // Both cells should have identical styles
    let cell0 = grid.row(0).unwrap().get(0).unwrap();
    let cell1 = grid.row(0).unwrap().get(1).unwrap();

    assert_eq!(cell0.colors(), cell1.colors());
    assert_eq!(cell0.flags(), cell1.flags());
}

/// Test that scrolling into tiered scrollback returns None gracefully.
///
/// Regression test for #292: When display_offset exceeds the ring buffer
/// portion of scrollback, `row_index()` previously underflowed, causing
/// wrong cell data to be returned (manifested as character substitution).
///
/// Fixed in 67b2b1f: `row_index()` now returns `Option<usize>` and checks
/// for underflow before subtraction.
#[test]
fn row_returns_none_when_scrolled_into_tiered_scrollback() {
    // Setup: small ring buffer (2 lines), small hot tier (5 lines)
    // This ensures content quickly overflows into tiered scrollback
    let scrollback = Scrollback::new(5, 100, 1_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 2, scrollback);

    // Fill content: 3 visible + 2 ring buffer + overflow into tiered
    // Write 15 lines to ensure some go into tiered scrollback
    for i in 0..15 {
        for c in format!("Line{i:02}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    // Verify we have tiered scrollback content
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

    // Scroll to top of all scrollback (into tiered territory)
    grid.scroll_to_top();
    assert_eq!(grid.display_offset(), total_scrollback);

    // Key assertion: row() should return None for rows in tiered scrollback
    // (instead of underflowing and returning garbage data)
    //
    // When display_offset > ring_scrollback + visible_row, the row is in
    // tiered scrollback and row_index() should return None.
    let row_result = grid.row(0);

    // At this scroll position, row 0 is in tiered scrollback (not ring buffer)
    // so it should return None, NOT panic or return wrong data
    assert!(
        row_result.is_none(),
        "row(0) should be None when scrolled into tiered scrollback (display_offset={}, ring={}, tiered={})",
        grid.display_offset(),
        ring_scrollback,
        tiered_scrollback
    );

    // Scroll back to live position - rows should be accessible again
    grid.scroll_to_bottom();
    assert_eq!(grid.display_offset(), 0);

    // All visible rows should now be Some
    for visible_row in 0..grid.rows() {
        assert!(
            grid.row(visible_row).is_some(),
            "row({visible_row}) should be Some at live position"
        );
    }
}

/// Test partial scroll into tiered scrollback boundary.
///
/// Verifies behavior at the exact boundary between ring buffer and tiered
/// scrollback storage.
#[test]
fn row_access_at_ring_tiered_boundary() {
    let scrollback = Scrollback::new(5, 100, 1_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 2, scrollback);

    // Fill enough to have both ring buffer and tiered content
    for i in 0..12 {
        for c in format!("Row{i:02}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    // After 12 line feeds in a 3-row grid, total_lines should exceed visible rows
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

    // Scroll to exactly the ring buffer boundary
    grid.scroll_display(ring_scrollback as i32);

    // At this position, at least one visible row should be accessible (Some)
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

    // One more scroll should put us into tiered territory
    grid.scroll_display(1);

    // Row 0 at this position comes from tiered scrollback: either accessible
    // (Some) or evicted (None), but must not panic
    let row = grid.row(0);
    // Whether Some or None, the display_offset must have advanced
    assert!(
        grid.display_offset() > 0,
        "after scrolling past ring boundary, display_offset should be positive"
    );
    // If the row is accessible, it should have non-zero length (grid width)
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

    // Move cursor to a specific position
    grid.set_cursor(10, 40);

    // No damage after clearing
    assert!(!grid.damage().has_damage());

    // Mark cursor damage
    grid.mark_cursor_damage();

    // Should have damage now
    assert!(grid.damage().has_damage());
    assert!(!grid.damage().is_full()); // Should be partial, not full

    // Verify the cursor cell is in the damaged region
    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    assert_eq!(bounds.len(), 1);
    let b = &bounds[0];
    assert_eq!(b.line, 10);
    assert!(b.left <= 40 && b.right > 40);
}

#[test]
fn mark_cursor_damage_after_cursor_move() {
    let mut grid = Grid::new(24, 80);

    // Move cursor and mark damage
    grid.set_cursor(5, 10);
    grid.clear_damage();
    grid.mark_cursor_damage();

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    assert_eq!(bounds.len(), 1);
    assert_eq!(bounds[0].line, 5);

    // Move cursor to new position and mark damage
    grid.set_cursor(15, 30);
    grid.clear_damage();
    grid.mark_cursor_damage();

    let bounds: Vec<_> = grid.damage().iter_bounds(24, 80).collect();
    assert_eq!(bounds.len(), 1);
    assert_eq!(bounds[0].line, 15);
}
// ========== Regression tests for algorithm audit findings ==========

/// Regression: 1-row terminal has scroll region top == bottom (both 0).
/// The TLA+ ScrollRegionValid invariant must allow top == bottom for single-row grids.
/// Bug: invariants.rs used strict `<` instead of `<=` for top < bottom check.
#[test]
fn grid_one_row_invariants_valid() {
    let grid = Grid::new(1, 80);
    assert_eq!(grid.scroll_region().top, 0);
    assert_eq!(grid.scroll_region().bottom, 0);
    // This must not panic:
    grid.assert_invariants();
}

/// Regression: scroll_region.bottom should be < visible_rows (0-indexed),
/// not <= visible_rows. The invariant was too permissive.
#[test]
fn grid_scroll_region_bottom_strictly_less_than_visible_rows() {
    let grid = Grid::new(24, 80);
    // Default scroll region: top=0, bottom=23, visible_rows=24
    // bottom (23) < visible_rows (24) — this is correct
    assert!(grid.scroll_region().bottom < grid.rows());
    grid.assert_invariants();
}

/// Regression: scroll_down() decrements total_lines but does not actually shift
/// row content or clear new rows at the top. Verify that scroll_region_down()
/// (the correct implementation) actually clears the top rows.
#[test]
fn grid_scroll_region_down_clears_top_rows() {
    let mut grid = Grid::new(5, 10);
    // Write identifiable content on each row
    for row in 0..5u16 {
        grid.move_cursor_to(row, 0);
        let ch = (b'A' + row as u8) as char;
        for _ in 0..5 {
            grid.write_char(ch);
        }
    }
    // Scroll region down by 1 (full screen region)
    grid.scroll_region_down(1);
    // Top row (row 0) should now be blank
    let first_cell_char = grid.cell(0, 0).unwrap().char();
    assert!(
        first_cell_char == ' ' || first_cell_char == '\0',
        "After scroll_region_down, top row should be blank, got '{first_cell_char}'"
    );
    // Row 1 should contain what was previously row 0 (A's)
    assert_eq!(
        grid.cell(1, 0).unwrap().char(),
        'A',
        "After scroll_region_down, row 1 should have old row 0's content"
    );
}

/// Regression (#1966): scroll_down() was a stub that only decremented total_lines
/// without shifting content. Now it delegates to scroll_region_down().
#[test]
fn grid_scroll_down_shifts_content_and_clears_top() {
    let mut grid = Grid::new(5, 10);
    // Write identifiable content: row 0='A', row 1='B', ..., row 4='E'
    for row in 0..5u16 {
        grid.move_cursor_to(row, 0);
        let ch = (b'A' + row as u8) as char;
        for _ in 0..5 {
            grid.write_char(ch);
        }
    }

    let total_before = grid.total_lines();
    grid.scroll_down(1);

    // total_lines must not change (no rows added or removed)
    assert_eq!(
        grid.total_lines(),
        total_before,
        "scroll_down must not alter total_lines"
    );

    // Top row should be blank
    let top_char = grid.cell(0, 0).unwrap().char();
    assert!(
        top_char == ' ' || top_char == '\0',
        "After scroll_down, top row should be blank, got '{top_char}'"
    );

    // Content shifted: old row 0 ('A') is now at row 1
    assert_eq!(
        grid.cell(1, 0).unwrap().char(),
        'A',
        "After scroll_down(1), row 1 should have old row 0 content"
    );

    // Old row 3 ('D') is now at row 4
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

    // Every row should be blank
    for row in 0..4u16 {
        let ch = grid.cell(row, 0).unwrap().char();
        assert!(
            ch == ' ' || ch == '\0',
            "After scroll_down(visible_rows), row {row} should be blank, got '{ch}'"
        );
    }
}

// -------------------------------------------------------------------------
// HAS_EXTRAS per-cell flag tests (#5551)
// -------------------------------------------------------------------------

/// Colored prompt (16 RGB-colored cells) + plain body (64 cells) scenario.
///
/// Verifies that has_extras() is true ONLY for cells that have extras entries
/// (the colored prompt), and false for plain body cells. This validates the
/// per-cell HAS_EXTRAS flag that eliminates 99% of hash probes in rendering.
#[test]
fn colored_prompt_has_extras_only_on_prompt_cells() {
    let mut grid = Grid::new(1, 80);

    // Write a colored prompt "user@host:~$ " (14 chars) with RGB extras.
    let prompt = "user@host:~$ ";
    grid.set_cursor(0, 0);
    for c in prompt.chars() {
        grid.write_char(c);
    }
    let prompt_len = prompt.len() as u16;

    // Add RGB foreground to each prompt cell via cell_extra_mut (sets HAS_EXTRAS).
    for col in 0..prompt_len {
        let extra = grid.cell_extra_mut(0, col);
        extra.set_fg_rgb(Some([0, 255, 0])); // green prompt
    }

    // Write plain body text after the prompt.
    grid.set_cursor(0, prompt_len);
    for c in "ls -la /tmp".chars() {
        grid.write_char(c);
    }

    // Extras map is NOT empty (prompt has entries).
    assert!(!grid.extras().is_empty());

    // Prompt cells have has_extras() == true.
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

    // Plain body cells have has_extras() == false.
    let body_end = prompt_len + 11; // "ls -la /tmp" is 11 chars
    for col in prompt_len..body_end {
        assert!(
            !grid.cell(0, col).unwrap().has_extras(),
            "body cell at col {col} should NOT have has_extras flag"
        );
    }

    // Untouched cells also have has_extras() == false.
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

    // Add a hyperlink to cell (0, 0).
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

    // Add extras
    let extra = grid.cell_extra_mut(0, 0);
    extra.set_fg_rgb(Some([255, 0, 0]));

    assert!(grid.cell(0, 0).unwrap().has_extras());
    assert!(grid.cell_extra(0, 0).is_some());

    // Remove extras atomically
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

    // Removing again should return false
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

    // Add and then remove extras
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

    // Write some chars
    for col in 0..5u16 {
        grid.set_cursor(0, col);
        grid.write_char((b'A' + col as u8) as char);
    }

    // Manually insert extras for cols 1, 3 without setting HAS_EXTRAS
    // (simulates checkpoint restore where flags are not yet synced)
    {
        let extra = grid.cell_extra_mut(0, 1);
        extra.set_fg_rgb(Some([255, 0, 0]));
        let extra = grid.cell_extra_mut(0, 3);
        extra.set_fg_rgb(Some([0, 255, 0]));
    }

    // Clear flags to simulate a stale state (like checkpoint load)
    for col in 0..10u16 {
        if let Some(row) = grid.row_mut(0)
            && let Some(cell) = row.get_mut(col)
        {
            cell.set_has_extras(false);
        }
    }

    // Verify flags are cleared
    assert!(!grid.cell(0, 1).unwrap().has_extras());
    assert!(!grid.cell(0, 3).unwrap().has_extras());

    // Now sync
    grid.sync_extras_flags_for_row(0, 10);

    // Cells with extras should now have the flag
    assert!(
        grid.cell(0, 1).unwrap().has_extras(),
        "col 1 has extras entry, flag should be set"
    );
    assert!(
        grid.cell(0, 3).unwrap().has_extras(),
        "col 3 has extras entry, flag should be set"
    );

    // Cells without extras should still not have the flag
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

/// sync_extras_flags_for_row clears stale HAS_EXTRAS when no extras entry exists.
///
/// Covers the true-to-false direction: a cell has HAS_EXTRAS set but no
/// corresponding extras map entry (e.g., after extras were removed by a
/// different code path that forgot to clear the flag).
#[test]
fn sync_extras_flags_for_row_clears_stale_flags() {
    let mut grid = Grid::new(1, 10);

    // Write chars and add extras to col 2
    for col in 0..5u16 {
        grid.set_cursor(0, col);
        grid.write_char((b'A' + col as u8) as char);
    }
    let extra = grid.cell_extra_mut(0, 2);
    extra.set_fg_rgb(Some([255, 0, 0]));

    // Manually set HAS_EXTRAS on cols 0, 1, 4 (no actual extras entries)
    // to simulate stale flags from a buggy code path.
    for col in [0u16, 1, 4] {
        if let Some(row) = grid.row_mut(0)
            && let Some(cell) = row.get_mut(col)
        {
            cell.set_has_extras(true);
        }
    }

    // Verify stale flags are set
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

    // Sync should clear stale flags and keep real ones
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

/// Erase operations clear HAS_EXTRAS by resetting cells to EMPTY.
#[test]
fn erase_to_end_of_screen_clears_has_extras() {
    let mut grid = Grid::new(3, 10);

    // Add extras to cells on multiple rows
    for row in 0..3u16 {
        grid.set_cursor(row, 0);
        grid.write_char('X');
        let extra = grid.cell_extra_mut(row, 0);
        extra.set_fg_rgb(Some([128, 128, 128]));
    }

    // Verify extras are set
    for row in 0..3u16 {
        assert!(grid.cell(row, 0).unwrap().has_extras());
    }

    // Erase from row 1 downward
    grid.set_cursor(1, 0);
    grid.erase_to_end_of_screen();

    // Row 0 should keep its extras
    assert!(
        grid.cell(0, 0).unwrap().has_extras(),
        "row 0 above cursor should keep has_extras"
    );

    // Rows 1-2 should have cleared extras (cells reset to EMPTY)
    for row in 1..3u16 {
        assert!(
            !grid.cell(row, 0).unwrap().has_extras(),
            "row {row} below cursor should have has_extras cleared"
        );
    }
}
