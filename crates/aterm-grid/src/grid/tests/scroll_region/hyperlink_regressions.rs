// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use super::lettered;
use crate::{CellCoord, Grid};

fn link(grid: &mut Grid, row: u16, url: &str) -> Arc<str> {
    let url: Arc<str> = Arc::from(url);
    grid.cell_extra_mut(row, 0).set_hyperlink(Some(url.clone()));
    url
}

fn link_at(grid: &Grid, row: u16) -> Option<Arc<str>> {
    grid.extras()
        .get(CellCoord::new(row, 0))
        .and_then(|e| e.hyperlink().cloned())
}

fn assert_link_at(grid: &Grid, row: u16, url: Arc<str>) {
    assert!(grid.extras().row_has_hyperlinks(row));
    assert_eq!(link_at(grid, row), Some(url));
}

#[test]
fn grid_scroll_region_up_shifts_hyperlinks() {
    let mut grid = lettered(Grid::new(8, 10));

    let url_outside_above = link(&mut grid, 1, "https://above.com");
    let url_inside = link(&mut grid, 3, "https://inside.com");
    let url_outside_below = link(&mut grid, 6, "https://below.com");

    grid.set_scroll_region(2, 5);

    grid.scroll_region_up(1);

    assert_link_at(&grid, 1, url_outside_above);

    assert_link_at(&grid, 2, url_inside);
    assert!(!grid.extras().row_has_hyperlinks(3));

    assert_link_at(&grid, 6, url_outside_below);
}

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

#[test]
fn grid_scroll_region_down_shifts_hyperlinks() {
    let mut grid = lettered(Grid::new(8, 10));

    let url_inside = link(&mut grid, 3, "https://inside.com");
    let url_outside = link(&mut grid, 7, "https://outside.com");

    grid.set_scroll_region(2, 5);

    grid.scroll_region_down(1);

    assert_link_at(&grid, 4, url_inside);
    assert!(!grid.extras().row_has_hyperlinks(3));

    assert_link_at(&grid, 7, url_outside);
}

fn assert_line_op_shifts_inside_link(op: impl FnOnce(&mut Grid), moved_to: u16) {
    let mut grid = lettered(Grid::new(8, 10));

    let url_above = link(&mut grid, 1, "https://above.com");
    let url_inside = link(&mut grid, 4, "https://inside.com");
    let url_below = link(&mut grid, 7, "https://below.com");

    grid.set_scroll_region(2, 5);

    grid.set_cursor(3, 0);
    op(&mut grid);

    assert_link_at(&grid, 1, url_above);

    assert_link_at(&grid, moved_to, url_inside);
    assert!(!grid.extras().row_has_hyperlinks(4));

    assert_link_at(&grid, 7, url_below);
}

#[test]
fn grid_insert_lines_shifts_hyperlinks() {
    assert_line_op_shifts_inside_link(|grid| grid.insert_lines(1), 5);
}

#[test]
fn grid_delete_lines_shifts_hyperlinks() {
    assert_line_op_shifts_inside_link(|grid| grid.delete_lines(1), 3);
}
