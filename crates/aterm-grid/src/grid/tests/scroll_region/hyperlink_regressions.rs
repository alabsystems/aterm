// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use super::lettered;
use crate::{CellCoord, Grid};

/// Link column 0 of `row` to `url`; returns the url for later comparison.
fn link(grid: &mut Grid, row: u16, url: &str) -> Arc<str> {
    let url: Arc<str> = Arc::from(url);
    grid.extras_mut()
        .get_or_create(CellCoord::new(row, 0))
        .set_hyperlink(Some(url.clone()));
    url
}

/// The url column 0 of `row` links to, if any.
fn link_at(grid: &Grid, row: u16) -> Option<Arc<str>> {
    grid.extras()
        .get(CellCoord::new(row, 0))
        .and_then(|e| e.hyperlink().cloned())
}

/// `row` carries hyperlinks and column 0 links to `url`.
fn assert_link_at(grid: &Grid, row: u16, url: Arc<str>) {
    assert!(grid.extras().row_has_hyperlinks(row));
    assert_eq!(link_at(grid, row), Some(url));
}

/// Regression: scroll_region_up must shift CellExtras within the region (#1967).
/// Before fix, hyperlinks would desync from their visual rows after region scroll.
#[test]
fn grid_scroll_region_up_shifts_hyperlinks() {
    let mut grid = lettered(Grid::new(8, 10));

    // Place hyperlinks: row 1 (outside), row 3 (inside region), row 6 (outside)
    let url_outside_above = link(&mut grid, 1, "https://above.com");
    let url_inside = link(&mut grid, 3, "https://inside.com");
    let url_outside_below = link(&mut grid, 6, "https://below.com");

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Scroll region up by 1
    grid.scroll_region_up(1);

    // Row 1 (outside, above): hyperlink preserved at row 1
    assert_link_at(&grid, 1, url_outside_above);

    // Row 3 was inside region, scrolled up by 1 → now at row 2
    assert_link_at(&grid, 2, url_inside);
    // Row 3 should no longer have the hyperlink
    assert!(!grid.extras().row_has_hyperlinks(3));

    // Row 6 (outside, below): hyperlink preserved at row 6
    assert_link_at(&grid, 6, url_outside_below);
}

/// The top-anchored archival path must split metadata in two directions: row
/// zero's extras enter history, while extras on the protected footer stay at
/// their visible coordinates.
#[test]
fn grid_top_anchored_partial_scroll_preserves_history_and_footer_hyperlinks() {
    let mut grid = lettered(Grid::with_scrollback(5, 10, 10));

    let history_url = link(&mut grid, 0, "https://history.example");
    let footer_url = link(&mut grid, 4, "https://footer.example");

    grid.set_scroll_region(0, 2);
    grid.scroll_region_up(1);

    let archived = grid
        .get_history_line(0)
        .expect("top row and its metadata enter history");
    let spans = archived
        .hyperlinks()
        .expect("archived hyperlink metadata is retained");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].url, history_url);

    assert_eq!(link_at(&grid, 4), Some(footer_url));
    assert!(
        !grid.extras().row_has_hyperlinks(3),
        "protected footer metadata must not remain at its temporary shifted row"
    );
    grid.assert_invariants();
}

/// scroll_region_down shifts CellExtras with their rows within the region.
#[test]
fn grid_scroll_region_down_shifts_hyperlinks() {
    let mut grid = lettered(Grid::new(8, 10));

    // Place hyperlink inside region at row 3
    let url_inside = link(&mut grid, 3, "https://inside.com");
    let url_outside = link(&mut grid, 7, "https://outside.com");

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Scroll region down by 1
    grid.scroll_region_down(1);

    // Row 3 hyperlink should have shifted down to row 4
    assert_link_at(&grid, 4, url_inside);
    // Row 3 should no longer have the hyperlink
    assert!(!grid.extras().row_has_hyperlinks(3));

    // Row 7 (outside): preserved
    assert_link_at(&grid, 7, url_outside);
}

/// Links at row 1 (above the 2..=5 region), row 4 (inside) and row 7 (below);
/// with the cursor at row 3, `op` runs. The outside links stay put and the
/// inside link lands on `moved_to`, leaving row 4.
fn assert_line_op_shifts_inside_link(op: impl FnOnce(&mut Grid), moved_to: u16) {
    let mut grid = lettered(Grid::new(8, 10));

    let url_above = link(&mut grid, 1, "https://above.com");
    let url_inside = link(&mut grid, 4, "https://inside.com");
    let url_below = link(&mut grid, 7, "https://below.com");

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    grid.set_cursor(3, 0);
    op(&mut grid);

    // Row 1 (outside above): preserved
    assert_link_at(&grid, 1, url_above);

    assert_link_at(&grid, moved_to, url_inside);
    // Row 4 should no longer have the hyperlink
    assert!(!grid.extras().row_has_hyperlinks(4));

    // Row 7 (outside below): preserved
    assert_link_at(&grid, 7, url_below);
}

/// insert_lines shifts CellExtras down within [cursor_row, region.bottom].
#[test]
fn grid_insert_lines_shifts_hyperlinks() {
    // Insert 1 line at row 3 — shifts rows 3-5 down by 1: the row 4 link lands on row 5.
    assert_line_op_shifts_inside_link(|grid| grid.insert_lines(1), 5);
}

/// delete_lines shifts CellExtras up within [cursor_row, region.bottom].
#[test]
fn grid_delete_lines_shifts_hyperlinks() {
    // Delete 1 line at row 3 — shifts rows 4-5 up by 1: the row 4 link lands on row 3.
    assert_line_op_shifts_inside_link(|grid| grid.delete_lines(1), 3);
}
